//! The flush's marks and log lines (CH2 §2.7; ST3a §5.6 item 4). Production code does not depend
//! on ST3a: the seam's adapter forwards these marks to `World::mark`, and ND1's forwards the log
//! lines to the debug log.

use std::sync::Mutex;
use std::time::Instant;

use dash_consensus::primitives::BlockHash;

use super::policy::FlushCauses;

/// A Core log category the flush writes under; [`LogCat::Always`] is `LogPrintf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogCat {
    Always,
    /// `BCLog::COINDB`.
    CoinDb,
    /// `BCLog::BENCH`: the flush's timers.
    Bench,
    /// `BCLog::PRUNE`.
    Prune,
    /// `LogError`: `LogPrintf` at level error, which the debug log tags `[error]`.
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObsMark {
    FlushBegin {
        flush: u64,
        old: Option<BlockHash>,
        new: BlockHash,
    },
    /// B1 (or the index-only write) returned Durable.
    Durable {
        flush: u64,
    },
    Log {
        cat: LogCat,
        line: String,
    },
    /// Step 0's decision (CH2 §4.2), at every prune check V runs: D, and the call's full-flush
    /// causes (empty when none is due).
    PruneDecision {
        /// D, the dry run's files.
        files: Vec<u32>,
        met_target: bool,
        /// `pruneblockchain`'s height; `None` for an automatic check.
        manual: Option<u32>,
        /// V's tip at the decision.
        tip: BlockHash,
        /// `fFlushForPrune`: D ≠ ∅.
        flush_for_prune: bool,
        causes: FlushCauses,
    },
    /// Step 7's selection, applied (CH2 §4.2): `files` ⊆ D, eligible at `bound`.
    PruneSelected {
        flush: u64,
        files: Vec<u32>,
        /// Core's `nLastBlockWeCanPrune` at the selection.
        bound: u32,
        /// The automatic check was armed again (T40).
        rearmed: bool,
        /// An automatic target is unmet after the selection.
        target_unmet: bool,
        /// The prune locks' bound, if a lock lowered it (CH2b b3).
        lock: Option<u32>,
    },
    /// V declined step 7's selection, its tip behind it (DEC-211): the flush prunes nothing.
    PruneDeclined {
        flush: u64,
        files: Vec<u32>,
    },
    /// Step 12's unlink begins: a failure to remove a file, or to sync the directory after the
    /// removals, is logged and left for the startup scan (CONS-28).
    BestEffortBegin,
    BestEffortEnd,
    /// Recovery began: from the open-time `sync_wal` (§4.3 step 0) to [`ObsMark::RecoveryEnd`],
    /// a failed read stops the node (ST3a §4.4).
    RecoveryBegin,
    /// Recovery wrote its last batch, or found nothing to replay.
    RecoveryEnd,
    /// A store call failed and tripped the latch.
    StoreFailed {
        op: &'static str,
    },
    /// The flush stopped; the driver hands Core's `AbortNode` this message.
    Abort {
        message: String,
    },
}

/// Where the flush's marks go.
pub trait FlushObserver: Send + Sync {
    fn mark(&self, m: ObsMark);
}

/// An observer that drops every mark.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullObserver;

impl FlushObserver for NullObserver {
    fn mark(&self, _: ObsMark) {}
}

/// An observer that records every mark: the tests'.
#[derive(Debug, Default)]
pub struct RecordingObserver {
    marks: Mutex<Vec<ObsMark>>,
}

impl RecordingObserver {
    pub fn marks(&self) -> Vec<ObsMark> {
        self.marks.lock().expect("unpoisoned").clone()
    }

    /// The log lines of `cat`, in order.
    pub fn lines(&self, cat: LogCat) -> Vec<String> {
        self.marks()
            .into_iter()
            .filter_map(|m| match m {
                ObsMark::Log {
                    cat: c,
                    line,
                } if c == cat => Some(line),
                _ => None,
            })
            .collect()
    }
}

impl FlushObserver for RecordingObserver {
    fn mark(&self, m: ObsMark) {
        self.marks.lock().expect("unpoisoned").push(m);
    }
}

pub(crate) fn log(obs: &dyn FlushObserver, cat: LogCat, line: String) {
    obs.mark(ObsMark::Log {
        cat,
        line,
    });
}

// Core: logging/timer.h Timer (LOG_TIME_MILLIS_WITH_CATEGORY, BCLog::BENCH)
/// One of `FlushStateToDisk`'s timers: "… started" now, "… completed (%.2fms)" at [`done`].
///
/// [`done`]: BenchTimer::done
pub(crate) struct BenchTimer {
    title: String,
    start: Instant,
}

impl BenchTimer {
    pub(crate) fn start(obs: &dyn FlushObserver, title: String) -> Self {
        log(obs, LogCat::Bench, format!("FlushStateToDisk: {title} started"));
        Self {
            title,
            start: Instant::now(),
        }
    }

    pub(crate) fn done(self, obs: &dyn FlushObserver) {
        let ms = self.start.elapsed().as_secs_f64() * 1e3;
        log(obs, LogCat::Bench, format!("FlushStateToDisk: {} completed ({ms:.2}ms)", self.title));
    }
}

// Core: logging/timer.h Timer (LOG_TIME_SECONDS: no category, so logged unconditionally)
/// One of `FlushStateToDisk`'s timers in seconds: "… started" now, "… completed (%.2fs)" at
/// [`done`](SecondsTimer::done).
pub(crate) struct SecondsTimer {
    title: &'static str,
    start: Instant,
}

impl SecondsTimer {
    pub(crate) fn start(obs: &dyn FlushObserver, title: &'static str) -> Self {
        log(obs, LogCat::Always, format!("FlushStateToDisk: {title} started"));
        Self {
            title,
            start: Instant::now(),
        }
    }

    pub(crate) fn done(self, obs: &dyn FlushObserver) {
        let s = self.start.elapsed().as_secs_f64();
        log(obs, LogCat::Always, format!("FlushStateToDisk: {} completed ({s:.2}s)", self.title));
    }
}
