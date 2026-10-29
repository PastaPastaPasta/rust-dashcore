//! `ChainViewHandle` and the `tip_seq` fence (N6, §6.2, §6.3).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_chain::{FeedSource, Handoff, Needs, Next, Notify, Parker, Status, TipStep, Work};
use dash_clock::NodeSeconds;

use super::support::{Rig, RigConfig, batch, branch};
use crate::chain::actor::tests::Stage;
use crate::chain::reply::WaitError;
use crate::chain::{ChainCommand, NoWaitGuard, TipSeqFence, spawn_thread};

const SOON: Duration = Duration::from_secs(120);

/// The handle's reads are SB4's and V's published state, and `Query` runs on V.
#[test]
fn the_handle_reads_the_published_state() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let h = rig.v.view_handle();
    let a3 = rig.id(&main[2]);
    let snap = h.snapshot();
    assert_eq!(snap.tip.tip, a3);
    assert_eq!(h.view().snap.tip.tip, a3);
    assert_eq!(h.lookup(&main[2].hash()), Some(a3));
    assert_eq!(h.entry(a3).map(|e| e.hash), Some(main[2].hash()));
    assert_eq!(h.ancestor(a3, 1), Some(rig.id(&main[0])));
    assert_eq!(h.locator(Some(a3)), rig.v.reader().locator(Some(a3)));
    assert_eq!(h.locator(Some(a3)).first(), Some(&main[2].hash()));
    assert_eq!(h.fence_seq(), snap.tip_seq);
    assert_eq!(h.query(|q| q.idx.best_candidate()).recv(), Ok(Some(a3)));
}

/// D63 (BL-206, P3 S1's shape): a reader's status is the status as of the view it holds, as
/// Core's one `cs_main` section shows the tip and its statuses together. Inside the hold that
/// connects a block, `connect_block` has raised it to Scripts before any view names it: the
/// live status shows Scripts, a reader of the old view does not; the new view's reader does,
/// and a view from before the block still does not.
#[test]
fn a_reader_sees_the_status_of_the_view_it_holds() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    rig.accept(&main[0]);
    let h = rig.v.view_handle();
    let a2 = main[1].hash();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (hook_h, at) = (h.clone(), seen.clone());
    rig.on_v(move |vv| {
        vv.release_hook = Some(Arc::new(move |stage| {
            let Some(id) = hook_h.lookup(&a2) else {
                return;
            };
            let view = hook_h.view();
            let live = hook_h.entry(id).expect("resident").status;
            at.lock().expect("seen").push((
                stage,
                view.snap.tip.tip == id,
                hook_h.valid_scripts(&view, id).expect("held briefly"),
                live.is_valid(Status::VALID_SCRIPTS),
            ));
        }));
        Ok(())
    });
    let before = h.view();
    rig.accept(&main[1]);
    let seen = std::mem::take(&mut *seen.lock().expect("seen"));
    // (the view names the block, its reader sees Scripts, the live status is Scripts)
    let at = |stage| {
        let s = seen.iter().find(|s| s.0 == stage).expect("the release ran");
        (s.1, s.2, s.3)
    };
    assert_eq!(at(Stage::Release), (false, false, true));
    assert_eq!(at(Stage::Snapshot), (false, false, true));
    assert_eq!(at(Stage::Published), (true, true, true));
    // A view from before the block reads its status at insertion: a header, no data.
    let id = rig.id(&main[1]);
    let inserted = h.status(&before, id).expect("held briefly").expect("in the index");
    assert!(
        inserted.is_valid(Status::VALID_TREE) && !inserted.has(Status::HAVE_DATA),
        "{inserted:?}"
    );
    assert_eq!(h.valid_scripts(&before, id), Ok(false));
    assert_eq!(h.valid_scripts(&h.view(), id), Ok(true));
}

/// D70 (P3-notes D122 (3)): the test-support hook runs once per release, on V, between the two
/// stores: SB4's snapshot names the new tip, the `ChainView` still the old one; `None` removes it.
#[cfg(feature = "test-support")]
#[test]
fn the_between_stores_hook_sees_the_snapshot_ahead_of_the_view() {
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 2, 70);
    rig.accept(&main[0]);
    let h = rig.v.view_handle();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (hook_h, at) = (h.clone(), seen.clone());
    rig.v.set_between_stores(Some(Arc::new(move || {
        let on_v = std::thread::current().name() == Some("dashd-v");
        at.lock().expect("seen").push((
            on_v,
            hook_h.snapshot().tip.tip,
            hook_h.view().snap.tip.tip,
        ));
    })));
    rig.accept(&main[1]);
    let (a1, a2) = (rig.id(&main[0]), rig.id(&main[1]));
    assert_eq!(*seen.lock().expect("seen"), [(true, a2, a1)]);
    assert_eq!(h.view().snap.tip.tip, a2);
    rig.v.set_between_stores(None);
    rig.accept(&branch(&rig.p, &main[1], 1, 70)[0]);
    assert_eq!(seen.lock().expect("seen").len(), 1, "removed");
}

