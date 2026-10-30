//! The threaded watchdog test (CH2 §4.4, §7.2 b3): the real W on its thread
//! ([`ThreadDriver`]), the hub, Synced and Syncing [`SubscriberDriver`]s on their own threads,
//! and the scripted producer with small queues, driven by a V that connects, reorgs and flushes
//! as a seeded schedule says. Every schedule must finish under a watchdog: V reaches the end of
//! its schedule and every subscriber is Synced with its locator at V's tip.
//!
//! - **Events threads.** Each subscriber catches up on its own thread. Once live it stays there
//!   (one events thread per subscriber), or moves to one events thread shared by all (§4.4.1:
//!   a subscriber never blocks its events thread, X17).
//! - **V's waits.** V's backpressure wait is [`FlushControl::backpressure_wait`] (rule 5): it
//!   answers `FlushDue` (X15 does not). V's waits for W run the pending fanout through
//!   [`ThreadDriver::set_pump`] (SB5's writer rule, D23). A pop and a `FlushDue` wake V. In the
//!   node, every `DriverEvent`, W's release too, rings V's `FlushBell` (the adapter, D13, CH1-node
//!   D60); this seam has no bell, so V's backpressure park polls W's events every millisecond,
//!   which stands in for it here and is not evidence for V's unbounded in-hold parks.
//! - **Independent fanout** (F3, n1) is the producer's: an event is emitted when every live
//!   queue has room, and V's step never waits for a slow subscriber except through it.

// Plain test threads stand in for W's, the events threads and V's watchdog (LO-9's markers
// are the node's, CH1-node §8.3).
#![expect(clippy::disallowed_methods, clippy::disallowed_types)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dash_chain::{BlockRef, IndexWrite, Notify, Parker};
use dash_consensus::primitives::TxOut;
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};

use super::{
    DriverConfig, NoBlocks, ScriptedFeeds, SubStep, SubscriberClass, SubscriberDriver,
    SyncProgress, TOY_DB, ToyIndex, TreeIndex,
};
use crate::coins::{CoinSrc, CoinsBudget, CoinsCache, OutPointKey, Ratio, Salt};
use crate::flush::{
    CrashRoll, FlushConfig, FlushControl, FlushCx, FlushPolicy, FlushStateMode, NoEvo, NoPrune,
    NullObserver, NullSignals, SubscriberHub, ThreadDriver, WThread,
};
use crate::store::{
    Failure, Fatal, GlobalFailStop, GuardedEngine, GuardedFiles, ProbeEngine, RecordingSink,
};
use crate::tuning::MemoryTuning;
use dashd_store::ErrorKind;

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

/// How long one schedule may take before the watchdog calls it a hang. Generous: the box runs
/// at load 50–190, and a schedule takes milliseconds.
const WATCHDOG: Duration = Duration::from_secs(60);

/// A schedule that must hang (X15) is called one sooner.
#[cfg(feature = "planted-bugs")]
const SHORT_WATCHDOG: Duration = Duration::from_secs(2);

/// The PR count of schedules per events-thread shape (§7.2: 10⁴).
const SCHEDULES: u64 = 10_000;

/// A planted bug the node's guard arms (CH2 §6.4); none without the feature.
#[cfg(feature = "planted-bugs")]
type Bug = crate::Planted;
#[cfg(not(feature = "planted-bugs"))]
type Bug = std::convert::Infallible;

/// Large enough that a small freeze leaves native Σ under B and F stays in flight: below about
/// 88 KiB every coins freeze is waited for (CH2c D32).
const SMALL_SHARE: u64 = 128 << 10;
const LARGE_SHARE: u64 = 64 << 20;

type Toy = SubscriberDriver<ToyIndex>;
type Ctl = FlushControl<ThreadDriver<()>>;
/// The shared events thread's inbox and wake: a live subscriber is handed to it.
type Handoff = (mpsc::Sender<(usize, Toy)>, Arc<Parker>);

