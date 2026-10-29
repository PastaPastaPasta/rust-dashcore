//! Storage abstraction for the Dash SPV client.

mod block_headers;
mod blocks;
mod filter_headers;
mod filters;
mod handle;
mod io;
mod lockfile;
mod masternode;
mod metadata;
mod migrator;
mod peers;
// The store is synchronous: its engine calls run on blocking threads (or in tests with no
// runtime), where tokio's locks cannot be taken (`blocking_lock` panics inside a runtime). Its
// locks are never held across an `.await` (SB13 D11). It is public only to `test-utils`, which
// read it in integration tests: an async host calling it would block a runtime worker (D47).
#[cfg(feature = "test-utils")]
#[allow(clippy::disallowed_types)]
pub mod store;
#[cfg(not(feature = "test-utils"))]
#[allow(clippy::disallowed_types, dead_code, unused_imports)]
pub(crate) mod store;

use crate::error::{StorageError, StorageResult};
use crate::storage::lockfile::LockFile;
use crate::storage::migrator::StorageMigrator;
use crate::types::{HashedBlock, HashedBlockHeader};
use crate::ClientConfig;
use async_trait::async_trait;
use dashcore::hash_types::FilterHeader;
use dashcore::network::message_qrinfo::QRInfo;
use dashcore::network::message_sml::MnListDiff;
use dashcore::prelude::CoreBlockHeight;
use dashcore::Network;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub use crate::storage::block_headers::{
    BlockHeaderStorage, BlockHeaderTip, PersistentBlockHeaderStorage,
};
pub use crate::storage::blocks::{BlockStorage, PersistentBlockStorage};
pub use crate::storage::filter_headers::{FilterHeaderStorage, PersistentFilterHeaderStorage};
pub use crate::storage::filters::{FilterStorage, PersistentFilterStorage};
#[cfg(feature = "test-utils")]
pub use crate::storage::handle::SpvStore;
#[cfg(not(feature = "test-utils"))]
pub(crate) use crate::storage::handle::SpvStore;
pub use crate::storage::masternode::{MasternodeStorage, PersistentMasternodeStorage};
pub use crate::storage::metadata::{MetadataStorage, PersistentMetadataStorage};
pub use crate::storage::peers::{PeerStorage, PersistentPeerStorage};

#[async_trait]
pub trait PersistentStorage: Sized {
    /// If the storage_path contains persisted data the storage will use it, if not,
    /// a empty storage will be created.
    async fn open(storage_path: impl Into<PathBuf> + Send) -> StorageResult<Self>;

    async fn persist(&mut self, storage_path: impl Into<PathBuf> + Send) -> StorageResult<()>;
}

#[async_trait]
pub trait StorageManager:
    BlockHeaderStorage
    + FilterHeaderStorage
    + FilterStorage
    + BlockStorage
    + MetadataStorage
    + MasternodeStorage
    + Send
    + Sync
    + 'static
{
    /// Deletes in-disk and in-memory data
    async fn clear(&mut self) -> StorageResult<()>;

    /// Starts the background tasks, restarting them after a [`Self::stop`].
    async fn start(&mut self);

    /// Stops all background tasks and makes every commit Durable. A failed flush is logged;
    /// [`try_stop`](Self::try_stop) returns it.
    async fn stop(&mut self) {
        if let Err(e) = self.try_stop().await {
            tracing::error!("SPV store sync at stop failed: {e}");
        }
    }

    /// [`stop`](Self::stop), returning a failed flush: the store is then latched (D18), and
    /// what was stored since the last flush may be lost (Opus-h r1 n3).
    async fn try_stop(&mut self) -> StorageResult<()>;

    /// Returns shared access to the block headers storage.
    fn block_headers(&self) -> Arc<RwLock<PersistentBlockHeaderStorage>>;

    /// Returns shared access to the filter headers storage.
    fn filter_headers(&self) -> Arc<RwLock<PersistentFilterHeaderStorage>>;

    /// Returns shared access to the filters storage.
    fn filters(&self) -> Arc<RwLock<PersistentFilterStorage>>;

    /// Returns shared access to the block storage.
    fn blocks(&self) -> Arc<RwLock<PersistentBlockStorage>>;

    /// Returns shared access to the metadata storage.
    fn metadata(&self) -> Arc<RwLock<PersistentMetadataStorage>>;

    fn masternodes(&self)
        -> Arc<RwLock<PersistentMasternodeStorage<PersistentBlockHeaderStorage>>>;
}

