//! F2's tests (SB5 §10.1, §10.3): the table under a model tip lock — fanout, cursors and
//! `retry_pending`, `register`, `subscribe_at`, `deregister` with the lazy reclaim — and the P1
//! model; `chain_tests.rs` runs the same feeds on SB4's `HeaderChain`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use proptest::prelude::*;

use super::test_support::{Model, block, cfg, ok_finish};
use super::*;
use crate::notify::Parker;
use crate::testing::Latch;
use crate::tip::BlockRef;

/// Pops every queued event (controls are skipped).
fn drain(f: &mut dyn SubscriberFeed) -> Vec<ChainEvent> {
    let mut out = Vec::new();
    loop {
        match f.next() {
            Next::Control(_) => continue,
            Next::Event(_) => out.push(f.pop().expect("selected")),
            Next::Empty => return out,
        }
    }
}

fn seqs(es: &[ChainEvent]) -> Vec<u64> {
    es.iter().map(|e| e.seq()).collect()
}

#[test]
fn config_validation_refuses_zero_counts_and_too_many_feeds() {
    assert_eq!(FeedConfig::node().validate(), Ok(()));
    assert_eq!(FeedConfig::spv_mobile().validate(), Ok(()));
    let zero = FeedConfig {
        capacity: 0,
        ..FeedConfig::node()
    };
    assert_eq!(zero.validate(), Err(FeedConfigError::Zero("capacity")));
    let zero = FeedConfig {
        max_feeds: 0,
        ..FeedConfig::spv()
    };
    assert_eq!(zero.validate(), Err(FeedConfigError::Zero("max_feeds")));
    let most = FeedConfig {
        max_feeds: MAX_FEEDS,
        ..FeedConfig::spv()
    };
    assert_eq!(most.validate(), Ok(()));
    let past = FeedConfig {
        max_feeds: MAX_FEEDS + 1,
        ..FeedConfig::spv()
    };
    assert_eq!(past.validate(), Err(FeedConfigError::MaxFeeds(MAX_FEEDS + 1)));
}

/// P8 and the latch (§4.3 `register` step 1, `subscribe_at`).
#[test]
fn register_policy_and_latch() {
    let latch = Arc::new(Latch::default());
    let m = Model::with_latch(FeedConfig::spv(), latch.clone());
    let block = Needs {
        block: true,
        undo: false,
    };
    let undo = Needs {
        block: false,
        undo: true,
    };
    assert_eq!(m.handle.register(block), Err(Refused::NoBlockData));
    assert_eq!(m.handle.register(undo), Err(Refused::NoUndo));
    let id = m.handle.register(Needs::default()).expect("SPV feeds need no block data");

    let node = Model::new(cfg(4));
    assert!(node.handle.register(block).is_ok() && node.handle.register(undo).is_ok());

    latch.read_failed("test");
    assert_eq!(m.handle.register(Needs::default()), Err(Refused::Latched));
    let tip = m.tip.lock().ctx.tip;
    assert!(matches!(
        m.handle.subscribe_at(id, &tip, &mut ok_finish()),
        Err(SubscribeError::Latched)
    ));
}

/// F10: a feed subscribed after steps goes live at `next_seq`. Its first event is the handoff's
/// `seq_synced`, and the registry's `seq_synced` is the same (review F-3: at a fresh table,
/// seq 1 hides an off-by-one).
#[test]
fn registry_seq_synced_is_the_handoffs() {
    let m = Model::new(cfg(8));
    {
        let mut g = m.tip.lock();
        for k in 1..=4 {
            g.connect(&m.shared, block(k));
        }
    }
    let (id, mut feed, seq_synced) = m.subscribe();
    assert_eq!(seq_synced, 5);
    let registry = m.shared.registry.load();
    let entry = registry.feeds.iter().find(|e| e.id == id).expect("registered");
    assert_eq!(entry.seq_synced, Some(seq_synced));
    m.tip.lock().connect(&m.shared, block(5));
    assert_eq!(seqs(&drain(&mut *feed)), [seq_synced]);
}

