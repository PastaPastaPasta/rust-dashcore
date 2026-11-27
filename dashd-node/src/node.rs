//! The node's lifetime in Core's order (`bitcoind.cpp` `AppInit`, `init.cpp`): init in two phases
//! around the daemon fork, then `Interrupt` and `Shutdown`, which run whether init succeeded or
//! not.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_chain::{ChainPolicy, updated_block_tip_schedules};
use dash_clock::MockClock;
use dash_consensus::BlockId;
use dash_consensus::primitives::BlockHash;
use dash_p2p::rng::FastRandom;
use dash_params::ChainSetup;
use dashd_chainstate::caches::CacheSizes;
#[cfg(feature = "test-hooks")]
use dashd_chainstate::chain::{BlockTailStage, StagedRecord};
use dashd_chainstate::chain::{ValidationInterface, ValidationSignals};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::sigcache::ScriptCaches;
use dashd_masternode::active::ActiveMasternode;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::mnauth::MnAuthSigning;
use dashd_masternode::sync::MasternodeSync;
use dashd_masternode::tip::TipMnList;
use dashd_mempool::is::IsMempool;
use dashd_mining::gbt::{ChainGbt, GbtParams, GbtParts};
use dashd_mining::{ChainTemplateSource, GbtEngine, MiningMempool};
use dashd_p2p::processing::seams::{MnAuthSigner, SeenChainLocksCleanup};
use dashd_store::flatfile::{FlatFs as _, MIN_DISK_SPACE, OsFs};

use crate::abort::{AbortSink as _, NodeAbort};
use crate::chain::Chain;
use crate::chainstate::digest::open_log;
use crate::chainstate::{
    CoinsSetup, CrashExit, OpenError, Opening, StorageProfile, chainstate_profile,
    reindex_question, storage_request,
};
use crate::coinjoin::role::CoinJoin;
use crate::datadir::{self, DirLock, PidFile};
use crate::indexes::{self, CoinStatsHandle, FilterIndexHandle, TxIndexHandle};
use crate::init::caches::{
    cache_sizes, crash_ratio, db_batch_size, init_validation_caches, max_upload_target,
    start_script_check_threads,
};
use crate::init::mempool::{MemPoolOptions, apply_args_man_options};
use crate::init::{self, ParamInteraction, SelectedParams};
use crate::logging::{self, Logger, set_thread_name};
use crate::mining_adapters::{MiningInstantSend, MiningPool, NotYetSafe};
use crate::mnsync::{MnMaintenance, SyncParts};
use crate::scheduler::Scheduler;
use crate::shutdown::{EXIT_FAILURE, ShutdownState};
use crate::signing;
use crate::spv_mode::{SPV_DIR, SpvHost, SpvStart};
use crate::steps::ChainSteps;
use crate::ui::Ui;
use crate::warnings::Warnings;
use crate::{InitFailure, host, notify, p2p, rpc};
use dashd_chainstate::tuning::{EngineTuning, RocksTuning};

/// The MNAUTH signer of step 7c's active masternode, over `tip`.
// Core: init.cpp:2156-2167; evo/mnauth.cpp:19-49 at v24.0.0
fn mnauth_signer(
    active: Option<&Arc<ActiveMasternode>>,
    tip: &TipMnList,
) -> Option<Arc<dyn MnAuthSigner>> {
    active.map(|active| {
        Arc::new(MnAuthSigning::new(Arc::clone(active), tip.clone())) as Arc<dyn MnAuthSigner>
    })
}

/// Core's `NodeContext`: the services init builds and shutdown takes down. Dropping it shuts it
/// down if [`Node::shutdown`] has not run.
pub struct Node {
    pub args: Arc<ArgsManager>,
    pub ui: Arc<Ui>,
    pub clock: Arc<MockClock>,
    pub shutdown: Arc<ShutdownState>,
    pub warnings: Arc<Warnings>,
    pub abort: Arc<NodeAbort>,
    params: SelectedParams,
    /// The support table's refusal, computed before any soft-set (DEC-201 Q12).
    unsupported: Option<String>,
    /// `-i2pacceptincoming`'s refusal with `-i2psam`, after parameter interaction; read after
    /// `unsupported`, once step 12's `-i2psam` check has passed.
    i2p_accept_refusal: Option<String>,
    /// `-storageprofile` (BL-634), read with `unsupported`: the request, or the init error to
    /// report in rdashd's own refusal slot.
    storage_request: Result<Option<StorageProfile>, String>,
    interaction: Option<ParamInteraction>,
    /// V, the coins and the spork manager (steps 7a to 11 and their shutdown).
    steps: ChainSteps,
    /// The network, from step 7c (P3 S6a).
    net: Option<p2p::Net>,
    /// SPV mode's client, from [`Node::app_init_main_spv`] until shutdown has stopped it.
    spv: Option<Box<dyn SpvHost>>,
    scheduler: Option<Scheduler>,
    /// The ChainLock handler's own scheduler and its `cl-schdlr` thread, made before step 7c; its
    /// task runs from step 10a (BL-626).
    cl_scheduler: Option<Scheduler>,
    /// The ChainLock handler's tip listener on V's signals, from step 10a to shutdown (BL-632).
    cl_tip: Option<(ValidationSignals, Arc<dyn ValidationInterface>)>,
    /// The RPC servers `-server` starts at step 4a.
    rpc: Option<rpc::Servers>,
    /// The chain the RPC commands read, V's once step 7 starts it.
    rpc_chain: Arc<rpc::ChainSlot>,
    /// The quorum manager, built at step 7 before the chainstate loads.
    pub(crate) quorums: Option<init::quorums::NodeQuorums>,
    /// Core's `g_txindex`, from step 7's end: `None` with `-txindex=0`.
    txindex: Option<TxIndexHandle>,
    /// The signing manager and its "recsigs" thread, from step 7c; the thread runs from step
    /// 10a to shutdown.
    signing: Option<init::signing::NodeSigning>,
    /// Core's `GetBlockFilterIndex(BASIC)`, from step 7's end: `None` without
    /// `-blockfilterindex`.
    filters: Option<FilterIndexHandle>,
    /// Core's `g_coin_stats_index`, from step 7's end: `None` without `-coinstatsindex`.
    coinstats: Option<CoinStatsHandle>,
    /// DK1's DKG on a masternode or an observer, from step 7c until shutdown stops it before V.
    #[cfg(feature = "dkg-runtime")]
    pub(crate) dkg: Option<init::dkg::NodeDkg>,
    /// A masternode's ChainLock and InstantSend signers, from step 7c; they run from step 10a to
    /// shutdown.
    lock_signers: Option<crate::locks::LockSigners>,
    /// The network the RPC commands read, from step 12.
    rpc_net: Arc<rpc::NetSlot>,
    /// `mn_sync` for `mnsync`, from step 7.
    rpc_sync: Arc<rpc::SyncSlot>,
    pid: Option<PidFile>,
    /// Released last: nothing may open the datadir's databases once another process can lock it.
    lock: Option<DirLock>,
    /// `Interrupt` ran (it runs once, whoever calls it first).
    interrupted: AtomicBool,
    shut_down: bool,
    /// `-dbcrashratio`'s exit, for W's roll and recovery's: Core's `_Exit(0)`.
    crash_exit: CrashExit,
    /// A test's crash exit returned during init: init ends with the status Core's exit has, 0.
    crash_returned: bool,
    /// `Random_SanityCheck`'s samples, taken before the daemon fork and mixed into the process
    /// RNG after it.
    rng_sanity: Option<dashd_random::SanityTimings>,
    /// Runs as the chainstate opens (tests: the clock moving during the load).
    #[cfg(test)]
    during_load: Option<LoadHook>,
    /// LK1a F's plant on route B's LLMQ reads, armed by
    /// [`fail_route_b_lists`](Self::fail_route_b_lists).
    #[cfg(test)]
    route_b_list_unavailable: Arc<std::sync::atomic::AtomicBool>,
    /// The masternode tip list step 7c installs in place of V's published one.
    #[cfg(test)]
    tip_override: Option<TipMnList>,
    /// Runs [`init::spv_preempt`] as if `-spv` and `-spvwallet` were served.
    #[cfg(test)]
    spv_served_as: bool,
}

/// A test's call before a held startup ABC ([`Node::hold_startup_abc`]): V and `mn_sync`.
#[cfg(all(test, feature = "test-support"))]
pub(crate) type BeforeHeldImport = Box<dyn FnOnce(&Chain, &dyn dashd_rpc::dash::MnSync) + Send>;

/// A test's step inside the chainstate load, with the node's quorum manager as it is then.
#[cfg(test)]
type LoadHook = Box<dyn Fn(&MockClock, Option<&init::quorums::NodeQuorums>)>;

impl Node {
    /// A node for `args`, which [`init::init_config`] has read with `params` selected.
    pub fn new(args: Arc<ArgsManager>, ui: Arc<Ui>, params: SelectedParams) -> Node {
        let clock = Arc::new(MockClock::new());
        ui.logger().set_clock(Arc::clone(&clock));
        let shutdown = Arc::new(ShutdownState::new());
        let warnings = Arc::new(Warnings::new());
        let abort =
            Arc::new(NodeAbort::new(Arc::clone(&ui), Arc::clone(&warnings), Arc::clone(&shutdown)));
        let mut steps =
            ChainSteps::new(Arc::clone(ui.logger()), Arc::clone(&shutdown), Arc::clone(&warnings));
        let rpc_chain: Arc<rpc::ChainSlot> = Arc::default();
        steps.set_block_change(Arc::clone(rpc_chain.block_change()));
        Node {
            args,
            ui,
            clock,
            shutdown,
            warnings,
            abort,
            params,
            unsupported: None,
            i2p_accept_refusal: None,
            storage_request: Ok(None),
            interaction: None,
            steps,
            net: None,
            spv: None,
            scheduler: None,
            cl_scheduler: None,
            cl_tip: None,
            rpc: None,
            rpc_chain,
            quorums: None,
            txindex: None,
            signing: None,
            filters: None,
            coinstats: None,
            #[cfg(feature = "dkg-runtime")]
            dkg: None,
            lock_signers: None,
            rpc_net: Arc::default(),
            rpc_sync: Arc::default(),
            pid: None,
            lock: None,
            interrupted: AtomicBool::new(false),
            shut_down: false,
            crash_exit: Arc::new(|| exit_now(0)),
            crash_returned: false,
            rng_sanity: None,
            #[cfg(test)]
            during_load: None,
            #[cfg(test)]
            route_b_list_unavailable: Arc::default(),
            #[cfg(test)]
            tip_override: None,
            #[cfg(test)]
            spv_served_as: false,
        }
    }

    pub fn params(&self) -> &SelectedParams {
        &self.params
    }

    /// Shows `failure` as Core's `AppInit` does, and marks init failed: `false`. After a test's
    /// crash exit returned, there is nothing to show and the status stays 0.
    pub fn fail(&self, failure: InitFailure) -> bool {
        if self.crash_returned {
            return false;
        }
        match failure {
            InitFailure::Error(msg) => self.ui.init_error(&msg),
            InitFailure::Exception(what) => host::print_exception_continue("AppInit()", &what),
            InitFailure::Reported => {}
        }
        self.shutdown.set_exit_status(EXIT_FAILURE);
        false
    }

    /// `AppInit` before the daemon fork: logging, parameter interaction, the sanity checks and
    /// the data directory probe. `false` once the failure is shown.
    // Core: bitcoind.cpp:172-196
    pub fn app_init_early(&mut self) -> bool {
        let r = self.try_app_init_early();
        r.map_or_else(|f| self.fail(f), |()| true)
    }

    fn try_app_init_early(&mut self) -> Result<(), InitFailure> {
        let args = &*self.args;
        let logger = self.ui.logger();
        self.spv_preempt(args)?;
        let unsupported = init::unsupported_option(args)?;
        self.unsupported = unsupported;
        self.storage_request = storage_request(args, &self.params.chain)?;
        // -server defaults to true for dashd.
        args.soft_set_bool_arg("-server", true);
        // InitLogging: early, so parameter interaction reaches the console.
        logging::set_logging_options(logger, args)?;
        logging::log_package_version(logger);
        init::init_parameter_interaction(args, logger)?;
        self.i2p_accept_refusal = init::i2p_accept_refusal(args)?;
        // AppInitBasicSetup: the signal handlers are the binary's.
        self.interaction = Some(init::app_init_parameter_interaction(
            &self.ui,
            args,
            &self.params.chain,
            &self.clock,
        )?);
        // AppInitSanityChecks: of kernel::SanityChecks, the ECC and BLS checks test C++ runtime
        // state with no Rust counterpart; Random_SanityCheck's checks run here, and their samples
        // are mixed into the process RNG once it is seeded. Then the lock probe.
        // (`init::init_config` already refused a Dash Core data directory.)
        // Core: init.cpp:1483-1494, kernel/checks.cpp:26-28
        let Some(rng_sanity) = dashd_random::random_sanity_check() else {
            self.ui.init_error("OS cryptographic RNG sanity check failure. Aborting.");
            return Err(InitFailure::Error(format!(
                "Initialization sanity check failed. {} is shutting down.",
                crate::PACKAGE_NAME
            )));
        };
        self.rng_sanity = Some(rng_sanity);
        datadir::lock_data_directory(args, true)?;
        Ok(())
    }

    /// [`init::spv_preempt`]. A test may run it as if `-spv` and `-spvwallet` were served.
    fn spv_preempt(&self, args: &ArgsManager) -> Result<(), InitFailure> {
        #[cfg(test)]
        if self.spv_served_as {
            return init::spv_preempt_as(args, true, true);
        }
        init::spv_preempt(args)
    }

    /// `AppInit` after the daemon fork: the data directory lock and `AppInitMain`.
    // Core: bitcoind.cpp:225-233
    pub fn app_init_main(&mut self) -> bool {
        let r = self.try_app_init_main();
        r.map_or_else(|f| self.fail(f), |()| true)
    }

    /// [`app_init_main`](Self::app_init_main) in SPV mode: the common start, then `host` in
    /// place of steps 6 to 12. The node keeps `host` and stops it in [`shutdown`](Self::shutdown),
    /// whether init succeeded or not.
    pub fn app_init_main_spv(&mut self, host: Box<dyn SpvHost>) -> bool {
        self.spv = Some(host);
        let r = self.try_app_init_main_spv();
        r.map_or_else(|f| self.fail(f), |()| true)
    }

    fn try_app_init_main_spv(&mut self) -> Result<(), InitFailure> {
        self.app_init_main_prefix()?;
        let args = &*self.args;
        let logger = self.ui.logger();
        // The SPV store's directory may not exist yet, so the disk space check reads its parent.
        let data_dir = args.get_data_dir_net()?;
        let scheduler = self.scheduler.insert(Scheduler::new());
        start_scheduler(scheduler, &self.ui, &self.shutdown, data_dir.clone())?;
        let scheduler = scheduler.clone();

        // The RPC server starts in warmup, with SPV mode's table and the host's commands.
        // Core: init.cpp:1602-1638
        if args.get_bool_arg("-server", false)? {
            let host = self.spv.as_ref().expect("app_init_main_spv set it");
            let mockable = ChainPolicy::for_setup(&self.params.chain, false).is_mockable_chain;
            let table = rpc::spv_command_table(
                &self.params.chain,
                logger,
                &self.shutdown,
                &self.clock,
                (mockable, &scheduler),
            );
            host.register_rpc(&table);
            self.rpc = Some(rpc::Servers::start(&self.ui, args, &table, &self.shutdown)?);
        }

        if let Some(refusal) = self.unsupported.take().or_else(|| self.i2p_accept_refusal.take()) {
            return Err(InitFailure::Error(refusal));
        }
        if let Err(refusal) = &self.storage_request {
            return Err(InitFailure::Error(refusal.clone()));
        }
        // Core: init.cpp:2570-2573
        if self.shutdown.shutdown_requested() {
            logger.log_printf("Shutdown requested. Exiting.\n");
            return Err(InitFailure::Reported);
        }

        // Step 12: start node.
        let host = self.spv.as_mut().expect("app_init_main_spv set it");
        host.start(SpvStart {
            args,
            chain: &self.params.chain,
            spv_dir: data_dir.join(SPV_DIR),
            abort: Arc::clone(&self.abort),
            shutdown: Arc::clone(&self.shutdown),
            clock: Arc::clone(&self.clock),
            scheduler,
            ui: Arc::clone(&self.ui),
        })?;

        // Step 13: finished.
        if let Some(rpc) = &self.rpc {
            rpc.set_warmup_finished();
        }
        self.ui.init_message("Done loading");
        notify::startup_notify(logger, args)
    }

    /// The start of `AppInitMain` both modes run: the process RNG, the data directory lock,
    /// `-maxuploadtarget`, then step 4a's pid file, logging and its first lines.
    fn app_init_main_prefix(&mut self) -> Result<(), InitFailure> {
        // The process RNG is made and seeded in the serving process after daemonizing: the
        // first draw (Core's static initializers), RandomInit, then Random_SanityCheck's mix.
        // Core: saltedhasher.cpp:8-10, kernel/context.cpp:25, random.cpp:734-738
        dashd_random::static_init();
        dashd_random::random_init();
        if let Some(t) = self.rng_sanity.take() {
            dashd_random::mix_sanity_timings(t);
        }
        let args = &*self.args;
        let logger = self.ui.logger();
        self.lock = datadir::lock_data_directory(args, false)?;
        let net = &self.interaction.as_ref().expect("app_init_early succeeded").net;
        let (max_connections, fd) = (net.max_connections, net.fd);

        // Core: init.cpp:1520-1523
        max_upload_target(args)?;

        // Step 4a: application initialization.
        self.pid = Some(PidFile::create(args)?);
        logging::start_logging(&self.ui, args, &args.get_default_data_dir())?;
        logger.log_printf(&format!(
            "Using at most {max_connections} automatic connections ({fd} file descriptors available)\n"
        ));
        if args.is_arg_set("-datadir") {
            let datadir = args.get_path_arg("-datadir", Path::new(""))?;
            if !datadir.is_absolute() {
                let cwd = std::env::current_dir().unwrap_or_default();
                logger.log_printf(&format!(
                    "Warning: relative datadir option '{}' specified, which will be interpreted relative to the current working directory '{}'. This is fragile, because if {} is started in the future from a different location, it will be unable to locate the current data files. There could also be data loss if {} is started while in a temporary directory.\n",
                    args.get_arg("-datadir", "")?,
                    cwd.display(),
                    crate::PACKAGE_NAME,
                    crate::PACKAGE_NAME,
                ));
            }
        }
        Ok(())
    }

