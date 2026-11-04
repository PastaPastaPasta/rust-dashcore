//! The feed table under the tip lock (SB5 §3.2, §4.3): entries, cursors and the pending backlog;
//! fanout and `retry_pending` for the writer; and the subscriber side's tip-lock operations
//! (`register`, `subscribe_at`, `deregister`'s step 4), which never block on the tip lock
//! (DEC-157 (2)).
//!
//! The table is reached through [`TipLock`]: `feed.rs` implements it for SB4's
//! `Mutex<TipState>`, whose `feeds` field is a [`TipFeeds`] (SB5-A14); the tests implement it for
//! a model tip.

use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::ops::Deref;
use std::sync::Arc;

use super::StatusKey;
use super::reader::FeedReader;
use super::source::{FeedId, Handoff, Registry, RegistryEntry, SubscribeError};
use super::{
    BlockData, ChainEvent, ControlMsg, FeedConfig, FeedShared, FeedSlot, Needs, Publish, Push,
    ReaderBuffers, Refused, SendControl,
};
use crate::error::ChainError;
use crate::tip::{BlockRef, TipStep};

/// The tip facts the subscriber side reads under the tip lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TipCtx {
    pub(crate) tip: BlockRef,
    pub(crate) next_seq: u64,
    /// CH1-A11: no tip yet; `subscribe_at` returns `Moved`.
    pub(crate) pre_genesis: bool,
}

/// The tip lock as the subscriber side takes it: only ever by `try_lock` (§4.3).
pub(crate) trait TipLock: Send + Sync {
    /// Runs `f` under the tip lock if the lock is free now; false if it is held.
    fn try_with(&self, f: &mut dyn FnMut(&mut TipFeeds, TipCtx)) -> bool;
}

/// A step's events as the writer hands them to fanout, after their `seq`s (SB5-A14).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Fanout<'a> {
    /// A publication with no tip step (status only).
    None,
    /// One node step.
    One(ChainEvent),
    /// One SPV composite: `disconnected` tip down, then `connected` fork up, with consecutive
    /// `seq`s from `first_seq`.
    Composite {
        first_seq: u64,
        disconnected: &'a [BlockRef],
        connected: &'a [BlockRef],
    },
}

impl<'a> Fanout<'a> {
    /// The events; `None` for a status-only publication.
    fn events(self) -> Option<Events<&'a [BlockRef]>> {
        match self {
            Fanout::None => None,
            Fanout::One(e) => Some(Events::One(e)),
            Fanout::Composite {
                first_seq,
                disconnected,
                connected,
            } => Some(Events::Composite {
                first: first_seq,
                disconnected,
                connected,
            }),
        }
    }

    /// One past the last event's `seq`; `None` for a status-only publication.
    pub(super) fn end(self) -> Option<u64> {
        self.events().map(|e| e.end())
    }
}

/// The events of one step (node) or one composite (SPV), with consecutive `seq`s from
/// `first()`: borrowed by fanout, and held once in `pending` whatever the number of targets
/// (§3.2, §6.1).
enum Events<B> {
    One(ChainEvent),
    Composite {
        first: u64,
        disconnected: B,
        connected: B,
    },
}

/// A pending segment.
type Segment = Events<Box<[BlockRef]>>;

impl<B: Deref<Target = [BlockRef]>> Events<B> {
    fn first(&self) -> u64 {
        match self {
            Events::One(e) => e.seq(),
            Events::Composite {
                first,
                ..
            } => *first,
        }
    }

    /// One past the last `seq`.
    fn end(&self) -> u64 {
        match self {
            Events::One(e) => e.seq() + 1,
            Events::Composite {
                first,
                disconnected,
                connected,
            } => first + (disconnected.len() + connected.len()) as u64,
        }
    }

    /// The event at `seq`: a composite's disconnects first (tip down), then its connects (§4.8).
    fn event(&self, seq: u64) -> ChainEvent {
        debug_assert!((self.first()..self.end()).contains(&seq), "seq {seq} outside the events");
        match self {
            Events::One(e) => *e,
            Events::Composite {
                first,
                disconnected,
                connected,
            } => {
                let i = (seq - first) as usize;
                match disconnected.get(i) {
                    Some(&block) => TipStep::Disconnected {
                        block,
                        seq,
                    },
                    None => TipStep::Connected {
                        block: connected[i - disconnected.len()],
                        seq,
                    },
                }
            }
        }
    }
}

