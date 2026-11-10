//! `resolve_list`: the list after a block, from the stored records (EV2 §2.10; Core's
//! `GetListForBlockInternal`), and the list cache it reads through.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::{BlockId, ChainAncestry};
use dash_params::BuriedDeployment;
use dash_primitives::BlockHash;

use super::{DmlReader, StoreError};
use crate::MnList;

/// Why a list is offered to the cache. The policy is not observable (O17 EVO-25): it only
/// decides how much work a later [`resolve_list`] repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheHint {
    /// The list after the tip.
    Tip,
    /// A quorum base's list that a live quorum still needs.
    QuorumBase,
    /// A mini-snapshot: every 32 heights of a replay (Core's `MINI_SNAPSHOT_INTERVAL`).
    Mini,
    /// A list `resolve_list` built or read.
    Built,
}

/// Lists by block hash that a [`resolve_list`] may start from instead of a stored snapshot, and
/// the list manager's other in-memory state: the initial-snapshot blocks.
pub trait ListCache: Sync {
    /// The cached list after `block`.
    fn get(&self, block: &BlockHash) -> Option<Arc<MnList>>;
    /// Offers `list` (the list after `list.block_hash()`).
    fn offer(&self, list: &Arc<MnList>, hint: CacheHint);
    /// Whether a [`resolve_list`] in this process took `block` as the initial snapshot, among the
    /// most recent [`INITIAL_SNAPSHOTS_KEPT`]. Core keeps one such block,
    /// `m_initial_snapshot_index` (`deterministicmns.h:780`), the last that fell back; this keeps
    /// the most recent ones, in memory only, so a restart starts empty and the next walk that
    /// reaches a block adds it again. Core's index, when it is a DIP3-active block, is always the
    /// most recent, so the set holds it. A block below DIP3 that ends a walk joins no set here,
    /// though Core's index moves to it (see [`resolve_list_traced`]).
    /// Connecting a child of any of them writes a snapshot row
    /// ([`process_block`](crate::process_block)'s `extra_snapshot`;
    /// `deterministicmns.cpp:722`), so the rows are a superset of Core's (DEC-1064, OD-CP-46,
    /// OD-CP-51).
    fn is_initial_snapshot(&self, block: &BlockHash) -> bool;
    /// Adds `block` as the most recent initial-snapshot block (again, if it is one already); past
    /// [`INITIAL_SNAPSHOTS_KEPT`] the least recently added goes.
    fn add_initial_snapshot(&self, block: BlockHash);
}

/// How many initial-snapshot blocks a [`ListCache`] keeps: Core's `MAX_CACHED_LISTS`
/// (`deterministicmns.h:769`), as many as the lists it caches (OD-CP-51).
pub const INITIAL_SNAPSHOTS_KEPT: usize = 64;

/// No cache: every [`resolve_list`] walks to a stored snapshot. It keeps no initial-snapshot
/// blocks either, so a node on it would never write their children's snapshot rows; tests only.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoListCache;

impl ListCache for NoListCache {
    fn get(&self, _block: &BlockHash) -> Option<Arc<MnList>> {
        None
    }
    fn offer(&self, _list: &Arc<MnList>, _hint: CacheHint) {}
    fn is_initial_snapshot(&self, _block: &BlockHash) -> bool {
        false
    }
    fn add_initial_snapshot(&self, _block: BlockHash) {}
}

/// The single-threaded reference cache, in v24's shape (`CleanupCache`,
/// `deterministicmns.cpp` v24:975-1014): the tip's list is pinned, quorum bases are kept while
/// alive, and above [`LruListCache::MAX_CACHED_LISTS`] the least recently used other lists go.
/// It also holds the most recent initial-snapshot blocks, which list eviction never clears. CH7a
/// owns the concurrent implementation and its budget.
#[derive(Debug, Default)]
pub struct LruListCache {
    inner: Mutex<Lru>,
}

#[derive(Debug, Default)]
struct Lru {
    lists: BTreeMap<BlockHash, Cached>,
    tip: Option<BlockHash>,
    clock: u64,
    /// Oldest first, at most [`INITIAL_SNAPSHOTS_KEPT`], each once.
    initial_snapshots: VecDeque<BlockHash>,
}

impl Lru {
    /// Advances the use clock and returns the new reading.
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
}

#[derive(Debug)]
struct Cached {
    list: Arc<MnList>,
    hint: CacheHint,
    used: u64,
}

impl LruListCache {
    /// Core's `MAX_CACHED_LISTS` trigger for a cleanup (`deterministicmns.h` v24:766).
    pub const MAX_CACHED_LISTS: usize = 64;

    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pins the list after `tip` (`UpdatedBlockTip`); the previous tip's list becomes ordinary.
    pub fn set_tip(&self, tip: Option<BlockHash>) {
        self.lock().tip = tip;
    }

