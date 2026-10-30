//! V's loop (N0): queries, loading flags, release points and class C deferral, shutdown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use dash_chain::Status;

use super::support::{Rig, RigConfig, child};
use crate::chain::actor::tests::FAIL_REAPER_SPAWN;
use crate::chain::reply::{ActorReply, NoWaitGuard, WaitError, reply};
use crate::chain::{AbortSink, BlockReply, ChainCommand, Fatal, LocalShutdown, ShutdownFlag};

pub(super) type Log = Arc<Mutex<Vec<String>>>;

/// A bound on a wait that should end at once: a regression fails instead of hanging.
const SOON: Duration = Duration::from_secs(120);

/// `n` gates: the senders that open them and the receivers [`composite`] waits on.
fn gates(n: usize) -> (Vec<mpsc::Sender<()>>, Vec<mpsc::Receiver<()>>) {
    (0..n).map(|_| mpsc::channel()).unzip()
}

/// A class C operation with `iterations` release points, logging `name:i` after each.
/// `gates[i]` is waited on before release point `i`, and `gates[iterations]` after the last one,
/// so a test can queue commands at chosen points.
fn composite(
    log: &Log,
    name: &'static str,
    iterations: u32,
    gates: Vec<mpsc::Receiver<()>>,
) -> (ChainCommand, ActorReply<()>) {
    let (done, wait) = reply();
    let log = log.clone();
    let gate = move |i: u32| {
        if let Some(g) = gates.get(i as usize) {
            let _ = g.recv();
        }
    };
    let c = ChainCommand::TestOnV(Box::new(move |v| {
        for i in 0..iterations {
            gate(i);
            v.release_point()?;
            log.lock().expect("log").push(format!("{name}:{i}"));
        }
        gate(iterations);
        done.send(());
        Ok(())
    }));
    (c, wait)
}

/// A query that logs `tag`.
pub(super) fn logged(log: &Log, tag: &'static str) -> ChainCommand {
    let log = log.clone();
    ChainCommand::Query(Box::new(move |_| log.lock().expect("log").push(tag.into())))
}

pub(super) fn entries(log: &Log) -> Vec<String> {
    log.lock().expect("log").clone()
}

#[test]
fn query_runs_on_v_and_shutdown_returns_the_state() {
    let mut rig = Rig::new();
    let g = rig.id(&rig.genesis);
    let (best, tip) = rig
        .v
        .query(|q| (q.chain.best_header(), q.chain.private_tip().map(|(t, _)| t.id)))
        .recv()
        .expect("served");
    assert_eq!((best, tip), (g, Some(g)));
    assert!(rig.v.view().snap.has_tip());
    let exit = rig.stop();
    assert!(exit.fatal.is_none());
    assert_eq!(exit.chain.best_header(), g);
    rig.assert_no_abort();
}

/// LO-9: a marked thread posts, but never parks on the reply.
#[test]
fn a_no_wait_thread_posts_but_never_parks_on_v() {
    let rig = Rig::new();
    let ran = Arc::new(AtomicBool::new(false));
    {
        let _g = NoWaitGuard::set();
        let ran = ran.clone();
        let r = rig.v.query(move |_| ran.store(true, Ordering::SeqCst));
        assert_eq!(r.recv(), Err(WaitError::WaitOnVFromNoWaitTask));
    }
    // DEC-192 orders nothing across the lanes: unmarked queries can overtake the marked one.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ran.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the marked thread's command ran");
        rig.v.query(|_| ()).recv().expect("V still serves");
    }
}

#[test]
fn replies_disconnect_after_shutdown() {
    let mut rig = Rig::new();
    rig.stop();
    assert_eq!(rig.v.query(|_| ()).recv(), Err(WaitError::Disconnected));
}

/// Commands still queued when V stops are dropped, so their callers do not wait forever.
#[test]
fn queued_commands_disconnect_when_v_stops() {
    let mut rig = Rig::new();
    let log = Log::default();
    let (open, gs) = gates(1);
    let (a, a_wait) = composite(&log, "A", 0, gs);
    rig.v.send(a).expect("sent");
    rig.v.send(ChainCommand::Shutdown).expect("sent");
    let queued = rig.v.query(|_| ());
    open[0].send(()).expect("A waits");
    a_wait.recv().expect("A ran");
    assert_eq!(queued.recv_timeout(SOON), Some(Err(WaitError::Disconnected)));
    assert!(rig.stop().fatal.is_none());
}

