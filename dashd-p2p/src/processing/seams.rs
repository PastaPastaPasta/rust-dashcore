//! The processor's seams to the chain and to later tasks (P2a §2.4), each with its M2 default.
//!
//! [`ChainAccess`] is the read side of SB4's `dash_chain::ChainReader` plus CH1-node's
//! `ChainCommand::ProcessHeaders` (CH1-node §2.2, §6.3). The shapes here mirror theirs (D-row
//! P2a-core D3) because `dash-chain`'s snapshot, `HeaderStop` result and `PeerId` newtype don't
//! fit `sim`; P2a-node moves to the real types (D59).

use std::any::Any;
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use dash_clock::NodeSeconds;
use dash_consensus::primitives::TxHash;
use dash_consensus::{Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, Header};
use dash_p2p::addr::AddrRelay;
use dash_p2p::{Address, CommandString, NetCore, PeerHandle, PeerId, ServiceFlags};
use dash_p2p_core::{ClSig, GetData, Inv, Inventory};
use dash_primitives::{BlockHash, BlockHeader};

use super::download::{BlockDownload, StallingTimeout};
use super::eviction::fully_connected;
use super::logic::{PeerMap, out_msg};
use super::misbehavior;
use super::peer::PeerState;
use super::peerman::RecentBlock;
use super::registry::{HandlerCx, MessageProcessingResult};

/// Core's `arith_uint256` chain work.
pub type Work = dash_num::Arith256;

/// The tip and best-header facts of one snapshot (SB4 `TipFacts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipFacts {
    /// `ActiveChain().Tip()`.
    pub tip: BlockId,
    /// Its chain work.
    pub tip_work: Work,
    /// Its `nTime`.
    pub tip_time: u32,
    /// Its height.
    pub tip_height: u32,
    /// Its hash.
    pub tip_hash: BlockHash,
    /// Core's `m_best_header` (never null after load, R5).
    pub best_header: BlockId,
    /// Its hash.
    pub best_header_hash: BlockHash,
    /// Its `nTime`.
    pub best_header_time: u32,
    /// Its chain work.
    pub best_header_work: Work,
    /// Its height.
    pub best_header_height: u32,
}

/// One coherent, retained read of the chain (SB4 `ChainReadSnapshot`, R8): a reader keeps the
/// `Arc` for the whole Core critical section.
#[derive(Clone, Debug)]
pub struct ChainSnapshot {
    /// Tip and best header.
    pub tip: TipFacts,
    /// The BLS scheme for the next block (`bls_ctx_for_block(tip_height + 1)`), the source of
    /// every read's `ProtoCtx.bls` (P2a §3.2.5).
    pub bls: BlsCtx,
    /// +1 on every publication.
    pub epoch: u64,
    /// The host's published view this snapshot was built from: the status and active-chain
    /// reads that take a snapshot answer at it, not at the live state (BL-236, D99).
    pub view: ViewToken,
}

/// The host's view behind a [`ChainSnapshot`], opaque to the processor (BL-236, D99): the host
/// stores what its reads need (CH1-node's `ChainView`; `sim`'s published statuses) and reads it
/// back with [`ViewToken::get`]. Empty for a snapshot no host built.
#[derive(Clone, Default)]
pub struct ViewToken(Option<Arc<dyn Any + Send + Sync>>);

impl ViewToken {
    /// A token holding `view`.
    pub fn new<T: Any + Send + Sync>(view: Arc<T>) -> Self {
        Self(Some(view))
    }

    /// The view, when the token holds a `T`.
    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        Arc::clone(self.0.as_ref()?).downcast().ok()
    }
}

impl std::fmt::Debug for ViewToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "ViewToken(..)"
        } else {
            "ViewToken(empty)"
        })
    }
}

/// An index entry's immutable fields (SB4 `EntryView`, the part P2a reads).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryFacts {
    /// The entry.
    pub id: BlockId,
    /// Its hash.
    pub hash: BlockHash,
    /// `pprev`.
    pub prev: Option<BlockId>,
    /// `nHeight`.
    pub height: u32,
    /// `nTime`.
    pub time: u32,
    /// `nChainWork`.
    pub chain_work: Work,
    /// `GetBlockHeader()`, for announcements.
    pub header: BlockHeader,
}

/// The `ProcessHeaders` payload (P2a R7; SB4 `HeaderBatch`): `hashes[i]` is `headers[i]`'s X11
/// hash, complete only after the continuity check (P2a §3.4.9).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeaderBatch {
    /// The headers, in message order.
    pub headers: Vec<Header>,
    /// Their hashes.
    pub hashes: Vec<BlockHash>,
}

impl HeaderBatch {
    /// A batch of `headers`, with the hashes each already holds.
    pub fn new(headers: Vec<Header>) -> Self {
        Self {
            hashes: headers.iter().map(Header::hash).collect(),
            headers,
        }
    }
}

/// The reply to a batch (SB4 `HeadersResult`, the fields P2a reads).
#[derive(Clone, Debug)]
pub struct HeadersResult {
    /// Core's `pindexLast`: the last header accepted or already known.
    pub last_accepted: Option<BlockId>,
    /// The state Core's `ProcessNewBlockHeaders` returned when a header was invalid.
    pub invalid: Option<BlockValidationError>,
    /// Published before the reply (R3).
    pub snap: Arc<ChainSnapshot>,
}

