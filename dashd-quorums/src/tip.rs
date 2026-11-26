//! `NetQuorum`'s tip handling (v24.0.0 `llmq/net_quorum.cpp:377-404`): [`NetQuorumTip`], the only
//! subscriber for NetQuorum's tip events (PLAN-QM1b §3a); [`RecoverySteps`], what it runs after
//! its guards; and [`TipWorker`], its listener on V's signals.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;

use dash_consensus::BlockId;
use dash_llmq::qdata::SyncMode;
use dash_llmq::qdata::recovery::{Claim, plan_tip, process_block_log};
use dash_llmq::qdata::source::{EncryptedContributions, QuorumDataSource};
pub use dash_llmq::qdata::{Role, TipRef};
use dash_params::{LlmqParams, LlmqType};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{ValidationInterface, no_wait_marked};
use tracing::debug;

use crate::manager::QuorumManager;
use crate::net_quorum::{CaughtException, NetQuorum, StoredQuorum};
use crate::traits::{QuorumKeyStore, WarmExecutor};

/// CheckQuorumConnections (llmq/net_quorum.cpp v24.0.0 :438-497) for one type at `tip`; applies its
/// RegistryOps in order before returning. Never called on V's thread nor on a no-wait-marked
/// thread (reply.rs:4-12). May make bounded reads of V (Core takes cs_main briefly inside it);
/// holds no NetCore or leaf lock across one.
pub trait QuorumConnectionsCheck: Send + Sync {
    fn check(&self, llmq_type: LlmqType, params: &LlmqParams, tip: &TipRef, role: &Role);
}

/// The check before P7-W3 installs its own: it does nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoQuorumConnectionsCheck;

impl QuorumConnectionsCheck for NoQuorumConnectionsCheck {
    fn check(&self, _: LlmqType, _: &LlmqParams, _: &TipRef, _: &Role) {}
}

/// A chain tip as the handler reads it: V's entry and its height and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tip {
    pub id: BlockId,
    pub at: TipRef,
}

/// What [`NetQuorumTip::updated_block_tip`] runs after the per-type connection checks.
pub trait TipSteps: Send + Sync {
    /// `m_qman.CleanupExpiredDataRequests()`.
    fn cleanup_expired_data_requests(&self);
    /// `TriggerQuorumDataRecoveryThreads(tip)`; `role` is never `None`. An error is Core's
    /// `ScanQuorums` throw, which leaves the tip handler.
    fn trigger_recovery(&self, role: Role, tip: &Tip) -> Result<(), TipFault>;
    /// `StartCleanupOldQuorumDataThread(tip)`.
    fn start_cleanup_old_quorum_data(&self, tip: &Tip);
}

/// What a tip step threw: the tip handler stops at it, as Core's throw leaves `UpdatedBlockTip`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipFault(pub String);

/// Why a recovery scan returned no quorums.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanError {
    /// V is gone (stopped, or not yet bound): the scan is skipped quietly.
    VGone,
    /// A store read failed: Core's `ScanQuorums` throws.
    Read(String),
}

/// The node's role as each tip event reads it: on a masternode its proTxHash is known only after
/// the active masternode's `Init`.
pub type RoleSource = Arc<dyn Fn() -> Role + Send + Sync>;

/// `NetQuorum`'s `CValidationInterface` half.
pub struct NetQuorumTip<T> {
    role: RoleSource,
    synced: Arc<dyn Fn() -> bool + Send + Sync>,
    llmqs: Vec<(LlmqType, LlmqParams)>,
    check: Arc<dyn QuorumConnectionsCheck>,
    steps: T,
}

impl<T: TipSteps> NetQuorumTip<T> {
    /// The handler over `llmqs` in `Params().GetConsensus().llmqs` order, with `synced` as
    /// `m_mn_sync.IsBlockchainSynced()` and the no-op connection check.
    pub fn new(
        role: RoleSource,
        synced: Arc<dyn Fn() -> bool + Send + Sync>,
        llmqs: Vec<(LlmqType, LlmqParams)>,
        steps: T,
    ) -> Self {
        Self {
            role,
            synced,
            llmqs,
            check: Arc::new(NoQuorumConnectionsCheck),
            steps,
        }
    }

    /// Installs P7-W3's `CheckQuorumConnections` in place of the no-op.
    pub fn with_connections_check(mut self, check: Arc<dyn QuorumConnectionsCheck>) -> Self {
        self.check = check;
        self
    }

