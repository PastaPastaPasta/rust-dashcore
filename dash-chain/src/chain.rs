//! The writer: `HeaderChain` (SB4 §4).

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use dash_clock::NodeTime;
use dash_consensus::ctx::{CheckpointView, Deployment, DeploymentView, bls_ctx_for_block};
use dash_consensus::header::{HeaderParams, check_block_header, contextual_check_block_header};
use dash_consensus::pow::{PowParams, block_proof, check_proof_of_work};
use dash_consensus::primitives::Header;
use dash_consensus::{
    BlockId, BlockValidationError, BlockValidationResult, ChainAncestry, ConsensusCtx, HeaderCtx,
};
use dash_num::CompactTarget;
use dash_params::ChainParams;
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric as _;
use hashbrown::HashTable;
use hashbrown::hash_table::Entry;
use parking_lot::{ArcMutexGuard, Mutex, RawMutex};
use rayon::prelude::*;

use self::sb5::HoldGuard;
use crate::Work;
use crate::active::ActiveChain;
use crate::ancestry::AnchoredAncestry;
use crate::anchor::{AnchorPack, CONTEXT_ENTRIES, floor_below};
use crate::arena;
use crate::batch::{AcceptOne, HeaderAccepted, HeaderBatch, HeaderStop, HeadersResult, PeerId};
use crate::chainlock::ChainLockRef;
use crate::config::{ChainConfig, ChainMode, Services};
use crate::entry::{EntryImm, MTP_UNKNOWN};
use crate::error::{
    AncestryError, AnchorError, ChainError, ConfigError, IndexInconsistency, LoadError, StoreError,
};
use crate::feed::{self, ChainFeeds, Fanout, StatusReader};
use crate::index::cheap_hash;
use crate::reader::{ChainReader, EntryView, FirstAncestors, IndexVisitor, ReadBackend as _};
use crate::record::{BlockIndexRecord, IndexWrite};
use crate::residency::{Permanent, Residency};
use crate::shared::{ColdInvalidation, Node, Shared};
use crate::snapshot::{ActiveChainView, ChainReadSnapshot, TipFacts};
use crate::status::Status;
use crate::status_journal::StatusJournals;
use crate::store::{HeaderStore, NoStore};
use crate::tip::{BlockRef, TipCell, TipChange, TipPlan, TipState, TipStep, TipTarget};
use crate::walk;

use rules::{PathEntry, RuleCache, RuleContext};

/// The mutable group of an entry, written under its seqlock by the single writer (§3.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MutableGroup {
    pub status: Status,
    /// Core `nTx`.
    pub n_tx: u32,
    /// Core `nFile`; `None` is Core's -1.
    pub file: Option<i32>,
    /// Core `nDataPos`.
    pub data_pos: u32,
    /// Core `nUndoPos`.
    pub undo_pos: u32,
}

/// Core's `m_failed_blocks`: a `std::set<CBlockIndex*>`, iterated here in id order (D9).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailedBlocks(BTreeSet<BlockId>);

impl FailedBlocks {
    pub fn insert(&mut self, id: BlockId) -> bool {
        self.0.insert(id)
    }

    pub fn remove(&mut self, id: BlockId) -> bool {
        self.0.remove(&id)
    }

    pub fn contains(&self, id: BlockId) -> bool {
        self.0.contains(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = BlockId> + '_ {
        self.0.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear()
    }
}

/// `getchaintips`' status of a tip (`rpc/blockchain.cpp:1914-1936`), in Core's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChainTipStatus {
    Active,
    Invalid,
    Conflicting,
    HeadersOnly,
    ValidFork,
    ValidHeaders,
    Unknown,
}

impl ChainTipStatus {
    /// Core's string.
    pub fn as_str(self) -> &'static str {
        match self {
            ChainTipStatus::Active => "active",
            ChainTipStatus::Invalid => "invalid",
            ChainTipStatus::Conflicting => "conflicting",
            ChainTipStatus::HeadersOnly => "headers-only",
            ChainTipStatus::ValidFork => "valid-fork",
            ChainTipStatus::ValidHeaders => "valid-headers",
            ChainTipStatus::Unknown => "unknown",
        }
    }
}

/// One `getchaintips` entry over the header index (§10.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainTip {
    pub id: BlockId,
    pub hash: BlockHash,
    pub height: u32,
    pub chain_work: Work,
    /// `height − find_fork(tip).height`.
    pub branchlen: u32,
    pub fork_point: BlockId,
    pub status: ChainTipStatus,
}

/// An entry the writer names (its tip, Core's `m_best_header`) with the facts the snapshot
/// shows: kept with the id, as the entry can leave the window (§13.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeadFacts {
    at: BlockRef,
    work: Work,
    time: u32,
}

/// One private tip step's leg, with the refs the step publishes (legs below the window are cold).
enum Leg {
    Connect(BlockRef),
    /// `old` leaves; `tip`, its parent, becomes the tip.
    Disconnect {
        old: BlockRef,
        tip: BlockRef,
    },
}

/// The header chain (SB0 §4.1; SB4 §4): the node runs it with `window = None` from genesis,
/// SPV anchored and windowed. Exactly one writer holds `&mut HeaderChain` (V in the node, the
/// SPV header task); readers hold [`ChainReader`] clones (§13.1).
pub struct HeaderChain<S: HeaderStore = NoStore, R: Residency = Permanent> {
    shared: Arc<Shared<S, R>>,
    params: Arc<ChainParams>,
    cfg: ChainConfig,
    svc: Services,
    /// The next arena id (dense; SB5 seeds it from `meta.next_id` in SPV).
    next_id: u32,
    /// Core's `m_best_header`; set by every constructor (R5).
    best_header: Option<HeadFacts>,
    /// The writer's tip, Core's `m_chain.Tip()`: `None` before the node has one (CH1-A11).
    /// Set with each step's last leg; SPV's can be cold (a segmented reorg's fork).
    tip: Option<HeadFacts>,
    /// The index's root, the node's placeholder before it has a tip (CH1-A11): the network's
    /// genesis, unless [`HeaderChain::load_records`] loaded another network's index, which the
    /// node refuses before V starts (CH1-node D54).
    root: BlockId,
    /// Node: Core's `m_prev_block_index` as sibling lists (§3.7).
    links: Links,
    /// Every resident entry with no child, by work (§3.7, §6.2).
    leaves: BTreeSet<(Work, Reverse<BlockId>)>,
    /// Core's `m_dirty_blockindex` (D15); node only. SPV's dirty set is the overlay's keys for
    /// status changes and the accept results for new records (SB5-A13).
    dirty: Dirty,
    /// SPV: every status change after insertion, resident or evicted, until the step commits
    /// (§3.7, D33, D36), status-only (SB5-A13): a cold id is marked without loading its record.
    /// A mark no stage 4 has applied outlives stage 5 (mi-2).
    overlay: HashMap<BlockId, Mark>,
    /// Core's `m_failed_blocks`.
    failed: FailedBlocks,
    /// The writer's active chain; each publication shares its root (§3.6).
    active: ActiveChain,
    /// SPV: the plan `tip_plan` returned last, which `set_active_tip` must run (§9.1 stage 1).
    pending_plan: Mutex<Option<TipPlan>>,
    /// A [`Hold`] is open: the tip lock is held by the writer's own frame (CH1-A1).
    held: bool,
    /// The `seq` of the last tip step, 0 after `init_active_tip`: the writer's copy of the tip
    /// lock's, for [`HeaderChain::private_tip`] inside a hold (CH1-A2).
    last_seq: u64,
    /// The header checks' view of the chain parameters.
    hp: HeaderParams,
    /// Node: each status write's old status, journalled, for readers' status
    /// as of their snapshot (CH1-node D63).
    journals: StatusJournals,
    /// Core's `mapCheckpoints` by hash: each checkpoint hash and its key, the highest key for a
    /// hash listed twice (§11.4).
    checkpoints: HashMap<BlockHash, u32>,
    /// `GetLastCheckpoint`'s answer, cached: the key and entry height of the highest-keyed
    /// checkpoint in the index (§11.4). Entries never leave the index (SPV's leave the arena
    /// only), so it only moves up.
    last_checkpoint: Option<(u32, u32)>,
    /// Core `bestChainLock` (§7.1).
    cl_best: Option<ChainLockRef>,
    /// Core `bestChainLockBlockIndex` (§7.1).
    cl_floor: Option<BlockRef>,
    /// The ChainLock handler's cached `isEnabled`, refreshed only by `check_cl_active_state`
    /// (§7.5).
    cl_enabled_observed: bool,
    /// Node: the tip state while `enforce_block` holds the tip mutex (§9.1, D-C6-1). The guard
    /// stays in `enforce_block`'s frame, so the chain stays `Send`; the host's
    /// `disconnect_tip` steps on this state.
    composite: Option<TipState>,
    /// The lowest height held: 0 from genesis, anchor − 24 when anchored (§8.4).
    floor: u32,
    /// SPV: the header rules' cached context (§8.2).
    rules: RuleCache,
    /// SPV: bumped by each stage 4 that evicts (§8.2 step 6, §13.3).
    residency_epoch: u64,
    /// SPV: the resident entries by height, the eviction order (SB0 §4.4).
    resident: BTreeSet<(u32, BlockId)>,
    /// SPV: each installed chunk's resident entries; a chunk is released at zero.
    chunk_live: HashMap<u32, u32>,
    /// SPV: the largest work of any leaf that left the window (§3.7), refreshed by the
    /// queries that read the store's leaves through `&self` (§6.2, §10.2).
    max_cold_leaf_work: Mutex<Work>,
}

/// SPV: one overlay mark (§3.7): the status, and whether a stage 4 has written it to the
/// resident group. Stage 5 drops applied marks only, so a step whose stage 4 never ran (it
/// erred, a hold was abandoned, or the driver skipped it) keeps its marks for the next one:
/// the resident status never falls behind the committed record for the writer, and readers
/// see it with a snapshot (D36).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    status: Status,
    applied: bool,
}

/// The overlay capacity kept between steps: a typical step marks a few entries, and an
/// adversarial step's table is freed (SB5 §6.2's marks account).
const OVERLAY_KEPT: usize = 64;

/// No id: the end of a sibling list.
const NO_ID: u32 = u32::MAX;

/// Core's `m_prev_block_index` for `Permanent` (§3.7): each entry's first child and next
/// sibling, writer-only, 8 B per entry. A new child goes to the head of its parent's list;
/// [`Links::children`] returns the list in id order (D9).
#[derive(Default)]
struct Links {
    first_child: Vec<u32>,
    next_sibling: Vec<u32>,
}

impl Links {
    fn add(&mut self, id: BlockId, prev: Option<BlockId>) {
        let i = id.0 as usize;
        // `load` allocates a parent's id after its first child's (first mention, §10.3).
        let len = prev.map_or(i, |p| i.max(p.0 as usize)) + 1;
        if self.first_child.len() < len {
            self.first_child.resize(len, NO_ID);
            self.next_sibling.resize(len, NO_ID);
        }
        if let Some(p) = prev {
            let p = p.0 as usize;
            self.next_sibling[i] = self.first_child[p];
            self.first_child[p] = id.0;
        }
    }

    /// Whether `prev` has a child.
    fn has_children(&self, prev: BlockId) -> bool {
        self.first_child.get(prev.0 as usize).is_some_and(|&c| c != NO_ID)
    }

    /// `prev`'s children, in id order.
    fn children(&self, prev: BlockId) -> Vec<BlockId> {
        let mut out = Vec::new();
        let mut c = self.first_child.get(prev.0 as usize).copied().unwrap_or(NO_ID);
        while c != NO_ID {
            out.push(BlockId(c));
            c = self.next_sibling[c as usize];
        }
        out.reverse();
        out
    }
}

/// Ids per page of the SPV store's children and leaves: a page's parent marks in
/// `cold_leaves` are one `u64`, its leaves 2,560 B of stack.
const PAGE: usize = 64;

/// Where the store's page after `page` starts: past its last id, `None` after an empty page
/// (the store's end) or a last id of `u32::MAX`. A short page is not the end: a store may
/// drop rows from a page, and stopping there would silently lose the rest.
fn next_page<T>(page: &[T], id: impl Fn(&T) -> BlockId) -> Option<BlockId> {
    page.last().and_then(|l| id(l).0.checked_add(1)).map(BlockId)
}

/// [`HeaderChain::children`]'s cursor.
#[expect(clippy::large_enum_variant, reason = "on the stack: a box would allocate per walk level")]
pub(crate) enum Children {
    /// The node's sibling list.
    Node(std::vec::IntoIter<BlockId>),
    Spv(SpvChildren),
}

/// SPV's children of `prev`: two id-ordered streams merged, the store's pages and the dirty
/// set's entries on `prev`.
pub(crate) struct SpvChildren {
    prev: BlockId,
    hash: BlockHash,
    /// The store's page; `page[at..len]` is not yet merged.
    page: [BlockId; PAGE],
    at: usize,
    len: usize,
    /// Where the store's next page starts; `None` after a short page.
    from: Option<BlockId>,
    /// The least uncommitted child not yet merged, once found.
    dirty: Option<BlockId>,
    /// Where the dirty set's scan resumes; `None` at its end.
    scan: Option<BlockId>,
}

impl Children {
    /// The next child in id order; `None` after the last.
    pub(crate) fn next<S: HeaderStore, R: Residency>(
        &mut self,
        chain: &HeaderChain<S, R>,
    ) -> Result<Option<BlockId>, ChainError> {
        let c = match self {
            Children::Node(ids) => return Ok(ids.next()),
            Children::Spv(c) => c,
        };
        if c.at == c.len
            && let Some(from) = c.from
        {
            c.len = chain.shared.cold_children(&c.hash, from, &mut c.page)?;
            c.at = 0;
            c.from = next_page(&c.page[..c.len], |&id| id);
        }
        while c.dirty.is_none()
            && let Some(from) = c.scan
        {
            let d = chain.dirty.first_from(from);
            c.scan = d.and_then(|d| d.0.checked_add(1)).map(BlockId);
            let on_prev = |d: &BlockId| chain.shared.arena.imm(*d, |e| e.prev == Some(c.prev));
            c.dirty = d.filter(|d| on_prev(d).unwrap_or(false));
        }
        let stored = c.page[c.at..c.len].first().copied();
        let next = match (stored, c.dirty) {
            (Some(s), Some(d)) => Some(s.min(d)),
            (s, d) => s.or(d),
        };
        // A stored child the dirty set also holds is merged once.
        if next.is_some() && stored == next {
            c.at += 1;
        }
        if next.is_some() && c.dirty == next {
            c.dirty = None;
        }
        Ok(next)
    }
}

/// Words of `Dirty`'s bits kept across a `take`: 4,096 ids, two of SPV's 2,000-header steps.
const DIRTY_BITS_KEPT: usize = 64;

/// The dirty set: ids in the order they were first marked, deduplicated by a bit per id
/// (§10.3). The bits start at the set's lowest word, so they span the ids marked since the last
/// `take`, not the chain: SPV's set is one step's new ids (A-31).
#[derive(Default)]
struct Dirty {
    ids: Vec<BlockId>,
    bits: Vec<u64>,
    /// The id word `bits[0]` stands for.
    lo: usize,
}

impl Dirty {
    fn insert(&mut self, id: BlockId) {
        let (w, b) = ((id.0 / 64) as usize, id.0 % 64);
        // Every bit is clear in an empty set, so it can start anywhere.
        if self.ids.is_empty() {
            self.lo = w;
        } else if w < self.lo {
            self.bits.splice(0..0, std::iter::repeat_n(0, self.lo - w));
            self.lo = w;
        }
        let w = w - self.lo;
        if self.bits.len() <= w {
            self.bits.resize(w + 1, 0);
        }
        if self.bits[w] & (1 << b) == 0 {
            self.bits[w] |= 1 << b;
            self.ids.push(id);
        }
    }

    #[cfg(all(test, not(loom)))]
    fn contains(&self, id: BlockId) -> bool {
        let w = (id.0 / 64) as usize;
        w.checked_sub(self.lo)
            .and_then(|w| self.bits.get(w))
            .is_some_and(|w| w & (1 << (id.0 % 64)) != 0)
    }

    /// The ids in marking order.
    fn ids(&self) -> &[BlockId] {
        &self.ids
    }

    /// The least id in the set from `from` up: the set in id order, read from its bits.
    fn first_from(&self, from: BlockId) -> Option<BlockId> {
        let (mut w, mut mask) = match ((from.0 / 64) as usize).checked_sub(self.lo) {
            Some(w) => (w, u64::MAX << (from.0 % 64)),
            None => (0, u64::MAX),
        };
        while let Some(&bits) = self.bits.get(w) {
            if bits & mask != 0 {
                let id = (self.lo + w) * 64 + (bits & mask).trailing_zeros() as usize;
                return Some(BlockId(id as u32));
            }
            (w, mask) = (w + 1, u64::MAX);
        }
        None
    }

    /// The ids in marking order; the set is empty afterwards, its bits kept only when small.
    fn take(&mut self) -> Vec<BlockId> {
        if self.bits.len() > DIRTY_BITS_KEPT {
            self.bits = Vec::new();
        } else {
            for id in &self.ids {
                self.bits[(id.0 / 64) as usize - self.lo] &= !(1 << (id.0 % 64));
            }
        }
        std::mem::take(&mut self.ids)
    }
}

/// Records per parallel decode batch in `load` (§10.3).
const LOAD_BATCH: usize = 1 << 16;

/// `load`'s ids by hash, allocated on first mention as Core's `InsertBlockIndex` allocates.
#[derive(Default)]
struct Mentions {
    hashes: Vec<BlockHash>,
    table: HashTable<u32>,
}

impl Mentions {
    /// `h`'s id, allocating the next one on its first mention.
    fn mention(&mut self, h: &BlockHash) -> u32 {
        let hashes = &self.hashes;
        let eq = |&id: &u32| hashes[id as usize] == *h;
        match self.table.entry(cheap_hash(h), eq, |&id| cheap_hash(&hashes[id as usize])) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                let id = u32::try_from(self.hashes.len()).expect("fewer than 2^32 entries");
                e.insert(id);
                self.hashes.push(*h);
                id
            }
        }
    }
}

/// One record decoded and checked against its stored hash, without X11 (`txdb.cpp:328-331`).
fn decode_checked(
    hash: &BlockHash,
    bytes: &[u8],
    pow: &PowParams,
) -> Result<BlockIndexRecord, LoadError> {
    // Core's `InsertBlockIndex` maps the null hash to no entry and then dereferences it.
    if *hash == BlockHash::from([0; 32]) {
        return Err(LoadError::NullHash);
    }
    let rec = BlockIndexRecord::decode(bytes).map_err(|err| LoadError::Decode {
        hash: *hash,
        err,
    })?;
    match check_proof_of_work(hash.as_bytes(), CompactTarget::new(rec.header.bits), pow) {
        true => Ok(rec),
        false => Err(LoadError::HighHash {
            hash: *hash,
        }),
    }
}

impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    // ---- construction ------------------------------------------------------------------------

    /// The index part of Core's `LoadGenesisBlock`: the genesis entry, `VALID_TREE`, dirty.
    // Core: validation.cpp LoadGenesisBlock → blockstorage.cpp:85-129 AddToBlockIndex
    pub fn new_genesis(
        params: Arc<ChainParams>,
        cfg: ChainConfig,
        svc: Services,
        store: S,
    ) -> Result<Self, ChainError> {
        check_config(&cfg)?;
        let hp = HeaderParams::from_chain_params(&params).map_err(ConfigError::Pow)?;
        let genesis = Header::new(params.genesis.header);
        debug_assert_eq!(
            genesis.hash().as_bytes(),
            params.consensus.hash_genesis_block.as_bytes(),
            "the genesis block hashes to hashGenesisBlock"
        );
        let mut chain = HeaderChain::empty(params, cfg, svc, store, hp, 0);
        let id =
            chain.add_to_block_index(genesis.raw(), &genesis.hash(), Status(Status::VALID_TREE))?;
        chain.publish_first(id)?;
        Ok(chain)
    }

    /// A constructor's first publication. SPV: `root` is the tip. Node: no tip yet (CH1-A11):
    /// the snapshot names `root` (genesis) as a placeholder with `tip_seq = 0` until V's
    /// `init_active_tip` (Core `LoadChainTip`) or `connect_tip(genesis)`, as Core's `m_chain`
    /// is empty until then.
    fn publish_first(&mut self, root: BlockId) -> Result<(), ChainError> {
        match self.cfg.mode {
            ChainMode::Spv => self.init_active_tip(root),
            ChainMode::Node => {
                debug_assert!(self.active.is_empty(), "the first publication");
                self.root = root;
                self.set_tip_cell(self.block_ref(root));
                Ok(())
            }
        }
    }

    /// The first publication: the tip cell, its snapshot at `tip_seq` 0 and epoch 0. No
    /// `after_publish`: no subscriber can exist before the cell does.
    fn set_tip_cell(&self, tip: BlockRef) {
        let cell = TipCell::new(tip, self.build_snapshot(0), self.cfg.feeds, self.svc.fail.clone());
        assert!(self.shared.readers.tip.set(cell).is_ok(), "one writer sets the tip cell");
    }

    /// The root entry, the node's placeholder tip before it has one (CH1-A11).
    fn genesis(&self) -> BlockId {
        self.root
    }

    /// A chain with no entries; every constructor adds its first entry next. `anchor` is the
    /// lowest accepted height: 0 from genesis, else the anchor's, whose 24 context entries
    /// below it put the floor, the lowest height held, at `anchor − 24` (§8.4).
    fn empty(
        params: Arc<ChainParams>,
        cfg: ChainConfig,
        svc: Services,
        store: S,
        hp: HeaderParams,
        anchor: u32,
    ) -> Self {
        // Core's `std::map<int, uint256>` from an initializer list: the first entry for a height.
        let mut by_height = std::collections::BTreeMap::new();
        for (height, hash) in &params.checkpoints {
            by_height.entry(height.to_u32()).or_insert(BlockHash::from(*hash.as_bytes()));
        }
        let mut checkpoints = HashMap::with_capacity(by_height.len());
        for (key, hash) in by_height {
            checkpoints.insert(hash, key);
        }
        let shared = Arc::new(Shared::new(&params, &cfg, store, svc.fail.clone(), anchor));
        HeaderChain {
            journals: StatusJournals::default(),
            shared,
            params,
            cfg,
            svc,
            next_id: 0,
            best_header: None,
            tip: None,
            root: BlockId(0),
            links: Links::default(),
            leaves: BTreeSet::new(),
            dirty: Dirty::default(),
            overlay: HashMap::new(),
            failed: FailedBlocks::default(),
            active: ActiveChain::default(),
            pending_plan: Mutex::new(None),
            held: false,
            last_seq: 0,
            hp,
            checkpoints,
            last_checkpoint: None,
            cl_best: None,
            cl_floor: None,
            cl_enabled_observed: false,
            composite: None,
            floor: floor_below(anchor),
            rules: RuleCache::default(),
            residency_epoch: 0,
            resident: BTreeSet::new(),
            chunk_live: HashMap::new(),
            max_cold_leaf_work: Mutex::new(Work::ZERO),
        }
    }

    /// Core's `BlockManager::AddToBlockIndex` (§5.4): a known hash returns its id; otherwise a
    /// new entry with `prev` found by hash, Core's height, skip, `nTimeMax`, chain work and
    /// median time past, published to readers last (§13.1). SPV resolves an evicted `prev` from
    /// its record; `Err` only for a failed cold read.
    // Core: blockstorage.cpp:85-129 BlockManager::AddToBlockIndex
    pub(crate) fn add_to_block_index(
        &mut self,
        header: &BlockHeader,
        hash: &BlockHash,
        status: Status,
    ) -> Result<BlockId, ChainError> {
        if let Some(id) = self.shared.try_lookup(hash)? {
            return Ok(id);
        }
        let prev = self.shared.try_lookup(&header.prev_hash)?;
        self.add_new(header, hash, prev, status)
    }

    /// [`HeaderChain::add_to_block_index`] for a hash the index lacks, `prev` already resolved.
    fn add_new(
        &mut self,
        header: &BlockHeader,
        hash: &BlockHash,
        prev: Option<BlockId>,
        status: Status,
    ) -> Result<BlockId, ChainError> {
        debug_assert!(!status.has(Status::FAILED_MASK), "no failed blocks allowed");
        let imm = self.derive(header, hash, prev)?;
        let mut group = MutableGroup::default();
        if status.has(Status::VALID_MASK) {
            group.status = group.status.raise_validity(status.bits()).0;
        } else {
            group.status = group.status.raise_validity(Status::VALID_TREE).0;
            group.status = Status(group.status.bits() | status.bits());
        }
        self.insert_entry(imm, &group, status.has(Status::VALID_MASK))
    }

    /// Sets, indexes and links a new entry at the next id; with `best`, the best header moves
    /// to it when it has more work.
    fn insert_entry(
        &mut self,
        imm: EntryImm,
        group: &MutableGroup,
        best: bool,
    ) -> Result<BlockId, ChainError> {
        let id = BlockId(self.next_id);
        if best && self.best_header.is_none_or(|b| b.work < imm.chain_work) {
            self.best_header = Some(HeadFacts {
                at: BlockRef {
                    id,
                    hash: imm.hash,
                    height: imm.height,
                },
                work: imm.chain_work,
                time: imm.header.time,
            });
        }
        let (prev, chain_work, height, hash) = (imm.prev, imm.chain_work, imm.height, imm.hash);
        crate::fault::residency(|| {
            self.shared.arena.set(id, imm, group);
            self.shared.index.insert(&self.shared.arena, id);
        });
        self.next_id = self.next_id.checked_add(1).expect("fewer than 2^32 entries");
        self.shared.issue(self.next_id);

        self.dirty.insert(id);
        if R::EVICTS {
            crate::fault::residency(|| {
                self.resident.insert((height, id));
                *self.chunk_live.entry(arena::chunk_of(id)).or_default() += 1;
            });
        } else {
            self.links.add(id, prev);
        }
        // A cold parent has left `leaves` already.
        if let Some((w, p)) =
            prev.and_then(|p| self.shared.arena.imm(p, |e| e.chain_work).map(|w| (w, p)))
        {
            self.leaves.remove(&(w, Reverse(p)));
        }
        self.leaves.insert((chain_work, Reverse(id)));
        self.note_checkpoint(&hash, height);
        Ok(id)
    }

    /// Raises the `GetLastCheckpoint` cache when `hash` is a checkpoint (§11.4).
    fn note_checkpoint(&mut self, hash: &BlockHash, height: u32) {
        if let Some(&key) = self.checkpoints.get(hash)
            && self.last_checkpoint.is_none_or(|(k, _)| key > k)
        {
            self.last_checkpoint = Some((key, height));
        }
    }

    /// An entry's derived fields (§5.4): Core's height, `BuildSkip`, `nTimeMax`, `nChainWork`
    /// and `GetMedianTimePast`. In SPV, `prev` and the entries its skip walk and median time
    /// read may be cold (§5.4's cold prev: `nTimeMax` from the record, D35), and a skip height
    /// below the floor gets no skip (D-C4-1). `Err` only for a failed cold read.
    // Core: blockstorage.cpp:85-129 AddToBlockIndex; :268-331 LoadBlockIndex
    fn derive(
        &self,
        header: &BlockHeader,
        hash: &BlockHash,
        prev: Option<BlockId>,
    ) -> Result<EntryImm, ChainError> {
        // An ancestor's link and the fields read here, resident or cold.
        let read = |id| match self.shared.arena.imm(id, Ancestor::of_entry) {
            Some(a) => Ok(a),
            None => self.node(id).map(|n| Ancestor::of_node(&n)),
        };
        let (height, skip, time_max, prev_work, mtp) = match prev {
            Some(p) => {
                let pn = read(p)?;
                let h = self.child_height(p, pn.link.height)?;
                let skip = match walk::skip_height(h) >= self.floor {
                    // Core's `BuildSkip`.
                    true => walk::ancestor(
                        p,
                        walk::skip_height(h),
                        |x| read(x).map(|a| a.link),
                        |x| self.off_height(x),
                    )?,
                    false => None,
                };
                let mut times = [header.time; walk::MEDIAN_TIME_SPAN];
                let mut n = 1;
                let mut cur = pn;
                loop {
                    times[n] = cur.time;
                    n += 1;
                    match cur.link.prev {
                        Some(x) if n < walk::MEDIAN_TIME_SPAN => cur = read(x)?,
                        _ => break,
                    }
                }
                let mtp = mtp_or_unknown(&times[..n], h);
                (h, skip, pn.time_max.max(header.time), pn.chain_work, mtp)
            }
            None => (0, None, header.time, Work::ZERO, i64::from(header.time)),
        };
        Ok(EntryImm {
            hash: *hash,
            header: *header,
            prev,
            skip,
            height,
            chain_work: prev_work + block_proof(CompactTarget::new(header.bits)),
            mtp,
            time_max,
            context: false,
        })
    }

    /// The height of a child of `prev`, at `height`: a stored height with no successor is
    /// corruption, the latch tripped (Opus r1 n-3).
    fn child_height(&self, prev: BlockId, height: u32) -> Result<u32, ChainError> {
        height.checked_add(1).ok_or_else(|| {
            let what = format!("entry {}: height {height} has no successor", prev.0);
            self.shared.corrupt(what).into()
        })
    }

    /// `id`, resident or cold. An id the chain handed out with no record is corruption.
    fn node(&self, id: BlockId) -> Result<Node, ChainError> {
        Ok(self.shared.node(id)?)
    }

    /// `id` as CH2 names it, resident or cold.
    fn block_ref_of(&self, id: BlockId) -> Result<BlockRef, ChainError> {
        if let Some(r) = self.shared.arena.imm(id, |e| (e.hash, e.height)) {
            return Ok(BlockRef {
                id,
                hash: r.0,
                height: r.1,
            });
        }
        let n = self.node(id)?;
        Ok(BlockRef {
            id,
            hash: n.hash,
            height: n.height,
        })
    }

    /// `id`'s facts as `best_header` and `tip` keep them, resident or cold.
    fn facts_of(&self, id: BlockId) -> Result<HeadFacts, ChainError> {
        let n = self.node(id)?;
        Ok(HeadFacts {
            at: BlockRef {
                id,
                hash: n.hash,
                height: n.height,
            },
            work: n.chain_work,
            time: n.header.time,
        })
    }

    /// `id`'s height, resident or cold.
    fn height_of(&self, id: BlockId) -> Result<u32, ChainError> {
        match self.shared.arena.imm(id, |e| e.height) {
            Some(h) => Ok(h),
            None => self.node(id).map(|n| n.height),
        }
    }

    /// A stored link off its height (Sol r1 m1): corruption, the latch tripped.
    fn off_height(&self, id: BlockId) -> ChainError {
        self.shared.off_height(id).into()
    }

    /// `n`'s parent, one height below it, resident or cold: a stored parent at another height
    /// is corruption, the latch tripped, so every walk down `prev` strictly descends, a resident
    /// entry's step to its cold parent included (Sol r1 m1).
    fn parent(&self, n: &Node) -> Result<Option<(BlockId, Node)>, ChainError> {
        let Some(p) = n.prev else {
            return Ok(None);
        };
        let pn = self.node(p)?;
        match pn.height.checked_add(1) == Some(n.height) {
            true => Ok(Some((p, pn))),
            false => Err(self.off_height(p)),
        }
    }

    /// A resident entry's chain work.
    fn work(&self, id: BlockId) -> Work {
        self.shared.arena.imm(id, |e| e.chain_work).expect("resident entry")
    }

    /// `prev`'s children in id order (§3.7), through [`Children::next`]. Node: the sibling
    /// lists. SPV: the committed children from the store's `prev` index, a page at a time, merged
    /// with the step's uncommitted new entries on `prev` (D-C3-4: the dirty set, which stage 5
    /// empties), so that a wide tree costs no memory (§6.2).
    pub(crate) fn children(&self, prev: BlockId) -> Result<Children, ChainError> {
        if !R::EVICTS {
            return Ok(Children::Node(self.links.children(prev).into_iter()));
        }
        Ok(Children::Spv(SpvChildren {
            prev,
            hash: self.node(prev)?.hash,
            page: [BlockId(0); PAGE],
            at: 0,
            len: 0,
            from: Some(BlockId(0)),
            dirty: None,
            scan: Some(BlockId(0)),
        }))
    }

    /// Every child of `prev`, in id order.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn child_ids(&self, prev: BlockId) -> Result<Vec<BlockId>, ChainError> {
        let (mut c, mut out) = (self.children(prev)?, Vec::new());
        while let Some(id) = c.next(self)? {
            out.push(id);
        }
        Ok(out)
    }

    /// Every cold leaf, in id order.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn cold_leaf_ids(&self) -> Result<Vec<BlockId>, ChainError> {
        let mut out = Vec::new();
        self.cold_leaves(|id, _| {
            out.push(id);
            Ok(())
        })?;
        Ok(out)
    }

    /// SPV: the leaves that left the window (§3.7), `(id, chain_work)` to `f` in id order: the
    /// store's leaves less the resident entries (each in `leaves`, or under a resident
    /// uncommitted child whose branch's leaf is), and less any with a child the store has not
    /// seen yet (the step's uncommitted new entries, as [`HeaderChain::children`] adds them). The
    /// store is read a page at a time (§6.2). A complete read refreshes `max_cold_leaf_work` to
    /// the exact maximum (§6.2, §10.2); a failed read trips the latch.
    fn cold_leaves(
        &self,
        mut f: impl FnMut(BlockId, Work) -> Result<(), ChainError>,
    ) -> Result<(), ChainError> {
        let arena = &self.shared.arena;
        let (mut page, mut from, mut max) =
            ([(BlockId(0), Work::ZERO); PAGE], Some(BlockId(0)), Work::ZERO);
        while let Some(lo) = from {
            let n = self.shared.cold_leaves(lo, &mut page)?;
            from = next_page(&page[..n], |&(id, _)| id);
            let page = &page[..n];
            // The page's leaves under an uncommitted entry, one pass over the dirty set.
            let mut parents = 0u64;
            for &id in self.dirty.ids() {
                if let Some(Some(p)) = arena.imm(id, |e| e.prev)
                    && let Ok(k) = page.binary_search_by_key(&p, |&(id, _)| id)
                {
                    parents |= 1 << k;
                }
            }
            for (k, &(id, work)) in page.iter().enumerate() {
                if arena.imm(id, |_| ()).is_some() {
                    continue;
                }
                // Over every cold leaf: one whose child is uncommitted stays a bound until the
                // commit.
                max = max.max(work);
                if parents & (1 << k) == 0 {
                    f(id, work)?;
                }
            }
        }
        *self.max_cold_leaf_work.lock() = max;
        Ok(())
    }

    /// SPV's Candidate start from a checked anchor pack (§11.1): the 24 context entries, lowest
    /// first, then the anchor, `VALID_TREE`, every one dirty. The anchor is the best header and
    /// the active tip; the floor is `anchor − 24`. SPV mode and `Evicting` residency only.
    pub fn new_anchored(
        params: Arc<ChainParams>,
        cfg: ChainConfig,
        svc: Services,
        store: S,
        pack: &AnchorPack,
    ) -> Result<Self, ChainError> {
        check_config(&cfg)?;
        if cfg.mode == ChainMode::Node {
            return Err(ConfigError::AnchoredNode.into());
        }
        // `Permanent`'s walks, work and children read the arena only, which an anchored chain
        // does not hold below its floor (SB5 D70).
        if !R::EVICTS {
            return Err(ConfigError::AnchoredPermanent.into());
        }
        let hp = HeaderParams::from_chain_params(&params).map_err(ConfigError::Pow)?;
        let (hashes, works) = check_pack(pack, hp.pow())?;
        let floor = floor_below(pack.height);
        let mut chain = HeaderChain::empty(params, cfg, svc, store, hp, pack.height);
        let group = MutableGroup {
            status: MutableGroup::default().status.raise_validity(Status::VALID_TREE).0,
            ..MutableGroup::default()
        };
        let (mut prev, mut time_max) = (None, 0);
        for (i, header) in pack.headers.iter().enumerate() {
            let height = floor + i as u32;
            let skip = match prev {
                Some(p) if walk::skip_height(height) >= floor => {
                    let link = |id| chain.shared.link(id).ok_or(());
                    walk::build_skip(Some(p), height, link, walk::resident_off_height)
                        .expect("pack entries are resident")
                }
                _ => None,
            };
            // nTimeMax over the pack only: the history below it is not held.
            time_max = header.time.max(time_max);
            let mtp = match i + 1 >= walk::MEDIAN_TIME_SPAN {
                true => walk::median_time_past(pack.headers[..=i].iter().rev().map(|h| h.time)),
                false => MTP_UNKNOWN,
            };
            let imm = EntryImm {
                hash: hashes[i],
                header: *header,
                prev,
                skip,
                height,
                chain_work: works[i],
                mtp,
                time_max,
                context: i < CONTEXT_ENTRIES as usize,
            };
            prev = Some(chain.insert_entry(imm, &group, true)?);
        }
        chain.publish_first(prev.expect("the anchor"))?;
        Ok(chain)
    }

    /// The node's block index from its records (§10.3): Core's `LoadBlockIndexGuts`,
    /// `BlockManager::LoadBlockIndex` and the best-header recompute. The decode and PoW checks
    /// run in the host's `pool`, 65,536 records at a time. The chain has no tip until V's
    /// `init_active_tip` (Core `LoadChainTip`, CH1-A11). Read, decode and
    /// proof-of-work errors come in record order; then `MissingParent` and `GenesisMismatch` in
    /// id order, `NonContiguous` (Core's only structural refusal), and `HeightMismatch` in
    /// height order.
    // Core: txdb.cpp:300-344 LoadBlockIndexGuts; node/blockstorage.cpp:268-331 LoadBlockIndex;
    // validation.cpp:5337-5361 ChainstateManager::LoadBlockIndex (m_best_header)
    pub fn load<I>(
        params: Arc<ChainParams>,
        cfg: ChainConfig,
        svc: Services,
        records: I,
        pool: &rayon::ThreadPool,
    ) -> Result<Self, LoadError>
    where
        I: Iterator<Item = Result<(BlockHash, Vec<u8>), LoadError>>,
        S: Default,
    {
        Self::load_inner(params, cfg, svc, records, pool, &|| false, true)
    }

    /// [`load`](Self::load) as Core's `LoadBlockIndex` runs it at init, staged for the node's
    /// `open_block_index` (CH1-node D54): the decode and its structural refusals only, with no
    /// network check, so a single root that is another network's genesis loads (Core's
    /// `LoadBlockIndexGuts` accepts it when its proof of work passes). The node checks its
    /// block files and only then that the network's genesis is indexed
    /// (`node/blockstorage.cpp:377-390`, `node/chainstate.cpp:188-193`). `interrupted` is asked
    /// before each record is read, the first before the scan (`txdb.cpp:307-309`); `true` stops
    /// the load with [`LoadError::Interrupted`] before any later record's read error.
    pub fn load_records<I>(
        params: Arc<ChainParams>,
        cfg: ChainConfig,
        svc: Services,
        records: I,
        pool: &rayon::ThreadPool,
        interrupted: impl Fn() -> bool,
    ) -> Result<Self, LoadError>
    where
        I: Iterator<Item = Result<(BlockHash, Vec<u8>), LoadError>>,
        S: Default,
    {
        Self::load_inner(params, cfg, svc, records, pool, &interrupted, false)
    }

    /// [`load`](Self::load); `network_root` refuses a root other than the network's genesis.
    fn load_inner<I>(
        params: Arc<ChainParams>,
        cfg: ChainConfig,
        svc: Services,
        mut records: I,
        pool: &rayon::ThreadPool,
        interrupted: &dyn Fn() -> bool,
        network_root: bool,
    ) -> Result<Self, LoadError>
    where
        I: Iterator<Item = Result<(BlockHash, Vec<u8>), LoadError>>,
        S: Default,
    {
        check_config(&cfg)?;
        if cfg.mode != ChainMode::Node {
            return Err(ConfigError::WrongMode("load").into());
        }
        let hp = HeaderParams::from_chain_params(&params).map_err(ConfigError::Pow)?;
        let pow = *hp.pow();

        // 1-2: decode in parallel; ids on first mention, the hash then its prev (:314-315).
        let mut ids = Mentions::default();
        let mut loaded: Vec<Option<(BlockIndexRecord, Option<BlockId>)>> = Vec::new();
        loop {
            let mut batch = Vec::with_capacity(LOAD_BATCH);
            let mut read_err = None;
            while batch.len() < LOAD_BATCH {
                // txdb.cpp:307-309
                if interrupted() {
                    return Err(LoadError::Interrupted);
                }
                match records.next() {
                    Some(Ok(kv)) => batch.push(kv),
                    Some(Err(e)) => {
                        read_err = Some(e);
                        break;
                    }
                    None => break,
                }
            }
            let decoded: Vec<_> = pool.install(|| {
                batch.par_iter().map(|(hash, bytes)| decode_checked(hash, bytes, &pow)).collect()
            });
            for ((hash, _), rec) in batch.iter().zip(decoded) {
                let rec = rec?;
                let id = ids.mention(hash);
                let prev = (rec.header.prev_hash != BlockHash::from([0; 32]))
                    .then(|| BlockId(ids.mention(&rec.header.prev_hash)));
                loaded.resize_with(ids.hashes.len(), || None);
                loaded[id as usize] = Some((rec, prev));
            }
            if let Some(e) = read_err {
                return Err(e);
            }
            if batch.len() < LOAD_BATCH {
                break;
            }
        }
        if loaded.is_empty() {
            return Err(LoadError::Empty);
        }

        // 3: every mentioned prev is defined (D21); one root, the network's genesis unless the
        // caller checks the network itself (`load_records`).
        let hash = |id: BlockId| ids.hashes[id.0 as usize];
        let mut root = None;
        for (i, slot) in loaded.iter().enumerate() {
            let id = BlockId(i as u32);
            match slot {
                Some((_, Some(p))) if loaded[p.0 as usize].is_none() => {
                    return Err(LoadError::MissingParent {
                        hash: hash(id),
                        prev: hash(*p),
                    });
                }
                Some((_, None)) => {
                    let genesis =
                        hash(id).as_bytes() == params.consensus.hash_genesis_block.as_bytes();
                    if root.is_some() || (network_root && !genesis) {
                        return Err(LoadError::GenesisMismatch);
                    }
                    root = Some(id);
                }
                _ => {}
            }
        }
        // No root: the records only link to one another (a cycle).
        let genesis = root.ok_or(LoadError::GenesisMismatch)?;

        // 4: height order; contiguous heights; derived fields, FAILED_CHILD (:283-327).
        // Sorted as packed (height, id) pairs: a key read from `loaded` per comparison would miss
        // the cache twice.
        let mut order: Vec<(u32, u32)> = loaded
            .iter()
            .enumerate()
            .map(|(i, slot)| (slot.as_ref().expect("defined").0.height, i as u32))
            .collect();
        order.sort_unstable();
        // Core refuses a gap before it reads any parent (:297-300), so the gap scan runs first.
        if let Some(w) = order.windows(2).find(|w| w[1].0 > w[0].0 + 1) {
            return Err(LoadError::NonContiguous {
                missing_height: w[0].0 + 1,
            });
        }
        let n = order.len();
        let mut chain = HeaderChain::empty(params, cfg, svc, S::default(), hp, 0);
        for (_, i) in order {
            let id = BlockId(i);
            let (rec, prev) = loaded[i as usize].take().expect("each id once");
            // A parent not yet set has a height at or above its child's.
            let prev_failed = match prev {
                Some(p) => {
                    let Some(g) = chain.shared.arena.own_group(p) else {
                        return Err(LoadError::HeightMismatch {
                            hash: hash(id),
                        });
                    };
                    g.status.has(Status::FAILED_MASK)
                }
                None => false,
            };
            // Every loaded ancestor is resident, and heights are contiguous from genesis (the
            // check above), so derive reads nothing cold and no child height overflows.
            let imm = chain.derive(&rec.header, &hash(id), prev).map_err(|_| {
                LoadError::HeightMismatch {
                    hash: hash(id),
                }
            })?;
            if imm.height != rec.height {
                return Err(LoadError::HeightMismatch {
                    hash: hash(id),
                });
            }
            let mut group = MutableGroup {
                status: rec.status,
                n_tx: rec.n_tx,
                file: rec.file,
                data_pos: rec.data_pos.unwrap_or(0),
                undo_pos: rec.undo_pos.unwrap_or(0),
            };
            if prev_failed && !group.status.has(Status::FAILED_MASK) {
                group.status = Status(group.status.bits() | Status::FAILED_CHILD);
                chain.dirty.insert(id);
            }
            chain.shared.arena.set(id, imm, &group);
        }

        // Publish every id, then the structures keyed by them (§3.5, §3.7).
        let shared = chain.shared.clone();
        shared.index.reserve(&shared.arena, n);
        for i in 0..n as u32 {
            let id = BlockId(i);
            shared.index.insert(&shared.arena, id);
            chain.links.add(id, shared.arena.imm(id, |e| e.prev).expect("set"));
        }
        chain.next_id = n as u32;
        shared.issue(chain.next_id);
        let listed: Vec<BlockHash> = chain.checkpoints.keys().copied().collect();
        for hash in listed {
            if let Some(id) = shared.index.lookup(&shared.arena, &hash) {
                chain.note_checkpoint(&hash, shared.arena.imm(id, |e| e.height).expect("set"));
            }
        }
        // 5: Core's m_best_header: the most work among VALID_TREE entries, the lowest id on a tie
        // (CBlockIndexWorkComparator with nSequenceId 0, ids for addresses, D9).
        let mut best: Option<(Work, BlockId)> = None;
        for i in 0..n as u32 {
            let id = BlockId(i);
            let work = chain.work(id);
            if !chain.links.has_children(id) {
                chain.leaves.insert((work, Reverse(id)));
            }
            let valid =
                shared.arena.own_group(id).expect("set").status.is_valid(Status::VALID_TREE);
            if valid && best.is_none_or(|(w, _)| work > w) {
                best = Some((work, id));
            }
        }
        // R5: never null, even when every entry failed (Core's would be; D-C7-3).
        let best = best.map_or(genesis, |(_, id)| id);
        chain.best_header = Some(chain.facts_of(best).expect("every loaded entry is resident"));
        chain.publish_first(genesis).expect("the genesis entry is set");
        Ok(chain)
    }

    // ---- acceptance (Core ProcessNewBlockHeaders) ---------------------------------------------

    /// Core `ProcessNewBlockHeaders` (§5.1). Rule failures are `stop`, never `Err`. `from` is
    /// the host's: Core's acceptance does not read the peer.
    ///
    /// SPV refuses a batch over `step_headers` before any work (SB5-A11 (a)).
    ///
    /// # Errors
    ///
    /// The refusals before any work (`hashes` not matching `headers`, `StepHeaders`) leave the
    /// chain as it was. Any later `Err` is fatal: a storage failure, which has tripped the
    /// fail-stop latch, or an `Internal` where Core asserts. The batch's entries inserted
    /// before it stay in the index but are not returned, and no status snapshot is published,
    /// so the host stops (P6) rather than carrying on.
    // Core: validation.cpp:4729-4756 ChainstateManager::ProcessNewBlockHeaders
    pub fn accept(
        &mut self,
        batch: HeaderBatch,
        from: Option<PeerId>,
    ) -> Result<HeadersResult, ChainError> {
        let _ = from;
        let HeaderBatch {
            headers,
            hashes,
        } = batch;
        if headers.len() != hashes.len() {
            debug_assert!(
                false,
                "a batch with {} headers and {} hashes",
                headers.len(),
                hashes.len()
            );
            return Err(ChainError::Internal("a batch's hashes do not match its headers"));
        }
        let cap = self.cfg.feeds.step_headers;
        if self.cfg.mode == ChainMode::Spv && headers.len() > cap as usize {
            return Err(ConfigError::StepHeaders {
                headers: headers.len(),
                cap,
            }
            .into());
        }
        let before = (self.best_header, self.cl_floor);
        let (mut last_accepted, mut floor_moved, mut inserted) = (None, None, Vec::new());
        let mut stop = None;
        for (index, (header, hash)) in headers.iter().zip(&hashes).enumerate() {
            // Core: validation.cpp:4602 ASSERT_IF_DEBUG(hash == block.GetHash())
            debug_assert_eq!(*hash, header.hash(), "hashes[{index}] is the header's X11");
            let one = self.accept_header(header)?;
            // Core's walk covers the whole tree from genesis: an anchored or evicting chain
            // holds part of it (D-C4-6).
            if self.cfg.consistency_checks && !R::EVICTS && self.floor == 0 {
                self.check_block_index()?;
            }
            floor_moved = one.floor_moved.or(floor_moved);
            match one.one {
                AcceptOne::Accepted(id) => {
                    inserted.push(id);
                    last_accepted = Some(id);
                }
                AcceptOne::Known(id) => last_accepted = Some(id),
                AcceptOne::ContextSkipped => {}
                AcceptOne::Invalid(reject) => {
                    stop = Some(HeaderStop::Invalid {
                        index,
                        reject,
                    });
                    break;
                }
                AcceptOne::BelowAnchor => {
                    stop = Some(HeaderStop::BelowAnchor {
                        index,
                    });
                    break;
                }
            }
        }
        if (self.best_header, self.cl_floor) != before {
            self.publish_status();
        }
        Ok(HeadersResult {
            last_accepted,
            stop,
            floor_moved,
            inserted,
            snap: self.snapshot(),
        })
    }

    /// Core `AcceptBlockHeader` alone (CH1-node's `AcceptBlock`): no per-header
    /// `CheckBlockIndex` and no publication; the caller publishes (O-m9). The header carries its
    /// hash (CS0 deviation 7). `Err` only for a storage or internal failure.
    // Core: validation.cpp:4598-4727 ChainstateManager::AcceptBlockHeader
    pub fn accept_header(&mut self, header: &Header) -> Result<HeaderAccepted, ChainError> {
        let one = self.header_verdict(header)?;
        // 11: AcceptedBlockHeader (:4724), only for an entry AddToBlockIndex returned.
        let floor_moved = match one {
            AcceptOne::Accepted(id) => self.accepted_block_header(id)?,
            _ => None,
        };
        Ok(HeaderAccepted {
            one,
            floor_moved,
        })
    }

    /// `CheckBlockIndex` after each header of a batch; Core asserts, so a failure is a bug.
    // Core: validation.cpp:4737 CheckBlockIndex()
    fn check_block_index(&self) -> Result<(), ChainError> {
        self.check_index().map_err(|e| {
            debug_assert!(false, "CheckBlockIndex: {e:?}");
            ChainError::Internal("CheckBlockIndex found an inconsistent index")
        })
    }

    /// One header's verdict, steps 0-10 of §5.2 in Core's order.
    fn header_verdict(&mut self, header: &Header) -> Result<AcceptOne, ChainError> {
        let hash = header.hash();
        let shared = &*self.shared;
        let context = |id: BlockId| match shared.arena.imm(id, |e| e.context) {
            Some(c) => Ok(c),
            None => self.node(id).map(|n| n.context),
        };
        let reject = |result, reason: &'static str| {
            AcceptOne::Invalid(BlockValidationError::new(result, reason))
        };
        let known = shared.try_lookup(&hash)?;
        if hash.as_bytes() == self.params.consensus.hash_genesis_block.as_bytes() {
            // Core's short-cut to AddToBlockIndex, which returns the entry (D-C3-2). Only an
            // anchored chain lacks genesis, where it would insert a second root.
            return Ok(known.map_or(AcceptOne::BelowAnchor, AcceptOne::Accepted));
        }
        {
            // 1: a known header (:4605-4621); SPV skips a known context entry (D13).
            if let Some(id) = known {
                if context(id)? {
                    return Ok(AcceptOne::ContextSkipped);
                }
                let status = self.writer_status(id)?;
                return Ok(if status.has(Status::FAILED_MASK) {
                    reject(BlockValidationResult::CachedInvalid, "duplicate")
                } else if status.has(Status::CONFLICT_CHAINLOCK) {
                    reject(BlockValidationResult::ChainLock, "duplicate")
                } else {
                    AcceptOne::Known(id)
                });
            }
            // 2: CheckBlockHeader (:4623-4626).
            if let Err(e) = check_block_header(header, &self.hp, true) {
                return Ok(AcceptOne::Invalid(e));
            }
            // 3-5: the parent (:4629-4647).
            let Some(prev) = shared.try_lookup(&header.prev_hash())? else {
                return Ok(reject(BlockValidationResult::MissingPrev, "prev-blk-not-found"));
            };
            if context(prev)? {
                return Ok(AcceptOne::BelowAnchor);
            }
            // The rules below read `prev`'s height + 1 unchecked.
            let height = self.child_height(prev, self.height_of(prev)?)?;
            let prev_status = self.writer_status(prev)?;
            if prev_status.has(Status::FAILED_MASK) {
                return Ok(reject(BlockValidationResult::InvalidPrev, "bad-prevblk"));
            }
            if prev_status.has(Status::CONFLICT_CHAINLOCK) {
                return Ok(reject(BlockValidationResult::ChainLock, "bad-prevblk-chainlock"));
            }
            // 6: ContextualCheckBlockHeader (:4649-4652).
            if let Err(e) = self.contextual_check(header, prev)? {
                return Ok(AcceptOne::Invalid(e));
            }
            // 7: descends from a block in m_failed_blocks (:4673-4691).
            if !prev_status.is_valid(Status::VALID_SCRIPTS) && self.mark_failed_descent(prev)? {
                return Ok(reject(BlockValidationResult::InvalidPrev, "bad-prevblk"));
            }
            // 8: conflicts with the ChainLock floor: inserted as conflicting, no signal (n1).
            if self.has_conflicting_chainlock(height, &hash)? {
                let status = Status(Status::CONFLICT_CHAINLOCK);
                self.add_new(header.raw(), &hash, Some(prev), status)?;
                return Ok(reject(BlockValidationResult::ChainLock, "bad-chainlock"));
            }
            // 10: AddToBlockIndex (:4701).
            let id = self.add_new(header.raw(), &hash, Some(prev), Status(Status::VALID_TREE))?;
            Ok(AcceptOne::Accepted(id))
        }
    }

    /// `ContextualCheckBlockHeader` for `header` on `prev` (§5.2): the node reads its complete
    /// arena; SPV reads only the `RuleContext` it refreshes for `prev` (§8.2). `Err` when a rule
    /// read outside that context: an internal failure, with nothing inserted and no blame.
    // Core: validation.cpp:4469-4522 ContextualCheckBlockHeader
    fn contextual_check(
        &mut self,
        header: &Header,
        prev: BlockId,
    ) -> Result<Result<(), BlockValidationError>, ChainError> {
        let now = self.svc.clock.node_now().secs();
        if self.cfg.mode == ChainMode::Node {
            return Ok(self.check_on(header, prev, self, now));
        }
        let prev_height = self.height_of(prev)?;
        let mut cache = std::mem::take(&mut self.rules);
        let envelope = cache.envelope(self.hp.pow(), prev_height);
        let arena = &self.shared.arena;
        let link = |id| {
            let n = self.node(id)?;
            Ok(PathEntry {
                prev: n.prev,
                cold: (!n.resident).then(|| node_ctx(&n)),
            })
        };
        let time = |id| arena.imm(id, |e| e.header.time);
        // On an error `self.rules` stays the default `take` left: the next header rebuilds.
        cache.refresh(
            prev,
            prev_height,
            envelope,
            (self.floor, self.residency_epoch),
            link,
            time,
        )?;
        let ctx = RuleContext::new(arena, &cache);
        let verdict = self.check_on(header, prev, &ctx, now);
        let outside = ctx.read_outside();
        self.rules = cache;
        match outside {
            true => Err(ChainError::Internal("rule read outside its envelope")),
            false => Ok(verdict),
        }
    }

    /// CH1-A10: `ContextualCheckBlockHeader` for `header` on `prev` without inserting it, as
    /// `TestBlockValidity` runs it on its dummy index (`validation.cpp:4953`). It inserts nothing
    /// and changes no state. `now` is `NodeClock::now()` in seconds, the `time-too-new` reference
    /// (`:4509`): the clock this chain's own accept reads. Node mode only: SPV keeps no arena to
    /// check a header off its path (`ConfigError::WrongMode`); an unknown `prev` is
    /// [`ChainError::UnknownBlock`].
    // Core: validation.cpp:4469-4522 ContextualCheckBlockHeader
    pub fn check_header_at(
        &self,
        prev: BlockId,
        header: &Header,
        now: i64,
    ) -> Result<Result<(), BlockValidationError>, ChainError> {
        self.require_mode(ChainMode::Node, "check_header_at")?;
        self.shared.arena.imm(prev, |_| ()).ok_or(ChainError::UnknownBlock(prev))?;
        Ok(self.check_on(header, prev, self, now))
    }

    /// `ContextualCheckBlockHeader` with `chain` as every view's ancestry and `now` as
    /// `NodeClock::now()` (`validation.cpp:4509`).
    fn check_on(
        &self,
        header: &Header,
        prev: BlockId,
        chain: &dyn ChainAncestry,
        now: i64,
    ) -> Result<(), BlockValidationError> {
        let prev_ctx = chain.header(prev);
        let height = prev_ctx.height + 1;
        let deployments = HeaderDeployments {
            params: &self.params,
            chain,
        };
        let ctx = ConsensusCtx {
            params: &self.params,
            height,
            prev: Some(prev),
            chain,
            deployments: &deployments,
            checkpoints: self,
            bls: bls_ctx_for_block(&self.params, height),
            median_time_past: prev_ctx.mtp,
            header_check_time: now,
        };
        contextual_check_block_header(header, &ctx, &self.hp)
    }

    /// Core's `m_failed_blocks` loop: for the first failed block (id order, D9) that is an
    /// ancestor of `prev`, marks `prev` down to its child `FAILED_CHILD` and dirty, and returns
    /// true. SPV's list is always empty.
    // Core: validation.cpp:4673-4691
    fn mark_failed_descent(&mut self, prev: BlockId) -> Result<bool, ChainError> {
        let height = |id: BlockId| self.shared.arena.imm(id, |e| e.height).expect("resident");
        let Some(failed) =
            self.failed.iter().find(|&f| ChainAncestry::ancestor(self, prev, height(f)) == Some(f))
        else {
            return Ok(false);
        };
        if !self.writer_status(failed)?.has(Status::FAILED_VALID) {
            debug_assert!(false, "m_failed_blocks entry {failed:?} without BLOCK_FAILED_VALID");
            return Err(ChainError::Internal("m_failed_blocks holds a block not FAILED_VALID"));
        }
        let mut walk = prev;
        while walk != failed {
            self.set_status(walk, |s| Status(s.bits() | Status::FAILED_CHILD))?;
            self.mark_dirty(walk);
            walk =
                self.shared.arena.imm(walk, |e| e.prev).flatten().expect("below the failed block");
        }
        Ok(true)
    }

    /// Core `Chainlocks::AcceptedBlockHeader`: a late header for the best ChainLock becomes the
    /// floor, unless its height differs (`:161-166`). Not gated by the spork (§7.1).
    /// A resent genesis can be cold in a windowed chain.
    // Core: chainlock/chainlock.cpp:152-172
    fn accepted_block_header(&mut self, id: BlockId) -> Result<Option<BlockId>, ChainError> {
        let Some(best) = self.cl_best else {
            return Ok(None);
        };
        let at = self.block_ref_of(id)?;
        if (at.hash, at.height) != (best.hash, best.height) {
            return Ok(None);
        }
        self.cl_floor = Some(at);
        Ok(Some(id))
    }

    /// Node: publishes a status snapshot (epoch + 1, same `tip_seq`) after a best-header, floor
    /// or spork-state change (§9.1).
    /// In SPV it does nothing: status changes ride on the step's single snapshot. Inside a hold
    /// it does nothing either: the release publishes (CH1-A13). Inside `enforce_block` it is a
    /// host callback's reentrant call, which panics in debug builds (§9.1, A-39).
    pub fn publish_status(&mut self) {
        debug_assert!(self.composite.is_none(), "publish_status inside enforce_block (D42)");
        if self.cfg.mode != ChainMode::Node || self.held || self.composite.is_some() {
            return;
        }
        let mut st = self.shared.readers.tip().state.lock();
        self.publish_locked(&mut st, Fanout::None);
    }

    // ---- ChainLocks: the floor's conflict checks (the rest is in `enforce.rs`) ------------------

    /// Core `HasChainLock` (`chainlock/chainlock.cpp:49-71`), gated by the live spork.
    pub fn has_chainlock(&self, height: u32, hash: &BlockHash) -> Result<bool, ChainError> {
        Ok(self.locked_hash_at(height)? == Some(*hash))
    }

    /// Core `HasConflictingChainLock` (`chainlock/chainlock.cpp:73-96`), gated by the live
    /// spork.
    pub fn has_conflicting_chainlock(
        &self,
        height: u32,
        hash: &BlockHash,
    ) -> Result<bool, ChainError> {
        Ok(self.locked_hash_at(height)?.is_some_and(|locked| locked != *hash))
    }

    /// The hash the floor locks at `height`: `None` while the live spork is off, with no floor,
    /// or above the floor; else the floor's ancestor at `height`, which Core asserts exists.
    // Core: chainlock/chainlock.cpp:49-96 HasChainLock, HasConflictingChainLock
    fn locked_hash_at(&self, height: u32) -> Result<Option<BlockHash>, ChainError> {
        let Some(floor) = self.cl_floor.filter(|_| self.svc.sporks.chainlocks_enabled()) else {
            return Ok(None);
        };
        if height > floor.height {
            return Ok(None);
        }
        // D-C3-10: through AnchoredAncestry, so an evicted floor resolves cold. No header below
        // the anchor is ever checked, so the floor's history is always held.
        let ancestor = match AnchoredAncestry::ancestor(self, floor.id, height) {
            Ok(a) => {
                a.ok_or(ChainError::Internal("the floor has no ancestor at a lower height"))?
            }
            Err(AncestryError::Storage(e)) => return Err(e.into()),
            Err(AncestryError::BelowFloor {
                ..
            }) => {
                return Err(ChainError::Internal("a ChainLock check below the floor"));
            }
        };
        Ok(Some(self.node(ancestor)?.hash))
    }

    // ---- tip writers: one family, one private step (D4) ---------------------------------------

    /// Core `LoadChainTip`: builds the active chain and publishes `tip_seq = 0` with no event.
    /// SPV constructors run it with their root; the node runs it once V knows its tip (CH1-A11).
    /// `seq` restarts at 1, so CH1-node calls it only before any tip event is handed out (SB5).
    /// Entries that left the window are read cold, down to the lowest held entry (genesis, or
    /// an anchored chain's floor), which is the active chain's base (§8.4). SPV's first step
    /// windows it (DEC-174). `ChainError::BelowAnchor` for a context entry (§8.4): the anchor
    /// is on every active chain (§3.6), as in [`HeaderChain::tip_target`].
    pub fn init_active_tip(&mut self, tip: BlockId) -> Result<(), ChainError> {
        self.refuse_in_hold("init_active_tip inside a hold")?;
        if tip.0 >= self.next_id {
            return Err(ChainError::UnknownBlock(tip));
        }
        let facts = self.facts_of(tip)?;
        if facts.at.height < self.shared.anchor {
            return Err(ChainError::BelowAnchor);
        }
        let mut path = vec![tip];
        let mut lowest = self.node(tip)?;
        while let Some((p, n)) = self.parent(&lowest)? {
            path.push(p);
            lowest = n;
        }
        if lowest.height != self.floor {
            debug_assert!(false, "init_active_tip: the walk ended at {}", lowest.height);
            return Err(ChainError::Internal("init_active_tip: the walk ended off the floor"));
        }
        let mut active = ActiveChain::starting_at(self.shared.anchor);
        for &id in path.iter().rev() {
            active.push(id);
        }
        self.active = active;
        self.tip = Some(facts);
        *self.pending_plan.lock() = None;
        let tip_ref = facts.at;
        self.last_seq = 0;
        match self.shared.readers.tip.get() {
            None => self.set_tip_cell(tip_ref),
            Some(cell) => {
                let mut st = cell.state.lock();
                st.tip = tip_ref;
                st.next_seq = 1;
                st.pre_genesis = false;
                self.publish_locked(&mut st, Fanout::None);
            }
        }
        Ok(())
    }

    /// Node: the `ConnectTip` boundary; `prev(id)` must be the tip, or `id` the genesis entry
    /// while the node has no tip (CH1-A11; its step is `Connected(genesis, 1)`). One critical
    /// section of the tip lock: the step, then its snapshot (§9.1).
    // Core: validation.cpp:3400 assert(pindexNew->pprev == m_chain.Tip())
    pub fn connect_tip(&mut self, id: BlockId) -> Result<TipStep, ChainError> {
        self.publish_step("connect_tip inside a hold", |c, st| c.connect_step(st, id))
    }

    /// Node: the `DisconnectTip` boundary. The tip must be above genesis. Inside
    /// `enforce_block` (the host's `DisconnectTip`) the step takes its `seq` under the held
    /// mutex, hands its event to the feeds (D43) and stores no snapshot: `enforce_block` stores
    /// one when it returns (§9.1). A step that unwinds puts the state back first, so the
    /// enforcement guard restores it (D-C6-1).
    pub fn disconnect_tip(&mut self) -> Result<TipStep, ChainError> {
        if let Some(mut st) = self.composite.take() {
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let step = self.disconnect_step(&mut st)?;
                feed::after_step(&mut st, self.shared.readers.tip(), step);
                Ok(step)
            }));
            self.composite = Some(st);
            return step.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        }
        self.publish_step("disconnect_tip inside a hold", |c, st| c.disconnect_step(st))
    }

    /// One step in its own critical section of the tip lock, then its snapshot (§9.1).
    fn publish_step(
        &mut self,
        what: &'static str,
        step: impl FnOnce(&mut Self, &mut TipState) -> Result<TipStep, ChainError>,
    ) -> Result<TipStep, ChainError> {
        let mut st = self.lock_tip(what)?;
        let step = step(self, &mut st)?;
        self.publish_locked(&mut st, Fanout::One(step));
        Ok(step)
    }

    /// Publishes the writer's state under the tip lock, `tip_seq` the last step's: also after
    /// an abandoned hold, whose steps the snapshot then shows (F2).
    /// Then SB5's fanout of `ev`, still under the lock (SB5-A14).
    #[inline(always)]
    fn publish_locked(&self, st: &mut TipState, ev: Fanout<'_>) -> Arc<ChainReadSnapshot> {
        let cell = self.shared.readers.tip();
        let snap = st.publish(&cell.published, self.build_snapshot(st.next_seq - 1));
        #[cfg(all(test, not(loom)))]
        AFTER_PUBLISH.with_borrow_mut(|hook| {
            if let Some(h) = hook.as_mut() {
                h(&snap);
            }
        });
        feed::after_publish(st, cell, &snap, ev);
        snap
    }

    /// `connect_tip`'s step under the tip lock, published by the caller.
    fn connect_step(&mut self, st: &mut TipState, id: BlockId) -> Result<TipStep, ChainError> {
        self.require_mode(ChainMode::Node, "connect_tip")?;
        feed::before_step(st)?;
        let prev = self.shared.arena.imm(id, |e| e.prev).ok_or(ChainError::UnknownBlock(id))?;
        if prev != (!st.pre_genesis).then_some(st.tip.id) {
            return Err(ChainError::Internal("connect_tip: prev is not the active tip"));
        }
        let facts = self.facts_of(id)?;
        let step = self.step(st, Leg::Connect(facts.at));
        self.tip = Some(facts);
        Ok(step)
    }

    /// `disconnect_tip`'s step under the tip lock, published by the caller.
    fn disconnect_step(&mut self, st: &mut TipState) -> Result<TipStep, ChainError> {
        self.require_mode(ChainMode::Node, "disconnect_tip")?;
        feed::before_step(st)?;
        let n = self.active.len();
        if n <= self.active.base() + 1 {
            return Err(ChainError::Internal("disconnect_tip: the tip is the chain's floor"));
        }
        let at = |h| self.active.get(h).expect("an active height");
        let (old, facts) = (self.block_ref_of(at(n - 1))?, self.facts_of(at(n - 2))?);
        let step = self.step(
            st,
            Leg::Disconnect {
                old,
                tip: facts.at,
            },
        );
        self.tip = Some(facts);
        Ok(step)
    }

    /// Opens a hold (CH1-A1): one critical section of the tip lock owned by the writer's frame
    /// across several tip steps, as Core keeps `cs_main` across `ActivateBestChain`'s inner
    /// loop (`validation.cpp:3763-3827`) and each `InvalidateBlock` iteration (`:3942-4003`).
    /// Each step takes its own `seq` (F2); snapshot readers see the pre-hold state until
    /// [`Hold::release`] publishes once. The guard lives in the returned `Hold`, never in the
    /// chain, so `HeaderChain` stays `Send`.
    ///
    /// # Panics
    ///
    /// Inside another hold, where it would deadlock.
    pub fn hold_tip(&mut self) -> Hold<'_, S, R> {
        let guard = self.lock_tip("hold_tip inside a hold").unwrap_or_else(|e| panic!("{e}"));
        self.held = true;
        self.feed_shared().hold_opened();
        Hold {
            chain: self,
            guard: HoldGuard::new(guard),
        }
    }

    /// `ChainError::Internal(what)` for a call that takes the tip lock while a hold owns it,
    /// which would deadlock (parking_lot's mutex is not reentrant).
    fn refuse_in_hold(&self, what: &'static str) -> Result<(), ChainError> {
        match self.held || self.composite.is_some() {
            true => Err(ChainError::Internal(what)),
            false => Ok(()),
        }
    }

    /// The tip lock for a writer, refused inside a hold. The steppers take it here;
    /// `publish_status` and `init_active_tip` check `held` first themselves.
    fn lock_tip(
        &self,
        what: &'static str,
    ) -> Result<ArcMutexGuard<RawMutex, TipState>, ChainError> {
        self.refuse_in_hold(what)?;
        Ok(self.shared.readers.tip().state.lock_arc())
    }

    /// The active diff to `to`: from the tip down to `FindFork(to)`, then up to `to` (Core's
    /// `ActivateBestChainStep` order). SPV records it as the pending plan for
    /// [`HeaderChain::set_active_tip`]. Unbounded: an SPV step sizes its change with
    /// [`HeaderChain::tip_target`] and moves the tip in bounded sub-steps (DEC-187, SB4 D47).
    // Core: validation.cpp ActivateBestChainStep (m_chain.FindFork(pindexMostWork))
    pub fn tip_plan(&self, to: BlockId) -> Result<TipPlan, ChainError> {
        let plan = self.plan_legs(&self.tip_target(to)?, to)?;
        if self.cfg.mode == ChainMode::Spv {
            *self.pending_plan.lock() = Some(plan.clone());
        }
        Ok(plan)
    }

    /// The shape of the tip change to `target`, with no leg materialized (SB5-A11 (c)): the
    /// fork is the last common ancestor of the writer's tip and `target` (Core's
    /// `FindFork`), found over skip links. `ChainError::BelowAnchor` for a fork below the
    /// anchor: SPV's context entries (§8.4) are never the tip, and the anchor stays on the
    /// active chain, as `disconnect_tip` keeps the node's floor (§3.6, §10.1 n6). From genesis
    /// the anchor is genesis, so this never refuses.
    // Core: chain.cpp:166-181 LastCommonAncestor
    pub fn tip_target(&self, target: BlockId) -> Result<TipTarget, ChainError> {
        let (tip, _) = self.private_tip().ok_or(ChainError::Internal("tip_target: no tip yet"))?;
        if target.0 >= self.next_id {
            return Err(ChainError::UnknownBlock(target));
        }
        let height = self.height_of(target)?;
        let fork = self.fork_of(tip.id, target)?;
        if fork.height < self.shared.anchor {
            return Err(ChainError::BelowAnchor);
        }
        Ok(TipTarget {
            fork,
            d: tip.height - fork.height,
            c: height - fork.height,
        })
    }

    /// The plan to `sub_target` when it has at most `max_legs` steps, legs at exact capacity
    /// (SB5-A11 (c)); SB5's segmented step picks sub-targets that fit (SB5 §4.8). Not recorded
    /// as the pending plan.
    pub fn tip_plan_bounded(
        &self,
        sub_target: BlockId,
        max_legs: u32,
    ) -> Result<TipPlan, ChainError> {
        let t = self.tip_target(sub_target)?;
        if t.d.checked_add(t.c).is_none_or(|n| n > max_legs) {
            return Err(ChainError::Internal("tip_plan_bounded: more steps than the bound"));
        }
        self.plan_legs(&t, sub_target)
    }

    /// The last common ancestor of `a` and `b` (Core's `FindFork` when `a` is the tip), over
    /// skip links, resident or cold; a store error trips the latch (SB5 §4.8 m3).
    // Core: chain.cpp:166-181 LastCommonAncestor
    fn fork_of(&self, a: BlockId, b: BlockId) -> Result<BlockRef, ChainError> {
        let link = |id| self.node(id).map(|n| n.link());
        let fork = walk::last_common_ancestor(a, b, link, |id| self.off_height(id))?
            .ok_or(ChainError::Internal("no fork with the active chain"))?;
        self.block_ref_of(fork)
    }

    /// `t`'s legs: tip down to the fork, then fork up to `to` (`ActivateBestChainStep`'s order).
    /// Both are read by ancestry, so SPV reads the legs that left the window, or SPV's window
    /// over the active chain (DEC-174), from the store.
    fn plan_legs(&self, t: &TipTarget, to: BlockId) -> Result<TipPlan, ChainError> {
        let tip = self.tip.ok_or(ChainError::Internal("tip plan: no tip yet"))?.at.id;
        let (disconnect, below) = self.walk_down(tip, t.d)?;
        let (mut connect, below_connects) = self.walk_down(to, t.c)?;
        connect.reverse();
        if (below, below_connects) != (Some(t.fork.id), Some(t.fork.id)) {
            // `fork_of` follows skips, the legs follow `prev`s: only a record whose skip lands at
            // the right height on another branch, which no walk can see (`walk::ancestor`),
            // splits them.
            return Err(match R::EVICTS {
                true => self
                    .shared
                    .corrupt(format!(
                        "tip plan legs end above {below:?} and {below_connects:?}, not the fork {:?}",
                        t.fork.id
                    ))
                    .into(),
                false => ChainError::Internal("tip plan: a leg misses the fork"),
            });
        }
        Ok(TipPlan {
            fork: t.fork,
            disconnect,
            connect,
        })
    }

    /// `n` refs down the parent links from `from`, `from` first, resident or cold, and the id
    /// below the last.
    fn walk_down(
        &self,
        from: BlockId,
        n: u32,
    ) -> Result<(Vec<BlockRef>, Option<BlockId>), ChainError> {
        let mut out = Vec::with_capacity(n as usize);
        if n == 0 {
            return Ok((out, Some(from)));
        }
        let (mut id, mut node) = (from, self.node(from)?);
        loop {
            out.push(BlockRef {
                id,
                hash: node.hash,
                height: node.height,
            });
            if out.len() == n as usize {
                return Ok((out, node.prev));
            }
            match self.parent(&node)? {
                Some(p) => (id, node) = p,
                None => return Ok((out, None)),
            }
        }
    }

    /// Composite tip change. Node: one `disconnect_tip`/`connect_tip` per step, each published.
    /// SPV: stage 4 of §9.1's recipe: runs the pending plan, refuses a target that conflicts
    /// with the floor (DEC-136, SB4 C6), publishes once.
    /// Node: a step refused for a pending fanout (SB5 §3.3) stops the change there; the steps
    /// taken stay published. Callers drain and call again.
    pub fn set_active_tip(&mut self, to: BlockId) -> Result<TipChange, ChainError> {
        self.refuse_in_hold("set_active_tip inside a hold")?;
        match self.cfg.mode {
            ChainMode::Node => {
                let plan = self.tip_plan(to)?;
                for _ in &plan.disconnect {
                    self.disconnect_tip()?;
                }
                for b in &plan.connect {
                    self.connect_tip(b.id)?;
                }
                Ok(TipChange {
                    disconnected: plan.disconnect,
                    connected: plan.connect,
                    last_seq: self.last_seq,
                    snap: self.snapshot(),
                })
            }
            ChainMode::Spv => {
                self.refuse_floor_conflict(to)?;
                // The plan must still start at the tip and end at `to` (§9.1 stage 4).
                let tip = self.active.get(self.active.len() - 1);
                let plan = self.pending_plan.lock().take().filter(|p| {
                    p.disconnect.first().unwrap_or(&p.fork).id == tip.expect("a tip")
                        && p.connect.last().unwrap_or(&p.fork).id == to
                });
                let plan = plan.ok_or(ChainError::Internal(
                    "set_active_tip: no pending plan from the tip to this target",
                ))?;
                // A cold end tip is read before the critical section.
                let end = self.plan_end(&plan)?;
                let mut st = self.lock_tip("set_active_tip inside a hold")?;
                feed::before_step(&st)?;
                let first_seq = st.next_seq;
                // Infallible from here; a panic (a broken invariant) leaves the writer
                // half-stepped, so it trips the latch before the guard drops, as the node's
                // enforcement composite does (Opus r1 n-4).
                let critical = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.run_plan(&mut st, &plan, end);
                    self.stage4_effects();
                    let ev = Fanout::Composite {
                        first_seq,
                        disconnected: &plan.disconnect,
                        connected: &plan.connect,
                    };
                    self.publish_locked(&mut st, ev)
                }));
                let snap = critical.unwrap_or_else(|panic| {
                    self.svc.fail.read_failed("set_active_tip unwound inside the tip lock");
                    std::panic::resume_unwind(panic)
                });
                Ok(TipChange {
                    disconnected: plan.disconnect,
                    connected: plan.connect,
                    last_seq: snap.tip_seq,
                    snap,
                })
            }
        }
    }

    /// The facts of `plan`'s end tip, read before [`HeaderChain::run_plan`] so a failed read
    /// leaves the writer as it was.
    fn plan_end(&self, plan: &TipPlan) -> Result<HeadFacts, ChainError> {
        self.facts_of(plan.connect.last().unwrap_or(&plan.fork).id)
    }

    /// `plan`'s steps, its disconnects then its connects, ending at `end`. The legs carry their
    /// refs, as those below the window are cold (§13.3).
    fn run_plan(&mut self, st: &mut TipState, plan: &TipPlan, end: HeadFacts) {
        for (i, &old) in plan.disconnect.iter().enumerate() {
            let tip = *plan.disconnect.get(i + 1).unwrap_or(&plan.fork);
            self.step(
                st,
                Leg::Disconnect {
                    old,
                    tip,
                },
            );
        }
        for &b in &plan.connect {
            self.step(st, Leg::Connect(b));
        }
        self.tip = Some(end);
    }

    /// The one private tip step (D4), under the tip lock: moves the active chain and the tip,
    /// then takes the next `seq` (F2).
    #[inline(always)]
    fn step(&mut self, st: &mut TipState, leg: Leg) -> TipStep {
        let step = match leg {
            Leg::Connect(block) => {
                self.active.push(block.id);
                st.tip = block;
                st.pre_genesis = false;
                TipStep::Connected {
                    block,
                    seq: st.take_seq(),
                }
            }
            Leg::Disconnect {
                old,
                tip,
            } => {
                // A disconnect below the window first re-extends the chain by the leg's parent,
                // read by ancestry with the plan (DEC-174).
                if tip.height < self.active.base() {
                    self.active.extend_down(tip.id);
                }
                let popped = self.active.pop().expect("a tip above the floor");
                debug_assert_eq!((popped, self.active.get(tip.height)), (old.id, Some(tip.id)));
                st.tip = tip;
                TipStep::Disconnected {
                    block: old,
                    seq: st.take_seq(),
                }
            }
        };
        self.last_seq = step.seq();
        step
    }

    // ---- the writer's private active chain (Core m_chain under cs_main; CH1-A2) ---------------

    /// The writer's tip and the `seq` of its last step: Core's `m_chain.Tip()`, current inside a
    /// hold while the published snapshot lags. `None` before the node has a tip (CH1-A11).
    pub fn private_tip(&self) -> Option<(BlockRef, u64)> {
        Some((self.tip?.at, self.last_seq))
    }

    /// `m_chain.Contains(id)` on the writer's active chain; `false` for an entry that left the
    /// window (D-C4-12).
    // Core: chain.h:465-468 CChain::Contains
    pub fn active_contains_private(&self, id: BlockId) -> bool {
        self.shared.arena.imm(id, |e| e.height).is_some_and(|h| self.active.get(h) == Some(id))
    }

    /// `m_chain[h]`: the writer's active entry at height `h`; `None` below SPV's window
    /// (DEC-174).
    pub fn ancestor_private(&self, h: u32) -> Option<BlockId> {
        self.active.get(h)
    }

    /// `m_chain.FindFork(id)` on the writer's active chain; `None` with no tip, as Core's.
    // Core: chain.cpp:61-70 CChain::FindFork
    pub fn find_fork_private(&self, id: BlockId) -> Option<BlockId> {
        self.active_view().find_fork(id, &self.reader())
    }

    /// Core `IsInitialBlockDownload` on the writer's tip, with the readers' latch (CH1-A8): the
    /// value Core computes under `cs_main`, current inside a hold. `now` as for
    /// [`ChainReader::is_initial_block_download`].
    pub fn is_ibd_private(&self, now: impl Into<NodeTime>, loading_blocks: bool) -> bool {
        let tip = self.tip.map(|t| (t.work, t.time));
        self.shared.readers.is_ibd(tip, now.into(), loading_blocks)
    }

    /// Every id the arena holds, in id order: Core's whole-index scans (CH1-A3).
    pub fn ids(&self) -> impl Iterator<Item = BlockId> + '_ {
        (0..self.next_id).map(BlockId).filter(|&id| self.shared.arena.imm(id, |_| ()).is_some())
    }

    /// A view of the writer's active chain.
    fn active_view(&self) -> ActiveChainView {
        ActiveChainView {
            chain: self.active.clone(),
        }
    }

    /// The snapshot of the current active chain and best header, and of every status written
    /// so far (CH1-node D63), at the status journals' next epoch (D68): build only snapshots
    /// that are published, in order.
    fn build_snapshot(&self, tip_seq: u64) -> ChainReadSnapshot {
        // First: its atomics in the middle of the fields' loads spilled them all (PF0).
        let epoch = self.journals.next_build();
        let active = self.active_view();
        // The node's placeholder before it has a tip is its genesis entry, always resident.
        let tip = match self.tip {
            Some(t) => t,
            None => self.facts_of(self.genesis()).expect("the root entry is resident"),
        };
        debug_assert_eq!(active.chain.is_empty(), self.tip.is_none());
        let best = self.best_header.expect("R5: every constructor sets the best header");
        let tip_height = tip.at.height;
        ChainReadSnapshot {
            tip: TipFacts {
                tip: tip.at.id,
                tip_work: tip.work,
                tip_time: tip.time,
                tip_height,
                tip_hash: tip.at.hash,
                best_header: best.at.id,
                best_header_time: best.time,
                best_header_work: best.work,
                best_header_height: best.at.height,
                best_header_hash: best.at.hash,
            },
            tip_seq,
            active,
            bls: bls_ctx_for_block(&self.params, tip_height + 1),
            cl_floor: self.cl_floor,
            cl_enabled_observed: self.cl_enabled_observed,
            epoch,
        }
    }

    /// A resident entry as CH2 names it.
    fn block_ref(&self, id: BlockId) -> BlockRef {
        self.shared
            .arena
            .imm(id, |e| BlockRef {
                id,
                hash: e.hash,
                height: e.height,
            })
            .expect("resident entry")
    }

    /// `ConfigError::WrongMode` unless the chain runs in `mode`.
    fn require_mode(&self, mode: ChainMode, what: &'static str) -> Result<(), ChainError> {
        match self.cfg.mode == mode {
            true => Ok(()),
            false => Err(ConfigError::WrongMode(what).into()),
        }
    }

    // ---- best header (Core m_best_header) and V's hooks ----------------------------------------

    /// Never null after construction (R5).
    pub fn best_header(&self) -> BlockId {
        self.best_header.expect("R5: every constructor sets the best header").at.id
    }

    /// `validation.cpp:3951`, `:3969`, `:4089`, `:4107`: if `marked` is `best_header`, its prev.
    /// The genesis entry stays, as R5 keeps `best_header` non-null (D-C7-3). `true` when
    /// `marked` was the best header, so V knows Core's branch ran (its `m_best_invalid`
    /// update, `:3952`) even at genesis.
    /// V's hook ([`NodeIndex::best_header_walk_back`]): node entries are resident. SPV's
    /// walk-backs run inside `mark_conflicting_block`, fallibly.
    pub(crate) fn best_header_walk_back(&mut self, marked: BlockId) -> bool {
        debug_assert!(!R::EVICTS, "V's hook; SPV walks back inside MarkConflictingBlock");
        self.walk_back_best_header(marked).expect("node entries are resident")
    }

    /// `validation.cpp:2043-2044`: if `invalid` is an ancestor-or-self of `best_header`, the
    /// writer's active tip (CH1-A2; R5 keeps it when there is none). V's hook
    /// ([`NodeIndex::best_header_invalid_chain`]).
    pub(crate) fn best_header_invalid_chain(&mut self, invalid: BlockId) {
        let Some(height) = self.shared.arena.imm(invalid, |e| e.height) else {
            return;
        };
        let reader = self.reader();
        if reader.ancestor(self.best_header(), height) == Some(invalid) && self.tip.is_some() {
            self.best_header = self.tip;
        }
    }

    // ---- status and block data (CH1-node) -----------------------------------------------------

    /// A seqlocked write of the mutable group; does not mark the entry dirty. SPV status
    /// changes go through the overlay instead (D36), which would overwrite this one at stage 4.
    /// CH1-node's through [`NodeIndex::modify`]. A changed status journals the old one first,
    /// which readers of a snapshot built before the write see in its place (CH1-node D63).
    pub(crate) fn modify(&mut self, id: BlockId, f: impl FnOnce(&mut MutableGroup)) {
        debug_assert!(!self.overlay.contains_key(&id), "modify under an overlay mark: set_status");
        let arena = &self.shared.arena;
        let mut g = arena.own_group(id).unwrap_or_else(|| panic!("modify: no entry {}", id.0));
        let old = g.status;
        f(&mut g);
        if g.status != old {
            self.journals.record(id, old, &self.shared.readers.journals);
        }
        #[cfg(test)]
        crate::status_journal::tests::hook(crate::status_journal::tests::Site::Writer);
        arena.write_group(id, &g);
    }

    /// A status change after insertion (§3.7, D36): `f` maps the writer's status (overlay first)
    /// to the new one. The node writes the entry under its seqlock, not dirty (D15). SPV writes
    /// the overlay: readers see it at stage 4 and the store at stage 3 (SB5-A13). An unchanged
    /// status writes nothing, as Core's `nStatus |=` of a set bit changes nothing: a re-mark
    /// leaves SPV's step empty (n8).
    pub(crate) fn set_status(
        &mut self,
        id: BlockId,
        f: impl FnOnce(Status) -> Status,
    ) -> Result<(), ChainError> {
        let old = self.writer_status(id)?;
        let status = f(old);
        if status == old {
            return Ok(());
        }
        match self.cfg.mode {
            ChainMode::Node => self.modify(id, |g| g.status = status),
            ChainMode::Spv => {
                self.overlay.insert(
                    id,
                    Mark {
                        status,
                        applied: false,
                    },
                );
            }
        }
        Ok(())
    }

    /// The writer's status read (§3.7's precedence): the overlay, then the resident entry, then
    /// the committed record (through SB5's cold cache, §5.2).
    pub(crate) fn writer_status(&self, id: BlockId) -> Result<Status, ChainError> {
        if let Some(m) = self.overlay.get(&id) {
            return Ok(m.status);
        }
        match self.shared.arena.own_group(id) {
            Some(g) => Ok(g.status),
            None if R::EVICTS && id.0 < self.next_id => Ok(self.node(id)?.status),
            None => Err(ChainError::UnknownBlock(id)),
        }
    }

    /// SPV: the overlay's marks, for the step's status rows at stage 3 (SB5-A13).
    pub(crate) fn overlay_marks(&self) -> impl ExactSizeIterator<Item = (BlockId, Status)> + '_ {
        self.overlay.iter().map(|(&id, m)| (id, m.status))
    }

    /// SPV stage 4, under the tip lock and immediately before the snapshot store (D36, SB5-A11
    /// (e)): the overlay's marks reach the resident seqlocked groups, then eviction. A cold
    /// mark's row was written at stage 3; SB5's cold cache drops it at stage 5 (SB5-A10). Every
    /// mark is applied from here on, which stage 5 drops (mi-2).
    fn stage4_effects(&mut self) {
        let arena = &self.shared.arena;
        for (&id, mark) in &mut self.overlay {
            if let Some(mut g) = arena.own_group(id) {
                g.status = mark.status;
                arena.write_group(id, &g);
            }
            mark.applied = true;
        }
        if R::EVICTS {
            self.evict();
            self.window_active();
        }
    }

    /// SPV's window over the writer's active chain (DEC-174): its base moves up to
    /// `max(floor, tip − w)`, `w` the configured window, and the leaves wholly below its leaf go.
    /// Stage 4 calls it after every committed step, right after eviction for the same tip.
    /// Inside a hold, SB5's `activate_segmented` calls it through [`Hold::chain`] after each
    /// segment's `Hold::set_active_tip`, the last one's included; eviction then follows at the
    /// release, and the heights between resolve by ancestry meanwhile. The base only rises
    /// here: after a disconnect below the window it stays at the fork until the tip is `w` past
    /// it. A no-op in the node and in any unwindowed or non-evicting chain.
    pub fn window_active(&mut self) {
        if let Some(low) = self.window_low().filter(|_| R::EVICTS) {
            self.active.set_base(low);
        }
    }

    /// SPV's window edge `tip − w`, at or above the floor: the lowest height eviction keeps.
    fn window_low(&self) -> Option<u32> {
        let tip = self.active.len().checked_sub(1)?;
        Some(tip.saturating_sub(self.cfg.window?).max(self.floor))
    }

    /// SPV eviction (§13.3, SB0 §4.4): every entry of any branch below `tip.height − w` leaves
    /// the arena and the resident index; its chunk is released once all its entries have. Runs
    /// after the step's commit, so every evicted record is stored. Leaves fold into
    /// `max_cold_leaf_work`, and an eviction bumps `residency_epoch`.
    fn evict(&mut self) {
        let Some(below) = self.window_low() else {
            return;
        };
        let shared = &*self.shared;
        let mut evicted = false;
        while let Some(&(height, id)) = self.resident.first() {
            if height >= below {
                break;
            }
            self.resident.pop_first();
            let work = shared.arena.imm(id, |e| e.chain_work).expect("a resident entry");
            if self.leaves.remove(&(work, Reverse(id))) {
                let bound = self.max_cold_leaf_work.get_mut();
                *bound = (*bound).max(work);
            }
            shared.index.remove(&shared.arena, id);
            shared.arena.evict(id);
            let c = arena::chunk_of(id);
            let live = self.chunk_live.get_mut(&c).expect("a resident entry's chunk");
            *live -= 1;
            if *live == 0 {
                self.chunk_live.remove(&c);
                shared.arena.release(c);
            }
            evicted = true;
        }
        self.residency_epoch += u64::from(evicted);
    }

    /// Marks `id` dirty, at Core's `m_dirty_blockindex` sites (D15). SPV's dirty set is the
    /// step's written set (SB5-A13); CH1-node's through [`NodeIndex::mark_dirty`].
    pub(crate) fn mark_dirty(&mut self, id: BlockId) {
        self.dirty.insert(id);
    }

    /// Core `m_failed_blocks` (id order, D9); CH1-node's through [`NodeIndex::failed_blocks`].
    pub(crate) fn failed_blocks(&mut self) -> &mut FailedBlocks {
        &mut self.failed
    }

    // ---- persistence --------------------------------------------------------------------------

    /// Node: the dirty entries' records for CH2's B1 (P10), encoded at take time; CH2's through
    /// [`NodeIndex::take_dirty`].
    // Core: node/blockstorage.cpp WriteBlockIndexDB (CDiskBlockIndex per m_dirty_blockindex entry)
    pub(crate) fn take_dirty(&mut self) -> Vec<IndexWrite> {
        self.dirty.take().into_iter().map(|id| self.record(id)).collect()
    }

    /// Node: [`take_dirty`](Self::take_dirty)'s records, each encoded into one reused buffer for
    /// `put`; CH2's through [`NodeIndex::take_dirty_into`].
    pub(crate) fn take_dirty_into(&mut self, mut put: impl FnMut(&BlockHash, &[u8])) {
        let mut bytes = Vec::with_capacity(96);
        for id in self.dirty.take() {
            let (hash, rec) = self.index_record(id);
            bytes.clear();
            rec.encode(&mut bytes);
            put(&hash, &bytes);
        }
    }

    /// Node: `id`'s record as B1 writes it, encoded now; CH2's through [`NodeIndex::record`].
    ///
    /// # Panics
    ///
    /// If `id` is not resident.
    // Core: chain.h CDiskBlockIndex
    pub(crate) fn record(&self, id: BlockId) -> IndexWrite {
        let (hash, rec) = self.index_record(id);
        IndexWrite {
            hash,
            bytes: rec.to_bytes(),
        }
    }

    /// `id`'s hash and record, as [`record`](Self::record) encodes it.
    ///
    /// # Panics
    ///
    /// If `id` is not resident.
    // Core: chain.h CDiskBlockIndex
    fn index_record(&self, id: BlockId) -> (BlockHash, BlockIndexRecord) {
        let arena = &self.shared.arena;
        let g = arena.own_group(id).expect("the node's entries are resident");
        let (hash, header, height) =
            arena.imm(id, |e| (e.hash, e.header, e.height)).expect("resident");
        let s = g.status;
        let rec = BlockIndexRecord {
            height,
            status: s,
            n_tx: g.n_tx,
            file: s.has(Status::HAVE_MASK).then_some(g.file.unwrap_or(0)),
            data_pos: s.has(Status::HAVE_DATA).then_some(g.data_pos),
            undo_pos: s.has(Status::HAVE_UNDO).then_some(g.undo_pos),
            header,
        };
        (hash, rec)
    }

    /// SPV: §9.1 stage 5, after the tip lock is released: SB5's cold cache drops the step's
    /// written and marked ids, at most `cold_cache` of them, or clears (SB5-A5, A10). Then the
    /// step's dirty set and every mark a stage 4 applied go, keeping a small table for the next
    /// step. A mark no stage 4 applied (`set_active_tip` erred after the commit, a hold was
    /// abandoned, or the driver skipped stage 4 for a step that marked but kept its tip, which
    /// §9.1 does not allow) stays for the next stage 4, which writes it to the resident group
    /// with that step's snapshot (§3.7, D36); until then the writer reads it from the overlay,
    /// as the committed record has it (mi-2).
    ///
    /// # Panics
    ///
    /// Inside a hold, whose release has not yet applied the overlay.
    pub fn records_committed(&mut self) {
        assert!(!self.held, "records_committed inside a hold");
        if self.cfg.mode == ChainMode::Spv {
            let written = self.dirty.take();
            self.shared.invalidate_cold(self.cold_invalidation(written));
        }
        self.overlay.retain(|_, m| !m.applied);
        self.overlay.shrink_to(OVERLAY_KEPT);
    }

    /// SB5-A5's list: the step's written ids and its overlay's, deduplicated, when they are at
    /// most `cold_cache`; else `Clear`. It never holds more than `2 · cold_cache` ids.
    fn cold_invalidation(&self, written: Vec<BlockId>) -> ColdInvalidation {
        let cap = self.cfg.cold_cache as usize;
        if written.len() > cap || self.overlay.len() > cap {
            return ColdInvalidation::Clear;
        }
        let mut ids = written;
        ids.extend(self.overlay.keys());
        ids.sort_unstable();
        ids.dedup();
        match ids.len() <= cap {
            true => ColdInvalidation::Ids(ids),
            false => ColdInvalidation::Clear,
        }
    }

    // ---- readers ------------------------------------------------------------------------------

    /// The chain parameters the chain was made with.
    pub fn params(&self) -> &ChainParams {
        &self.params
    }

    /// The config the chain was made with.
    pub fn config(&self) -> &ChainConfig {
        &self.cfg
    }

    /// A `Clone + Send + Sync` reader handle.
    pub fn reader(&self) -> ChainReader {
        ChainReader {
            backend: self.shared.clone(),
        }
    }

    /// SB5's subscriber-side handle (SB5 §3.3, SB5-A14).
    pub fn feeds(&self) -> ChainFeeds {
        feed::chain_feeds(self.shared.readers.tip(), self.reader(), self.shared.clone())
    }

    /// SB5's status subscriber (SB5 §3.3, SB5-A14).
    pub fn status_feed(&self) -> StatusReader {
        feed::status_reader(self.shared.readers.tip())
    }

    /// The last published snapshot.
    pub fn snapshot(&self) -> Arc<ChainReadSnapshot> {
        self.shared.readers.tip().snapshot()
    }

    /// Complete, infallible ancestry: `Some` only for a genesis-origin chain with no window.
    pub fn complete(&self) -> Option<&dyn ChainAncestry> {
        // An anchored chain is SPV, so windowed (`check_config`).
        match self.cfg.window {
            None => Some(self),
            Some(_) => None,
        }
    }

    /// [`HeaderChain::complete`] as an owned handle that borrows nothing (CH3 §2.16): `Some`
    /// only for a genesis-origin chain with no window.
    pub fn complete_ancestry(&self) -> Option<CompleteAncestry<S, R>> {
        self.complete().map(|_| CompleteAncestry {
            shared: self.shared.clone(),
        })
    }

    /// Partial ancestry, in every configuration.
    pub fn anchored(&self) -> &dyn AnchoredAncestry {
        self
    }

    /// `getchaintips` over the header index (§10.2), every tip `headers-only` unless active,
    /// failed or conflicting (a header-only index has no `nChainTx`).
    pub fn chain_tips(&self) -> Result<Vec<ChainTip>, ChainError> {
        self.chain_tips_with(&|_| false)
    }

    /// `getchaintips` with the node's `HaveTxsDownloaded()` (`nChainTx != 0`) as `has_txs`. The
    /// tips are the leaves and the active tip (§10.2): in SPV the resident leaves and the store's
    /// (Sol r1 M1), whose read trips the latch on failure; the order is Core's
    /// `CompareBlocksByHeight` with ids for addresses (D9).
    // Core: rpc/blockchain.cpp:1827-1943 getchaintips, :1745-1757 CompareBlocksByHeight
    pub fn chain_tips_with(
        &self,
        has_txs: &dyn Fn(BlockId) -> bool,
    ) -> Result<Vec<ChainTip>, ChainError> {
        if self.active.is_empty() {
            return Err(ChainError::Internal("chain_tips: the node has no tip yet"));
        }
        let view = self.active_view();
        let mut ids: Vec<BlockId> = self.leaves.iter().map(|&(_, Reverse(id))| id).collect();
        if R::EVICTS {
            self.cold_leaves(|id, _| {
                ids.push(id);
                Ok(())
            })?;
        }
        ids.push(view.tip());
        let mut tips = Vec::with_capacity(ids.len());
        for id in ids {
            // A cold leaf, or the active tip (D-C4-12), is read from its record.
            let n = self.node(id)?;
            let (hash, height, chain_work) = (n.hash, n.height, n.chain_work);
            // FindFork over cold links: a leaf can fork below the window.
            let BlockRef {
                id: fork,
                height: fork_height,
                ..
            } = self.fork_of(view.tip(), id)?;
            let s = self.writer_status(id)?;
            // A leaf is on the active chain only as its tip, which can be cold (D-C4-12).
            let status = if id == view.tip() {
                ChainTipStatus::Active
            } else if s.has(Status::FAILED_MASK) {
                ChainTipStatus::Invalid
            } else if s.has(Status::CONFLICT_CHAINLOCK) {
                ChainTipStatus::Conflicting
            } else if !has_txs(id) {
                ChainTipStatus::HeadersOnly
            } else if s.is_valid(Status::VALID_SCRIPTS) {
                ChainTipStatus::ValidFork
            } else if s.is_valid(Status::VALID_TREE) {
                ChainTipStatus::ValidHeaders
            } else {
                ChainTipStatus::Unknown
            };
            tips.push(ChainTip {
                id,
                hash,
                height,
                chain_work,
                branchlen: height - fork_height,
                fork_point: fork,
                status,
            });
        }
        tips.sort_by_key(|t| (Reverse(t.height), t.id));
        tips.dedup_by_key(|t| t.id);
        Ok(tips)
    }

    /// The header subset of Core's `CheckBlockIndex` (`-checkblockindex`): the tree, heights,
    /// work, skip pointers and status implications, with no block data, `nTx`, sequence or
    /// candidate checks. [`HeaderChain::check_index_with`] adds the node's half.
    pub fn check_index(&self) -> Result<(), IndexInconsistency> {
        struct HeadersOnly;
        impl IndexVisitor for HeadersOnly {
            fn visit(
                &mut self,
                _: &EntryView,
                _: &FirstAncestors,
            ) -> Result<(), IndexInconsistency> {
                Ok(())
            }
        }
        self.check_index_with(&mut HeadersOnly)
    }

    /// Core's `CheckBlockIndex` walk (CH1-A9): SB4's header checks for each entry, then
    /// `visitor` with Core's `pindexFirst*` state at that entry. Entries are visited in height
    /// order, parents first; each entry's state is the one Core's depth-first walk carries
    /// there, so only which failure is reported first can differ from Core's (after `load`, a
    /// parent's id can follow its child's). It keeps that state for every id, 56 bytes each
    /// (about 134 MB at mainnet's height), as only `-checkblockindex` runs it.
    // Core: validation.cpp:5607-5855 ChainstateManager::CheckBlockIndex
    pub fn check_index_with(
        &self,
        visitor: &mut dyn IndexVisitor,
    ) -> Result<(), IndexInconsistency> {
        let arena = &self.shared.arena;
        // Core: before the first tip (a reindex's genesis), only the genesis entry may exist.
        if self.active.is_empty() {
            return match self.next_id {
                0 | 1 => Ok(()),
                _ => Err(IndexInconsistency {
                    id: BlockId(1),
                    what: "no active chain, yet more than the genesis entry",
                }),
            };
        }
        // Per id: Core's walk state at the entry.
        let mut first = vec![FirstAncestors::default(); self.next_id as usize];
        let mut roots = 0;
        // Packed pairs, as in `load`: an arena read per comparison misses the cache.
        let mut order: Vec<(u32, u32)> = (0..self.next_id)
            .map(|i| (arena.imm(BlockId(i), |e| e.height).unwrap_or(u32::MAX), i))
            .collect();
        order.sort_unstable();
        for (_, i) in order {
            let id = BlockId(i);
            let fail = |what| {
                Err(IndexInconsistency {
                    id,
                    what,
                })
            };
            let Some(e) = self.shared.entry(id) else {
                return fail("entry is not resident");
            };
            if self.shared.index.lookup(arena, &e.hash) != Some(id) {
                return fail("the hash index must map the entry's hash to it");
            }
            let validity = e.status.bits() & Status::VALID_MASK;
            let parent = match e.prev {
                None => {
                    roots += 1;
                    if roots > 1 {
                        return fail("a second entry with no parent");
                    }
                    if e.hash.as_bytes() != self.params.consensus.hash_genesis_block.as_bytes() {
                        return fail("genesis hash must match");
                    }
                    if self.active.get(0) != Some(id) {
                        return fail("the chain's genesis block must be this block");
                    }
                    if e.height != 0 {
                        return fail("nHeight must be consistent");
                    }
                    FirstAncestors::default()
                }
                Some(p) => {
                    let (p_height, p_work) =
                        arena.imm(p, |e| (e.height, e.chain_work)).expect("resident parent");
                    if e.height != p_height + 1 {
                        return fail("nHeight must be consistent");
                    }
                    if e.chain_work < p_work {
                        return fail("chainwork must be at least the parent's");
                    }
                    if validity < Status::VALID_TREE {
                        return fail("every entry must be TREE valid");
                    }
                    first[p.0 as usize]
                }
            };
            let f = parent.extend(&e);
            first[i as usize] = f;
            let skip = arena.imm(id, |e| e.skip).expect("resident");
            if e.height >= 2
                && !skip.is_some_and(|s| arena.imm(s, |s| s.height < e.height) == Some(true))
            {
                return fail("pskip must point back");
            }
            if e.status.has(Status::HAVE_UNDO) && !e.status.has(Status::HAVE_DATA) {
                return fail("HAVE_UNDO without HAVE_DATA");
            }
            if validity >= Status::VALID_CHAIN && f.not_chain_valid.is_some() {
                return fail("CHAIN valid implies all parents are CHAIN valid");
            }
            if validity >= Status::VALID_SCRIPTS && f.not_scripts_valid.is_some() {
                return fail("SCRIPTS valid implies all parents are SCRIPTS valid");
            }
            if f.invalid.is_none() && e.status.has(Status::FAILED_MASK) {
                return fail("failed mask without an invalid parent");
            }
            // Vacuous, as in Core: the entry's own bit sets `pindexFirstConflicing` first.
            if f.conflicting.is_none() && e.status.has(Status::CONFLICT_CHAINLOCK) {
                return fail("conflict mask without a conflicting parent");
            }
            visitor.visit(&e, &f)?;
        }
        Ok(())
    }
}

