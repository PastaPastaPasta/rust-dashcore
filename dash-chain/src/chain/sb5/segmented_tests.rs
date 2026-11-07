//! SB5 §4.8 and §6.2: `activate_segmented` against the unsegmented composite, its waits, its
//! release order, an error mid-hold, and the step budget.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::task::{Poll, Wake, Waker};

use dash_params::Network;
use proptest::prelude::*;

use super::hold_tests::{WAIT, subscribe, take};
use super::step_tests::{Stage, drive, meta, real_step, stages};
use crate::budget::check_sub_step;
use crate::chain::spv_tests::{Spv, T0, mine_n, regtest_pack};
use crate::chain::tests::{branch, with_cfg};
use crate::error::ChainError;
use crate::feed::{
    ChainEvent, ChainFeeds, FeedConfig, FeedSource, Handoff, Needs, Next, Publish, Refused,
    SubscribeError, SubscriberFeed,
};
use crate::mem_store::{MemHeaderStore, ReadKind};
use crate::notify::{Notify, Parker};
use crate::shared::ColdInvalidation;
use crate::snapshot::ChainReadSnapshot;
use crate::status::Status;
use crate::testing::{batch, params, services};
use crate::tip::BlockRef;
use crate::{BlockId, ChainConfig, HeaderChain, step_cap};

const T: u32 = 1_700_000_000;

/// An SPV chain from genesis with feed capacity `q` and `segment_events` = `e`.
pub(super) fn spv(q: u32, e: u32) -> HeaderChain {
    with_cfg(|p| {
        let mut cfg = ChainConfig::spv(p, 10_000);
        cfg.feeds.capacity = q;
        cfg.feeds.segment_events = e;
        cfg
    })
}

pub(super) fn bref(c: &HeaderChain, id: BlockId) -> BlockRef {
    c.block_ref_of(id).expect("held")
}

/// A trunk with the tip at `trunk[tip]`, and a side branch on `trunk[fork]`; the step goes to
/// the side branch's end, or to `trunk[to]` when there is none.
#[derive(Clone, Debug)]
struct Shape {
    trunk: u64,
    tip: usize,
    fork: usize,
    side: u64,
    to: usize,
}

fn shape() -> impl Strategy<Value = Shape> {
    (1..24u64, 0..24usize, 0..24usize, 0..24u64, 0..24usize).prop_map(
        |(trunk, tip, fork, side, to)| Shape {
            trunk,
            tip: tip % trunk as usize,
            fork: fork % trunk as usize,
            side,
            to: to % trunk as usize,
        },
    )
}

/// Builds `s` on `c` with its tip in place (one unsegmented step); returns the target.
fn build(c: &mut HeaderChain, s: &Shape) -> BlockId {
    let a = branch(c, BlockId(0), 1, s.trunk, T);
    let b = branch(c, a[s.fork], 1000, s.side, T + 500);
    c.tip_plan(a[s.tip]).expect("plan");
    c.set_active_tip(a[s.tip]).expect("to the start");
    b.last().copied().unwrap_or(a[s.to])
}

/// What a segmented step delivered, and what readers saw at each wait.
struct Run {
    summary: crate::TipSummary,
    events: Vec<ChainEvent>,
    waits: usize,
    pre: Arc<ChainReadSnapshot>,
}

/// After `activate_segmented` returns: drains the release's fanout, taking `f`'s events as it
/// goes, as the header writer does before its next step (§3.3).
fn finish_fanout(c: &mut HeaderChain, f: &mut dyn SubscriberFeed) -> Vec<ChainEvent> {
    let mut events = take(f);
    while c.retry_pending() == Publish::Blocked {
        events.extend(take(f));
    }
    events.extend(take(f));
    events
}

