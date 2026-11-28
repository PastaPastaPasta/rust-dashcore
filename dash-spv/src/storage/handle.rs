//! The SPV store as the async storage types share it: one [`Store`] on SB14's header files and
//! SQLite, whose engine and file calls run on tokio's blocking pool so they never stall an async
//! worker thread.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dash_kv_sqlite::SqliteEngine;

use crate::error::{StorageError, StorageResult};
use crate::storage::store::hdr::io::OsIo;
use crate::storage::store::legacy::RawHeader;
use crate::storage::store::{self, Recovered, Store, Subscriber, View};

/// The production store.
pub type SpvStoreInner = Store<SqliteEngine, OsIo>;

/// What the handles share: the store. A drop writes nothing (D8): `stop`, `persist` and the
/// worker's tick are the clean raises.
pub(crate) struct Shared {
    store: SpvStoreInner,
    jobs: Arc<tokio::sync::watch::Sender<Jobs>>,
    /// `<dir>.lock`, the direct open's or `DiskStorageManager`'s: released only once the last
    /// handle has closed the store (fields drop in order), so no handle, and no worker sync
    /// that upgraded one, writes after another opener is admitted (Opus-h r2 m1).
    _lock: Arc<super::lockfile::LockFile>,
    /// A test storage's directory. Fields drop in order, so it is deleted once the store has
    /// closed its files and connections.
    #[cfg(test)]
    temp_dir: std::sync::OnceLock<tempfile::TempDir>,
}

/// The blocking jobs on the store: a stop closes it to new ones and waits for those running.
#[derive(Default)]
struct Jobs {
    closed: bool,
    running: usize,
}

/// A running job's count, released when its blocking closure ends, finished or unwound, once
/// the closure has let go of the store.
struct Running(Arc<tokio::sync::watch::Sender<Jobs>>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.send_modify(|jobs| jobs.running -= 1);
    }
}

/// The store closed to new jobs by [`SpvStore::close`], until this drops.
pub(crate) struct Closed<'a>(&'a SpvStore);

impl Closed<'_> {
    /// Waits up to `limit` for the jobs started before the close; on a timeout, logs and returns
    /// how many still run. A job is never cancelled.
    pub(crate) async fn drain(&self, limit: Duration) -> StorageResult<()> {
        let mut jobs = self.0 .0.jobs.subscribe();
        if tokio::time::timeout(limit, jobs.wait_for(|jobs| jobs.running == 0)).await.is_ok() {
            return Ok(());
        }
        let running = jobs.borrow().running;
        tracing::error!("{running} SPV store jobs still running {limit:?} into the stop");
        Err(StorageError::StopTimedOut {
            running,
            limit,
        })
    }

    /// Runs `f` as [`SpvStore::run`] does, while the store is closed to everyone else.
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&SpvStoreInner) -> store::Result<T> + Send + 'static,
    ) -> StorageResult<T> {
        self.0.spawn(f, true).await
    }
}

impl Drop for Closed<'_> {
    fn drop(&mut self) {
        self.0 .0.jobs.send_modify(|jobs| jobs.closed = false);
    }
}

/// A shared handle to the SPV store. It is public only to `test-utils`; without it, it is
/// reachable through `SpvChainHost::open` but not nameable outside the crate.
#[derive(Clone)]
#[cfg_attr(not(feature = "test-utils"), allow(unnameable_types))]
pub struct SpvStore(Arc<Shared>);

impl std::fmt::Debug for SpvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.store.fmt(f)
    }
}

