//! Core's `CQuorumManager` as init builds it (`LLMQContext::qman`) for a node that takes no part
//! in DKG and watches no quorums (an active masternode's DKG is DK1's), its quorum database and
//! its `q-cache` thread.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;

use dash_kv::{ErrorKind, FlatFs as _, MemoryEngine, StoreError};
use dash_params::ChainSetup;
use dashd_chainstate::chain::ChainHandle;
use dashd_chainstate::llmq::quorum::SharedQuorumHook;
use dashd_quorums::{
    ChainQuorums, CleanupCounts, KvQuorumStore, NoDkgSessions, NotMasternode, PlatformQuorums,
    QUORUMDB_SCHEMA, QuorumDbKey, QuorumKeyStore, QuorumManager, StoredContributions, WarmExecutor,
};
use dashd_store::flatfile::OsFs;
use dashd_store::{RocksConfig, RocksEngine};

use crate::chainstate::QmanStart;
use crate::host::trace_thread;

/// rdashd's `CQuorumManager`: the node's contribution store and absent DKG and masternode seams.
/// Its type does not depend on the executor init passes it.
pub type NodeQuorumManager =
    QuorumManager<QuorumDb, NoDkgSessions, NotMasternode, Arc<dyn WarmExecutor + Send + Sync>>;

/// The quorum database's directory in the network data directory.
///
/// Core's is `llmq/quorumdb`. rdashd takes an `llmq/` in a network directory for Dash Core's and
/// refuses the directory (`datadir::refuse_core_dir`), so its own would refuse its next start;
/// this is the one difference from Core's layout. It is outside the directories a regtest live
/// copy keeps (`blocks`, `chainstate`, `evodb`, `indexes`), so a copy opens a new, empty one.
// Core: v24.0.0 src/llmq/quorumsman.cpp:40
pub const QUORUM_DB_DIR: &str = "quorumdb";

/// Where a wipe moves the quorum database before deleting it.
const WIPING_DIR: &str = "quorumdb.wipe";

/// The node's quorum database (`CQuorumManager::db`): a handle the manager and init share, opened
/// by [`open`](Self::open) inside the chainstate load, where Core's `LLMQContext` builds `qman`.
/// Before the open every read and write is an error.
#[derive(Clone)]
pub struct QuorumDb {
    backend: Arc<OnceLock<Backend>>,
    #[cfg(test)]
    fault: Arc<Mutex<Option<WipeStep>>>,
}

enum Backend {
    Rocks(KvQuorumStore<RocksEngine>),
    Memory(KvQuorumStore<MemoryEngine>),
}

/// A step of the quorum database's wipe, for a test's injected failure.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WipeStep {
    Rename,
    Sync,
    Delete,
}

/// Why the quorum database did not open: Core's `dbwrapper_error` from its constructor.
#[derive(Debug)]
pub enum QuorumDbError {
    /// A wipe step failed on `path`.
    Wipe {
        path: PathBuf,
        error: io::Error,
    },
    /// The database was moved to `path` (`quorumdb.wipe`), but syncing the move failed: its rows
    /// do not survive the next open.
    Sync {
        path: PathBuf,
        error: io::Error,
    },
    /// A wipe found `path` is neither a directory nor a symbolic link, and left it.
    NotADirectory {
        path: PathBuf,
    },
    /// The engine did not open the database at `path`.
    Open {
        path: PathBuf,
        error: StoreError,
    },
    /// The handle was already open.
    AlreadyOpen,
}

impl fmt::Display for QuorumDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wipe {
                path,
                error,
            } => write!(f, "Error wiping the quorum database {}: {error}", path.display()),
            Self::Sync {
                path,
                error,
            } => {
                write!(f, "Error syncing the quorum database's move to {}: {error}", path.display())
            }
            Self::NotADirectory {
                path,
            } => write!(f, "Error wiping the quorum database {}: not a directory", path.display()),
            Self::Open {
                path,
                error,
            } => write!(f, "Error opening the quorum database {}: {error}", path.display()),
            Self::AlreadyOpen => f.write_str("the quorum database is already open"),
        }
    }
}

impl std::error::Error for QuorumDbError {}