/// A hold on the tip lock (CH1-A1; SB5 §3.3), from [`HeaderChain::hold_tip`]. `!Send`, as
/// its guard is.
///
/// The writer's other calls go through [`Hold::chain`]. There the tip writers return
/// `ChainError::Internal` instead of deadlocking, `hold_tip` panics, and `publish_status`
/// does nothing: the release publishes (CH1-A13). Dropping a hold without [`Hold::release`]
/// is [`Hold::abandon`].
///
/// ```compile_fail,E0277
/// fn send<T: Send>(_: T) {}
/// fn hold(chain: &mut dash_chain::HeaderChain) {
///     send(chain.hold_tip());
/// }
/// ```
pub struct Hold<'c, S: HeaderStore = NoStore, R: Residency = Permanent> {
    /// Crate-visible so that SB5's `impl Hold` (`with_tip`, `retry_pending`) reaches them.
    pub(crate) chain: &'c mut HeaderChain<S, R>,
    pub(crate) guard: HoldGuard,
}

impl<S: HeaderStore, R: Residency> Hold<'_, S, R> {
    /// Node: the `ConnectTip` boundary inside the hold: the step and its `seq`, no snapshot.
    pub fn connect_tip(&mut self, id: BlockId) -> Result<TipStep, ChainError> {
        self.fanned(|c, st| c.connect_step(st, id))
    }

    /// Node: the `DisconnectTip` boundary inside the hold: the step and its `seq`, no snapshot.
    pub fn disconnect_tip(&mut self) -> Result<TipStep, ChainError> {
        self.fanned(|c, st| c.disconnect_step(st))
    }

    /// SPV: one sub-step of a segmented tip change (SB5 §4.8, SB5-A12): the plan to
    /// `sub_target` within `max_legs` steps ([`HeaderChain::tip_plan_bounded`]), stepped with
    /// consecutive `seq`s. Nothing is published or applied: the release runs stage 4 once, for
    /// the final tip (SB5-A11 (e)). The change's `snap` is the pre-hold snapshot, which SPV
    /// sub-steps do not use (SB5-A11 (d)). Its first `seq` is `last_seq + 1 − (d + c)`. Called
    /// from SB5 F3's `activate_segmented`, which fans each sub-step out around this call and
    /// the last one at its own release. Crate-private: [`Hold::release`] fans nothing out, so a
    /// caller stepping SPV here and releasing would leave a gap in the feeds' `seq` stream.
    pub(crate) fn set_active_tip(
        &mut self,
        sub_target: BlockId,
        max_legs: u32,
    ) -> Result<TipChange, ChainError> {
        self.chain.require_mode(ChainMode::Spv, "Hold::set_active_tip (SPV only)")?;
        feed::before_step(&self.guard)?;
        let plan = self.chain.tip_plan_bounded(sub_target, max_legs)?;
        let end = self.chain.plan_end(&plan)?;
        self.chain.run_plan(&mut self.guard, &plan, end);
        Ok(TipChange {
            disconnected: plan.disconnect,
            connected: plan.connect,
            last_seq: self.chain.last_seq,
            snap: self.guard.snapshot.clone(),
        })
    }

    /// The writer's calls that do not take the tip lock: status writes, the best-header hooks,
    /// the private reads (CH1-A2) and readers.
    pub fn chain(&mut self) -> &mut HeaderChain<S, R> {
        self.chain
    }

    /// Ends the hold: stage 4's side effects, once (SB5-A11 (e)), then one snapshot of the
    /// writer's state, `tip_seq` the last step's (or the pre-hold `tip_seq`), stored before the
    /// guard drops (CH1-node §2.5).
    pub fn release(mut self) -> Arc<ChainReadSnapshot> {
        let own =
            Arc::ptr_eq(ArcMutexGuard::mutex(&self.guard), &self.chain.shared.readers.tip().state);
        assert!(own, "the hold's chain was replaced through Hold::chain");
        self.chain.stage4_effects();
        self.chain.publish_locked(&mut self.guard, Fanout::None)
    }

    /// Ends the hold with no snapshot: after an error, nothing of the hold is published (SB5
    /// §4.8). The steps taken stay taken; the next publication, `publish_status`'s included,
    /// shows them with their `tip_seq`.
    pub fn abandon(self) {}
}

