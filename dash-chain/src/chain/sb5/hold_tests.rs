//! SB5 §4.7: the writer's drains through the held guard and through `enforce_block`'s composite
//! state (SB4 D-C6-1), `feed::after_step`'s per-disconnect handoff, and `h7_*`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Wake, Waker};
use std::time::Duration;

use dash_params::Network;

use crate::chain::tests::{branch, connect_all, with_cfg};
use crate::chainlock::ChainLockRef;
use crate::config::{ChainConfig, Services, StaticSporks};
use crate::error::ChainError;
use crate::feed::SubscriberFeed;
use crate::feed::{
    ChainEvent, ChainFeeds, FeedId, FeedSource, Handoff, Needs, Next, Publish, Refused,
    SubscribeError,
};
use crate::host::TipHost;
use crate::notify::{Notify, Parker};
use crate::reader::ChainReader;
use crate::residency::Permanent;
use crate::store::NoStore;
use crate::testing::{params, services};
use crate::tip::TipState;
use crate::{BlockId, HeaderChain};

pub(super) const T: u32 = 1_700_000_000;
/// A threaded test's bound on any one wait: only a failing run pays it.
pub(super) const WAIT: Duration = Duration::from_secs(30);

/// A node at genesis with feed capacity `q`, ChainLocks on once `sporks.set(true)`.
pub(super) fn node(q: u32) -> (HeaderChain, Arc<StaticSporks>) {
    let p = params(Network::Regtest);
    let sporks = Arc::new(StaticSporks::for_network(&p, Some(false)));
    let svc = Services {
        sporks: sporks.clone(),
        ..services(&p)
    };
    let mut cfg = ChainConfig::node(&p);
    cfg.feeds.capacity = q;
    let mut chain = HeaderChain::new_genesis(p, cfg, svc, NoStore).expect("a valid configuration");
    chain.init_active_tip(BlockId(0)).expect("Core LoadChainTip");
    (chain, sporks)
}

/// A node on B1…B3 with the floor on the header-only A3, A1 being B1's sibling: enforcing it
/// disconnects B3, B2, B1 (SB4 `cl_tests::locked_off_tip`).
pub(super) fn locked_off_tip(q: u32) -> (HeaderChain, Vec<BlockId>, Vec<BlockId>) {
    locked_off_tip_n(q, 3, 100)
}

/// As `locked_off_tip`, with an `n`-block losing branch under hashes `fake_hash(seed..)`.
pub(super) fn locked_off_tip_n(
    q: u32,
    n: u64,
    seed: u64,
) -> (HeaderChain, Vec<BlockId>, Vec<BlockId>) {
    let (mut chain, sporks) = node(q);
    let a = branch(&mut chain, BlockId(0), 1, 3, T);
    let b = branch(&mut chain, BlockId(0), seed, n, T + 1);
    connect_all(&mut chain, &b);
    sporks.set(true);
    chain.check_cl_active_state();
    let lock = lock_at(&chain.reader(), a[2]);
    chain.record_chainlock(lock);
    (chain, a, b)
}

/// A lock on `id` at its own height.
pub(super) fn lock_at(r: &ChainReader, id: BlockId) -> ChainLockRef {
    let e = r.entry(id).expect("resident");
    ChainLockRef {
        height: e.height,
        hash: e.hash,
    }
}

/// A live subscriber at the published tip.
pub(super) fn subscribe(feeds: &ChainFeeds) -> (FeedId, Box<dyn SubscriberFeed>) {
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(feeds).tip();
    match feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(())) {
        Ok(Handoff::Live {
            feed,
            ..
        }) => (id, feed),
        other => panic!("subscribe_at: {:?}", other.err()),
    }
}

/// Parks on `p` until `next()` selects an event, which it leaves selected.
pub(super) fn await_event(f: &mut dyn SubscriberFeed, p: &Parker, who: &str) {
    loop {
        match f.next() {
            Next::Event(_) => return,
            Next::Empty => assert!(p.park_timeout(WAIT), "{who} was never woken"),
            Next::Control(_) => {}
        }
    }
}

/// Runs `f` on another thread; it must return while the caller holds the tip mutex.
pub(super) fn elsewhere<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    rx.recv_timeout(WAIT).expect("returns during the hold")
}

