//! The `PeerManager` actor: Core's `ThreadMessageHandler` (P2a §3.1.1) as one OS thread around
//! [`MessageLoop::run_one_pass`], and [`Shared`], the processor's half of SB6's
//! [`NetEvents`].
//!
//! `initialize` runs synchronously on the dialling or accepting task before the peer enters the
//! connection table (it creates the peer's state and pushes our `version` to outbound peers);
//! `finalize` removes the state at once and leaves the counters to the actor's next pass.
//! Neither ever waits on the actor (P2a §2.2.1 (i)).

use std::cell::Cell;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(feature = "sim")]
use std::sync::mpsc;
use std::sync::{Arc, OnceLock, Weak};
use std::thread::{JoinHandle, LocalKey};
use std::time::{Duration, Instant};

use dash_consensus::{Block, BlockValidationError};
use dash_p2p::handshake::{self, LocalIdentity};
use dash_p2p::rng::{FastRandom, RandSource, process_source};
use dash_p2p::{
    CommandString, NetCore, NetEvents, OutMsg, PeerHandle, PeerId, Service, ServiceFlags,
};
use dash_p2p_core::{DsQueue, Inventory};
use dash_primitives::{BlockHash, TxHash};
use parking_lot::Mutex;

use super::compact::{CompactBlock, draw_nonce};
use super::download::StallingTimeout;
use super::eviction::{EXTRA_PEER_CHECK_INTERVAL, OutboundFlags, fully_connected};
use super::fetch::{FetchBlockError, FetchQueue, FetchTicket};
use super::headers::MAX_BLOCKS_TO_ANNOUNCE;
use super::logic::{PeerLogic, PeerMap, PeerSlot, ProcessorOptions, Region, Seams, version_inputs};
use super::misbehavior;
use super::peer::{HeaderCell, HeaderGuard, PeerHeaders, PeerState};
use super::registry::{MessageProcessingResult, Registry};
use super::relay::{self, Related};
use super::seams::{ChainAccess, EntryFacts, MasternodeSeams, TreeRead};
use super::signals::{Bells, SignalQueue, SyncSignal};
use super::stats::{self, NodeStateStats};
use super::sync::{peer_has_header, process_block_availability};
use super::txrequest::TxRequestTracker;

/// Core's masternode send throttle: masternode connections are sent to on the first pass at or
/// after each 100 ms boundary.
pub const MN_SEND_INTERVAL: Duration = Duration::from_millis(100);
/// The longest wait without work (Core's `condMsgProc.wait_until(now + 100ms)`).
pub const WAIT_INTERVAL: Duration = Duration::from_millis(100);