    fn check_connections(&self, tip: &Tip, role: &Role) {
        for (llmq_type, params) in &self.llmqs {
            self.check.check(*llmq_type, params, &tip.at, role);
        }
    }

    /// The tip handler; a fault ends it before the old data cleanup.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:390-404 NetQuorum::UpdatedBlockTip
    pub fn updated_block_tip(
        &self,
        new: Option<&Tip>,
        fork: Option<BlockId>,
        ibd: bool,
    ) -> Result<(), TipFault> {
        let role = (self.role)();
        if role == Role::None {
            return Ok(());
        }
        let Some(new) = new else {
            return Ok(());
        };
        if ibd || Some(new.id) == fork {
            return Ok(());
        }
        if !(self.synced)() {
            return Ok(());
        }
        self.check_connections(new, &role);
        self.steps.cleanup_expired_data_requests();
        self.steps.trigger_recovery(role, new)?;
        self.steps.start_cleanup_old_quorum_data(new);
        Ok(())
    }

    /// The startup handler; a fault in the guarded path ends it before the per-type check.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:377-386 NetQuorum::InitializeCurrentBlockTip
    pub fn initialize_current_block_tip(
        &self,
        tip: Option<&Tip>,
        ibd: bool,
    ) -> Result<(), TipFault> {
        let role = (self.role)();
        if role == Role::None {
            return Ok(());
        }
        self.updated_block_tip(tip, None, ibd)?;
        if let Some(tip) = tip {
            self.check_connections(tip, &role);
        }
        Ok(())
    }
}

/// `ScanQuorums(type, tip, count)` as the recovery trigger reads it: each quorum with its hash,
/// newest first.
pub trait RecoveryScan: Send + Sync {
    type Q;
    fn scan(
        &self,
        llmq_type: LlmqType,
        tip: BlockId,
        count: usize,
    ) -> Result<Vec<(BlockHash, Self::Q)>, ScanError>;
}

/// QM1a-3's old quorum data cleanup, which `NetQuorumTip` calls last.
pub trait OldDataCleanup: Send + Sync {
    fn start_cleanup_old_quorum_data(&self, tip: BlockId, height: u32);
}

impl<S, C, M, W> OldDataCleanup for QuorumManager<S, C, M, W>
where
    S: QuorumKeyStore + Send + Sync + 'static,
    S::Error: Send + Sync + 'static,
    C: Send + Sync,
    M: Send + Sync,
    W: WarmExecutor + Send + Sync + 'static,
{
    fn start_cleanup_old_quorum_data(&self, tip: BlockId, height: u32) {
        QuorumManager::start_cleanup_old_quorum_data(self, tip, height);
    }
}

impl<T: OldDataCleanup + ?Sized> OldDataCleanup for Arc<T> {
    fn start_cleanup_old_quorum_data(&self, tip: BlockId, height: u32) {
        (**self).start_cleanup_old_quorum_data(tip, height);
    }
}

/// The node's [`TipSteps`]: the handler's data request cleanup and recovery jobs, and the
/// manager's old data cleanup.
pub struct RecoverySteps<S, E, K, W, Sc, Cl> {
    pub net: NetQuorum<S, E, K, W>,
    pub scan: Sc,
    pub cleanup: Cl,
    /// `m_quorums_recovery` (`-llmq-data-recovery`).
    pub enabled: bool,
    /// `m_sync_map` (`-llmq-qvvec-sync`).
    pub sync: Vec<(LlmqType, SyncMode)>,
    /// Each type with its `keepOldConnections`, in `Params().GetConsensus().llmqs` order.
    pub llmqs: Vec<(LlmqType, usize)>,
}