/// Every event the subscriber can take now.
pub(super) fn take(f: &mut dyn SubscriberFeed) -> Vec<ChainEvent> {
    let mut out = Vec::new();
    while let Next::Event(_) = f.next() {
        out.push(f.pop().expect("selected"));
    }
    out
}

/// A wake that counts.
#[derive(Default)]
pub(super) struct Count(AtomicUsize);

impl Count {
    pub(super) fn notify(self: &Arc<Self>) -> Notify {
        Notify::Task(Waker::from(self.clone()))
    }

    pub(super) fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl Wake for Count {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// V in `enforce_block`: each `DisconnectTip` steps, runs `on_step`, then drains (§4.7), with
/// `on_block` run on each `Blocked` drain.
struct Drain<'f> {
    feed: Option<&'f mut dyn SubscriberFeed>,
    /// Per disconnect: the step, what the subscriber could take right after it, and the
    /// published `tip_seq` then.
    seen: Vec<(ChainEvent, Vec<ChainEvent>, u64)>,
    on_step: Box<dyn FnMut() + 'f>,
    on_block: Box<dyn FnMut() + 'f>,
    blocked: usize,
}

impl<'f> Drain<'f> {
    fn new(feed: Option<&'f mut dyn SubscriberFeed>) -> Drain<'f> {
        Drain {
            feed,
            seen: Vec::new(),
            on_step: Box::new(|| {}),
            on_block: Box::new(|| {}),
            blocked: 0,
        }
    }
}

impl TipHost<NoStore, Permanent> for Drain<'_> {
    type Error = ChainError;

    fn tip(&self, chain: &HeaderChain) -> BlockId {
        chain.private_tip().expect("a tip").0.id
    }

    fn contains(&self, chain: &HeaderChain, id: BlockId) -> Result<bool, ChainError> {
        Ok(chain.active_contains_private(id))
    }

    fn disconnect_tip(&mut self, chain: &mut HeaderChain) -> Result<(), ChainError> {
        let step = chain.disconnect_tip()?;
        let got = self.feed.as_mut().map(|f| take(&mut **f)).unwrap_or_default();
        self.seen.push((step, got, chain.snapshot().tip_seq));
        (self.on_step)();
        while chain.retry_pending() == Publish::Blocked {
            self.blocked += 1;
            assert!(self.blocked < 100, "the drain never ends");
            (self.on_block)();
        }
        assert!(chain.pending_is_empty(), "no step while pending (§3.3)");
        Ok(())
    }