/// The block-tree reads `ProcessBlockAvailability` and `PeerHasHeader` make (np:1433-1456):
/// lookup by hash, an entry's fields (its chain work) and `GetAncestor`. The actor reads the
/// published tree through [`ChainAccess`]; V's `NewPoWValidBlock` visit reads its own in-scope
/// tree through C8's `TreeRead`, so a header new in that accept is found as Core finds it under
/// `cs_main` (P3 §3.6, D50, D101).
pub trait TreeRead {
    /// Core's `LookupBlockIndex` (R1).
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId>;
    /// An entry's immutable fields (R2).
    ///
    /// Total for every entry [`TreeRead::ancestor`] returns on a snapshot's tip, and for the
    /// `prev` of such an entry: `getheaders` serving (P2b) `expect`s them, as Core dereferences
    /// `CBlockIndex*` there.
    fn entry(&self, id: BlockId) -> Option<EntryFacts>;
    /// `LookupBlockIndex`, then the entry's fields.
    fn entry_by_hash(&self, hash: &BlockHash) -> Option<EntryFacts> {
        self.lookup(hash).and_then(|id| self.entry(id))
    }
    /// Core's `GetAncestor`: `None` only above `id`'s height.
    ///
    /// Total for every height `0..=snap.tip.tip_height` with `id` a snapshot's tip: `getheaders`
    /// serving (P2b) `expect`s it there. A host adapter over SB4 must therefore not map
    /// `BelowFloor` (a pruned-away or not-yet-loaded height) to `None` for those heights; it
    /// must keep every active-chain entry reachable, as Core's block index does.
    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId>;
}

/// What the processor reads from the chain and the one command it awaits (P2a §2.5.3, R1–R8).
/// Every method but [`ChainAccess::process_headers`] is lock-free or a short read.
///
/// Lock order (D66): the processor calls these while it holds a peer's state, so no method may
/// take a lock that the chain side holds while it calls into the processor
/// ([`Shared::updated_block_tip`](super::peerman::Shared::updated_block_tip),
/// [`Shared::block_checked`](super::peerman::Shared::block_checked)), or that waits on the peer
/// map, a peer's state or a misbehaviour cell. CH1-node's `ChainViewHandle` reads satisfy it:
/// they take only CH1-internal leaf locks (a status journal's log mutex since CH1 D63, the
/// journal index's read lock since D68), which the chain side never holds across a call into
/// the processor (CH1 D68 r1 N3).
pub trait ChainAccess: TreeRead + Send + Sync {
    /// The last published snapshot (R8).
    fn snapshot(&self) -> Arc<ChainSnapshot>;
    /// Core's `pindex->IsValid(BLOCK_VALID_SCRIPTS)`: the block was connected once and is not
    /// marked failed (`BLOCK_FAILED_MASK`, chain.h:305; SB4 `EntryView::status`). Core never
    /// raises genesis to it (validation.cpp:2536 returns before :2900), which no caller can
    /// observe: genesis is always on the active chain. `BlockRequestAllowed` reads it for an
    /// entry off the active chain (P2b D6). Read at `snap`'s view, as [`ChainAccess::status`]
    /// is (BL-236). Unlike `status`, it is never read again on a newer view: a host whose view
    /// reads can expire (CH1-node BL-224) answers [`ViewExpired`], and the caller restarts its
    /// whole decision on [`ChainAccess::snapshot`] (DEC-292, D121).
    fn valid_scripts(&self, snap: &ChainSnapshot, id: BlockId) -> Result<bool, ViewExpired>;
    /// Core's `GetLocator`; `None` is v24's empty locator (DEC-82).
    fn locator(&self, id: Option<BlockId>) -> Vec<BlockHash>;
    /// Core's `LoadingBlocks()`.
    fn loading_blocks(&self) -> bool;
    /// `nMinimumChainWork`.
    fn min_chain_work(&self) -> Work;
    /// Core's latched `IsInitialBlockDownload()` against `snap` and the node clock (R4).
    fn is_initial_block_download(&self, snap: &ChainSnapshot, now: NodeSeconds) -> bool;
    /// `ChainCommand::ProcessHeaders`: blocks until V answers (P2a §3.1.5), or until the chain
    /// side shuts down ([`Interrupted`], CH1-node's `WaitError`).
    fn process_headers(
        &self,
        batch: HeaderBatch,
        from: PeerId,
    ) -> Result<HeadersResult, Interrupted>;
    /// `ProcessNewBlock(block, force, &new_block)` through V (CH1-node `ChainCommand::ProcessBlock
    /// { reply: BlockReply::Activated }`): returns the reply the actor waits on. Sending can block
    /// on V's bounded command lane, as any `ChainCommand` send does (P3 §2.4).
    fn process_block(&self, req: BlockSubmit) -> Result<BlockWait, Interrupted>;
    /// The entry's mutable status (`nStatus`, `nTx`, `nChainTx != 0`) as published with `snap`'s
    /// view, so a caller holding a snapshot across a publication still reads that snapshot's
    /// status (P3 §2.4, D14, BL-236). `None`
    /// for an entry whose status is not published yet: section 12 then schedules nothing from
    /// that peer this round (D84). Direct fetch differs: its walk reads `None` as "no data" and
    /// would request the block, and `None` for the last header skips it silently as not
    /// `VALID_TREE`. It relies on R3 instead ([`HeadersResult::snap`] is published before the
    /// reply, so every header it reads is), and a host that relaxes R3 must revisit it. A host
    /// whose view reads can expire (CH1-node BL-224) never answers `None` for an expired one: it
    /// reads the entry again on its newest view (D120).
    fn status(&self, snap: &ChainSnapshot, id: BlockId) -> Option<EntryStatus>;
    /// `ActiveChain().Contains(pindex)` at `snap`.
    fn in_active_chain(&self, snap: &ChainSnapshot, id: BlockId) -> bool;
    /// `ActiveChain()[height]` at `snap`.
    fn active_at(&self, snap: &ChainSnapshot, height: u32) -> Option<BlockId>;
    /// `LastCommonAncestor(a, b)` (chain.cpp:166); both are in the block tree, so it exists.
    fn last_common_ancestor(&self, a: BlockId, b: BlockId) -> BlockId;
    /// One `ChainView` load: its snapshot and the `view_seq` it was published as. Every drain,
    /// and so every message's snapshot, takes both from this one load, never from
    /// [`ChainAccess::snapshot`], whose tip cell may be one store ahead (P3 §3.6, D47).
    fn drain_snapshot(&self) -> (Arc<ChainSnapshot>, u64);
    /// Returns once V has published a `ChainView` with `view_seq ≥ seq`: at once if the current
    /// one has, else through a class R no-op round trip (DEC-229 (b)). Only a header cell's
    /// `Ahead` calls it (P3 §3.6, D50, D56, D57).
    fn wait_published(&self, seq: u64) -> Result<(), Interrupted>;
    /// Core's `ReadBlockFromDisk(block, pindex)` (blockstorage.cpp:805-848), for section 7's
    /// `cmpctblock` (np:6491; A-3): the block of an entry with data, from the LRU or the block
    /// files at the entry's position now. A failed read is [`BlockRead::Pruned`] when the entry
    /// has since lost its data to a prune, Core's `IsBlockPruned`, else [`BlockRead::Failed`],
    /// with Core's error line logged (P4b D3). It takes no lock that V holds while it calls the
    /// processor (P3 §3.6, D57).
    fn read_block(&self, id: BlockId) -> BlockRead;
    /// Core's `ReadBlockFromDisk(block, pos)` for `ProcessGetBlockData` (np:2819) and
    /// `GETBLOCKTXN` (np:4737): `id`'s block read at `pos`, the position the caller took with its
    /// `HAVE_DATA` test ([`EntryStatus::data_pos`]), so a block pruned since but not yet unlinked
    /// is still read, as in Core (P4b D16). A block whose hash is not `id`'s is a failed read
    /// (np:2820); a failed read is classified as [`ChainAccess::read_block`] classifies it.
    fn read_block_at(&self, id: BlockId, pos: BlockPos) -> BlockRead;
    /// Core's `fPruneMode` (blockstorage.h `BlockManager::IsPruneMode`): what `GETBLOCKS`'
    /// prune stop checks (np:4680). The NODE_NETWORK_LIMITED bound (np:2795) reads the local
    /// services instead. Off until CH8 prunes (P4b D4).
    fn prune_mode(&self) -> bool {
        false
    }
}

