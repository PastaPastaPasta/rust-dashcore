//! The node's quorum contribution store and its absent DKG and masternode seams: what a node that
//! is neither a masternode nor watching quorums gives Core's `CQuorumManager`.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::sync::{Mutex, MutexGuard, PoisonError};

use dash_evo::types::ProTxHash;
use dash_kv::{
    Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead, MemoryEngine, StoreError,
};
use dash_llmq::quorum::cleanup::{DB_QUORUM_SK_SHARE, DB_QUORUM_VVEC, MemQuorumStore};
use dash_llmq::quorum::{
    CleanupCounts, QuorumDbKey, StoredContributions, decode_sk, decode_vvec, encode_sk, encode_vvec,
};
use dash_params::LlmqType;
use dash_primitives::BlockHash;
use tracing::debug;

use crate::traits::{Contributions, LocalMasternode, QuorumKeyStore, VerifiedContributions};

/// Core's quorum database (`CQuorumManager::db`) as a key-value schema: its `q_Qqvvec` and `q_Qsk`
/// rows in `rows`, under Core's key bytes ([`vvec_row_key`], [`sk_row_key`]) and with Core's
/// values. Core's database is not obfuscated (`MakeDbWrapper` passes `obfuscate = false`), so the
/// bytes are Core's own.
// Core: v24.0.0 src/llmq/quorumsman.cpp:40, src/dbwrapper.h:762-773
pub static QUORUMDB_SCHEMA: DbSchema = DbSchema {
    name: "quorumdb",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("rows", CfProfile::Ordered)],
};

const ROWS: Cf = Cf::new(1);

/// The size estimate at which Core's cleanup writes its batch and starts a new one.
// Core: v24.0.0 src/llmq/quorums.cpp:56-59
const CLEANUP_BATCH_BYTES: usize = 1 << 24;

const VVEC_ROW_KEY: usize = 1 + DB_QUORUM_VVEC.len() + 32;
const SK_ROW_KEY: usize = 1 + DB_QUORUM_SK_SHARE.len() + 32;

/// Core's database key of a quorum's verification vector row: `CDBWrapper`'s serialization of
/// `std::pair<std::string, uint256>`, the prefix's CompactSize length, its bytes, then the key.
// Core: v24.0.0 src/llmq/quorums.cpp:203
pub fn vvec_row_key(key: &QuorumDbKey) -> [u8; VVEC_ROW_KEY] {
    row_key(DB_QUORUM_VVEC, key)
}

/// Core's database key of a quorum's secret key share row, as [`vvec_row_key`].
// Core: v24.0.0 src/llmq/quorums.cpp:206
pub fn sk_row_key(key: &QuorumDbKey) -> [u8; SK_ROW_KEY] {
    row_key(DB_QUORUM_SK_SHARE, key)
}

/// A serialized prefix string: one CompactSize byte (every prefix is shorter than 253) and its
/// bytes.
fn prefix_bytes(prefix: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + prefix.len());
    out.push(u8::try_from(prefix.len()).expect("a short prefix"));
    out.extend_from_slice(prefix.as_bytes());
    out
}

fn row_key<const N: usize>(prefix: &str, key: &QuorumDbKey) -> [u8; N] {
    let mut out = [0; N];
    let (head, tail) = out.split_at_mut(N - 32);
    head.copy_from_slice(&prefix_bytes(prefix));
    tail.copy_from_slice(&key.0);
    out
}

/// Core's quorum database on a [`KvEngine`]: the reads, writes and cleanup of `CQuorumManager`,
/// each under the manager's `cs_db`.
///
/// A write is Core's two `db.Write` calls, each its own buffered commit (`fSync = false`): the
/// verification vector, then the secret key share. A crash between them leaves the vector alone,
/// which reads back as Core's `ReadContributions` does, without a share. An engine error is the
/// call's `Err`; Core's `dbwrapper_error` is thrown per call too.
// Core: v24.0.0 src/llmq/quorumsman.h:78-79 cs_db, src/llmq/quorums.cpp:192-235
pub struct KvQuorumStore<E> {
    db: E,
    cs_db: Mutex<()>,
}