/// `activate_segmented` to `to`, with one subscriber that takes at each wait and after the
/// release drains what is left, as the header writer does before its next step (§3.3).
fn run(c: &mut HeaderChain, to: BlockId) -> Run {
    let feeds = c.feeds();
    let (_, mut f) = subscribe(&feeds);
    let reader = c.reader();
    let pre = reader.snapshot();
    let mut events = Vec::new();
    let mut waits = 0;
    let summary = c
        .activate_segmented(bref(c, to), &mut || {
            assert!(Arc::ptr_eq(&reader.snapshot(), &pre), "readers see the pre-step snapshot");
            events.extend(take(&mut *f));
            waits += 1;
        })
        .expect("activates");
    events.extend(finish_fanout(c, &mut *f));
    Run {
        summary,
        events,
        waits,
        pre,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// `segmented_equals_single` (§4.8): the F1 stream of `activate_segmented` equals the
    /// unsegmented composite's event for event; every sub-step is a pure run of at most E
    /// events (m10), so there are ⌈d/E⌉ + ⌈c/E⌉ of them; readers see only the pre- and
    /// post-step snapshots; with Q below E the writer waits between sub-steps.
    #[test]
    fn segmented_equals_single(s in shape(), e in 1..5u32, q in prop_oneof![Just(1u32), Just(2), Just(64)]) {
        let mut reference = spv(1024, 1024);
        let to = build(&mut reference, &s);
        let feeds = reference.feeds();
        let (_, mut f) = subscribe(&feeds);
        reference.tip_plan(to).expect("plan");
        let single = reference.set_active_tip(to).expect("the composite");
        let want = take(&mut *f);

        let mut c = spv(q, e);
        assert_eq!(build(&mut c, &s), to);
        let r = run(&mut c, to);
        prop_assert_eq!(&r.events, &want);
        let (d, cn) = (single.disconnected.len() as u32, single.connected.len() as u32);
        prop_assert_eq!((r.summary.disconnected, r.summary.connected), (d, cn));
        prop_assert_eq!(r.summary.sub_steps, d.div_ceil(e) + cn.div_ceil(e));
        let snap = c.snapshot();
        prop_assert!(Arc::ptr_eq(&snap, &r.summary.snap));
        prop_assert_eq!((snap.tip.tip, snap.tip_seq), (to, single.snap.tip_seq));
        prop_assert_eq!(snap.epoch, r.pre.epoch + 1, "one publication");
        if d + cn == 0 {
            prop_assert_eq!(r.summary.sub_steps, 0);
        }
        let first = if d > 0 { d.min(e) } else { cn.min(e) };
        if r.summary.sub_steps > 1 && first > q {
            prop_assert!(r.waits > 0, "a full target is drained between sub-steps");
        }
        prop_assert!(c.step_budget().peak <= c.step_budget().cap);
    }
}

/// A subscriber wake that records the published `tip_seq` when it fires.
struct SeenSeq {
    reader: crate::ChainReader,
    seen: parking_lot::Mutex<Vec<u64>>,
}

impl Wake for SeenSeq {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.seen.lock().push(self.reader.snapshot().tip_seq);
    }
}

/// `spv_single_segment_publish_before_notify` (m9): the last sub-step fans out after the
/// snapshot store, so a subscriber woken by an event finds `tip_seq` at least its `seq`.
#[test]
fn spv_single_segment_publish_before_notify() {
    let mut c = spv(64, 64);
    let to = build(
        &mut c,
        &Shape {
            trunk: 6,
            tip: 1,
            fork: 1,
            side: 0,
            to: 5,
        },
    );
    let feeds = c.feeds();
    let (_, mut f) = subscribe(&feeds);
    let probe = Arc::new(SeenSeq {
        reader: c.reader(),
        seen: parking_lot::Mutex::new(Vec::new()),
    });
    f.set_waker(Notify::Task(Waker::from(probe.clone())));
    let summary = c.activate_segmented(bref(&c, to), &mut || panic!("no wait")).expect("steps");
    assert_eq!(summary.sub_steps, 1);
    let events = take(&mut *f);
    assert_eq!(events.len(), 4);
    // One wake per run of pushes; each finds the whole step published.
    let seen = probe.seen.lock().clone();
    let last = events.last().expect("events").seq();
    assert!(!seen.is_empty() && seen.iter().all(|&t| t >= last), "{seen:?} vs {last}");
}

