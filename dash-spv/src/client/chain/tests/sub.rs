//! Chain subscribers (PLAN-r1 §2.4, §4.3): notices fenced on the published snapshot.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use dash_chain::{FeedConfig, FeedId, Parker};
use dash_clock::MockClock;
use tokio::sync::{watch, Notify as Signal};
use tokio::task::JoinHandle;

use super::step::{bridged, mine, PEER};
use super::*;
use crate::client::chain::step::{StepOut, PARK_SLICE};
use crate::client::chain::sub::SubTable;

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

/// `rig_lag` on the store `h`, in `dir`, its subscribers started whatever the store's state.
pub(super) async fn rig_in<H: StoreHandle>(
    h: H,
    dir: tempfile::TempDir,
    mn: bool,
    q: u32,
    w: u32,
    lag: impl FnOnce(&mut LagBudget),
) -> Rig {
    let r = open_rig(h, dir, mn, q, w, lag).await;
    r.host.subs.start();
    r
}

/// `rig_in`, its subscribers not started.
pub(super) async fn open_rig<H: StoreHandle>(
    h: H,
    dir: tempfile::TempDir,
    mn: bool,
    q: u32,
    w: u32,
    lag: impl FnOnce(&mut LagBudget),
) -> Rig {
    open_rig_with(h, dir, mn, q, w, |s| lag(&mut s.lag)).await
}

/// `open_rig`, its whole setup changed by `f`.
pub(super) async fn open_rig_with<H: StoreHandle>(
    h: H,
    dir: tempfile::TempDir,
    mn: bool,
    q: u32,
    w: u32,
    f: impl FnOnce(&mut ChainHostSetup),
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
    f(&mut setup);
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
        ..
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
        ..
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

/// A sub's test hooks: its `register` and `deregister` calls in order, its handoffs' reads of
/// the state, and holds at points of `next()`.
#[derive(Default)]
pub(crate) struct Hooks {
    pub(crate) log: Mutex<Vec<Slot>>,
    pub(crate) checks: AtomicUsize,
    holds: Mutex<Vec<(At, Hold)>>,
    /// The hook of the next pump `next()` starts.
    pub(crate) pump: Mutex<Option<PumpHook>>,
    /// The probe on the next pump's feed.
    pub(crate) probe: Mutex<Option<Arc<Probe>>>,
    /// Runs in the handoff's finish callback, under the tip lock, after the entry's insert.
    finish: Mutex<Option<Finish>>,
    /// Signalled each time a handoff's wait for a state past the one behind its pin polls
    /// pending.
    pub(crate) waits: Signal,
}

type Finish = Box<dyn FnMut(&SubTable) + Send>;

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
    /// Parks as usual; once armed, stalls for good at its next wake, before it pops.
    Armed(Arc<AtomicBool>),
}

/// A point of a pump's `FeedWait::poll`, on an empty feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PollAt {
    /// The feed read empty; `parked` not set yet.
    Empty,
    /// Inside the feed's `set_waker`, the waker installed, before it returns.
    WakerSet,
    /// The feed's next read, the second readiness check, before it reads.
    Recheck,
}

/// Gates in a pump's poll, each passed once: the pump reports its `parked` flag there and
/// blocks its thread until the gate is released or dropped. It also keeps the pump's last
/// completed poll: its `popped` and waker runs when its second check read the feed empty.
#[derive(Default)]
pub(crate) struct Probe {
    gates: Mutex<Vec<PollGate>>,
    polled: Mutex<Option<(u64, u64)>>,
}

/// A gate's point, where the pump reports its flag, and where it waits for its release.
type PollGate = (PollAt, std::sync::mpsc::Sender<bool>, std::sync::mpsc::Receiver<()>);

/// The test's side of a [`Probe`]'s gate.
pub(crate) struct Gate {
    reached: Option<std::sync::mpsc::Receiver<bool>>,
    release: std::sync::mpsc::Sender<()>,
}

impl Probe {
    pub(crate) fn gate(&self, at: PollAt) -> Gate {
        let (reached, seen) = std::sync::mpsc::channel();
        let (release, go) = std::sync::mpsc::channel();
        self.gates.lock().push((at, reached, go));
        Gate {
            reached: Some(seen),
            release,
        }
    }

    pub(crate) fn at(&self, at: PollAt, parked: &AtomicBool) {
        let gate = {
            let mut gates = self.gates.lock();
            gates.iter().position(|g| g.0 == at).map(|i| gates.remove(i))
        };
        if let Some((_, reached, go)) = gate {
            let _ = reached.send(parked.load(Ordering::SeqCst));
            let _ = go.recv();
        }
    }

    pub(crate) fn polled(&self, popped: u64, wakes: u64) {
        *self.polled.lock() = Some((popped, wakes));
    }

    fn last_poll(&self) -> Option<(u64, u64)> {
        *self.polled.lock()
    }
}

impl Gate {
    /// The pump's `parked` flag at the gate, once it is there.
    pub(crate) async fn reached(&mut self) -> bool {
        let seen = self.reached.take().expect("reached once");
        let at = tokio::task::spawn_blocking(move || seen.recv_timeout(Duration::from_secs(20)));
        at.await.unwrap().expect("the pump reached the gate")
    }

    /// [`reached`](Self::reached), blocking the caller's thread: the writer's.
    fn wait(&mut self) -> bool {
        let seen = self.reached.take().expect("reached once");
        seen.recv_timeout(Duration::from_secs(20)).expect("the pump reached the gate")
    }

