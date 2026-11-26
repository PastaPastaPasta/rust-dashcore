//! Core's `CThreadInterrupt` for the signing threads.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

/// A shared interrupt flag; clones share one state.
#[derive(Clone, Debug, Default)]
pub struct SigningInterrupt(Arc<(Mutex<bool>, Condvar)>);

impl SigningInterrupt {
    pub fn new() -> Self {
        Self::default()
    }

    // Core: v24.0.2 src/util/threadinterrupt.cpp:21-28 CThreadInterrupt::operator()
    pub fn trigger(&self) {
        let (flag, cond) = &*self.0;
        *flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cond.notify_all();
    }

    // Core: v24.0.2 src/util/threadinterrupt.cpp:11-14 CThreadInterrupt::operator bool
    pub fn is_set(&self) -> bool {
        *self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sleeps for `d` unless triggered first; false when interrupted.
    // Core: v24.0.2 src/util/threadinterrupt.cpp:30-34 CThreadInterrupt::sleep_for
    pub fn sleep_for(&self, d: Duration) -> bool {
        let (flag, cond) = &*self.0;
        let set = flag.lock().unwrap_or_else(PoisonError::into_inner);
        let (set, _) =
            cond.wait_timeout_while(set, d, |set| !*set).unwrap_or_else(PoisonError::into_inner);
        !*set
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Instant;

    use super::*;

    #[test]
    fn trigger_wakes_a_sleeper() {
        let interrupt = SigningInterrupt::new();
        assert!(interrupt.sleep_for(Duration::from_millis(1)));
        let sleeper = interrupt.clone();
        let start = Instant::now();
        let handle = thread::spawn(move || sleeper.sleep_for(Duration::from_secs(600)));
        interrupt.trigger();
        assert!(!handle.join().unwrap());
        assert!(start.elapsed() < Duration::from_secs(300));
        assert!(!interrupt.sleep_for(Duration::from_secs(600)));
    }

    #[test]
    fn clones_share_the_state() {
        let a = SigningInterrupt::new();
        let b = a.clone();
        assert!(!a.is_set() && !b.is_set());
        b.trigger();
        assert!(a.is_set());
    }
}
