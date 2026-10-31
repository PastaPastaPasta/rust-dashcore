//! The evo caches CH7a hosts (CH7 §3.7): process state Core keeps for cost, never for an answer.
//! Each is keyed by block hash over rows that are never rewritten with other content (D6), so a
//! hit and a miss give the same answer.

use std::sync::{Mutex, MutexGuard};

use dash_dml::creditpool::{PoolState, pool_after};
use dash_dml::mnhf::MnhfManager;
use dash_dml::store::LruListCache;
use dash_dml::{EvoStore, SmlRootCache, StoreError};
use dash_primitives::BlockHash;

use crate::llmq::cache::Lru;

/// `CreditPoolCacheSize` (`evo/creditpool.h:119`).
pub const CREDIT_POOL_CACHE_SIZE: usize = 1000;

/// `creditPoolCache` (`cp:122-171`): each block's pool after it, as read from the store.
#[derive(Debug)]
pub struct CreditPoolCache {
    lru: Mutex<Lru<BlockHash, PoolState>>,
}

impl Default for CreditPoolCache {
    fn default() -> Self {
        Self {
            lru: Mutex::new(Lru::new(CREDIT_POOL_CACHE_SIZE)),
        }
    }
}

impl CreditPoolCache {
    fn lru(&self) -> MutexGuard<'_, Lru<BlockHash, PoolState>> {
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
        if let Some(pool) = self.lru().get(block).cloned() {
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
