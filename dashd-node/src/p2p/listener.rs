//! P3's `ValidationInterface` on V's signals (P3 §2.5): `BlockChecked` onto the processor's
//! ordered queue, and the queued half, `UpdatedBlockTip` and `BlockConnected`, from the signals
//! thread (S6a); `NewPoWValidBlock` whole on V (C8), and V's waits on the queue at its release
//! points (C5) and before an accept outside IBD (C5′), through V's own condition wait (S6b).

use std::sync::Arc;

use crossbeam_channel::Receiver;
use dash_clock::Clock;
use dash_consensus::{Block, BlockId};
use dash_p2p::NetCore;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{
    self, BlockState, ChainViewHandle, NewBlock, ValidationInterface, WaitError,
};
use dashd_p2p::Shared;
use dashd_p2p::processing::headers::MAX_BLOCKS_TO_ANNOUNCE;
use dashd_p2p::processing::seams::{EntryFacts, Interrupted, TreeRead};
use dashd_p2p::processing::signals::{ServiceWait, SignalQueue, WaitEvent};

use super::NodeChain;
use super::chain::facts;

/// Core's `PeerManagerImpl` as a validation listener.
pub struct Listener {
    pub(super) shared: Arc<Shared>,
    pub(super) core: Arc<NetCore>,
    pub(super) chain: Arc<NodeChain>,
    pub(super) view: ChainViewHandle,
    pub(super) clock: Arc<dyn Clock>,
    /// V's wake while it waits on P3's queue: the queue's notify hook rings it, into a channel
    /// of one slot (CH1 A-1b D67 N3, R2-2), and only V, in its one wait at a time, reads it.
    pub(super) wake: Receiver<()>,
}

impl Listener {
    /// The queue's notify hook rings `wake` from here on.
    pub(super) fn wake_on(queue: &SignalQueue) -> Receiver<()> {
        let (ring, wake) = crossbeam_channel::bounded(1);
        // A full slot already carries a ring.
        queue.on_notify(Box::new(move || {
            let _ = ring.try_send(());
        }));
        wake
    }

    /// One of P3's queue waits, served by V's `wait` meanwhile. Holds no lock across it: a
    /// command V serves in the wait may call `block_checked` here (D67 N2). V's error comes back
    /// as V raised it, its stop or an `AbortNode` (D67 N1).
    fn wait_on_queue(
        &self,
        wait: &mut dyn chain::ServiceWait,
        until: fn(&SignalQueue, &mut dyn ServiceWait) -> Result<(), Interrupted>,
    ) -> Result<(), WaitError> {
        let mut on_v = OnV {
            v: wait,
            wake: &self.wake,
            err: None,
        };
        until(&self.shared.signals().queue, &mut on_v)
            .map_err(|Interrupted| on_v.err.expect("`OnV` keeps V's error"))
    }
}

/// P3's [`ServiceWait`] as V's `wait_until` on the queue's wake (C5, D110).
struct OnV<'a> {
    v: &'a mut dyn chain::ServiceWait,
    wake: &'a Receiver<()>,
    err: Option<WaitError>,
}

impl ServiceWait for OnV<'_> {
    fn wait(
        &mut self,
        _event: &WaitEvent,
        done: &mut dyn FnMut() -> bool,
    ) -> Result<(), Interrupted> {
        self.v.wait_until(self.wake, done).map_err(|e| {
            self.err = Some(e);
            Interrupted
        })
    }
}

/// V's in-scope tree as P3 reads it (C8, D101 (1)).
struct VTree<'a>(&'a dyn chain::TreeRead);

impl TreeRead for VTree<'_> {
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
        self.0.lookup(hash)
    }

    fn entry(&self, id: BlockId) -> Option<EntryFacts> {
        self.0.entry(id).as_ref().map(facts)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.0.ancestor(id, height)
    }
}

