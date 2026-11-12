//! CH2b b4: V over the node's own stores. [`NodeBlockFiles`] (M2) is Core's `SaveBlockToDisk`,
//! `ReadBlockFromDisk` and `LoadBlockIndexDB`'s file check on ST2's files; [`NodeFlush`] (M1) is
//! V's flush over CH3a's coins and W's thread. The rig wires them as the node does: one
//! [`GuardedFiles`] for V's blocks, the connector's undo and W's flush, and one latch under
//! every store.

use std::collections::HashSet;
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use tracing::Level;

use dash_chain::{
    BlockIndexRecord, BlockRef, ChainConfig, ChainError, FeedSource, FlatFilePos, Handoff,
    HeaderChain, Needs, Next, Notify, Parker, Status, SubscriberFeed,
};
use dash_consensus::coins::stats::CoinStatsHashType;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{Header, Tx};
use dash_consensus::{Block, BlockId, BlsCtx};
use dash_kv::ErrorKind;
use dash_num::CompactTarget;
use dash_params::ChainSetup;
use dash_primitives::{BlockHash, BlockHeader, Transaction, TxHash};
use dash_types::codec::BaseCodec as _;
use dashd_store::flatfile::{
    BlockFiles as StoreFiles, BlockFilesConfig, BlockFilesState, FlatFilePos as StorePos, FlatFs,
    FlatRead, OpenFailed, OsFs,
};
use dashd_store::{KvEngine, KvRead as _, MemoryEngine, SchemaState, StoreError};

use super::connector::checked;
use super::failure::{at_close, pause, tip};
#[cfg(feature = "test-support")]
use super::failure::{replies_at_first_release, take};
use super::flush::{InitCalls, assert_init_dropped, block_ref};
#[cfg(feature = "test-support")]
use super::locks::sig_hash;
use super::locks::{intake, lock, record, settle};
use super::support::{
    Mined, REGTEST_BITS, Rig, RigConfig, SPACING, assert_logged_before, branch, cb_script, child,
    coinbase, install_accept_log, levels_here, lines_of, lines_on, lines_with, mine, regtest,
    services,
};
#[cfg(feature = "test-support")]
use crate::chain::Quiet;
use crate::chain::abc::DISCONNECT_FAILED;
use crate::chain::actor::V;
use crate::chain::actor::tests::Stage;
use crate::chain::flush::AT_PUMP;
use crate::chain::read_ahead::Probe;
use crate::chain::reply::WaitError;
use crate::chain::{
    AbortCause, AbortPath, BlockConnector, BlockFiles, BlockResult, BlockState, BlockStatusWriter,
    CacheUsage, ChainCommand, ConnectCx, ConnectOutcome, DisconnectPool, DisconnectResult, EhfRows,
    EvoReads, Fatal, FlushHost, FlushSite, LoadFailure, LoadedIndex, LocalShutdown, MempoolSink,
    PREVIOUSLY_PRUNED, ProposalChecks, ReadAheadSetup, RecordingAbort, RemovalReason,
    ScratchParent, ScratchView, ShutdownFlag as _, ShutdownFlush, SiteDecision, StoredRecord,
    VContext, ValidationInterface, ViewTarget, open_block_index,
};
#[cfg(feature = "test-support")]
use crate::chain::{BlockTailStage, StagedRecord};
use crate::checkqueue::CheckQueue;
use crate::coins::{
    CoinLayerOps as _, CoinsBoundary, CoinsBudget, CoinsCache, CoinsDump, CoinsScan, CoinsScanJob,
    CoinsStatsJob, Ratio, Salt, start_coins_scan, start_coins_stats,
};
use crate::connect::{
    ChainstateCell, ConnectorParts, CoreConnector, EvoHooks, NoEvo as NoEvoHooks, NodeHelper,
};
use crate::flush::{
    CrashRoll, FilesPruneHost, FlushConfig, FlushInputs, FlushObserver, FlushPolicy,
    FlushStateMode, NoEvo, NoInputs, NoPrune, NodeFlush, NodeFlushParts, NullObserver, ObsMark,
    PruneDryRun, PruneHost, RecordingObserver, SubscriberHub, ThreadDriver, WThread,
};
use crate::flush::{Kept, kept};
use crate::recovery::{ChainstateOpen, FileRead};
use crate::reorg::{PoolCall, PoolsOnly};
use crate::sigcache::{CacheSizes, ScriptCaches};
use crate::store::held::{HeldSink, W_THREAD, fail_on_w, report_on_w, until};
use crate::store::sites::{BLOCK_FILES_WRITE_SITE, V_PANIC_SITE, W_PANIC_SITE, WRITE_SITE};
use crate::store::{
    CHAINSTATE_SCHEMA, Failure, GlobalFailStop, GuardedEngine, GuardedFiles, Meta, NodeBlockFiles,
    ProbeEngine, TIP,
};
use crate::tuning::{EngineMemoryReport, EngineTuning, MemoryTuning};

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
/// How long a test waits for what must happen at once.
const PROMPT: Duration = Duration::from_secs(10);
/// How long a row probes for a reply to a command sent while V is inside a prune's call: V
/// answers it only after the call, so the probe is spent in full.
#[cfg(feature = "test-support")]
const PROBE: Duration = Duration::from_secs(2);
/// A generous bound on a wait a test needs to end.
const SOON: Duration = Duration::from_secs(120);

/// The OS's files, with what a test changes: the free space, a failing write, space query,
/// sync or read's open, and a gate that holds W's space check (W's thread only: V's own `CheckDiskSpace` at a block's write goes on),
/// and a panic of W's there.
struct TestFs {
    os: OsFs,
    free: AtomicU64,
    fail_write: AtomicBool,
    fail_create: AtomicBool,
    fail_space: AtomicBool,
    fail_sync: AtomicBool,
    /// Every read's file fails to open.
    fail_open: AtomicBool,
    gate: Gate,
    /// W panics in its space check, past the gate.
    panic_w: AtomicBool,
    /// Holds W at its removes: a prune's unlink (step 12).
    unlink: Gate,
    /// Holds a write of a thread but W's, then fails it (BL-554's losing save).
    held_write: Gate,
    /// Runs once inside the next block write of a thread but W's, which then fails (BL-677).
    at_save: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// [`Self::at_save`] ran.
    saved: AtomicBool,
    /// The files each remove took off disk.
    removed: Mutex<Vec<String>>,
}

impl TestFs {
    fn new() -> TestFs {
        TestFs {
            os: OsFs::new(),
            free: AtomicU64::new(u64::MAX),
            fail_write: AtomicBool::new(false),
            fail_create: AtomicBool::new(false),
            fail_space: AtomicBool::new(false),
            fail_sync: AtomicBool::new(false),
            fail_open: AtomicBool::new(false),
            gate: Gate::default(),
            panic_w: AtomicBool::new(false),
            unlink: Gate::default(),
            held_write: Gate::default(),
            at_save: Mutex::default(),
            saved: AtomicBool::new(false),
            removed: Mutex::default(),
        }
    }
}

/// [`TestFs`]'s hold on W, in its job's space check (`machine.rs`'s `check_space`) or its
/// removes.
#[derive(Default)]
struct Gate {
    /// (armed, W held at it).
    state: Mutex<(bool, bool)>,
    cv: Condvar,
}

impl Gate {
    fn arm(&self) {
        *self.state.lock().expect("gate") = (true, false);
    }

    /// Whether W reached the gate within [`PROMPT`].
    fn wait_held(&self) -> bool {
        let state = self.state.lock().expect("gate");
        let (state, _) = self.cv.wait_timeout_while(state, PROMPT, |s| !s.1).expect("gate");
        state.1
    }

    fn release(&self) {
        self.state.lock().expect("gate").0 = false;
        self.cv.notify_all();
    }

    fn pass(&self) {
        if on_w() {
            self.hold();
        }
    }

    /// Holds this thread while armed; whether it held it.
    fn hold(&self) -> bool {
        let mut state = self.state.lock().expect("gate");
        if !state.0 {
            return false;
        }
        state.1 = true;
        self.cv.notify_all();
        // Bounded, so a test that fails with W held still ends: W then goes on.
        let _held = self.cv.wait_timeout_while(state, 6 * PROMPT, |s| s.0).expect("gate");
        true
    }
}

/// Whether this is W's thread.
fn on_w() -> bool {
    std::thread::current().name() == Some("dashd-flush")
}

impl FlatFs for TestFs {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.link_count(path)
    }
    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.len(path)
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        if self.fail_create.load(Ordering::SeqCst) {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        self.os.create(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        self.os.open_read(path)
    }
    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if self.fail_open.load(Ordering::SeqCst) {
            return Err(OpenFailed::wrap(io::ErrorKind::NotFound.into()));
        }
        self.os.read_exact_at(path, offset, buf)
    }
    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        let blk = path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("blk"));
        if blk
            && !on_w()
            && let Some(at_save) = self.at_save.lock().expect("at_save").take()
        {
            at_save();
            self.saved.store(true, Ordering::SeqCst);
            return Err(io::Error::other("injected write failure"));
        }
        if self.fail_write.load(Ordering::SeqCst) || (!on_w() && self.held_write.hold()) {
            return Err(io::Error::other("injected write failure"));
        }
        self.os.write_all_at(path, offset, data)
    }
    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        self.os.allocate(path, offset, len);
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.os.truncate(path, len)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        if self.fail_sync.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected sync failure"));
        }
        self.os.sync_data(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.os.sync_dir(dir)
    }
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.copy(from, to)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<bool> {
        self.unlink.pass();
        let removed = self.os.remove(path)?;
        if removed && let Some(name) = path.file_name() {
            self.removed.lock().expect("removed").push(name.to_string_lossy().into_owned());
        }
        Ok(removed)
    }
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.os.create_dir_all(dir)
    }
    fn available_space(&self, dir: &Path) -> io::Result<u64> {
        self.gate.pass();
        assert!(!(on_w() && self.panic_w.load(Ordering::SeqCst)), "a test's panic on W");
        if self.fail_space.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected space query failure"));
        }
        let _ = self.os.available_space(dir)?;
        Ok(self.free.load(Ordering::SeqCst))
    }
}

/// One node's stores, which outlive its V.
pub(super) struct Stores {
    p: Arc<ChainSetup>,
    dir: tempfile::TempDir,
    fs: Arc<TestFs>,
    pub(super) latch: GlobalFailStop,
    /// The node's `AbortNode`, every store's and V's.
    pub(super) sink: Arc<RecordingAbort>,
    /// The engine's way to `sink`, where a test holds W's report (BL-554).
    held: Arc<HeldSink>,
    db: Arc<GuardedEngine<ProbeEngine>>,
    coins: ChainstateCell<ProbeEngine>,
    files: Arc<GuardedFiles<TestFs>>,
    hub: Arc<SubscriberHub>,
    /// `-prune` with `-fastprune`'s 64 KiB blk files.
    prune: bool,
    /// [`Switched`]'s countdown: when set, its host turns on at that many more Core-placed
    /// decisions (each reads the check once, in flight or not).
    countdown: Arc<AtomicU32>,
}

/// What a test sets in the flush's seams beside the stores.
pub(super) struct Seams {
    crash: CrashRoll,
    on_crash: Box<dyn FnOnce() + Send>,
    prune: Box<dyn PruneHost + Send>,
    /// V's and W's marks.
    obs: Arc<dyn FlushObserver>,
    /// `-dbcrashratio` is set (D14).
    synchronous: bool,
    tuning: Arc<HookTuning>,
    /// Core's `fReindex`, as the flush reads it ([`Reindexing`]).
    reindex: bool,
    /// Regtest's live copy: an ALWAYS flush ends with the engine's barriers (A1 §2.9).
    live_copy: bool,
    /// V's waits for W park on the driver's own parker, not on V's bell ([`NoBell`]).
    parker: bool,
    /// CH5-5 D3-3's `AfterRelease` sync point on W.
    after_release: Option<crate::flush::AfterRelease>,
    /// The tally V books its jobs in.
    #[cfg(feature = "test-support")]
    tally: Option<Arc<crate::flush::FlushTally>>,
}

/// [`MemoryTuning`] whose post-Bk hook takes `delay`, then counts itself: a hook that runs after
/// the release is still running when V has answered. `on_barrier` runs once on W, at the start of
/// the next live-copy barrier (CH5-5 D3-3).
#[derive(Default)]
struct HookTuning {
    delay: Duration,
    hooks: AtomicU32,
    on_barrier: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Set, the next post-Bk hook records its thread and waits until the gate opens.
    hold: Mutex<Option<Arc<BkGate>>>,
}

/// A gate a W hook waits at, and the thread that waited.
#[derive(Default)]
struct BkGate {
    open: Mutex<bool>,
    opened: Condvar,
    entered: Mutex<Option<std::thread::ThreadId>>,
}

impl EngineTuning for HookTuning {
    type E = MemoryEngine;

    fn open_chainstate(
        &self,
        path: &Path,
        sizes: &crate::caches::CacheSizes,
        sub_batch: u64,
    ) -> Result<(MemoryEngine, SchemaState), StoreError> {
        MemoryTuning.open_chainstate(path, sizes, sub_batch)
    }

    fn batch_reserve(&self, sub_batch: u64) -> u64 {
        MemoryTuning.batch_reserve(sub_batch)
    }

    fn after_bk<K: KvEngine>(&self, db: &GuardedEngine<K>) -> Result<(), StoreError> {
        std::thread::sleep(self.delay);
        if let Some(gate) = self.hold.lock().expect("hold").take() {
            *gate.entered.lock().expect("entered") = Some(std::thread::current().id());
            let open = gate.open.lock().expect("gate");
            let wait = gate.opened.wait_timeout_while(open, Duration::from_secs(120), |o| !*o);
            assert!(*wait.expect("gate").0, "the test never opened the gate");
        }
        self.hooks.fetch_add(1, Ordering::SeqCst);
        MemoryTuning.after_bk(db)
    }

    fn live_copy_barrier<K: KvEngine>(
        &self,
        db: &GuardedEngine<K>,
        snapshot: &mut dyn FnMut(),
    ) -> Result<Option<dashd_store::LiveVerdict>, StoreError> {
        if let Some(f) = self.on_barrier.lock().expect("on_barrier").take() {
            f();
        }
        MemoryTuning.live_copy_barrier(db, snapshot)
    }

    fn journal_budget(&self) -> u64 {
        MemoryTuning.journal_budget()
    }

    fn unbounded_delete_span(&self) -> u32 {
        MemoryTuning.unbounded_delete_span()
    }

    fn memory_report(&self, db: &GuardedEngine<MemoryEngine>) -> EngineMemoryReport {
        MemoryTuning.memory_report(db)
    }
}

impl Default for Seams {
    fn default() -> Self {
        Seams {
            crash: CrashRoll::seeded(0, 0),
            on_crash: Box::new(|| {}),
            prune: Box::new(NoPrune),
            obs: Arc::new(NullObserver),
            synchronous: false,
            tuning: Arc::default(),
            reindex: false,
            live_copy: false,
            parker: false,
            after_release: None,
            #[cfg(feature = "test-support")]
            tally: None,
        }
    }
}

/// The flush's inputs under `-reindex`: no mempool, `fReindex` set.
struct Reindexing;

impl FlushInputs for Reindexing {
    fn mempool_usage(&self) -> u64 {
        0
    }

    fn reindex(&self) -> bool {
        true
    }
}

/// [`FilesPruneHost`] whose automatic check stays off, its flag kept, until `on` is set, or until
/// its countdown, when set, reaches zero.
struct Switched {
    on: Arc<AtomicBool>,
    countdown: Arc<AtomicU32>,
    host: FilesPruneHost<TestFs>,
}

impl Switched {
    /// One Core-placed call: the policy reads the check once per call, through `check_due` with
    /// nothing in flight and through `check_armed` in flight, so each counts it.
    fn tick(&self) {
        let left =
            self.countdown.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
        if left == Ok(1) {
            self.on.store(true, Ordering::SeqCst);
        }
    }
}

impl PruneHost for Switched {
    fn check_due(&mut self) -> bool {
        self.tick();
        self.on.load(Ordering::SeqCst) && self.host.check_due()
    }

    fn check_armed(&self) -> bool {
        self.tick();
        self.on.load(Ordering::SeqCst) && self.host.check_armed()
    }

    fn would_select(&self, tip: u32, bound: u32, is_ibd: bool) -> bool {
        self.host.would_select(tip, bound, is_ibd)
    }

    fn dry_run(&self, tip: u32, bound: u32, manual: Option<u32>, is_ibd: bool) -> PruneDryRun {
        self.host.dry_run(tip, bound, manual, is_ibd)
    }

    fn limited(&self, name: &str, height: u32) {
        self.host.limited(name, height);
    }

    fn arm(&mut self) {
        self.host.arm();
    }
}

/// BL-818 (CH3b D243): with `on` unset, [`Switched`]'s peek reads no check, and the armed check
/// stays for when it is set.
#[test]
fn a_peek_takes_no_check() {
    let s = Stores::pruned();
    let on = Arc::new(AtomicBool::new(false));
    let mut host = s.prune_host(1, &on);
    s.files
        .mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        })
        .unwrap();
    assert!(!host.check_armed() && !host.check_due(), "off");
    on.store(true, Ordering::SeqCst);
    assert!(host.check_armed(), "the check is still there");
    assert!(host.check_due() && !host.check_armed(), "taken");
}

/// A flush site that never takes V's bell (BL-554): V's waits for W park on the driver's own
/// parker, the other way the driver wakes V. Only a synchronous policy's waits suit it: V's
/// idle select hears nothing of W.
struct NoBell(NodeFlush<ProbeEngine, ()>);

impl FlushSite for NoBell {
    fn at_site(
        &mut self,
        mode: FlushStateMode,
        host: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal> {
        self.0.at_site(mode, host)
    }

    fn after_step(&mut self, decided: SiteDecision, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.after_step(decided, host)
    }

    fn service(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.service(host)
    }

    fn answer_due(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.answer_due(host)
    }

    fn move_prune_locks_back(&mut self, height: u32) {
        self.0.move_prune_locks_back(height);
    }

    fn flush(
        &mut self,
        mode: FlushStateMode,
        manual_prune_height: Option<u32>,
        host: &mut FlushHost<'_>,
    ) -> Result<(), Fatal> {
        self.0.flush(mode, manual_prune_height, host)
    }

    fn coins_stats(
        &mut self,
        hash_type: CoinStatsHashType,
        interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        dump: Option<CoinsDump>,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<CoinsStatsJob>, Fatal> {
        self.0.coins_stats(hash_type, interrupted, dump, host)
    }

    fn coins_scan(
        &mut self,
        scan: CoinsScan,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<CoinsScanJob>, Fatal> {
        self.0.coins_scan(scan, host)
    }

    fn prune_and_flush(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.prune_and_flush(host)
    }

    fn stop(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.stop(host)
    }

    fn abort(&mut self, cause: AbortCause, host: &mut FlushHost<'_>) -> bool {
        self.0.abort(cause, host)
    }

    fn cache_usage(&self) -> CacheUsage {
        self.0.cache_usage()
    }

    fn admit(&self) -> Result<(), Fatal> {
        self.0.admit()
    }
}

impl Stores {
    pub(super) fn new() -> Stores {
        Self::build(false)
    }

    /// The node's chainstate engine, where a test fails its reads ([`ProbeEngine::fail_reads`]).
    pub(super) fn db(&self) -> Arc<GuardedEngine<ProbeEngine>> {
        self.db.clone()
    }

    /// A node under `-prune` and `-fastprune`.
    fn pruned() -> Stores {
        Self::build(true)
    }

    fn build(prune: bool) -> Stores {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::DirBuilder::new().mode(0o700).create(dir.path().join("blocks")).expect("dir");
        let fs = Arc::new(TestFs::new());
        let latch = GlobalFailStop::new();
        let sink = Arc::new(RecordingAbort::default());
        let held = Arc::new(HeldSink::new(sink.clone()));
        let db = Arc::new(GuardedEngine::new(
            ProbeEngine::new(&CHAINSTATE_SCHEMA),
            latch.clone(),
            held.clone(),
        ));
        let budget = CoinsBudget::new(8 << 20, 0, Ratio::HALF, 0);
        let coins = Arc::new(Mutex::new(CoinsCache::new(db.clone(), budget, Salt::fixed(7), None)));
        let files = files(&dir, &fs, &latch, BlockFilesState::default(), prune);
        Stores {
            p: regtest(),
            dir,
            fs,
            latch,
            sink,
            held,
            db,
            coins,
            files,
            hub: Arc::new(SubscriberHub::new()),
            prune,
            countdown: Arc::default(),
        }
    }

    /// The prune side over these files: `-prune`'s `target` bytes (0 for manual only), at regtest's
    /// retention, from height 0, its automatic check off until `on`.
    fn prune_host(&self, target: u64, on: &Arc<AtomicBool>) -> Box<dyn PruneHost + Send> {
        Box::new(Switched {
            on: on.clone(),
            countdown: self.countdown.clone(),
            host: FilesPruneHost {
                files: self.files.clone(),
                target,
                prune_after_height: 0,
                min_blocks_to_keep: super::support::regtest_keep(),
            },
        })
    }

    fn blocks_dir(&self) -> PathBuf {
        self.dir.path().join("blocks")
    }

    fn path(&self) -> AbortPath {
        AbortPath::new(self.sink.clone(), self.latch.clone())
    }

    fn block_files(&self) -> NodeBlockFiles<TestFs> {
        NodeBlockFiles::new(self.files.clone(), pow(&self.p), Arc::new(self.path()))
    }

    /// The node's V seams over these stores: CH3a's connector, M2's block files and M1's flush
    /// on W's thread.
    fn config_with(&self, seams: Seams) -> RigConfig {
        self.config_with_hooks(seams, NoEvoHooks, Arc::new(()))
    }

    /// [`Self::config_with`], the connector's evo side `hooks` over `facade`.
    pub(super) fn config_with_hooks<H: EvoHooks + Send + 'static>(
        &self,
        seams: Seams,
        hooks: H,
        facade: Arc<H::Facade>,
    ) -> RigConfig {
        let Seams {
            crash,
            on_crash,
            prune,
            obs,
            synchronous,
            tuning,
            reindex,
            live_copy,
            parker,
            after_release,
            #[cfg(feature = "test-support")]
            tally,
        } = seams;
        let parts = ConnectorParts {
            coins: self.coins.clone(),
            files: self.files.clone(),
            queue: Arc::new(CheckQueue::serial()),
            caches: Arc::new(ScriptCaches::new(CacheSizes::default()).expect("caches")),
            helper: Box::new(NodeHelper),
            abort: self.path(),
            assume_valid: None,
            digest: None,
            xcheck: false,
        };
        let connector = CoreConnector::new(&self.p, parts, hooks, facade).expect("connector");
        let driver = ThreadDriver::spawn(WThread {
            engine: self.db.clone(),
            files: self.files.clone(),
            fs: self.fs.clone(),
            hub: self.hub.clone(),
            obs: obs.clone(),
            tuning,
            cfg: FlushConfig {
                sub_batch: 4 << 10,
                data_dir: self.dir.path().to_path_buf(),
                blocks_dir: self.blocks_dir(),
                live_copy,
            },
            covered_seq: 0,
            crash,
            on_crash,
            after_release,
        })
        .expect("W's thread");
        #[cfg_attr(not(feature = "test-support"), allow(unused_mut))]
        let mut flush = NodeFlush::new(NodeFlushParts {
            coins: self.coins.clone(),
            driver,
            policy: FlushPolicy::new(synchronous),
            ledger: Arc::default(),
            hub: self.hub.clone(),
            obs,
            evo: Box::new(NoEvo),
            prune,
            inputs: if reindex {
                Box::new(Reindexing)
            } else {
                Box::new(NoInputs)
            },
        });
        #[cfg(feature = "test-support")]
        if let Some(tally) = tally {
            flush.set_tally(tally);
        }
        let flush: Box<dyn FlushSite> = if parker {
            Box::new(NoBell(flush))
        } else {
            Box::new(flush)
        };
        RigConfig {
            params: Some(self.p.clone()),
            files: Some(Box::new(self.block_files())),
            connector: Some(Box::new(connector)),
            flush: Some(flush),
            abort: Some(self.sink.clone()),
            ..RigConfig::default()
        }
    }

    /// The block files a restart opens: ST2's from `state`, under a new process's latch and file
    /// handles.
    fn reopened(&self, state: BlockFilesState) -> NodeBlockFiles<TestFs> {
        let latch = GlobalFailStop::new();
        let files = files(&self.dir, &Arc::new(TestFs::new()), &latch, state, self.prune);
        NodeBlockFiles::new(files, pow(&self.p), Arc::new(AbortPath::new(self.sink.clone(), latch)))
    }
}

fn files(
    dir: &tempfile::TempDir,
    fs: &Arc<TestFs>,
    latch: &GlobalFailStop,
    state: BlockFilesState,
    prune: bool,
) -> Arc<GuardedFiles<TestFs>> {
    let mut cfg = BlockFilesConfig::new(dir.path().join("blocks"), MAGIC);
    cfg.fastprune = prune;
    cfg.prune_mode = prune;
    Arc::new(GuardedFiles::new(StoreFiles::with_fs(fs.clone(), cfg, state), latch.clone()))
}

fn pow(p: &ChainSetup) -> PowParams {
    *HeaderParams::from_chain_params(p).expect("regtest").pow()
}

/// A block whose header fails regtest's proof of work: `ReadBlockFromDisk`'s
/// `CheckProofOfWork`.
fn unmined(p: &ChainSetup, parent: &Mined) -> Arc<Block> {
    let txs = vec![coinbase(&cb_script(parent.height + 1, 9))];
    let mut raw = {
        let base = BlockHeader::default();
        BlockHeader::new(
            0x2000_0000,
            parent.hash(),
            base.merkle_root(),
            parent.time() + 1,
            REGTEST_BITS,
            base.nonce(),
        )
    };
    raw.set_merkle_root(Block::new(Header::new(raw), txs.clone()).merkle().0);
    let pow = pow(p);
    let header = (0..)
        .map(|nonce| {
            Header::new(BlockHeader::new(
                raw.version(),
                raw.prev_hash(),
                raw.merkle_root(),
                raw.time(),
                raw.bits(),
                nonce,
            ))
        })
        .find(|h| !check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), &pow))
        .expect("a nonce");
    Arc::new(Block::new(header, txs))
}

fn save(files: &mut dyn BlockFiles, m: &Mined) -> FlatFilePos {
    files.save_block(&m.block, m.height, None).expect("no error").expect("a position")
}

/// M2: what `SaveBlockToDisk` wrote reads back, at Core's positions (the record's 8-byte header
/// first), and its file opens. Another file does not open, and logs `FlatFileSeq::Open`'s line;
/// Core's null position opens nothing, silently.
#[test]
fn saved_blocks_read_back_and_their_file_opens() {
    install_accept_log();
    let s = Stores::new();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    let chain = branch(&s.p, &genesis, 2, 0);
    let at = save(&mut files, &genesis);
    assert_eq!(
        at,
        FlatFilePos {
            file: 0,
            pos: 8
        }
    );
    for m in &chain {
        save(&mut files, m);
    }
    let again = files.read_block(at, &genesis.hash()).expect("reads");
    assert_eq!(again.hash(), genesis.hash());
    assert!(files.open_block_file(0));
    let missing = s.blocks_dir().join("blk00001.dat");
    let line = format!("Unable to open file {}", missing.display());
    assert!(lines_with(&line).is_empty());
    assert!(!files.open_block_file(1));
    assert_eq!(lines_with(&line), std::slice::from_ref(&line));
    assert!(!files.open_block_file(-1));
    assert_eq!(lines_with(&line).len(), 1);
    assert!(s.sink.messages().is_empty());
}

/// M2: a known position (`fKnown`, a reindex) is Core's `FindBlockPos` with nothing written.
#[test]
fn a_known_position_writes_nothing() {
    let s = Stores::new();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    let at = save(&mut files, &genesis);
    let blk = s.blocks_dir().join("blk00000.dat");
    let before = std::fs::read(&blk).expect("blk");
    let known = files.save_block(&genesis.block, 0, Some(at)).expect("no error");
    assert_eq!(known, Some(at));
    assert_eq!(std::fs::read(&blk).expect("blk"), before);
    let refused = files.save_block(
        &genesis.block,
        0,
        Some(FlatFilePos {
            file: -1,
            pos: 0,
        }),
    );
    assert_eq!(
        refused,
        Err("a known position in no file: FlatFilePos(nFile=-1, nPos=0)".to_owned())
    );
}

/// M2: `ReadBlockFromDisk`'s lines: the open, the deserialization, the proof of work, then the
/// hash against the index's.
#[test]
fn read_failures_are_cores_lines() {
    install_accept_log();
    let s = Stores::new();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    let at = save(&mut files, &genesis);
    let other = BlockHash::from_hex(&"07".repeat(32)).expect("hex");
    let wrong = files.read_block(at, &other).err().expect("another hash");
    assert_eq!(
        wrong,
        format!(
            "ReadBlockFromDisk(CBlock&, CBlockIndex*): GetHash() doesn't match index for {other} \
             at FlatFilePos(nFile=0, nPos=8)"
        )
    );
    let missing = FlatFilePos {
        file: 3,
        pos: 8,
    };
    let open = files.read_block(missing, &genesis.hash()).err().expect("no file");
    assert_eq!(open, "ReadBlockFromDisk: OpenBlockFile failed for FlatFilePos(nFile=3, nPos=8)");
    let line = format!("Unable to open file {}", s.blocks_dir().join("blk00003.dat").display());
    assert_eq!(levels_here(&line), [Level::INFO]);
    let null = FlatFilePos {
        file: -1,
        pos: 8,
    };
    let none = files.read_block(null, &genesis.hash()).err().expect("Core's null file");
    assert_eq!(none, "ReadBlockFromDisk: OpenBlockFile failed for FlatFilePos(nFile=-1, nPos=8)");
    let bad = Mined {
        block: unmined(&s.p, &genesis),
        height: 1,
    };
    let bad_at = save(&mut files, &bad);
    let header = files.read_block(bad_at, &bad.hash()).err().expect("no work");
    assert_eq!(header, format!("ReadBlockFromDisk: Errors in block header at {}", fmt(bad_at)));
    // A record whose bytes do not decode: genesis's transaction count made a 9-byte
    // `CompactSize` past the record's end.
    let blk = s.blocks_dir().join("blk00000.dat");
    let mut bytes = std::fs::read(&blk).expect("blk");
    bytes[at.pos as usize + 80] = 0xff;
    std::fs::write(&blk, bytes).expect("blk");
    let decode = files.read_block(at, &genesis.hash()).err().expect("no block");
    assert!(decode.starts_with("ReadBlockFromDisk: Deserialize or I/O error - "), "{decode}");
    assert!(decode.ends_with(&format!(" at {}", fmt(at))), "{decode}");
    assert!(s.sink.messages().is_empty());
}

fn fmt(p: FlatFilePos) -> String {
    format!("FlatFilePos(nFile={}, nPos={})", p.file, p.pos)
}

/// V's reading of the latch names the files' one report (CH3b D221): the guard latched the text
/// the files reported.
fn assert_the_latch_names_the_report(s: &Stores) {
    let latched = s.latch.tripped().expect("latched").clone();
    assert_eq!(s.sink.messages(), [crate::flush::abort_text(latched)]);
}

/// M2 (D62): with too little space Core's `FindBlockPos` aborts the node, `SaveBlockToDisk`
/// logs its line and returns the null position, which `AcceptBlock` turns into its error. The
/// files then refuse writes, silently: the latch holds the report already made.
#[test]
fn too_little_space_aborts_once_then_saves_nothing() {
    install_accept_log();
    let s = Stores::new();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    s.fs.free.store(0, Ordering::SeqCst);
    assert_eq!(files.save_block(&genesis.block, 0, None), Ok(None));
    let low = "Disk space is too low!".to_owned();
    assert_eq!(s.sink.reports(), [(low.clone(), Some(low))]);
    assert_eq!(levels_here("SaveBlockToDisk: FindBlockPos failed"), [Level::ERROR]);
    assert_eq!(s.latch.tripped().map(|f| f.kind), Some(ErrorKind::NoSpace));
    assert_the_latch_names_the_report(&s);
    s.fs.free.store(u64::MAX, Ordering::SeqCst);
    assert_eq!(files.save_block(&genesis.block, 0, None), Ok(None));
    assert_eq!(s.sink.reports().len(), 1, "the latch's failure is reported once");
}

/// M2 (D62): a failed write is Core's `WriteBlockToDisk` open failure, `SaveBlockToDisk`'s
/// "Failed to write block".
#[test]
fn a_failed_write_is_failed_to_write_block() {
    install_accept_log();
    let s = Stores::new();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    s.fs.fail_write.store(true, Ordering::SeqCst);
    assert_eq!(files.save_block(&genesis.block, 0, None), Ok(None));
    assert_eq!(s.sink.reports(), [("Failed to write block".to_owned(), None)]);
    assert_eq!(levels_here("WriteBlockToDisk: OpenBlockFile failed"), [Level::ERROR]);
    assert_eq!(s.latch.tripped().map(|f| f.site), Some("block files write"));
    assert_the_latch_names_the_report(&s);
}

