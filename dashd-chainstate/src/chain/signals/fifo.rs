//! The queue under `ValidationSignals` (§8.2, R2-M2): Core's `SingleThreadedSchedulerClient`
//! list with every critical section O(1) under its leaf mutex L2s (DEC-152).
//!
//! - **Enqueue** builds its one-node list before the lock; under L2s it only appends (pointer
//!   writes) and counts; the wake is a condvar notify after the unlock.
//! - **Dequeue** takes the whole list under L2s (`mem::take`) and runs it outside the lock, so
//!   nodes are freed outside it too.
//! - **`pending`** counts what is queued or detached and not yet started: Core's
//!   `CallbacksPending()` (`scheduler.cpp:196-200`), from which only the running callback has
//!   been popped. A held entry counts as running.
//! - **Holds** (LK1a F): an entry that hands its work to another thread returns
//!   [`Step::Hold`]; the consumer runs nothing else until [`Fifo::complete`] stores that
//!   ticket's [`Outcome`] in its own slot, beside the list, then resumes the entry. Core runs the
//!   same work inside the callback (`net_processing.cpp:2167-2169` at v24.0.0), so no later
//!   callback overtakes it.
//!
//! Generic over the entry so the loom model (`--cfg loom`) runs it without the listeners.

use std::any::Any;
use std::collections::LinkedList;
use std::panic::{self, AssertUnwindSafe};

#[cfg(loom)]
use loom::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
#[cfg(loom)]
use loom::sync::{Condvar, Mutex, MutexGuard};
#[cfg(not(loom))]
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
#[cfg(not(loom))]
use std::sync::{Condvar, Mutex, MutexGuard};

struct State<T> {
    list: LinkedList<T>,
    /// The consumer was asked to stop: it runs what is queued, then closes.
    stopping: bool,
    /// The consumer has exited: later entries are handed back to their producer.
    closed: bool,
    /// The ticket the consumer holds an entry on.
    held: Option<u64>,
    /// A ticket's outcome, stored here and not in `list`, so a full list never delays it.
    completion: Option<(u64, Outcome)>,
}

/// How a held entry's work ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The work ran to its end, its own early returns included.
    Done,
    /// The work stopped because V had stopped.
    Stopped,
    /// The work was dropped unfinished: its worker panicked or exited.
    Aborted,
}

/// What running an entry asks of the consumer.
pub(crate) enum Step<R> {
    /// The entry is done.
    Done,
    /// Wait for `ticket`'s outcome, then run `resume`.
    Hold {
        ticket: u64,
        resume: R,
    },
}

/// What the consumer runs: a queued entry, or a held one's rest.
pub(crate) enum Work<T, R> {
    Entry(T),
    Resume(R),
}

/// What `on_panic` hears when the consumer refuses a held entry's outcome.
#[derive(Debug)]
pub(crate) struct Refused(pub(crate) Outcome);

