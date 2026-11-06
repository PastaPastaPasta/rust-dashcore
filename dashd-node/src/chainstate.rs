//! The chainstate as init and shutdown see it: CH2's RocksDB chainstate opened and recovered
//! (DEC-201 Q7), with no chain on top until CH1-node composes one (C11). `NodeSut` opens the same
//! chainstate on its own engine and block files (ND1-D10).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dash_args::{ArgsManager, RuntimeError};
use dash_chain::IndexAccess;
use dash_consensus::BlsCtx;
use dash_consensus::ctx::bls_ctx_for_block;
use dash_consensus::pow::PowParams;
use dash_consensus::primitives::BlockHash;
use dash_evo::types::ProTxHash;
use dash_params::{ChainSetup, Network};
use dashd_chainstate::caches::CacheSizes;
use dashd_chainstate::chain::verify::CORRUPTED_BLOCK_DB;
use dashd_chainstate::chain::{
    AbortPath, AttachCoins, Fatal, INIT_BLOCK_DB_FAILED, LOAD_BLOCK_DB_FAILED, LoadFailure,
    OPEN_BLOCK_DB_FAILED, PRUNED_DATADIR, ValidationSignals,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use dashd_chainstate::connect::{ConnectorParts, CoreConnector, NodeHelper};
use dashd_chainstate::evo::RocksEvoStore;
use dashd_chainstate::evo::cl::node_cl_verifier;
use dashd_chainstate::evo::effects::{
    EffectAppliers, FacadeMineable, MnListSignal, PlatformBanApplier, StoreSnapshots,
    UnreachableIsLock,
};
use dashd_chainstate::evo::gates::{NodeEv1Checks, node_ev1_checks};
use dashd_chainstate::evo::node::{
    CommittedMinedViews, FileBlocks, NodeEvo, NodeEvoParts, SharedEvoLayer, node_codec,
    node_net_info,
};
use dashd_chainstate::flush::{
    Abort, CrashRoll, EffectLedger, FlushConfig, FlushObserver, FlushPolicy, Kept, LogCat,
    NoInputs, NoPrune, NodeFlush, NodeFlushParts, ObsMark, SubscriberHub, ThreadDriver, WThread,
    check_disk_space, empty_coins_commit, index_timed, kept, kept_site, log_empty_evodb_commit,
    log_empty_files_flush,
};
use dashd_chainstate::llmq::facade::MembersFacade;
use dashd_chainstate::llmq::rows::RuntimeRows;
use dashd_chainstate::recovery::{
    ChainstateOpen, InitError, Opened, RecoveredChainstate, RecoveryConfig,
};
use dashd_chainstate::sigcache::ScriptCaches;
use dashd_chainstate::store::sites::FLUSH_SITE;
use dashd_chainstate::store::{
    AbortSink, GlobalFailStop, GuardedEngine, GuardedSnapshot, NodeBlockFiles,
};
use dashd_chainstate::tuning::{self, EngineTuning, RocksTuning};
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_store::flatfile::{BlockFilesConfig, FlatFs, OsFs};
use dashd_store::{ErrorKind, KvEngine, SchemaState};

use crate::logging::{Level, LogFlags, Logger};

// Core: init.cpp:2037
const OPEN_FAILED: &str = "Error opening block database";

/// The chainstate directory under the network's data directory, where Core's functional tests
/// expect the consensus database.
pub const CHAINSTATE_DIR: &str = "chainstate";

/// CH2's flush and recovery marks: the log lines go to the debug log, the rest is CH2b's
/// driver's.
struct LogObserver {
    logger: Arc<Logger>,
}

impl FlushObserver for LogObserver {
    fn mark(&self, m: ObsMark) {
        let ObsMark::Log {
            cat,
            line,
        } = m
        else {
            return;
        };
        let line = format!("{line}\n");
        let (category, level) = match cat {
            LogCat::Always => (LogFlags::ALL, Level::Info),
            LogCat::Error => (LogFlags::ALL, Level::Error),
            LogCat::CoinDb => (LogFlags::COINDB, Level::Debug),
            LogCat::Bench => (LogFlags::BENCHMARK, Level::Debug),
            LogCat::Prune => (LogFlags::PRUNE, Level::Debug),
        };
        if category == LogFlags::ALL || self.logger.will_log_category_level(category, level) {
            self.logger.log_print_str(&line, "", "", 0, category, level);
        }
    }
}

/// The opened chainstate: the node's is RocksDB over the OS's files. Dropping it closes the
/// database.
pub struct Chainstate<E: KvEngine = Engine, F: FlatFs = OsFs> {
    recovered: RecoveredChainstate<F>,
    // Dropped last: the block files hold the engine's latch.
    engine: Arc<GuardedEngine<E>>,
    fs: Arc<F>,
    abort: Arc<dyn AbortSink>,
    blocks_dir: PathBuf,
    data_dir: PathBuf,
    sub_batch: u64,
    coins_share: u64,
    logger: Arc<Logger>,
    /// The node's engine's switch for [`fail_next_wal_sync`](Self::fail_next_wal_sync); `None`
    /// on another engine.
    #[cfg(any(test, feature = "test-hooks"))]
    wal_fault: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The address of the subscriber hub recovery ran on.
    #[cfg(all(test, feature = "test-support"))]
    recovered_on: Option<usize>,
    /// The planted bug [`attach_parts`](Self::attach_parts) arms in the coins cache too.
    #[cfg(feature = "planted-bugs")]
    plant: Option<dashd_chainstate::Planted>,
    /// The coins cache [`attach_parts`](Self::attach_parts) gave V, set once from `&self` so the
    /// attach keeps its signature: `gettxout`'s read on V (RP1a D35), and the tests'.
    coins_cell: std::sync::OnceLock<Arc<Mutex<CoinsCache<E>>>>,
    /// What reached `-dbcrashratio`'s two rolls and the flush policy.
    #[cfg(test)]
    crash_record: CrashRecord,
    /// The coins' best height `node_evo` built the initial scheme from (RP1a D68).
    #[cfg(test)]
    evo_scheme_input: std::sync::OnceLock<Option<u32>>,
}

/// Core's process flag `bls::bls_legacy_scheme` once `VerifyLoadedChainstate` has run, for a
/// start whose coins' best block is at `coins_height` (RP1a D68): the flag's initial legacy on an
/// empty coins view, which Core never stores, even where V19 is active at height 0; otherwise
/// `!DeploymentActiveAfter(tip, V19)`, the height rule after the tip. A genesis best block is not
/// empty: it takes block 1's rule.
// Core: bls/bls.cpp:18, node/chainstate.cpp:413-433
pub fn startup_bls(params: &ChainSetup, coins_height: Option<u32>) -> BlsCtx {
    match coins_height {
        None => BlsCtx::LEGACY,
        Some(height) => bls_ctx_for_block(params, height + 1),
    }
}

/// `-dbcrashratio`'s exit: Core's `_Exit(0)` after its line, from W's roll or recovery's. Only a
/// test's returns.
pub type CrashExit = Arc<dyn Fn() + Send + Sync>;

/// What reached `-dbcrashratio`'s consumers: recovery's roll, then at the attach W's roll and the
/// policy's synchronous flag, each read from the object handed over.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct CrashRecord {
    pub(crate) recovery: Option<CrashRoll>,
    pub(crate) attached: std::sync::OnceLock<(CrashRoll, bool)>,
}

/// The chainstate between its open and its recovery, with the block index loaded in between
/// from [`Opened::block_index`] (2b-D18): Core opens `blocks/index`, loads it, then opens and
/// replays the coins; here the index is in the chainstate's database, so the database opens
/// first. [`open`](Self::open) runs recovery's steps 0–3, [`recover`](Self::recover) the rest on
/// the loaded index, and [`into_chainstate`](Self::into_chainstate) keeps the result.
pub struct Opening<E: KvEngine = Engine, F: FlatFs = OsFs> {
    engine: Arc<GuardedEngine<E>>,
    fs: Arc<F>,
    abort: Arc<dyn AbortSink>,
    data_dir: PathBuf,
    coins_share: u64,
    blocks: BlockFilesConfig,
    obs: LogObserver,
    cfg: RecoveryConfig,
    #[cfg(any(test, feature = "test-hooks"))]
    wal_fault: Option<Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(all(test, feature = "test-support"))]
    recovered_on: std::cell::Cell<Option<usize>>,
    #[cfg(feature = "planted-bugs")]
    plant: Option<dashd_chainstate::Planted>,
}