/// M2 (D62): a blk file `Allocate` cannot create is its `Open`'s failure, which only logs, and
/// the write's open then fails: Core's two open lines, `WriteBlockToDisk`'s, then "Failed to
/// write block".
#[test]
fn a_blk_file_that_cannot_be_created_fails_the_write() {
    install_accept_log();
    let s = Stores::new();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    s.fs.fail_create.store(true, Ordering::SeqCst);
    assert_eq!(files.save_block(&genesis.block, 0, None), Ok(None));
    assert_eq!(s.sink.reports(), [("Failed to write block".to_owned(), None)]);
    let open = format!("Unable to open file {}", s.blocks_dir().join("blk00000.dat").display());
    assert_eq!(levels_here(&open), [Level::INFO, Level::INFO]);
    assert_eq!(levels_here("WriteBlockToDisk: OpenBlockFile failed"), [Level::ERROR]);
    assert_the_latch_names_the_report(&s);
}

/// M2 (D62, Opus r1 m-1): a space query that fails is an exception in `FindBlockPos`, which
/// `AcceptBlock` catches as "System error: …" (`v:4843-4844`), not the null position's
/// "Failed to find position". The latch holds it, so the files report it and return `Err`.
#[test]
fn a_failed_space_query_is_a_system_error() {
    let s = Stores::new();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    s.fs.fail_space.store(true, Ordering::SeqCst);
    let what = files.save_block(&genesis.block, 0, None).expect_err("Core's exception");
    assert_eq!(what, "injected space query failure");
    assert_eq!(s.sink.reports(), [(format!("System error: {what}"), None)]);
    assert_eq!(s.latch.tripped().map(|f| f.site), Some("block files write"));
    assert_the_latch_names_the_report(&s);
}

/// D62 through V: the "System error" stops V on its `Err` path, reported once (by the files: V's
/// stop sees the latch). `-fastprune`'s 16 KiB chunks make a later block's write query the
/// space.
#[test]
fn v_stops_on_a_system_error_reported_once() {
    let s = Stores::pruned();
    let (mut rig, _) = node(&s, 0);
    let chain = padded_branch(&rig.p, &rig.genesis, 40);
    rig.accept(&chain[0]);
    s.fs.fail_space.store(true, Ordering::SeqCst);
    let stopped = chain[1..].iter().any(|m| {
        let r = rig.v.process_block(m.block.clone(), true, None, true).recv();
        matches!(r, Err(WaitError::Disconnected))
    });
    assert!(stopped, "a block's write queried the space");
    assert!(rig.stop().fatal.is_some());
    assert_eq!(s.sink.messages(), ["System error: injected space query failure"]);
}

/// M2 (D62, Opus r1 m-5): a blk file's flush at its rollover that fails is `FlushBlockFile`'s
/// `AbortNode` (`bs:602-604` at v24.0.0); ST2 then stops at its latch (D62 difference (2)).
#[test]
fn a_failed_flush_at_a_rollover_is_cores_abort() {
    let s = Stores::pruned();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    save(&mut files, &genesis);
    s.fs.fail_sync.store(true, Ordering::SeqCst);
    let chain = padded_branch(&s.p, &genesis, 60);
    let saved = chain.iter().take_while(|m| files.save_block(&m.block, m.height, None) != Ok(None));
    let saved = saved.count();
    assert!((20..60).contains(&saved), "file 0 filled first: {saved}");
    let flush = "Flushing block file to disk failed. This is likely the result of an I/O error.";
    assert_eq!(s.sink.reports(), [(flush.to_owned(), None)]);
    assert_the_latch_names_the_report(&s);
    assert!(!s.blocks_dir().join("blk00001.dat").exists(), "nothing written past the latch");
}

/// A fault in the node's block files before V connects blocks 1 to 3 from them.
#[derive(Clone, Copy, Debug)]
enum ReadFault {
    None,
    /// Block 2's stored header fails its proof of work: zero `nBits`.
    Pow,
    /// No blk file opens.
    Open,
}

/// What [`connect_from_disk`] left.
#[derive(Debug, PartialEq, Eq)]
struct DiskRun {
    aborts: Vec<String>,
    tip: BlockHash,
    /// The lines, from any thread, naming the stores' blk file.
    blk_lines: usize,
    /// V's `ReadBlockFromDisk` lines.
    reads: Vec<String>,
}

/// CH4-4 over the node's files: blocks 1 to 3 arrive as headers, then 3, 2 and 1, so V
/// connects all three from disk, through `BlockRead` on the reader's thread when the
/// read-ahead runs.
fn connect_from_disk(fault: ReadFault, read_ahead: ReadAheadSetup) -> DiskRun {
    install_accept_log();
    let s = Stores::new();
    let mut cfg = s.config_with(Seams::default());
    cfg.read_ahead = read_ahead;
    let mut rig = Rig::with(cfg);
    let chain = branch(&rig.p, &rig.genesis, 3, 0);
    rig.headers(&chain);
    for m in chain[1..].iter().rev() {
        rig.accept(m);
    }
    match fault {
        ReadFault::None => {}
        ReadFault::Pow => {
            use std::os::unix::fs::FileExt as _;
            let at = rig.v.reader().entry(rig.id(&chain[1])).expect("stored").data_pos;
            let blk =
                std::fs::OpenOptions::new().write(true).open(s.blocks_dir().join("blk00000.dat"));
            // The header's `nBits`, 72 bytes in.
            blk.expect("blk").write_all_at(&[0; 4], u64::from(at) + 72).expect("written");
        }
        ReadFault::Open => s.fs.fail_open.store(true, Ordering::SeqCst),
    }
    let v = rig.v_thread();
    if matches!(fault, ReadFault::None) {
        rig.accept(&chain[0]);
    } else {
        rig.submit_stopping(&chain[0], &format!("{fault:?}"));
    }
    let tip = rig.v.reader().entry(tip(&rig)).expect("the tip").hash;
    let exit = rig.stop();
    assert_eq!(exit.fatal.is_some(), !matches!(fault, ReadFault::None), "{fault:?}");
    let blk = s.blocks_dir().join("blk00000.dat").display().to_string();
    DiskRun {
        aborts: s.sink.messages(),
        tip,
        blk_lines: lines_with(&blk).len(),
        reads: lines_on(v).into_iter().filter(|l| l.contains("ReadBlockFromDisk")).collect(),
    }
}

/// CH4-4 (validator r1 #1): the node's own reads under the read-ahead, which `BlockRead` serves
/// silently. Genesis at V's startup and blocks 1 to 3 are read ahead once each. A header that
/// fails its proof of work at block 2, and a blk file that does not open at block 1, are V's
/// own `AbortNode("Failed to read block")` at that block, as without the read-ahead, with V's
/// lines alone: one `ReadBlockFromDisk` line, and for the file `FlatFileSeq::Open`'s line once.
#[test]
fn the_read_ahead_over_the_node_files_fails_where_v_does() {
    let p = regtest();
    let genesis = Mined::genesis(&p);
    let block_1 = branch(&p, &genesis, 1, 0)[0].hash();
    for fault in [ReadFault::None, ReadFault::Pow, ReadFault::Open] {
        let off = connect_from_disk(fault, ReadAheadSetup::off());
        let probe = Probe::settled();
        let ahead = connect_from_disk(fault, ReadAheadSetup::threaded(false).probed(&probe));
        assert_eq!(ahead, off, "{fault:?}");
        let opened = usize::from(matches!(fault, ReadFault::Open));
        assert_eq!(off.blk_lines, opened, "{fault:?}: FlatFileSeq::Open's line");
        let c = probe.counts();
        assert_eq!(c.reads, 4, "{fault:?}");
        let (tip, hits) = match fault {
            ReadFault::None => {
                assert!(off.aborts.is_empty() && off.reads.is_empty(), "{off:?}");
                assert_eq!((c.hits, c.fallbacks), (4, 0));
                continue;
            }
            ReadFault::Pow => (block_1, 2),
            ReadFault::Open => (genesis.hash(), 1),
        };
        assert_eq!(off.aborts, ["Failed to read block"], "{fault:?}");
        assert_eq!(off.tip, tip, "{fault:?}");
        assert_eq!(off.reads.len(), 1, "{fault:?}: {:?}", off.reads);
        assert_eq!((c.hits, c.fallbacks), (hits, 1), "{fault:?}");
    }
}

/// A rig on `s`'s stores with `n` blocks connected past genesis.
fn node(s: &Stores, n: u32) -> (Rig, Vec<Mined>) {
    node_with(s, n, Seams::default())
}

fn node_with(s: &Stores, n: u32, seams: Seams) -> (Rig, Vec<Mined>) {
    let rig = Rig::with(s.config_with(seams));
    let chain = branch(&rig.p, &rig.genesis, n, 0);
    for m in &chain {
        rig.accept(m);
    }
    (rig, chain)
}

/// M1: V's ALWAYS flush through the adapter writes the coins at V's tip, through W's thread, and
/// returns once they are durable (Core's `ForceFlushStateToDisk`).
#[test]
fn an_always_flush_makes_the_tip_durable() {
    let s = Stores::new();
    let (rig, chain) = node(&s, 3);
    let tip = chain.last().expect("a tip").hash();
    assert_ne!(durable_tip(&s), Some(tip));
    rig.on_v(|v| v.flush_now(FlushStateMode::Always));
    assert_eq!(durable_tip(&s), Some(tip));
    rig.assert_no_abort();
}

#[test]
fn tip_step_flush_signals_name_the_durable_post_step_tip() {
    struct Flushed {
        stores: Arc<Stores>,
        tips: Mutex<Vec<(BlockHash, Option<BlockHash>)>>,
        tip_step: AtomicBool,
    }
    impl ValidationInterface for Flushed {
        fn chain_state_flushed(&self, locator: &[BlockHash]) {
            if !self.tip_step.load(Ordering::SeqCst) {
                return;
            }
            // CRASH checks the handoff ordering ("never leads"); this callback checks values.
            let tip = locator[0];
            self.tips.lock().expect("tips").push((tip, stored_tip(&self.stores)));
        }

        fn block_connected(&self, _: &Arc<Block>, _: BlockId) {
            self.tip_step.store(false, Ordering::SeqCst);
        }

        fn block_disconnected(&self, _: &Arc<Block>, _: BlockId) {
            self.tip_step.store(false, Ordering::SeqCst);
        }
    }
    let s = Arc::new(Stores::new());
    let (rig, _) = node_with(
        &s,
        0,
        Seams {
            synchronous: true,
            ..Seams::default()
        },
    );
    // Register after startup; BlockConnected/BlockDisconnected closes each tip-step window
    // before ABC's PERIODIC flush. Drain each window before the next step or shutdown.
    rig.on_v(|v| v.flush_now(FlushStateMode::Always));
    rig.sync();
    let flushed = Arc::new(Flushed {
        stores: s.clone(),
        tips: Mutex::default(),
        tip_step: AtomicBool::new(true),
    });
    rig.signals.register(flushed.clone());
    s.coins.lock().expect("coins").resize(1, false);
    assert_eq!(
        s.coins.lock().expect("coins").boundary_state(0, false),
        crate::coins::CoinsBoundary::Critical
    );
    let chain = branch(&rig.p, &rig.genesis, 2, 0);
    rig.accept(&chain[0]);
    rig.sync();
    flushed.tip_step.store(true, Ordering::SeqCst);
    rig.accept(&chain[1]);
    rig.sync();
    flushed.tip_step.store(true, Ordering::SeqCst);
    assert_eq!(rig.v.invalidate_block(chain[1].hash()).recv(), Ok(Ok(())));
    rig.sync();
    // Core v24.0.0: validation.cpp:3105, :3123-3126
    let tips = flushed.tips.lock().expect("tips");
    assert_eq!(
        tips.iter().map(|(tip, _)| *tip).collect::<Vec<_>>(),
        [chain[0].hash(), chain[1].hash(), chain[0].hash()]
    );
    for (tip, durable) in tips.iter() {
        assert_eq!(Some(*tip), *durable, "the durable tip when ChainStateFlushed was recorded");
    }
    rig.assert_no_abort();
}

/// RP1a D11 (DEC-618 M-1, m-2): `gettxoutsetinfo`'s scan reads a copy point. V's ALWAYS flush
/// runs the engine's hook before its release (E-b1.8), so the hook has run when V answers; the
/// scan's snapshot is at that flush, V's tip with the cache's coins; and a block connected and
/// flushed during the walk does not show through it.
// Core: rpc/blockchain.cpp:1235-1247, :1276 at v24.0.0 (ForceFlushStateToDisk, then the scan of
// CoinsDB())
#[test]
fn the_coins_scan_reads_a_copy_point_at_vs_tip() {
    let s = Stores::new();
    let tuning = Arc::new(HookTuning {
        delay: Duration::from_millis(100),
        ..HookTuning::default()
    });
    let seams = Seams {
        tuning: tuning.clone(),
        ..Seams::default()
    };
    let (rig, chain) = node_with(&s, 3, seams);
    let tip = chain.last().expect("a tip");
    assert_ne!(durable_tip(&s), Some(tip.hash()), "the cache holds the blocks' coins");
    let hooks = tuning.hooks.load(Ordering::SeqCst);
    let go = Arc::new(AtomicBool::new(false));
    let walk = go.clone();
    // The walk waits at its first coin until the test has moved the database on.
    let held: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        while !walk.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    });
    let hash_type = CoinStatsHashType::HashSerialized;
    let job = rig.v.coins_stats(hash_type, held, None).recv().expect("V answers").expect("a scan");
    assert_eq!(tuning.hooks.load(Ordering::SeqCst), hooks + 1, "the hook ran before the release");
    assert_eq!(durable_tip(&s), Some(tip.hash()));
    assert_eq!(job.snapshot_epoch(), Some(s.db.write_epoch()), "nothing written since");
    let scan_now = || {
        let job =
            start_coins_stats(s.db.clone(), hash_type, Arc::new(|| false), None).expect("a thread");
        job.wait().expect("the database's coins")
    };
    let at_flush = scan_now();

    let next = &branch(&rig.p, tip, 1, 0)[0];
    rig.accept(next);
    assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
    assert_eq!(durable_tip(&s), Some(next.hash()));
    go.store(true, Ordering::SeqCst);
    let stats = job.wait().expect("the scan");
    // `disk_size` is the live estimate (Core's `EstimateSize()`), not the snapshot's.
    assert_eq!((stats.best_block, &stats.stats), (tip.hash(), &at_flush.stats));
    assert_eq!(at_flush.best_block, tip.hash());
    let after = scan_now();
    assert_eq!(after.best_block, next.hash());
    assert_ne!(after.stats, stats.stats);
    rig.assert_no_abort();
}

/// RP1b-1a: `scantxoutset`'s walk reads a copy point too, as `gettxoutsetinfo`'s scan does:
/// the hook has run when V answers, the snapshot is at that flush and V's tip, and a block
/// connected and flushed after it does not show through it.
// Core: rpc/blockchain.cpp:2767-2774 at v24.0.0 (ForceFlushStateToDisk, then CoinsDB().Cursor())
#[test]
fn the_coins_walk_reads_a_copy_point_at_vs_tip() {
    let s = Stores::new();
    let tuning = Arc::new(HookTuning {
        delay: Duration::from_millis(100),
        ..HookTuning::default()
    });
    let seams = Seams {
        tuning: tuning.clone(),
        ..Seams::default()
    };
    let (rig, chain) = node_with(&s, 3, seams);
    let tip = chain.last().expect("a tip");
    assert_ne!(durable_tip(&s), Some(tip.hash()), "the cache holds the blocks' coins");
    let hooks = tuning.hooks.load(Ordering::SeqCst);
    let walk = || CoinsScan {
        needles: HashSet::new(),
        progress: Arc::default(),
        should_abort: Arc::default(),
        interrupted: Arc::new(|| false),
    };
    let job = rig.v.coins_scan(walk()).recv().expect("V answers").expect("a walk");
    assert_eq!(tuning.hooks.load(Ordering::SeqCst), hooks + 1, "the hook ran before the release");
    assert_eq!(durable_tip(&s), Some(tip.hash()));
    assert_eq!(job.snapshot_epoch(), Some(s.db.write_epoch()), "nothing written since");
    let walk_now =
        || start_coins_scan(s.db.clone(), walk()).expect("a thread").wait().expect("a walk");
    let at_flush = walk_now();

    let next = &branch(&rig.p, tip, 1, 0)[0];
    rig.accept(next);
    assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
    assert_eq!(durable_tip(&s), Some(next.hash()));
    let r = job.wait().expect("the walk");
    assert!(r.success);
    assert_eq!((r.best_block, r.count), (tip.hash(), at_flush.count));
    assert!(r.count > 0);
    let after = walk_now();
    assert_eq!(after.best_block, next.hash());
    assert_ne!(after.count, r.count);
    rig.assert_no_abort();
}

/// `f`'s result on a thread of its own, within `bound`, so a wait that never ends fails the test.
fn within<T: Send + 'static>(bound: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(bound).expect("the wait ends")
}

/// The coins cache's last released tip.
fn durable_tip(s: &Stores) -> Option<BlockHash> {
    s.coins.lock().expect("coins").durable().map(|b| b.hash)
}

/// The physical chainstate meta/TIP a process restart reads.
fn stored_tip(s: &Stores) -> Option<BlockHash> {
    s.db.inner()
        .unprobed()
        .get(dashd_store::DbSchema::META, TIP)
        .expect("reads")
        .map(|b| BlockHash::from(<[u8; 32]>::try_from(b.as_slice()).expect("a tip")))
}

/// `m`'s stored index record, which a restart loads, if it has one.
fn durable_record(s: &Stores, m: &Mined) -> Option<BlockIndexRecord> {
    let cf = CHAINSTATE_SCHEMA.cf("block_index").expect("the schema has it");
    let bytes = s.db.inner().unprobed().get(cf, m.hash().as_bytes()).expect("reads");
    bytes.map(|b| BlockIndexRecord::decode(&b).expect("a record"))
}

/// `m`'s stored index record's status, which a restart loads, if it has one.
fn durable_status(s: &Stores, m: &Mined) -> Option<Status> {
    durable_record(s, m).map(|r| r.status)
}

/// Whether `m`'s stored index record is connected (`VALID_SCRIPTS`), if it has one.
fn durable_connected(s: &Stores, m: &Mined) -> Option<bool> {
    durable_status(s, m).map(|st| st.is_valid(Status::VALID_SCRIPTS))
}

/// DEC-525: V's `Flush` command in each mode Core's `FLUSH` op serves, after an ALWAYS flush and
/// one more block, then a stop, which writes nothing here (the node's shutdown flushes run on its
/// `VExit`, BL-271), so a kill's restart loads the same.
/// ALWAYS makes the new tip and its connected index entry durable. IF_NEEDED with a small cache,
/// and PERIODIC within the hour after the first ALWAYS flush (which restarts both timers), write
/// nothing: the restart loads the earlier tip and no entry for the block. With nothing due, the
/// two decide as NONE would; the mode V's flush side receives is
/// `the_flush_command_passes_its_mode_at_vs_tip`'s.
// Core: src/validation.cpp:3082-3106 (the conditions), :3116-3171 (the writes; the coins
// :3146-3171, both timers :3144, :3170) at the pin
#[test]
fn each_flush_mode_leaves_cores_durable_tip_and_index() {
    for (mode, writes) in [
        (FlushStateMode::Always, true),
        (FlushStateMode::IfNeeded, false),
        (FlushStateMode::Periodic, false),
    ] {
        let s = Stores::new();
        let (mut rig, chain) = node(&s, 2);
        assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
        let next = &branch(&rig.p, &chain[1], 1, 0)[0];
        rig.accept(next);
        assert_eq!(rig.v.flush(mode).recv(), Ok(()), "{mode:?}");
        let exit = rig.stop();
        assert!(exit.fatal.is_none(), "{mode:?}: {:?}", exit.fatal);
        let durable = if writes {
            next
        } else {
            &chain[1]
        };
        assert_eq!(durable_tip(&s), Some(durable.hash()), "{mode:?}");
        assert_eq!(durable_connected(&s, &chain[1]), Some(true), "{mode:?}");
        assert_eq!(durable_connected(&s, next), writes.then_some(true), "{mode:?}");
    }
}

/// The first flush in [`a_chainlock_mark_is_stored_as_cores_dirt_stores_it`]'s rows, after the
/// connects and before the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FirstFlush {
    None,
    Always,
    LeftIbd,
    Threshold,
    Dmax,
    CacheCritical,
}

/// Shrinks the coins share until V's check at IF_NEEDED finds `target`; the old share.
fn coins_at(s: &Stores, target: CoinsBoundary) -> u64 {
    let mut coins = s.coins.lock().expect("coins");
    let old = coins.budget().coins_share();
    let mut share = old;
    while coins.boundary_state(0, false) != target {
        assert!(share > 1, "the cache reaches {target:?}");
        share -= share / 16 + 1;
        coins.resize(share, false);
    }
    old
}

/// CONS-11 case 2 on V's stores (DEC-525) and CH3b D242 (BL-813): `MarkConflictingBlock`'s marks
/// set no dirty entry, so a flush writes a mark only through an entry Core still holds dirty.
/// With no flush between the connects and the lock, the ALWAYS flush after the lock stores the
/// marked main blocks with `CONFLICT_CHAINLOCK`. With an ALWAYS flush there, as `l1_chain_locks`
/// runs the case, Core's take cleaned them and it stores them without it. With a flush Core does
/// not make there (LEFT_IBD, THRESHOLD, DMAX, the native CACHE_CRITICAL), Core still holds them
/// dirty, and so does the node: the mark is stored. The locked block, connected since, is stored
/// without it either way.
// Core: src/validation.cpp:4237-4246 (the marks), :3353-3420 (DisconnectTip) at the pin
#[test]
fn a_chainlock_mark_is_stored_as_cores_dirt_stores_it() {
    use FirstFlush as F;
    for first in [F::Always, F::None, F::LeftIbd, F::Threshold, F::Dmax, F::CacheCritical] {
        let s = Stores::new();
        #[cfg(feature = "test-support")]
        let tally = Arc::new(crate::flush::FlushTally::default());
        let mut rig = if first == F::LeftIbd {
            let seams = Seams {
                #[cfg(feature = "test-support")]
                tally: Some(tally.clone()),
                ..Seams::default()
            };
            importing_rig_with(&s, true, seams, None)
        } else {
            // Each first flush is waited for (`-dbcrashratio`'s synchronous policy), so the
            // freeze has landed before the lock.
            let seams = Seams {
                synchronous: true,
                #[cfg(feature = "test-support")]
                tally: Some(tally.clone()),
                ..Seams::default()
            };
            Rig::with(RigConfig {
                chainlocks: true,
                ..s.config_with(seams)
            })
        };
        let main = branch(&rig.p, &rig.genesis, 2, 0);
        let side = branch(&rig.p, &rig.genesis, 1, 1);
        for m in main.iter().chain(&side) {
            rig.accept(m);
        }
        let mut share = None;
        let mode = match first {
            F::None => None,
            F::Always => Some(FlushStateMode::Always),
            F::LeftIbd => {
                done_importing(&rig);
                Some(FlushStateMode::None)
            }
            F::Threshold => {
                share = Some(coins_at(&s, CoinsBoundary::Threshold));
                Some(FlushStateMode::IfNeeded)
            }
            F::Dmax => {
                s.hub.post_flush_due();
                Some(FlushStateMode::None)
            }
            F::CacheCritical => {
                share = Some(coins_at(&s, CoinsBoundary::Critical));
                Some(FlushStateMode::IfNeeded)
            }
        };
        #[cfg(feature = "test-support")]
        tally.begin_op();
        if let Some(mode) = mode {
            assert_eq!(rig.v.flush(mode).recv(), Ok(()), "{first:?}");
            assert_eq!(durable_tip(&s), Some(main[1].hash()), "{first:?}: the first flush");
        }
        #[cfg(feature = "test-support")]
        {
            let cause = match first {
                F::None => None,
                F::Always => Some("ALWAYS"),
                F::LeftIbd => Some("LEFT_IBD"),
                F::Threshold => Some("THRESHOLD"),
                F::Dmax => Some("DMAX"),
                F::CacheCritical => Some("CACHE_CRITICAL"),
            };
            let want = cause.map(|c| (c.to_owned(), 1)).into_iter().collect();
            let got = tally.delta();
            assert_eq!(
                (got.full, got.index_only.len()),
                (want, 0),
                "{first:?}: the first flush's cause"
            );
        }
        if let Some(share) = share {
            s.coins.lock().expect("coins").resize(share, false);
        }
        assert!(!s.hub.flush_due(), "{first:?}");
        intake(&rig);
        assert!(record(&rig, &lock(&side[0])));
        settle(&rig);
        assert_eq!(tip(&rig), rig.id(&side[0]));
        assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
        for m in &main {
            assert!(
                rig.status(m).has(Status::CONFLICT_CHAINLOCK),
                "{first:?} {}: marked",
                m.height
            );
        }
        let exit = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert_eq!(durable_tip(&s), Some(side[0].hash()));
        for m in &main {
            let stored = durable_status(&s, m).expect("stored");
            let marked = stored.has(Status::CONFLICT_CHAINLOCK);
            assert_eq!(marked, first != F::Always, "first flush {first:?}: {}", m.height);
        }
        let locked = durable_status(&s, &side[0]).expect("stored");
        assert!(!locked.has(Status::CONFLICT_CHAINLOCK) && locked.is_valid(Status::VALID_SCRIPTS));
    }
}

/// CH3b D242 (BL-813, TRIAGE-389's shape): the first flush site after V leaves IBD is the
/// enforce's own `DisconnectTip` (IF_NEEDED), so LEFT_IBD freezes inside `MarkConflictingBlock`,
/// after the disconnect and before the marks. Core takes no flush there, so its entries are still
/// dirty from their connects at its next write; the node's ALWAYS stores the marks too.
// Core: src/validation.cpp:3315 (DisconnectTip's flush), :4125-4133 (the marks) at v24.0.0
#[test]
fn a_mark_after_a_left_ibd_freeze_inside_the_enforce_reaches_the_disk() {
    let s = Stores::new();
    let mut rig = importing_rig(&s, true);
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 1, 1);
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    let before = durable_tip(&s);
    intake(&rig);
    done_importing(&rig);
    assert!(record(&rig, &lock(&side[0])));
    settle(&rig);
    assert_eq!(tip(&rig), rig.id(&side[0]));
    assert_ne!(before, Some(main[0].hash()));
    assert_eq!(durable_tip(&s), Some(main[0].hash()), "LEFT_IBD froze at main[1]'s disconnect");
    for m in &main {
        assert!(rig.status(m).has(Status::CONFLICT_CHAINLOCK), "{}: marked", m.height);
    }
    assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    for m in &main {
        let stored = durable_status(&s, m).expect("stored");
        assert!(stored.has(Status::CONFLICT_CHAINLOCK), "{}: the mark is stored", m.height);
    }
}

/// BL-281 (D69): `UpdateTip`'s caches through [`NodeFlush`] are the active layer's, as Core's
/// tip cache stands after the step's flush: three blocks leave three coins in it, and the block
/// after an ALWAYS flush, whose coins W took frozen, logs its own one alone.
#[test]
fn update_tip_reports_the_active_layer_alone() {
    install_accept_log();
    let s = Stores::new();
    let rig = Rig::with(s.config_with(Seams::default()));
    let cache = |m: &Mined, n: u64| {
        let lines = lines_with(&format!("UpdateTip: new best={} ", m.hash()));
        let coins = s.coins.lock().expect("coins");
        let active = coins.active();
        assert_eq!(active.len() as u64, n);
        let mib = active.allocated_bytes() as f64 * (1.0 / f64::from(1_u32 << 20));
        let want = format!(" cache={mib:.1}MiB({n}txo) evodb_cache=0.0MiB");
        assert!(lines.len() == 1 && lines[0].ends_with(&want), "{lines:?} {want}");
    };
    let chain = branch(&rig.p, &rig.genesis, 3, 2810);
    for m in &chain {
        rig.accept(m);
    }
    cache(&chain[2], 3);
    rig.on_v(|v| v.flush_now(FlushStateMode::Always));
    let next = branch(&rig.p, &chain[2], 1, 2810);
    rig.accept(&next[0]);
    cache(&next[0], 1);
    rig.assert_no_abort();
}

/// M2 retires BL-127's refusal of every restart: an index whose blocks were written to the node's
/// files loads on them, and each block reads back. A blk file lost since is still the load's
/// "Error loading block database".
#[test]
fn a_restart_on_the_written_files_loads_and_a_lost_one_is_refused() {
    install_accept_log();
    let s = Stores::new();
    let (mut rig, chain) = node(&s, 3);
    rig.on_v(|v| v.flush_now(FlushStateMode::Always));
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let state = s.files.mutate(StoreFiles::sync_and_state).expect("the files' state");
    let mut files = s.reopened(state.clone());
    let opened = open_block_index(
        s.p.clone(),
        LoadedIndex::Loaded(exit.chain),
        &mut files,
        &LocalShutdown::default(),
        false,
    );
    let (chain_ix, _) = opened.expect("the index loads on its files");
    let r = chain_ix.reader();
    for m in std::iter::once(&Mined::genesis(&s.p)).chain(&chain) {
        let e = r.entry(r.lookup(&m.hash()).expect("indexed")).expect("an entry");
        let at = FlatFilePos {
            file: e.file.expect("stored"),
            pos: e.data_pos,
        };
        assert_eq!(files.read_block(at, &m.hash()).expect("reads").hash(), m.hash());
    }
    drop(r);
    let blk = s.blocks_dir().join("blk00000.dat");
    std::fs::remove_file(&blk).expect("lose blk00000.dat");
    let mut files = s.reopened(state);
    let refused = open_block_index(
        s.p.clone(),
        LoadedIndex::Loaded(chain_ix),
        &mut files,
        &LocalShutdown::default(),
        false,
    );
    assert_eq!(refused.err(), Some(LoadFailure::LoadingBlockDb));
    let line = format!("Unable to open file {}", blk.display());
    assert!(!lines_with(&line).is_empty());
}

/// A stopped node's index, reopened on its files' state as a restart does, and the state.
fn restarted(s: &Stores, n: u32) -> (HeaderChain, Vec<Mined>, BlockFilesState) {
    let (mut rig, chain) = node(s, n);
    rig.on_v(|v| v.flush_now(FlushStateMode::Always));
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let state = s.files.mutate(StoreFiles::sync_and_state).expect("the files' state");
    (exit.chain, chain, state)
}

/// The stored entry of `m`: its file, data and undo positions.
fn stored(ix: &HeaderChain, m: &Mined) -> (i32, u32, u32) {
    let r = ix.reader();
    let e = r.entry(r.lookup(&m.hash()).expect("indexed")).expect("an entry");
    (e.file.expect("stored"), e.data_pos, e.undo_pos)
}

/// Sol r1 N4: a restart on a blk file whose final record was cut short loads (Core's
/// `LoadBlockIndexDB` only opens the files), and the read of that block is then Core's
/// `ReadBlockFromDisk` failure at the end of the file; the blocks before it still read.
#[test]
fn a_restart_on_a_truncated_final_blk_record_loads_and_its_read_fails() {
    let s = Stores::new();
    let (ix, chain, state) = restarted(&s, 3);
    let last = chain.last().expect("a tip");
    let (file, data, _) = stored(&ix, last);
    assert_eq!(file, 0);
    let blk = s.blocks_dir().join("blk00000.dat");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&blk)
        .and_then(|f| f.set_len(u64::from(data) + 10))
        .expect("cut the final record short");
    let mut files = s.reopened(state);
    let (ix, _) = open_block_index(
        s.p.clone(),
        LoadedIndex::Loaded(ix),
        &mut files,
        &LocalShutdown::default(),
        false,
    )
    .expect("the index loads");
    let at = FlatFilePos {
        file,
        pos: data,
    };
    assert_eq!(
        files.read_block(at, &last.hash()).err(),
        Some(format!(
            "ReadBlockFromDisk: Deserialize or I/O error - AutoFile::read: end of file: iostream \
             error at {}",
            fmt(at)
        ))
    );
    let (file, data, _) = stored(&ix, &chain[1]);
    let before = FlatFilePos {
        file,
        pos: data,
    };
    assert_eq!(files.read_block(before, &chain[1].hash()).map(|b| b.hash()), Ok(chain[1].hash()));
    assert!(s.sink.messages().is_empty());
}

/// Sol r1 N4: a restart with a rev file the index refers to gone loads (Core checks the blk
/// files only), and the undo read is then Core's `UndoReadFromDisk` open failure, naming the file.
#[test]
fn a_restart_with_a_missing_rev_file_loads_and_its_undo_read_fails() {
    let s = Stores::new();
    let (ix, chain, state) = restarted(&s, 3);
    let rev = s.blocks_dir().join("rev00000.dat");
    std::fs::remove_file(&rev).expect("lose rev00000.dat");
    let mut reopened = s.reopened(state.clone());
    let (ix, _) = open_block_index(
        s.p.clone(),
        LoadedIndex::Loaded(ix),
        &mut reopened,
        &LocalShutdown::default(),
        false,
    )
    .expect("the index loads");
    let (file, _, undo) = stored(&ix, &chain[2]);
    // The node reads undo only in recovery's replay, through ST2's reader and `FileRead`'s line
    // (the connector's disconnect is CH3a a5's).
    let reader = files(&s.dir, &Arc::new(TestFs::new()), &GlobalFailStop::new(), state, s.prune)
        .read(StoreFiles::reader);
    let at = StorePos::new(u32::try_from(file).expect("a file"), undo);
    let e =
        reader.read_undo(at, chain[1].hash().as_bytes(), &mut Vec::new()).expect_err("no rev file");
    assert_eq!(FileRead::Undo.line(&e), "UndoReadFromDisk: OpenUndoFile failed");
    assert_eq!(e.open_failed(), Some(rev.as_path()));
    assert!(s.sink.messages().is_empty());
}

