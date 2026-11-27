//! Chain subscribers (PLAN-r1 §2.4, §4.3): notices fenced on the published snapshot.

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::time::{Duration, Instant};

use dash_chain::{FeedConfig, FeedId};
use dash_clock::MockClock;
use tokio::sync::{watch, Notify as Signal};
use tokio::task::JoinHandle;

use super::step::{bridged, mine, PEER};
use super::*;
use crate::client::chain::sub::{SubEntry, SubTable};

/// A host on a memory store at the 20000 pack, its clock past every mined header, with
/// `Q = E = q` and window `w`. It is Established, so it cannot be replaced.
pub(super) struct Rig {
    pub(super) host: SpvChainHost,
    pub(super) setup: ChainHostSetup,
    _dir: tempfile::TempDir,
}

pub(super) async fn rig(q: u32, w: u32) -> Rig {
    rig_on(false, q, w).await
}

/// `rig`, with masternodes on when `mn`: a Candidate store, which can be replaced.
async fn rig_on(mn: bool, q: u32, w: u32) -> Rig {
    rig_lag(mn, q, w, |_| {}).await
}

/// `rig_on`, its lag budget changed by `lag`.
pub(super) async fn rig_lag(mn: bool, q: u32, w: u32, lag: impl FnOnce(&mut LagBudget)) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    rig_in(h, dir, mn, q, w, lag).await
}

/// `rig_lag` on the store `h`, in `dir`.
pub(super) async fn rig_in<H: StoreHandle>(
    h: H,
    dir: tempfile::TempDir,
    mn: bool,
    q: u32,
    w: u32,
    lag: impl FnOnce(&mut LagBudget),
) -> Rig {
    let clock = Arc::new(MockClock::at(2_000_000));
    let mut setup = setup(mn, None);
    setup.config.clock = clock.clone();
    setup.feeds = FeedConfig {
        capacity: q,
        segment_events: q,
        ..FeedConfig::spv()
    };
    setup.window = w;
    setup.lag = LagBudget::spv(&setup.feeds);
    lag(&mut setup.lag);
    let host = SpvChainHost::open_on(h, setup.clone()).await.unwrap();
    clock.set_mocktime(10_000_000);
    Rig {
        host,
        setup,
        _dir: dir,
    }
}

impl Rig {
    pub(super) async fn send(&self, headers: &[Header]) -> HeadersReply {
        let reply = self.host.handle().headers(PEER, bridged(headers)).await.unwrap();
        assert_eq!(reply.stop, None);
        reply
    }
}

pub(super) fn anchor() -> AnchorPack {
    pack(20000, 1_000_000)
}

pub(super) async fn next(sub: &mut ChainSub) -> ChainNotice {
    match tokio::time::timeout(Duration::from_secs(60), sub.next()).await {
        Ok(Ok(SubItem::Notice(n))) => n,
        other => panic!("{other:?}"),
    }
}

/// A notice's snapshot is the one it names: `snap.tip_seq == to_seq`, its tip the new tip.
pub(super) fn fenced(n: &ChainNotice) {
    match n {
        ChainNotice::Advanced {
            to,
            to_seq,
            pin,
            ..
        }
        | ChainNotice::Reorg {
            new_tip: to,
            to_seq,
            pin,
            ..
        } => {
            assert_eq!(pin.snap.tip_seq, *to_seq, "{n:?}");
            assert_eq!(pin.snap.tip.tip, to.id, "{n:?}");
        }
        ChainNotice::Resync {
            ..
        } => {}
    }
}

#[tokio::test]
async fn the_first_notice_is_a_resync_then_each_step_advances() {
    let r = rig(1024, HEADER_WINDOW).await;
    let mut sub = r.host.subscribe(SubKind::Filters);
    let ChainNotice::Resync {
        pin,
        why: ResyncWhy::Start,
    } = next(&mut sub).await
    else {
        panic!("not a start");
    };
    assert_eq!((pin.gen, pin.snap.tip.tip_hash), (ChainGen(1), anchor().hash));
    let a = mine(anchor().hash, 1_000_000, 5, 1);
    let reply = r.send(&a).await;
    let n = next(&mut sub).await;
    fenced(&n);
    let ChainNotice::Advanced {
        from,
        to,
        ..
    } = n
    else {
        panic!("{n:?}");
    };
    assert_eq!((from.hash, to.id), (anchor().hash, reply.snap.tip.tip));
}