/// Sol cp1 #3: V needs its reaper to disconnect a stopped V's queued replies while a handle
/// lives (the bounded lane keeps its buffer), so a reaper that cannot start fails V's start.
#[test]
fn v_does_not_start_without_its_reaper() {
    FAIL_REAPER_SPAWN.with(|f| f.set(true));
    let r = Rig::try_with(RigConfig::default());
    FAIL_REAPER_SPAWN.with(|f| f.set(false));
    let e = r.err().expect("no V without its reaper");
    assert!(e.to_string().contains("reaper"), "{e}");
    assert!(Rig::try_with(RigConfig::default()).is_ok(), "the next start has its reaper");
}

/// After an abort the reaper disconnects the queued replies and later ones while a handle
/// lives; a reply sent before the abort stays readable (actor.rs's module docs).
#[test]
fn an_abort_disconnects_queued_replies_and_keeps_sent_ones() {
    let mut rig = Rig::new();
    let early = rig.v.query(|_| 7);
    assert_eq!(rig.v.query(|_| ()).recv(), Ok(()), "`early` was served");
    let log = Log::default();
    let (open, gs) = gates(1);
    let (a, _) = composite(&log, "A", 0, gs);
    rig.v.send(a).expect("sent");
    rig.v.send(ChainCommand::TestOnV(Box::new(|_| Err(Fatal::new("disk on fire"))))).expect("sent");
    let queued = rig.v.query(|_| ());
    open[0].send(()).expect("A waits");
    assert_eq!(queued.recv_timeout(SOON), Some(Err(WaitError::Disconnected)));
    assert_eq!(
        rig.v.query(|_| ()).recv_timeout(SOON),
        Some(Err(WaitError::Disconnected)),
        "sent after"
    );
    assert_eq!(early.recv(), Ok(7));
    assert!(rig.stop().fatal.is_some());
}

/// A V that panics stops the node like an `AbortNode`: the reply queued behind the panicking
/// command disconnects, and the panic reaches V's joiner.
#[test]
fn a_panicking_v_disconnects_queued_replies() {
    let mut rig = Rig::new();
    let log = Log::default();
    let (open, gs) = gates(1);
    let (a, _) = composite(&log, "A", 0, gs);
    rig.v.send(a).expect("sent");
    rig.v.send(ChainCommand::TestOnV(Box::new(|_| panic!("index corrupt")))).expect("sent");
    let queued = rig.v.query(|_| ());
    open[0].send(()).expect("A waits");
    assert_eq!(queued.recv_timeout(SOON), Some(Err(WaitError::Disconnected)));
    assert!(rig.v.shutdown_requested());
    let joined = rig.join.take().expect("running").join().expect("an unmarked joiner");
    assert!(joined.is_err(), "the panic reaches V's joiner");
    assert_eq!(rig.abort.messages(), ["V panicked: index corrupt"]);
}

/// An `AbortNode` sink that panics.
pub(super) struct PanickingAbort;

impl AbortSink for PanickingAbort {
    fn abort_node(&self, msg: &str, _: Option<&str>) {
        panic!("the sink failed on {msg:?}");
    }

    fn read_failed(&self, what: &str) {
        panic!("the sink failed on {what:?}");
    }
}

/// V's latch, the node's flag and the reaper's lanes do not depend on the `AbortNode` sink
/// returning: a panicking sink still disconnects the queued reply, and the node still hears the
/// stop (D52, validator L-1).
#[test]
fn a_panicking_abort_sink_still_disconnects_queued_replies() {
    let mut rig = Rig::with(RigConfig {
        abort: Some(Arc::new(PanickingAbort)),
        ..RigConfig::default()
    });
    let log = Log::default();
    let (open, gs) = gates(1);
    let (a, _) = composite(&log, "A", 0, gs);
    rig.v.send(a).expect("sent");
    rig.v.send(ChainCommand::TestOnV(Box::new(|_| Err(Fatal::new("disk on fire"))))).expect("sent");
    let queued = rig.v.query(|_| ());
    open[0].send(()).expect("A waits");
    assert_eq!(queued.recv_timeout(SOON), Some(Err(WaitError::Disconnected)));
    // ND1's sink never reached its `StartShutdown()`; V's stop set the flag.
    assert!(rig.v.stopping() && rig.v.shutdown_requested());
    let joined = rig.join.take().expect("running").join().expect("an unmarked joiner");
    assert!(joined.is_err(), "the sink's panic reaches V's joiner");
}

