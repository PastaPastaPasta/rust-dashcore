//! Core's LRU caches (LQ1 §3.7.4; CH7 §4.2.1, §4.4): the members facade's two members caches,
//! the runtime rows' snapshot cache and CH7a's credit-pool cache. Each is process state Core keeps
//! for cost; what an LRU keeps can change which later calls hit, so its policy is Core's.

use std::collections::BTreeMap;

use dash_evo::llmq::ParamsOutOfDomain;
use dash_params::{ChainParams, LlmqParams};
use dash_primitives::LlmqType;
use dash_types::Numeric;

/// Core's `unordered_lru_cache` of `max_size`, with its default threshold of twice that
/// (`unordered_lru_cache.h:26-34`): every `get`, `exists` and `insert` stamps the entry with the
/// next access count, a miss takes none, and once the map holds more than the threshold only the
/// `max_size` most recently stamped entries stay (`:93-118`).
///
/// [`Lru::peek`] reads without a stamp. It is not one of Core's operations: it is for observers
/// that must leave the cache as it was (EV addendum §4.3).
#[derive(Clone, Debug)]
pub struct Lru<K, V> {
    max_size: usize,
    map: BTreeMap<K, (V, i64)>,
    access_counter: i64,
}

impl<K: Ord, V> Lru<K, V> {
    /// An empty cache.
    ///
    /// # Panics
    ///
    /// If `max_size` is zero, which Core asserts too.
    // Core: v24 unordered_lru_cache.h:26 unordered_lru_cache
    pub fn new(max_size: usize) -> Self {
        assert_ne!(max_size, 0, "unordered_lru_cache of size zero");
        Lru {
            max_size,
            map: BTreeMap::new(),
            access_counter: 0,
        }
    }

    /// `max_size()`.
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache holds no entry.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `insert` and `emplace`: the value, overwritten if the key is present, with a new stamp.
    // Core: v24 unordered_lru_cache.h:39 _emplace
    pub fn insert(&mut self, key: K, value: V) {
        let stamp = self.access_counter;
        self.access_counter += 1;
        self.map.insert(key, (value, stamp));
        self.truncate_if_needed();
    }

    /// `get`: the entry, restamped.
    // Core: v24 unordered_lru_cache.h:61 get
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let stamp = self.access_counter;
        let entry = self.map.get_mut(key)?;
        entry.1 = stamp;
        self.access_counter += 1;
        Some(&entry.0)
    }

    /// `exists`: restamps the entry, as `get` does.
    // Core: v24 unordered_lru_cache.h:72 exists
    pub fn exists(&mut self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// The entry, with no stamp.
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.map.get(key).map(|(value, _)| value)
    }

    /// `clear`. The access counter runs on, as Core's does.
    // Core: v24 unordered_lru_cache.h:87 clear
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// The entry's stamp, for tests that check recency.
    #[cfg(test)]
    pub(crate) fn stamp(&self, key: &K) -> Option<i64> {
        self.map.get(key).map(|(_, stamp)| *stamp)
    }

    // Core: v24 unordered_lru_cache.h:93 truncate_if_needed
    fn truncate_if_needed(&mut self) {
        if self.map.len() <= self.max_size.saturating_mul(2) {
            return;
        }
        // Stamps are distinct, so the `max_size` newest are one set, as Core's `nth_element`
        // partition gives.
        let mut stamps: Vec<i64> = self.map.values().map(|(_, stamp)| *stamp).collect();
        let (_, nth, _) = stamps.select_nth_unstable_by(self.max_size - 1, |a, b| b.cmp(a));
        let oldest_kept = *nth;
        self.map.retain(|_, (_, stamp)| *stamp >= oldest_kept);
    }
}

/// Core's `PerLlmqTypeCache` after `Init` (`llmq/cache.h:25-95`): one [`Lru`] per type the chain
/// configures. Core builds them on the first call that uses the cache; the node builds them here,
/// which gives the same answers for fixed parameters (CH7 D79). A type the chain does not
/// configure has no cache: its reads miss and its writes are dropped (`cache.h:20-24`).
#[derive(Clone, Debug)]
pub struct PerTypeLru<K, V> {
    caches: BTreeMap<u8, Lru<K, V>>,
}

impl<K: Ord, V> PerTypeLru<K, V> {
    /// `Init(consensus, limit_by_connections)`: each type's cache sized `keepOldConnections`, or
    /// `keepOldKeys` when `limit_by_connections` is false.
    ///
    /// # Errors
    ///
    /// A type whose size is below one, outside [`LlmqCtx::new`]'s domain.
    ///
    /// [`LlmqCtx::new`]: dash_evo::llmq::LlmqCtx::new
    // Core: v24 llmq/cache.h:45 PerLlmqTypeCache::Init
    pub fn new(
        params: &ChainParams,
        limit_by_connections: bool,
    ) -> Result<Self, ParamsOutOfDomain> {
        Self::sized(params, |llmq| {
            i64::from(if limit_by_connections {
                llmq.keep_old_connections
            } else {
                llmq.keep_old_keys
            })
        })
    }

    /// `Init(consensus, size_fn)`: each type's cache sized by `size`.
    ///
    /// # Errors
    ///
    /// A type whose size is below one or does not fit a `usize`.
    // Core: v24 llmq/cache.h:37 PerLlmqTypeCache::Init
    pub fn sized(
        params: &ChainParams,
        size: impl Fn(&LlmqParams) -> i64,
    ) -> Result<Self, ParamsOutOfDomain> {
        let mut caches = BTreeMap::new();
        for llmq in &params.consensus.llmqs {
            let max_size = usize::try_from(size(llmq))
                .ok()
                .filter(|n| *n > 0)
                .ok_or(ParamsOutOfDomain(llmq.llmq_type))?;
            // `std::map::emplace` keeps the first of a repeated type.
            caches.entry(llmq.llmq_type.to_base()).or_insert_with(|| Lru::new(max_size));
        }
        Ok(PerTypeLru {
            caches,
        })
    }

    /// The type's cache, if the chain configures the type.
    pub fn cache(&self, llmq_type: LlmqType) -> Option<&Lru<K, V>> {
        self.caches.get(&llmq_type.to_base())
    }

    /// `get`.
    // Core: v24 llmq/cache.h:54 PerLlmqTypeCache::get
    pub fn get(&mut self, llmq_type: LlmqType, key: &K) -> Option<&V> {
        self.caches.get_mut(&llmq_type.to_base())?.get(key)
    }

    /// `insert` and `emplace`.
    // Core: v24 llmq/cache.h:60 PerLlmqTypeCache::insert
    pub fn insert(&mut self, llmq_type: LlmqType, key: K, value: V) {
        if let Some(cache) = self.caches.get_mut(&llmq_type.to_base()) {
            cache.insert(key, value);
        }
    }

    /// [`Lru::peek`] in the type's cache.
    pub fn peek(&self, llmq_type: LlmqType, key: &K) -> Option<&V> {
        self.cache(llmq_type)?.peek(key)
    }

    /// `clear(type)`: the type's entries; the other types' are untouched.
    // Core: v24 llmq/cache.h:89 PerLlmqTypeCache::clear
    pub fn clear(&mut self, llmq_type: LlmqType) {
        if let Some(cache) = self.caches.get_mut(&llmq_type.to_base()) {
            cache.clear();
        }
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