/// PLAN-r1 §4.3: Q = E = 4, w = 2,048 and a reorg of 2,900 blocks in one step, the headers
/// sent one step at a time. A fast and a slow subscriber get notices whose snapshot is a step's
/// release, the one its reply names, so none from inside a step's hold, where the published
/// snapshot is still the pre-step one; their `to_seq`s rise. The fast one has reached `a`'s
/// tip before `b` is sent, so it gets the reorg, whose fork is the last common ancestor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn notices_pair_with_post_step_snapshot() {
    let r = rig(4, 2048).await;
    let a = mine(anchor().hash, 1_000_000, 3000, 1);
    let b = mine(a[99].hash(), 1_000_000 + 100 * step::SPACING, 2950, 2);
    let mut owners = Vec::new();
    let (at_a, reached_a) = tokio::sync::oneshot::channel();
    let mut at_a = Some(at_a);
    for (kind, pace) in [(SubKind::Filters, 0), (SubKind::Blocks, 20)] {
        let mut sub = r.host.subscribe(kind);
        next(&mut sub).await;
        let (until, tip_a) = (b[2949].hash(), a[2999].hash());
        let mut at_a = at_a.take();
        owners.push(tokio::spawn(async move {
            let mut seen = Vec::new();
            loop {
                tokio::time::sleep(Duration::from_millis(pace)).await;
                let n = next(&mut sub).await;
                fenced(&n);
                let (ChainNotice::Advanced {
                    to,
                    ..
                }
                | ChainNotice::Reorg {
                    new_tip: to,
                    ..
                }) = &n
                else {
                    panic!("{n:?}");
                };
                let done = to.hash == until;
                if let Some(s) = at_a.take_if(|_| to.hash == tip_a) {
                    s.send(()).unwrap();
                }
                seen.push(n);
                if done {
                    return seen;
                }
            }
        }));
    }
    let step = r.setup.feeds.step_headers as usize;
    let mut released = std::collections::BTreeSet::new();
    for batch in a.chunks(step) {
        released.insert(r.send(batch).await.snap.tip_seq);
    }
    let tip_a = *released.last().unwrap();
    reached_a.await.unwrap();
    let mut tip_b = tip_a;
    for batch in b.chunks(step) {
        tip_b = r.send(batch).await.snap.tip_seq;
        released.insert(tip_b);
    }
    for (fast, owner) in [true, false].into_iter().zip(owners) {
        let seen = owner.await.unwrap();
        let seqs: Vec<u64> = seen
            .iter()
            .map(|n| match n {
                ChainNotice::Advanced {
                    to_seq,
                    ..
                }
                | ChainNotice::Reorg {
                    to_seq,
                    ..
                } => *to_seq,
                ChainNotice::Resync {
                    ..
                } => unreachable!(),
            })
            .collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
        assert!(seqs.iter().all(|s| released.contains(s)), "{seqs:?} not in {released:?}");
        assert_eq!(*seqs.last().unwrap(), tip_b);
        if fast || seqs.contains(&tip_a) {
            let Some(ChainNotice::Reorg {
                fork,
                disconnected,
                connected,
                ..
            }) = seen.last()
            else {
                panic!("{:?}", seen.last());
            };
            assert_eq!(fork.hash, a[99].hash(), "the last common ancestor");
            assert_eq!((*disconnected, *connected), (2900, 2950));
        }
    }
    assert!(!r.setup.fail.is_tripped());
}

/// A feed closed under its subscriber gets one `Resync { Lag }` at the latest tip, then
/// notices as before.
#[tokio::test]
async fn a_closed_feed_resyncs_once_at_the_latest_tip() {
    let r = rig(1024, HEADER_WINDOW).await;
    let mut sub = r.host.subscribe(SubKind::Masternodes);
    next(&mut sub).await;
    let a = mine(anchor().hash, 1_000_000, 10, 1);
    r.send(&a[..5]).await;
    for e in r.host.subs.live(ChainGen(1)) {
        e.feeds.deregister(e.id);
    }
    let reply = r.send(&a[5..]).await;
    let n = next(&mut sub).await;
    let ChainNotice::Resync {
        pin,
        why: ResyncWhy::Lag,
    } = &n
    else {
        panic!("{n:?}");
    };
    assert_eq!(pin.snap.tip_seq, reply.snap.tip_seq);
    let more = mine(a[9].hash(), 1_000_000 + 10 * step::SPACING, 1, 1);
    r.send(&more).await;
    let n = next(&mut sub).await;
    assert!(matches!(n, ChainNotice::Advanced { .. }), "{n:?}");
    assert_eq!(r.host.subs.live(ChainGen(1)).len(), 1);
}

/// A sub's test hooks: its `register` and `deregister` calls in order, and holds at points of
/// `next()`.
#[derive(Default)]
pub(crate) struct Hooks {
    pub(crate) log: Mutex<Vec<Slot>>,
    holds: Mutex<Vec<(At, Hold)>>,
    /// The hook of the next pump `next()` starts.
    pub(crate) pump: Mutex<Option<PumpHook>>,
}

/// What a pump does after each event it pops.
pub(crate) enum PumpHook {
    /// Sleeps this long: a slow owner's pump.
    Throttle(Duration),
    /// Awaits this before its first pop: a pump stuck on another wait, which pops nothing.
    Stall(Pin<Box<dyn Future<Output = ()> + Send>>),
    /// At its first park, polls the feed once, signals `parked`, then awaits `until` without
    /// polling it again: a pump woken on a refilled slot but not run, whose `parked` flag stays
    /// set. After `until`, it runs; with `stall`, it stalls for good after its next pop.
    Unpolled {
        until: Pin<Box<dyn Future<Output = ()> + Send>>,
        parked: Arc<Signal>,
        stall: bool,
    },
    /// Stalls for good after its next pop.
    Stuck,
}