thread_local! {
    /// Whether this thread is inside [`MessageLoop::pass_with`] (debug builds check D66).
    static IN_PASS: Cell<bool> = const { Cell::new(false) };
    /// Whether this thread is inside [`MessageLoop::run`]: the actor thread, whose every step
    /// (passes, scheduled tasks, waits, the exit drain) must not wait on `FetchBlock`.
    static IN_RUN: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread is running a pass.
fn in_pass() -> bool {
    IN_PASS.with(Cell::get)
}

/// Whether this thread is the actor: inside [`MessageLoop::run`], or a pass `sim` drives.
fn on_actor() -> bool {
    IN_RUN.with(Cell::get) || in_pass()
}

/// Sets one of this thread's flags until dropped.
struct ThreadFlag(&'static LocalKey<Cell<bool>>, bool);

impl ThreadFlag {
    fn set(flag: &'static LocalKey<Cell<bool>>) -> Self {
        Self(flag, flag.with(|f| f.replace(true)))
    }
}

impl Drop for ThreadFlag {
    fn drop(&mut self) {
        self.0.with(|f| f.set(self.1));
    }
}

/// `m_highest_fast_announce` and `m_most_recent_*` (np:1038-1044, `m_most_recent_block_mutex`):
/// `NewPoWValidBlock` writes them on V's thread, section 7 reads the head's copy, and getdata
/// serving reads them live (P3 §2.5, D56, D58). A leaf lock.
#[derive(Clone, Default)]
pub struct RecentBlock {
    /// `m_highest_fast_announce`.
    pub highest_fast_announce: i32,
    /// `m_most_recent_block_hash`.
    pub hash: Option<BlockHash>,
    /// `m_most_recent_block`.
    pub block: Option<Arc<Block>>,
    /// `m_most_recent_compact_block`, as its wire payload.
    pub cmpct: Option<Arc<[u8]>>,
}

/// A header cell's announce stamp was newer than the caller's snapshot (P3 D56): release every
/// processor lock, [`ChainAccess::wait_published`] for the stamp, take a new snapshot, retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ahead(pub u64);

/// What V's signal paths and the actor's drains share (P3 §2.5): the signal queue, the bells,
/// the announce stamp and `recent`.
pub struct Signals {
    /// The queued `BlockChecked` signals.
    pub queue: SignalQueue,
    /// The doorbell and the wake flag.
    pub bells: Arc<Bells>,
    /// The stamp of the latest `NewPoWValidBlock` call that passed its height test, stored by V
    /// before it touches `recent` or any cell (D56).
    announce_stamp: AtomicU64,
    /// `recent`, a leaf lock.
    recent: Mutex<RecentBlock>,
    /// `mapBlockSource.size()`, which DEC-230's guard reads.
    pub(super) source_len: Arc<AtomicUsize>,
    /// `m_block_stalling_timeout`: section 9 raises it, `BlockConnected` decays it.
    pub(super) stalling_timeout: StallingTimeout,
    /// `sim` only: runs at each [`LockPoint`] of [`Signals::lock_headers`] (S4-x's pauses).
    #[cfg(feature = "sim")]
    lock_hook: Mutex<Option<LockHook>>,
    /// `sim` only: runs after `UpdatedBlockTip`'s visit of each peer, with its id (S4-x (5)).
    #[cfg(feature = "sim")]
    relay_hook: Mutex<Option<RelayHook>>,
    /// `sim` only: [`Shared::request_pass_ack`]'s requests, which the next pass takes at its start.
    #[cfg(feature = "sim")]
    pass_acks: Mutex<Vec<mpsc::Sender<PassAck>>>,
}

/// `sim` only: the actor's acknowledgment of a [`Shared::request_pass_ack`], sent at the end of
/// the first pass that started after the request and ran over every peer not marked to
/// disconnect (an interrupted pass drops it).
#[cfg(feature = "sim")]
#[derive(Clone, Debug)]
pub struct PassAck {
    /// Peers whose `m_blocks_for_headers_relay` or `m_blocks_for_inv_relay` still held a hash
    /// at the pass's end, except snapshot peers marked to disconnect.
    pub relay_pending: Vec<PeerId>,
}

/// Where a `sim` test's hook runs: in [`Signals::lock_headers`] with the cell held, and before
/// V or `UpdatedBlockTip` locks a peer's cell, so a test can wait for a thread to reach a cell
/// it holds instead of sleeping.
#[cfg(feature = "sim")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockPoint {
    /// After the cell's lock, before the `read` and the stamp load.
    Locked,
    /// After the stamp load, where rev10 copied `recent` (D58, D59).
    StampRead,
    /// `NewPoWValidBlock`'s visit, before it locks the peer's header cell.
    Visit(PeerId),
    /// `UpdatedBlockTip`, before it locks the peer's `block_inv` cell.
    Relay(PeerId),
    /// The head of `send_messages`, before it locks the peer's `block_inv` cell (DEC-236's
    /// barrier (i), D122).
    HeadEntry(PeerId),
    /// The head of `send_messages`, with the peer's `block_inv` cell locked, before it takes S
    /// (DEC-236's barriers (ii) and (iii), D122).
    Head(PeerId),
    /// `FetchBlock` for the peer, after the remove-all and before `BlockRequested`, with the
    /// peer's state lock held (BL-503).
    FetchBetween(PeerId),
}

/// See [`LockPoint`].
#[cfg(feature = "sim")]
pub type LockHook = Arc<dyn Fn(LockPoint) + Send + Sync>;

/// A `sim` test's hook after `UpdatedBlockTip`'s visit of each peer, with the peer's id.
#[cfg(feature = "sim")]
pub type RelayHook = Arc<dyn Fn(PeerId) + Send + Sync>;

impl Signals {
    fn new() -> Self {
        let bells = Arc::new(Bells::default());
        let source_len = Arc::new(AtomicUsize::new(0));
        Self {
            queue: SignalQueue::new(Arc::clone(&bells), Arc::clone(&source_len)),
            bells,
            announce_stamp: AtomicU64::new(0),
            recent: Mutex::default(),
            source_len,
            stalling_timeout: StallingTimeout::default(),
            #[cfg(feature = "sim")]
            lock_hook: Mutex::default(),
            #[cfg(feature = "sim")]
            relay_hook: Mutex::default(),
            #[cfg(feature = "sim")]
            pass_acks: Mutex::default(),
        }
    }

    /// Runs `hook` after `UpdatedBlockTip`'s visit of each peer, appended to or skipped.
    #[cfg(feature = "sim")]
    pub fn set_relay_hook(&self, hook: Option<RelayHook>) {
        *self.relay_hook.lock() = hook;
    }

    /// Runs `hook` at each [`LockPoint`]: every [`Signals::lock_headers`] call's two, and each
    /// visit's and relay's cell lock.
    #[cfg(feature = "sim")]
    pub fn set_lock_hook(&self, hook: Option<LockHook>) {
        *self.lock_hook.lock() = hook;
    }

    #[cfg(feature = "sim")]
    pub(super) fn on_lock(&self, at: LockPoint) {
        let hook = self.lock_hook.lock().clone();
        if let Some(hook) = hook {
            hook(at);
        }
    }

    /// `m_block_stalling_timeout`, in seconds.
    pub fn stalling_timeout(&self) -> i64 {
        self.stalling_timeout.get()
    }

    /// Every reader but V: lock `cell`, run `read`, then load the announce stamp. The guard and
    /// `read`'s result when the stamp is ≤ `seq`, the `view_seq` of the caller's snapshot; else
    /// unlock and return [`Ahead`] (P3 §3.6, D56, D58).
    ///
    /// V stores the stamp before it writes `recent` or locks a cell, and `read` runs between
    /// this lock and the load, so a `read` that sees a call's `recent` always meets that call's
    /// stamp. Mutex ordering and the atomic's coherence are all the proof needs.
    pub fn lock_headers<R>(
        &self,
        cell: &HeaderCell,
        seq: u64,
        read: impl FnOnce(&Self) -> R,
    ) -> Result<(HeaderGuard, R), Ahead> {
        let guard = HeaderGuard::lock(cell);
        #[cfg(feature = "sim")]
        self.on_lock(LockPoint::Locked);
        let r = read(self);
        let stamp = self.announce_stamp.load(Ordering::Relaxed);
        #[cfg(feature = "sim")]
        self.on_lock(LockPoint::StampRead);
        if stamp > seq {
            return Err(Ahead(stamp));
        }
        Ok((guard, r))
    }

    /// A copy of `recent` (three `Arc`s and a hash), under its leaf lock.
    pub fn recent(&self) -> RecentBlock {
        self.recent.lock().clone()
    }

    /// V's store of the announce stamp, the first step of `NewPoWValidBlock` after its height
    /// test (D56). [`Shared::new_pow_valid_block`] calls it; tests stand in for V with it.
    pub fn store_announce_stamp(&self, stamp: u64) {
        self.announce_stamp.store(stamp, Ordering::Relaxed);
    }
}

/// What the network core's callbacks and the actor share.
pub struct Shared {
    identity: Arc<LocalIdentity>,
    opts: ProcessorOptions,
    chain: Arc<dyn ChainAccess>,
    /// The masternode seams (BL-702), copied into the processor.
    mn: MasternodeSeams,
    peers: Arc<PeerMap>,
    /// Bound before the actor can create any object requests.
    object_request: OnceLock<Arc<Mutex<TxRequestTracker>>>,
    /// The signal queue, the bells (Core's `fMsgProcWake` under `mutexMsgProc` among them), the
    /// announce stamp and `recent`.
    signals: Arc<Signals>,
    /// Core's `flagInterruptMsgProc`.
    interrupt: Arc<AtomicBool>,
    /// Whether the message thread ended in a panic.
    panicked: AtomicBool,
    /// States `finalize` removed, for the actor's counters.
    finalized: Mutex<Vec<(PeerHandle, Arc<Mutex<PeerState>>)>>,
    /// ConnMan's flags the processor sets; the host passes them to SB6's dial loop.
    flags: Arc<OutboundFlags>,
    /// `mockscheduler` time not yet applied to the 45 s check, in µs.
    mock_forward_us: AtomicU64,
    /// The network core, bound by [`MessageLoop::new`], for `NewPoWValidBlock`'s visit and
    /// draw on V's thread (the core holds this `Shared` as its events, hence the `Weak`).
    core: OnceLock<Weak<NetCore>>,
    /// `FetchBlock` requests from other threads, for the actor (BL-503).
    fetches: FetchQueue,
    pending_dsq: Mutex<Vec<DsQueue>>,
}

impl Shared {
    /// The shared half; pass it to [`NetCore::new`] as the events, then to
    /// [`MessageLoop::new`]. We run as a masternode exactly when `mn` has a signer:
    /// [`LocalIdentity::masternode`] is set from it here, whatever `identity` says.
    pub fn new(
        mut identity: LocalIdentity,
        opts: ProcessorOptions,
        chain: Arc<dyn ChainAccess>,
        mn: MasternodeSeams,
    ) -> Arc<Self> {
        // Core: net_processing.cpp:3939 (`is_masternode = m_nodeman != nullptr`)
        identity.masternode = mn.signer.is_some();
        Arc::new(Self {
            identity: Arc::new(identity),
            opts,
            chain,
            mn,
            peers: Arc::default(),
            object_request: OnceLock::new(),
            signals: Arc::new(Signals::new()),
            interrupt: Arc::default(),
            panicked: AtomicBool::new(false),
            finalized: Mutex::default(),
            flags: Arc::default(),
            mock_forward_us: AtomicU64::new(0),
            core: OnceLock::new(),
            fetches: FetchQueue::default(),
            pending_dsq: Mutex::default(),
        })
    }

    /// Our identity: the user agent is Core's `strSubVersion`.
    pub fn identity(&self) -> &LocalIdentity {
        &self.identity
    }

    /// Core's `PeerManager::IgnoresIncomingTxs()`: `-blocksonly`.
    pub fn ignores_incoming_txs(&self) -> bool {
        self.opts.blocks_only
    }

    /// The flags SB6's dial loop reads (`m_try_another_outbound_peer`,
    /// `m_start_extra_block_relay_peers`): pass this to `NetCore::dial_tick` as the policy.
    pub fn dial_policy(&self) -> Arc<OutboundFlags> {
        Arc::clone(&self.flags)
    }

    /// Core's `UpdatedBlockTip` from the chain side: the IBD service-flags cache, then (outside
    /// IBD) the new tip hashes, oldest first, on every fully connected `CanRelay()` peer, and a
    /// wake. `new_blocks` is Core's `vHashes`: from the new tip back to (not including) the fork
    /// point, newest first; only the first [`MAX_BLOCKS_TO_ANNOUNCE`] are used. The chain side
    /// collects them from its own index, so this never calls back into [`ChainAccess`] and takes
    /// only the peer map and the block-inv cells, never a state lock (D24).
    ///
    /// Lock order (D66): the chain side calls this from its queued signals thread (CH1-node's
    /// `ValidationSignals`), never synchronously from a [`ChainAccess`] call or while any of the
    /// processor's locks is held; it may hold its own index lock only because no [`ChainAccess`]
    /// read takes it. Debug builds assert that it is not called from inside a pass.
    // Core: net_processing.cpp:2273-2308 PeerManagerImpl::UpdatedBlockTip
    pub fn updated_block_tip(
        &self,
        core: &NetCore,
        new_blocks: &[BlockHash],
        initial_download: bool,
    ) {
        debug_assert!(!in_pass(), "UpdatedBlockTip called synchronously from a pass (D66)");
        // `m_best_height` is the snapshot's tip height (D10).
        core.set_ibd_complete(!initial_download);
        if initial_download {
            return;
        }
        let hashes = &new_blocks[..new_blocks.len().min(MAX_BLOCKS_TO_ANNOUNCE)];
        // Each recipient is read at its visit, as Core's `ForEachNode` reads each node it reaches.
        for p in core.peers().iter() {
            if fully_connected(p) && p.can_relay() {
                let cell = self.peers.lock().get(&p.id()).map(|s| Arc::clone(&s.block_inv));
                if let Some(cell) = cell {
                    #[cfg(feature = "sim")]
                    self.signals.on_lock(LockPoint::Relay(p.id()));
                    let mut inv = cell.lock();
                    for h in hashes.iter().rev() {
                        inv.headers_relay.push(*h);
                    }
                }
            }
            #[cfg(feature = "sim")]
            {
                let hook = self.signals.relay_hook.lock().clone();
                if let Some(hook) = hook {
                    hook(p.id());
                }
            }
        }
        self.wake();
    }

    /// Core's `BlockChecked` from the chain side, on V's thread: `invalid` is the failed state,
    /// `None` for a valid block, and `ibd` is `IsInitialBlockDownload()` as V evaluated it when
    /// it signalled (CH1-node's V-evaluated argument; the seam never re-reads the published
    /// view). Core's signal is synchronous; here it is pushed on the ordered signal queue with V's
    /// stamp, one more than the `view_seq` V last published (C7 (a)), and the actor applies it
    /// at the first drain whose snapshot covers it (P3 §2.5, D37, D65). Never blocks.
    pub fn block_checked(&self, hash: BlockHash, invalid: Option<BlockValidationError>, ibd: bool) {
        let stamp = self.chain.drain_snapshot().1 + 1;
        self.signals.queue.push(
            stamp,
            SyncSignal::BlockChecked {
                hash,
                invalid,
                ibd,
            },
        );
        self.signals.bells.ring_all();
    }

    /// Core's `NewPoWValidBlock`, whole, on V's thread inside the accept and before the block
    /// is written (C8): the encoding with a fresh nonce, the height test, the announce stamp and
    /// `recent`, then a visit of every fully connected peer in table order under its header
    /// cell, against V's in-scope `tree`. `stamp` is one more than the `view_seq` V last
    /// published (C7 (a)). It waits only for a header cell an actor region holds, and the actor
    /// never waits on V while it holds one (P3 §2.5, §3.6, D50, D56, D57).
    // Core: net_processing.cpp:2228-2268 PeerManagerImpl::NewPoWValidBlock
    pub fn new_pow_valid_block(
        &self,
        facts: &EntryFacts,
        block: &Arc<Block>,
        tree: &dyn TreeRead,
        stamp: u64,
    ) {
        let core = self.core.get().and_then(Weak::upgrade);
        // Before the network core exists Core's `GetRand` still draws, from the process's RNG.
        let rand: Arc<dyn RandSource> = match &core {
            Some(c) => Arc::clone(c.rand()),
            None => process_source(),
        };
        // Core builds the compact block, and so draws its nonce, before the height test.
        let cmpct: Arc<[u8]> =
            CompactBlock::from_block(block, draw_nonce(&rand)).wire_bytes().into();
        {
            let mut recent = self.signals.recent.lock();
            let height = i32::try_from(facts.height).unwrap_or(i32::MAX);
            if height <= recent.highest_fast_announce {
                return;
            }
            // The stamp before `recent`'s new value can be seen (D56, D58).
            self.signals.store_announce_stamp(stamp);
            *recent = RecentBlock {
                highest_fast_announce: height,
                hash: Some(facts.hash),
                block: Some(Arc::clone(block)),
                cmpct: Some(Arc::clone(&cmpct)),
            };
        }
        // Core's shutdown stops connman first: a stopped actor's peers get nothing.
        let Some(core) = core else {
            return;
        };
        if self.signals.queue.is_closed() {
            return;
        }
        let prev = facts.prev.and_then(|id| tree.entry(id));
        for p in core.peers().iter() {
            if !fully_connected(p) {
                continue;
            }
            let Some(cell) = self.peers.lock().get(&p.id()).map(|s| s.header_cell.clone()) else {
                continue;
            };
            #[cfg(feature = "sim")]
            self.signals.on_lock(LockPoint::Visit(p.id()));
            let mut h = HeaderGuard::lock(&cell);
            // An actor region may have disconnected the peer while V waited for its cell
            // (np:2252, D57).
            if !fully_connected(p) {
                continue;
            }
            process_block_availability(tree, &mut h);
            if h.requested_hb_cmpctblocks
                && !peer_has_header(tree, &h, facts)
                && prev.is_some_and(|prev| peer_has_header(tree, &h, &prev))
            {
                tracing::debug!(target: "net", "PeerManager::NewPoWValidBlock sending header-and-ids {} to peer={}", facts.hash, p.id());
                core.push(p, OutMsg::new(CommandString::CMPCTBLOCK, cmpct.to_vec()));
                h.best_header_sent = Some(facts.id);
            }
        }
    }

    /// The block-download half of Core's `BlockConnected`: `m_last_tip_update = GetTime()`,
    /// then the stalling timeout's decay (P3 §3.3.3). It also rings, so entries the publication
    /// before it made eligible are drained promptly (P3 §2.5).
    // Core: net_processing.cpp:2185, 2199-2207
    pub fn block_connected(&self, now: i64) {
        self.flags.set_last_tip_update(now);
        self.signals.stalling_timeout.decay();
        self.signals.bells.ring_all();
    }

    /// Queue the transaction half of `BlockConnected` after its chain publication.
    /// The retained block also supplies the orphanage's erase/candidate hooks.
    /// Core v24.0.0 `src/net_processing.cpp:2167-2202`, `BlockConnected`.
    pub fn transactions_connected(&self, block: &Arc<Block>) {
        let stamp = self.chain.drain_snapshot().1;
        self.signals.queue.push_queued(
            stamp,
            SyncSignal::BlockConnected {
                block: Arc::clone(block),
            },
        );
        self.signals.bells.ring_all();
    }

    /// Queue the confirmed-filter reset after a disconnected block's publication.
    /// Core v24.0.0 `src/net_processing.cpp:2215-2227`, `BlockDisconnected`.
    pub fn block_disconnected(&self) {
        let stamp = self.chain.drain_snapshot().1;
        self.signals.queue.push_queued(stamp, SyncSignal::BlockDisconnected);
        self.signals.bells.ring_all();
    }

    /// `sim` only: an acknowledgment from the actor at the end of the first full pass that
    /// starts after this call ([`PassAck`]), and a wake so that pass starts promptly.
    #[cfg(feature = "sim")]
    pub fn request_pass_ack(&self) -> mpsc::Receiver<PassAck> {
        let (tx, rx) = mpsc::channel();
        self.signals.pass_acks.lock().push(tx);
        self.wake();
        rx
    }

    /// The signal queue, the bells, the announce stamp and `recent` (P3 §2.5).
    pub fn signals(&self) -> &Arc<Signals> {
        &self.signals
    }

    /// `Signals::lock_headers` (P3 D56, D58).
    pub fn lock_headers<R>(
        &self,
        cell: &HeaderCell,
        seq: u64,
        read: impl FnOnce(&Signals) -> R,
    ) -> Result<(HeaderGuard, R), Ahead> {
        self.signals.lock_headers(cell, seq, read)
    }

    /// Reads `id`'s header fields under the cell's lock alone, with no stamp check: tests and
    /// diagnostics only, never a path whose output depends on the chain state.
    pub fn peek_headers(&self, id: PeerId) -> Option<PeerHeaders> {
        let cell = self.peers.lock().get(&id)?.header_cell.clone();
        Some(cell.peek())
    }

    /// Core's `GetNodeStateStats` for `peer` (`getpeerinfo`). Takes the peer's state lock, then
    /// its header cell for the `view_seq` of the snapshot the heights resolve against, as Core
    /// takes `cs_main`, and reads the node clock under both, as Core reads `GetTime()` under
    /// `cs_main`: a ping the actor sent while this call waited is never newer than its `now`, so
    /// `ping_wait_us` is never negative (BL-294). On `Ahead` it releases both, waits for that
    /// publication and retries with a fresh snapshot (P3 §3.6). It never blocks behind the
    /// actor's `ProcessBlock` wait, which holds neither. `None` for an unknown peer, or once the
    /// network core is gone.
    pub fn node_state_stats(&self, peer: &PeerHandle) -> Option<NodeStateStats> {
        self.state_stats_with(peer, false)
    }

    /// [`Shared::node_state_stats`], except that the peer's state lock is only tried: `None`
    /// while another thread holds it. A `sim` test reads with it from a thread that must never
    /// block (BL-503's U11).
    #[cfg(feature = "sim")]
    pub fn try_node_state_stats(&self, peer: &PeerHandle) -> Option<NodeStateStats> {
        self.state_stats_with(peer, true)
    }

    fn state_stats_with(&self, peer: &PeerHandle, try_only: bool) -> Option<NodeStateStats> {
        let state = self.state_of(peer.id())?;
        let core = self.core.get().and_then(Weak::upgrade)?;
        loop {
            // Entries are immutable, so the heights resolve against any later view; the
            // snapshot only names the `view_seq` the cell is checked against.
            let (_, seq) = self.chain.drain_snapshot();
            let st = if try_only {
                state.try_lock()?
            } else {
                state.lock()
            };
            match self.signals.lock_headers(&st.header_cell, seq, |_| ()) {
                Ok((headers, ())) => {
                    let now_us = core.clock().node_now_micros();
                    let (mut s, blocks) = stats::node_state_stats(peer, &st, &headers, now_us);
                    drop((headers, st));
                    stats::resolve_heights(&*self.chain, &mut s, blocks);
                    return Some(s);
                }
                Err(Ahead(stamp)) => {
                    drop(st);
                    if self.chain.wait_published(stamp).is_err() {
                        return None;
                    }
                }
            }
        }
    }

    /// Core's `mockscheduler`: moves the 45 s `CheckForStaleTipAndEvictPeers` deadline `d`
    /// earlier (scheduler time only; node time is `setmocktime`'s) and wakes the loop.
    pub fn mock_scheduler(&self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        self.mock_forward_us.fetch_add(us, Ordering::SeqCst);
        self.wake();
    }

    /// Core's `interruptMsgProc`: the loop returns at its next check. `FetchBlock` requests
    /// are still served after it, until [`Shared::close_fetches`] (P2a-net D51).
    pub fn interrupt(&self) {
        self.interrupt.store(true, Ordering::SeqCst);
        self.wake();
    }

    /// Closes the `FetchBlock` queue: a request made from now on answers `Stopped` at once, and
    /// the actor answers those already queued in its exit drain, then returns (P2a-net D51).
    /// [`PeerManager::stop`] calls it, after the host's RPC workers have finished.
    pub fn close_fetches(&self) {
        self.fetches.close();
        self.wake();
    }

    /// Queues Core's `FetchBlock(peer, hash)` for the actor and wakes it (Core's
    /// `WakeMessageHandler`, so the idle wait does not run its 100 ms). Never blocks. A closed
    /// queue answers `Err(Stopped)` at once (BL-503).
    pub fn request_fetch(&self, hash: BlockHash, peer: PeerId) -> FetchTicket {
        let ticket = self.fetches.push(hash, peer);
        self.wake();
        ticket
    }

    /// [`Shared::request_fetch`], then the wait for the actor's answer: the RPC thread's
    /// `getblockfrompeer` call. It waits until the actor's next drain point (plan §2.4), with
    /// no timeout, as Core's RPC thread waits on `cs_main` for as long as the handler holds it.
    /// After [`Shared::interrupt`] the actor keeps serving requests until
    /// [`Shared::close_fetches`], and answers those queued then in its exit drain (P2a-net D51);
    /// one made after the close answers `Stopped` at once.
    ///
    /// The caller must hold no lock the actor or V takes, and must not be the actor (it would
    /// wait on itself; debug builds assert it) or V's thread (the actor may be waiting on V).
    // Core: net_processing.cpp:2057-2089 PeerManagerImpl::FetchBlock
    pub fn fetch_block(&self, hash: BlockHash, peer: PeerId) -> Result<(), FetchBlockError> {
        debug_assert!(!on_actor(), "FetchBlock waited on from the actor thread");
        self.request_fetch(hash, peer).wait()
    }

    /// Whether [`Shared::interrupt`] was called.
    pub fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }

    /// Whether the message thread stopped on a panic (a bug). The host polls it (ND1's
    /// watchdog) and shuts the node down; until then the network core keeps calling
    /// `initialize` and `finalize`, which only touch the peer map.
    pub fn panicked(&self) -> bool {
        self.panicked.load(Ordering::SeqCst)
    }

    /// Core's `PeerManager::PeerMisbehaving` for any thread (handlers, RPC, the chain side):
    /// scores `id` now, under the misbehaviour cell only, so it never waits on the actor; the
    /// discouragement acts at the peer's next `SendMessages`.
    pub fn misbehaving(&self, id: PeerId, howmuch: i32, msg: &str) {
        let cell = self.peers.lock().get(&id).map(|s| Arc::clone(&s.misbehavior));
        if let Some(cell) = cell {
            misbehavior::misbehaving(id, &cell, howmuch, msg);
        }
    }

    /// Core's `PeerManager::RelayInv` for any thread (the `sporkupdate` RPC, the chain side):
    /// queues `inv` on every peer's inventory relay, under the map and each leaf cell only
    /// (P5a D10, D11); each peer's next `SendMessages` announces it.
    pub fn relay_inv(&self, inv: &Inventory) {
        relay::relay_inv(&self.peers, inv);
    }

    /// Core's filtered inventory relay for a transaction or transaction hash.
    pub fn relay_inv_filtered(&self, inv: &Inventory, related: Related<'_>) {
        let Some(core) = self.core.get().and_then(Weak::upgrade) else {
            return;
        };
        relay::relay_inv_filtered(&core, &self.peers, inv, related);
    }

    /// Pushes to the fully connected participant at `addr`.
    // Core v24.0.0: coinjoin/server.cpp:1312-1315,1375-1378.
    pub fn push_to_addr(&self, addr: &Service, msg: OutMsg) -> bool {
        let Some(core) = self.core.get().and_then(Weak::upgrade) else {
            return false;
        };
        let peers = core.peers();
        let Some(peer) = peers.iter().find(|p| p.addr().service == *addr && fully_connected(p))
        else {
            return false;
        };
        core.push(peer, msg);
        true
    }

    /// Relays a queue; calls inside a handler run after its state lock is released.
    // Core v24.0.0: net_processing.cpp:6999-7002,2560-2585.
    pub fn relay_dsq(&self, dsq: &DsQueue) {
        if in_pass() {
            self.pending_dsq.lock().push(dsq.clone());
            self.wake();
        } else if let Some(core) = self.core.get().and_then(Weak::upgrade) {
            relay::relay_dsq(&core, &self.peers, dsq);
        }
    }

    /// Transaction-relay entry point for the mempool relay owner to connect.
    // Core v24.0.0: net_processing.cpp:7004-7007.
    pub fn relay_transaction(&self, txid: &TxHash) {
        tracing::debug!(target: "net", "transaction relay pending mempool wiring: {txid}");
    }

    fn drain_dsq(&self, core: &NetCore) {
        let queues = std::mem::take(&mut *self.pending_dsq.lock());
        for dsq in queues {
            relay::relay_dsq(core, &self.peers, &dsq);
        }
    }

    /// Applies a result from no peer: forgets requests for available inventory and relays it.
    // Core: net_processing.cpp:3824-3841 PostProcessMessage
    pub fn post_process_external(&self, result: MessageProcessingResult) {
        relay::post_process_inventory(
            &self.peers,
            self.object_request.get().map(Arc::as_ref),
            &result.inventory,
        );
    }

    /// `id`'s misbehaviour score and discourage flag.
    pub fn misbehavior(&self, id: PeerId) -> Option<(i32, bool)> {
        let cell = self.peers.lock().get(&id).map(|s| Arc::clone(&s.misbehavior))?;
        let m = cell.lock();
        Some((m.score, m.should_discourage))
    }

    fn state_of(&self, id: PeerId) -> Option<Arc<Mutex<PeerState>>> {
        self.peers.lock().get(&id).map(|s| Arc::clone(&s.state))
    }

    /// Whether `id` has processor state.
    pub fn has_peer(&self, id: PeerId) -> bool {
        self.peers.lock().contains_key(&id)
    }

    /// Reads `id`'s state, header fields included (for tests and diagnostics: it takes the
    /// state's lock and the header cell briefly, with no announce-stamp check, so a chain-state
    /// reader uses [`Shared::node_state_stats`]'s path instead).
    pub fn with_state<R>(&self, id: PeerId, f: impl FnOnce(&PeerState) -> R) -> Option<R> {
        self.with_state_inner(id, |st| f(st))
    }

    /// Changes `id`'s state, header fields included, to set up a test.
    #[cfg(feature = "sim")]
    pub fn with_state_mut<R>(&self, id: PeerId, f: impl FnOnce(&mut PeerState) -> R) -> Option<R> {
        self.with_state_inner(id, f)
    }

    fn with_state_inner<R>(&self, id: PeerId, f: impl FnOnce(&mut PeerState) -> R) -> Option<R> {
        let state = self.state_of(id)?;
        // The actor leaves its region before it unlocks the state, so none is open here; the
        // region's drop releases the cell on every path, a panic's included.
        let mut region = Region::peek(state.lock());
        Some(f(&mut region))
    }
}

