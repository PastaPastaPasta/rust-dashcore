//! The drivers (CH2 §2.7): what runs W's flush machines. [`ThreadDriver`] runs them on W's own
//! thread (production); [`InlineDriver`] runs them on the caller's, one step at a time (the seam,
//! ST3a §5.6 item 2; §4.2 concurrency rule 6).
//!
//! A driver runs its jobs one at a time, in the order V submitted them, and stops a job at a
//! latched failure. It is Core's `AbortNode` call site for a failed step: it hands the machine's
//! [`Abort`](super::machine::Abort) to the engine's abort sink, with Core's text. A failure that
//! another store latched is reported by that store's caller, so a job that meets it stops without
//! a second report. Every wait registers with the latch before it parks (E-b1.7).

use std::collections::VecDeque;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Waker;

use dash_chain::Notify;
use dashd_store::flatfile::FlatFs;
use dashd_store::{ErrorKind, KvEngine};

use super::clears::PruneClears;
use super::hub::{ParkerWake, SubscriberHub};
use super::machine::{
    CRASH_MESSAGE, CrashRoll, FlushConfig, FlushEnv, FlushMachine, FlushReport, FlushStep, Step,
    abort_text,
};
use super::observer::FlushObserver;
use super::packet::StateDelta;
use crate::store::{
    Failure, Fatal, GlobalFailStop, GuardedEngine, GuardedFiles, Parker, Registration,
};
use crate::tuning::EngineTuning;

/// What W reports to V. The node's adapter rings V's `FlushBell` for every event, [`Released`]
/// too (CH1-node D60): V's in-hold drain parks without a timeout, and a `FlushDue` V took while a
/// job was in flight is answered only at a later `answer_due` (D24), which a release must wake.
///
/// [`Released`]: DriverEvent::Released
#[derive(Debug, PartialEq, Eq)]
pub enum DriverEvent {
    /// Step 11, `FlushDone`: W no longer holds F.
    Released(FlushReport),
    /// Step 7: W waits for V's answer, the one wait on V that LO-9 allows W.
    PruneClears(PruneClears),
    /// A step failed, or W found the latch tripped: the job stopped, and the failure has reached
    /// the abort sink.
    Failed,
    /// `-dbcrashratio`'s roll came up: W writes nothing more.
    Crashed,
}

/// Runs W's flush machines for V.
pub trait FlushDriver {
    /// The evo delta its jobs carry.
    type Delta: StateDelta;

    /// Hands W a job, to run after those before it.
    fn submit(&mut self, job: FlushMachine<Self::Delta>);

    /// W's next event, if one is ready.
    fn poll(&mut self) -> Option<DriverEvent>;

    /// Waits for W's next event, registered with the latch (E-b1.7).
    ///
    /// # Errors
    ///
    /// [`Fatal`] once the latch has tripped with no event ready.
    fn wait(&mut self) -> Result<DriverEvent, Fatal>;

    /// [`wait`](Self::wait) at one of V's waits inside its flush (CH1-node D47 item 2): `pump`,
    /// V's pending fanout (SB5's writer rule), runs before each park. A driver with no thread to
    /// wait for ignores it.
    ///
    /// # Errors
    ///
    /// As [`wait`](Self::wait).
    fn wait_pumped(&mut self, pump: &mut dyn FnMut()) -> Result<DriverEvent, Fatal> {
        let _ = pump;
        self.wait()
    }
}

/// What one step of a job came to.
enum Ran {
    /// A step ran, and V hears of it if it released F or asks V for step 7's clears.
    Step(FlushStep, Option<DriverEvent>),
    /// The job waits for V's answer to its `PruneClears`: nothing ran.
    Waiting,
    /// The job has no step left.
    Ended,
    /// The job stopped at this step: it failed and reported, or crashed.
    Stopped(FlushStep, DriverEvent),
    /// The job stopped at a failure another store latched, which that store's caller reports: the
    /// step did not run, or its store call was refused.
    Refused,
}

/// The failure a crash latches: after the simulated `_Exit`, nothing more is written (ND1 exits
/// before anything could).
fn crashed() -> Failure {
    Failure {
        kind: ErrorKind::Other,
        site: "dbcrashratio",
        message: CRASH_MESSAGE.into(),
    }
}