impl Events<&[BlockRef]> {
    fn to_segment(&self) -> Segment {
        match *self {
            Events::One(e) => Events::One(e),
            Events::Composite {
                first,
                disconnected,
                connected,
            } => Events::Composite {
                first,
                disconnected: disconnected.into(),
                connected: connected.into(),
            },
        }
    }
}

/// A `seq`: every step's is at least 1 (`next_seq` starts there), so `Option<Seq>` is 8 bytes.
type Seq = NonZeroU64;

/// One table index (§3.2). Slot objects are never reused: a new registration at the index gets
/// a new slot and the next `generation` (D3).
#[derive(Default)]
struct TableEntry {
    generation: u32,
    slot: Option<Arc<FeedSlot>>,
    needs: Needs,
    /// `seq_synced` once `subscribe_at` made the slot live (F10); `None` while inactive.
    live: Option<Seq>,
    /// The `seq` of the first event this live target has not received; `None` = caught up.
    cursor: Option<Seq>,
    /// The reader's buffers, reserved by `register` before the tip lock; `subscribe_at` builds
    /// the reader on them.
    bufs: Option<ReaderBuffers>,
}

impl TableEntry {
    /// Holds `id`'s slot: the index's current generation, not retired.
    fn holds(&self, id: FeedId) -> Option<&Arc<FeedSlot>> {
        self.slot.as_ref().filter(|_| self.generation == id.generation)
    }

    fn id(&self, index: usize) -> FeedId {
        FeedId {
            index: index as u32,
            generation: self.generation,
        }
    }
}

/// One table entry's bytes, in §6.1's fixed term.
pub(super) const TABLE_ENTRY_BYTES: usize = size_of::<TableEntry>();

/// The feeds' part of the tip lock's state (SB5-A14: `TipState.feeds`).
pub(crate) struct TipFeeds {
    entries: Vec<TableEntry>,
    /// Indices whose entry was removed; never one whose generation is spent.
    free: Vec<u32>,
    /// The events some live target still lacks, in `seq` order, contiguous (§3.2). The writer
    /// rule leaves at most one step's segment here, so [`TipFeeds::new`] reserves one slot and
    /// a step never allocates the queue (§6.2's fixed term).
    pending: VecDeque<Segment>,
    max_feeds: u32,
    /// The registry's epoch: +1 on every table change (H2).
    epoch: u64,
    /// The key of the status facts the writer last stored in the status cell (§3.6),
    /// [`StatusKey::UNSET`] before the first: an unchanged step compares it and loads nothing
    /// from the cell.
    pub(crate) status: StatusKey,
    #[cfg(test)]
    pub(crate) faults: TableFaults,
}

/// Planted negative controls for the loom models and directed tests (§10.2).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TableFaults {
    /// L3: `retry_pending` keeps a closing target's cursor.
    pub(crate) retry_ignores_closing: bool,
    /// L3: `deregister`'s step 4 removes the entry at the index without `ptr_eq`.
    pub(crate) cleanup_without_ptr_eq: bool,
    /// L4: `subscribe_at` gives the new slot a cursor at the oldest pending event (rev2's).
    pub(crate) cursor_at_subscribe: bool,
}

impl TipFeeds {
    pub(crate) fn new(cfg: &FeedConfig) -> TipFeeds {
        TipFeeds {
            entries: Vec::with_capacity(cfg.max_feeds as usize),
            free: Vec::with_capacity(cfg.max_feeds as usize),
            pending: VecDeque::with_capacity(1),
            max_feeds: cfg.max_feeds,
            epoch: 0,
            status: StatusKey::UNSET,
            #[cfg(test)]
            faults: TableFaults::default(),
        }
    }

    /// The table in the tip mutex while `enforce_block` walks with the real one (SB4 D-C6-1):
    /// never read, and allocation-free.
    pub(crate) fn vacant() -> TipFeeds {
        TipFeeds {
            entries: Vec::new(),
            free: Vec::new(),
            pending: VecDeque::new(),
            max_feeds: 0,
            epoch: 0,
            status: StatusKey::UNSET,
            #[cfg(test)]
            faults: TableFaults::default(),
        }
    }