impl PumpHook {
    pub(crate) async fn start(hook: &mut Option<PumpHook>) {
        if let Some(PumpHook::Stall(f)) = hook.take_if(|h| matches!(h, PumpHook::Stall(_))) {
            f.await
        }
    }

    pub(crate) async fn after_pop(hook: &mut Option<PumpHook>) {
        match hook {
            Some(PumpHook::Throttle(d)) => tokio::time::sleep(*d).await,
            Some(PumpHook::Stuck) => std::future::pending().await,
            _ => {}
        }
    }

    pub(crate) async fn park(hook: &mut Option<PumpHook>, wait: impl Future<Output = ()>) {
        let mut wait = std::pin::pin!(wait);
        let Some(PumpHook::Unpolled {
            until,
            parked,
            stall,
        }) = hook.take_if(|h| matches!(h, PumpHook::Unpolled { .. }))
        else {
            return wait.await;
        };
        if std::future::poll_fn(|cx| Poll::Ready(wait.as_mut().poll(cx).is_ready())).await {
            return;
        }
        parked.notify_one();
        until.await;
        if stall {
            *hook = Some(PumpHook::Stuck);
        }
        wait.await
    }
}

/// The lag budget's record: each step's parked time, its parks and closes in order with the
/// limit that closed each, a signal each time the writer parks, and a hook the writer runs after
/// each park and each close.
#[derive(Default)]
pub(crate) struct LagLog {
    steps: Mutex<Vec<Duration>>,
    log: Mutex<Vec<Lag>>,
    pub(crate) parks: Signal,
    pub(crate) on_lag: Mutex<Option<OnLag>>,
}

/// What the writer runs after it parks or closes a pump.
pub(crate) type OnLag = Box<dyn FnMut(Lag) + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lag {
    Park,
    Closed(SubKind, &'static str),
}

impl LagLog {
    pub(crate) fn step(&self) {
        self.steps.lock().push(Duration::ZERO);
    }

    pub(crate) fn parked(&self, d: Duration) {
        *self.steps.lock().last_mut().expect("a step") += d;
        self.log.lock().push(Lag::Park);
        self.parks.notify_one();
        self.on(Lag::Park);
    }

    pub(crate) fn closed(&self, kind: SubKind, limit: &'static str) {
        self.log.lock().push(Lag::Closed(kind, limit));
        self.on(Lag::Closed(kind, limit));
    }

    fn on(&self, l: Lag) {
        if let Some(f) = self.on_lag.lock().as_mut() {
            f(l)
        }
    }

    /// The longest any one step parked.
    pub(crate) fn most(&self) -> Duration {
        self.steps.lock().iter().copied().max().unwrap_or_default()
    }

    pub(crate) fn closes(&self) -> Vec<(SubKind, &'static str)> {
        let log = self.log.lock();
        log.iter()
            .filter_map(|l| match *l {
                Lag::Closed(kind, limit) => Some((kind, limit)),
                Lag::Park => None,
            })
            .collect()
    }

    pub(crate) fn log(&self) -> Vec<Lag> {
        self.log.lock().clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum At {
    /// Before a live sub waits for its pump or the source.
    Parked,
    /// In the `Replacing` phase, before it waits for the source.
    Replacing,
    BeforeRegister,
    BeforeRecheck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Reg(FeedId),
    Dereg(FeedId),
}

/// The sub reached the hold's point; it stays there until `release`.
#[derive(Clone)]
struct Hold {
    reached: Arc<Signal>,
    release: Arc<Signal>,
}

impl Hooks {
    /// Holds the sub's next pass through `at`.
    fn hold(&self, at: At) -> Hold {
        let h = Hold {
            reached: Arc::new(Signal::new()),
            release: Arc::new(Signal::new()),
        };
        self.holds.lock().push((at, h.clone()));
        h
    }

    /// Signals the sub's next pass through `at`, and lets it go on.
    fn pass(&self, at: At) -> Arc<Signal> {
        let h = self.hold(at);
        h.release.notify_one();
        h.reached
    }

    pub(crate) async fn at(&self, at: At) {
        let hold = {
            let mut holds = self.holds.lock();
            holds.iter().position(|h| h.0 == at).map(|i| holds.remove(i))
        };
        if let Some((_, h)) = hold {
            h.reached.notify_one();
            h.release.notified().await;
        }
    }

    fn log(&self) -> Vec<Slot> {
        self.log.lock().clone()
    }

    /// The ids registered, in order.
    fn regs(&self) -> Vec<FeedId> {
        self.log()
            .into_iter()
            .filter_map(|s| match s {
                Slot::Reg(id) => Some(id),
                Slot::Dereg(_) => None,
            })
            .collect()
    }

    /// Every slot but the last registered was deregistered once, right after it: the log of a
    /// sub fenced out of each generation but its last.
    fn fenced_out(&self) {
        let regs = self.regs();
        let mut want: Vec<Slot> =
            regs.iter().flat_map(|&id| [Slot::Reg(id), Slot::Dereg(id)]).collect();
        want.pop();
        assert_eq!(self.log(), want);
    }
}

type Next = JoinHandle<(ChainSub, Result<SubItem, SubError>)>;

fn spawn_next(mut sub: ChainSub) -> Next {
    tokio::spawn(async move {
        let out = sub.next().await;
        (sub, out)
    })
}

async fn bound<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), f).await.expect("within the test's bound")
}

