//! The processor's seams to the chain and to later tasks (P2a §2.4), each with its M2 default.
//!
//! [`ChainAccess`] is the read side of SB4's `dash_chain::ChainReader` plus CH1-node's
//! `ChainCommand::ProcessHeaders` (CH1-node §2.2, §6.3). The shapes here mirror theirs (D-row
//! P2a-core D3) because `dash-chain`'s snapshot, `HeaderStop` result and `PeerId` newtype don't
//! fit `sim`; P2a-node moves to the real types (D59).

use std::any::Any;
use std::cmp::Ordering as CmpOrdering;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use dash_clock::{Clock, NodeSeconds};
use dash_coinjoin::dstx::{DstxScore, DstxValidation};
use dash_coinjoin::relay::TxInvKind;
use dash_consensus::primitives::TxHash;
use dash_consensus::{
    Amount, Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, Header, TxRef,
    TxValidationResult,
};
use dash_p2p::addr::AddrRelay;
use dash_p2p::{Address, CommandString, NetCore, PeerHandle, PeerId, Service, ServiceFlags};
use dash_p2p_core::{ClSig, DsTx, GetData, Inv, Inventory, MnAuth, ProtocolVersion};
use dash_primitives::{BlockHash, BlockHeader};

use super::chainlock_seen::SeenChainLocksMap;
pub use super::chainlock_seen::{
    CLEANUP_SEEN_TIMEOUT, SEEN_CHAINLOCKS_CUTOFF_SIZE, SEEN_CHAINLOCKS_MAX_SIZE,
    SeenChainLocksCleanup,
};
use super::compact::CompactTxSource;
use super::download::{BlockDownload, PeerFacts, StallingTimeout};
use super::eviction::fully_connected;
use super::logic::{PeerMap, out_msg};
use super::misbehavior;
use super::peer::PeerState;
use super::peerman::RecentBlock;
use super::registry::{HandlerCx, MessageProcessingResult};

/// The basic-filter index read side. Ranges follow the stop block's ancestry, including
/// retained side-chain entries. A failed lookup returns no partial range.
pub trait BlockFilterIndex: Send + Sync {
    /// Whether a basic index exists, independently of its synchronization progress.
    fn available(&self) -> bool {
        true
    }
    /// One block's encoded basic filter.
    fn filter(&self, id: BlockId) -> Option<Vec<u8>>;
    /// One block's filter header.
    fn filter_header(&self, id: BlockId) -> Option<dash_num::Hash256>;
    /// Encoded filters, with their block hashes, from `start` through `stop`, inclusive.
    fn filters(&self, start: u32, stop: BlockId) -> Option<Vec<(BlockHash, Vec<u8>)>>;
    /// Filter hashes from `start` through `stop`, inclusive.
    fn filter_hashes(&self, start: u32, stop: BlockId) -> Option<Vec<dash_num::Hash256>>;
    /// The highest indexed active-chain height, or none before indexing genesis.
    fn best_height(&self) -> Option<u32>;
}

/// No basic-filter index is configured.
#[derive(Debug, Default)]
pub struct NoBlockFilterIndex;

impl BlockFilterIndex for NoBlockFilterIndex {
    fn available(&self) -> bool {
        false
    }
    fn filter(&self, _id: BlockId) -> Option<Vec<u8>> {
        None
    }
    fn filter_header(&self, _id: BlockId) -> Option<dash_num::Hash256> {
        None
    }
    fn filters(&self, _start: u32, _stop: BlockId) -> Option<Vec<(BlockHash, Vec<u8>)>> {
        None
    }
    fn filter_hashes(&self, _start: u32, _stop: BlockId) -> Option<Vec<dash_num::Hash256>> {
        None
    }
    fn best_height(&self) -> Option<u32> {
        None
    }
}

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
    /// `GETBLOCKTXN`'s shallow read (np:4722-4733, :4737), one step: Core tests the entry's
    /// `BLOCK_HAVE_DATA` and takes its position under `cs_main` (np:4722-4733), then reads there
    /// after releasing it (np:4737). This reads the live entry, not a snapshot's:
    /// [`LiveRead::NoData`] without `HAVE_DATA`, with no read; [`LiveRead::Same`] when its
    /// position is still `failed_at`, the position of the caller's last failed read, with no
    /// read; else the block read at its position, or the failure with that position. A failed
    /// read carries its `ReadBlockFromDisk:` line for the caller; an open failure also logs
    /// Core's `Unable to open file` INFO line from the read (P4b D16, CH3b D244).
    fn read_live(&self, id: BlockId, failed_at: Option<BlockPos>) -> LiveRead;
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

