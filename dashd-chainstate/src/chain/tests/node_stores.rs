//! CH2b b4: V over the node's own stores. [`NodeBlockFiles`] (M2) is Core's `SaveBlockToDisk`,
//! `ReadBlockFromDisk` and `LoadBlockIndexDB`'s file check on ST2's files; [`NodeFlush`] (M1) is
//! V's flush over CH3a's coins and W's thread. The rig wires them as the node does: one
//! [`GuardedFiles`] for V's blocks, the connector's undo and W's flush, and one latch under
//! every store.

use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use tracing::Level;

use dash_chain::{
    BlockIndexRecord, ChainError, FeedSource, FlatFilePos, Handoff, HeaderChain, Needs, Next,
    Notify, Parker, Status, SubscriberFeed,
};
use dash_consensus::Block;
use dash_consensus::coins::stats::CoinStatsHashType;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{Header, Tx};
use dash_kv::ErrorKind;
use dash_num::CompactTarget;
use dash_params::ChainParams;
use dash_primitives::{BlockHash, BlockHeader, Transaction};
use dash_types::codec::BaseCodec as _;
use dashd_store::flatfile::{
    BlockFiles as StoreFiles, BlockFilesConfig, BlockFilesState, FlatFilePos as StorePos, FlatFs,
    FlatRead, OpenFailed, OsFs,
};
use dashd_store::{KvEngine, KvRead as _, MemoryEngine, SchemaState, StoreError};

use super::failure::tip;
use super::locks::{intake, lock, record, settle};
use super::support::{
    Mined, REGTEST_BITS, Rig, RigConfig, SPACING, branch, cb_script, coinbase, install_accept_log,
    levels_here, lines_on, lines_with, mine, regtest,
};
use crate::chain::actor::V;
use crate::chain::actor::tests::Stage;
use crate::chain::flush::AT_PUMP;
use crate::chain::read_ahead::Probe;
use crate::chain::reply::WaitError;
use crate::chain::{
    AbortPath, BlockFiles, ChainCommand, Fatal, LoadFailure, LoadedIndex, LocalShutdown,
    ReadAheadSetup, RecordingAbort, ShutdownFlush, ValidationInterface, open_block_index,
};
use crate::checkqueue::CheckQueue;
use crate::coins::{CoinLayerOps as _, CoinsBudget, CoinsCache, Ratio, Salt, start_coins_stats};
use crate::connect::{
    ChainstateCell, ConnectorParts, CoreConnector, NoEvo as NoEvoHooks, NodeHelper,
};
use crate::flush::{
    CrashRoll, FilesPruneHost, FlushConfig, FlushInputs as _, FlushObserver, FlushPolicy,
    FlushStateMode, NoEvo, NoInputs, NoPrune, NodeFlush, NodeFlushParts, NullObserver, ObsMark,
    PruneDryRun, PruneHost, RecordingObserver, SubscriberHub, ThreadDriver, WThread,
};
use crate::recovery::{ChainstateOpen, FileRead};
use crate::sigcache::{CacheSizes, ScriptCaches};
use crate::store::{
    CHAINSTATE_SCHEMA, Failure, GlobalFailStop, GuardedEngine, GuardedFiles, Meta, NodeBlockFiles,
    ProbeEngine,
};
use crate::tuning::{EngineMemoryReport, EngineTuning, MemoryTuning};

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
/// How long a test waits for what must happen at once.
const PROMPT: Duration = Duration::from_secs(10);

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
        if !on_w() {
            return;
        }
        let mut state = self.state.lock().expect("gate");
        if state.0 {
            state.1 = true;
            self.cv.notify_all();
            // Bounded, so a test that fails with W held still ends: W then goes on.
            let _held = self.cv.wait_timeout_while(state, 6 * PROMPT, |s| s.0).expect("gate");
        }
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
        if self.fail_write.load(Ordering::SeqCst) {
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
        self.os.remove(path)
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
struct Stores {
    p: Arc<ChainParams>,
    dir: tempfile::TempDir,
    fs: Arc<TestFs>,
    latch: GlobalFailStop,
    /// The node's `AbortNode`, every store's and V's.
    sink: Arc<RecordingAbort>,
    db: Arc<GuardedEngine<ProbeEngine>>,
    coins: ChainstateCell<ProbeEngine>,
    files: Arc<GuardedFiles<TestFs>>,
    hub: Arc<SubscriberHub>,
    /// `-prune` with `-fastprune`'s 64 KiB blk files.
    prune: bool,
}

/// What a test sets in the flush's seams beside the stores.
struct Seams {
    crash: CrashRoll,
    on_crash: Box<dyn FnOnce() + Send>,
    prune: Box<dyn PruneHost + Send>,
    /// V's and W's marks.
    obs: Arc<dyn FlushObserver>,
    /// `-dbcrashratio` is set (D14).
    synchronous: bool,
    tuning: Arc<HookTuning>,
}

/// [`MemoryTuning`] whose post-Bk hook takes `delay`, then counts itself: a hook that runs after
/// the release is still running when V has answered.
#[derive(Default)]
struct HookTuning {
    delay: Duration,
    hooks: AtomicU32,
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
        self.hooks.fetch_add(1, Ordering::SeqCst);
        MemoryTuning.after_bk(db)
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
        }
    }
}