    pub(crate) fn release(&self) {
        let _ = self.release.send(());
    }
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
        if let Some(PumpHook::Armed(armed)) = hook {
            let armed = Arc::clone(armed);
            wait.await;
            if armed.load(Ordering::SeqCst) {
                std::future::pending::<()>().await
            }
            return;
        }
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

/// The lag budget's record: each step's parks, its parks and closes in order with the limit
/// that closed each, a signal each time the writer parks, and the writer's hooks: one at each
/// wait's start, one inside each park's measured time, and one after each park and each close.
/// With `charged`, the test owns the clock: each park counts the time its permit names, or its
/// whole slice if none, whenever the real park ends.
#[derive(Default)]
pub(crate) struct LagLog {
    steps: Mutex<Vec<Vec<Parked>>>,
    log: Mutex<Vec<Lag>>,
    pub(crate) parks: Signal,
    pub(crate) on_lag: Mutex<Option<OnLag>>,
    pub(crate) on_wait: Mutex<Option<OnWait>>,
    pub(crate) permit: Mutex<Option<PermitAt>>,
    pub(crate) charged: AtomicBool,
}

/// What the writer runs after it parks or closes a pump.
pub(crate) type OnLag = Box<dyn FnMut(Lag) + Send>;

/// What the writer runs as a wait starts, before its first scan: in the drain after the release,
/// before each retry's wait.
pub(crate) type OnWait = Box<dyn FnMut() + Send>;

/// Given the step's parked time and a park's slice, when to set the writer's permit, timed from
/// the park's start and inside it: a permit that wakes the park then, as a pump's or a close's.
/// With [`LagLog::charged`] the permit is set before the park, which returns woken at once and
/// counts that time.
pub(crate) type PermitAt = Box<dyn FnMut(Duration, Duration) -> Option<Duration> + Send>;

/// One park: the slice asked, the time it took, and whether a permit woke it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Parked {
    pub(crate) asked: Duration,
    pub(crate) took: Duration,
    pub(crate) woken: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lag {
    Park,
    /// DEC-1947's backstop fired; its closes follow.
    Backstop,
    Closed(SubKind, &'static str),
}

impl LagLog {
    pub(crate) fn step(&self) {
        self.steps.lock().push(Vec::new());
    }

    pub(crate) fn waiting(&self) {
        if let Some(f) = self.on_wait.lock().as_mut() {
            f()
        }
    }

    /// The park's permit, as [`PermitAt`] says; with [`charged`](Self::charged), the time the
    /// park counts.
    pub(crate) fn parking(
        &self,
        parked: Duration,
        slice: Duration,
        parker: &Parker,
    ) -> Option<Duration> {
        let at = self.permit.lock().as_mut().and_then(|f| f(parked, slice));
        if self.charged.load(Ordering::SeqCst) {
            if at.is_some() {
                parker.notify();
            }
            return Some(at.unwrap_or(slice));
        }
        if let Some(at) = at {
            std::thread::sleep(at);
            parker.notify();
        }
        None
    }

    pub(crate) fn parked(&self, asked: Duration, took: Duration, woken: bool) {
        self.steps.lock().last_mut().expect("a step").push(Parked {
            asked,
            took,
            woken,
        });
        self.log.lock().push(Lag::Park);
        self.parks.notify_one();
        self.on(Lag::Park);
    }

    pub(crate) fn backstop(&self) {
        self.log.lock().push(Lag::Backstop);
        self.on(Lag::Backstop);
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
        self.steps.lock().iter().map(|p| total(p)).max().unwrap_or_default()
    }

    /// Each step's parks.
    fn steps(&self) -> Vec<Vec<Parked>> {
        self.steps.lock().clone()
    }

    pub(crate) fn closes(&self) -> Vec<(SubKind, &'static str)> {
        let log = self.log.lock();
        log.iter()
            .filter_map(|l| match *l {
                Lag::Closed(kind, limit) => Some((kind, limit)),
                Lag::Park | Lag::Backstop => None,
            })
            .collect()
    }

    pub(crate) fn log(&self) -> Vec<Lag> {
        self.log.lock().clone()
    }
}

/// The time `parks` took.
fn total(parks: &[Parked]) -> Duration {
    parks.iter().map(|p| p.took).sum()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum At {
    /// Before a live sub waits for its pump or the source.
    Parked,
    /// In the `Replacing` phase, before it waits for the source.
    Replacing,
    BeforeRegister,
    BeforeRecheck,
    /// After a state behind the pin released the slot, before the sub waits for the next.
    Trailing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Reg(FeedId),
    Dereg(FeedId),
}

/// The sub reached the hold's point; it stays there until `release`.
#[derive(Clone)]
pub(crate) struct Hold {
    pub(crate) reached: Arc<Signal>,
    pub(crate) release: Arc<Signal>,
}

impl Hold {
    fn new() -> Self {
        Self {
            reached: Arc::new(Signal::new()),
            release: Arc::new(Signal::new()),
        }
    }

    fn arm(slot: &Mutex<Option<Hold>>) -> Self {
        let h = Self::new();
        *slot.lock() = Some(h.clone());
        h
    }

    async fn pass(slot: &Mutex<Option<Hold>>) {
        let hold = slot.lock().take();
        if let Some(h) = hold {
            h.reached.notify_one();
            h.release.notified().await;
        }
    }
}

/// The header task's test hooks: a hold as it takes its next batch, before the batch's steps,
/// one after each of a batch's steps that does not stop it, and one as a batch settles, after
/// its steps, before its round's state.
#[derive(Default)]
pub(crate) struct TaskHooks {
    batch: Mutex<Option<Hold>>,
    step: Mutex<Option<Hold>>,
    settle: Mutex<Option<Hold>>,
}

impl TaskHooks {
    pub(crate) fn hold_step(&self) -> Hold {
        Hold::arm(&self.step)
    }

    pub(crate) async fn stepped(&self) {
        Hold::pass(&self.step).await
    }

    pub(crate) fn hold_batch(&self) -> Hold {
        Hold::arm(&self.batch)
    }

    pub(crate) fn hold_settle(&self) -> Hold {
        Hold::arm(&self.settle)
    }

    pub(crate) async fn batch(&self) {
        Hold::pass(&self.batch).await
    }

    pub(crate) async fn settle(&self) {
        Hold::pass(&self.settle).await
    }
}

impl Hooks {
    /// Holds the sub's next pass through `at`.
    pub(crate) fn hold(&self, at: At) -> Hold {
        let h = Hold::new();
        self.holds.lock().push((at, h.clone()));
        h
    }

    /// Signals the sub's next pass through `at`, and lets it go on.
    pub(crate) fn pass(&self, at: At) -> Arc<Signal> {
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

    /// `f`, signalling [`waits`](Self::waits) each time it polls pending.
    pub(crate) async fn waiting<F: Future>(&self, f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        std::future::poll_fn(|cx| {
            let polled = f.as_mut().poll(cx);
            if polled.is_pending() {
                self.waits.notify_one();
            }
            polled
        })
        .await
    }

    /// Runs `f` in each handoff's finish callback, after the entry's insert.
    fn on_finish(&self, f: impl FnMut(&SubTable) + Send + 'static) {
        *self.finish.lock() = Some(Box::new(f));
    }

    pub(crate) fn finished(&self, table: &SubTable) {
        if let Some(f) = self.finish.lock().as_mut() {
            f(table)
        }
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

pub(super) fn spawn_next(mut sub: ChainSub) -> Next {
    tokio::spawn(async move {
        let out = sub.next().await;
        (sub, out)
    })
}

pub(super) async fn bound<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), f).await.expect("within the test's bound")
}

/// Waits for `at`; the sub's `next()` must not return first.
pub(super) async fn reach(at: &Signal, task: &mut Next) {
    tokio::select! {
        _ = bound(at.notified()) => {}
        out = task => panic!("returned first: {:?}", out.map(|o| o.1)),
    }
}

pub(super) async fn returned(task: Next) -> (ChainSub, Result<SubItem, SubError>) {
    bound(task).await.unwrap()
}

/// A `Resync`'s generation, its pin's and its state's, and why.
fn resync(out: Result<SubItem, SubError>) -> (ChainGen, ResyncWhy) {
    match out {
        Ok(SubItem::Notice(ChainNotice::Resync {
            pin,
            why,
            sync,
        })) => {
            assert_eq!(sync.gen, pin.gen, "{sync:?}");
            (pin.gen, why)
        }
        other => panic!("{other:?}"),
    }
}

/// The source and state a [`hand`] sub reads, which the test publishes.
struct Hand {
    source: watch::Sender<ChainSource>,
    status: watch::Sender<HeaderSyncState>,
}

impl Hand {
    /// Publishes `source`, as a replacement does: the host's state as `source`'s generation's
    /// first, then `source`.
    fn live(&self, r: &Rig, source: ChainSource) {
        self.status.send_replace(HeaderSyncState {
            gen: source.gen,
            ..*r.host.status().borrow()
        });
        self.source.send_replace(source);
    }
}

/// A sub on a source and state the test publishes, starting as the host's.
fn hand(r: &Rig) -> (Hand, ChainSub) {
    let (source, rx) = watch::channel(r.host.source().borrow().clone());
    let (status, sync) = watch::channel(*r.host.status().borrow());
    let sub = ChainSub::new(SubKind::Filters, rx, sync, Arc::clone(&r.host.subs));
    (
        Hand {
            source,
            status,
        },
        sub,
    )
}

/// Publishes the host's chain as generation `gen`, in production's order.
fn relive(r: &Rig, tx: &Hand, gen: u64) {
    let mut s = r.host.source().borrow().clone();
    s.gen = ChainGen(gen);
    tx.live(r, s);
}

fn replacing(tx: &Hand) {
    tx.source.send_modify(|s| *s = s.with(SourceState::Replacing));
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
        ..
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
    tx.source.send_modify(|s| *s = s.with(SourceState::Stopped));
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
    tx.live(&r, next_gen);
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

/// The slack in [`within_budget`]'s check, past the crossing park's measured oversleep.
const MARGIN: Duration = Duration::from_millis(1);

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

/// `f`, within a bound for a whole reorg's steps on a loaded box.
async fn workload<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(300), f).await.expect("within the workload's bound")
}

/// Sends `b` a step at a time; the last reply's `tip_seq`.
async fn send_steps(r: &Rig, b: &[Header]) -> u64 {
    let mut tip = 0;
    for batch in b.chunks(r.setup.feeds.step_headers as usize) {
        tip = r.send(batch).await.snap.tip_seq;
    }
    tip
}

/// DEC-1954's bound held: no step parked past `time`, one slice and ε, the measured oversleep
/// of the park that crossed them, past the slice it asked, with [`MARGIN`].
fn within_budget(r: &Rig) {
    let bound = r.setup.lag.time + PARK_SLICE;
    for parks in r.host.subs.lag.steps() {
        let (mut sum, mut over) = (Duration::ZERO, Duration::ZERO);
        for p in &parks {
            let before = sum;
            sum += p.took;
            if before < bound && sum >= bound {
                over = p.took.saturating_sub(p.asked);
            }
        }
        assert!(sum <= bound + over + MARGIN, "parked {sum:?}, ε {over:?}: {parks:?}");
    }
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
/// box they can pass 200 ms, closing it for time too. The reorg's steps have their own
/// generous bound, and so has the woken sub's wait for the hold to release; the 20 s watchdog
/// times only its return after its handoff.
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
    let handed = stuck.hooks.pass(At::BeforeRecheck);
    let mut task = spawn_next(stuck);
    reach(&parked, &mut task).await;
    let closed = Arc::new(Signal::new());
    let on_close = Arc::clone(&closed);
    *r.host.subs.lag.on_lag.lock() = Some(Box::new(move |l| {
        if matches!(l, Lag::Closed(SubKind::Blocks, _)) {
            on_close.notify_one();
        }
    }));
    let sending = async { workload(send_steps(&r, &b)).await };
    let woken = async {
        workload(closed.notified()).await;
        workload(handed.notified()).await;
        returned(task).await
    };
    let (tip_b, (mut stuck, out)) = tokio::join!(sending, woken);
    let Ok(SubItem::Notice(ChainNotice::Resync {
        pin,
        why: ResyncWhy::Lag,
        ..
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
        ..
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

/// Blocks the writer until the open pump of `kind` runs.
fn until_running(subs: &SubTable, kind: SubKind) {
    let until = Instant::now() + Duration::from_secs(20);
    let pump = || subs.live(ChainGen(1)).into_iter().find(|e| e.kind == kind);
    while pump().expect("the pump is open").parked() {
        assert!(Instant::now() < until, "{kind:?} never ran");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Blocks the writer until the open pump of `kind` has completed a poll that read its feed empty
/// at the `seq` it has popped, with no waker run since: it holds nothing the writer pushed. The
/// writer also reads the pump parked, as its scan will (S5d scoped2 minor 4): a waker the writer
/// copied before this wait, run after that poll, clears the flag, and the pump's next poll sets
/// it again. The writer runs every waker it copies before it waits, and only its pushes run one,
/// so nothing clears the flag between this read and the scan.
fn until_drained(subs: &SubTable, kind: SubKind, probe: &Probe) {
    let until = Instant::now() + Duration::from_secs(20);
    let pump = || subs.live(ChainGen(1)).into_iter().find(|e| e.kind == kind);
    loop {
        let e = pump().expect("the pump is open");
        if e.parked() && e.popped().is_some_and(|p| probe.last_poll() == Some((p, e.wakes()))) {
            return;
        }
        assert!(Instant::now() < until, "{kind:?} never drained");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// `headers` sent within 10 s; past that every pump is closed so the step returns, and `None`.
async fn send_bounded(r: &Rig, headers: &[Header]) -> Option<HeadersReply> {
    let sent = tokio::time::timeout(Duration::from_secs(10), r.send(headers)).await;
    if sent.is_err() {
        for e in r.host.subs.live(ChainGen(1)) {
            e.close();
        }
    }
    sent.ok()
}

/// Minor m1 of S5c's round 1: a pump woken on a refilled slot but never run. The waker the
/// writer ran as it pushed cleared its `parked`, so the check that spends the time closes it:
/// the step parks `time` and that slice's oversleep, within one slice, and the pump's sub
/// resyncs. Had the flag stayed set, the writer would park on in slices until the test's 10 s
/// bound, past it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_parked_pump_is_closed_once_the_time_is_spent() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| l.time = TIME).await;
    let mut stale = unpolled(&r, SubKind::Filters, std::future::pending(), false).await;
    let sent = send_bounded(&r, &mine(anchor().hash, 1_000_000, 100, 1)).await;
    within_budget(&r);
    let tip = sent.expect("the step returned").snap.tip_seq;
    assert_eq!(r.host.subs.lag.closes(), [(SubKind::Filters, "time")]);
    let ChainNotice::Resync {
        pin,
        why: ResyncWhy::Lag,
        ..
    } = next(&mut stale).await
    else {
        panic!("not a lag resync");
    };
    assert_eq!(pin.snap.tip_seq, tip);
}

/// PLAN-r1 §2.4.3's two time-path rules (S5c round 1, m2), with `events` out of reach. Three
/// pumps on one step of 100 events:
/// - `stuck`, stalled before its first pop, is closed once the time is spent;
/// - `healthy` and `late` drained what the writer pushed them and are parked: both are spared;
/// - the writer's hook after that close closes `healthy`'s slot by hand and arms `late`, which
///   stalls at its next wake as its slot fills. It blocks the step again and is closed at once,
///   with no park between the two closes.
///
/// Until that close, the hook holds the writer at each park until `healthy` and `late` have
/// drained its pushes: each completed a poll on its empty feed at the `seq` it popped, with no
/// waker run since. So neither holds an event at the check that spends the time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_spent_time_spares_parked_pumps_and_closes_later_blockers_at_once() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
        l.time = TIME;
        l.events = u64::MAX;
    })
    .await;
    let (fast, slow) = (Arc::new(Probe::default()), Arc::new(Probe::default()));
    let mut healthy = r.host.subscribe(SubKind::Filters);
    *healthy.hooks.probe.lock() = Some(Arc::clone(&fast));
    next(&mut healthy).await;
    let mut stuck = r.host.subscribe(SubKind::Blocks);
    *stuck.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
    next(&mut stuck).await;
    let armed = Arc::new(AtomicBool::new(false));
    let mut late = r.host.subscribe(SubKind::Masternodes);
    *late.hooks.pump.lock() = Some(PumpHook::Armed(Arc::clone(&armed)));
    *late.hooks.probe.lock() = Some(Arc::clone(&slow));
    next(&mut late).await;
    let subs = Arc::clone(&r.host.subs);
    *r.host.subs.lag.on_lag.lock() = Some(Box::new(move |l| match l {
        Lag::Park if !armed.load(Ordering::SeqCst) => {
            until_drained(&subs, SubKind::Filters, &fast);
            until_drained(&subs, SubKind::Masternodes, &slow);
        }
        Lag::Closed(SubKind::Blocks, "time") => {
            let healthy = subs.live(ChainGen(1)).into_iter().find(|e| e.kind == SubKind::Filters);
            if let Some(e) = healthy {
                e.close();
            }
            armed.store(true, Ordering::SeqCst);
        }
        _ => {}
    }));
    r.send(&mine(anchor().hash, 1_000_000, 100, 1)).await;
    let log = r.host.subs.lag.log();
    let closes = [Lag::Closed(SubKind::Blocks, "time"), Lag::Closed(SubKind::Masternodes, "time")];
    assert!(log.ends_with(&closes), "{log:?}");
    assert_eq!(r.host.subs.lag.closes().len(), 2, "healthy is spared: {log:?}");
    within_budget(&r);
}

/// DEC-1944: the step parks at most `time` and one slice, with one pump per [`SubKind`].
/// `stuck` stalls before its first pop; three pumps are woken on a refilled slot but not run,
/// and the writer's hook lets one of them run at each park after the first close, so a time
/// limit that spared them would close them one park apart; `healthy` drains and is parked.
/// Every pump the writer waits on is closed at the check that spends the time, and `healthy`
/// is spared.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_time_limit_closes_every_pump_it_waits_on_at_once() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
        l.time = TIME;
        l.events = u64::MAX;
    })
    .await;
    let mut healthy = r.host.subscribe(SubKind::Progress);
    next(&mut healthy).await;
    let mut stuck = r.host.subscribe(SubKind::Blocks);
    *stuck.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
    next(&mut stuck).await;
    let (mut woken, mut gates) = (Vec::new(), Vec::new());
    for kind in [SubKind::FilterHeaders, SubKind::Filters, SubKind::Masternodes] {
        let gate = Arc::new(Signal::new());
        let until = Arc::clone(&gate);
        woken.push(unpolled(&r, kind, async move { until.notified().await }, true).await);
        gates.push((kind, gate));
    }
    gates.pop();
    let (subs, mut armed) = (Arc::clone(&r.host.subs), false);
    *r.host.subs.lag.on_lag.lock() = Some(Box::new(move |l| match l {
        Lag::Closed(SubKind::Blocks, _) => {
            let healthy = subs.live(ChainGen(1)).into_iter().find(|e| e.kind == SubKind::Progress);
            if let Some(e) = healthy {
                e.close();
            }
            armed = true;
        }
        Lag::Park if armed && !gates.is_empty() => {
            let (kind, gate) = gates.remove(0);
            gate.notify_one();
            until_running(&subs, kind);
        }
        _ => {}
    }));
    let sent = send_bounded(&r, &mine(anchor().hash, 1_000_000, 100, 1)).await;
    assert!(sent.is_some(), "the step returned");
    within_budget(&r);
    let (log, most) = (r.host.subs.lag.log(), r.host.subs.lag.most());
    let first = log.iter().position(|l| matches!(l, Lag::Closed(..))).expect("a close");
    assert!(!log[first..].contains(&Lag::Park), "parked {most:?}: {log:?}");
    let closes = r.host.subs.lag.closes();
    assert_eq!(closes.len(), 4, "{closes:?}");
    assert!(closes.iter().all(|&(k, l)| k != SubKind::Progress && l == "time"), "{closes:?}");
}

/// DEC-1947 (a), the handoff window (S5d r1 major 2, r2 minor 5): a slot is in the lag table
/// from the moment it goes live. Inside the handoff's finish callback the entry is already in the
/// table, at `UNSEEN`. The sub is held after its handoff, before it spawns its pump, while the
/// writer fills the slot: the entry reads busy, so the check that spends the time closes it.
/// The sub, let go, finds the state past its pin and hands off again at the step's tip.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slot_is_closeable_from_its_handoff() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
        l.time = TIME;
        l.events = u64::MAX;
    })
    .await;
    let sub = r.host.subscribe(SubKind::Filters);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let at = Arc::clone(&seen);
    sub.hooks.on_finish(move |t| {
        let entry = t.live(ChainGen(1)).into_iter().find(|e| e.kind == SubKind::Filters);
        at.lock().push(entry.map(|e| e.popped()));
    });
    let held = sub.hooks.hold(At::BeforeRecheck);
    let mut task = spawn_next(sub);
    reach(&held.reached, &mut task).await;
    assert_eq!(*seen.lock(), [Some(None)], "in the table at UNSEEN inside the finish");
    let sent = send_bounded(&r, &mine(anchor().hash, 1_000_000, 100, 1)).await;
    held.release.notify_one();
    assert!(sent.is_some(), "the step returned");
    within_budget(&r);
    assert_eq!(r.host.subs.lag.closes(), [(SubKind::Filters, "time")]);
    let (_, out) = returned(task).await;
    let tip = sent.expect("returned").snap.tip_seq;
    let start = matches!(
        &out,
        Ok(SubItem::Notice(ChainNotice::Resync {
            why: ResyncWhy::Start,
            pin,
            ..
        })) if pin.snap.tip_seq == tip
    );
    assert!(start, "{out:?}");
}

