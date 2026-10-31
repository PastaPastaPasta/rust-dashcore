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
// Core: validation.cpp:3040
/// `fEvoDbCacheCritical`'s bound on the EvoDB's memory: 64 MiB.
pub const EVO_CRITICAL_BYTES: u64 = 64 << 20;

/// Core's `FlushStateMode` (`validation.h:414-419`).
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
}

/// What V does at a call site (CH2 §2.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlushAction {
    Nothing,
    /// The block index alone (Core's `fPeriodicWrite`, or a full flush with no best block): the
    /// files barrier and one Durable batch. `signal`: Core's `ChainStateFlushed` follows its
    /// return (the ALWAYS shortcut of CH2 §4.2).
    IndexOnly {
        signal: bool,
    },
    /// Freeze active now. With `wait_done`, V waits in its service loop until W releases F (ALWAYS,
    /// manual prune, and every freeze under `-dbcrashratio`, D14). V also waits when the freeze
    /// leaves native Σ above B(t), which only the cache knows after it (CH2c D32).
    Freeze {
        causes: FlushCauses,
        prune: Option<PruneDecision>,
        wait_done: bool,
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

/// What [`FlushPolicy::decide`] reads at a call site.
pub struct PolicyCtx<'a> {
    pub now: Instant,
    /// The coin side: `CoinsCache::boundary_state` at this site (CH2a, O-R6-m5).
    pub boundary: CoinsBoundary,
    /// The window's evo memory ([`StateDelta::bytes`](super::StateDelta::bytes)).
    pub evo_bytes: u64,
    /// A flush is in flight: F, or an index-only write, which V counts until W releases it.
    pub in_flight: bool,
    /// The coins have a best block. Core flushes them only at one (`validation.cpp:3078`); without
    /// one, a full flush writes the block index alone.
    pub has_best_block: bool,
    /// Active holds no DIRTY entry and the tip is `meta.tip`: ALWAYS needs only the index (§4.2).
    pub clean_at_durable: bool,
    pub prune: &'a mut dyn PruneHost,
    /// Core's `m_chain.Height()` at the call site: the dry run's tip and initial bound.
    pub chain_height: u32,
    /// `pruneblockchain`'s height. `Some(0)` is none, as Core tests `nManualPruneHeight > 0`.
    pub manual_prune_height: Option<u32>,
    /// Core's `fReindex`: no pruning while it is set (`validation.cpp:2989`).
    pub reindex: bool,
    pub is_ibd: bool,
    /// The node has left initial block download.
    pub left_ibd: bool,
    /// A subscriber's `FlushDue` is pending (§4.4.1 rule 5).
    pub dmax_due: bool,
    /// Step 0's prune locks (`SubscriberHub::predicted_locks` at V's tip), read only when a
    /// check is due.
    pub prune_locks: &'a dyn Fn() -> Vec<(&'static str, u32)>,
    /// A test asks for a flush.
    pub scripted: bool,
}

/// V's flush decision across calls: Core's timers, the causes kept for a flush in flight, and the
/// once-only LEFT_IBD.
#[derive(Debug)]
pub struct FlushPolicy {
    last_write: Option<Instant>,
    last_flush: Option<Instant>,
    left_ibd_done: bool,
    pending: FlushCauses,
    /// `-dbcrashratio` > 0: every freeze is waited for (D14).
    synchronous: bool,
}

impl FlushPolicy {
    /// `synchronous`: `-dbcrashratio` is set, so V waits for every flush it starts (D14, §4.6).
    pub fn new(synchronous: bool) -> Self {
        Self {
            last_write: None,
            last_flush: None,
            left_ibd_done: false,
            pending: FlushCauses::empty(),
            synchronous,
        }
    }

    /// The causes kept for the flush in flight.
    pub fn pending(&self) -> FlushCauses {
        self.pending
    }

    /// The block index was written as of a decision at `decided_at`: Core's `m_last_write = nNow`.
    /// V calls it when B1 or an index-only write has returned.
    pub fn written(&mut self, decided_at: Instant) {
        self.last_write = Some(decided_at);
    }

