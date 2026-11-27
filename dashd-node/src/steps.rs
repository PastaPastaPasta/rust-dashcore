//! The chainstate part of Core's `NodeContext` (`chainman`, `sporkman`) with init's and shutdown's
//! chainstate-only steps, in Core's order. `Node` runs them between its other services, and
//! `NodeSut` runs them alone (ND1-D10, O2b D32: the harness takes the production path).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use dash_args::ArgsManager;
use dash_clock::Clock;
use dash_consensus::primitives::BlockHash;
use dash_params::ChainSetup;
use dashd_chainstate::chain::{
    BlockAtPos, BlockFiles, DEFAULT_CHECKBLOCKS, DEFAULT_CHECKLEVEL, Fatal, LoadFailure,
    OPEN_BLOCK_DB_FAILED, ReadAheadSetup, ShutdownFlush, StartError, VExit, VParts,
    ValidationInterface, ValidationSignals, VerifyLoadError, VerifyLoadOptions,
    check_startup_readiness, load_chain_tip,
};
use dashd_chainstate::coins::CoinsCache;
use dashd_chainstate::flush::{Kept, SubscriberHub};
use dashd_chainstate::recovery::Wipe;
#[cfg(feature = "test-support")]
use dashd_chainstate::reorg::PoolsOnly;
use dashd_chainstate::store::GuardedSnapshot;
use dashd_chainstate::subscriber::ChainFeedTable;
use dashd_chainstate::tuning::EngineTuning;
#[cfg(test)]
use dashd_locks::is::db::SCHEMA as IS_SCHEMA;
use dashd_locks::is::listener::IsListenerSigner;
use dashd_locks::is::seams::SyncView;
use dashd_masternode::meta::{self, MasternodeMetaMan};
use dashd_masternode::netfulfilled::{self, NetFulfilledRequestManager};
use dashd_masternode::sporks::SporkManager;
use dashd_masternode::sporks::file::FILE_NAME;
use dashd_masternode::sync::MasternodeSync;
use dashd_mempool::PoolSink;
use dashd_mempool::is::IsMempool;
use dashd_rpc::blockchain::BlockChange;
use dashd_store::KvEngine;
#[cfg(test)]
use dashd_store::MemoryEngine;
use dashd_store::flatfile::{FlatFs, OsFs};

use crate::abort::{AbortSink as _, NodeAbort, Report};
use crate::chain::{self, Chain, ImportHooks, NodeSeams};
use crate::chainstate::{
    Chainstate, CoinsSetup, Engine, NodeEvoHooks, OpenError, Opening, failed_sync,
};
use crate::indexes::{COINSTATS_CACHE, IndexCaches, IndexHandles, IndexParts, NodeIndexes};
use crate::init::SelectedParams;
use crate::init::caches::start_bls_check_threads;
use crate::init::consts::{DEFAULT_COINSTATSINDEX, DEFAULT_TXINDEX};
#[cfg(test)]
use crate::instantsend::engine_opener;
use crate::instantsend::{
    IsConflictsSlot, IsOpener, IsParts, NeverSynced, NodeChainLocks, NodeSyncView,
};
use crate::logging::Logger;
use crate::mempool::NodeMempool;
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
    /// Core's `fReindex` after the block index load: `-reindex`, or the reindex flag the block
    /// tree DB holds (a reindex that began and did not finish).
    pub(crate) reindex: bool,
    /// The `-reindex` argument (Core's `options.reindex`).
    pub(crate) reindex_arg: bool,
    /// CH1-node's V, from step 7.
    chain: Option<Chain>,
    /// The node's mempool, from step 7's load until shutdown resets it.
    mempool: Option<Arc<NodeMempool>>,
    /// What V handed back as shutdown stopped it: its flush side, which the shutdown flushes run
    /// on, until `ResetCoinsViews` (BL-271).
    exit: Option<VExit>,
    /// The coins from recovery, or from a refusal in a state where Core is past `InitCoinsCache`
    /// ([`InitError::after_coins_cache`], BL-636).
    ///
    /// [`InitError::after_coins_cache`]: dashd_chainstate::recovery::InitError::after_coins_cache
    chainstate: Option<Chainstate<E, F>>,
    /// Step 7 attempted the coins' attach to V. Until then the shutdown flushes are bare
    /// ([`Chainstate::bare_flush`]).
    attach_attempted: bool,
    /// A bare flush's WAL sync failed and latched the store, so the next flush is bare too and
    /// meets that failure, as Core's next flush meets the failed log sync LevelDB keeps. No path
    /// latches the store before the attach today; the gate's latch clause keeps today's handling
    /// should one, since a failure other than the bare flush's own would otherwise be reported as
    /// a failed sync.
    bare_latched: bool,
    /// A second reader of V's block files, for the processor's `getdata` serving (P4b D3),
    /// until step 7c takes it.
    serve_files: Option<Box<dyn BlockFiles>>,
    /// A reader of V's block files of RPC's own, for `getblock` (DEC-606), until step 7 attaches
    /// the RPC slot.
    rpc_files: Option<Box<dyn BlockFiles>>,
    /// The process's one subscriber hub, from step 7's load: recovery's step 10 sets its `T_ack`,
    /// and CH2b's flush control and W thread take the same one (b3 D44) when 2b-2 wires them.
    /// It runs over V's feeds and block index, which the indexes subscribe to.
    hub: Option<Arc<SubscriberHub>>,
    /// A reader of V's block files, from step 7's load to shutdown: the indexes' catch-up and the
    /// ChainLock signer's reads.
    index_files: Option<IndexFiles>,
    /// Step 8's indexes, from step 7's end to their stop between shutdown's two flushes.
    indexes: Option<NodeIndexes>,
    /// Step 7's open wiped the chainstate alone (`-reindex-chainstate`, or its interrupted wipe
    /// resumed), so step 11's import rebuilds it over the kept block index.
    rebuilds_chainstate: bool,
    /// The open such an import defers to its end (Core's `g_indexes_ready_to_sync`), for step
    /// 11's `loadblk` thread.
    deferred_open: Option<NodeIndexes>,
    /// The node's `AbortNode`, from step 7, which a failed shutdown flush reports to.
    abort: Option<Arc<NodeAbort>>,
    /// Opens `llmq/isdb` at step 7 (rdashd's RocksDB, `NodeSut`'s engine); without one, the load
    /// fails, except a unit test's, which keeps its locks in memory.
    pub(crate) is_open: Option<IsOpener>,
    /// Step 7's `mn_sync`, which NetInstantSend's listener reads; a unit test's load has none.
    pub(crate) is_sync: Option<Arc<MasternodeSync>>,
    /// GV1b's governance, made before the load: V is bound to it as V spawns, its store loads
    /// after the masternode cache, and a clean shutdown stores it.
    pub(crate) governance: Option<Arc<crate::governance::NodeGovernance>>,
    /// The InstantSend parts the coins' attach built: the store, the gate and the tip cache the
    /// connector, the evo applier and the listener share. Not the attempt ledger, which only V's
    /// objects hold.
    is: Option<IsParts>,
    /// NetInstantSend's listener on V's signals, from the load's verify to shutdown.
    is_listener: Option<(ValidationSignals, Arc<dyn ValidationInterface>)>,
    /// The listener's build over the load's inputs, which step 7c reruns with a masternode's
    /// signer.
    is_relisten: Option<IsRelisten>,
    /// The `AbortNode` that failed a shutdown flush on V's state, which the next flush repeats
    /// when the failure was the flush's own (BL-482).
    flush_abort: Option<Report>,
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
    /// A test's wrapper around the indexes' block files.
    #[cfg(test)]
    pub(crate) wrap_index_files: Option<Box<dyn Fn(IndexFiles) -> IndexFiles + Send>>,
    /// The harness's listener factory (CH3a a10 M-2): each V gets its listener right after
    /// `Chain::start`, before the coins' attach, `VerifyLoadedChainstate` and every refusal after
    /// them, so a refused load's teardown is heard, as Core's EventLog, registered before
    /// `LoadChainstate`, hears its own.
    #[cfg(feature = "test-support")]
    pub(crate) listener: Option<Arc<ListenerFactory>>,
    /// Every V's ChainLock schedule, set as it starts (CH3a a10 C3).
    #[cfg(feature = "test-support")]
    pub(crate) cl_schedule: dashd_chainstate::chain::ClSchedule,
    /// A test's call on V just before init's `VerifyLoaded` (BL-484).
    #[cfg(test)]
    pub(crate) before_verify: Option<BeforeVerify>,
    /// The `coins_empty` init's `VerifyLoaded` was sent (RP1a D68).
    #[cfg(test)]
    pub(crate) verified_coins_empty: Option<bool>,
    /// A test's failure of the start after recovery.
    #[cfg(test)]
    pub(crate) start_fault: Option<StartFault>,
    /// V's mempool in place of the node's pool: `NodeSut`'s disconnect pools (DEC-500).
    #[cfg(feature = "test-support")]
    pub(crate) pools: Option<PoolsOnly>,
    /// V without a mempool in place of the node's pool (`NodeSut`'s `no_mempool`).
    #[cfg(feature = "test-support")]
    pub(crate) null_mempool: bool,
    /// A test's call just before the pool's admission worker attaches, after the coins and evo
    /// attach.
    #[cfg(test)]
    pub(crate) before_pool_attach: Option<BeforePoolAttach>,
    /// This load's `-maxtipage`, which only `NodeSut` sets.
    #[cfg(feature = "test-support")]
    pub(crate) max_tip_age: Option<std::time::Duration>,
    /// The first shutdown flush, unless it returns before W, has W panic before its next step
    /// (BL-586).
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) panic_w_in_first_flush: bool,
    /// A test's call at step 11's start, before startup's ABC is sent (BL-586).
    #[cfg(test)]
    pub(crate) before_import: Option<BeforeImport>,
    /// Commands a `before_import` hook awaited on V. [`Chain::import`]'s count allows them.
    #[cfg(all(test, debug_assertions))]
    pub(crate) forgive_import_sends: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// With it V starts holding commands until startup's ABC completes, as after a tied
    /// fallback (D38), and step 11 calls it with V before it sends that ABC.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) held_import: Option<HeldImport>,
    /// A test's call on the opened engine before recovery's open (CH6a's `request_reindex`).
    #[cfg(test)]
    pub(crate) before_open: Option<BeforeOpen<E, F>>,
    /// A regtest import of a Dash Core directory whose databases have moved: the load stores
    /// the fresh store's reindex flag and completes it (CH5-4b).
    pub(crate) core_import: Option<crate::core_import::Pending>,
    /// What step 11's `loadblk` thread calls ([`Chain::set_import_hooks`]).
    import_hooks: Option<ImportHooks>,
    /// The chain's parameters and V's block files' `ReadBlockFromDisk(pos)`, from step 7, for
    /// step 11's reindex scan.
    scan_parts: Option<(Arc<ChainSetup>, Arc<dyn BlockAtPos>)>,
    /// A test's blk files in place of the blocks directory's.
    #[cfg(test)]
    pub(crate) blk_files: Option<Box<dyn crate::chain::BlkFiles>>,
    /// The shutdown flush in which V's flush side panics before its job (BL-674).
    #[cfg(any(all(test, feature = "test-support"), feature = "test-hooks"))]
    pub(crate) panic_v_in_flush: Option<ShutdownFlush>,
}

