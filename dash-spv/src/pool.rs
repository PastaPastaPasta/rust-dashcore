//! The CPU pool (SB0 §7, DEC-112 O5): dash-spv's parallel work (header and filter validation,
//! the wallet's filter matching) runs inside one pool the host injects, through
//! [`CpuPool::install`], so rayon's global pool is never built.

use std::fmt;
use std::sync::Arc;

/// The name prefix of the pool's threads (`dash-spv-cpu-0`, …).
pub const CPU_POOL_THREAD_NAME: &str = "dash-spv-cpu";

/// A shared rayon pool for dash-spv's CPU work.
#[derive(Clone)]
pub struct CpuPool(Arc<rayon::ThreadPool>);

impl fmt::Debug for CpuPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuPool").field("threads", &self.threads()).finish()
    }
}

impl CpuPool {
    /// SB0 §7's default size: `min(2, cores)`.
    pub fn default_threads() -> usize {
        std::thread::available_parallelism().map_or(1, |n| n.get()).min(2)
    }

    /// A pool of `threads` threads (at least one), named [`CPU_POOL_THREAD_NAME`]`-i`.
    pub fn new(threads: usize) -> Result<Self, rayon::ThreadPoolBuildError> {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads.max(1))
            .thread_name(|i| format!("{CPU_POOL_THREAD_NAME}-{i}"))
            .build()
            .map(|pool| Self(Arc::new(pool)))
    }

    /// A pool the host built.
    pub fn from_pool(pool: Arc<rayon::ThreadPool>) -> Self {
        Self(pool)
    }

    /// Runs `f` on the pool, waiting for it: rayon's parallel iterators inside use this pool.
    pub fn install<R: Send>(&self, f: impl FnOnce() -> R + Send) -> R {
        self.0.install(f)
    }

    /// Runs `f` on the pool without blocking the caller's thread: an async caller awaits it,
    /// and its runtime (a current-thread one too) keeps running its other tasks meanwhile
    /// (SB13 D38). A panic in `f` resumes in the caller, as with [`install`](Self::install).
    pub async fn run<R: Send + 'static>(&self, f: impl FnOnce() -> R + Send + 'static) -> R {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.0.spawn(move || {
            let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
        });
        match rx.await {
            Ok(Ok(r)) => r,
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(_) => unreachable!("a pool job always sends its result"),
        }
    }

    pub fn threads(&self) -> usize {
        self.0.current_num_threads()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_runs_on_the_pool_threads() {
        let pool = CpuPool::new(2).unwrap();
        assert_eq!(pool.threads(), 2);
        let name = pool.install(|| std::thread::current().name().map(str::to_owned));
        assert!(name.is_some_and(|n| n.starts_with(CPU_POOL_THREAD_NAME)));
        assert!((1..=2).contains(&CpuPool::default_threads()));
    }

    /// `run` works on a current-thread runtime, whose only thread it does not block.
    #[tokio::test]
    async fn run_awaits_the_pool_without_blocking_the_runtime() {
        let pool = CpuPool::new(1).unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let job = pool.run(move || rx.recv().is_ok());
        // The job waits for this task, which runs on the runtime's only thread meanwhile.
        let unblock = async move {
            tokio::task::yield_now().await;
            tx.send(()).unwrap();
        };
        let (done, ()) = tokio::join!(job, unblock);
        assert!(done);
        let name = pool.run(|| std::thread::current().name().map(str::to_owned)).await;
        assert!(name.is_some_and(|n| n.starts_with(CPU_POOL_THREAD_NAME)));
        let panicked = std::panic::AssertUnwindSafe(pool.run(|| panic!("in the pool")));
        assert!(futures::FutureExt::catch_unwind(panicked).await.is_err());
    }
}
