//! `ValidationSignals`' queued half (N0s, §8.2), the growth test (§9.6, R2-M2), and DEC-192's
//! marked lane against V's in-hold drain and its queue-limit wait.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use crossbeam_channel::SendTimeoutError;
use dash_chain::{FeedSource, Handoff, Needs, Next, Notify, Parker};
use dash_consensus::BlockId;

use super::actor::{Log, entries, logged};
use super::support::{Rig, RigConfig, branch};
use crate::chain::reply::{
    ActorReply, NoWaitGuard, WaitError, no_wait_marked, reply, spawn_thread,
};
use crate::chain::{
    AbortSink, Blocking, ChainCommand, ChainHandle, Queued, RecordingAbort, SignalsThread, Staged,
    ValidationInterface, ValidationSignals,
};

/// Generous: a wait that should end at once, bounded so a regression fails instead of hanging.
const SOON: Duration = Duration::from_secs(120);

fn tip(id: u32) -> Queued {
    Queued::UpdatedBlockTip {
        new: BlockId(id),
        fork: None,
        ibd: false,
    }
}

fn start(signals: &ValidationSignals) -> (SignalsThread, Arc<RecordingAbort>) {
    let abort = Arc::new(RecordingAbort::default());
    (signals.start(abort.clone()).expect("start"), abort)
}

fn stop(thread: SignalsThread) {
    thread.stop().expect("an unmarked joiner").expect("no listener panicked");
}

/// One queued `UpdatedBlockTip`, as [`Tips`] saw it.
#[derive(Clone, Copy, Debug)]
struct Seen {
    id: u32,
    /// `callbacks_pending()` as the callback started (read by the growth test, which needs
    /// `alloc-tag`).
    #[cfg_attr(not(feature = "alloc-tag"), allow(dead_code))]
    pending: usize,
    marked: bool,
}

/// Records each queued `UpdatedBlockTip`; panics on `panic_on`.
struct Tips {
    seen: Mutex<Vec<Seen>>,
    signals: ValidationSignals,
    panic_on: Option<u32>,
}

impl Tips {
    fn watch(signals: &ValidationSignals) -> Arc<Tips> {
        Tips::with(signals, None)
    }

    fn with(signals: &ValidationSignals, panic_on: Option<u32>) -> Arc<Tips> {
        let t = Arc::new(Tips {
            seen: Mutex::default(),
            signals: signals.clone(),
            panic_on,
        });
        signals.register(t.clone());
        t
    }

    fn seen(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().expect("seen"))
    }

    fn ids(&self) -> Vec<u32> {
        self.seen().into_iter().map(|s| s.id).collect()
    }

    /// Unregisters, which also breaks the listener ↔ signals cycle.
    fn close(self: Arc<Self>) {
        self.signals.clone().unregister(&(self as Arc<dyn ValidationInterface>));
    }
}

impl ValidationInterface for Tips {
    fn updated_block_tip(&self, new: BlockId, _fork: Option<BlockId>, _ibd: bool) {
        if self.panic_on == Some(new.0) {
            panic!("listener failed on {}", new.0);
        }
        self.seen.lock().expect("seen").push(Seen {
            id: new.0,
            pending: self.signals.callbacks_pending(),
            marked: no_wait_marked(),
        });
    }
}

