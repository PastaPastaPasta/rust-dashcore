//! The hub over the scripted producer: step 5's capture and controls (F7, H2), H7's predicates,
//! the gate, the locks (DEC-64), W's latch-registered wait (E-b1.7), `FlushDue` and the
//! moveback's log lines.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;

use dash_chain::{BlockRef, Handoff, Needs, Next, SubscriberFeed};
use dashd_store::ErrorKind;

use super::*;
use crate::store::{Failure, GlobalFailStop};
use crate::subscriber::{ScriptedFeeds, SubscriberError, TreeIndex};

struct Fx {
    tree: TreeIndex,
    feeds: ScriptedFeeds,
    hub: SubscriberHub,
    main: Vec<BlockRef>,
    latch: GlobalFailStop,
}

/// Genesis plus 10 blocks, all connected, Q = 4.
fn fx() -> Fx {
    let tree = TreeIndex::new();
    let g = tree.genesis();
    let main = std::iter::once(g).chain(tree.branch(g, 10)).collect::<Vec<_>>();
    let feeds = ScriptedFeeds::new(4, main.clone());
    let hub = SubscriberHub::with_feeds(Arc::new(feeds.clone()), Arc::new(tree.clone()));
    Fx {
        tree,
        feeds,
        hub,
        main,
        latch: GlobalFailStop::new(),
    }
}

fn reg(name: &'static str, mandatory: bool, durable: Option<BlockRef>) -> Registration {
    Registration {
        name,
        needs: Needs::default(),
        mandatory,
        start_height: 1,
        durable,
    }
}

impl Fx {
    fn register(&self, r: Registration) -> SubscriberHandle {
        self.hub.register(r, &self.latch).expect("not latched").expect("registers")
    }

    /// A registered subscriber, Synced at the tip if `live`.
    fn sub(
        &self,
        name: &'static str,
        mandatory: bool,
        durable: Option<BlockRef>,
        live: bool,
    ) -> (SubscriberHandle, Option<Box<dyn SubscriberFeed>>) {
        let h = self.register(reg(name, mandatory, durable));
        let feed = live.then(|| {
            let tip = self.feeds.tip().expect("a tip");
            match self.hub.table().expect("feeds").subscribe_at(h.id, &tip, &mut || Ok(())) {
                Ok(Handoff::Live {
                    feed,
                    ..
                }) => feed,
                _ => panic!("subscribe_at"),
            }
        });
        (h, feed)
    }
}

fn kinds(f: &mut dyn SubscriberFeed) -> Vec<u8> {
    let mut out = Vec::new();
    while let Next::Control(c) = f.next() {
        out.push(c.kind().get());
    }
    out
}

#[test]
fn a_hub_with_no_feeds_records_t_ack_and_awaits_nobody() {
    let tree = TreeIndex::new();
    let hub = SubscriberHub::new();
    let g = tree.genesis();
    assert_eq!(hub.publish_durable(1, g, 7), Vec::new());
    assert_eq!(
        hub.t_ack(),
        Some(CommitTarget {
            tip: g,
            tip_seq: 7
        })
    );
    assert_eq!(hub.commit_cap(), Some(g));
    assert_eq!(hub.open_rewind_gate(2, g), Vec::new());
    assert!(hub.acks_done(1) && hub.gate_done(2));
    assert_eq!((hub.predicted_locks(&g), hub.actual_locks(&g)), (Vec::new(), Vec::new()));
}

