//! The subscriber-side handle (SB5 §3.4): `ChainFeeds` (CH2's `FeedSource`), the registry
//! snapshot (H2) and the catch-up snapshot.
//!
//! The table operations are in `table.rs`.

use std::sync::Arc;

use super::table::FeedHandle;
use super::{ControlMsg, FeedSlot, Needs, Refused, SendControl, SubscriberFeed};
use crate::BlockId;
use crate::cold::ColdWalk;
use crate::error::ChainError;
use crate::notify::Notify;
use crate::reader::{ChainReader, EntryView};
use crate::snapshot::ChainReadSnapshot;
use crate::tip::BlockRef;

/// A subscriber's id. Generational: a reused table index gets a new `generation`; every call
/// with a stale id is a no-op (D3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FeedId {
    pub(crate) index: u32,
    pub(crate) generation: u32,
}

/// The subscriber-side handle: register, deregister, control, the registry. `Clone + Send +
/// Sync`. No call ever blocks on the tip lock (DEC-157 (2)).
#[derive(Clone)]
pub struct ChainFeeds {
    pub(crate) handle: FeedHandle,
    /// An owning reader (SB5-A15): the catch-up snapshot, and F5's reads below its window.
    pub(crate) reader: ChainReader,
    /// The snapshot ranges' cold walk, past the cache (§5.3).
    pub(crate) cold: Arc<dyn ColdWalk>,
}

impl ChainFeeds {
    /// P8. Allocates the slot and its three Q-sized buffers before the tip lock, then installs it
    /// inactive. While a hold is open it returns `HoldActive` (§4.3).
    pub fn register(&self, needs: Needs) -> Result<FeedId, Refused> {
        self.handle.register(needs)
    }

    /// F11. Sets `closing`, notifies the producer and W, then reclaims the entry only if the tip
    /// lock is free; otherwise the writer's next scan or the next `register` does. Never blocks.
    pub fn deregister(&self, id: FeedId) {
        self.handle.deregister(id);
    }

    /// A one-shot wake when the current hold is released. Protocol: on `HoldActive`,
    /// `on_release(w)`, retry once, and wait for `w` only if the retry is `HoldActive` again.
    /// Each call adds a wake until the release, so a caller must wait as the protocol says
    /// rather than call it in a loop.
    pub fn on_release(&self, w: Notify) {
        self.handle.shared.on_release(w);
    }

    /// F4: out of band, no tip lock; notifies the subscriber. A same-kind message that is still
    /// outstanding has its payload replaced (DEC-155 (1)).
    pub fn send_control(&self, id: FeedId, c: ControlMsg) -> SendControl {
        self.handle.send_control(id, c)
    }

    /// The outstanding control kinds, as a bit set; 0 for a stale id.
    pub fn control_outstanding(&self, id: FeedId) -> u8 {
        self.handle.control_outstanding(id)
    }

    /// H2: the lock-free registry snapshot W captures from.
    pub fn registry(&self) -> Arc<Registry> {
        self.handle.shared.registry.load()
    }

    /// True once `id` is closing, and for a stale id.
    pub fn is_closing(&self, id: FeedId) -> bool {
        self.handle.is_closing(id)
    }

    /// W's wake on any slot closing (H4). Set once.
    pub fn set_close_hook(&self, w: Notify) {
        self.handle.shared.set_close_hook(w);
    }
}

/// The feed table as W sees it, republished under the tip lock on every change (H2).
pub struct Registry {
    pub epoch: u64,
    pub feeds: Vec<RegistryEntry>,
}

pub struct RegistryEntry {
    pub id: FeedId,
    pub needs: Needs,
    /// `None` while inactive.
    pub seq_synced: Option<u64>,
    pub(crate) slot: Arc<FeedSlot>,
}

impl RegistryEntry {
    /// H7(b): filter after reading the snapshot.
    pub fn is_closing(&self) -> bool {
        self.slot.is_closing()
    }
}

/// CH2's feed source (DEC-151 OQ1), generic over the handoff's finish error (D10).
pub trait FeedSource<E>: Send + Sync {
    fn snapshot(&self) -> Box<dyn ChainSnapshot + '_>;

    /// F10: if `p` is the tip, runs `finish()` and makes the slot live at `next_seq`, atomically
    /// under the tip lock. A `register` or `subscribe_at` from inside `finish()` is
    /// `WouldDeadlock`; a writer call that locks the tip, such as
    /// `HeaderChain::pending_is_empty`, deadlocks there.
    fn subscribe_at(
        &self,
        id: FeedId,
        p: &BlockRef,
        finish: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<Handoff, SubscribeError<E>>;
}

impl ChainFeeds {
    /// The last published snapshot with its checked reads (§5.3); `FeedSource::snapshot` boxes
    /// the same.
    pub fn feed_snapshot(&self) -> FeedSnapshot {
        FeedSnapshot {
            snap: self.reader.snapshot(),
            reader: self.reader.clone(),
            cold: self.cold.clone(),
        }
    }
}

impl<E> FeedSource<E> for ChainFeeds {
    fn snapshot(&self) -> Box<dyn ChainSnapshot + '_> {
        Box::new(self.feed_snapshot())
    }

