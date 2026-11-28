//! A masternode's ChainLock and InstantSend signers (LK1c, DEC-1887 item 2): their reads over V's
//! published view, and [`LockSigners`], the share of Core's `ActiveContext` that runs them.
//!
//! Deviations from Core v24.0.2:
//! 1. `txFirstSeenTime` exists only on a masternode (Core's miner reads it on every node; rdashd
//!    has no block assembler). Its cleanup runs on `cls-schdlr` every 5 s,
//!    not on the handler's tip signal, and only once the txindex has indexed the active tip.
//! 2. The IS signer's calls from NetInstantSend run on an `is-signer` FIFO thread, joined after
//!    `Stop`, before the IS store and the txindex close.
//! 3. A locally completed islock enters the pending queue with the BLS scheme V publishes then.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use dash_chain::{BlockId, IndexAccess as _, SporkView as _};
use dash_clock::Clock;
use dash_consensus::primitives::TxHash;
use dash_consensus::{Block, Tx, TxRef};
use dash_evo::locks::wire::RecoveredSig;
use dash_evo::locks::{ClsigHashSchemes, clsig_hash};
use dash_evo::spork::SporkId;
use dash_llmq::signing::api::{QuorumSigning, RecoveredSigListener, RecoveredSigResult};
use dash_p2p_core::{ClSig, Inventory};
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{
    BlockFiles, ChainViewHandle, LoadingFlags, ValidationInterface, ValidationSignals,
};
use dashd_indexes::{IndexStatus as _, MempoolLookup, RetrievedTx};
use dashd_locks::chainlock::Read;
use dashd_locks::chainlock::first_seen::{Synced, TxFirstSeen};
use dashd_locks::chainlock::reads::{Abort, ChainEntry, ChainReads};
use dashd_locks::chainlock::signer::{
    ClProcess, ClReads, ClScheduler, ClSigner, ClSignerParts, has_chainlock,
    has_conflicting_chainlock,
};
use dashd_locks::is::listener::{IsListenerSigner, IsSignerQueue};
use dashd_locks::is::manager::IsManager;
use dashd_locks::is::reads::StoreLockView;
use dashd_locks::is::seams::TxLookup;
use dashd_locks::is::signer::{IsLockSink, IsReads, IsSigner, IsSignerParts, IsTxSource};
use dashd_locks::is::{IsLockHash, LockedTx, core_height};
use dashd_locks::signing::{IsLockView, TxSource};
use dashd_masternode::sporks::SporkManager;
use dashd_masternode::sync::MasternodeSync;
use dashd_mempool::api::MempoolHandle;
use dashd_p2p::processing::seams::ChainLocks as _;

use super::SharedHandler;
use crate::abort::AbortSink;
use crate::host::trace_thread;
use crate::indexes::{TxIndexHandle, get_transaction};
use crate::instantsend::{IsParts, SignerSlot};
use crate::scheduler::Scheduler;

// Core: v24.0.2 src/chainlock/handler.cpp:63-71 Start
const FIRST_SEEN_CLEANUP_EVERY: Duration = Duration::from_secs(5);

/// V's published tree and ChainLock, the sporks and `mn_sync`, as both signers read them. No
/// read waits on V or the signals queue.
pub struct NodeLockReads {
    pub view: ChainViewHandle,
    pub loading: Arc<LoadingFlags>,
    pub sporks: Arc<SporkManager>,
    pub sync: Arc<MasternodeSync>,
    pub files: Option<Arc<dyn BlockFiles + Sync>>,
}

impl NodeLockReads {
    fn entry(&self, id: BlockId) -> Option<ChainEntry> {
        let e = self.view.entry(id)?;
        Some(ChainEntry {
            hash: e.hash,
            height: core_height(e.height),
            prev: e.prev.and_then(|p| self.view.entry(p)).map(|p| p.hash),
        })
    }

    /// `bestChainLockBlockIndex`, while ChainLocks are enabled.
    fn best(&self) -> Option<ChainEntry> {
        let floor = self.view.view().snap.cl_floor?;
        self.sporks.chainlocks_enabled().then(|| self.entry(floor.id)).flatten()
    }
}

