use std::ops::Range;

use async_trait::async_trait;

use crate::error::StorageResult;
use crate::storage::handle::SpvStore;
use crate::storage::store::Subscriber;

#[async_trait]
pub trait FilterStorage: Send + Sync + 'static {
    async fn store_filter(&mut self, height: u32, filter: &[u8]) -> StorageResult<()>;

    /// Store the filters of heights `start`, `start + 1`, …; an implementation should do it in
    /// one write (the default stores them one by one).
    async fn store_filters(&mut self, start: u32, filters: Vec<Vec<u8>>) -> StorageResult<()> {
        for (height, filter) in (start..).zip(&filters) {
            self.store_filter(height, filter).await?;
        }
        Ok(())
    }

    /// Load a contiguous range of filters by height.
    ///
    /// Returns `StorageError::InvalidArgument` when the range extends outside the
    /// stored heights or holds a height with no row. Callers must clamp the range to at most `filter_tip_height`.
    async fn load_filters(&self, range: Range<u32>) -> StorageResult<Vec<Vec<u8>>>;

    async fn filter_tip_height(&self) -> StorageResult<u32>;

    /// Lowest filter height present in storage, or `None` when no filters are
    /// stored.
    ///
    /// `filter_tip_height` is a tip watermark only: storage is dense within
    /// `[filter_start_height, filter_tip_height]`, but heights below the start
    /// were never stored. Callers must not treat the tip as proof that storage
    /// is contiguous from an arbitrary lower height.
    async fn filter_start_height(&self) -> Option<u32>;

    /// Drop every stored filter, leaving the storage empty.
    ///
    /// Used when the stored range cannot serve the current scan (e.g. filters
    /// were previously stored from a checkpoint above a wallet's rescan
    /// start): keeping an unreachable island of tip-region filters would leave
    /// a permanent hole below it that the contiguous store path cannot fill.
    ///
    /// Like `truncate_above`, the deletion commits Buffered, with the locator.
    async fn clear_filters(&mut self) -> StorageResult<()>;

    /// Drop all filters with `height > target_height`.
    ///
    /// Truncating above the current tip is a no-op, truncating below
    /// `start_height` drops everything. The truncation commits Buffered, with the
    /// subscriber's locator: a power loss before the next Durable barrier may undo
    /// it, never in part.
    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()>;
}

/// The SPV store's `cfilter` subscriber table, with its locator.
pub struct PersistentFilterStorage {
    store: SpvStore,
}

impl PersistentFilterStorage {
    const SUB: Subscriber = Subscriber::Filters;

    pub(crate) fn new(store: SpvStore) -> Self {
        Self {
            store,
        }
    }
}

#[async_trait]
impl FilterStorage for PersistentFilterStorage {
    async fn store_filter(&mut self, height: u32, filter: &[u8]) -> StorageResult<()> {
        let filter = filter.to_vec();
        self.store.run(move |s| s.put_sub(Self::SUB, height, &[&filter])).await
    }

    async fn store_filters(&mut self, start: u32, filters: Vec<Vec<u8>>) -> StorageResult<()> {
        self.store
            .run(move |s| {
                let rows: Vec<&[u8]> = filters.iter().map(Vec::as_slice).collect();
                s.put_sub(Self::SUB, start, &rows)
            })
            .await
    }

    async fn load_filters(&self, range: Range<u32>) -> StorageResult<Vec<Vec<u8>>> {
        self.store.run(move |s| s.sub_range(Self::SUB, range)).await
    }

    async fn filter_tip_height(&self) -> StorageResult<u32> {
        Ok(self.store.view().sub(Self::SUB).hi.unwrap_or(0))
    }

    async fn filter_start_height(&self) -> Option<u32> {
        self.store.view().sub(Self::SUB).lo
    }

