//! `ValidationSignals`: Core's `CMainSignals` (CH1-node §8.2, DEC-153).
//!
//! - **Queued** signals go to a FIFO run on the signals thread, Core's
//!   `SingleThreadedSchedulerClient`: [`ValidationSignals::enqueue`] is O(1) under a leaf and
//!   never waits for the consumer (DEC-152); V, inside a hold, stages them in [`Staged`] and
//!   [`ValidationSignals::splice`]s the whole list at the release. Each runs over the listeners
//!   current at dispatch, as Core's `Iterate` does.
//! - **Synchronous** signals iterate the listeners inline on the caller (V), with V-evaluated
//!   facts as arguments (LO-5, §2.5).
//! - **Backpressure** is only [`ValidationSignals::limit_queue`] and
//!   [`ValidationSignals::sync`], Core's `LimitValidationInterfaceQueue` and
//!   `SyncWithValidationInterfaceQueue`; both wait through a [`ServiceWait`], which on V services
//!   class R and M commands (§2.4). A marked task never waits (LO-9).
//!
//! The signals thread carries the no-wait marker: listeners post, they never await a reply.

mod fifo;

use std::collections::LinkedList;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use dash_chain::EntryView;
use dash_consensus::{Block, BlockId};
use dash_dml::{MnList, MnListDiff};
use dash_primitives::BlockHash;

use super::command::BlockState;
use super::locks::ChainLockSig;
use super::reply::{
    NoWaitGuard, NodeJoinHandle, WaitError, no_wait_marked, panic_message, spawn_thread,
};
use super::seams::{AbortSink, ShutdownFlag};
use fifo::Fifo;
#[cfg(all(test, feature = "alloc-tag", not(loom), not(miri)))]
pub(crate) use fifo::LEAF_ALLOCS;

/// A `CValidationInterface` listener. Every method defaults to nothing, as in Core. Synchronous
/// methods run on V: they never send a `ChainCommand` and await it, and never take the tip lock
/// (LO-5).
pub trait ValidationInterface: Send + Sync {
    /// `BlockChecked(block, state)` (`validationinterface.cpp:258`). `ibd` is V's
    /// `IsInitialBlockDownload()` at the call (rev3: P2b's gate).
    fn block_checked(&self, block: &Block, state: &BlockState, ibd: bool) {
        let _ = (block, state, ibd);
    }

    /// `NewPoWValidBlock(pindex, block)` (`:264-267`), inline in `AcceptBlock` (`v:4830-4832`):
    /// C8 (CH1-node §8.4). V calls it only while a listener is registered.
    fn new_pow_valid_block(&self, nb: &NewBlock<'_>) {
        let _ = nb;
    }

    /// `AcceptedBlockHeader(pindexNew)` (`:268`).
    fn accepted_block_header(&self, id: BlockId) {
        let _ = id;
    }

    /// `NotifyHeaderTip(pindexNew, fInitialDownload)` (`:274`).
    fn notify_header_tip(&self, id: BlockId, ibd: bool) {
        let _ = (id, ibd);
    }

