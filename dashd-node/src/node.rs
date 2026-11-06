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
use dash_params::ChainSetup;
use dashd_chainstate::caches::CacheSizes;
#[cfg(feature = "test-hooks")]
use dashd_chainstate::chain::{BlockTailStage, StagedRecord};
use dashd_chainstate::chain::{ValidationInterface, ValidationSignals};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::sigcache::ScriptCaches;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::sync::MasternodeSync;
use dashd_mining::ChainTemplateSource;
use dashd_p2p::processing::seams::SeenChainLocksCleanup;
use dashd_store::flatfile::{FlatFs as _, MIN_DISK_SPACE, OsFs};

use crate::abort::NodeAbort;
use crate::chain::Chain;
use crate::chainstate::{
    CoinsSetup, CrashExit, OpenError, Opening, StorageProfile, chainstate_profile,
    reindex_question, storage_request,
};
use crate::datadir::{self, DirLock, PidFile};
use crate::init::caches::{
    cache_sizes, crash_ratio, db_batch_size, init_validation_caches, max_upload_target,
    start_script_check_threads,
};
use crate::init::mempool::{MemPoolOptions, apply_args_man_options};
use crate::init::{self, ParamInteraction, SelectedParams};
use crate::logging::{self, Logger, set_thread_name};
use crate::mnsync::SyncParts;
use crate::scheduler::Scheduler;
use crate::shutdown::{EXIT_FAILURE, ShutdownState};
use crate::steps::ChainSteps;
use crate::ui::Ui;
use crate::warnings::Warnings;
use crate::{InitFailure, host, notify, p2p, rpc};
use dashd_chainstate::tuning::{EngineTuning, RocksTuning};

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
    /// `-storageprofile` (BL-634), read with `unsupported`: the request, or the init error to
    /// report in rdashd's own refusal slot.
    storage_request: Result<Option<StorageProfile>, String>,
    interaction: Option<ParamInteraction>,
    /// V, the coins and the spork manager (steps 7a to 11 and their shutdown).
    steps: ChainSteps,
    /// The network, from step 7c (P3 S6a).
    net: Option<p2p::Net>,
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
}

