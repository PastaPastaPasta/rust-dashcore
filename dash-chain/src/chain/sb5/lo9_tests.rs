//! SB5 §4.9, SPV-LO9, threaded: `spv_lo9_actor_cycle`. The SPV header writer waits inside a
//! multi-segment `activate_segmented` hold on F, the filters task; F asks a model network actor
//! for a `cfilter` before popping; the actor first forwards an inbound `headers` to the header
//! task's full ingress channel. The no-wait marker and the actor are models of CH1-node's
//! (SB14 owns the real ones, SB5-Y2).

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::hold_tests::subscribe;
use super::segmented_tests::{bref, spv};
use crate::BlockId;
use crate::chain::tests::branch;
use crate::feed::{Next, Publish};
use crate::notify::{Notify, Parker};

const T: u32 = 1_700_000_000;
/// The compliant shapes finish well inside this.
const DONE: Duration = Duration::from_secs(30);
/// The negative control is judged deadlocked once its cycle has formed and held this long.
const STUCK: Duration = Duration::from_secs(2);
const SUB_STEPS: usize = 3;

thread_local! {
    /// CH1-node's no-wait marker, modelled per thread: the pop task carries it.
    static NO_WAIT: Cell<bool> = const { Cell::new(false) };
}

#[derive(Debug, PartialEq, Eq)]
struct WaitOnNoWaitTask;

/// The model actor reply: awaiting it from a marked task fails at once.
struct ActorReply(Receiver<()>);