impl QuorumDb {
    /// A handle whose database is not open yet.
    pub fn unopened() -> Self {
        Self {
            backend: Arc::default(),
            #[cfg(test)]
            fault: Arc::default(),
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

    /// Core's `CDBWrapper` constructor for the quorum database in `net_dir`: with `wipe`, the
    /// database is destroyed without being opened, so a wipe also recovers an unreadable one; then
    /// it opens, created if missing.
    ///
    /// The destroy is a rename to `quorumdb.wipe`, made durable, then a delete; a wipe a crash
    /// interrupted is finished at the next open, whatever its `wipe`. So a crash leaves the old
    /// database whole or none of it, where Core's `DestroyDB` deletes file by file. Nothing outside
    /// those two directories is touched. If the rename succeeds and its sync fails, the open
    /// fails with [`QuorumDbError::Sync`]: the database is already at `quorumdb.wipe`, so its
    /// rows do not survive the next open, where a failed rename keeps them.
    ///
    /// Differences from Core's `DestroyDB`, which deletes only LevelDB's own files of the
    /// directory and ignores its failure to remove the directory:
    /// - the database is RocksDB, and the wipe deletes the whole directory with whatever else it
    ///   holds;
    /// - a symbolic link at `quorumdb` is moved and unlinked and its target left whole, where
    ///   `DestroyDB` deletes LevelDB's files through the link.
    ///
    /// As with Core, a `quorumdb` that is neither a directory nor a link is left as it is and the
    /// open fails ([`QuorumDbError::NotADirectory`]): `DestroyDB` cannot list it and returns, and
    /// the open after it fails.
    // Core: v24.0.0 src/dbwrapper.cpp:130-158, src/dbwrapper.h:762-773;
    // src/leveldb/db/db_impl.cc:1525-1554 DestroyDB
    pub fn open(&self, net_dir: &Path, wipe: bool) -> Result<(), QuorumDbError> {
        match self.backend.get() {
            Some(Backend::Memory(_)) => return Ok(()),
            Some(Backend::Rocks(_)) => return Err(QuorumDbError::AlreadyOpen),
            None => {}
        }
        let (path, wiping) = (net_dir.join(QUORUM_DB_DIR), net_dir.join(WIPING_DIR));
        let failed = |path: &Path| {
            let path = path.to_path_buf();
            move |error| QuorumDbError::Wipe {
                path,
                error,
            }
        };
        if exists(&wiping).map_err(failed(&wiping))? {
            self.remove(&wiping).map_err(failed(&wiping))?;
        }
        if wipe && exists(&path).map_err(failed(&path))? {
            if !wipeable(&path).map_err(failed(&path))? {
                return Err(QuorumDbError::NotADirectory {
                    path,
                });
            }
            self.rename(&path, &wiping).map_err(failed(&path))?;
            self.sync(net_dir).map_err(|error| QuorumDbError::Sync {
                path: wiping.clone(),
                error,
            })?;
            self.remove(&wiping).map_err(failed(&wiping))?;
        }
        let (engine, _) =
            RocksEngine::open(&path, &QUORUMDB_SCHEMA, &engine_config()).map_err(|error| {
                QuorumDbError::Open {
                    path: path.clone(),
                    error,
                }
            })?;
        self.backend
            .set(Backend::Rocks(KvQuorumStore::new(engine)))
            .map_err(|_| QuorumDbError::AlreadyOpen)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(test)]
        self.injected(WipeStep::Rename)?;
        OsFs::new().rename(from, to)
    }

    fn sync(&self, dir: &Path) -> io::Result<()> {
        #[cfg(test)]
        self.injected(WipeStep::Sync)?;
        OsFs::new().sync_dir(dir)
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
                Err(io::Error::other(format!("injected quorum database {step:?} failure")))
            }
            _ => Ok(()),
        }
    }

    /// Makes `step` of every later wipe fail.
    #[cfg(test)]
    pub(crate) fn fail_wipe_at(&self, step: WipeStep) {
        *self.fault.lock().unwrap_or_else(PoisonError::into_inner) = Some(step);
    }

    fn backend(&self) -> Result<&Backend, StoreError> {
        self.backend.get().ok_or_else(|| {
            StoreError::engine("quorumdb", ErrorKind::Other, "the quorum database is not open")
        })
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
fn engine_config() -> RocksConfig {
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
    store: QuorumDb,
    chain: Arc<ChainQuorums>,
    qcache: Option<Arc<QCacheThread>>,
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

    #[cfg(test)]
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
/// as the executor, kept in the result. The rest of Core's `CQuorumManager` constructor goes
/// into `qman`, which the load runs after the BLS check threads start (Core's `LLMQContext`
/// builds `qman` after its block processor): the database opens, wiped if the load says so, then
/// the thread starts. Dropping the result joins the thread.
// Core: v24.0.0 src/llmq/context.cpp:19-22, src/llmq/quorumsman.cpp:31-46
pub fn init_node_quorums(
    chain: &Arc<ChainSetup>,
    net_dir: &Path,
    hook: &mut Option<SharedQuorumHook>,
    qman: &mut Option<QmanStart>,
) -> NodeQuorums {
    let thread = QCacheThread::new();
    let store = QuorumDb::unopened();
    let mut quorums = init_quorums(chain, store.clone(), thread.clone(), hook);
    let (starter, net_dir) = (Arc::clone(&thread), net_dir.to_path_buf());
    *qman = Some(Box::new(move |wipe| {
        store.open(&net_dir, wipe).map_err(|e| e.to_string())?;
        // Core's `MigrateOldQuorumDB(evoDb)` is not ported: it moves pre-v20 rows from evoDb,
        // and rdashd refuses a Core data directory (`datadir::refuse_core_dir`), so no Core
        // evoDb rows exist to move (D-row).
        // Core: v24.0.0 src/llmq/quorumsman.cpp:44, :678-741
        starter.start().map_err(|e| e.to_string())
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
    let manager = QuorumManager::new(
        Arc::clone(chain),
        store.clone(),
        NoDkgSessions,
        NotMasternode,
        executor,
    )
    .with_quorum_lookup(seams.clone())
    .with_cleanup_facade(seams.clone());
    #[cfg(feature = "test-hooks")]
    let manager = manager.with_share_seam_view(seams.clone());
    let manager = Arc::new(manager);
    *hook = Some(manager.clone());
    NodeQuorums {
        platform: manager.clone(),
        manager,
        store,
        chain: seams,
        qcache: None,
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
        let quorums = init_node_quorums(&chain, dir.path(), &mut hook, &mut qman);
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
                if d == dir
                    && [QUORUM_DB_DIR, WIPING_DIR]
                        .map(Some)
                        .contains(&path.file_name().and_then(|n| n.to_str()))
                {
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
        assert!(matches!(db.open(dir.path(), false), Err(QuorumDbError::AlreadyOpen)));
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
        std::fs::write(&path, "not a database").unwrap();
        let open = QuorumDb::unopened().open(dir.path(), false);
        assert!(matches!(open, Err(QuorumDbError::Open { .. })), "{open:?}");
        let open = QuorumDb::unopened().open(dir.path(), true);
        assert!(
            matches!(&open, Err(QuorumDbError::NotADirectory { path: p }) if *p == path),
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
        let db = opened(dir.path(), false);
        assert!(!leftover.exists(), "the interrupted wipe is finished");
        assert_eq!(summary(db.read(&key(1)).unwrap()), Some((1, Some(1))));
        drop(db);

        let db = QuorumDb::unopened();
        db.fail_wipe_at(WipeStep::Rename);
        let open = db.open(dir.path(), true);
        assert!(
            matches!(&open, Err(QuorumDbError::Wipe { path: p, .. }) if *p == path),
            "{open:?}"
        );
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
                Err(QuorumDbError::Sync {
                    path: p,
                    ..
                }) if *p == leftover => WipeStep::Sync,
                Err(QuorumDbError::Wipe {
                    path: p,
                    ..
                }) if *p == leftover => WipeStep::Delete,
                _ => panic!("{step:?}: {open:?}"),
            };
            assert_eq!(failed_at, step);
            assert!(!path.exists() && leftover.exists(), "{step:?}: moved, not deleted");
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
        std::fs::create_dir(&net).unwrap();
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

    /// T10: the database's directory is not one rdashd takes for Dash Core's (`evodb`, `llmq`).
    #[test]
    fn the_quorum_store_is_not_a_core_sign() {
        let dir = tempfile::tempdir().unwrap();
        opened(dir.path(), false).write(&key(1), &contributions(1)).unwrap();
        assert!(dir.path().join(QUORUM_DB_DIR).join("CURRENT").is_file());
        assert!(crate::datadir::probe_core_data_dir(dir.path()).is_none());
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
        for sub in ["chainstate", "blocks", "evodb-not", "quorumdb2", "quorumdb.wipe2", "llmqx"] {
            std::fs::create_dir_all(net.join(sub).join("inner")).unwrap();
            std::fs::write(net.join(sub).join("inner").join("data"), sub).unwrap();
        }
        std::fs::write(net.join("blocks").join("blk00000.dat"), [0xfa; 64]).unwrap();
        for file in ["peers.dat", "quorumdb.dat", "quorumdb.wipe.dat", "settings.json"] {
            std::fs::write(net.join(file), file).unwrap();
        }
        let before = outside_the_store(net);
        assert_eq!(before.len(), 23);
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
        std::fs::write(dir.path().join(QUORUM_DB_DIR), "not a database").unwrap();
        let (mut hook, mut qman) = (None, None);
        let quorums = init_node_quorums(&chain, dir.path(), &mut hook, &mut qman);
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
}
