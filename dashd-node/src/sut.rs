//! `NodeSut`: the chainstate O2b's comparator drives, on the node's own startup and shutdown
//! steps ([`ChainSteps`]) over an in-memory coins database (ND1-D10, CH3 D24, O2b D32/D34). Its
//! lifecycle only: CH3a's a10 adds the ops, the reports and the images on top.

use std::path::Path;
use std::sync::Arc;

use dash_args::ArgsManager;
use dash_clock::MockClock;
use dash_consensus::primitives::BlockHash;
use dash_params::ChainParams;
use dashd_chainstate::chain::{Blocking, ChainHandle, ChainQuery, ValidationInterface};
use dashd_chainstate::coins::{OutPointKey, PackedCoin};
use dashd_chainstate::reorg::PoolsOnly;
use dashd_chainstate::store::{CHAINSTATE_SCHEMA, CoinsReadError};
use dashd_masternode::sporks::SporkManager;
use dashd_store::flatfile::OsFs;
use dashd_store::{
    Cf, Commit, KvEngine, KvRead, MemoryBatch, MemoryEngine, MemorySnapshot, Result as KvResult,
};
#[cfg(test)]
use dashd_store::{ErrorKind, StoreError};

use crate::InitFailure;
use crate::abort::NodeAbort;
use crate::chainstate::{OpenError, Opening, reindex_question};
use crate::init::caches::{db_batch_size, init_validation_caches, start_script_check_threads};
use crate::init::{self, ParamInteraction, SelectedParams, setup_server_args};
use crate::node::coins_configuration;
use crate::shutdown::ShutdownState;
use crate::steps::ChainSteps;
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
}

/// The session's files: the OS's, whose free space a test can take away.
#[cfg(not(test))]
type SutFs = OsFs;
#[cfg(test)]
type SutFs = LowSpaceFs;