/// Runs `f` on V without waiting: the receiver hears `f`'s outcome.
fn on_v_async(
    rig: &Rig,
    f: impl FnOnce(&mut V) -> Result<(), Fatal> + Send + 'static,
) -> mpsc::Receiver<bool> {
    let (tx, rx) = mpsc::channel();
    let cmd = ChainCommand::TestUnderV(Box::new(move |v| {
        let r = f(v);
        let _ = tx.send(r.is_ok());
        r
    }));
    rig.v.send(cmd).expect("V runs");
    rx
}

/// M1 (CH1-node D60, C1): W rings V for every event, `Released` too. V idle with a `FlushDue`
/// kept for the job in flight learns of the job's release at once and answers the cause.
#[test]
fn a_release_wakes_idle_v_to_answer_a_kept_flush_due() {
    let s = Stores::new();
    let (rig, _) = node(&s, 2);
    s.fs.gate.arm();
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    assert!(!s.hub.flush_due(), "the job took it");
    // With the job in flight the cause is kept, whether or not V has taken the ring yet.
    s.hub.post_flush_due();
    rig.idx(|_| ());
    assert!(s.hub.flush_due());
    s.fs.gate.release();
    assert!(until(|| !s.hub.flush_due()), "V answered the kept FlushDue after the release");
    rig.assert_no_abort();
}

/// CH3a a10 Q2: `ChainHandle::settle` replies only once W has released the job in flight, and
/// the `FlushDue` kept for that job has been answered and its job released too. A second settle
/// finds V quiet.
#[cfg(feature = "test-support")]
#[test]
fn settle_waits_for_w_and_answers_the_kept_flush_due() {
    let s = Stores::new();
    let (rig, _) = node(&s, 2);
    s.fs.gate.arm();
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    s.hub.post_flush_due();
    rig.idx(|_| ());
    assert!(s.hub.flush_due(), "kept for the job in flight");
    let settled = rig.v.settle();
    std::thread::sleep(Duration::from_millis(200));
    assert!(settled.try_recv().is_none(), "V waits for W");
    s.fs.gate.release();
    let quiet = settled.recv_timeout(PROMPT).expect("settled").expect("V runs");
    assert!(quiet.is_quiet() && quiet.rounds >= 1, "{quiet:?}");
    assert!(!s.hub.flush_due(), "the kept FlushDue was answered");
    let again = rig.v.settle().recv_timeout(PROMPT).expect("settled").expect("V runs");
    assert_eq!(again, Quiet::default());
    rig.assert_no_abort();
}

/// M1 (b4 D54, E-b1.7): V waiting for W inside its flush wakes when another store trips the
/// latch, while W is still inside a store call, and stops on the failure.
#[test]
fn a_trip_elsewhere_wakes_v_waiting_for_w_in_its_flush() {
    let s = Stores::new();
    let (mut rig, _) = node(&s, 2);
    s.fs.gate.arm();
    let done = on_v_async(&rig, |v| v.flush_now(FlushStateMode::Always));
    assert!(s.fs.gate.wait_held(), "the ALWAYS flush's job is at W");
    // Long enough for V to reach its park, which is what the latch's ring must end.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(done.try_recv(), Err(mpsc::TryRecvError::Empty), "V waits for W");
    s.latch.trip(Failure {
        kind: ErrorKind::Io,
        site: "evodb write",
        message: "another store failed".into(),
    });
    let woke = done.recv_timeout(PROMPT);
    s.fs.gate.release();
    assert_eq!(woke, Ok(false), "V woke on the trip and failed its flush");
    let exit = rig.stop();
    assert!(exit.fatal.is_some());
    assert!(s.sink.messages().is_empty(), "the store that tripped it reports, not V");
}

/// Item 8 (CH1-node D62): V's panic trips the latch without taking the coins' cell, which the
/// panic may have left held, so W writes nothing more.
#[test]
fn a_panic_trips_the_latch_with_the_coins_cell_held() {
    let s = Stores::new();
    let (mut rig, _) = node(&s, 1);
    let held = s.coins.lock().expect("coins");
    rig.v.send(ChainCommand::TestUnderV(Box::new(|_| panic!("index corrupt")))).expect("V runs");
    assert!(until(|| s.latch.tripped().is_some()));
    let tripped = s.latch.tripped().cloned();
    drop(held);
    let tripped = tripped.expect("tripped");
    assert_eq!(
        (tripped.site, tripped.message.as_str()),
        (V_PANIC_SITE, "the chain actor panicked")
    );
    assert_eq!(kept(&tripped), Kept::Assert);
    let joined = rig.join.take().expect("running").join().expect("an unmarked joiner");
    assert!(joined.is_err(), "the panic reaches V's joiner");
    assert_eq!(s.sink.messages(), ["V panicked: index corrupt"]);
}

/// CH1-node D62 (DEC-241): an `AbortNode` leaves the store open for the shutdown flushes. V
/// reports it once and stops the flush side; the latch stays clear.
#[test]
fn an_abort_node_leaves_the_latch_clear() {
    let s = Stores::new();
    let (mut rig, _) = node(&s, 1);
    let done = on_v_async(&rig, |_| Err(Fatal::new("a test's AbortNode")));
    assert_eq!(done.recv_timeout(PROMPT), Ok(false));
    assert!(rig.stop().fatal.is_some());
    assert!(s.latch.tripped().is_none());
    assert_eq!(s.sink.messages(), ["a test's AbortNode"]);
}

/// BL-271 (DEC-613): Core's two shutdown flushes on what V hands back, after a clean stop and
/// after an `AbortNode` (DEC-241). The first makes V's tip durable with its connected index entry
/// and queues its `ChainStateFlushed`; the second's is not queued (ND1 2b-D3 drops it).
// Core: init.cpp:380-386, :450-457; validation.cpp:3128 at v24.0.0
#[test]
fn the_shutdown_flushes_on_vs_exit_make_its_tip_durable() {
    #[derive(Default)]
    struct Flushed(AtomicU64);
    impl ValidationInterface for Flushed {
        fn chain_state_flushed(&self, _: &[BlockHash]) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    for abort in [false, true] {
        let s = Stores::new();
        let (mut rig, chain) = node(&s, 2);
        let tip = chain.last().expect("a tip");
        if abort {
            let done = on_v_async(&rig, |_| Err(Fatal::new("a test's AbortNode")));
            assert_eq!(done.recv_timeout(PROMPT), Ok(false));
        }
        let flushed = Arc::new(Flushed::default());
        rig.signals.register(flushed.clone());
        let mut exit = rig.stop();
        assert_eq!(exit.fatal.is_some(), abort);
        rig.sync();
        let count = || flushed.0.load(Ordering::SeqCst);
        let before = count();
        assert_ne!(durable_tip(&s), Some(tip.hash()), "abort {abort}");
        assert_eq!(durable_connected(&s, tip), None, "abort {abort}");
        exit.force_flush(ShutdownFlush::First).expect("the first flush");
        assert_eq!(durable_tip(&s), Some(tip.hash()), "abort {abort}");
        assert_eq!(durable_connected(&s, tip), Some(true), "abort {abort}");
        rig.sync();
        assert_eq!(count(), before + 1, "abort {abort}");
        exit.force_flush(ShutdownFlush::Second).expect("the second flush");
        rig.sync();
        assert_eq!(count(), before + 1, "abort {abort}");
        let reports: &[&str] = if abort {
            &["a test's AbortNode"]
        } else {
            &[]
        };
        assert_eq!(s.sink.messages(), reports);
    }
}

/// BL-271 r1 m-2: V stopped by an SB4 contract failure (`ChainError`) keeps no flush side for the
/// shutdown flushes, as after one of Core's asserts: SB4 refused a step V may have half taken, so
/// the restart comes back at V's last flush (b1), not at V's tip (b2). V reports the failure once
/// and the latch stays clear.
#[test]
fn a_contract_failure_leaves_the_shutdown_flushes_nothing_to_write() {
    let s = Stores::new();
    let (mut rig, chain) = node(&s, 1);
    rig.on_v(|v| v.flush_now(FlushStateMode::Always));
    let b1 = &chain[0];
    assert_eq!(durable_tip(&s), Some(b1.hash()), "V's last flush");
    let b2 = branch(&rig.p, b1, 1, 0).remove(0);
    rig.accept(&b2);
    assert_eq!(tip(&rig), rig.id(&b2));
    let done = on_v_async(&rig, |_| Err(Fatal::from(ChainError::Internal("a refused step"))));
    assert_eq!(done.recv_timeout(PROMPT), Ok(false));
    let mut exit = rig.stop();
    assert!(exit.fatal.is_some());
    for which in [ShutdownFlush::First, ShutdownFlush::Second] {
        assert_eq!(exit.force_flush(which), Ok(()), "nothing to flush");
    }
    assert_eq!(durable_tip(&s), Some(b1.hash()), "V's last flush");
    assert_eq!(durable_connected(&s, b1), Some(true));
    assert_eq!(durable_connected(&s, &b2), None);
    assert!(s.latch.tripped().is_none());
    assert_eq!(s.sink.messages(), ["chain: internal: a refused step"]);
}

/// A shutdown flush on V's exit that fails is reported once, as V reports one (L-3), and
/// returned after W's report, which ND1's failure line follows. W tripped the latch, so the flush
/// side is gone: the second flush writes nothing, and the caller's WAL sync reports the latch.
#[test]
fn a_failed_shutdown_flush_on_vs_exit_is_reported_once() {
    let s = Stores::new();
    let (mut rig, _) = node(&s, 2);
    let mut exit = rig.stop();
    s.fs.fail_space.store(true, Ordering::SeqCst);
    let f = exit.force_flush(ShutdownFlush::First).expect_err("the space query fails");
    assert_eq!(s.sink.messages(), [f.0.as_str()]);
    assert_eq!(exit.force_flush(ShutdownFlush::Second), Ok(()));
    assert_eq!(s.sink.messages(), [f.0]);
}

/// W's panic in a shutdown flush on V's exit is reported as W's thread ends (M2) and returned as
/// the latched failure; it does not reach the caller, as V's does not reach ND1 (its join).
#[test]
fn a_w_panic_in_a_shutdown_flush_on_vs_exit_is_reported_once() {
    let s = Stores::new();
    let (mut rig, _) = node(&s, 2);
    let mut exit = rig.stop();
    s.fs.panic_w.store(true, Ordering::SeqCst);
    let f = exit.force_flush(ShutdownFlush::First).expect_err("W panics");
    assert_eq!(s.sink.messages(), ["the chainstate flush thread panicked"]);
    assert_eq!(f.0, "the chainstate flush thread panicked");
    assert_eq!(s.latch.tripped().map(|f| f.site), Some(W_PANIC_SITE));
    assert_eq!(exit.force_flush(ShutdownFlush::Second), Ok(()));
}

/// V's report of a test's panic in its flush-side code (BL-674).
#[cfg(feature = "test-support")]
const V_PANICKED: &str = "V panicked: planted panic in the shutdown flush";

/// BL-674: V's flush-side code panics in a shutdown flush on V's exit, on the caller's thread,
/// as one of Core's asserts inside `FlushStateToDisk`. The flush catches it and returns the
/// assert, reported: the latch trips at V's site, V reports once unless a failure came first,
/// and the flush side is dropped, so a later shutdown flush writes nothing. For each flush, and
/// with a store failure latched first, whose report stands alone and whose `AbortNode` the flush
/// returns, as it returns any failure that came first (CH3b D221).
// Core: util/check.h:49-51; init.cpp:384, :454; validation.cpp:2980-2986 at v24.0.0
#[cfg(feature = "test-support")]
#[test]
fn a_v_panic_in_a_shutdown_flush_is_reported_once() {
    use crate::chain::FlushPanic;
    let store = format!("System error while flushing: {STORE_FAILED}");
    for (panics, prelatched) in [
        (ShutdownFlush::First, false),
        (ShutdownFlush::Second, false),
        (ShutdownFlush::First, true),
    ] {
        let case = format!("{panics:?}, prelatched {prelatched}");
        let s = Stores::new();
        let (mut rig, _) = node(&s, 2);
        let mut exit = rig.stop();
        if panics == ShutdownFlush::Second {
            exit.force_flush(ShutdownFlush::First).expect("the first flush");
        }
        if prelatched {
            let failure = Failure {
                kind: ErrorKind::Io,
                site: WRITE_SITE,
                message: STORE_FAILED.into(),
            };
            fail_on_w(&s.latch, s.sink.clone(), failure, store.clone()).join().expect("W reports");
        }
        exit.panic_at_next_flush(FlushPanic::BeforeJob);
        let f = exit.force_flush(panics).expect_err("V's flush panics");
        if prelatched {
            assert_eq!(f.0, store, "{case}");
            assert!(f.is_abort_node() && f.is_reported(), "{case}: {f:?}");
        } else {
            assert_eq!(f.0, V_PANICKED, "{case}");
            assert!(!f.is_abort_node() && f.is_reported(), "{case}: {f:?}");
        }
        let want = if prelatched {
            [(store.clone(), None)]
        } else {
            [(V_PANICKED.to_owned(), None)]
        };
        assert_eq!(s.sink.reports(), want, "{case}");
        let first = s.latch.tripped().expect("tripped").clone();
        if prelatched {
            assert_eq!(first.site, WRITE_SITE, "{case}");
            assert!(s.latch.any_site(|site| site == V_PANIC_SITE), "{case}: V's trip lost");
        } else {
            assert_eq!(
                (first.site, first.message.as_str()),
                (V_PANIC_SITE, "the chain actor panicked"),
                "{case}"
            );
        }
        assert_eq!(exit.force_flush(ShutdownFlush::Second), Ok(()), "{case}: no flush side");
        assert_eq!(s.sink.reports(), want, "{case}");
    }
}

/// BL-674: V panics with its job at W and the coins cell held (poisoned by the unwind). W is
/// held at its job's space check before V is released into the panic, and
/// the latch's wake releases it: the trip comes before the flush side's drop, so W's held job
/// takes no further guarded step and the durable tip stays at the last flush. The trip refuses
/// W's next guarded step or mutation; it does not cancel one already admitted, which this does
/// not test. Twice [`PROMPT`] bounds the flush, so a drop before the trip (W held, joined) fails here.
// Core: util/check.h:49-51; validation.cpp:2980-2986 at v24.0.0
#[cfg(feature = "test-support")]
#[test]
fn a_v_panic_in_a_shutdown_flush_refuses_the_job_in_flight() {
    use crate::chain::FlushPanic;
    let s = Stores::new();
    let (mut rig, chain) = node(&s, 2);
    rig.on_v(|v| v.flush_now(FlushStateMode::Always));
    let b2 = chain.last().expect("b2");
    assert_eq!(durable_tip(&s), Some(b2.hash()));
    let b3 = branch(&rig.p, b2, 1, 0).remove(0);
    rig.accept(&b3);
    let mut exit = rig.stop();
    s.fs.gate.arm();
    let _released = ReleaseOnDrop(&s.fs.gate);
    let fs = s.fs.clone();
    let _woken = s.latch.register_fn(Arc::new(move || fs.gate.release()));
    let fs = s.fs.clone();
    let at_w = Box::new(move || assert!(fs.gate.wait_held(), "W at the gate"));
    exit.panic_at_next_flush(FlushPanic::JobInFlight(at_w));
    let (tx, rx) = mpsc::channel();
    let flushing = std::thread::spawn(move || {
        let flushed = exit.force_flush(ShutdownFlush::First);
        _ = tx.send(flushed);
        exit
    });
    let flushed = rx.recv_timeout(2 * PROMPT).expect("the flush returns within the bound");
    let f = flushed.expect_err("V's flush panics");
    assert_eq!(f.0, V_PANICKED);
    assert!(!f.is_abort_node() && f.is_reported(), "{f:?}");
    assert_eq!(s.sink.reports(), [(V_PANICKED.to_owned(), None)]);
    assert_eq!(s.latch.tripped().map(|f| f.site), Some(V_PANIC_SITE));
    assert_eq!(Meta::read(&*s.db).expect("meta").tip, Some(b2.hash()), "the last flush's tip");
    assert_eq!(durable_record(&s, &b3), None);
    let mut exit = flushing.join().expect("the flush thread");
    assert_eq!(exit.force_flush(ShutdownFlush::Second), Ok(()), "no flush side");
}

/// BL-546 (CH3b D221): a shutdown flush on V's exit whose chainstate write, or under a live copy
/// whose barrier, fails returns the text W's `AbortNode` reported, not the engine's text the
/// guard latched.
// Core: validation.cpp:3127-3128, :2250-2254 at v24.0.0
#[test]
fn a_failed_store_call_in_a_shutdown_flush_on_vs_exit_returns_the_reported_text() {
    for site in ["chainstate write", "chainstate barrier"] {
        let s = Stores::new();
        let seams = Seams {
            live_copy: site == "chainstate barrier",
            ..Seams::default()
        };
        let (mut rig, _) = node_with(&s, 2, seams);
        let mut exit = rig.stop();
        if site == "chainstate write" {
            s.db.inner().crash_at_write(1);
        } else {
            s.db.inner().fail_barrier_at(1);
        }
        let f = exit.force_flush(ShutdownFlush::First).expect_err(site);
        let text = reported_store_failure(&s, site);
        assert_eq!(f.0, text);
        assert_eq!(s.sink.messages(), [text]);
    }
}

/// The text W's `AbortNode` reports for the store failure the guard latched at `site`.
fn reported_store_failure(s: &Stores, site: &str) -> String {
    let latched = s.latch.tripped().expect("the guard latched the failure").clone();
    assert_eq!(latched.site, site);
    format!("System error while flushing: {}", latched.message)
}

/// V's stop on W's failed chainstate write, `fatal` its exit's: the sink heard W's report, then
/// ABC's "Failed to disconnect block" if `second`, and the stop text is W's (BL-559). W's text.
fn assert_stopped_on_flush(s: &Stores, fatal: Option<Fatal>, second: bool) -> String {
    let text = reported_store_failure(s, "chainstate write");
    let mut want = vec![(text.clone(), None)];
    if second {
        want.push((DISCONNECT_FAILED.to_owned(), None));
    }
    assert_eq!(s.sink.reports(), want);
    assert_eq!(fatal.map(|f| f.0).as_deref(), Some(text.as_str()));
    text
}

/// A node whose first flush site out of IBD comes when the test calls [`done_importing`], and
/// is waited for (`-dbcrashratio`'s synchronous policy): genesis is ten minutes old, but init
/// connects it, and the test's blocks connect, under `fImporting`, so in IBD.
fn importing_rig(s: &Stores, chainlocks: bool) -> Rig {
    importing_rig_with(s, chainlocks, Seams::default(), None)
}

/// [`importing_rig`] over `seams`, its policy synchronous, and `mempool` (default none).
fn importing_rig_with(
    s: &Stores,
    chainlocks: bool,
    seams: Seams,
    mempool: Option<Box<dyn MempoolSink>>,
) -> Rig {
    Rig::with(importing_config(s, chainlocks, seams, mempool))
}

/// [`importing_rig_with`]'s configuration.
fn importing_config(
    s: &Stores,
    chainlocks: bool,
    seams: Seams,
    mempool: Option<Box<dyn MempoolSink>>,
) -> RigConfig {
    let seams = Seams {
        synchronous: true,
        ..seams
    };
    RigConfig {
        after_genesis: Some(600),
        chainlocks,
        mempool,
        before_init: Some(Box::new(|v: &crate::chain::ChainHandle| {
            v.send(ChainCommand::SetLoading {
                importing: Some(true),
                reindex: None,
            })
            .expect("V runs");
        })),
        ..s.config_with(seams)
    }
}

/// Clears `fImporting`: V's next flush site freezes for LEFT_IBD and waits for W.
fn done_importing(rig: &Rig) {
    let done = ChainCommand::SetLoading {
        importing: Some(false),
        reindex: None,
    };
    rig.v.send(done).expect("V runs");
}

/// OD-CP-29 (CH7 D102): a failed snapshot write is a store fault with one report, its store's
/// (EV3 D21, CH3b D221), whoever reads the latch next. A `preciousblock` moves V to a tied
/// branch, and route B's call runs at its ABC's first release point (§2.4), before the hold:
/// its inline write fails in the node's engine, as dashd-node wires the rows (one engine, its
/// latch and sink), so the guard latches the raw row write (`RAW_WRITE_SITE`) and the store
/// reports it. The disconnect then commits the coins, and its flush call is the first to read
/// the latch: it takes the failure as reported, not the flush's (BL-559's gate, `kept` is
/// `Nothing`), so V stops on the store's text before the tip step, and ABC adds no "Failed to
/// disconnect block".
#[test]
fn a_disconnect_whose_flush_reads_a_failed_snapshot_write_adds_no_report() {
    use dash_evo::llmq::QuorumSnapshot;
    use dash_params::LlmqType;

    use super::failure::pause;
    use crate::evo::{RocksEvoStore, testkit};
    use crate::flush::EffectLedger;
    use crate::llmq::capability::Capability as _;
    use crate::llmq::facade::MembersFacade;
    use crate::llmq::rows::RuntimeRows;

    install_accept_log();
    let s = Stores::new();
    let store = Arc::new(RocksEvoStore::new(s.db.clone(), testkit::codec(), testkit::genesis()));
    let rows = Arc::new(RuntimeRows::new(store, Arc::new(EffectLedger::default())));
    let facade = MembersFacade::new(&s.p, rows, s.latch.clone()).expect("in domain");
    let mut rig = Rig::with(RigConfig {
        facade: Some(Arc::new(facade)),
        ..importing_config(&s, false, Seams::default(), None)
    });
    let a = branch(&rig.p, &rig.genesis, 1, 0);
    let b = branch(&rig.p, &rig.genesis, 1, 1);
    rig.accept(&a[0]);
    rig.accept(&b[0]);
    assert_eq!(tip(&rig), rig.id(&a[0]), "b1 ties a1");
    assert!(s.latch.tripped().is_none());
    let thread = rig.v_thread();
    s.db.inner().crash_at_write(1);
    // One lane, in order: the command first, then the call its release point serves.
    let resume = pause(&rig.v);
    let moved = rig.v.precious_block(b[0].hash());
    let base = rig.genesis.hash();
    let wrote = rig.v.on_v(move |on_v, _| {
        let snapshot = QuorumSnapshot {
            active_quorum_members: vec![true, false],
            skip_list_mode: 0,
            skip_list: vec![],
        };
        let mut effects = Vec::new();
        let mut rows = on_v.rows(&mut effects);
        rows.store(LlmqType::LlmqTestDip0024, &base, &snapshot).map_err(|e| e.to_string())
    });
    resume.send(()).expect("V paused");
    let err = wrote.recv().expect("served at the release point").expect_err("the write fails");
    assert_eq!(moved.recv(), Err(WaitError::Disconnected), "V stops in the reorg");
    let text = reported_store_failure(&s, "chainstate raw write");
    assert!(err.contains(&text), "{err}");
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some(text.as_str()), "the store's text");
    assert_eq!(s.sink.messages(), [text.as_str()], "the store's report, and only it");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(a[0].hash()), "no tip step");
    let coins = s.coins.lock().expect("coins").best_block().map(|b| b.hash);
    let lines = lines_on(thread);
    assert_eq!(coins, Some(rig.genesis.hash()), "DisconnectBlock committed: {lines:#?}");
}

/// BL-546 (CH3b D221): an ABC whose flush fails at W's write answers W's `AbortNode` text, its
/// `state.ToString()`, reported once. Init's ABC connects genesis, ten minutes old, then its
/// PERIODIC flush freezes for LEFT_IBD and waits for W (`-dbcrashratio`'s synchronous policy).
// Core: validation.cpp:3127-3128, :2250-2254, :3862 at v24.0.0
#[test]
fn an_abc_whose_flush_fails_answers_the_reported_text() {
    let s = Stores::new();
    let db = Arc::clone(&s.db);
    let seams = Seams {
        synchronous: true,
        ..Seams::default()
    };
    let mut rig = Rig::with(RigConfig {
        after_genesis: Some(600),
        aborts_before_init: true,
        before_init: Some(Box::new(move |_: &crate::chain::ChainHandle| {
            db.inner().crash_at_write(1);
        })),
        ..s.config_with(seams)
    });
    let text = reported_store_failure(&s, "chainstate write");
    assert_eq!(rig.init, Ok(Err(text.clone())));
    // W's report comes before V's reply (BL-554, CH3b D222).
    assert_eq!(s.sink.messages(), [text]);
    assert!(rig.stop().fatal.is_some());
}

/// BL-546 (CH3b D221): `ProcessNewBlock`'s failure line names W's `AbortNode` text when the
/// block's ABC fails at W's write.
// Core: validation.cpp:4910-4913, :3127-3128 at v24.0.0
#[test]
fn process_new_block_logs_the_reported_text_of_a_failed_flush() {
    install_accept_log();
    let s = Stores::new();
    let mut rig = importing_rig(&s, false);
    let a1 = branch(&rig.p, &rig.genesis, 1, 0).remove(0);
    let thread = rig.v_thread();
    done_importing(&rig);
    s.db.inner().crash_at_write(1);
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    // W's report comes before V's line and its reply (BL-554, CH3b D222).
    let text = reported_store_failure(&s, "chainstate write");
    assert_eq!(s.sink.messages(), [text.as_str()]);
    let line = format!("ProcessNewBlock: ActivateBestChain failed ({text})");
    assert!(lines_on(thread).contains(&line), "{:#?}", lines_on(thread));
    assert!(rig.stop().fatal.is_some());
}

/// BL-546 (CH3b D221): `EnforceBestChainLock`'s failure line names W's `AbortNode` text. The lock
/// is on an invalidated block, which enforcement resets (`v:4085-4088` at v24.0.0) with nothing to
/// mark, so the first flush out of IBD is its ABC's.
// Core: chainlock/handler.cpp:299-305; validation.cpp:3127-3128 at v24.0.0
#[test]
fn enforce_best_chainlock_logs_the_reported_text_of_a_failed_flush() {
    install_accept_log();
    let s = Stores::new();
    let mut rig = importing_rig(&s, true);
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    for m in &main {
        rig.accept(m);
    }
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    intake(&rig);
    let thread = rig.v_thread();
    done_importing(&rig);
    s.db.inner().crash_at_write(1);
    assert!(record(&rig, &lock(&main[1])));
    let failed = "ChainlockHandler::EnforceBestChainLock -- ActivateBestChain failed: ";
    // A query wakes V for its deferred enforcement; once V has stopped it is dropped.
    let logged = || {
        let _ = rig.v.query(|_| ()).recv();
        lines_on(thread).iter().any(|l| l.starts_with(failed))
    };
    assert!(until(logged), "{:#?}", lines_on(thread));
    // W's report comes before V's line (BL-554, CH3b D222).
    let text = reported_store_failure(&s, "chainstate write");
    assert_eq!(s.sink.messages(), [text.as_str()]);
    assert!(lines_on(thread).contains(&format!("{failed}{text}")), "{:#?}", lines_on(thread));
    assert!(rig.stop().fatal.is_some());
}

/// BL-554 (CH3b D222): init's ABC whose flush fails at W's write replies only after W's
/// `AbortNode`. W's report is held: V waits for it (the checkpoint) and has not replied; once it
/// is released, the reply is W's text, which the sink already holds. Init runs on a thread of
/// its own (`Rig::with` waits for its reply), under the test's release; V's thread is learnt
/// before init's ABC, so the checkpoint is V's own wait.
// Core: validation.cpp:3127-3128, :2250-2254, :3862 at v24.0.0
fn init_replies_after_ws_report(parker: bool) {
    let s = Stores::new();
    let db = Arc::clone(&s.db);
    let (init, heard, stopped, parked, early) = std::thread::scope(|scope| {
        let release = s.held.arm();
        let s = &s;
        let (on_v, v_id) = mpsc::channel();
        let init = scope.spawn(move || {
            let seams = Seams {
                synchronous: true,
                parker,
                ..Seams::default()
            };
            let mut rig = Rig::with(RigConfig {
                after_genesis: Some(600),
                aborts_before_init: true,
                before_init: Some(Box::new(move |v: &crate::chain::ChainHandle| {
                    let id = v.query(|_| std::thread::current().id()).recv();
                    on_v.send(id.expect("V runs")).expect("the test waits");
                    db.inner().crash_at_write(1);
                })),
                ..s.config_with(seams)
            });
            let heard = s.sink.messages();
            let init = std::mem::replace(&mut rig.init, Ok(Ok(())));
            (init, heard, rig.stop().fatal.is_some())
        });
        let v_id = v_id.recv_timeout(PROMPT).expect("V answers before init's ABC");
        let parked = s.held.parked(&s.latch, v_id);
        let early = init.is_finished();
        release.release();
        let (init, heard, stopped) = init.join().expect("init runs");
        (init, heard, stopped, parked, early)
    });
    assert!(parked.is_some(), "W reached its report, and V waits for it");
    assert!(!early, "init replied before W's report");
    let text = reported_store_failure(&s, "chainstate write");
    assert_eq!(init, Ok(Err(text.clone())));
    assert_eq!(heard, std::slice::from_ref(&text), "the report comes before the reply");
    assert!(stopped);
    assert_eq!(s.sink.messages(), [text]);
}

#[test]
fn init_replies_after_ws_report_on_vs_bell() {
    init_replies_after_ws_report(false);
}

#[test]
fn init_replies_after_ws_report_on_the_drivers_parker() {
    init_replies_after_ws_report(true);
}

/// BL-554 (CH3b D222): `ProcessNewBlock`'s failure line follows W's `AbortNode`. W's report is
/// held: V waits for it, with no line yet; released, W's `***` line comes first.
// Core: validation.cpp:4910-4913, :3127-3128 at v24.0.0
fn process_new_block_logs_after_ws_report(parker: bool) {
    install_accept_log();
    let s = Stores::new();
    let seams = Seams {
        parker,
        ..Seams::default()
    };
    let mut rig = importing_rig_with(&s, false, seams, None);
    let a1 = branch(&rig.p, &rig.genesis, 1, 0).remove(0);
    let v = rig.v_thread();
    done_importing(&rig);
    s.db.inner().crash_at_write(1);
    let failed = "ProcessNewBlock: ActivateBestChain failed";
    let release = s.held.arm();
    let reply = rig.v.process_block(a1.block.clone(), true, None, true);
    let w = s.held.parked(&s.latch, v);
    let early = lines_on(v).iter().any(|l| l.starts_with(failed));
    release.release();
    assert_eq!(reply.recv(), Err(WaitError::Disconnected));
    let w = w.expect("V waits for W's report");
    assert!(!early, "V logged before W's report: {:#?}", lines_on(v));
    let text = reported_store_failure(&s, "chainstate write");
    assert_eq!(s.sink.messages(), [text.as_str()]);
    let line = format!("{failed} ({text})");
    assert_logged_before(&lines_of(&[w, v]), &format!("*** {text}"), &line);
    assert!(rig.stop().fatal.is_some());
}

#[test]
fn process_new_block_logs_after_ws_report_on_vs_bell() {
    process_new_block_logs_after_ws_report(false);
}

#[test]
fn process_new_block_logs_after_ws_report_on_the_drivers_parker() {
    process_new_block_logs_after_ws_report(true);
}

/// BL-554 (CH3b D222): V's own `AbortNode` stop while W's report is pending waits for it, though
/// V read no latch before. V is inside a command when W's job fails at its write; the command's
/// `AbortNode` stops V, and V's stop, which reads the latch (`NodeFlush::abort`), waits for W's
/// report before it sets the node's flag; V does not report again.
// Core: shutdown.cpp:27-37
#[test]
fn vs_abort_node_stop_waits_for_ws_pending_report() {
    let s = Stores::new();
    let (rig, _) = node(&s, 2);
    s.fs.gate.arm();
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    s.db.inner().crash_at_write(1);
    let v = rig.v_thread();
    let release = s.held.arm();
    let (in_command, on_v) = mpsc::channel();
    let (go, wait_go) = mpsc::channel::<()>();
    let done = on_v_async(&rig, move |_| {
        in_command.send(()).expect("the test waits");
        let _ = wait_go.recv_timeout(PROMPT);
        Err(Fatal::new("V's own AbortNode"))
    });
    on_v.recv_timeout(PROMPT).expect("V runs the command");
    s.fs.gate.release();
    let w = s.held.wait_entered();
    drop(go);
    let parked = w.is_some() && until(|| s.latch.pending_waiters().contains(&v));
    let early = rig.shutdown.shutdown_requested();
    release.release();
    assert!(w.is_some(), "W reached its report");
    assert!(parked, "V's stop waits for W's report");
    assert!(!early, "V's stop set the node's flag before W's report");
    assert_eq!(done.recv_timeout(PROMPT), Ok(false));
    let exit = stop_within_prompt(rig);
    assert!(exit.fatal.is_some());
    let text = reported_store_failure(&s, "chainstate write");
    assert_eq!(s.sink.messages(), [text], "V does not report a latched failure again");
}