/// `subscribe_at`'s refusals change nothing; a failed `finish` leaves the slot inactive (F10).
#[test]
fn subscribe_at_refusals() {
    let m = Model::new(cfg(4));
    let id = m.handle.register(Needs::default()).expect("registers");
    let tip = m.tip.lock().ctx.tip;
    let stale = FeedId {
        generation: id.generation + 1,
        ..id
    };
    assert!(matches!(
        m.handle.subscribe_at(stale, &tip, &mut ok_finish()),
        Err(SubscribeError::Stale)
    ));

    let mut called = false;
    let moved = m.handle.subscribe_at(id, &block(9), &mut || {
        called = true;
        Ok::<(), ()>(())
    });
    assert!(matches!(moved, Ok(Handoff::Moved)) && !called, "Moved runs no finish");

    m.tip.lock().ctx.pre_genesis = true;
    assert!(matches!(m.handle.subscribe_at(id, &tip, &mut ok_finish()), Ok(Handoff::Moved)));
    m.tip.lock().ctx.pre_genesis = false;

    let failed = m.handle.subscribe_at(id, &tip, &mut || Err("disk"));
    assert!(matches!(failed, Err(SubscribeError::Finish("disk"))));
    assert_eq!(m.handle.shared.registry.load().feeds[0].seq_synced, None, "still inactive");

    let live = m.handle.subscribe_at(id, &tip, &mut ok_finish());
    let Ok(Handoff::Live {
        seq_synced,
        feed,
    }) = live
    else {
        panic!("live")
    };
    assert_eq!(seq_synced, 1);
    assert!(matches!(
        m.handle.subscribe_at(id, &tip, &mut ok_finish()),
        Err(SubscribeError::AlreadyLive)
    ));
    let id2 = m.handle.register(Needs::default()).expect("registers");
    m.handle.deregister(id2);
    assert!(matches!(
        m.handle.subscribe_at(id2, &tip, &mut ok_finish()),
        Err(SubscribeError::Stale)
    ));
    drop(feed);
}

/// A closing slot that is not yet reclaimed refuses with `Closing`.
#[test]
fn subscribe_at_closing() {
    let m = Model::new(cfg(4));
    let id = m.handle.register(Needs::default()).expect("registers");
    let tip = m.tip.lock().ctx.tip;
    {
        // Step 4 loses the try_lock: the entry stays until a scan reclaims it.
        let _held = m.tip.lock();
        m.handle.deregister(id);
    }
    assert!(m.handle.is_closing(id));
    assert!(matches!(
        m.handle.subscribe_at(id, &tip, &mut ok_finish()),
        Err(SubscribeError::Closing)
    ));
}

/// X28 (§10.1): S1 full at 7; S2 subscribes at the tip with `seq_synced` 8 and never sees 7.
#[test]
fn x28_pending_at_handoff() {
    let m = Model::new(cfg(1));
    {
        let mut g = m.tip.lock();
        for k in 1..=5 {
            g.connect(&m.shared, block(k));
        }
    }
    let (_, mut s1, synced1) = m.subscribe();
    assert_eq!(synced1, 6);
    let mut g = m.tip.lock();
    assert_eq!(g.connect(&m.shared, block(6)).1, Publish::Done);
    let (e7, p) = g.connect(&m.shared, block(7));
    assert_eq!((e7.seq(), p), (7, Publish::Blocked), "S1 is full: cursor at 7");
    drop(g);

    let (id2, mut s2, synced2) = m.subscribe();
    assert_eq!(synced2, 8);
    assert_eq!(m.tip.lock().feeds.cursor(id2), None, "a new slot has no cursor");

    assert_eq!(seqs(&drain(&mut *s1)), [6]);
    let mut g = m.tip.lock();
    assert_eq!(g.retry(&m.shared), Publish::Done, "7 goes to S1 only");
    assert_eq!(g.connect(&m.shared, block(8)).1, Publish::Blocked, "S1 holds 7");
    drop(g);
    assert_eq!(seqs(&drain(&mut *s2)), [8]);
    assert_eq!(seqs(&drain(&mut *s1)), [7]);
    assert_eq!(m.tip.lock().retry(&m.shared), Publish::Done);
    assert_eq!(seqs(&drain(&mut *s1)), [8]);
    assert!(drain(&mut *s2).is_empty(), "S2 never sees 7");
}