/// Queued and spliced signals run in FIFO order on the signals thread, which is marked; `sync`
/// returns once they all ran.
#[test]
fn queued_signals_run_in_order_on_a_marked_thread() {
    let signals = ValidationSignals::default();
    let tips = Tips::watch(&signals);
    let (thread, _) = start(&signals);
    for i in 1..=3 {
        signals.enqueue(tip(i));
    }
    let mut staged = Staged::default();
    staged.push(tip(4));
    staged.push(tip(5));
    signals.splice(&mut staged);
    assert!(staged.is_empty(), "the splice takes the whole list");
    signals.sync(&mut Blocking).expect("the barrier is reached");
    let seen = tips.seen();
    assert_eq!(seen.iter().map(|s| s.id).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
    assert!(seen.iter().all(|s| s.marked), "listeners run marked (LO-9)");
    assert_eq!(signals.callbacks_pending(), 0);
    tips.close();
    stop(thread);
}

/// Sol cp1 #2: registration is keyed by the listener (`validationinterface.cpp:50-55`): a
/// listener registered twice runs once per signal, in its first place.
#[test]
fn registering_a_listener_again_keeps_one_registration() {
    struct Named(&'static str, Arc<Mutex<Vec<&'static str>>>);
    impl ValidationInterface for Named {
        fn accepted_block_header(&self, _: BlockId) {
            self.1.lock().expect("log").push(self.0);
        }

        fn updated_block_tip(&self, _: BlockId, _: Option<BlockId>, _: bool) {
            self.1.lock().expect("log").push(self.0);
        }
    }
    let signals = ValidationSignals::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let a: Arc<dyn ValidationInterface> = Arc::new(Named("a", log.clone()));
    let b: Arc<dyn ValidationInterface> = Arc::new(Named("b", log.clone()));
    signals.register(a.clone());
    signals.register(b.clone());
    signals.register(a.clone());
    signals.accepted_block_header(BlockId(1));
    assert_eq!(*log.lock().expect("log"), ["a", "b"], "synchronous");
    log.lock().expect("log").clear();
    let (thread, _) = start(&signals);
    signals.enqueue(tip(1));
    signals.sync(&mut Blocking).expect("the barrier is reached");
    assert_eq!(*log.lock().expect("log"), ["a", "b"], "queued");
    signals.unregister(&a);
    signals.unregister(&b);
    stop(thread);
}

/// LO-9: a marked task's `sync` is refused at once and queues nothing.
#[test]
fn a_marked_sync_is_refused_and_queues_nothing() {
    let signals = ValidationSignals::default();
    let _marked = NoWaitGuard::set();
    assert_eq!(signals.sync(&mut Blocking), Err(WaitError::WaitOnVFromNoWaitTask));
    assert_eq!(signals.limit_queue(&mut Blocking), Ok(()), "nothing pending: no wait");
    assert_eq!(signals.callbacks_pending(), 0);
}

/// Stopping runs what is queued (Core's `FlushBackgroundCallbacks`); a later barrier is dropped,
/// so its `sync` returns instead of waiting forever. One consumer per instance.
#[test]
fn stop_runs_the_queue_then_later_syncs_disconnect() {
    let signals = ValidationSignals::default();
    let tips = Tips::watch(&signals);
    signals.enqueue(tip(1));
    signals.enqueue(tip(2));
    let (thread, _) = start(&signals);
    let abort: Arc<dyn AbortSink> = Arc::new(RecordingAbort::default());
    assert!(signals.start(abort).is_err(), "a second consumer would break the FIFO order");
    stop(thread);
    assert_eq!(tips.ids(), [1, 2]);
    assert_eq!(signals.sync(&mut Blocking), Err(WaitError::Disconnected));
    assert_eq!(signals.callbacks_pending(), 0, "dropped entries are not pending");
    tips.close();
}

/// A listener's panic is `AbortNode`, as an exception escaping Core's scheduler thread ends the
/// process: the queue closes, so a barrier behind it disconnects instead of hanging `sync` and
/// `limit_queue`, and nothing stays counted.
#[test]
fn a_listener_panic_aborts_and_closes_the_queue() {
    let signals = ValidationSignals::default();
    let tips = Tips::with(&signals, Some(2));
    for i in 1..=13 {
        signals.enqueue(tip(i));
    }
    let (thread, abort) = start(&signals);
    assert_eq!(signals.sync(&mut Blocking), Err(WaitError::Disconnected));
    assert_eq!(signals.callbacks_pending(), 0);
    assert_eq!(signals.limit_queue(&mut Blocking), Ok(()));
    signals.enqueue(tip(20));
    assert_eq!(signals.callbacks_pending(), 0, "a closed queue takes nothing");
    assert_eq!(
        abort.messages(),
        ["a validation interface listener panicked: listener failed on 2"]
    );
    assert!(thread.stop().expect("an unmarked joiner").is_err(), "the thread ended in the panic");
    assert_eq!(tips.ids(), [1]);
    tips.close();
}

/// §9.6's growth test (R2-M2): with the consumer not yet running, three producers enqueue a
/// backlog of 1, 128, 16,384 and 1,048,576, and a release splices a staged list as long. No
/// allocator call happens while L2s is held; `callbacks_pending()` equals the backlog, and falls
/// by one as each callback starts; each producer's order and the barrier hold. It counts with the
/// tagging allocator, so it runs with `alloc-tag` (the gate's feature run).
#[cfg(all(feature = "alloc-tag", not(miri)))]
#[test]
fn the_queue_grows_without_allocating_under_its_leaf() {
    use crate::alloc_tag::thread_alloc_events;
    use crate::chain::signals::LEAF_ALLOCS;

    // The counter counts (its positive control).
    let before = thread_alloc_events();
    drop(std::hint::black_box(Box::new(0u8)));
    assert_eq!(thread_alloc_events(), before + 2, "one allocation and one free");

    for n in [1u32, 128, 16_384, 1_048_576] {
        let signals = ValidationSignals::default();
        let tips = Tips::watch(&signals);
        let leaf_before = LEAF_ALLOCS.load(Ordering::SeqCst);
        // Producer k enqueues ids `k << 28 | i`, so its own order is visible.
        let share = [n / 3, n / 3, n - 2 * (n / 3)];
        std::thread::scope(|s| {
            for (k, c) in share.into_iter().enumerate() {
                let signals = &signals;
                s.spawn(move || {
                    for i in 0..c {
                        signals.enqueue(tip((k as u32) << 28 | i));
                    }
                });
            }
        });
        assert_eq!(signals.callbacks_pending(), n as usize, "backlog {n}");
        let mut staged = Staged::default();
        for i in 0..n {
            staged.push(tip(3 << 28 | i));
        }
        signals.splice(&mut staged);
        // The barrier goes in before the consumer starts, so every callback sees it pending.
        let (reached, barrier) = crossbeam_channel::bounded(1);
        signals.enqueue(Queued::Barrier(reached));
        let total = 2 * n as usize;
        assert_eq!(signals.callbacks_pending(), total + 1);
        assert_eq!(LEAF_ALLOCS.load(Ordering::SeqCst), leaf_before, "no allocation under L2s");

        let (thread, _) = start(&signals);
        barrier.recv_timeout(SOON).expect("the barrier is reached");
        let seen = tips.seen();
        assert_eq!(seen.len(), total);
        // At the k-th callback's start, k + 1 entries have started.
        for (k, s) in seen.iter().enumerate() {
            assert_eq!(s.pending, total - k, "callbacks_pending at callback {k} (backlog {n})");
        }
        for p in 0..4u32 {
            let mine: Vec<u32> =
                seen.iter().filter(|s| s.id >> 28 == p).map(|s| s.id & 0xfff_ffff).collect();
            assert!(mine.windows(2).all(|w| w[0] < w[1]), "producer {p}'s order (backlog {n})");
        }
        assert!(seen[n as usize..].iter().all(|s| s.id >> 28 == 3), "the splice is contiguous");
        assert_eq!(LEAF_ALLOCS.load(Ordering::SeqCst), leaf_before, "nor the consumer's sections");
        tips.close();
        stop(thread);
    }
}

/// How a marked task posts to V ([`post`]).
#[derive(Clone, Copy)]
enum Post {
    /// A blocking send into the bounded lane, bounded here by a timeout: DEC-192's negative
    /// control.
    Bounded,
    /// `ChainHandle::send`, which a marked task routes to the marked lane.
    Handle,
}

const POSTER_TIP: u32 = 7;

/// A listener that, on [`POSTER_TIP`], waits for the test, then posts a logged query to V
/// through `ChainHandle::send`.
struct Poster {
    go: Mutex<mpsc::Receiver<()>>,
    v: ChainHandle,
    log: Log,
    posted: Mutex<Option<Sent>>,
}

/// A post's outcome, and its reply, which comes once V has run it.
type Sent = (Result<(), String>, ActorReply<()>);

impl Poster {
    /// Registers a poster on the rig; the sender releases it.
    fn register(rig: &Rig, log: &Log) -> (Arc<Poster>, mpsc::Sender<()>) {
        let (go, gate) = mpsc::channel();
        let p = Arc::new(Poster {
            go: Mutex::new(gate),
            v: rig.v.clone(),
            log: log.clone(),
            posted: Mutex::new(None),
        });
        rig.signals.register(p.clone());
        (p, go)
    }

    fn unregister(self: Arc<Self>, rig: &Rig) {
        rig.signals.unregister(&(self as Arc<dyn ValidationInterface>));
    }
}

impl ValidationInterface for Poster {
    fn updated_block_tip(&self, new: BlockId, _fork: Option<BlockId>, _ibd: bool) {
        if new.0 != POSTER_TIP {
            return;
        }
        let _ = self.go.lock().expect("go").recv();
        let (cmd, ran) = logged_replying(&self.log, "posted");
        *self.posted.lock().expect("posted") = Some((post(&self.v, Post::Handle, cmd), ran));
    }
}

/// [`logged`], with a reply that comes once V has run it.
fn logged_replying(log: &Log, tag: &'static str) -> (ChainCommand, ActorReply<()>) {
    let (done, ran) = reply();
    let log = log.clone();
    let cmd = ChainCommand::Query(Box::new(move |_| {
        log.lock().expect("log").push(tag.into());
        done.send(());
    }));
    (cmd, ran)
}

/// Posts `cmd` to V as `how` says.
fn post(v: &ChainHandle, how: Post, cmd: ChainCommand) -> Result<(), String> {
    match how {
        Post::Bounded => match v.bounded_lane().send_timeout(cmd, Duration::from_millis(300)) {
            Ok(()) => Ok(()),
            Err(SendTimeoutError::Timeout(_)) => Err("timeout".into()),
            Err(SendTimeoutError::Disconnected(_)) => Err("disconnected".into()),
        },
        Post::Handle => v.send(cmd).map_err(|e| e.to_string()),
    }
}

/// What [`post_from_a_drained_subscriber`] saw.
struct Posted {
    outcome: Result<(), String>,
    /// The marked lane's depth just after the post.
    depth: usize,
    log: Vec<String>,
    /// How many commands filled the bounded lane.
    filled: usize,
    peak: usize,
}

/// DEC-192's scenario on N3's in-hold drain (§2.5): at Q = 1 a reorg's hold waits, after each
/// step, for a marked subscriber S to pop the step's event (`retry_pending`). Once the first
/// step's event reaches S, the test fills the bounded lane, which V does not serve inside the
/// hold, and S posts to V as `how` says before it pops anything.
fn post_from_a_drained_subscriber(how: Post) -> Posted {
    let rig = Rig::with(RigConfig {
        feed_capacity: Some(1),
        ..RigConfig::default()
    });
    let log = Log::default();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    for m in &main {
        rig.accept(m);
    }
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in &side[..2] {
        rig.accept(m);
    }
    let pre_hold = rig.id(&main[1]);
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
    let (stepped, at_step) = mpsc::channel();
    let (go, gate) = mpsc::channel::<()>();
    let (sent, posted) = mpsc::channel();
    let (v, s_log) = (rig.v.clone(), log.clone());
    let s = spawn_thread("s", move || {
        let _marked = NoWaitGuard::set();
        let p = Parker::new();
        feed.set_waker(Notify::Thread(p.clone()));
        let mut done = false;
        let mut popped = 0;
        // Two disconnects, three connects.
        while popped < 5 {
            match feed.next() {
                Next::Event(_) => {}
                Next::Empty => {
                    assert!(p.park_timeout(SOON), "V never stepped");
                    continue;
                }
                Next::Control(_) => continue,
            }
            if !done {
                stepped.send(()).expect("the test waits");
                gate.recv_timeout(SOON).expect("the test filled the lane");
                let r = post(&v, how, logged(&s_log, "posted"));
                let _ = sent.send((r, v.marked_lane_depth(), v.view().snap.tip.tip));
                done = true;
            }
            feed.pop().expect("selected");
            popped += 1;
        }
    })
    .expect("spawn S");

    let reorg = rig.v.process_block(side[2].block.clone(), true, None, true);
    at_step.recv_timeout(SOON).expect("V stepped and waits for S's pop");
    let lane = rig.v.bounded_lane().clone();
    let mut filled = 0;
    while lane.try_send(logged(&log, "queued")).is_ok() {
        filled += 1;
    }
    go.send(()).expect("S waits");
    let Ok((outcome, depth, tip_then)) = posted.recv_timeout(SOON) else {
        // S and V wait for each other: leak them, as dropping the rig would join V forever.
        std::mem::forget(rig);
        panic!("S's post never returned");
    };
    s.join().expect("an unmarked joiner").expect("S");
    let r = reorg.recv_timeout(SOON).expect("the reorg ends").expect("V replies");
    assert!(r.accepted && r.state.is_valid(), "{r:?}");
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(&side[2]));
    rig.v.query(|_| ()).recv_timeout(SOON).expect("V serves on").expect("served");
    assert_eq!(tip_then, pre_hold, "S posted before the hold's release");
    feeds.deregister(id);
    Posted {
        outcome,
        depth,
        log: entries(&log),
        filled,
        peak: rig.v.marked_lane_peak(),
    }
}

