//! SB5 §4.7's worked case, threaded, on the real chain: V holds the tip mutex across three
//! disconnects at Q = 1 while S is parked on its own `FlushDue`, which only V's in-hold wait
//! answers. Variants: S closes while parked; `-dbcrashratio`, where W's completion after the
//! second step needs S's ack of it. Each wait has a timeout, so a deadlock fails the test.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError, mpsc};
use std::time::Instant;

use super::hold_tests::{T, WAIT, await_event, node, subscribe};
use crate::BlockId;
use crate::chain::tests::{branch, connect_all};
use crate::feed::{ChainEvent, Needs, Publish, Refused};
use crate::notify::{Notify, Parker};

/// V's in-hold service (§4.7): one parker for every wake source, and S's `FlushDue` requests,
/// answered on V's next wake. Class R and M commands never reach it (CH1-node's queue).
struct Service {
    parker: Arc<Parker>,
    flush_due: Mutex<Vec<mpsc::Sender<()>>>,
    /// W's completion of the freeze after the second step.
    completed: AtomicBool,
    /// A model hub lock V takes inside the hold to answer `FlushDue` (CH2b's, Opus high case 3).
    hub: Mutex<()>,
}

impl Service {
    fn new() -> Service {
        Service {
            parker: Parker::new(),
            flush_due: Mutex::new(Vec::new()),
            completed: AtomicBool::new(false),
            hub: Mutex::new(()),
        }
    }

