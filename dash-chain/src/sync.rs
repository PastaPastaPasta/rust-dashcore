//! The loom facade for the feeds (SB5 §10.2, D23): the leaf mutexes, the condvar, the atomics and
//! the thread token. Production builds use `parking_lot` and `std`; `RUSTFLAGS="--cfg loom"`
//! builds swap in loom's types so the models run the production protocols unchanged.
//!
//! `Arc` stays `std`'s in both builds: loom checks the atomics and locks *inside* the shared
//! values, and `std::sync::Arc` keeps `Arc<dyn FailStop>` and the public `Notify` type unchanged.

#![cfg_attr(all(test, loom), allow(dead_code, reason = "the loom build runs only the models"))]

#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
#[cfg(not(loom))]
pub(crate) use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Duration;

/// A leaf mutex: `parking_lot`'s (no poisoning), or loom's.
#[derive(Debug, Default)]
pub(crate) struct Mutex<T>(Inner<T>);

#[cfg(not(loom))]
type Inner<T> = parking_lot::Mutex<T>;
#[cfg(loom)]
type Inner<T> = loom::sync::Mutex<T>;

#[cfg(not(loom))]
type InnerGuard<'a, T> = parking_lot::MutexGuard<'a, T>;
#[cfg(loom)]
type InnerGuard<'a, T> = loom::sync::MutexGuard<'a, T>;

pub(crate) struct MutexGuard<'a, T>(InnerGuard<'a, T>);

impl<T> Mutex<T> {
    pub(crate) fn new(t: T) -> Mutex<T> {
        Mutex(Inner::new(t))
    }

    #[cfg(not(loom))]
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        MutexGuard(self.0.lock())
    }

    #[cfg(loom)]
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        MutexGuard(self.0.lock().expect("loom mutexes are not poisoned by the models"))
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// The condvar paired with a facade [`Mutex`] (the `Parker`'s).
#[derive(Debug, Default)]
pub(crate) struct Condvar(#[cfg(not(loom))] parking_lot::Condvar, #[cfg(loom)] loom::sync::Condvar);

impl Condvar {
    pub(crate) fn new() -> Condvar {
        Condvar(Default::default())
    }

    #[cfg(not(loom))]
    pub(crate) fn wait<'a, T>(&self, mut g: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.0.wait(&mut g.0);
        g
    }

    #[cfg(loom)]
    pub(crate) fn wait<'a, T>(&self, g: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        MutexGuard(self.0.wait(g.0).expect("loom mutexes are not poisoned by the models"))
    }

    /// Waits at most `d`; spurious wakeups are possible, as for `wait`.
    #[cfg(not(loom))]
    pub(crate) fn wait_timeout<'a, T>(
        &self,
        mut g: MutexGuard<'a, T>,
        d: Duration,
    ) -> MutexGuard<'a, T> {
        self.0.wait_for(&mut g.0, d);
        g
    }

    #[cfg(loom)]
    pub(crate) fn wait_timeout<'a, T>(
        &self,
        g: MutexGuard<'a, T>,
        d: Duration,
    ) -> MutexGuard<'a, T> {
        let (g, _) =
            self.0.wait_timeout(g.0, d).expect("loom mutexes are not poisoned by the models");
        MutexGuard(g)
    }

    pub(crate) fn notify_one(&self) {
        self.0.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn notify_all(&self) {
        self.0.notify_all();
    }
}

/// A value republished whole and loaded lock-free (§4.2): `ArcSwap`, or under loom a mutex
/// around the `Arc`. No model relies on the mutex's extra edge (§10.2).
pub(crate) struct Published<T>(#[cfg(not(loom))] arc_swap::ArcSwap<T>, #[cfg(loom)] Mutex<Arc<T>>);

impl<T> Published<T> {
    pub(crate) fn new(t: T) -> Published<T> {
        #[cfg(not(loom))]
        return Published(arc_swap::ArcSwap::from_pointee(t));
        #[cfg(loom)]
        return Published(Mutex::new(Arc::new(t)));
    }

    pub(crate) fn load(&self) -> Arc<T> {
        #[cfg(not(loom))]
        return self.0.load_full();
        #[cfg(loom)]
        return self.0.lock().clone();
    }

    pub(crate) fn store(&self, t: Arc<T>) {
        #[cfg(not(loom))]
        self.0.store(t);
        #[cfg(loom)]
        {
            *self.0.lock() = t;
        }
    }
}

/// This thread's token: non-zero, unique per thread for the process's life. `held_owner` stores
/// it (§4.3), so `0` means "no hold".
pub(crate) fn thread_token() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    fn fresh() -> u64 {
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
    // Loom runs every model thread on one OS thread, so its thread-locals are per model thread.
    #[cfg(loom)]
    loom::thread_local!(static TOKEN: u64 = fresh());
    #[cfg(not(loom))]
    std::thread_local!(static TOKEN: u64 = fresh());
    TOKEN.with(|t| *t)
}

/// Yields to the scheduler (loom: a preemption point).
pub(crate) fn yield_now() {
    #[cfg(loom)]
    loom::thread::yield_now();
    #[cfg(not(loom))]
    std::thread::yield_now();
}
