//! The fail-stop latch and the host's abort sink (CH2 §2.4.4, §2.10; SB0 §6.3 P6).
//!
//! One [`GlobalFailStop`] is shared by every store of a node (one per process) or of an SPV
//! client (one per client); a test world gets its own. The first failure trips it, and from then
//! on every write, barrier or flat-file mutation through a guard is refused (ST3a R8 across
//! stores). Fatal failures go to the host's [`AbortSink`].
//!
//! Every wait that must end on a trip registers a [`Parker`] with the latch before it parks, and a
//! trip wakes them all (E-b1.7). A wait that parks on another kind of handle registers a wake
//! function instead ([`GlobalFailStop::register_fn`]).
//!
//! A thread that reports its own failures later than it trips the latch defers them
//! ([`GlobalFailStop::defer_reports`], BL-554): the node's flush thread, W, whose guard trips at
//! the failing store call and whose driver reports afterwards. Until that report the failure is
//! pending, and the other threads' reads of the latch wait for it, so nothing they log or answer
//! comes before the report, as Core's synchronous flush reports before anything sees the failure.
//! A latch with no deferrer, an SPV client's, is never pending.

use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use crate::ErrorKind;

/// The failure a [`GlobalFailStop`] holds: the first one to trip it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: ErrorKind,
    /// Where it happened: `"<database> read"`, `"<database> write"`, `"<database> barrier"` or
    /// `"block files write"`.
    pub site: &'static str,
    pub message: String,
}

/// A latched failure, seen by an operation that cannot go on ([`GlobalFailStop::check`]). Whoever
/// tripped the latch has reported it already (a deferred report has been made, BL-554), so its
/// holder stops without a second `AbortNode`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", .0.message)]
pub struct Fatal(pub Failure);

/// A wait's wake handle (E-b1.7): a condvar permit, shaped like SB5's `Parker`. A `notify` before
/// the `park` is kept, so no wake is lost; one waiter parks on it.
#[derive(Debug, Default)]
pub struct Parker {
    permit: Mutex<bool>,
    cv: Condvar,
}

impl Parker {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// The permit, also after a panic elsewhere: a trip from an unwinding thread still wakes.
    fn permit(&self) -> MutexGuard<'_, bool> {
        self.permit.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for the permit, and takes it.
    pub fn park(&self) {
        let permit = self.permit();
        let mut permit =
            self.cv.wait_while(permit, |p| !*p).unwrap_or_else(PoisonError::into_inner);
        *permit = false;
    }

    /// Waits at most `timeout` for the permit; whether it came (and was taken).
    pub fn park_timeout(&self, timeout: Duration) -> bool {
        let permit = self.permit();
        let (mut permit, _) = self
            .cv
            .wait_timeout_while(permit, timeout, |p| !*p)
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *permit)
    }

    pub fn notify(&self) {
        *self.permit() = true;
        self.cv.notify_one();
    }
}

/// The fail-stop latch: one per node (in production, one per process) or per SPV client, shared by
/// every engine and the block files; a test world gets its own. Once tripped it stays tripped, and
/// every later write, barrier or flat-file mutation is refused (ST3a R8 across stores).
///
/// **Deferred reports (BL-554).** The latch's one deferrer ([`defer_reports`]) trips it ahead of
/// its report: a winning trip of the deferrer's is pending until [`DeferScope::reported`]. The
/// waiting reads ([`tripped`], [`check`], [`settle`], [`wait_tripped`]) return a pending failure
/// only once it is reported; the deferrer never waits on itself. [`tripped_now`] and [`trip`]
/// never wait, so a caller that learns of a failure it did not latch through either, and then
/// reports, logs or replies, [`settle`]s first: a losing [`trip`] is followed by a `settle`
/// before its caller's `AbortNode`.
///
/// The deferrer's pending section takes only report locks: the abort sink's, the logger's and
/// their callbacks', the observers' and the latch's own wakes. Those are leaves, which nothing
/// holds while it waits on the latch, so a reader may wait holding any other lock.
///
/// [`defer_reports`]: Self::defer_reports
/// [`tripped`]: Self::tripped
/// [`check`]: Self::check
/// [`settle`]: Self::settle
/// [`wait_tripped`]: Self::wait_tripped
/// [`tripped_now`]: Self::tripped_now
/// [`trip`]: Self::trip
#[derive(Clone, Debug, Default)]
pub struct GlobalFailStop(Arc<Latch>);