/// F7 and H2: step 5 captures the registered mandatory subscribers Synced now. Each gets
/// `FlushCommit` and `ChainStateFlushed`; a Synced optional one gets `ChainStateFlushed` only, as
/// each of Core's listeners does; a Syncing one nothing.
#[test]
fn step_5_captures_the_synced_mandatory_set() {
    let fx = fx();
    let new = fx.main[8];
    let (a, fa) = fx.sub("a", true, Some(fx.main[5]), true);
    let (b, _) = fx.sub("b", true, Some(fx.main[2]), false);
    let (c, fc) = fx.sub("c", false, None, true);
    let (mut fa, mut fc) = (fa.expect("live"), fc.expect("live"));
    let to = fx.hub.publish_durable(3, new, 8);
    assert_eq!(to, vec![a.id]);
    assert_eq!(kinds(&mut *fa), [FLUSH_COMMIT.get(), CHAIN_STATE_FLUSHED.get()]);
    assert_eq!(kinds(&mut *fc), [CHAIN_STATE_FLUSHED.get()]);
    let commit = FlushCommit {
        flush: 3,
        new,
        tip_seq: 8,
    };
    assert_eq!(fx.hub.outstanding(a.id).commit, Some(commit));
    assert_eq!(fx.hub.outstanding(b.id), Outstanding::default());
    assert_eq!(fx.hub.outstanding(c.id), Outstanding::default());

    assert!(!fx.hub.acks_done(3));
    fx.hub.ack(a.id, 2, new);
    assert!(!fx.hub.acks_done(3), "another flush's ack");
    assert_eq!(fx.hub.durable_locator(a.id), Some(fx.main[5]), "a stale ack leaves L alone");
    fx.hub.ack(b.id, 3, new);
    assert_eq!(fx.hub.durable_locator(b.id), Some(fx.main[2]), "so does an uncaptured one");
    fx.hub.rewound(a.id, 3, fx.main[1]);
    assert_eq!(fx.hub.durable_locator(a.id), Some(fx.main[5]), "and a report with no gate");
    fx.hub.ack(a.id, 3, new);
    assert!(fx.hub.acks_done(3));
    assert_eq!(fx.hub.outstanding(a.id).commit, None);
    assert_eq!(fx.hub.durable_locator(a.id), Some(new));
}

/// D27: the ALWAYS shortcut waits for every Synced locator at the tip; a Syncing one does not
/// count.
#[test]
fn synced_at_needs_every_synced_locator_at_the_tip() {
    let fx = fx();
    let tip = fx.main[10];
    assert!(SubscriberHub::new().synced_at(&tip), "no feeds");
    let (a, _fa) = fx.sub("a", true, Some(fx.main[8]), true);
    let (_, _) = fx.sub("b", true, Some(fx.main[2]), false);
    assert!(!fx.hub.synced_at(&tip));
    fx.hub.set_durable(a.id, tip);
    assert!(fx.hub.synced_at(&tip));
}

/// H7: a closing slot retires its obligation at once, and its ack is ignored.
#[test]
fn closing_retires_the_obligation_and_its_ack_is_ignored() {
    let fx = fx();
    let (a, fa) = fx.sub("a", true, Some(fx.main[5]), true);
    fx.hub.publish_durable(1, fx.main[10], 10);
    assert!(!fx.hub.acks_done(1));
    drop(fa);
    assert!(fx.hub.acks_done(1));
    fx.hub.ack(a.id, 1, fx.main[10]);
    assert_eq!(fx.hub.durable_locator(a.id), Some(fx.main[5]), "the ack is ignored");
    fx.hub.set_durable(a.id, fx.main[9]);
    assert_eq!(fx.hub.durable_locator(a.id), Some(fx.main[5]), "so is a commit report");
}

/// Step 3b (rule 4): the gate targets the mandatory subscribers whose L strictly descends from
/// `to`, caps commits at `to`, and closes at step 5.
#[test]
fn the_gate_targets_locators_above_new_and_caps_commits() {
    let fx = fx();
    let side = fx.tree.branch(fx.main[4], 3);
    // The readers stay open: a dropped reader closes its slot.
    let (above, _fa) = fx.sub("above", true, Some(fx.main[9]), true);
    let (at, _fb) = fx.sub("at", true, Some(fx.main[6]), true);
    let (stale, _) = fx.sub("stale", true, Some(side[2]), false);
    let (optional, _fd) = fx.sub("optional", false, Some(fx.main[9]), true);
    fx.hub.publish_durable(1, fx.main[10], 10);
    let to = fx.main[6];
    let targets = fx.hub.open_rewind_gate(2, to);
    assert_eq!(targets, vec![above.id], "not at `to`, off its chain or optional");
    let req = RewindRequest {
        flush: 2,
        to,
    };
    assert_eq!(fx.hub.outstanding(above.id).rewind, Some(req));
    for id in [at.id, stale.id, optional.id] {
        assert_eq!(fx.hub.outstanding(id).rewind, None);
    }
    assert_eq!(fx.hub.commit_cap(), Some(to));
    assert!(!fx.hub.gate_done(2));
    fx.hub.rewound(above.id, 2, fx.main[6]);
    assert!(fx.hub.gate_done(2));
    assert_eq!(fx.hub.durable_locator(above.id), Some(fx.main[6]));
    fx.hub.publish_durable(2, to, 12);
    assert_eq!(fx.hub.commit_cap(), Some(to));
    assert_eq!(fx.hub.outstanding(above.id).rewind, None, "step 5 closed the gate");
    assert_eq!(fx.hub.open_rewind_gate(3, fx.main[6]), Vec::new());
}