/// m2, F10, H7 during a multi-segment hold (`segmented_status_invisible_during_hold`,
/// `segmented_subscribe_returns_hold_active`, `segmented_close_during_hold`): in `wait()` a
/// mark made before stage 4 is not yet visible, `register` is refused, and a full target that
/// deregisters retires; the release applies the mark.
#[test]
fn segmented_hold_waits() {
    let mut c = spv(1, 1);
    let to = build(
        &mut c,
        &Shape {
            trunk: 6,
            tip: 5,
            fork: 1,
            side: 3,
            to: 0,
        },
    );
    let marked = c.reader().snapshot().active.at(6).expect("the tip");
    c.set_status(marked, |s| Status(s.bits() | Status::FAILED_CHILD)).expect("mark");
    let feeds = c.feeds();
    let (t, _f) = subscribe(&feeds);
    let reader = c.reader();
    let mut waits = 0;
    let summary = c
        .activate_segmented(bref(&c, to), &mut || {
            waits += 1;
            assert!(!reader.entry(marked).expect("resident").status.has(Status::FAILED_CHILD));
            assert_eq!(feeds.register(Needs::default()), Err(Refused::WouldDeadlock));
            let elsewhere = std::thread::scope(|s| {
                s.spawn(|| match feeds.register(Needs::default()) {
                    Err(Refused::HoldActive) => {
                        feeds.deregister(t);
                        true
                    }
                    _ => false,
                })
                .join()
                .expect("returns")
            });
            assert!(elsewhere, "HoldActive from another thread");
        })
        .expect("activates");
    assert_eq!(waits, 1, "the closed target is not waited on again");
    assert_eq!((summary.disconnected, summary.connected, summary.sub_steps), (4, 3, 7));
    assert!(feeds.registry().feeds.is_empty(), "the closed target is reclaimed");
    assert!(reader.entry(marked).expect("resident").status.has(Status::FAILED_CHILD));
    feeds.register(Needs::default()).expect("after the release");
}

/// Driver bugs, refused before the hold and before a stage-1 plan is taken: a ref off its
/// block, and the previous step's fanout not drained (§3.3), even with nothing to step.
#[test]
fn activate_segmented_refuses_a_bad_ref_and_a_pending_fanout() {
    let mut c = spv(1, 2);
    let a = branch(&mut c, BlockId(0), 1, 3, T);
    let feeds = c.feeds();
    let (_, mut f) = subscribe(&feeds);
    c.tip_plan(a[1]).expect("plan");
    c.set_active_tip(a[1]).expect("two connects, one left pending at Q = 1");
    assert!(!c.pending_is_empty());
    let pending = |c: &mut HeaderChain, to| c.activate_segmented(to, &mut || {});
    for to in [bref(&c, a[1]), bref(&c, a[2])] {
        let r = pending(&mut c, to);
        assert!(matches!(r, Err(ChainError::Internal("tip step with a pending fanout"))), "{r:?}");
    }
    while c.retry_pending() == Publish::Blocked {
        take(&mut *f);
    }
    c.tip_plan(a[2]).expect("plan");
    let lie = BlockRef {
        height: 1,
        ..bref(&c, a[2])
    };
    let r = c.activate_segmented(lie, &mut || panic!("a step towards a bad ref"));
    assert!(matches!(r, Err(ChainError::Internal(_))), "{r:?}");
    assert!(c.pending_plan.lock().is_some(), "the plan stays");
    c.activate_segmented(bref(&c, a[2]), &mut || {}).expect("its own ref");
}

/// A stage-1 plan to another target is a driver bug.
#[test]
fn activate_segmented_refuses_another_plan() {
    let mut c = spv(8, 2);
    let a = branch(&mut c, BlockId(0), 1, 3, T);
    c.tip_plan(a[1]).expect("plan");
    let r = c.activate_segmented(bref(&c, a[2]), &mut || {});
    assert!(matches!(r, Err(ChainError::Internal(_))), "{r:?}");
    c.tip_plan(a[2]).expect("plan");
    c.activate_segmented(bref(&c, a[2]), &mut || {}).expect("its own target");
}

