//! `resolve_list`: the list after a block, from the stored records (EV2 §2.10; Core's
//! `GetListForBlockInternal`), and the list cache it reads through.

use std::collections::BTreeMap;
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

/// Lists by block hash that a [`resolve_list`] may start from instead of a stored snapshot.
pub trait ListCache: Sync {
    /// The cached list after `block`.
    fn get(&self, block: &BlockHash) -> Option<Arc<MnList>>;
    /// Offers `list` (the list after `list.block_hash()`).
    fn offer(&self, list: &Arc<MnList>, hint: CacheHint);
}

/// No cache: every [`resolve_list`] walks to a stored snapshot.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoListCache;

impl ListCache for NoListCache {
    fn get(&self, _block: &BlockHash) -> Option<Arc<MnList>> {
        None
    }
    fn offer(&self, _list: &Arc<MnList>, _hint: CacheHint) {}
}

/// The single-threaded reference cache, in v24's shape (`CleanupCache`,
/// `deterministicmns.cpp` v24:975-1014): the tip's list is pinned, quorum bases are kept while
/// alive, and above [`LruListCache::MAX_CACHED_LISTS`] the least recently used other lists go.
/// CH7a owns the concurrent implementation and its budget.
#[derive(Debug, Default)]
pub struct LruListCache {
    inner: Mutex<Lru>,
}

#[derive(Debug, Default)]
struct Lru {
    lists: BTreeMap<BlockHash, Cached>,
    tip: Option<BlockHash>,
    clock: u64,
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
}

/// Core's `MINI_SNAPSHOT_INTERVAL`.
const MINI_SNAPSHOT_INTERVAL: i32 = 32;

/// `GetListForBlockInternal(at)` (`deterministicmns.cpp` v24:826-939): the list after block
/// `at`.
///
/// Below DIP3 it is the default list. Otherwise the walk goes back through `prev` to a cached
/// list, a stored snapshot, or the last block before DIP3 (whose list is the default one: Core's
/// "initial snapshot" there has the same content once the first diff sets the position), collecting
/// the diff records, then applies them forward with [`MnList::apply_record`], so the counter and
/// the collision order are the online list's. Every 32 heights the replay offers a
/// mini-snapshot; the result is offered as [`CacheHint::Built`].
///
/// A DIP3-active block with neither a snapshot nor a diff, a record at the wrong height, or a
/// record that does not apply is [`StoreError::Inconsistent`]: fail-stop (A1 I5).
// Core: evo/deterministicmns.cpp:826 CDeterministicMNManager::GetListForBlockInternal
pub fn resolve_list(
    at: BlockId,
    chain: &dyn ChainAncestry,
    deployments: &dyn DeploymentView,
    store: &dyn DmlReader,
    cache: &dyn ListCache,
) -> Result<Arc<MnList>, StoreError> {
    let dip3 = Deployment::Buried(BuriedDeployment::Dip0003);
    if !deployments.active_at(at, dip3) {
        return Ok(Arc::new(MnList::default_pre_dip3()));
    }
    let mut records = Vec::new();
    let mut cursor = at;
    let base = loop {
        let header = chain.header(cursor);
        if let Some(list) = cache.get(&header.hash) {
            break list;
        }
        if let Some(list) = store.mn_list_snapshot(&header.hash)? {
            cache.offer(&list, CacheHint::Built);
            break list;
        }
        if !deployments.active_at(cursor, dip3) {
            break Arc::new(MnList::default_pre_dip3());
        }
        let record = store.mn_list_diff(&header.hash)?.ok_or_else(|| {
            StoreError::inconsistent(format!(
                "no mn_diff row for block {} at height {}",
                header.hash, header.height
            ))
        })?;
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
        return Ok(base);
    }
    let at_hash = chain.header(at).hash;
    let mut list = (*base).clone();
    for (hash, record) in records.iter().rev() {
        list.apply_record(*hash, record)
            .map_err(|e| StoreError::inconsistent(format!("mn_diff row for block {hash}: {e}")))?;
        if list.height() % MINI_SNAPSHOT_INTERVAL == 0 && *hash != at_hash {
            cache.offer(&Arc::new(list.clone()), CacheHint::Mini);
        }
    }
    let list = Arc::new(list);
    cache.offer(&list, CacheHint::Built);
    Ok(list)
}
