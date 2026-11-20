//! The status tests (SB5 §10.1): the cell, its coalescing and Core's dedups, the `arm` race and
//! a dropped reader's waker. `status_publish_order` and the facts `after_publish` derives run on
//! `HeaderChain`, in `feed::chain_tests`.

use std::sync::atomic::AtomicUsize;
use std::task::{Wake, Waker};
use std::thread;
use std::time::Duration;

use super::*;
use crate::feed::Next;
use crate::feed::test_support::{Model, block, cfg, shared};
use crate::notify::Parker;

fn best(k: u32) -> StatusFacts {
    StatusFacts {
        best_header: Some((block(k), 1_700_000_000 + k)),
        cl: None,
    }
}

fn with_cl(mut f: StatusFacts, k: u32, active: bool) -> StatusFacts {
    f.cl = Some(ClFact {
        block: block(k),
        active,
    });
    f
}

fn accepted(k: u32) -> StatusEvent {
    StatusEvent::HeaderAccepted {
        best_header: block(k),
        time: 1_700_000_000 + k,
    }
}

fn locked(k: u32) -> StatusEvent {
    StatusEvent::ChainLocked {
        block: block(k),
        lock: ChainLockRef {
            height: block(k).height,
            hash: block(k).hash,
        },
    }
}

/// Status facts never enter the F1 stream: a live feed sees nothing, the status reader sees the
/// change.
#[test]
fn status_events_not_in_f1() {
    let m = Model::new(cfg(4));
    let (_, mut feed, _) = m.subscribe();
    let mut r = StatusReader::new(m.shared.clone(), StatusFacts::default);
    assert!(m.shared.status.publish(with_cl(best(3), 2, true)));
    assert!(matches!(feed.next(), Next::Empty));
    assert_eq!(r.next_events().as_slice(), [accepted(3), locked(2)]);
}

/// A reader behind by several publications gets the latest facts once; unchanged facts bump
/// nothing.
#[test]
fn status_coalesces() {
    let sh = shared(4);
    let mut r = StatusReader::new(sh.clone(), StatusFacts::default);
    assert!(r.next_events().is_empty(), "a new reader starts at the current facts");
    for k in 1..=3 {
        assert!(sh.status.publish(best(k)));
    }
    assert!(!sh.status.publish(best(3)), "unchanged facts are not published");
    assert_eq!(r.next_events().as_slice(), [accepted(3)]);
    assert!(r.next_events().is_empty());

    // A new reader starts at the facts it was created on.
    let mut late = StatusReader::new(sh.clone(), StatusFacts::default);
    assert!(late.next_events().is_empty());
    sh.status.publish(best(4));
    assert_eq!(late.next_events().as_slice(), [accepted(4)]);
}

/// Core's `NotifyChainLock` conditions: the floor is on the active chain, and it is new to this
/// reader (`lastNotifyChainLockBlockIndex`).
#[test]
fn chainlocked_only_when_active() {
    let sh = shared(4);
    let mut r = StatusReader::new(sh.clone(), StatusFacts::default);
    sh.status.publish(with_cl(best(5), 4, false));
    assert_eq!(r.next_events().as_slice(), [accepted(5)], "an inactive floor is not reported");
    sh.status.publish(with_cl(best(5), 4, true));
    assert_eq!(r.next_events().as_slice(), [locked(4)]);
    // The same floor leaves the active chain and comes back: reported once, whether or not the
    // reader looked in between.
    sh.status.publish(with_cl(best(6), 4, false));
    sh.status.publish(with_cl(best(6), 4, true));
    assert_eq!(r.next_events().as_slice(), [accepted(6)]);
    sh.status.publish(with_cl(best(6), 4, false));
    assert!(r.next_events().is_empty());
    sh.status.publish(with_cl(best(6), 4, true));
    assert!(r.next_events().is_empty());
    // A newer floor, active at once.
    sh.status.publish(with_cl(best(6), 5, true));
    assert_eq!(r.next_events().as_slice(), [locked(5)]);
    // A reader created on an inactive floor reports it when it becomes active.
    sh.status.publish(with_cl(best(7), 6, false));
    let mut late = StatusReader::new(sh.clone(), StatusFacts::default);
    sh.status.publish(with_cl(best(7), 6, true));
    assert_eq!(late.next_events().as_slice(), [locked(6)]);
}