impl<E: KvEngine> KvQuorumStore<E> {
    /// The store over `db`, an engine opened with [`QUORUMDB_SCHEMA`].
    pub fn new(db: E) -> Self {
        assert!(std::ptr::eq(db.schema(), &QUORUMDB_SCHEMA), "an engine of the quorum schema");
        Self {
            db,
            cs_db: Mutex::new(()),
        }
    }

    /// The engine the rows are in: a test's, since a write through it would bypass `cs_db`.
    #[cfg(test)]
    fn engine(&self) -> &E {
        &self.db
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.cs_db.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let mut batch = self.db.batch();
        batch.put(ROWS, key, value);
        self.db.write(batch, Commit::Buffered)
    }

    fn erase(&self, keys: &[Vec<u8>]) -> Result<(), StoreError> {
        let mut batch = self.db.batch();
        for key in keys {
            batch.delete(ROWS, key);
        }
        self.db.write(batch, Commit::Buffered)
    }

    /// Core's `DataCleanupHelper(db, keep, compact = false)`, writing its batch whenever the size
    /// estimate reaches `limit`.
    ///
    /// One scan of a fixed view walks each prefix in Core's order from Core's seek (the prefix with
    /// the all-zero key) and stops at the first key too short for a database key. A key with
    /// trailing bytes counts, and its erase names the key Core re-serializes from the decoded
    /// pair, as `batch.Erase(k)` does. An iterator error ends that prefix's scan as Core's
    /// `pcursor->Valid()` turning false does: Core never reads the iterator's status, so the
    /// erases so far are written, the count logged, the next prefix scanned from its own seek, and
    /// the cleanup succeeds. As Core's `CDBBatch`, the batch is written at the end of each prefix
    /// without being cleared, so the second prefix's write repeats the first's erases; it is
    /// cleared only after a write at the limit.
    // Core: v24.0.0 src/llmq/quorums.cpp:31-76 DataCleanupHelper; src/dbwrapper.h:138-147
    fn cleanup(
        &self,
        keep: &BTreeSet<QuorumDbKey>,
        limit: usize,
    ) -> Result<CleanupCounts, StoreError> {
        let _db = self.lock();
        let view = self.db.snapshot();
        let mut batch: Vec<Vec<u8>> = Vec::new();
        let mut estimate = 0;
        let mut counts = CleanupCounts::default();
        for prefix in [DB_QUORUM_VVEC, DB_QUORUM_SK_SHARE] {
            let head = prefix_bytes(prefix);
            // `Seek(make_tuple(prefix, uint256()))`: the prefix's keys from its all-zero key on.
            let seek = [&head[..], &[0; 32]].concat();
            let mut count = 0;
            let rows = view.iter_prefix_uncached(ROWS, &head);
            for row in rows.skip_while(|row| row.as_ref().is_ok_and(|(key, _)| *key < seek)) {
                let Ok((key, _)) = row else {
                    break;
                };
                let Some(key) = key.get(head.len()..head.len() + 32) else {
                    break;
                };
                let key = QuorumDbKey(key.try_into().expect("32 bytes"));
                if keep.contains(&key) {
                    continue;
                }
                count += 1;
                let erase = [&head[..], &key.0].concat();
                estimate += 2 + usize::from(erase.len() > 127) + erase.len();
                batch.push(erase);
                if estimate >= limit {
                    self.erase(&batch)?;
                    batch.clear();
                    estimate = 0;
                }
            }
            self.erase(&batch)?;
            debug!(target: "llmq", "CQuorumManager::DataCleanupHelper -- {prefix} removed {count}");
            if prefix == DB_QUORUM_VVEC {
                counts.vvec = count;
            } else {
                counts.sk_share = count;
            }
        }
        Ok(counts)
    }
}

