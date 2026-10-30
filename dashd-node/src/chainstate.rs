//! The chainstate as init and shutdown see it: CH2's RocksDB chainstate opened and recovered
//! (DEC-201 Q7), with no chain on top until CH1-node composes one (C11). `NodeSut` opens the same
//! chainstate on its own engine and block files (ND1-D10).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dash_chain::IndexAccess;
use dash_consensus::ctx::bls_ctx_for_block;
use dash_consensus::pow::PowParams;
use dash_consensus::primitives::BlockHash;
use dash_evo::types::ProTxHash;
use dash_params::ChainParams;
use dashd_chainstate::caches::CacheSizes;
use dashd_chainstate::chain::{
    AbortPath, AttachCoins, Fatal, INIT_BLOCK_DB_FAILED, LOAD_BLOCK_DB_FAILED, LoadFailure,
    OPEN_BLOCK_DB_FAILED,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use dashd_chainstate::connect::{ConnectorParts, CoreConnector, NodeHelper};
use dashd_chainstate::evo::RocksEvoStore;
use dashd_chainstate::evo::cl::NotBuiltClVerifier;
use dashd_chainstate::evo::effects::{
    EffectAppliers, PlatformBanApplier, StoreSnapshots, UnreachableIsLock, UnreachableMineable,
    UnreachableNotify,
};
use dashd_chainstate::evo::ev1::DmlChecks;
use dashd_chainstate::evo::node::{
    FileBlocks, NodeEvo, NodeEvoParts, SharedEvoLayer, node_codec, node_net_info,
};
use dashd_chainstate::flush::{
    CrashRoll, EffectLedger, FlushConfig, FlushObserver, FlushPolicy, LogCat, NoInputs, NoPrune,
    NodeFlush, NodeFlushParts, ObsMark, SubscriberHub, ThreadDriver, WThread,
};
use dashd_chainstate::recovery::{
    ChainstateOpen, InitError, Opened, RecoveredChainstate, RecoveryConfig,
};
use dashd_chainstate::sigcache::ScriptCaches;
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

/// The connector's evo hooks: CH7a's `NodeEvo`, incomplete (`COMPLETE = false`) until CH7a-5, so
/// the readiness guard (CH3a D70) is live at connect and at startup.
pub type NodeEvoHooks<E = Engine> = NodeEvo<E, DmlChecks>;

/// The node's engine: RocksDB; in test builds behind a switch that fails its next WAL sync.
#[cfg(not(any(test, feature = "test-hooks")))]
pub type Engine = dashd_store::RocksEngine;
#[cfg(any(test, feature = "test-hooks"))]
pub type Engine = wal_fault::WalFault;

/// Why the chainstate did not open.
#[derive(Debug)]
pub enum OpenError {
    /// Core's message for a chainstate that may load after `-reindex`; `cause` is logged first.
    Recoverable {
        message: String,
        cause: Option<String>,
    },
    /// Core's `FAILURE_INCOMPATIBLE_DB`: a plain `InitError`, no reindex question. The readiness
    /// guard's refusal at startup (CH3a D70) and `FAILURE_INSUFFICIENT_DBCACHE` take the same
    /// path (`init.cpp:2115-2117`).
    Incompatible(String),
    /// `-dbcrashratio` came up (`_Exit(0)` after its log line).
    Crashed,
    /// Core's `INTERRUPTED`: the load stopped with shutdown requested. Init asks nothing and
    /// exits as for any shutdown requested during the load.
    Interrupted,
}

/// Core's statuses for the block index load (CH1-node's [`LoadFailure`]): another chain's
/// datadir is `FAILURE_INCOMPATIBLE_DB`, a false `LoadGenesisBlock` is `FAILURE`.
// Core: node/chainstate.cpp:189-198, :210-211
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
            LoadFailure::Interrupted => OpenError::Interrupted,
            LoadFailure::NotReady(refusal) => OpenError::Incompatible(refusal),
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
    /// the block index too, and the block files.
    // Core: node/chainstate.cpp:166-171, :220-247
    #[allow(clippy::too_many_arguments)]
    pub fn open_node(
        datadir: &Path,
        blocks_dir: &Path,
        chain: &ChainParams,
        sizes: &CacheSizes,
        db_batch_size: u64,
        logger: Arc<Logger>,
        abort: Arc<dyn AbortSink>,
    ) -> Result<Opening, OpenError> {
        let path = datadir.join(CHAINSTATE_DIR);
        #[cfg(any(test, feature = "test-hooks"))]
        let mut wal_fault = None;
        let rocks = |sub_batch| {
            let (inner, schema) = RocksTuning::default()
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
    /// Makes the next WAL sync fail with an I/O error, as a full or failing disk would.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn fail_next_wal_sync(&self) {
        let switch = self.wal_fault.as_ref().expect("the node's engine");
        switch.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The latch site of the flush's own failures, a copy of dashd-chainstate's four literals: W's
/// failed step (flush/machine.rs:469, its space check), W's panic (flush/driver.rs:351), and V's
/// `AbortNode` (flush/control.rs:372, :967). Only the first is pinned here, by the SUT's low-disk
/// test; the others have no Core counterpart.
const FLUSH_SITE: &str = "chainstate flush";

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
    /// the block files in `blocks_dir` on `fs`. `db_batch_size` is `-dbbatchsize`.
    // Prototype (2b-D24): 2b-2 proper bundles the node-side paths.
    #[allow(clippy::too_many_arguments)]
    pub fn on(
        engine: impl FnOnce(u64) -> Result<E, OpenError>,
        fs: Arc<F>,
        data_dir: &Path,
        blocks_dir: &Path,
        chain: &ChainParams,
        sizes: &CacheSizes,
        db_batch_size: u64,
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
                crash: CrashRoll::seeded(0, 0), // -dbcrashratio is CH2b's
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
    /// [`block_index`](Opened::block_index).
    pub fn open(&self) -> Result<Opened<'_, E, F>, OpenError> {
        ChainstateOpen::open(&self.engine, Arc::clone(&self.fs), self.blocks.clone(), &self.obs)
            .map_err(init_error)
    }

    /// The rest of recovery on the loaded block `index`, through the node's `hub`: Core's
    /// `ReplayBlocks`. Without a block index the chainstate must be empty.
    // Core: node/chainstate.cpp:249-253
    pub fn recover(
        &self,
        opened: Opened<'_, E, F>,
        index: &dyn IndexAccess,
        hub: &SubscriberHub,
    ) -> Result<RecoveredChainstate<F>, OpenError> {
        #[cfg(all(test, feature = "test-support"))]
        self.recovered_on.set(Some(std::ptr::from_ref(hub).addr()));
        opened.recover(index, hub, self.cfg.clone()).map_err(init_error)
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
    /// [`NodeSut`](crate::sut::NodeSut) unless its arguments negate it.
    pub xcheck: bool,
    /// The connector bug (L1 or T12, the only ones [`CoreConnector::plant`] arms) for xcheck's
    /// positive controls. Any other is refused: cache bugs are armed through `CoinsCache::plant`,
    /// which this path never calls, so they would arm nothing.
    #[cfg(feature = "planted-bugs")]
    pub planted: Option<dashd_chainstate::Planted>,
    /// Whether step 6 leaves CJDNS reachable: the NetInfo rules' `IsReachable(NET_CJDNS)`.
    pub cjdns_reachable: bool,
    /// Step 6's `mn_metaman`, made before the chainstate loads and read from its cache after
    /// (Core: init.cpp:1735, :2098-2106, :2249-2265): the chainstate's platform-ban resets go
    /// to it.
    pub metaman: Arc<MasternodeMetaMan>,
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
    /// against b4 315bb6f9d): no mempool, no prune, no `-dbcrashratio`; the evo hooks are CH7a's
    /// incomplete `NodeEvo`, built by `node_evo`.
    ///
    /// # Errors
    ///
    /// W's thread that cannot be spawned.
    // Core: validation.cpp:2937-2966 (GetCoinsCacheSizeState), :2542-2570 (assumevalid)
    pub fn attach_parts<T: EngineTuning + 'static>(
        &self,
        params: &ChainParams,
        hub: &Arc<SubscriberHub>,
        setup: &CoinsSetup<T>,
    ) -> std::io::Result<AttachCoins> {
        let obs: Arc<dyn FlushObserver> = Arc::new(LogObserver {
            logger: Arc::clone(&self.logger),
        });
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
        let path = AbortPath::new(Arc::clone(&self.abort), self.engine.latch().clone());
        let parts = ConnectorParts {
            coins: Arc::clone(&coins),
            files: Arc::clone(&self.recovered.files),
            queue: Arc::clone(&setup.queue),
            caches: Arc::clone(&setup.caches),
            helper: Box::new(NodeHelper),
            abort: path,
            assume_valid: setup.assume_valid,
            digest: None,
            xcheck: setup.xcheck,
        };
        let ledger = Arc::new(EffectLedger::default());
        let (hooks, layer) = self.node_evo(params, tip, setup, &ledger)?;
        #[cfg_attr(not(feature = "planted-bugs"), allow(unused_mut))]
        let mut connector = CoreConnector::new(params, parts, hooks, Arc::new(()))
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        #[cfg(feature = "planted-bugs")]
        {
            use dashd_chainstate::Planted;
            assert!(
                matches!(setup.planted, None | Some(Planted::L1 | Planted::T12)),
                "{:?} is not a connector bug: it would arm nothing here",
                setup.planted
            );
            connector.plant(setup.planted);
        }
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
                live_copy: false,
            },
            covered_seq: 0,
            crash: CrashRoll::seeded(0, 0),
            on_crash: Box::new(|| {}),
        })?;
        let flush = NodeFlush::new(NodeFlushParts {
            coins,
            driver,
            policy: FlushPolicy::new(false),
            ledger,
            hub: Arc::clone(hub),
            obs,
            evo: Box::new(layer),
            prune: Box::new(NoPrune),
            inputs: Box::new(NoInputs),
        });
        Ok(AttachCoins {
            connector: Box::new(connector),
            flush: Box::new(flush),
        })
    }

    /// The evo hooks over the chainstate's database (CH7 §2.1, D51): the evo layer resumed from
    /// the raw `meta.evo_tip` now, after recovery and any wipe (D37), shared with the flush; the
    /// snapshot applier on `ledger`, the flush's (D47); the scheme Core starts from at the tip
    /// (D9).
    fn node_evo<T: EngineTuning>(
        &self,
        params: &ChainParams,
        tip: Option<dash_chain::BlockRef>,
        setup: &CoinsSetup<T>,
        ledger: &Arc<EffectLedger>,
    ) -> std::io::Result<(NodeEvoHooks<E>, SharedEvoLayer)> {
        let failed = |e: String| std::io::Error::other(e);
        let net_info = node_net_info(params, setup.cjdns_reachable).map_err(|e| failed(e.0))?;
        let codec = node_codec(net_info);
        let genesis = dashd_chainstate::chain::genesis_block(params).hash();
        let store = Arc::new(RocksEvoStore::new(Arc::clone(&self.engine), codec, genesis));
        let layer = SharedEvoLayer::new(store.resume_layer().map_err(|e| failed(e.to_string()))?);
        let pow = PowParams::from_chain_params(params).map_err(|e| failed(e.to_string()))?;
        let path = AbortPath::new(Arc::clone(&self.abort), self.engine.latch().clone());
        let blocks = NodeBlockFiles::new(Arc::clone(&self.recovered.files), pow, Arc::new(path));
        let appliers = EffectAppliers {
            snapshot: Box::new(StoreSnapshots {
                store: Arc::clone(&store),
                ledger: Arc::clone(ledger),
            }),
            is_lock: Box::new(UnreachableIsLock),
            mineable: Box::new(UnreachableMineable),
            platform_ban: Box::new(MetaPlatformBans(Arc::clone(&setup.metaman))),
            notify: Box::new(UnreachableNotify),
        };
        let next = tip.map_or(0, |t| t.height + 1);
        let hooks = NodeEvo::new(NodeEvoParts {
            store,
            layer: layer.clone(),
            appliers,
            params: Arc::new(params.clone()),
            net_info,
            ev1: DmlChecks,
            cl: Box::new(NotBuiltClVerifier),
            blocks: Box::new(FileBlocks(blocks)),
            scheme: bls_ctx_for_block(params, next),
        })
        .map_err(|e| failed(e.0))?;
        Ok((hooks, layer))
    }
}

