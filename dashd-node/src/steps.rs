//! The chainstate part of Core's `NodeContext` (`chainman`, `sporkman`) with init's and shutdown's
//! chainstate-only steps, in Core's order. `Node` runs them between its other services, and
//! `NodeSut` runs them alone (ND1-D10, O2b D32: the harness takes the production path).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use dash_args::ArgsManager;
use dash_clock::Clock;
use dash_consensus::primitives::BlockHash;
use dash_params::ChainParams;
use dashd_chainstate::chain::{
    BlockFiles, DEFAULT_CHECKBLOCKS, DEFAULT_CHECKLEVEL, Fatal, LoadFailure, ShutdownFlush,
    StartError, VExit, VParts, VerifyLoadError, VerifyLoadOptions, check_startup_readiness,
    load_chain_tip,
};
use dashd_chainstate::coins::CoinsCache;
use dashd_chainstate::flush::SubscriberHub;
#[cfg(feature = "test-support")]
use dashd_chainstate::reorg::PoolsOnly;
use dashd_chainstate::store::GuardedSnapshot;
use dashd_chainstate::tuning::EngineTuning;
use dashd_masternode::meta::{self, MasternodeMetaMan};
use dashd_masternode::netfulfilled::{self, NetFulfilledRequestManager};
use dashd_masternode::sporks::SporkManager;
use dashd_masternode::sporks::file::FILE_NAME;
use dashd_rpc::blockchain::BlockChange;
use dashd_store::KvEngine;
use dashd_store::flatfile::{FlatFs, OsFs};

use crate::abort::{AbortSink as _, NodeAbort};
use crate::chain::{self, Chain, NodeSeams};
use crate::chainstate::{Chainstate, CoinsSetup, Engine, NodeEvoHooks, OpenError, Opening};
use crate::init::SelectedParams;
use crate::logging::Logger;
use crate::shutdown::ShutdownState;
use crate::ui::Ui;
use crate::warnings::Warnings;
use crate::{InitFailure, notify, sporks};

/// V, the coins under it and the spork manager, from step 7a to `ResetCoinsViews`.
pub struct ChainSteps<E: KvEngine = Engine, F: FlatFs = OsFs> {
    logger: Arc<Logger>,
    shutdown: Arc<ShutdownState>,
    /// The node's warnings, which V's `DoWarning` sets.
    warnings: Arc<Warnings>,
    /// The RPC waits' `latestblock`, which V's `NotifyBlockTip` feeds (RP1a-2a).
    block_change: Arc<BlockChange>,
    /// Core's `sporkman`, and its file once step 7a loaded it (its destructor stores it then).
    sporks: Option<Arc<SporkManager>>,
    sporks_file: Option<(PathBuf, [u8; 4])>,
    /// Core's `netfulfilledman` and `mn_metaman` (SP1), from step 6; their destructors store
    /// their files when `LoadCache` succeeded (`is_valid`).
    netfulfilled: Option<Arc<NetFulfilledRequestManager>>,
    metaman: Option<Arc<MasternodeMetaMan>>,
    /// The network's data directory and message start the two caches load from and store to.
    net_caches: Option<(PathBuf, [u8; 4])>,
    /// CH1-node's V, from step 7.
    chain: Option<Chain>,
    /// What V handed back as shutdown stopped it: its flush side, which the shutdown flushes run
    /// on, until `ResetCoinsViews` (BL-271).
    exit: Option<VExit>,
    chainstate: Option<Chainstate<E, F>>,
    /// A second reader of V's block files, for the processor's `getdata` serving (P4b D3),
    /// until step 7c takes it.
    serve_files: Option<Box<dyn BlockFiles>>,
    /// A reader of V's block files of RPC's own, for `getblock` (DEC-606), until step 7 attaches
    /// the RPC slot.
    rpc_files: Option<Box<dyn BlockFiles>>,
    /// The process's one subscriber hub, from step 7's load: recovery's step 10 sets its `T_ack`,
    /// and CH2b's flush control and W thread take the same one (b3 D44) when 2b-2 wires them.
    hub: Option<Arc<SubscriberHub>>,
    /// The node's `AbortNode`, from step 7, which a failed shutdown flush reports to.
    abort: Option<Arc<NodeAbort>>,
    /// How many times shutdown tried to flush the chainstate: Core's two whenever it loaded,
    /// aborted or not (DEC-241).
    flushes: u32,
    /// Hears step 11 start, and shutdown reach V's stop and each flush.
    #[cfg(test)]
    pub(crate) probe: Option<Box<dyn FnMut(Point) + Send>>,
    /// A third reader of V's block files, for a test's own `NodeChain` (step 7c takes the second).
    #[cfg(test)]
    pub(crate) test_files: Option<Box<dyn BlockFiles>>,
    /// A test's fault around V's block files.
    #[cfg(test)]
    pub(crate) wrap_files: Option<WrapFiles>,
    /// V's mempool in place of none: `NodeSut`'s disconnect pools (DEC-500).
    #[cfg(feature = "test-support")]
    pub(crate) pools: Option<PoolsOnly>,
}