/// `fence_by_tip_seq` (§4.4's events before publication, m9): an intermediate sub-step's event
/// reaches the subscriber before the snapshot that holds it. The consumer's fence sees
/// `tip_seq < seq`, so it resolves the block by id (immutable fields) and never asks the older
/// snapshot whether the block is active, which would answer no. After the release the snapshot
/// covers every event.
#[test]
fn fence_by_tip_seq() {
    let mut c = spv(1, 1);
    let a = branch(&mut c, BlockId(0), 1, 4, T);
    let to = bref(&c, a[3]);
    let feeds = c.feeds();
    let (_, mut f) = subscribe(&feeds);
    let reader = c.reader();
    let mut fenced = Vec::new();
    c.activate_segmented(to, &mut || {
        for e in take(&mut *f) {
            let snap = reader.snapshot();
            assert!(snap.tip_seq < e.seq(), "the event precedes its snapshot");
            let b = e.block();
            assert!(!snap.active.contains(b.id, &reader), "the older snapshot misjudges it");
            let entry = reader.entry(b.id).expect("resolved by id");
            assert_eq!((entry.hash, entry.height), (b.hash, b.height));
            fenced.push(e);
        }
    })
    .expect("activates");
    let rest = finish_fanout(&mut c, &mut *f);
    assert_eq!((fenced.len(), rest.len()), (2, 2), "two events met the fence inside the hold");
    let snap = reader.snapshot();
    for e in fenced.iter().chain(&rest) {
        assert!(snap.tip_seq >= e.seq() && snap.active.contains(e.block().id, &reader));
    }
}

/// The next event, awaited through the feed's waker.
async fn next_event(f: &mut dyn SubscriberFeed) -> ChainEvent {
    std::future::poll_fn(|cx| {
        f.set_waker(Notify::Task(cx.waker().clone()));
        loop {
            match f.next() {
                Next::Event(_) => return Poll::Ready(f.pop().expect("selected")),
                Next::Control(_) => {}
                Next::Empty => return Poll::Pending,
            }
        }
    })
    .await
}

/// A hub call with §4.3's protocol: `op` answers `None` for `HoldActive`; on one, `on_release`,
/// retry once, then wait for the release wake. `refused` counts the calls that met the hold.
async fn after_hold<T>(
    feeds: &ChainFeeds,
    refused: &AtomicUsize,
    mut op: impl FnMut() -> Option<T>,
) -> T {
    let mut met = false;
    std::future::poll_fn(|cx| {
        if let Some(r) = op() {
            return Poll::Ready(r);
        }
        feeds.on_release(Notify::Task(cx.waker().clone()));
        match op() {
            Some(r) => Poll::Ready(r),
            None => {
                if !std::mem::replace(&mut met, true) {
                    refused.fetch_add(1, Ordering::SeqCst);
                }
                Poll::Pending
            }
        }
    })
    .await
}

/// `spv_hold_subscribe_on_two_workers` (Opus high case 2), on a `current_thread` runtime and on
/// two workers: the header writer runs a four-sub-step `activate_segmented` at Q = 1 on its own
/// thread. Subscriber task L takes one event and then lags, so the writer waits inside the
/// hold. Meanwhile task X deregisters a feed and registers another, and task X2 runs
/// `subscribe_at`: both answer `HoldActive` without blocking a worker, so L is still polled
/// even when the two hub tasks could take both workers. L then takes the rest; after the
/// release wake X registers and X2's handoff is `Moved`. The writer drains the release's
/// fanout after it returns.
#[test]
fn spv_hold_subscribe_on_two_workers() {
    for workers in [1, 2] {
        let rt = match workers {
            1 => tokio::runtime::Builder::new_current_thread().build(),
            n => tokio::runtime::Builder::new_multi_thread().worker_threads(n).build(),
        };
        let rt = rt.expect("a runtime");
        // The deadline is kept off the runtime: a blocked worker would stop its timer too.
        let (done, finished) = std::sync::mpsc::channel();
        let run = std::thread::spawn(move || {
            rt.block_on(subscribe_during_hold());
            let _ = done.send(());
        });
        if finished.recv_timeout(WAIT) == Err(RecvTimeoutError::Timeout) {
            panic!("deadlock on {workers} worker(s)");
        }
        if let Err(p) = run.join() {
            std::panic::resume_unwind(p);
        }
    }
}