    fn conflicting_marked(
        &mut self,
        _: &mut HeaderChain,
        _: BlockId,
        _: bool,
    ) -> Result<(), ChainError> {
        Ok(())
    }
}

/// SB4 D-C6-1, A-39's feed half: inside `enforce_block` each disconnect's event reaches the
/// subscriber at once, while readers keep the pre-walk snapshot; the walk's one snapshot fans
/// nothing more out.
#[test]
fn enforce_block_hands_each_disconnect_to_the_feeds() {
    let (mut chain, _, b) = locked_off_tip(8);
    let feeds = chain.feeds();
    let (_, mut f) = subscribe(&feeds);
    let before = chain.snapshot().tip_seq;
    let mut v = Drain::new(Some(&mut *f));
    let r = chain.enforce_best_chainlock(&mut v).expect("enforces").expect("not skipped");
    assert_eq!(r.disconnected, [b[2], b[1], b[0]]);
    assert_eq!(v.seen.len(), 3);
    for (k, (step, got, published)) in v.seen.iter().enumerate() {
        assert_eq!((step.seq(), step.block().id), (before + 1 + k as u64, b[2 - k]));
        assert_eq!(got, &[*step], "fanned out in the step");
        assert_eq!(*published, before, "no snapshot inside the walk");
    }
    assert_eq!(chain.snapshot().tip_seq, before + 3);
    let f = v.feed.take().expect("the subscriber");
    assert!(take(f).is_empty(), "the walk's snapshot fans nothing out");
}

/// `h7_closing_retires_during_hold` (§10.1): in `enforce_block` with three disconnects and a
/// full target T, T's hub deregisters while V drains under the guard. `deregister` returns at
/// once (its `try_lock` fails); V's `retry_pending` clears T's cursor and reclaims the entry,
/// and the enforcement proceeds. W's close hook is notified once.
#[test]
fn h7_closing_retires_during_hold() {
    let (mut chain, _, b) = locked_off_tip(1);
    let feeds = chain.feeds();
    let w = Arc::new(Count::default());
    feeds.set_close_hook(w.notify());
    let (t, _reader) = subscribe(&feeds);
    let hub = feeds.clone();
    let mut v = Drain::new(None);
    v.on_block = Box::new(move || {
        std::thread::scope(|s| s.spawn(|| hub.deregister(t)).join()).expect("returns");
    });
    let r = chain.enforce_best_chainlock(&mut v).expect("enforces").expect("not skipped");
    assert_eq!(r.disconnected, [b[2], b[1], b[0]]);
    assert_eq!(v.blocked, 1, "blocked once, at the second disconnect");
    assert!(feeds.registry().feeds.is_empty(), "T's entry is reclaimed");
    assert_eq!(w.get(), 1);
}

/// §4.7: inside a hold, `retry_pending` and `pending_is_empty` run on the held guard. A step's
/// event left pending before the hold is pushed through it once the target takes, without
/// re-locking the tip mutex (which would deadlock).
#[test]
fn hold_drains_through_the_held_guard() {
    let (mut chain, _) = node(1);
    let a = branch(&mut chain, BlockId(0), 1, 2, T);
    let feeds = chain.feeds();
    let (_, mut f) = subscribe(&feeds);
    let s1 = chain.connect_tip(a[0]).expect("connects");
    let s2 = chain.connect_tip(a[1]).expect("connects");
    assert!(!chain.pending_is_empty());

    let mut hold = chain.hold_tip();
    assert!(!hold.pending_is_empty());
    assert_eq!(hold.retry_pending(), Publish::Blocked);
    assert_eq!(take(&mut *f), [s1]);
    assert_eq!(hold.retry_pending(), Publish::Done);
    assert!(hold.pending_is_empty());
    assert_eq!(take(&mut *f), [s2]);
    hold.release();
    assert!(chain.pending_is_empty());
}

// ---- the seams in SB4's files (D56): each test fails if its call is removed ----------------

/// `register` from another thread. A `register` that spins on the tip mutex instead of
/// refusing fails the test (and the unwind ends the hold, which lets it finish).
fn register_elsewhere(feeds: &ChainFeeds) -> Result<FeedId, Refused> {
    let feeds = feeds.clone();
    elsewhere(move || feeds.register(Needs::default()))
}

/// DEC-157 (2), `hold_owner_refused`: during a hold `register` returns `HoldActive` on another
/// thread and `WouldDeadlock` on the owner's, and succeeds after the release. A second hold
/// finds the owner cleared (`hold_opened` asserts that holds do not nest).
#[test]
fn register_is_refused_during_a_hold() {
    let (mut chain, _) = node(8);
    let feeds = chain.feeds();
    for _ in 0..2 {
        let hold = chain.hold_tip();
        assert_eq!(register_elsewhere(&feeds), Err(Refused::HoldActive));
        assert_eq!(feeds.register(Needs::default()), Err(Refused::WouldDeadlock));
        hold.release();
        feeds.register(Needs::default()).expect("after the release");
    }
}

/// A `register` or `subscribe_at` from inside a handoff's `finish()` is `WouldDeadlock` on the
/// subscribing thread, where it spun on the tip lock that thread holds (review F-1). The owner
/// is cleared after the handoff, a failed or panicking `finish()` included.
#[test]
fn reentry_from_finish_is_refused() {
    let (chain, _) = node(8);
    let feeds = chain.feeds();
    let first = feeds.register(Needs::default()).expect("registers");
    let second = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(&feeds).tip();
    let f = feeds.clone();
    let (live, inner, after, unwound) = elsewhere(move || {
        let mut inner = None;
        let out = FeedSource::<()>::subscribe_at(&f, first, &tip, &mut || {
            let again = FeedSource::<()>::subscribe_at(&f, second, &tip, &mut || Ok(()));
            inner = Some((f.register(Needs::default()), again.err()));
            Ok(())
        });
        let live = matches!(out, Ok(Handoff::Live { .. }));
        let failed = FeedSource::<()>::subscribe_at(&f, second, &tip, &mut || Err(()));
        let after = (failed.err(), f.register(Needs::default()).is_ok());
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            FeedSource::<()>::subscribe_at(&f, second, &tip, &mut || panic!("finish panics"))
        }));
        let again = FeedSource::<()>::subscribe_at(&f, second, &tip, &mut || Err(()));
        let unwound = (panicked.is_err(), f.register(Needs::default()).is_ok(), again.err());
        (live, inner, after, unwound)
    });
    assert!(live, "the outer handoff completes");
    assert_eq!(
        inner,
        Some((Err(Refused::WouldDeadlock), Some(SubscribeError::WouldDeadlock))),
        "re-entry refuses on the subscribing thread"
    );
    assert_eq!(after, (Some(SubscribeError::Finish(())), true), "the owner is cleared");
    assert_eq!(
        unwound,
        (true, true, Some(SubscribeError::Finish(()))),
        "the owner is cleared on unwind"
    );
    let handoff = FeedSource::<()>::subscribe_at(&feeds, second, &tip, &mut || Ok(()));
    assert!(matches!(handoff, Ok(Handoff::Live { .. })), "another thread subscribes after");
}