/// splitmix64: a schedule from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Clone, Copy, Debug)]
enum Op {
    /// V connects `n` blocks back to back, with only its backpressure waits between them.
    Connect(u32),
    /// V disconnects `depth` blocks, then connects `len` on a new branch.
    Reorg {
        depth: u32,
        len: u32,
    },
    /// An ALWAYS flush, waited for.
    Always,
    /// V's idle service point (rule 5).
    Service,
    /// A subscriber opens now, catching up from nothing.
    Open(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// One events thread per subscriber.
    PerSubscriber,
    /// One events thread for every live subscriber.
    Shared,
}

/// A schedule: Q, V's coins share, each subscriber's `d_max` and cadence, and V's ops.
#[derive(Debug)]
struct Schedule {
    queue: usize,
    /// Small: IF_NEEDED sites freeze and CRITICAL ones wait for the flight. Large: only
    /// `FlushDue`, ALWAYS and the end flush.
    coins_share: u64,
    subs: Vec<DriverConfig>,
    ops: Vec<Op>,
}

impl Schedule {
    fn seeded(seed: u64) -> Self {
        let mut r = Rng(seed);
        let queue = 1 + r.below(3) as usize;
        let coins_share = if r.below(2) == 0 {
            SMALL_SHARE
        } else {
            LARGE_SHARE
        };
        let n = 1 + r.below(3) as usize;
        let subs = (0..n)
            .map(|_| DriverConfig {
                journal_budget: 1 << 20,
                unbounded_delete_span: 64,
                d_max: Some(1 + r.below(6) as u32),
                cadence: 1 + r.below(8) as u32,
            })
            .collect();
        let mut ops = Vec::new();
        // Most subscribers open before V starts, the rest at a random point.
        let mut late = Vec::new();
        for i in 0..n {
            if r.below(3) == 0 {
                late.push(i);
            } else {
                ops.push(Op::Open(i));
            }
        }
        let len = 4 + r.below(12);
        for _ in 0..len {
            ops.push(match r.below(10) {
                0..=4 => Op::Connect(1 + r.below(12) as u32),
                5 | 6 => Op::Reorg {
                    depth: 1 + r.below(4) as u32,
                    len: 1 + r.below(6) as u32,
                },
                7 => Op::Always,
                _ => Op::Service,
            });
            if !late.is_empty() && r.below(4) == 0 {
                ops.push(Op::Open(late.remove(0)));
            }
        }
        // V connects at least once: at genesis no flush commits, so no locator ever lands there
        // and `settle` could not end.
        if !ops.iter().any(|o| matches!(o, Op::Connect(_) | Op::Reorg { .. })) {
            ops.push(Op::Connect(1));
        }
        ops.extend(late.into_iter().map(Op::Open));
        Self {
            queue,
            coins_share,
            subs,
            ops,
        }
    }
}

/// What a subscriber thread last saw of its driver, for V's end condition.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    synced: bool,
    progress: Option<BlockRef>,
    durable: Option<BlockRef>,
}

/// The threads' common state.
struct Shared {
    stop: AtomicBool,
    seen: Mutex<Vec<Seen>>,
    /// Every thread's parker, so the stop reaches each.
    parkers: Mutex<Vec<Arc<Parker>>>,
    /// A subscriber's failure: the schedule fails with it.
    failed: Mutex<Option<String>>,
}

impl Shared {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn see(&self, i: usize, d: &Toy) {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)[i] = Seen {
            synced: d.class() == SubscriberClass::Synced,
            progress: d.progress(),
            durable: d.durable(),
        };
    }

    fn fail(&self, what: String) {
        self.failed.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert(what);
        self.stop_all();
    }

    fn stop_all(&self) {
        self.stop.store(true, Ordering::Release);
        for p in self.parkers.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            p.notify();
        }
    }

    fn parker(&self) -> Arc<Parker> {
        let p = Parker::new();
        self.parkers.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::clone(&p));
        p
    }
}

/// Steps a live subscriber until it is idle or parked; false once it failed.
fn serve(shared: &Shared, i: usize, d: &mut Toy) -> bool {
    loop {
        match d.step() {
            Ok(SubStep::Progress) => shared.see(i, d),
            Ok(SubStep::Idle | SubStep::Parked) => {
                shared.see(i, d);
                return true;
            }
            Err(e) => {
                shared.fail(format!("subscriber {i}: {e}"));
                return false;
            }
        }
    }
}

