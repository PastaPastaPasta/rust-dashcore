//! Core's block-download state (P3 §2.2), actor-private: `mapBlocksInFlight`, each peer's
//! `vBlocksInFlight` with its download and stall times, `mapBlockSource` and
//! `m_peers_downloading_from`. Core guards them with `cs_main`; the actor alone touches them here.
//!
//! S1 is the skeleton the two-phase `block` handler and `BlockChecked`'s punish branch need:
//! requests, removals, the source map and `finalize`. S2 adds the scheduler, stalling and
//! timeouts. S3 adds the compact-block client's state: each request's `partialBlock`, the
//! extra-transactions ring, the mempool source and direct fetch's `MSG_CMPCT_BLOCK` swap.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use dash_consensus::{Block, BlockId, BlockValidationError, TxRef};
use dash_p2p::{CommandString, PeerId, ServiceFlags, Why};
use dash_p2p_core::{GetData, InvType, Inventory, SendCmpct};
use dash_primitives::BlockHash;
use parking_lot::Mutex;

use super::compact::{
    self, CompactBlock, CompactTxSource, ExtraTxns, NoMempool, PartiallyDownloadedBlock, ReadStatus,
};
use super::eviction::can_direct_fetch;
use super::logic::{CMPCTBLOCKS_VERSION, can_serve_blocks, out_msg};
use super::peer::PeerState;
use super::peerman::RecentBlock;
use super::seams::{
    BlockCheckedCx, BlockSync, ChainAccess, ChainSnapshot, EntryFacts, GetDataBatch, SyncCx,
    Validity,
};
use super::sync::{inv_hash, process_block_availability};

/// Core's `MAX_BLOCKS_IN_TRANSIT_PER_PEER`.
pub const MAX_BLOCKS_IN_TRANSIT_PER_PEER: usize = 16;
/// Core's `MAX_CMPCTBLOCKS_INFLIGHT_PER_BLOCK`.
pub const MAX_CMPCTBLOCKS_INFLIGHT_PER_BLOCK: usize = 3;
/// Core's `BLOCK_DOWNLOAD_WINDOW`.
const BLOCK_DOWNLOAD_WINDOW: u32 = 1024;
/// Core's `BLOCK_STALLING_TIMEOUT_DEFAULT`, in seconds.
const BLOCK_STALLING_TIMEOUT_DEFAULT: i64 = 2;
/// Core's `BLOCK_STALLING_TIMEOUT_MAX`, in seconds.
const BLOCK_STALLING_TIMEOUT_MAX: i64 = 64;
/// Core's `BLOCK_DOWNLOAD_TIMEOUT_BASE`, in block intervals.
const BLOCK_DOWNLOAD_TIMEOUT_BASE: f64 = 1.0;
/// Core's `BLOCK_DOWNLOAD_TIMEOUT_PER_PEER`, in block intervals.
const BLOCK_DOWNLOAD_TIMEOUT_PER_PEER: f64 = 0.5;

/// Core's `IsLimitedPeer`: `NODE_NETWORK_LIMITED` without `NODE_NETWORK`.
// Core: net_processing.cpp:1203-1207
fn is_limited_peer(services: ServiceFlags) -> bool {
    !services.has(ServiceFlags::NODE_NETWORK) && services.has(ServiceFlags::NODE_NETWORK_LIMITED)
}

/// `m_block_stalling_timeout` (np:982), in seconds: section 9 doubles it and
/// [`Shared::block_connected`](super::peerman::Shared::block_connected) decays it, each with
/// Core's `compare_exchange_strong`, so a racing pair wins or loses as in Core (P3 §3.3).
#[derive(Clone, Debug)]
pub struct StallingTimeout(Arc<AtomicI64>);

impl Default for StallingTimeout {
    fn default() -> Self {
        Self(Arc::new(AtomicI64::new(BLOCK_STALLING_TIMEOUT_DEFAULT)))
    }
}

impl StallingTimeout {
    /// The current timeout.
    pub(super) fn get(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    /// Section 9's doubling from `seen`, the value it compared against: the new value when this
    /// call's exchange stored it.
    // Core: net_processing.cpp:6762-6765
    fn raise(&self, seen: i64) -> Option<i64> {
        let new = (2 * seen).min(BLOCK_STALLING_TIMEOUT_MAX);
        if seen == new
            || self.0.compare_exchange(seen, new, Ordering::SeqCst, Ordering::SeqCst).is_err()
        {
            return None;
        }
        tracing::debug!(target: "net", "Increased stalling timeout temporarily to {new} seconds");
        Some(new)
    }

    /// `BlockConnected`'s decay toward the default: the new value when this call's exchange
    /// stored it. `duration_cast<seconds>(t * 0.85)` truncates toward zero.
    // Core: net_processing.cpp:2199-2207
    pub(super) fn decay(&self) -> Option<i64> {
        let seen = self.get();
        debug_assert!(
            seen >= BLOCK_STALLING_TIMEOUT_DEFAULT,
            "Assume(stalling_timeout >= default)"
        );
        if seen == BLOCK_STALLING_TIMEOUT_DEFAULT {
            return None;
        }
        let new = ((seen as f64 * 0.85) as i64).max(BLOCK_STALLING_TIMEOUT_DEFAULT);
        self.0.compare_exchange(seen, new, Ordering::SeqCst, Ordering::SeqCst).ok()?;
        tracing::debug!(target: "net", "Decreased stalling timeout to {new} seconds");
        Some(new)
    }

    #[cfg(test)]
    fn set(&self, t: i64) {
        self.0.store(t, Ordering::SeqCst);
    }
}

/// Section 9's verdict on a peer: why it is disconnected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section9 {
    /// It stalled the download window for longer than the timeout, which read `seen`.
    Stalling {
        /// The timeout the check compared against.
        seen: i64,
    },
    /// Its oldest request, this block, outlived `spacing × (1 + 0.5 × others)`.
    Timeout(BlockHash),
}

/// A peer's `vHeightInFlight` mirror for `getpeerinfo` (P3 §2.7): the heights of its blocks in
/// flight in `vBlocksInFlight` order, a leaf lock. The peer's state creates it; its
/// `PeerDownload` writes it.
pub type HeightsCell = Arc<Mutex<Vec<i32>>>;

/// What [`BlockDownload::block_requested`] records about the requesting peer.
#[derive(Clone, Copy, Debug)]
pub struct PeerFacts<'a> {
    /// The peer.
    pub id: PeerId,
    /// `m_is_inbound`.
    pub inbound: bool,
    /// Its `vHeightInFlight` mirror.
    pub heights: &'a HeightsCell,
}

impl<'a> PeerFacts<'a> {
    /// `cx`'s peer.
    pub fn of(cx: &'a SyncCx<'_>) -> Self {
        Self {
            id: cx.peer.id(),
            inbound: cx.peer.is_inbound(),
            heights: &cx.state.heights_in_flight,
        }
    }
}

/// What HB selection reads and does outside its list: Core's `State(id)` and two-argument
/// `ForNode` (P3 §3.5.2).
pub trait HbPeers {
    /// `(m_provides_cmpctblocks, m_is_inbound)` while `id` has a processor state, which lasts
    /// until `FinalizeNode`.
    fn node_state(&self, id: PeerId) -> Option<(bool, bool)>;
    /// When `id` is fully connected: push `sendcmpct(high_bandwidth, 1)`, set its
    /// `m_bip152_highbandwidth_to`, and return true; else nothing and false.
    fn set_high_bandwidth_to(&self, id: PeerId, high_bandwidth: bool) -> bool;
}

impl HbPeers for BlockCheckedCx<'_> {
    fn node_state(&self, id: PeerId) -> Option<(bool, bool)> {
        BlockCheckedCx::node_state(self, id)
    }

    fn set_high_bandwidth_to(&self, id: PeerId, high_bandwidth: bool) -> bool {
        self.for_node(id, |core, p| {
            core.push(p, out_msg(CommandString::SENDCMPCT, &send_cmpct(high_bandwidth)));
            p.set_bip152_highbandwidth_to(high_bandwidth);
        })
    }
}

/// Core's `MaybeSetPeerAsAnnouncingHeaderAndIDs(source)` after its `-blocksonly` return: make
/// `source` one of the three peers in `hb_list` (`lNodesAnnouncingHeaderAndIDs`) asked for
/// high-bandwidth announcements, keeping an outbound one when an inbound one joins, and tell
/// the peer it replaces (P3 §3.5.2). Finalized ids stay in the list: they count toward the
/// three, and popping one tells nobody (Q4).
// Core: net_processing.cpp:1360-1416
pub fn maybe_set_peer_as_announcing_header_and_ids(
    hb_list: &mut VecDeque<PeerId>,
    peers: &dyn HbPeers,
    source: PeerId,
) {
    let Some((true, source_inbound)) = peers.node_state(source) else {
        return;
    };
    let outbound = |id| peers.node_state(id).is_some_and(|(_, inbound)| !inbound);
    let mut outbound_hb = 0;
    for (i, &id) in hb_list.iter().enumerate() {
        if id == source {
            hb_list.remove(i);
            hb_list.push_back(source);
            return;
        }
        // A finalized peer has no state: not counted.
        if outbound(id) {
            outbound_hb += 1;
        }
    }
    if source_inbound && hb_list.len() >= 3 && outbound_hb == 1 && outbound(hb_list[0]) {
        // Keep the last outbound HB peer out of the slot the pop below empties.
        hb_list.swap(0, 1);
    }
    if peers.set_high_bandwidth_to(source, true) {
        hb_list.push_back(source);
    }
    if hb_list.len() > 3 {
        let front = hb_list.pop_front().expect("more than three");
        peers.set_high_bandwidth_to(front, false);
    }
}

