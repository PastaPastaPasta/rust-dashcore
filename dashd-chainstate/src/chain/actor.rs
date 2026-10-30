//! V: one thread owning `&mut HeaderChain` and [`NodeBlockIndex`] (§2), serving [`ChainCommand`]s
//! by Core lock class.
//!
//! - **Two lanes** (DEC-192): unmarked senders use the bounded lane and its backpressure; a
//!   sender carrying LO-9's no-wait marker uses the unbounded marked lane, so its post never
//!   blocks on V. V serves both under the same rules, FIFO within each lane, with no order
//!   between them (as between `cs_main` waiters).
//! - **Idle**, V runs `deferred` first, then the lanes, one command at a time.
//! - **Release points** are where Core releases `cs_main` inside an `m_chainstate_mutex`
//!   operation (the top of each ABC outer iteration and `InvalidateBlock` iteration). V takes the
//!   commands queued at that moment (each lane's `len()`), runs class R and M ones to
//!   completion, and sends class C commands and class C tails to the back of `deferred` (§2.4,
//!   R6). V's waits for the signals queue are release-point waits ([`VWait`]).
//! - **After DEC-196 (2)** (D38), V holds every command but startup's own until startup's ABC
//!   completes, then serves the held ones first, in arrival order. A startup driver must send
//!   that ABC without waiting on any other command; the bounded lane's backpressure is off
//!   until then.
//! - **Every class M scope ends with [`V::scope_end`]**, success or failure, before its reply.
//! - A [`Fatal`] is Core's `AbortNode`: V reports it to the [`AbortSink`], sets the shutdown
//!   flag and stops; outstanding replies disconnect. A panic on V does the same, then unwinds.
//!   V's reaper drops what is still queued and whatever is sent later, so no caller waits on a
//!   stopped V; V does not start without it. A reply V sent before the abort stays readable:
//!   its command completed and its effects stand, as a call that returned before Core's
//!   `AbortNode` keeps its result. V stops where Core's `AbortNode` reports (CH3b D213): as the
//!   `Fatal` leaves ABC, `InvalidateBlock` or a ChainLock's mark (D214), before their callers log,
//!   and as a command run at a release point fails, after which the class C operation it
//!   interrupted ends as Core's does.

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(feature = "test-support")]
use std::sync::{Mutex, PoisonError};

use arc_swap::ArcSwap;
use crossbeam_channel::{RecvError, Sender, TryRecvError};
use dash_chain::{ChainReader, HeaderBatch, HeaderChain, HeadersResult, Notify, Parker, PeerId};
use dash_clock::{Clock, NodeTime};
use dash_consensus::coins::stats::CoinStatsHashType;
use dash_consensus::{Block, BlockId};
use dash_num::Hash256;
use dash_params::ChainParams;
use dash_primitives::BlockHash;

use super::check::check_block_index;
use super::command::{
    AttachCoins, BlockNotFound, BlockReply, BlockResult, BlockRpcReply, BlockState, ChainCommand,
    ChainQuery, Class, IsResolution, NotTested, QueryDeployments,
};
use super::deployments::ChainDeployments;
use super::flush::{AbortCause, Bell, FlushBell, FlushHost, FlushSite, FlushStateMode, take_ring};
use super::index::NodeBlockIndex;
use super::load::LoadFailure;
use super::locks::{ChainLockSig, ClIntakeReply, ClReply, ClSigs, ClState, ClTrigger};
use super::read_ahead::{ReadAhead, ReadAheadSetup};
use super::reply::{
    ActorReply, NodeJoinHandle, ParkDrops, ReleaseParked, ReplySender, WaitError, no_wait_marked,
    panic_message, release_parked, reply, spawn_thread,
};
use super::seams::{
    AbortSink, BlockChecks, BlockConnector, BlockFiles, Fatal, GuardScope, IsConflicts,
    MempoolSink, ShutdownFlag, UiNotifications,
};
use super::signals::{Barrier, ServiceWait, ValidationSignals};
use super::verify::{VerifyDbArgs, VerifyDbResult, VerifyLoadError, VerifyLoadOptions};
use super::view::{ChainView, ChainViewHandle, LoadingFlags, TipWatch};
use crate::coins::{CoinsDump, CoinsScan, CoinsScanJob, CoinsStatsJob};

/// A lane's receiving end: only V and its reaper hold one.
#[expect(clippy::disallowed_types, reason = "V's own lanes; V carries no marker")]
type Lane = crossbeam_channel::Receiver<ChainCommand>;

/// What V's idle select took.
enum Next {
    Command(ChainCommand),
    /// W rang: its events wait for V's flush side (BL-123).
    Bell,
}

/// V's two lanes, bounded then marked (DEC-192), each served until its senders are gone and it
/// is empty: a disconnected lane can still hold commands, and must not end the other's.
#[derive(Clone)]
struct Lanes {
    rx: [Lane; 2],
    live: [bool; 2],
}

/// What [`Lanes::select`] took.
enum Selected {
    /// A waiter's receiver.
    Until(Result<(), RecvError>),
    /// A ring of W's bell.
    Bell,
    /// A command from either lane.
    Command(ChainCommand),
    /// A lane found disconnected and empty, now no longer live, or W's bell found disconnected,
    /// now dropped.
    Closed,
}

impl Lanes {
    /// Blocks until a waiter's receiver, W's `bell` (if any) or a live lane is ready, and takes
    /// it. `select!` keeps its operations in an array on V's stack (BL-453): on a channel V has
    /// parked on before, a wait allocates nothing (its waker's list keeps its capacity), while a
    /// new one (a `sync` barrier's) grows that list on V's first park. An absent receiver and a
    /// lane no longer live are each `never()`, which is never ready: a disconnected receiver is
    /// always ready, so one left in would spin. Like `Select::select`, it picks at random among
    /// the ready operations.
    #[expect(clippy::disallowed_types, reason = "a waiter's receiver, read by V")]
    #[expect(
        clippy::disallowed_methods,
        reason = "V's wait on its lanes, W's bell and a waiter's receiver"
    )]
    fn select(
        &mut self,
        until: Option<&crossbeam_channel::Receiver<()>>,
        bell: &mut Option<Bell>,
    ) -> Selected {
        #[cfg(test)]
        tests::LANE_SELECTS.with(|n| n.set(n.get() + 1));
        let (absent, dead) = (crossbeam_channel::never(), crossbeam_channel::never());
        let [rx0, rx1] = [0, 1].map(|i| {
            if self.live[i] {
                &self.rx[i]
            } else {
                &dead
            }
        });
        let (i, r) = crossbeam_channel::select! {
            recv(until.unwrap_or(&absent)) -> r => return Selected::Until(r),
            recv(bell.as_ref().unwrap_or(&absent)) -> r => {
                if r.is_ok() {
                    return Selected::Bell;
                }
                *bell = None;
                return Selected::Closed;
            }
            recv(rx0) -> r => (0, r),
            recv(rx1) -> r => (1, r),
        };
        match r {
            Ok(cmd) => Selected::Command(cmd),
            Err(_) => {
                self.live[i] = false;
                Selected::Closed
            }
        }
    }

    /// The next command from either lane, or a ring of `bell`; `None` once both lanes are
    /// disconnected and empty. A bell whose sender is gone (no W, or W has ended) is dropped:
    /// a disconnected receiver is always ready.
    fn recv(&mut self, bell: &mut Option<Bell>) -> Option<Next> {
        while self.live.contains(&true) {
            match self.select(None, bell) {
                Selected::Command(cmd) => return Some(Next::Command(cmd)),
                Selected::Bell => return Some(Next::Bell),
                Selected::Closed => {}
                Selected::Until(_) => unreachable!("no waiter's receiver is in the select"),
            }
        }
        None
    }
}

/// V's affinity proof (EV addendum §2.6; CH3 §3.2, D19): zero-sized and stateless, `!Send` and
/// `!Sync`. V's command loop makes exactly one, on V's thread as it starts, and lends it only
/// into the connector's calls; it is never handed out. Its constructor is private to this
/// module, with no test or feature-gated twin, so nothing else can make one:
///
/// ```compile_fail,E0624
/// let v = dashd_chainstate::chain::VContext::new(); // private to V's loop module
/// ```
///
/// ```compile_fail,E0451
/// let v = dashd_chainstate::chain::VContext { _v_only: std::marker::PhantomData };
/// ```
///
/// Nor can one, or a borrow of one, leave V's thread:
///
/// ```compile_fail,E0277
/// fn on_another_thread<T: Send>() {}
/// on_another_thread::<&mut dashd_chainstate::chain::VContext>();
/// ```
///
/// ```compile_fail,E0277
/// fn shared<T: Sync>() {}
/// shared::<dashd_chainstate::chain::VContext>();
/// ```
///
/// Code outside V takes it only as a borrow V lends. This passing twin keeps the four cases
/// above honest: stable rustdoc checks only that they fail, not their error codes, so it shows
/// that the path and the scaffolding compile and each case fails on its one marked line:
///
/// ```
/// fn on_another_thread<T: Send>() {}
/// fn shared<T: Sync>() {}
/// on_another_thread::<&mut u8>();
/// shared::<u8>();
/// fn connect(v: &mut dashd_chainstate::chain::VContext) {
///     let _ = v;
/// }
/// ```
#[derive(Debug)]
pub struct VContext {
    _v_only: PhantomData<*const ()>,
}

impl VContext {
    /// V's one context. Only [`spawn_v`]'s thread calls it, before V's loop starts.
    fn new() -> VContext {
        VContext {
            _v_only: PhantomData,
        }
    }
}

/// V's bounded lane's capacity (A1 §2.10).
pub const COMMAND_CAPACITY: usize = 1_024;

