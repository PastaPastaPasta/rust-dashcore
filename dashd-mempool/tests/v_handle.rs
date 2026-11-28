//! The [`MempoolHandle`] on a live pool under a real V: admissions as class-M turns in arrival
//! order (`ProcessTransaction` under `cs_main`, `validation.cpp:4920-4932` at v24.0.0), the relay
//! reads and the writes beside them.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::Duration;

use dash_consensus::TxRef;
use dash_consensus::primitives::{OutPoint, Tx, TxHash};
use dash_consensus::validation::TxValidationResult;
use dashd_chainstate::chain::{
    ActorReply, Blocking, ChainCommand, MempoolCompletion, NoWaitGuard, RecordingAbort,
    RemovalReason, SignalsThread, ValidationInterface, ValidationSignals, WaitError,
};
use dashd_mempool::PoolSink;
use dashd_mempool::api::MempoolHandle;
use dashd_mempool::options::MemPoolOptions;
use dashd_mempool::pool::Mempool;
use dashd_mempool::types::{
    MempoolAcceptResult, MempoolAcceptResultType, PackageMempoolAcceptResult,
    PackageValidationResult, PackageValidationState,
};
use support::{MATURITY, Matured, OUTPUT_VALUE, V, child, params, spend};

/// Mature coinbases at heights `1..=SPENDABLE` once the rig is up.
const SPENDABLE: u32 = 8;

/// What the listener saw, in order, and whether each added entry was resident when its
/// `TransactionAddedToMempool` ran.
#[derive(Default)]
struct Events {
    seen: Mutex<Vec<String>>,
    pool: OnceLock<Weak<Mempool>>,
}

impl Events {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.seen.lock().expect("events"))
    }
}

impl ValidationInterface for Events {
    fn transaction_added_to_mempool(&self, tx: &TxRef, accept_time: i64, sequence: u64) {
        let pool = self.pool.get().and_then(Weak::upgrade).expect("the pool");
        let resident = pool.read().exists(&tx.txid());
        self.seen
            .lock()
            .expect("events")
            .push(format!("added {} at {accept_time} #{sequence} resident={resident}", tx.txid()));
    }
    fn transaction_removed_from_mempool(&self, tx: &TxRef, reason: RemovalReason, _: u64) {
        self.seen.lock().expect("events").push(format!("remove {reason:?} {}", tx.txid()));
    }
}

/// V with the real pool as its sink, the pool's admission worker attached, and coinbases at
/// heights `1..=SPENDABLE` mature for the next block.
struct Rig {
    v: V,
    m: Matured,
    pool: Arc<Mempool>,
    handle: Arc<dyn MempoolHandle>,
    events: Arc<Events>,
    signals: ValidationSignals,
    thread: SignalsThread,
}

impl Rig {
    fn start() -> Rig {
        let p = params();
        let abort = Arc::new(RecordingAbort::default());
        let signals = ValidationSignals::default();
        let events = Arc::new(Events::default());
        signals.register(events.clone());
        let thread = signals.start(abort.clone()).expect("the signals thread");
        let options = MemPoolOptions {
            require_standard: false,
            check_ratio: 1,
            ..MemPoolOptions::default()
        };
        let pool = Arc::new(Mempool::with_signals(options, signals.clone()));
        events.pool.set(Arc::downgrade(&pool)).expect("set once");
        let sink = Box::new(PoolSink::new(pool.clone(), p.clone()));
        let v = V::start(false, &p, sink, &signals, &abort);
        pool.set_clock(v.clock.clone());
        pool.attach_chain(v.v.clone()).expect("the admission worker");
        let mut m = Matured::new();
        for h in MATURITY + 1..MATURITY + SPENDABLE {
            let block = m.on(m.tip(), h, 0, &[]);
            m.chain.push(block);
        }
        v.connect(&m.chain[1..]);
        let handle: Arc<dyn MempoolHandle> = pool.clone();
        Rig {
            v,
            m,
            pool,
            handle,
            events,
            signals,
            thread,
        }
    }