/// [`ChainAccess::read_live`]'s answer.
#[derive(Clone)]
pub enum LiveRead {
    /// The live entry has no `BLOCK_HAVE_DATA`; nothing was read.
    NoData,
    /// The live entry's data position is the caller's `failed_at`; nothing was read again.
    Same,
    /// The block, read at the live entry's position.
    Block(Arc<Block>),
    /// The read at `pos`, the live entry's position, failed. `error` is the read's own
    /// `ReadBlockFromDisk:` line for the caller; an open failure also logs Core's `Unable to open
    /// file` INFO line from the read. `None` is used for a block with another hash there, which is
    /// only the caller's (np:2820).
    Failed {
        pos: BlockPos,
        error: Option<String>,
    },
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
    /// `BlockRequested(peer, block, nullptr)` at `now_us`, the node clock read at the call (D86):
    /// `false` when `peer` already has it in flight. `FetchBlock` calls it (BL-503). M2's
    /// default records nothing, so under it a fetched block arrives unrequested (not forced).
    // Core: net_processing.cpp:1324-1358 PeerManagerImpl::BlockRequested
    fn block_requested(&mut self, peer: PeerFacts<'_>, block: &EntryFacts, now_us: i64) -> bool {
        let _ = (peer, block, now_us);
        true
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
        self.core.push(self.peer, out_msg(CommandString::GETDATA, &GetData::new(inventory)));
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
        self.core.push(self.peer, out_msg(CommandString::INV, &Inv::new(inventory)));
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

/// A transaction and its mempool admission time, in node-clock seconds.
#[derive(Clone)]
pub struct TxInfo {
    /// The immutable, hash-cached transaction (`CTransactionRef`).
    pub tx: TxRef,
    /// The time this entry entered the pool; preserved when an unlock instance is replaced.
    pub time_s: i64,
}

impl std::fmt::Debug for TxInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxInfo")
            .field("txid", &self.tx.txid())
            .field("time_s", &self.time_s)
            .finish()
    }
}

/// The admission outcome in Core's `MempoolAcceptResult`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptResultType {
    /// Fully validated and accepted.
    Valid,
    /// Rejected by validation or policy.
    Invalid,
    /// Valid and already in the mempool.
    MempoolEntry,
}

/// The completed admission decision for one transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptResult {
    /// Whether admission succeeded, failed, or found an existing entry.
    pub result_type: AcceptResultType,
    /// The validation result; `Unset` unless rejected, including `MissingInputs`.
    pub validation_result: TxValidationResult,
    /// The validation state's reject reason.
    pub reason: String,
    /// Additional validation detail used with the reject reason in logs.
    pub debug_message: String,
    /// The mempool size including sigop adjustment; present for valid or existing entries.
    pub vsize: Option<i64>,
    /// The original fee in satoshis; present for valid or existing entries.
    pub base_fee: Option<Amount>,
}

/// The admission reply the P2P actor awaits before processing its next transaction.
pub struct AcceptWait(pub Pin<Box<dyn Future<Output = AcceptResult> + Send>>);

impl AcceptWait {
    /// An admission decision already available.
    pub fn ready(result: AcceptResult) -> Self {
        Self(Box::pin(std::future::ready(result)))
    }
}

impl Future for AcceptWait {
    type Output = AcceptResult;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.0.as_mut().poll(cx)
    }
}