/// Sol cp1 M4: class C commands deferred at one release point run in arrival order, as the
/// `m_chainstate_mutex` waiters they stand for.
#[test]
fn deferred_class_c_commands_run_in_arrival_order() {
    let rig = Rig::new();
    let log = Log::default();
    let (open, gs) = gates(1);
    let (a, a_wait) = composite(&log, "A", 1, gs);
    let (b, b_wait) = composite(&log, "B", 1, Vec::new());
    let (c, c_wait) = composite(&log, "C", 1, Vec::new());
    for cmd in [a, b, c] {
        rig.v.send(cmd).expect("sent");
    }
    // B and C are queued before A's release point, which defers both.
    open[0].send(()).expect("A waits");
    for w in [a_wait, b_wait, c_wait] {
        w.recv().expect("ran");
    }
    assert_eq!(entries(&log), ["A:0", "B:0", "C:0"]);
}

/// §2.4: at a release point V runs the R and M commands queued at that moment, in order, and
/// defers class C ones until the running operation ends.
#[test]
fn release_points_serve_r_and_m_and_defer_c() {
    let rig = Rig::new();
    let log = Log::default();
    let (open, gs) = gates(1);
    let (a, a_wait) = composite(&log, "A", 2, gs);
    let (b, b_wait) = composite(&log, "B", 1, Vec::new());
    rig.v.send(a).expect("sent");
    rig.v.send(logged(&log, "q1")).expect("sent");
    rig.v
        .send(ChainCommand::SetLoading {
            importing: Some(true),
            reindex: None,
        })
        .expect("sent");
    rig.v.send(b).expect("sent");
    rig.v.send(logged(&log, "q2")).expect("sent");
    // Everything above is queued before A's first release point.
    open[0].send(()).expect("A waits");
    a_wait.recv().expect("A ran");
    b_wait.recv().expect("B ran");
    assert_eq!(entries(&log), ["q1", "q2", "A:0", "A:1", "B:0"]);
    assert!(rig.v.loading().importing());
}

/// A release point takes only the commands queued when it starts (`rx.len()`): one sent while
/// it runs waits for the next.
#[test]
fn a_release_point_takes_only_what_was_queued_at_its_start() {
    let rig = Rig::new();
    let log = Log::default();
    let (open, gs) = gates(1);
    let (a, a_wait) = composite(&log, "A", 2, gs);
    rig.v.send(a).expect("sent");
    let (v, l) = (rig.v.clone(), log.clone());
    rig.v
        .send(ChainCommand::Query(Box::new(move |_| {
            l.lock().expect("log").push("q1".into());
            v.send(logged(&l, "q2")).expect("room in the lane");
        })))
        .expect("sent");
    open[0].send(()).expect("A waits");
    a_wait.recv().expect("A ran");
    assert_eq!(entries(&log), ["q1", "A:0", "q2", "A:1"]);
}

/// Idle, V runs `deferred` before the channel: B (deferred at A's release point) runs before C,
/// which was queued after the last release point.
#[test]
fn deferred_work_runs_before_later_commands() {
    let rig = Rig::new();
    let log = Log::default();
    let (open, gs) = gates(2);
    let (a, a_wait) = composite(&log, "A", 1, gs);
    let (b, _b_wait) = composite(&log, "B", 1, Vec::new());
    let (c, c_wait) = composite(&log, "C", 1, Vec::new());
    rig.v.send(a).expect("sent");
    rig.v.send(b).expect("sent");
    open[0].send(()).expect("A waits at its release point");
    // B is deferred now (A logs A:0 after the release point); C stays in the channel.
    while entries(&log).is_empty() {
        std::thread::yield_now();
    }
    rig.v.send(c).expect("sent");
    open[1].send(()).expect("A waits after its release point");
    a_wait.recv().expect("A ran");
    c_wait.recv().expect("C ran");
    assert_eq!(entries(&log), ["A:0", "B:0", "C:0"]);
}

