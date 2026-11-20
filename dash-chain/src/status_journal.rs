//! The statuses each published snapshot saw (CH1-node D63, BL-206). A node status write is a
//! seqlocked arena write that every reader sees at once, while the snapshot carrying the tip it
//! belongs with is stored later; Core shows both in one `cs_main` section. So the writer first
//! appends an entry's old status to the current journal, tagged with the epoch of the next
//! snapshot to be built, then writes the arena. A reader's status as of a snapshot is then the
//! first old status appended for the entry with a tag above the snapshot's epoch, in the journal
//! current at the snapshot's build or a later one, else the arena's. A snapshot carries no
//! journal (DEC-281, D68): the reader finds that one by the snapshot's epoch in the
//! [`JournalIndex`], so a build costs a relaxed load and store and no reference count. The writer moves
//! to a new journal at a write once the current one is full, and makes the first at the first
//! write. SPV writes no journal: its overlay reaches the arena at stage 4, immediately before the
//! snapshot store.
//!
//! The writer, not the journals, keeps the later journals alive (BL-224, D65): the last
//! [`RETAIN_AT_MOST`] entries' (D68), and always the newest snapshot's. A read of a snapshot
//! which needs a dropped journal is [`SnapshotExpired`]. [`JournalRetention`] gauges what the
//! writer keeps.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use dash_consensus::BlockId;
use once_cell::race::OnceBox;
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;

use crate::status::Status;

/// The writer moves to a new journal at a write once the current one holds this many entries:
/// it bounds how long a reader's scan of one journal is.
const ROTATE_AT: usize = 32;

/// The most entries the writer keeps in full journals before the open one (BL-224, D65, D68):
/// 1 MiB of 16-byte entries, about 1.14 MiB with their 2,048 journals, the longest walk past the
/// open one. At IBD's two or three journalled writes a block, a snapshot expires after some
/// 22,000 to 33,000 blocks. The newest snapshot's journals stay whatever their number: a command
/// that writes more with no build between (a deep `invalidateblock`, a large prune) keeps them
/// until the next build.
pub const RETAIN_AT_MOST: usize = 1 << 16;

/// Old statuses in write order, each with the epoch of the first snapshot built after its
/// write, linked to the journal the writer moved to next.
#[derive(Default)]
pub(crate) struct StatusJournal {
    log: Mutex<Vec<Entry>>,
    /// Kept alive by the writer, not by this journal (D65): one it has dropped no longer
    /// upgrades.
    next: OnceLock<Weak<StatusJournal>>,
    /// The writer's epoch when it moved to this journal: the oldest a snapshot reading from it
    /// can have.
    first_epoch: u64,
}

struct Entry {
    id: BlockId,
    old: Status,
    /// The epoch of the first snapshot built after the write.
    tag: u64,
}

impl StatusJournal {
    /// `id`'s status as of the snapshot at `epoch` whose build found this journal current: the
    /// first old status appended for it after that build; `None` if it has not been written
    /// since. A journal's `next` is read before its log: a journal with a `next` is final, and
    /// the walk steps on only through a `next` seen before that log was read. One without ends
    /// the walk: it has every write the caller's live read saw, as the writer links and appends
    /// before its arena write, which the live read synchronized with (the order of link and
    /// append does not matter). Reading the log first, the writer could append to it after the
    /// read, move on, and append a later status to the next, which the walk would return. A
    /// `next` that no longer upgrades was dropped past [`RETAIN_AT_MOST`]: the snapshot expired.
    pub(crate) fn status_at(
        &self,
        id: BlockId,
        epoch: u64,
    ) -> Result<Option<Status>, SnapshotExpired> {
        let mut hop: Arc<StatusJournal>;
        let mut j = self;
        loop {
            let next = j.next.get();
            #[cfg(test)]
            tests::hook(tests::Site::Walk);
            if let Some(e) = j.log.lock().iter().find(|e| e.id == id && e.tag > epoch) {
                return Ok(Some(e.old));
            }
            let Some(next) = next else {
                return Ok(None);
            };
            hop = next.upgrade().ok_or(SnapshotExpired)?;
            j = &hop;
        }
    }

    fn with_capacity(n: usize, first_epoch: u64) -> StatusJournal {
        StatusJournal {
            log: Mutex::new(Vec::with_capacity(n)),
            next: OnceLock::new(),
            first_epoch,
        }
    }
}

