//! The queue under `ValidationSignals` (§8.2, R2-M2): Core's `SingleThreadedSchedulerClient`
//! list with every critical section O(1) under its leaf mutex L2s (DEC-152).
//!
//! - **Enqueue** builds its one-node list before the lock; under L2s it only appends (pointer
//!   writes) and counts; the wake is a condvar notify after the unlock.
//! - **Dequeue** takes the whole list under L2s (`mem::take`) and runs it outside the lock, so
//!   nodes are freed outside it too.
//! - **`pending`** counts what is queued or detached and not yet started: Core's
//!   `CallbacksPending()` (`scheduler.cpp:196-200`), from which only the running callback has
//!   been popped.
//!
//! Generic over the entry so the loom model (`--cfg loom`) runs it without the listeners.

use std::any::Any;
use std::collections::LinkedList;
use std::panic::{self, AssertUnwindSafe};

#[cfg(loom)]
use loom::sync::atomic::{AtomicUsize, Ordering};
#[cfg(loom)]
use loom::sync::{Condvar, Mutex, MutexGuard};
#[cfg(not(loom))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(not(loom))]
use std::sync::{Condvar, Mutex, MutexGuard};

struct State<T> {
    list: LinkedList<T>,
    /// The consumer was asked to stop: it runs what is queued, then closes.
    stopping: bool,
    /// The consumer has exited: later entries are handed back to their producer.
    closed: bool,
}

pub(crate) struct Fifo<T> {
    /// L2s, a DEC-152 leaf.
    state: Mutex<State<T>>,
    pending: AtomicUsize,
    /// Notification only (DEC-152 (2)).
    wake: Condvar,
}

impl<T> Default for Fifo<T> {
    fn default() -> Self {
        Fifo::new()
    }
}

impl<T> Fifo<T> {
    pub(crate) fn new() -> Fifo<T> {
        Fifo {
            state: Mutex::new(State {
                list: LinkedList::new(),
                stopping: false,
                closed: false,
            }),
            pending: AtomicUsize::new(0),
            wake: Condvar::new(),
        }
    }

    /// The leaf section: `f` under L2s, which it may release only by returning or by waiting
    /// on `wake`. Test builds with `alloc-tag` (the tagging allocator) count the allocator calls
    /// this thread makes meanwhile, which must be none (§9.6's growth test).
    fn leaf<R>(&self, f: impl FnOnce(MutexGuard<'_, State<T>>) -> R) -> R {
        #[cfg(all(test, feature = "alloc-tag", not(loom), not(miri)))]
        let before = crate::alloc_tag::thread_alloc_events();
        let r = f(self.state.lock().unwrap_or_else(|p| p.into_inner()));
        #[cfg(all(test, feature = "alloc-tag", not(loom), not(miri)))]
        LEAF_ALLOCS.fetch_add(crate::alloc_tag::thread_alloc_events() - before, Ordering::SeqCst);
        r
    }

    /// Appends `list` in O(1). On a closed queue the entries stay in `list`, to be dropped
    /// outside the lock.
    pub(crate) fn append(&self, list: &mut LinkedList<T>) -> Result<(), ()> {
        if list.is_empty() {
            return Ok(());
        }
        let n = list.len();
        self.leaf(|mut g| {
            if g.closed {
                return Err(());
            }
            g.list.append(list);
            self.pending.fetch_add(n, Ordering::SeqCst);
            Ok(())
        })?;
        self.wake.notify_one();
        Ok(())
    }

    /// Enqueues one entry; the node is allocated before the lock. `Err` hands it back.
    pub(crate) fn push(&self, e: T) -> Result<(), T> {
        let mut one = LinkedList::new();
        one.push_back(e);
        self.append(&mut one).map_err(|()| one.pop_front().expect("handed back"))
    }

    pub(crate) fn pending(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    /// The consumer: runs every entry in FIFO order on the calling thread until [`Fifo::stop`],
    /// then runs what is left and closes. If `run` panics, `on_panic` hears it first (so the
    /// node is stopping before any waiter learns the queue is gone); then the queue closes, even
    /// if `on_panic` panicked: every entry left is dropped (outside L2s, so an entry's `Drop`
    /// must not panic) and uncounted, so barriers disconnect and later entries come back to
    /// their producers; then the panic continues, `on_panic`'s if it panicked (Core's scheduler
    /// thread ends on an exception, and the process with it).
    pub(crate) fn serve(&self, mut run: impl FnMut(T), on_panic: impl FnOnce(&(dyn Any + Send))) {
        loop {
            let Some(mut batch) = self.leaf(|mut g| {
                // DEC-152's loom rule: the reader is preempted inside the leaf section.
                #[cfg(loom)]
                loom::thread::yield_now();
                while g.list.is_empty() {
                    if g.stopping {
                        g.closed = true;
                        return None;
                    }
                    g = self.wake.wait(g).unwrap_or_else(|p| p.into_inner());
                }
                Some(std::mem::take(&mut g.list))
            }) else {
                return;
            };
            while let Some(e) = batch.pop_front() {
                self.pending.fetch_sub(1, Ordering::SeqCst);
                let ran = panic::catch_unwind(AssertUnwindSafe(|| run(e)));
                if let Err(p) = ran {
                    // Closes even if `on_panic` panics too, which would otherwise leave every
                    // waiter on a queue nobody serves.
                    let close = CloseOnDrop {
                        fifo: self,
                        batch: &mut batch,
                    };
                    on_panic(p.as_ref());
                    drop(close);
                    panic::resume_unwind(p);
                }
            }
        }
    }

    /// Asks the consumer to finish what is queued and exit.
    pub(crate) fn stop(&self) {
        self.leaf(|mut g| g.stopping = true);
        self.wake.notify_one();
    }
}

/// The consumer's exit after a panic ([`Fifo::serve`]): the queue closes, and every
/// entry left, queued or in the detached batch, is dropped outside L2s and uncounted.
struct CloseOnDrop<'a, T> {
    fifo: &'a Fifo<T>,
    batch: &'a mut LinkedList<T>,
}

impl<T> Drop for CloseOnDrop<'_, T> {
    fn drop(&mut self) {
        let mut left = self.fifo.leaf(|mut g| {
            g.closed = true;
            std::mem::take(&mut g.list)
        });
        left.append(self.batch);
        self.fifo.pending.fetch_sub(left.len(), Ordering::SeqCst);
    }
}