    /// Whether some live target still lacks events: the writer's rule (no tip step while
    /// pending) and its wait read it.
    pub(crate) fn pending_is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// The writer's rule (§3.3): no tip step starts while some live target lacks earlier
    /// events. The `debug_assert` is in addition to the error, which release builds return.
    pub(crate) fn before_step(&self) -> Result<(), ChainError> {
        let empty = self.pending_is_empty();
        debug_assert!(empty, "tip step with a pending fanout");
        match empty {
            true => Ok(()),
            false => Err(ChainError::Internal("tip step with a pending fanout")),
        }
    }

    fn publish_state(&self) -> Publish {
        if self.lowest_cursor().is_some() {
            Publish::Blocked
        } else {
            Publish::Done
        }
    }

    fn lowest_cursor(&self) -> Option<u64> {
        self.entries.iter().filter_map(|e| e.cursor).map(Seq::get).min()
    }

    /// Fanout (§4.3), inside the step's critical section after its `seq`s (and, outside a hold,
    /// the snapshot store). Targets are the live, non-closing entries at this moment (X28);
    /// a target with a cursor receives nothing directly (F1). `Blocked` iff a target lacks
    /// events.
    #[inline]
    pub(crate) fn fanout(&mut self, shared: &FeedShared, ev: Fanout<'_>) -> Publish {
        // No entry ever registered: nothing to push, no cursor, no backlog. Inlined, so a chain
        // with no subscriber pays one test per step (PF0).
        if self.entries.is_empty() {
            debug_assert!(self.pending.is_empty(), "a backlog needs a cursor");
            return Publish::Done;
        }
        self.fanout_to_entries(shared, ev)
    }

    fn fanout_to_entries(&mut self, shared: &FeedShared, ev: Fanout<'_>) -> Publish {
        let Some(ev) = ev.events() else {
            return self.publish_state();
        };
        let (first, end) = (ev.first(), ev.end());
        let mut reclaimed = false;
        for (index, e) in self.entries.iter_mut().enumerate() {
            let Some(slot) = &e.slot else {
                continue;
            };
            if slot.is_closing() {
                retire(e, index, &mut self.free);
                reclaimed = true;
                continue;
            }
            let Some(live) = e.live.map(Seq::get) else {
                continue;
            };
            if e.cursor.is_some() {
                continue;
            }
            debug_assert!(first >= live, "a target's events start at its cut");
            // A closing target gets no cursor: the next scan retires it.
            if let (next, true) = push_run(slot, first, end, |seq| ev.event(seq)) {
                e.cursor = Seq::new(next);
            }
        }
        // Any cursor lacks these events: each cursor is below `end`, and the backlog stays
        // contiguous from the lowest one.
        if self.lowest_cursor().is_some() {
            debug_assert!(
                self.pending.back().is_none_or(|s| s.end() == first),
                "the backlog stays contiguous: every step is fanned out"
            );
            self.pending.push_back(ev.to_segment());
        }
        if reclaimed {
            self.retired(shared);
        }
        self.publish_state()
    }

    /// `retry_pending` (§4.3): pushes what fits to each target from its cursor, in `seq` order;
    /// a closing target's cursor is cleared (F11). Drops the segments below the lowest cursor.
    pub(crate) fn retry_pending(&mut self, shared: &FeedShared) -> Publish {
        let end = self.pending.back().map_or(0, Segment::end);
        let mut reclaimed = false;
        for (index, e) in self.entries.iter_mut().enumerate() {
            let Some(slot) = &e.slot else {
                continue;
            };
            let closing = slot.is_closing();
            #[cfg(test)]
            let closing = closing && !self.faults.retry_ignores_closing;
            if closing {
                retire(e, index, &mut self.free);
                reclaimed = true;
                continue;
            }
            let Some(c) = e.cursor.map(Seq::get) else {
                continue;
            };
            let (next, _) = push_run(slot, c, end, |seq| pending_event(&self.pending, seq));
            e.cursor = Seq::new(next).filter(|_| next < end);
        }
        self.trim_pending();
        if reclaimed {
            self.republish(shared);
        }
        self.publish_state()
    }

