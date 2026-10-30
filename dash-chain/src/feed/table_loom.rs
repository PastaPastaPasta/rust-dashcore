//! Loom models L3, L4 and L7 (SB5 §10.2) over the production table, `retry_pending`,
//! `subscribe_at` and `deregister`, on the model tip lock (a user-level mutex: loom 0.7's own
//! `try_lock` reports false deadlocks). Each has its planted negative control. Run with
//! `RUSTFLAGS="--cfg loom" cargo test -p dash-chain --release --lib loom_`.

use std::sync::Arc;

use loom::thread;

use super::loom_tests::{drain, model, model_with_wake, run_deadlock_negatives, take_event};
use super::test_support::{Model, block, cfg, ok_finish};
use super::*;
use crate::notify::Parker;

const DEADLOCK_NEGATIVES: &[&str] = &[
    "feed::table_loom::loom_l3_neg_retry_ignores_closing",
    "feed::table_loom::loom_l3_neg_blocking_step4",
    "feed::table_loom::loom_l7_neg_no_recheck",
];

#[test]
fn loom_table_negatives_fail() {
    run_deadlock_negatives(DEADLOCK_NEGATIVES);
}

// ---- L3: deregistration against the writer -----------------------------------------------

/// L3 (a): S is full with a cursor; `deregister(S)` alone releases the waiting writer, and no
/// push reaches S after it.
fn l3_dereg_releases_writer(retry_ignores_closing: bool) {
    model(move || {
        let (m, p) = model_with_wake(2);
        let (s, mut feed, _) = m.subscribe();
        {
            let mut g = m.tip.lock();
            g.feeds.faults.retry_ignores_closing = retry_ignores_closing;
            g.connect(&m.shared, block(1));
            assert_eq!(g.connect(&m.shared, block(2)).1, Publish::Blocked);
        }
        let d = {
            let m = m.clone();
            thread::spawn(move || m.handle.deregister(s))
        };
        drain(&m, &p);
        d.join().expect("deregister returns");
        let reg = m.shared.registry.load();
        assert!(reg.feeds.iter().all(|e| e.id != s), "S is reclaimed");
        assert_eq!(m.tip.lock().connect(&m.shared, block(3)).1, Publish::Done);
        m.tip.lock().feeds.assert_consistent();
        let mut got = Vec::new();
        while let Next::Event(_) = feed.next() {
            got.push(feed.pop().expect("selected").seq());
        }
        assert_eq!(got, [1], "no push reaches S after deregister");
    });
}

#[test]
fn loom_l3_dereg_releases_writer() {
    l3_dereg_releases_writer(false);
}

/// L3's negative control: `retry_pending` keeps a closing target's cursor (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_table_negatives_fail"]
fn loom_l3_neg_retry_ignores_closing() {
    l3_dereg_releases_writer(true);
}

/// L3 (b): `register` reusing the index ‖ A's step 4. A's cleanup never removes B, and the
/// entry is reclaimed exactly once.
fn l3_reuse(cleanup_without_ptr_eq: bool) {
    model(move || {
        let m = Model::new(cfg(1).with_max_feeds(1));
        m.tip.lock().feeds.faults.cleanup_without_ptr_eq = cleanup_without_ptr_eq;
        let a = m.handle.register(Needs::default()).expect("registers");
        let d = {
            let m = m.clone();
            thread::spawn(move || m.handle.deregister(a))
        };
        let b = m.handle.register(Needs::default());
        d.join().expect("deregister returns");
        let reg = m.shared.registry.load();
        match b {
            Ok(b) => {
                assert!(reg.feeds.iter().any(|e| e.id == b), "A's cleanup never removes B")
            }
            Err(r) => assert_eq!(r, Refused::TableFull, "A was not closing yet"),
        }
        m.tip.lock().feeds.assert_consistent();
    });
}

#[test]
fn loom_l3_reuse_keeps_b() {
    l3_reuse(false);
}

/// L3's negative control: step 4 without `ptr_eq` removes B.
#[test]
#[should_panic(expected = "A's cleanup never removes B")]
fn loom_l3_neg_cleanup_without_ptr_eq() {
    l3_reuse(true);
}

/// L3 (c), rev3: step 4 contended by a hold (Opus high M1 case 1). V holds the tip lock and
/// drains with S1 and S2 full; one events thread deregisters S1, then pops S2. Step 4 loses the
/// `try_lock`, V's scan reclaims S1 exactly once, and the hold completes.
fn l3_hold(blocking_step4: bool) {
    model(move || {
        let (m, p) = model_with_wake(2);
        let (s1, f1, _) = m.subscribe();
        let (_, mut f2, _) = m.subscribe();
        let v = {
            let (m, p) = (m.clone(), p.clone());
            thread::spawn(move || {
                let mut g = m.tip.lock();
                m.shared.hold_opened();
                g.connect(&m.shared, block(1));
                g.connect(&m.shared, block(2));
                // The per-step drain through the held guard (§4.7).
                while g.retry(&m.shared) == Publish::Blocked {
                    p.park();
                }
                m.shared.hold_closing();
                drop(g);
                m.shared.fire_release_wakes();
            })
        };
        if blocking_step4 {
            let slot =
                m.shared.registry.load().feeds.iter().find(|e| e.id == s1).map(|e| e.slot.clone());
            let slot = slot.expect("registered");
            slot.close(&m.shared);
            m.tip.lock().feeds.remove(&m.shared, s1, &slot);
        } else {
            m.handle.deregister(s1);
        }
        let me = Parker::new();
        assert_eq!(
            [
                take_event(&mut *f2, &me, &mut Vec::new()),
                take_event(&mut *f2, &me, &mut Vec::new())
            ],
            [1, 2]
        );
        v.join().expect("the hold completes");
        assert!(m.shared.registry.load().feeds.iter().all(|e| e.id != s1));
        m.tip.lock().feeds.assert_consistent();
        drop(f1);
    });
}

