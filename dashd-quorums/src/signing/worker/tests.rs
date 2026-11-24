use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use dashd_chainstate::chain::reply::no_wait_marked;

use super::*;

const WAIT: Duration = Duration::from_secs(20);

/// A pass's thread: whether it is marked, and its name.
type Seen = (bool, Option<String>);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Pass(bool),
    Cleanup,
    Sleep(Duration),
}

/// A work whose passes answer `more` in order, then `Err(fault)` or no more work.
#[derive(Default)]
struct Script {
    more: Mutex<VecDeque<bool>>,
    fault: Option<SigningFault>,
    cleanup_fault: Option<SigningFault>,
    log: Arc<Mutex<Vec<Step>>>,
    /// Each pass sends whether its thread is marked, and its name.
    seen: Option<Mutex<Sender<Seen>>>,
}

impl Script {
    fn answering(more: &[bool]) -> Self {
        Self {
            more: Mutex::new(more.iter().copied().collect()),
            ..Self::default()
        }
    }

    fn reporting() -> (Self, Receiver<Seen>) {
        let (tx, rx) = mpsc::channel();
        let script = Self {
            seen: Some(Mutex::new(tx)),
            ..Self::default()
        };
        (script, rx)
    }

    fn log(&self) -> Vec<Step> {
        self.log.lock().unwrap().clone()
    }
}

impl RecSigsWork for Script {
    fn process_pending(&self) -> Result<bool, SigningFault> {
        if let Some(seen) = &self.seen {
            let name = thread::current().name().map(str::to_owned);
            let _ = seen.lock().unwrap().send((no_wait_marked(), name));
        }
        if let Some(fault) = &self.fault {
            return Err(fault.clone());
        }
        let more = self.more.lock().unwrap().pop_front().unwrap_or(false);
        self.log.lock().unwrap().push(Step::Pass(more));
        Ok(more)
    }

    fn cleanup(&self) -> Result<bool, SigningFault> {
        if let Some(fault) = &self.cleanup_fault {
            return Err(fault.clone());
        }
        self.log.lock().unwrap().push(Step::Cleanup);
        Ok(true)
    }
}

fn no_abort() -> RecSigsAbort {
    Box::new(|fault| panic!("unexpected abort: {fault}"))
}

fn abort_channel() -> (RecSigsAbort, Receiver<SigningFault>) {
    let (tx, rx) = mpsc::channel();
    (
        Box::new(move |fault| {
            let _ = tx.send(fault);
        }),
        rx,
    )
}

/// Every pass is followed by `Cleanup`; only a pass with no more work sleeps, for 100 ms, and an
/// interrupted sleep ends the loop.
// Core: v24.0.2 src/llmq/net_signing.cpp:283-298
#[test]
fn the_loop_sleeps_100ms_only_without_more_work() {
    let work = Script::answering(&[true, false, true, true, false]);
    let interrupt = SigningInterrupt::new();
    let log = work.log.clone();
    let mut sleeps = 0;
    let sleep = |d| {
        log.lock().unwrap().push(Step::Sleep(d));
        sleeps += 1;
        sleeps < 2
    };
    assert_eq!(run(&work, &interrupt, sleep), Ok(()));
    let (pass, idle) = (|more| Step::Pass(more), Step::Sleep(IDLE_SLEEP));
    assert_eq!(
        work.log(),
        [
            pass(true),
            Step::Cleanup,
            pass(false),
            Step::Cleanup,
            idle.clone(),
            pass(true),
            Step::Cleanup,
            pass(true),
            Step::Cleanup,
            pass(false),
            Step::Cleanup,
            idle,
        ]
    );
}