    /// `SynchronousUpdatedBlockTip(pindexNew, pindexFork, fInitialDownload)` (`:205`).
    fn synchronous_updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        let _ = (new, fork, ibd);
    }

    /// `InitializeCurrentBlockTip(tip, ibd)` (`:201`).
    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        let _ = (tip, ibd);
    }

    /// `NotifyMasternodeListChanged(undo, oldMNList, diff)` (`validationinterface.cpp:327-329`),
    /// inside the attempt that changed the list (CH7 D15): after its best-block write, before its
    /// commit. Only Core-equivalent listeners take it, as Core's one listener is
    /// `CDSNotificationInterface` (`dsnotificationinterface.cpp:90-96`): it must not dispatch to
    /// or wait on V, and must not read an effect target for this block. The rest take the
    /// asynchronous MN-list event (CH7 §5.2), which comes with its first consumer (D61).
    fn notify_masternode_list_changed(
        &self,
        undo: bool,
        old: &Arc<MnList>,
        diff: &Arc<MnListDiff>,
    ) {
        let _ = (undo, old, diff);
    }

    /// Queued `UpdatedBlockTip(pindexNew, pindexFork, fInitialDownload)` (`:186`).
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        let _ = (new, fork, ibd);
    }

    /// Queued `BlockConnected(pblock, pindex)` (`:225`).
    fn block_connected(&self, block: &Arc<Block>, id: BlockId) {
        let _ = (block, id);
    }

    /// Queued `BlockDisconnected(pblock, pindex)`.
    fn block_disconnected(&self, block: &Arc<Block>, id: BlockId) {
        let _ = (block, id);
    }

    /// Queued `ChainStateFlushed(locator)`.
    fn chain_state_flushed(&self, locator: &[BlockHash]) {
        let _ = locator;
    }

    /// Queued `NotifyChainLock(pindex, clsig)` (`:287`).
    fn notify_chain_lock(&self, id: BlockId, sig: &Arc<ChainLockSig>) {
        let _ = (id, sig);
    }

    /// C5 (A-1b (1), DEC-229, D67): V is at a release point, where Core would run
    /// `LimitValidationInterfaceQueue` (`v:3760`, `v:3940`), or at its idle point. The listener
    /// may wait through `wait`, which serves V's class R and M commands meanwhile. Never called
    /// while V holds commands (D38) or from inside another such wait.
    ///
    /// Re-entry (D67 N2): a command served in the wait may call this listener's own inline
    /// methods (`block_checked`, `accepted_block_header`, `notify_header_tip`) on V, inside the
    /// wait, so the listener holds no lock across `wait`/`wait_until` that those take, and
    /// `wait_until`'s `done` takes and releases its own. `done`'s lock is one V waits behind
    /// (LO-5): no holder of it waits on V. `new_pow_valid_block` and these two hooks are never
    /// re-entered (D67 r2 R2-1): V nests no hook (`in_wait`); outside IBD it defers a
    /// `ProcessBlock` met in the wait whole (R3); in IBD it does run the accept (its ABC tail
    /// deferred), but C8 needs `!is_ibd()`, which only an ABC moving the tip can clear, and
    /// ABC is class C, which the wait defers. So in IBD the wait can see headers inserted,
    /// `HAVE_DATA` set and `block_checked` for a rejected block, but no new tip. An `Err` from
    /// the wait is V's stop or an `AbortNode` a served command raised (D67 N1): return it, and
    /// do not wait again; a wait after it serves nothing and returns `Err` at once.
    fn release_point(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        let _ = wait;
        Ok(())
    }

    /// C5′ (DEC-240, D67): V is about to run a `ProcessBlock` accept outside IBD. The listener
    /// may wait through `wait` as at [`Self::release_point`]; V runs the accept after it returns.
    fn before_accept(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        let _ = wait;
        Ok(())
    }
}

/// `NewPoWValidBlock`'s arguments (C8, CH1-node §8.4, D67): what Core's callback reads under
/// `cs_main` (np:2253-2258), from V's own tree.
pub struct NewBlock<'a> {
    pub id: BlockId,
    /// The entry as V reads it after `AcceptBlock`'s checks.
    pub entry: EntryView,
    pub block: &'a Arc<Block>,
    /// The `view_seq` of the `ChainView` the accept's `scope_end` stores: nothing publishes
    /// between the call and it (P3 C7 (a)). If V stops or aborts after the call, before
    /// `scope_end`, no view carries the stamp (D67 N8): a C7 wait for it ends on V's stop too.
    pub stamp: u64,
    /// V's in-scope block tree, which sees the header this accept inserted. Valid only for the
    /// call.
    pub tree: &'a dyn TreeRead,
}

