//! CH1-node's V in the node: step 7 loads its chain and starts it with its signals thread,
//! step 11 sends startup's `ActivateBestChain` and waits for genesis, and `Shutdown` stops V
//! after the `loadblk` thread and the signals thread after the chainstate flush.
//!
//! **Startup contract (CH1-node N4, D38, D64).** After DEC-196 (2) V holds every command but
//! startup's own (the coins' attach, `VerifyLoaded`, `SetLoading` and control) until startup's
//! `ActivateBestChain` completes, so the driver sends that ABC without waiting on any other.
//! `VerifyLoaded`, Core's `VerifyLoadedChainstate` (the VerifyDB point and its reset), goes
//! before the ABC, as in Core's init; the `-loadblock` import (CH5) must be exempt too or come
//! after it. Until the ABC completes the bounded lane applies no backpressure. On a fresh index
//! V holds nothing, so step 7 waits on V there ([`Chain::start`], a devnet's genesis block).
//!
//! The chain persists through the node's stores (2b-2): `AcceptBlock` writes blocks into blk
//! files ([`NodeBlockFiles`](dashd_chainstate::store::NodeBlockFiles)), and V's flush syncs
//! them and writes the block index into the chainstate's `block_index`. The next start reads
//! the index back ([`index_records`]), recovers the coins and loads the chain tip at theirs.
//! The shutdown flushes run on the state V hands back as it stops ([`VExit`], BL-271), and a
//! devnet restarts on its stored chain as any network does (2b-D8). Blocks arrive through RPC's
//! `submitblock` (DEC-279) until P2P's (S6a).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use dash_chain::{ChainConfig, FailStop, HeaderChain, LoadError, Services, SporkView, StoreError};
use dash_clock::Clock;
use dash_kv::ErrorKind;
use dash_params::ChainParams;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{
    self, AbortSink, AttachCoins, BlockFiles, Blocking, ChainCommand, ChainHandle, CoreChecks,
    Fatal, GuardScope, LoadFailure, NoFlush, NoInstantSend, NodeBlockIndex, NodeJoinHandle,
    NullConnector, NullMempool, ShutdownFlag, SignalsThread, StartError, StoredRecord,
    UiNotifications, VExit, VParts, ValidationSignals, VerifyLoadError, VerifyLoadOptions,
    WaitError,
};

use crate::host;

/// Core's step-11 wait for genesis checks `ShutdownRequested()` this often.
// Core: init.cpp:2563-2571
const GENESIS_WAIT: Duration = Duration::from_millis(500);

/// The header chain's fail-stop latch (D5): a read failure is Core's coins-error callback
/// (`read_failed`, which aborts the process). Nothing reads headers back until N7.
struct ReadFailStop {
    abort: Arc<dyn AbortSink>,
    tripped: AtomicBool,
}

impl FailStop for ReadFailStop {
    fn read_failed(&self, what: &str) {
        self.tripped.store(true, Ordering::SeqCst);
        self.abort.read_failed(what);
    }

    fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }
}

/// The node's side of V's parts.
pub struct NodeSeams {
    pub clock: Arc<dyn Clock>,
    /// The node's spork manager (SP0).
    pub sporks: Arc<dyn SporkView>,
    /// The node's [`NodeAbort`](crate::abort::NodeAbort): V's `AbortNode` and the chain's
    /// read-failure sink.
    pub abort: Arc<dyn AbortSink>,
    /// Hears V's `uiInterface` notifications (`-blocknotify`).
    pub ui: Arc<dyn UiNotifications>,
    /// The node's `ShutdownState` (CH1-node D52): V polls it in the index load, ABC and
    /// InvalidateBlock, and sets it for `-stopatheight` and a listener's panic.
    pub shutdown: Arc<dyn ShutdownFlag>,
    /// `-stopatheight` (0: never).
    // Core: validation.cpp:3752
    pub stop_at_height: i64,
    /// `-checkblockindex` resolved against the network's default (BL-525): V's `CheckBlockIndex`
    /// and the chain's header subset of it both follow it.
    // Core: node/chainstatemanager_args.cpp:22, validation.cpp:6774
    pub check_block_index: bool,
}

