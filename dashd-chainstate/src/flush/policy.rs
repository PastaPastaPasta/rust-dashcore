//! The flush decision (CH2 §3.3): Core's `FlushStateToDisk` trigger table, run on V at Core's call
//! sites, with our causes beside Core's.
//!
//! [`FlushPolicy::decide`] decides only. The freeze it decides follows the tip step (E-b1.2), and a
//! cause that meets a flush in flight is kept until that flush is released.

use std::time::{Duration, Instant};

use super::prune::{PruneDecision, PruneHost, prune_lock_bound};
use crate::coins::CoinsBoundary;

// Core: validation.cpp:104-106
/// How long the block index may go unwritten (`DATABASE_WRITE_INTERVAL`): `fPeriodicWrite`.
pub const DATABASE_WRITE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How long the coins may go unflushed (`DATABASE_FLUSH_INTERVAL`): `fPeriodicFlush`.
pub const DATABASE_FLUSH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
// Core: validation.cpp:3051 at v24.0.0
/// `fEvoDbCacheCritical`'s bound on the EvoDB's memory: 64 MiB.
pub const EVO_CRITICAL_BYTES: u64 = 64 << 20;

/// Core's `FlushStateMode` (`validation.h:415-420` at v24.0.0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlushStateMode {
    None,
    IfNeeded,
    Periodic,
    Always,
}

bitflags::bitflags! {
    /// Why a flush runs (CH2 §2.7, §3.3's table).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct FlushCauses: u16 {
        /// `mode == ALWAYS`.
        const ALWAYS = 1 << 0;
        /// `fCacheLarge`, on native Σ (DEC-99).
        const CACHE_LARGE = 1 << 1;
        /// `fCacheCritical`, on native Σ (DEC-99).
        const CACHE_CRITICAL = 1 << 2;
        /// `fEvoDbCacheCritical`.
        const EVO_CRITICAL = 1 << 3;
        /// `fPeriodicFlush`.
        const PERIODIC_FLUSH = 1 << 4;
        /// `fFlushForPrune`: step 0 selected files.
        const PRUNE = 1 << 5;
        /// Ours (A1 §3.4): active at f·B(t), or a shard of it at its trigger.
        const THRESHOLD = 1 << 6;
        /// Ours (ST3a §5.6 item 6): a subscriber at its limit posted `FlushDue`.
        const DMAX = 1 << 7;
        /// Ours (A1 §3.4): the node has left initial block download; once.
        const LEFT_IBD = 1 << 8;
        /// Tests.
        const SCRIPTED = 1 << 9;
        /// The empty flush for a pending `FlushDue` (§4.4.1 rule 5; CH2b b3).
        const EMPTY = 1 << 10;
    }
}

impl FlushCauses {
    /// Causes that make V wait for F's release at the call site, then freeze (§3.3's table: ALWAYS,
    /// `fCacheCritical`, `fEvoDbCacheCritical`). The rest wait for the release as pending causes.
    const WAIT: Self = Self::ALWAYS.union(Self::CACHE_CRITICAL).union(Self::EVO_CRITICAL);

    /// The point where V's take matches Core's dirty set: a take with it is Core's, and ends the
    /// kept dirt (CH3b D242, DEC-1500, DEC-1517). Every other take keeps: V's timers, measures
    /// and prune selections can differ from Core's, so its take there may clean what Core
    /// still holds.
    pub const CORE_POINT: Self = Self::ALWAYS;

    /// Core's full flush at a call, of the causes V models: ALWAYS, PRUNE and PERIODIC_FLUSH.
    /// Core sets `m_last_write` there (`validation.cpp:3086` at v24.0.0), as at `fPeriodicWrite`,
    /// and `m_last_flush` with a best block (`:3113`). The native-measure causes are not in it
    /// (CH3b D243, DEC-1566 Q1).
    const CORE_WRITE: Self = Self::ALWAYS.union(Self::PRUNE).union(Self::PERIODIC_FLUSH);
}