/// The allocator calls counted inside leaf sections, process-wide (test builds with `alloc-tag`).
#[cfg(all(test, feature = "alloc-tag", not(loom), not(miri)))]
pub(crate) static LEAF_ALLOCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// DEC-152's model (`--cfg loom`, contrib/node/check-loom.sh): a pusher and a splicer against
/// the consumer, which loom preempts inside its leaf section, and a late push racing `stop`.
/// Every accepted entry runs once, in each producer's order; a refused one comes back; `pending`
/// never wraps and ends at zero.
#[cfg(all(loom, test))]
mod loom_tests {
    use loom::sync::Arc;
    use loom::thread;

    use super::*;

    #[test]
    fn loom_two_producers_one_preempted_consumer() {
        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(2);
        model.check(|| {
            let q = Arc::new(Fifo::<u32>::new());
            let consumer = {
                let q = q.clone();
                thread::spawn(move || {
                    let mut seen = Vec::new();
                    q.serve(|e| seen.push((e, q.pending())), |_| ());
                    seen
                })
            };
            let pusher = {
                let q = q.clone();
                thread::spawn(move || {
                    q.push(1).expect("open");
                    q.push(2).expect("open");
                })
            };
            let splicer = {
                let q = q.clone();
                thread::spawn(move || {
                    let mut staged = LinkedList::from([10, 11]);
                    q.append(&mut staged).expect("open");
                    assert!(staged.is_empty());
                })
            };
            pusher.join().expect("pusher");
            splicer.join().expect("splicer");
            let late = {
                let q = q.clone();
                thread::spawn(move || q.push(99))
            };
            q.stop();
            let late = late.join().expect("late");
            let seen = consumer.join().expect("consumer");
            let ids: Vec<u32> = seen.iter().map(|s| s.0).filter(|&i| i != 99).collect();
            let (spliced, pushed): (Vec<u32>, Vec<u32>) = ids.iter().partition(|&&i| i >= 10);
            assert_eq!(pushed, [1, 2]);
            assert_eq!(spliced, [10, 11]);
            assert_eq!(late.is_ok(), seen.iter().any(|s| s.0 == 99), "run or handed back");
            assert!(seen.iter().all(|s| s.1 < 5), "pending never wraps: {seen:?}");
            assert_eq!(q.pending(), 0);
            assert_eq!(q.push(3), Err(3), "closed after the consumer exits");
        });
    }
}