    /// A spend of the coinbase at height `h` paying `fee`.
    fn spend_coinbase(&self, h: usize, fee: i64) -> TxRef {
        spend(&[(self.m.coin_at(h), u32::MAX)], 0, (OUTPUT_VALUE - fee) / 2)
    }

    /// What the listener saw since the last call, every earlier signal run.
    fn seen(&self) -> Vec<String> {
        self.signals.sync(&mut Blocking).expect("signals");
        self.events.take()
    }

    fn resident(&self, tx: &Tx) -> bool {
        self.pool.read().exists(&tx.txid())
    }

    fn stop(self) -> Arc<Mempool> {
        self.v.v.shutdown();
        let exit = self.v.join.join().expect("an unmarked joiner").expect("V does not panic");
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert!(self.v.abort.messages().is_empty(), "{:?}", self.v.abort.messages());
        let _ = self.thread.stop();
        self.pool
    }
}

fn wait(reply: ActorReply<MempoolAcceptResult>) -> MempoolAcceptResult {
    reply.recv().expect("the turn replies")
}

fn assert_valid(result: &MempoolAcceptResult, fee: i64) {
    assert_eq!(result.result_type, MempoolAcceptResultType::Valid, "{result:?}");
    assert_eq!(result.base_fee, Some(fee), "{result:?}");
}

fn assert_invalid(result: &MempoolAcceptResult, class: TxValidationResult, reason: &str) {
    assert_eq!(result.result_type, MempoolAcceptResultType::Invalid, "{result:?}");
    assert_eq!((result.validation_result, result.reason.as_str()), (class, reason), "{result:?}");
}

fn added(tx: &Tx, at: i64, sequence: u64) -> String {
    format!("added {} at {at} #{sequence} resident=true", tx.txid())
}

/// `ProcessTransaction` (`validation.cpp:4920-4932`) round trips: `accept` and `accept_async`
/// admit at `GetTime()` with `TransactionAddedToMempool` and its sequence (`:1455`) published
/// before the reply resolves; the listener runs off the guard and sees the entry; a second
/// submission is `txn-already-in-mempool` (`:896-900`); the relay reads see what was admitted,
/// parents by admission time (`net_processing.cpp:2996-3010`).
#[test]
fn handle_round_trips_through_v_turns_and_the_relay_reads_see_them() {
    let rig = Rig::start();
    let at = rig.v.clock.mocktime();
    let parent = rig.spend_coinbase(1, 2_000);
    let result = rig.handle.accept(parent.clone(), false).expect("V replies");
    assert_valid(&result, 2_000);
    assert_eq!(rig.seen(), [added(&parent, at, 1)]);

    rig.v.clock.set_mocktime(at + 10);
    let kid = child(&parent, 10_000);
    let result = wait(rig.handle.accept_async(kid.clone(), Some(7)));
    assert_valid(&result, 4_000);
    assert_eq!(rig.seen(), [added(&kid, at + 10, 2)]);

    let again = rig.handle.accept(parent.clone(), false).expect("V replies");
    assert_invalid(&again, TxValidationResult::Conflict, "txn-already-in-mempool");
    assert!(rig.seen().is_empty());

    let (infos, newer, older) = rig.handle.read(|read| {
        (
            read.info_all().iter().map(|info| (info.tx.txid(), info.time_s)).collect::<Vec<_>>(),
            read.parents_entered_after(&kid.txid(), at - 1),
            read.parents_entered_after(&kid.txid(), at),
        )
    });
    assert_eq!(infos, [(parent.txid(), at), (kid.txid(), at + 10)]);
    assert_eq!((newer, older), (vec![parent.txid()], vec![]));
    rig.stop();
}

