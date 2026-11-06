//! CH5-5's live-copy tripwire (§2.2) and the node's test-hook events (§6.2): monotonic counters
//! of the work that may change a copied datadir's files, process-wide. Feature `test-hooks` only,
//! so shipping builds have neither the counters nor their increments.
//!
//! A copy point snapshots the tripwire counters inside its barrier, while nothing else can count
//! (§2.2); the test reads them again after copying, and any increase fails it.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::Duration;

use dash_kv::LiveVerdict;

/// The process's counters.
pub static COUNTERS: Counters = Counters::new();

thread_local! {
    /// `cl_turned_on_reset` counted on this thread: an exact delta for a unit test whose actor
    /// thread is its own, while other tests in the binary count too.
    static TURNED_ON_RESETS_HERE: Cell<u64> = const { Cell::new(0) };
}

/// Monotonic counters. The first four are the tripwire (§2.2); the rest are events the test hooks
/// wait on (§6.2).
#[derive(Debug)]
pub struct Counters {
    /// Blocks connected to the active chain (`ConnectTip`).
    activations: AtomicU64,
    /// Blocks disconnected from it, counted once per disconnect on every route.
    disconnects: AtomicU64,
    /// Calls that create, write, preallocate, truncate, replace or remove a block or undo file.
    flatfile_mutations: AtomicU64,
    /// Undo records written.
    undo_writes: AtomicU64,
    /// ChainLock enforcement's off→on reset ran.
    cl_turned_on_reset: AtomicU64,
    /// ChainLock enforcement ran on a best ChainLock.
    cl_enforced: AtomicU64,
    /// The block hash of the last enforced ChainLock.
    cl_enforced_floor: Mutex<Option<[u8; 32]>>,
    /// Flush jobs whose last step returned.
    flush_jobs_done: AtomicU64,
    /// The last [`JOBS_KEPT`] jobs W threads finished, oldest first.
    jobs_done: Mutex<VecDeque<JobDone>>,
    /// The `flush` hook's tickets, from request to the job that carried them.
    scripted: Mutex<ScriptedTickets>,
    /// Scripted jobs frozen: handed to W.
    scripted_frozen: AtomicU64,
    /// Where V parks right after taking a scripted request, when a test arms it.
    take_park: ParkPoint,
    /// Every copy point's barrier verdict since start.
    verdicts: Mutex<Vec<LiveVerdict>>,
}

/// The tripwire counters at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tripwire {
    pub activations: u64,
    pub disconnects: u64,
    pub flatfile_mutations: u64,
    pub undo_writes: u64,
}

/// A copy point's record (§2.2): the tripwire counters its barrier snapshotted while nothing
/// else could count, and each column family's super version the barrier ended at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CopyPointSnapshot {
    pub tripwire: Tripwire,
    pub super_versions: Vec<(&'static str, u64)>,
}

/// How many finished jobs [`Counters::jobs_done`] keeps.
pub const JOBS_KEPT: usize = 64;

/// A flush job W finished (§6.2 `JobDone`): the count of jobs done with it, its steps in order,
/// whether a test scripted it and the scripted tickets it carried, and the W thread that ran it
/// (a test binary runs many).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobDone {
    pub done: u64,
    pub steps: Vec<String>,
    pub scripted: bool,
    pub tickets: Vec<u64>,
    pub thread: ThreadId,
}

/// The scripted-flush tickets under one lock (Sol r2 C-3): V's claim of a request and its record
/// as taken are one step, so no caller sees a request that is neither requested nor taken; each
/// job names the tickets it carried. Every critical section is a few loads and stores, and no
/// caller holds the lock across a wait, so V never waits on a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedTickets {
    /// The last ticket issued.
    pub issued: u64,
    /// The published request V has not taken.
    pub requested: Option<u64>,
    /// Tickets V took that no job has frozen yet.
    pub taken: Vec<u64>,
    /// Tickets frozen into a job W has not finished.
    pub running: Vec<u64>,
    /// The last ticket V refused: its coins had no best block, so the flush could write only the
    /// index and no scripted job would carry it (Opus r1 O-3).
    pub refused: Option<u64>,
}

/// Where a scripted ticket is (§6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketState {
    /// Published, not taken by V.
    Requested,
    /// Taken by V into a flush decision; its job is not frozen yet.
    Taken,
    /// Frozen into a job W has not finished.
    Running,
    /// Its job finished, or the caller withdrew it before V took it.
    Done,
    /// V refused it: no best block, so no scripted job.
    Refused,
}

/// How long a [`ParkPoint`] holds a thread at most, so a failing test cannot hang.
pub const PARK_LIMIT: Duration = Duration::from_secs(30);

