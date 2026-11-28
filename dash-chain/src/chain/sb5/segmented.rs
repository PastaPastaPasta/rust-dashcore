//! SPV composites in sub-steps (SB5 §4.8) and the step budget (§6.2).

use crate::budget::{StepBudgetStats, check_sub_step, step_cap};
use crate::chain::{HeaderChain, Hold};
use crate::config::ChainMode;
use crate::error::ChainError;
use crate::feed::{Fanout, Publish};
use crate::residency::Residency;
use crate::step::TipSummary;
use crate::store::HeaderStore;
use crate::tip::{BlockRef, TipChange, TipTarget};
use crate::walk;

impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    /// SPV stage 4 (SB4 §9.1, SB5 §4.8): moves the tip to `target` in sub-steps of at most
    /// `segment_events` (E) events inside one hold. Each sub-step is a pure disconnect or
    /// connect run; every one but the last is fanned out as one segment and drained before the
    /// next is planned, `wait` parking the writer while a target lacks events. The release then
    /// runs stage 4's side effects once, stores the one snapshot and the status, and fans out
    /// the last sub-step. Readers see only the pre- and post-step snapshots.
    ///
    /// `target` must be the block's own ref, the previous step's fanout drained (§3.3), and a
    /// pending plan from stage 1 must end at `target`; the sub-steps plan their own. An error
    /// after the hold opened abandons it: nothing is published and no side effect runs. If a
    /// sub-step had already moved the private tip, the latch trips (a cold read's error already
    /// has). `wait` must not take the tip lock.
    pub fn activate_segmented(
        &mut self,
        target: BlockRef,
        wait: &mut dyn FnMut(),
    ) -> Result<TipSummary, ChainError> {
        self.require_mode(ChainMode::Spv, "activate_segmented")?;
        self.refuse_in_hold("activate_segmented inside a hold")?;
        self.refuse_floor_conflict(target.id)?;
        if self.block_ref_of(target.id)? != target {
            return Err(ChainError::Internal("activate_segmented: a target ref off its block"));
        }
        if !self.pending_is_empty() {
            return Err(ChainError::Internal("tip step with a pending fanout"));
        }
        if let Some(p) = self.pending_plan.lock().take()
            && p.connect.last().unwrap_or(&p.fork).id != target.id
        {
            return Err(ChainError::Internal("activate_segmented: a plan to another target"));
        }
        let start = self.private_tip().map(|t| t.0.id);
        let mut hold = self.hold_tip();
        match hold.sub_steps(target, wait) {
            Ok((t, n, last)) => Ok(hold.release_segmented(t, n, last)),
            Err(e) => {
                let fail = &hold.chain.svc.fail;
                if hold.chain.private_tip().map(|t| t.0.id) != start && !fail.is_tripped() {
                    fail.read_failed("activate_segmented abandoned after a sub-step");
                }
                hold.abandon();
                Err(e)
            }
        }
    }

    /// The step budget's cap and peaks (§6.2).
    pub fn step_budget(&self) -> StepBudgetStats {
        let b = &self.feed_shared().budget;
        StepBudgetStats {
            cap: step_cap(&self.cfg.feeds, self.cfg.cold_cache),
            peak: b.peak(),
            marks_peak: b.marks_peak(),
        }
    }
}

impl<S: HeaderStore, R: Residency> Hold<'_, S, R> {
    /// §4.8 step 3: the sub-steps to `target`, all but the last fanned out and drained. Returns
    /// the change's shape, the sub-step count and the last sub-step, `None` with no step.
    fn sub_steps(
        &mut self,
        target: BlockRef,
        wait: &mut dyn FnMut(),
    ) -> Result<(TipTarget, u32, Option<TipChange>), ChainError> {
        let t = self.chain.tip_target(target.id)?;
        let e = self.chain.cfg.feeds.segment_events;
        let (mut n, mut disconnects) = (0, t.d);
        loop {
            let tip = self.chain.private_tip().expect("tip_target found a tip").0;
            if tip.id == target.id {
                return Ok((t, n, None));
            }
            // Down to the fork first, then up to the target: no sub-step mixes the two (m10).
            // Both by ancestry: below SPV's window the active chain no longer holds the heights.
            let (from, h) = if disconnects > 0 {
                let k = disconnects.min(e);
                disconnects -= k;
                (tip.id, tip.height - k)
            } else {
                (target.id, target.height.min(tip.height + e))
            };
            let link = |x| self.chain.node(x).map(|n| n.link());
            let sub = walk::ancestor(from, h, link, |x| self.chain.off_height(x))?
                .ok_or(ChainError::Internal("activate_segmented: a sub-step's height"))?;
            let change = self.set_active_tip(sub, e)?;
            #[cfg(all(test, not(loom)))]
            let change = self.chain.feed_shared().faults.inflated(change);
            check_sub_step(&change, e)?;
            if change.disconnected.is_empty() && change.connected.is_empty() {
                return Err(ChainError::Internal("activate_segmented: an empty sub-step"));
            }
            // SPV's window over the active chain moves with each sub-step, not at the release
            // only: a long run of connects would otherwise hold every height it passes (DEC-174).
            self.chain.window_active();
            debug_assert!(self.window_bounded(), "the active chain stays within the window");
            n += 1;
            debug_assert!(
                change.disconnected.is_empty() || change.connected.is_empty(),
                "no mixed sub-step (m10)"
            );
            if sub == target.id {
                return Ok((t, n, Some(change)));
            }
            let publish = self.with_tip(|st, shared| st.feeds.fanout(shared, composite(&change)));
            self.chain.feed_shared().budget.sub_step(&change, publish == Publish::Blocked);
            while self.retry_pending() == Publish::Blocked {
                wait();
            }
        }
    }

    /// After a sub-step's [`HeaderChain::window_active`]: an evicting chain holds at most
    /// `w + 1` active heights, `max(floor, tip − w)` up, or fewer after a disconnect below it.
    fn window_bounded(&self) -> bool {
        let (a, w) = (&self.chain.active, self.chain.cfg.window);
        !R::EVICTS || w.is_none_or(|w| a.len() - a.base() <= w + 1)
    }

    /// §4.8 step 4: stage 4's side effects once, for the final tip; the snapshot and the
    /// status; the last sub-step's fanout; then the hold's end (owner cleared, guard dropped,
    /// release wakes).
    fn release_segmented(
        mut self,
        t: TipTarget,
        sub_steps: u32,
        last: Option<TipChange>,
    ) -> TipSummary {
        let chain = &mut *self.chain;
        chain.stage4_effects();
        chain.feed_shared().budget.marks(&chain.overlay);
        let snap =
            chain.publish_locked(&mut self.guard, last.as_ref().map_or(Fanout::None, composite));
        if let Some(c) = &last {
            let copied = !self.pending_is_empty();
            self.chain.feed_shared().budget.sub_step(c, copied);
        }
        TipSummary {
            fork: t.fork,
            disconnected: t.d,
            connected: t.c,
            sub_steps,
            snap,
        }
    }
}

/// A sub-step's events as one segment, `seq`s from its first.
fn composite(c: &TipChange) -> Fanout<'_> {
    Fanout::Composite {
        first_seq: c.last_seq + 1 - (c.disconnected.len() + c.connected.len()) as u64,
        disconnected: &c.disconnected,
        connected: &c.connected,
    }
}