impl ValidationInterface for Listener {
    /// On V's thread. Core's `BlockChecked` acts on an invalid state or a valid one; a
    /// `state.Error` (an `AbortNode` already raised) takes neither branch and only erases the
    /// block's source. P3's signal has no error state, and `ibd = true` with no invalid state
    /// reaches exactly that: the valid branch needs `!IsInitialBlockDownload()` (D112).
    // Core: net_processing.cpp:2314-2343 PeerManagerImpl::BlockChecked
    fn block_checked(&self, block: &Block, state: &BlockState, ibd: bool) {
        let hash = block.hash();
        match state {
            BlockState::Valid => self.shared.block_checked(hash, None, ibd),
            BlockState::Invalid(e) => self.shared.block_checked(hash, Some(e.clone()), ibd),
            BlockState::Error(_) => self.shared.block_checked(hash, None, true),
        }
    }

    /// On V's thread, inside the accept and before the block is written: the encoding, `recent`
    /// and the visit of every fully connected peer, against V's in-scope tree (C8, D50).
    // Core: net_processing.cpp:2228-2268 PeerManagerImpl::NewPoWValidBlock
    fn new_pow_valid_block(&self, nb: &NewBlock<'_>) {
        self.shared.new_pow_valid_block(&facts(&nb.entry), nb.block, &VTree(nb.tree), nb.stamp);
    }

    /// C5: Core's `LimitValidationInterfaceQueue` at V's release points (v:3760, v:3940) and its
    /// idle point, on P3's queue: V serves meanwhile until the queue is within its bounds.
    fn release_point(&self, wait: &mut dyn chain::ServiceWait) -> Result<(), WaitError> {
        self.wait_on_queue(wait, SignalQueue::wait_for_space)
    }

    /// C5′ (DEC-240): before an accept outside IBD, V serves until no `BlockChecked` is
    /// outstanding, so the actor has applied every earlier one before the accept's
    /// `NewPoWValidBlock` (D55).
    fn before_accept(&self, wait: &mut dyn chain::ServiceWait) -> Result<(), WaitError> {
        self.wait_on_queue(wait, SignalQueue::wait_quiescent)
    }

    /// Queued. Core's `vHashes`: from the new tip back to, not including, the fork point, at most
    /// `MAX_BLOCKS_TO_ANNOUNCE`, gathered only outside IBD. Entries are immutable, so V's
    /// published index answers the walk.
    // Core: net_processing.cpp:2273-2308 PeerManagerImpl::UpdatedBlockTip
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        let hashes = announcements(&self.view, new, fork, ibd);
        self.shared.updated_block_tip(&self.core, &hashes, ibd);
    }

    /// Queued: `m_last_tip_update = GetTime()` and the stalling timeout's decay. The block also
    /// goes into the adapter's connected-block cache first, so `UpdatedBlockTip`, queued after it,
    /// announces only blocks `read_block` holds (D113).
    // Core: net_processing.cpp:2185, 2199-2207
    fn block_connected(&self, block: &Arc<Block>, _id: BlockId) {
        self.chain.connected(block);
        self.shared.block_connected(self.clock.node_now().0);
    }
}

/// Core's `vHashes`: from `new` back to, not including, `fork`, at most `MAX_BLOCKS_TO_ANNOUNCE`,
/// newest first; none in IBD.
// Core: net_processing.cpp:2282-2295
pub(super) fn announcements(
    view: &ChainViewHandle,
    new: BlockId,
    fork: Option<BlockId>,
    ibd: bool,
) -> Vec<BlockHash> {
    let mut hashes = Vec::new();
    if ibd {
        return hashes;
    }
    let mut at = Some(new);
    while let Some(id) = at
        && Some(id) != fork
        && let Some(e) = view.entry(id)
    {
        hashes.push(e.hash);
        if hashes.len() == MAX_BLOCKS_TO_ANNOUNCE {
            break;
        }
        at = e.prev;
    }
    hashes
}
