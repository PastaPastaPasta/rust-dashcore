//! Loom models L1, L2, L5 and L9 (SB5 §10.2) over the production slot, reader and `Parker`,
//! each with its planted negative control. Run with
//! `RUSTFLAGS="--cfg loom" cargo test -p dash-chain --release --lib loom_`.
//!
//! Every model drives the real operations with their real lock boundaries; a model never merges
//! two API calls into one action. Preemptions are bounded at 3 (`LOOM_MAX_PREEMPTIONS`, §10.2).

use std::collections::VecDeque;
use std::sync::Arc;

use loom::thread;

use super::test_support::{Model, cfg, conn, feed, feed_on};
use super::*;
use crate::notify::Parker;

pub(crate) fn model(f: impl Fn() + Sync + Send + 'static) {
    let mut b = loom::model::Builder::new();
    b.preemption_bound = Some(3);
    b.check(f);
}

fn ev(seq: u64) -> ChainEvent {
    conn(seq as u32, seq)
}

/// The producer's push loop on one slot: on `Full` it parks until the space wake.
fn produce(slot: &FeedSlot, producer: &Parker, seq: u64) {
    loop {
        match slot.try_push(ev(seq)) {
            Push::Pushed(w) => {
                if let Some(w) = w {
                    w.notify();
                }
                return;
            }
            Push::Full => producer.park(),
            Push::Closed => panic!("the slot never closes here"),
        }
    }
}

/// The subscriber's park protocol (F5): `next`; on `Empty`, `set_waker`, `next` again, park.
/// `recheck = false` is L2's planted negative control.
fn take(r: &mut FeedReader, me: &Arc<Parker>, recheck: bool) -> u64 {
    loop {
        if let Some(s) = pop_event(r) {
            return s;
        }
        r.set_waker(Notify::Thread(me.clone()));
        if recheck && let Some(s) = pop_event(r) {
            return s;
        }
        me.park();
    }
}

fn pop_event(r: &mut FeedReader) -> Option<u64> {
    let s = match r.next() {
        Next::Event(e) => e.seq(),
        _ => return None,
    };
    r.pop();
    Some(s)
}

/// The negative controls that end in a loom deadlock. Loom's deadlock panic aborts the binary
/// while other model threads unwind (§10.2), so each runs alone in a child process of this test
/// binary and must fail with loom's deadlock report.
const DEADLOCK_NEGATIVES: &[&str] =
    &["feed::loom_tests::loom_l1_neg_lost_permit", "feed::loom_tests::loom_l2_neg_no_recheck"];

#[test]
fn loom_negatives_fail() {
    run_deadlock_negatives(DEADLOCK_NEGATIVES);
}

/// A model chain with `max_feeds` feeds at Q = 1, its producer wake `p`.
pub(crate) fn model_with_wake(max_feeds: u32) -> (Model, Arc<Parker>) {
    let m = Model::new(cfg(1).with_max_feeds(max_feeds));
    let p = Parker::new();
    m.shared.set_producer_wake(Notify::Thread(p.clone()));
    (m, p)
}

/// The writer's wait outside a hold (§4.3): `retry_pending` until `Done`, parking between.
pub(crate) fn drain(m: &Model, p: &Parker) {
    while m.tip.lock().retry(&m.shared) == Publish::Blocked {
        p.park();
    }
}

/// The next event's `seq` with F5's park protocol (`next`, `set_waker`, `next`, park); control
/// messages go to `ctl`.
pub(crate) fn take_event(f: &mut dyn SubscriberFeed, me: &Arc<Parker>, ctl: &mut Vec<u64>) -> u64 {
    let mut parked = false;
    loop {
        match f.next() {
            Next::Event(_) => return f.pop().expect("selected").seq(),
            Next::Control(c) => ctl.push(c.downcast::<u64>().expect("a u64")),
            Next::Empty if parked => {
                me.park();
                parked = false;
            }
            Next::Empty => {
                f.set_waker(Notify::Thread(me.clone()));
                parked = true;
            }
        }
    }
}

/// Runs each `#[ignore]`d negative control in a child process (a loom deadlock aborts the whole
/// binary) and asserts that it failed with a deadlock.
pub(crate) fn run_deadlock_negatives(names: &[&str]) {
    run_negatives(names, "deadlock");
}

/// As `run_deadlock_negatives`, for controls that fail with `needle` (a model's own assertion:
/// its panic while loom unwinds the other threads aborts the binary too).
pub(crate) fn run_negatives(names: &[&str], needle: &str) {
    for name in names {
        let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args(["--ignored", "--exact", "--test-threads=1", name])
            .env("RUST_BACKTRACE", "0")
            .output()
            .expect("run the negative control");
        let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
        assert!(text.contains("running 1 test"), "{name} was not selected:\n{text}");
        assert!(!out.status.success(), "{name} passed; it must fail");
        assert!(text.contains(needle), "{name} failed without {needle:?}:\n{text}");
    }
}

// ---- L1: the space wake; the reader is preempted inside each slot section ----

fn l1(producer: fn() -> Arc<Parker>) {
    model(move || {
        let (slot, shared, mut r) = feed(1);
        let p = producer();
        shared.set_producer_wake(Notify::Thread(p.clone()));
        let reader = thread::spawn(move || {
            let me = Parker::new();
            let got = [take(&mut r, &me, true), take(&mut r, &me, true)];
            assert_eq!(got, [1, 2]);
        });
        for s in 1..=2 {
            produce(&slot, &p, s);
            assert!(slot.counts().0 <= 1, "used ≤ Q");
        }
        reader.join().unwrap();
    });
}