/// A test's fault around V's block files.
#[cfg(test)]
pub(crate) type WrapFiles = fn(Box<dyn BlockFiles>) -> Box<dyn BlockFiles>;

/// Where init or shutdown is, for a test's probe.
#[cfg(test)]
#[derive(Debug, PartialEq)]
pub(crate) enum Point {
    /// Step 11 starts.
    Import,
    /// V has stopped; the drain comes next.
    VStopped,
    /// A flush starts, with whether the node had aborted by then.
    Flush {
        aborted: bool,
    },
}

impl<E: KvEngine, F: FlatFs> ChainSteps<E, F> {
    /// Nothing loaded yet; `shutdown` and `warnings` are the node's.
    pub fn new(logger: Arc<Logger>, shutdown: Arc<ShutdownState>, warnings: Arc<Warnings>) -> Self {
        ChainSteps {
            logger,
            shutdown,
            warnings,
            block_change: Arc::default(),
            sporks: None,
            sporks_file: None,
            netfulfilled: None,
            metaman: None,
            net_caches: None,
            chain: None,
            exit: None,
            chainstate: None,
            serve_files: None,
            rpc_files: None,
            hub: None,
            abort: None,
            flushes: 0,
            #[cfg(test)]
            probe: None,
            #[cfg(test)]
            test_files: None,
            #[cfg(test)]
            wrap_files: None,
            #[cfg(feature = "test-support")]
            pools: None,
        }
    }

    /// The RPC waits' `latestblock`, which V's `uiInterface` sink feeds from step 7.
    pub fn set_block_change(&mut self, block_change: Arc<BlockChange>) {
        self.block_change = block_change;
    }

    /// Step 6's `sporkman`: the spork address `params` checked, then `-sporkkey`.
    // Core: init.cpp:1746-1757
    pub fn make_sporks(
        &mut self,
        args: &ArgsManager,
        params: &SelectedParams,
        clock: Arc<dyn Clock>,
    ) -> Result<(), InitFailure> {
        if let Some(error) = &params.spork_error {
            return Err(InitFailure::Error(error.clone()));
        }
        self.sporks = Some(sporks::spork_manager(args, &params.chain, clock)?);
        Ok(())
    }

    /// Step 7a: [`make_sporks`](Self::make_sporks)'s manager reads its cache in the network's
    /// data directory. Shutdown stores it there only if it loaded.
    // Core: init.cpp:1967-1972
    pub fn load_sporks(
        &mut self,
        args: &ArgsManager,
        params: &SelectedParams,
    ) -> Result<(), InitFailure> {
        let sporks = self.sporks.as_ref().expect("made at step 6");
        let path = args.get_data_dir_net()?.join(FILE_NAME);
        let message_start = params.chain.message_start;
        if sporks.load(&path, message_start).is_err() {
            return Err(InitFailure::Error(format!(
                "Failed to load sporks cache from {}",
                path.display()
            )));
        }
        self.sporks_file = Some((path, message_start));
        Ok(())
    }

