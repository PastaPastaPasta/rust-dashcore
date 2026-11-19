//! Core's quorum contribution retention (v24.0.0 src/llmq/net_quorum.cpp:711-764).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::convert::Infallible;

use dash_consensus::BlockId;
use dash_consensus::primitives::BlockHash;
use dash_params::{LlmqParams, LlmqParamsExt, LlmqType};
use dash_types::Numeric;

use super::{QuorumDbKey, StoredContributions, decode_sk, decode_vvec, encode_sk, encode_vvec};

// Core: v24.0.0 src/llmq/quorums.cpp:17-18 DB_QUORUM_SK_SHARE / DB_QUORUM_QUORUM_VVEC
/// Key prefix of a quorum's stored secret key share row.
pub const DB_QUORUM_SK_SHARE: &str = "q_Qsk";
/// Key prefix of a quorum's stored verification vector row.
pub const DB_QUORUM_VVEC: &str = "q_Qqvvec";

/// Whether Core runs the old quorum data cleanup for a new tip at `height`.
// Core: v24.0.0 src/llmq/net_quorum.cpp:711-722 NetQuorum::StartCleanupOldQuorumDataThread
pub fn cleanup_due(height: i32) -> bool {
    height % 576 == 58
}

#[derive(Clone, Debug)]
struct TypeCache {
    capacity: usize,
    entries: VecDeque<(BlockHash, QuorumDbKey)>,
}

impl TypeCache {
    // Core: v24.0.0 src/unordered_lru_cache.h:28-36 unordered_lru_cache
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::new(),
        }
    }

    // Core: v24.0.0 src/unordered_lru_cache.h:68-77 unordered_lru_cache::get
    fn get(&mut self, base: &BlockHash) -> Option<QuorumDbKey> {
        let position = self.entries.iter().position(|(cached, _)| cached == base)?;
        let entry = self.entries.remove(position)?;
        let key = entry.1;
        self.entries.push_back(entry);
        Some(key)
    }

    // Core: v24.0.0 src/unordered_lru_cache.h:41-52,93-114 unordered_lru_cache::_emplace / truncate_if_needed
    fn insert(&mut self, base: BlockHash, key: QuorumDbKey) {
        if let Some(position) = self.entries.iter().position(|(cached, _)| *cached == base) {
            self.entries.remove(position);
        }
        self.entries.push_back((base, key));
        if self.entries.len() > self.capacity * 2 {
            self.entries.drain(..self.entries.len() - self.capacity);
        }
    }
}

/// Core's per-type LRU caches, initialized once with `keepOldKeys` capacities
/// (v24.0.0 src/llmq/cache.h:37-50).
#[derive(Clone, Debug, Default)]
pub struct CleanupCache {
    caches: BTreeMap<u8, TypeCache>,
}

impl CleanupCache {
    // Core: v24.0.0 src/llmq/cache.h:37-50 PerLlmqTypeCache::Init
    fn init(&mut self, params: &[(LlmqType, LlmqParams)]) {
        if self.caches.is_empty() {
            for (llmq_type, params) in params {
                self.caches
                    .entry(llmq_type.to_base())
                    .or_insert_with(|| TypeCache::new(usize::from(params.keep_old_keys)));
            }
        }
    }

    // Core: v24.0.0 src/llmq/cache.h:54-58 PerLlmqTypeCache::get
    fn get(&mut self, llmq_type: LlmqType, base: &BlockHash) -> Option<QuorumDbKey> {
        self.caches.get_mut(&llmq_type.to_base())?.get(base)
    }

    // Core: v24.0.0 src/llmq/cache.h:60-65 PerLlmqTypeCache::insert
    fn insert(&mut self, llmq_type: LlmqType, base: BlockHash, key: QuorumDbKey) {
        if let Some(cache) = self.caches.get_mut(&llmq_type.to_base()) {
            cache.insert(base, key);
        }
    }
}