/// Missing inputs are `Invalid` with `TX_MISSING_INPUTS` and
/// `bad-txns-inputs-missingorspent` (`validation.cpp:972-990`), and the pool is unchanged.
#[test]
fn missing_inputs_are_invalid_with_tx_missing_inputs() {
    let rig = Rig::start();
    let orphan = spend(&[(OutPoint::new(TxHash::from([0x77; 32]), 0), u32::MAX)], 0, 1_000);
    let result = rig.handle.accept(orphan.clone(), false).expect("V replies");
    assert_invalid(&result, TxValidationResult::MissingInputs, "bad-txns-inputs-missingorspent");
    assert!(!rig.resident(&orphan));
    assert!(rig.seen().is_empty());
    rig.stop();
}

/// Arrival order is admission order (`ProcessTransaction` serialised by `cs_main`): a parent
/// submitted before its child admits both; a child submitted first is missing its inputs
/// (`validation.cpp:972-990`) and the parent after it still admits.
#[test]
fn arrival_order_decides_whether_a_child_finds_its_parent() {
    let rig = Rig::start();
    let parent = rig.spend_coinbase(1, 2_000);
    let kid = child(&parent, 10_000);
    let first = rig.handle.accept_async(parent.clone(), None);
    let second = rig.handle.accept_async(kid.clone(), None);
    assert_eq!(wait(second).result_type, MempoolAcceptResultType::Valid);
    assert_eq!(wait(first).result_type, MempoolAcceptResultType::Valid);

    let parent = rig.spend_coinbase(2, 2_000);
    let kid = child(&parent, 10_000);
    let first = rig.handle.accept_async(kid.clone(), None);
    let second = rig.handle.accept_async(parent.clone(), None);
    assert_invalid(
        &wait(first),
        TxValidationResult::MissingInputs,
        "bad-txns-inputs-missingorspent",
    );
    assert_eq!(wait(second).result_type, MempoolAcceptResultType::Valid);
    assert!(!rig.resident(&kid) && rig.resident(&parent));
    rig.stop();
}

/// A dropped `accept_async` reply (a peer gone) never wedges the queue: its turn completes
/// unobserved and later admissions complete.
#[test]
fn a_dropped_accept_async_reply_lets_later_accepts_complete() {
    let rig = Rig::start();
    let dropped = rig.spend_coinbase(1, 2_000);
    let later = rig.spend_coinbase(2, 3_000);
    drop(rig.handle.accept_async(dropped.clone(), None));
    let result = rig.handle.accept(later.clone(), false).expect("V replies");
    assert_valid(&result, 3_000);
    assert!(rig.resident(&dropped), "the dropped turn completed unobserved");
    assert_eq!(rig.seen().len(), 2);
    rig.stop();
}

/// `test_accept` returns before `Finalize` (`validation.cpp:1446-1451`): the result is a valid
/// one with the fee, and the pool, its sequence and the queue are unchanged.
#[test]
fn test_accept_leaves_the_pool_and_events_unchanged() {
    let rig = Rig::start();
    let resident = rig.spend_coinbase(1, 2_000);
    rig.handle.accept(resident.clone(), false).expect("V replies");
    let _ = rig.seen();
    let snapshot = |pool: &Mempool| {
        let state = pool.read();
        (state.size(), state.dynamic_memory_usage(), state.sequence(), state.transactions_updated())
    };
    let before = snapshot(&rig.pool);
    let probe = rig.spend_coinbase(2, 4_000);
    let result = rig.handle.test_accept(probe.clone(), false).expect("V replies");
    assert_valid(&result, 4_000);
    let missing = child(&probe, 1_000);
    let result = rig.handle.test_accept(missing, false).expect("V replies");
    assert_eq!(result.validation_result, TxValidationResult::MissingInputs);
    assert_eq!(snapshot(&rig.pool), before);
    assert!(!rig.resident(&probe));
    assert!(rig.seen().is_empty());
    rig.stop();
}