impl ChainReads for NodeLockReads {
    fn tip(&self) -> Option<ChainEntry> {
        let snap = self.view.snapshot();
        snap.has_tip().then(|| self.entry(snap.tip.tip)).flatten()
    }

    fn lookup(&self, hash: &BlockHash) -> Option<ChainEntry> {
        self.entry(self.view.lookup(hash)?)
    }

    fn ancestor_hash(&self, hash: &BlockHash, height: i32) -> Option<BlockHash> {
        let ancestor = self.view.ancestor(self.view.lookup(hash)?, u32::try_from(height).ok()?)?;
        self.view.entry(ancestor).map(|e| e.hash)
    }

    // Core: v24.0.2 src/node/blockstorage.cpp:837-850 ReadBlockFromDisk
    fn read_block(&self, hash: &BlockHash) -> Option<Arc<Block>> {
        let pos = self.view.reader().have_data_pos(self.view.lookup(hash)?)?;
        self.files.as_ref()?.read_block(pos, hash).ok()
    }
}

impl ClReads for NodeLockReads {
    // Core: v24.0.2 src/chainlock/chainlock.cpp:33 IsEnabled
    fn enabled(&self) -> bool {
        self.sporks.chainlocks_enabled()
    }

    // Core: v24.0.2 src/chainlock/chainlock.cpp:35 IsSigningEnabled
    fn signing_enabled(&self) -> bool {
        self.sporks.value(SporkId::SPORK_19_CHAINLOCKS_ENABLED) == 0
    }

    // Core: v24.0.2 src/chainlock/chainlock.cpp:43-47 GetBestChainLockHeight
    fn best_height(&self) -> i32 {
        self.view.view().cl.best.as_ref().map_or(-1, |sig| sig.height)
    }

    fn has_chainlock(&self, height: i32, hash: &BlockHash) -> bool {
        has_chainlock(self.best().as_ref(), height, hash, self)
    }

    fn has_conflicting_chainlock(&self, height: i32, hash: &BlockHash) -> bool {
        has_conflicting_chainlock(self.best().as_ref(), height, hash, self)
    }
}

impl IsReads for NodeLockReads {
    fn blockchain_synced(&self) -> bool {
        self.sync.is_blockchain_synced()
    }

    fn mempool_signing_enabled(&self) -> bool {
        !self.loading.any() && self.sporks.value(SporkId::SPORK_2_INSTANTSEND_ENABLED) == 0
    }

    fn block_height(&self, hash: &BlockHash) -> Option<i32> {
        ChainReads::lookup(self, hash).map(|e| e.height)
    }

    fn has_chain_lock(&self, height: i32, hash: &BlockHash) -> bool {
        ClReads::has_chainlock(self, height, hash)
    }

    fn ancestor_hash(&self, hash: &BlockHash, height: i32) -> Option<BlockHash> {
        ChainReads::ancestor_hash(self, hash, height)
    }
}

/// Core's `GetTransaction(nullptr, &mempool, txid)`: the mempool, then the txindex. A txindex
/// read failure is a miss, so no read fails.
pub struct NodeTxSource {
    pub mempool: Option<Arc<dyn MempoolHandle>>,
    pub txindex: Option<TxIndexHandle>,
}

impl NodeTxSource {
    fn find(&self, txid: &TxHash) -> Option<RetrievedTx> {
        let pool = self.mempool.as_deref().map(|pool| {
            move |txid: &TxHash| pool.read(|r| r.get(txid)).map(|tx| Arc::new(tx.raw().clone()))
        });
        let pool = pool.as_ref().map(|f| f as &MempoolLookup<'_>);
        get_transaction(pool, self.txindex.as_ref(), txid)
    }

    /// Whether the txindex has indexed `tip`, by hash: always without a txindex.
    pub(crate) fn indexed(&self, tip: Option<&ChainEntry>) -> bool {
        self.txindex.as_ref().is_none_or(|index| {
            index.consumed_tip().zip(tip).is_some_and(|((_, hash), tip)| hash == tip.hash)
        })
    }
}

impl TxSource for NodeTxSource {
    fn get_transaction(&self, txid: &TxHash) -> dash_kv::Result<Option<Option<BlockHash>>> {
        Ok(self.find(txid).map(|(_, block)| block))
    }
}

impl IsTxSource for NodeTxSource {
    fn get_tx(&self, txid: &TxHash) -> dash_kv::Result<Option<(TxRef, Option<BlockHash>)>> {
        Ok(self.find(txid).map(|(tx, block)| (Arc::new(Tx::new((*tx).clone())), block)))
    }