/// F1 (§10.1): a step taken while S holds a cursor reaches S only through `pending`, after the
/// earlier backlog: S never receives k+1 before k, even with space when the step fans out. The
/// node and SPV writers drain between steps (§4.7, §4.8); the table must not rely on it.
#[test]
fn f1_cursor_order() {
    let m = Model::new(cfg(1));
    let (_, mut s, _) = m.subscribe();
    let mut g = m.tip.lock();
    g.connect(&m.shared, block(1));
    assert_eq!(g.connect(&m.shared, block(2)).1, Publish::Blocked, "cursor at 2");
    drop(g);
    assert_eq!(seqs(&drain(&mut *s)), [1]);
    assert_eq!(m.tip.lock().connect(&m.shared, block(3)).1, Publish::Blocked);
    assert!(drain(&mut *s).is_empty(), "3 waits behind 2");
    assert_eq!(m.tip.lock().retry(&m.shared), Publish::Blocked);
    assert_eq!(seqs(&drain(&mut *s)), [2]);
    assert_eq!(m.tip.lock().retry(&m.shared), Publish::Done);
    assert_eq!(seqs(&drain(&mut *s)), [3]);
    assert!(m.tip.lock().feeds.pending_is_empty());
}

/// F11: a `deregister` whose step 4 wins the tip lock clears the last cursor and drops the
/// backlog with it; the writer's next step is `Done`, not a spurious `Blocked`.
#[test]
fn f11_step4_drops_the_backlog() {
    let m = Model::new(cfg(1));
    let (id, _s, _) = m.subscribe();
    let mut g = m.tip.lock();
    g.connect(&m.shared, block(1));
    assert_eq!(g.connect(&m.shared, block(2)).1, Publish::Blocked);
    drop(g);
    m.handle.deregister(id);
    let mut g = m.tip.lock();
    assert!(g.feeds.pending_is_empty(), "no cursor needs 2");
    g.feeds.assert_consistent();
    assert_eq!(g.connect(&m.shared, block(3)).1, Publish::Done);
    assert!(g.feeds.pending_is_empty());
}

/// X27 (§10.1), on the model: after a reorg `subscribe_at(A3)` is `Moved` and runs no finish.
#[test]
fn x27_moved_on_reorg() {
    let m = Model::new(cfg(8));
    let a3 = block(3);
    {
        let mut g = m.tip.lock();
        for k in 1..=3 {
            g.connect(&m.shared, block(k));
        }
        for k in (0..3).rev() {
            g.disconnect(&m.shared, block(k));
        }
        for k in 101..=103 {
            g.connect(&m.shared, block(k));
        }
    }
    let id = m.handle.register(Needs::default()).expect("registers");
    let mut called = false;
    let r = m.handle.subscribe_at(id, &a3, &mut || {
        called = true;
        Ok::<(), ()>(())
    });
    assert!(matches!(r, Ok(Handoff::Moved)) && !called);
    let b3 = m.tip.lock().ctx.tip;
    assert!(matches!(m.handle.subscribe_at(id, &b3, &mut ok_finish()), Ok(Handoff::Live { .. })));
}

/// F11 (§10.1): three full targets with cursors at 5; one deregisters; `retry_pending` clears
/// its cursor and the writer finishes once the other two take.
#[test]
fn f11_dereg_with_pending_fanout() {
    let m = Model::new(cfg(1));
    let mut subs: Vec<_> = (0..3).map(|_| m.subscribe()).collect();
    let mut g = m.tip.lock();
    g.connect(&m.shared, block(1));
    for k in 2..=4 {
        // The writer's rule: drain before the next step (each target holds one event).
        assert_eq!(g.connect(&m.shared, block(k)).1, Publish::Blocked);
        drop(g);
        for (_, f, _) in &mut subs {
            assert_eq!(drain(&mut **f).len(), 1);
        }
        g = m.tip.lock();
        assert_eq!(g.retry(&m.shared), Publish::Done);
    }
    assert_eq!(g.connect(&m.shared, block(5)).1, Publish::Blocked);
    for (id, _, _) in &subs {
        assert_eq!(g.feeds.cursor(*id), Some(5));
    }
    drop(g);

    let (gone, mut gone_feed, _) = subs.remove(0);
    let gone_used = |m: &Model| {
        m.shared.registry.load().feeds.iter().find(|e| e.id == gone).map(|e| e.slot.counts().0)
    };
    assert_eq!(gone_used(&m), Some(1));
    // The writer holds the tip lock, so step 4 loses and `retry_pending` clears the cursor.
    let mut g = m.tip.lock();
    m.handle.deregister(gone);
    assert!(gone_feed.is_closed());
    assert_eq!(g.feeds.cursor(gone), Some(5), "step 4 lost the tip lock");
    assert_eq!(g.retry(&m.shared), Publish::Blocked, "the other two still lack 5");
    assert_eq!(g.feeds.cursor(gone), None, "the closing target's cursor is cleared");
    assert_eq!(g.feeds.occupied(), 2, "and its entry reclaimed");
    drop(g);
    for (_, f, _) in &mut subs {
        assert_eq!(seqs(&drain(&mut **f)), [4]);
    }
    assert_eq!(m.tip.lock().retry(&m.shared), Publish::Done);
    for (_, f, _) in &mut subs {
        assert_eq!(seqs(&drain(&mut **f)), [5]);
    }
    assert_eq!(seqs(&drain(&mut *gone_feed)), [4], "nothing after deregister");
}