/// What V does at a call site (CH2 §2.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlushAction {
    Nothing,
    /// The block index alone (Core's `fPeriodicWrite`, or a full flush with no best block): the
    /// files barrier and one Durable batch. `signal`: Core's `ChainStateFlushed` follows its
    /// return (the ALWAYS shortcut of CH2 §4.2), a full flush of Core's. `causes`: the full
    /// flush's, none for the periodic write alone. `write_due`: Core's `fPeriodicWrite` at this
    /// call, or one that came due while the job before it was in flight (CH3b D242).
    IndexOnly {
        signal: bool,
        causes: FlushCauses,
        write_due: bool,
    },
    /// Freeze active now. With `wait_done`, V waits in its service loop until W releases F (ALWAYS,
    /// every prune, automatic or manual (CH3b D244), and every freeze under `-dbcrashratio`,
    /// D14). V also waits when the freeze leaves native Σ above B(t), which only the cache knows
    /// after it (CH2c D32). `write_due` as [`IndexOnly`](Self::IndexOnly)'s.
    Freeze {
        causes: FlushCauses,
        prune: Option<PruneDecision>,
        wait_done: bool,
        write_due: bool,
    },
    /// A flush is in flight: wait in the service loop for its release, then decide again with the
    /// same mode. The waiting causes are kept, so that decision freezes.
    WaitThenFreeze {
        causes: FlushCauses,
    },
    /// A flush is in flight: `causes` are kept, and the decision after its release freezes.
    Pending {
        causes: FlushCauses,
    },
}

impl FlushAction {
    /// Whether this action's take may clean what Core's dirty set still holds, so it keeps it
    /// (CH3b D242, DEC-1500, DEC-1517): its causes include no [`FlushCauses::CORE_POINT`],
    /// whatever else they hold, a PRUNE (automatic or manual) or a due periodic write included.
    /// A take with ALWAYS is Core's.
    pub fn keeps_cores_dirt(&self) -> bool {
        match self {
            Self::IndexOnly {
                causes,
                ..
            }
            | Self::Freeze {
                causes,
                ..
            } => !causes.intersects(FlushCauses::CORE_POINT),
            _ => false,
        }
    }
}

/// What [`FlushPolicy::decide`] reads at a call site.
pub struct PolicyCtx<'a> {
    pub now: Instant,
    /// The coin side: `CoinsCache::boundary_state` at this site (CH2a, O-R6-m5).
    pub boundary: CoinsBoundary,
    /// The window's evo memory ([`StateDelta::bytes`](super::StateDelta::bytes)).
    pub evo_bytes: u64,
    /// A flush is in flight: F, or an index-only write, which V counts until W releases it.
    pub in_flight: bool,
    /// The coins have a best block. Core flushes them only at one (`validation.cpp:3089` at
    /// v24.0.0); without one, a full flush writes the block index alone.
    pub has_best_block: bool,
    /// Active holds no DIRTY entry and the tip is `meta.tip`: ALWAYS needs only the index (§4.2).
    pub clean_at_durable: bool,
    pub prune: &'a mut dyn PruneHost,
    /// Core's `m_chain.Height()` at the call site: the dry run's tip and initial bound.
    pub chain_height: u32,
    /// `pruneblockchain`'s height. `Some(0)` is none, as Core tests `nManualPruneHeight > 0`.
    pub manual_prune_height: Option<u32>,
    /// Core's `fReindex`: no pruning while it is set (`validation.cpp:3000` at v24.0.0).
    pub reindex: bool,
    pub is_ibd: bool,
    /// The node has left initial block download.
    pub left_ibd: bool,
    /// A subscriber's `FlushDue` is pending (§4.4.1 rule 5).
    pub dmax_due: bool,
    /// Step 0's prune locks (`SubscriberHub::predicted_locks` at V's tip), read only when a
    /// check is due or peeked at. They are the call's snapshot: the same rows at every decision
    /// of one call, as Core reads its lock table once per call (`validation.cpp:3003-3014` at
    /// v24.0.0; CH3b D244).
    pub prune_locks: &'a dyn Fn() -> Vec<(&'static str, u32)>,
    /// A test asks for a flush.
    pub scripted: bool,
    /// The decision is one of Core's `FlushStateToDisk` calls, not V's own `poll` or
    /// `answer_due` flush: only such a call moves Core's timers (CH3b D243) and runs an automatic
    /// prune check (CH3b D244).
    pub core_call: bool,
}