/// Core's `FlatFilePos` of a block's data (`CBlockIndex::GetBlockPos`): its `blk` file and the
/// offset of its bytes there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockPos {
    /// `nFile`.
    pub file: i32,
    /// `nDataPos`.
    pub pos: u32,
}

/// [`ChainAccess::read_block`]'s answer (P4b D3).
#[derive(Clone)]
pub enum BlockRead {
    /// The block.
    Block(Arc<Block>),
    /// The read failed and the entry has no data now: a prune raced it (np:2815, O17 P2P-17).
    Pruned,
    /// The read failed with the data still indexed: an I/O fault (np:2822).
    Failed,
}

impl BlockRead {
    /// The block, if read.
    #[must_use]
    pub fn block(self) -> Option<Arc<Block>> {
        match self {
            Self::Block(b) => Some(b),
            Self::Pruned | Self::Failed => None,
        }
    }
}

/// A block handed to V (P3 §2.4).
#[derive(Clone)]
pub struct BlockSubmit {
    /// The block.
    pub block: Arc<Block>,
    /// Its hash.
    pub hash: BlockHash,
    /// `force_processing`.
    pub force: bool,
    /// The source peer.
    pub from: PeerId,
    /// `m_checked_merkle_root` (v:4326, v:4362): `is_block_mutated` has already checked this
    /// block's merkle root, so V's `CheckBlock` skips that pass (P3 A-4b, D24).
    pub merkle_checked: bool,
}

impl std::fmt::Debug for BlockSubmit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockSubmit")
            .field("hash", &self.hash)
            .field("force", &self.force)
            .field("from", &self.from)
            .field("merkle_checked", &self.merkle_checked)
            .finish_non_exhaustive()
    }
}

/// V's reply to a [`BlockSubmit`]: `ProcessNewBlock`'s `*new_block` (v:4875, v:4835).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockOutcome {
    /// `AcceptBlock`'s `*fNewBlock`: set before `SaveBlockToDisk`, false when `CheckBlock`
    /// fails or `AcceptBlock` returns before v:4835 (P3 C3).
    pub new_block: bool,
}

/// The reply slot the actor waits on: the host wraps CH1-node's `ActorReply<BlockResult>`, a
/// `Mutex` + `Condvar` future whose `close` stores the value before it wakes (reply.rs:287-298),
/// so a poll after each wake never loses it (P3 §2.6, D33). A reply V drops is `Interrupted`.
pub struct BlockWait(pub Pin<Box<dyn Future<Output = Result<BlockOutcome, Interrupted>> + Send>>);

impl BlockWait {
    /// A reply already answered.
    pub fn ready(r: Result<BlockOutcome, Interrupted>) -> Self {
        Self(Box::pin(std::future::ready(r)))
    }
}

impl std::fmt::Debug for BlockWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BlockWait")
    }
}