/// A release wake that records whether the tip mutex was free when it fired.
struct TryLock {
    state: Arc<parking_lot::Mutex<TipState>>,
    seen: crate::sync::Mutex<Vec<bool>>,
}

impl TryLock {
    fn new(chain: &HeaderChain) -> Arc<TryLock> {
        Arc::new(TryLock {
            state: chain.shared.readers.tip().state.clone(),
            seen: crate::sync::Mutex::new(Vec::new()),
        })
    }

    fn notify(self: &Arc<Self>) -> Notify {
        Notify::Task(Waker::from(self.clone()))
    }

    fn seen(&self) -> Vec<bool> {
        self.seen.lock().clone()
    }
}

impl Wake for TryLock {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let free = self.state.try_lock().is_some();
        self.seen.lock().push(free);
    }
}

/// The node's `enforce_block` is a hold for `register` too (h7, A-40/A-41): refused from both
/// threads at each disconnect; its release wake fires once, after the mutex is free; and the
/// owner is cleared at the end.
#[test]
fn register_is_refused_during_enforce_block() {
    let (mut chain, _, _) = locked_off_tip(8);
    let feeds = chain.feeds();
    let wake = TryLock::new(&chain);
    let (owner, w) = (feeds.clone(), wake.clone());
    let mut refusals = Vec::new();
    let mut v = Drain::new(None);
    v.on_step = Box::new(|| {
        refusals.push((register_elsewhere(&owner), owner.register(Needs::default())));
        owner.on_release(w.notify());
    });
    chain.enforce_best_chainlock(&mut v).expect("enforces").expect("not skipped");
    drop(v);
    let refused = (Err(Refused::HoldActive), Err(Refused::WouldDeadlock));
    assert_eq!(refusals, [refused; 3]);
    assert_eq!(wake.seen(), [true; 3], "every waiter, once the mutex is free");
    chain.hold_tip().release();
    feeds.register(Needs::default()).expect("after the walk");
}

/// A walk that unwinds still clears the owner and fires its release wakes once the mutex is
/// free, so no `register` waits on a hold that ended (SB4 D-C6-1's latch trips).
#[test]
fn enforce_block_unwind_fires_release_wakes() {
    let (mut chain, _, _) = locked_off_tip(8);
    let feeds = chain.feeds();
    let wake = TryLock::new(&chain);
    let (owner, w) = (feeds.clone(), wake.clone());
    let mut v = Drain::new(None);
    v.on_step = Box::new(move || {
        owner.on_release(w.notify());
        panic!("the host fails inside the walk");
    });
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = chain.enforce_best_chainlock(&mut v);
    }));
    assert!(unwound.is_err() && chain.svc.fail.is_tripped());
    assert_eq!(wake.seen(), [true], "fired once, after the guard dropped");
    assert_eq!(register_elsewhere(&feeds), Err(Refused::Latched));
}

