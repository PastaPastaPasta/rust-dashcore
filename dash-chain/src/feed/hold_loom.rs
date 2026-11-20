//! Loom models L10–L13 (SB5 §10.2): holds against the subscribers they wait on, over the
//! production table, slots and release wakes, on the model tip lock. A hold is the model guard
//! taken once and kept across its steps, `hold_opened` after the lock and `hold_closing` before
//! the drop, then the release wakes, as `Hold::end` does. Run with
//! `RUSTFLAGS="--cfg loom" cargo test -p dash-chain --release --lib loom_`.

use std::sync::Arc;

use loom::thread;

use super::loom_tests::{
    drain, model, model_with_wake, run_deadlock_negatives, run_negatives, take_event,
};
use super::test_support::{Model, ModelGuard, block};
use super::*;
use crate::notify::Parker;
use crate::sync::{AtomicBool, Condvar, Mutex, Ordering};

const DEADLOCK_NEGATIVES: &[&str] = &[
    "feed::hold_loom::loom_l10_neg_no_flush_due_answer",
    "feed::hold_loom::loom_l10_neg_relocking_retry",
    "feed::hold_loom::loom_l10_neg_closing_ignored",
    "feed::hold_loom::loom_l10_neg_completion_without_retry",
    "feed::hold_loom::loom_l12_neg_blocking_step4_under_hub",
    "feed::hold_loom::loom_l12_neg_on_release_without_retry",
    "feed::hold_loom::loom_l13_neg_blocking_ingress_send",
];

#[test]
fn loom_hold_negatives_fail() {
    run_deadlock_negatives(DEADLOCK_NEGATIVES);
    run_negatives(
        &["feed::hold_loom::loom_l11_neg_planned_before_drain"],
        "tip step with a pending fanout",
    );
}

fn open(m: &Model) -> ModelGuard<'_> {
    let g = m.tip.lock();
    m.shared.hold_opened();
    g
}

fn release(m: &Model, g: ModelGuard<'_>) {
    m.shared.hold_closing();
    drop(g);
    m.shared.fire_release_wakes();
}

// ---- L10: V's hold, the in-hold wait and S's FlushDue (§4.7) ------------------------------------

/// V's in-hold service: S's `FlushDue` request, answered on V's next wake, and W's completion.
struct Service {
    /// S's parker while its request is unanswered.
    flush_due: Mutex<Option<Arc<Parker>>>,
    answered: AtomicBool,
    /// W's completion of the freeze (the `-dbcrashratio` variant).
    completed: AtomicBool,
}

impl Service {
    fn new() -> Service {
        Service {
            flush_due: Mutex::new(None),
            answered: AtomicBool::new(false),
            completed: AtomicBool::new(false),
        }
    }

    /// S's `FlushDue`, waited out on `me`.
    fn flush_due(&self, v: &Parker, me: &Arc<Parker>) {
        *self.flush_due.lock() = Some(me.clone());
        v.notify();
        while !self.answered.load(Ordering::SeqCst) {
            me.park();
        }
    }

