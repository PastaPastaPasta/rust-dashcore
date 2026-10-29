//! The fail-stop latch and the host's abort sink (CH2 §2.4.4, §2.10; SB0 §6.3 P6).
//!
//! One [`GlobalFailStop`] is shared by every store of a node (one per process) or of an SPV
//! client (one per client); a test world gets its own. The first failure trips it, and from then
//! on every write, barrier or flat-file mutation through a guard is refused (ST3a R8 across
//! stores). Fatal failures go to the host's [`AbortSink`].
//!
//! Every wait that must end on a trip registers a [`Parker`] with the latch before it parks, and a
//! trip wakes them all (E-b1.7). A wait that parks on another kind of handle registers a wake
//! function instead ([`GlobalFailStop::register_fn`]).

use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::ErrorKind;

/// The failure a [`GlobalFailStop`] holds: the first one to trip it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: ErrorKind,
    /// Where it happened: `"<database> read"`, `"<database> write"`, `"<database> barrier"` or
    /// `"block files write"`.
    pub site: &'static str,
    pub message: String,
}

/// A latched failure, seen by an operation that cannot go on ([`GlobalFailStop::check`]). Whoever
/// tripped the latch has reported it already, so its holder stops without a second `AbortNode`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", .0.message)]
pub struct Fatal(pub Failure);

/// A wait's wake handle (E-b1.7): a condvar permit, shaped like SB5's `Parker`. A `notify` before
/// the `park` is kept, so no wake is lost; one waiter parks on it.
#[derive(Debug, Default)]
pub struct Parker {
    permit: Mutex<bool>,
    cv: Condvar,
}

impl Parker {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// The permit, also after a panic elsewhere: a trip from an unwinding thread still wakes.
    fn permit(&self) -> MutexGuard<'_, bool> {
        self.permit.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for the permit, and takes it.
    pub fn park(&self) {
        let permit = self.permit();
        let mut permit =
            self.cv.wait_while(permit, |p| !*p).unwrap_or_else(PoisonError::into_inner);
        *permit = false;
    }

    /// Waits at most `timeout` for the permit; whether it came (and was taken).
    pub fn park_timeout(&self, timeout: Duration) -> bool {
        let permit = self.permit();
        let (mut permit, _) = self
            .cv
            .wait_timeout_while(permit, timeout, |p| !*p)
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *permit)
    }

    pub fn notify(&self) {
        *self.permit() = true;
        self.cv.notify_one();
    }
}

/// The fail-stop latch: one per node (in production, one per process) or per SPV client, shared by
/// every engine and the block files; a test world gets its own. Once tripped it stays tripped, and
/// every later write, barrier or flat-file mutation is refused (ST3a R8 across stores).
#[derive(Clone, Debug, Default)]
pub struct GlobalFailStop(Arc<Latch>);

#[derive(Debug, Default)]
struct Latch {
    failure: OnceLock<Failure>,
    /// The registered waits' handles, one entry per registration.
    wakers: Mutex<Vec<Wake>>,
}

impl Latch {
    /// The registry, also after a panic elsewhere (a trip from an unwinding thread, M2).
    fn wakers(&self) -> MutexGuard<'_, Vec<Wake>> {
        self.wakers.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A registered wait's handle.
#[derive(Clone)]
enum Wake {
    Parker(Arc<Parker>),
    /// Called under the registry's lock: it must not block, or touch the latch.
    Fn(Arc<dyn Fn() + Send + Sync>),
}

impl Wake {
    fn wake(&self) {
        match self {
            Self::Parker(p) => p.notify(),
            Self::Fn(f) => f(),
        }
    }