/// §4.7: inside a hold each node step fans out in the step, while readers keep the pre-hold
/// snapshot; the release fans nothing more out.
#[test]
fn hold_steps_fan_out_per_step() {
    let (mut chain, _) = node(8);
    let a = branch(&mut chain, BlockId(0), 1, 2, T);
    chain.connect_tip(a[0]).expect("connects");
    let feeds = chain.feeds();
    let (_, mut f) = subscribe(&feeds);
    let reader = chain.reader();
    let before = reader.snapshot();

    let mut hold = chain.hold_tip();
    let up = hold.connect_tip(a[1]).expect("connects");
    assert_eq!(take(&mut *f), [up]);
    let down = hold.disconnect_tip().expect("disconnects");
    assert_eq!(take(&mut *f), [down]);
    assert!(Arc::ptr_eq(&reader.snapshot(), &before), "nothing published in the hold");
    let snap = hold.release();
    assert_eq!((snap.tip.tip, snap.tip_seq), (a[0], down.seq()));
    assert!(take(&mut *f).is_empty());
}

/// §4.8 step 4: a hold's release wakes fire after its guard drops, on `release` and on
/// `abandon`.
#[test]
fn release_wakes_after_the_guard_drops() {
    let (mut chain, _) = node(8);
    let feeds = chain.feeds();
    let wake = TryLock::new(&chain);
    let hold = chain.hold_tip();
    feeds.on_release(wake.notify());
    hold.release();
    assert_eq!(wake.seen(), [true]);
    feeds.on_release(wake.notify());
    chain.hold_tip().abandon();
    assert_eq!(wake.seen(), [true, true]);
}

/// The writer's rule (§3.3): with a live target lacking an event, every tip step is refused:
/// `Internal` in release builds, a `debug_assert` panic in debug builds.
fn refused<T: std::fmt::Debug>(f: impl FnOnce() -> Result<T, ChainError>) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    if cfg!(debug_assertions) {
        let panic = r.expect_err("debug builds assert");
        let msg = panic.downcast_ref::<String>().map(String::as_str);
        let msg = msg.or_else(|| panic.downcast_ref::<&str>().copied());
        assert!(msg.is_some_and(|m| m.contains("a pending fanout")), "{msg:?}");
    } else {
        let r = r.expect("release builds return the error");
        assert!(matches!(r, Err(ChainError::Internal("tip step with a pending fanout"))), "{r:?}");
    }
}

#[test]
fn no_step_while_pending() {
    let (mut chain, _) = node(1);
    let a = branch(&mut chain, BlockId(0), 1, 3, T);
    let (_, _f) = subscribe(&chain.feeds());
    connect_all(&mut chain, &a[..2]);
    assert!(!chain.pending_is_empty());
    refused(|| chain.connect_tip(a[2]));
    refused(|| chain.disconnect_tip());
    let mut hold = chain.hold_tip();
    refused(|| hold.connect_tip(a[2]));
    refused(|| hold.disconnect_tip());
    hold.abandon();

    let mut chain = with_cfg(|p| {
        let mut cfg = ChainConfig::spv(p, 10_000);
        cfg.feeds.capacity = 1;
        cfg
    });
    let a = branch(&mut chain, BlockId(0), 1, 3, T);
    let (_, _f) = subscribe(&chain.feeds());
    chain.tip_plan(a[2]).expect("plan");
    chain.set_active_tip(a[2]).expect("a composite of three");
    assert!(!chain.pending_is_empty());
    chain.tip_plan(a[2]).expect("an empty plan");
    refused(|| chain.set_active_tip(a[2]));
    refused(|| chain.hold_tip().set_active_tip(a[2], 1));
}

/// Through [`Hold::chain`](crate::chain::Hold::chain), `HeaderChain`'s drains refuse to re-lock
/// the held tip mutex: they panic where the lock would deadlock.
#[test]
fn chain_drains_refused_inside_a_hold() {
    let (mut chain, _) = node(1);
    let mut hold = chain.hold_tip();
    let c = hold.chain();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.pending_is_empty()));
    assert!(r.is_err(), "pending_is_empty");
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.retry_pending()));
    assert!(r.is_err(), "retry_pending");
    hold.release();
    assert!(chain.pending_is_empty());
}