async fn subscribe_during_hold() {
    let mut c = spv(1, 1);
    let a = branch(&mut c, BlockId(0), 1, 4, T);
    let to = bref(&c, a[3]);
    let parker = Parker::new();
    c.set_producer_wake(Notify::Thread(parker.clone()));
    let feeds = c.feeds();
    let (_, mut lag) = subscribe(&feeds);
    let doomed = feeds.register(Needs::default()).expect("registers");
    let y = feeds.register(Needs::default()).expect("registers");
    let old_tip = FeedSource::<()>::snapshot(&feeds).tip();
    let pre = c.snapshot().tip_seq;
    let writer = std::thread::spawn(move || {
        let wait = || assert!(parker.park_timeout(WAIT), "a writer wake");
        let s = c.activate_segmented(to, &mut || wait()).expect("activates");
        assert_eq!(s.sub_steps, 4);
        // The last sub-step fans out at the release; if L still holds an event, its push
        // waits for the producer wake, as in the node's loop (§3.3).
        while c.retry_pending() == Publish::Blocked {
            wait();
        }
    });
    let refused = Arc::new(AtomicUsize::new(0));
    let got_one = Arc::new(AtomicBool::new(false));
    let l = tokio::spawn({
        let (refused, got_one) = (refused.clone(), got_one.clone());
        async move {
            let mut seqs = Vec::new();
            while seqs.len() < 4 {
                seqs.push(next_event(&mut *lag).await.seq());
                got_one.store(true, Ordering::SeqCst);
                while refused.load(Ordering::SeqCst) < 2 {
                    tokio::task::yield_now().await;
                }
            }
            (seqs, lag)
        }
    });
    // L holds its first event, so the writer is inside the hold until X and X2 are refused.
    let hub = |task: u8| {
        let (feeds, refused, got_one) = (feeds.clone(), refused.clone(), got_one.clone());
        tokio::spawn(async move {
            while !got_one.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            if task == 0 {
                feeds.deregister(doomed);
                after_hold(&feeds, &refused, || match feeds.register(Needs::default()) {
                    Err(Refused::HoldActive) => None,
                    r => Some(r.expect("registers")),
                })
                .await;
            } else {
                let r = after_hold(&feeds, &refused, || {
                    match feeds.subscribe_at(y, &old_tip, &mut || Ok::<(), ()>(())) {
                        Err(SubscribeError::HoldActive) => None,
                        r => Some(r),
                    }
                })
                .await;
                assert!(matches!(r, Ok(Handoff::Moved)), "the tip moved under X2");
            }
        })
    };
    let (x, x2) = (hub(0), hub(1));
    let (seqs, _lag) = l.await.expect("L");
    x.await.expect("X");
    x2.await.expect("X2");
    writer.join().expect("the writer");
    assert_eq!(seqs, (pre + 1..=pre + 4).collect::<Vec<_>>());
    assert_eq!(refused.load(Ordering::SeqCst), 2, "X and X2 met the hold");
    let n = feeds.registry().feeds.len();
    assert_eq!(n, 3, "L, X's and X2's feeds; the deregistered one reclaimed");
}

