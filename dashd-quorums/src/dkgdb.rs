//! Core's DKG contributions database (`CDKGSessionManager::db`, v24.0.0
//! `llmq/dkgsessionmgr.cpp`): the verified vvecs, verified sk contributions and encrypted
//! contributions a masternode's DKG sessions write, and the verified contributions cache.
//!
//! Core keeps it in LevelDB at `<datadir>/llmq/dkgdb` (dkgsessionmgr.cpp:35), wiped under
//! `-reindex` and `-reindex-chainstate` (init.cpp:2155, through active/context.cpp:43 and
//! llmq/observer.cpp:16), where `fReindex` includes the stored reindexing flag
//! (node/blockstorage.cpp:402-404). Here it is a [`KvEngine`] of [`DKGDB_SCHEMA`] behind the
//! node's fail-stop latch ([`LatchedEngine`]); the host opens it at [`dkgdb_path`] and passes
//! its reindex flags, the stored one included, to [`DkgDb::open`].
//!
//! The DKG session writes and reads it through [`DkgStore`], the quorum manager through
//! [`VerifiedContributions`]. The host's two schedules: [`DkgDb::cleanup_cache`] at every
//! `UpdatedBlockTip`, before its IBD and spork checks (dkgsessionmgr.cpp:41-43), and
//! [`DkgDb::cleanup_old_contributions`] once an hour off V's thread (init.cpp:2369).

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dash_consensus::primitives::{CoreWrite, read_compact_size, write_compact_size};
use dash_evo::types::ProTxHash;
use dash_kv::{
    AbortSink, Cf, CfProfile, CfSpec, Commit, DbSchema, ErrorKind, Failure, GlobalFailStop,
    KvBatch, KvEngine, KvRead, LatchedEngine, Sites, StoreError,
};
use dash_llmq::dkg::bls::{BlsIesMultiBlobs, BlsPkValue, BlsSchemeKind, BlsSkValue, IES_SCHEME};
use dash_llmq::dkg::session::{self, DkgStore, MissingVvec};
use dash_params::{ChainSetup, LlmqParamsExt, LlmqType};
use dash_pkc::bls::core::BlsIesBlobValue;
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use tracing::{debug, warn};

use crate::traits::{Contributions, DkgChain, VerifiedContributions};

/// The dkgdb's schema: its rows in one ordered column family, keyed as Core keys them.
pub static DKGDB_SCHEMA: DbSchema = DbSchema {
    name: "dkgdb",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("dkg", CfProfile::Ordered)],
};

const ROWS: Cf = Cf::new(1);

const SITES: Sites = Sites {
    read: "dkgdb read",
    write: "dkgdb write",
    barrier: "dkgdb barrier",
};

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:24-26
const DB_VVEC: &[u8; 6] = b"qdkg_V";
const DB_SKCONTRIB: &[u8; 6] = b"qdkg_S";
const DB_ENC_CONTRIB: &[u8; 6] = b"qdkg_E";
const PREFIXES: [&[u8; 6]; 3] = [DB_VVEC, DB_SKCONTRIB, DB_ENC_CONTRIB];

/// A row key's length: the prefix string, the type, the base hash and the proTxHash.
const KEY_LEN: usize = 1 + 6 + 1 + 32 + 32;

/// `MAX_CONTRIBUTION_CACHE_TIME` (v24.0.0 src/llmq/dkgsessionmgr.h:53).
pub const MAX_CONTRIBUTION_CACHE_TIME: Duration = Duration::from_secs(60);

/// `<datadir>/llmq/dkgdb` under the network's data directory (dkgsessionmgr.cpp:35).
pub fn dkgdb_path(net_dir: &Path) -> PathBuf {
    net_dir.join("llmq").join("dkgdb")
}