impl<S: HeaderStore, R: Residency> Drop for Hold<'_, S, R> {
    /// Clears the chain's flag, then ends the hold: the guard drops and the release wakes fire.
    /// A hold dropped by an unwind (a broken invariant in a sub-step or the release's stage 4)
    /// may leave the writer half-stepped, so it trips the latch first, as SPV's `set_active_tip`
    /// does (Opus r1 n-4).
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.chain.svc.fail.read_failed("a tip hold unwound inside the tip lock");
        }
        self.chain.held = false;
        self.end();
    }
}

/// §11.1's pack check: the anchor hashes to the pack's hash, the headers are hash-linked, the
/// anchor is at least `dgw_height + 24`, and the absolute work covers every proof below it,
/// subtracted one checked step at a time (Sol r1 m3). Returns each header's hash and chain work.
fn check_pack(
    pack: &AnchorPack,
    pow: &PowParams,
) -> Result<([BlockHash; 25], [Work; 25]), AnchorError> {
    let hashes = pack.headers.map(|h| Header::new(h).hash());
    if hashes[24] != pack.hash {
        return Err(AnchorError::Hash);
    }
    if let Some(index) = (0..24).find(|&i| hashes[i] != pack.headers[i + 1].prev_hash) {
        return Err(AnchorError::Link {
            index,
        });
    }
    // The first test keeps `height − 24` in range whatever DGW height the parameters give.
    if pack.height < 24 || i64::from(pack.height) < i64::from(pow.config().dgw_height) + 24 {
        return Err(AnchorError::PreDgw {
            height: pack.height,
        });
    }
    let mut works = [pack.chain_work; 25];
    for k in (0..24).rev() {
        let proof = block_proof(CompactTarget::new(pack.headers[k + 1].bits));
        if works[k + 1] <= proof {
            return Err(AnchorError::Work);
        }
        works[k] = works[k + 1] - proof;
    }
    Ok((hashes, works))
}