/// What V is built from: the loaded (or genesis) chain, its rebuilt tables and the seams.
pub struct VParts {
    pub params: Arc<ChainParams>,
    pub chain: HeaderChain,
    /// [`NodeBlockIndex::load`] of `chain`, or new tables for a fresh genesis chain.
    pub idx: NodeBlockIndex,
    pub checks: Box<dyn BlockChecks>,
    pub files: Box<dyn BlockFiles>,
    /// `ConnectTip`'s read-ahead over `files` (CH4-4).
    pub read_ahead: ReadAheadSetup,
    pub connector: Box<dyn BlockConnector>,
    pub mempool: Box<dyn MempoolSink>,
    pub flush: Box<dyn FlushSite>,
    /// InstantSend's half of its block conflicts (D42).
    pub is: Box<dyn IsConflicts>,
    pub ui: Arc<dyn UiNotifications>,
    pub abort: Arc<dyn AbortSink>,
    /// The node's shutdown flag (D52): ND1's `ShutdownState`, or a
    /// [`LocalShutdown`](super::LocalShutdown).
    pub shutdown: Arc<dyn ShutdownFlag>,
    pub signals: ValidationSignals,
    /// The node clock (mockable): `IsInitialBlockDownload`'s and the checks' `now`.
    pub clock: Arc<dyn Clock>,
    /// Core `-checkblockindex`.
    pub check_block_index: bool,
    /// Core `m_blockman.m_have_pruned` as `LoadBlockIndexDB` reads it from the block index's
    /// `prunedblockfiles` flag (`node/blockstorage.cpp:393`): false for a new chain. CH8 loads it
    /// for the node (CH3a D170).
    pub have_pruned: bool,
    /// Core `-stopatheight` as `GetIntArg` returns it (0: off; -1 stops at genesis).
    pub stop_at_height: i64,
    /// The readiness guard's scope (CH3 §2.17): [`GuardScope::of`] the network, except where a
    /// test injects it.
    pub guard: GuardScope,
}

impl VParts {
    /// Whether V starts on a devnet's pending genesis step (D18): `LoadGenesisBlock` made this
    /// chain new. A stored chain has none, so its load runs `LoadChainTip` as on any network
    /// (ND1 2b-D8, DEC-613).
    pub fn devnet_pending(&self) -> bool {
        self.idx.devnet_pending()
    }
}

/// Which of Core's two shutdown flushes [`VExit::force_flush`] runs.
// Core: init.cpp:379-385, :449-456
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownFlush {
    /// Before `FlushBackgroundCallbacks`: its `ChainStateFlushed` reaches the listeners.
    First,
    /// After the indexes stop: its `ChainStateFlushed` is dropped, as Core's lands in a queue
    /// that never runs again (ND1 2b-D3).
    Second,
}

/// What V hands back when it stops: its state, for the shutdown flushes and for tests. Its flush
/// side comes with it, the coins cache and W included, until it drops (Core's
/// `ResetCoinsViews`), unless the store latch had tripped, one of Core's asserts stopped V or an
/// SB4 contract failure did: then nothing more is written, and the flush side dropped on V's
/// thread.
pub struct VExit {
    pub chain: HeaderChain,
    pub idx: NodeBlockIndex,
    /// Core `m_blockman.m_have_pruned`.
    pub have_pruned: bool,
    /// The `AbortNode` that stopped V, if one did.
    pub fatal: Option<Fatal>,
    flush: Option<Box<dyn FlushSite>>,
    reader: ChainReader,
    signals: ValidationSignals,
    clock: Arc<dyn Clock>,
    loading: Arc<LoadingFlags>,
    abort: Arc<dyn AbortSink>,
}

impl VExit {
    /// Core's `ForceFlushStateToDisk` at shutdown (BL-271, DEC-613): an ALWAYS flush of V's
    /// state, waited for, with V stopped, as `PrepareShutdown` runs it with `m_load_block`
    /// joined, after an `AbortNode` too (DEC-241). Its `ChainStateFlushed` goes to the signals
    /// queue for the [first](ShutdownFlush::First) only. A failure is reported as V reports one
    /// (L-3), then returned, after W's report if it tripped the latch: the flush side then drops,
    /// W's thread joined. Without the flush side nothing is flushed: after one of Core's asserts
    /// (D62a) Core's process has ended, after an SB4 contract failure nothing past V's last flush
    /// is written (BL-271 r1 m-2), and a tripped latch fails every write with its first failure,
    /// which the caller reports.
    ///
    /// # Errors
    ///
    /// The flush's [`Fatal`], reported.
    // Core: init.cpp:379-385, :449-456; validation.cpp:3128
    pub fn force_flush(&mut self, which: ShutdownFlush) -> Result<(), Fatal> {
        let Some(flush) = self.flush.as_deref_mut() else {
            return Ok(());
        };
        let (now, loading) = (self.clock.node_time(), self.loading.any());
        let host =
            FlushHost::new(&mut self.chain, &mut self.idx, &mut self.have_pruned, now, loading);
        let signals = &self.signals;
        let flushed = host.lend(
            flush,
            &self.reader,
            |q| {
                if which == ShutdownFlush::First {
                    signals.enqueue(q);
                }
            },
            |f, host| f.flush(FlushStateMode::Always, None, host),
        );
        let Err(f) = flushed else {
            return Ok(());
        };
        let latched = flush.abort(
            AbortCause::AbortNode,
            &mut FlushHost::new(
                &mut self.chain,
                &mut self.idx,
                &mut self.have_pruned,
                now,
                loading,
            ),
        );
        if f.reported_by_v(latched) {
            self.abort.abort_node(&f.0, None);
        }
        if latched {
            // Nothing more is written. W may have tripped the latch and not yet reported, so the
            // flush side drops here, W's thread joined: its report comes before the caller's
            // line, as Core's `AbortNode` precedes "failed to flush state". W's panic was
            // reported as its thread ended (M2); here it ends as V's does at its join.
            let flush = self.flush.take();
            let _ = panic::catch_unwind(AssertUnwindSafe(|| drop(flush)));
        }
        Err(f)
    }
}

/// The handle every other thread holds.
#[derive(Clone)]
pub struct ChainHandle {
    tx: Sender<ChainCommand>,
    /// DEC-192's unbounded marked lane.
    marked: Sender<ChainCommand>,
    marked_peak: Arc<AtomicUsize>,
    view: Arc<ArcSwap<ChainView>>,
    tip_watch: Arc<TipWatch>,
    reader: ChainReader,
    loading: Arc<LoadingFlags>,
    /// V's own stop latch (D52), which [`ChainHandle::shutdown`] sets for ABC's check.
    stopping: Arc<AtomicBool>,
    /// The node's flag.
    node_shutdown: Arc<dyn ShutdownFlag>,
    clock: Arc<dyn Clock>,
    /// [`ChainHandle::set_between_stores`]'s hook, which V runs.
    #[cfg(feature = "test-support")]
    between_stores: Arc<Mutex<Option<BetweenStores>>>,
}

/// A hook V runs between a release's two stores (CH1-node D70, P3-notes D122 (3)).
#[cfg(feature = "test-support")]
pub type BetweenStores = Arc<dyn Fn() + Send + Sync>;

/// V has stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("V has stopped")]
pub struct Stopped;

impl ChainHandle {
    /// Has V run `hook` between each release's two stores, or nothing with `None` (CH1-node
    /// D70, P3-notes D122 (3)): after SB4's snapshot store, `Hold::release`'s or
    /// `enforce_block`'s, and before the `ChainView` store, on V's thread. A reader there sees the
    /// new tip in SB4's tip cell and the old one in the view. V waits while `hook` runs.
    #[cfg(feature = "test-support")]
    pub fn set_between_stores(&self, hook: Option<BetweenStores>) {
        *self.between_stores.lock().unwrap_or_else(PoisonError::into_inner) = hook;
    }

    /// Sends a command. From a marked task it goes to the marked lane and never blocks
    /// (DEC-192); otherwise it blocks while the bounded lane is full.
    pub fn send(&self, cmd: ChainCommand) -> Result<(), Stopped> {
        if !no_wait_marked() {
            return self.tx.send(cmd).map_err(|_| Stopped);
        }
        self.marked.send(cmd).map_err(|_| Stopped)?;
        self.marked_peak.fetch_max(self.marked.len(), Ordering::Relaxed);
        Ok(())
    }

    /// The marked lane's depth now (DEC-192 (4): the metric).
    pub fn marked_lane_depth(&self) -> usize {
        self.marked.len()
    }

    /// The marked lane's greatest depth seen by a send since V started.
    pub fn marked_lane_peak(&self) -> usize {
        self.marked_peak.load(Ordering::Relaxed)
    }

