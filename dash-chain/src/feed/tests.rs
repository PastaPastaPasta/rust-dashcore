//! F1's directed and property tests (SB5 §10.1): the slot and reader halves, the per-kind
//! controls, the wakes, the hold-aware tip-lock helper and the feed budget.

use std::sync::atomic::AtomicBool as StdAtomicBool;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use proptest::prelude::*;

use super::test_support::{block, cfg, conn, disc, feed, latch, push};
use super::*;
use crate::notify::Parker;

const A: ControlKind = ControlKind::new(0);
const B: ControlKind = ControlKind::new(1);

fn next_kind(r: &mut FeedReader) -> Option<Result<(ControlKind, u64), u64>> {
    match r.next() {
        Next::Control(c) => {
            let k = c.kind();
            Some(Ok((k, c.downcast::<u64>().expect("u64 payloads"))))
        }
        Next::Event(e) => Some(Err(e.seq())),
        Next::Empty => None,
    }
}

// ---- ControlKind, ControlMsg ----

#[test]
fn control_kind_in_range() {
    assert_eq!(ControlKind::new(7).get(), 7);
    let m = ControlMsg::new(B, [7u8; CONTROL_PAYLOAD_MAX]);
    assert_eq!(m.kind(), B);
    let m = m.downcast::<u64>().expect_err("wrong type keeps the message");
    assert_eq!(m.downcast::<[u8; CONTROL_PAYLOAD_MAX]>().unwrap()[0], 7);
}

/// n8: kinds ≥ MAX_CONTROL_KINDS are refused (a const kind fails to compile).
#[test]
#[should_panic(expected = "control kind out of range")]
fn control_kind_out_of_range() {
    let _ = ControlKind::new(std::hint::black_box(MAX_CONTROL_KINDS as u8));
}

// ---- F3 ----

/// F3: `used ≤ Q` under push, intake, pop and compression, and `used` = queued + staged.
#[test]
fn f3_capacity_with_staging() {
    for q in [1u32, 2, 8] {
        let (slot, _sh, mut r) = feed(q);
        let mut seq = 0u64;
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..20_000 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            match rng % 5 {
                0 | 1 => {
                    let k = (rng >> 8) as u32 % 4;
                    let e = if rng & 0x100 == 0 {
                        conn(k, seq + 1)
                    } else {
                        disc(k, seq + 1)
                    };
                    if push(&slot, e) {
                        seq += 1;
                    }
                }
                2 => {
                    let _ = r.next();
                }
                3 => {
                    if let Next::Event(_) = r.next() {
                        r.pop();
                    }
                }
                _ => {
                    let _ = r.remove_round_trip(&block((rng >> 8) as u32 % 4).hash);
                }
            }
            let (used, queued, _) = slot.counts();
            assert!(used <= q, "used {used} > Q {q}");
            assert_eq!(used as usize, queued + r.staged(), "used counts queue + staging");
        }
        assert!(seq > 0);
    }
}

/// A full slot refuses and asks for space; the first pop wakes the producer.
#[test]
fn f3_full_sets_want_space_and_pop_wakes() {
    let (slot, sh, mut r) = feed(2);
    let p = Parker::new();
    sh.set_producer_wake(Notify::Thread(p.clone()));
    assert!(push(&slot, conn(1, 1)));
    assert!(push(&slot, conn(2, 2)));
    assert!(matches!(slot.try_push(conn(3, 3)), Push::Full));
    // Intake alone frees nothing: staged events still count.
    assert!(matches!(r.next(), Next::Event(_)));
    assert!(matches!(slot.try_push(conn(3, 3)), Push::Full));
    assert!(!p.park_timeout(Duration::ZERO));
    r.pop();
    assert!(p.park_timeout(Duration::ZERO), "the pop after Full wakes the producer");
    assert!(push(&slot, conn(3, 3)));
}

// ---- F4 ----