/// R11a's race (D33): a commit begun before step 3b's read is a candidate, so a locator landing
/// above `new` after the read is still awaited; a commit begun after is capped at
/// `LCA(T_ack, new)`, with targets or without, until step 5.
#[test]
fn step_3b_sees_a_begun_commit_and_caps_later_ones() {
    let fx = fx();
    let side = fx.tree.branch(fx.main[4], 3);
    let (late, _fa) = fx.sub("late", true, Some(fx.main[5]), true);
    fx.hub.publish_durable(1, fx.main[10], 10);
    assert_eq!(fx.hub.begin_commit(late.id, fx.main[9]), Some(fx.main[9]));
    assert_eq!(fx.hub.open_rewind_gate(2, fx.main[6]), vec![late.id], "the pending commit");
    fx.hub.set_durable(late.id, fx.main[9]);
    assert_eq!(fx.hub.begin_commit(late.id, fx.main[10]), Some(fx.main[6]));
    fx.hub.end_commit(late.id);
    fx.hub.rewound(late.id, 2, fx.main[6]);
    fx.hub.publish_durable(2, fx.main[6], 12);
    assert_eq!(fx.hub.commit_cap(), Some(fx.main[6]));
    // A reorg's flush with no target: the cap is the fork until step 5.
    assert_eq!(fx.hub.open_rewind_gate(3, side[2]), Vec::new());
    assert_eq!(fx.hub.commit_cap(), Some(fx.main[4]));
    assert_eq!(fx.hub.begin_commit(late.id, fx.main[6]), Some(fx.main[4]));
    fx.hub.end_commit(late.id);
    fx.hub.publish_durable(3, side[2], 14);
    assert_eq!(fx.hub.commit_cap(), Some(side[2]));
    assert_eq!(fx.hub.begin_commit(late.id, side[1]), Some(side[1]));
}

/// DEC-64: step 0 predicts Synced subscribers at `new`, the others at `LCA(L, new)`, and the
/// start height with no locator; step 7 counts every mandatory one at `LCA(L, new)`, a closing
/// one until its deregistration completes.
#[test]
fn the_locks_are_predicted_at_step_0_and_actual_at_step_7() {
    let fx = fx();
    let side = fx.tree.branch(fx.main[4], 3);
    let (synced, fs) = fx.sub("synced", true, Some(fx.main[3]), true);
    let (_, _) = fx.sub("syncing", true, Some(side[2]), false);
    let (_, _) = fx.sub("fresh", true, None, false);
    let (_, _) = fx.sub("optional", false, Some(fx.main[1]), true);
    let new = fx.main[10];
    assert_eq!(fx.hub.predicted_locks(&new), [("synced", 10), ("syncing", 4), ("fresh", 1)]);
    assert_eq!(fx.hub.actual_locks(&new), [("synced", 3), ("syncing", 4), ("fresh", 1)]);
    drop(fs);
    assert_eq!(fx.hub.predicted_locks(&new)[0], ("synced", 3), "closing is not Synced");
    assert_eq!(fx.hub.actual_locks(&new)[0], ("synced", 3), "held until deregistration");
    fx.hub.deregister(synced.id);
    assert_eq!(fx.hub.actual_locks(&new), [("syncing", 4), ("fresh", 1)]);
}