    /// The number of cached lists.
    pub fn len(&self) -> usize {
        self.lock().lists.len()
    }

    /// Whether the cache holds no list.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Lru> {
        // The state stays consistent at every step, so a panic elsewhere leaves it usable.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ListCache for LruListCache {
    fn get(&self, block: &BlockHash) -> Option<Arc<MnList>> {
        let mut lru = self.lock();
        let now = lru.tick();
        lru.lists.get_mut(block).map(|cached| {
            cached.used = now;
            Arc::clone(&cached.list)
        })
    }

    fn offer(&self, list: &Arc<MnList>, hint: CacheHint) {
        let mut lru = self.lock();
        let used = lru.tick();
        // A quorum base stays one when its list is offered again for another reason.
        let hint = match lru.lists.get(list.block_hash()) {
            Some(cached) if cached.hint == CacheHint::QuorumBase => CacheHint::QuorumBase,
            _ => hint,
        };
        lru.lists.insert(
            *list.block_hash(),
            Cached {
                list: Arc::clone(list),
                hint,
                used,
            },
        );
        while lru.lists.len() > Self::MAX_CACHED_LISTS {
            let tip = lru.tip;
            // Evict the least recently used list that is neither the tip's nor a quorum base's,
            // then, if only those remain, the least recently used quorum base.
            let victim = lru
                .lists
                .iter()
                .filter(|(hash, _)| Some(**hash) != tip)
                .min_by_key(|(_, c)| (c.hint == CacheHint::QuorumBase, c.used))
                .map(|(hash, _)| *hash);
            let Some(hash) = victim else {
                break;
            };
            lru.lists.remove(&hash);
        }
    }

    fn is_initial_snapshot(&self, block: &BlockHash) -> bool {
        self.lock().initial_snapshots.contains(block)
    }

    fn add_initial_snapshot(&self, block: BlockHash) {
        let mut lru = self.lock();
        lru.initial_snapshots.retain(|b| *b != block);
        lru.initial_snapshots.push_back(block);
        while lru.initial_snapshots.len() > INITIAL_SNAPSHOTS_KEPT {
            lru.initial_snapshots.pop_front();
        }
    }
}

/// Core's `MINI_SNAPSHOT_INTERVAL`.
const MINI_SNAPSHOT_INTERVAL: i32 = 32;

/// A [`resolve_list_traced`] answer: the list, and whether its walk stood on Core's initial
/// snapshot.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The list after the block.
    pub list: Arc<MnList>,
    /// Whether the walk took the initial snapshot (the list is built over a made-up empty list).
    /// A result computed from such a list must not enter a cache that another view reads
    /// (OD-CP-51, DEC-1073).
    pub over_initial_snapshot: bool,
}

/// `GetListForBlockInternal(at)` (`deterministicmns.cpp` v24:826-939): the list after block
/// `at`; [`resolve_list_traced`] without the trace.
// Core: evo/deterministicmns.cpp:826 CDeterministicMNManager::GetListForBlockInternal
pub fn resolve_list(
    at: BlockId,
    chain: &dyn ChainAncestry,
    deployments: &dyn DeploymentView,
    store: &dyn DmlReader,
    cache: &dyn ListCache,
) -> Result<Arc<MnList>, StoreError> {
    resolve_list_traced(at, chain, deployments, store, cache).map(|r| r.list)
}