/// DEC-192's negative control (D24): from a marked subscriber that V's hold waits on, a
/// blocking send into V's full bounded lane cannot complete, as V serves no lane inside the
/// hold and the hold cannot end before the subscriber pops. The timeout ends it; without one,
/// S and V would wait for each other forever.
#[test]
fn a_bounded_send_from_a_drained_subscriber_deadlocks_against_the_hold() {
    let p = post_from_a_drained_subscriber(Post::Bounded);
    assert_eq!(p.filled, crate::chain::COMMAND_CAPACITY, "the bounded lane was full");
    assert_eq!(p.outcome, Err("timeout".to_string()), "the send could not complete");
    assert!(!p.log.contains(&"posted".to_string()));
    assert_eq!(p.log.len(), p.filled, "V served the queued commands after the hold");
    assert_eq!(p.peak, 0, "nothing used the marked lane");
}

/// The positive control: the same subscriber's `ChainHandle::send` goes to the unbounded marked
/// lane and returns at once inside the hold; S pops, the hold completes, and V runs the post.
#[test]
fn the_marked_lane_takes_a_drained_subscribers_post() {
    let p = post_from_a_drained_subscriber(Post::Handle);
    assert_eq!(p.filled, crate::chain::COMMAND_CAPACITY, "the bounded lane was full");
    assert_eq!(p.outcome, Ok(()));
    assert_eq!(p.depth, 1, "it waited in the marked lane while V held the tip");
    assert_eq!(p.peak, 1, "the metric saw it");
    assert_eq!(p.log.iter().filter(|e| *e == "posted").count(), 1, "V ran it");
    assert_eq!(p.log.len(), p.filled + 1);
}