#[test]
fn loom_l3_contended_step4() {
    l3_hold(false);
}

/// L3's negative control: a blocking step 4 waits on the hold, which waits on S2's pop that the
/// same thread was to do next (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_table_negatives_fail"]
fn loom_l3_neg_blocking_step4() {
    l3_hold(true);
}

// ---- L4: the handoff against a pending event ------------------------------------------------

/// L4: the writer takes two steps, the second pending for S1 (Q = 1) ‖ `subscribe_at(S2)`. S2
/// receives exactly the events with `seq ≥ seq_synced`, once each (X28).
fn l4(cursor_at_subscribe: bool) {
    model(move || {
        let (m, p) = model_with_wake(2);
        m.tip.lock().feeds.faults.cursor_at_subscribe = cursor_at_subscribe;
        let (_, mut f1, _) = m.subscribe();
        let w = {
            let (m, p) = (m.clone(), p.clone());
            thread::spawn(move || {
                for k in 1..=2 {
                    drain(&m, &p);
                    m.tip.lock().connect(&m.shared, block(k));
                }
                drain(&m, &p);
            })
        };
        let s1 = thread::spawn(move || {
            let me = Parker::new();
            assert_eq!(
                [
                    take_event(&mut *f1, &me, &mut Vec::new()),
                    take_event(&mut *f1, &me, &mut Vec::new())
                ],
                [1, 2]
            );
            f1
        });
        let id = m.handle.register(Needs::default()).expect("registers");
        let (mut f2, synced) = loop {
            let tip = m.tip.lock().ctx.tip;
            match m.handle.subscribe_at(id, &tip, &mut ok_finish()) {
                Ok(Handoff::Live {
                    feed,
                    seq_synced,
                }) => break (feed, seq_synced),
                Ok(Handoff::Moved) => thread::yield_now(),
                Err(e) => panic!("subscribe_at: {e:?}"),
            }
        };
        let me = Parker::new();
        for want in synced..=2 {
            assert_eq!(
                take_event(&mut *f2, &me, &mut Vec::new()),
                want,
                "X28: from the cut, in order"
            );
        }
        w.join().expect("the writer finishes");
        let f1 = s1.join().expect("S1 takes both");
        assert!(matches!(f2.next(), Next::Empty), "X28: nothing before the cut");
        drop((f1, f2));
    });
}

#[test]
fn loom_l4_handoff_with_pending() {
    l4(false);
}

/// L4's negative control: rev2's handoff gives the new slot a cursor into the backlog.
#[test]
#[should_panic(expected = "X28")]
fn loom_l4_neg_cursor_at_subscribe() {
    l4(true);
}

// ---- L7: W's barriers against `deregister` --------------------------------------------------

/// L7: `deregister(S)` ‖ a model W with two barriers, each capturing the registry's non-closing
/// feeds and waiting until each has closed (no subscriber acks here). W sets its close hook once,
/// before its first capture, as the real W does: the `OnceLock` is not loom-visible, so arming it
/// mid-race would hide the hook's ordering from the model. The barrier captured before the
/// `closing` store ends by the predicate re-check after the hook's wake (H7 (a)); the one
/// captured after it ends at once by the capture filter (H7 (b)).
fn l7(w_parker: fn() -> Arc<Parker>) {
    model(move || {
        let m = Model::new(cfg(1).with_max_feeds(2));
        let (s, f, _) = m.subscribe();
        let wake = w_parker();
        m.shared.set_close_hook(Notify::Thread(wake.clone()));
        let d = {
            let m = m.clone();
            thread::spawn(move || m.handle.deregister(s))
        };
        for _ in 0..2 {
            let reg = m.shared.registry.load();
            let captured: Vec<_> =
                reg.feeds.iter().filter(|e| !e.is_closing()).map(|e| e.slot.clone()).collect();
            while !captured.iter().all(|c| c.is_closing()) {
                wake.park();
            }
        }
        d.join().expect("deregister returns");
        drop(f);
    });
}

#[test]
fn loom_l7_w_barriers() {
    l7(Parker::new);
}

/// L7's negative control: W's parker keeps no permit, so a close between W's predicate check and
/// its park is lost: the re-check after arming is missing (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_table_negatives_fail"]
fn loom_l7_neg_no_recheck() {
    l7(Parker::new_lossy);
}
