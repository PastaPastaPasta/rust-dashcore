//! The shutdown request (`shutdown.cpp`): who asked to stop, the exit status, and whether an
//! `AbortNode` ran (its shutdown still flushes, DEC-241).

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use dashd_chainstate::chain::ShutdownFlag;

/// `EXIT_SUCCESS` and `EXIT_FAILURE`.
pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;

/// Core's shutdown globals (`fRequestShutdown`, `fRequestRestart`, `g_exit_status`, and the
/// self-pipe `WaitForShutdown` blocks on), one per node.
///
/// Core's `StartShutdown` must be async-signal-safe; this one takes a lock. Signals reach it
/// from a thread that waits for them, never from a handler (`rdashd`'s `signals`).
#[derive(Debug)]
pub struct ShutdownState {
    requested: Mutex<bool>,
    wake: Condvar,
    /// `requested` for lock-free reads (`ShutdownRequested` is polled in loops).
    requested_flag: AtomicBool,
    restart: AtomicBool,
    exit_status: AtomicI32,
    aborted: AtomicBool,
}

impl Default for ShutdownState {
    fn default() -> Self {
        ShutdownState::new()
    }
}

impl ShutdownState {
    /// Core's `InitShutdownState`, with the exit status at `EXIT_SUCCESS`.
    pub fn new() -> ShutdownState {
        ShutdownState {
            requested: Mutex::new(false),
            wake: Condvar::new(),
            requested_flag: AtomicBool::new(false),
            restart: AtomicBool::new(false),
            exit_status: AtomicI32::new(EXIT_SUCCESS),
            aborted: AtomicBool::new(false),
        }
    }

    /// Core's `StartShutdown`: idempotent, from any thread.
    // Core: shutdown.cpp:66-84
    pub fn start_shutdown(&self) {
        let mut requested = self.requested.lock().unwrap_or_else(PoisonError::into_inner);
        *requested = true;
        self.requested_flag.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    /// Core's `StartRestart` (the GUI's; kept for the `RestartRequested` it sets).
    pub fn start_restart(&self) {
        self.restart.store(true, Ordering::SeqCst);
        self.start_shutdown();
    }

    /// Core's `AbortShutdown`: withdraws a request. Only during init, before anything waits.
    // Core: shutdown.cpp:91-99
    pub fn abort_shutdown(&self) {
        let mut requested = self.requested.lock().unwrap_or_else(PoisonError::into_inner);
        *requested = false;
        self.requested_flag.store(false, Ordering::SeqCst);
    }

    /// Core's `ShutdownRequested`.
    pub fn shutdown_requested(&self) -> bool {
        self.requested_flag.load(Ordering::SeqCst)
    }

    /// Core's `RestartRequested`.
    pub fn restart_requested(&self) -> bool {
        self.restart.load(Ordering::SeqCst)
    }

    /// Core's `WaitForShutdown`: blocks until a shutdown is requested.
    pub fn wait_for_shutdown(&self) {
        let requested = self.requested.lock().unwrap_or_else(PoisonError::into_inner);
        let _requested =
            self.wake.wait_while(requested, |r| !*r).unwrap_or_else(PoisonError::into_inner);
    }

    /// [`wait_for_shutdown`](Self::wait_for_shutdown) for at most `timeout`; whether a shutdown
    /// is requested.
    pub fn wait_for_shutdown_timeout(&self, timeout: Duration) -> bool {
        let requested = self.requested.lock().unwrap_or_else(PoisonError::into_inner);
        let (requested, _) = self
            .wake
            .wait_timeout_while(requested, timeout, |r| !*r)
            .unwrap_or_else(PoisonError::into_inner);
        *requested
    }

    /// The process's exit status (`g_exit_status`).
    pub fn exit_status(&self) -> i32 {
        self.exit_status.load(Ordering::SeqCst)
    }

    pub fn set_exit_status(&self, status: i32) {
        self.exit_status.store(status, Ordering::SeqCst);
    }

    /// Records an `AbortNode`: exit status 1. The caller then requests the shutdown, which
    /// still flushes (DEC-241).
    pub fn mark_aborted(&self) {
        self.exit_status.store(EXIT_FAILURE, Ordering::SeqCst);
        self.aborted.store(true, Ordering::SeqCst);
    }

    /// Whether an `AbortNode` ran.
    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

/// V's view of the node's flag (CH1-node D52): `-stopatheight` and a listener's panic wake
/// [`ShutdownState::wait_for_shutdown`], and V's checks see every other request.
impl ShutdownFlag for ShutdownState {
    fn start_shutdown(&self) {
        ShutdownState::start_shutdown(self);
    }

    fn shutdown_requested(&self) -> bool {
        ShutdownState::shutdown_requested(self)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_request_wakes_every_waiter_and_stays_set() {
        let state = Arc::new(ShutdownState::new());
        assert!(!state.shutdown_requested());
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let state = Arc::clone(&state);
                std::thread::spawn(move || state.wait_for_shutdown())
            })
            .collect();
        state.start_shutdown();
        state.start_shutdown();
        for w in waiters {
            w.join().unwrap();
        }
        assert!(state.shutdown_requested());
        // A later wait returns at once.
        state.wait_for_shutdown();
        assert_eq!(state.exit_status(), EXIT_SUCCESS);
        assert!(!state.aborted() && !state.restart_requested());
    }

    #[test]
    fn abort_shutdown_withdraws_the_request() {
        let state = ShutdownState::new();
        state.start_shutdown();
        state.abort_shutdown();
        assert!(!state.shutdown_requested());
        let start = Instant::now();
        assert!(!state.wait_for_shutdown_timeout(Duration::from_millis(20)));
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn restart_and_abort_marks() {
        let state = ShutdownState::new();
        state.start_restart();
        assert!(state.restart_requested() && state.shutdown_requested());
        state.mark_aborted();
        assert!(state.aborted());
        assert_eq!(state.exit_status(), EXIT_FAILURE);
    }
}