/// An interrupt set before the loop runs no pass, and one set while there is more work ends
/// the loop at the next check, with no sleep.
#[test]
fn an_interrupt_ends_the_loop_at_its_next_check() {
    let interrupt = SigningInterrupt::new();
    interrupt.trigger();
    let work = Script::answering(&[true]);
    assert_eq!(run(&work, &interrupt, |_| unreachable!()), Ok(()));
    assert!(work.log().is_empty());

    struct Interrupting(Script, SigningInterrupt);
    impl RecSigsWork for Interrupting {
        fn process_pending(&self) -> Result<bool, SigningFault> {
            self.1.trigger();
            self.0.process_pending()
        }
        fn cleanup(&self) -> Result<bool, SigningFault> {
            self.0.cleanup()
        }
    }
    let interrupt = SigningInterrupt::new();
    let work = Interrupting(Script::answering(&[true, true]), interrupt.clone());
    assert_eq!(run(&work, &interrupt, |_| unreachable!()), Ok(()));
    assert_eq!(work.0.log(), [Step::Pass(true), Step::Cleanup]);
}

/// A pass's or a cleanup's fault ends the loop at once, before the cleanup or the sleep.
#[test]
fn a_fault_ends_the_loop_at_once() {
    let interrupt = SigningInterrupt::new();
    let work = Script {
        fault: Some(SigningFault::Lookup("q".into())),
        ..Script::default()
    };
    assert_eq!(run(&work, &interrupt, |_| unreachable!()), Err(SigningFault::Lookup("q".into())));
    assert!(work.log().is_empty());
    let work = Script {
        cleanup_fault: Some(SigningFault::Storage("db".into())),
        ..Script::answering(&[false])
    };
    assert_eq!(run(&work, &interrupt, |_| unreachable!()), Err(SigningFault::Storage("db".into())));
    assert_eq!(work.log(), [Step::Pass(false)]);
}

fn passes(work: &Script) -> usize {
    work.log().iter().filter(|s| matches!(s, Step::Pass(_))).count()
}

/// The thread is "recsigs" and unmarked, as its listeners may wait on V; `interrupt` and `stop`
/// join it within the tick in progress, with no pass after the interrupt but that one.
// Core: v24.0.2 src/llmq/net_signing.cpp:152-160, :174-182
#[test]
fn the_thread_is_unmarked_and_interrupt_then_stop_joins_it() {
    let (work, seen) = Script::reporting();
    let work = Arc::new(work);
    let mut worker =
        RecSigsWorker::start(work.clone(), SigningInterrupt::new(), no_abort(), std_spawn).unwrap();
    assert_eq!(seen.recv_timeout(WAIT), Ok((false, Some("recsigs".into()))));
    worker.interrupt();
    let at_interrupt = passes(&work);
    worker.stop();
    assert!(passes(&work) - at_interrupt <= 1, "at most the pass in progress");
    assert!(worker.thread.is_none());
}

/// A busy loop, never sleeping, still stops on the interrupt.
#[test]
fn a_busy_loop_stops_on_the_interrupt() {
    struct Busy(Mutex<Sender<()>>);
    impl RecSigsWork for Busy {
        fn process_pending(&self) -> Result<bool, SigningFault> {
            let _ = self.0.lock().unwrap().send(());
            Ok(true)
        }
        fn cleanup(&self) -> Result<bool, SigningFault> {
            Ok(false)
        }
    }
    let (tx, rx) = mpsc::channel();
    let interrupt = SigningInterrupt::new();
    let mut worker = RecSigsWorker::start(
        Arc::new(Busy(Mutex::new(tx))),
        interrupt.clone(),
        no_abort(),
        std_spawn,
    )
    .unwrap();
    rx.recv_timeout(WAIT).unwrap();
    interrupt.trigger();
    worker.stop();
}

#[test]
#[should_panic(expected = "recsigs stopped before its interrupt")]
fn stop_before_interrupt_is_a_programming_error() {
    let mut worker = RecSigsWorker::start(
        Arc::new(Script::default()),
        SigningInterrupt::new(),
        no_abort(),
        std_spawn,
    )
    .unwrap();
    worker.stop();
}