#[derive(Debug, Default)]
struct Latch {
    failure: OnceLock<Failure>,
    /// The registered waits' handles, one entry per registration.
    wakers: Mutex<Vec<Wake>>,
    /// `reports.pending.is_some()`, set before the failure is published and cleared at its
    /// report: an untripped or settled read takes no lock.
    pending: AtomicBool,
    /// The deferrer and its pending report. Every trip takes it, so a failure is published with
    /// its pending state.
    reports: Mutex<Reports>,
    /// Notified when a pending report has been made.
    reported: Condvar,
}

/// A latch's deferred-report state (BL-554).
#[derive(Debug, Default)]
struct Reports {
    /// The thread of the live [`DeferScope`].
    deferrer: Option<ThreadId>,
    /// The deferrer, from its winning trip to its report.
    pending: Option<ThreadId>,
    /// The threads waiting for that report, one entry per wait.
    waiters: Vec<ThreadId>,
}

impl Latch {
    /// The registry, also after a panic elsewhere (a trip from an unwinding thread, M2).
    fn wakers(&self) -> MutexGuard<'_, Vec<Wake>> {
        self.wakers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The deferred-report state, also after a panic elsewhere.
    fn reports(&self) -> MutexGuard<'_, Reports> {
        self.reports.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until no other thread's report is pending, or until `deadline`; whether none is.
    fn wait_reported(&self, deadline: Option<Instant>) -> bool {
        if !self.pending.load(Ordering::Acquire) {
            return true;
        }
        let me = thread::current().id();
        let mut reports = self.reports();
        if reports.pending.is_none_or(|owner| owner == me) {
            return true;
        }
        reports.waiters.push(me);
        let settled = loop {
            if reports.pending.is_none() {
                break true;
            }
            reports = match deadline {
                None => self.reported.wait(reports).unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break false;
                    }
                    self.reported
                        .wait_timeout(reports, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        };
        if let Some(at) = reports.waiters.iter().position(|&w| w == me) {
            reports.waiters.swap_remove(at);
        }
        settled
    }

    /// Ends `me`'s pending report, if it has one, and wakes its waiters.
    fn end_pending(&self, me: ThreadId) {
        let mut reports = self.reports();
        if reports.pending == Some(me) {
            reports.pending = None;
            self.pending.store(false, Ordering::Release);
            self.reported.notify_all();
        }
    }
}

/// A registered wait's handle.
#[derive(Clone)]
enum Wake {
    Parker(Arc<Parker>),
    /// Called under the registry's lock: it must not block, or touch the latch.
    Fn(Arc<dyn Fn() + Send + Sync>),
}

impl Wake {
    fn wake(&self) {
        match self {
            Self::Parker(p) => p.notify(),
            Self::Fn(f) => f(),
        }
    }