    /// Drops the leading segments below the lowest cursor, all of them if none remains. Runs
    /// after every cursor clear, a retire's included: a backlog no cursor needs would report
    /// `pending` with the writer `Done`.
    fn trim_pending(&mut self) {
        match self.lowest_cursor() {
            None => self.pending.clear(),
            Some(low) => {
                while self.pending.front().is_some_and(|s| s.end() <= low) {
                    self.pending.pop_front();
                }
            }
        }
    }

    /// `register` step 3 (§4.3): reclaim closing entries, take an index, install the slot
    /// inactive, republish.
    fn install(
        &mut self,
        shared: &FeedShared,
        needs: Needs,
        slot: &Arc<FeedSlot>,
        bufs: &mut Option<ReaderBuffers>,
    ) -> Result<FeedId, Refused> {
        if self.reclaim_closing() {
            self.retired(shared);
        }
        let index = match self.free.pop() {
            Some(i) => i as usize,
            None if self.entries.len() < self.max_feeds as usize => {
                self.entries.push(TableEntry::default());
                self.entries.len() - 1
            }
            None => return Err(Refused::TableFull),
        };
        let e = &mut self.entries[index];
        e.generation += 1;
        e.slot = Some(slot.clone());
        e.needs = needs;
        e.bufs = bufs.take();
        debug_assert!(e.live.is_none() && e.cursor.is_none(), "a free entry is clear");
        let id = e.id(index);
        self.republish(shared);
        Ok(id)
    }