/// A dkgdb failure. A database read or write failure, or a vvec row that does not read, has
/// tripped the node's latch and reached its abort sink before the caller sees it; a members
/// failure has not.
#[derive(Debug, thiserror::Error)]
pub enum DkgDbError {
    #[error("dkgdb: {0}")]
    Db(#[from] StoreError),
    #[error("dkgdb quorum members: {0}")]
    Members(#[from] dash_dml::StoreError),
}

type CacheKey = (u8, [u8; 32], [u8; 32]);

struct CacheEntry {
    time: Instant,
    vvec: Arc<Vec<BlsPkValue>>,
    sk: BlsSkValue,
}

/// Core's `CDKGSessionManager` database and contributions cache. `C` gives
/// `GetAllQuorumMembers`, `LookupBlockIndex`, `ActiveHeight` and the BLS scheme ([`DkgChain`]).
pub struct DkgDb<E, C> {
    db: LatchedEngine<E>,
    abort: Arc<dyn AbortSink>,
    params: Arc<ChainSetup>,
    chain: C,
    cache: Mutex<BTreeMap<CacheKey, CacheEntry>>,
    now: Box<dyn Fn() -> Instant + Send + Sync>,
}

/// Core's tuple key `(prefix, llmqType, quorumBaseBlockHash, proTxHash)`.
// Core: v24.0.0 src/serialize.h:1038-1044 (string), :955-964 (enum), src/uint256.h:79-83
fn row_key(prefix: &[u8; 6], t: LlmqType, base: &BlockHash, pro_tx: &ProTxHash) -> Vec<u8> {
    let mut key = type_prefix(prefix, t);
    key.write(base.as_bytes());
    key.write(pro_tx.as_bytes());
    key
}

fn type_prefix(prefix: &[u8; 6], t: LlmqType) -> Vec<u8> {
    let mut key = Vec::with_capacity(KEY_LEN);
    write_compact_size(prefix.len() as u64, &mut key);
    key.write(prefix);
    key.write(&[t.to_base()]);
    key
}

/// The vvec row: a compact size, then each key in the basic encoding.
// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:127-131
fn encode_vvec(vvec: &[BlsPkValue]) -> Vec<u8> {
    let mut row = Vec::with_capacity(9 + vvec.len() * 48);
    write_compact_size(vvec.len() as u64, &mut row);
    for pk in vvec {
        row.write(&pk.to_bytes(BlsSchemeKind::Basic));
    }
    row
}

/// `ReadCompactSize`, then each key in the basic encoding, as [`DkgStore`]'s contract reads it:
/// bytes that don't decode under the basic scheme are the invalid key. `None` for a row too short
/// for its count, where Core's read throws. Trailing bytes are not read.
// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:171-178
fn decode_vvec(mut row: &[u8]) -> Option<Vec<BlsPkValue>> {
    let count = read_compact_size(&mut row, true).ok()?;
    let mut vvec = Vec::new();
    for _ in 0..count {
        let (bytes, rest) = row.split_first_chunk::<48>()?;
        row = rest;
        let key = BlsPkValue::from_canonical(bytes, BlsSchemeKind::Basic);
        vvec.push(key.unwrap_or_else(BlsPkValue::invalid));
    }
    Some(vvec)
}

/// `db->Read(key, skContribution)` into a default key: canonical bytes give the key; a short
/// row, zeros or bytes that do not decode leave it invalid. Read under the basic scheme, which
/// differs from the legacy one only for the group order's own bytes, which no writer writes.
// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:180-181, src/dbwrapper.h:315-327
fn decode_sk(row: &[u8]) -> BlsSkValue {
    row.first_chunk::<32>()
        .and_then(|bytes| BlsSkValue::from_canonical(bytes, BlsSchemeKind::Basic))
        .unwrap_or_else(BlsSkValue::invalid)
}

impl<E: KvEngine, C: DkgChain> DkgDb<E, C> {
    /// Core's `MakeDbWrapper({path / "llmq" / "dkgdb", memory, wipe})`: `engine` behind `latch`,
    /// emptied first when `wipe` (`-reindex` or `-reindex-chainstate`) in one durable batch.
    ///
    /// # Errors
    ///
    /// An engine of another schema, which trips nothing. The wipe's write failure, which has
    /// tripped the latch: its caller (init) reports one of any kind but `ReadOnly` through
    /// [`AbortSink::abort_node`], as the [`DkgStore`] writers do.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:35, src/init.cpp:2155
    pub fn open(
        engine: E,
        wipe: bool,
        params: Arc<ChainSetup>,
        chain: C,
        latch: GlobalFailStop,
        abort: Arc<dyn AbortSink>,
    ) -> Result<Self, DkgDbError> {
        if engine.schema().name != DKGDB_SCHEMA.name {
            return Err(StoreError::engine("dkgdb", ErrorKind::Other, "not a dkgdb engine").into());
        }
        let db = LatchedEngine::new(engine, latch, abort.clone(), SITES);
        if wipe {
            let mut batch = db.batch();
            batch.delete_range(ROWS, &[], &[0xff; 1024]);
            db.write(batch, Commit::Durable)?;
        }
        Ok(Self {
            db,
            abort,
            params,
            chain,
            cache: Mutex::default(),
            now: Box::new(Instant::now),
        })
    }

    /// The cache's clock, `SteadyClock::now`: tests inject one.
    pub fn with_clock(mut self, now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        self.now = Box::new(now);
        self
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), DkgDbError> {
        let mut batch = self.db.batch();
        batch.put(ROWS, key, value);
        Ok(self.db.write(batch, Commit::Buffered)?)
    }

    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:125-133 WriteVerifiedVvecContribution
    fn put_vvec(
        &self,
        t: LlmqType,
        base: &BlockHash,
        who: &ProTxHash,
        vvec: &[BlsPkValue],
    ) -> Result<(), DkgDbError> {
        self.put(&row_key(DB_VVEC, t, base, who), &encode_vvec(vvec))
    }

    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:135-138 WriteVerifiedSkContribution
    fn put_sk(
        &self,
        t: LlmqType,
        base: &BlockHash,
        who: &ProTxHash,
        sk: &BlsSkValue,
    ) -> Result<(), DkgDbError> {
        self.put(&row_key(DB_SKCONTRIB, t, base, who), &sk.to_bytes(BlsSchemeKind::Basic))
    }

    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:140-143 WriteEncryptedContributions
    fn put_encrypted(
        &self,
        t: LlmqType,
        base: &BlockHash,
        who: &ProTxHash,
        blobs: &BlsIesMultiBlobs,
        scheme: BlsSchemeKind,
    ) -> Result<(), DkgDbError> {
        self.put(&row_key(DB_ENC_CONTRIB, t, base, who), &blobs.to_bytes(scheme))
    }

    /// `GetVerifiedContributions`: the member indexes, vvecs and sk contributions of `valid`'s
    /// members, in member order. A missing sk row gives the invalid key.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:145-192 GetVerifiedContributions
    fn contributions(
        &self,
        t: LlmqType,
        base: &BlockHash,
        valid: &[bool],
    ) -> Result<Result<Contributions, MissingVvec>, DkgDbError> {
        let members = self.chain.members(t, base)?;
        self.contributions_of(t, base, &members, valid)
    }

    /// [`contributions`](Self::contributions) over `members`, the quorum's members read by the
    /// caller.
    fn contributions_of(
        &self,
        t: LlmqType,
        base: &BlockHash,
        members: &[ProTxHash],
        valid: &[bool],
    ) -> Result<Result<Contributions, MissingVvec>, DkgDbError> {
        let mut indexes = Vec::with_capacity(members.len());
        let mut vvecs = Vec::with_capacity(members.len());
        let mut sks = Vec::with_capacity(members.len());
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        for (i, pro_tx) in members.iter().enumerate().filter(|&(i, _)| valid.get(i) == Some(&true))
        {
            let cache_key = (t.to_base(), *base.as_bytes(), *pro_tx.as_bytes());
            let entry = match cache.entry(cache_key) {
                Entry::Occupied(cached) => cached.into_mut(),
                Entry::Vacant(slot) => {
                    let Some(read) = self.read_verified(t, base, pro_tx)? else {
                        return Ok(Err(MissingVvec {
                            who: *pro_tx,
                        }));
                    };
                    slot.insert(read)
                }
            };
            indexes.push(i as u16);
            vvecs.push(entry.vvec.clone());
            sks.push(entry.sk.clone());
        }
        Ok(Ok((indexes, vvecs, sks)))
    }

    /// A member's vvec row and sk row, stamped now; `None` without a vvec row.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:164-183
    fn read_verified(
        &self,
        t: LlmqType,
        base: &BlockHash,
        pro_tx: &ProTxHash,
    ) -> Result<Option<CacheEntry>, DkgDbError> {
        let Some(row) = self.db.get(ROWS, &row_key(DB_VVEC, t, base, pro_tx))? else {
            return Ok(None);
        };
        let Some(vvec) = decode_vvec(&row) else {
            return Err(self.corrupt(format!("vvec row for protx={pro_tx}")).into());
        };
        let sk = self
            .db
            .get(ROWS, &row_key(DB_SKCONTRIB, t, base, pro_tx))?
            .map_or_else(BlsSkValue::invalid, |row| decode_sk(&row));
        Ok(Some(CacheEntry {
            time: (self.now)(),
            vvec: Arc::new(vvec),
            sk,
        }))
    }

    /// `pro_tx`'s encrypted contribution from each of `valid`'s members, in member order, read
    /// under [`IES_SCHEME`] whatever the chain's scheme (develop `bls/bls_ies.h:82`). `None`,
    /// Core's `false`, when `pro_tx` is not a member or a valid member's row is missing or does
    /// not read; a row without a blob at `pro_tx`'s index, where Core reads out of bounds, is
    /// `None` too.
    ///
    /// # Errors
    ///
    /// The members access's fault; a database read failure, which has tripped the latch.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:194-225 GetEncryptedContributions
    pub fn encrypted_contributions(
        &self,
        t: LlmqType,
        base: &BlockHash,
        valid: &[bool],
        pro_tx: &ProTxHash,
    ) -> Result<Option<Vec<BlsIesBlobValue>>, DkgDbError> {
        let members = self.chain.members(t, base)?;
        let Some(requested) = members.iter().position(|m| m == pro_tx) else {
            debug!(target: "llmq", "CDKGSessionManager::GetEncryptedContributions -- not a member, nProTxHash={pro_tx}");
            return Ok(None);
        };
        let mut out = Vec::with_capacity(members.len());
        for (_, member) in members.iter().enumerate().filter(|&(i, _)| valid.get(i) == Some(&true))
        {
            let blob = self
                .db
                .get(ROWS, &row_key(DB_ENC_CONTRIB, t, base, member))?
                .and_then(|row| BlsIesMultiBlobs::decode(&mut &row[..], IES_SCHEME).ok())
                .and_then(|blobs| blobs.get(requested));
            let Some(blob) = blob else {
                debug!(target: "llmq", "CDKGSessionManager::GetEncryptedContributions -- can't read from db, nProTxHash={pro_tx}");
                return Ok(None);
            };
            out.push(blob);
        }
        Ok(Some(out))
    }

    /// Drops the cache entries older than [`MAX_CONTRIBUTION_CACHE_TIME`].
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:227-238 CleanupCache
    pub fn cleanup_cache(&self) {
        let now = (self.now)();
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.retain(|_, e| now.saturating_duration_since(e.time) <= MAX_CONTRIBUTION_CACHE_TIME);
    }

    /// Erases, per configured LLMQ type and per row kind, the rows whose base block is unknown or
    /// more than `max_store_depth()` below the active height, in one batch per type. Returns at
    /// once on an empty database, and without writing the type's batch when no chain answers.
    ///
    /// # Errors
    ///
    /// A database read or write failure, which has tripped the latch and been reported (a write
    /// failure through [`AbortSink::abort_node`], as the [`DkgStore`] writers report theirs): its
    /// caller (the hourly scheduler) reports nothing more. A caller could not tell the two apart.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:240-282 CleanupOldContributions
    pub fn cleanup_old_contributions(&self) -> Result<(), DkgDbError> {
        if self.db.iter_prefix(ROWS, &[]).next().transpose()?.is_none() {
            return Ok(());
        }
        for (t, params) in self.params.consensus.llmq_params() {
            debug!(target: "llmq", "CDKGSessionManager::CleanupOldContributions -- looking for old entries for llmq type {}", t.to_base());
            let depth = i64::from(params.max_store_depth());
            let mut batch = self.db.batch();
            let (mut old, mut all) = (0usize, 0usize);
            for prefix in PREFIXES {
                let Some(active) = self.chain.active_height() else {
                    return Ok(());
                };
                let active = i64::from(active);
                for row in self.db.iter_prefix(ROWS, &type_prefix(prefix, t)) {
                    let (key, _) = row?;
                    let Some(key) = key.get(..KEY_LEN) else {
                        break;
                    };
                    all += 1;
                    let base =
                        BlockHash::from(<[u8; 32]>::try_from(&key[8..40]).expect("a 72-byte key"));
                    let height = self.chain.height(&base);
                    if height.is_none_or(|h| active - i64::from(h) > depth) {
                        batch.delete(ROWS, key);
                        old += 1;
                    }
                }
            }
            debug!(target: "llmq", "CDKGSessionManager::CleanupOldContributions -- found {all} entries for llmq type {}", t.to_base());
            if old > 0 {
                self.db.write(batch, Commit::Buffered).map_err(|e| {
                    let e = e.into();
                    self.fail_stop(&e, true);
                    e
                })?;
                debug!(target: "llmq", "CDKGSessionManager::CleanupOldContributions -- removed {old} old entries for llmq type {}", t.to_base());
            }
        }
        Ok(())
    }

    /// A row that does not read: Core's read throws. Trips the latch as a failed read does.
    fn corrupt(&self, what: String) -> StoreError {
        let message = format!("dkgdb: {what} does not read");
        self.trip(ErrorKind::Corruption, &message);
        StoreError::engine("dkgdb", ErrorKind::Corruption, message)
    }

    fn trip(&self, kind: ErrorKind, message: &str) {
        let failure = Failure {
            kind,
            site: SITES.read,
            message: message.to_owned(),
        };
        if self.db.latch().trip(failure) {
            self.abort.read_failed(message);
        }
    }

    /// [`DkgStore`]'s fail-stop. A members fault trips the latch here. A database failure has
    /// tripped it already, and a failed read has reported; a failed write of any kind but
    /// `ReadOnly` latched in this call (`LatchedEngine`'s contract) and reports here, as Core's
    /// uncaught `dbwrapper_error` stops the node. A `ReadOnly` refusal follows a report.
    // Core: v24.0.0 src/dbwrapper.cpp:208, :261-269 HandleError
    fn fail_stop(&self, e: &DkgDbError, wrote: bool) {
        match e {
            DkgDbError::Members(_) => self.trip(ErrorKind::Other, &e.to_string()),
            DkgDbError::Db(e) if wrote && e.kind() != Some(ErrorKind::ReadOnly) => {
                self.abort.abort_node(&format!("Failed to write to the DKG database: {e}"), None);
            }
            DkgDbError::Db(_) => {}
        }
    }
}

fn base_hash(base: &QuorumHash) -> BlockHash {
    BlockHash::from(*base.as_bytes())
}

/// The DKG session's dkgdb, shared: the session writes through `&DkgDb` while the quorum
/// manager reads. A failure fail-stops: the latch trips and the call returns as if nothing were
/// read or written.
impl<E: KvEngine, C: DkgChain> DkgStore for &DkgDb<E, C> {
    fn write_vvec(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, vvec: &[BlsPkValue]) {
        if let Err(e) = self.put_vvec(t, &base_hash(base), who, vvec) {
            self.fail_stop(&e, true);
        }
    }

    fn write_sk(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, sk: &BlsSkValue) {
        if let Err(e) = self.put_sk(t, &base_hash(base), who, sk) {
            self.fail_stop(&e, true);
        }
    }

    /// The ephemeral key serialized under [`IES_SCHEME`] whatever the chain's scheme (develop
    /// `bls/bls_ies.h:82`).
    fn write_encrypted(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        who: &ProTxHash,
        blobs: &BlsIesMultiBlobs,
    ) {
        if let Err(e) = self.put_encrypted(t, &base_hash(base), who, blobs, IES_SCHEME) {
            self.fail_stop(&e, true);
        }
    }

    fn verified_contributions(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        valid: &[bool],
    ) -> Result<session::VerifiedContributions, MissingVvec> {
        match self.contributions(t, &base_hash(base), valid) {
            Ok(read) => read.map(|(member_indexes, vvecs, sk_contributions)| {
                session::VerifiedContributions {
                    member_indexes,
                    vvecs: vvecs.iter().map(|v| v.to_vec()).collect(),
                    sk_contributions,
                }
            }),
            Err(e) => {
                self.fail_stop(&e, false);
                Ok(session::VerifiedContributions::default())
            }
        }
    }
}

/// The owned store, through the shared one.
impl<E: KvEngine, C: DkgChain> DkgStore for DkgDb<E, C> {
    fn write_vvec(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, vvec: &[BlsPkValue]) {
        (&*self).write_vvec(t, base, who, vvec);
    }

    fn write_sk(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, sk: &BlsSkValue) {
        (&*self).write_sk(t, base, who, sk);
    }

    fn write_encrypted(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        who: &ProTxHash,
        blobs: &BlsIesMultiBlobs,
    ) {
        (&*self).write_encrypted(t, base, who, blobs);
    }

    fn verified_contributions(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        valid: &[bool],
    ) -> Result<session::VerifiedContributions, MissingVvec> {
        (&*self).verified_contributions(t, base, valid)
    }
}

impl<E: KvEngine, C: DkgChain> VerifiedContributions for DkgDb<E, C> {
    /// `GetVerifiedContributions` for `BuildQuorumContributions` over the members the build read
    /// on V; `None` for a missing vvec row or a fault, the latch already tripped for a database
    /// one. It never calls `C`, whose `ChainQuorums` members access waits on V, where the
    /// quorum manager's build hook runs.
    fn verified(
        &self,
        t: LlmqType,
        base: &BlockHash,
        members: &[ProTxHash],
        valid: &[bool],
    ) -> Option<Contributions> {
        match self.contributions_of(t, base, members, valid) {
            Ok(Ok(read)) => Some(read),
            Ok(Err(MissingVvec {
                who,
            })) => {
                debug!(target: "llmq", "CDKGSessionManager::GetVerifiedContributions -- this node does not have vvec for llmq={} block={base} protx={who}", t.to_base());
                None
            }
            Err(e) => {
                warn!(target: "llmq", "CDKGSessionManager::GetVerifiedContributions -- {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