/// `segmented_cold_error_mid_hold` (m3): a store fault in sub-step 2's cold leg read trips the
/// latch and abandons the hold: sub-step 1's events were delivered, but no snapshot, status or
/// side effect follows, so readers never see the intermediate tip.
#[test]
fn segmented_cold_error_mid_hold() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let mut cfg = ChainConfig::spv(&p, 4);
    cfg.feeds = FeedConfig {
        capacity: 1,
        segment_events: 4,
        ..cfg.feeds
    };
    let mut store = MemHeaderStore::new();
    let svc = services(&p);
    let mut c = Spv::new_anchored(p.clone(), cfg, svc, store.clone(), &pack).expect("a pack");
    store.reset(meta(pack.hash, pack.height)).expect("reset");
    stages(&mut c, &mut store);
    let a = mine_n(&p, pack.hash, T0 + 10_000, 12);
    real_step(&mut c, &mut store, &a);
    // Back to a[1], d = 10 at E = 4: sub-step 2's legs are below the window (w = 4), so cold.
    let a1 = c.reader().try_lookup(&a[1].hash()).expect("read").expect("held");
    let to = c.block_ref_of(a1).expect("a[1]");
    assert_eq!(to.hash, a[1].hash());

    let feeds = c.feeds();
    let (_, mut f) = subscribe(&feeds);
    let reader = c.reader();
    let pre = reader.snapshot();
    let mut status = c.status_feed();
    status.next_events();
    let mut delivered = Vec::new();
    let c_shared = c.shared.clone();
    let r = c.activate_segmented(to, &mut || {
        delivered.extend(take(&mut *f));
        c_shared.invalidate_cold(ColdInvalidation::Clear);
        store.faults(|x| {
            x.fail_read(ReadKind::ById, 1);
            x.fail_read(ReadKind::ByHash, 1);
        });
    });
    delivered.extend(take(&mut *f));
    assert!(matches!(r, Err(ChainError::Storage(_))), "{r:?}");
    assert!(c.svc.fail.is_tripped());
    assert!(Arc::ptr_eq(&reader.snapshot(), &pre), "nothing published after the trip");
    assert!(status.next_events().is_empty(), "no status published");
    assert_eq!(delivered.len(), 4, "sub-step 1 was delivered");
}

/// DEC-174 inside a hold: each sub-step windows SPV's active chain, so a reorg far below the
/// window and longer than it stays within `w + 1` heights throughout (checked after every
/// sub-step), its disconnects re-extending the chain by ancestry. The release leaves the base
/// at `tip − w` and the chain steps on from there.
#[test]
fn segmented_windows_each_sub_step() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let mut cfg = ChainConfig::spv(&p, 4);
    cfg.feeds.segment_events = 3;
    let mut store = MemHeaderStore::new();
    let svc = services(&p);
    let mut c = Spv::new_anchored(p.clone(), cfg, svc, store.clone(), &pack).expect("a pack");
    store.reset(meta(pack.hash, pack.height)).expect("reset");
    stages(&mut c, &mut store);
    let trunk = mine_n(&p, pack.hash, T0 + 10_000, 20);
    for chunk in trunk.chunks(5) {
        real_step(&mut c, &mut store, chunk);
    }
    let old = c.private_tip().expect("a tip").0;
    assert_eq!((old.height, c.active.base()), (pack.height + 20, pack.height + 16));

    // Off trunk[2], 17 below the tip: 30 headers outweigh the trunk's 17 above the fork.
    let side = mine_n(&p, trunk[2].hash(), T0 + 20_000, 30);
    c.accept(batch(&side), None).expect("accept");
    let (_, target) = drive(&mut c, &mut store, |_, _| {}, Stage::Commit).expect("committed");
    let to = c.block_ref_of(target.id).expect("the side's tip");
    assert_eq!(to.hash, side[29].hash());
    let s = c.activate_segmented(to, &mut || {}).expect("activates");
    assert_eq!((s.disconnected, s.connected, s.sub_steps), (17, 30, 6 + 10));
    c.records_committed();

    let tip = c.private_tip().expect("a tip").0;
    assert_eq!((tip.id, tip.height), (to.id, pack.height + 33));
    assert_eq!((c.active.base(), c.active.len()), (tip.height - 4, tip.height + 1));
    assert_eq!(c.snapshot().active.base(), tip.height - 4);
    assert_eq!(c.ancestor_private(tip.height - 5), None, "below the window");
    let more = mine_n(&p, side[29].hash(), T0 + 30_000, 3);
    real_step(&mut c, &mut store, &more);
    assert_eq!(c.active.base(), tip.height + 3 - 4);
}