    fn subscribe_at(
        &self,
        id: FeedId,
        p: &BlockRef,
        finish: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<Handoff, SubscribeError<E>> {
        self.handle.subscribe_at(id, p, finish)
    }
}

/// The handoff's result.
pub enum Handoff {
    Live {
        feed: Box<dyn SubscriberFeed>,
        seq_synced: u64,
    },
    /// The tip moved (or the node is pre-genesis): catch up again.
    Moved,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SubscribeError<E> {
    Stale,
    AlreadyLive,
    Closing,
    Latched,
    /// A hold is open: `on_release`, retry (§4.3).
    HoldActive,
    /// Called on the hold owner's thread, or from inside `finish()`.
    WouldDeadlock,
    Finish(E),
}

/// CH2's catch-up view of the chain.
pub trait ChainSnapshot {
    /// Node: called only after genesis (m6).
    fn tip(&self) -> BlockRef;
    fn tip_seq(&self) -> u64;
    fn at_height(&self, h: u32) -> Option<BlockRef>;
}

/// One published snapshot and the reader that resolves heights below its window (§5.3).
pub struct FeedSnapshot {
    pub(super) snap: Arc<ChainReadSnapshot>,
    pub(super) reader: ChainReader,
    pub(super) cold: Arc<dyn ColdWalk>,
}

impl FeedSnapshot {
    /// `at_height`, with a cold error returned rather than mapped to `None` (§5.3). `None` above
    /// the tip and below the floor. Below the window's base `b`, it is `ancestor(V.at(b), h)`
    /// over immutable links, resident or through the cold cache, so always on the snapshot's
    /// chain; it never reads `active_at`.
    pub fn at_height_checked(&self, h: u32) -> Result<Option<BlockRef>, ChainError> {
        if !self.snap.has_tip() || h > self.snap.active.height() {
            return Ok(None);
        }
        let Some(id) = self.id_at(h)? else {
            return Ok(None);
        };
        self.block(id).map(Some)
    }

    /// Heights `lo..=hi` on the snapshot's chain, ascending (§5.3). Heights above the tip and
    /// below the floor are not returned. One walk down `prev` from `hi`'s block: resident
    /// entries in place, cold runs by `walk_down` in batches of `WALK_BATCH`, past the cache
    /// (D14). The view can hold cold heights too (SB4 D-C4-7), so the walk covers the window as
    /// well (D42).
    pub fn range(&self, lo: u32, hi: u32) -> Result<Vec<BlockRef>, ChainError> {
        if !self.snap.has_tip() {
            return Ok(Vec::new());
        }
        let (lo, hi) = (lo.max(self.reader.backend.floor()), hi.min(self.snap.active.height()));
        if lo > hi {
            return Ok(Vec::new());
        }
        let mut out = self.walk(self.id_at(hi)?, hi, lo)?;
        out.reverse();
        Ok(out)
    }

    pub fn read(&self) -> &Arc<ChainReadSnapshot> {
        &self.snap
    }

    /// `V.at(h)`, or `ancestor(V.at(b), h)` below the window.
    fn id_at(&self, h: u32) -> Result<Option<BlockId>, ChainError> {
        match self.snap.active.at(h) {
            Some(id) => Ok(Some(id)),
            None => self.reader.try_ancestor(self.base_id(), h),
        }
    }

    /// Where the walks below the window start: the view's lowest entry, or the tip when the
    /// window holds none (a base at the view's length). The view has a tip.
    fn base_id(&self) -> BlockId {
        let v = &self.snap.active;
        v.at(v.base()).unwrap_or(self.snap.tip.tip)
    }

    /// `at` (at height `hi`) and its ancestors down to `lo`, descending.
    fn walk(&self, mut at: Option<BlockId>, hi: u32, lo: u32) -> Result<Vec<BlockRef>, ChainError> {
        let mut out = Vec::with_capacity((hi - lo) as usize + 1);
        while let Some(id) = at {
            if let Some(e) = self.reader.entry(id) {
                out.push(to_ref(&e));
                at = e.prev.filter(|_| e.height > lo);
                continue;
            }
            // A cold run: this batch, plus the next batch's first record to name its id.
            let h = out.last().map_or(hi, |r| r.height - 1);
            let want = (h - lo + 1).min(WALK_BATCH + 1);
            let n = want.min(WALK_BATCH) as usize;
            let recs = self.cold.walk_down(id, want)?;
            out.extend(recs[..n].iter().map(|r| BlockRef {
                id: r.id,
                hash: r.hash,
                height: r.height,
            }));
            at = recs.get(n).map(|r| r.id);
        }
        Ok(out)
    }

    /// A snapshot's block, resident or cold; one with neither entry nor record is corruption,
    /// which trips the latch.
    fn block(&self, id: BlockId) -> Result<BlockRef, ChainError> {
        match self.reader.try_entry(id)? {
            Some(e) => Ok(to_ref(&e)),
            None => Err(self.reader.backend.missing(id).into()),
        }
    }
}

/// The records one `walk_down` of a snapshot range reads (§5.3).
const WALK_BATCH: u32 = 1_024;

fn to_ref(e: &EntryView) -> BlockRef {
    BlockRef {
        id: e.id,
        hash: e.hash,
        height: e.height,
    }
}

impl ChainSnapshot for FeedSnapshot {
    fn tip(&self) -> BlockRef {
        let t = &self.snap.tip;
        BlockRef {
            id: t.tip,
            hash: t.tip_hash,
            height: t.tip_height,
        }
    }

    fn tip_seq(&self) -> u64 {
        self.snap.tip_seq
    }

    /// A cold error trips the latch and returns `None` (§5.4).
    fn at_height(&self, h: u32) -> Option<BlockRef> {
        self.at_height_checked(h).ok().flatten()
    }
}