/// Waits for `at`; the sub's `next()` must not return first.
async fn reach(at: &Signal, task: &mut Next) {
    tokio::select! {
        _ = bound(at.notified()) => {}
        out = task => panic!("returned first: {:?}", out.map(|o| o.1)),
    }
}

async fn returned(task: Next) -> (ChainSub, Result<SubItem, SubError>) {
    bound(task).await.unwrap()
}

fn resync(out: Result<SubItem, SubError>) -> (ChainGen, ResyncWhy) {
    match out {
        Ok(SubItem::Notice(ChainNotice::Resync {
            pin,
            why,
        })) => (pin.gen, why),
        other => panic!("{other:?}"),
    }
}

/// A sub on a source the test publishes, starting as the host's.
fn hand(r: &Rig) -> (watch::Sender<ChainSource>, ChainSub) {
    let (tx, rx) = watch::channel(r.host.source().borrow().clone());
    (tx, ChainSub::new(SubKind::Filters, rx, Arc::clone(&r.host.subs)))
}

/// Publishes the host's chain as generation `gen`.
fn relive(r: &Rig, tx: &watch::Sender<ChainSource>, gen: u64) {
    let mut s = r.host.source().borrow().clone();
    s.gen = ChainGen(gen);
    tx.send_replace(s);
}

fn replacing(tx: &watch::Sender<ChainSource>) {
    tx.send_modify(|s| *s = s.with(SourceState::Replacing));
}

/// At the `Replacing` wait: the sub's one slot is deregistered, and its pump is gone from the
/// table.
fn detached(r: &Rig, hooks: &Hooks) {
    let [Slot::Reg(a), Slot::Dereg(b)] = hooks.log()[..] else {
        panic!("{:?}", hooks.log());
    };
    assert_eq!(a, b);
    assert!(r.host.subs.live(ChainGen(1)).is_empty());
}

/// The one live slot of the host's generation 1, closed as the lag budget closes it.
fn close_for_lag(r: &Rig) -> FeedId {
    let [e] = &r.host.subs.live(ChainGen(1))[..] else {
        panic!("one live sub");
    };
    e.close();
    e.id
}

/// PLAN-r1 §4.3, mutant #14: subscribers parked in `next()` each return one `Resync` of the
/// new generation once a `Replace` publishes it, then its notices.
#[tokio::test]
async fn replacement_wakes_subscribers_parked_in_next() {
    let r = rig_on(true, 1024, HEADER_WINDOW).await;
    let mut parked = Vec::new();
    for kind in [SubKind::FilterHeaders, SubKind::Filters, SubKind::Blocks, SubKind::Masternodes] {
        let mut sub = r.host.subscribe(kind);
        next(&mut sub).await;
        let at = sub.hooks.pass(At::Parked);
        let mut task = spawn_next(sub);
        reach(&at, &mut task).await;
        parked.push(task);
    }
    let gen2 = pack(10000, 1_000_000);
    let out = r.host.handle().replace(pack_anchor(&gen2)).await;
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    let mut subs = Vec::new();
    for task in parked {
        let (sub, out) = returned(task).await;
        assert_eq!(resync(out), (ChainGen(2), ResyncWhy::Generation));
        subs.push(sub);
    }
    r.send(&mine(gen2.hash, 1_000_000, 3, 1)).await;
    for mut sub in subs {
        let n = next(&mut sub).await;
        fenced(&n);
        let ChainNotice::Advanced {
            from,
            pin,
            ..
        } = n
        else {
            panic!("{n:?}");
        };
        assert_eq!((from.hash, pin.gen), (gen2.hash, ChainGen(2)));
    }
}

/// PLAN-r1 §4.3: notices the old generation left undelivered are dropped; the sub's next
/// notice is the new generation's `Resync`, and every one after it is of that generation.
#[tokio::test]
async fn replacement_discards_queued_old_notices() {
    let r = rig_on(true, 1024, HEADER_WINDOW).await;
    let mut sub = r.host.subscribe(SubKind::Blocks);
    next(&mut sub).await;
    let a = mine(anchor().hash, 1_000_000, 6, 1);
    r.send(&a[..3]).await;
    r.send(&a[3..]).await;
    let gen2 = pack(10000, 1_000_000);
    let out = r.host.handle().replace(pack_anchor(&gen2)).await;
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    let ChainNotice::Resync {
        pin,
        why: ResyncWhy::Generation,
    } = next(&mut sub).await
    else {
        panic!("not a generation resync");
    };
    assert_eq!((pin.gen, pin.snap.tip.tip_hash), (ChainGen(2), gen2.hash));
    let b = mine(gen2.hash, 1_000_000, 4, 1);
    r.send(&b[..2]).await;
    r.send(&b[2..]).await;
    let mut to = gen2.hash;
    while to != b[3].hash() {
        let n = next(&mut sub).await;
        fenced(&n);
        let ChainNotice::Advanced {
            from,
            to: t,
            pin,
            ..
        } = n
        else {
            panic!("{n:?}");
        };
        assert_eq!((from.hash, pin.gen), (to, ChainGen(2)));
        to = t.hash;
    }
}