/// The SPV store (SB0 §9.5) behind dash-spv's storage traits. Every write commits Buffered, and
/// headers that extend the tip go to the segment files (SB14); a background worker raises the
/// committed tip and makes every commit Durable every few seconds, and
/// [`StorageManager::stop`] does once more. A drop alone writes nothing (D8). Only one instance
/// of DiskStorageManager working on the same storage path can exist at a time.
pub struct DiskStorageManager {
    storage_path: PathBuf,
    network: Network,
    store: SpvStore,

    block_headers: Arc<RwLock<PersistentBlockHeaderStorage>>,
    filter_headers: Arc<RwLock<PersistentFilterHeaderStorage>>,
    filters: Arc<RwLock<PersistentFilterStorage>>,
    blocks: Arc<RwLock<PersistentBlockStorage>>,
    metadata: Arc<RwLock<PersistentMetadataStorage>>,
    masternodes: Arc<RwLock<PersistentMasternodeStorage<PersistentBlockHeaderStorage>>>,

    // Background worker
    worker_handle: Option<(tokio_util::sync::CancellationToken, tokio::task::JoinHandle<()>)>,
}

/// Derives the lockfile path for a storage directory by appending `.lock` to
/// the directory's full name. `Path::set_extension` is unsuitable here: it
/// replaces everything after the last dot, so directory names containing dots
/// (e.g. `95.183.53.19-9999` and `95.183.53.20-9999`) would collide on the
/// same lockfile.
fn lock_file_path(storage_path: &Path) -> PathBuf {
    match storage_path.file_name() {
        Some(name) => {
            let mut lock_name = name.to_os_string();
            lock_name.push(".lock");
            storage_path.with_file_name(lock_name)
        }
        // Paths without a final component (e.g. `/` or `..`); keep the
        // previous `set_extension` behavior.
        None => {
            let mut lock_file = storage_path.to_path_buf();
            lock_file.set_extension("lock");
            lock_file
        }
    }
}

impl DiskStorageManager {
    /// How often the worker raises the committed tip and makes the Buffered commits Durable: the
    /// bound on what a crash loses (SB14 §3 "Drop without `stop`").
    const SYNC_INTERVAL: Duration = Duration::from_secs(5);

    pub async fn new(config: &ClientConfig) -> StorageResult<Self> {
        let storage_path = config.storage_path.clone();
        let lock_file = lock_file_path(&storage_path);

        crate::storage::io::create_dir_synced(&storage_path)?;

        // Held here across the migration and a failed first open's clear, then by the store,
        // which keeps it until its last handle closes (Opus-h r2 m1).
        let lock = Arc::new(LockFile::new(lock_file)?);

        StorageMigrator::migrate(&storage_path).await?;

        let store = match Self::open_store(&storage_path, &lock).await {
            // SB14 §3 Open step 2: the files contradict their slot. D19's clear, then once more.
            Err(crate::storage::store::Error::Layout(what)) => {
                tracing::warn!("SPV header layout corrupt, clearing the storage: {what}");
                StorageMigrator::clear(&storage_path).await?;
                Self::open_store(&storage_path, &lock).await.map_err(StorageError::from)?
            }
            r => r.map_err(StorageError::from)?,
        };

        let (block_headers, masternodes) = Self::header_handles(&store, config.network).await?;
        let mut storage = Self {
            storage_path,
            network: config.network,
            filter_headers: Arc::new(RwLock::new(PersistentFilterHeaderStorage::new(
                store.clone(),
            ))),
            filters: Arc::new(RwLock::new(PersistentFilterStorage::new(store.clone()))),
            blocks: Arc::new(RwLock::new(PersistentBlockStorage::new(store.clone()))),
            metadata: Arc::new(RwLock::new(PersistentMetadataStorage::new(store.clone()))),
            block_headers,
            masternodes,
            store,

            worker_handle: None,
        };

        storage.start_worker().await;

        Ok(storage)
    }

    async fn open_store(
        storage_path: &Path,
        lock: &Arc<LockFile>,
    ) -> Result<SpvStore, crate::storage::store::Error> {
        let (dir, lock) = (storage_path.to_path_buf(), Arc::clone(lock));
        tokio::task::spawn_blocking(move || SpvStore::open_migrated(&dir, lock))
            .await
            .map_err(|e| crate::storage::store::Error::Io(std::io::Error::other(e)))?
    }

    #[allow(clippy::type_complexity)]
    async fn header_handles(
        store: &SpvStore,
        network: Network,
    ) -> StorageResult<(
        Arc<RwLock<PersistentBlockHeaderStorage>>,
        Arc<RwLock<PersistentMasternodeStorage<PersistentBlockHeaderStorage>>>,
    )> {
        let block_headers = Arc::new(RwLock::new(PersistentBlockHeaderStorage::new(store.clone())));
        let masternodes =
            PersistentMasternodeStorage::open(store.clone(), Arc::clone(&block_headers), network)
                .await?;
        Ok((block_headers, Arc::new(RwLock::new(masternodes))))
    }

