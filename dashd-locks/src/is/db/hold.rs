//! The attempt gate's hold as an owned guard.
//!
//! The first successful stage of an attempt stores a [`GateHold`] in the overlay. Committing the
//! last stage, or discarding them, ends the hold through [`GateHold::release`]. An overlay that is
//! dropped while it still holds the gate (V unwinding between a stage and `end_attempt`) runs the
//! guard's `Drop`, which latches the store and releases the gate, so parked callers return the
//! latched error instead of waiting for a commit that will never come.

use std::sync::Arc;

/// The gate operations the hold needs.
///
/// Both methods run with only the gate mutex taken, wake every waiter under it, and make no
/// engine call. [`GateRelease::latch_and_release`] runs inside `Drop`, possibly during an unwind,
/// so it must not panic: no `unwrap`, `expect`, assertion or allocation.
///
/// The latch is an error kind and a static text under the gate mutex, not a built `StoreError`.
/// `latch_and_release` only sets it, and each caller the latch refuses builds its own error after
/// it wakes.
///
/// A stage takes the gate under the gate mutex, releases that mutex, and only then builds the
/// [`GateHold`]. A hold must never be released or dropped while the same thread holds the gate
/// mutex: the mutex is not reentrant, so either method would deadlock there, and during an
/// unwind every parked caller would then wait forever.
pub(super) trait GateRelease: Send + Sync {
    /// Ends the hold and wakes every waiter.
    fn release(&self);

    /// Latches the store, ends the hold and wakes every waiter.
    fn latch_and_release(&self);
}

/// The attempt's hold on the store's gate.
#[must_use = "dropping a hold latches the store"]
pub(super) struct GateHold {
    gate: Option<Arc<dyn GateRelease>>,
}

impl GateHold {
    /// A hold the caller has just taken on `gate`.
    pub(super) fn new(gate: Arc<dyn GateRelease>) -> Self {
        Self {
            gate: Some(gate),
        }
    }

    /// Ends the hold normally: the last stage committed, or every stage discarded.
    pub(super) fn release(mut self) {
        if let Some(gate) = self.gate.take() {
            gate.release();
        }
    }
}

impl Drop for GateHold {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.take() {
            gate.latch_and_release();
        }
    }
}

impl std::fmt::Debug for GateHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateHold").field("held", &self.gate.is_some()).finish()
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use dash_kv::{ErrorKind, StoreError};
    use parking_lot::{Condvar, Mutex};

    use super::{GateHold, GateRelease};

    /// A gate with one mutex over the hold and the latch, and one condvar. `StoreError` is not
    /// `Clone`, so each latched caller builds its own error from the latch's fixed text.
    struct TestGate {
        state: Mutex<GateState>,
        wake: Condvar,
        waiters: AtomicUsize,
    }

    const LATCHED: &str = "latched";

    fn latched() -> StoreError {
        StoreError::engine("dashd-locks", ErrorKind::Other, LATCHED)
    }

    struct GateState {
        held: bool,
        latched: bool,
    }

    impl TestGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(GateState {
                    held: false,
                    latched: false,
                }),
                wake: Condvar::new(),
                waiters: AtomicUsize::new(0),
            })
        }

        fn take(self: &Arc<Self>) -> GateHold {
            let mut state = self.state.lock();
            assert!(!state.held && !state.latched);
            state.held = true;
            GateHold::new(self.clone())
        }

        /// A non-owner call: it waits while the gate is held, then fails if the store latched.
        fn enter(&self) -> dash_kv::Result<()> {
            let mut state = self.state.lock();
            while state.held && !state.latched {
                self.waiters.fetch_add(1, Ordering::SeqCst);
                self.wake.wait(&mut state);
                self.waiters.fetch_sub(1, Ordering::SeqCst);
            }
            if state.latched {
                return Err(latched());
            }
            Ok(())
        }

        fn parked(&self) -> usize {
            self.waiters.load(Ordering::SeqCst)
        }
    }

    impl GateRelease for TestGate {
        fn release(&self) {
            let mut state = self.state.lock();
            state.held = false;
            self.wake.notify_all();
        }

        fn latch_and_release(&self) {
            let mut state = self.state.lock();
            state.latched = true;
            state.held = false;
            self.wake.notify_all();
        }
    }

    /// How long a parked reader may take to wake before its test fails instead of hanging.
    const WAKE_TIMEOUT: Duration = Duration::from_secs(10);

    /// Readers parked on the held gate, each sending its result once it wakes.
    struct Readers {
        results: mpsc::Receiver<dash_kv::Result<()>>,
        n: usize,
    }

    impl Readers {
        /// Every reader's result; a reader the gate never wakes fails the test.
        fn results(self) -> Vec<dash_kv::Result<()>> {
            (0..self.n)
                .map(|_| {
                    self.results
                        .recv_timeout(WAKE_TIMEOUT)
                        .expect("a parked reader was never woken")
                })
                .collect()
        }
    }

    fn park_readers(gate: &Arc<TestGate>, n: usize) -> Readers {
        let (send, results) = mpsc::channel();
        for _ in 0..n {
            let gate = gate.clone();
            let send = send.clone();
            thread::spawn(move || send.send(gate.enter()));
        }
        while gate.parked() < n {
            thread::yield_now();
        }
        Readers {
            results,
            n,
        }
    }

    #[test]
    fn release_wakes_parked_readers_without_latching() {
        let gate = TestGate::new();
        let hold = gate.take();
        let readers = park_readers(&gate, 2);
        hold.release();
        for result in readers.results() {
            assert!(result.is_ok());
        }
        assert!(gate.enter().is_ok());
        assert!(!gate.state.lock().latched);
    }

    #[test]
    fn panic_while_held_latches_and_wakes_parked_readers() {
        let gate = TestGate::new();
        let readers = {
            let hold = gate.take();
            let readers = park_readers(&gate, 2);
            let staging = thread::spawn(move || {
                let _overlay = hold;
                panic!("unwind between a stage and end_attempt");
            });
            assert!(staging.join().is_err());
            readers
        };
        for result in readers.results() {
            assert_eq!(result.unwrap_err().to_string(), latched().to_string());
        }
        assert_eq!(gate.enter().unwrap_err().to_string(), latched().to_string());
        assert!(!gate.state.lock().held);
    }

    #[test]
    fn panic_on_the_holding_thread_unwinds_once_through_the_guard() {
        let gate = TestGate::new();
        let hold = gate.take();
        let readers = park_readers(&gate, 1);
        let unwound = catch_unwind(AssertUnwindSafe(move || {
            let _overlay = hold;
            panic!("unwind between a stage and end_attempt");
        }));
        assert!(unwound.is_err());
        assert!(readers.results()[0].is_err());
        assert!(gate.enter().is_err());
    }

    #[test]
    fn dropping_an_unreleased_hold_latches_without_a_panic() {
        let gate = TestGate::new();
        let hold = gate.take();
        let readers = park_readers(&gate, 1);
        drop(hold);
        assert!(readers.results()[0].is_err());
        assert!(gate.state.lock().latched);
        assert!(!gate.state.lock().held);
    }
}