/// P5, and its mutant (`Replacing` treated like `Live`): a sub parked in `next()` wakes on
/// `Replacing`, delivers nothing and registers nothing, even as the old chain moves on; on
/// `Live` it returns one `Resync { Generation }`.
#[tokio::test]
async fn subscriber_woken_between_replacing_and_live() {
    let r = rig(1024, HEADER_WINDOW).await;
    let (tx, mut sub) = hand(&r);
    next(&mut sub).await;
    let hooks = Arc::clone(&sub.hooks);
    let (parked, waiting) = (hooks.pass(At::Parked), hooks.pass(At::Replacing));
    let mut task = spawn_next(sub);
    reach(&parked, &mut task).await;
    replacing(&tx);
    r.send(&mine(anchor().hash, 1_000_000, 5, 1)).await;
    reach(&waiting, &mut task).await;
    detached(&r, &hooks);
    relive(&r, &tx, 2);
    let (_sub, out) = returned(task).await;
    assert_eq!(resync(out), (ChainGen(2), ResyncWhy::Generation));
    hooks.fenced_out();
}

/// P5: a sub whose slot was closed for lag just before `Replacing` does not register again
/// until `Live`, and resyncs for the generation.
#[tokio::test]
async fn lagged_sub_waits_out_replacing() {
    let r = rig(1024, HEADER_WINDOW).await;
    let (tx, mut sub) = hand(&r);
    next(&mut sub).await;
    let hooks = Arc::clone(&sub.hooks);
    let (parked, waiting) = (hooks.hold(At::Parked), hooks.pass(At::Replacing));
    let mut task = spawn_next(sub);
    reach(&parked.reached, &mut task).await;
    close_for_lag(&r);
    replacing(&tx);
    parked.release.notify_one();
    reach(&waiting, &mut task).await;
    detached(&r, &hooks);
    relive(&r, &tx, 2);
    let (_sub, out) = returned(task).await;
    assert_eq!(resync(out), (ChainGen(2), ResyncWhy::Generation));
    hooks.fenced_out();
}

/// P5: `Stopped` published while a sub waits out `Replacing` returns `Stop`.
#[tokio::test]
async fn stop_during_replacing() {
    let r = rig(1024, HEADER_WINDOW).await;
    let (tx, mut sub) = hand(&r);
    next(&mut sub).await;
    let hooks = Arc::clone(&sub.hooks);
    let waiting = hooks.pass(At::Replacing);
    replacing(&tx);
    let mut task = spawn_next(sub);
    reach(&waiting, &mut task).await;
    detached(&r, &hooks);
    tx.send_modify(|s| *s = s.with(SourceState::Stopped));
    let (_sub, out) = returned(task).await;
    assert!(matches!(out, Ok(SubItem::Stop)), "{out:?}");
    assert_eq!(hooks.log().len(), 2);
}

/// Decision 3: a generation retired between `notice`'s two reads, the `lca` and then the
/// fork's `entry`, as a replacement does it (`Replacing` published, then the guard retired),
/// is no error: `next()` returns nothing until the next generation is live, then one
/// `Resync { Generation }`, with nothing tripped.
#[tokio::test]
async fn a_generation_retired_inside_a_notice_resyncs() {
    let r = rig(1024, HEADER_WINDOW).await;
    let (tx, mut sub) = hand(&r);
    let tx = Arc::new(tx);
    next(&mut sub).await;
    r.send(&mine(anchor().hash, 1_000_000, 3, 1)).await;
    let n = next(&mut sub).await;
    assert!(matches!(n, ChainNotice::Advanced { .. }), "{n:?}");
    r.send(&mine(anchor().hash, 1_000_000, 5, 2)).await;
    let guard = Arc::clone(&r.host.source().borrow().guard);
    let (publish, retiring) = (Arc::clone(&tx), Arc::clone(&guard));
    r.host.pinned().unwrap().on_read(move || {
        replacing(&publish);
        let retire = Arc::clone(&retiring);
        std::thread::spawn(move || retire.retire());
        while !retiring.retiring() {
            std::thread::yield_now();
        }
    });
    let hooks = Arc::clone(&sub.hooks);
    let waiting = hooks.pass(At::Replacing);
    let mut task = spawn_next(sub);
    reach(&waiting, &mut task).await;
    detached(&r, &hooks);
    let mut next_gen = r.host.source().borrow().clone();
    next_gen.gen = ChainGen(2);
    next_gen.guard = Arc::default();
    tx.send_replace(next_gen);
    let (_sub, out) = returned(task).await;
    assert_eq!(resync(out), (ChainGen(2), ResyncWhy::Generation));
    assert!(!r.setup.fail.is_tripped());
}