/// `ProcessNewPackage` (`validation.cpp:1763-1790` at v24.0.2) through the handle: an empty
/// package is a `PCKG_POLICY` refusal that is never queued, so it reaches neither V nor Core's
/// `assert(!package.empty())`, even unattached or for a no-wait caller; a test accept of a parent
/// and its child leaves the pool and the events unchanged; the submission admits both, signalled
/// in package order; a caller marked no-wait (LO-9) is refused the wait; an unattached pool
/// replies `Disconnected`.
#[test]
fn a_package_through_the_handle() {
    let empty = PackageMempoolAcceptResult {
        unavailable: None,
        state: PackageValidationState {
            result: PackageValidationResult::Policy,
            reason: "package-empty".to_owned(),
            debug_message: String::new(),
        },
        tx_results: BTreeMap::new(),
    };
    let rig = Rig::start();
    for test_accept in [true, false] {
        assert_eq!(rig.handle.process_new_package(vec![], test_accept), Ok(empty.clone()));
    }
    let parent = rig.spend_coinbase(1, 2_000);
    let kid = child(&parent, 10_000);
    let package = vec![parent.clone(), kid.clone()];
    let tested = rig.handle.process_new_package(package.clone(), true).expect("V replies");
    assert_eq!(tested.state, PackageValidationState::default());
    assert_valid(&tested.tx_results[&parent.txid()], 2_000);
    assert_valid(&tested.tx_results[&kid.txid()], 4_000);
    assert!(!rig.resident(&parent) && rig.seen().is_empty());

    let (at, sequence) = (rig.v.clock.mocktime(), rig.pool.read().sequence());
    let submitted = rig.handle.process_new_package(package.clone(), false).expect("V replies");
    assert_eq!(submitted.state.result, PackageValidationResult::Unset);
    assert!(rig.resident(&parent) && rig.resident(&kid));
    assert_eq!(rig.seen(), [added(&parent, at, sequence), added(&kid, at, sequence + 1)]);

    {
        let _marked = NoWaitGuard::set();
        assert_eq!(
            rig.handle.process_new_package(package.clone(), true),
            Err(WaitError::WaitOnVFromNoWaitTask)
        );
        assert_eq!(rig.handle.process_new_package(vec![], true), Ok(empty.clone()));
    }
    let pool = rig.stop();
    let handle: Arc<dyn MempoolHandle> = pool;
    assert_eq!(handle.process_new_package(package.clone(), true), Err(WaitError::Disconnected));
    let unattached: Arc<dyn MempoolHandle> = Arc::new(Mempool::default());
    assert_eq!(unattached.process_new_package(package, false), Err(WaitError::Disconnected));
    assert_eq!(unattached.process_new_package(vec![], false), Ok(empty));
}

/// `PrioritiseTransaction` (`txmempool.cpp:1878-1903`) joins the admission order: a delta for an
/// absent txid is kept and applied at admission (`ApplyDelta`, `:1905-1912`); one queued behind an
/// admission finds the entry and moves its descendants' ancestor fees; deltas saturate
/// (`SaturatingAdd`, `:1882`).
#[test]
fn prioritise_applies_in_arrival_order_and_saturates() {
    let rig = Rig::start();
    let parent = rig.spend_coinbase(1, 2_000);
    rig.handle.prioritise(&parent.txid(), 500);
    let pending = rig.handle.accept_async(parent.clone(), None);
    rig.handle.prioritise(&parent.txid(), 100);
    assert_valid(&wait(pending), 2_000);
    let fee = |tx: &Tx| rig.pool.read().get_iter(&tx.txid()).expect("resident").modified_fee();
    assert_eq!(fee(&parent), 2_600);

    let kid = child(&parent, 10_000);
    rig.handle.accept(kid.clone(), false).expect("V replies");
    rig.handle.prioritise(&parent.txid(), -1_000);
    let state = rig.pool.read();
    let entry = state.get_iter(&kid.txid()).expect("resident");
    assert_eq!(entry.ancestors().modified_fees, 1_600 + 4_000);
    drop(state);

    let absent = TxHash::from([0x99; 32]);
    rig.handle.prioritise(&absent, i64::MAX);
    rig.handle.prioritise(&absent, i64::MAX);
    rig.handle.prioritise(&parent.txid(), i64::MIN);
    rig.handle.prioritise(&parent.txid(), i64::MIN);
    let state = rig.pool.read();
    assert_eq!(state.deltas().get(&absent), Some(&i64::MAX));
    assert_eq!(state.deltas().get(&parent.txid()), Some(&i64::MIN));
    assert_eq!(state.get_iter(&parent.txid()).expect("resident").modified_fee(), i64::MIN);
    drop(state);
    rig.stop();
}