/// Which causes a test's policy keeps (CH3a a10 M-3): a Diff leg compares V's flushes with
/// Core's, so it runs with Core's causes alone.
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlushRegime {
    /// Every cause, as the node runs.
    #[default]
    Production,
    /// Core's causes only: `THRESHOLD`, `DMAX`, `LEFT_IBD` and `EMPTY` are removed, and counted,
    /// before they keep, start or time anything. `SCRIPTED` is a failed assertion.
    CoreCausesOnly,
}

/// The causes [`FlushRegime::CoreCausesOnly`] removed: each decision that held one counts once.
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Suppressed {
    pub threshold: u32,
    pub dmax: u32,
    /// At most one per policy, as Production's once-only LEFT_IBD (m-4).
    pub left_ibd: u32,
    pub empty: u32,
}

/// Core's timers (CH3b D243, D244): `m_last_write` and `m_last_flush`, and what came due at
/// Core's calls made while a job was in flight, for the decision after the release.
#[derive(Debug, Default)]
struct CoreTimers {
    last_write: Option<Instant>,
    last_flush: Option<Instant>,
    /// Core's `fPeriodicWrite` came due at a call made with a job in flight (CH3b D242, D8).
    pending_write: bool,
    /// Core's `fPeriodicFlush` came due at a call made with a job in flight.
    flush_held: bool,
}

impl CoreTimers {
    /// Core's `fPeriodicFlush` and `fPeriodicWrite` at a call in `mode`, read against the timers
    /// as they stand before the call writes (`validation.cpp:3053-3055` at v24.0.0).
    fn due(&self, mode: FlushStateMode, now: Instant) -> (bool, bool) {
        let past = |at: Option<Instant>, interval| at.is_some_and(|t| now > t + interval);
        let periodic = mode == FlushStateMode::Periodic;
        (
            periodic && past(self.last_flush, DATABASE_FLUSH_INTERVAL),
            periodic && past(self.last_write, DATABASE_WRITE_INTERVAL),
        )
    }

    /// Core's records at a call whose full flush has `causes` (`validation.cpp:3086`, `:3113`
    /// at v24.0.0), or whose `fPeriodicWrite` is due.
    fn record(&mut self, now: Instant, causes: FlushCauses, write: bool, has_best_block: bool) {
        let full = causes.intersects(FlushCauses::CORE_WRITE);
        if full || write {
            self.last_write = Some(now);
        }
        if full && has_best_block {
            self.last_flush = Some(now);
        }
    }

    /// This call's own flags against the timers, then its records: PERIODIC_FLUSH, if due, and
    /// whether the periodic write is.
    fn call(
        &mut self,
        mode: FlushStateMode,
        ctx: &PolicyCtx<'_>,
        own: FlushCauses,
    ) -> (bool, bool) {
        let (flush, write) = self.due(mode, ctx.now);
        let causes = own | flag(flush, FlushCauses::PERIODIC_FLUSH);
        self.record(ctx.now, causes, write, ctx.has_best_block);
        (flush, write)
    }
}

/// V's flush decision across calls: Core's timers, the causes kept for a flush in flight, and the
/// once-only LEFT_IBD.
///
/// Only Core's calls ([`PolicyCtx::core_call`]) move the timers, at the call, a job in flight or
/// not; a node-own cause, a native-measure one, a held cause's later freeze and a release move
/// nothing (CH3b D243). An automatic prune check that Core's call meets is decided at that call
/// (CH3b D244): with a job in flight V peeks at the selection, takes the check at once if it is
/// empty, and otherwise waits for the release and selects in the same call, so a selection
/// records the timers at its call.
#[derive(Debug)]
pub struct FlushPolicy {
    /// Core's timers.
    timers: CoreTimers,
    left_ibd_done: bool,
    /// The causes kept for the flush in flight, but PERIODIC_FLUSH, which the timers keep.
    pending: FlushCauses,
    /// `-dbcrashratio` > 0: every freeze is waited for (D14).
    synchronous: bool,
    #[cfg(feature = "test-support")]
    regime: FlushRegime,
    #[cfg(feature = "test-support")]
    suppressed: Suppressed,
}