/// BL-224 (D65, D68): within the bound the writer keeps the status journals a view reads,
/// whether it is held or not, and the gauge counts them; dropping a view frees nothing.
#[test]
fn a_view_reads_while_the_writer_keeps_its_journals() {
    let rig = Rig::new();
    let h = rig.v.view_handle();
    let main = branch(&rig.p, &rig.genesis, 100, 0);
    let held = h.view();
    for m in &main {
        rig.accept(m);
    }
    let kept = h.reader().journal_retention();
    // Every block written at least twice since `held`.
    assert!(kept.entries >= 160 && kept.oldest_epoch == 0, "{kept:?}");
    assert_eq!(h.status(&held, rig.id(&main[99])).map(|s| s.is_some()), Ok(true));
    drop(held);
    let more = branch(&rig.p, &main[99], 100, 1);
    for m in &more {
        rig.accept(m);
    }
    let after = h.reader().journal_retention();
    assert!(after.entries >= kept.entries + 160 && after.oldest_epoch == 0, "{after:?}");
    assert_eq!(after.expired, 0);
}

/// `nMinimumChainWork` is `-minimumchainwork`'s.
#[test]
fn min_chain_work_is_the_configured_one() {
    let w = Work::from_u64(1 << 40);
    let rig = Rig::with(RigConfig {
        min_chain_work: Some(w),
        ..RigConfig::default()
    });
    assert_eq!(rig.v.view_handle().min_chain_work(), w);
}

/// `LoadingBlocks()` feeds the latch before it latches, and the latch holds after (R4).
#[test]
fn loading_and_the_ibd_latch_follow_v() {
    let rig = Rig::new();
    let h = rig.v.view_handle();
    let set_flags = |importing, reindex| {
        rig.v
            .send(ChainCommand::SetLoading {
                importing,
                reindex,
            })
            .expect("V runs");
        rig.assert_tables();
    };
    let set = |importing| set_flags(Some(importing), None);
    // Recent enough for the tip's age; the rig's clock, a day past genesis, is not.
    let fresh = NodeSeconds(i64::from(rig.genesis.time()) + 60);
    assert!(!h.loading_blocks());
    set_flags(None, Some(true));
    assert!(h.loading_blocks(), "reindex");
    set_flags(None, Some(false));
    assert!(!h.loading_blocks());
    set(true);
    assert!(h.loading_blocks());
    assert!(h.is_initial_block_download(&h.snapshot(), fresh), "loading");
    set(false);
    assert!(h.ibd_service_cache(), "the node clock: the tip is a day old");
    assert!(!h.is_initial_block_download(&h.snapshot(), fresh), "latches");
    assert!(!h.ibd_service_cache(), "the latch is SB4's, shared");
    set(true);
    assert!(!h.is_initial_block_download(&h.snapshot(), fresh), "latched");
    assert!(!h.ibd_service_cache());
}

/// `process_headers` blocks until V replies, with the reply's snapshot published first (R3); a
/// marked task is refused (LO-9), and a stopped V disconnects the reply.
#[test]
fn process_headers_blocks_for_the_reply() {
    let mut rig = Rig::new();
    let h = rig.v.view_handle();
    let headers = branch(&rig.p, &rig.genesis, 2, 0);
    let r = h.process_headers(batch(&headers), None).expect("V replies");
    assert!(r.stop.is_none(), "{:?}", r.stop);
    let b2 = rig.id(&headers[1]);
    assert_eq!((r.last_accepted, r.snap.tip.best_header), (Some(b2), b2));
    let fresh = branch(&rig.p, &rig.genesis, 1, 1);
    let marked = {
        let _m = NoWaitGuard::set();
        h.process_headers(batch(&fresh), None).err()
    };
    assert_eq!(marked, Some(WaitError::WaitOnVFromNoWaitTask));
    // Refused before the send: once the marked lane is empty, the query after it would follow
    // the batch.
    let deadline = std::time::Instant::now() + SOON;
    while rig.v.marked_lane_depth() > 0 {
        assert!(std::time::Instant::now() < deadline, "V never drained the marked lane");
        std::thread::yield_now();
    }
    h.query(|_| ()).recv().expect("V serves");
    assert_eq!(h.lookup(&fresh[0].hash()), None, "V never saw the refused batch");
    rig.stop();
    assert_eq!(h.process_headers(batch(&headers), None).err(), Some(WaitError::Disconnected));
}