/// A test's call before the pool attaches (`ChainSteps::before_pool_attach`).
#[cfg(test)]
pub(crate) type BeforePoolAttach = Box<dyn FnOnce(&Arc<NodeMempool>, &Chain) + Send>;

/// A test's call with V before a held startup ABC (`ChainSteps::held_import`).
#[cfg(all(test, feature = "test-support"))]
pub(crate) type HeldImport = Box<dyn FnOnce(&Chain) + Send>;

/// A test's call before startup's ABC (`ChainSteps::before_import`): V's handle, the chainstate's
/// latch and the node's `AbortNode`.
#[cfg(test)]
pub(crate) type BeforeImport = Box<
    dyn FnOnce(
            &dashd_chainstate::chain::ChainHandle,
            &dashd_chainstate::store::GlobalFailStop,
            &Arc<NodeAbort>,
        ) + Send,
>;

/// NetInstantSend's listener over one load's inputs, with or without a masternode's signer.
type IsRelisten =
    Box<dyn Fn(Option<IsListenerSigner>) -> Arc<dyn ValidationInterface> + Send + Sync>;

/// Makes the listener a new V's signals get, over that V's reader (`ChainSteps::listener`).
#[cfg(feature = "test-support")]
pub type ListenerFactory = dyn Fn(&dash_chain::ChainReader) -> Arc<dyn dashd_chainstate::chain::ValidationInterface>
    + Send
    + Sync;

/// A test's fault around V's block files.
#[cfg(test)]
pub(crate) type WrapFiles = fn(Box<dyn BlockFiles>) -> Box<dyn BlockFiles>;

type IndexFiles = Arc<dyn BlockFiles + Sync>;

/// A test's call on V just before init's `VerifyLoaded`.
#[cfg(test)]
pub(crate) type BeforeVerify = fn(&Chain);

/// [`ChainSteps::before_open`]'s call.
#[cfg(test)]
pub(crate) type BeforeOpen<E, F> = Box<dyn FnOnce(&mut Opening<E, F>)>;

