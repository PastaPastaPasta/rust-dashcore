//! `NodeSut`: the chainstate O2b's comparator drives, on the node's own startup and shutdown
//! steps ([`ChainSteps`]) over an in-memory coins database (ND1-D10, CH3 D24, O2b D32/D34): its
//! lifecycle, the restart reports and the images (CH3a a10); node-l1 drives the ops on V.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use dash_args::ArgsManager;
use dash_clock::{Clock, MockClock};
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::BlockHash;
use dash_kv::MemoryImage;
use dash_params::ChainSetup;
use dash_params::Network;
use dash_statedigest::{Chain, FileKind, Header, Lines, Record, Writer};
use dashd_chainstate::chain::{
    Blocking, ChainHandle, ChainLockSig, ChainQuery, ClSchedule, Fatal, Quiet, ValidationInterface,
};
use dashd_chainstate::coins::{OutPointKey, PackedCoin};
use dashd_chainstate::connect::{
    ChainHelper, DecisionTap, DigestLog, DigestSink, NodeHelper, RecordingHelper, TapedLog,
};
use dashd_chainstate::flush::{FlushRegime, FlushTally};
use dashd_chainstate::reorg::PoolsOnly;
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, CoinsReadError, GlobalFailStop, GuardedEngine, RecordingSink, Stored,
};
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::sporks::SporkManager;
#[cfg(test)]
use dashd_store::ErrorKind;
use dashd_store::StoreError;
use dashd_store::flatfile::OsFs;
use dashd_store::{
    Cf, Commit, KvEngine, KvRead, MemoryBatch, MemoryEngine, MemorySnapshot, Result as KvResult,
};

use crate::InitFailure;
use crate::abort::NodeAbort;
use crate::chainstate::{OpenError, Opening, reindex_question};
use crate::init::caches::{db_batch_size, init_validation_caches, start_script_check_threads};
use crate::init::{self, ParamInteraction, SelectedParams, setup_server_args};
use crate::node::coins_configuration;
use crate::shutdown::ShutdownState;
use crate::steps::{ChainSteps, ListenerFactory};
use crate::ui::Ui;
use crate::warnings::Warnings;
use dashd_chainstate::tuning::MemoryTuning;

/// The session's coins database, which every open shares as a restarted process reopens its
/// files (ND1-D12: `MemoryEngine` is not `Clone`, and the guard takes its engine by value).
pub struct SharedMemory {
    coins: Arc<MemoryEngine>,
    /// The read error this open's block index scan starts with.
    #[cfg(test)]
    scan_fault: Option<ErrorKind>,
    /// The error this open's WAL syncs fail with: a chainstate that cannot open.
    #[cfg(test)]
    sync_fault: Option<ErrorKind>,
    /// While set, every write fails with an I/O error.
    #[cfg(test)]
    write_fault: Arc<std::sync::atomic::AtomicBool>,
    /// Once set, the next WAL sync fails with an I/O error, and clears it.
    #[cfg(test)]
    sync_once: Arc<std::sync::atomic::AtomicBool>,
    /// The column family whose whole-family scans (recovery's no-tip scan) start with a read
    /// error.
    #[cfg(test)]
    prefix_fault: Option<Cf>,
    /// How many WAL syncs reached this engine, failed ones included.
    #[cfg(test)]
    syncs: Arc<std::sync::atomic::AtomicUsize>,
}

impl SharedMemory {
    /// `coins` with no fault armed: the persisted image's read between a teardown and a load.
    fn plain(coins: Arc<MemoryEngine>) -> SharedMemory {
        SharedMemory {
            coins,
            #[cfg(test)]
            scan_fault: None,
            #[cfg(test)]
            sync_fault: None,
            #[cfg(test)]
            write_fault: Arc::default(),
            #[cfg(test)]
            sync_once: Arc::default(),
            #[cfg(test)]
            prefix_fault: None,
            #[cfg(test)]
            syncs: Arc::default(),
        }
    }
}

/// The session's files: the OS's, whose free space a test can take away.
#[cfg(not(test))]
type SutFs = OsFs;
#[cfg(test)]
type SutFs = LowSpaceFs;

/// The OS's files, with no free space while `low` is set, or in the directory `low_dir` holds:
/// the flush's and a block write's `CheckDiskSpace` fail.
#[cfg(test)]
struct LowSpaceFs {
    os: OsFs,
    low: Arc<std::sync::atomic::AtomicBool>,
    low_dir: Arc<std::sync::Mutex<Option<PathBuf>>>,
}

#[cfg(test)]
impl dashd_store::flatfile::FlatFs for LowSpaceFs {
    fn link_count(&self, path: &Path) -> std::io::Result<Option<u64>> {
        self.os.link_count(path)
    }

    fn len(&self, path: &Path) -> std::io::Result<Option<u64>> {
        self.os.len(path)
    }

    fn create(&self, path: &Path) -> std::io::Result<()> {
        self.os.create(path)
    }

    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.os.read_exact_at(path, offset, buf)
    }

    fn open_read(
        &self,
        path: &Path,
    ) -> std::io::Result<Box<dyn dashd_store::flatfile::FlatRead + '_>> {
        self.os.open_read(path)
    }

    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> std::io::Result<()> {
        self.os.write_all_at(path, offset, data)
    }

    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        self.os.allocate(path, offset, len);
    }

    fn truncate(&self, path: &Path, len: u64) -> std::io::Result<()> {
        self.os.truncate(path, len)
    }

    fn sync_data(&self, path: &Path) -> std::io::Result<()> {
        self.os.sync_data(path)
    }

    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        self.os.sync_dir(dir)
    }

    fn copy(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.os.copy(from, to)
    }

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.os.rename(from, to)
    }

    fn remove(&self, path: &Path) -> std::io::Result<bool> {
        self.os.remove(path)
    }

    fn create_dir_all(&self, dir: &Path) -> std::io::Result<()> {
        self.os.create_dir_all(dir)
    }

    fn available_space(&self, dir: &Path) -> std::io::Result<u64> {
        let low_dir = self.low_dir.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.low.load(std::sync::atomic::Ordering::SeqCst) || low_dir.as_deref() == Some(dir) {
            return Ok(0);
        }
        self.os.available_space(dir)
    }
}

impl KvRead for SharedMemory {
    fn get(&self, cf: Cf, key: &[u8]) -> KvResult<Option<Vec<u8>>> {
        self.coins.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> KvResult<Vec<Option<Vec<u8>>>> {
        self.coins.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = KvResult<(Vec<u8>, Vec<u8>)>> + 'a {
        #[cfg(test)]
        let fault = (self.prefix_fault == Some(cf) && prefix.is_empty())
            .then(|| Err(StoreError::engine("memory", ErrorKind::Io, "injected scan")));
        #[cfg(not(test))]
        let fault = None;
        fault.into_iter().chain(self.coins.iter_prefix(cf, prefix))
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = KvResult<(Vec<u8>, Vec<u8>)>> + 'a {
        self.coins.iter_prefix_from(cf, prefix, start)
    }

    /// Forwarded, as every wrapper does (2b-D19); in tests the block index scan (the only
    /// uncached read) starts with the injected read error, if one is set.
    fn iter_prefix_uncached<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = KvResult<(Vec<u8>, Vec<u8>)>> + 'a {
        #[cfg(test)]
        let fault = self.scan_fault.map(|kind| Err(StoreError::engine("memory", kind, "injected")));
        #[cfg(not(test))]
        let fault = None;
        fault.into_iter().chain(self.coins.iter_prefix_uncached(cf, prefix))
    }
}

impl KvEngine for SharedMemory {
    type Batch = MemoryBatch;
    type Snapshot<'a> = MemorySnapshot;

    fn schema(&self) -> &'static dashd_store::DbSchema {
        self.coins.schema()
    }

    fn batch(&self) -> MemoryBatch {
        self.coins.batch()
    }

    fn batch_with_capacity(&self, bytes: usize) -> MemoryBatch {
        self.coins.batch_with_capacity(bytes)
    }

    fn write(&self, batch: MemoryBatch, commit: Commit) -> KvResult<()> {
        #[cfg(test)]
        if self.write_fault.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::engine("memory", ErrorKind::Io, "injected write"));
        }
        self.coins.write(batch, commit)
    }

    fn sync_wal(&self) -> KvResult<()> {
        #[cfg(test)]
        self.syncs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        #[cfg(test)]
        if let Some(kind) = self.sync_fault {
            return Err(StoreError::engine("memory", kind, "injected"));
        }
        #[cfg(test)]
        if self.sync_once.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::engine("memory", ErrorKind::Io, "injected sync"));
        }
        self.coins.sync_wal()
    }

    fn flush_memtables(&self) -> KvResult<()> {
        self.coins.flush_memtables()
    }

    fn snapshot(&self) -> MemorySnapshot {
        self.coins.snapshot()
    }

    fn approximate_size(&self, cf: Cf) -> KvResult<u64> {
        self.coins.approximate_size(cf)
    }
}

/// How a restart ends the running process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Teardown {
    /// Core's `Shutdown`: the two flushes and the spork store (O2b D34).
    Clean,
    /// A killed process: nothing flushed or stored; the coins keep every committed batch.
    Kill,
}

/// A load's status, as the shim names Core's (`sh:834-836`, `:774-782`, `:847-848`): the
/// `ChainstateLoadStatus` of `LoadChainstate` and `VerifyLoadedChainstate`, the spork setup's
/// `init_error` and the import's `activate_failed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadStatus {
    Success,
    /// `FAILURE`: the index load's failures and `VerifyLoadedChainstate`'s, which init follows
    /// with the reindex question.
    Failure,
    /// `FAILURE_FATAL`: Core returns it only from a background (snapshot) chainstate's cleanup
    /// (`ncs:375`), which neither side has. Out of the session's domain.
    FailureFatal,
    FailureIncompatibleDb,
    FailureInsufficientDbcache,
    Interrupted,
    /// Init refused a step before the chainstate (spork address or key, a cache, an argument).
    InitError,
    /// The import's `ActivateBestChain` failed: "Failed to connect best block".
    ActivateFailed,
    /// An image's file step failed (a save's read, a restore's rewrite), so nothing loaded. Not a
    /// shim status: the image's callers return the error instead.
    ImageFiles,
}

impl LoadStatus {
    /// The shim's name.
    pub fn name(self) -> &'static str {
        match self {
            LoadStatus::Success => "success",
            LoadStatus::Failure => "failure",
            LoadStatus::FailureFatal => "failure_fatal",
            LoadStatus::FailureIncompatibleDb => "failure_incompatible_db",
            LoadStatus::FailureInsufficientDbcache => "failure_insufficient_dbcache",
            LoadStatus::Interrupted => "interrupted",
            LoadStatus::InitError => "init_error",
            LoadStatus::ActivateFailed => "activate_failed",
            LoadStatus::ImageFiles => "image_files",
        }
    }
}

/// The last load's status and Core's raw message (`error.original`, before init words it), kept
/// beside the CLI text [`NodeSut::down`] holds (CH3a a10 m-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadResult {
    pub status: LoadStatus,
    /// Empty on success.
    pub message: String,
}

impl LoadResult {
    fn success() -> LoadResult {
        LoadResult {
            status: LoadStatus::Success,
            message: String::new(),
        }
    }
}

/// What a restart or a restore read and loaded (`sh:940-953`).
#[derive(Debug)]
pub struct Restarted {
    /// The storage between the teardown and the load, before any load logic (O2b D37).
    pub persisted: Result<Stored, StoreError>,
    pub load: LoadResult,
}

/// A saved session (`sh:1104-1124`, `:1293-1308`): the coins' committed data and every regular
/// file under the network's data directory but the log.
#[derive(Clone)]
pub struct SutImage {
    coins: MemoryImage,
    /// Paths relative to the network's data directory.
    files: Vec<(PathBuf, Vec<u8>)>,
}

/// What a harness gives each open (CH3a a10): its listener, the flush regime, V's ChainLock
/// schedule, and whether its connector records ([`NodeSut::take_records`]).
#[derive(Clone, Default)]
pub struct Harness {
    pub listener: Option<Arc<ListenerFactory>>,
    pub regime: FlushRegime,
    pub cl_schedule: ClSchedule,
    pub records: bool,
}

/// The node's log, which a restore leaves where it is.
const LOG_FILE: &str = "debug.log";

/// A node's chainstate in a session directory: the block files and the spork file on disk there,
/// the coins in memory. One clock for the session, never reset (F10).
pub struct NodeSut {
    args: Arc<ArgsManager>,
    params: SelectedParams,
    /// Parameter interaction's decisions, which each load's coins' setup reads.
    interaction: ParamInteraction,
    ui: Arc<Ui>,
    clock: Arc<MockClock>,
    coins: Arc<MemoryEngine>,
    /// The read error every later open's block index scan starts with.
    #[cfg(test)]
    scan_fault: Option<ErrorKind>,
    /// The error every later open's WAL syncs fail with.
    #[cfg(test)]
    sync_fault: Option<ErrorKind>,
    /// The bug every later open arms: its connector's (xcheck's positive controls) or its step-10
    /// verifier's (the census controls).
    #[cfg(feature = "planted-bugs")]
    planted: Option<dashd_chainstate::Planted>,
    /// While set, every open's writes fail.
    #[cfg(test)]
    write_fault: Arc<std::sync::atomic::AtomicBool>,
    /// While set, every open's free disk space reads as none.
    #[cfg(test)]
    low_space: Arc<std::sync::atomic::AtomicBool>,
    /// The directory every open's free disk space reads as none in, while set.
    #[cfg(test)]
    low_dir: Arc<std::sync::Mutex<Option<PathBuf>>>,
    /// Once set, every open's next WAL sync fails (one-shot).
    #[cfg(test)]
    sync_once: Arc<std::sync::atomic::AtomicBool>,
    /// The column family whose no-tip scan fails in every later open.
    #[cfg(test)]
    prefix_fault: Option<Cf>,
    /// How many WAL syncs every open's engine has made, failed ones included.
    #[cfg(test)]
    syncs: Arc<std::sync::atomic::AtomicUsize>,
    /// Every later open's probe (`ChainSteps::probe`).
    #[cfg(test)]
    probe: Option<Arc<dyn Fn(crate::steps::Point) + Send + Sync>>,
    /// Every later open's start fault (`ChainSteps::start_fault`).
    #[cfg(test)]
    start_fault: Option<crate::steps::StartFault>,
    /// Every later open's fault around V's block files.
    #[cfg(test)]
    wrap_files: Option<crate::steps::WrapFiles>,
    /// Every open's listener and flush regime.
    harness: Harness,
    /// What every open's flush side completed, by cause (CH3a a10-2).
    tally: Arc<FlushTally>,
    /// Every later open's call on V just before init's `VerifyLoaded`.
    #[cfg(test)]
    before_verify: Option<crate::steps::BeforeVerify>,
    /// The state-hash sink the next load's connector takes (CH7a-5b X5): set before the first
    /// open by [`new_with_digest`](Self::new_with_digest), so the sink sees genesis's `C` first;
    /// empty after that load, so a restart reopens without one.
    #[cfg(test)]
    pending_digest: Option<Box<dyn dashd_chainstate::connect::DigestSink>>,
    /// Every later open's CH7a-5b plant: H11 reads the credit pool as Core's exception at this
    /// height ([`NodeEvo::plant_h11_pool_error_at`]).
    #[cfg(test)]
    h11_pool_error_at: Option<u32>,
    /// This open's shutdown flags, as a process's.
    shutdown: Arc<ShutdownState>,
    /// This open's warnings, as a process's.
    warnings: Arc<Warnings>,
    steps: ChainSteps<SharedMemory, SutFs>,
    /// The refused load's message: the production shutdown ran and nothing is loaded (F6).
    down: Option<String>,
    /// The last load's typed result.
    last_load: LoadResult,
    /// The handle on every open's disconnect pools (DEC-500): its count spans the opens, which
    /// `steps` (rebuilt per open) cannot keep.
    pools: PoolsOnly,
    /// What every open's records tap wrote and [`take_records`](Self::take_records) has not
    /// taken.
    records: RecordsBuffer,
    /// The ChainLock handler's `seenChainLocks` ([`cl_record`](Self::cl_record)): Core's handler
    /// is rebuilt at each load, so each open starts it empty.
    seen: HashSet<[u8; 32]>,
}

/// The bytes a records tap writes, shared by its log and its decisions.
#[derive(Clone, Default)]
struct RecordsBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for RecordsBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// O2b's records tap on one open's connector (CH3a a10 C4), as the shim records Core's in memory
/// (`statedigest::InitMemory`): a [`DigestLog::without_golden`] and a [`RecordingHelper`] over
/// [`NodeHelper`], each `C` and `R` followed by its `A`, into the session's buffer.
#[derive(Clone)]
pub struct RecordsTap {
    out: RecordsBuffer,
    clock: Arc<dyn Clock>,
}

impl RecordsTap {
    /// The connector's helper and digest sink. The log's `H` opens its records.
    pub(crate) fn parts(
        &self,
        params: &ChainSetup,
    ) -> std::io::Result<(Box<dyn ChainHelper>, Box<dyn DigestSink>)> {
        let chain = match params.network {
            Network::Main => Chain::Main,
            Network::Testnet3 => Chain::Test,
            Network::Regtest => Chain::Regtest,
            Network::Devnet => Chain::Devnet,
        };
        let header = Header {
            file: FileKind::Memory,
            src: Writer::Rdashd,
            ver: format!("rdashd {} NodeSut", env!("CARGO_PKG_VERSION")).into_bytes(),
            chain,
            run: b"NodeSut".to_vec(),
        };
        let log = DigestLog::without_golden(self.out.clone(), header)
            .map_err(|f: Fatal| std::io::Error::other(f.0))?;
        let tap = DecisionTap::new();
        let helper =
            RecordingHelper::new(Box::new(NodeHelper), tap.clone(), Arc::clone(&self.clock));
        Ok((Box::new(helper), Box::new(TapedLog::new(log, self.out.clone(), tap))))
    }
}

/// `CL_RECORD`'s outcome (`ORACLE_CL_OUTCOME`, `chainlock/handler.cpp:139-206` at the pin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClRecorded {
    Seen,
    Stale,
    NotBetter,
    Accepted,
}

impl ClRecorded {
    /// The shim's reply text.
    pub fn name(self) -> &'static str {
        match self {
            ClRecorded::Seen => "seen",
            ClRecorded::Stale => "stale",
            ClRecorded::NotBetter => "not-better",
            ClRecorded::Accepted => "accepted",
        }
    }
}

/// Why a load was refused: init's CLI text, and the typed result.
struct Refusal {
    message: String,
    result: LoadResult,
    /// `-dbcrashratio` came up: Core's `_Exit(0)` runs no `Shutdown`.
    crashed: bool,
}

impl Refusal {
    /// A refusal whose CLI text is `message` and whose raw message is `raw`.
    fn new(status: LoadStatus, message: String, raw: String) -> Refusal {
        Refusal {
            message,
            result: LoadResult {
                status,
                message: raw,
            },
            crashed: false,
        }
    }
}

/// Init's own refusals: `init_error`, the CLI text as the raw message.
impl From<InitFailure> for Refusal {
    fn from(failure: InitFailure) -> Refusal {
        let message = match failure {
            InitFailure::Error(message) | InitFailure::Exception(message) => message,
            InitFailure::Reported => {
                debug_assert!(false, "no chainstate step reports its own failure");
                String::new()
            }
        };
        Refusal::new(LoadStatus::InitError, message.clone(), message)
    }
}

// Core: init.cpp:2137, :2578
const SHUTDOWN_REQUESTED: &str = "Shutdown requested. Exiting.";

/// The import's `AbortNode` when its `ActivateBestChain` fails (`crate::chain::Chain::import`).
// Core: node/blockstorage.cpp:968-969 at the pin
const ACTIVATE_FAILED: &str = "Failed to connect best block";

impl NodeSut {
    /// Parses `argv` as rdashd's command line with `-datadir=dir` first, runs production's
    /// `InitConfig` and parameter interaction over it (F4) with `-mocktime` set to `mocktime`, then
    /// loads. A refused load leaves the SUT [down](Self::down), with Core's message there rather
    /// than in the log.
    ///
    /// Only `InitConfig`, parameter interaction, the validation caches and step 7's cache
    /// configuration (its lines, the `-maxmempool` floor and the coins' setup) are the node's: no
    /// `-server` soft-set or upload target, and no unsupported-option refusal (the harness passes
    /// `-dbcache`). `-consensusxcheck` is soft-set on (CH3a D189), so `-noconsensusxcheck` in `argv`
    /// turns it off. Parameter interaction applies `-debug`
    /// and `-loglevel` to `ui`'s logger, sends its warnings to `ui`, and raises the process's
    /// descriptor limit; `ui`'s logging is the caller's to set up and start.
    ///
    /// # Errors
    ///
    /// Arguments init refuses before any load, and a `-mocktime` in `argv` (the session's clock
    /// is `mocktime`).
    pub fn new(
        dir: &Path,
        argv: &[&str],
        mocktime: i64,
        ui: Arc<Ui>,
    ) -> Result<NodeSut, InitFailure> {
        Self::with_harness(dir, argv, mocktime, ui, Harness::default())
    }

