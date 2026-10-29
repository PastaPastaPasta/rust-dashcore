//! Loom L8 (SB5 §10.2): the status writer's publish ‖ a reader's `arm`, over the production
//! cell. A reader that armed and saw no change is notified. L8b: two publishes ‖ the reader's
//! `arm`/`next_events` loop, which reports the last facts (the load order in `next_events`).
//! Run with `RUSTFLAGS="--cfg loom" cargo test -p dash-chain --release --lib loom_`.

use loom::thread;

use super::*;
use crate::feed::loom_tests::{model, run_deadlock_negatives};
use crate::feed::test_support::{block, cfg, latch};
use crate::notify::Parker;

const DEADLOCK_NEGATIVES: &[&str] = &[
    "status_feed::loom_tests::loom_l8_neg_no_recheck",
    "status_feed::loom_tests::loom_l8b_neg_facts_first",
];

#[test]
fn loom_status_negatives_fail() {
    run_deadlock_negatives(DEADLOCK_NEGATIVES);
}

fn l8(arm_without_recheck: bool) {
    model(move || {
        let mut sh = FeedShared::new(cfg(1), latch());
        sh.status.faults.arm_without_recheck = arm_without_recheck;
        let sh = Arc::new(sh);
        let mut r = StatusReader::new(sh.clone(), StatusFacts::default);
        let w = {
            let sh = sh.clone();
            thread::spawn(move || {
                sh.status.publish(StatusFacts {
                    best_header: Some((block(1), 1)),
                    cl: None,
                });
            })
        };
        let p = Parker::new();
        while !r.arm(Notify::Thread(p.clone())) {
            p.park();
        }
        assert_eq!(
            r.next_events().as_slice(),
            [StatusEvent::HeaderAccepted {
                best_header: block(1),
                time: 1,
            }]
        );
        w.join().expect("the writer");
    });
}

#[test]
fn loom_l8_status_arm() {
    l8(false);
}

/// L8's negative control: `arm` without the re-load parks past the bump (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_status_negatives_fail"]
fn loom_l8_neg_no_recheck() {
    l8(true);
}

fn facts(k: u32) -> StatusFacts {
    StatusFacts {
        best_header: Some((block(k), k)),
        cl: None,
    }
}

fn l8b(facts_before_version: bool) {
    model(move || {
        let mut sh = FeedShared::new(cfg(1), latch());
        sh.status.faults.facts_before_version = facts_before_version;
        let sh = Arc::new(sh);
        let mut r = StatusReader::new(sh.clone(), StatusFacts::default);
        let w = {
            let sh = sh.clone();
            thread::spawn(move || {
                sh.status.publish(facts(1));
                sh.status.publish(facts(2));
            })
        };
        let p = Parker::new();
        let mut last = 0;
        while last != 2 {
            if !r.arm(Notify::Thread(p.clone())) {
                p.park();
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
                last = best_header.height;
            }
        }
        w.join().expect("the writer");
    });
}

#[test]
fn loom_l8b_status_latest() {
    l8b(false);
}

/// L8b's negative control: `next_events` loads the facts before `version`, so a publish between
/// the loads is marked seen but not reported, and the reader parks past it (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_status_negatives_fail"]
fn loom_l8b_neg_facts_first() {
    l8b(true);
}