    fn wait_synced(&self) {
        if let Some(index) = &self.txindex {
            index.block_until_synced();
        }
    }
}

/// The InstantSend processor's `GetTransaction`.
// Core: v24.0.0 src/instantsend/net_instantsend.cpp:369-371; src/node/transaction.cpp:126-156
impl TxLookup for NodeTxSource {
    fn get_transaction(&self, txid: &TxHash) -> Option<(TxRef, Option<BlockHash>)> {
        IsTxSource::get_tx(self, txid).ok().flatten()
    }
}

/// The scheduled `txFirstSeenTime` cleanup, skipped until the txindex has indexed the active tip.
pub(crate) fn cleanup_first_seen(
    first_seen: &TxFirstSeen,
    txs: &NodeTxSource,
    reads: &NodeLockReads,
) {
    if txs.indexed(ChainReads::tip(reads).as_ref()) {
        first_seen.cleanup(txs, reads);
    }
}

/// `ProcessNewChainLock(-1, clsig, qman, ::SerializeHash(clsig))` on the node's handler.
struct NodeClProcess {
    handler: SharedHandler,
    view: ChainViewHandle,
}

impl ClProcess for NodeClProcess {
    // Core: v24.0.2 src/chainlock/signing.cpp:286
    fn process_local(&self, clsig: ClSig) -> Option<Inventory> {
        let bls = self.view.view().bls;
        let hash = clsig_hash(&clsig, ClsigHashSchemes::both(bls))?;
        let read = Read {
            msg: clsig,
            read: bls,
        };
        self.handler.lock().process_new_chainlock(-1, read, &hash).inventory.into_iter().next()
    }
}

impl ClScheduler for Scheduler {
    fn schedule_every(&self, task: Box<dyn Fn() + Send + Sync>, every: Duration) {
        Scheduler::schedule_every(self, task, every);
    }

    fn schedule(&self, task: Box<dyn FnOnce() + Send>) {
        self.schedule_from_now(task, Duration::ZERO);
    }

    fn stop(&self) {
        Scheduler::stop(self);
    }
}

/// `TryEmplacePendingLock(hash, -1, islock)` on the IS manager.
struct NodeLockSink {
    manager: Arc<IsManager>,
    view: ChainViewHandle,
}

impl IsLockSink for NodeLockSink {
    // Core: v24.0.2 src/instantsend/signing.cpp:357
    fn try_emplace_pending(&self, hash: IsLockHash, lock: LockedTx) -> dash_kv::Result<()> {
        let legacy = self.view.view().bls.is_legacy();
        self.manager.try_emplace_pending_lock(hash, -1, Arc::new(lock), legacy)
    }
}

type Job = Box<dyn FnOnce() + Send>;

/// The IS signer's FIFO thread. `post` never blocks; after [`SignerQueue::stop`] it drops the job,
/// and no job runs once `stop` returns.
pub struct SignerQueue {
    running: Mutex<Option<(Sender<Job>, JoinHandle<()>)>>,
    stopped: Arc<AtomicBool>,
}

impl SignerQueue {
    pub fn start() -> io::Result<Self> {
        let (jobs, queued) = mpsc::channel::<Job>();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let thread = trace_thread("is-signer", move || {
            for job in queued {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                job();
            }
        })?;
        Ok(Self {
            running: Mutex::new(Some((jobs, thread))),
            stopped,
        })
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let running = self.running.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some((jobs, thread)) = running {
            drop(jobs);
            let _ = thread.join();
        }
    }
}

impl IsSignerQueue for SignerQueue {
    fn post(&self, job: Job) {
        if let Some((jobs, _)) = &*self.running.lock().unwrap_or_else(PoisonError::into_inner) {
            let _ = jobs.send(job);
        }
    }
}

/// What [`LockSigners`] is built from: step 7c's services on a masternode.
pub struct LockSignerParts<'a> {
    pub params: &'a ChainSetup,
    pub view: ChainViewHandle,
    pub loading: Arc<LoadingFlags>,
    pub signals: ValidationSignals,
    pub sporks: Arc<SporkManager>,
    pub sync: Arc<MasternodeSync>,
    pub is: &'a IsParts,
    pub signing: Arc<dyn QuorumSigning>,
    pub chainlocks: SharedHandler,
    pub mempool: Option<Arc<dyn MempoolHandle>>,
    pub txindex: Option<TxIndexHandle>,
    pub files: Option<Arc<dyn BlockFiles + Sync>>,
    pub clock: Arc<dyn Clock>,
    pub abort: Arc<dyn AbortSink>,
}