/// `nStatus & BLOCK_VALID_MASK` (chain.h:126-160), in Core's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Validity {
    /// `BLOCK_VALID_UNKNOWN`.
    Unknown,
    /// `BLOCK_VALID_RESERVED`.
    Reserved,
    /// `BLOCK_VALID_TREE`.
    Tree,
    /// `BLOCK_VALID_TRANSACTIONS`.
    Transactions,
    /// `BLOCK_VALID_CHAIN`.
    Chain,
    /// `BLOCK_VALID_SCRIPTS`.
    Scripts,
}

/// An entry's mutable status (P3 §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryStatus {
    /// `BLOCK_HAVE_DATA`.
    pub have_data: bool,
    /// `GetBlockPos()` when `have_data`, read with it.
    pub data_pos: Option<BlockPos>,
    /// `nStatus & BLOCK_VALID_MASK`.
    pub validity: Validity,
    /// `nStatus & BLOCK_FAILED_MASK`.
    pub failed: bool,
    /// `nTx`.
    pub n_tx: u32,
    /// `nChainTx != 0` (`HaveTxsDownloaded`, chain.h:264).
    pub have_txs_downloaded: bool,
}

impl EntryStatus {
    /// `CBlockIndex::IsValid(up_to)`.
    // Core: chain.h:300-308
    pub fn is_valid(&self, up_to: Validity) -> bool {
        !self.failed && self.validity >= up_to
    }
}

/// [`ChainAccess::valid_scripts`] can no longer read the snapshot's view (CH1-node BL-224): the
/// caller restarts its decision on a newer snapshot (DEC-292).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewExpired;

/// [`ChainAccess::process_headers`] got no answer: the chain side is shutting down. The
/// message ends quietly, with no score and no reply, and the host's shutdown interrupts the loop
/// (D23). Core's `ProcessNewBlockHeaders` never waits, so it has no such outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interrupted;

/// What a block-sync hook reaches for one peer, as Core's `PeerManagerImpl` methods reach
/// `CNodeState` under `cs_main` (P2a §2.4 `Cx`): the peer's state, read and written in place
/// (`pindexBestKnownBlock`, `pindexLastCommonBlock`), its handle, the chain and the snapshot of
/// the message or pass that called the hook (D65).
pub struct SyncCx<'a> {
    /// The network core.
    pub core: &'a NetCore,
    /// The peer.
    pub peer: &'a PeerHandle,
    /// Its processor state.
    pub state: &'a mut PeerState,
    /// The chain.
    pub chain: &'a dyn ChainAccess,
    /// The caller's chain snapshot (R8).
    pub snap: &'a ChainSnapshot,
}

impl<'a> SyncCx<'a> {
    /// The hook context for `peer` with `state`, at `snap`.
    pub fn new(
        core: &'a NetCore,
        peer: &'a PeerHandle,
        state: &'a mut PeerState,
        chain: &'a dyn ChainAccess,
        snap: &'a ChainSnapshot,
    ) -> Self {
        Self {
            core,
            peer,
            state,
            chain,
            snap,
        }
    }

    /// Core's `Misbehaving(peer, howmuch, msg)`.
    pub fn misbehaving(&mut self, howmuch: i32, msg: &str) {
        misbehavior::misbehaving(self.state.id, &self.state.misbehavior, howmuch, msg);
    }

    /// Core's `MaybePunishNodeForBlock` for this peer. Returns whether it punished.
    pub fn maybe_punish_node_for_block(
        &mut self,
        result: BlockValidationResult,
        via_compact_block: bool,
        msg: &str,
    ) -> bool {
        let inbound = self.peer.is_inbound();
        misbehavior::maybe_punish_node_for_block(
            self.state,
            result,
            inbound,
            via_compact_block,
            msg,
        )
    }
}

/// What [`BlockSync::block_checked`] reaches: any peer by id (Core's `mapBlockSource` names the
/// source, which may be gone) and, through it, the chain. Core's two branches act on the source
/// alone, so the chain is reached only through a live peer's [`SyncCx`].
pub struct BlockCheckedCx<'a> {
    pub(super) core: &'a NetCore,
    pub(super) chain: &'a dyn ChainAccess,
    pub(super) peers: &'a PeerMap,
    /// The snapshot of the drain delivering the signal (P3 §3.6, D47).
    pub(super) snap: &'a ChainSnapshot,
}

impl BlockCheckedCx<'_> {
    /// Runs `f` on `id`'s [`SyncCx`] under its state lock, at the drain's snapshot; `None` when
    /// the peer is gone. Core's test is `State(nodeid)` non-null, which holds until
    /// `FinalizeNode`; here the peer must also still be in SB6's table, which it leaves just
    /// before (a punishment then would act on no `SendMessages` anyway). The header cell is not
    /// held: `BlockChecked`'s branches read none of its fields.
    pub fn with_peer<R>(&self, id: PeerId, f: impl FnOnce(&mut SyncCx<'_>) -> R) -> Option<R> {
        let peer = self.core.peer(id)?;
        let state = self.peers.lock().get(&id).map(|s| Arc::clone(&s.state))?;
        let mut st = state.lock();
        Some(f(&mut SyncCx::new(self.core, &peer, &mut st, self.chain, self.snap)))
    }

    /// Core's `State(id)` as the HB list reads it: `(m_provides_cmpctblocks, m_is_inbound)`
    /// while `id` has a processor state, which lasts until `FinalizeNode`, even after the peer
    /// has left the connection table (P3 §3.5.2).
    pub fn node_state(&self, id: PeerId) -> Option<(bool, bool)> {
        let state = self.peers.lock().get(&id).map(|s| Arc::clone(&s.state))?;
        let st = state.lock();
        Some((st.provides_cmpctblocks, st.inbound))
    }

    /// Core's two-argument `ForNode(id, f)`: `f` on the peer when it is fully connected
    /// (`FullyConnectedOnly`, net.cpp:4852-4855), else nothing. Returns whether `f` ran.
    pub fn for_node(&self, id: PeerId, f: impl FnOnce(&NetCore, &PeerHandle)) -> bool {
        let peer = self.core.peer(id).filter(fully_connected);
        if let Some(p) = &peer {
            f(self.core, p);
        }
        peer.is_some()
    }
}