/// The connector's evo hooks: CH7a's `NodeEvo`, complete since CH7a-4, so G1 is lifted; CH7b-2
/// removed G1q (CH7 D3), so they judge every block.
pub type NodeEvoHooks<E = Engine> = NodeEvo<E, NodeEv1Checks>;

/// The node's engine: RocksDB; in test builds behind a switch that fails its next WAL sync.
#[cfg(not(any(test, feature = "test-hooks")))]
pub type Engine = dashd_store::RocksEngine;
#[cfg(any(test, feature = "test-hooks"))]
pub type Engine = wal_fault::WalFault;

/// [`Opening::recover`]'s failure: init's error, and the chainstate kept for the shutdown
/// flushes after a refusal in a state where Core is past `InitCoinsCache`
/// ([`InitError::after_coins_cache`], BL-636).
type RecoverError<F> = (OpenError, Option<Box<RecoveredChainstate<F>>>);

/// Why the chainstate did not open.
#[derive(Debug)]
pub enum OpenError {
    /// Core's message for a chainstate that may load after `-reindex`; `cause` is logged first.
    Recoverable {
        message: String,
        cause: Option<String>,
    },
    /// Core's `FAILURE_INCOMPATIBLE_DB`: a plain `InitError`, no reindex question. The readiness
    /// guard's refusal at startup (CH3a D70) takes the same path.
    Incompatible(String),
    /// Core's `FAILURE_INSUFFICIENT_DBCACHE`: init handles it as
    /// [`Incompatible`](Self::Incompatible) (`init.cpp:2115-2117`); its status is its own.
    InsufficientDbcache(String),
    /// `-dbcrashratio` came up (`_Exit(0)` after its log line).
    Crashed,
    /// Core's `INTERRUPTED`: the load stopped with shutdown requested. Init asks nothing and
    /// exits as for any shutdown requested during the load. Core's message: empty for the load's
    /// own interruption (`node/chainstate.cpp:177`, `:184`, `:402`), VerifyDB's text for one
    /// during verification (`:445-446`).
    Interrupted(String),
}

/// Core's statuses for the block index load (CH1-node's [`LoadFailure`]): another chain's
/// datadir is `FAILURE_INCOMPATIBLE_DB`; a false `LoadGenesisBlock` and a pruned index loaded
/// unpruned are `FAILURE`.
// Core: node/chainstate.cpp:189-211
impl From<LoadFailure> for OpenError {
    fn from(failure: LoadFailure) -> OpenError {
        match failure {
            LoadFailure::Incompatible(message) => OpenError::Incompatible(message.to_owned()),
            LoadFailure::Failure {
                cause,
            } => OpenError::Recoverable {
                message: INIT_BLOCK_DB_FAILED.to_owned(),
                cause,
            },
            LoadFailure::LoadingBlockDb => OpenError::Recoverable {
                message: LOAD_BLOCK_DB_FAILED.to_owned(),
                cause: None,
            },
            LoadFailure::Store {
                cause,
            } => OpenError::Recoverable {
                message: OPEN_BLOCK_DB_FAILED.to_owned(),
                cause: Some(cause),
            },
            LoadFailure::CorruptedBlockDb {
                cause,
            } => OpenError::Recoverable {
                message: CORRUPTED_BLOCK_DB.to_owned(),
                cause: Some(cause),
            },
            LoadFailure::Interrupted => OpenError::Interrupted(String::new()),
            LoadFailure::NotReady(refusal) => OpenError::Incompatible(refusal),
            LoadFailure::PrunedDatadir => OpenError::Recoverable {
                message: PRUNED_DATADIR.to_owned(),
                cause: None,
            },
        }
    }
}

/// Core's reindex question for a load it may recover from (non-interactive: the answer is no).
// Core: init.cpp:2119-2133
pub fn reindex_question(message: &str) -> String {
    format!("{message}.\nPlease restart with -reindex or -reindex-chainstate to recover.")
}

// Core: node/chainstate.cpp:244-247
const NEEDS_UPGRADE: &str = "Unsupported chainstate database format found. Please restart with \
                             -reindex-chainstate. This will rebuild the chainstate database.";

impl Opening {
    /// Core's `LoadChainstate` from its first database: opens `<datadir>/chainstate`, which holds
    /// the block index too, and the block files. `crash_ratio` is `-dbcrashratio`; `tuning` is
    /// the network's [`chainstate_profile`].
    // Core: node/chainstate.cpp:166-171, :220-247
    #[allow(clippy::too_many_arguments)]
    pub fn open_node(
        datadir: &Path,
        blocks_dir: &Path,
        chain: &ChainSetup,
        sizes: &CacheSizes,
        db_batch_size: u64,
        crash_ratio: u64,
        tuning: &RocksTuning,
        logger: Arc<Logger>,
        abort: Arc<dyn AbortSink>,
    ) -> Result<Opening, OpenError> {
        let path = datadir.join(CHAINSTATE_DIR);
        #[cfg(any(test, feature = "test-hooks"))]
        let mut wal_fault = None;
        let rocks = |sub_batch| {
            let (inner, schema) = tuning
                .open_chainstate(&path, sizes, sub_batch)
                .map_err(|e| store_failed(e.to_string()))?;
            if let SchemaState::Older(_) = schema {
                return Err(OpenError::Incompatible(NEEDS_UPGRADE.to_owned()));
            }
            let inner = Engine::from(inner);
            #[cfg(any(test, feature = "test-hooks"))]
            {
                wal_fault = Some(inner.switch());
            }
            Ok(inner)
        };
        let opening = Opening::on(
            rocks,
            Arc::new(OsFs::new()),
            datadir,
            blocks_dir,
            chain,
            sizes,
            db_batch_size,
            crash_ratio,
            logger,
            abort,
        );
        #[cfg(any(test, feature = "test-hooks"))]
        let opening = opening.map(|o| Opening {
            wal_fault,
            ..o
        });
        opening
    }
}

impl Chainstate {
    /// The engine, for CH5-5's live-copy seams.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn engine(&self) -> &Arc<GuardedEngine<Engine>> {
        &self.engine
    }

    /// Makes the next WAL sync fail with an I/O error, as a full or failing disk would.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn fail_next_wal_sync(&self) {
        let switch = self.wal_fault.as_ref().expect("the node's engine");
        switch.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The failed WAL sync `e` as Core's failed synced write: `HandleError`'s two lines, logged here,
/// and the flush's `AbortNode`, which the caller reports.
// Core: dbwrapper.cpp:265-268; validation.cpp:3127-3129
pub(crate) fn failed_sync(logger: &Logger, e: &str) -> Abort {
    logger.log_printf(&format!("{e}\n"));
    logger.log_printf("You can use -debug=leveldb to get more complete diagnostic messages\n");
    Abort {
        log: format!("System error while flushing: {e}"),
        user: None,
    }
}

/// Core's message for a store that would not open, with its cause.
fn store_failed(cause: String) -> OpenError {
    OpenError::Recoverable {
        message: OPEN_FAILED.to_owned(),
        cause: Some(cause),
    }
}

/// ST3b's plant build (feature `planted-bugs`; ST3b plan §3.7): `RDASHD_ST3B_PLANT` names the
/// planted bug (`T1`, `T18`, …) armed in the chainstate's guard and coins cache, read once as the
/// chainstate opens. A name that is no planted bug refuses the start.
#[cfg(feature = "planted-bugs")]
fn st3b_plant() -> Result<Option<dashd_chainstate::Planted>, OpenError> {
    let Some(name) = std::env::var_os("RDASHD_ST3B_PLANT") else {
        return Ok(None);
    };
    let name = name.to_string_lossy();
    name.parse().map(Some).map_err(|()| {
        OpenError::Incompatible(format!("RDASHD_ST3B_PLANT={name} names no planted bug"))
    })
}