/// F11 (§10.1): A's id is stale once B holds the index; A's reader sees nothing of B's.
#[test]
fn f11_slot_reuse_stale_gen() {
    let m = Model::new(cfg(1).with_max_feeds(1));
    let (a, mut a_feed, _) = m.subscribe();
    m.handle.deregister(a);
    let (b, mut b_feed, _) = m.subscribe();
    assert_eq!((b.index, b.generation), (a.index, a.generation + 1));

    assert!(m.handle.is_closing(a), "a stale id reads as closing");
    assert_eq!(
        m.handle.send_control(a, ControlMsg::new(ControlKind::new(0), 1u8)),
        SendControl::Stale
    );
    assert_eq!(m.handle.control_outstanding(a), 0);
    m.handle.deregister(a);
    assert!(!m.handle.is_closing(b), "A's deregister is a no-op on B");
    let tip = m.tip.lock().ctx.tip;
    assert!(matches!(m.handle.subscribe_at(a, &tip, &mut ok_finish()), Err(SubscribeError::Stale)));

    assert_eq!(m.tip.lock().connect(&m.shared, block(1)).1, Publish::Done);
    assert!(drain(&mut *a_feed).is_empty());
    assert_eq!(seqs(&drain(&mut *b_feed)), [1]);
}

/// F11 (Sol r1, §10.1): A's `deregister` pauses before step 4; B's `register` reclaims A's entry
/// and takes its index; A's step 4 then finds B (`ptr_eq` fails) and removes nothing.
#[test]
fn f11_delayed_cleanup_after_reuse() {
    for negative in [false, true] {
        let m = Model::new(cfg(4).with_max_feeds(1));
        m.tip.lock().feeds.faults.cleanup_without_ptr_eq = negative;
        let (a, a_feed, _) = m.subscribe();
        m.shared.faults.before_step4.arm();
        let dereg = {
            let m = m.clone();
            thread::spawn(move || m.handle.deregister(a))
        };
        m.shared.faults.before_step4.wait_paused();
        let (b, mut b_feed, _) = m.subscribe();
        assert_eq!(b.index, a.index, "B reuses A's index after the lazy reclaim");
        m.shared.faults.before_step4.release();
        dereg.join().expect("deregister returns");

        let reg = m.shared.registry.load();
        let b_kept = reg.feeds.iter().any(|e| e.id == b);
        assert_eq!(m.tip.lock().connect(&m.shared, block(1)).1, Publish::Done);
        if negative {
            assert!(!b_kept, "the planted cleanup without ptr_eq removes B");
        } else {
            assert!(b_kept, "A's step 4 leaves B alone");
            assert_eq!(seqs(&drain(&mut *b_feed)), [1], "B keeps receiving events");
        }
        drop(a_feed);
    }
}

/// F11 (§10.1): a closing entry whose step 4 lost the `try_lock` is reclaimed by the writer's
/// next scan, and `register` at `max_feeds` reclaims before reporting `TableFull`.
#[test]
fn f11_lazy_reclaim_before_table_full() {
    let m = Model::new(cfg(2).with_max_feeds(2));
    let (a, _a_feed, _) = m.subscribe();
    let (b, _b_feed, _) = m.subscribe();
    assert_eq!(m.handle.register(Needs::default()), Err(Refused::TableFull));
    {
        let _held = m.tip.lock();
        m.handle.deregister(a);
        m.handle.deregister(b);
    }
    assert_eq!(m.tip.lock().feeds.occupied(), 2, "step 4 lost the try_lock");
    assert_eq!(m.shared.registry.load().feeds.len(), 2, "W filters by is_closing (H7(b))");
    assert!(m.shared.registry.load().feeds.iter().all(|e| e.is_closing()));

    // The writer's scan reclaims...
    m.tip.lock().connect(&m.shared, block(1));
    assert_eq!(m.tip.lock().feeds.occupied(), 0);
    assert!(m.shared.registry.load().feeds.is_empty());

    // ...and so does a `register` at max_feeds.
    let (c, _c_feed, _) = m.subscribe();
    let (_d, _d_feed, _) = m.subscribe();
    {
        let _held = m.tip.lock();
        m.handle.deregister(c);
    }
    let e = m.handle.register(Needs::default()).expect("the closing entry is reclaimed first");
    assert_eq!(e.index, c.index);
}