/// P7 with the source turned `Replacing` at `at` of the first registration: the slot is
/// registered and deregistered once, nothing is returned until the next generation is live,
/// and then one `Resync` of it.
async fn replaced_inside_the_fence(at: At) {
    let r = rig(1024, HEADER_WINDOW).await;
    let (tx, sub) = hand(&r);
    let hooks = Arc::clone(&sub.hooks);
    let (held, waiting) = (hooks.hold(at), hooks.pass(At::Replacing));
    let mut task = spawn_next(sub);
    reach(&held.reached, &mut task).await;
    replacing(&tx);
    held.release.notify_one();
    reach(&waiting, &mut task).await;
    assert_eq!(hooks.regs().len(), 1);
    relive(&r, &tx, 2);
    let (_sub, out) = returned(task).await;
    assert_eq!(resync(out), (ChainGen(2), ResyncWhy::Start));
    hooks.fenced_out();
}

#[tokio::test]
async fn replacing_between_source_read_and_register() {
    replaced_inside_the_fence(At::BeforeRegister).await;
}

#[tokio::test]
async fn replacing_between_handoff_and_recheck() {
    replaced_inside_the_fence(At::BeforeRecheck).await;
}

/// P7: a sub registering for generation 2 when 2 turns `Replacing` returns no `Resync` of it,
/// and one of generation 3 once that is live.
#[tokio::test]
async fn generation_change_registers_into_a_new_replacement() {
    let r = rig(1024, HEADER_WINDOW).await;
    let (tx, mut sub) = hand(&r);
    next(&mut sub).await;
    let hooks = Arc::clone(&sub.hooks);
    let (held, waiting) = (hooks.hold(At::BeforeRegister), hooks.pass(At::Replacing));
    relive(&r, &tx, 2);
    let mut task = spawn_next(sub);
    reach(&held.reached, &mut task).await;
    replacing(&tx);
    held.release.notify_one();
    reach(&waiting, &mut task).await;
    assert_eq!(hooks.regs().len(), 2);
    relive(&r, &tx, 3);
    let (_sub, out) = returned(task).await;
    assert_eq!(resync(out), (ChainGen(3), ResyncWhy::Generation));
    hooks.fenced_out();
}

/// P7 on the lag path: `Replacing` published while a lagged sub registers again; it returns
/// no `Resync { Lag }`, and one `Resync { Generation }` once the next generation is live.
#[tokio::test]
async fn lag_reregister_into_replacing() {
    let r = rig(1024, HEADER_WINDOW).await;
    let (tx, mut sub) = hand(&r);
    next(&mut sub).await;
    let hooks = Arc::clone(&sub.hooks);
    let (held, waiting) = (hooks.hold(At::BeforeRegister), hooks.pass(At::Replacing));
    close_for_lag(&r);
    let mut task = spawn_next(sub);
    reach(&held.reached, &mut task).await;
    replacing(&tx);
    held.release.notify_one();
    reach(&waiting, &mut task).await;
    assert_eq!(hooks.regs().len(), 2);
    relive(&r, &tx, 2);
    let (_sub, out) = returned(task).await;
    assert_eq!(resync(out), (ChainGen(2), ResyncWhy::Generation));
    hooks.fenced_out();
}

/// P7.1: a `next()` dropped between its registration and the re-check leaves its slot to the
/// next call, which deregisters it before anything else and returns one `Resync`.
#[tokio::test]
async fn next_dropped_inside_the_fence() {
    let r = rig(1024, HEADER_WINDOW).await;
    let (_tx, mut sub) = hand(&r);
    let hooks = Arc::clone(&sub.hooks);
    let held = hooks.hold(At::BeforeRecheck);
    {
        let call = sub.next();
        tokio::pin!(call);
        tokio::select! {
            _ = bound(held.reached.notified()) => {}
            out = &mut call => panic!("returned first: {out:?}"),
        }
    }
    let [Slot::Reg(stranded)] = hooks.log()[..] else {
        panic!("{:?}", hooks.log());
    };
    let n = next(&mut sub).await;
    assert!(
        matches!(
            n,
            ChainNotice::Resync {
                why: ResyncWhy::Start,
                ..
            }
        ),
        "{n:?}"
    );
    let [Slot::Reg(_), Slot::Dereg(first), Slot::Reg(_)] = hooks.log()[..] else {
        panic!("{:?}", hooks.log());
    };
    assert_eq!(first, stranded);
}

/// The budget's `time` in the lag tests.
const TIME: Duration = Duration::from_millis(200);

/// The reorg of `notices_pair_with_post_step_snapshot`: `a`, 3,000 blocks, sent, then `b`,
/// which disconnects 2,900 of them, to send.
pub(super) async fn reorg_after_a(r: &Rig) -> Vec<Header> {
    let a = mine(anchor().hash, 1_000_000, 3000, 1);
    let step = r.setup.feeds.step_headers as usize;
    for batch in a.chunks(step) {
        r.send(batch).await;
    }
    mine(a[99].hash(), 1_000_000 + 100 * step::SPACING, 2950, 2)
}

/// Sends `b` a step at a time; the last reply's `tip_seq`.
async fn send_steps(r: &Rig, b: &[Header]) -> u64 {
    let mut tip = 0;
    for batch in b.chunks(r.setup.feeds.step_headers as usize) {
        tip = r.send(batch).await.snap.tip_seq;
    }
    tip
}

