//! The evo caches CH7a hosts (CH7 §3.7): process state Core keeps for cost, never for an answer.
//! Each is keyed by block hash over rows that are never rewritten with other content (D6), so a
//! hit and a miss give the same answer.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use dash_dml::creditpool::{PoolState, pool_after};
use dash_dml::mnhf::MnhfManager;
use dash_dml::store::LruListCache;
use dash_dml::{EvoStore, SmlRootCache, StoreError};
use dash_primitives::BlockHash;

/// `CreditPoolCacheSize` (`evo/creditpool.h:119`).
pub const CREDIT_POOL_CACHE_SIZE: usize = 1000;

/// Core's `unordered_lru_cache` of `size` (`unordered_lru_cache.h:15-118`), as dash-dml's MNHF
/// cache ports it: every hit and insert stamps the entry with the next access count, a miss takes
/// none, and past twice the size only the `size` most recently stamped entries stay.
#[derive(Debug)]
struct UnorderedLru<V> {
    size: usize,
    map: BTreeMap<BlockHash, (V, i64)>,
    access_counter: i64,
}

impl<V: Clone> UnorderedLru<V> {
    fn new(size: usize) -> Self {
        Self {
            size,
            map: BTreeMap::new(),
            access_counter: 0,
        }
    }

    fn get(&mut self, key: &BlockHash) -> Option<V> {
        let stamp = self.access_counter;
        let entry = self.map.get_mut(key)?;
        entry.1 = stamp;
        self.access_counter += 1;
        Some(entry.0.clone())
    }

    fn insert(&mut self, key: BlockHash, value: V) {
        let stamp = self.access_counter;
        self.access_counter += 1;
        self.map.insert(key, (value, stamp));
        if self.map.len() > 2 * self.size {
            let mut stamps: Vec<i64> = self.map.values().map(|(_, stamp)| *stamp).collect();
            let (_, nth, _) = stamps.select_nth_unstable_by(self.size - 1, |a, b| b.cmp(a));
            let oldest_kept = *nth;
            self.map.retain(|_, (_, stamp)| *stamp >= oldest_kept);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

/// `creditPoolCache` (`cp:122-171`): each block's pool after it, as read from the store.
#[derive(Debug)]
pub struct CreditPoolCache {
    lru: Mutex<UnorderedLru<PoolState>>,
}

impl Default for CreditPoolCache {
    fn default() -> Self {
        Self {
            lru: Mutex::new(UnorderedLru::new(CREDIT_POOL_CACHE_SIZE)),
        }
    }
}

impl CreditPoolCache {
    fn lru(&self) -> MutexGuard<'_, UnorderedLru<PoolState>> {
        // A panic while the lock was held cannot leave an entry half-written.
        self.lru.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// [`pool_after`] through the cache: below V20 the empty pool, uncached, as Core's
    /// `GetFromCache` answers before its lookup; otherwise a hit, or the store's answer, kept.
    ///
    /// # Errors
    ///
    /// The store's fault, which is not kept.
    pub fn pool_after(
        &self,
        store: &dyn EvoStore,
        block: &BlockHash,
        v20_active_at: bool,
    ) -> Result<PoolState, StoreError> {
        if !v20_active_at {
            return pool_after(store, block, false);
        }
        if let Some(pool) = self.lru().get(block) {
            return Ok(pool);
        }
        let pool = pool_after(store, block, true)?;
        self.lru().insert(*block, pool.clone());
        Ok(pool)
    }
}

/// The caches of one chainstate (§3.7).
#[derive(Default)]
pub struct EvoCaches {
    /// `mnListsCache` and its mini-snapshots: `MAX_CACHED_LISTS` with Core's cleanup.
    pub lists: LruListCache,
    /// The MN-list root's incremental SML (`sml:110-114`), one per chainstate.
    pub sml: SmlRootCache,
    /// `creditPoolCache`.
    pub pools: CreditPoolCache,
    /// `mnhfCache`, inside dash-dml's MNHF manager.
    pub mnhf: MnhfManager,
}

#[cfg(test)]
#[path = "caches_tests.rs"]
mod tests;