/// Runs `job`'s next step, its failure reported before the other threads see it (BL-554, CH3b
/// D222): the step's trips are deferred until the driver's report, and a step's winning trip is
/// reported whatever the step comes to. A step that panics after it won reports the latched
/// failure's text before the unwind goes on. A failed step's registration window closes after
/// its report, so the report's pending section takes no hub lock.
fn run_step<E: KvEngine, F: FlatFs, T: EngineTuning, D: StateDelta>(
    job: &mut FlushMachine<D>,
    env: &mut FlushEnv<'_, E, F, T>,
) -> Ran {
    let Some(at) = job.next_step() else {
        return Ran::Ended;
    };
    let (engine, hub, flush) = (env.engine, env.hub, job.flush_id());
    if engine.latch().tripped().is_some() {
        // The job is dropped without its last step: its registration window closes here (D43).
        hub.end_flush(flush);
        return Ran::Refused;
    }
    let deferred = engine.latch().defer_reports();
    // Nothing reuses `job` or `env` after a panic: the fallback reads the engine's latch and sink.
    let step = match panic::catch_unwind(AssertUnwindSafe(|| job.step(env))) {
        Ok(Err(abort)) => {
            engine.abort_sink().abort_node(&abort.log, abort.user.as_deref());
            deferred.reported();
            hub.end_flush(flush);
            return Ran::Stopped(at, DriverEvent::Failed);
        }
        Ok(Ok(step)) => {
            // No step wins a trip and returns: report it all the same, then flag it.
            let owned = deferred.owns();
            if owned {
                report_latched(engine);
                deferred.reported();
            }
            debug_assert!(!owned, "a winning trip with no report");
            step
        }
        Err(unwind) => {
            if deferred.owns() {
                report_latched(engine);
                deferred.reported();
            }
            drop(deferred);
            panic::resume_unwind(unwind)
        }
    };
    drop(deferred);
    match step {
        Step::Progress(_) => Ran::Step(at, None),
        Step::Released(report) => Ran::Step(at, Some(DriverEvent::Released(report))),
        Step::PruneClears(req) => Ran::Step(at, Some(DriverEvent::PruneClears(req))),
        Step::Waiting => Ran::Waiting,
        Step::Done => Ran::Ended,
        Step::Refused => Ran::Refused,
        Step::Crash => {
            hub.end_flush(flush);
            Ran::Stopped(at, DriverEvent::Crashed)
        }
    }
}

/// The report of a failure this thread latched and its step did not report: the text V's stop
/// reads (CH3b D221).
fn report_latched<E: KvEngine>(engine: &GuardedEngine<E>) {
    let latched = engine.latch().tripped_now().expect("a winning trip latched").clone();
    engine.abort_sink().abort_node(&abort_text(latched), None);
}

/// The seam's driver: W's steps run on the caller's thread, one per [`step`](Self::step), and V's
/// waits run them until W reports.
pub struct InlineDriver<'a, E: KvEngine, F: FlatFs, T: EngineTuning, D: StateDelta> {
    pub env: FlushEnv<'a, E, F, T>,
    jobs: VecDeque<FlushMachine<D>>,
    events: VecDeque<DriverEvent>,
    crashed: bool,
    /// The seam's subscribers: run while W waits on them (steps 3b and 6); `true` if one made
    /// progress.
    pump: Option<Box<dyn FnMut() -> bool + 'a>>,
}

impl<'a, E: KvEngine, F: FlatFs, T: EngineTuning, D: StateDelta> InlineDriver<'a, E, F, T, D> {
    pub fn new(env: FlushEnv<'a, E, F, T>) -> Self {
        Self {
            env,
            jobs: VecDeque::new(),
            events: VecDeque::new(),
            crashed: false,
            pump: None,
        }
    }

    /// The seam's subscriber steps, which W's waits on subscribers run on this thread.
    pub fn set_pump(&mut self, pump: impl FnMut() -> bool + 'a) {
        self.pump = Some(Box::new(pump));
    }

    /// Runs W's next step: the step that ran, or `None` if W has nothing to run, waits for V's
    /// answer to its `PruneClears` (or on subscribers the pump cannot move), has crashed, or
    /// found the latch tripped (each job then stops, and V hears `Failed`).
    pub fn step(&mut self) -> Option<FlushStep> {
        while !self.crashed {
            let job = self.jobs.front_mut()?;
            match run_step(job, &mut self.env) {
                Ran::Step(step, event) => {
                    self.events.extend(event);
                    return Some(step);
                }
                Ran::Waiting => {
                    let pumped =
                        job.awaits_subscribers() && self.pump.as_mut().is_some_and(|pump| pump());
                    if !pumped {
                        return None;
                    }
                }
                Ran::Ended => {
                    self.jobs.pop_front();
                }
                Ran::Refused => {
                    self.jobs.pop_front();
                    self.events.push_back(DriverEvent::Failed);
                }
                Ran::Stopped(step, event) => {
                    self.crashed = event == DriverEvent::Crashed;
                    if self.crashed {
                        self.env.engine.latch().trip(crashed());
                    }
                    self.jobs.pop_front();
                    self.events.push_back(event);
                    return Some(step);
                }
            }
        }
        None
    }
}