/// Sol's four-step witness, with the real two-call boundary on S's thread: S's `next()` is
/// empty; H sends C; V pushes E (from the main thread, in that order); S's next `next()` returns C, and the one after returns E.
#[test]
fn f4_control_before_data() {
    let (slot, _sh, mut r) = feed(4);
    let gate = Arc::new(Barrier::new(2));
    let g = gate.clone();
    let s = thread::spawn(move || {
        assert!(matches!(r.next(), Next::Empty));
        g.wait(); // between the two calls
        g.wait();
        let first = next_kind(&mut r);
        let second = next_kind(&mut r);
        (first, second)
    });
    gate.wait();
    assert_eq!(slot.send_control(ControlMsg::new(A, 1u64)), SendControl::Queued);
    assert!(push(&slot, conn(5, 1)));
    gate.wait();
    let (first, second) = s.join().unwrap();
    assert_eq!(first, Some(Ok((A, 1))));
    assert_eq!(second, Some(Err(1)));
}

/// A control message is delivered while the data queue is full.
#[test]
fn f4_control_bypasses_full_queue() {
    let (slot, _sh, mut r) = feed(1);
    assert!(push(&slot, conn(1, 1)));
    assert!(matches!(slot.try_push(conn(2, 2)), Push::Full));
    assert_eq!(slot.send_control(ControlMsg::new(B, 9u64)), SendControl::Queued);
    assert_eq!(next_kind(&mut r), Some(Ok((B, 9))));
    assert_eq!(next_kind(&mut r), Some(Err(1)));
}

/// n1: A₁, B₁, A₂ delivers B₁ then A₂, with A₂'s payload; A is outstanding until consumed.
#[test]
fn f4_coalesced_moves_to_back() {
    let (slot, _sh, mut r) = feed(4);
    assert_eq!(slot.send_control(ControlMsg::new(A, 1u64)), SendControl::Queued);
    assert_eq!(slot.send_control(ControlMsg::new(B, 2u64)), SendControl::Queued);
    assert_eq!(slot.send_control(ControlMsg::new(A, 3u64)), SendControl::Coalesced);
    assert_eq!(slot.control_outstanding(), 0b11);
    assert_eq!(next_kind(&mut r), Some(Ok((B, 2))));
    assert_eq!(slot.control_outstanding(), 0b01);
    assert_eq!(next_kind(&mut r), Some(Ok((A, 3))));
    assert_eq!(slot.control_outstanding(), 0);
    // Consumed: the kind is free again.
    assert_eq!(slot.send_control(ControlMsg::new(A, 4u64)), SendControl::Queued);
    assert_eq!(next_kind(&mut r), Some(Ok((A, 4))));
    assert_eq!(next_kind(&mut r), None);
}

/// Compression takes data only: a queued control stays for the next `next()`.
#[test]
fn f4_compression_leaves_control_queued() {
    let (slot, _sh, mut r) = feed(4);
    assert!(push(&slot, conn(1, 1)));
    assert!(push(&slot, disc(1, 2)));
    assert_eq!(slot.send_control(ControlMsg::new(A, 7u64)), SendControl::Queued);
    assert_eq!(r.remove_round_trip(&block(1).hash), Some((1, 2)));
    assert_eq!(slot.control_outstanding(), 0b1);
    assert_eq!(next_kind(&mut r), Some(Ok((A, 7))));
    assert_eq!(slot.counts().0, 0);
}

#[test]
fn send_control_to_closing_slot_is_stale() {
    let (slot, sh, _r) = feed(4);
    slot.close(&sh);
    assert_eq!(slot.send_control(ControlMsg::new(A, 1u64)), SendControl::Stale);
    assert_eq!(slot.control_outstanding(), 0);
}

// ---- compression (CH2 §4.4.1 rule 3) ----

#[test]
fn remove_round_trip_finds_first_later_disconnect() {
    let (slot, _sh, mut r) = feed(8);
    for e in [conn(1, 1), conn(2, 2), disc(2, 3), disc(1, 4), conn(1, 5), disc(1, 6)] {
        assert!(push(&slot, e));
    }
    // Head is Connected(1): pairs with the first later Disconnected(1), seq 4.
    assert_eq!(r.remove_round_trip(&block(1).hash), Some((1, 4)));
    assert_eq!(slot.counts().0, 4);
    // Head is now Connected(2), not Connected(1).
    assert_eq!(r.remove_round_trip(&block(1).hash), None);
    assert_eq!(r.remove_round_trip(&block(2).hash), Some((2, 3)));
    assert_eq!(next_kind(&mut r), Some(Err(5)));
    assert_eq!(r.pop().map(|e| e.seq()), Some(5));
    assert_eq!(next_kind(&mut r), Some(Err(6)));
    // A head with no later disconnect is not compressed.
    assert_eq!(r.remove_round_trip(&block(1).hash), None);
}