/// A subscriber's thread: catch-up until live, then its own events thread, or a hand-off to the
/// shared one.
fn subscriber_thread(shared: Arc<Shared>, i: usize, mut d: Toy, events: Option<Handoff>) {
    let parker = shared.parker();
    d.set_waker(Notify::from(Arc::clone(&parker)));
    while !shared.stopped() {
        match d.sync_step() {
            Ok(SyncProgress::Progress) => shared.see(i, &d),
            Ok(SyncProgress::Live) => break,
            Ok(SyncProgress::Blocked) => {
                shared.see(i, &d);
                parker.park();
            }
            Err(e) => return shared.fail(format!("subscriber {i} catching up: {e}")),
        }
    }
    shared.see(i, &d);
    if let Some((tx, wake)) = events {
        // The shared events thread re-registers its own wake.
        if tx.send((i, d)).is_ok() {
            wake.notify();
        }
        return;
    }
    while !shared.stopped() {
        if !serve(&shared, i, &mut d) {
            return;
        }
        parker.park();
    }
}

/// The shared events thread: every live subscriber it was handed, round robin.
fn events_thread(shared: Arc<Shared>, rx: mpsc::Receiver<(usize, Toy)>, parker: Arc<Parker>) {
    let mut subs: Vec<(usize, Toy)> = Vec::new();
    while !shared.stopped() {
        for (i, mut d) in rx.try_iter() {
            d.set_waker(Notify::from(Arc::clone(&parker)));
            subs.push((i, d));
        }
        for (i, d) in &mut subs {
            if !serve(&shared, *i, d) {
                return;
            }
        }
        parker.park();
    }
}

/// V: its coins, its tip and `seq`, and the block index's dirty records.
struct V {
    coins: CoinsCache<ProbeEngine>,
    tip: BlockRef,
    seq: u64,
    dirty: Vec<IndexWrite>,
    next_coin: u32,
}