/// `ValidateDSTX`'s fee delta, `0.1*COIN`.
// Core: net_processing.cpp:3716 at v24.0.2
const DSTX_DELTA: i64 = 10_000_000;

/// V held in a class-M turn until the returned sender is dropped or sent to.
fn hold_v(rig: &Rig) -> mpsc::Sender<()> {
    let (release, held) = mpsc::channel::<()>();
    let (entered, inside) = mpsc::channel();
    rig.v
        .v
        .send(ChainCommand::Mempool(Box::new(move |_| {
            entered.send(()).expect("the test waits");
            let _ = held.recv();
            Ok(MempoolCompletion {
                uncache: vec![],
                reply: Box::new(|| {}),
            })
        })))
        .expect("V runs");
    inside.recv().expect("V runs the hold");
    release
}

/// `ValidateDSTX` before `ProcessTransaction` (`net_processing.cpp:3716`, `:4959`): a delta
/// posted ahead of a zero-fee transaction's admission is applied first, so the admission's
/// `ApplyDelta` (`validation.cpp:1040` at v24.0.2) clears the min relay fee, with modified fee
/// base + delta. The post returns while the worker still waits on V for the admission ahead of
/// it, and its delta lands only in its FIFO turn.
#[test]
fn a_posted_delta_lands_before_the_admission_queued_after_it_without_waiting() {
    let rig = Rig::start();
    let ahead = rig.spend_coinbase(1, 2_000);
    let zero = rig.spend_coinbase(2, 0);
    let release = hold_v(&rig);
    let first = rig.handle.accept_async(ahead.clone(), None);
    let (posted, returned) = mpsc::channel();
    let handle = Arc::clone(&rig.handle);
    let txid = zero.txid();
    std::thread::spawn(move || {
        handle.post_prioritise(&txid, DSTX_DELTA);
        let _ = posted.send(());
    });
    returned.recv_timeout(Duration::from_secs(60)).expect("the post returns with V held");
    let second = rig.handle.accept_async(zero.clone(), None);
    assert_eq!(rig.pool.read().deltas().get(&zero.txid()), None, "queued behind the admission");
    drop(release);
    assert_valid(&wait(first), 2_000);
    assert_valid(&wait(second), 0);
    let state = rig.pool.read();
    assert_eq!(state.deltas().get(&zero.txid()), Some(&DSTX_DELTA));
    assert_eq!(state.get_iter(&zero.txid()).expect("resident").modified_fee(), DSTX_DELTA);
    drop(state);
    rig.stop();
}

/// A refused admission leaves the posted delta in `mapDeltas` (`PrioritiseTransaction`,
/// `txmempool.cpp:1878-1903`; only `ClearPrioritisation`, `:1915-1919`, erases it), so a later
/// post adds to it and the next admission applies the sum.
#[test]
fn a_refused_admission_keeps_its_posted_delta_for_a_later_accept() {
    let rig = Rig::start();
    let zero = rig.spend_coinbase(1, 0);
    rig.handle.post_prioritise(&zero.txid(), 10);
    let refused = rig.handle.accept(zero.clone(), false).expect("V replies");
    assert_eq!(refused.validation_result, TxValidationResult::MempoolPolicy, "{refused:?}");
    assert_eq!(refused.reason, "min relay fee not met", "{refused:?}");
    assert_eq!(rig.pool.read().deltas().get(&zero.txid()), Some(&10));
    rig.handle.post_prioritise(&zero.txid(), DSTX_DELTA);
    assert_valid(&rig.handle.accept(zero.clone(), false).expect("V replies"), 0);
    let fee = rig.pool.read().get_iter(&zero.txid()).expect("resident").modified_fee();
    assert_eq!(fee, 10 + DSTX_DELTA);
    rig.stop();
}