/// Not derived: a journal's `Debug` would print the whole log.
impl fmt::Debug for StatusJournal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatusJournal").finish_non_exhaustive()
    }
}

/// A read of a snapshot more than [`RETAIN_AT_MOST`] status writes old that needed a journal
/// the writer has dropped (BL-224, D65): take a new snapshot or view and read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the snapshot is past the status journals' bound; read a newer one")]
pub struct SnapshotExpired;

/// What the status journals keep (BL-224, D65, D68), whoever holds a snapshot: the writer keeps
/// the same journals for every one. As of the writer's last move to a new journal, but for the
/// open journal's entries, read as they are; `expired` is read on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct JournalRetention {
    /// The entries in the journals the writer keeps: the open one's, and the full ones', at
    /// most [`RETAIN_AT_MOST`] or those since the last build.
    pub entries: u64,
    /// The floor as of this read: a snapshot at this epoch or later reads exactly until the
    /// writer next drops a journal; an older one may read [`SnapshotExpired`]. 0 while the
    /// front journal kept is the first.
    pub oldest_epoch: u64,
    /// The reads that returned [`SnapshotExpired`], ever: a reader read a snapshot more than
    /// [`RETAIN_AT_MOST`] status writes old.
    pub expired: u64,
}

/// The journals the writer keeps, oldest first, in the chain's shared reader state: where a
/// reader finds a snapshot's journal by its epoch (DEC-281, D68), and what the gauge reads.
#[derive(Default)]
pub(crate) struct JournalIndex {
    /// Readers share it for a search; the writer takes it at each move on (cold).
    kept: RwLock<Kept>,
    expired: AtomicU64,
}

#[derive(Default)]
struct Kept {
    /// Oldest first, the open one last; never empty, as the index is published with the first.
    journals: VecDeque<Arc<StatusJournal>>,
    /// The full journals' entries.
    full: u64,
    /// The front's `first_epoch` when the writer last dropped one, else 0: a snapshot below it
    /// may need a dropped journal.
    floor: u64,
}

impl JournalIndex {
    /// `id`'s status as of the snapshot at `epoch`, from the journal current at its build (the
    /// newest whose `first_epoch` is at most `epoch`: later ones were moved to after the build)
    /// on; `None` if no journal has it since. A snapshot below the floor needs a dropped one.
    pub(crate) fn status_at(
        &self,
        id: BlockId,
        epoch: u64,
    ) -> Result<Option<Status>, SnapshotExpired> {
        let start = {
            let kept = self.kept.read();
            if epoch < kept.floor {
                None
            } else {
                // 0 when every journal is newer: then from the front.
                let n = kept.journals.partition_point(|j| j.first_epoch <= epoch);
                Some(kept.journals[n.saturating_sub(1)].clone())
            }
        };
        let r = match start {
            None => Err(SnapshotExpired),
            Some(j) => j.status_at(id, epoch),
        };
        if r.is_err() {
            self.expired.fetch_add(1, Ordering::Relaxed);
        }
        r
    }

    pub(crate) fn read(&self) -> JournalRetention {
        let (full, open, floor) = {
            let kept = self.kept.read();
            (kept.full, kept.journals.back().cloned(), kept.floor)
        };
        JournalRetention {
            entries: full + open.map_or(0, |j| j.log.lock().len() as u64),
            oldest_epoch: floor,
            expired: self.expired.load(Ordering::Relaxed),
        }
    }
}

/// [`JournalIndex`] in the chain's shared reader state, made at the first status write: a chain
/// that writes none (SPV's, a load's) allocates nothing for it. One pointer wide, so the state
/// keeps the size the arena's offset needs (`shared.rs`); a `OnceLock` would be two. Not an
/// `ArcSwapOption`: a second instance of it moved the arena chunks' `load` codegen (PF0
/// chain-accept-spv-2000).
#[derive(Default)]
pub(crate) struct JournalSlot(OnceBox<JournalIndex>);

impl JournalSlot {
    /// As [`JournalIndex::status_at`]; `None` before the first status write, when the arena's is
    /// every status there has been.
    pub(crate) fn status_at(
        &self,
        id: BlockId,
        epoch: u64,
    ) -> Result<Option<Status>, SnapshotExpired> {
        self.0.get().map_or(Ok(None), |ix| ix.status_at(id, epoch))
    }