/// DEC-1947 (a), the backstop (S5d r1 major 1, r2 minor 4): a first registration's pump reads its
/// feed empty, the writer fills the slot before the pump marks itself parked, and the pump is
/// held before its second check, its slot full and its flag set with no waker run since. The
/// writer's first park waits for it there, so the scan that spends the time reads it parked. The
/// slice past the time runs out with nothing to close, so the backstop closes every open pump;
/// without it the writer would park on in slices.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_backstop_closes_a_full_slot_read_parked() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
        l.time = TIME;
        l.events = u64::MAX;
    })
    .await;
    let held = read_parked(&r, SubKind::Filters, |_| {}).await;
    let sent = send_bounded(&r, &mine(anchor().hash, 1_000_000, 100, 1)).await;
    held.recheck.lock().release();
    assert_eq!(*held.seen.lock(), Some(true), "the pump marked itself parked over a full slot");
    assert!(sent.is_some(), "the step returned");
    within_budget(&r);
    assert_eq!(r.host.subs.lag.closes(), [(SubKind::Filters, "time")]);
    assert!(r.host.subs.lag.log().contains(&Lag::Backstop));
}

/// A pump [`read_parked`] holds: its sub, its gates, and its flag at its second check. The
/// writer's hook holds none of them, nor the table, so dropping this, on a pass or an unwind,
/// drops the gates, and a pump held at either runs on.
struct ReadParked {
    _sub: ChainSub,
    empty: Arc<Mutex<Option<Gate>>>,
    recheck: Arc<Mutex<Gate>>,
    seen: Arc<Mutex<Option<bool>>>,
}

