//! Core's `NetQuorum` handler (v24.0.0 `llmq/net_quorum.cpp`): the `qgetdata` and `qdata`
//! gates, the runtime around dash-llmq's pure `qdata` serve, intake and recovery job, and the
//! handler's lifecycle. The recovery worker pool is injected ([`RecoveryPool`]); QM1b-7 creates
//! it, plans recovery at each tip and registers the handler in the node.

use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dash_evo::types::ProTxHash;
use dash_llmq::dkg::wire::{QData, QuorumDataRequest};
use dash_llmq::qdata::intake::{IntakeCx, intake};
use dash_llmq::qdata::recovery::{ClaimedJob, Next};
use dash_llmq::qdata::serve::{ServeCx, serve};
use dash_llmq::qdata::source::{
    EncryptedContributions, OperatorDecrypt, QuorumData, QuorumDataSource, ValidMnsAt,
};
use dash_llmq::qdata::tracker::{Request, Tracker, TrackerAccess};
use dash_llmq::qdata::{Act, Effect, PeerId, Role};
use dash_llmq::quorum::{QuorumDbKey, StoredContributions};
use dash_p2p::policy::{self, ReadError, Throw};
use dash_p2p::{CommandString, NetCore, Why};
use dash_params::LlmqType;
use dash_primitives::BlockHash;
use dashd_p2p::processing::logic::out_msg;
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::{HandlerCx, NetHandler};
use tracing::{debug, info};

use crate::traits::{QuorumKeyStore, WarmExecutor};

/// How often the expired data requests are dropped.
// Core: v24.0.0 src/llmq/net_quorum.cpp:72-75 NetQuorum::Schedule
pub const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

const COMMANDS: [CommandString; 2] = [CommandString::QGETDATA, CommandString::QDATA];

/// `typeid(dbwrapper_error).name()`.
const DBWRAPPER_ERROR: &str = "15dbwrapper_error";

/// `HandleError`'s second line.
// Core: v24.0.0 src/dbwrapper.cpp:267
const DEBUG_LEVELDB: &str = "You can use -debug=leveldb to get more complete diagnostic messages";

const QDATA_ERROR_LINE: &str = "NetQuorum::ProcessMessage -- qdata: Error ";

/// A built quorum as the runtime warms and persists it (Core's `CQuorum`). Clones share one
/// quorum, as Core's `CQuorumPtr`.
pub trait StoredQuorum: QuorumData + Clone + Send + Sync + 'static {
    /// `MakeQuorumKey`.
    fn db_key(&self) -> QuorumDbKey;
    /// What `WriteContributions` writes: the verification vector and the valid secret key share,
    /// read together.
    fn contributions(&self) -> StoredContributions;
    /// `QueueQuorumForWarming`'s job.
    fn warm_job(&self) -> Box<dyn FnOnce() + Send>;
}

/// An error Core throws out of the handler: `what()` is its text.
pub trait CaughtException: fmt::Display {
    /// `typeid(e).name()`.
    fn type_name(&self) -> &'static str;
}

/// Core's `CConnman` as the recovery jobs and `quorum getdata` reach it.
pub trait QuorumPeers: Send + Sync {
    /// `ForEachNode`: the fully connected peers in connection order, with each one's verified
    /// proRegTx hash (null when unverified).
    fn verified_peers(&self) -> Vec<(PeerId, [u8; 32])>;
    /// Pushes `qgetdata` to `peer`; false when it is gone.
    fn push_qgetdata(&self, peer: PeerId, req: &QuorumDataRequest) -> bool;
    /// `pNode->fDisconnect = true`.
    fn disconnect(&self, peer: PeerId);
    /// `AddPendingMasternode`.
    fn add_pending_masternode(&self, pro_tx: ProTxHash);
}

