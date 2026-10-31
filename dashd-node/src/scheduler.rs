//! Core's lightweight task scheduler (`scheduler.{h,cpp}`): one `scheduler` thread runs timed
//! tasks on the steady clock.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::host;

type Task = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Queue {
    /// Ordered by time, then by insertion (Core's `std::multimap` keeps equal times in insertion
    /// order).
    tasks: BTreeMap<(Instant, u64), Task>,
    next_seq: u64,
    stop_requested: bool,
    stop_when_empty: bool,
    threads_servicing: usize,
}

impl Queue {
    fn should_stop(&self) -> bool {
        self.stop_requested || (self.stop_when_empty && self.tasks.is_empty())
    }

    fn insert(&mut self, at: Instant, task: Task) {
        self.tasks.insert((at, self.next_seq), task);
        self.next_seq += 1;
    }
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    new_task: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn schedule(&self, task: Task, at: Instant) {
        self.lock().insert(at, task);
        self.new_task.notify_one();
    }
}

/// Core's `CScheduler`. Clones share the queue.
#[derive(Clone, Default)]
pub struct Scheduler {
    shared: Arc<Shared>,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl Scheduler {
    pub fn new() -> Scheduler {
        Scheduler::default()
    }

    /// Starts the `scheduler` thread running [`service_queue`](Self::service_queue) (Core's
    /// `m_service_thread`, through `TraceThread`).
    // Core: init.cpp:1576
    pub fn start(&self) -> std::io::Result<()> {
        let mut thread = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(thread.is_none(), "the scheduler thread is running");
        let this = self.clone();
        *thread = Some(host::trace_thread("scheduler", move || this.service_queue())?);
        Ok(())
    }

    /// Core's `serviceQueue`: runs tasks as they fall due until stopped.
    // Core: scheduler.cpp:23-73
    pub fn service_queue(&self) {
        let mut q = self.shared.lock();
        q.threads_servicing += 1;
        while !q.should_stop() {
            while !q.should_stop() && q.tasks.is_empty() {
                q = self.shared.new_task.wait(q).unwrap_or_else(PoisonError::into_inner);
            }
            // Wait for a new task or the first task's time.
            while !q.should_stop() {
                let Some(&(due, _)) = q.tasks.keys().next() else {
                    break;
                };
                let now = Instant::now();
                if now >= due {
                    break;
                }
                let (guard, timeout) = self
                    .shared
                    .new_task
                    .wait_timeout(q, due - now)
                    .unwrap_or_else(PoisonError::into_inner);
                q = guard;
                if timeout.timed_out() {
                    break;
                }
            }
            if q.should_stop() {
                continue;
            }
            let Some(task) = q.tasks.pop_first().map(|(_, t)| t) else {
                continue;
            };
            // Unlocked, so the task can schedule itself or another.
            drop(q);
            task();
            q = self.shared.lock();
        }
        q.threads_servicing -= 1;
        drop(q);
        self.shared.new_task.notify_one();
    }

    /// Core's `schedule`: runs `f` at `at`.
    pub fn schedule(&self, f: impl FnOnce() + Send + 'static, at: Instant) {
        self.shared.schedule(Box::new(f), at);
    }

    /// Core's `scheduleFromNow`.
    pub fn schedule_from_now(&self, f: impl FnOnce() + Send + 'static, delta: Duration) {
        self.schedule(f, Instant::now() + delta);
    }

    /// Core's `scheduleEvery`: runs `f` every `delta`, the first time after one `delta`, until
    /// the scheduler stops.
    // Core: scheduler.cpp:105-114
    pub fn schedule_every(&self, f: impl Fn() + Send + Sync + 'static, delta: Duration) {
        let f: Arc<dyn Fn() + Send + Sync> = Arc::new(f);
        schedule_repeat(Arc::downgrade(&self.shared), f, delta);
    }

    /// Core's `MockForward`: brings every task `delta` closer (`mockscheduler`). `delta` is in
    /// (0, 1 h].
    // Core: scheduler.cpp:84-103
    pub fn mock_forward(&self, delta: Duration) {
        assert!(delta > Duration::ZERO && delta <= Duration::from_secs(3600));
        {
            let mut q = self.shared.lock();
            let tasks = std::mem::take(&mut q.tasks);
            q.tasks = tasks
                .into_iter()
                .map(|((at, seq), t)| ((at.checked_sub(delta).unwrap_or(at), seq), t))
                .collect();
        }
        self.shared.new_task.notify_one();
    }

    /// Core's `getQueueInfo`: the task count and the first and last due times.
    pub fn queue_info(&self) -> (usize, Option<(Instant, Instant)>) {
        let q = self.shared.lock();
        let first = q.tasks.keys().next().map(|k| k.0);
        let last = q.tasks.keys().next_back().map(|k| k.0);
        (q.tasks.len(), first.zip(last))
    }

    /// Core's `AreThreadsServicingQueue`.
    pub fn are_threads_servicing_queue(&self) -> bool {
        self.shared.lock().threads_servicing > 0
    }

    /// Core's `stop`: the thread exits once its current task is done, and is joined. Tasks left
    /// in the queue never run.
    // Core: scheduler.h:78-83
    pub fn stop(&self) {
        self.shared.lock().stop_requested = true;
        self.shared.new_task.notify_all();
        self.join();
    }

    /// Core's `StopWhenDrained`: the thread exits once the queue is empty, and is joined.
    pub fn stop_when_drained(&self) {
        self.shared.lock().stop_when_empty = true;
        self.shared.new_task.notify_all();
        self.join();
    }

    fn join(&self) {
        let mut slot = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
        // A task that stops the scheduler runs on its thread, which cannot join itself (Core's
        // `stop` from a task: `joinable()`, then `join()` would throw).
        if slot.as_ref().is_some_and(|t| t.thread().id() == std::thread::current().id()) {
            return;
        }
        if let Some(thread) = slot.take() {
            drop(slot);
            // A task's panic has already aborted the process (`host::trace_thread`).
            let _ = thread.join();
        }
    }
}

/// Core's `Repeat`. The queue holds the task, so the task holds the queue weakly: dropping every
/// [`Scheduler`] frees both.
fn schedule_repeat(shared: Weak<Shared>, f: Arc<dyn Fn() + Send + Sync>, delta: Duration) {
    let Some(strong) = shared.upgrade() else {
        return;
    };
    let task = move || {
        f();
        schedule_repeat(shared, f, delta);
    };
    strong.schedule(Box::new(task), Instant::now() + delta);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn tasks_run_in_time_order_and_equal_times_in_insertion_order() {
        let s = Scheduler::new();
        let (tx, rx) = mpsc::channel();
        let now = Instant::now();
        for (name, ms) in [("c", 30), ("a", 10), ("b1", 20), ("b2", 20)] {
            let tx = tx.clone();
            s.schedule(move || tx.send(name).unwrap(), now + Duration::from_millis(ms));
        }
        s.start().unwrap();
        let order: Vec<_> = (0..4).map(|_| rx.recv().unwrap()).collect();
        assert_eq!(order, ["a", "b1", "b2", "c"]);
        s.stop();
        assert!(!s.are_threads_servicing_queue());
    }

    #[test]
    fn stop_leaves_later_tasks_and_drain_runs_them() {
        let s = Scheduler::new();
        let ran = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&ran);
        s.schedule_from_now(
            move || {
                r.fetch_add(1, Ordering::SeqCst);
            },
            Duration::from_secs(3600),
        );
        s.start().unwrap();
        s.stop();
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(s.queue_info().0, 1);

        let s = Scheduler::new();
        for _ in 0..3 {
            let r = Arc::clone(&ran);
            s.schedule_from_now(
                move || {
                    r.fetch_add(1, Ordering::SeqCst);
                },
                Duration::ZERO,
            );
        }
        s.start().unwrap();
        s.stop_when_drained();
        assert_eq!(ran.load(Ordering::SeqCst), 3);
        assert_eq!(s.queue_info(), (0, None));
    }