/// A first registration's pump of `kind`, held where it read its feed empty. The writer's first
/// park lets it go and waits until it is held at its second check, `seen` its flag there; the
/// test releases `recheck`. At every later park the writer runs `then` on the table.
async fn read_parked(
    r: &Rig,
    kind: SubKind,
    mut then: impl FnMut(&SubTable) + Send + 'static,
) -> ReadParked {
    let probe = Arc::new(Probe::default());
    let (mut empty, recheck) = (probe.gate(PollAt::Empty), probe.gate(PollAt::Recheck));
    let mut sub = r.host.subscribe(kind);
    *sub.hooks.probe.lock() = Some(probe);
    next(&mut sub).await;
    assert!(!empty.reached().await);
    let held = ReadParked {
        _sub: sub,
        empty: Arc::new(Mutex::new(Some(empty))),
        recheck: Arc::new(Mutex::new(recheck)),
        seen: Arc::default(),
    };
    let (empty, gate, at) =
        (Arc::downgrade(&held.empty), Arc::downgrade(&held.recheck), Arc::downgrade(&held.seen));
    let subs = Arc::downgrade(&r.host.subs);
    *r.host.subs.lag.on_lag.lock() = Some(Box::new(move |l| {
        let (Some(empty), Some(gate), Some(at), Some(subs)) =
            (empty.upgrade(), gate.upgrade(), at.upgrade(), subs.upgrade())
        else {
            return;
        };
        if l != Lag::Park {
            return;
        }
        let first = empty.lock().take();
        match first {
            Some(empty) => {
                empty.release();
                *at.lock() = Some(gate.lock().wait());
            }
            None => then(&subs),
        }
    }));
    held
}