impl QuorumPeers for NetCore {
    // Core: v24.0.0 src/net.h CConnman::ForEachNode (NodeFullyConnected)
    fn verified_peers(&self) -> Vec<(PeerId, [u8; 32])> {
        self.peers()
            .iter()
            .filter(|p| p.successfully_connected() && !p.disconnect_requested())
            .map(|p| (p.id(), p.verified_proreg_hash()))
            .collect()
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:371-372 NetQuorum::RequestQuorumData
    fn push_qgetdata(&self, peer: PeerId, req: &QuorumDataRequest) -> bool {
        let Some(handle) = self.peer(peer) else {
            return false;
        };
        self.push(&handle, out_msg(CommandString::QGETDATA, req));
        true
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:621,626 NetQuorum::DataRecoveryThread
    fn disconnect(&self, peer: PeerId) {
        NetCore::disconnect(self, peer, Why::Processor("quorum data recovery"));
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:604 NetQuorum::DataRecoveryThread
    fn add_pending_masternode(&self, pro_tx: ProTxHash) {
        NetCore::add_pending_masternode(self, *pro_tx.as_bytes());
    }
}

/// What a recovery job reads from the node between polls, and how it sleeps.
pub trait RecoveryEnv: Send + Sync {
    /// `GetTime()` seconds.
    fn now(&self) -> i64;
    /// `m_mn_sync.IsBlockchainSynced()`.
    fn blockchain_synced(&self) -> bool;
    /// `quorumThreadInterrupt.sleep_for(d)`: returns early once `interrupt` is set.
    fn sleep(&self, d: Duration, interrupt: &Interrupt) {
        interrupt.sleep_for(d);
    }
}

/// Core's `workerPool` as `NetQuorum` drives it. QM1b-7's `RecoveryPool` implements it.
pub trait RecoveryPool: Send + Sync {
    /// `workerPool.resize(worker_count)`.
    fn start(&self);
    /// `workerPool.push(job)`.
    fn push(&self, job: Box<dyn FnOnce() + Send>);
    /// `workerPool.clear_queue()`: drops every job not started.
    fn clear_queue(&self);
    /// `workerPool.stop(true)`: waits for the running jobs.
    fn stop(&self);
}

/// Core's `quorumThreadInterrupt`: a flag whose sleeps wake when it is set.
#[derive(Debug, Default)]
pub struct Interrupt {
    set: Mutex<bool>,
    wake: Condvar,
}

impl Interrupt {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.set.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the flag and wakes every sleeper.
    pub fn set(&self) {
        *self.lock() = true;
        self.wake.notify_all();
    }

    pub fn is_set(&self) -> bool {
        *self.lock()
    }

    /// Sleeps for `d` or until the flag is set; returns whether it is set.
    pub fn sleep_for(&self, d: Duration) -> bool {
        let guard = self.lock();
        let (guard, _) = self
            .wake
            .wait_timeout_while(guard, d, |set| !*set)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

/// What a [`NetQuorum`] is built from.
pub struct NetQuorumParts<S, E, K, W> {
    /// `m_role`: the gates and `Start` read only its kind.
    pub role: Role,
    /// `m_qman`'s quorums.
    pub source: S,
    /// The dkgdb's encrypted contributions.
    pub contributions: E,
    /// `m_nodeman`: the operator key; None on a node that runs no masternode.
    pub operator: Option<Arc<dyn OperatorDecrypt + Send + Sync>>,
    /// `m_dmnman.GetListForBlock`'s enabled masternodes, for a recovery job's start offset.
    pub valid_mns: Arc<dyn ValidMnsAt + Send + Sync>,
    /// `m_qman`'s quorum database.
    pub store: K,
    /// `m_qman`'s warm-up executor.
    pub warm: W,
    pub peers: Arc<dyn QuorumPeers>,
    pub env: Arc<dyn RecoveryEnv>,
    pub pool: Arc<dyn RecoveryPool>,
}

struct Inner<S, E, K, W> {
    parts: NetQuorumParts<S, E, K, W>,
    /// `mapQuorumDataRequests` under `cs_data_requests`.
    tracker: Mutex<Tracker>,
    interrupt: Interrupt,
}

/// Core's `NetQuorum`. Clones share one handler: the registry owns one, and QM1b-7 keeps another
/// for `Start`, `Schedule`, recovery jobs and `quorum getdata`.
pub struct NetQuorum<S, E, K, W> {
    inner: Arc<Inner<S, E, K, W>>,
}

impl<S, E, K, W> Clone for NetQuorum<S, E, K, W> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// The source as intake reads it, keeping the cached quorum it hands out: Core's `pQuorum`,
/// which the warm-up and the write use after intake.
struct KeepCached<'a, S: QuorumDataSource> {
    source: &'a S,
    cached: RefCell<Option<S::Q>>,
}

impl<S: QuorumDataSource> QuorumDataSource for KeepCached<'_, S>
where
    S::Q: Clone,
{
    type Q = S::Q;
    type Error = S::Error;

    fn llmq_registered(&self, t: LlmqType) -> bool {
        self.source.llmq_registered(t)
    }

    fn active_block(&self, h: &BlockHash) -> bool {
        self.source.active_block(h)
    }

    fn quorum(&self, t: LlmqType, h: &BlockHash) -> Result<Option<S::Q>, S::Error> {
        self.source.quorum(t, h)
    }

    fn cached_quorum(&self, t: LlmqType, h: &BlockHash) -> Option<S::Q> {
        let q = self.source.cached_quorum(t, h);
        self.cached.replace(q.clone());
        q
    }
}

/// `ValidMnsAt` behind a trait object, for the generic `ClaimedJob::start`.
struct DynValidMns<'a>(&'a (dyn ValidMnsAt + Send + Sync));

impl ValidMnsAt for DynValidMns<'_> {
    fn valid_pro_tx_hashes(&self, block: &BlockHash) -> Option<Vec<ProTxHash>> {
        self.0.valid_pro_tx_hashes(block)
    }
}

/// A claimed job in the pool's queue. Dropped before it starts, it releases the quorum's claim.
struct Queued<Q: QuorumData> {
    quorum: Q,
    job: Option<ClaimedJob>,
}

impl<Q: QuorumData> Drop for Queued<Q> {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.release(&self.quorum);
        }
    }
}