/// V's parts from the block index `records` ([`index_records`]): Core's `LoadChainstate` up to
/// the coins. A store with records gets [`chain::open_block_index`]: SB4's record load on a pool
/// of [`LOAD_THREADS`], Core's load rules, the blk-file check (BL-127) and the datadir checks,
/// then `LoadGenesisBlock`. An empty one gets that call's empty-index steps, step for step: the
/// node's shutdown flag polled, `LoadBlockIndexDB`'s line, then `LoadGenesisBlock`, with no pool
/// built and SB4's loaded-index path never entered, so a genesis start faults in none of its
/// pages (PF0 node-startup-genesis, ND1-D5, 2b-D7). The devnet step is V's first act
/// ([`Chain::start`]). `-checkblockindex` is `node`'s.
///
/// # Errors
///
/// Core's init errors, never `AbortNode`: a [`LoadFailure`] from the load, and
/// [`LoadFailure::Failure`] for checks that cannot be built, and [`LoadFailure::Store`] for a
/// pool that cannot.
// Core: node/chainstate.cpp:177-212, node/blockstorage.cpp:378
pub fn open_parts(
    params: Arc<ChainParams>,
    node: NodeSeams,
    records: impl Iterator<Item = StoredRecord>,
    mut files: Box<dyn BlockFiles>,
) -> Result<VParts, LoadFailure> {
    let svc = Services {
        clock: Arc::clone(&node.clock),
        sporks: Arc::clone(&node.sporks),
        fail: Arc::new(ReadFailStop {
            abort: Arc::clone(&node.abort),
            tripped: AtomicBool::new(false),
        }),
    };
    let failure = |e: String| LoadFailure::Failure {
        cause: Some(e),
    };
    // :177, before the first record is read.
    if node.shutdown.shutdown_requested() {
        return Err(LoadFailure::Interrupted);
    }
    let cfg = ChainConfig {
        consistency_checks: node.check_block_index,
        ..ChainConfig::node(&params)
    };
    let mut records = records.peekable();
    let (chain, idx) = if records.peek().is_none() {
        // No entry, no block file to check.
        tracing::info!("Checking all blk files are present...");
        chain::load_genesis_block(Arc::clone(&params), cfg, svc, files.as_mut())
            .map_err(|fatal| failure(fatal.0))?
    } else {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(LOAD_THREADS)
            .thread_name(|i| format!("dashd-index.{i}"))
            .build()
            .map_err(|e| LoadFailure::Store {
                cause: e.to_string(),
            })?;
        let found = chain::LoadedIndex::Stored {
            cfg,
            svc,
            records: Box::new(records),
            pool: &pool,
        };
        chain::open_block_index(Arc::clone(&params), found, files.as_mut(), &*node.shutdown)?
    };
    parts(params, chain, idx, files, node).map_err(|fatal| failure(fatal.0))
}

/// The threads SB4's record load decodes a stored index on (2b-D22). Core's
/// `LoadBlockIndexGuts` reads on init's thread; the pool is built for a stored index only and
/// dropped when its load returns, its workers exiting after. One that cannot be built is an
/// exception in Core's terms: "Error opening block database" with its cause.
pub const LOAD_THREADS: usize = 4;

/// A stored block index's records ([`Opened::block_index`]'s rows) in key order, as SB4's load
/// reads them ([`chain::LoadedIndex::Stored`]). A value is SB4's to decode. A key that is not a
/// block hash, or a read the engine reports as corrupt, is a corrupt index: `LoadBlockIndexGuts`'
/// false, "Error loading block database". Any other read error is the store's: "Error opening
/// block database", as [`chain::open_block_index`] maps them. A raw engine's scan fails only with
/// an engine error: `Codec` is a typed table's decode and `UnknownDatabase` the schema check at
/// open, so neither reaches here, and either would read as the store's. Core ends its scan at a
/// key outside its prefix or too short for a hash (`txdb.cpp:338-339`; the entry's hash comes
/// from its value, :313), because its keys share `blocks/index` with other records. This column
/// family holds index records only, so such a key can only be damage, and it fails the load
/// rather than hiding the records after it. Core never checks its iterator's status, so a failed
/// read ends its index early; here it fails the load (2b-D14). The rows come uncached, as Core's
/// (2b-D19).
///
/// [`Opened::block_index`]: dashd_chainstate::recovery::Opened::block_index
// Core: txdb.cpp:300-344 (LoadBlockIndexGuts)
pub fn index_records(
    rows: impl Iterator<Item = dash_kv::Result<(Vec<u8>, Vec<u8>)>>,
) -> impl Iterator<Item = StoredRecord> {
    rows.map(|read| {
        let (key, value) = read.map_err(|e| {
            LoadError::Read(match e.kind() {
                Some(ErrorKind::Corruption) => StoreError::Corrupt(e.to_string()),
                _ => StoreError::Io(e.to_string()),
            })
        })?;
        let hash = <[u8; 32]>::try_from(key.as_slice()).map_err(|_| {
            LoadError::Read(StoreError::Corrupt(format!(
                "a block_index key of {} bytes, not a block hash",
                key.len()
            )))
        })?;
        Ok((BlockHash::from(hash), value))
    })
}