impl ActorReply {
    fn wait(self) -> Result<(), WaitOnNoWaitTask> {
        if NO_WAIT.get() {
            return Err(WaitOnNoWaitTask);
        }
        self.0.recv().expect("the actor replies");
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    /// Marker on, blocking ingress `send`: F's await is refused, so it pops.
    MarkerOn,
    /// Marker off, blocking ingress `send` (the negative control): a cycle.
    Cycle,
    /// Marker off, F awaits; the actor uses `try_send`.
    TrySend,
    /// F posts the request and pops; the actor uses `try_send`.
    Compliant,
}

/// Where the cycle's two waits stand: F awaiting a reply, the actor inside a blocking `send`.
#[derive(Default)]
struct Waits {
    awaiting: AtomicBool,
    in_send: AtomicBool,
}

/// What a run reports: F's `seq`s, the refused awaits, the replies F handled.
#[derive(Default)]
struct Report {
    seqs: Vec<u64>,
    refused: usize,
    replies: usize,
}

/// Runs `shape`; returns whether it finished with no help (within `DONE`; for the cycle, see
/// `finished`), then its report.
fn run(shape: Shape) -> (bool, Report) {
    let mut c = spv(1, 1);
    let a = branch(&mut c, BlockId(0), 1, SUB_STEPS as u64, T);
    let to = bref(&c, a[SUB_STEPS - 1]);
    let wake = Parker::new();
    c.set_producer_wake(Notify::Thread(wake.clone()));
    let feeds = c.feeds();
    let (_, mut f) = subscribe(&feeds);
    let pre = c.snapshot().tip_seq;

    // The header task's ingress (capacity 1), already holding one message.
    let (ingress_tx, ingress_rx) = mpsc::sync_channel::<()>(1);
    ingress_tx.send(()).expect("room for one");
    let ingress_rx = Arc::new(Mutex::new(Some(ingress_rx)));
    let (mailbox, requests) = mpsc::channel::<mpsc::Sender<()>>();

    let waits = Arc::new(Waits::default());
    let actor = {
        let waits = waits.clone();
        std::thread::spawn(move || actor(shape, requests, ingress_tx, &waits))
    };
    let filters = {
        let waits = waits.clone();
        std::thread::spawn(move || {
            NO_WAIT.set(shape == Shape::MarkerOn);
            let p = Parker::new();
            f.set_waker(Notify::Thread(p.clone()));
            let (mut report, mut posted) = (Report::default(), Vec::new());
            while report.seqs.len() < SUB_STEPS {
                let seq = match f.next() {
                    Next::Event(e) => e.seq(),
                    Next::Control(_) => continue,
                    Next::Empty => {
                        p.park_timeout(Duration::from_millis(100));
                        continue;
                    }
                };
                // `Connected(b)`: ask the actor for `cfilter(b)` before popping it.
                let (tx, rx) = mpsc::channel();
                mailbox.send(tx).expect("the actor listens");
                match shape {
                    Shape::Compliant => posted.push(rx),
                    _ => {
                        waits.awaiting.store(true, Ordering::SeqCst);
                        match ActorReply(rx).wait() {
                            Ok(()) => report.replies += 1,
                            Err(WaitOnNoWaitTask) => report.refused += 1,
                        }
                    }
                }
                assert_eq!(f.pop().map(|e| e.seq()), Some(seq));
                report.seqs.push(seq);
            }
            // The posted requests' answers, handled as they arrive (after the pops).
            for rx in posted {
                rx.recv().expect("a reply");
                report.replies += 1;
            }
            drop(mailbox);
            report
        })
    };
    let (done_tx, done) = mpsc::channel();
    let writer = {
        let ingress_rx = ingress_rx.clone();
        std::thread::spawn(move || {
            let s = c
                .activate_segmented(to, &mut || {
                    wake.park_timeout(Duration::from_millis(100));
                })
                .expect("activates");
            assert_eq!(s.sub_steps, SUB_STEPS as u32);
            while c.retry_pending() == Publish::Blocked {
                wake.park_timeout(Duration::from_millis(100));
            }
            let _ = done_tx.send(());
            // Back in its loop, the header task reads its ingress until the actor leaves.
            let rx = ingress_rx.lock().expect("ingress").take();
            if let Some(rx) = rx {
                while rx.recv().is_ok() {}
            }
        })
    };
    let unaided = finished(shape, &waits, &done);
    if !unaided {
        // Break the cycle: the header task's ingress goes away, so the actor's send fails.
        drop(ingress_rx.lock().expect("ingress").take());
        done.recv_timeout(DONE).expect("the run finishes once the cycle is broken");
    }
    writer.join().expect("the writer");
    let report = filters.join().expect("F");
    actor.join().expect("the actor");
    assert_eq!(report.seqs, (pre + 1..=pre + SUB_STEPS as u64).collect::<Vec<_>>());
    (unaided, report)
}

/// Whether the writer finished with no help. A shape finishes within `DONE`; for the cycle,
/// it waits up to `DONE` for both waits to form (finishing first counts as finished), then
/// judges it deadlocked if `done` stays silent for `STUCK` with the actor still in `send`.
fn finished(shape: Shape, waits: &Waits, done: &Receiver<()>) -> bool {
    let judged = |r: Result<(), RecvTimeoutError>| match r {
        Ok(()) => true,
        Err(RecvTimeoutError::Timeout) => false,
        Err(RecvTimeoutError::Disconnected) => panic!("the writer failed"),
    };
    if shape != Shape::Cycle {
        return judged(done.recv_timeout(DONE));
    }
    let formed_by = Instant::now() + DONE;
    while !(waits.awaiting.load(Ordering::SeqCst) && waits.in_send.load(Ordering::SeqCst)) {
        assert!(Instant::now() < formed_by, "the cycle's waits never formed");
        if judged(done.recv_timeout(Duration::from_millis(10))) {
            return true;
        }
    }
    let unaided = judged(done.recv_timeout(STUCK));
    assert!(unaided || waits.in_send.load(Ordering::SeqCst), "the actor's send returned");
    unaided
}

/// The network actor: per `cfilter` request, it forwards an inbound `headers` to the header
/// task, then replies. Under `try_send` a full ingress pauses that peer (`fPauseRecv`).
fn actor(shape: Shape, requests: Receiver<mpsc::Sender<()>>, ingress: SyncSender<()>, w: &Waits) {
    for reply in requests {
        match shape {
            // Every outcome is fine: `Full` pauses the peer, `Disconnected` is shutdown.
            Shape::TrySend | Shape::Compliant => {
                let _ = ingress.try_send(());
            }
            Shape::MarkerOn | Shape::Cycle => {
                w.in_send.store(true, Ordering::SeqCst);
                let _ = ingress.send(());
                w.in_send.store(false, Ordering::SeqCst);
            }
        }
        let _ = reply.send(());
    }
}

/// `spv_lo9_actor_cycle` (DEC-157 (1), Opus high M2). SB5-Y2's two rules each break the cycle
/// alone:
/// - the marker on, with a blocking `send`: F's await fails with `WaitOnNoWaitTask` and the run
///   completes;
/// - `try_send` ingress, with F awaiting and no marker: the run completes, F gets every reply;
/// - neither (the negative control): once F awaits and the actor is in `send`, the run makes no
///   progress;
/// - the compliant shape (post, then pop; `try_send` ingress) completes, F gets every reply.
#[test]
fn spv_lo9_actor_cycle() {
    let (unaided, r) = run(Shape::MarkerOn);
    assert!(unaided, "the marker breaks the cycle");
    assert_eq!(r.refused, SUB_STEPS, "every await from the pop task is refused");

    let (unaided, r) = run(Shape::Cycle);
    assert!(!unaided, "an awaiting pop task and a blocking send deadlock");
    assert_eq!(r.replies, SUB_STEPS);

    let (unaided, r) = run(Shape::TrySend);
    assert!(unaided, "try_send ingress breaks the cycle");
    assert_eq!((r.refused, r.replies), (0, SUB_STEPS));

    let (unaided, r) = run(Shape::Compliant);
    assert!(unaided, "post-then-pop with try_send ingress completes");
    assert_eq!((r.refused, r.replies), (0, SUB_STEPS));
}