/// §6.2: the cap from the real sizes is the design's on x86-64, and a sub-step's charge is its
/// legs plus their pending copy when a target lacked them.
#[test]
fn step_budget_cap_and_charges() {
    assert_eq!(step_cap(&FeedConfig::spv(), 1024), 433_104);
    assert_eq!(step_cap(&FeedConfig::spv_mobile(), 256), 202_704);
    // Connects only: SB4 reserves their legs at exact capacity.
    let mut c = spv(1, 2);
    let to = build(
        &mut c,
        &Shape {
            trunk: 6,
            tip: 0,
            fork: 0,
            side: 0,
            to: 5,
        },
    );
    assert_eq!(run(&mut c, to).summary.sub_steps, 3);
    let b = c.step_budget();
    let bref_size = size_of::<BlockRef>();
    assert_eq!(b.peak, bref_size * (2 + 2), "two legs and their pending copy");
    assert!(b.peak <= b.cap);
    assert_eq!(b.cap, step_cap(&c.cfg.feeds, c.cfg.cold_cache));

    // Disconnects only: exact capacity too (SB4's `walk_down`).
    let mut c = spv(1, 2);
    let shape = Shape {
        trunk: 6,
        tip: 5,
        fork: 0,
        side: 0,
        to: 1,
    };
    let to = build(&mut c, &shape);
    assert_eq!(run(&mut c, to).summary.sub_steps, 2);
    assert_eq!(c.step_budget().peak, bref_size * (2 + 2));

    // Past the sub-step term, a change is `Internal`.
    let mut c = spv(1, 2);
    let a = branch(&mut c, BlockId(0), 1, 2, T);
    let mut hold = c.hold_tip();
    let mut change = hold.set_active_tip(a[1], 2).expect("two connects");
    assert_eq!(check_sub_step(&change, 2), Ok(()));
    change.connected.reserve_exact(5);
    assert!(matches!(check_sub_step(&change, 2), Err(ChainError::Internal(_))));
}

/// `budget_overrun_is_internal_error` (§10.4): a sub-step charged over §6.2's sub-step term (its
/// connect leg inflated, after the sub-step moved the private tip) is `Internal`; the latch
/// trips, the hold is abandoned, and nothing is published or fanned out. A batch over
/// `step_headers` is `Config` before any work. The A5 list and the fixed term have no runtime
/// check: the list clears past C, and the fixed term's containers are sized by construction.
#[test]
fn budget_overrun_is_internal_error() {
    const E: u32 = 4;
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let mut cfg = ChainConfig::spv(&p, 16);
    cfg.feeds.segment_events = E;
    cfg.feeds.step_headers = 8;
    let mut store = MemHeaderStore::new();
    let svc = services(&p);
    let mut c = Spv::new_anchored(p.clone(), cfg, svc, store.clone(), &pack).expect("a pack");
    store.reset(meta(pack.hash, pack.height)).expect("reset");
    stages(&mut c, &mut store);
    let a = mine_n(&p, pack.hash, T0 + 10_000, 9);
    let r = c.accept(batch(&a), None);
    assert!(matches!(r, Err(ChainError::Config(crate::ConfigError::StepHeaders { .. }))), "{r:?}");
    assert_eq!(c.reader().try_lookup(&a[0].hash()).expect("read"), None, "nothing accepted");
    real_step(&mut c, &mut store, &a[..2]);

    let feeds = c.feeds();
    let (_, mut f) = subscribe(&feeds);
    let reader = c.reader();
    let pre = reader.snapshot();
    let mut status = c.status_feed();
    status.next_events();
    let before = c.step_budget();
    c.accept(batch(&a[2..]), None).expect("accept");
    let (_, target) = drive(&mut c, &mut store, |_, _| {}, Stage::Commit).expect("committed");
    let to = c.block_ref_of(target.id).expect("a[8]");
    // 3 · E slots of spare capacity: the legs alone pass the term.
    c.feed_shared().faults.inflate_legs.store(3 * E as usize, Ordering::Relaxed);
    let r = c.activate_segmented(to, &mut || {});
    assert!(
        matches!(r, Err(ChainError::Internal("step budget: a sub-step past its term"))),
        "{r:?}"
    );
    assert!(c.svc.fail.is_tripped(), "the private tip had moved");
    assert!(Arc::ptr_eq(&reader.snapshot(), &pre), "nothing published");
    assert!(status.next_events().is_empty(), "no status published");
    assert!(take(&mut *f).is_empty(), "nothing fanned out");
    assert_eq!(c.step_budget(), before, "the charge was refused, not recorded");
}