    /// The bounded lane, whatever the marker: DEC-192's negative control.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn bounded_lane(&self) -> &Sender<ChainCommand> {
        &self.tx
    }

    /// `ProcessNewBlockHeaders`.
    pub fn process_headers(
        &self,
        batch: HeaderBatch,
        from: Option<PeerId>,
    ) -> ActorReply<HeadersResult> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::ProcessHeaders {
            batch,
            from,
            reply: tx,
        });
        rx
    }

    /// `ProcessNewBlock`, replying after the block's ABC (Core's return) or, with
    /// `activated = false`, when the accept scope ends.
    pub fn process_block(
        &self,
        block: Arc<Block>,
        force: bool,
        from: Option<PeerId>,
        activated: bool,
    ) -> ActorReply<BlockResult> {
        self.submit_block(block, force, from, activated, false)
    }

    /// [`Self::process_block`] for a block whose merkle root the caller has checked
    /// (`merkle_checked`, A-4b): V's `CheckBlock` skips that pass, as Core's does for
    /// `m_checked_merkle_root`.
    pub fn submit_block(
        &self,
        block: Arc<Block>,
        force: bool,
        from: Option<PeerId>,
        activated: bool,
        merkle_checked: bool,
    ) -> ActorReply<BlockResult> {
        let (tx, rx) = reply();
        let hash = block.hash();
        let reply = if activated {
            BlockReply::Activated(tx)
        } else {
            BlockReply::Accepted(tx)
        };
        let _ = self.send(ChainCommand::ProcessBlock {
            block,
            hash,
            force,
            from,
            merkle_checked,
            reply: Some(reply),
        });
        rx
    }

    /// A class R read on V.
    pub fn query<T: Send + 'static>(
        &self,
        f: impl FnOnce(&ChainQuery<'_>) -> T + Send + 'static,
    ) -> ActorReply<T> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::Query(Box::new(move |q| tx.send(f(q)))));
        rx
    }

    /// Core `ActivateBestChain(state)` with no block (init, the RPCs).
    pub fn activate_best_chain(&self) -> ActorReply<()> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::ActivateBestChain {
            reply: Some(tx),
        });
        rx
    }

    /// The coins side, once the coins open after `LoadGenesisBlock` and before init's ABC (D51).
    /// V spawns on [`NullConnector`](super::NullConnector) and [`NoFlush`](super::NoFlush) for
    /// it, and drops them unstopped. A second attach, or one after a class C operation, is an
    /// `AbortNode`, and the reply disconnects.
    pub fn attach_coins(&self, coins: AttachCoins) -> ActorReply<()> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::AttachCoins {
            coins,
            reply: tx,
        });
        rx
    }

    /// Core `VerifyLoadedChainstate` (CH1-node-LR, D64): startup sends it after
    /// [`attach_coins`](Self::attach_coins) and before init's `ActivateBestChain`. It is exempt from
    /// D38's hold, as that ABC is.
    pub fn verify_loaded(
        &self,
        options: VerifyLoadOptions,
    ) -> ActorReply<Result<(), VerifyLoadError>> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::VerifyLoaded {
            options,
            reply: tx,
        });
        rx
    }

    /// Core `CVerifyDB::VerifyDB(level, depth)` on the active chainstate under `cs_main`
    /// (`verifychain`, `rpc/blockchain.cpp:1436-1440`).
    pub fn verify_db(&self, args: VerifyDbArgs) -> ActorReply<VerifyDbResult> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::VerifyDb {
            args,
            reply: tx,
        });
        rx
    }

    /// Core `TestBlockValidity` on the tip (`validation.cpp:4923-4974`; CH3 §2.14), as MN0's
    /// `CreateNewBlock` calls it (`node/miner.cpp:339`, PoW and merkle off): Core's
    /// `BlockValidationState`, or [`NotTested::NotBestPrev`] when `block`'s parent is not the tip.
    pub fn test_block_validity(
        &self,
        block: Arc<Block>,
        check_pow: bool,
        check_merkle: bool,
    ) -> ActorReply<Result<BlockState, NotTested>> {
        self.send_test_block_validity(block, check_pow, check_merkle, false)
    }

    /// RP's `getblocktemplate` proposal mode (`rpc/mining.cpp:707-724`) under one V hold: GBT's
    /// duplicate answers, then [`NotTested::NotBestPrev`], then `TestBlockValidity` with PoW off
    /// and merkle on.
    pub fn propose_block(&self, block: Arc<Block>) -> ActorReply<Result<BlockState, NotTested>> {
        self.send_test_block_validity(block, false, true, true)
    }

    fn send_test_block_validity(
        &self,
        block: Arc<Block>,
        check_pow: bool,
        check_merkle: bool,
        lookup: bool,
    ) -> ActorReply<Result<BlockState, NotTested>> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::TestBlockValidity {
            block,
            check_pow,
            check_merkle,
            lookup,
            reply: tx,
        });
        rx
    }

    /// Core `FlushStateToDisk(state, mode)` with no tip step ([`ChainCommand::Flush`]): the reply
    /// once V's flush returns (an ALWAYS write done), or disconnected if it failed and V stopped.
    pub fn flush(&self, mode: FlushStateMode) -> ActorReply<()> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::Flush {
            mode,
            reply: tx,
        });
        rx
    }

    /// `gettxoutsetinfo`'s and `dumptxoutset`'s flush and coins snapshot
    /// ([`ChainCommand::CoinsStats`]): the scan, started on the coins at the flushed tip, which
    /// writes `dump` after its statistics.
    pub fn coins_stats(
        &self,
        hash_type: CoinStatsHashType,
        interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        dump: Option<CoinsDump>,
    ) -> ActorReply<Option<CoinsStatsJob>> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::CoinsStats {
            hash_type,
            interrupted,
            dump,
            reply: tx,
        });
        rx
    }

    /// `scantxoutset`'s flush and coins snapshot ([`ChainCommand::CoinsScan`]): the walk,
    /// started on the coins at the flushed tip.
    pub fn coins_scan(&self, scan: CoinsScan) -> ActorReply<Option<CoinsScanJob>> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::CoinsScan {
            scan,
            reply: tx,
        });
        rx
    }

    /// `invalidateblock`: Core `InvalidateBlock`, then `ActivateBestChain`.
    pub fn invalidate_block(&self, hash: BlockHash) -> ActorReply<Result<(), BlockNotFound>> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::InvalidateBlock {
            hash,
            reply: tx,
        });
        rx
    }

    /// `reconsiderblock`: Core `ResetBlockFailureFlags`, then `ActivateBestChain`.
    pub fn reconsider_block(
        &self,
        hash: BlockHash,
        ignore_chainlocks: bool,
    ) -> ActorReply<Result<(), BlockNotFound>> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::ReconsiderBlock {
            hash,
            ignore_chainlocks,
            reply: tx,
        });
        rx
    }

    /// `preciousblock`: Core `PreciousBlock`.
    pub fn precious_block(&self, hash: BlockHash) -> ActorReply<Result<(), BlockNotFound>> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::PreciousBlock {
            hash,
            reply: tx,
        });
        rx
    }

    /// `ProcessNewChainLock`'s `CheckActiveState`, before LK1a's checks (§5.1, step 1).
    pub fn cl_intake(&self) -> ActorReply<ClIntakeReply> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::ClIntake {
            reply: tx,
        });
        rx
    }

    /// `UpdateBestChainlock` for a lock LK1a has verified (§5.1, step 5): the reply says
    /// whether to relay it.
    pub fn record_chainlock(
        &self,
        sig: Arc<ChainLockSig>,
        sig_hash: Hash256,
        from: Option<PeerId>,
    ) -> ActorReply<ClReply> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::RecordChainLock {
            sig,
            sig_hash,
            from,
            reply: tx,
        });
        rx
    }

    /// The ChainLock handler's 5 s task (`h:63-72`): ND1's scheduler posts it.
    pub fn chainlock_timer(&self) -> Result<(), Stopped> {
        self.send(ChainCommand::ClEnforce {
            source: ClTrigger::Timer,
        })
    }

    /// `NetInstantSend::ResolveBlockConflicts` for an InstantSend lock (§5.6).
    pub fn is_conflict_invalidate(&self, lock: Hash256, txid: Hash256) -> ActorReply<IsResolution> {
        let (tx, rx) = reply();
        let _ = self.send(ChainCommand::IsConflictInvalidate {
            lock,
            txid,
            reply: tx,
        });
        rx
    }

    /// Asks V to stop after its current command, as ND1's shutdown does once the node's flag is
    /// set (Core's `Interrupt`, then `Shutdown`); it does not set that flag itself (D52).
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.send(ChainCommand::Shutdown);
    }

    /// Core `ShutdownRequested()`: the node's flag, which `-stopatheight`, a listener's panic and
    /// the `AbortNode` sink set (D52).
    pub fn shutdown_requested(&self) -> bool {
        self.node_shutdown.shutdown_requested()
    }

    /// V's own stop latch (D52).
    #[cfg(test)]
    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// The last published [`ChainView`].
    pub fn view(&self) -> Arc<ChainView> {
        self.view.load_full()
    }

    pub fn reader(&self) -> &ChainReader {
        &self.reader
    }

    /// Core `g_best_block` (§4.11).
    pub fn tip_watch(&self) -> &Arc<TipWatch> {
        &self.tip_watch
    }

    pub fn loading(&self) -> &Arc<LoadingFlags> {
        &self.loading
    }

    /// The read side for other components (§6.3).
    pub fn view_handle(&self) -> ChainViewHandle {
        ChainViewHandle {
            chain: self.clone(),
        }
    }

    /// The node clock's `now` (mockable).
    pub(crate) fn node_time(&self) -> NodeTime {
        self.clock.node_time()
    }
}

/// Work V defers to after the running class C operation (§2.4).
pub(crate) enum Deferred {
    /// A class C command that arrived at a release point.
    Command(ChainCommand),
    /// `ProcessNewBlock`'s ABC tail, after its accept scope ran at a release point.
    BlockTail {
        block: Arc<Block>,
        result: BlockResult,
        reply: Option<ReplySender<BlockResult>>,
    },
    /// The ABC tail of `reconsiderblock` or `preciousblock`.
    RpcTail {
        reply: BlockRpcReply,
    },
}

/// C++'s `int x = int64_value` on Core's platforms: the low 32 bits, two's complement.
#[expect(clippy::cast_possible_truncation, reason = "Core's narrowing, ported as it is")]
pub(super) fn core_int(v: i64) -> i32 {
    v as i32
}

/// D51's `AbortNode` for an attach V no longer takes.
pub(crate) const COINS_ATTACH_MISUSE: &str =
    "The coins were attached twice, or after a class C operation (D51)";

/// D64's `AbortNode` for a `VerifyLoaded` outside Core's init: a second one, or one after a
/// class C operation such as startup's ABC.
pub(crate) const VERIFY_LOADED_MISUSE: &str =
    "The loaded chainstate was verified twice, or after a class C operation (D64)";

/// V's state (§2.3).
pub(crate) struct V {
    pub(crate) params: Arc<ChainParams>,
    pub(crate) chain: HeaderChain,
    pub(crate) reader: ChainReader,
    pub(crate) idx: NodeBlockIndex,
    pub(crate) checks: Box<dyn BlockChecks>,
    pub(crate) files: Box<dyn BlockFiles>,
    /// `ConnectTip`'s read-ahead (CH4-4).
    pub(crate) read_ahead: ReadAhead,
    pub(crate) connector: Box<dyn BlockConnector>,
    pub(crate) mempool: Box<dyn MempoolSink>,
    pub(crate) flush: Box<dyn FlushSite>,
    pub(crate) is: Box<dyn IsConflicts>,
    pub(crate) ui: Arc<dyn UiNotifications>,
    abort: Arc<dyn AbortSink>,
    pub(crate) signals: ValidationSignals,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) loading: Arc<LoadingFlags>,
    /// The feeds' producer wake: V's in-hold drain parks on it (§2.5). W's bell rings it too.
    pub(crate) wake: Arc<Parker>,
    /// W's bell (BL-123): V's idle and barrier selects take it, and release points poll it.
    /// `None` once its sender is gone.
    bell: Option<Bell>,
    pub(crate) tip_watch: Arc<TipWatch>,
    /// The ChainLock mirror, `lastNotify` and the tip task's flag (§5.5).
    pub(crate) cl: ClState,
    view: Arc<ArcSwap<ChainView>>,
    view_seq: u64,
    lanes: Lanes,
    /// The started reaper, until V hands it the lanes as it stops.
    reaper: Option<Sender<Lanes>>,
    deferred: VecDeque<Deferred>,
    /// D38: V holds commands until startup's ABC completes (DEC-196 (2) left candidates ranked
    /// below the failed tip).
    holding: bool,
    /// V is in a [`VWait`]: C5's and C5′'s waits never nest, and a `ProcessBlock` reaching V
    /// meanwhile is deferred whole when C5′ applies (D67 R3).
    in_wait: bool,
    /// The held commands in arrival order: once `holding` ends, an oldest lane, served first.
    held: VecDeque<ChainCommand>,
    /// A class C operation is running: commands at release points are serviced, class C work
    /// is deferred.
    in_composite: bool,
    /// D51: `AttachCoins` is still accepted (no attach yet, and no class C operation).
    coins_attachable: bool,
    /// D64: `VerifyLoaded` is still accepted (none yet, and no class C operation).
    pub(super) verify_loadable: bool,
    /// V's own stop latch (D52): a `Shutdown` command, an init error and `stop_serving` set it,
    /// never the node's flag, so init still sees its own errors as errors.
    stopping: Arc<AtomicBool>,
    /// Core `StartShutdown()`/`ShutdownRequested()`: the node's flag.
    node_shutdown: Arc<dyn ShutdownFlag>,
    /// Core `-stopatheight` in Core's `int nStopAtHeight` (`validation.cpp:3752`).
    pub(crate) stop_at_height: i32,
    /// A `Shutdown` command was taken, or V stopped where an `AbortNode` arose (D213): V stops
    /// once the running operation ends.
    stop: bool,
    /// The `AbortNode` V carried past ([`V::carry_past`]), which V has stopped on: a command's
    /// run at a release point (D213), or one Core's caller ignores or answers with (BL-279). The
    /// running operation goes on to Core's `ShutdownRequested()` check, and V's loop then ends on
    /// it.
    pub(super) interrupted: Option<Fatal>,
    pub(crate) check_index: bool,
    /// Core `m_blockman.m_have_pruned`: [`VParts::have_pruned`] at start; step 7's clears set it
    /// (§7.2).
    pub(crate) have_pruned: bool,
    /// [`start`]'s wait for V's first act, the devnet step (D48).
    started: Option<ReplySender<Result<(), LoadFailure>>>,
    /// The readiness guard's scope, for the connector's context (CH3 §2.17).
    pub(crate) guard: GuardScope,
    /// CS4's deployments over `chain` with Core's two caches (CH3 §2.16): the connector's
    /// context and `UpdateTip`'s warning.
    pub(super) deployments: ChainDeployments,
    /// `DoWarning`'s `static bool fWarned` (`validation.cpp:3152`): V has run `AlertNotify`.
    pub(super) warned: bool,
    /// V's one context (EV addendum §2.6): lent only into the connector's calls, through a hold.
    pub(super) vcx: VContext,
    /// Runs on V at each stage of a hold's release: §9.4's adversarial schedule.
    #[cfg(test)]
    pub(crate) release_hook: Option<tests::Hook>,
    /// The VerifyDB point's result, set by a test (CH5 has not filled the point).
    #[cfg(test)]
    pub(crate) verify_db_result: Option<super::VerifyDbResult>,
    /// [`ChainHandle::set_between_stores`]'s hook.
    #[cfg(feature = "test-support")]
    pub(super) between_stores: Arc<Mutex<Option<BetweenStores>>>,
}