    fn try_app_init_main(&mut self) -> Result<(), InitFailure> {
        self.app_init_main_prefix()?;
        let args = &*self.args;
        let logger = self.ui.logger();
        let digest_log = open_log(args)?;
        let caches = Arc::new(init_validation_caches(logger, args)?);
        let queue = Arc::new(start_script_check_threads(logger, args)?);

        let blocks_dir = args.get_blocks_dir_path()?;
        let scheduler = self.scheduler.insert(Scheduler::new());
        start_scheduler(scheduler, &self.ui, &self.shutdown, blocks_dir.clone())?;

        // The RPC server starts in warmup: requests are answered with the init message until
        // step 13.
        // Core: init.cpp:1602-1638
        if args.get_bool_arg("-server", false)? {
            let deprecated = rpc::DeprecatedRpc {
                banscore: dashd_rpc::server::is_deprecated_rpc_enabled(args, "banscore"),
                softforks: dashd_rpc::server::is_deprecated_rpc_enabled(args, "softforks"),
                service: dashd_rpc::server::is_deprecated_rpc_enabled(args, "service"),
                permissive_bool: dashd_rpc::server::is_deprecated_rpc_enabled(
                    args,
                    "permissive_bool",
                ),
            };
            let table = rpc::command_table(
                &self.params.chain,
                logger,
                &self.shutdown,
                &self.clock,
                (ChainPolicy::for_setup(&self.params.chain, false).is_mockable_chain, &*scheduler),
                &self.rpc_chain,
                &self.rpc_net,
                &self.rpc_sync,
                &self.warnings,
                deprecated,
                args.get_data_dir_net()?,
            );
            self.rpc = Some(rpc::Servers::start(&self.ui, args, &table, &self.shutdown)?);
        }

        // Step 6: network initialization: the network core with `peers.dat` and the ban list
        // (P3 D113), `sporkman`, `mn_metaman` and `netfulfilledman` (SP1), then the port,
        // socket-address and `-uacomment` checks (BL-327 D2). The masternode role is fixed here
        // for step 7c and step 12 (BL-703).
        // Core: init.cpp:1649-1890
        let net = &self.interaction.as_ref().expect("app_init_early succeeded").net;
        let role = p2p::MasternodeRole::from_args(args);
        let net = self.net.insert(p2p::Net::new(
            args,
            &Arc::new(self.params.chain.clone()),
            net,
            role,
            Arc::clone(&self.clock) as _,
            &args.get_data_dir_net()?,
        )?);
        // Core: init.cpp:1746-1752
        if let Some(warning) = &self.params.chain.spork_signers.warning {
            self.ui.init_warning(warning);
        }
        self.steps.make_sporks(args, &self.params, Arc::clone(&self.clock) as _)?;
        self.rpc_sync.attach_sporks(self.steps.sporks().expect("made above"));
        self.steps.make_net_caches(&self.params, Arc::clone(&self.clock) as _);
        self.steps.set_meta(&self.rpc_chain);
        net.check_options(args, logger)?;

        // Step 7a: load sporks.
        self.steps.load_sporks(args, &self.params)?;

        // Step 7: load the chainstate.
        // An `Err` returns in rdashd's refusal slot below, before the tuning opens anything.
        let requested = self.storage_request.as_ref().ok().copied().flatten();
        let (tuning, live_copy) = storage_profile(&self.params.chain, requested, logger);
        let (sizes, mut setup) = coins_configuration(
            args,
            &self.params.chain,
            self.interaction.as_ref().expect("app_init_early succeeded"),
            logger,
            Arc::new(tuning.clone()),
            live_copy,
            caches,
            queue,
            Arc::clone(self.steps.metaman().expect("made at step 6")),
            Arc::clone(&self.crash_exit),
        )?;
        setup.digest_log = digest_log;
        #[cfg(test)]
        {
            setup.route_b_list_unavailable = Some(Arc::clone(&self.route_b_list_unavailable));
        }
        let batch_size = db_batch_size(args)?;
        let crash_ratio = setup.crash_ratio;

        // rdashd: an option it does not support yet, after every argument check Core makes and
        // before anything is read from or written to the chainstate.
        if let Some(refusal) = self.unsupported.take() {
            return Err(InitFailure::Error(refusal));
        }
        if let Some(refusal) = self.i2p_accept_refusal.take() {
            // Core: init.cpp:2742-2749
            self.net.as_ref().expect("step 6 built it").i2psam_proxy(args)?;
            return Err(InitFailure::Error(refusal));
        }
        if let Err(refusal) = &self.storage_request {
            return Err(InitFailure::Error(refusal.clone()));
        }
        #[cfg(feature = "test-hooks")]
        if let Some(refusal) = hook_profile_refusal(requested) {
            return Err(InitFailure::Error(refusal));
        }

        // Core's loop runs while nothing is loaded and no shutdown is requested; without a GUI
        // it runs at most once.
        // Core: init.cpp:2025-2138
        let mut sync = None;
        if !self.shutdown.shutdown_requested() {
            self.ui.init_message("Loading block index…");
            // `mn_sync`, before `LoadChainstate`: its asset start time is the load's start. Its
            // listener registers at step 7c.
            // Core: init.cpp:2086
            let flags = &self.interaction.as_ref().expect("app_init_early succeeded").flags;
            let mut parts = SyncParts::new(
                args,
                &self.params.chain,
                Arc::clone(self.net.as_ref().expect("step 6 built it").core()),
                Arc::clone(&self.clock) as _,
                Arc::clone(self.steps.netfulfilled().expect("made at step 6")),
                flags.governance_enabled,
            )?;
            self.rpc_sync.attach(&parts.sync);
            self.steps.is_sync = Some(Arc::clone(&parts.sync));
            // `node.govman`, unloaded, before the chainstate: the connector's helper holds its
            // superblock back end.
            // Core: init.cpp v24.0.2:2149
            let governance = crate::governance::NodeGovernance::new(
                flags.governance_enabled,
                &self.params.chain,
                Arc::clone(self.net.as_ref().expect("step 6 built it").core()),
                Arc::clone(&parts.sync),
                Arc::clone(self.steps.metaman().expect("made at step 6")),
            )?;
            parts.set_governance(governance.sync_seam());
            setup.governance = Some(Arc::clone(&governance));
            self.steps.governance = Some(governance);
            sync = Some(parts);
            let started = Instant::now();
            let data_dir = args.get_data_dir_net()?;
            self.steps.is_open = Some(crate::instantsend::rocks_opener(&data_dir));
            let chain = Arc::new(self.params.chain.clone());
            let clock = Arc::clone(&self.clock) as _;
            let max_recsigs_age = init::recsigs::max_recsigs_age(args)?;
            self.quorums = Some(init::quorums::init_node_quorums(
                &chain,
                &data_dir,
                max_recsigs_age,
                &mut setup.quorum_hook,
                &mut setup.qman,
            ));
            let opened = self.steps.load(
                args,
                &self.ui,
                chain,
                clock,
                Arc::clone(&self.abort),
                setup,
                || {
                    #[cfg(test)]
                    if let Some(hook) = &self.during_load {
                        hook(&self.clock, self.quorums.as_ref());
                    }
                    Opening::open_node(
                        &data_dir,
                        &blocks_dir,
                        &self.params.chain,
                        &sizes,
                        batch_size,
                        crash_ratio,
                        &tuning,
                        Arc::clone(logger),
                        Arc::clone(&self.abort) as _,
                    )
                },
            )?;
            match opened {
                Ok(()) => {
                    let files = self.steps.take_rpc_files();
                    let chain = self.steps.chain().expect("a loaded chainstate runs V");
                    // The node's one block assembler (MN0 r1), with `-blockversion` (MN0 D3),
                    // `ConfiguredOptions()` and the node's pool (MN1-7).
                    // Core: v24.0.2 src/node/miner.cpp:66-101
                    let mut options = dashd_mining::Options::default();
                    dashd_mining::ApplyArgsManOptions(args, &mut options)?;
                    let mut miner = ChainTemplateSource::new(
                        chain.handle().clone(),
                        Arc::new(self.params.chain.clone()),
                        Arc::clone(&self.clock) as _,
                        dashd_mining::block_version_arg(args, &self.params.chain)?,
                    )
                    .with_options(dashd_mining::Options::new(options))
                    .cjdns_reachable(crate::p2p::cjdns_reachable(args)?);
                    let pool = self.steps.mempool().map(|mempool| {
                        Arc::new(MiningPool::new(Arc::clone(mempool))) as Arc<dyn MiningMempool>
                    });
                    if let Some(pool) = &pool {
                        miner = miner.with_mempool(Arc::clone(pool));
                    }
                    if let Some(is) = self.steps.instantsend() {
                        let latch = self.steps.latch().expect("a loaded chainstate");
                        let hooks = is.accept_hooks(latch, Arc::clone(&self.abort) as _);
                        miner = miner.with_instant_send(
                            Arc::new(MiningInstantSend::new(hooks)),
                            Arc::new(NotYetSafe),
                        );
                    }
                    let coins = self.steps.coins_cell().map(|c| rpc::coins_read(Arc::clone(c)));
                    self.quorums.as_ref().expect("made before the load").bind_chain(chain.handle());
                    let miner = Arc::new(miner);
                    let gbt = GbtEngine::new(GbtParts {
                        chain: Arc::new(
                            ChainGbt::new(chain.handle().clone(), &self.params.chain)
                                .map_err(InitFailure::Exception)?,
                        ),
                        source: Arc::clone(&miner) as _,
                        mempool: pool,
                        status: Arc::new(rpc::GbtStatus::new(
                            &self.rpc_chain,
                            &self.rpc_net,
                            &self.rpc_sync,
                        )),
                        clock: Arc::clone(&self.clock) as _,
                        params: GbtParams::of(&self.params.chain)
                            .map_err(InitFailure::Exception)?,
                    });
                    self.rpc_chain.attach(
                        chain.handle(),
                        chain.signals(),
                        files,
                        Some(miner),
                        coins,
                    );
                    self.rpc_chain.attach_gbt(Arc::new(gbt));
                    if let Some(mempool) = self.steps.mempool() {
                        self.rpc_chain.attach_mempool(Arc::clone(mempool));
                    }
                    if let Some(is) = self.steps.instantsend() {
                        self.rpc_chain.attach_instantsend(Arc::clone(&is.manager));
                    }
                    logger.log_printf(&format!(
                        " block index {:15}ms\n",
                        started.elapsed().as_millis()
                    ));
                }
                // CH2 has logged "Simulating a crash. Goodbye."; Core calls `_Exit(0)`. Only a
                // test's exit returns.
                Err(OpenError::Crashed) => {
                    (self.crash_exit)();
                    self.crash_returned = true;
                    return Err(InitFailure::Reported);
                }
                Err(OpenError::Incompatible(message) | OpenError::InsufficientDbcache(message)) => {
                    return Err(InitFailure::Error(message));
                }
                // Core: node/chainstate.cpp:177, :184; the exit below.
                Err(OpenError::Interrupted(_)) => {
                    debug_assert!(self.shutdown.shutdown_requested(), "interrupted by a shutdown");
                }
                // Without a GUI, Core's reindex question shows its non-interactive text and is
                // declined. Under `-reindex` there is no question: the message is init's error.
                // Core: init.cpp:2109-2126, noui.cpp:22-54
                Err(OpenError::Recoverable {
                    message,
                    cause,
                }) => {
                    if let Some(cause) = cause {
                        logger.log_printf(&format!("{cause}\n"));
                    }
                    if self.steps.reindex_arg && !self.shutdown.shutdown_requested() {
                        return Err(InitFailure::Error(message));
                    }
                    if !self.shutdown.shutdown_requested() {
                        self.ui.thread_safe_question(&reindex_question(&message));
                        logger.log_printf("Aborted block database rebuild. Exiting.\n");
                        return Err(InitFailure::Reported);
                    }
                }
            }
        }
        if self.shutdown.shutdown_requested() {
            logger.log_printf("Shutdown requested. Exiting.\n");
            return Err(InitFailure::Reported);
        }
        // The ChainLock handler, with its own scheduler and `cl-schdlr` thread; idle until step
        // 10a.
        // Core: init.cpp:2152, chainlock/handler.cpp:43-55
        let cl = Scheduler::new();
        cl.start_as("cl-schdlr").map_err(|e| InitFailure::Exception(e.to_string()))?;
        self.cl_scheduler = Some(cl);

        // The indexes register before step 7c's services, so their handles exist there; Step 8
        // opens them.
        let filters =
            self.interaction.as_ref().expect("app_init_early succeeded").net.basic_filter_index;
        let handles =
            self.steps.register_indexes(args, &sizes, filters, indexes::driver_config(&tuning))?;
        (self.txindex, self.filters, self.coinstats) =
            (handles.txindex, handles.filters, handles.coinstats);
        if let Some(governance) = &self.steps.governance {
            governance.bind_index(self.txindex.clone().map(|t| t as _));
        }
        self.rpc_chain.attach_indexes(
            self.txindex.clone(),
            self.filters.clone(),
            self.steps.indexes().cloned(),
        );

        // Step 7c: `PeerManager::make` and its registration on V's signals, and the
        // `CDSNotificationInterface` with step 7's `mn_sync`, after the load and before step
        // 11's ABC (P3 D112), then the fulfilled-request and masternode caches (SP1).
        // Core: init.cpp:2194-2199, :2225-2228, :2273-2289
        let sync = sync.expect("made before the load, which ran: no shutdown was requested");
        let files = self.steps.take_serve_files();
        let chain = self.steps.chain().expect("step 7 started V");
        let sporks = self.steps.sporks().expect("step 7a made sporkman");
        let net = self.net.as_mut().expect("step 6 built it");
        let tip = p2p::tip_mn_list::new(chain.handle().view_handle());
        #[cfg(test)]
        let tip = self.tip_override.clone().unwrap_or(tip);
        // Core: v24.0.0 src/init.cpp:2152-2154
        let recovery = args.get_bool_arg(
            "-llmq-data-recovery",
            init::consts::DEFAULT_ENABLE_QUORUM_DATA_RECOVERY,
        )?;
        let watch = args.get_bool_arg("-watchquorums", init::consts::DEFAULT_WATCH_QUORUMS)?;
        net.set_quorums_watch(watch);
        // Core: init.cpp:2156-2160 at v24.0.0
        let active = dashd_masternode::active::load(args)?;
        if let Some(active) = &active {
            self.rpc_chain.set_active(Arc::clone(active));
        }
        let signer = mnauth_signer(active.as_ref(), &tip);
        let metaman = self.steps.metaman().expect("made at step 6");
        // Core: init.cpp:2139-2140 at v24.0.0
        let limits = dash_coinjoin::dstx::PoolLimits::of(&self.params.chain);
        let mut coinjoin = CoinJoin::new(limits, Arc::clone(metaman), active.clone());
        if let Some(mempool) =
            crate::coinjoin::mempool::NodeMempool::of(&self.steps, chain.handle())
        {
            coinjoin = coinjoin.with_mempool(mempool);
        }
        let flags = &self.interaction.as_ref().expect("app_init_early succeeded").flags;
        let quorum_options = p2p::quorum_net::QuorumOptions {
            role: net.role().quorum_role(),
            recovery,
            sync: flags.qvvec_sync.iter().map(|&(t, mode)| (t, mode.into())).collect(),
        };
        let params = Arc::new(self.params.chain.clone());
        let quorums = self.quorums.as_ref().expect("made at step 7");
        let quorum_net = p2p::quorum_net::quorum_net(
            &params,
            quorums,
            chain.handle(),
            net.core(),
            &sync.sync,
            sporks,
            active.clone(),
            quorum_options.role,
        );
        #[cfg(feature = "dkg-runtime")]
        let quorum_net_dkg = quorum_net.net_dkg;
        net.set_quorum_handlers(quorum_net.handlers);
        let (net_quorum, quorum_source) = (quorum_net.net_quorum, quorum_net.source);
        let tip_list = tip.clone();
        if let Some(is) = self.steps.instantsend() {
            net.set_instantsend(p2p::net_instantsend::IsNetParts {
                is: is.clone(),
                params: Arc::clone(&params),
                chain: chain.handle().clone(),
                chainlocks: Arc::new(crate::instantsend::NodeChainLocks {
                    view: chain.handle().view_handle(),
                    sporks: Arc::clone(sporks),
                }),
                txs: Arc::new(crate::locks::signers::NodeTxSource {
                    mempool: self.steps.mempool().map(|m| m.handle()),
                    txindex: self.txindex.clone(),
                }),
                mempool: self.steps.mempool().map(|m| Arc::clone(m.pool()) as _),
                masternode: active.is_some(),
            });
        }
        if let Some(handle) = &self.txindex {
            net.set_txindex(Arc::clone(handle));
        }
        if let Some(governance) = &self.steps.governance {
            net.set_governance(Arc::clone(governance));
        }
        net.attach(
            chain,
            files,
            Arc::clone(&self.clock) as _,
            sporks,
            &sync,
            tip,
            signer,
            metaman,
            active.clone(),
            coinjoin,
            self.steps.mempool().map(|m| m.handle()),
        )?;
        // P7's `CheckQuorumConnections`, `EnsureQuorumConnections` and
        // `AddQuorumProbeConnections`: `NetQuorumTip` calls the check, DK1's rounds the rest.
        let connections = Arc::new(p2p::NodeQuorumConnections::new(p2p::QuorumConnParts {
            params: Arc::clone(&params),
            chain: Arc::new(chain.handle().clone()),
            registry: Arc::clone(net.quorum_nodes()),
            core: Arc::clone(net.core()),
            sporks: Arc::clone(sporks),
            metaman: Arc::clone(metaman),
            tip_list,
            clock: Arc::clone(&self.clock) as _,
            watch_seed: *dashd_masternode::mnconn::watch_seed(),
        }));
        let (tip_worker, tip_thread, armed) = p2p::quorum_net::spawn_tip_listener(
            &params,
            quorums,
            &net_quorum,
            quorum_source,
            chain.handle(),
            &sync.sync,
            active.clone(),
            &quorum_options,
            Arc::clone(&connections) as _,
            Arc::clone(&self.abort) as _,
        )
        .map_err(|e| InitFailure::Exception(e.to_string()))?;
        #[cfg(feature = "dkg-runtime")]
        let quorum_tip = tip_worker.barrier();
        net.attach_quorum_tip(tip_worker, tip_thread);
        #[cfg(feature = "dkg-runtime")]
        let dkg = init::dkg::NodeDkg::build(init::dkg::DkgParts {
            params: Arc::clone(&params),
            net_dir: &args.get_data_dir_net()?,
            wipe: self.steps.reindex || args.get_bool_arg("-reindex-chainstate", false)?,
            watch,
            active: active.clone(),
            chain: chain.handle().clone(),
            signals: chain.signals().clone(),
            core: Arc::clone(net.core()),
            shared: Arc::clone(net.shared().expect("attached above")),
            sporks: Arc::clone(sporks),
            metaman: Arc::clone(metaman),
            connections,
            registry: Arc::clone(net.quorum_nodes()),
            net_dkg: quorum_net_dkg,
            managers: quorums.dkg_managers().clone(),
            abort: Arc::clone(&self.abort) as _,
            quorum_tip,
        })?;
        #[cfg(not(feature = "dkg-runtime"))]
        let _ = connections;
        #[cfg(feature = "dkg-runtime")]
        let duties = dkg.as_ref().and_then(init::dkg::NodeDkg::duties);
        #[cfg(not(feature = "dkg-runtime"))]
        let duties = None;
        #[cfg(feature = "dkg-runtime")]
        if let Some(dkg) = &dkg {
            self.rpc_chain.set_dkg_status(dkg.status());
            if let Some(interrupt) = dkg.interrupter() {
                net.on_interrupt_handlers(interrupt);
            }
        }
        #[cfg(feature = "dkg-runtime")]
        {
            self.dkg = dkg;
        }
        let masternode = active.is_some();
        let (view, sporks) = (chain.handle().view_handle(), Arc::clone(sporks));
        let rpc_view = chain.handle().view_handle();
        if let Some(startup) =
            net.attach_active(chain, active.clone(), duties, Arc::clone(&self.abort) as _)?
        {
            self.steps.set_masternode_startup(Box::new(p2p::quorum_net::ArmAfterInit {
                inner: startup,
                armed,
            }));
        }
        // The relay binds to the processor made above, as `NotifyRecoveredSig` reaches
        // `PeerManager` once it is registered.
        // Core: v24.0.2 src/init.cpp:2202
        let shared = Arc::clone(net.shared().expect("attached above"));
        let relay = Arc::new(signing::RelayRecoveredSigs::default());
        relay.bind(&shared);
        // Core: v24.0.2 src/init.cpp:2163-2170
        let active = active.map(|active| {
            let abort = Arc::clone(&self.abort) as _;
            init::signing::ActiveSigning {
                active,
                sporks,
                net: Arc::new(signing::shares::SharesNet {
                    shared: Arc::clone(&shared),
                    core: Arc::clone(net.core()),
                    quorums: Arc::clone(net.quorum_nodes()),
                }),
                legacy: Box::new(move || view.view().bls.is_legacy()),
                abort: init::signing::share_abort(abort, Arc::clone(&self.shutdown)),
            }
        });
        let mut node_signing = init::signing::NodeSigning::new(
            self.quorums.as_ref().expect("made at step 7"),
            Arc::new(self.params.chain.clone()),
            Arc::new(init::signing::NodeSigningNet(shared)),
            relay,
            Arc::clone(&self.clock) as _,
            active,
        );
        // Step 7d: NetInstantSend's `m_sigman`, then `AddExtraHandler(NetSigning)`.
        // Core: v24.0.2 src/init.cpp:2201-2202
        let runtime = Arc::clone(node_signing.runtime());
        let faults = Arc::new(signing::SigningFaults::new(Arc::clone(&self.abort) as _));
        if let Some(is) = self.steps.instantsend() {
            let recsigs = signing::NodeRecSigs::new(Arc::clone(&runtime), Arc::clone(&faults));
            is.recsigs.bind(Arc::new(recsigs));
        }
        net.add_signing(Arc::new(signing::P2pSigning::new(runtime, faults)));
        if let Some(shares) = node_signing.shares() {
            net.add_sig_shares(Arc::new(signing::shares::P2pSigShares(Arc::clone(shares))));
        }
        if masternode {
            let signers = self
                .lock_signers
                .insert(self.build_lock_signers(&sync.sync, node_signing.runtime())?);
            // Core: v24.0.2 src/active/context.cpp:58-59
            for listener in signers.recovered_sig_listeners() {
                assert!(node_signing.add_active_signer(listener), "a masternode signs shares");
            }
            self.steps.attach_is_signer(|| signers.listener_signer());
        }
        self.signing = Some(node_signing);
        let runtime = self.signing().expect("made above");
        self.rpc_chain.set_signing(Arc::clone(runtime) as _);
        let node_signing = self.signing.as_ref().expect("made above");
        self.rpc_chain.set_sign(crate::rpc::evo::RpcSign::new(
            Arc::new(self.params.chain.clone()),
            Box::new(move || rpc_view.view().bls.is_legacy()),
            Arc::clone(runtime) as _,
            Arc::clone(node_signing.lookup()) as _,
            node_signing.shares().map(|s| Arc::clone(s) as _),
        ));
        self.steps.load_net_caches(args, &self.params)?;

        // Step 8: start indexers. The compact-filter handlers serve from the filter index's
        // handle before step 12 starts the connection manager.
        // Core: v24.0.0 src/init.cpp:2305-2310
        self.steps.start_indexes()?;
        if let (Some(net), Some(filters)) = (&self.net, &self.filters) {
            net.attach_filters(Arc::clone(filters) as _);
        }

        // Step 10a: the ChainLock handler's 5 s task, then the Dash tasks on the scheduler.
        // Core: init.cpp:2366-2368, :2389-2396
        // Core: v24.0.0 src/init.cpp:2360, :2365 StartHandlers, ScheduleHandlers
        let scheduler = self.scheduler.as_ref().expect("started at step 4a");
        if let Some(net) = &mut self.net {
            net.start_instantsend(Arc::clone(&self.abort) as _)?;
        }
        net_quorum.start();
        #[cfg(feature = "dkg-runtime")]
        if let Some(dkg) = &self.dkg {
            dkg.start()?;
        }
        p2p::quorum_net::schedule(&net_quorum, scheduler);
        #[cfg(feature = "dkg-runtime")]
        if let Some(dkg) = &self.dkg {
            dkg.schedule_cleanup(scheduler);
        }
        let chain = self.steps.chain().expect("step 7 started V");
        // Core: v24.0.2 src/init.cpp:2363
        let signing = self.signing.as_mut().expect("made at step 7c");
        let view = chain.handle().view_handle();
        let legacy = Box::new(move || view.view().bls.is_legacy());
        signing
            .start(legacy, FastRandom::new(), Arc::clone(&self.abort) as _)
            .map_err(|e| InitFailure::Exception(e.to_string()))?;
        let cleanup = self.net.as_ref().and_then(p2p::Net::chainlock_cleanup);
        let cl = self.cl_scheduler.as_ref().expect("made before step 7c");
        start_chainlock_handler(cl, chain, cleanup.clone(), Arc::clone(&sync.sync));
        // The handler's `UpdatedBlockTip`. Core registers the handler as it makes it, before
        // step 7c; no tip is signalled before step 11's ABC, so registering here misses none.
        // Core: init.cpp:2153
        if let Some(cleanup) = cleanup {
            let listener: Arc<dyn ValidationInterface> =
                Arc::new(ClTipCleanup::new(cl.clone(), cleanup, Arc::clone(&sync.sync)));
            chain.signals().register(Arc::clone(&listener));
            self.cl_tip = Some((chain.signals().clone(), listener));
        }
        let scheduler = self.scheduler.as_ref().expect("started at step 4a");
        let net = self.net.as_ref().expect("step 6 built it");
        let maintenance = MnMaintenance::new(
            Arc::clone(net.core()),
            Arc::clone(&sync.sync),
            Arc::clone(net.quorum_nodes()),
            Arc::clone(&self.shutdown),
            Arc::clone(self.ui.logger()),
        );
        // `NetGovernance::Schedule`, in handler order before `SyncManager`'s.
        // Core: init.cpp v24.0.2:2368; governance/net_governance.cpp v24.0.2:39-78
        net.schedule_governance(scheduler);
        sync.schedule(scheduler, &self.shutdown, maintenance, &net.coinjoin_ticks());
        // Core: v24.0.2 src/init.cpp:2370-2371, src/active/context.cpp:55-63
        if let Some(signers) = &self.lock_signers {
            signers.start();
        }
        self.signing.as_mut().expect("made at step 7c").start_active();
        if let Some(signers) = &self.lock_signers {
            signers.subscribe();
        }

        // Step 11: import blocks. `ThreadImport` sends V's startup ABC, then init waits for
        // genesis. Its thread then broadcasts the tip, as `InitializeCurrentBlockTip` to every
        // subscriber: a node that is not a masternode has no `active_ctx` (CH7 §6.3 step 6).
        // Core: init.cpp:2448-2468
        #[cfg(feature = "test-hooks")]
        arm_after_scan(&mut self.steps, &self.shutdown);
        self.steps.import(args)?;
        // Core: init.cpp:2570-2573
        if self.shutdown.shutdown_requested() {
            logger.log_printf("Shutdown requested. Exiting.\n");
            return Err(InitFailure::Reported);
        }

        // Step 12: start node.
        // Core: init.cpp:2575-2768
        let net = self.net.as_mut().expect("step 6 built it");
        let metaman = self.steps.metaman().expect("made at step 6");
        net.start(args, &self.ui, Arc::clone(&self.abort) as _, metaman)?;
        let (shared, connman) = (net.shared().expect("attached"), net.connman().expect("started"));
        self.rpc_net.attach(net.core(), shared, connman, net.runtime_handle());

        // Step 13: finished.
        // Core: init.cpp:2769-2800
        #[cfg(feature = "test-hooks")]
        self.hold_warmup(args);
        #[cfg(feature = "test-hooks")]
        self.arm_f01(args);
        #[cfg(feature = "test-hooks")]
        self.arm_v_panic();
        #[cfg(feature = "test-hooks")]
        self.arm_live_copy();
        if let Some(rpc) = &self.rpc {
            self.rpc_chain.notify_loaded_tip();
            rpc.set_warmup_finished();
        }
        self.ui.init_message("Done loading");
        notify::startup_notify(logger, args)
    }

    /// `RDASHD_TEST_HOOK=hold-warmup`: init waits before it ends the RPC warmup until the file
    /// `release-warmup` appears in the network data directory, or shutdown is requested.
    #[cfg(feature = "test-hooks")]
    fn hold_warmup(&self, args: &ArgsManager) {
        if std::env::var("RDASHD_TEST_HOOK").as_deref() != Ok("hold-warmup") {
            return;
        }
        let Ok(release) = args.get_data_dir_net().map(|d| d.join("release-warmup")) else {
            return;
        };
        while !release.exists() && !self.shutdown.shutdown_requested() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// `RDASHD_TEST_HOOK=f01-park` (CH3b D223): at the start of the ABC tail of the block
    /// `RDASHD_TEST_HOOK_BLOCK` names (display hex), V's hook makes the block's index record
    /// durable by V's own ALWAYS flush, writes what it read back from the database to `f01-ready`
    /// in the network data directory (synced, then renamed into place), and parks V's thread for
    /// good, so the test can kill the node between the block's accept and its connect. Armed
    /// before the RPC warmup ends, so before any `submitblock` is served. The hook sends V nothing
    /// and waits on nothing of V's (CH3 D39). A failed flush is logged and the hook returns
    /// without parking, so the tail stops V on the flush's report and the node exits; a failed
    /// `f01-ready` write is logged and returns the same way, and the block's ABC then runs.
    #[cfg(feature = "test-hooks")]
    fn arm_f01(&self, args: &ArgsManager) {
        if std::env::var("RDASHD_TEST_HOOK").as_deref() != Ok("f01-park") {
            return;
        }
        let block = std::env::var("RDASHD_TEST_HOOK_BLOCK").expect("RDASHD_TEST_HOOK_BLOCK");
        let dir = args.get_data_dir_net().expect("the network data directory");
        let logger = Arc::clone(self.ui.logger());
        let chain = self.steps.chain().expect("step 7 started V");
        chain.handle().set_before_block_tail(Some(Arc::new(
            move |stage: &mut BlockTailStage<'_>| {
                if stage.hash().to_string() != block {
                    return;
                }
                let staged = match stage.flush_and_read_back() {
                    Ok(Some(staged)) => staged,
                    Ok(None) => {
                        logger.log_printf("test-hooks: F01 failed: no database to read back\n");
                        return;
                    }
                    Err(e) => {
                        logger.log_printf(&format!("test-hooks: F01 flush failed: {e}\n"));
                        return;
                    }
                };
                if let Err(e) = write_f01_ready(&dir, &staged) {
                    logger.log_printf(&format!("test-hooks: F01 failed to write f01-ready: {e}\n"));
                    return;
                }
                logger.log_printf("test-hooks: F01 ready, parking V\n");
                loop {
                    std::thread::park();
                }
            },
        )));
    }

    /// `RDASHD_TEST_HOOK=v-panic-at-block` (BL-586): V panics at the start of the ABC tail of the
    /// block `RDASHD_TEST_HOOK_BLOCK` names (display hex), after its accept and before its
    /// `ActivateBestChain`, as one of Core's asserts would stop it there (`tests/hooks.rs`'s
    /// differential against dashd's SIGABRT). Armed before the RPC warmup ends, so before any
    /// `submitblock` is served.
    #[cfg(feature = "test-hooks")]
    fn arm_v_panic(&self) {
        if std::env::var("RDASHD_TEST_HOOK").as_deref() != Ok("v-panic-at-block") {
            return;
        }
        let block = std::env::var("RDASHD_TEST_HOOK_BLOCK").expect("RDASHD_TEST_HOOK_BLOCK");
        let chain = self.steps.chain().expect("step 7 started V");
        chain.handle().set_before_block_tail(Some(Arc::new(
            move |stage: &mut BlockTailStage<'_>| {
                if stage.hash().to_string() == block {
                    panic!("test-hooks: injected V panic");
                }
            },
        )));
    }

    /// CH5-5's live-copy seams (§6.2), only under `RDASHD_TEST_HOOK=live-copy`
    /// ([`crate::livecopy::enabled`]): `livecopycounters` reads the loaded chainstate's engine
    /// from now on, and the file requests in `RDASHD_TEST_HOOK_DIR` are served
    /// ([`crate::livecopy`]). Armed before the RPC warmup ends.
    #[cfg(feature = "test-hooks")]
    fn arm_live_copy(&self) {
        if !crate::livecopy::enabled() {
            return;
        }
        if let Some(engine) = self.steps.chainstate_engine() {
            crate::livecopy::attach(engine);
        }
        let dir = std::env::var_os("RDASHD_TEST_HOOK_DIR").expect("RDASHD_TEST_HOOK_DIR");
        crate::livecopy::Hooks {
            dir: dir.into(),
            chain: self.steps.chain().expect("step 7 started V").handle().clone(),
            sporks: Arc::clone(self.steps.sporks().expect("made at step 6")),
            logger: Arc::clone(self.ui.logger()),
            shutdown: Arc::clone(&self.shutdown),
        }
        .spawn();
    }

    /// Core's `Interrupt`: runs the `-shutdownnotify` commands, then wakes what `Shutdown` will
    /// stop. Runs whether init succeeded or not, and once: `shutdown` (and so `Drop`) calls it
    /// for a caller that did not.
    // Core: init.cpp:279-312
    pub fn interrupt(&self) {
        if self.interrupted.swap(true, Ordering::SeqCst) {
            return;
        }
        notify::shutdown_notify(self.ui.logger(), &self.args);
        if let Some(rpc) = &self.rpc {
            rpc.interrupt();
        }
        if let Some(host) = &self.spv {
            host.interrupt();
        }
        // Core: v24.0.2 src/init.cpp:290-292
        if let Some(signing) = &self.signing {
            signing.interrupt();
        }
        // Core: init.cpp:289-295
        if let Some(net) = &self.net {
            net.interrupt();
        }
        // Core: v24.0.0 src/init.cpp:296-310
        self.steps.interrupt_indexes();
    }

    /// Core's `Shutdown` (`PrepareShutdown`, then part 2): stops the services in Core's order,
    /// flushes a loaded chainstate twice, after an `AbortNode` too (DEC-241), removes the pid file
    /// and releases the lock. Runs once.
    // Core: init.cpp:314-526
    pub fn shutdown(&mut self) {
        if std::mem::replace(&mut self.shut_down, true) {
            return;
        }
        self.interrupt();
        let logger = self.ui.logger();
        logger.log_printf("PrepareShutdown: In progress...\n");
        set_thread_name("shutoff");
        // Core: v24.0.2 src/init.cpp:328
        if let Some(mempool) = self.steps.mempool() {
            IsMempool::add_transactions_updated(&**mempool.pool(), 1);
        }
        if let Some(mut rpc) = self.rpc.take() {
            rpc.stop();
        }
        self.rpc_chain.detach();
        self.rpc_net.detach();
        self.rpc_sync.detach();
        if self.spv.is_some() {
            self.shutdown_spv();
            return;
        }
        // Core: v24.0.2 src/init.cpp:335, src/active/context.cpp:65-73
        if let Some(signers) = &self.lock_signers {
            signers.unsubscribe();
        }
        if let Some(signing) = &mut self.signing {
            signing.stop_active();
        }
        self.lock_signers = None;
        // Core: v24.0.2 src/init.cpp:336
        if let Some(cl) = self.cl_scheduler.take() {
            cl.stop();
        }
        // Core: v24.0.0 src/init.cpp:337 StopHandlers: `NetInstantSend::Stop` joins `isman`.
        if let Some(net) = &mut self.net {
            net.stop_instantsend();
        }
        // Core: v24.0.2 src/init.cpp:337
        if let Some(signing) = &mut self.signing {
            signing.stop();
        }
        // Core: v24.0.0 src/init.cpp:337 StopHandlers: `NetDKG::Stop` joins the phase threads.
        #[cfg(feature = "dkg-runtime")]
        if let Some(mut dkg) = self.dkg.take() {
            dkg.stop();
        }
        // Core: init.cpp:346
        if let Some((signals, listener)) = self.cl_tip.take() {
            signals.unregister(&listener);
        }
        // Core: init.cpp:347-348
        if let Some(mut net) = self.net.take() {
            net.stop();
        }
        if let Some(scheduler) = &self.scheduler {
            scheduler.stop();
        }
        self.steps.shutdown();
        // Core: v24.0.0 src/init.cpp:462 node.llmq_ctx.reset()
        self.signing = None;
        self.quorums = None;
        self.scheduler = None;
        if let Some(pid) = self.pid.take() {
            pid.remove();
        }
        logger.log_printf("Shutdown: done\n");
        self.lock = None;
    }

    /// The rest of [`shutdown`](Self::shutdown) in SPV mode: the client and its store, then the
    /// scheduler, the pid file and the lock. The SPV store has one flush; a failed one is an
    /// `AbortNode` (DEC-241).
    fn shutdown_spv(&mut self) {
        if let Some(host) = self.spv.as_mut() {
            if let Err(e) = host.shutdown() {
                self.abort.abort_node(&format!("Failed to flush the SPV store: {e}"), None);
            }
            self.spv = None;
        }
        if let Some(scheduler) = self.scheduler.take() {
            scheduler.stop();
        }
        if let Some(pid) = self.pid.take() {
            pid.remove();
        }
        self.ui.logger().log_printf("Shutdown: done\n");
        self.lock = None;
    }

    /// Makes the chainstate's next WAL sync fail (the process tests' failed shutdown flush).
    #[cfg(feature = "test-hooks")]
    pub fn fail_next_wal_sync(&self) {
        self.steps.fail_next_wal_sync();
    }

    /// Has V's flush side panic before its job in the second shutdown flush if `second`, else in
    /// the first (the process tests' V panic in a shutdown flush, BL-674).
    #[cfg(feature = "test-hooks")]
    pub fn panic_v_in_shutdown_flush(&mut self, second: bool) {
        use dashd_chainstate::chain::ShutdownFlush;
        self.steps.panic_v_in_flush = Some(if second {
            ShutdownFlush::Second
        } else {
            ShutdownFlush::First
        });
    }

    /// Replaces `-dbcrashratio`'s exit, which then returns: W stops, and init fails with status 0.
    #[cfg(test)]
    pub(crate) fn set_crash_exit(&mut self, exit: CrashExit) {
        self.crash_exit = exit;
    }

    /// Core's `g_txindex` for the node's services: `None` with `-txindex=0`, or before step 7
    /// ends. [`indexes::get_transaction`] is Core's `GetTransaction` over it.
    pub fn txindex(&self) -> Option<&TxIndexHandle> {
        self.txindex.as_ref()
    }

    /// Step 7c on a masternode: `ActiveContext`'s ChainLock and InstantSend signers over the
    /// services built so far.
    // Core: v24.0.2 src/init.cpp:2165-2169, :2201
    fn build_lock_signers(
        &self,
        sync: &Arc<MasternodeSync>,
        signing: &Arc<init::signing::NodeSigningRuntime>,
    ) -> Result<crate::locks::LockSigners, InitFailure> {
        let chain = self.steps.chain().expect("step 7 started V");
        let is = self.steps.instantsend().expect("step 7's attach built the IS parts");
        let chainlocks = self.net.as_ref().and_then(p2p::Net::chainlocks);
        let chainlocks = chainlocks.expect("the masternode services built the ChainLock handler");
        crate::locks::LockSigners::new(crate::locks::LockSignerParts {
            params: &self.params.chain,
            view: chain.handle().view_handle(),
            loading: Arc::clone(chain.handle().loading()),
            signals: chain.signals().clone(),
            sporks: Arc::clone(self.steps.sporks().expect("step 7a made sporkman")),
            sync: Arc::clone(sync),
            is,
            signing: Arc::clone(signing) as _,
            chainlocks,
            mempool: self.steps.mempool().map(|pool| pool.handle()),
            txindex: self.txindex.clone(),
            files: self.steps.block_files().cloned(),
            clock: Arc::clone(&self.clock) as _,
            abort: Arc::clone(&self.abort) as _,
        })
        .map_err(|e| InitFailure::Exception(e.to_string()))
    }

    /// The signing runtime, from step 7c: listener registration and `set_sign_queue`.
    pub fn signing(&self) -> Option<&Arc<init::signing::NodeSigningRuntime>> {
        self.signing.as_ref().map(init::signing::NodeSigning::runtime)
    }

    /// Core's `GetBlockFilterIndex(BASIC)`: `None` without `-blockfilterindex`, or before step 7
    /// ends.
    pub fn filter_index(&self) -> Option<&FilterIndexHandle> {
        self.filters.as_ref()
    }

    /// Core's `g_coin_stats_index`: `None` without `-coinstatsindex`, or before step 7 ends.
    pub fn coinstats_index(&self) -> Option<&CoinStatsHandle> {
        self.coinstats.as_ref()
    }

    /// V, from step 7 (P3's (A − B) passes).
    #[cfg(test)]
    pub(crate) fn chain(&self) -> Option<&crate::chain::Chain> {
        self.steps.chain()
    }

    /// The network, from step 6 (P3's (A − B) pass A; the masternode role tests).
    #[cfg(test)]
    pub(crate) fn net(&self) -> Option<&crate::p2p::Net> {
        self.net.as_ref()
    }

    /// The spork manager, from step 7 (LK1a F's node tests).
    #[cfg(test)]
    pub(crate) fn sporks(&self) -> Option<&Arc<dashd_masternode::sporks::SporkManager>> {
        self.steps.sporks()
    }

    /// Sets the shutdown steps' probe (LK1a F's node tests).
    #[cfg(test)]
    pub(crate) fn set_probe(&mut self, probe: Box<dyn FnMut(crate::steps::Point) + Send>) {
        self.steps.probe = Some(probe);
    }

    /// V starts holding commands until startup's ABC completes, as after a tied fallback (D38),
    /// and `before` runs as step 11 starts, before that ABC is sent, with V and `mn_sync`
    /// (DEC-1811's node tests).
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn hold_startup_abc(&mut self, before: BeforeHeldImport) {
        use dashd_rpc::dash::NodeSync as _;
        let slot = Arc::clone(&self.rpc_sync);
        self.steps.held_import = Some(Box::new(move |chain| {
            before(chain, &*slot.mn_sync().expect("constructed at step 7"));
        }));
    }

    /// From now on route B's LLMQ reads answer `ListUnavailable` (LK1a F's node tests; with
    /// test-support).
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn fail_route_b_lists(&self) {
        self.route_b_list_unavailable.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The node's `mn_sync` slot (LK1a F's node tests).
    #[cfg(test)]
    pub(crate) fn sync_slot(&self) -> Arc<rpc::SyncSlot> {
        Arc::clone(&self.rpc_sync)
    }

    /// `mn_sync`, from step 7 (LK1a F's node tests).
    #[cfg(test)]
    pub(crate) fn mn_sync(&self) -> Option<Arc<dyn dashd_rpc::dash::MnSync>> {
        use dashd_rpc::dash::NodeSync as _;
        self.rpc_sync.mn_sync()
    }

    /// A reader of V's block files of the test's own, from step 7 (P4b D3).
    #[cfg(test)]
    pub(crate) fn take_test_block_files(
        &mut self,
    ) -> Option<Box<dyn dashd_chainstate::chain::BlockFiles>> {
        self.steps.test_files.take()
    }

    /// The network commands' slot, from step 12 (BL-503's node test).
    #[cfg(test)]
    pub(crate) fn rpc_net(&self) -> Arc<rpc::NetSlot> {
        Arc::clone(&self.rpc_net)
    }

    /// The chain commands' slot, with V's coins and the pool from step 7 (RP3a-1's node test).
    #[cfg(test)]
    pub(crate) fn rpc_chain(&self) -> Arc<rpc::ChainSlot> {
        Arc::clone(&self.rpc_chain)
    }

    /// The processor's shared half, from step 7c (BL-503's node test interrupts it alone).
    #[cfg(test)]
    pub(crate) fn net_shared(&self) -> Option<Arc<dashd_p2p::processing::peerman::Shared>> {
        self.net.as_ref().and_then(p2p::Net::shared).cloned()
    }

    /// Stops the network as `Shutdown` does, leaving V running (P3's (A − B) pass B).
    #[cfg(test)]
    pub(crate) fn stop_net(&mut self) {
        if let Some(net) = &mut self.net {
            net.stop();
        }
    }

    /// The process's exit status.
    pub fn exit_status(&self) -> i32 {
        self.shutdown.exit_status()
    }

    /// The chainstate's tip, once loaded.
    pub fn tip(&self) -> Option<BlockHash> {
        self.steps.coins_tip()
    }

    /// Step 6's `mn_metaman`, for tests that read what the node recorded in it.
    #[cfg(feature = "test-support")]
    pub fn metaman(&self) -> Option<&Arc<dashd_masternode::meta::MasternodeMetaMan>> {
        self.steps.metaman()
    }

    /// The node's mempool once step 7 built it ([`ChainSteps::mempool`]); never call
    /// `prioritise` on it from V. Core v24.0.0 `src/node/context.h:83`, `NodeContext::mempool`.
    pub fn mempool(&self) -> Option<&Arc<crate::mempool::NodeMempool>> {
        self.steps.mempool()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Core's `_Exit`: no destructors, no `atexit` handlers, no stdio flush.
fn exit_now(status: i32) -> ! {
    #[cfg(unix)]
    // SAFETY: `_exit` ends the process at once; nothing runs after it.
    unsafe {
        libc::_exit(status)
    }
    #[cfg(not(unix))]
    std::process::exit(status)
}

/// Core's `ChainlockHandler::Start` on the handler's own `cl-schdlr` scheduler, every 5 s:
/// `CheckActiveState` and `EnforceBestChainLock` as V's `ClEnforce { Timer }`, so V sees a
/// SPORK_19 flip and re-enforces its best lock without a new tip or lock; then `Cleanup`, which
/// with the blockchain synced (and at most every 30 s) drops seen-CLSIG hashes first seen 24 h
/// ago, so a peer's announcement of one is requested again. Core's `Cleanup` also trims its
/// first-seen times of transactions, which only a masternode keeps, and
/// [`LockSigners`](crate::locks::LockSigners) trims. A V that has stopped ignores the task.
// Core: chainlock/handler.cpp:63-72, :334-357
fn start_chainlock_handler(
    cl: &Scheduler,
    chain: &Chain,
    cleanup: Option<SeenChainLocksCleanup>,
    sync: Arc<MasternodeSync>,
) {
    let v = chain.handle().clone();
    cl.schedule_every(
        move || {
            let _ = v.chainlock_timer();
            if let Some(cleanup) = &cleanup {
                cleanup.run(sync.is_blockchain_synced());
            }
        },
        Duration::from_secs(5),
    );
}

/// Core's `ChainlockHandler::UpdatedBlockTip`: outside IBD, a tip that brings new blocks posts
/// the handler's task to `cl-schdlr` at once, unless one is posted and has not run yet
/// (`tryLockChainTipScheduled`). V runs that task's `CheckActiveState` and
/// `EnforceBestChainLock` itself, as its `ClEnforce { TipScheduled }`; this listener posts the
/// rest, `Cleanup`, which shares the 5 s task's 30 s throttle.
// Core: chainlock/handler.cpp:185-206
struct ClTipCleanup {
    cl: Scheduler,
    cleanup: SeenChainLocksCleanup,
    sync: Arc<MasternodeSync>,
    /// `tryLockChainTipScheduled`, for the `Cleanup` this listener posts.
    scheduled: Arc<AtomicBool>,
}

impl ClTipCleanup {
    fn new(cl: Scheduler, cleanup: SeenChainLocksCleanup, sync: Arc<MasternodeSync>) -> Self {
        ClTipCleanup {
            cl,
            cleanup,
            sync,
            scheduled: Arc::default(),
        }
    }
}

impl ValidationInterface for ClTipCleanup {
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        // V's guard for its half: not for blocks disconnected only, nor in IBD.
        if !updated_block_tip_schedules(new, fork, ibd) {
            return;
        }
        if self.scheduled.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok()
        {
            let (cleanup, sync) = (self.cleanup.clone(), Arc::clone(&self.sync));
            let scheduled = Arc::clone(&self.scheduled);
            self.cl.schedule_from_now(
                move || {
                    cleanup.run(sync.is_blockchain_synced());
                    scheduled.store(false, Ordering::SeqCst);
                },
                Duration::ZERO,
            );
        }
    }
}

