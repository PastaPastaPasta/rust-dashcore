//! The runtime rotation-snapshot rows (CH7 §4.4; EV addendum §2.6): Core's
//! `CQuorumSnapshotManager`, the `llmq_snapshot` rows and their 32-entry cache, with Core's two
//! `snapshotCacheCs` sections. A getter holds the section across its cache lookup, its disk read
//! and its insert; a write holds it across the raw write and its insert. So a getter that read a
//! row can never insert it after a write it overlapped has replaced the row (rev6, O1).

use std::sync::Arc;

use dash_dml::StoreError;
use dash_evo::llmq::QuorumSnapshot;
use dash_primitives::{BlockHash, LlmqType};
use dash_types::Numeric;
use dashd_store::KvEngine;
use parking_lot::{Mutex, MutexGuard};

use super::cache::Lru;
use crate::evo::RocksEvoStore;
use crate::flush::EffectLedger;

/// `quorumSnapshotCache{32}` (`llmq/snapshot.cpp:295`).
pub const SNAPSHOT_CACHE_SIZE: usize = 32;

/// The raw rows under [`RuntimeRows`]: read and written outside any evo transaction, as Core's
/// `GetRawDB()` writes them. The facade holds it as a trait object, so it is not generic over the
/// KV engine (CH7 D76).
pub trait SnapshotStore: Send + Sync {
    /// The row, if one was ever written. A row that does not decode is absent, as Core's
    /// `CDBWrapper::Read` answers one whose deserialization throws.
    ///
    /// # Errors
    ///
    /// The read's fault.
    fn read_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError>;

    /// Overwrites the row.
    ///
    /// # Errors
    ///
    /// The write's fault.
    fn write_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError>;
}

impl<E: KvEngine> SnapshotStore for RocksEvoStore<E> {
    /// The `llmq_snapshot` row, read from the DB directly. Core reads it through the evodb's open
    /// transaction (`snapshot.cpp:312`), which never holds the key: its only writer is the raw
    /// write at `:327` (CH7 D80).
    fn read_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError> {
        self.quorum_snapshot(llmq_type, cycle_base)
    }

    fn write_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        self.store_quorum_snapshot(llmq_type, cycle_base, snapshot)
    }
}

/// The rows' cache key: Core hashes `(type, cycle base)`, the node keys on the pair (CH7 D80).
pub(crate) type SnapshotKey = (u8, BlockHash);

pub(crate) fn key(llmq_type: LlmqType, cycle_base: &BlockHash) -> SnapshotKey {
    (llmq_type.to_base(), *cycle_base)
}

/// Core's `CQuorumSnapshotManager` over the node's rows: the one cache, the one section, and the
/// presumed-effect ledger each write records into (§6.2). Outside the crate they are only built,
/// for the facade and the applier: every read and write is a route's (`capability_rules.md`).
pub struct RuntimeRows {
    store: Arc<dyn SnapshotStore>,
    ledger: Arc<EffectLedger>,
    /// `snapshotCacheCs` and the cache it guards.
    section: Mutex<Lru<SnapshotKey, QuorumSnapshot>>,
    #[cfg(test)]
    pub(crate) hooks: RowHooks,
}

impl RuntimeRows {
    // Core: v24 llmq/snapshot.cpp:293 CQuorumSnapshotManager
    pub fn new(store: Arc<dyn SnapshotStore>, ledger: Arc<EffectLedger>) -> Self {
        Self {
            store,
            ledger,
            section: Mutex::new(Lru::new(SNAPSHOT_CACHE_SIZE)),
            #[cfg(test)]
            hooks: RowHooks::default(),
        }
    }

    /// `GetSnapshotForBlock`: a cache hit, restamped; else the disk row, inserted on a hit. The
    /// lookup, the read and the insert are one section.
    ///
    /// # Errors
    ///
    /// The read's fault; nothing is inserted.
    // Core: v24 llmq/snapshot.cpp:301 GetSnapshotForBlock
    pub(crate) fn snapshot_for_block(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError> {
        let key = key(llmq_type, cycle_base);
        let mut cache = self.section();
        if let Some(snapshot) = cache.get(&key) {
            return Ok(Some(snapshot.clone()));
        }
        #[cfg(test)]
        if self.hooks.unlocked_read.load(std::sync::atomic::Ordering::SeqCst) {
            // Case 7's negative control: the section dropped around the read.
            drop(cache);
            let read = self.store.read_snapshot(llmq_type, cycle_base)?;
            self.hooks.after_read();
            cache = self.section();
            if let Some(snapshot) = &read {
                cache.insert(key, snapshot.clone());
            }
            return Ok(read);
        }
        let read = self.store.read_snapshot(llmq_type, cycle_base)?;
        #[cfg(test)]
        self.hooks.after_read();
        if let Some(snapshot) = &read {
            cache.insert(key, snapshot.clone());
        }
        Ok(read)
    }

    /// `StoreSnapshotForBlock`: the raw write and its cache insert in one section, then the
    /// ledger's record, so the next B1 syncs the WAL first (§6.2).
    ///
    /// # Errors
    ///
    /// The write's fault (the guarded engine has tripped the latch); the cache is untouched.
    // Core: v24 llmq/snapshot.cpp:320 StoreSnapshotForBlock
    pub(crate) fn write(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        let mut cache = self.section();
        self.store.write_snapshot(llmq_type, cycle_base, snapshot)?;
        cache.insert(key(llmq_type, cycle_base), snapshot.clone());
        drop(cache);
        self.ledger.record_presumed();
        Ok(())
    }

    /// The cached row, with no stamp (EV addendum §4.3's `peek`), for tests that inspect it.
    #[cfg(test)]
    pub(crate) fn peek(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Option<QuorumSnapshot> {
        self.section().peek(&key(llmq_type, cycle_base)).cloned()
    }

    #[cfg(not(test))]
    fn section(&self) -> MutexGuard<'_, Lru<SnapshotKey, QuorumSnapshot>> {
        self.section.lock()
    }

    #[cfg(test)]
    fn section(&self) -> MutexGuard<'_, Lru<SnapshotKey, QuorumSnapshot>> {
        self.hooks.counted(&self.section)
    }
}

/// The rows' test points (case 7, EV addendum §2.6).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct RowHooks {
    /// Runs in `snapshot_for_block` after its disk read, before its insert, with the section held
    /// (unless `unlocked_read`).
    pub(crate) after_read: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// `snapshot_for_block` drops the section around its disk read.
    pub(crate) unlocked_read: std::sync::atomic::AtomicBool,
    /// Threads blocked on the section now.
    pub(crate) section_waiters: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl RowHooks {
    fn after_read(&self) {
        let hook = self.after_read.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn counted<'a, T>(&self, lock: &'a Mutex<T>) -> MutexGuard<'a, T> {
        use std::sync::atomic::Ordering::SeqCst;
        if let Some(guard) = lock.try_lock() {
            return guard;
        }
        self.section_waiters.fetch_add(1, SeqCst);
        let guard = lock.lock();
        self.section_waiters.fetch_sub(1, SeqCst);
        guard
    }
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
