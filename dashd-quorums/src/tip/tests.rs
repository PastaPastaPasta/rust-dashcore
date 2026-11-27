use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use dash_evo::types::ProTxHash;
use dash_llmq::dkg::wire::QuorumDataRequest;
use dash_llmq::qdata::PeerId;
use dash_llmq::qdata::source::{QuorumData, ValidMnsAt};
use dash_params::{ChainParamsBuilder, Network, Release};

use super::*;
use crate::net_quorum::tests::{ME, TestOperator, TestQuorum};
use crate::net_quorum::{NetQuorumParts, QuorumPeers, RecoveryEnv, RecoveryPool};
use crate::source::NoEncryptedContributions;
use crate::store::MemKeyStore;

type Log = Arc<Mutex<Vec<String>>>;

fn llmqs() -> Vec<(LlmqType, LlmqParams)> {
    let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    setup.consensus.llmq_params().map(|(t, p)| (t, *p)).collect()
}

fn tip(height: i32) -> Tip {
    Tip {
        id: BlockId(height as u32),
        at: TipRef {
            height,
            hash: BlockHash::from([height as u8; 32]),
        },
    }
}

struct RecordingCheck(Log);

impl QuorumConnectionsCheck for RecordingCheck {
    fn check(&self, llmq_type: LlmqType, _: &LlmqParams, tip: &TipRef, _: &Role) {
        self.0.lock().unwrap().push(format!("check {llmq_type:?} at {}", tip.height));
    }
}

struct RecordingSteps(Log);

impl TipSteps for RecordingSteps {
    fn cleanup_expired_data_requests(&self) {
        self.0.lock().unwrap().push("cleanup requests".into());
    }

    fn trigger_recovery(&self, role: Role, tip: &Tip) -> Result<(), TipFault> {
        self.0.lock().unwrap().push(format!("recovery {role:?} at {}", tip.at.height));
        Ok(())
    }

    fn start_cleanup_old_quorum_data(&self, tip: &Tip) {
        self.0.lock().unwrap().push(format!("old data at {}", tip.at.height));
    }
}

fn recording(role: Role, synced: bool) -> (NetQuorumTip<RecordingSteps>, Log) {
    let log = Log::default();
    let handler = NetQuorumTip::new(
        Arc::new(move || role),
        Arc::new(move || synced),
        llmqs(),
        RecordingSteps(Arc::clone(&log)),
    )
    .with_connections_check(Arc::new(RecordingCheck(Arc::clone(&log))));
    (handler, log)
}

fn checks(height: i32) -> Vec<String> {
    llmqs().iter().map(|(t, _)| format!("check {t:?} at {height}")).collect()
}

const MN: Role = Role::Masternode {
    pro_tx: None,
    watching: false,
};

#[test]
fn the_no_op_check_is_object_safe() {
    let (t, params) = llmqs()[0];
    let check: Arc<dyn QuorumConnectionsCheck> = Arc::new(NoQuorumConnectionsCheck);
    check.check(t, &params, &tip(1).at, &Role::Observer);
}

/// Each `UpdatedBlockTip` guard on its own returns before any step: no role, a null tip, IBD,
/// a tip equal to its fork, and an unsynced blockchain.
// Core: v24.0.0 src/llmq/net_quorum.cpp:392-395
#[test]
fn each_tip_guard_returns_before_any_step() {
    let t = tip(7);
    let cases = [
        ("no role", Role::None, true, Some(&t), None::<BlockId>, false),
        ("null tip", MN, true, None, None, false),
        ("IBD", MN, true, Some(&t), None, true),
        ("new == fork", MN, true, Some(&t), Some(t.id), false),
        ("unsynced", Role::Observer, false, Some(&t), None, false),
    ];
    for (name, role, synced, new, fork, ibd) in cases {
        let (handler, log) = recording(role, synced);
        handler.updated_block_tip(new, fork, ibd).unwrap();
        assert!(log.lock().unwrap().is_empty(), "{name}: {:?}", log.lock().unwrap());
    }
    let (handler, log) = recording(MN, true);
    handler.updated_block_tip(Some(&t), Some(BlockId(6)), false).unwrap();
    assert!(!log.lock().unwrap().is_empty(), "a fork below the tip passes");
}