/// Starts step 4a's `scheduler`, with its entropy task and the disk space check on `disk_dir`.
fn start_scheduler(
    scheduler: &Scheduler,
    ui: &Arc<Ui>,
    shutdown: &Arc<ShutdownState>,
    disk_dir: PathBuf,
) -> Result<(), InitFailure> {
    scheduler.start().map_err(|e| InitFailure::Exception(e.to_string()))?;
    // Gather some entropy once per minute.
    // Core: init.cpp:1578-1581
    scheduler.schedule_every(dashd_random::rand_add_periodic, Duration::from_secs(60));
    schedule_disk_space_check(scheduler, ui, shutdown, disk_dir);
    Ok(())
}

/// Every five minutes: with less than 50 MB to spare beyond Core's minimum, a plain shutdown
/// (`StartShutdown`, not `AbortNode`: the exit status stays 0). Where Core's `fs::space` would
/// throw (and end the scheduler thread), an unreadable free space counts as plenty.
// Core: init.cpp:1584-1590, util/fs_helpers.cpp CheckDiskSpace
fn schedule_disk_space_check(
    scheduler: &Scheduler,
    ui: &Arc<Ui>,
    shutdown: &Arc<ShutdownState>,
    blocks_dir: PathBuf,
) {
    const MIN_FREE_FOR_CHECK: u64 = 50 << 20;
    let (logger, shutdown) = (Arc::clone(ui.logger()), Arc::clone(shutdown));
    scheduler.schedule_every(
        move || {
            let available = OsFs::new().available_space(&blocks_dir).unwrap_or(u64::MAX);
            if available < MIN_DISK_SPACE + MIN_FREE_FOR_CHECK {
                logger.log_printf("Shutting down due to lack of disk space!\n");
                shutdown.start_shutdown();
            }
        },
        Duration::from_secs(5 * 60),
    );
}

/// Core's `Cache configuration` lines and the `-maxmempool` check between them, and the coins'
/// setup step 7 hands `LoadChainstate` with `tuning` and step 4a's `caches` and `queue`: the cache
/// sizes, the mempool's maximum in bytes that the coins may use while it is empty (Core's headroom
/// with no transaction in the pool, `validation.cpp:2952-2953`), `-assumevalid`,
/// `-consensusxcheck` (O9b; rdashd's own, so no Core line), step 6's `mn_metaman`, which
/// `CDeterministicMNManager` holds for the chainstate's platform-ban resets, and `-dbcrashratio`
/// with the exit its roll runs. The node and `NodeSut` both build their setup here.
// Core: init.cpp:1983-2023, node/chainstatemanager_args.cpp:33
#[expect(
    clippy::too_many_arguments,
    reason = "Core's chainstate inputs from steps 4a and 6, which the node and NodeSut both pass"
)]
pub(crate) fn coins_configuration<T: EngineTuning>(
    args: &ArgsManager,
    chain: &ChainSetup,
    interaction: &ParamInteraction,
    logger: &Logger,
    tuning: Arc<T>,
    live_copy: bool,
    caches: Arc<ScriptCaches>,
    queue: Arc<CheckQueue>,
    metaman: Arc<MasternodeMetaMan>,
    crash_exit: CrashExit,
) -> Result<(CacheSizes, CoinsSetup<T>), InitFailure> {
    let (indexes, sizes) = cache_sizes(args, &interaction.net)?;

    let mut mempool = MemPoolOptions {
        check_ratio: i32::from(ChainPolicy::for_setup(chain, false).default_consistency_checks),
        ..MemPoolOptions::default()
    };
    apply_args_man_options(args, chain, logger, &mut mempool)?.map_err(InitFailure::Error)?;
    // Core: init.cpp:2007 at v24.0.0
    mempool.check_ratio = mempool.check_ratio.clamp(0, 1_000_000);
    let max_mempool = mempool.max_size_bytes;
    let max_mempool_bytes = u64::try_from(max_mempool).unwrap_or(0);
    let mut lines = sizes.log_lines(&indexes, max_mempool_bytes);
    let last = lines.pop();
    for line in lines {
        logger.log_printf(&format!("{line}\n"));
    }
    let descendant_limit_bytes = mempool.limits.descendant_size_vbytes.wrapping_mul(40);
    if max_mempool < 0 || max_mempool < descendant_limit_bytes {
        return Err(InitFailure::Error(format!(
            "-maxmempool must be at least {} MB",
            (descendant_limit_bytes as f64 / 1_000_000.0).ceil()
        )));
    }
    if let Some(line) = last {
        logger.log_printf(&format!("{line}\n"));
    }
    let xcheck = args.get_bool_arg("-consensusxcheck", false)?;
    if xcheck {
        logger.log_printf("Consensus cross-check (-consensusxcheck) is on\n");
    }
    let setup = CoinsSetup {
        tuning,
        max_mempool: max_mempool_bytes,
        mempool,
        caches,
        queue,
        assume_valid: interaction.flags.chainman.assumed_valid(chain),
        check_block_index: interaction.flags.chainman.check_block_index(chain),
        xcheck,
        digest_log: None,
        #[cfg(feature = "planted-bugs")]
        planted: None,
        #[cfg(feature = "test-support")]
        regime: Default::default(),
        #[cfg(feature = "test-support")]
        tally: None,
        #[cfg(feature = "test-support")]
        w_schedule: None,
        #[cfg(feature = "test-support")]
        coins_salt: None,
        #[cfg(feature = "test-support")]
        records: None,
        #[cfg(feature = "test-support")]
        release_fault_on_is_removal: false,
        cjdns_reachable: crate::p2p::cjdns_reachable(args)?,
        governance: None,
        metaman,
        crash_ratio: crash_ratio(args)?,
        crash_exit,
        #[cfg(test)]
        digest: Default::default(),
        #[cfg(test)]
        h11_pool_error_at: None,
        #[cfg(test)]
        route_b_list_unavailable: None,
        live_copy,
        quorum_hook: None,
        qman: None,
    };
    Ok((sizes, setup))
}

/// The chainstate's storage profile for `chain` ([`chainstate_profile`]) as `-storageprofile`
/// requests it. Said once at start-up when the option is set, and in test-hooks builds on
/// regtest always (CH5-5 §6.2), so a test can tell which profile its node runs.
fn storage_profile(
    chain: &ChainSetup,
    requested: Option<StorageProfile>,
    logger: &Logger,
) -> (RocksTuning, bool) {
    let profile = chainstate_profile(chain.network, requested);
    match requested {
        Some(StorageProfile::Production) => logger.log_printf(
            "Chainstate storage profile: production (-storageprofile=production); a copy of the \
             running data directory is not promised to reopen\n",
        ),
        Some(StorageProfile::LiveCopy) => {
            logger.log_printf("Chainstate storage profile: livecopy (-storageprofile=livecopy)\n");
        }
        None => {}
    }
    #[cfg(feature = "test-hooks")]
    if chain.network == dash_params::Network::Regtest {
        logger.log_printf(match profile.1 {
            true => "test-hooks: live-copy storage profile on regtest\n",
            false => "test-hooks: production storage profile on regtest\n",
        });
    }
    profile
}

/// The reindex's `after_scan` hooks, between the scan and the clear:
/// - `RDASHD_TEST_HOOK=abort-after-scan`: the chainstate engine's next durable write fails,
///   which is the reindex's clear. V raises its `AbortNode` and stops there before the clear is
///   written.
/// - `RDASHD_TEST_HOOK=shutdown-after-scan`: `StartShutdown`, then a wait (at most a minute) for
///   init to see it and reach `Shutdown()`'s stop of V, which waits for the import first, then
///   200 ms more, which only lets a stop sent before the join land first.
#[cfg(feature = "test-hooks")]
fn arm_after_scan(steps: &mut ChainSteps, shutdown: &Arc<ShutdownState>) {
    let mut on_stop: Option<Box<dyn FnOnce() + Send>> = None;
    let after_scan: Box<dyn FnOnce() + Send> = match std::env::var("RDASHD_TEST_HOOK").as_deref() {
        Ok("abort-after-scan") => {
            let faults = steps.durable_write_fault();
            Box::new(move || faults.fail_next_durable_write())
        }
        Ok("shutdown-after-scan") => {
            let shutdown = Arc::clone(shutdown);
            let (stopping, stopped) = std::sync::mpsc::channel();
            on_stop = Some(Box::new(move || {
                let _ = stopping.send(());
            }));
            Box::new(move || {
                shutdown.start_shutdown();
                let _ = stopped.recv_timeout(std::time::Duration::from_secs(60));
                std::thread::sleep(std::time::Duration::from_millis(200));
            })
        }
        _ => return,
    };
    steps.set_import_hooks(crate::chain::ImportHooks {
        on_reindex_finished: None,
        on_import_done: None,
        after_scan: Some(after_scan),
        on_stop,
        load_mempool: None,
    });
}

/// The test-hooks builds' own refusals in rdashd's refusal slot (BL-634 §2.6, §6.3): the retired
/// `RDASHD_TEST_HOOK_PROFILE` seam, which a leftover script would otherwise leave on the live
/// profile unnoticed; and the live-copy hooks (`RDASHD_TEST_HOOK=live-copy`) on the production
/// profile, whose copies carry no promise for their tripwires to report on.
#[cfg(feature = "test-hooks")]
fn hook_profile_refusal(requested: Option<StorageProfile>) -> Option<String> {
    if std::env::var_os("RDASHD_TEST_HOOK_PROFILE").is_some() {
        return Some(
            "RDASHD_TEST_HOOK_PROFILE was replaced by -storageprofile (BL-634)".to_owned(),
        );
    }
    (requested == Some(StorageProfile::Production) && crate::livecopy::enabled()).then(|| {
        "test-hooks: the live-copy hooks need the livecopy storage profile \
         (-storageprofile=production is set)"
            .to_owned()
    })
}