impl SpvStore {
    /// Opens the store in the storage directory `storage` directly, with no migration: a new
    /// (empty) directory, or one at the current version. Any other is refused unchanged (SB14
    /// D39); `DiskStorageManager::new` migrates it. It holds `<dir>.lock` as that does, so
    /// neither can admit or change what the other is migrating or clearing.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn open(storage: &Path) -> StorageResult<Self> {
        crate::storage::io::create_dir_synced(storage)?;
        let lock = super::lockfile::LockFile::new(super::lock_file_path(storage))?;
        let generation = crate::storage::migrator::admit_direct_open(storage)?;
        Ok(Self::open_migrated(storage, Arc::new(lock), generation)?)
    }

    /// Opens the store in the storage directory `storage`, which migration has made current
    /// (SB14 §3 Open; SQLite only if it exists, D4), and runs its blind recovery (SB0 §9.5)
    /// against the winning slot. The store keeps `lock`, `<dir>.lock` held by the caller.
    pub(super) fn open_migrated(
        storage: &Path,
        lock: Arc<super::lockfile::LockFile>,
        generation: u64,
    ) -> store::Result<Self> {
        let store = Store::open_dir(storage)?;
        store.set_generation(generation);
        let recovered = store.recover()?;
        for (s, r) in Subscriber::ALL.into_iter().zip(recovered) {
            if r != Recovered::Kept {
                tracing::info!("SPV store recovery: {s:?} {r:?}");
            }
        }
        Ok(SpvStore(Arc::new(Shared {
            store,
            jobs: Arc::new(tokio::sync::watch::Sender::new(Jobs::default())),
            _lock: lock,
            #[cfg(test)]
            temp_dir: std::sync::OnceLock::new(),
        })))
    }

    /// Deletes `dir` when the last handle closes the store.
    #[cfg(test)]
    pub(crate) fn delete_on_close(&self, dir: tempfile::TempDir) {
        assert!(self.0.temp_dir.set(dir).is_ok(), "the store already owns a directory");
    }

    /// A handle that does not keep the store open: the background sync's, so dropping the
    /// storage closes the database.
    pub(crate) fn downgrade(&self) -> std::sync::Weak<Shared> {
        Arc::downgrade(&self.0)
    }

    pub(crate) fn upgrade(weak: &std::sync::Weak<Shared>) -> Option<Self> {
        weak.upgrade().map(SpvStore)
    }

    /// The synchronous store itself, for tests: an async caller goes through [`run`](Self::run).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn store(&self) -> &SpvStoreInner {
        &self.0.store
    }

    /// The synchronous store, for calls made inside one of its jobs: the header chain's.
    pub(crate) fn inner(&self) -> &SpvStoreInner {
        &self.0.store
    }

    /// What the store holds.
    pub fn view(&self) -> View {
        self.0.store.view()
    }

    /// Stores headers at `start..` (`Store::put_headers`).
    pub async fn put_headers(&self, start: u32, headers: Vec<RawHeader>) -> StorageResult<()> {
        self.run(move |s| s.put_headers(start, &headers)).await
    }

    /// Runs `f` on the store on tokio's blocking pool. While a stop runs, the store refuses every
    /// job, reads included, with [`StorageError::ShuttingDown`]; so does a runtime shutting down,
    /// which drops a job it never started.
    pub async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&SpvStoreInner) -> store::Result<T> + Send + 'static,
    ) -> StorageResult<T> {
        self.spawn(f, false).await
    }

    /// Closes the store to new jobs (BL-911): a stop drains those running, so none outlives it.
    pub(crate) fn close(&self) -> Closed<'_> {
        self.0.jobs.send_modify(|jobs| jobs.closed = true);
        Closed(self)
    }

    async fn spawn<T: Send + 'static>(
        &self,
        f: impl FnOnce(&SpvStoreInner) -> store::Result<T> + Send + 'static,
        closing: bool,
    ) -> StorageResult<T> {
        let started = self.0.jobs.send_if_modified(|jobs| {
            if jobs.closed && !closing {
                return false;
            }
            jobs.running += 1;
            true
        });
        if !started {
            return Err(StorageError::ShuttingDown);
        }
        let running = Running(Arc::clone(&self.0.jobs));
        let shared = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || {
            let _running = running;
            // Dropped first, also on unwind: a drained stop leaves no job holding the store.
            let shared = shared;
            f(&shared.store)
        })
        .await
        .map_err(|e| {
            if e.is_cancelled() {
                StorageError::ShuttingDown
            } else {
                StorageError::Io(std::io::Error::other(e))
            }
        })?
        .map_err(StorageError::from)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::Weak;
    use std::task::{Context, Wake, Waker};

    use super::*;

    /// Records how many handles still hold the store when the job count changes.
    struct Holders(Weak<Shared>, tokio::sync::mpsc::UnboundedSender<usize>);

    impl Wake for Holders {
        fn wake(self: Arc<Self>) {
            let _ = self.1.send(self.0.strong_count());
        }
    }

    /// BL-911 r1 M1: a job's count is released only after its closure has let go of the store,
    /// so when a stop's drain sees the last job end, the job no longer holds the directory.
    #[tokio::test]
    async fn a_job_lets_go_of_the_store_before_its_count_is_released() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SpvStore::open(tmp.path()).unwrap();
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let job = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .run(move |s| {
                        let _ = started.send(());
                        let _ = held.recv();
                        s.put_metadata("bl911", b"written")
                    })
                    .await
            }
        });
        running.await.unwrap();
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());

        let mut jobs = store.0.jobs.subscribe();
        let (holders, mut recorded) = tokio::sync::mpsc::unbounded_channel();
        let waker = Waker::from(Arc::new(Holders(Arc::downgrade(&store.0), holders)));
        let mut changed = Box::pin(jobs.changed());
        assert!(changed.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
        drop(store);

        release.send(()).unwrap();
        let at_release = tokio::time::timeout(Duration::from_secs(30), recorded.recv()).await;
        assert_eq!(at_release.unwrap(), Some(0), "the job still held the store at its release");
        drop(changed);
        SpvStore::open(tmp.path()).unwrap();
    }
}