impl V {
    fn with_cx<T>(&mut self, f: impl FnOnce(&mut FlushCx<'_, ProbeEngine, ()>) -> T) -> T {
        let mut cx = FlushCx {
            now: Instant::now(),
            coins: &mut self.coins,
            mempool_usage: 0,
            tip: self.tip,
            last_seq: self.seq,
            index: &mut self.dirty,
            evo: &mut NoEvo,
            prune: &mut NoPrune,
            manual_prune_height: None,
            reindex: false,
            is_ibd: false,
            left_ibd: false,
            scripted: false,
        };
        f(&mut cx)
    }

    /// A block's coins and its index record; the tip moves at the step.
    fn stage(&mut self, b: BlockRef) {
        for _ in 0..8 {
            let mut k = [0u8; 36];
            k[..4].copy_from_slice(&self.next_coin.to_le_bytes());
            let out = TxOut {
                value: 1_000,
                script_pubkey: vec![0x51; 25].into(),
            };
            let src = CoinSrc::Output {
                out: &out,
                height: b.height,
                coinbase: false,
            };
            self.coins.add(&OutPointKey(k), src, false).expect("adds");
            self.next_coin += 1;
        }
        self.coins.set_best_block(Some(b));
        self.dirty.push(IndexWrite {
            hash: b.hash,
            bytes: vec![0; 40],
        });
    }
}

/// The node: W, the hub, the producer and the tree, V.
struct Node {
    _dir: tempfile::TempDir,
    tree: TreeIndex,
    feeds: ScriptedFeeds,
    hub: Arc<SubscriberHub>,
    ctl: Ctl,
    v: V,
    v_park: Arc<Parker>,
    latch: GlobalFailStop,
}

#[derive(Debug)]
enum Outcome {
    Done,
    /// The watchdog fired: what V, W and the subscribers were left in.
    Hang(#[expect(dead_code, reason = "read through `Debug` in a failure's message")] String),
    #[expect(dead_code, reason = "read through `Debug` in a failure's message")]
    Failed(String),
}

impl Node {
    fn new(sc: &Schedule, plant: Option<Bug>) -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let blocks = dir.path().join("blocks");
        std::fs::create_dir_all(&blocks).expect("the blocks dir");
        let sink = Arc::new(RecordingSink::default());
        #[cfg_attr(not(feature = "planted-bugs"), allow(unused_mut))]
        let mut db = GuardedEngine::probe(sink);
        #[cfg(feature = "planted-bugs")]
        if let Some(bug) = plant {
            db.plant(bug);
        }
        #[cfg(not(feature = "planted-bugs"))]
        let _ = plant;
        let db = Arc::new(db);
        let fs = Arc::new(OsFs::new());
        let files = BlockFiles::with_fs(
            fs.clone(),
            BlockFilesConfig::new(&blocks, MAGIC),
            BlockFilesState::default(),
        );
        let files = Arc::new(GuardedFiles::new(files, db.latch().clone()));
        let tree = TreeIndex::new();
        let genesis = tree.genesis();
        let feeds = ScriptedFeeds::new(sc.queue, vec![genesis]);
        let hub =
            Arc::new(SubscriberHub::with_feeds(Arc::new(feeds.clone()), Arc::new(tree.clone())));
        let mut driver = ThreadDriver::spawn(WThread {
            engine: db.clone(),
            files,
            fs,
            hub: hub.clone(),
            obs: Arc::new(NullObserver),
            tuning: Arc::new(MemoryTuning),
            cfg: FlushConfig {
                sub_batch: 4 << 10,
                data_dir: dir.path().to_path_buf(),
                blocks_dir: blocks,
                live_copy: false,
            },
            covered_seq: 0,
            crash: CrashRoll::seeded(0, 0),
            on_crash: Box::new(|| {}),
        })
        .expect("W's thread");
        // V's producer wake is its wait for W's (D23), and its backpressure park.
        let v_park = Parker::new();
        let wakes = (driver.v_wake(), Notify::from(Arc::clone(&v_park)));
        feeds.set_producer_wake(Notify::Task(std::task::Waker::from(Arc::new(Both(wakes)))));
        let pump = feeds.clone();
        driver.set_pump(move || {
            pump.retry_pending();
        });
        let due = Arc::clone(&v_park);
        hub.set_due_wake(move || due.notify());
        let mut ctl = FlushControl::new(
            driver,
            FlushPolicy::new(false),
            Arc::new(NullSignals),
            Arc::default(),
            Arc::new(NullObserver),
        );
        ctl.set_hub(hub.clone());
        let budget = CoinsBudget::new(sc.coins_share, 0, Ratio::HALF, 0);
        let latch = db.latch().clone();
        let v = V {
            coins: CoinsCache::new(db, budget, Salt::fixed(5), None),
            tip: genesis,
            seq: 0,
            dirty: Vec::new(),
            next_coin: 0,
        };
        Self {
            _dir: dir,
            tree,
            feeds,
            hub,
            ctl,
            v,
            v_park,
            latch,
        }
    }

    fn open(&self, cfg: DriverConfig) -> Toy {
        SubscriberDriver::open(
            ToyIndex::mandatory(),
            Arc::new(GuardedEngine::new(
                ProbeEngine::new(&TOY_DB),
                GlobalFailStop::new(),
                Arc::new(RecordingSink::default()),
            )),
            Arc::clone(&self.hub),
            Arc::new(NoBlocks),
            cfg,
        )
        .expect("opens")
    }