impl KvQuorumStore<MemoryEngine> {
    /// An empty store in memory, Core's `memory = true` database (`dash_dbs_in_memory`, tests
    /// only): nothing is on disk, and a wipe has nothing to do.
    // Core: v24.0.0 src/dbwrapper.cpp:139-142
    pub fn in_memory() -> Self {
        Self::new(MemoryEngine::new(&QUORUMDB_SCHEMA))
    }
}

impl<E: KvEngine> QuorumKeyStore for KvQuorumStore<E> {
    type Error = StoreError;

    /// No vector row is "not found"; a share row that does not decode is no share, as Core
    /// ignores its `db.Read` result.
    // Core: v24.0.0 src/llmq/quorums.cpp:210-235 CQuorum::ReadContributions
    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, StoreError> {
        let _db = self.lock();
        let view = self.db.snapshot();
        let Some(vvec) = view.get(ROWS, &vvec_row_key(key))? else {
            return Ok(None);
        };
        let sk_share = view.get(ROWS, &sk_row_key(key))?.and_then(|bytes| decode_sk(&bytes));
        Ok(Some(StoredContributions {
            vvec: decode_vvec(&vvec).map(Some),
            sk_share,
        }))
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:192-208 CQuorum::WriteContributions
    fn write(&self, key: &QuorumDbKey, c: &StoredContributions) -> Result<(), StoreError> {
        let _db = self.lock();
        if let Ok(Some(vvec)) = &c.vvec {
            self.put(&vvec_row_key(key), &encode_vvec(vvec))?;
        }
        if let Some(sk_share) = c.sk_share.as_ref().filter(|sk| sk.is_valid()) {
            self.put(&sk_row_key(key), &encode_sk(sk_share))?;
        }
        Ok(())
    }

    // Core: v24.0.0 src/llmq/quorumsman.cpp:485-489 CQuorumManager::CleanupOldQuorumData
    fn erase_except(&self, keep: &BTreeSet<QuorumDbKey>) -> Result<CleanupCounts, StoreError> {
        self.cleanup(keep, CLEANUP_BATCH_BYTES)
    }
}

/// Core's quorum database in memory for the process's lifetime, over dash-llmq's
/// `MemQuorumStore`: a test's store, and any caller's that wants no engine. The node's is
/// persistent ([`KvQuorumStore`] on its own database, wiped under `-reindex` and
/// `-reindex-chainstate`).
// Core: v24.0.0 src/llmq/quorumsman.cpp:40, src/node/chainstate.cpp:149, :159-160
#[derive(Default)]
pub struct MemKeyStore(Mutex<MemQuorumStore>);

impl MemKeyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl QuorumKeyStore for MemKeyStore {
    type Error = Infallible;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, Infallible> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).read(key)
    }

    fn write(&self, key: &QuorumDbKey, c: &StoredContributions) -> Result<(), Infallible> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).write(key, c)
    }

    fn erase_except(&self, keep: &BTreeSet<QuorumDbKey>) -> Result<CleanupCounts, Infallible> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).erase_except(keep)
    }
}

/// No DKG session manager: Core's null `m_qdkgsman`, which only a masternode's or a quorum
/// watcher's `NetDKG` connects (v24.0.0 src/llmq/net_dkg.cpp:475, :496; src/init.cpp:2213-2226).
/// `BuildQuorumContributions` then fails at once (quorumsman.cpp:118-122), so a build only reads
/// the store and writes nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoDkgSessions;

impl VerifiedContributions for NoDkgSessions {
    // Core: v24.0.0 src/llmq/quorumsman.cpp:118-122 BuildQuorumContributions
    fn verified(
        &self,
        _: LlmqType,
        _: &BlockHash,
        _: &[ProTxHash],
        _: &[bool],
    ) -> Option<Contributions> {
        None
    }
}

/// Not a masternode: Core's null `m_handler`, whose `QuorumRole::GetProTxHash` default is the
/// null hash (v24.0.0 src/llmq/quorumsman.h:74-75, :251).
#[derive(Clone, Copy, Debug, Default)]
pub struct NotMasternode;

impl LocalMasternode for NotMasternode {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        None
    }
}

#[cfg(test)]
mod tests;