/// `ActiveContext`'s `cl_signer` and `is_signer`, with the IS signer's queue.
pub struct LockSigners {
    cl: Arc<ClSigner>,
    is: Arc<IsSigner>,
    queue: Arc<SignerQueue>,
    first_seen: Arc<TxFirstSeen>,
    txs: Arc<NodeTxSource>,
    reads: Arc<NodeLockReads>,
    scheduler: Scheduler,
    signals: ValidationSignals,
    hooks: [Arc<dyn ValidationInterface>; 2],
    subscribed: Mutex<bool>,
    slot: SignerSlot,
}

impl LockSigners {
    /// Both signers, with the ChainLock signer's `cls-schdlr` thread and the IS signer's queue
    /// thread running and nothing scheduled.
    // Core: v24.0.2 src/active/context.cpp:47-50; src/chainlock/signing.cpp:23-39
    pub fn new(p: LockSignerParts<'_>) -> io::Result<Self> {
        let queue = Arc::new(SignerQueue::start()?);
        let scheduler = Scheduler::new();
        if let Err(e) = scheduler.start_as("cls-schdlr") {
            queue.stop();
            return Err(e);
        }
        let reads = Arc::new(NodeLockReads {
            view: p.view.clone(),
            loading: p.loading,
            sporks: p.sporks,
            sync: Arc::clone(&p.sync),
            files: p.files,
        });
        let txs = Arc::new(NodeTxSource {
            mempool: p.mempool,
            txindex: p.txindex,
        });
        let node = p.abort;
        let abort: Abort = Arc::new(move |msg: &str| node.abort_node(msg, None));
        let sync = p.sync;
        let synced: Synced = Arc::new(move || sync.is_blockchain_synced());
        let first_seen = Arc::new(TxFirstSeen::new(
            Arc::clone(&p.clock),
            Arc::clone(&synced),
            Arc::clone(&abort),
        ));
        let view: Arc<dyn IsLockView> = Arc::new(StoreLockView {
            store: Arc::clone(&p.is.store),
            gate: Arc::clone(&p.is.gate),
        });
        let cl = ClSigner::new(
            p.params,
            ClSignerParts {
                chain: Arc::clone(&reads) as _,
                cl: Arc::clone(&reads) as _,
                is: Arc::clone(&view),
                signing: Arc::clone(&p.signing),
                process: Arc::new(NodeClProcess {
                    handler: p.chainlocks,
                    view: p.view.clone(),
                }),
                first_seen: Arc::clone(&first_seen),
                synced,
                clock: p.clock,
                scheduler: Arc::new(scheduler.clone()),
                abort: Arc::clone(&abort),
            },
        );
        let is = Arc::new(IsSigner::new(
            p.params,
            IsSignerParts {
                view,
                store: Arc::clone(&p.is.store) as _,
                signing: Arc::clone(&p.signing),
                txs: Arc::clone(&txs) as _,
                reads: Arc::clone(&reads) as _,
                sink: Arc::new(NodeLockSink {
                    manager: Arc::clone(&p.is.manager),
                    view: p.view,
                }),
                tip: p.is.tip.clone(),
                abort,
            },
        ));
        Ok(Self {
            hooks: [Arc::clone(&first_seen) as _, Arc::clone(&cl) as _],
            cl,
            is,
            queue,
            first_seen,
            txs,
            reads,
            scheduler,
            signals: p.signals,
            subscribed: Mutex::default(),
            slot: p.is.signer.clone(),
        })
    }