/// `GetMedianTimePast` from `times` (an entry at `height` and its ancestors, newest first), or
/// `MTP_UNKNOWN` when the floor ended the walk before eleven times above genesis: an anchored
/// chain's lowest entries do not hold all of theirs.
fn mtp_or_unknown(times: &[u32], height: u32) -> i64 {
    match times.len() < walk::MEDIAN_TIME_SPAN && height >= times.len() as u32 {
        true => MTP_UNKNOWN,
        false => walk::median_time_past(times.iter().copied()),
    }
}

/// What [`HeaderChain::derive`] reads of an ancestor, resident or cold.
#[derive(Clone, Copy)]
struct Ancestor {
    link: walk::Link,
    time: u32,
    time_max: u32,
    chain_work: Work,
}

impl Ancestor {
    fn of_entry(e: &EntryImm) -> Ancestor {
        Ancestor {
            link: walk::Link {
                height: e.height,
                prev: e.prev,
                skip: e.skip,
            },
            time: e.header.time,
            time_max: e.time_max,
            chain_work: e.chain_work,
        }
    }

    fn of_node(n: &Node) -> Ancestor {
        Ancestor {
            link: n.link(),
            time: n.header.time,
            time_max: n.time_max,
            chain_work: n.chain_work,
        }
    }
}

/// Refuses a configuration the mode cannot run (§4, SB0 §4.4).
fn check_config(cfg: &ChainConfig) -> Result<(), ConfigError> {
    cfg.feeds.validate()?;
    match (cfg.mode, cfg.window) {
        (ChainMode::Node, Some(_)) => Err(ConfigError::WindowInNodeMode),
        (ChainMode::Spv, None) => Err(ConfigError::NoWindowInSpvMode),
        (ChainMode::Spv, _) if cfg.cold_cache == 0 => Err(ConfigError::ColdCache),
        _ => Ok(()),
    }
}

/// Core `GetLastCheckpoint`'s height for `ContextualCheckBlockHeader` (§11.4): the highest-keyed
/// checkpoint whose hash is in the index, read from the cache; `None` with `-checkpoints=0`.
// Core: node/blockstorage.cpp:428-440 GetLastCheckpoint; validation.cpp:4493-4501
impl<S: HeaderStore, R: Residency> CheckpointView for HeaderChain<S, R> {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        let (_, height) = self.last_checkpoint.filter(|_| self.cfg.checkpoints_enabled)?;
        Some(height)
    }
}

/// The deployments the header rules read (§12, D14): buried ones by height. Versionbits ones are
/// unreachable from `ContextualCheckBlockHeader`; block-level deployments are CH1-node's.
struct HeaderDeployments<'a> {
    params: &'a ChainParams,
    chain: &'a dyn ChainAncestry,
}

impl HeaderDeployments<'_> {
    fn buried_active_at(&self, height: u32, deployment: Deployment) -> bool {
        match deployment {
            Deployment::Buried(d) => height >= self.params.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(pos) => {
                debug_assert!(false, "the header rules read no versionbits deployment ({pos:?})");
                false
            }
        }
    }
}

impl DeploymentView for HeaderDeployments<'_> {
    // Core: deploymentstatus.h DeploymentActiveAfter (buried: nHeight + 1 >= height)
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| self.chain.header(p).height + 1);
        self.buried_active_at(next, deployment)
    }

    // Core: deploymentstatus.h DeploymentActiveAt (buried: nHeight >= height)
    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.buried_active_at(self.chain.header(block).height, deployment)
    }
}

/// The rules' view of an entry.
fn header_ctx(e: &EntryImm) -> HeaderCtx {
    HeaderCtx {
        height: e.height,
        hash: e.hash,
        time: e.header.time,
        bits: CompactTarget::new(e.header.bits),
        version: e.header.version,
        mtp: e.mtp,
        chain_work: e.chain_work,
    }
}

/// A [`Node`]'s rule view; a cold node's `mtp` is unknown (§8.3).
fn node_ctx(n: &Node) -> HeaderCtx {
    HeaderCtx {
        height: n.height,
        hash: n.hash,
        time: n.header.time,
        bits: CompactTarget::new(n.header.bits),
        version: n.header.version,
        mtp: n.mtp,
        chain_work: n.chain_work,
    }
}

/// The writer's partial ancestry (§8.4): resident entries, then committed records, down to the
/// floor. A caller's id the chain never issued panics, as in `ChainAncestry`; one read from a
/// record is corruption.
impl<S: HeaderStore, R: Residency> AnchoredAncestry for HeaderChain<S, R> {
    fn floor(&self) -> u32 {
        self.floor
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        let n = self.issued(id)?;
        let mut ctx = node_ctx(&n);
        if !n.resident {
            ctx.mtp = self.cold_mtp(&n)?;
        }
        Ok(ctx)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        if height < self.floor {
            return Err(AncestryError::BelowFloor {
                height,
            });
        }
        self.issued(id)?;
        let link = |x| self.stored(x).map(|n| n.link());
        Ok(walk::ancestor(id, height, link, |x| self.shared.off_height(x))?)
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        let n = self.issued(id)?;
        match (n.prev, n.height) {
            (None, h) if h > 0 => Err(AncestryError::BelowFloor {
                height: h - 1,
            }),
            (prev, _) => Ok(prev),
        }
    }
}

impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    /// `id` for [`AnchoredAncestry`]: resident or cold; panics for an id never issued.
    fn issued(&self, id: BlockId) -> Result<Node, StoreError> {
        assert!(id.0 < self.next_id, "AnchoredAncestry: no entry {}", id.0);
        self.shared.node(id)
    }

    /// An id read from a record (a skip, a parent), resident or cold: one the chain never issued
    /// is corruption, where [`HeaderChain::issued`] panics for a caller's.
    fn stored(&self, id: BlockId) -> Result<Node, StoreError> {
        match id.0 < self.next_id {
            true => self.shared.node(id),
            false => {
                Err(self.shared.corrupt(format!("a record names entry {}, never issued", id.0)))
            }
        }
    }

    /// A cold entry's `GetMedianTimePast` from its ancestors' records (§8.3): unknown when the
    /// floor ends the eleven times above genesis.
    fn cold_mtp(&self, n: &Node) -> Result<i64, StoreError> {
        let mut times = Vec::with_capacity(walk::MEDIAN_TIME_SPAN);
        let mut cur = Some(*n);
        while let Some(x) = cur.filter(|_| times.len() < walk::MEDIAN_TIME_SPAN) {
            times.push(x.header.time);
            cur = x.prev.map(|p| self.stored(p)).transpose()?;
        }
        Ok(mtp_or_unknown(&times, n.height))
    }
}

/// The complete ancestry of a genesis-origin, unwindowed chain (`complete()`): every id the
/// arena issued is resident.
impl<S: HeaderStore, R: Residency> ChainAncestry for HeaderChain<S, R> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.shared
            .arena
            .imm(id, header_ctx)
            .unwrap_or_else(|| panic!("ChainAncestry: no entry {}", id.0))
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        walk::ancestor(id, height, |id| self.shared.link(id).ok_or(id), walk::resident_off_height)
            .unwrap_or_else(|id| panic!("ChainAncestry: no entry {}", id.0))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        self.shared
            .arena
            .imm(id, |e| e.prev)
            .unwrap_or_else(|| panic!("ChainAncestry: no entry {}", id.0))
    }
}

/// The complete ancestry of a genesis-origin, unwindowed chain ([`HeaderChain::complete`]) as an
/// owned handle: the same reads of the arena the chain shares with its readers, through no
/// borrow of the chain. It reads only fields an entry never changes once set, so a writer
/// holding `&mut HeaderChain` (V inside a hold) can lend it out meanwhile; CH3a's deployments
/// read it there (CH3 §2.16).
pub struct CompleteAncestry<S: HeaderStore = NoStore, R: Residency = Permanent> {
    shared: Arc<Shared<S, R>>,
}

// Not derived: the handle clones its `Arc` whatever `S` and `R` are.
impl<S: HeaderStore, R: Residency> Clone for CompleteAncestry<S, R> {
    fn clone(&self) -> Self {
        CompleteAncestry {
            shared: self.shared.clone(),
        }
    }
}

impl<S: HeaderStore, R: Residency> ChainAncestry for CompleteAncestry<S, R> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.shared
            .arena
            .imm(id, header_ctx)
            .unwrap_or_else(|| panic!("ChainAncestry: no entry {}", id.0))
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        walk::ancestor(id, height, |id| self.shared.link(id).ok_or(id), walk::resident_off_height)
            .unwrap_or_else(|id| panic!("ChainAncestry: no entry {}", id.0))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        self.shared
            .arena
            .imm(id, |e| e.prev)
            .unwrap_or_else(|| panic!("ChainAncestry: no entry {}", id.0))
    }
}

/// A test's probe of each publication (A-37).
#[cfg(all(test, not(loom)))]
type PublishHook = Box<dyn FnMut(&ChainReadSnapshot)>;

#[cfg(all(test, not(loom)))]
thread_local! {
    /// Tests only: called under the tip lock right after each snapshot store, before the fanout,
    /// so a test reads what a lock-free reader sees at the instant a snapshot becomes visible
    /// (A-37: never the new tip with the old statuses).
    static AFTER_PUBLISH: std::cell::RefCell<Option<PublishHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, not(loom)))]
mod accept_tests;
#[cfg(all(test, not(loom)))]
mod cl_tests;
mod enforce;
#[cfg(all(any(test, feature = "test-util"), not(loom)))]
mod mem_step;
mod node_index;
mod rules;
mod sb5;
#[cfg(all(test, not(loom)))]
mod scenario_tests;
#[cfg(all(test, not(loom)))]
mod spv_tests;

pub use node_index::NodeIndex;