/// A test's failure of the start after recovery (`ChainSteps::start_fault`).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartFault {
    /// V's spawn, as a thread that cannot be spawned.
    Spawn,
    /// The attach once it is attempted: V stops on a panic before it holds a flush side, so it
    /// hands back nothing (BL-636's guard).
    Attach,
}

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
            reindex: false,
            reindex_arg: false,
            chain: None,
            mempool: None,
            exit: None,
            chainstate: None,
            attach_attempted: false,
            bare_latched: false,
            serve_files: None,
            rpc_files: None,
            hub: None,
            index_files: None,
            indexes: None,
            rebuilds_chainstate: false,
            deferred_open: None,
            abort: None,
            is_open: None,
            is_sync: None,
            governance: None,
            is: None,
            is_listener: None,
            is_relisten: None,
            flush_abort: None,
            flushes: 0,
            #[cfg(test)]
            probe: None,
            #[cfg(test)]
            test_files: None,
            #[cfg(test)]
            wrap_files: None,
            #[cfg(test)]
            wrap_index_files: None,
            #[cfg(feature = "test-support")]
            listener: None,
            #[cfg(feature = "test-support")]
            cl_schedule: Default::default(),
            #[cfg(test)]
            before_verify: None,
            #[cfg(test)]
            verified_coins_empty: None,
            #[cfg(test)]
            start_fault: None,
            #[cfg(feature = "test-support")]
            pools: None,
            #[cfg(feature = "test-support")]
            null_mempool: false,
            #[cfg(test)]
            before_pool_attach: None,
            #[cfg(feature = "test-support")]
            max_tip_age: None,
            #[cfg(all(test, feature = "test-support"))]
            panic_w_in_first_flush: false,
            #[cfg(test)]
            before_import: None,
            #[cfg(all(test, debug_assertions))]
            forgive_import_sends: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(all(test, feature = "test-support"))]
            held_import: None,
            #[cfg(test)]
            before_open: None,
            core_import: None,
            import_hooks: None,
            scan_parts: None,
            #[cfg(test)]
            blk_files: None,
            #[cfg(any(all(test, feature = "test-support"), feature = "test-hooks"))]
            panic_v_in_flush: None,
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

    /// A reader of V's block files, from step 7's load.
    pub fn block_files(&self) -> Option<&Arc<dyn BlockFiles + Sync>> {
        self.index_files.as_ref()
    }

    /// The InstantSend parts step 7's attach built.
    pub fn instantsend(&self) -> Option<&IsParts> {
        self.is.as_ref()
    }

    /// Step 7c on a masternode: NetInstantSend's `m_signer`, for the processor, V's conflict half
    /// and a listener rebuilt with it in place of the load's. Nothing is signalled between the
    /// load's verify and step 11's import, so the swap misses no notification.
    // Core: v24.0.2 src/init.cpp:2201
    pub fn attach_is_signer(&mut self, signer: impl Fn() -> IsListenerSigner) {
        let (Some(is), Some(relisten)) = (&self.is, &self.is_relisten) else {
            return;
        };
        is.signer.set(signer());
        let Some((signals, old)) = self.is_listener.take() else {
            return;
        };
        let listener = relisten(Some(signer()));
        signals.unregister(&old);
        signals.register(Arc::clone(&listener));
        self.is_listener = Some((signals, listener));
    }

    #[cfg(test)]
    pub(crate) fn is_listener(&self) -> Option<&(ValidationSignals, Arc<dyn ValidationInterface>)> {
        self.is_listener.as_ref()
    }

    /// Takes NetInstantSend's listener off V's signals: the signer it drives holds the mempool,
    /// whose signals hold the listener.
    fn drop_is_listener(&mut self) {
        if let Some((signals, listener)) = self.is_listener.take() {
            signals.unregister(&listener);
        }
        self.is_relisten = None;
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
    /// node whose loaded active chain has a tip, without a reindex (step 7's `reindex`, or
    /// `-reindex-chainstate`), loads them; otherwise they are stored empty (SP1 D4).
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
        let reindex = self.reindex || args.get_bool_arg("-reindex-chainstate", false)?;
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
        // Core: init.cpp v24.0.2:2260-2267
        if let Some(governance) = &self.governance {
            let height = self
                .chain
                .as_ref()
                .map_or(0, |c| c.handle().view_handle().snapshot().tip.tip_height);
            let height = i32::try_from(height).unwrap_or(i32::MAX);
            governance.load_cache(dir, message_start, load, height)?;
        }
        Ok(())
    }

    pub fn netfulfilled(&self) -> Option<&Arc<NetFulfilledRequestManager>> {
        self.netfulfilled.as_ref()
    }

    pub(crate) fn set_meta(&self, rpc: &crate::rpc::ChainSlot) {
        if let Some(meta) = &self.metaman {
            rpc.set_meta(Arc::clone(meta));
        }
    }

    pub fn metaman(&self) -> Option<&Arc<MasternodeMetaMan>> {
        self.metaman.as_ref()
    }

    /// Step 7's load, once, in Core's order (2b-D18): the chainstate `coins` opens, whose database
    /// holds the block index, and recovery's steps 0–3; the block index from it and
    /// `LoadGenesisBlock` (V's devnet step); then recovery on the loaded index through the load's
    /// subscriber hub (Core's `ReplayBlocks`), and the coins attached to V before anything else
    /// reaches it. `LoadChainTip` then runs at the coins' best block, and the readiness guard (CH3a
    /// D70; G1q from CH7a-4 until CH7b-2, CH7 D3) on that tip and the blocks recovery replayed (2b-D32); then
    /// `VerifyLoadedChainstate` (CH1-node D64), whose init message goes to `ui`. V's startup ABC
    /// waits for [`import`](Self::import). After
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
        params: Arc<ChainSetup>,
        clock: Arc<dyn Clock>,
        abort: Arc<NodeAbort>,
        mut setup: CoinsSetup<T>,
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
        let is_clock = Arc::clone(&clock);
        let seams = NodeSeams {
            clock,
            sporks: Arc::clone(self.sporks.as_ref().expect("step 7a made sporkman")) as _,
            abort,
            ui: notify::chain_ui(&self.logger, args, &self.warnings, &self.block_change)?,
            shutdown: Arc::clone(&self.shutdown) as _,
            stop_at_height: args.get_int_arg("-stopatheight", 0)?,
            // `-consensusxcheck` compares each block V takes from it with its own read (Q13).
            read_ahead: ReadAheadSetup::threaded(setup.xcheck),
            check_block_index: setup.check_block_index,
            #[cfg(feature = "test-support")]
            max_tip_age: self.max_tip_age.unwrap_or(dash_chain::DEFAULT_MAX_TIP_AGE),
            #[cfg(not(feature = "test-support"))]
            max_tip_age: dash_chain::DEFAULT_MAX_TIP_AGE,
        };
        // Core: init.cpp:1966
        self.reindex_arg = args.get_bool_arg("-reindex", false)?;
        // Core opens `blocks/index` first; here it is in the chainstate's database.
        let mut opening = match coins() {
            Ok(opening) => opening,
            Err(e) => return Ok(Err(e)),
        };
        #[cfg(test)]
        if let Some(before) = self.before_open.take() {
            before(&mut opening);
        }
        // The import's S6 to S8, before any record is read: the store holds its reindex before
        // the journal completes, and the scan starts from `blk00000.dat` (CH5-4b, D44).
        if let Some(import) = self.core_import.take() {
            let net = args.get_data_dir_net()?;
            if let Err(e) = import.finish(&net, || opening.request_reindex()) {
                return Ok(Err(OpenError::Incompatible(e)));
            }
        }
        // Core opens the InstantSend database after the evodb, before `LoadChainstate`, wiped on
        // `-reindex` or `-reindex-chainstate`; a failure throws inside `catch_exceptions`.
        // Core: init.cpp:1966-1967, :2022-2034
        let reindex_chainstate = args.get_bool_arg("-reindex-chainstate", false)?;
        // Only a unit test's load may run without the store's opener, its locks in memory.
        #[cfg(test)]
        let open_is = self
            .is_open
            .take()
            .or_else(|| Some(engine_opener(Arc::new(MemoryEngine::new(&IS_SCHEMA)))));
        #[cfg(not(test))]
        let open_is = self.is_open.take();
        let Some(open_is) = open_is else {
            return Ok(Err(OpenError::from(LoadFailure::Store {
                cause: format!("no opener for {}", crate::instantsend::ISDB_DIR),
            })));
        };
        let is_store = match open_is(self.reindex_arg || reindex_chainstate) {
            Ok(store) => store,
            Err(e) => {
                return Ok(Err(OpenError::from(LoadFailure::Store {
                    cause: e.to_string(),
                })));
            }
        };
        // `node.mempool`, after the InstantSend manager and before `LoadChainstate`, on the queue
        // V will signal on.
        // Core: init.cpp:2035-2037
        self.mempool = Some(Arc::new(NodeMempool::new(
            setup.mempool,
            ValidationSignals::default(),
            Arc::clone(&is_clock),
        )));
        // `LLMQContext`'s block processor, between the block tree database and `LoadBlockIndex`.
        // Threads that fail to start throw inside Core's `catch_exceptions`: the exception is
        // logged and the load fails with "Error opening block database".
        // Core: node/chainstate.cpp:157-161; init.cpp:2022-2029, :2088
        let bls_queue = match start_bls_check_threads(&self.logger, args)? {
            Ok(queue) => Arc::new(queue),
            Err(e) => {
                return Ok(Err(OpenError::from(LoadFailure::Store {
                    cause: e.to_string(),
                })));
            }
        };
        // `LLMQContext::qman`, built right after its block processor: its quorum database opens,
        // wiped under `-reindex` or `-reindex-chainstate` as given, never for a stored reindex;
        // `MigrateOldQuorumDB(evoDb)` is not ported (D-row: rdashd refuses a Core data directory,
        // `datadir::refuse_core_dir`, or on regtest moves its evodb aside, `core_import`, so no
        // Core evoDb rows exist); then `q-cache` starts; then
        // `sigman`'s recovered-signature database opens with the same wipe.
        // Core: node/chainstate.cpp:149, :157-161; llmq/context.cpp:19-22,
        // llmq/quorumsman.cpp:40-46
        let wipe_quorums = self.reindex_arg || reindex_chainstate;
        if let Some(Err(cause)) = setup.qman.take().map(|qman| qman(wipe_quorums)) {
            return Ok(Err(OpenError::from(LoadFailure::Store {
                cause,
            })));
        }
        let wipe = Wipe {
            reindex: self.reindex_arg,
            reindex_chainstate,
        };
        let recovery = match opening.open(wipe) {
            Ok(recovery) => recovery,
            Err(e) => return Ok(Err(e)),
        };
        let meta = recovery.meta();
        // Core resumes a reindex its block tree DB records: `LoadBlockIndex` tests `fReindex` on
        // entry, `LoadBlockIndexDB` ORs the flag in inside its `!fReindex` block, which runs to
        // its end, and the first step it then skips is `LoadGenesisBlock`; the caches are cleared
        // and ThreadImport replays the block files.
        // Core: validation.cpp:5337-5338; node/blockstorage.cpp:401-404;
        // node/chainstate.cpp:181-183, :210
        let reindex = chain::Reindex {
            argument: self.reindex_arg,
            stored: meta.reindexing,
        };
        self.reindex = reindex.any();
        self.rebuilds_chainstate = recovery.wiped() && !self.reindex;
        let stored = meta.tip.is_some() || meta.head_blocks.is_some();
        let records = chain::index_records(recovery.block_index());
        let files: Box<dyn BlockFiles> = Box::new(opening.block_files(&recovery));
        #[cfg(test)]
        let files = match self.wrap_files {
            Some(wrap) => wrap(files),
            None => files,
        };
        self.serve_files = Some(Box::new(opening.block_files(&recovery)));
        self.scan_parts =
            Some((Arc::clone(&params), opening.block_files(&recovery).block_at_pos()));
        self.rpc_files = Some(Box::new(opening.block_files(&recovery)));
        let index_files: IndexFiles = Arc::new(opening.block_files(&recovery));
        #[cfg(test)]
        let index_files = match &self.wrap_index_files {
            Some(wrap) => wrap(index_files),
            None => index_files,
        };
        self.index_files = Some(index_files);
        #[cfg(test)]
        {
            self.test_files = Some(Box::new(opening.block_files(&recovery)));
        }
        // V's half of `ResolveBlockConflicts`, installed with the InstantSend parts below.
        let is_conflicts = IsConflictsSlot::default();
        let (mut parts, mut opened) = match chain::open_parts(
            Arc::clone(&params),
            seams,
            records,
            files,
            meta.pruned_files,
            reindex,
        ) {
            Ok(parts) => (
                Some(VParts {
                    is: Box::new(is_conflicts.clone()),
                    ..parts
                }),
                Ok(()),
            ),
            Err(failure) => (None, Err(OpenError::from(failure))),
        };
        let index = parts.as_ref().map(|parts| (parts.chain.reader(), parts.chain.feeds()));
        if let Some(parts) = parts.take_if(|parts| parts.devnet_pending()) {
            // After the index load's own refusals (Core reports them first), before V starts.
            if stored {
                return Ok(Err(OpenError::Incompatible(devnet_lost_index(&params))));
            }
            opened = self.start_chain(parts)?;
        }
        if let (Ok(()), Some((index, feeds))) = (&opened, index) {
            let table = Arc::new(ChainFeedTable::new(feeds));
            let hub = SubscriberHub::with_feeds(table, Arc::new(index.clone()));
            let hub = self.hub.insert(Arc::new(hub));
            match opening.recover(recovery, &index, hub) {
                // Stored first: shutdown flushes the coins even if the attach fails.
                Ok(recovered) => self.chainstate = Some(opening.into_chainstate(recovered)),
                // A refusal in a state where Core is past `InitCoinsCache`
                // (`InitError::after_coins_cache`), so its shutdown flushes run: the chainstate is
                // kept for them, and nothing else reads it (BL-636).
                Err((e, kept)) => {
                    self.chainstate = kept.map(|kept| opening.into_chainstate(*kept));
                    opened = Err(e);
                }
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
        let mut is_listener = None;
        let opened = match opened {
            Ok(()) => {
                self.attach_attempted = true;
                #[cfg(test)]
                if self.start_fault == Some(StartFault::Attach) {
                    let v = self.chain.as_ref().expect("V started").handle();
                    let _ = v.query(|_| panic!("injected attach fault")).recv();
                    return Err(InitFailure::Exception("injected attach fault".to_owned()));
                }
                let chainstate = self.chainstate.as_ref().expect("recovered");
                let hub = self.hub.as_ref().expect("recovered through it");
                let chain = self.chain.as_ref().ok_or_else(|| {
                    InitFailure::Exception("the validation thread has not started".to_owned())
                })?;
                let signals = chain.signals().clone();
                let loading = Arc::clone(chain.handle().loading());
                let sporks = Arc::clone(self.sporks.as_ref().expect("step 7a made sporkman"));
                let is =
                    IsParts::new(is_store, Arc::clone(&sporks), Arc::clone(&loading), is_clock);
                let product_digest = setup
                    .digest_log
                    .take()
                    .map(|file| chainstate.seed_digest(&params, chain.handle().reader(), file))
                    .transpose()?;
                // V's helper, applier and `fx` source alone hold the attempt ledger (PLAN-LK1b
                // §5.3 G1-b): `for_v`'s parts drop with this statement.
                let coins = chainstate
                    .attach_parts(
                        &params,
                        hub,
                        &setup,
                        bls_queue,
                        &signals,
                        product_digest,
                        loading,
                        &is.for_v(),
                    )
                    .map_err(|e| InitFailure::Exception(e.to_string()))?;
                let abort = Arc::clone(self.abort.as_ref().expect("set above"));
                is.install_conflicts(&is_conflicts, chainstate.latch(), Arc::clone(&abort) as _);
                let chainlocks = Arc::new(NodeChainLocks {
                    view: chain.handle().view_handle(),
                    sporks: Arc::clone(&sporks),
                });
                let sync: Arc<dyn SyncView> = match &self.is_sync {
                    Some(sync) => Arc::new(NodeSyncView(Arc::clone(sync))),
                    None => Arc::new(NeverSynced),
                };
                let (listen, reader, latch) =
                    (is.clone(), chain.handle().reader().clone(), chainstate.latch());
                // `mempool_opts.isman` and NetInstantSend's `m_mempool`.
                // Core: v24.0.0 src/init.cpp:2033-2037, :2198
                let mempool = self.mempool.as_ref().map(|pool| {
                    pool.pool()
                        .set_is_hooks(is.accept_hooks(latch.clone(), Arc::clone(&abort) as _));
                    Arc::clone(pool.pool()) as Arc<dyn IsMempool>
                });
                let params = Arc::clone(&params);
                let relisten: IsRelisten = Box::new(move |signer| {
                    listen.listener(
                        &params,
                        reader.clone(),
                        Arc::clone(&sporks),
                        Arc::clone(&chainlocks) as _,
                        Arc::clone(&sync),
                        mempool.clone(),
                        latch.clone(),
                        Arc::clone(&abort) as _,
                        signer,
                    )
                });
                is_listener = Some(relisten(None));
                self.is_relisten = Some(relisten);
                self.is = Some(is);
                // A failed attach is V's `AbortNode`: once V is joined, its sink has requested
                // the shutdown, whose flushes run on the state V handed back (DEC-241).
                let chain = self.chain.as_mut().expect("V started");
                let attached = chain.attach_coins(coins).map_err(|_| {
                    self.exit = chain.stop();
                    OpenError::Interrupted(String::new())
                });
                // DEC-1867: admissions reach V only from here, with its connector and evo state
                // attached, so no mempool turn meets a readiness refusal. Core admits nothing
                // before its P2P and RPC serve, after this load.
                if attached.is_ok() {
                    let pool = self.mempool.as_ref().expect("built before the load");
                    #[cfg(test)]
                    if let Some(before) = self.before_pool_attach.take() {
                        before(pool, chain);
                    }
                    pool.pool()
                        .attach_chain(chain.handle().clone())
                        .map_err(|e| InitFailure::Exception(e.to_string()))?;
                }
                attached
            }
            failed => failed,
        };
        let verify = VerifyLoadOptions {
            check_blocks,
            check_level,
            require_full_verification,
            // The evodb is the chainstate engine's evo column families; recovery's step 5 has
            // refused evo rows without a tip with Core's `:471-473` failure (CH7-notes D71), so
            // `evodb_empty` keeps its default.
            coins_empty: coins_empty(
                reindex,
                reindex_chainstate,
                self.chainstate.as_ref().is_none_or(Chainstate::coins_empty),
            ),
            ..VerifyLoadOptions::default()
        };
        #[cfg(test)]
        {
            self.verified_coins_empty = Some(verify.coins_empty);
        }
        // Core: init.cpp:2100-2112
        let opened = opened.and_then(|()| {
            ui.init_message("Verifying blocks…");
            let chain = self.chain.as_mut().expect("V started");
            #[cfg(test)]
            if let Some(before) = self.before_verify {
                before(chain);
            }
            match chain.verify_loaded(verify) {
                Ok(verified) => verified.map_err(verify_failure).map(|()| {
                    // NetInstantSend's listener hears nothing of VerifyDB's walk: Core registers
                    // it after the load loop, as `AddExtraHandler` does.
                    // Core: init.cpp:2198; net_processing.cpp:1691-1695
                    if let Some(listener) = &is_listener {
                        chain.signals().register(Arc::clone(listener));
                    }
                }),
                // V stopped on an `AbortNode`, as for the attach.
                Err(_) => {
                    self.exit = chain.stop();
                    Err(OpenError::Interrupted(String::new()))
                }
            }
        });
        if opened.is_ok() {
            let signals = self.chain.as_ref().map(|chain| chain.signals().clone());
            self.is_listener = signals.zip(is_listener);
        }
        Ok(opened)
    }

    /// Starts V on `parts`. A load failure V reports as it starts (the devnet step) is init's; a
    /// thread that cannot be spawned is an exception.
    fn start_chain(&mut self, parts: VParts) -> Result<Result<(), OpenError>, InitFailure> {
        #[cfg(test)]
        if self.start_fault == Some(StartFault::Spawn) {
            let e = std::io::Error::other("injected spawn failure");
            return Err(InitFailure::Exception(e.to_string()));
        }
        #[cfg(all(test, feature = "test-support"))]
        let parts = {
            let mut parts = parts;
            if self.held_import.is_some() {
                parts.idx.hold_for_startup_abc();
            }
            parts
        };
        // V's tip steps run through the pool, and V signals on the pool's queue.
        let pool = self.mempool.as_ref().expect("built before the load").pool();
        let parts = VParts {
            mempool: Box::new(PoolSink::new(Arc::clone(pool), Arc::clone(&parts.params))),
            signals: pool.signals().clone(),
            ..parts
        };
        #[cfg(feature = "test-support")]
        let parts = match (&self.pools, self.null_mempool) {
            (Some(pools), _) => VParts {
                mempool: Box::new(pools.clone()),
                ..parts
            },
            (None, true) => VParts {
                mempool: Box::new(dashd_chainstate::chain::NullMempool),
                ..parts
            },
            (None, false) => parts,
        };
        match Chain::start(parts) {
            Err(StartError::Load(failure)) => Ok(Err(failure.into())),
            Err(StartError::Spawn(e)) => Err(InitFailure::Exception(e.to_string())),
            Ok(chain) => {
                if let Some(governance) = &self.governance {
                    governance.bind(chain.handle().clone())?;
                }
                #[cfg(feature = "test-support")]
                {
                    if let Some(listener) = &self.listener {
                        chain.signals().register(listener(chain.handle().reader()));
                    }
                    chain.handle().set_cl_schedule(self.cl_schedule);
                }
                self.chain = Some(chain);
                Ok(Ok(()))
            }
        }
    }

    /// The hooks step 11's `loadblk` thread calls (CH6a's `on_reindex_finished`).
    pub fn set_import_hooks(&mut self, hooks: ImportHooks) {
        self.import_hooks = Some(hooks);
    }

    /// Sets [`ImportHooks::after_mn_init`] without replacing any other import hook
    /// ([`Chain::set_after_mn_init`]).
    #[cfg(feature = "test-hooks")]
    pub fn set_after_mn_init(&mut self, f: Box<dyn FnOnce() + Send>) {
        self.import_hooks.get_or_insert_default().after_mn_init = Some(f);
    }

    /// The active masternode's startup `Init` ([`Chain::set_masternode_startup`]).
    pub fn set_masternode_startup(&mut self, startup: Box<dyn chain::MnStartup>) {
        self.chain.as_mut().expect("step 7 started V").set_masternode_startup(startup);
    }

    /// Step 11: `ThreadImport` sends V's startup ABC, then init waits for genesis or a shutdown.
    // Core: init.cpp:2419-2425, :2448-2451
    pub fn import(&mut self, args: &ArgsManager) -> Result<(), InitFailure> {
        #[cfg(all(test, feature = "test-support"))]
        if let Some(before) = self.held_import.take() {
            before(self.chain.as_ref().expect("step 7 started V"));
        }
        #[cfg(test)]
        if let Some(before) = self.before_import.take() {
            let chain = self.chain.as_ref().expect("step 7 started V");
            let chainstate = self.chainstate.as_ref().expect("step 7 loaded the chainstate");
            let abort = self.abort.as_ref().expect("step 7 loaded the chainstate");
            before(chain.handle(), chainstate.abort_path().latch(), abort);
        }
        #[cfg(all(test, debug_assertions))]
        {
            let extra = self.forgive_import_sends.swap(0, std::sync::atomic::Ordering::Relaxed);
            let chain = self.chain.as_ref().expect("step 7 started V");
            for _ in 0..extra {
                chain.allow_unawaited_send();
            }
        }
        #[cfg(test)]
        if let Some(probe) = &mut self.probe {
            probe(Point::Import);
        }
        let chain = self.chain.as_mut().expect("step 7 started V");
        let stop = args.get_bool_arg("-stopafterblockimport", false)?.then(|| {
            let (logger, shutdown) = (Arc::clone(&self.logger), Arc::clone(&self.shutdown));
            Box::new(move || {
                logger.log_printf("Stopping after block import\n");
                shutdown.start_shutdown();
            }) as Box<dyn FnOnce() + Send>
        });
        let mut hooks = self.import_hooks.take().unwrap_or_default();
        if let Some(pool) = &self.mempool {
            let (pool, shutdown) = (Arc::clone(pool), Arc::clone(&self.shutdown));
            // `LoadMempool` reads no `mempool.dat` yet (MP1b): it records the attempt.
            // Core: validation.cpp:4995-5000
            hooks.load_mempool =
                Some(Box::new(move || pool.set_load_tried(!shutdown.shutdown_requested())));
        }
        if let Some(indexes) = self.deferred_open.take() {
            let abort = Arc::clone(self.abort.as_ref().expect("step 7 loaded the chainstate"));
            hooks.on_import_done = Some(Box::new(move || indexes.open_after_import(&*abort)));
        }
        chain.set_import_hooks(hooks);
        let (params, read) = self.scan_parts.clone().expect("step 7 loaded the chainstate");
        let files: Box<dyn crate::chain::BlkFiles> =
            Box::new(crate::chain::BlocksDir(args.get_blocks_dir_path()?));
        #[cfg(test)]
        let files = self.blk_files.take().unwrap_or(files);
        let loadblock = args.get_args("-loadblock")?.into_iter().map(PathBuf::from).collect();
        let scan = crate::chain::ReindexScan {
            params,
            files,
            read,
            shutdown: Arc::clone(&self.shutdown) as _,
            loadblock,
        };
        chain.import(stop, scan).map_err(|e| InitFailure::Exception(e.to_string()))?;
        let shutdown = Arc::clone(&self.shutdown);
        chain.wait_for_genesis(|| shutdown.shutdown_requested());
        Ok(())
    }

    /// Step 7's end: the index host over the loaded chain with the txindex (`-txindex`, on by
    /// default), the basic filter index (`filters`, from `-blockfilterindex`) and the coin
    /// statistics index (`-coinstatsindex`, off by default) registered,
    /// wiped under Core's `fReindex`; no handles with neither, or with no loaded chainstate.
    /// Each handle serves the retained rows until [`start_indexes`].
    ///
    /// # Errors
    ///
    /// Core's `InitError` when a wipe or a read-only open fails.
    ///
    /// [`start_indexes`]: Self::start_indexes
    // Core: v24.0.2 src/init.cpp:2280-2281, :2308-2309, :2315-2316
    pub fn register_indexes(
        &mut self,
        args: &ArgsManager,
        sizes: &dashd_chainstate::caches::CacheSizes,
        filters: bool,
        driver: dashd_chainstate::subscriber::DriverConfig,
    ) -> Result<IndexHandles, InitFailure> {
        let files = self.index_files.clone();
        let caches = IndexCaches {
            txindex: args.get_bool_arg("-txindex", DEFAULT_TXINDEX)?.then_some(sizes.tx_index),
            filters: filters.then_some(sizes.filter_index),
            coinstats: args
                .get_bool_arg("-coinstatsindex", DEFAULT_COINSTATSINDEX)?
                .then_some(COINSTATS_CACHE),
        };
        let (Some(hub), Some(chain), Some(chainstate), Some(abort), Some(files)) =
            (&self.hub, &self.chain, &self.chainstate, &self.abort, files)
        else {
            return Ok(IndexHandles::default());
        };
        let (params, _) = self.scan_parts.as_ref().expect("step 7 loaded the chainstate");
        let parts = IndexParts {
            data_dir: args.get_data_dir_net()?,
            blocks_dir: args.get_blocks_dir_path()?,
            latch: chainstate.latch(),
            sink: Arc::clone(abort) as _,
            index: Arc::new(chain.handle().reader().clone()),
            files,
            driver,
            chain: chain.handle().reader().clone(),
            params: Arc::clone(params),
        };
        let Some((indexes, handles)) =
            NodeIndexes::register(Arc::clone(hub), self.reindex, parts, caches)?
        else {
            return Ok(IndexHandles::default());
        };
        self.indexes = Some(indexes);
        Ok(handles)
    }

    /// The indexes [`register_indexes`](Self::register_indexes) registered, until their stop.
    pub fn indexes(&self) -> Option<&NodeIndexes> {
        self.indexes.as_ref()
    }

    /// Step 8: Core's `Start` for each index, on init's thread, which holds no node lock and has
    /// submitted no flush. Under `-reindex-chainstate`, or its interrupted wipe resumed, at the
    /// end of step 11's import instead (Core's `g_indexes_ready_to_sync`): an earlier open would
    /// rewind each index towards the partial rebuild's tip.
    ///
    /// # Errors
    ///
    /// Core's `InitError`, such as an unknown locator, or the hub's `WouldDeadlock`.
    // Core: v24.0.0 src/init.cpp:2273-2318
    pub fn start_indexes(&mut self) -> Result<(), InitFailure> {
        let Some(indexes) = &self.indexes else {
            return Ok(());
        };
        if self.rebuilds_chainstate {
            self.deferred_open = Some(indexes.clone());
            return Ok(());
        }
        indexes.open()
    }

    /// Ends the indexes' catch-ups.
    pub fn interrupt_indexes(&self) {
        if let Some(indexes) = &self.indexes {
            indexes.interrupt();
        }
    }

    /// [`Chain::join_import`] on step 11's `loadblk` thread, if V started: startup's ABC has
    /// replied, its flushes done. Whether it ended without a panic (true if V did not start).
    #[cfg(any(test, feature = "test-support"))]
    pub fn join_import(&mut self) -> bool {
        self.chain.as_mut().is_none_or(Chain::join_import)
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

    /// [`Chainstate::crash_record`], while loaded.
    #[cfg(test)]
    pub(crate) fn crash_record(&self) -> Option<&crate::chainstate::CrashRecord> {
        self.chainstate.as_ref().map(Chainstate::crash_record)
    }

    /// The loaded chainstate's node latch.
    pub(crate) fn latch(&self) -> Option<dashd_chainstate::store::GlobalFailStop> {
        self.chainstate.as_ref().map(Chainstate::latch)
    }

    /// The loaded chainstate.
    #[cfg(test)]
    pub(crate) fn chainstate(&self) -> &Chainstate<E, F> {
        self.chainstate.as_ref().expect("a loaded chainstate")
    }

    /// The node's `AbortNode`, from step 7.
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn abort(&self) -> &Arc<NodeAbort> {
        self.abort.as_ref().expect("step 7 loaded the chainstate")
    }

    pub fn chain(&self) -> Option<&Chain> {
        self.chain.as_ref()
    }

    /// The node's mempool, from step 7's load until shutdown resets it: as `Arc<Mempool>`
    /// ([`NodeMempool::pool`]) and as `Arc<dyn MempoolHandle>` ([`NodeMempool::handle`]). Never
    /// call `prioritise` on it from V: it waits behind the queued admissions, which wait on V.
    /// Core v24.0.0 `src/node/context.h:83`, `NodeContext::mempool`.
    pub fn mempool(&self) -> Option<&Arc<NodeMempool>> {
        self.mempool.as_ref()
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
            // The held coinbase work ends against a running V; only then does V stop.
            // Core: init.cpp:347-365 at v24.0.0
            chain.drain_coinbase_jobs();
            // V's state the first time it stops: a failed load's stop kept it.
            if let Some(exit) = chain.stop() {
                self.exit = Some(exit);
            }
            detach_mempool(self.mempool.as_deref(), &self.logger, &self.shutdown);
            #[cfg(test)]
            if let Some(probe) = &mut self.probe {
                probe(Point::VStopped);
            }
            chain.drain_background_callbacks();
        }
        // Core: init.cpp:379-385
        self.force_flush(ShutdownFlush::First);
        // DEC-1156: any of Core's asserts skips `mncache.dat`, also one after an earlier failure:
        // its platform-ban resets are chain effects.
        let asserted = self.chainstate.as_ref().is_some_and(Chainstate::asserted);
        if let Some(mut chain) = self.chain.take() {
            chain.flush_background_callbacks();
        }
        // `~CGovernanceManager`, a loaded store, unless an assert stopped the node (DEC-1156):
        // the store holds chain effects.
        // Core: init.cpp v24.0.2:414
        if let Some(governance) = self.governance.take() {
            governance.stop_effects();
            if !asserted {
                governance.flush();
            }
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
            if let Some(m) = self.metaman.take().filter(|m| m.is_valid() && !asserted) {
                m.store(&dir.join(meta::FILE_NAME), message_start);
            }
        }
        // The indexes stop after the first flush's acks: their final commits.
        // Core: v24.0.0 src/init.cpp:421-441
        if let Some(indexes) = self.indexes.take() {
            indexes.stop();
        }
        // Once the indexes have stopped, the second flush, then `ResetCoinsViews`.
        // Core: init.cpp:449-455
        self.force_flush(ShutdownFlush::Second);
        self.exit = None;
        self.chainstate = None;
        // Core: init.cpp:460
        self.mempool = None;
        // `isman.reset()` closes the IS store: RocksDB syncs its WAL as the last handle drops.
        // Core: init.cpp:463
        self.drop_is_listener();
        self.is = None;
        self.index_files = None;
    }

    /// A start refused before a regtest Core directory's import moved anything: shutdown stores
    /// none of `sporks.dat`, `netfulfilled.dat` and `mncache.dat` (`core_import::HeldStore`).
    pub(crate) fn forget_flat_files(&mut self) {
        self.sporks_file = None;
        self.net_caches = None;
    }

    /// A killed process: V is sent its stop before step 11's `loadblk` thread is joined
    /// ([`Chain::kill`]), so the import runs no further command; nothing is flushed and the sporks
    /// are not stored. The signals thread still runs what is queued as it ends (the listeners are
    /// the harness's).
    #[cfg(any(test, feature = "test-support"))]
    pub fn kill(&mut self) {
        if let Some(mut chain) = self.chain.take() {
            chain.kill();
        }
        // A killed process reports nothing: a worker panic was logged as it unwound.
        if let Some(pool) = self.mempool.take() {
            let _ = pool.pool().detach_chain();
        }
        self.exit = None;
        self.sporks = None;
        self.sporks_file = None;
        self.netfulfilled = None;
        self.metaman = None;
        self.net_caches = None;
        self.governance = None;
        self.chainstate = None;
        self.drop_is_listener();
        self.is = None;
    }

    /// The pool's admission worker once V has stopped: its chain handle dropped, the worker
    /// joined. A worker that panicked fails the shutdown: its error line, then exit status 1.
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
    /// two lines, `FlushStateToDisk`'s `AbortNode` (exit 1), then the flush's failure line.
    ///
    /// Coins that never reached V (the attach not attempted: a refusal before it, or recovery's
    /// refusal in a state where Core is past `InitCoinsCache`, BL-636) get Core's ALWAYS flush
    /// with nothing cached, [`Chainstate::bare_flush`], unless a store failure other than its own
    /// failed sync has latched. A failed space check is reported as W's, and latches nothing, so
    /// the second flush checks again.
    ///
    /// Over a latched failure, the flush does what Core's next flush meets after it ([`Kept`]).
    /// A store's failure stays, as in Core: LevelDB returns a failed log sync's status to every
    /// later write to that database, so Core's next flush fails again with the same text. Here
    /// the store's latch refuses the sync, and the flush reports the latched failure the same
    /// way, a second `AbortNode` included (ND1-D16, DEC-253). The inner text is the engine's (CH2
    /// D10). A failure of the first flush's own (W's space check) is not the store's: Core's
    /// second flush checks again and fails the same way, so the second repeats the first's
    /// `AbortNode` (BL-482). So do the flush's other own failures, which Core does not have: a
    /// failed free-space read, and V's `AbortNode` at the flush's site; one latched before
    /// shutdown goes to the WAL sync, as a store's (BL-213). A failure Core's database does not
    /// keep (a connector's `AbortNode`, a block file's failure, a read's, the simulated crash)
    /// leaves Core's flush a healthy database, which it writes, reporting nothing. Here nothing is
    /// written (CH3 D52, DEC-309), and nothing is reported: the first `AbortNode` stands alone
    /// (BL-407). After one of Core's asserts (V's or W's panic, V's ports of Core's asserts, an EV
    /// attempt's unwind: [`Kept::Assert`]) Core's process has ended, so neither flush runs: here
    /// both return before any output, and a W panic inside the first logs no failure line there
    /// (BL-586, BL-601).
    // Core: validation.cpp:3052-3126, init.cpp:379-386, :449-456, shutdown.cpp:27-38,
    // dbwrapper.cpp:261-268, txdb.cpp:276-285, leveldb/db/db_impl.cc:1228-1244, :650-655,
    // :1328-1331, :1112-1157
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
        if !self.attach_attempted && (!chainstate.latched() || self.bare_latched) {
            let abort = self.abort.as_ref().expect("step 7 loaded the chainstate");
            if let Err(log) = chainstate.bare_flush(&**abort) {
                self.log_flush_failed(&log);
            }
            self.bare_latched = chainstate.latched();
            return;
        }
        let reports = self.abort.as_ref().map(|a| a.reports());
        #[cfg(all(test, feature = "test-support"))]
        if which == ShutdownFlush::First
            && std::mem::take(&mut self.panic_w_in_first_flush)
            && let Some(exit) = &mut self.exit
        {
            exit.panic_w_at_next_step();
        }
        #[cfg(any(all(test, feature = "test-support"), feature = "test-hooks"))]
        if self.panic_v_in_flush.take_if(|w| *w == which).is_some()
            && let Some(exit) = &mut self.exit
        {
            exit.panic_at_next_flush(dashd_chainstate::chain::FlushPanic::BeforeJob);
        }
        // A latched store fails the flush with its first failure, which the WAL sync reports.
        if !chainstate.latched()
            && let Some(exit) = &mut self.exit
            && let Err(f) = exit.force_flush(which)
        {
            // `state.ToString()`: the failure's `Fatal` carries the text of the `AbortNode` that
            // reported it, W's or V's (CH3b D221). The report itself, the first since the flush
            // started, is kept for BL-482's repeat below, which needs its user message too. That
            // first one is the flush's: `Node::shutdown` stopped RPC, the net and the scheduler
            // first, V has stopped, and the signals were drained before the first flush (the
            // signals thread is gone by the second), so no listener runs in between to raise one
            // (a listener's panic is an `AbortNode`), and a failed flush queues no
            // `ChainStateFlushed`.
            // Core: validation.cpp:2245-2249, :3132
            // W's panic inside this flush is one of Core's asserts inside `FlushStateToDisk`: W
            // reported it before `VExit::force_flush` returned (CH3b D222), and Core's flush ends
            // there with no failure line of its own.
            // Core: util/check.h:49-51; init.cpp:383
            if chainstate.latched_kept() == Some(Kept::Assert) {
                return;
            }
            let report = self.abort.as_ref().zip(reports).and_then(|(a, n)| a.report(n));
            debug_assert!(report.as_ref().is_none_or(|r| r.log == f.0), "{report:?} for {f:?}");
            self.logger
                .log_printf(&format!("ForceFlushStateToDisk: failed to flush state ({})\n", f.0));
            self.flush_abort = report;
            return;
        }
        // Core's next flush runs from the top again, so a failed check of its own (the disk space)
        // fails again with the same `AbortNode` and failure line (BL-482). Here the latch holds
        // that failure, as the flush's own, and the flush repeats it; a store's failure goes on to
        // the WAL sync below, as LevelDB keeps it.
        // Core: validation.cpp:3056-3058, :3094-3096, :2245-2249, :3128-3134; shutdown.cpp:27-38
        if let Some(first) = &self.flush_abort
            && chainstate.latched_by_flush(&first.log)
        {
            self.report_flush_abort(&first.log, first.user.as_deref());
            return;
        }
        // A failure Core's database does not keep, or one of Core's asserts: the doc's last
        // paragraph (BL-407, BL-586).
        if matches!(chainstate.latched_kept(), Some(Kept::Nothing | Kept::Assert)) {
            return;
        }
        if let Err(e) = chainstate.force_flush() {
            self.report_failed_sync(&e);
        }
    }

    /// A flush's own `AbortNode` with `log` and `user`, then its failure line.
    // Core: shutdown.cpp:27-38; validation.cpp:3137
    fn report_flush_abort(&self, log: &str, user: Option<&str>) {
        self.abort.as_ref().expect("step 7 loaded the chainstate").abort_node(log, user);
        self.log_flush_failed(log);
    }

    /// The flush's failure line for its `AbortNode` with `log`.
    // Core: validation.cpp:3137
    fn log_flush_failed(&self, log: &str) {
        self.logger.log_printf(&format!("ForceFlushStateToDisk: failed to flush state ({log})\n"));
    }

    /// The failed WAL sync `e` as Core's failed synced write: `HandleError`'s two lines, then the
    /// flush's `AbortNode` and failure line.
    // Core: dbwrapper.cpp:265-267; validation.cpp:3127-3129
    fn report_failed_sync(&self, e: &str) {
        let abort = failed_sync(&self.logger, e);
        self.report_flush_abort(&abort.log, abort.user.as_deref());
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

    /// The loaded chainstate's engine faults, for `RDASHD_TEST_HOOK=abort-after-scan`.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn durable_write_fault(&self) -> Arc<crate::chainstate::Faults> {
        self.chainstate.as_ref().expect("a loaded chainstate").durable_write_fault()
    }

    /// The loaded chainstate's engine, for CH5-5's live-copy seams.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn chainstate_engine(
        &self,
    ) -> Option<&Arc<dashd_chainstate::store::GuardedEngine<Engine>>> {
        self.chainstate.as_ref().map(Chainstate::engine)
    }
}