    /// [`new`](Self::new) with `harness`'s listener and flush regime from the first open on
    /// (CH3a a10): each V gets a listener from it as it starts, and every open's flush policy
    /// runs the regime.
    ///
    /// # Errors
    ///
    /// As [`new`](Self::new).
    pub fn with_harness(
        dir: &Path,
        argv: &[&str],
        mocktime: i64,
        ui: Arc<Ui>,
        harness: Harness,
    ) -> Result<NodeSut, InitFailure> {
        let coins = Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA));
        let mut sut = Self::unopened(dir, argv, mocktime, ui, coins)?;
        sut.harness = harness;
        sut.open();
        Ok(sut)
    }

    /// [`new`](Self::new) with `sink` recording every connector attempt from the first load's
    /// genesis on (CH7a-5b X5). The sink is taken once, by that load: a restart reopens without
    /// it. Fresh datadirs only: a `DigestLog` expects genesis first (CH3a D141).
    #[cfg(test)]
    pub(crate) fn new_with_digest(
        dir: &Path,
        argv: &[&str],
        mocktime: i64,
        ui: Arc<Ui>,
        sink: Box<dyn dashd_chainstate::connect::DigestSink>,
    ) -> Result<NodeSut, InitFailure> {
        let coins = Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA));
        let mut sut = Self::unopened(dir, argv, mocktime, ui, coins)?;
        sut.pending_digest = Some(sink);
        sut.open();
        Ok(sut)
    }

    /// Whether a sink is still waiting for a load ([`new_with_digest`](Self::new_with_digest)).
    #[cfg(test)]
    pub(crate) fn digest_pending(&self) -> bool {
        self.pending_digest.is_some()
    }

    /// [`new`](Self::new) over `coins`: a session with other arguments on a datadir and coins an
    /// earlier one left (its teardown done and `coins` restarted), as a node restarted with new
    /// arguments.
    #[cfg(test)]
    fn with_coins(
        dir: &Path,
        argv: &[&str],
        mocktime: i64,
        ui: Arc<Ui>,
        coins: Arc<MemoryEngine>,
    ) -> Result<NodeSut, InitFailure> {
        let mut sut = Self::unopened(dir, argv, mocktime, ui, coins)?;
        sut.open();
        Ok(sut)
    }

    /// [`with_coins`](Self::with_coins) before its first open.
    fn unopened(
        dir: &Path,
        argv: &[&str],
        mocktime: i64,
        ui: Arc<Ui>,
        coins: Arc<MemoryEngine>,
    ) -> Result<NodeSut, InitFailure> {
        let args = ArgsManager::new();
        setup_server_args(&args);
        let datadir = format!("-datadir={}", dir.display());
        let mut full = vec!["rdashd", datadir.as_str()];
        full.extend_from_slice(argv);
        args.parse_parameters(&full).map_err(InitFailure::Error)?;
        let params = init::init_config(&args)?;
        if args.is_arg_set("-mocktime") {
            return Err(InitFailure::Error("NodeSut sets -mocktime itself".to_owned()));
        }
        args.force_set_arg("-mocktime", &mocktime.to_string());
        let logger = Arc::clone(ui.logger());
        let clock = Arc::new(MockClock::new());
        logger.set_clock(Arc::clone(&clock));
        // `diff_chain`'s node side runs with `-consensusxcheck` (CH3 §6.2 L1, CH3a D189), read by
        // the node's own coins setup. Soft-set after the config files, as Core's `SoftSet*` calls.
        args.soft_set_bool_arg("-consensusxcheck", true);
        init::init_parameter_interaction(&args, &logger)?;
        let interaction = init::app_init_parameter_interaction(&ui, &args, &params.chain, &clock)?;
        let shutdown = Arc::new(ShutdownState::new());
        let warnings = Arc::new(Warnings::new());
        Ok(NodeSut {
            args: Arc::new(args),
            params,
            interaction,
            ui,
            clock,
            coins,
            #[cfg(test)]
            scan_fault: None,
            #[cfg(test)]
            sync_fault: None,
            #[cfg(feature = "planted-bugs")]
            planted: None,
            #[cfg(test)]
            write_fault: Arc::default(),
            #[cfg(test)]
            low_space: Arc::default(),
            #[cfg(test)]
            low_dir: Arc::default(),
            #[cfg(test)]
            sync_once: Arc::default(),
            #[cfg(test)]
            prefix_fault: None,
            #[cfg(test)]
            syncs: Arc::default(),
            #[cfg(test)]
            probe: None,
            #[cfg(test)]
            start_fault: None,
            #[cfg(test)]
            wrap_files: None,
            harness: Harness::default(),
            tally: Arc::default(),
            #[cfg(test)]
            before_verify: None,
            #[cfg(test)]
            pending_digest: None,
            #[cfg(test)]
            h11_pool_error_at: None,
            steps: ChainSteps::new(logger, Arc::clone(&shutdown), Arc::clone(&warnings)),
            shutdown,
            warnings,
            down: None,
            last_load: LoadResult::success(),
            pools: PoolsOnly::default(),
            records: RecordsBuffer::default(),
            seen: HashSet::new(),
        })
    }

    /// Ends this process the way `how` says, then starts the next one on the same files and
    /// clock. Returns the teardown's flushes: none while down, whose teardown ran at the load
    /// (its flushes are [`flushes`](Self::flushes) until this restart).
    pub fn restart(&mut self, how: Teardown) -> u32 {
        let flushes = self.teardown(how);
        self.open();
        flushes
    }

    /// [`restart`](Self::restart), with what the storage held between the teardown and the
    /// load and the load's result (`sh:940-953`). A down SUT's teardown ran at its load
    /// (`sh:943`).
    pub fn restart_report(&mut self, how: Teardown) -> Restarted {
        self.teardown(how);
        self.reopen()
    }

    /// Ends this process the way `how` says, its coins reopened as the next process's; the
    /// teardown's flushes.
    fn teardown(&mut self, how: Teardown) -> u32 {
        let before = self.steps.flushes();
        match how {
            Teardown::Clean => self.steps.shutdown(),
            Teardown::Kill => self.steps.kill(),
        }
        let flushes = self.steps.flushes() - before;
        self.coins.restart();
        flushes
    }

    /// The storage as the next load finds it (O2b D37), read through a guard of its own with no
    /// fault armed.
    fn persisted(&self) -> Result<Stored, StoreError> {
        let db = GuardedEngine::new(
            SharedMemory::plain(Arc::clone(&self.coins)),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        );
        Stored::read(&db)
    }

    /// Core's `oracle_chain_save` (`sh:1293-1308`): a clean teardown, the image, then the next
    /// process on it.
    ///
    /// # Errors
    ///
    /// The data directory could not be read; the SUT is then left down ([`down`](Self::down)).
    pub fn save_image(&mut self) -> std::io::Result<SutImage> {
        self.teardown(Teardown::Clean);
        let files = self.net_dir().and_then(|dir| read_files(&dir));
        let image = SutImage {
            coins: self.coins.image(),
            files: self.left_down(files)?,
        };
        self.open();
        Ok(image)
    }

    /// Core's `oracle_chain_restore` (`sh:1311-1328`): the running process killed (no flush, no
    /// spork store; `sh:892`), the clock at `mocktime`, the data directory rewritten to exactly
    /// `image`'s files, its coins, then the next process on them.
    ///
    /// # Errors
    ///
    /// The data directory could not be rewritten; the SUT is then left down
    /// ([`down`](Self::down)).
    pub fn restore(&mut self, image: &SutImage, mocktime: i64) -> std::io::Result<Restarted> {
        self.teardown(Teardown::Kill);
        self.clock.set_mocktime(mocktime);
        let written = self.net_dir().and_then(|dir| write_files(&dir, &image.files));
        self.left_down(written)?;
        self.coins.replace(&image.coins);
        Ok(self.reopen())
    }

    /// `result`, which an image's file step gave after the teardown; an error leaves the SUT
    /// down with it, nothing loaded, and the load's result [`LoadStatus::ImageFiles`] with it.
    fn left_down<T>(&mut self, result: std::io::Result<T>) -> std::io::Result<T> {
        if let Err(e) = &result {
            let message = format!("the data directory: {e}");
            self.down = Some(message.clone());
            self.last_load = LoadResult {
                status: LoadStatus::ImageFiles,
                message,
            };
        }
        result
    }

    /// The next process on what a teardown left: the storage it finds, then its load.
    fn reopen(&mut self) -> Restarted {
        let persisted = self.persisted();
        self.open();
        Restarted {
            persisted,
            load: self.last_load.clone(),
        }
    }

    /// The network's data directory: the block files, the spork file and the caches.
    fn net_dir(&self) -> std::io::Result<PathBuf> {
        self.args.get_data_dir_net().map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Steps 7a to 11 on fresh shutdown flags. A refusal runs the production shutdown over what
    /// loaded, or a kill's after a simulated crash.
    fn open(&mut self) {
        self.tally.opened();
        self.shutdown = Arc::new(ShutdownState::new());
        self.warnings = Arc::new(Warnings::new());
        self.steps = ChainSteps::new(
            Arc::clone(self.ui.logger()),
            Arc::clone(&self.shutdown),
            Arc::clone(&self.warnings),
        );
        self.steps.pools = Some(self.pools.clone());
        self.steps.listener.clone_from(&self.harness.listener);
        self.steps.cl_schedule = self.harness.cl_schedule;
        self.seen.clear();
        #[cfg(test)]
        {
            self.steps.wrap_files = self.wrap_files;
            self.steps.before_verify = self.before_verify;
            self.steps.start_fault = self.start_fault;
            if let Some(probe) = &self.probe {
                let probe = Arc::clone(probe);
                self.steps.probe = Some(Box::new(move |point| probe(point)));
            }
        }
        (self.down, self.last_load) = match self.load() {
            Ok(()) => (None, LoadResult::success()),
            Err(refusal) => {
                if refusal.crashed {
                    self.steps.kill();
                } else {
                    // rdashd's `main` after a failed `AppInit`, before `Shutdown`.
                    self.shutdown.set_exit_status(crate::shutdown::EXIT_FAILURE);
                    self.steps.shutdown();
                }
                (Some(refusal.message), refusal.result)
            }
        };
    }

    /// `AppInitMain`'s chainstate steps as the node runs them, with its load statuses.
    // Core: init.cpp:2025-2138, :2577-2580
    fn load(&mut self) -> Result<(), Refusal> {
        let args = &*self.args;
        let logger = self.ui.logger();
        let chain = &self.params.chain;
        self.steps.make_sporks(args, &self.params, Arc::clone(&self.clock) as _)?;
        self.steps.make_net_caches(&self.params, Arc::clone(&self.clock) as _);
        self.steps.load_sporks(args, &self.params)?;
        let abort = Arc::new(NodeAbort::new(
            Arc::clone(&self.ui),
            Arc::clone(&self.warnings),
            Arc::clone(&self.shutdown),
        ));
        let import_abort = Arc::clone(&abort);
        let blocks_dir = args.get_blocks_dir_path().map_err(InitFailure::from)?;
        let data_dir = args.get_data_dir_net().map_err(InitFailure::from)?;
        let coins = SharedMemory {
            coins: Arc::clone(&self.coins),
            #[cfg(test)]
            scan_fault: self.scan_fault,
            #[cfg(test)]
            sync_fault: self.sync_fault,
            #[cfg(test)]
            write_fault: Arc::clone(&self.write_fault),
            #[cfg(test)]
            sync_once: Arc::clone(&self.sync_once),
            #[cfg(test)]
            prefix_fault: self.prefix_fault,
            #[cfg(test)]
            syncs: Arc::clone(&self.syncs),
        };
        let clock = Arc::clone(&self.clock) as _;
        let caches = Arc::new(init_validation_caches(logger, args)?);
        let queue = Arc::new(start_script_check_threads(logger, args)?);
        let (sizes, setup) = coins_configuration(
            args,
            chain,
            &self.interaction,
            logger,
            Arc::new(MemoryTuning),
            false,
            caches,
            queue,
            Arc::clone(self.steps.metaman().expect("made at step 6")),
            // W's crash returns: its latch trip stops the flush side, as a kill would.
            Arc::new(|| {}),
        )?;
        let batch_size = db_batch_size(args)?;
        let crash_ratio = setup.crash_ratio;
        #[cfg(feature = "planted-bugs")]
        let setup = crate::chainstate::CoinsSetup {
            planted: self.planted,
            ..setup
        };
        let setup = crate::chainstate::CoinsSetup {
            regime: self.harness.regime,
            tally: Some(Arc::clone(&self.tally)),
            records: self.harness.records.then(|| RecordsTap {
                out: self.records.clone(),
                clock: Arc::clone(&self.clock) as _,
            }),
            ..setup
        };
        // X5: the pending sink moves to this load's connector, once.
        #[cfg(test)]
        let setup = crate::chainstate::CoinsSetup {
            digest: std::sync::Mutex::new(self.pending_digest.take()),
            h11_pool_error_at: self.h11_pool_error_at,
            ..setup
        };
        #[cfg(test)]
        let fs = Arc::new(LowSpaceFs {
            os: OsFs::new(),
            low: Arc::clone(&self.low_space),
            low_dir: Arc::clone(&self.low_dir),
        });
        #[cfg(not(test))]
        let fs = Arc::new(OsFs::new());
        let opened = self.steps.load(
            args,
            &self.ui,
            Arc::new(chain.clone()),
            clock,
            Arc::clone(&abort),
            setup,
            || {
                Opening::on(
                    |_| Ok(coins),
                    Arc::clone(&fs),
                    &data_dir,
                    &blocks_dir,
                    chain,
                    &sizes,
                    batch_size,
                    crash_ratio,
                    Arc::clone(logger),
                    abort,
                )
            },
        )?;
        if let Err(error) = opened {
            if let OpenError::Recoverable {
                cause: Some(cause),
                ..
            } = &error
            {
                logger.log_printf(&format!("{cause}\n"));
            }
            return Err(open_refusal(error, self.shutdown.shutdown_requested()));
        }
        self.steps.load_net_caches(args, &self.params)?;
        let reports = import_abort.reports();
        self.steps.import()?;
        self.steps.join_import();
        if self.shutdown.shutdown_requested() {
            // The import's `ActivateBestChain` failed, or a shutdown came. V reports its own
            // `AbortNode` first, then `ThreadImport` its "Failed to connect best block".
            let failed = (reports..import_abort.reports())
                .filter_map(|n| import_abort.report(n))
                .map(|report| report.log)
                .find(|log| log.starts_with(ACTIVATE_FAILED));
            let (status, raw) = match failed {
                Some(log) => (LoadStatus::ActivateFailed, log),
                None => (LoadStatus::Interrupted, String::new()),
            };
            return Err(Refusal::new(status, SHUTDOWN_REQUESTED.to_owned(), raw));
        }
        Ok(())
    }

    /// The refused load's message while nothing is loaded.
    pub fn down(&self) -> Option<&str> {
        self.down.as_deref()
    }

    /// The last load's status and Core's raw message (CH3a a10 m-1).
    pub fn load_result(&self) -> &LoadResult {
        &self.last_load
    }

    /// Every later open's listener factory ([`Harness::listener`]).
    pub fn set_listener(&mut self, listener: Option<Arc<ListenerFactory>>) {
        self.harness.listener = listener;
    }

    /// A step's end (CH3a a10 Q2, `sh:1044-1049`): V settles (W's releases and the flushes they
    /// leave, [`ChainHandle::settle`]), then its signals queue drains, so a listener has heard
    /// all the step caused. What V left: empty but for `rounds` and `suppressed` (a debug
    /// assertion). `None` while down, or once V has stopped.
    ///
    /// # Panics
    ///
    /// Leg A's check (CH3a a10-2): under [`FlushRegime::CoreCausesOnly`], if the policy removed
    /// a cause of ours other than `LEFT_IBD` (expected once, and counted), or a booked job had
    /// one.
    pub fn settle(&self) -> Option<Quiet> {
        let chain = self.steps.chain()?;
        let quiet = chain.handle().settle().recv().ok()?;
        debug_assert!(quiet.is_quiet(), "V settled: {quiet:?}");
        if self.harness.regime == FlushRegime::CoreCausesOnly {
            let (s, booked) = (quiet.suppressed, self.tally.non_core());
            assert!(
                s.threshold == 0 && s.dmax == 0 && s.empty == 0 && booked.is_empty(),
                "leg A met a cause outside Core's: suppressed {s:?}, booked {booked:?}"
            );
        }
        chain.signals().sync(&mut Blocking).ok()?;
        Some(quiet)
    }

    /// V, while loaded.
    pub fn chain(&self) -> Option<&ChainHandle> {
        self.steps.chain().map(|chain| chain.handle())
    }

    /// This open's concise warning, Core's `GetWarnings(false)` (`getblockchaininfo`'s
    /// `warnings`).
    pub fn warnings(&self) -> String {
        self.warnings.get_warnings(false)
    }

    /// The spork manager, while loaded.
    pub fn sporks(&self) -> Option<&Arc<SporkManager>> {
        self.steps.sporks()
    }

    /// The chain parameters init built from the command line (O2b D8's parity check reads them).
    pub fn params(&self) -> &ChainSetup {
        &self.params.chain
    }

    pub fn clock(&self) -> &Arc<MockClock> {
        &self.clock
    }

    /// The block the coins are at, while loaded.
    pub fn coins_tip(&self) -> Option<BlockHash> {
        self.steps.coins_tip()
    }

    /// The UTXO set as O2b's shim reads Core's ([`CoinsCache::tip_coins`]), in one class R query
    /// on V: the coins cache is V's leaf (CH3a D50), and between commands V holds no view of it.
    /// `None` while the coins are not attached or once V has stopped.
    ///
    /// [`CoinsCache::tip_coins`]: dashd_chainstate::coins::CoinsCache::tip_coins
    pub fn tip_coins(&self) -> Option<Result<Vec<(OutPointKey, PackedCoin)>, CoinsReadError>> {
        let cell = Arc::clone(self.steps.coins_cell()?);
        let read = move |_: &ChainQuery<'_>| {
            cell.try_lock().expect("V's leaf, taken on V between commands").tip_coins()
        };
        self.chain()?.query(read).recv().ok()
    }

    /// V's disconnect pools over this SUT's opens ([`PoolsOnly`], DEC-500): what they evicted and
    /// V's calls on them. V keeps Core's pools as a node with a mempool does; the oracle's Core
    /// has no mempool and keeps none (`validation.cpp:3412` at the pin), which no compared plane
    /// sees.
    pub fn pools(&self) -> &PoolsOnly {
        &self.pools
    }

    /// Registers `listener` on the loaded V's validation signals, after the harness's own
    /// ([`Harness::listener`]); `false` while nothing is loaded. The next load drops it.
    pub fn register_listener(&self, listener: Arc<dyn ValidationInterface>) -> bool {
        self.steps.chain().map(|chain| chain.signals().register(listener)).is_some()
    }

    /// `SyncWithValidationInterfaceQueue` on V's signals, from a thread that may wait: whether
    /// the queue's thread reached the barrier ([`settle`](Self::settle) syncs too, after V's
    /// flush work).
    pub fn sync_signals(&self) -> bool {
        self.steps.chain().is_some_and(|chain| chain.signals().sync(&mut Blocking).is_ok())
    }

    /// Step 6's masternode meta store, which the chainstate's `ResetPlatformBan` effects reach
    /// (`MetaPlatformBans`); `None` before step 6. BL-526 plants a platform ban here.
    pub fn metaman(&self) -> Option<&Arc<MasternodeMetaMan>> {
        self.steps.metaman()
    }

    /// The flushes since the last load (its own teardown's, while down).
    pub fn flushes(&self) -> u32 {
        self.steps.flushes()
    }

    /// The flushes V's flush side completed, by cause, across this SUT's opens (CH3a a10-2;
    /// O2b §2.3): `since_open` from each open, `delta` from each [`begin_op`](Self::begin_op),
    /// across a teardown and the reopen, a refused load's teardown included. Booked as W
    /// releases each job; [`settle`](Self::settle) takes the releases a step left.
    // Core: src/test/oracle/oracle_chain.cpp:497-505, :763-768, :928-930 at the pin
    pub fn tally(&self) -> &FlushTally {
        &self.tally
    }

    /// An op starts: the tally's `delta` is empty.
    pub fn begin_op(&self) {
        self.tally.begin_op();
    }

    /// The records every open's tap wrote since the last call (empty without
    /// [`Harness::records`]), each open's starting with its `H`.
    ///
    /// # Errors
    ///
    /// A line the tap wrote does not parse.
    pub fn take_records(&self) -> Result<Vec<Record>, String> {
        let bytes =
            std::mem::take(&mut *self.records.0.lock().unwrap_or_else(PoisonError::into_inner));
        Lines::new(&bytes)
            .map(|(line, record)| record.map_err(|e| format!("record line {line}: {e:?}")))
            .collect()
    }

    /// The shim's `CL_RECORD` (O2b D12): Core's `ProcessNewChainLock` for the lock `(height,
    /// hash, sig)` from no peer, in its order. `CheckActiveState` and the best height first
    /// (`ClIntake`), then the seen check on `::SerializeHash(clsig)` (height, hash and signature),
    /// inserted before the stale check, the signature taken as valid (an M3 session has no
    /// quorums), then `UpdateBestChainlock`. `None` once V has stopped. The key hashes `sig` as
    /// given, which is Core's serialization only for a valid or all-zero encoding: Core reads an
    /// invalid one as the null signature. The seen set is unbounded, where Core's
    /// `seenChainLocks` is a limited map pruned on insert: past its 2048 distinct locks in one
    /// process, Core may forget a lock this still calls seen.
    // Core: src/chainlock/handler.h:57-58, src/chainlock/handler.cpp:150-163 at the pin
    // Core: src/chainlock/handler.cpp:139-206, src/test/oracle/oracle_chain.cpp:1051-1056 at the
    // pin
    pub fn cl_record(&mut self, height: i32, hash: BlockHash, sig: [u8; 96]) -> Option<ClRecorded> {
        let v = self.steps.chain()?.handle();
        let intake = v.cl_intake().recv().ok()?;
        let mut ser = height.to_le_bytes().to_vec();
        ser.extend_from_slice(hash.as_bytes());
        ser.extend_from_slice(&sig);
        let sig_hash = sha256d(&ser);
        if !self.seen.insert(sig_hash) {
            return Some(ClRecorded::Seen);
        }
        if height <= intake.best_height {
            return Some(ClRecorded::Stale);
        }
        let sig = Arc::new(ChainLockSig {
            height,
            block_hash: hash,
            sig,
        });
        let reply = v.record_chainlock(
            sig,
            <dash_num::Hash256 as dash_types::Numeric>::from_lendian(sig_hash),
            None,
        );
        Some(match reply.recv().ok()? {
            r if r.relay => ClRecorded::Accepted,
            _ => ClRecorded::NotBetter,
        })
    }
}

/// The chainstate load's refusal: init's text (the reindex question for a load `-reindex` may
/// recover, unless a shutdown came) and Core's status and raw message.
// Core: init.cpp:2115-2137
fn open_refusal(error: OpenError, shutdown_requested: bool) -> Refusal {
    let refused = |status, raw: String| Refusal::new(status, raw.clone(), raw);
    match error {
        // Out of domain: the shim runs no `-dbcrashratio`, and `NodeSide::with_argv` refuses
        // it; `failure` only stands in.
        OpenError::Crashed => Refusal {
            crashed: true,
            ..refused(LoadStatus::Failure, "Simulating a crash. Goodbye.".to_owned())
        },
        OpenError::Incompatible(message) => refused(LoadStatus::FailureIncompatibleDb, message),
        OpenError::InsufficientDbcache(message) => {
            refused(LoadStatus::FailureInsufficientDbcache, message)
        }
        OpenError::Interrupted(message) => {
            Refusal::new(LoadStatus::Interrupted, SHUTDOWN_REQUESTED.to_owned(), message)
        }
        OpenError::Recoverable {
            message,
            ..
        } => {
            let text = if shutdown_requested {
                SHUTDOWN_REQUESTED.to_owned()
            } else {
                reindex_question(&message)
            };
            Refusal::new(LoadStatus::Failure, text, message)
        }
    }
}

/// Every regular file under `dir` but the log, by path relative to it, in path order.
fn read_files(dir: &Path) -> std::io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut files = Vec::new();
    for path in file_paths(dir)? {
        let data = std::fs::read(dir.join(&path))?;
        files.push((path, data));
    }
    Ok(files)
}

/// Rewrites `dir` to exactly `files` (the log aside): every other regular file is removed.
fn write_files(dir: &Path, files: &[(PathBuf, Vec<u8>)]) -> std::io::Result<()> {
    for path in file_paths(dir)? {
        if !files.iter().any(|(kept, _)| *kept == path) {
            std::fs::remove_file(dir.join(path))?;
        }
    }
    for (path, data) in files {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, data)?;
    }
    Ok(())
}

