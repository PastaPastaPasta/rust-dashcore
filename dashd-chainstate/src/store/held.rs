//! BL-554's held sink: a thread's `AbortNode` held at the sink's door, so a test can look at the
//! other threads while that report is still to come.

// Test gates and waits use raw channels; LO-9's ban is for the crate's own wait paths.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use dash_kv::{AbortSink, Failure, GlobalFailStop};

/// How long a test waits for the holder to reach the sink; the hold itself ends after six times
/// that, so a failing test still ends.
const PROMPT: Duration = Duration::from_secs(10);

/// The name of W's thread, which the held sink holds.
pub(crate) const W_THREAD: &str = "dashd-flush";

/// Whether `cond` holds within [`PROMPT`], polled each millisecond.
pub(crate) fn until(cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + PROMPT;
    while !cond() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// An [`AbortSink`] whose `abort_node` on W's thread ([`W_THREAD`]), while armed, says it entered,
/// then waits for the release before it forwards to `sink`. Every other call forwards at once.
pub(crate) struct HeldSink {
    sink: Arc<dyn AbortSink>,
    state: Mutex<Held>,
    cv: Condvar,
}

#[derive(Default)]
struct Held {
    armed: bool,
    /// The held call's thread, once it has entered.
    entered: Option<ThreadId>,
}

impl HeldSink {
    pub(crate) fn new(sink: Arc<dyn AbortSink>) -> HeldSink {
        HeldSink {
            sink,
            state: Mutex::default(),
            cv: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, Held> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Holds the next `abort_node` on the thread, until the guard drops: the test takes it before
    /// anything that can block, so that it is released before any join.
    pub(crate) fn arm(&self) -> Release<'_> {
        *self.state() = Held {
            armed: true,
            entered: None,
        };
        Release(self)
    }

    /// The held call's thread, once it has entered within [`PROMPT`].
    pub(crate) fn wait_entered(&self) -> Option<ThreadId> {
        let state = self.state();
        let (state, _) = self
            .cv
            .wait_timeout_while(state, PROMPT, |s| s.entered.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        state.entered
    }

    /// BL-554's checkpoint: the held call has entered, and `reader` is parked on `latch` for its
    /// report, within [`PROMPT`]. The held call's thread, if both held.
    pub(crate) fn parked(&self, latch: &GlobalFailStop, reader: ThreadId) -> Option<ThreadId> {
        let w = self.wait_entered()?;
        until(|| latch.pending_waiters().contains(&reader)).then_some(w)
    }

    fn release(&self) {
        self.state().armed = false;
        self.cv.notify_all();
    }
}

impl AbortSink for HeldSink {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        let me = std::thread::current();
        if me.name() == Some(W_THREAD) {
            let mut state = self.state();
            if state.armed && state.entered.is_none() {
                state.entered = Some(me.id());
                self.cv.notify_all();
                let _held = self
                    .cv
                    .wait_timeout_while(state, 6 * PROMPT, |s| s.armed)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
        self.sink.abort_node(log_msg, user_msg);
    }

    fn read_failed(&self, what: &str) {
        self.sink.read_failed(what);
    }
}

/// Releases the held call when dropped.
#[must_use = "dropping it releases the held report"]
pub(crate) struct Release<'a>(&'a HeldSink);

impl Release<'_> {
    /// Releases the held call now.
    pub(crate) fn release(self) {}
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// W's step as a test stages it, on W's thread: `fail` makes the step's store failure latch (a
/// deferred trip) and returns the report text, then the report goes to `sink` (held there if
/// armed) and is marked made.
pub(crate) fn report_on_w(
    latch: &GlobalFailStop,
    sink: &dyn AbortSink,
    fail: impl FnOnce() -> String,
) {
    let deferred = latch.defer_reports();
    let report = fail();
    assert!(deferred.owns(), "W's failure latched first");
    sink.abort_node(&report, None);
    deferred.reported();
}

/// W's failure as a test stages it: on a thread named [`W_THREAD`], as W's step runs, `failure`
/// latches deferred, then `report` goes to `sink` (held there if armed) and is marked made.
/// Returns once the failure has latched.
pub(crate) fn fail_on_w(
    latch: &GlobalFailStop,
    sink: Arc<dyn AbortSink>,
    failure: Failure,
    report: String,
) -> std::thread::JoinHandle<()> {
    let latch = latch.clone();
    let (tripped, on_tripped) = mpsc::channel();
    let w = std::thread::Builder::new()
        .name(W_THREAD.into())
        .spawn(move || {
            report_on_w(&latch, &*sink, || {
                assert!(latch.trip(failure), "W's failure latches first");
                tripped.send(()).expect("the test waits");
                report
            });
        })
        .expect("W's thread");
    on_tripped.recv_timeout(PROMPT).expect("W's failure latched");
    w
}