/// P3's block download at Core's `SendMessages` positions (P2a §3.1.4 sections 7, 9 and 12), the
/// `headers` and `inv` paths, and `BlockChecked`. M2 ships [`NoBlockSync`]. The per-peer hooks get
/// the peer's [`SyncCx`] (P2a §2.4); the contract P3 implements against is D65.
pub trait BlockSync: Send {
    /// `IsBlockRequested(hash)`.
    fn is_block_requested(&self, hash: &BlockHash) -> bool;
    /// `mapBlocksInFlight.empty()` is false.
    fn any_in_flight(&self) -> bool;
    /// The peer's blocks in flight.
    fn blocks_in_flight(&self, peer: PeerId) -> usize;
    /// Section 9: stalling and the block-download timeout. `true` when it disconnected the peer
    /// (Core returns from `SendMessages`).
    fn stall_section(&mut self, cx: &mut SyncCx<'_>, now_us: i64) -> bool;
    /// Section 12: `getdata` for blocks, appended to the pass's one batch. `sync_from_peer` is
    /// section 6's `sync_blocks_and_headers_from_peer`.
    fn getdata_section(
        &mut self,
        cx: &mut SyncCx<'_>,
        batch: &mut GetDataBatch<'_>,
        sync_from_peer: bool,
        now_us: i64,
    );
    /// `HeadersDirectFetchBlocks` after a `headers` message ending in `last`.
    fn headers_direct_fetch(&mut self, cx: &mut SyncCx<'_>, last: BlockId) {
        let _ = (cx, last);
    }
    /// Section 7's high-bandwidth announcement: one `cmpctblock` for `block`. Section 7 has
    /// logged it and set `best_header_sent` already; the hook only pushes, from `recent` (the
    /// head's copy of `m_most_recent_*`) when it names `block`, else encoded afresh from
    /// [`ChainAccess::read_block`] (np:6481-6494, P3 §3.6).
    fn push_compact_announcement(
        &mut self,
        cx: &mut SyncCx<'_>,
        block: &EntryFacts,
        recent: &RecentBlock,
    ) {
        let _ = (cx, block, recent);
    }
    /// Core's `BlockChecked`: `invalid` is the failed state, `None` when the block was valid.
    /// Core punishes `mapBlockSource`'s peer through `MaybePunishNodeForBlock` and erases the
    /// entry (D65). `ibd` is the chain side's `IsInitialBlockDownload()` at the signal, for the
    /// valid branch's `MaybeSetPeerAsAnnouncingHeaderAndIDs` test: use it, never the published
    /// view, which may already have moved on (CH1-node's V-evaluated argument).
    ///
    /// The actor applies the signal while it waits on V's `ProcessNewBlock` reply, and drains
    /// once more after the reply, so the hook runs before the handler's `ProcessBlock` tail
    /// (np:3717-3732) as Core's runs inside `ProcessNewBlock`, and the source is still there to
    /// punish. Erase it here, as Core does (np:2342; P3 D1).
    // Core: net_processing.cpp:2314-2343 PeerManagerImpl::BlockChecked
    fn block_checked(
        &mut self,
        cx: &mut BlockCheckedCx<'_>,
        hash: &BlockHash,
        invalid: Option<&BlockValidationError>,
        ibd: bool,
    ) {
        let _ = (cx, hash, invalid, ibd);
    }
    /// `FinalizeNode`'s block-download cleanup.
    fn finalize(&mut self, peer: PeerId);
    /// `RemoveBlockRequest(hash, from_peer)` at `now_us` (P3's `block` handler and tail): the
    /// node clock read at the call, as Core's `GetTime<µs>()` inside it is (D86).
    fn remove_block_request(&mut self, hash: &BlockHash, from: Option<PeerId>, now_us: i64) {
        let _ = (hash, from, now_us);
    }
    /// `mapBlockSource.emplace(hash, {peer, full})`.
    fn emplace_block_source(&mut self, hash: BlockHash, peer: PeerId, full: bool) {
        let _ = (hash, peer, full);
    }
    /// `mapBlockSource.erase(hash)` (the `ProcessBlock` tail of a block that is not new).
    fn erase_block_source(&mut self, hash: &BlockHash) {
        let _ = hash;
    }
    /// Hands over the counter DEC-230's guard reads: the implementation keeps it equal to
    /// `mapBlockSource.size()` (P3 §2.5). The message loop calls it once, at construction.
    fn bind_source_counter(&mut self, counter: Arc<AtomicUsize>) {
        let _ = counter;
    }
    /// Hands over `m_block_stalling_timeout`, which
    /// [`Shared::block_connected`](super::peerman::Shared::block_connected) decays on the
    /// signals thread (P3 §3.3.3, D78). The message loop calls it once, at construction.
    fn bind_stalling_timeout(&mut self, timeout: StallingTimeout) {
        let _ = timeout;
    }
    /// Hands over `m_ignore_incoming_txs` (`-blocksonly`), which direct fetch's
    /// `MSG_CMPCT_BLOCK` swap reads (np:3248). The message loop calls it once, at construction.
    fn bind_ignore_incoming_txs(&mut self, ignore: bool) {
        let _ = ignore;
    }
    /// The block-download state the `cmpctblock` and `blocktxn` handlers work on (P3 §3.5.3,
    /// §3.5.4), which read and change one request's `partialBlock` as Core's do under
    /// `cs_main`. `None` keeps M2's handling: `cmpctblock` ignored, `blocktxn` unexpected.
    fn download(&mut self) -> Option<&mut BlockDownload> {
        None
    }
}

/// Core's `MAX_GETDATA_SZ`.
pub const MAX_GETDATA_SZ: usize = 1000;

/// Core's `vGetData` in `SendMessages`: sections 12 (blocks) and 13 (objects) append to one
/// batch, which is pushed whenever it reaches [`MAX_GETDATA_SZ`] and once at the end, so a pass
/// sends one `getdata` where Core does.
pub struct GetDataBatch<'a> {
    core: &'a NetCore,
    peer: &'a PeerHandle,
    invs: Vec<Inventory>,
}

