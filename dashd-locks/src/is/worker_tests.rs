//! The worker's tick and sleep rule against Core v24.0.0
//! `instantsend/net_instantsend.cpp:37, :425-445`.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use dash_consensus::Tx;
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxType};
use dash_evo::locks::isdlock_hash;

use super::*;

use crate::is::s3_fixtures::{Event, FakeView, Passes, Rig, TestRng, lock};

/// An interrupt that records each sleep and interrupts at the `stop_at`-th.
struct Sleeps {
    slept: Mutex<Vec<Duration>>,
    stop_at: usize,
}

impl WorkInterrupt for Sleeps {
    fn interrupted(&self) -> bool {
        false
    }

    fn sleep_for(&self, d: Duration) -> bool {
        let mut slept = self.slept.lock().unwrap();
        slept.push(d);
        slept.len() < self.stop_at
    }
}

fn enqueue(rig: &Rig, n: u32) {
    for t in 0..n {
        let l = lock(t, 1);
        rig.net.manager().enqueue_instant_send_lock(1, isdlock_hash(&l), Arc::new(l), false);
    }
}

/// 33 queued locks: the first tick takes 32 and reports more work, so the loop does not sleep;
/// the second takes the last, and the loop sleeps 100 ms, where it is interrupted.
// Core: net_instantsend.cpp:37, :431-441; instantsend/instantsend.cpp:58-85
#[test]
fn the_worker_sleeps_100ms_only_after_a_tick_without_more_work() {
    let rig = Rig::new(false);
    enqueue(&rig, 33);
    let passes = Passes::one(FakeView::with_quorums(1));
    let sleeps = Sleeps {
        slept: Mutex::default(),
        stop_at: 1,
    };
    rig.net.work_thread_main(&passes, &sleeps, &mut TestRng(3)).expect("ran");
    assert_eq!(rig.processed().len(), 33);
    assert_eq!(passes.taken(), 2, "one pass per tick, no second pass");
    assert_eq!(*sleeps.slept.lock().unwrap(), [WORK_THREAD_SLEEP_INTERVAL]);
    assert_eq!(WORK_THREAD_SLEEP_INTERVAL, Duration::from_millis(100));
}

/// An idle tick sleeps each time, and takes no pass.
#[test]
fn an_idle_worker_sleeps_every_tick() {
    let rig = Rig::new(false);
    let passes = Passes::one(FakeView::with_quorums(1));
    let sleeps = Sleeps {
        slept: Mutex::default(),
        stop_at: 3,
    };
    rig.net.work_thread_main(&passes, &sleeps, &mut TestRng(3)).expect("ran");
    assert_eq!(sleeps.slept.lock().unwrap().len(), 3);
    assert_eq!(passes.taken(), 0);
}

/// With InstantSend off the tick does nothing and reports no work, leaving the queue.
// Core: net_instantsend.cpp:429
#[test]
fn a_disabled_tick_leaves_the_queue() {
    let rig = Rig::new(false);
    enqueue(&rig, 2);
    rig.spork2.store(false, Ordering::SeqCst);
    let passes = Passes::one(FakeView::with_quorums(1));
    assert!(!rig.net.worker_tick(&passes, &mut TestRng(3)).unwrap());
    assert_eq!(passes.taken(), 0);
    rig.spork2.store(true, Ordering::SeqCst);
    assert!(!rig.net.worker_tick(&passes, &mut TestRng(3)).unwrap());
    assert_eq!(rig.processed().len(), 2);
}

/// With a signer, the tick hands it the manager's retry transactions; an empty list is not
/// handed on, where Core's call does nothing.
// Core: net_instantsend.cpp:435-437; instantsend/signing.cpp:150-187
#[test]
fn a_signer_gets_the_retry_transactions() {
    let rig = Rig::new(true);
    let passes = Passes::one(FakeView::with_quorums(1));
    rig.net.worker_tick(&passes, &mut TestRng(3)).unwrap();
    assert!(rig.events().is_empty());

    let parent = TxHash::from([0x51; 32]);
    let child = Arc::new(Tx::new(Transaction::new(
        1,
        TxType::new(0),
        vec![TxIn::new(OutPoint::new(parent, 0), Default::default(), u32::MAX)],
        vec![],
        0,
        vec![],
    )));
    let manager = rig.net.manager();
    manager.add_non_locked_tx(&child, None);
    manager.remove_non_locked_tx(&parent, true, false);
    rig.net.worker_tick(&passes, &mut TestRng(3)).unwrap();
    assert_eq!(rig.events(), [Event::Retry(1)]);
}

/// `CThreadInterrupt`: a sleep returns true after its time, false once interrupted.
#[test]
fn thread_interrupt_sleeps_until_interrupted() {
    let interrupt = Arc::new(ThreadInterrupt::new());
    assert!(interrupt.sleep_for(Duration::from_millis(1)));
    let other = Arc::clone(&interrupt);
    let sleeper = std::thread::spawn(move || other.sleep_for(Duration::from_secs(600)));
    interrupt.interrupt();
    assert!(!sleeper.join().unwrap());
    assert!(interrupt.interrupted() && !interrupt.sleep_for(Duration::from_secs(600)));
    interrupt.reset();
    assert!(!interrupt.interrupted());
}