impl<S, E, K, W, Sc, Cl> TipSteps for RecoverySteps<S, E, K, W, Sc, Cl>
where
    S: QuorumDataSource + Send + Sync + 'static,
    S::Q: StoredQuorum,
    S::Error: CaughtException,
    E: EncryptedContributions + Send + Sync + 'static,
    K: QuorumKeyStore + Send + Sync + 'static,
    W: WarmExecutor + Send + Sync + 'static,
    Sc: RecoveryScan<Q = S::Q>,
    Cl: OldDataCleanup,
{
    fn cleanup_expired_data_requests(&self) {
        self.net.cleanup_expired_data_requests();
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:499-535,640-686 NetQuorum::TriggerQuorumDataRecoveryThreads
    fn trigger_recovery(&self, role: Role, tip: &Tip) -> Result<(), TipFault> {
        if !self.enabled {
            return Ok(());
        }
        if let Some(line) = process_block_log(role, self.enabled, tip.at.hash) {
            debug!(target: "llmq", "{line}");
        }
        for &(llmq_type, keep) in &self.llmqs {
            let scanned = match self.scan.scan(llmq_type, tip.id, keep) {
                Ok(scanned) => scanned,
                Err(ScanError::VGone) => continue,
                // Core: v24.0.0 src/llmq/net_quorum.cpp:509 ScanQuorums throws
                Err(ScanError::Read(e)) => {
                    return Err(TipFault(format!(
                        "NetQuorum::TriggerQuorumDataRecoveryThreads -- ScanQuorums: {e}"
                    )));
                }
            };
            let scans = [(llmq_type, scanned)];
            for plan in plan_tip(role, self.enabled, &self.sync, &scans, tip.at) {
                if let Some(line) = plan.no_data_needed_log(tip.at.height) {
                    debug!(target: "llmq", "{line}");
                    continue;
                }
                let hash = match plan {
                    dash_llmq::qdata::recovery::Plan::SkShare {
                        q,
                        ..
                    }
                    | dash_llmq::qdata::recovery::Plan::VvecSync {
                        q,
                    } => q.quorum_hash,
                    dash_llmq::qdata::recovery::Plan::NoDataNeeded {
                        ..
                    } => continue,
                };
                let Some((_, quorum)) = scans[0].1.iter().find(|(h, _)| *h == hash) else {
                    continue;
                };
                match plan.claim(quorum) {
                    Claim::Skip => {}
                    Claim::AlreadyRunning(line) => debug!(target: "llmq", "{line}"),
                    Claim::Claimed(job) => self.net.enqueue_recovery(job, quorum.clone()),
                }
            }
        }
        Ok(())
    }

    fn start_cleanup_old_quorum_data(&self, tip: &Tip) {
        if let Ok(height) = u32::try_from(tip.at.height) {
            self.cleanup.start_cleanup_old_quorum_data(tip.id, height);
        }
    }
}

enum Event {
    Init(Option<BlockId>, bool),
    Updated(BlockId, Option<BlockId>, bool),
}

/// [`NetQuorumTip`] on V's signals. The signals thread never waits, and the recovery trigger
/// reads V, so each event is posted, in order, to the worker `d-q-tip`, which runs the handler.
///
/// On a masternode it ignores startup's `InitializeCurrentBlockTip` until `armed` is set, after
/// the active masternode's `Init`: Core sends that first broadcast only to
/// `CDSNotificationInterface` there, and the full one after `Init` (init.cpp:2457-2466, :2538-2547).
pub struct TipWorker {
    tx: Mutex<Option<Sender<Event>>>,
    armed: Arc<AtomicBool>,
    progress: Arc<Progress>,
}

/// How far `d-q-tip` has got through the events posted to it.
#[derive(Default)]
struct Progress {
    state: Mutex<Counts>,
    moved: Condvar,
}

#[derive(Default)]
struct Counts {
    posted: u64,
    finished: u64,
    ended: bool,
}

impl Progress {
    fn update(&self, f: impl FnOnce(&mut Counts)) {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner));
        self.moved.notify_all();
    }
}

/// Counts one event finished when dropped, however its run ends.
struct Finished<'a>(&'a Progress);

impl Drop for Finished<'_> {
    fn drop(&mut self) {
        self.0.update(|c| c.finished += 1);
    }
}

/// Marks the worker ended when its thread exits, so no barrier waits on it any more.
struct Ended(Arc<Progress>);

impl Drop for Ended {
    fn drop(&mut self) {
        self.0.update(|c| c.ended = true);
    }
}