impl<'a> GetDataBatch<'a> {
    /// An empty batch for `peer`.
    pub fn new(core: &'a NetCore, peer: &'a PeerHandle) -> Self {
        Self {
            core,
            peer,
            invs: Vec::new(),
        }
    }

    /// The network core.
    pub fn core(&self) -> &'a NetCore {
        self.core
    }

    /// The peer the batch goes to.
    pub fn peer(&self) -> &'a PeerHandle {
        self.peer
    }

    /// Section 12's `vGetData.emplace_back` (blocks never fill a batch: at most 16 in flight).
    pub fn push_block(&mut self, inv: Inventory) {
        self.invs.push(inv);
    }

    /// Section 13's append, which pushes the batch when it reaches [`MAX_GETDATA_SZ`].
    // Core: net_processing.cpp:6870-6875
    pub fn push(&mut self, inv: Inventory) {
        self.invs.push(inv);
        if self.invs.len() >= MAX_GETDATA_SZ {
            self.send();
        }
    }

    fn send(&mut self) {
        let inventory = std::mem::take(&mut self.invs);
        self.core.push(
            self.peer,
            out_msg(
                CommandString::GETDATA,
                &GetData {
                    inventory,
                },
            ),
        );
    }

    /// The final push, if anything is left.
    // Core: net_processing.cpp:6894-6897
    pub fn finish(mut self) {
        if !self.invs.is_empty() {
            let n = self.invs.len();
            self.send();
            tracing::debug!(target: "net", "SendMessages -- GETDATA -- pushed size = {n} peer={}", self.peer.id());
        }
    }
}

/// The M2 block download: nothing requested, nothing in flight.
#[derive(Debug, Default)]
pub struct NoBlockSync;

impl BlockSync for NoBlockSync {
    fn is_block_requested(&self, _hash: &BlockHash) -> bool {
        false
    }

    fn any_in_flight(&self) -> bool {
        false
    }

    fn blocks_in_flight(&self, _peer: PeerId) -> usize {
        0
    }

    fn stall_section(&mut self, _cx: &mut SyncCx<'_>, _now_us: i64) -> bool {
        false
    }

    fn getdata_section(
        &mut self,
        _cx: &mut SyncCx<'_>,
        _batch: &mut GetDataBatch<'_>,
        _sync: bool,
        _now_us: i64,
    ) {
    }

    fn finalize(&mut self, _peer: PeerId) {}
}

/// Core's `vInv` in `SendMessages` section 8: block announcements first, then P5's trickle, each
/// pushed whenever the batch reaches [`MAX_INV_SZ`](super::logic::MAX_INV_SZ), and the rest once.
pub struct InvBatch<'a> {
    core: &'a NetCore,
    peer: &'a PeerHandle,
    invs: Vec<Inventory>,
}

impl<'a> InvBatch<'a> {
    /// An empty batch for `peer`.
    pub fn new(core: &'a NetCore, peer: &'a PeerHandle) -> Self {
        Self {
            core,
            peer,
            invs: Vec::new(),
        }
    }

    /// The peer the batch goes to.
    pub fn peer(&self) -> &'a PeerHandle {
        self.peer
    }

    /// The block loop's append.
    // Core: net_processing.cpp:6571-6577
    pub fn push_block(&mut self, inv: Inventory) {
        self.invs.push(inv);
        if self.invs.len() == super::logic::MAX_INV_SZ {
            self.send();
        }
    }

    /// `queueAndMaybePushInv`, P5's append.
    // Core: net_processing.cpp:6581-6590
    pub fn push(&mut self, inv: Inventory) {
        let id = self.peer.id();
        tracing::debug!(target: "net", "SendMessages -- queued inv: {inv}  index={} peer={id}", self.invs.len());
        self.invs.push(inv);
        if self.invs.len() == super::logic::MAX_INV_SZ {
            tracing::debug!(target: "net", "SendMessages -- pushing invs: count={} peer={id}", self.invs.len());
            self.send();
        }
    }

    fn send(&mut self) {
        let inventory = std::mem::take(&mut self.invs);
        self.core.push(
            self.peer,
            out_msg(
                CommandString::INV,
                &Inv {
                    inventory,
                },
            ),
        );
    }

    /// The final push, if anything is left.
    // Core: net_processing.cpp:6749-6750
    pub fn finish(mut self) {
        if !self.invs.is_empty() {
            self.send();
        }
    }
}

