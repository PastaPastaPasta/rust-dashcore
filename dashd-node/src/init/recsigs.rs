//! Core's recovered-signature database (`CSigningManager::db`) as init opens it, and
//! `-maxrecsigsage`.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use dash_args::{ArgsManager, RuntimeError};
use dash_kv::{MemoryEngine, StoreError};
use dashd_quorums::{KvRecSigStore, RECSIGDB_SCHEMA, RecSigBatch, RecSigKv, RecSigScan};
use dashd_store::{RocksConfig, RocksEngine};

use super::quorums::{LlmqDbError, LlmqDbFiles};

/// The recovered-signature database's directory in the network data directory, Core's
/// `llmq/recsigdb`.
// Core: v24.0.2 src/llmq/signing.cpp:25
pub const RECSIG_DB_DIR: &str = "llmq/recsigdb";

/// Where a wipe moves the recovered-signature database before deleting it.
pub(crate) const WIPING_DIR: &str = "llmq/recsigdb.wipe";

/// `-maxrecsigsage`'s default: a week, in seconds.
// Core: v24.0.2 src/llmq/options.h:30-31
pub const DEFAULT_MAX_RECOVERED_SIGS_AGE: i64 = 60 * 60 * 24 * 7;

/// Core's `options.max_recsigs_age`: `-maxrecsigsage` as `GetIntArg` reads it, unchecked.
// Core: v24.0.2 src/init.cpp:2075
pub fn max_recsigs_age(args: &ArgsManager) -> Result<i64, RuntimeError> {
    args.get_int_arg("-maxrecsigsage", DEFAULT_MAX_RECOVERED_SIGS_AGE)
}

/// The node's recovered-signature database (`CRecoveredSigsDb::db`): a handle the signing manager
/// and init share, opened by [`open`](Self::open) inside the chainstate load, right after the
/// quorum database, where Core's `LLMQContext` builds `sigman` right after `qman`. Before the open
/// every read and write is an error. The database closes when the last handle drops.
#[derive(Clone)]
pub struct RecoveredSigsDb {
    backend: Arc<OnceLock<Backend>>,
    files: LlmqDbFiles,
}

enum Backend {
    Rocks(KvRecSigStore<RocksEngine>),
    Memory(KvRecSigStore<MemoryEngine>),
}

impl RecoveredSigsDb {
    /// A handle whose database is not open yet.
    pub fn unopened() -> Self {
        Self {
            backend: Arc::default(),
            files: LlmqDbFiles::new(RECSIG_DB_DIR, WIPING_DIR, "recovered signature database"),
        }
    }

    /// An open, empty database in memory, Core's `memory = true` (`dash_dbs_in_memory`, tests
    /// only): [`open`](Self::open) then touches no file.
    // Core: v24.0.2 src/dbwrapper.cpp:139-142
    pub fn in_memory() -> Self {
        let db = Self::unopened();
        let _ = db.backend.set(Backend::Memory(KvRecSigStore::in_memory()));
        db
    }

    /// Core's `CDBWrapper` constructor for `llmq/recsigdb`, opened at `<net_dir>/recsigdb`, with the quorum
    /// database's wipe ([`QuorumDb::open`](super::quorums::QuorumDb::open)).
    // Core: v24.0.2 src/llmq/signing.cpp:24-27, src/dbwrapper.cpp:130-158
    pub fn open(&self, net_dir: &Path, wipe: bool) -> Result<(), LlmqDbError> {
        match self.backend.get() {
            Some(Backend::Memory(_)) => return Ok(()),
            Some(Backend::Rocks(_)) => return Err(self.files.already_open()),
            None => {}
        }
        let engine = self.files.open(net_dir, wipe, &RECSIGDB_SCHEMA, &engine_config())?;
        self.backend
            .set(Backend::Rocks(KvRecSigStore::new(engine)))
            .map_err(|_| self.files.already_open())
    }

    #[cfg(test)]
    pub(crate) fn fail_wipe_at(&self, step: super::quorums::WipeStep) {
        self.files.fail_wipe_at(step);
    }

    fn backend(&self) -> Result<&Backend, StoreError> {
        self.backend.get().ok_or_else(|| self.files.not_open())
    }
}

/// Core's `GetOptions(8 << 20)` for the recovered-signature database: half of the 8 MiB budget
/// for the block cache, a quarter per write buffer with two of them; one background job. The
/// ordered column family takes no bloom filter.
// Core: v24.0.2 src/dbwrapper.cpp:113-117, src/llmq/signing.cpp:25
fn engine_config() -> RocksConfig {
    RocksConfig {
        block_cache_bytes: 1 << 22,
        write_buffer_bytes: 1 << 21,
        max_write_buffers: 2,
        total_write_buffer_bytes: 1 << 22,
        max_background_jobs: 1,
        bloom_bits_per_key: 10.0,
        ..RocksConfig::default()
    }
}

/// Each call is the open database's; before the open, an error. An engine failure is the call's
/// error, which the signing composition maps to its fault.
impl RecSigKv for RecoveredSigsDb {
    type Error = StoreError;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        match self.backend()? {
            Backend::Rocks(store) => store.get(key),
            Backend::Memory(store) => store.get(key),
        }
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, StoreError>, StoreError> {
        match self.backend()? {
            Backend::Rocks(store) => store.scan_from(start),
            Backend::Memory(store) => store.scan_from(start),
        }
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), StoreError> {
        match self.backend()? {
            Backend::Rocks(store) => store.write(batch),
            Backend::Memory(store) => store.write(batch),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