/// Core runs `NetQuorum::UpdatedBlockTip` and the DKG's `CDKGSessionManager::UpdatedBlockTip`
/// (through `ActiveContext` or `ObserverContext`) on one serial validation-interface queue, so
/// the DKG learns of a tip only after `CheckQuorumConnections` has run for every tip before it.
/// The node runs them on separate workers; the DKG's waits on this barrier before it takes a tip,
/// so a stale check can never remove the connections a newer DKG session's
/// `EnsureQuorumConnections` set up.
///
/// The wait's target is fixed when the DKG's worker reaches tip h, so it covers every check
/// posted by then: h's own, since the node registers `d-q-tip`'s listener before the DKG's, the
/// reverse of Core, and any later tip's already posted. Hence the node runs `NetQuorum(h)` before
/// `qdkgsman(h)`, where Core runs `qdkgsman(h)` first. For the connections that is an order
/// Core's queue gives too: Core's DKG thread runs `EnsureQuorumConnections` after its phase wait,
/// so it may land after `NetQuorum(h)` and later passes, and every check in h's DKG cycle keeps
/// h's current DKG block. Nothing here orders the active masternode's own update against
/// `NetQuorum(h)`.
// Core: v24.0.0 src/init.cpp:2167, :2171, :2210 (registration order);
// src/llmq/net_quorum.cpp:390-405, :408-436; src/active/context.cpp:93-113;
// src/llmq/observer.cpp:27-33; src/llmq/net_dkg.cpp:850, :880-882
#[derive(Clone)]
pub struct TipBarrier(Arc<Progress>);

impl TipBarrier {
    /// Waits until `d-q-tip` has finished every event posted to it before the call, or has
    /// ended. Never called on a no-wait thread.
    pub fn wait_for_posted(&self) {
        debug_assert!(!no_wait_marked(), "a tip barrier waits on d-q-tip");
        let progress = &self.0;
        let mut counts = progress.state.lock().unwrap_or_else(PoisonError::into_inner);
        let target = counts.posted;
        while counts.finished < target && !counts.ended {
            counts = progress.moved.wait(counts).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl TipWorker {
    /// Spawns the worker over `tip`. `resolve` reads an entry's height and hash; `on_panic` gets
    /// the message of the first panic or [`TipFault`], after which the worker drops every event:
    /// Core's throw escapes the thread that runs the signals.
    pub fn spawn<T: TipSteps + 'static>(
        tip: NetQuorumTip<T>,
        resolve: Box<dyn Fn(BlockId) -> Option<Tip> + Send>,
        armed: Arc<AtomicBool>,
        on_panic: Box<dyn Fn(String) + Send>,
    ) -> std::io::Result<(Arc<TipWorker>, JoinHandle<()>)> {
        let (tx, rx) = channel::<Event>();
        let progress = Arc::new(Progress::default());
        let ended = Ended(Arc::clone(&progress));
        let handle = std::thread::Builder::new().name("d-q-tip".into()).spawn(move || {
            debug_assert!(!no_wait_marked(), "d-q-tip carries no no-wait marker");
            let ended = ended;
            let mut panicked = false;
            for event in rx {
                let _finished = Finished(&ended.0);
                if panicked {
                    continue;
                }
                let run = catch_unwind(AssertUnwindSafe(|| match event {
                    Event::Init(id, ibd) => {
                        let at = id.and_then(&resolve);
                        tip.initialize_current_block_tip(at.as_ref(), ibd)
                    }
                    Event::Updated(new, fork, ibd) => {
                        let at = resolve(new);
                        tip.updated_block_tip(at.as_ref(), fork, ibd)
                    }
                }));
                let what = match run {
                    Ok(Ok(())) => continue,
                    Ok(Err(TipFault(what))) => what,
                    Err(p) => p
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_default(),
                };
                panicked = true;
                on_panic(what);
            }
        })?;
        let worker = Arc::new(TipWorker {
            tx: Mutex::new(Some(tx)),
            armed,
            progress,
        });
        Ok((worker, handle))
    }

    /// The barrier the DKG's tip worker waits on.
    pub fn barrier(&self) -> TipBarrier {
        TipBarrier(Arc::clone(&self.progress))
    }

    fn post(&self, event: Event) {
        if let Some(tx) = &*self.tx.lock().unwrap_or_else(PoisonError::into_inner) {
            self.progress.update(|c| c.posted += 1);
            if tx.send(event).is_err() {
                self.progress.update(|c| c.ended = true);
            }
        }
    }

    /// Drops the sender: the worker ends after the events already posted.
    pub fn close(&self) {
        self.tx.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}

impl ValidationInterface for TipWorker {
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        self.post(Event::Updated(new, fork, ibd));
    }

    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        if self.armed.load(Ordering::SeqCst) {
            self.post(Event::Init(tip, ibd));
        }
    }
}

#[cfg(test)]
mod tests;
