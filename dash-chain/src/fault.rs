//! Test hooks (SB5 §10.1): pause points inside slot critical sections, the planted allocating
//! intake, and two flags that counting allocators read: the per-thread count of open slot
//! sections (`tests/feed_alloc.rs`) and the residency flag (`tests/step_budget.rs`). Outside
//! `cfg(test)` and the `test-util` feature every hook is a no-op on a zero-sized type.

pub(crate) use imp::*;
#[cfg(all(feature = "test-util", not(loom)))]
pub use imp::{PausePoint, in_residency, in_slot_section};

#[cfg(all(any(test, feature = "test-util"), not(loom)))]
mod imp {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};

    use crate::feed::ChainEvent;

    thread_local! {
        /// Slot sections this thread is inside.
        static DEPTH: Cell<u32> = const { Cell::new(0) };
        /// Whether this thread is growing §6.3's residency.
        static RESIDENCY: Cell<bool> = const { Cell::new(false) };
    }

    /// Whether this thread is growing §6.3's residency: the arena's chunks and tables, the hash
    /// index's table, SPV's resident-height set and chunk counts, and the active chain's fresh
    /// nodes (DEC-187 amendment 2 (c)). Safe to call from an allocator.
    #[cfg_attr(not(feature = "test-util"), allow(dead_code, reason = "`test-util`'s export"))]
    pub fn in_residency() -> bool {
        RESIDENCY.try_with(Cell::get).unwrap_or(false)
    }

    /// Runs `f`, which grows the resident set's structures, as residency work. The flag is
    /// restored on unwind too.
    pub(crate) fn residency<T>(f: impl FnOnce() -> T) -> T {
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                RESIDENCY.with(|r| r.set(self.0));
            }
        }
        let _restore = Restore(RESIDENCY.with(|r| r.replace(true)));
        f()
    }

    /// Whether this thread is inside a slot critical section. Safe to call from an allocator:
    /// a const-initialized `Cell` thread-local never allocates.
    #[cfg_attr(not(feature = "test-util"), allow(dead_code, reason = "`test-util`'s export"))]
    pub fn in_slot_section() -> bool {
        DEPTH.try_with(|d| d.get() > 0).unwrap_or(false)
    }

    /// Marks one slot section on this thread (held by `SlotGuard`).
    pub(crate) struct Section(());

    impl Section {
        pub(crate) fn enter() -> Section {
            DEPTH.with(|d| d.set(d.get() + 1));
            Section(())
        }
    }

    impl Drop for Section {
        fn drop(&mut self) {
            DEPTH.with(|d| d.set(d.get() - 1));
        }
    }

    /// A point a thread stops at, inside a critical section, until the test releases it.
    #[derive(Default)]
    pub struct PausePoint {
        /// The fast path: `hit` takes no lock unless armed (benches build with `test-util`).
        armed: AtomicBool,
        state: Mutex<Pause>,
        cv: Condvar,
    }

    #[derive(Default, PartialEq, Eq)]
    enum Pause {
        #[default]
        Idle,
        Armed,
        Paused,
        Released,
    }

    impl PausePoint {
        /// The next `hit` stops.
        pub fn arm(&self) {
            *self.state.lock().unwrap() = Pause::Armed;
            self.armed.store(true, Ordering::SeqCst);
        }

        /// Waits until a thread stopped at the point.
        pub fn wait_paused(&self) {
            let mut s = self.state.lock().unwrap();
            while *s != Pause::Paused {
                s = self.cv.wait(s).unwrap();
            }
        }

        pub fn release(&self) {
            *self.state.lock().unwrap() = Pause::Released;
            self.cv.notify_all();
        }

        pub(crate) fn hit(&self) {
            if !self.armed.load(Ordering::SeqCst) {
                return;
            }
            let mut s = self.state.lock().unwrap();
            if *s != Pause::Armed {
                return;
            }
            self.armed.store(false, Ordering::SeqCst);
            *s = Pause::Paused;
            self.cv.notify_all();
            while *s != Pause::Released {
                s = self.cv.wait(s).unwrap();
            }
            *s = Pause::Idle;
        }
    }

    /// One slot's hooks.
    #[derive(Default)]
    pub(crate) struct SlotFaults {
        /// Inside `next()`'s section, after the intake.
        pub(crate) intake: PausePoint,
        /// The planted negative control: the intake reallocates staging inside the section.
        pub(crate) alloc_in_intake: AtomicBool,
    }

    impl SlotFaults {
        pub(crate) fn in_intake(&self, staging: &mut VecDeque<ChainEvent>) {
            if self.alloc_in_intake.load(Ordering::SeqCst) {
                staging.reserve_exact(staging.capacity() + 1);
            }
            self.intake.hit();
        }
    }
}

#[cfg(not(all(any(test, feature = "test-util"), not(loom))))]
mod imp {
    use std::collections::VecDeque;

    use crate::feed::ChainEvent;

    #[inline(always)]
    pub(crate) fn residency<T>(f: impl FnOnce() -> T) -> T {
        f()
    }

    pub(crate) struct Section;

    impl Section {
        pub(crate) fn enter() -> Section {
            Section
        }
    }

    #[derive(Default)]
    pub(crate) struct SlotFaults {}

    impl SlotFaults {
        pub(crate) fn in_intake(&self, _staging: &mut VecDeque<ChainEvent>) {}
    }
}