/// The fence serves the newest state its `tip_seq` covers, never one ahead of it, and never
/// goes back.
#[test]
fn the_fence_serves_the_newest_covered_state() {
    let mut f = TipSeqFence::new(4, "initial");
    for (seq, s) in [(5, "five"), (6, "six"), (7, "seven")] {
        f.record(seq, s, 4);
    }
    assert_eq!(*f.serve(4), "initial");
    assert_eq!(*f.serve(6), "six");
    assert_eq!(*f.serve(5), "six");
    assert_eq!(*f.serve(7), "seven");
    f.record(8, "eight", 7);
    assert_eq!(*f.serve(100), "eight");
}

/// `record` drops what its fence covers, so an owner that never serves keeps at most the states
/// ahead of the fence.
#[test]
fn recording_drops_the_covered_states() {
    let mut f = TipSeqFence::new(0, 0);
    for seq in 1..=1_000 {
        f.record(seq, seq, seq - 1);
    }
    assert_eq!(f.pending(), 1);
    assert_eq!(*f.serve(999), 999);
}

/// A `seq` that does not ascend is a broken run (a missed restart, or a fold recorded at its
/// first event).
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "seqs ascend: 4 after 4")]
fn a_seq_that_does_not_ascend_panics() {
    let mut f = TipSeqFence::new(3, ());
    f.record(4, (), 4);
    f.record(4, (), 4);
}

/// §6.2's fence (OH-M1) over a reorg inside one hold at Q = 1: an F1 subscriber learns each step
/// while the view still names the old tip, so its derived height, served behind the fence, stays
/// at `getblockcount` until the release, then follows the steps the view covers.
#[test]
fn f1_state_behind_the_fence_never_runs_ahead_of_the_view() {
    let rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let side = branch(&rig.p, &rig.genesis, 4, 1);
    for m in &side[..3] {
        rig.accept(m);
    }
    let feeds = rig.v.query(|q| q.chain.feeds()).recv().expect("V");
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(&feeds).tip();
    let Ok(Handoff::Live {
        mut feed,
        ..
    }) = feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(()))
    else {
        panic!("subscribe_at")
    };
    let h = rig.v.view_handle();
    let s = spawn_thread("s", move || {
        let _marked = NoWaitGuard::set();
        let p = Parker::new();
        feed.set_waker(Notify::Thread(p.clone()));
        let start = h.view();
        let mut synced = TipSeqFence::new(start.snap.tip_seq, start.snap.tip.tip_height);
        for i in 0..7 {
            loop {
                match feed.next() {
                    Next::Event(_) => break,
                    Next::Empty => assert!(p.park_timeout(SOON), "V never stepped"),
                    Next::Control(_) => {}
                }
            }
            if i < 6 {
                // V cannot take the last step until this pop: the hold is open.
                assert_eq!(h.view().snap.tip.tip_height, 3, "no release yet");
            }
            let step = feed.pop().expect("selected");
            let height = match &step {
                TipStep::Disconnected {
                    block,
                    ..
                } => block.height - 1,
                TipStep::Connected {
                    block,
                    ..
                } => block.height,
            };
            assert_eq!(height, [2, 1, 0, 1, 2, 3, 4][i]);
            // One load: the fence and `getblockcount` from the same view. Inside the hold it
            // names a3 and covers no step, so the fence serves a3's height. From the 6th pop V
            // may already have released: the view then covers every step, and the fence serves
            // the newest recorded one, which may lag the view but never leads it.
            let view = h.view();
            synced.record(step.seq(), height, view.snap.tip_seq);
            let covered = step.seq() <= view.snap.tip_seq;
            assert_eq!(
                *synced.serve(view.snap.tip_seq),
                if covered {
                    height
                } else {
                    3
                }
            );
            assert_eq!(
                view.snap.tip.tip_height,
                if covered {
                    4
                } else {
                    3
                }
            );
        }
        (synced, h)
    })
    .expect("spawn S");
    rig.accept(&side[3]);
    let (mut synced, h) = s.join().expect("unmarked joiner").expect("S");
    // The last pop let V release: only then does the fence pass the new tip.
    assert_eq!(*synced.serve(h.fence_seq()), 4);
    assert_eq!(h.view().snap.tip.tip_height, 4);
    feeds.deregister(id);
}