impl NetEvents for Shared {
    // Core: net.cpp:2671-2678 WakeMessageHandler
    fn wake(&self) {
        self.signals.bells.wake();
    }

    // Core: net_processing.cpp:1745-1760 PeerManagerImpl::InitializeNode
    fn initialize(&self, core: &NetCore, peer: &PeerHandle, our_services: ServiceFlags) {
        // The `CNodeState`'s `m_recently_announced_invs` and the `Peer`'s always-built
        // `TxRelay::m_tx_inventory_known_filter` each draw a bloom tweak (`GetRand<unsigned
        // int>()`, common/bloom.cpp:385-387), in construction order. Transaction relay owns the
        // first filter; the known filter is P5a's (D10).
        // Core: net_processing.cpp:546 (CNodeState), :314,:446 (Peer::TxRelay)
        let announced_tweak = core.get_rand(u64::from(u32::MAX)) as u32;
        let known_tweak = core.get_rand(u64::from(u32::MAX)) as u32;
        let mut state = PeerState::new(peer.id(), our_services, core.clock().node_now_micros());
        state.recently_announced_invs.reset(announced_tweak);
        state.set_known_filter_tweak(known_tweak);
        state.inbound = peer.is_inbound();
        self.peers.lock().insert(peer.id(), PeerSlot::new(state));
        if !peer.is_inbound() {
            let inputs =
                version_inputs(core, &*self.chain, peer, our_services, self.opts.blocks_only);
            core.push(peer, handshake::version_for(core, peer, &self.identity, &inputs));
        }
    }