/// `ProcessNewBlock` at a release point (R6): the accept scope runs there, so an `Accepted`
/// reply arrives and the block is stored; the ABC tail is deferred, so an `Activated` reply
/// waits until the running operation has ended.
#[test]
fn a_block_at_a_release_point_defers_its_tail() {
    let rig = Rig::new();
    let x = child(&rig.p, &rig.genesis, 1);
    let y = child(&rig.p, &rig.genesis, 2);
    let log = Log::default();
    let (open, gs) = gates(2);
    let (a, a_wait) = composite(&log, "A", 1, gs);
    rig.v.send(a).expect("sent");
    let (xr, x_wait) = reply();
    let (yr, y_wait) = reply();
    for (m, r) in [(&x, BlockReply::Accepted(xr)), (&y, BlockReply::Activated(yr))] {
        rig.v
            .send(ChainCommand::ProcessBlock {
                block: m.block.clone(),
                hash: m.hash(),
                force: true,
                from: None,
                merkle_checked: false,
                reply: Some(r),
            })
            .expect("sent");
    }
    let (xh, yh) = (x.hash(), y.hash());
    let stored = rig.v.query(move |q| {
        let r = q.chain.reader();
        let has = |h| {
            r.lookup(&h).and_then(|id| r.entry(id)).is_some_and(|e| e.status.has(Status::HAVE_DATA))
        };
        (has(xh), has(yh))
    });
    open[0].send(()).expect("A waits");
    assert!(x_wait.recv().expect("accepted").accepted);
    assert_eq!(stored.recv().expect("served"), (true, true));
    assert!(y_wait.try_recv().is_none(), "the tail waits for A");
    open[1].send(()).expect("A waits after its release point");
    assert!(y_wait.recv().expect("activated").accepted);
    assert_eq!(a_wait.try_recv(), Some(Ok(())), "A ended before the tail ran");
}

/// D52: V's own stop (ND1's `Chain::stop` after the node's shutdown has started) is not the
/// node's `StartShutdown()`.
#[test]
fn a_shutdown_command_leaves_the_nodes_flag_clear() {
    let mut rig = Rig::new();
    rig.v.shutdown();
    assert!(rig.stop().fatal.is_none());
    assert!(rig.v.stopping() && !rig.shutdown.shutdown_requested());
}

/// D52: V's `AbortNode` reaches the node's flag through the sink (ND1's `NodeAbort` ends with
/// `StartShutdown()`, as Core's `AbortNode` does, `shutdown.cpp:36`).
#[test]
fn an_abort_node_starts_the_nodes_shutdown_through_the_sink() {
    let shutdown = Arc::new(LocalShutdown::default());
    let sink = Arc::new(FlagAtSink {
        shutdown: shutdown.clone(),
        seen: Mutex::default(),
    });
    let mut rig = Rig::with(RigConfig {
        shutdown: Some(shutdown),
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    rig.v.send(ChainCommand::TestOnV(Box::new(|_| Err(Fatal::new("disk on fire"))))).expect("sent");
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some("disk on fire"));
    // Core's `AbortNode` calls `StartShutdown()` after the rest (`shutdown.cpp:35-36`, Opus r1
    // m-2): the sink runs with the node's flag still clear.
    assert_eq!(*sink.seen.lock().expect("seen"), [("disk on fire".to_owned(), false)]);
    assert!(rig.shutdown.shutdown_requested());
}

/// An `AbortNode` sink that records each message with the node's `ShutdownRequested()` as it
/// was called.
struct FlagAtSink {
    shutdown: Arc<LocalShutdown>,
    seen: Mutex<Vec<(String, bool)>>,
}

impl AbortSink for FlagAtSink {
    fn abort_node(&self, msg: &str, _: Option<&str>) {
        let at = (msg.to_owned(), self.shutdown.shutdown_requested());
        self.seen.lock().expect("seen").push(at);
    }

    fn read_failed(&self, what: &str) {
        unreachable!("V reads no store here: {what}");
    }
}