/// The OS's files, with no free space while `low` is set: the flush's and a block write's
/// `CheckDiskSpace` fail.
#[cfg(test)]
struct LowSpaceFs {
    os: OsFs,
    low: Arc<std::sync::atomic::AtomicBool>,
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
        if self.low.load(std::sync::atomic::Ordering::SeqCst) {
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
        self.coins.iter_prefix(cf, prefix)
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
        if let Some(kind) = self.sync_fault {
            return Err(StoreError::engine("memory", kind, "injected"));
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
    /// The bug every later open's connector arms (xcheck's positive controls).
    #[cfg(feature = "planted-bugs")]
    planted: Option<dashd_chainstate::Planted>,
    /// While set, every open's writes fail.
    #[cfg(test)]
    write_fault: Arc<std::sync::atomic::AtomicBool>,
    /// While set, every open's free disk space reads as none.
    #[cfg(test)]
    low_space: Arc<std::sync::atomic::AtomicBool>,
    /// Every later open's fault around V's block files.
    #[cfg(test)]
    wrap_files: Option<crate::steps::WrapFiles>,
    /// Every later open's call on V just before init's `VerifyLoaded`.
    #[cfg(test)]
    before_verify: Option<crate::steps::BeforeVerify>,
    /// This open's shutdown flags, as a process's.
    shutdown: Arc<ShutdownState>,
    /// This open's warnings, as a process's.
    warnings: Arc<Warnings>,
    steps: ChainSteps<SharedMemory, SutFs>,
    /// The refused load's message: the production shutdown ran and nothing is loaded (F6).
    down: Option<String>,
    /// The handle on every open's disconnect pools (DEC-500): its count spans the opens, which
    /// `steps` (rebuilt per open) cannot keep.
    pools: PoolsOnly,
}

/// Why a load was refused, with Core's message.
struct Refusal {
    message: String,
    /// `-dbcrashratio` came up: Core's `_Exit(0)` runs no `Shutdown`.
    crashed: bool,
}

impl From<InitFailure> for Refusal {
    fn from(failure: InitFailure) -> Refusal {
        let message = match failure {
            InitFailure::Error(message) | InitFailure::Exception(message) => message,
            InitFailure::Reported => {
                debug_assert!(false, "no chainstate step reports its own failure");
                String::new()
            }
        };
        Refusal {
            message,
            crashed: false,
        }
    }
}

// Core: init.cpp:2137, :2578
const SHUTDOWN_REQUESTED: &str = "Shutdown requested. Exiting.";

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
        Self::with_coins(dir, argv, mocktime, ui, Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA)))
    }

    /// [`new`](Self::new) over `coins`: a session with other arguments on a datadir and coins an
    /// earlier one left (its teardown done and `coins` restarted), as a node restarted with new
    /// arguments.
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
            wrap_files: None,
            #[cfg(test)]
            before_verify: None,
            steps: ChainSteps::new(logger, Arc::clone(&shutdown), Arc::clone(&warnings)),
            shutdown,
            warnings,
            down: None,
            pools: PoolsOnly::default(),
        })
    }

    /// Ends this process the way `how` says, then starts the next one on the same files and
    /// clock. Returns the teardown's flushes: none while down, whose teardown ran at the load
    /// (its flushes are [`flushes`](Self::flushes) until this restart).
    pub fn restart(&mut self, how: Teardown) -> u32 {
        let before = self.steps.flushes();
        match how {
            Teardown::Clean => self.steps.shutdown(),
            Teardown::Kill => self.steps.kill(),
        }
        let flushes = self.steps.flushes() - before;
        self.coins.restart();
        self.open();
        flushes
    }

    /// Steps 7a to 11 on fresh shutdown flags. A refusal runs the production shutdown over what
    /// loaded, or a kill's after a simulated crash.
    fn open(&mut self) {
        self.shutdown = Arc::new(ShutdownState::new());
        self.warnings = Arc::new(Warnings::new());
        self.steps = ChainSteps::new(
            Arc::clone(self.ui.logger()),
            Arc::clone(&self.shutdown),
            Arc::clone(&self.warnings),
        );
        self.steps.pools = Some(self.pools.clone());
        #[cfg(test)]
        {
            self.steps.wrap_files = self.wrap_files;
            self.steps.before_verify = self.before_verify;
        }
        self.down = self.load().err().map(|refusal| {
            if refusal.crashed {
                self.steps.kill();
            } else {
                self.steps.shutdown();
            }
            refusal.message
        });
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
            caches,
            queue,
            Arc::clone(self.steps.metaman().expect("made at step 6")),
        )?;
        let batch_size = db_batch_size(args)?;
        #[cfg(feature = "planted-bugs")]
        let setup = crate::chainstate::CoinsSetup {
            planted: self.planted,
            ..setup
        };
        #[cfg(test)]
        let fs = Arc::new(LowSpaceFs {
            os: OsFs::new(),
            low: Arc::clone(&self.low_space),
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
                    Arc::clone(logger),
                    abort,
                )
            },
        )?;
        let message = match opened {
            Ok(()) => None,
            Err(OpenError::Crashed) => {
                return Err(Refusal {
                    message: "Simulating a crash. Goodbye.".to_owned(),
                    crashed: true,
                });
            }
            Err(OpenError::Incompatible(message)) => Some(message),
            Err(OpenError::Interrupted) => Some(SHUTDOWN_REQUESTED.to_owned()),
            Err(OpenError::Recoverable {
                message,
                cause,
            }) => {
                if let Some(cause) = cause {
                    logger.log_printf(&format!("{cause}\n"));
                }
                Some(if self.shutdown.shutdown_requested() {
                    SHUTDOWN_REQUESTED.to_owned()
                } else {
                    reindex_question(&message)
                })
            }
        };
        if let Some(message) = message {
            return Err(InitFailure::Error(message).into());
        }
        self.steps.load_net_caches(args, &self.params)?;
        self.steps.import()?;
        if self.shutdown.shutdown_requested() {
            return Err(InitFailure::Error(SHUTDOWN_REQUESTED.to_owned()).into());
        }
        Ok(())
    }

    /// The refused load's message while nothing is loaded.
    pub fn down(&self) -> Option<&str> {
        self.down.as_deref()
    }

    /// V, while loaded.
    pub fn chain(&self) -> Option<&ChainHandle> {
        self.steps.chain().map(|chain| chain.handle())
    }

    /// The spork manager, while loaded.
    pub fn sporks(&self) -> Option<&Arc<SporkManager>> {
        self.steps.sporks()
    }

    /// The chain parameters init built from the command line (O2b D8's parity check reads them).
    pub fn params(&self) -> &ChainParams {
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

    /// Registers `listener` on V's validation signals; `false` while nothing is loaded. O2b's
    /// event plane listens here. Scaffolding until CH3a's a10 gives the SUT its own event
    /// capture.
    pub fn register_listener(&self, listener: Arc<dyn ValidationInterface>) -> bool {
        self.steps.chain().map(|chain| chain.signals().register(listener)).is_some()
    }

    /// `SyncWithValidationInterfaceQueue` on V's signals, from a thread that may wait: whether
    /// the queue's thread reached the barrier. O2b's event plane syncs before it reads what its
    /// listener heard (scaffolding until a10, as [`register_listener`](Self::register_listener)).
    pub fn sync_signals(&self) -> bool {
        self.steps.chain().is_some_and(|chain| chain.signals().sync(&mut Blocking).is_ok())
    }

    /// The flushes since the last load (its own teardown's, while down).
    pub fn flushes(&self) -> u32 {
        self.steps.flushes()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use dash_consensus::header::MAX_FUTURE_BLOCK_TIME;
    use dash_consensus::primitives::{OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxType};
    use dash_evo::spork::SporkId;
    use dashd_chainstate::chain::verify::FUTURE_TIP;
    use dashd_store::KvBatch as _;

    use super::*;
    use crate::chain::tests::NoRoomAtOne;
    use crate::logging::Logger;
    use crate::steps::Point;

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
        let mut sut = sut(dir.path(), &[]);
        let cf = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let mut put = sut.coins.batch();
        put.put(cf, b"coin", b"unspent");
        sut.coins.write(put, Commit::Buffered).unwrap();
        sut.coins.inject_failure(ErrorKind::NoSpace);
        assert_eq!(sut.restart(Teardown::Kill), 0);
        let down = sut.down().expect("refused");
        assert!(down.contains("no tip, but `coins` holds records"), "{down}");
        assert!(down.ends_with("Please restart with -reindex or -reindex-chainstate to recover."));

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
        p: &dash_params::ChainParams,
        prev: &dash_consensus::primitives::Header,
        height: u8,
        time: u32,
    ) -> Arc<dash_consensus::Block> {
        regtest_block_paying(p, prev, height, time, 0)
    }

    /// [`regtest_block`] whose coinbase pays `value` duffs.
    fn regtest_block_paying(
        p: &dash_params::ChainParams,
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
        p: &dash_params::ChainParams,
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

    /// `raw` with the first nonce that meets the regtest target.
    fn grind(
        p: &dash_params::ChainParams,
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
        let (ui, lines) = logged_ui();
        let sut = try_sut(dir, &["-debug=all"], ui).unwrap_or_else(|_| panic!("accepted"));
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
        imported(&sut);
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
            imported(&sut);
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

    /// Init's wait ends at genesis, before `loadblk` clears `fImporting`.
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
        let failed = format!("ForceFlushStateToDisk: failed to flush state ({INJECTED_SYSTEM})\n");
        let report = [&write_failure()[..], &[failed]].concat();
        let lines = lines.lock().unwrap();
        let got = failure_lines(&lines, &report);
        let want: Vec<&str> = report.iter().map(String::as_str).cycle().take(5 * times).collect();
        assert_eq!(got, want, "{lines:?}");
    }

    /// The injected write's engine text, and W's `AbortNode` message for it.
    const INJECTED: &str = "memory error (Io): injected write";
    const INJECTED_SYSTEM: &str = "System error while flushing: memory error (Io): injected write";

    /// What a write W fails logs, in Core's order: `HandleError`'s two lines, then the
    /// `AbortNode`'s log line and user error.
    fn write_failure() -> [String; 4] {
        [
            format!("{INJECTED}\n"),
            "You can use -debug=leveldb to get more complete diagnostic messages\n".to_owned(),
            format!("*** {INJECTED_SYSTEM}\n"),
            "Error: A fatal internal error occurred, see debug.log for details\n".to_owned(),
        ]
    }

    /// Every line of `lines` about a write failure or an `AbortNode`, each as the line of `report`
    /// it ends with (the longest: the `***` line ends with the engine's text too), or whole.
    fn failure_lines<'a>(lines: &'a [String], report: &'a [String]) -> Vec<&'a str> {
        let kinds = [INJECTED, "-debug=leveldb", "*** ", "Error: ", "failed to flush state"];
        (lines.iter())
            .filter(|l| kinds.iter().any(|k| l.contains(k)))
            .map(|l| {
                let ends = report.iter().filter(|w| l.ends_with(w.as_str()));
                ends.max_by_key(|w| w.len()).map_or(l.as_str(), |w| w)
            })
            .collect()
    }

    /// BL-485: a write W fails outside shutdown (V's ALWAYS flush on unflushed state) logs Core's
    /// `HandleError` lines before W's `AbortNode`, as the shutdown flushes' failures do.
    // Core: dbwrapper.cpp:261-268; validation.cpp:3121-3123; shutdown.cpp:27-37
    #[test]
    fn a_failed_write_in_a_flush_logs_cores_handle_error_lines() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            sut,
            lines,
            ..
        } = left_ibd_at_b1(dir.path());
        lines.lock().unwrap().clear();
        sut.write_fault.store(true, Ordering::SeqCst);
        let always = dashd_chainstate::flush::FlushStateMode::Always;
        let _ = sut.chain().unwrap().flush(always).recv();
        wait_for("AbortNode ran", || sut.shutdown.aborted());
        sut.write_fault.store(false, Ordering::SeqCst);
        let report = write_failure();
        let lines = lines.lock().unwrap();
        assert_eq!(
            failure_lines(&lines, &report),
            report.each_ref().map(String::as_str),
            "{lines:?}"
        );
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
        let abort = "*** Disk space is too low!\n";
        let error = "Error: Disk space is too low!\n";
        let failed = "ForceFlushStateToDisk: failed to flush state (Disk space is too low!)\n";
        let lines = lines.lock().unwrap();
        // Every `AbortNode`, user error and failure line of the shutdown, in order.
        let got: Vec<&str> = lines
            .iter()
            .filter(|l| ["*** ", "Error: ", "failed to flush state"].iter().any(|w| l.contains(w)))
            .map(|l| [abort, error, failed].into_iter().find(|w| l.ends_with(w)).unwrap_or(l))
            .collect();
        assert_eq!(got, [abort, error, failed, abort, error, failed], "{lines:?}");
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

    /// Waits for `loadblk`'s import to end.
    fn imported(sut: &NodeSut) {
        let start = std::time::Instant::now();
        while sut.chain().unwrap().view_handle().loading_blocks() {
            assert!(start.elapsed().as_secs() < 60, "the import ended");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
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
        imported(&sut);
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
        imported(&sut);
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
    /// load's teardown runs both flushes: the coins cache exists by then.
    // Core: node/chainstate.cpp:272-277; validation.cpp:4992-5018, :2969 (CanFlushToDisk)
    #[test]
    fn an_invalid_stored_tip_fails_load_chain_tip_after_the_coins() {
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
        imported(&sut);
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

    /// The readiness guard at startup (CH3a D70, 2b-D32): a stored tip this build's incomplete evo
    /// hooks cannot judge, here b2, which shutdown's flush stored, because the restart's
    /// `-dip3params` moves DIP0003 to height 1, refuses the load after `LoadChainTip` with the
    /// connector's refusal text, a plain init error. The coins cache exists by then, so the
    /// teardown runs both flushes.
    #[test]
    fn a_stored_tip_the_build_cannot_judge_is_refused_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b2,
            ..
        } = left_ibd_at_b1(dir.path());
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        coins.restart();
        let argv = ["-regtest", "-printtoconsole=0", "-dip3params=1:1"];
        let sut = NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui(), coins).expect("accepted");
        let down = sut.down().expect("refused");
        let refusal = format!("rdashd cannot validate block {} at height 2 on regtest", b2.hash());
        assert!(down.starts_with(&refusal) && down.contains("DIP0003"), "{down}");
        assert!(sut.chain().is_none());
        assert_eq!(sut.flushes(), 2, "after the coins cache");
    }

    /// The readiness guard checks the blocks recovery replayed, not the tip alone (review r2 m2):
    /// a split flush's marker for b2 over null replays b1 and b2, and under `-dip3params=1:1` the
    /// refusal names b1, the first block replayed, where the tip's check would name b2.
    #[test]
    fn the_blocks_recovery_replays_are_checked_for_readiness() {
        let dir = tempfile::tempdir().unwrap();
        let LeftIbd {
            mut sut,
            b1,
            b2,
            ..
        } = left_ibd_at(dir.path(), 2);
        sut.steps.shutdown();
        let coins = Arc::clone(&sut.coins);
        drop(sut);
        // As a first flush interrupted after its marker (`old` null): the coins' tip is erased.
        let mut marker = coins.batch();
        marker.delete(meta_cf(), b"tip");
        marker.put(
            meta_cf(),
            b"head_blocks",
            &[b2.hash().as_bytes().as_slice(), &[0; 32]].concat(),
        );
        coins.write(marker, Commit::Durable).unwrap();
        coins.restart();
        let argv = ["-regtest", "-printtoconsole=0", "-dip3params=1:1"];
        let sut = NodeSut::with_coins(dir.path(), &argv, MOCKTIME, ui(), coins).expect("accepted");
        let down = sut.down().expect("refused");
        let refusal = format!("rdashd cannot validate block {} at height 1 on regtest", b1.hash());
        assert!(down.starts_with(&refusal) && down.contains("DIP0003"), "{down}");
        assert!(sut.chain().is_none());
        assert_eq!(stored_tip(&sut), Some(b2.hash()), "recovery stored its replay to b2");
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
            let sut = try_sut(dir.path(), &full, ui).unwrap();
            let p = sut.params.chain.clone();
            let start = std::time::Instant::now();
            while sut.chain().unwrap().view_handle().loading_blocks() {
                assert!(start.elapsed().as_secs() < 60, "the import ended");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
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
            while stored_tip(&sut) != Some(tip.hash()) {
                assert!(start.elapsed().as_secs() < 60, "{argv:?}: LEFT_IBD's flush landed");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
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
            let v = sut.chain().unwrap();
            for chunk in headers.chunks(2_000) {
                let batch = dash_chain::HeaderBatch {
                    hashes: chunk.iter().map(|h| h.hash()).collect(),
                    headers: chunk.to_vec(),
                };
                let r = v.process_headers(batch, None).recv().expect("V replies");
                assert!(r.stop.is_none(), "{:?}", r.stop);
            }
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
}