/// E-b1.7 and H7: W's wait wakes on an ack, on a closing slot, and on the latch.
#[test]
#[expect(clippy::disallowed_methods, reason = "plain test threads standing in for subscribers")]
fn w_wakes_on_an_ack_a_closing_and_the_latch() {
    let fx = Arc::new(fx());
    let latch = GlobalFailStop::new();
    let (a, fa) = fx.sub("a", true, None, true);
    fx.hub.publish_durable(1, fx.main[10], 10);
    let s = {
        let fx = Arc::clone(&fx);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            fx.hub.ack(a.id, 1, fx.main[10]);
        })
    };
    while !fx.hub.acks_done(1) {
        fx.hub.park_w(&latch).unwrap();
    }
    s.join().unwrap();

    fx.hub.publish_durable(2, fx.main[10], 10);
    let s = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        drop(fa);
    });
    while !fx.hub.acks_done(2) {
        fx.hub.park_w(&latch).unwrap();
    }
    s.join().unwrap();

    let (_, _fb) = fx.sub("b", true, None, true);
    fx.hub.publish_durable(3, fx.main[10], 10);
    let tripper = latch.clone();
    let s = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        tripper.trip(Failure {
            kind: ErrorKind::Other,
            site: "test",
            message: "a failure".into(),
        });
    });
    let mut parked = Ok(());
    while !fx.hub.acks_done(3) && parked.is_ok() {
        parked = fx.hub.park_w(&latch);
    }
    assert!(parked.is_err());
    s.join().unwrap();
}

/// Rule 3's post: the flag, then V's wake; V takes it once.
#[test]
fn flush_due_rings_v_and_is_taken_once() {
    let fx = fx();
    let rang = Arc::new(AtomicUsize::new(0));
    let r = Arc::clone(&rang);
    fx.hub.set_due_wake(move || {
        r.fetch_add(1, AtomicOrdering::Relaxed);
    });
    assert!(!fx.hub.take_flush_due());
    fx.hub.post_flush_due();
    assert_eq!(rang.load(AtomicOrdering::Relaxed), 1);
    assert!(fx.hub.flush_due());
    assert!(fx.hub.take_flush_due());
    assert!(!fx.hub.take_flush_due());
}

/// Core's moveback (`validation.cpp:3292-3301`): each lock above `height − 1` moves there,
/// once per disconnect, until the subscriber's best block moves it again.
#[test]
fn the_moveback_logs_each_lock_above_the_disconnected_parent() {
    let fx = fx();
    let (a, _) = fx.sub("a", true, Some(fx.main[10]), true);
    let (b, _) = fx.sub("b", true, Some(fx.main[8]), true);
    assert_eq!(fx.hub.on_disconnect_tip(10), [("a", 9)]);
    assert_eq!(fx.hub.on_disconnect_tip(9), [("a", 8)]);
    assert_eq!(fx.hub.on_disconnect_tip(8), [("a", 7), ("b", 7)]);
    a.set_best_height(9);
    b.set_best_height(7);
    assert_eq!(fx.hub.on_disconnect_tip(8), [("a", 7)]);
}

/// The scripted producer's cut (F10): an event assigned before `subscribe_at` is not delivered.
#[test]
fn the_cut_excludes_an_event_pending_at_the_handoff() {
    let fx = fx();
    let (_, mut fa) = fx.sub("a", true, None, true);
    let fa = fa.as_mut().expect("live");
    for b in fx.tree.branch(fx.main[10], 5) {
        fx.feeds.connect(b);
    }
    assert!(!fx.feeds.pending_is_empty(), "Q = 4: the fifth waits");
    let (late, _) = fx.sub("late", true, None, false);
    let tip = fx.feeds.tip().expect("a tip");
    let Ok(Handoff::Live {
        feed: mut fl,
        seq_synced,
    }) = fx
        .hub
        .table()
        .expect("feeds")
        .subscribe_at(late.id, &tip, &mut || Ok::<(), SubscriberError>(()))
    else {
        panic!("subscribe_at");
    };
    assert_eq!(seq_synced, fx.feeds.tip_seq() + 1);
    while let Next::Event(_) = fa.next() {
        fa.pop();
        fx.feeds.retry_pending();
    }
    assert!(fx.feeds.pending_is_empty());
    assert!(matches!(fl.next(), Next::Empty), "the pending event is in p already");
}