impl<E: KvEngine, F: FlatFs> Opening<E, F> {
    /// [`Opening::open_node`] on the engine `engine` opens for the sub-batch size it is given, and
    /// the block files in `blocks_dir` on `fs`. `db_batch_size` is `-dbbatchsize`, and
    /// `crash_ratio` `-dbcrashratio`, which arms recovery's roll.
    // Prototype (2b-D24): 2b-2 proper bundles the node-side paths.
    #[allow(clippy::too_many_arguments)]
    pub fn on(
        engine: impl FnOnce(u64) -> Result<E, OpenError>,
        fs: Arc<F>,
        data_dir: &Path,
        blocks_dir: &Path,
        chain: &ChainSetup,
        sizes: &CacheSizes,
        db_batch_size: u64,
        crash_ratio: u64,
        logger: Arc<Logger>,
        abort: Arc<dyn AbortSink>,
    ) -> Result<Opening<E, F>, OpenError> {
        let pow = PowParams::from_chain_params(chain).map_err(|e| store_failed(e.to_string()))?;
        let sub_batch = tuning::sub_batch(db_batch_size, sizes);
        let inner = engine(sub_batch)?;
        #[allow(unused_mut)]
        let mut guarded = GuardedEngine::new(inner, GlobalFailStop::default(), Arc::clone(&abort));
        #[cfg(feature = "planted-bugs")]
        let plant = st3b_plant()?;
        #[cfg(feature = "planted-bugs")]
        if let Some(bug) = plant {
            guarded.plant(bug);
            let line = format!("ST3b: planted bug {bug} armed\n");
            logger.log_print_str(&line, "", "", 0, LogFlags::ALL, Level::Info);
        }
        Ok(Opening {
            engine: Arc::new(guarded),
            fs,
            abort,
            data_dir: data_dir.to_path_buf(),
            coins_share: sizes.coins,
            blocks: BlockFilesConfig::new(blocks_dir, chain.message_start),
            obs: LogObserver {
                logger,
            },
            cfg: RecoveryConfig {
                pow,
                sub_batch,
                crash: CrashRoll::new(crash_ratio),
            },
            #[cfg(any(test, feature = "test-hooks"))]
            wal_fault: None,
            #[cfg(all(test, feature = "test-support"))]
            recovered_on: std::cell::Cell::new(None),
            #[cfg(feature = "planted-bugs")]
            plant,
        })
    }

    /// Recovery's steps 0–3, before the block index loads from the result's
    /// [`block_index`](Opened::block_index); step 2 wipes the chainstate for
    /// `reindex_chainstate` (`-reindex-chainstate`).
    pub fn open(&self, reindex_chainstate: bool) -> Result<Opened<'_, E, F>, OpenError> {
        let (fs, blocks) = (Arc::clone(&self.fs), self.blocks.clone());
        ChainstateOpen::open_wiping(&self.engine, fs, blocks, &self.obs, reindex_chainstate)
            .map_err(init_error)
    }

    /// The rest of recovery on the loaded block `index`, through the node's `hub`: Core's
    /// `ReplayBlocks`. Without a block index the chainstate must be empty.
    ///
    /// # Errors
    ///
    /// Init's error, with the chainstate as recovery found it after a refusal in a state where
    /// Core is past `InitCoinsCache` ([`InitError::after_coins_cache`]): no tip and nothing
    /// replayed, kept for the shutdown flushes alone (BL-636).
    // Core: node/chainstate.cpp:249-253
    pub fn recover(
        &self,
        opened: Opened<'_, E, F>,
        index: &dyn IndexAccess,
        hub: &SubscriberHub,
    ) -> Result<RecoveredChainstate<F>, RecoverError<F>> {
        #[cfg(all(test, feature = "test-support"))]
        self.recovered_on.set(Some(std::ptr::from_ref(hub).addr()));
        let (meta, files) = (*opened.meta(), Arc::clone(opened.files()));
        opened.recover(index, hub, self.cfg.clone()).map_err(|e| {
            let kept = e.after_coins_cache().then(|| {
                Box::new(RecoveredChainstate {
                    tip: None,
                    replayed: vec![],
                    meta,
                    files,
                })
            });
            (init_error(e), kept)
        })
    }

    /// V's block files (CH2b b4 M2) on the files steps 0–3 opened: the stored index's blk-file
    /// check, `LoadGenesisBlock`'s write, and every block V saves or reads after.
    pub fn block_files(&self, opened: &Opened<'_, E, F>) -> NodeBlockFiles<F> {
        NodeBlockFiles::new(Arc::clone(opened.files()), self.cfg.pow, Arc::new(self.abort_path()))
    }

    fn abort_path(&self) -> AbortPath {
        AbortPath::new(Arc::clone(&self.abort), self.engine.latch().clone())
    }

    /// The chainstate `recovered` left.
    pub fn into_chainstate(self, recovered: RecoveredChainstate<F>) -> Chainstate<E, F> {
        Chainstate {
            recovered,
            engine: self.engine,
            fs: self.fs,
            abort: self.abort,
            blocks_dir: self.blocks.dir.clone(),
            data_dir: self.data_dir,
            sub_batch: self.cfg.sub_batch,
            coins_share: self.coins_share,
            logger: Arc::clone(&self.obs.logger),
            #[cfg(any(test, feature = "test-hooks"))]
            wal_fault: self.wal_fault,
            #[cfg(all(test, feature = "test-support"))]
            recovered_on: self.recovered_on.get(),
            #[cfg(feature = "planted-bugs")]
            plant: self.plant,
            coins_cell: std::sync::OnceLock::new(),
            #[cfg(test)]
            crash_record: CrashRecord {
                recovery: Some(self.cfg.crash),
                ..CrashRecord::default()
            },
            #[cfg(test)]
            evo_scheme_input: std::sync::OnceLock::new(),
        }
    }
}

/// Core's init error for a failed recovery step.
fn init_error(e: InitError) -> OpenError {
    match &e {
        InitError::Crashed => OpenError::Crashed,
        // Core's `catch_exceptions` logs the exception's text.
        InitError::Store(inner) => store_failed(inner.to_string()),
        InitError::UnableToReplay {
            detail,
        }
        | InitError::EvoWithoutBlocks {
            detail,
        } => OpenError::Recoverable {
            message: e.to_string(),
            cause: Some(detail.clone()),
        },
        _ => OpenError::Recoverable {
            message: e.to_string(),
            cause: None,
        },
    }
}

/// What init gives the coins' attach beyond the chainstate.
pub struct CoinsSetup<T> {
    /// The engine's tuning (RocksDB's in the node).
    pub tuning: Arc<T>,
    /// The mempool's maximum, `-maxmempool` in bytes: the coins may use what the mempool does
    /// not (`GetCoinsCacheSizeState`). rdashd has no mempool yet, so all of it, as Core with an
    /// empty one.
    pub max_mempool: u64,
    /// The validation caches init built at step 4a.
    pub caches: Arc<ScriptCaches>,
    /// The script-check threads init started at step 4a (`-par`), none at `-par=1`.
    pub queue: Arc<CheckQueue>,
    /// `AssumedValidBlock()`, `None` when null.
    pub assume_valid: Option<BlockHash>,
    /// `-checkblockindex` resolved ([`ChainmanOptions::check_block_index`]), for V's seams.
    ///
    /// [`ChainmanOptions::check_block_index`]: crate::init::flags::ChainmanOptions::check_block_index
    pub check_block_index: bool,
    /// `-consensusxcheck` (CH3a D189, O9b): the connector compares every memo read with a direct
    /// read of the overlay, and the coins cache every prefetch hit with a database read; a
    /// difference stops V. Off by default on every network; on in
    /// `NodeSut` unless its arguments negate it.
    pub xcheck: bool,
    /// The connector bug (L1 or T12, the only ones [`CoreConnector::plant`] arms) for xcheck's
    /// positive controls, or the census control step 10's verifier arms (CH7b-3 C7:
    /// `ClCensusSuppressOnce`, `ClCensusDuplicateOnce`). Any other is refused: cache bugs are
    /// armed through `CoinsCache::plant`, which this path never calls, so they would arm nothing.
    #[cfg(feature = "planted-bugs")]
    pub planted: Option<dashd_chainstate::Planted>,
    /// The flush policy's regime (CH3a a10 M-3): `Production` but in O2b's leg A, whose
    /// [`NodeSut`](crate::sut::NodeSut) keeps Core's flush causes alone.
    #[cfg(feature = "test-support")]
    pub regime: dashd_chainstate::flush::FlushRegime,
    /// Where the flush side books each job W released (CH3a a10-2): `NodeSut`'s tally, which
    /// spans its opens.
    #[cfg(feature = "test-support")]
    pub tally: Option<Arc<dashd_chainstate::flush::FlushTally>>,
    /// O2b's records tap (CH3a a10 C4): `NodeSut`'s, which records every connect attempt's `C` or
    /// `R` with its `A` and every disconnect's `D`, without the golden keys.
    #[cfg(feature = "test-support")]
    pub records: Option<crate::sut::RecordsTap>,
    /// Whether step 6 leaves CJDNS reachable: the NetInfo rules' `IsReachable(NET_CJDNS)`.
    pub cjdns_reachable: bool,
    /// Step 6's `mn_metaman`, made before the chainstate loads and read from its cache after
    /// (Core: init.cpp:1735, :2098-2106, :2249-2265): the chainstate's platform-ban resets go
    /// to it.
    pub metaman: Arc<MasternodeMetaMan>,
    /// `-dbcrashratio` ([`crash_ratio`](crate::init::caches::crash_ratio)): W's roll after each
    /// partial sub-batch, and above 0 every freeze waited for (CH2 D14).
    pub crash_ratio: u64,
    /// What W runs when its roll comes up.
    pub crash_exit: CrashExit,
    /// The state-hash sink [`Chainstate::attach_parts`] gives the connector, taken once
    /// (CH7a-5b X5, [`NodeSut::new_with_digest`](crate::sut::NodeSut::new_with_digest)).
    #[cfg(test)]
    pub digest: std::sync::Mutex<Option<Box<dyn dashd_chainstate::connect::DigestSink>>>,
    /// CH7a-5b's plant for the connector's evo hooks (`NodeEvo::plant_h11_pool_error_at`).
    #[cfg(test)]
    pub h11_pool_error_at: Option<u32>,
    /// Regtest's live-copy rule (CH5-5): W's copy points and ALWAYS flushes end with the
    /// engine's live-copy barrier ([`FlushConfig::live_copy`]), from [`chainstate_profile`].
    pub live_copy: bool,
}

