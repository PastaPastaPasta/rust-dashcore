//! Tip steps and the `TipCell` core (SB4 §4, §9.1, §9.2; SB0 §6.1). SB5 adds the feeds.

use std::sync::Arc;

use arc_swap::ArcSwap;
use dash_consensus::BlockId;
use dash_primitives::BlockHash;
use parking_lot::Mutex;

use crate::error::FailStop;
use crate::feed::{FeedConfig, FeedShared, TipFeeds};
use crate::snapshot::ChainReadSnapshot;

/// A block as CH2 names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockRef {
    pub id: BlockId,
    pub hash: BlockHash,
    pub height: u32,
}

/// One tip step and its `seq` (F1, F2): CH2's exact event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipStep {
    Connected {
        block: BlockRef,
        seq: u64,
    },
    Disconnected {
        block: BlockRef,
        seq: u64,
    },
}

impl TipStep {
    pub fn seq(&self) -> u64 {
        match *self {
            TipStep::Connected {
                seq,
                ..
            }
            | TipStep::Disconnected {
                seq,
                ..
            } => seq,
        }
    }

    pub fn block(&self) -> BlockRef {
        match *self {
            TipStep::Connected {
                block,
                ..
            }
            | TipStep::Disconnected {
                block,
                ..
            } => block,
        }
    }
}

/// The result of `set_active_tip`.
#[derive(Clone, Debug)]
pub struct TipChange {
    /// Tip down.
    pub disconnected: Vec<BlockRef>,
    /// Fork up.
    pub connected: Vec<BlockRef>,
    /// The `seq` of the last step (the old `tip_seq` when no step ran). After an abandoned
    /// hold, a node no-op returns the writer's seq beside the older published `snap`.
    pub last_seq: u64,
    /// The snapshot published last (SPV: the step's only one).
    pub snap: Arc<ChainReadSnapshot>,
}

/// The shape of a tip change with no leg materialized (SB5-A11 (c)): the fork with the
/// writer's active chain and the legs' lengths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipTarget {
    pub fork: BlockRef,
    /// Disconnects: tip height − fork height.
    pub d: u32,
    /// Connects: target height − fork height.
    pub c: u32,
}

/// The active-chain diff of a tip change (SPV's WriteSet, §9.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipPlan {
    pub fork: BlockRef,
    /// Tip down.
    pub disconnect: Vec<BlockRef>,
    /// Fork up.
    pub connect: Vec<BlockRef>,
}

/// The tip lock's state (SB0 §6.1). SB5 adds feeds, `pending` and the producer wake.
///
/// Aligned to 16 so its `Arc<Mutex<_>>` keeps the 0xd0-byte block it had with an `epoch` field
/// (D68): 8 bytes less moved every later allocation by 16, and one snapshot copy in two lost its
/// 32-byte alignment (PF0 status-publish-coalesce, +6 instructions a publication in `memcpy`).
#[repr(align(16))]
pub(crate) struct TipState {
    pub(crate) tip: BlockRef,
    pub(crate) next_seq: u64,
    /// Its `epoch` is the last publication's; the status journals count epochs (D68).
    pub(crate) snapshot: Arc<ChainReadSnapshot>,
    /// Node, CH1-A11: no tip yet. The active chain is empty and `tip` names genesis as a
    /// placeholder until V's `init_active_tip` (Core `LoadChainTip`) or `connect_tip(genesis)`.
    pub(crate) pre_genesis: bool,
    /// SB5's feed table (SB5-A14).
    pub(crate) feeds: TipFeeds,
}

/// The tip lock, `seq` and the published snapshot (SB4 §9.2). Readers load `published`
/// without the lock (D6); it is written only inside the lock's critical section, so F2 holds.
pub struct TipCell {
    /// An `Arc` so the node's `enforce_block` can hold an owned guard (§9.1, `lock_arc`).
    pub(crate) state: Arc<Mutex<TipState>>,
    pub(crate) published: ArcSwap<ChainReadSnapshot>,
    /// SB5's feed state outside the lock (SB5-A14).
    pub(crate) shared: Arc<FeedShared>,
}

impl TipCell {
    /// A cell whose first publication is `snap`: SPV's `init_active_tip`, or the node's
    /// pre-genesis snapshot (CH1-A11).
    pub(crate) fn new(
        tip: BlockRef,
        snap: ChainReadSnapshot,
        feeds: FeedConfig,
        fail: Arc<dyn FailStop>,
    ) -> TipCell {
        let snap = Arc::new(snap);
        TipCell {
            state: Arc::new(Mutex::new(TipState {
                tip,
                next_seq: snap.tip_seq + 1,
                pre_genesis: !snap.has_tip(),
                snapshot: snap.clone(),
                feeds: TipFeeds::new(&feeds),
            })),
            published: ArcSwap::new(snap),
            shared: Arc::new(FeedShared::new(feeds, fail)),
        }
    }

    /// The last published snapshot, lock-free.
    pub fn snapshot(&self) -> Arc<ChainReadSnapshot> {
        self.published.load_full()
    }
}

impl TipState {
    /// The state `enforce_block` leaves in the mutex while it walks with the real one (D-C6-1).
    /// No one reads it: the walk holds the mutex.
    pub(crate) fn vacant(&self) -> TipState {
        TipState {
            tip: self.tip,
            next_seq: self.next_seq,
            snapshot: self.snapshot.clone(),
            pre_genesis: self.pre_genesis,
            feeds: TipFeeds::vacant(),
        }
    }

    /// F2: the next step's `seq`.
    pub(crate) fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Stores `snap` as the published snapshot, inside the lock's critical section (D6, F2).
    pub(crate) fn publish(
        &mut self,
        published: &ArcSwap<ChainReadSnapshot>,
        snap: ChainReadSnapshot,
    ) -> Arc<ChainReadSnapshot> {
        debug_assert_eq!(snap.epoch, self.snapshot.epoch + 1, "epochs grow by one per publication");
        let snap = Arc::new(snap);
        self.snapshot = snap.clone();
        published.store(snap.clone());
        snap
    }
}