/// [`Node::arm_f01`]'s `f01-ready`: one `key=value` line a field, hashes in display hex, values
/// in hex, `none` for an absent one; `head_blocks` is `new,old`. Written to `f01-ready.tmp`,
/// synced, renamed into place and the directory synced, so a reader never sees a torn file.
#[cfg(feature = "test-hooks")]
fn write_f01_ready(dir: &Path, staged: &StagedRecord) -> std::io::Result<()> {
    use std::fmt::Display;
    use std::io::Write as _;

    fn or_none<T: Display>(v: Option<T>) -> String {
        v.map_or_else(|| "none".to_owned(), |v| v.to_string())
    }
    fn hex(bytes: Option<&[u8]>) -> String {
        or_none(bytes.map(|b| b.iter().map(|b| format!("{b:02x}")).collect::<String>()))
    }
    let head_blocks = staged.head_blocks.map(|(new, old)| format!("{new},{}", or_none(old)));
    let text = format!(
        "tip={}\ncoins_tip={}\nhead_blocks={}\nbefore={}\nreleased_epoch={}\n\
         snapshot_epoch={}\nrecord={}\n",
        or_none(staged.tip),
        or_none(staged.coins_tip),
        or_none(head_blocks),
        hex(staged.before.as_deref()),
        or_none(staged.released_epoch),
        staged.snapshot_epoch,
        hex(staged.record.as_deref()),
    );
    let tmp = dir.join("f01-ready.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, dir.join("f01-ready"))?;
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use dashd_rpc::blockchain::NodeChain as _;

    use dashd_chainstate::connect::{
        BlockAt, CORRUPT_BLOCK, DISK_SPACE_LOW, EVODB_INCONSISTENCY, FLUSH_UNDO_FAILED,
        g1_capability, refusal_text,
    };
    use dashd_chainstate::flush::Kept;
    use dashd_chainstate::store::sites::{
        BARRIER_SITE, BLOCK_FILES_WRITE_SITE, CRASH_SITE, FLUSH_SITE, RAW_WRITE_SITE, READ_SITE,
        UNWIND_SITE, V_PANIC_SITE, W_PANIC_SITE, WRITE_SITE,
    };

    use super::*;
    use crate::abort::DEFAULT_ABORT_USER_MESSAGE;
    use crate::init::{Lines, setup_server_args};
    use crate::logging::Logger;
    use crate::steps::Point;

    mod coinjoin;
    mod coinstats;
    mod filters;
    mod governance;
    mod instantsend;
    mod lifecycle;
    mod lock_signers;
    mod mining;
    mod txindex;

    /// `ParseArgs` and `InitConfig` as the binary runs them, on `dir`.
    fn config(
        dir: &Path,
        argv: &[&str],
    ) -> (Arc<ArgsManager>, Result<SelectedParams, InitFailure>) {
        config_on(dir, "-regtest", argv)
    }

    /// [`config`] on the chain `chain_flag` selects.
    fn config_on(
        dir: &Path,
        chain_flag: &str,
        argv: &[&str],
    ) -> (Arc<ArgsManager>, Result<SelectedParams, InitFailure>) {
        let args = ArgsManager::new();
        setup_server_args(&args);
        let datadir = format!("-datadir={}", dir.display());
        let mut full = vec!["rdashd", datadir.as_str(), chain_flag, "-printtoconsole=0"];
        full.extend(init::quiet_node_args(argv));
        full.extend_from_slice(argv);
        args.parse_parameters(&full).unwrap();
        let params = init::init_config(&args);
        (Arc::new(args), params)
    }

    /// A node on `dir` whose debug log lines and stderr text are collected.
    fn start(dir: &Path, argv: &[&str]) -> (Node, Lines, Lines) {
        start_on(dir, "-regtest", argv)
    }

    /// [`start`] on the chain `chain_flag` selects.
    fn start_on(dir: &Path, chain_flag: &str, argv: &[&str]) -> (Node, Lines, Lines) {
        let (args, params) = config_on(dir, chain_flag, argv);
        let logger = Arc::new(Logger::new());
        let (log, stderr) = (Lines::default(), Lines::default());
        let sink = Arc::clone(&log);
        logger.push_back_callback(move |s| sink.lock().unwrap().push(s.to_owned()));
        let sink = Arc::clone(&stderr);
        let ui =
            Ui::with_stderr(logger, Box::new(move |s| sink.lock().unwrap().push(s.to_owned())));
        (Node::new(args, Arc::new(ui), params.unwrap()), log, stderr)
    }

    fn has(lines: &Mutex<Vec<String>>, needle: &str) -> bool {
        lines.lock().unwrap().iter().any(|l| l.contains(needle))
    }

    fn position(lines: &Mutex<Vec<String>>, needle: &str) -> usize {
        let lines = lines.lock().unwrap();
        lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no {needle:?}"))
    }

    /// Every path under `dir` with its length (directories: `None`).
    fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<u64>> {
        let mut out = BTreeMap::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let e = e.unwrap();
                let meta = e.metadata().unwrap();
                out.insert(e.path(), meta.is_file().then_some(meta.len()));
                if meta.is_dir() {
                    todo.push(e.path());
                }
            }
        }
        out
    }

    /// Each `init_config` binds `IPv4OptionalHeight` to the chain it selects
    /// (deploymentstatus.h:14-18): two regtest configurations moved to different heights, then a
    /// mainnet one, all in one process, and each chain still answers by its own height.
    #[test]
    fn each_init_keeps_its_own_ipv4_optional_height() {
        use dashd_chainstate::connect::ipv4_optional_active_at;
        let chain = |flag: &str, argv: &[&str]| {
            let dir = tempfile::tempdir().unwrap();
            config_on(dir.path(), flag, argv).1.expect("selected").chain
        };
        let a = chain("-regtest", &["-testactivationheight=ipv4_optional@500"]);
        let b = chain("-regtest", &["-testactivationheight=ipv4_optional@501"]);
        let main = chain("-chain=main", &[]);
        // Each chain at its own height minus one, at it, and plus one.
        for height in 499..=502 {
            assert_eq!(ipv4_optional_active_at(&a, height), height >= 500, "a at {height}");
            assert_eq!(ipv4_optional_active_at(&b, height), height >= 501, "b at {height}");
            assert!(!ipv4_optional_active_at(&main, height), "main at {height}");
        }
    }

    #[test]
    fn starts_and_stops_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        assert!(node.app_init_main());
        let pid = dir.path().join("regtest").join("dashd.pid");
        assert!(pid.exists());
        let chainstate = dir.path().join("regtest").join(crate::chainstate::CHAINSTATE_DIR);
        assert!(chainstate.join(dashd_store::STORE_MARKER).is_file(), "BL-509");
        assert_eq!(node.tip(), None);
        assert!(
            position(&log, "Cache configuration:") < position(&log, "for chain state database")
        );
        assert!(position(&log, "init message: Loading block index…") < position(&log, "ms\n"));
        assert!(has(&log, "init message: Done loading"));
        assert!(node.rpc_chain.published().is_some(), "the commands read the loaded chain");

        node.interrupt();
        node.shutdown();
        // Core flushes twice on a clean shutdown (ND1-D9).
        assert_eq!(node.steps.flushes(), 2);
        assert!(node.rpc_chain.published().is_none(), "V's handle is not kept past the lock");
        assert!(!pid.exists());
        assert!(
            position(&log, "PrepareShutdown: In progress...") < position(&log, "Shutdown: done")
        );
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        assert_eq!(node.exit_status(), 0);

        // The datadir and its chainstate open again.
        let (mut again, _, stderr) = start(dir.path(), &[]);
        assert!(again.app_init_early() && again.app_init_main());
        again.shutdown();
        assert!(stderr.lock().unwrap().is_empty());
    }

    /// RP1a D68 (BL-701): the scheme Core's flag holds after `VerifyLoadedChainstate`, by the
    /// coins' best block and V19's height: legacy on an empty view whatever V19's height, even 0,
    /// where the height rule at genesis is basic; otherwise the rule after the tip, so a genesis
    /// best block is basic with V19 at 0 or 1 and legacy with V19 at 10, and a tip at 8 is
    /// legacy and at 9 basic with V19 at 10.
    // Core: bls/bls.cpp:18, node/chainstate.cpp:413-433
    #[test]
    fn the_startup_scheme_is_legacy_on_an_empty_view_else_the_height_rule() {
        use dash_consensus::BlsCtx;
        use dash_params::{ChainOption, ChainParamsBuilder, Network, Release};

        use crate::chainstate::startup_bls;

        let (legacy, basic) = (BlsCtx::LEGACY, BlsCtx::BASIC);
        for (v19, genesis, eight, nine) in
            [(0, basic, basic, basic), (1, basic, basic, basic), (10, legacy, legacy, basic)]
        {
            let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
                .arg(ChainOption::TestActivationHeight, format!("v19@{v19}"))
                .build()
                .unwrap();
            assert_eq!(startup_bls(&p, None), legacy, "empty, V19 at {v19}");
            assert_eq!(startup_bls(&p, Some(0)), genesis, "genesis, V19 at {v19}");
            assert_eq!(startup_bls(&p, Some(8)), eight, "tip 8, V19 at {v19}");
            assert_eq!(startup_bls(&p, Some(9)), nine, "tip 9, V19 at {v19}");
        }
    }

    /// RP1a D68 (BL-701): init's `VerifyLoaded` and the evo hooks' initial scheme read one
    /// source, the coins' best block at this start (`Chainstate::coins_empty`), not V's tip. A
    /// fresh datadir is empty: `coins_empty`, and legacy published though genesis has connected
    /// since, whose height rule is basic. The restart on it is not: its coins are at genesis,
    /// and the scheme is basic. RPC reads the published scheme.
    // Core: node/chainstate.cpp:415-417, :429-431, validation.cpp:2545-2550
    #[test]
    fn the_startup_scheme_and_verify_read_the_same_coins_best_block() {
        use dash_consensus::BlsCtx;

        let dir = tempfile::tempdir().unwrap();
        let mut genesis = None;
        for (run, empty, scheme) in [(0, true, BlsCtx::LEGACY), (1, false, BlsCtx::BASIC)] {
            let (mut node, _, stderr) = start(dir.path(), &[]);
            assert!(node.app_init_early() && node.app_init_main(), "start {run}");
            assert!(node.steps.join_import(), "loadblk ends");
            let chainstate = node.steps.chainstate();
            let input = (!empty).then_some(0);
            assert_eq!(chainstate.evo_scheme_input(), Some(input), "start {run}");
            assert_eq!(chainstate.coins_empty(), empty, "start {run}");
            assert_eq!(chainstate.tip().is_none(), empty, "start {run}");
            assert_eq!(node.steps.verified_coins_empty, Some(empty), "start {run}");
            let view = node.chain().unwrap().handle().view();
            assert!(view.snap.has_tip() && view.snap.tip.tip_height == 0, "start {run}");
            if run == 1 {
                assert_eq!(chainstate.tip(), genesis, "the coins are at genesis");
            }
            genesis = Some(view.snap.tip.tip_hash);
            assert_eq!(view.snap.bls, BlsCtx::BASIC, "the height rule at genesis");
            assert_eq!(view.bls, scheme, "start {run}");
            let rpc = node.rpc_chain.published().expect("loaded");
            assert_eq!(rpc.bls(), scheme, "start {run}");
            node.shutdown();
            assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        }
    }

    /// BL-714: `-reindex-chainstate` on a datadir whose coins are at genesis reaches the evo
    /// hooks and `VerifyLoaded` with an empty coins view, as Core's `is_coinsview_empty` is
    /// under `fReindexChainState`: the scheme stays legacy though genesis connects again during
    /// the replay. The restart after it is at genesis again, and basic. With V19 at 0 the height
    /// rule at genesis is basic, so only the empty view gives legacy there.
    // Core: node/chainstate.cpp:415-417, :429-431
    #[test]
    fn reindex_chainstate_starts_the_scheme_from_an_empty_coins_view() {
        use dash_consensus::BlsCtx;

        for v19 in ["-testactivationheight=v19@1", "-testactivationheight=v19@0"] {
            let dir = tempfile::tempdir().unwrap();
            for (run, reindex, empty, scheme) in [
                (0, false, true, BlsCtx::LEGACY),
                (1, false, false, BlsCtx::BASIC),
                (2, true, true, BlsCtx::LEGACY),
                (3, false, false, BlsCtx::BASIC),
            ] {
                let mut argv = vec!["-txindex=0", v19];
                if reindex {
                    argv.push("-reindex-chainstate");
                }
                let (mut node, _, stderr) = start(dir.path(), &argv);
                let at = format!("{v19}, start {run}");
                assert!(node.app_init_early() && node.app_init_main(), "{at}");
                assert!(node.steps.join_import(), "loadblk ends");
                let chainstate = node.steps.chainstate();
                assert_eq!(chainstate.evo_scheme_input(), Some((!empty).then_some(0)), "{at}");
                assert_eq!(chainstate.coins_empty(), empty, "{at}");
                assert_eq!(node.steps.verified_coins_empty, Some(empty), "{at}");
                let view = node.chain().unwrap().handle().view();
                assert!(view.snap.has_tip() && view.snap.tip.tip_height == 0, "{at}");
                assert_eq!(view.bls, scheme, "{at}");
                assert_eq!(node.rpc_chain.published().expect("loaded").bls(), scheme, "{at}");
                node.shutdown();
                assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
            }
        }
    }

    /// `-consensusxcheck` (O9b) reaches step 7's coins setup: off unless set, with a log line when
    /// on. That the setup's flag turns the checks on is `NodeSut`'s positive control
    /// (`sut.rs`, `planted-bugs`), which builds its setup in the same function.
    #[test]
    fn consensusxcheck_is_read_at_step_7() {
        let cases: [(&[&str], bool); 3] =
            [(&[], false), (&["-consensusxcheck"], true), (&["-noconsensusxcheck"], false)];
        for (argv, on) in cases {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr) = start(dir.path(), argv);
            assert!(node.app_init_early() && node.app_init_main(), "{argv:?}");
            assert_eq!(has(&log, "Consensus cross-check (-consensusxcheck) is on"), on, "{argv:?}");
            node.shutdown();
            assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_locked_data_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("regtest");
        std::fs::create_dir_all(&net).unwrap();
        let mut holder = crate::datadir::tests::spawn_lock_holder(&net);

        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(!node.app_init_early());
        let expected = format!(
            "Error: Cannot obtain a lock on data directory {}. rdashd is probably already running.\n",
            net.display()
        );
        // Before StartLogging, so stderr is the only record (as Core's).
        assert_eq!(*stderr.lock().unwrap(), [expected]);
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        node.shutdown();
        assert!(!net.join("dashd.pid").exists());

        drop(holder.stdin.take());
        assert!(holder.wait().unwrap().success());
        let (mut node, _, _) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
    }

    /// BL-509: the chainstate a power loss cut off while rdashd made it (ST3b run
    /// 1793266802-2137011, point 27: these files, byte for byte, with no `OPTIONS-*`) carries
    /// rdashd's mark, so the next start opens it and loads from nothing, as nothing was flushed.
    #[test]
    fn a_chainstate_cut_off_while_rdashd_made_it_loads() {
        const MANIFEST: &[u8] = b"\x04\x06\xd9\x00\x2d\x00\x01\x81\x40\x24\
                                  f0694d59-368a-42df-8999-5d1d1d8818c3\x02\x00\x03\x02\x04\x00";
        let dir = tempfile::tempdir().unwrap();
        let chainstate = dir.path().join("regtest").join(crate::chainstate::CHAINSTATE_DIR);
        crate::datadir::tests::mark(&chainstate);
        for (name, bytes) in [
            ("CURRENT", &b"MANIFEST-000001\n"[..]),
            ("MANIFEST-000001", MANIFEST),
            ("IDENTITY", b"f0694d59-368a-42df-8999-5d1d1d8818c3"),
            ("LOCK", b""),
            ("LOG", b""),
        ] {
            std::fs::write(chainstate.join(name), bytes).unwrap();
        }
        let (mut node, log, _) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        assert!(node.app_init_main());
        assert!(has(&log, "init message: Done loading"));
        assert_eq!(node.tip(), None);
        assert!(
            chainstate.read_dir().unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("OPTIONS-"))
        );
        node.interrupt();
        node.shutdown();
        assert!(chainstate.join(dashd_store::STORE_MARKER).is_file(), "RocksDB kept the mark");
    }

    /// A Dash Core data directory, for any network, is refused with nothing written to it.
    #[test]
    fn a_core_data_directory_is_left_untouched() {
        for net in ["regtest", "testnet3", ""] {
            let dir = tempfile::tempdir().unwrap();
            let core = if net.is_empty() {
                dir.path().to_path_buf()
            } else {
                dir.path().join(net)
            };
            let chainstate = core.join("chainstate");
            std::fs::create_dir_all(&chainstate).unwrap();
            std::fs::write(chainstate.join("CURRENT"), "MANIFEST-000002\n").unwrap();
            std::fs::write(chainstate.join("MANIFEST-000002"), [0u8; 16]).unwrap();
            let before = tree(dir.path());

            let (_, params) = config(dir.path(), &[]);
            let Err(InitFailure::Error(msg)) = params else {
                panic!("{net:?} not refused");
            };
            let expected = format!("{} holds a Dash Core data directory", core.display());
            assert!(msg.starts_with(&expected), "{msg}");
            assert_eq!(tree(dir.path()), before, "{net:?}");
        }
    }

    /// `app_init_early` as if `-spv` and `-spvwallet` were served, on `chain_flag` with `conf`
    /// as `dash.conf`: whether it succeeded, and stderr.
    fn spv_early(chain_flag: &str, argv: &[&str], conf: Option<&str>) -> (bool, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        if let Some(conf) = conf {
            std::fs::write(dir.path().join("dash.conf"), conf).unwrap();
        }
        let (mut node, log, stderr) = start_on(dir.path(), chain_flag, argv);
        node.spv_served_as = true;
        let ok = node.app_init_early();
        assert!(!has(&log, "is not supported by rdashd yet"));
        let stderr = stderr.lock().unwrap().clone();
        (ok, stderr)
    }

    /// The SPV refusals come before every Core check: `-peertimeout=0` would fail step 3 first.
    #[test]
    fn spv_refusals_come_before_core_s_checks() {
        let txindex =
            "Error: -txindex cannot be used with -spv: an SPV node keeps no blocks or undo data.\n";
        let mn = "Error: -masternodeblsprivkey cannot be used with -spv: a masternode must run a full node.\n";
        let cases: [(&str, &[&str], &str); 6] = [
            ("-regtest", &["-spv", "-txindex=1", "-peertimeout=0"], txindex),
            ("-regtest", &["-spv", "-txindex=1"], txindex),
            (
                "-regtest",
                &["-spv", "-prune=1"],
                "Error: -prune cannot be used with -spv: an SPV node keeps no blocks or chainstate.\n",
            ),
            ("-testnet", &["-spv", "-masternodeblsprivkey=00"], mn),
            ("-testnet", &["-spv", "-nomasternodeblsprivkey"], mn),
            ("-regtest", &["-spvwallet=f", "-peertimeout=0"], "Error: -spvwallet requires -spv.\n"),
        ];
        for (chain, argv, line) in cases {
            assert_eq!(spv_early(chain, argv, None), (false, vec![line.to_owned()]), "{argv:?}");
        }
    }

    /// A plain `-spv` start passes the SPV refusals and the rest of `app_init_early`; with
    /// `txindex=1` in the config file it is refused.
    #[test]
    fn a_plain_spv_start_passes_early_init() {
        let (ok, stderr) = spv_early("-regtest", &["-spv"], None);
        assert!(ok && stderr.is_empty(), "{stderr:?}");
        let txindex =
            "Error: -txindex cannot be used with -spv: an SPV node keeps no blocks or undo data.\n";
        let refused = spv_early("-regtest", &["-spv"], Some("txindex=1\n"));
        assert_eq!(refused, (false, vec![txindex.to_owned()]));
    }

    /// P8 S1: `-i2psam` sets the I2P proxy at step 12, with no random credentials, and I2P
    /// stays reachable (`-listen=0` soft-set `-i2pacceptincoming=0`). With
    /// `-i2pacceptincoming=1` init refuses it for P8c-2 in the refusal slot, after Core's checks,
    /// step 12's `-i2psam` check among them.
    // Core: init.cpp:2742-2758
    #[test]
    fn i2psam_sets_the_i2p_proxy() {
        use dash_p2p::proxy::{Proxy, ProxyAddr};
        use dash_p2p::{NetAddr, Network, Service};
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &["-i2psam=127.0.0.1:19052"]);
        assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
        let core = node.net.as_ref().expect("step 12 ran").core();
        let sam = Service::new(NetAddr::ipv4([127, 0, 0, 1]), 19052);
        let expected = Proxy {
            addr: ProxyAddr::Service(sam),
            randomize_credentials: false,
        };
        assert_eq!(core.proxy(Network::I2p), Some(expected));
        assert!(core.is_reachable(Network::I2p));
        node.shutdown();

        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) =
            start(dir.path(), &["-i2psam=127.0.0.1:19052", "-i2pacceptincoming=1"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(
            *stderr.lock().unwrap(),
            ["Error: -i2pacceptincoming is not supported by rdashd yet (P8c-2).\n"]
        );
        assert!(!has(&log, "Loading block index"));

        for (arg, shown) in [("-i2psam=0.0.0.0", "0.0.0.0"), ("-noi2psam", "0")] {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr) = start(dir.path(), &[arg, "-i2pacceptincoming=1"]);
            assert!(node.app_init_early());
            assert!(!node.app_init_main());
            assert_eq!(
                *stderr.lock().unwrap(),
                [format!("Error: Invalid -i2psam address or hostname: '{shown}'\n")]
            );
            assert!(!has(&log, "Loading block index"));
        }
    }

    #[test]
    fn unsupported_option_is_refused_after_core_s_checks() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &["-forcecompactdb"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        let refusal = position(&log, "-forcecompactdb is not supported by rdashd yet (CH2b).");
        assert!(position(&log, "* Using") < refusal);
        assert_eq!(stderr.lock().unwrap().len(), 1);
        assert!(!has(&log, "Loading block index"));
        drop(node);
        assert!(!dir.path().join("regtest").join("dashd.pid").exists());

        // Core's own argument errors come first, even in step 7.
        let (mut node, _, stderr) = start(dir.path(), &["-forcecompactdb", "-maxmempool=1"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(*stderr.lock().unwrap(), ["Error: -maxmempool must be at least 5 MB\n"]);
    }

    /// Step 6 reads the masternode role (BL-703a) and step 7c parses the key: an invalid one
    /// fails init with Core's `InitError` text, after the network core became an active
    /// masternode's.
    // Core: init.cpp:2156-2159 at v24.0.0
    #[test]
    fn step_7c_refuses_an_invalid_key_with_cores_init_error() {
        for key in
            ["00".to_owned(), "zz".repeat(32), "01".repeat(31), "00".repeat(32), "ff".repeat(32)]
        {
            let dir = tempfile::tempdir().unwrap();
            let arg = format!("-masternodeblsprivkey={key}");
            let (mut node, log, stderr) = start(dir.path(), &[&arg]);
            assert!(node.app_init_early(), "{:?}", stderr.lock().unwrap());
            assert!(!node.app_init_main());
            assert_eq!(
                *stderr.lock().unwrap(),
                ["Error: Invalid masternodeblsprivkey. Please see documentation.\n"],
                "{key}"
            );
            assert!(!has(&log, "MASTERNODE:"));
            let net = node.net.as_ref().expect("step 6 ran");
            assert!(net.role().active());
            assert!(net.core().config().active_masternode);
            assert!(net.shared().is_none(), "nothing attached at step 7c");
        }
    }

    /// Starts a regtest node in `dir` whose `settings.json` holds `-masternodeblsprivkey` as
    /// `value` (`None`: no settings file), runs init and returns its stderr. The key's settings are
    /// set for step 3, so `argv` carries the `-txindex=1` it asks for.
    fn failed_init_with_key_setting(value: Option<&str>, argv: &[&str]) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        if let Some(value) = value {
            let net_dir = dir.path().join("regtest");
            std::fs::create_dir_all(&net_dir).unwrap();
            let json = format!("{{\"masternodeblsprivkey\": {value}}}");
            std::fs::write(net_dir.join("settings.json"), json).unwrap();
        }
        let (mut node, _, stderr) = start(dir.path(), argv);
        assert!(node.app_init_early(), "{value:?}: {:?}", stderr.lock().unwrap());
        assert!(!node.app_init_main(), "{value:?}");
        stderr.lock().unwrap().clone()
    }

    /// A `settings.json` key that is not a string (an array or an object) does not move Core's
    /// error order: step 6's port check still reports first. Core reads the key with `GetArg`
    /// only at 7c (init.cpp:2156), and `-port=65536` stops init at step 6 with or without it.
    // Core: init.cpp:1752-1759, :2156
    #[test]
    fn a_non_string_key_setting_leaves_step_6s_port_error_first() {
        let argv = ["-txindex=1", "-port=65536"];
        let port = ["Error: Invalid port specified in -port: '65536'\n"];
        assert_eq!(failed_init_with_key_setting(None, &argv), port);
        for value in ["[]", "{}"] {
            assert_eq!(failed_init_with_key_setting(Some(value), &argv), port, "{value}");
        }
    }

    /// With no earlier error, a key setting ends init at step 7c: a string that is no key with
    /// Core's `InitError`, an array or an object with `GetArg`'s exception.
    // Core: init.cpp:2156-2159; util/system.cpp:529-535 at v24.0.0
    #[test]
    fn a_key_setting_ends_init_at_step_7c_with_cores_error() {
        for (value, failure) in [
            (
                "\"00\"",
                InitFailure::Error(
                    "Invalid masternodeblsprivkey. Please see documentation.".into(),
                ),
            ),
            (
                "[]",
                InitFailure::Exception(
                    "JSON value of type array is not of expected type string".into(),
                ),
            ),
            (
                "{}",
                InitFailure::Exception(
                    "JSON value of type object is not of expected type string".into(),
                ),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let net_dir = dir.path().join("regtest");
            std::fs::create_dir_all(&net_dir).unwrap();
            let json = format!("{{\"masternodeblsprivkey\": {value}}}");
            std::fs::write(net_dir.join("settings.json"), json).unwrap();
            let (mut node, _, stderr) = start(dir.path(), &[]);
            assert!(node.app_init_early(), "{value}: {:?}", stderr.lock().unwrap());
            assert_eq!(node.try_app_init_main(), Err(failure), "{value}");
            assert!(node.net.as_ref().expect("step 6 ran").role().active(), "{value}");
            node.interrupt();
            node.shutdown();
        }
    }

    /// Step 7c's registry: `NetQuorum` owns `qgetdata` and `qdata` for every role, and `NetDKG`
    /// owns the four DKG pushes and `qwatch` for an observer and, with `dkg-runtime`, for an
    /// active masternode (the stub keeps them otherwise).
    // Core: v24.0.0 src/init.cpp:2201-2224
    fn assert_quorum_owners(net: &p2p::Net, observer: bool) {
        use dash_p2p::CommandString as C;
        use dashd_p2p::processing::registry::CoreHandler;
        let masternode = net.role().active() && cfg!(feature = "dkg-runtime");
        let dkg = (observer || masternode).then_some(CoreHandler::NetDkg);
        let want = [
            (C::QGETDATA, Some(CoreHandler::NetQuorum)),
            (C::QDATA, Some(CoreHandler::NetQuorum)),
            (C::QCONTRIB, dkg),
            (C::QCOMPLAINT, dkg),
            (C::QJUSTIFY, dkg),
            (C::QPCOMMIT, dkg),
            (C::QWATCH, dkg),
        ];
        assert_eq!(net.quorum_owners(), want, "observer {observer}, masternode {masternode}");
    }

    /// m4(a): a node with no quorum role and a `-watchquorums` observer both register
    /// `NetQuorum`; the observer also registers its `NetDKG` (the masternode case is in
    /// [`a_regtest_key_attaches_the_active_masternode_and_runs_its_init`]).
    #[test]
    fn step_7c_registers_net_quorum_for_every_role() {
        for (argv, observer) in [(&[][..], false), (&["-watchquorums=1"][..], true)] {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, _, stderr) = start(dir.path(), argv);
            assert!(node.app_init_early(), "{argv:?}: {:?}", stderr.lock().unwrap());
            assert!(node.app_init_main(), "{argv:?}: {:?}", stderr.lock().unwrap());
            let net = node.net.as_ref().expect("step 6 ran");
            assert_eq!(net.role().observer(), observer, "{argv:?}");
            let watching =
                dashd_p2p::processing::seams::QuorumWatch::is_watching(&**net.quorum_nodes());
            assert_eq!(watching, observer, "{argv:?}: BL-946");
            assert_quorum_owners(net, observer);
            node.interrupt();
            node.shutdown();
        }
    }

    /// A regtest key makes the node an active masternode at step 7c: its listener is on V's
    /// signals and step 11's `loadblk` runs its startup `Init`. The log carries the key's public
    /// halves, legacy and basic serializations, and the masked command-line value, never the
    /// key's hex.
    // Core: init.cpp:2156-2167, :2538-2547; active/masternode.cpp:60-62 at v24.0.0
    #[test]
    fn a_regtest_key_attaches_the_active_masternode_and_runs_its_init() {
        use dash_pkc::bls::{BlsScChia, BlsScIetf, BlsSecretKey};
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let secret = [1u8; 32];
        let basic =
            hex(&BlsSecretKey::<BlsScIetf>::from_bytes(&secret).unwrap().public_key().to_bytes());
        let legacy =
            hex(&BlsSecretKey::<BlsScChia>::from_bytes(&secret).unwrap().public_key().to_bytes());
        assert_ne!(legacy, basic);
        let dir = tempfile::tempdir().unwrap();
        let key = format!("-masternodeblsprivkey={}", hex(&secret));
        let (mut node, log, stderr) = start(dir.path(), &[&key, "-dnsseed=0", "-fixedseeds=0"]);
        crate::logging::route_args_log(&node.args, node.ui.logger());
        assert!(node.app_init_early(), "{:?}", stderr.lock().unwrap());
        let (started, traced) = crate::test_log::logged(|| node.app_init_main());
        assert!(started, "{:?}", stderr.lock().unwrap());
        let traced: Vec<String> = traced.into_iter().map(|(_, line)| line).collect();
        let pubkeys = format!(
            "MASTERNODE:\n  blsPubKeyOperator legacy: {legacy}\n  blsPubKeyOperator basic: {basic}\n"
        );
        assert_eq!(traced.iter().filter(|l| **l == pubkeys).count(), 1, "{traced:?}");
        assert!(has(&log, "Command-line arg: masternodeblsprivkey=****"), "{log:?}");
        let text =
            log.lock().unwrap().concat() + &traced.concat() + &stderr.lock().unwrap().concat();
        assert!(!text.to_ascii_lowercase().contains(&hex(&secret)), "the key's hex was logged");
        let net = node.net.as_ref().expect("step 6 ran");
        assert!(net.role().active());
        assert!(net.parts().active_registered);
        assert_quorum_owners(net, false);
        let deadline = Instant::now() + Duration::from_secs(60);
        while net.active_started() != Some(true) {
            assert!(Instant::now() < deadline, "the startup Init never ran");
            std::thread::sleep(Duration::from_millis(10));
        }
        // One ActiveMasternode per node: step 7c's load is the one the MNAUTH signer, the
        // MNAUTH handler, the am1-tip worker, the CoinJoin server, NetQuorum's operator key, its
        // tip listener's role, the signing manager's identity and the share manager's facts and
        // operator key hold.
        // Core: init.cpp:2156-2160, :2201-2224, :2228-2231; net_processing.cpp:5801 at v24.0.0
        // Core: v24.0.2 src/active/context.cpp:44
        // With DK1: the DKG's `m_mn_activeman`, its operator key, `quorum dkgstatus` and the
        // quorum manager's `m_handler` from ConnectManagers, whose `SetQuorumSecretKeyShare`
        // reads the proTxHash. `masternode status` reads it through the RPC chain slot.
        // Core: llmq/quorumsman.cpp:134, llmq/quorumsman.h:114-121 at v24.0.0
        // Core: v24.0.2 src/rpc/masternode.cpp:200-205
        assert_eq!(
            net.active_masternode_holders(),
            Some(if cfg!(feature = "dkg-runtime") {
                14
            } else {
                10
            }),
            "the MNAUTH signer, the MNAUTH handler, the am1-tip worker, the CoinJoin server, \
             NetQuorum's operator key, the quorum tip listener, the signing identity, the \
             share manager's facts and operator key, and the RPC chain slot"
        );
        // Below DIP3 Core's `InitInternal` returns at once: no proTx, outpoint or service.
        // Core: v24.0.2 src/active/masternode.cpp:112-116
        let status = crate::chain::tests::rpc(
            &command_table(&node, dir.path()),
            "masternode",
            r#"["status"]"#,
        );
        assert_eq!(
            status.unwrap(),
            format!(
                r#"{{"outpoint":"{}-4294967295","service":"[::]:0","state":"WAITING_FOR_PROTX","status":"Waiting for ProTx to appear on-chain","quorumParticipation":true}}"#,
                "0".repeat(64)
            )
        );
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        node.interrupt();
        node.shutdown();
    }

    /// DK1-L3 at step 7c: an active masternode gets the DKG runtime, whose phase threads start at
    /// step 10a, and the dkgdb; with `-watchquorums` alone the observer's sessions, the dkgdb and
    /// no thread; a plain node neither. Shutdown stops the DKG before V.
    // Core: v24.0.0 src/init.cpp:2155-2172, :2360, :2367-2370, :337
    #[cfg(feature = "dkg-runtime")]
    #[test]
    fn step_7c_builds_the_dkg_for_a_masternode_or_an_observer() {
        let key = format!("-masternodeblsprivkey={}", "01".repeat(32));
        let quiet = ["-dnsseed=0", "-fixedseeds=0"];
        for (role, extra) in
            [("masternode", Some(key.as_str())), ("observer", None), ("plain", None)]
        {
            let dir = tempfile::tempdir().unwrap();
            let mut argv: Vec<&str> = quiet.to_vec();
            argv.extend(extra);
            if role == "observer" {
                argv.push("-watchquorums=1");
            }
            let (mut node, _, stderr) = start(dir.path(), &argv);
            assert!(node.app_init_early(), "{role}: {:?}", stderr.lock().unwrap());
            assert!(node.app_init_main(), "{role}: {:?}", stderr.lock().unwrap());
            let net_dir = dir.path().join("regtest");
            let dkgdb = net_dir.join(init::dkg::DKGDB_DIR);
            match role {
                "masternode" => {
                    use std::collections::BTreeSet;

                    use dash_types::Numeric as _;

                    let dkg = node.dkg.as_ref().expect("a masternode's DKG");
                    assert!(dkg.has_runtime() && dkg.sessions().is_active());
                    assert!(!dkg.sessions().handlers().is_empty());
                    assert!(dkg.duties().is_some());
                    let expected: BTreeSet<String> = dkg
                        .sessions()
                        .handlers()
                        .iter()
                        .map(|h| {
                            format!(
                                "d-llmq-{}-{}",
                                h.key().llmq_type.to_base(),
                                h.key().quorum_index
                            )
                        })
                        .collect();
                    let wait_start = Instant::now();
                    let deadline = wait_start + Duration::from_secs(30);
                    let elapsed = loop {
                        let seen: BTreeSet<String> = std::fs::read_dir("/proc/self/task")
                            .unwrap()
                            .filter_map(|t| {
                                let comm =
                                    std::fs::read_to_string(t.ok()?.path().join("comm")).ok()?;
                                let name = comm.trim_end_matches('\n');
                                name.starts_with("d-llmq-").then(|| name.to_owned())
                            })
                            .collect();
                        if expected.is_subset(&seen) {
                            break wait_start.elapsed();
                        }
                        if Instant::now() >= deadline {
                            let missing: Vec<&str> =
                                expected.difference(&seen).map(String::as_str).collect();
                            panic!(
                                "phase-thread names timed out after {:?}: missing {missing:?}, \
                                 seen {seen:?}, stderr {:?}",
                                wait_start.elapsed(),
                                stderr.lock().unwrap()
                            );
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    };
                    eprintln!("step_7c phase-thread names after {elapsed:?}");
                    assert!(dkgdb.join("CURRENT").is_file());
                }
                "observer" => {
                    let dkg = node.dkg.as_ref().expect("an observer's DKG");
                    assert!(!dkg.has_runtime() && !dkg.sessions().is_active());
                    assert!(dkg.sessions().handlers().is_empty(), "no phase thread to run");
                    assert!(!dkg.sessions().keys().is_empty(), "the observer map");
                    assert!(dkg.duties().is_none());
                    assert!(dkgdb.join("CURRENT").is_file());
                }
                _ => {
                    assert!(node.dkg.is_none());
                    assert!(!dkgdb.exists());
                }
            }
            assert_eq!(dkgdb, net_dir.join("llmq").join("dkgdb"), "Core's dkgdb path");
            assert!(!net_dir.join("dkgdb").exists());
            node.interrupt();
            node.shutdown();
            assert!(node.dkg.is_none());
            assert!(stderr.lock().unwrap().is_empty(), "{role}: {:?}", stderr.lock().unwrap());
        }
    }

    /// Unserved `-spv` gives only a refusal: no soft-set, governance warning or `-spv` index
    /// refusal comes first (BL-679). With an unserved index option the refusal is that option's,
    /// in help order; a served `-txindex=1` or `-coinstatsindex` leaves `-spv`'s.
    #[test]
    fn unserved_spv_gives_only_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        for (argv, refusal) in [
            (&["-spv"][..], "-spv is not supported by rdashd yet (SB18)."),
            (&["-spv", "-txindex=1"], "-spv is not supported by rdashd yet (SB18)."),
            (&["-spv", "-coinstatsindex"], "-spv is not supported by rdashd yet (SB18)."),
            (&["-spv", "-spentindex"], "-spentindex is not supported by rdashd yet (IX2)."),
        ] {
            let (mut node, log, stderr) = start(dir.path(), argv);
            assert!(node.app_init_early(), "{argv:?}");
            assert!(!node.app_init_main(), "{argv:?}");
            assert_eq!(*stderr.lock().unwrap(), [format!("Error: {refusal}\n")], "{argv:?}");
            assert!(!has(&log, "-spv set"), "{argv:?}");
        }
    }

    /// `-storageprofile` (BL-634) is refused off regtest in rdashd's own slot: after Core's
    /// argument checks, before the chainstate is read or written.
    #[test]
    fn storageprofile_is_refused_off_regtest_after_core_s_checks() {
        for (chain_flag, net_dir, id) in
            [("-testnet", "testnet3", "test"), ("-devnet=x", "devnet-x", "devnet")]
        {
            for value in ["production", "livecopy"] {
                let dir = tempfile::tempdir().unwrap();
                let arg = format!("-storageprofile={value}");
                let (mut node, log, stderr) = start_on(dir.path(), chain_flag, &[arg.as_str()]);
                assert!(node.app_init_early());
                assert!(!node.app_init_main(), "{chain_flag} {value}");
                let expected = format!("Error: storageprofile is not supported on {id} chain.\n");
                assert_eq!(*stderr.lock().unwrap(), [expected], "{chain_flag} {value}");
                assert!(!has(&log, "Loading block index"));
                drop(node);
                assert!(
                    !dir.path().join(net_dir).join("chainstate").exists(),
                    "{chain_flag} {value}"
                );
            }
        }
        // Core's own argument errors come first.
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) =
            start_on(dir.path(), "-testnet", &["-storageprofile=production", "-maxmempool=1"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(*stderr.lock().unwrap(), ["Error: -maxmempool must be at least 5 MB\n"]);
    }

    /// On regtest a bad `-storageprofile` is refused in the same slot, with nothing loaded.
    #[test]
    fn storageprofile_bad_value_is_refused_on_regtest() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &["-storageprofile=prod"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(
            *stderr.lock().unwrap(),
            ["Error: Invalid -storageprofile value 'prod' (livecopy or production).\n"]
        );
        assert!(!has(&log, "Loading block index"));
    }

    /// `-storageprofile` says its profile once, and only when set (DEC-1070 (e)).
    #[test]
    fn storageprofile_logs_its_profile() {
        const PRODUCTION: &str = "Chainstate storage profile: production (-storageprofile=production); \
                                  a copy of the running data directory is not promised to reopen";
        const LIVECOPY: &str = "Chainstate storage profile: livecopy (-storageprofile=livecopy)";
        let count = |log: &Lines, needle: &str| {
            log.lock().unwrap().iter().filter(|l| l.contains(needle)).count()
        };
        for (argv, production, livecopy) in [
            (&[][..], 0, 0),
            (&["-storageprofile=production"][..], 1, 0),
            (&["-storageprofile=livecopy"][..], 0, 1),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, _) = start(dir.path(), argv);
            assert!(node.app_init_early() && node.app_init_main(), "{argv:?}");
            assert_eq!(count(&log, PRODUCTION), production, "{argv:?}");
            assert_eq!(count(&log, LIVECOPY), livecopy, "{argv:?}");
            assert_eq!(count(&log, "Chainstate storage profile"), production + livecopy);
            node.shutdown();
        }
    }

    /// SP0: `-sporkkey` must be the spork address's key; shutdown stores `sporks.dat`, and step
    /// 7a refuses a corrupt one and leaves it as it was.
    #[test]
    fn sporks_load_at_step_7a_and_store_at_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &["-sporkkey=nonsense"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(*stderr.lock().unwrap(), ["Error: Unable to sign spork message, wrong key?\n"]);
        drop(node);

        // Core's functional-test key is regtest's spork key.
        let key = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
        let (mut node, _, stderr) = start(dir.path(), &[key]);
        assert!(node.app_init_early() && node.app_init_main());
        node.shutdown();
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        let path = dir.path().join("regtest").join(dashd_masternode::sporks::file::FILE_NAME);
        let mut bytes = std::fs::read(&path).unwrap();

        bytes[20] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(
            *stderr.lock().unwrap(),
            [format!("Error: Failed to load sporks cache from {}\n", path.display())]
        );
        node.shutdown();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// Shutdown closes `llmq/isdb` as Core's `isman.reset()` does: with the node still alive, the
    /// database opens again (RocksDB's `LOCK` is released, its WAL synced on the close).
    // Core: init.cpp:463
    #[test]
    fn shutdown_closes_the_is_store() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        node.shutdown();
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        let reopened = crate::instantsend::rocks_opener(&dir.path().join("regtest"))(false);
        assert!(reopened.is_ok(), "{:?}", reopened.err());
        drop(node);
    }

    /// `Shutdown` drops the message processor (`node.peerman.reset()`) and closes `quorumdb` at
    /// `node.llmq_ctx.reset()`, with the node still alive, on a plain node, an observer and a
    /// masternode: the core's binding does not keep the processor, nor `NetQuorum`'s handle the
    /// database, which opens again.
    // Core: v24.0.0 src/init.cpp:365, :462
    #[test]
    fn shutdown_drops_the_processor_and_closes_the_quorum_db() {
        use dashd_quorums::{QuorumDbKey, QuorumKeyStore as _};

        let key = format!("-masternodeblsprivkey={}", "01".repeat(32));
        let quiet = ["-blocksonly=0", "-dnsseed=0", "-fixedseeds=0"];
        for (role, extra) in [
            ("plain", None),
            ("observer", Some("-watchquorums=1")),
            ("masternode", Some(key.as_str())),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let argv: Vec<&str> = quiet.iter().copied().chain(extra).collect();
            let (mut node, _, stderr) = start(dir.path(), &argv);
            assert!(
                node.app_init_early() && node.app_init_main(),
                "{role}: {:?}",
                stderr.lock().unwrap()
            );
            let shared = Arc::downgrade(node.net.as_ref().unwrap().shared().unwrap());
            let store = node.quorums.as_ref().unwrap().store().downgrade();
            let row = QuorumDbKey([7; 32]);
            assert!(store.read(&row).is_ok(), "{role}");
            node.shutdown();
            assert!(stderr.lock().unwrap().is_empty(), "{role}: {:?}", stderr.lock().unwrap());
            assert_eq!(shared.strong_count(), 0, "{role}: the processor outlived the shutdown");
            assert!(store.read(&row).is_err(), "{role}: the quorum database outlived the shutdown");
            let net_dir = dir.path().join("regtest");
            let reopened = init::quorums::QuorumDb::unopened().open(&net_dir, false);
            assert!(reopened.is_ok(), "{role}: {:?}", reopened.err());
            drop(node);
        }
    }

    /// SP1: shutdown stores `netfulfilled.dat` and `mncache.dat`. A node with a tip (shutdown's
    /// flush stored genesis, BL-271) loads them, and one without (a new data directory) clears
    /// them; either refuses a file Core cannot read with Core's init error, leaving it as it was.
    // Core: init.cpp:2273-2289, :418-419
    #[test]
    fn net_caches_store_at_shutdown_and_load_or_clear() {
        use dashd_masternode::{meta, netfulfilled};
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        // The platform-ban applier, RPC, the MNAUTH handler and step 12's dial policy share step
        // 6's loaded and stored manager (BL-425; SP1c-1 r1 m-1; Core: init.cpp:2757;
        // net_processing.cpp:5801 at v24.0.0), and so do the CoinJoin counters (CJ1-W5;
        // Core: init.cpp:2178-2179) and the quorum-connection check's probes (Core: v24.0.0
        // src/llmq/net_dkg.cpp:801-819, :885), and governance's effects worker (GV1b S5a;
        // Core: governance/governance.h v24.0.2:252).
        assert_eq!(
            Arc::strong_count(node.steps.metaman().unwrap()),
            8,
            "steps, the applier, RPC, the MNAUTH handler, the dial policy, the CoinJoin counters, \
             the quorum-connection check and governance's effects worker"
        );
        // One quorum registry for the node, as Core has one CConnman: the seam's quorum watch,
        // the MNAUTH handler's relay-member answer, step 12's dial policy, step 10a's
        // maintenance task and the quorum-connection check all hold Net's.
        // Core: net.cpp:4465-4506; evo/mnauth.cpp:165-173; init.cpp:2364, :2757;
        // llmq/net_quorum.cpp:814-829 at v24.0.0
        assert_eq!(
            Arc::strong_count(node.net.as_ref().unwrap().quorum_nodes()),
            6,
            "Net, the seam's quorum watch, the MNAUTH handler, the dial policy, the maintenance \
             task and the quorum-connection check"
        );
        node.shutdown();
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        let fresh = tempfile::tempdir().unwrap();
        std::fs::create_dir(fresh.path().join("regtest")).unwrap();
        for (file, what) in
            [(netfulfilled::FILE_NAME, "fulfilled requests"), (meta::FILE_NAME, "masternode")]
        {
            for (dir, verb, prep) in [(dir.path(), "load", "from"), (fresh.path(), "clear", "at")] {
                let path = dir.join("regtest").join(file);
                let good = std::fs::read(&path).ok();
                std::fs::write(&path, b"cut").unwrap();
                let (mut node, _, stderr) = start(dir, &[]);
                assert!(node.app_init_early());
                assert!(!node.app_init_main());
                assert_eq!(
                    *stderr.lock().unwrap(),
                    [format!("Error: Failed to {verb} {what} cache {prep} {}\n", path.display())]
                );
                node.shutdown();
                assert_eq!(std::fs::read(&path).unwrap(), b"cut");
                match good {
                    Some(good) => std::fs::write(&path, good).unwrap(),
                    None => std::fs::remove_file(&path).unwrap(),
                }
            }
        }
    }

    /// With no tip (a fresh datadir) the start-up clears populated caches: both managers start
    /// empty, and the files already hold the empty stores before any shutdown store. The requests
    /// are still live at the node's time, so a start-up that loaded one would keep it.
    // Core: init.cpp:2273-2288; netfulfilledman.cpp:21-29; masternode/meta.cpp:31-36
    #[test]
    fn a_start_without_a_tip_clears_populated_caches() {
        use dash_clock::MockClock;
        use dash_evo::types::ProTxHash;
        use dash_netaddr::netaddr::{NetAddr, Service};
        use dashd_masternode::flatdb;
        use dashd_masternode::meta::{self, MasternodeMetaMan, MasternodeMetaStore};
        use dashd_masternode::netfulfilled::{self, NetFulfilledRequestManager, NetFulfilledStore};
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("regtest");
        std::fs::create_dir(&net).unwrap();
        let ms = config(dir.path(), &[]).1.unwrap().chain.message_start;
        let (nf_path, meta_path) = (net.join(netfulfilled::FILE_NAME), net.join(meta::FILE_NAME));
        let fulfilled =
            NetFulfilledRequestManager::new(Arc::new(MockClock::at(1_700_000_000)), 300);
        fulfilled
            .add_fulfilled_request(&Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19899), "full-sync");
        assert!(fulfilled.store(&nf_path, ms));
        let metaman = MasternodeMetaMan::new();
        let protx = ProTxHash::from([7; 32]);
        metaman.allow_mixing(&protx);
        metaman.add_used_masternode(&protx);
        assert!(metaman.store(&meta_path, ms));
        let (mut node, _, stderr) = start(dir.path(), &["-mocktime=1700000010"]);
        assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
        assert_eq!(node.steps.netfulfilled().unwrap().snapshot(), NetFulfilledStore::default());
        assert_eq!(node.steps.metaman().unwrap().snapshot(), MasternodeMetaStore::default());
        let nf = flatdb::decode::<NetFulfilledStore>(&std::fs::read(&nf_path).unwrap(), ms);
        assert_eq!(nf.unwrap(), NetFulfilledStore::default(), "cleared at start-up");
        let mn = flatdb::decode::<MasternodeMetaStore>(&std::fs::read(&meta_path).unwrap(), ms);
        assert_eq!(mn.unwrap(), MasternodeMetaStore::default(), "cleared at start-up");
        node.shutdown();
    }

    /// `mn_sync` is constructed before the chainstate loads, so `mnsync status` reports the
    /// load's start as its asset start time, however long the load takes (Sol r1 Minor 3).
    // Core: init.cpp:2086
    #[test]
    fn mn_sync_is_constructed_before_the_chainstate_loads() {
        use dash_clock::Clock as _;
        use dashd_rpc::dash::NodeSync as _;
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, _) = start(dir.path(), &["-mocktime=1700000000"]);
        // Under BLOCKCHAIN's 30 s timeout, so no tick switches the asset meanwhile.
        node.during_load = Some(Box::new(|clock, _| clock.set_mocktime(1_700_000_020)));
        assert!(node.app_init_early() && node.app_init_main());
        assert_eq!(node.clock.node_now().secs(), 1_700_000_020, "the load moved the clock");
        let sync = node.rpc_sync.mn_sync().expect("constructed at step 7");
        assert_eq!((sync.asset_id(), sync.asset_start_time()), (1, 1_700_000_000));
        node.shutdown();
        assert!(node.rpc_sync.mn_sync().is_none(), "shutdown resets mn_sync");
    }

    /// `spork` reads the node's `sporkman` through the node's command table: every spork's value
    /// and status in Core's order, a signed update included, until shutdown resets it.
    // Core: rpc/node.cpp:156-204; init.cpp:417
    #[test]
    fn spork_reads_the_nodes_sporkman_until_shutdown() {
        use dashd_rpc::dash::NodeSporks as _;
        let dir = tempfile::tempdir().unwrap();
        let key = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
        let (mut node, _, _) = start(dir.path(), &[key]);
        assert!(node.app_init_early() && node.app_init_main());
        let table = rpc::command_table(
            &node.params.chain,
            node.ui.logger(),
            &node.shutdown,
            &node.clock,
            (true, node.scheduler.as_ref().unwrap()),
            &node.rpc_chain,
            &node.rpc_net,
            &node.rpc_sync,
            &node.warnings,
            Default::default(),
            dir.path().join("regtest"),
        );
        table.start();
        table.set_warmup_finished();
        let spork = |command: &str| {
            crate::chain::tests::rpc(&table, "spork", &format!(r#"["{command}"]"#)).unwrap()
        };
        let sporks = Arc::clone(node.steps.sporks().unwrap());
        sporks.update_spork(dash_evo::spork::SporkId::SPORK_19_CHAINLOCKS_ENABLED, 0).unwrap();
        assert_eq!(
            spork("show"),
            r#"{"SPORK_2_INSTANTSEND_ENABLED":4070908800,"SPORK_17_QUORUM_DKG_ENABLED":4070908800,"SPORK_19_CHAINLOCKS_ENABLED":0,"SPORK_21_QUORUM_ALL_CONNECTED":4070908800,"SPORK_23_QUORUM_POSE":4070908800}"#
        );
        assert_eq!(
            spork("active"),
            r#"{"SPORK_2_INSTANTSEND_ENABLED":false,"SPORK_17_QUORUM_DKG_ENABLED":false,"SPORK_19_CHAINLOCKS_ENABLED":true,"SPORK_21_QUORUM_ALL_CONNECTED":false,"SPORK_23_QUORUM_POSE":false}"#
        );
        node.shutdown();
        assert!(node.rpc_sync.sporkman().is_none(), "shutdown resets sporkman");
    }

    /// A lookup of an unmined `llmq_test` quorum through the node's Platform handle.
    fn platform_lookup(quorums: &init::quorums::NodeQuorums) -> Result<bool, dash_dml::StoreError> {
        let (t, hash) =
            (dash_params::LlmqType::LlmqTest, dash_primitives::QuorumHash::from([1; 32]));
        quorums.platform.quorum(t, &hash).map(|q| q.is_some())
    }

    /// The quorum manager exists when the chainstate opens, as Core's `LLMQContext` is built
    /// inside `LoadChainstate` before the block index loads; V is not bound yet, so a Platform
    /// lookup is an error then, not a panic.
    // Core: v24.0.0 src/node/chainstate.cpp:153-161, :183
    #[test]
    fn the_quorum_manager_is_made_before_the_chainstate_opens() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, _) = start(dir.path(), &[]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let at_open = Arc::clone(&seen);
        node.during_load = Some(Box::new(move |_, quorums| {
            at_open.lock().unwrap().push(quorums.map(|q| platform_lookup(q).is_err()));
        }));
        assert!(node.app_init_early() && node.app_init_main());
        assert_eq!(*seen.lock().unwrap(), [Some(true)], "made, and not yet bound, at the open");
        node.shutdown();
    }

    /// The manager's hook is on V's members facade when init sends `VerifyLoaded`, so VerifyDB's
    /// quorum builds would run it (Core's builds go through the one `CQuorumManager`,
    /// `llmq/quorumsman.cpp:544`, `:563`): a probe installed then finds the slot taken. The
    /// fresh chain mines no quorum, so this shows the slot, not a build.
    // Core: v24.0.0 src/node/chainstate.cpp:157-161 before src/init.cpp:2096
    #[cfg(feature = "test-support")]
    #[test]
    fn the_quorum_managers_hook_is_installed_before_verifydb() {
        use std::sync::atomic::{AtomicU8, Ordering::SeqCst};
        static PROBED: AtomicU8 = AtomicU8::new(0);
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, _) = start(dir.path(), &[]);
        node.steps.before_verify = Some(|chain| {
            let probe: dashd_chainstate::llmq::quorum::SharedQuorumHook =
                Arc::new(dashd_chainstate::llmq::quorum::NoQuorumHook);
            let installed = chain.handle().on_v(move |o, _| o.install_quorum_hook(probe));
            let installed = installed.recv().expect("V serves");
            chain.allow_unawaited_send();
            PROBED.store(
                if installed {
                    2
                } else {
                    1
                },
                SeqCst,
            );
        });
        assert!(node.app_init_early() && node.app_init_main());
        assert_eq!(PROBED.load(SeqCst), 1, "the facade had a hook when VerifyLoaded was sent");
        node.shutdown();
    }

    /// Once the chainstate has loaded, the node's Platform lookups read V (route B): an unmined
    /// quorum is Core's nullptr, `Ok(None)`. After shutdown stops V, a lookup is an error, and
    /// once the node drops only the kept handles hold the manager: V's facade let its hook go.
    // Core: v24.0.0 src/net_processing.cpp:3898, src/llmq/quorumsman.cpp:491-512
    #[test]
    fn platform_lookups_read_v_once_the_chainstate_loads() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, _) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        let quorums = node.quorums.take().expect("made at step 7");
        assert!(matches!(platform_lookup(&quorums), Ok(false)), "bound to V, nothing mined");
        node.shutdown();
        assert!(platform_lookup(&quorums).is_err(), "V has stopped");
        drop(node);
        assert_eq!(Arc::strong_count(&quorums.manager), 2, "the manager and its Platform handle");
    }

    /// Core starts `q-cache` in `CQuorumManager`'s constructor inside `LoadChainstate`, after the
    /// block tree database opens and after `LLMQContext`'s block processor logs its BLS line and
    /// starts the BLS check threads: not before them, and not at all when the open fails.
    // Core: v24.0.0 src/node/chainstate.cpp:151-161, src/llmq/context.cpp:19-22,
    // src/llmq/blockprocessor.cpp:89-90, src/llmq/quorumsman.cpp:45-46
    #[test]
    fn the_qcache_thread_starts_after_the_chainstate_opens() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let thread = Arc::new(Mutex::new(None));
        let (at_open, kept) = (Arc::clone(&seen), Arc::clone(&thread));
        node.during_load = Some(Box::new(move |_, quorums| {
            let quorums = quorums.expect("made before the open");
            at_open.lock().unwrap().push(("open", quorums.qcache().is_started()));
            *kept.lock().unwrap() = Some(Arc::clone(quorums.qcache()));
        }));
        let (at_bls, kept) = (Arc::clone(&seen), Arc::clone(&thread));
        node.ui.logger().push_back_callback(move |s| {
            if s.contains("BLS verification uses ")
                && let Some(thread) = kept.lock().unwrap().as_ref()
            {
                at_bls.lock().unwrap().push(("bls line", thread.is_started()));
            }
        });
        assert!(node.app_init_early() && node.app_init_main());
        assert_eq!(*seen.lock().unwrap(), [("open", false), ("bls line", false)]);
        assert!(node.quorums.as_ref().expect("made at step 7").qcache().is_started());
        node.shutdown();
        assert!(stderr.lock().unwrap().is_empty());

        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        std::fs::write(dir.path().join("regtest").join("chainstate"), "not a database").unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        let question = ": Error opening block database.\nPlease restart with -reindex or \
                        -reindex-chainstate to recover.\n";
        assert_eq!(*stderr.lock().unwrap(), [question], "the open is what failed");
        let quorums = node.quorums.as_ref().expect("made before the open");
        assert!(!quorums.qcache().is_started(), "a failed open starts no thread");
        node.shutdown();
    }

    /// A `q-cache` thread that fails to start is Core's exception inside `LoadChainstate`, which
    /// init reports as "Error opening block database": the question, or under `-reindex` init's
    /// error, never `InitFailure::Exception`'s line.
    // Core: v24.0.0 src/init.cpp:2022-2029, :2088, :2109-2126
    #[test]
    fn a_qcache_thread_start_failure_is_the_load_failure() {
        let question = ": Error opening block database.\nPlease restart with -reindex or \
                        -reindex-chainstate to recover.\n";
        for (argv, error) in
            [(&[][..], question), (&["-reindex"][..], "Error: Error opening block database\n")]
        {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr) = start(dir.path(), argv);
            node.during_load = Some(Box::new(|_, quorums| {
                quorums.expect("made before the open").qcache().fail_next_start();
            }));
            assert!(node.app_init_early());
            assert!(!node.app_init_main());
            assert_eq!(*stderr.lock().unwrap(), [error], "{argv:?}");
            assert!(has(&log, "injected q-cache spawn failure"), "{argv:?}");
            assert!(!has(&log, "raised an exception"), "{argv:?}");
            assert_eq!(node.exit_status(), EXIT_FAILURE);
            node.shutdown();
        }
    }

    const QUORUM_KEY: dashd_quorums::QuorumDbKey = dashd_quorums::QuorumDbKey([7; 32]);

    /// Core's reindex question for "Error opening block database", declined.
    const OPEN_QUESTION: &str = ": Error opening block database.\nPlease restart with -reindex or \
                                 -reindex-chainstate to recover.\n";

    /// The node's quorum row, as the vector's length and the share's last byte.
    fn quorum_row(node: &Node) -> Option<(usize, Option<u8>)> {
        use dashd_quorums::QuorumKeyStore as _;
        let store = node.quorums.as_ref().expect("made at step 7").store();
        init::quorums::tests::summary(store.read(&QUORUM_KEY).expect("the store is open"))
    }

    /// Writes the quorum row into the quorum database in `net`, with no node running.
    fn plant_quorum_row(net: &Path) {
        use dashd_quorums::QuorumKeyStore as _;
        let db = init::quorums::QuorumDb::unopened();
        db.open(net, false).unwrap();
        db.write(&QUORUM_KEY, &init::quorums::tests::contributions(3)).unwrap();
    }

    /// The quorum row of a node started on `dir` with `argv`, which loads, then shut down.
    fn started_row(dir: &Path, argv: &[&str]) -> Option<(usize, Option<u8>)> {
        let (mut node, _, stderr) = start(dir, argv);
        assert!(node.app_init_early() && node.app_init_main(), "{argv:?}: {stderr:?}");
        let row = quorum_row(&node);
        node.shutdown();
        row
    }

    /// The chainstate database in `net`, opened with no node running.
    fn chainstate_db(net: &Path) -> dashd_store::RocksEngine {
        use dashd_chainstate::store::CHAINSTATE_SCHEMA;
        use dashd_store::{RocksConfig, RocksEngine};
        let path = net.join("chainstate");
        RocksEngine::open(&path, &CHAINSTATE_SCHEMA, &RocksConfig::default()).unwrap().0
    }

    /// Every entry of the chainstate database in `net`.
    fn chainstate_rows(net: &Path) -> Vec<(u16, Vec<u8>, Vec<u8>)> {
        use dashd_store::KvRead as _;
        let db = chainstate_db(net);
        (dashd_chainstate::store::CHAINSTATE_SCHEMA.all_cfs())
            .flat_map(|cf| {
                let rows: Vec<_> = db.iter_prefix(cf, &[]).map(Result::unwrap).collect();
                rows.into_iter().map(move |(k, v)| (cf.index() as u16, k, v))
            })
            .collect()
    }

    /// Commits `put` to the chainstate database in `net`.
    fn chainstate_put(net: &Path, put: impl FnOnce(&mut dashd_store::RocksBatch)) {
        use dashd_store::{Commit, KvEngine as _};
        let db = chainstate_db(net);
        let mut batch = db.batch();
        put(&mut batch);
        db.write(batch, Commit::Durable).unwrap();
    }

    /// T12: the quorum database is on disk: a row written through the node's handle is there
    /// after a plain restart.
    // Core: v24.0.0 src/llmq/quorumsman.cpp:40, src/dbwrapper.cpp:143-158
    #[test]
    fn plain_restart_keeps_quorum_rows() {
        use dashd_quorums::QuorumKeyStore as _;
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, _) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        assert_eq!(quorum_row(&node), None);
        let store = node.quorums.as_ref().expect("made at step 7").store();
        store.write(&QUORUM_KEY, &init::quorums::tests::contributions(3)).unwrap();
        node.shutdown();
        drop(node);
        let net = dir.path().join("regtest");
        assert!(net.join(init::quorums::QUORUM_DB_DIR).join("CURRENT").is_file());
        assert_eq!(started_row(dir.path(), &[]), Some((3, Some(3))));
        assert_eq!(started_row(dir.path(), &[]), Some((3, Some(3))));
    }

    /// T13: `-reindex-chainstate` wipes the quorum database, Core's `wipe = reindex ||
    /// reindex_chainstate`; the next plain start finds it empty.
    // Core: v24.0.0 src/node/chainstate.cpp:149, :159-160
    #[test]
    fn reindex_chainstate_wipes_quorum_rows() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(started_row(dir.path(), &[]), None);
        plant_quorum_row(&dir.path().join("regtest"));
        assert_eq!(started_row(dir.path(), &["-reindex-chainstate"]), None);
        assert_eq!(started_row(dir.path(), &[]), None);
    }

    /// T14: `-reindex` wipes the quorum database. rdashd refuses `-reindex` over stored block data
    /// until CH5-3b-2, so the datadir holds a quorum database and no block files.
    // Core: v24.0.0 src/node/chainstate.cpp:149, :159-160
    #[test]
    fn reindex_wipes_quorum_rows() {
        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        let net = dir.path().join("regtest");
        plant_quorum_row(&net);
        assert!(!net.join("blocks").join("blk00000.dat").exists(), "no block data");
        assert_eq!(started_row(dir.path(), &["-reindex"]), None);
    }

    /// T15: a chainstate wipe an earlier `-reindex-chainstate` left half done is resumed on a
    /// plain start, which keeps the quorum rows: Core decides the quorum wipe from the arguments
    /// alone and resumes no wipe.
    // Core: v24.0.0 src/node/chainstate.cpp:149, :159-160
    #[test]
    fn a_resumed_chainstate_wipe_keeps_quorum_rows() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(started_row(dir.path(), &[]), None);
        let net = dir.path().join("regtest");
        plant_quorum_row(&net);
        chainstate_put(&net, |put| dashd_chainstate::store::Meta::put_wiping(put, true));
        let (mut node, log, _) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        assert!(has(&log, "Resuming the interrupted wipe of the chainstate"));
        assert_eq!(quorum_row(&node), Some((3, Some(3))));
        node.shutdown();
    }

    /// T16: a quorum database that does not open is Core's `dbwrapper_error` inside
    /// `LoadChainstate`: its cause is logged, then "Error opening block database" and the
    /// reindex question, declined; no `q-cache` thread starts. A wipe does not recover this one, a
    /// regular file (T20).
    // Core: v24.0.0 src/dbwrapper.cpp:155-156; src/init.cpp:2022-2029, :2088, :2109-2126
    #[test]
    fn a_quorum_store_open_failure_is_cores_load_failure() {
        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        let path = dir.path().join("regtest").join(init::quorums::QUORUM_DB_DIR);
        crate::datadir::tests::mark(&path);
        std::fs::write(path.join("CURRENT"), "").unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(*stderr.lock().unwrap(), [OPEN_QUESTION]);
        let cause = position(&log, "Error opening the quorum database");
        assert!(cause < position(&log, "Aborted block database rebuild. Exiting."));
        assert!(!has(&log, "raised an exception"));
        assert!(!node.quorums.as_ref().expect("made at step 7").qcache().is_started());
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        node.shutdown();
    }

    /// A start whose network directory still holds `<name>` or `<name>.wipe` of any LLMQ database
    /// where rdashd kept them before Core's `llmq/<name>` is refused at `InitConfig` with the
    /// database's `OldPath` text before any store opens, leaving the old directory untouched.
    #[test]
    fn an_llmq_database_at_its_old_path_fails_init_and_is_left() {
        for (db, name, argv) in [
            ("quorum database", "quorumdb", &[][..]),
            ("recovered signature database", "recsigdb", &[][..]),
            ("DKG database", "dkgdb", &["-watchquorums=1"][..]),
        ] {
            for old in [name.to_owned(), format!("{name}.wipe")] {
                let dir = tempfile::tempdir().unwrap();
                let (_, params) = config(dir.path(), argv);
                params.unwrap();
                let net = dir.path().join("regtest");
                let path = net.join(&old);
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(path.join("CURRENT"), "MANIFEST-000001\n").unwrap();
                let before = tree(dir.path());
                let (_, params) = config(dir.path(), argv);
                let Err(InitFailure::Error(msg)) = params else {
                    panic!("{old} not refused");
                };
                let text = format!(
                    "Found the {db} at its old path {}: move it to {} by hand and restart",
                    path.display(),
                    net.join("llmq").join(&old).display()
                );
                assert_eq!(msg, text, "{old}");
                assert_eq!(tree(dir.path()), before, "{old}");
                assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1, "{old}");
                assert_eq!(std::fs::read(path.join("CURRENT")).unwrap(), b"MANIFEST-000001\n");
                assert!(!net.join("llmq").exists(), "{old}");
            }
        }
    }

    /// A plain (non-masternode, no `-watchquorums`) start still refuses an old `<netdir>/dkgdb`
    /// (or `<netdir>/dkgdb.wipe`) up front and leaves it untouched.
    #[test]
    fn a_plain_node_refuses_an_old_dkgdb_path_and_leaves_it() {
        for old in ["dkgdb", "dkgdb.wipe"] {
            let dir = tempfile::tempdir().unwrap();
            let (_, params) = config(dir.path(), &[]);
            params.unwrap();
            let net = dir.path().join("regtest");
            let path = net.join(old);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("CURRENT"), "MANIFEST-000001\n").unwrap();
            let before = tree(dir.path());
            let (_, params) = config(dir.path(), &[]);
            let Err(InitFailure::Error(msg)) = params else {
                panic!("{old} not refused");
            };
            assert_eq!(
                msg,
                format!(
                    "Found the DKG database at its old path {}: move it to {} by hand and restart",
                    path.display(),
                    net.join("llmq").join(old).display()
                ),
                "{old}"
            );
            assert_eq!(tree(dir.path()), before, "{old}");
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1, "{old}");
            assert_eq!(std::fs::read(path.join("CURRENT")).unwrap(), b"MANIFEST-000001\n");
            assert!(!net.join("llmq").exists(), "{old}");
        }
    }

    /// Under `-reindex`, an old `<netdir>/recsigdb` is refused before any store opens, so neither
    /// `llmq/isdb`, `llmq/quorumdb`, `llmq/recsigdb` nor `chainstate` is touched or wiped, and the
    /// old directory is left as it was. Stops at `init_config`, relying on every start path
    /// (`main`, `NodeSut`, `amb` and the test harnesses) going through `init_config` before any
    /// store opens.
    #[test]
    fn reindex_with_an_old_recsigdb_leaves_stores_and_chainstate_untouched() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(started_row(dir.path(), &[]), None);
        let net = dir.path().join("regtest");
        plant_quorum_row(&net);
        let mut recsigs = init::recsigs::RecoveredSigsDb::unopened();
        recsigs.open(&net, false).unwrap();
        init::recsigs::tests::plant(&mut recsigs);
        drop(recsigs);

        let old = net.join("recsigdb");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("CURRENT"), "MANIFEST-000001\n").unwrap();

        let before_chainstate = chainstate_rows(&net);
        assert!(!before_chainstate.is_empty());
        let before_tree = tree(&net);

        let (_, params) = config(dir.path(), &["-reindex"]);
        let Err(InitFailure::Error(msg)) = params else {
            panic!("not refused");
        };
        assert_eq!(
            msg,
            format!(
                "Found the recovered signature database at its old path {}: move it to {} by hand \
                 and restart",
                old.display(),
                net.join(init::recsigs::RECSIG_DB_DIR).display()
            )
        );
        assert_eq!(tree(&net), before_tree, "no store or file was touched or wiped");
        assert_eq!(std::fs::read_dir(&old).unwrap().count(), 1);
        assert_eq!(std::fs::read(old.join("CURRENT")).unwrap(), b"MANIFEST-000001\n");
        assert_eq!(chainstate_rows(&net), before_chainstate);

        std::fs::remove_dir_all(&old).unwrap();
        assert_eq!(started_row(dir.path(), &[]), Some((3, Some(3))));
        let recsigs = init::recsigs::RecoveredSigsDb::unopened();
        recsigs.open(&net, false).unwrap();
        assert!(init::recsigs::tests::planted(&recsigs));
    }

    /// T17: a stored reindex resumed without `-reindex` keeps the quorum rows: Core's
    /// `options.reindex` is the argument, and the stored flag only sets `fReindex` later, in
    /// `LoadBlockIndexDB`. A fresh chainstate holding only the flag and no block data reaches
    /// that state through the node.
    // Core: v24.0.0 src/node/chainstate.cpp:149, :159-161; src/node/blockstorage.cpp:401-404
    #[test]
    fn a_resumed_stored_reindex_without_the_argument_keeps_quorum_rows() {
        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        let net = dir.path().join("regtest");
        plant_quorum_row(&net);
        chainstate_put(&net, |put| dashd_chainstate::store::Meta::put_reindexing(put, true));
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
        assert!(node.steps.reindex && !node.steps.reindex_arg, "the stored reindex resumed");
        assert_eq!(quorum_row(&node), Some((3, Some(3))));
        node.shutdown();
    }

    /// T19 (E2 condition 2): a wipe whose rename, sync or delete fails is a clean init error: the
    /// cause logged, "Error opening block database" with the reindex question, and the chainstate
    /// as it was, although `-reindex-chainstate` would wipe it: the failure comes before any
    /// chainstate write. A failed sync or delete leaves the moved database, which the next start
    /// finishes deleting, so it opens an empty store.
    // Core: v24.0.0 src/dbwrapper.cpp:143-147; src/init.cpp:2022-2029, :2088, :2109-2126
    #[test]
    fn a_failed_quorum_wipe_fails_init_before_any_chainstate_write() {
        use init::quorums::WipeStep;
        for step in [WipeStep::Rename, WipeStep::Sync, WipeStep::Delete] {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(started_row(dir.path(), &[]), None);
            let net = dir.path().join("regtest");
            plant_quorum_row(&net);
            let before = chainstate_rows(&net);
            assert!(!before.is_empty());
            let (mut node, log, stderr) = start(dir.path(), &["-reindex-chainstate"]);
            node.during_load = Some(Box::new(move |_, quorums| {
                quorums.expect("made before the open").store().fail_wipe_at(step);
            }));
            assert!(node.app_init_early());
            assert!(!node.app_init_main(), "{step:?}");
            assert_eq!(*stderr.lock().unwrap(), [OPEN_QUESTION], "{step:?}");
            let cause = position(&log, &format!("injected quorum database {step:?} failure"));
            assert!(cause < position(&log, "Aborted block database rebuild. Exiting."));
            assert_eq!(node.exit_status(), EXIT_FAILURE);
            node.shutdown();
            drop(node);
            assert_eq!(chainstate_rows(&net), before, "{step:?}");
            let moved = net.join(init::quorums::WIPING_DIR).exists();
            assert_eq!(moved, step != WipeStep::Rename, "{step:?}");
            let kept = if moved {
                None
            } else {
                Some((3, Some(3)))
            };
            assert_eq!(started_row(dir.path(), &[]), kept, "{step:?}");
            assert!(!net.join(init::quorums::WIPING_DIR).exists(), "{step:?}");
        }
    }

    /// T20: a regular file at `llmq/quorumdb` is not rdashd's, so a `-reindex-chainstate`
    /// start refuses the directory as Dash Core's before the wipe, leaving the file and the
    /// chainstate unchanged. The wipe's own refusal of a non-directory is
    /// `init::quorums::tests::wipe_destroys_even_an_unreadable_store`'s.
    #[test]
    fn a_non_directory_quorum_store_is_refused_and_left() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(started_row(dir.path(), &[]), None);
        let net = dir.path().join("regtest");
        let path = net.join(init::quorums::QUORUM_DB_DIR);
        std::fs::remove_dir_all(&path).unwrap();
        std::fs::write(&path, "not a database").unwrap();
        let before = chainstate_rows(&net);
        assert!(!before.is_empty());
        let (_, params) = config(dir.path(), &["-reindex-chainstate"]);
        let refused = format!("{:?}", params.expect_err("refused"));
        assert!(refused.contains("holds a Dash Core data directory"), "{refused}");
        assert_eq!(std::fs::read(&path).unwrap(), b"not a database");
        assert!(!net.join(init::quorums::WIPING_DIR).exists());
        assert_eq!(chainstate_rows(&net), before);
    }

    /// "recsigs" runs from step 10a until shutdown joins it; the runtime then drops with the
    /// LLMQ context.
    // Core: v24.0.2 src/init.cpp:2363, :290-292, :337, :462
    #[test]
    fn recsigs_runs_from_step_10a_until_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
        assert!(node.signing().is_some());
        assert!(node.signing.as_ref().expect("made at step 7c").is_started());
        node.shutdown();
        assert!(node.signing.is_none());
    }

    /// An active masternode builds `ActiveContext`'s share manager at step 7c and binds its seam
    /// into the processor's slot; step 10a registers it as a recovered-sig listener, and
    /// shutdown unregisters it and joins its threads. A node that is not one keeps NoSigShares.
    // Core: v24.0.2 src/init.cpp:2163-2170, :2202, :2363, :2370-2371, :335, :337
    #[test]
    fn only_an_active_masternode_runs_the_share_manager() {
        use dashd_p2p::processing::seams::SigSharesSeam as _;
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
        let signing = node.signing.as_ref().expect("made at step 7c");
        assert!(signing.shares().is_none());
        assert!(node.net.as_ref().and_then(p2p::Net::sig_shares).is_none());
        node.shutdown();

        let dir = tempfile::tempdir().unwrap();
        let key = format!("-masternodeblsprivkey={}", "01".repeat(32));
        let argv = [key.as_str(), "-dnsseed=0", "-fixedseeds=0"];
        let (mut node, _, stderr) = start(dir.path(), &argv);
        assert!(node.app_init_early(), "{stderr:?}");
        assert!(node.app_init_main(), "{stderr:?}");
        let signing = node.signing.as_ref().expect("made at step 7c");
        assert!(signing.shares().is_some());
        assert_eq!(signing.shares_started(), Some(true));
        assert_eq!(signing.active_listeners(), Some(3), "the lock signers, then shareman");
        let seam = node.net.as_ref().and_then(p2p::Net::sig_shares).expect("a masternode's slot");
        assert!(seam.enabled());
        node.shutdown();
        assert!(node.signing.is_none());
    }

    /// Step 7d adds NetSigning's halves over the runtime: NetInstantSend's truncation and the
    /// processor's recovered-sig seam, which answers from the runtime's chain.
    // Core: v24.0.2 src/init.cpp:2201-2202
    #[test]
    fn step_7d_adds_net_signing_to_instantsend_and_the_processor() {
        use dashd_p2p::processing::seams::SigningSeam as _;
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
        assert!(node.steps.instantsend().expect("loaded").recsigs.is_bound());
        let seam = node.net.as_ref().and_then(p2p::Net::signing).expect("attached");
        assert!(seam.enabled());
        assert!(seam.llmq_configured(dash_params::LlmqType::LlmqTest));
        assert!(!seam.already_have(&[7; 32]));
        node.shutdown();
    }

    /// The recovered-signature commands read step 7c's signing manager: a sig stored through it
    /// is there for its type, id and message, conflicts with another message for the id, and
    /// reads back as stored. Once the shutdown drops the manager, there is no LLMQ context.
    // Core: v24.0.0 src/llmq/signing.cpp:649-652, :670-690; src/rpc/server_util.cpp:113-119
    #[test]
    fn recsig_commands_read_the_signing_manager_until_shutdown() {
        use dash_evo::locks::RecoveredSig;
        use dashd_quorums::signing::RecoveredSigIntake as _;
        use dashd_rpc::evo::{EvoError, NodeRecSigs as _};
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        let slot = Arc::clone(&node.rpc_chain);
        assert_eq!(slot.llmq_context(), Err(EvoError::LlmqUnavailable), "before step 7c");
        assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
        let sig = RecoveredSig {
            llmq_type: dash_params::LlmqType::LlmqTest,
            quorum_hash: [1; 32].into(),
            id: [2; 32],
            msg_hash: [3; 32],
            sig: [4; 96],
        };
        let runtime = node.signing().expect("made at step 7c");
        assert!(runtime.process_recovered_sig(Arc::new(sig.clone()), false).unwrap());
        let t = 100;
        assert_eq!(slot.llmq_context(), Ok(()));
        assert_eq!(slot.has_recovered_sig(t, &[2; 32], &[3; 32]), Ok(true));
        assert_eq!(slot.has_recovered_sig(t, &[2; 32], &[5; 32]), Ok(false));
        assert_eq!(slot.is_conflicting(t, &[2; 32], &[5; 32]), Ok(true));
        assert_eq!(slot.is_conflicting(t, &[2; 32], &[3; 32]), Ok(false));
        assert_eq!(slot.is_conflicting(t, &[6; 32], &[5; 32]), Ok(false));
        assert_eq!(slot.get_recovered_sig(t, &[2; 32], &[3; 32]), Ok(Some(sig)));
        assert_eq!(slot.get_recovered_sig(t, &[2; 32], &[5; 32]), Ok(None));
        node.shutdown();
        assert_eq!(slot.has_recovered_sig(t, &[2; 32], &[3; 32]), Err(EvoError::LlmqUnavailable));
    }

    /// A read fault in the recovered-signature commands is their RPC_MISC_ERROR with the fault's
    /// text, as Core's handlers let the database's exception escape: no abort, and the node runs
    /// on.
    // Core: v24.0.0 src/rpc/server.cpp:540-541
    #[test]
    fn a_recsig_read_fault_is_the_commands_error_and_the_node_runs_on() {
        use dash_evo::locks::RecoveredSig;
        use dash_params::LlmqType;
        use dashd_quorums::signing::SigningFault;
        use dashd_rpc::evo::{EvoError, NodeRecSigs as _};
        use dashd_rpc::protocol::RpcError;
        struct Faulty;
        fn fault<T>() -> Result<T, SigningFault> {
            Err(SigningFault::Storage("injected read fault".into()))
        }
        impl rpc::evo::recsig::RecSigReads for Faulty {
            fn has(&self, _: LlmqType, _: &[u8; 32], _: &[u8; 32]) -> Result<bool, SigningFault> {
                fault()
            }
            fn get(
                &self,
                _: LlmqType,
                _: &[u8; 32],
                _: &[u8; 32],
            ) -> Result<Option<RecoveredSig>, SigningFault> {
                fault()
            }
            fn conflicting(
                &self,
                _: LlmqType,
                _: &[u8; 32],
                _: &[u8; 32],
            ) -> Result<bool, SigningFault> {
                fault()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        let faulty: Arc<dyn rpc::evo::recsig::RecSigReads> = Arc::new(Faulty);
        node.rpc_chain.set_signing(Arc::clone(&faulty));
        assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
        let slot = Arc::clone(&node.rpc_chain);
        let text = fault::<()>().unwrap_err().to_string();
        let want = RpcError::new(-1, &text).to_object();
        let rpc = |e: EvoError| e.into_rpc(|_| unreachable!()).to_object();
        assert_eq!(rpc(slot.has_recovered_sig(100, &[2; 32], &[3; 32]).unwrap_err()), want);
        assert_eq!(rpc(slot.get_recovered_sig(100, &[2; 32], &[3; 32]).unwrap_err()), want);
        assert_eq!(rpc(slot.is_conflicting(100, &[2; 32], &[3; 32]).unwrap_err()), want);
        assert_eq!(node.abort.reports(), 0);
        assert!(!node.shutdown.shutdown_requested());
        node.shutdown();
    }

    /// Whether the node's recovered-signature database holds the test row.
    fn recsig_row(node: &Node) -> bool {
        init::recsigs::tests::planted(&node.quorums.as_ref().expect("made at step 7").recsigs)
    }

    /// Writes the test row into the recovered-signature database in `net`, with no node running.
    fn plant_recsig_row(net: &Path) {
        let mut db = init::recsigs::RecoveredSigsDb::unopened();
        db.open(net, false).unwrap();
        init::recsigs::tests::plant(&mut db);
    }

    /// Whether a node started on `dir` with `argv`, which loads, holds the test row.
    fn started_recsig_row(dir: &Path, argv: &[&str]) -> bool {
        let (mut node, _, stderr) = start(dir, argv);
        assert!(node.app_init_early() && node.app_init_main(), "{argv:?}: {stderr:?}");
        let row = recsig_row(&node);
        node.shutdown();
        row
    }

    /// The recovered-signature database is on disk beside the quorum database: a row written
    /// through the node's handle is there after a plain restart, and the shutdown closes the
    /// database, so another handle opens it before the node is dropped. `-reindex-chainstate`
    /// wipes it, and the next plain start finds it empty.
    // Core: v24.0.2 src/llmq/signing.cpp:24-27, src/node/chainstate.cpp:149, :159-160,
    // src/init.cpp:462
    #[test]
    fn recsig_rows_survive_a_restart_and_reindex_chainstate_wipes_them() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, _) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        assert!(!recsig_row(&node));
        let mut db = node.quorums.as_ref().expect("made at step 7").recsigs.clone();
        init::recsigs::tests::plant(&mut db);
        drop(db);
        node.shutdown();
        let net = dir.path().join("regtest");
        assert!(net.join(init::recsigs::RECSIG_DB_DIR).join("CURRENT").is_file());
        assert!(init::recsigs::RecoveredSigsDb::unopened().open(&net, false).is_ok(), "closed");
        drop(node);
        assert!(started_recsig_row(dir.path(), &[]));
        assert!(started_recsig_row(dir.path(), &[]));
        assert!(!started_recsig_row(dir.path(), &["-reindex-chainstate"]));
        assert!(!started_recsig_row(dir.path(), &[]));
    }

    /// `-reindex` wipes the recovered-signature database (a datadir with no block data, as T14).
    // Core: v24.0.2 src/node/chainstate.cpp:149, :159-160
    #[test]
    fn reindex_wipes_recsig_rows() {
        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        let net = dir.path().join("regtest");
        plant_recsig_row(&net);
        assert!(!net.join("blocks").join("blk00000.dat").exists(), "no block data");
        assert!(!started_recsig_row(dir.path(), &["-reindex"]));
    }

    /// `-maxrecsigsage` reaches the value init keeps next to the recovered-signature database,
    /// a week when it is not given.
    // Core: v24.0.2 src/init.cpp:2075, src/node/chainstate.cpp:159-161, src/llmq/context.cpp:23
    #[test]
    fn maxrecsigsage_reaches_the_signing_managers_options() {
        for (argv, age) in [(&[][..], 604_800), (&["-maxrecsigsage=60"][..], 60)] {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, _, stderr) = start(dir.path(), argv);
            assert!(node.app_init_early() && node.app_init_main(), "{argv:?}: {stderr:?}");
            assert_eq!(node.quorums.as_ref().expect("made at step 7").max_recsigs_age, age);
            node.shutdown();
        }
    }

    /// A recovered-signature database that does not open fails the load as the quorum database's
    /// does, after the quorum database opened and `q-cache` started: Core builds `sigman` after
    /// `qman`, whose constructor starts the thread.
    // Core: v24.0.2 src/llmq/context.cpp:21-23, src/llmq/quorumsman.cpp:40-46,
    // src/dbwrapper.cpp:155-156; src/init.cpp:2022-2029, :2088, :2109-2126
    #[test]
    fn a_recsig_store_open_failure_is_cores_load_failure() {
        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        let net = dir.path().join("regtest");
        let path = net.join(init::recsigs::RECSIG_DB_DIR);
        crate::datadir::tests::mark(&path);
        std::fs::write(path.join("CURRENT"), "").unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(*stderr.lock().unwrap(), [OPEN_QUESTION]);
        let cause = position(&log, "Error opening the recovered signature database");
        assert!(cause < position(&log, "Aborted block database rebuild. Exiting."));
        let quorums = node.quorums.as_ref().expect("made at step 7");
        assert!(quorums.qcache().is_started());
        assert!(net.join(init::quorums::QUORUM_DB_DIR).join("CURRENT").is_file());
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        node.shutdown();
    }

    /// `node.llmq_ctx.reset()` joins `q-cache` after the shutdown flushes and before
    /// "Shutdown: done", even while another handle to the manager lives. The marker is a
    /// thread-local of `d-q-cache`'s, whose destructor runs before the join returns.
    // Core: v24.0.0 src/init.cpp:462, :519; src/llmq/quorumsman.cpp:50-56
    #[test]
    fn shutdown_joins_the_qcache_thread_before_shutdown_done() {
        use std::cell::RefCell;

        use dashd_quorums::WarmExecutor as _;

        struct Marker(Lines);

        // The 100 ms wait makes a shutdown that does not join log "Shutdown: done" first.
        impl Drop for Marker {
            fn drop(&mut self) {
                std::thread::sleep(Duration::from_millis(100));
                let mut lines = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                lines.push("q-cache thread joined".to_owned());
            }
        }

        thread_local! {
            static MARKER: RefCell<Option<Marker>> = const { RefCell::new(None) };
        }

        for round in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr, points) = settled(dir.path());
            let quorums = node.quorums.as_ref().expect("made at step 7");
            let kept = (round % 2 == 1).then(|| Arc::clone(&quorums.manager));
            let (planted, on_thread) = std::sync::mpsc::channel();
            let sink = Arc::clone(&log);
            quorums.qcache().spawn_warm(Box::new(move || {
                MARKER.with(|m| *m.borrow_mut() = Some(Marker(sink)));
                planted.send(()).unwrap();
            }));
            on_thread.recv_timeout(Duration::from_secs(30)).expect("the job runs");
            node.shutdown();
            let joined = position(&log, "q-cache thread joined");
            let flushes: Vec<usize> = (points.lock().unwrap().iter())
                .filter(|(p, _)| matches!(p, Point::Flush { .. }))
                .map(|&(_, at)| at)
                .collect();
            assert!(!flushes.is_empty(), "round {round}");
            assert!(flushes.iter().all(|&at| at <= joined), "round {round}: {flushes:?} {joined}");
            assert!(joined < position(&log, "Shutdown: done"), "round {round}");
            assert!(stderr.lock().unwrap().is_empty(), "round {round}");
            drop(kept);
        }
    }

    /// Core's reindex question without a GUI: an empty caption, and the answer is no.
    #[test]
    fn an_unreadable_chainstate_asks_for_a_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        std::fs::write(dir.path().join("regtest").join("chainstate"), "not a database").unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        let question = ": Error opening block database.\nPlease restart with -reindex or \
                        -reindex-chainstate to recover.\n";
        assert_eq!(*stderr.lock().unwrap(), [question]);
        // The store's own error first, as Core's `catch_exceptions` logs it.
        let asked = position(&log, question);
        assert!(position(&log, "init message: Loading block index…") < asked - 1);
        assert!(position(&log, "Aborted block database rebuild. Exiting.") > asked);
        // Without a devnet step V starts once the coins are open (ND1-D6): never, here.
        assert!(node.steps.chain().is_none());
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        // No coins cache, no flush (Core's `CanFlushToDisk`).
        node.shutdown();
        assert_eq!(node.steps.flushes(), 0);
    }

    /// Under `-reindex` Core asks no question: the load's message is init's error, with no
    /// "Aborted block database rebuild" line.
    // Core: init.cpp:2109-2126
    #[test]
    fn under_reindex_an_unreadable_chainstate_is_init_s_error() {
        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        std::fs::write(dir.path().join("regtest").join("chainstate"), "not a database").unwrap();
        let (mut node, log, stderr) = start(dir.path(), &["-reindex"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(*stderr.lock().unwrap(), ["Error: Error opening block database\n"]);
        assert!(!has(&log, "Please restart with -reindex"));
        assert!(!has(&log, "Aborted block database rebuild"));
        assert!(node.steps.chain().is_none());
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        node.shutdown();
    }

    /// Core asks its reindex question unless the argument is given: a stored reindex (Core's
    /// `fReindex` after `LoadBlockIndexDB`) over coins whose best block the index lacks is still
    /// "Error initializing block database" and the question, declined.
    // Core: init.cpp:2109-2126, node/chainstate.cpp:272-276
    #[test]
    fn a_stored_reindex_still_asks_the_rebuild_question() {
        use dashd_chainstate::store::{CHAINSTATE_SCHEMA, Meta};
        use dashd_store::{Commit, KvEngine as _, RocksConfig, RocksEngine};
        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        let chainstate = dir.path().join("regtest").join("chainstate");
        let (db, _) =
            RocksEngine::open(&chainstate, &CHAINSTATE_SCHEMA, &RocksConfig::default()).unwrap();
        let mut put = db.batch();
        dashd_store::KvBatch::put(
            &mut put,
            CHAINSTATE_SCHEMA.cf("meta").unwrap(),
            b"tip",
            &[7; 32],
        );
        Meta::put_reindexing(&mut put, true);
        db.write(put, Commit::Durable).unwrap();
        drop(db);
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        let question = ": Error initializing block database.\nPlease restart with -reindex or \
                        -reindex-chainstate to recover.\n";
        assert_eq!(*stderr.lock().unwrap(), [question]);
        assert!(has(&log, "Aborted block database rebuild. Exiting."));
        node.shutdown();
    }

    /// BL-636 row 10 through the node (DEC-1071 condition 1's exit status): coins whose best
    /// block the freshly initialized block index lacks. Init fails with "Error initializing block
    /// database" and the reindex question, exit status 1, and shutdown runs both bare flushes,
    /// without a fault and with an `AbortNode` in them. That fault is the first flush's WAL sync,
    /// which stays failed: the node's files are the OS's, so no free-space fault reaches it.
    // Core: node/chainstate.cpp:272-276; validation.cpp:3127-3129, :3137; bitcoind.cpp:280-287
    #[test]
    fn an_unknown_coins_tip_fails_init_with_exit_status_1() {
        use dashd_chainstate::store::CHAINSTATE_SCHEMA;
        use dashd_store::{Commit, KvBatch as _, KvEngine as _, RocksConfig, RocksEngine};
        for sync_fails in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (_, params) = config(dir.path(), &[]);
            params.unwrap();
            let chainstate = dir.path().join("regtest").join("chainstate");
            let (db, _) =
                RocksEngine::open(&chainstate, &CHAINSTATE_SCHEMA, &RocksConfig::default())
                    .unwrap();
            let mut put = db.batch();
            put.put(CHAINSTATE_SCHEMA.cf("meta").unwrap(), b"tip", &[7; 32]);
            db.write(put, Commit::Durable).unwrap();
            drop(db);
            let (mut node, log, stderr) = start(dir.path(), &[]);
            assert!(node.app_init_early());
            assert!(!node.app_init_main());
            let question = ": Error initializing block database.\nPlease restart with -reindex or \
                            -reindex-chainstate to recover.\n";
            assert_eq!(*stderr.lock().unwrap(), [question], "{sync_fails}");
            assert_eq!(node.exit_status(), EXIT_FAILURE, "{sync_fails}");
            if sync_fails {
                node.steps.fail_next_wal_sync();
            }
            node.shutdown();
            assert_eq!(node.steps.flushes(), 2, "{sync_fails}");
            assert_eq!(node.exit_status(), EXIT_FAILURE, "{sync_fails}");
            let lines = log.lock().unwrap();
            let reports = lines.iter().filter(|l| l.contains("*** System error while flushing"));
            assert_eq!(
                reports.count(),
                if sync_fails {
                    2
                } else {
                    0
                },
                "{lines:?}"
            );
            let started = "FlushStateToDisk: write evodb cache to disk started\n";
            let pairs = lines.iter().filter(|l| l.ends_with(started)).count();
            assert_eq!(
                pairs,
                if sync_fails {
                    0
                } else {
                    2
                },
                "{lines:?}"
            );
        }
    }

    /// Polls `path` until it holds `n` lines (up to ten seconds); its lines.
    #[cfg(unix)]
    fn lines_of(path: &Path, n: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let lines: Vec<String> = text.lines().map(str::to_owned).collect();
            if lines.len() >= n || Instant::now() > deadline {
                return lines;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// `feature_startupnotify`: once per successful start, never on a failed one;
    /// `-shutdownnotify` runs in `Interrupt`, joined.
    #[cfg(unix)]
    #[test]
    fn notify_hooks_run_once_per_start_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        let (started, stopped) = (dir.path().join("started"), dir.path().join("stopped"));
        let startup = format!("-startupnotify=echo up >> {}", started.display());
        let shutdown = format!("-shutdownnotify=echo down >> {}", stopped.display());
        for run in 1..=2 {
            let (mut node, _, _) = start(dir.path(), &[&startup, &shutdown]);
            assert!(node.app_init_early() && node.app_init_main());
            assert_eq!(lines_of(&started, run).len(), run);
            node.interrupt();
            assert_eq!(std::fs::read_to_string(&stopped).unwrap().lines().count(), run);
            node.shutdown();
        }

        // A failed init: no startup command, the shutdown command still (Core runs Interrupt).
        let (mut node, _, _) = start(dir.path(), &[&startup, &shutdown, "-sporkkey=nonsense"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        node.interrupt();
        assert_eq!(std::fs::read_to_string(&stopped).unwrap().lines().count(), 3);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(std::fs::read_to_string(&started).unwrap().lines().count(), 2);
    }

    /// Core's order (`init.cpp:504-519`): the pid file goes while the lock is still held, so
    /// another process never sees the lock free next to a pid file. Checked at "Shutdown: done",
    /// which comes between the two, from another process.
    #[cfg(unix)]
    #[test]
    fn the_lock_outlives_the_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _log, _stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        let net = dir.path().join("regtest");
        assert!(net.join(datadir::PID_FILENAME).exists());
        let seen = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&seen);
        let at = net.clone();
        node.ui.logger().push_back_callback(move |s| {
            if s.contains("Shutdown: done") {
                let refused = crate::datadir::tests::another_process_is_refused(&at);
                *sink.lock().unwrap() = Some((at.join(datadir::PID_FILENAME).exists(), refused));
            }
        });
        node.shutdown();
        assert_eq!(*seen.lock().unwrap(), Some((false, true)), "(pid file, lock held)");
        assert!(!crate::datadir::tests::another_process_is_refused(&net));
    }

    /// Exactly one `-shutdownnotify` per node however `Interrupt` is reached (Sol r1 P2a): called
    /// twice, left to `shutdown`, or left to `Drop` after a partial init.
    #[test]
    fn interrupt_runs_once_on_every_path() {
        let dir = tempfile::tempdir().unwrap();
        let stopped = dir.path().join("stopped");
        let shutdown = format!("-shutdownnotify=echo down >> {}", stopped.display());
        let count = || std::fs::read_to_string(&stopped).unwrap_or_default().lines().count();

        let (mut node, _, _) = start(dir.path(), &[&shutdown]);
        assert!(node.app_init_early() && node.app_init_main());
        node.interrupt();
        node.interrupt();
        node.shutdown();
        assert_eq!(count(), 1);

        let (mut node, _, _) = start(dir.path(), &[&shutdown]);
        assert!(node.app_init_early() && node.app_init_main());
        node.shutdown();
        drop(node);
        assert_eq!(count(), 2);

        let (mut node, _, _) = start(dir.path(), &[&shutdown]);
        assert!(node.app_init_early());
        drop(node);
        assert_eq!(count(), 3);
    }

    #[test]
    fn a_shutdown_during_init_exits_before_loading() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        node.shutdown.start_shutdown();
        assert!(!node.app_init_main());
        assert!(has(&log, "Shutdown requested. Exiting."));
        assert!(!has(&log, "Loading block index"));
        assert!(!has(&log, "Done loading"));
        assert!(stderr.lock().unwrap().is_empty());
        assert_eq!(node.exit_status(), EXIT_FAILURE);
    }

    /// ND1-D27's `Option`-gated shutdown (1 of 4): step 6 fails on Core's `-uacomment` check, as
    /// Core does before the chainstate loads. The network core exists by then, as Core's
    /// `CConnman` does (BL-327 D2); shutdown stops it and writes no anchors.
    // Core: init.cpp:1690-1739, :1835-1837
    #[test]
    fn a_bad_uacomment_fails_step_6_before_the_load() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &["-uacomment=a/b"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(
            *stderr.lock().unwrap(),
            ["Error: User Agent comment (a/b) contains unsafe characters.\n"]
        );
        assert!(!has(&log, "Loading block index"));
        assert!(!node.net.as_ref().expect("built at step 6").parts().attached);
        node.interrupt();
        node.shutdown();
        assert!(node.net.is_none());
        assert!(!dir.path().join("regtest").join("anchors.dat").exists());
    }

    /// ND1-D27 (2 of 4): a shutdown requested before step 7c leaves step 6's network core with
    /// no processor; shutdown stops only the core, and writes no anchors (`Start` never ran).
    #[test]
    fn a_shutdown_before_step_7c_stops_only_the_core() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        node.shutdown.start_shutdown();
        assert!(!node.app_init_main());
        assert_eq!(
            node.net.as_ref().expect("built at step 6").parts(),
            p2p::Parts {
                attached: false,
                msghand: false,
                connman: false,
                registered: false,
                sync_registered: false,
                dstx_registered: false,
                list_changed_registered: false,
                active_registered: false,
                stopped: false
            }
        );
        node.interrupt();
        node.shutdown();
        assert!(node.net.is_none());
        assert!(!dir.path().join("regtest").join("anchors.dat").exists());
        assert!(stderr.lock().unwrap().is_empty());
    }

    /// ND1-D27 (3 of 4): init that fails between step 7c and step 12 leaves a network that was
    /// attached, with the listener on V's signals, but never started; shutdown unregisters it,
    /// has no thread to join, and writes no anchors.
    // Core: init.cpp:2577-2580, :347-348, net.cpp:4187-4194
    #[test]
    fn a_network_attached_but_never_started_only_unregisters() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        let shutdown = Arc::clone(&node.shutdown);
        node.steps.probe = Some(Box::new(move |point| {
            if point == Point::Import {
                shutdown.start_shutdown();
            }
        }));
        assert!(!node.app_init_main());
        assert!(has(&log, "Shutdown requested. Exiting."));
        let parts = node.net.as_ref().expect("built at step 6").parts();
        assert_eq!(
            parts,
            p2p::Parts {
                attached: true,
                msghand: false,
                connman: false,
                registered: true,
                sync_registered: true,
                dstx_registered: true,
                list_changed_registered: true,
                active_registered: false,
                stopped: false
            }
        );
        node.interrupt();
        node.shutdown();
        assert!(node.net.is_none());
        assert!(!dir.path().join("regtest").join("anchors.dat").exists());
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
    }

    /// ND1-D27 (4 of 4): a started network stops in `Shutdown`, after the RPC servers: the
    /// listener off V's signals, then `StopThreads` (`msghand`, the connection manager), then
    /// `StopNodes`, which writes `peers.dat` and, with no `-connect`, `anchors.dat`, then the
    /// processor dropped (`node.peerman.reset()`).
    // Core: init.cpp:347-348, :365, net.cpp:4168-4210
    #[test]
    fn a_started_network_stops_in_core_s_order() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), &["-dnsseed=0", "-fixedseeds=0"]);
        assert!(node.app_init_early() && node.app_init_main());
        let net = node.net.as_mut().expect("started at step 12");
        assert_eq!(
            net.parts(),
            p2p::Parts {
                attached: true,
                msghand: true,
                connman: true,
                registered: true,
                sync_registered: true,
                dstx_registered: true,
                list_changed_registered: true,
                active_registered: false,
                stopped: false
            }
        );
        let regtest = dir.path().join("regtest");
        assert!(!regtest.join("anchors.dat").exists(), "Start reads and removes it");
        net.stop();
        assert_eq!(
            net.parts(),
            p2p::Parts {
                attached: false,
                msghand: false,
                connman: false,
                registered: false,
                sync_registered: false,
                dstx_registered: false,
                list_changed_registered: false,
                active_registered: false,
                stopped: true
            }
        );
        assert!(regtest.join("peers.dat").is_file(), "{:?}", tree(dir.path()));
        assert!(regtest.join("anchors.dat").is_file(), "{:?}", tree(dir.path()));
        // A second stop, as `Drop` runs it, does nothing.
        net.stop();
        node.interrupt();
        node.shutdown();
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        assert_eq!(node.exit_status(), 0);
    }

    /// Once a shutdown is requested, a recoverable load failure asks no question and init exits
    /// quietly. The case is the coins store's error, whose cause init logs before its check (as
    /// `catch_exceptions` does); the request comes with that line, after every interrupt check
    /// of the load. `Failure` and `Store` take the same arm (`load_failures_take_core_s_statuses`).
    // Core: init.cpp:2032-2038, :2119, :2142-2145
    #[test]
    fn a_load_failure_after_a_shutdown_request_asks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (_, params) = config(dir.path(), &[]);
        params.unwrap();
        std::fs::write(dir.path().join("regtest").join("chainstate"), "not a database").unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        let shutdown = Arc::clone(&node.shutdown);
        node.ui.logger().push_back_callback(move |s| {
            if s.contains("rocksdb error") {
                shutdown.start_shutdown();
            }
        });
        assert!(!node.app_init_main());
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        assert!(!has(&log, "Please restart with -reindex"));
        assert!(!has(&log, "Aborted block database rebuild"));
        assert!(position(&log, "rocksdb error") < position(&log, "Shutdown requested. Exiting."));
        assert_eq!(node.exit_status(), EXIT_FAILURE);
    }

    /// W3 on a masternode: step 7c's `attach_active` puts the active masternode's listener on
    /// V's signals and hands back its startup `Init`; `Shutdown` takes the listener off and joins
    /// the `am1-tip` worker. Without one (every node until W1) it does nothing.
    // Core: init.cpp:2538-2547; active/context.cpp:88-98 at v24.0.0
    #[test]
    fn the_active_masternode_wiring_registers_and_stops_with_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, _) = start(dir.path(), &["-dnsseed=0", "-fixedseeds=0"]);
        assert!(node.app_init_early() && node.app_init_main());
        let chain = node.steps.chain().expect("step 7 started V");
        let net = node.net.as_mut().expect("started at step 12");
        let abort = Arc::clone(&node.abort) as Arc<dyn crate::abort::AbortSink>;
        assert!(net.attach_active(chain, None, None, Arc::clone(&abort)).unwrap().is_none());
        assert!(!net.parts().active_registered);
        let args = dash_args::ArgsManager::new();
        args.force_set_arg("-masternodeblsprivkey", &"01".repeat(32));
        let key = dashd_masternode::active::parse(&args).unwrap().unwrap();
        let active = Arc::new(dashd_masternode::active::ActiveMasternode::new(key));
        let startup = net.attach_active(chain, Some(active), None, abort).unwrap();
        assert!(startup.is_some());
        assert!(net.parts().active_registered);
        let exit = net.active_worker_exit().expect("a worker");
        // A worker that ends after `stop` without a join is still running as it returns.
        exit.delay_ms.store(1_000, std::sync::atomic::Ordering::SeqCst);
        assert!(!exit.done.load(std::sync::atomic::Ordering::SeqCst));
        net.stop();
        assert!(!net.parts().active_registered);
        assert!(exit.done.load(std::sync::atomic::Ordering::SeqCst), "`stop` joined the worker");
        drop(startup);
        node.interrupt();
        node.shutdown();
    }

    /// A shutdown requested once the load has begun is the index load's INTERRUPTED: no blk-file
    /// check, no question.
    // Core: node/chainstate.cpp:177, init.cpp:2142-2145
    #[test]
    fn a_shutdown_as_the_index_loads_interrupts_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        let shutdown = Arc::clone(&node.shutdown);
        node.ui.logger().push_back_callback(move |s| {
            if s.contains("init message: Loading block index") {
                shutdown.start_shutdown();
            }
        });
        assert!(!node.app_init_main());
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        assert!(!has(&log, "Error"));
        assert!(
            position(&log, "Loading block index") < position(&log, "Shutdown requested. Exiting.")
        );
        assert!(node.steps.chain().is_none(), "V never started");
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        node.shutdown();
        assert_eq!(node.steps.flushes(), 0);
    }

    /// Sol F2 r1 F1: complete init, then `AbortNode`, then shutdown. As Core, both flushes run
    /// (`CanFlushToDisk` is the loaded coins, not the abort), and the exit status stays 1.
    // Core: shutdown.cpp:27-37, init.cpp:379-385, :449-455
    #[test]
    fn an_aborted_node_still_flushes_twice_and_exits_1() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        node.abort.abort_node("injected abort", None);
        node.shutdown();
        assert_eq!(node.steps.flushes(), 2);
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        assert_eq!(
            *stderr.lock().unwrap(),
            ["Error: A fatal internal error occurred, see debug.log for details\n"]
        );
        assert!(!has(&log, "failed to flush state"));
        assert!(!dir.path().join("regtest").join("dashd.pid").exists());
        assert!(has(&log, "Shutdown: done"));
    }

    /// Sol F2 r1 F2, r2 P2, DEC-253: the first shutdown flush's WAL sync fails, once. As Core's
    /// `FlushStateToDisk`, it is reported as `HandleError`'s two lines, an `AbortNode` (exit 1) and
    /// the flush's failure line. The failure stays, as a failed log sync does in LevelDB: the
    /// second flush runs, the store's latch refuses it although the fault has cleared, and it
    /// reports the same failure the same way, a second `AbortNode` included.
    // Core: validation.cpp:3116-3117, :3122-3126, shutdown.cpp:27-37, noui.cpp:42-45,
    // dbwrapper.cpp:265-267, leveldb/db/db_impl.cc:1229-1244, :650-655, :1328-1331
    #[test]
    fn a_failed_shutdown_flush_aborts_the_node() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        node.steps.fail_next_wal_sync();
        node.shutdown();
        assert_eq!(node.steps.flushes(), 2);
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        let error = "Error: A fatal internal error occurred, see debug.log for details\n";
        assert_eq!(*stderr.lock().unwrap(), [error, error]);
        let e = "rocksdb error (Io): injected WAL sync failure";
        let system = format!("System error while flushing: {e}");
        let report = [
            format!("{e}\n"),
            "You can use -debug=leveldb to get more complete diagnostic messages\n".to_owned(),
            format!("*** {system}\n"),
            error.to_owned(),
            format!("ForceFlushStateToDisk: failed to flush state ({system})\n"),
        ];
        let lines = log.lock().unwrap();
        let tail = |l: &String| l.split_once("Z ").map_or(l.clone(), |(_, t)| t.to_owned());
        let from = lines.iter().position(|l| l.ends_with(&report[0])).expect("the first report");
        let got: Vec<_> = lines[from..].iter().map(tail).filter(|l| report.contains(l)).collect();
        assert_eq!(got, [report.clone(), report].concat());
        drop(lines);
        assert!(has(&log, "Shutdown: done"));
    }

    /// Each probe point with the log's length when it was reached.
    type Points = Arc<Mutex<Vec<(Point, usize)>>>;

    /// BL-407's node on `dir`: init done, then step 11's `loadblk` joined, so startup's ABC and
    /// its flushes are over before a test trips the latch (a flush of theirs meeting it would
    /// raise D216's second abort). The probe records the points from then on.
    fn settled(dir: &Path) -> (Node, Lines, Lines, Points) {
        settled_with(dir, &[])
    }

    /// [`settled`] with `argv`.
    fn settled_with(dir: &Path, argv: &[&str]) -> (Node, Lines, Lines, Points) {
        let (mut node, log, stderr) = start(dir, argv);
        assert!(node.app_init_early() && node.app_init_main());
        assert!(node.steps.join_import(), "loadblk ends");
        let points = Points::default();
        let (record, lines) = (Arc::clone(&points), Arc::clone(&log));
        node.steps.probe = Some(Box::new(move |point| {
            let at = lines.lock().unwrap().len();
            record.lock().unwrap().push((point, at));
        }));
        (node, log, stderr, points)
    }

    /// A log line without its timestamp and newline.
    fn text(line: &str) -> &str {
        line.split_once("Z ").map_or(line, |(_, t)| t).trim_end_matches('\n')
    }

    /// The shutdown's failure lines among `lines`: an `AbortNode`'s two, the engine's
    /// (`HandleError`'s, the standalone `Fatal LevelDB error` included), and a flush's failure
    /// line.
    fn failure_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        let kinds = [
            "*** ",
            "Error: ",
            "Fatal LevelDB error",
            "-debug=leveldb",
            "System error while flushing",
            "failed to flush state",
            "fatal internal error",
        ];
        lines.into_iter().filter(|l| kinds.iter().any(|k| l.contains(k))).collect()
    }

    /// Each shutdown flush's own lines: from its probe point to the second flush's, and from that
    /// one to `Shutdown: done`, where nothing else of shutdown logs but the hooks build's lines.
    /// Both flushes started after V stopped, with `aborted` as the node then was.
    fn flush_local(log: &Lines, points: &Points, aborted: [bool; 2]) -> [Vec<String>; 2] {
        let points = points.lock().unwrap();
        let reached: Vec<&Point> = points.iter().map(|(p, _)| p).collect();
        let flush = |aborted| Point::Flush {
            aborted,
        };
        assert_eq!(reached, [&Point::VStopped, &flush(aborted[0]), &flush(aborted[1])]);
        let done = position(log, "Shutdown: done");
        let lines = log.lock().unwrap();
        let local = |from: usize, to: usize| -> Vec<String> {
            (lines[from..to].iter().map(|l| text(l)))
                .filter(|l| !l.starts_with("test-hooks: "))
                .map(str::to_owned)
                .collect()
        };
        [local(points[1].1, points[2].1), local(points[2].1, done)]
    }

    /// A shutdown after `first`, the lines of its one `AbortNode` (none: no report), in `ctx`:
    /// both flushes ran, neither logged a line of its own, `first` are the whole log's failure
    /// lines, and stderr, the report count and the exit status are that one report's.
    // Core: validation.cpp:3052-3121; init.cpp:379-386, :449-456; shutdown.cpp:27-38
    fn assert_silent(
        node: &Node,
        (log, stderr, points): (&Lines, &Lines, &Points),
        first: &[String],
        ctx: &str,
    ) {
        assert_eq!(node.steps.flushes(), 2, "{ctx}");
        let aborted = !first.is_empty();
        let none: [Vec<String>; 2] = Default::default();
        assert_eq!(flush_local(log, points, [aborted; 2]), none, "{ctx}: both flushes silent");
        let lines = log.lock().unwrap();
        assert_eq!(failure_lines(lines.iter().map(|l| text(l))), first, "{ctx}");
        drop(lines);
        let error: Vec<String> = first.get(1).map(|e| format!("{e}\n")).into_iter().collect();
        assert_eq!(*stderr.lock().unwrap(), error, "{ctx}");
        assert_eq!(node.abort.reports(), usize::from(aborted), "{ctx}");
        let exit = if aborted {
            EXIT_FAILURE
        } else {
            0
        };
        assert_eq!(node.exit_status(), exit, "{ctx}");
    }

    /// BL-407's reported shutdown on a settled node on `dir`: the latch tripped at `site` first if
    /// given, as a store's guard latches before its reporter reports; then the `AbortNode` whose
    /// text and user message `report` makes from the node's parameters, through the connector's
    /// abort path; then the shutdown. Also returns that report's two lines, the `Error:` line
    /// with Core's default user message unless `report` gives one.
    fn reported_shutdown(
        dir: &Path,
        site: Option<&'static str>,
        report: impl FnOnce(&ChainSetup) -> (String, Option<String>),
    ) -> (Node, Lines, Lines, Points, Vec<String>) {
        let (mut node, log, stderr, points) = settled(dir);
        let (t, user) = report(&node.params.chain);
        let chainstate = node.steps.chainstate();
        if let Some(site) = site {
            chainstate.trip_latch(site, &t);
        }
        chainstate.abort_path().abort_node(&t, user.as_deref());
        node.shutdown();
        let error = format!("Error: {}", user.as_deref().unwrap_or(DEFAULT_ABORT_USER_MESSAGE));
        (node, log, stderr, points, vec![format!("*** {t}"), error])
    }

    /// The DEC-253 form a shutdown flush logs for the latched store text `e`: `HandleError`'s two
    /// lines, the flush's `AbortNode` and its user error, then the flush's failure line.
    fn store_report(e: &str) -> Vec<String> {
        let system = format!("System error while flushing: {e}");
        vec![
            e.to_owned(),
            "You can use -debug=leveldb to get more complete diagnostic messages".to_owned(),
            format!("*** {system}"),
            format!("Error: {DEFAULT_ABORT_USER_MESSAGE}"),
            format!("ForceFlushStateToDisk: failed to flush state ({system})"),
        ]
    }

    /// BL-407: after a readiness refusal (AD-CH3-4; G1's, as G2's waits since DEC-1874) through
    /// the connector's abort path, as `SerialCore::refuse` makes it (the refusal is the user's
    /// message too), both shutdown flushes report nothing, as Core's do on the healthy database
    /// such an `AbortNode` leaves.
    /// The refusal stays the shutdown's one `AbortNode`: its two lines once, its user error, its
    /// warning, exit 1.
    // Core: validation.cpp:3052-3121; init.cpp:379-386, :449-456; shutdown.cpp:27-38
    #[test]
    fn a_readiness_refusal_leaves_both_shutdown_flushes_silent() {
        let dir = tempfile::tempdir().unwrap();
        let at = BlockAt {
            hash: BlockHash::from([0x18; 32]),
            height: 1,
            prev: None,
            id: None,
        };
        let (node, log, stderr, points, first) = reported_shutdown(dir.path(), None, |p| {
            let t = refusal_text(p, &at, &g1_capability("DIP0003"));
            (t.clone(), Some(t))
        });
        let t = first[0].strip_prefix("*** ").unwrap();
        assert_eq!(first[1], format!("Error: {t}"), "the refusal is the user's message");
        assert_silent(&node, (&log, &stderr, &points), &first, "readiness refusal");
        assert_eq!(node.warnings.get_warnings(false), t);
    }

    /// BL-407: the connector's other `AbortNode`s, with Core's default user message, leave both
    /// shutdown flushes silent too.
    // Core: validation.cpp:2296, :2516, :2535, :3052-3121; shutdown.cpp:27-38
    #[test]
    fn a_connector_abort_node_leaves_the_shutdown_flushes_silent() {
        for t in [CORRUPT_BLOCK, EVODB_INCONSISTENCY] {
            let dir = tempfile::tempdir().unwrap();
            let (node, log, stderr, points, first) =
                reported_shutdown(dir.path(), None, |_| (t.to_owned(), None));
            assert_silent(&node, (&log, &stderr, &points), &first, t);
            assert_eq!(node.warnings.get_warnings(false), t);
        }
    }

    /// BL-407, DEC-996: a block file's failure, latched where `GuardedFiles::mutate_as` latches it
    /// and reported as its reporter does, through the connector's abort path. Core keeps no file
    /// state and its shutdown flush reopens the file; under DEC-253 row 4 the latch refuses that
    /// retry, and both flushes are silent, Core's output when its retried commit succeeds. The
    /// failure's own `AbortNode` line is logged once, at the failure.
    // Core: node/blockstorage.cpp:577-605, :715, :784, :869; flatfile.cpp:82-100;
    // validation.cpp:3052-3121
    #[test]
    fn a_block_file_failure_leaves_the_shutdown_flushes_silent() {
        let cases = [
            ("Failed to write block", None),
            (DISK_SPACE_LOW, Some(DISK_SPACE_LOW)),
            (FLUSH_UNDO_FAILED, None),
        ];
        for (t, user) in cases {
            let dir = tempfile::tempdir().unwrap();
            let (node, log, stderr, points, first) =
                reported_shutdown(dir.path(), Some(BLOCK_FILES_WRITE_SITE), |_| {
                    (t.to_owned(), user.map(str::to_owned))
                });
            assert_silent(&node, (&log, &stderr, &points), &first, t);
            let lines = log.lock().unwrap();
            let logged: Vec<usize> = (lines.iter().enumerate())
                .filter(|(_, l)| text(l) == first[0])
                .map(|(i, _)| i)
                .collect();
            let first_flush = points.lock().unwrap()[1].1;
            assert!(logged.len() == 1 && logged[0] < first_flush, "{t}: once, at the failure");
        }
    }

    /// BL-407: a read's latch and the simulated crash's, which nothing reports here (a read's
    /// reporter ends the process, and so does the crash, before any flush): both flushes are
    /// silent and the shutdown reports nothing.
    // Core: coins.cpp:356-370; txdb.cpp:156-161; leveldb/db/db_impl.cc:1112-1157
    #[test]
    fn a_read_or_crash_latch_leaves_the_shutdown_flushes_silent() {
        let cases = [
            (READ_SITE, "rocksdb error (Io): a failed read"),
            (CRASH_SITE, "Simulating a crash. Goodbye."),
        ];
        for (site, t) in cases {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr, points) = settled(dir.path());
            node.steps.chainstate().trip_latch(site, t);
            node.shutdown();
            assert_silent(&node, (&log, &stderr, &points), &[], site);
        }
    }

    /// BL-407 with CH7 D102: a raw chainstate row write's latch, reported as its store reports
    /// it, leaves both shutdown flushes silent: a failed evo snapshot write stops the node with
    /// one report (EV3 D21: store faults are not consensus rejects). The store's report is the
    /// shutdown's one `AbortNode` (BL-630).
    #[test]
    fn a_raw_write_latch_leaves_the_shutdown_flushes_silent() {
        let e = "rocksdb error (Io): injected write";
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr, points) = settled(dir.path());
        node.steps.chainstate().trip_latch(RAW_WRITE_SITE, e);
        let system = format!("System error while flushing: {e}");
        node.abort.abort_node(&system, None);
        node.shutdown();
        let first = [format!("*** {system}"), format!("Error: {DEFAULT_ABORT_USER_MESSAGE}")];
        assert_silent(&node, (&log, &stderr, &points), &first, RAW_WRITE_SITE);
    }

    /// BL-407 masks no store failure: a chainstate write's or barrier's latch, reported as W
    /// reports it, still fails both shutdown flushes with the DEC-253 form, LevelDB's kept status.
    // Core: dbwrapper.cpp:261-268; validation.cpp:3122-3134; leveldb/db/db_impl.cc:650-656,
    // :1228-1244, :1328-1331
    #[test]
    fn a_store_failure_still_fails_both_shutdown_flushes() {
        let e = "rocksdb error (Io): injected write";
        for site in [WRITE_SITE, BARRIER_SITE] {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr, points) = settled(dir.path());
            node.steps.chainstate().trip_latch(site, e);
            node.abort.abort_node(&format!("System error while flushing: {e}"), None);
            node.shutdown();
            let report = store_report(e);
            assert_eq!(flush_local(&log, &points, [true; 2]), [report.clone(), report], "{site}");
            assert_eq!(node.abort.reports(), 3, "{site}");
            let error = format!("Error: {DEFAULT_ABORT_USER_MESSAGE}\n");
            assert_eq!(*stderr.lock().unwrap(), [error.clone(), error.clone(), error], "{site}");
            assert_eq!(node.exit_status(), EXIT_FAILURE, "{site}");
        }
    }

    /// BL-407: a site `kept` does not know is the store's, so its failure is never silenced.
    #[test]
    fn an_unknown_site_is_never_silenced() {
        let e = "subscriber error: a failed write";
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, _, points) = settled(dir.path());
        node.steps.chainstate().trip_latch("subscriber store", e);
        node.shutdown();
        let report = store_report(e);
        assert_eq!(flush_local(&log, &points, [false, true]), [report.clone(), report]);
        assert_eq!(node.abort.reports(), 2);
        assert_eq!(node.exit_status(), EXIT_FAILURE);
    }

    /// BL-586, BL-601: a shutdown after one of Core's asserts, latched at `site` with `latched` as
    /// its trip names it and reported once as `report` with Core's default user message, as its
    /// reporter reports it: both flushes are silent, as Core runs neither after its assert. The
    /// report stands alone: its two lines, its user error, its warning, exit 1.
    // Core: util/check.h:49-51; init.cpp:383, :453; validation.cpp:3133-3139
    fn assert_after_an_assert(site: &'static str, latched: &str, report: &str) {
        let dir = tempfile::tempdir().unwrap();
        assert_after_an_assert_on(dir.path(), site, latched, report, |_| {});
    }

    /// [`assert_after_an_assert`] on `dir`, with `before` run on the settled node before the trip,
    /// its result returned.
    fn assert_after_an_assert_on<R>(
        dir: &Path,
        site: &'static str,
        latched: &str,
        report: &str,
        before: impl FnOnce(&Node) -> R,
    ) -> R {
        let (mut node, log, stderr, points) = settled(dir);
        let seen = before(&node);
        node.steps.chainstate().trip_latch(site, latched);
        node.abort.abort_node(report, None);
        assert_eq!(node.steps.chainstate().latched_kept(), Some(Kept::Assert), "{site}");
        node.shutdown();
        let first = [format!("*** {report}"), format!("Error: {DEFAULT_ABORT_USER_MESSAGE}")];
        assert_silent(&node, (&log, &stderr, &points), &first, site);
        assert_eq!(node.warnings.get_warnings(false), report, "{site}");
        seen
    }

    /// BL-586: V's panic (CH1-node D62).
    #[test]
    fn a_v_panic_leaves_both_shutdown_flushes_silent() {
        assert_after_an_assert(V_PANIC_SITE, "the chain actor panicked", "V panicked: x");
    }

    /// BL-586: W's panic (CH2b M2), reported as its thread's end reports it.
    #[test]
    fn a_w_panic_leaves_both_shutdown_flushes_silent() {
        let panicked = "the chainstate flush thread panicked";
        assert_after_an_assert(W_PANIC_SITE, panicked, panicked);
    }

    /// BL-601: an EV attempt's unwind with no tip hold, reported by the attempt (CH3a D71).
    #[test]
    fn a_reported_ev_unwind_leaves_both_shutdown_flushes_silent() {
        let unwound = "an EV attempt unwound before its commit or rollback returned";
        assert_after_an_assert(UNWIND_SITE, unwound, unwound);
    }

    /// A file [`seed_net_caches`] seeded: its seed, and the node's empty store, which a shutdown
    /// store writes over the seed.
    struct Seeded {
        path: PathBuf,
        seed: Vec<u8>,
        empty: Vec<u8>,
    }

    impl Seeded {
        fn kept(&self) -> bool {
            std::fs::read(&self.path).unwrap() == self.seed
        }

        fn stored(&self) -> bool {
            std::fs::read(&self.path).unwrap() == self.empty
        }
    }

    /// `netfulfilled.dat` and `mncache.dat` in `node`'s network directory under `dir`, each
    /// replaced by a valid store with an entry the node's own managers do not hold.
    fn seed_net_caches(node: &Node, dir: &Path) -> [Seeded; 2] {
        use dash_clock::MockClock;
        use dash_evo::types::ProTxHash;
        use dash_netaddr::netaddr::{NetAddr, Service};
        use dashd_masternode::flatdb;
        use dashd_masternode::meta::{self, MasternodeMetaMan, MasternodeMetaStore};
        use dashd_masternode::netfulfilled::{self, NetFulfilledRequestManager, NetFulfilledStore};
        let (net, ms) = (dir.join("regtest"), node.params.chain.message_start);
        let (nf_path, meta_path) = (net.join(netfulfilled::FILE_NAME), net.join(meta::FILE_NAME));
        let fulfilled =
            NetFulfilledRequestManager::new(Arc::new(MockClock::at(1_700_000_000)), 300);
        fulfilled
            .add_fulfilled_request(&Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19899), "full-sync");
        assert!(fulfilled.store(&nf_path, ms));
        let metaman = MasternodeMetaMan::new();
        metaman.add_used_masternode(&ProTxHash::from([7; 32]));
        assert!(metaman.store(&meta_path, ms));
        let seeded = |path: PathBuf, empty: Vec<u8>| {
            let seed = std::fs::read(&path).unwrap();
            assert_ne!(seed, empty, "{}", path.display());
            Seeded {
                path,
                seed,
                empty,
            }
        };
        [
            seeded(nf_path, flatdb::encode(&NetFulfilledStore::default(), ms)),
            seeded(meta_path, flatdb::encode(&MasternodeMetaStore::default(), ms)),
        ]
    }

    /// N6's control: a clean shutdown of the same settled node stores both files over their
    /// seeds, so both managers are valid there.
    // Core: init.cpp:416-417; netfulfilledman.cpp:15-19; masternode/meta.cpp:25-29
    #[test]
    fn a_clean_shutdown_stores_both_net_caches() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr, _) = settled(dir.path());
        let seeds = seed_net_caches(&node, dir.path());
        node.shutdown();
        assert_eq!(node.steps.flushes(), 2);
        assert_eq!(node.abort.reports(), 0);
        assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
        assert_eq!(node.exit_status(), 0);
        for seed in &seeds {
            assert!(seed.stored(), "{}", seed.path.display());
        }
    }

    /// BL-586, DEC-1156 (N6): after each of N1-N3's asserts, shutdown leaves `mncache.dat` as it
    /// was, since its platform-ban resets are chain effects, and still stores `netfulfilled.dat`,
    /// network state. Core's killed process writes neither; each shutdown is otherwise N1-N3's.
    #[test]
    fn an_assert_keeps_the_previous_mncache() {
        let panicked = "the chainstate flush thread panicked";
        let unwound = "an EV attempt unwound before its commit or rollback returned";
        let cases = [
            (V_PANIC_SITE, "the chain actor panicked", "V panicked: x"),
            (W_PANIC_SITE, panicked, panicked),
            (UNWIND_SITE, unwound, unwound),
        ];
        for (site, latched, report) in cases {
            let dir = tempfile::tempdir().unwrap();
            let [netfulfilled, mncache] =
                assert_after_an_assert_on(dir.path(), site, latched, report, |node| {
                    seed_net_caches(node, dir.path())
                });
            assert!(mncache.kept(), "{site}: mncache.dat kept");
            assert!(netfulfilled.stored(), "{site}: netfulfilled.dat stored");
        }
    }

    /// BL-586, DEC-1156 (N6b): an assert latched once the first shutdown flush has started (W's
    /// panic there, N4's case) also keeps `mncache.dat`, since the store reads the latch after that
    /// flush. Both flushes are silent: the panic's report stands alone.
    #[test]
    fn an_assert_inside_the_first_shutdown_flush_keeps_the_previous_mncache() {
        let panicked = "the chainstate flush thread panicked";
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr, _) = settled(dir.path());
        let [netfulfilled, mncache] = seed_net_caches(&node, dir.path());
        let latch = node.steps.chainstate().abort_path().latch().clone();
        let abort = Arc::clone(&node.abort);
        let mut first = true;
        node.steps.probe = Some(Box::new(move |point| {
            if matches!(point, Point::Flush { .. }) && std::mem::take(&mut first) {
                let failure = dash_kv::Failure {
                    kind: dash_kv::ErrorKind::Other,
                    site: W_PANIC_SITE,
                    message: panicked.to_owned(),
                };
                assert!(latch.trip(failure), "W's panic latches first");
                abort.abort_node(panicked, None);
            }
        }));
        node.shutdown();
        assert_eq!(node.steps.flushes(), 2);
        assert_eq!(node.abort.reports(), 1);
        let error = format!("Error: {DEFAULT_ABORT_USER_MESSAGE}\n");
        assert_eq!(*stderr.lock().unwrap(), [error]);
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        assert!(mncache.kept(), "mncache.dat kept");
        assert!(netfulfilled.stored(), "netfulfilled.dat stored");
    }

    /// BL-586, DEC-1156 (N7): a failure that is not an assert still stores `mncache.dat`, as Core's
    /// orderly shutdown after an `AbortNode` does: here the first shutdown flush's failed WAL
    /// sync, a store's failure latched inside that flush and reported twice as in
    /// [`a_failed_shutdown_flush_aborts_the_node`].
    // Core: init.cpp:416-417; masternode/meta.cpp:25-29; validation.cpp:3127-3128, :3133-3139
    #[test]
    fn a_failed_shutdown_flush_still_stores_the_mncache() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr, _) = settled(dir.path());
        let [_, mncache] = seed_net_caches(&node, dir.path());
        node.steps.fail_next_wal_sync();
        node.shutdown();
        assert_eq!(node.steps.flushes(), 2);
        assert_eq!(node.abort.reports(), 2);
        let error = format!("Error: {DEFAULT_ABORT_USER_MESSAGE}\n");
        assert_eq!(*stderr.lock().unwrap(), [error.clone(), error]);
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        assert!(mncache.stored(), "mncache.dat stored");
    }

    /// BL-586, DEC-1156 (N7): each latched class but [`Kept::Assert`] still stores `mncache.dat`
    /// at shutdown: a read's latch, which nothing reports (`Nothing`); the flush's own failed
    /// space check (`Flush`), and a chainstate write's (`Store`), each reported as in
    /// [`a_flush_site_failure_from_before_shutdown_is_not_silenced`] and
    /// [`a_store_failure_still_fails_both_shutdown_flushes`].
    // Core: init.cpp:416-417; masternode/meta.cpp:25-29
    #[test]
    fn every_other_latched_failure_still_stores_the_mncache() {
        let (read, write) =
            ("rocksdb error (Io): a failed read", "rocksdb error (Io): injected write");
        let system = format!("System error while flushing: {write}");
        let cases = [
            (READ_SITE, read, None, Kept::Nothing, 0, 0),
            (
                FLUSH_SITE,
                DISK_SPACE_LOW,
                Some((DISK_SPACE_LOW, Some(DISK_SPACE_LOW))),
                Kept::Flush,
                3,
                EXIT_FAILURE,
            ),
            (WRITE_SITE, write, Some((system.as_str(), None)), Kept::Store, 3, EXIT_FAILURE),
        ];
        for (site, latched, report, kept, reports, exit) in cases {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, ..) = settled(dir.path());
            let [_, mncache] = seed_net_caches(&node, dir.path());
            node.steps.chainstate().trip_latch(site, latched);
            if let Some((t, user)) = report {
                node.abort.abort_node(t, user);
            }
            assert_eq!(node.steps.chainstate().latched_kept(), Some(kept), "{site}");
            node.shutdown();
            assert_eq!(node.abort.reports(), reports, "{site}");
            assert_eq!(node.exit_status(), exit, "{site}");
            assert!(mncache.stored(), "{site}: mncache.dat stored");
        }
    }

    /// What a shutdown's first failure decides: its reports, stderr, each shutdown flush's own
    /// lines, the flush count and the exit status.
    #[derive(Debug, PartialEq)]
    struct FirstFailure {
        reports: Vec<crate::abort::Report>,
        stderr: Vec<String>,
        flush_lines: [Vec<String>; 2],
        flushes: u32,
        exit: i32,
    }

    /// N7b's `Store` case on `dir`, a chainstate write's latch reported as W reports it, then a
    /// trip at `losing`, if given, that loses to it and reports nothing, as a losing trip does;
    /// then the shutdown, and its seeded caches.
    fn a_store_failure_then(
        dir: &Path,
        losing: Option<&'static str>,
    ) -> (FirstFailure, [Seeded; 2]) {
        let e = "rocksdb error (Io): injected write";
        let (mut node, log, stderr, points) = settled(dir);
        let seeds = seed_net_caches(&node, dir);
        node.steps.chainstate().trip_latch(WRITE_SITE, e);
        node.abort.abort_node(&format!("System error while flushing: {e}"), None);
        assert_eq!(node.steps.chainstate().latched_kept(), Some(Kept::Store));
        if let Some(site) = losing {
            let lost = !node.steps.chainstate().try_trip_latch(site, "a later assert");
            assert!(lost, "{site}: the trip loses to the store failure");
        }
        assert_eq!(node.steps.chainstate().asserted(), losing.is_some(), "{losing:?}");
        node.shutdown();
        let reports = (0..node.abort.reports()).filter_map(|n| node.abort.report(n)).collect();
        let first = FirstFailure {
            reports,
            stderr: stderr.lock().unwrap().clone(),
            flush_lines: flush_local(&log, &points, [true; 2]),
            flushes: node.steps.flushes(),
            exit: node.exit_status(),
        };
        (first, seeds)
    }

    /// BL-586, DEC-1156 (N8): an assert after an earlier failure also keeps `mncache.dat`, as
    /// Core's process ends at any assert. N7b's `Store` failure latches and is reported first;
    /// then V's, W's or an EV attempt's assert trips the latch, loses and reports nothing.
    /// `netfulfilled.dat` is still stored, and the reports, stderr, flushes and exit status stay
    /// exactly the `Store` case's: the first failure keeps its reports and flushes (B8).
    // Core: util/check.h:49-51; init.cpp:416-417; masternode/meta.cpp:25-29
    #[test]
    fn an_assert_after_an_earlier_failure_keeps_the_previous_mncache() {
        let e = "rocksdb error (Io): injected write";
        let dir = tempfile::tempdir().unwrap();
        let (store, [netfulfilled, mncache]) = a_store_failure_then(dir.path(), None);
        let system = format!("System error while flushing: {e}");
        let report = crate::abort::Report {
            log: system,
            user: None,
        };
        let error = format!("Error: {DEFAULT_ABORT_USER_MESSAGE}\n");
        let expected = FirstFailure {
            reports: vec![report; 3],
            stderr: vec![error; 3],
            flush_lines: [store_report(e), store_report(e)],
            flushes: 2,
            exit: EXIT_FAILURE,
        };
        assert_eq!(store, expected, "N7b's Store case");
        assert!(mncache.stored() && netfulfilled.stored(), "N7b's Store case stores both");
        for site in [V_PANIC_SITE, W_PANIC_SITE, UNWIND_SITE] {
            let dir = tempfile::tempdir().unwrap();
            let (got, [netfulfilled, mncache]) = a_store_failure_then(dir.path(), Some(site));
            assert_eq!(got, expected, "{site}: the store failure's shutdown");
            assert!(mncache.kept(), "{site}: mncache.dat kept");
            assert!(netfulfilled.stored(), "{site}: netfulfilled.dat stored");
        }
    }

    /// BL-407: the flush's own failure latched before shutdown (W's runtime space check) is not
    /// silenced: both shutdown flushes report again. Its text is BL-213's, which removes that
    /// latch; this pins only that each flush reports an `AbortNode`.
    // Core: validation.cpp:3056-3058, :3094-3096, :3128-3134
    #[test]
    fn a_flush_site_failure_from_before_shutdown_is_not_silenced() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, _, points) = settled(dir.path());
        node.steps.chainstate().trip_latch(FLUSH_SITE, DISK_SPACE_LOW);
        node.abort.abort_node(DISK_SPACE_LOW, Some(DISK_SPACE_LOW));
        node.shutdown();
        for (n, local) in flush_local(&log, &points, [true; 2]).iter().enumerate() {
            assert!(local.iter().any(|l| l.starts_with("*** ")), "flush {n}: {local:?}");
        }
        assert_eq!(node.abort.reports(), 3);
    }

    /// Dash Core v24.0.0-rc.4's shutdown after `AbortNode(state, "Failed to read block")` on a
    /// healthy database (`contrib/node/bl407-captures`), from the `***` line to `Shutdown: done`.
    const BL407_CAPTURE: &str = include_str!("../../contrib/node/bl407-captures/shutdown.txt");

    /// BL-407 against dashd v24: the connector's `AbortNode` with Core's "Failed to read block"
    /// and Core's default user message. The shutdown's failure lines match Core's: the first
    /// report's `***` line and user error, and none from either flush. This is not full log
    /// equality: Core's flushes write, and each logs the evodb timer pair, which rdashd's cannot
    /// after the trip (DEC-309, DEC-995, until BL-325).
    // Core: validation.cpp:3415-3418, :3052-3121; shutdown.cpp:27-38
    #[test]
    fn a_failed_block_read_abort_matches_dashd_v24() {
        let core = failure_lines(BL407_CAPTURE.lines());
        assert_eq!(
            core,
            [
                "*** Failed to read block",
                "Error: A fatal internal error occurred, see debug.log for details"
            ],
            "the capture's failure lines"
        );
        let started = "FlushStateToDisk: write evodb cache to disk started";
        assert_eq!(BL407_CAPTURE.lines().filter(|l| *l == started).count(), 2, "Core's pairs");
        let dir = tempfile::tempdir().unwrap();
        let (node, log, stderr, points, first) =
            reported_shutdown(dir.path(), None, |_| ("Failed to read block".to_owned(), None));
        assert_eq!(first, core, "the report's lines are Core's");
        let (from, done) = (position(&log, "*** "), position(&log, "Shutdown: done"));
        let lines = log.lock().unwrap();
        assert_eq!(failure_lines(lines[from..=done].iter().map(|l| text(l))), core);
        drop(lines);
        assert_silent(&node, (&log, &stderr, &points), &first, "Core's capture");
    }

    /// The first block of each `ChainStateFlushed` locator a listener is handed.
    #[derive(Default)]
    struct Locators(Mutex<Vec<String>>);

    impl dashd_chainstate::chain::ValidationInterface for Locators {
        fn chain_state_flushed(&self, locator: &[BlockHash]) {
            if let Some(tip) = locator.first() {
                self.0.lock().unwrap().push(tip.to_string());
            }
        }
    }

    /// `generatetoaddress n` on `node`'s chain through the node's command table: the blocks.
    fn generate(node: &Node, dir: &Path, n: u32) -> Vec<String> {
        let table = command_table(node, dir);
        // rpc_generate.py's regtest address of the generator point's key.
        let params = format!(r#"[{n}, "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6"]"#);
        let mined = crate::chain::tests::rpc(&table, "generatetoaddress", &params).expect("mined");
        let mined = dashd_rpc::UniValue::read(mined.as_bytes()).expect("JSON");
        let mined = mined.get_values().expect("an array");
        mined.iter().map(|h| h.get_str().unwrap().to_owned()).collect()
    }

    /// The node's command table, started and out of warmup.
    fn command_table(node: &Node, dir: &Path) -> Arc<dashd_rpc::server::RpcServer> {
        let table = rpc::command_table(
            &node.params.chain,
            node.ui.logger(),
            &node.shutdown,
            &node.clock,
            (
                ChainPolicy::for_setup(&node.params.chain, false).is_mockable_chain,
                node.scheduler.as_ref().unwrap(),
            ),
            &node.rpc_chain,
            &node.rpc_net,
            &node.rpc_sync,
            &node.warnings,
            Default::default(),
            dir.join("regtest"),
        );
        table.start();
        table.set_warmup_finished();
        table
    }

    /// DEC-995: a connector `AbortNode` lets no listener record a locator past the last durable
    /// chainstate flush. Core's first shutdown flush writes, then queues `ChainStateFlushed`; here
    /// nothing is written after the trip, so nothing may be signalled either. A recording listener
    /// stands in for a wallet, and the txindex, the basic block filter index and the coinstats
    /// index run on every start. A first run stops cleanly at `d`, a second connects three blocks
    /// on it and aborts, and a third finds the chainstate at its last durable flush: no locator
    /// the second run signalled, and neither the filter index's nor the coinstats index's stored
    /// one, is past it.
    // Core: validation.cpp:3117-3121; init.cpp:379-390, :421-441
    #[test]
    fn a_connector_abort_node_records_no_locator_past_the_last_durable_flush() {
        let filters = ["-blockfilterindex=basic", "-coinstatsindex"];
        let dir = tempfile::tempdir().unwrap();
        let (mut node, ..) = settled_with(dir.path(), &filters);
        let d = generate(&node, dir.path(), 2).pop().unwrap();
        node.shutdown();
        assert_eq!(node.exit_status(), 0);
        drop(node);

        let (mut node, ..) = settled_with(dir.path(), &filters);
        assert_eq!(node.tip().map(|h| h.to_string()), Some(d.clone()), "durable at d");
        let locators = Arc::new(Locators::default());
        node.steps.chain().unwrap().signals().register(Arc::clone(&locators) as _);
        let mined = generate(&node, dir.path(), 3);
        node.steps.chainstate().abort_path().abort_node(CORRUPT_BLOCK, None);
        node.shutdown();
        assert_eq!(node.steps.flushes(), 2);
        assert_eq!(node.abort.reports(), 1);
        drop(node);
        let indexed = filters::stored_locator(dir.path()).expect("a filter index locator");
        let stats = coinstats::stored_locator(dir.path()).expect("a coinstats index locator");

        let (mut node, ..) = settled_with(dir.path(), &filters);
        let durable = node.tip().expect("a tip").to_string();
        node.shutdown();
        assert_ne!(durable, mined[2], "blocks connected after the last durable flush");
        let mut chain = std::iter::once(&d).chain(&mined);
        assert!(chain.any(|h| *h == durable), "the last durable flush is d or a block on it");
        let past: Vec<&String> = chain.collect();
        let recorded = locators.0.lock().unwrap();
        assert!(recorded.iter().all(|l| !past.contains(&l)), "{recorded:?} past {durable}");
        assert!(!past.contains(&&indexed), "the filter index at {indexed}, past {durable}");
        assert!(!past.contains(&&stats), "the coinstats index at {stats}, past {durable}");
    }

    /// A listener whose `ChainStateFlushed` waits until shutdown has stopped V, then for the
    /// first flush (at most `WAIT` more), then panics: the node's `AbortNode`.
    struct GatedPanic(Mutex<std::sync::mpsc::Receiver<()>>);

    impl dashd_chainstate::chain::ValidationInterface for GatedPanic {
        fn chain_state_flushed(&self, _: &[BlockHash]) {
            const WAIT: Duration = Duration::from_secs(2);
            let gate = self.0.lock().unwrap();
            gate.recv_timeout(Duration::from_secs(120)).expect("shutdown stops V");
            let _ = gate.recv_timeout(WAIT);
            panic!("a gated listener fails");
        }
    }

    /// Sol F2 r1 F3: shutdown's drain (`ChainSteps::shutdown`'s `drain_background_callbacks`) runs a
    /// queued listener before the first flush. Once V has stopped, the listener holds until that
    /// flush starts or two seconds pass, then panics: with the drain, the flush can only start
    /// after the panic, so the abort is recorded before it; without, the flush starts at once
    /// (the next statement after V's stop), before the panic. The clock starts at V's stop, so
    /// a slow stop under load cannot let the mutant pass.
    // Core: init.cpp:354-360, :378-390
    #[test]
    fn shutdown_drains_the_signals_before_the_first_flush() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early() && node.app_init_main());
        let (release, gate) = std::sync::mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        node.steps.probe = Some(Box::new(move |point| {
            record.lock().unwrap().push(point);
            let _ = release.send(());
        }));
        let signals = node.steps.chain().unwrap().signals();
        signals.register(Arc::new(GatedPanic(Mutex::new(gate))));
        signals.enqueue(dashd_chainstate::chain::Queued::ChainStateFlushed(Vec::new()));
        node.shutdown();
        let aborted = || Point::Flush {
            aborted: true,
        };
        assert_eq!(
            *seen.lock().unwrap(),
            [Point::VStopped, aborted(), aborted()],
            "the abort before each flush"
        );
        assert_eq!(node.steps.flushes(), 2);
        assert_eq!(node.exit_status(), EXIT_FAILURE);
        assert_eq!(
            *stderr.lock().unwrap(),
            ["Error: A fatal internal error occurred, see debug.log for details\n"]
        );
        assert!(has(&log, "Shutdown: done"));
    }

    #[test]
    fn mocktime_stamps_the_log_from_step_3() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, _) = start(dir.path(), &["-mocktime=1700000000", "-logtimestamps=0"]);
        assert!(node.app_init_early());
        assert_eq!(node.clock.mocktime(), 1_700_000_000);
        assert!(node.app_init_main());
        drop(node);
        assert!(has(&log, "Done loading"));
    }

    /// BL-626: step 10a starts the ChainLock handler's 5 s task, so V observes SPORK_19 turning
    /// on with no block and no lock to trigger it, within a period; the task runs the seen-CLSIG
    /// `Cleanup` only once the blockchain is synced; shutdown stops the task.
    // Core: chainlock/handler.cpp:63-72, :209-222, :334-343; init.cpp:335, :2368
    #[test]
    fn the_chainlock_timer_observes_a_spork_19_flip_without_a_block() {
        let dir = tempfile::tempdir().unwrap();
        let key = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
        let (mut node, _, _) = start(dir.path(), &[key]);
        assert!(node.app_init_early() && node.app_init_main());
        let observed = |node: &Node| node.chain().unwrap().handle().view().snap.cl_enabled_observed;
        assert!(!observed(&node), "regtest starts with SPORK_19 off");
        let tip = node.tip();
        let sporks = Arc::clone(node.steps.sporks().unwrap());
        sporks.update_spork(dash_evo::spork::SporkId::SPORK_19_CHAINLOCKS_ENABLED, 0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !observed(&node) {
            assert!(Instant::now() < deadline, "no ChainLock task ran");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(node.tip(), tip, "no block arrived");
        let cl = node.cl_scheduler.clone().expect("made before step 7c");
        assert!(cl.are_threads_servicing_queue());

        // The task's `Cleanup`: held while `mn_sync` is at BLOCKCHAIN (no peer ever moves it on
        // here), run within a period once past it.
        // Core: chainlock/handler.cpp:334-343
        use dashd_rpc::dash::NodeSync as _;
        let after_the_running_task = |cl: &Scheduler| {
            let (tx, rx) = std::sync::mpsc::channel();
            cl.schedule_from_now(move || tx.send(()).unwrap(), Duration::ZERO);
            rx.recv_timeout(Duration::from_secs(30)).expect("cl-schdlr runs");
        };
        let cleanup =
            node.net.as_ref().unwrap().chainlock_cleanup().expect("with masternode services");
        let sync = node.rpc_sync.mn_sync().expect("constructed at step 7");
        after_the_running_task(&cl);
        assert!(!sync.is_blockchain_synced());
        assert_eq!(cleanup.next_cleanup_micros(), 0, "no cleanup before the blockchain is synced");
        sync.switch_to_next_asset();
        assert!(sync.is_blockchain_synced());
        let deadline = Instant::now() + Duration::from_secs(30);
        while cleanup.next_cleanup_micros() == 0 {
            assert!(Instant::now() < deadline, "the task ran no cleanup");
            std::thread::sleep(Duration::from_millis(50));
        }
        node.shutdown();
        assert!(!cl.are_threads_servicing_queue(), "stopped and joined");
    }

    /// BL-632: the ChainLock handler's `UpdatedBlockTip` posts its `Cleanup` to `cl-schdlr` for a
    /// tip that brings new blocks outside IBD, one post until it runs; the post runs the cleanup
    /// with the sync state.
    // Core: chainlock/handler.cpp:185-206, :334-343
    #[test]
    fn a_new_tip_posts_one_chainlock_cleanup_until_it_runs() {
        struct NoNotices;
        impl dashd_masternode::sync::NodeSyncNotifier for NoNotices {
            fn sync_reset(&self) {}

            fn sync_finished(&self) {}
        }
        let clock = Arc::new(MockClock::at(1_700_000_000));
        let seen =
            dashd_p2p::processing::seams::SeenChainLocks::new(|| true, Arc::clone(&clock) as _);
        let sync = Arc::new(MasternodeSync::new(clock, Box::new(NoNotices)));
        // No thread: a post waits in the queue until `service_queue` runs the queue here.
        let cl = Scheduler::new();
        cl.stop_when_drained();
        let listener = ClTipCleanup::new(cl.clone(), seen.cleanup(), Arc::clone(&sync));
        let posted = || cl.queue_info().0;
        let tip = |new, fork: Option<u32>, ibd| {
            listener.updated_block_tip(BlockId(new), fork.map(BlockId), ibd);
        };

        tip(1, Some(1), false);
        assert_eq!(posted(), 0, "blocks disconnected, none connected");
        tip(2, Some(1), true);
        assert_eq!(posted(), 0, "in IBD");
        tip(2, Some(1), false);
        assert_eq!(posted(), 1, "a new tip");
        let (_, due) = cl.queue_info();
        assert!(due.unwrap().0 <= Instant::now(), "posted at 0 s");
        tip(3, Some(2), false);
        tip(4, None, false);
        assert_eq!(posted(), 1, "one post until it runs");

        assert!(!sync.is_blockchain_synced());
        cl.service_queue();
        assert_eq!(posted(), 0);
        assert_eq!(seen.cleanup().next_cleanup_micros(), 0, "held while mn_sync is at BLOCKCHAIN");

        sync.switch_to_next_asset();
        assert!(sync.is_blockchain_synced());
        tip(5, Some(4), false);
        assert_eq!(posted(), 1, "the run lets the next tip post");
        cl.service_queue();
        assert_eq!(
            seen.cleanup().next_cleanup_micros(),
            1_700_000_030_000_000,
            "the cleanup ran at the mock time, throttled for 30 s"
        );
    }

    /// BL-632: step 10a registers the ChainLock handler's tip listener on V's signals, so a block
    /// posts its `Cleanup` to `cl-schdlr`; shutdown unregisters it.
    // Core: init.cpp:346, :2153; chainlock/handler.cpp:185-206
    #[test]
    fn a_block_posts_the_chainlock_cleanup_to_cl_schdlr() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, _) = start(dir.path(), &["-mocktime=1700000000"]);
        assert!(node.app_init_early() && node.app_init_main());
        let cl = node.cl_scheduler.clone().expect("made before step 7c");
        // `cl-schdlr` held, so a post stays in its queue beside the 5 s task's next run.
        let (held_tx, held) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        cl.schedule_from_now(
            move || {
                held_tx.send(()).unwrap();
                let _ = gate.recv();
            },
            Duration::ZERO,
        );
        held.recv_timeout(Duration::from_secs(30)).expect("cl-schdlr runs");
        let before = cl.queue_info().0;

        // A block connected while the import's `fImporting` is set is still initial block
        // download, which posts nothing (BL-616's join, as the dbcrash tests wait).
        assert!(node.steps.join_import(), "loadblk ends");
        let chain = node.chain().unwrap();
        assert!(!chain.handle().view_handle().loading_blocks(), "the import ended");
        // Not BL-175's own block: its test counts the `UpdateTip` lines for its hashes.
        let block = dbcrash::blocks(1).remove(0);
        let r = chain.handle().process_block(block, true, None, true).recv();
        assert!(r.as_ref().is_ok_and(|r| r.accepted && r.state.is_valid()), "{r:?}");
        let signals = chain.signals().clone();
        // The tip's queued signals may still be on their way after the reply.
        let deadline = Instant::now() + Duration::from_secs(30);
        while cl.queue_info().0 == before {
            assert!(Instant::now() < deadline, "the block posted nothing");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(cl.queue_info().0, before + 1, "the block's one post");
        // First in the queue, ahead of the 5 s task's next run: due at once.
        let (_, due) = cl.queue_info();
        assert!(due.unwrap().0 <= Instant::now(), "posted at 0 s");

        let listener = Arc::downgrade(&node.cl_tip.as_ref().expect("registered at step 10a").1);
        release.send(()).unwrap();
        node.shutdown();
        assert!(listener.upgrade().is_none(), "unregistered at shutdown");
        drop(signals);
    }

    /// BL-175's regtest blocks (`tests/data/bl175/blocks.txt`): coinbase only, mined at
    /// `-mocktime=1700000000`.
    fn bl175_blocks() -> impl Iterator<Item = dash_consensus::Block> {
        include_str!("../tests/data/bl175/blocks.txt").lines().map(|hex| {
            let raw: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            dash_consensus::decode_block(&mut &raw[..]).unwrap().into_value()
        })
    }

    /// `-dbcrashratio` on the in-process node (CH3b b4b): the ratio reaches both rolls and the
    /// flush policy, and each roll's crash runs the node's exit, which these tests replace.
    mod dbcrash {
        use std::thread::ThreadId;

        use dash_consensus::coins::stats::CoinStatsHashType;

        use super::*;

        const MOCKTIME: &str = "-mocktime=1700000000";

        /// BL-175's first `n` blocks, each a second later and ground again: the same chain under
        /// hashes of its own, as BL-175's test picks its `UpdateTip` lines by hash from a capture
        /// every test of this binary logs to.
        pub(super) fn blocks(n: usize) -> Vec<Arc<dash_consensus::Block>> {
            use dash_consensus::header::HeaderParams;
            use dash_consensus::pow::check_proof_of_work;
            use dash_consensus::primitives::Header;
            use dash_num::CompactTarget;
            use dash_params::{ChainParamsBuilder, Network, Release};

            let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
            let pow = *HeaderParams::from_chain_params(&p).unwrap().pow();
            let mut prev = None;
            bl175_blocks()
                .take(n)
                .map(|block| {
                    let mut raw = *block.header().raw();
                    raw.set_time(raw.time() + 1);
                    raw = dash_primitives::BlockHeader::new(
                        raw.version(),
                        prev.unwrap_or(raw.prev_hash()),
                        raw.merkle_root(),
                        raw.time(),
                        raw.bits(),
                        raw.nonce(),
                    );
                    let header = (0..)
                        .map(|nonce| {
                            Header::new(dash_primitives::BlockHeader::new(
                                raw.version(),
                                raw.prev_hash(),
                                raw.merkle_root(),
                                raw.time(),
                                raw.bits(),
                                nonce,
                            ))
                        })
                        .find(|h| {
                            check_proof_of_work(
                                h.hash().as_bytes(),
                                CompactTarget::new(raw.bits()),
                                &pow,
                            )
                        })
                        .unwrap();
                    prev = Some(header.hash());
                    Arc::new(dash_consensus::Block::new(header, block.txs().to_vec()))
                })
                .collect()
        }

        /// The threads the node's crash exit ran on.
        fn recorder(node: &mut Node) -> Arc<Mutex<Vec<ThreadId>>> {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let record = Arc::clone(&calls);
            node.set_crash_exit(Arc::new(move || {
                record.lock().unwrap().push(std::thread::current().id());
            }));
            calls
        }

        fn submit(node: &Node, block: &Arc<dash_consensus::Block>) {
            let chain = node.chain().unwrap().handle();
            let r = chain.process_block(Arc::clone(block), true, None, true).recv();
            assert!(r.as_ref().is_ok_and(|r| r.accepted && r.state.is_valid()), "{r:?}");
        }

        /// U5: a full-width ratio (`-1`, so `u64::MAX`) reaches recovery's roll and W's, and makes
        /// the policy handed to the flush side synchronous (D14); 0 reaches them as 0 and leaves
        /// it asynchronous. Each is read from the object handed over.
        #[test]
        fn a_full_width_ratio_reaches_both_rolls() {
            for (arg, ratio, synchronous) in
                [("-dbcrashratio=-1", u64::MAX, true), ("-dbcrashratio=0", 0, false)]
            {
                let dir = tempfile::tempdir().unwrap();
                let (mut node, _, _) = start(dir.path(), &[arg]);
                assert!(node.app_init_early() && node.app_init_main(), "{arg}");
                let record = node.steps.crash_record().expect("loaded");
                let recovery = record.recovery.as_ref().expect("recovery's roll");
                let (w, policy) = record.attached.get().expect("attached");
                assert_eq!((recovery.ratio(), w.ratio(), *policy), (ratio, ratio, synchronous));
                node.shutdown();
            }
        }

        /// U3: under a ratio, V waits for a freeze that is not ALWAYS: the flush on leaving IBD
        /// has written its coins when the block's reply arrives. `-1` never comes up here.
        #[test]
        fn a_crash_ratio_makes_every_freeze_synchronous() {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, _) =
                start(dir.path(), &["-dbcrashratio=-1", "-debug=bench", MOCKTIME]);
            let calls = recorder(&mut node);
            assert!(node.app_init_early() && node.app_init_main());
            let written = |log: &Lines| {
                log.lock().unwrap().iter().any(|l| {
                    l.contains("FlushStateToDisk: write coins cache to disk")
                        && l.contains("completed")
                })
            };
            assert!(!written(&log));
            // Init's wait ends before `loadblk` clears `fImporting`, and a block connected while
            // it is set is still initial block download: no LEFT_IBD flush then. The join, not
            // the flag, which reads clear until V serves the import's `SetLoading(true)` (BL-616).
            assert!(node.steps.join_import(), "loadblk ends");
            assert!(
                !node.chain().unwrap().handle().view_handle().loading_blocks(),
                "the import ended"
            );
            submit(&node, &blocks(1)[0]);
            assert!(written(&log), "the LEFT_IBD flush, waited for");
            assert!(calls.lock().unwrap().is_empty());
            node.shutdown();
        }

        /// U4: ratio 1 crashes the copy point's flush at its first threshold sub-batch, on W; the
        /// restart under ratio 1 crashes in recovery's replay, on init's thread, and init fails
        /// with status 0; a restart under 0 replays to the crashed flush's tip.
        #[test]
        fn a_recovery_roll_crashes_as_core() {
            let dir = tempfile::tempdir().unwrap();
            let blocks = blocks(100);
            let tip = blocks.last().unwrap().hash();

            let (mut node, log, _) = start(dir.path(), &["-dbcrashratio=1", "-dbbatchsize=300"]);
            let calls = recorder(&mut node);
            assert!(node.app_init_early() && node.app_init_main());
            for block in &blocks {
                submit(&node, block);
            }
            assert!(calls.lock().unwrap().is_empty(), "no flush yet");
            let chain = node.chain().unwrap().handle().clone();
            let _ = chain.coins_stats(CoinStatsHashType::None, Arc::new(|| false), None).recv();
            let deadline = Instant::now() + Duration::from_secs(120);
            while calls.lock().unwrap().is_empty() {
                assert!(Instant::now() < deadline, "no crash");
                std::thread::sleep(Duration::from_millis(10));
            }
            let w = calls.lock().unwrap().clone();
            assert_eq!(w.len(), 1, "once");
            assert_ne!(w[0], std::thread::current().id(), "on W");
            assert!(has(&log, "Simulating a crash. Goodbye."));
            drop(node);

            let (mut node, log, stderr) =
                start(dir.path(), &["-dbcrashratio=1", "-dbbatchsize=300"]);
            let calls = recorder(&mut node);
            assert!(node.app_init_early());
            assert!(!node.app_init_main());
            assert_eq!(*calls.lock().unwrap(), [std::thread::current().id()], "on init");
            assert!(
                position(&log, "Replaying blocks") < position(&log, "Simulating a crash. Goodbye.")
            );
            assert_eq!(node.exit_status(), 0);
            assert!(stderr.lock().unwrap().is_empty());
            drop(node);

            let (mut node, log, _) = start(dir.path(), &["-dbbatchsize=300"]);
            assert!(node.app_init_early() && node.app_init_main());
            assert!(has(&log, "Replaying blocks"));
            assert_eq!(node.tip(), Some(tip));
            node.shutdown();
        }
    }

    /// BL-175's Core differential. Linux only: its record reads `/proc`.
    #[cfg(target_os = "linux")]
    mod bl175 {
        use super::*;

        /// Core's capture's clock: `capture.py` ran dashd with this `-mocktime`.
        const MOCKTIME: i64 = 1_700_000_000;

        /// `UpdateTip` lines V logged, from every test of this binary: V logs on its own thread, so
        /// only the global subscriber hears it (a [`crate::test_log::tap`]). A test picks its
        /// blocks' lines by hash.
        static UPDATE_TIP: Mutex<Vec<String>> = Mutex::new(Vec::new());

        struct UpdateTipLog;

        impl tracing::Subscriber for UpdateTipLog {
            fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
                meta.target().starts_with("dashd_chainstate")
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                struct Message(String);
                impl tracing::field::Visit for Message {
                    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                        if f.name() == "message" {
                            self.0 = format!("{v:?}");
                        }
                    }
                }
                let mut m = Message(String::new());
                event.record(&mut m);
                if m.0.starts_with("UpdateTip: ") {
                    UPDATE_TIP.lock().unwrap().push(m.0);
                }
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        /// Core's capture for BL-175 (`contrib/node/bl175-alert-captures`): a regtest dashd
        /// v24.0.0-rc.3 mined 431 blocks, bit 22 LOCKED_IN from tip 287 and ACTIVE at 431, bit 23 one
        /// signal short.
        const BL175_CAPTURE: &str = include_str!("../tests/data/bl175/capture.txt");
        const BL175_RECORD_SH: &str = include_str!("../tests/data/bl175/record.sh");

        /// The capture's records of `kind`, the rest of each line.
        fn bl175(kind: &str) -> Vec<&'static str> {
            BL175_CAPTURE.lines().filter_map(|l| l.strip_prefix(kind)?.strip_prefix(' ')).collect()
        }

        /// `UpdateTip`'s ` warning='...'` suffix, or nothing.
        fn warning_suffix(line: &str) -> &str {
            line.find(" warning='").map_or("", |at| &line[at..])
        }

        /// BL-175 (DEC-284): Core's blocks replayed through V with Core's `-alertnotify` give
        /// dashd's text. The misc warning `GetWarnings` reports equals the `warnings` field of
        /// dashd's `getblockchaininfo`, `getnetworkinfo` and `getmininginfo`; `-alertnotify` runs
        /// once, with the same `sh -c` command line and arguments; and each `UpdateTip` line carries
        /// dashd's ` warning='...'` suffix, or none where dashd's has none.
        // Core: validation.cpp:1995-2013, :3150-3158, :3218-3236; warnings.cpp:24-65
        #[test]
        fn unknown_rules_warn_and_alert_as_dashd_v24_does() {
            static TAP: std::sync::OnceLock<crate::test_log::Tap> = std::sync::OnceLock::new();
            TAP.get_or_init(|| crate::test_log::tap(UpdateTipLog));
            assert!(
                BL175_CAPTURE.contains(&format!(", -mocktime={MOCKTIME}, ")),
                "the capture's clock"
            );
            let dir = tempfile::tempdir().unwrap();
            let d = dir.path().display().to_string();
            std::fs::write(dir.path().join("record.sh"), BL175_RECORD_SH).unwrap();
            let [template] = bl175("template")[..] else {
                panic!("one template")
            };
            let alertnotify = format!("-alertnotify={}", template.replace("<dir>", &d));
            let mocktime = format!("-mocktime={MOCKTIME}");
            let (mut node, _, _) = start(dir.path(), &[&mocktime, &alertnotify]);
            assert!(node.app_init_early() && node.app_init_main());
            let chain = node.steps.chain().unwrap().handle();
            for (h, block) in (1..).zip(bl175_blocks()) {
                let r = chain.process_block(Arc::new(block), true, None, true).recv();
                assert!(r.as_ref().is_ok_and(|r| r.accepted && r.state.is_valid()), "{h}: {r:?}");
            }
            let tips: Vec<(&str, &str)> =
                bl175("tip").into_iter().map(|t| t.split_once(' ').unwrap()).collect();
            assert_eq!(tips.len(), 432, "genesis, then 431 blocks");
            let (_, last) = tips[431];
            assert_eq!(chain.view().snap.tip.tip_hash.to_string(), &last[..64], "dashd's tip");

            // The detached command: one run, as dashd's.
            // glibc 2.38 and later run `sh -c -- <cmd>` (BZ #28519): the `--` is the host's, not
            // the node's.
            let host = |l: &&str| *l != "sh_argv=--";
            let expected: Vec<&str> = bl175("alert").into_iter().filter(host).collect();
            let out = dir.path().join("alert.txt");
            let read = || -> Vec<String> {
                let text = std::fs::read_to_string(&out).unwrap_or_default();
                text.replace(&d, "<dir>").lines().filter(host).map(str::to_owned).collect()
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while read().len() < expected.len() {
                assert!(std::time::Instant::now() < deadline, "alertnotify wrote {:?}", read());
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            assert_eq!(read(), expected, "dashd's alertnotify run");

            for w in bl175("warnings") {
                let (rpc, text) = w.split_once(' ').unwrap();
                assert_eq!(node.warnings.get_warnings(false), text, "{rpc}'s warnings");
            }
            assert!(bl175("run_command_error").is_empty());

            let logged = UPDATE_TIP.lock().unwrap().clone();
            for (h, line) in &tips[1..] {
                let (hash, core) = (&line[..64], &line[64..]);
                let ours: Vec<&String> = logged
                    .iter()
                    .filter(|l| l.starts_with(&format!("UpdateTip: new best={hash} ")))
                    .collect();
                assert_eq!(ours.len(), 1, "height {h}: {ours:?}");
                assert!(ours[0].contains(&format!(" height={h} ")), "{}", ours[0]);
                assert_eq!(warning_suffix(ours[0]), core, "height {h}");
            }
            assert_eq!(read(), expected, "still one alertnotify run");
            node.shutdown();
        }
    }

    /// SPV mode's init and shutdown around a fake host (SB18 T-SHUT).
    #[cfg(unix)]
    mod spv {
        use std::sync::atomic::AtomicBool;
        use std::thread;

        use super::*;
        use crate::spv_mode::{SPV_DIR, SpvHost, SpvStart};

        /// What the node still held when it shut the host down.
        #[derive(Debug, Default, PartialEq, Eq)]
        struct Held {
            rpc_running: bool,
            scheduler: bool,
            pid_file: bool,
            datadir_lock: bool,
        }

        /// What the fake host's start does after recording the call.
        type StartHook = Box<dyn FnMut(&SpvStart<'_>) -> Result<(), InitFailure> + Send>;

        /// A host that records what the node calls and what the node held at its shutdown.
        struct Fake {
            net: PathBuf,
            start: StartHook,
            flush: Result<(), String>,
            calls: Lines,
            held: Arc<Mutex<Option<Held>>>,
            rpc_stopped: Arc<AtomicBool>,
            table: Arc<Mutex<Option<Arc<dashd_rpc::server::RpcServer>>>>,
            started: Option<(Scheduler, Arc<Logger>)>,
        }

        /// The fake host's records, kept by the test.
        struct Probe {
            calls: Lines,
            held: Arc<Mutex<Option<Held>>>,
            table: Arc<Mutex<Option<Arc<dashd_rpc::server::RpcServer>>>>,
        }

        impl Fake {
            fn new(dir: &Path) -> Fake {
                Fake {
                    net: dir.join("regtest"),
                    start: Box::new(|_| Ok(())),
                    flush: Ok(()),
                    calls: Lines::default(),
                    held: Arc::default(),
                    rpc_stopped: Arc::default(),
                    table: Arc::default(),
                    started: None,
                }
            }

            fn probe(&self) -> Probe {
                Probe {
                    calls: Arc::clone(&self.calls),
                    held: Arc::clone(&self.held),
                    table: Arc::clone(&self.table),
                }
            }

            fn call(&self, what: &str) {
                self.calls.lock().unwrap().push(what.to_owned());
            }
        }

        impl SpvHost for Fake {
            fn start(&mut self, ctx: SpvStart<'_>) -> Result<(), InitFailure> {
                assert_eq!(ctx.spv_dir, self.net.join(SPV_DIR));
                let warmup = self.table.lock().unwrap().as_ref().and_then(|t| t.warmup_status());
                self.call(if warmup.is_some() {
                    "start in warmup"
                } else {
                    "start"
                });
                self.started = Some((ctx.scheduler.clone(), Arc::clone(ctx.ui.logger())));
                (self.start)(&ctx)
            }

            fn register_rpc(&self, table: &Arc<dashd_rpc::server::RpcServer>) {
                self.call("register_rpc");
                let stopped = Arc::clone(&self.rpc_stopped);
                table.on_stopped(move || stopped.store(true, Ordering::SeqCst));
                *self.table.lock().unwrap() = Some(Arc::clone(table));
            }

            fn interrupt(&self) {
                self.call("interrupt");
            }

            fn shutdown(&mut self) -> Result<(), String> {
                self.call("shutdown");
                let Some((scheduler, logger)) = self.started.as_ref() else {
                    return self.flush.clone();
                };
                logger.log_printf("fake host: shut down\n");
                *self.held.lock().unwrap() = Some(Held {
                    rpc_running: !self.rpc_stopped.load(Ordering::SeqCst),
                    scheduler: scheduler.are_threads_servicing_queue(),
                    pid_file: self.net.join(datadir::PID_FILENAME).exists(),
                    datadir_lock: crate::datadir::tests::another_process_is_refused(&self.net),
                });
                self.flush.clone()
            }
        }

        /// A regtest node with the RPC server on a free port.
        fn spv_node(dir: &Path) -> (Node, Lines, Lines) {
            let port = format!("-rpcport={}", node_test_ports::free_port());
            start(dir, &["-server=1", &port])
        }

        /// The node serves SPV mode's table, then stops the RPC server, then the host, then the
        /// scheduler, the pid file and the datadir lock, last. It opens no chainstate.
        ///
        /// The tail's order is seen from a scheduler task that is still running when the
        /// scheduler stops, which checks the pid file 200 ms after the host's shutdown, and at
        /// `Shutdown: done`, which comes between the pid file and the lock (as
        /// `the_lock_outlives_the_pid_file` checks full mode).
        #[test]
        fn spv_shutdown_stops_the_rpc_server_then_the_host_then_the_rest() {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr) = spv_node(dir.path());
            let net = dir.path().join("regtest");
            let mut fake = Fake::new(dir.path());
            let probe = fake.probe();
            // The pid file as the last scheduler task saw it, while the scheduler stops.
            let pid_at_stop = Arc::new(Mutex::new(None));
            let task_running = Arc::new(AtomicBool::new(false));
            let (held, at, sink, running) = (
                Arc::clone(&probe.held),
                net.join(datadir::PID_FILENAME),
                Arc::clone(&pid_at_stop),
                Arc::clone(&task_running),
            );
            fake.start = Box::new(move |ctx| {
                let (held, at, sink, running) =
                    (Arc::clone(&held), at.clone(), Arc::clone(&sink), Arc::clone(&running));
                let task = move || {
                    running.store(true, Ordering::SeqCst);
                    let deadline = Instant::now() + Duration::from_secs(30);
                    while held.lock().unwrap().is_none() && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(1));
                    }
                    thread::sleep(Duration::from_millis(200));
                    *sink.lock().unwrap() = Some(at.exists());
                };
                ctx.scheduler.schedule_from_now(task, Duration::ZERO);
                Ok(())
            });
            assert!(node.app_init_early());
            assert!(node.app_init_main_spv(Box::new(fake)));
            assert!(net.join(datadir::PID_FILENAME).exists());
            assert!(!net.join(crate::chainstate::CHAINSTATE_DIR).exists());
            assert!(has(&log, "init message: Done loading"));
            let scheduler = node.scheduler.clone().expect("started at step 4a");
            let table = probe.table.lock().unwrap().clone().expect("registered");
            let spv = rpc::spv_command_table(
                &node.params.chain,
                node.ui.logger(),
                &node.shutdown,
                &node.clock,
                (true, &scheduler),
            );
            assert_eq!(table.list_commands(), spv.list_commands());
            assert!(!table.list_commands().iter().any(|c| c == "getblockcount"));
            let deadline = Instant::now() + Duration::from_secs(30);
            while !task_running.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(task_running.load(Ordering::SeqCst), "the scheduler task never ran");
            // (scheduler running, pid file, lock held) at "Shutdown: done".
            let at_done = Arc::new(Mutex::new(None));
            let (sink, at, sched) = (Arc::clone(&at_done), net.clone(), scheduler.clone());
            node.ui.logger().push_back_callback(move |s| {
                if s.contains("Shutdown: done") {
                    let refused = crate::datadir::tests::another_process_is_refused(&at);
                    let pid_file = at.join(datadir::PID_FILENAME).exists();
                    *sink.lock().unwrap() =
                        Some((sched.are_threads_servicing_queue(), pid_file, refused));
                }
            });

            node.interrupt();
            node.shutdown();
            assert_eq!(
                *pid_at_stop.lock().unwrap(),
                Some(true),
                "pid file while the scheduler stops"
            );
            let done = Some((false, false, true));
            assert_eq!(*at_done.lock().unwrap(), done, "(scheduler, pid file, lock held)");
            let calls = probe.calls.lock().unwrap().clone();
            assert_eq!(calls, ["register_rpc", "start in warmup", "interrupt", "shutdown"]);
            let held = Held {
                rpc_running: false,
                scheduler: true,
                pid_file: true,
                datadir_lock: true,
            };
            assert_eq!(*probe.held.lock().unwrap(), Some(held));
            assert!(!scheduler.are_threads_servicing_queue());
            assert!(!net.join(datadir::PID_FILENAME).exists());
            assert!(!crate::datadir::tests::another_process_is_refused(&net));
            let shut = position(&log, "fake host: shut down");
            assert!(position(&log, "PrepareShutdown: In progress...") < shut);
            assert!(shut < position(&log, "Shutdown: done"));
            assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
            assert_eq!(node.exit_status(), 0);
        }

        /// A failed host shutdown is the SPV store's failed flush: an `AbortNode`, exit status
        /// 1, and the rest of the shutdown still runs.
        #[test]
        fn a_failed_spv_flush_aborts_the_node() {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr) = start(dir.path(), &[]);
            let mut fake = Fake::new(dir.path());
            fake.flush = Err("injected sync failure".into());
            assert!(node.app_init_early() && node.app_init_main_spv(Box::new(fake)));
            node.interrupt();
            node.shutdown();
            assert_eq!(node.exit_status(), EXIT_FAILURE);
            let log_msg = "Failed to flush the SPV store: injected sync failure";
            let report = crate::abort::Report {
                log: log_msg.to_owned(),
                user: None,
            };
            assert_eq!((node.abort.reports(), node.abort.report(0)), (1, Some(report)));
            let error = format!("Error: {DEFAULT_ABORT_USER_MESSAGE}\n");
            assert_eq!(*stderr.lock().unwrap(), [error]);
            assert!(position(&log, &format!("*** {log_msg}")) < position(&log, "Shutdown: done"));
            let net = dir.path().join("regtest");
            assert!(!net.join(datadir::PID_FILENAME).exists());
            assert!(!crate::datadir::tests::another_process_is_refused(&net));
        }

        /// A host's start error is init's error, and shutdown still shuts the host down.
        #[test]
        fn a_failed_spv_start_fails_init_and_still_shuts_the_host_down() {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr) = start(dir.path(), &[]);
            let mut fake = Fake::new(dir.path());
            fake.start = Box::new(|_| Err(InitFailure::Error("the SPV store is gone".into())));
            let probe = fake.probe();
            assert!(node.app_init_early());
            assert!(!node.app_init_main_spv(Box::new(fake)));
            assert_eq!(*stderr.lock().unwrap(), ["Error: the SPV store is gone\n"]);
            assert!(!has(&log, "init message: Done loading"));
            node.interrupt();
            node.shutdown();
            assert_eq!(*probe.calls.lock().unwrap(), ["start", "interrupt", "shutdown"]);
            assert_eq!(node.exit_status(), EXIT_FAILURE);
            assert!(has(&log, "Shutdown: done"));
        }

        /// A shutdown requested before step 12 ends init there: the host never starts, init
        /// fails with exit status 1, and shutdown still shuts the host down.
        // Core: init.cpp:2570-2573
        #[test]
        fn a_shutdown_requested_before_step_12_skips_the_host_start() {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, log, stderr) = start(dir.path(), &[]);
            let fake = Fake::new(dir.path());
            let probe = fake.probe();
            assert!(node.app_init_early());
            node.shutdown.start_shutdown();
            assert!(!node.app_init_main_spv(Box::new(fake)));
            assert!(has(&log, "Shutdown requested. Exiting."));
            assert!(!has(&log, "init message: Done loading"));
            assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
            node.interrupt();
            node.shutdown();
            assert_eq!(*probe.calls.lock().unwrap(), ["interrupt", "shutdown"]);
            assert_eq!(node.exit_status(), EXIT_FAILURE);
            assert!(has(&log, "Shutdown: done"));
        }

        /// An `AbortNode` while the host starts requests the shutdown and makes the exit status
        /// 1, even when the start itself returns, and shutdown still shuts the host down.
        #[test]
        fn an_abort_during_spv_start_exits_1() {
            let dir = tempfile::tempdir().unwrap();
            let (mut node, _log, _stderr) = start(dir.path(), &[]);
            let mut fake = Fake::new(dir.path());
            fake.start = Box::new(|ctx| {
                ctx.abort.abort_node("SPV client stopped: injected", None);
                Ok(())
            });
            let probe = fake.probe();
            assert!(node.app_init_early());
            // Init itself completes, as Core's step 13 does after an `AbortNode` in `StartNode`;
            // the abort shows in the exit status.
            assert!(node.app_init_main_spv(Box::new(fake)));
            assert!(node.shutdown.shutdown_requested());
            node.interrupt();
            node.shutdown();
            assert_eq!(*probe.calls.lock().unwrap(), ["start", "interrupt", "shutdown"]);
            assert_eq!(node.exit_status(), EXIT_FAILURE);
        }
    }
}