pub(crate) struct Fifo<T> {
    /// L2s, a DEC-152 leaf.
    state: Mutex<State<T>>,
    pending: AtomicUsize,
    /// Notification only (DEC-152 (2)).
    wake: Condvar,
    tickets: AtomicU64,
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
                held: None,
                completion: None,
            }),
            pending: AtomicUsize::new(0),
            wake: Condvar::new(),
            tickets: AtomicU64::new(0),
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

    /// A new ticket for [`Step::Hold`] and [`Fifo::complete`].
    pub(crate) fn ticket(&self) -> u64 {
        self.tickets.fetch_add(1, Ordering::SeqCst)
    }

    /// Stores `ticket`'s outcome and wakes the consumer. Ignored once the queue has closed, or
    /// while the consumer holds another ticket.
    pub(crate) fn complete(&self, ticket: u64, outcome: Outcome) {
        let stored = self.leaf(|mut g| {
            if g.closed || g.held.is_some_and(|held| held != ticket) {
                return false;
            }
            g.completion = Some((ticket, outcome));
            true
        });
        if stored {
            self.wake.notify_one();
        }
    }

    /// The consumer's wait for `ticket`'s outcome, whatever else is queued or a stop asked for.
    fn wait_for(&self, ticket: u64) -> Outcome {
        self.leaf(|mut g| {
            g.held = Some(ticket);
            loop {
                if let Some((id, outcome)) = g.completion
                    && id == ticket
                {
                    g.completion = None;
                    g.held = None;
                    return outcome;
                }
                g = self.wake.wait(g).unwrap_or_else(|p| p.into_inner());
            }
        })
    }

    /// The consumer: runs every entry in FIFO order on the calling thread until [`Fifo::stop`],
    /// then runs what is left and closes. An entry that returns [`Step::Hold`] is resumed once
    /// its ticket completes and `accept` takes the outcome; nothing else runs meanwhile, also
    /// after a stop. If `run` panics, `on_panic` hears it first (so the node is stopping before
    /// any waiter learns the queue is gone); then the queue closes, even if `on_panic` panicked:
    /// every entry left is dropped (outside L2s, so an entry's `Drop` must not panic) and
    /// uncounted, so barriers disconnect and later entries come back to their producers; then
    /// the panic continues, `on_panic`'s if it panicked (Core's scheduler thread ends on an
    /// exception, and the process with it). An outcome `accept` refuses is handled as a panic,
    /// with [`Refused`] as its payload, except that the consumer then returns.
    pub(crate) fn serve<R>(
        &self,
        mut run: impl FnMut(Work<T, R>) -> Step<R>,
        mut accept: impl FnMut(Outcome) -> bool,
        on_panic: impl FnOnce(&(dyn Any + Send)),
    ) {
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
                let mut work = Work::Entry(e);
                loop {
                    let ran = panic::catch_unwind(AssertUnwindSafe(|| run(work)));
                    let (ticket, resume) = match ran {
                        Ok(Step::Done) => break,
                        Ok(Step::Hold {
                            ticket,
                            resume,
                        }) => (ticket, resume),
                        Err(p) => {
                            // Closes even if `on_panic` panics too, which would otherwise leave
                            // every waiter on a queue nobody serves.
                            let close = CloseOnDrop {
                                fifo: self,
                                batch: &mut batch,
                            };
                            on_panic(p.as_ref());
                            drop(close);
                            panic::resume_unwind(p);
                        }
                    };
                    let outcome = self.wait_for(ticket);
                    if !accept(outcome) {
                        drop(resume);
                        let close = CloseOnDrop {
                            fifo: self,
                            batch: &mut batch,
                        };
                        on_panic(&Refused(outcome));
                        drop(close);
                        return;
                    }
                    work = Work::Resume(resume);
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
/// never wraps and ends at zero. LK1a F's hold: a completer racing the hold, a producer and
/// `stop`; the held entry resumes before anything queued after it, `pending` excludes it.
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
                    q.serve(
                        |w| {
                            if let Work::Entry(e) = w {
                                seen.push((e, q.pending()));
                            }
                            Step::<()>::Done
                        },
                        |_| true,
                        |_| (),
                    );
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

    #[test]
    fn loom_hold_completes_before_later_entries_and_the_stop() {
        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(2);
        model.check(|| {
            let q = Arc::new(Fifo::<u32>::new());
            let ticket = q.ticket();
            q.push(1).expect("open");
            let consumer = {
                let q = q.clone();
                thread::spawn(move || {
                    let mut seen = Vec::new();
                    q.serve(
                        |w| match w {
                            Work::Entry(1) => {
                                seen.push((1, q.pending()));
                                Step::Hold {
                                    ticket,
                                    resume: 100,
                                }
                            }
                            Work::Entry(e) | Work::Resume(e) => {
                                seen.push((e, q.pending()));
                                Step::Done
                            }
                        },
                        |outcome| outcome == Outcome::Done,
                        |_| panic!("accepted"),
                    );
                    seen
                })
            };
            let completer = {
                let q = q.clone();
                thread::spawn(move || q.complete(ticket, Outcome::Done))
            };
            let pusher = {
                let q = q.clone();
                thread::spawn(move || q.push(2).expect("open"))
            };
            pusher.join().expect("pusher");
            q.stop();
            completer.join().expect("completer");
            let seen = consumer.join().expect("consumer");
            let ids: Vec<u32> = seen.iter().map(|s| s.0).collect();
            assert_eq!(ids, [1, 100, 2], "the held entry resumes first");
            assert!(seen[1].1 <= 1, "the held entry is not pending: {seen:?}");
            assert_eq!(q.pending(), 0);
        });
    }
}

/// LK1a F's hold on a plain queue: what runs while a ticket is outstanding, and what an outcome
/// does.
#[cfg(all(test, not(loom)))]
mod tests {
    // Test gates and waits use raw channels; LO-9's ban is for the crate's own wait paths.
    #![allow(clippy::disallowed_types, clippy::disallowed_methods)]

    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::*;

    const SOON: Duration = Duration::from_secs(120);

    /// What the consumer ran: an entry or a resume, with `pending()` as it started.
    #[derive(Debug, PartialEq, Eq)]
    enum Ran {
        Entry(u32, usize),
        Resume(u32, usize),
    }

    type Log = Arc<Mutex<Vec<Ran>>>;

    /// A consumer whose entry `held` holds on `ticket`; `entered` hears each run. Returns what
    /// ran, and whether `on_panic` heard a refused outcome and which.
    fn consumer(
        q: &Arc<Fifo<u32>>,
        held: u32,
        ticket: u64,
        entered: mpsc::Sender<u32>,
    ) -> (Log, thread::JoinHandle<Option<Outcome>>) {
        let log = Log::default();
        let (q, out) = (q.clone(), log.clone());
        let join = thread::spawn(move || {
            let mut refused = None;
            q.serve(
                |w| {
                    let pending = q.pending();
                    match w {
                        Work::Entry(e) => {
                            out.lock().unwrap().push(Ran::Entry(e, pending));
                            let _ = entered.send(e);
                            if e == held {
                                return Step::Hold {
                                    ticket,
                                    resume: e,
                                };
                            }
                        }
                        Work::Resume(e) => out.lock().unwrap().push(Ran::Resume(e, pending)),
                    }
                    Step::Done
                },
                |outcome| outcome == Outcome::Done,
                |p| refused = p.downcast_ref::<Refused>().map(|r| r.0),
            );
            refused
        });
        (log, join)
    }

    /// The hold keeps every later entry, queued before or after it, behind the completion;
    /// the completion takes its own slot, so a long list does not delay it; `pending` counts the
    /// later entries and not the held one.
    #[test]
    fn a_held_entry_resumes_before_anything_queued_after_it() {
        let q = Arc::new(Fifo::new());
        let ticket = q.ticket();
        let (entered, rx) = mpsc::channel();
        q.push(1).unwrap();
        q.push(2).unwrap();
        let (log, join) = consumer(&q, 1, ticket, entered);
        assert_eq!(rx.recv_timeout(SOON), Ok(1));
        for e in 3..1_003 {
            q.push(e).unwrap();
        }
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        assert_eq!(q.pending(), 1_001, "the batch's 2 and the 1,000 queued, not the held 1");
        q.complete(ticket, Outcome::Done);
        assert_eq!(rx.recv_timeout(SOON), Ok(2));
        q.stop();
        assert_eq!(join.join().unwrap(), None);
        let log = log.lock().unwrap();
        assert_eq!(log[..3], [Ran::Entry(1, 1), Ran::Resume(1, 1_001), Ran::Entry(2, 1_000)]);
        assert_eq!(log.len(), 1_003);
        assert_eq!(q.pending(), 0);
    }

    /// A completion made before the consumer reaches the hold is kept for it.
    #[test]
    fn an_early_completion_is_kept_for_the_hold() {
        let q = Arc::new(Fifo::new());
        let ticket = q.ticket();
        q.complete(ticket, Outcome::Done);
        q.push(1).unwrap();
        q.push(2).unwrap();
        let (entered, _rx) = mpsc::channel();
        let (log, join) = consumer(&q, 1, ticket, entered);
        q.stop();
        assert_eq!(join.join().unwrap(), None);
        assert_eq!(*log.lock().unwrap(), [Ran::Entry(1, 1), Ran::Resume(1, 1), Ran::Entry(2, 0)]);
    }

    /// A stop asked for while an entry is held waits for its completion, then runs the rest.
    #[test]
    fn a_stop_while_held_waits_for_the_completion_then_drains() {
        let q = Arc::new(Fifo::new());
        let ticket = q.ticket();
        let (entered, rx) = mpsc::channel();
        q.push(1).unwrap();
        let (log, join) = consumer(&q, 1, ticket, entered);
        assert_eq!(rx.recv_timeout(SOON), Ok(1));
        q.push(2).unwrap();
        q.stop();
        thread::sleep(Duration::from_millis(100));
        assert!(!join.is_finished(), "the stop waits for the held entry");
        assert_eq!(q.push(3), Ok(()), "still open");
        q.complete(ticket, Outcome::Done);
        assert_eq!(join.join().unwrap(), None);
        let ran: Vec<_> =
            log.lock().unwrap().iter().map(|r| matches!(r, Ran::Resume(..))).collect();
        assert_eq!(ran, [false, true, false, false]);
        assert_eq!(q.push(4), Err(4), "closed after the drain");
    }

    /// An outcome `accept` refuses (an aborted ticket) is a panic's path without the unwind:
    /// `on_panic` hears [`Refused`], the held entry is not resumed, the queue closes and drops
    /// what is left, and a later completion is ignored.
    #[test]
    fn a_refused_outcome_closes_the_queue_as_a_panic_does() {
        for outcome in [Outcome::Aborted, Outcome::Stopped] {
            let q = Arc::new(Fifo::new());
            let ticket = q.ticket();
            let (entered, rx) = mpsc::channel();
            q.push(1).unwrap();
            q.push(2).unwrap();
            let (log, join) = consumer(&q, 1, ticket, entered);
            assert_eq!(rx.recv_timeout(SOON), Ok(1));
            q.complete(ticket, outcome);
            assert_eq!(join.join().unwrap(), Some(outcome));
            assert_eq!(*log.lock().unwrap(), [Ran::Entry(1, 1)]);
            assert_eq!(q.pending(), 0, "the batch's 2 was dropped and uncounted");
            assert_eq!(q.push(3), Err(3));
            q.complete(ticket, Outcome::Done);
        }
    }

    /// While a ticket is held, another ticket's completion is not taken for it.
    #[test]
    fn another_tickets_completion_does_not_release_the_hold() {
        let q = Arc::new(Fifo::new());
        let (other, ticket) = (q.ticket(), q.ticket());
        let (entered, rx) = mpsc::channel();
        q.push(1).unwrap();
        let (log, join) = consumer(&q, 1, ticket, entered);
        assert_eq!(rx.recv_timeout(SOON), Ok(1));
        q.complete(other, Outcome::Aborted);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(log.lock().unwrap().len(), 1, "still held");
        q.complete(ticket, Outcome::Done);
        q.stop();
        assert_eq!(join.join().unwrap(), None);
        assert_eq!(log.lock().unwrap().len(), 2);
    }
}