/// The regular files under `dir` but the log, relative to it, sorted.
fn file_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(sub) = dirs.pop() {
        for entry in std::fs::read_dir(dir.join(&sub))? {
            let entry = entry?;
            let path = sub.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() && path != Path::new(LOG_FILE) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use dash_consensus::header::MAX_FUTURE_BLOCK_TIME;
    use dash_consensus::primitives::{OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxType};
    use dash_evo::spork::SporkId;
    use dashd_chainstate::chain::verify::{
        CORRUPTED_BLOCK_DB, FUTURE_TIP, INSUFFICIENT_DBCACHE, VERIFY_INTERRUPTED,
    };
    use dashd_chainstate::flush::CauseCounts;
    use dashd_store::KvBatch as _;

    use super::*;
    use crate::chain::tests::NoRoomAtOne;
    use crate::logging::Logger;
    use crate::steps::Point;

    mod bl636;
    mod recorded;
    mod state_hash;
    mod testnet;

    /// Core's functional-test key, regtest's spork signer.
    const SPORK_KEY: &str = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
    const MOCKTIME: i64 = 1_700_000_000;
    const SPORK: SporkId = SporkId::SPORK_2_INSTANTSEND_ENABLED;

    fn ui() -> Arc<Ui> {
        Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})))
    }

    fn try_sut(dir: &Path, argv: &[&str], ui: Arc<Ui>) -> Result<NodeSut, InitFailure> {
        let mut full = vec!["-regtest", "-printtoconsole=0"];
        full.extend_from_slice(argv);
        NodeSut::new(dir, &full, MOCKTIME, ui)
    }

    /// [`try_sut`] with `sink` recording from genesis ([`NodeSut::new_with_digest`]).
    fn try_sut_with_digest(
        dir: &Path,
        argv: &[&str],
        ui: Arc<Ui>,
        sink: Box<dyn dashd_chainstate::connect::DigestSink>,
    ) -> Result<NodeSut, InitFailure> {
        try_sut_at(dir, argv, ui, MOCKTIME, Some(sink))
    }

    /// [`try_sut`] at `mocktime`, with `sink` recording from genesis when there is one.
    fn try_sut_at(
        dir: &Path,
        argv: &[&str],
        ui: Arc<Ui>,
        mocktime: i64,
        sink: Option<Box<dyn dashd_chainstate::connect::DigestSink>>,
    ) -> Result<NodeSut, InitFailure> {
        let mut full = vec!["-regtest", "-printtoconsole=0"];
        full.extend_from_slice(argv);
        match sink {
            Some(sink) => NodeSut::new_with_digest(dir, &full, mocktime, ui, sink),
            None => NodeSut::new(dir, &full, mocktime, ui),
        }
    }

    fn sut(dir: &Path, argv: &[&str]) -> NodeSut {
        try_sut(dir, argv, ui()).unwrap_or_else(|refused| panic!("{argv:?} refused: {refused:?}"))
    }

    fn loaded(sut: &NodeSut) {
        assert_eq!(sut.down(), None);
        let v = sut.chain().expect("V runs");
        assert!(v.view().snap.has_tip(), "startup reached genesis");
    }

    /// A clean restart runs Core's two flushes and stores the sporks the next load reads.
    // Core: init.cpp:385, :415, :455
    #[test]
    fn a_clean_restart_flushes_twice_and_keeps_the_sporks() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &[SPORK_KEY]);
        loaded(&sut);
        let off = sut.sporks().unwrap().value(SPORK);
        sut.sporks().unwrap().update_spork(SPORK, off + 1).expect("signed");
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        assert_eq!(sut.flushes(), 0, "counted per open");
        assert_eq!(sut.sporks().unwrap().value(SPORK), off + 1);
        assert_eq!(sut.clock().mocktime(), MOCKTIME, "one clock, never reset");
    }

    /// BL-525: step 3's `-checkblockindex` reaches V's `ShouldCheckBlockIndex()` and the
    /// chain's header subset of it at step 7: on at regtest's default, off at `=0` and its
    /// negation, on again when given. Both the fresh index and, after a clean restart, the
    /// stored one take it.
    // Core: node/chainstatemanager_args.cpp:22, validation.cpp:6774, validation.h:961
    #[test]
    fn checkblockindex_reaches_v_and_the_chain() {
        let cases: [(&[&str], bool); 4] = [
            (&[], true),
            (&["-checkblockindex=0"], false),
            (&["-nocheckblockindex"], false),
            (&["-checkblockindex"], true),
        ];
        for (argv, on) in cases {
            let dir = tempfile::tempdir().unwrap();
            let mut sut = sut(dir.path(), argv);
            for open in ["fresh", "stored"] {
                if open == "stored" {
                    sut.restart(Teardown::Clean);
                }
                loaded(&sut);
                let checks =
                    |q: &ChainQuery<'_>| (q.check_block_index, q.chain.config().consistency_checks);
                let got = sut.chain().unwrap().query(checks).recv();
                assert_eq!(got, Ok((on, on)), "{argv:?} {open}");
            }
        }
    }

    /// SP1: a clean restart stores `netfulfilled.dat` and `mncache.dat` and a restart on a
    /// stored tip loads them; a kill stores neither. A file Core cannot read stops the load with
    /// Core's init error and stays as it was.
    // Core: init.cpp:2273-2289, :418-419
    #[test]
    fn the_net_caches_survive_a_clean_restart() {
        use dash_evo::types::ProTxHash;
        use dash_netaddr::netaddr::{NetAddr, Service};
        use dashd_masternode::{meta, netfulfilled};

        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            ..
        } = left_ibd_at_b1(dir.path());
        let peer = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 9999);
        let protx = ProTxHash::from([7; 32]);
        sut.steps.netfulfilled().unwrap().add_fulfilled_request(&peer, "full-sync");
        sut.steps.metaman().unwrap().add_used_masternode(&protx);
        sut.restart(Teardown::Clean);
        loaded(&sut);
        assert!(sut.steps.netfulfilled().unwrap().has_fulfilled_request(&peer, "full-sync"));
        assert!(sut.steps.metaman().unwrap().is_used_masternode(&protx));
        // The loaded chainstate's platform-ban applier holds step 6's manager, not one of its
        // own (BL-425): the resets reach the store this restart loaded and the next one stores.
        assert_eq!(Arc::strong_count(sut.steps.metaman().unwrap()), 2, "steps and the applier");

        sut.steps.metaman().unwrap().add_used_masternode(&ProTxHash::from([8; 32]));
        sut.restart(Teardown::Kill);
        loaded(&sut);
        assert_eq!(sut.steps.metaman().unwrap().used_masternodes_count(), 1);

        let regtest = dir.path().join("regtest");
        for (file, what) in
            [(netfulfilled::FILE_NAME, "fulfilled requests"), (meta::FILE_NAME, "masternode")]
        {
            let path = regtest.join(file);
            let good = std::fs::read(&path).unwrap();
            std::fs::write(&path, b"cut").unwrap();
            // A missing `netfulfilled.dat` loads (Core recreates it), so its destructor stores it
            // although the mncache load then fails.
            let fulfilled = regtest.join(netfulfilled::FILE_NAME);
            if file == meta::FILE_NAME {
                std::fs::remove_file(&fulfilled).unwrap();
            }
            sut.restart(Teardown::Clean);
            let error = format!("Failed to load {what} cache from {}", path.display());
            assert_eq!(sut.down(), Some(error.as_str()));
            assert_eq!(std::fs::read(&path).unwrap(), b"cut");
            assert!(fulfilled.is_file(), "stored after the mncache failure");
            std::fs::write(&path, good).unwrap();
            sut.restart(Teardown::Clean);
            loaded(&sut);
        }
    }

    /// BL-528: a reindex the block tree DB records (one that began and did not finish) is Core's
    /// `fReindex` at start, which replays every block file. rdashd cannot replay them yet, so the
    /// load is refused as `-reindex` is, before the caches: both files stay as they were, and so
    /// does the flag. Without the flag the same files load.
    // Core: node/blockstorage.cpp:415-418, :915-939; init.cpp:2273
    #[test]
    fn a_stored_reindex_flag_refuses_the_load() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            ..
        } = left_ibd_at_b1(dir.path());
        let files = populated_net_caches(&sut);
        let set_flag = |sut: &NodeSut, reindexing: bool| {
            let mut put = sut.coins.batch();
            dashd_chainstate::store::Meta::put_reindexing(&mut put, reindexing);
            sut.coins.write(put, Commit::Durable).unwrap();
        };
        set_flag(&sut, true);
        assert_eq!(sut.restart(Teardown::Kill), 0);
        assert_eq!(sut.down(), Some("-reindex is not supported by rdashd yet (CH5)."));
        assert!(sut.chain().is_none());
        for (path, bytes) in &files {
            assert_eq!(&std::fs::read(path).unwrap(), bytes, "{}", path.display());
        }
        assert_eq!(sut.coins.get(meta_cf(), b"reindexing").unwrap(), Some(vec![1]));

        set_flag(&sut, false);
        assert_eq!(sut.restart(Teardown::Kill), 0);
        loaded(&sut);
        assert!(holds_net_caches(&sut), "loaded");
    }

    /// The caches' decision takes step 7's `reindex` (Core's `fReindex` after the block index
    /// load), not `-reindex`, or `-reindex-chainstate`: either set, a node with a tip stores both
    /// empty; neither, it loads them. The support table refuses `-reindex-chainstate` at startup,
    /// so it is set on the arguments here.
    // Core: init.cpp:2273-2289
    #[test]
    fn the_net_caches_follow_step_7s_reindex() {
        use dashd_masternode::flatdb;
        use dashd_masternode::meta::MasternodeMetaStore;
        use dashd_masternode::netfulfilled::NetFulfilledStore;

        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            ..
        } = left_ibd_at_b1(dir.path());
        let ms = sut.params.chain.message_start;
        for (reindex, chainstate) in [(true, "0"), (false, "1")] {
            let case = format!("reindex {reindex}, -reindex-chainstate={chainstate}");
            let [(nf_path, _), (mn_path, _)] = populated_net_caches(&sut);
            sut.steps.reindex = reindex;
            sut.args.force_set_arg("-reindex-chainstate", chainstate);
            sut.steps.load_net_caches(&sut.args, &sut.params).unwrap();
            let nf_held = sut.steps.netfulfilled().unwrap().snapshot();
            assert_eq!(nf_held, NetFulfilledStore::default(), "{case}");
            let mn_held = sut.steps.metaman().unwrap().snapshot();
            assert_eq!(mn_held, MasternodeMetaStore::default(), "{case}");
            let nf = flatdb::decode::<NetFulfilledStore>(&std::fs::read(&nf_path).unwrap(), ms);
            assert_eq!(nf.unwrap(), NetFulfilledStore::default(), "{case}: cleared");
            let mn = flatdb::decode::<MasternodeMetaStore>(&std::fs::read(&mn_path).unwrap(), ms);
            assert_eq!(mn.unwrap(), MasternodeMetaStore::default(), "{case}: cleared");
        }

        populated_net_caches(&sut);
        sut.steps.reindex = false;
        sut.args.force_set_arg("-reindex-chainstate", "0");
        sut.steps.load_net_caches(&sut.args, &sut.params).unwrap();
        assert!(holds_net_caches(&sut), "loaded");
    }

    const CACHE_PEER: [u8; 4] = [10, 0, 0, 1];
    const CACHE_PROTX: [u8; 32] = [7; 32];

    /// Writes `netfulfilled.dat` and `mncache.dat` into `sut`'s data directory, each holding an
    /// entry live at the session's time, from managers of their own; returns each path and its
    /// bytes.
    fn populated_net_caches(sut: &NodeSut) -> [(std::path::PathBuf, Vec<u8>); 2] {
        use dash_evo::types::ProTxHash;
        use dash_netaddr::netaddr::{NetAddr, Service};
        use dashd_masternode::meta::{self, MasternodeMetaMan};
        use dashd_masternode::netfulfilled::{self, NetFulfilledRequestManager};

        let dir = sut.args.get_data_dir_net().unwrap();
        let ms = sut.params.chain.message_start;
        let (nf_path, mn_path) = (dir.join(netfulfilled::FILE_NAME), dir.join(meta::FILE_NAME));
        let fulfilled = NetFulfilledRequestManager::new(Arc::new(MockClock::at(MOCKTIME)), 300);
        let peer = Service::new(NetAddr::ipv4(CACHE_PEER), 9999);
        fulfilled.add_fulfilled_request(&peer, "full-sync");
        assert!(fulfilled.store(&nf_path, ms));
        let metaman = MasternodeMetaMan::new();
        metaman.add_used_masternode(&ProTxHash::from(CACHE_PROTX));
        assert!(metaman.store(&mn_path, ms));
        [nf_path, mn_path].map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
    }

    /// Whether `sut`'s managers hold [`populated_net_caches`]' entries.
    fn holds_net_caches(sut: &NodeSut) -> bool {
        use dash_evo::types::ProTxHash;
        use dash_netaddr::netaddr::{NetAddr, Service};

        let peer = Service::new(NetAddr::ipv4(CACHE_PEER), 9999);
        sut.steps.netfulfilled().unwrap().has_fulfilled_request(&peer, "full-sync")
            && sut.steps.metaman().unwrap().is_used_masternode(&ProTxHash::from(CACHE_PROTX))
    }

    /// A kill flushes nothing and stores no spork, so the next load has the last stored ones.
    #[test]
    fn a_kill_skips_the_flushes_and_the_spork_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &[SPORK_KEY]);
        let off = sut.sporks().unwrap().value(SPORK);
        sut.sporks().unwrap().update_spork(SPORK, off + 1).expect("signed");
        sut.clock().set_mocktime(MOCKTIME + 60);
        assert_eq!(sut.restart(Teardown::Kill), 0);
        loaded(&sut);
        assert_eq!(sut.sporks().unwrap().value(SPORK), off);
        assert_eq!(sut.clock().mocktime(), MOCKTIME + 60);
    }

    /// Every open reopens the session's coins, as a restarted process its files: a coin committed
    /// unsynced survives a kill (not a power loss), and recovery then refuses coins without a
    /// tip; the reopen also cleared the store's fail-stop latch, so the coin can be erased.
    // Core: node/chainstate.cpp:220-260
    #[test]
    fn every_open_reopens_the_same_coins() {
        let dir = tempfile::tempdir().unwrap();
        let (ui, lines) = logged_ui();
        let mut sut = try_sut(dir.path(), &[], ui).unwrap_or_else(|_| panic!("accepted"));
        let cf = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let mut put = sut.coins.batch();
        put.put(cf, b"coin", b"unspent");
        sut.coins.write(put, Commit::Buffered).unwrap();
        sut.coins.inject_failure(ErrorKind::NoSpace);
        assert_eq!(sut.restart(Teardown::Kill), 0);
        let down = sut.down().expect("refused");
        assert!(down.contains("no tip, but `coins` holds records"), "{down}");
        assert!(down.ends_with("Please restart with -reindex or -reindex-chainstate to recover."));
        // Core loads these coins, past `InitCoinsCache`: the refused load keeps both flushes
        // (BL-636), with no stored best block, so no timer pair.
        assert_eq!(sut.flushes(), 2, "the refused load's own flushes");
        assert_eq!(bl636::pairs(&lines.lock().unwrap()), (0, 0));

        let mut erase = sut.coins.batch();
        erase.delete(cf, b"coin");
        sut.coins.write(erase, Commit::Durable).expect("the reopen cleared the latch");
        assert_eq!(sut.restart(Teardown::Clean), 0);
        loaded(&sut);
    }

    /// Recovery runs on the steps' one subscriber hub, the one CH2b's flush control and W thread
    /// take at 2b-2 (b3 D44), and each open, a new process, has a new one.
    #[test]
    fn the_coins_recover_through_the_steps_hub() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &[]);
        loaded(&sut);
        let addr =
            |hub: &Arc<dashd_chainstate::flush::SubscriberHub>| Some(Arc::as_ptr(hub).addr());
        // Held, so the next open's hub cannot reuse its address.
        let first = Arc::clone(sut.steps.hub().expect("the coins opened"));
        assert_eq!(sut.steps.recovered_on(), addr(&first));
        sut.restart(Teardown::Clean);
        loaded(&sut);
        assert_eq!(sut.steps.recovered_on(), addr(sut.steps.hub().expect("the coins opened")));
        assert_ne!(sut.steps.recovered_on(), addr(&first), "a new process, a new hub");
    }

    /// Each open is a new process: an abort in one still flushes its teardown (DEC-241), and the
    /// next open's steps and V run on clear flags (a requested shutdown would refuse its load).
    // Core: init.cpp:379-385, :449-455
    #[test]
    fn each_open_has_its_own_shutdown_flags() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &[]);
        sut.shutdown.mark_aborted();
        sut.shutdown.start_shutdown();
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        let points = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&points);
        sut.steps.probe = Some(Box::new(move |p| sink.lock().unwrap().push(p)));
        assert_eq!(sut.restart(Teardown::Clean), 2);
        let not_aborted = || Point::Flush {
            aborted: false,
        };
        assert_eq!(*points.lock().unwrap(), [Point::VStopped, not_aborted(), not_aborted()]);
    }

    /// The node's log shows the session's mocktime (`LogTimestampStr`).
    #[test]
    fn the_log_runs_on_the_session_clock() {
        let dir = tempfile::tempdir().unwrap();
        let (ui, lines) = logged_ui();
        let mut sut =
            try_sut(dir.path(), &[], Arc::clone(&ui)).unwrap_or_else(|_| panic!("accepted"));
        sut.restart(Teardown::Clean);
        ui.logger().log_printf("probe\n");
        let lines = lines.lock().unwrap();
        let last = lines.last().unwrap();
        assert!(last.ends_with(" (mocktime: 1700000000) probe\n"), "{last}");
    }

    /// A failed read of the stored block index refuses the load with Core's init error, by its
    /// kind (2b-D14): a corrupt read is a corrupt index, any other is the store's, its cause
    /// logged as Core logs the exception. With the read healed the next start loads. That the
    /// scan leaves the guard's latch alone is `recovery`'s test (2b-D20).
    // Core: node/chainstate.cpp:180-198, txdb.cpp:300-344, init.cpp:2128-2131
    #[test]
    fn a_failed_index_read_refuses_the_load() {
        let dir = tempfile::tempdir().unwrap();
        let (ui, lines) = logged_ui();
        let mut sut =
            try_sut(dir.path(), &[], Arc::clone(&ui)).unwrap_or_else(|_| panic!("accepted"));
        for (kind, message, cause) in [
            (ErrorKind::Io, "Error opening block database", true),
            (ErrorKind::Corruption, "Error loading block database", false),
        ] {
            lines.lock().unwrap().clear();
            sut.scan_fault = Some(kind);
            assert_eq!(sut.restart(Teardown::Kill), 0);
            assert_eq!(sut.down(), Some(reindex_question(message).as_str()), "{kind:?}");
            assert!(sut.chain().is_none());
            if cause {
                let logged = lines.lock().unwrap().iter().any(|l| l.contains("injected"));
                assert!(logged, "{kind:?}: the cause");
            }
        }
        sut.scan_fault = None;
        assert_eq!(sut.restart(Teardown::Kill), 0);
        loaded(&sut);
    }

    /// A chainstate that cannot open and a corrupt index together: the open's error, where Core,
    /// which opens the coins only after the index loads, says the index's (2b-D18's first
    /// ordering; CH2b's steps 0-3 run before the index load).
    // Core: node/chainstate.cpp:180-198, :220-235
    #[test]
    fn a_failed_open_comes_before_a_corrupt_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &[]);
        sut.scan_fault = Some(ErrorKind::Corruption);
        sut.sync_fault = Some(ErrorKind::Io);
        assert_eq!(sut.restart(Teardown::Kill), 0);
        assert_eq!(sut.down(), Some(reindex_question("Error opening block database").as_str()));
        sut.sync_fault = None;
        assert_eq!(sut.restart(Teardown::Kill), 0);
        assert_eq!(sut.down(), Some(reindex_question("Error loading block database").as_str()));
    }

    /// A refused load runs the production shutdown and leaves the SUT down with Core's message,
    /// until a restart loads again; a teardown while down has nothing left to flush.
    #[test]
    fn a_refused_load_leaves_it_down() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &[SPORK_KEY]);
        assert_eq!(sut.restart(Teardown::Clean), 2);
        let path = dir.path().join("regtest").join(dashd_masternode::sporks::file::FILE_NAME);
        let stored = std::fs::read(&path).expect("stored at the clean stop");
        std::fs::write(&path, b"not a spork cache").unwrap();
        assert_eq!(sut.restart(Teardown::Kill), 0);
        let message = format!("Failed to load sporks cache from {}", path.display());
        assert_eq!(sut.down(), Some(message.as_str()));
        assert!(sut.chain().is_none() && sut.sporks().is_none());
        assert_eq!(sut.restart(Teardown::Kill), 0);
        assert_eq!(sut.down(), Some(message.as_str()));
        assert_eq!(std::fs::read(&path).unwrap(), b"not a spork cache", "not stored over");

        std::fs::write(&path, stored).unwrap();
        assert_eq!(sut.restart(Teardown::Clean), 0, "torn down at the load");
        loaded(&sut);
    }

    /// CH3a a10 m-1: each refused open keeps Core's status and raw message beside init's text;
    /// too small a dbcache and an incompatible database are distinct, and an interruption keeps
    /// VerifyDB's message.
    // Core: node/chainstate.cpp:177, :423-428, :445-446; init.cpp:2115-2137
    #[test]
    fn a_refused_open_keeps_core_s_status_and_raw_message() {
        use LoadStatus::{Failure, FailureIncompatibleDb, FailureInsufficientDbcache, Interrupted};
        let future = || OpenError::Recoverable {
            message: FUTURE_TIP.to_owned(),
            cause: None,
        };
        let shut = SHUTDOWN_REQUESTED.to_owned();
        let cases = [
            (future(), false, Failure, FUTURE_TIP, reindex_question(FUTURE_TIP)),
            (future(), true, Failure, FUTURE_TIP, shut.clone()),
            (
                OpenError::Incompatible("other".into()),
                false,
                FailureIncompatibleDb,
                "other",
                "other".into(),
            ),
            (
                OpenError::InsufficientDbcache(INSUFFICIENT_DBCACHE.into()),
                false,
                FailureInsufficientDbcache,
                INSUFFICIENT_DBCACHE,
                INSUFFICIENT_DBCACHE.into(),
            ),
            (
                OpenError::Interrupted(VERIFY_INTERRUPTED.into()),
                true,
                Interrupted,
                VERIFY_INTERRUPTED,
                shut.clone(),
            ),
            (OpenError::Interrupted(String::new()), true, Interrupted, "", shut),
        ];
        for (error, shutdown, status, raw, text) in cases {
            let what = format!("{error:?}");
            let refusal = open_refusal(error, shutdown);
            let result = LoadResult {
                status,
                message: raw.to_owned(),
            };
            assert_eq!(
                (refusal.result, refusal.message, refusal.crashed),
                (result, text, false),
                "{what}"
            );
        }
        assert!(open_refusal(OpenError::Crashed, false).crashed);
        let names = [
            LoadStatus::Success,
            LoadStatus::Failure,
            LoadStatus::FailureFatal,
            LoadStatus::FailureIncompatibleDb,
            LoadStatus::FailureInsufficientDbcache,
            LoadStatus::Interrupted,
            LoadStatus::InitError,
            LoadStatus::ActivateFailed,
        ]
        .map(LoadStatus::name);
        assert_eq!(
            names,
            [
                "success",
                "failure",
                "failure_fatal",
                "failure_incompatible_db",
                "failure_insufficient_dbcache",
                "interrupted",
                "init_error",
                "activate_failed"
            ]
        );
    }

    /// m-1: a spork key init cannot sign with refuses the load as `init_error`, the shim's
    /// status for the spork setup's refusals, with init's message as the raw one.
    #[test]
    fn a_bad_spork_key_is_an_init_error() {
        let dir = tempfile::tempdir().unwrap();
        let sut = try_sut(dir.path(), &["-sporkkey=notakey"], ui()).expect("refused at the load");
        let down = sut.down().expect("down").to_owned();
        assert_eq!(
            sut.load_result(),
            &LoadResult {
                status: LoadStatus::InitError,
                message: down,
            }
        );
        assert!(sut.chain().is_none());
    }

    /// CH3a a10: a restore is a kill (no flush, no spork store), the data directory rewritten to
    /// exactly the image's files and the coins to its data, then a load: the saved tip, coins,
    /// storage and sporks come back, a file made after the image is gone, the block file is the
    /// image's again, and the clock is the restore's.
    // Core: oracle_chain.cpp `oracle_chain_save`, `oracle_chain_restore` (sh:1293-1328)
    #[test]
    fn a_restore_reopens_exactly_the_saved_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &[SPORK_KEY]);
        imported(&mut sut);
        let p = Arc::new(sut.params.chain.clone());
        let genesis = dashd_chainstate::chain::genesis_block(&p);
        let b1 = regtest_block(&p, genesis.header(), 1, genesis.header().time() + 1);
        submit(&sut, &b1);
        let spork = sut.sporks().unwrap().value(SPORK);
        let image = sut.save_image().expect("saved");
        loaded(&sut);
        let saved = sut.restart_report(Teardown::Clean).persisted.expect("read");
        assert_eq!(saved.meta.tip, Some(b1.hash()), "save's clean teardown stored b1");
        let net = dir.path().join("regtest");
        let blk = net.join("blocks").join("blk00000.dat");
        let saved_blk = std::fs::read(&blk).unwrap();
        let utxo = sut.tip_coins().unwrap().unwrap();
        // After the image: a block and a spork, both stored; a stray rev file; an unstored spork.
        imported(&mut sut);
        let b2 = regtest_block(&p, b1.header(), 2, genesis.header().time() + 2);
        submit(&sut, &b2);
        sut.sporks().unwrap().update_spork(SPORK, spork + 1).expect("signed");
        assert_eq!(sut.restart(Teardown::Clean), 2);
        let stray = net.join("blocks").join("rev00001.dat");
        std::fs::write(&stray, b"after the image").unwrap();
        sut.sporks().unwrap().update_spork(SPORK, spork + 2).expect("signed");

        let restored = sut.restore(&image, MOCKTIME + 7).expect("restored");
        assert_eq!(restored.load, LoadResult::success());
        assert_eq!(restored.persisted.expect("read"), saved);
        loaded(&sut);
        assert_eq!(sut.flushes(), 0);
        assert_eq!(sut.coins_tip(), Some(b1.hash()));
        assert_eq!(sut.tip_coins().unwrap().unwrap(), utxo);
        assert_eq!(sut.sporks().unwrap().value(SPORK), spork, "the image's spork file");
        assert!(!stray.exists(), "made after the image");
        assert_eq!(std::fs::read(&blk).unwrap(), saved_blk);
        assert_eq!(sut.clock().mocktime(), MOCKTIME + 7);
    }

    /// An image's error path (Sol r1): a restore whose rewrite fails returns the filesystem's
    /// error, and the SUT is down with it, nothing loaded, its load result `image_files` with the
    /// same message (Opus r1 n-5). The rewrite fails on an image file under a path the image
    /// also holds as a file.
    #[test]
    fn a_failed_restore_rewrite_leaves_the_sut_down() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &[]);
        loaded(&sut);
        let mut image = sut.save_image().expect("saved");
        loaded(&sut);
        let (file, _) = image.files.first().expect("a file").clone();
        image.files.push((file.join("under-a-file"), Vec::new()));
        let error = sut.restore(&image, MOCKTIME).expect_err("the rewrite fails");
        let message = format!("the data directory: {error}");
        assert_eq!(sut.down(), Some(message.as_str()));
        let image_files = LoadResult {
            status: LoadStatus::ImageFiles,
            message,
        };
        assert_eq!(sut.load_result(), &image_files);
        assert!(sut.chain().is_none(), "nothing loaded");
    }

    /// Records each `ChainStateFlushed`'s locator tip.
    #[derive(Default)]
    struct Flushed(Mutex<Vec<BlockHash>>);

    impl ValidationInterface for Flushed {
        fn chain_state_flushed(&self, locator: &[BlockHash]) {
            self.0
                .lock()
                .unwrap()
                .push(locator.first().copied().unwrap_or(BlockHash::from([0; 32])));
        }
    }

    /// Gives every later V an `L` of its own; the ones made so far, in open order.
    fn listener_per_open<L: ValidationInterface + Default + 'static>(
        sut: &mut NodeSut,
    ) -> Arc<Mutex<Vec<Arc<L>>>> {
        let made = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&made);
        sut.set_listener(Some(Arc::new(move |_: &dash_chain::ChainReader| {
            let listener = Arc::new(L::default());
            sink.lock().unwrap().push(Arc::clone(&listener));
            listener as Arc<dyn ValidationInterface>
        })));
        made
    }

    /// M-2 and m-1: a stored tip from the future refuses the load in `VerifyLoadedChainstate`,
    /// after V started. The listener that V got as it started hears the refused load's own
    /// teardown: the first shutdown flush's `ChainStateFlushed`, as Core's EventLog, registered
    /// before `LoadChainstate`. The restart reports `failure` with Core's text (no reindex
    /// question) and the storage it found; a restart at a good clock loads again.
    // Core: node/chainstate.cpp:423-428; init.cpp:379-385; oracle_chain.cpp (sh:764-768, :821)
    #[test]
    fn a_future_tip_refusal_is_heard_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        let made = listener_per_open::<Flushed>(&mut sut);
        // Leg A's regime from the next open: Production's first flush of each open carries
        // LEFT_IBD (a10-2's tally below).
        sut.harness.regime = FlushRegime::CoreCausesOnly;
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        let tip_time = i64::from(b2.header().time());
        sut.clock().set_mocktime(tip_time - MAX_FUTURE_BLOCK_TIME - 1);
        assert!(sut.settle().is_some());
        sut.begin_op();
        let refused = sut.restart_report(Teardown::Clean);
        // CH3a a10-2 (M-7's witness that a refusal with V is counted): the teardown's two flushes,
        // then the refused load's own, each a completed W release.
        assert_eq!(sut.tally().delta(), full_always(4));
        assert_eq!(sut.tally().since_open(), full_always(2), "the refused load's own");
        assert_eq!(
            refused.load,
            LoadResult {
                status: LoadStatus::Failure,
                message: FUTURE_TIP.to_owned(),
            }
        );
        assert_eq!(sut.load_result(), &refused.load);
        assert_eq!(sut.down(), Some(reindex_question(FUTURE_TIP).as_str()));
        assert_eq!(refused.persisted.expect("read").meta.tip, Some(b2.hash()));
        {
            let made = made.lock().unwrap();
            assert_eq!(made.len(), 2, "one listener per V");
            assert_eq!(*made[1].0.lock().unwrap(), [b2.hash()], "the refused load's teardown");
        }
        sut.clock().set_mocktime(tip_time);
        assert_eq!(sut.restart_report(Teardown::Clean).load, LoadResult::success());
        loaded(&sut);
        assert_eq!(made.lock().unwrap().len(), 3);
    }

    /// `n` full `ALWAYS` flushes and nothing else, as the tally books them.
    fn full_always(n: u64) -> CauseCounts {
        CauseCounts {
            full: [("ALWAYS".to_owned(), n)].into(),
            index_only: Default::default(),
        }
    }

    /// CH3a a10-2 C2: the tally books the jobs W completed. A CLEAN restart's two shutdown
    /// flushes are two full `ALWAYS`, in the op's delta, and the load's `since_open` starts
    /// empty; a KILL and a restore (a kill, the image, the load) book none. The attempt counter
    /// is unchanged.
    // Core: init.cpp:379-385, :449-455; src/test/oracle/oracle_chain.cpp:862-878, :763 at the pin
    #[test]
    fn the_tally_books_a_clean_restart_s_two_flushes_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            ..
        } = left_ibd_at_b1(dir.path());
        assert!(sut.settle().is_some());
        sut.begin_op();
        assert_eq!(sut.tally().delta(), CauseCounts::default(), "an op starts empty");
        let clean = sut.restart_report(Teardown::Clean);
        assert_eq!(clean.load, LoadResult::success());
        assert_eq!(sut.tally().delta(), full_always(2), "CLEAN");
        assert_eq!(sut.tally().since_open(), CauseCounts::default(), "the new load's");
        assert!(sut.settle().is_some());
        let image = sut.save_image().expect("saved");
        assert!(sut.settle().is_some());
        for (what, how) in [("KILL", Some(Teardown::Kill)), ("restore", None)] {
            sut.begin_op();
            let load = match how {
                Some(how) => sut.restart_report(how).load,
                None => sut.restore(&image, MOCKTIME).expect("restored").load,
            };
            assert_eq!(load, LoadResult::success(), "{what}");
            assert!(sut.settle().is_some());
            assert_eq!(sut.tally().delta(), CauseCounts::default(), "{what}");
            assert_eq!(sut.tally().since_open(), CauseCounts::default(), "{what}");
        }
        assert_eq!(sut.flushes(), 0, "the attempts since the last load");
    }

    /// Records each `BlockConnected`'s block.
    #[derive(Default)]
    struct Connected(Mutex<Vec<BlockHash>>);

    impl ValidationInterface for Connected {
        fn block_connected(&self, block: &Arc<dash_consensus::Block>, _: dash_consensus::BlockId) {
            self.0.lock().unwrap().push(block.hash());
        }
    }

    /// M-2: the listener a V gets as it starts hears that load's own `ActivateBestChain`. A
    /// tip invalidated and flushed is reconsidered by the load's `ResetBlockFailureFlags(nullptr)`
    /// (D32), so after a clean restart the startup ABC connects it again, and the new V's
    /// listener hears the connect.
    // Core: node/chainstate.cpp:464; validation.cpp:4284-4304 (rc.5 shim tree)
    #[test]
    fn the_listener_hears_the_load_s_activate_best_chain() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        let v = sut.chain().unwrap();
        assert_eq!(v.invalidate_block(b2.hash()).recv().ok(), Some(Ok(())));
        assert!(v.flush(dashd_chainstate::chain::FlushStateMode::Always).recv().is_ok());
        let made = listener_per_open::<Connected>(&mut sut);
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        imported(&mut sut);
        assert!(sut.settle().is_some_and(|quiet| quiet.is_quiet()));
        assert_eq!(sut.chain().unwrap().tip_watch().get(), Some(b2.hash()));
        let made = made.lock().unwrap();
        assert_eq!(made.len(), 1);
        assert_eq!(*made[0].0.lock().unwrap(), [b2.hash()]);
    }

    /// A startup ABC that fails is the load's `activate_failed`, decided once the import has
    /// ended, as the shim's `Open()` runs that ABC inline. b2 is invalidated and stored, then its
    /// block unreadable: the load's `ResetBlockFailureFlags` lets the ABC try b2 again, its read
    /// fails (V's `AbortNode`), then `ThreadImport`'s "Failed to connect best block", the
    /// report the status takes its message from. With the byte restored the next load connects
    /// b2.
    // Core: src/test/oracle/oracle_chain.cpp:873-883 at the shim's ABI 33; init.cpp:2455-2458
    #[test]
    fn a_failed_startup_activation_is_activate_failed() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        let v = sut.chain().unwrap();
        assert_eq!(v.invalidate_block(b2.hash()).recv().ok(), Some(Ok(())));
        assert!(v.flush(dashd_chainstate::chain::FlushStateMode::Always).recv().is_ok());
        let (at, was) = garble_tx_count(dir.path(), &b2);
        let restarted = sut.restart_report(Teardown::Kill);
        // The storage before the load (O2b D37): b2 failed, as the refusal's shutdown flushes
        // would no longer leave it once the load reset its flags.
        let stored = restarted.persisted.expect("read");
        let b2_status = stored.index.iter().find(|(hash, _)| *hash == b2.hash()).unwrap().1.status;
        assert!(b2_status.has(dash_chain::Status::FAILED_VALID), "{b2_status:?}");
        let load = restarted.load;
        assert_eq!(load.status, LoadStatus::ActivateFailed, "{load:?}");
        assert!(load.message.starts_with(ACTIVATE_FAILED), "{load:?}");
        assert_eq!(sut.down(), Some(SHUTDOWN_REQUESTED));
        set_blk0_byte(dir.path(), at, was);
        assert_eq!(sut.restart_report(Teardown::Kill).load, LoadResult::success());
        assert_eq!(view_tip(&sut), b2.hash());
    }

    /// V's flush site reads IBD without latching, as Core's `FlushStateToDisk` reads it only to
    /// prune: a flush on a tip the clock has made recent, outside loading, leaves the latch
    /// open, so a later clock is IBD again; the next signal-side read latches it.
    // Core: validation.cpp:3076 at the pin; :1970-1993 (the latch)
    #[test]
    fn a_flush_does_not_latch_ibd() {
        let dir = tempfile::tempdir().unwrap();
        let sut = sut(dir.path(), &[]);
        loaded(&sut);
        let p = Arc::new(sut.params.chain.clone());
        let genesis = dashd_chainstate::chain::genesis_block(&p);
        let b1_time = genesis.header().time() + 1;
        let b1 = regtest_block(&p, genesis.header(), 1, b1_time);
        submit(&sut, &b1);
        let v = sut.chain().unwrap();
        assert_eq!(v.tip_watch().get(), Some(b1.hash()));
        let reader = v.reader().clone();
        let ibd = |t: u32| {
            let now = dash_clock::NodeSeconds(i64::from(t));
            reader.is_initial_block_download(&reader.snapshot(), now, false)
        };
        let (recent, old) = (b1_time + 60, b1_time + 3 * 24 * 60 * 60);
        assert!(ibd(old), "b1 connected in IBD");
        sut.clock().set_mocktime(i64::from(recent));
        let always = dashd_chainstate::chain::FlushStateMode::Always;
        assert!(v.flush(always).recv().is_ok());
        assert!(ibd(old), "the flush left the latch open");
        assert!(!ibd(recent), "a recent tip leaves IBD, and the read latches");
        assert!(!ibd(old), "latched");
    }

    /// C3b (m-4): each open builds a new flush policy, so a reopened V leaves IBD again, once:
    /// under `CoreCausesOnly` the mask removes one LEFT_IBD per V, a reopened one's too. The
    /// import's flushes run while loading (IBD), so an `IF_NEEDED` flush after it reaches the
    /// site out of IBD.
    #[test]
    fn a_reopen_re_arms_left_ibd() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            ..
        } = left_ibd_at_b1(dir.path());
        sut.harness.regime = FlushRegime::CoreCausesOnly;
        for how in [Teardown::Clean, Teardown::Kill, Teardown::Clean] {
            sut.restart(how);
            loaded(&sut);
            let if_needed = dashd_chainstate::chain::FlushStateMode::IfNeeded;
            assert!(sut.chain().unwrap().flush(if_needed).recv().is_ok());
            let quiet = sut.settle().expect("V serves");
            assert_eq!(quiet.suppressed.left_ibd, 1, "{how:?}: {quiet:?}");
        }
    }

    /// The constructor runs production's parameter interaction: `-prune` soft-sets `-txindex=0`
    /// and `-disablegovernance=1`, without which the app-init checks refuse the argv (`-prune`
    /// with `-txindex`, and with governance), so the pruned SUT loads.
    // Core: init.cpp:1084-1090, :1248-1260
    #[test]
    fn the_argv_runs_production_parameter_interaction() {
        let dir = tempfile::tempdir().unwrap();
        let sut = sut(dir.path(), &["-prune=550"]);
        assert_eq!(sut.args.get_bool_arg_opt("-txindex").unwrap(), Some(false));
        assert_eq!(sut.args.get_bool_arg_opt("-disablegovernance").unwrap(), Some(true));
        loaded(&sut);
    }

    /// Arguments init refuses are the constructor's error, before any load; so is a
    /// `-mocktime`, which the constructor sets.
    #[test]
    fn refused_arguments_load_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let refusals =
            [&["-nosuchoption"][..], &["-testnet"], &["-mocktime=5"], &["-blockfilterindex=x"]];
        for argv in refusals {
            let refused = try_sut(dir.path(), argv, ui());
            assert!(refused.is_err(), "{argv:?}");
            let genesis = dir.path().join("regtest").join("blocks").join("blk00000.dat");
            assert!(!genesis.exists(), "{argv:?}");
        }
    }

    /// A regtest block on `prev` at `height`: a coinbase alone, ground to the regtest target.
    fn regtest_block(
        p: &dash_params::ChainSetup,
        prev: &dash_consensus::primitives::Header,
        height: u8,
        time: u32,
    ) -> Arc<dash_consensus::Block> {
        regtest_block_paying(p, prev, height, time, 0)
    }

    /// [`regtest_block`] whose coinbase pays `value` duffs.
    fn regtest_block_paying(
        p: &dash_params::ChainSetup,
        prev: &dash_consensus::primitives::Header,
        height: u8,
        time: u32,
        value: i64,
    ) -> Arc<dash_consensus::Block> {
        mine(p, prev, u32::from(height), time, &[(value, &[0x51])], vec![])
    }

    /// A regtest block on `prev` at `height`: a coinbase paying `outputs` (value, script), then
    /// `txs`, ground to the regtest target.
    fn mine(
        p: &dash_params::ChainSetup,
        prev: &dash_consensus::primitives::Header,
        height: u32,
        time: u32,
        outputs: &[(i64, &[u8])],
        txs: Vec<Arc<Tx>>,
    ) -> Arc<dash_consensus::Block> {
        use dash_consensus::Block;
        use dash_consensus::primitives::Header;

        // BIP34's `CScript() << nHeight`, then four bytes.
        let mut script_sig = match height {
            1..=16 => vec![0x50 + height as u8],
            _ => {
                let mut n = height.to_le_bytes().to_vec();
                while n.last() == Some(&0) {
                    n.pop();
                }
                if n.last().is_some_and(|b| b & 0x80 != 0) {
                    n.push(0);
                }
                [vec![n.len() as u8], n].concat()
            }
        };
        script_sig.extend([4, 0, 0, 0, 0]);
        let coinbase = tx(
            vec![TxIn {
                prevout: OutPoint {
                    hash: TxHash::from([0; 32]),
                    index: u32::MAX,
                },
                script_sig: script_sig.into(),
                sequence: u32::MAX,
            }],
            outputs,
        );
        let txs = [vec![coinbase], txs].concat();
        let mut raw = regtest_header(prev.hash(), time);
        raw.merkle_root = Block::new(Header::new(raw), txs.clone()).merkle().0;
        Arc::new(Block::new(grind(p, raw), txs))
    }

    /// A regtest block on `prev` at `height` past DIP0003: a coinbase paying nothing, made
    /// DIP0003's (`nVersion` 3, `nType` 5) with a version-2 CbTx payload (`evo/cbtx.h`; regtest's
    /// DIP0008 is active from height 1, `CheckCbTx`): the height, and null MN-list and quorum
    /// roots, an empty list's and no commitments', ground to the regtest target.
    fn cbtx_block(
        p: &dash_params::ChainSetup,
        prev: &dash_consensus::primitives::Header,
        height: u32,
        time: u32,
    ) -> Arc<dash_consensus::Block> {
        use dash_consensus::Block;
        use dash_consensus::primitives::Header;

        let plain = mine(p, prev, height, time, &[(0, &[0x51])], vec![]);
        let mut cb = plain.txs()[0].to_mutable();
        cb.version = 3;
        cb.tx_type = TxType::new(5);
        cb.extra_payload = [&2u16.to_le_bytes()[..], &height.to_le_bytes(), &[0; 64]].concat();
        let txs = vec![Arc::new(Tx::new(cb))];
        let mut raw = regtest_header(prev.hash(), time);
        raw.merkle_root = Block::new(Header::new(raw), txs.clone()).merkle().0;
        Arc::new(Block::new(grind(p, raw), txs))
    }

    /// A version-1 transaction with no lock time, paying `outputs` (value, script).
    fn tx(inputs: Vec<TxIn>, outputs: &[(i64, &[u8])]) -> Arc<Tx> {
        Arc::new(Tx::new(Transaction {
            version: 1,
            tx_type: TxType::new(0),
            inputs,
            outputs: outputs
                .iter()
                .map(|&(value, script)| TxOut {
                    value,
                    script_pubkey: script.to_vec().into(),
                })
                .collect(),
            lock_time: 0,
            extra_payload: Vec::new(),
        }))
    }

    /// A regtest header on `prev` at `time`, its merkle root and nonce zero.
    fn regtest_header(prev: BlockHash, time: u32) -> dash_primitives::BlockHeader {
        dash_primitives::BlockHeader {
            version: 0x2000_0000,
            prev_hash: prev,
            time,
            bits: 0x207f_ffff,
            ..Default::default()
        }
    }

    /// Sends V `headers` in batches of 2,000, each accepted whole.
    fn send_headers(sut: &NodeSut, headers: &[dash_consensus::primitives::Header]) {
        let v = sut.chain().unwrap();
        for chunk in headers.chunks(2_000) {
            let batch = dash_chain::HeaderBatch {
                hashes: chunk.iter().map(|h| h.hash()).collect(),
                headers: chunk.to_vec(),
            };
            let r = v.process_headers(batch, None).recv().expect("V replies");
            assert!(r.stop.is_none(), "{:?}", r.stop);
        }
    }

    /// `raw` with the first nonce that meets the regtest target.
    fn grind(
        p: &dash_params::ChainSetup,
        raw: dash_primitives::BlockHeader,
    ) -> dash_consensus::primitives::Header {
        use dash_consensus::header::HeaderParams;
        use dash_consensus::pow::check_proof_of_work;
        use dash_consensus::primitives::Header;
        use dash_num::CompactTarget;

        let pow = *HeaderParams::from_chain_params(p).unwrap().pow();
        (0..)
            .map(|nonce| {
                Header::new(dash_primitives::BlockHeader {
                    nonce,
                    ..raw
                })
            })
            .find(|h| check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(raw.bits), &pow))
            .unwrap()
    }

    /// DEC-279's chain for the RPC commands: V's active chain at each height, published by the
    /// time V replies to the block (Core's `cs_main` order), and nothing after `detach`.
    #[test]
    fn the_rpc_chain_reads_the_active_chain_at_each_height() {
        use dashd_rpc::blockchain::NodeChain as _;

        let dir = tempfile::tempdir().unwrap();
        let sut = sut(dir.path(), &[]);
        let p = sut.params.chain.clone();
        let genesis = dashd_chainstate::chain::genesis_block(&Arc::new(p.clone()));
        let slot = crate::rpc::ChainSlot::default();
        assert!(slot.published().is_none(), "before step 7");
        let chain = sut.chain().unwrap();
        slot.attach(chain, sut.steps.chain().unwrap().signals(), None, None, None);
        let time = u32::try_from(MOCKTIME).unwrap() - 300;
        let mut blocks = vec![genesis];
        for h in 1..=2u8 {
            let b = regtest_block(&p, blocks.last().unwrap().header(), h, time + u32::from(h));
            let r = chain.process_block(Arc::clone(&b), true, None, true).recv();
            assert!(r.is_ok_and(|r| r.accepted), "accepted");
            let active = slot.published().unwrap();
            assert_eq!(active.height(), i32::from(h), "published before the reply");
            blocks.push(b);
        }
        let active = slot.published().unwrap();
        for (h, b) in blocks.iter().enumerate() {
            assert_eq!(active.block_hash(i32::try_from(h).unwrap()), b.hash(), "height {h}");
        }
        slot.detach();
        assert!(slot.published().is_none(), "after shutdown");
    }

    /// DEC-279's `submitblock` on V: the index status it reads first, then `ProcessNewBlock`
    /// with its catcher on the node's signals. A new block is caught valid; one stored on a side
    /// chain is caught nowhere (`inconclusive`); one `ConnectBlock` rejects is caught with its
    /// reason, then indexed as failed (`duplicate-invalid`).
    // Core: rpc/mining.cpp:1036-1063
    #[test]
    fn the_rpc_chainman_processes_a_block_as_submitblock_reads_it() {
        use dashd_rpc::blockchain::{IndexStatus, NodeChain as _, ProcessedBlock};
        use dashd_rpc::mining::{StateCatcher, ValidationState};

        let dir = tempfile::tempdir().unwrap();
        let sut = sut(dir.path(), &[]);
        let p = sut.params.chain.clone();
        let genesis = dashd_chainstate::chain::genesis_block(&Arc::new(p.clone()));
        let slot = crate::rpc::ChainSlot::default();
        assert!(slot.chainman().is_none(), "before step 7");
        slot.attach(sut.chain().unwrap(), sut.steps.chain().unwrap().signals(), None, None, None);
        let chainman = slot.chainman().unwrap();
        let status = |valid_scripts, failed| {
            Some(IndexStatus {
                valid_scripts,
                failed,
            })
        };
        let processed = |accepted, new_block| ProcessedBlock {
            accepted,
            new_block,
        };
        let process = |b: &Arc<dash_consensus::Block>| {
            let sc = Arc::new(StateCatcher::new(b.hash()));
            (chainman.process_new_block(Arc::clone(b), true, &sc), sc.state())
        };
        // `ConnectBlock` returns for the genesis block before it raises `BLOCK_VALID_SCRIPTS`
        // (validation.cpp:2536, :2900), so Core's genesis answers `duplicate` from
        // `ProcessNewBlock`, as stored and not new.
        assert_eq!(chainman.lookup_block_index(&genesis.hash()), status(false, false));
        assert_eq!(process(&genesis), (processed(true, false), None));

        let time = u32::try_from(MOCKTIME).unwrap() - 300;
        let b1 = regtest_block(&p, genesis.header(), 1, time + 1);
        assert_eq!(chainman.lookup_block_index(&b1.hash()), None);
        assert_eq!(process(&b1), (processed(true, true), Some(ValidationState::Valid)));
        assert_eq!(chainman.lookup_block_index(&b1.hash()), status(true, false));
        // Past the lookup, as a racing duplicate is: accepted, not written, nothing checked. The
        // catcher is unregistered after the call: a later check of b1 doesn't reach it.
        let sc = Arc::new(StateCatcher::new(b1.hash()));
        assert_eq!(chainman.process_new_block(Arc::clone(&b1), true, &sc), processed(true, false));
        let late = dashd_chainstate::chain::BlockState::Error("late".into());
        sut.steps.chain().unwrap().signals().block_checked(&b1, &late, false);
        assert_eq!(sc.state(), None, "nothing checked, and unregistered");

        let side = regtest_block(&p, genesis.header(), 1, time + 2);
        assert_eq!(process(&side), (processed(true, true), None), "stored, not connected");
        assert_eq!(chainman.lookup_block_index(&side.hash()), status(false, false));

        let overpaid = regtest_block_paying(&p, b1.header(), 2, time + 3, 100_000 * 100_000_000);
        let (outcome, state) = process(&overpaid);
        assert_eq!(outcome, processed(true, true), "accepted, then failed in ConnectBlock");
        let Some(ValidationState::Invalid(e)) = state else {
            panic!("caught invalid: {state:?}");
        };
        assert_eq!(e.reject_reason(), "bad-cb-amount");
        assert_eq!(chainman.lookup_block_index(&overpaid.hash()), status(false, true));
        slot.detach();
        assert!(slot.chainman().is_none(), "after shutdown");
    }

    /// A regtest session in `dir` whose V connected b1 and b2 (recent blocks), with V's once-only
    /// LEFT_IBD flush at b1 stored: the coins' stored tip is b1, and b2 is dirty in V only. Its
    /// log lines are collected. LEFT_IBD is ours (A1 §3.4): Core v24 latches IBD and flushes
    /// nothing as it leaves it (validation.cpp:1990), so this is the one flush a test can drive
    /// (NodeFlush's timers are `Instant`, 2b-D25).
    fn left_ibd_at_b1(dir: &Path) -> LeftIbd {
        left_ibd_at(dir, 1)
    }

    /// [`left_ibd_at_b1`] with the flush at block `at` (1 or 2): b1 is an old block when `at` is
    /// 2, so V leaves IBD at b2 and both blocks are stored.
    fn left_ibd_at(dir: &Path, at: u8) -> LeftIbd {
        left_ibd_with(dir, at, &["-debug=all"])
    }

    /// [`left_ibd_at`]'s session started with `argv` in place of `-debug=all`.
    fn left_ibd_with(dir: &Path, at: u8, argv: &[&str]) -> LeftIbd {
        let (ui, lines) = logged_ui();
        let mut sut = try_sut(dir, argv, ui).unwrap_or_else(|_| panic!("accepted"));
        let p = Arc::new(sut.params.chain.clone());
        let genesis = dashd_chainstate::chain::genesis_block(&p);
        let recent = u32::try_from(MOCKTIME).unwrap() - 300;
        let b1_time = if at == 1 {
            recent
        } else {
            genesis.header().time() + 1
        };
        let b1 = regtest_block(&p, genesis.header(), 1, b1_time);
        let b2 = regtest_block(&p, b1.header(), 2, recent + 150);
        // Init's wait ends at genesis, before `loadblk` clears `fImporting`: a block that V
        // connects while it is set is still initial block download, and LEFT_IBD would come at b2.
        imported(&mut sut);
        submit(&sut, &b1);
        submit(&sut, &b2);
        assert_eq!(sut.chain().unwrap().tip_watch().get(), Some(b2.hash()));
        // The write thread lands after the reply; Core writes the index, then the coins, so wait
        // for the coins' stored tip.
        let flushed = if at == 1 {
            b1.hash()
        } else {
            b2.hash()
        };
        stored_tip_reaches(&sut, &flushed, "LEFT_IBD's flush landed");
        LeftIbd {
            sut,
            lines,
            b1,
            b2,
        }
    }

    /// A ui whose log lines the test reads.
    fn logged_ui() -> (Arc<Ui>, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let logger = Arc::new(Logger::new());
        let sink = Arc::clone(&lines);
        logger.push_back_callback(move |s| sink.lock().unwrap().push(s.to_owned()));
        assert!(logger.start_logging());
        (Arc::new(Ui::with_stderr(logger, Box::new(|_| {}))), lines)
    }

    /// [`left_ibd_at_b1`]'s session, its log lines and its two blocks.
    struct LeftIbd {
        sut: NodeSut,
        lines: Arc<Mutex<Vec<String>>>,
        b1: Arc<dash_consensus::Block>,
        b2: Arc<dash_consensus::Block>,
    }

    fn submit(sut: &NodeSut, b: &Arc<dash_consensus::Block>) {
        let r = sut.chain().unwrap().process_block(Arc::clone(b), true, None, true).recv();
        assert!(r.is_ok_and(|r| r.accepted), "accepted");
    }

    /// xcheck's positive control in the node (O9b; CH3a D189's connector-matrix case, here
    /// through `-consensusxcheck` and the node's own coins setup). Planted L1 answers a read of
    /// input 1 with input 0's coin, so a spend of 30 and 20 paying out 55 reads 60 in. Unplanted
    /// it is `CheckTxInputs`' `bad-txns-in-belowout`; planted with `-noconsensusxcheck` the
    /// invalid block connects; planted with the flag (soft-set on in `NodeSut`) V stops on the
    /// `consensusxcheck` Fatal through `AbortNode`, with the block not connected.
    #[cfg(feature = "planted-bugs")]
    #[test]
    fn consensusxcheck_stops_the_node_on_a_planted_memo() {
        const COIN: i64 = 100_000_000;
        // A session (with `planted` armed from its second open) and the block that spends
        // coinbases 1 and 2, which it has not been sent yet.
        let session = |dir: &Path, argv: &[&str], planted| {
            let (ui, lines) = logged_ui();
            let mut sut = try_sut(dir, argv, ui).unwrap_or_else(|_| panic!("accepted"));
            sut.planted = planted;
            assert_eq!(sut.restart(Teardown::Clean), 2);
            loaded(&sut);
            imported(&mut sut);
            let p = Arc::new(sut.params.chain.clone());
            let time = u32::try_from(MOCKTIME).unwrap() - 3600;
            let mut tip = dashd_chainstate::chain::genesis_block(&p);
            let mut paid = Vec::new();
            for h in 1..=102u32 {
                let value = match h {
                    1 => 30 * COIN,
                    2 => 20 * COIN,
                    _ => 0,
                };
                let b = mine(&p, tip.header(), h, time + h, &[(value, &[0x51])], vec![]);
                submit(&sut, &b);
                if h <= 2 {
                    paid.push(b.txs()[0].txid());
                }
                tip = b;
            }
            let input = |hash| TxIn {
                prevout: OutPoint {
                    hash,
                    index: 0,
                },
                script_sig: Vec::new().into(),
                sequence: u32::MAX,
            };
            let spend = tx(paid.into_iter().map(input).collect(), &[(55 * COIN, &[0x51])]);
            let block = mine(&p, tip.header(), 103, time + 103, &[(0, &[0x51])], vec![spend]);
            (sut, lines, tip, block)
        };
        let fatal = "consensusxcheck: the memo's coin for";

        let dir = tempfile::tempdir().unwrap();
        let (sut, _, tip, block) = session(dir.path(), &[], None);
        let checked = Arc::new(Checked::default());
        assert!(sut.register_listener(Arc::clone(&checked) as _));
        submit(&sut, &block); // accepted, then invalid at connect
        assert_eq!(sut.chain().unwrap().tip_watch().get(), Some(tip.hash()));
        assert!(sut.sync_signals());
        let reasons = checked.0.lock().unwrap().clone();
        assert_eq!(reasons, [(block.hash(), "bad-txns-in-belowout".to_owned())]);
        assert!(!sut.shutdown.aborted());

        let dir = tempfile::tempdir().unwrap();
        let planted = Some(dashd_chainstate::Planted::L1);
        let (sut, lines, _, block) = session(dir.path(), &["-noconsensusxcheck"], planted);
        submit(&sut, &block);
        assert_eq!(sut.chain().unwrap().tip_watch().get(), Some(block.hash()), "connected");
        assert!(!sut.shutdown.aborted());
        assert!(!lines.lock().unwrap().iter().any(|l| l.contains(fatal)));

        let dir = tempfile::tempdir().unwrap();
        let (mut sut, lines, tip, block) = session(dir.path(), &[], planted);
        // V's flush at 102: the reopen below comes back there, whatever the shutdown writes.
        let always = dashd_chainstate::flush::FlushStateMode::Always;
        sut.chain().unwrap().flush(always).recv().expect("flushed");
        let _ = sut.chain().unwrap().process_block(Arc::clone(&block), true, None, true).recv();
        wait_for("AbortNode ran", || sut.shutdown.aborted());
        let logged = lines.lock().unwrap().clone();
        assert!(logged.iter().any(|l| l.contains(&format!("*** {fatal}"))), "{logged:?}");
        assert_ne!(sut.chain().unwrap().tip_watch().get(), Some(block.hash()));
        // Nothing of the block committed: after the abort's teardown (its flushes run, DEC-241)
        // and a reopen, unplanted (the block is then invalid, and V keeps running), the store and
        // the node are at 102, the coins the block spends are unspent and its output is not a
        // coin. Until BL-271 the shutdown flushes are the WAL-sync stub, so this holds what
        // reached the store; with BL-271 it holds the shutdown's flush of the coins cache too
        // (O9b D4).
        sut.planted = None;
        sut.restart(Teardown::Clean);
        loaded(&sut);
        // The teardown's flush found nothing of the block in the coins cache: with BL-271 a cache
        // at the block would make the shutdown flush a mislabelled freeze, a second AbortNode
        // (CH1-node's `check_pair`, O9b D4). The logger survives `open()`.
        let logged = lines.lock().unwrap().clone();
        assert_eq!(logged.iter().filter(|l| l.contains("*** ")).count(), 1, "{logged:?}");
        assert!(!logged.iter().any(|l| l.contains("a freeze at")), "{logged:?}");
        assert_ne!(stored_tip(&sut), Some(block.hash()), "the store's tip");
        assert_eq!(sut.coins_tip(), Some(tip.hash()), "the reopened coins' tip");
        assert_ne!(sut.chain().unwrap().tip_watch().get(), Some(block.hash()), "V's tip");
        let coins = sut.tip_coins().expect("V runs").unwrap();
        let has = |o: &OutPoint| coins.iter().any(|(k, _)| *k == OutPointKey::new(o));
        let spend = &block.txs()[1];
        let made = OutPoint {
            hash: spend.txid(),
            index: 0,
        };
        assert!(!has(&made), "the block's output is not a coin");
        for input in spend.inputs() {
            assert!(has(&input.prevout), "a coin the block spends is unspent");
        }
    }

    /// Set in [`consensusxcheck_reaches_the_coins_cache_prefetch_hits`]'s child process.
    const PREFETCH_CHILD: &str = "O9B_PREFETCH_XCHECK_CHILD";

    /// Admits a prefetch batch holding `coin` at `key` on V, as a producer's results are
    /// admitted (the node has none until CH4), and fetches `key`.
    fn prefetched(
        sut: &NodeSut,
        key: OutPointKey,
        coin: PackedCoin,
    ) -> Result<Option<PackedCoin>, CoinsReadError> {
        use dashd_chainstate::coins::PrefetchBatch;
        let cell = Arc::clone(sut.steps.coins_cell().expect("the coins attached"));
        let read = move |_: &ChainQuery<'_>| {
            let mut cache = cell.try_lock().expect("V's leaf, taken on V between commands");
            let batch = PrefetchBatch {
                fetch_gen: cache.released_gen(),
                // Read after every write so far.
                epoch: u64::MAX,
                entries: vec![(key, coin)],
            };
            assert_eq!(cache.admit_prefetch(batch, 0), 1, "admitted");
            cache.fetch(&key).map(|c| c.map(PackedCoin::from_coin))
        };
        sut.chain().unwrap().query(read).recv().unwrap()
    }

    /// The coins cache's prefetch-hit xcheck in the node (O9b; CH2's `CoinsCache::set_xcheck`,
    /// through `-consensusxcheck` and the node's coins setup), with a prefetched coin the
    /// database does not hold. With `-noconsensusxcheck` it is returned as found. Under the flag
    /// (soft-set on in `NodeSut`) its hit is a corrupt read, which Core's read-error callback
    /// ends in `std::abort()` (`CoreAbortSink::read_failed`), so that case runs in a child
    /// process (the test binary, running only this test), which must die of SIGABRT with Core's
    /// message and the xcheck's cause.
    #[cfg(unix)]
    #[test]
    fn consensusxcheck_reaches_the_coins_cache_prefetch_hits() {
        use std::os::unix::process::ExitStatusExt as _;
        let key = OutPointKey::new(&OutPoint {
            hash: TxHash::from([7; 32]),
            index: 3,
        });
        let out = TxOut {
            value: 5,
            script_pubkey: vec![0x51].into(),
        };
        let coin = PackedCoin::from_output(&out, 1, false).unwrap();

        // The parent makes the child's datadir and removes it: the child's abort skips every drop.
        if let Some(datadir) = std::env::var_os(PREFETCH_CHILD).map(std::path::PathBuf::from) {
            // The log's lines to stderr, written through before the abort.
            let logger = Arc::new(Logger::new());
            logger.push_back_callback(|s| {
                use std::io::Write as _;
                let _ = std::io::stderr().write_all(s.as_bytes());
            });
            assert!(logger.start_logging());
            let ui = Arc::new(Ui::with_stderr(logger, Box::new(|_| {})));
            let sut = try_sut(&datadir, &[], ui).unwrap_or_else(|_| panic!("accepted"));
            loaded(&sut);
            let read = prefetched(&sut, key, coin);
            panic!("returned: {read:?}");
        }

        let dir = tempfile::tempdir().unwrap();
        let sut = sut(dir.path(), &["-noconsensusxcheck"]);
        loaded(&sut);
        assert_eq!(prefetched(&sut, key, coin.clone()).unwrap(), Some(coin), "returned as found");
        assert!(!sut.shutdown.aborted());

        let child_dir = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("sh")
            .args(["-c", r#"ulimit -c 0; exec "$0" "$@""#])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "sut::tests::consensusxcheck_reaches_the_coins_cache_prefetch_hits"])
            .args(["--nocapture", "--test-threads=1"])
            .env(PREFETCH_CHILD, child_dir.path())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.signal(), Some(6), "{:?}, not SIGABRT: {stderr}", out.status);
        assert!(stderr.contains("Error: Error reading from database, shutting down."), "{stderr}");
        let cause = "consensusxcheck: the prefetched coin differs from the database's";
        assert!(stderr.contains(cause), "{stderr}");
    }

    /// The reject reason of each invalid block V checked (`BlockChecked`).
    #[derive(Default)]
    struct Checked(Mutex<Vec<(BlockHash, String)>>);

    impl ValidationInterface for Checked {
        fn block_checked(
            &self,
            block: &dash_consensus::Block,
            state: &dashd_chainstate::chain::BlockState,
            _: bool,
        ) {
            if let dashd_chainstate::chain::BlockState::Invalid(e) = state {
                self.0.lock().unwrap().push((block.hash(), e.reject_reason().to_owned()));
            }
        }
    }

    /// Polls `cond` every 20 ms for up to a minute.
    fn wait_for(what: &str, cond: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !cond() {
            assert!(start.elapsed().as_secs() < 60, "{what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The coins' tip in the store's `meta`.
    fn stored_tip(sut: &NodeSut) -> Option<BlockHash> {
        // `store::meta::TIP` (private to the store).
        stored_meta_hash(sut, b"tip")
    }

    fn stored_meta_hash(sut: &NodeSut, key: &[u8]) -> Option<BlockHash> {
        let tip = sut.coins.get(meta_cf(), key).unwrap()?;
        Some(BlockHash::from(<[u8; 32]>::try_from(tip.as_slice()).ok()?))
    }

    /// Waits for the coins' stored tip to reach `at`.
    fn stored_tip_reaches(sut: &NodeSut, at: &BlockHash, what: &str) {
        let start = std::time::Instant::now();
        while stored_tip(sut) != Some(*at) {
            assert!(start.elapsed().as_secs() < 60, "{what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn meta_cf() -> Cf {
        CHAINSTATE_SCHEMA.cf("meta").unwrap()
    }

    /// `hash`'s stored block index record.
    fn stored_record(sut: &NodeSut, hash: &BlockHash) -> dash_chain::BlockIndexRecord {
        let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        let bytes = sut.coins.get(cf, hash.as_bytes()).unwrap().expect("stored");
        dash_chain::BlockIndexRecord::decode(&bytes).unwrap()
    }

    /// Blocks V connects through the node's own stores persist into blk files (M2:
    /// `NodeBlockFiles`) and the chainstate's `block_index`, at V's own flush (M1: `NodeFlush` on
    /// W's thread) and at shutdown's (BL-271): the next process loads that stored index (BL-127's
    /// file check passing), recovers the coins at its tip, and keeps it. A clean stop comes back
    /// at the tip it stopped on, as Core's shutdown flushes write it (`ForceFlushStateToDisk`);
    /// [`a_killed_node_comes_back_at_v_s_last_flush`] is its twin.
    // Core: node/chainstate.cpp:182-260, :272-277; init.cpp:379-385, :449-456
    #[test]
    fn a_restart_keeps_its_tip_through_the_node_stores() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            lines,
            b1,
            b2,
        } = left_ibd_at_b1(dir.path());
        lines.lock().unwrap().clear();
        let flushes = sut.restart(Teardown::Clean);
        assert!(!lines.lock().unwrap().iter().any(|l| l.contains("Error")), "{lines:?}");
        assert_eq!(flushes, 2);
        loaded(&sut);
        // b1's blk record and stored position survived: the block reads back from them.
        let record = stored_record(&sut, &b1.hash());
        let blk = format!("regtest/blocks/blk{:05}.dat", record.file.expect("HAVE_DATA"));
        let raw = std::fs::read(dir.path().join(blk)).unwrap();
        let at = usize::try_from(record.data_pos.expect("HAVE_DATA")).unwrap();
        let read = dash_consensus::decode_block(&mut &raw[at..]).unwrap().into_value();
        assert_eq!(read.hash(), b1.hash(), "b1 read back at its stored position");
        assert_eq!(sut.steps.coins_tip(), Some(b2.hash()), "the coins' tip, shutdown's flush's");
        assert_eq!(view_tip(&sut), b2.hash(), "V's tip, from the stored index (LoadChainTip)");
    }

    /// Shutdown's first flush writes V's state, and its `ChainStateFlushed` reaches the listeners,
    /// before the second flush starts: there b2's index entry is stored and the listener has
    /// seen b2, so a kill between the two comes back at b2, as Core's does. The second flush's
    /// signal is dropped (2b-D3). The listener counts b2's signals only: V's flush at b1 may
    /// still be queued as it registers.
    // Core: init.cpp:379-390, :449-456, :484-485
    #[test]
    fn the_first_shutdown_flush_stores_the_tip_and_signals_it() {
        #[derive(Default)]
        struct Flushed(BlockHash, AtomicU32);
        impl ValidationInterface for Flushed {
            fn chain_state_flushed(&self, locator: &[BlockHash]) {
                if locator[0] == self.0 {
                    self.1.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        let b2_hash = b2.hash();
        let flushed = Arc::new(Flushed(b2_hash, AtomicU32::new(0)));
        sut.steps.chain().unwrap().signals().register(Arc::clone(&flushed) as _);
        let at_flushes = Arc::new(Mutex::new(Vec::new()));
        let (record, seen, coins) =
            (Arc::clone(&at_flushes), Arc::clone(&flushed), sut.coins.clone());
        let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        sut.steps.probe = Some(Box::new(move |point| {
            if matches!(point, Point::Flush { .. }) {
                let stored = coins.get(cf, b2_hash.as_bytes()).unwrap().is_some();
                record.lock().unwrap().push((stored, seen.1.load(Ordering::SeqCst)));
            }
        }));
        assert_eq!(sut.restart(Teardown::Clean), 2);
        assert_eq!(*at_flushes.lock().unwrap(), [(false, 0), (true, 1)]);
        assert_eq!(flushed.1.load(Ordering::SeqCst), 1, "the second flush's signal dropped");
    }

    /// `lines` hold, in order, `times` reports of the injected write: Core's `HandleError` lines
    /// (the engine's text, then the `-debug=leveldb` hint; BL-485), W's `AbortNode` and user
    /// error, and the flush's failure line (Core's `state.ToString()` is the `AbortNode`'s
    /// message). They are the shutdown's only such lines.
    // Core: dbwrapper.cpp:261-268; validation.cpp:3121-3123, :3132; shutdown.cpp:27-37
    fn assert_flush_failures(lines: &Mutex<Vec<String>>, times: usize) {
        let report = flush_failure_for(INJECTED);
        let lines = lines.lock().unwrap();
        let got = failure_lines(&lines, &report);
        let want: Vec<&str> = report.iter().map(String::as_str).cycle().take(5 * times).collect();
        assert_eq!(got, want, "{lines:?}");
    }

    /// The injected write's engine text.
    const INJECTED: &str = "memory error (Io): injected write";

    /// What a write W fails with `engine_text` logs, in Core's order: `HandleError`'s two lines,
    /// then the `AbortNode`'s log line and user error.
    fn write_failure_for(engine_text: &str) -> [String; 4] {
        [
            format!("{engine_text}\n"),
            "You can use -debug=leveldb to get more complete diagnostic messages\n".to_owned(),
            format!("*** System error while flushing: {engine_text}\n"),
            "Error: A fatal internal error occurred, see debug.log for details\n".to_owned(),
        ]
    }

    fn write_failure() -> [String; 4] {
        write_failure_for(INJECTED)
    }

    /// [`write_failure_for`] and the flush's failure line, which carries the `AbortNode`'s message.
    fn flush_failure_for(engine_text: &str) -> Vec<String> {
        let failed = format!(
            "ForceFlushStateToDisk: failed to flush state (System error while flushing: \
             {engine_text})\n"
        );
        [&write_failure_for(engine_text)[..], &[failed]].concat()
    }

    /// What a flush that finds no free disk space logs: W's `AbortNode` and user error, then the
    /// flush's failure line.
    // Core: validation.cpp:3061-3063, :3128-3134; shutdown.cpp:27-38
    fn space_report() -> [String; 3] {
        [
            "*** Disk space is too low!\n".to_owned(),
            "Error: Disk space is too low!\n".to_owned(),
            "ForceFlushStateToDisk: failed to flush state (Disk space is too low!)\n".to_owned(),
        ]
    }

    /// Every line of `lines` about a write failure of `engine_text` or an `AbortNode`, each as the
    /// line of `report` it ends with (the longest: the `***` line ends with the engine's text too),
    /// or whole.
    fn failure_lines_for<'a>(
        engine_text: &str,
        lines: &'a [String],
        report: &'a [String],
    ) -> Vec<&'a str> {
        let kinds = [engine_text, "-debug=leveldb", "*** ", "Error: ", "failed to flush state"];
        (lines.iter())
            .filter(|l| kinds.iter().any(|k| l.contains(k)))
            .map(|l| {
                let ends = report.iter().filter(|w| l.ends_with(w.as_str()));
                ends.max_by_key(|w| w.len()).map_or(l.as_str(), |w| w)
            })
            .collect()
    }

    fn failure_lines<'a>(lines: &'a [String], report: &'a [String]) -> Vec<&'a str> {
        failure_lines_for(INJECTED, lines, report)
    }

    /// BL-485: a write W fails outside shutdown (V's ALWAYS flush on unflushed state) logs Core's
    /// `HandleError` lines before W's `AbortNode`, as the shutdown flushes' failures do.
    // Core: dbwrapper.cpp:261-268; validation.cpp:3121-3123; shutdown.cpp:27-37
    #[test]
    fn a_failed_write_in_a_flush_logs_cores_handle_error_lines() {
        let dir = tempfile::tempdir().unwrap();
        let left = left_ibd_at_b1(dir.path());
        failed_always_flush(&left);
        let report = write_failure();
        let lines = left.lines.lock().unwrap();
        assert_eq!(
            failure_lines(&lines, &report),
            report.each_ref().map(String::as_str),
            "{lines:?}"
        );
        // The flush's whole log under `-debug=all` (BL-668), each completed line up to its
        // duration. One batch carries the index, the evo window and the coins, so the evodb pair
        // (CH7 D20, D42) and the coins pair sit inside the index pair; each pair closes after
        // `HandleError`'s lines and before the `AbortNode`, as Core's `Timer`s do while the
        // exception unwinds.
        // Core: validation.cpp:3065-3079, :3090-3111, :3127-3129; dbwrapper.cpp:266-267;
        // logging/timer.h:42-49; shutdown.cpp:30-34
        let bench = |what: &str| {
            [
                format!("[bench] FlushStateToDisk: {what} started"),
                format!("[bench] FlushStateToDisk: {what} completed ("),
            ]
        };
        let [files_s, files_c] = bench("write block and undo data to disk");
        let [index_s, index_c] = bench("write block index to disk");
        let [coins_s, coins_c] = bench("write coins cache to disk (1 coins, 15915kB)");
        let evodb = "FlushStateToDisk: write evodb cache to disk";
        let [he1, he2, abort, user] = write_failure().map(|l| l.trim_end().to_owned());
        let want = [
            files_s,
            files_c,
            index_s,
            format!("{evodb} started"),
            coins_s,
            "[coindb] Writing final batch of 0.00 MiB".to_owned(),
            he1,
            he2,
            coins_c,
            format!("{evodb} completed ("),
            index_c,
            abort,
            user,
        ];
        assert_flush_window(&lines, &want);
    }

    /// Fails the write of an ALWAYS flush in `left`'s session and waits for the `AbortNode`; the
    /// session's log then holds only that flush's lines.
    fn failed_always_flush(left: &LeftIbd) {
        left.lines.lock().unwrap().clear();
        left.sut.write_fault.store(true, Ordering::SeqCst);
        let always = dashd_chainstate::flush::FlushStateMode::Always;
        let _ = left.sut.chain().unwrap().flush(always).recv();
        wait_for("AbortNode ran", || left.sut.shutdown.aborted());
        left.sut.write_fault.store(false, Ordering::SeqCst);
    }

    /// The failed flush's log in `lines`, as [`bl636::flush_log_for`] cuts it, from its last
    /// `want[0]` to the user error after it, is `want`.
    fn assert_flush_window(lines: &[String], want: &[String]) {
        let log = bl636::flush_log_for(INJECTED, lines);
        let at = log.iter().rposition(|l| *l == want[0]).expect("the window's first line");
        let len = log[at..].iter().position(|l| l.starts_with("Error: ")).expect("the user error");
        assert_eq!(log[at..=at + len], *want, "{log:?}");
    }

    /// At the default level, the failed flush's log is Core's `HandleError` lines, the evodb pair
    /// around them and the report, with no bench or COINDB line (BL-668).
    // Core: logging/timer.h:55-59, :103-108; dbwrapper.cpp:266-267; shutdown.cpp:30-34
    #[test]
    fn a_failed_write_in_a_flush_logs_no_bench_line_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let left = left_ibd_with(dir.path(), 1, &[]);
        failed_always_flush(&left);
        let report = write_failure();
        let lines = left.lines.lock().unwrap();
        assert_eq!(
            failure_lines(&lines, &report),
            report.each_ref().map(String::as_str),
            "{lines:?}"
        );
        let debug = ["[bench]", "[coindb]"];
        assert!(!lines.iter().any(|l| debug.iter().any(|d| l.contains(d))), "{lines:?}");
        let evodb = "FlushStateToDisk: write evodb cache to disk";
        let [he1, he2, abort, user] = write_failure().map(|l| l.trim_end().to_owned());
        let want =
            [format!("{evodb} started"), he1, he2, format!("{evodb} completed ("), abort, user];
        assert_flush_window(&lines, &want);
    }

    /// BL-484: V's `AbortNode` while init verifies the loaded chainstate (here D64's: init's
    /// `VerifyLoaded` is V's second) leaves the store open, and the load fails. The teardown's two
    /// flushes run on the state V handed back as it stopped (BL-271, DEC-241), each writing the
    /// block index as Core's do after an `AbortNode`, not only syncing the WAL.
    // Core: init.cpp:2101-2112, :379-385, :449-455; validation.cpp:3060-3075
    #[test]
    fn an_abort_node_while_verifying_flushes_twice_on_vs_state() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            lines,
            ..
        } = left_ibd_at_b1(dir.path());
        sut.before_verify = Some(|chain| {
            let empty = dashd_chainstate::chain::VerifyLoadOptions {
                coins_empty: true,
                ..Default::default()
            };
            assert!(matches!(chain.verify_loaded(empty), Ok(Ok(()))), "V's first VerifyLoaded");
        });
        lines.lock().unwrap().clear();
        sut.restart(Teardown::Clean);
        assert_eq!(sut.down(), Some(SHUTDOWN_REQUESTED));
        assert!(sut.shutdown.aborted());
        let lines = lines.lock().unwrap();
        let abort =
            "*** The loaded chainstate was verified twice, or after a class C operation (D64)\n";
        let at = lines.iter().position(|l| l.ends_with(abort));
        let at = at.unwrap_or_else(|| panic!("V's AbortNode: {lines:?}"));
        let index = "FlushStateToDisk: write block index to disk started";
        let written = lines[at..].iter().filter(|l| l.contains(index)).count();
        assert_eq!(written, 2, "both flushes write V's state: {lines:?}");
        assert!(!lines.iter().any(|l| l.contains("failed to flush state")), "{lines:?}");
    }

    /// A write that fails in the first shutdown flush on V's state: W's `AbortNode`, then the
    /// flush's failure line with that `AbortNode`'s message (Core's `state.ToString()`), not the
    /// store's text. The failure is latched, so the second flush reports it as DEC-253's, a second
    /// `AbortNode` with the same text, as LevelDB's kept write error does in Core.
    // Core: validation.cpp:2245-2249, :3121-3132; leveldb/db/db_impl.cc:1229-1244
    #[test]
    fn a_failed_write_in_the_first_shutdown_flush_reports_cores_line() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            lines,
            ..
        } = left_ibd_at_b1(dir.path());
        lines.lock().unwrap().clear();
        sut.write_fault.store(true, Ordering::SeqCst);
        sut.steps.shutdown();
        sut.write_fault.store(false, Ordering::SeqCst);
        assert!(sut.shutdown.aborted());
        assert_flush_failures(&lines, 2);
    }

    /// BL-482: no disk space at the first shutdown flush on V's state. W's space check fails it
    /// with Core's `AbortNode`, its user message, and the flush's failure line. Core's second flush
    /// runs the same check and fails the same way, so the second repeats all three lines, not the
    /// failure form of a store's kept error (DEC-253's).
    // Core: validation.cpp:3056-3058, :2245-2249, :3128-3134; shutdown.cpp:27-38
    #[test]
    fn low_disk_space_fails_both_shutdown_flushes_as_cores_does() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            lines,
            ..
        } = left_ibd_at_b1(dir.path());
        lines.lock().unwrap().clear();
        sut.low_space.store(true, Ordering::SeqCst);
        sut.steps.shutdown();
        sut.low_space.store(false, Ordering::SeqCst);
        assert!(sut.shutdown.aborted());
        let report = space_report();
        let lines = lines.lock().unwrap();
        // Every `AbortNode`, user error and failure line of the shutdown, in order.
        let got = failure_lines(&lines, &report);
        let want: Vec<&str> = report.iter().map(String::as_str).cycle().take(6).collect();
        assert_eq!(got, want, "{lines:?}");
        let database = ["System error while flushing", "-debug=leveldb", "fatal internal error"];
        assert!(!lines.iter().any(|l| database.iter().any(|d| l.contains(d))), "{lines:?}");
    }

    /// BL-482 r1 m-2: the second flush repeats an `AbortNode` only for the flush's own failure, a
    /// latch at the flush's site holding that `AbortNode`'s text. The same text at a store's site,
    /// or another text at the flush's, is not one.
    #[test]
    fn only_the_flush_s_own_failure_is_latched_by_the_flush() {
        let low = "Disk space is too low!";
        for (site, message, query, want) in [
            ("chainstate read", low, low, false),
            ("chainstate flush", "A", "B", false),
            ("chainstate flush", low, low, true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let sut = sut(dir.path(), &[]);
            loaded(&sut);
            let chainstate = sut.steps.chainstate();
            assert!(!chainstate.latched_by_flush(low), "not latched");
            chainstate.trip_latch(site, message);
            assert_eq!(chainstate.latched_by_flush(query), want, "{site}: {message} ({query})");
        }
    }

    /// A write that fails in the second shutdown flush only, the first having stored V's state:
    /// the second flush also runs on V's state, so it fails as Core's does, with W's `AbortNode`,
    /// then its failure line, and the node exits aborted. Nothing had aborted before it.
    // Core: init.cpp:449-455; validation.cpp:2245-2249, :3121-3132
    #[test]
    fn a_failed_write_in_the_second_shutdown_flush_reports_cores_line() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            lines,
            ..
        } = left_ibd_at_b1(dir.path());
        lines.lock().unwrap().clear();
        let aborted_at = Arc::new(Mutex::new(Vec::new()));
        let (record, fault) = (Arc::clone(&aborted_at), Arc::clone(&sut.write_fault));
        sut.steps.probe = Some(Box::new(move |point| {
            if let Point::Flush {
                aborted,
            } = point
            {
                let mut seen = record.lock().unwrap();
                seen.push(aborted);
                // The second flush only.
                fault.store(seen.len() == 2, Ordering::SeqCst);
            }
        }));
        sut.steps.shutdown();
        sut.write_fault.store(false, Ordering::SeqCst);
        assert_eq!(*aborted_at.lock().unwrap(), [false, false]);
        assert!(sut.shutdown.aborted(), "exit 1");
        assert_flush_failures(&lines, 1);
    }

    /// DEC-241: after an `AbortNode` the shutdown flushes still write V's state, so the restart
    /// comes back at the tip the node stopped on, as Core's does.
    // Core: shutdown.cpp:27-37; init.cpp:379-385, :449-456
    #[test]
    fn an_aborted_node_comes_back_at_the_tip_it_stopped_on() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        sut.shutdown.mark_aborted();
        sut.shutdown.start_shutdown();
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        assert_eq!(sut.steps.coins_tip(), Some(b2.hash()));
        assert_eq!(view_tip(&sut), b2.hash());
    }

    fn view_tip(sut: &NodeSut) -> BlockHash {
        sut.chain().unwrap().view_handle().view().snap.tip.tip_hash
    }

    /// A killed node runs no shutdown flush: it comes back at V's last flush, b1 (LEFT_IBD's),
    /// whose stored entry and blk record take b2 again, connecting on the flushed coins.
    #[test]
    fn a_killed_node_comes_back_at_v_s_last_flush() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b1,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        assert_eq!(sut.restart(Teardown::Kill), 0);
        loaded(&sut);
        assert_eq!(sut.steps.coins_tip(), Some(b1.hash()), "the coins' tip, V's last flush's");
        assert_eq!(view_tip(&sut), b1.hash());
        submit(&sut, &b2);
        assert_eq!(view_tip(&sut), b2.hash());
    }

    /// The evo state's stored best block, the raw `meta.evo_tip`.
    fn stored_evo_tip(sut: &NodeSut) -> Option<BlockHash> {
        // `store::meta::EVO_TIP`.
        stored_meta_hash(sut, b"evo_tip")
    }

    /// Waits for the coins' stored tip and the raw `meta.evo_tip` to reach `at`: the freeze that
    /// stores them checks that the evo state's best block is V's tip.
    fn flushed_at(sut: &NodeSut, at: &BlockHash, what: &str) {
        stored_tip_reaches(sut, at, &format!("{what}: the flush landed"));
        assert_eq!(stored_evo_tip(sut), Some(*at), "{what}: the evo tip is the coins' tip");
        assert_eq!(sut.down(), None, "{what}");
    }

    /// Waits for `loadblk`'s import to end: joins the thread, which sends V `SetLoading(false)` and
    /// then waits for V to serve `InitializeCurrentBlockTip`, so V has cleared `fImporting`. Init's
    /// wait ends at genesis, before `loadblk` clears it, and V sets it only when it serves the
    /// import's `SetLoading(true)`, so the flag can read clear before the import has begun: a block
    /// connected then is still initial block download (BL-616).
    fn imported(sut: &mut NodeSut) {
        assert!(sut.steps.join_import(), "loadblk ends");
        assert!(!sut.chain().unwrap().view_handle().loading_blocks(), "the import ended");
    }

    /// CH7 §6.4 and D37 on the node path, with `NodeEvo` below DIP0003. The raw `meta.evo_tip`
    /// the layer resumes from is empty on a new datadir, the coins' tip after a crash, and erased
    /// by a wipe that recovery finishes; each process's first freeze, its LEFT_IBD flush, stores
    /// the evo tip with the coins' tip, and the run ends where an uncrashed one does. Those
    /// freezes follow an evo commit, so they read the committed tip, not the resumed one (D51).
    #[test]
    fn the_evo_tip_resumes_from_the_meta_tip_after_a_crash_and_a_wipe() {
        const DAY: i64 = 24 * 60 * 60;
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b1,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        let p = Arc::new(sut.params.chain.clone());
        let time = |t: i64| u32::try_from(t).unwrap();
        let b3 = regtest_block(&p, b2.header(), 3, time(MOCKTIME + DAY - 300));
        let b4 = regtest_block(&p, b3.header(), 4, time(MOCKTIME + 2 * DAY - 300));
        let b5 = regtest_block(&p, b4.header(), 5, time(MOCKTIME + 2 * DAY - 200));
        let view_tip = |sut: &NodeSut| sut.chain().unwrap().view_handle().view().snap.tip.tip_hash;
        // A new datadir: the first freeze after open, at b1.
        flushed_at(&sut, &b1.hash(), "open");
        // A crash a day later: the layer resumes at b1; V leaves IBD at b3, and the first freeze
        // after the recovery stores it.
        sut.clock.set_mocktime(MOCKTIME + DAY);
        assert_eq!(sut.restart(Teardown::Kill), 0);
        loaded(&sut);
        assert_eq!(view_tip(&sut), b1.hash());
        assert_eq!(stored_evo_tip(&sut), Some(b1.hash()), "the raw tip the layer resumes from");
        imported(&mut sut);
        submit(&sut, &b2);
        submit(&sut, &b3);
        flushed_at(&sut, &b3.hash(), "recovery");
        // `-reindex-chainstate`'s wipe interrupted after its marker, a day later: the next start
        // finishes it, erasing both tips, and V reconnects from its block files on an empty layer.
        let mut put = sut.coins.batch();
        put.put(meta_cf(), b"wiping", &[1]);
        sut.coins.write(put, Commit::Durable).unwrap();
        sut.clock.set_mocktime(MOCKTIME + 2 * DAY);
        assert_eq!(sut.restart(Teardown::Kill), 0);
        loaded(&sut);
        assert_eq!((stored_tip(&sut), stored_evo_tip(&sut)), (None, None), "wiped");
        imported(&mut sut);
        assert_eq!(view_tip(&sut), b3.hash(), "reconnected from the block files");
        submit(&sut, &b4);
        flushed_at(&sut, &b4.hash(), "wipe");
        submit(&sut, &b5);
        // The same blocks, two days on, with no crash and no wipe.
        let other = tempfile::tempdir().unwrap();
        let uncrashed = left_ibd_at_b1(other.path()).sut;
        uncrashed.clock.set_mocktime(MOCKTIME + 2 * DAY);
        for b in [&b2, &b3, &b4, &b5] {
            submit(&uncrashed, b);
        }
        assert_eq!(view_tip(&uncrashed), b5.hash());
        assert_eq!(view_tip(&sut), b5.hash());
        assert_eq!(sut.steps.coins_tip(), uncrashed.steps.coins_tip());
    }

    /// 2b-D6's `LoadChainTip` row: a stored coins tip the stored index marks failed, with no valid
    /// block of at least its work, is `LoadChainTip`'s INVALID_TIP. Init says Core's "Error
    /// initializing block database" with the reindex question, after logging the cause, and the
    /// load's teardown runs both flushes: the coins cache exists by then. At every `-checklevel`:
    /// Core's `LoadChainTip` prune leaves no candidate there and asserts (rc.5 `:3603`) before
    /// VerifyDB, so neither node reaches init's reset (DEC-992).
    ///
    /// CH3a a10-1's rerun (Q5, OD-L3-10, M-7) rides on this row since CH7b-2 removed G1q
    /// (OD-L3-44, OD-L3-47): each later `RESTART` on the same arguments, of either kind, is
    /// refused the same way, with the refusal's two shutdown flushes (with no attach they are
    /// BL-636's bare flushes, each logging the evodb timer pair: the ruled extension of the
    /// DEC-196(1) refusal, not Core's output; they run no V flush job, so the a10-2 tally books
    /// none), no V, and the storage as it was; a down SUT's restart runs no teardown of its own
    /// (`sh:943`). Under G1q that rerun was
    /// `failure_incompatible_db` with no reindex question; here it is `failure` with the question.
    /// A node-only witness, as the G1q one was: Core asserts on this storage instead of failing.
    // Core: node/chainstate.cpp:272-277; validation.cpp:4992-5018, :2969 (CanFlushToDisk), rc.5 :3595-3604
    #[test]
    fn an_invalid_stored_tip_fails_load_chain_tip_after_the_coins_at_each_restart() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            lines,
            b1,
            ..
        } = left_ibd_at_b1(dir.path());
        // As an index written after `invalidateblock`, the coins before it.
        let mut record = stored_record(&sut, &b1.hash());
        record.status = dash_chain::Status(record.status.bits() | dash_chain::Status::FAILED_VALID);
        let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        let mut put = sut.coins.batch();
        put.put(cf, b1.hash().as_bytes(), &record.to_bytes());
        sut.coins.write(put, Commit::Durable).unwrap();
        lines.lock().unwrap().clear();
        assert_eq!(sut.restart(Teardown::Clean), 2);
        let message = reindex_question(dashd_chainstate::chain::INIT_BLOCK_DB_FAILED);
        assert_eq!(sut.down(), Some(message.as_str()));
        assert!(sut.chain().is_none());
        assert_eq!(sut.flushes(), 2, "after the coins cache");
        let cause = dashd_chainstate::chain::INVALID_TIP;
        assert!(lines.lock().unwrap().iter().any(|l| l.contains(cause)), "{lines:?}");
        let stored = sut.persisted().expect("read");
        for how in [Teardown::Kill, Teardown::Clean, Teardown::Kill] {
            lines.lock().unwrap().clear();
            sut.begin_op();
            let restarted = sut.restart_report(how);
            let load = restarted.load;
            assert_eq!(load.status, LoadStatus::Failure, "{how:?}");
            assert_eq!(load.message, dashd_chainstate::chain::INIT_BLOCK_DB_FAILED, "{how:?}");
            assert_eq!(sut.down(), Some(message.as_str()), "{how:?}: the reindex question");
            assert!(lines.lock().unwrap().iter().any(|l| l.contains(cause)), "{how:?}: {lines:?}");
            assert_eq!(restarted.persisted.expect("read"), stored, "{how:?}: nothing stored");
            assert!(sut.chain().is_none(), "{how:?}: no V");
            assert_eq!(sut.flushes(), 2, "{how:?}: the refusal's shutdown flushes");
            let pairs = bl636::pairs(&lines.lock().unwrap());
            assert_eq!(pairs, (2, 2), "{how:?}: the bare flushes' timer pairs (BL-636 R2)");
            // OD-L3-10 (M-7): with no V there is no flush to book, on either kind.
            assert_eq!(sut.tally().delta(), CauseCounts::default(), "{how:?}: none booked");
            assert_eq!(sut.tally().since_open(), CauseCounts::default(), "{how:?}");
        }
        assert_eq!(sut.restart(Teardown::Clean), 0, "a down SUT's restart runs no teardown");
        assert_eq!(sut.load_result().status, LoadStatus::Failure);
        assert_eq!(sut.persisted().expect("read"), stored);
        lines.lock().unwrap().clear();
        let level_2 = ["-regtest", "-printtoconsole=0", "-checklevel=2"];
        let ui = Arc::clone(&sut.ui);
        let sut = restarted_with(sut, dir.path(), &level_2, ui);
        assert_eq!(sut.down(), Some(message.as_str()), "-checklevel=2");
        assert!(lines.lock().unwrap().iter().any(|l| l.contains(cause)), "{lines:?}");
    }

    /// BL-636: coins whose best block's index record is gone. Core's `LoadChainTip` finds no
    /// entry after `InitCoinsCache`, so the load is `FAILURE` "Error initializing block database"
    /// with the reindex question, and shutdown runs both flushes (`CanFlushToDisk`), each logging
    /// the evodb timer pair at the stored best block. rdashd refuses in recovery
    /// (`InitError::UnknownTip`), with the same text, and keeps the chainstate for the flushes.
    // Core (v24.0.0-rc.5): node/chainstate.cpp:272-276; validation.cpp:5003-5018, :3089, :3107;
    // validation.h:565; init.cpp:380-385, :450-455, :2115-2131
    #[test]
    fn a_coins_tip_without_its_index_record_fails_after_the_coins() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        // The clean shutdown's flush stores V's tip, b2: the coins' best block.
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        coins.restart();
        let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        let mut drop_tip = coins.batch();
        drop_tip.delete(cf, b2.hash().as_bytes());
        coins.write(drop_tip, Commit::Durable).unwrap();
        let argv = ["-regtest", "-printtoconsole=0"];
        let (ui, lines) = logged_ui();
        let sut = NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui, coins).expect("accepted");
        assert_eq!(sut.persisted().expect("read").meta.tip, Some(b2.hash()));
        assert_eq!(sut.load_result().status, LoadStatus::Failure);
        let message = reindex_question(dashd_chainstate::chain::INIT_BLOCK_DB_FAILED);
        assert_eq!(sut.down(), Some(message.as_str()));
        assert!(sut.chain().is_none());
        assert_eq!(sut.flushes(), 2, "after the coins cache, as Core's");
        assert_eq!(bl636::pairs(&lines.lock().unwrap()), (2, 2), "one pair per flush");
    }

    /// A clean stop of `sut` and a start on its coins at MOCKTIME with `argv`, logging to `ui`.
    fn restarted_with(mut sut: NodeSut, dir: &Path, argv: &[&str], ui: Arc<Ui>) -> NodeSut {
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        coins.restart();
        NodeSut::with_coins(dir, argv, MOCKTIME, ui, coins).expect("accepted")
    }

    /// DEC-981a's budget: the coins share the attached cache compares VerifyDB's level-3 figure
    /// with is Core's `m_coinstip_cache_size_bytes`, `CalculateCacheSizes(..).coins`, at every
    /// `-dbcache` and with the indexes off and on (`calculate_equals_cores_grids` pins the split
    /// to Core's numbers).
    // Core: node/caches.cpp; init.cpp:1983-1988; validation.cpp:5119-5122
    #[test]
    fn the_level_3_budget_is_cores_coins_share() {
        use dashd_chainstate::caches::{CacheSizes, IndexArgs};
        let on = IndexArgs {
            txindex: true,
            addressindex: true,
            timestampindex: true,
            spentindex: true,
            filters: &["basic"],
        };
        let indexes = [
            "-txindex=1",
            "-addressindex=1",
            "-timestampindex=1",
            "-spentindex=1",
            "-blockfilterindex=basic",
        ];
        for dbcache in [4, 100, 300, 1000, 5000] {
            for (args, with) in [(IndexArgs::default(), &[][..]), (on, &indexes[..])] {
                let dir = tempfile::tempdir().unwrap();
                let flag = format!("-dbcache={dbcache}");
                let mut argv = vec![flag.as_str()];
                argv.extend_from_slice(with);
                let sut = sut(dir.path(), &argv);
                let cell = Arc::clone(sut.steps.coins_cell().expect("the coins attached"));
                let share = sut
                    .chain()
                    .unwrap()
                    .query(move |_| cell.try_lock().expect("V's leaf").budget().coins_share());
                let want = CacheSizes::calculate(dbcache, &args).coins;
                assert_eq!(share.recv().unwrap(), want, "{argv:?}");
            }
        }
    }

    /// BL-119 (CH1-node D39) through a real startup: a loaded tip a2 the stored index marks failed,
    /// as `invalidateblock a1` before a crash leaves it, and two blocks tying it, b2 and c2, whose
    /// coinbases overpay (ConnectBlock's `bad-cb-amount`) and which no ABC has tried, so a2 is the
    /// best known invalid block. At the default `-checklevel`, VerifyDB disconnects a2 and a1 over
    /// its scratch view (level 3) and init's `ResetBlockFailureFlags(nullptr)` un-fails a2: the
    /// node runs on a2 whatever startup's ABC tries first. At `-checklevel=2` no reset runs: a2
    /// stays failed, both tied blocks fail, and the node runs on from the side block it last
    /// connected; a block extending it connects. Both are dashd v24's outcomes (D39, reproduced at
    /// rc.3). `-checkblockindex=0`: Core asserts at the next check after the level-2 run
    /// (`v:5755`), and so does V.
    // Core: node/chainstate.cpp:463-465; validation.cpp:4155-4163, :3528-3529
    #[test]
    fn a_failed_loaded_tip_is_reset_at_the_default_checklevel_only() {
        use dash_chain::Status;
        const COIN: i64 = 100_000_000;
        for level in [None, Some("-checklevel=2")] {
            let dir = tempfile::tempdir().unwrap();
            let mut argv = vec!["-checkblockindex=0"];
            argv.extend(level);
            let mut sut = sut(dir.path(), &argv);
            imported(&mut sut);
            let p = Arc::new(sut.params.chain.clone());
            let genesis = dashd_chainstate::chain::genesis_block(&p);
            let t = u32::try_from(MOCKTIME).unwrap() - 600;
            // a1 old and a2 recent: V leaves IBD at a2, whose flush stores the coins and index.
            let a1 = regtest_block(&p, genesis.header(), 1, genesis.header().time() + 1);
            let a2 = regtest_block(&p, a1.header(), 2, t);
            submit(&sut, &a1);
            submit(&sut, &a2);
            stored_tip_reaches(&sut, &a2.hash(), "LEFT_IBD's flush landed");
            let b1 = regtest_block_paying(&p, genesis.header(), 1, t + 1, 1);
            let b2 = regtest_block_paying(&p, b1.header(), 2, t + 2, 10_000 * COIN);
            let c1 = regtest_block_paying(&p, genesis.header(), 1, t + 3, 2);
            let c2 = regtest_block_paying(&p, c1.header(), 2, t + 4, 10_000 * COIN);
            for b in [&b1, &b2, &c1, &c2] {
                submit(&sut, b);
            }
            assert_eq!(view_tip(&sut), a2.hash(), "the first block at its work");
            // `invalidateblock a1`'s index write, the coins still at a2; shutdown's flush writes
            // the side blocks' entries only.
            let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
            let mut put = sut.coins.batch();
            let flags = [(&a1, Status::FAILED_VALID), (&a2, Status::FAILED_CHILD)];
            for (b, flag) in flags {
                let mut record = stored_record(&sut, &b.hash());
                record.status = Status(record.status.bits() | flag);
                put.put(cf, b.hash().as_bytes(), &record.to_bytes());
            }
            sut.coins.write(put, Commit::Durable).unwrap();
            assert_eq!(sut.restart(Teardown::Clean), 2);
            loaded(&sut);
            // Startup's ABC, in the import thread after `fImporting` clears: its end state is
            // waited for.
            imported(&mut sut);
            let v = sut.chain().unwrap().view_handle();
            let status = |b: &Arc<dash_consensus::Block>| {
                let id = v.lookup(&b.hash()).expect("indexed");
                v.entry(id).expect("an entry").status
            };
            let failed = |b| status(b).has(Status::FAILED_MASK);
            if level.is_none() {
                wait_for("the ABC ends on a2", || view_tip(&sut) == a2.hash());
                assert!(!failed(&a1) && !failed(&a2), "{:?} {:?}", status(&a1), status(&a2));
                // DEC-992: shutdown's flush stores the reset, so a start at `-checklevel=2`, which
                // runs none, loads a2 valid and stays on it; the tied blocks stay untried.
                let argv = ["-regtest", "-printtoconsole=0", "-checkblockindex=0", "-checklevel=2"];
                let mut sut = restarted_with(sut, dir.path(), &argv, ui());
                for b in [&a1, &a2] {
                    let stored = stored_record(&sut, &b.hash()).status;
                    assert!(!stored.has(Status::FAILED_MASK), "{stored:?}");
                }
                loaded(&sut);
                imported(&mut sut);
                assert_eq!(view_tip(&sut), a2.hash(), "the stored reset");
            } else {
                assert!(failed(&a1) && failed(&a2), "no reset at level 2");
                wait_for("every tied block failed", || failed(&b2) && failed(&c2));
                let side = [b1.hash(), c1.hash()];
                wait_for("the ABC ends on a side block", || side.contains(&view_tip(&sut)));
                let tip = view_tip(&sut);
                let on = if tip == b1.hash() {
                    &b1
                } else {
                    assert_eq!(tip, c1.hash(), "the last side block connected");
                    &c1
                };
                let d2 = regtest_block(&p, on.header(), 2, t + 5);
                submit(&sut, &d2);
                assert_eq!(view_tip(&sut), d2.hash(), "the node runs on from it");
            }
        }
    }

    /// A devnet restarts on its stored chainstate (2b-D8, DEC-613): a stored index has no pending
    /// devnet step, as Core's `LoadGenesisBlock` returns once genesis is indexed, so the load runs
    /// `LoadChainTip` at the tip shutdown's flush stored, as on any network. The index load's own
    /// refusal is unchanged.
    // Core: validation.cpp:5406-5407; node/chainstate.cpp:272-277
    #[test]
    fn a_devnet_restarts_on_its_stored_chainstate() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["-devnet=nd1", "-listen=0", "-printtoconsole=0"];
        let mut sut = NodeSut::new(dir.path(), &argv, MOCKTIME, ui()).expect("accepted");
        loaded(&sut);
        let tip = view_tip(&sut);
        for _ in 0..2 {
            assert_eq!(sut.restart(Teardown::Clean), 2);
            loaded(&sut);
            assert_eq!(sut.steps.coins_tip(), Some(tip), "stored by shutdown's flush");
            assert_eq!(view_tip(&sut), tip);
        }
        // The index load's refusal (review r2 n2).
        sut.scan_fault = Some(ErrorKind::Corruption);
        assert_eq!(sut.restart(Teardown::Kill), 0);
        assert_eq!(sut.down(), Some(reindex_question("Error loading block database").as_str()));
        sut.scan_fault = None;
        assert_eq!(sut.restart(Teardown::Kill), 0);
        loaded(&sut);
        assert_eq!(view_tip(&sut), tip);
    }

    /// A devnet's stored coins whose block index is gone meet V's pending devnet step, which has no
    /// `LoadChainTip` after it (CH1's), so init refuses before V starts instead of V's startup ABC
    /// failing over the coins (BL-271 r1). Core loads there, at the coins' tip (genesis).
    // Core: validation.cpp:5406-5407; node/chainstate.cpp:272-277
    #[test]
    fn a_devnet_s_coins_without_their_index_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["-devnet=nd1", "-listen=0", "-printtoconsole=0"];
        let (ui, lines) = logged_ui();
        let mut sut = NodeSut::new(dir.path(), &argv, MOCKTIME, ui).expect("accepted");
        loaded(&sut);
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        let keys: Vec<_> = sut.coins.iter_prefix(cf, b"").map(|r| r.unwrap().0).collect();
        assert!(!keys.is_empty());
        let mut wipe = sut.coins.batch();
        for key in &keys {
            wipe.delete(cf, key);
        }
        sut.coins.write(wipe, Commit::Durable).unwrap();
        lines.lock().unwrap().clear();
        assert_eq!(sut.restart(Teardown::Kill), 0);
        let down = sut.down().expect("refused");
        assert!(down.starts_with("rdashd cannot load devnet"), "{down}");
        assert!(down.contains("on stored coins without their block index"), "{down}");
        assert!(!down.contains("-reindex"), "no reindex question: {down}");
        assert!(sut.chain().is_none(), "V never started");
        assert_eq!(sut.flushes(), 0, "before the coins cache");
        assert!(!lines.lock().unwrap().iter().any(|l| l.contains("***")), "{lines:?}");
    }

    /// A fresh devnet's step (V's `AcceptBlock` of the devnet genesis block) runs before the coins
    /// open, as Core's `LoadGenesisBlock` runs before `InitCoinsDB`: a failed write there is init's
    /// `AbortNode` with no coins cache, so shutdown runs neither flush (`CanFlushToDisk`).
    // Core: node/chainstate.cpp:210, :233; validation.cpp:5421, :4849-4850, init.cpp:379-385
    #[test]
    fn a_fresh_devnet_s_failed_step_comes_before_the_coins() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["-devnet=nd1", "-listen=0", "-printtoconsole=0"];
        let (ui, lines) = logged_ui();
        let coins = Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA));
        let mut sut = NodeSut::unopened(dir.path(), &argv, MOCKTIME, ui, coins).expect("accepted");
        sut.wrap_files = Some(|files| Box::new(NoRoomAtOne(files)));
        sut.open();
        assert_eq!(sut.down(), Some(SHUTDOWN_REQUESTED));
        assert!(sut.shutdown.aborted());
        assert_eq!(sut.flushes(), 0, "no coins cache");
        let lines = lines.lock().unwrap();
        assert!(lines.iter().any(|l| l.ends_with("*** System error: disk full\n")), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("ForceFlushStateToDisk")), "{lines:?}");
    }

    /// BL-279: `submitblock` through the node's command table when the block's write throws, as
    /// Core's `SaveBlockToDisk` can. Core's `AbortNode(state, ...)` reports once and requests
    /// shutdown first; then `BlockChecked(Error)` answers -25 with "System error: <what>", not
    /// "inconclusive".
    // Core: validation.cpp:4849-4850, :2245-2249, :4896-4899; rpc/mining.cpp:532-538, :1056-1063
    #[test]
    fn submitblock_answers_a_failed_write_with_core_s_system_error() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["-regtest", "-printtoconsole=0"];
        let (ui, lines) = logged_ui();
        let coins = Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA));
        let mut sut = NodeSut::unopened(dir.path(), &argv, MOCKTIME, ui, coins).expect("accepted");
        sut.wrap_files = Some(|files| Box::new(NoRoomAtOne(files)));
        sut.open();
        loaded(&sut);
        imported(&mut sut);
        let p = Arc::new(sut.params.chain.clone());
        let genesis = dashd_chainstate::chain::genesis_block(&p);
        let b1 = regtest_block(&p, genesis.header(), 1, u32::try_from(MOCKTIME).unwrap() - 300);
        let chain = sut.steps.chain().expect("V runs");
        let slot = Arc::new(crate::rpc::ChainSlot::default());
        slot.attach(chain.handle(), chain.signals(), None, None, None);
        let table = crate::rpc::command_table(
            &p,
            &Arc::new(Logger::new()),
            &sut.shutdown,
            sut.clock(),
            (true, &crate::scheduler::Scheduler::new()),
            &slot,
            &Arc::default(),
            &Arc::default(),
            &Arc::default(),
            Default::default(),
            std::env::temp_dir(),
        );
        table.start();
        table.set_warmup_finished();
        let hex: String = dash_consensus::primitives::core_serialize(&*b1)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let answer = crate::chain::tests::rpc(&table, "submitblock", &format!(r#"["{hex}"]"#));
        assert!(sut.shutdown.aborted(), "AbortNode's request came before the answer");
        slot.detach();
        table.interrupt();
        let message = r#"{"code":-25,"message":"System error: disk full"}"#;
        assert_eq!(answer, Err((-25, message.to_owned())));
        let lines = lines.lock().unwrap();
        let aborts = lines.iter().filter(|l| l.ends_with("*** System error: disk full\n"));
        assert_eq!(aborts.count(), 1, "{lines:?}");
    }

    /// Submits the blocks after `tip` up to `height`, a second apart, and returns them.
    fn extend_to(
        sut: &NodeSut,
        tip: &Arc<dash_consensus::Block>,
        height: u8,
    ) -> Vec<Arc<dash_consensus::Block>> {
        let p = Arc::new(sut.params.chain.clone());
        let mut blocks: Vec<Arc<dash_consensus::Block>> = Vec::new();
        let from = u8::try_from(sut.chain().unwrap().view_handle().view().snap.tip.tip_height)
            .unwrap()
            + 1;
        for h in from..=height {
            let prev = *blocks.last().unwrap_or(tip).header();
            let b = regtest_block(&p, &prev, h, prev.time() + 1);
            submit(sut, &b);
            blocks.push(b);
        }
        blocks
    }

    /// A stored tip where G1q held until CH7b-2 (CH7 D3): block 24, which shutdown's flush
    /// stored, is a rotation cycle base past DIP0003 once the restart's `-dip3params` moves
    /// DIP0003 to height 1 (arm (c)). The startup check refuses nothing now, as dashd's has no
    /// such check, so the node loads at 24.
    #[test]
    fn a_stored_tip_g1q_held_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        let b24 = extend_to(&sut, &b2, 24).pop().unwrap();
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        coins.restart();
        let argv = ["-regtest", "-printtoconsole=0", "-dip3params=1:1"];
        let mut sut =
            NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui(), coins).expect("accepted");
        assert!(sut.steps.join_import(), "loadblk ends");
        assert!(!sut.shutdown.aborted(), "loadblk's activation did not abort");
        loaded(&sut);
        assert_eq!(sut.chain().unwrap().view().snap.tip.tip_hash, b24.hash());
    }

    /// BL-523: a start on a chainstate whose `pruned_files` flag is set (Core's
    /// `prunedblockfiles`) runs the startup scan, which removes the blk and rev files of a file
    /// below the last one whose info is empty, as a prune whose unlink was lost leaves them; then
    /// it refuses the unpruned load with Core's `-prune` state error and its reindex question,
    /// before V starts. The last file's, empty too, stay (Core's `file_number < m_last_blockfile`).
    /// With the flag clear the files stay and the node loads. The scan's lines are
    /// dashd-chainstate's tests'.
    // Core (375c64d2317): validation.cpp:5473; node/blockstorage.cpp:429-444, :635-646;
    // node/chainstate.cpp:200-204
    #[test]
    fn a_pruned_chainstate_is_scanned_then_refused() {
        use dashd_chainstate::chain::PRUNED_DATADIR;
        use dashd_chainstate::store::Meta;
        let argv = ["-regtest", "-printtoconsole=0"];
        for have_pruned in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut sut = NodeSut::new(dir.path(), &argv, MOCKTIME, ui()).expect("accepted");
            loaded(&sut);
            sut.steps.shutdown();
            let coins = Arc::clone(&sut.coins);
            drop(sut);
            coins.restart();
            let blocks = dir.path().join("regtest").join("blocks");
            let planted = ["blk00001.dat", "rev00001.dat"].map(|f| blocks.join(f));
            let last = ["blk00002.dat", "rev00002.dat"].map(|f| blocks.join(f));
            for f in planted.iter().chain(&last) {
                std::fs::write(f, [0; 8]).unwrap();
            }
            // The infos of files 1 and 2 are absent, so empty; file 1 is below the last file.
            let mut put = coins.batch();
            Meta::put_last_file(&mut put, 2);
            Meta::put_pruned_files(&mut put, have_pruned);
            coins.write(put, Commit::Durable).unwrap();
            let sut =
                NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui(), coins).expect("accepted");
            assert!(blocks.join("blk00000.dat").exists(), "genesis's file stays");
            assert!(planted.iter().all(|f| f.exists() != have_pruned), "{have_pruned}");
            assert!(last.iter().all(|f| f.exists()), "{have_pruned}: the last file stays");
            if have_pruned {
                assert_eq!(sut.down(), Some(reindex_question(PRUNED_DATADIR).as_str()));
                assert!(sut.chain().is_none(), "V never started");
            } else {
                loaded(&sut);
            }
        }
    }

    /// BL-523: on an empty index (no tip yet) the start runs `LoadBlockIndexDB` and the scan too,
    /// then the `-prune` state check before `LoadGenesisBlock`: a set `pruned_files` flag removes
    /// the files a prune left below the last file and refuses, with no genesis block written; the
    /// last file's stay as they were.
    // Core (375c64d2317): validation.cpp:5473; node/chainstate.cpp:200-211
    #[test]
    fn an_empty_pruned_chainstate_is_scanned_then_refused_before_genesis() {
        use dashd_chainstate::chain::PRUNED_DATADIR;
        use dashd_chainstate::store::Meta;
        let dir = tempfile::tempdir().unwrap();
        let blocks = dir.path().join("regtest").join("blocks");
        std::fs::create_dir_all(&blocks).unwrap();
        let planted = ["blk00001.dat", "rev00001.dat"].map(|f| blocks.join(f));
        for f in &planted {
            std::fs::write(f, [0; 8]).unwrap();
        }
        let last = ["blk00002.dat", "rev00002.dat"];
        for f in last {
            std::fs::write(blocks.join(f), [0; 8]).unwrap();
        }
        let coins = Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA));
        let mut put = coins.batch();
        Meta::put_last_file(&mut put, 2);
        Meta::put_pruned_files(&mut put, true);
        coins.write(put, Commit::Durable).unwrap();
        let argv = ["-regtest", "-printtoconsole=0"];
        let sut = NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui(), coins).expect("accepted");
        assert_eq!(sut.down(), Some(reindex_question(PRUNED_DATADIR).as_str()));
        assert!(sut.chain().is_none(), "V never started");
        assert!(planted.iter().all(|f| !f.exists()), "scanned");
        // `LoadGenesisBlock` would write at the last file, 2.
        let mut left: Vec<_> = std::fs::read_dir(&blocks)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("blk") || name.starts_with("rev"))
            .collect();
        left.sort();
        assert_eq!(left, last, "no genesis block written");
        for f in last {
            assert_eq!(std::fs::read(blocks.join(f)).unwrap(), [0; 8], "{f} as planted");
        }
    }

    /// dashd's regtest blocks 1 to 57 with DIP0003 at 48 and DIP0024 out of the window
    /// (`tests/data/ch7a4/record.sh`): block 48 is no rotation cycle base there, so each connects.
    const CH7A4_BLOCKS: &str = include_str!("../tests/data/ch7a4/blocks.txt");

    /// dashd's blocks 1 to 200 on the same settings (`dashd-chainstate/tests/data/ch7a4b`): from
    /// 58, every block of `llmq_test`'s mining windows (58-66, then every 24) carries null
    /// commitments.
    const CH7A4B_BLOCKS: &str = include_str!("../../dashd-chainstate/tests/data/ch7a4b/blocks.txt");

    /// The recorded chain's settings, as `record.sh` ran dashd.
    const CH7A4_ARGS: [&str; 2] = ["-dip3params=48:48", "-testactivationheight=dip0024@1000"];

    /// A recording's blocks, skipping its `#` header lines.
    fn recorded_blocks(text: &str) -> Vec<Arc<dash_consensus::Block>> {
        text.lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| {
                let bytes: Vec<u8> = (0..l.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap())
                    .collect();
                let mut input = bytes.as_slice();
                let block = dash_consensus::primitives::decode_block(&mut input).expect("a block");
                Arc::new(block.into_value())
            })
            .collect()
    }

    /// CH7a-4b (D3, D67): a node stopped at each of `stops` on the recorded chain restarts, its
    /// startup readiness check reading the tip's block and the blocks recovery replays, and
    /// connects on to the chain's tip, 200. A clean stop restarts at its tip; a killed node comes
    /// back at V's last flush, at or below it, and takes the blocks after it again.
    fn restarts_on_the_recorded_chain(stops: &[usize]) {
        let blocks = recorded_blocks(CH7A4B_BLOCKS);
        for &stop in stops {
            for how in [Teardown::Clean, Teardown::Kill] {
                let dir = tempfile::tempdir().unwrap();
                let mut sut = try_sut(dir.path(), &CH7A4_ARGS, ui()).expect("accepted");
                imported(&mut sut);
                for b in &blocks[..stop] {
                    submit(&sut, b);
                }
                assert_eq!(view_tip(&sut), blocks[stop - 1].hash());
                sut.restart(how);
                loaded(&sut);
                imported(&mut sut);
                let tip = view_tip(&sut);
                let at = blocks.iter().position(|b| b.hash() == tip).map_or(0, |i| i + 1);
                assert!(at <= stop, "{how:?} at {stop}: back at {at}");
                if how == Teardown::Clean {
                    assert_eq!(at, stop, "a clean stop restarts at its tip");
                }
                for b in &blocks[at..] {
                    submit(&sut, b);
                }
                let last = blocks.last().expect("200 blocks");
                assert_eq!(view_tip(&sut), last.hash(), "{how:?} at {stop}");
                assert_eq!(sut.down(), None);
            }
        }
    }

    /// At DIP0003's block 48 and at 57, the last block before the first mining window, whose
    /// first block, 58, is the first the reopened node connects after 57.
    #[test]
    fn a_restart_at_dip3_and_before_the_first_window_connects_on() {
        restarts_on_the_recorded_chain(&[48, 57]);
    }

    /// Inside the first mining window (58, its first block; 62; 66, its last) and right after it
    /// (67): each tip carries dashd's null commitments, which the readiness check reads.
    #[test]
    fn a_restart_inside_a_mining_window_and_after_it_connects_on() {
        restarts_on_the_recorded_chain(&[58, 62, 66, 67]);
    }

    /// CH7-notes D70 (CH7a-4a r1 m-1), which G1q refused until CH7b-2: dashd's blocks connect
    /// with DIP0024 moved out; on regtest's own DIP0024 (from height 1) after the restart, block 48
    /// is a rotation cycle base and the tip 57 is not. `invalidateblock 48` undoes 57 to 49, then
    /// 48, as dashd does: H9's precompute computes the rotated members at 48 from its work block's
    /// list, below DIP0003. The tip is 47 and nothing aborts.
    #[test]
    fn an_undo_at_a_cycle_base_g1q_held_undoes() {
        let blocks = recorded_blocks(CH7A4_BLOCKS);
        let (b47, b48, b57) = (&blocks[46], &blocks[47], &blocks[56]);
        let dir = tempfile::tempdir().unwrap();
        let (ui, lines) = logged_ui();
        let dip3 = CH7A4_ARGS[0];
        let mut sut = try_sut(dir.path(), &CH7A4_ARGS, Arc::clone(&ui)).expect("accepted");
        imported(&mut sut);
        for b in &blocks {
            submit(&sut, b);
        }
        assert_eq!(sut.chain().unwrap().tip_watch().get(), Some(b57.hash()));
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        coins.restart();
        let argv = ["-regtest", "-printtoconsole=0", dip3];
        let sut = NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui, coins).expect("accepted");
        loaded(&sut);
        assert_eq!(sut.chain().unwrap().view().snap.tip.tip_hash, b57.hash(), "57 passed");
        let _ = sut.chain().unwrap().invalidate_block(b48.hash()).recv();
        let tip = || sut.chain().unwrap().view().snap.tip.tip_hash;
        wait_for("48 undone", || tip() == b47.hash());
        assert!(!sut.shutdown.aborted());
        let logged = lines.lock().unwrap().clone();
        assert!(!logged.iter().any(|l| l.contains("cannot validate")), "{logged:?}");
    }

    /// BL-650 (DEC-1064): a stored chain whose blocks have no list rows, under a restart's
    /// `-dip3params=1:1`, loads at b24 by taking b24 as Core's initial snapshot, and loads so again
    /// after a second restart, since the initial-snapshot block is in memory only and nothing was
    /// written at b24. Connecting b25, a child of it, writes its diff and a snapshot row (Core's
    /// `pindex->pprev == m_initial_snapshot_index`); b24 keeps no row.
    // Core: evo/deterministicmns.cpp:722, :882-889; evo/deterministicmns.h:780
    #[test]
    fn the_initial_snapshot_block_s_child_writes_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        let b24 = extend_to(&sut, &b2, 24).pop().unwrap();
        let argv = ["-regtest", "-printtoconsole=0", "-dip3params=1:1"];
        let stopped = |mut sut: NodeSut| {
            sut.steps.shutdown();
            let coins = Arc::clone(&sut.coins);
            drop(sut);
            coins.restart();
            coins
        };
        let restarted = |sut: NodeSut| {
            let coins = stopped(sut);
            let mut sut =
                NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui(), coins).expect("accepted");
            assert!(sut.steps.join_import(), "loadblk ends");
            assert!(!sut.shutdown.aborted(), "loadblk's activation did not abort");
            loaded(&sut);
            assert_eq!(view_tip(&sut), b24.hash());
            sut
        };
        let sut = restarted(restarted(sut));
        let p = sut.params.chain.clone();
        let b25 = cbtx_block(&p, b24.header(), 25, b24.header().time() + 1);
        submit(&sut, &b25);
        let coins = stopped(sut);
        let row = |cf: &str, block: &Arc<dash_consensus::Block>| {
            let cf = CHAINSTATE_SCHEMA.cf(cf).unwrap();
            coins.get(cf, block.hash().as_bytes()).unwrap().is_some()
        };
        assert!(row("mn_diff", &b25), "b25's diff");
        assert!(row("mn_snapshot", &b25), "the initial-snapshot block's child's snapshot");
        assert!(!row("mn_diff", &b24) && !row("mn_snapshot", &b24), "b24 keeps no row");
    }

    /// BL-650 (OD-CP-51 F1): on the same row-less store, `invalidateblock b24` undoes b24, as
    /// Core's `UndoBlock` does: b24 has no stored diff, which counts as no changes, so no list is
    /// resolved and the undo succeeds. The tip is b23 and nothing aborts. A sibling of b24 with a
    /// CbTx then connects over b23, which its walk takes as the initial snapshot, so the sibling
    /// writes its diff and a snapshot row, and b25 connects on the sibling. b24 itself is not
    /// reconnected: it is a block from before the move, with no CbTx, which DIP0003 at height 1
    /// requires, so `reconsiderblock b24` leaves it failed and the tip at b23. A row-less block
    /// cannot be reconnected under the new rules (Opus r1 §2), so the reconnect is the sibling's.
    // Core: evo/deterministicmns.cpp:784-817 (:793-798 the diff read and HasChanges), :722
    #[test]
    fn a_row_less_tip_undoes_and_a_sibling_connects() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        let mut after = extend_to(&sut, &b2, 24);
        let b24 = after.pop().unwrap();
        let b23 = after.pop().unwrap();
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        coins.restart();
        let argv = ["-regtest", "-printtoconsole=0", "-dip3params=1:1"];
        let mut sut =
            NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui(), coins).expect("accepted");
        assert!(sut.steps.join_import(), "loadblk ends");
        loaded(&sut);
        assert_eq!(view_tip(&sut), b24.hash());
        let chain = sut.chain().unwrap();
        assert_eq!(chain.invalidate_block(b24.hash()).recv(), Ok(Ok(())));
        wait_for("b24 undone", || view_tip(&sut) == b23.hash());
        assert!(!sut.shutdown.aborted(), "the undo of b24 did not abort");
        let p = sut.params.chain.clone();
        let sibling = cbtx_block(&p, b23.header(), 24, b24.header().time() + 1);
        submit(&sut, &sibling);
        assert_eq!(view_tip(&sut), sibling.hash());
        let b25 = cbtx_block(&p, sibling.header(), 25, sibling.header().time() + 1);
        submit(&sut, &b25);
        assert_eq!(view_tip(&sut), b25.hash());
        assert!(!sut.shutdown.aborted());
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        let row = |cf: &str, block: &Arc<dash_consensus::Block>| {
            let cf = CHAINSTATE_SCHEMA.cf(cf).unwrap();
            coins.get(cf, block.hash().as_bytes()).unwrap().is_some()
        };
        assert!(row("mn_diff", &sibling), "the sibling's diff");
        assert!(
            row("mn_snapshot", &sibling),
            "b23 is the initial snapshot, so the sibling writes one"
        );
        assert!(row("mn_diff", &b25), "b25's diff");
        assert!(!row("mn_diff", &b23) && !row("mn_snapshot", &b23), "b23 keeps no row");
        assert!(!row("mn_diff", &b24) && !row("mn_snapshot", &b24), "b24 keeps none either");
    }

    /// The blocks recovery replays were G1q's too until CH7b-2 (review r2 m2): a split flush's
    /// marker for b25 over null replays b1 to b25, and under `-dip3params=1:1` b24 is a cycle base
    /// past DIP0003. The startup check refuses none of them now, as dashd's has no such check: the
    /// replay stores its tip and the node loads at b25.
    #[test]
    fn the_blocks_recovery_replays_load_without_a_readiness_check() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at(dir.path(), 2);
        let after = extend_to(&sut, &b2, 25);
        let b25 = &after[after.len() - 1];
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        // As a first flush interrupted after its marker (`old` null): the coins' tip is erased.
        let mut marker = coins.batch();
        marker.delete(meta_cf(), b"tip");
        marker.put(
            meta_cf(),
            b"head_blocks",
            &[b25.hash().as_bytes().as_slice(), &[0; 32]].concat(),
        );
        coins.write(marker, Commit::Durable).unwrap();
        coins.restart();
        let argv = ["-regtest", "-printtoconsole=0", "-dip3params=1:1"];
        let mut sut =
            NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui(), coins).expect("accepted");
        assert!(sut.steps.join_import(), "loadblk ends");
        assert!(!sut.shutdown.aborted(), "loadblk's activation did not abort");
        loaded(&sut);
        assert_eq!(sut.chain().unwrap().view().snap.tip.tip_hash, b25.hash());
        assert_eq!(stored_tip(&sut), Some(b25.hash()), "recovery stored its replay to b25");
    }

    /// 2b-D6's VerifyDB row (BL-278 (a)): a node restarted with its clock more than two hours
    /// behind the stored tip's time fails `VerifyLoadedChainstate` with FUTURE_TIP. Init says it
    /// with the reindex question, and the load's teardown runs both flushes: the coins cache
    /// exists by then. Two hours exactly still loads.
    // Core: node/chainstate.cpp:423-428; init.cpp:2115-2133; validation.cpp:2969 (CanFlushToDisk)
    #[test]
    fn a_stored_tip_from_the_future_fails_verification_after_the_coins() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        // Shutdown's flushes stored b2 (BL-271), and the restarts judge b2's time, as Core's.
        assert_eq!(sut.coins_tip(), Some(b2.hash()), "a stored tip");
        let tip_time = i64::from(b2.header().time());
        let argv = ["-regtest", "-printtoconsole=0"];
        let restart_at = |mut sut: NodeSut, mocktime: i64| {
            sut.steps.shutdown();
            let coins = Arc::clone(&sut.coins);
            drop(sut);
            coins.restart();
            NodeSut::with_coins(dir.path(), &argv, mocktime, ui(), coins).expect("accepted")
        };
        let sut = restart_at(sut, tip_time - MAX_FUTURE_BLOCK_TIME);
        loaded(&sut);
        assert_eq!(sut.coins_tip(), Some(b2.hash()));
        let sut = restart_at(sut, tip_time - MAX_FUTURE_BLOCK_TIME - 1);
        assert_eq!(sut.down(), Some(reindex_question(FUTURE_TIP).as_str()));
        assert!(sut.chain().is_none());
        assert_eq!(sut.flushes(), 2, "after the coins cache");
    }

    /// The node's `blk00000.dat` with `block`'s transaction count garbled, as DIFF-RPC's
    /// `garbled_count`: Core's `ReadCompactSize` refuses `0xff` ("size too large"), so a read of
    /// the block fails while its file and index entry stay. The byte's offset and old value.
    // Core: serialize.h ReadCompactSize; node/blockstorage.cpp:837
    fn garble_tx_count(dir: &Path, block: &dash_consensus::Block) -> (usize, u8) {
        let raw = dash_consensus::primitives::core_serialize(block);
        let at = blk0(dir).windows(raw.len()).position(|w| w == raw).expect("in blk00000.dat") + 80;
        let was = set_blk0_byte(dir, at, 0xff);
        (at, was)
    }

    /// `blk00000.dat`'s bytes.
    fn blk0(dir: &Path) -> Vec<u8> {
        std::fs::read(dir.join("regtest").join("blocks").join("blk00000.dat")).unwrap()
    }

    /// Sets `blk00000.dat`'s byte at `at` to `value`; the byte it held.
    fn set_blk0_byte(dir: &Path, at: usize, value: u8) -> u8 {
        let path = dir.join("regtest").join("blocks").join("blk00000.dat");
        let mut bytes = std::fs::read(&path).unwrap();
        let was = std::mem::replace(&mut bytes[at], value);
        std::fs::write(&path, bytes).unwrap();
        was
    }

    /// A clean stop of `sut`, `change` to its files while it is down, and a start on the same
    /// coins at MOCKTIME.
    fn restarted_after(mut sut: NodeSut, dir: &Path, change: impl FnOnce()) -> NodeSut {
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        change();
        coins.restart();
        let argv = ["-regtest", "-printtoconsole=0"];
        NodeSut::with_coins(dir, &argv, MOCKTIME, ui(), coins).expect("accepted")
    }

    /// BL-275 (a): a stored tip whose block no longer reads, its file and index entry intact,
    /// fails `VerifyLoadedChainstate` with VerifyDB's CORRUPTED_BLOCK_DB, as Core's level-0 read
    /// does. Init asks the reindex question after the coins, so both flushes run. No load step
    /// reads the tip before VerifyDB: the readiness guard leaves its failed read to it
    /// (a7e8eee5a), where rdashd once refused with "Error opening block database". With the byte
    /// restored, the same files load at the tip.
    // Core: validation.cpp:5237-5240; node/chainstate.cpp:447-448; init.cpp:2139-2154
    #[test]
    fn an_unreadable_stored_tip_fails_verification_after_the_coins() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        assert_eq!(sut.coins_tip(), Some(b2.hash()), "a stored tip");
        let mut garbled = None;
        let sut =
            restarted_after(sut, dir.path(), || garbled = Some(garble_tx_count(dir.path(), &b2)));
        assert_eq!(sut.down(), Some(reindex_question(CORRUPTED_BLOCK_DB).as_str()));
        assert!(sut.chain().is_none());
        assert_eq!(sut.flushes(), 2, "after the coins cache");
        assert_eq!(stored_tip(&sut), Some(b2.hash()), "nothing rewrote the coins");
        let (at, was) = garbled.unwrap();
        let sut = restarted_after(sut, dir.path(), || {
            set_blk0_byte(dir.path(), at, was);
        });
        loaded(&sut);
        assert_eq!(view_tip(&sut), b2.hash());
    }

    /// BL-275 (b): a genesis tip whose block no longer reads still starts, as Core's: VerifyDB
    /// returns before reading a tip without a parent, and no other load step reads it. The
    /// coins' tip is genesis after a clean stop (BL-271's shutdown flush), so `LoadChainTip`
    /// runs and startup's ABC has nothing to connect. A block on top then connects: its
    /// ConnectBlock reads no genesis data.
    // Core: validation.cpp:5189-5191; node/chainstate.cpp:272-277; evo/mnhftx.cpp:63
    #[test]
    fn an_unreadable_genesis_tip_still_starts() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &[]);
        loaded(&sut);
        let p = Arc::new(sut.params.chain.clone());
        let genesis = dashd_chainstate::chain::genesis_block(&p);
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        assert_eq!(sut.coins_tip(), Some(genesis.hash()), "BL-275 (b)'s precondition");
        let mut sut = restarted_after(sut, dir.path(), || {
            garble_tx_count(dir.path(), &genesis);
        });
        loaded(&sut);
        assert_eq!(view_tip(&sut), genesis.hash());
        assert_eq!(sut.coins_tip(), Some(genesis.hash()));
        let b1 = regtest_block(&p, genesis.header(), 1, u32::try_from(MOCKTIME).unwrap() - 300);
        imported(&mut sut);
        submit(&sut, &b1);
        assert_eq!(sut.chain().unwrap().tip_watch().get(), Some(b1.hash()));
    }

    /// BL-273: the coins budget is Core's `nTotalSpace`, the coins share plus what the mempool
    /// leaves of its maximum, all of it while the pool is empty. At `-dbcache=4` the share alone
    /// (1.5 MiB) cannot hold three blocks of 9,500 coins below f·B, so W would flush them; with
    /// `-maxmempool`'s 300 MB they stay in memory, as dashd's do. The blocks are old, so V stays
    /// in initial block download (no LEFT_IBD flush), and a kill runs no flush of its own: V's
    /// stop waits for W's job, so the store holds what W wrote. The 300 MB is set: rdashd's
    /// `-blocksonly` default (DEC-322) would leave 5 MB, which a salt that bunches the coins in a
    /// few shards can fill (BL-353, BL-465). A real rdashd refuses `-maxmempool` (MP1b,
    /// init/support.rs:151); the SUT skips that refusal, so this pins Core's default-config
    /// arithmetic, not a configuration rdashd accepts.
    // Core: validation.cpp:2937-2966, :2994-2998
    #[test]
    fn the_coins_budget_has_the_empty_mempool_s_headroom() {
        let dir = tempfile::tempdir().unwrap();
        let mut sut = sut(dir.path(), &["-dbcache=4", "-maxmempool=300"]);
        let p = sut.params.chain.clone();
        let mut tip = dashd_chainstate::chain::genesis_block(&p);
        let t0 = tip.header().time();
        // DIP0001's 100 kB transactions: 9,500 outputs each.
        let outputs = vec![(0, &[0x51][..]); 9_500];
        for h in 1..=3 {
            tip = mine(&p, tip.header(), h, t0 + h, &outputs, vec![]);
            submit(&sut, &tip);
        }
        assert_eq!(sut.chain().unwrap().tip_watch().get(), Some(tip.hash()), "connected");
        sut.steps.kill();
        assert_eq!(stored_tip(&sut), None, "nothing flushed");
    }

    /// ST3b S0: `-dbbatchsize` bounds the coins flush's sub-batches, so a small value splits a
    /// flush into partial batches ("Writing partial batch") that the default writes as one, and
    /// both store the same tip. The flush is V's LEFT_IBD flush at b3, the first recent block (see
    /// [`left_ibd_at`]; the shutdown flushes are still BL-271's WAL-sync stub). 600 dirty coins
    /// are far above 1,000 bytes and far below the default's sub-batch.
    // Core: txdb.cpp:117, :145-148
    #[test]
    fn dbbatchsize_splits_the_coins_flush() {
        let flush = |argv: &[&str]| {
            let dir = tempfile::tempdir().unwrap();
            let (ui, lines) = logged_ui();
            let mut full = vec!["-debug=coindb"];
            full.extend_from_slice(argv);
            let mut sut = try_sut(dir.path(), &full, ui).unwrap();
            let p = sut.params.chain.clone();
            imported(&mut sut);
            let mut tip = dashd_chainstate::chain::genesis_block(&p);
            let t0 = tip.header().time();
            let recent = u32::try_from(MOCKTIME).unwrap() - 300;
            let outputs = vec![(0, &[0x51][..]); 200];
            for h in 1..=3 {
                let time = if h == 3 {
                    recent
                } else {
                    t0 + h
                };
                tip = mine(&p, tip.header(), h, time, &outputs, vec![]);
                submit(&sut, &tip);
            }
            stored_tip_reaches(&sut, &tip.hash(), &format!("{argv:?}: LEFT_IBD's flush landed"));
            let lines = lines.lock().unwrap();
            lines.iter().filter(|l| l.contains("Writing partial batch")).count()
        };
        assert_eq!(flush(&[]), 0);
        assert!(flush(&["-dbbatchsize=1000"]) > 1);
    }

    /// RP1a-2b's chain-state reads over the node's own coins cache (Opus r1 n-3): gettxout's
    /// coin read in V's turn, with its lock assert, for an unspent and a spent coin;
    /// getchaintxstats and getblockheaders from a block off the active chain; getdeploymentinfo
    /// and getblockheaders from an older block; getblockchaininfo's `headers` past its tip.
    // Core: rpc/blockchain.cpp:680-784, :1328-1411, :1594-1631, :1696-1739, :2103-2117
    #[test]
    fn the_chain_state_reads_the_coins_and_the_index() {
        use dashd_rpc::blockchain::{ChainTxError, NodeChainState as _};

        let dir = tempfile::tempdir().unwrap();
        let sut = sut(dir.path(), &[]);
        let p = sut.params.chain.clone();
        let genesis = dashd_chainstate::chain::genesis_block(&p);
        let t0 = genesis.header().time();
        let mut blocks = vec![mine(&p, genesis.header(), 1, t0 + 1, &[(1_000, &[0x51])], vec![])];
        for h in 2..=100 {
            let prev = blocks.last().unwrap().header();
            blocks.push(mine(&p, prev, h, t0 + h, &[(0, &[0x51])], vec![]));
        }
        let spent = OutPoint {
            hash: blocks[0].txs()[0].txid(),
            index: 0,
        };
        let input = TxIn {
            prevout: spent,
            script_sig: Vec::new().into(),
            sequence: u32::MAX,
        };
        let spend = tx(vec![input], &[(700, &[0x51])]);
        let unspent = OutPoint {
            hash: spend.txid(),
            index: 0,
        };
        blocks.push(mine(&p, blocks[99].header(), 101, t0 + 101, &[(0, &[0x51])], vec![spend]));
        for b in &blocks {
            submit(&sut, b);
        }
        // A block on block 99 beside the active block 100: stored, off the active chain.
        let side = mine(&p, blocks[98].header(), 100, t0 + 200, &[(1, &[0x51])], vec![]);
        submit(&sut, &side);
        // A header past the tip.
        let next = grind(&p, regtest_header(blocks[100].hash(), t0 + 102));
        let batch = dash_chain::HeaderBatch {
            hashes: vec![next.hash()],
            headers: vec![next],
        };
        let r = sut.chain().unwrap().process_headers(batch, None).recv().expect("V replies");
        assert!(r.stop.is_none(), "{:?}", r.stop);

        let slot = crate::rpc::ChainSlot::default();
        let coins = sut.steps.coins_cell().map(|c| crate::rpc::coins_read(Arc::clone(c)));
        assert!(coins.is_some(), "the node's coins are attached");
        slot.attach(sut.chain().unwrap(), sut.steps.chain().unwrap().signals(), None, None, coins);
        let state = slot.chain_state().unwrap();

        let tip = state.tip_info().unwrap();
        assert_eq!((tip.hash, tip.height, tip.headers), (blocks[100].hash(), 101, 102));
        let found = state.txout(&unspent).unwrap().expect("the spend's output is unspent");
        assert_eq!(
            (found.coin.value(), found.coin.height(), found.best_block, found.best_height),
            (700, 101, blocks[100].hash(), 101)
        );
        assert!(state.txout(&spent).unwrap().is_none(), "block 1's coinbase is spent");
        assert!(matches!(
            state.chain_tx_final(Some(&side.hash())),
            Err(ChainTxError::NotInMainChain)
        ));
        let off_chain = state.headers_from(&side.hash(), 10).unwrap();
        assert_eq!(off_chain.len(), 1, "one entry for a block off the active chain");
        assert_eq!((off_chain[0].hash, off_chain[0].confirmations), (side.hash(), -1));
        let older = state.headers_from(&blocks[49].hash(), 3).unwrap();
        let heights: Vec<_> = older.iter().map(|f| (f.height, f.confirmations)).collect();
        assert_eq!(heights, [(50, 52), (51, 51), (52, 50)]);
        let report = state.deployment_info(Some(&blocks[49].hash())).unwrap().expect("indexed");
        assert_eq!((report.hash, report.height), (blocks[49].hash(), 50));
    }

    /// BL-273: `-assumevalid` reaches V's connector. b101 spends b1's coinbase, whose script
    /// fails. Under `-assumevalid=0` its scripts are checked and V refuses it. With `-assumevalid`
    /// at the last of 8,065 later headers they are skipped, and b101 connects: 8,065 headers are
    /// over two weeks of proof-equivalent time at regtest's 150 s spacing and constant work
    /// (8,064 is exactly two weeks, which checks). Regtest's `-checkblockindex` runs at each
    /// header, as dashd's does, so the headers take most of the test's time.
    // Core: validation.cpp:2542-2571
    #[test]
    fn assumevalid_reaches_the_connector() {
        const AHEAD: u32 = 8_065;
        let dir = tempfile::tempdir().unwrap();
        let checked = sut(dir.path(), &["-assumevalid=0"]);
        let p = checked.params.chain.clone();
        let genesis = dashd_chainstate::chain::genesis_block(&p);
        let t0 = genesis.header().time();
        // OP_0: an empty stack top, so the spend fails EvalScript's result.
        let mut blocks = vec![mine(&p, genesis.header(), 1, t0 + 1, &[(0, &[0x00])], vec![])];
        for h in 2..=100 {
            let prev = blocks.last().unwrap().header();
            blocks.push(mine(&p, prev, h, t0 + h, &[(0, &[0x51])], vec![]));
        }
        let spend = tx(
            vec![TxIn {
                prevout: OutPoint {
                    hash: blocks[0].txs()[0].txid(),
                    index: 0,
                },
                script_sig: Vec::new().into(),
                sequence: u32::MAX,
            }],
            &[(0, &[0x51])],
        );
        let b101 = mine(&p, blocks[99].header(), 101, t0 + 101, &[(0, &[0x51])], vec![spend]);

        // The tip once `sut` has the blocks, then `headers`, then b101.
        let connect_b101 = |sut: &NodeSut, headers: &[dash_consensus::primitives::Header]| {
            for b in &blocks {
                submit(sut, b);
            }
            send_headers(sut, headers);
            let v = sut.chain().unwrap();
            assert!(v.process_block(Arc::clone(&b101), true, None, true).recv().is_ok());
            v.tip_watch().get()
        };
        // The spend fails its scripts once they are checked, whatever headers follow it.
        assert_eq!(connect_b101(&checked, &[]), Some(blocks[99].hash()), "checked");

        let mut headers = vec![*b101.header()];
        for i in 1..=AHEAD {
            let prev = headers.last().unwrap().hash();
            headers.push(grind(&p, regtest_header(prev, t0 + 101 + i)));
        }
        let dir = tempfile::tempdir().unwrap();
        let at_best = format!("-assumevalid={}", headers.last().unwrap().hash());
        let skipped = sut(dir.path(), &[&at_best]);
        assert_eq!(connect_b101(&skipped, &headers), Some(b101.hash()), "skipped");
    }

    /// Core's evodb with no blocks (`node/chainstate.cpp:471-473`, CH7-notes D71, CH7a-4a r1
    /// m-2): a row in an evo column family with no coins tip, here after a kill before the first
    /// flush, refuses the load with Core's text and its reindex question. The twin of the restart
    /// below, which keeps such rows at a tip.
    #[test]
    fn evo_rows_without_a_tip_are_cores_evodb_without_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let (ui, lines) = logged_ui();
        let mut sut = try_sut(dir.path(), &[], ui).unwrap_or_else(|_| panic!("accepted"));
        let cf = CHAINSTATE_SCHEMA.cf("mn_diff").unwrap();
        let mut put = sut.coins.batch();
        put.put(cf, b"stray", b"row");
        sut.coins.write(put, Commit::Durable).unwrap();
        lines.lock().unwrap().clear();
        assert_eq!(sut.restart(Teardown::Kill), 0);
        let down = sut.down().expect("refused");
        assert!(down.starts_with("Error initializing block database"), "{down}");
        assert!(down.ends_with("Please restart with -reindex or -reindex-chainstate to recover."));
        // Past `InitCoinsCache` in Core: both flushes, with a null best block, so no timer pair
        // (BL-636).
        assert_eq!(sut.flushes(), 2, "the refused load's own flushes");
        let lines = lines.lock().unwrap();
        assert_eq!(bl636::pairs(&lines), (0, 0));
        assert!(lines.iter().any(|l| l.contains("no tip, but `mn_diff` holds records")));
    }

    /// CH7 §6.3 steps 3 and 6 below DIP0003: on a restart at a stored tip, `loadblk`'s
    /// `InitializeCurrentBlockTip` sets the DMN tip to it, where the list is Core's empty one; the
    /// parent-block list is that one too. Rows in the evo column families at a tip below DIP0003,
    /// as an undo back below it keeps (CH7 D6), do not stop the load: Core checks the evodb only
    /// when there are no blocks (BL-539).
    // Core: init.cpp:2455-2473, evo/deterministicmns.cpp:941-947, node/chainstate.cpp:471-473
    #[test]
    fn a_restart_below_dip3_sets_the_dmn_tip_and_keeps_the_evo_rows() {
        use dash_types::Numeric as _;

        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        imported(&mut sut);
        let lists = || {
            let read = |q: &ChainQuery<'_>| {
                let e = q.evo_view();
                let (tip, dmn) = (e.tip_list().map(|l| *l.block_hash()), e.dmn_list());
                (e.tip, e.dmn_tip, tip.ok(), dmn.ok().map(|l| *l.block_hash()))
            };
            sut.chain().unwrap().query(read).recv().unwrap()
        };
        let start = std::time::Instant::now();
        let (tip, dmn_tip, tip_list, dmn_list) = loop {
            let read = lists();
            if read.1.is_some() {
                break read;
            }
            assert!(start.elapsed().as_secs() < 60, "init's command ran");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!((dmn_tip, tip.is_some()), (tip, true), "the DMN tip is V's tip");
        assert_eq!(view_tip(&sut), b2.hash());
        let view = sut.chain().unwrap().view_handle().view();
        assert_eq!(view.dmn_tip, dmn_tip, "published");
        assert_eq!(*view.mn_list.block_hash(), BlockHash::ZERO);
        assert!(view.credit_pool.is_none(), "below V20");
        assert_eq!((tip_list, dmn_list), (Some(BlockHash::ZERO), Some(BlockHash::ZERO)));

        let cf = CHAINSTATE_SCHEMA.cf("mn_diff").unwrap();
        let mut put = sut.coins.batch();
        put.put(cf, b"stray", b"row");
        sut.coins.write(put, Commit::Durable).unwrap();
        assert_eq!(sut.restart(Teardown::Clean), 2);
        loaded(&sut);
        assert_eq!(view_tip(&sut), b2.hash());
    }

    /// Records each `InitializeCurrentBlockTip`'s `ibd`.
    #[derive(Default)]
    struct InitIbd(std::sync::Mutex<Vec<bool>>);

    impl ValidationInterface for InitIbd {
        fn initialize_current_block_tip(&self, _: Option<dash_consensus::BlockId>, ibd: bool) {
            self.0.lock().unwrap().push(ibd);
        }
    }

    /// D62 on the node path: loadblk clears `fImporting` after startup's ABC and only then sends
    /// `InitializeCurrentBlockTip`, so at a recent tip its listeners hear `ibd == false`, as
    /// Core's `CDSNotificationInterface` passes it to mn-sync.
    // Core: node/blockstorage.cpp:947-961, init.cpp:2460-2473, dsnotificationinterface.cpp:31-35
    #[test]
    fn init_tells_the_listeners_a_recent_tip_is_out_of_ibd() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            ..
        } = left_ibd_at_b1(dir.path());
        let heard = Arc::new(InitIbd::default());
        let listener = Arc::clone(&heard);
        sut.set_listener(Some(Arc::new(move |_: &dash_chain::ChainReader| {
            Arc::clone(&listener) as Arc<dyn ValidationInterface>
        })));
        sut.restart(Teardown::Clean);
        loaded(&sut);
        imported(&mut sut);
        let start = std::time::Instant::now();
        while heard.0.lock().unwrap().is_empty() {
            assert!(start.elapsed().as_secs() < 60, "init's command ran");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(*heard.0.lock().unwrap(), [false]);
    }
}
