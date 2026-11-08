//! The only wake type (SB5 §3.1, DEC-152 (2), D25): notification-only by construction.

use std::sync::Arc;
use std::time::Duration;

use crate::sync::{Condvar, Mutex};

/// A wake. It never runs caller code: `Task` schedules a task, `Thread` sets a permit.
///
/// **Contract on `Task`:** the `Waker` must be notification-only (tokio's are). A custom
/// `RawWaker` must not block, do I/O or run a handler inline: `notify` can run under the tip
/// lock (§4.2). Its `clone` runs inside slot sections, so it must not allocate either. Its `drop`
/// can run under the tip lock (a writer's clone outlives the reader's copy), so it may only
/// release memory, as tokio's does. The type system cannot check a vtable.
#[derive(Clone, Debug)]
pub enum Notify {
    /// `Waker::wake_by_ref`: tokio tasks (SPV subscribers, SB14's header task).
    Task(std::task::Waker),
    /// A permit parker: driver threads (CH2b's events threads, V's service loop, W).
    Thread(Arc<Parker>),
}

impl Notify {
    pub fn notify(&self) {
        match self {
            Notify::Task(w) => w.wake_by_ref(),
            Notify::Thread(p) => p.notify(),
        }
    }
}

impl From<Arc<Parker>> for Notify {
    fn from(p: Arc<Parker>) -> Notify {
        Notify::Thread(p)
    }
}

/// A permit: `notify` sets it and signals; `park`/`park_timeout` wait for it and clear it. A
/// notify before the park is not lost. Both critical sections are O(1).
#[derive(Debug)]
pub struct Parker {
    permit: Mutex<Permit>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct Permit {
    set: bool,
    /// The planted negative control of loom L1: a notify with no parked thread is dropped.
    #[cfg(all(test, loom))]
    lossy: bool,
    #[cfg(all(test, loom))]
    parked: bool,
}

impl Parker {
    pub fn new() -> Arc<Parker> {
        Arc::new(Parker {
            permit: Mutex::new(Permit::default()),
            cv: Condvar::new(),
        })
    }

    /// L1's negative control: a parker that keeps no permit.
    #[cfg(all(test, loom))]
    pub(crate) fn new_lossy() -> Arc<Parker> {
        Arc::new(Parker {
            permit: Mutex::new(Permit {
                lossy: true,
                ..Permit::default()
            }),
            cv: Condvar::new(),
        })
    }

    /// Waits for the permit and clears it.
    pub fn park(&self) {
        let mut p = self.permit.lock();
        #[cfg(all(test, loom))]
        {
            p.parked = true;
        }
        while !p.set {
            p = self.cv.wait(p);
        }
        p.set = false;
        #[cfg(all(test, loom))]
        {
            p.parked = false;
        }
    }

    /// Waits at most `d` for the permit. Returns true, clearing it, if the permit was set.
    #[cfg(not(loom))]
    pub fn park_timeout(&self, d: Duration) -> bool {
        let Some(deadline) = std::time::Instant::now().checked_add(d) else {
            self.park();
            return true;
        };
        let mut p = self.permit.lock();
        while !p.set {
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            p = self.cv.wait_timeout(p, deadline - now);
        }
        p.set = false;
        true
    }

    /// Loom has no clock, and loom 0.7's `Condvar::wait_timeout` never times out: under loom
    /// this blocks until notified, so a model must not rely on the timeout.
    #[cfg(loom)]
    pub fn park_timeout(&self, d: Duration) -> bool {
        let mut p = self.permit.lock();
        if !p.set {
            p = self.cv.wait_timeout(p, d);
        }
        std::mem::take(&mut p.set)
    }

    /// Sets the permit and signals. For service loops that wait on several sources: register
    /// this parker (as [`Notify::Thread`]) with each.
    pub fn notify(&self) {
        let mut p = self.permit.lock();
        #[cfg(all(test, loom))]
        if p.lossy && !p.parked {
            return;
        }
        p.set = true;
        drop(p);
        self.cv.notify_one();
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Wake, Waker};
    use std::thread;

    use super::*;

    #[test]
    fn parker_permit_before_park() {
        let p = Parker::new();
        p.notify();
        p.notify();
        // One permit, not a count: the first park consumes it, the second times out.
        p.park();
        assert!(!p.park_timeout(Duration::from_millis(20)));
    }

    #[test]
    fn parker_timeout() {
        let p = Parker::new();
        let t = std::time::Instant::now();
        assert!(!p.park_timeout(Duration::from_millis(30)));
        assert!(t.elapsed() >= Duration::from_millis(30));
        p.notify();
        assert!(p.park_timeout(Duration::from_secs(5)));
    }

    #[test]
    fn parker_wakes_parked_thread() {
        let p = Parker::new();
        let q = p.clone();
        let h = thread::spawn(move || q.park());
        thread::sleep(Duration::from_millis(10));
        Notify::from(p).notify();
        h.join().unwrap();
    }

    #[test]
    fn notify_task_wakes_by_ref() {
        struct Flag(AtomicBool);
        impl Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let f = Arc::new(Flag(AtomicBool::new(false)));
        let n = Notify::Task(Waker::from(f.clone()));
        n.notify();
        assert!(f.0.load(Ordering::SeqCst));
    }

    /// A `Notify::Task` from a tokio task: a wake that lands before the task's `poll` returns
    /// `Pending` is not lost (§10.1, the production adapter loom does not model).
    #[test]
    fn tokio_notify_adapter_permit() {
        use std::future::poll_fn;
        use std::task::Poll;

        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        rt.block_on(async {
            let f = fired.clone();
            let task = tokio::spawn(async move {
                poll_fn(|cx| {
                    if f.load(Ordering::SeqCst) {
                        return Poll::Ready(());
                    }
                    // Register, then a producer fires before this poll returns `Pending`.
                    let n = Notify::Task(cx.waker().clone());
                    f.store(true, Ordering::SeqCst);
                    n.notify();
                    Poll::Pending
                })
                .await
            });
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("the wake before Pending was lost")
                .unwrap();
        });
    }
}