/// The slot's prune lock stays at its durable locator while the slot closes, until
/// deregistration completes (b2's bound, b3's predicate).
#[test]
fn a_closing_slot_holds_its_prune_lock_until_deregistered() {
    let fx = fx();
    let (a, fa) = fx.sub("a", true, Some(fx.main[5]), true);
    let new = fx.main[10];
    assert_eq!(fx.hub.actual_locks(&new), [("a", 5)]);
    drop(fa);
    assert_eq!(fx.hub.actual_locks(&new), [("a", 5)], "closing, still registered");
    fx.hub.deregister(a.id);
    assert_eq!(fx.hub.actual_locks(&new), []);
}

/// Sol r1's blocker 1: a registration that arrives after step 3b waits until the flush has
/// applied its prune and released (D35), so it misses neither the gate nor step 7's locks.
#[test]
#[expect(clippy::disallowed_methods, reason = "plain test threads standing in for a registrant")]
fn a_registration_after_step_3b_waits_for_the_flush() {
    let fx = fx();
    fx.hub.publish_durable(1, fx.main[10], 10);
    assert!(fx.hub.open_rewind_gate(2, fx.main[6]).is_empty());
    let fx = &fx;
    std::thread::scope(|sc| {
        let (tx, rx) = std::sync::mpsc::channel();
        sc.spawn(move || {
            let h = fx.register(reg("late", true, Some(fx.main[9])));
            tx.send(h).expect("sent");
        });
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "it waits for the flush");
        assert_eq!(fx.hub.actual_locks(&fx.main[6]), [], "no lock entry inside the window");
        fx.hub.publish_durable(2, fx.main[6], 12);
        let still = rx.recv_timeout(Duration::from_millis(300));
        assert!(still.is_err(), "step 5 is not the end: step 7 may still prune");
        fx.hub.end_flush(2);
        let late = rx.recv_timeout(Duration::from_secs(60)).expect("registered after the flush");
        // The hub registers L as given; the driver's open rewinds it onto G's path (D45).
        assert_eq!(fx.hub.durable_locator(late.id), Some(fx.main[9]));
    });
}

/// Sol r2's minor 3 (D47): the thread that registered a subscriber owns it before its driver
/// ever steps, so a registration there while the window waits on that subscriber is refused
/// (the wait would wait on itself); elsewhere it waits, and once the subscriber has rewound
/// the flush no longer waits on its owner.
#[test]
#[expect(clippy::disallowed_methods, reason = "plain test threads standing in for W and others")]
fn a_registration_on_the_owner_of_a_gated_subscriber_is_refused() {
    let fx = fx();
    fx.hub.publish_durable(1, fx.main[10], 10);
    let a = fx.register(reg("a", true, Some(fx.main[9])));
    let fx = &fx;
    std::thread::scope(|sc| {
        let w = sc.spawn(|| fx.hub.open_rewind_gate(2, fx.main[6]));
        assert_eq!(w.join().expect("W"), [a.id]);
        // Without the refusal the register waits on itself: the watchdog's release turns that
        // into a failure, not a hang.
        let (done, watched) = std::sync::mpsc::channel::<()>();
        sc.spawn(move || {
            if watched.recv_timeout(Duration::from_secs(30)).is_err() {
                fx.hub.end_flush(2);
            }
        });
        let here = fx.hub.register(reg("b", false, None), &fx.latch).expect("not latched");
        assert!(matches!(here, Err(Refused::WouldDeadlock)), "{here:?}");
        done.send(()).expect("sent");
        let (tx, rx) = std::sync::mpsc::channel();
        sc.spawn(move || tx.send(fx.register(reg("c", false, None))).expect("sent"));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "elsewhere it waits");
        fx.hub.rewound(a.id, 2, fx.main[6]);
        sc.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            fx.hub.end_flush(2);
        });
        let owner = fx.hub.register(reg("d", false, None), &fx.latch).expect("not latched");
        assert!(owner.is_ok(), "the owner waits once nothing it owns is waited on: {owner:?}");
        rx.recv_timeout(Duration::from_secs(60)).expect("registered after the flush");
    });
}