    // Core: net_processing.cpp:1783-1798 PeerManagerImpl::FinalizeNode (RemovePeer)
    fn finalize(&self, _core: &NetCore, peer: &PeerHandle) {
        if let Some(slot) = self.peers.lock().remove(&peer.id()) {
            self.finalized.lock().push((peer.clone(), slot.state));
        }
        self.wake();
    }
}

#[cfg(feature = "sim")]
type AfterProcessHook = Box<dyn Fn(&Shared) + Send>;

/// Core's `ThreadMessageHandler` body: one pass over a shuffled snapshot of the connection table
/// (P2a §3.1.1). The thread in [`PeerManager`] is a driver around it; `sim` calls it directly.
pub struct MessageLoop {
    logic: PeerLogic,
    #[cfg(feature = "sim")]
    after_process: Option<AfterProcessHook>,
    shared: Arc<Shared>,
    rng: FastRandom,
    /// `nLastSendMessagesTimeMasternodes`; `None` is Core's epoch start.
    last_mn_send: Option<Instant>,
    /// When `CheckForStaleTipAndEvictPeers` is next due (scheduler time); `None` before the
    /// first pass, which schedules it 45 s ahead (Core's `StartScheduledTasks`).
    next_stale_check: Option<Instant>,
}

impl MessageLoop {
    /// A loop over `core`. `rng` shuffles each pass (Core takes a fresh `FastRandomContext` per
    /// snapshot; `sim` passes a deterministic one).
    pub fn new(
        core: Arc<NetCore>,
        shared: Arc<Shared>,
        registry: Registry,
        seams: Seams,
        rng: FastRandom,
    ) -> Self {
        // A second loop over the same `Shared` would be a host bug; the first binding stays.
        let _ = shared.core.set(Arc::downgrade(&core));
        let logic = PeerLogic::new(
            core,
            Arc::clone(&shared.identity),
            shared.opts.clone(),
            Arc::clone(&shared.chain),
            Arc::clone(&shared.peers),
            Arc::clone(&shared.interrupt),
            registry,
            seams,
            shared.mn.clone(),
            Arc::clone(&shared.flags),
            Arc::clone(&shared.signals),
        );
        let _ = shared.object_request.set(Arc::clone(&logic.object_request));
        Self {
            #[cfg(feature = "sim")]
            after_process: None,
            logic,
            shared,
            rng,
            last_mn_send: None,
            next_stale_check: None,
        }
    }