/// The five steps in Core's order: the P7 check per type in `Params().llmqs` order, the data
/// request cleanup, the recovery trigger, then the old data cleanup.
// Core: v24.0.0 src/llmq/net_quorum.cpp:397-403
#[test]
fn the_tip_steps_run_in_cores_order() {
    for role in [MN, Role::Observer] {
        let (handler, log) = recording(role, true);
        handler.updated_block_tip(Some(&tip(9)), None, false).unwrap();
        let mut want = checks(9);
        want.extend([
            "cleanup requests".to_owned(),
            format!("recovery {role:?} at 9"),
            "old data at 9".to_owned(),
        ]);
        assert_eq!(*log.lock().unwrap(), want);
    }
}

/// Role None returns before both init paths: no P7 check and no job.
// Core: v24.0.0 src/llmq/net_quorum.cpp:379
#[test]
fn init_with_no_role_does_nothing() {
    for synced in [false, true] {
        let (handler, log) = recording(Role::None, synced);
        handler.initialize_current_block_tip(Some(&tip(3)), false).unwrap();
        assert!(log.lock().unwrap().is_empty());
    }
}

/// At startup the blockchain is unsynced: only the unguarded per-type check runs, and no
/// recovery starts.
// Core: v24.0.0 src/llmq/net_quorum.cpp:380-385, src/masternode/sync.h:56,79
#[test]
fn init_unsynced_runs_only_the_unguarded_check() {
    for ibd in [false, true] {
        let (handler, log) = recording(MN, false);
        handler.initialize_current_block_tip(Some(&tip(4)), ibd).unwrap();
        assert_eq!(*log.lock().unwrap(), checks(4));
    }
    let (handler, log) = recording(Role::Observer, false);
    handler.initialize_current_block_tip(None, false).unwrap();
    assert!(log.lock().unwrap().is_empty(), "a null tip has no check");
}

/// Synced at init: the guarded path runs with a null fork, then the per-type check again.
#[test]
fn init_synced_runs_the_guarded_path_then_the_check() {
    let (handler, log) = recording(MN, true);
    handler.initialize_current_block_tip(Some(&tip(5)), false).unwrap();
    let mut want = checks(5);
    want.extend([
        "cleanup requests".to_owned(),
        format!("recovery {MN:?} at 5"),
        "old data at 5".to_owned(),
    ]);
    want.extend(checks(5));
    assert_eq!(*log.lock().unwrap(), want);
}

#[derive(Debug)]
struct NeverFails;

impl std::fmt::Display for NeverFails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("never")
    }
}

impl CaughtException for NeverFails {
    fn type_name(&self) -> &'static str {
        "never"
    }
}

struct NoQuorums;

impl QuorumDataSource for NoQuorums {
    type Q = TestQuorum;
    type Error = NeverFails;
    fn llmq_registered(&self, _: LlmqType) -> bool {
        true
    }
    fn active_block(&self, _: &BlockHash) -> bool {
        false
    }
    fn quorum(&self, _: LlmqType, _: &BlockHash) -> Result<Option<TestQuorum>, NeverFails> {
        Ok(None)
    }
    fn cached_quorum(&self, _: LlmqType, _: &BlockHash) -> Option<TestQuorum> {
        None
    }
}

struct NoWarm;

impl WarmExecutor for NoWarm {
    fn spawn_warm(&self, _: Box<dyn FnOnce() + Send>) {}
}

struct NoPeers;

impl QuorumPeers for NoPeers {
    fn verified_peers(&self) -> Vec<(PeerId, [u8; 32])> {
        Vec::new()
    }
    fn push_qgetdata(&self, _: PeerId, _: &QuorumDataRequest) -> bool {
        false
    }
    fn disconnect(&self, _: PeerId) {}
    fn add_pending_masternode(&self, _: ProTxHash) {}
}

struct Env;

impl RecoveryEnv for Env {
    fn now(&self) -> i64 {
        0
    }
    fn blockchain_synced(&self) -> bool {
        true
    }
}

struct NoMns;

impl ValidMnsAt for NoMns {
    fn valid_pro_tx_hashes(&self, _: &BlockHash) -> Option<Vec<ProTxHash>> {
        None
    }
}

/// Keeps the jobs queued on it and never runs them: a dropped job releases its claim.
#[derive(Default)]
struct CountingPool(Mutex<Vec<Box<dyn FnOnce() + Send>>>);

