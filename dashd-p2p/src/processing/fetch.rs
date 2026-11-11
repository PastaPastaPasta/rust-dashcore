//! Core's `PeerManager::FetchBlock` (np:2057-2089), the processor half of `getblockfrompeer`
//! (BL-503): one actor step that drops every earlier request for the block, records it in
//! flight from the peer and asks the peer for it, and the queue that carries an RPC thread's
//! request to the actor.
//!
//! Core's RPC thread runs `FetchBlock` itself once it holds `cs_main`, which it gets whenever
//! the message handler is between two `cs_main` sections. Here only the actor reaches the
//! in-flight state, so the RPC thread queues the request, wakes the actor and waits for its next
//! [`MessageLoop::drain_fetches`](super::peerman::MessageLoop::drain_fetches).

use std::collections::VecDeque;
use std::fmt;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use dash_p2p::PeerId;
use dash_primitives::BlockHash;
use parking_lot::Mutex;

use super::blocks::getdata_block;
use super::download::PeerFacts;
use super::eviction::fully_connected;
use super::logic::PeerLogic;
#[cfg(feature = "sim")]
use super::peerman::LockPoint;

/// Why `PeerManager::FetchBlock` refused (np:2057-2089). `Display` is Core's string for the
/// four Core variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchBlockError {
    /// np:2059: "Loading blocks ..."
    LoadingBlocks,
    /// np:2063: "Peer does not exist"
    PeerDoesNotExist,
    /// np:2071: "Already requested from this peer". Unreachable after np:2068's remove-all,
    /// which runs under the same lock; ported so that dropping the remove-all shows up as this
    /// error.
    AlreadyRequested,
    /// np:2084: "Peer not fully connected"
    PeerNotFullyConnected,
    /// Not Core's. The actor's index has no entry for the hash. The RPC's lookup (rb:512) found
    /// one first, and entries are never erased, so a consistent host never returns this. Its
    /// text is rb:515's "Block header missing", so even a host bug cannot show a string Core
    /// lacks.
    HeaderMissing,
    /// Not Core's. The request was made after the processor's fetch queue closed (its stop),
    /// or the actor dropped it unrun (a panic, or a loop that never runs). It has no string:
    /// the host stops its RPC workers before the processor (P2a-net D51), so a host answers
    /// this as for a null peer manager.
    Stopped,
}

impl FetchBlockError {
    /// Core's text; `None` for [`FetchBlockError::Stopped`].
    // Core: net_processing.cpp:2059, 2063, 2071, 2084; rpc/blockchain.cpp:515
    pub fn core_message(self) -> Option<&'static str> {
        Some(match self {
            Self::LoadingBlocks => "Loading blocks ...",
            Self::PeerDoesNotExist => "Peer does not exist",
            Self::AlreadyRequested => "Already requested from this peer",
            Self::PeerNotFullyConnected => "Peer not fully connected",
            Self::HeaderMissing => "Block header missing",
            Self::Stopped => return None,
        })
    }
}

impl fmt::Display for FetchBlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.core_message().unwrap_or("the message processor stopped"))
    }
}

impl std::error::Error for FetchBlockError {}

impl PeerLogic {
    /// Core's `FetchBlock(peer, index)` for the block `hash` as one actor step: the lookup of
    /// `hash` in the actor's index (Core's caller passes the entry), then np:2059-2088 with no
    /// other actor work in between, and `peer`'s state lock held from before the
    /// remove-all until `BlockRequested` has run, so a `getpeerinfo` on another thread sees the
    /// peer's blocks in flight before or after the step, as Core's under `cs_main` does. The
    /// lock is released before the push. Actor thread only, between per-peer steps (called
    /// with no state lock held).
    ///
    /// The entry recorded before the push is kept when the peer is not fully connected, as
    /// Core keeps it.
    // Core: net_processing.cpp:2057-2089 PeerManagerImpl::FetchBlock
    pub fn fetch_block(&mut self, hash: &BlockHash, peer: PeerId) -> Result<(), FetchBlockError> {
        let entry = self.chain.entry_by_hash(hash).ok_or(FetchBlockError::HeaderMissing)?;
        if self.chain.loading_blocks() {
            return Err(FetchBlockError::LoadingBlocks);
        }
        let state = self.state_of(peer).ok_or(FetchBlockError::PeerDoesNotExist)?;
        let st = state.lock();
        let sync = &mut self.seams.block_sync;
        sync.remove_block_request(hash, None, self.core.clock().node_now_micros());
        #[cfg(feature = "sim")]
        self.signals.on_lock(LockPoint::FetchBetween(peer));
        let facts = PeerFacts {
            id: peer,
            inbound: st.inbound,
            heights: &st.heights_in_flight,
        };
        let requested = sync.block_requested(facts, &entry, self.core.clock().node_now_micros());
        drop(st);
        if !requested {
            return Err(FetchBlockError::AlreadyRequested);
        }
        let node = self.core.peer(peer).filter(fully_connected);
        let node = node.ok_or(FetchBlockError::PeerNotFullyConnected)?;
        getdata_block(&self.core, &node, hash);
        tracing::debug!(target: "net", "Requesting block {hash} from peer={peer}");
        Ok(())
    }
}