/// The budget held: no step parked past `time` and one slice.
fn within_budget(r: &Rig) {
    let most = r.host.subs.lag.most();
    assert!(most <= r.setup.lag.time + super::super::step::PARK_SLICE, "parked {most:?}");
}

/// `sub`'s notices from here to `tip_seq` hold no `Resync`.
async fn no_resync_to(sub: &mut ChainSub, tip_seq: u64) {
    loop {
        match next(sub).await {
            ChainNotice::Resync {
                ..
            } => panic!("a second resync"),
            ChainNotice::Advanced {
                to_seq,
                ..
            }
            | ChainNotice::Reorg {
                to_seq,
                ..
            } if to_seq == tip_seq => return,
            _ => {}
        }
    }
}

/// PLAN-r1 §4.3 with one stuck pump and one normal one (Q = E = 4, so `events` = 8): the
/// normal pump drains on past the stuck one, and the event limit closes the stuck one alone,
/// before `time`. Its sub, parked in `next()`, is woken by the writer, since its pump never
/// sees the close, and returns one `Resync { Lag }`. `time` is SPV's 5 s: the normal pump's
/// waits over the reorg's ~1,460 sub-steps add up to the step's parked time, and on a loaded
/// box they can pass 200 ms, closing it for time too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lag_budget_is_per_step_events() {
    let r = rig_lag(false, 4, 2048, |_| {}).await;
    let b = reorg_after_a(&r).await;
    let mut normal = r.host.subscribe(SubKind::Filters);
    next(&mut normal).await;
    let mut stuck = r.host.subscribe(SubKind::Blocks);
    *stuck.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
    next(&mut stuck).await;
    let parked = stuck.hooks.pass(At::Parked);
    let mut task = spawn_next(stuck);
    reach(&parked, &mut task).await;
    let sending = async { send_steps(&r, &b).await };
    let (tip_b, (mut stuck, out)) = tokio::join!(sending, returned(task));
    let Ok(SubItem::Notice(ChainNotice::Resync {
        pin,
        why: ResyncWhy::Lag,
    })) = out
    else {
        panic!("{out:?}");
    };
    assert_eq!(r.host.subs.lag.closes(), [(SubKind::Blocks, "events")]);
    within_budget(&r);
    if pin.snap.tip_seq < tip_b {
        no_resync_to(&mut stuck, tip_b).await;
    }
    assert!(!r.setup.fail.is_tripped());
}

/// PLAN-r1 §4.3 with one slow pump alone, so no event limit: the time limit closes it once
/// the step has parked `time`, and not before; its sub, asked after the send, returns one
/// `Resync { Lag }` at the latest tip. Mutant #13 (the clock reset at each wake) never spends
/// the time, so a step parks on past the bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lag_budget_is_per_step_time() {
    let r = rig_lag(false, 4, 2048, |l| l.time = TIME).await;
    let b = reorg_after_a(&r).await;
    let mut slow = r.host.subscribe(SubKind::Masternodes);
    *slow.hooks.pump.lock() = Some(PumpHook::Throttle(Duration::from_millis(2)));
    next(&mut slow).await;
    let tip_b = send_steps(&r, &b).await;
    let ChainNotice::Resync {
        pin,
        why: ResyncWhy::Lag,
    } = next(&mut slow).await
    else {
        panic!("not a lag resync");
    };
    assert_eq!(pin.snap.tip_seq, tip_b);
    assert_eq!(r.host.subs.lag.closes(), [(SubKind::Masternodes, "time")]);
    within_budget(&r);
    assert!(r.host.subs.lag.most() >= r.setup.lag.time);
    let more = mine(b[2949].hash(), 1_000_000 + 3050 * step::SPACING, 1, 2);
    let tip = r.send(&more).await.snap.tip_seq;
    no_resync_to(&mut slow, tip).await;
}

/// A sub of `kind` whose pump is [`PumpHook::Unpolled`] until `until`, once it has parked.
async fn unpolled(
    r: &Rig,
    kind: SubKind,
    until: impl Future<Output = ()> + Send + 'static,
    stall: bool,
) -> ChainSub {
    let parked = Arc::new(Signal::new());
    let mut sub = r.host.subscribe(kind);
    *sub.hooks.pump.lock() = Some(PumpHook::Unpolled {
        until: Box::pin(until),
        parked: Arc::clone(&parked),
        stall,
    });
    next(&mut sub).await;
    bound(parked.notified()).await;
    sub
}

/// The open pump of `kind` in the host's generation 1.
fn entry(r: &Rig, kind: SubKind) -> Option<SubEntry> {
    r.host.subs.live(ChainGen(1)).into_iter().find(|e| e.kind == kind && !e.closed())
}

