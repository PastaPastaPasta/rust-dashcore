use crate::error::{StorageError, StorageResult};
use crate::storage::handle::SpvStore;
use crate::storage::store::Subscriber;
use async_trait::async_trait;
use dashcore::hash_types::FilterHeader;
use dashcore_hashes::Hash;
use std::ops::Range;

#[async_trait]
pub trait FilterHeaderStorage: Send + Sync + 'static {
    async fn store_filter_headers(&mut self, headers: &[FilterHeader]) -> StorageResult<()>;

    async fn store_filter_headers_at_height(
        &mut self,
        headers: &[FilterHeader],
        height: u32,
    ) -> StorageResult<()>;

    /// Load a contiguous range of filter headers by height.
    ///
    /// Returns `StorageError::InvalidArgument` when the range extends outside the
    /// stored heights or holds a height with no row. Callers must clamp the range to at most `get_filter_tip_height`.
    async fn load_filter_headers(&self, range: Range<u32>) -> StorageResult<Vec<FilterHeader>>;

    async fn get_filter_header(&self, height: u32) -> StorageResult<Option<FilterHeader>> {
        if let Some(tip_height) = self.get_filter_tip_height().await? {
            if height > tip_height {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }

        if let Some(start_height) = self.get_filter_start_height().await {
            if height < start_height {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }

        Ok(self.load_filter_headers(height..height + 1).await?.first().copied())
    }

    async fn get_filter_tip_height(&self) -> StorageResult<Option<u32>>;

    async fn get_filter_start_height(&self) -> Option<u32>;

    /// Drop all filter headers with `height > target_height`.
    ///
    /// Truncating above the current tip is a no-op, truncating below
    /// `start_height` returns an error. The truncation commits Buffered, with the
    /// subscriber's locator: a power loss before the next Durable barrier may undo
    /// it, never in part.
    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()>;
}

/// The SPV store's `cfhdr` subscriber table, with its locator.
pub struct PersistentFilterHeaderStorage {
    store: SpvStore,
}

impl PersistentFilterHeaderStorage {
    const SUB: Subscriber = Subscriber::FilterHeaders;

    pub(crate) fn new(store: SpvStore) -> Self {
        Self {
            store,
        }
    }
}

#[async_trait]
impl FilterHeaderStorage for PersistentFilterHeaderStorage {
    async fn store_filter_headers(&mut self, headers: &[FilterHeader]) -> StorageResult<()> {
        let height = self.store.view().sub(Self::SUB).hi.map_or(0, |h| h + 1);
        self.store_filter_headers_at_height(headers, height).await
    }

    async fn store_filter_headers_at_height(
        &mut self,
        headers: &[FilterHeader],
        height: u32,
    ) -> StorageResult<()> {
        let rows: Vec<[u8; 32]> = headers.iter().map(|h| h.to_byte_array()).collect();
        self.store
            .run(move |s| {
                let refs: Vec<&[u8]> = rows.iter().map(|r| &r[..]).collect();
                s.put_sub(Self::SUB, height, &refs)
            })
            .await
    }

    async fn load_filter_headers(&self, range: Range<u32>) -> StorageResult<Vec<FilterHeader>> {
        let rows = self.store.run(move |s| s.sub_range(Self::SUB, range)).await?;
        rows.into_iter()
            .map(|r| {
                let bytes: [u8; 32] = r.try_into().map_err(|r: Vec<u8>| {
                    StorageError::Corruption(format!("filter header of {} bytes", r.len()))
                })?;
                Ok(FilterHeader::from_byte_array(bytes))
            })
            .collect()
    }

    async fn get_filter_tip_height(&self) -> StorageResult<Option<u32>> {
        Ok(self.store.view().sub(Self::SUB).hi)
    }

    async fn get_filter_start_height(&self) -> Option<u32> {
        self.store.view().sub(Self::SUB).lo
    }

    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()> {
        self.store.run(move |s| s.truncate_sub(Self::SUB, target_height)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_truncate_above_wrapper_smoke() {
        let tmp_dir = TempDir::new().unwrap();
        let mut storage =
            PersistentFilterHeaderStorage::new(SpvStore::open(tmp_dir.path()).unwrap());

        let headers = FilterHeader::dummy_batch(0..5);
        storage.store_filter_headers(&headers).await.unwrap();

        storage.truncate_above(2).await.unwrap();

        assert_eq!(storage.get_filter_tip_height().await.unwrap(), Some(2));
        assert_eq!(storage.get_filter_header(3).await.unwrap(), None);
    }
}