/// `FeedWait::poll` sets `parked` before its waker (S5d r1, the surviving mutant; r2 minor 6): the
/// pump is held inside its feed's `set_waker`, the waker installed, while the writer pushes one
/// event and runs that waker, then at its second check, where the push has cleared its flag. A
/// store after `set_waker` returns, wherever it sits, comes after that push.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_push_after_the_waker_clears_parked() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |_| {}).await;
    let probe = Arc::new(Probe::default());
    let (mut set, mut recheck) = (probe.gate(PollAt::WakerSet), probe.gate(PollAt::Recheck));
    let mut sub = r.host.subscribe(SubKind::Filters);
    *sub.hooks.probe.lock() = Some(probe);
    next(&mut sub).await;
    set.reached().await;
    r.send(&mine(anchor().hash, 1_000_000, 1, 1)).await;
    set.release();
    let parked = recheck.reached().await;
    recheck.release();
    assert!(!parked, "the push after the waker left the pump marked parked");
}

/// DEC-1952 (S5d r2 major 1, MGR; scoped minor 4): the parks past the spent time share one
/// slice, woken or not. A first registration's pump marks itself parked over its full slot and
/// is held at its second check; `busy` stalls before its first pop. Both slots fill at the step's
/// release, so the writer waits in the drain after it. The check that spends the time closes
/// `busy`. Past `time`, a permit wakes each park 10 ms in, as a close or a pump making room
/// would, until 35 ms past `time`: each is a woken park with nothing to close, and the permit is
/// the one it consumed. The last park asks what is left of the slice and runs it out, and the
/// backstop closes the parked pump: the step parks at most `time`, one slice and ε. Asking a
/// fresh slice at each wake parks a further slice past the last wake: `time` and 90 ms. The test
/// owns the clock (scoped2 minor 5), so the order holds whenever a real park ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_parks_past_the_time_share_one_slice() {
    const EVERY: Duration = Duration::from_millis(10);
    const UNTIL: Duration = Duration::from_millis(35);
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
        l.time = TIME;
        l.events = u64::MAX;
    })
    .await;
    r.host.subs.lag.charged.store(true, Ordering::SeqCst);
    *r.host.subs.lag.permit.lock() =
        Some(Box::new(|parked, _| (parked >= TIME && parked - TIME < UNTIL).then_some(EVERY)));
    let held = read_parked(&r, SubKind::Filters, |_| {}).await;
    let mut busy = r.host.subscribe(SubKind::Blocks);
    *busy.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
    next(&mut busy).await;
    let sent = send_bounded(&r, &mine(anchor().hash, 1_000_000, 8, 1)).await;
    held.recheck.lock().release();
    assert_eq!(*held.seen.lock(), Some(true), "the pump marked itself parked over a full slot");
    assert!(sent.is_some(), "the step returned");
    within_budget(&r);
    let closes = [(SubKind::Blocks, "time"), (SubKind::Filters, "time")];
    assert_eq!(r.host.subs.lag.closes(), closes);
    let log = r.host.subs.lag.log();
    let first = log.iter().position(|l| matches!(l, Lag::Closed(..))).expect("busy's close");
    let between = &log[first + 1..log.len() - 2];
    assert!(between.iter().all(|l| *l == Lag::Park), "nothing closed between: {log:?}");
    assert_eq!(log[log.len() - 2..], [Lag::Backstop, Lag::Closed(SubKind::Filters, "time")]);
    let parks = r.host.subs.lag.steps().into_iter().max_by_key(|p| total(p)).expect("a step");
    let past = &parks[parks.len() - between.len()..];
    let (last, woken) = past.split_last().expect("parks past the time");
    assert!(!woken.is_empty() && woken.iter().all(|p| p.woken), "{past:?}");
    assert!(!last.woken, "the last park ran its slice out: {past:?}");
}