/// V's parts on a loaded `chain` and its tables, with the initial seams: Core's checks, no
/// connector, no mempool, no flush and no InstantSend. The coins' attach replaces the connector
/// and the flush ([`Chain::attach_coins`]).
pub fn parts(
    params: Arc<ChainParams>,
    chain: HeaderChain,
    idx: NodeBlockIndex,
    files: Box<dyn BlockFiles>,
    node: NodeSeams,
) -> Result<VParts, Fatal> {
    // One `-checkblockindex` for V and the chain's header subset of it (BL-525).
    debug_assert_eq!(chain.config().consistency_checks, node.check_block_index);
    Ok(VParts {
        checks: Box::new(CoreChecks::new(Arc::clone(&params))?),
        check_block_index: node.check_block_index,
        // CH8 loads the block index's `prunedblockfiles` flag (`node/blockstorage.cpp:393`).
        have_pruned: false,
        // The readiness guard's scope from the network (CH3 §2.17).
        guard: GuardScope::of(&params),
        params,
        chain,
        idx,
        files,
        connector: Box::new(NullConnector),
        mempool: Box::new(NullMempool),
        flush: Box::new(NoFlush),
        is: Box::new(NoInstantSend),
        ui: node.ui,
        abort: node.abort,
        shutdown: node.shutdown,
        signals: ValidationSignals::default(),
        clock: node.clock,
        stop_at_height: node.stop_at_height,
    })
}

/// `SetLoading` for `fImporting` alone.
fn importing(on: bool) -> ChainCommand {
    ChainCommand::SetLoading {
        importing: Some(on),
        reindex: None,
    }
}

/// What `loadblk` sends V once `ThreadImport`'s ABC has replied.
trait LoadblkV {
    fn send(&self, cmd: ChainCommand);
    fn initialize_current_block_tip(&self);
}

impl LoadblkV for ChainHandle {
    fn send(&self, cmd: ChainCommand) {
        let _ = ChainHandle::send(self, cmd);
    }

    fn initialize_current_block_tip(&self) {
        let _ = ChainHandle::initialize_current_block_tip(self).recv();
    }
}

/// `loadblk`'s tail in Core's order (CH7-notes D62): `fImporting` cleared as `CImportingNow`
/// ends, MP1's `LoadMempool` slot, then `InitializeCurrentBlockTip`, whose `ibd` V reads as it
/// serves it, so a tip the import made recent is out of IBD.
// Core: node/blockstorage.cpp:960-961, init.cpp:2460-2473
fn after_import(v: &dyn LoadblkV) {
    v.send(importing(false));
    // MP1: `LoadMempool` (blockstorage.cpp:961).
    v.initialize_current_block_tip();
}

/// V, its signals thread and step 11's `loadblk` thread.
pub struct Chain {
    v: ChainHandle,
    join: Option<NodeJoinHandle<VExit>>,
    signals: Option<SignalsThread>,
    /// The node's signals: the queue the signals thread runs (drained before the flush), and
    /// the listeners V calls, `submitblock`'s catcher among them.
    queue: ValidationSignals,
    import: Option<JoinHandle<()>>,
    /// V took [`attach_coins`](Self::attach_coins).
    attached: bool,
}

impl Chain {
    /// Starts the signals thread, then V on `parts`, and awaits V's first act: on a fresh
    /// devnet index, `LoadGenesisBlock`'s devnet step (D48). No command is sent to V.
    ///
    /// # Errors
    ///
    /// [`StartError::Load`] when the devnet step fails, Core's init error; V has stopped, after
    /// its `AbortNode` if `AcceptBlock` raised one. The signals thread is stopped too.
    // Core: validation.cpp:5409-5417
    pub fn start(parts: VParts) -> Result<Chain, StartError> {
        let queue = parts.signals.clone();
        let signals = queue.start(Arc::clone(&parts.abort))?;
        match chain::start(parts) {
            Ok((v, join)) => Ok(Chain {
                v,
                join: Some(join),
                signals: Some(signals),
                queue,
                import: None,
                attached: false,
            }),
            Err(e) => {
                let _ = signals.stop();
                Err(e)
            }
        }
    }

    pub fn handle(&self) -> &ChainHandle {
        &self.v
    }

    /// Step 7's attach, between the coins and startup's ABC (CH1-node D51): V started with no
    /// connector and no flush, so the devnet step flushed nothing (Core's NONE flush while
    /// `CanFlushToDisk()` is false). V takes the attach once, before any ABC, even under D38's
    /// hold. The node sends CH3a's `CoreConnector` and CH2b b4's `NodeFlush`, built over the
    /// coins by `Chainstate::attach_parts`; tests send stand-ins.
    ///
    /// # Errors
    ///
    /// The reply disconnects when V stops on an `AbortNode`, perhaps before its sink has
    /// requested the shutdown; [`stop`](Self::stop) joins V after it has.
    // Core: validation.cpp:4853-4855
    pub fn attach_coins(&mut self, coins: AttachCoins) -> Result<(), WaitError> {
        let attached = self.v.attach_coins(coins).recv();
        self.attached = attached.is_ok();
        attached
    }

