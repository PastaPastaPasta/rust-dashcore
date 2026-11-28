//! A handler's waits and `HandlePhase` (DK1 plan §2, R-c): Core's loops over 100 ms
//! `UninterruptibleSleep` ticks, through an injected [`PhaseClock`].

use std::time::Duration;

use dash_llmq::dkg::phase::sleep_before_phase_ms;
use dash_primitives::QuorumHash;
use tracing::debug;

use super::handler::Handler;
use super::seams::{DkgChain, PhaseMarks};
use super::types::QuorumPhase;

/// `UninterruptibleSleep(std::chrono::milliseconds{100})`: one tick of every wait.
pub const TICK: Duration = Duration::from_millis(100);

/// The phase thread's clock and the chain parameters its sleeps read.
pub trait PhaseClock: Send + Sync {
    /// `SteadyClock::now()`, from any fixed origin.
    fn steady_now(&self) -> Duration;
    /// Sleeps one [`TICK`].
    fn sleep_tick(&self);
    /// `nPowTargetSpacing`, in seconds.
    fn pow_spacing_s(&self) -> i64;
    /// `MineBlocksOnDemand()`.
    fn mine_on_demand(&self) -> bool;
}

/// `AbortPhaseException`: the round ends here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abort;

/// What the waits read besides the clock: the handler, the chain's snapshot gate and the debug
/// manager.
#[derive(Clone, Copy)]
pub struct PhaseCtx<'a> {
    /// The handler.
    pub handler: &'a Handler,
    /// `m_chainman`.
    pub chain: &'a dyn DkgChain,
    /// `m_dkgdbgman`.
    pub marks: &'a dyn PhaseMarks,
}

fn is_null(hash: &QuorumHash) -> bool {
    *hash == QuorumHash::default()
}

fn phase_int(phase: Option<QuorumPhase>) -> i32 {
    phase.map_or(-1, |p| p as i32)
}

impl PhaseCtx<'_> {
    fn abort(&self, func: &str, why: &str) -> Result<(), Abort> {
        let h = self.handler.key();
        debug!(
            target: "llmq-dkg",
            "ActiveDKGSessionHandler::{func} -- {} qi[{}] - {why}",
            h.llmq_type,
            h.quorum_index
        );
        Err(Abort)
    }

    /// The two checks every wait makes first on each pass: the snapshot gate, then the stop flag.
    fn check_snapshot_and_stop(&self, func: &str) -> Result<(), Abort> {
        if self.chain.snapshot_unvalidated() {
            return self
                .abort(func, "aborting because snapshot background validation is incomplete");
        }
        if self.handler.stop_requested() {
            return self.abort(func, "aborting due to stop/shutdown requested");
        }
        Ok(())
    }
}

/// Waits until the handler reaches `next`, aborting on a snapshot, a stop, a quorum hash other
/// than a non-null `expected`, or a phase other than `cur` (when given). While it waits,
/// `should_not_wait` runs on each pass and the tick is skipped while it returns true. Reaching
/// Initialized resets the local session status.
///
/// # Errors
///
/// [`Abort`] on any of the above.
// Core: v24.0.0 src/active/dkgsessionhandler.cpp:111-147 ActiveDKGSessionHandler::WaitForNextPhase
pub fn wait_for_next_phase(
    ctx: &PhaseCtx<'_>,
    clock: &dyn PhaseClock,
    cur: Option<QuorumPhase>,
    next: QuorumPhase,
    expected: &QuorumHash,
    should_not_wait: &mut dyn FnMut() -> bool,
) -> Result<(), Abort> {
    const F: &str = "WaitForNextPhase";
    let key = ctx.handler.key();
    let (t, qi) = (key.llmq_type, key.quorum_index);
    let (cur_int, next_int) = (phase_int(cur), next as i32);
    debug!(
        target: "llmq-dkg",
        "ActiveDKGSessionHandler::{F} -- {t} qi[{qi}] - starting, curPhase={cur_int}, nextPhase={next_int}"
    );
    loop {
        ctx.check_snapshot_and_stop(F)?;
        let (phase, hash) = ctx.handler.phase_and_quorum_hash();
        if !is_null(expected) && hash != *expected {
            return ctx.abort(F, "aborting due unexpected expectedQuorumHash change");
        }
        if phase == next {
            break;
        }
        if cur.is_some_and(|c| phase != c) {
            let why = format!(
                "aborting due unexpected phase change, _phase={}, curPhase={cur_int}",
                phase as i32
            );
            return ctx.abort(F, &why);
        }
        if !should_not_wait() {
            clock.sleep_tick();
        }
    }
    debug!(
        target: "llmq-dkg",
        "ActiveDKGSessionHandler::{F} -- {t} qi[{qi}] - done, curPhase={cur_int}, nextPhase={next_int}"
    );
    if next == QuorumPhase::Initialized {
        ctx.marks.reset_local_session_status(t, qi);
    }
    Ok(())
}

