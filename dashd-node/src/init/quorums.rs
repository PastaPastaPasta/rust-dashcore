//! Core's `CQuorumManager` as init builds it (`LLMQContext::qman`) for a node that takes no part
//! in DKG and watches no quorums (an active masternode's DKG is DK1's), its quorum database and
//! its `q-cache` thread.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;

#[cfg(feature = "dkg-runtime")]
use dash_evo::types::ProTxHash;
use dash_kv::{DbSchema, ErrorKind, FlatFs as _, MemoryEngine, StoreError};
use dash_params::ChainSetup;
#[cfg(feature = "dkg-runtime")]
use dash_params::LlmqType;
#[cfg(feature = "dkg-runtime")]
use dash_primitives::BlockHash;
use dashd_chainstate::chain::ChainHandle;
use dashd_chainstate::llmq::quorum::SharedQuorumHook;
use dashd_quorums::signing::SigningQuorums;
use dashd_quorums::{
    ChainQuorums, CleanupCounts, KvQuorumStore, PlatformQuorums, QUORUMDB_SCHEMA, QuorumDbKey,
    QuorumKeyStore, QuorumManager, StoredContributions, WarmExecutor,
};
#[cfg(feature = "dkg-runtime")]
use dashd_quorums::{LocalMasternode, VerifiedContributions, traits::Contributions};
#[cfg(not(feature = "dkg-runtime"))]
use dashd_quorums::{NoDkgSessions, NotMasternode};
use dashd_store::flatfile::OsFs;
use dashd_store::{RocksConfig, RocksEngine};

use super::recsigs::{DEFAULT_MAX_RECOVERED_SIGS_AGE, RecoveredSigsDb};
use crate::chainstate::QmanStart;
use crate::host::trace_thread;

/// rdashd's `CQuorumManager`: the node's contribution store and absent DKG and masternode seams.
/// Its type does not depend on the executor init passes it.
#[cfg(not(feature = "dkg-runtime"))]
pub type NodeQuorumManager =
    QuorumManager<QuorumDb, NoDkgSessions, NotMasternode, Arc<dyn WarmExecutor + Send + Sync>>;

/// rdashd's `CQuorumManager` with the DKG runtime: the node's contribution store and the DKG
/// session manager and masternode role step 7c connects. Its type does not depend on the
/// executor init passes it.
#[cfg(feature = "dkg-runtime")]
pub type NodeQuorumManager =
    QuorumManager<QuorumDb, DkgManagers, DkgManagers, Arc<dyn WarmExecutor + Send + Sync>>;

/// The `m_qdkgsman` and `m_handler` a `NetDKG` connects to the quorum manager at step 7c: the
/// dkgdb's `GetVerifiedContributions` and, on an active masternode, its proTxHash. Unconnected,
/// a build reads the store only, as with `NoDkgSessions` and `NotMasternode`; an observer
/// connects the dkgdb alone, as its role has no proTxHash. Both are read on V, without a lock.
// Core: v24.0.0 src/llmq/quorumsman.h:114-121 ConnectManagers, src/llmq/net_dkg.cpp:475, :496
#[cfg(feature = "dkg-runtime")]
#[derive(Clone, Default)]
pub struct DkgManagers(Arc<ConnectedManagers>);

#[cfg(feature = "dkg-runtime")]
#[derive(Default)]
struct ConnectedManagers {
    qdkgsman: OnceLock<Arc<dyn VerifiedContributions + Send + Sync>>,
    handler: OnceLock<Arc<dyn LocalMasternode + Send + Sync>>,
}

#[cfg(feature = "dkg-runtime")]
impl DkgManagers {
    /// `ConnectManagers`, once: a second connect panics, as Core's asserts abort.
    pub fn connect(
        &self,
        qdkgsman: Arc<dyn VerifiedContributions + Send + Sync>,
        handler: Option<Arc<dyn LocalMasternode + Send + Sync>>,
    ) {
        assert!(self.0.qdkgsman.set(qdkgsman).is_ok(), "the DKG session manager is connected");
        if let Some(handler) = handler {
            assert!(self.0.handler.set(handler).is_ok(), "the masternode role is connected");
        }
    }
}

#[cfg(feature = "dkg-runtime")]
impl VerifiedContributions for DkgManagers {
    // Core: v24.0.0 src/llmq/quorumsman.cpp:120-122 BuildQuorumContributions
    fn verified(
        &self,
        llmq_type: LlmqType,
        base: &BlockHash,
        members: &[ProTxHash],
        valid: &[bool],
    ) -> Option<Contributions> {
        self.0.qdkgsman.get()?.verified(llmq_type, base, members, valid)
    }
}

#[cfg(feature = "dkg-runtime")]
impl LocalMasternode for DkgManagers {
    // Core: v24.0.0 src/llmq/quorumsman.cpp:134 BuildQuorumContributions
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.0.handler.get()?.pro_tx_hash()
    }
}

/// The quorum database's directory in the network data directory, Core's `llmq/quorumdb`. The
/// data directory probe (`datadir::probe_core_data_dir`) takes it for rdashd's when it is empty
/// or holds the store's mark. It is outside the directories a regtest live copy keeps (`blocks`,
/// `chainstate`, `evodb`, `indexes`), so a copy opens a new, empty one.
// Core: v24.0.2 src/llmq/quorumsman.cpp:40
pub const QUORUM_DB_DIR: &str = "llmq/quorumdb";

/// Where a wipe moves the quorum database before deleting it.
pub(crate) const WIPING_DIR: &str = "llmq/quorumdb.wipe";

/// The DKG database's directory in the network data directory, Core's `llmq/dkgdb`.
// Core: v24.0.2 src/llmq/dkgsessionmgr.cpp:35
pub(crate) const DKG_DB_DIR: &str = "llmq/dkgdb";

/// Where a wipe moves the DKG database before deleting it.
pub(crate) const DKG_WIPING_DIR: &str = "llmq/dkgdb.wipe";

/// The LLMQ databases that moved under `llmq/`, checked before any store opens.
const MOVED_LLMQ_DBS: [(&str, &str, &str); 3] = [
    (DKG_DB_DIR, DKG_WIPING_DIR, "DKG database"),
    (QUORUM_DB_DIR, WIPING_DIR, "quorum database"),
    (super::recsigs::RECSIG_DB_DIR, super::recsigs::WIPING_DIR, "recovered signature database"),
];

/// Refuses a network directory that still holds any LLMQ database (`dkgdb`, `quorumdb`,
/// `recsigdb`) or its `.wipe` directory where rdashd kept them before `llmq/`
/// ([`LlmqDbError::OldPath`]); a link or a file there counts too, and nothing is touched.
pub fn refuse_old_llmq_paths(net_dir: &Path) -> Result<(), LlmqDbError> {
    for (dir, wiping, name) in MOVED_LLMQ_DBS {
        LlmqDbFiles::new(dir, wiping, name).refuse_old_paths(net_dir)?;
    }
    Ok(())
}

/// The node's quorum database (`CQuorumManager::db`): a handle the manager and init share, opened
/// by [`open`](Self::open) inside the chainstate load, where Core's `LLMQContext` builds `qman`.
/// Before the open every read and write is an error.
#[derive(Clone)]
pub struct QuorumDb {
    backend: Arc<OnceLock<Backend>>,
    files: LlmqDbFiles,
}

enum Backend {
    Rocks(KvQuorumStore<RocksEngine>),
    Memory(KvQuorumStore<MemoryEngine>),
}

/// A step of an LLMQ database's wipe, for a test's injected failure.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WipeStep {
    Rename,
    Sync,
    Delete,
}

/// Why an LLMQ database ([`QuorumDb`], [`super::recsigs::RecoveredSigsDb`]) did not open:
/// Core's `dbwrapper_error` from its constructor. `db` names the database.
#[derive(Debug)]
pub enum LlmqDbError {
    /// A wipe step failed on `path`.
    Wipe {
        db: &'static str,
        path: PathBuf,
        error: io::Error,
    },
    /// The database was moved to `path` (its `.wipe` directory), but syncing the move failed: its
    /// rows do not survive the next open.
    Sync {
        db: &'static str,
        path: PathBuf,
        error: io::Error,
    },
    /// A wipe found `path` is neither a directory nor a symbolic link, and left it.
    NotADirectory {
        db: &'static str,
        path: PathBuf,
    },
    /// The engine did not open the database at `path`.
    Open {
        db: &'static str,
        path: PathBuf,
        error: StoreError,
    },
    /// The handle was already open.
    AlreadyOpen {
        db: &'static str,
    },
    /// The database, or its wipe, is still at `path`, where rdashd kept it before it moved to
    /// `moved_to`. Nothing was opened, moved or deleted.
    OldPath {
        db: &'static str,
        path: PathBuf,
        moved_to: PathBuf,
    },
}

impl fmt::Display for LlmqDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wipe {
                db,
                path,
                error,
            } => write!(f, "Error wiping the {db} {}: {error}", path.display()),
            Self::Sync {
                db,
                path,
                error,
            } => write!(f, "Error syncing the {db}'s move to {}: {error}", path.display()),
            Self::NotADirectory {
                db,
                path,
            } => write!(f, "Error wiping the {db} {}: not a directory", path.display()),
            Self::Open {
                db,
                path,
                error,
            } => write!(f, "Error opening the {db} {}: {error}", path.display()),
            Self::AlreadyOpen {
                db,
            } => write!(f, "the {db} is already open"),
            Self::OldPath {
                db,
                path,
                moved_to,
            } => write!(
                f,
                "Found the {db} at its old path {}: move it to {} by hand and restart",
                path.display(),
                moved_to.display()
            ),
        }
    }
}

impl std::error::Error for LlmqDbError {}

impl QuorumDb {
    /// A handle whose database is not open yet.
    pub fn unopened() -> Self {
        Self {
            backend: Arc::default(),
            files: LlmqDbFiles::new(QUORUM_DB_DIR, WIPING_DIR, "quorum database"),
        }
    }

    /// An open, empty database in memory, Core's `memory = true` (`dash_dbs_in_memory`, tests
    /// only): [`open`](Self::open) then touches no file, and a wipe has nothing to do.
    // Core: v24.0.0 src/dbwrapper.cpp:139-142
    pub fn in_memory() -> Self {
        let db = Self::unopened();
        let _ = db.backend.set(Backend::Memory(KvQuorumStore::in_memory()));
        db
    }