/// §9.6 P-2: while V waits in `LimitValidationInterfaceQueue` (a release-point wait), an
/// unmarked query is served; the class C operation ends only after the queue drains below 11.
#[test]
fn limit_queue_services_queries_while_it_waits() {
    let rig = Rig::new();
    let log = Log::default();
    let (poster, go) = Poster::register(&rig, &log);
    // The poster holds the thread on its tip; eleven more stay pending (> 10).
    for i in 0..12 {
        rig.signals.enqueue(tip(POSTER_TIP + i));
    }
    let (done, waited) = reply();
    rig.v
        .send(ChainCommand::TestOnV(Box::new(move |v| {
            v.limit_queue()?;
            done.send(());
            Ok(())
        })))
        .expect("sent");
    assert_eq!(rig.v.query(|_| 42).recv_timeout(SOON), Some(Ok(42)), "served at the wait");
    assert!(waited.try_recv().is_none(), "the class C operation still waits");
    go.send(()).expect("the poster waits");
    waited.recv_timeout(SOON).expect("the wait ends").expect("ran");
    assert!(rig.signals.callbacks_pending() <= 10);
    let (outcome, ran) = poster.posted.lock().expect("posted").take().expect("posted");
    assert_eq!(outcome, Ok(()));
    // BL-471: V's select takes the post (marked lane) and a query of the test's (unmarked) in
    // either order, so the test waits for the post's own reply.
    ran.recv_timeout(SOON).expect("V runs the post").expect("ran");
    assert_eq!(entries(&log), ["posted"], "V ran the poster's post");
    rig.v.query(|_| ()).recv_timeout(SOON).expect("V serves on").expect("served");
    poster.unregister(&rig);
}