    fn same(&self, other: &Wake) -> bool {
        match (self, other) {
            (Self::Parker(a), Self::Parker(b)) => Arc::ptr_eq(a, b),
            (Self::Fn(a), Self::Fn(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl fmt::Debug for Wake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parker(p) => f.debug_tuple("Parker").field(p).finish(),
            Self::Fn(_) => f.write_str("Fn"),
        }
    }
}

impl GlobalFailStop {
    pub fn new() -> Self {
        Self::default()
    }

    /// Latches `f` unless a failure is latched already, then wakes every registered wait; whether
    /// this call latched it. The failure is stored before the wakes, so a wait that registered,
    /// then found no failure, is woken. It never waits. On the deferrer's thread a winning trip is
    /// pending until its report, and the pending state is set before the failure is published; a
    /// losing trip changes nothing. A caller whose trip lost, and that then reports, logs or
    /// replies, [`settle`](Self::settle)s first.
    pub fn trip(&self, f: Failure) -> bool {
        let latched = {
            let mut reports = self.0.reports();
            if self.0.failure.get().is_some() {
                false
            } else {
                let me = thread::current().id();
                if reports.deferrer == Some(me) {
                    reports.pending = Some(me);
                    self.0.pending.store(true, Ordering::Release);
                }
                // Every trip sets it under `reports`, so this one does.
                self.0.failure.set(f).is_ok()
            }
        };
        if latched {
            self.0.wakers().iter().for_each(Wake::wake);
        }
        latched
    }

    /// The latched failure, once reported: a failure the deferrer has latched but not reported
    /// yet is waited for, unless this is the deferrer's thread (BL-554).
    pub fn tripped(&self) -> Option<&Failure> {
        let f = self.0.failure.get()?;
        self.0.wait_reported(None);
        Some(f)
    }

    /// The latched failure, reported or not: for a caller that must not wait where it reads it
    /// (it holds a lock the deferrer's report may need), and [`settle`](Self::settle)s before it
    /// reports, logs or replies.
    pub fn tripped_now(&self) -> Option<&Failure> {
        self.0.failure.get()
    }

    /// Waits until a latched failure is reported (BL-554); at once if none is latched, if its
    /// report has been made or if this is the deferrer's thread.
    pub fn settle(&self) {
        if self.0.failure.get().is_some() {
            self.0.wait_reported(None);
        }
    }

    /// The latched failure as an error, once reported ([`tripped`](Self::tripped)).
    ///
    /// # Errors
    ///
    /// [`Fatal`] once the latch has tripped.
    pub fn check(&self) -> Result<(), Fatal> {
        self.tripped().map_or(Ok(()), |f| Err(Fatal(f.clone())))
    }

    /// Makes this thread the latch's deferrer while the scope lives (BL-554): a trip of its that
    /// wins is pending until [`DeferScope::reported`], and the other threads' waiting reads wait
    /// for that report. One deferrer per latch: a second live scope is a bug.
    ///
    /// The pending section, from the winning trip to `reported`, takes only report locks, which
    /// are leaves and which nothing holds while it waits on the latch (CH3b D222).
    pub fn defer_reports(&self) -> DeferScope {
        let me = thread::current().id();
        let mut reports = self.0.reports();
        debug_assert!(reports.deferrer.is_none(), "a latch has one deferrer");
        reports.deferrer = Some(me);
        DeferScope {
            latch: self.clone(),
            thread: me,
            _on_its_thread: PhantomData,
        }
    }

    /// The threads waiting for the deferrer's pending report; empty unless one is pending. A test's
    /// checkpoint: its reader is parked while the report is still to come.
    pub fn pending_waiters(&self) -> Vec<ThreadId> {
        let reports = self.0.reports();
        if reports.pending.is_some() {
            reports.waiters.clone()
        } else {
            Vec::new()
        }
    }

    /// Registers `parker`, so a trip notifies it, until the registration is dropped. A wait
    /// registers, then checks the latch and its own predicate, and parks only if neither holds.
    pub fn register(&self, parker: &Arc<Parker>) -> Registration {
        self.register_wake(Wake::Parker(Arc::clone(parker)))
    }

    /// Registers `wake`, called once at a trip, until the registration is dropped: for a wait
    /// that parks on a handle of its own, which `wake` notifies. It runs on the tripping thread,
    /// under the registry's lock, so it must not block or touch the latch. A registration made
    /// once the failure is stored may not be called (it is not after `trip` has returned): its
    /// wait sees the failure in the latch check it makes after registering.
    pub fn register_fn(&self, wake: Arc<dyn Fn() + Send + Sync>) -> Registration {
        self.register_wake(Wake::Fn(wake))
    }

    fn register_wake(&self, wake: Wake) -> Registration {
        self.0.wakers().push(wake.clone());
        Registration {
            latch: self.clone(),
            wake,
        }
    }

    /// Parks until a failure is latched and reported, or until `timeout` has passed; whether one
    /// is. A pending report is waited for within the same `timeout`: `false` may mean a failure
    /// is latched whose report has not been made.
    pub fn wait_tripped(&self, timeout: Duration) -> bool {
        let parker = Parker::new();
        let _registered = self.register(&parker);
        let deadline = Instant::now() + timeout;
        loop {
            if self.tripped_now().is_some() {
                return self.0.wait_reported(Some(deadline));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            parker.park_timeout(left);
        }
    }
}

/// A thread's deferral of its reports ([`GlobalFailStop::defer_reports`]), until dropped. It stays
/// on its thread.
#[must_use = "dropping the scope ends the deferral"]
#[derive(Debug)]
pub struct DeferScope {
    latch: GlobalFailStop,
    thread: ThreadId,
    _on_its_thread: PhantomData<*const ()>,
}

impl DeferScope {
    /// This scope's trip won and its report has not been made.
    pub fn owns(&self) -> bool {
        self.latch.0.reports().pending == Some(self.thread)
    }

    /// The report of this scope's trip has been made: the waiting reads return. Its owner calls it
    /// right after its abort sink returns.
    pub fn reported(&self) {
        self.latch.0.end_pending(self.thread);
    }
}

impl Drop for DeferScope {
    /// Ends the deferral. A pending report still owned here was not made (its sink panicked):
    /// the waiting reads return all the same, so that no reader is left parked.
    fn drop(&mut self) {
        self.latch.0.end_pending(self.thread);
        let mut reports = self.latch.0.reports();
        if reports.deferrer == Some(self.thread) {
            reports.deferrer = None;
        }
    }
}

/// A [`Parker`]'s registration with a [`GlobalFailStop`], or a wake function's: dropping it
/// unregisters it.
#[must_use = "dropping the registration unregisters the parker"]
#[derive(Debug)]
pub struct Registration {
    latch: GlobalFailStop,
    wake: Wake,
}

impl Drop for Registration {
    /// Removes one entry of its parker: a parker registered twice stays registered once.
    fn drop(&mut self) {
        let mut wakers = self.latch.0.wakers();
        if let Some(at) = wakers.iter().position(|w| w.same(&self.wake)) {
            wakers.swap_remove(at);
        }
    }
}

/// Where fatal storage failures go, implemented by the host (SB0 §7, L5): the node's `AbortNode`
/// and Core's coins error catcher (ND1), or an SPV client's stop and its FFI error event. The
/// one trait for both CH2's store and CH1-node's V (BL-109).
pub trait AbortSink: Send + Sync {
    /// Core's `AbortNode`: log `log_msg`, show `user_msg`, shut down with exit status 1 (v24).
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>);

    /// A store read failed and the latch has tripped. The node logs Core's two strings ("Error
    /// reading from database: %s", and "Error reading from database, shutting down." to the user,
    /// `init.cpp:2089`) and calls `std::process::abort()`, with no shutdown flush. A test sink
    /// records it and returns, and the read's error carries the failure out of the operation.
    fn read_failed(&self, what: &str);
}

/// What a [`RecordingSink`] was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortMark {
    AbortNode {
        log: String,
        user: Option<String>,
    },
    ReadFailed(String),
}

/// An [`AbortSink`] that records its calls and returns: the seam's and the tests' sink.
#[derive(Debug, Default)]
pub struct RecordingSink {
    marks: Mutex<Vec<AbortMark>>,
}

impl RecordingSink {
    pub fn marks(&self) -> Vec<AbortMark> {
        self.marks.lock().expect("unpoisoned").clone()
    }
}

impl AbortSink for RecordingSink {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        self.marks.lock().expect("unpoisoned").push(AbortMark::AbortNode {
            log: log_msg.into(),
            user: user_msg.map(Into::into),
        });
    }