/// `sendcmpct(high_bandwidth, CMPCTBLOCKS_VERSION)`.
fn send_cmpct(high_bandwidth: bool) -> SendCmpct {
    SendCmpct {
        high_bandwidth,
        version: CMPCTBLOCKS_VERSION,
    }
}

/// One `mapBlocksInFlight` entry: the requesting peer and its list entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InFlight {
    peer: PeerId,
    seq: u64,
}

/// Core's `QueuedBlock` (np:227-232) as callers see it: everything but `partialBlock`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueuedBlock {
    /// Identifies the entry in its peer's list (Core erases by iterator).
    seq: u64,
    /// `pindex`.
    pub id: BlockId,
    /// Its hash.
    pub hash: BlockHash,
    /// Its height.
    pub height: i32,
}

/// One `vBlocksInFlight` entry: the block and, for a compact request, its `partialBlock`.
#[derive(Debug)]
struct Queued {
    block: QueuedBlock,
    partial: Option<Box<PartiallyDownloadedBlock>>,
}

/// The `CNodeState` block-download fields, created on a peer's first request and removed in
/// `finalize`. An absent entry reads as an empty one.
#[derive(Debug)]
struct PeerDownload {
    /// `m_is_inbound`.
    inbound: bool,
    /// `vBlocksInFlight`, front = oldest.
    blocks: VecDeque<Queued>,
    /// `m_downloading_since`.
    downloading_since_us: i64,
    /// `m_stalling_since`; 0 = not stalling.
    stalling_since_us: i64,
    /// The stats mirror.
    heights: HeightsCell,
}

impl PeerDownload {
    fn mirror(&self) {
        *self.heights.lock() = self.blocks.iter().map(|q| q.block.height).collect();
    }

    /// The entry for `hash`: a peer has at most one, as `BlockRequested` keeps it.
    fn entry_mut(&mut self, hash: &BlockHash) -> Option<&mut Queued> {
        self.blocks.iter_mut().find(|q| q.block.hash == *hash)
    }
}

/// The mempool `InitData` reads (MP1a's; [`NoMempool`] until then).
struct TxSource(Arc<dyn CompactTxSource>);

impl Default for TxSource {
    fn default() -> Self {
        Self(Arc::new(NoMempool))
    }
}

impl std::fmt::Debug for TxSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TxSource")
    }
}

/// `mapBlocksInFlight.equal_range(hash)` read for one peer, as `cmpctblock` and
/// `ProcessCompactBlockTxns` compute it before they change anything (np:5124-5138,
/// np:3742-3756).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InFlightFacts {
    /// `already_in_flight`: the entries for the hash.
    pub already: usize,
    /// `first_in_flight`: none, or the peer's is the first.
    pub first: bool,
    /// `requested_block_from_this_peer`.
    pub from_this_peer: bool,
}

/// The tracked branch's `BlockRequested` and `InitData` (np:5160-5189), and the indexes
/// `getblocktxn` would ask for (np:5191-5195).
#[derive(Debug, PartialEq, Eq)]
pub enum TrackedInit {
    /// The peer already has a `partialBlock` for this block: "already syncing".
    AlreadySyncing,
    /// `InitData` returned `READ_STATUS_INVALID`.
    Invalid,
    /// `InitData` returned `READ_STATUS_FAILED`.
    Failed,
    /// `InitData` succeeded; the indexes still missing, ascending.
    Missing(Vec<u16>),
}

/// `ProcessCompactBlockTxns`' read of this peer's `partialBlock` (np:3758-3778).
pub enum CompactFill {
    /// `requested_block_from_this_peer` is false: no entry from this peer holds a partial.
    NotExpected,
    /// The partial's header is null: an earlier `FillBlock` failed.
    HeaderGone,
    /// `FillBlock` returned `READ_STATUS_INVALID`.
    Invalid,
    /// `FillBlock` returned `READ_STATUS_FAILED`.
    Failed,
    /// `FillBlock` returned `READ_STATUS_OK` and the block.
    Block(Block),
}

/// Core's block-download state (np:227-232, 487-490, 1075-1076, 1112).
#[derive(Debug, Default)]
pub struct BlockDownload {
    /// `mapBlocksInFlight`: per hash, the requesting peers in insertion order (a multimap keeps
    /// equal keys in insertion order).
    in_flight: HashMap<BlockHash, Vec<InFlight>>,
    /// `mapBlocksInFlight.size()`.
    in_flight_entries: usize,
    peers: HashMap<PeerId, PeerDownload>,
    /// `mapBlockSource`: hash → (peer, `true` when the full block came by `block`).
    block_source: HashMap<BlockHash, (PeerId, bool)>,
    /// `m_peers_downloading_from`.
    peers_downloading_from: i32,
    /// `m_block_stalling_timeout`, shared with the signals thread.
    stalling_timeout: StallingTimeout,
    next_seq: u64,
    /// `mapBlockSource.size()` for DEC-230's guard on the signal queue.
    source_len: Arc<AtomicUsize>,
    /// `vExtraTxnForCompact` with its insert position (np:1129-1131).
    extra_txn: ExtraTxns,
    /// The mempool for reconstruction.
    tx_source: TxSource,
    /// `m_ignore_incoming_txs` (`-blocksonly`), bound by the message loop.
    ignore_incoming_txs: bool,
    /// `lNodesAnnouncingHeaderAndIDs`: the peers asked for high-bandwidth `cmpctblock`
    /// announcements, oldest first. Finalized ids stay until they are popped, as in Core (Q4).
    hb_list: VecDeque<PeerId>,
}

impl BlockDownload {
    /// An empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// `IsBlockRequested(hash)`.
    // Core: net_processing.cpp:1270-1273
    pub fn is_block_requested(&self, hash: &BlockHash) -> bool {
        self.in_flight.contains_key(hash)
    }

    /// `IsBlockRequestedFromOutbound(hash)`: some peer it is in flight from is outbound.
    // Core: net_processing.cpp:1275-1285
    pub fn is_block_requested_from_outbound(&self, hash: &BlockHash) -> bool {
        self.in_flight.get(hash).into_iter().flatten().any(|f| {
            !self.peers.get(&f.peer).expect("an in-flight entry's peer has a state").inbound
        })
    }

    /// `m_peers_downloading_from`.
    pub fn peers_downloading_from(&self) -> i32 {
        self.peers_downloading_from
    }

    /// An empty state whose extra-transactions ring holds `max_extra_txn` entries
    /// (`-blockreconstructionextratxn`).
    pub fn with_max_extra_txn(max_extra_txn: i64) -> Self {
        Self {
            extra_txn: ExtraTxns::new(max_extra_txn),
            ..Self::default()
        }
    }

    /// `AddToCompactExtraTransactions(tx)`, the entry point for P5's orphans and small rejected
    /// transactions and MP1a's (P3 D19). Nothing calls it before them, so the ring stays empty.
    // Core: net_processing.cpp:1922-1931
    pub fn add_to_compact_extra_transactions(&mut self, tx: TxRef) {
        self.extra_txn.add(tx);
    }

    /// The extra-transactions ring `InitData` searches.
    pub fn extra_txn(&self) -> &ExtraTxns {
        &self.extra_txn
    }

    /// Sets the mempool `InitData` reads (MP1a's; the sim's stand-in). [`NoMempool`] by default.
    pub fn set_tx_source(&mut self, source: Arc<dyn CompactTxSource>) {
        self.tx_source = TxSource(source);
    }

    /// `already_in_flight`, `first_in_flight` and `requested_block_from_this_peer` for `hash`
    /// and `peer`, as `cmpctblock` reads them. (`ProcessCompactBlockTxns`' stricter
    /// `requested_block_from_this_peer`, which needs a `partialBlock`, is
    /// [`CompactFill::NotExpected`].)
    // Core: net_processing.cpp:5124-5138, 3742-3747
    pub fn in_flight_facts(&self, hash: &BlockHash, peer: PeerId) -> InFlightFacts {
        let entries = self.in_flight.get(hash).map_or(&[][..], Vec::as_slice);
        InFlightFacts {
            already: entries.len(),
            first: entries.first().is_none_or(|f| f.peer == peer),
            from_this_peer: entries.iter().any(|f| f.peer == peer),
        }
    }