    /// Core's `CDBWrapper` constructor for the quorum database in `net_dir` (the
    /// recovered-signature database's is the same, in `recsigdb`): with `wipe`, the database is
    /// destroyed without being opened, so a wipe also recovers an unreadable one; then it opens,
    /// created if missing.
    ///
    /// The destroy is a rename to the `.wipe` directory (`quorumdb.wipe`), made durable, then a
    /// delete; a wipe a crash interrupted is finished at the next open, whatever its `wipe`. So a
    /// crash leaves the old database whole or none of it, where Core's `DestroyDB` deletes file by
    /// file. Nothing outside those two directories is touched. If the rename succeeds and its sync
    /// fails, the open fails with [`LlmqDbError::Sync`]: the database is already in the `.wipe`
    /// directory, so its rows do not survive the next open, where a failed rename keeps them.
    ///
    /// Differences from Core's `DestroyDB`, which deletes only LevelDB's own files of the
    /// directory and ignores its failure to remove the directory:
    /// - the database is RocksDB, and the wipe deletes the whole directory with whatever else it
    ///   holds;
    /// - a symbolic link at the directory's path is moved and unlinked and its target left whole,
    ///   where `DestroyDB` deletes LevelDB's files through the link.
    ///
    /// As with Core, a path that is neither a directory nor a link is left as it is and the open
    /// fails ([`LlmqDbError::NotADirectory`]): `DestroyDB` cannot list it and returns, and the
    /// open after it fails.
    // Core: v24.0.0 src/dbwrapper.cpp:130-158, src/dbwrapper.h:762-773;
    // src/leveldb/db/db_impl.cc:1525-1554 DestroyDB
    pub fn open(&self, net_dir: &Path, wipe: bool) -> Result<(), LlmqDbError> {
        match self.backend.get() {
            Some(Backend::Memory(_)) => return Ok(()),
            Some(Backend::Rocks(_)) => return Err(self.files.already_open()),
            None => {}
        }
        let engine = self.files.open(net_dir, wipe, &QUORUMDB_SCHEMA, &engine_config())?;
        self.backend
            .set(Backend::Rocks(KvQuorumStore::new(engine)))
            .map_err(|_| self.files.already_open())
    }

    /// Makes `step` of every later wipe fail.
    #[cfg(test)]
    pub(crate) fn fail_wipe_at(&self, step: WipeStep) {
        self.files.fail_wipe_at(step);
    }

    /// Directories synced by this handle's opens and wipes, in order.
    #[cfg(test)]
    pub(crate) fn synced_dirs(&self) -> Vec<PathBuf> {
        self.files.synced_dirs()
    }

    fn backend(&self) -> Result<&Backend, StoreError> {
        self.backend.get().ok_or_else(|| self.files.not_open())
    }

    /// A handle that does not keep the database open.
    pub fn downgrade(&self) -> WeakQuorumDb {
        WeakQuorumDb {
            backend: Arc::downgrade(&self.backend),
            files: self.files.clone(),
        }
    }
}

/// `NetQuorum`'s handle on the quorum database (Core's `m_qman` reference): the manager's
/// handles keep it open, so it closes at `node.llmq_ctx.reset()` whatever handler clones live.
/// Once closed, each call is the unopened database's error.
// Core: v24.0.0 src/init.cpp:365, :462
#[derive(Clone)]
pub struct WeakQuorumDb {
    backend: Weak<OnceLock<Backend>>,
    files: LlmqDbFiles,
}

impl WeakQuorumDb {
    fn upgrade(&self) -> Result<QuorumDb, StoreError> {
        let backend = self.backend.upgrade().ok_or_else(|| self.files.not_open())?;
        Ok(QuorumDb {
            backend,
            files: self.files.clone(),
        })
    }
}

impl QuorumKeyStore for WeakQuorumDb {
    type Error = StoreError;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, StoreError> {
        self.upgrade()?.read(key)
    }

    fn write(&self, key: &QuorumDbKey, c: &StoredContributions) -> Result<(), StoreError> {
        self.upgrade()?.write(key, c)
    }

    fn erase_except(&self, keep: &BTreeSet<QuorumDbKey>) -> Result<CleanupCounts, StoreError> {
        self.upgrade()?.erase_except(keep)
    }
}

/// One of the node's LLMQ databases on disk: its directory in the network data directory, the
/// directory a wipe moves it to, and its name in messages.
#[derive(Clone)]
pub(crate) struct LlmqDbFiles {
    dir: &'static str,
    wiping: &'static str,
    name: &'static str,
    #[cfg(test)]
    fault: Arc<Mutex<Option<WipeStep>>>,
    #[cfg(test)]
    synced: Arc<Mutex<Vec<PathBuf>>>,
}

impl LlmqDbFiles {
    pub(crate) fn new(dir: &'static str, wiping: &'static str, name: &'static str) -> Self {
        Self {
            dir,
            wiping,
            name,
            #[cfg(test)]
            fault: Arc::default(),
            #[cfg(test)]
            synced: Arc::default(),
        }
    }

    /// Refuses a network directory that still holds this database, or its `.wipe` directory, where
    /// rdashd kept them before `llmq/` ([`LlmqDbError::OldPath`]); a link or a file there counts
    /// too, and nothing is touched.
    pub(crate) fn refuse_old_paths(&self, net_dir: &Path) -> Result<(), LlmqDbError> {
        let (db, path, wiping) = (self.name, net_dir.join(self.dir), net_dir.join(self.wiping));
        for moved in [&path, &wiping] {
            let Some(name) = moved.file_name() else {
                continue;
            };
            let old = net_dir.join(name);
            if old == *moved {
                continue;
            }
            match std::fs::symlink_metadata(&old) {
                Ok(_) => {
                    return Err(LlmqDbError::OldPath {
                        db,
                        path: old,
                        moved_to: moved.clone(),
                    });
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) => {}
                Err(e) => return Err(io_failure(db, old, e)),
            }
        }
        Ok(())
    }

    /// [`QuorumDb::open`]'s open and wipe for this database's directory, with its `schema` and
    /// engine `config`.
    // Core: v24.0.0 src/dbwrapper.cpp:130-158, src/dbwrapper.h:762-773;
    // src/leveldb/db/db_impl.cc:1525-1554 DestroyDB
    ///
    /// Before anything else, the open refuses a network directory that still holds the database,
    /// or its `.wipe` directory, where rdashd kept them before `llmq/`
    /// ([`LlmqDbError::OldPath`]); a link or a file there counts too, and nothing is touched.
    /// `llmq/` is created if missing and its entry in `net_dir` is synced on every open.
    pub(crate) fn open(
        &self,
        net_dir: &Path,
        wipe: bool,
        schema: &'static DbSchema,
        config: &RocksConfig,
    ) -> Result<RocksEngine, LlmqDbError> {
        self.refuse_old_paths(net_dir)?;
        let (db, path, wiping) = (self.name, net_dir.join(self.dir), net_dir.join(self.wiping));
        let failed = |path: &Path| {
            let path = path.to_path_buf();
            move |error| LlmqDbError::Wipe {
                db,
                path,
                error,
            }
        };
        let parent = path.parent().unwrap_or(net_dir);
        if parent != net_dir {
            self.create_durably(parent).map_err(|e| io_failure(db, parent.to_path_buf(), e))?;
        }
        if exists(&wiping).map_err(failed(&wiping))? {
            self.remove(&wiping).map_err(failed(&wiping))?;
        }
        if wipe && exists(&path).map_err(failed(&path))? {
            if !wipeable(&path).map_err(failed(&path))? {
                return Err(LlmqDbError::NotADirectory {
                    db,
                    path,
                });
            }
            self.rename(&path, &wiping).map_err(failed(&path))?;
            self.sync(parent).map_err(|error| LlmqDbError::Sync {
                db,
                path: wiping.clone(),
                error,
            })?;
            self.remove(&wiping).map_err(failed(&wiping))?;
        }
        let (engine, _) =
            RocksEngine::open(&path, schema, config).map_err(|error| LlmqDbError::Open {
                db,
                path: path.clone(),
                error,
            })?;
        Ok(engine)
    }

    /// Creates the directory `dir` (`llmq/`) if missing and syncs its entry in its parent, so the
    /// databases made in it survive a crash even when another opener created `dir` first.
    fn create_durably(&self, dir: &Path) -> io::Result<()> {
        create_dir_durably_with(dir, |parent| self.sync_dir(parent))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(test)]
        self.injected(WipeStep::Rename)?;
        OsFs::new().rename(from, to)
    }

    fn sync(&self, dir: &Path) -> io::Result<()> {
        #[cfg(test)]
        self.injected(WipeStep::Sync)?;
        self.sync_dir(dir)
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        #[cfg(test)]
        self.synced.lock().unwrap_or_else(PoisonError::into_inner).push(dir.to_path_buf());
        sync_dir_os(dir)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        #[cfg(test)]
        self.injected(WipeStep::Delete)?;
        if std::fs::symlink_metadata(path)?.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        }
    }

    #[cfg(test)]
    fn injected(&self, step: WipeStep) -> io::Result<()> {
        match *self.fault.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(fault) if fault == step => {
                Err(io::Error::other(format!("injected {} {step:?} failure", self.name)))
            }
            _ => Ok(()),
        }
    }

    /// Makes `step` of every later wipe fail.
    #[cfg(test)]
    pub(crate) fn fail_wipe_at(&self, step: WipeStep) {
        *self.fault.lock().unwrap_or_else(PoisonError::into_inner) = Some(step);
    }

    /// Directories synced by this handle's opens and wipes, in order.
    #[cfg(test)]
    pub(crate) fn synced_dirs(&self) -> Vec<PathBuf> {
        self.synced.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub(crate) fn already_open(&self) -> LlmqDbError {
        LlmqDbError::AlreadyOpen {
            db: self.name,
        }
    }

    pub(crate) fn not_open(&self) -> StoreError {
        StoreError::engine(self.dir, ErrorKind::Other, format!("the {} is not open", self.name))
    }
}

/// Creates the directory `dir` (`llmq/`) if missing and syncs its entry in its parent, so the
/// databases made in it survive a crash even when another opener created `dir` first.
pub(crate) fn create_dir_durably(dir: &Path) -> io::Result<()> {
    create_dir_durably_with(dir, sync_dir_os)
}