/// BL-554 (Sol r1 M-2, CH3b D222): a block save refused at its check, after W's file barrier
/// failed, returns after W's report. W (a thread of the test's, as W runs a step) trips the latch
/// at its failing barrier and holds its report; the save waits for it, then returns `Ok(None)`.
// Core: validation.cpp:4848-4851 at v24.0.0
#[test]
fn a_save_refused_at_its_check_returns_after_ws_report() {
    let s = Stores::new();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    save(&mut files, &genesis);
    let a1 = branch(&s.p, &genesis, 1, 0).remove(0);
    let (saved, heard, parked, unlocked) = std::thread::scope(|scope| {
        let release = s.held.arm();
        let s = &s;
        std::thread::Builder::new()
            .name(W_THREAD.into())
            .spawn_scoped(scope, move || {
                report_on_w(&s.latch, s.db.abort_sink(), || {
                    s.fs.fail_sync.store(true, Ordering::SeqCst);
                    let e =
                        s.files.mutate(|f| f.sync_and_take_dirty_infos()).expect_err("W's barrier");
                    e.to_string()
                });
            })
            .expect("W");
        let w = s.held.wait_entered();
        let v = scope.spawn(move || {
            let saved = files.save_block(&a1.block, a1.height, None);
            (saved, s.sink.messages())
        });
        let id = v.thread().id();
        let parked = w.is_some() && until(|| s.latch.pending_waiters().contains(&id));
        // The files' lock is free while the save waits.
        let (read, unlocked) = mpsc::channel();
        scope.spawn(move || {
            s.files.read(|_| ());
            _ = read.send(());
        });
        let unlocked = unlocked.recv_timeout(PROMPT).is_ok();
        release.release();
        let (saved, heard) = v.join().expect("the save returns");
        (saved, heard, parked, unlocked)
    });
    assert!(parked, "the refused save waits for W's report");
    assert!(unlocked, "the save waits without the files' lock");
    assert_eq!(saved, Ok(None));
    let barrier = s.latch.tripped().expect("W's barrier latched").message.clone();
    assert_eq!(heard, [barrier], "W's report comes before the refusal");
}

/// BL-554 (Sol r3 m-2, CH3b D222): a block save whose write fails after W's failure latched,
/// its trip losing, returns after W's report, without the files' lock while it waits. The save
/// is held inside its write (past its check); W (a thread of the test's) fails a chainstate
/// write, which takes no files lock, and holds its report; the save's write then fails.
// Core: validation.cpp:4848-4851 at v24.0.0
#[test]
fn a_save_whose_write_loses_to_w_returns_after_ws_report() {
    let s = Stores::new();
    let mut files = s.block_files();
    let genesis = Mined::genesis(&s.p);
    save(&mut files, &genesis);
    let a1 = branch(&s.p, &genesis, 1, 0).remove(0);
    let (saved, heard, parked, unlocked) = std::thread::scope(|scope| {
        let release = s.held.arm();
        s.fs.held_write.arm();
        let _write = Released(&s.fs.held_write);
        let s = &s;
        let v = scope.spawn(move || {
            let saved = files.save_block(&a1.block, a1.height, None);
            (saved, s.sink.messages())
        });
        assert!(s.fs.held_write.wait_held(), "the save is inside its write");
        std::thread::Builder::new()
            .name(W_THREAD.into())
            .spawn_scoped(scope, move || {
                report_on_w(&s.latch, s.db.abort_sink(), || {
                    s.db.inner().crash_at_write(1);
                    let mut batch = s.db.batch();
                    dashd_store::KvBatch::put(
                        &mut batch,
                        CHAINSTATE_SCHEMA.cf("coins").unwrap(),
                        b"k",
                        b"v",
                    );
                    let e =
                        s.db.write(batch, dashd_store::Commit::Buffered).expect_err("W's write");
                    format!("System error while flushing: {e}")
                });
            })
            .expect("W");
        let w = s.held.wait_entered();
        s.fs.held_write.release();
        let id = v.thread().id();
        let parked = w.is_some() && until(|| s.latch.pending_waiters().contains(&id));
        // The files' lock is free while the save waits.
        let (read, unlocked) = mpsc::channel();
        scope.spawn(move || {
            s.files.read(|_| ());
            _ = read.send(());
        });
        let unlocked = unlocked.recv_timeout(PROMPT).is_ok();
        release.release();
        let (saved, heard) = v.join().expect("the save returns");
        (saved, heard, parked, unlocked)
    });
    assert!(parked, "the refused save waits for W's report");
    assert!(unlocked, "the save waits without the files' lock");
    assert_eq!(saved, Ok(None));
    let text = reported_store_failure(&s, "chainstate write");
    assert_eq!(heard, [text], "W's report comes before the refusal");
}

/// Releases a [`Gate`] when dropped, so a failing test does not hold its thread.
struct Released<'a>(&'a Gate);

impl Drop for Released<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// BL-554 (Sol r2 M-2, CH3b D222): W's report of a failed step does not wait on the hub's
/// registration window: the test holds its lock while W's Files step fails, and W still enters
/// its report. A report behind the window would wait out the watchdog, which then lets it go.
#[test]
fn ws_report_does_not_wait_on_the_registration_window() {
    let s = Stores::new();
    let (rig, _) = node(&s, 2);
    s.fs.gate.arm();
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    let release = s.held.arm();
    let window = s.hub.hold_window();
    s.fs.fail_sync.store(true, Ordering::SeqCst);
    s.fs.gate.release();
    let entered = s.held.wait_entered();
    drop(window);
    release.release();
    assert!(entered.is_some(), "W's report waited on the window");
    let exit = stop_within_prompt(rig);
    assert!(exit.fatal.is_some());
    assert_eq!(s.sink.messages().len(), 1, "{:?}", s.sink.messages());
}

/// BL-546 r1 m-1 (CH3b D221): a block write that fails is reported once, by `SaveBlockToDisk`,
/// and `AcceptBlock` fails the block with Core's state. V, stopping, reads the latch: its `Fatal`,
/// the text the node's shutdown logs, is that one report.
// Core: validation.cpp:4848-4851, :4903 at v24.0.0
#[test]
fn a_failed_block_write_is_the_text_v_stops_on() {
    install_accept_log();
    let s = Stores::new();
    let mut rig = importing_rig(&s, false);
    let a1 = branch(&rig.p, &rig.genesis, 1, 0).remove(0);
    let thread = rig.v_thread();
    done_importing(&rig);
    s.fs.fail_write.store(true, Ordering::SeqCst);
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv().expect("V replies");
    let position = "AcceptBlock: Failed to find position to write new block to disk";
    assert_eq!(r.state, BlockState::Error(position.into()));
    let report = "Failed to write block";
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(report));
    assert_eq!(s.sink.reports(), [(report.to_owned(), None)]);
    let line = format!("ProcessNewBlock: AcceptBlock FAILED ({position})");
    assert!(lines_on(thread).contains(&line), "{:#?}", lines_on(thread));
}

/// How [`Tamper`] changes the connect, or the proposal, of its block (BL-673).
enum Tampered {
    /// Another block's body for the block's id.
    Body(Arc<Block>),
    /// The coins cell's best block moved off the parent: to the block itself for a connect, to
    /// none for a proposal.
    View,
    /// An id no index entry holds.
    Id,
}

/// The rig's real connector, which changes one input of the connect or the proposal of the block
/// `at` as `how` says, then forwards it; `calls` counts what it forwards for `at`.
struct Tamper {
    inner: Box<dyn BlockConnector>,
    at: BlockHash,
    how: Option<Tampered>,
    coins: ChainstateCell<ProbeEngine>,
    calls: Arc<AtomicU32>,
}

impl BlockConnector for Tamper {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        if block.hash() != self.at {
            return self.inner.connect_block(cx, v, target, block, checked, id, st);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.how {
            None => self.inner.connect_block(cx, v, target, block, checked, id, st),
            Some(Tampered::Body(other)) => {
                self.inner.connect_block(cx, v, target, other, checked, id, st)
            }
            Some(Tampered::View) => {
                let e = cx.reader.entry(id).expect("the block is indexed");
                self.coins.lock().expect("coins").set_best_block(Some(BlockRef {
                    id,
                    hash: e.hash,
                    height: e.height,
                }));
                self.inner.connect_block(cx, v, target, block, checked, id, st)
            }
            Some(Tampered::Id) => {
                self.inner.connect_block(cx, v, target, block, checked, BlockId(u32::MAX), st)
            }
        }
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        self.inner.disconnect_block(cx, v, target, block, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        check_pow: bool,
        check_merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        if block.hash() == self.at {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(Tampered::View) = self.how {
                self.coins.lock().expect("coins").set_best_block(None);
            }
        }
        self.inner.test_block_validity(cx, v, checks, block, prev, check_pow, check_merkle)
    }

    fn published_bls(&self) -> Option<BlsCtx> {
        self.inner.published_bls()
    }

    fn evo_reads(&self) -> &dyn EvoReads {
        self.inner.evo_reads()
    }

    fn ehf_rows(&self) -> Option<Arc<dyn EhfRows>> {
        self.inner.ehf_rows()
    }

    fn verify_scratch(
        &mut self,
        parent: ScratchParent,
    ) -> Result<Option<Box<dyn ScratchView>>, Fatal> {
        self.inner.verify_scratch(parent)
    }

    fn take_scratch_refusal(&mut self) -> Option<String> {
        self.inner.take_scratch_refusal()
    }

    fn take_undo_fault(&mut self) -> Option<Fatal> {
        self.inner.take_undo_fault()
    }

    fn open_scheme_guard(&mut self, holder: crate::chain::SchemeGuardHolder) {
        self.inner.open_scheme_guard(holder);
    }

    fn commit_scheme_guard(&mut self) {
        self.inner.commit_scheme_guard();
    }

    fn exit_scheme_guard(&mut self, log: bool) {
        self.inner.exit_scheme_guard(log);
    }
}

/// A rig on `s`'s stores whose real connector is wrapped in a [`Tamper`] for `at`: the rig and
/// the wrapper's count.
fn tampered(s: &Stores, at: &Mined, how: Option<Tampered>) -> (Rig, Arc<AtomicU32>) {
    tampered_with(s, s.config_with(Seams::default()), at, how)
}

/// [`tampered`] over the config `cfg` on `s`'s stores.
fn tampered_with(
    s: &Stores,
    mut cfg: RigConfig,
    at: &Mined,
    how: Option<Tampered>,
) -> (Rig, Arc<AtomicU32>) {
    let calls = Arc::new(AtomicU32::new(0));
    cfg.connector = Some(Box::new(Tamper {
        inner: cfg.connector.take().expect("the real connector"),
        at: at.hash(),
        how,
        coins: s.coins.clone(),
        calls: calls.clone(),
    }));
    (Rig::with(cfg), calls)
}

/// BL-673's oracle: V stopped on `text` as on one of Core's asserts (CH1-node D62a). No caller
/// logged a failure line, V reported the assert once and tripped the latch at its site, and the
/// shutdown flushes kept no flush side: they write nothing, so the durable tip is still
/// `durable` and `unsaved` has no stored index record.
fn assert_stopped_on_an_assert(
    s: &Stores,
    mut rig: Rig,
    text: &str,
    durable: Option<BlockHash>,
    unsaved: &Mined,
) {
    let v = rig.v_thread();
    let mut exit = rig.stop();
    let fatal = exit.fatal.clone().expect("V stopped on the connect");
    assert_eq!(fatal.0, text);
    assert!(!fatal.is_abort_node(), "{fatal:?}");
    let lines = lines_on(v);
    assert!(
        !lines.iter().any(|l| l.starts_with("ConnectTip:")
            || l.starts_with("ProcessNewBlock:")
            || l.contains("ChainlockHandler")),
        "{lines:#?}"
    );
    let reports = [(text.to_owned(), None)];
    assert_eq!(s.sink.reports(), reports);
    assert_eq!(s.latch.tripped().map(|f| f.site), Some(V_PANIC_SITE));
    for which in [ShutdownFlush::First, ShutdownFlush::Second] {
        assert_eq!(exit.force_flush(which), Ok(()), "{which:?}");
    }
    assert_eq!(s.sink.reports(), reports, "the shutdown flushes report nothing");
    assert_eq!(durable_tip(s), durable);
    assert_eq!(durable_record(s, unsaved), None);
}

/// BL-673 (CH1-node D62a): `ConnectBlock`'s block that is not its index entry's is Core's assert,
/// not an `AbortNode`: the block's `ProcessBlock` reply disconnects and V stops on the assert.
// Core: validation.cpp:2490 at v24.0.0
#[test]
fn a_block_that_is_not_its_index_entry_is_an_assert() {
    install_accept_log();
    let s = Stores::new();
    let genesis = Mined::genesis(&s.p);
    let a1 = branch(&s.p, &genesis, 1, 0).remove(0);
    let other = branch(&s.p, &genesis, 1, 1).remove(0);
    let (rig, calls) = tampered(&s, &a1, Some(Tampered::Body(other.block.clone())));
    let durable = durable_tip(&s);
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r.map(|_| ()), Err(WaitError::Disconnected));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let text =
        format!("ConnectBlock: block {} is not block {} at height 1", other.hash(), a1.hash());
    assert_stopped_on_an_assert(&s, rig, &text, durable, &a1);
}

/// BL-673 (CH1-node D62a): `ConnectBlock`'s view off the block's parent is Core's assert.
// Core: validation.cpp:2534 at v24.0.0
#[test]
fn a_view_off_the_parent_is_an_assert() {
    install_accept_log();
    let s = Stores::new();
    let genesis = Mined::genesis(&s.p);
    let a1 = branch(&s.p, &genesis, 1, 0).remove(0);
    let (rig, calls) = tampered(&s, &a1, Some(Tampered::View));
    let durable = durable_tip(&s);
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r.map(|_| ()), Err(WaitError::Disconnected));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let text = format!(
        "ConnectBlock: the view's best block {:?} is not the parent {:?}",
        Some(a1.hash()),
        Some(genesis.hash())
    );
    assert_stopped_on_an_assert(&s, rig, &text, durable, &a1);
}

/// BL-673 (CH1-node D62a): `ConnectBlock` on an id with no index entry is Core's assert.
// Core: validation.cpp:2487 at v24.0.0
#[test]
fn an_unindexed_block_is_an_assert() {
    install_accept_log();
    let s = Stores::new();
    let genesis = Mined::genesis(&s.p);
    let a1 = branch(&s.p, &genesis, 1, 0).remove(0);
    let (rig, calls) = tampered(&s, &a1, Some(Tampered::Id));
    let durable = durable_tip(&s);
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r.map(|_| ()), Err(WaitError::Disconnected));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_stopped_on_an_assert(&s, rig, "ConnectBlock: an unindexed block", durable, &a1);
}

/// BL-673 (CH1-node D62a): `TestBlockValidity`'s view off the tip, past V's own check of the
/// proposal's parent, is Core's assert: the proposal's reply disconnects.
// Core: validation.cpp:4945 at v24.0.0
#[test]
fn a_proposal_off_the_tip_is_an_assert() {
    install_accept_log();
    let s = Stores::new();
    let genesis = Mined::genesis(&s.p);
    let a1 = branch(&s.p, &genesis, 1, 0).remove(0);
    let (rig, calls) = tampered(&s, &a1, Some(Tampered::View));
    let durable = durable_tip(&s);
    let r = rig.v.test_block_validity(a1.block.clone(), true, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let text = "TestBlockValidity: the parent is not the tip";
    assert_stopped_on_an_assert(&s, rig, text, durable, &a1);
}

/// W's panic, as its thread's end reports it.
const W_PANICKED: &str = "the chainstate flush thread panicked";
/// A store failure W latches and reports (BL-677's controls).
const STORE_FAILED: &str = "injected store failure";

/// The block checks V signalled, in order.
#[derive(Default)]
struct Checked(Mutex<Vec<(BlockHash, BlockState)>>);

impl ValidationInterface for Checked {
    fn block_checked(&self, block: &Block, state: &BlockState, _: bool) {
        self.0.lock().expect("checked").push((block.hash(), state.clone()));
    }
}

/// BL-677's run on `s`'s stores: b1's `ProcessBlock`, whose block write first has W latch and
/// report `failures` (the first wins; the others lose), then fails, so the real block files
/// refuse the save. The injection fired at the save, and the connector was never entered for b1.
struct RefusedSave {
    rig: Rig,
    b1: Mined,
    reply: Result<BlockResult, WaitError>,
    checked: Arc<Checked>,
    /// V's and W's threads, for their lines.
    threads: [std::thread::ThreadId; 2],
}

fn refused_save(s: &Stores, failures: Vec<(&'static str, &'static str, String)>) -> RefusedSave {
    install_accept_log();
    let genesis = Mined::genesis(&s.p);
    let b1 = branch(&s.p, &genesis, 1, 0).remove(0);
    let (rig, calls) = tampered(s, &b1, None);
    let checked = Arc::new(Checked::default());
    rig.signals.register(checked.clone());
    let (latch, sink) = (s.latch.clone(), s.sink.clone());
    let w = Arc::new(Mutex::new(None));
    let w_id = w.clone();
    *s.fs.at_save.lock().expect("at_save") = Some(Box::new(move || {
        for (n, (site, message, report)) in failures.into_iter().enumerate() {
            let failure = Failure {
                kind: ErrorKind::Io,
                site,
                message: message.into(),
            };
            if n == 0 {
                let w = fail_on_w(&latch, sink.clone(), failure, report);
                *w_id.lock().expect("w") = Some(w.thread().id());
                w.join().expect("W reports");
            } else {
                assert!(!latch.trip(failure), "a losing trip");
            }
        }
    }));
    let reply = rig.v.process_block(b1.block.clone(), true, None, true).recv();
    assert!(s.fs.saved.load(Ordering::SeqCst), "the injection fired at the block save");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the connector was never entered for b1");
    let w = w.lock().expect("w").expect("W ran");
    let threads = [rig.v_thread(), w];
    RefusedSave {
        rig,
        b1,
        reply,
        checked,
        threads,
    }
}

impl RefusedSave {
    /// Stops V: its stop's `Fatal`, after checking that b1 has no data.
    fn stop(&mut self) -> Fatal {
        let exit = self.rig.stop();
        let r = exit.chain.reader();
        let e = r.entry(r.lookup(&self.b1.hash()).expect("b1 is indexed")).expect("an entry");
        assert!(!e.status.has(Status::HAVE_DATA), "{:?}", e.status);
        exit.fatal.expect("V stopped on the latch")
    }

    /// The block checks V signalled for b1.
    fn checked(&self) -> Vec<BlockState> {
        let checked = self.checked.0.lock().expect("checked");
        checked.iter().filter(|(h, _)| *h == self.b1.hash()).map(|(_, st)| st.clone()).collect()
    }
}

/// BL-677: a block save that W's panic, latched meanwhile, refuses is that assert (BL-586's
/// carrier, K8): `AcceptBlock` returns it, so no `BlockChecked`, no `ProcessNewBlock` line and no
/// reply follow, and V stops at once on W's one report. Core's process ended at the assert,
/// before the block.
// Core: util/check.h:49-51; validation.cpp:4845-4856, :4901-4905 at v24.0.0
#[test]
fn a_w_panic_first_read_at_a_refused_block_save_is_that_assert() {
    let s = Stores::new();
    let mut run = refused_save(&s, vec![(W_PANIC_SITE, W_PANICKED, W_PANICKED.to_owned())]);
    assert_eq!(run.reply.as_ref().map(|_| ()), Err(&WaitError::Disconnected));
    let fatal = run.stop();
    assert_eq!(fatal.0, W_PANICKED);
    assert!(!fatal.is_abort_node() && fatal.is_reported(), "{fatal:?}");
    assert_eq!(s.sink.reports(), [(W_PANICKED.to_owned(), None)]);
    let lines = lines_on(run.threads[0]);
    assert!(
        !lines.iter().any(|l| l.starts_with("ProcessNewBlock:")
            || l.contains("AcceptBlock")
            || l.starts_with("ConnectTip:")),
        "{lines:#?}"
    );
    assert!(run.checked().is_empty(), "no BlockChecked for b1");
}

/// The refused block save's result after a store failure W latched first: D221's state, its
/// one `BlockChecked`, and `ProcessNewBlock`'s line after W's report; the latch's later losing
/// assert, if any, changes nothing (a window BL-678 leaves open: the failure latches inside
/// the command, after its admission).
fn assert_d221s_refused_save(s: &Stores, mut run: RefusedSave, report: &str) {
    let position = "AcceptBlock: Failed to find position to write new block to disk";
    let r = run.reply.as_ref().expect("V replies");
    assert!(!r.accepted && r.new_block, "{r:?}");
    assert_eq!(r.state, BlockState::Error(position.into()));
    assert_eq!(run.checked(), [BlockState::Error(position.into())]);
    let line = format!("ProcessNewBlock: AcceptBlock FAILED ({position})");
    let lines = lines_of(&run.threads);
    assert_eq!(lines.iter().filter(|l| **l == line).count(), 1, "{lines:#?}");
    assert_logged_before(&lines, &format!("*** {report}"), &line);
    let fatal = run.stop();
    assert!(fatal.is_abort_node(), "{fatal:?}");
    assert_eq!(s.sink.reports(), [(report.to_owned(), None)]);
}

/// BL-677's control: a block save refused after a store failure keeps Core's state (CH3b D221).
// Core: validation.cpp:4845-4856, :4901-4905 at v24.0.0
#[test]
fn a_store_failure_first_read_at_a_refused_block_save_keeps_cores_state() {
    let s = Stores::new();
    let report = format!("System error while flushing: {STORE_FAILED}");
    let run = refused_save(&s, vec![(WRITE_SITE, STORE_FAILED, report.clone())]);
    assert_d221s_refused_save(&s, run, &report);
}

/// BL-677's control: the first failure's class decides, as at K1-K4: an assert that lost to an
/// earlier store failure leaves the refused save D221's state. Core's process would already have
/// ended at that assert; the difference is BL-678's declared residual, as both latch inside the
/// command, after its admission.
// Core: validation.cpp:4845-4856, :4901-4905 at v24.0.0; util/check.h:49-51
#[test]
fn a_store_failure_then_a_losing_assert_at_a_refused_block_save_keeps_d221s_state() {
    let s = Stores::new();
    let report = format!("System error while flushing: {STORE_FAILED}");
    let failures = vec![
        (WRITE_SITE, STORE_FAILED, report.clone()),
        (W_PANIC_SITE, W_PANICKED, W_PANICKED.to_owned()),
    ];
    let run = refused_save(&s, failures);
    assert_d221s_refused_save(&s, run, &report);
}

/// What [`always_flush_held`] hears: V at a fanout pump in its wait for W, the latch tripped, or
/// V's flush returned.
#[derive(Debug, PartialEq)]
enum Heard {
    Pumped,
    Latched,
    Returned,
}

/// V's ALWAYS flush on `rig`, its job held at W's space check until `release`, which runs once V
/// is in its wait for the job (it has pumped its fanout there, as it does before each park):
/// what V's flush returned. [`PROMPT`] bounds V's reactions only, reaching that wait while W is
/// held and waking after W's end; W's store work up to its end (the latch tripped) is waited for
/// without a limit, as the other flushes here are (BL-354).
fn always_flush_held(
    s: &Stores,
    rig: &Rig,
    release: impl FnOnce(),
) -> Result<bool, mpsc::RecvTimeoutError> {
    let (tx, heard) = mpsc::channel();
    let latched = tx.clone();
    let _woken = s.latch.register_fn(Arc::new(move || _ = latched.send(Heard::Latched)));
    s.fs.gate.arm();
    let done = on_v_async(rig, move |v| {
        let pumped = tx.clone();
        AT_PUMP.set(Some(Box::new(move || _ = pumped.send(Heard::Pumped))));
        let r = v.flush_now(FlushStateMode::Always);
        AT_PUMP.set(None);
        _ = tx.send(Heard::Returned);
        r
    });
    assert!(s.fs.gate.wait_held(), "the ALWAYS flush's job is at W");
    assert_eq!(heard.recv_timeout(PROMPT), Ok(Heard::Pumped), "V waits for W");
    assert_eq!(done.try_recv(), Err(mpsc::TryRecvError::Empty), "V waits for W");
    release();
    s.fs.gate.release();
    // V pumps before every park: those until W's end (the latch) or V's return.
    while heard.recv() == Ok(Heard::Pumped) {}
    done.recv_timeout(PROMPT)
}

/// Sol r1 N2: W ending without a panic while V waits for its flush's release, as at
/// `-dbcrashratio`'s simulated crash (ratio 1: at the first partial coins batch): W runs the
/// exit, latches the crash and ends; V wakes on the latch or W's `Crashed` and stops on the
/// latched failure, which Core reports to no one (`_Exit(0)`).
#[test]
fn w_ending_while_v_waits_for_the_release_stops_v_on_the_crash() {
    let s = Stores::new();
    let exited = Arc::new(AtomicBool::new(false));
    let exit = exited.clone();
    let seams = Seams {
        crash: CrashRoll::seeded(1, 0),
        on_crash: Box::new(move || exit.store(true, Ordering::SeqCst)),
        synchronous: true,
        ..Seams::default()
    };
    // Enough coins for a partial sub-batch, after which W rolls (Core's `BatchWrite`).
    let (mut rig, _) = node_with(&s, 300, seams);
    assert_eq!(always_flush_held(&s, &rig, || ()), Ok(false), "V woke on W's end");
    assert!(exited.load(Ordering::SeqCst), "the exit ran before V woke");
    assert_eq!(s.latch.tripped().map(|f| f.site), Some("dbcrashratio"));
    assert!(rig.stop().fatal.is_some());
    assert!(s.sink.messages().is_empty());
}

/// Sol r1 N2: W panicking while V waits for its flush's release: W's end latches and reports the
/// panic, V wakes and stops on it, and the panic reaches V's joiner when V drops W's driver.
/// That drop comes before V answers its reaper, so init's command is then dropped (CH7-notes
/// D72, Sol BL-547 r2 M-1).
#[test]
fn a_panic_on_w_while_v_waits_for_the_release_reaches_vs_joiner() {
    let s = Stores::new();
    let (mut rig, _) = node(&s, 2);
    let calls = Arc::new(InitCalls::default());
    rig.signals.register(calls.clone());
    let woke = always_flush_held(&s, &rig, || s.fs.panic_w.store(true, Ordering::SeqCst));
    assert_eq!(woke, Ok(false), "V woke on W's end");
    let panicked = "the chainstate flush thread panicked";
    assert_eq!(s.latch.tripped().map(|f| f.message.as_str()), Some(panicked));
    assert_eq!(s.latch.tripped().map(|f| f.site), Some(W_PANIC_SITE));
    rig.v.shutdown();
    let join = rig.join.take().expect("running");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || _ = tx.send(join.join().expect("an unmarked joiner").err()));
    let payload = rx.recv_timeout(PROMPT).expect("V's thread ends").expect("V's thread panicked");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"W's flush thread panicked"));
    assert_eq!(s.sink.messages(), [panicked]);
    assert_init_dropped(&rig, &calls);
}

/// Runs `rig.stop()` on a thread of its own: its exit within [`PROMPT`], so a stop that hangs
/// fails the test.
fn stop_within_prompt(mut rig: Rig) -> crate::chain::actor::VExit {
    within(PROMPT, move || rig.stop())
}

/// Sol r1 N3: an `AbortNode` with W's job in flight and held in a store step (its space check):
/// V's stop waits for the job's release, W's thread ends at the driver's drop, the latch stays
/// clear, and the coins are durable at the tip for ND1's shutdown flush, which succeeds.
#[test]
fn an_abort_node_with_w_held_in_a_store_step_completes_the_job() {
    let s = Stores::new();
    let (rig, chain) = node(&s, 2);
    let tip = chain.last().expect("a tip").hash();
    s.fs.gate.arm();
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    let done = on_v_async(&rig, |_| Err(Fatal::new("a test's AbortNode")));
    assert_eq!(done.recv_timeout(PROMPT), Ok(false));
    let fs = s.fs.clone();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        fs.gate.release();
    });
    let mut exit = stop_within_prompt(rig);
    release.join().expect("released");
    assert!(exit.fatal.is_some());
    assert!(s.latch.tripped().is_none());
    assert_eq!(s.sink.messages(), ["a test's AbortNode"]);
    assert_eq!(durable_tip(&s), Some(tip));
    assert_eq!(Meta::read(&*s.db).expect("meta").tip, Some(tip));
    // ND1's shutdown flush on V's exit, then its WAL sync, with the latch clear (BL-271).
    exit.force_flush(ShutdownFlush::First).expect("ND1's shutdown flush");
    s.db.sync_wal().expect("ND1's WAL sync");
}

/// `n` blocks under `-prune`, `-fastprune` and `seams`' observer, with the automatic check off
/// until `on` (target `target` bytes, 0 for manual only): the rig, the chain, and the marks.
fn pruned_node(
    s: &Stores,
    n: u32,
    target: u64,
    on: &Arc<AtomicBool>,
) -> (Rig, Vec<Mined>, Arc<RecordingObserver>) {
    pruned_node_with(s, n, target, on, false)
}

/// [`pruned_node`], with ChainLocks on when `chainlocks`.
fn pruned_node_with(
    s: &Stores,
    n: u32,
    target: u64,
    on: &Arc<AtomicBool>,
    chainlocks: bool,
) -> (Rig, Vec<Mined>, Arc<RecordingObserver>) {
    let obs = Arc::new(RecordingObserver::default());
    let seams = Seams {
        prune: s.prune_host(target, on),
        obs: obs.clone(),
        ..Seams::default()
    };
    let rig = Rig::with(RigConfig {
        chainlocks,
        ..s.config_with(seams)
    });
    let chain = padded_branch(&rig.p, &rig.genesis, n);
    for m in &chain {
        rig.accept(m);
    }
    (rig, chain, obs)
}

/// The bytes of [`padded_branch`]'s unspendable coinbase output.
const PAD: usize = 1500;

/// `n` blocks on `parent` whose coinbases carry an unspendable output of [`PAD`] bytes, so a
/// `-fastprune` blk file holds about forty: below DIP3's height on regtest, prunable files
/// under the 288 blocks Core keeps.
fn padded_branch(p: &ChainSetup, parent: &Mined, n: u32) -> Vec<Mined> {
    let mut out: Vec<Mined> = Vec::new();
    for _ in 0..n {
        let prev = out.last().unwrap_or(parent);
        let height = prev.height + 1;
        let cb = padded_coinbase(&cb_script(height, 0));
        let block = mine(p, prev.hash(), prev.time() + SPACING, vec![cb]);
        out.push(Mined {
            block,
            height,
        });
    }
    out
}

/// [`coinbase`] with its output's script `OP_RETURN OP_PUSHDATA2 <PAD bytes>`.
fn padded_coinbase(script_sig: &[u8]) -> Arc<Tx> {
    let pad = u16::try_from(PAD).expect("a small pad");
    let mut raw = 1i32.to_le_bytes().to_vec();
    raw.push(1);
    raw.extend([0u8; 32]);
    raw.extend(u32::MAX.to_le_bytes());
    raw.push(u8::try_from(script_sig.len()).expect("a short script"));
    raw.extend(script_sig);
    raw.extend(u32::MAX.to_le_bytes());
    raw.push(1);
    raw.extend(0i64.to_le_bytes());
    raw.push(0xfd);
    raw.extend((pad + 4).to_le_bytes());
    raw.extend([0x6a, 0x4d]);
    raw.extend(pad.to_le_bytes());
    raw.resize(raw.len() + PAD, 0);
    raw.extend(0u32.to_le_bytes());
    let tx = Transaction::decode(&mut raw.as_slice()).expect("a coinbase");
    Arc::new(Tx::new(tx))
}

/// The files of the prune selections in `marks`, in order.
fn selected(marks: &[ObsMark]) -> Vec<Vec<u32>> {
    marks
        .iter()
        .filter_map(|m| match m {
            ObsMark::PruneSelected {
                files,
                ..
            } => Some(files.clone()),
            _ => None,
        })
        .collect()
}

/// Whether file `f`'s blk and rev files are both gone.
fn unlinked(s: &Stores, f: u32) -> bool {
    ["blk", "rev"].iter().all(|k| !s.blocks_dir().join(format!("{k}{f:05}.dat")).exists())
}

/// The blocks of `chain` at or below `height` have no data in `ix` (Core's `PruneOneBlockFile`).
fn pruned_below(ix: &HeaderChain, chain: &[Mined], height: u32) -> bool {
    let r = ix.reader();
    chain.iter().filter(|m| m.height <= height).all(|m| {
        let e = r.entry(r.lookup(&m.hash()).expect("indexed")).expect("an entry");
        !e.status.has(dash_chain::Status::HAVE_DATA)
    })
}

/// An automatic prune's flush on W, with V's `AbortNode` raised right after the prune's call
/// returned.
struct Aborted {
    exit: crate::chain::actor::VExit,
    chain: Vec<Mined>,
    /// V's and W's marks.
    obs: Arc<RecordingObserver>,
    /// Each file's last height before the prune, which zeroes the selected files' infos.
    heights: Vec<u32>,
}

impl Aborted {
    /// The files of the one prune selection, and the last height they held.
    fn selection(&self) -> (Vec<u32>, u32) {
        selection(&self.obs, &self.heights)
    }
}