    /// Core's IF_NEEDED site around one tip step, then the backpressure wait for its fanout.
    fn step(&mut self, connect: Option<BlockRef>) -> Result<(), Fatal> {
        let Self {
            ctl,
            v,
            feeds,
            tree,
            ..
        } = self;
        let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx))?;
        let tip = match connect {
            Some(b) => {
                v.stage(b);
                feeds.connect(b);
                b
            }
            None => {
                feeds.disconnect();
                let parent = tree.path(v.tip)[v.tip.height as usize - 1];
                v.coins.set_best_block(Some(parent));
                parent
            }
        };
        v.tip = tip;
        v.seq = feeds.tip_seq();
        let seq = v.seq;
        v.with_cx(|cx| pending.freeze_after_step(ctl, cx, tip, seq))?;
        self.backpressure()
    }

    /// The backpressure wait; the watchdog's trip ends it with the latch's `Fatal`.
    fn backpressure(&mut self) -> Result<(), Fatal> {
        let Self {
            ctl,
            v,
            feeds,
            v_park,
            latch,
            ..
        } = self;
        v.with_cx(|cx| {
            ctl.backpressure_wait(
                cx,
                || feeds.retry_pending() || latch.tripped().is_some(),
                || _ = v_park.park_timeout(Duration::from_millis(1)),
            )
        })?;
        latch.check()
    }

    fn run(&mut self, op: Op) -> Result<(), Fatal> {
        match op {
            Op::Connect(n) => {
                for _ in 0..n {
                    let b = self.tree.child(self.v.tip);
                    self.step(Some(b))?;
                }
            }
            Op::Reorg {
                depth,
                len,
            } => {
                let depth = depth.min(self.v.tip.height);
                for _ in 0..depth {
                    self.step(None)?;
                }
                for _ in 0..len {
                    let b = self.tree.child(self.v.tip);
                    self.step(Some(b))?;
                }
            }
            Op::Always => self.v.with_cx(|cx| self.ctl.flush(FlushStateMode::Always, cx))?,
            Op::Service => self.v.with_cx(|cx| self.ctl.answer_due(cx))?,
            Op::Open(_) => unreachable!("the schedule opens subscribers"),
        }
        // Rule 5 at V's service point.
        self.v.with_cx(|cx| self.ctl.answer_due(cx))
    }

    /// V's end: ALWAYS flushes, answering `FlushDue`, until every subscriber is Synced with
    /// its locator at V's tip.
    fn settle(&mut self, shared: &Shared) -> Outcome {
        loop {
            if let Some(f) = shared.failed.lock().unwrap_or_else(PoisonError::into_inner).clone() {
                return Outcome::Failed(f);
            }
            let tip = Some(self.v.tip);
            let done = shared
                .seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .all(|s| s.synced && s.progress == tip && s.durable == tip);
            if done {
                return Outcome::Done;
            }
            let flushed = self.v.with_cx(|cx| {
                self.ctl.answer_due(cx)?;
                self.ctl.flush(FlushStateMode::Always, cx)
            });
            if let Err(e) = flushed {
                return Outcome::Failed(format!("V's flush at the end: {e:?}"));
            }
            self.v_park.park_timeout(Duration::from_millis(1));
        }
    }
}

impl Node {
    /// The state a hang left, for its report.
    fn snapshot(&self, shared: &Shared) -> String {
        let seen = shared.seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let h = |b: Option<BlockRef>| b.map(|b| b.height);
        let subs: Vec<_> = seen.iter().map(|s| (s.synced, h(s.progress), h(s.durable))).collect();
        format!(
            "V at {} (seq {}), fanout pending {}, W in flight {}, FlushDue {}, T_ack {:?}, \
             subscribers (synced, p, L) {subs:?}",
            self.v.tip.height,
            self.v.seq,
            !self.feeds.pending_is_empty(),
            self.ctl.in_flight(),
            self.hub.flush_due(),
            self.hub.t_ack().map(|t| t.tip.height),
        )
    }
}

/// A `Notify::Task` that wakes two parkers: V's producer wake ends both of V's waits.
struct Both((Notify, Notify));

impl std::task::Wake for Both {
    fn wake(self: Arc<Self>) {
        self.0.0.notify();
        self.0.1.notify();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.0.notify();
        self.0.1.notify();
    }
}

/// The watchdog's thread: past `after`, it trips the chainstate latch, which ends every wait of
/// V's and W's, and stops the subscriber threads. True if it fired.
fn watchdog(
    after: Duration,
    latch: GlobalFailStop,
    shared: Arc<Shared>,
    done: Arc<Parker>,
) -> JoinHandle<bool> {
    std::thread::spawn(move || {
        if done.park_timeout(after) {
            return false;
        }
        latch.trip(Failure {
            kind: ErrorKind::Other,
            site: "watchdog",
            message: "the watchdog fired".into(),
        });
        shared.stop_all();
        true
    })
}