/// Starts V's reaper, which waits for V's lanes and then drops every command still queued or
/// sent after V stopped, so their replies disconnect: a bounded crossbeam channel keeps its
/// buffer while any sender lives, even with no receiver left. It ends when the last
/// [`ChainHandle`] is dropped (both lanes' senders go with it), or at once if V never hands the
/// lanes over (V's own thread failed to start).
fn spawn_reaper() -> std::io::Result<Sender<Lanes>> {
    #[cfg(test)]
    if tests::FAIL_REAPER_SPAWN.with(std::cell::Cell::get) {
        return Err(std::io::Error::other("the reaper's spawn failed (injected)"));
    }
    let (to_reaper, from_v) = crossbeam_channel::bounded::<Lanes>(1);
    spawn_thread("dashd-v-reaper", move || {
        #[expect(clippy::disallowed_methods, reason = "the reaper is unmarked and waits on V")]
        if let Ok(mut lanes) = from_v.recv() {
            while lanes.recv(&mut None).is_some() {}
        }
    })?;
    Ok(to_reaper)
}

/// Hands `lanes` to the reaper, if it still waits for them.
fn reap(reaper: Option<Sender<Lanes>>, lanes: &Lanes) {
    if let Some(reaper) = reaper {
        // Its one slot is free, and the reaper waits on it until it is filled.
        let _ = reaper.try_send(lanes.clone());
    }
}

/// The end of [`V::stop_serving`], run when it is dropped: after the `AbortNode` sink, or as a
/// panicking sink unwinds. V's latch, then the node's `StartShutdown()`, which ND1's sink has
/// already called unless it panicked (Core's `AbortNode` ends with it, `shutdown.cpp:36`; it is
/// idempotent), so the node never waits on a V that has stopped.
struct StopTail<'a> {
    stopping: &'a AtomicBool,
    node: &'a dyn ShutdownFlag,
    reaper: Option<Sender<Lanes>>,
    lanes: &'a Lanes,
}

impl Drop for StopTail<'_> {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.node.start_shutdown();
        reap(self.reaper.take(), self.lanes);
    }
}

/// Why [`start`] has no V.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// V's thread or its reaper could not start.
    #[error(transparent)]
    Spawn(#[from] std::io::Error),
    /// `LoadGenesisBlock`'s devnet step failed, Core's init error (N-5): V has stopped.
    #[error(transparent)]
    Load(LoadFailure),
}

/// [`spawn`], then V's first act awaited: on a fresh devnet index, `LoadGenesisBlock`'s devnet
/// step (D48). ND1's step 7 calls it after [`open_block_index`]; it waits on V, so its caller
/// is unmarked (LO-9).
///
/// [`open_block_index`]: super::open_block_index
///
/// # Errors
///
/// [`StartError::Load`] when the step fails: init's error, not `AbortNode` (V's sink has an
/// `AbortNode` only if `AcceptBlock` raised one). V has stopped and been joined.
///
/// # Panics
///
/// As Core's `assert` on the devnet genesis block's `CheckBlock`: V's panic, resumed here.
pub fn start(parts: VParts) -> Result<(ChainHandle, NodeJoinHandle<VExit>), StartError> {
    let (handle, join, started) = spawn_v(parts)?;
    let loaded = started.recv();
    if let Ok(Ok(())) = loaded {
        return Ok((handle, join));
    }
    let joined = join.join().expect("start's caller is unmarked");
    match (loaded, joined) {
        (Ok(Err(f)), _) => Err(StartError::Load(f)),
        (_, Err(p)) => panic::resume_unwind(p),
        (r, Ok(_)) => unreachable!("V reports its first act before it stops: {r:?}"),
    }
}

/// Starts V on its own thread. Fails, and V never starts, if its reaper cannot start (Sol cp1
/// #3): a stop must disconnect every queued reply whatever spawns fail by then. After
/// DEC-196 (2), V holds other commands until its first `ActivateBestChain` completes (D38).
/// [`start`] also waits for V's first act: under `spawn` a rejected devnet step stops V with no
/// `AbortNode` and no init error (D50).
pub fn spawn(parts: VParts) -> std::io::Result<(ChainHandle, NodeJoinHandle<VExit>)> {
    let (handle, join, _) = spawn_v(parts)?;
    Ok((handle, join))
}

/// [`spawn`], with the wait for V's first act.
#[expect(clippy::type_complexity, reason = "spawn's pair and start's wait")]
fn spawn_v(
    mut parts: VParts,
) -> std::io::Result<(ChainHandle, NodeJoinHandle<VExit>, ActorReply<Result<(), LoadFailure>>)> {
    let reaper = spawn_reaper()?;
    let (tx, rx) = crossbeam_channel::bounded(COMMAND_CAPACITY);
    let (marked, rx_marked) = crossbeam_channel::unbounded();
    let reader = parts.chain.reader();
    let loading = Arc::new(LoadingFlags::default());
    let stopping = Arc::new(AtomicBool::new(false));
    parts.signals.link_shutdown(parts.shutdown.clone());
    // Before any subscriber registers (SB5: set once, first).
    let wake = Parker::new();
    parts.chain.set_producer_wake(Notify::Thread(wake.clone()));
    let (bell, bell_rx) = FlushBell::new(wake.clone());
    parts.flush.attach(bell);
    let tip_watch = Arc::new(TipWatch::default());
    let holding = parts.idx.take_tied_fallback();
    // Core starts with no lock (`cl:37-41`); a fresh `HeaderChain` carries none.
    debug_assert!(
        parts.chain.cl_state().best.is_none() && parts.chain.cl_floor().is_none(),
        "V's mirror starts empty"
    );
    let cl = ClState::new(parts.chain.cl_enabled_observed());
    let (started_tx, started) = reply();
    // V's first `publish_view`, stored before the handle exists.
    let view_seq = 1;
    let view = Arc::new(ArcSwap::from_pointee(chain_view(
        &parts.chain,
        &reader,
        &parts.idx,
        &cl.sigs,
        view_seq,
    )));
    let handle = ChainHandle {
        tx,
        marked,
        marked_peak: Arc::new(AtomicUsize::new(0)),
        view: view.clone(),
        tip_watch: tip_watch.clone(),
        reader: reader.clone(),
        loading: loading.clone(),
        stopping: stopping.clone(),
        node_shutdown: parts.shutdown.clone(),
        clock: parts.clock.clone(),
        #[cfg(feature = "test-support")]
        between_stores: Arc::default(),
    };
    let deployments = ChainDeployments::new(parts.params.clone(), &parts.chain);
    let read_ahead = ReadAhead::new(parts.read_ahead, parts.files.read_ahead(), &parts.params);
    #[cfg(feature = "test-support")]
    let between_stores = handle.between_stores.clone();
    let join = spawn_thread("dashd-v", move || {
        // V is built on its own thread, so its one context is made where V's loop runs (EV
        // addendum §2.6) and V itself never crosses a thread.
        let v = V {
            params: parts.params,
            reader,
            chain: parts.chain,
            idx: parts.idx,
            checks: parts.checks,
            files: parts.files,
            read_ahead,
            connector: parts.connector,
            mempool: parts.mempool,
            flush: parts.flush,
            is: parts.is,
            ui: parts.ui,
            abort: parts.abort,
            signals: parts.signals,
            clock: parts.clock,
            loading,
            wake,
            bell: Some(bell_rx),
            tip_watch,
            view,
            cl,
            view_seq,
            lanes: Lanes {
                rx: [rx, rx_marked],
                live: [true; 2],
            },
            reaper: Some(reaper),
            deferred: VecDeque::new(),
            holding,
            held: VecDeque::new(),
            in_composite: false,
            in_wait: false,
            coins_attachable: true,
            verify_loadable: true,
            stopping,
            node_shutdown: parts.shutdown,
            // `v:3752`: the C++ `int` narrows the int64 argument, keeping its low 32 bits.
            stop_at_height: core_int(parts.stop_at_height),
            stop: false,
            interrupted: None,
            check_index: parts.check_block_index,
            have_pruned: parts.have_pruned,
            started: Some(started_tx),
            guard: parts.guard,
            deployments,
            warned: false,
            vcx: VContext::new(),
            #[cfg(test)]
            release_hook: None,
            #[cfg(test)]
            verify_db_result: None,
            #[cfg(feature = "test-support")]
            between_stores,
        };
        v.run()
    })?;
    Ok((handle, join, started))
}