/// A test's one-shot park point: once armed, the next thread to pass it waits there until the
/// test releases it, or [`PARK_LIMIT`].
#[derive(Debug)]
pub struct ParkPoint {
    state: Mutex<Park>,
    changed: Condvar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Park {
    Idle,
    Armed,
    Parked,
    Released,
}

impl ParkPoint {
    const fn new() -> Self {
        Self {
            state: Mutex::new(Park::Idle),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Park> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Parks the next thread that passes.
    pub fn arm(&self) {
        *self.lock() = Park::Armed;
    }

    /// Whether a thread parked within `limit`.
    pub fn wait_parked(&self, limit: Duration) -> bool {
        let state = self.lock();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, limit, |p| *p == Park::Armed)
            .unwrap_or_else(|e| e.into_inner());
        *state == Park::Parked
    }

    /// Lets the parked thread go, or disarms the point if none parked.
    pub fn release(&self) {
        let mut state = self.lock();
        *state = if *state == Park::Parked {
            Park::Released
        } else {
            Park::Idle
        };
        self.changed.notify_all();
    }

    fn pass(&self) {
        let mut state = self.lock();
        if *state != Park::Armed {
            return;
        }
        *state = Park::Parked;
        self.changed.notify_all();
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, PARK_LIMIT, |p| *p == Park::Parked)
            .unwrap_or_else(|e| e.into_inner());
        *state = Park::Idle;
    }
}

/// The event counters at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Events {
    pub cl_turned_on_reset: u64,
    pub cl_enforced: u64,
    pub cl_enforced_floor: Option<[u8; 32]>,
    pub flush_jobs_done: u64,
}

impl Counters {
    const fn new() -> Self {
        Self {
            activations: AtomicU64::new(0),
            disconnects: AtomicU64::new(0),
            flatfile_mutations: AtomicU64::new(0),
            undo_writes: AtomicU64::new(0),
            cl_turned_on_reset: AtomicU64::new(0),
            cl_enforced: AtomicU64::new(0),
            cl_enforced_floor: Mutex::new(None),
            flush_jobs_done: AtomicU64::new(0),
            jobs_done: Mutex::new(VecDeque::new()),
            scripted: Mutex::new(ScriptedTickets {
                issued: 0,
                requested: None,
                taken: Vec::new(),
                running: Vec::new(),
                refused: None,
            }),
            scripted_frozen: AtomicU64::new(0),
            take_park: ParkPoint::new(),
            verdicts: Mutex::new(Vec::new()),
        }
    }