    /// Runs `CheckForStaleTipAndEvictPeers` if it is due, then schedules it 45 s after now
    /// (Core's `scheduleEvery` re-arms from the end of each run). `mockscheduler` time moves the
    /// deadline first.
    // Core: net_processing.cpp:2149-2150; scheduler.cpp Repeat
    fn scheduled_tasks(&mut self, steady: Instant) {
        let due = self.next_stale_check.get_or_insert(steady + EXTRA_PEER_CHECK_INTERVAL);
        let fwd = Duration::from_micros(self.shared.mock_forward_us.swap(0, Ordering::SeqCst));
        *due = due.checked_sub(fwd).unwrap_or(steady);
        if steady >= *due {
            self.logic.check_for_stale_tip_and_evict_peers();
            self.next_stale_check = Some(steady + EXTRA_PEER_CHECK_INTERVAL);
        }
    }

    /// The processor.
    pub fn logic(&self) -> &PeerLogic {
        &self.logic
    }

    /// The processor, to drive a branch directly (`sim`, the differential harness).
    pub fn logic_mut(&mut self) -> &mut PeerLogic {
        &mut self.logic
    }

    /// The finalized peers' counters and seams (`FinalizeNode` after `RemovePeer`). Each pass
    /// runs it first; `sim` also runs it right after a removal, where Core's socket thread runs
    /// `FinalizeNode`. Returns the peers it finalized.
    pub fn drain_finalized(&mut self) -> Vec<PeerId> {
        let done = std::mem::take(&mut *self.shared.finalized.lock());
        done.into_iter()
            .map(|(peer, state)| {
                self.logic.finalize(&peer, &state.lock());
                peer.id()
            })
            .collect()
    }