/// Blocks the writer until the open pump of `kind` runs.
fn until_running(subs: &SubTable, kind: SubKind) {
    let until = Instant::now() + Duration::from_secs(20);
    let pump = || subs.live(ChainGen(1)).into_iter().find(|e| e.kind == kind);
    while pump().expect("the pump is open").parked() {
        assert!(Instant::now() < until, "{kind:?} never ran");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Minor m1 of S5c's round 1: a pump woken on a refilled slot but never run still reads as
/// parked, so once the time is spent the time limit finds nothing to close. The next check
/// finds nothing again and closes every open pump before it parks, so the step parks `time`
/// and the oversleep of the slice clipped to it, within one slice, and the pump's sub resyncs.
/// Without that close the writer parks on in slices until the test's 10 s bound, past it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_parked_pump_is_closed_once_the_time_is_spent() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| l.time = TIME).await;
    let mut stale = unpolled(&r, SubKind::Filters, std::future::pending(), false).await;
    let headers = mine(anchor().hash, 1_000_000, 100, 1);
    let sent = tokio::time::timeout(Duration::from_secs(10), r.send(&headers)).await;
    if sent.is_err() {
        r.host.subs.live(ChainGen(1)).iter().for_each(SubEntry::close);
    }
    within_budget(&r);
    let tip = sent.expect("the step returned").snap.tip_seq;
    assert_eq!(r.host.subs.lag.closes(), [(SubKind::Filters, "time")]);
    let ChainNotice::Resync {
        pin,
        why: ResyncWhy::Lag,
    } = next(&mut stale).await
    else {
        panic!("not a lag resync");
    };
    assert_eq!(pin.snap.tip_seq, tip);
}

/// PLAN-r1 §2.4.3's two time-path rules (S5c round 1, m2), with `events` out of reach. Three
/// pumps; the first step fills two slots, the second's release blocks on both:
/// - `stuck`, stalled before its first pop, is closed once the time is spent;
/// - `healthy` drained both steps and is parked: it is spared;
/// - `late` was parked but never run, so it is spared too; the writer's hook after the first
///   close lets it run until it pops one event and stalls. It blocks the drain again, and is
///   closed at once, with no park between the two closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_spent_time_spares_parked_pumps_and_closes_later_blockers_at_once() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
        l.time = TIME;
        l.events = u64::MAX;
    })
    .await;
    let mut healthy = r.host.subscribe(SubKind::Filters);
    next(&mut healthy).await;
    let mut stuck = r.host.subscribe(SubKind::Blocks);
    *stuck.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
    next(&mut stuck).await;
    let gate = Arc::new(Signal::new());
    let _late = {
        let gate = Arc::clone(&gate);
        unpolled(&r, SubKind::Masternodes, async move { gate.notified().await }, true).await
    };
    let subs = Arc::clone(&r.host.subs);
    *r.host.subs.lag.on_lag.lock() = Some(Box::new(move |l| {
        if l != Lag::Closed(SubKind::Blocks, "time") {
            return;
        }
        gate.notify_one();
        until_running(&subs, SubKind::Masternodes);
    }));
    let a = mine(anchor().hash, 1_000_000, 8, 1);
    let first = r.send(&a[..4]).await.snap.tip_seq;
    bound(async {
        while !entry(&r, SubKind::Filters).is_some_and(|e| e.parked() && e.popped() == first) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    r.send(&a[4..]).await;
    let log = r.host.subs.lag.log();
    assert!(
        log.ends_with(&[
            Lag::Closed(SubKind::Blocks, "time"),
            Lag::Closed(SubKind::Masternodes, "time")
        ]),
        "{log:?}"
    );
    assert_eq!(r.host.subs.lag.closes().len(), 2, "{log:?}");
    assert!(entry(&r, SubKind::Filters).is_some(), "healthy is spared");
    within_budget(&r);
}

/// Nit 2 of S5c's round 2: the catch-up needs two checks in a row that close nothing. `stuck` is
/// closed once the time is spent; the next check finds `late` and `stale` parked, so the writer
/// parks, and its hook lets `late` run. `late` is closed at the wake; the check after that finds
/// only `stale`, parked, so the writer parks again before it closes `stale`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_catch_up_needs_two_fruitless_checks_in_a_row() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| l.time = TIME).await;
    let mut stuck = r.host.subscribe(SubKind::Blocks);
    *stuck.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
    next(&mut stuck).await;
    let gate = Arc::new(Signal::new());
    let _late = {
        let gate = Arc::clone(&gate);
        unpolled(&r, SubKind::Masternodes, async move { gate.notified().await }, true).await
    };
    let _stale = unpolled(&r, SubKind::Filters, std::future::pending(), false).await;
    let (subs, mut armed) = (Arc::clone(&r.host.subs), false);
    *r.host.subs.lag.on_lag.lock() = Some(Box::new(move |l| match l {
        Lag::Closed(SubKind::Blocks, _) => armed = true,
        Lag::Park if std::mem::take(&mut armed) => {
            gate.notify_one();
            until_running(&subs, SubKind::Masternodes);
        }
        _ => {}
    }));
    r.send(&mine(anchor().hash, 1_000_000, 100, 1)).await;
    let log = r.host.subs.lag.log();
    let closed = |kind| Lag::Closed(kind, "time");
    let (b, m, f) = (SubKind::Blocks, SubKind::Masternodes, SubKind::Filters);
    let tail = [closed(b), Lag::Park, closed(m), Lag::Park, closed(f)];
    assert!(log.ends_with(&tail), "{log:?}");
}