impl<E: KvEngine, F: FlatFs, T: EngineTuning, D: StateDelta> FlushDriver
    for InlineDriver<'_, E, F, T, D>
{
    type Delta = D;

    fn submit(&mut self, job: FlushMachine<D>) {
        self.jobs.push_back(job);
    }

    fn poll(&mut self) -> Option<DriverEvent> {
        self.events.pop_front()
    }

    /// Runs W's steps until it reports. On one thread there is nothing to park for: the latch is
    /// checked before each step.
    fn wait(&mut self) -> Result<DriverEvent, Fatal> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(event);
            }
            self.env.engine.latch().check()?;
            if self.step().is_none() && self.events.is_empty() {
                self.env.engine.latch().check()?;
                panic!("V waits for W with nothing to run");
            }
        }
    }
}

/// What W's thread owns: [`FlushEnv`]'s stores, shared with V, and W's state across flushes.
pub struct WThread<E: KvEngine, F: FlatFs, T: EngineTuning> {
    pub engine: Arc<GuardedEngine<E>>,
    pub files: Arc<GuardedFiles<F>>,
    /// The filesystem under `files`: the space checks.
    pub fs: Arc<F>,
    pub hub: Arc<SubscriberHub>,
    pub obs: Arc<dyn FlushObserver>,
    pub tuning: Arc<T>,
    pub cfg: FlushConfig,
    /// The presumed-effect seq the last pre-B1 barrier covered: recovery's, at startup.
    pub covered_seq: u64,
    pub crash: CrashRoll,
    /// `-dbcrashratio`'s exit, run on W when the roll comes up: ND1's Core `_Exit(0)`. It returns
    /// only in tests, and W then stops.
    pub on_crash: Box<dyn FnOnce() + Send>,
}

/// The production driver: W's thread runs each job's steps back to back.
pub struct ThreadDriver<D: StateDelta> {
    shared: Arc<Shared<D>>,
    latch: GlobalFailStop,
    /// V's parker, registered for the driver's life.
    _registered: Registration,
    /// V's bell, rung at a trip too (D54): V parked on its own wake learns of a failure W cannot
    /// report from inside a store call.
    _bell_registered: Option<Registration>,
    /// V's own wake, which the bell rings ([`set_bell`](Self::set_bell)); without a bell, V
    /// parks on `shared.v`.
    wake: Option<Arc<dash_chain::Parker>>,
    /// SB5's writer rule (D23): V's pending fanout, run before each of V's parks for W.
    pump: Option<Box<dyn FnMut() + Send>>,
    #[expect(
        clippy::disallowed_types,
        reason = "V waits on W, which LO-9 allows: W waits only on its job queue and on V's answer to step 7's PruneClears, which V gives at every wait (CH1-node §7.1 H3, §8.3)"
    )]
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Shared<D: StateDelta> {
    queue: Mutex<Queue<D>>,
    /// W's wake: a job, the close, or the latch.
    w: Arc<Parker>,
    /// V's wake: an event, W's end, or the latch.
    v: Arc<Parker>,
}

struct Queue<D: StateDelta> {
    jobs: VecDeque<FlushMachine<D>>,
    events: VecDeque<DriverEvent>,
    closed: bool,
    /// W's thread has ended: closed, crashed or panicked.
    ended: bool,
    /// V's bell (CH1-node D45, D60): rung for every event W hands V, and at W's end.
    ring: Option<Ring>,
}

/// V's bell as W rings it.
type Ring = Arc<dyn Fn() + Send + Sync>;

impl<D: StateDelta> Shared<D> {
    /// The queue, also after a panic on W.
    fn lock(&self) -> MutexGuard<'_, Queue<D>> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// W's next job; `None` once the driver is closed and every job has run.
    fn next_job(&self) -> Option<FlushMachine<D>> {
        loop {
            {
                let mut q = self.lock();
                if let Some(job) = q.jobs.pop_front() {
                    return Some(job);
                }
                if q.closed {
                    return None;
                }
            }
            self.w.park();
        }
    }

    /// Hands V `event`; once V has dropped the driver, drops it: a dropped `PruneClears` ends W's
    /// wait for an answer that will not come, and W's flush stops. Every event rings V's bell,
    /// `Released` too (CH1-node D60).
    fn report(&self, event: DriverEvent) {
        let mut q = self.lock();
        if q.closed {
            return;
        }
        q.events.push_back(event);
        let ring = q.ring.clone();
        drop(q);
        self.wake_v(ring);
    }