    pub fn sporks(&self) -> Option<&Arc<SporkManager>> {
        self.sporks.as_ref()
    }

    /// Step 6's `mn_metaman` and `netfulfilledman`, after `sporkman`.
    // Core: init.cpp:1759-1762
    pub fn make_net_caches(&mut self, params: &SelectedParams, clock: Arc<dyn Clock>) {
        self.metaman = Some(Arc::new(MasternodeMetaMan::new()));
        let expire = i64::from(params.chain.fulfilled_request_expire_time);
        self.netfulfilled = Some(Arc::new(NetFulfilledRequestManager::new(clock, expire)));
    }

    /// `LoadCache(fLoadCacheFiles)` of `netfulfilledman`, then `mn_metaman`, after step 7c: a
    /// node whose loaded active chain has a tip, without a reindex, loads them; otherwise they
    /// are stored empty (SP1 D4).
    ///
    /// # Errors
    ///
    /// Core's init error for a cache it refuses to load or cannot clear.
    // Core: init.cpp:2273-2289
    pub fn load_net_caches(
        &mut self,
        args: &ArgsManager,
        params: &SelectedParams,
    ) -> Result<(), InitFailure> {
        let reindex = args.get_bool_arg("-reindex", false)?
            || args.get_bool_arg("-reindex-chainstate", false)?;
        let tip =
            self.chain.as_ref().is_some_and(|c| c.handle().view_handle().snapshot().has_tip());
        let load = !reindex && tip;
        let dir = args.get_data_dir_net()?;
        let message_start = params.chain.message_start;
        let fail = |what: &str, file: &str| {
            let path = dir.join(file).display().to_string();
            InitFailure::Error(if load {
                format!("Failed to load {what} cache from {path}")
            } else {
                format!("Failed to clear {what} cache at {path}")
            })
        };
        // Shutdown stores each manager that is valid, after a later failure too: Core's
        // destructors run whatever init returned.
        self.net_caches = Some((dir.clone(), message_start));
        let netfulfilled = self.netfulfilled.as_ref().expect("made at step 6");
        if !netfulfilled.load_cache(&dir.join(netfulfilled::FILE_NAME), message_start, load) {
            return Err(fail("fulfilled requests", netfulfilled::FILE_NAME));
        }
        let metaman = self.metaman.as_ref().expect("made at step 6");
        if !metaman.load_cache(&dir.join(meta::FILE_NAME), message_start, load) {
            return Err(fail("masternode", meta::FILE_NAME));
        }
        Ok(())
    }

    pub fn netfulfilled(&self) -> Option<&Arc<NetFulfilledRequestManager>> {
        self.netfulfilled.as_ref()
    }

    pub fn metaman(&self) -> Option<&Arc<MasternodeMetaMan>> {
        self.metaman.as_ref()
    }