/// A listener that, on [`POSTER_TIP`], waits for the test, then panics.
struct Fails {
    go: Mutex<mpsc::Receiver<()>>,
    /// The signals thread is in the listener, so every earlier entry has run.
    holding: AtomicBool,
}

impl Fails {
    /// Registers one on the rig; the sender releases it.
    fn register(rig: &Rig) -> (Arc<Fails>, mpsc::Sender<()>) {
        let (go, gate) = mpsc::channel();
        let f = Arc::new(Fails {
            go: Mutex::new(gate),
            holding: AtomicBool::new(false),
        });
        rig.signals.register(f.clone());
        (f, go)
    }

    fn unregister(self: Arc<Self>, rig: &Rig) {
        rig.signals.unregister(&(self as Arc<dyn ValidationInterface>));
    }
}

/// Releases a gated listener when dropped, so a test that fails before releasing it still lets
/// the signals thread, and so the rig's teardown, finish.
struct Gate(Option<mpsc::Sender<()>>);

impl Gate {
    fn release(&mut self) {
        if let Some(go) = self.0.take() {
            let _ = go.send(());
        }
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.release();
    }
}

impl ValidationInterface for Fails {
    fn updated_block_tip(&self, new: BlockId, _fork: Option<BlockId>, _ibd: bool) {
        if new.0 == POSTER_TIP {
            self.holding.store(true, Ordering::SeqCst);
            let _ = self.go.lock().expect("go").recv();
            panic!("listener failed on {}", new.0);
        }
    }
}