    #[test]
    fn mock_forward() {
        // Core: src/test/scheduler_tests.cpp:175-214 (`mockforward`; commented out in Dash v24).
        let s = Scheduler::new();
        let counter = Arc::new(AtomicUsize::new(0));
        for minutes in [2u64, 5, 8] {
            let c = Arc::clone(&counter);
            s.schedule_from_now(
                move || {
                    c.fetch_add(1, Ordering::SeqCst);
                },
                Duration::from_secs(60 * minutes),
            );
        }
        assert_eq!(s.queue_info().0, 3);
        let thread = std::thread::spawn({
            let s = s.clone();
            move || s.service_queue()
        });
        s.mock_forward(Duration::from_secs(5 * 60));
        let stopper = s.clone();
        s.schedule_from_now(move || stopper.stop(), Duration::from_millis(1));
        thread.join().unwrap();
        let (n, Some((first, _))) = s.queue_info() else {
            panic!()
        };
        assert_eq!(n, 1);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        let delta = first.saturating_duration_since(Instant::now()).as_secs();
        assert!(delta > 2 * 60 && delta < 3 * 60, "{delta}");
    }

    #[test]
    fn schedule_every_repeats_until_stopped() {
        let s = Scheduler::new();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        s.schedule_every(move || tx.lock().unwrap().send(()).unwrap(), Duration::from_millis(1));
        s.start().unwrap();
        for _ in 0..5 {
            rx.recv().unwrap();
        }
        s.stop();
        // One repeat is queued, and the queue holds the scheduler only weakly.
        assert_eq!(s.queue_info().0, 1);
        let weak = Arc::downgrade(&s.shared);
        drop(s);
        assert!(weak.upgrade().is_none());
    }
}
