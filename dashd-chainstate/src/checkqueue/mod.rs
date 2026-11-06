//! Core's check queue (`checkqueue.h`), the script checks block validation queues on it
//! ([`script`]; A1 §3.3, CS6) and the mined commitments' BLS checks ([`bls`]; LQ2 D11).
//!
//! **What Core reports depends on the mode** (FR-CHS-10; A1 §3.3, GPT F20). With script-check
//! threads (`-par` > 1), `ConnectBlock` queues every input and reports only the queue's verdict:
//! `block-validation-failed`, once the transaction loop's own checks (`CheckTxInputs`, the
//! shared-collateral spends, the fee total, BIP68, sigops: `validation.cpp:2698-2743`) have passed
//! for every transaction, and before the checks that follow `Wait`
//! (`conflict-tx-lock`, `bad-cb-amount`, `bad-cb-payee`: `validation.cpp:2778-2826`). Without them
//! (`-par=1`) it runs each transaction's inputs inline and reports the first failing input's
//! reason at once. [`CheckQueue::control`] gives the verdict; [`script`] builds both reports.
//!
//! **Scheduling (D2, D3).** Core's master thread adds checks while it connects the block, `N`
//! workers take batches off a shared LIFO stack (at most 128 checks, fewer as the stack drains:
//! `checkqueue.h:107-113`), and at `Wait` the master joins them until the stack is empty. The
//! verdict is the AND of every check that ran, and once one fails the rest are skipped. That
//! protocol is [`Shared`], run here by `N` helper jobs on a rayon pool of `N` threads and by the
//! calling thread at the end, so the effective thread count is Core's: its workers plus the
//! caller. Batching and skipping change only how fast the verdict comes, never what it is
//! (`queue_verdict_is_the_and_of_its_checks` and the loom model).
//!
//! **Lifetimes.** A check may borrow the block, its spent coins and per-transaction data for
//! `'s`. Core stores raw pointers and keeps the data alive by hand (`txsdata` "must not be
//! invalidated until after `control` has run"); here the rayon scope that runs the helpers ends
//! before [`CheckQueue::control`] returns, so the borrow checker holds that rule.

pub mod bls;
pub mod script;

use std::cell::Cell;
use std::sync::Arc;
#[cfg(not(loom))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(loom))]
use std::sync::{Mutex, MutexGuard, PoisonError};

#[cfg(loom)]
use loom::sync::atomic::{AtomicBool, Ordering};
#[cfg(loom)]
use loom::sync::{Mutex, MutexGuard};

/// `MAX_SCRIPTCHECK_THREADS`: the most additional script-check threads `-par` can ask for.
// Core: validation.h:77 MAX_SCRIPTCHECK_THREADS
pub const MAX_SCRIPTCHECK_THREADS: usize = 15;

/// `DEFAULT_SCRIPTCHECK_THREADS`: `-par`'s default, 0 (auto).
// Core: validation.h:79 DEFAULT_SCRIPTCHECK_THREADS
pub const DEFAULT_SCRIPTCHECK_THREADS: i64 = 0;

/// The `nBatchSize` `validation.cpp` builds its queue with: the most checks one batch takes.
// Core: validation.cpp:2380 scriptcheckqueue(128)
pub const BATCH_SIZE: usize = 128;

/// Core's `-par` arithmetic: the number of **additional** script-check threads for `-par=par` on
/// a host with `num_cores` cores. `-par=0` (the default) means one fewer than the cores, `-par=-n`
/// leaves `n` cores free, and the result is clamped to `[0, 15]`; 0 means serial checks.
///
/// Core reads the option into an `int`, so `par` wraps to 32 bits first, as GCC's conversion
/// does. `num_cores` is Core's `GetNumCores()`, `std::thread::hardware_concurrency()`: libstdc++
/// returns `get_nprocs()`, the online processors, whatever the affinity mask.
// Core: init.cpp:1554-1565 (AppInitMain, script_threads)
pub fn script_check_threads(par: i64, num_cores: usize) -> usize {
    let mut threads = par as i32;
    if threads <= 0 {
        // -par=0: autodetect (cores - 1 threads); -par=-n: leave n cores free.
        threads = threads.saturating_add(i32::try_from(num_cores).unwrap_or(i32::MAX));
    }
    // The main thread counts towards the par threads.
    let threads = threads.saturating_sub(1).max(0);
    usize::try_from(threads).map_or(0, |t| t.min(MAX_SCRIPTCHECK_THREADS))
}

/// One unit of work for the queue: Core's `T::operator()()`.
pub trait Check: Send {
    /// The `nBatchSize` of the queue Core runs this check on: the most checks one batch takes.
    /// [`BATCH_SIZE`] is the script check queue's.
    const BATCH_SIZE: usize = BATCH_SIZE;

    /// Runs the check: whether it passed.
    fn check(self) -> bool;
}

