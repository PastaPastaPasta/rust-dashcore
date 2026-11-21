//! LK1a F: a queued `BlockConnected` held on a listener's [`Ticket`], against V. The holder
//! posts its job to a worker of the test's, which waits at a gate, then runs V's class M
//! ChainLock commands (`ClIntake`, the verifier's `OnV`, `RecordChainLock`) and completes the
//! ticket. Core runs the coinbase ChainLock inside `PeerManagerImpl::BlockConnected`
//! (`net_processing.cpp:2167-2169` at v24.0.0), so the event's later listeners, every later event
//! and every barrier see its effects; and V, waiting in `LimitValidationInterfaceQueue`
//! meanwhile, serves those commands.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use dash_consensus::{Block, BlockId};
use dash_num::Hash256;
use dash_types::Numeric as _;

use super::llmq_members::world;
use super::locks::lock;
use super::support::{Mined, Rig, RigConfig, branch, regtest};
use crate::chain::reply::{no_wait_marked, reply, spawn_thread};
use crate::chain::{
    Blocking, ChainCommand, ChainHandle, Cont, Fatal, Outcome, Queued, Ticket, Tickets,
    ValidationInterface, WaitError,
};

const SOON: Duration = Duration::from_secs(120);
/// How long a check waits for something that must not happen.
const QUIET: Duration = Duration::from_millis(300);

/// What the worker does with a job once its gate opens.
#[derive(Clone, Copy, Debug)]
enum Work {
    /// `ClIntake`, an `OnV` turn as the verifier's, and `RecordChainLock` for the lock on the
    /// job's block, then [`Outcome::Done`].
    Record,
    /// `ClIntake` only: [`Outcome::Stopped`] if V has stopped, else [`Outcome::Done`].
    Intake,
    /// Completes [`Outcome::Stopped`] with V running.
    Stopped,
    /// Drops the ticket.
    Drop,
    /// Panics holding the ticket.
    Panic,
}

/// A held job: the block and its ticket.
struct Job {
    block: Arc<Block>,
    ticket: Ticket,
}

/// The holding listener: posts each `BlockConnected` to the worker and answers `Pending`. It
/// records whether it ran marked (LO-9) and never waits.
struct Holder {
    tickets: Tickets,
    jobs: Mutex<mpsc::Sender<Job>>,
    marked: Mutex<Vec<bool>>,
}

impl ValidationInterface for Holder {
    fn block_connected(&self, block: &Arc<Block>, _: BlockId) -> Cont {
        self.marked.lock().expect("marked").push(no_wait_marked());
        let (ticket, hold) = self.tickets.issue();
        let job = Job {
            block: Arc::clone(block),
            ticket,
        };
        self.jobs.lock().expect("jobs").send(job).expect("the worker runs");
        Cont::Pending(hold)
    }
}

/// A listener that records, per callback, V's best ChainLock height as it ran.
struct Seen {
    v: ChainHandle,
    tag: &'static str,
    log: Arc<Mutex<Vec<(&'static str, i32)>>>,
}

impl Seen {
    fn note(&self, what: &'static str) {
        let best = self.v.view().cl.best_height();
        self.log.lock().expect("log").push((what, best));
    }
}

impl ValidationInterface for Seen {
    fn block_connected(&self, _: &Arc<Block>, _: BlockId) -> Cont {
        self.note(self.tag);
        Cont::Done
    }