    fn same(&self, other: &Wake) -> bool {
        match (self, other) {
            (Self::Parker(a), Self::Parker(b)) => Arc::ptr_eq(a, b),
            (Self::Fn(a), Self::Fn(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl fmt::Debug for Wake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parker(p) => f.debug_tuple("Parker").field(p).finish(),
            Self::Fn(_) => f.write_str("Fn"),
        }
    }
}

impl GlobalFailStop {
    pub fn new() -> Self {
        Self::default()
    }

    /// Latches `f` unless a failure is latched already, then wakes every registered wait; whether
    /// this call latched it. The failure is stored before the wakes, so a wait that registered,
    /// then found no failure, is woken.
    pub fn trip(&self, f: Failure) -> bool {
        let latched = self.0.failure.set(f).is_ok();
        if latched {
            self.0.wakers().iter().for_each(Wake::wake);
        }
        latched
    }

    /// The latched failure.
    pub fn tripped(&self) -> Option<&Failure> {
        self.0.failure.get()
    }

    /// The latched failure as an error.
    ///
    /// # Errors
    ///
    /// [`Fatal`] once the latch has tripped.
    pub fn check(&self) -> Result<(), Fatal> {
        self.tripped().map_or(Ok(()), |f| Err(Fatal(f.clone())))
    }

    /// Registers `parker`, so a trip notifies it, until the registration is dropped. A wait
    /// registers, then checks the latch and its own predicate, and parks only if neither holds.
    pub fn register(&self, parker: &Arc<Parker>) -> Registration {
        self.register_wake(Wake::Parker(Arc::clone(parker)))
    }

    /// Registers `wake`, called once at a trip, until the registration is dropped: for a wait
    /// that parks on a handle of its own, which `wake` notifies. It runs on the tripping thread,
    /// under the registry's lock, so it must not block or touch the latch. A registration made
    /// once the failure is stored may not be called (it is not after `trip` has returned): its
    /// wait sees the failure in the latch check it makes after registering.
    pub fn register_fn(&self, wake: Arc<dyn Fn() + Send + Sync>) -> Registration {
        self.register_wake(Wake::Fn(wake))
    }

    fn register_wake(&self, wake: Wake) -> Registration {
        self.0.wakers().push(wake.clone());
        Registration {
            latch: self.clone(),
            wake,
        }
    }

    /// Parks until a failure is latched or `timeout` has passed; whether one is latched.
    pub fn wait_tripped(&self, timeout: Duration) -> bool {
        let parker = Parker::new();
        let _registered = self.register(&parker);
        let deadline = Instant::now() + timeout;
        loop {
            if self.tripped().is_some() {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            parker.park_timeout(left);
        }
    }
}

/// A [`Parker`]'s registration with a [`GlobalFailStop`], or a wake function's: dropping it
/// unregisters it.
#[must_use = "dropping the registration unregisters the parker"]
#[derive(Debug)]
pub struct Registration {
    latch: GlobalFailStop,
    wake: Wake,
}

impl Drop for Registration {
    /// Removes one entry of its parker: a parker registered twice stays registered once.
    fn drop(&mut self) {
        let mut wakers = self.latch.0.wakers();
        if let Some(at) = wakers.iter().position(|w| w.same(&self.wake)) {
            wakers.swap_remove(at);
        }
    }
}

/// Where fatal storage failures go, implemented by the host (SB0 §7, L5): the node's `AbortNode`
/// and Core's coins error catcher (ND1), or an SPV client's stop and its FFI error event. The
/// one trait for both CH2's store and CH1-node's V (BL-109).
pub trait AbortSink: Send + Sync {
    /// Core's `AbortNode`: log `log_msg`, show `user_msg`, shut down with exit status 1 (v24).
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>);

    /// A store read failed and the latch has tripped. The node logs Core's two strings ("Error
    /// reading from database: %s", and "Error reading from database, shutting down." to the user,
    /// `init.cpp:2089`) and calls `std::process::abort()`, with no shutdown flush. A test sink
    /// records it and returns, and the read's error carries the failure out of the operation.
    fn read_failed(&self, what: &str);
}

/// What a [`RecordingSink`] was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortMark {
    AbortNode {
        log: String,
        user: Option<String>,
    },
    ReadFailed(String),
}

/// An [`AbortSink`] that records its calls and returns: the seam's and the tests' sink.
#[derive(Debug, Default)]
pub struct RecordingSink {
    marks: Mutex<Vec<AbortMark>>,
}

impl RecordingSink {
    pub fn marks(&self) -> Vec<AbortMark> {
        self.marks.lock().expect("unpoisoned").clone()
    }
}

impl AbortSink for RecordingSink {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        self.marks.lock().expect("unpoisoned").push(AbortMark::AbortNode {
            log: log_msg.into(),
            user: user_msg.map(Into::into),
        });
    }