/// [`FilesPruneHost`] whose automatic check stays off, its flag kept, until `on` is set.
struct Switched {
    on: Arc<AtomicBool>,
    host: FilesPruneHost<TestFs>,
}

impl PruneHost for Switched {
    fn check_due(&mut self) -> bool {
        self.on.load(Ordering::SeqCst) && self.host.check_due()
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

impl Stores {
    fn new() -> Stores {
        Self::build(false)
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
        let db = Arc::new(GuardedEngine::new(
            ProbeEngine::new(&CHAINSTATE_SCHEMA),
            latch.clone(),
            sink.clone(),
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
            db,
            coins,
            files,
            hub: Arc::new(SubscriberHub::new()),
            prune,
        }
    }

    /// The prune side over these files: `-prune`'s `target` bytes (0 for manual only), from
    /// height 0, its automatic check off until `on`.
    fn prune_host(&self, target: u64, on: &Arc<AtomicBool>) -> Box<dyn PruneHost + Send> {
        Box::new(Switched {
            on: on.clone(),
            host: FilesPruneHost {
                files: self.files.clone(),
                target,
                prune_after_height: 0,
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
        let Seams {
            crash,
            on_crash,
            prune,
            obs,
            synchronous,
            tuning,
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
        let connector =
            CoreConnector::new(&self.p, parts, NoEvoHooks, Arc::new(())).expect("connector");
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
                live_copy: false,
            },
            covered_seq: 0,
            crash,
            on_crash,
        })
        .expect("W's thread");
        let flush = NodeFlush::new(NodeFlushParts {
            coins: self.coins.clone(),
            driver,
            policy: FlushPolicy::new(synchronous),
            ledger: Arc::default(),
            hub: self.hub.clone(),
            obs,
            evo: Box::new(NoEvo),
            prune,
            inputs: Box::new(NoInputs),
        });
        RigConfig {
            params: Some(self.p.clone()),
            files: Some(Box::new(self.block_files())),
            connector: Some(Box::new(connector)),
            flush: Some(Box::new(flush)),
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

fn pow(p: &ChainParams) -> PowParams {
    *HeaderParams::from_chain_params(p).expect("regtest").pow()
}

/// A block whose header fails regtest's proof of work: `ReadBlockFromDisk`'s
/// `CheckProofOfWork`.
fn unmined(p: &ChainParams, parent: &Mined) -> Arc<Block> {
    let txs = vec![coinbase(&cb_script(parent.height + 1, 9))];
    let mut raw = BlockHeader {
        version: 0x2000_0000,
        prev_hash: parent.hash(),
        time: parent.time() + 1,
        bits: REGTEST_BITS,
        ..BlockHeader::default()
    };
    raw.merkle_root = Block::new(Header::new(raw), txs.clone()).merkle().0;
    let pow = pow(p);
    let header = (0..)
        .map(|nonce| {
            Header::new(BlockHeader {
                nonce,
                ..raw
            })
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
/// `AbortNode` (`bs:599-601`); ST2 then stops at its latch (D62 difference (2)).
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

/// RP1a D11 (DEC-618 M-1, m-2): `gettxoutsetinfo`'s scan reads a copy point. V's ALWAYS flush
/// runs the engine's hook before its release (E-b1.8), so the hook has run when V answers; the
/// scan's snapshot is at that flush, V's tip with the cache's coins; and a block connected and
/// flushed during the walk does not show through it.
// Core: rpc/blockchain.cpp:1212-1220 (ForceFlushStateToDisk, then the scan of CoinsDB())
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
    let job = rig.v.coins_stats(hash_type, held).recv().expect("V answers").expect("a scan");
    assert_eq!(tuning.hooks.load(Ordering::SeqCst), hooks + 1, "the hook ran before the release");
    assert_eq!(durable_tip(&s), Some(tip.hash()));
    assert_eq!(job.snapshot_epoch(), Some(s.db.write_epoch()), "nothing written since");
    let scan_now = || {
        let job = start_coins_stats(s.db.clone(), hash_type, Arc::new(|| false)).expect("a thread");
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

/// The coins' durable tip, which a restart loads.
fn durable_tip(s: &Stores) -> Option<BlockHash> {
    s.coins.lock().expect("coins").durable().map(|b| b.hash)
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

/// CONS-11 case 2 on V's stores (DEC-525): `MarkConflictingBlock`'s marks set no dirty entry, so
/// a flush writes a mark only through an entry still dirty from its connect. With an ALWAYS flush
/// between the connects and the lock, as `l1_chain_locks` runs the case, the ALWAYS flush after
/// the lock stores the marked main blocks without `CONFLICT_CHAINLOCK`; without the first flush
/// it stores them with it. The locked block, connected since, is stored without it either way.
// Core: src/validation.cpp:4237-4246 (the marks), :3353-3420 (DisconnectTip) at the pin
#[test]
fn a_chainlock_mark_is_stored_only_through_its_connects_dirty_entry() {
    for flushed_first in [true, false] {
        let s = Stores::new();
        let mut rig = Rig::with(RigConfig {
            chainlocks: true,
            ..s.config_with(Seams::default())
        });
        let main = branch(&rig.p, &rig.genesis, 2, 0);
        let side = branch(&rig.p, &rig.genesis, 1, 1);
        for m in main.iter().chain(&side) {
            rig.accept(m);
        }
        if flushed_first {
            assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
        }
        intake(&rig);
        assert!(record(&rig, &lock(&side[0])));
        settle(&rig);
        assert_eq!(tip(&rig), rig.id(&side[0]));
        assert_eq!(rig.v.flush(FlushStateMode::Always).recv(), Ok(()));
        for m in &main {
            assert!(rig.status(m).has(Status::CONFLICT_CHAINLOCK), "{}: marked", m.height);
        }
        let exit = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert_eq!(durable_tip(&s), Some(side[0].hash()));
        for m in &main {
            let stored = durable_status(&s, m).expect("stored");
            let marked = stored.has(Status::CONFLICT_CHAINLOCK);
            assert_eq!(marked, !flushed_first, "flushed first {flushed_first}: {}", m.height);
        }
        let locked = durable_status(&s, &side[0]).expect("stored");
        assert!(!locked.has(Status::CONFLICT_CHAINLOCK) && locked.is_valid(Status::VALID_SCRIPTS));
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
        let want = format!(" cache={mib:.1}MiB({n}utxo) evodb_cache=0.0MiB");
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

/// Whether `cond` holds within [`PROMPT`], polled each millisecond.
fn until(cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + PROMPT;
    while !cond() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// Runs `f` on V without waiting: the receiver hears `f`'s outcome.
fn on_v_async(
    rig: &Rig,
    f: impl FnOnce(&mut V) -> Result<(), Fatal> + Send + 'static,
) -> mpsc::Receiver<bool> {
    let (tx, rx) = mpsc::channel();
    let cmd = ChainCommand::TestOnV(Box::new(move |v| {
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
    rig.v.send(ChainCommand::TestOnV(Box::new(|_| panic!("index corrupt")))).expect("V runs");
    assert!(until(|| s.latch.tripped().is_some()));
    let tripped = s.latch.tripped().map(|f| f.message.clone());
    drop(held);
    assert_eq!(tripped.as_deref(), Some("the chain actor panicked"));
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
// Core: init.cpp:379-385, :449-456; validation.cpp:3128
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
    assert_eq!(exit.force_flush(ShutdownFlush::Second), Ok(()));
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
#[test]
fn a_panic_on_w_while_v_waits_for_the_release_reaches_vs_joiner() {
    let s = Stores::new();
    let (mut rig, _) = node(&s, 2);
    let woke = always_flush_held(&s, &rig, || s.fs.panic_w.store(true, Ordering::SeqCst));
    assert_eq!(woke, Ok(false), "V woke on W's end");
    let panicked = "the chainstate flush thread panicked";
    assert_eq!(s.latch.tripped().map(|f| f.message.as_str()), Some(panicked));
    rig.v.shutdown();
    let join = rig.join.take().expect("running");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || _ = tx.send(join.join().expect("an unmarked joiner").err()));
    let payload = rx.recv_timeout(PROMPT).expect("V's thread ends").expect("V's thread panicked");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"W's flush thread panicked"));
    assert_eq!(s.sink.messages(), [panicked]);
}

/// Runs `rig.stop()` on a thread of its own: its exit within [`PROMPT`], so a stop that hangs
/// fails the test.
fn stop_within_prompt(rig: Rig) -> crate::chain::actor::VExit {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut rig = rig;
        let _ = tx.send(rig.stop());
    });
    rx.recv_timeout(PROMPT).expect("V's stop ends")
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
    let obs = Arc::new(RecordingObserver::default());
    let seams = Seams {
        prune: s.prune_host(target, on),
        obs: obs.clone(),
        ..Seams::default()
    };
    let rig = Rig::with(s.config_with(seams));
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
fn padded_branch(p: &ChainParams, parent: &Mined, n: u32) -> Vec<Mined> {
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

/// An automatic prune's flush on W, which V's `AbortNode` stopped while W was held at the
/// prune's clears.
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
        let mut picked = selected(&self.obs.marks());
        assert_eq!(picked.len(), 1);
        let picked = picked.remove(0);
        let last = picked.iter().map(|&n| self.heights[n as usize]).max();
        (picked, last.expect("a selected file"))
    }
}

/// [`Aborted`]: W held at the prune's clears, waiting for V's answer to its selection while V
/// is busy, when V aborts; then V's stop.
fn abort_at_a_prune_clear(s: &Stores) -> Aborted {
    let on = Arc::new(AtomicBool::new(false));
    let (rig, chain, obs) = pruned_node(s, 400, 1, &on);
    let tip = chain.last().expect("a tip").hash();
    let heights =
        s.files.read(|f| (0..).map_while(|n| f.file_info(n).map(|i| i.height_last)).collect());
    on.store(true, Ordering::SeqCst);
    let (db, marks) = (s.db.clone(), obs.clone());
    let done = on_v_async(&rig, move |v| {
        v.flush_now(FlushStateMode::IfNeeded)?;
        // B1 is durable, so W's next step is the selection, whose clears only V answers: W is
        // held there (parked, after this best-effort pause, or about to be) when V aborts.
        assert!(until(|| Meta::read(&*db).is_ok_and(|m| m.tip == Some(tip))));
        std::thread::sleep(Duration::from_millis(200));
        assert!(selected(&marks.marks()).is_empty(), "W waits for V's clears");
        Err(Fatal::new("a test's AbortNode"))
    });
    assert_eq!(done.recv_timeout(PROMPT), Ok(false));
    let exit = stop_within_prompt(rig);
    assert!(exit.fatal.is_some());
    Aborted {
        exit,
        chain,
        obs,
        heights,
    }
}

/// Releases a [`Gate`] when it drops, so a failed check does not leave W held there while the
/// test's `VExit` drops and joins W.
struct ReleaseOnDrop<'a>(&'a Gate);

impl Drop for ReleaseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Sol r1 N3: an `AbortNode` with W held at a prune clear: V's stop answers it inline, so the
/// prune's clears are made (its blocks without data), the latch stays clear, and ND1's shutdown
/// flush succeeds. The prune's unlink, step 12, comes after W's release, which is all V's stop
/// waits for; W lives on in the `VExit` (BL-271), so the files are gone once W has run the
/// shutdown flush's job, which it takes after the prune's (BL-519).
#[test]
fn an_abort_node_with_w_held_at_a_prune_clear_completes_the_prune() {
    let s = Stores::pruned();
    let mut a = abort_at_a_prune_clear(&s);
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

/// BL-519: a process that ends after V's `AbortNode` stop, with W at the prune's unlink, leaves
/// the prune durable: no stored record keeps data in a selected file, and `meta.pruned_files` is
/// set, so the restart's scan (Core's `ScanAndUnlinkAlreadyPrunedFiles`, which CH8 runs at the
/// node's start) removes the files the durable infos record as pruned. Then W goes on.
#[test]
fn a_process_ended_at_the_unlink_after_an_abort_node_leaves_the_prune_replayable() {
    let s = Stores::pruned();
    s.fs.unlink.arm();
    let a = abort_at_a_prune_clear(&s);
    let _w = ReleaseOnDrop(&s.fs.unlink);
    let (picked, last) = a.selection();
    assert!(s.fs.unlink.wait_held(), "W reaches a remove");
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
    let scanned = opened.files().mutate(|f| f.scan_and_unlink_already_pruned_files(have_pruned));
    scanned.expect("the scan");
    drop(opened);
    assert!(picked.iter().all(|&f| unlinked(&s, f)));
    s.fs.unlink.release();
    drop(a);
    assert!(s.latch.tripped().is_none());
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
    let expected = s.files.read(|f| f.plan_files_to_prune_manual(height, Some(height)).files);
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