    pub(crate) fn read(&self) -> JournalRetention {
        self.0.get().map_or_else(JournalRetention::default, |ix| ix.read())
    }

    /// No status write has made the index.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.get().is_none()
    }
}

/// The writer's side: the journal the next status write appends to; the [`JournalIndex`] keeps
/// it and the full ones before it.
pub(crate) struct StatusJournals {
    /// The open journal, also the index's last; `None` before the first status write.
    current: Option<Arc<StatusJournal>>,
    /// `current`'s length; only the writer appends.
    len: usize,
    /// The epoch of the next snapshot to be built: one above the last's (epochs grow by one
    /// per publication), 0 before the first. The chain's one epoch counter (D68): the tip
    /// state keeps none of its own.
    epoch: AtomicU64,
    /// [`ROTATE_AT`]; a field for tests.
    pub(crate) rotate_at: usize,
    /// [`RETAIN_AT_MOST`]; a field for tests.
    pub(crate) retain_at_most: usize,
    /// The id and tag of the last entry appended. Another write of that id before the next
    /// build appends nothing: a reader takes the first old status after its snapshot's build,
    /// and the same tag means no build since that entry, so a snapshot that reads either write
    /// reads that entry first.
    last: Option<(BlockId, u64)>,
}

impl Default for StatusJournals {
    fn default() -> StatusJournals {
        StatusJournals {
            current: None,
            len: 0,
            epoch: AtomicU64::new(0),
            rotate_at: ROTATE_AT,
            retain_at_most: RETAIN_AT_MOST,
            last: None,
        }
    }
}

impl StatusJournals {
    /// A snapshot is built now: its epoch, one above the last build's, and later writes are
    /// tagged above it. One build an epoch: a second at one epoch would see the writes between
    /// the two. No journal: a reader finds the snapshot's by its epoch (DEC-281). This is the
    /// tip state's former epoch increment, moved, so a build costs what it did before D63.
    #[inline]
    pub(crate) fn next_build(&self) -> u64 {
        let epoch = self.epoch.load(Ordering::Relaxed);
        self.epoch.store(epoch + 1, Ordering::Relaxed);
        epoch
    }

    /// As [`Self::next_build`], at a test's own epoch.
    #[cfg(test)]
    pub(crate) fn mark(&self, epoch: u64) {
        debug_assert!(epoch >= self.epoch.load(Ordering::Relaxed), "snapshot epochs grow");
        self.epoch.store(epoch + 1, Ordering::Relaxed);
    }

    /// Before the writer changes `id`'s status from `old`; nothing when it was the last id
    /// journalled, with no build since. A full journal is left for a new one.
    #[inline]
    pub(crate) fn record(&mut self, id: BlockId, old: Status, slot: &JournalSlot) {
        let tag = *self.epoch.get_mut();
        if self.last != Some((id, tag)) {
            self.append(id, old, tag, slot);
        }
    }

    fn append(&mut self, id: BlockId, old: Status, tag: u64, slot: &JournalSlot) {
        let current = match &self.current {
            Some(c) if self.len < self.rotate_at => c,
            _ => self.move_on(tag, slot),
        };
        current.log.lock().push(Entry {
            id,
            old,
            tag,
        });
        self.len += 1;
        self.last = Some((id, tag));
    }