/// A spent generation retires its index for good (§3.2).
#[test]
fn spent_generation_retires_the_index() {
    let m = Model::new(cfg(1).with_max_feeds(1));
    let id = m.handle.register(Needs::default()).expect("registers");
    m.tip.lock().feeds.retire_spent(id);
    assert_eq!(m.handle.register(Needs::default()), Err(Refused::TableFull));
}

/// An SPV composite fans out disconnects tip-down then connects fork-up, partially at Q, and
/// `retry_pending` continues from the cursor inside the held segment, which takes the slot the
/// table reserved (§6.2's fixed term holds no segment).
#[test]
fn composite_fanout_and_retry() {
    let m = Model::new(cfg(2));
    let (_, mut f, synced) = m.subscribe();
    assert_eq!(synced, 1);
    let down = [block(3), block(2)];
    let up = [block(12), block(13), block(14)];
    let mut g = m.tip.lock();
    let reserved = g.feeds.pending_capacity();
    assert!(reserved >= 1);
    g.ctx.next_seq = 6;
    let p = g.feeds.fanout(
        &m.shared,
        Fanout::Composite {
            first_seq: 1,
            disconnected: &down,
            connected: &up,
        },
    );
    assert_eq!(p, Publish::Blocked);
    assert_eq!((g.feeds.pending_segments(), g.feeds.pending_capacity()), (1, reserved));
    drop(g);
    let mut got = drain(&mut *f);
    loop {
        if m.tip.lock().retry(&m.shared) == Publish::Done {
            got.extend(drain(&mut *f));
            break;
        }
        got.extend(drain(&mut *f));
    }
    let want: Vec<ChainEvent> = vec![
        TipStep::Disconnected {
            block: block(3),
            seq: 1,
        },
        TipStep::Disconnected {
            block: block(2),
            seq: 2,
        },
        TipStep::Connected {
            block: block(12),
            seq: 3,
        },
        TipStep::Connected {
            block: block(13),
            seq: 4,
        },
        TipStep::Connected {
            block: block(14),
            seq: 5,
        },
    ];
    assert_eq!(got, want);
    assert!(m.tip.lock().feeds.pending_is_empty());
}

/// DEC-157 (2) at the handle: on the hold owner's thread `WouldDeadlock`, elsewhere
/// `HoldActive`; `on_release`, then the retry (register-then-retry: no lost wake).
#[test]
fn hold_owner_refused_and_release_wake() {
    let m = Model::new(cfg(2));
    let id = m.handle.register(Needs::default()).expect("registers");
    let hold = m.tip.lock();
    m.shared.hold_opened();
    assert_eq!(m.handle.register(Needs::default()), Err(Refused::WouldDeadlock));
    assert!(matches!(
        m.handle.subscribe_at(id, &block(0), &mut ok_finish()),
        Err(SubscribeError::WouldDeadlock)
    ));

    let waiter = {
        let m = m.clone();
        thread::spawn(move || {
            let p = Parker::new();
            loop {
                match m.handle.register(Needs::default()) {
                    Err(Refused::HoldActive) => {
                        m.handle.shared.on_release(Notify::Thread(p.clone()));
                        match m.handle.register(Needs::default()) {
                            Err(Refused::HoldActive) => p.park(),
                            r => return r,
                        }
                    }
                    r => return r,
                }
            }
        })
    };
    thread::sleep(Duration::from_millis(20));
    m.shared.hold_closing();
    drop(hold);
    m.shared.fire_release_wakes();
    assert!(waiter.join().expect("joins").is_ok(), "register completes after the release");
}

/// H2 under contention: the production `ArcSwap` registry while 4 threads register and
/// deregister: every load is a consistent table and epochs never go back.
#[test]
fn registry_publish_stress() {
    let m = Model::new(cfg(1).with_max_feeds(8));
    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (m, stop) = (m.clone(), stop.clone());
            thread::spawn(move || {
                let mut last = 0;
                let mut loads = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let reg = m.shared.registry.load();
                    assert!(reg.epoch >= last, "epochs never go back");
                    last = reg.epoch;
                    let mut idx: Vec<u32> = reg.feeds.iter().map(|e| e.id.index).collect();
                    idx.dedup();
                    assert_eq!(idx.len(), reg.feeds.len(), "one entry per index, in order");
                    loads += 1;
                }
                loads
            })
        })
        .collect();
    let writers: Vec<_> = (0..4)
        .map(|_| {
            let m = m.clone();
            thread::spawn(move || {
                for _ in 0..2_000 {
                    let id =
                        m.handle.register(Needs::default()).expect("closing entries are reclaimed");
                    m.handle.deregister(id);
                }
            })
        })
        .collect();
    for w in writers {
        w.join().expect("writer");
    }
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        assert!(r.join().expect("reader") > 0);
    }
}