/// Regtest's chainstate storage profile (`-storageprofile`, BL-634).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageProfile {
    /// CH5-5's live-copy profile: regtest's default.
    LiveCopy,
    /// The mainnet and testnet profile, `RocksTuning::default()`, without the live-copy rule.
    Production,
}

/// `-storageprofile` as the user set it: `Ok(None)` when unset. The inner `Err` is the init
/// error, for the value that is not `livecopy` or `production` (a negation included) and for any
/// value off regtest. The outer one is an argument that cannot be read.
///
/// The off-regtest refusal is `-acceptstalefeeestimates`' (Core: init.cpp:1723-1725); rdashd's
/// own refusals are reported after every argument check Core makes (DEC-201 (5)).
pub fn storage_request(
    args: &ArgsManager,
    chain: &ChainSetup,
) -> Result<Result<Option<StorageProfile>, String>, RuntimeError> {
    const ARG: &str = "-storageprofile";
    if !args.is_arg_set(ARG) {
        return Ok(Ok(None));
    }
    if chain.network != Network::Regtest {
        return Ok(Err(format!(
            "storageprofile is not supported on {} chain.",
            chain.network.network_id()
        )));
    }
    let value = args.get_arg(ARG, "")?;
    Ok(match (args.is_arg_negated(ARG), value.as_str()) {
        (false, "livecopy") => Ok(Some(StorageProfile::LiveCopy)),
        (false, "production") => Ok(Some(StorageProfile::Production)),
        _ => Err(format!("Invalid -storageprofile value '{value}' (livecopy or production).")),
    })
}

/// The chainstate's storage profile on `network` (CH5-5 §5): its RocksDB tuning, and whether W
/// keeps the live-copy rule. Regtest takes the live-copy profile unless `requested` is
/// `Production`. Every other network takes the default, **whatever `requested` says**: the
/// refusal in [`storage_request`] is the first guard, this the second (BL-634 §2.7).
pub fn chainstate_profile(
    network: Network,
    requested: Option<StorageProfile>,
) -> (RocksTuning, bool) {
    match (network, requested) {
        (Network::Regtest, None | Some(StorageProfile::LiveCopy)) => {
            (RocksTuning::for_regtest(), true)
        }
        _ => (RocksTuning::default(), false),
    }
}

/// `CDeterministicMNManager::ProcessBlock`'s `m_mn_metaman.ResetPlatformBan` (BL-425): the
/// chainstate's `ResetPlatformBan` effects reach the node's masternode meta store. A reset Core
/// refuses (no entry, or `SetPlatformBan`'s height rule) is only logged there, under `llmq`, so
/// it is no fault here.
// Core: evo/deterministicmns.cpp:733-748; masternode/meta.cpp:187-195
pub struct MetaPlatformBans(pub Arc<MasternodeMetaMan>);

impl PlatformBanApplier for MetaPlatformBans {
    fn reset(&mut self, pro_tx_hash: &ProTxHash, height: i32) -> Result<(), Fatal> {
        if !self.0.reset_platform_ban(pro_tx_hash, height) {
            tracing::debug!(
                target: "llmq",
                "ProcessBlock -- MN {pro_tx_hash} is failed to Platform revived at height {height}"
            );
        }
        Ok(())
    }
}

/// The coins cache's budget: the coins share, and what the mempool leaves of its maximum, as
/// Core's `GetCoinsCacheSizeState` adds them (`nTotalSpace`); R from the engine's tuning.
// Core: validation.cpp:2937-2966
fn coins_budget<T: EngineTuning>(
    coins_share: u64,
    sub_batch: u64,
    setup: &CoinsSetup<T>,
) -> CoinsBudget {
    CoinsBudget::new(
        coins_share,
        setup.max_mempool,
        Ratio::HALF,
        setup.tuning.batch_reserve(sub_batch),
    )
}

