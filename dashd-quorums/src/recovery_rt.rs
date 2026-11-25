//! Core's `NetQuorum::workerPool` (a `ctpl::thread_pool`): the bounded worker pool, owned by
//! `NetQuorum`, that runs its quorum data recovery jobs. The quorum manager's warm jobs keep
//! their own `q-cache` thread, as Core's `CQuorumManager` does (PLAN-QM1b §7g correction).
//!
//! Submitting never blocks and never runs a job on the caller's thread: the queue is unbounded,
//! as ctpl's is. A job queued before [`RecoveryPool::try_start`] waits for it.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::net_quorum;

type Job = Box<dyn FnOnce() + Send>;

/// `DEFAULT_WORKER_COUNT`: half the hardware threads, at least 1 and at most 4. An unknown count
/// is Core's `hardware_concurrency() == 0`.
// Core: v24.0.0 src/llmq/options.cpp:24 DEFAULT_WORKER_COUNT
pub fn default_worker_count() -> usize {
    let hw = std::thread::available_parallelism().map_or(0, usize::from);
    (hw / 2).clamp(1, 4)
}

/// The pool's thread name prefix (`RenameThreadPool(workerPool, "q-mngr")`).
// Core: v24.0.0 src/llmq/net_quorum.cpp:63, src/util/system.cpp:1131
const THREAD_NAME: &str = "q-mngr";

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// `isDone`: workers finish the queue, then exit; later pushes are dropped.
    done: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn work(&self) {
        loop {
            let job = {
                let mut queue = self.lock();
                loop {
                    if let Some(job) = queue.jobs.pop_front() {
                        break job;
                    }
                    if queue.done {
                        return;
                    }
                    queue = self.wake.wait(queue).unwrap_or_else(PoisonError::into_inner);
                }
            };
            // ctpl runs each job as a `packaged_task`: what it throws goes to a future nobody
            // reads, and the worker takes the next job.
            // Core: v24.0.0 src/ctpl_stl.h:190-196
            let _ = catch_unwind(AssertUnwindSafe(job));
        }
    }
}

#[derive(Default)]
struct Threads {
    handles: Vec<JoinHandle<()>>,
    started: bool,
}

/// The worker pool. [`new`](Self::new) makes it unstarted; [`try_start`](Self::try_start) spawns its
/// workers once; [`halt`](Self::halt) waits for them. Dropping it clears the queue and stops it.
pub struct RecoveryPool {
    workers: usize,
    shared: Arc<Shared>,
    threads: Mutex<Threads>,
}

impl RecoveryPool {
    /// An unstarted pool of `workers` threads (at least one).
    pub fn new(workers: usize) -> Arc<Self> {
        Arc::new(Self {
            workers: workers.max(1),
            shared: Arc::default(),
            threads: Mutex::default(),
        })
    }

    /// The number of workers [`try_start`](Self::try_start) spawns.
    pub fn workers(&self) -> usize {
        self.workers
    }