impl std::fmt::Debug for AcceptWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AcceptWait")
    }
}

/// The mempool operations used by transaction relay and compact-block reconstruction.
///
/// Hash arguments are stable txids unless explicitly named `instance_hash`. Each read is
/// coherent within that call; callers must allow an entry to disappear between calls.
/// Methods may take only mempool-internal locks, never the peer map, peer state or a relay
/// cell. Admission publishes mutations and TransactionAdded/Removed events before its reply
/// resolves. `from = None` denotes a transaction without a source peer, including block-enabled
/// orphan retries.
pub trait TxPool: CompactTxSource + Send + Sync {
    /// Submit one immutable transaction and await its validation/policy decision in submission order.
    /// Missing inputs return `Invalid` with `MissingInputs`; callers derive parents from all inputs.
    fn accept(&self, tx: TxRef, from: Option<PeerId>) -> AcceptWait;
    /// Find an entry by stable txid (Core v24.0.0 `src/txmempool.cpp:1572-1579`, `info`).
    fn info(&self, txid: &TxHash) -> Option<TxInfo>;
    /// Resolve only a version-2+ asset unlock's current instance hash to its stable txid
    /// (Core v24.0.0 `src/txmempool.cpp:852-858`, `GetAssetUnlockByInstanceHash`).
    fn asset_unlock_by_instance_hash(&self, instance_hash: &TxHash) -> Option<TxHash>;
    /// Test stable-txid membership (Core v24.0.0 `src/txmempool.h:720-724`, `exists`).
    fn exists(&self, txid: &TxHash) -> bool;
    /// Direct in-pool parents whose admission time is strictly greater than `since_s`,
    /// in `GetMemPoolParentsConst` order (ascending txid bytes); empty if `txid` is absent.
    /// Core v24.0.0 `src/net_processing.cpp:2996-3010`, `ProcessGetData`.
    fn recent_parents(&self, txid: &TxHash, since_s: i64) -> Vec<TxHash>;
    /// All entries in ascending ancestor-count order, then descending fee/virtual-size, then
    /// descending txid bytes. Score compares fee*virtual-size products in double precision,
    /// using the original fee, never a prioritization delta.
    /// Core v24.0.0 `src/txmempool.cpp:1515-1561`, `infoAll`.
    fn info_all(&self) -> Vec<TxInfo>;
    /// `Less` means `a` is considered sooner: absent before present, then fewer ancestors,
    /// then higher original fee/virtual-size using double-precision fee*virtual-size products, then descending
    /// txid bytes; two absent txids compare equal (not an exact rational comparison).
    /// Core v24.0.0 `src/txmempool.cpp:1495-1513`, `src/txmempool.h:131-143`.
    fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> CmpOrdering;
    /// The unbroadcast stable txids, in ascending txid-byte order.
    /// Core v24.0.0 `src/txmempool.h:785-790`, `GetUnbroadcastTxs`.
    fn unbroadcast(&self) -> Vec<TxHash>;
    /// Remove a stable txid from the unbroadcast set; harmless if it is absent.
    /// `unchecked` marks removal before confirmed relay in Core's log.
    fn remove_unbroadcast(&self, txid: &TxHash, unchecked: bool);
    /// The number of entries currently in the pool.
    fn size(&self) -> usize;
    /// The pool's dynamic memory usage in bytes, under a short pool read lock.
    /// Callers must not hold relay locks while calling this method.
    fn dynamic_memory_usage(&self) -> usize;
}

/// No mempool installed: no transaction is stored or served.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTxPool;

impl CompactTxSource for NoTxPool {
    fn for_each_in_vtxhashes_order(&self, _: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {}
}

impl TxPool for NoTxPool {
    fn accept(&self, _: TxRef, _: Option<PeerId>) -> AcceptWait {
        AcceptWait::ready(AcceptResult {
            result_type: AcceptResultType::Invalid,
            validation_result: TxValidationResult::NoMempool,
            reason: "no-mempool".into(),
            debug_message: String::new(),
            vsize: None,
            base_fee: None,
        })
    }