/// `GetListForBlockInternal(at)` (`deterministicmns.cpp` v24:826-939): the list after block
/// `at`, and whether it stands on the initial snapshot.
///
/// Below DIP3 it is the default list. Otherwise the walk goes back through `prev` to a cached
/// list, a stored snapshot, or the first block with neither a snapshot nor a diff, collecting the
/// diff records, then applies them forward with [`MnList::apply_record`], so the counter and the
/// collision order are the online list's. Every 32 heights the replay offers a mini-snapshot; the
/// result is offered as [`CacheHint::Built`].
///
/// The walk reads every row it meets whatever the block's DIP3 state, as Core's does (`:850-864`):
/// rows below DIP3, which a regtest run with a lower DIP3 height wrote, are applied like any other.
/// A block below DIP3 with neither a snapshot nor a diff ends the walk with the default list:
/// Core's initial snapshot there (`:882-888`) has the same content once the first diff sets the
/// position. That block joins no initial-snapshot set, where Core's index moves to it (and leaves
/// an earlier index, whose child's snapshot row the node keeps): Core's only use of the index is
/// the snapshot rule for the block's child (`:722`), and that child is either the first DIP3 block,
/// which the node always snapshots (DEC-11), or below DIP3, where `ProcessBlock` returns before the
/// rule (`:696-698`).
///
/// A DIP3-active block with neither a snapshot nor a diff takes Core's split (DEC-1064):
/// - with the dual-chainstate marker ([`DmlReader::has_dual_chainstate_marker`]) it is
///   [`StoreError::ListUnavailable`], Core's `BlockDataUnavailableError`: fail-stop (A1 I5);
/// - otherwise it is the initial snapshot: the empty list at that block's hash and height with
///   counter 0 (`CDeterministicMNList(hash, height, 0)`), and the block joins the cache's
///   initial-snapshot blocks ([`ListCache::is_initial_snapshot`]). Core also logs "initial
///   snapshot. blockHash=… nHeight=…"; this crate has no logger, so the line is not ported.
///
/// A row that does not decode is not "neither": the reader's [`StoreError`] stops the walk, a
/// fail-stop, with or without the marker. Core's `CDBWrapper::Read` returns false for bytes that
/// fail to deserialize (`dbwrapper.h:322-326`), so Core would take a corrupt row as missing and
/// go on over an empty list; the node stops instead of turning local corruption into a later
/// consensus-looking reject (a declared deviation, DEC-1073, after D7/DEC-78 and DEC-180).
///
/// A walk that takes the initial snapshot offers nothing to the cache (OD-CP-46): no mini-snapshot
/// and no result, as if Core's `CleanupCache` evicted them at once (Core's own entry is
/// overwritten at `deterministicmns.cpp:729`/`:753`, erased by `UndoBlock` at `:800`, evicted at
/// `:836-837`/`:975-1006`, and dropped by a restart). Its answer reports it
/// ([`Resolved::over_initial_snapshot`]), so a caller keeps whatever it computes from the list out
/// of its shared caches too (OD-CP-51). A walk that reaches the block again takes the initial
/// snapshot again, as Core's does after an eviction.
///
/// The cache is read before the store, as Core's `mnListsCache` is (`:843-848`). So a hit and a
/// miss give the same answer whenever the reading view holds the rows the cached list was built
/// from; they differ only where the view lacks a DIP3 block's rows: a hit returns the real list a
/// view with those rows built, a miss the initial snapshot, exactly as in Core (DEC-1073).
///
/// A record at the wrong height, or one that does not apply, is [`StoreError::Inconsistent`]:
/// fail-stop (A1 I5).
// Core: evo/deterministicmns.cpp:826 CDeterministicMNManager::GetListForBlockInternal
pub fn resolve_list_traced(
    at: BlockId,
    chain: &dyn ChainAncestry,
    deployments: &dyn DeploymentView,
    store: &dyn DmlReader,
    cache: &dyn ListCache,
) -> Result<Resolved, StoreError> {
    let dip3 = Deployment::Buried(BuriedDeployment::Dip0003);
    let resolved = |list, over_initial_snapshot| Resolved {
        list,
        over_initial_snapshot,
    };
    if !deployments.active_at(at, dip3) {
        return Ok(resolved(Arc::new(MnList::default_pre_dip3()), false));
    }
    let mut records = Vec::new();
    let mut cursor = at;
    let mut made_up = false;
    let base = loop {
        let header = chain.header(cursor);
        if let Some(list) = cache.get(&header.hash) {
            break list;
        }
        if let Some(list) = store.mn_list_snapshot(&header.hash)? {
            cache.offer(&list, CacheHint::Built);
            break list;
        }
        let Some(record) = store.mn_list_diff(&header.hash)? else {
            // Core: evo/deterministicmns.cpp:863-889
            if !deployments.active_at(cursor, dip3) {
                break Arc::new(MnList::default_pre_dip3());
            }
            if store.has_dual_chainstate_marker()? {
                return Err(StoreError::ListUnavailable(header.hash));
            }
            let height = i32::try_from(header.height)
                .map_err(|_| StoreError::inconsistent("a block height above i32::MAX"))?;
            cache.add_initial_snapshot(header.hash);
            made_up = true;
            break Arc::new(MnList::new(header.hash, height, 0));
        };
        if i64::from(record.height) != i64::from(header.height) {
            return Err(StoreError::inconsistent(format!(
                "mn_diff row for block {} has height {}, the block {}",
                header.hash, record.height, header.height
            )));
        }
        records.push((header.hash, record));
        cursor = chain
            .prev(cursor)
            .ok_or_else(|| StoreError::inconsistent("the mn_diff walk passed genesis"))?;
    };
    if records.is_empty() {
        return Ok(resolved(base, made_up));
    }
    let at_hash = chain.header(at).hash;
    let mut list = (*base).clone();
    for (hash, record) in records.iter().rev() {
        list.apply_record(*hash, record)
            .map_err(|e| StoreError::inconsistent(format!("mn_diff row for block {hash}: {e}")))?;
        if !made_up && list.height() % MINI_SNAPSHOT_INTERVAL == 0 && *hash != at_hash {
            cache.offer(&Arc::new(list.clone()), CacheHint::Mini);
        }
    }
    let list = Arc::new(list);
    if !made_up {
        cache.offer(&list, CacheHint::Built);
    }
    Ok(resolved(list, made_up))
}