    fn read_failed(&self, what: &str) {
        self.marks.lock().expect("unpoisoned").push(AbortMark::ReadFailed(what.into()));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn failure(message: &str) -> Failure {
        Failure {
            kind: ErrorKind::Io,
            site: "test",
            message: message.into(),
        }
    }

    /// E-b1.7: a permit given before the park is kept, and taken once.
    #[test]
    fn a_parkers_permit_is_never_lost() {
        let p = Parker::new();
        assert!(!p.park_timeout(Duration::ZERO));
        p.notify();
        p.park();
        p.notify();
        assert!(p.park_timeout(Duration::ZERO));
        assert!(!p.park_timeout(Duration::from_millis(1)));
    }

    /// E-b1.7: `trip` stores the failure, then wakes every registered wait, so a wait that
    /// registered before it checked is woken whenever the trip comes; `check` and `wait_tripped`
    /// see the first failure, and a second trip changes nothing.
    #[test]
    fn a_trip_wakes_every_registered_wait() {
        let latch = GlobalFailStop::new();
        assert_eq!(latch.check(), Ok(()));
        assert!(!latch.wait_tripped(Duration::from_millis(5)));
        std::thread::scope(|scope| {
            let waits: Vec<_> = (0..4)
                .map(|i| {
                    let latch = &latch;
                    scope.spawn(move || {
                        if i % 2 == 0 {
                            return latch.wait_tripped(Duration::from_secs(60));
                        }
                        let p = Parker::new();
                        let _registered = latch.register(&p);
                        while latch.tripped().is_none() {
                            assert!(p.park_timeout(Duration::from_secs(60)), "a lost wake");
                        }
                        true
                    })
                })
                .collect();
            // Every wait has registered (each before it checks), so the trip must reach all four.
            while latch.0.wakers().len() < 4 {
                std::thread::yield_now();
            }
            assert!(latch.trip(failure("first")));
            assert!(waits.into_iter().all(|w| w.join().unwrap()));
        });
        assert!(!latch.trip(failure("second")));
        assert_eq!(latch.check(), Err(Fatal(failure("first"))));
        assert_eq!(latch.check().unwrap_err().to_string(), "first");
        assert!(latch.wait_tripped(Duration::ZERO));
        assert!(latch.0.wakers().is_empty(), "every registration was dropped");
    }

    /// A dropped registration is not woken; a parker registered twice stays registered once.
    #[test]
    fn a_dropped_registration_is_not_woken() {
        let latch = GlobalFailStop::new();
        let (kept, dropped) = (Parker::new(), Parker::new());
        let _kept = latch.register(&kept);
        drop(latch.register(&kept));
        drop(latch.register(&dropped));
        latch.trip(failure("x"));
        assert!(kept.park_timeout(Duration::ZERO));
        assert!(!dropped.park_timeout(Duration::ZERO));
    }

    /// A registered wake function is called once, at the trip that latches; a dropped one is
    /// not, and dropping one function's registration keeps a parker's.
    #[test]
    fn a_registered_function_is_called_at_the_trip() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let latch = GlobalFailStop::new();
        let (kept, dropped) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let count = |n: &Arc<AtomicUsize>| -> Arc<dyn Fn() + Send + Sync> {
            let n = Arc::clone(n);
            Arc::new(move || _ = n.fetch_add(1, Ordering::SeqCst))
        };
        let parker = Parker::new();
        let _parker = latch.register(&parker);
        let _kept = latch.register_fn(count(&kept));
        drop(latch.register_fn(count(&dropped)));
        assert_eq!(latch.0.wakers().len(), 2);
        assert!(latch.trip(failure("x")));
        assert!(!latch.trip(failure("y")));
        assert_eq!(kept.load(Ordering::SeqCst), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert!(parker.park_timeout(Duration::ZERO));
    }

    fn counter(n: &Arc<std::sync::atomic::AtomicUsize>) -> Arc<dyn Fn() + Send + Sync> {
        let n = Arc::clone(n);
        Arc::new(move || _ = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }

    /// Sol r1 N1: a function registered after the trip has returned is not called, and the
    /// latch check its wait makes after registering sees the failure.
    #[test]
    fn a_function_registered_after_the_trip_is_not_called() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let latch = GlobalFailStop::new();
        assert!(latch.trip(failure("x")));
        let late = Arc::new(AtomicUsize::new(0));
        let registered = latch.register_fn(counter(&late));
        assert_eq!(latch.check(), Err(Fatal(failure("x"))));
        assert!(!latch.trip(failure("y")));
        assert_eq!(late.load(Ordering::SeqCst), 0);
        drop(registered);
        assert!(latch.0.wakers().is_empty());
    }

    /// Sol r1 N1: one function registered twice is called once per registration, and dropping
    /// one registration leaves the other.
    #[test]
    fn a_function_registered_twice_is_called_per_registration() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let n = Arc::new(AtomicUsize::new(0));
        let f = counter(&n);
        let latch = GlobalFailStop::new();
        let _a = latch.register_fn(Arc::clone(&f));
        let _b = latch.register_fn(Arc::clone(&f));
        assert!(latch.trip(failure("x")));
        assert_eq!(n.load(Ordering::SeqCst), 2);

        let latch = GlobalFailStop::new();
        let _a = latch.register_fn(Arc::clone(&f));
        drop(latch.register_fn(Arc::clone(&f)));
        assert_eq!(latch.0.wakers().len(), 1);
        n.store(0, Ordering::SeqCst);
        assert!(latch.trip(failure("x")));
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    /// How long a held-scope test waits for what must happen at once.
    const PROMPT: Duration = Duration::from_secs(10);

    /// Polls `done` until it holds or [`PROMPT`] has passed; whether it held.
    fn until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + PROMPT;
        while !done() {
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }

    /// The deferrer's side of a held-scope test: on its own thread, a scope and a winning trip,
    /// then "entered", then its report once `hold` is released (dropped by the test, before any
    /// join). `reported`, if given, is set just before the report is marked made.
    fn held_deferrer<'s>(
        scope: &'s std::thread::Scope<'s, '_>,
        latch: &'s GlobalFailStop,
        hold: mpsc::Receiver<()>,
        reported: Option<&'s AtomicBool>,
    ) -> (mpsc::Receiver<()>, std::thread::ScopedJoinHandle<'s, ()>) {
        let (entered, on_entered) = mpsc::channel();
        let w = scope.spawn(move || {
            let deferred = latch.defer_reports();
            assert!(latch.trip(failure("w")));
            assert!(deferred.owns());
            entered.send(()).unwrap();
            let _ = hold.recv();
            if let Some(reported) = reported {
                reported.store(true, Ordering::SeqCst);
            }
            deferred.reported();
            assert!(!deferred.owns());
        });
        (on_entered, w)
    }