/// V's waits outside a hold (§8.2, D67): for a signals barrier, or a listener's condition (C5,
/// C5′). Each serves the commands queued meanwhile, one at a time, as a release point would:
/// class R and M run, class C is deferred, and under C5′ a `ProcessBlock` accept outside IBD is
/// deferred whole (R3). Inside a class C operation and at the idle point (A-1b (3)) it is the
/// same wait.
pub(crate) struct VWait<'a> {
    v: &'a mut V,
    /// The first failure of a serviced command that V does not carry on past (D213): the wait
    /// ends, serves nothing more, and V stops. One V carries on past has stopped V all the same,
    /// so the wait ends `Disconnected`.
    fatal: Option<Fatal>,
}

/// What a [`VWait`] waits for.
#[expect(clippy::disallowed_types, reason = "a listener's wake, never a reply from V")]
enum Until<'w> {
    Barrier(&'w Barrier),
    /// A-1b (2): `done` decides; `wake` rings when it may have changed.
    Cond {
        wake: &'w crossbeam_channel::Receiver<()>,
        done: &'w mut dyn FnMut() -> bool,
    },
}

impl Until<'_> {
    /// `Some` once the wait is over.
    fn over(&mut self) -> Option<Result<(), WaitError>> {
        match self {
            Until::Barrier(b) => b.poll(),
            Until::Cond {
                done,
                ..
            } => done().then_some(Ok(())),
        }
    }

    #[expect(clippy::disallowed_types, reason = "a barrier's or a listener's wake, read by V")]
    fn receiver(&self) -> &crossbeam_channel::Receiver<()> {
        match self {
            Until::Barrier(b) => b.receiver(),
            Until::Cond {
                wake,
                ..
            } => wake,
        }
    }

    /// After a receipt on [`Self::receiver`]: `Some` once the wait is over.
    fn received(&mut self, r: Result<(), RecvError>) -> Option<Result<(), WaitError>> {
        match self {
            Until::Barrier(_) => Some(r.map_err(|_| WaitError::Disconnected)),
            // `wake`'s sender is gone: the wait ends unless `done` holds.
            Until::Cond {
                done,
                ..
            } => r.is_err().then(|| {
                if done() {
                    Ok(())
                } else {
                    Err(WaitError::Disconnected)
                }
            }),
        }
    }
}

impl VWait<'_> {
    /// A release point each time a lane has commands, until `until` is over or V must stop.
    fn serve(&mut self, mut until: Until<'_>) -> Result<(), WaitError> {
        // A listener that waits again after an `Err` gets the same `Err` and nothing served
        // (Opus r1 N1).
        if self.fatal.is_some() {
            return Err(WaitError::Disconnected);
        }
        loop {
            if let Some(r) = until.over() {
                return r;
            }
            if self.v.stop {
                return Err(WaitError::Disconnected);
            }
            // D38, D67 R2: a held command is older than any in the lanes.
            if let Some(cmd) = self.v.take_held() {
                if let Err(f) = self.v.dispatch_at_release(cmd) {
                    return self.fail(f);
                }
                continue;
            }
            if !self.v.lanes.live.contains(&true) && self.v.bell.is_none() {
                // The handles and W are gone, and the lanes empty: nothing is left to service.
                #[expect(clippy::disallowed_methods, reason = "V, unmarked, with nothing to serve")]
                let r = until.receiver().recv();
                if let Some(r) = until.received(r) {
                    return r;
                }
                continue;
            }
            match self.v.lanes.select(Some(until.receiver()), &mut self.v.bell) {
                Selected::Until(r) => {
                    if let Some(r) = until.received(r) {
                        return r;
                    }
                }
                // H3 (BL-123): the wait answers W, and a subscriber's `FlushDue` (D47 item 10).
                Selected::Bell => {
                    if let Err(f) = self.v.answer_flush() {
                        return self.fail(f);
                    }
                }
                // The command was queued: a release point takes it. The loop then rechecks
                // `until`, `stop`, `held` and the bell before the next one (Opus r1 N7).
                Selected::Command(cmd) => {
                    if let Err(f) = self.v.dispatch_at_release(cmd) {
                        return self.fail(f);
                    }
                }
                Selected::Closed => {}
            }
        }
    }

    /// Keeps the first failure: V's stop path branches on its cause (D62).
    fn fail(&mut self, f: Fatal) -> Result<(), WaitError> {
        self.fatal.get_or_insert(f);
        Err(WaitError::Disconnected)
    }
}

impl ServiceWait for VWait<'_> {
    fn wait(&mut self, barrier: &Barrier) -> Result<(), WaitError> {
        self.serve(Until::Barrier(barrier))
    }

    #[expect(clippy::disallowed_types, reason = "a listener's wake, never a reply from V")]
    fn wait_until(
        &mut self,
        wake: &crossbeam_channel::Receiver<()>,
        done: &mut dyn FnMut() -> bool,
    ) -> Result<(), WaitError> {
        self.serve(Until::Cond {
            wake,
            done,
        })
    }
}

/// C5's and C5′'s hooks (D67).
#[derive(Clone, Copy)]
enum QueueHook {
    /// [`ValidationInterface::release_point`](super::signals::ValidationInterface::release_point).
    Release,
    /// [`ValidationInterface::before_accept`](super::signals::ValidationInterface::before_accept).
    BeforeAccept,
}

/// The `ChainView` of `chain`'s current snapshot (§6.2), as [`V::publish_view`] stores it.
fn chain_view(
    chain: &HeaderChain,
    reader: &ChainReader,
    idx: &NodeBlockIndex,
    cl: &Arc<ClSigs>,
    view_seq: u64,
) -> ChainView {
    let snap = chain.snapshot();
    let (mtp, tip_chain_tx) = if snap.has_tip() {
        (reader.entry(snap.tip.tip).map_or(0, |e| e.mtp), idx.n_chain_tx(snap.tip.tip))
    } else {
        (0, 0)
    };
    ChainView {
        snap,
        cl: cl.clone(),
        mtp,
        tip_chain_tx,
        large_work_invalid_chain: idx.large_work_invalid_chain(),
        view_seq,
    }
}

impl V {
    fn run(mut self) -> VExit {
        // D55: a reply V drops unsent disconnects after the step that dropped it, or, when the
        // step goes Fatal, after `stop_serving` has set the node's flag.
        let _parked = ParkDrops::start();
        let served = panic::catch_unwind(AssertUnwindSafe(|| self.serve()));
        // CH4-4: no block is read ahead for a V that has stopped serving.
        self.read_ahead.stop();
        let fatal = match served {
            Ok(fatal) => fatal,
            Err(p) => {
                // Core's `assert` aborts the process; V reports and stops the node instead of
                // leaving it running without a validation thread.
                let what = panic_message(p.as_ref());
                self.stop_serving(&Fatal::new(format!("V panicked: {what}")), AbortCause::Panic);
                panic::resume_unwind(p);
            }
        };
        // Whether the shutdown flushes can still write: the store latch is open.
        let (fatal, open) = match fatal {
            None => match self.stop_flush_at_exit(false) {
                Ok(()) => {
                    self.reap();
                    (None, true)
                }
                Err(f) => {
                    self.stop_serving(&f, f.cause());
                    (Some(f), false)
                }
            },
            Some(f) => {
                // D62 (DEC-241): an `AbortNode` leaves the store open, and the flush side still
                // stops in order after the sink, as Core's shutdown flushes run after one. A
                // failure there is the flush side's, which it has reported (L-3). One of Core's
                // asserts trips the latch and stops nothing (D62a). An SB4 contract failure stops
                // the flush side too, but keeps nothing for the shutdown flushes: SB4 refused a
                // step V may have half taken, so nothing past V's last flush is written (BL-271
                // r1 m-2).
                let cause = f.cause();
                let open = !self.stop_serving(&f, cause)
                    && cause == AbortCause::AbortNode
                    && self.stop_flush_at_exit(true).is_ok()
                    && !f.is_contract();
                (Some(f), open)
            }
        };
        // Otherwise the flush side drops here, on V's thread, so a W panic reaches V's joiner as
        // W's driver drops (Sol r1 N2).
        let flush = open.then_some(self.flush);
        VExit {
            chain: self.chain,
            idx: self.idx,
            have_pruned: self.have_pruned,
            fatal,
            flush,
            reader: self.reader,
            signals: self.signals,
            clock: self.clock,
            loading: self.loading,
            abort: self.abort,
        }
    }

    /// The stop order (§7.1, D45): W's pending events are answered, then the flush side's stop
    /// waits for a job in flight, answering what W sends meanwhile. The flush side, and W with it,
    /// goes to [`VExit`] for the shutdown flushes (BL-271).
    fn stop_flush(&mut self) -> Result<(), Fatal> {
        self.with_flush(|f, host| {
            f.service(host)?;
            f.stop(host)
        })
    }

    /// [`stop_flush`](Self::stop_flush) at V's end, outside `serve`'s `catch_unwind`. A panic
    /// there is V's panic (D62): the store latch trips, and V reports and stops serving unless
    /// its sink has already reported (`reported`); then the panic resumes.
    fn stop_flush_at_exit(&mut self, reported: bool) -> Result<(), Fatal> {
        let stopped = panic::catch_unwind(AssertUnwindSafe(|| self.stop_flush()));
        stopped.unwrap_or_else(|p| {
            if reported {
                let (now, loading) = (self.now(), self.loading.any());
                self.flush.abort(
                    AbortCause::Panic,
                    &mut FlushHost::new(
                        &mut self.chain,
                        &mut self.idx,
                        &mut self.have_pruned,
                        now,
                        loading,
                    ),
                );
            } else {
                let what = panic_message(p.as_ref());
                self.stop_serving(&Fatal::new(format!("V panicked: {what}")), AbortCause::Panic);
            }
            panic::resume_unwind(p)
        })
    }

    /// Hands the lanes to the reaper, once.
    fn reap(&mut self) {
        reap(self.reaper.take(), &self.lanes);
    }

    /// V's idle select: a held command first (D38), then a command from either lane, or W's
    /// bell; `None` once both lanes are disconnected and empty.
    fn next(&mut self) -> Option<Next> {
        if let Some(cmd) = self.take_held() {
            return Some(Next::Command(cmd));
        }
        self.lanes.recv(&mut self.bell)
    }