/// V's block tree as a `NewPoWValidBlock` listener reads it during the call (C8): the arena V
/// writes, not a published view.
pub trait TreeRead {
    /// Core `LookupBlockIndex`, over the entries in memory, as `ChainViewHandle::lookup` (D67
    /// R6).
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId>;
    fn entry(&self, id: BlockId) -> Option<EntryView>;
    /// Core `GetAncestor`.
    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId>;
}

/// A queued signal (§8.2). The mempool, InstantSend, governance and recovered-sig signals join
/// with their producers (MP1, IS, governance, LLMQ; D23).
pub enum Queued {
    UpdatedBlockTip {
        new: BlockId,
        fork: Option<BlockId>,
        ibd: bool,
    },
    BlockConnected {
        block: Arc<Block>,
        id: BlockId,
    },
    BlockDisconnected {
        block: Arc<Block>,
        id: BlockId,
    },
    ChainStateFlushed(Vec<BlockHash>),
    /// `ClEnforce`'s notification, with the signature it captured (§5.2).
    NotifyChainLock {
        id: BlockId,
        sig: Arc<ChainLockSig>,
    },
    /// `SyncWithValidationInterfaceQueue`'s function (`:160-174`): signalled when reached, so
    /// after every earlier entry.
    Barrier(crossbeam_channel::Sender<()>),
}

/// Signals V produces inside a hold (rev3, N1): V-local, no lock; the release splices the whole
/// list into the queue at once.
#[derive(Default)]
pub struct Staged(LinkedList<Queued>);

impl Staged {
    pub fn push(&mut self, e: Queued) {
        self.0.push_back(e);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A `sync` caller's barrier: reached when the signals thread has run every earlier entry;
/// disconnected if the thread stopped first.
pub struct Barrier {
    #[expect(clippy::disallowed_types, reason = "read only through `sync`'s LO-9 check")]
    done: crossbeam_channel::Receiver<()>,
}

impl Barrier {
    /// Blocks until the barrier is reached.
    pub fn wait(&self) -> Result<(), WaitError> {
        #[expect(clippy::disallowed_methods, reason = "`sync` checked the marker")]
        let reached = self.done.recv();
        reached.map_err(|_| WaitError::Disconnected)
    }

    /// Whether it is reached (`Some(Ok)`) or can no longer be (`Some(Err)`).
    pub fn poll(&self) -> Option<Result<(), WaitError>> {
        match self.done.try_recv() {
            Ok(()) => Some(Ok(())),
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Some(Err(WaitError::Disconnected))
            }
        }
    }

    /// For a waiter that selects over its own channels too (V).
    #[expect(clippy::disallowed_types, reason = "V's select; V carries no marker")]
    pub(crate) fn receiver(&self) -> &crossbeam_channel::Receiver<()> {
        &self.done
    }
}

/// How a `sync` caller waits for its barrier. On V it is a release-point wait that services
/// class R and M commands meanwhile (§8.2); elsewhere [`Blocking`].
pub trait ServiceWait {
    fn wait(&mut self, barrier: &Barrier) -> Result<(), WaitError>;

    /// C5's condition wait (A-1b (2), D67): serves as [`Self::wait`] does until `done()`, which
    /// is checked first and after every ring of `wake`. `Err` once `wake`'s sender is gone with
    /// `done()` still false, or when the waiter must stop.
    ///
    /// `wake`'s contract (D67 N3): unbounded, or bounded with capacity at least 1, never a
    /// rendezvous channel (a full one already carries a ring, so the ringer may `try_send` and
    /// drop it), rung after the change `done`
    /// reads is visible to `done` (made under `done`'s lock, or with Release before the ring),
    /// and read by this waiter alone while it waits: another reader can take the ring V needs.
    /// Dropping every sender means closed: the wait ends `Ok` if `done()`, else `Err`.
    #[expect(clippy::disallowed_types, reason = "a listener's wake, never a reply from V")]
    fn wait_until(
        &mut self,
        wake: &crossbeam_channel::Receiver<()>,
        done: &mut dyn FnMut() -> bool,
    ) -> Result<(), WaitError>;
}

/// A plain blocking wait, for callers that serve nothing (RPC, wallet loading).
pub struct Blocking;

impl ServiceWait for Blocking {
    fn wait(&mut self, barrier: &Barrier) -> Result<(), WaitError> {
        barrier.wait()
    }