/// Waits until the handler's quorum hash differs from `old`, aborting on a snapshot or a stop.
/// Core's loop runs no wait function, so this one takes none.
///
/// # Errors
///
/// [`Abort`] on a snapshot or a stop.
// Core: v24.0.0 src/active/dkgsessionhandler.cpp:149-170 ActiveDKGSessionHandler::WaitForNewQuorum
pub fn wait_for_new_quorum(
    ctx: &PhaseCtx<'_>,
    clock: &dyn PhaseClock,
    old: &QuorumHash,
) -> Result<(), Abort> {
    const F: &str = "WaitForNewQuorum";
    let key = ctx.handler.key();
    let (t, qi) = (key.llmq_type, key.quorum_index);
    debug!(target: "llmq-dkg", "ActiveDKGSessionHandler::{F} -- {t} qi[{qi}]- starting");
    loop {
        ctx.check_snapshot_and_stop(F)?;
        let (_, hash) = ctx.handler.phase_and_quorum_hash();
        if hash != *old {
            break;
        }
        clock.sleep_tick();
    }
    debug!(target: "llmq-dkg", "ActiveDKGSessionHandler::{F} -- {t} qi[{qi}] - done");
    Ok(())
}

/// A member's wait before a phase's start function, so that the members do not all send at
/// once: none without a session, for a non-member or where blocks are mined on demand; else
/// [`sleep_before_phase_ms`] for our index, ended early once the blocks since the start would
/// have taken longer. `run_while_waiting` runs on each pass and the tick is skipped while it
/// returns true.
///
/// # Errors
///
/// [`Abort`] on a snapshot, a stop, or a phase or quorum hash other than `cur` and `expected`.
// Core: v24.0.0 src/active/dkgsessionhandler.cpp:172-235 ActiveDKGSessionHandler::SleepBeforePhase
pub fn sleep_before_phase(
    ctx: &PhaseCtx<'_>,
    clock: &dyn PhaseClock,
    cur: QuorumPhase,
    expected: &QuorumHash,
    factor: f64,
    run_while_waiting: &mut dyn FnMut() -> bool,
) -> Result<(), Abort> {
    const F: &str = "SleepBeforePhase";
    let Some(my_idx) = ctx.handler.session().and_then(|s| s.my_index()) else {
        return Ok(());
    };
    if clock.mine_on_demand() {
        return Ok(());
    }
    let h = ctx.handler;
    let key = h.key();
    let (t, qi) = (key.llmq_type, key.quorum_index);
    let spacing = clock.pow_spacing_s();
    let sleep_ms = sleep_before_phase_ms(h.params(), spacing, Some(my_idx), factor, false);
    let end = clock.steady_now() + Duration::from_millis(u64::try_from(sleep_ms).unwrap_or(0));
    let mut height_tmp = h.current_height();
    let height_start = height_tmp;
    let cur_int = cur as i32;
    debug!(
        target: "llmq-dkg",
        "ActiveDKGSessionHandler::{F} -- {t} qi[{qi}] - starting sleep for {sleep_ms} ms, curPhase={cur_int}"
    );
    while clock.steady_now() < end {
        ctx.check_snapshot_and_stop(F)?;
        let cur_height = h.current_height();
        if cur_height > height_tmp {
            let expected_block_time = i64::from(cur_height - height_start) * spacing * 1000;
            if expected_block_time > sleep_ms {
                break;
            }
            height_tmp = cur_height;
        }
        let (phase, hash) = h.phase_and_quorum_hash();
        if phase != cur || hash != *expected {
            return ctx.abort(F, "aborting due unexpected phase/expectedQuorumHash change");
        }
        if !run_while_waiting() {
            clock.sleep_tick();
        }
    }
    debug!(target: "llmq-dkg", "ActiveDKGSessionHandler::{F} -- {t} qi[{qi}] - done, curPhase={cur_int}");
    Ok(())
}

/// One phase of a round: the sleep, the snapshot gate, `start`, `MarkPhaseAdvanced(cur)`, then
/// the wait for `next` with `run_while_waiting`.
///
/// # Errors
///
/// [`Abort`] from the sleep, the snapshot gate or the wait.
// Core: v24.0.0 src/active/dkgsessionhandler.cpp:237-255 ActiveDKGSessionHandler::HandlePhase
#[allow(clippy::too_many_arguments, reason = "Core's HandlePhase and its handler")]
pub fn handle_phase(
    ctx: &PhaseCtx<'_>,
    clock: &dyn PhaseClock,
    cur: QuorumPhase,
    next: QuorumPhase,
    expected: &QuorumHash,
    factor: f64,
    start: impl FnOnce(),
    run_while_waiting: &mut dyn FnMut() -> bool,
) -> Result<(), Abort> {
    const F: &str = "HandlePhase";
    let key = ctx.handler.key();
    let (t, qi) = (key.llmq_type, key.quorum_index);
    let (cur_int, next_int) = (cur as i32, next as i32);
    debug!(
        target: "llmq-dkg",
        "ActiveDKGSessionHandler::{F} -- {t} qi[{qi}] - starting, curPhase={cur_int}, nextPhase={next_int}"
    );
    sleep_before_phase(ctx, clock, cur, expected, factor, run_while_waiting)?;
    if ctx.chain.snapshot_unvalidated() {
        debug!(
            target: "llmq-dkg",
            "{F} -- refusing DKG participation while snapshot background validation is incomplete"
        );
        return Err(Abort);
    }
    start();
    ctx.marks.mark_phase_advanced(t, qi, cur);
    wait_for_next_phase(ctx, clock, Some(cur), next, expected, run_while_waiting)?;
    debug!(
        target: "llmq-dkg",
        "ActiveDKGSessionHandler::{F} -- {t} qi[{qi}] - done, curPhase={cur_int}, nextPhase={next_int}"
    );
    Ok(())
}