    async fn clear_filters(&mut self) -> StorageResult<()> {
        self.store.run(|s| s.clear_sub(Self::SUB, dash_kv::Commit::Buffered)).await
    }

    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()> {
        self.store.run(move |s| s.truncate_sub(Self::SUB, target_height)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn filter_bytes(seed: u8) -> Vec<u8> {
        vec![seed; 8]
    }

    #[tokio::test]
    async fn test_truncate_above_wrapper_smoke() {
        let tmp_dir = TempDir::new().unwrap();
        let mut storage = PersistentFilterStorage::new(SpvStore::open(tmp_dir.path()).unwrap());

        for height in 0..5 {
            storage.store_filter(height, &filter_bytes(height as u8)).await.unwrap();
        }

        storage.truncate_above(2).await.unwrap();

        assert_eq!(storage.filter_tip_height().await.unwrap(), 2);
        assert!(storage.load_filters(3..4).await.is_err());
    }

    /// A batch is one write of consecutive heights; it reads back as stored, and extends the
    /// bounds as single stores do.
    #[tokio::test]
    async fn test_store_filters_batch() {
        let tmp_dir = TempDir::new().unwrap();
        let mut storage = PersistentFilterStorage::new(SpvStore::open(tmp_dir.path()).unwrap());

        storage.store_filters(10, vec![]).await.unwrap();
        assert_eq!(storage.filter_start_height().await, None);

        storage.store_filters(10, (10..20).map(filter_bytes).collect()).await.unwrap();
        storage.store_filter(20, &filter_bytes(20)).await.unwrap();
        storage.store_filters(21, (21..25).map(filter_bytes).collect()).await.unwrap();
        assert_eq!(storage.filter_start_height().await, Some(10));
        assert_eq!(storage.filter_tip_height().await.unwrap(), 24);
        let want: Vec<Vec<u8>> = (10..25).map(filter_bytes).collect();
        assert_eq!(storage.load_filters(10..25).await.unwrap(), want);
    }

    /// Filters stored only in a high region (as when headers previously
    /// synced from a checkpoint): the start accessor reports the true lowest
    /// stored height, reads below it fail loudly instead of returning
    /// sentinels, and `clear_filters` durably empties the storage.
    #[tokio::test]
    async fn test_filter_start_height_and_clear_filters() {
        let tmp_dir = TempDir::new().unwrap();
        let mut storage = PersistentFilterStorage::new(SpvStore::open(tmp_dir.path()).unwrap());

        assert_eq!(storage.filter_start_height().await, None);

        for height in 900..=1000 {
            storage.store_filter(height, &filter_bytes(height as u8)).await.unwrap();
        }
        assert_eq!(storage.filter_start_height().await, Some(900));
        assert_eq!(storage.filter_tip_height().await.unwrap(), 1000);

        // Reads that begin below the stored range must error, not hand back
        // sentinel data zipped against real headers.
        assert!(storage.load_filters(100..200).await.is_err());
        assert!(storage.load_filters(850..950).await.is_err());
        assert_eq!(storage.load_filters(900..1001).await.unwrap().len(), 101);

        // The start height survives a reload.
        drop(storage);
        let mut storage = PersistentFilterStorage::new(SpvStore::open(tmp_dir.path()).unwrap());
        assert_eq!(storage.filter_start_height().await, Some(900));

        storage.clear_filters().await.unwrap();
        assert_eq!(storage.filter_start_height().await, None);
        assert_eq!(storage.filter_tip_height().await.unwrap(), 0);
        assert!(storage.load_filters(900..901).await.is_err());

        drop(storage);

        // Reopen: the clear is kept, and the storage accepts a fresh
        // contiguous range from a lower start.
        let mut storage = PersistentFilterStorage::new(SpvStore::open(tmp_dir.path()).unwrap());
        assert_eq!(storage.filter_start_height().await, None);
        assert_eq!(storage.filter_tip_height().await.unwrap(), 0);

        for height in 100..=110 {
            storage.store_filter(height, &filter_bytes(height as u8)).await.unwrap();
        }
        assert_eq!(storage.filter_start_height().await, Some(100));
        assert_eq!(storage.load_filters(100..111).await.unwrap().len(), 11);
    }
}