    #[expect(clippy::disallowed_types, reason = "a listener's wake, never a reply from V")]
    fn wait_until(
        &mut self,
        wake: &crossbeam_channel::Receiver<()>,
        done: &mut dyn FnMut() -> bool,
    ) -> Result<(), WaitError> {
        loop {
            if done() {
                return Ok(());
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "a listener's wake, never a reply from V"
            )]
            if wake.recv().is_err() {
                return if done() {
                    Ok(())
                } else {
                    Err(WaitError::Disconnected)
                };
            }
        }
    }
}

/// Core's `CMainSignals`: one per node, owned by ND1; V, MP1, LK1a, IS and governance hold
/// clones.
#[derive(Clone, Default)]
pub struct ValidationSignals {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    queue: Fifo<Queued>,
    listeners: ArcSwap<Vec<Arc<dyn ValidationInterface>>>,
    /// `listeners` is not empty: V's gate for the signals it builds arguments for (D67).
    registered: AtomicBool,
    /// Serializes register/unregister's copy-and-swap (Core's `m_mutex` around the list).
    edit: Mutex<()>,
    started: AtomicBool,
    /// Core's `CDeterministicMNManager::tipIndex` (CH7 D16): set only by V, through the
    /// synchronous tip dispatchers and the reorg drain.
    dmn_tip: Mutex<Option<BlockId>>,
    /// The node's shutdown flags a listener's panic sets (`StartShutdown`, D52) after the
    /// `AbortNode` sink runs, as Core's `AbortNode` calls `StartShutdown` last
    /// (`shutdown.cpp:35-36`), and even if the sink panics too: V's, linked when V spawns.
    shutdown: Mutex<Vec<Arc<dyn ShutdownFlag>>>,
}

/// The running signals thread ([`ValidationSignals::start`]). Dropping it asks the thread to
/// stop without waiting for it.
pub struct SignalsThread {
    signals: ValidationSignals,
    join: Option<NodeJoinHandle<()>>,
}

impl SignalsThread {
    /// Core's `FlushBackgroundCallbacks` then the scheduler's stop: runs what is queued, exits;
    /// later entries are dropped (a later `sync` returns `Disconnected`). As
    /// [`NodeJoinHandle::join`]: `Ok(Err(_))` is a listener's panic, or the `AbortNode` sink's
    /// if that panicked in turn.
    pub fn stop(mut self) -> Result<std::thread::Result<()>, WaitError> {
        self.signals.inner.queue.stop();
        self.join.take().expect("joined once").join()
    }
}

impl Drop for SignalsThread {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.signals.inner.queue.stop();
        }
    }
}

/// `StartShutdown()` on the linked flags once a listener's panic has been reported, or as a
/// panicking sink unwinds.
struct ShutdownOnDrop<'a>(&'a ValidationSignals);

impl Drop for ShutdownOnDrop<'_> {
    fn drop(&mut self) {
        self.0.request_shutdown();
    }
}