/// Core's `LogEscapeMessage` of one byte.
// Core: v24.0.0 src/logging.cpp:420-431 BCLog::LogEscapeMessage
fn push_escaped(out: &mut Vec<u8>, b: u8) {
    if (b >= 32 || b == b'\n') && b != 0x7f {
        out.push(b);
    } else {
        out.extend_from_slice(format!("\\x{b:02x}").as_bytes());
    }
}

/// The logged bytes of intake's error-code line `line` for the raw error byte `error`:
/// tinyformat's `%d` of the `uint8_t` enum writes the byte itself, which the logger escapes
/// when it is a control byte; 0xFF stays one raw byte. None for any other line.
// Core: v24.0.0 src/llmq/net_quorum.cpp:231-233 NetQuorum::ProcessMessage
pub fn qdata_error_line(line: &str, error: u8) -> Option<Vec<u8>> {
    let rest = line.strip_prefix(QDATA_ERROR_LINE)?;
    let mut chars = rest.chars();
    chars.next()?;
    let mut out = QDATA_ERROR_LINE.as_bytes().to_vec();
    push_escaped(&mut out, error);
    out.extend_from_slice(chars.as_str().as_bytes());
    Some(out)
}

fn lock(tracker: &Mutex<Tracker>) -> MutexGuard<'_, Tracker> {
    tracker.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The tracker as serve and intake reach it: `cs_data_requests` held only for the one call.
struct Locked<'a>(&'a Mutex<Tracker>);

impl TrackerAccess for Locked<'_> {
    fn with_tracker<R>(&mut self, f: impl FnOnce(&mut Tracker) -> R) -> R {
        f(&mut lock(self.0))
    }
}