/// Core's `is_coinsview_empty`: `options.reindex`, the argument alone (OD-CH5-58), not the
/// stored flag; `-reindex-chainstate` has wiped the coins at the open.
// Core: node/chainstate.cpp:415-417 at v24.0.0
fn coins_empty(reindex: chain::Reindex, reindex_chainstate: bool, coins_empty: bool) -> bool {
    reindex.argument || reindex_chainstate || coins_empty
}

/// Init's error for V's pending devnet step over stored coins, whose block index is gone (a
/// stored one has no pending step, 2b-D8). Core's `LoadChainTip` follows `LoadGenesisBlock` there:
/// it loads at a coins tip `LoadGenesisBlock` re-added (genesis or the devnet's genesis block),
/// and otherwise refuses with "Error initializing block database" and the reindex question (a
/// marker alone fails `ReplayBlocks`). V takes a devnet's parts before recovery and has neither
/// path (CH1's): its startup ABC would connect from genesis onto the stored coins and fail
/// fatally, so init refuses every case first, with no reindex question (BL-271 r1).
// Core: validation.cpp:5406-5407; node/chainstate.cpp:272-277
fn devnet_lost_index(params: &ChainSetup) -> String {
    format!(
        "rdashd cannot load {} on stored coins without their block index: a devnet's \
         LoadChainTip after its genesis step is not built (ND1 2b-D8); start the devnet in a new \
         data directory",
        params.network.network_id()
    )
}