/// S5d scoped minor 1 (DEC-1938): a wait that parks nothing runs no event check. Every park is
/// woken at its slice's end, so no event check runs before the time is spent. `trailing` reads
/// parked over its full slot, eight events behind `ahead`, which drains each push and parks, with
/// `events` = 4. At the park that crosses `time` and one slice, `busy` goes live in the drain
/// and reads busy, so that scan closes it and the backstop waits. The next wait asks a zero
/// slice and parks nothing: the backstop closes `trailing` and `ahead` for time, where an event
/// check there would close `trailing` alone, for events. The test owns the clock (scoped2 minor
/// 5): each park counts its whole slice, so the crossing park is the one that attaches `busy`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wait_that_parks_nothing_runs_no_event_check() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
        l.time = TIME;
        l.events = 4;
    })
    .await;
    r.host.subs.lag.charged.store(true, Ordering::SeqCst);
    let probe = Arc::new(Probe::default());
    let mut ahead = r.host.subscribe(SubKind::Masternodes);
    *ahead.hooks.probe.lock() = Some(Arc::clone(&probe));
    next(&mut ahead).await;
    let drained = Arc::clone(&probe);
    let held = read_parked(&r, SubKind::Filters, move |subs| {
        until_drained(subs, SubKind::Masternodes, &drained);
    })
    .await;
    let subs = Arc::downgrade(&r.host.subs);
    *r.host.subs.lag.on_wait.lock() = Some(Box::new(move || {
        if let Some(subs) = subs.upgrade() {
            until_drained(&subs, SubKind::Masternodes, &probe);
        }
    }));
    let bystanders = Arc::new(Bystanders::default());
    let (attach, rt) = (Arc::downgrade(&bystanders), tokio::runtime::Handle::current());
    let (source, status, subs) = (r.host.source(), r.host.status(), Arc::downgrade(&r.host.subs));
    let mut busy = Some(());
    *r.host.subs.lag.permit.lock() = Some(Box::new(move |parked, slice| {
        if parked >= TIME && busy.take().is_some() {
            if let (Some(attach), Some(subs)) = (attach.upgrade(), subs.upgrade()) {
                attach.attach(&rt, &source, &status, &subs, SubKind::Blocks);
            }
        }
        Some(slice)
    }));
    let sent = send_bounded(&r, &mine(anchor().hash, 1_000_000, 8, 1)).await;
    held.recheck.lock().release();
    assert_eq!(*held.seen.lock(), Some(true), "the pump marked itself parked over a full slot");
    assert!(sent.is_some(), "the step returned");
    within_budget(&r);
    let closes =
        [(SubKind::Blocks, "time"), (SubKind::Masternodes, "time"), (SubKind::Filters, "time")];
    assert_eq!(r.host.subs.lag.closes(), closes);
    let parks = r.host.subs.lag.steps().into_iter().max_by_key(|p| total(p)).expect("a step");
    assert!(parks.iter().all(|p| p.woken), "{parks:?}");
}