/// Core's `CCheckQueue<T>` state for one [`CheckQueue::control`]: the LIFO stack of checks and
/// the verdict.
pub(crate) struct Shared<C> {
    inner: Mutex<Inner<C>>,
    /// Core's `fAllOk`, negated: set when a check fails, read before each batch runs.
    failed: AtomicBool,
    /// The pool's threads (Core's workers; the master is not counted).
    workers: usize,
}

struct Inner<C> {
    /// Core's `queue`, used as a stack: the order of the checks does not matter for the AND.
    queue: Vec<C>,
    /// Helper jobs started and not yet returned (Core's `nTotal - nIdle` among the workers).
    active: usize,
    /// Whether the master has reached `Wait` (Core counts it in `nTotal` from then on).
    master: bool,
}

impl<C: Check> Shared<C> {
    pub(crate) fn new(workers: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                queue: Vec::new(),
                active: 0,
                master: false,
            }),
            failed: AtomicBool::new(false),
            workers,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner<C>> {
        let guard = self.inner.lock();
        // No check runs under the lock, so a poisoned lock holds a consistent stack.
        #[cfg(not(loom))]
        let guard = guard.unwrap_or_else(PoisonError::into_inner);
        #[cfg(loom)]
        let guard = guard.unwrap();
        guard
    }

    /// Core's `Add`: pushes `checks` and returns how many helpers to start, at most one per idle
    /// worker and per queued check (Core wakes one worker for one check, all of them otherwise).
    // Core: checkqueue.h:163 CCheckQueue::Add
    pub(crate) fn add(&self, checks: Vec<C>) -> usize {
        if checks.is_empty() {
            return 0;
        }
        let mut inner = self.lock();
        if inner.queue.is_empty() {
            inner.queue = checks;
        } else {
            inner.queue.extend(checks);
        }
        let start = (self.workers - inner.active).min(inner.queue.len());
        inner.active += start;
        start
    }

    /// Core's `Loop`: takes batches off the top of the stack until it is empty. A helper
    /// (`helper`) leaves the active count under the same lock that saw the stack empty, so an
    /// [`Self::add`] either finds it active or starts another; the master (`!helper`, Core's
    /// `Wait`) never counts as active, only towards the batch size.
    // Core: checkqueue.h:69 CCheckQueue::Loop
    pub(crate) fn drain(&self, helper: bool) {
        let mut batch = Vec::new();
        loop {
            let mut ok = {
                let mut inner = self.lock();
                inner.master |= !helper;
                let len = inner.queue.len();
                if len == 0 {
                    if helper {
                        inner.active -= 1;
                    }
                    return;
                }
                // Increasingly smaller batches so that all finish at about the same time, counting
                // the idle workers that will start helping: Core's nTotal + nIdle + 1, where
                // nTotal is every worker, and the master once it waits.
                let idle = self.workers - inner.active;
                let total = self.workers + usize::from(inner.master);
                let now = (len / (total + idle + 1)).clamp(1, C::BATCH_SIZE);
                batch.extend(inner.queue.drain(len - now..));
                // Whether there is work to do at all: Core's `fOk = fAllOk`.
                !self.failed.load(Ordering::Acquire)
            };
            for check in batch.drain(..) {
                if ok {
                    ok = check.check();
                }
            }
            if !ok {
                self.failed.store(true, Ordering::Release);
            }
        }
    }

    /// Whether every check that ran passed (Core's `fAllOk` at `Wait`).
    pub(crate) fn all_ok(&self) -> bool {
        !self.failed.load(Ordering::Acquire)
    }
}

/// Core's `scriptcheckqueue`: the pool of script-check threads, or none for serial checks.
#[derive(Debug)]
pub struct CheckQueue {
    pool: Option<rayon::ThreadPool>,
    /// Core's `m_control_mutex`: one [`Self::control`] over the pool at a time.
    control: std::sync::Mutex<()>,
    /// How many controls over the pool were opened (unit tests only).
    #[cfg(test)]
    controls: std::sync::atomic::AtomicUsize,
}

impl CheckQueue {
    /// A queue with `threads` additional threads (see [`script_check_threads`]), named
    /// `d-scriptch.<n>` from 0 as Core's are (`util::ThreadRename` prefixes `d-`); none makes
    /// every check serial.
    ///
    /// # Errors
    ///
    /// When the threads cannot be started.
    // Core: checkqueue.h:139-154 CCheckQueue::StartWorkerThreads, util/threadnames.cpp:59-63
    pub fn new(threads: usize) -> Result<Self, rayon::ThreadPoolBuildError> {
        if threads == 0 {
            return Ok(Self::serial());
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|n| format!("d-scriptch.{n}"))
            .build()?;
        Ok(Self {
            pool: Some(pool),
            control: std::sync::Mutex::new(()),
            #[cfg(test)]
            controls: Default::default(),
        })
    }

    /// A queue without threads (`-par=1`).
    pub fn serial() -> Self {
        Self {
            pool: None,
            control: std::sync::Mutex::new(()),
            #[cfg(test)]
            controls: Default::default(),
        }
    }

    /// Core's `HasThreads()`: whether `ConnectBlock` runs its script checks in parallel.
    // Core: checkqueue.h:194 CCheckQueue::HasThreads
    pub fn has_threads(&self) -> bool {
        self.pool.is_some()
    }