    fn info(&self, _: &TxHash) -> Option<TxInfo> {
        None
    }
    fn asset_unlock_by_instance_hash(&self, _: &TxHash) -> Option<TxHash> {
        None
    }
    fn exists(&self, _: &TxHash) -> bool {
        false
    }
    fn recent_parents(&self, _: &TxHash, _: i64) -> Vec<TxHash> {
        Vec::new()
    }
    fn info_all(&self) -> Vec<TxInfo> {
        Vec::new()
    }
    fn compare_depth_and_score(&self, _: &TxHash, _: &TxHash) -> CmpOrdering {
        CmpOrdering::Equal
    }
    fn unbroadcast(&self) -> Vec<TxHash> {
        Vec::new()
    }
    fn remove_unbroadcast(&self, _: &TxHash, _: bool) {}
    fn size(&self) -> usize {
        0
    }
    fn dynamic_memory_usage(&self) -> usize {
        0
    }
}

/// Removes orphans included in or conflicting with a connected block.
pub trait OrphanageHooks: Send {
    /// Erases matching transactions before the confirmed filter records the block.
    fn erase_for_block(&mut self, block: &Block);
}

/// Leaves orphan storage untouched when none is installed.
#[derive(Debug, Default)]
pub struct NoOrphanageHooks;

impl OrphanageHooks for NoOrphanageHooks {
    fn erase_for_block(&mut self, _: &Block) {}
}

/// Serialized CoinJoin objects available to the P2P serving and announcement paths.
pub trait CoinJoinObjects: Send + Sync {
    /// A serialized DSTX by stable transaction hash.
    fn dstx_by_hash(&self, hash: &TxHash) -> Option<Vec<u8>>;
    /// A serialized DSQ by inventory hash.
    fn queue_by_hash(&self, hash: &[u8; 32]) -> Option<Vec<u8>>;
}

/// No CoinJoin object stores installed.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCoinJoinObjects;

impl CoinJoinObjects for NoCoinJoinObjects {
    fn dstx_by_hash(&self, _: &TxHash) -> Option<Vec<u8>> {
        None
    }
    fn queue_by_hash(&self, _: &[u8; 32]) -> Option<Vec<u8>> {
        None
    }
}

/// Serialized platform-ban objects available to the P2P serving path.
pub trait PlatformBans: Send + Sync {
    /// A serialized `platformban` payload by inventory hash.
    fn get(&self, hash: &[u8; 32]) -> Option<Vec<u8>>;
}

/// No platform-ban store is installed before G7.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPlatformBans;

impl PlatformBans for NoPlatformBans {
    fn get(&self, _: &[u8; 32]) -> Option<Vec<u8>> {
        None
    }
}

/// Core's `m_isman.GetInstantSendLockByTxid`, for the locks that follow a filtered block's
/// matched transactions (np:2849-2854, P4b D9). LK1b's lock store installs it.
pub trait IsLockLookup: Send {
    /// The `isdlock` payload of the lock on `txid`, if one is known.
    fn islock_by_txid(&self, txid: &TxHash) -> Option<Vec<u8>>;
    /// The inventory hash of the lock on a stable txid, if known.
    fn islock_hash_by_txid(&self, txid: &TxHash) -> Option<[u8; 32]>;
    /// The `isdlock` payload identified by its inventory hash, if known.
    fn islock_by_hash(&self, hash: &[u8; 32]) -> Option<Vec<u8>>;
}

/// No InstantSend lock store before LK1b: no lock follows a filtered block.
#[derive(Debug, Default)]
pub struct NoIsLocks;

impl IsLockLookup for NoIsLocks {
    fn islock_hash_by_txid(&self, _: &TxHash) -> Option<[u8; 32]> {
        None
    }
    fn islock_by_hash(&self, _: &[u8; 32]) -> Option<Vec<u8>> {
        None
    }

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

/// Carries the BLS context sampled when a message was decoded.
#[derive(Clone, Debug)]
pub struct Read<T> {
    pub msg: T,
    pub read: BlsCtx,
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
        clsig: Read<ClSig>,
        hash: &[u8; 32],
    ) -> MessageProcessingResult;
    /// `Chainlocks::GetChainLockByHash`, for `getdata`.
    fn chainlock_by_hash(&self, hash: &[u8; 32], write: BlsCtx) -> Option<ClSig>;
    /// `Chainlocks::GetBestChainLock`, for P5's `mempool` reply; `None` is Core's null lock.
    fn best_chainlock(&self, write: BlsCtx) -> Option<ClSig>;
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
        _clsig: Read<ClSig>,
        _hash: &[u8; 32],
    ) -> MessageProcessingResult {
        MessageProcessingResult::default()
    }