    /// Counts a connected block; returns the new count.
    pub fn activation(&self) -> u64 {
        self.activations.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn disconnect(&self) {
        self.disconnects.fetch_add(1, Ordering::SeqCst);
    }

    pub fn flatfile_mutation(&self) {
        self.flatfile_mutations.fetch_add(1, Ordering::SeqCst);
    }

    pub fn undo_write(&self) {
        self.undo_writes.fetch_add(1, Ordering::SeqCst);
    }

    pub fn cl_turned_on_reset(&self) {
        self.cl_turned_on_reset.fetch_add(1, Ordering::SeqCst);
        TURNED_ON_RESETS_HERE.with(|n| n.set(n.get() + 1));
    }

    /// The `cl_turned_on_reset` increments made on the calling thread.
    pub fn cl_turned_on_resets_on_this_thread(&self) -> u64 {
        TURNED_ON_RESETS_HERE.with(Cell::get)
    }

    /// Records an enforcement of the ChainLock on block `floor`.
    pub fn cl_enforced(&self, floor: [u8; 32]) {
        *self.cl_enforced_floor.lock().unwrap_or_else(|e| e.into_inner()) = Some(floor);
        self.cl_enforced.fetch_add(1, Ordering::SeqCst);
    }

    /// Counts a job whose last step returned, on the W thread that ran it; `scripted` holds the
    /// tickets of a scripted job. Its tickets are done once its record is kept: both under the
    /// jobs' lock, so a caller that sees its ticket done finds the record.
    pub fn flush_job_done(&self, steps: Vec<String>, scripted: Option<Vec<u64>>) {
        let mut jobs = self.jobs_done.lock().unwrap_or_else(|e| e.into_inner());
        let done = self.flush_jobs_done.fetch_add(1, Ordering::SeqCst) + 1;
        let tickets = scripted.clone().unwrap_or_default();
        self.scripted_lock().running.retain(|t| !tickets.contains(t));
        if jobs.len() == JOBS_KEPT {
            jobs.pop_front();
        }
        jobs.push_back(JobDone {
            done,
            steps,
            scripted: scripted.is_some(),
            tickets,
            thread: std::thread::current().id(),
        });
    }

    /// The last jobs W threads finished, oldest first.
    pub fn jobs_done(&self) -> Vec<JobDone> {
        self.jobs_done.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
    }

    fn scripted_lock(&self) -> MutexGuard<'_, ScriptedTickets> {
        self.scripted.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publishes a request for V's next `FlushStateToDisk` call to take; returns its ticket.
    pub fn request_scripted_flush(&self) -> u64 {
        let mut s = self.scripted_lock();
        s.issued += 1;
        let ticket = s.issued;
        s.requested = Some(ticket);
        ticket
    }

    /// V's take: whether a scripted flush was asked for. Claiming the request and recording its
    /// ticket as taken are one step (Sol r2 C-3). With no best block the flush writes only the
    /// index, so V refuses the request instead (Opus r1 O-3).
    pub fn take_scripted_flush(&self, has_best_block: bool) -> bool {
        let mut s = self.scripted_lock();
        let Some(ticket) = s.requested.take() else {
            return false;
        };
        if !has_best_block {
            s.refused = Some(ticket);
            return false;
        }
        s.taken.push(ticket);
        drop(s);
        // A test parks V here, right after the claim (Sol r2 C-3's interleaving).
        self.take_park.pass();
        true
    }

    /// The caller's withdrawal of `ticket` if V has not taken it; whether it was still untaken.
    /// A taken ticket stays outstanding until its job is done.
    pub fn withdraw_scripted_flush(&self, ticket: u64) -> bool {
        let mut s = self.scripted_lock();
        let untaken = s.requested == Some(ticket);
        if untaken {
            s.requested = None;
        }
        untaken
    }

    /// Freezes a scripted job for W: counts it and moves every taken ticket into it.
    pub fn scripted_freeze(&self) -> Vec<u64> {
        self.scripted_frozen.fetch_add(1, Ordering::SeqCst);
        let mut s = self.scripted_lock();
        let tickets = std::mem::take(&mut s.taken);
        s.running.extend_from_slice(&tickets);
        tickets
    }

    /// The scripted jobs frozen since start.
    pub fn scripted_frozen(&self) -> u64 {
        self.scripted_frozen.load(Ordering::SeqCst)
    }

    /// Whether any ticket is requested, taken or running: a new call waits for it, so it never
    /// sees that ticket's job as its own.
    pub fn scripted_outstanding(&self) -> bool {
        let s = self.scripted_lock();
        s.requested.is_some() || !s.taken.is_empty() || !s.running.is_empty()
    }

    /// Where `ticket` is.
    pub fn ticket_state(&self, ticket: u64) -> TicketState {
        let s = self.scripted_lock();
        if s.requested == Some(ticket) {
            TicketState::Requested
        } else if s.taken.contains(&ticket) {
            TicketState::Taken
        } else if s.running.contains(&ticket) {
            TicketState::Running
        } else if s.refused == Some(ticket) {
            TicketState::Refused
        } else {
            TicketState::Done
        }
    }

    /// The tickets at this moment.
    pub fn scripted_tickets(&self) -> ScriptedTickets {
        self.scripted_lock().clone()
    }

    /// The finished job that carried `ticket`, if it is still kept.
    pub fn scripted_job(&self, ticket: u64) -> Option<JobDone> {
        let jobs = self.jobs_done.lock().unwrap_or_else(|e| e.into_inner());
        jobs.iter().rev().find(|j| j.tickets.contains(&ticket)).cloned()
    }

    /// The point V passes right after taking a scripted request.
    pub fn take_park(&self) -> &ParkPoint {
        &self.take_park
    }

    pub fn record_verdict(&self, verdict: LiveVerdict) {
        self.verdicts.lock().unwrap_or_else(|e| e.into_inner()).push(verdict);
    }

    pub fn tripwire(&self) -> Tripwire {
        Tripwire {
            activations: self.activations.load(Ordering::SeqCst),
            disconnects: self.disconnects.load(Ordering::SeqCst),
            flatfile_mutations: self.flatfile_mutations.load(Ordering::SeqCst),
            undo_writes: self.undo_writes.load(Ordering::SeqCst),
        }
    }

    pub fn events(&self) -> Events {
        Events {
            cl_turned_on_reset: self.cl_turned_on_reset.load(Ordering::SeqCst),
            cl_enforced: self.cl_enforced.load(Ordering::SeqCst),
            cl_enforced_floor: *self.cl_enforced_floor.lock().unwrap_or_else(|e| e.into_inner()),
            flush_jobs_done: self.flush_jobs_done.load(Ordering::SeqCst),
        }
    }

    pub fn verdicts(&self) -> Vec<LiveVerdict> {
        self.verdicts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opus r1 O-3: with no best block V refuses the request, which leaves nothing outstanding
    /// and names the ticket refused; the next request is taken and done as usual.
    #[test]
    fn a_request_with_no_best_block_is_refused() {
        let c = Counters::new();
        let refused = c.request_scripted_flush();
        assert!(!c.take_scripted_flush(false));
        assert_eq!(c.ticket_state(refused), TicketState::Refused);
        assert!(!c.scripted_outstanding());
        let taken = c.request_scripted_flush();
        assert!(c.take_scripted_flush(true));
        assert_eq!(c.ticket_state(taken), TicketState::Taken);
        assert_eq!(c.scripted_freeze(), vec![taken]);
        c.flush_job_done(Vec::new(), Some(vec![taken]));
        assert_eq!(c.ticket_state(taken), TicketState::Done);
        assert_eq!(c.ticket_state(refused), TicketState::Refused);
        assert_eq!(c.scripted_job(taken).map(|j| j.tickets), Some(vec![taken]));
    }
}