/// An `AbortNode` raised by V right after a prune's waited call (CH3b D244), before V's stop: the
/// node, its chain and marks, each file's last height, and V's pending reply. While `s.fs.unlink`
/// is armed, V is still inside the call, with W at the prune's unlink.
struct AbortPending {
    rig: Rig,
    done: mpsc::Receiver<bool>,
    chain: Vec<Mined>,
    obs: Arc<RecordingObserver>,
    heights: Vec<u32>,
}

impl AbortPending {
    /// The files of the one prune selection, and the last height they held.
    fn selection(&self) -> (Vec<u32>, u32) {
        selection(&self.obs, &self.heights)
    }

    /// V's reply, then V's stop.
    fn finish(self) -> Aborted {
        assert_eq!(self.done.recv_timeout(PROMPT), Ok(false));
        let exit = stop_within_prompt(self.rig);
        assert!(exit.fatal.is_some());
        Aborted {
            exit,
            chain: self.chain,
            obs: self.obs,
            heights: self.heights,
        }
    }
}

/// The last height of `files` before the prune.
fn last_height(heights: &[u32], files: &[u32]) -> u32 {
    files.iter().map(|&f| heights[f as usize]).max().expect("a file")
}

/// Each file's last height now.
fn heights(st: &Stores) -> Vec<u32> {
    st.files.read(|f| (0..).map_while(|n| f.file_info(n).map(|i| i.height_last)).collect())
}

/// The files of the one prune selection in `obs`, and the last height they held.
fn selection(obs: &RecordingObserver, heights: &[u32]) -> (Vec<u32>, u32) {
    let mut picked = selected(&obs.marks());
    assert_eq!(picked.len(), 1);
    let picked = picked.remove(0);
    let last = last_height(heights, &picked);
    (picked, last)
}

/// V's call that prunes, sent: the `AbortNode` comes right after it returns, so the prune is
/// complete when the abort is raised.
fn start_abort_after_a_prune(s: &Stores) -> AbortPending {
    let on = Arc::new(AtomicBool::new(false));
    let (rig, chain, obs) = pruned_node(s, 400, 1, &on);
    let heights = heights(s);
    on.store(true, Ordering::SeqCst);
    let marks = obs.clone();
    let done = on_v_async(&rig, move |v| {
        v.flush_now(FlushStateMode::IfNeeded)?;
        assert!(!selected(&marks.marks()).is_empty(), "the prune selected inside its call");
        Err(Fatal::new("a test's AbortNode"))
    });
    AbortPending {
        rig,
        done,
        chain,
        obs,
        heights,
    }
}

/// [`Aborted`]: V's `AbortNode` right after a prune's call, then V's stop.
fn abort_after_a_prune(s: &Stores) -> Aborted {
    start_abort_after_a_prune(s).finish()
}

/// Releases a [`Gate`] when it drops, so a failed check does not leave W held there while the
/// test's `VExit` drops and joins W.
struct ReleaseOnDrop<'a>(&'a Gate);

impl Drop for ReleaseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Sol r1 N3: an `AbortNode` right after a prune's call: the prune is complete when the abort is
/// raised (CH3b D244), so the prune's clears are made (its blocks without data), the latch stays
/// clear, and ND1's shutdown flush succeeds. The prune's unlink, step 12, came before W's
/// release; W lives on in the `VExit` (BL-271).
#[test]
fn an_abort_node_with_w_held_at_a_prune_clear_completes_the_prune() {
    let s = Stores::pruned();
    let mut a = abort_after_a_prune(&s);
    let (picked, last) = a.selection();
    assert!(a.exit.have_pruned);
    assert!(pruned_below(&a.exit.chain, &a.chain, last));
    assert!(s.latch.tripped().is_none());
    assert_eq!(s.sink.messages(), ["a test's AbortNode"]);
    // ND1's shutdown flush on V's exit, then its WAL sync, with the latch clear (BL-271).
    a.exit.force_flush(ShutdownFlush::First).expect("ND1's shutdown flush");
    s.db.sync_wal().expect("ND1's WAL sync");
    assert!(picked.iter().all(|&f| unlinked(&s, f)));
}

/// BL-519: a process that ends with W at the prune's unlink, inside V's call (CH3b D244), leaves
/// the prune durable: no stored record keeps data in a selected file, and `meta.pruned_files` is
/// set, so the restart's scan (Core's `ScanAndUnlinkAlreadyPrunedFiles`, which the node's index
/// load runs, BL-523) removes the files the durable infos record as pruned. Then W goes on, and
/// V's `AbortNode` and stop follow.
#[test]
fn a_process_ended_at_the_unlink_after_an_abort_node_leaves_the_prune_replayable() {
    let s = Stores::pruned();
    s.fs.unlink.arm();
    let a = start_abort_after_a_prune(&s);
    let _w = ReleaseOnDrop(&s.fs.unlink);
    assert!(s.fs.unlink.wait_held(), "W reaches a remove");
    let (picked, last) = a.selection();
    assert!(a.obs.marks().contains(&ObsMark::BestEffortBegin), "the prune's unlink");
    assert!(picked.iter().all(|&f| s.blocks_dir().join(format!("blk{f:05}.dat")).exists()));
    // What a restart loads, and its scan, as recovery runs them.
    let mut cfg = BlockFilesConfig::new(s.blocks_dir(), MAGIC);
    (cfg.fastprune, cfg.prune_mode) = (true, true);
    let restart = RecordingObserver::default();
    let opened = ChainstateOpen::open(&*s.db, Arc::new(TestFs::new()), cfg, &restart);
    let opened = opened.expect("the restart's open");
    assert!(opened.meta().pruned_files);
    let mut pruned = a.chain.iter().filter(|m| m.height <= last);
    assert!(pruned.all(|m| durable_status(&s, m).is_some_and(|st| !st.has(Status::HAVE_DATA))));
    let have_pruned = opened.meta().pruned_files;
    let scanned = crate::store::scan_already_pruned_files(opened.files(), have_pruned);
    scanned.expect("the scan");
    drop(opened);
    assert!(picked.iter().all(|&f| unlinked(&s, f)));
    s.fs.unlink.release();
    drop(a.finish());
    assert!(s.latch.tripped().is_none());
}

/// BL-523: the restart after a process that ended with W at the prune's unlink (the clears and
/// `meta.pruned_files` durable, the selected files on disk) runs Core's startup scan in its index
/// load: `open_block_index` on the stored records, with the stored flag, removes the selected
/// files after the blk-file check and the flag's line, logging each, then refuses the unpruned
/// load with Core's `-prune` state error.
// Core (375c64d2317): validation.cpp:5473; node/blockstorage.cpp:395-413, :429-444, :635-646;
// node/chainstate.cpp:200-204
#[test]
fn a_restart_after_a_prune_ended_at_its_unlink_removes_its_files_then_refuses() {
    install_accept_log();
    let s = Stores::pruned();
    s.fs.unlink.arm();
    let a = start_abort_after_a_prune(&s);
    let _w = ReleaseOnDrop(&s.fs.unlink);
    assert!(s.fs.unlink.wait_held(), "W reaches a remove");
    let (picked, _) = a.selection();
    assert!(picked.iter().all(|&f| s.blocks_dir().join(format!("blk{f:05}.dat")).exists()));
    let mut cfg = BlockFilesConfig::new(s.blocks_dir(), MAGIC);
    (cfg.fastprune, cfg.prune_mode) = (true, true);
    let restart = RecordingObserver::default();
    let opened = ChainstateOpen::open(&*s.db, Arc::new(TestFs::new()), cfg, &restart);
    let opened = opened.expect("the restart's open");
    let have_pruned = opened.meta().pruned_files;
    assert!(have_pruned);
    let records = opened.block_index().map(|row| -> StoredRecord {
        let (key, value) = row.expect("a record");
        Ok((BlockHash::from(<[u8; 32]>::try_from(key.as_slice()).expect("a hash")), value))
    });
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().expect("a test pool");
    let stored = LoadedIndex::Stored {
        cfg: ChainConfig::node(&s.p),
        svc: services(&s.p),
        records: Box::new(records),
        pool: &pool,
    };
    let abort = Arc::new(AbortPath::new(s.sink.clone(), GlobalFailStop::new()));
    let mut files = NodeBlockFiles::new(Arc::clone(opened.files()), pow(&s.p), abort);
    let shutdown = LocalShutdown::default();
    let here = std::thread::current().id();
    let before = lines_on(here).len();
    let loaded = open_block_index(s.p.clone(), stored, &mut files, &shutdown, have_pruned);
    assert_eq!(loaded.err(), Some(LoadFailure::PrunedDatadir));
    assert!(picked.iter().all(|&f| unlinked(&s, f)));
    let lines = lines_on(here)[before..].to_vec();
    assert_logged_before(&lines, "Checking all blk files are present...", PREVIOUSLY_PRUNED);
    for f in &picked {
        let deleted = format!("Prune: UnlinkPrunedFiles deleted blk/rev ({f:05})");
        assert_eq!(lines.iter().filter(|l| **l == deleted).count(), 1, "{lines:#?}");
        assert_logged_before(&lines, PREVIOUSLY_PRUNED, &deleted);
    }
    drop((files, opened));
    s.fs.unlink.release();
    drop(a.finish());
    assert!(s.latch.tripped().is_none());
}

/// BL-523: the node's block files refuse the startup scan once their guard has latched a
/// failure, which was reported where it happened: the load fails as a false `LoadBlockIndex`
/// ("Error loading block database"), before the `-prune` state check. Core has no such path.
#[test]
fn latched_block_files_refuse_the_startup_scan_and_fail_the_load() {
    let s = Stores::new();
    assert!(s.latch.trip(Failure {
        kind: ErrorKind::Other,
        site: "block files write",
        message: "a test's failure".into(),
    }));
    let empty = LoadedIndex::Empty {
        cfg: ChainConfig::node(&s.p),
        svc: services(&s.p),
    };
    let mut files = s.block_files();
    let loaded = open_block_index(s.p.clone(), empty, &mut files, &LocalShutdown::default(), true);
    assert_eq!(loaded.err(), Some(LoadFailure::LoadingBlockDb));
}

/// Sol r1 N5: `pruneblockchain`'s `FlushStateToDisk(NONE, height)` through V's flush site: its
/// decision is ST2's `FindFilesToPruneManual` on the state before it, and when it returns the
/// flush has completed in Core's order (the decision, B1 durable, the selection, the unlink) with
/// the selected files gone and the coins durable at the tip. The index then has the pruned blocks
/// without data, and the reference tables match at the stop.
#[test]
fn a_manual_prune_flush_unlinks_cores_files_before_it_returns() {
    let s = Stores::pruned();
    let off = Arc::new(AtomicBool::new(false));
    let (mut rig, chain, obs) = pruned_node(&s, 400, 0, &off);
    let tip = chain.last().expect("a tip");
    let (tip_hash, height) = (tip.hash(), tip.height);
    let keep = super::support::regtest_keep();
    let expected = s.files.read(|f| f.plan_files_to_prune_manual(height, Some(height), keep).files);
    assert!(!expected.is_empty());
    assert!(expected.iter().all(|&f| !unlinked(&s, f)), "the files were written");
    let last = s
        .files
        .read(|f| expected.iter().map(|&n| f.file_info(n).map_or(0, |i| i.height_last)).max());
    let (tx, rx) = mpsc::channel();
    let (coins, dir) = (s.coins.clone(), s.blocks_dir());
    let marks = obs.clone();
    let files = expected.clone();
    let _ = on_v_async(&rig, move |v| {
        let r = v.flush_at_site(FlushStateMode::None, Some(height));
        let gone = files
            .iter()
            .all(|f| ["blk", "rev"].iter().all(|k| !dir.join(format!("{k}{f:05}.dat")).exists()));
        let durable = coins.lock().expect("coins").durable().map(|b| b.hash);
        let _ = tx.send((r.is_ok(), gone, durable, marks.marks()));
        r
    });
    let (ok, gone, durable, at_return) = rx.recv_timeout(PROMPT).expect("V ran it");
    assert!(ok);
    assert!(gone, "the selected files are unlinked before the flush returns");
    assert_eq!(durable, Some(tip_hash));
    // Each mark with its flush, where it names one.
    let order: Vec<(&str, Option<u64>)> = at_return
        .iter()
        .filter_map(|m| match m {
            ObsMark::PruneDecision {
                manual: Some(h),
                files,
                ..
            } => {
                assert_eq!((*h, files), (height, &expected));
                Some(("decision", None))
            }
            ObsMark::Durable {
                flush,
            } => Some(("durable", Some(*flush))),
            ObsMark::PruneSelected {
                flush,
                files,
                ..
            } => {
                assert_eq!(files, &expected);
                Some(("selected", Some(*flush)))
            }
            ObsMark::BestEffortBegin => Some(("unlink", None)),
            ObsMark::BestEffortEnd => Some(("unlinked", None)),
            _ => None,
        })
        .skip_while(|&(m, _)| m != "decision")
        .collect();
    let names: Vec<&str> = order.iter().map(|&(m, _)| m).collect();
    assert_eq!(names, ["decision", "durable", "selected", "unlink", "unlinked"]);
    assert_eq!(order[1].1, order[2].1, "one flush's B1 and selection");
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert!(exit.have_pruned);
    assert!(pruned_below(&exit.chain, &chain, last.expect("a file")));
    rig.assert_no_abort();
}

/// b4 D57 (Opus r1 m-3): `PruneAndFlush` (init step 10) arms the automatic check itself and
/// returns once its NONE flush is done, as Core's synchronous one: the files its selection
/// chose are unlinked and the coins durable at the tip.
#[test]
fn prune_and_flush_arms_the_check_and_returns_with_the_files_unlinked() {
    let s = Stores::pruned();
    let on = Arc::new(AtomicBool::new(false));
    let (mut rig, chain, obs) = pruned_node(&s, 400, 1, &on);
    let tip = chain.last().expect("a tip").hash();
    // The blocks' allocations armed the check: disarm it, so only the call's own arms it.
    s.files.mutate(|f| Ok(f.take_check_for_pruning())).expect("files");
    on.store(true, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    let (coins, marks) = (s.coins.clone(), obs.clone());
    let dir = s.blocks_dir();
    let _ = on_v_async(&rig, move |v| {
        let r = v.prune_and_flush_at_site();
        let picked = selected(&marks.marks());
        let gone = picked
            .iter()
            .flatten()
            .all(|f| ["blk", "rev"].iter().all(|k| !dir.join(format!("{k}{f:05}.dat")).exists()));
        let durable = coins.lock().expect("coins").durable().map(|b| b.hash);
        let _ = tx.send((r.is_ok(), picked, gone, durable));
        r
    });
    let (ok, picked, gone, durable) = rx.recv_timeout(PROMPT).expect("V ran it");
    assert!(ok);
    assert_eq!(picked.len(), 1, "one selection");
    assert!(!picked[0].is_empty(), "the armed check selected files");
    assert!(gone, "the selected files are unlinked before the call returns");
    assert_eq!(durable, Some(tip));
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert!(exit.have_pruned);
    rig.assert_no_abort();
}

/// D47 item 4, A1 §3.4's `LEFT_IBD`: the flush reads IBD from V's host. A node that has left IBD
/// freezes once for it at its first flush site, and one in IBD does not: after the blocks and an
/// IF_NEEDED site, V's ALWAYS flush (which waits for the job in flight) is the second full flush
/// out of IBD and the first in it. Each flush is waited for (`-dbcrashratio`'s synchronous
/// policy): with a job in flight, ABC's PERIODIC sees its frozen layer beside the active one as
/// a LARGE cache and keeps a flush for later, as W's timing decides.
#[test]
fn leaving_ibd_freezes_the_coins_once() {
    for (after_genesis, in_ibd) in [(None, true), (Some(600), false)] {
        let s = Stores::new();
        let obs = Arc::new(RecordingObserver::default());
        let seams = Seams {
            obs: obs.clone(),
            synchronous: true,
            ..Seams::default()
        };
        let rig = Rig::with(RigConfig {
            after_genesis,
            ..s.config_with(seams)
        });
        for m in &branch(&rig.p, &rig.genesis, 2, 0) {
            rig.accept(m);
        }
        let (tx, rx) = mpsc::channel();
        rig.on_v(move |v| {
            let _ = tx.send(v.is_ibd());
            Ok(())
        });
        assert_eq!(rx.recv(), Ok(in_ibd));
        rig.on_v(|v| v.flush_now(FlushStateMode::IfNeeded));
        rig.on_v(|v| v.flush_now(FlushStateMode::Always));
        let marks = obs.marks();
        let begun = marks.iter().filter(|m| matches!(m, ObsMark::FlushBegin { .. })).count();
        assert_eq!(
            begun,
            if in_ibd {
                1
            } else {
                2
            },
            "in IBD: {in_ibd}, marks {marks:?}"
        );
        rig.assert_no_abort();
    }
}

/// The IBD latch after one flush of a pruned node that left IBD while it imported (W-P3b's
/// shape): the blocks connect while `fImporting` is set, so no read latches, then the import
/// ends with the tip recent and V runs `flush` with the automatic check armed (or, under
/// `reindex`, Core's `fReindex`). Whether a later read on a clock that has made the tip old says
/// IBD (the latch open), and V's and W's marks.
fn latch_after_a_pruned_flush(
    reindex: bool,
    flush: impl FnOnce(&mut V, u32) -> Result<(), Fatal> + Send + 'static,
) -> (bool, Vec<ObsMark>) {
    let s = Stores::pruned();
    let obs = Arc::new(RecordingObserver::default());
    let on = Arc::new(AtomicBool::new(false));
    let seams = Seams {
        prune: s.prune_host(0, &on),
        obs: obs.clone(),
        synchronous: true,
        reindex,
        ..Seams::default()
    };
    // The clock ten minutes past the tip, within the 6 hours of `DEFAULT_MAX_TIP_AGE`.
    const N: u32 = 400;
    let mut rig = Rig::with(RigConfig {
        after_genesis: Some(i64::from(N * SPACING) + 600),
        ..s.config_with(seams)
    });
    rig.on_v(|v| {
        v.loading.set(Some(true), None);
        Ok(())
    });
    let chain = padded_branch(&rig.p, &rig.genesis, N);
    for m in &chain {
        rig.accept(m);
    }
    let tip = chain.last().expect("a tip");
    let (height, reader) = (tip.height, rig.v.reader().clone());
    let old = dash_clock::NodeSeconds(i64::from(tip.time()) + 3 * 24 * 60 * 60);
    let ibd_at_old = move || reader.is_initial_block_download(&reader.snapshot(), old, false);
    assert!(ibd_at_old(), "the blocks connected in IBD, unlatched");
    rig.on_v(|v| {
        v.loading.set(Some(false), None);
        Ok(())
    });
    on.store(true, Ordering::SeqCst);
    rig.on_v(move |v| {
        assert!(!v.chain.is_ibd_private_unlatched(v.now(), false), "the tip is recent");
        flush(v, height)
    });
    let open = ibd_at_old();
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    rig.assert_no_abort();
    (open, obs.marks())
}

/// The prune decisions' manual heights in `marks`.
fn prune_decisions(marks: &[ObsMark]) -> Vec<Option<u32>> {
    marks
        .iter()
        .filter_map(|m| match m {
            ObsMark::PruneDecision {
                manual,
                ..
            } => Some(*manual),
            _ => None,
        })
        .collect()
}

/// The automatic prune selection reads IBD through the latch, as Core's `FindFilesToPrune(...,
/// IsInitialBlockDownload())`: a flush that runs it on a recent tip latches, and a later clock
/// that makes the tip old is no longer IBD.
// Core: validation.cpp:3027 at v24.0.0
#[test]
fn the_automatic_prune_selection_latches_ibd() {
    let (open, marks) =
        latch_after_a_pruned_flush(false, |v, _| v.flush_now(FlushStateMode::Always));
    assert_eq!(prune_decisions(&marks), [None], "{marks:?}");
    assert!(!open, "the automatic selection latched");
}

/// A manual prune (`pruneblockchain`) reads no IBD (`FindFilesToPruneManual`), so its flush
/// leaves the latch open, the automatic check armed beside it.
// Core: validation.cpp:3008-3013 at the pin
#[test]
fn a_manual_prune_flush_does_not_latch_ibd() {
    let (open, marks) =
        latch_after_a_pruned_flush(false, |v, h| v.flush_at_site(FlushStateMode::None, Some(h)));
    assert!(matches!(prune_decisions(&marks)[..], [Some(_)]), "{marks:?}");
    assert!(open, "a manual prune left the latch open");
}

/// Under `fReindex` no prune is selected, so the flush reads no IBD to latch.
// Core: validation.cpp:2989 at the pin
#[test]
fn a_flush_under_reindex_does_not_latch_ibd() {
    let (open, marks) =
        latch_after_a_pruned_flush(true, |v, _| v.flush_now(FlushStateMode::Always));
    assert_eq!(prune_decisions(&marks), [], "{marks:?}");
    assert!(open, "a flush under reindex left the latch open");
}

/// The node's flush inputs until MP1 and CH6: no mempool, no reindex.
#[test]
fn no_inputs_is_no_mempool_and_no_reindex() {
    assert_eq!((NoInputs.mempool_usage(), NoInputs.reindex()), (0, false));
}

/// The subscriber's next event within [`PROMPT`], left in its queue.
fn next_event(feed: &mut dyn SubscriberFeed, wake: &Parker) -> bool {
    let deadline = Instant::now() + PROMPT;
    loop {
        match feed.next() {
            Next::Event(_) => return true,
            Next::Control(_) => {}
            Next::Empty => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() || !wake.park_timeout(left) {
                    return false;
                }
            }
        }
    }
}

/// Opus r1 M-1 (b4 D81): under `-dbcrashratio` (synchronous flushes) V's in-hold drain answers a
/// `FlushDue` with a flush and waits for its release on V's wake, which is SB5's producer wake
/// too. The subscriber's pop during that wait is answered there (SB5's pump: the stepped event
/// reaches the subscriber while W is still held), and W's release ends the wait; the drain then
/// sees its fanout emitted rather than parking on a wake no one rings again.
#[test]
fn a_flush_in_the_in_hold_drain_leaves_the_drain_awake() {
    let s = Stores::new();
    let seams = Seams {
        synchronous: true,
        ..Seams::default()
    };
    let rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        ..s.config_with(seams)
    });
    let chain = branch(&rig.p, &rig.genesis, 2, 0);
    let feeds = rig.v.query(|q| q.chain.feeds()).recv().expect("V");
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(&feeds).tip();
    let Ok(Handoff::Live {
        mut feed,
        ..
    }) = feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(()))
    else {
        panic!("subscribe_at")
    };
    let wake = Parker::new();
    feed.set_waker(Notify::Thread(wake.clone()));
    rig.accept(&chain[0]);
    assert!(next_event(&mut *feed, &wake), "block 1's step");
    // The queue is full: block 2's step waits in V's in-hold drain. The step's IF_NEEDED
    // decision was made before the drain, so once V parks there only the drain's `answer_due`
    // can take the `FlushDue`.
    let drained = Arc::new(AtomicBool::new(false));
    let at = drained.clone();
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            if stage == Stage::Drain {
                at.store(true, Ordering::SeqCst);
            }
        }));
        Ok(())
    });
    let replied = rig.v.process_block(chain[1].block.clone(), true, None, true);
    assert!(until(|| drained.load(Ordering::SeqCst)), "block 2's step parks in the drain");
    assert!(replied.recv_timeout(Duration::ZERO).is_none(), "V waits in its drain");
    s.fs.gate.arm();
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "the drain answers FlushDue with a job");
    // Time for V to park in its wait for the release.
    std::thread::sleep(Duration::from_millis(200));
    feed.pop().expect("block 1's step");
    let pumped = next_event(&mut *feed, &wake);
    s.fs.gate.release();
    assert!(pumped, "V's wait for W pumps the fanout while W is held");
    let Some(r) = replied.recv_timeout(PROMPT) else {
        // V is parked for good: its stop would hang too.
        std::mem::forget(rig);
        panic!("V's drain parked after the release");
    };
    assert!(r.expect("V replies").accepted);
    feed.pop().expect("block 2's step");
    feeds.deregister(id);
    rig.assert_no_abort();
}

/// What [`ChainHandle::set_before_block_tail`]'s hook saw: every block it ran for, and for
/// `staged`, what its flush read back.
///
/// [`ChainHandle::set_before_block_tail`]: crate::chain::ChainHandle::set_before_block_tail
#[cfg(feature = "test-support")]
#[derive(Default)]
struct TailHook {
    ran: Vec<BlockHash>,
    staged: Vec<Option<StagedRecord>>,
}

/// Sets a hook that records each block it runs for and runs the flush for `staged`.
#[cfg(feature = "test-support")]
fn hook_tails(rig: &Rig, staged: BlockHash) -> Arc<Mutex<TailHook>> {
    let seen = Arc::new(Mutex::new(TailHook::default()));
    let hook = seen.clone();
    rig.v.set_before_block_tail(Some(Arc::new(move |stage: &mut BlockTailStage<'_>| {
        let mut seen = hook.lock().expect("hook");
        seen.ran.push(stage.hash());
        if stage.hash() == staged {
            seen.staged.push(stage.flush_and_read_back().expect("the flush"));
        }
    })));
    seen
}

/// `staged`'s read-back of `m`, on parent `p`, as the hook saw it before `m`'s ABC: the record
/// written by the hook's own flush, at its released epoch, `HAVE_DATA` and `VALID_TRANSACTIONS`
/// with no failed bit, and the coins at `p` with no transition marker.
#[cfg(feature = "test-support")]
fn assert_staged_before_connect(staged: &StagedRecord, m: &Mined, p: &Mined) {
    assert_eq!(staged.tip, Some(p.hash()), "V's tip at the hook is the parent");
    assert_eq!(staged.before, None, "AcceptBlock's NONE flush wrote nothing");
    assert_eq!(staged.released_epoch, Some(staged.snapshot_epoch), "read at the copy point");
    assert_eq!((staged.coins_tip, staged.head_blocks), (Some(p.hash()), None));
    let record = BlockIndexRecord::decode(staged.record.as_deref().expect("durable")).unwrap();
    assert_eq!((record.height, Header::new(record.header).hash()), (m.height, m.hash()));
    assert_eq!(record.n_tx, 1);
    let status = record.status;
    assert!(status.has(Status::HAVE_DATA) && !status.has(Status::HAVE_UNDO), "{status:?}");
    assert_eq!(status.bits() & Status::VALID_MASK, Status::VALID_TRANSACTIONS, "{status:?}");
    assert!(!status.has(Status::FAILED_MASK | Status::CONFLICT_CHAINLOCK), "{status:?}");
    assert_eq!((record.file, record.data_pos.is_some()), (Some(0), true));
}

/// CH3b D223: the hook at a submitted block's ABC tail runs the copy point's ALWAYS flush on V and
/// reads the block's record back from the database, not V's index: absent before the flush,
/// present after it, the block accepted and not connected, V's tip and the coins at its parent.
/// Then the hook returns and ABC connects the block as usual. It ran once, for that block.
#[test]
#[cfg(feature = "test-support")]
fn a_block_tail_hook_reads_the_block_back_from_the_database_after_an_always_flush() {
    let s = Stores::new();
    let (rig, chain) = node(&s, 3);
    let parent = chain.last().expect("a tip");
    let next = &branch(&rig.p, parent, 1, 0)[0];
    let seen = hook_tails(&rig, next.hash());
    rig.accept(next);
    let seen = seen.lock().expect("hook");
    assert_eq!(seen.ran, [next.hash()]);
    let [Some(staged)] = &seen.staged[..] else {
        panic!("one read-back: {:?}", seen.staged)
    };
    assert_staged_before_connect(staged, next, parent);
    assert_eq!(tip(&rig), rig.id(next), "ABC connected it after the hook");
    assert_eq!(durable_connected(&s, next), Some(false), "the hook's record, not yet connected");
    rig.assert_no_abort();
}

/// CH3b D223, Sol r1 m-1: a block served at a release point defers its ABC tail (§2.4), which
/// runs the hook too, once the class C operation is over. Here `invalidateblock`'s walk serves
/// it: the hook sees V's tip at the walk's end, not where the release point was, and the
/// block, on genesis with no more work than that tip, stays unconnected.
#[test]
#[cfg(feature = "test-support")]
fn a_deferred_tail_runs_the_hook_too() {
    let s = Stores::new();
    let (rig, main) = node(&s, 3);
    let side = super::support::child(&rig.p, &rig.genesis, 2);
    let seen = hook_tails(&rig, side.hash());
    let block = side.block.clone();
    let stash =
        replies_at_first_release(&rig, move |v| vec![v.process_block(block, true, None, true)]);
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    let [r] = &take(&stash)[..] else {
        unreachable!()
    };
    assert!(r.accepted && r.state.is_valid(), "{r:?}");
    let seen = seen.lock().expect("hook");
    assert_eq!(seen.ran, [side.hash()]);
    let [Some(staged)] = &seen.staged[..] else {
        panic!("one read-back: {:?}", seen.staged)
    };
    assert_staged_before_connect(staged, &side, &main[0]);
    assert_eq!(tip(&rig), rig.id(&main[0]));
    rig.assert_no_abort();
}

/// CH3b D223, Opus r1 m-1: a hook whose flush fails stops V with that failure even when the hook
/// swallows the error and returns, as the `Flush` command's failure does: the block's tail
/// returns it before its ABC, the reply disconnects, and the abort sink has one report.
#[test]
#[cfg(feature = "test-support")]
fn a_hook_flush_failure_stops_v_with_one_report() {
    /// `NoFlush`, whose staged read-back fails.
    struct StagingFails;
    impl FlushSite for StagingFails {
        fn at_site(
            &mut self,
            mode: FlushStateMode,
            host: &mut FlushHost<'_>,
        ) -> Result<SiteDecision, Fatal> {
            crate::chain::NoFlush.at_site(mode, host)
        }
        fn after_step(
            &mut self,
            decided: SiteDecision,
            host: &mut FlushHost<'_>,
        ) -> Result<(), Fatal> {
            crate::chain::NoFlush.after_step(decided, host)
        }
        fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
            Ok(())
        }
        fn admit(&self) -> Result<(), Fatal> {
            crate::chain::NoFlush.admit()
        }
        fn staged_record(
            &mut self,
            _: &BlockHash,
            _: &mut FlushHost<'_>,
        ) -> Result<Option<StagedRecord>, Fatal> {
            Err(Fatal::new("the hook's flush failed"))
        }
    }
    let mut rig = Rig::with(RigConfig {
        flush: Some(Box::new(StagingFails)),
        ..RigConfig::default()
    });
    let block = branch(&rig.p, &rig.genesis, 1, 0).remove(0);
    rig.v.set_before_block_tail(Some(Arc::new(|stage: &mut BlockTailStage<'_>| {
        assert!(stage.flush_and_read_back().is_err(), "the site's failure");
    })));
    rig.submit_stopping(&block, "the tail returns the hook's failure");
    rig.stop_on("the hook's flush failed");
}

/// A site with no database (the tests' `NoFlush`) stages nothing, and the block connects.
#[test]
#[cfg(feature = "test-support")]
fn a_site_without_a_database_stages_nothing() {
    let rig = Rig::new();
    let next = branch(&rig.p, &rig.genesis, 1, 0).remove(0);
    let seen = hook_tails(&rig, next.hash());
    rig.accept(&next);
    let seen = seen.lock().expect("hook");
    assert_eq!((&seen.ran[..], &seen.staged[..]), (&[next.hash()][..], &[None][..]));
    assert_eq!(tip(&rig), rig.id(&next));
}