/// L1: Q = 1, two events: the producer always resumes after `Full`.
#[test]
fn loom_l1_space_wake() {
    l1(Parker::new);
}

/// L1's negative control: a producer parker that drops a notify arriving before the park.
#[test]
#[ignore = "negative control: aborts with a loom deadlock; `loom_negatives_fail` runs it alone"]
fn loom_l1_neg_lost_permit() {
    l1(Parker::new_lossy);
}

// ---- L2: wake on enqueue ----

fn l2(recheck: bool) {
    model(move || {
        let (slot, _shared, mut r) = feed(2);
        let reader = thread::spawn(move || {
            let me = Parker::new();
            assert_eq!(take(&mut r, &me, recheck), 1);
        });
        let Push::Pushed(w) = slot.try_push(ev(1)) else {
            panic!("room")
        };
        if let Some(w) = w {
            w.notify();
        }
        reader.join().unwrap();
    });
}

/// L2: the reader never parks with an item queued and no wake pending.
#[test]
fn loom_l2_wake_on_enqueue() {
    l2(true);
}

/// L2's negative control: no `next()` re-check after `set_waker`.
#[test]
#[ignore = "negative control: aborts with a loom deadlock; `loom_negatives_fail` runs it alone"]
fn loom_l2_neg_no_recheck() {
    l2(false);
}

// ---- L5: control before data, with the real two-call boundary ----

/// What one `next()` returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Got {
    Control,
    Event,
    Empty,
}

/// rev0's split protocol (the negative control): `pop_control()` and then `front()`, two slot
/// sections, over the production slot's own operations.
struct SplitReader {
    slot: Arc<FeedSlot>,
    staging: VecDeque<ChainEvent>,
    spare: VecDeque<ChainEvent>,
}

impl SplitReader {
    fn next(&mut self) -> Got {
        if self.slot.lock().take_control().is_some() {
            return Got::Control;
        }
        let append = self.slot.lock().intake(&mut self.staging, &mut self.spare);
        if append {
            self.staging.append(&mut self.spare);
        }
        if self.staging.is_empty() {
            Got::Empty
        } else {
            Got::Event
        }
    }
}

fn l5(split: bool) {
    model(move || {
        let (slot, _shared, mut r) = feed(2);
        // Set by H after `send_control` returns; V reads it before pushing. If V saw it, C's
        // push precedes E's push, and F4 says `next()` returns C before E.
        let control_sent = Arc::new(AtomicBool::new(false));
        let (hs, hc) = (slot.clone(), control_sent.clone());
        let h = thread::spawn(move || {
            hs.send_control(ControlMsg::new(ControlKind::new(0), 7u64));
            hc.store(true, Ordering::SeqCst);
        });
        let (vs, vc) = (slot.clone(), control_sent.clone());
        let v = thread::spawn(move || {
            let after = vc.load(Ordering::SeqCst);
            let Push::Pushed(_) = vs.try_push(ev(1)) else {
                panic!("room")
            };
            after
        });
        let mut split_reader = split.then(|| SplitReader {
            slot: slot.clone(),
            staging: VecDeque::with_capacity(2),
            spare: VecDeque::with_capacity(2),
        });
        let mut got = Vec::new();
        for _ in 0..3 {
            let g = match &mut split_reader {
                Some(s) => s.next(),
                None => match r.next() {
                    Next::Control(_) => Got::Control,
                    Next::Event(_) => Got::Event,
                    Next::Empty => Got::Empty,
                },
            };
            got.push(g);
            if g == Got::Event {
                break;
            }
        }
        h.join().unwrap();
        let data_after_control = v.join().unwrap();
        if data_after_control && let Some(i) = got.iter().position(|&g| g == Got::Event) {
            assert!(
                got[..i].contains(&Got::Control),
                "data observed before earlier control: {got:?}"
            );
        }
    });
}

/// L5: Sol's witness over the production `next()`: control pushed before data is returned first.
#[test]
fn loom_l5_control_before_data() {
    l5(false);
}

/// L5's negative control: rev0's `pop_control()` then `front()` in two sections.
#[test]
#[should_panic(expected = "data observed before earlier control")]
fn loom_l5_neg_split_calls() {
    l5(true);
}

// ---- L9: the producer blocks only on a reader's O(1) section ----

/// L9: the producer fans two events out to slot A (Q = 1) and slot B (Q = 2) while reader A is
/// preempted anywhere in its sections and reader B pops. On A's `Full` the producer parks until
/// A's pop; otherwise it blocks only on a reader's O(1) section. The step always completes
/// (loom's deadlock detection), and both readers get both events in order. L9's allocation
/// negative control runs under the counting allocator in `tests/feed_alloc.rs`: loom's
/// scheduler allocates on every operation.
#[test]
fn loom_l9_fanout_past_preempted_reader() {
    model(|| {
        let (a, shared, mut ra) = feed(1);
        let (b, mut rb) = feed_on(&shared, 2);
        let p = Parker::new();
        shared.set_producer_wake(Notify::Thread(p.clone()));
        let ta = thread::spawn(move || {
            let me = Parker::new();
            [take(&mut ra, &me, true), take(&mut ra, &me, true)]
        });
        let tb = thread::spawn(move || {
            let me = Parker::new();
            [take(&mut rb, &me, true), take(&mut rb, &me, true)]
        });
        for s in 1..=2 {
            produce(&a, &p, s);
            produce(&b, &p, s);
        }
        assert_eq!(ta.join().unwrap(), [1, 2]);
        assert_eq!(tb.join().unwrap(), [1, 2]);
    });
}