/// The driver's fallback (`reader/next-pop-compress-1k`): a compression that finds nothing
/// keeps `next`'s selection, so `pop` takes the head; one that removes clears it.
#[test]
fn pop_after_a_failed_compression() {
    let (slot, _sh, mut r) = feed(4);
    for e in [conn(1, 1), conn(2, 2), disc(2, 3)] {
        assert!(push(&slot, e));
    }
    assert_eq!(next_kind(&mut r), Some(Err(1)));
    assert_eq!(r.remove_round_trip(&block(1).hash), None);
    assert_eq!(r.pop().map(|e| e.seq()), Some(1));
    assert_eq!(next_kind(&mut r), Some(Err(2)));
    assert_eq!(r.remove_round_trip(&block(2).hash), Some((2, 3)));
    let popped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.pop()));
    assert_eq!(popped.is_err(), cfg!(debug_assertions), "a compression clears the selection");
}

/// X20: compression frees space and wakes a waiting producer.
#[test]
fn compress_then_wake() {
    let (slot, sh, mut r) = feed(2);
    let p = Parker::new();
    sh.set_producer_wake(Notify::Thread(p.clone()));
    assert!(push(&slot, conn(1, 1)));
    assert!(push(&slot, disc(1, 2)));
    assert!(matches!(slot.try_push(conn(2, 3)), Push::Full));
    assert_eq!(r.remove_round_trip(&block(1).hash), Some((1, 2)));
    assert!(p.park_timeout(Duration::ZERO));
    assert!(push(&slot, conn(2, 3)));
    assert!(push(&slot, conn(3, 4)));
}

// ---- F5 and the producer wakes ----

/// The park protocol: `set_waker`, `next()` again, park only on `Empty`.
fn park_for_event(r: &mut FeedReader, p: &Arc<Parker>) -> Result<(ControlKind, u64), u64> {
    loop {
        if let Some(x) = next_kind(r) {
            return x;
        }
        r.set_waker(Notify::Thread(p.clone()));
        if let Some(x) = next_kind(r) {
            return x;
        }
        p.park();
    }
}

#[test]
fn f5_wake_on_enqueue() {
    for _ in 0..200 {
        let (slot, _sh, mut r) = feed(4);
        let s = thread::spawn(move || {
            let p = Parker::new();
            let x = park_for_event(&mut r, &p);
            if x.is_err() {
                r.pop();
            }
            x
        });
        thread::yield_now();
        assert!(push(&slot, conn(1, 1)));
        assert_eq!(s.join().unwrap(), Err(1));
    }
}

#[test]
fn f5_wake_on_control() {
    for _ in 0..200 {
        let (slot, _sh, mut r) = feed(4);
        let s = thread::spawn(move || park_for_event(&mut r, &Parker::new()));
        thread::yield_now();
        slot.send_control(ControlMsg::new(B, 5u64));
        assert_eq!(s.join().unwrap(), Ok((B, 5)));
    }
}

/// Q = 1: the producer parks on `Full` and the reader's pop resumes it, for 1,000 events.
#[test]
fn space_wake_resumes_parked_producer() {
    let (slot, sh, mut r) = feed(1);
    let p = Parker::new();
    sh.set_producer_wake(Notify::Thread(p.clone()));
    const N: u64 = 1_000;
    let producer = {
        let slot = slot.clone();
        thread::spawn(move || {
            for s in 1..=N {
                while !push(&slot, conn(s as u32, s)) {
                    p.park();
                }
            }
        })
    };
    let rp = Parker::new();
    for s in 1..=N {
        assert_eq!(park_for_event(&mut r, &rp), Err(s));
        r.pop();
    }
    producer.join().unwrap();
}