/// A `Storage` or `Lookup` fault reaches the abort once and ends the thread; `Stopped` ends it
/// quietly.
#[test]
fn a_fault_reaches_the_abort_and_stopped_is_dropped() {
    for fault in [SigningFault::Storage("db".into()), SigningFault::Lookup("q".into())] {
        let (abort, aborted) = abort_channel();
        let work = Script {
            fault: Some(fault.clone()),
            ..Script::default()
        };
        let mut worker =
            RecSigsWorker::start(Arc::new(work), SigningInterrupt::new(), abort, std_spawn)
                .unwrap();
        assert_eq!(aborted.recv_timeout(WAIT), Ok(fault));
        worker.thread.take().unwrap().join().unwrap();
        assert!(aborted.try_recv().is_err());
    }
    let (abort, aborted) = abort_channel();
    let work = Script {
        fault: Some(SigningFault::Stopped),
        ..Script::default()
    };
    let mut worker =
        RecSigsWorker::start(Arc::new(work), SigningInterrupt::new(), abort, std_spawn).unwrap();
    worker.thread.take().unwrap().join().unwrap();
    assert!(aborted.try_recv().is_err(), "Stopped is never fatal");
}

/// Dropping a running worker interrupts and joins it.
#[test]
fn drop_interrupts_and_joins() {
    let interrupt = SigningInterrupt::new();
    let worker =
        RecSigsWorker::start(Arc::new(Script::default()), interrupt.clone(), no_abort(), std_spawn)
            .unwrap();
    drop(worker);
    assert!(interrupt.is_set());
}

/// A panic in a pass, a listener's on this thread, escapes the worker's body to its spawner:
/// it is neither swallowed nor turned into a quiet exit or a fault, so production's
/// `trace_thread` ends the process as Core's `TraceThread` does.
// Core: v24.0.2 src/llmq/net_signing.cpp:159; src/util/thread.cpp:16-26
#[test]
fn a_panicking_pass_escapes_to_the_spawner() {
    struct Panics;
    impl RecSigsWork for Panics {
        fn process_pending(&self) -> Result<bool, SigningFault> {
            panic!("a listener panicked");
        }
        fn cleanup(&self) -> Result<bool, SigningFault> {
            unreachable!("no cleanup after the panic")
        }
    }
    let (caught_tx, caught) = mpsc::channel();
    let catching = move |name: &str, body: RecSigsBody| {
        thread::Builder::new().name(name.into()).spawn(move || {
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).err();
            let text = panic.as_ref().and_then(|p| p.downcast_ref::<&str>().copied());
            let _ = caught_tx
                .send((thread::current().name().map(str::to_owned), text.map(str::to_owned)));
        })
    };
    let (abort, aborted) = abort_channel();
    let interrupt = SigningInterrupt::new();
    let mut worker =
        RecSigsWorker::start(Arc::new(Panics), interrupt.clone(), abort, catching).unwrap();
    assert_eq!(
        caught.recv_timeout(WAIT),
        Ok((Some("recsigs".into()), Some("a listener panicked".into())))
    );
    interrupt.trigger();
    worker.stop();
    assert!(aborted.try_recv().is_err(), "a panic is not a fault");
}

/// A panic the spawner lets reach the join is resumed by `stop`, never discarded.
#[test]
#[should_panic(expected = "a listener panicked")]
fn stop_resumes_a_panic_that_reached_the_join() {
    struct Panics;
    impl RecSigsWork for Panics {
        fn process_pending(&self) -> Result<bool, SigningFault> {
            panic!("a listener panicked");
        }
        fn cleanup(&self) -> Result<bool, SigningFault> {
            unreachable!()
        }
    }
    let interrupt = SigningInterrupt::new();
    let mut worker =
        RecSigsWorker::start(Arc::new(Panics), interrupt.clone(), no_abort(), std_spawn).unwrap();
    let start = Instant::now();
    while !worker.thread.as_ref().unwrap().is_finished() {
        assert!(start.elapsed() < WAIT, "the pass never ran");
        thread::yield_now();
    }
    interrupt.trigger();
    worker.stop();
}
