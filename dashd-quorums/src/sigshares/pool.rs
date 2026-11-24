//! Core's `sigsh-work` pool: named workers over one FIFO queue.

use std::collections::VecDeque;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use dashd_chainstate::chain::reply::no_wait_marked;

pub(super) type Task = Box<dyn FnOnce() + Send>;

/// Called with a thread's name when its body or one of its tasks panics.
pub(super) type OnPanic = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct Queue {
    tasks: VecDeque<Task>,
    stopping: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Shared {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Where the dispatcher pushes its tasks.
#[derive(Clone)]
pub(super) struct PoolQueue(Arc<Shared>);

impl PoolQueue {
    pub(super) fn push(&self, task: Task) {
        self.0.queue().tasks.push_back(task);
        self.0.ready.notify_one();
    }
}

pub(super) struct Pool {
    queue: PoolQueue,
    workers: Vec<JoinHandle<()>>,
}

pub(super) fn spawn(
    name: String,
    on_panic: OnPanic,
    f: impl FnOnce() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name(name.clone()).spawn(move || caught(&on_panic, &name, f))
}

fn caught(on_panic: &OnPanic, name: &str, f: impl FnOnce()) {
    if panic::catch_unwind(AssertUnwindSafe(f)).is_err() {
        on_panic(name);
    }
}

impl Pool {
    /// `workers` threads named `sigsh-work.<i>`.
    // Core: v24.0.0 src/llmq/net_signing.cpp:163-165 NetSigning::Start
    pub(super) fn start(workers: usize, on_panic: &OnPanic) -> io::Result<Self> {
        let mut pool = Self {
            queue: PoolQueue(Arc::default()),
            workers: Vec::with_capacity(workers),
        };
        for i in 0..workers {
            let (shared, hook) = (Arc::clone(&pool.queue.0), Arc::clone(on_panic));
            let name = format!("sigsh-work.{i}");
            match spawn(name.clone(), Arc::clone(on_panic), move || run(&shared, &hook, &name)) {
                Ok(worker) => pool.workers.push(worker),
                Err(e) => {
                    pool.stop();
                    return Err(e);
                }
            }
        }
        Ok(pool)
    }

    pub(super) fn queue(&self) -> PoolQueue {
        self.queue.clone()
    }

    /// Drops the queued tasks, then joins the workers once their running tasks end.
    // Core: v24.0.0 src/llmq/net_signing.cpp:192-194 NetSigning::Stop
    pub(super) fn stop(self) {
        let dropped = {
            let mut queue = self.queue.0.queue();
            queue.stopping = true;
            std::mem::take(&mut queue.tasks)
        };
        self.queue.0.ready.notify_all();
        drop(dropped);
        for worker in self.workers {
            let _ = worker.join();
        }
    }
}

fn run(shared: &Shared, on_panic: &OnPanic, name: &str) {
    debug_assert!(!no_wait_marked(), "sigsh-work runs unmarked");
    loop {
        let task = {
            let mut queue = shared.queue();
            loop {
                if let Some(task) = queue.tasks.pop_front() {
                    break task;
                }
                if queue.stopping {
                    return;
                }
                queue = shared.ready.wait(queue).unwrap_or_else(PoisonError::into_inner);
            }
        };
        caught(on_panic, name, task);
    }
}