    /// Runs every queued `FetchBlock` request in arrival order and replies to each; returns how
    /// many ran. A pass calls it at its head, after the finalized peers and the signal queue,
    /// and after each peer's `SendMessages`: points between Core's `cs_main` sections, so both
    /// are schedules Core's RPC thread can reach. `sim` calls it directly (BL-503).
    pub fn drain_fetches(&mut self) -> usize {
        let requests = self.shared.fetches.take();
        let n = requests.len();
        for r in requests {
            // A gone waiter has nothing to tell.
            let _ = r.reply.send(self.logic.fetch_block(&r.hash, r.peer));
        }
        n
    }

    /// A drain of the signal queue ([`PeerLogic::drain_sync_signals`]): at the start of a pass
    /// after [`MessageLoop::drain_finalized`] (a source finalized first is gone, Core's
    /// `State(nodeid)` is null), and after each peer's `ProcessMessages`; the processor drains
    /// at its own points too (P3 §3.6 point 3).
    pub fn drain_sync_signals(&mut self) {
        self.logic.drain_sync_signals();
    }

    /// One pass: the scheduled tasks when due, then every peer not marked for disconnection
    /// gets `ProcessMessages`, then `SendMessages` (masternode connections at most once per
    /// 100 ms). Returns Core's `fMoreWork`.
    pub fn run_one_pass(&mut self) -> bool {
        self.drain_finalized();
        self.scheduled_tasks(self.logic.core().clock().steady());
        let mut rng = std::mem::replace(&mut self.rng, FastRandom::deterministic());
        let more_work = self.pass_with(&mut rng);
        self.rng = rng;
        more_work
    }