impl<S, E, K, W> NetQuorum<S, E, K, W>
where
    S: QuorumDataSource + Send + Sync + 'static,
    S::Q: StoredQuorum,
    S::Error: CaughtException,
    E: EncryptedContributions + Send + Sync + 'static,
    K: QuorumKeyStore + Send + Sync + 'static,
    W: WarmExecutor + Send + Sync + 'static,
{
    // Core: v24.0.0 src/llmq/net_quorum.cpp:32-54 NetQuorum::NetQuorum
    pub fn new(parts: NetQuorumParts<S, E, K, W>) -> Self {
        Self {
            inner: Arc::new(Inner {
                parts,
                tracker: Mutex::new(Tracker::new()),
                interrupt: Interrupt::default(),
            }),
        }
    }

    /// Starts the worker pool, unless the node has no quorum role.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:58-64 NetQuorum::Start
    pub fn start(&self) {
        if self.inner.parts.role == Role::None {
            return;
        }
        self.inner.parts.pool.start();
    }

    /// Sets the interrupt flag: every recovery job ends at its next poll.
    // Core: v24.0.0 src/llmq/net_quorum.h:63 NetQuorum::Interrupt
    pub fn interrupt(&self) {
        self.inner.interrupt.set();
    }

    /// Drops the queued recovery jobs, releasing their claims, and waits for the running ones.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:66-70 NetQuorum::Stop
    pub fn stop(&self) {
        self.inner.parts.pool.clear_queue();
        self.inner.parts.pool.stop();
    }

    /// Hands `every` the expired data request cleanup and its interval, for the node's scheduler.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:72-75 NetQuorum::Schedule
    pub fn schedule(&self, every: impl FnOnce(Box<dyn Fn() + Send + Sync>, Duration)) {
        let this = self.clone();
        every(Box::new(move || this.cleanup_expired_data_requests()), CLEANUP_INTERVAL);
    }

    // Core: v24.0.0 src/llmq/quorumsman.cpp:460-483 CQuorumManager::CleanupExpiredDataRequests
    pub fn cleanup_expired_data_requests(&self) {
        let now = self.inner.parts.env.now();
        lock(&self.inner.tracker).cleanup(now);
    }

    /// The data request map, for a caller's read (`quorum` RPCs and tests).
    pub fn with_tracker<T>(&self, f: impl FnOnce(&Tracker) -> T) -> T {
        f(&lock(&self.inner.tracker))
    }

    /// `quorum getdata`'s request to `peer`: registered under its verified proRegTx hash, then
    /// pushed. False when the peer is not fully connected or not verified, or the registration
    /// is refused.
    // Core: v24.0.0 src/rpc/quorums.cpp:942-954 quorum_getdata
    pub fn request_rpc(&self, peer: PeerId, req: Request) -> bool {
        let peers = &self.inner.parts.peers;
        let Some((_, verified)) = peers.verified_peers().into_iter().find(|(id, _)| *id == peer)
        else {
            return false;
        };
        if verified == [0; 32] {
            return false;
        }
        let now = self.inner.parts.env.now();
        let wire = lock(&self.inner.tracker).request(ProTxHash::from(verified), req, now);
        match wire {
            Ok(wire) => peers.push_qgetdata(peer, &wire),
            Err(_) => false,
        }
    }

    /// Queues `job`, whose claim on `quorum` is held, on the worker pool. Dropped from the queue
    /// before it starts, it releases the claim.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:648-651,682-685 StartVvecSyncThread / StartSkShareRecoveryThread
    pub fn enqueue_recovery(&self, job: ClaimedJob, quorum: S::Q) {
        let mut queued = Queued {
            quorum,
            job: Some(job),
        };
        let this = self.clone();
        self.inner.parts.pool.push(Box::new(move || {
            let Some(job) = queued.job.take() else {
                return;
            };
            this.run_job(job, &queued.quorum);
        }));
    }

    /// One `DataRecoveryThread`: QM1b-4's job polled until it is done, its acts applied in
    /// order and each sleep cut short by the interrupt. The tracker is locked only for the poll.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:537-638,682-685 DataRecoveryThread / StartSkShareRecoveryThread
    fn run_job(&self, job: ClaimedJob, quorum: &S::Q) {
        let parts = &self.inner.parts;
        let me = parts.operator.as_ref().and_then(|o| o.pro_tx_hash()).unwrap_or_default();
        let Some(mut job) = job.start(quorum, &DynValidMns(&*parts.valid_mns), me) else {
            return;
        };
        loop {
            let synced = parts.env.blockchain_synced();
            let interrupted = self.inner.interrupt.is_set();
            let have_vvec = quorum.vvec().is_some();
            let have_sk = quorum.has_sk_share();
            let peers = parts.peers.verified_peers();
            let now = parts.env.now();
            let poll = job.poll(
                now,
                synced,
                have_vvec,
                have_sk,
                interrupted,
                &peers,
                &mut lock(&self.inner.tracker),
            );
            if poll.next == Next::Done {
                quorum.release_recovery();
            }
            for act in poll.acts {
                match act {
                    Act::Send(peer, req) => {
                        parts.peers.push_qgetdata(peer, &QuorumDataRequest::from(&req));
                    }
                    Act::Disconnect(peer) => parts.peers.disconnect(peer),
                    Act::AddPendingMasternode(pro_tx) => parts.peers.add_pending_masternode(pro_tx),
                }
            }
            for line in poll.log {
                debug!(target: "llmq", "{line}");
            }
            match poll.next {
                Next::Done => return,
                Next::Sleep(d) => parts.env.sleep(d, &self.inner.interrupt),
            }
        }
    }

    fn masternode(&self) -> bool {
        matches!(self.inner.parts.role, Role::Masternode { .. })
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:79-201 NetQuorum::ProcessMessage (QGETDATA)
    fn on_qgetdata(&self, cx: &mut HandlerCx<'_>, payload: &[u8]) -> Result<(), Throw> {
        let verified = cx.peer.verified_proreg_hash();
        if !self.masternode() || (verified == [0; 32] && !cx.peer.qwatch()) {
            cx.misbehaving(10, "not a verified masternode or a qwatch connection");
            return Ok(());
        }
        let mut data = payload;
        let req: QuorumDataRequest = match policy::read(&mut data, &cx.ctx) {
            Ok(req) => req,
            Err(ReadError::Throw(t)) => return Err(t),
            Err(refusal) => {
                if let Some((score, msg)) = refusal.misbehaving() {
                    cx.misbehaving(score, &msg);
                }
                return Ok(());
            }
        };
        let parts = &self.inner.parts;
        let served = serve(
            ServeCx {
                source: &parts.source,
                tracker: &mut Locked(&self.inner.tracker),
                requester: ProTxHash::from(verified),
                now: cx.now_us.div_euclid(1_000_000),
                local_mn: parts.operator.is_some(),
                contributions: &parts.contributions,
                scheme: cx.ctx.bls().scheme(),
            },
            &req,
        );
        let out = served.map_err(|e| Throw {
            what: e.to_string().into(),
            type_name: e.type_name(),
        })?;
        if let Some(reply) = out.reply {
            cx.push(out_msg(CommandString::QDATA, &reply));
        }
        for (score, msg) in out.scores {
            cx.misbehaving(score, msg);
        }
        Ok(())
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:203-270 NetQuorum::ProcessMessage (QDATA)
    fn on_qdata(&self, cx: &mut HandlerCx<'_>, payload: &[u8]) -> Result<(), Throw> {
        let verified = cx.peer.verified_proreg_hash();
        if self.inner.parts.role == Role::None || verified == [0; 32] {
            cx.misbehaving(10, "not a verified masternode and -watchquorums is not enabled");
            return Ok(());
        }
        let mut data = payload;
        let msg: QData = cx.read(&mut data)?;
        let raw_error =
            payload.get(QuorumDataRequest::SIZE).copied().unwrap_or(Request::ERROR_UNDEFINED);
        let parts = &self.inner.parts;
        let source = KeepCached {
            source: &parts.source,
            cached: RefCell::new(None),
        };
        let mut effects = Vec::new();
        let result = intake(
            IntakeCx {
                source: &source,
                tracker: &mut Locked(&self.inner.tracker),
                requester: ProTxHash::from(verified),
                peer: cx.id(),
                operator: parts.operator.as_deref().map(|o| o as &dyn OperatorDecrypt),
                scheme: cx.ctx.bls().scheme(),
            },
            &msg,
            &mut |e| effects.push(e),
        );
        let quorum = source.cached.into_inner();
        for effect in effects {
            match effect {
                Effect::Score(score, msg) => cx.misbehaving(score, msg),
                Effect::Log(line) => match qdata_error_line(&line, raw_error) {
                    Some(bytes) => debug!(target: "llmq", "{}", String::from_utf8_lossy(&bytes)),
                    None => debug!(target: "llmq", "{line}"),
                },
                Effect::QueueWarm(_) => {
                    if let Some(q) = &quorum {
                        parts.warm.spawn_warm(q.warm_job());
                    }
                }
                Effect::Write(_) => {
                    if let Some(q) = &quorum {
                        write_contributions(&parts.store, q)?;
                    }
                }
            }
        }
        result.map_err(|t| Throw::ios_failure(t.msg))
    }
}

/// `m_qman.WriteContributions(pQuorum)`: a failed write is `HandleError`'s two lines and its
/// throw, which `ProcessMessages` catches.
// Core: v24.0.0 src/llmq/quorums.cpp:192-207 CQuorum::WriteContributions; src/dbwrapper.cpp:261-268 HandleError
fn write_contributions<K: QuorumKeyStore, Q: StoredQuorum>(store: &K, q: &Q) -> Result<(), Throw> {
    store.write(&q.db_key(), &q.contributions()).map_err(|e| {
        let what = format!("Fatal LevelDB error: {e}");
        info!("{what}");
        info!("{DEBUG_LEVELDB}");
        Throw {
            what: what.into(),
            type_name: DBWRAPPER_ERROR,
        }
    })
}

impl<S, E, K, W> NetHandler for NetQuorum<S, E, K, W>
where
    S: QuorumDataSource + Send + Sync + 'static,
    S::Q: StoredQuorum,
    S::Error: CaughtException,
    E: EncryptedContributions + Send + Sync + 'static,
    K: QuorumKeyStore + Send + Sync + 'static,
    W: WarmExecutor + Send + Sync + 'static,
{
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::NetQuorum)
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:77-271 NetQuorum::ProcessMessage
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        if *cmd == CommandString::QGETDATA {
            self.on_qgetdata(cx, payload)
        } else if *cmd == CommandString::QDATA {
            self.on_qdata(cx, payload)
        } else {
            Ok(())
        }
    }

    fn interrupt(&mut self) {
        NetQuorum::interrupt(self);
    }

    fn stop(&mut self) {
        NetQuorum::stop(self);
    }
}

#[cfg(test)]
mod tests;
