//! The node's lifetime in Core's order (`bitcoind.cpp` `AppInit`, `init.cpp`): init in two phases
//! around the daemon fork, then `Interrupt` and `Shutdown`, which run whether init succeeded or
//! not.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_clock::MockClock;
use dash_consensus::primitives::BlockHash;
use dash_params::ChainParams;
use dashd_chainstate::caches::CacheSizes;
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::sigcache::ScriptCaches;
use dashd_mining::ChainTemplateSource;
use dashd_store::flatfile::{FlatFs as _, MIN_DISK_SPACE, OsFs};

use crate::abort::NodeAbort;
use crate::chainstate::{CoinsSetup, OpenError, Opening, reindex_question};
use crate::datadir::{self, DirLock, PidFile};
use crate::init::caches::{
    cache_sizes, init_validation_caches, max_upload_target, start_script_check_threads,
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
    interaction: Option<ParamInteraction>,
    /// V, the coins and the spork manager (steps 7a to 11 and their shutdown).
    steps: ChainSteps,
    /// The network, from step 7c (P3 S6a).
    net: Option<p2p::Net>,
    scheduler: Option<Scheduler>,
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
            interaction: None,
            steps,
            net: None,
            scheduler: None,
            rpc: None,
            rpc_chain,
            rpc_net: Arc::default(),
            rpc_sync: Arc::default(),
            pid: None,
            lock: None,
            interrupted: AtomicBool::new(false),
            shut_down: false,
            #[cfg(test)]
            during_load: None,
        }
    }

    pub fn params(&self) -> &SelectedParams {
        &self.params
    }

    /// Shows `failure` as Core's `AppInit` does, and marks init failed: `false`.
    pub fn fail(&self, failure: InitFailure) -> bool {
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
        // AppInitSanityChecks: kernel::SanityChecks tests the C++ runtime (ECC, clock), which
        // has no Rust counterpart; then the lock probe. (`init::init_config` already refused a
        // Dash Core data directory.)
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
        schedule_disk_space_check(scheduler, &self.ui, &self.shutdown, blocks_dir.clone());

        // The RPC server starts in warmup: requests are answered with the init message until
        // step 13.
        // Core: init.cpp:1602-1638
        if args.get_bool_arg("-server", false)? {
            let deprecated = rpc::DeprecatedRpc {
                banscore: dashd_rpc::server::is_deprecated_rpc_enabled(args, "banscore"),
                softforks: dashd_rpc::server::is_deprecated_rpc_enabled(args, "softforks"),
            };
            let table = rpc::command_table(
                &self.params.chain,
                logger,
                &self.shutdown,
                &self.clock,
                (self.params.chain.is_mockable_chain, &*scheduler),
                &self.rpc_chain,
                &self.rpc_net,
                &self.rpc_sync,
                &self.warnings,
                deprecated,
            );
            self.rpc = Some(rpc::Servers::start(&self.ui, args, &table, &self.shutdown)?);
        }

        // Step 6: network initialization: the network core with `peers.dat` and the ban list
        // (P3 D113), `sporkman`, `mn_metaman` and `netfulfilledman` (SP1), then the port,
        // socket-address and `-uacomment` checks (BL-327 D2).
        // Core: init.cpp:1649-1890
        let net = &self.interaction.as_ref().expect("app_init_early succeeded").net;
        let net = self.net.insert(p2p::Net::new(
            args,
            &Arc::new(self.params.chain.clone()),
            net,
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
        let (sizes, setup) = coins_configuration(
            args,
            &self.params.chain,
            self.interaction.as_ref().expect("app_init_early succeeded"),
            logger,
            Arc::new(RocksTuning::default()),
            caches,
            queue,
        )?;

        // rdashd: an option it does not support yet, after every argument check Core makes and
        // before anything is read from or written to the chainstate.
        if let Some(refusal) = self.unsupported.take() {
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
                // CH2 has logged "Simulating a crash. Goodbye."; Core calls `_Exit(0)`.
                Err(OpenError::Crashed) => exit_now(0),
                Err(OpenError::Incompatible(message)) => return Err(InitFailure::Error(message)),
                // Core: node/chainstate.cpp:177, :184; the exit below.
                Err(OpenError::Interrupted) => {
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

        // Step 10a: the Dash tasks on the scheduler.
        // Core: init.cpp:2389-2396
        let scheduler = self.scheduler.as_ref().expect("started at step 4a");
        sync.schedule(scheduler, &self.shutdown);

        // Step 11: import blocks. `ThreadImport` sends V's startup ABC, then init waits for
        // genesis.
        self.steps.import()?;
        // `ThreadImport` then broadcasts the tip: a node that is not a masternode has no
        // `active_ctx`.
        // Core: init.cpp:2484-2499
        let chain = self.steps.chain().expect("step 7 started V");
        let view = chain.handle().view_handle();
        let snap = view.view().snap.clone();
        let ibd = view.ibd_service_cache();
        let tip = snap.has_tip().then_some(snap.tip.tip);
        chain.signals().initialize_current_block_tip(tip, ibd);
        // Core: init.cpp:2577-2580
        if self.shutdown.shutdown_requested() {
            logger.log_printf("Shutdown requested. Exiting.\n");
            return Err(InitFailure::Reported);
        }

        // Step 12: start node.
        // Core: init.cpp:2626-2755
        let net = self.net.as_mut().expect("step 6 built it");
        net.start(args, &self.ui, Arc::clone(&self.abort) as _)?;
        let (shared, connman) = (net.shared().expect("attached"), net.connman().expect("started"));
        self.rpc_net.attach(net.core(), shared, connman);

        // Step 13: finished.
        // Core: init.cpp:2761-2772
        #[cfg(feature = "test-hooks")]
        self.hold_warmup(args);
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

    /// V, from step 7 (P3's (A − B) passes).
    #[cfg(test)]
    pub(crate) fn chain(&self) -> Option<&crate::chain::Chain> {
        self.steps.chain()
    }

    /// A reader of V's block files of the test's own, from step 7 (P4b D3).
    #[cfg(test)]
    pub(crate) fn take_test_block_files(
        &mut self,
    ) -> Option<Box<dyn dashd_chainstate::chain::BlockFiles>> {
        self.steps.test_files.take()
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
/// with no transaction in the pool, `validation.cpp:2952-2953`), and `-assumevalid`. The node and
/// `NodeSut` both build their setup here.
// Core: init.cpp:1983-2023, node/chainstatemanager_args.cpp:33
pub(crate) fn coins_configuration<T: EngineTuning>(
    args: &ArgsManager,
    chain: &ChainParams,
    interaction: &ParamInteraction,
    logger: &Logger,
    tuning: Arc<T>,
    caches: Arc<ScriptCaches>,
    queue: Arc<CheckQueue>,
) -> Result<(CacheSizes, CoinsSetup<T>), InitFailure> {
    let (indexes, sizes) = cache_sizes(args, &interaction.net)?;

    let mut mempool = MemPoolOptions {
        check_ratio: i32::from(chain.default_consistency_checks),
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
    let setup = CoinsSetup {
        tuning,
        max_mempool: max_mempool_bytes,
        caches,
        queue,
        assume_valid: interaction.flags.chainman.assumed_valid(chain),
        xcheck: false,
        cjdns_reachable: crate::p2p::cjdns_reachable(args)?,
    };
    Ok((sizes, setup))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use dashd_rpc::blockchain::NodeChain as _;

    use super::*;
    use crate::abort::AbortSink as _;
    use crate::init::{Lines, setup_server_args};
    use crate::logging::Logger;
    use crate::steps::Point;

    /// `ParseArgs` and `InitConfig` as the binary runs them, on `dir`.
    fn config(
        dir: &Path,
        argv: &[&str],
    ) -> (Arc<ArgsManager>, Result<SelectedParams, InitFailure>) {
        let args = ArgsManager::new();
        setup_server_args(&args);
        let datadir = format!("-datadir={}", dir.display());
        let mut full = vec!["rdashd", datadir.as_str(), "-regtest", "-printtoconsole=0"];
        full.extend(init::quiet_node_args(argv));
        full.extend_from_slice(argv);
        args.parse_parameters(&full).unwrap();
        let params = init::init_config(&args);
        (Arc::new(args), params)
    }

    /// A node on `dir` whose debug log lines and stderr text are collected.
    fn start(dir: &Path, argv: &[&str]) -> (Node, Lines, Lines) {
        let (args, params) = config(dir, argv);
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

    #[test]
    fn starts_and_stops_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, log, stderr) = start(dir.path(), &[]);
        assert!(node.app_init_early());
        assert!(node.app_init_main());
        let pid = dir.path().join("regtest").join("dashd.pid");
        assert!(pid.exists());
        assert!(dir.path().join("regtest").join(crate::chainstate::CHAINSTATE_DIR).is_dir());
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
        const BL175_BLOCKS: &str = include_str!("../tests/data/bl175/blocks.txt");
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
            for (h, hex) in (1..).zip(BL175_BLOCKS.lines()) {
                let raw: Vec<u8> = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect();
                let block = dash_consensus::decode_block(&mut &raw[..]).unwrap().into_value();
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