    fn chainlock_by_hash(&self, _hash: &[u8; 32], _write: BlsCtx) -> Option<ClSig> {
        None
    }

    fn best_chainlock(&self, _write: BlsCtx) -> Option<ClSig> {
        None
    }
}

/// What `CMNAuth::PushMNAUTH` signs besides our operator key: the peer's challenge, our side of
/// the connection and the version we advertise (BL-702 §3).
// Core: evo/mnauth.cpp:36-40
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MnAuthChallenge {
    /// The challenge the peer sent in its `version` (`GetReceivedMNAuthChallenge`); never zero.
    pub received: [u8; 32],
    /// `peer.IsInboundConn()`.
    pub inbound: bool,
    /// `nOurNodeVersion`: `-pushversion` off mainnet, else `PROTOCOL_VERSION`
    /// ([`LocalIdentity::advertised_version`](dash_p2p::LocalIdentity::advertised_version)).
    pub our_version: i32,
}

/// A masternode's operator key as the `mnauth` intake verifies and grants with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperatorKeyRef {
    /// `pdmnState->pubKeyOperator` in its stored encoding.
    pub stored: [u8; 48],
    /// The stored key's scheme, from the entry's state version.
    // Core: evo/dmnstate.h:107; bls/bls.h:495-513 at v24.0.0
    pub stored_legacy: bool,
    /// SHA256d of `stored`, in internal byte order, as `TryMarkVerified` records it.
    // Core: bls/bls.h:545-560; evo/mnauth.cpp:158 at v24.0.0
    pub pubkey_hash: [u8; 32],
}

/// One published deterministic masternode list, kept by value for every read of one message.
pub trait MnListView: Send + Sync {
    /// `GetMN(proRegTxHash)`'s operator key; `None` when the list has no such masternode.
    fn operator(&self, pro_reg_tx_hash: &[u8; 32]) -> Option<OperatorKeyRef>;
    /// `GetMNByService(pnode->addr)`'s proTxHash; `None` when the list has no such service slot.
    // Core: net.cpp:4471-4476 at v24.0.0
    fn by_service(&self, service: &Service) -> Option<[u8; 32]>;
    /// Core's `bls::bls_legacy_scheme` at this view.
    // Core: bls/bls.h:177-180; evo/mnauth.cpp:93-95 at v24.0.0
    fn bls_legacy_scheme(&self) -> bool;
}

/// The list and sync half of Core's masternode wiring, installed on every full node: the
/// `mnauth` intake verifies on any synced node, and `-watchquorums` sends `qwatch` without a
/// masternode key.
///
/// Read contract: the processor calls every method with a peer's state held, so each is short and
/// non-reentrant. No method waits on validation or on a list publication, takes a lock that the
/// chain or the masternode side holds while it calls into the processor, or calls back into the
/// processor or the network core's peer table. [`MasternodeNet::tip_list`] is a wait-free load of
/// the last published list: when publication lags it returns the older one, and it never blocks.
/// [`MasternodeNet::wants_qwatch`] follows the same contract: one such live load (never the
/// message's [`ChainSnapshot`]), with only leaf locks for the quorum-node sets.
pub trait MasternodeNet: Send + Sync {
    /// `CMasternodeSync::IsBlockchainSynced()`.
    // Core: evo/mnauth.cpp:57-60
    fn blockchain_synced(&self) -> bool;
    /// `CDeterministicMNManager::GetListAtChainTip()`. The processor never calls it; a handler
    /// reads it at most once per message and keeps the `Arc` for every read of that message.
    // Core: net_processing.cpp:5801
    fn tip_list(&self) -> Arc<dyn MnListView>;
    /// `qman->IsWatching() && IsMasternodeQuorumNode(peer, GetListAtChainTip())`.
    // Core: net_processing.cpp:4246-4247
    fn wants_qwatch(&self, peer: &PeerHandle) -> bool;
}