impl FlushPolicy {
    /// `synchronous`: `-dbcrashratio` is set, so V waits for every flush it starts (D14, §4.6).
    pub fn new(synchronous: bool) -> Self {
        Self {
            timers: CoreTimers::default(),
            left_ibd_done: false,
            pending: FlushCauses::empty(),
            synchronous,
            #[cfg(feature = "test-support")]
            regime: FlushRegime::Production,
            #[cfg(feature = "test-support")]
            suppressed: Suppressed::default(),
        }
    }

    /// A policy in `regime` (CH3a a10 M-3). Each open makes a new policy, so LEFT_IBD is armed
    /// again at a reopen.
    #[cfg(feature = "test-support")]
    pub fn with_regime(synchronous: bool, regime: FlushRegime) -> Self {
        Self {
            regime,
            ..Self::new(synchronous)
        }
    }

    /// What [`FlushRegime::CoreCausesOnly`] has removed so far.
    #[cfg(feature = "test-support")]
    pub fn suppressed(&self) -> Suppressed {
        self.suppressed
    }

    #[cfg(feature = "test-support")]
    pub fn regime(&self) -> FlushRegime {
        self.regime
    }

    /// Whether V waits for every flush it starts (D14).
    pub fn synchronous(&self) -> bool {
        self.synchronous
    }

    /// The causes kept for the flush in flight.
    pub fn pending(&self) -> FlushCauses {
        self.pending | self.held_flush()
    }

    /// PERIODIC_FLUSH if the timers hold it for the decision after the release.
    fn held_flush(&self) -> FlushCauses {
        flag(self.timers.flush_held, FlushCauses::PERIODIC_FLUSH)
    }

    /// Core's index write came due at a decision made with a job in flight, and no decision has
    /// taken it since (CH3b D242, D8).
    pub fn write_pending(&self) -> bool {
        self.timers.pending_write
    }