    /// S's request: V answers it on its next wake.
    fn flush_due(&self) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel();
        self.flush_due.lock().expect("lock").push(tx);
        self.parker.notify();
        rx
    }

    /// `in_hold_service_wait()`: park, then answer every `FlushDue` under the hub lock.
    fn wait(&self) {
        assert!(self.parker.park_timeout(WAIT), "V's in-hold wait was never woken");
        let _hub = self.hub();
        for tx in self.flush_due.lock().expect("lock").drain(..) {
            let _ = tx.send(());
        }
    }

    /// The hub lock, taken with a deadline: a hub call that blocked on the hold while holding
    /// it would otherwise hang the test instead of failing it.
    fn hub(&self) -> MutexGuard<'_, ()> {
        let start = Instant::now();
        loop {
            match self.hub.try_lock() {
                Ok(g) => return g,
                Err(TryLockError::WouldBlock) => {
                    assert!(start.elapsed() < WAIT, "the hub lock was never released");
                    std::thread::yield_now();
                }
                Err(TryLockError::Poisoned(_)) => panic!("the hub holder panicked"),
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    Pops,
    Closes,
    CrashRatio,
}

/// Runs the case; returns what S took.
fn hold_q1_three_disconnects(variant: Variant) -> Vec<ChainEvent> {
    let (mut chain, _) = node(1);
    let b = branch(&mut chain, BlockId(0), 1, 3, T);
    connect_all(&mut chain, &b);
    let svc = Service::new();
    chain.set_producer_wake(Notify::Thread(svc.parker.clone()));
    let feeds = chain.feeds();
    let (id, mut feed) = subscribe(&feeds);
    let reader = chain.reader();
    let pre = reader.snapshot();
    let (ack_tx, ack_rx) = mpsc::channel::<u64>();

    let taken = std::thread::scope(|s| {
        let svc = &svc;
        let s_thread = s.spawn(|| {
            let p = Parker::new();
            feed.set_waker(Notify::Thread(p.clone()));
            svc.flush_due().recv_timeout(WAIT).expect("V answers FlushDue inside the hold");
            if variant == Variant::Closes {
                feeds.deregister(id);
                return Vec::new();
            }
            let mut got = Vec::new();
            while got.len() < 3 {
                await_event(&mut *feed, &p, "S");
                if got.len() < 2 {
                    // Until d₂'s pop V cannot push d₃, so the hold is still open.
                    assert!(Arc::ptr_eq(&reader.snapshot(), &pre), "no release yet");
                }
                let e = feed.pop().expect("selected");
                if variant == Variant::CrashRatio {
                    // W stops listening once d₂ is acked.
                    let _ = ack_tx.send(e.seq());
                }
                got.push(e);
            }
            got
        });
        if variant == Variant::CrashRatio {
            let want = pre.tip_seq + 2;
            s.spawn(move || {
                while ack_rx.recv_timeout(WAIT).expect("S acks") != want {}
                svc.completed.store(true, Ordering::SeqCst);
                svc.parker.notify();
            });
        }

        // V: one hold, three steps, drained between each two through the held guard.
        let mut hold = chain.hold_tip();
        for k in 0..3 {
            assert!(hold.pending_is_empty(), "no step starts with pending non-empty");
            hold.disconnect_tip().expect("a step");
            if variant == Variant::CrashRatio && k == 1 {
                // The completion wait runs `retry_pending` on every wake (CH1-node §7.1).
                while !svc.completed.load(Ordering::SeqCst) {
                    hold.retry_pending();
                    svc.wait();
                }
                assert!(hold.pending_is_empty(), "S acked d₂, so it was pushed");
            }
            while hold.retry_pending() == Publish::Blocked {
                svc.wait();
            }
            assert!(Arc::ptr_eq(&reader.snapshot(), &pre), "readers keep the pre-hold snapshot");
        }
        let snap = hold.release();
        assert_eq!(snap.tip_seq, pre.tip_seq + 3, "the release publishes the last step's seq");
        assert!(Arc::ptr_eq(&reader.snapshot(), &snap), "readers see the release's snapshot");
        s_thread.join().expect("S")
    });
    if variant == Variant::Closes {
        assert!(feeds.registry().feeds.is_empty(), "S's entry is reclaimed");
    }
    if variant != Variant::Closes {
        let seqs: Vec<_> = taken.iter().map(|e| (e.seq(), e.block().id)).collect();
        let want: Vec<_> = (1..=3).map(|k| (pre.tip_seq + k, b[3 - k as usize])).collect();
        assert_eq!(seqs, want, "S gets d₁, d₂, d₃ in order");
    }
    taken
}

/// `hold_q1_two_disconnects_parked` (§4.7 steps 1–5, DEC-153) and `hold_snapshot_at_release`:
/// d₂ blocks on S's full queue while S waits on a `FlushDue` that only V's in-hold wait
/// answers; S then takes d₁ and V pushes d₂ through the held guard before d₃. Readers see the
/// pre-hold snapshot throughout, and the release's `tip_seq` is the last step's.
#[test]
fn hold_q1_two_disconnects_parked() {
    assert_eq!(hold_q1_three_disconnects(Variant::Pops).len(), 3);
}

/// §4.7 step 6: S deregisters while parked. `deregister` does not block on the held mutex,
/// `retry_pending` clears S's cursor, and the hold goes on to its third step.
#[test]
fn hold_q1_two_disconnects_closing() {
    assert!(hold_q1_three_disconnects(Variant::Closes).is_empty());
}

/// §4.7 step 7: `-dbcrashratio`. W's completion after d₂ needs S's ack of d₂; V's completion
/// wait runs `retry_pending` on each wake, so d₂ reaches S once S pops d₁.
#[test]
fn hold_q1_two_disconnects_crashratio() {
    assert_eq!(hold_q1_three_disconnects(Variant::CrashRatio).len(), 3);
}

// ---- DEC-157 (2), M1: nothing V waits on blocks on V's hold -----------------------------------

/// `hold_dereg_from_shared_events_thread` (Opus high case 1): S1 and S2 share one events thread;
/// V holds across two disconnects with both full. The thread deregisters S1, then pops S2, which
/// is what V waits for. A `deregister` that waited for the tip mutex would deadlock here.
#[test]
fn hold_dereg_from_shared_events_thread() {
    let (mut chain, _) = node(1);
    let b = branch(&mut chain, BlockId(0), 1, 2, T);
    connect_all(&mut chain, &b);
    let svc = Service::new();
    chain.set_producer_wake(Notify::Thread(svc.parker.clone()));
    let feeds = chain.feeds();
    let (s1, _f1) = subscribe(&feeds);
    let (_, mut f2) = subscribe(&feeds);
    let pre = chain.snapshot().tip_seq;
    let (stepped_tx, stepped_rx) = mpsc::channel();

    std::thread::scope(|s| {
        let (feeds, f2) = (&feeds, &mut f2);
        let events = s.spawn(move || {
            stepped_rx.recv_timeout(WAIT).expect("V stepped twice");
            feeds.deregister(s1);
            let p = Parker::new();
            f2.set_waker(Notify::Thread(p.clone()));
            let mut got = Vec::new();
            while got.len() < 2 {
                await_event(&mut **f2, &p, "S2");
                got.push(f2.pop().expect("selected").seq());
            }
            got
        });
        let mut hold = chain.hold_tip();
        hold.disconnect_tip().expect("d1");
        assert_eq!(hold.retry_pending(), Publish::Done);
        hold.disconnect_tip().expect("d2, pending for both");
        stepped_tx.send(()).expect("the events thread listens");
        while hold.retry_pending() == Publish::Blocked {
            svc.wait();
        }
        hold.release();
        assert_eq!(events.join().expect("events"), [pre + 1, pre + 2]);
    });
    assert_eq!(feeds.registry().feeds.len(), 1, "S1's entry is reclaimed, S2's stays");
}

/// `dereg_under_hub_lock_during_hold` (Opus high case 3): V's in-hold wait takes a hub lock to
/// answer `FlushDue`; a watchdog holding that lock deregisters the parked S and tries to
/// register a new feed. Both return at once (`register` with `HoldActive`), the watchdog lets
/// the lock go, and V proceeds. After the release the retry protocol registers.
#[test]
fn dereg_under_hub_lock_during_hold() {
    let (mut chain, _) = node(1);
    let b = branch(&mut chain, BlockId(0), 1, 2, T);
    connect_all(&mut chain, &b);
    let svc = Service::new();
    chain.set_producer_wake(Notify::Thread(svc.parker.clone()));
    let feeds = chain.feeds();
    let (id, _f) = subscribe(&feeds);
    let (stepped_tx, stepped_rx) = mpsc::channel();

    std::thread::scope(|s| {
        let (svc, feeds) = (&svc, &feeds);
        let watchdog = s.spawn(move || {
            stepped_rx.recv_timeout(WAIT).expect("V stepped twice");
            let hub = svc.hub();
            svc.parker.notify();
            feeds.deregister(id);
            let refused = feeds.register(Needs::default());
            let p = Parker::new();
            feeds.on_release(Notify::Thread(p.clone()));
            drop(hub);
            assert_eq!(refused, Err(Refused::HoldActive));
            loop {
                match feeds.register(Needs::default()) {
                    Ok(id) => return id,
                    Err(e) => assert_eq!(e, Refused::HoldActive),
                }
                assert!(p.park_timeout(WAIT), "the release wake");
            }
        });
        let mut hold = chain.hold_tip();
        hold.disconnect_tip().expect("d1");
        hold.disconnect_tip().expect("d2, pending for S");
        assert_eq!(hold.retry_pending(), Publish::Blocked);
        stepped_tx.send(()).expect("the watchdog listens");
        // Each retry follows a wait, which takes the hub lock: none runs while the watchdog
        // holds it, so its calls all land inside the hold.
        loop {
            svc.wait();
            if hold.retry_pending() == Publish::Done {
                break;
            }
        }
        hold.release();
        watchdog.join().expect("watchdog");
    });
    assert_eq!(feeds.registry().feeds.len(), 1, "S reclaimed, the new feed registered");
}