/// S5d scoped, the surviving mutant: the park that crosses `time` and one slice is woken, by a
/// permit at its slice's end, with only the full slot read parked open. The backstop runs at that
/// crossing and closes it. A wait that left the backstop to a later call would first meet a busy
/// sub that goes live in the drain before each retry, and close it. The test owns the clock
/// (scoped2 minor 5): the parks before `time` count their slices, so none crosses it early.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_woken_park_past_the_slice_runs_the_backstop() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
        l.time = TIME;
        l.events = u64::MAX;
    })
    .await;
    r.host.subs.lag.charged.store(true, Ordering::SeqCst);
    let crossed = Arc::new(AtomicBool::new(false));
    let at = Arc::clone(&crossed);
    *r.host.subs.lag.permit.lock() = Some(Box::new(move |parked, slice| {
        (parked >= TIME && !at.swap(true, Ordering::SeqCst)).then_some(slice)
    }));
    let held = read_parked(&r, SubKind::Filters, |_| {}).await;
    let bystanders = Arc::new(Bystanders::default());
    let (attach, rt) = (Arc::downgrade(&bystanders), tokio::runtime::Handle::current());
    let (source, status, subs) = (r.host.source(), r.host.status(), Arc::downgrade(&r.host.subs));
    let mut left = 2;
    *r.host.subs.lag.on_wait.lock() = Some(Box::new(move || {
        if !crossed.load(Ordering::SeqCst) || left == 0 {
            return;
        }
        left -= 1;
        if let (Some(attach), Some(subs)) = (attach.upgrade(), subs.upgrade()) {
            attach.attach(&rt, &source, &status, &subs, SubKind::Masternodes);
        }
    }));
    let sent = send_bounded(&r, &mine(anchor().hash, 1_000_000, 8, 1)).await;
    held.recheck.lock().release();
    assert!(sent.is_some(), "the step returned");
    within_budget(&r);
    assert_eq!(r.host.subs.lag.closes(), [(SubKind::Filters, "time")]);
    let log = r.host.subs.lag.log();
    let end = [Lag::Park, Lag::Backstop, Lag::Closed(SubKind::Filters, "time")];
    assert!(log.ends_with(&end), "{log:?}");
    let parks = r.host.subs.lag.steps().into_iter().max_by_key(|p| total(p)).expect("a step");
    assert!(parks.last().is_some_and(|p| p.woken), "the crossing park was woken: {parks:?}");
}