    // Core: validation.cpp:2980-3131 at v24.0.0 Chainstate::FlushStateToDisk (the decision)
    /// Decides what a call in `mode` does (CH2 §3.3's table).
    pub fn decide(&mut self, mode: FlushStateMode, ctx: &mut PolicyCtx<'_>) -> FlushAction {
        let now = ctx.now;
        if ctx.core_call {
            // Core: validation.cpp:3039-3045 at v24.0.0, no write or flush right after startup.
            self.timers.last_write.get_or_insert(now);
            self.timers.last_flush.get_or_insert(now);
        }
        // Core runs its prune half first (`validation.cpp:3000-3037`, then `:3038` at v24.0.0).
        let (prune, peek_waits) = if ctx.in_flight {
            (None, Self::peek_step(ctx))
        } else {
            (Self::prune_step(ctx), false)
        };

        let held = std::mem::take(&mut self.pending);
        let mut causes = FlushCauses::empty();
        if mode == FlushStateMode::Always {
            causes |= FlushCauses::ALWAYS;
        }
        causes |= coin_causes(mode, ctx.boundary);
        if mode == FlushStateMode::IfNeeded && ctx.evo_bytes >= EVO_CRITICAL_BYTES {
            causes |= FlushCauses::EVO_CRITICAL;
        }
        if ctx.dmax_due {
            causes |= FlushCauses::DMAX;
        }
        if ctx.left_ibd && !self.left_ibd_done {
            causes |= FlushCauses::LEFT_IBD;
        }
        if ctx.scripted {
            causes |= FlushCauses::SCRIPTED;
        }
        #[cfg(feature = "test-support")]
        self.mask(&mut causes);
        // Core's own calls write and flush at their `now`, in flight too, as Core's call writes
        // (B-1). A node-own decision is no Core call, so it records nothing.
        let own = causes & FlushCauses::ALWAYS;
        causes |= held;

        if ctx.in_flight {
            // A manual prune, and an automatic check whose peek selects, wait for the release and
            // are decided after it in the same call (CH3b D244); a check whose peek is empty was
            // taken above, as Core clears it after every run (`:3021-3036` at v24.0.0).
            let wait =
                causes.intersects(FlushCauses::WAIT) || manual_height(ctx).is_some() || peek_waits;
            if ctx.core_call {
                let (flush, write) = self.timers.call(mode, ctx, own);
                self.timers.flush_held |= flush;
                self.timers.pending_write |= write;
            }
            // F's B1 covers the records dirty at its freeze; the next check covers the rest.
            // ALWAYS is this call's own: the decision after the release has its own mode.
            self.pending = causes.difference(FlushCauses::ALWAYS);
            causes |= self.held_flush();
            return if wait {
                FlushAction::WaitThenFreeze {
                    causes,
                }
            } else if causes.is_empty() {
                FlushAction::Nothing
            } else {
                FlushAction::Pending {
                    causes,
                }
            };
        }

        // This call's flags against the timers as they were, then its records (a selection's
        // among them, `v:3053-3057` before `:3086`, `:3113` at v24.0.0), and what was carried from
        // a call made in flight, which Core wrote at that call (D8).
        let selection =
            flag(prune.as_ref().is_some_and(|p| !p.files.is_empty()), FlushCauses::PRUNE);
        let (mut periodic_flush, mut write_due) = if ctx.core_call {
            self.timers.call(mode, ctx, own | selection)
        } else {
            (false, false)
        };
        periodic_flush |= std::mem::take(&mut self.timers.flush_held);
        write_due |= std::mem::take(&mut self.timers.pending_write);
        if periodic_flush {
            causes |= FlushCauses::PERIODIC_FLUSH;
        }
        causes |= selection;
        let manual = prune.as_ref().is_some_and(|p| p.manual.is_some());

        if causes.is_empty() {
            return if write_due {
                FlushAction::IndexOnly {
                    signal: false,
                    causes,
                    write_due,
                }
            } else {
                FlushAction::Nothing
            };
        }
        if !ctx.has_best_block {
            // Core: validation.cpp:3064-3089 at v24.0.0, the block index is written and the coins
            // wait for a best block. A prune needs `fReindex` clear, so no best block means no
            // selection.
            debug_assert!(!causes.contains(FlushCauses::PRUNE), "a prune with no best block");
            return FlushAction::IndexOnly {
                signal: false,
                causes,
                write_due,
            };
        }
        if causes == FlushCauses::ALWAYS && ctx.clean_at_durable {
            // CH2 §4.2's ALWAYS shortcut: nothing to write but the index. Never taken with another
            // cause, so never while a `FlushDue` is pending (§4.4.1 rule 5).
            return FlushAction::IndexOnly {
                signal: true,
                causes,
                write_due,
            };
        }
        if causes.contains(FlushCauses::LEFT_IBD) {
            self.left_ibd_done = true;
        }
        FlushAction::Freeze {
            causes,
            prune,
            wait_done: self.synchronous
                || causes.intersects(FlushCauses::ALWAYS | FlushCauses::PRUNE)
                || manual,
            write_due,
        }
    }

    /// [`FlushRegime::CoreCausesOnly`]'s mask, before the causes keep, start or time anything. A
    /// LEFT_IBD it removes is consumed as Production's freeze would consume it: once per policy,
    /// with no job, no timer and no `last_flush`.
    #[cfg(feature = "test-support")]
    fn mask(&mut self, causes: &mut FlushCauses) {
        if self.regime == FlushRegime::Production {
            return;
        }
        assert!(!causes.contains(FlushCauses::SCRIPTED), "SCRIPTED under CoreCausesOnly");
        let s = &mut self.suppressed;
        for (cause, count) in [
            (FlushCauses::THRESHOLD, &mut s.threshold),
            (FlushCauses::DMAX, &mut s.dmax),
            (FlushCauses::LEFT_IBD, &mut s.left_ibd),
            (FlushCauses::EMPTY, &mut s.empty),
        ] {
            if causes.contains(cause) {
                causes.remove(cause);
                *count += 1;
            }
        }
        if s.left_ibd > 0 {
            self.left_ibd_done = true;
        }
    }