    /// BL-554: a deferrer's winning trip is pending until its report. The other threads'
    /// `tripped`, `check` and `settle` wait for it (each is listed as a pending waiter while
    /// it is), and return only after it; then no waiter is listed.
    #[test]
    fn a_deferred_trip_holds_the_waiting_reads_until_its_report() {
        let latch = GlobalFailStop::new();
        let reported = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (release, hold) = mpsc::channel::<()>();
            let (on_entered, w) = held_deferrer(scope, &latch, hold, Some(&reported));
            on_entered.recv_timeout(PROMPT).expect("W tripped");
            let (latch, reported) = (&latch, &reported);
            let readers: Vec<_> = (0..3)
                .map(|i| {
                    scope.spawn(move || {
                        match i {
                            0 => assert_eq!(latch.tripped().map(|f| &*f.message), Some("w")),
                            1 => assert!(latch.check().is_err()),
                            _ => latch.settle(),
                        }
                        reported.load(Ordering::SeqCst)
                    })
                })
                .collect();
            let ids: Vec<_> = readers.iter().map(|r| r.thread().id()).collect();
            let parked = until(|| {
                let waiters = latch.pending_waiters();
                ids.iter().all(|id| waiters.contains(id))
            });
            assert!(parked, "every reader waits for the report: {:?}", latch.pending_waiters());
            assert!(
                readers.iter().all(|r| !r.is_finished()),
                "a reader returned before the report"
            );
            drop(release);
            assert!(readers.into_iter().all(|r| r.join().unwrap()), "a read before the report");
            w.join().unwrap();
            assert!(latch.pending_waiters().is_empty());
        });
        assert_eq!(latch.check(), Err(Fatal(failure("w"))));
    }

    /// BL-554: the deferrer reads its own pending failure without waiting.
    #[test]
    fn the_deferrer_does_not_wait_on_itself() {
        let latch = GlobalFailStop::new();
        let deferred = latch.defer_reports();
        assert!(latch.trip(failure("w")));
        assert!(deferred.owns());
        assert_eq!(latch.tripped(), Some(&failure("w")));
        assert_eq!(latch.check(), Err(Fatal(failure("w"))));
        latch.settle();
        assert!(latch.wait_tripped(Duration::ZERO));
        assert!(latch.pending_waiters().is_empty());
        deferred.reported();
        assert!(!deferred.owns());
    }

    /// BL-554: while a report is pending, another thread's losing `trip` and its `tripped_now`
    /// return at once, and the loser leaves the pending state as it was.
    #[test]
    fn a_losing_trip_and_tripped_now_do_not_wait() {
        let latch = GlobalFailStop::new();
        std::thread::scope(|scope| {
            let (release, hold) = mpsc::channel::<()>();
            let (on_entered, w) = held_deferrer(scope, &latch, hold, None);
            on_entered.recv_timeout(PROMPT).expect("W tripped");
            let latch = &latch;
            let (done, on_done) = mpsc::channel();
            scope.spawn(move || {
                let lost = !latch.trip(failure("v"));
                let now = latch.tripped_now().cloned();
                done.send((lost, now)).unwrap();
            });
            let got = on_done.recv_timeout(PROMPT);
            let pending = latch.0.pending.load(Ordering::SeqCst);
            drop(release);
            assert_eq!(got, Ok((true, Some(failure("w")))));
            assert!(pending, "the loser left the report pending");
            w.join().unwrap();
        });
    }

    /// BL-554: a latch with no deferrer, an SPV client's, is never pending.
    #[test]
    fn a_latch_with_no_deferrer_is_never_pending() {
        let latch = GlobalFailStop::new();
        std::thread::scope(|scope| {
            scope.spawn(|| assert!(latch.trip(failure("x")))).join().unwrap();
        });
        assert!(!latch.0.pending.load(Ordering::SeqCst));
        assert!(latch.pending_waiters().is_empty());
        assert_eq!(latch.tripped(), Some(&failure("x")));
        assert!(latch.wait_tripped(Duration::ZERO));
    }

    /// BL-554: a scope dropped with its report still pending (its sink panicked) ends it: the
    /// waiting reads return.
    #[test]
    fn a_scope_dropped_while_pending_releases_the_readers() {
        let latch = GlobalFailStop::new();
        std::thread::scope(|scope| {
            let (go, on_go) = mpsc::channel::<()>();
            let (entered, on_entered) = mpsc::channel();
            let latch = &latch;
            let w = scope.spawn(move || {
                let deferred = latch.defer_reports();
                assert!(latch.trip(failure("w")));
                entered.send(()).unwrap();
                let _ = on_go.recv();
                drop(deferred);
            });
            on_entered.recv_timeout(PROMPT).expect("W tripped");
            let reader = scope.spawn(move || latch.settle());
            let id = reader.thread().id();
            let parked = until(|| latch.pending_waiters().contains(&id));
            drop(go);
            assert!(parked, "the reader waits for the report");
            w.join().unwrap();
            reader.join().unwrap();
            assert!(latch.pending_waiters().is_empty());
        });
        assert!(!latch.0.pending.load(Ordering::SeqCst));
        assert!(latch.0.reports().deferrer.is_none(), "the deferral ended with its scope");
    }

    /// BL-554 (Sol r2 m-2): `wait_tripped` waits for a pending report within its timeout, a zero
    /// one or a finite one, and is false at the deadline; once reported, it is true.
    #[test]
    fn wait_tripped_keeps_its_deadline_while_a_report_is_pending() {
        let latch = GlobalFailStop::new();
        std::thread::scope(|scope| {
            let (release, hold) = mpsc::channel::<()>();
            let (on_entered, w) = held_deferrer(scope, &latch, hold, None);
            on_entered.recv_timeout(PROMPT).expect("W tripped");
            let latch = &latch;
            let (done, on_done) = mpsc::channel();
            scope.spawn(move || {
                let zero = latch.wait_tripped(Duration::ZERO);
                let finite = latch.wait_tripped(Duration::from_millis(50));
                done.send((zero, finite)).unwrap();
            });
            let got = on_done.recv_timeout(PROMPT);
            drop(release);
            assert_eq!(got, Ok((false, false)), "the deadline holds while the report is pending");
            w.join().unwrap();
            assert!(latch.wait_tripped(Duration::ZERO));
        });
    }

    /// BL-554: one deferrer per latch.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "a latch has one deferrer")]
    fn a_second_live_scope_is_a_bug() {
        let latch = GlobalFailStop::new();
        let _first = latch.defer_reports();
        std::thread::scope(|scope| {
            let second = scope.spawn(|| drop(latch.defer_reports())).join();
            std::panic::resume_unwind(second.unwrap_err());
        });
    }

    /// Sol r1 N1: registrations and drops racing a trip. Each wait registers a function that
    /// notifies its own parker, then parks until the latch holds a failure: none is left parked
    /// (no lost wake), and every registration is gone after. A registration that sees the failure
    /// stored may still be called, by the trip's wakes; one made after `trip` returned is not.
    #[test]
    fn registrations_racing_a_trip_lose_no_wake() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for round in 0..200_usize {
            let latch = GlobalFailStop::new();
            std::thread::scope(|scope| {
                let latch = &latch;
                for _ in 0..3 {
                    scope.spawn(move || {
                        let n = Arc::new(AtomicUsize::new(0));
                        while latch.tripped().is_none() {
                            drop(latch.register_fn(counter(&n)));
                        }
                        drop(latch.register_fn(counter(&n)));
                    });
                }
                let waits: Vec<_> = (0..4)
                    .map(|_| {
                        scope.spawn(move || {
                            let parker = Parker::new();
                            let wake: Arc<dyn Fn() + Send + Sync> = {
                                let parker = Arc::clone(&parker);
                                Arc::new(move || parker.notify())
                            };
                            let _registered = latch.register_fn(wake);
                            while latch.tripped().is_none() {
                                assert!(
                                    parker.park_timeout(Duration::from_secs(60)),
                                    "a lost wake"
                                );
                            }
                        })
                    })
                    .collect();
                for _ in 0..round % 50 {
                    std::thread::yield_now();
                }
                assert!(latch.trip(failure("x")));
                for w in waits {
                    w.join().unwrap();
                }
                // Once `trip` has returned, a registration is not called.
                let late = Arc::new(AtomicUsize::new(0));
                drop(latch.register_fn(counter(&late)));
                assert_eq!(late.load(Ordering::SeqCst), 0);
            });
            assert!(latch.0.wakers().is_empty(), "round {round}: a registration outlived its drop");
        }
    }
}