/// P5's object requests and relay at Core's positions, each after P5a's tracker call there (P5a
/// D8): the `inv` branch's announcements, `notfound` completions (`np:5756-5764`), `SendMessages`
/// section 8 (the trickle after the block invs) and section 13 (`getdata` for objects). M2 ships
/// [`NoObjectRequests`].
pub trait ObjectRequests: Send {
    /// At `AddObjectAnnouncement`'s place for an entry not already had, outside IBD (or a spork
    /// in IBD) (`np:4574-4577`), after the tracker's call: P5's transaction producers.
    fn add_object_announcement(&mut self, peer: PeerId, inv: &Inventory, now_us: i64) {
        let _ = (peer, inv, now_us);
    }
    /// `m_object_request.ReceivedResponse(peer, inv)` for a known-type `notfound` entry.
    /// Runs under the tracker's lock (P5a D24): it must not take the peer state, the peer map or a
    /// relay cell.
    fn received_response(&mut self, peer: PeerId, inv: &Inventory);
    /// Section 8 after the block invs: the transaction and object trickle.
    fn inv_section(&mut self, batch: &mut InvBatch<'_>, now_us: i64) {
        let _ = (batch, now_us);
    }
    /// Section 13: `getdata` for objects, appended to the batch section 12 started.
    fn send_section(&mut self, batch: &mut GetDataBatch<'_>, now_us: i64);
    /// `FinalizeNode`'s `DisconnectedPeer`.
    fn finalize(&mut self, peer: PeerId);
}

/// The M2 object requests: nothing requested.
#[derive(Debug, Default)]
pub struct NoObjectRequests;

impl ObjectRequests for NoObjectRequests {
    fn received_response(&mut self, _peer: PeerId, _inv: &Inventory) {}

    fn send_section(&mut self, _batch: &mut GetDataBatch<'_>, _now_us: i64) {}

    fn finalize(&mut self, _peer: PeerId) {}
}

/// P2a-net's address relay at Core's positions: `RelayAddress` after SB6's `addr` intake,
/// `MaybeSendAddr` (`SendMessages` section 5) and the inbound `getaddr` reply. M2 ships
/// [`NoAddressRelay`].
///
/// `relay` runs while the actor holds the source peer's state; it may lock any other peer's
/// state in `peers` (Core's `RelayAddress` never picks the source). While it holds the map or a
/// state it calls nothing outside the processor: no [`ChainAccess`] read and no stats (D66).
pub trait AddressRelay: Send {
    /// `RelayAddress` for each address SB6's intake returned, in Core's order.
    fn relay(&mut self, core: &NetCore, peers: &PeerMap, from: PeerId, addrs: &[(Address, bool)]);
    /// Section 5: `MaybeSendAddr`. `our_services` is the peer's `m_our_services`, which the
    /// self-announcement carries (np:6205).
    fn send_section(
        &mut self,
        core: &NetCore,
        peer: &PeerHandle,
        relay: &mut AddrRelay,
        our_services: ServiceFlags,
        now_us: i64,
    );
    /// The inbound `getaddr` branch after the outbound check (`np:5400-5424`).
    fn on_getaddr(&mut self, cx: &mut HandlerCx<'_>);
}

/// Core's `m_isman.GetInstantSendLockByTxid`, for the locks that follow a filtered block's
/// matched transactions (np:2849-2854, P4b D9). LK1b's lock store installs it.
pub trait IsLockLookup: Send {
    /// The `isdlock` payload of the lock on `txid`, if one is known.
    fn islock_by_txid(&self, txid: &TxHash) -> Option<Vec<u8>>;
}

/// No InstantSend lock store before LK1b: no lock follows a filtered block.
#[derive(Debug, Default)]
pub struct NoIsLocks;

impl IsLockLookup for NoIsLocks {
    fn islock_by_txid(&self, _: &TxHash) -> Option<Vec<u8>> {
        None
    }
}

/// The M2 address relay before P2a-net: nothing relayed, sent or answered.
#[derive(Debug, Default)]
pub struct NoAddressRelay;

impl AddressRelay for NoAddressRelay {
    fn relay(
        &mut self,
        _core: &NetCore,
        _peers: &PeerMap,
        _from: PeerId,
        _addrs: &[(Address, bool)],
    ) {
    }

    fn send_section(
        &mut self,
        _core: &NetCore,
        _peer: &PeerHandle,
        _relay: &mut AddrRelay,
        _our_services: ServiceFlags,
        _now_us: i64,
    ) {
    }

    fn on_getaddr(&mut self, _cx: &mut HandlerCx<'_>) {}
}

/// The Core state `AlreadyHave` reads for the overlay types whose managers are not ported yet
/// (P5a D20): whether governance takes invs (the governance manager and SP1's sync) and
/// InstantSend's spork (LK1b). [`StaticOverlayState`]'s default is a node with neither: no
/// governance, no sync tick and no sporks.
pub trait OverlayState: Send {
    /// `CGovernanceManager::IsValid() && CMasternodeSync::IsBlockchainSynced()`: governance invs
    /// are had until it holds. It is not plain `IsBlockchainSynced()`: with governance disabled
    /// (rdashd's default until GV1b) it is always false.
    // Core: governance/net_governance.cpp:273-282; governance/governance.cpp:612-613
    fn governance_ready(&self) -> bool;
    /// `CInstantSendManager::IsInstantSendEnabled()`: `isdlock` invs are had until it holds.
    // Core: instantsend/instantsend.cpp:354-356, :510-513
    fn instantsend_enabled(&self) -> bool;
}

/// Fixed answers. The default is Core before its sync tick, with
/// `SPORK_2_INSTANTSEND_ENABLED` at its test-chain default (off).
#[derive(Clone, Copy, Debug, Default)]
pub struct StaticOverlayState {
    /// [`OverlayState::governance_ready`].
    pub governance_ready: bool,
    /// [`OverlayState::instantsend_enabled`].
    pub instantsend_enabled: bool,
}

impl OverlayState for StaticOverlayState {
    fn governance_ready(&self) -> bool {
        self.governance_ready
    }