/// A queued request's reply.
#[derive(Debug)]
#[must_use = "the request still runs; drop the ticket only when its answer is not wanted"]
pub struct FetchTicket {
    reply: Receiver<Result<(), FetchBlockError>>,
    got: Option<Result<(), FetchBlockError>>,
}

impl FetchTicket {
    /// Blocks until the actor has run the request, at a drain point or at its exit drain
    /// (P2a-net D51). `Err(Stopped)` at once when the request was made after the queue closed, or
    /// when the actor drops it unrun (a panic, or a loop that never runs).
    pub fn wait(self) -> Result<(), FetchBlockError> {
        match self.got {
            Some(r) => r,
            None => self.reply.recv().unwrap_or(Err(FetchBlockError::Stopped)),
        }
    }

    /// The reply if it is in (for `sim`, which has no actor thread).
    pub fn try_result(&mut self) -> Option<Result<(), FetchBlockError>> {
        if self.got.is_none() {
            self.got = match self.reply.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(FetchBlockError::Stopped)),
            };
        }
        self.got
    }
}

/// One queued `FetchBlock(peer, hash)` and where its reply goes.
pub(super) struct FetchRequest {
    pub(super) hash: BlockHash,
    pub(super) peer: PeerId,
    pub(super) reply: SyncSender<Result<(), FetchBlockError>>,
}

/// The requests the actor has not taken yet, a leaf lock. It has no bound: a
/// [`Shared::fetch_block`](super::peerman::Shared::fetch_block) caller waits for its own reply,
/// so those requests never outnumber the RPC threads; a
/// [`Shared::request_fetch`](super::peerman::Shared::request_fetch) caller bounds its own.
/// [`Shared::close_fetches`](super::peerman::Shared::close_fetches) closes it to new requests,
/// whose tickets read `Stopped` at once; what it already holds waits for the actor's exit drain
/// (P2a-net D51), or is abandoned when no actor will drain it.
#[derive(Default)]
pub(super) struct FetchQueue(Mutex<Pending>);

#[derive(Default)]
struct Pending {
    queue: VecDeque<FetchRequest>,
    closed: bool,
}

impl FetchQueue {
    /// Queues `FetchBlock(peer, hash)`; the ticket reads `Stopped` at once on a closed queue.
    pub(super) fn push(&self, hash: BlockHash, peer: PeerId) -> FetchTicket {
        let (reply, rx) = mpsc::sync_channel(1);
        let mut p = self.0.lock();
        if !p.closed {
            p.queue.push_back(FetchRequest {
                hash,
                peer,
                reply,
            });
        }
        FetchTicket {
            reply: rx,
            got: None,
        }
    }

    /// Every queued request, oldest first.
    pub(super) fn take(&self) -> VecDeque<FetchRequest> {
        std::mem::take(&mut self.0.lock().queue)
    }

    /// Closes the queue to new requests, which answer `Stopped` at once. The queued ones stay
    /// for the actor's exit drain (P2a-net D51).
    pub(super) fn close(&self) {
        self.0.lock().closed = true;
    }

    /// Whether [`FetchQueue::close`] or [`FetchQueue::abandon`] ran.
    pub(super) fn is_closed(&self) -> bool {
        self.0.lock().closed
    }

    /// Closes the queue and drops what it holds, whose tickets read `Stopped`: no actor will
    /// drain it (a panic, or a loop that stops without its exit drain).
    pub(super) fn abandon(&self) {
        let dropped = {
            let mut p = self.0.lock();
            p.closed = true;
            std::mem::take(&mut p.queue)
        };
        drop(dropped);
    }
}