// ---- T35 (§10.1): subscribe stress against a reorging writer ---------------------------------

/// Unique blocks: id `k`, height as given.
fn blk(k: u32, height: u32) -> BlockRef {
    BlockRef {
        height,
        ..block(k)
    }
}

/// The writer's published state for catch-up: the active chain and its `tip_seq`.
type PublishedChain = std::sync::Mutex<(Arc<Vec<BlockRef>>, u64)>;

/// 16 subscribers catch up from the published chain and `subscribe_at`, retried on `Moved`,
/// while one writer reorgs (depth ≤ 20). Every local chain ends equal to the writer's, and each
/// subscriber's events are consecutive from `seq_synced` (no `FeedOrder`).
#[test]
fn t35_subscribe_stress() {
    let steps: u64 =
        std::env::var("SB5_T35_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(3_000);
    for q in [1u32, 2, 8] {
        t35(q, steps);
    }
}

fn t35(q: u32, steps: u64) {
    const SUBS: usize = 16;
    let m = Model::new(cfg(q).with_max_feeds(SUBS as u32));
    let published: Arc<PublishedChain> =
        Arc::new(std::sync::Mutex::new((Arc::new(vec![blk(0, 0)]), 0)));
    let done_seq = Arc::new(AtomicU64::new(u64::MAX));
    let producer = Parker::new();
    m.shared.set_producer_wake(Notify::Thread(producer.clone()));
    let start = Arc::new(Barrier::new(SUBS + 1));

    let subs: Vec<_> = (0..SUBS)
        .map(|_| {
            let (m, published, done_seq, start) =
                (m.clone(), published.clone(), done_seq.clone(), start.clone());
            thread::spawn(move || {
                start.wait();
                let id = m.handle.register(Needs::default()).expect("16 fit");
                let (mut local, mut feed, mut last) = loop {
                    let (chain, tip_seq) = {
                        let p = published.lock().expect("published");
                        (p.0.clone(), p.1)
                    };
                    let tip = *chain.last().expect("a tip");
                    match m.handle.subscribe_at(id, &tip, &mut ok_finish()) {
                        Ok(Handoff::Live {
                            feed,
                            seq_synced,
                        }) => {
                            // `seq_synced` may exceed `tip_seq + 1` when the tip went away and
                            // came back (ABA): the chain to that tip is the same one.
                            assert!(seq_synced > tip_seq, "the cut is at or after the snapshot");
                            break ((*chain).clone(), feed, seq_synced - 1);
                        }
                        Ok(Handoff::Moved) => thread::yield_now(),
                        Err(e) => panic!("subscribe_at: {e:?}"),
                    }
                };
                let me = Parker::new();
                feed.set_waker(Notify::Thread(me.clone()));
                while last != done_seq.load(Ordering::SeqCst) {
                    match feed.next() {
                        Next::Event(_) => {
                            let e = feed.pop().expect("selected");
                            assert_eq!(e.seq(), last + 1, "consecutive from the cut");
                            last = e.seq();
                            match e {
                                TipStep::Connected {
                                    block,
                                    ..
                                } => {
                                    assert_eq!(block.height as usize, local.len());
                                    local.push(block);
                                }
                                TipStep::Disconnected {
                                    block,
                                    ..
                                } => assert_eq!(local.pop(), Some(block)),
                            }
                        }
                        Next::Control(_) => unreachable!("no controls here"),
                        Next::Empty => {
                            if let Next::Empty = feed.next() {
                                me.park_timeout(Duration::from_millis(5));
                            }
                        }
                    }
                }
                local
            })
        })
        .collect();

    start.wait();
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut chain = vec![blk(0, 0)];
    let mut k = 1u32;
    let mut taken = 0u64;
    while taken < steps {
        let depth = (next() % 21) as usize;
        let depth = if chain.len() > depth + 1 && next() % 4 == 0 {
            depth
        } else {
            0
        };
        let grow = depth + 1 + (next() % 3) as usize;
        let mut ops: Vec<Option<BlockRef>> = vec![None; depth];
        let base = chain.len() - depth;
        for i in 0..grow {
            ops.push(Some(blk(k, (base + i) as u32)));
            k += 1;
        }
        for op in ops {
            // The writer's rule: no step while a target lacks events.
            loop {
                if m.tip.lock().retry(&m.shared) == Publish::Done {
                    break;
                }
                producer.park_timeout(Duration::from_millis(5));
            }
            let mut g = m.tip.lock();
            match op {
                Some(b) => {
                    chain.push(b);
                    g.connect(&m.shared, b);
                }
                None => {
                    chain.pop();
                    g.disconnect(&m.shared, *chain.last().expect("above genesis"));
                }
            }
            taken += 1;
            *published.lock().expect("published") = (Arc::new(chain.clone()), g.ctx.next_seq - 1);
        }
    }
    let final_seq = m.tip.lock().ctx.next_seq - 1;
    done_seq.store(final_seq, Ordering::SeqCst);
    loop {
        if m.tip.lock().retry(&m.shared) == Publish::Done {
            break;
        }
        producer.park_timeout(Duration::from_millis(5));
    }
    for s in subs {
        assert_eq!(
            s.join().expect("subscriber"),
            chain,
            "Q = {q}: the local chain is the writer's"
        );
    }
}

// ---- P1 (§10.3): the feed model --------------------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    /// `true`: drain first if blocked, as the writers do; `false`: step past the backlog.
    Connect(bool),
    Disconnect(bool),
    Register,
    Subscribe(usize),
    Deregister(usize),
    Take(usize, usize),
    Compress(usize),
    Control(usize, u8),
    Retry,
    Drop(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => proptest::bool::weighted(0.7).prop_map(Op::Connect),
        2 => proptest::bool::weighted(0.7).prop_map(Op::Disconnect),
        1 => Just(Op::Register),
        2 => (0..6usize).prop_map(Op::Subscribe),
        1 => (0..6usize).prop_map(Op::Deregister),
        4 => (0..6usize, 1..4usize).prop_map(|(i, n)| Op::Take(i, n)),
        1 => (0..6usize).prop_map(Op::Compress),
        1 => (0..6usize, 0..3u8).prop_map(|(i, k)| Op::Control(i, k)),
        3 => Just(Op::Retry),
        1 => (0..6usize).prop_map(Op::Drop),
    ]
}