impl CountingPool {
    fn queued(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

impl RecoveryPool for CountingPool {
    fn start(&self) {}
    fn push(&self, job: Box<dyn FnOnce() + Send>) {
        self.0.lock().unwrap().push(job);
    }
    fn clear_queue(&self) {}
    fn stop(&self) {}
}

/// `quorums` for `LlmqTest` at hashes `[1; 32]`, `[2; 32]`, …, none for the other types, and
/// `fail`'s error for its type.
struct Scans {
    quorums: Vec<TestQuorum>,
    fail: Option<(LlmqType, ScanError)>,
    log: Log,
}

impl RecoveryScan for Scans {
    type Q = TestQuorum;
    fn scan(
        &self,
        t: LlmqType,
        _: BlockId,
        count: usize,
    ) -> Result<Vec<(BlockHash, TestQuorum)>, ScanError> {
        self.log.lock().unwrap().push(format!("scan {t:?} count {count}"));
        if let Some((_, e)) = self.fail.as_ref().filter(|(f, _)| *f == t) {
            return Err(e.clone());
        }
        if t != LlmqType::LlmqTest {
            return Ok(Vec::new());
        }
        Ok(self
            .quorums
            .iter()
            .enumerate()
            .map(|(i, q)| (BlockHash::from([i as u8 + 1; 32]), q.clone()))
            .collect())
    }
}

struct Cleanup(Log);

impl OldDataCleanup for Cleanup {
    fn start_cleanup_old_quorum_data(&self, _: BlockId, height: u32) {
        self.0.lock().unwrap().push(format!("old data at {height}"));
    }
}

type Steps =
    RecoverySteps<NoQuorums, NoEncryptedContributions, MemKeyStore, NoWarm, Scans, Cleanup>;

fn net_quorum(
    role: Role,
    pool: Arc<dyn RecoveryPool>,
) -> NetQuorum<NoQuorums, NoEncryptedContributions, MemKeyStore, NoWarm> {
    NetQuorum::new(NetQuorumParts {
        role,
        source: NoQuorums,
        contributions: NoEncryptedContributions,
        operator: Some(Arc::new(TestOperator(Arc::default()))),
        valid_mns: Arc::new(NoMns),
        store: MemKeyStore::new(),
        warm: NoWarm,
        peers: Arc::new(NoPeers),
        env: Arc::new(Env),
        pool,
    })
}

fn recovery(
    role: Role,
    synced: bool,
    quorums: Vec<TestQuorum>,
    enabled: bool,
) -> (NetQuorumTip<Steps>, Arc<CountingPool>, Log) {
    failing_recovery(role, synced, quorums, enabled, None)
}

fn failing_recovery(
    role: Role,
    synced: bool,
    quorums: Vec<TestQuorum>,
    enabled: bool,
    fail: Option<(LlmqType, ScanError)>,
) -> (NetQuorumTip<Steps>, Arc<CountingPool>, Log) {
    let pool = Arc::new(CountingPool::default());
    let net = net_quorum(role, Arc::clone(&pool) as Arc<dyn RecoveryPool>);
    let log = Log::default();
    let all = llmqs();
    let steps = RecoverySteps {
        net,
        scan: Scans {
            quorums,
            fail,
            log: Arc::clone(&log),
        },
        cleanup: Cleanup(Arc::clone(&log)),
        enabled,
        sync: Vec::new(),
        llmqs: all.iter().map(|(t, p)| (*t, usize::from(p.keep_old_connections))).collect(),
    };
    let handler = NetQuorumTip::new(Arc::new(move || role), Arc::new(move || synced), all, steps);
    (handler, pool, log)
}

fn me_mn() -> Role {
    Role::Masternode {
        pro_tx: Some(ProTxHash::from(ME)),
        watching: false,
    }
}

/// `NetQuorum` owns its `workerPool`: with no role `Start` spawns no `q-mngr` thread; with a role
/// it spawns them, and `Stop` clears the queue and joins them.
// Core: v24.0.0 src/llmq/net_quorum.cpp:58-70
#[test]
fn a_role_none_net_quorum_starts_no_pool_threads() {
    for (role, started) in [(Role::None, false), (Role::Observer, true), (me_mn(), true)] {
        let pool = crate::recovery_rt::RecoveryPool::new(2);
        let net = net_quorum(role, Arc::clone(&pool) as Arc<dyn RecoveryPool>);
        net.start();
        assert_eq!(pool.is_started(), started, "{role:?}");
        assert_eq!(pool.is_running(), started, "{role:?}");
        let (tx, rx) = std::sync::mpsc::channel();
        RecoveryPool::push(&*pool, Box::new(move || tx.send(()).unwrap()));
        let wait = Duration::from_millis(if started {
            30_000
        } else {
            100
        });
        assert_eq!(rx.recv_timeout(wait).is_ok(), started, "{role:?}");
        net.interrupt();
        net.stop();
        assert!(!pool.is_running(), "{role:?}: the stop joined the workers");
    }
}

/// Plan §4 [r1]: synced at init, the recovery trigger plans over each type's scan in
/// `Params().llmqs` order and queues a claimed job for each quorum missing data, then the old
/// data cleanup runs.
#[test]
fn init_synced_plans_and_queues_recovery() {
    let quorums = vec![TestQuorum::new(), TestQuorum::new()];
    let (handler, pool, log) = recovery(me_mn(), true, quorums.clone(), true);
    handler.initialize_current_block_tip(Some(&tip(30)), false).unwrap();
    assert_eq!(pool.queued(), 2);
    assert!(quorums.iter().all(|q| q.recovery_running()), "claimed before the enqueue");
    let mut want: Vec<String> = llmqs()
        .iter()
        .map(|(t, p)| format!("scan {t:?} count {}", p.keep_old_connections))
        .collect();
    want.push("old data at 30".into());
    assert_eq!(*log.lock().unwrap(), want);

    handler.updated_block_tip(Some(&tip(31)), None, false).unwrap();
    assert_eq!(pool.queued(), 2, "a claimed quorum is already running");
}

/// Unsynced at init, nothing is scanned or queued; `-llmq-data-recovery=0` scans nothing but
/// still runs the old data cleanup.
#[test]
fn no_recovery_unsynced_or_disabled() {
    let (handler, pool, log) = recovery(me_mn(), false, vec![TestQuorum::new()], true);
    handler.initialize_current_block_tip(Some(&tip(30)), false).unwrap();
    assert_eq!(pool.queued(), 0);
    assert!(log.lock().unwrap().is_empty());

    let (handler, pool, log) = recovery(me_mn(), true, vec![TestQuorum::new()], false);
    handler.updated_block_tip(Some(&tip(30)), None, false).unwrap();
    assert_eq!(pool.queued(), 0);
    assert_eq!(*log.lock().unwrap(), ["old data at 30"]);
}

/// A quorum that has its vvec needs only the share; an observer without `-llmq-qvvec-sync`
/// queues nothing.
#[test]
fn only_quorums_missing_data_get_jobs() {
    let (handler, pool, _) = recovery(Role::Observer, true, vec![TestQuorum::new()], true);
    handler.updated_block_tip(Some(&tip(30)), None, false).unwrap();
    assert_eq!(pool.queued(), 0);

    let (handler, pool, _) = recovery(me_mn(), true, vec![TestQuorum::new().with_vvec()], true);
    handler.updated_block_tip(Some(&tip(30)), None, false).unwrap();
    assert_eq!(pool.queued(), 1, "the share is still missing");
}

/// Steps that block until released, recording the order they ran in.
struct Blocking {
    gate: Mutex<Receiver<()>>,
    log: Log,
}

impl TipSteps for Blocking {
    fn cleanup_expired_data_requests(&self) {}
    fn trigger_recovery(&self, _: Role, tip: &Tip) -> Result<(), TipFault> {
        let _ = self.gate.lock().unwrap().recv();
        self.log.lock().unwrap().push(format!("tip {}", tip.at.height));
        Ok(())
    }
    fn start_cleanup_old_quorum_data(&self, _: &Tip) {}
}

fn blocking_worker(armed: bool) -> (Arc<TipWorker>, JoinHandle<()>, Sender<()>, Log) {
    let (release, gate) = channel();
    let log = Log::default();
    let tip_handler = NetQuorumTip::new(
        Arc::new(|| MN),
        Arc::new(|| true),
        Vec::new(),
        Blocking {
            gate: Mutex::new(gate),
            log: Arc::clone(&log),
        },
    );
    let (worker, handle) = TipWorker::spawn(
        tip_handler,
        Box::new(|id: BlockId| Some(tip(id.0 as i32))),
        Arc::new(AtomicBool::new(armed)),
        Box::new(|_| {}),
    )
    .unwrap();
    (worker, handle, release, log)
}

/// Addendum 2: the listener only posts. While the handler is parked (as a V read would park it)
/// every signal returns at once, and the worker then runs the events in order on `d-q-tip`.
#[test]
fn the_listener_never_waits_on_the_handler() {
    let (worker, handle, release, log) = blocking_worker(true);
    let at = Instant::now();
    worker.initialize_current_block_tip(Some(BlockId(1)), false);
    for h in 2..6 {
        worker.updated_block_tip(BlockId(h), None, false);
    }
    assert!(at.elapsed() < Duration::from_secs(1), "the signals returned at once");
    assert!(log.lock().unwrap().is_empty(), "the handler is parked");
    for _ in 1..6 {
        release.send(()).unwrap();
    }
    worker.close();
    handle.join().unwrap();
    assert_eq!(*log.lock().unwrap(), ["tip 1", "tip 2", "tip 3", "tip 4", "tip 5"]);
}

/// Unarmed (a masternode before its `Init`) the startup broadcast is dropped; tips still pass.
#[test]
fn an_unarmed_worker_drops_only_the_startup_broadcast() {
    let (worker, handle, release, log) = blocking_worker(false);
    worker.initialize_current_block_tip(Some(BlockId(1)), false);
    worker.updated_block_tip(BlockId(2), None, false);
    worker.armed.store(true, Ordering::SeqCst);
    worker.initialize_current_block_tip(Some(BlockId(3)), false);
    drop(release);
    worker.close();
    handle.join().unwrap();
    assert_eq!(*log.lock().unwrap(), ["tip 2", "tip 3"]);
}

/// Steps that record whether the thread they run on carries LO-9's no-wait marker.
struct MarkProbe(Arc<Mutex<Vec<bool>>>);

impl TipSteps for MarkProbe {
    fn cleanup_expired_data_requests(&self) {}
    fn trigger_recovery(&self, _: Role, _: &Tip) -> Result<(), TipFault> {
        self.0.lock().unwrap().push(dashd_chainstate::chain::no_wait_marked());
        Ok(())
    }
    fn start_cleanup_old_quorum_data(&self, _: &Tip) {}
}

/// The handler may read V, so `d-q-tip` is never no-wait-marked, even when the worker is spawned
/// and signalled from a marked thread (the `ValidationSignals` thread is one).
// Core: none (rdashd's LO-9 marker, dashd-chainstate/src/chain/reply.rs:4-12)
#[test]
fn the_tip_worker_thread_is_never_no_wait_marked() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let probe = MarkProbe(Arc::clone(&seen));
    let joined = std::thread::spawn(move || {
        let _marked = dashd_chainstate::chain::NoWaitGuard::set();
        assert!(dashd_chainstate::chain::no_wait_marked());
        let tip_handler = NetQuorumTip::new(Arc::new(|| MN), Arc::new(|| true), Vec::new(), probe);
        let (worker, handle) = TipWorker::spawn(
            tip_handler,
            Box::new(|id: BlockId| Some(tip(id.0 as i32))),
            Arc::new(AtomicBool::new(true)),
            Box::new(|what| panic!("the worker panicked: {what}")),
        )
        .unwrap();
        worker.initialize_current_block_tip(Some(BlockId(1)), false);
        worker.updated_block_tip(BlockId(2), None, false);
        worker.close();
        handle
    })
    .join()
    .unwrap();
    joined.join().unwrap();
    assert_eq!(*seen.lock().unwrap(), [false, false]);
}

/// R-c/DEC-1646: a store read error in the recovery scan is Core's `ScanQuorums` throw. It ends
/// the tip handler there: no later type is scanned and the old data cleanup does not start. V
/// gone still skips only that type.
// Core: v24.0.0 src/llmq/net_quorum.cpp:509, :403
#[test]
fn a_scan_read_error_ends_the_tip_and_v_gone_skips_only_its_type() {
    let all = llmqs();
    assert!(all.len() > 2);
    let (first, second) = (all[0].0, all[1].0);
    let scanned = |n: usize| -> Vec<String> {
        all[..n]
            .iter()
            .map(|(t, p)| format!("scan {t:?} count {}", p.keep_old_connections))
            .collect()
    };
    let read = ScanError::Read("disk on fire".into());
    let (handler, _, log) =
        failing_recovery(me_mn(), true, vec![TestQuorum::new()], true, Some((second, read)));
    let fault = handler.updated_block_tip(Some(&tip(30)), None, false).unwrap_err();
    assert_eq!(
        fault,
        TipFault("NetQuorum::TriggerQuorumDataRecoveryThreads -- ScanQuorums: disk on fire".into())
    );
    assert_eq!(*log.lock().unwrap(), scanned(2), "no later type, no old data cleanup");
    log.lock().unwrap().clear();
    let fault = handler.initialize_current_block_tip(Some(&tip(31)), false).unwrap_err();
    assert!(fault.0.ends_with("disk on fire"));
    assert_eq!(*log.lock().unwrap(), scanned(2));

    let (handler, _, log) = failing_recovery(
        me_mn(),
        true,
        vec![TestQuorum::new()],
        true,
        Some((first, ScanError::VGone)),
    );
    handler.updated_block_tip(Some(&tip(30)), None, false).unwrap();
    let mut want = scanned(all.len());
    want.push("old data at 30".into());
    assert_eq!(*log.lock().unwrap(), want, "V gone skips only its type");
}

/// Steps whose recovery trigger faults on every tip.
struct Faulty(Log);

impl TipSteps for Faulty {
    fn cleanup_expired_data_requests(&self) {}
    fn trigger_recovery(&self, _: Role, tip: &Tip) -> Result<(), TipFault> {
        self.0.lock().unwrap().push(format!("tip {}", tip.at.height));
        Err(TipFault(format!("read failed at {}", tip.at.height)))
    }
    fn start_cleanup_old_quorum_data(&self, _: &Tip) {
        self.0.lock().unwrap().push("old data".into());
    }
}

/// A fault is reported once on the panic path's sink, and the worker then drops every event, as
/// after a panic: Core's throw escapes the thread that runs the signals.
#[test]
fn a_tip_fault_is_reported_once_and_ends_the_worker() {
    let log = Log::default();
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reported);
    let tip_handler =
        NetQuorumTip::new(Arc::new(|| MN), Arc::new(|| true), Vec::new(), Faulty(Arc::clone(&log)));
    let (worker, handle) = TipWorker::spawn(
        tip_handler,
        Box::new(|id: BlockId| Some(tip(id.0 as i32))),
        Arc::new(AtomicBool::new(true)),
        Box::new(move |what| sink.lock().unwrap().push(what)),
    )
    .unwrap();
    worker.updated_block_tip(BlockId(1), None, false);
    worker.updated_block_tip(BlockId(2), None, false);
    worker.close();
    handle.join().unwrap();
    assert_eq!(*reported.lock().unwrap(), ["read failed at 1"]);
    assert_eq!(*log.lock().unwrap(), ["tip 1"]);
}