/// Runs a schedule under the watchdog.
fn run_schedule(sc: &Schedule, shape: Shape, plant: Option<Bug>, after: Duration) -> Outcome {
    let mut node = Node::new(sc, plant);
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        seen: Mutex::new(vec![Seen::default(); sc.subs.len()]),
        parkers: Mutex::default(),
        failed: Mutex::default(),
    });
    let mut threads: Vec<JoinHandle<()>> = Vec::new();
    let events = (shape == Shape::Shared).then(|| {
        let (tx, rx) = mpsc::channel();
        let parker = shared.parker();
        let s = Arc::clone(&shared);
        let p = Arc::clone(&parker);
        threads.push(std::thread::spawn(move || events_thread(s, rx, p)));
        (tx, parker)
    });
    let done = Parker::new();
    let dog = watchdog(after, node.latch.clone(), Arc::clone(&shared), Arc::clone(&done));
    let mut outcome = None;
    for &op in &sc.ops {
        let r = match op {
            Op::Open(i) => {
                let d = node.open(sc.subs[i]);
                shared.see(i, &d);
                let s = Arc::clone(&shared);
                let ev = events.as_ref().map(|(tx, p)| (tx.clone(), Arc::clone(p)));
                threads.push(std::thread::spawn(move || subscriber_thread(s, i, d, ev)));
                Ok(())
            }
            op => node.run(op),
        };
        if let Err(e) = r {
            outcome = Some(Outcome::Failed(format!("V at {op:?}: {e:?}")));
            break;
        }
    }
    let outcome = outcome.unwrap_or_else(|| node.settle(&shared));
    done.notify();
    let fired = dog.join().expect("the watchdog");
    shared.stop_all();
    drop(events);
    for t in threads {
        t.join().expect("a subscriber thread");
    }
    // W ends once V drops the control: the subscribers are gone, so it waits on no one.
    let outcome = match outcome {
        _ if fired => Outcome::Hang(node.snapshot(&shared)),
        o => o,
    };
    drop(node);
    outcome
}

fn sweep(shape: Shape, from: u64, n: u64) {
    for seed in from..from + n {
        match run_schedule(&Schedule::seeded(seed), shape, None, WATCHDOG) {
            Outcome::Done => {}
            o => panic!("{shape:?} seed {seed}: {o:?}\n{:?}", Schedule::seeded(seed)),
        }
    }
}

#[test]
fn ten_thousand_schedules_per_subscriber_events_threads() {
    sweep(Shape::PerSubscriber, 0, SCHEDULES);
}

#[test]
fn ten_thousand_schedules_shared_events_thread() {
    sweep(Shape::Shared, 1 << 32, SCHEDULES);
}

/// The directed X15 schedule: a Synced subscriber with `d_max` 2 and Q = 1, no flush but the ones
/// `FlushDue` asks for, and V connecting 60 blocks back to back. At each `D_max` cycle the
/// subscriber parks at its limit and posts `FlushDue`. A site V reaches after the post takes it;
/// when the post lands while V already waits on the full queue, only the flush the backpressure
/// wait starts frees it. Which happens is the threads' race, so the cycles repeat it.
fn x15_schedule() -> Schedule {
    Schedule {
        queue: 1,
        coins_share: LARGE_SHARE,
        subs: vec![DriverConfig {
            journal_budget: 1 << 20,
            unbounded_delete_span: 64,
            d_max: Some(2),
            cadence: 4,
        }],
        ops: vec![Op::Open(0), Op::Connect(1), Op::Always, Op::Connect(60)],
    }
}

#[test]
fn the_x15_schedule_finishes() {
    for shape in [Shape::PerSubscriber, Shape::Shared] {
        let o = run_schedule(&x15_schedule(), shape, None, WATCHDOG);
        assert!(matches!(o, Outcome::Done), "{shape:?}: {o:?}");
    }
}

/// X15: V's backpressure wait services W's events but not `FlushDue`. The watchdog fires once
/// a post lands while V waits (within a few runs; each run has 30 chances).
#[cfg(feature = "planted-bugs")]
#[test]
fn x15_backpressure_without_flush_due_hangs() {
    for shape in [Shape::PerSubscriber, Shape::Shared] {
        let hung = (0..20).any(|_| {
            let o = run_schedule(&x15_schedule(), shape, Some(crate::Planted::X15), SHORT_WATCHDOG);
            match o {
                Outcome::Hang(_) => true,
                Outcome::Done => false,
                o => panic!("{shape:?}: {o:?}"),
            }
        });
        assert!(hung, "{shape:?}: X15 never hung");
    }
}