    /// Wakes V where it waits for W: its parker, and its bell once set.
    fn wake_v(&self, ring: Option<Ring>) {
        self.v.notify();
        if let Some(ring) = ring {
            ring();
        }
    }
}

/// Marks W's end, however it ends, and wakes V. A panic on W is a failure like any other (M2): it
/// trips the latch and is reported, so V and the other stores stop.
struct Ended<'a, D: StateDelta, E: KvEngine> {
    shared: &'a Shared<D>,
    engine: &'a GuardedEngine<E>,
}

impl<D: StateDelta, E: KvEngine> Drop for Ended<'_, D, E> {
    /// A panic's report is made before V can see its failure (BL-554): its trip is deferred, and
    /// reported, before the queue says W has ended.
    fn drop(&mut self) {
        if std::thread::panicking() {
            let message = "the chainstate flush thread panicked";
            let deferred = self.engine.latch().defer_reports();
            let latched = self.engine.latch().trip(Failure {
                kind: ErrorKind::Other,
                site: "chainstate flush",
                message: message.into(),
            });
            if latched {
                self.engine.abort_sink().abort_node(message, None);
            }
            deferred.reported();
        }
        let ring = {
            let mut q = self.shared.lock();
            q.ended = true;
            q.ring.clone()
        };
        self.shared.wake_v(ring);
    }
}

impl<D: StateDelta> ThreadDriver<D> {
    /// Starts W's thread.
    ///
    /// # Errors
    ///
    /// The thread's spawn.
    pub fn spawn<E, F, T>(w: WThread<E, F, T>) -> io::Result<Self>
    where
        E: KvEngine + 'static,
        F: FlatFs + 'static,
        T: EngineTuning + 'static,
    {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                events: VecDeque::new(),
                closed: false,
                ended: false,
                ring: None,
            }),
            w: Parker::new(),
            v: Parker::new(),
        });
        let latch = w.engine.latch().clone();
        let registered = latch.register(&shared.v);
        let on_w = Arc::clone(&shared);
        #[expect(
            clippy::disallowed_methods,
            reason = "V waits on W, which LO-9 allows: W waits only on its job queue and on V's answer to step 7's PruneClears, which V gives at every wait (CH1-node §7.1 H3, §8.3)"
        )]
        let thread = std::thread::Builder::new()
            .name("dashd-flush".into())
            .spawn(move || w_loop(&on_w, w))?;
        Ok(Self {
            shared,
            latch,
            _registered: registered,
            _bell_registered: None,
            wake: None,
            pump: None,
            thread: Some(thread),
        })
    }

    /// SB5's writer rule (D23): every wait of V's for W runs `pump`, V's retry of its pending
    /// fanout, before it parks, so that a subscriber W waits on at step 6 gets the events toward
    /// `new` V has not emitted yet. Register [`v_wake`](Self::v_wake) as the producer's wake, so
    /// a queue's room ends the park.
    ///
    /// A driver with a [bell](Self::set_bell) takes no pump: V's fanout drains in its own waits.
    pub fn set_pump(&mut self, pump: impl FnMut() + Send + 'static) {
        debug_assert!(self.wake.is_none(), "a belled driver takes no pump");
        self.pump = Some(Box::new(pump));
    }

    /// V's bell (CH1-node D45, D60): W rings it for every event it hands V, `Released` included,
    /// and when its thread ends, and the latch rings it at a trip (D54), so that V's untimed waits
    /// answer W and see a failure. Set, V waits for W on `wake`, the wake the bell rings. That
    /// wake is V's other waits' too, SB5's producer wake among them: a wait that parked on it
    /// leaves it set when it returns, so a notify meant for the next waiter is not lost to it
    /// (b4 D81).
    ///
    /// A belled driver takes no [pump](Self::set_pump).
    pub fn set_bell(
        &mut self,
        ring: impl Fn() + Send + Sync + 'static,
        wake: Arc<dash_chain::Parker>,
    ) {
        debug_assert!(self.pump.is_none(), "a pumped driver takes no bell");
        let ring: Ring = Arc::new(ring);
        self._bell_registered = Some(self.latch.register_fn(Arc::clone(&ring)));
        self.wake = Some(wake);
        self.shared.lock().ring = Some(ring);
    }

    /// V's wait for W's next event: W's end and the latch checked, then `pump` and SB5's pump,
    /// before each park. Having parked on V's own wake, it leaves the wake set: the permit it took
    /// may have been another waiter's.
    fn wait_for_event(&mut self, pump: &mut dyn FnMut()) -> Result<DriverEvent, Fatal> {
        let mut parked = false;
        let event = self.next_event(pump, &mut parked);
        if parked && let Some(wake) = &self.wake {
            wake.notify();
        }
        event
    }

    fn next_event(
        &mut self,
        pump: &mut dyn FnMut(),
        parked: &mut bool,
    ) -> Result<DriverEvent, Fatal> {
        loop {
            {
                let mut q = self.shared.lock();
                if let Some(event) = q.events.pop_front() {
                    return Ok(event);
                }
                if q.ended {
                    // A panic on W has latched its failure.
                    self.latch.check()?;
                    panic!("V waits for W, whose thread has ended");
                }
            }
            self.latch.check()?;
            pump();
            if let Some(pump) = &mut self.pump {
                pump();
            }
            if !self.shared.lock().events.is_empty() {
                continue;
            }
            match &self.wake {
                Some(wake) => {
                    *parked = true;
                    wake.park();
                }
                None => self.shared.v.park(),
            }
        }
    }

    /// V's wake while it waits for W.
    pub fn v_wake(&self) -> Notify {
        Notify::Task(Waker::from(Arc::new(ParkerWake(Arc::clone(&self.shared.v)))))
    }
}