/// BL-945: the barrier returns once `d-q-tip` has finished every event posted before the call,
/// not on the first of them, and the ones a fault made it drop count too.
#[test]
fn a_tip_barrier_waits_for_the_events_posted_before_it() {
    let (worker, handle, release, log) = blocking_worker(true);
    let barrier = worker.barrier();
    barrier.wait_for_posted();
    worker.updated_block_tip(BlockId(1), None, false);
    worker.updated_block_tip(BlockId(2), None, false);
    let waiter = std::thread::spawn(move || barrier.wait_for_posted());
    release.send(()).unwrap();
    while log.lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(!waiter.is_finished(), "the barrier waits for the second event too");
    release.send(()).unwrap();
    waiter.join().unwrap();
    assert_eq!(*log.lock().unwrap(), ["tip 1", "tip 2"]);
    worker.close();
    handle.join().unwrap();
}

/// A worker that faulted still counts each event it drops, so its barrier never holds the DKG.
#[test]
fn a_tip_barrier_counts_the_events_a_fault_drops() {
    let log = Log::default();
    let tip_handler =
        NetQuorumTip::new(Arc::new(|| MN), Arc::new(|| true), Vec::new(), Faulty(Arc::clone(&log)));
    let (worker, handle) = TipWorker::spawn(
        tip_handler,
        Box::new(|id: BlockId| Some(tip(id.0 as i32))),
        Arc::new(AtomicBool::new(true)),
        Box::new(|_| {}),
    )
    .unwrap();
    let barrier = worker.barrier();
    for h in 1..4 {
        worker.updated_block_tip(BlockId(h), None, false);
    }
    let (done, passed) = channel();
    std::thread::spawn(move || {
        barrier.wait_for_posted();
        let _ = done.send(());
    });
    assert!(passed.recv_timeout(Duration::from_secs(10)).is_ok(), "the barrier passed");
    assert_eq!(*log.lock().unwrap(), ["tip 1"]);
    worker.close();
    handle.join().unwrap();
}