/// `deregister` steps 2–3 wake a producer waiting on the slot; the next push is `Closed`.
#[test]
fn dereg_wake_releases_producer() {
    let (slot, sh, r) = feed(1);
    let p = Parker::new();
    let w = Parker::new();
    sh.set_producer_wake(Notify::Thread(p.clone()));
    sh.set_close_hook(Notify::Thread(w.clone()));
    assert!(push(&slot, conn(1, 1)));
    let producer = {
        let slot = slot.clone();
        thread::spawn(move || {
            while let Push::Full = slot.try_push(conn(2, 2)) {
                p.park();
            }
            slot.is_closing()
        })
    };
    thread::sleep(Duration::from_millis(5));
    slot.close(&sh);
    assert!(producer.join().unwrap());
    assert!(r.is_closed());
    assert!(w.park_timeout(Duration::ZERO), "close hook fired");
    assert!(matches!(slot.try_push(conn(3, 3)), Push::Closed));
}

/// `deregister` also wakes a parked reader, once, so it sees the close without a timed wake
/// (review F-7). A waker set after the close fires at once: the park protocol's second `next()`
/// returns `Empty` on a closed slot, so a stored one would never be woken.
#[test]
fn dereg_wakes_the_parked_reader() {
    let (slot, sh, mut r) = feed(2);
    let rw = Parker::new();
    r.set_waker(Notify::Thread(rw.clone()));
    slot.close(&sh);
    assert!(rw.park_timeout(Duration::ZERO), "the reader's waker fired");
    assert!(r.is_closed());
    r.set_waker(Notify::Thread(rw.clone()));
    assert!(rw.park_timeout(Duration::ZERO), "a waker set after the close fires at once");
    slot.close(&sh);
    assert!(!rw.park_timeout(Duration::ZERO), "and is not stored for a later close");
}

/// D9: dropping the reader closes the slot and fires both wakes, without the tip lock.
#[test]
fn reader_drop_closes_slot() {
    let (slot, sh, r) = feed(2);
    let p = Parker::new();
    let w = Parker::new();
    sh.set_producer_wake(Notify::Thread(p.clone()));
    sh.set_close_hook(Notify::Thread(w.clone()));
    drop(r);
    assert!(slot.is_closing());
    assert!(p.park_timeout(Duration::ZERO));
    assert!(w.park_timeout(Duration::ZERO));
}