/// The quorum-watch half of Core's quorum manager used when sending `qwatch`.
///
/// Follows [`MasternodeNet`]'s read contract: leaf locks only, no waiting or calls back into
/// the processor or `NetCore`. The supplied tip is the one loaded view; implementations never
/// reload it.
pub trait QuorumWatch: Send + Sync {
    /// Core's `qman->IsWatching()`.
    // Core: net_processing.cpp:4246-4247 at v24.0.0
    fn is_watching(&self) -> bool;
    /// Core's `IsMasternodeQuorumNode(peer, GetListAtChainTip())`.
    // Core: net.cpp:4465-4490 at v24.0.0
    fn is_quorum_node(&self, peer: &PeerHandle, tip: &dyn MnListView) -> bool;
}

/// No quorum watch connections until the quorum manager supplies the production implementation.
#[derive(Debug, Default)]
pub struct NoQuorumWatch;

impl QuorumWatch for NoQuorumWatch {
    fn is_watching(&self) -> bool {
        false
    }

    fn is_quorum_node(&self, _peer: &PeerHandle, _tip: &dyn MnListView) -> bool {
        false
    }
}

/// An active masternode's signing half (Core's `CActiveMasternodeManager`); its presence is what
/// makes the node a masternode. Same read contract as [`MasternodeNet`]: one BLS sign under
/// leaf locks only.
pub trait MnAuthSigner: Send + Sync {
    /// `PushMNAUTH`'s message for `c`, signed with the basic BLS scheme; `None` while our
    /// proTxHash is null.
    // Core: evo/mnauth.cpp:19-46
    fn mnauth_payload(&self, c: &MnAuthChallenge) -> Option<MnAuth>;
}

/// The processor's masternode seams: the net half on every full node, the signer on an active
/// masternode only.
#[derive(Clone)]
pub struct MasternodeSeams {
    /// The list and sync half.
    pub net: Arc<dyn MasternodeNet>,
    /// The signer; `Some` exactly when we run as a masternode.
    pub signer: Option<Arc<dyn MnAuthSigner>>,
}

impl Default for MasternodeSeams {
    /// No masternode wiring: [`NoMasternodeNet`] and no signer, for SPV and tests.
    fn default() -> Self {
        Self {
            net: Arc::new(NoMasternodeNet),
            signer: None,
        }
    }
}

/// A node with no masternode list: never synced, an empty list, no `qwatch`.
#[derive(Debug, Default)]
pub struct NoMasternodeNet;

/// The empty list [`NoMasternodeNet`] returns.
#[derive(Debug, Default)]
struct EmptyMnList;

impl MnListView for EmptyMnList {
    fn operator(&self, _pro_reg_tx_hash: &[u8; 32]) -> Option<OperatorKeyRef> {
        None
    }

    fn by_service(&self, _service: &Service) -> Option<[u8; 32]> {
        None
    }

    fn bls_legacy_scheme(&self) -> bool {
        false
    }
}

impl MasternodeNet for NoMasternodeNet {
    fn blockchain_synced(&self) -> bool {
        false
    }

    fn tip_list(&self) -> Arc<dyn MnListView> {
        Arc::new(EmptyMnList)
    }

    fn wants_qwatch(&self, _peer: &PeerHandle) -> bool {
        false
    }
}