    /// Step 7's load, once, in Core's order (2b-D18): the chainstate `coins` opens, whose database
    /// holds the block index, and recovery's steps 0–3; the block index from it and
    /// `LoadGenesisBlock` (V's devnet step); then recovery on the loaded index through the load's
    /// subscriber hub (Core's `ReplayBlocks`), and the coins attached to V before anything else
    /// reaches it. `LoadChainTip` then runs at the coins' best block, and the readiness guard (CH3a
    /// D70) on that tip and the blocks recovery replayed (2b-D32); then `VerifyLoadedChainstate`
    /// (CH1-node D64), whose init message goes to `ui`. V's startup ABC waits for
    /// [`import`](Self::import). After
    /// [`load_sporks`](Self::load_sporks), whose manager V reads. The outer error is an exception;
    /// the inner one is Core's load status, whose message init shows.
    ///
    /// Without a devnet step V's start shows nothing, so V starts once the coins are open:
    /// starting V's threads before the coins database measured a few more page faults at ready
    /// (PF0 node-startup-genesis, ND1-D6). This is keyed on V's pending devnet step, not the
    /// network (2b-D8): a devnet's stored chain has none, as Core's `LoadGenesisBlock` returns
    /// early once genesis is indexed, so its restart runs `LoadChainTip` as on any network.
    // Core: node/chainstate.cpp:182-212, :220-260, :272-277; validation.cpp:4992-5018
    #[expect(
        clippy::too_many_arguments,
        reason = "Core's LoadChainstate inputs: the arguments, init's ui, the chain, the node's seams and the coins"
    )]
    pub fn load<T: EngineTuning + 'static>(
        &mut self,
        args: &ArgsManager,
        ui: &Ui,
        params: Arc<ChainParams>,
        clock: Arc<dyn Clock>,
        abort: Arc<NodeAbort>,
        setup: CoinsSetup<T>,
        coins: impl FnOnce() -> Result<Opening<E, F>, OpenError>,
    ) -> Result<Result<(), OpenError>, InitFailure>
    where
        E: 'static,
        F: 'static,
        for<'a> GuardedSnapshot<'a, E>: Sync,
    {
        self.abort = Some(Arc::clone(&abort));
        // Core: init.cpp:2083-2085, before the load.
        let check_blocks = args.get_int_arg("-checkblocks", DEFAULT_CHECKBLOCKS)?;
        let check_level = args.get_int_arg("-checklevel", DEFAULT_CHECKLEVEL)?;
        let require_full_verification =
            args.is_arg_set("-checkblocks") || args.is_arg_set("-checklevel");
        // Core: node/chainstate.cpp:307-311
        let assume_valid = match setup.assume_valid {
            Some(block) => format!("Assuming ancestors of block {block} have valid signatures.\n"),
            None => "Validating signatures for all blocks.\n".to_owned(),
        };
        self.logger.log_printf(&assume_valid);
        // Core connects `-blocknotify` after the chainstate loads (init.cpp:2429); V notifies
        // nothing before step 11's ABC, so it goes in with V's parts.
        // V polls and sets the node's own shutdown flag (CH1-node D52).
        let seams = NodeSeams {
            clock,
            sporks: Arc::clone(self.sporks.as_ref().expect("step 7a made sporkman")) as _,
            abort,
            ui: notify::chain_ui(&self.logger, args, &self.warnings, &self.block_change)?,
            shutdown: Arc::clone(&self.shutdown) as _,
            stop_at_height: args.get_int_arg("-stopatheight", 0)?,
        };
        // Core opens `blocks/index` first; here it is in the chainstate's database.
        let opening = match coins() {
            Ok(opening) => opening,
            Err(e) => return Ok(Err(e)),
        };
        let recovery = match opening.open() {
            Ok(recovery) => recovery,
            Err(e) => return Ok(Err(e)),
        };
        let meta = recovery.meta();
        let stored = meta.tip.is_some() || meta.head_blocks.is_some();
        let records = chain::index_records(recovery.block_index());
        let files: Box<dyn BlockFiles> = Box::new(opening.block_files(&recovery));
        #[cfg(test)]
        let files = match self.wrap_files {
            Some(wrap) => wrap(files),
            None => files,
        };
        self.serve_files = Some(Box::new(opening.block_files(&recovery)));
        self.rpc_files = Some(Box::new(opening.block_files(&recovery)));
        #[cfg(test)]
        {
            self.test_files = Some(Box::new(opening.block_files(&recovery)));
        }
        let (mut parts, mut opened) =
            match chain::open_parts(Arc::clone(&params), seams, records, files) {
                Ok(parts) => (Some(parts), Ok(())),
                Err(failure) => (None, Err(OpenError::from(failure))),
            };
        let index = parts.as_ref().map(|parts| parts.chain.reader());
        if let Some(parts) = parts.take_if(|parts| parts.devnet_pending()) {
            // After the index load's own refusals (Core reports them first), before V starts.
            if stored {
                return Ok(Err(OpenError::Incompatible(devnet_lost_index(&params))));
            }
            opened = self.start_chain(parts)?;
        }
        if let (Ok(()), Some(index)) = (&opened, index) {
            let hub = self.hub.insert(Arc::new(SubscriberHub::new()));
            match opening.recover(recovery, &index, hub) {
                // Stored first: shutdown flushes the coins even if the attach fails.
                Ok(recovered) => self.chainstate = Some(opening.into_chainstate(recovered)),
                Err(e) => opened = Err(e),
            }
        }
        // `LoadChainTip` at the coins' best block; a null one skips it (node/chainstate.cpp:272).
        if let (Ok(()), Some(parts), Some(chainstate)) =
            (&opened, parts.as_mut(), self.chainstate.as_ref())
            && let Some(tip) = chainstate.tip()
        {
            let reader = parts.chain.reader();
            let lookup = |hash: &BlockHash, what: &str| {
                reader.lookup(hash).ok_or_else(|| Fatal::new(format!("{what}: unknown block")))
            };
            let loaded = lookup(&tip, "LoadChainTip")
                .and_then(|id| load_chain_tip(&mut parts.chain, &mut parts.idx, id).map(|()| id));
            opened = match loaded {
                // The guard on recovery's transition, then the tip once (2b-D32), as `open_parts`
                // maps it.
                Ok(id) => chainstate
                    .replayed()
                    .iter()
                    .filter(|&hash| *hash != tip)
                    .map(|hash| lookup(hash, "check_startup_readiness"))
                    .chain(std::iter::once(Ok(id)))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|fatal| LoadFailure::Failure {
                        cause: Some(fatal.0),
                    })
                    .and_then(|blocks| {
                        check_startup_readiness::<NodeEvoHooks>(
                            &params,
                            &parts.chain,
                            &*parts.files,
                            &blocks,
                        )
                    })
                    .map_err(OpenError::from),
                Err(fatal) => Err(OpenError::from(LoadFailure::Failure {
                    cause: Some(fatal.0),
                })),
            };
        }
        let opened = match (opened, parts) {
            (Ok(()), Some(parts)) => self.start_chain(parts)?,
            (opened, _) => opened,
        };
        let opened = match opened {
            Ok(()) => {
                let chainstate = self.chainstate.as_ref().expect("recovered");
                let hub = self.hub.as_ref().expect("recovered through it");
                let coins = chainstate
                    .attach_parts(&params, hub, &setup)
                    .map_err(|e| InitFailure::Exception(e.to_string()))?;
                // A failed attach is V's `AbortNode`: once V is joined, its sink has requested
                // the shutdown, whose flushes run on the state V handed back (DEC-241).
                let chain = self.chain.as_mut().expect("V started");
                chain.attach_coins(coins).map_err(|_| {
                    self.exit = chain.stop();
                    OpenError::Interrupted
                })
            }
            failed => failed,
        };
        let verify = VerifyLoadOptions {
            check_blocks,
            check_level,
            require_full_verification,
            // rdashd refuses -reindex and -reindex-chainstate (CH5), and opens no evodb.
            coins_empty: self.chainstate.as_ref().is_none_or(|c| c.tip().is_none()),
            ..VerifyLoadOptions::default()
        };
        // Core: init.cpp:2100-2112
        let opened = opened.and_then(|()| {
            ui.init_message("Verifying blocks…");
            let chain = self.chain.as_mut().expect("V started");
            match chain.verify_loaded(verify) {
                Ok(verified) => verified.map_err(verify_failure),
                // V stopped on an `AbortNode`, as for the attach.
                Err(_) => {
                    self.exit = chain.stop();
                    Err(OpenError::Interrupted)
                }
            }
        });
        Ok(opened)
    }

    /// Starts V on `parts`. A load failure V reports as it starts (the devnet step) is init's; a
    /// thread that cannot be spawned is an exception.
    fn start_chain(&mut self, parts: VParts) -> Result<Result<(), OpenError>, InitFailure> {
        #[cfg(feature = "test-support")]
        let parts = match &self.pools {
            Some(pools) => VParts {
                mempool: Box::new(pools.clone()),
                ..parts
            },
            None => parts,
        };
        match Chain::start(parts) {
            Err(StartError::Load(failure)) => Ok(Err(failure.into())),
            Err(StartError::Spawn(e)) => Err(InitFailure::Exception(e.to_string())),
            Ok(chain) => {
                self.chain = Some(chain);
                Ok(Ok(()))
            }
        }
    }

    /// Step 11: `ThreadImport` sends V's startup ABC, then init waits for genesis or a shutdown.
    // Core: init.cpp:2419-2425, :2455-2458
    pub fn import(&mut self) -> Result<(), InitFailure> {
        #[cfg(test)]
        if let Some(probe) = &mut self.probe {
            probe(Point::Import);
        }
        let chain = self.chain.as_mut().expect("step 7 started V");
        chain.import().map_err(|e| InitFailure::Exception(e.to_string()))?;
        let shutdown = Arc::clone(&self.shutdown);
        chain.wait_for_genesis(|| shutdown.shutdown_requested());
        Ok(())
    }

    /// The subscriber hub the coins recovered through; `None` before recovery.
    pub fn hub(&self) -> Option<&Arc<SubscriberHub>> {
        self.hub.as_ref()
    }

    /// The address of the subscriber hub the loaded chainstate's recovery ran on.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn recovered_on(&self) -> Option<usize> {
        self.chainstate.as_ref().and_then(Chainstate::recovered_on)
    }

    pub fn chain(&self) -> Option<&Chain> {
        self.chain.as_ref()
    }

    /// The block-file reader step 7 opened for serving blocks (P4b D3), once.
    pub fn take_serve_files(&mut self) -> Option<Box<dyn BlockFiles>> {
        self.serve_files.take()
    }

    /// The block-file reader step 7 opened for RPC's `getblock`, once.
    pub fn take_rpc_files(&mut self) -> Option<Box<dyn BlockFiles>> {
        self.rpc_files.take()
    }

    /// The block the coins are at; `None` before they load or for an empty chainstate.
    pub fn coins_tip(&self) -> Option<BlockHash> {
        self.chainstate.as_ref()?.tip()
    }

    /// [`Chainstate::coins_cell`], while loaded.
    pub fn coins_cell(&self) -> Option<&Arc<Mutex<CoinsCache<E>>>> {
        self.chainstate.as_ref()?.coins_cell()
    }

    pub fn flushes(&self) -> u32 {
        self.flushes
    }

    /// Shutdown's chainstate part, after the scheduler stops: V stops once the `loadblk` thread
    /// is done, the queued signals run, then Core's two flushes on the state V handed back
    /// around the last signals, the spork store and the stopped indexes, and `ResetCoinsViews`;
    /// after an `AbortNode` too (DEC-241). Over whatever step 7 loaded, as for a failed init.
    // Core: init.cpp:355-458
    pub fn shutdown(&mut self) {
        if let Some(chain) = &mut self.chain {
            // V's state the first time it stops: a failed load's stop kept it.
            if let Some(exit) = chain.stop() {
                self.exit = Some(exit);
            }
            #[cfg(test)]
            if let Some(probe) = &mut self.probe {
                probe(Point::VStopped);
            }
            chain.drain_background_callbacks();
        }
        // Core: init.cpp:379-385
        self.force_flush(ShutdownFlush::First);
        if let Some(mut chain) = self.chain.take() {
            chain.flush_background_callbacks();
        }
        // Core: init.cpp:415 (`~CSporkManager` stores a file `LoadCache` read)
        if let (Some(sporks), Some((path, message_start))) =
            (self.sporks.take(), self.sporks_file.take())
        {
            #[cfg(feature = "test-hooks")]
            self.logger.log_printf("test-hooks: storing the sporks\n");
            if let Err(e) = sporks.store(&path, message_start) {
                self.logger
                    .log_printf(&format!("Error: CoreWrite: Serialize or I/O error - {e}\n"));
            }
        }
        // `~CNetFulfilledRequestManager`, then `~CMasternodeMetaMan`, each storing a file that
        // `LoadCache` accepted.
        // Core: init.cpp:418-419; netfulfilledman.cpp:15-19; masternode/meta.cpp:20-24
        if let Some((dir, message_start)) = self.net_caches.take() {
            if let Some(m) = self.netfulfilled.take().filter(|m| m.is_valid()) {
                m.store(&dir.join(netfulfilled::FILE_NAME), message_start);
            }
            if let Some(m) = self.metaman.take().filter(|m| m.is_valid()) {
                m.store(&dir.join(meta::FILE_NAME), message_start);
            }
        }
        // Once the indexes have stopped, the second flush, then `ResetCoinsViews`.
        // Core: init.cpp:449-455
        self.force_flush(ShutdownFlush::Second);
        self.exit = None;
        self.chainstate = None;
    }

    /// A killed process: V stops, nothing is flushed and the sporks are not stored. The signals
    /// thread still runs what is queued as it ends (the listeners are the harness's).
    #[cfg(feature = "test-support")]
    pub fn kill(&mut self) {
        if let Some(mut chain) = self.chain.take() {
            chain.stop();
        }
        self.exit = None;
        self.sporks = None;
        self.sporks_file = None;
        self.netfulfilled = None;
        self.metaman = None;
        self.net_caches = None;
        self.chainstate = None;
    }

    /// Core's `ForceFlushStateToDisk` at shutdown, whenever the chainstate loaded
    /// (`CanFlushToDisk`), whether or not the node aborted (DEC-241): first the ALWAYS flush of
    /// what V handed back as it stopped (BL-271, DEC-613), the coins, the block index and the
    /// block files, waited for; its `ChainStateFlushed` reaches the listeners after the
    /// [first](ShutdownFlush::First) only. Its failure was reported as V reports one, and gets the
    /// flush's failure line. V hands back nothing to flush after one of Core's asserts or an SB4
    /// contract failure (BL-271 r1 m-2), so only the WAL sync runs.
    ///
    /// Then a WAL sync: Core's ALWAYS always makes a synced write (the block index's
    /// `WriteBatchSync`), even with nothing dirty. A failed sync reports as Core's: `HandleError`'s
    /// two lines, `FlushStateToDisk`'s `AbortNode` (exit 1), then the flush's failure line. It
    /// stays failed, as in Core: LevelDB returns a failed log sync's status to every later write,
    /// so Core's second flush fails again with the same text. Here the store's latch refuses the
    /// second sync, and the flush reports the latched failure the same way, a second `AbortNode`
    /// included (ND1-D16, DEC-253). The inner text is the engine's (CH2 D10).
    // Core: validation.cpp:3052-3104, :3116-3117, :3122-3126, init.cpp:379-385, :449-455,
    // dbwrapper.cpp:261-268, txdb.cpp:276-285, leveldb/db/db_impl.cc:1229-1244, :650-655,
    // :1328-1331
    fn force_flush(&mut self, which: ShutdownFlush) {
        let Some(chainstate) = &self.chainstate else {
            return;
        };
        self.flushes += 1;
        #[cfg(test)]
        if let Some(probe) = &mut self.probe {
            probe(Point::Flush {
                aborted: self.shutdown.aborted(),
            });
        }
        #[cfg(feature = "test-hooks")]
        self.logger.log_printf("test-hooks: flushing the chainstate\n");
        let reports = self.abort.as_ref().map(|a| a.reports());
        // A latched store fails the flush with its first failure, which the WAL sync reports.
        if !chainstate.latched()
            && let Some(exit) = &mut self.exit
            && let Err(f) = exit.force_flush(which)
        {
            // `state.ToString()`: the message of the `AbortNode` that reported the failure (W's or
            // V's, landed by now), the first since the flush started, not the latched store text.
            // That first one is the flush's: `Node::shutdown` stopped RPC, the net and the
            // scheduler first, V has stopped, and the signals were drained before the first flush
            // (the signals thread is gone by the second), so no listener runs in between to raise
            // one (a listener's panic is an `AbortNode`), and a failed flush queues no
            // `ChainStateFlushed`.
            // Core: validation.cpp:2245-2249, :3132
            let reason = self.abort.as_ref().zip(reports).and_then(|(a, n)| a.report(n));
            let reason = reason.unwrap_or(f.0);
            self.logger
                .log_printf(&format!("ForceFlushStateToDisk: failed to flush state ({reason})\n"));
            return;
        }
        if let Err(e) = chainstate.force_flush() {
            // Core: dbwrapper.cpp:265-267
            self.logger.log_printf(&format!("{e}\n"));
            self.logger.log_printf(
                "You can use -debug=leveldb to get more complete diagnostic messages\n",
            );
            let reason = format!("System error while flushing: {e}");
            self.abort.as_ref().expect("step 7 loaded the chainstate").abort_node(&reason, None);
            self.logger
                .log_printf(&format!("ForceFlushStateToDisk: failed to flush state ({reason})\n"));
        }
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl ChainSteps {
    /// Makes the chainstate's next WAL sync fail (the failed shutdown flush's tests).
    pub fn fail_next_wal_sync(&self) {
        if let Some(chainstate) = &self.chainstate {
            chainstate.fail_next_wal_sync();
        }
    }
}

/// Init's error for V's pending devnet step over stored coins, whose block index is gone (a
/// stored one has no pending step, 2b-D8). Core's `LoadChainTip` follows `LoadGenesisBlock` there:
/// it loads at a coins tip `LoadGenesisBlock` re-added (genesis or the devnet's genesis block),
/// and otherwise refuses with "Error initializing block database" and the reindex question (a
/// marker alone fails `ReplayBlocks`). V takes a devnet's parts before recovery and has neither
/// path (CH1's): its startup ABC would connect from genesis onto the stored coins and fail
/// fatally, so init refuses every case first, with no reindex question (BL-271 r1).
// Core: validation.cpp:5406-5407; node/chainstate.cpp:272-277
fn devnet_lost_index(params: &ChainParams) -> String {
    format!(
        "rdashd cannot load {} on stored coins without their block index: a devnet's \
         LoadChainTip after its genesis step is not built (ND1 2b-D8); start the devnet in a new \
         data directory",
        params.network.network_id()
    )
}

/// `VerifyLoadedChainstate`'s failing statuses as the load's (`init.cpp:2115-2133`): a failure
/// asks for a reindex, an interruption exits, too small a dbcache is a plain `InitError`.
fn verify_failure(error: VerifyLoadError) -> OpenError {
    match error {
        VerifyLoadError::Failure(message) => OpenError::Recoverable {
            message: message.to_owned(),
            cause: None,
        },
        VerifyLoadError::Interrupted(_) => OpenError::Interrupted,
        VerifyLoadError::InsufficientDbcache(message) => {
            OpenError::Incompatible(message.to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use dashd_chainstate::chain::verify::{FUTURE_TIP, INSUFFICIENT_DBCACHE, VERIFY_INTERRUPTED};

    use super::*;

    #[test]
    fn verifys_statuses_map_to_cores_load_statuses() {
        assert!(matches!(
            verify_failure(VerifyLoadError::Failure(FUTURE_TIP)),
            OpenError::Recoverable { message, cause: None } if message == FUTURE_TIP
        ));
        assert!(matches!(
            verify_failure(VerifyLoadError::Interrupted(VERIFY_INTERRUPTED)),
            OpenError::Interrupted
        ));
        assert!(matches!(
            verify_failure(VerifyLoadError::InsufficientDbcache(INSUFFICIENT_DBCACHE)),
            OpenError::Incompatible(message) if message == INSUFFICIENT_DBCACHE
        ));
    }
}