impl<E: KvEngine + 'static, F: FlatFs + 'static> Chainstate<E, F>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    /// V's coins (CH3a's connector) and flush (CH2b b4 M1) over this chainstate, attached as
    /// Core's `InitCoinsCache` ends: one coins cache at the recovered tip, W on its own thread
    /// over the engine and the block files, and the node's one `hub` (2b-1). PROTOTYPE (2b-2
    /// against b4 315bb6f9d): no mempool, no prune; the evo hooks are CH7a's
    /// `NodeEvo`, complete since CH7a-4, built by `node_evo`.
    ///
    /// # Errors
    ///
    /// W's thread that cannot be spawned.
    // Core: validation.cpp:2937-2966 (GetCoinsCacheSizeState), :2542-2570 (assumevalid)
    pub fn attach_parts<T: EngineTuning + 'static>(
        &self,
        params: &ChainSetup,
        hub: &Arc<SubscriberHub>,
        setup: &CoinsSetup<T>,
        signals: &ValidationSignals,
    ) -> std::io::Result<AttachCoins> {
        let obs: Arc<dyn FlushObserver> = Arc::new(self.observer());
        let tip = self.recovered.tip;
        let budget = coins_budget(self.coins_share, self.sub_batch, setup);
        let mut cache = CoinsCache::new(Arc::clone(&self.engine), budget, Salt::random(), tip);
        cache.set_best_block(tip);
        cache.set_xcheck(setup.xcheck);
        #[cfg(feature = "planted-bugs")]
        if let Some(bug) = self.plant {
            cache.plant(bug);
        }
        let coins = Arc::new(Mutex::new(cache));
        assert!(self.coins_cell.set(Arc::clone(&coins)).is_ok(), "a chainstate attaches once");
        let path = self.abort_path();
        #[cfg(feature = "test-support")]
        let (helper, digest) = match &setup.records {
            Some(tap) => {
                let (helper, digest) = tap.parts(params)?;
                (helper, Some(digest))
            }
            None => (Box::new(NodeHelper) as _, None),
        };
        #[cfg(not(feature = "test-support"))]
        let (helper, digest) = (Box::new(NodeHelper), None);
        let parts = ConnectorParts {
            coins: Arc::clone(&coins),
            files: Arc::clone(&self.recovered.files),
            queue: Arc::clone(&setup.queue),
            caches: Arc::clone(&setup.caches),
            helper,
            abort: path,
            assume_valid: setup.assume_valid,
            #[cfg(test)]
            digest: {
                let pending = setup.digest.lock().expect("the pending sink").take();
                debug_assert!(
                    pending.is_none() || digest.is_none(),
                    "a pending digest sink and a records tap on one connector"
                );
                pending.or(digest)
            },
            #[cfg(not(test))]
            digest,
            xcheck: setup.xcheck,
        };
        let ledger = Arc::new(EffectLedger::default());
        let (hooks, layer, facade) = self.node_evo(params, setup, &ledger, signals)?;
        #[cfg_attr(not(feature = "planted-bugs"), allow(unused_mut))]
        let mut connector = CoreConnector::new(params, parts, hooks, Arc::clone(&facade))
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        #[cfg(feature = "planted-bugs")]
        {
            use dashd_chainstate::Planted;
            let census = matches!(
                setup.planted,
                Some(Planted::ClCensusSuppressOnce | Planted::ClCensusDuplicateOnce)
            );
            assert!(
                census || matches!(setup.planted, None | Some(Planted::L1 | Planted::T12)),
                "{:?} is neither a connector bug nor a census control: it would arm nothing here",
                setup.planted
            );
            connector.plant(setup.planted.filter(|_| !census));
        }
        debug_assert_eq!(setup.live_copy, setup.tuning.live_copy(), "the profile's two halves");
        let crash = CrashRoll::new(setup.crash_ratio);
        let crash_exit = Arc::clone(&setup.crash_exit);
        #[cfg(test)]
        let w_crash = crash.clone();
        let driver = ThreadDriver::spawn(WThread {
            engine: Arc::clone(&self.engine),
            files: Arc::clone(&self.recovered.files),
            fs: Arc::clone(&self.fs),
            hub: Arc::clone(hub),
            obs: Arc::clone(&obs),
            tuning: Arc::clone(&setup.tuning),
            cfg: FlushConfig {
                sub_batch: self.sub_batch,
                data_dir: self.data_dir.clone(),
                blocks_dir: self.blocks_dir.clone(),
                live_copy: setup.live_copy,
            },
            covered_seq: 0,
            crash,
            on_crash: Box::new(move || crash_exit()),
        })?;
        let parts = NodeFlushParts {
            coins,
            driver,
            // CH2 D14: under `-dbcrashratio` V waits for every flush it starts.
            #[cfg(not(feature = "test-support"))]
            policy: FlushPolicy::new(setup.crash_ratio != 0),
            #[cfg(feature = "test-support")]
            policy: FlushPolicy::with_regime(setup.crash_ratio != 0, setup.regime),
            ledger,
            hub: Arc::clone(hub),
            obs,
            evo: Box::new(layer),
            prune: Box::new(NoPrune),
            inputs: Box::new(NoInputs),
        };
        #[cfg(test)]
        assert!(
            self.crash_record.attached.set((w_crash, parts.policy.synchronous())).is_ok(),
            "a chainstate attaches once"
        );
        let flush = NodeFlush::new(parts);
        #[cfg(feature = "test-support")]
        let flush = {
            let mut flush = flush;
            if let Some(tally) = &setup.tally {
                flush.set_tally(Arc::clone(tally));
            }
            flush
        };
        Ok(AttachCoins {
            connector: Box::new(connector),
            flush: Box::new(flush),
            facade: Some(facade),
        })
    }

    /// The evo hooks over the chainstate's database (CH7 §2.1, D51): the evo layer resumed from
    /// the raw `meta.evo_tip` now, after recovery and any wipe (D37), shared with the flush; the
    /// snapshot applier on `ledger`, the flush's (D47); the scheme Core starts from at the coins'
    /// best block ([`startup_bls`], D9, RP1a D68); the MN-list signal on V's `signals` (D15).
    /// With them the members facade over the applier's rows and the node latch (EV addendum
    /// §2.6), which the connector and V share, and whose mineable pool the mineable applier
    /// erases from and re-adds to (D14); route A reads the layer's mined rows through it (D94).
    fn node_evo<T: EngineTuning>(
        &self,
        params: &ChainSetup,
        setup: &CoinsSetup<T>,
        ledger: &Arc<EffectLedger>,
        signals: &ValidationSignals,
    ) -> std::io::Result<(NodeEvoHooks<E>, SharedEvoLayer, Arc<MembersFacade>)> {
        let failed = |e: String| std::io::Error::other(e);
        let net_info = node_net_info(params, setup.cjdns_reachable).map_err(|e| failed(e.0))?;
        let codec = node_codec(net_info);
        let genesis = dashd_chainstate::chain::genesis_block(params).hash();
        let store = Arc::new(RocksEvoStore::new(Arc::clone(&self.engine), codec, genesis));
        let layer = SharedEvoLayer::new(store.resume_layer().map_err(|e| failed(e.to_string()))?);
        let pow = PowParams::from_chain_params(params).map_err(|e| failed(e.to_string()))?;
        let blocks = NodeBlockFiles::new(
            Arc::clone(&self.recovered.files),
            pow,
            Arc::new(self.abort_path()),
        );
        let mn_list = MnListSignal::new(signals.clone());
        let rows = Arc::new(RuntimeRows::new(store.clone(), Arc::clone(ledger)));
        let latch = self.engine.latch().clone();
        let mined = CommittedMinedViews::new(Arc::clone(&store), layer.clone());
        let facade = MembersFacade::new(params, Arc::clone(&rows), latch)
            .map_err(|e| failed(format!("LLMQ parameters: {e:?}")))?
            .with_mined_views(Arc::new(mined));
        let facade = Arc::new(facade);
        let appliers = EffectAppliers {
            snapshot: Box::new(StoreSnapshots {
                rows,
            }),
            is_lock: Box::new(UnreachableIsLock),
            mineable: Box::new(FacadeMineable(Arc::clone(&facade))),
            platform_ban: Box::new(MetaPlatformBans(Arc::clone(&setup.metaman))),
            notify: Box::new(mn_list.applier()),
        };
        #[cfg_attr(not(feature = "planted-bugs"), allow(unused_mut))]
        let mut cl = node_cl_verifier(&self.data_dir);
        #[cfg(feature = "planted-bugs")]
        if let Some(bug) = setup.planted {
            cl.plant(bug);
        }
        let coins_height = self.coins_best_height();
        #[cfg(test)]
        assert!(self.evo_scheme_input.set(coins_height).is_ok(), "one evo hooks per chainstate");
        let hooks = NodeEvo::new(NodeEvoParts {
            store,
            layer: layer.clone(),
            appliers,
            params: Arc::new(params.clone()),
            net_info,
            ev1: node_ev1_checks(&self.data_dir),
            cl: Box::new(cl),
            blocks: Box::new(FileBlocks(blocks)),
            scheme: startup_bls(params, coins_height),
            mn_list,
        })
        .map_err(|e| failed(e.0))?;
        #[cfg(all(test, feature = "test-support"))]
        let hooks = {
            let mut hooks = hooks;
            if let Some(height) = setup.h11_pool_error_at {
                hooks.plant_h11_pool_error_at(height);
            }
            hooks
        };
        Ok((hooks, layer, facade))
    }
}