/// DEC-152 at slot level (§4.5): a reader paused inside its intake section blocks a producer's
/// push for exactly the pause; when it resumes, the push completes. F3 adds the V-loop variant.
#[test]
fn producer_resumes_after_paused_slot_holder() {
    let (slot, _sh, mut r) = feed(4);
    assert!(push(&slot, conn(1, 1)));
    slot.faults.intake.arm();
    let s = thread::spawn(move || {
        assert!(matches!(r.next(), Next::Event(_)));
        r
    });
    slot.faults.intake.wait_paused();
    let pushed = Arc::new(StdAtomicBool::new(false));
    let t = Instant::now();
    let producer = {
        let (slot, pushed) = (slot.clone(), pushed.clone());
        thread::spawn(move || {
            assert!(push(&slot, conn(2, 2)));
            pushed.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    };
    let pause = Duration::from_millis(50);
    thread::sleep(pause);
    assert!(
        !pushed.load(std::sync::atomic::Ordering::SeqCst),
        "the push waits on the paused section"
    );
    slot.faults.intake.release();
    producer.join().unwrap();
    // Measured from before the spawn, so a late-scheduled producer cannot shorten it.
    assert!(t.elapsed() >= pause);
    let mut r = s.join().unwrap();
    r.pop();
    assert_eq!(next_kind(&mut r), Some(Err(2)));
}

// ---- DEC-155 (1) ----

#[derive(Clone, Debug)]
enum Op {
    Send(u8),
    Next,
    NextPop,
    Push(bool),
    Compress(u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0u8..3).prop_map(Op::Send),
        2 => Just(Op::Next),
        2 => Just(Op::NextPop),
        2 => any::<bool>().prop_map(Op::Push),
        1 => (0u8..3).prop_map(Op::Compress),
    ]
}

proptest! {
    /// DEC-155 (1): at every point the slot holds ≤ MAX_CONTROL_KINDS messages, one per kind;
    /// every consumed payload is the newest sent for its kind; the delivered sequence is a
    /// subsequence of the send order (n1); `next()` returns control before data.
    #[test]
    fn control_outstanding_per_kind(ops in proptest::collection::vec(op(), 1..400)) {
        let (slot, _sh, mut r) = feed(3);
        let mut order: Vec<u8> = Vec::new();
        let mut latest: [Option<u64>; MAX_CONTROL_KINDS] = [None; MAX_CONTROL_KINDS];
        let (mut sent, mut seq, mut last_delivered) = (0u64, 0u64, 0u64);
        for o in ops {
            match o {
                Op::Send(k) => {
                    sent += 1;
                    let got = slot.send_control(ControlMsg::new(ControlKind::new(k), sent));
                    if latest[k as usize].replace(sent).is_some() {
                        prop_assert_eq!(got, SendControl::Coalesced);
                        order.retain(|&x| x != k);
                    } else {
                        prop_assert_eq!(got, SendControl::Queued);
                    }
                    order.push(k);
                }
                Op::Next | Op::NextPop => match next_kind(&mut r) {
                    Some(Ok((kind, payload))) => {
                        let k = order.remove(0);
                        prop_assert_eq!(kind.get(), k);
                        prop_assert_eq!(Some(payload), latest[k as usize].take());
                        prop_assert!(payload > last_delivered, "delivery is a subsequence of sends");
                        last_delivered = payload;
                    }
                    Some(Err(_)) => {
                        prop_assert!(order.is_empty(), "data returned with control outstanding");
                        if matches!(o, Op::NextPop) {
                            r.pop();
                        }
                    }
                    None => prop_assert!(order.is_empty()),
                },
                Op::Push(c) => {
                    let e = if c { conn((seq % 3) as u32, seq + 1) } else { disc((seq % 3) as u32, seq + 1) };
                    if push(&slot, e) {
                        seq += 1;
                    }
                }
                Op::Compress(k) => {
                    let _ = r.remove_round_trip(&block(k as u32).hash);
                }
            }
            let (_, _, held) = slot.counts();
            prop_assert!(held as usize <= MAX_CONTROL_KINDS);
            prop_assert_eq!(held as usize, order.len());
            let mask = order.iter().fold(0u8, |m, &k| m | 1 << k);
            prop_assert_eq!(slot.control_outstanding(), mask);
        }
    }
}

/// A-F12 (Sol r2's interleaving): a `FlushCommit` + `ChainStateFlushed` pair between every two
/// reader `next()` calls, 10⁴ rounds. The slot never holds more than 2; variants compress
/// between calls and pause the reader for 100 rounds.
#[test]
fn peak_control_mirror_pairs_between_intakes() {
    const FLUSH_COMMIT: ControlKind = ControlKind::new(0);
    const STATE_FLUSHED: ControlKind = ControlKind::new(1);
    for variant in 0..3 {
        let (slot, _sh, mut r) = feed(8);
        let mut peak = 0;
        for round in 0..10_000u64 {
            slot.send_control(ControlMsg::new(FLUSH_COMMIT, round));
            slot.send_control(ControlMsg::new(STATE_FLUSHED, round));
            if variant == 1 {
                push(&slot, conn(1, 2 * round + 1));
                push(&slot, disc(1, 2 * round + 2));
                let _ = r.remove_round_trip(&block(1).hash);
            }
            peak = peak.max(slot.counts().2);
            let paused = variant == 2 && (5_000..5_100).contains(&round);
            if !paused {
                while let Some(Ok(_)) = next_kind(&mut r) {}
            }
        }
        assert_eq!(peak, 2, "variant {variant}");
        // The control bytes held stay within (K + 1) · (24 + 256).
        assert!(peak as usize * (size_of::<ControlMsg>() + CONTROL_PAYLOAD_MAX) <= CONTROL_BYTES);
    }
}

// ---- the tip-lock helper and release wakes (DEC-157 (2)) ----

/// A stand-in for the tip mutex: `acquire_tip` sees only `try_lock`.
fn tip_mutex() -> Arc<parking_lot::Mutex<()>> {
    Arc::new(parking_lot::Mutex::new(()))
}

fn acquire(sh: &FeedShared, m: &parking_lot::Mutex<()>) -> Result<(), Refused> {
    sh.acquire_tip(|| m.try_lock()).map(drop)
}

#[test]
fn acquire_tip_free_lock() {
    let sh = FeedShared::new(cfg(1), latch());
    let m = tip_mutex();
    assert!(acquire(&sh, &m).is_ok());
}

/// An ordinary step section is waited out; it never reports `HoldActive`.
#[test]
fn acquire_tip_waits_out_ordinary_section() {
    let sh = Arc::new(FeedShared::new(cfg(1), latch()));
    let m = tip_mutex();
    let g = m.lock();
    let t = {
        let (sh, m) = (sh.clone(), m.clone());
        thread::spawn(move || acquire(&sh, &m))
    };
    thread::sleep(Duration::from_millis(10));
    drop(g);
    assert_eq!(t.join().unwrap(), Ok(()));
}

/// `hold_owner_refused` at helper level: the owner's thread gets `WouldDeadlock`, any other
/// `HoldActive`, in every build.
#[test]
fn acquire_tip_refuses_during_hold() {
    let sh = Arc::new(FeedShared::new(cfg(1), latch()));
    let m = tip_mutex();
    let g = m.lock_arc();
    sh.hold_opened();
    assert_eq!(acquire(&sh, &m), Err(Refused::WouldDeadlock));
    let t = {
        let (sh, m) = (sh.clone(), m.clone());
        thread::spawn(move || acquire(&sh, &m))
    };
    assert_eq!(t.join().unwrap(), Err(Refused::HoldActive));
    sh.hold_closing();
    drop(g);
    sh.fire_release_wakes();
    assert!(acquire(&sh, &m).is_ok());
}

/// The register-then-retry protocol: on `HoldActive`, `on_release`, retry once, and wait only
/// if the retry is `HoldActive` again. The release clears the owner, drops the guard, then fires.
#[test]
fn hold_active_release_wake() {
    for round in 0..200u32 {
        let sh = Arc::new(FeedShared::new(cfg(1), latch()));
        let m = tip_mutex();
        let g = m.lock_arc();
        sh.hold_opened();
        let caller = {
            let (sh, m) = (sh.clone(), m.clone());
            thread::spawn(move || {
                let p = Parker::new();
                let mut waits = 0;
                loop {
                    if acquire(&sh, &m).is_ok() {
                        return waits;
                    }
                    sh.on_release(Notify::Thread(p.clone()));
                    match acquire(&sh, &m) {
                        Ok(()) => return waits,
                        Err(e) => assert_eq!(e, Refused::HoldActive),
                    }
                    p.park();
                    waits += 1;
                }
            })
        };
        if round.is_multiple_of(2) {
            thread::yield_now();
        }
        sh.hold_closing();
        drop(g);
        sh.fire_release_wakes();
        let waits = caller.join().unwrap();
        assert!(waits <= 1);
    }
}

// ---- §6.1 budget ----

/// §6.1's figures with the 576 B fixed term: per-feed 175,128 / 46,104 B; feed caps 2,802,048
/// (node 16), 1,401,024 (SPV 8) and 368,832 (mobile 8).
#[test]
fn feed_budget_matches_design() {
    assert_eq!(size_of::<ChainEvent>(), 56);
    assert_eq!(size_of::<ControlMsg>(), 24);
    assert_eq!(CONTROL_BYTES, 2_520);
    assert_eq!(per_feed_cap(1024), 175_128);
    assert_eq!(per_feed_cap(256), 46_104);
    assert_eq!(FeedConfig::node().feed_cap(), 2_802_048);
    assert_eq!(FeedConfig::spv().feed_cap(), 1_401_024);
    assert_eq!(FeedConfig::spv_mobile().feed_cap(), 368_832);
}

/// The buffers are reserved once, at Q each, and never grow (DEC-152 (1)).
#[test]
fn buffers_reserved_at_q() {
    let (slot, _sh, mut r) = feed(16);
    let caps = |s: &FeedSlot, r: &FeedReader| {
        let (st, sp) = r.buffer_caps();
        (s.lock().queue.capacity(), st, sp)
    };
    let sorted = |s: &FeedSlot, r: &FeedReader| {
        let (a, b, c) = caps(s, r);
        let mut v = [a, b, c];
        v.sort_unstable();
        v
    };
    let before = sorted(&slot, &r);
    assert!(before[0] >= 16);
    for s in 1..=10_000u64 {
        push(&slot, conn(s as u32, s));
        if s.is_multiple_of(3) {
            while let Next::Event(_) = r.next() {
                r.pop();
            }
        } else {
            let _ = r.next();
        }
        // The three buffers trade places by swaps; none ever grows.
        assert_eq!(sorted(&slot, &r), before);
    }
}
