//! F2 on SB4's `HeaderChain` (SB5-A14): `after_publish` fans out each node step and each SPV
//! composite, `snapshot().tip_seq` carries the pending event's `seq` (F2), and `subscribe_at`
//! hands off at the published tip (F10). The status facts `after_publish` derives (SB5-A17).

use std::sync::Arc;
use std::task::{Wake, Waker};

use dash_params::{ChainSetup, Network};
use dash_primitives::BlockHash;

use super::test_support::ok_finish;
use super::*;
use crate::BlockId;
use crate::chain::HeaderChain;
use crate::config::ChainConfig;
use crate::reader::ChainReader;
use crate::snapshot::ChainReadSnapshot;
use crate::status::Status;
use crate::status_feed::StatusEvent;
use crate::store::NoStore;
use crate::sync::Mutex;
use crate::testing::{fake_hash, header, params, services};
use crate::tip::BlockRef;

fn chain(cfg: impl FnOnce(&ChainSetup) -> ChainConfig, q: u32) -> HeaderChain {
    let p = params(Network::Regtest);
    let mut cfg = cfg(&p);
    cfg.feeds.capacity = q;
    let svc = services(&p);
    HeaderChain::new_genesis(p, cfg, svc, NoStore).expect("a valid configuration")
}

/// A node at genesis (`LoadChainTip`).
fn node(q: u32) -> HeaderChain {
    let mut c = chain(ChainConfig::node, q);
    c.init_active_tip(BlockId(0)).expect("Core LoadChainTip");
    c
}

/// `n` headers on `from` under hashes `fake_hash(k0..)`.
fn branch(c: &mut HeaderChain, from: BlockId, k0: u64, n: u64) -> Vec<BlockId> {
    let mut prev = from;
    (0..n)
        .map(|k| {
            let prev_hash: BlockHash = c.reader().entry(prev).expect("resident").hash;
            prev = c
                .add_to_block_index(
                    &header(prev_hash, 1_700_000_000 + k as u32, (k0 + k) as u32),
                    &fake_hash(k0 + k),
                    Status(Status::VALID_TREE),
                )
                .expect("a resident parent");
            prev
        })
        .collect()
}

fn bref(c: &HeaderChain, id: BlockId) -> BlockRef {
    let e = c.reader().entry(id).expect("resident");
    BlockRef {
        id,
        hash: e.hash,
        height: e.height,
    }
}

fn brefs(c: &HeaderChain, ids: impl IntoIterator<Item = BlockId>) -> Vec<BlockRef> {
    ids.into_iter().map(|id| bref(c, id)).collect()
}

fn subscribe(feeds: &ChainFeeds) -> (FeedId, Box<dyn SubscriberFeed>, u64) {
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(feeds).tip();
    match feeds.subscribe_at(id, &tip, &mut ok_finish()) {
        Ok(Handoff::Live {
            feed,
            seq_synced,
        }) => (id, feed, seq_synced),
        other => panic!("subscribe_at: {:?}", other.err()),
    }
}

fn take(f: &mut dyn SubscriberFeed) -> Vec<ChainEvent> {
    let mut out = Vec::new();
    while let Next::Event(_) = f.next() {
        out.push(f.pop().expect("selected"));
    }
    out
}

/// F2 (§10.1): the target is full, so the step's event is pending; the published snapshot's
/// `tip_seq` is that event's `seq` meanwhile, and `retry_pending` delivers it after a take.
#[test]
fn f2_seq_before_fanout() {
    let mut c = node(1);
    let ids = branch(&mut c, BlockId(0), 1, 3);
    let feeds = c.feeds();
    let (_, mut f, synced) = subscribe(&feeds);
    assert_eq!(synced, 1);

    let s1 = c.connect_tip(ids[0]).expect("connects");
    assert!(c.pending_is_empty());
    let s2 = c.connect_tip(ids[1]).expect("connects");
    assert!(!c.pending_is_empty(), "Q = 1: the second event waits");
    assert_eq!(c.snapshot().tip_seq, s2.seq(), "tip_seq is the pending event's");
    assert_eq!(FeedSource::<()>::snapshot(&feeds).tip_seq(), s2.seq());
    assert_eq!(c.retry_pending(), Publish::Blocked);

    assert_eq!(take(&mut *f), [s1]);
    assert_eq!(c.retry_pending(), Publish::Done);
    assert!(c.pending_is_empty());
    assert_eq!(take(&mut *f), [s2]);
}

