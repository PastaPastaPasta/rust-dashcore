//! The host's bounded service for blocking work (SB0 §7, L5): storage calls and DNS.
//!
//! A shared crate starts no threads and calls no blocking-offload entry point (`spawn_blocking`,
//! `block_in_place`). It takes an `Arc<dyn` [`BlockingIo`]`>` from its host and runs blocking work
//! through [`run_blocking`], which gives a [`Pending`] result: async code awaits it, a thread
//! [`wait`](Pending::wait)s. The node's host is ND1's bounded `BlockingPool`; on mobile it is the
//! client runtime's blocking pool, bounded by dash-spv-ffi (`max_blocking_threads = 2`).

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

/// A unit of blocking work, boxed for the host.
pub type Job = Box<dyn FnOnce() + Send + 'static>;

/// A host's bounded service for blocking work: a thread pool it owns and sizes.
pub trait BlockingIo: Send + Sync {
    /// Runs `job` on one of the host's threads, now or after the work queued before it. A host
    /// that cannot run it (it is shutting down, or has no thread) drops it unrun, and the
    /// [`Pending`] it belongs to resolves to [`BlockingError::Cancelled`].
    fn spawn(&self, job: Job);
}

/// Runs each job on the calling thread, at once: for tests, and for a caller that is a blocking
/// thread of the host already.
#[derive(Clone, Copy, Debug, Default)]
pub struct InlineIo;

impl BlockingIo for InlineIo {
    fn spawn(&self, job: Job) {
        job();
    }
}

/// Why a [`Pending`] has no result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BlockingError {
    /// The host dropped the job unrun.
    #[error("the blocking job was cancelled")]
    Cancelled,
    /// The job panicked. The panic is caught on the host's thread, which runs on.
    #[error("the blocking job panicked")]
    Panicked,
}

/// Runs `f` through `io`, and gives its result.
pub fn run_blocking<T: Send + 'static>(
    io: &dyn BlockingIo,
    f: impl FnOnce() -> T + Send + 'static,
) -> Pending<T> {
    let slot = Arc::new(Slot::default());
    let done = Completer(Some(Arc::clone(&slot)));
    io.spawn(Box::new(move || {
        done.finish(catch_unwind(AssertUnwindSafe(f)).map_err(|_| BlockingError::Panicked));
    }));
    Pending {
        slot,
    }
}

/// The result of a [`run_blocking`]: [`wait`](Self::wait) for it, or await it. Dropping it does
/// not cancel the job, which runs and drops its result.
#[must_use = "a blocking job's result is only seen through its Pending"]
pub struct Pending<T> {
    slot: Arc<Slot<T>>,
}