/// CH5-5's tests that read the process-wide live-copy counters exactly (D3-3, §6.2): each runs
/// alone in a child process of this test binary, so no other test counts meanwhile.
#[cfg(feature = "test-hooks")]
#[test]
fn the_live_copy_seams_run_alone() {
    for name in [
        "the_flush_hooks_job_done_follows_after_bk",
        "the_copy_points_counter_snapshot_is_atomic",
        "one_scripted_flush_call_adds_exactly_one_job",
        "a_scripted_flush_fails_at_its_deadline_while_v_is_parked",
        "a_call_after_a_timeout_in_vs_take_returns_its_own_job",
    ] {
        let path = format!("chain::tests::node_stores::{name}");
        let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
            .args([&path, "--exact", "--ignored", "--test-threads=1", "--nocapture"])
            .env(ALONE, "1")
            .output()
            .expect("the child runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // A typo'd name or a filter runs nothing and still exits 0.
        let ran_one = text.contains("running 1 test\n") && text.contains(" 1 passed;");
        assert!(out.status.success() && ran_one, "{name}:\n{text}");
    }
}

/// Set in the child [`the_live_copy_seams_run_alone`] starts.
#[cfg(feature = "test-hooks")]
const ALONE: &str = "CH5_5_LIVE_COPY_ALONE";

/// Whether this test runs alone; prints why it does nothing otherwise.
#[cfg(feature = "test-hooks")]
fn alone() -> bool {
    let alone = std::env::var_os(ALONE).is_some();
    if !alone {
        eprintln!("run by the_live_copy_seams_run_alone only: it reads process-wide counters");
    }
    alone
}

/// CH5-5 §6.2's acknowledgement: the `flush` hook's scripted flush is an ordinary one, so W's
/// `JobDone`, which `flush.done` waits for, comes only once the post-Bk hook has returned, and
/// the job's steps end with the release and then that hook.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "alone: run by the_live_copy_seams_run_alone"]
fn the_flush_hooks_job_done_follows_after_bk() {
    use dashd_store::livecopy::COUNTERS;

    if !alone() {
        return;
    }
    let s = Stores::new();
    let tuning = Arc::new(HookTuning::default());
    let seams = Seams {
        tuning: tuning.clone(),
        ..Seams::default()
    };
    let (rig, _) = node_with(&s, 3, seams);
    let gate = Arc::new(BkGate::default());
    *tuning.hold.lock().expect("hold") = Some(gate.clone());
    let before = COUNTERS.events().flush_jobs_done;
    COUNTERS.request_scripted_flush();
    assert_eq!(rig.v.flush(FlushStateMode::None).recv(), Ok(()));
    let deadline = Instant::now() + Duration::from_secs(120);
    let w = loop {
        if let Some(w) = *gate.entered.lock().expect("entered") {
            break w;
        }
        assert!(Instant::now() < deadline, "W never ran the post-Bk hook");
        std::thread::sleep(Duration::from_millis(1));
    };
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(COUNTERS.events().flush_jobs_done, before, "no JobDone while the hook runs");
    *gate.open.lock().expect("gate") = true;
    gate.opened.notify_all();
    let job = loop {
        if let Some(job) =
            COUNTERS.jobs_done().into_iter().find(|j| j.thread == w && j.done > before)
        {
            break job;
        }
        assert!(Instant::now() < deadline, "no JobDone after the hook");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(job.scripted, "{job:?}");
    assert!(job.steps.ends_with(&["Release".to_owned(), "AfterBk".to_owned()]), "{job:?}");
    rig.assert_no_abort();
}

/// OD-CH5-22 (a): one [`ChainHandle::scripted_flush`] call adds exactly one job, the scripted
/// one, with V idle and with a job in flight (the cause kept pending till it is done). Nothing
/// the call sent before the freeze starts a further job. A call that timed out after V took its
/// request leaves a job that freezes later; the next call waits for it and returns its own.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "alone: run by the_live_copy_seams_run_alone"]
fn one_scripted_flush_call_adds_exactly_one_job() {
    use dashd_store::livecopy::COUNTERS;

    if !alone() {
        return;
    }
    const PATIENCE: Duration = Duration::from_secs(120);
    let done = || COUNTERS.events().flush_jobs_done;
    let settled = |n: u64, case: &str| {
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(done(), n, "{case}: no further job after the call");
    };
    let s = Stores::new();
    let (rig, _) = node(&s, 3);

    let j0 = done();
    let job = rig.v.scripted_flush(PATIENCE).expect("the scripted job");
    assert!(job.scripted, "{job:?}");
    assert_eq!(job.done, j0 + 1, "idle: the call adds exactly the scripted job");
    settled(j0 + 1, "idle");

    s.fs.gate.arm();
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    let (j1, frozen) = (done(), COUNTERS.scripted_frozen());
    let v = rig.v.clone();
    let call = std::thread::spawn(move || v.scripted_flush(PATIENCE));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(COUNTERS.scripted_frozen(), frozen, "the cause waits for the job in flight");
    s.fs.gate.release();
    let job = call.join().expect("the call returns").expect("the scripted job");
    assert!(job.scripted, "{job:?}");
    assert_eq!(job.done, j1 + 2, "in flight: that job, then exactly the scripted one");
    settled(j1 + 2, "in flight");

    s.fs.gate.arm();
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    let j2 = done();
    let timed_out = rig.v.scripted_flush(Duration::from_millis(300));
    assert_eq!(timed_out.err().as_deref(), Some("no scripted job was frozen"));
    let v = rig.v.clone();
    let call = std::thread::spawn(move || v.scripted_flush(PATIENCE));
    std::thread::sleep(Duration::from_millis(300));
    s.fs.gate.release();
    let job = call.join().expect("the call returns").expect("the scripted job");
    assert!(job.scripted, "{job:?}");
    assert_eq!(job.done, j2 + 3, "after a timeout: that job, the earlier request's, then this one");
    settled(j2 + 3, "after a timeout");
    rig.assert_no_abort();
}

/// Sol r1 C-1: `scripted_flush`'s patience bounds its waits for V. With V parked under
/// `TestUnderV`, a call with 100 ms of patience returns `Err` within that plus a small margin
/// (not when V is released); its request, never taken, is withdrawn, so once V runs again the
/// next call gets exactly one job, its own.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "alone: run by the_live_copy_seams_run_alone"]
fn a_scripted_flush_fails_at_its_deadline_while_v_is_parked() {
    use dashd_store::livecopy::COUNTERS;

    if !alone() {
        return;
    }
    const PATIENCE: Duration = Duration::from_millis(100);
    const MARGIN: Duration = Duration::from_millis(400);
    let done = || COUNTERS.events().flush_jobs_done;
    let s = Stores::new();
    let (rig, _) = node(&s, 3);
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let v_done = on_v_async(&rig, move |_| {
        entered_tx.send(()).expect("the test waits for V");
        release_rx.recv_timeout(Duration::from_secs(30)).expect("the test releases V");
        Ok(())
    });
    entered.recv_timeout(PROMPT).expect("V parked");
    let j0 = done();
    let v = rig.v.clone();
    let (answer_tx, answer) = mpsc::channel();
    let caller = std::thread::spawn(move || {
        let begin = Instant::now();
        let result = v.scripted_flush(PATIENCE);
        answer_tx.send((begin.elapsed(), result)).expect("the test waits for the answer");
    });
    let prompt = answer.recv_timeout(PATIENCE + MARGIN);
    release.send(()).expect("V waits for the release");
    assert_eq!(v_done.recv_timeout(PROMPT), Ok(true), "V ran the parking step");
    caller.join().expect("the caller returns");
    let (elapsed, result) = prompt.expect("no answer within the deadline and margin");
    assert_eq!(result.err().as_deref(), Some("no scripted job was frozen"), "after {elapsed:?}");
    assert!(elapsed < PATIENCE + MARGIN, "answered after {elapsed:?}");

    let job = rig.v.scripted_flush(Duration::from_secs(120)).expect("the scripted job");
    assert!(job.scripted, "{job:?}");
    assert_eq!(job.done, j0 + 1, "after the parked call: exactly this call's job");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(done(), j0 + 1, "no further job after the call");
    rig.assert_no_abort();
}

/// Sol r2 C-3: V's take claims a request and records it as taken in one step. A job in flight
/// (held at W's gate) keeps a scripted cause pending; V parks right after taking caller A's
/// request, and A times out there. Caller B, started once A has returned, must wait for A's
/// ticket rather than publish its own, so the two never share a job: the job in flight, then A's
/// ticket's job, then B's, which B returns. With the claim split (TR), B publishes while A's
/// ticket is in neither place, V takes both into the pending cause, and B returns that one job.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "alone: run by the_live_copy_seams_run_alone"]
fn a_call_after_a_timeout_in_vs_take_returns_its_own_job() {
    use dashd_store::livecopy::COUNTERS;

    if !alone() {
        return;
    }
    let done = || COUNTERS.events().flush_jobs_done;
    let wait_while = |limit: Duration, busy: &dyn Fn() -> bool| {
        let until = Instant::now() + limit;
        while busy() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let s = Stores::new();
    let (rig, _) = node(&s, 3);
    s.fs.gate.arm();
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    let (j0, frozen0, issued) =
        (done(), COUNTERS.scripted_frozen(), COUNTERS.scripted_tickets().issued);
    let (a_ticket, b_ticket) = (issued + 1, issued + 2);

    COUNTERS.take_park().arm();
    let v = rig.v.clone();
    let a = std::thread::spawn(move || v.scripted_flush(Duration::from_millis(300)));
    let parked = COUNTERS.take_park().wait_parked(PROMPT);
    let a_result = a.join();
    let v = rig.v.clone();
    let b = std::thread::spawn(move || v.scripted_flush(Duration::from_secs(60)));
    // B publishes now only if it missed A's taken ticket.
    wait_while(Duration::from_millis(500), &|| COUNTERS.scripted_tickets().requested.is_none());
    let b_published_early = COUNTERS.scripted_tickets().requested == Some(b_ticket);
    COUNTERS.take_park().release();
    // Whatever B published, V takes it while the job in flight keeps the cause pending.
    wait_while(Duration::from_secs(5), &|| COUNTERS.scripted_tickets().requested.is_some());
    s.fs.gate.release();
    let b_result = b.join();
    std::thread::sleep(Duration::from_millis(1500));
    let after = COUNTERS.scripted_tickets();
    let scripted_jobs: Vec<_> =
        COUNTERS.jobs_done().into_iter().filter(|j| j.done > j0 && j.scripted).collect();

    assert!(parked, "V parked right after taking A's request");
    let a_result = a_result.expect("A returns");
    assert_eq!(a_result.err().as_deref(), Some("no scripted job was frozen"), "A errs");
    assert!(!b_published_early, "B waited for A's taken ticket");
    let job = b_result.expect("B returns").expect("B's job");
    assert_eq!(job.tickets, [b_ticket], "B's job carries B's ticket only: {job:?}");
    assert_eq!(job.done, j0 + 3, "the job in flight, A's ticket's job, then B's: {job:?}");
    assert_eq!((after.requested, &after.taken[..], &after.running[..]), (None, &[][..], &[][..]));
    assert_eq!(COUNTERS.scripted_frozen() - frozen0, 2, "one scripted job per ticket");
    let tickets: Vec<_> = scripted_jobs.iter().map(|j| j.tickets.clone()).collect();
    assert_eq!(tickets, [vec![a_ticket], vec![b_ticket]], "{scripted_jobs:?}");
    assert_eq!(done(), j0 + 3, "no further job after B returned");
    rig.assert_no_abort();
}

/// CH5-5 D3-3: the copy point's counter snapshot is taken in step 11, before the release. The
/// test queues a block for V while W runs the barrier; W, once it has published the release,
/// parks at `AfterRelease` until V has connected that block and acknowledged it. The snapshot the
/// reply carries excludes that activation, and the live counters include it (+1). M15 (the
/// snapshot taken after the sync point) includes it.
#[cfg(feature = "test-hooks")]
#[test]
#[ignore = "alone: run by the_live_copy_seams_run_alone"]
fn the_copy_points_counter_snapshot_is_atomic() {
    use dashd_store::livecopy::COUNTERS;

    if !alone() {
        return;
    }
    type Park = (mpsc::Sender<()>, mpsc::Receiver<()>);
    let s = Stores::new();
    let tuning = Arc::new(HookTuning::default());
    let slot: Arc<Mutex<Option<Park>>> = Arc::default();
    let hook_slot = slot.clone();
    let after_release: crate::flush::AfterRelease = Arc::new(move || {
        let park = hook_slot.lock().expect("slot").take();
        if let Some((parked, go)) = park {
            parked.send(()).expect("the test waits");
            go.recv_timeout(SOON).expect("the test releases W");
        }
    });
    let seams = Seams {
        tuning: tuning.clone(),
        live_copy: true,
        after_release: Some(after_release),
        ..Seams::default()
    };
    let (rig, chain) = node_with(&s, 3, seams);
    let next = branch(&rig.p, chain.last().expect("a tip"), 1, 0).remove(0);

    // 1. While W runs step 11's barrier, a block is queued for V.
    let (queued_tx, queued) = mpsc::channel();
    let (v, block) = (rig.v.clone(), next.block.clone());
    *tuning.on_barrier.lock().expect("on_barrier") = Some(Box::new(move || {
        queued_tx.send(v.process_block(block, true, None, true)).expect("the test waits");
    }));
    let (parked_tx, parked) = mpsc::channel();
    let (go, go_rx) = mpsc::channel();
    *slot.lock().expect("slot") = Some((parked_tx, go_rx));
    let at_copy_point = COUNTERS.tripwire();
    let hash_type = CoinStatsHashType::HashSerialized;
    let job = rig.v.coins_stats(hash_type, Arc::new(|| false), None).recv().expect("V answers");
    let job = job.expect("a scan");

    // 2. W has published the release and parks at `AfterRelease`.
    parked.recv_timeout(SOON).expect("W parks at AfterRelease");
    // 3. V connects the queued block and acknowledges it; only then is W released.
    let reply = queued.recv_timeout(SOON).expect("the barrier queued the block");
    let r = reply.recv().expect("V connects the block");
    assert!(r.accepted && r.state.is_valid(), "{r:?}");
    assert_eq!(tip(&rig), rig.id(&next));
    assert_eq!(COUNTERS.tripwire().activations, at_copy_point.activations + 1, "V's activation");
    go.send(()).expect("W waits");

    let snapshot = job.live_copy().expect("the copy point's counter snapshot").tripwire;
    assert_eq!(snapshot, at_copy_point, "the reply's snapshot excludes the activation");
    assert_eq!(COUNTERS.tripwire().activations, snapshot.activations + 1, "the live read has it");
    job.wait().expect("the scan");
    rig.assert_no_abort();
}

// ---- BL-559: ActivateBestChainStep's second AbortNode after a failed disconnect flush --------

/// [`PoolsOnly`], and the reports the node's sink held at each `MaybeUpdateMempoolForReorg`.
/// Clones share the logs: a test keeps one while V owns another.
#[derive(Clone)]
struct DrainLog {
    pools: PoolsOnly,
    sink: Arc<RecordingAbort>,
    at_drain: Arc<Mutex<Vec<usize>>>,
}

impl DrainLog {
    fn new(s: &Stores) -> DrainLog {
        DrainLog {
            pools: PoolsOnly::default(),
            sink: s.sink.clone(),
            at_drain: Arc::default(),
        }
    }

    /// Forgets the calls so far.
    fn take(&self) {
        self.pools.take_calls();
        self.at_drain.lock().expect("drains").clear();
    }
}

impl MempoolSink for DrainLog {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        self.pools.new_disconnect_pool()
    }

    fn maybe_update_for_reorg_at(
        &mut self,
        pool: Box<dyn DisconnectPool>,
        tip_list: Option<&Arc<dash_dml::MnList>>,
    ) {
        self.at_drain.lock().expect("drains").push(self.sink.reports().len());
        self.pools.maybe_update_for_reorg_at(pool, tip_list);
    }

    fn remove_for_block_at(
        &mut self,
        vtx: &[dash_consensus::TxRef],
        height: u32,
        tip_list: &Arc<dash_dml::MnList>,
    ) {
        self.pools.remove_for_block_at(vtx, height, tip_list);
    }

    fn remove_expired_asset_unlock(&mut self, height: u32) {
        self.pools.remove_expired_asset_unlock(height);
    }

    fn remove_recursive(&mut self, tx: &dash_consensus::TxRef, reason: RemovalReason) {
        self.pools.remove_recursive(tx, reason);
    }

    fn add_transactions_updated(&mut self, n: u32) {
        self.pools.add_transactions_updated(n);
    }

    fn check(&mut self, next_height: u32) {
        self.pools.check(next_height);
    }
}

/// The one drain since the last take (`Open`, then an unadded `Update`): its txids and the
/// reports the sink held at it.
fn the_failed_steps_drain(pool: &DrainLog) -> (Vec<TxHash>, usize) {
    let (calls, at_drain) =
        (pool.pools.take_calls(), pool.at_drain.lock().expect("drains").clone());
    let ([PoolCall::Open, PoolCall::Update(false, txids)], [reports]) = (&calls[..], &at_drain[..])
    else {
        panic!("one step, its pool drained unadded: {calls:?}, reports {at_drain:?}");
    };
    (txids.clone(), *reports)
}

/// The tips of W's freezes that began a full flush, in order.
fn freezes(obs: &RecordingObserver) -> Vec<BlockHash> {
    let marks = obs.marks();
    marks
        .iter()
        .filter_map(|m| match m {
            ObsMark::FlushBegin {
                new,
                ..
            } => Some(*new),
            _ => None,
        })
        .collect()
}

/// Main 2 and side `side_len` (seed 99) on `rig`, main and side[..2] accepted: V's tip is main[1].
fn two_branches(rig: &Rig, side_len: u32) -> (Vec<Mined>, Vec<Mined>) {
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, side_len, 99);
    for block in main.iter().chain(&side[..2]) {
        rig.accept(block);
    }
    assert_eq!(tip(rig), rig.id(&main[1]));
    (main, side)
}

/// An importing rig whose next flush site is the first out of IBD: [`two_branches`] accepted
/// while importing, then `done_importing`.
struct Reorg {
    rig: Rig,
    main: Vec<Mined>,
    side: Vec<Mined>,
    pool: DrainLog,
    obs: Arc<RecordingObserver>,
    /// V's thread, and its line count after the accepts.
    v: std::thread::ThreadId,
    accepted: usize,
}

/// [`reorg_rig_with`] on V's bell, side 2: a precious side[1] whose reorg's first disconnect is
/// the first flush site out of IBD.
fn reorg_rig(s: &Stores) -> Reorg {
    reorg_rig_with(s, false, 2)
}

/// A [`Reorg`] on `s`, V taking no bell ([`NoBell`]) if `parker`, with side `side_len`.
fn reorg_rig_with(s: &Stores, parker: bool, side_len: u32) -> Reorg {
    install_accept_log();
    let pool = DrainLog::new(s);
    let obs = Arc::new(RecordingObserver::default());
    let seams = Seams {
        obs: obs.clone(),
        parker,
        ..Seams::default()
    };
    let rig = importing_rig_with(s, false, seams, Some(Box::new(pool.clone())));
    let (main, side) = two_branches(&rig, side_len);
    let v = rig.v_thread();
    let accepted = lines_on(v).len();
    pool.take();
    done_importing(&rig);
    Reorg {
        rig,
        main,
        side,
        pool,
        obs,
        v,
        accepted,
    }
}

impl Reorg {
    /// `preciousblock(side[1])`, whose reply disconnects as V stops; V's exit.
    fn precious_side_stops(&mut self) -> crate::chain::actor::VExit {
        let reply = self.rig.v.precious_block(self.side[1].hash()).recv();
        assert_eq!(reply, Err(WaitError::Disconnected));
        self.rig.stop()
    }
}

/// `ProcessNewBlock`'s failure line for `text`.
fn pnb_failed(text: &str) -> String {
    format!("ProcessNewBlock: ActivateBestChain failed ({text})")
}

/// BL-559 (a): an ABC whose disconnect's own waited freeze fails at W's write gives Core's two
/// reports, in Core's order: W's `AbortNode`, the step's pool drain, then
/// `ActivateBestChainStep`'s "Failed to disconnect block". The parity it pins is per failing
/// write, on the condition that Core also writes and fails at its `DisconnectTip` flush: then
/// Core's reports, their order and its `state` text are these. (What makes this run's freeze is
/// the leaving-IBD flush, CH2 D1's timing.) The stop text stays W's, as Core's `state` keeps its
/// first reason; nothing more is durable, and V never logs the disconnect's `UpdateTip`.
// Core: validation.cpp:3315-3317 (v24.0.0), :3627-3633 (v24.0.0), :2250-2254 (v24.0.0);
// consensus/validation.h:92-97
#[test]
fn a_failed_disconnect_flush_in_abc_reports_cores_second_abort() {
    let s = Stores::new();
    let mut r = reorg_rig(&s);
    let (durable, index) = (durable_tip(&s), durable_record(&s, &r.main[1]));
    s.db.inner().crash_at_write(1);
    let exit = r.precious_side_stops();
    // 1-3: W's failure latched at its write, both reports, and Core's `state.ToString()`.
    assert_stopped_on_flush(&s, exit.fatal, true);
    // 4: the failed block's transactions never reached the pool. 5: the drain comes after W's
    // report and before ABC's.
    let (txids, at_drain) = the_failed_steps_drain(&r.pool);
    assert!(txids.is_empty(), "the pool lacks main[1]'s transactions: {txids:?}");
    assert_eq!(at_drain, 1, "the drain follows W's report and precedes ABC's");
    // 6: nothing more is durable.
    assert_eq!((durable_tip(&s), durable_record(&s, &r.main[1])), (durable, index));
    // 7: the failure is inside `DisconnectTip`, at the freeze after its step, `(main[0], seq)`.
    let lines = lines_on(r.v).split_off(r.accepted);
    assert!(!lines.iter().any(|l| l.starts_with("UpdateTip")), "{lines:#?}");
    assert_eq!(freezes(&r.obs), [r.main[0].hash()]);
}

/// The carried case: a [`Reorg`] with side 3 (V taking no bell if `parker`), W's next write
/// failing, then `ProcessNewBlock(side[2])`. Its accept's NONE flush is the first site out of
/// IBD: the leaving-IBD write at V's tip, main[1] (`policy.rs:188-193`), which fails; W reports
/// it, and V, stopped on it, carries on past the accept's flush (BL-279) to the block's ABC
/// tail. W's text.
fn carried_flush_failure(s: &Stores, parker: bool) -> (Reorg, String) {
    let r = reorg_rig_with(s, parker, 3);
    s.db.inner().crash_at_write(1);
    let reply = r.rig.v.process_block(r.side[2].block.clone(), true, None, true).recv();
    assert_eq!(reply, Err(WaitError::Disconnected));
    let text = reported_store_failure(s, "chainstate write");
    // The failing freeze is the accept's, at V's tip before any disconnect.
    assert_eq!(freezes(&r.obs), [r.main[1].hash()]);
    let lines = lines_on(r.v);
    assert!(lines.contains(&pnb_failed(&text)), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.contains(&format!("failed ({DISCONNECT_FAILED})"))));
    (r, text)
}

/// BL-559 (b): `ProcessNewBlock`'s line names W's text after both reports, for a W failure
/// carried into the reorg. The first failure is the leaving-IBD write ([`carried_flush_failure`]).
/// The reorg's disconnect then reads the same latched failure at its site's decision, before its
/// step, and `ActivateBestChainStep`'s `AbortNode` follows. V takes no bell ([`NoBell`]): with
/// one, the tail ABC's first release point (`actor.rs`'s `release_point`) answers W's ring and
/// reads the carried failure first, outside any disconnect
/// (`a_carried_flush_failure_read_at_the_release_point_reports_once`). The production order it
/// stands for is a `ProcessBlock` served at an outer ABC's release point (D213(b), BL-535), whose
/// accept carries the failed leaving-IBD freeze, with the outer step's disconnect the first to
/// read the latch
/// (`a_carried_flush_failure_served_at_an_outer_abcs_release_point_gets_the_second_abort`).
// Core: validation.cpp:4865-4867 (v24.0.0), :3315-3317 (v24.0.0), :3632 (v24.0.0), :4910-4913
// (v24.0.0); no Core count: the first failure is rdashd's own leaving-IBD write (DEC-1038)
#[test]
fn process_new_block_logs_the_flush_text_after_both_reports() {
    let s = Stores::new();
    let (mut r, text) = carried_flush_failure(&s, true);
    assert_eq!(s.sink.messages(), [text.as_str(), DISCONNECT_FAILED]);
    let (_txids, at_drain) = the_failed_steps_drain(&r.pool);
    assert_eq!(at_drain, 1, "the drain follows W's report and precedes ABC's");
    assert_logged_before(&lines_on(r.v), &format!("*** {DISCONNECT_FAILED}"), &pnb_failed(&text));
    assert!(r.rig.stop().fatal.is_some());
}

/// BL-559, the carried case's one-report instance (DEC-1036's observing-site rule): on V's bell,
/// the tail ABC's first release point answers W's ring and reads the carried failure before the
/// ABC takes a step (no disconnect pool is opened), outside any disconnect. One report, W's, and
/// W's text is the stop's and the `ProcessNewBlock` line's.
// Core: validation.cpp:4865-4867, :4910-4913 at v24.0.0
#[test]
fn a_carried_flush_failure_read_at_the_release_point_reports_once() {
    let s = Stores::new();
    let (mut r, text) = carried_flush_failure(&s, false);
    assert_eq!(s.sink.messages(), [text.as_str()]);
    let calls = r.pool.pools.take_calls();
    assert!(calls.is_empty(), "no ABC step began: {calls:?}");
    let lines = lines_on(r.v);
    assert!(!lines.iter().any(|l| l.contains(DISCONNECT_FAILED)), "{lines:#?}");
    assert_eq!(r.rig.stop().fatal.map(|f| f.0), Some(text));
}

/// BL-559, the carried case's two-report order on V's bell: a `ProcessNewBlock(side[2])` queued
/// behind `preciousblock(side[1])` is served at the precious ABC's first release point, after its
/// bell check found no ring (D213(b), BL-535). Its accept's NONE flush is the first site out of
/// IBD, the leaving-IBD write at V's tip, main[1], which fails; W reports it and rings V, and V,
/// stopped on it, carries on past (BL-279) and the release point serves nothing more. The ABC
/// then takes its step, and its first disconnect's site decision is the first V site to read the
/// latch: both reports, the step's pool drained between them, W's text the stop's.
// Core: validation.cpp:3771 (v24.0.0), :4865-4867 (v24.0.0), :3315-3317 (v24.0.0), :3627-3633
// (v24.0.0); no Core count: the first failure is rdashd's own leaving-IBD write (DEC-1038)
#[test]
fn a_carried_flush_failure_served_at_an_outer_abcs_release_point_gets_the_second_abort() {
    let s = Stores::new();
    let mut r = reorg_rig_with(&s, false, 3);
    s.db.inner().crash_at_write(1);
    let resume = pause(&r.rig.v);
    let precious = r.rig.v.precious_block(r.side[1].hash());
    let processed = r.rig.v.process_block(r.side[2].block.clone(), true, None, true);
    resume.send(()).expect("V paused");
    assert_eq!(precious.recv_timeout(PROMPT), Some(Err(WaitError::Disconnected)));
    assert_eq!(processed.recv_timeout(PROMPT), Some(Err(WaitError::Disconnected)));
    // The failing freeze is the accept's, at V's tip before any disconnect.
    assert_eq!(freezes(&r.obs), [r.main[1].hash()]);
    assert_stopped_on_flush(&s, r.rig.stop().fatal, true);
    let (txids, at_drain) = the_failed_steps_drain(&r.pool);
    assert_eq!((txids, at_drain), (vec![], 1), "the step began, and drained between the reports");
}

/// BL-559 (c): the disconnect's freeze failing W's space check (`Kept::Flush`, BL-482) gives
/// both reports too, each with Core's user message: the flush's "Disk space is too low!", then
/// ABC's default. The stop text is the flush's.
// Core: validation.cpp:3061-3062 (v24.0.0), :3099-3100 (v24.0.0), :3632 (v24.0.0);
// shutdown.cpp:31-33
#[test]
fn a_disconnect_flush_out_of_space_reports_both_with_cores_user_messages() {
    let s = Stores::new();
    let mut r = reorg_rig(&s);
    s.fs.free.store(0, Ordering::SeqCst);
    let exit = r.precious_side_stops();
    let low = "Disk space is too low!";
    assert_eq!(s.latch.tripped().expect("W's check latched it").site, "chainstate flush");
    let want = [(low.to_owned(), Some(low.to_owned())), (DISCONNECT_FAILED.to_owned(), None)];
    assert_eq!(s.sink.reports(), want);
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some(low));
}

/// BL-559 (d): a disconnect whose flush site reads another store's latched failure
/// (`Kept::Nothing`: a block file's write) adds no report. Core's flush would write and succeed
/// there, and its ABC go on; V stops on the latched failure, reported once by its store (CH3
/// D52, DEC-309). The failure latches after the `preciousblock`'s admission, at its ABC's first
/// release point ([`behind`]; BL-678 refuses a command admitted after it), and V takes no bell
/// ([`NoBell`]): with one, the trip rings V, which stops at the bell before the disconnect reads
/// the latch.
#[test]
fn a_disconnect_seeing_another_stores_failure_adds_no_abort() {
    let s = Stores::new();
    let mut r = reorg_rig_with(&s, true, 2);
    let msg = "Failed to write block";
    let failure = Failure {
        kind: ErrorKind::Io,
        site: BLOCK_FILES_WRITE_SITE,
        message: msg.into(),
    };
    let (latch, sink) = (s.latch.clone(), s.sink.clone());
    let side1 = r.side[1].hash();
    let reply = behind(
        &r.rig,
        |v| v.precious_block(side1),
        move || {
            fail_on_w(&latch, sink, failure, msg.to_owned()).join().expect("W reports");
        },
    );
    assert_eq!(reply.recv(), Err(WaitError::Disconnected));
    let exit = r.rig.stop();
    assert_eq!(s.sink.reports(), [(msg.to_owned(), None)]);
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some(msg));
}

/// BL-559 (e): a W failure latched before the disconnect, which V has not seen, gets the second
/// report where the disconnect's site first reads it. The job is asynchronous (no
/// `-dbcrashratio`), started by a `FlushDue` V answers inside a command, and V's waits take no
/// bell ([`NoBell`]), so V does not hear W's end. W's write fails after the `preciousblock`'s
/// admission, at its ABC's first release point ([`behind`]; BL-678 refuses a command admitted
/// after it): it stands for a W failure while V is inside the hold, before the disconnect's site.
/// The pool is empty: the site's decision comes before the block's transactions are added.
// Core: validation.cpp:3315-3317, :3627-3633 (v24.0.0)
#[test]
fn a_w_failure_latched_before_the_disconnect_gets_the_second_abort() {
    install_accept_log();
    let s = Stores::new();
    let pool = DrainLog::new(&s);
    let seams = Seams {
        parker: true,
        ..Seams::default()
    };
    let mut rig = Rig::with(RigConfig {
        mempool: Some(Box::new(pool.clone())),
        ..s.config_with(seams)
    });
    let (_, side) = two_branches(&rig, 2);
    pool.take();
    s.fs.gate.arm();
    let _gate = Released(&s.fs.gate);
    let hub = s.hub.clone();
    let started = on_v_async(&rig, move |v| {
        hub.post_flush_due();
        v.flush_now(FlushStateMode::None)
    });
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    assert_eq!(started.recv_timeout(PROMPT), Ok(true), "V does not wait for it");
    s.db.inner().crash_at_write(1);
    let (fs, latch) = (s.fs.clone(), s.latch.clone());
    let reply = behind(
        &rig,
        |v| v.precious_block(side[1].hash()),
        move || {
            fs.gate.release();
            assert!(latch.wait_tripped(PROMPT), "W's write fails and W reports it");
        },
    );
    assert_eq!(reply.recv(), Err(WaitError::Disconnected));
    assert_stopped_on_flush(&s, rig.stop().fatal, true);
    let (txids, at_drain) = the_failed_steps_drain(&pool);
    assert_eq!((txids, at_drain), (vec![], 1));
}

/// BL-559 (f): `InvalidateBlock` whose disconnect flush fails reports once, the flush's: Core's
/// `InvalidateBlock` returns its `false` with no `AbortNode` of its own.
// Core: validation.cpp:3970-3977 at v24.0.0
#[test]
fn invalidate_with_a_failed_disconnect_flush_reports_once() {
    let s = Stores::new();
    let mut r = reorg_rig(&s);
    s.db.inner().crash_at_write(1);
    let _ = r.rig.v.invalidate_block(r.main[1].hash()).recv();
    let exit = r.rig.stop();
    let text = reported_store_failure(&s, "chainstate write");
    assert_eq!(s.sink.messages(), [text.as_str()]);
    assert_eq!(exit.fatal.map(|f| f.0), Some(text));
    let (txids, at_drain) = the_failed_steps_drain(&r.pool);
    assert_eq!((txids, at_drain), (vec![], 1), "the pool lacks main[1]'s transactions");
}

/// BL-559 (g): a ChainLock mark whose disconnect flush fails reports once, the flush's, then
/// asserts with `EnforceBlock`'s line: Core's `MarkConflictingBlock` returns its `false` with no
/// `AbortNode` of its own.
// Core: validation.cpp:4112-4116, :4075-4078 at v24.0.0
#[test]
fn a_mark_whose_disconnect_flush_fails_asserts_after_one_report() {
    install_accept_log();
    let s = Stores::new();
    let mut rig = importing_rig(&s, true);
    let (_, side) = two_branches(&rig, 2);
    intake(&rig);
    let v = rig.v_thread();
    done_importing(&rig);
    s.db.inner().crash_at_write(1);
    assert!(record(&rig, &lock(&side[1])));
    let failed = "Chainstate::EnforceBlock -- MarkConflictingBlock failed: ";
    // A query wakes V for its deferred enforcement; once V has stopped it is dropped.
    let logged = || {
        let _ = rig.v.query(|_| ()).recv();
        lines_on(v).iter().any(|l| l.starts_with(failed))
    };
    assert!(until(logged), "{:#?}", lines_on(v));
    let text = reported_store_failure(&s, "chainstate write");
    assert_eq!(s.sink.messages(), [text.as_str()]);
    assert!(lines_on(v).contains(&format!("{failed}{text}")), "{:#?}", lines_on(v));
    let fatal = rig.stop().fatal.expect("V stops");
    assert!(!fatal.is_abort_node(), "EnforceBlock's assert");
}