/// The producer wake: a take on a full target wakes the writer's parker.
#[test]
fn producer_wake_on_space() {
    let mut c = node(1);
    let ids = branch(&mut c, BlockId(0), 1, 2);
    let p = crate::notify::Parker::new();
    c.set_producer_wake(Notify::Thread(p.clone()));
    let feeds = c.feeds();
    let (_, mut f, _) = subscribe(&feeds);
    c.connect_tip(ids[0]).expect("connects");
    c.connect_tip(ids[1]).expect("connects");
    assert!(!p.park_timeout(std::time::Duration::ZERO), "no wake yet");
    take(&mut *f);
    assert!(p.park_timeout(std::time::Duration::ZERO), "the take woke the writer");
}

/// X27 (§10.1) on the node: the snapshot ends at A3, the writer reorgs to B3, and
/// `subscribe_at(A3)` is `Moved`; the fresh snapshot's tip is B3.
#[test]
fn x27_moved_on_reorg_node() {
    let mut c = node(8);
    let a = branch(&mut c, BlockId(0), 1, 3);
    let b = branch(&mut c, BlockId(0), 101, 4);
    for &id in &a {
        c.connect_tip(id).expect("connects");
    }
    let feeds = c.feeds();
    let id = feeds.register(Needs::default()).expect("registers");
    let a3 = FeedSource::<()>::snapshot(&feeds).tip();
    assert_eq!(a3.id, a[2]);

    c.set_active_tip(b[2]).expect("reorgs to B3");
    let mut called = false;
    let r = feeds.subscribe_at(id, &a3, &mut || {
        called = true;
        Ok::<(), ()>(())
    });
    assert!(matches!(r, Ok(Handoff::Moved)) && !called);
    let snap = FeedSource::<()>::snapshot(&feeds);
    assert_eq!(snap.tip(), bref(&c, b[2]));
    assert!(matches!(
        feeds.subscribe_at(id, &snap.tip(), &mut ok_finish()),
        Ok(Handoff::Live {
            seq_synced: 10,
            ..
        })
    ));
}

/// Before genesis (CH1-A11) `subscribe_at` is `Moved`; after `connect_tip(genesis)` it hands
/// off and the subscriber receives the next step.
#[test]
fn pre_genesis_handoff_is_moved() {
    let mut c = chain(ChainConfig::node, 4);
    let ids = branch(&mut c, BlockId(0), 1, 1);
    let feeds = c.feeds();
    let id = feeds.register(Needs::default()).expect("registers");
    let placeholder = FeedSource::<()>::snapshot(&feeds).tip();
    assert!(matches!(feeds.subscribe_at(id, &placeholder, &mut ok_finish()), Ok(Handoff::Moved)));
    c.connect_tip(BlockId(0)).expect("genesis");
    let (_, mut f, synced) = subscribe(&feeds);
    assert_eq!(synced, 2);
    let s = c.connect_tip(ids[0]).expect("connects");
    assert_eq!(take(&mut *f), [s]);
}

/// SPV's `set_active_tip` fans its composite out once: disconnects tip-down, then connects
/// fork-up, with consecutive `seq`s ending at the snapshot's `tip_seq`.
#[test]
fn spv_composite_reaches_subscribers() {
    let mut c = chain(|p| ChainConfig::spv(p, 10_000), 2);
    let a = branch(&mut c, BlockId(0), 1, 2);
    let b = branch(&mut c, BlockId(0), 101, 3);
    c.tip_plan(a[1]).expect("plans");
    c.set_active_tip(a[1]).expect("extends");
    let feeds = c.feeds();
    let (_, mut f, synced) = subscribe(&feeds);
    assert_eq!(synced, 3);

    c.tip_plan(b[2]).expect("plans");
    let change = c.set_active_tip(b[2]).expect("reorgs");
    assert_eq!(change.snap.tip_seq, 7);
    let mut got = take(&mut *f);
    while c.retry_pending() == Publish::Blocked {
        got.extend(take(&mut *f));
    }
    got.extend(take(&mut *f));
    let mut want: Vec<ChainEvent> = [a[1], a[0]]
        .iter()
        .enumerate()
        .map(|(i, &id)| TipStep::Disconnected {
            block: bref(&c, id),
            seq: 3 + i as u64,
        })
        .collect();
    want.extend(b.iter().enumerate().map(|(i, &id)| TipStep::Connected {
        block: bref(&c, id),
        seq: 5 + i as u64,
    }));
    assert_eq!(got, want);
}

/// A status-only publication fans nothing out and leaves the backlog as it was.
#[test]
fn status_publication_fans_nothing_out() {
    let mut c = node(4);
    let ids = branch(&mut c, BlockId(0), 1, 1);
    let feeds = c.feeds();
    let (_, mut f, _) = subscribe(&feeds);
    c.publish_status();
    assert!(take(&mut *f).is_empty());
    let s = c.connect_tip(ids[0]).expect("connects");
    c.publish_status();
    assert_eq!(take(&mut *f), [s]);
    assert_eq!(feeds.registry().feeds.len(), 1);
}