/// The P5a D5 stand-in: runs only the seen check and insert before signature verification.
/// `is_enabled` reads `SPORK_19` live through `enabled`. Each new hash records the node clock's
/// first-seen time, so `AlreadyHave` turns true and a repeat costs nothing. Every result is empty:
/// no relay, no score. Nothing is accepted or served. The map shares Core's `cs` with the
/// [`SeenChainLocksCleanup`] run by the handler's 5 s task.
pub struct SeenChainLocks {
    /// Reads `SPORK_19` live, as Core's `Chainlocks::IsEnabled` does.
    enabled: Box<dyn Fn() -> bool + Send>,
    /// The seen hashes, first-seen times and node clock, shared with the cleanup task.
    seen: SeenChainLocksMap,
}

impl SeenChainLocks {
    /// A stand-in whose `IsEnabled` is `enabled()` and whose times are `clock`'s.
    pub fn new(enabled: impl Fn() -> bool + Send + 'static, clock: Arc<dyn Clock>) -> Self {
        Self {
            enabled: Box::new(enabled),
            seen: SeenChainLocksMap::new(clock),
        }
    }

    /// How many hashes the seen map holds.
    pub fn seen_len(&self) -> usize {
        self.seen.len()
    }

    /// The seen map's `Cleanup`, for the handler's 5 s task.
    pub fn cleanup(&self) -> SeenChainLocksCleanup {
        self.seen.cleanup()
    }
}

impl std::fmt::Debug for SeenChainLocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeenChainLocks").field("seen", &self.seen_len()).finish_non_exhaustive()
    }
}

impl ChainLocks for SeenChainLocks {
    fn is_enabled(&self) -> bool {
        (self.enabled)()
    }

    fn already_have(&self, hash: &[u8; 32]) -> bool {
        self.seen.contains(hash)
    }

    fn process_new_chainlock(
        &mut self,
        _from: PeerId,
        _clsig: Read<ClSig>,
        hash: &[u8; 32],
    ) -> MessageProcessingResult {
        self.seen.insert(*hash);
        MessageProcessingResult::default()
    }

    fn chainlock_by_hash(&self, _hash: &[u8; 32], _write: BlsCtx) -> Option<ClSig> {
        None
    }

    fn best_chainlock(&self, _write: BlsCtx) -> Option<ClSig> {
        None
    }
}

/// The mixing-transaction store and validator used by transaction processing and serving.
pub trait DstxSeam: Send {
    /// Tests `CDSTXManager::GetDSTX` membership (Core v24.0.0 net_processing.cpp:2657).
    fn contains(&self, txid: &TxHash) -> bool;
    /// Returns a kept wrapper (Core v24.0.0 net_processing.cpp:2984-2990).
    fn get(&self, txid: &TxHash) -> Option<DsTx>;
    /// Validates before mempool admission (Core v24.0.0 net_processing.cpp:3642-3720).
    fn validate(&mut self, dstx: &DsTx, txid: &TxHash) -> DstxValidation;
    /// Stores the wrapper after acceptance (Core v24.0.0 net_processing.cpp:4964-4967).
    fn on_accepted(&mut self, dstx: DsTx);
    /// Chooses the inventory kind for a peer (Core v24.0.0 net_processing.cpp:6649-6655).
    fn announce_kind(&self, txid: &TxHash, peer_version: ProtocolVersion) -> TxInvKind;
}

/// An empty DSTX store until the runtime is wired.
#[derive(Default)]
pub struct NoDstx;

impl DstxSeam for NoDstx {
    fn contains(&self, _txid: &TxHash) -> bool {
        false
    }

    fn get(&self, _txid: &TxHash) -> Option<DsTx> {
        None
    }

    fn validate(&mut self, _dstx: &DsTx, _txid: &TxHash) -> DstxValidation {
        DstxValidation::Stop(DstxScore::None)
    }

    fn on_accepted(&mut self, _dstx: DsTx) {}

    fn announce_kind(&self, _txid: &TxHash, _peer_version: ProtocolVersion) -> TxInvKind {
        TxInvKind::Tx
    }
}