/// An `AbortNode` sink that records each message with `callbacks_pending()` and V's
/// `ShutdownRequested()` as it was called, then panics if `panics`.
#[derive(Default)]
struct Probe {
    panics: bool,
    rig: OnceLock<(ValidationSignals, ChainHandle)>,
    seen: Mutex<Vec<(String, usize, bool)>>,
}

impl Probe {
    /// A rig whose V and signals thread report to a new probe.
    fn rig(panics: bool) -> (Rig, Arc<Probe>) {
        let probe = Arc::new(Probe {
            panics,
            ..Probe::default()
        });
        let rig = Rig::with(RigConfig {
            abort: Some(probe.clone()),
            ..RigConfig::default()
        });
        assert!(probe.rig.set((rig.signals.clone(), rig.v.clone())).is_ok());
        (rig, probe)
    }

    fn seen(&self) -> Vec<(String, usize, bool)> {
        self.seen.lock().expect("seen").clone()
    }
}

impl AbortSink for Probe {
    fn read_failed(&self, what: &str) {
        unreachable!("V reads no store here: {what}");
    }

    fn abort_node(&self, msg: &str, _: Option<&str>) {
        let (signals, v) = self.rig.get().expect("set before V runs");
        let at = (msg.to_owned(), signals.callbacks_pending(), v.shutdown_requested());
        self.seen.lock().expect("seen").push(at);
        if self.panics {
            panic!("the sink failed on {msg:?}");
        }
    }
}