fn create_dir_durably_with(
    dir: &Path,
    sync_dir: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    match dir.parent() {
        Some(parent) if parent.as_os_str().is_empty() => sync_dir(Path::new(".")),
        Some(parent) => sync_dir(parent),
        None => Ok(()),
    }
}

fn sync_dir_os(dir: &Path) -> io::Result<()> {
    #[cfg(test)]
    record_synced_dir(dir);
    OsFs::new().sync_dir(dir)
}

#[cfg(test)]
thread_local! {
    static SYNCED_DIRS: std::cell::RefCell<Option<Vec<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn record_synced_dir(dir: &Path) {
    SYNCED_DIRS.with(|slot| {
        if let Some(dirs) = slot.borrow_mut().as_mut() {
            dirs.push(dir.to_path_buf());
        }
    });
}

/// Runs `f` and returns its result together with every directory synced through `sync_dir_os` on
/// this thread.
#[cfg(test)]
pub(crate) fn capture_synced_dirs<T>(f: impl FnOnce() -> T) -> (T, Vec<PathBuf>) {
    struct Reset(Option<Vec<PathBuf>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            SYNCED_DIRS.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let prev = SYNCED_DIRS.with(|slot| slot.borrow_mut().replace(Vec::new()));
    let _reset = Reset(prev);
    let result = f();
    let synced = SYNCED_DIRS.with(|slot| slot.borrow_mut().take()).unwrap_or_default();
    (result, synced)
}

fn io_failure(db: &'static str, path: PathBuf, e: io::Error) -> LlmqDbError {
    LlmqDbError::Open {
        db,
        path,
        error: StoreError::engine("filesystem", ErrorKind::Io, e.to_string()),
    }
}

/// Whether a wipe moves `path`: a directory or a symbolic link, not followed.
fn wipeable(path: &Path) -> io::Result<bool> {
    let meta = std::fs::symlink_metadata(path)?;
    Ok(meta.is_dir() || meta.file_type().is_symlink())
}

fn exists(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Core's `GetOptions(1 << 20)` for the quorum database: half of the 1 MiB budget for the block
/// cache, a quarter per write buffer with two of them, 10-bit bloom filters; one background job.
// Core: v24.0.0 src/dbwrapper.cpp:113-119, src/llmq/quorumsman.cpp:40
pub(crate) fn engine_config() -> RocksConfig {
    RocksConfig {
        block_cache_bytes: 1 << 19,
        write_buffer_bytes: 1 << 18,
        max_write_buffers: 2,
        total_write_buffer_bytes: 1 << 19,
        max_background_jobs: 1,
        bloom_bits_per_key: 10.0,
        ..RocksConfig::default()
    }
}

/// Each call is the open database's; before the open, an error, which a build maps to its read
/// error. An engine failure is the call's error and is not the node's fail-stop: Core throws
/// `dbwrapper_error` per call and latches nothing, while RocksDB here refuses writes after a
/// failed one until the node restarts.
impl QuorumKeyStore for QuorumDb {
    type Error = StoreError;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, StoreError> {
        match self.backend()? {
            Backend::Rocks(store) => store.read(key),
            Backend::Memory(store) => store.read(key),
        }
    }

    fn write(&self, key: &QuorumDbKey, c: &StoredContributions) -> Result<(), StoreError> {
        match self.backend()? {
            Backend::Rocks(store) => store.write(key, c),
            Backend::Memory(store) => store.write(key, c),
        }
    }

    fn erase_except(&self, keep: &BTreeSet<QuorumDbKey>) -> Result<CleanupCounts, StoreError> {
        match self.backend()? {
            Backend::Rocks(store) => store.erase_except(keep),
            Backend::Memory(store) => store.erase_except(keep),
        }
    }
}

/// What init keeps of the quorum manager: the manager, its Platform lookup, the chain seams
/// [`bind_chain`](Self::bind_chain) binds once V runs, and, from [`init_node_quorums`], its
/// `q-cache` thread. Dropping it interrupts and joins that thread, as `~CQuorumManager` does,
/// whatever other handles to the manager live.
pub struct NodeQuorums {
    pub manager: Arc<NodeQuorumManager>,
    pub platform: Arc<dyn PlatformQuorums + Send + Sync>,
    /// The signing manager's database (`CSigningManager::db`), opened after the quorum database;
    /// from [`init_quorums`], an unopened one.
    pub recsigs: RecoveredSigsDb,
    /// `-maxrecsigsage`, Core's `max_recsigs_age` for the signing manager; from [`init_quorums`],
    /// the default.
    pub max_recsigs_age: i64,
    store: QuorumDb,
    chain: Arc<ChainQuorums>,
    qcache: Option<Arc<QCacheThread>>,
    #[cfg(feature = "dkg-runtime")]
    dkg: DkgManagers,
}

impl NodeQuorums {
    /// The manager's quorum database.
    pub fn store(&self) -> &QuorumDb {
        &self.store
    }

    /// Binds the manager's lookup, cleanup scan and share-seam view to V; before it each lookup
    /// is an error and each cleanup erases nothing. A second bind is ignored.
    pub fn bind_chain(&self, chain: &ChainHandle) {
        self.chain.bind(chain.clone());
    }

    /// What step 7c's DKG connects to the manager.
    #[cfg(feature = "dkg-runtime")]
    pub fn dkg_managers(&self) -> &DkgManagers {
        &self.dkg
    }

    /// The manager's chain seams, which `NetQuorum`'s quorum reads share.
    pub fn chain_quorums(&self) -> &Arc<ChainQuorums> {
        &self.chain
    }

    /// The signing manager's quorum lookup, over the manager's route-B seams.
    pub fn signing_quorums(&self, setup: Arc<ChainSetup>) -> SigningQuorums {
        SigningQuorums::new(Arc::clone(&self.chain), setup)
    }

    /// The `q-cache` thread from [`init_node_quorums`], the manager's warm executor, which
    /// `NetQuorum` also gets as `m_qman`'s.
    pub(crate) fn qcache(&self) -> &Arc<QCacheThread> {
        self.qcache.as_ref().expect("init_node_quorums made it")
    }
}

impl Drop for NodeQuorums {
    // Core: v24.0.0 src/init.cpp:462 node.llmq_ctx.reset(), src/llmq/quorumsman.cpp:50-56
    fn drop(&mut self) {
        if let Some(thread) = &self.qcache {
            thread.interrupt_and_join();
        }
    }
}

/// [`init_quorums`] over an unopened [`QuorumDb`] in `net_dir`, with an unstarted [`QCacheThread`]
/// as the executor, kept in the result, and an unopened [`RecoveredSigsDb`] with
/// `max_recsigs_age`. The rest of Core's `CQuorumManager` and `CSigningManager` constructors goes
/// into `qman`, which the load runs after the BLS check threads start (Core's `LLMQContext`
/// builds `qman` after its block processor, then `sigman`): the quorum database opens, wiped if
/// the load says so, the thread starts, then the recovered-signature database opens with the same
/// wipe. Dropping the result joins the thread and closes both databases once no other handle
/// holds them.
// Core: v24.0.2 src/llmq/context.cpp:14-27, src/llmq/quorumsman.cpp:31-46,
// src/llmq/signing.cpp:24-27, :385-391
pub fn init_node_quorums(
    chain: &Arc<ChainSetup>,
    net_dir: &Path,
    max_recsigs_age: i64,
    hook: &mut Option<SharedQuorumHook>,
    qman: &mut Option<QmanStart>,
) -> NodeQuorums {
    let thread = QCacheThread::new();
    let store = QuorumDb::unopened();
    let mut quorums = init_quorums(chain, store.clone(), thread.clone(), hook);
    quorums.max_recsigs_age = max_recsigs_age;
    let recsigs = quorums.recsigs.clone();
    let (starter, net_dir) = (Arc::clone(&thread), net_dir.to_path_buf());
    *qman = Some(Box::new(move |wipe| {
        store.open(&net_dir, wipe).map_err(|e| e.to_string())?;
        // Core's `MigrateOldQuorumDB(evoDb)` is not ported: it moves pre-v20 rows from evoDb,
        // and rdashd refuses a Core data directory (`datadir::refuse_core_dir`), so no Core
        // evoDb rows exist to move (D-row).
        // Core: v24.0.0 src/llmq/quorumsman.cpp:44, :678-741
        starter.start().map_err(|e| e.to_string())?;
        // Core: v24.0.2 src/llmq/context.cpp:23
        recsigs.open(&net_dir, wipe).map_err(|e| e.to_string())
    }));
    quorums.qcache = Some(thread);
    quorums
}

/// Core's `LLMQContext::qman`, built inside `LoadChainstate` before the block index loads and
/// before VerifyDB, with Core's null `m_qdkgsman` and `m_handler`: each build reads the store and
/// never builds or writes contributions. The build hook goes into `hook`, which the coins' attach
/// installs on the members facade, so every route's builds (VerifyDB's included) run it.
///
/// `store` is the manager's database, kept in the result too; `executor` is the `q-cache`
/// thread's stand-in; the caller owns it, and nothing here stops it. It must not run jobs on V's
/// thread: a job that reads V through the chain seams would deadlock.
///
/// rdashd builds the manager just before the chainstate opens, where Core builds it after the
/// block tree database opens (node/chainstate.cpp:153-161), and opens its database and starts its
/// thread after the open and the BLS check threads, so a load that fails before them opens and
/// starts nothing, as Core's does not. No build reads the database or queues a job in between:
/// the members facade is attached later.
// Core: v24.0.0 src/node/chainstate.cpp:157-161, src/llmq/context.cpp:20-21,
// src/llmq/quorumsman.cpp:31-46 CQuorumManager::CQuorumManager
pub fn init_quorums(
    chain: &Arc<ChainSetup>,
    store: QuorumDb,
    executor: Arc<dyn WarmExecutor + Send + Sync>,
    hook: &mut Option<SharedQuorumHook>,
) -> NodeQuorums {
    let seams = Arc::new(ChainQuorums::new());
    #[cfg(feature = "dkg-runtime")]
    let dkg = DkgManagers::default();
    #[cfg(feature = "dkg-runtime")]
    let (qdkgsman, handler) = (dkg.clone(), dkg.clone());
    #[cfg(not(feature = "dkg-runtime"))]
    let (qdkgsman, handler) = (NoDkgSessions, NotMasternode);
    let manager = QuorumManager::new(Arc::clone(chain), store.clone(), qdkgsman, handler, executor)
        .with_quorum_lookup(seams.clone())
        .with_cleanup_facade(seams.clone());
    #[cfg(feature = "test-hooks")]
    let manager = manager.with_share_seam_view(seams.clone());
    let manager = Arc::new(manager);
    *hook = Some(manager.clone());
    NodeQuorums {
        platform: manager.clone(),
        manager,
        recsigs: RecoveredSigsDb::unopened(),
        max_recsigs_age: DEFAULT_MAX_RECOVERED_SIGS_AGE,
        store,
        chain: seams,
        qcache: None,
        #[cfg(feature = "dkg-runtime")]
        dkg,
    }
}

type Job = Box<dyn FnOnce() + Send>;

/// Core's `q-cache` thread: one `TraceThread` running the warming jobs in the order they come.
/// [`new`](Self::new) makes it unstarted, with jobs waiting in its queue;
/// [`start`](Self::start) spawns the thread; [`interrupt_and_join`](Self::interrupt_and_join) is
/// `~CQuorumManager`'s stop: queued jobs never run and the thread is joined. A running job is
/// not interrupted, where Core's warm loop checks the interrupt before each member
/// (quorumsman.cpp:654-660); no warm is queued until quorums have contributions (DK1). Dropping
/// the last handle stops it too.
// Core: v24.0.0 src/llmq/quorumsman.cpp:45-56, :632-668 CacheWarmingThreadMain
pub struct QCacheThread {
    state: Mutex<QCacheState>,
    interrupted: Arc<AtomicBool>,
    #[cfg(test)]
    fail_start: AtomicBool,
}

struct QCacheState {
    jobs: Option<Sender<Job>>,
    queue: Option<Receiver<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl QCacheThread {
    /// An unstarted thread.
    pub fn new() -> Arc<Self> {
        let (jobs, queue) = channel();
        Arc::new(Self {
            state: Mutex::new(QCacheState {
                jobs: Some(jobs),
                queue: Some(queue),
                thread: None,
            }),
            interrupted: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_start: AtomicBool::new(false),
        })
    }

    /// Spawns the thread; after a start or a stop it does nothing.
    pub fn start(&self) -> std::io::Result<()> {
        let mut state = self.lock();
        let Some(queue) = state.queue.take() else {
            return Ok(());
        };
        #[cfg(test)]
        if self.fail_start.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("injected q-cache spawn failure"));
        }
        let interrupted = Arc::clone(&self.interrupted);
        state.thread = Some(trace_thread("q-cache", move || {
            for job in queue {
                if interrupted.load(Ordering::Acquire) {
                    break;
                }
                job();
            }
        })?);
        Ok(())
    }

    /// Core's `m_cache_interrupt(); m_cache_thread.join();`. On `d-q-cache` itself, which a job
    /// dropping the last handle can be, it interrupts without joining.
    pub fn interrupt_and_join(&self) {
        self.interrupted.store(true, Ordering::Release);
        let (jobs, queue, thread) = {
            let mut state = self.lock();
            let on_thread = state
                .thread
                .as_ref()
                .is_some_and(|thread| thread.thread().id() == std::thread::current().id());
            (state.jobs.take(), state.queue.take(), state.thread.take().filter(|_| !on_thread))
        };
        drop((jobs, queue));
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }

    fn lock(&self) -> MutexGuard<'_, QCacheState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_start(&self) {
        self.fail_start.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn is_started(&self) -> bool {
        self.lock().thread.is_some()
    }
}

impl Drop for QCacheThread {
    fn drop(&mut self) {
        self.interrupt_and_join();
    }
}

impl WarmExecutor for QCacheThread {
    // Core: v24.0.0 src/llmq/quorumsman.cpp:670-676 QueueQuorumForWarming
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
        let jobs = self.lock().jobs.clone();
        if let Some(jobs) = jobs {
            let _ = jobs.send(job);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::time::Duration;

    use dash_params::LlmqType;
    use dash_primitives::QuorumHash;

    use super::*;

    /// Waits for a job on `d-q-cache` to start, and holds it there until the returned sender
    /// sends or drops; the job then sets the flag 100 ms later, so a join that returns before
    /// the flag is set did not wait for the job.
    fn hold(thread: &QCacheThread) -> (Sender<()>, Arc<AtomicBool>) {
        let (entered, started) = channel();
        let (release, gate) = channel::<()>();
        let finished = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&finished);
        thread.spawn_warm(Box::new(move || {
            entered.send(()).unwrap();
            let _ = gate.recv();
            std::thread::sleep(Duration::from_millis(100));
            done.store(true, SeqCst);
        }));
        started.recv_timeout(Duration::from_secs(30)).expect("the job runs");
        (release, finished)
    }

    /// Jobs wait until `start`, then run on `d-q-cache` in order; a second start does nothing.
    #[test]
    fn qcache_thread_runs_jobs_on_its_thread_once_started() {
        let thread = QCacheThread::new();
        let (tx, rx) = channel();
        for n in 0..3 {
            let tx = tx.clone();
            thread.spawn_warm(Box::new(move || {
                tx.send((n, std::thread::current().name().map(str::to_owned))).unwrap();
            }));
        }
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err(), "nothing runs unstarted");
        assert!(!thread.is_started());
        thread.start().unwrap();
        thread.start().unwrap();
        for n in 0..3 {
            let got = rx.recv_timeout(Duration::from_secs(30)).unwrap();
            assert_eq!(got, (n, Some("d-q-cache".to_owned())));
        }
    }

    /// `~CQuorumManager`'s stop: the running job finishes, queued and later jobs never run, and
    /// the thread is joined while other handles live.
    #[test]
    fn interrupt_and_join_finishes_the_running_job_and_drops_the_queue() {
        let thread = QCacheThread::new();
        thread.start().unwrap();
        let ran = Arc::new(AtomicUsize::new(0));
        let (release, finished) = hold(&thread);
        let queued = Arc::clone(&ran);
        thread.spawn_warm(Box::new(move || {
            queued.fetch_add(1, SeqCst);
        }));
        let kept = Arc::clone(&thread);
        let stopper = std::thread::spawn(move || kept.interrupt_and_join());
        while !thread.interrupted.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(!stopper.is_finished(), "the join waits for the running job");
        drop(release);
        stopper.join().unwrap();
        assert!(finished.load(SeqCst), "the join waited for the running job");
        assert!(!thread.is_started(), "joined");
        let later = Arc::clone(&ran);
        thread.spawn_warm(Box::new(move || {
            later.fetch_add(1, SeqCst);
        }));
        thread.start().unwrap();
        assert_eq!(ran.load(SeqCst), 0);
        assert!(!thread.is_started(), "a stopped thread does not restart");
    }

    /// Dropping `NodeQuorums` joins its thread although the manager, which holds the executor,
    /// outlives it; a job dropping the last handle on `d-q-cache` does not join itself.
    #[test]
    fn dropping_node_quorums_joins_and_a_self_drop_does_not_deadlock() {
        let chain = crate::chain::tests::regtest();
        let dir = tempfile::tempdir().unwrap();
        let (mut hook, mut qman) = (None, None);
        let quorums = init_node_quorums(&chain, dir.path(), 60, &mut hook, &mut qman);
        assert!(!quorums.qcache().is_started(), "init_node_quorums starts nothing");
        qman.take().expect("the construction goes into the slot")(false).unwrap();
        let thread = Arc::clone(quorums.qcache());
        assert!(thread.is_started());
        let manager = Arc::clone(&quorums.manager);
        let (release, finished) = hold(&thread);
        let dropper = std::thread::spawn(move || drop(quorums));
        while !thread.interrupted.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(!dropper.is_finished(), "the drop waits for the running job");
        drop(release);
        dropper.join().unwrap();
        assert!(finished.load(SeqCst), "the drop waited for the running job");
        assert!(!thread.is_started(), "joined while the manager lives");
        drop((manager, hook, thread));

        let thread = QCacheThread::new();
        thread.start().unwrap();
        let (done, finished) = channel();
        let last = Mutex::new(Some(Arc::clone(&thread)));
        let (release, _) = hold(&thread);
        thread.spawn_warm(Box::new(move || {
            drop(last.lock().unwrap().take());
            done.send(()).unwrap();
        }));
        drop(thread);
        drop(release);
        finished.recv_timeout(Duration::from_secs(30)).expect("d-q-cache does not join itself");
    }

    fn key(n: u8) -> QuorumDbKey {
        QuorumDbKey([n; 32])
    }

    /// Stored contributions with `n` placeholder vector keys and a share.
    pub(crate) fn contributions(n: u8) -> StoredContributions {
        use dash_pkc::bls::BlsSchemeKind;
        use dash_pkc::bls::core::{BlsPkValue, BlsSkValue};
        let mut share = [0; 32];
        share[31] = n;
        StoredContributions {
            vvec: Ok(Some(vec![BlsPkValue::invalid(); usize::from(n)])),
            sk_share: BlsSkValue::from_canonical(&share, BlsSchemeKind::Basic),
        }
    }

    /// A read as the vector's length and the share's last byte.
    pub(crate) fn summary(read: Option<StoredContributions>) -> Option<(usize, Option<u8>)> {
        read.map(|c| {
            let share = c.sk_share.map(|sk| sk.to_bytes(dash_pkc::bls::BlsSchemeKind::Basic)[31]);
            (c.vvec.expect("decodes").expect("present").len(), share)
        })
    }

    fn opened(net: &Path, wipe: bool) -> QuorumDb {
        let db = QuorumDb::unopened();
        db.open(net, wipe).unwrap();
        db
    }

    /// Every path under `dir` but the quorum database's two directories, with a file's bytes.
    fn outside_the_store(dir: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut out = std::collections::BTreeMap::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let path = e.unwrap().path();
                if [QUORUM_DB_DIR, WIPING_DIR].map(|p| dir.join(p)).contains(&path) {
                    continue;
                }
                if path.is_dir() {
                    todo.push(path.clone());
                    out.insert(path, None);
                } else {
                    out.insert(path.clone(), Some(std::fs::read(&path).unwrap()));
                }
            }
        }
        out
    }

    /// T7: before the open each call is an error, not a panic, which a build maps to its read
    /// error; a clone shares the open, and a second open is refused.
    #[test]
    fn an_unopened_handle_errors_and_never_panics() {
        let db = QuorumDb::unopened();
        assert!(db.read(&key(1)).is_err());
        assert!(db.write(&key(1), &contributions(1)).is_err());
        assert!(db.erase_except(&BTreeSet::new()).is_err());
        let kept = db.clone();
        let dir = tempfile::tempdir().unwrap();
        db.open(dir.path(), false).unwrap();
        kept.write(&key(1), &contributions(1)).unwrap();
        assert_eq!(summary(db.read(&key(1)).unwrap()), Some((1, Some(1))));
        assert!(matches!(db.open(dir.path(), false), Err(LlmqDbError::AlreadyOpen { .. })));
    }

    /// T8: rows survive a reopen without a wipe; a wipe empties the database, and a later open
    /// without one finds it empty.
    // Core: v24.0.0 src/dbwrapper.cpp:143-158
    #[test]
    fn rows_survive_a_reopen_without_wipe() {
        let dir = tempfile::tempdir().unwrap();
        let db = opened(dir.path(), false);
        db.write(&key(1), &contributions(1)).unwrap();
        db.write(&key(2), &contributions(2)).unwrap();
        drop(db);
        let db = opened(dir.path(), false);
        assert_eq!(summary(db.read(&key(1)).unwrap()), Some((1, Some(1))));
        assert_eq!(summary(db.read(&key(2)).unwrap()), Some((2, Some(2))));
        drop(db);
        let db = opened(dir.path(), true);
        assert!(db.read(&key(1)).unwrap().is_none());
        drop(db);
        let db = opened(dir.path(), false);
        assert!(db.read(&key(2)).unwrap().is_none());
    }

    /// T9: a wipe destroys the database directory without opening it, so it recovers one that
    /// does not open, as Core's `DestroyDB` recovers a LevelDB one (rdashd deletes the whole
    /// directory: see [`QuorumDb::open`]); without a wipe such a database is the open's error. A
    /// non-directory is the open's error with or without a wipe, and a wipe leaves it, as Core's
    /// `DestroyDB` does. A wipe a crash left in `quorumdb.wipe` is finished by any open, and the
    /// live database is kept. A failed rename keeps the database; a failed sync or delete leaves
    /// the moved copy for the next open.
    // Core: v24.0.0 src/dbwrapper.cpp:143-158, src/leveldb/db/db_impl.cc:1525-1532
    #[test]
    fn wipe_destroys_even_an_unreadable_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(QUORUM_DB_DIR);
        std::fs::create_dir(dir.path().join("llmq")).unwrap();
        std::fs::write(&path, "not a database").unwrap();
        let open = QuorumDb::unopened().open(dir.path(), false);
        assert!(matches!(open, Err(LlmqDbError::Open { .. })), "{open:?}");
        let open = QuorumDb::unopened().open(dir.path(), true);
        assert!(
            matches!(&open, Err(LlmqDbError::NotADirectory { path: p, .. }) if *p == path),
            "{open:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"not a database");
        assert!(!dir.path().join(WIPING_DIR).exists());
        std::fs::remove_file(&path).unwrap();

        let db = opened(dir.path(), false);
        db.write(&key(1), &contributions(1)).unwrap();
        drop(db);
        std::fs::write(path.join("CURRENT"), "").unwrap();
        assert!(QuorumDb::unopened().open(dir.path(), false).is_err(), "a truncated CURRENT");
        assert!(opened(dir.path(), true).read(&key(1)).unwrap().is_none());

        let db = opened(dir.path(), false);
        db.write(&key(1), &contributions(1)).unwrap();
        drop(db);
        let leftover = dir.path().join(WIPING_DIR);
        std::fs::create_dir_all(leftover.join("sub")).unwrap();
        std::fs::write(leftover.join("sub").join("000004.log"), "old").unwrap();
        for file in ["CURRENT", "MANIFEST-000001"] {
            std::fs::write(leftover.join(file), "").unwrap();
        }
        assert!(crate::datadir::probe_core_data_dir(dir.path()).is_none(), "an unmarked leftover");
        let db = opened(dir.path(), false);
        assert!(!leftover.exists(), "the interrupted wipe is finished");
        assert_eq!(summary(db.read(&key(1)).unwrap()), Some((1, Some(1))));
        drop(db);

        let db = QuorumDb::unopened();
        db.fail_wipe_at(WipeStep::Rename);
        let open = db.open(dir.path(), true);
        assert!(matches!(&open, Err(LlmqDbError::Wipe { path: p, .. }) if *p == path), "{open:?}");
        assert!(!leftover.exists());
        assert!(db.read(&key(1)).is_err(), "not open");
        for step in [WipeStep::Sync, WipeStep::Delete] {
            let db = opened(dir.path(), false);
            db.write(&key(1), &contributions(1)).unwrap();
            drop(db);
            let db = QuorumDb::unopened();
            db.fail_wipe_at(step);
            let open = db.open(dir.path(), true);
            let failed_at = match &open {
                Err(LlmqDbError::Sync {
                    path: p,
                    ..
                }) if *p == leftover => WipeStep::Sync,
                Err(LlmqDbError::Wipe {
                    path: p,
                    ..
                }) if *p == leftover => WipeStep::Delete,
                _ => panic!("{step:?}: {open:?}"),
            };
            assert_eq!(failed_at, step);
            assert!(!path.exists() && leftover.exists(), "{step:?}: moved, not deleted");
            assert!(crate::datadir::probe_core_data_dir(dir.path()).is_none(), "{step:?}");
            let db = opened(dir.path(), false);
            assert!(!leftover.exists(), "{step:?}: finished by the next open");
            assert!(db.read(&key(1)).unwrap().is_none(), "{step:?}");
        }
    }

    /// A symbolic link at `quorumdb` is moved and unlinked by a wipe, its target left whole, where
    /// Core's `DestroyDB` deletes LevelDB's files through it (declared in [`QuorumDb::open`]).
    #[cfg(unix)]
    #[test]
    fn wipe_unlinks_a_symlinked_store_and_keeps_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        opened(&target, false).write(&key(1), &contributions(1)).unwrap();
        let net = dir.path().join("net");
        std::fs::create_dir_all(net.join("llmq")).unwrap();
        std::os::unix::fs::symlink(target.join(QUORUM_DB_DIR), net.join(QUORUM_DB_DIR)).unwrap();
        let before = outside_the_store(&target.join(QUORUM_DB_DIR));
        assert!(!before.is_empty());
        assert!(opened(&net, true).read(&key(1)).unwrap().is_none());
        assert!(net.join(QUORUM_DB_DIR).is_dir() && !net.join(WIPING_DIR).exists());
        assert!(!std::fs::symlink_metadata(net.join(QUORUM_DB_DIR)).unwrap().is_symlink());
        assert_eq!(outside_the_store(&target.join(QUORUM_DB_DIR)), before);
        assert!(target.join(QUORUM_DB_DIR).join("CURRENT").is_file());
        assert_eq!(summary(opened(&target, false).read(&key(1)).unwrap()), Some((1, Some(1))));
    }

    /// T10: the database's directory is Core's `llmq/quorumdb`, which the open makes with its
    /// `llmq/` parent, and is not one rdashd takes for Dash Core's.
    // Core: v24.0.2 src/llmq/quorumsman.cpp:40
    #[test]
    fn the_quorum_store_is_not_a_core_sign() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join("llmq").exists());
        opened(dir.path(), false).write(&key(1), &contributions(1)).unwrap();
        assert_eq!(QUORUM_DB_DIR, "llmq/quorumdb");
        assert!(dir.path().join("llmq/quorumdb/CURRENT").is_file());
        assert!(crate::datadir::probe_core_data_dir(dir.path()).is_none());
    }

    /// The LLMQ databases' opens, by name: the quorum, recovered-signature and DKG databases.
    fn llmq_opens() -> [(&'static str, &'static str, LlmqOpen); 3] {
        use super::super::dkg::{DKGDB_DIR, DKGDB_WIPING_DIR, dkgdb_files};
        use super::super::recsigs::{RECSIG_DB_DIR, RecoveredSigsDb, WIPING_DIR as RECSIG_WIPE};
        [
            (QUORUM_DB_DIR, WIPING_DIR, |net, wipe| QuorumDb::unopened().open(net, wipe)),
            (RECSIG_DB_DIR, RECSIG_WIPE, |net, wipe| RecoveredSigsDb::unopened().open(net, wipe)),
            (DKGDB_DIR, DKGDB_WIPING_DIR, |net, wipe| {
                let schema = &dashd_quorums::DKGDB_SCHEMA;
                dkgdb_files().open(net, wipe, schema, &engine_config()).map(drop)
            }),
        ]
    }

    type LlmqOpen = fn(&Path, bool) -> Result<(), LlmqDbError>;

    /// A network directory still holding `<name>` or `<name>.wipe` where rdashd kept an LLMQ
    /// database before `llmq/` is refused before the open, with or without a wipe, naming the path
    /// and where it moved; it is neither opened, moved nor deleted, whether a directory, a file or
    /// a link.
    #[cfg(unix)]
    #[test]
    fn the_old_llmq_database_paths_are_refused_and_left() {
        for (dir_name, wiping, open) in llmq_opens() {
            for new in [dir_name, wiping] {
                let old = new.strip_prefix("llmq/").unwrap();
                let dir = tempfile::tempdir().unwrap();
                let net = dir.path();
                let target = net.join("elsewhere");
                open(&target, false).unwrap();
                let kept = target.join(dir_name);
                for shape in ["directory", "file", "link"] {
                    let path = net.join(old);
                    match shape {
                        "directory" => {
                            std::fs::create_dir(&path).unwrap();
                            std::fs::write(path.join("CURRENT"), "x").unwrap();
                        }
                        "file" => std::fs::write(&path, "x").unwrap(),
                        _ => std::os::unix::fs::symlink(&kept, &path).unwrap(),
                    }
                    let before = outside_the_store(net);
                    let upfront = refuse_old_llmq_paths(net).unwrap_err();
                    assert!(
                        matches!(
                            &upfront,
                            LlmqDbError::OldPath {
                                path: p,
                                moved_to,
                                ..
                            } if *p == path && *moved_to == net.join(new)
                        ),
                        "{old} {shape}: {upfront:?}"
                    );
                    assert_eq!(outside_the_store(net), before, "{old} {shape}: up-front check");
                    assert!(!net.join("llmq").exists(), "{old} {shape}: up-front check");
                    for wipe in [false, true] {
                        let failed = open(net, wipe).unwrap_err();
                        let LlmqDbError::OldPath {
                            db,
                            path: p,
                            moved_to,
                        } = &failed
                        else {
                            panic!("{old} {shape}: {failed:?}");
                        };
                        assert_eq!((p, moved_to), (&path, &net.join(new)), "{old} {shape}");
                        assert_eq!(
                            failed.to_string(),
                            format!(
                                "Found the {db} at its old path {}: move it to {} by hand and \
                                 restart",
                                path.display(),
                                net.join(new).display()
                            )
                        );
                        assert_eq!(upfront.to_string(), failed.to_string(), "{old} {shape}");
                        assert_eq!(outside_the_store(net), before, "{old} {shape}");
                        assert!(!net.join("llmq").exists(), "{old} {shape}: nothing opened");
                    }
                    if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                        std::fs::remove_dir_all(&path).unwrap();
                    } else {
                        std::fs::remove_file(&path).unwrap();
                    }
                }
                assert!(kept.join("CURRENT").is_file(), "{old}");
                open(net, false).unwrap();
                assert!(net.join(dir_name).join("CURRENT").is_file(), "{old}: opens once moved");
            }
        }
    }

    /// Each LLMQ database opens at Core's `llmq/<name>`, making `llmq/`; an open with a wipe
    /// empties its directory there and leaves no `.wipe`, and a `.wipe` a crash left there is
    /// removed by the next open and is no Core sign meanwhile. The dkgdb's wipe is its batch
    /// ([`DkgDb::open`](dashd_quorums::DkgDb::open)), so its directory open with a wipe here only
    /// shows the files layer's rename wipe works at its path too.
    // Core: v24.0.2 src/llmq/dkgsessionmgr.cpp:35, src/llmq/quorumsman.cpp:40,
    // src/llmq/signing.cpp:25
    #[test]
    fn each_llmq_database_opens_and_wipes_under_llmq() {
        for (dir_name, wiping, open) in llmq_opens() {
            let dir = tempfile::tempdir().unwrap();
            let net = dir.path();
            open(net, false).unwrap();
            let path = net.join(dir_name);
            assert!(path.join("CURRENT").is_file(), "{dir_name}");
            assert_eq!(path.parent().unwrap(), net.join("llmq"));
            std::fs::write(path.join("planted"), "x").unwrap();
            assert!(crate::datadir::probe_core_data_dir(net).is_none(), "{dir_name}");
            open(net, true).unwrap();
            assert!(!path.join("planted").exists(), "{dir_name}: wiped");
            assert!(!net.join(wiping).exists(), "{dir_name}");

            let leftover = net.join(wiping);
            std::fs::create_dir_all(leftover.join("sub")).unwrap();
            for file in ["CURRENT", "MANIFEST-000001", "sub/000004.log"] {
                std::fs::write(leftover.join(file), "old").unwrap();
            }
            assert!(crate::datadir::probe_core_data_dir(net).is_none(), "{wiping}");
            open(net, false).unwrap();
            assert!(!leftover.exists(), "{wiping}: finished by the next open");
        }
        assert_eq!(
            Path::new("/n").join(super::super::dkg::DKGDB_DIR),
            dashd_quorums::dkgdb_path(Path::new("/n"))
        );
    }

    /// `LlmqDbFiles::open` syncs `llmq/`'s entry in `net_dir` on every open, including when
    /// `llmq/` already existed (as when isdb's opener created it first without syncing `net_dir`).
    #[test]
    fn llmq_parent_is_synced_even_when_llmq_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path();
        std::fs::create_dir(net.join("llmq")).unwrap();
        let db = QuorumDb::unopened();
        db.open(net, false).unwrap();
        assert_eq!(db.synced_dirs(), [net.to_path_buf()]);
        drop(db);

        let db = QuorumDb::unopened();
        db.open(net, true).unwrap();
        assert_eq!(db.synced_dirs(), [net.to_path_buf(), net.join("llmq")]);
        drop(db);

        let fresh = tempfile::tempdir().unwrap();
        let db = QuorumDb::unopened();
        db.open(fresh.path(), false).unwrap();
        assert!(fresh.path().join("llmq").is_dir());
        assert_eq!(db.synced_dirs(), [fresh.path().to_path_buf()]);
    }

    /// A failure on `llmq/` itself during a plain open (such as a regular file at `llmq` or a
    /// read-only network directory) reports an I/O open error, never a wipe error.
    #[test]
    fn a_parent_dir_failure_reports_open_io_not_wipe() {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path();
        let parent = net.join("llmq");
        std::fs::write(&parent, "not a directory").unwrap();
        let err = QuorumDb::unopened().open(net, false).unwrap_err();
        assert!(
            matches!(
                &err,
                LlmqDbError::Open {
                    path,
                    error,
                    ..
                } if *path == parent && error.kind() == Some(ErrorKind::Io)
            ),
            "{err:?}"
        );

        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt as _;

            struct Restore<'a>(&'a Path);
            impl Drop for Restore<'_> {
                fn drop(&mut self) {
                    let _ =
                        std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
                }
            }

            let ro = tempfile::tempdir().unwrap();
            let net = ro.path();
            let parent = net.join("llmq");
            std::fs::set_permissions(net, std::fs::Permissions::from_mode(0o500)).unwrap();
            let _restore = Restore(net);
            let err = QuorumDb::unopened().open(net, false).unwrap_err();
            assert!(
                matches!(
                    &err,
                    LlmqDbError::Open {
                        path,
                        error,
                        ..
                    } if *path == parent && error.kind() == Some(ErrorKind::Io)
                ),
                "{err:?}"
            );
        }
    }

    /// T11: the in-memory database is open and empty when made, keeps its rows through an open
    /// with a wipe (Core's memory database ignores it), and writes no file.
    // Core: v24.0.0 src/dbwrapper.cpp:139-142
    #[test]
    fn in_memory_ignores_wipe_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db = QuorumDb::in_memory();
        db.write(&key(1), &contributions(1)).unwrap();
        db.open(dir.path(), true).unwrap();
        assert_eq!(summary(db.read(&key(1)).unwrap()), Some((1, Some(1))));
        assert!(QuorumDb::in_memory().read(&key(1)).unwrap().is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// `NetQuorum`'s weak handle reads and writes the open database, and once the last strong
    /// handle drops the database is closed: each call is the unopened database's error, and the
    /// directory opens again.
    #[test]
    fn a_weak_handle_does_not_keep_the_database_open() {
        let dir = tempfile::tempdir().unwrap();
        let db = opened(dir.path(), false);
        let weak = db.downgrade();
        weak.write(&key(1), &contributions(1)).unwrap();
        assert_eq!(summary(db.read(&key(1)).unwrap()), Some((1, Some(1))));
        drop(db);
        let closed = weak.read(&key(1)).unwrap_err().to_string();
        assert!(closed.contains("the quorum database is not open"), "{closed}");
        assert!(weak.write(&key(2), &contributions(2)).is_err());
        assert!(weak.erase_except(&BTreeSet::new()).is_err());
        assert_eq!(summary(opened(dir.path(), false).read(&key(1)).unwrap()), Some((1, Some(1))));
    }

    /// T18 (E2 condition 1): a wipe deletes the quorum database's directory and nothing else:
    /// every other path in the network directory, the chainstate's, the block files and
    /// look-alike names included, is byte-identical afterwards.
    #[test]
    fn a_wipe_touches_nothing_outside_the_quorum_database() {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path();
        let db = opened(net, false);
        db.write(&key(1), &contributions(1)).unwrap();
        drop(db);
        for sub in [
            "chainstate",
            "blocks",
            "evodb-not",
            "quorumdb2",
            "quorumdb.wipe2",
            "llmqx",
            "llmq/quorumdb2",
            "llmq/quorumdb.wipe2",
        ] {
            std::fs::create_dir_all(net.join(sub).join("inner")).unwrap();
            std::fs::write(net.join(sub).join("inner").join("data"), sub).unwrap();
        }
        std::fs::write(net.join("blocks").join("blk00000.dat"), [0xfa; 64]).unwrap();
        for file in ["peers.dat", "quorumdb.dat", "quorumdb.wipe.dat", "settings.json"] {
            std::fs::write(net.join(file), file).unwrap();
        }
        let before = outside_the_store(net);
        assert_eq!(before.len(), 30);
        assert!(opened(net, true).read(&key(1)).unwrap().is_none());
        assert_eq!(outside_the_store(net), before);
    }

    /// The `q-cache` thread starts after the database opens, as Core's constructor opens `db`
    /// before it spawns the thread: a database that does not open starts no thread.
    // Core: v24.0.0 src/llmq/quorumsman.cpp:40, :45-46
    #[test]
    fn the_store_opens_before_the_qcache_thread_starts() {
        let chain = crate::chain::tests::regtest();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("llmq")).unwrap();
        std::fs::write(dir.path().join(QUORUM_DB_DIR), "not a database").unwrap();
        let (mut hook, mut qman) = (None, None);
        let quorums = init_node_quorums(&chain, dir.path(), 60, &mut hook, &mut qman);
        let failed = qman.take().unwrap()(false).unwrap_err();
        assert!(failed.starts_with("Error opening the quorum database"), "{failed}");
        assert!(!quorums.qcache().is_started());
        assert!(quorums.store().read(&key(1)).is_err(), "not open");
    }

    /// An executor the test holds, counting its jobs.
    #[derive(Default)]
    struct Jobs(AtomicUsize);

    impl WarmExecutor for Jobs {
        fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
            self.0.fetch_add(1, SeqCst);
            job();
        }
    }

    /// The executor is `init_quorums`' argument (a test's here), kept by its caller; the hook
    /// goes into the slot; before the bind a Platform lookup is an error, not a panic.
    #[test]
    fn init_quorums_takes_the_callers_executor_and_fills_the_hook_slot() {
        let jobs = Arc::new(Jobs::default());
        let chain = crate::chain::tests::regtest();
        let mut hook = None;
        let quorums = init_quorums(&chain, QuorumDb::in_memory(), jobs.clone(), &mut hook);
        let hook = hook.expect("the slot holds the manager's hook");
        let manager: SharedQuorumHook = quorums.manager.clone();
        assert!(Arc::ptr_eq(&hook, &manager), "the slot's hook is the kept manager");
        assert_eq!(Arc::strong_count(&jobs), 2, "the manager holds the caller's executor");
        let unbound = quorums.platform.quorum(LlmqType::LlmqTest, &QuorumHash::from([1; 32]));
        assert!(unbound.is_err(), "{unbound:?}");
        assert_eq!(jobs.0.load(SeqCst), 0);
    }

    /// Need (c): the caller owns the executor. Init's support and flag steps and the shutdown
    /// path never name the manager or its thread; node.rs never names the thread, and its
    /// shutdown touches the quorums on one line.
    #[test]
    fn no_stop_of_the_quorum_executor_exists_outside_its_caller() {
        for (name, text) in [
            ("init/support.rs", include_str!("support.rs")),
            ("init/flags.rs", include_str!("flags.rs")),
            ("shutdown.rs", include_str!("../shutdown.rs")),
        ] {
            for needle in [
                "QCacheThread",
                "NodeQuorums",
                "init_quorums",
                "init_node_quorums",
                "WarmExecutor",
                "quorums = None",
                "quorums.take()",
            ] {
                assert!(!text.contains(needle), "{name} names {needle}");
            }
        }
        let (node, _) = include_str!("../node.rs").split_once("\nmod tests {").expect("node tests");
        for needle in ["QCacheThread", "spawn_warm", "WarmExecutor"] {
            assert!(!node.contains(needle), "node.rs names {needle}");
        }
        let (_, shutdown) = node.split_once("pub fn shutdown(&mut self) {").expect("shutdown");
        let (shutdown, _) = shutdown.split_once("\n    }\n").expect("its end");
        let lines: Vec<_> =
            shutdown.lines().map(str::trim).filter(|l| l.contains("quorums")).collect();
        assert_eq!(lines, ["self.quorums = None;"]);
    }

    /// DK1-L3 M1: the DKG's `ConnectManagers`. A build runs on V and must never wait on it: the
    /// dkgdb here sits on a chain whose every read panics, so a members read on the way (where
    /// `ChainQuorums` would send to V and deadlock) fails the test.
    #[cfg(feature = "dkg-runtime")]
    pub(crate) mod connect_managers {
        use dash_consensus::BlockId;
        use dash_dml::testkit::dmn;
        use dash_dml::{Dmn, MnType};
        use dash_evo::llmq::LlmqCommitment;
        use dash_kv::MemoryEngine;
        use dash_llmq::dkg::bls::{
            aggregate_secret_keys, build_quorum_verification_vector, generate_contributions,
            member_id,
        };
        use dash_llmq::dkg::session::DkgStore as _;
        use dash_llmq::dkg::wire::vvec_hash;
        use dash_pkc::bls::core::{BlsPkValue, BlsSigValue, BlsSkValue};
        use dash_pkc::bls::{BlsEagerPk, BlsEagerSig, BlsSchemeKind};
        use dash_primitives::{DynBitset, QuorumVvecHash};
        use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumBuildHook as _, QuorumPtr};
        use dashd_quorums::traits::DkgChain;
        use dashd_quorums::{DKGDB_SCHEMA, DkgDb};

        use super::*;

        const SCHEME: BlsSchemeKind = BlsSchemeKind::Basic;
        const T: LlmqType = LlmqType::LlmqTest;
        /// This node is member 2 (index 1).
        pub(crate) const ME: usize = 1;

        /// V's thread: any read is a send that would wait on the build's own thread.
        struct OnlyV;

        impl DkgChain for OnlyV {
            fn members(
                &self,
                _: LlmqType,
                _: &BlockHash,
            ) -> Result<Vec<ProTxHash>, dash_dml::StoreError> {
                panic!("the build hook read members through the chain (a send to V)")
            }

            fn height(&self, _: &BlockHash) -> Option<u32> {
                panic!("the build hook read a height through the chain")
            }

            fn active_height(&self) -> Option<i32> {
                panic!("the build hook read the tip through the chain")
            }
        }

        struct NoAbort;

        impl dash_kv::AbortSink for NoAbort {
            fn abort_node(&self, log_msg: &str, _: Option<&str>) {
                panic!("abort: {log_msg}")
            }

            fn read_failed(&self, what: &str) {
                panic!("read failed: {what}")
            }
        }

        pub(crate) struct Me(pub(crate) ProTxHash);

        impl LocalMasternode for Me {
            fn pro_tx_hash(&self) -> Option<ProTxHash> {
                Some(self.0)
            }
        }

        fn sk(seed: u8) -> BlsSkValue {
            BlsSkValue::from_canonical(&[seed; 32], SCHEME).unwrap()
        }

        /// A mined LLMQ_TEST quorum, every member valid, whose three dealers' rows the dkgdb
        /// holds as this node's session wrote them; and this member's share, the sum of its
        /// three contributions.
        pub(crate) struct Dkg {
            pub(crate) quorum: QuorumPtr,
            pub(crate) members: Arc<[Arc<Dmn>]>,
            pub(crate) db: Arc<dyn VerifiedContributions + Send + Sync>,
            pub(crate) my_share: BlsSkValue,
        }

        pub(crate) fn dkg() -> Dkg {
            let chain = crate::chain::tests::regtest();
            let llmq = *chain.consensus.llmq(T).unwrap();
            let (size, threshold) = (usize::from(llmq.size), usize::from(llmq.threshold));
            let members: Arc<[Arc<Dmn>]> =
                (1..=size).map(|n| dmn(n as u8, MnType::Regular)).collect::<Vec<_>>().into();
            let ids: Vec<_> = members.iter().map(|m| member_id(&m.pro_tx_hash)).collect();
            let base = QuorumHash::from(std::array::from_fn::<u8, 32, _>(|i| i as u8 + 1));
            let db = DkgDb::open(
                MemoryEngine::new(&DKGDB_SCHEMA),
                false,
                Arc::clone(&chain),
                OnlyV,
                dash_kv::GlobalFailStop::new(),
                Arc::new(NoAbort),
            )
            .unwrap();
            let mut vvecs = Vec::new();
            let mut mine = Vec::new();
            for (dealer, member) in members.iter().enumerate() {
                let coefficients: Vec<_> =
                    (0..threshold).map(|c| sk((dealer * 16 + c + 1) as u8)).collect();
                let (vvec, shares) = generate_contributions(&coefficients, &ids).unwrap();
                (&db).write_vvec(T, &base, &member.pro_tx_hash, &vvec);
                (&db).write_sk(T, &base, &member.pro_tx_hash, &shares[ME]);
                vvecs.push(vvec);
                mine.push(shares[ME].clone());
            }
            let refs: Vec<&[BlsPkValue]> = vvecs.iter().map(Vec::as_slice).collect();
            let vvec = build_quorum_verification_vector(&refs, SCHEME).unwrap();
            let my_share = aggregate_secret_keys(&mine.iter().collect::<Vec<_>>(), SCHEME);
            let mut bits = vec![0u8; size.div_ceil(8)];
            for i in 0..size {
                bits[i / 8] |= 1 << (i % 8);
            }
            let bits = DynBitset::new(size as u64, bits).unwrap();
            let commitment = LlmqCommitment::new(
                3,
                T,
                base,
                None,
                bits.clone(),
                bits,
                BlsEagerPk::new(vvec[0].clone(), SCHEME),
                QuorumVvecHash::from(vvec_hash(&vvec, SCHEME)),
                BlsEagerSig::new(BlsSigValue::invalid(), SCHEME),
                BlsEagerSig::new(BlsSigValue::invalid(), SCHEME),
            );
            let quorum = QuorumPtr(Arc::new(NodeQuorum {
                llmq_type: T,
                commitment: Arc::new(commitment),
                base: BlockId(1),
                base_hash: BlockHash::from(*base.as_bytes()),
                mined_block: BlockHash::from([0xee; 32]),
                members: members.clone(),
                keys: Mutex::new(Default::default()),
            }));
            Dkg {
                quorum,
                members,
                db: Arc::new(db),
                my_share,
            }
        }

        /// The quorum's vvec and this node's skShare after a build through `quorums`' manager.
        pub(crate) fn build(quorums: &NodeQuorums, dkg: &Dkg) -> (bool, Option<[u8; 32]>) {
            quorums.manager.built(&dkg.quorum, false, SCHEME).unwrap();
            let keys = dkg.quorum.keys.lock().unwrap();
            (keys.has_vvec(), keys.sk_share().map(|sk| sk.to_bytes(SCHEME)))
        }

        pub(crate) fn quorums() -> NodeQuorums {
            let chain = crate::chain::tests::regtest();
            init_quorums(&chain, QuorumDb::in_memory(), Arc::new(Jobs::default()), &mut None)
        }

        /// A member's build gets the dkgdb's verified contributions over the members V read for
        /// the build, and its skShare is the sum of its three contributions.
        // Core: v24.0.0 src/llmq/quorumsman.cpp:80, :120-122, :134; src/llmq/dkgsessionmgr.cpp:147
        #[test]
        fn a_members_build_gets_its_verified_contributions_and_its_sk_share() {
            let dkg = dkg();
            let quorums = quorums();
            let me = dkg.members[ME].pro_tx_hash;
            quorums.dkg_managers().connect(Arc::clone(&dkg.db), Some(Arc::new(Me(me))));
            assert_eq!(build(&quorums, &dkg), (true, Some(dkg.my_share.to_bytes(SCHEME))));
        }

        /// Unconnected (no DKG on this node), a build reads the store only, as `NoDkgSessions`;
        /// an observer connects the dkgdb without a role: the vvec, and no skShare.
        // Core: v24.0.0 src/llmq/quorumsman.cpp:120-122, :134; src/llmq/net_dkg.cpp:475
        #[test]
        fn without_a_dkg_no_contributions_and_an_observer_has_no_sk_share() {
            let dkg = dkg();
            assert_eq!(build(&quorums(), &dkg), (false, None));
            let observer = quorums();
            observer.dkg_managers().connect(Arc::clone(&dkg.db), None);
            assert_eq!(build(&observer, &dkg), (true, None));
        }

        /// A second `ConnectManagers` panics, as Core's assert aborts.
        // Core: v24.0.0 src/llmq/quorumsman.h:117-120
        #[test]
        #[should_panic(expected = "the DKG session manager is connected")]
        fn a_second_connect_panics() {
            let dkg = dkg();
            let quorums = quorums();
            quorums.dkg_managers().connect(Arc::clone(&dkg.db), None);
            quorums.dkg_managers().connect(Arc::clone(&dkg.db), None);
        }
    }

    /// DK1-L4: `quorum info`'s key shares, read from the cached quorum's keys.
    mod quorum_info_shares {
        use std::sync::{Arc, Mutex};

        use dash_consensus::{BlockId, BlsCtx};
        use dash_dml::testkit::dmn;
        use dash_dml::{Dmn, MnType};
        use dash_evo::llmq::LlmqCommitment;
        use dash_llmq::dkg::bls::{
            aggregate_secret_keys, build_quorum_verification_vector, generate_contributions,
            member_id, secret_to_public,
        };
        use dash_llmq::dkg::wire::vvec_hash;
        use dash_pkc::bls::core::{BlsPkValue, BlsSigValue, BlsSkValue};
        use dash_pkc::bls::{BlsEagerPk, BlsEagerSig, BlsSchemeKind};
        use dash_primitives::{DynBitset, QuorumVvecHash};
        use dashd_chainstate::llmq::quorum::NodeQuorum;

        use dash_evo::llmq::OperatorKeyView as _;
        use dash_evo::types::ProTxHash;
        use dash_llmq::quorum::QuorumView;
        use dash_params::{LlmqParams, LlmqType};
        use dash_primitives::{BlockHash, QuorumHash};
        use dashd_chainstate::llmq::quorum::QuorumPtr;
        use dashd_rpc::evo::QuorumFacts;

        use crate::rpc::evo::quorum::add_shares;

        const SCHEME: BlsSchemeKind = BlsSchemeKind::Basic;
        const T: LlmqType = LlmqType::LlmqTest;
        /// Member 2 (index 1) is not valid in the commitment.
        const INVALID: usize = 1;

        fn sk(seed: u8) -> BlsSkValue {
            BlsSkValue::from_canonical(&[seed; 32], SCHEME).unwrap()
        }

        /// A mined LLMQ_TEST quorum whose member 2 is invalid, and each member's share: the sum of
        /// what every dealer dealt it.
        fn quorum(params: &LlmqParams) -> (QuorumPtr, Vec<BlsPkValue>, Vec<BlsSkValue>) {
            let size = usize::from(params.size);
            let members: Arc<[Arc<Dmn>]> =
                (1..=size).map(|n| dmn(n as u8, MnType::Regular)).collect::<Vec<_>>().into();
            let ids: Vec<_> = members.iter().map(|m| member_id(&m.pro_tx_hash)).collect();
            let mut vvecs = Vec::new();
            let mut dealt = vec![Vec::new(); size];
            for dealer in 0..size {
                let coefficients: Vec<_> = (0..usize::from(params.threshold))
                    .map(|c| sk((dealer * 16 + c + 1) as u8))
                    .collect();
                let (vvec, shares) = generate_contributions(&coefficients, &ids).unwrap();
                vvecs.push(vvec);
                for (to, share) in shares.into_iter().enumerate() {
                    dealt[to].push(share);
                }
            }
            let refs: Vec<&[BlsPkValue]> = vvecs.iter().map(Vec::as_slice).collect();
            let vvec = build_quorum_verification_vector(&refs, SCHEME).unwrap();
            let shares = dealt
                .iter()
                .map(|d| aggregate_secret_keys(&d.iter().collect::<Vec<_>>(), SCHEME))
                .collect();
            let mut bits = vec![0u8; size.div_ceil(8)];
            for i in (0..size).filter(|i| *i != INVALID) {
                bits[i / 8] |= 1 << (i % 8);
            }
            let bits = DynBitset::new(size as u64, bits).unwrap();
            let base = QuorumHash::from([7; 32]);
            let commitment = LlmqCommitment::new(
                3,
                T,
                base,
                None,
                bits.clone(),
                bits,
                BlsEagerPk::new(vvec[0].clone(), SCHEME),
                QuorumVvecHash::from(vvec_hash(&vvec, SCHEME)),
                BlsEagerSig::new(BlsSigValue::invalid(), SCHEME),
                BlsEagerSig::new(BlsSigValue::invalid(), SCHEME),
            );
            let quorum = QuorumPtr(Arc::new(NodeQuorum {
                llmq_type: T,
                commitment: Arc::new(commitment),
                base: BlockId(1),
                base_hash: BlockHash::from(*base.as_bytes()),
                mined_block: BlockHash::from([0xee; 32]),
                members,
                keys: Mutex::new(Default::default()),
            }));
            (quorum, vvec, shares)
        }

        fn facts(quorum: &QuorumPtr) -> QuorumFacts {
            QuorumFacts {
                type_name: T.to_string(),
                rotated: false,
                base_height: 1,
                hash: quorum.base_hash,
                index: 0,
                mined_block: quorum.mined_block,
                prev_failures: None,
                members: quorum.members.iter().map(|m| (Arc::clone(m), true)).collect(),
                valid_count: 2,
                single_member: false,
                bls: BlsCtx::BASIC,
                quorum_pk: [0; 48],
                pub_key_shares: vec![],
                sk_share: None,
            }
        }

        fn install(
            quorum: &QuorumPtr,
            params: &LlmqParams,
            vvec: Vec<BlsPkValue>,
            me: usize,
            sk: BlsSkValue,
        ) {
            let members: Vec<ProTxHash> = quorum.members.iter().map(|m| m.pro_tx_hash).collect();
            let operator_keys: Vec<_> = quorum.members.iter().map(|m| m.operator_key()).collect();
            let view = QuorumView {
                llmq: params,
                commitment: &quorum.commitment,
                members: &members,
                operator_keys: &operator_keys,
            };
            let mut keys = quorum.keys.lock().unwrap();
            assert!(keys.set_verification_vector(&view, vvec, SCHEME));
            assert!(keys.set_secret_key_share(&view, sk, &members[me]));
        }

        /// A member's answer carries every valid member's share, each the public key of the secret
        /// share that member holds, none for the invalid member, and its own secret share only when
        /// asked for.
        // Core: v24.0.0 src/rpc/quorums.cpp:225-244, src/llmq/quorums.cpp:155-179
        #[test]
        fn a_members_answer_has_each_valid_members_share_and_its_own_secret_share_on_request() {
            let chain = crate::chain::tests::regtest();
            let params = *chain.consensus.llmq(T).unwrap();
            let (quorum, vvec, shares) = quorum(&params);
            install(&quorum, &params, vvec, 0, shares[0].clone());
            for include in [false, true] {
                let mut f = facts(&quorum);
                add_shares(&mut f, &quorum, &params, include);
                let expected: Vec<_> = shares
                    .iter()
                    .enumerate()
                    .map(|(i, sk)| (i != INVALID).then(|| secret_to_public(sk).to_bytes(SCHEME)))
                    .collect();
                assert_eq!(f.pub_key_shares, expected);
                assert_eq!(f.sk_share, include.then(|| shares[0].to_bytes(SCHEME)));
            }
        }

        /// A node without the quorum's verification vector or a share (an observer that neither watched
        /// the DKG nor recovered the vector) gives neither.
        #[test]
        fn an_observers_answer_has_no_shares() {
            let chain = crate::chain::tests::regtest();
            let params = *chain.consensus.llmq(T).unwrap();
            let (quorum, _, _) = quorum(&params);
            let mut f = facts(&quorum);
            add_shares(&mut f, &quorum, &params, true);
            assert_eq!(f.pub_key_shares, vec![None; usize::from(params.size)]);
            assert_eq!(f.sk_share, None);
        }

        /// Under the legacy scheme the shares and the secret share are written as Core writes them while
        /// its global flag is legacy, whatever scheme the keys were built under.
        // Core: v24.0.0 src/bls/bls.h:229-231, src/rpc/quorums.cpp:236, :243
        #[test]
        fn a_legacy_answer_writes_its_shares_in_the_legacy_scheme() {
            let chain = crate::chain::tests::regtest();
            let params = *chain.consensus.llmq(T).unwrap();
            let (quorum, vvec, shares) = quorum(&params);
            install(&quorum, &params, vvec, 0, shares[0].clone());
            let mut f = facts(&quorum);
            f.bls = BlsCtx::LEGACY;
            add_shares(&mut f, &quorum, &params, true);
            let legacy = |i: usize| secret_to_public(&shares[i]).to_bytes(BlsSchemeKind::Legacy);
            assert_ne!(legacy(0), secret_to_public(&shares[0]).to_bytes(SCHEME));
            assert_eq!(f.pub_key_shares, vec![Some(legacy(0)), None, Some(legacy(2))]);
            assert_eq!(f.sk_share, Some(shares[0].to_bytes(BlsSchemeKind::Legacy)));
        }

        /// A member's answer reads the keys its quorum build filled: the manager's build over the
        /// dkgdb's verified contributions gives every member's share and this member's secret share.
        // Core: v24.0.0 src/llmq/quorumsman.cpp:120-134, src/rpc/quorums.cpp:225-244
        #[cfg(feature = "dkg-runtime")]
        #[test]
        fn a_members_answer_reads_the_keys_its_quorum_build_filled() {
            use super::connect_managers::{ME, Me, build, dkg, quorums};

            let chain = crate::chain::tests::regtest();
            let params = *chain.consensus.llmq(T).unwrap();
            let dkg = dkg();
            let quorums = quorums();
            let me = dkg.members[ME].pro_tx_hash;
            quorums.dkg_managers().connect(Arc::clone(&dkg.db), Some(Arc::new(Me(me))));
            assert_eq!(build(&quorums, &dkg), (true, Some(dkg.my_share.to_bytes(SCHEME))));
            let mut f = facts(&dkg.quorum);
            add_shares(&mut f, &dkg.quorum, &params, true);
            assert_eq!(f.pub_key_shares.len(), usize::from(params.size));
            assert!(f.pub_key_shares.iter().all(Option::is_some), "{:?}", f.pub_key_shares);
            assert_eq!(
                f.pub_key_shares[ME],
                Some(secret_to_public(&dkg.my_share).to_bytes(SCHEME))
            );
            assert_eq!(f.sk_share, Some(dkg.my_share.to_bytes(SCHEME)));
        }
    }
}