impl<E: KvEngine, F: FlatFs> Chainstate<E, F> {
    /// The address of the subscriber hub recovery ran on.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn recovered_on(&self) -> Option<usize> {
        self.recovered_on
    }

    /// What reached `-dbcrashratio`'s rolls and the flush policy.
    #[cfg(test)]
    pub(crate) fn crash_record(&self) -> &CrashRecord {
        &self.crash_record
    }

    /// The coins' best height `node_evo` received; `None` before the attach builds it.
    #[cfg(test)]
    pub(crate) fn evo_scheme_input(&self) -> Option<Option<u32>> {
        self.evo_scheme_input.get().copied()
    }

    /// The block the coins are at; `None` for an empty chainstate.
    pub fn tip(&self) -> Option<BlockHash> {
        self.coins_best_block().map(|b| b.hash)
    }

    /// Core's `CoinsTip().GetBestBlock()` at this start: recovery's `meta.tip`; `None` for an
    /// empty coins view.
    fn coins_best_block(&self) -> Option<dash_chain::BlockRef> {
        self.recovered.tip
    }

    /// The height of [`coins_best_block`](Self::coins_best_block): the one read that both
    /// [`coins_empty`](Self::coins_empty) and the evo hooks' initial scheme ([`startup_bls`])
    /// take, so the two never disagree (RP1a D68).
    fn coins_best_height(&self) -> Option<u32> {
        self.coins_best_block().map(|b| b.height)
    }

    /// Core's `is_coinsview_empty` (`node/chainstate.cpp:415-417`) at this start: the coins have
    /// no best block. `VerifyLoadOptions::coins_empty` reads it. Under `-reindex-chainstate` the
    /// open has wiped the coins, so it holds; rdashd refuses `-reindex` and a stored reindex (CH5).
    pub fn coins_empty(&self) -> bool {
        self.coins_best_height().is_none()
    }

    fn observer(&self) -> LogObserver {
        LogObserver {
            logger: Arc::clone(&self.logger),
        }
    }

    /// Core's ALWAYS flush on coins never attached to V, in Core's order, with W's logs: the blocks
    /// directory's `CheckDiskSpace`; the block and undo files' timer pair, around no file flush
    /// (DEC-1071 residual 1); the synced write, here the WAL sync
    /// ([`force_flush`](Self::force_flush)), in the block index's timer pair; then, only with a
    /// stored best block (Core's `CoinsTip().GetBestBlock()`, which a chainstate kept after a
    /// refusal holds without a loaded tip), the coins timer pair at the empty cache's figures
    /// around the data directory's `CheckDiskSpace` (an empty cache adds nothing to the floor) and
    /// the empty commit's lines, then the evodb timer pair around an empty commit. It sends no
    /// `ChainStateFlushed`: nothing is written (DEC-995). Nothing latches a failed space check, as
    /// Core's next flush checks again. On the invalid stored tip (DEC-196(1)) this is the ruled
    /// extension of rdashd's refusal, not Core's output: Core asserts there.
    ///
    /// Each failure is reported to `abort` where Core's `AbortNode` runs: a failed sync's after
    /// its timer pair, as Core's `catch` is outside every timer's scope; a low data directory's
    /// inside the coins pair.
    ///
    /// # Errors
    ///
    /// The first failure's `AbortNode` message, for the flush's failure line; nothing after it
    /// runs.
    // Core: validation.cpp:3056-3110, :3127-3129; util/fs_helpers.cpp:107-113
    pub(crate) fn bare_flush(&self, abort: &dyn AbortSink) -> Result<(), String> {
        let obs = self.observer();
        let report = |a: Abort| {
            abort.abort_node(&a.log, a.user.as_deref());
            a.log
        };
        check_disk_space(&*self.fs, &self.blocks_dir, 0).map_err(report)?;
        log_empty_files_flush(&obs);
        index_timed(&obs, || self.force_flush().map_err(|e| failed_sync(&self.logger, &e)))
            .map_err(report)?;
        if self.recovered.meta.tip.is_some() {
            empty_coins_commit(&obs, || {
                check_disk_space(&*self.fs, &self.data_dir, 0).map_err(report)
            })?;
            log_empty_evodb_commit(&obs);
        }
        Ok(())
    }

    /// The coins cache [`attach_parts`](Self::attach_parts) gave V, once attached: V's leaf
    /// (CH3a D50), for reads that run on V.
    pub fn coins_cell(&self) -> Option<&Arc<Mutex<CoinsCache<E>>>> {
        self.coins_cell.get()
    }

    /// The blocks recovery replayed, in order: its transition's forward leg (empty without one).
    pub fn replayed(&self) -> &[BlockHash] {
        &self.recovered.replayed
    }

    /// Core's `ForceFlushStateToDisk`: nothing is cached above the store until CH2b's flush
    /// machine runs; the write-ahead log is synced so a later kill loses nothing.
    ///
    /// # Errors
    ///
    /// The failure's text, the engine's where Core's `HandleError` throws LevelDB's (CH2 D10); the
    /// shutdown flush adds Core's report around it. It is this sync's own, or, when an earlier
    /// failure had tripped the guard's latch and the sync was refused, that failure's, as LevelDB
    /// returns a failed log sync's status to every later write (ND1-D16).
    // Core: leveldb/db/db_impl.cc:1239-1244, :1328-1331
    pub fn force_flush(&self) -> Result<(), String> {
        self.engine.sync_wal().map_err(|e| match self.engine.latch().tripped() {
            Some(first) if e.kind() == Some(ErrorKind::ReadOnly) => first.message.clone(),
            _ => e.to_string(),
        })
    }

    /// Whether a store failure has tripped the engine's latch: the chainstate writes nothing more.
    pub fn latched(&self) -> bool {
        self.engine.latch().tripped().is_some()
    }

    /// Whether the latch holds a flush's own failure whose `AbortNode` logged `message` (BL-482):
    /// W's failed space check, or V's `AbortNode`, which latch at [`FLUSH_SITE`] with the
    /// `AbortNode`'s text. A store's failure latches at its database's site with the engine's text.
    pub fn latched_by_flush(&self, message: &str) -> bool {
        self.engine.latch().tripped().is_some_and(|f| f.site == FLUSH_SITE && f.message == message)
    }

    /// What Core's next flush meets after the latched failure, once one has latched (BL-407). A
    /// waiting read, as [`latched`](Self::latched) is: its reporter has reported it (CH3b D222).
    pub fn latched_kept(&self) -> Option<Kept> {
        self.engine.latch().tripped().map(kept)
    }

    /// Whether the latched failure, or any trip that lost to it, is one of Core's asserts
    /// ([`Kept::Assert`]). Core's process ends at any assert, also one after an earlier failure,
    /// so it runs no next flush and writes no later store (BL-586, DEC-1156). A waiting read, as
    /// [`latched_kept`](Self::latched_kept) is. Only shutdown's `mncache.dat` store reads it: the
    /// shutdown flushes read `latched_kept`, the first failure's (B8).
    pub fn asserted(&self) -> bool {
        let latch = self.engine.latch();
        latch.tripped().is_some() && latch.any_site(|site| kept_site(site) == Kept::Assert)
    }

    /// The connector's abort path over the node's sink and latch: V's, and its block files'.
    pub(crate) fn abort_path(&self) -> AbortPath {
        AbortPath::new(Arc::clone(&self.abort), self.engine.latch().clone())
    }

    /// Trips the engine's latch with `message` at `site`, as a failure there would.
    #[cfg(test)]
    pub(crate) fn trip_latch(&self, site: &'static str, message: &str) {
        assert!(self.try_trip_latch(site, message));
    }

    /// [`trip_latch`](Self::trip_latch)'s trip, which may lose; whether it latched.
    #[cfg(test)]
    pub(crate) fn try_trip_latch(&self, site: &'static str, message: &str) -> bool {
        let failure = dash_kv::Failure {
            kind: ErrorKind::Other,
            site,
            message: message.to_owned(),
        };
        self.engine.latch().trip(failure)
    }
}

/// The engine `fail_next_wal_sync` drives: RocksDB, whose next WAL sync can be made to fail.
/// dashd-chainstate's `ProbeEngine` can do this too, but only behind its `model` feature, which
/// the hooks build would then pull in.
#[cfg(any(test, feature = "test-hooks"))]
mod wal_fault {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use dashd_store::{
        Cf, Commit, DbSchema, ErrorKind, KvEngine, KvRead, LiveVerdict, RocksEngine, StoreError,
        copy_point_steps,
    };

    pub struct WalFault {
        inner: RocksEngine,
        fail_next: Arc<AtomicBool>,
    }

    impl From<RocksEngine> for WalFault {
        fn from(inner: RocksEngine) -> Self {
            WalFault {
                inner,
                fail_next: Arc::default(),
            }
        }
    }

    impl WalFault {
        /// Set, it fails the next WAL sync.
        pub fn switch(&self) -> Arc<AtomicBool> {
            Arc::clone(&self.fail_next)
        }

        /// The RocksDB engine inside (CH5-5's `livecopycounters` reads its super versions).
        #[cfg(any(test, feature = "test-hooks"))]
        pub fn rocks(&self) -> &RocksEngine {
            &self.inner
        }