impl<T> Pending<T> {
    /// Blocks the calling thread until the job is done, or the host dropped it.
    ///
    /// # Errors
    ///
    /// [`BlockingError`] if the job was cancelled or panicked.
    pub fn wait(self) -> Result<T, BlockingError> {
        let mut state = self.slot.lock();
        loop {
            if let Some(out) = state.out.take() {
                return out;
            }
            state = self.slot.ready.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl<T> Future for Pending<T> {
    type Output = Result<T, BlockingError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.slot.lock();
        if let Some(out) = state.out.take() {
            return Poll::Ready(out);
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// A one-shot result slot, shared by the job and its [`Pending`].
struct Slot<T> {
    state: Mutex<SlotState<T>>,
    ready: Condvar,
}

struct SlotState<T> {
    out: Option<Result<T, BlockingError>>,
    /// The awaiting task's waker.
    waker: Option<Waker>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot {
            state: Mutex::new(SlotState {
                out: None,
                waker: None,
            }),
            ready: Condvar::new(),
        }
    }
}

impl<T> Slot<T> {
    /// The state, also after a panic elsewhere.
    fn lock(&self) -> MutexGuard<'_, SlotState<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the result, and wakes the waiter.
    fn complete(&self, out: Result<T, BlockingError>) {
        let mut state = self.lock();
        state.out = Some(out);
        let waker = state.waker.take();
        drop(state);
        self.ready.notify_all();
        if let Some(w) = waker {
            w.wake();
        }
    }
}

/// The job's hold on its slot, which completes it once: with the job's result, or as cancelled
/// if the job is dropped unrun.
struct Completer<T>(Option<Arc<Slot<T>>>);

impl<T> Completer<T> {
    fn finish(mut self, out: Result<T, BlockingError>) {
        if let Some(slot) = self.0.take() {
            slot.complete(out);
        }
    }
}

impl<T> Drop for Completer<T> {
    fn drop(&mut self) {
        if let Some(slot) = self.0.take() {
            slot.complete(Err(BlockingError::Cancelled));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;
    use std::thread::JoinHandle;

    use super::*;

    /// A host that keeps its jobs until the test runs or drops them.
    #[derive(Default)]
    struct Queue(Mutex<Vec<Job>>);

    impl BlockingIo for Queue {
        fn spawn(&self, job: Job) {
            self.0.lock().unwrap().push(job);
        }
    }

    impl Queue {
        fn take(&self) -> Vec<Job> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    /// A host with one thread per job.
    #[derive(Default)]
    struct Threads(Mutex<Vec<JoinHandle<()>>>);

    impl BlockingIo for Threads {
        fn spawn(&self, job: Job) {
            self.0.lock().unwrap().push(std::thread::spawn(job));
        }
    }

    #[derive(Default)]
    struct CountWakes(AtomicUsize);

    impl Wake for CountWakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn inline_runs_at_once() {
        assert_eq!(run_blocking(&InlineIo, || 7).wait(), Ok(7));
    }

    #[test]
    fn a_thread_waits_for_another_hosts_thread() {
        let io = Threads::default();
        let p = run_blocking(&io, || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            "done"
        });
        assert_eq!(p.wait(), Ok("done"));
        io.0.lock().unwrap().drain(..).for_each(|h| h.join().unwrap());
    }

    /// The job runs after the first poll: the poll stores the waker, the job wakes it once, and
    /// the next poll gives the result.
    #[test]
    fn a_poll_before_the_job_is_woken_by_it() {
        let io = Queue::default();
        let mut p = run_blocking(&io, || 5);
        let wakes = Arc::new(CountWakes::default());
        let waker = Waker::from(Arc::clone(&wakes));
        let mut cx = Context::from_waker(&waker);
        assert_eq!(Pin::new(&mut p).poll(&mut cx), Poll::Pending);
        io.take().into_iter().for_each(|job| job());
        assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
        assert_eq!(Pin::new(&mut p).poll(&mut cx), Poll::Ready(Ok(5)));
    }

    /// A host that drops a job unrun resolves its waiter as cancelled, also one that already
    /// waits on another thread.
    #[test]
    fn a_dropped_job_is_cancelled() {
        // Polled first, so a missed cancel fails here instead of hanging the wait below.
        let io = Queue::default();
        let mut p = run_blocking(&io, || 1);
        drop(io.take());
        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(Pin::new(&mut p).poll(&mut cx), Poll::Ready(Err(BlockingError::Cancelled)));

        let p = run_blocking(&io, || 1);
        let waiter = std::thread::spawn(move || p.wait());
        // Either order gives `Cancelled`; the pause makes a parked waiter the likely case.
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(io.take());
        assert_eq!(waiter.join().unwrap(), Err(BlockingError::Cancelled));
    }

    /// A panic is the job's result, not the host thread's: the thread runs the next job.
    #[test]
    fn a_panic_is_caught_on_the_host_thread() {
        let io = Queue::default();
        let p = run_blocking(&io, || -> u8 { panic!("job") });
        let q = run_blocking(&io, || 2);
        io.take().into_iter().for_each(|job| job());
        assert_eq!(p.wait(), Err(BlockingError::Panicked));
        assert_eq!(q.wait(), Ok(2));
    }
}