const FAILED: &str = "a validation interface listener panicked: listener failed on 7";

/// A listener panics while a `sync` and V's `LimitValidationInterfaceQueue` wait are queued
/// behind it. The `AbortNode` sink runs first, with the queue still open and the node's flag
/// still clear (Core's `AbortNode` sets the exit status before `StartShutdown()`,
/// `shutdown.cpp:35-36`); then, even if the sink panics, the flag is set and the queue closes:
/// both waits end `Disconnected`, V's with shutdown requested, and nothing stays counted.
fn a_listener_panic_ends_the_waits(panics: bool) {
    let (rig, probe) = Probe::rig(panics);
    let (fails, go) = Fails::register(&rig);
    let mut gate = Gate(Some(go));
    // The listener holds the thread on its tip; eleven more stay pending (> 10), all in the
    // held tip's batch.
    let mut staged = Staged::default();
    for i in 0..12 {
        staged.push(tip(POSTER_TIP + i));
    }
    rig.signals.splice(&mut staged);
    let (done, waited) = reply();
    rig.v
        .send(ChainCommand::TestOnV(Box::new(move |v| {
            v.limit_queue()?;
            done.send(v.shutdown_requested());
            Ok(())
        })))
        .expect("sent");
    assert_eq!(rig.v.query(|_| ()).recv_timeout(SOON), Some(Ok(())), "V waits at the limit");
    let (synced, sync_wait) = reply();
    let signals = rig.signals.clone();
    std::thread::spawn(move || synced.send(signals.sync(&mut Blocking)));
    // Eleven tips and both barriers, behind the held one.
    let deadline = std::time::Instant::now() + SOON;
    while !fails.holding.load(Ordering::SeqCst) || rig.signals.callbacks_pending() < 13 {
        assert!(std::time::Instant::now() < deadline, "the listener never held 13 entries");
        std::thread::yield_now();
    }
    assert_eq!(rig.signals.callbacks_pending(), 13);

    gate.release();
    assert_eq!(sync_wait.recv_timeout(SOON), Some(Ok(Err(WaitError::Disconnected))));
    assert_eq!(waited.recv_timeout(SOON), Some(Ok(true)), "V's wait ended, shutdown requested");
    assert_eq!(probe.seen(), [(FAILED.to_owned(), 13, false)], "sink, then flag, then close");
    assert_eq!(rig.signals.callbacks_pending(), 0);
    rig.signals.enqueue(tip(99));
    assert_eq!(rig.signals.callbacks_pending(), 0, "a closed queue takes nothing");
    fails.unregister(&rig);
}

/// A listener's panic with an `AbortNode` sink that returns.
#[test]
fn a_listener_panic_disconnects_the_waits_behind_it() {
    a_listener_panic_ends_the_waits(false);
}

/// The same with an `AbortNode` sink that panics too: the flag is still set and the queue still
/// closes, so neither wait hangs.
#[test]
fn a_listener_panic_with_a_panicking_sink_still_disconnects_the_waits() {
    a_listener_panic_ends_the_waits(true);
}

/// V need not be waiting: a listener's panic stops an idle V, whose next `ShutdownRequested`
/// check sees the flag the signals thread sets after the sink, even if the sink panics.
#[test]
fn a_listener_panic_with_a_panicking_sink_stops_an_idle_v() {
    let (rig, probe) = Probe::rig(true);
    let (fails, go) = Fails::register(&rig);
    go.send(()).expect("the listener's gate");
    rig.signals.enqueue(tip(POSTER_TIP));
    let deadline = std::time::Instant::now() + SOON;
    while !rig.v.shutdown_requested() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(probe.seen(), [(FAILED.to_owned(), 0, false)]);
    assert!(rig.v.shutdown_requested());
    fails.unregister(&rig);
}