    fn instantsend_enabled(&self) -> bool {
        self.instantsend_enabled
    }
}

/// LK1a's ChainLocks at `net_processing`'s positions (P5a D4): Core's `m_chainlocks` and
/// `m_clhandler` as the processor asks them. The processor does the `clsig` branch's spork gate,
/// read, hash and solicitation check, then applies [`ChainLocks::process_new_chainlock`]'s
/// result as `PostProcessMessage` does.
pub trait ChainLocks: Send {
    /// `Chainlocks::IsEnabled()`: `SPORK_19_CHAINLOCKS_ENABLED` is active.
    fn is_enabled(&self) -> bool;
    /// `ChainlockHandler::AlreadyHave` for a `MSG_CLSIG` hash.
    fn already_have(&self, hash: &[u8; 32]) -> bool;
    /// `ChainlockHandler::ProcessNewChainLock(from, clsig, qman, hash)`: the relay decision.
    fn process_new_chainlock(
        &mut self,
        from: PeerId,
        clsig: &ClSig,
        hash: &[u8; 32],
    ) -> MessageProcessingResult;
    /// `Chainlocks::GetChainLockByHash`, for `getdata`.
    fn chainlock_by_hash(&self, hash: &[u8; 32]) -> Option<ClSig>;
    /// `Chainlocks::GetBestChainLock`, for P5's `mempool` reply; `None` is Core's null lock.
    fn best_chainlock(&self) -> Option<ClSig>;
}

/// ChainLocks never enabled (P5a D5): Core with `SPORK_19` off. A `clsig` is not read, nothing is
/// had or served.
#[derive(Debug, Default)]
pub struct NoChainLocks;

impl ChainLocks for NoChainLocks {
    fn is_enabled(&self) -> bool {
        false
    }

    fn already_have(&self, _hash: &[u8; 32]) -> bool {
        false
    }

    fn process_new_chainlock(
        &mut self,
        _from: PeerId,
        _clsig: &ClSig,
        _hash: &[u8; 32],
    ) -> MessageProcessingResult {
        MessageProcessingResult::default()
    }

    fn chainlock_by_hash(&self, _hash: &[u8; 32]) -> Option<ClSig> {
        None
    }

    fn best_chainlock(&self) -> Option<ClSig> {
        None
    }
}

/// Core's `SEEN_CHAINLOCKS_MAX_SIZE`: the seen map's size that triggers a trim.
// Core: chainlock/handler.h:57-58
pub const SEEN_CHAINLOCKS_MAX_SIZE: usize = 2 * SEEN_CHAINLOCKS_CUTOFF_SIZE;
/// Core's `SEEN_CHAINLOCKS_CUTOFF_SIZE`: what a trim keeps.
pub const SEEN_CHAINLOCKS_CUTOFF_SIZE: usize = 1024;

/// The stand-in until LK1a (P5a D5): what `ProcessNewChainLock` does before verification and no
/// more. `is_enabled` reads `SPORK_19` live through `enabled`; each new hash goes into the seen
/// map (handler.cpp:138-143), so `AlreadyHave` turns true and a repeat costs nothing; the result
/// is always empty: no relay, no score. Nothing is accepted, so nothing is served.
pub struct SeenChainLocks {
    enabled: Box<dyn Fn() -> bool + Send>,
    seen: HashSet<[u8; 32]>,
    /// The seen hashes, oldest first.
    order: VecDeque<[u8; 32]>,
}

impl SeenChainLocks {
    /// A stand-in whose `IsEnabled` is `enabled()`.
    pub fn new(enabled: impl Fn() -> bool + Send + 'static) -> Self {
        Self {
            enabled: Box::new(enabled),
            seen: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// How many hashes the seen map holds.
    pub fn seen_len(&self) -> usize {
        self.seen.len()
    }
}

impl std::fmt::Debug for SeenChainLocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeenChainLocks").field("seen", &self.seen.len()).finish_non_exhaustive()
    }
}

impl ChainLocks for SeenChainLocks {
    fn is_enabled(&self) -> bool {
        (self.enabled)()
    }

    // Core: chainlock/handler.cpp:76-87 (nothing accepted: `GetChainLockByHash` finds nothing)
    fn already_have(&self, hash: &[u8; 32]) -> bool {
        self.seen.contains(hash)
    }

    // Core: chainlock/handler.cpp:138-143; the rest needs `VerifyChainLock` (LK1a)
    fn process_new_chainlock(
        &mut self,
        _from: PeerId,
        _clsig: &ClSig,
        hash: &[u8; 32],
    ) -> MessageProcessingResult {
        if self.seen.insert(*hash) {
            self.order.push_back(*hash);
            if self.seen.len() > SEEN_CHAINLOCKS_MAX_SIZE {
                while self.seen.len() > SEEN_CHAINLOCKS_CUTOFF_SIZE {
                    let old = self.order.pop_front().expect("order mirrors seen");
                    self.seen.remove(&old);
                }
            }
        }
        MessageProcessingResult::default()
    }

    fn chainlock_by_hash(&self, _hash: &[u8; 32]) -> Option<ClSig> {
        None
    }

    fn best_chainlock(&self) -> Option<ClSig> {
        None
    }
}