    // Core: validation.cpp:2966-3125 Chainstate::FlushStateToDisk (the decision; v24.0.0-rc.3)
    /// Decides what a call in `mode` does (CH2 §3.3's table).
    pub fn decide(&mut self, mode: FlushStateMode, ctx: &mut PolicyCtx<'_>) -> FlushAction {
        let now = ctx.now;
        // Core: validation.cpp:3028-3034, no write or flush right after startup.
        let last_write = *self.last_write.get_or_insert(now);
        let last_flush = *self.last_flush.get_or_insert(now);

        let mut causes = std::mem::take(&mut self.pending);
        if mode == FlushStateMode::Always {
            causes |= FlushCauses::ALWAYS;
        }
        causes |= coin_causes(mode, ctx.boundary);
        if mode == FlushStateMode::IfNeeded && ctx.evo_bytes >= EVO_CRITICAL_BYTES {
            causes |= FlushCauses::EVO_CRITICAL;
        }
        if mode == FlushStateMode::Periodic && now > last_flush + DATABASE_FLUSH_INTERVAL {
            causes |= FlushCauses::PERIODIC_FLUSH;
        }
        let periodic_write =
            mode == FlushStateMode::Periodic && now > last_write + DATABASE_WRITE_INTERVAL;
        if ctx.dmax_due {
            causes |= FlushCauses::DMAX;
        }
        if ctx.left_ibd && !self.left_ibd_done {
            causes |= FlushCauses::LEFT_IBD;
        }
        if ctx.scripted {
            causes |= FlushCauses::SCRIPTED;
        }

        if ctx.in_flight {
            // An automatic prune check stays armed: Core clears it only when it selects (`:3017`),
            // so it is decided afresh after the release. A manual one waits for the release.
            let wait = causes.intersects(FlushCauses::WAIT) || manual_height(ctx).is_some();
            // F's B1 covers the records dirty at its freeze; the next check covers the rest.
            // ALWAYS is this call's own: the decision after the release has its own mode.
            self.pending = causes.difference(FlushCauses::ALWAYS);
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

        let prune = self.prune_step(ctx);
        if prune.as_ref().is_some_and(|p| !p.files.is_empty()) {
            causes |= FlushCauses::PRUNE;
        }
        let manual = prune.as_ref().is_some_and(|p| p.manual.is_some());

        if causes.is_empty() {
            return if periodic_write {
                FlushAction::IndexOnly {
                    signal: false,
                }
            } else {
                FlushAction::Nothing
            };
        }
        if !ctx.has_best_block {
            // Core: validation.cpp:3053-3078, the block index is written and the coins wait for a
            // best block. A prune needs `fReindex` clear, so no best block means no selection.
            debug_assert!(!causes.contains(FlushCauses::PRUNE), "a prune with no best block");
            return FlushAction::IndexOnly {
                signal: false,
            };
        }
        // Core: validation.cpp:3101, an empty cache's full flush sets `m_last_flush` too.
        self.last_flush = Some(now);
        if causes == FlushCauses::ALWAYS && ctx.clean_at_durable {
            // CH2 §4.2's ALWAYS shortcut: nothing to write but the index. Never taken with another
            // cause, so never while a `FlushDue` is pending (§4.4.1 rule 5).
            return FlushAction::IndexOnly {
                signal: true,
            };
        }
        if causes.contains(FlushCauses::LEFT_IBD) {
            self.left_ibd_done = true;
        }
        FlushAction::Freeze {
            causes,
            prune,
            wait_done: self.synchronous || causes.contains(FlushCauses::ALWAYS) || manual,
        }
    }

    // Core: validation.cpp:2989-3026 (the prune half of FlushStateToDisk)
    /// Step 0 (CH2 §4.2), with no flush in flight: the dry run, if a manual height or an armed
    /// automatic check asks for one.
    fn prune_step(&self, ctx: &mut PolicyCtx<'_>) -> Option<PruneDecision> {
        if ctx.reindex {
            return None;
        }
        let manual = manual_height(ctx);
        // A manual prune leaves the automatic check armed (`:3010-3013`).
        if manual.is_none() && !ctx.prune.check_due() {
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
        })
    }
}

// Core: validation.cpp:2989, 3009 (`nManualPruneHeight > 0`)
fn manual_height(ctx: &PolicyCtx<'_>) -> Option<u32> {
    ctx.manual_prune_height.filter(|&h| h > 0)
}

// Core: validation.cpp:3036-3040 (fCacheLarge, fCacheCritical)
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