impl<E: KvEngine, F: FlatFs> Chainstate<E, F> {
    /// The address of the subscriber hub recovery ran on.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn recovered_on(&self) -> Option<usize> {
        self.recovered_on
    }

    /// The block the coins are at; `None` for an empty chainstate.
    pub fn tip(&self) -> Option<BlockHash> {
        self.recovered.tip.map(|b| b.hash)
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

    /// Trips the engine's latch with `message` at `site`, as a failure there would.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn trip_latch(&self, site: &'static str, message: &str) {
        let failure = dash_kv::Failure {
            kind: ErrorKind::Other,
            site,
            message: message.to_owned(),
        };
        assert!(self.engine.latch().trip(failure));
    }
}

/// The engine `fail_next_wal_sync` drives: RocksDB, whose next WAL sync can be made to fail.
/// dashd-chainstate's `ProbeEngine` can do this too, but only behind its `model` feature, which
/// the hooks build would then pull in.
#[cfg(any(test, feature = "test-hooks"))]
mod wal_fault {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use dashd_store::{Cf, Commit, DbSchema, ErrorKind, KvEngine, KvRead, RocksEngine, StoreError};

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
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(StoreError::engine(
                    "rocksdb",
                    ErrorKind::Io,
                    "injected WAL sync failure",
                ));
            }
            self.inner.sync_wal()
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
    use dashd_chainstate::chain::{WRONG_DEVNET_DATADIR, WRONG_NETWORK_DATADIR};

    use super::*;

    /// A default node's coins budget is dashd's `nTotalSpace` with an empty mempool: the coins
    /// share of the default `-dbcache` plus the default `-maxmempool`, so the cache turns LARGE
    /// and CRITICAL at Core's sizes (BL-273). Core's arithmetic is written out here.
    // Core: validation.cpp:2945-2966, init.cpp:1990-2023
    #[test]
    fn a_default_node_s_coins_budget_is_core_s_total_space() {
        use dashd_chainstate::caches::{DEFAULT_DB_CACHE, IndexArgs};
        use dashd_chainstate::coins::CoinsCacheSizeState;
        use dashd_chainstate::sigcache::CacheSizes as SigCacheSizes;

        use crate::init::mempool::MemPoolOptions;

        let coins = CacheSizes::calculate(DEFAULT_DB_CACHE, &IndexArgs::default()).coins;
        let max_mempool = MemPoolOptions::default().max_size_bytes;
        assert_eq!(max_mempool, 300_000_000, "DEFAULT_MAX_MEMPOOL_SIZE_MB");
        let setup = CoinsSetup {
            tuning: Arc::new(tuning::MemoryTuning),
            max_mempool: max_mempool as u64,
            caches: Arc::new(
                ScriptCaches::new(SigCacheSizes::from_max_sig_cache_size(Some(0))).unwrap(),
            ),
            queue: Arc::new(CheckQueue::serial()),
            assume_valid: None,
            check_block_index: false,
            xcheck: false,
            #[cfg(feature = "planted-bugs")]
            planted: None,
            cjdns_reachable: false,
            metaman: Arc::new(MasternodeMetaMan::new()),
        };
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
        assert!(matches!(OpenError::from(LoadFailure::Interrupted), OpenError::Interrupted));
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