/// BL-559 (h), DEC-1036's one-report case: an asynchronous W job that fails while V is idle is
/// seen on W's bell, outside any disconnect, and no later disconnect reads it: one report, W's,
/// and W's text is the stop's. (A connect site's own failed freeze, which that site reads
/// first, is `enforce_best_chainlock_logs_the_reported_text_of_a_failed_flush`.)
// Core: validation.cpp:3127-3128, :2250-2254 at v24.0.0
#[test]
fn a_w_failure_seen_outside_a_disconnect_reports_once() {
    let s = Stores::new();
    let (mut rig, _) = node(&s, 2);
    s.fs.gate.arm();
    let _gate = Released(&s.fs.gate);
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    s.db.inner().crash_at_write(1);
    s.fs.gate.release();
    assert!(s.latch.wait_tripped(PROMPT), "W's write fails and W reports it");
    // No command is sent: one admitted after W's failure would be refused there (BL-678).
    let stopped = || rig.join.as_ref().is_some_and(|j| j.is_finished());
    assert!(until(stopped), "idle V stops on W's bell");
    assert_stopped_on_flush(&s, rig.stop().fatal, false);
}

/// BL-559 (i): W's failure first seen in the disconnect's blocked fanout drain, after its freeze
/// returned, gets the second report too, after the drain. The feeds' queue holds one event: the
/// reorg's first disconnect (main[1]) fills it, and the second's (main[0]) waits in V's in-hold
/// drain, the checkpoint. Only then does a `FlushDue` start W's job there, which fails at its
/// write. The pool keeps main[1]'s transactions, the step's earlier disconnect, but not
/// main[0]'s.
// Core: validation.cpp:3315-3319, :3627-3633 (v24.0.0)
#[test]
fn a_w_failure_in_the_disconnects_blocked_drain_gets_the_second_abort() {
    install_accept_log();
    let s = Stores::new();
    let pool = DrainLog::new(&s);
    let seams = Seams {
        synchronous: true,
        ..Seams::default()
    };
    let mut rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        mempool: Some(Box::new(pool.clone())),
        ..s.config_with(seams)
    });
    let (main, side) = two_branches(&rig, 2);
    let feeds = rig.v.query(|q| q.chain.feeds()).recv().expect("V");
    let id = feeds.register(Needs::default()).expect("registers");
    let at = FeedSource::<()>::snapshot(&feeds).tip();
    let Ok(Handoff::Live {
        mut feed,
        ..
    }) = feeds.subscribe_at(id, &at, &mut || Ok::<(), ()>(()))
    else {
        panic!("subscribe_at")
    };
    let wake = Parker::new();
    feed.set_waker(Notify::Thread(wake.clone()));
    let drained = Arc::new(AtomicBool::new(false));
    let hook = drained.clone();
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            if stage == Stage::Drain {
                hook.store(true, Ordering::SeqCst);
            }
        }));
        Ok(())
    });
    pool.take();
    let reply = rig.v.precious_block(side[1].hash());
    assert!(until(|| drained.load(Ordering::SeqCst)), "main[0]'s step parks in the drain");
    assert!(next_event(&mut *feed, &wake), "main[1]'s step fills the queue");
    s.fs.gate.arm();
    let _gate = Released(&s.fs.gate);
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "the drain answers FlushDue with a job");
    s.db.inner().crash_at_write(1);
    s.fs.gate.release();
    assert_eq!(reply.recv_timeout(PROMPT), Some(Err(WaitError::Disconnected)));
    let exit = rig.stop();
    feeds.deregister(id);
    assert_stopped_on_flush(&s, exit.fatal, true);
    let (txids, at_drain) = the_failed_steps_drain(&pool);
    assert_eq!(txids, [main[1].block.txs()[0].txid()], "main[1]'s disconnect, not main[0]'s");
    assert_eq!(at_drain, 1, "the drain follows W's report and precedes ABC's");
}

/// W's panic text, as its thread's end reports it.
const PANICKED: &str = "the chainstate flush thread panicked";

/// The latch after a W panic: at [`W_PANIC_SITE`] with [`PANICKED`], an assert.
fn assert_w_panic_latched(s: &Stores) {
    let f = s.latch.tripped().expect("tripped").clone();
    assert_eq!((f.site, f.message.as_str()), (W_PANIC_SITE, PANICKED));
    assert_eq!(kept(&f), Kept::Assert);
}

/// An asynchronous W job held at the gate, started by a `FlushDue` V answers inside a command:
/// V does not wait for it. [`w_panics_at_the_gate`] ends it.
fn w_job_at_the_gate(s: &Stores, rig: &Rig) {
    s.fs.gate.arm();
    let hub = s.hub.clone();
    let started = on_v_async(rig, move |v| {
        hub.post_flush_due();
        v.flush_now(FlushStateMode::None)
    });
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    assert_eq!(started.recv_timeout(PROMPT), Ok(true), "V does not wait for it");
}

/// Releases [`w_job_at_the_gate`]'s job to panic at W's space check: W's end latches and reports
/// it. V's waits take no bell ([`NoBell`]), so V does not hear W's end.
fn w_panics_at_the_gate(fs: &TestFs, latch: &GlobalFailStop) {
    fs.panic_w.store(true, Ordering::SeqCst);
    fs.gate.release();
    assert!(latch.wait_tripped(PROMPT), "W panics and reports it");
}

/// BL-586 (D227's last case): W's panic first read at a disconnect's flush site, as BL-559 (e)'s
/// W failure, is one of Core's asserts: W's report is the only one, with no "Failed to disconnect
/// block" after it. V re-raises W's panic as it drops W's driver; the step's pool drain still ran,
/// after W's report. As in BL-559 (e), W panics after the `preciousblock`'s admission, at its
/// ABC's first release point ([`behind`]).
// Core: util/check.h:49-51; validation.cpp:3315-3317 (v24.0.0)
#[test]
fn a_w_panic_first_read_at_a_disconnects_site_reports_once() {
    install_accept_log();
    let s = Stores::new();
    let pool = DrainLog::new(&s);
    let seams = Seams {
        parker: true,
        ..Seams::default()
    };
    let mut rig = Rig::with(RigConfig {
        mempool: Some(Box::new(pool.clone())),
        ..s.config_with(seams)
    });
    let (_, side) = two_branches(&rig, 2);
    pool.take();
    let _gate = Released(&s.fs.gate);
    w_job_at_the_gate(&s, &rig);
    let (fs, latch) = (s.fs.clone(), s.latch.clone());
    let reply = behind(
        &rig,
        |v| v.precious_block(side[1].hash()),
        move || {
            w_panics_at_the_gate(&fs, &latch);
        },
    );
    assert_eq!(reply.recv(), Err(WaitError::Disconnected));
    assert_eq!(rig.join_panicked(), "W's flush thread panicked");
    assert_eq!(s.sink.reports(), [(PANICKED.to_owned(), None)], "W's report only");
    assert_w_panic_latched(&s);
    assert_eq!(the_failed_steps_drain(&pool), (vec![], 1));
}

/// BL-586: W's panic first read in the disconnect's blocked fanout drain, as BL-559 (i)'s W
/// failure: W's one report, no "Failed to disconnect block"; the pool keeps main[1]'s coinbase,
/// the step's earlier disconnect, not main[0]'s.
// Core: util/check.h:49-51; validation.cpp:3315-3319 (v24.0.0)
#[test]
fn a_w_panic_in_the_disconnects_blocked_drain_reports_once() {
    install_accept_log();
    let s = Stores::new();
    let pool = DrainLog::new(&s);
    let seams = Seams {
        synchronous: true,
        ..Seams::default()
    };
    let mut rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        mempool: Some(Box::new(pool.clone())),
        ..s.config_with(seams)
    });
    let (main, side) = two_branches(&rig, 2);
    let feeds = rig.v.query(|q| q.chain.feeds()).recv().expect("V");
    let id = feeds.register(Needs::default()).expect("registers");
    let at = FeedSource::<()>::snapshot(&feeds).tip();
    let Ok(Handoff::Live {
        mut feed,
        ..
    }) = feeds.subscribe_at(id, &at, &mut || Ok::<(), ()>(()))
    else {
        panic!("subscribe_at")
    };
    let wake = Parker::new();
    feed.set_waker(Notify::Thread(wake.clone()));
    let drained = Arc::new(AtomicBool::new(false));
    let hook = drained.clone();
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            if stage == Stage::Drain {
                hook.store(true, Ordering::SeqCst);
            }
        }));
        Ok(())
    });
    pool.take();
    let reply = rig.v.precious_block(side[1].hash());
    assert!(until(|| drained.load(Ordering::SeqCst)), "main[0]'s step parks in the drain");
    assert!(next_event(&mut *feed, &wake), "main[1]'s step fills the queue");
    s.fs.gate.arm();
    let _gate = Released(&s.fs.gate);
    s.hub.post_flush_due();
    assert!(s.fs.gate.wait_held(), "the drain answers FlushDue with a job");
    s.fs.panic_w.store(true, Ordering::SeqCst);
    s.fs.gate.release();
    assert_eq!(reply.recv_timeout(PROMPT), Some(Err(WaitError::Disconnected)));
    assert_eq!(rig.join_panicked(), "W's flush thread panicked");
    feeds.deregister(id);
    assert_eq!(s.sink.reports(), [(PANICKED.to_owned(), None)], "W's report only");
    assert_w_panic_latched(&s);
    let (txids, at_drain) = the_failed_steps_drain(&pool);
    assert_eq!(txids, [main[1].block.txs()[0].txid()], "main[1]'s disconnect, not main[0]'s");
    assert_eq!(at_drain, 1, "the drain follows W's report");
}

/// BL-586 (P2(b)): W's panic first read inside `ProcessNewBlock`'s ABC tail, past the block's
/// connect: W's one report, and no `ProcessNewBlock` failure line, as Core's assert never returns
/// to it. A W job waits at the gate while V accepts and connects the block; V releases it at its
/// hold's release (the connect's step is done), and W panics before V's next flush-side call. V
/// takes no bell ([`NoBell`]), so only that call reads the latch.
// Core: util/check.h:49-51; validation.cpp:4910-4913 at v24.0.0
#[test]
fn a_w_panic_read_at_a_connects_site_reports_once() {
    install_accept_log();
    let s = Stores::new();
    let seams = Seams {
        parker: true,
        ..Seams::default()
    };
    let (mut rig, chain) = node_with(&s, 2, seams);
    let next = branch(&rig.p, chain.last().expect("a tip"), 1, 0).remove(0);
    let v = rig.v_thread();
    s.fs.gate.arm();
    let _gate = Released(&s.fs.gate);
    let hub = s.hub.clone();
    let started = on_v_async(&rig, move |v| {
        hub.post_flush_due();
        v.flush_now(FlushStateMode::None)
    });
    assert!(s.fs.gate.wait_held(), "V's answer to FlushDue starts a job");
    assert_eq!(started.recv_timeout(PROMPT), Ok(true), "V does not wait for it");
    let (fs, latch) = (s.fs.clone(), s.latch.clone());
    let fired = Arc::new(AtomicBool::new(false));
    let hook = fired.clone();
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            if stage == Stage::Release && !hook.swap(true, Ordering::SeqCst) {
                fs.panic_w.store(true, Ordering::SeqCst);
                fs.gate.release();
                assert!(latch.wait_tripped(PROMPT), "W panics and reports it");
            }
        }));
        Ok(())
    });
    let reply = rig.v.process_block(next.block.clone(), true, None, true).recv();
    assert_eq!(reply.map(|_| ()), Err(WaitError::Disconnected));
    assert_eq!(rig.join_panicked(), "W's flush thread panicked");
    assert!(fired.load(Ordering::SeqCst), "the connect's step released its hold");
    assert_eq!(s.sink.reports(), [(PANICKED.to_owned(), None)], "W's report only");
    assert_w_panic_latched(&s);
    let pnb = lines_on(v).into_iter().filter(|l| l.starts_with("ProcessNewBlock:")).count();
    assert_eq!(pnb, 0, "no ProcessNewBlock line");
}

/// BL-586 (D62a): one of V's ports of Core's asserts latches at [`V_PANIC_SITE`] as V's stop
/// trips it, and reports once, V's; V returns (it does not panic) with the assert, and a shutdown
/// flush on its exit has no flush side to run.
#[test]
fn an_assert_on_v_latches_at_the_v_panic_site_and_reports_once() {
    let s = Stores::new();
    let (mut rig, _) = node(&s, 1);
    rig.v.send(ChainCommand::TestUnderV(Box::new(|_| Err(Fatal::assert("x"))))).expect("V runs");
    let mut exit = rig.stop();
    let fatal = exit.fatal.clone().expect("the assert");
    assert_eq!((fatal.0.as_str(), fatal.is_abort_node()), ("x", false));
    assert_eq!(s.sink.reports(), [("x".to_owned(), None)], "V's one report");
    let f = s.latch.tripped().expect("tripped").clone();
    assert_eq!((f.site, f.message.as_str()), (V_PANIC_SITE, "the chain actor panicked"));
    assert_eq!(kept(&f), Kept::Assert);
    assert_eq!(exit.force_flush(ShutdownFlush::First), Ok(()));
    assert_eq!(s.sink.reports().len(), 1);
}

/// BL-586 (Sol r1 B-1, D216), BL-678: W's panic, reported once genesis is stored and before
/// startup's ABC, is that ABC's refusal at its admission. It stays an assert, reported, so init's
/// reply is dropped instead of answered `Err` (Core's assert never returns to `ThreadImport`):
/// W's report is the only one. W did not really panic here, so V stops on the assert without
/// re-raising. K1, the panic latched after the admission and first read at the ABC's connect of
/// genesis, is [`a_w_panic_first_read_at_startups_connect_drops_its_reply`].
// Core: util/check.h:49-51; node/blockstorage.cpp:952-955 at v24.0.0
#[test]
fn a_w_panic_before_startups_abc_drops_its_reply() {
    let s = Stores::new();
    let (latch, sink) = (s.latch.clone(), s.sink.clone());
    let seams = Seams {
        parker: true,
        ..Seams::default()
    };
    let mut rig = Rig::with(RigConfig {
        aborts_before_init: true,
        before_init: Some(Box::new(move |_: &crate::chain::ChainHandle| {
            let failure = Failure {
                kind: ErrorKind::Other,
                site: W_PANIC_SITE,
                message: PANICKED.into(),
            };
            fail_on_w(&latch, sink, failure, PANICKED.to_owned()).join().expect("W reports");
        })),
        ..s.config_with(seams)
    });
    assert_eq!(rig.init, Err(WaitError::Disconnected), "init's reply dropped");
    let fatal = rig.stop().fatal.expect("V stopped on the latch");
    assert_eq!(s.sink.reports(), [(PANICKED.to_owned(), None)], "W's report only");
    assert_eq!(fatal.0, PANICKED);
    assert!(!fatal.is_abort_node(), "an assert");
    assert!(fatal.is_reported(), "reported by W");
}

/// `send`'s command to V, with a query queued behind it that runs `on_v` on V (V paused while both
/// queue): when the command's ABC starts at once, the query is served at that ABC's first release
/// point, after the command's admission and before the ABC's step. `send`'s reply.
fn behind<R>(
    rig: &Rig,
    send: impl FnOnce(&crate::chain::ChainHandle) -> R,
    on_v: impl FnOnce() + Send + 'static,
) -> R {
    let resume = pause(&rig.v);
    let r = send(&rig.v);
    drop(rig.v.query(move |_| on_v()));
    resume.send(()).expect("V paused");
    r
}

/// W's failure at `site` as a test stages it, and W's report text: a store failure at W's write
/// (an `AbortNode`), or W's panic (an assert).
fn w_failure(site: &'static str) -> (Failure, String) {
    if site == W_PANIC_SITE {
        let failure = Failure {
            kind: ErrorKind::Other,
            site,
            message: W_PANICKED.into(),
        };
        (failure, W_PANICKED.to_owned())
    } else {
        let failure = Failure {
            kind: ErrorKind::Io,
            site,
            message: STORE_FAILED.into(),
        };
        (failure, format!("System error while flushing: {STORE_FAILED}"))
    }
}

/// BL-678's idle rig on `s`: two blocks connected, V taking no bell ([`NoBell`]), so idle V hears
/// nothing of W, and a block-check recorder.
fn idle_rig(s: &Stores) -> (Rig, Vec<Mined>, Arc<Checked>) {
    install_accept_log();
    let seams = Seams {
        parker: true,
        ..Seams::default()
    };
    let (rig, chain) = node_with(s, 2, seams);
    let checked = Arc::new(Checked::default());
    rig.signals.register(checked.clone());
    rig.sync();
    (rig, chain, checked)
}

/// BL-678, path 1's run on [`idle_rig`]: W's failure at `site` latches with its report held, the
/// latch's later trips at `losing` lose, then `send` gives idle V a command. V must wait at the
/// admission for W's report with nothing of the command run (no line on V, `untouched` true, no
/// reply); then the report goes out. The command's reply, V's stop `Fatal` and W's text.
fn refused_when_idle<T: Send + 'static>(
    s: &Stores,
    rig: &mut Rig,
    site: &'static str,
    losing: &[&'static str],
    send: impl FnOnce(&crate::chain::ChainHandle) -> crate::chain::ActorReply<T>,
    untouched: impl Fn(&Rig) -> bool,
) -> (Result<T, WaitError>, Fatal, String) {
    let v = rig.v_thread();
    let before = lines_on(v).len();
    let (failure, text) = w_failure(site);
    let release = s.held.arm();
    let w = fail_on_w(&s.latch, s.held.clone(), failure, text.clone());
    for &site in losing {
        assert!(!s.latch.trip(w_failure(site).0), "a losing trip");
    }
    let reply = send(&rig.v);
    let parked = s.held.parked(&s.latch, v);
    let early = (lines_on(v).len() == before, untouched(rig), reply.try_recv().is_none());
    release.release();
    w.join().expect("W reports");
    assert!(parked.is_some(), "V waits at the admission for W's report");
    assert_eq!(early, (true, true, true), "nothing of the command ran before W's report");
    let reply = reply.recv();
    let fatal = rig.stop().fatal.expect("V stopped on W's failure");
    (reply, fatal, text)
}

/// BL-678, path 1: idle V admits no command once W's failure has latched. W's failure latches with
/// its report held, then a new block's `ProcessBlock` arrives: V waits at the admission for W's
/// report (the latch's waiting read, as V's flush sites make it), and nothing of the block runs
/// before it: no line, no header, no reply. V then stops on W's failure as its own, with W's one
/// report: the reply disconnects, the block stays unknown, and no `BlockChecked` follows. A store
/// failure is an `AbortNode` (Core's has started shutdown as W's write returns); W's panic stays
/// the assert it is. The latch's later trips at `losing` lose.
// Core: shutdown.cpp:27-38
fn idle_v_admits_no_block_after(site: &'static str, losing: &[&'static str]) {
    let s = Stores::new();
    let (mut rig, chain, checked) = idle_rig(&s);
    let b = branch(&rig.p, &chain[1], 1, 0).remove(0);
    let hash = b.hash();
    let unknown = move |rig: &Rig| rig.v.reader().lookup(&hash).is_none();
    let (reply, fatal, text) = refused_when_idle(
        &s,
        &mut rig,
        site,
        losing,
        |v| v.process_block(b.block.clone(), true, None, true),
        unknown,
    );
    assert_eq!(reply.map(|_| ()), Err(WaitError::Disconnected), "{site}");
    assert_eq!(fatal.0, text);
    assert_eq!(fatal.is_abort_node(), site != W_PANIC_SITE, "{fatal:?}");
    assert!(fatal.is_reported(), "{fatal:?}");
    assert_eq!(s.sink.reports(), [(text, None)], "W's report only");
    assert!(unknown(&rig), "{site}: no header");
    assert!(checked.0.lock().expect("checked").is_empty(), "{site}: no BlockChecked");
}

#[test]
fn idle_v_admits_no_block_after_ws_store_failure() {
    idle_v_admits_no_block_after(WRITE_SITE, &[]);
}

#[test]
fn idle_v_admits_no_block_after_ws_panic() {
    idle_v_admits_no_block_after(W_PANIC_SITE, &[]);
}

/// BL-678, path 1, the first failure's class decides: W's store failure latches first and W's
/// panic then loses. The refusal is the store failure, an `AbortNode`, with W's one report.
#[test]
fn a_losing_assert_after_ws_store_failure_leaves_the_refusal_an_abort_node() {
    idle_v_admits_no_block_after(WRITE_SITE, &[W_PANIC_SITE]);
}

/// BL-678, path 1: a block V already has is refused too. The active tip's `ProcessBlock` after
/// W's failure gets no `ProcessNewBlock` line and no reply: it disconnects, with W's one report.
// Core: validation.cpp:4911-4916 at v24.0.0
#[test]
fn idle_v_admits_no_known_block_after_ws_failure() {
    let s = Stores::new();
    let (mut rig, chain, checked) = idle_rig(&s);
    let v = rig.v_thread();
    let before = lines_on(v).len();
    let tip = chain[1].block.clone();
    let (reply, fatal, text) = refused_when_idle(
        &s,
        &mut rig,
        WRITE_SITE,
        &[],
        |v| v.process_block(tip, true, None, true),
        |_| true,
    );
    assert_eq!(reply.map(|_| ()), Err(WaitError::Disconnected));
    assert_eq!(fatal.0, text);
    let lines = lines_on(v).split_off(before);
    assert!(!lines.iter().any(|l| l.starts_with("ProcessNewBlock")), "{lines:#?}");
    assert_eq!(s.sink.reports(), [(text, None)], "W's report only");
    assert!(checked.0.lock().expect("checked").is_empty(), "no BlockChecked");
}

/// BL-678, D240 (DEC-1373): W's store failure, latched before startup's ABC with its report
/// held, is that ABC's refusal at its admission. V waits there for W's report, then stops, and
/// the reaper answers init's ABC `Ok`: it took no step, as Core's ABC with shutdown requested
/// returns `true` (fresh, genesis is due and stays unconnected here), so the startup thread
/// raises no second abort. One report, W's.
// Core: v24.0.0 validation.cpp:3789-3791, :3823; node/blockstorage.cpp:952-953;
// shutdown.cpp:27-38
#[test]
fn a_store_failure_before_startups_abc_answers_it_ok() {
    let s = Stores::new();
    let (failure, text) = w_failure(WRITE_SITE);
    let release = s.held.arm();
    let (latch, held, report) = (s.latch.clone(), s.held.clone(), text.clone());
    let (to_checker, at_checker) = mpsc::channel();
    let cfg = RigConfig {
        stops_at_init: true,
        before_init: Some(Box::new(move |v: &crate::chain::ChainHandle| {
            let id = v.query(|_| std::thread::current().id()).recv().expect("V runs");
            to_checker.send(id).expect("the checker waits");
            drop(fail_on_w(&latch, held, failure, report));
        })),
        ..s.config_with(Seams {
            parker: true,
            ..Seams::default()
        })
    };
    let st = &s;
    let (mut rig, parked) = std::thread::scope(|sc| {
        let checker = sc.spawn(move || {
            let v = at_checker.recv().expect("before_init runs");
            let parked = st.held.parked(&st.latch, v);
            release.release();
            parked
        });
        let rig = Rig::with(cfg);
        (rig, checker.join().expect("the checker"))
    });
    assert!(parked.is_some(), "V waits at the ABC's admission for W's report");
    assert_eq!(rig.init, Ok(Ok(())), "init's ABC answered Ok (D240)");
    let exit = rig.stop();
    let fatal = exit.fatal.clone().expect("V stopped on W's failure");
    assert_eq!(fatal.0, text);
    assert!(fatal.is_abort_node(), "{fatal:?}");
    assert_eq!(s.sink.reports(), [(text, None)], "W's report only");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), None, "no step: genesis unconnected");
}

/// BL-586 (Sol r1 B-1, D216)'s K1, restaged for BL-678: W's panic latches after startup's ABC was
/// admitted, at that ABC's first release point ([`behind`]'s query, queued once the ABC is), and
/// is first read at its connect of genesis. It stays an assert, reported, so ABC drops init's
/// reply instead of answering it `Err` (Core's assert never returns to `ThreadImport`): W's
/// report is the only one. W did not really panic here, so V stops on the assert without
/// re-raising.
#[test]
fn a_w_panic_first_read_at_startups_connect_drops_its_reply() {
    let s = Stores::new();
    let (latch, sink) = (s.latch.clone(), s.sink.clone());
    let (to_test, queued) = mpsc::channel();
    let mut rig = Rig::with(RigConfig {
        aborts_before_init: true,
        before_init: Some(Box::new(move |v: &crate::chain::ChainHandle| {
            let (resume, paused) = mpsc::channel::<()>();
            let (at, on_v) = mpsc::channel();
            let pause = ChainCommand::TestUnderV(Box::new(move |_| {
                at.send(()).expect("the helper waits");
                paused.recv().expect("the helper resumes V");
                Ok(())
            }));
            v.send(pause).expect("V runs");
            let v = v.clone();
            let helper = std::thread::spawn(move || {
                on_v.recv().expect("V pauses");
                assert!(until(|| !v.bounded_lane().is_empty()), "init's ABC is queued");
                let (failure, report) = w_failure(W_PANIC_SITE);
                drop(v.query(move |_| {
                    fail_on_w(&latch, sink, failure, report).join().expect("W reports");
                }));
                resume.send(()).expect("V paused");
            });
            to_test.send(helper).expect("the test joins it");
        })),
        ..s.config_with(Seams {
            parker: true,
            ..Seams::default()
        })
    });
    queued.recv().expect("spawned").join().expect("the helper");
    assert_eq!(rig.init, Err(WaitError::Disconnected), "init's reply dropped");
    let fatal = rig.stop().fatal.expect("V stopped on the latch");
    assert_eq!(s.sink.reports(), [(PANICKED.to_owned(), None)], "W's report only");
    assert_eq!(fatal.0, PANICKED);
    assert!(!fatal.is_abort_node(), "an assert");
    assert!(fatal.is_reported(), "reported by W");
}

/// BL-678 (D220, D240): a `SetLoading` V refuses at its admission is still the reaper's. Startup
/// raises `fImporting` before its ABC, and W's failure at `site` has latched first, while V
/// idles, its report held: V waits at the `SetLoading`'s admission for W's report, the flag still
/// unset, then stops, and the reaper sets the flag from the refused command before it takes
/// init's ABC from the lane, which it answers `init`: `Ok` after a store failure (D240, its ABC
/// took no step), dropped after W's panic (an assert). V takes no bell ([`NoBell`]), so idle V
/// hears nothing of W. `start` starts V on `cfg`. It returns V's exit.
// Core: v24.0.0 validation.cpp:3789-3791, :3823; node/blockstorage.cpp:886-890, :952-953
fn a_refused_set_loading_still_sets_the_flag(
    s: &Stores,
    site: &'static str,
    init: Result<Result<(), String>, WaitError>,
    start: impl FnOnce(RigConfig) -> Rig,
) -> crate::chain::VExit {
    let (failure, text) = w_failure(site);
    let release = s.held.arm();
    let (latch, held, report) = (s.latch.clone(), s.held.clone(), text.clone());
    let (to_checker, at_checker) = mpsc::channel();
    let cfg = RigConfig {
        aborts_before_init: init != Ok(Ok(())),
        stops_at_init: init == Ok(Ok(())),
        before_init: Some(Box::new(move |v: &crate::chain::ChainHandle| {
            drop(fail_on_w(&latch, held, failure, report));
            let importing = ChainCommand::SetLoading {
                importing: Some(true),
                reindex: None,
            };
            v.send(importing).expect("V runs");
            to_checker.send(v.clone()).expect("the checker waits");
        })),
        ..s.config_with(Seams {
            parker: true,
            ..Seams::default()
        })
    };
    let (mut rig, (parked, early)) = std::thread::scope(|sc| {
        let checker = sc.spawn(move || {
            let v = at_checker.recv().expect("before_init runs");
            let parked = s.held.wait_entered().and_then(|_| {
                until(|| s.latch.pending_waiters().len() == 1).then(|| s.latch.pending_waiters()[0])
            });
            let early = v.loading().importing();
            assert!(until(|| !v.bounded_lane().is_empty()), "init's ABC is queued");
            release.release();
            (parked, early)
        });
        let rig = start(cfg);
        (rig, checker.join().expect("the checker"))
    });
    assert_eq!(
        parked,
        Some(rig.v_thread()),
        "{site}: V waits at the SetLoading's admission for W's report"
    );
    assert!(!early, "{site}: the flag is unset while V waits");
    assert!(rig.v.loading().importing(), "{site}: the reaper set the flag");
    assert_eq!(rig.init, init, "{site}");
    let exit = rig.stop();
    let fatal = exit.fatal.clone().expect("V stopped on W's failure");
    assert_eq!(fatal.0, text);
    assert_eq!(s.sink.reports(), [(text, None)], "{site}: W's report only");
    exit
}

#[test]
fn a_refused_set_loading_after_ws_store_failure_still_sets_the_flag() {
    let s = Stores::new();
    a_refused_set_loading_still_sets_the_flag(&s, WRITE_SITE, Ok(Ok(())), Rig::with);
}

#[test]
fn a_refused_set_loading_after_ws_panic_still_sets_the_flag() {
    let s = Stores::new();
    let dropped = Err(WaitError::Disconnected);
    a_refused_set_loading_still_sets_the_flag(&s, W_PANIC_SITE, dropped, Rig::with);
}

/// BL-678, D240 (DEC-1373): [`a_refused_set_loading_still_sets_the_flag`] on a restart, V's tip
/// loaded from the stores: the startup a node with a best tip makes, whose init ABC has no step
/// to take. Core's ABC has no work here and returns `true`, so `ThreadImport` raises nothing and
/// Core has W's one report; the reaper answers the queued ABC `Ok` too, so the startup thread
/// raises no second abort.
// Core: v24.0.0 validation.cpp:3789-3791, :3823; node/blockstorage.cpp:952-953
#[test]
fn a_restart_with_nothing_to_connect_answers_its_queued_abc_ok() {
    let s = Stores::new();
    let (mut first, chain) = node(&s, 2);
    first.on_v(|v| v.flush_now(FlushStateMode::Always));
    let tip = first.id(&chain[1]);
    let exit = first.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let files = Box::new(s.block_files());
    let restart = move |cfg| Rig::restart_with(exit, files, tip, cfg).expect("V restarts");
    let exit = a_refused_set_loading_still_sets_the_flag(&s, WRITE_SITE, Ok(Ok(())), restart);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(chain[1].hash()));
}

/// BL-678, D240 (DEC-1373): [`a_refused_set_loading_still_sets_the_flag`] on a restart whose
/// loaded tip is `chain[0]`, with `chain[1]` in the set: work is due (Core's ABC would take a
/// step). The refused `SetLoading`'s ABC is answered `Ok` all the same, as it took no step:
/// `chain[1]` stays unconnected and unchecked. The coins stay at the first run's flushed tip,
/// ahead of the loaded one; the commands are refused before any coin access, so that is unread.
// Core: v24.0.0 validation.cpp:3642-3683, :3789-3791, :3823; node/blockstorage.cpp:952-953
#[test]
fn a_restart_with_a_block_to_connect_answers_its_refused_abc_ok() {
    let s = Stores::new();
    let (mut first, chain) = node(&s, 2);
    first.on_v(|v| v.flush_now(FlushStateMode::Always));
    let (below, due) = (first.id(&chain[0]), first.id(&chain[1]));
    let exit = first.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let files = Box::new(s.block_files());
    let checked = Arc::new(Checked::default());
    let listener = checked.clone();
    let restart = move |cfg| {
        let cfg = RigConfig {
            listener: Some(listener),
            ..cfg
        };
        Rig::restart_with(exit, files, below, cfg).expect("V restarts")
    };
    let exit = a_refused_set_loading_still_sets_the_flag(&s, WRITE_SITE, Ok(Ok(())), restart);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(chain[0].hash()), "no step");
    assert!(exit.idx.is_candidate(due), "chain[1] was due");
    let seen = checked.0.lock().expect("checked").clone();
    assert!(!seen.iter().any(|(h, _)| *h == chain[1].hash()), "chain[1] unchecked: {seen:?}");
}

/// D240's post-progress control (DEC-1373 (1)): an admitted startup ABC that meets W's failure
/// after its first step still answers `Err`, so the startup thread's second abort follows, as
/// Core's ABC returns `false` at the failing flush (D216). A restart at `chain[0]` (coins and
/// their database there, `chain[1]` and `chain[2]` indexed with their bodies and reconsidered)
/// has init's ABC connect `chain[1]` and stop at the better position, so its outer loop goes
/// round. The release hook latches W's store failure once, its report held, and queues a probe
/// on V's lane; at the second iteration's release point the probe's admission waits for W's
/// report, then refuses. That boundary is not the first, so the answer is W's text, and
/// `chain[1]` stays connected.
// Core: v24.0.0 validation.cpp:3676-3679, :3779-3852, :3795-3797; node/blockstorage.cpp:952-953
#[cfg(feature = "test-support")]
#[test]
fn an_abc_meeting_ws_failure_after_its_first_step_answers_err() {
    let s = Stores::new();
    let (mut first, chain) = node(&s, 3);
    let reconsidered = first.id(&chain[1]);
    assert_eq!(first.v.invalidate_block(chain[1].hash()).recv(), Ok(Ok(())));
    first.on_v(|v| v.flush_now(FlushStateMode::Always));
    first.on_v(move |v| v.reconsider_block(reconsidered, false));
    assert_eq!(durable_tip(&s), Some(chain[0].hash()), "the coins' database at chain[0]");
    let below = first.id(&chain[0]);
    let exit = first.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(chain[0].hash()));
    let files = Box::new(s.block_files());
    let (failure, text) = w_failure(WRITE_SITE);
    let release = s.held.arm();
    let (to_checker, at_checker) = mpsc::channel();
    let once = Mutex::new(Some((failure, text.clone(), to_checker)));
    let (latch, held) = (s.latch.clone(), s.held.clone());
    let cfg = RigConfig {
        aborts_before_init: true,
        before_init: Some(Box::new(move |v: &crate::chain::ChainHandle| {
            let probe = v.clone();
            v.set_between_stores(Some(Arc::new(move || {
                let Some((failure, report, to_checker)) = once.lock().expect("once").take() else {
                    return;
                };
                drop(fail_on_w(&latch, held.clone(), failure, report));
                drop(probe.query(|_| ()));
                to_checker.send(std::thread::current().id()).expect("the checker waits");
            })));
        })),
        ..s.config_with(Seams {
            parker: true,
            ..Seams::default()
        })
    };
    let st = &s;
    let (mut rig, parked) = std::thread::scope(|sc| {
        let checker = sc.spawn(move || {
            let Ok(v) = at_checker.recv_timeout(Duration::from_secs(120)) else {
                release.release();
                panic!("the release hook never ran");
            };
            let parked = st.held.parked(&st.latch, v);
            release.release();
            parked
        });
        let rig = Rig::restart_with(exit, files, below, cfg).expect("V restarts");
        (rig, checker.join().expect("the checker"))
    });
    assert!(parked.is_some(), "V waits at the second release point for W's report");
    assert_eq!(rig.init, Ok(Err(text.clone())), "after a step, init's ABC answers Err");
    let exit = rig.stop();
    let fatal = exit.fatal.clone().expect("V stopped on W's failure");
    assert_eq!(fatal.0, text);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(chain[1].hash()), "kept");
    assert_eq!(s.sink.reports(), [(text, None)], "W's report only");
}