        /// The injected failure, once, if the switch is set.
        fn injected(&self) -> Result<(), StoreError> {
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(StoreError::engine(
                    "rocksdb",
                    ErrorKind::Io,
                    "injected WAL sync failure",
                ));
            }
            Ok(())
        }
    }

    impl KvRead for WalFault {
        fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
            self.inner.get(cf, key)
        }

        fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, StoreError> {
            self.inner.multi_get(cf, keys)
        }

        fn iter_prefix<'a>(
            &'a self,
            cf: Cf,
            prefix: &[u8],
        ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + 'a {
            self.inner.iter_prefix(cf, prefix)
        }

        fn iter_prefix_from<'a>(
            &'a self,
            cf: Cf,
            prefix: &[u8],
            start: &[u8],
        ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + 'a {
            self.inner.iter_prefix_from(cf, prefix, start)
        }

        fn iter_prefix_uncached<'a>(
            &'a self,
            cf: Cf,
            prefix: &[u8],
        ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + 'a {
            self.inner.iter_prefix_uncached(cf, prefix)
        }
    }

    impl KvEngine for WalFault {
        type Batch = <RocksEngine as KvEngine>::Batch;
        type Snapshot<'a> = <RocksEngine as KvEngine>::Snapshot<'a>;

        fn schema(&self) -> &'static DbSchema {
            self.inner.schema()
        }

        fn batch(&self) -> Self::Batch {
            self.inner.batch()
        }

        fn batch_with_capacity(&self, bytes: usize) -> Self::Batch {
            self.inner.batch_with_capacity(bytes)
        }

        fn write(&self, batch: Self::Batch, commit: Commit) -> Result<(), StoreError> {
            self.inner.write(batch, commit)
        }

        fn sync_wal(&self) -> Result<(), StoreError> {
            self.injected()?;
            self.inner.sync_wal()
        }

        fn wait_idle_purged(&self) -> Result<(), StoreError> {
            KvEngine::wait_idle_purged(&self.inner)
        }

        /// The inner engine's barrier (CH5-5 C3), so the live-copy profile keeps its verdict and
        /// exclusive section. On the live path the switch fails the barrier before it starts
        /// (before its memtable flush), where the default path fails at its WAL sync.
        fn live_barrier(
            &self,
            snapshot: &mut dyn FnMut(),
        ) -> Result<Option<LiveVerdict>, StoreError> {
            if !self.inner.is_live() {
                copy_point_steps(self, snapshot)?;
                return Ok(None);
            }
            self.injected()?;
            self.inner.live_barrier(snapshot).map(Some)
        }

        fn flush_memtables(&self) -> Result<(), StoreError> {
            self.inner.flush_memtables()
        }

        fn snapshot(&self) -> Self::Snapshot<'_> {
            self.inner.snapshot()
        }

        fn approximate_size(&self, cf: Cf) -> Result<u64, StoreError> {
            self.inner.approximate_size(cf)
        }
    }
}

#[cfg(test)]
mod tests {
    use dash_params::{ChainOption, ChainParamsBuilder, Release};
    use dashd_chainstate::chain::{WRONG_DEVNET_DATADIR, WRONG_NETWORK_DATADIR};

    use super::*;
    use crate::init::mempool::MemPoolOptions;

    /// A [`CoinsSetup`] with `tuning` and `max_mempool`, for the budget.
    fn coins_setup<T>(tuning: Arc<T>, max_mempool: u64) -> CoinsSetup<T> {
        use dashd_chainstate::sigcache::CacheSizes as SigCacheSizes;

        CoinsSetup {
            tuning,
            max_mempool,
            caches: Arc::new(
                ScriptCaches::new(SigCacheSizes::from_max_sig_cache_size(Some(0))).unwrap(),
            ),
            queue: Arc::new(CheckQueue::serial()),
            assume_valid: None,
            check_block_index: false,
            xcheck: false,
            #[cfg(feature = "planted-bugs")]
            planted: None,
            #[cfg(feature = "test-support")]
            regime: Default::default(),
            #[cfg(feature = "test-support")]
            tally: None,
            #[cfg(feature = "test-support")]
            records: None,
            cjdns_reachable: false,
            metaman: Arc::new(MasternodeMetaMan::new()),
            crash_ratio: 0,
            crash_exit: Arc::new(|| {}),
            digest: Default::default(),
            h11_pool_error_at: None,
            live_copy: false,
        }
    }

    /// CH5-5 §6.7's selection (M7), with BL-634's request: regtest takes the live-copy profile and
    /// its rule, unless `Production` is requested; every other network takes the default profile,
    /// without it, whatever is requested (the second guard).
    #[test]
    fn chainstate_profile_selects_the_live_profile_on_regtest_only() {
        let default = format!("{:?}", RocksTuning::default());
        let requests = [None, Some(StorageProfile::LiveCopy), Some(StorageProfile::Production)];
        for network in [Network::Main, Network::Testnet3, Network::Devnet] {
            for request in requests {
                let (tuning, live) = chainstate_profile(network, request);
                assert_eq!(
                    (format!("{tuning:?}"), live),
                    (default.clone(), false),
                    "{network:?} {request:?}"
                );
                assert!(!tuning.live_copy(), "{network:?} {request:?}");
            }
        }
        let (tuning, live) = chainstate_profile(Network::Regtest, Some(StorageProfile::Production));
        assert_eq!((format!("{tuning:?}"), live), (default, false));
        assert!(!tuning.live_copy());
        for request in [None, Some(StorageProfile::LiveCopy)] {
            let (tuning, live) = chainstate_profile(Network::Regtest, request);
            assert_eq!(
                (format!("{tuning:?}"), live),
                (format!("{:?}", RocksTuning::for_regtest()), true),
                "{request:?}"
            );
            assert!(tuning.live_copy());
        }
    }

    fn request(network: Network, argv: &[&str]) -> Result<Option<StorageProfile>, String> {
        let args = crate::init::test_args(argv);
        let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
        if network == Network::Devnet {
            builder = builder.arg(ChainOption::Devnet, "x");
        }
        let chain = builder.build().unwrap();
        storage_request(&args, &chain).unwrap()
    }

    /// `-storageprofile` on regtest: unset is `None`, the two names parse, anything else (a typo,
    /// an empty value, a number, a negation) is the invalid-value error.
    #[test]
    fn storage_request_parses() {
        assert_eq!(request(Network::Regtest, &[]), Ok(None));
        assert_eq!(
            request(Network::Regtest, &["-storageprofile=livecopy"]),
            Ok(Some(StorageProfile::LiveCopy))
        );
        assert_eq!(
            request(Network::Regtest, &["-storageprofile=production"]),
            Ok(Some(StorageProfile::Production))
        );
        for (arg, shown) in [
            ("-storageprofile=prod", "prod"),
            ("-storageprofile=", ""),
            ("-storageprofile=1", "1"),
            ("-nostorageprofile", "0"),
        ] {
            assert_eq!(
                request(Network::Regtest, &[arg]),
                Err(format!("Invalid -storageprofile value '{shown}' (livecopy or production).")),
                "{arg}"
            );
        }
    }

    /// Any value off regtest is refused with Core's "not supported on <id> chain." form.
    #[test]
    fn storage_request_refuses_off_regtest() {
        // Core's chain names, written out so a wrong `network_id` cannot agree with itself.
        for (network, id) in
            [(Network::Main, "main"), (Network::Testnet3, "test"), (Network::Devnet, "devnet")]
        {
            for arg in ["-storageprofile=livecopy", "-storageprofile=production"] {
                assert_eq!(
                    request(network, &[arg]),
                    Err(format!("storageprofile is not supported on {id} chain.")),
                    "{network:?} {arg}"
                );
            }
            assert_eq!(request(network, &[]), Ok(None), "{network:?}");
        }
    }

    /// The node's engine (behind the WAL-sync switch in test builds) keeps RocksDB's live-copy
    /// barrier and its verdict, and its idle wait; on the default profile, the plain barrier. The
    /// switch fails the barrier's WAL sync.
    #[test]
    fn the_node_engine_keeps_the_live_barrier() {
        use dashd_chainstate::caches::IndexArgs;

        let sizes = CacheSizes::calculate(
            dashd_chainstate::caches::DEFAULT_DB_CACHE,
            &IndexArgs::default(),
        );
        let open = |tuning: RocksTuning| {
            let dir = tempfile::tempdir().unwrap();
            let (inner, _) =
                tuning.open_chainstate(&dir.path().join("c"), &sizes, 1 << 20).unwrap();
            (dir, Engine::from(inner))
        };
        use dashd_store::KvBatch as _;

        let (_dir, live) = open(RocksTuning::for_regtest());
        // `quiesce` (§6.2): the idle wait returns at once on the profile, and flushes nothing.
        let mut b = live.batch();
        b.put(dashd_store::Cf::new(0), b"quiesce", b"no flush");
        live.write(b, dashd_store::Commit::Buffered).unwrap();
        let versions = live.rocks().super_versions().unwrap();
        let started = std::time::Instant::now();
        live.wait_idle_purged().unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
        assert_eq!(live.rocks().super_versions().unwrap(), versions, "no memtable flush");
        let mut ran = 0;
        let verdict = live.live_barrier(&mut || ran += 1).unwrap().expect("the live verdict");
        assert_eq!(verdict.cfs.len(), 11);
        assert_eq!(ran, 1);
        live.switch().store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(live.live_barrier(&mut || ran += 1).is_err(), "the injected WAL sync failure");
        assert_eq!(ran, 1);

        let (_dir, plain) = open(RocksTuning::default());
        assert_eq!(plain.live_barrier(&mut || ran += 1).unwrap(), None);
        assert_eq!(ran, 2);
    }