impl ValidationSignals {
    /// Starts the thread that runs queued signals, marked no-wait (LO-9); at most once per
    /// instance, as one consumer keeps the FIFO order. A listener's panic is `AbortNode`, as an
    /// exception escaping Core's scheduler thread ends the process: `abort` hears it, then the
    /// linked shutdown flags are set (Core's order, `shutdown.cpp:35-36`), the queue closes
    /// (pending barriers disconnect) and the thread ends, each even if `abort` panicked.
    pub fn start(&self, abort: Arc<dyn AbortSink>) -> std::io::Result<SignalsThread> {
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return Err(std::io::Error::other("the validation signals thread already started"));
        }
        let signals = self.clone();
        let join = spawn_thread("dashd-signals", move || {
            let _marked = NoWaitGuard::set();
            signals.inner.queue.serve(
                |e| signals.dispatch(e),
                |p| {
                    let _flags = ShutdownOnDrop(&signals);
                    let what = panic_message(p);
                    abort.abort_node(
                        &format!("a validation interface listener panicked: {what}"),
                        None,
                    );
                },
            );
        })?;
        Ok(SignalsThread {
            signals: self.clone(),
            join: Some(join),
        })
    }

    /// Links a shutdown flag that a listener's panic sets ([`ValidationSignals::start`]).
    pub(crate) fn link_shutdown(&self, flag: Arc<dyn ShutdownFlag>) {
        self.inner.shutdown.lock().unwrap_or_else(|p| p.into_inner()).push(flag);
    }

    /// `StartShutdown()` on every linked flag.
    fn request_shutdown(&self) {
        let flags = self.inner.shutdown.lock().unwrap_or_else(|p| p.into_inner());
        for flag in flags.iter() {
            flag.start_shutdown();
        }
    }

    fn dispatch(&self, e: Queued) {
        match e {
            Queued::UpdatedBlockTip {
                new,
                fork,
                ibd,
            } => self.each(|l| l.updated_block_tip(new, fork, ibd)),
            Queued::BlockConnected {
                block,
                id,
            } => self.each(|l| l.block_connected(&block, id)),
            Queued::BlockDisconnected {
                block,
                id,
            } => self.each(|l| l.block_disconnected(&block, id)),
            Queued::ChainStateFlushed(locator) => self.each(|l| l.chain_state_flushed(&locator)),
            Queued::NotifyChainLock {
                id,
                sig,
            } => self.each(|l| l.notify_chain_lock(id, &sig)),
            Queued::Barrier(done) => {
                let _ = done.send(());
            }
        }
    }

    /// Queues `e` in O(1) and wakes the thread; never waits for it. After the thread stopped
    /// the entry is dropped.
    pub fn enqueue(&self, e: Queued) {
        let _ = self.inner.queue.push(e);
    }

    /// Appends the whole staged list in one O(1) section and one wake (rev3, N1).
    pub fn splice(&self, staged: &mut Staged) {
        if self.inner.queue.append(&mut staged.0).is_err() {
            staged.0.clear();
        }
    }

    /// Core's `CallbacksPending()`: queued or detached and not yet started.
    pub fn callbacks_pending(&self) -> usize {
        self.inner.queue.pending()
    }

    /// `SyncWithValidationInterfaceQueue` (`:165-174`): a barrier, then `wait` until the thread
    /// reaches it. A marked task is refused at once (LO-9) and queues nothing.
    pub fn sync(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        if no_wait_marked() {
            return Err(WaitError::WaitOnVFromNoWaitTask);
        }
        let (tx, done) = crossbeam_channel::bounded(1);
        self.enqueue(Queued::Barrier(tx));
        wait.wait(&Barrier {
            done,
        })
    }

    /// `LimitValidationInterfaceQueue` (`validation.cpp:3716-3722`).
    pub fn limit_queue(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        if self.callbacks_pending() > 10 {
            self.sync(wait)?;
        }
        Ok(())
    }

    /// `RegisterSharedValidationInterface`. Core keys its listeners by pointer
    /// (`validationinterface.cpp:50-55`): registering one again keeps its one place in the order.
    pub fn register(&self, l: Arc<dyn ValidationInterface>) {
        let _edit = self.inner.edit.lock().unwrap_or_else(|p| p.into_inner());
        let listeners = self.inner.listeners.load();
        if listeners.iter().any(|x| Arc::ptr_eq(x, &l)) {
            return;
        }
        let mut next = Vec::clone(&listeners);
        next.push(l);
        // The flag first: a V that loads the new list then sees it, so no hook skips a listener
        // that already receives signals (Opus r1 N4). `unregister` clears it after the list: a
        // stale `true` costs one empty iteration.
        self.inner.registered.store(true, Ordering::Relaxed);
        self.inner.listeners.store(Arc::new(next));
    }

    /// `UnregisterSharedValidationInterface` (`:145-150`). As in Core, a queued signal already
    /// dispatching may still reach `l`; quiescence needs a following `sync`.
    pub fn unregister(&self, l: &Arc<dyn ValidationInterface>) {
        let _edit = self.inner.edit.lock().unwrap_or_else(|p| p.into_inner());
        let next: Vec<_> =
            self.inner.listeners.load().iter().filter(|x| !Arc::ptr_eq(x, l)).cloned().collect();
        let registered = !next.is_empty();
        self.inner.listeners.store(Arc::new(next));
        self.inner.registered.store(registered, Ordering::Relaxed);
    }

    /// Whether a listener is registered: one relaxed load. V builds no [`NewBlock`] while there
    /// is none (D67). A listener registered concurrently with a signal may miss it, as in Core.
    pub fn has_listeners(&self) -> bool {
        self.inner.registered.load(Ordering::Relaxed)
    }

    fn each(&self, f: impl Fn(&dyn ValidationInterface)) {
        for l in self.inner.listeners.load().iter() {
            f(l.as_ref());
        }
    }

    pub fn block_checked(&self, block: &Block, state: &BlockState, ibd: bool) {
        self.each(|l| l.block_checked(block, state, ibd));
    }

    pub fn new_pow_valid_block(&self, nb: &NewBlock<'_>) {
        self.each(|l| l.new_pow_valid_block(nb));
    }

    /// C5's hook (D67) on each listener in order; the first `Err` ends it. The list is cloned,
    /// as a listener may wait here.
    pub fn release_point(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        self.inner.listeners.load_full().iter().try_for_each(|l| l.release_point(wait))
    }

    /// C5′'s hook (D67), as [`Self::release_point`].
    pub fn before_accept(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        self.inner.listeners.load_full().iter().try_for_each(|l| l.before_accept(wait))
    }

    pub fn accepted_block_header(&self, id: BlockId) {
        self.each(|l| l.accepted_block_header(id));
    }

    pub fn notify_header_tip(&self, id: BlockId, ibd: bool) {
        self.each(|l| l.notify_header_tip(id, ibd));
    }

    /// `SynchronousUpdatedBlockTip`: the DMN tip moves first, under Core's guard, so every
    /// listener sees the new list (D16).
    // Core: dsnotificationinterface.cpp:47-53, evo/deterministicmns.cpp:819-823
    pub(crate) fn sync_updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        if fork != Some(new) {
            self.move_dmn_tip(new);
        }
        self.each(|l| l.synchronous_updated_block_tip(new, fork, ibd));
    }

    /// `InitializeCurrentBlockTip`: the fork is null, so the DMN tip moves whenever there is a
    /// tip (D16).
    // Core: dsnotificationinterface.cpp:31-35
    pub(crate) fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        if let Some(tip) = tip {
            self.move_dmn_tip(tip);
        }
        self.each(|l| l.initialize_current_block_tip(tip, ibd));
    }

    pub(crate) fn notify_masternode_list_changed(
        &self,
        undo: bool,
        old: &Arc<MnList>,
        diff: &Arc<MnListDiff>,
    ) {
        self.each(|l| l.notify_masternode_list_changed(undo, old, diff));
    }

    /// Core's DMN tip, `tipIndex` (D16): `None` until the first move. V's own, ahead of the
    /// published `ChainView::dmn_tip` within a step.
    pub(crate) fn dmn_tip(&self) -> Option<BlockId> {
        *self.inner.dmn_tip.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `CDeterministicMNManager::UpdatedBlockTip(pindex)`: V's only write of the DMN tip.
    // Core: evo/deterministicmns.cpp:819-823
    pub(crate) fn move_dmn_tip(&self, to: BlockId) {
        *self.inner.dmn_tip.lock().unwrap_or_else(|p| p.into_inner()) = Some(to);
    }
}