    /// To a new journal, first written at `epoch`, which the index then keeps; then the oldest
    /// full journals past [`RETAIN_AT_MOST`] go, but not the last build's. The first makes the
    /// index with itself in it: a reader never finds one empty (D68 r1 N1).
    #[cold]
    #[inline(never)]
    fn move_on(&mut self, epoch: u64, slot: &JournalSlot) -> &Arc<StatusJournal> {
        let next = Arc::new(StatusJournal::with_capacity(self.rotate_at, epoch));
        self.len = 0;
        let Some(index) = slot.0.get() else {
            let kept = Kept {
                journals: VecDeque::from([next.clone()]),
                ..Kept::default()
            };
            let index = JournalIndex {
                kept: RwLock::new(kept),
                expired: AtomicU64::new(0),
            };
            assert!(slot.0.set(Box::new(index)).is_ok(), "the writer alone stores the index");
            return self.current.insert(next);
        };
        if let Some(c) = &self.current {
            assert!(c.next.set(Arc::downgrade(&next)).is_ok(), "the writer moves on once");
        }
        let mut kept = index.kept.write();
        kept.journals.push_back(next.clone());
        // Freed after the lock: a deep command's burst would otherwise hold readers off.
        let mut gone = SmallVec::<[Arc<StatusJournal>; 2]>::new();
        while kept.journals.len() > 1 {
            // The newest snapshot reads from the front on when the journal after it was moved to
            // since the last build (`first_epoch` is the writer's epoch at the move): the front
            // is then the last build's, or itself moved to since, if none was open at the build.
            let newest = kept.journals[1].first_epoch == epoch;
            if newest || (kept.journals.len() - 1) * self.rotate_at <= self.retain_at_most {
                break;
            }
            gone.extend(kept.journals.pop_front());
        }
        if !gone.is_empty() {
            kept.floor = kept.journals.front().expect("the open one").first_epoch;
        }
        // A command's burst of journals gives its room back once they go.
        if kept.journals.capacity() > 1024 && kept.journals.capacity() / 4 > kept.journals.len() {
            let len = kept.journals.len();
            kept.journals.shrink_to(2 * len);
        }
        kept.full = ((kept.journals.len() - 1) * self.rotate_at) as u64;
        drop(kept);
        drop(gone);
        self.current.insert(next)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Where a test runs the other side between two steps of the protocol.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(crate) enum Site {
        /// [`StatusJournal::status_at`]: between a journal's `next` read and its log read.
        Walk,
        /// [`crate::ChainReader::status_at`]: between the live read and the walk.
        Reader,
        /// `HeaderChain::modify`: between the journal append and the arena write.
        Writer,
    }

    type Hook = (Site, Box<dyn FnOnce()>);

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Runs `f` once, the next time this thread reaches `site`.
    pub(crate) fn set_hook(site: Site, f: impl FnOnce() + 'static) {
        HOOK.set(Some((site, Box::new(f))));
    }

    pub(crate) fn hook(site: Site) {
        if let Some((_, f)) = HOOK.with_borrow_mut(|h| h.take_if(|(s, _)| *s == site)) {
            f();
        }
    }

    const VALID_TREE: Status = Status(Status::VALID_TREE);
    const VALID_TRANSACTIONS: Status = Status(Status::VALID_TRANSACTIONS);
    const VALID_SCRIPTS: Status = Status(Status::VALID_SCRIPTS);

    /// A writer and its slot, as a chain has them.
    struct Writer {
        journals: StatusJournals,
        index: Arc<JournalSlot>,
    }

    impl Writer {
        fn record(&mut self, id: BlockId, old: Status) {
            self.journals.record(id, old, &self.index);
        }

        fn mark(&self, epoch: u64) {
            self.journals.mark(epoch);
        }

        fn at(&self, id: BlockId, epoch: u64) -> Result<Option<Status>, SnapshotExpired> {
            self.index.status_at(id, epoch)
        }

        fn journals(&self) -> usize {
            kept(&self.index)
        }
    }

    /// The journals `slot` keeps.
    fn kept(slot: &JournalSlot) -> usize {
        slot.0.get().map_or(0, |ix| ix.kept.read().journals.len())
    }

    fn rotating_at(rotate_at: usize) -> Writer {
        bounded(rotate_at, RETAIN_AT_MOST)
    }

    fn bounded(rotate_at: usize, retain_at_most: usize) -> Writer {
        Writer {
            journals: StatusJournals {
                rotate_at,
                retain_at_most,
                ..StatusJournals::default()
            },
            index: Arc::default(),
        }
    }

    #[test]
    fn a_status_is_read_as_the_snapshot_saw_it() {
        for rotate_at in [1, ROTATE_AT] {
            let mut w = rotating_at(rotate_at);
            let (a, b) = (BlockId(1), BlockId(2));
            w.record(a, VALID_SCRIPTS);
            w.mark(0);
            w.record(a, VALID_TREE);
            w.mark(1);
            w.record(a, VALID_TRANSACTIONS);
            w.record(a, VALID_SCRIPTS);
            w.mark(2);
            w.record(b, VALID_TREE);
            let at = |id, epoch| (rotate_at, w.at(id, epoch));
            let want = |s| (rotate_at, Ok(s));
            assert_eq!(at(a, 0), want(Some(VALID_TREE)), "the first since the build");
            assert_eq!(at(a, 1), want(Some(VALID_TRANSACTIONS)));
            assert_eq!(at(a, 2), want(None), "the arena's");
            assert_eq!(at(b, 0), want(Some(VALID_TREE)), "a later write is found");
            assert_eq!(at(BlockId(3), 0), want(None));
            let journals = if rotate_at == 1 {
                4
            } else {
                1
            };
            assert_eq!(w.journals(), journals, "the writer moves on at a full journal only");
        }
    }

    /// DEC-281 (D68): the writer makes its first journal at its first write, and a snapshot
    /// built before any reads the live status.
    #[test]
    fn the_first_journal_comes_with_the_first_write() {
        let mut w = rotating_at(ROTATE_AT);
        w.mark(0);
        assert_eq!((w.journals(), w.at(BlockId(1), 0)), (0, Ok(None)));
        w.record(BlockId(1), VALID_TREE);
        assert_eq!((w.journals(), w.at(BlockId(1), 0)), (1, Ok(Some(VALID_TREE))));
    }

    /// T18-D9: a second write of an id before the next build appends nothing, and so does not
    /// rotate a full journal; a write of another id, or after a build, does.
    #[test]
    fn a_rewrite_before_the_next_build_appends_nothing() {
        let mut w = rotating_at(1);
        let (a, b) = (BlockId(1), BlockId(2));
        w.mark(0);
        w.record(a, VALID_TREE);
        w.record(a, VALID_TRANSACTIONS);
        assert_eq!((w.index.read().entries, w.journals()), (1, 1), "no entry, no rotation");
        assert_eq!(w.at(a, 0), Ok(Some(VALID_TREE)), "the first old status stands");
        w.record(b, VALID_TREE);
        w.record(a, VALID_TRANSACTIONS);
        w.mark(1);
        w.record(a, VALID_SCRIPTS);
        assert_eq!(w.at(a, 0), Ok(Some(VALID_TREE)));
        assert_eq!(w.at(b, 0), Ok(Some(VALID_TREE)));
        assert_eq!(w.at(a, 1), Ok(Some(VALID_SCRIPTS)), "a write after the build");
    }

    /// Sol BL-224 r1: the gauge counts the open journal's entries as they are written, before
    /// any move on.
    #[test]
    fn the_gauge_counts_the_open_journal() {
        let mut w = rotating_at(ROTATE_AT);
        assert_eq!(w.index.read().entries, 0);
        w.mark(0);
        w.record(BlockId(1), VALID_TREE);
        assert_eq!(w.index.read().entries, 1, "one write, no move on");
        w.record(BlockId(2), VALID_TREE);
        assert_eq!((w.index.read().entries, w.journals()), (2, 1));
    }

    /// D68: within the bound the writer keeps every journal, whether a snapshot is held or not,
    /// and a snapshot reads every write since its build from them.
    #[test]
    fn the_writer_keeps_the_journals_within_the_bound() {
        let mut w = rotating_at(2);
        w.mark(0);
        w.record(BlockId(0), VALID_TREE);
        w.mark(1);
        for i in 1..=10 {
            w.record(BlockId(i), VALID_TREE);
        }
        let kept = JournalRetention {
            entries: 11,
            oldest_epoch: 0,
            expired: 0,
        };
        assert_eq!(w.index.read(), kept, "five full journals and the open one");
        assert_eq!(w.journals(), 6);
        for i in 0..=10 {
            let old = (i > 0).then_some(VALID_TREE);
            assert_eq!(w.at(BlockId(i), 1), Ok(old), "{i}");
        }
        w.mark(2);
        w.record(BlockId(11), VALID_TREE);
        w.record(BlockId(12), VALID_TREE);
        assert_eq!(w.index.read().entries, 13, "no holder, and nothing goes within the bound");
        assert_eq!(w.at(BlockId(5), 0), Ok(Some(VALID_TREE)), "an unheld epoch still reads");
    }

    /// BL-224 (D65, D68): past [`RETAIN_AT_MOST`] the writer drops the oldest journal. A
    /// snapshot that needs it reads expired, every read of it counted; the oldest one kept and a
    /// newer snapshot read as before.
    #[test]
    fn a_snapshot_past_the_bound_expires() {
        let mut w = bounded(1, 4);
        let (a, b) = (BlockId(1), BlockId(2));
        w.mark(0);
        w.record(a, VALID_TREE);
        for i in 0..4 {
            w.mark(i + 1);
            w.record(b, VALID_TREE);
        }
        assert_eq!((w.index.read().oldest_epoch, w.at(b, 0)), (0, Ok(Some(VALID_TREE))));
        w.mark(5);
        w.record(b, VALID_TRANSACTIONS);
        let kept = w.index.read();
        assert_eq!((kept.entries, kept.oldest_epoch), (5, 2), "four full journals and the open");
        assert_eq!(w.at(a, 0), Err(SnapshotExpired), "its journal went");
        assert_eq!(w.at(b, 0), Err(SnapshotExpired));
        assert_eq!(w.at(BlockId(3), 0), Err(SnapshotExpired), "never written");
        assert_eq!(w.at(b, 2), Ok(Some(VALID_TREE)), "the oldest epoch kept");
        assert_eq!(w.at(b, 5), Ok(Some(VALID_TRANSACTIONS)));
        assert_eq!(w.at(BlockId(3), 5), Ok(None));
        assert_eq!(w.index.read().expired, 3, "each expired read");
    }

    /// Validator r1: the newest snapshot never expires. Past the bound with no build between, the
    /// writer keeps its journals, whether one was open at its build or not; once a build follows
    /// them, an older snapshot expires and the newer one still reads.
    #[test]
    fn the_newest_snapshot_never_expires() {
        for open in [false, true] {
            let mut w = bounded(1, 2);
            if open {
                w.record(BlockId(99), VALID_TREE);
            }
            w.mark(0);
            for i in 0..10 {
                w.record(BlockId(i), VALID_TREE);
            }
            assert_eq!(w.journals(), 10 + usize::from(open), "{open}");
            for i in 0..10 {
                assert_eq!(w.at(BlockId(i), 0), Ok(Some(VALID_TREE)), "{open} {i}");
            }
        }

        // `older`'s writes, then `newer`'s.
        let mut w = bounded(1, 2);
        w.mark(0);
        for i in 0..3 {
            w.record(BlockId(i), VALID_TREE);
        }
        w.mark(1);
        for i in 3..10 {
            w.record(BlockId(i), VALID_TRANSACTIONS);
        }
        assert_eq!(w.at(BlockId(0), 0), Err(SnapshotExpired), "`older` is past the bound");
        for i in 3..10 {
            assert_eq!(w.at(BlockId(i), 1), Ok(Some(VALID_TRANSACTIONS)), "{i}");
        }
        assert_eq!(w.at(BlockId(2), 1), Ok(None), "written before `newer`'s build");
    }

    /// Validator r1: the writer drops the reader's next journal between its `next` read and its
    /// log read; the walk ends expired, not at the live status.
    #[test]
    fn a_journal_dropped_under_a_walk_expires_it() {
        let mut w = bounded(1, 1);
        w.mark(0);
        w.record(BlockId(0), VALID_TREE);
        w.mark(1);
        w.record(BlockId(1), VALID_TREE);
        w.mark(2);
        let index = w.index.clone();
        set_hook(Site::Walk, move || {
            w.record(BlockId(2), VALID_TREE);
            w.mark(3);
            w.record(BlockId(3), VALID_TREE);
            assert_eq!(w.index.read().oldest_epoch, 3, "the reader's journal and the next went");
        });
        assert_eq!(index.status_at(BlockId(4), 0), Err(SnapshotExpired));
        assert_eq!(index.read().expired, 1, "the walk's expiry counts");
    }

    /// Validator r1: the writer appends to the reader's open journal, builds, moves on and
    /// appends again while the reader is between that journal's reads; the reader must not step
    /// past the first old status to the second.
    #[test]
    fn a_walk_stops_at_a_journal_still_open() {
        let mut w = rotating_at(2);
        w.mark(0);
        w.record(BlockId(9), VALID_TREE);
        let index = w.index.clone();
        let x = BlockId(1);
        set_hook(Site::Walk, move || {
            w.record(x, VALID_TREE);
            w.mark(1);
            w.record(x, VALID_SCRIPTS);
        });
        assert_eq!(index.status_at(x, 0), Ok(Some(VALID_TREE)));
        assert_eq!(kept(&index), 2, "the writer moved on");
    }
}