    fn threads(&self) -> MutexGuard<'_, Threads> {
        self.threads.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `workerPool.resize(n)`: spawns the workers `d-q-mngr-0` … `d-q-mngr-<n-1>`. After a start
    /// or a stop it does nothing. A failed spawn stops the workers already spawned.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:58-64 NetQuorum::Start
    pub fn try_start(&self) -> std::io::Result<()> {
        let mut threads = self.threads();
        if threads.started {
            return Ok(());
        }
        threads.started = true;
        for i in 0..self.workers {
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name(format!("d-{THREAD_NAME}-{i}"))
                .spawn(move || shared.work());
            match spawned {
                Ok(handle) => threads.handles.push(handle),
                Err(e) => {
                    let handles = std::mem::take(&mut threads.handles);
                    drop(threads);
                    self.finish(handles);
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Whether [`try_start`](Self::try_start) has run.
    pub fn is_started(&self) -> bool {
        self.threads().started
    }

    /// Whether workers are running: started and not yet halted.
    pub fn is_running(&self) -> bool {
        !self.threads().handles.is_empty()
    }

    /// `workerPool.push(job)`: queues `job` and returns at once. After [`halt`](Self::halt) the
    /// job is dropped unrun.
    pub fn submit(&self, job: Job) {
        let mut queue = self.shared.lock();
        if queue.done {
            drop(queue);
            drop(job);
            return;
        }
        queue.jobs.push_back(job);
        drop(queue);
        self.shared.wake.notify_one();
    }

    /// `workerPool.clear_queue()`: drops every job not started, outside the pool's lock.
    pub fn clear(&self) {
        let jobs = std::mem::take(&mut self.shared.lock().jobs);
        drop(jobs);
    }

    /// `workerPool.stop(true)`: the workers run what is queued, then exit, and are joined;
    /// nothing queued later runs. On a worker itself, which a job dropping the last handle can
    /// be, it does not join that worker.
    // Core: v24.0.0 src/ctpl_stl.h:142-170
    pub fn halt(&self) {
        let handles = std::mem::take(&mut self.threads().handles);
        self.finish(handles);
    }

    fn finish(&self, handles: Vec<JoinHandle<()>>) {
        let unrun = {
            let mut queue = self.shared.lock();
            queue.done = true;
            if handles.is_empty() {
                std::mem::take(&mut queue.jobs)
            } else {
                VecDeque::new()
            }
        };
        drop(unrun);
        self.shared.wake.notify_all();
        let me = std::thread::current().id();
        for handle in handles {
            if handle.thread().id() != me {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for RecoveryPool {
    fn drop(&mut self) {
        self.clear();
        self.halt();
    }
}

/// `NetQuorum`'s `workerPool`: `Start` spawns the workers, `Stop` clears the queue and joins them.
// Core: v24.0.0 src/llmq/net_quorum.cpp:58-70
impl net_quorum::RecoveryPool for RecoveryPool {
    fn start(&self) {
        if let Err(e) = self.try_start() {
            tracing::error!(target: "llmq", "NetQuorum::Start -- {e}");
        }
    }

    fn push(&self, job: Box<dyn FnOnce() + Send>) {
        self.submit(job);
    }

    fn clear_queue(&self) {
        self.clear();
    }

    fn stop(&self) {
        self.halt();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::{Duration, Instant};

    use super::*;

    const WAIT: Duration = Duration::from_secs(30);

    /// A job that reports its start on `started`, then parks until `gate` sends or drops.
    fn parked(started: &Sender<String>, gate: Receiver<()>) -> Job {
        let started = started.clone();
        Box::new(move || {
            let name = std::thread::current().name().unwrap_or_default().to_owned();
            started.send(name).unwrap();
            let _ = gate.recv();
        })
    }

    #[test]
    fn the_worker_count_is_half_the_hardware_threads_clamped_to_one_to_four() {
        let n = default_worker_count();
        assert!((1..=4).contains(&n));
        let hw = std::thread::available_parallelism().map_or(0, usize::from);
        assert_eq!(n, (hw / 2).clamp(1, 4));
    }

    /// Plan §4 QM1b-7: five jobs on four workers. Four run at once, each on its own `q-mngr`
    /// worker; the fifth waits for a free worker.
    #[test]
    fn five_jobs_on_four_workers() {
        let pool = RecoveryPool::new(4);
        pool.try_start().unwrap();
        let (started, starts) = channel();
        let mut gates = Vec::new();
        for _ in 0..5 {
            let (gate, wait) = channel();
            gates.push(gate);
            pool.submit(parked(&started, wait));
        }
        let mut names: Vec<String> = (0..4).map(|_| starts.recv_timeout(WAIT).unwrap()).collect();
        assert!(starts.recv_timeout(Duration::from_millis(200)).is_err(), "the fifth waits");
        names.sort();
        assert_eq!(names, ["d-q-mngr-0", "d-q-mngr-1", "d-q-mngr-2", "d-q-mngr-3"]);
        drop(gates.remove(0));
        assert!(starts.recv_timeout(WAIT).unwrap().starts_with("d-q-mngr-"));
        drop(gates);
        pool.halt();
    }

    /// Plan §4 QM1b-7: with every worker parked, `workerPool.push` returns at once on the
    /// caller's thread without running the job, and the job runs once a worker frees.
    #[test]
    fn push_never_blocks_or_runs_inline() {
        let pool = RecoveryPool::new(2);
        pool.try_start().unwrap();
        let (started, starts) = channel();
        let (gate_a, wait_a) = channel();
        let (gate_b, wait_b) = channel();
        pool.submit(parked(&started, wait_a));
        pool.submit(parked(&started, wait_b));
        starts.recv_timeout(WAIT).unwrap();
        starts.recv_timeout(WAIT).unwrap();
        let ran_on = Arc::new(Mutex::new(None));
        let at = Instant::now();
        let caller = std::thread::current().id();
        for _ in 0..100 {
            let ran_on = Arc::clone(&ran_on);
            net_quorum::RecoveryPool::push(
                &*pool,
                Box::new(move || {
                    ran_on.lock().unwrap().get_or_insert(std::thread::current().id());
                }),
            );
        }
        assert!(at.elapsed() < Duration::from_secs(1), "the submits returned at once");
        assert!(ran_on.lock().unwrap().is_none(), "nothing ran while every worker is parked");
        drop(gate_a);
        let deadline = Instant::now() + WAIT;
        while ran_on.lock().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the warm job runs once a worker frees");
            std::thread::yield_now();
        }
        assert_ne!(ran_on.lock().unwrap().unwrap(), caller);
        drop(gate_b);
        pool.halt();
    }

    /// Jobs queued before the start wait for it; a second start does nothing.
    #[test]
    fn jobs_wait_for_the_start() {
        let pool = RecoveryPool::new(1);
        let (tx, rx) = channel();
        pool.submit(Box::new(move || tx.send(()).unwrap()));
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(!pool.is_started());
        pool.try_start().unwrap();
        pool.try_start().unwrap();
        rx.recv_timeout(WAIT).unwrap();
        assert_eq!(pool.threads().handles.len(), 1);
    }

    /// `NetQuorum::Stop`: the queue is cleared, the running job is waited for, and a job pushed
    /// after the stop never runs.
    #[test]
    fn stop_clears_the_queue_and_joins_the_running_job() {
        let pool = RecoveryPool::new(1);
        pool.try_start().unwrap();
        let (started, starts) = channel();
        let (gate, wait) = channel();
        pool.submit(parked(&started, wait));
        starts.recv_timeout(WAIT).unwrap();
        let ran = Arc::new(AtomicUsize::new(0));
        let queued = Arc::clone(&ran);
        pool.submit(Box::new(move || {
            queued.fetch_add(1, SeqCst);
        }));
        net_quorum::RecoveryPool::clear_queue(&*pool);
        let stopper = {
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || net_quorum::RecoveryPool::stop(&*pool))
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(!stopper.is_finished(), "the stop waits for the running job");
        drop(gate);
        stopper.join().unwrap();
        let late = Arc::clone(&ran);
        pool.submit(Box::new(move || {
            late.fetch_add(1, SeqCst);
        }));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(ran.load(SeqCst), 0);
    }

    /// `stop(true)` without a clear runs what is queued before the workers exit.
    #[test]
    fn stop_runs_the_queue_left_uncleared() {
        let pool = RecoveryPool::new(1);
        let ran = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let ran = Arc::clone(&ran);
            pool.submit(Box::new(move || {
                ran.fetch_add(1, SeqCst);
            }));
        }
        pool.try_start().unwrap();
        pool.halt();
        assert_eq!(ran.load(SeqCst), 3);
    }

    /// A panicking job does not take its worker down, as a ctpl `packaged_task`'s throw does not.
    #[test]
    fn a_panicking_job_leaves_the_worker_running() {
        let pool = RecoveryPool::new(1);
        pool.try_start().unwrap();
        pool.submit(Box::new(|| panic!("job failed")));
        let (tx, rx) = channel();
        pool.submit(Box::new(move || tx.send(()).unwrap()));
        rx.recv_timeout(WAIT).unwrap();
    }

    /// A job that drops the last handle stops the pool from its own worker without joining it.
    #[test]
    fn dropping_the_last_handle_on_a_worker_does_not_deadlock() {
        let pool = RecoveryPool::new(1);
        pool.try_start().unwrap();
        let (tx, rx) = channel();
        let last = Arc::clone(&pool);
        drop(pool);
        let holder = Mutex::new(Some(last));
        let pool = holder.lock().unwrap().clone().unwrap();
        pool.submit(Box::new(move || {
            drop(holder.lock().unwrap().take());
            tx.send(()).unwrap();
        }));
        drop(pool);
        rx.recv_timeout(WAIT).unwrap();
    }
}