    /// NetInstantSend's `m_signer`: the IS signer behind its queue.
    pub fn listener_signer(&self) -> IsListenerSigner {
        IsListenerSigner {
            signer: Arc::clone(&self.is),
            queue: Arc::clone(&self.queue) as _,
        }
    }

    /// The recovered-signature listeners `ActiveContext::Start` adds before the sig-shares
    /// manager's: the ChainLock signer, then the IS signer. Each holds its signer weakly, so the
    /// signing runtime never outlives [`stop`](Self::stop)'s release of the signers' state.
    // Core: v24.0.2 src/active/context.cpp:58-59
    pub fn recovered_sig_listeners(&self) -> [Arc<dyn RecoveredSigListener>; 2] {
        [
            Arc::new(WeakListener(Arc::downgrade(&self.cl))),
            Arc::new(WeakListener(Arc::downgrade(&self.is))),
        ]
    }

    /// `cl_signer->Start()`, with `txFirstSeenTime`'s cleanup.
    // Core: v24.0.2 src/active/context.cpp:57; src/chainlock/handler.cpp:63-71, :339-348
    pub fn start(&self) {
        self.cl.start();
        let (first_seen, txs, reads) =
            (Arc::clone(&self.first_seen), Arc::clone(&self.txs), Arc::clone(&self.reads));
        self.scheduler.schedule_every(
            move || cleanup_first_seen(&first_seen, &txs, &reads),
            FIRST_SEEN_CLEANUP_EVERY,
        );
    }

    /// `RegisterValidationInterface(cl_signer)`, after the recovered-signature listeners.
    // Core: v24.0.2 src/active/context.cpp:62
    pub fn subscribe(&self) {
        let mut subscribed = self.subscribed.lock().unwrap_or_else(PoisonError::into_inner);
        if !std::mem::replace(&mut *subscribed, true) {
            for hook in &self.hooks {
                self.signals.register(Arc::clone(hook));
            }
        }
    }

    /// `UnregisterValidationInterface(cl_signer)`, before the recovered-signature listeners go.
    // Core: v24.0.2 src/active/context.cpp:67
    pub fn unsubscribe(&self) {
        let mut subscribed = self.subscribed.lock().unwrap_or_else(PoisonError::into_inner);
        if std::mem::take(&mut *subscribed) {
            for hook in self.hooks.iter().rev() {
                self.signals.unregister(hook);
            }
        }
    }

    /// `cl_signer->Stop()` once the listeners have gone, then the IS queue's join, and
    /// NetInstantSend's `m_signer` back to null. Later calls do nothing.
    // Core: v24.0.2 src/active/context.cpp:72
    pub fn stop(&self) {
        self.unsubscribe();
        self.cl.stop();
        self.queue.stop();
        self.slot.clear();
    }
}

#[cfg(test)]
impl LockSigners {
    /// Whether the hooks hear V, `cls-schdlr` and the IS queue.
    pub(crate) fn probe(&self) -> (bool, Scheduler, Arc<SignerQueue>) {
        let subscribed = *self.subscribed.lock().unwrap_or_else(PoisonError::into_inner);
        (subscribed, self.scheduler.clone(), Arc::clone(&self.queue))
    }
}

#[cfg(test)]
impl SignerQueue {
    pub(crate) fn joined(&self) -> bool {
        self.running.lock().unwrap_or_else(PoisonError::into_inner).is_none()
    }
}

struct WeakListener<T>(Weak<T>);

impl<T: RecoveredSigListener> RecoveredSigListener for WeakListener<T> {
    fn handle_new_recovered_sig(&self, sig: &RecoveredSig) -> RecoveredSigResult {
        self.0.upgrade().map_or(RecoveredSigResult::Nothing, |l| l.handle_new_recovered_sig(sig))
    }
}

impl Drop for LockSigners {
    // Core: v24.0.2 src/chainlock/signing.cpp:41 ~ChainLockSigner
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[path = "signers_tests.rs"]
mod tests;