    /// The replays' budget ([`CoinsBudget::node`], DEC-1005) is the one rdashd builds with
    /// RocksDB and the default `-dbbatchsize`, at every `-dbcache`, with `-txindex` on and off and
    /// at both default `-maxmempool`s (rdashd's 5 MB under its default `-blocksonly`, DEC-322, and
    /// Core's 300 MB): the replays' figures are the node's.
    #[test]
    fn the_replays_coins_budget_is_the_node_s() {
        use dashd_chainstate::caches::IndexArgs;
        use dashd_chainstate::tuning::DEFAULT_DB_BATCH_SIZE;

        use crate::init::consts::DEFAULT_BLOCKSONLY_MAX_MEMPOOL_SIZE_MB;

        let blocks_only = DEFAULT_BLOCKSONLY_MAX_MEMPOOL_SIZE_MB as u64 * 1_000_000;
        for max_mempool in [blocks_only, MemPoolOptions::default().max_size_bytes as u64] {
            let setup = coins_setup(Arc::new(RocksTuning::default()), max_mempool);
            for txindex in [true, false] {
                let args = IndexArgs {
                    txindex,
                    ..IndexArgs::default()
                };
                for dbcache in [4, 5, 6, 7, 18, 80, 245, 246, 280, 281, 300, 610, 16_000] {
                    let sizes = CacheSizes::calculate(dbcache, &args);
                    let sub_batch = tuning::sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes);
                    let node = coins_budget(sizes.coins, sub_batch, &setup);
                    let replay = CoinsBudget::node(&sizes, max_mempool);
                    assert_eq!(replay, node, "{dbcache} {txindex} {max_mempool}");
                }
            }
        }
    }

    /// A default node's coins budget is dashd's `nTotalSpace` with an empty mempool: the coins
    /// share of the default `-dbcache` plus the default `-maxmempool`, so the cache turns LARGE
    /// and CRITICAL at Core's sizes (BL-273). Core's arithmetic is written out here.
    // Core: validation.cpp:2945-2966, init.cpp:1990-2023
    #[test]
    fn a_default_node_s_coins_budget_is_core_s_total_space() {
        use dashd_chainstate::caches::{DEFAULT_DB_CACHE, IndexArgs};
        use dashd_chainstate::coins::CoinsCacheSizeState;

        let coins = CacheSizes::calculate(DEFAULT_DB_CACHE, &IndexArgs::default()).coins;
        let max_mempool = MemPoolOptions::default().max_size_bytes;
        assert_eq!(max_mempool, 300_000_000, "DEFAULT_MAX_MEMPOOL_SIZE_MB");
        let setup = coins_setup(Arc::new(tuning::MemoryTuning), max_mempool as u64);
        let budget = coins_budget(coins, 1 << 20, &setup);
        let core = |cache_size: i64, mempool_usage: i64| {
            let total = coins as i64 + (max_mempool - mempool_usage).max(0);
            let large = ((9 * total) / 10).max(total - 10 * 1024 * 1024);
            if cache_size > total {
                CoinsCacheSizeState::Critical
            } else if cache_size > large {
                CoinsCacheSizeState::Large
            } else {
                CoinsCacheSizeState::Ok
            }
        };
        for mempool_usage in [0, 100_000_000, 300_000_000, 400_000_000] {
            let total = budget.total(mempool_usage);
            assert_eq!(total as i64, coins as i64 + (max_mempool - mempool_usage as i64).max(0));
            let large = ((9 * total) / 10).max(total - 10 * 1024 * 1024);
            for size in [large - 1, large, large + 1, total - 1, total, total + 1] {
                let want = core(size as i64, mempool_usage as i64);
                assert_eq!(budget.size_state(size, mempool_usage), want, "{size} {mempool_usage}");
            }
        }
    }

    /// Another chain's datadir is a plain `InitError`; a false `LoadGenesisBlock` asks for a
    /// reindex, after its cause.
    #[test]
    fn load_failures_take_core_s_statuses() {
        for text in [WRONG_NETWORK_DATADIR, WRONG_DEVNET_DATADIR] {
            let open = OpenError::from(LoadFailure::Incompatible(text));
            assert!(matches!(&open, OpenError::Incompatible(m) if m == text), "{open:?}");
        }
        for cause in [None, Some("System error: disk full".to_owned())] {
            let open = OpenError::from(LoadFailure::Failure {
                cause: cause.clone(),
            });
            assert!(
                matches!(&open, OpenError::Recoverable { message, cause: c }
                    if message == "Error initializing block database" && *c == cause),
                "{open:?}"
            );
        }
        let open = OpenError::from(LoadFailure::LoadingBlockDb);
        assert!(
            matches!(&open, OpenError::Recoverable { message, cause: None }
                if message == "Error loading block database"),
            "{open:?}"
        );
        let open = OpenError::from(LoadFailure::Store {
            cause: "IO error: no space".to_owned(),
        });
        assert!(
            matches!(&open, OpenError::Recoverable { message, cause: Some(c) }
                if message == "Error opening block database" && c == "IO error: no space"),
            "{open:?}"
        );
        assert!(matches!(
            OpenError::from(LoadFailure::Interrupted),
            OpenError::Interrupted(m) if m.is_empty()
        ));
        let open = OpenError::from(LoadFailure::NotReady("refused".to_owned()));
        assert!(matches!(&open, OpenError::Incompatible(m) if m == "refused"), "{open:?}");
    }

    /// The chainstate's resets reach the meta store the node loads and stores (BL-425), with
    /// `ResetPlatformBan`'s rules: a masternode without an entry gets none, and an entry never
    /// banned (its `m_platform_ban_updated` 0) takes the reset's height. Neither is a fault.
    // Core: masternode/meta.cpp:187-195, masternode/meta.h:47, :73-84
    #[test]
    fn platform_ban_resets_reach_the_meta_store() {
        let metaman = Arc::new(MasternodeMetaMan::new());
        let mut bans = MetaPlatformBans(Arc::clone(&metaman));
        let (absent, mixer) = (ProTxHash::from([1; 32]), ProTxHash::from([2; 32]));
        // A refused reset is Core's `llmq` debug line and nothing else; an applied one logs none
        // (SP1c-1 r1 n-2).
        let refused = |protx: &ProTxHash, height: i32| {
            vec![(
                tracing::Level::DEBUG,
                format!(
                    "ProcessBlock -- MN {protx} is failed to Platform revived at height {height}"
                ),
            )]
        };
        let mut reset = |protx: &ProTxHash, height: i32| {
            let (r, lines) = crate::test_log::logged(|| bans.reset(protx, height));
            r.expect("Core only logs a refused reset");
            lines
        };
        assert_eq!(reset(&absent, 7), refused(&absent, 7));
        assert!(metaman.snapshot().infos.is_empty(), "no entry is made");
        metaman.allow_mixing(&mixer);
        assert_eq!(metaman.info(&mixer).platform_ban_updated, 0);
        assert_eq!(reset(&mixer, 7), [], "applied");
        let info = metaman.info(&mixer);
        assert!(!info.platform_ban);
        assert_eq!(info.platform_ban_updated, 7, "an existing entry takes the height");
        assert_eq!(reset(&mixer, 7), refused(&mixer, 7), "an unban at the same height");
        assert_eq!(reset(&mixer, 6), refused(&mixer, 6), "an older height");
        assert_eq!(metaman.info(&mixer).platform_ban_updated, 7);
    }
}