    /// `in_hold_service_wait()`: park, then answer `FlushDue` (unless the fault drops it).
    fn wait(&self, v: &Parker, answer: bool) {
        v.park();
        let asked = self.flush_due.lock().take();
        if let Some(s) = asked.filter(|_| answer) {
            self.answered.store(true, Ordering::SeqCst);
            s.notify();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum L10 {
    Pops,
    Closes,
    CrashRatio,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum L10Fault {
    None,
    /// The in-hold wait does not answer `FlushDue`.
    NoFlushDueAnswer,
    /// The drain re-locks the tip mutex instead of using the held guard.
    RelockingRetry,
    /// `retry_pending` keeps a closing target's cursor.
    ClosingIgnored,
    /// The completion wait does not run `retry_pending`.
    CompletionWithoutRetry,
}

/// L10: V holds the tip lock across three disconnects at Q = 1, draining each through the held
/// guard with the in-hold wait between, while S, before taking events, waits on a `FlushDue`
/// only that wait answers. No step starts with `pending` non-empty, S gets 1, 2, 3, and the
/// hold is never released between the steps (one guard throughout).
fn l10(variant: L10, fault: L10Fault) {
    model(move || {
        let (m, p) = model_with_wake(1);
        let (id, mut f, _) = m.subscribe();
        m.tip.lock().feeds.faults.retry_ignores_closing = fault == L10Fault::ClosingIgnored;
        let svc = Arc::new(Service::new());
        let w_wake = Parker::new();
        let w = (variant == L10::CrashRatio).then(|| {
            let (svc, p, w_wake) = (svc.clone(), p.clone(), w_wake.clone());
            thread::spawn(move || {
                // W completes once S acks event 2 (it parks until then).
                w_wake.park();
                svc.completed.store(true, Ordering::SeqCst);
                p.notify();
            })
        });
        let v = {
            let (m, p, svc) = (m.clone(), p.clone(), svc.clone());
            thread::spawn(move || {
                let answer = fault != L10Fault::NoFlushDueAnswer;
                let mut g = open(&m);
                for k in 0..3 {
                    g.before_step();
                    g.disconnect(&m.shared, block(10 + k));
                    if variant == L10::CrashRatio && k == 1 {
                        while !svc.completed.load(Ordering::SeqCst) {
                            if fault != L10Fault::CompletionWithoutRetry {
                                g.retry(&m.shared);
                            }
                            svc.wait(&p, answer);
                        }
                    }
                    loop {
                        let r = if fault == L10Fault::RelockingRetry {
                            m.tip.lock().retry(&m.shared)
                        } else {
                            g.retry(&m.shared)
                        };
                        if r == Publish::Done {
                            break;
                        }
                        svc.wait(&p, answer);
                    }
                }
                release(&m, g);
            })
        };
        let me = Parker::new();
        svc.flush_due(&p, &me);
        if variant == L10::Closes {
            m.handle.deregister(id);
        } else {
            let mut ctl = Vec::new();
            let mut got = Vec::new();
            for _ in 0..3 {
                got.push(take_event(&mut *f, &me, &mut ctl));
                if got.len() == 2 {
                    w_wake.notify();
                }
            }
            assert_eq!(got, [1, 2, 3], "S gets the steps in order");
        }
        v.join().expect("V");
        if let Some(w) = w {
            w.join().expect("W");
        }
        if variant == L10::Closes {
            assert!(m.shared.registry.load().feeds.is_empty(), "S's entry is reclaimed");
        }
        drop(f);
    });
}

#[test]
fn loom_l10_hold_parked_on_flush_due() {
    l10(L10::Pops, L10Fault::None);
}

#[test]
fn loom_l10_closes_while_parked() {
    l10(L10::Closes, L10Fault::None);
}

#[test]
fn loom_l10_crashratio() {
    l10(L10::CrashRatio, L10Fault::None);
}

/// L10's negative control: the in-hold wait does not answer `FlushDue` (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_hold_negatives_fail"]
fn loom_l10_neg_no_flush_due_answer() {
    l10(L10::Pops, L10Fault::NoFlushDueAnswer);
}

/// L10's negative control: `retry_pending` re-locks the tip mutex (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_hold_negatives_fail"]
fn loom_l10_neg_relocking_retry() {
    l10(L10::Pops, L10Fault::RelockingRetry);
}

/// L10's negative control: closing ignored by `retry_pending`, so S's cursor stays (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_hold_negatives_fail"]
fn loom_l10_neg_closing_ignored() {
    l10(L10::Closes, L10Fault::ClosingIgnored);
}

/// L10's negative control: the completion wait without `retry_pending` (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_hold_negatives_fail"]
fn loom_l10_neg_completion_without_retry() {
    l10(L10::CrashRatio, L10Fault::CompletionWithoutRetry);
}

// ---- L11: SPV sub-steps drained on the held guard, with controls ------------------------------

const KIND: ControlKind = ControlKind::new(0);

/// L11: the SPV writer's one hold with three sub-steps (E = 1, Q = 1), each but the last
/// drained through the held guard with `wait()` between, the last fanned out at the release
/// and drained after it ‖ S at Q = 1 ‖ a thread sending two same-kind control messages. No
/// sub-step starts with `pending` non-empty (the writer's own check); S gets 1, 2, 3; the slot
/// holds ≤ 1 message of the kind, one per outstanding kind, and the last payload S consumes is
/// the newest.
fn l11(drain_between: bool) {
    model(move || {
        let (m, p) = model_with_wake(1);
        let (id, mut f, _) = m.subscribe();
        let c = {
            let m = m.clone();
            thread::spawn(move || {
                let reg = m.shared.registry.load();
                let slot = reg.feeds.iter().find(|e| e.id == id).map(|e| e.slot.clone());
                let slot = slot.expect("registered");
                for k in 1..=2u64 {
                    m.handle.send_control(id, ControlMsg::new(KIND, k));
                    let (held, kinds) = slot.control_held();
                    assert!(held <= 1 && u32::from(held) == kinds.count_ones(), "{held} held");
                }
            })
        };
        let s = thread::spawn(move || {
            let me = Parker::new();
            let mut ctl = Vec::new();
            let got = [0; 3].map(|_| take_event(&mut *f, &me, &mut ctl));
            (got, ctl, f)
        });
        // The writer runs on the model's own thread, so its check is the model's panic.
        let mut g = open(&m);
        for k in 0..3 {
            g.before_step();
            g.connect(&m.shared, block(k + 1));
            if k < 2 && drain_between {
                while g.retry(&m.shared) == Publish::Blocked {
                    p.park();
                }
            }
        }
        release(&m, g);
        drain(&m, &p);
        let (got, mut ctl, mut f) = s.join().expect("S");
        c.join().expect("C");
        while let Next::Control(c) = f.next() {
            ctl.push(c.downcast::<u64>().expect("a u64"));
        }
        assert_eq!(got, [1, 2, 3], "the single-composite order");
        assert!(ctl == [2] || ctl == [1, 2], "the newest payload is consumed last: {ctl:?}");
    });
}

#[test]
fn loom_l11_sub_steps_drained_on_the_guard() {
    l11(true);
}

/// L11's negative control: the next sub-step is planned before the drain, so the writer's
/// no-pending check fails (`Internal`; loom's release build has no `debug_assert`).
#[test]
#[ignore = "a negative control; run by loom_hold_negatives_fail"]
fn loom_l11_neg_planned_before_drain() {
    l11(false);
}

// ---- L12: the hub lock V takes inside the hold; register against a hold ------------------------

/// L12 (b): V holds with S1 and S2 full, and its in-hold wait takes a hub lock ‖ the events
/// thread, holding the hub, runs `deregister(S1)`, then lets the hub go and pops S2. Step 4's
/// `try_lock` fails at once, V's scan reclaims S1 exactly once, and the hold completes.
/// (L12's case without the hub is L3 (c), `loom_l3_contended_step4`.)
fn l12_hub(blocking_step4: bool) {
    model(move || {
        let (m, p) = model_with_wake(2);
        let (s1, f1, _) = m.subscribe();
        let (_, mut f2, _) = m.subscribe();
        let hub = Arc::new(Mutex::new(()));
        let v = {
            let (m, p, hub) = (m.clone(), p.clone(), hub.clone());
            thread::spawn(move || {
                let mut g = open(&m);
                g.connect(&m.shared, block(1));
                g.connect(&m.shared, block(2));
                while g.retry(&m.shared) == Publish::Blocked {
                    p.park();
                    drop(hub.lock());
                }
                release(&m, g);
            })
        };
        {
            let _hub = hub.lock();
            if blocking_step4 {
                let reg = m.shared.registry.load();
                let slot = reg.feeds.iter().find(|e| e.id == s1).map(|e| e.slot.clone());
                let slot = slot.expect("registered");
                slot.close(&m.shared);
                m.tip.lock().feeds.remove(&m.shared, s1, &slot);
            } else {
                m.handle.deregister(s1);
            }
        }
        let me = Parker::new();
        let mut ctl = Vec::new();
        assert_eq!(
            [take_event(&mut *f2, &me, &mut ctl), take_event(&mut *f2, &me, &mut ctl)],
            [1, 2]
        );
        v.join().expect("the hold completes");
        let reg = m.shared.registry.load();
        assert_eq!(reg.feeds.iter().filter(|e| e.id == s1).count(), 0, "S1 reclaimed");
        assert_eq!(reg.feeds.len(), 1, "S2 stays");
        m.tip.lock().feeds.assert_consistent();
        drop(f1);
    });
}

#[test]
fn loom_l12_dereg_under_hub_lock() {
    l12_hub(false);
}

/// L12's negative control: a blocking step 4 under the hub V's wait needs (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_hold_negatives_fail"]
fn loom_l12_neg_blocking_step4_under_hub() {
    l12_hub(true);
}

/// L12 (c): V's one hold ‖ `register` from another thread with §4.3's protocol (on
/// `HoldActive`: `on_release`, retry once, then wait). The register completes after the release.
fn l12_register(retry: bool) {
    model(move || {
        let (m, _) = model_with_wake(1);
        let v = {
            let m = m.clone();
            thread::spawn(move || {
                let mut g = open(&m);
                g.connect(&m.shared, block(1));
                release(&m, g);
            })
        };
        let me = Parker::new();
        let try_register = || match m.handle.register(Needs::default()) {
            Ok(id) => Some(id),
            Err(r) => {
                assert_eq!(r, Refused::HoldActive);
                None
            }
        };
        let id = loop {
            if let Some(id) = try_register() {
                break id;
            }
            m.shared.on_release(Notify::Thread(me.clone()));
            if retry && let Some(id) = try_register() {
                break id;
            }
            me.park();
        };
        v.join().expect("V");
        assert!(m.shared.registry.load().feeds.iter().any(|e| e.id == id), "registered");
    });
}

#[test]
fn loom_l12_register_after_release() {
    l12_register(true);
}

/// L12's negative control: `on_release` without the retry loses a release between the two.
#[test]
#[ignore = "a deadlock negative control; run by loom_hold_negatives_fail"]
fn loom_l12_neg_on_release_without_retry() {
    l12_register(false);
}

// ---- L13: SPV-LO9, the header ingress against the writer's in-hold wait (§4.9) ------------------

/// The header task's bounded ingress channel, full at the start (capacity 1).
struct Ingress {
    queued: Mutex<u32>,
    space: Condvar,
}

/// The network actor's mailbox: F's `cfilter` request and the actor's reply.
struct Actor {
    request: AtomicBool,
    reply: AtomicBool,
}

/// L13: the SPV header writer waits in its hold with event 2 pending for F (Q = 1) ‖ F awaits
/// an actor reply before popping ‖ the actor forwards a `headers` message to the writer's full
/// ingress channel, then replies. With `try_send` the actor pauses that peer instead of
/// blocking, so it replies, F pops, and the hold completes.
fn l13(blocking_send: bool) {
    model(move || {
        let (m, p) = model_with_wake(1);
        let (_, mut f, _) = m.subscribe();
        let ingress = Arc::new(Ingress {
            queued: Mutex::new(1),
            space: Condvar::new(),
        });
        let actor = Arc::new(Actor {
            request: AtomicBool::new(false),
            reply: AtomicBool::new(false),
        });
        let (a_wake, f_wake) = (Parker::new(), Parker::new());
        let a = {
            let (ingress, actor, a_wake, f_wake) =
                (ingress.clone(), actor.clone(), a_wake.clone(), f_wake.clone());
            thread::spawn(move || {
                while !actor.request.load(Ordering::SeqCst) {
                    a_wake.park();
                }
                // An inbound `headers` for the header task, then the `cfilter` reply. A
                // `try_send` meeting `Full` pauses the peer's headers (`fPauseRecv`) instead.
                let mut q = ingress.queued.lock();
                if blocking_send {
                    while *q == 1 {
                        q = ingress.space.wait(q);
                    }
                    *q += 1;
                }
                drop(q);
                actor.reply.store(true, Ordering::SeqCst);
                f_wake.notify();
            })
        };
        let s = {
            let (actor, a_wake, f_wake) = (actor.clone(), a_wake.clone(), f_wake.clone());
            thread::spawn(move || {
                actor.request.store(true, Ordering::SeqCst);
                a_wake.notify();
                while !actor.reply.load(Ordering::SeqCst) {
                    f_wake.park();
                }
                let mut ctl = Vec::new();
                [take_event(&mut *f, &f_wake, &mut ctl), take_event(&mut *f, &f_wake, &mut ctl)]
            })
        };
        let mut g = open(&m);
        g.connect(&m.shared, block(1));
        g.connect(&m.shared, block(2));
        while g.retry(&m.shared) == Publish::Blocked {
            p.park();
        }
        release(&m, g);
        // After the hold the header task reads its ingress again.
        *ingress.queued.lock() = 0;
        ingress.space.notify_all();
        assert_eq!(s.join().expect("F"), [1, 2]);
        a.join().expect("the actor");
    });
}

#[test]
fn loom_l13_ingress_try_send() {
    l13(false);
}

/// L13's negative control: a blocking ingress `send` (deadlock).
#[test]
#[ignore = "a deadlock negative control; run by loom_hold_negatives_fail"]
fn loom_l13_neg_blocking_ingress_send() {
    l13(true);
}