    /// Runs a validation callback before each peer's send section.
    #[cfg(feature = "sim")]
    pub fn set_after_process(&mut self, hook: Box<dyn Fn(&Shared) + Send>) {
        self.after_process = Some(hook);
    }

    /// One pass shuffled by `rng`, without the scheduled tasks: a pass of Core's message
    /// handler alone, for `sim` scripts that run the scheduler's task as a separate tick (as
    /// Core's oracle sessions do).
    // Core: net.cpp:3676-3714 CConnman::ThreadMessageHandler
    pub fn pass_with(&mut self, rng: &mut FastRandom) -> bool {
        let _pass = ThreadFlag::set(&IN_PASS);
        #[cfg(feature = "sim")]
        let acks = std::mem::take(&mut *self.shared.signals.pass_acks.lock());
        self.drain_finalized();
        self.drain_sync_signals();
        self.drain_fetches();
        let core = Arc::clone(self.logic.core());
        self.shared.drain_dsq(&core);
        let steady = core.clock().steady();
        let skip_mn = match self.last_mn_send {
            Some(t) if steady.saturating_duration_since(t) < MN_SEND_INTERVAL => true,
            _ => {
                self.last_mn_send = Some(steady);
                false
            }
        };
        let mut snap: Vec<PeerHandle> = core.peers().to_vec();
        rng.shuffle(&mut snap);
        let mut more_work = false;
        for peer in &snap {
            if peer.disconnect_requested() {
                continue;
            }
            let more = self.logic.process_messages(peer);
            self.shared.drain_dsq(&core);
            more_work |= more && !peer.pause_send();
            if self.shared.interrupted() {
                return more_work;
            }
            self.drain_sync_signals();
            #[cfg(feature = "sim")]
            if let Some(hook) = &self.after_process {
                IN_PASS.with(|flag| {
                    let saved = flag.replace(false);
                    hook(&self.shared);
                    flag.set(saved);
                });
            }
            if !skip_mn || !peer.masternode_connection() {
                self.logic.send_messages(peer);
            }
            if self.shared.interrupted() {
                return more_work;
            }
            self.drain_fetches();
        }
        #[cfg(feature = "sim")]
        if !acks.is_empty() {
            let relay_pending: Vec<PeerId> = (self.shared.peers.lock().iter())
                .filter(|(id, _)| !snap.iter().any(|p| p.id() == **id && p.disconnect_requested()))
                .filter(|(_, slot)| {
                    let inv = slot.block_inv.lock();
                    !inv.headers_relay.is_empty() || !inv.inv_relay.is_empty()
                })
                .map(|(id, _)| *id)
                .collect();
            for ack in acks {
                let _ = ack.send(PassAck {
                    relay_pending: relay_pending.clone(),
                });
            }
        }
        more_work
    }

    /// The end of a pass: waits up to 100 ms for a wake unless there is more work, then clears
    /// the wake flag whether or not it waited.
    // Core: net.cpp:3715-3720
    pub fn wait(&self, more_work: bool) {
        let bells = &self.shared.signals.bells;
        let mut woken = bells.wake.lock();
        if !more_work {
            bells.cond.wait_while_for(&mut woken, |w| !*w, WAIT_INTERVAL);
        }
        *woken = false;
    }

    /// The loop until interrupted; then only `FetchBlock` requests are served, until
    /// [`Shared::close_fetches`]; then the exit drain (P2a-net D51).
    pub fn run(&mut self) {
        let _actor = ThreadFlag::set(&IN_RUN);
        while !self.shared.interrupted() {
            let more = self.run_one_pass();
            if self.shared.interrupted() {
                break;
            }
            self.wait(more);
        }
        // P2a-net D51: Core's HTTP workers run the requests accepted before `Interrupt`, and
        // their `FetchBlock` reaches a live peer manager, until `StopHTTPServer` has joined them
        // and `StopHandlers` runs (init.cpp:286, 329-337; httpserver.cpp:114-117, 540-591).
        // The step waits on no socket, block or V. V's signals are no longer drained, so their
        // queue closes first: a V waiting on it resumes at once, as when the thread ended here.
        self.shared.signals.queue.close();
        while !self.shared.fetches.is_closed() {
            self.drain_fetches();
            self.wait(false);
        }
        self.finish_fetches();
    }

    /// The exit drain (P2a-net D51): closes the queue to new requests and answers every queued
    /// one ([`MessageLoop::drain_fetches`]). [`MessageLoop::run`] ends with it, and
    /// [`MessageLoop::stop_handlers`] starts with it for a loop no thread ran to its end.
    pub fn finish_fetches(&mut self) -> usize {
        self.shared.fetches.close();
        self.drain_fetches()
    }

    /// Core's `PeerManager::InterruptHandlers` then `StopHandlers`, on a loop no thread runs: a
    /// stopped [`PeerManager`]'s, or one the host built and never started. After a panic the
    /// handlers may hold half-applied state; a second panic here must not escape into the host's
    /// shutdown.
    // Core: init.cpp:290, :337
    pub fn stop_handlers(&mut self) {
        // P2a-net D51: queued requests are answered before the handlers stop.
        if std::panic::catch_unwind(AssertUnwindSafe(|| self.finish_fetches())).is_err() {
            tracing::error!(target: "net", "FetchBlock panicked in the exit drain");
            self.shared.fetches.abandon();
        }
        for (step, f) in [
            ("Interrupt", PeerLogic::interrupt_handlers as fn(&mut PeerLogic)),
            ("Stop", PeerLogic::stop_handlers),
        ] {
            if std::panic::catch_unwind(AssertUnwindSafe(|| f(&mut self.logic))).is_err() {
                tracing::error!(target: "net", "a message handler panicked in {step}");
            }
        }
    }
}