    /// The SPV store (tests).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn store(&self) -> &SpvStore {
        &self.store
    }

    #[cfg(test)]
    pub async fn with_temp_dir() -> StorageResult<Self> {
        use tempfile::TempDir;

        // Deleted with the store, not on return: handles cloned from it may outlive the manager.
        let temp_dir = TempDir::new()?;
        let storage =
            Self::new(&ClientConfig::testnet().with_storage_path(temp_dir.path())).await?;
        storage.store.delete_on_close(temp_dir);
        Ok(storage)
    }

    /// Start the background worker: every few seconds it raises the committed tip and makes the
    /// Buffered commits Durable (`sync_wal`).
    async fn start_worker(&mut self) {
        if self.worker_handle.as_ref().is_some_and(|(_, handle)| !handle.is_finished()) {
            return;
        }

        let store = self.store.downgrade();
        let cancel = tokio_util::sync::CancellationToken::new();
        let stop = cancel.clone();
        let worker_handle = tokio::spawn(async move {
            let first = tokio::time::Instant::now() + Self::SYNC_INTERVAL;
            let mut ticker = tokio::time::interval_at(first, Self::SYNC_INTERVAL);

            loop {
                // Cancelled between syncs only: an abort would leave a sync running on its
                // blocking thread, holding the store after `stop` returned.
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => return,
                    _ = ticker.tick() => {}
                };
                let Some(store) = SpvStore::upgrade(&store) else {
                    return;
                };
                let done = store.run(|s| s.sync_wal()).await;
                if let Err(e) = done {
                    tracing::error!("SPV store sync failed: {e}");
                }
            }
        });

        self.worker_handle = Some((cancel, worker_handle));
    }

    /// Stops the background worker, waiting out a sync it is running.
    async fn stop_worker(&mut self) {
        if let Some((cancel, handle)) = self.worker_handle.take() {
            cancel.cancel();
            let _ = handle.await;
        }
    }
}

impl Drop for DiskStorageManager {
    fn drop(&mut self) {
        // No wait here: the worker ends after a sync it is running (`stop` waits for it).
        if let Some((cancel, _)) = self.worker_handle.take() {
            cancel.cancel();
        }
    }
}

#[async_trait]
impl StorageManager for DiskStorageManager {
    async fn clear(&mut self) -> StorageResult<()> {
        let worker_was_running = self.worker_handle.is_some();
        self.stop_worker().await;

        // D19: the intent; `tip` unlinked and every row cleared Durable (the store stays
        // open); every other file; then the version without the intent.
        StorageMigrator::write_clearing(&self.storage_path).await?;
        self.store.run(|s| s.clear_all()).await?;
        let mut entries = tokio::fs::read_dir(&self.storage_path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            // D19: what the resumed clear keeps, `logs/` and `v2-unmigrated/` too (Opus-h r1 m5).
            if name == migrator::STORE_DIR
                || name == store::HEADERS_DIR
                || migrator::kept_by_clear(&name)
                || entry.path() == lock_file_path(&self.storage_path)
            {
                continue;
            }
            if entry.file_type().await?.is_dir() {
                tokio::fs::remove_dir_all(entry.path()).await?;
            } else {
                tokio::fs::remove_file(entry.path()).await?;
            }
        }
        StorageMigrator::write_current_version(&self.storage_path).await?;

        let (block_headers, masternodes) = Self::header_handles(&self.store, self.network).await?;
        self.block_headers = block_headers;
        self.masternodes = masternodes;
        self.filter_headers =
            Arc::new(RwLock::new(PersistentFilterHeaderStorage::new(self.store.clone())));
        self.filters = Arc::new(RwLock::new(PersistentFilterStorage::new(self.store.clone())));
        self.blocks = Arc::new(RwLock::new(PersistentBlockStorage::new(self.store.clone())));
        self.metadata = Arc::new(RwLock::new(PersistentMetadataStorage::new(self.store.clone())));

        if worker_was_running {
            self.start_worker().await;
        }

        Ok(())
    }

    async fn start(&mut self) {
        self.start_worker().await;
    }

    async fn try_stop(&mut self) -> StorageResult<()> {
        self.stop_worker().await;
        // The raise and the barrier.
        Ok(self.store.run(|s| s.sync_wal()).await?)
    }

    fn block_headers(&self) -> Arc<RwLock<PersistentBlockHeaderStorage>> {
        Arc::clone(&self.block_headers)
    }