/// `VerifyLoadedChainstate`'s failing statuses as the load's (`init.cpp:2115-2133`): a failure
/// asks for a reindex, an interruption exits with its message kept, too small a dbcache is a
/// plain `InitError`. A read that threw in VerifyDB is a failure too, with its text as the cause
/// (BL-787).
fn verify_failure(error: VerifyLoadError) -> OpenError {
    match error {
        VerifyLoadError::Failure(message) => OpenError::Recoverable {
            message: message.to_owned(),
            cause: None,
        },
        VerifyLoadError::Interrupted(message) => OpenError::Interrupted(message.to_owned()),
        VerifyLoadError::InsufficientDbcache(message) => {
            OpenError::InsufficientDbcache(message.to_owned())
        }
        // BL-787: the read's throw, caught as init's `catch_exceptions` catches it, which logs
        // `e.what()` once: the engine's text, as the cause.
        // Core: init.cpp:2022-2028, :2096 at v24.0.0
        VerifyLoadError::Read(what) => OpenError::Recoverable {
            message: OPEN_BLOCK_DB_FAILED.to_owned(),
            cause: Some(what),
        },
    }
}

/// Joins the pool's admission worker once V has stopped; its panic fails the shutdown.
// Core: init.cpp:460 at v24.0.0
fn detach_mempool(pool: Option<&NodeMempool>, logger: &Logger, shutdown: &ShutdownState) {
    if let Some(Err(e)) = pool.map(|pool| pool.pool().detach_chain()) {
        logger.log_printf(&format!("Error: {e}\n"));
        shutdown.set_exit_status(crate::shutdown::EXIT_FAILURE);
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
            OpenError::Interrupted(message) if message == VERIFY_INTERRUPTED
        ));
        assert!(matches!(
            verify_failure(VerifyLoadError::InsufficientDbcache(INSUFFICIENT_DBCACHE)),
            OpenError::InsufficientDbcache(message) if message == INSUFFICIENT_DBCACHE
        ));
        assert!(matches!(
            verify_failure(VerifyLoadError::Read("probe error (Io): read".into())),
            OpenError::Recoverable { message, cause: Some(cause) }
                if message == OPEN_BLOCK_DB_FAILED && cause == "probe error (Io): read"
        ));
    }

    /// OD-CH5-58: `is_coinsview_empty` takes the argument, not the stored flag. A stored reindex
    /// over coins the store still holds verifies them.
    // Core: node/chainstate.cpp:415-417 at v24.0.0
    #[test]
    fn the_coins_are_empty_under_the_argument_not_the_stored_flag() {
        let stored = chain::Reindex {
            argument: false,
            stored: true,
        };
        assert!(!coins_empty(stored, false, false), "a stored reindex over coins");
        assert!(coins_empty(stored, false, true));
        assert!(coins_empty(stored, true, false), "-reindex-chainstate");
        let argument = chain::Reindex {
            argument: true,
            stored: false,
        };
        assert!(coins_empty(argument, false, false), "-reindex");
        assert!(!coins_empty(chain::Reindex::default(), false, false));
    }
}
