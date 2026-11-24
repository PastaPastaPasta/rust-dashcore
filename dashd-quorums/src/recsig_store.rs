//! Core's recovered-signature database (`CRecoveredSigsDb::db`) on the node's storage contract.

use dash_kv::{
    Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, MemoryEngine, StoreError,
};
use dash_llmq::signing::db::{RecSigBatch, RecSigKv, RecSigOp, RecSigScan};

/// Core's recovered-signature database as a key-value schema: every row of `CRecoveredSigsDb` in
/// `rows`, under Core's key bytes and with Core's values, which dash-llmq's `RecSigDb` builds.
/// Core's database is not obfuscated (`MakeDbWrapper` passes `obfuscate = false`), so the bytes
/// are Core's own.
// Core: v24.0.2 src/llmq/signing.cpp:24-27, src/dbwrapper.h:770-773
pub static RECSIGDB_SCHEMA: DbSchema = DbSchema {
    name: "recsigdb",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("rows", CfProfile::Ordered)],
};

const ROWS: Cf = Cf::new(1);

/// Core's recovered-signature database on a [`KvEngine`]: dash-llmq's [`RecSigKv`] over the
/// engine's `rows`.
///
/// - A read is the engine's point read, as Core's `db->Read`.
/// - A scan is the engine's iterator positioned at the start key, as Core's `db->NewIterator()` then
///   `Seek`: a view of the rows as of its creation (RocksDB's implicit snapshot, the in-memory
///   engine's map), which later writes do not change. Core's iterators skip the block cache
///   (`fill_cache = false`); this one fills it.
/// - A write is one atomic batch committed [`Commit::Buffered`], Core's `db->WriteBatch(batch)`
///   with `fSync = false`.
///
/// An engine error is the call's `Err`, as Core's `dbwrapper_error` is thrown per call.
// Core: v24.0.2 src/dbwrapper.h:378-385, src/llmq/signing.cpp:176, :238, :269, :344
pub struct KvRecSigStore<E> {
    db: E,
}

impl<E: KvEngine> KvRecSigStore<E> {
    /// The store over `db`, an engine opened with [`RECSIGDB_SCHEMA`].
    pub fn new(db: E) -> Self {
        assert!(std::ptr::eq(db.schema(), &RECSIGDB_SCHEMA), "an engine of the recsig schema");
        Self {
            db,
        }
    }

    /// The value under `key`.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.db.get(ROWS, key)
    }

    /// The rows from `start` on, in key order.
    pub fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, StoreError>, StoreError> {
        Ok(Box::new(self.db.iter_prefix_from(ROWS, &[], start)))
    }

    /// Applies `batch` atomically.
    pub fn write(&self, batch: RecSigBatch) -> Result<(), StoreError> {
        let mut rows = self.db.batch();
        for op in &batch.ops {
            match op {
                RecSigOp::Put(key, value) => rows.put(ROWS, key, value),
                RecSigOp::Delete(key) => rows.delete(ROWS, key),
            }
        }
        self.db.write(rows, Commit::Buffered)
    }
}

impl KvRecSigStore<MemoryEngine> {
    /// An empty store in memory, Core's `memory = true` database (`dash_dbs_in_memory`, tests
    /// only).
    // Core: v24.0.2 src/dbwrapper.cpp:139-142
    pub fn in_memory() -> Self {
        Self::new(MemoryEngine::new(&RECSIGDB_SCHEMA))
    }
}

impl<E: KvEngine> RecSigKv for KvRecSigStore<E> {
    type Error = StoreError;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Self::get(self, key)
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, StoreError>, StoreError> {
        Self::scan_from(self, start)
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), StoreError> {
        Self::write(self, batch)
    }
}

#[cfg(test)]
mod tests;