    fn updated_block_tip(&self, _: BlockId, _: Option<BlockId>, _: bool) {
        self.note("tip");
    }
}

/// The test's worker: per job, waits for `gate`, does `work`, and reports each V reply.
struct Worker {
    gate: mpsc::Sender<()>,
    got: mpsc::Receiver<()>,
    replies: mpsc::Receiver<Result<bool, WaitError>>,
}

fn worker(v: &ChainHandle, work: Work) -> (mpsc::Sender<Job>, Worker) {
    let (jobs, rx) = mpsc::channel::<Job>();
    let (gate, gated) = mpsc::channel();
    let (got_tx, got) = mpsc::channel();
    let (reply_tx, replies) = mpsc::channel();
    let v = v.clone();
    spawn_thread("cl-cb-test", move || {
        while let Ok(job) = rx.recv() {
            let _ = got_tx.send(());
            if gated.recv().is_err() {
                return;
            }
            match work {
                Work::Drop => drop(job.ticket),
                Work::Panic => panic!("the worker failed"),
                Work::Stopped => job.ticket.complete(Outcome::Stopped),
                Work::Intake | Work::Record => {
                    let intake = v.cl_intake().recv();
                    let _ = reply_tx.send(intake.map(|_| true));
                    if intake.is_err() {
                        job.ticket.complete(Outcome::Stopped);
                        continue;
                    }
                    if let Work::Record = work {
                        let verified = v.on_v(|_, _| ()).recv();
                        let _ = reply_tx.send(verified.map(|()| true));
                        let m = Mined {
                            height: v
                                .view_handle()
                                .lookup(&job.block.hash())
                                .and_then(|id| v.view_handle().entry(id))
                                .expect("connected")
                                .height,
                            block: Arc::clone(&job.block),
                        };
                        let sig = lock(&m);
                        let hash = Hash256::from_lendian([m.height as u8; 32]);
                        let recorded = v.record_chainlock(sig, hash, None).recv();
                        let _ = reply_tx.send(recorded.map(|r| r.relay));
                    }
                    job.ticket.complete(Outcome::Done);
                }
            }
        }
    })
    .expect("spawn the worker");
    (
        jobs,
        Worker {
            gate,
            got,
            replies,
        },
    )
}

/// A rig with route B's facade, so V runs `OnV`, and the holder, then `seen`'s `after` listener,
/// registered in that order. The worker drops first, so a failed check's unwind ends a held job
/// before the rig stops.
struct Held {
    worker: Worker,
    rig: Rig,
    holder: Arc<Holder>,
    log: Arc<Mutex<Vec<(&'static str, i32)>>>,
    listeners: Vec<Arc<dyn ValidationInterface>>,
}

impl Held {
    fn new(work: Work) -> Held {
        let rig = Rig::with(RigConfig {
            facade: Some(world(&regtest()).facade),
            ..RigConfig::default()
        });
        rig.signals.sync(&mut Blocking).expect("startup's signals ran");
        let (jobs, worker) = worker(&rig.v, work);
        let holder = Arc::new(Holder {
            tickets: rig.signals.tickets(),
            jobs: Mutex::new(jobs),
            marked: Mutex::default(),
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let after: Arc<dyn ValidationInterface> = Arc::new(Seen {
            v: rig.v.clone(),
            tag: "after",
            log: Arc::clone(&log),
        });
        let listeners = vec![Arc::clone(&holder) as Arc<dyn ValidationInterface>, after];
        for l in &listeners {
            rig.signals.register(Arc::clone(l));
        }
        Held {
            worker,
            rig,
            holder,
            log,
            listeners,
        }
    }

    fn log(&self) -> Vec<(&'static str, i32)> {
        self.log.lock().expect("log").clone()
    }

    /// A `sync` on its own thread, its result sent when it returns.
    fn sync_aside(&self) -> mpsc::Receiver<Result<(), WaitError>> {
        let (tx, rx) = mpsc::channel();
        let signals = self.rig.signals.clone();
        std::thread::spawn(move || {
            let _ = tx.send(signals.sync(&mut Blocking));
        });
        rx
    }

    /// `fence_connected(l)` on its own thread, its result sent when it returns.
    fn fence_aside(
        &self,
        l: &Arc<dyn ValidationInterface>,
    ) -> mpsc::Receiver<Result<(), WaitError>> {
        let (tx, rx) = mpsc::channel();
        let (signals, l) = (self.rig.signals.clone(), Arc::clone(l));
        std::thread::spawn(move || {
            let _ = tx.send(signals.fence_connected(&l, &mut Blocking));
        });
        rx
    }

    fn close(self) {
        drop(self.worker.gate);
        for l in &self.listeners {
            self.rig.signals.unregister(l);
        }
    }
}

/// The worker's job holds the block's `BlockConnected`: the event's next listener, the block's
/// `UpdatedBlockTip` and a later barrier all wait for it, then see the lock it recorded; the
/// holder ran marked and returned at once (LO-9).
#[test]
fn a_held_block_connected_resumes_after_the_job_and_before_later_events() {
    let held = Held::new(Work::Record);
    let m = branch(&held.rig.p, &held.rig.genesis, 1, 0).remove(0);
    held.rig.accept(&m);
    held.worker.got.recv_timeout(SOON).expect("the holder posted the job");
    let synced = held.sync_aside();
    assert_eq!(synced.recv_timeout(QUIET), Err(mpsc::RecvTimeoutError::Timeout));
    assert!(held.log().is_empty(), "nothing after the holder ran: {:?}", held.log());
    assert!(held.rig.signals.callbacks_pending() >= 1, "the tip waits behind the held event");
    assert_eq!(*held.holder.marked.lock().unwrap(), [true], "the holder ran marked, unblocked");

    held.worker.gate.send(()).expect("the worker waits");
    assert_eq!(held.worker.replies.recv_timeout(SOON).expect("intake"), Ok(true));
    assert_eq!(held.worker.replies.recv_timeout(SOON).expect("verify"), Ok(true));
    assert_eq!(held.worker.replies.recv_timeout(SOON).expect("record"), Ok(true), "relayed");
    assert_eq!(synced.recv_timeout(SOON).expect("the barrier"), Ok(()));
    let height = i32::try_from(m.height).unwrap();
    assert_eq!(held.log(), [("after", height), ("tip", height)]);
    assert!(held.rig.abort.messages().is_empty());
    held.close();
}

/// V waits in `LimitValidationInterfaceQueue` behind a held event with eleven more queued; the
/// worker's `ClIntake`, `OnV` and `RecordChainLock` are served in that wait, so the job completes
/// and the wait ends.
#[test]
fn v_limit_queue_behind_a_held_job_serves_the_workers_commands() {
    let held = Held::new(Work::Record);
    let m = branch(&held.rig.p, &held.rig.genesis, 1, 0).remove(0);
    held.rig.accept(&m);
    held.worker.got.recv_timeout(SOON).expect("the holder posted the job");
    for i in 0..11 {
        held.rig.signals.enqueue(Queued::UpdatedBlockTip {
            new: BlockId(100 + i),
            fork: None,
            ibd: false,
        });
    }
    let (done, waited) = reply();
    held.rig
        .v
        .send(ChainCommand::TestUnderV(Box::new(move |v| {
            v.limit_queue()?;
            done.send(());
            Ok(())
        })))
        .expect("sent");
    assert_eq!(held.rig.v.query(|_| ()).recv_timeout(SOON), Some(Ok(())), "V waits, serving");
    assert!(waited.try_recv().is_none(), "the class C operation waits");
    held.worker.gate.send(()).expect("the worker waits");
    assert_eq!(held.worker.replies.recv_timeout(SOON).expect("intake"), Ok(true));
    assert!(waited.try_recv().is_none(), "the class C operation still waits");
    assert_eq!(held.worker.replies.recv_timeout(SOON).expect("verify"), Ok(true));
    assert_eq!(held.worker.replies.recv_timeout(SOON).expect("record"), Ok(true));
    waited.recv_timeout(SOON).expect("the wait ends").expect("ran");
    assert!(held.rig.signals.callbacks_pending() <= 10);
    held.rig.sync();
    assert!(held.rig.abort.messages().is_empty());
    held.close();
}

/// A ticket the worker drops, or holds as it panics, is `Aborted`: the node's `AbortNode`, once,
/// with the queue closed behind it, as a listener's panic.
#[test]
fn a_dropped_or_panicked_ticket_aborts_once_and_closes_the_queue() {
    for work in [Work::Drop, Work::Panic] {
        let held = Held::new(work);
        let m = branch(&held.rig.p, &held.rig.genesis, 1, 0).remove(0);
        held.rig.accept(&m);
        held.worker.got.recv_timeout(SOON).expect("the holder posted the job");
        let synced = held.sync_aside();
        held.worker.gate.send(()).expect("the worker waits");
        assert_eq!(synced.recv_timeout(SOON), Ok(Err(WaitError::Disconnected)), "{work:?}");
        assert_eq!(
            held.rig.abort.messages(),
            ["a validation interface listener's pending work ended Aborted"],
            "{work:?}"
        );
        assert!(held.rig.v.shutdown_requested(), "{work:?}");
        assert!(held.log().is_empty(), "the held event is not resumed: {work:?}");
        held.close();
    }
}

/// `Stopped` from a worker while V has not stopped fatally is a failure, as `Aborted`.
#[test]
fn stopped_with_a_healthy_v_aborts() {
    let held = Held::new(Work::Stopped);
    let m = branch(&held.rig.p, &held.rig.genesis, 1, 0).remove(0);
    held.rig.accept(&m);
    held.worker.got.recv_timeout(SOON).expect("the holder posted the job");
    held.worker.gate.send(()).expect("the worker waits");
    let deadline = std::time::Instant::now() + SOON;
    while held.rig.abort.messages().is_empty() {
        assert!(std::time::Instant::now() < deadline, "no abort");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        held.rig.abort.messages(),
        ["a validation interface listener's pending work ended Stopped"]
    );
    assert!(!held.rig.signals.v_fatal());
    assert_eq!(held.rig.signals.sync(&mut Blocking), Err(WaitError::Disconnected));
    held.close();
}

/// V stops on an `AbortNode` while the job waits before `ClIntake`: the intake sees V stopped
/// and the job completes `Stopped`. The signals thread raises no second `AbortNode` and resumes
/// the event, so its next listener, the tip and a barrier still run.
#[test]
fn stopped_after_a_fatal_v_stop_resumes_without_a_second_abort() {
    let held = Held::new(Work::Intake);
    let m = branch(&held.rig.p, &held.rig.genesis, 1, 0).remove(0);
    held.rig.accept(&m);
    held.worker.got.recv_timeout(SOON).expect("the holder posted the job");
    let _ = held.rig.v.send(ChainCommand::TestUnderV(Box::new(|_| Err(Fatal::new("injected")))));
    let deadline = std::time::Instant::now() + SOON;
    while !held.rig.signals.v_fatal() {
        assert!(std::time::Instant::now() < deadline, "V never stopped");
        std::thread::sleep(Duration::from_millis(5));
    }
    held.worker.gate.send(()).expect("the worker waits");
    assert!(held.worker.replies.recv_timeout(SOON).expect("intake").is_err(), "V stopped");
    assert_eq!(held.rig.signals.sync(&mut Blocking), Ok(()), "the event resumed");
    assert_eq!(held.log(), [("after", -1), ("tip", -1)]);
    assert_eq!(held.rig.abort.messages(), ["injected"], "no second AbortNode");
    held.close();
}

/// A listener whose `ChainStateFlushed` waits for the test, then notes it finished.
struct Stall(Mutex<mpsc::Receiver<()>>, std::sync::atomic::AtomicBool);

impl Stall {
    fn new(release: mpsc::Receiver<()>) -> Arc<Stall> {
        Arc::new(Stall(Mutex::new(release), std::sync::atomic::AtomicBool::new(false)))
    }

    fn finished(&self) -> bool {
        self.1.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl ValidationInterface for Stall {
    fn chain_state_flushed(&self, _: &[dash_primitives::BlockHash]) {
        let _ = self.0.lock().expect("stall").recv_timeout(SOON);
        self.1.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Shutdown's fence after unregistering the holder: it waits while the held event, whose
/// snapshot took the holder, runs, through its later listener; it does not wait for the signals
/// queued behind it, which Core runs after the scheduler stops (`init.cpp:347-361` at v24.0.0).
#[test]
fn the_fence_waits_for_the_held_event_that_took_the_listener_and_not_for_the_queue() {
    let held = Held::new(Work::Record);
    let (release, stalled) = mpsc::channel();
    let stall = Stall::new(stalled);
    let stall_l: Arc<dyn ValidationInterface> = Arc::clone(&stall) as _;
    held.rig.signals.register(Arc::clone(&stall_l));
    let m = branch(&held.rig.p, &held.rig.genesis, 1, 0).remove(0);
    held.rig.accept(&m);
    held.worker.got.recv_timeout(SOON).expect("the holder posted the job");
    held.rig.signals.enqueue(Queued::ChainStateFlushed(Vec::new()));
    let holder = Arc::clone(&held.listeners[0]);
    held.rig.signals.unregister(&holder);

    let fenced = held.fence_aside(&holder);
    assert_eq!(fenced.recv_timeout(QUIET), Err(mpsc::RecvTimeoutError::Timeout), "held");
    held.worker.gate.send(()).expect("the worker waits");
    assert_eq!(fenced.recv_timeout(SOON).expect("the fence"), Ok(()));
    let height = i32::try_from(m.height).unwrap();
    assert_eq!(held.log().first(), Some(&("after", height)), "the event's rest ran first");
    assert!(!stall.finished(), "the queued flush not waited for");

    release.send(()).expect("the stall waits");
    held.rig.signals.sync(&mut Blocking).expect("the queue drains");
    assert!(stall.finished());
    held.rig.signals.unregister(&stall_l);
    assert!(held.rig.abort.messages().is_empty());
    held.close();
}

/// A fence for a listener the running event's snapshot does not hold returns at once, the
/// holder's job still held.
#[test]
fn the_fence_does_not_wait_for_an_event_without_the_listener() {
    let held = Held::new(Work::Record);
    let m = branch(&held.rig.p, &held.rig.genesis, 1, 0).remove(0);
    held.rig.accept(&m);
    held.worker.got.recv_timeout(SOON).expect("the holder posted the job");
    let stranger: Arc<dyn ValidationInterface> = Stall::new(mpsc::channel().1);
    let fenced = held.fence_aside(&stranger);
    assert_eq!(fenced.recv_timeout(SOON).expect("the fence"), Ok(()));
    assert!(held.log().is_empty(), "still held: {:?}", held.log());
    held.worker.gate.send(()).expect("the worker waits");
    held.rig.signals.sync(&mut Blocking).expect("the event resumed");
    assert!(held.rig.abort.messages().is_empty());
    held.close();
}

/// A `BlockConnected` still queued when the holder is unregistered: the fence returns at once,
/// and the dispatch, run later, takes the list without the holder.
#[test]
fn a_fence_after_unregister_skips_a_still_queued_event() {
    let held = Held::new(Work::Record);
    let (release, stalled) = mpsc::channel();
    let stall = Stall::new(stalled);
    let stall_l: Arc<dyn ValidationInterface> = Arc::clone(&stall) as _;
    held.rig.signals.register(Arc::clone(&stall_l));
    held.rig.signals.enqueue(Queued::ChainStateFlushed(Vec::new()));
    let m = branch(&held.rig.p, &held.rig.genesis, 1, 0).remove(0);
    held.rig.accept(&m);
    let holder = Arc::clone(&held.listeners[0]);
    held.rig.signals.unregister(&holder);

    let fenced = held.fence_aside(&holder);
    assert_eq!(fenced.recv_timeout(SOON).expect("the fence"), Ok(()));
    assert!(!stall.finished(), "the queue ahead still stalled");
    release.send(()).expect("the stall waits");
    held.rig.signals.sync(&mut Blocking).expect("the queue drains");
    assert_eq!(held.worker.got.recv_timeout(QUIET), Err(mpsc::RecvTimeoutError::Timeout));
    assert!(held.holder.marked.lock().expect("marked").is_empty(), "the holder never ran");
    assert_eq!(held.log(), [("after", -1), ("tip", -1)]);
    held.rig.signals.unregister(&stall_l);
    assert!(held.rig.abort.messages().is_empty());
    held.close();
}

/// A refused hold ends the dispatch's record with the rest of the event: the fence that waited
/// for it returns.
#[test]
fn a_refused_hold_releases_the_fence() {
    let held = Held::new(Work::Drop);
    let m = branch(&held.rig.p, &held.rig.genesis, 1, 0).remove(0);
    held.rig.accept(&m);
    held.worker.got.recv_timeout(SOON).expect("the holder posted the job");
    let holder = Arc::clone(&held.listeners[0]);
    held.rig.signals.unregister(&holder);
    let fenced = held.fence_aside(&holder);
    assert_eq!(fenced.recv_timeout(QUIET), Err(mpsc::RecvTimeoutError::Timeout), "held");
    held.worker.gate.send(()).expect("the worker waits");
    assert_eq!(fenced.recv_timeout(SOON).expect("the fence"), Ok(()));
    assert!(held.log().is_empty(), "the refused event is not resumed");
    held.close();
}
