//! The coalescing status cell (SB5 §3.6), apart from the F1 stream. `status.rs` is SB4's
//! `Status`.
//!
//! The writer stores the facts, then bumps `version`, then wakes the readers (§4.3, n7). A reader
//! diffs the latest facts against what it last reported, so a reader that is behind sees only the
//! latest facts, and each fact once (Core's two dedups). `arm` is register-then-recheck (R1-m1).

use std::sync::Arc;
use std::task::{Context, Poll};

use dash_consensus::BlockId;
use smallvec::SmallVec;

use crate::chainlock::ChainLockRef;
use crate::feed::FeedShared;
use crate::notify::Notify;
use crate::sync::{AtomicBool, AtomicU64, Mutex, Ordering, Published};
use crate::tip::BlockRef;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusEvent {
    /// Core `NotifyHeaderTip` (`validation.cpp:3693-3714`): `m_best_header` changed.
    HeaderAccepted {
        best_header: BlockRef,
        time: u32,
    },
    /// Core `NotifyChainLock` (`chainlock/handler.cpp:312-320`): the locked block is new and on
    /// the active chain.
    ChainLocked {
        block: BlockRef,
        lock: ChainLockRef,
    },
}

/// What one publication says (§3.6): the best header and its time, and the ChainLock floor with
/// whether it is on the active chain of the same snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusFacts {
    pub(crate) best_header: Option<(BlockRef, u32)>,
    pub(crate) cl: Option<ClFact>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClFact {
    pub(crate) block: BlockRef,
    /// The floor is on the published active chain (`chainlock/handler.cpp:299-309`).
    pub(crate) active: bool,
}

/// The facts and the readers' wakes, in `FeedShared` (§3.2).
pub(crate) struct StatusCell {
    facts: Published<StatusFacts>,
    /// Bumped after every facts store (SeqCst): `arm`'s recheck reads it.
    version: AtomicU64,
    /// The writer only loads it.
    readers: Published<Vec<Arc<StatusSlot>>>,
    /// Adds and removes a reader; the writer never takes it (§4.2).
    readers_lock: Mutex<()>,
    #[cfg(test)]
    pub(crate) faults: StatusFaults,
}

/// One reader's wake: a leaf lock, every section O(1).
struct StatusSlot {
    waker: Mutex<Option<Notify>>,
    closed: AtomicBool,
}

/// Planted faults for the status tests (§10.1, §10.2 L8).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct StatusFaults {
    /// L8's negative control: `arm` registers the waker without re-loading `version`.
    pub(crate) arm_without_recheck: bool,
    /// L8b's negative control: `next_events` loads the facts before `version`.
    pub(crate) facts_before_version: bool,
    /// `status_arm_race`: inside `arm`, between its check and the registration.
    #[cfg(not(loom))]
    pub(crate) in_arm: crate::fault::PausePoint,
}

impl StatusCell {
    pub(crate) fn new() -> StatusCell {
        StatusCell {
            facts: Published::new(StatusFacts::default()),
            version: AtomicU64::new(0),
            readers: Published::new(Vec::new()),
            readers_lock: Mutex::new(()),
            #[cfg(test)]
            faults: StatusFaults::default(),
        }
    }

    /// The published facts.
    #[cfg(all(any(debug_assertions, feature = "test-util"), not(loom)))]
    pub(crate) fn facts(&self) -> Arc<StatusFacts> {
        self.facts.load()
    }

    /// The writer, under the tip lock after the snapshot store (§4.3, n7): when the facts
    /// changed, stores them, bumps `version`, then wakes each open reader after its slot section.
    /// Returns whether they changed.
    pub(crate) fn publish(&self, facts: StatusFacts) -> bool {
        if *self.facts.load() == facts {
            return false;
        }
        self.facts.store(Arc::new(facts));
        self.version.fetch_add(1, Ordering::SeqCst);
        for slot in self.readers.load().iter() {
            if slot.closed.load(Ordering::SeqCst) {
                continue;
            }
            let w = slot.waker.lock().clone();
            if let Some(w) = w {
                w.notify();
            }
        }
        true
    }

    /// Adds or removes a reader: copies the list under `readers_lock` and republishes it.
    fn update_readers(&self, f: impl FnOnce(&mut Vec<Arc<StatusSlot>>)) {
        let _g = self.readers_lock.lock();
        let mut list = Vec::clone(&self.readers.load());
        f(&mut list);
        self.readers.store(Arc::new(list));
    }
}

/// One status subscriber, from [`crate::HeaderChain::status_feed`]. It starts at the current
/// facts and reports only later changes. A reader that is behind sees only the latest facts.
pub struct StatusReader {
    slot: Arc<StatusSlot>,
    shared: Arc<FeedShared>,
    seen_version: u64,
    seen_best: Option<BlockId>,
    seen_cl: Option<BlockId>,
}