    fn filter_headers(&self) -> Arc<RwLock<PersistentFilterHeaderStorage>> {
        Arc::clone(&self.filter_headers)
    }

    fn filters(&self) -> Arc<RwLock<PersistentFilterStorage>> {
        Arc::clone(&self.filters)
    }

    fn blocks(&self) -> Arc<RwLock<PersistentBlockStorage>> {
        Arc::clone(&self.blocks)
    }

    fn metadata(&self) -> Arc<RwLock<PersistentMetadataStorage>> {
        Arc::clone(&self.metadata)
    }

    fn masternodes(
        &self,
    ) -> Arc<RwLock<PersistentMasternodeStorage<PersistentBlockHeaderStorage>>> {
        Arc::clone(&self.masternodes)
    }
}

#[async_trait]
impl BlockHeaderStorage for DiskStorageManager {
    async fn store_headers(&mut self, headers: &[HashedBlockHeader]) -> StorageResult<()> {
        self.block_headers.write().await.store_headers(headers).await
    }

    async fn store_headers_at_height(
        &mut self,
        headers: &[HashedBlockHeader],
        height: u32,
    ) -> StorageResult<()> {
        self.block_headers.write().await.store_headers_at_height(headers, height).await
    }

    async fn load_headers(&self, range: Range<u32>) -> StorageResult<Vec<HashedBlockHeader>> {
        self.block_headers.read().await.load_headers(range).await
    }

    async fn get_tip_height(&self) -> Option<u32> {
        self.block_headers.read().await.get_tip_height().await
    }

    async fn get_tip(&self) -> Option<BlockHeaderTip> {
        self.block_headers.read().await.get_tip().await
    }

    async fn get_start_height(&self) -> Option<u32> {
        self.block_headers.read().await.get_start_height().await
    }

    async fn get_stored_headers_len(&self) -> u32 {
        self.block_headers.read().await.get_stored_headers_len().await
    }

    async fn get_header_height_by_hash(
        &self,
        hash: &dashcore::BlockHash,
    ) -> StorageResult<Option<u32>> {
        self.block_headers.read().await.get_header_height_by_hash(hash).await
    }

    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()> {
        self.block_headers.write().await.truncate_above(target_height).await
    }
}

#[async_trait]
impl FilterHeaderStorage for DiskStorageManager {
    async fn store_filter_headers(&mut self, headers: &[FilterHeader]) -> StorageResult<()> {
        self.filter_headers.write().await.store_filter_headers(headers).await
    }

    async fn store_filter_headers_at_height(
        &mut self,
        headers: &[FilterHeader],
        height: u32,
    ) -> StorageResult<()> {
        self.filter_headers.write().await.store_filter_headers_at_height(headers, height).await
    }

    async fn load_filter_headers(&self, range: Range<u32>) -> StorageResult<Vec<FilterHeader>> {
        self.filter_headers.read().await.load_filter_headers(range).await
    }

    async fn get_filter_tip_height(&self) -> StorageResult<Option<u32>> {
        self.filter_headers.read().await.get_filter_tip_height().await
    }

    async fn get_filter_start_height(&self) -> Option<u32> {
        self.filter_headers.read().await.get_filter_start_height().await
    }

    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()> {
        self.filter_headers.write().await.truncate_above(target_height).await
    }
}

#[async_trait]
impl filters::FilterStorage for DiskStorageManager {
    async fn store_filter(&mut self, height: u32, filter: &[u8]) -> StorageResult<()> {
        self.filters.write().await.store_filter(height, filter).await
    }

    async fn store_filters(&mut self, start: u32, filters: Vec<Vec<u8>>) -> StorageResult<()> {
        self.filters.write().await.store_filters(start, filters).await
    }

    async fn load_filters(&self, range: Range<u32>) -> StorageResult<Vec<Vec<u8>>> {
        self.filters.read().await.load_filters(range).await
    }

    async fn filter_tip_height(&self) -> StorageResult<u32> {
        self.filters.read().await.filter_tip_height().await
    }

    async fn filter_start_height(&self) -> Option<u32> {
        self.filters.read().await.filter_start_height().await
    }

    async fn clear_filters(&mut self) -> StorageResult<()> {
        self.filters.write().await.clear_filters().await
    }

    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()> {
        self.filters.write().await.truncate_above(target_height).await
    }
}

#[async_trait]
impl BlockStorage for DiskStorageManager {
    async fn store_block(&mut self, height: u32, block: HashedBlock) -> StorageResult<()> {
        self.blocks.write().await.store_block(height, block).await
    }

