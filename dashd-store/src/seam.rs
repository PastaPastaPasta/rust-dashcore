//! A test seam on one production call (ST3b S6 plan §3c): it counts the calls and, when armed,
//! fails the next one in place of making it. A field of the engine or file system that owns the
//! call, so tests running in parallel don't share it; tests only.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug, Default)]
pub(crate) struct CallSeam {
    calls: AtomicUsize,
    armed: AtomicBool,
}

impl CallSeam {
    /// The next call fails.
    pub(crate) fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// How many calls reached the seam.
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Counts a call; `true` if it must fail (once per [`arm`](Self::arm)).
    pub(crate) fn enter(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.armed.swap(false, Ordering::SeqCst)
    }
}