impl StatusReader {
    /// Adds a reader under the list lock, off the writer's path. Until the writer's first
    /// publication the cell holds no facts, so the current ones are `first()`'s: the facts of the
    /// cell's first snapshot, or of a later one whose facts are not yet stored.
    pub(crate) fn new(
        shared: Arc<FeedShared>,
        first: impl FnOnce() -> StatusFacts,
    ) -> StatusReader {
        let slot = Arc::new(StatusSlot {
            waker: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        let cell = &shared.status;
        // The list store and the writer's bump are SeqCst on both sides (arc-swap's swap and
        // load, `version`), so the writer sees this slot or the load below sees the bump.
        cell.update_readers(|list| list.push(slot.clone()));
        // Start at the current facts: version first, as in `next_events`. While the version is
        // still 0 after `first()`, the writer's first publication is at most in progress, so its
        // stored facts are no older than `first()`'s and its bump is still to come. (Today every
        // publication needs `&mut HeaderChain`, so the re-check is defensive.)
        let mut seen_version = cell.version.load(Ordering::SeqCst);
        let mut seed = None;
        if seen_version == 0 {
            seed = Some(first());
            seen_version = cell.version.load(Ordering::SeqCst);
        }
        // A bump since `first()`: the stored facts are at least as new.
        let facts = seed.filter(|_| seen_version == 0).unwrap_or_else(|| *cell.facts.load());
        StatusReader {
            seen_version,
            seen_best: facts.best_header.map(|(b, _)| b.id),
            seen_cl: facts.cl.filter(|c| c.active).map(|c| c.block.id),
            slot,
            shared,
        }
    }

    fn cell(&self) -> &StatusCell {
        &self.shared.status
    }

    /// At most two events, coalesced: the latest best header, then the latest active lock. Each
    /// fires only when it differs from this reader's last report (Core's `pindexHeaderOld` and
    /// `lastNotifyChainLockBlockIndex`). An empty result after `arm` reported a change is
    /// normal: the change may be one this reader already reported, or none it reports at all.
    pub fn next_events(&mut self) -> SmallVec<[StatusEvent; 2]> {
        #[cfg(test)]
        if self.cell().faults.facts_before_version {
            let facts = *self.cell().facts.load();
            self.seen_version = self.cell().version.load(Ordering::SeqCst);
            return self.report(facts);
        }
        // The version first: a fact stored after it is reported now, and its bump wakes `arm`
        // again, which then finds nothing new.
        self.seen_version = self.cell().version.load(Ordering::SeqCst);
        let facts = *self.cell().facts.load();
        self.report(facts)
    }

    /// The events `facts` holds that this reader has not reported.
    fn report(&mut self, facts: StatusFacts) -> SmallVec<[StatusEvent; 2]> {
        let mut out = SmallVec::new();
        if let Some((b, time)) = facts.best_header
            && self.seen_best != Some(b.id)
        {
            self.seen_best = Some(b.id);
            out.push(StatusEvent::HeaderAccepted {
                best_header: b,
                time,
            });
        }
        if let Some(cl) = facts.cl
            && cl.active
            && self.seen_cl != Some(cl.block.id)
        {
            self.seen_cl = Some(cl.block.id);
            out.push(StatusEvent::ChainLocked {
                block: cl.block,
                lock: ChainLockRef {
                    height: cl.block.height,
                    hash: cl.block.hash,
                },
            });
        }
        out
    }

    /// Register-then-recheck (§4.3): true if a change is already visible (call `next_events`);
    /// otherwise `w` is armed and fires on the next change.
    pub fn arm(&mut self, w: Notify) -> bool {
        if self.cell().version.load(Ordering::SeqCst) != self.seen_version {
            return true;
        }
        #[cfg(all(test, not(loom)))]
        self.cell().faults.in_arm.hit();
        let old = self.slot.waker.lock().replace(w);
        drop(old);
        #[cfg(test)]
        if self.cell().faults.arm_without_recheck {
            return false;
        }
        self.cell().version.load(Ordering::SeqCst) != self.seen_version
    }

    /// `arm` with the task's waker: `Ready` while a change is unreported, until `next_events`.
    pub fn poll_changed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.arm(Notify::Task(cx.waker().clone())) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for StatusReader {
    /// Closes the slot and republishes the list without it, so no waker is left behind (R1-m1).
    fn drop(&mut self) {
        self.slot.closed.store(true, Ordering::SeqCst);
        let old = self.slot.waker.lock().take();
        drop(old);
        self.shared.status.update_readers(|list| list.retain(|s| !Arc::ptr_eq(s, &self.slot)));
    }
}

#[cfg(all(test, loom))]
mod loom_tests;
#[cfg(all(test, not(loom)))]
mod tests;