fn w_loop<E: KvEngine, F: FlatFs, T: EngineTuning, D: StateDelta>(
    shared: &Shared<D>,
    w: WThread<E, F, T>,
) {
    let WThread {
        engine,
        files,
        fs,
        hub,
        obs,
        tuning,
        cfg,
        covered_seq,
        crash,
        on_crash,
    } = w;
    let _ended = Ended {
        shared,
        engine: &engine,
    };
    let _registered = engine.latch().register(&shared.w);
    let mut env = FlushEnv {
        engine: &engine,
        files: &files,
        fs: &fs,
        hub: &hub,
        obs: &*obs,
        tuning: &*tuning,
        cfg: &cfg,
        covered_seq,
        crash,
    };
    let mut on_crash = Some(on_crash);
    while let Some(mut job) = shared.next_job() {
        loop {
            match run_step(&mut job, &mut env) {
                Ran::Step(_, event) => event.into_iter().for_each(|e| shared.report(e)),
                // A tripped latch ends the park, and the next step finds it.
                Ran::Waiting => _ = job.park(engine.latch()),
                Ran::Ended => break,
                Ran::Stopped(_, DriverEvent::Crashed) => {
                    // The exit first: V, woken by the latch, must not stop the node before it.
                    on_crash.take().expect("W crashes once")();
                    engine.latch().trip(crashed());
                    shared.report(DriverEvent::Crashed);
                    return;
                }
                Ran::Stopped(_, event) => {
                    shared.report(event);
                    break;
                }
                Ran::Refused => {
                    shared.report(DriverEvent::Failed);
                    break;
                }
            }
        }
    }
}

impl<D: StateDelta> FlushDriver for ThreadDriver<D> {
    type Delta = D;

    fn submit(&mut self, job: FlushMachine<D>) {
        self.shared.lock().jobs.push_back(job);
        self.shared.w.notify();
    }

    fn poll(&mut self) -> Option<DriverEvent> {
        self.shared.lock().events.pop_front()
    }

    fn wait(&mut self) -> Result<DriverEvent, Fatal> {
        self.wait_for_event(&mut || {})
    }

    /// With a bell set, V parks on its own wake: W's ring ends the park for each event and at
    /// W's end, and the latch's at a trip (E-b1.7).
    fn wait_pumped(&mut self, pump: &mut dyn FnMut()) -> Result<DriverEvent, Fatal> {
        self.wait_for_event(pump)
    }
}

impl<D: StateDelta> Drop for ThreadDriver<D> {
    /// W runs the jobs it was given, then its thread ends. V answers nothing more: its unread
    /// events are dropped, and with them a `PruneClears` W waits on or is about to send, so a
    /// prune in flight stops its flush with `AbortNode`. V's orderly stop waits for the release
    /// first, as Core's shutdown flush does.
    #[expect(
        clippy::disallowed_methods,
        reason = "V waits for W's end, which LO-9 allows: W waits only on its job queue and on V's answer to step 7's PruneClears, which this drop gives as the request's drop"
    )]
    fn drop(&mut self) {
        let unread = {
            let mut q = self.shared.lock();
            q.closed = true;
            std::mem::take(&mut q.events)
        };
        drop(unread);
        self.shared.w.notify();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
            && !std::thread::panicking()
        {
            panic!("W's flush thread panicked");
        }
    }
}