#[cfg(all(test, not(loom)))]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use dash_clock::NodeSeconds;
    use dash_params::NetworkKind;

    use super::*;
    use crate::reader::{EntryView, FlatFilePos, IndexAccess};
    use crate::testing::{fake_hash, header, params, services};
    use crate::{RETAIN_AT_MOST, SnapshotExpired};

    pub(super) fn node() -> HeaderChain {
        let mut chain = node_pre_genesis();
        chain.init_active_tip(BlockId(0)).expect("Core LoadChainTip");
        chain
    }

    /// A node before `LoadChainTip` (CH1-A11).
    fn node_pre_genesis() -> HeaderChain {
        with_cfg(ChainConfig::node)
    }

    fn assert_internal<T: std::fmt::Debug>(r: Result<T, ChainError>) {
        assert!(matches!(r, Err(ChainError::Internal(_))), "{r:?}");
    }

    pub(super) fn spv() -> HeaderChain {
        with_cfg(|p| ChainConfig::spv(p, 10_000))
    }

    pub(super) fn with_cfg(cfg: impl FnOnce(&ChainParams) -> ChainConfig) -> HeaderChain {
        let p = params(NetworkKind::Regtest);
        let cfg = cfg(&p);
        let svc = services(&p);
        HeaderChain::new_genesis(p, cfg, svc, NoStore).expect("a valid configuration")
    }

    /// `n` headers on `from`, one second apart from `time`, under hashes `fake_hash(k0..)`.
    pub(super) fn branch(
        chain: &mut HeaderChain,
        from: BlockId,
        k0: u64,
        n: u64,
        time: u32,
    ) -> Vec<BlockId> {
        let mut ids = vec![from];
        for k in 0..n {
            ids.push(add(chain, ids[ids.len() - 1], k0 + k, time + k as u32));
        }
        ids.split_off(1)
    }

    /// Connects `ids` one step each (node).
    pub(super) fn connect_all(chain: &mut HeaderChain, ids: &[BlockId]) {
        for &id in ids {
            chain.connect_tip(id).expect("prev is the tip");
        }
    }

    /// Adds a header on `prev` under the synthetic hash `fake_hash(k)`.
    pub(super) fn add(chain: &mut HeaderChain, prev: BlockId, k: u64, time: u32) -> BlockId {
        let prev_hash = chain.shared.arena.imm(prev, |e| e.hash).expect("resident prev");
        let id = chain
            .add_to_block_index(
                &header(prev_hash, time, k as u32),
                &fake_hash(k),
                Status(Status::VALID_TREE),
            )
            .expect("added");
        assert_eq!(chain.shared.arena.imm(id, |e| e.hash), Some(fake_hash(k)));
        id
    }

    #[test]
    fn genesis_is_the_first_entry() {
        let chain = node();
        let p = params(NetworkKind::Regtest);
        let g = Header::new(p.genesis.header);
        let reader = chain.reader();
        assert_eq!(reader.lookup(&g.hash()), Some(BlockId(0)));
        assert_eq!(chain.best_header(), BlockId(0));
        let e = reader.entry(BlockId(0)).expect("genesis");
        assert_eq!((e.height, e.prev, e.status), (0, None, Status(Status::VALID_TREE)));
        assert_eq!(e.chain_work, block_proof(CompactTarget::new(p.genesis.header.bits)));
        assert_eq!(e.mtp, i64::from(p.genesis.header.time));
        assert_eq!(e.time_max, p.genesis.header.time);
        assert_eq!((e.n_tx, e.file), (0, None));
        assert!(chain.dirty.contains(BlockId(0)));
        assert_eq!(chain.leaves.len(), 1);
        let ctx = chain.complete().expect("genesis-origin, no window").header(BlockId(0));
        assert_eq!((ctx.hash, ctx.height), (g.hash(), 0));
    }

    /// CH3 §2.16: the owned handle reads what the chain reads, entries added after it was taken
    /// included, and a windowed chain has none.
    #[test]
    fn the_complete_ancestry_handle_reads_as_the_chain() {
        let mut chain = node();
        let handle = chain.complete_ancestry().expect("genesis-origin, no window");
        let g_time = chain.shared.arena.imm(BlockId(0), |e| e.header.time).expect("genesis");
        let trunk = branch(&mut chain, BlockId(0), 1, 30, g_time + 1);
        let side = branch(&mut chain, trunk[9], 100, 5, g_time + 50);
        let whole: &dyn ChainAncestry = &chain;
        for &id in std::iter::once(&BlockId(0)).chain(&trunk).chain(&side) {
            assert_eq!(handle.header(id), whole.header(id));
            assert_eq!(handle.prev(id), whole.prev(id));
            let height = whole.header(id).height;
            for h in [0, height / 2, height, height + 1] {
                assert_eq!(handle.ancestor(id, h), whole.ancestor(id, h));
            }
        }
        assert!(spv().complete_ancestry().is_none());
    }

    #[test]
    fn refuses_a_configuration_its_mode_cannot_run() {
        let p = params(NetworkKind::Regtest);
        let mut cfg = ChainConfig::node(&p);
        cfg.window = Some(10_000);
        let err = HeaderChain::<NoStore>::new_genesis(p.clone(), cfg, services(&p), NoStore);
        assert!(matches!(err, Err(ChainError::Config(ConfigError::WindowInNodeMode))));

        let mut cfg = ChainConfig::spv(&p, 10_000);
        cfg.window = None;
        let err = HeaderChain::<NoStore>::new_genesis(p.clone(), cfg, services(&p), NoStore);
        assert!(matches!(err, Err(ChainError::Config(ConfigError::NoWindowInSpvMode))));
    }

    /// A trunk of 40 and a branch of 45 from height 10, with times that go back now and then.
    #[test]
    fn index_fields_follow_core_on_two_branches() {
        let mut chain = node();
        let g_time = chain.shared.arena.imm(BlockId(0), |e| e.header.time).expect("genesis");
        let time = |k: u64| {
            g_time + (k as u32) * 150
                - if k % 7 == 3 {
                    2000
                } else {
                    0
                }
        };
        let mut trunk = vec![BlockId(0)];
        for k in 1..=40 {
            trunk.push(add(&mut chain, trunk[trunk.len() - 1], k, time(k)));
        }
        let mut branch = vec![trunk[10]];
        for k in 101..=145 {
            branch.push(add(&mut chain, branch[branch.len() - 1], k, time(k - 90)));
        }
        let reader = chain.reader();

        // A known hash returns its id and adds nothing.
        let before = chain.next_id;
        assert_eq!(add(&mut chain, trunk[4], 5, 0), trunk[5]);
        assert_eq!(chain.next_id, before);

        for (&id, prev) in branch.iter().skip(1).zip(&branch) {
            let e = reader.entry(id).expect("resident");
            assert_eq!(e.prev, Some(*prev));
            assert_eq!(e.height, reader.height(*prev).expect("prev") + 1);
            // pskip is the ancestor at GetSkipHeight; every ancestor is the naive walk's.
            let skip = chain.shared.arena.imm(id, |e| e.skip).expect("resident");
            let naive = |h: u32| -> BlockId {
                let mut w = id;
                while reader.height(w) != Some(h) {
                    w = reader.entry(w).and_then(|e| e.prev).expect("above genesis");
                }
                w
            };
            assert_eq!(skip, Some(naive(walk::skip_height(e.height))));
            for h in 0..=e.height {
                assert_eq!(reader.ancestor(id, h), Some(naive(h)));
            }
            // Median of the last eleven times; running maximum of all.
            let mut times: Vec<i64> = (0..=e.height.min(10))
                .map(|d| i64::from(reader.entry(naive(e.height - d)).expect("anc").header.time))
                .collect();
            times.sort_unstable();
            assert_eq!(e.mtp, times[times.len() / 2]);
            let max = (0..=e.height).map(|h| reader.entry(naive(h)).expect("anc").header.time);
            assert_eq!(e.time_max, max.max().expect("non-empty"));
            assert_eq!(
                e.chain_work,
                reader.entry(*prev).expect("prev").chain_work
                    + block_proof(CompactTarget::new(e.header.bits))
            );
        }

        assert_eq!(reader.lca(trunk[40], branch[45]), Some(trunk[10]));
        assert_eq!(reader.lca(trunk[3], branch[20]), Some(trunk[3]));
        assert_eq!(chain.child_ids(trunk[10]), Ok(vec![trunk[11], branch[1]]));
        assert_eq!(chain.child_ids(trunk[40]), Ok(vec![]));
        let leaves: Vec<BlockId> = chain.leaves.iter().map(|(_, Reverse(id))| *id).collect();
        assert_eq!(leaves, vec![trunk[40], branch[45]], "by work");
        // The branch is longer at equal per-block work.
        assert_eq!(chain.best_header(), branch[45]);
    }

    /// `blockstorage.cpp:111-115`: strictly more work moves `best_header`, so the first of two
    /// equal-work headers stays.
    #[test]
    fn best_header_is_first_seen_on_a_tie() {
        let mut chain = node();
        let a = add(&mut chain, BlockId(0), 1, 1_700_000_100);
        let b = add(&mut chain, BlockId(0), 2, 1_700_000_100);
        assert_eq!(chain.best_header(), a);
        let c = add(&mut chain, b, 3, 1_700_000_200);
        assert_eq!(chain.best_header(), c);
        // A status with no validity level is added at VALID_TREE with its bits, and never moves
        // best_header.
        let prev_hash = chain.shared.arena.imm(c, |e| e.hash).expect("c");
        let d = chain
            .add_to_block_index(
                &header(prev_hash, 1_700_000_300, 4),
                &fake_hash(4),
                Status(Status::CONFLICT_CHAINLOCK),
            )
            .expect("added");
        assert_eq!(chain.best_header(), c);
        let s = chain.reader().status(d).expect("d");
        assert_eq!(s, Status(Status::VALID_TREE | Status::CONFLICT_CHAINLOCK));
    }

    /// A-31: the bits span the ids marked since the last `take`, from its lowest word; a lower
    /// id shifts them, and order and deduplication are unchanged.
    #[test]
    fn dirty_bits_span_the_marked_ids_only() {
        let mut d = Dirty::default();
        for id in [3_000_000, 3_000_001, 3_000_000] {
            d.insert(BlockId(id));
        }
        assert_eq!(
            (d.take(), d.lo, d.bits.len()),
            (vec![BlockId(3_000_000), BlockId(3_000_001)], 3_000_000 / 64, 1)
        );
        for id in [5_000_100, 5_000_000, 4_999_000, 5_000_100, 4_999_000] {
            d.insert(BlockId(id));
        }
        assert_eq!((d.lo, d.bits.len()), (4_999_000 / 64, 5_000_100 / 64 - 4_999_000 / 64 + 1));
        assert!(d.contains(BlockId(5_000_000)) && !d.contains(BlockId(5_000_001)));
        assert!(!d.contains(BlockId(64)), "below the bits");
        let ids = d.take();
        assert_eq!(ids, [5_000_100, 5_000_000, 4_999_000].map(BlockId));
        assert!(d.bits.iter().all(|&w| w == 0) && d.ids().is_empty());
        // A wide set's bits go at `take`.
        for id in [6_010_000, 6_000_000] {
            d.insert(BlockId(id));
        }
        assert!(d.bits.len() > DIRTY_BITS_KEPT);
        assert_eq!(d.take().len(), 2);
        assert!(d.bits.is_empty() && !d.contains(BlockId(6_000_000)));
    }

    /// CH1-node D63: a status write is read as of a snapshot. A reader of the snapshot before
    /// the write sees the old status until it takes the next one; the live read sees the new.
    /// The first old status since a snapshot stands, and a write that keeps the status journals
    /// nothing.
    #[test]
    fn a_status_write_is_read_as_of_the_snapshot() {
        let mut chain = node();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 2, 1_700_000_000);
        let (a, b) = (trunk[0], trunk[1]);
        let tree = reader.status(a).expect("resident");
        let before = reader.snapshot();
        chain.modify(a, |g| g.status = g.status.raise_validity(Status::VALID_TRANSACTIONS).0);
        chain.modify(a, |g| g.status = g.status.raise_validity(Status::VALID_SCRIPTS).0);
        let scripts = reader.status(a).expect("resident");
        assert!(scripts.is_valid(Status::VALID_SCRIPTS) && !tree.is_valid(Status::VALID_SCRIPTS));
        assert_eq!(reader.status_at(&before, a), Ok(Some(tree)), "the first old status stands");
        chain.connect_tip(a).expect("connect");
        let after = reader.snapshot();
        assert_eq!(reader.status_at(&after, a), Ok(Some(scripts)));
        assert_eq!(reader.status_at(&before, a), Ok(Some(tree)), "the old snapshot keeps its view");
        let written = reader.journal_retention().entries;
        chain.modify(b, |g| g.n_tx = 1);
        chain.connect_tip(b).expect("connect");
        assert_eq!(reader.journal_retention().entries, written);
        assert_eq!(reader.status_at(&before, BlockId(99)), Ok(None), "not in the index");
    }

    /// Opus-h r1 F2: a lowered status is journalled too (Core's `ResetBlockFailureFlags` clears
    /// FAILED_VALID): the snapshot published while it was set still reads it.
    #[test]
    fn a_lowered_status_is_read_as_of_the_snapshot() {
        let mut chain = node();
        let reader = chain.reader();
        let a = branch(&mut chain, BlockId(0), 1, 1, 1_700_000_000)[0];
        let failed = |s: Status| s.bits() & Status::FAILED_VALID != 0;
        let at = |snap| reader.status_at(snap, a).expect("held briefly").expect("resident");
        chain.modify(a, |g| g.status = Status(g.status.bits() | Status::FAILED_VALID));
        chain.publish_status();
        let set = reader.snapshot();
        chain.modify(a, |g| g.status = Status(g.status.bits() & !Status::FAILED_VALID));
        assert!(!failed(reader.status(a).expect("resident")), "the live status is cleared");
        assert!(failed(at(&set)), "before the next build");
        chain.publish_status();
        let cleared = reader.snapshot();
        assert!(failed(at(&set)), "the old snapshot keeps it");
        assert!(!failed(at(&cleared)));
    }

    /// Sol BL-206 r1 (`reader.rs:166`): insertion is not journalled, so an entry inserted after
    /// a snapshot reads at its insertion status; a write after that journals the insertion
    /// status, the snapshot's pre-image.
    #[test]
    fn an_entry_inserted_after_a_snapshot_reads_as_of_its_insertion() {
        let mut chain = node();
        let reader = chain.reader();
        let before = reader.snapshot();
        let a = add(&mut chain, BlockId(0), 1, 1_700_000_000);
        let inserted = reader.status(a).expect("resident");
        assert_eq!(reader.status_at(&before, a), Ok(Some(inserted)), "its insertion status");
        chain.modify(a, |g| g.status = g.status.raise_validity(Status::VALID_TRANSACTIONS).0);
        assert_ne!(reader.status(a), Some(inserted), "the live status moved");
        assert_eq!(reader.status_at(&before, a), Ok(Some(inserted)), "the pre-image");
    }

    /// Sol BL-224 r1: one status write after a publication shows in the gauge at once, before
    /// the writer moves on.
    #[test]
    fn one_status_write_shows_in_the_retention_gauge() {
        let flip = |g: &mut MutableGroup| g.status = Status(g.status.bits() ^ Status::HAVE_UNDO);
        let mut chain = node();
        let reader = chain.reader();
        let ids = branch(&mut chain, BlockId(0), 1, 1, 1_700_000_000);
        chain.publish_status();
        let before = reader.journal_retention().entries;
        chain.modify(ids[0], flip);
        assert_eq!(reader.journal_retention().entries, before + 1);
    }

    /// D68 (DEC-281): no snapshot carries a journal, so whatever holds one, bare, a feed's or a
    /// reply, the writer keeps the same journals, and a holder's drop frees none.
    #[test]
    fn the_retention_gauge_is_the_same_whoever_holds_a_snapshot() {
        let flip = |g: &mut MutableGroup| g.status = Status(g.status.bits() ^ Status::HAVE_UNDO);
        let mut seen = Vec::new();
        for holder in ["none", "snapshot", "feed", "reply"] {
            let mut chain = node();
            chain.journals.rotate_at = 1;
            let reader = chain.reader();
            let ids = branch(&mut chain, BlockId(0), 1, 8, 1_700_000_000);
            let held: Option<Box<dyn std::any::Any>> = match holder {
                "none" => None,
                "snapshot" => Some(Box::new(reader.snapshot())),
                "feed" => Some(Box::new(chain.feeds().feed_snapshot())),
                _ => Some(Box::new(chain.set_active_tip(ids[0]).expect("a step"))),
            };
            for &id in &ids {
                chain.modify(id, flip);
            }
            chain.publish_status();
            chain.modify(ids[0], flip);
            let kept = reader.journal_retention();
            drop(held);
            chain.publish_status();
            chain.modify(ids[1], flip);
            let after = reader.journal_retention();
            assert_eq!(after.entries, kept.entries + 1, "{holder}: the drop freed nothing");
            assert_eq!((kept.expired, after.expired), (0, 0), "{holder}");
            seen.push(kept);
        }
        assert!(seen.windows(2).all(|w| w[0].entries == w[1].entries), "{seen:?}");
    }

    /// BL-224 (D65, D68): a snapshot past the bound, with builds after it, reads expired
    /// through the reader once it needs a dropped journal, never at the live status, and the
    /// gauge counts each such read; the newest snapshot reads as before.
    #[test]
    fn a_snapshot_held_past_the_bound_reads_expired() {
        let mut chain = node();
        (chain.journals.rotate_at, chain.journals.retain_at_most) = (1, 2);
        let reader = chain.reader();
        let ids = branch(&mut chain, BlockId(0), 1, 8, 1_700_000_000);
        let old = reader.snapshot();
        for &id in &ids {
            chain.modify(id, |g| g.status = Status(g.status.bits() | Status::FAILED_CHILD));
            chain.publish_status();
        }
        assert!(reader.journal_retention().oldest_epoch > old.epoch);
        assert_eq!(reader.status_at(&old, ids[0]), Err(SnapshotExpired), "its journal went");
        assert_eq!(reader.status_at(&old, ids[7]), Err(SnapshotExpired));
        assert_eq!(reader.journal_retention().expired, 2);
        let live = reader.status(ids[7]);
        assert_eq!(reader.status_at(&reader.snapshot(), ids[7]), Ok(live));
    }

    /// Validator BL-224 r1: a command that writes past the bound with no build between keeps
    /// the newest snapshot's journals: its readers read as of it, never expired.
    #[test]
    fn the_tip_cells_snapshot_never_expires() {
        let mut chain = node();
        (chain.journals.rotate_at, chain.journals.retain_at_most) = (1, 2);
        let reader = chain.reader();
        let ids = branch(&mut chain, BlockId(0), 1, 16, 1_700_000_000);
        let snap = reader.snapshot();
        let before: Vec<_> = ids.iter().map(|&id| reader.status(id)).collect();
        for &id in &ids {
            chain.modify(id, |g| g.status = Status(g.status.bits() | Status::FAILED_CHILD));
        }
        let at: Vec<_> = ids.iter().map(|&id| reader.status_at(&snap, id)).collect();
        assert_eq!(at, before.into_iter().map(Ok).collect::<Vec<_>>());
        assert_eq!(reader.journal_retention().expired, 0);
    }

    /// Opus-h r1 F3 (its probe, bounded): a writer journals and publishes while four reader
    /// threads check that every status read is the one written before their snapshot's build.
    /// Each round writes every entry twice, the first value a decoy, so a reader that takes
    /// anything written after its build (a second live read after the walk, say) fails. With
    /// BL-224's bound at two entries (D65), the writer drops journals under the readers' walks:
    /// a read may expire, never return another status.
    #[test]
    fn status_at_holds_under_concurrent_writes() {
        const ROUNDS: u32 = 5_000;
        // The value written last before the build of epoch `e` for entry `k`, and the decoy.
        let enc = |e: u64, k: usize| Status((((e as u32) & 0xFFFF) << 8) | (k as u32) | 0x10);
        let junk = |e: u64, k: usize| Status((((e as u32) & 0xFFFF) << 8) | (k as u32) | 1 << 24);
        for (rotate_at, retain_at_most) in [(1, RETAIN_AT_MOST), (32, RETAIN_AT_MOST), (1, 2)] {
            let mut chain = node();
            chain.journals.rotate_at = rotate_at;
            chain.journals.retain_at_most = retain_at_most;
            let reader = chain.reader();
            let ids = branch(&mut chain, BlockId(0), 1, 4, 1_700_000_000);
            let first = chain.snapshot().epoch + 1;
            for (k, &id) in ids.iter().enumerate() {
                chain.modify(id, |g| g.status = enc(first, k));
            }
            chain.publish_status();
            assert_eq!(chain.snapshot().epoch, first);
            let stop = Arc::new(AtomicBool::new(false));
            let (bad, checks, expired) = (
                Arc::new(AtomicU32::new(0)),
                Arc::new(AtomicU32::new(0)),
                Arc::new(AtomicU32::new(0)),
            );
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    let (reader, ids) = (reader.clone(), ids.clone());
                    let (stop, bad, checks, expired) =
                        (stop.clone(), bad.clone(), checks.clone(), expired.clone());
                    std::thread::spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            let snap = reader.snapshot();
                            for (k, &id) in ids.iter().enumerate() {
                                let got = reader.status_at(&snap, id);
                                checks.fetch_add(1, Ordering::Relaxed);
                                match got {
                                    Ok(got) if got == Some(enc(snap.epoch, k)) => {}
                                    Err(SnapshotExpired) => {
                                        expired.fetch_add(1, Ordering::Relaxed);
                                    }
                                    Ok(_) => {
                                        bad.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    })
                })
                .collect();
            // The readers run before the writes start, so the rounds race them.
            while checks.load(Ordering::Relaxed) < 64 {
                std::thread::yield_now();
            }
            for _ in 0..ROUNDS {
                let next = chain.snapshot().epoch + 1;
                for (k, &id) in ids.iter().enumerate() {
                    chain.modify(id, |g| g.status = junk(next, k));
                    chain.modify(id, |g| g.status = enc(next, k));
                }
                chain.publish_status();
            }
            stop.store(true, Ordering::Relaxed);
            threads.into_iter().for_each(|t| t.join().expect("a reader"));
            let case = (rotate_at, retain_at_most);
            let bounded = retain_at_most < RETAIN_AT_MOST;
            assert_eq!(bad.load(Ordering::Relaxed), 0, "{case:?}");
            let expired = expired.load(Ordering::Relaxed);
            assert!(bounded || expired == 0, "{case:?}: {expired}");
            let gauge = reader.journal_retention();
            assert_eq!(
                gauge.expired,
                u64::from(expired),
                "{case:?}: the gauge counts each expired read"
            );
            // Validator (D68): the bounded case must have dropped journals, or it tests no expiry.
            assert!(!bounded || gauge.oldest_epoch > 0, "{case:?}: {gauge:?}");
        }
    }

    /// Validator r2: a reader reads the entry before the journal, and the writer journals
    /// before it writes the entry. A write landing between either pair of steps must still read
    /// as of the reader's snapshot.
    #[test]
    fn a_status_write_between_the_reads_is_read_as_of_the_snapshot() {
        use crate::status_journal::tests::{Site, set_hook};
        use std::cell::Cell;
        use std::rc::Rc;

        let scripts = |g: &mut MutableGroup| {
            g.status = g.status.raise_validity(Status::VALID_SCRIPTS).0;
        };
        let setup = || {
            let mut chain = node();
            let reader = chain.reader();
            let a = branch(&mut chain, BlockId(0), 1, 1, 1_700_000_000)[0];
            let tree = reader.status(a).expect("resident");
            let before = reader.snapshot();
            (chain, reader, a, tree, before)
        };
        // The write lands between the reader's live read and its walk.
        let (mut chain, reader, a, tree, before) = setup();
        set_hook(Site::Reader, move || chain.modify(a, scripts));
        assert_eq!(reader.status_at(&before, a), Ok(Some(tree)), "a write after the live read");
        assert!(reader.status(a).expect("resident").is_valid(Status::VALID_SCRIPTS));

        // A reader runs between the writer's journal append and its entry write.
        let (mut chain, reader, a, tree, before) = setup();
        let seen = Rc::new(Cell::new(None));
        set_hook(Site::Writer, {
            let seen = seen.clone();
            move || seen.set(Some(reader.status_at(&before, a)))
        });
        chain.modify(a, scripts);
        assert_eq!(seen.get(), Some(Ok(Some(tree))), "a write the live read has not seen");
    }

    #[test]
    fn modify_writes_the_group_and_dirtiness_is_separate() {
        let mut chain = node();
        let a = add(&mut chain, BlockId(0), 1, 1_700_000_100);
        chain.dirty = Dirty::default();
        chain.modify(a, |g| {
            g.status = Status(g.status.bits() | Status::HAVE_DATA | Status::HAVE_UNDO);
            g.n_tx = 3;
            g.file = Some(2);
            g.data_pos = 8;
            g.undo_pos = 99;
        });
        assert!(!chain.dirty.contains(a), "modify does not mark dirty");
        let reader = chain.reader();
        assert_eq!(
            reader.have_data_pos(a),
            Some(FlatFilePos {
                file: 2,
                pos: 8
            })
        );
        assert_eq!(
            reader.undo_pos(a),
            Some(FlatFilePos {
                file: 2,
                pos: 99
            })
        );
        assert_eq!(reader.have_data_pos(BlockId(0)), None);
        assert_eq!(reader.entry(a).map(|e| (e.n_tx, e.file)), Some((3, Some(2))));
        chain.mark_dirty(a);
        chain.mark_dirty(a);
        chain.mark_dirty(BlockId(0));
        assert_eq!(chain.dirty.ids, vec![a, BlockId(0)]);
    }

    /// SB5-A19: `IndexAccess::hash` is Core's `GetBlockHash`; an id never issued has none.
    #[test]
    fn index_access_hash_is_get_block_hash() {
        let mut chain = node();
        let a = add(&mut chain, BlockId(0), 1, 1_700_000_100);
        let reader = chain.reader();
        assert_eq!(IndexAccess::hash(&reader, a), Some(fake_hash(1)));
        let genesis = reader.entry(BlockId(0)).expect("genesis").hash;
        assert_eq!(IndexAccess::hash(&reader, BlockId(0)), Some(genesis));
        assert_eq!(IndexAccess::hash(&reader, BlockId(99)), None);
    }

    /// A-14 r1: readers on other threads resolve every id the writer published, while it
    /// appends (R1, §13.1).
    #[test]
    fn r1_lookup_during_insert() {
        let mut chain = node();
        let reader = chain.reader();
        let published = Arc::new(AtomicU32::new(0));
        let n = 20_000u32;
        let readers: Vec<_> = (0..2)
            .map(|t| {
                let (reader, published) = (reader.clone(), published.clone());
                std::thread::spawn(move || {
                    let mut k = t;
                    loop {
                        let top = published.load(Ordering::Acquire);
                        if top > 0 {
                            k = (k * 31 + 7) % top + 1;
                            let id = reader.lookup(&fake_hash(u64::from(k))).expect("published");
                            let e = reader.entry(id).expect("resident");
                            assert_eq!(e.height, k);
                            assert_eq!(
                                reader.ancestor(id, k / 2).and_then(|a| reader.height(a)),
                                Some(k / 2)
                            );
                        }
                        if top == n {
                            break;
                        }
                    }
                })
            })
            .collect();
        let mut tip = BlockId(0);
        for k in 1..=n {
            tip = add(&mut chain, tip, u64::from(k), 1_700_000_000 + k);
            published.store(k, Ordering::Release);
        }
        for r in readers {
            r.join().expect("reader");
        }
        assert_eq!(chain.shared.index.len(), n as usize + 1);
    }

    /// A-14 r2: a reader's immutable fields never change while the writer appends, moves the
    /// tip and rewrites statuses (R2).
    #[test]
    fn r2_immutables_stable() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 500, 1_700_000_000);
        let reader = chain.reader();
        let fixed: Vec<EntryView> =
            trunk.iter().map(|&id| reader.entry(id).expect("set")).collect();
        let done = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let (reader, done, fixed) = (reader.clone(), done.clone(), fixed.clone());
                std::thread::spawn(move || {
                    while !done.load(Ordering::Relaxed) {
                        for f in &fixed {
                            let e = reader.entry(f.id).expect("resident");
                            let imm = |e: &EntryView| {
                                (
                                    e.hash,
                                    e.header,
                                    e.prev,
                                    e.height,
                                    e.chain_work,
                                    e.mtp,
                                    e.time_max,
                                )
                            };
                            assert_eq!(imm(&e), imm(f));
                            assert_eq!(e.n_tx, e.data_pos, "one write's group");
                        }
                    }
                })
            })
            .collect();
        for round in 0..20u32 {
            for &id in &trunk {
                chain.modify(id, |g| {
                    g.status = g.status.raise_validity(Status::VALID_TREE + round % 3).0;
                    g.n_tx = round;
                    g.data_pos = round;
                });
            }
            branch(&mut chain, trunk[(round * 7) as usize], 10_000 + u64::from(round) * 10, 3, 1);
            let r = round as usize;
            connect_all(&mut chain, &trunk[r * 25..(r + 1) * 25]);
        }
        done.store(true, Ordering::Relaxed);
        for r in readers {
            r.join().expect("reader");
        }
    }

    /// A-14 r3: every tip writer publishes before it returns, and a status change reaches
    /// readers through `publish_status` (R3).
    #[test]
    fn r3_snapshot_before_reply() {
        let mut chain = node();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 5, 1_700_000_000);
        let step = chain.connect_tip(trunk[0]).expect("connect");
        let snap = reader.snapshot();
        assert_eq!((snap.tip.tip, snap.tip_seq), (trunk[0], step.seq()));
        let change = chain.set_active_tip(trunk[4]).expect("connect four");
        assert!(Arc::ptr_eq(&change.snap, &reader.snapshot()));
        assert_eq!(change.snap.tip.tip, trunk[4]);

        // A header with more work moves best_header; the node publishes it with the same tip.
        let side = branch(&mut chain, trunk[4], 100, 2, 1_700_000_100);
        let before = reader.snapshot();
        assert_eq!(before.tip.best_header, trunk[4]);
        chain.publish_status();
        let after = reader.snapshot();
        assert_eq!((after.tip.best_header, after.tip.best_header_height), (side[1], 7));
        assert_eq!((after.tip.tip, after.tip_seq), (before.tip.tip, before.tip_seq));
        assert_eq!(after.epoch, before.epoch + 1);

        // SPV: the step's one snapshot is the reply's.
        let mut chain = spv();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 5, 1_700_000_000);
        chain.tip_plan(trunk[4]).expect("plan");
        let change = chain.set_active_tip(trunk[4]).expect("run the plan");
        assert!(Arc::ptr_eq(&change.snap, &reader.snapshot()));
        assert_eq!((change.snap.tip.tip, change.snap.tip.best_header), (trunk[4], trunk[4]));
    }

    /// A-14 r4: IBD follows the tip's work and age against the mock clock, then latches (R4).
    /// Both comparisons are strict, as in Core: work equal to the minimum and a tip exactly
    /// `max_tip_age` old leave IBD.
    #[test]
    fn r4_ibd_mocktime_flip() {
        let proof = block_proof(CompactTarget::new(0x207f_ffff));
        let max_age = 6 * 60 * 60;
        let tip_time = 1_800_000_000u32;
        // A fresh chain with `n` connected blocks at `tip_time`; the minimum is genesis + 3.
        let fresh = |n: usize| {
            let mut chain = with_cfg(|p| ChainConfig {
                min_chain_work: Some(proof * 4u32),
                ..ChainConfig::node(p)
            });
            chain.init_active_tip(BlockId(0)).expect("Core LoadChainTip");
            let trunk = branch(&mut chain, BlockId(0), 1, n as u64, tip_time);
            connect_all(&mut chain, &trunk);
            if n > 0 {
                let last = *trunk.last().expect("n > 0");
                assert_eq!(
                    chain.reader().entry(last).map(|e| e.header.time),
                    Some(tip_time + n as u32 - 1)
                );
            }
            chain
        };
        let ibd = |chain: &HeaderChain, now: i64| {
            chain.reader().is_initial_block_download(&chain.snapshot(), NodeSeconds(now), false)
        };
        // The tip of `fresh(3)` is at tip_time + 2.
        let edge = i64::from(tip_time + 2) + max_age;
        assert!(ibd(&fresh(0), edge - 100), "genesis is below the minimum work");
        assert!(ibd(&fresh(2), edge - 100), "one proof short of the minimum");
        assert!(!ibd(&fresh(3), edge), "work equal to the minimum, tip exactly max_tip_age old");
        assert!(ibd(&fresh(3), edge + 1), "one second older than max_tip_age");
        // Core compares against `NodeClock::now()` as is: a nanosecond past is too old.
        let at = |chain: &HeaderChain, nanos| {
            let now = NodeTime::new(edge, nanos);
            chain.reader().is_initial_block_download(&chain.snapshot(), now, false)
        };
        assert!(at(&fresh(3), 1), "a nanosecond older than max_tip_age");
        assert!(!at(&fresh(3), 0), "exactly max_tip_age old");

        // Latched: an old tip, loading blocks or a later clock no longer matter.
        let chain = fresh(3);
        let reader = chain.reader();
        assert!(ibd(&chain, edge + 1));
        assert!(!ibd(&chain, edge - 20), "a younger clock flips it");
        assert!(!reader.is_initial_block_download(&chain.snapshot(), NodeSeconds(edge + 1), true));
        assert!(!ibd(&chain, edge + 1_000_000));
        assert_eq!(reader.min_chain_work(), proof * 4u32);

        // Loading blocks keeps an unlatched chain in IBD.
        let chain = node();
        let now = i64::from(chain.snapshot().tip.tip_time);
        assert!(chain.reader().is_initial_block_download(
            &chain.snapshot(),
            NodeSeconds(now),
            true
        ));
        assert!(!ibd(&chain, now), "regtest's minimum work is zero");
    }

    /// D-C5-a: `init_active_tip` again (CH1-node's `LoadChainTip`) rebuilds the active chain,
    /// restarts `seq` at 1 with `tip_seq` 0, bumps the epoch and drops a pending plan.
    #[test]
    fn init_active_tip_again_restarts_seq() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 8, 1_700_000_000);
        connect_all(&mut chain, &trunk[..3]);
        assert_eq!(chain.snapshot().tip_seq, 3);
        chain.init_active_tip(trunk[6]).expect("set");
        let snap = chain.snapshot();
        assert_eq!(
            (snap.tip.tip, snap.tip_seq, snap.epoch, snap.active.height()),
            (trunk[6], 0, 5, 7)
        );
        assert_eq!(snap.active.at(4), Some(trunk[3]));
        assert_eq!(chain.connect_tip(trunk[7]).expect("connect").seq(), 1);
        assert!(matches!(chain.init_active_tip(BlockId(99)), Err(ChainError::UnknownBlock(_))));

        let mut chain = spv();
        let trunk = branch(&mut chain, BlockId(0), 1, 3, 1_700_000_000);
        chain.tip_plan(trunk[2]).expect("plan");
        chain.init_active_tip(trunk[0]).expect("set");
        assert!(matches!(chain.set_active_tip(trunk[2]), Err(ChainError::Internal(_))), "dropped");
        // SPV status changes ride on the step's snapshot: publish_status publishes nothing.
        let epoch = chain.snapshot().epoch;
        chain.publish_status();
        assert_eq!(chain.snapshot().epoch, epoch);
    }

    /// A-14 r5: `best_header` is set from construction and published with the first snapshot
    /// (R5).
    #[test]
    fn r5_best_header_nonnull() {
        // The node's helper ran `init_active_tip` after its pre-genesis publication.
        for (chain, epoch) in [(node(), 1), (spv(), 0)] {
            let snap = chain.reader().snapshot();
            assert_eq!(chain.best_header(), BlockId(0));
            assert_eq!((snap.tip.tip, snap.tip.best_header), (BlockId(0), BlockId(0)));
            assert_eq!((snap.tip_seq, snap.epoch, snap.active.height()), (0, epoch, 0));
        }
    }

    /// CH1-A11: the node starts with no tip, as Core's `m_chain` before `LoadChainTip` or the
    /// genesis `ConnectTip`; the published types keep a genesis placeholder.
    #[test]
    fn node_starts_without_a_tip() {
        let mut chain = node_pre_genesis();
        let child = add(&mut chain, BlockId(0), 1, 1_700_000_000);
        let reader = chain.reader();
        let snap = reader.snapshot();
        assert!(!snap.has_tip());
        assert_eq!((snap.tip.tip, snap.tip_seq, snap.epoch), (BlockId(0), 0, 0));
        assert!(reader.is_initial_block_download(&snap, NodeSeconds(0), false), "Core: no tip");
        assert_eq!(snap.active.find_fork(BlockId(0), &reader), None);
        assert_internal(chain.tip_plan(child));
        assert_internal(chain.chain_tips());
        assert_internal(chain.disconnect_tip());
        assert_internal(chain.connect_tip(child));
        assert!(
            chain.check_index().is_err(),
            "Core: no active chain with more than the genesis entry"
        );

        // The genesis ConnectTip: seq 1, then the chain grows as usual.
        let step = chain.connect_tip(BlockId(0)).expect("genesis");
        assert!(matches!(
            step,
            TipStep::Connected {
                seq: 1,
                ..
            }
        ));
        assert_eq!(step.block().id, BlockId(0));
        let snap = reader.snapshot();
        assert!(snap.has_tip());
        assert_eq!(
            (snap.tip.tip, snap.tip_seq, snap.epoch, snap.active.height()),
            (BlockId(0), 1, 1, 0)
        );
        assert_internal(chain.connect_tip(BlockId(0)));
        assert_eq!(chain.connect_tip(child).expect("child").seq(), 2);
        assert_eq!(chain.check_index(), Ok(()));
    }

    /// CH1-A1/A13: a hold's steps take their own `seq` and publish nothing; readers keep the
    /// pre-hold snapshot until the release stores one.
    #[test]
    fn a_hold_publishes_once_at_release() {
        let mut chain = node();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 6, 1_700_000_000);
        let fork = branch(&mut chain, trunk[2], 100, 3, 1_700_000_050);
        connect_all(&mut chain, &trunk[..5]);
        let before = reader.snapshot();
        assert_eq!((before.tip_seq, before.epoch), (5, 6));

        let mut hold = chain.hold_tip();
        assert_eq!(hold.disconnect_tip().expect("down").seq(), 6);
        assert_eq!(hold.disconnect_tip().expect("down").seq(), 7);
        for (k, &id) in fork.iter().enumerate() {
            assert_eq!(hold.connect_tip(id).expect("up").seq(), 8 + k as u64);
        }
        assert_internal(hold.connect_tip(trunk[5]));
        // Readers and status publication wait for the release; tip-lock calls refuse.
        assert_eq!(reader.snapshot().epoch, before.epoch);
        hold.chain().publish_status();
        assert_eq!(reader.snapshot().epoch, before.epoch, "CH1-A13");
        let c = hold.chain();
        assert_internal(c.connect_tip(trunk[5]));
        assert_internal(c.disconnect_tip());
        assert_internal(c.set_active_tip(trunk[5]));
        assert_internal(c.init_active_tip(trunk[0]));
        let snap = hold.release();
        assert_eq!((snap.tip.tip, snap.tip_seq, snap.epoch), (fork[2], 10, 7));
        assert!(Arc::ptr_eq(&snap, &reader.snapshot()));
        assert_eq!((snap.active.at(3), snap.active.at(4)), (Some(trunk[2]), Some(fork[0])));

        // An empty hold publishes the pre-hold seq; outside a hold, calls work again.
        let snap = chain.hold_tip().release();
        assert_eq!((snap.tip_seq, snap.epoch), (10, 8));
        chain.publish_status();
        assert_eq!(reader.snapshot().epoch, 9);
        assert_eq!(chain.disconnect_tip().expect("down").seq(), 11);
    }

    /// CH1-A1: an abandoned (or dropped) hold publishes nothing; its steps stay taken, and the
    /// next publication shows them.
    #[test]
    fn an_abandoned_hold_publishes_nothing() {
        let mut chain = node();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 4, 1_700_000_000);
        let mut hold = chain.hold_tip();
        hold.connect_tip(trunk[0]).expect("up");
        hold.abandon();
        assert_eq!((reader.snapshot().tip.tip, reader.snapshot().tip_seq), (BlockId(0), 0));
        {
            let mut hold = chain.hold_tip();
            hold.connect_tip(trunk[1]).expect("up");
        }
        assert_eq!(reader.snapshot().tip_seq, 0);
        // A status publication shows the abandoned steps with their seq (F2).
        chain.publish_status();
        assert_eq!((reader.snapshot().tip.tip, reader.snapshot().tip_seq), (trunk[1], 2));
        assert_eq!(chain.connect_tip(trunk[2]).expect("up").seq(), 3);
        let snap = reader.snapshot();
        assert_eq!((snap.tip.tip, snap.tip_seq, snap.active.height()), (trunk[2], 3, 3));

        // Before the first tip, the genesis ConnectTip can run inside a hold too (CH1-A11).
        let mut chain = node_pre_genesis();
        let mut hold = chain.hold_tip();
        assert_eq!(hold.connect_tip(BlockId(0)).expect("genesis").seq(), 1);
        let snap = hold.release();
        assert!(snap.has_tip());
        assert_eq!((snap.tip_seq, snap.epoch), (1, 1));
    }

    /// A hold inside a hold would deadlock on the tip lock; it panics instead.
    #[test]
    #[should_panic(expected = "hold_tip inside a hold")]
    fn a_nested_hold_panics() {
        let mut chain = node();
        let mut hold = chain.hold_tip();
        let _ = hold.chain().hold_tip();
    }

    /// CH1-A2/A3/A8: the writer's reads follow its private active chain, which leads the
    /// published snapshot inside a hold.
    #[test]
    fn private_reads_follow_the_writer_inside_a_hold() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 4, 1_700_000_000);
        let fork = branch(&mut chain, trunk[0], 100, 2, 1_700_000_050);
        connect_all(&mut chain, &trunk[..3]);
        assert_eq!(chain.private_tip().map(|(b, s)| (b.id, s)), Some((trunk[2], 3)));

        let mut hold = chain.hold_tip();
        hold.disconnect_tip().expect("down");
        hold.disconnect_tip().expect("down");
        hold.connect_tip(fork[0]).expect("up");
        let c = hold.chain();
        assert_eq!(c.snapshot().tip.tip, trunk[2], "readers see the pre-hold tip");
        assert_eq!(c.private_tip().map(|(b, s)| (b.id, b.height, s)), Some((fork[0], 2, 6)));
        assert!(c.active_contains_private(fork[0]) && c.active_contains_private(trunk[0]));
        assert!(!c.active_contains_private(trunk[1]));
        assert_eq!((c.ancestor_private(1), c.ancestor_private(2)), (Some(trunk[0]), Some(fork[0])));
        assert_eq!(c.ancestor_private(3), None);
        assert_eq!(c.find_fork_private(trunk[3]), Some(trunk[0]));
        assert_eq!(c.find_fork_private(fork[1]), Some(fork[0]));
        // Regtest's minimum work is zero; IBD turns on the tip's age.
        assert!(c.is_ibd_private(NodeSeconds(4_000_000_000), false));
        assert!(c.is_ibd_private(NodeSeconds(1_700_000_050), true), "loading");
        assert!(!c.is_ibd_private(NodeSeconds(1_700_000_050), false), "the latch flips");
        assert!(!c.reader().is_initial_block_download(&c.snapshot(), NodeSeconds(0), true));
        hold.release();

        let ids: Vec<BlockId> = chain.ids().collect();
        assert_eq!(ids, (0..7).map(BlockId).collect::<Vec<_>>());

        // No tip yet: Core's null m_chain.Tip().
        let chain = node_pre_genesis();
        assert_eq!(chain.private_tip(), None);
        assert!(!chain.active_contains_private(BlockId(0)));
        assert_eq!((chain.ancestor_private(0), chain.find_fork_private(BlockId(0))), (None, None));
        assert!(chain.is_ibd_private(NodeSeconds(0), false));
    }

    /// The best-header hooks (`validation.cpp:2043-2044`, `:3951`), on the writer's chain.
    #[test]
    fn best_header_hooks_follow_core() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 4, 1_700_000_000);
        let side = branch(&mut chain, trunk[0], 100, 2, 1_700_000_050);
        connect_all(&mut chain, &trunk[..2]);
        assert_eq!(chain.best_header(), trunk[3]);
        assert!(!chain.best_header_walk_back(trunk[2]));
        assert_eq!(chain.best_header(), trunk[3], "not the best header: unchanged");
        assert!(chain.best_header_walk_back(trunk[3]));
        assert_eq!(chain.best_header(), trunk[2]);

        // An invalid block off best_header's chain changes nothing; one on it moves
        // best_header to the writer's tip: inside a hold, the private one.
        chain.best_header_invalid_chain(side[1]);
        assert_eq!(chain.best_header(), trunk[2]);
        let mut hold = chain.hold_tip();
        hold.disconnect_tip().expect("down");
        hold.chain().best_header_invalid_chain(trunk[2]);
        assert_eq!(hold.chain().best_header(), trunk[0], "Core: m_chain.Tip()");
        assert_eq!(hold.chain().snapshot().tip.tip, trunk[1], "not the published tip");
        let snap = hold.release();
        assert_eq!(snap.tip.best_header, trunk[0]);
        let hash = chain.reader().entry(trunk[0]).expect("resident").hash;
        assert_eq!(snap.tip.best_header_hash, hash, "SB5-A17");

        // R5: the genesis entry never walks back to nothing.
        let mut chain = node();
        assert!(chain.best_header_walk_back(BlockId(0)), "Core's branch ran");
        assert_eq!(chain.best_header(), BlockId(0));
    }

    /// CH1-A9: the visitor sees every entry after its parent, with Core's `pindexFirst*`
    /// state there, and its error ends the walk.
    #[test]
    fn check_index_with_carries_cores_walk_state() {
        struct Record(Vec<(BlockId, FirstAncestors)>, Option<BlockId>);
        impl IndexVisitor for Record {
            fn visit(
                &mut self,
                e: &EntryView,
                f: &FirstAncestors,
            ) -> Result<(), IndexInconsistency> {
                if let Some(p) = e.prev {
                    assert!(self.0.iter().any(|&(id, _)| id == p), "the parent came first");
                }
                self.0.push((e.id, *f));
                match self.1 == Some(e.id) {
                    true => Err(IndexInconsistency {
                        id: e.id,
                        what: "the visitor's rule",
                    }),
                    false => Ok(()),
                }
            }
        }
        let mut chain = node();
        let g = BlockId(0);
        let trunk = branch(&mut chain, g, 1, 4, 1_700_000_000);
        let side = branch(&mut chain, trunk[0], 100, 1, 1_700_000_050);
        let with_data = |c: &mut HeaderChain, id, validity| {
            c.modify(id, |m| {
                m.status =
                    Status(m.status.bits() & !Status::VALID_MASK | validity | Status::HAVE_DATA);
                m.n_tx = 1;
            })
        };
        with_data(&mut chain, g, Status::VALID_TREE);
        with_data(&mut chain, trunk[0], Status::VALID_SCRIPTS);
        with_data(&mut chain, trunk[1], Status::VALID_TRANSACTIONS);
        chain.modify(trunk[3], |m| m.status = Status(m.status.bits() | Status::FAILED_VALID));
        chain.modify(side[0], |m| m.status = Status(m.status.bits() | Status::CONFLICT_CHAINLOCK));

        let mut seen = Record(Vec::new(), None);
        assert_eq!(chain.check_index_with(&mut seen), Ok(()));
        assert_eq!(seen.0.len(), 6);
        let at = |id| seen.0.iter().find(|&&(i, _)| i == id).expect("visited").1;
        assert_eq!(at(g), FirstAncestors::default(), "the not_* fields skip genesis");
        assert_eq!(at(trunk[0]), FirstAncestors::default());
        let t1 = at(trunk[1]);
        assert_eq!((t1.not_chain_valid, t1.not_scripts_valid), (Some(trunk[1]), Some(trunk[1])));
        assert_eq!((t1.missing, t1.never_processed, t1.not_transactions_valid), (None, None, None));
        let t3 = at(trunk[3]);
        assert_eq!((t3.missing, t3.never_processed), (Some(trunk[2]), Some(trunk[2])));
        assert_eq!(
            (t3.not_transactions_valid, t3.not_chain_valid),
            (Some(trunk[2]), Some(trunk[1]))
        );
        assert_eq!((t3.invalid, t3.conflicting), (Some(trunk[3]), None));
        let s0 = at(side[0]);
        assert_eq!((s0.invalid, s0.conflicting, s0.missing), (None, Some(side[0]), Some(side[0])));

        // FAILED_CHILD alone is not pindexFirstInvalid.
        let mut e = chain.reader().entry(trunk[2]).expect("resident");
        e.status = Status(e.status.bits() | Status::FAILED_CHILD);
        assert_eq!(FirstAncestors::default().extend(&e).invalid, None);

        let mut stop = Record(Vec::new(), Some(trunk[1]));
        let err = chain.check_index_with(&mut stop).expect_err("the visitor refused");
        assert_eq!(err.what, "the visitor's rule");
        assert!(stop.0.iter().all(|&(id, _)| id != trunk[3]), "the walk ended there");
    }

    /// SB5-A13, D36: an SPV status change waits in the overlay: the writer reads it first,
    /// readers see it at stage 4, and stage 5 drops it. The node writes the entry, not dirty.
    #[test]
    fn spv_status_changes_wait_in_the_overlay() {
        let conflict = |s: Status| Status(s.bits() | Status::CONFLICT_CHAINLOCK);
        let marked = |r: &ChainReader, id| {
            r.entry(id).expect("resident").status.has(Status::CONFLICT_CHAINLOCK)
        };
        let mut chain = spv();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 3, 1_700_000_000);
        chain.set_status(trunk[2], conflict).expect("resident");
        let status = chain.writer_status(trunk[2]).expect("resident");
        assert!(status.has(Status::CONFLICT_CHAINLOCK), "the writer reads the overlay first");
        assert!(!marked(&reader, trunk[2]), "readers never read the overlay");
        assert_eq!(chain.overlay_marks().collect::<Vec<_>>(), vec![(trunk[2], status)]);
        assert!(matches!(
            chain.set_status(BlockId(99), conflict),
            Err(ChainError::UnknownBlock(_))
        ));

        chain.tip_plan(trunk[1]).expect("plan");
        chain.set_active_tip(trunk[1]).expect("stage 4");
        assert!(marked(&reader, trunk[2]), "applied with the step's snapshot");
        assert_eq!(chain.overlay_marks().len(), 1, "kept until the commit");
        chain.records_committed();
        assert_eq!(chain.overlay_marks().len(), 0);
        assert!(chain.writer_status(trunk[2]).expect("resident").has(Status::CONFLICT_CHAINLOCK));

        let mut chain = node();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 1, 1_700_000_000);
        chain.take_dirty();
        chain.set_status(trunk[0], conflict).expect("resident");
        assert!(marked(&reader, trunk[0]));
        assert_eq!(chain.overlay_marks().len(), 0);
        assert!(chain.take_dirty().is_empty(), "D15: the caller marks dirty at Core's sites");
    }

    /// mi-2: stage 5 drops the marks a stage 4 applied, and the dirty set, and keeps a mark
    /// made since (the next stage 4's).
    #[test]
    fn records_committed_drops_the_applied_marks_only() {
        let conflict = |s: Status| Status(s.bits() | Status::CONFLICT_CHAINLOCK);
        let mut chain = spv();
        let trunk = branch(&mut chain, BlockId(0), 1, 3, 1_700_000_000);
        chain.set_status(trunk[0], conflict).expect("resident");
        chain.tip_plan(trunk[2]).expect("plan");
        chain.set_active_tip(trunk[2]).expect("stage 4");
        chain.set_status(trunk[1], conflict).expect("resident");
        assert!(!chain.dirty.ids().is_empty());
        chain.records_committed();
        let kept: Vec<_> = chain.overlay_marks().map(|(id, _)| id).collect();
        assert_eq!(kept, vec![trunk[1]], "the applied mark goes, the later one stays");
        assert!(chain.dirty.ids().is_empty());
        let reader = chain.reader();
        for id in [trunk[0], trunk[1]] {
            assert!(chain.writer_status(id).expect("resident").has(Status::CONFLICT_CHAINLOCK));
        }
        assert!(reader.entry(trunk[0]).expect("resident").status.has(Status::CONFLICT_CHAINLOCK));
        assert!(!reader.entry(trunk[1]).expect("resident").status.has(Status::CONFLICT_CHAINLOCK));
    }

    /// SB5-A11 (c): `tip_target` sizes a change without building it; `tip_plan_bounded`
    /// builds it at exact capacity only within its bound, and agrees with `tip_plan`.
    #[test]
    fn tip_target_sizes_and_bounded_plans_build() {
        for mut chain in [node(), spv()] {
            let trunk = branch(&mut chain, BlockId(0), 1, 6, 1_700_000_000);
            let fork = branch(&mut chain, trunk[1], 100, 4, 1_700_000_050);
            match chain.cfg.mode {
                ChainMode::Node => connect_all(&mut chain, &trunk[..5]),
                ChainMode::Spv => {
                    chain.tip_plan(trunk[4]).expect("plan");
                    chain.set_active_tip(trunk[4]).expect("step");
                }
            }
            let t = chain.tip_target(fork[3]).expect("target");
            assert_eq!((t.fork.id, t.fork.height, t.d, t.c), (trunk[1], 2, 3, 4));
            let up = chain.tip_target(trunk[5]).expect("target");
            assert_eq!((up.fork.id, up.d, up.c), (trunk[4], 0, 1));
            let down = chain.tip_target(trunk[0]).expect("target");
            assert_eq!((down.fork.id, down.d, down.c), (trunk[0], 4, 0));
            assert!(matches!(chain.tip_target(BlockId(99)), Err(ChainError::UnknownBlock(_))));

            assert_internal(chain.tip_plan_bounded(fork[3], 6));
            let plan = chain.tip_plan_bounded(fork[3], 7).expect("within the bound");
            assert_eq!((plan.disconnect.len(), plan.connect.len()), (3, 4));
            let ids = |v: &[BlockRef]| v.iter().map(|b| b.id).collect::<Vec<_>>();
            assert_eq!(ids(&plan.disconnect), vec![trunk[4], trunk[3], trunk[2]]);
            assert_eq!(ids(&plan.connect), fork[..4].to_vec());
            assert_eq!(plan, chain.tip_plan(fork[3]).expect("plan"));
        }
        assert_internal(node_pre_genesis().tip_target(BlockId(0)));
    }

    /// SB5-A12, SB5 §4.8: an SPV composite in sub-steps inside one hold. Each sub-step takes
    /// consecutive seqs within its bound; readers keep the pre-hold snapshot, and the overlay
    /// reaches readers only at the release, with the final tip.
    #[test]
    fn spv_sub_steps_inside_a_hold_publish_once() {
        let mut chain = spv();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 5, 1_700_000_000);
        let fork = branch(&mut chain, trunk[1], 100, 4, 1_700_000_050);
        chain.tip_plan(trunk[4]).expect("plan");
        let before = chain.set_active_tip(trunk[4]).expect("step").snap;
        assert_eq!((before.tip_seq, before.epoch), (5, 1));
        chain.set_status(fork[0], |s| Status(s.bits() | Status::FAILED_CHILD)).expect("mark");

        let mut hold = chain.hold_tip();
        let ids = |v: &[BlockRef]| v.iter().map(|b| b.id).collect::<Vec<_>>();
        assert_internal(hold.set_active_tip(fork[3], 6));
        let s1 = hold.set_active_tip(trunk[2], 2).expect("disconnects");
        assert_eq!((ids(&s1.disconnected), s1.last_seq), (vec![trunk[4], trunk[3]], 7));
        assert!(Arc::ptr_eq(&s1.snap, &before), "the pre-hold snapshot");
        let s2 = hold.set_active_tip(trunk[1], 2).expect("to the fork");
        assert_eq!((ids(&s2.disconnected), s2.last_seq), (vec![trunk[2]], 8));
        let s3 = hold.set_active_tip(fork[1], 2).expect("connects");
        assert_eq!((ids(&s3.connected), s3.last_seq), (fork[..2].to_vec(), 10));
        let s4 = hold.set_active_tip(fork[3], 2).expect("connects");
        assert_eq!((ids(&s4.connected), s4.last_seq), (fork[2..4].to_vec(), 12));
        assert_eq!(reader.snapshot().tip.tip, trunk[4], "nothing published in the hold");
        assert!(!reader.entry(fork[0]).expect("resident").status.has(Status::FAILED_CHILD));

        let snap = hold.release();
        assert_eq!((snap.tip.tip, snap.tip_seq, snap.epoch), (fork[3], 12, 2));
        assert!(reader.entry(fork[0]).expect("resident").status.has(Status::FAILED_CHILD));

        // An abandoned hold applies no mark: readers and the overlay keep them apart.
        chain.set_status(fork[1], |s| Status(s.bits() | Status::FAILED_CHILD)).expect("mark");
        let mut hold = chain.hold_tip();
        hold.set_active_tip(fork[2], 1).expect("down");
        hold.abandon();
        assert_eq!(reader.snapshot().tip.tip, fork[3]);
        assert!(!reader.entry(fork[1]).expect("resident").status.has(Status::FAILED_CHILD));
        assert_eq!(chain.overlay_marks().len(), 2, "kept for the next stage 4");

        let mut chain = node();
        let mut hold = chain.hold_tip();
        assert!(matches!(hold.set_active_tip(BlockId(0), 1), Err(ChainError::Config(_))));
    }

    /// §9.1 stage 4 (Opus r1 mi-4, M16): a plan the tip moved away from since `tip_plan` is
    /// refused even though it still ends at the target. Here a hold's sub-step moves the tip
    /// between `tip_plan` and `set_active_tip`, which would otherwise connect an entry twice.
    #[test]
    fn spv_refuses_a_plan_the_tip_moved_away_from() {
        let mut chain = spv();
        let trunk = branch(&mut chain, BlockId(0), 1, 5, 1_700_000_000);
        chain.tip_plan(trunk[2]).expect("plan");
        chain.set_active_tip(trunk[2]).expect("step");
        let stale = chain.tip_plan(trunk[4]).expect("plan");
        assert_eq!((stale.fork.id, stale.connect.len()), (trunk[2], 2));

        let mut hold = chain.hold_tip();
        hold.set_active_tip(trunk[3], 1).expect("one connect");
        let snap = hold.release();
        assert_eq!(snap.tip.tip, trunk[3]);
        assert_internal(chain.set_active_tip(trunk[4]));
        assert_eq!(chain.private_tip().expect("a tip").0.id, trunk[3], "nothing ran");
        assert_eq!(chain.reader().snapshot().tip_seq, snap.tip_seq, "nothing published");

        chain.tip_plan(trunk[4]).expect("plan");
        let change = chain.set_active_tip(trunk[4]).expect("a fresh plan runs");
        assert_eq!(change.connected.iter().map(|b| b.id).collect::<Vec<_>>(), vec![trunk[4]]);
        let active = (chain.active.len(), chain.active.get(4), chain.active.get(5));
        assert_eq!(active, (6, Some(trunk[3]), Some(trunk[4])), "each entry once");
    }

    /// Opus r1 n-4: a panic inside SPV `set_active_tip`'s critical section trips the latch
    /// before the tip lock is released. A plan whose disconnect leg names a parent off the
    /// active chain breaks `step`'s invariant (a debug assertion).
    #[cfg(debug_assertions)]
    #[test]
    fn an_spv_step_that_unwinds_trips_the_latch() {
        use crate::error::FailStop as _;
        let latch = Arc::new(crate::testing::Latch::default());
        let mut chain = with_cfg(|p| ChainConfig::spv(p, 10_000));
        chain.svc.fail = latch.clone();
        let trunk = branch(&mut chain, BlockId(0), 1, 3, 1_700_000_000);
        let side = branch(&mut chain, trunk[0], 100, 1, 1_700_000_050)[0];
        chain.tip_plan(trunk[2]).expect("plan");
        chain.set_active_tip(trunk[2]).expect("step");
        let (tip, off) = (chain.block_ref(trunk[2]), chain.block_ref(side));
        *chain.pending_plan.lock() = Some(TipPlan {
            fork: off,
            disconnect: vec![tip],
            connect: vec![],
        });
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            chain.set_active_tip(side).map(|_| ())
        }));
        assert!(r.is_err(), "step's invariant");
        assert!(latch.is_tripped(), "tripped inside the critical section");
        assert!(chain.shared.readers.tip().state.try_lock().is_some(), "the guard dropped");

        // A hold dropped by an unwind trips it too; one ended normally or abandoned does not.
        for unwind in [false, true] {
            let latch = Arc::new(crate::testing::Latch::default());
            let mut chain = node();
            chain.svc.fail = latch.clone();
            chain.hold_tip().abandon();
            chain.hold_tip().release();
            assert!(!latch.is_tripped());
            if unwind {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _hold = chain.hold_tip();
                    panic!("a broken invariant inside the hold");
                }));
                assert!(r.is_err() && latch.is_tripped());
                assert!(!chain.held, "the flag is cleared");
            }
        }
    }

    /// SB5 F0's static assertion: the hold's guard is never in the chain, so the writer can
    /// move between threads (CH1-A1).
    #[test]
    fn the_chain_is_send() {
        fn send<T: Send>() {}
        send::<HeaderChain>();
        send::<HeaderChain<NoStore, crate::Evicting>>();
    }

    /// A-14 r6: the node publishes each connect and disconnect as its own step; SPV runs its
    /// pending plan and publishes once (§9.1).
    #[test]
    fn r6_single_steps() {
        let mut chain = node();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 6, 1_700_000_000);
        let fork = branch(&mut chain, trunk[2], 100, 5, 1_700_000_050);
        connect_all(&mut chain, &trunk);
        let start = reader.snapshot();
        assert_eq!((start.tip_seq, start.epoch, start.tip.tip), (6, 7, trunk[5]));

        let step = chain.disconnect_tip().expect("above genesis");
        let snap = reader.snapshot();
        assert_eq!(step.block().id, trunk[5]);
        assert_eq!((snap.tip.tip, snap.tip_seq, snap.epoch), (trunk[4], 7, 8));
        // connect_tip needs the tip as prev.
        assert_internal(chain.connect_tip(fork[1]));
        assert_eq!(reader.snapshot().epoch, 8, "a refused step publishes nothing");

        let plan = chain.tip_plan(fork[4]).expect("plan");
        assert_eq!(plan.fork.id, trunk[2]);
        assert_eq!(plan.disconnect.iter().map(|b| b.id).collect::<Vec<_>>(), [trunk[4], trunk[3]]);
        assert_eq!(plan.connect.iter().map(|b| b.id).collect::<Vec<_>>(), fork);
        let change = chain.set_active_tip(fork[4]).expect("reorg");
        assert_eq!((change.last_seq, change.snap.epoch), (14, 15), "one publication per step");
        assert_eq!(change.connected, plan.connect);
        assert_eq!(chain.set_active_tip(fork[4]).expect("no-op").last_seq, 14);
        assert_eq!(reader.snapshot().epoch, 15, "a node no-op publishes nothing");
        let down = chain.set_active_tip(BlockId(0)).expect("to genesis");
        assert_eq!(down.disconnected.len(), 8);
        assert_internal(chain.disconnect_tip());

        // SPV: no per-step entry points; one publication for the whole plan.
        let mut chain = spv();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 6, 1_700_000_000);
        let fork = branch(&mut chain, trunk[2], 100, 5, 1_700_000_050);
        assert!(matches!(
            chain.connect_tip(trunk[0]),
            Err(ChainError::Config(ConfigError::WrongMode(_)))
        ));
        assert!(matches!(chain.disconnect_tip(), Err(ChainError::Config(_))));
        assert_internal(chain.set_active_tip(trunk[5]));
        chain.tip_plan(trunk[5]).expect("plan");
        let change = chain.set_active_tip(trunk[5]).expect("plan");
        assert_eq!((change.last_seq, change.snap.epoch, change.connected.len()), (6, 1, 6));
        chain.tip_plan(fork[4]).expect("plan");
        assert!(
            matches!(chain.set_active_tip(fork[3]), Err(ChainError::Internal(_))),
            "a different target than the plan's"
        );
        assert!(matches!(chain.set_active_tip(fork[4]), Err(ChainError::Internal(_))), "taken");
        chain.tip_plan(fork[4]).expect("plan");
        let change = chain.set_active_tip(fork[4]).expect("reorg");
        assert_eq!((change.last_seq, change.snap.epoch), (14, 2));
        assert_eq!((change.disconnected.len(), change.connected.len()), (3, 5));
        assert_eq!(reader.snapshot().tip.tip, fork[4]);
        // An empty plan still publishes the step's status (§9.1 stage 4).
        chain.tip_plan(fork[4]).expect("plan");
        let change = chain.set_active_tip(fork[4]).expect("no step");
        assert_eq!((change.last_seq, change.snap.epoch), (14, 3));
    }

    /// A-14 r8: a held snapshot keeps its tip and active chain across a reorg (R8).
    #[test]
    fn r8_reorg_between_loads() {
        let mut chain = node();
        let reader = chain.reader();
        let trunk = branch(&mut chain, BlockId(0), 1, 1500, 1_700_000_000);
        let fork = branch(&mut chain, trunk[1000], 10_000, 600, 1_700_000_000);
        connect_all(&mut chain, &trunk);
        let held = reader.snapshot();
        chain.set_active_tip(fork[599]).expect("reorg");
        let now = reader.snapshot();
        assert_eq!((held.tip.tip, held.active.height(), held.epoch), (trunk[1499], 1500, 1501));
        assert_eq!(held.active.at(1400), Some(trunk[1399]));
        assert_eq!(now.active.at(1400), Some(fork[398]));
        assert_eq!(held.active.at(1001), now.active.at(1001));
        assert!(
            held.active.contains(trunk[1200], &reader)
                && !now.active.contains(trunk[1200], &reader)
        );
        assert_eq!(now.epoch, held.epoch + 499 + 600);
    }

    /// `CChain::Contains`, `Next`, `FindFork` and `FindForkInGlobalIndex` over a view.
    #[test]
    fn active_view_walks_follow_core() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 30, 1_700_000_000);
        let fork = branch(&mut chain, trunk[9], 100, 30, 1_700_000_000);
        connect_all(&mut chain, &trunk);
        let (snap, r) = (chain.snapshot(), chain.reader());
        let v = &snap.active;
        assert_eq!((v.height(), v.tip(), v.base(), v.at(0)), (30, trunk[29], 0, Some(BlockId(0))));
        assert!(v.contains(trunk[3], &r) && !v.contains(fork[0], &r));
        assert_eq!((v.next(trunk[3], &r), v.next(trunk[29], &r)), (Some(trunk[4]), None));
        assert_eq!(v.next(fork[0], &r), None);
        assert_eq!(v.find_fork(fork[29], &r), Some(trunk[9]), "a longer branch");
        assert_eq!(v.find_fork(fork[2], &r), Some(trunk[9]));
        assert_eq!(v.find_fork(trunk[20], &r), Some(trunk[20]));
        let hash = |id| r.entry(id).expect("set").hash;
        let locator = [fake_hash(999), hash(fork[5]), hash(trunk[12]), hash(trunk[2])];
        assert_eq!(v.fork_in_global_index(&locator, &r), trunk[12], "first known and active");
        assert_eq!(v.fork_in_global_index(&locator[..2], &r), BlockId(0), "else genesis");
    }

    /// `LocatorEntries`: Core's heights for a tip at 300, and the null locator per profile.
    #[test]
    fn locator_matches_cores_locator_entries() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 300, 1_700_000_000);
        let reader = chain.reader();
        let heights = |hashes: Vec<BlockHash>| -> Vec<u32> {
            hashes
                .iter()
                .map(|h| reader.height(reader.lookup(h).expect("known")).unwrap())
                .collect()
        };
        // Ten single steps, then doubling from 289, then genesis (chain.cpp:32-49).
        let mut want: Vec<u32> = (290..=300).rev().collect();
        want.extend([289, 287, 283, 275, 259, 227, 163, 35, 0]);
        assert_eq!(heights(reader.locator(Some(trunk[299]))), want);
        assert_eq!(heights(reader.locator(Some(trunk[4]))), [5, 4, 3, 2, 1, 0]);
        assert_eq!(heights(reader.locator(Some(BlockId(0)))), [0]);
        // v24's `LocatorEntries(nullptr)` (DEC-82).
        assert_eq!(reader.locator(None), Vec::<BlockHash>::new());
    }

    /// `getchaintips`: the leaves and the active tip, Core's status order, height-descending
    /// order with ids for equal heights.
    #[test]
    fn chain_tips_follow_core() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 20, 1_700_000_000);
        let a = branch(&mut chain, trunk[9], 100, 3, 1_700_000_000); // tip at 13
        let b = branch(&mut chain, trunk[14], 200, 1, 1_700_000_000); // tip at 16
        let c = branch(&mut chain, trunk[4], 300, 8, 1_700_000_000); // tip at 13
        let d = branch(&mut chain, trunk[2], 400, 2, 1_700_000_000); // tip at 5
        let e = branch(&mut chain, trunk[2], 500, 2, 1_700_000_000); // tip at 5
        connect_all(&mut chain, &trunk[..17]);
        chain.modify(b[0], |g| g.status = Status(g.status.bits() | Status::FAILED_VALID));
        chain.modify(c[7], |g| g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK));
        chain.modify(d[1], |g| g.status = g.status.raise_validity(Status::VALID_SCRIPTS).0);
        let tips = chain.chain_tips().expect("tips");
        let got: Vec<_> =
            tips.iter().map(|t| (t.id, t.height, t.branchlen, t.fork_point, t.status)).collect();
        use ChainTipStatus::*;
        assert_eq!(
            got,
            [
                (trunk[19], 20, 3, trunk[16], HeadersOnly),
                (trunk[16], 17, 0, trunk[16], Active),
                (b[0], 16, 1, trunk[14], Invalid),
                (a[2], 13, 3, trunk[9], HeadersOnly),
                (c[7], 13, 8, trunk[4], Conflicting),
                (d[1], 5, 2, trunk[2], HeadersOnly),
                (e[1], 5, 2, trunk[2], HeadersOnly),
            ]
        );
        assert!(a[2] < c[7] && d[1] < e[1], "ids break height ties");
        assert_eq!((tips[0].hash, tips[0].chain_work), {
            let e = chain.reader().entry(trunk[19]).expect("set");
            (e.hash, e.chain_work)
        });
        // With the node's nChainTx hook: valid-fork at VALID_SCRIPTS, else valid-headers.
        let tips = chain.chain_tips_with(&|id| id == d[1] || id == e[1]).expect("tips");
        let status = |id| tips.iter().find(|t| t.id == id).map(|t| t.status);
        assert_eq!((status(d[1]), status(e[1])), (Some(ValidFork), Some(ValidHeaders)));
        assert_eq!((status(a[2]), status(b[0])), (Some(HeadersOnly), Some(Invalid)));
        // The tip is reported once when it is also a leaf.
        connect_all(&mut chain, &trunk[17..]);
        let tips = chain.chain_tips().expect("tips");
        assert_eq!(tips.iter().filter(|t| t.id == trunk[19]).count(), 1);
        assert_eq!(tips[0].status, Active);
    }

    /// The header subset of `CheckBlockIndex`: a consistent index passes; each status
    /// implication names the entry that breaks it.
    #[test]
    fn check_index_names_the_first_inconsistent_entry() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 10, 1_700_000_000);
        let side = branch(&mut chain, trunk[4], 100, 3, 1_700_000_000);
        connect_all(&mut chain, &trunk);
        assert_eq!(chain.check_index(), Ok(()));

        // FAILED_VALID on an entry, FAILED_CHILD below it, CONFLICT below a conflicting one.
        chain.modify(side[0], |g| g.status = Status(g.status.bits() | Status::FAILED_VALID));
        chain.modify(side[2], |g| g.status = Status(g.status.bits() | Status::FAILED_CHILD));
        chain.modify(trunk[6], |g| g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK));
        chain.modify(trunk[9], |g| g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK));
        assert_eq!(chain.check_index(), Ok(()));

        // Heights 1-3 are CHAIN valid, so a SCRIPTS-valid height 4 breaks only its own rule.
        let broken = |f: &dyn Fn(&mut HeaderChain) -> BlockId, what: &str| {
            let mut chain = node();
            let trunk = branch(&mut chain, BlockId(0), 1, 10, 1_700_000_000);
            connect_all(&mut chain, &trunk);
            for &id in &trunk[..3] {
                chain.modify(id, |g| g.status = g.status.raise_validity(Status::VALID_CHAIN).0);
            }
            assert_eq!(chain.check_index(), Ok(()));
            let id = f(&mut chain);
            let err = chain.check_index().expect_err(what);
            assert_eq!((err.id, err.what), (id, what));
        };
        let at = |chain: &HeaderChain, h: u32| chain.snapshot().active.at(h).expect("active");
        let set = |chain: &mut HeaderChain, h: u32, bits: u32| {
            let id = at(chain, h);
            chain.modify(id, |g| g.status = Status(g.status.bits() | bits));
            id
        };
        let level = |chain: &mut HeaderChain, h: u32, validity: u32| {
            let id = at(chain, h);
            chain.modify(id, |g| {
                g.status = Status(g.status.bits() & !Status::VALID_MASK | validity)
            });
            id
        };
        broken(&|c| set(c, 5, Status::FAILED_CHILD), "failed mask without an invalid parent");
        broken(&|c| set(c, 7, Status::HAVE_UNDO), "HAVE_UNDO without HAVE_DATA");
        broken(
            &|c| level(c, 4, Status::VALID_SCRIPTS),
            "SCRIPTS valid implies all parents are SCRIPTS valid",
        );
        broken(
            &|c| level(c, 5, Status::VALID_CHAIN),
            "CHAIN valid implies all parents are CHAIN valid",
        );
        broken(&|c| level(c, 6, Status::VALID_RESERVED), "every entry must be TREE valid");
    }

    /// Records as `load` reads them: (key, value).
    type Records = Vec<(BlockHash, Vec<u8>)>;

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new().num_threads(3).build().expect("a test pool")
    }

    /// Every entry's record, as a fresh node writes them all (each insert is dirty), in the
    /// key order Core's LevelDB cursor yields (`DB_BLOCK_INDEX` + hash).
    fn records(chain: &mut HeaderChain) -> Records {
        let mut recs: Vec<_> = chain.take_dirty().into_iter().map(|w| (w.hash, w.bytes)).collect();
        recs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        recs
    }

    fn load(recs: Records) -> Result<HeaderChain, LoadError> {
        let p = params(NetworkKind::Regtest);
        let (cfg, svc) = (ChainConfig::node(&p), services(&p));
        HeaderChain::load(p, cfg, svc, recs.into_iter().map(Ok), &pool())
    }

    fn load_records_with(
        network: NetworkKind,
        recs: impl Iterator<Item = Result<(BlockHash, Vec<u8>), LoadError>>,
        interrupted: impl Fn() -> bool,
    ) -> Result<HeaderChain, LoadError> {
        let p = params(network);
        let (cfg, svc) = (ChainConfig::node(&p), services(&p));
        HeaderChain::load_records(p, cfg, svc, recs, &pool(), interrupted)
    }

    /// Core's `CheckBlockIndex` after V's `LoadChainTip` at genesis (CH1-A11).
    fn check_at_genesis(chain: &mut HeaderChain) -> Result<(), IndexInconsistency> {
        chain.init_active_tip(chain.genesis()).expect("the genesis entry is set");
        chain.check_index()
    }

    #[test]
    fn take_dirty_into_hands_take_dirty_s_records_in_order() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 3, 1_700_000_000);
        let drain = |chain: &mut HeaderChain| {
            let mut got = Vec::new();
            chain.take_dirty_into(|hash, bytes| {
                got.push(IndexWrite {
                    hash: *hash,
                    bytes: bytes.to_vec(),
                })
            });
            got
        };
        // Records of other lengths share the buffer: block data adds the position fields.
        chain.modify(trunk[1], |g| {
            g.status = Status(Status::VALID_SCRIPTS | Status::HAVE_DATA | Status::HAVE_UNDO);
            (g.n_tx, g.file, g.data_pos, g.undo_pos) = (300, Some(70_000), 1 << 30, 81);
        });
        let want = [BlockId(0), trunk[0], trunk[1], trunk[2]].map(|id| chain.record(id));
        assert!(
            want[2].bytes.len() > want[1].bytes.len() && want[2].bytes.len() > want[3].bytes.len()
        );
        assert_eq!(drain(&mut chain), want);
        assert_eq!(drain(&mut chain), []);

        chain.mark_dirty(trunk[2]);
        chain.mark_dirty(trunk[0]);
        let want = [chain.record(trunk[2]), chain.record(trunk[0])];
        assert_eq!(drain(&mut chain), want);
    }

    #[test]
    fn take_dirty_drains_in_marking_order_and_round_trips() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 3, 1_700_000_000);
        let first = chain.take_dirty();
        let hashes: Vec<BlockHash> = first.iter().map(|w| w.hash).collect();
        let reader = chain.reader();
        let hash = |id| reader.entry(id).expect("set").hash;
        assert_eq!(hashes, [hash(BlockId(0)), hash(trunk[0]), hash(trunk[1]), hash(trunk[2])]);
        assert!(chain.take_dirty().is_empty(), "drained");

        chain.modify(trunk[1], |g| {
            g.status = Status(Status::VALID_SCRIPTS | Status::HAVE_DATA | Status::HAVE_UNDO);
            (g.n_tx, g.file, g.data_pos, g.undo_pos) = (2, Some(3), 81, 17);
        });
        chain.mark_dirty(trunk[1]);
        chain.mark_dirty(BlockId(0));
        chain.mark_dirty(trunk[1]);
        let w = chain.take_dirty();
        assert_eq!(
            w.iter().map(|w| w.hash).collect::<Vec<_>>(),
            [hash(trunk[1]), hash(BlockId(0))]
        );
        let rec = BlockIndexRecord::decode(&w[0].bytes).expect("decodes");
        let e = reader.entry(trunk[1]).expect("set");
        assert_eq!(
            rec,
            BlockIndexRecord {
                height: 2,
                status: e.status,
                n_tx: 2,
                file: Some(3),
                data_pos: Some(81),
                undo_pos: Some(17),
                header: e.header,
            }
        );
        let genesis = BlockIndexRecord::decode(&w[1].bytes).expect("decodes");
        assert_eq!((genesis.file, genesis.data_pos, genesis.undo_pos), (None, None, None));
    }

    /// `load` rebuilds what the inserts built: the same fields, links, leaves, best header and a
    /// consistent index, with ids on first mention (hash, then prev) in record order.
    #[test]
    fn load_round_trips_the_index() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 40, 1_700_000_000);
        let a = branch(&mut chain, trunk[19], 100, 25, 1_700_000_500);
        let b = branch(&mut chain, trunk[30], 200, 4, 1_699_000_000);
        chain.modify(b[0], |g| g.status = Status(g.status.bits() | Status::HAVE_DATA));
        chain.modify(b[0], |g| (g.n_tx, g.file, g.data_pos) = (1, Some(0), 8));
        chain.mark_dirty(b[0]);
        let recs = records(&mut chain);
        let mut loaded = load(recs.clone()).expect("loads");
        assert_eq!(check_at_genesis(&mut loaded), Ok(()));

        // Ids: the first record's hash, then its prev, and so on.
        let (r, l) = (chain.reader(), loaded.reader());
        let first = BlockIndexRecord::decode(&recs[0].1).expect("decodes");
        assert_eq!(l.lookup(&recs[0].0), Some(BlockId(0)));
        assert_eq!(l.lookup(&first.header.prev_hash), Some(BlockId(1)));
        assert_eq!(loaded.next_id, chain.next_id);

        let same = |id: BlockId| {
            let e = r.entry(id).expect("original");
            let f = l.entry(l.lookup(&e.hash).expect("loaded")).expect("loaded");
            let prev_hash =
                |v: &ChainReader, p: Option<BlockId>| p.map(|p| v.entry(p).expect("p").hash);
            assert_eq!(prev_hash(&r, e.prev), prev_hash(&l, f.prev));
            let fields = |e: &EntryView| {
                (
                    e.header,
                    e.height,
                    e.chain_work,
                    e.mtp,
                    e.time_max,
                    e.status,
                    e.n_tx,
                    e.file,
                    e.data_pos,
                )
            };
            assert_eq!(fields(&e), fields(&f), "entry {}", e.height);
            f.id
        };
        for id in (0..chain.next_id).map(BlockId) {
            same(id);
        }
        assert_eq!(same(chain.best_header()), loaded.best_header());
        let leaves = |c: &HeaderChain| {
            let mut h: Vec<_> = c
                .leaves
                .iter()
                .map(|&(_, Reverse(id))| c.reader().entry(id).unwrap().hash)
                .collect();
            h.sort_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
            h
        };
        assert_eq!(leaves(&chain), leaves(&loaded));
        let children = |c: &HeaderChain, v: &ChainReader, id| -> Vec<BlockHash> {
            let mut h: Vec<_> =
                c.child_ids(id).expect("node").iter().map(|&k| v.entry(k).unwrap().hash).collect();
            h.sort_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
            h
        };
        let fork = |v: &ChainReader| v.lookup(&r.entry(trunk[19]).unwrap().hash).unwrap();
        assert_eq!(children(&chain, &r, fork(&r)), children(&loaded, &l, fork(&l)));
        assert_eq!(children(&loaded, &l, fork(&l)).len(), 2);
        // The genesis entry is the active tip until LoadChainTip; nothing is dirty.
        let snap = loaded.snapshot();
        assert_eq!((snap.tip.tip_height, snap.tip_seq, snap.tip.best_header_height), (0, 0, 45));
        assert!(loaded.dirty.ids.is_empty());
        let _ = a;
    }

    /// A-17 (node half; SB4 C6 adds the lock that makes the mark): a CONFLICT mark that was
    /// never dirtied does not reach the records (D15), and the reload recomputes `best_header`.
    #[test]
    fn a17_conflict_mark_is_not_persisted_and_best_header_is_recomputed() {
        let mut chain = node();
        let a = branch(&mut chain, BlockId(0), 1, 100, 1_700_000_000);
        let b = branch(&mut chain, a[94], 1000, 15, 1_700_000_200);
        connect_all(&mut chain, &a[..95]);
        // CH2's flush writes every insert; the mark comes after it and is not dirtied.
        let flushed = records(&mut chain);
        chain.modify(b[0], |g| g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK));
        assert_eq!(chain.reader().status(b[0]), Some(Status(130)));
        assert_eq!(chain.best_header(), b[14]);
        assert!(chain.take_dirty().is_empty(), "D15: Core does not dirty the mark");
        let loaded = load(flushed).expect("loads");
        let l = loaded.reader();
        let b96 = l.lookup(&chain.reader().entry(b[0]).unwrap().hash).expect("B96");
        assert_eq!(l.status(b96), Some(Status(Status::VALID_TREE)), "the mark was never dirty");
        assert_eq!(
            l.entry(loaded.best_header()).unwrap().hash,
            chain.reader().entry(b[14]).unwrap().hash
        );
    }

    /// `:321-324`: a child of a failed entry loads with `FAILED_CHILD`, marked dirty, in height
    /// order.
    #[test]
    fn load_propagates_failed_child_and_marks_it_dirty() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 6, 1_700_000_000);
        let side = branch(&mut chain, trunk[3], 100, 1, 1_700_000_000);
        chain.modify(trunk[2], |g| g.status = Status(g.status.bits() | Status::FAILED_VALID));
        chain.mark_dirty(trunk[2]);
        let mut loaded = load(records(&mut chain)).expect("loads");
        let l = loaded.reader();
        let id = |k: BlockId| l.lookup(&chain.reader().entry(k).unwrap().hash).unwrap();
        assert_eq!(l.status(id(trunk[2])), Some(Status(Status::VALID_TREE | Status::FAILED_VALID)));
        for k in [trunk[3], trunk[4], trunk[5], side[0]] {
            assert_eq!(l.status(id(k)), Some(Status(Status::VALID_TREE | Status::FAILED_CHILD)));
        }
        assert_eq!(l.status(id(trunk[1])), Some(Status(Status::VALID_TREE)));
        let dirty: Vec<u32> = loaded
            .take_dirty()
            .iter()
            .map(|w| l.height(l.lookup(&w.hash).unwrap()).unwrap())
            .collect();
        assert_eq!(dirty, [4, 5, 5, 6]);
        // best_header skips failed entries.
        assert_eq!(loaded.best_header(), id(trunk[1]));
        assert_eq!(check_at_genesis(&mut loaded), Ok(()));
    }

    /// Core's `m_best_header` after a reload: the most work, and on a tie the lowest id, which
    /// is the first mention in record order (D9), not the first-seen header before the reload.
    #[test]
    fn load_breaks_a_best_header_tie_by_load_id() {
        let mut chain = node();
        let a = branch(&mut chain, BlockId(0), 1, 3, 1_700_000_000);
        let b = branch(&mut chain, BlockId(0), 100, 3, 1_700_000_000);
        assert_eq!(chain.work(a[2]), chain.work(b[2]));
        let reader = chain.reader();
        let hash = |id| reader.entry(id).expect("set").hash;
        let recs = records(&mut chain);
        let first = |tip: BlockId| {
            let mut r = recs.clone();
            let i = r.iter().position(|(h, _)| *h == hash(tip)).expect("a record");
            r.swap(0, i);
            let loaded = load(r).expect("loads");
            assert_eq!(loaded.reader().lookup(&hash(tip)), Some(BlockId(0)));
            loaded.reader().entry(loaded.best_header()).expect("set").hash
        };
        assert_eq!(first(b[2]), hash(b[2]));
        assert_eq!(first(a[2]), hash(a[2]));
    }

    /// Core's `LoadBlockIndexGuts` has no network check (`txdb.cpp:300-344`): the staged load
    /// takes a mainnet genesis on testnet, and the node checks its block files and then the
    /// network (`node/chainstate.cpp:188-193`).
    #[test]
    fn load_records_takes_another_networks_genesis_and_load_refuses_it() {
        let main = params(NetworkKind::Mainnet);
        let mut chain = HeaderChain::<NoStore>::new_genesis(
            main.clone(),
            ChainConfig::node(&main),
            services(&main),
            NoStore,
        )
        .expect("a mainnet genesis");
        let recs = records(&mut chain);
        let test = params(NetworkKind::Testnet);
        assert_ne!(test.consensus.hash_genesis_block, main.consensus.hash_genesis_block);

        let loaded =
            load_records_with(NetworkKind::Testnet, recs.clone().into_iter().map(Ok), || false)
                .expect("structurally sound");
        let reader = loaded.reader();
        let genesis = reader.entry(loaded.genesis()).expect("set").hash;
        assert_eq!(genesis.as_bytes(), main.consensus.hash_genesis_block.as_bytes());
        assert_eq!(reader.lookup(&genesis), Some(loaded.genesis()));

        let err = HeaderChain::<NoStore>::load(
            test.clone(),
            ChainConfig::node(&test),
            services(&test),
            recs.into_iter().map(Ok),
            &pool(),
        );
        assert_eq!(err.err(), Some(LoadError::GenesisMismatch));
    }

    /// SB4's refusal of a second root (D21) holds without the network rule (CH1-node D54, Opus
    /// r1 m-3): testnet's genesis and mainnet's, both height 0 and both within testnet's proof
    /// of work, are two roots.
    #[test]
    fn load_records_refuses_a_second_genesis_root() {
        let genesis_records = |network| {
            let p = params(network);
            let mut chain = HeaderChain::<NoStore>::new_genesis(
                p.clone(),
                ChainConfig::node(&p),
                services(&p),
                NoStore,
            )
            .expect("a genesis");
            records(&mut chain)
        };
        let mut recs = genesis_records(NetworkKind::Testnet);
        recs.extend(genesis_records(NetworkKind::Mainnet));
        recs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        assert_eq!(recs.len(), 2);
        let err = load_records_with(NetworkKind::Testnet, recs.into_iter().map(Ok), || false);
        assert_eq!(err.err(), Some(LoadError::GenesisMismatch));
    }

    /// `ShutdownRequested()` before the scan and before each entry (`txdb.cpp:307-309`): the
    /// staged load stops ahead of the next read, so a read error there is never reported.
    #[test]
    fn load_records_polls_for_shutdown_before_each_read() {
        let mut chain = node();
        branch(&mut chain, BlockId(0), 1, 3, 1_700_000_000);
        let recs = records(&mut chain);
        let stop = std::cell::Cell::new(false);
        let reads = std::cell::Cell::new(0);
        let mut it = recs.clone().into_iter();
        let controlled = std::iter::from_fn(|| {
            reads.set(reads.get() + 1);
            match reads.get() {
                1 => {
                    stop.set(true);
                    it.next().map(Ok)
                }
                _ => Some(Err(LoadError::Read(crate::error::StoreError::Io("reached".into())))),
            }
        });
        let err = load_records_with(NetworkKind::Regtest, controlled, || stop.get());
        assert_eq!(err.err(), Some(LoadError::Interrupted));
        assert_eq!(reads.get(), 1, "the read after the shutdown request");

        let read = std::cell::Cell::new(false);
        let untouched = std::iter::from_fn(|| {
            read.set(true);
            None
        });
        let err = load_records_with(NetworkKind::Regtest, untouched, || true);
        assert_eq!(err.err(), Some(LoadError::Interrupted));
        assert!(!read.get(), "the poll before the scan");

        let all =
            load_records_with(NetworkKind::Regtest, recs.clone().into_iter().map(Ok), || false)
                .expect("loads");
        assert!(recs.iter().all(|(k, _)| all.reader().lookup(k).is_some()));
    }

    #[test]
    fn load_refuses_an_inconsistent_index() {
        let mut chain = node();
        branch(&mut chain, BlockId(0), 1, 12, 1_700_000_000);
        let recs = records(&mut chain);
        let reader = chain.reader();
        let height_of = |h: &BlockHash| reader.height(reader.lookup(h).unwrap()).unwrap();
        let with = |f: &dyn Fn(&mut Records)| {
            let mut r = recs.clone();
            f(&mut r);
            load(r).err()
        };
        let at = |h: u32| recs.iter().position(|(k, _)| height_of(k) == h).unwrap();
        let rewrite = |r: &mut Records, h: u32, f: &dyn Fn(&mut BlockIndexRecord)| {
            let i = at(h);
            let mut rec = BlockIndexRecord::decode(&r[i].1).unwrap();
            f(&mut rec);
            r[i].1 = rec.to_bytes();
        };

        assert_eq!(load(Vec::new()).err(), Some(LoadError::Empty));
        let null = BlockHash::from([0; 32]);
        assert_eq!(with(&|r| r[at(5)].0 = null), Some(LoadError::NullHash));
        let h5 = recs[at(5)].0;
        assert_eq!(
            with(&|r| r[at(5)].1.truncate(10)),
            Some(LoadError::Decode {
                hash: h5,
                err: crate::error::CodecError::Truncated
            })
        );
        let mut high = *h5.as_bytes();
        high[31] = 0xff;
        let high = BlockHash::from(high);
        assert_eq!(
            with(&|r| r[at(5)].0 = high),
            Some(LoadError::HighHash {
                hash: high
            })
        );
        let missing = with(&|r| {
            r.remove(at(5));
        });
        assert_eq!(
            missing,
            Some(LoadError::MissingParent {
                hash: recs[at(6)].0,
                prev: h5
            })
        );
        assert_eq!(
            with(&|r| r.retain(|(k, _)| height_of(k) != 0)),
            Some(LoadError::MissingParent {
                hash: recs[at(1)].0,
                prev: recs[at(0)].0,
            })
        );
        assert_eq!(
            with(&|r| rewrite(r, 7, &|rec| rec.header.prev_hash = BlockHash::from([0; 32]))),
            Some(LoadError::GenesisMismatch),
            "a second root"
        );
        assert_eq!(
            with(&|r| {
                let h1 = recs[at(1)].0;
                rewrite(r, 0, &|rec| rec.header.prev_hash = h1);
                r.retain(|(k, _)| height_of(k) <= 1);
            }),
            Some(LoadError::GenesisMismatch),
            "no root: genesis and height 1 name each other"
        );
        assert_eq!(
            with(&|r| rewrite(r, 12, &|rec| rec.height = 40)),
            Some(LoadError::NonContiguous {
                missing_height: 12
            })
        );
        assert_eq!(
            with(&|r| rewrite(r, 12, &|rec| rec.height = 11)),
            Some(LoadError::HeightMismatch {
                hash: recs[at(12)].0
            })
        );
        // Core refuses the gap first, before any parent is read (:297-300).
        assert_eq!(
            with(&|r| rewrite(r, 6, &|rec| rec.height = 4)),
            Some(LoadError::NonContiguous {
                missing_height: 6
            }),
            "a gap and a misplaced height"
        );
        assert_eq!(
            with(&|r| {
                rewrite(r, 6, &|rec| rec.height = 7);
                rewrite(r, 7, &|rec| rec.height = 6);
            }),
            Some(LoadError::HeightMismatch {
                hash: recs[at(7)].0
            }),
            "a parent not yet set"
        );
        // A read error is returned in record order.
        let p = params(NetworkKind::Regtest);
        let read = recs.iter().take(3).cloned().map(Ok).chain(std::iter::once(Err(
            LoadError::Read(crate::error::StoreError::Io("disk".into())),
        )));
        let err = HeaderChain::<NoStore>::load(
            p.clone(),
            ChainConfig::node(&p),
            services(&p),
            read,
            &pool(),
        );
        assert_eq!(err.err(), Some(LoadError::Read(crate::error::StoreError::Io("disk".into()))));
        let err = HeaderChain::<NoStore>::load(
            p.clone(),
            ChainConfig::spv(&p, 10_000),
            services(&p),
            recs.into_iter().map(Ok),
            &pool(),
        );
        assert_eq!(err.err(), Some(LoadError::Config(ConfigError::WrongMode("load"))));
    }

    /// More records than one parallel batch.
    #[test]
    fn load_spans_decode_batches() {
        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, LOAD_BATCH as u64 + 4_000, 1_700_000_000);
        let mut loaded = load(records(&mut chain)).expect("loads");
        let tip = *trunk.last().unwrap();
        let l = loaded.reader();
        let id = l.lookup(&chain.reader().entry(tip).unwrap().hash).unwrap();
        assert_eq!(loaded.best_header(), id);
        assert_eq!(l.entry(id).unwrap().chain_work, chain.reader().entry(tip).unwrap().chain_work);
        assert_eq!(check_at_genesis(&mut loaded), Ok(()));
    }
}