    /// The tracked branch of `cmpctblock`: `BlockRequested(peer, block, &it)`, the
    /// `partialBlock` it needs, and `InitData` on it, at `now_us` (BlockRequested's `GetTime`).
    // Core: net_processing.cpp:5160-5195
    pub fn init_tracked(
        &mut self,
        peer: PeerFacts<'_>,
        block: &EntryFacts,
        cmpct: &CompactBlock,
        now_us: i64,
    ) -> TrackedInit {
        let id = peer.id;
        // Core's new entry gets its partial inside `BlockRequested` and an old one without gets
        // it after; either way the entry holds a fresh one unless it already had one.
        self.block_requested(peer, block, now_us);
        let Self {
            peers,
            tx_source,
            extra_txn,
            ..
        } = self;
        let q = peers.get_mut(&id).and_then(|p| p.entry_mut(&block.hash)).expect("requested");
        if q.partial.is_some() {
            return TrackedInit::AlreadySyncing;
        }
        let partial = q.partial.insert(Box::default());
        match partial.init_data(cmpct, &*tx_source.0, extra_txn) {
            ReadStatus::Invalid => TrackedInit::Invalid,
            ReadStatus::Failed => TrackedInit::Failed,
            ReadStatus::Ok => TrackedInit::Missing(
                (0..partial.tx_count())
                    .filter(|&i| !partial.is_tx_available(i))
                    // `BlockTxCount()` is at most 65,535 (the decoder's bound).
                    .map(|i| i as u16)
                    .collect(),
            ),
        }
    }

    /// The untracked branch: a temporary `PartiallyDownloadedBlock`, `InitData` and a
    /// `FillBlock` with no transactions; the block when both succeed.
    // Core: net_processing.cpp:5219-5236
    pub fn reconstruct_untracked(&self, cmpct: &CompactBlock) -> Option<Block> {
        let mut temp = PartiallyDownloadedBlock::new();
        if temp.init_data(cmpct, &*self.tx_source.0, &self.extra_txn) != ReadStatus::Ok {
            return None;
        }
        temp.fill_block(&[]).1
    }

    /// `ProcessCompactBlockTxns`' `FillBlock` on `peer`'s `partialBlock` for `hash`.
    // Core: net_processing.cpp:3758-3778
    pub fn fill_partial(&mut self, peer: PeerId, hash: &BlockHash, txn: &[TxRef]) -> CompactFill {
        let partial = self
            .peers
            .get_mut(&peer)
            .and_then(|p| p.entry_mut(hash))
            .and_then(|q| q.partial.as_mut());
        let Some(partial) = partial else {
            return CompactFill::NotExpected;
        };
        if partial.header().is_none() {
            return CompactFill::HeaderGone;
        }
        match partial.fill_block(txn) {
            (ReadStatus::Ok, block) => {
                CompactFill::Block(block.expect("FillBlock's Ok carries it"))
            }
            (ReadStatus::Invalid, _) => CompactFill::Invalid,
            (ReadStatus::Failed, _) => CompactFill::Failed,
        }
    }

    /// `mapBlocksInFlight.size()`.
    pub fn in_flight_entries(&self) -> usize {
        self.in_flight_entries
    }

    /// `peer`'s `vBlocksInFlight`, oldest first.
    pub fn queued(&self, peer: PeerId) -> Vec<QueuedBlock> {
        self.peers.get(&peer).map_or_else(Vec::new, |p| p.blocks.iter().map(|q| q.block).collect())
    }

    /// `peer`'s `(m_downloading_since, m_stalling_since)`, if it ever had a request.
    pub fn times(&self, peer: PeerId) -> Option<(i64, i64)> {
        self.peers.get(&peer).map(|p| (p.downloading_since_us, p.stalling_since_us))
    }

    /// `mapBlockSource[hash]`.
    pub fn block_source(&self, hash: &BlockHash) -> Option<(PeerId, bool)> {
        self.block_source.get(hash).copied()
    }

    /// `RemoveBlockRequest(hash, from_peer)` at `now_us`, its own `GetTime<µs>()`: the caller
    /// reads the node clock at the call, not at a pass's or handler's start (D86).
    // Core: net_processing.cpp:1287-1322
    pub fn remove_block_request(&mut self, hash: &BlockHash, from: Option<PeerId>, now_us: i64) {
        let Some(entries) = self.in_flight.get_mut(hash) else {
            return;
        };
        let mut i = 0;
        while i < entries.len() {
            let InFlight {
                peer,
                seq,
            } = entries[i];
            if from.is_some_and(|f| f != peer) {
                i += 1;
                continue;
            }
            let p = self.peers.get_mut(&peer).expect("an in-flight entry's peer has a state");
            if p.blocks.front().is_some_and(|q| q.block.seq == seq) {
                p.downloading_since_us = p.downloading_since_us.max(now_us);
            }
            p.blocks.retain(|q| q.block.seq != seq);
            p.mirror();
            if p.blocks.is_empty() {
                self.peers_downloading_from -= 1;
            }
            p.stalling_since_us = 0;
            entries.remove(i);
            self.in_flight_entries -= 1;
        }
        if entries.is_empty() {
            self.in_flight.remove(hash);
        }
    }

    /// `BlockRequested(nodeid, block, nullptr)` at `now_us`, its own `GetTime<µs>()` read at the
    /// call (D86): `false` when `peer` already has it in flight. `inbound` and `heights` are the
    /// peer's, recorded on its first request. A new entry holds no `partialBlock`;
    /// [`BlockDownload::init_tracked`] gives it one.
    // Core: net_processing.cpp:1324-1358
    pub fn block_requested(
        &mut self,
        peer: PeerFacts<'_>,
        block: &EntryFacts,
        now_us: i64,
    ) -> bool {
        let PeerFacts {
            id: peer,
            inbound,
            heights,
        } = peer;
        let hash = block.hash;
        if self.in_flight.get(&hash).is_some_and(|v| v.iter().any(|f| f.peer == peer)) {
            return false;
        }
        self.remove_block_request(&hash, Some(peer), now_us);
        let seq = self.next_seq;
        self.next_seq += 1;
        let p = self.peers.entry(peer).or_insert_with(|| PeerDownload {
            inbound,
            blocks: VecDeque::new(),
            downloading_since_us: 0,
            stalling_since_us: 0,
            heights: Arc::clone(heights),
        });
        p.blocks.push_back(Queued {
            block: QueuedBlock {
                seq,
                id: block.id,
                hash,
                height: block.height as i32,
            },
            partial: None,
        });
        p.mirror();
        if p.blocks.len() == 1 {
            p.downloading_since_us = now_us;
            self.peers_downloading_from += 1;
        }
        self.in_flight.entry(hash).or_default().push(InFlight {
            peer,
            seq,
        });
        self.in_flight_entries += 1;
        true
    }