/// With no admission worker, before `attach_chain` or after `detach_chain`, a post applies at
/// once, as `prioritise` does.
#[test]
fn a_post_without_an_admission_worker_applies_at_once() {
    let unattached = Mempool::default();
    let txid = TxHash::from([0x42; 32]);
    unattached.post_prioritise(&txid, DSTX_DELTA);
    assert_eq!(unattached.read().deltas().get(&txid), Some(&DSTX_DELTA));

    let rig = Rig::start();
    let pool = rig.stop();
    assert_eq!(pool.detach_chain(), Ok(()));
    pool.post_prioritise(&txid, 7);
    assert_eq!(pool.read().deltas().get(&txid), Some(&7));
}

/// Shutdown: once V has stopped every admission replies `Disconnected`, as does one on a pool
/// whose worker was never attached; a prioritisation needs no V and still applies.
#[test]
fn shutdown_and_an_unattached_pool_give_disconnected() {
    let rig = Rig::start();
    let tx = rig.spend_coinbase(1, 2_000);
    let pool = rig.stop();
    let handle: Arc<dyn MempoolHandle> = pool.clone();
    assert_eq!(handle.accept(tx.clone(), false), Err(WaitError::Disconnected));
    assert_eq!(handle.accept_async(tx.clone(), None).recv(), Err(WaitError::Disconnected));
    assert_eq!(handle.test_accept(tx.clone(), false), Err(WaitError::Disconnected));
    handle.prioritise(&tx.txid(), 5);
    assert_eq!(pool.read().deltas().get(&tx.txid()), Some(&5));

    let unattached: Arc<dyn MempoolHandle> = Arc::new(Mempool::default());
    assert_eq!(unattached.accept(tx.clone(), false), Err(WaitError::Disconnected));
    unattached.prioritise(&tx.txid(), 5);
}

/// A second worker is refused: one FIFO per pool.
#[test]
fn a_pool_attaches_one_admission_worker() {
    let rig = Rig::start();
    assert!(rig.pool.attach_chain(rig.v.v.clone()).is_err());
    let tx = rig.spend_coinbase(1, 2_000);
    assert_valid(&rig.handle.accept(tx, false).expect("V replies"), 2_000);
    rig.stop();
}

/// Once V stops, `detach_chain` closes the FIFO and joins the worker, which drops its chain
/// handle: later admissions reply `Disconnected`, prioritisation applies at once, and the pool
/// never attaches again. A pool never attached has nothing to detach.
// Core: init.cpp:355-356, :460 at v24.0.0
#[test]
fn detach_joins_the_worker_once_v_has_stopped() {
    let rig = Rig::start();
    let tx = rig.spend_coinbase(1, 2_000);
    let chain = rig.v.v.clone();
    let pool = rig.stop();
    assert_eq!(pool.detach_chain(), Ok(()));
    assert_eq!(pool.accept(tx.clone(), false), Err(WaitError::Disconnected));
    pool.prioritise(&tx.txid(), 5);
    assert_eq!(pool.read().deltas().get(&tx.txid()), Some(&5));
    assert!(pool.attach_chain(chain).is_err(), "detached for good");
    assert_eq!(pool.detach_chain(), Ok(()));
    assert_eq!(Mempool::default().detach_chain(), Ok(()));
}

/// A worker panic is not only a `Disconnected` reply: `detach_chain` returns it, with the
/// admissions queued behind it disconnected.
#[cfg(feature = "test-support")]
#[test]
fn a_worker_panic_surfaces_at_detach() {
    let rig = Rig::start();
    let tx = rig.spend_coinbase(1, 2_000);
    rig.pool.panic_admission_worker();
    assert_eq!(rig.handle.accept(tx, false), Err(WaitError::Disconnected));
    let pool = rig.stop();
    assert_eq!(
        pool.detach_chain(),
        Err("the mempool admission worker panicked: injected mempool admission worker panic"
            .to_owned())
    );
}