    /// How many controls over the pool [`Self::control`] has opened (unit tests only).
    #[cfg(test)]
    pub(crate) fn controls(&self) -> usize {
        self.controls.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The additional threads.
    pub fn threads(&self) -> usize {
        self.pool.as_ref().map_or(0, rayon::ThreadPool::current_num_threads)
    }

    /// Core's `CCheckQueueControl` over one block: `f` adds checks through the [`Control`], the
    /// pool runs them meanwhile, and once `f` returns the caller joins in until every queued
    /// check has run or been skipped (Core's `Wait`). Returns `f`'s result and the verdict:
    /// whether every check passed.
    ///
    /// Core waits for the queue even when `ConnectBlock` returns early (the control's
    /// destructor), and so does this: a non-script failure `f` reports still lets the queued
    /// checks finish, and the caller ignores the verdict.
    ///
    /// Without threads, [`Control::add`] runs each check at once on the caller. With them, one
    /// control runs at a time, as Core's `m_control_mutex` makes it; `f` must not open another
    /// on the same queue.
    ///
    /// # Panics
    ///
    /// When `f` or a check panics, once the running jobs have returned; the rest of the
    /// panicking check's batch is dropped unrun, and no verdict is reported.
    // Core: checkqueue.h:207 CCheckQueueControl (constructor, Wait, destructor)
    pub fn control<'s, C, R, F>(&self, f: F) -> (R, bool)
    where
        C: Check + 's,
        F: FnOnce(&Control<'_, 's, C>) -> R,
    {
        let Some(pool) = &self.pool else {
            return self.control_inline(f);
        };
        // Nothing that panics under the lock leaves state behind: the shared state is per control.
        let _one = self.control.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(test)]
        self.controls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let shared = Arc::new(Shared::new(pool.current_num_threads()));
        let r = pool.in_place_scope(|scope: &rayon::Scope<'s>| {
            let control = Control {
                mode: Mode::Queue {
                    scope,
                    shared: &shared,
                },
            };
            let r = f(&control);
            // Core's `Wait`: the master joins the workers until the stack is empty; the scope then
            // waits for the batches still running.
            shared.drain(false);
            r
        });
        (r, shared.all_ok())
    }

    /// Core's `CCheckQueueControl` built without a queue (`nullptr`), whatever this queue's
    /// threads: [`Self::control`]'s inline mode. `f` adds checks through the [`Control`], each runs
    /// at once on the caller ([`Control::is_parallel`] is false), and the verdict is whether every
    /// check passed. It takes no pool and not the control lock, so it never waits for another
    /// control on this queue.
    ///
    /// `ConnectBlock` builds its control without the queue when script checks are off
    /// (`fScriptChecks && parallel_script_checks ? &scriptcheckqueue : nullptr`): below
    /// `-assumevalid` nothing is added, and the control only scopes the transaction loop (CH3
    /// D41).
    ///
    /// # Panics
    ///
    /// When `f` or a check panics; no verdict is reported.
    // Core: checkqueue.h:217 CCheckQueueControl (pqueue == nullptr); validation.cpp:2649
    pub fn control_inline<'s, C, R, F>(&self, f: F) -> (R, bool)
    where
        C: Check + 's,
        F: FnOnce(&Control<'_, 's, C>) -> R,
    {
        let failed = Cell::new(false);
        let control = Control {
            mode: Mode::Inline {
                failed: &failed,
            },
        };
        let r = f(&control);
        (r, !failed.get())
    }
}

/// The adding side of one [`CheckQueue::control`] (Core's `CCheckQueueControl`).
pub struct Control<'q, 's, C> {
    mode: Mode<'q, 's, C>,
}

enum Mode<'q, 's, C> {
    /// No threads: checks run as they are added.
    Inline {
        failed: &'q Cell<bool>,
    },
    Queue {
        scope: &'q rayon::Scope<'s>,
        shared: &'q Arc<Shared<C>>,
    },
}

impl<'s, C: Check + 's> Control<'_, 's, C> {
    /// Whether the checks run on the queue's threads (Core passes `pvChecks` to
    /// `CheckInputScripts` exactly then).
    pub fn is_parallel(&self) -> bool {
        matches!(self.mode, Mode::Queue { .. })
    }

    /// Core's `Add`: queues `checks`. Without threads they run here and now, skipped once one
    /// has failed (Core never adds without threads; this keeps the verdict the same).
    // Core: checkqueue.h:234 CCheckQueueControl::Add
    pub fn add(&self, checks: Vec<C>) {
        match &self.mode {
            Mode::Inline {
                failed,
            } => {
                for check in checks {
                    if !failed.get() && !check.check() {
                        failed.set(true);
                    }
                }
            }
            Mode::Queue {
                scope,
                shared,
            } => {
                for _ in 0..shared.add(checks) {
                    let shared = Arc::clone(shared);
                    scope.spawn(move |_| shared.drain(true));
                }
            }
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests;

#[cfg(all(test, loom))]
mod loom_tests;