    /// Core's `VerifyLoadedChainstate`, after the attach and before step 11's ABC: V's
    /// `VerifyLoaded` (CH1-node-LR), exempt from D38's hold like that ABC.
    ///
    /// # Errors
    ///
    /// The reply disconnects when V stops on an `AbortNode`, as for the attach.
    // Core: init.cpp:2101-2108, node/chainstate.cpp:412-479
    pub fn verify_loaded(
        &self,
        options: VerifyLoadOptions,
    ) -> Result<Result<(), VerifyLoadError>, WaitError> {
        self.v.verify_loaded(options).recv()
    }

    /// Step 11's `ThreadImport`: startup's `ActivateBestChain`, sent here as V's first command
    /// (the module's contract), then awaited on the `loadblk` thread, with `fImporting` set
    /// around it (`CImportingNow`; `SetLoading` is exempt from the hold). Core's ABC returns
    /// `false` only after an `AbortNode`, and its `ThreadImport` then aborts again with "Failed
    /// to connect best block (state)"; V's abort stops it and disconnects the reply instead, so
    /// the node aborts once, with V's message.
    ///
    /// The thread then sends `InitializeCurrentBlockTip` (CH7 §6.3 step 6), as Core's `loadblk`
    /// calls it once `ThreadImport` returns; V reads the tip and `ibd` as it serves it, after
    /// `fImporting` is cleared. MP1's `LoadMempool` goes before it.
    // Core: init.cpp:2455-2473, node/blockstorage.cpp:876-892, :944-961
    pub fn import(&mut self) -> std::io::Result<()> {
        debug_assert!(self.attached, "step 7 attaches the coins before startup's ABC");
        let _ = self.v.send(importing(true));
        let abc = self.v.activate_best_chain();
        let v = self.v.clone();
        let import = host::trace_thread("loadblk", move || {
            let _ = abc.recv();
            after_import(&v);
        });
        // A thread that never ran leaves nothing importing.
        self.import = Some(import.inspect_err(|_| {
            let _ = self.v.send(importing(false));
        })?);
        Ok(())
    }

    /// Core's wait for genesis: until V has a tip (one loaded or one connected) or
    /// `shutdown_requested`. V publishes its view before it sets the tip watch, so a connect
    /// wakes the wait.
    // Core: init.cpp:2419-2425, :2563-2574
    pub fn wait_for_genesis(&self, shutdown_requested: impl Fn() -> bool) {
        let watch = self.v.tip_watch();
        while !self.v.view().snap.has_tip() && !shutdown_requested() {
            // A marked thread may not wait on V (LO-9); init's is not, but it would poll.
            if watch.wait_changed(None, GENESIS_WAIT).is_err() {
                std::thread::sleep(GENESIS_WAIT);
            }
        }
    }

    /// Stops V once the `loadblk` thread is done with it: Core's `m_load_block.join()` sees
    /// `ShutdownRequested()` end the import's ABC. Its `AbortNode`, if any, already went to the
    /// sink. Returns V's state the first time, unless V panicked.
    // Core: init.cpp:355
    pub fn stop(&mut self) -> Option<VExit> {
        self.v.shutdown();
        if let Some(import) = self.import.take() {
            let _ = import.join();
        }
        self.join.take()?.join().ok()?.ok()
    }

    /// Core's `FlushBackgroundCallbacks` before the chainstate flush: after [`stop`](Self::stop),
    /// the queued signals run and the thread keeps going, so the flush starts after them, with a
    /// listener's `AbortNode` already recorded. A queue that closed on a panic has nothing to
    /// drain.
    // Core: init.cpp:360
    pub fn drain_background_callbacks(&self) {
        if self.signals.is_some() {
            let drained = self.queue.sync(&mut Blocking);
            // A marked thread would skip the drain, and an abort in it could land after a flush.
            debug_assert!(!matches!(drained, Err(WaitError::WaitOnVFromNoWaitTask)));
        }
    }

    /// Core's `FlushBackgroundCallbacks` after the chainstate flush: the signals the flush queued
    /// run, then the thread ends.
    // Core: init.cpp:390
    pub fn flush_background_callbacks(&mut self) {
        if let Some(signals) = self.signals.take() {
            let _ = signals.stop();
        }
    }

    /// V's signal queue: P3's listener (step 7c) and submitblock's catcher register on it;
    /// O2b's NodeSut reads it in tests.
    pub fn signals(&self) -> &ValidationSignals {
        &self.queue
    }
}

impl Drop for Chain {
    fn drop(&mut self) {
        self.stop();
        self.flush_background_callbacks();
    }
}

#[cfg(test)]
pub(crate) mod tests;