    async fn load_block(&self, height: u32) -> StorageResult<Option<HashedBlock>> {
        self.blocks.read().await.load_block(height).await
    }

    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()> {
        self.blocks.write().await.truncate_above(target_height).await
    }
}

#[async_trait]
impl metadata::MetadataStorage for DiskStorageManager {
    async fn store_metadata(&mut self, key: &str, value: &[u8]) -> StorageResult<()> {
        self.metadata.write().await.store_metadata(key, value).await
    }

    async fn load_metadata(&self, key: &str) -> StorageResult<Option<Vec<u8>>> {
        self.metadata.read().await.load_metadata(key).await
    }

    async fn store_last_target_height(&mut self, height: CoreBlockHeight) -> StorageResult<()> {
        self.metadata.write().await.store_last_target_height(height).await
    }

    async fn load_last_target_height(&self) -> StorageResult<CoreBlockHeight> {
        self.metadata.read().await.load_last_target_height().await
    }
}

#[async_trait]
impl masternode::MasternodeStorage for DiskStorageManager {
    async fn store_diff(
        &mut self,
        height: CoreBlockHeight,
        diff: &MnListDiff,
    ) -> StorageResult<()> {
        self.masternodes.write().await.store_diff(height, diff).await
    }

    async fn store_qr_info(
        &mut self,
        height: CoreBlockHeight,
        qr_info: &QRInfo,
    ) -> StorageResult<()> {
        self.masternodes.write().await.store_qr_info(height, qr_info).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashcore::Header as BlockHeader;
    use tempfile::TempDir;

    fn hashed_batch(r: std::ops::Range<u32>) -> Vec<HashedBlockHeader> {
        BlockHeader::dummy_batch(r).into_iter().map(HashedBlockHeader::from).collect()
    }

    #[test]
    fn test_lock_file_path_appends_to_full_name() {
        // Dot-free directory names keep the historical behavior.
        assert_eq!(
            lock_file_path(Path::new("data/discovery")),
            PathBuf::from("data/discovery.lock")
        );

        // Dotted directory names must not have their "extension" replaced:
        // distinct sibling directories need distinct lockfiles.
        let a = lock_file_path(Path::new("probes/95.183.53.19-9999"));
        let b = lock_file_path(Path::new("probes/95.183.53.20-9999"));
        assert_eq!(a, PathBuf::from("probes/95.183.53.19-9999.lock"));
        assert_eq!(b, PathBuf::from("probes/95.183.53.20-9999.lock"));
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn test_dotted_sibling_dirs_do_not_share_lock() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = TempDir::new()?;
        let path_a = temp_dir.path().join("95.183.53.19-9999");
        let path_b = temp_dir.path().join("95.183.53.20-9999");

        let config_a = ClientConfig::testnet().with_storage_path(&path_a);
        let config_b = ClientConfig::testnet().with_storage_path(&path_b);

        let _storage_a = DiskStorageManager::new(&config_a).await?;
        // Before the fix this failed with "Data directory ... is already in
        // use by another process" because both paths derived the same lockfile.
        let _storage_b = DiskStorageManager::new(&config_b).await?;

        Ok(())
    }

    /// `stop` raises the stored headers (SB14 §3); a drop alone writes nothing (D8).
    #[tokio::test]
    async fn stop_raises_the_stored_headers() {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig::testnet().with_storage_path(temp_dir.path());
        let headers = hashed_batch(0..50);
        {
            let mut storage = DiskStorageManager::new(&config).await.unwrap();
            storage.store_headers(&headers).await.unwrap();
            storage.stop().await;
        }
        let mut storage = DiskStorageManager::new(&config).await.unwrap();
        assert_eq!(storage.get_tip_height().await, Some(49));
        assert_eq!(storage.load_headers(0..50).await.unwrap(), headers);
        storage.stop().await;
    }

    /// Opus-h r1 n3: a flush that fails at stop is returned by `try_stop` (`stop` logs it).
    #[tokio::test]
    #[cfg(unix)]
    async fn try_stop_returns_a_failed_flush() {
        use std::os::unix::fs::PermissionsExt;
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig::testnet().with_storage_path(temp_dir.path());
        let mut storage = DiskStorageManager::new(&config).await.unwrap();
        storage.store_headers(&hashed_batch(0..50)).await.unwrap();
        // The raise cannot create `block_headers/tip`.
        let headers = temp_dir.path().join(store::HEADERS_DIR);
        let mode = |m| std::fs::set_permissions(&headers, std::fs::Permissions::from_mode(m));
        mode(0o500).unwrap();
        let stopped = storage.try_stop().await;
        mode(0o700).unwrap();
        assert!(stopped.is_err());
    }

    /// D19 (r3 n7, r4 nit e): a kill after each step of a clear; the next open finishes it.
    #[tokio::test]
    async fn an_interrupted_clear_finishes_at_the_next_open() {
        let headers = hashed_batch(0..50);
        // The steps done before the kill: none, `tip` unlinked, `spv/` deleted, the rest deleted.
        for done in 0..4 {
            let temp_dir = TempDir::new().unwrap();
            let path = temp_dir.path();
            let config = ClientConfig::testnet().with_storage_path(path);
            {
                let mut storage = DiskStorageManager::new(&config).await.unwrap();
                storage.store_headers(&headers).await.unwrap();
                BlockHeaderStorage::truncate_above(&mut storage, 40).await.unwrap();
                storage.stop().await;
            }
            assert!(path.join("spv").exists(), "the truncation created the database");
            StorageMigrator::write_clearing(path).await.unwrap();
            if done >= 1 {
                // Absent when every raise went with the SQLite slot.
                let _ = std::fs::remove_file(path.join("block_headers/tip"));
            }
            if done >= 2 {
                std::fs::remove_dir_all(path.join("spv")).unwrap();
            }
            if done >= 3 {
                std::fs::remove_dir_all(path.join("block_headers")).unwrap();
            }
            let mut storage = DiskStorageManager::new(&config).await.unwrap();
            assert_eq!(storage.get_tip_height().await, None, "after {done} steps");
            let version = std::fs::read_to_string(path.join("version.json")).unwrap();
            assert!(!version.contains("clearing"), "after {done} steps: {version}");
            storage.store_headers(&headers[..5]).await.unwrap();
            assert_eq!(storage.get_tip_height().await, Some(4));
            storage.stop().await;
        }
    }

    /// An SB14 open that finds the layout corrupt (a segment missing under the slot) reaches
    /// D19's clear through `new` (r4 nit e(iii)), and the storage opens empty.
    #[tokio::test]
    async fn a_corrupt_header_layout_is_cleared() {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig::testnet().with_storage_path(temp_dir.path());
        {
            let mut storage = DiskStorageManager::new(&config).await.unwrap();
            storage.store_headers(&hashed_batch(0..50)).await.unwrap();
            storage.stop().await;
        }
        std::fs::remove_file(temp_dir.path().join("block_headers/segment_000000.dat")).unwrap();
        let mut storage = DiskStorageManager::new(&config).await.unwrap();
        assert_eq!(storage.get_tip_height().await, None);
        storage.stop().await;
    }

    /// A test storage's directory outlives the manager while a handle holds the store, and goes
    /// with the last one.
    #[tokio::test]
    async fn a_temp_dir_storage_deletes_its_directory_with_the_last_handle() {
        let mut storage = DiskStorageManager::with_temp_dir().await.unwrap();
        storage.stop().await;
        let dir = storage.storage_path.clone();
        let handle = storage.store.clone();
        drop(storage);
        assert!(dir.join("block_headers").exists(), "a handle still holds the store");
        drop(handle);
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn test_store_load_headers() -> Result<(), Box<dyn std::error::Error>> {
        // Create a temporary directory for the test
        let temp_dir = TempDir::new()?;
        let config = ClientConfig::testnet().with_storage_path(temp_dir.path());
        let mut storage = DiskStorageManager::new(&config).await.expect("Unable to create storage");

        let headers = hashed_batch(0..60_000);

        storage.store_headers(&headers[0..0]).await.expect("Should handle empty header batch");
        assert_eq!(storage.get_tip_height().await, None);

        storage.store_headers(&headers[0..1]).await.expect("Failed to store headers");
        let loaded_headers = storage.load_headers(0..1).await?;
        assert_eq!(loaded_headers.len(), 1);
        assert_eq!(loaded_headers[0], headers[0]);

        storage.store_headers(&headers[1..100]).await.expect("Failed to store headers");
        let loaded_headers = storage.load_headers(50..60).await.unwrap();
        assert_eq!(loaded_headers.len(), 10);
        assert_eq!(&loaded_headers, &headers[50..60]);

        storage.store_headers(&headers[100..headers.len()]).await.expect("Failed to store headers");

        let tip_height = storage.get_tip_height().await.unwrap();
        let tip_header = storage.get_header(tip_height).await.unwrap().unwrap();
        let expected_header = &headers[headers.len() - 1];
        assert_eq!(tip_header, *expected_header);

        let non_existing_height = tip_height + 1;
        let non_existing_header = storage.get_header(non_existing_height).await.unwrap();
        assert!(non_existing_header.is_none());

        storage.stop().await;
        drop(storage);
        let storage = DiskStorageManager::new(&config).await.expect("Unable to open storage");

        let loaded_headers = storage.load_headers(49_999..50_002).await.unwrap();
        assert_eq!(loaded_headers.len(), 3);
        assert_eq!(&loaded_headers, &headers[49_999..50_002]);

        Ok(())
    }

    #[tokio::test]
    async fn test_checkpoint_storage_indexing() -> StorageResult<()> {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config = ClientConfig::testnet().with_storage_path(temp_dir.path());
        let mut storage = DiskStorageManager::new(&config).await?;

        // Create test headers starting from checkpoint height
        const CHECKPOINT_HEIGHT: u32 = 1_100_000;
        let headers: Vec<HashedBlockHeader> =
            hashed_batch(CHECKPOINT_HEIGHT..CHECKPOINT_HEIGHT + 100);

        storage.store_headers_at_height(&headers, CHECKPOINT_HEIGHT).await?;

        check_storage(&storage, &headers).await?;

        storage.stop().await;
        drop(storage);

        let storage = DiskStorageManager::new(&config).await?;

        check_storage(&storage, &headers).await?;

        return Ok(());

        async fn check_storage(
            storage: &DiskStorageManager,
            headers: &[HashedBlockHeader],
        ) -> StorageResult<()> {
            assert_eq!(storage.get_stored_headers_len().await, headers.len() as u32);

            let header_at_base = storage.get_header(CHECKPOINT_HEIGHT).await?;
            assert_eq!(header_at_base, Some(headers[0].clone()));

            let header_at_ending = storage.get_header(CHECKPOINT_HEIGHT + 99).await?;
            assert_eq!(header_at_ending, Some(headers[99].clone()));

            // Test the reverse index (hash -> blockchain height)
            let hash_0 = *headers[0].hash();
            let height_0 = storage.get_header_height_by_hash(&hash_0).await?;
            assert_eq!(
                height_0,
                Some(CHECKPOINT_HEIGHT),
                "Hash should map to blockchain height 1,100,000"
            );

            let hash_99 = *headers[99].hash();
            let height_99 = storage.get_header_height_by_hash(&hash_99).await?;
            assert_eq!(
                height_99,
                Some(CHECKPOINT_HEIGHT + 99),
                "Hash should map to blockchain height 1,100,099"
            );

            Ok(())
        }
    }

    #[tokio::test]
    async fn test_reverse_index_disk_storage() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = ClientConfig::regtest().with_storage_path(temp_dir.path());

        {
            let mut storage = DiskStorageManager::new(&config).await.unwrap();

            // Create and store headers
            let headers = hashed_batch(0..10);

            storage.store_headers(&headers).await.unwrap();

            // Test reverse lookups
            for (i, header) in headers.iter().enumerate() {
                let hash = *header.hash();
                let height = storage.get_header_height_by_hash(&hash).await.unwrap();
                assert_eq!(height, Some(i as u32), "Height mismatch for header {}", i);
            }

            storage.stop().await;
        }

        // Test persistence - reload storage and verify index still works
        {
            let storage = DiskStorageManager::new(&config).await.unwrap();

            // The index should have been rebuilt from the loaded headers
            // We need to get the actual headers that were stored to test properly
            for i in 0..10 {
                let stored_header = storage.get_header(i).await.unwrap().unwrap();
                let hash = *stored_header.hash();
                let height = storage.get_header_height_by_hash(&hash).await.unwrap();
                assert_eq!(height, Some(i), "Height mismatch after reload for header {}", i);
            }
        }
    }

    #[tokio::test]
    async fn test_clear_clears_index() {
        let mut storage =
            DiskStorageManager::with_temp_dir().await.expect("Failed to create tmp storage");

        // Store some headers
        let header = hashed_batch(0..1);
        storage.store_headers(&header).await.unwrap();

        let hash = *header[0].hash();
        assert!(storage.get_header_height_by_hash(&hash).await.unwrap().is_some());

        // Clear storage
        storage.clear().await.unwrap();

        // Verify index is cleared
        assert!(storage.get_header_height_by_hash(&hash).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_disk_storage_manager_truncate_above_block_headers() {
        let temp_dir = TempDir::new().unwrap();
        let config = ClientConfig::regtest().with_storage_path(temp_dir.path());
        let mut mgr = DiskStorageManager::new(&config).await.unwrap();

        let headers = hashed_batch(0..10);
        mgr.store_headers(&headers).await.unwrap();

        let orphaned_hash = *headers[7].hash();
        assert_eq!(mgr.get_header_height_by_hash(&orphaned_hash).await.unwrap(), Some(7));

        <DiskStorageManager as BlockHeaderStorage>::truncate_above(&mut mgr, 5).await.unwrap();

        assert_eq!(mgr.get_tip_height().await, Some(5));
        assert_eq!(mgr.get_header_height_by_hash(&orphaned_hash).await.unwrap(), None);

        let kept_hash = *headers[3].hash();
        assert_eq!(mgr.get_header_height_by_hash(&kept_hash).await.unwrap(), Some(3));
    }

    /// D19, Opus-h r1 m5: the online `clear` keeps what the resumed clear keeps: `logs/` and the
    /// version-2 data a migration set aside.
    #[tokio::test]
    async fn an_online_clear_keeps_the_logs_and_the_aside() {
        let tmp = TempDir::new().unwrap();
        let config = crate::ClientConfig::testnet().with_storage_path(tmp.path());
        let mut manager = DiskStorageManager::new(&config).await.unwrap();
        for kept in ["logs", "v2-unmigrated"] {
            std::fs::create_dir_all(tmp.path().join(kept)).unwrap();
            std::fs::write(tmp.path().join(kept).join("f"), b"kept").unwrap();
        }
        std::fs::write(tmp.path().join("stray"), b"cleared").unwrap();
        manager.clear().await.unwrap();
        for kept in ["logs", "v2-unmigrated"] {
            assert_eq!(std::fs::read(tmp.path().join(kept).join("f")).unwrap(), b"kept");
        }
        assert!(!tmp.path().join("stray").exists());
        StorageManager::stop(&mut manager).await;
    }

    /// Opus-h r1 n2: storage a newer client wrote is refused, as the direct open refuses it, and
    /// not cleared: a downgrade keeps the store.
    #[tokio::test]
    async fn a_newer_storage_version_is_refused_not_cleared() {
        // With or without a clear to finish: that intent is the newer client's (r6 n1).
        for version in [&br#"{"version": 5}"#[..], br#"{"version": 5, "clearing": true}"#] {
            let tmp = TempDir::new().unwrap();
            std::fs::write(tmp.path().join("version.json"), version).unwrap();
            std::fs::create_dir(tmp.path().join("spv")).unwrap();
            std::fs::write(tmp.path().join("spv").join("kv.sqlite"), b"newer").unwrap();
            let config = crate::ClientConfig::testnet().with_storage_path(tmp.path());
            assert!(DiskStorageManager::new(&config).await.is_err());
            assert_eq!(std::fs::read(tmp.path().join("version.json")).unwrap(), version);
            assert_eq!(std::fs::read(tmp.path().join("spv").join("kv.sqlite")).unwrap(), b"newer");
        }
    }

    /// Opus-h r2 m1: the lock lives as long as the store, not the manager. A handle that
    /// outlives its manager keeps a second opener out until it drops; so does a worker sync that
    /// upgraded one, which is how a drop without `stop` never writes unlocked.
    #[tokio::test]
    async fn a_store_handle_keeps_the_directory_locked() {
        let tmp = TempDir::new().unwrap();
        let config = ClientConfig::testnet().with_storage_path(tmp.path());
        let first = DiskStorageManager::new(&config).await.unwrap();
        let held = first.block_headers();
        drop(first);
        let second = DiskStorageManager::new(&config).await;
        assert!(
            matches!(second, Err(StorageError::DirectoryLocked(_))),
            "{:?}",
            second.as_ref().err()
        );
        drop(held);
        DiskStorageManager::new(&config).await.unwrap();
    }

    #[tokio::test]
    async fn test_lock_lifecycle() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let path = temp_dir.path().to_path_buf();
        let lock_path = {
            let mut lock_file = path.clone();
            lock_file.set_extension("lock");
            lock_file
        };
        let config = ClientConfig::regtest().with_storage_path(path);

        let mut storage1 = DiskStorageManager::new(&config).await.unwrap();
        assert!(lock_path.exists(), "Lock file should exist while storage is open");
        storage1.clear().await.expect("Failed to clear the storage");
        assert!(lock_path.exists(), "Lock file should exist after storage is cleared");

        let storage2 = DiskStorageManager::new(&config).await;
        assert!(storage2.is_err(), "Second storage manager should fail");

        // Lock file removed when storage drops (on Unix: elsewhere it stays, see `LockFile`)
        drop(storage1);
        #[cfg(unix)]
        assert!(!lock_path.exists(), "Lock file should be removed after storage drops");

        // Can reopen storage after previous one dropped
        let storage3 = DiskStorageManager::new(&config).await;
        assert!(storage3.is_ok(), "Should reopen after previous storage dropped");
    }
}