    fn read_failed(&self, what: &str) {
        self.marks.lock().expect("unpoisoned").push(AbortMark::ReadFailed(what.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(message: &str) -> Failure {
        Failure {
            kind: ErrorKind::Io,
            site: "test",
            message: message.into(),
        }
    }

    /// E-b1.7: a permit given before the park is kept, and taken once.
    #[test]
    fn a_parkers_permit_is_never_lost() {
        let p = Parker::new();
        assert!(!p.park_timeout(Duration::ZERO));
        p.notify();
        p.park();
        p.notify();
        assert!(p.park_timeout(Duration::ZERO));
        assert!(!p.park_timeout(Duration::from_millis(1)));
    }

    /// E-b1.7: `trip` stores the failure, then wakes every registered wait, so a wait that
    /// registered before it checked is woken whenever the trip comes; `check` and `wait_tripped`
    /// see the first failure, and a second trip changes nothing.
    #[test]
    fn a_trip_wakes_every_registered_wait() {
        let latch = GlobalFailStop::new();
        assert_eq!(latch.check(), Ok(()));
        assert!(!latch.wait_tripped(Duration::from_millis(5)));
        std::thread::scope(|scope| {
            let waits: Vec<_> = (0..4)
                .map(|i| {
                    let latch = &latch;
                    scope.spawn(move || {
                        if i % 2 == 0 {
                            return latch.wait_tripped(Duration::from_secs(60));
                        }
                        let p = Parker::new();
                        let _registered = latch.register(&p);
                        while latch.tripped().is_none() {
                            assert!(p.park_timeout(Duration::from_secs(60)), "a lost wake");
                        }
                        true
                    })
                })
                .collect();
            // Every wait has registered (each before it checks), so the trip must reach all four.
            while latch.0.wakers().len() < 4 {
                std::thread::yield_now();
            }
            assert!(latch.trip(failure("first")));
            assert!(waits.into_iter().all(|w| w.join().unwrap()));
        });
        assert!(!latch.trip(failure("second")));
        assert_eq!(latch.check(), Err(Fatal(failure("first"))));
        assert_eq!(latch.check().unwrap_err().to_string(), "first");
        assert!(latch.wait_tripped(Duration::ZERO));
        assert!(latch.0.wakers().is_empty(), "every registration was dropped");
    }

    /// A dropped registration is not woken; a parker registered twice stays registered once.
    #[test]
    fn a_dropped_registration_is_not_woken() {
        let latch = GlobalFailStop::new();
        let (kept, dropped) = (Parker::new(), Parker::new());
        let _kept = latch.register(&kept);
        drop(latch.register(&kept));
        drop(latch.register(&dropped));
        latch.trip(failure("x"));
        assert!(kept.park_timeout(Duration::ZERO));
        assert!(!dropped.park_timeout(Duration::ZERO));
    }

    /// A registered wake function is called once, at the trip that latches; a dropped one is
    /// not, and dropping one function's registration keeps a parker's.
    #[test]
    fn a_registered_function_is_called_at_the_trip() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let latch = GlobalFailStop::new();
        let (kept, dropped) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let count = |n: &Arc<AtomicUsize>| -> Arc<dyn Fn() + Send + Sync> {
            let n = Arc::clone(n);
            Arc::new(move || _ = n.fetch_add(1, Ordering::SeqCst))
        };
        let parker = Parker::new();
        let _parker = latch.register(&parker);
        let _kept = latch.register_fn(count(&kept));
        drop(latch.register_fn(count(&dropped)));
        assert_eq!(latch.0.wakers().len(), 2);
        assert!(latch.trip(failure("x")));
        assert!(!latch.trip(failure("y")));
        assert_eq!(kept.load(Ordering::SeqCst), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert!(parker.park_timeout(Duration::ZERO));
    }

    fn counter(n: &Arc<std::sync::atomic::AtomicUsize>) -> Arc<dyn Fn() + Send + Sync> {
        let n = Arc::clone(n);
        Arc::new(move || _ = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }

    /// Sol r1 N1: a function registered after the trip has returned is not called, and the
    /// latch check its wait makes after registering sees the failure.
    #[test]
    fn a_function_registered_after_the_trip_is_not_called() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let latch = GlobalFailStop::new();
        assert!(latch.trip(failure("x")));
        let late = Arc::new(AtomicUsize::new(0));
        let registered = latch.register_fn(counter(&late));
        assert_eq!(latch.check(), Err(Fatal(failure("x"))));
        assert!(!latch.trip(failure("y")));
        assert_eq!(late.load(Ordering::SeqCst), 0);
        drop(registered);
        assert!(latch.0.wakers().is_empty());
    }

    /// Sol r1 N1: one function registered twice is called once per registration, and dropping
    /// one registration leaves the other.
    #[test]
    fn a_function_registered_twice_is_called_per_registration() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let n = Arc::new(AtomicUsize::new(0));
        let f = counter(&n);
        let latch = GlobalFailStop::new();
        let _a = latch.register_fn(Arc::clone(&f));
        let _b = latch.register_fn(Arc::clone(&f));
        assert!(latch.trip(failure("x")));
        assert_eq!(n.load(Ordering::SeqCst), 2);

        let latch = GlobalFailStop::new();
        let _a = latch.register_fn(Arc::clone(&f));
        drop(latch.register_fn(Arc::clone(&f)));
        assert_eq!(latch.0.wakers().len(), 1);
        n.store(0, Ordering::SeqCst);
        assert!(latch.trip(failure("x")));
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    /// Sol r1 N1: registrations and drops racing a trip. Each wait registers a function that
    /// notifies its own parker, then parks until the latch holds a failure: none is left parked
    /// (no lost wake), and every registration is gone after. A registration that sees the failure
    /// stored may still be called, by the trip's wakes; one made after `trip` returned is not.
    #[test]
    fn registrations_racing_a_trip_lose_no_wake() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for round in 0..200_usize {
            let latch = GlobalFailStop::new();
            std::thread::scope(|scope| {
                let latch = &latch;
                for _ in 0..3 {
                    scope.spawn(move || {
                        let n = Arc::new(AtomicUsize::new(0));
                        while latch.tripped().is_none() {
                            drop(latch.register_fn(counter(&n)));
                        }
                        drop(latch.register_fn(counter(&n)));
                    });
                }
                let waits: Vec<_> = (0..4)
                    .map(|_| {
                        scope.spawn(move || {
                            let parker = Parker::new();
                            let wake: Arc<dyn Fn() + Send + Sync> = {
                                let parker = Arc::clone(&parker);
                                Arc::new(move || parker.notify())
                            };
                            let _registered = latch.register_fn(wake);
                            while latch.tripped().is_none() {
                                assert!(
                                    parker.park_timeout(Duration::from_secs(60)),
                                    "a lost wake"
                                );
                            }
                        })
                    })
                    .collect();
                for _ in 0..round % 50 {
                    std::thread::yield_now();
                }
                assert!(latch.trip(failure("x")));
                for w in waits {
                    w.join().unwrap();
                }
                // Once `trip` has returned, a registration is not called.
                let late = Arc::new(AtomicUsize::new(0));
                drop(latch.register_fn(counter(&late)));
                assert_eq!(late.load(Ordering::SeqCst), 0);
            });
            assert!(latch.0.wakers().is_empty(), "round {round}: a registration outlived its drop");
        }
    }
}