    /// `subscribe_at`'s critical section (§4.3, F10).
    fn activate<E>(
        &mut self,
        shared: &Arc<FeedShared>,
        ctx: TipCtx,
        id: FeedId,
        p: &BlockRef,
        finish: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<Handoff, SubscribeError<E>> {
        let e = self.entries.get_mut(id.index as usize).ok_or(SubscribeError::Stale)?;
        let slot = e.holds(id).ok_or(SubscribeError::Stale)?.clone();
        if slot.is_closing() {
            return Err(SubscribeError::Closing);
        }
        if e.live.is_some() {
            return Err(SubscribeError::AlreadyLive);
        }
        if ctx.pre_genesis || ctx.tip.id != p.id || ctx.tip.hash != p.hash {
            return Ok(Handoff::Moved);
        }
        finish().map_err(SubscribeError::Finish)?;
        // One value for the handoff and the registry (H2).
        let live = Seq::new(ctx.next_seq).expect("seqs start at 1");
        e.live = Some(live);
        #[cfg(test)]
        if self.faults.cursor_at_subscribe
            && let Some(s) = self.pending.front()
        {
            e.cursor = Seq::new(s.first());
        }
        let bufs = e.bufs.take().expect("an inactive entry holds its buffers");
        let feed = Box::new(FeedReader::new(slot, shared.clone(), bufs));
        self.republish(shared);
        Ok(Handoff::Live {
            feed,
            seq_synced: live.get(),
        })
    }

    /// `deregister` step 4 (§4.3): removes the entry only if it still holds this very slot.
    pub(super) fn remove(&mut self, shared: &FeedShared, id: FeedId, slot: &Arc<FeedSlot>) {
        let Some(e) = self.entries.get_mut(id.index as usize) else {
            return;
        };
        let ours = e.holds(id).is_some_and(|s| Arc::ptr_eq(s, slot));
        #[cfg(test)]
        let ours = ours || self.faults.cleanup_without_ptr_eq;
        if ours {
            retire(e, id.index as usize, &mut self.free);
            self.retired(shared);
        }
    }

    /// After a retire: the retired cursor may have held the backlog (`trim_pending`), and the
    /// registry changed.
    fn retired(&mut self, shared: &FeedShared) {
        self.trim_pending();
        self.republish(shared);
    }

    /// The lazy reclaim (§4.3): every entry whose slot is closing. True if any was.
    fn reclaim_closing(&mut self) -> bool {
        let mut any = false;
        for (index, e) in self.entries.iter_mut().enumerate() {
            if e.slot.as_ref().is_some_and(|s| s.is_closing()) {
                retire(e, index, &mut self.free);
                any = true;
            }
        }
        any
    }

    /// H2: the registry, republished on every table change. Its entries are reserved at
    /// `max_feeds`, never grown: §6.2's `registry_bytes`.
    fn republish(&mut self, shared: &FeedShared) {
        self.epoch += 1;
        let mut feeds = Vec::with_capacity(self.max_feeds as usize);
        feeds.extend(self.entries.iter().enumerate().filter_map(|(index, e)| {
            Some(RegistryEntry {
                id: e.id(index),
                needs: e.needs,
                seq_synced: e.live.map(Seq::get),
                slot: e.slot.clone()?,
            })
        }));
        shared.registry.store(Arc::new(Registry {
            epoch: self.epoch,
            feeds,
        }));
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self, id: FeedId) -> Option<u64> {
        self.entries.get(id.index as usize).and_then(|e| e.holds(id).and(e.cursor).map(Seq::get))
    }

    #[cfg(test)]
    pub(crate) fn pending_segments(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub(crate) fn pending_capacity(&self) -> usize {
        self.pending.capacity()
    }

    /// Every index is either occupied or free, and free at most once: each entry is reclaimed
    /// exactly once.
    #[cfg(test)]
    pub(crate) fn assert_consistent(&self) {
        let mut free = self.free.clone();
        free.sort_unstable();
        free.dedup();
        assert_eq!(free.len(), self.free.len(), "an index is freed twice");
        for &i in &free {
            assert!(self.entries[i as usize].slot.is_none(), "a free index holds a slot");
        }
        // The backlog runs contiguously from the lowest cursor, and is empty without one.
        match self.lowest_cursor() {
            None => assert!(self.pending.is_empty(), "a backlog no cursor needs"),
            Some(low) => {
                let front = self.pending.front().expect("a cursor without a backlog");
                assert!(front.first() <= low && low < front.end(), "the front starts the backlog");
                for (a, b) in self.pending.iter().zip(self.pending.iter().skip(1)) {
                    assert_eq!(a.end(), b.first(), "the backlog is contiguous");
                }
            }
        }
    }

    /// Retires `id`'s entry as if its generation were spent: the index is never reused.
    #[cfg(test)]
    pub(crate) fn retire_spent(&mut self, id: FeedId) {
        let e = &mut self.entries[id.index as usize];
        e.generation = u32::MAX;
        retire(e, id.index as usize, &mut self.free);
    }

    /// Entries holding a slot (live, inactive or closing but not yet reclaimed).
    #[cfg(test)]
    pub(crate) fn occupied(&self) -> usize {
        self.entries.iter().filter(|e| e.slot.is_some()).count()
    }
}

/// Removes an entry and frees its index, unless its generation is spent (§3.2).
fn retire(e: &mut TableEntry, index: usize, free: &mut Vec<u32>) {
    let generation = e.generation;
    *e = TableEntry {
        generation,
        ..TableEntry::default()
    };
    if generation != u32::MAX {
        free.push(index as u32);
    }
}

/// Pushes `from..end` to `slot` in `seq` order until it refuses, then wakes its reader once,
/// after the slot sections. Returns the first `seq` not pushed, and whether the slot was full.
fn push_run(
    slot: &FeedSlot,
    from: u64,
    end: u64,
    event: impl Fn(u64) -> ChainEvent,
) -> (u64, bool) {
    let mut wake = None;
    let mut seq = from;
    let mut full = false;
    while seq < end {
        match slot.try_push(event(seq)) {
            Push::Pushed(w) => {
                wake = w;
                seq += 1;
            }
            Push::Full => {
                full = true;
                break;
            }
            Push::Closed => break,
        }
    }
    if let Some(w) = wake {
        w.notify();
    }
    (seq, full)
}

/// The pending event at `seq`; the backlog is contiguous.
fn pending_event(pending: &VecDeque<Segment>, seq: u64) -> ChainEvent {
    pending.iter().find(|s| seq < s.end()).expect("a cursor points into the backlog").event(seq)
}

/// The subscriber-side handle on the table (§3.4), without the snapshot reader: `ChainFeeds`
/// adds that. `Clone + Send + Sync`; no call blocks on the tip lock (DEC-157 (2)).
#[derive(Clone)]
pub(crate) struct FeedHandle {
    pub(crate) tip: Arc<dyn TipLock>,
    pub(crate) shared: Arc<FeedShared>,
}

impl FeedHandle {
    /// Runs `f` under the tip lock, taken by `try_lock` only (§4.3): `HoldActive` or
    /// `WouldDeadlock` while a hold is open, `WouldDeadlock` from inside a handoff's `finish()`,
    /// else retried across an ordinary step section.
    fn with_tip<T>(&self, mut f: impl FnMut(&mut TipFeeds, TipCtx) -> T) -> Result<T, Refused> {
        self.shared.acquire_tip(|| {
            let mut out = None;
            self.tip.try_with(&mut |feeds, ctx| out = Some(f(feeds, ctx)));
            out
        })
    }

    /// P8 (§4.3): the policy check, then the slot, its reader and three Q-sized buffers before
    /// the tip lock; installed inactive.
    pub(crate) fn register(&self, needs: Needs) -> Result<FeedId, Refused> {
        let cfg = &self.shared.cfg;
        if self.shared.is_latched() {
            return Err(Refused::Latched);
        }
        if cfg.block_data == BlockData::None {
            if needs.block {
                return Err(Refused::NoBlockData);
            }
            if needs.undo {
                return Err(Refused::NoUndo);
            }
        }
        let (slot, bufs) = FeedSlot::new(cfg.capacity);
        let mut bufs = Some(bufs);
        self.with_tip(|feeds, _| feeds.install(&self.shared, needs, &slot, &mut bufs))?
    }

    /// F10 (§4.3): if `p` is the tip, runs `finish()` and makes the slot live at `next_seq`, with
    /// no cursor, in one critical section of the tip lock.
    pub(crate) fn subscribe_at<E>(
        &self,
        id: FeedId,
        p: &BlockRef,
        finish: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<Handoff, SubscribeError<E>> {
        if self.shared.is_latched() {
            return Err(SubscribeError::Latched);
        }
        let mut finish = || self.shared.in_finish(&mut *finish);
        self.with_tip(|feeds, ctx| feeds.activate(&self.shared, ctx, id, p, &mut finish))
            .unwrap_or_else(|r| match r {
                Refused::HoldActive => Err(SubscribeError::HoldActive),
                Refused::WouldDeadlock => Err(SubscribeError::WouldDeadlock),
                other => unreachable!("acquire_tip refuses only for holds and finish: {other}"),
            })
    }

    /// F11 (§4.3): steps 1–3 lock-free, then step 4 only if the tip lock is free now; otherwise
    /// the writer's next scan or the next `register` reclaims the entry. Never blocks. A stale id
    /// is a no-op.
    pub(crate) fn deregister(&self, id: FeedId) {
        let Some(slot) = self.resolve(id) else {
            return;
        };
        slot.close(&self.shared);
        #[cfg(all(test, not(loom)))]
        self.shared.faults.before_step4.hit();
        self.tip.try_with(&mut |feeds, _| feeds.remove(&self.shared, id, &slot));
    }

    /// F4 (§4.3): out of band, no tip lock.
    pub(crate) fn send_control(&self, id: FeedId, c: ControlMsg) -> SendControl {
        match self.resolve(id) {
            Some(slot) => slot.send_control(c),
            None => SendControl::Stale,
        }
    }

    /// The outstanding kinds, as a bit set; 0 for a stale id.
    pub(crate) fn control_outstanding(&self, id: FeedId) -> u8 {
        self.resolve(id).map_or(0, |s| s.control_outstanding())
    }

    /// True once `id` is closing, and for a stale id.
    pub(crate) fn is_closing(&self, id: FeedId) -> bool {
        self.resolve(id).is_none_or(|s| s.is_closing())
    }

    /// `id`'s slot through the lock-free registry (§4.3 `send_control` step 1).
    fn resolve(&self, id: FeedId) -> Option<Arc<FeedSlot>> {
        let reg = self.shared.registry.load();
        reg.feeds.iter().find(|e| e.id == id).map(|e| e.slot.clone())
    }
}