/// R1-m1's race, directed: the writer publishes between `arm`'s check and its registration.
/// `arm`'s re-load sees the bump, so it returns true and the reader never parks on it.
#[test]
fn status_arm_race() {
    let sh = shared(4);
    let mut r = StatusReader::new(sh.clone(), StatusFacts::default);
    let p = Parker::new();
    sh.status.faults.in_arm.arm();
    let reader = thread::spawn(move || {
        let changed = r.arm(Notify::Thread(p.clone()));
        (changed, r.next_events())
    });
    sh.status.faults.in_arm.wait_paused();
    assert!(sh.status.publish(best(1)), "the writer finds no armed waker");
    sh.status.faults.in_arm.release();
    let (changed, events) = reader.join().expect("the reader");
    assert!(changed, "the recheck sees the bump");
    assert_eq!(events.as_slice(), [accepted(1)]);
}

/// The armed waker fires on the next change, and `arm` then reports it.
#[test]
fn status_arm_wakes() {
    let sh = shared(4);
    let mut r = StatusReader::new(sh.clone(), StatusFacts::default);
    let p = Parker::new();
    assert!(!r.arm(Notify::Thread(p.clone())));
    assert!(sh.status.publish(best(1)));
    assert!(p.park_timeout(Duration::ZERO), "the publish woke the reader");
    assert!(r.arm(Notify::Thread(p.clone())));
    assert_eq!(r.next_events().as_slice(), [accepted(1)]);
    assert!(!r.arm(Notify::Thread(p)), "nothing new after the report");
}

/// A dropped reader leaves the list and no waker behind, even while a publish still holds the
/// old list: that publish finds no waker to clone.
#[test]
fn status_reader_drop_removes_waker() {
    let sh = shared(4);
    let mut r = StatusReader::new(sh.clone(), StatusFacts::default);
    let keep = StatusReader::new(sh.clone(), StatusFacts::default);
    let p = Parker::new();
    assert!(!r.arm(Notify::Thread(p.clone())));
    assert_eq!(sh.status.readers.load().len(), 2);
    let in_flight = sh.status.readers.load();
    drop(r);
    assert_eq!(sh.status.readers.load().len(), 1);
    assert_eq!(Arc::strong_count(&p), 1, "the slot's waker is gone");
    drop(in_flight);
    sh.status.publish(best(1));
    assert!(!p.park_timeout(Duration::ZERO), "no wake reaches a dropped reader");
    drop(keep);
    assert_eq!(sh.status.readers.load().len(), 0);
}

struct CountWake(AtomicUsize);

impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// `poll_changed` arms the task's waker, wakes it on a change, and stays `Ready` until
/// `next_events` reports the change.
#[test]
fn status_poll_changed() {
    let sh = shared(4);
    let mut r = StatusReader::new(sh.clone(), StatusFacts::default);
    let count = Arc::new(CountWake(AtomicUsize::new(0)));
    let waker = Waker::from(count.clone());
    let mut cx = Context::from_waker(&waker);
    assert_eq!(r.poll_changed(&mut cx), Poll::Pending);
    sh.status.publish(best(1));
    assert_eq!(count.0.load(Ordering::SeqCst), 1);
    assert_eq!(r.poll_changed(&mut cx), Poll::Ready(()));
    assert_eq!(r.poll_changed(&mut cx), Poll::Ready(()), "level-triggered until reported");
    assert_eq!(r.next_events().as_slice(), [accepted(1)]);
    assert_eq!(r.poll_changed(&mut cx), Poll::Pending);
}

/// Many readers and a writer: every reader's last report is the writer's last facts.
#[test]
fn status_stress_readers_converge() {
    const STEPS: u32 = 2_000;
    let sh = shared(4);
    let readers: Vec<_> = (0..8)
        .map(|_| {
            let mut r = StatusReader::new(sh.clone(), StatusFacts::default);
            thread::spawn(move || {
                let p = Parker::new();
                let mut last = 0;
                while last != STEPS {
                    if !r.arm(Notify::Thread(p.clone())) {
                        assert!(p.park_timeout(Duration::from_secs(30)), "a lost status wake");
                        continue;
                    }
                    for e in r.next_events() {
                        let StatusEvent::HeaderAccepted {
                            best_header,
                            ..
                        } = e
                        else {
                            panic!("no locks published");
                        };
                        assert!(last < best_header.height, "reports only move forward");
                        last = best_header.height;
                    }
                }
            })
        })
        .collect();
    for k in 1..=STEPS {
        sh.status.publish(best(k));
    }
    for r in readers {
        r.join().expect("a reader converges");
    }
}