/// A loop dropped without its exit drain (one never started, or whose thread failed to spawn)
/// leaves no `FetchBlock` waiter hanging: what is still queued answers `Stopped`.
impl Drop for MessageLoop {
    fn drop(&mut self) {
        self.shared.fetches.abandon();
    }
}

/// Closes the signal queue when the actor thread ends.
struct CloseOnDrop(Arc<Signals>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.queue.close();
    }
}

/// The running processor: the `msghand` thread.
pub struct PeerManager {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<MessageLoop>>,
}

impl PeerManager {
    /// Starts the thread (Core's `threadMessageHandler`).
    ///
    /// # Panics
    ///
    /// When the OS refuses a thread.
    pub fn spawn(lp: MessageLoop) -> Self {
        Self::try_spawn(lp, Box::new(|| {})).expect("spawning the message handler thread")
    }

    /// As [`PeerManager::spawn`], returning the OS's refusal, with `on_panic` run on the thread
    /// after a panic stops processing, so the host can fail-stop as Core's `TraceThread` does
    /// (an exception escaping `threadMessageHandler` terminates dashd).
    ///
    /// # Errors
    ///
    /// When the OS refuses a thread (Core's `std::system_error`).
    // Core: util/thread.cpp:16-28 TraceThread
    pub fn try_spawn(
        mut lp: MessageLoop,
        on_panic: Box<dyn FnOnce() + Send>,
    ) -> std::io::Result<Self> {
        let shared = Arc::clone(&lp.shared);
        let thread = std::thread::Builder::new().name("msghand".into()).spawn(move || {
            // The queue closes on every exit, a panic's included, so a V parked in a
            // signal wait resumes at once (P3 §2.5 m-6).
            let _close = CloseOnDrop(Arc::clone(&lp.shared.signals));
            // A panic is a bug, not Core's per-message exception (that is `Throw`): stop
            // processing, keep the loop so `stop` still stops the handlers, and let the host
            // see it through `Shared::panicked` and `on_panic`.
            if let Err(e) = std::panic::catch_unwind(AssertUnwindSafe(|| lp.run())) {
                let what = e
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| e.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("non-string payload");
                tracing::error!(target: "net", "the message handler thread panicked: {what}");
                // Interrupt first: `panicked` implies `interrupted`. The handlers may hold
                // half-applied state, so no exit drain: unrun requests answer `Stopped`.
                lp.shared.interrupt();
                lp.shared.fetches.abandon();
                lp.shared.panicked.store(true, Ordering::SeqCst);
                on_panic();
            }
            lp
        })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// The shared half.
    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Core's unfiltered inventory relay for every peer in the processor map.
    pub fn relay_inv(&self, inv: &Inventory) {
        self.shared.relay_inv(inv);
    }

    /// Core's filtered inventory relay for a transaction or transaction hash.
    pub fn relay_inv_filtered(&self, inv: &Inventory, related: Related<'_>) {
        self.shared.relay_inv_filtered(inv, related);
    }

    /// Sends a message to a connected participant by address.
    pub fn push_to_addr(&self, addr: &Service, msg: OutMsg) -> bool {
        self.shared.push_to_addr(addr, msg)
    }

    /// Relays a queue using each peer's requested mode.
    pub fn relay_dsq(&self, dsq: &DsQueue) {
        self.shared.relay_dsq(dsq);
    }

    /// Passes collateral transaction relay to the shared relay entry point.
    pub fn relay_transaction(&self, txid: &TxHash) {
        self.shared.relay_transaction(txid);
    }

    /// Interrupts the loop, joins the thread and stops the handlers (the host's shutdown order
    /// calls this before `NetCore::shutdown`). Returns the loop for inspection.
    pub fn stop(mut self) -> Option<MessageLoop> {
        self.shared.interrupt();
        self.shared.close_fetches();
        let joined = self.thread.take().and_then(|t| t.join().ok());
        // The thread's exit drain answered the queue; a thread lost without it answers nothing.
        self.shared.fetches.abandon();
        let mut lp = joined?;
        lp.stop_handlers();
        Some(lp)
    }
}

impl Drop for PeerManager {
    fn drop(&mut self) {
        self.shared.interrupt();
        self.shared.close_fetches();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.shared.fetches.abandon();
    }
}

#[cfg(all(test, feature = "sim"))]
mod pass_ack_tests {
    use super::*;
    use crate::fingerprint::{IdentityOptions, local_identity};
    use crate::sim::{PeerVersionMsg, SIM_START, Sim};
    use dash_p2p::ConnectionType;
    use dash_params::{ChainParamsBuilder, Network, Release};
    use dash_types::Numeric;

    #[test]
    fn pass_ack_includes_peers_registered_after_snapshot() {
        let params =
            Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
        let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
        let mut sim = Sim::new(params, identity);
        let peer = sim.connect(1, ConnectionType::OutboundFullRelay, Default::default());
        sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
        let shared = Arc::clone(&sim.shared);
        let mut lp = MessageLoop::new(
            Arc::clone(&sim.core),
            Arc::clone(&shared),
            Registry::new(),
            Seams::default(),
            FastRandom::deterministic(),
        );
        let registered = Arc::new(Mutex::new(None));
        let observed = Arc::clone(&registered);
        let sim = Mutex::new(sim);
        lp.set_after_process(Box::new(move |shared| {
            let mut sim = sim.lock();
            let peer = sim.connect(2, ConnectionType::OutboundFullRelay, Default::default());
            sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
            shared.updated_block_tip(&sim.core, &[sim.chain.genesis().hash], false);
            assert!(
                shared
                    .with_state(peer.handle.id(), |s| !s.blocks_for_relay().1.is_empty())
                    .unwrap()
            );
            *observed.lock() = Some(peer);
        }));
        let ack = shared.request_pass_ack();
        lp.run_one_pass();
        let registered = registered.lock();
        let peer = registered.as_ref().unwrap();
        assert!(!peer.handle.disconnect_requested());
        assert!(
            ack.try_recv().unwrap().relay_pending.contains(&peer.handle.id()),
            "pass ack retains the post-snapshot peer"
        );
    }

    #[test]
    fn pass_ack_omits_peers_marked_to_disconnect() {
        let params =
            Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
        let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
        let mut sim = Sim::new(params, identity);
        let peer = sim.connect(1, ConnectionType::OutboundFullRelay, Default::default());
        sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
        sim.shared.updated_block_tip(&sim.core, &[BlockHash::from_lendian([1; 32])], false);
        sim.core.disconnect_by_id(peer.handle.id());
        assert!(
            sim.shared
                .with_state(peer.handle.id(), |s| !s.blocks_for_relay().1.is_empty())
                .unwrap()
        );
        let ack = sim.shared.request_pass_ack();
        sim.pass();
        assert!(ack.try_recv().unwrap().relay_pending.is_empty());
    }
}