/// Subs the writer's hooks attach in the drain, each stalled before its first pop. Dropping this
/// ends them.
#[derive(Default)]
struct Bystanders(Mutex<Vec<(JoinHandle<()>, watch::Sender<HeaderSyncState>)>>);

impl Bystanders {
    /// From the writer's thread: a sub of `kind` attaches at the published tip, on a state the
    /// test publishes at that tip, as the step's own update would be, so its `Resync` starts its
    /// pump; blocks until its slot is open in the table, where it reads busy.
    fn attach(
        &self,
        rt: &tokio::runtime::Handle,
        source: &watch::Receiver<ChainSource>,
        status: &watch::Receiver<HeaderSyncState>,
        subs: &Arc<SubTable>,
        kind: SubKind,
    ) {
        let SourceState::Live {
            reader,
            ..
        } = source.borrow().state.clone()
        else {
            panic!("not live");
        };
        let at = status.borrow().at(&reader.snapshot(), &StepOut::default());
        let (tx, rx) = watch::channel(at);
        let mut sub = ChainSub::new(kind, source.clone(), rx, Arc::clone(subs));
        *sub.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
        let task = rt.spawn(async move {
            let _ = sub.next().await;
            std::future::pending::<()>().await
        });
        self.0.lock().push((task, tx));
        let until = Instant::now() + Duration::from_secs(20);
        while !subs.live(ChainGen(1)).iter().any(|e| e.kind == kind && e.open()) {
            assert!(Instant::now() < until, "{kind:?} never went live");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for Bystanders {
    fn drop(&mut self) {
        self.0.lock().iter().for_each(|(task, _)| task.abort());
    }
}

/// S5d r2 minor 1: a retired slot is not counted. `ahead` popped one event and stalls for good;
/// its slot is deregistered under it, as a subscriber retiring it does before it drops the
/// entry, and its pump never sees the close, so its entry stays in the table, busy and ahead.
/// `stuck` blocks the next step, one event behind `ahead`, with `events` = 1: the event limit
/// never counts `ahead`, the time limit closes `stuck` alone, and `ahead` gets no close.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retired_slot_is_not_counted() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
        l.time = TIME;
        l.events = 1;
    })
    .await;
    let mut stuck = r.host.subscribe(SubKind::Blocks);
    *stuck.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
    next(&mut stuck).await;
    let mut ahead = r.host.subscribe(SubKind::Filters);
    *ahead.hooks.pump.lock() = Some(PumpHook::Stuck);
    next(&mut ahead).await;
    let a = mine(anchor().hash, 1_000_000, 104, 1);
    r.send(&a[..4]).await;
    let pumps = r.host.subs.live(ChainGen(1));
    let retired = pumps.iter().find(|e| e.kind == SubKind::Filters).expect("ahead's entry");
    let behind = pumps.iter().find(|e| e.kind == SubKind::Blocks).expect("stuck's entry");
    bound(async {
        while retired.popped() == behind.popped() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    retired.feeds.deregister(retired.id);
    let sent = send_bounded(&r, &a[4..]).await;
    assert!(sent.is_some(), "the step returned");
    within_budget(&r);
    assert_eq!(r.host.subs.lag.closes(), [(SubKind::Blocks, "time")]);
    assert!(!retired.closed());
}

/// S5d scoped minor 2: a slot retired, or closed by a stop, after the writer's scan sampled it
/// open is neither closed again nor logged by the writer. `first` and `second` stall before
/// their first pop, and the check that spends the time closes both; the writer's hook after
/// `first`'s close retires `second`'s slot, as its subscriber does, or runs
/// `stop_subscribers()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slot_retired_after_the_scan_is_not_closed_again() {
    let mut closes = Vec::new();
    for stop in [false, true] {
        let r = rig_lag(false, 4, HEADER_WINDOW, |l| {
            l.time = TIME;
            l.events = u64::MAX;
        })
        .await;
        let mut first = r.host.subscribe(SubKind::Blocks);
        *first.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
        next(&mut first).await;
        let mut second = r.host.subscribe(SubKind::Masternodes);
        *second.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(std::future::pending())));
        next(&mut second).await;
        let subs = Arc::downgrade(&r.host.subs);
        *r.host.subs.lag.on_lag.lock() = Some(Box::new(move |l| {
            let Some(subs) = subs.upgrade().filter(|_| l == Lag::Closed(SubKind::Blocks, "time"))
            else {
                return;
            };
            match stop {
                false => {
                    let pumps = subs.live(ChainGen(1));
                    let e = pumps.iter().find(|e| e.kind == SubKind::Masternodes).expect("second");
                    e.feeds.deregister(e.id);
                }
                true => subs.close_all("stop"),
            }
        }));
        let sent = send_bounded(&r, &mine(anchor().hash, 1_000_000, 100, 1)).await;
        assert!(sent.is_some(), "the step returned");
        within_budget(&r);
        closes.push(r.host.subs.lag.closes());
    }
    assert_eq!(closes, [[(SubKind::Blocks, "time")]; 2], "retired, then stopped");
}