    /// Runs `f` on the flush side with V's chain lent, outside any hold (§7.1): the
    /// `ChainStateFlushed` signals W released meanwhile are queued (DEC-166).
    fn with_flush<T>(
        &mut self,
        f: impl FnOnce(&mut dyn FlushSite, &mut FlushHost<'_>) -> Result<T, Fatal>,
    ) -> Result<T, Fatal> {
        let (now, loading) = (self.now(), self.loading.any());
        let host =
            FlushHost::new(&mut self.chain, &mut self.idx, &mut self.have_pruned, now, loading);
        host.lend(&mut *self.flush, &self.reader, |q| self.signals.enqueue(q), f)
    }

    /// W's events at a service point of V's that is not one of D45's waits: a coins attach.
    fn service_flush(&mut self) -> Result<(), Fatal> {
        self.with_flush(|f, host| f.service(host))
    }

    /// W's events, and a subscriber's `FlushDue`, at one of V's waits outside a hold (H3,
    /// BL-123, D47 item 10).
    fn answer_flush(&mut self) -> Result<(), Fatal> {
        self.with_flush(|f, host| f.answer_due(host))
    }

    /// A flush site with no tip step (`AcceptBlock`'s NONE, ABC's PERIODIC): the decision and
    /// its freeze at once, at `(V's tip, the last seq)`.
    pub(crate) fn flush_now(&mut self, mode: FlushStateMode) -> Result<(), Fatal> {
        self.with_flush(|f, host| {
            let decided = f.at_site(mode, host)?;
            f.after_step(decided, host)
        })
    }

    /// The tests' stand-in for `pruneblockchain`'s `FlushStateToDisk(NONE, height)` (CH2b b4 Sol
    /// r1 N5): [`ChainCommand::Flush`] carries no manual height until that RPC's slice.
    #[cfg(test)]
    pub(crate) fn flush_at_site(
        &mut self,
        mode: FlushStateMode,
        manual_prune_height: Option<u32>,
    ) -> Result<(), Fatal> {
        self.with_flush(|f, host| f.flush(mode, manual_prune_height, host))
    }

    /// The tests' stand-in for M3's init step 10 (`PruneAndFlush`, CH2b b4 D57).
    #[cfg(test)]
    pub(crate) fn prune_and_flush_at_site(&mut self) -> Result<(), Fatal> {
        self.with_flush(|f, host| f.prune_and_flush(host))
    }

    /// V's loop: `deferred` first, then the lanes. `Some` is the `AbortNode` that stopped it.
    fn serve(&mut self) -> Option<Fatal> {
        // D18 (D48): the devnet step finishes Core's `LoadGenesisBlock` before V serves anything;
        // init hears how it went (N-5).
        let loaded = self.load_devnet_genesis();
        if let Some(started) = self.started.take() {
            started.send(match &loaded {
                Ok(r) => r.clone(),
                // AcceptBlock's `AbortNode`, which logs; Core's init error follows it.
                Err(_) => Err(LoadFailure::Failure {
                    cause: None,
                }),
            });
        }
        match loaded {
            Err(f) => return Some(f),
            // Init's error, with no `AbortNode`: V stops as init's shutdown stops it.
            Ok(Err(_)) => {
                self.stopping.store(true, Ordering::SeqCst);
                return None;
            }
            Ok(Ok(())) => {}
        }
        loop {
            if self.stop {
                return None;
            }
            let r = match self.deferred.pop_front() {
                Some(d) => self.run_deferred(d),
                None => match self.idle_point() {
                    // The wait deferred commands, or took a `Shutdown`: those come first.
                    Ok(()) if !self.deferred.is_empty() || self.stop => Ok(()),
                    // D38's hold holds commands, never W's events.
                    Ok(()) => match self.next()? {
                        Next::Command(cmd) => self.dispatch(cmd, false),
                        Next::Bell => self.answer_flush(),
                    },
                    Err(f) => Err(f),
                },
            };
            if let Err(f) = r.and_then(|()| self.debug_invariants()) {
                return Some(f);
            }
            // D213: V stopped on a command at a release point, and the operation it interrupted
            // has ended. What that operation parked is released by `run`'s stop.
            if let Some(f) = self.interrupted.take() {
                return Some(self.run_carried_tails(f));
            }
            release_parked();
        }
    }

    /// The block tails left once V has stopped on `f`, an `AbortNode` it carried past (BL-535). A
    /// `ProcessBlock` served at a release point whose `AcceptBlock` carried one past (its NONE
    /// flush, BL-279) deferred its ABC tail after `stop_serving` had cleared `deferred`. Core's
    /// thread for that block waits on `m_chainstate_mutex` (`validation.cpp:3744`) and runs its
    /// ABC once the interrupted one has broken at `ShutdownRequested()` (`:3847`): one step, the
    /// same check, then `ProcessNewBlock`'s answer (`:4905-4911`). So V runs each tail now, and
    /// it stops at that check as Core's does. V serves nothing once stopped, so nothing else can
    /// be deferred. A tail that fails is the operation failing on its own, which V stops on
    /// instead (D213).
    fn run_carried_tails(&mut self, f: Fatal) -> Fatal {
        for d in std::mem::take(&mut self.deferred) {
            debug_assert!(matches!(d, Deferred::BlockTail { .. }), "V serves nothing once stopped");
            if let tail @ Deferred::BlockTail {
                ..
            } = d
                && let Err(e) = self.run_deferred(tail)
            {
                return e;
            }
        }
        f
    }

    /// `AbortNode`: the flush side's `abort` (D45, D62), the sink, V's latch and the node's
    /// flag, and every outstanding reply disconnected. The latch, the flag and the reaper's lanes
    /// follow the sink even if it panics, and the replies follow the flag (D55): the failed
    /// command's, parked as it unwound, and the deferred and held commands'. `true` if the store
    /// latch had already tripped, its failure reported (L-3).
    fn stop_serving(&mut self, f: &Fatal, cause: AbortCause) -> bool {
        let _replies = ReleaseParked;
        let _tail = StopTail {
            stopping: &self.stopping,
            node: &*self.node_shutdown,
            reaper: self.reaper.take(),
            lanes: &self.lanes,
        };
        // Before the sink: on a panic or an assert, W writes nothing more and its queued jobs do
        // not run (D62, D62a). A failure that tripped the latch first has been reported (L-3),
        // and one the connector or a store reported is not reported again; one Core reports
        // after another is (D52, CH3a D60).
        let (now, loading) = (self.now(), self.loading.any());
        let latched = self.flush.abort(
            cause,
            &mut FlushHost::new(
                &mut self.chain,
                &mut self.idx,
                &mut self.have_pruned,
                now,
                loading,
            ),
        );
        if f.reported_by_v(latched) {
            self.abort.abort_node(&f.0, None);
        }
        self.deferred.clear();
        self.held.clear();
        latched
    }

    /// Core's `AbortNode` where it arises (D213): V stops now, so its report comes before
    /// anything the failed operation's callers log, and serves no more commands; the lanes are
    /// the reaper's. The `Fatal` returned is marked reported, so `run`'s stop only reads the
    /// latch again (an `AbortNode`'s `abort`, D62) and stops the flush side as before. One of
    /// Core's asserts is returned as it is: Core's ends the process at once, and V stops in `run`
    /// (D62a).
    pub(crate) fn abort_at_fault<T>(&mut self, r: Result<T, Fatal>) -> Result<T, Fatal> {
        r.map_err(|f| {
            if !f.is_abort_node() {
                return f;
            }
            self.stop_serving(&f, AbortCause::AbortNode);
            self.stop = true;
            f.into_reported()
        })
    }

    /// A command at a release point (§2.4). Core runs it on another thread: its `AbortNode`
    /// reports and requests shutdown there, and the class C operation goes on to its
    /// `ShutdownRequested()` check (`v:3841`, `v:3937`) (D213). So V stops on it at once, and its
    /// loop ends on the first such failure once that operation has. An SB4 contract failure stops
    /// V there too but unwinds the operation, as V's own; so does one of Core's asserts, with V's
    /// stop in `run`. Outside a class C operation a failure is V's own.
    fn dispatch_at_release(&mut self, cmd: ChainCommand) -> Result<(), Fatal> {
        let r = self.dispatch(cmd, true);
        if !self.in_composite {
            return r;
        }
        self.carry_past(r)
    }

    /// An `AbortNode` whose caller carries on, as Core's does past another thread's (D213) or
    /// past a result it ignores (`AcceptBlock`'s flush, BL-279): V stops at the fault, the running
    /// operation goes on to Core's `ShutdownRequested()` check, and V's loop then ends on it. One
    /// of Core's asserts or an SB4 contract failure is returned, as V never carries on past it.
    pub(crate) fn carry_past(&mut self, r: Result<(), Fatal>) -> Result<(), Fatal> {
        match self.abort_at_fault(r) {
            Err(f) if f.carried_past() => {
                self.interrupted.get_or_insert(f);
                Ok(())
            }
            r => r,
        }
    }

    /// Runs `cmd` now, or defers its class C part when `at_release` (§2.4).
    fn dispatch(&mut self, cmd: ChainCommand, at_release: bool) -> Result<(), Fatal> {
        if self.holding && !cmd.exempt_from_hold() {
            if self.held.is_empty() {
                tracing::info!(
                    "Holding commands until the startup ActivateBestChain completes (invalid \
                     chainstate tip, DEC-196)"
                );
            }
            self.held.push_back(cmd);
            return Ok(());
        }
        match cmd.class() {
            Class::Control => {
                self.stopping.store(true, Ordering::SeqCst);
                self.stop = true;
                Ok(())
            }
            Class::C if at_release => {
                self.deferred.push_back(Deferred::Command(cmd));
                Ok(())
            }
            Class::R | Class::M | Class::C => self.handle(cmd, at_release),
        }
    }

    fn handle(&mut self, cmd: ChainCommand, at_release: bool) -> Result<(), Fatal> {
        // C5′ (DEC-240, D67): outside IBD an accept waits until P3's queue holds nothing
        // outstanding. Inside another wait it is deferred whole and comes back here (R3).
        if matches!(cmd, ChainCommand::ProcessBlock { .. })
            && self.signals.has_listeners()
            && !self.is_ibd()
        {
            if self.in_wait {
                self.deferred.push_back(Deferred::Command(cmd));
                return Ok(());
            }
            self.queue_hook(QueueHook::BeforeAccept)?;
        }
        match cmd {
            ChainCommand::ProcessHeaders {
                batch,
                from,
                reply,
            } => self.process_headers(batch, from, reply),
            ChainCommand::ProcessBlock {
                block,
                hash,
                force,
                from: _,
                merkle_checked,
                reply,
            } => {
                let Some((result, reply)) =
                    self.process_block_accept(&block, hash, force, merkle_checked, reply)?
                else {
                    return Ok(());
                };
                let tail = Deferred::BlockTail {
                    block,
                    result,
                    reply,
                };
                self.run_tail(tail, at_release)
            }
            ChainCommand::ActivateBestChain {
                reply,
            } => {
                self.composite(|v| v.activate_best_chain(None))?;
                self.holding = false;
                if let Some(r) = reply {
                    r.send(());
                }
                Ok(())
            }
            ChainCommand::SetLoading {
                importing,
                reindex,
            } => {
                self.loading.set(importing, reindex);
                Ok(())
            }
            ChainCommand::AttachCoins {
                coins,
                reply,
            } => {
                self.attach_coins(coins)?;
                reply.send(());
                Ok(())
            }
            ChainCommand::VerifyLoaded {
                options,
                reply,
            } => {
                reply.send(self.verify_loaded(&options)?);
                Ok(())
            }
            ChainCommand::VerifyDb {
                args,
                reply,
            } => {
                reply.send(self.verify_db(args)?);
                Ok(())
            }
            ChainCommand::InvalidateBlock {
                hash,
                reply,
            } => {
                let Some(id) = self.reader.lookup(&hash) else {
                    reply.send(Err(BlockNotFound));
                    return Ok(());
                };
                // rpc:2015-2018: the state stays valid unless an AbortNode stops V, so ABC runs
                // whatever InvalidateBlock returned.
                self.composite(|v| {
                    v.invalidate_block(id)?;
                    v.activate_best_chain(None)
                })?;
                reply.send(Ok(()));
                Ok(())
            }
            ChainCommand::ReconsiderBlock {
                hash,
                ignore_chainlocks,
                reply,
            } => {
                let Some(id) = self.reader.lookup(&hash) else {
                    reply.send(Err(BlockNotFound));
                    return Ok(());
                };
                self.reconsider_block(id, ignore_chainlocks)?;
                self.run_tail(
                    Deferred::RpcTail {
                        reply,
                    },
                    at_release,
                )
            }
            ChainCommand::PreciousBlock {
                hash,
                reply,
            } => {
                let Some(id) = self.reader.lookup(&hash) else {
                    reply.send(Err(BlockNotFound));
                    return Ok(());
                };
                if self.precious_block(id) {
                    self.run_tail(
                        Deferred::RpcTail {
                            reply,
                        },
                        at_release,
                    )
                } else {
                    reply.send(Ok(()));
                    Ok(())
                }
            }
            ChainCommand::ClIntake {
                reply,
            } => {
                self.cl_intake(reply);
                Ok(())
            }
            ChainCommand::RecordChainLock {
                sig,
                sig_hash,
                from,
                reply,
            } => self.record_chainlock(sig, sig_hash, from, reply),
            ChainCommand::TestBlockValidity {
                block,
                check_pow,
                check_merkle,
                lookup,
                reply,
            } => {
                let r = self.test_block_validity(&block, check_pow, check_merkle, lookup)?;
                // CH1-node §4.12: a class M scope ends with `scope_end` before the reply.
                self.scope_end();
                reply.send(r);
                Ok(())
            }
            ChainCommand::Flush {
                mode,
                reply,
            } => {
                self.with_flush(|f, host| f.flush(mode, None, host))?;
                self.scope_end();
                reply.send(());
                Ok(())
            }
            ChainCommand::CoinsStats {
                hash_type,
                interrupted,
                dump,
                reply,
            } => {
                let job =
                    self.with_flush(|f, host| f.coins_stats(hash_type, interrupted, dump, host))?;
                self.scope_end();
                reply.send(job);
                Ok(())
            }
            ChainCommand::CoinsScan {
                scan,
                reply,
            } => {
                let job = self.with_flush(|f, host| f.coins_scan(scan, host))?;
                self.scope_end();
                reply.send(job);
                Ok(())
            }
            ChainCommand::ClEnforce {
                source,
            } => self.cl_enforce(source),
            ChainCommand::IsConflictInvalidate {
                lock,
                txid,
                reply,
            } => {
                let r = self.composite(|v| v.resolve_block_conflicts(&lock, &txid))?;
                reply.send(r);
                Ok(())
            }
            ChainCommand::Query(f) => {
                let deployments = self.deployments.view();
                f(&ChainQuery {
                    chain: &self.chain,
                    idx: &self.idx,
                    cl: &self.cl.sigs,
                    have_pruned: self.have_pruned,
                    check_block_index: self.check_index,
                    deployments: QueryDeployments::new(&deployments),
                });
                Ok(())
            }
            ChainCommand::Shutdown => unreachable!("dispatched as control"),
            // A test's probe is no class C operation of Core's init: it leaves D64's latch.
            #[cfg(test)]
            ChainCommand::TestOnV(f) => {
                let loadable = self.verify_loadable;
                let r = self.composite(f);
                self.verify_loadable = loadable;
                r
            }
            #[cfg(test)]
            ChainCommand::TestUnderMain(f) => {
                let r = f(self);
                self.scope_end();
                r
            }
        }
    }

    fn run_deferred(&mut self, d: Deferred) -> Result<(), Fatal> {
        match d {
            Deferred::Command(cmd) => self.handle(cmd, false),
            Deferred::BlockTail {
                block,
                result,
                reply,
            } => self.process_block_tail(&block, result, reply),
            Deferred::RpcTail {
                reply,
            } => {
                self.composite(|v| v.activate_best_chain(None))?;
                reply.send(Ok(()));
                Ok(())
            }
        }
    }

    /// A command's ABC tail: now, or deferred at a release point (§2.4).
    fn run_tail(&mut self, tail: Deferred, at_release: bool) -> Result<(), Fatal> {
        if at_release {
            self.deferred.push_back(tail);
            Ok(())
        } else {
            self.run_deferred(tail)
        }
    }

    /// A release point (§2.4): the commands queued now, in either lane, run; class C work is
    /// deferred.
    pub(crate) fn release_point(&mut self) -> Result<(), Fatal> {
        debug_assert!(self.in_composite, "release points exist only inside class C operations");
        // H3 (BL-123): a release point answers W, and a subscriber's `FlushDue` (D47 item 10).
        let rang = self.bell.as_ref().map_or(Err(TryRecvError::Empty), Bell::try_recv);
        if take_ring(&mut self.bell, rang) {
            self.answer_flush()?;
        }
        // D38: held commands are older than any in the lanes.
        while let Some(cmd) = self.take_held() {
            if self.stop {
                return Ok(());
            }
            self.dispatch_at_release(cmd)?;
        }
        let queued = self.lanes.rx.each_ref().map(Lane::len);
        for (i, n) in queued.into_iter().enumerate() {
            for _ in 0..n {
                if self.stop {
                    return Ok(());
                }
                match self.lanes.rx[i].try_recv() {
                    Ok(cmd) => self.dispatch_at_release(cmd)?,
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }

    /// Queues `d` behind the work already deferred: it runs when V is next idle, or after the
    /// running class C operation (§2.4).
    pub(crate) fn defer(&mut self, d: Deferred) {
        self.deferred.push_back(d);
    }

    /// How much work is deferred.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn deferred_len(&self) -> usize {
        self.deferred.len()
    }

    /// The oldest held command, once startup's ABC has completed (D38).
    fn take_held(&mut self) -> Option<ChainCommand> {
        if self.holding {
            return None;
        }
        self.held.pop_front()
    }

    /// Stages `cmd` as a command held before startup's ABC completed (D38's regression).
    #[cfg(all(test, not(loom)))]
    pub(crate) fn stage_held(&mut self, cmd: ChainCommand) {
        self.held.push_back(cmd);
    }

    /// Treats both lanes as closed, as when every handle is gone (D45's lanes-closed wait).
    #[cfg(all(test, not(loom)))]
    pub(crate) fn forget_lanes(&mut self) {
        self.lanes.live = [false; 2];
    }

    /// Core's `LimitValidationInterfaceQueue` (`v:3716-3722`), called where Core does
    /// (`v:3760`, `v:3940`): a release-point wait while the queue is over 10. Then C5's hook, at
    /// the same point (D67 R4).
    pub(crate) fn limit_queue(&mut self) -> Result<(), Fatal> {
        // `Disconnected`: the signals thread has stopped (the node's shutdown, or a listener's
        // panic, which sets the node's flag before the queue closes), or a `Shutdown` command ran
        // at the wait, which sets V's latch, or V stopped on a command's `AbortNode` there (D213),
        // which sets both. Either way ABC takes its step and stops at its `ShutdownRequested`
        // check (:3841), which reads both (D52), as Core's does.
        self.wait_with(|signals, wait| signals.limit_queue(wait))?;
        self.queue_hook(QueueHook::Release)
    }

    /// C5's idle point (A-1b (1), (3)): before V takes its next command, once `held` is empty
    /// (D67 R2).
    fn idle_point(&mut self) -> Result<(), Fatal> {
        if !self.held.is_empty() {
            return Ok(());
        }
        self.queue_hook(QueueHook::Release)
    }

    /// P3's hook, when one is due: a listener is registered, and V is neither holding commands
    /// (D38), inside another wait (they never nest), nor stopping. With no listener this is one
    /// relaxed load.
    fn queue_hook(&mut self, hook: QueueHook) -> Result<(), Fatal> {
        if !self.signals.has_listeners() || self.holding || self.in_wait || self.stop {
            return Ok(());
        }
        self.wait_with(|signals, wait| match hook {
            QueueHook::Release => signals.release_point(wait),
            QueueHook::BeforeAccept => signals.before_accept(wait),
        })
    }

    /// Runs `f` with a [`VWait`], inside a class C operation or at the idle point (A-1b (3)). A
    /// wait that ends `Disconnected` is V's or the node's stop, which V reads at its next check;
    /// `Err` is a served command's failure that V does not carry on past (D213), whatever `f`
    /// returned.
    fn wait_with(
        &mut self,
        f: impl FnOnce(&ValidationSignals, &mut VWait<'_>) -> Result<(), WaitError>,
    ) -> Result<(), Fatal> {
        debug_assert!(!self.in_wait, "V's waits never nest");
        let signals = self.signals.clone();
        self.in_wait = true;
        let mut wait = VWait {
            v: self,
            fatal: None,
        };
        let r = f(&signals, &mut wait);
        let fatal = wait.fatal;
        self.in_wait = false;
        debug_assert_ne!(r, Err(WaitError::WaitOnVFromNoWaitTask), "V carries no marker");
        fatal.map_or(Ok(()), Err)
    }

    /// Runs `f` as a class C operation: release points inside it service R/M commands.
    pub(crate) fn composite<T>(
        &mut self,
        f: impl FnOnce(&mut V) -> Result<T, Fatal>,
    ) -> Result<T, Fatal> {
        debug_assert!(!self.in_composite, "class C operations never overlap");
        self.in_composite = true;
        self.coins_attachable = false;
        self.verify_loadable = false;
        let r = f(self);
        self.in_composite = false;
        r
    }

    /// D51: the coins side replaces the spawn's, which stops in D45's order first (a no-op for
    /// the `NoFlush` ND1 spawns with), and W's bell is V's anew (that `NoFlush` dropped the
    /// first). V then services the new side once, for any event W queued before it had a bell.
    fn attach_coins(&mut self, coins: AttachCoins) -> Result<(), Fatal> {
        let AttachCoins {
            connector,
            flush,
        } = coins;
        if !self.coins_attachable {
            // It drops with its store open (D62): it was given no job (D51's precondition), and V
            // answers it nothing.
            return Err(Fatal::new(COINS_ATTACH_MISUSE));
        }
        self.coins_attachable = false;
        // If the spawn's side fails to stop, the new side never became V's: it drops, as above
        // (D51, D62).
        self.stop_flush()?;
        self.connector = connector;
        self.flush = flush;
        let (bell, rx) = FlushBell::new(self.wake.clone());
        self.bell = Some(rx);
        self.flush.attach(bell);
        self.service_flush()
    }

    /// A class C operation is running.
    pub(crate) fn in_composite(&self) -> bool {
        self.in_composite
    }

    /// Core `ShutdownRequested()` at its sites on V (`v:3841`, `v:3937`): the node's flag, or V's
    /// own stop latch (D52).
    pub(crate) fn shutdown_requested(&self) -> bool {
        self.stopping.load(Ordering::SeqCst) || self.node_shutdown.shutdown_requested()
    }

    /// Core `StartShutdown()`: the node's flag, which wakes ND1's main wait (D52).
    pub(crate) fn request_shutdown(&self) {
        self.node_shutdown.start_shutdown();
    }

    /// The node clock's `now`.
    pub(crate) fn now(&self) -> NodeTime {
        self.clock.node_time()
    }

    /// `now` in seconds (`GetTime()`).
    pub(crate) fn now_secs(&self) -> i64 {
        self.clock.node_now().secs()
    }

    /// Core `IsInitialBlockDownload()` under `cs_main`: on V's private tip (CH1-A8).
    pub(crate) fn is_ibd(&self) -> bool {
        self.chain.is_ibd_private(self.now(), self.loading.any())
    }

    /// V's private tip (Core `m_chain.Tip()`): `None` before genesis.
    pub(crate) fn tip(&self) -> Option<BlockId> {
        self.chain.private_tip().map(|(t, _)| t.id)
    }

    /// §4.12: every class M scope's and hold release's end, before the reply: the status
    /// snapshot, then the `ChainView` with the ChainLock mirror, which V reconciles at each SB4
    /// call that moves it (§5.3). Emits no signal.
    pub(crate) fn scope_end(&mut self) {
        self.chain.publish_status();
        self.publish_view();
    }

    /// The `view_seq` the next [`Self::publish_view`] stores: C8's stamp.
    pub(crate) fn next_view_seq(&self) -> u64 {
        self.view_seq + 1
    }

    /// Stores a `ChainView` over SB4's current snapshot (§6.2).
    pub(crate) fn publish_view(&mut self) {
        self.view_seq += 1;
        let view = chain_view(&self.chain, &self.reader, &self.idx, &self.cl.sigs, self.view_seq);
        self.view.store(Arc::new(view));
    }

    /// Core `CheckBlockIndex()` where Core calls it, under `-checkblockindex`.
    pub(crate) fn check_block_index(&self) -> Result<(), Fatal> {
        if !self.check_index {
            return Ok(());
        }
        check_block_index(&self.chain, &self.idx, self.have_pruned)
            .map_err(|e| Fatal::assert(format!("CheckBlockIndex: {} at entry {}", e.what, e.id.0)))
    }

    /// Invariants C1 (§4.8) and M1 (§5.3) after every command in debug builds, and D65's: V's
    /// view is the last build, as every publication is followed by [`Self::publish_view`] with
    /// no status write between, so a view read again after [`dash_chain::SnapshotExpired`] is the newest
    /// snapshot, which never expires (Opus BL-224 r1 N2).
    fn debug_invariants(&self) -> Result<(), Fatal> {
        if cfg!(debug_assertions) {
            self.idx.check_c1().map_err(Fatal::assert)?;
            self.check_m1().map_err(Fatal::assert)?;
            let (view, tip) = (self.view.load().snap.epoch, self.chain.snapshot().epoch);
            if view != tip {
                return Err(Fatal::assert(format!(
                    "D65: V's view at epoch {view}, the tip cell's at {tip}"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A class C operation's body, run inside [`V::composite`].
    pub(crate) type OnV = Box<dyn FnOnce(&mut V) -> Result<(), Fatal> + Send>;

    /// A [`VWait`] on `v` that a served command has already failed with `f` (Opus r1 N1).
    pub(crate) fn failed_wait(v: &mut V, f: Fatal) -> VWait<'_> {
        VWait {
            v,
            fatal: Some(f),
        }
    }

    thread_local! {
        /// Fails [`spawn_reaper`] on this thread (Sol cp1 #3's regression).
        pub(crate) static FAIL_REAPER_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// How many times [`Lanes::ready`] has selected on this thread (BL-453).
        pub(crate) static LANE_SELECTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    /// V's two lanes as [`spawn`] makes them, both live, and their senders.
    fn lanes() -> (Lanes, [Sender<ChainCommand>; 2]) {
        let ((tx, rx), (marked, rx_marked)) =
            (crossbeam_channel::bounded(COMMAND_CAPACITY), crossbeam_channel::unbounded());
        let lanes = Lanes {
            rx: [rx, rx_marked],
            live: [true; 2],
        };
        (lanes, [tx, marked])
    }

    /// Sends `msg` on `tx` 100 ms on, from a thread of `s`, so a select parks first.
    #[cfg(all(feature = "alloc-tag", not(miri)))]
    fn send_later<'s, T: Send + 's>(s: &'s std::thread::Scope<'s, '_>, tx: &'s Sender<T>, msg: T) {
        s.spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            tx.send(msg).expect("the select's receiver");
        });
    }

    /// BL-453 (DEC-614): a disconnected lane or bell is taken once, then left out of V's select
    /// (the lane as `never()`, the bell dropped); left in, it would be ready on every select, a
    /// spin. A lane and the bell are closed here, and the other lane closes 100 ms on: V selects
    /// once for each, whichever closes first, then finds no lane live.
    #[test]
    fn a_closed_lane_or_bell_leaves_vs_select() {
        let (mut lanes, [tx0, tx1]) = lanes();
        drop(tx0);
        let (ring, bell) = crossbeam_channel::bounded::<()>(1);
        drop(ring);
        let mut bell = Some(bell);
        LANE_SELECTS.with(|n| n.set(0));
        let next = std::thread::scope(|s| {
            s.spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(100));
                drop(tx1);
            });
            lanes.recv(&mut bell)
        });
        assert!(next.is_none(), "both lanes are closed");
        let selects = LANE_SELECTS.with(std::cell::Cell::get);
        assert_eq!(selects, 3, "a closed receiver was selected again");
        assert_eq!(lanes.live, [false, false]);
        assert!(bell.is_none(), "the closed bell is dropped");
    }

    /// BL-453: V's lane select allocates nothing on channels it has parked on before (its lanes,
    /// W's bell, a listener's wake), ready at once or parked, in V's idle loop or a release
    /// point's wait. A first park grows each channel's waker list ([`Lanes::select`]), so the
    /// test parks until one select has (a select its sender beats parks nowhere; a send landing
    /// while the select registers would leave a later list to grow, which at 100 ms on is not
    /// seen). Counted with the tagging allocator, so it runs with `alloc-tag` (the gate's feature
    /// run).
    #[cfg(all(feature = "alloc-tag", not(miri)))]
    #[test]
    fn vs_lane_select_allocates_nothing() {
        use crate::alloc_tag::thread_alloc_events;

        // The counter counts (its positive control).
        let before = thread_alloc_events();
        drop(std::hint::black_box(Box::new(0u8)));
        assert_eq!(thread_alloc_events(), before + 2, "one allocation and one free");

        let (mut lanes, [tx0, tx1]) = lanes();
        let (ring, bell) = crossbeam_channel::bounded(1);
        let mut bell = Some(bell);
        let (wake, until) = crossbeam_channel::bounded(1);
        // A select, with the allocations and frees it made.
        let mut select = |waiter: bool| {
            let before = thread_alloc_events();
            let r = lanes.select(waiter.then_some(&until), &mut bell);
            (r, thread_alloc_events() - before)
        };
        std::thread::scope(|s| {
            let mut warm = false;
            for _ in 0..20 {
                send_later(s, &tx0, ChainCommand::Shutdown);
                let (r, events) = select(true);
                assert!(matches!(r, Selected::Command(ChainCommand::Shutdown)));
                if events > 0 {
                    warm = true;
                    break;
                }
            }
            assert!(warm, "no warm-up select parked");

            tx1.send(ChainCommand::Shutdown).expect("lane 1");
            let (r, events) = select(false);
            assert!(matches!(r, Selected::Command(ChainCommand::Shutdown)));
            assert_eq!(events, 0, "at once");

            send_later(s, &tx0, ChainCommand::Shutdown);
            let (r, events) = select(false);
            assert!(matches!(r, Selected::Command(ChainCommand::Shutdown)));
            assert_eq!(events, 0, "parked, idle");

            send_later(s, &ring, ());
            let (r, events) = select(true);
            assert!(matches!(r, Selected::Bell));
            assert_eq!(events, 0, "parked, rung");

            send_later(s, &wake, ());
            let (r, events) = select(true);
            assert!(matches!(r, Selected::Until(Ok(()))));
            assert_eq!(events, 0, "parked, woken");
        });
    }

    /// [`V::release_hook`]'s hook.
    pub(crate) type Hook = Arc<dyn Fn(Stage) + Send + Sync>;

    /// Where in a release (N1) the hook runs: inside the hold, then after each of the release's
    /// actions in turn.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Stage {
        /// Inside the hold, before its snapshot.
        Release,
        /// After `Hold::release`'s snapshot.
        Snapshot,
        /// After `publish_view`.
        Published,
        /// After the signals' splice.
        Spliced,
        /// After `TipWatch::set`, when the hold moved the tip.
        Watched,
        /// After each `NotifyBlockTip`.
        Ui,
        /// Inside the hold, as V parks in its fanout drain (b4 D81).
        Drain,
    }
}