/// Core's keep-set, with configured types in chain order (v24.0.0
/// src/llmq/net_quorum.cpp:734-755).
///
/// `ancestors(id)` returns the predecessor ID and the hash of `id`; only genesis has no
/// predecessor. Each predecessor is exactly one block lower, regardless of its arena ID.
/// `scan` supplies at most the requested number of `(base hash, database key)` pairs from the
/// tip, including pairs already found in the cache, as Core's `ScanQuorums` does (:749).
/// `None` means interruption; the caller must leave the store untouched (:758-760).
// Core: v24.0.0 src/llmq/net_quorum.cpp:728-760 NetQuorum::StartCleanupOldQuorumDataThread
pub fn keys_to_keep(
    cache: &mut CleanupCache,
    params: &[(LlmqType, LlmqParams)],
    tip: BlockId,
    ancestors: &dyn Fn(BlockId) -> (Option<BlockId>, BlockHash),
    scan: &mut dyn FnMut(LlmqType, usize) -> Vec<(BlockHash, QuorumDbKey)>,
    interrupted: &dyn Fn() -> bool,
) -> Option<BTreeSet<QuorumDbKey>> {
    cache.init(params);
    let mut keep = BTreeSet::new();
    for &(llmq_type, params) in params {
        if interrupted() {
            return None;
        }
        let capacity = usize::from(params.keep_old_keys);
        let mut current = Some(tip);
        let mut found = BTreeSet::new();
        for _ in 0..LlmqParamsExt::max_store_depth(&params) {
            let Some(id) = current else {
                break;
            };
            let (previous, base_hash) = ancestors(id);
            if let Some(key) = cache.get(llmq_type, &base_hash) {
                found.insert(key);
                if found.len() >= capacity {
                    break;
                }
            }
            current = previous;
        }
        for (base_hash, key) in scan(llmq_type, capacity - found.len()) {
            found.insert(key);
            cache.insert(llmq_type, base_hash, key);
        }
        keep.extend(found);
    }
    (!interrupted()).then_some(keep)
}

/// Core's count of removed rows per prefix (v24.0.0 src/llmq/quorums.cpp:38-65).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CleanupCounts {
    pub vvec: usize,
    pub sk_share: usize,
}

/// Core's two serialized contribution rows, keyed by prefix and quorum database key
/// (v24.0.0 src/llmq/quorums.cpp:17-18,192-235).
#[derive(Default)]
pub struct MemQuorumStore {
    rows: RefCell<BTreeMap<(&'static str, QuorumDbKey), Vec<u8>>>,
}

impl MemQuorumStore {
    /// Empty in-memory equivalent of Core's quorum DB.
    pub fn new() -> Self {
        Self::default()
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:210-235 CQuorum::ReadContributions
    pub fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, Infallible> {
        let rows = self.rows.borrow();
        let Some(bytes) = rows.get(&(DB_QUORUM_VVEC, *key)) else {
            return Ok(None);
        };
        let vvec = decode_vvec(bytes).map(Some);
        let sk_share = rows.get(&(DB_QUORUM_SK_SHARE, *key)).and_then(|bytes| decode_sk(bytes));
        Ok(Some(StoredContributions {
            vvec,
            sk_share,
        }))
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:192-208 CQuorum::WriteContributions
    pub fn write(
        &self,
        key: &QuorumDbKey,
        contributions: &StoredContributions,
    ) -> Result<(), Infallible> {
        let mut rows = self.rows.borrow_mut();
        if let Ok(Some(vvec)) = &contributions.vvec {
            rows.insert((DB_QUORUM_VVEC, *key), encode_vvec(vvec));
        }
        if let Some(sk_share) = contributions.sk_share.as_ref().filter(|sk| sk.is_valid()) {
            rows.insert((DB_QUORUM_SK_SHARE, *key), encode_sk(sk_share).to_vec());
        }
        Ok(())
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:31-76 DataCleanupHelper
    // Core: v24.0.0 src/llmq/quorumsman.cpp:485-489 CQuorumManager::CleanupOldQuorumData
    pub fn erase_except(&self, keep: &BTreeSet<QuorumDbKey>) -> Result<CleanupCounts, Infallible> {
        let mut counts = CleanupCounts::default();
        self.rows.borrow_mut().retain(|(prefix, key), _| {
            if keep.contains(key) {
                return true;
            }
            match *prefix {
                DB_QUORUM_VVEC => counts.vvec += 1,
                DB_QUORUM_SK_SHARE => counts.sk_share += 1,
                _ => return true,
            }
            false
        });
        Ok(counts)
    }
}

#[cfg(test)]
mod tests;