impl FeedSnapshot {
    /// This snapshot with its view's window raised to `base`, as SB5's segmented activation
    /// leaves SPV's view (SB4 D-C4-7: no step trims it yet).
    pub(crate) fn windowed(&self, base: u32) -> FeedSnapshot {
        let mut snap = ChainReadSnapshot::clone(&self.snap);
        snap.active.chain.set_base(base);
        FeedSnapshot {
            snap: Arc::new(snap),
            reader: self.reader.clone(),
            cold: self.cold.clone(),
        }
    }
}

/// F8 (§5.3): every height and every range, resident or below the window, answers the
/// snapshot's chain; above the tip answers nothing. Base 11 leaves the window empty.
#[test]
fn snapshot_heights_below_window() {
    let mut c = node(4);
    let ids = branch(&mut c, BlockId(0), 1, 10);
    c.set_active_tip(ids[9]).expect("extends");
    let want = brefs(&c, [BlockId(0)].into_iter().chain(ids));
    let t = want.len() - 1;
    let feeds = c.feeds();
    for base in [0, 1, 4, 10, 11] {
        let s = feeds.feed_snapshot().windowed(base);
        assert_eq!(s.read().active.base(), base);
        for h in 0..=12 {
            assert_eq!(
                s.at_height_checked(h).unwrap(),
                want.get(h as usize).copied(),
                "{base}/{h}"
            );
            assert_eq!(s.at_height(h), want.get(h as usize).copied());
        }
        for lo in 0..=12u32 {
            for hi in 0..=12u32 {
                let w = want.get(lo as usize..=(hi as usize).min(t)).unwrap_or_default();
                assert_eq!(s.range(lo, hi).unwrap(), w, "{base}: {lo}..={hi}");
            }
        }
    }
}

/// A retained snapshot answers its own chain after a reorg, below its window too: the cold part
/// is ancestry from the view, never the live active chain.
#[test]
fn snapshot_heights_survive_reorg() {
    let mut c = node(4);
    let a = branch(&mut c, BlockId(0), 1, 6);
    let b = branch(&mut c, a[1], 101, 6);
    c.set_active_tip(a[5]).expect("extends");
    let feeds = c.feeds();
    let old = feeds.feed_snapshot().windowed(5);
    c.set_active_tip(b[5]).expect("reorgs at height 2");
    for (h, &id) in (1..).zip(&a) {
        assert_eq!(old.at_height_checked(h).unwrap(), Some(bref(&c, id)));
    }
    let now = feeds.feed_snapshot().windowed(5);
    assert_eq!(now.at_height_checked(3).unwrap(), Some(bref(&c, b[0])));
    let want = brefs(&c, [a[0], a[1]].into_iter().chain(b.iter().copied()));
    assert_eq!(now.range(1, 99).unwrap(), want);
    assert_eq!(old.range(1, 99).unwrap(), brefs(&c, a.iter().copied()));
}

/// A window above whole dropped leaves: the walks below it take real skip jumps.
#[test]
fn snapshot_heights_wide_window() {
    let mut c = node(4);
    let ids = branch(&mut c, BlockId(0), 1, 2_100);
    c.set_active_tip(ids[2_099]).expect("extends");
    let want = brefs(&c, [BlockId(0)].into_iter().chain(ids));
    let s = c.feeds().feed_snapshot().windowed(1_500);
    for h in 0..=2_101 {
        assert_eq!(s.at_height_checked(h).unwrap(), want.get(h as usize).copied(), "{h}");
    }
    assert_eq!(s.range(0, u32::MAX).unwrap(), want);
    assert_eq!(s.range(1_020, 1_530).unwrap(), want[1_020..=1_530]);
}

/// Before the node has a tip, the snapshot answers no height.
#[test]
fn snapshot_heights_pre_genesis() {
    let c = chain(ChainConfig::node, 4);
    let s = c.feeds().feed_snapshot();
    assert_eq!(s.at_height_checked(0).unwrap(), None);
    assert!(s.range(0, 5).unwrap().is_empty());
}

/// The best header's ref and time.
fn best(c: &HeaderChain, id: BlockId) -> (BlockRef, u32) {
    (bref(c, id), c.reader().entry(id).expect("resident").header.time)
}

fn accepted(c: &HeaderChain, id: BlockId) -> StatusEvent {
    let (best_header, time) = best(c, id);
    StatusEvent::HeaderAccepted {
        best_header,
        time,
    }
}