/// D240's work-due staging on `s`'s stores (BL-792): genesis's children a1 and its tie b1, a1
/// connected, b1 stored, and the coins flushed at a1, so W is settled. One paused operation
/// invalidates a1, so b1 is due, then waits while the test queues its commands, so no release
/// point of the invalidation takes them, then runs `then` with b1. It returns the rig, b1 and
/// the sender that resumes V. The default clock keeps the rig in IBD: no ChainLock task, and no
/// accept waits.
fn with_b1_due(
    s: &Stores,
    then: impl FnOnce(&mut V, BlockRef) -> Result<(), Fatal> + Send + 'static,
) -> (Rig, BlockRef, mpsc::Sender<()>) {
    let rig = Rig::with(s.config_with(Seams::default()));
    let genesis = rig.genesis.clone();
    let (a1, b1) = (child(&rig.p, &genesis, 0), child(&rig.p, &genesis, 1));
    rig.accept(&a1);
    rig.accept(&b1);
    rig.on_v(|v| v.flush_now(FlushStateMode::Always));
    let (a1_id, b1_ref) = (rig.id(&a1), block_ref(&rig, &b1));
    rig.rec.take();
    let (staged, at_staged) = mpsc::channel();
    let (go, queued) = mpsc::channel::<()>();
    let stage = ChainCommand::TestUnderV(Box::new(move |v| {
        assert!(v.invalidate_block(a1_id, &mut false)?, "a1 is disconnected");
        assert_eq!(v.deferred_len(), 0, "nothing deferred");
        staged.send(()).expect("the test waits");
        queued.recv_timeout(SOON).expect("the test queues its commands");
        then(v, b1_ref)
    }));
    rig.v.send(stage).expect("V runs");
    at_staged.recv_timeout(SOON).expect("V staged");
    (rig, b1_ref, go)
}

/// V's exit after [`with_b1_due`]'s ABC took no step: the tip still genesis, b1 due and
/// unchecked.
fn stopped_without_a_step(rig: Rig, b1: BlockRef) -> crate::chain::actor::VExit {
    let genesis = rig.genesis.hash();
    let b1_checked = checked(&rig).iter().any(|(h, _)| *h == b1.hash);
    let exit = stop_within_prompt(rig);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(genesis), "no step");
    assert!(exit.idx.is_candidate(b1.id), "b1 was due");
    assert!(!b1_checked, "no work resumed");
    exit
}

/// BL-792, CH3b D240's route (i) at the node's flush site: an admitted `ActivateBestChain` whose
/// first release point answers a `FlushDue` (a NONE freeze with `DMAX`) at a pair §7.1's check
/// refuses meets V's own `AbortNode` at the bell, an `Own` failure before any search or step. It
/// answers `Ok` after the node's flag, with V's one report. The staging admits and defers the
/// ABC at a release point while the pair holds, then moves the coins' best block to b1 and posts
/// the flag: V takes the deferred ABC before its idle select, so its own release point rings. b1
/// is due and stays unconnected and unchecked. Core has no counterpart to the pair check.
#[test]
fn an_abc_meeting_a_flush_due_pair_failure_at_its_first_release_point_answers_ok() {
    let s = Stores::new();
    let (coins, hub) = (s.coins.clone(), s.hub.clone());
    let (rig, b1, go) = with_b1_due(&s, move |v, b1| {
        v.release_point()?;
        assert_eq!(v.deferred_len(), 1, "the ABC is admitted and deferred");
        coins.lock().expect("coins").set_best_block(Some(b1));
        hub.post_flush_due();
        Ok(())
    });
    let abc = at_close(rig.v.activate_best_chain(), &rig.shutdown, rig.v.loading(), &s.sink);
    go.send(()).expect("V waits");
    assert_eq!(within(SOON, abc), (Ok(Ok(())), Some((true, false, 1))), "Ok after the node's flag");
    let reports = s.sink.reports();
    assert!(
        matches!(&reports[..], [(log, None)] if log.starts_with("a freeze at ")),
        "V's one report: {reports:?}"
    );
    let fatal = stopped_without_a_step(rig, b1).fatal.expect("V stopped on its own failure");
    assert_eq!(fatal.0, reports[0].0);
    assert!(fatal.is_own() && fatal.is_abort_node() && !fatal.is_contract(), "{fatal:?}");
}

/// BL-792, CH3b D240's route (ii) (DEC-1433): a block-file write fault on a `ProcessBlock` V runs
/// at an admitted `ActivateBestChain`'s first release point is the files' `AbortNode`, which
/// trips the common latch; the block is rejected, so nothing is carried. The admission of the
/// query queued behind it reads the latch, an `Own` failure before any search or step, so the
/// ABC answers `Ok`. The commands stand for other Core threads (D213 (b)): there
/// `SaveBlockToDisk` aborts the node and `AcceptBlock` rejects the block. With b1 due, Core's
/// ABC would still take its step (AD-CH3-3); the end state is the node's fail-stop all the same
/// (DEC-1433 (1)): the flag set before the ABC's reply closes, the files' report the one abort,
/// V stopped on the `Own` failure and serving nothing after it, the latch tripped, the tip
/// unmoved, and the chainstate on disk as it was at the ABC, with nothing written since. Mutant
/// M5 (the admission mapped without `Fatal::own`) fails it at the ABC's reply with
/// `Ok(Err(..))`, which only the release-point route gives; an ABC refused at its own admission
/// would fail at `is_own` instead.
// Core: node/blockstorage.cpp:751-757, :870-873; validation.cpp:4848-4851, :3784-3797 at v24.0.0
#[test]
fn an_abc_meeting_a_block_save_failure_at_its_first_release_point_answers_ok() {
    let s = Stores::new();
    let fs = s.fs.clone();
    let (rig, b1, go) = with_b1_due(&s, move |_, _| {
        fs.fail_write.store(true, Ordering::SeqCst);
        Ok(())
    });
    let a1 = child(&rig.p, &rig.genesis, 0);
    let on_disk = (Meta::read(&*s.db).expect("meta").tip, durable_tip(&s));
    assert_eq!(on_disk, (Some(a1.hash()), Some(a1.hash())), "flushed at a1");
    let epoch = s.db.write_epoch();
    let c1 = child(&rig.p, &rig.genesis, 2);
    let abc = at_close(rig.v.activate_best_chain(), &rig.shutdown, rig.v.loading(), &s.sink);
    let block = rig.v.process_block(c1.block.clone(), true, None, true);
    let query = rig.v.query(|_| ());
    go.send(()).expect("V waits");
    assert_eq!(within(SOON, abc), (Ok(Ok(())), Some((true, false, 1))), "Ok after the node's flag");
    let r = block.recv_timeout(SOON).expect("V answers").expect("the block's reply");
    let position = "AcceptBlock: Failed to find position to write new block to disk";
    assert_eq!(r.state, BlockState::Error(position.into()));
    assert_eq!(query.recv_timeout(SOON), Some(Err(WaitError::Disconnected)), "refused");
    let report = "Failed to write block";
    assert_eq!(s.sink.reports(), [(report.to_owned(), None)], "the files' one report");
    let later = rig.v.query(|_| ());
    assert_eq!(later.recv_timeout(SOON), Some(Err(WaitError::Disconnected)), "V serves no more");
    let fatal = stopped_without_a_step(rig, b1).fatal.expect("V stopped on the files' failure");
    assert_eq!(fatal.0, report);
    assert!(fatal.is_own() && fatal.is_abort_node(), "{fatal:?}");
    assert_eq!(s.latch.tripped().map(|f| f.site), Some(BLOCK_FILES_WRITE_SITE), "still latched");
    assert_eq!((Meta::read(&*s.db).expect("meta").tip, durable_tip(&s)), on_disk, "unchanged");
    assert_eq!(s.db.write_epoch(), epoch, "nothing written after the fault");
}

/// BL-678: the node flush's admission is the latch's waiting read. With nothing latched it
/// answers at once; with W's failure latched and its report held, its caller waits for the report
/// (among the latch's pending waiters), then gets W's failure as V's flush sites would.
#[test]
fn the_node_flush_admission_waits_for_ws_report() {
    let s = Stores::new();
    let seams = Seams {
        parker: true,
        ..Seams::default()
    };
    let flush = s.config_with(seams).flush.expect("the node's flush");
    assert_eq!(flush.admit().map_err(|f| f.0), Ok(()), "nothing latched");
    let (failure, text) = w_failure(WRITE_SITE);
    let release = s.held.arm();
    let w = fail_on_w(&s.latch, s.held.clone(), failure, text.clone());
    let (to_test, admitted) = mpsc::channel();
    let (parked, early, flush) = std::thread::scope(|sc| {
        let probe = sc.spawn(move || {
            _ = to_test.send(flush.admit().map_err(|f| (f.0.clone(), f.is_abort_node())));
            flush
        });
        let parked = s.held.parked(&s.latch, probe.thread().id());
        let early = admitted.try_recv().is_ok();
        release.release();
        (parked, early, probe.join().expect("the probe"))
    });
    w.join().expect("W reports");
    assert!(parked.is_some(), "the admission waits for W's report");
    assert!(!early, "the admission answered before W's report");
    assert_eq!(admitted.recv_timeout(PROMPT), Ok(Err((text.clone(), true))));
    assert_eq!(flush.admit().map_err(|f| f.0), Err(text.clone()), "and again, at once");
    assert_eq!(s.sink.reports(), [(text, None)]);
}

/// A pruned node with ChainLocks on, and two header-only children of its tip, `h` and its
/// sibling `s`, stored clean by an ALWAYS flush: the rig, its tip, the observer, `h` and `s`.
fn header_only_siblings(
    st: &Stores,
    target: u64,
    on: &Arc<AtomicBool>,
) -> (Rig, Mined, Arc<RecordingObserver>, Mined, Mined) {
    let (rig, chain, obs) = pruned_node_with(st, 400, target, on, true);
    let tip = chain.last().expect("a tip").clone();
    let h = branch(&rig.p, &tip, 1, 7).remove(0);
    let s = branch(&rig.p, &tip, 1, 8).remove(0);
    rig.headers(std::slice::from_ref(&h));
    rig.headers(std::slice::from_ref(&s));
    assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
    let stored = durable_status(st, &h).expect("h stored");
    assert!(!stored.has(Status::CONFLICT_CHAINLOCK) && !stored.has(Status::HAVE_DATA));
    (rig, tip, obs, h, s)
}

/// A ChainLock on `s` marks its header-only sibling `h` with no disconnect (the sibling walk).
// Core: src/validation.cpp:4237-4246 at the pin
fn lock_marks_the_sibling(rig: &Rig, tip: &Mined, h: &Mined, s: &Mined) {
    intake(rig);
    assert!(record(rig, &lock(s)));
    settle(rig);
    assert_eq!(super::failure::tip(rig), rig.id(tip), "nothing disconnected");
    assert!(rig.status(h).has(Status::CONFLICT_CHAINLOCK), "h marked");
}

/// `m`'s record as V's index holds it now.
#[cfg(feature = "test-support")]
fn memory_record(rig: &Rig, m: &Mined) -> BlockIndexRecord {
    let id = rig.id(m);
    let (tx, rx) = mpsc::channel();
    rig.on_v(move |v| {
        let ix = v.chain.node_index().expect("the node's");
        let _ = tx.send(ix.record(id).bytes);
        Ok(())
    });
    BlockIndexRecord::decode(&rx.recv().expect("V ran it")).expect("a record")
}

/// A manual prune at the tip, which selects file 0, run to its release.
fn prune_file_0(rig: &Rig, obs: &RecordingObserver, height: u32) {
    rig.on_v(move |v| v.flush_at_site(FlushStateMode::None, Some(height)));
    let picked = selected(&obs.marks());
    assert!(picked.last().is_some_and(|f| f.contains(&0)), "file 0 selected: {picked:?}");
}

/// CH3b D242 (BL-813, B-2): Core's `nFile` for an entry with no block data is 0, so a prune that
/// selects file 0 rewrites every header-only entry; one marked since its last write (the mark
/// dirtied nothing, D15) is stored with the mark, and a kill keeps it.
// Core: src/node/blockstorage.cpp:236-262 (PruneOneBlockFile) at the pin
#[test]
fn a_header_only_mark_is_stored_by_a_file_0_prune() {
    let st = Stores::pruned();
    let off = Arc::new(AtomicBool::new(false));
    let (mut rig, tip, obs, h, s) = header_only_siblings(&st, 0, &off);
    lock_marks_the_sibling(&rig, &tip, &h, &s);
    prune_file_0(&rig, &obs, tip.height);
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let stored = durable_status(&st, &h).expect("h stored");
    assert!(stored.has(Status::CONFLICT_CHAINLOCK), "the clear stored the mark");
}

/// CH3b D242 (BL-813, Q4): a prune's clear writes an entry without dirtying it, as Core's
/// `PruneOneBlockFile` does once its write has cleaned it: a later mark, which dirties nothing,
/// is not stored by the next ALWAYS, and a kill drops it, as Core's disk does.
// Core: src/node/blockstorage.cpp:236-262 at the pin
#[test]
fn a_mark_after_a_prune_on_a_rewritten_entry_is_dropped_as_core_drops_it() {
    let st = Stores::pruned();
    let off = Arc::new(AtomicBool::new(false));
    let (mut rig, tip, obs, h, s) = header_only_siblings(&st, 0, &off);
    prune_file_0(&rig, &obs, tip.height);
    lock_marks_the_sibling(&rig, &tip, &h, &s);
    assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let stored = durable_status(&st, &h).expect("h stored");
    assert!(!stored.has(Status::CONFLICT_CHAINLOCK), "the mark was never dirty");
}

/// CH3b D242/D244 (BL-813, BL-819): an automatic prune that selects file 0 is waited for inside
/// its call, so the ops the base's window let in (a ChainLock on s that marks the header-only g,
/// g stored clean, and a header n on s) are sent while V is in the call, held at W's space check,
/// and run after it. The prune's clears find g unmarked and no n, as modeled Core's write does.
/// Killed then: g is stored unmarked and n absent, as modeled Core's disk. After an ALWAYS instead,
/// n is stored with its in-memory record, and g is still unmarked: the mark dirtied nothing.
#[cfg(feature = "test-support")]
#[test]
fn an_automatic_file_0_prune_writes_the_records_of_its_call() {
    for kill in [true, false] {
        let st = Stores::pruned();
        let on = Arc::new(AtomicBool::new(false));
        let (mut rig, tip, obs, g, s) = header_only_siblings(&st, 1, &on);
        let n = branch(&rig.p, &s, 1, 9).remove(0);
        intake(&rig);
        st.fs.gate.arm();
        let released = ReleaseOnDrop(&st.fs.gate);
        on.store(true, Ordering::SeqCst);
        let done = on_v_async(&rig, |v| v.flush_at_site(FlushStateMode::None, None));
        assert!(st.fs.gate.wait_held(), "the prune's job is at W");
        let sig = lock(&s);
        let marked = rig.v.record_chainlock(sig.clone(), sig_hash(&sig), None);
        let headers = rig.v.process_headers(super::support::batch(std::slice::from_ref(&n)), None);
        st.fs.gate.release();
        drop(released);
        assert_eq!(done.recv_timeout(PROMPT), Ok(true), "the prune's call returns");
        assert!(marked.recv().expect("V replies").relay);
        assert!(headers.recv().expect("V replies").stop.is_none());
        settle(&rig);
        assert_eq!(super::failure::tip(&rig), rig.id(&tip), "nothing disconnected");
        assert!(rig.status(&g).has(Status::CONFLICT_CHAINLOCK), "g marked in memory");
        let marks = obs.marks();
        assert!(automatic_decisions(&marks).iter().any(|f| f.contains(&0)), "file 0 decided");
        assert!(selected(&marks).last().is_some_and(|f| f.contains(&0)), "step 7 cleared file 0");
        let label = if kill {
            "killed"
        } else {
            "ALWAYS"
        };
        if !kill {
            assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
        }
        let mem = memory_record(&rig, &n);
        let exit = rig.stop();
        assert!(exit.fatal.is_none(), "{label}: {:?}", exit.fatal);
        drop(exit);
        let stored_g = durable_status(&st, &g).expect("g stored");
        assert!(!stored_g.has(Status::CONFLICT_CHAINLOCK), "{label}: g unmarked, as Core's");
        let want_n = (!kill).then_some(mem);
        assert_eq!(durable_record(&st, &n), want_n, "{label}: n as Core's");
    }
}

/// FF2's boundary fixture: a pruned node mined so that file 0's last block is B and the tip T is
/// exactly B + keep, with `on` unset. Returns the rig, its chain, its marks, B and keep.
#[cfg(feature = "test-support")]
fn boundary_node(
    st: &Stores,
    on: &Arc<AtomicBool>,
    chainlocks: bool,
) -> (Rig, Vec<Mined>, Arc<RecordingObserver>, u32, u32) {
    let (rig, mut chain, obs) = pruned_node_with(st, 100, 1, on, chainlocks);
    let keep = super::support::regtest_keep();
    let b = st.files.read(|f| f.file_info(0).map(|i| i.height_last)).expect("file 0");
    let last = chain.last().expect("a tip").clone();
    let more = padded_branch(&rig.p, &last, b + keep - last.height);
    for m in &more {
        rig.accept(m);
    }
    chain.extend(more);
    let tip = chain.last().expect("a tip");
    let b_now = st.files.read(|f| f.file_info(0).map(|i| i.height_last)).expect("file 0");
    assert_eq!((b_now, tip.height), (b, b + keep), "file 0 ends at the tip's bound");
    (rig, chain, obs, b, keep)
}

/// BL-819 (CH3b D244, DEC-1604 Q1 condition (a)): an automatic prune decided at the at-bound
/// `DisconnectTip` site unlinks Core's selection and nothing more. On FF2's fixture (file 0 ends
/// at B, the tip T = B + keep, the check armed by the mining with the host off, a target no file
/// meets), Core's choice at T is file 0: by rule (`blockstorage.cpp:201` at v24.0.0, the bound at
/// the tip before `SetTip` is T − keep = B) and by the store's selector over the same infos, which
/// at T − 1 selects nothing. The host is turned on right before a one-block `invalidateblock`, so
/// the disconnect site at T is the first decision that sees the check.
#[cfg(feature = "test-support")]
#[test]
fn a_prune_at_a_disconnect_site_unlinks_cores_selection() {
    let st = Stores::pruned();
    let on = Arc::new(AtomicBool::new(false));
    let (mut rig, chain, obs, b, keep) = boundary_node(&st, &on, false);
    let tip = chain.last().expect("a tip").clone();
    let parent = chain[chain.len() - 2].clone();
    let infos = st.files.read(|f| f.infos().to_vec());
    assert!(infos[1].height_last > b, "file 1 is not eligible at B");
    let plan = |tip: u32| {
        st.files.read(|f| f.plan_files_to_prune(1, 0, keep, Some(tip), i32::MAX, false).files)
    };
    assert_eq!(plan(tip.height), [0], "Core's selector at T");
    assert!(plan(tip.height - 1).is_empty(), "a selection at T - 1 would differ");
    assert!(st.files.read(|f| f.check_for_pruning()), "the mining armed the check");
    st.fs.removed.lock().expect("removed").clear();
    on.store(true, Ordering::SeqCst);
    assert_eq!(rig.v.invalidate_block(tip.hash()).recv(), Ok(Ok(())), "invalidateblock");
    rig.v.settle().recv().expect("V settles");
    let marks = obs.marks();
    let decided: Vec<_> = marks
        .iter()
        .filter_map(|m| match m {
            ObsMark::PruneDecision {
                manual: None,
                files,
                tip,
                ..
            } => Some((files.clone(), *tip)),
            _ => None,
        })
        .collect();
    assert_eq!(decided, [(vec![0], tip.hash())], "decided at T");
    let (flush, files, bound) = marks
        .iter()
        .find_map(|m| match m {
            ObsMark::PruneSelected {
                flush,
                files,
                bound,
                ..
            } => Some((*flush, files.clone(), *bound)),
            _ => None,
        })
        .expect("a selection");
    assert_eq!((files, bound), (vec![0], b), "Core's choice, at the bound B");
    let froze = marks.iter().find_map(|m| match m {
        ObsMark::FlushBegin {
            flush: f,
            new,
            ..
        } if *f == flush => Some(*new),
        _ => None,
    });
    assert_eq!(froze, Some(parent.hash()), "frozen at T - 1, after the step");
    assert!(marks.iter().all(|m| !matches!(m, ObsMark::PruneDeclined { .. })));
    let mut removed = st.fs.removed.lock().expect("removed").clone();
    removed.sort();
    assert_eq!(removed, ["blk00000.dat", "rev00000.dat"], "file 0 alone");
    assert!(st.blocks_dir().join("blk00001.dat").exists(), "file 1 kept");
    let in_file_1 = chain.iter().find(|m| m.height > b && m.height <= infos[1].height_last);
    let in_file_1 = in_file_1.expect("a block in file 1");
    assert!(rig.status(in_file_1).has(Status::HAVE_DATA), "file 1's entries keep their data");
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
}

/// BL-819 (CH3b D244, DEC-1604 Q1 condition (c), `getdata`'s predicate): a block pruned inside
/// its call reads as pruned. An automatic prune at a NONE site with W held at its unlink, so V is
/// inside the call, the clears durable and the files still on disk. The live entry read
/// (`ChainViewHandle::entry`, which `NodeChain::read`'s re-read answers `Pruned` from) shows no
/// data, `nTx` kept and no data position, there and after the release; a view taken before the
/// call still shows HAVE_DATA there, and one taken after V republishes (at the next block's
/// connect) shows it cleared. After the release the block's old position no longer reads.
#[cfg(feature = "test-support")]
#[test]
fn a_block_pruned_inside_its_call_reads_as_pruned() {
    let st = Stores::pruned();
    let on = Arc::new(AtomicBool::new(false));
    let (rig, chain, obs) = pruned_node(&st, 400, 1, &on);
    let handle = rig.v.view_handle();
    let m = &chain[1];
    let id = rig.id(m);
    let before = handle.entry(id).expect("indexed");
    assert!(before.status.has(Status::HAVE_DATA) && before.file == Some(0), "{before:?}");
    let at = FlatFilePos {
        file: 0,
        pos: before.data_pos,
    };
    let view_before = handle.view();
    st.fs.unlink.arm();
    let held = ReleaseOnDrop(&st.fs.unlink);
    on.store(true, Ordering::SeqCst);
    let done = on_v_async(&rig, |v| v.flush_at_site(FlushStateMode::None, None));
    assert!(st.fs.unlink.wait_held(), "W at the prune's unlink, inside V's call");
    assert!(selected(&obs.marks()).last().is_some_and(|f| f.contains(&0)), "file 0 selected");
    let pruned = |e: &dash_chain::EntryView| {
        !e.status.has(Status::HAVE_DATA) && e.n_tx > 0 && e.data_pos == 0
    };
    let live = handle.entry(id).expect("indexed");
    assert!(pruned(&live), "the live read, inside the call: {live:?}");
    let seen = handle.status(&view_before, id).expect("the view before the call is kept");
    assert!(seen.expect("indexed").has(Status::HAVE_DATA), "the view before the call");
    st.fs.unlink.release();
    drop(held);
    assert_eq!(done.recv_timeout(PROMPT), Ok(true), "the prune's call returns");
    rig.v.settle().recv().expect("V settles");
    let live = handle.entry(id).expect("indexed");
    assert!(pruned(&live), "the live read, after the release: {live:?}");
    let next = padded_branch(&rig.p, chain.last().expect("a tip"), 1).remove(0);
    rig.accept(&next);
    let view_after = handle.view();
    let seen = handle.status(&view_after, id).expect("a fresh view").expect("indexed");
    assert!(!seen.has(Status::HAVE_DATA), "a view after V republishes");
    assert!(st.block_files().read_block(at, &m.hash()).is_err(), "the old position is gone");
}

/// BL-819 (CH3b D244, DEC-1604 Q1 condition (c), V's own read): after an automatic prune of the
/// low files, an `invalidateblock` of a block inside file 0 disconnects down to the highest
/// pruned block and fails there as Core's `DisconnectTip` does on an entry with no data, before
/// any undo read: "DisconnectTip(): Failed to read block" (`validation.cpp:3274` at v24.0.0).
/// The reply is the base's for a failed disconnect, V raises no `Fatal` and no abort is
/// reported, and the tip stays at the pruned block.
#[cfg(feature = "test-support")]
#[test]
fn a_rewind_into_pruned_undo_fails_as_cores() {
    install_accept_log();
    let st = Stores::pruned();
    let on = Arc::new(AtomicBool::new(false));
    let (mut rig, chain, obs) = pruned_node(&st, 400, 1, &on);
    let heights = heights(&st);
    on.store(true, Ordering::SeqCst);
    let done = on_v_async(&rig, |v| v.flush_at_site(FlushStateMode::None, None));
    assert_eq!(done.recv_timeout(PROMPT), Ok(true), "the prune's call returns");
    let picked = selected(&obs.marks()).pop().expect("a selection");
    assert!(picked.contains(&0), "{picked:?}");
    let pruned_to = last_height(&heights, &picked);
    let top = chain.iter().find(|m| m.height == pruned_to).expect("the highest pruned block");
    assert!(!rig.status(top).has(Status::HAVE_DATA), "pruned");
    let here = rig.v_thread();
    let before = lines_on(here).len();
    assert_eq!(rig.v.invalidate_block(chain[5].hash()).recv(), Ok(Ok(())), "the base's reply");
    rig.v.settle().recv().expect("V settles");
    let lines = lines_on(here)[before..].to_vec();
    let failed = lines.iter().filter(|l| l.starts_with("DisconnectTip(): Failed to read block"));
    assert_eq!(failed.count(), 1, "{lines:#?}");
    assert!(!lines.iter().any(|l| l.contains("UndoReadFromDisk")), "no undo read: {lines:#?}");
    assert_eq!(super::failure::tip(&rig), rig.id(top), "the tip stays at the pruned block");
    rig.assert_no_abort();
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
}

/// The automatic decisions in `marks`: each one's files.
#[cfg(feature = "test-support")]
fn automatic_decisions(marks: &[ObsMark]) -> Vec<Vec<u32>> {
    marks
        .iter()
        .filter_map(|m| match m {
            ObsMark::PruneDecision {
                manual: None,
                files,
                ..
            } => Some(files.clone()),
            _ => None,
        })
        .collect()
}

/// BL-819 FF1, the row's (d') (CH3b D244): `h` is header-only, stored clean, then marked by a
/// ChainLock on its sibling (the mark dirties nothing). An automatic prune selecting file 0 is
/// decided at a NONE site while W is held at its space check; `reconsiderblock` with
/// `ignore_chainlocks` on `h`, which clears the mark and dirties it, and `h`'s body follow. Core
/// wrote `h` marked with its prune (`PruneOneBlockFile(0)`, then `WriteBlockIndexDB`, in the one
/// call), so after a KILL before the next take its disk holds `h` marked with no data.
#[cfg(feature = "test-support")]
#[test]
fn a_reset_and_receipt_after_a_file_0_prunes_call_leave_cores_marked_record() {
    let st = Stores::pruned();
    let on = Arc::new(AtomicBool::new(false));
    let (mut rig, tip, obs, h, s) = header_only_siblings(&st, 1, &on);
    lock_marks_the_sibling(&rig, &tip, &h, &s);
    st.fs.gate.arm();
    let _released = ReleaseOnDrop(&st.fs.gate);
    on.store(true, Ordering::SeqCst);
    let done = on_v_async(&rig, |v| v.flush_at_site(FlushStateMode::None, None));
    assert!(st.fs.gate.wait_held(), "the prune's job is at W");
    let reset = rig.v.reconsider_block(h.hash(), true);
    let receipt = rig.v.process_block(h.block.clone(), true, None, true);
    let early_reset = reset.recv_timeout(PROBE);
    let early_receipt = receipt.recv_timeout(PROBE);
    st.fs.gate.release();
    assert_eq!(done.recv_timeout(PROMPT), Ok(true), "the prune's call returns");
    assert_eq!(early_reset.unwrap_or_else(|| reset.recv()), Ok(Ok(())), "reconsiderblock");
    let receipt = early_receipt.unwrap_or_else(|| receipt.recv());
    assert!(receipt.expect("V replies").accepted, "h's body is stored");
    rig.v.settle().recv().expect("V settles");
    let marks = obs.marks();
    assert!(automatic_decisions(&marks).iter().any(|f| f.contains(&0)), "file 0 decided");
    assert!(selected(&marks).iter().any(|f| f.contains(&0)), "file 0 cleared");
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let stored = durable_status(&st, &h).expect("h stored");
    assert!(stored.has(Status::CONFLICT_CHAINLOCK), "Core's h: marked");
    assert!(!stored.has(Status::HAVE_DATA), "Core's h: no data");
}

/// BL-819 FF2, the row's (f), on a boundary fixture (CH3b D244, DEC-1604 Q1): file 0's last block
/// is B and the tip is B + keep, so an automatic prune at the tip selects file 0 alone, which a
/// tip one lower no longer allows. The header-only `g` is marked (no dirt). The prune is decided
/// at a NONE site while W is held at its space check, and a one-block `invalidateblock` of the tip
/// follows. Core pruned at its call: after a KILL its disk holds `g` marked, `pruned_files` set
/// and file 0's info reset. The node must not decline.
#[cfg(feature = "test-support")]
#[test]
fn a_rewind_after_an_automatic_prunes_call_finds_it_cleared() {
    let st = Stores::pruned();
    let on = Arc::new(AtomicBool::new(false));
    let (mut rig, chain, obs, b, keep) = boundary_node(&st, &on, true);
    let tip = chain.last().expect("a tip").clone();
    let g = branch(&rig.p, &tip, 1, 7).remove(0);
    let s = branch(&rig.p, &tip, 1, 8).remove(0);
    rig.headers(std::slice::from_ref(&g));
    rig.headers(std::slice::from_ref(&s));
    assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
    lock_marks_the_sibling(&rig, &tip, &g, &s);
    let rule = crate::flush::PruneRule::Automatic {
        after: 0,
        keep,
    };
    assert!(rule.allows(b, tip.height) && !rule.allows(b, tip.height - 1), "the boundary");
    st.fs.gate.arm();
    let _released = ReleaseOnDrop(&st.fs.gate);
    on.store(true, Ordering::SeqCst);
    let done = on_v_async(&rig, |v| v.flush_at_site(FlushStateMode::None, None));
    assert!(st.fs.gate.wait_held(), "the prune's job is at W");
    assert_eq!(automatic_decisions(&obs.marks()), [vec![0]], "D = {{0}}");
    let rewind = rig.v.invalidate_block(tip.hash());
    let early = rewind.recv_timeout(PROBE);
    st.fs.gate.release();
    assert_eq!(done.recv_timeout(PROMPT), Ok(true), "the prune's call returns");
    assert_eq!(early.unwrap_or_else(|| rewind.recv()), Ok(Ok(())), "invalidateblock");
    rig.v.settle().recv().expect("V settles");
    let marks = obs.marks();
    let declines: Vec<_> =
        marks.iter().filter(|m| matches!(m, ObsMark::PruneDeclined { .. })).collect();
    assert!(declines.is_empty(), "no PruneDeclined: {declines:?}");
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let stored = durable_status(&st, &g).expect("g stored");
    assert!(stored.has(Status::CONFLICT_CHAINLOCK), "Core's g: marked");
    assert!(Meta::read(&*st.db).expect("meta").pruned_files, "pruned_files");
    let info = st.files.read(|f| f.file_info(0).copied()).expect("file 0's info");
    assert_eq!(info, dashd_store::flatfile::BlockFileInfo::default(), "file 0's info reset");
}

#[cfg(feature = "test-support")]
mod liveness;