    // Core: validation.cpp:3000-3037 at v24.0.0 (the prune half of FlushStateToDisk)
    /// Step 0 (CH2 §4.2): the dry run, if a manual height or an armed automatic check asks for
    /// one. A manual prune leaves the automatic check armed (`validation.cpp:3021-3024` at
    /// v24.0.0). A node-own decision runs no automatic check: Core's next call does (CH3b D244,
    /// DEC-1566 Q4).
    fn prune_step(ctx: &mut PolicyCtx<'_>) -> Option<PruneDecision> {
        if ctx.reindex {
            return None;
        }
        let manual = manual_height(ctx);
        if manual.is_none() && (!ctx.core_call || !ctx.prune.check_due()) {
            return None;
        }
        // Core's `last_prune`: the chain's height, lowered to the predicted locks (DEC-64).
        let (bound, limiting) = prune_lock_bound(ctx.chain_height, (ctx.prune_locks)());
        if let Some(name) = limiting {
            ctx.prune.limited(name, bound);
        }
        let run = ctx.prune.dry_run(ctx.chain_height, bound, manual, ctx.is_ibd);
        Some(PruneDecision {
            files: run.files,
            met_target: run.met_target,
            manual,
            tip_height: ctx.chain_height,
            target: run.target,
            prune_after_height: run.prune_after_height,
            min_blocks_to_keep: run.min_blocks_to_keep,
        })
    }

    // Core: validation.cpp:3000-3036 at v24.0.0, at a call V makes with a job in flight
    /// The prune half of a Core call made with a job in flight (CH3b D244): whether the call waits
    /// for the release to select. An armed automatic check, with no manual height and no reindex,
    /// is peeked at from the call's snapshot. A peek that selects leaves the check armed for the
    /// decision after the release, in the same call. An empty one takes the check there and then
    /// with the logged run ([`prune_step`](Self::prune_step)), as Core clears it after every run;
    /// if that run selects after all, the check is armed again and the call waits, so no selection
    /// is dropped.
    fn peek_step(ctx: &mut PolicyCtx<'_>) -> bool {
        if !ctx.core_call || ctx.reindex || manual_height(ctx).is_some() || !ctx.prune.check_armed()
        {
            return false;
        }
        let (bound, _) = prune_lock_bound(ctx.chain_height, (ctx.prune_locks)());
        if ctx.prune.would_select(ctx.chain_height, bound, ctx.is_ibd) {
            return true;
        }
        let selects = Self::prune_step(ctx).is_some_and(|d| !d.files.is_empty());
        if selects {
            ctx.prune.arm();
        }
        selects
    }
}

/// `cause` if `on`, else none.
fn flag(on: bool, cause: FlushCauses) -> FlushCauses {
    if on {
        cause
    } else {
        FlushCauses::empty()
    }
}

// Core: validation.cpp:3000, 3020 at v24.0.0 (`nManualPruneHeight > 0`)
fn manual_height(ctx: &PolicyCtx<'_>) -> Option<u32> {
    ctx.manual_prune_height.filter(|&h| h > 0)
}

// Core: validation.cpp:3047-3051 at v24.0.0 (fCacheLarge, fCacheCritical)
/// The coin side's causes in `mode` (CH2 §3.3; moved from a2, O-R6-m5). Native Σ is checked only
/// at Core's call sites (DEC-95): LARGE or worse in PERIODIC is `fCacheLarge`, CRITICAL in
/// IF_NEEDED is `fCacheCritical`, and THRESHOLD freezes in either. NONE checks nothing, and ALWAYS
/// freezes anyway.
fn coin_causes(mode: FlushStateMode, boundary: CoinsBoundary) -> FlushCauses {
    match (mode, boundary) {
        (FlushStateMode::IfNeeded, CoinsBoundary::Critical) => FlushCauses::CACHE_CRITICAL,
        (FlushStateMode::Periodic, CoinsBoundary::Large | CoinsBoundary::Critical) => {
            FlushCauses::CACHE_LARGE
        }
        (FlushStateMode::IfNeeded | FlushStateMode::Periodic, CoinsBoundary::Threshold) => {
            FlushCauses::THRESHOLD
        }
        _ => FlushCauses::empty(),
    }
}

#[cfg(test)]
mod tests;