/// A test's step inside the chainstate load.
#[cfg(test)]
type LoadHook = Box<dyn Fn(&MockClock)>;

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
            storage_request: Ok(None),
            interaction: None,
            steps,
            net: None,
            scheduler: None,
            cl_scheduler: None,
            cl_tip: None,
            rpc: None,
            rpc_chain,
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
        self.unsupported = init::unsupported_option(args)?;
        self.storage_request = storage_request(args, &self.params.chain)?;
        // -server defaults to true for dashd.
        args.soft_set_bool_arg("-server", true);
        // InitLogging: early, so parameter interaction reaches the console.
        logging::set_logging_options(logger, args)?;
        logging::log_package_version(logger);
        init::init_parameter_interaction(args, logger)?;
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

    /// `AppInit` after the daemon fork: the data directory lock and `AppInitMain`.
    // Core: bitcoind.cpp:225-233
    pub fn app_init_main(&mut self) -> bool {
        let r = self.try_app_init_main();
        r.map_or_else(|f| self.fail(f), |()| true)
    }

    fn try_app_init_main(&mut self) -> Result<(), InitFailure> {
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
        let caches = Arc::new(init_validation_caches(logger, args)?);
        let queue = Arc::new(start_script_check_threads(logger, args)?);

        let blocks_dir = args.get_blocks_dir_path()?;
        let scheduler = self.scheduler.insert(Scheduler::new());
        scheduler.start().map_err(|e| InitFailure::Exception(e.to_string()))?;
        // Gather some entropy once per minute.
        // Core: init.cpp:1578-1581
        scheduler.schedule_every(dashd_random::rand_add_periodic, Duration::from_secs(60));
        schedule_disk_space_check(scheduler, &self.ui, &self.shutdown, blocks_dir.clone());

        // The RPC server starts in warmup: requests are answered with the init message until
        // step 13.
        // Core: init.cpp:1602-1638
        if args.get_bool_arg("-server", false)? {
            let deprecated = rpc::DeprecatedRpc {
                banscore: dashd_rpc::server::is_deprecated_rpc_enabled(args, "banscore"),
                softforks: dashd_rpc::server::is_deprecated_rpc_enabled(args, "softforks"),
                service: dashd_rpc::server::is_deprecated_rpc_enabled(args, "service"),
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
        self.steps.make_net_caches(&self.params, Arc::clone(&self.clock) as _);
        net.check_options(args, logger)?;

        // Step 7a: load sporks.
        self.steps.load_sporks(args, &self.params)?;

        // Step 7: load the chainstate.
        // An `Err` returns in rdashd's refusal slot below, before the tuning opens anything.
        let requested = self.storage_request.as_ref().ok().copied().flatten();
        let (tuning, live_copy) = storage_profile(&self.params.chain, requested, logger);
        let (sizes, setup) = coins_configuration(
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
        let batch_size = db_batch_size(args)?;
        let crash_ratio = setup.crash_ratio;

        // rdashd: an option it does not support yet, after every argument check Core makes and
        // before anything is read from or written to the chainstate.
        if let Some(refusal) = self.unsupported.take() {
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
            let parts = SyncParts::new(
                args,
                &self.params.chain,
                Arc::clone(self.net.as_ref().expect("step 6 built it").core()),
                Arc::clone(&self.clock) as _,
                Arc::clone(self.steps.netfulfilled().expect("made at step 6")),
                flags.governance_enabled,
            )?;
            self.rpc_sync.attach(&parts.sync);
            sync = Some(parts);
            let started = Instant::now();
            let data_dir = args.get_data_dir_net()?;
            let chain = Arc::new(self.params.chain.clone());
            let clock = Arc::clone(&self.clock) as _;
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
                        hook(&self.clock);
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
                    // The node's one block assembler (MN0 r1), with `-blockversion` (MN0 D3).
                    let miner = ChainTemplateSource::new(
                        chain.handle().clone(),
                        Arc::new(self.params.chain.clone()),
                        Arc::clone(&self.clock) as _,
                        dashd_mining::block_version_arg(args, &self.params.chain)?,
                    );
                    let coins = self.steps.coins_cell().map(|c| rpc::coins_read(Arc::clone(c)));
                    self.rpc_chain.attach(
                        chain.handle(),
                        chain.signals(),
                        files,
                        Some(Arc::new(miner)),
                        coins,
                    );
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
                // declined.
                // Core: init.cpp:2119-2133, noui.cpp:22-54
                Err(OpenError::Recoverable {
                    message,
                    cause,
                }) => {
                    if let Some(cause) = cause {
                        logger.log_printf(&format!("{cause}\n"));
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

        // Step 7c: `PeerManager::make` and its registration on V's signals, and the
        // `CDSNotificationInterface` with step 7's `mn_sync`, after the load and before step
        // 11's ABC (P3 D112), then the fulfilled-request and masternode caches (SP1).
        // Core: init.cpp:2194-2199, :2225-2228, :2273-2289
        let sync = sync.expect("made before the load, which ran: no shutdown was requested");
        let files = self.steps.take_serve_files();
        let chain = self.steps.chain().expect("step 7 started V");
        let sporks = self.steps.sporks().expect("step 7a made sporkman");
        let net = self.net.as_mut().expect("step 6 built it");
        net.attach(chain, files, Arc::clone(&self.clock) as _, sporks, &sync);
        self.steps.load_net_caches(args, &self.params)?;

        // Step 10a: the ChainLock handler's 5 s task, then the Dash tasks on the scheduler.
        // Core: init.cpp:2366-2368, :2389-2396
        let chain = self.steps.chain().expect("step 7 started V");
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
        sync.schedule(scheduler, &self.shutdown);

        // Step 11: import blocks. `ThreadImport` sends V's startup ABC, then init waits for
        // genesis. Its thread then broadcasts the tip, as `InitializeCurrentBlockTip` to every
        // subscriber: a node that is not a masternode has no `active_ctx` (CH7 §6.3 step 6).
        // Core: init.cpp:2448-2468
        self.steps.import(args)?;
        // Core: init.cpp:2570-2573
        if self.shutdown.shutdown_requested() {
            logger.log_printf("Shutdown requested. Exiting.\n");
            return Err(InitFailure::Reported);
        }

        // Step 12: start node.
        // Core: init.cpp:2575-2768
        let net = self.net.as_mut().expect("step 6 built it");
        net.start(args, &self.ui, Arc::clone(&self.abort) as _)?;
        let (shared, connman) = (net.shared().expect("attached"), net.connman().expect("started"));
        self.rpc_net.attach(net.core(), shared, connman);

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
        // Core: init.cpp:289-295
        if let Some(net) = &self.net {
            net.interrupt();
        }
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
        if let Some(mut rpc) = self.rpc.take() {
            rpc.stop();
        }
        self.rpc_chain.detach();
        self.rpc_net.detach();
        self.rpc_sync.detach();
        // Core: init.cpp:335
        if let Some(cl) = self.cl_scheduler.take() {
            cl.stop();
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
        self.scheduler = None;
        if let Some(pid) = self.pid.take() {
            pid.remove();
        }
        logger.log_printf("Shutdown: done\n");
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
/// first-seen times of transactions, which rdashd does not keep. A V that has stopped ignores
/// the task.
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
        caches,
        queue,
        assume_valid: interaction.flags.chainman.assumed_valid(chain),
        check_block_index: interaction.flags.chainman.check_block_index(chain),
        xcheck,
        #[cfg(feature = "planted-bugs")]
        planted: None,
        #[cfg(feature = "test-support")]
        regime: Default::default(),
        #[cfg(feature = "test-support")]
        tally: None,
        #[cfg(feature = "test-support")]
        records: None,
        cjdns_reachable: crate::p2p::cjdns_reachable(args)?,
        metaman,
        crash_ratio: crash_ratio(args)?,
        crash_exit,
        #[cfg(test)]
        digest: Default::default(),
        #[cfg(test)]
        h11_pool_error_at: None,
        live_copy,
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
        INTERIM_REFUSAL, g2_refusal_text,
    };
    use dashd_chainstate::flush::Kept;
    use dashd_chainstate::store::sites::{
        BARRIER_SITE, BLOCK_FILES_WRITE_SITE, CRASH_SITE, FLUSH_SITE, RAW_WRITE_SITE, READ_SITE,
        UNWIND_SITE, V_PANIC_SITE, W_PANIC_SITE, WRITE_SITE,
    };

    use super::*;
    use crate::abort::{AbortSink as _, DEFAULT_ABORT_USER_MESSAGE};
    use crate::init::{Lines, setup_server_args};
    use crate::logging::Logger;
    use crate::steps::Point;

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

    #[test]
    fn unsupported_option_is_refused_after_core_s_checks() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &["-reindex"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        let refusal = position(&log, "-reindex is not supported by rdashd yet (CH5).");
        assert!(position(&log, "* Using") < refusal);
        assert_eq!(stderr.lock().unwrap().len(), 1);
        assert!(!has(&log, "Loading block index"));
        drop(node);
        assert!(!dir.path().join("regtest").join("dashd.pid").exists());

        // Core's own argument errors come first, even in step 7.
        let (mut node, _, stderr) = start(dir.path(), &["-reindex", "-maxmempool=1"]);
        assert!(node.app_init_early());
        assert!(!node.app_init_main());
        assert_eq!(*stderr.lock().unwrap(), ["Error: -maxmempool must be at least 5 MB\n"]);
    }

    /// Step 6 reads the masternode role (BL-703a): with a `-masternodeblsprivkey` (and the
    /// `-txindex=1` and `-blocksonly=0` Core's step 3 requires with it) the network core is an
    /// active masternode's before rdashd's step 7 refuses an option it does not support yet.
    #[test]
    fn step_6_reads_the_masternode_role_before_step_7_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["-masternodeblsprivkey=00", "-txindex=1", "-blocksonly=0"];
        let (mut node, _, stderr) = start(dir.path(), &argv);
        assert!(node.app_init_early(), "{:?}", stderr.lock().unwrap());
        assert!(!node.app_init_main());
        let stderr = stderr.lock().unwrap().clone();
        assert!(
            matches!(&stderr[..], [e] if e.contains("is not supported by rdashd yet")),
            "{stderr:?}"
        );
        let net = node.net.as_ref().expect("step 6 ran");
        assert!(net.role().active());
        assert!(net.core().config().active_masternode);
    }

    /// Starts a regtest node in `dir` whose `settings.json` holds `-masternodeblsprivkey` as
    /// `value` (`None`: no settings file), runs init and returns its stderr. The key's settings are
    /// set for step 3, so `argv` carries the `-txindex=1` and `-blocksonly=0` it asks for.
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
        let argv = ["-txindex=1", "-blocksonly=0", "-port=65536"];
        let port = ["Error: Invalid port specified in -port: '65536'\n"];
        assert_eq!(failed_init_with_key_setting(None, &argv), port);
        for value in ["[]", "{}"] {
            assert_eq!(failed_init_with_key_setting(Some(value), &argv), port, "{value}");
        }
    }

    /// With no earlier error, a non-string key setting still ends init at rdashd's step 7
    /// refusal, as a string key setting does: the refusal tests `IsArgSet`, not the value.
    #[test]
    fn a_non_string_key_setting_ends_init_at_step_7s_refusal() {
        let argv = ["-txindex=1", "-blocksonly=0"];
        let refusal = failed_init_with_key_setting(Some("\"00\""), &argv);
        assert!(
            matches!(&refusal[..], [e] if e.contains("is not supported by rdashd yet")),
            "{refusal:?}"
        );
        for value in ["[]", "{}"] {
            assert_eq!(failed_init_with_key_setting(Some(value), &argv), refusal, "{value}");
        }
    }

    /// Unserved `-spv` gives only a refusal: no soft-set, governance warning or `-spv` index
    /// refusal comes first (BL-679). With `-txindex=1` the refusal is `-txindex`'s, in help order.
    #[test]
    fn unserved_spv_gives_only_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        for (argv, refusal) in [
            (&["-spv"][..], "-spv is not supported by rdashd yet (SB18)."),
            (&["-spv", "-txindex=1"], "-txindex is not supported by rdashd yet (IX1)."),
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
        // The loaded chainstate's platform-ban applier holds step 6's manager, the one loaded
        // and stored here, not one of its own (BL-425; SP1c-1 r1 m-1).
        assert_eq!(Arc::strong_count(node.steps.metaman().unwrap()), 2, "steps and the applier");
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
        node.during_load = Some(Box::new(|clock| clock.set_mocktime(1_700_000_020)));
        assert!(node.app_init_early() && node.app_init_main());
        assert_eq!(node.clock.node_now().secs(), 1_700_000_020, "the load moved the clock");
        let sync = node.rpc_sync.mn_sync().expect("constructed at step 7");
        assert_eq!((sync.asset_id(), sync.asset_start_time()), (1, 1_700_000_000));
        node.shutdown();
        assert!(node.rpc_sync.mn_sync().is_none(), "shutdown resets mn_sync");
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
        let (mut node, _, _) = start(dir.path(), &[&startup, &shutdown, "-reindex"]);
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
    /// `StopNodes`, which writes `peers.dat` and, with no `-connect`, `anchors.dat`.
    // Core: init.cpp:347-348, net.cpp:4168-4210
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
                stopped: false
            }
        );
        let regtest = dir.path().join("regtest");
        assert!(!regtest.join("anchors.dat").exists(), "Start reads and removes it");
        net.stop();
        assert_eq!(
            net.parts(),
            p2p::Parts {
                attached: true,
                msghand: false,
                connman: false,
                registered: false,
                sync_registered: false,
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
        let (mut node, log, stderr) = start(dir, &[]);
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

    /// BL-407: after the D18 refusal through the connector's abort path, as `SerialCore::refuse`
    /// makes it (the refusal is the user's message too), both shutdown flushes report nothing, as
    /// Core's do on the healthy database such an `AbortNode` leaves. The refusal stays the
    /// shutdown's one `AbortNode`: its two lines once, its user error, its warning, exit 1.
    // Core: validation.cpp:3052-3121; init.cpp:379-386, :449-456; shutdown.cpp:27-38
    #[test]
    fn a_d18_refusal_leaves_both_shutdown_flushes_silent() {
        let dir = tempfile::tempdir().unwrap();
        let at = BlockAt {
            hash: BlockHash::from([0x18; 32]),
            height: 1,
            prev: None,
            id: None,
        };
        let (node, log, stderr, points, first) = reported_shutdown(dir.path(), None, |p| {
            let t = g2_refusal_text(p, &at, INTERIM_REFUSAL);
            (t.clone(), Some(t))
        });
        let t = first[0].strip_prefix("*** ").unwrap();
        assert_eq!(first[1], format!("Error: {t}"), "the refusal is the user's message");
        assert_silent(&node, (&log, &stderr, &points), &first, "D18");
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
        // rpc_generate.py's regtest address of the generator point's key.
        let params = format!(r#"[{n}, "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6"]"#);
        let mined = crate::chain::tests::rpc(&table, "generatetoaddress", &params).expect("mined");
        let mined = dashd_rpc::UniValue::read(mined.as_bytes()).expect("JSON");
        let mined = mined.get_values().expect("an array");
        mined.iter().map(|h| h.get_str().unwrap().to_owned()).collect()
    }

    /// DEC-995: a connector `AbortNode` lets no listener record a locator past the last durable
    /// chainstate flush. Core's first shutdown flush writes, then queues `ChainStateFlushed`; here
    /// nothing is written after the trip, so nothing may be signalled either. rdashd has no index
    /// or wallet that stores a locator, so a recording listener stands in for one. A first run
    /// stops cleanly at `d`, a second connects three blocks on it and aborts, and a third finds
    /// the chainstate at its last durable flush: no locator the second run signalled is past it.
    // Core: validation.cpp:3117-3121; init.cpp:379-390
    #[test]
    fn a_connector_abort_node_records_no_locator_past_the_last_durable_flush() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, ..) = settled(dir.path());
        let d = generate(&node, dir.path(), 2).pop().unwrap();
        node.shutdown();
        assert_eq!(node.exit_status(), 0);
        drop(node);

        let (mut node, ..) = settled(dir.path());
        assert_eq!(node.tip().map(|h| h.to_string()), Some(d.clone()), "durable at d");
        let locators = Arc::new(Locators::default());
        node.steps.chain().unwrap().signals().register(Arc::clone(&locators) as _);
        let mined = generate(&node, dir.path(), 3);
        node.steps.chainstate().abort_path().abort_node(CORRUPT_BLOCK, None);
        node.shutdown();
        assert_eq!(node.steps.flushes(), 2);
        assert_eq!(node.abort.reports(), 1);
        drop(node);

        let (mut node, ..) = settled(dir.path());
        let durable = node.tip().expect("a tip").to_string();
        node.shutdown();
        assert_ne!(durable, mined[2], "blocks connected after the last durable flush");
        let mut chain = std::iter::once(&d).chain(&mined);
        assert!(chain.any(|h| *h == durable), "the last durable flush is d or a block on it");
        let past: Vec<&String> = chain.collect();
        let recorded = locators.0.lock().unwrap();
        assert!(recorded.iter().all(|l| !past.contains(&l)), "{recorded:?} past {durable}");
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
        /// only a global subscriber hears it. A test picks its blocks' lines by hash.
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
            static INSTALL: std::sync::Once = std::sync::Once::new();
            INSTALL.call_once(|| {
                tracing::subscriber::set_global_default(UpdateTipLog).expect("the only one");
            });
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
}
