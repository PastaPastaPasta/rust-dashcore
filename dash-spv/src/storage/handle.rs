//! The SPV store as the async storage types share it: one [`Store`] on SB14's header files and
//! SQLite, whose engine and file calls run on tokio's blocking pool so they never stall an async
//! worker thread.

use std::path::Path;
use std::sync::Arc;

use dash_kv_sqlite::SqliteEngine;

use crate::error::StorageResult;
use crate::storage::store::hdr::io::OsIo;
use crate::storage::store::legacy::RawHeader;
use crate::storage::store::{self, Recovered, Store, Subscriber, View};

/// The production store.
pub type SpvStoreInner = Store<SqliteEngine, OsIo>;

/// What the handles share: the store. A drop writes nothing (D8): `stop`, `persist` and the
/// worker's tick are the clean raises.
pub(crate) struct Shared {
    store: SpvStoreInner,
    /// `<dir>.lock`, the direct open's or `DiskStorageManager`'s: released only once the last
    /// handle has closed the store (fields drop in order), so no handle, and no worker sync
    /// that upgraded one, writes after another opener is admitted (Opus-h r2 m1).
    _lock: Arc<super::lockfile::LockFile>,
    /// A test storage's directory. Fields drop in order, so it is deleted once the store has
    /// closed its files and connections.
    #[cfg(test)]
    temp_dir: std::sync::OnceLock<tempfile::TempDir>,
}

/// A shared handle to the SPV store.
#[derive(Clone)]
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
    pub fn open(storage: &Path) -> StorageResult<Self> {
        crate::storage::io::create_dir_synced(storage)?;
        let lock = super::lockfile::LockFile::new(super::lock_file_path(storage))?;
        crate::storage::migrator::admit_direct_open(storage)?;
        Ok(Self::open_migrated(storage, Arc::new(lock))?)
    }

    /// Opens the store in the storage directory `storage`, which migration has made current
    /// (SB14 §3 Open; SQLite only if it exists, D4), and runs its blind recovery (SB0 §9.5)
    /// against the winning slot. The store keeps `lock`, `<dir>.lock` held by the caller.
    pub(super) fn open_migrated(
        storage: &Path,
        lock: Arc<super::lockfile::LockFile>,
    ) -> store::Result<Self> {
        let store = Store::open_dir(storage)?;
        let recovered = store.recover()?;
        for (s, r) in Subscriber::ALL.into_iter().zip(recovered) {
            if r != Recovered::Kept {
                tracing::info!("SPV store recovery: {s:?} {r:?}");
            }
        }
        Ok(SpvStore(Arc::new(Shared {
            store,
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

    /// What the store holds.
    pub fn view(&self) -> View {
        self.0.store.view()
    }

    /// Stores headers at `start..` ([`Store::put_headers`]).
    pub async fn put_headers(&self, start: u32, headers: Vec<RawHeader>) -> StorageResult<()> {
        self.run(move |s| s.put_headers(start, &headers)).await
    }

    /// Runs `f` on the store on tokio's blocking pool.
    pub async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&SpvStoreInner) -> store::Result<T> + Send + 'static,
    ) -> StorageResult<T> {
        let shared = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || f(&shared.store))
            .await
            .map_err(|e| crate::error::StorageError::Io(std::io::Error::other(e)))?
            .map_err(crate::error::StorageError::from)
    }
}