    /// `mapBlockSource.emplace(hash, {peer, full})`: never overwrites (Q5).
    pub fn emplace_block_source(&mut self, hash: BlockHash, peer: PeerId, full: bool) {
        if let std::collections::hash_map::Entry::Vacant(v) = self.block_source.entry(hash) {
            v.insert((peer, full));
            self.source_len.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// `mapBlockSource.erase(hash)`.
    pub fn erase_block_source(&mut self, hash: &BlockHash) {
        if self.block_source.remove(hash).is_some() {
            self.source_len.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// `lNodesAnnouncingHeaderAndIDs`, oldest first.
    pub fn hb_list(&self) -> &VecDeque<PeerId> {
        &self.hb_list
    }

    /// Whether `peer` is inbound, as recorded at its first request (`m_is_inbound`).
    pub fn is_inbound(&self, peer: PeerId) -> Option<bool> {
        self.peers.get(&peer).map(|p| p.inbound)
    }

    /// Section 9's verdict for `peer` at `now_us` with block interval `spacing` seconds: the stall
    /// check, then the block download timeout. It decides only: the caller raises the timeout
    /// after its log line, as Core does.
    // Core: net_processing.cpp:6752-6781
    fn section9(&self, peer: PeerId, now_us: i64, spacing: i64) -> Option<Section9> {
        let p = self.peers.get(&peer)?;
        let seen = self.stalling_timeout.get();
        if p.stalling_since_us != 0 && p.stalling_since_us < now_us - seen * 1_000_000 {
            return Some(Section9::Stalling {
                seen,
            });
        }
        let front = &p.blocks.front()?.block;
        let others = self.peers_downloading_from - 1;
        // `seconds × double` is a `duration<double>`; the sum and the compare are in double µs.
        let factor =
            BLOCK_DOWNLOAD_TIMEOUT_BASE + BLOCK_DOWNLOAD_TIMEOUT_PER_PEER * f64::from(others);
        let deadline = p.downloading_since_us as f64 + spacing as f64 * factor * 1e6;
        ((now_us as f64) > deadline).then_some(Section9::Timeout(front.hash))
    }

    /// Section 12's tail: once `peer`'s requests leave it nothing in flight, the staller
    /// `FindNextBlocksToDownload` named starts its stall clock, if it is not running.
    // Core: net_processing.cpp:6832-6837
    pub(crate) fn start_stall(&mut self, peer: PeerId, staller: Option<PeerId>, now_us: i64) {
        let Some(staller) = staller else {
            return;
        };
        if self.peers.get(&peer).is_some_and(|p| !p.blocks.is_empty()) {
            return;
        }
        let s = self.peers.get_mut(&staller).expect("the staller has a block in flight");
        if s.stalling_since_us == 0 {
            s.stalling_since_us = now_us;
            tracing::debug!(target: "net", "Stall started peer={staller}");
        }
    }

    /// `HeadersDirectFetchBlocks`' walk from `last` back to the active chain at `snap`: the
    /// blocks to fetch, newest first; `None` when it collected 17 of them before reaching the
    /// active chain (the "Large reorg" branch).
    // Core: net_processing.cpp:3210-3223
    fn direct_fetch_walk(
        &self,
        chain: &dyn ChainAccess,
        snap: &ChainSnapshot,
        last: &EntryFacts,
    ) -> Option<Vec<EntryFacts>> {
        let mut to_fetch = Vec::new();
        let mut walk = Some(*last);
        while let Some(e) = walk
            && !chain.in_active_chain(snap, e.id)
            && to_fetch.len() <= MAX_BLOCKS_IN_TRANSIT_PER_PEER
        {
            let have_data = chain.status(snap, e.id).is_some_and(|s| s.have_data);
            if !have_data && !self.is_block_requested(&e.hash) {
                to_fetch.push(e);
            }
            walk = e.prev.and_then(|p| chain.entry(p));
        }
        walk.is_some_and(|e| chain.in_active_chain(snap, e.id)).then_some(to_fetch)
    }

    /// `FindNextBlocksToDownload(peer, count, vBlocks, nodeStaller)` for `st`'s peer, every read
    /// at `snap` (P3 §3.2.1). It takes the chain, snapshot and state rather than a whole
    /// [`SyncCx`], so the scheduler's units run without a network core (D77). A read Core's
    /// block index could not miss (an unindexed ancestor, an unpublished status) ends the walk
    /// as an invalid chain does, rather than panicking on the host's publication lag (D84).
    // Core: net_processing.cpp:1477-1559
    fn find_next_blocks_to_download(
        &self,
        chain: &dyn ChainAccess,
        snap: &ChainSnapshot,
        st: &mut PeerState,
        count: usize,
        out: &mut Vec<EntryFacts>,
        staller: &mut Option<PeerId>,
    ) {
        if count == 0 {
            return;
        }
        out.reserve(count);
        process_block_availability(chain, st.headers_mut());
        self.schedule(chain, snap, st, count, out, staller);
    }

    /// The body of [`Self::find_next_blocks_to_download`] after `ProcessBlockAvailability`, as
    /// an `Option` so that a missing read can end it with `?` (D84); every return is Core's.
    fn schedule(
        &self,
        chain: &dyn ChainAccess,
        snap: &ChainSnapshot,
        st: &mut PeerState,
        count: usize,
        out: &mut Vec<EntryFacts>,
        staller: &mut Option<PeerId>,
    ) -> Option<()> {
        let best = chain.entry(st.headers().best_known_block?)?;
        if best.chain_work < snap.tip.tip_work || best.chain_work < chain.min_chain_work() {
            // This peer has nothing interesting.
            return None;
        }
        let last = match st.last_common_block {
            Some(last) => last,
            // Bootstrap quickly by guessing a parent of our best tip is the forking point.
            None => chain.active_at(snap, best.height.min(snap.tip.tip_height))?,
        };
        // If the peer reorganized, the old last common block may not be an ancestor of its tip.
        let last = chain.last_common_ancestor(last, best.id);
        st.last_common_block = Some(last);
        if last == best.id {
            return None;
        }
        let mut walk = chain.entry(last)?;
        // The +1 detects stalling: the next block we could fetch if the window were 1 larger.
        let window_end = walk.height + BLOCK_DOWNLOAD_WINDOW;
        let max_height = best.height.min(window_end + 1);
        let mut waiting_for = None;
        let mut to_fetch = Vec::new();
        while walk.height < max_height {
            // Up to 128 (or more, if more are needed) successors of `walk` toward `best`.
            let n = (max_height - walk.height).min(count.saturating_sub(out.len()).max(128) as u32);
            walk = chain.entry(chain.ancestor(best.id, walk.height + n)?)?;
            to_fetch.clear();
            to_fetch.push(walk);
            for _ in 1..n {
                let prev = to_fetch.last()?.prev?;
                to_fetch.push(chain.entry(prev)?);
            }
            to_fetch.reverse();
            for e in &to_fetch {
                let status = chain.status(snap, e.id)?;
                if !status.is_valid(Validity::Tree) {
                    // We consider the chain that this peer is on invalid.
                    return None;
                }
                if status.have_data || chain.in_active_chain(snap, e.id) {
                    if status.have_txs_downloaded {
                        st.last_common_block = Some(e.id);
                    }
                } else if !self.is_block_requested(&e.hash) {
                    if e.height > window_end {
                        // We would be able to fetch it if the window were one larger.
                        if out.is_empty() && waiting_for != Some(st.id) {
                            *staller = waiting_for;
                        }
                        return None;
                    }
                    out.push(*e);
                    if out.len() == count {
                        return None;
                    }
                } else if waiting_for.is_none() {
                    // The first block already in flight: `lower_bound(hash)`'s peer.
                    waiting_for = Some(self.in_flight[&e.hash][0].peer);
                }
            }
        }
        Some(())
    }
}

impl BlockSync for BlockDownload {
    fn is_block_requested(&self, hash: &BlockHash) -> bool {
        BlockDownload::is_block_requested(self, hash)
    }

    fn any_in_flight(&self) -> bool {
        !self.in_flight.is_empty()
    }

    fn blocks_in_flight(&self, peer: PeerId) -> usize {
        self.peers.get(&peer).map_or(0, |p| p.blocks.len())
    }

    // Core: net_processing.cpp:6752-6781
    fn stall_section(&mut self, cx: &mut SyncCx<'_>, now_us: i64) -> bool {
        let id = cx.peer.id();
        let spacing = cx.core.config().params.consensus.pow_target_spacing;
        match self.section9(id, now_us, spacing) {
            None => return false,
            Some(Section9::Stalling {
                seen,
            }) => {
                tracing::info!(target: "net", "Peer={id} is stalling block download, disconnecting");
                cx.core.disconnect(id, Why::Processor("stalling block download"));
                self.stalling_timeout.raise(seen);
            }
            Some(Section9::Timeout(hash)) => {
                tracing::info!(target: "net", "Timeout downloading block {hash} from peer={id}, disconnecting");
                cx.core.disconnect(id, Why::Processor("block download timeout"));
            }
        }
        true
    }

    // Core: net_processing.cpp:6820-6839
    fn getdata_section(
        &mut self,
        cx: &mut SyncCx<'_>,
        batch: &mut GetDataBatch<'_>,
        sync_from_peer: bool,
        now_us: i64,
    ) {
        let id = cx.peer.id();
        let services = cx.peer.version_services();
        let queued = BlockSync::blocks_in_flight(self, id);
        // Core's short-circuit order: the IBD read latches.
        if !(can_serve_blocks(services)
            && cx.peer.can_relay()
            && ((sync_from_peer && !is_limited_peer(services))
                || !cx.chain.is_initial_block_download(cx.snap, cx.core.clock().node_now()))
            && queued < MAX_BLOCKS_IN_TRANSIT_PER_PEER)
        {
            return;
        }
        let (mut to_download, mut staller) = (Vec::new(), None);
        let count = MAX_BLOCKS_IN_TRANSIT_PER_PEER - queued;
        self.find_next_blocks_to_download(
            cx.chain,
            cx.snap,
            cx.state,
            count,
            &mut to_download,
            &mut staller,
        );
        for e in &to_download {
            batch.push_block(Inventory {
                inv_type: InvType::Block,
                hash: inv_hash(&e.hash),
            });
            // `BlockRequested` reads `GetTime()` itself, after the walk (D86).
            self.block_requested(PeerFacts::of(cx), e, cx.core.clock().node_now_micros());
            tracing::debug!(target: "net", "Requesting block {} ({}) peer={id}", e.hash, e.height);
        }
        self.start_stall(id, staller, now_us);
    }

    // Core: net_processing.cpp:3202-3258
    fn headers_direct_fetch(&mut self, cx: &mut SyncCx<'_>, last: BlockId) {
        let last = cx.chain.entry(last).expect("ProcessHeaders' last header is indexed");
        let valid_tree =
            cx.chain.status(cx.snap, last.id).is_some_and(|s| s.is_valid(Validity::Tree));
        if !(can_direct_fetch(cx.core, cx.snap)
            && valid_tree
            && cx.snap.tip.tip_work <= last.chain_work)
        {
            return;
        }
        let Some(to_fetch) = self.direct_fetch_walk(cx.chain, cx.snap, &last) else {
            tracing::debug!(target: "net", "Large reorg, won't direct fetch to {} ({})", last.hash, last.height);
            return;
        };
        let id = cx.peer.id();
        let mut inventory = Vec::new();
        // As much as possible, earliest first.
        for e in to_fetch.iter().rev() {
            if BlockSync::blocks_in_flight(self, id) >= MAX_BLOCKS_IN_TRANSIT_PER_PEER {
                break;
            }
            inventory.push(Inventory {
                inv_type: InvType::Block,
                hash: inv_hash(&e.hash),
            });
            self.block_requested(PeerFacts::of(cx), e, cx.core.clock().node_now_micros());
            tracing::debug!(target: "net", "Requesting block {} from  peer={id}", e.hash);
        }
        if inventory.len() > 1 {
            tracing::debug!(target: "net", "Downloading blocks toward {} ({}) via headers direct fetch", last.hash, last.height);
        }
        if !inventory.is_empty() {
            // In any case, download a lone block by compact block.
            let prev_valid_chain = last
                .prev
                .and_then(|p| cx.chain.status(cx.snap, p))
                .is_some_and(|s| s.is_valid(Validity::Chain));
            if !self.ignore_incoming_txs
                && cx.state.provides_cmpctblocks
                && inventory.len() == 1
                && self.in_flight_entries == 1
                && prev_valid_chain
            {
                inventory[0].inv_type = InvType::CompactBlock;
            }
            cx.core.push(
                cx.peer,
                out_msg(
                    CommandString::GETDATA,
                    &GetData {
                        inventory,
                    },
                ),
            );
        }
    }

    fn remove_block_request(&mut self, hash: &BlockHash, from: Option<PeerId>, now_us: i64) {
        BlockDownload::remove_block_request(self, hash, from, now_us);
    }

    fn emplace_block_source(&mut self, hash: BlockHash, peer: PeerId, full: bool) {
        BlockDownload::emplace_block_source(self, hash, peer, full);
    }

    fn erase_block_source(&mut self, hash: &BlockHash) {
        BlockDownload::erase_block_source(self, hash);
    }

    fn bind_source_counter(&mut self, counter: Arc<AtomicUsize>) {
        counter.store(self.block_source.len(), Ordering::SeqCst);
        self.source_len = counter;
    }

    fn bind_stalling_timeout(&mut self, timeout: StallingTimeout) {
        self.stalling_timeout = timeout;
    }

    fn bind_ignore_incoming_txs(&mut self, ignore: bool) {
        self.ignore_incoming_txs = ignore;
    }

    fn download(&mut self) -> Option<&mut BlockDownload> {
        Some(self)
    }

    fn push_compact_announcement(
        &mut self,
        cx: &mut SyncCx<'_>,
        block: &EntryFacts,
        recent: &RecentBlock,
    ) {
        compact::push_compact_announcement(cx, block, recent);
    }

    // Core: net_processing.cpp:2314-2343 PeerManagerImpl::BlockChecked
    fn block_checked(
        &mut self,
        cx: &mut BlockCheckedCx<'_>,
        hash: &BlockHash,
        invalid: Option<&BlockValidationError>,
        ibd: bool,
    ) {
        let source = self.block_source.get(hash).copied();
        match invalid {
            Some(state) => {
                if let Some((peer, full)) = source {
                    cx.with_peer(peer, |p| {
                        p.maybe_punish_node_for_block(state.result(), !full, "")
                    });
                }
            }
            // "The only blocks in flight are this one's": Core's stand-in for "this is the best
            // block we know of" while the tip is not updated yet.
            None if !ibd
                && self.in_flight.get(hash).map_or(0, Vec::len) == self.in_flight_entries =>
            {
                if let Some((peer, _)) = source
                    && !self.ignore_incoming_txs
                {
                    maybe_set_peer_as_announcing_header_and_ids(&mut self.hb_list, cx, peer);
                }
            }
            None => {}
        }
        self.erase_block_source(hash);
    }

    // Core: net_processing.cpp:1804-1830 (FinalizeNode, the block-download part)
    fn finalize(&mut self, peer: PeerId) {
        let Some(p) = self.peers.remove(&peer) else {
            return;
        };
        for b in p.blocks.iter().map(|q| &q.block) {
            if let Some(entries) = self.in_flight.get_mut(&b.hash) {
                let before = entries.len();
                entries.retain(|f| f.peer != peer);
                self.in_flight_entries -= before - entries.len();
                if entries.is_empty() {
                    self.in_flight.remove(&b.hash);
                }
            }
        }
        self.peers_downloading_from -= i32::from(!p.blocks.is_empty());
        assert!(self.peers_downloading_from >= 0, "m_peers_downloading_from < 0");
        if self.peers.is_empty() {
            debug_assert!(self.in_flight.is_empty() && self.in_flight_entries == 0);
            debug_assert_eq!(self.peers_downloading_from, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use dash_primitives::BlockHeader;
    use dash_types::Numeric as _;

    use dash_clock::NodeSeconds;
    use dash_consensus::BlsCtx;
    use dash_p2p::ServiceFlags;

    use super::*;
    use crate::processing::peer::HeaderGuard;
    use crate::processing::seams::{
        BlockPos, BlockRead, BlockSubmit, BlockWait, EntryStatus, HeaderBatch, HeadersResult,
        Interrupted, TipFacts, TreeRead, ViewExpired, Work,
    };

    fn entry(i: u8) -> EntryFacts {
        EntryFacts {
            id: BlockId(u32::from(i)),
            hash: BlockHash::from_lendian([i; 32]),
            prev: None,
            height: u32::from(i) + 100,
            time: 0,
            chain_work: Work::ONE,
            header: BlockHeader::default(),
        }
    }

    /// An entry with only its header: `BLOCK_VALID_TREE`, no data.
    const TREE_ONLY: EntryStatus = EntryStatus {
        have_data: false,
        data_pos: None,
        validity: Validity::Tree,
        failed: false,
        n_tx: 0,
        have_txs_downloaded: false,
    };

    /// A block tree for the scheduler's units: every entry `BLOCK_VALID_TREE` with no data
    /// unless [`Tree::status`] says otherwise, `chain_work = height + 1`.
    struct Tree {
        entries: Vec<EntryFacts>,
        status: HashMap<BlockId, EntryStatus>,
        tip: BlockId,
        min_work: Work,
        /// `GetAncestor` calls not from the tip: the walk's, one per 128-entry chunk.
        walk_calls: AtomicUsize,
    }

    impl Tree {
        /// Genesis only, the active tip, with data.
        fn new() -> Self {
            let mut t = Self {
                entries: Vec::new(),
                status: HashMap::new(),
                tip: BlockId(0),
                min_work: Work::ZERO,
                walk_calls: AtomicUsize::new(0),
            };
            t.push(None);
            t.have(t.tip);
            t
        }

        fn push(&mut self, prev: Option<BlockId>) -> BlockId {
            let id = BlockId(self.entries.len() as u32);
            let height = prev.map_or(0, |p| self.entries[p.0 as usize].height + 1);
            let mut hash = [0; 32];
            hash[..4].copy_from_slice(&id.0.to_le_bytes());
            self.entries.push(EntryFacts {
                id,
                hash: BlockHash::from_lendian(hash),
                prev,
                height,
                time: 0,
                chain_work: Work::from_u64(u64::from(height) + 1),
                header: BlockHeader::default(),
            });
            id
        }

        /// `n` entries on top of `from`; returns their ids, lowest first.
        fn extend(&mut self, from: BlockId, n: usize) -> Vec<BlockId> {
            let mut at = from;
            (0..n)
                .map(|_| {
                    at = self.push(Some(at));
                    at
                })
                .collect()
        }

        /// `BLOCK_HAVE_DATA` and `HaveTxsDownloaded`.
        fn have(&mut self, id: BlockId) {
            let s = self.status.entry(id).or_insert(EntryStatus {
                have_data: true,
                data_pos: None,
                validity: Validity::Transactions,
                failed: false,
                n_tx: 1,
                have_txs_downloaded: true,
            });
            s.have_data = true;
            s.data_pos = Some(BlockPos {
                file: 0,
                pos: id.0,
            });
        }

        fn status_mut(&mut self, id: BlockId) -> &mut EntryStatus {
            self.status.entry(id).or_insert(TREE_ONLY)
        }

        fn snap(&self) -> ChainSnapshot {
            let tip = self.entries[self.tip.0 as usize];
            ChainSnapshot {
                tip: TipFacts {
                    tip: tip.id,
                    tip_work: tip.chain_work,
                    tip_time: tip.time,
                    tip_height: tip.height,
                    tip_hash: tip.hash,
                    best_header: tip.id,
                    best_header_hash: tip.hash,
                    best_header_time: tip.time,
                    best_header_work: tip.chain_work,
                    best_header_height: tip.height,
                },
                bls: BlsCtx::from_legacy(false),
                epoch: 1,
                view: Default::default(),
            }
        }

        fn height(&self, id: BlockId) -> u32 {
            self.entries[id.0 as usize].height
        }
    }

    impl TreeRead for Tree {
        fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
            self.entries.iter().find(|e| e.hash == *hash).map(|e| e.id)
        }
        fn entry(&self, id: BlockId) -> Option<EntryFacts> {
            self.entries.get(id.0 as usize).copied()
        }
        fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
            if id != self.tip {
                self.walk_calls.fetch_add(1, Ordering::SeqCst);
            }
            let mut e = self.entries[id.0 as usize];
            if height > e.height {
                return None;
            }
            while e.height > height {
                e = self.entries[e.prev?.0 as usize];
            }
            Some(e.id)
        }
    }

    impl ChainAccess for Tree {
        fn snapshot(&self) -> Arc<ChainSnapshot> {
            Arc::new(self.snap())
        }
        fn valid_scripts(&self, _: &ChainSnapshot, _: BlockId) -> Result<bool, ViewExpired> {
            unreachable!()
        }
        fn locator(&self, _: Option<BlockId>) -> Vec<BlockHash> {
            unreachable!()
        }
        fn loading_blocks(&self) -> bool {
            false
        }
        fn min_chain_work(&self) -> Work {
            self.min_work
        }
        fn is_initial_block_download(&self, _: &ChainSnapshot, _: NodeSeconds) -> bool {
            unreachable!()
        }
        fn process_headers(&self, _: HeaderBatch, _: PeerId) -> Result<HeadersResult, Interrupted> {
            unreachable!()
        }
        fn process_block(&self, _: BlockSubmit) -> Result<BlockWait, Interrupted> {
            unreachable!()
        }
        fn status(&self, _: &ChainSnapshot, id: BlockId) -> Option<EntryStatus> {
            self.entries.get(id.0 as usize)?;
            Some(self.status.get(&id).copied().unwrap_or(TREE_ONLY))
        }
        fn in_active_chain(&self, _: &ChainSnapshot, id: BlockId) -> bool {
            let h = self.height(id);
            self.ancestor(self.tip, h) == Some(id)
        }
        fn active_at(&self, _: &ChainSnapshot, height: u32) -> Option<BlockId> {
            self.ancestor(self.tip, height)
        }
        fn last_common_ancestor(&self, a: BlockId, b: BlockId) -> BlockId {
            let (mut a, mut b) = (a, b);
            while self.height(a) > self.height(b) {
                a = self.entries[a.0 as usize].prev.unwrap();
            }
            while self.height(b) > self.height(a) {
                b = self.entries[b.0 as usize].prev.unwrap();
            }
            while a != b {
                a = self.entries[a.0 as usize].prev.unwrap();
                b = self.entries[b.0 as usize].prev.unwrap();
            }
            a
        }
        fn drain_snapshot(&self) -> (Arc<ChainSnapshot>, u64) {
            (self.snapshot(), 1)
        }
        fn wait_published(&self, _: u64) -> Result<(), Interrupted> {
            unreachable!()
        }
        fn read_block(&self, _: BlockId) -> BlockRead {
            unreachable!()
        }
        fn read_block_at(&self, _: BlockId, _: BlockPos) -> BlockRead {
            unreachable!()
        }
    }

    const ME: PeerId = 1;
    const OTHER: PeerId = 2;

    /// `ME`'s state inside a region (its header cell held), as a hook sees it.
    fn state(best: Option<BlockId>) -> PeerState {
        let mut st = PeerState::new(ME, ServiceFlags::NONE, 0);
        let cell = st.header_cell.clone();
        st.enter_region(HeaderGuard::lock(&cell));
        st.headers_mut().best_known_block = best;
        st
    }

    /// `FindNextBlocksToDownload(ME, count)`: the heights it returns and the staller.
    fn find(
        bd: &BlockDownload,
        t: &Tree,
        st: &mut PeerState,
        count: usize,
    ) -> (Vec<u32>, Option<PeerId>) {
        let (mut out, mut staller) = (Vec::new(), None);
        bd.find_next_blocks_to_download(t, &t.snap(), st, count, &mut out, &mut staller);
        (out.iter().map(|e| e.height).collect(), staller)
    }

    /// `BlockRequested` for `peer` at `now_us`, which must be new.
    fn enqueue(bd: &mut BlockDownload, peer: PeerId, inbound: bool, e: &EntryFacts, now_us: i64) {
        let heights = HeightsCell::default();
        let facts = PeerFacts {
            id: peer,
            inbound,
            heights: &heights,
        };
        assert!(bd.block_requested(facts, e, now_us));
    }

    fn request(bd: &mut BlockDownload, t: &Tree, peer: PeerId, inbound: bool, id: BlockId) {
        enqueue(bd, peer, inbound, &t.entries[id.0 as usize], 0);
    }

    #[test]
    fn nothing_interesting_returns_before_the_walk() {
        let mut t = Tree::new();
        let chain = t.extend(BlockId(0), 10);
        let bd = BlockDownload::new();
        // No best known block, or count 0.
        assert_eq!(find(&bd, &t, &mut state(None), 16), (vec![], None));
        assert_eq!(find(&bd, &t, &mut state(Some(chain[9])), 0), (vec![], None));
        // Less work than the tip.
        t.tip = chain[5];
        let mut st = state(Some(chain[4]));
        assert_eq!(find(&bd, &t, &mut st, 16), (vec![], None));
        assert_eq!(st.last_common_block, None, "returns before the bootstrap");
        // Less work than `nMinimumChainWork`.
        t.tip = BlockId(0);
        t.min_work = Work::from_u64(12);
        assert_eq!(find(&bd, &t, &mut state(Some(chain[9])), 16), (vec![], None));
        t.min_work = Work::from_u64(11);
        assert_eq!(find(&bd, &t, &mut state(Some(chain[9])), 16).0, (1..=10).collect::<Vec<_>>());
    }

    /// `pindex->nStatus & BLOCK_HAVE_DATA || ActiveChain().Contains(pindex)`: a pruned
    /// active-chain block (no data, its transactions once downloaded) is neither requested nor a
    /// stop for the last common block.
    #[test]
    fn a_pruned_active_block_is_not_requested_and_advances_the_last_common_block() {
        let mut t = Tree::new();
        let main = t.extend(BlockId(0), 10);
        for &id in &main {
            t.have(id);
        }
        for &id in &main[2..5] {
            t.status_mut(id).have_data = false;
        }
        t.tip = main[9];
        let ext = t.extend(main[9], 3);
        let bd = BlockDownload::new();
        let mut st = state(Some(ext[2]));
        st.last_common_block = Some(BlockId(0));
        assert_eq!(find(&bd, &t, &mut st, 16), (vec![11, 12, 13], None));
        assert_eq!(st.last_common_block, Some(main[9]));
    }

    #[test]
    fn the_last_common_block_bootstraps_from_the_tip_and_follows_a_reorg() {
        let mut t = Tree::new();
        let main = t.extend(BlockId(0), 10);
        for &id in &main {
            t.have(id);
        }
        t.tip = main[9];
        let fork = t.extend(main[4], 10); // heights 6..=15, from height 5
        let bd = BlockDownload::new();
        // `ActiveChain()[min(10, 15)]`, then `LastCommonAncestor` with the fork: height 5.
        let mut st = state(Some(fork[9]));
        assert_eq!(find(&bd, &t, &mut st, 16).0, (6..=15).collect::<Vec<_>>());
        assert_eq!(st.last_common_block, Some(main[4]));
        // On the tip's own chain, the walk raises it through every downloaded block.
        let tail = t.extend(main[9], 5);
        let mut st = state(Some(tail[4]));
        assert_eq!(find(&bd, &t, &mut st, 16).0, (11..=15).collect::<Vec<_>>());
        assert_eq!(st.last_common_block, Some(main[9]));
        // The peer reorganized onto the fork: the old common block is not its ancestor.
        st.headers_mut().best_known_block = Some(fork[9]);
        assert_eq!(find(&bd, &t, &mut st, 16).0, (6..=15).collect::<Vec<_>>());
        assert_eq!(st.last_common_block, Some(main[4]));
        // Equal to the best known block: nothing to fetch.
        let mut st = state(Some(main[9]));
        st.last_common_block = Some(main[9]);
        assert_eq!(find(&bd, &t, &mut st, 16), (vec![], None));
    }

    #[test]
    fn only_blocks_with_their_txs_downloaded_advance_the_last_common_block() {
        let mut t = Tree::new();
        let chain = t.extend(BlockId(0), 20);
        for &id in &chain[..5] {
            t.have(id);
        }
        // Height 6 has data but not its parent's txs; 7 has data.
        t.status_mut(chain[5]).have_data = true;
        t.status_mut(chain[6]).have_data = true;
        let bd = BlockDownload::new();
        let mut st = state(Some(chain[19]));
        assert_eq!(find(&bd, &t, &mut st, 16).0, (8..=20).collect::<Vec<_>>());
        assert_eq!(st.last_common_block, Some(chain[4]));
    }

    #[test]
    fn a_block_not_valid_tree_stops_the_walk() {
        let mut t = Tree::new();
        let chain = t.extend(BlockId(0), 10);
        t.status_mut(chain[2]).failed = true;
        let bd = BlockDownload::new();
        assert_eq!(find(&bd, &t, &mut state(Some(chain[9])), 16).0, vec![1, 2]);
        t.status_mut(chain[2]).failed = false;
        t.status_mut(chain[2]).validity = Validity::Reserved;
        assert_eq!(find(&bd, &t, &mut state(Some(chain[9])), 16).0, vec![1, 2]);
    }

    #[test]
    fn count_caps_the_result_and_blocks_in_flight_are_skipped() {
        let mut t = Tree::new();
        let chain = t.extend(BlockId(0), 40);
        let mut bd = BlockDownload::new();
        request(&mut bd, &t, OTHER, false, chain[0]);
        request(&mut bd, &t, ME, false, chain[2]);
        assert_eq!(
            find(&bd, &t, &mut state(Some(chain[39])), 5).0,
            vec![2, 4, 5, 6, 7],
            "heights 1 and 3 are in flight"
        );
    }

    /// The window is `last_common + 1024`; the walk reads one more block to detect stalling.
    #[test]
    fn the_window_edge_names_the_first_in_flight_peer_as_the_staller() {
        let mut t = Tree::new();
        let chain = t.extend(BlockId(0), 1030);
        let mut bd = BlockDownload::new();
        for &id in &chain[..1023] {
            request(&mut bd, &t, OTHER, false, id);
        }
        // Height 1024 is the window's last.
        assert_eq!(find(&bd, &t, &mut state(Some(chain[1029])), 16), (vec![1024], None));
        request(&mut bd, &t, 3, false, chain[1023]);
        // 1025 is past the window: OTHER holds the first block in flight.
        assert_eq!(find(&bd, &t, &mut state(Some(chain[1029])), 16), (vec![], Some(OTHER)));
        // With the best known block at the window's end, nothing past it is read.
        assert_eq!(find(&bd, &t, &mut state(Some(chain[1023])), 16), (vec![], None));
        // `waitingfor` is `mapBlocksInFlight.lower_bound(hash)`: the earliest request.
        request(&mut bd, &t, 3, false, chain[0]);
        assert_eq!(find(&bd, &t, &mut state(Some(chain[1029])), 16).1, Some(OTHER));
        // Never the peer itself.
        let mut bd = BlockDownload::new();
        request(&mut bd, &t, ME, false, chain[0]);
        for &id in &chain[1..1024] {
            request(&mut bd, &t, OTHER, false, id);
        }
        assert_eq!(find(&bd, &t, &mut state(Some(chain[1029])), 16), (vec![], None));
    }

    #[test]
    fn the_staller_is_named_only_when_nothing_else_was_found() {
        let mut t = Tree::new();
        let chain = t.extend(BlockId(0), 1030);
        let mut bd = BlockDownload::new();
        for &id in &chain[..1020] {
            request(&mut bd, &t, OTHER, false, id);
        }
        // 1021..=1024 are fetchable; then the window ends with `vBlocks` non-empty.
        assert_eq!(
            find(&bd, &t, &mut state(Some(chain[1029])), 16),
            (vec![1021, 1022, 1023, 1024], None)
        );
    }

    /// Chunks are `min(nMaxHeight − walk, max(count − found, 128))` entries, each one
    /// `GetAncestor` from the best known block.
    #[test]
    fn the_walk_reads_128_entry_chunks() {
        let mut t = Tree::new();
        let chain = t.extend(BlockId(0), 300);
        for &id in &chain[..200] {
            t.have(id);
        }
        t.tip = chain[199];
        let bd = BlockDownload::new();
        let mut st = state(Some(chain[299]));
        st.last_common_block = Some(BlockId(0));
        assert_eq!(find(&bd, &t, &mut st, 16).0, (201..=216).collect::<Vec<_>>());
        // 1..=128, then 129..=256.
        assert_eq!(t.walk_calls.load(Ordering::SeqCst), 2);
        assert_eq!(st.last_common_block, Some(chain[199]));
    }

    #[test]
    fn requested_from_outbound_looks_at_every_requester() {
        let mut t = Tree::new();
        let chain = t.extend(BlockId(0), 2);
        let mut bd = BlockDownload::new();
        let h = t.entries[chain[0].0 as usize].hash;
        assert!(!bd.is_block_requested_from_outbound(&h));
        request(&mut bd, &t, 5, true, chain[0]);
        assert!(!bd.is_block_requested_from_outbound(&h));
        request(&mut bd, &t, 6, false, chain[0]);
        assert!(bd.is_block_requested_from_outbound(&h));
        bd.remove_block_request(&h, Some(6), 0);
        assert!(!bd.is_block_requested_from_outbound(&h));
    }

    /// Direct fetch's walk: every block from `last` down to the active chain that has no data
    /// and is not in flight, while at most 16 were collected.
    #[test]
    fn the_direct_fetch_walk_stops_at_the_active_chain_or_after_17_blocks() {
        let mut t = Tree::new();
        let main = t.extend(BlockId(0), 10);
        for &id in &main {
            t.have(id);
        }
        t.tip = main[9];
        let snap = t.snap();
        let mut bd = BlockDownload::new();
        let walk = |bd: &BlockDownload, t: &Tree, last: BlockId| {
            let last = t.entries[last.0 as usize];
            bd.direct_fetch_walk(t, &snap, &last)
                .map(|v| v.iter().map(|e| e.height).collect::<Vec<_>>())
        };
        let ahead = t.extend(main[9], 18);
        assert_eq!(walk(&bd, &t, ahead[2]), Some(vec![13, 12, 11]));
        assert_eq!(walk(&bd, &t, main[9]), Some(vec![]), "the tip itself");
        t.status_mut(ahead[1]).have_data = true;
        request(&mut bd, &t, OTHER, false, ahead[0]);
        assert_eq!(walk(&bd, &t, ahead[2]), Some(vec![13]), "data and in-flight skipped");
        let mut bd = BlockDownload::new();
        t.status_mut(ahead[1]).have_data = false;
        // 17 missing blocks reach the tip; 18 do not.
        assert_eq!(walk(&bd, &t, ahead[16]).map(|v| v.len()), Some(17));
        assert_eq!(walk(&bd, &t, ahead[17]), None);
        // Blocks the walk skips do not count toward the 17, and the walk stops as soon as it
        // holds 17: the 17th found one block short of the tip is a large reorg.
        request(&mut bd, &t, OTHER, false, ahead[17]);
        assert_eq!(walk(&bd, &t, ahead[17]).map(|v| v.len()), Some(17));
        let mut bd = BlockDownload::new();
        request(&mut bd, &t, OTHER, false, ahead[0]);
        assert_eq!(walk(&bd, &t, ahead[17]), None);
        // A fork rejoins the active chain at its fork point.
        let fork = t.extend(main[4], 7);
        assert_eq!(walk(&bd, &t, fork[6]), Some((6..=12).rev().collect()));
    }

    #[test]
    fn the_stalling_timeout_doubles_to_64_and_decays_as_core_truncates() {
        let t = StallingTimeout::default();
        assert_eq!(t.get(), BLOCK_STALLING_TIMEOUT_DEFAULT);
        assert_eq!(t.decay(), None, "at the default");
        // Bounded, so a mutant that never reaches the end fails instead of hanging.
        let ups: Vec<_> = std::iter::from_fn(|| t.raise(t.get())).take(10).collect();
        assert_eq!(ups, [4, 8, 16, 32, 64]);
        assert_eq!(t.get(), BLOCK_STALLING_TIMEOUT_MAX);
        let downs: Vec<_> = std::iter::from_fn(|| t.decay()).take(20).collect();
        assert_eq!(downs, [54, 45, 38, 32, 27, 22, 18, 15, 12, 10, 8, 6, 5, 4, 3, 2]);
        // Every value, not only the chain's: `max(duration_cast<seconds>(t * 0.85), 2s)`.
        for v in 3..=64i64 {
            t.set(v);
            assert_eq!(t.decay(), Some((v * 85 / 100).max(2)), "t = {v}");
        }
        // `min(2t, 64)`: 33 → 64; 64 → 64 is no change and no exchange.
        t.set(33);
        assert_eq!(t.raise(33), Some(64));
        assert_eq!(t.raise(64), None);
    }

    #[test]
    fn a_raise_from_a_stale_value_loses_the_exchange() {
        let t = StallingTimeout::default();
        // Another peer's section 9 doubled it after this one loaded 2.
        assert_eq!(t.raise(2), Some(4));
        assert_eq!(t.raise(2), None);
        assert_eq!(t.get(), 4);
        // A decay between the load and the exchange: the loser leaves the winner's value.
        assert_eq!(t.decay(), Some(3));
        assert_eq!(t.raise(4), None);
        assert_eq!(t.get(), 3);
    }

    fn queue(bd: &mut BlockDownload, peer: PeerId, b: u8, now_us: i64) {
        enqueue(bd, peer, false, &entry(b), now_us);
    }

    #[test]
    fn a_stall_disconnects_strictly_after_the_timeout_and_doubles_it() {
        let mut bd = BlockDownload::new();
        assert_eq!(bd.section9(ME, 0, 150), None, "no state");
        queue(&mut bd, ME, 1, 0);
        bd.start_stall(OTHER, Some(ME), 5_000_000);
        assert_eq!(bd.times(ME), Some((0, 5_000_000)));
        // `m_stalling_since < now - 2s`.
        assert_eq!(bd.section9(ME, 7_000_000, 150), None);
        assert_eq!(
            bd.section9(ME, 7_000_001, 150),
            Some(Section9::Stalling {
                seen: 2
            })
        );
        assert_eq!(bd.stalling_timeout.get(), 2, "the verdict alone raises nothing");
        assert_eq!(bd.stalling_timeout.raise(2), Some(4));
        // The next staller gets 4 s.
        assert_eq!(bd.section9(ME, 9_000_000, 150), None);
        assert_eq!(
            bd.section9(ME, 9_000_001, 150),
            Some(Section9::Stalling {
                seen: 4
            })
        );
    }

    #[test]
    fn the_stall_clock_starts_once_and_only_when_the_requester_has_nothing_in_flight() {
        let mut bd = BlockDownload::new();
        queue(&mut bd, OTHER, 1, 0);
        bd.start_stall(ME, None, 10);
        assert_eq!(bd.times(OTHER), Some((0, 0)), "no staller");
        queue(&mut bd, ME, 2, 0);
        bd.start_stall(ME, Some(OTHER), 10);
        assert_eq!(bd.times(OTHER), Some((0, 0)), "the requester has a block in flight");
        bd.remove_block_request(&entry(2).hash, None, 0);
        bd.start_stall(ME, Some(OTHER), 10);
        bd.start_stall(ME, Some(OTHER), 20);
        assert_eq!(bd.times(OTHER), Some((0, 10)), "not restarted");
        // Only the staller's own removal resets it.
        bd.remove_block_request(&entry(1).hash, None, 30);
        assert_eq!(bd.times(OTHER), Some((30, 0)));
    }

    /// `now > since + spacing × (1 + 0.5 × others)`, in double µs, for 0, 1 and 7 other peers.
    #[test]
    fn the_block_timeout_is_a_strict_double_compare_scaled_by_other_downloaders() {
        for others in [0i64, 1, 7] {
            let mut bd = BlockDownload::new();
            queue(&mut bd, ME, 1, 1_000);
            queue(&mut bd, ME, 2, 2_000);
            for p in 0..others {
                queue(&mut bd, 10 + p, 3, 0);
            }
            assert_eq!(i64::from(bd.peers_downloading_from()), others + 1);
            let deadline = 1_000 + 150 * 1_000_000 * (2 + others) / 2;
            assert_eq!(bd.section9(ME, deadline, 150), None, "others = {others}");
            assert_eq!(
                bd.section9(ME, deadline + 1, 150),
                Some(Section9::Timeout(entry(1).hash)),
                "the oldest request; others = {others}"
            );
        }
        // A one-second spacing with one other downloader: 1 s × 1.5.
        let mut bd = BlockDownload::new();
        queue(&mut bd, ME, 1, 0);
        queue(&mut bd, OTHER, 2, 0);
        assert_eq!(bd.section9(ME, 1_500_000, 1), None);
        assert_eq!(bd.section9(ME, 1_500_001, 1), Some(Section9::Timeout(entry(1).hash)));
        // Completing the front request moves `m_downloading_since` to then.
        queue(&mut bd, ME, 3, 0);
        bd.remove_block_request(&entry(1).hash, None, 1_400_000);
        assert_eq!(bd.section9(ME, 2_900_000, 1), None);
        assert_eq!(bd.section9(ME, 2_900_001, 1), Some(Section9::Timeout(entry(3).hash)));
    }

    #[test]
    fn a_stall_is_judged_before_the_block_timeout() {
        let mut bd = BlockDownload::new();
        queue(&mut bd, ME, 1, 0);
        bd.start_stall(OTHER, Some(ME), 1);
        assert_eq!(
            bd.section9(ME, 1_000 * 1_000_000, 150),
            Some(Section9::Stalling {
                seen: 2
            })
        );
    }

    #[test]
    fn the_source_map_never_overwrites_and_keeps_the_guard_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut bd = BlockDownload::new();
        BlockSync::bind_source_counter(&mut bd, Arc::clone(&counter));
        let h = entry(1).hash;
        bd.emplace_block_source(h, 7, true);
        bd.emplace_block_source(h, 8, false);
        assert_eq!(bd.block_source(&h), Some((7, true)));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        bd.erase_block_source(&h);
        bd.erase_block_source(&h);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    /// The HB list's world: each peer's `(provides_cmpctblocks, inbound)` while it has a state,
    /// which peers are fully connected, and the `sendcmpct`s pushed.
    #[derive(Default)]
    struct HbWorld {
        states: HashMap<PeerId, (bool, bool)>,
        connected: Vec<PeerId>,
        sent: Mutex<Vec<(PeerId, bool)>>,
    }

    impl HbWorld {
        /// Fully connected peers with states, each providing compact blocks.
        fn with(peers: &[(PeerId, bool)]) -> Self {
            Self {
                states: peers.iter().map(|&(id, inbound)| (id, (true, inbound))).collect(),
                connected: peers.iter().map(|&(id, _)| id).collect(),
                ..Self::default()
            }
        }

        /// `FinalizeNode` for `id`: no state, not connected.
        fn finalize(&mut self, id: PeerId) {
            self.states.remove(&id);
            self.connected.retain(|&p| p != id);
        }

        fn select(&self, list: &mut VecDeque<PeerId>, source: PeerId) -> Vec<(PeerId, bool)> {
            maybe_set_peer_as_announcing_header_and_ids(list, self, source);
            std::mem::take(&mut self.sent.lock())
        }
    }

    impl HbPeers for HbWorld {
        fn node_state(&self, id: PeerId) -> Option<(bool, bool)> {
            self.states.get(&id).copied()
        }
        fn set_high_bandwidth_to(&self, id: PeerId, high_bandwidth: bool) -> bool {
            let ok = self.connected.contains(&id);
            if ok {
                self.sent.lock().push((id, high_bandwidth));
            }
            ok
        }
    }

    const O1: PeerId = 11;
    const O2: PeerId = 12;
    const I1: PeerId = 21;
    const I2: PeerId = 22;
    const I3: PeerId = 23;

    fn list(ids: &[PeerId]) -> VecDeque<PeerId> {
        ids.iter().copied().collect()
    }

    /// np:1371-1375: no state, or no `sendcmpct(_, 1)` from the peer: nothing.
    #[test]
    fn hb_needs_a_state_that_provides_compact_blocks() {
        let mut w = HbWorld::with(&[(O1, false)]);
        let mut l = list(&[]);
        assert_eq!(w.select(&mut l, O2), []);
        w.states.insert(O1, (false, false));
        assert_eq!(w.select(&mut l, O1), []);
        assert!(l.is_empty());
    }

    /// np:1405-1410: a new peer gets `sendcmpct(1)` and joins at the back.
    #[test]
    fn hb_a_new_peer_joins_with_sendcmpct_1() {
        let w = HbWorld::with(&[(O1, false), (I1, true)]);
        let mut l = list(&[O1]);
        assert_eq!(w.select(&mut l, I1), [(I1, true)]);
        assert_eq!(l, [O1, I1]);
    }

    /// np:1378-1382: a member moves to the back, and nobody is told.
    #[test]
    fn hb_a_member_moves_to_the_back_silently() {
        let w = HbWorld::with(&[(O1, false), (I1, true), (I2, true)]);
        let mut l = list(&[O1, I1, I2]);
        assert_eq!(w.select(&mut l, O1), []);
        assert_eq!(l, [I1, I2, O1]);
    }

    /// np:1411-1415: a fourth peer pops the front, which gets `sendcmpct(0)`.
    #[test]
    fn hb_a_fourth_peer_pops_the_front_with_sendcmpct_0() {
        let w = HbWorld::with(&[(O1, false), (O2, false), (I1, true), (I2, true)]);
        let mut l = list(&[O1, O2, I1]);
        assert_eq!(w.select(&mut l, I2), [(I2, true), (O1, false)]);
        assert_eq!(l, [O2, I1, I2]);
    }

    /// np:1386-1395: an inbound joiner swaps the only outbound HB peer out of the front.
    #[test]
    fn hb_an_inbound_joiner_keeps_the_last_outbound_peer() {
        let w = HbWorld::with(&[(O1, false), (I1, true), (I2, true), (I3, true)]);
        let mut l = list(&[O1, I1, I2]);
        assert_eq!(w.select(&mut l, I3), [(I3, true), (I1, false)]);
        assert_eq!(l, [O1, I2, I3]);
    }

    /// The swap's other guards: an outbound joiner, two outbound members, and an inbound front
    /// each leave the order alone.
    #[test]
    fn hb_no_swap_without_every_guard() {
        let w = HbWorld::with(&[(O1, false), (O2, false), (I1, true), (I2, true), (I3, true)]);
        let mut l = list(&[O1, I1, I2]);
        assert_eq!(w.select(&mut l, O2), [(O2, true), (O1, false)]);
        assert_eq!(l, [I1, I2, O2]);

        let mut l = list(&[O1, O2, I1]);
        assert_eq!(w.select(&mut l, I2), [(I2, true), (O1, false)]);
        assert_eq!(l, [O2, I1, I2]);

        let mut l = list(&[I1, I2, O1]);
        assert_eq!(w.select(&mut l, I3), [(I3, true), (I1, false)]);
        assert_eq!(l, [I2, O1, I3]);
    }

    /// Q4: a finalized id stays, counts toward the three but not as outbound, fails the swap's
    /// front guard, and its pop tells nobody (np:1384, np:1391, np:1411-1415).
    #[test]
    fn hb_a_finalized_front_still_counts_and_is_popped_silently() {
        let mut w = HbWorld::with(&[(O1, false), (O2, false), (I1, true), (I2, true)]);
        w.finalize(O1);
        let mut l = list(&[O1, O2, I1]);
        // O2 is the only outbound member counted, but the front has no state: no swap.
        assert_eq!(w.select(&mut l, I2), [(I2, true)]);
        assert_eq!(l, [O2, I1, I2]);
    }

    /// np:1405-1410: `ForNode` skips a source that is not fully connected, so it does not join;
    /// the swap before it has already run, and with three members nothing is popped.
    #[test]
    fn hb_a_source_that_is_not_fully_connected_does_not_join() {
        let mut w = HbWorld::with(&[(O1, false), (I1, true), (I2, true), (I3, true)]);
        w.connected.retain(|&p| p != I3);
        let mut l = list(&[O1, I1, I2]);
        assert_eq!(w.select(&mut l, I3), []);
        assert_eq!(l, [I1, O1, I2]);
    }
}
