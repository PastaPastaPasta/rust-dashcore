//! The seam's adapters (CH2 §2.7; ST3a §5.6 item 4): CH2's marks and abort calls onto ST3a's
//! `World`.

use std::sync::{Arc, Mutex};

use dash_consensus::primitives::BlockHash;
use dashd_store::faulty::{Abort, FlushCause, Hash, Mark, PruneDue, World};

use crate::flush::{FlushCauses, FlushObserver, LogCat, ObsMark};
use crate::store::AbortSink;

// Core: init.cpp:2089 (a coins read that fails stops the node with this, to the user)
pub(crate) const DB_READ_FAILED: &str = "Error reading from database, shutting down.";

pub(crate) fn hash(h: &BlockHash) -> Hash {
    *h.as_bytes()
}

/// A coins read that failed: the latch has tripped and the sink has the stop, so V only returns.
pub(crate) fn read_failed<E>(_: E) -> Abort {
    Abort::new(DB_READ_FAILED)
}

/// Forwards CH2's marks to a world: [`FlushObserver`] for W and recovery, and through
/// [`WorldSink`] the abort sink's calls.
///
/// W numbers its flushes per [`FlushControl`](crate::flush::FlushControl), which the system
/// rebuilds for each op, so the adapter numbers each full flush in the trace itself, and forwards
/// `Durable` only for a full flush's B1: an index-only write has neither `FlushBegin` nor a
/// `Durable` in ST3a's model. Its bench timers (wall-clock durations) are dropped, so a trace is
/// a function of its schedule. The first `Abort` of an epoch is forwarded, and later ones only
/// repeat the failure already reported.
pub struct WorldObserver {
    world: World,
    state: Mutex<ObsState>,
}

#[derive(Debug, Default)]
struct ObsState {
    flushes: u64,
    /// The full flush begun and not yet durable: W's number and the trace's.
    open: Option<(u64, u64)>,
    /// The last full flush begun, for its step 7: W's number and the trace's.
    last: Option<(u64, u64)>,
    aborted: Option<String>,
}

impl WorldObserver {
    pub fn new(world: World) -> Self {
        Self {
            world,
            state: Mutex::new(ObsState::default()),
        }
    }

    /// Core's `AbortNode` (or init's stop) with `message`, after `StoreFailed { op }` if the
    /// failure is a store call's, unless this epoch has already aborted.
    pub fn abort(&self, op: Option<&'static str>, message: &str) {
        if let Some(op) = op {
            self.mark(ObsMark::StoreFailed {
                op,
            });
        }
        self.mark(ObsMark::Abort {
            message: message.into(),
        });
    }

    /// The message this epoch aborted with.
    pub fn aborted(&self) -> Option<String> {
        self.state.lock().expect("unpoisoned").aborted.clone()
    }

    pub fn world(&self) -> &World {
        &self.world
    }
}

impl FlushObserver for WorldObserver {
    fn mark(&self, m: ObsMark) {
        let mut st = self.state.lock().expect("unpoisoned");
        // Later failures only repeat the one already reported.
        if st.aborted.is_some() && matches!(m, ObsMark::StoreFailed { .. } | ObsMark::Abort { .. })
        {
            return;
        }
        let mark = match m {
            ObsMark::FlushBegin {
                flush,
                old,
                new,
            } => {
                st.flushes += 1;
                st.open = Some((flush, st.flushes));
                st.last = st.open;
                Mark::FlushBegin {
                    flush: st.flushes,
                    old: old.as_ref().map(hash),
                    new: hash(&new),
                }
            }
            ObsMark::Durable {
                flush,
            } => match st.open {
                Some((w, n)) if w == flush => {
                    st.open = None;
                    Mark::Durable {
                        flush: n,
                    }
                }
                _ => return,
            },
            ObsMark::Log {
                cat: LogCat::Bench,
                ..
            } => return,
            ObsMark::Log {
                line,
                ..
            } => Mark::Log {
                line,
            },
            ObsMark::PruneDecision {
                files,
                met_target,
                manual,
                tip,
                flush_for_prune,
                causes,
            } => Mark::PruneDecision {
                due: manual.map_or(PruneDue::Automatic, PruneDue::Manual),
                unlocked: files.clone(),
                dry_run: files,
                met_target,
                // b2 has no prune locks.
                predicted_lock: None,
                flush_for_prune,
                full_flush: full_flush(causes),
                new: hash(&tip),
            },
            ObsMark::PruneSelected {
                flush,
                files,
                bound,
                rearmed,
                target_unmet,
                lock,
            } => match st.last {
                Some((w, n)) if w == flush => Mark::PruneSelected {
                    flush: n,
                    files,
                    bound,
                    rearmed,
                    target_unmet,
                    lock,
                },
                _ => unreachable!("step 7 of a flush that did not begin"),
            },
            // No selection: the flush prunes nothing, and no `PruneSelected` follows (DEC-211).
            ObsMark::PruneDeclined {
                flush,
                files,
            } => match st.last {
                Some((w, n)) if w == flush => Mark::PruneDeclined {
                    flush: n,
                    files,
                },
                _ => unreachable!("step 7 of a flush that did not begin"),
            },
            ObsMark::BestEffortBegin => Mark::BestEffortBegin,
            ObsMark::BestEffortEnd => Mark::BestEffortEnd,
            ObsMark::RecoveryBegin => Mark::RecoveryBegin,
            ObsMark::RecoveryEnd => Mark::RecoveryEnd,
            ObsMark::StoreFailed {
                op,
            } => Mark::StoreFailed {
                op,
            },
            ObsMark::Abort {
                message,
            } => {
                st.aborted = Some(message.clone());
                Mark::Abort {
                    message,
                }
            }
        };
        drop(st);
        self.world.mark(mark);
    }
}

/// ST3a's causes of a full flush for CH2's: the coin and evo boundaries are the cache's, the
/// periodic flush the interval's, and the rest a scripted cause's.
fn full_flush(causes: FlushCauses) -> Vec<FlushCause> {
    let any = |c: FlushCauses| causes.intersects(c);
    let cache = FlushCauses::CACHE_LARGE
        | FlushCauses::CACHE_CRITICAL
        | FlushCauses::EVO_CRITICAL
        | FlushCauses::THRESHOLD;
    let scripted =
        FlushCauses::ALWAYS | FlushCauses::SCRIPTED | FlushCauses::LEFT_IBD | FlushCauses::EMPTY;
    [
        (FlushCauses::PRUNE, FlushCause::Prune),
        (cache, FlushCause::Cache),
        (FlushCauses::PERIODIC_FLUSH, FlushCause::Interval),
        (scripted, FlushCause::Scripted),
        (FlushCauses::DMAX, FlushCause::DMax),
    ]
    .into_iter()
    .filter_map(|(c, cause)| any(c).then_some(cause))
    .collect()
}

/// The abort sink of the system's engine: Core's `AbortNode` and the coins read failure, as
/// `Abort` marks.
pub struct WorldSink(pub Arc<WorldObserver>);

impl AbortSink for WorldSink {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        self.0.abort(None, user_msg.unwrap_or(log_msg));
    }

    fn read_failed(&self, _what: &str) {
        self.0.abort(Some("read"), DB_READ_FAILED);
    }
}