/// One subscriber in the reference: the events it must still apply, in order.
struct RefSub {
    id: FeedId,
    feed: Option<Box<dyn SubscriberFeed>>,
    /// `seq`s still expected (pushed or pending), front first.
    expect: VecDeque<u64>,
    closed: bool,
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// P1: random steps (some past a pending backlog), registrations, handoffs, takes,
    /// compressions, controls, deregistrations and reader drops against a sequential reference. Each
    /// subscriber's applied sequence is the writer's from its cut, minus compressed pairs;
    /// `used ≤ Q`; when the writer is `Done`, every expected event is in the slot. `register`
    /// and `deregister` may clear the last cursor (the lazy reclaim, step 4), so the reference
    /// re-reads the writer's state after them; the slot counts hold the table to account.
    #[test]
    fn p1_feed_model(q in 1u32..4, ops in proptest::collection::vec(op(), 1..120)) {
        let m = Model::new(cfg(q).with_max_feeds(6));
        let mut log: Vec<ChainEvent> = Vec::new();
        let mut chain = vec![block(0)];
        let mut k = 1u32;
        let mut subs: Vec<RefSub> = Vec::new();
        let mut blocked = false;
        for op in ops {
            let mut retried = false;
            match op {
                Op::Connect(true) | Op::Disconnect(true) if blocked => {
                    blocked = m.tip.lock().retry(&m.shared) == Publish::Blocked;
                    retried = true;
                }
                Op::Connect(_) => {
                    let b = blk(k, chain.len() as u32);
                    k += 1;
                    chain.push(b);
                    let (e, p) = m.tip.lock().connect(&m.shared, b);
                    log.push(e);
                    blocked = p == Publish::Blocked;
                    for s in subs.iter_mut().filter(|s| s.feed.is_some() && !s.closed) {
                        s.expect.push_back(e.seq());
                    }
                }
                Op::Disconnect(_) => {
                    if chain.len() < 2 {
                        continue;
                    }
                    chain.pop();
                    let (e, p) = m.tip.lock().disconnect(&m.shared, *chain.last().expect("tip"));
                    log.push(e);
                    blocked = p == Publish::Blocked;
                    for s in subs.iter_mut().filter(|s| s.feed.is_some() && !s.closed) {
                        s.expect.push_back(e.seq());
                    }
                }
                Op::Register => {
                    if let Ok(id) = m.handle.register(Needs::default()) {
                        subs.push(RefSub { id, feed: None, expect: VecDeque::new(), closed: false });
                    }
                    blocked = !m.tip.lock().feeds.pending_is_empty();
                }
                Op::Subscribe(i) => {
                    let Some(s) = subs.get_mut(i) else { continue };
                    let tip = m.tip.lock().ctx.tip;
                    let r = m.handle.subscribe_at(s.id, &tip, &mut ok_finish());
                    match (r, &s.feed, s.closed) {
                        (Ok(Handoff::Live { feed, seq_synced }), None, false) => {
                            prop_assert_eq!(seq_synced, log.len() as u64 + 1);
                            s.feed = Some(feed);
                        }
                        (Err(SubscribeError::AlreadyLive), Some(_), false) => {}
                        (Err(SubscribeError::Closing | SubscribeError::Stale), _, true) => {}
                        (r, _, closed) => prop_assert!(false, "subscribe_at: {:?} closed {}", r.err(), closed),
                    }
                }
                Op::Deregister(i) => {
                    let Some(s) = subs.get_mut(i) else { continue };
                    m.handle.deregister(s.id);
                    s.closed = true;
                    blocked = !m.tip.lock().feeds.pending_is_empty();
                }
                Op::Take(i, n) => {
                    let Some(s) = subs.get_mut(i) else { continue };
                    let Some(f) = s.feed.as_mut() else { continue };
                    for _ in 0..n {
                        match f.next() {
                            Next::Control(_) => {}
                            Next::Event(_) => {
                                let e = f.pop().expect("selected");
                                prop_assert_eq!(Some(e.seq()), s.expect.pop_front(), "in order from the cut");
                                prop_assert_eq!(e, log[e.seq() as usize - 1]);
                            }
                            Next::Empty => break,
                        }
                    }
                }
                Op::Compress(i) => {
                    let Some(s) = subs.get_mut(i) else { continue };
                    let Some(f) = s.feed.as_mut() else { continue };
                    let Next::Event(&TipStep::Connected { block: b, .. }) = f.next() else { continue };
                    if let Some((c, d)) = f.remove_round_trip(&b.hash) {
                        prop_assert_eq!(s.expect.front(), Some(&c));
                        let at = s.expect.iter().position(|&x| x == d);
                        prop_assert!(at.is_some(), "the disconnect was expected");
                        s.expect.remove(at.expect("checked"));
                        s.expect.pop_front();
                    }
                }
                Op::Control(i, kind) => {
                    let Some(s) = subs.get(i) else { continue };
                    let r = m.handle.send_control(s.id, ControlMsg::new(ControlKind::new(kind), 0u8));
                    prop_assert!(s.closed || r != SendControl::Stale);
                }
                Op::Retry => {
                    blocked = m.tip.lock().retry(&m.shared) == Publish::Blocked;
                    retried = true;
                }
                Op::Drop(i) => {
                    // The reader's drop closes the slot; no step 4, so the table reclaims it
                    // lazily.
                    let Some(s) = subs.get_mut(i) else { continue };
                    if s.feed.take().is_some() {
                        s.closed = true;
                    }
                }
            }
            // used ≤ Q, and with the writer Done every expected event is queued or staged.
            let reg = m.shared.registry.load();
            for s in &subs {
                let Some(e) = reg.feeds.iter().find(|e| e.id == s.id) else { continue };
                let (used, _, _) = e.slot.counts();
                prop_assert!(used <= q);
                if !blocked && !s.closed && s.feed.is_some() {
                    prop_assert_eq!(used as usize, s.expect.len());
                }
            }
            // After a retry, only a full open target keeps the writer waiting: a closing one never
            // does (F11).
            if retried && blocked {
                let full = |s: &RefSub| {
                    reg.feeds.iter().any(|e| e.id == s.id && e.slot.counts().0 == q)
                };
                prop_assert!(
                    subs.iter().any(|s| !s.closed && s.feed.is_some() && full(s)),
                    "Blocked after a retry with no full open target"
                );
            }
            let g = m.tip.lock();
            g.feeds.assert_consistent();
            prop_assert_eq!(blocked, !g.feeds.pending_is_empty());
        }
    }
}