/// The facts are the snapshot's best header and its time, and its floor, active only while the
/// observed spork state is on and the snapshot's chain holds it at its height: a fork block, a
/// floor below an SPV window and a node before genesis read as inactive.
#[test]
fn status_facts_from_the_snapshot() {
    let mut c = node(4);
    let a = branch(&mut c, BlockId(0), 1, 3);
    let b = branch(&mut c, BlockId(0), 101, 2);
    c.set_active_tip(a[2]).expect("extends");
    let facts = status_facts(&c.snapshot());
    assert_eq!(facts.best_header, Some(best(&c, a[2])));
    assert_eq!(facts.cl, None);
    let floor = |c: &HeaderChain, enabled, base, id| {
        let mut s = ChainReadSnapshot::clone(&c.snapshot());
        if base > 0 {
            s.active.chain.set_base(base);
        }
        s.cl_enabled_observed = enabled;
        s.cl_floor = Some(bref(c, id));
        status_facts(&s).cl.expect("a floor")
    };
    assert_eq!(
        floor(&c, true, 0, a[1]),
        ClFact {
            block: bref(&c, a[1]),
            active: true
        }
    );
    assert!(!floor(&c, false, 0, a[1]).active, "the spork is off");
    assert!(!floor(&c, true, 0, b[1]).active, "a fork block");
    assert!(!floor(&c, true, 3, a[1]).active, "below the window");
    assert!(!floor(&chain(ChainConfig::node, 4), true, 0, BlockId(0)).active, "before genesis");
}

/// A reader that attaches before the cell's first `after_publish` starts at the first
/// snapshot's facts: the next publication, with the same best header, reports nothing.
#[test]
fn status_reader_seeded_before_first_publication() {
    let mut c = chain(ChainConfig::node, 4);
    let mut r = c.status_feed();
    c.connect_tip(BlockId(0)).expect("genesis");
    assert!(r.next_events().is_empty());
    let id = branch(&mut c, BlockId(0), 1, 1)[0];
    c.publish_status();
    assert_eq!(r.next_events().as_slice(), [accepted(&c, id)]);
}

/// SPV: a header batch publishes no status; the step's snapshot carries it.
#[test]
fn spv_status_rides_on_the_step() {
    let mut c = chain(|p| ChainConfig::spv(p, 10_000), 2);
    let mut r = c.status_feed();
    let a = branch(&mut c, BlockId(0), 1, 2);
    c.publish_status();
    assert!(r.next_events().is_empty());
    c.tip_plan(a[1]).expect("plans");
    c.set_active_tip(a[1]).expect("extends");
    assert_eq!(r.next_events().as_slice(), [accepted(&c, a[1])]);
}

/// A waker that runs inside the writer's publication, under the tip lock: it takes a status
/// reader's events with the published snapshot's best header at that moment.
struct Probe {
    status: Mutex<StatusReader>,
    reader: ChainReader,
    seen: Mutex<Vec<(StatusEvent, BlockId)>>,
}

impl Probe {
    fn new(c: &HeaderChain) -> Arc<Probe> {
        Arc::new(Probe {
            status: Mutex::new(c.status_feed()),
            reader: c.reader(),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn notify(self: &Arc<Self>) -> Notify {
        Notify::Task(Waker::from(self.clone()))
    }

    fn events(&self) -> Vec<StatusEvent> {
        self.seen.lock().iter().map(|(e, _)| e.clone()).collect()
    }
}

impl Wake for Probe {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let best = self.reader.snapshot().tip.best_header;
        let events = self.status.lock().next_events();
        self.seen.lock().extend(events.into_iter().map(|e| (e, best)));
    }
}

/// n7 (§4.3): the facts are stored after the snapshot, so a status wake already finds the
/// reported best header in the published snapshot.
#[test]
fn status_publish_order() {
    let mut c = node(4);
    let p = Probe::new(&c);
    assert!(!p.status.lock().arm(p.notify()));
    let mut tip = BlockId(0);
    let mut want = Vec::new();
    for k in 1..=3 {
        tip = branch(&mut c, tip, k, 1)[0];
        c.publish_status();
        want.push((accepted(&c, tip), tip));
    }
    assert_eq!(*p.seen.lock(), want);
}

/// The status publication precedes the fanout (§4.8's release order, N1): a subscriber woken by
/// a step's `Connected` already finds the step's status change.
#[test]
fn status_before_fanout() {
    let mut c = node(4);
    let h1 = branch(&mut c, BlockId(0), 1, 1)[0];
    let feeds = c.feeds();
    let (_, mut f, _) = subscribe(&feeds);
    let p = Probe::new(&c);
    f.set_waker(p.notify());
    assert!(matches!(f.next(), Next::Empty));
    c.connect_tip(h1).expect("connects");
    assert_eq!(p.events(), [accepted(&c, h1)]);
}
