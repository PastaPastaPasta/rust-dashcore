//! Header storage operations for DiskStorageManager.

use std::ops::Range;

use crate::error::{StorageError, StorageResult};
use crate::storage::handle::SpvStore;
use crate::storage::store::legacy::RawHeader;
use crate::storage::store::Bounds;
use crate::types::HashedBlockHeader;
use async_trait::async_trait;
use dashcore::block::Header as BlockHeader;
use dashcore::consensus::{deserialize, Encodable};
use dashcore::prelude::CoreBlockHeight;
use dashcore::BlockHash;
use dashcore_hashes::Hash;

#[derive(Debug, PartialEq)]
pub struct BlockHeaderTip {
    height: CoreBlockHeight,
    header: BlockHeader,
    hash: BlockHash,
}

impl BlockHeaderTip {
    pub fn new(height: CoreBlockHeight, hashed_block_header: HashedBlockHeader) -> Self {
        Self {
            height,
            header: *hashed_block_header.header(),
            hash: *hashed_block_header.hash(),
        }
    }
    pub fn height(&self) -> CoreBlockHeight {
        self.height
    }
    pub fn header(&self) -> &BlockHeader {
        &self.header
    }
    pub fn hash(&self) -> &BlockHash {
        &self.hash
    }
}

#[async_trait]
pub trait BlockHeaderStorage: Send + Sync + 'static {
    async fn store_headers(&mut self, headers: &[HashedBlockHeader]) -> StorageResult<()>;

    async fn store_headers_at_height(
        &mut self,
        headers: &[HashedBlockHeader],
        height: u32,
    ) -> StorageResult<()>;

    /// Load a contiguous range of headers by height.
    ///
    /// Returns `StorageError::InvalidArgument` when the range extends outside the
    /// stored heights. Callers must clamp the range to at most `get_tip_height`.
    async fn load_headers(&self, range: Range<u32>) -> StorageResult<Vec<HashedBlockHeader>>;

    async fn get_header(&self, height: u32) -> StorageResult<Option<HashedBlockHeader>> {
        if let Some(tip_height) = self.get_tip_height().await {
            if height > tip_height {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }

        if let Some(start_height) = self.get_start_height().await {
            if height < start_height {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }

        Ok(self.load_headers(height..height + 1).await?.into_iter().next())
    }

    async fn get_tip_height(&self) -> Option<u32>;

    async fn get_tip(&self) -> Option<BlockHeaderTip>;

    async fn get_start_height(&self) -> Option<u32>;

    async fn get_stored_headers_len(&self) -> u32;

    async fn get_header_height_by_hash(
        &self,
        hash: &dashcore::BlockHash,
    ) -> StorageResult<Option<u32>>;

    /// Drop all headers with `height > target_height`.
    ///
    /// Truncating above the current tip is a no-op, truncating below
    /// `start_height` returns an error. The truncation commits Buffered: a power
    /// loss before the next Durable barrier may undo it, never in part.
    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()>;
}

/// The block headers of the SPV store: its `active` projection, by height (SB13).
pub struct PersistentBlockHeaderStorage {
    store: SpvStore,
}

impl PersistentBlockHeaderStorage {
    pub(crate) fn new(store: SpvStore) -> Self {
        Self {
            store,
        }
    }

    /// Headers alone, in a store of their own in the storage directory `dir` (tests and
    /// tools).
    pub fn open(dir: &std::path::Path) -> StorageResult<Self> {
        Ok(Self::new(SpvStore::open(dir)?))
    }
}

/// v2's bench-facing API (D9). `persist` keeps v2's contract: when it returns, every stored
/// header survives a power cut, so it is `sync_wal` (a raise with every SQLite commit Durable),
/// not the Buffered raise the worker's tick may take (SB14 D40).
#[async_trait::async_trait]
impl crate::storage::PersistentStorage for PersistentBlockHeaderStorage {
    async fn open(storage_path: impl Into<std::path::PathBuf> + Send) -> StorageResult<Self> {
        let dir = storage_path.into();
        tokio::task::spawn_blocking(move || PersistentBlockHeaderStorage::open(&dir))
            .await
            .map_err(|e| crate::error::StorageError::Io(std::io::Error::other(e)))?
    }

    /// Makes every stored header durable (the storage path is the store's own).
    async fn persist(
        &mut self,
        _storage_path: impl Into<std::path::PathBuf> + Send,
    ) -> StorageResult<()> {
        self.store.run(|s| s.sync_wal()).await
    }
}

/// The headers as records, each encoded in place (no allocation per header).
fn raw(headers: &[HashedBlockHeader]) -> Vec<RawHeader> {
    headers
        .iter()
        .map(|h| {
            let mut bytes = [0u8; 80];
            let n = h
                .header()
                .consensus_encode(&mut &mut bytes[..])
                .expect("a block header fits 80 bytes");
            assert_eq!(n, 80, "a block header encodes to 80 bytes");
            (*h.hash().as_byte_array(), bytes)
        })
        .collect()
}

fn hashed((hash, header): RawHeader) -> StorageResult<HashedBlockHeader> {
    let header: BlockHeader = deserialize(&header)
        .map_err(|e| StorageError::Corruption(format!("stored header does not decode: {e}")))?;
    Ok(HashedBlockHeader::with_trusted_hash(header, BlockHash::from_byte_array(hash)))
}

#[async_trait]
impl BlockHeaderStorage for PersistentBlockHeaderStorage {
    async fn store_headers(&mut self, headers: &[HashedBlockHeader]) -> StorageResult<()> {
        let height = self.store.view().active.hi.map_or(0, |h| h + 1);
        self.store_headers_at_height(headers, height).await
    }

    async fn store_headers_at_height(
        &mut self,
        headers: &[HashedBlockHeader],
        height: u32,
    ) -> StorageResult<()> {
        self.store.put_headers(height, raw(headers)).await
    }

    async fn load_headers(&self, range: Range<u32>) -> StorageResult<Vec<HashedBlockHeader>> {
        self.store.run(move |s| s.headers(range)).await?.into_iter().map(hashed).collect()
    }

    async fn get_tip_height(&self) -> Option<u32> {
        self.store.view().active.hi
    }

    async fn get_tip(&self) -> Option<BlockHeaderTip> {
        let tip = self.store.view().active.hi?;
        let header = self.load_headers(tip..tip + 1).await.ok()?.into_iter().next()?;
        Some(BlockHeaderTip::new(tip, header))
    }

    async fn get_start_height(&self) -> Option<u32> {
        self.store.view().active.lo
    }

    async fn get_stored_headers_len(&self) -> u32 {
        match self.store.view().active {
            Bounds {
                lo: Some(lo),
                hi: Some(hi),
            } => hi - lo + 1,
            _ => 0,
        }
    }

    async fn get_header_height_by_hash(
        &self,
        hash: &dashcore::BlockHash,
    ) -> StorageResult<Option<u32>> {
        let hash = *hash.as_byte_array();
        self.store.run(move |s| s.active_height_of(&hash)).await
    }

    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()> {
        self.store.run(move |s| s.truncate_headers(target_height)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::PersistentStorage;
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> PersistentBlockHeaderStorage {
        PersistentBlockHeaderStorage::open(dir.path()).unwrap()
    }

    fn hashed_batch(r: std::ops::Range<u32>) -> Vec<HashedBlockHeader> {
        BlockHeader::dummy_batch(r).into_iter().map(HashedBlockHeader::from).collect()
    }

    /// SB14 D39, Sol r1 #1: the public direct opens never migrate. Version-2 data (and unversioned
    /// data) is refused with every file as it was; `DiskStorageManager::new` migrates it, after
    /// which the direct open admits it. A new directory is made version 4.
    #[tokio::test]
    async fn a_direct_open_refuses_v2_data_unchanged() {
        let tmp = TempDir::new().unwrap();
        crate::test_utils::v2_fixture::V2Fixture {
            lo: 0,
            hi: 3_000,
            filter_every: 1,
            blocks: vec![5],
        }
        .write(tmp.path())
        .unwrap();
        let before = crate::storage::io::files_of(tmp.path());
        let refused = PersistentBlockHeaderStorage::open(tmp.path()).err().expect("refused");
        assert!(refused.to_string().contains("storage version 2"), "{refused}");
        assert!(<PersistentBlockHeaderStorage as PersistentStorage>::open(tmp.path())
            .await
            .is_err());
        assert!(SpvStore::open(tmp.path()).is_err());
        assert_eq!(crate::storage::io::files_of(tmp.path()), before, "nothing changed");

        let config = crate::ClientConfig::testnet().with_storage_path(tmp.path());
        let mut manager = crate::storage::DiskStorageManager::new(&config).await.unwrap();
        crate::storage::StorageManager::stop(&mut manager).await;
        drop(manager);
        assert_eq!(open(&tmp).get_tip_height().await, Some(2_999), "migrated, then admitted");

        let fresh = TempDir::new().unwrap();
        drop(open(&fresh));
        let version = std::fs::read_to_string(fresh.path().join("version.json")).unwrap();
        assert!(version.contains("\"version\": 4"), "{version}");
        drop(open(&fresh));

        let junk = TempDir::new().unwrap();
        std::fs::write(junk.path().join("filters"), b"x").unwrap();
        assert!(PersistentBlockHeaderStorage::open(junk.path()).is_err(), "unversioned data");
        assert!(!junk.path().join("version.json").exists());
    }

    /// SB14 D40, Sol r1 #2: `persist` leaves nothing Buffered, even when a fast append after a
    /// lowering started a run whose slot a plain raise commits Buffered (the control).
    #[tokio::test]
    async fn persist_leaves_no_buffered_commit() {
        let tmp = TempDir::new().unwrap();
        let mut storage = open(&tmp);
        storage.store_headers(&hashed_batch(0..6)).await.unwrap();
        storage.persist(tmp.path()).await.unwrap();
        storage.truncate_above(2).await.unwrap();
        storage.store_headers_at_height(&hashed_batch(100..102), 3).await.unwrap();
        storage.persist(tmp.path()).await.unwrap();
        assert!(!storage.store.store().has_buffered(), "persist is durable");

        storage.truncate_above(3).await.unwrap();
        storage.store_headers_at_height(&hashed_batch(200..202), 4).await.unwrap();
        storage.store.store().raise().unwrap();
        assert!(storage.store.store().has_buffered(), "the control: a raise may buffer");
    }

    /// SB14 D39: the direct open holds `<dir>.lock`, so it and `DiskStorageManager` exclude
    /// each other.
    #[tokio::test]
    async fn a_direct_open_takes_the_directory_lock() {
        let tmp = TempDir::new().unwrap();
        let config = crate::ClientConfig::testnet().with_storage_path(tmp.path());
        let held = open(&tmp);
        assert!(crate::storage::DiskStorageManager::new(&config).await.is_err());
        drop(held);
        let mut manager = crate::storage::DiskStorageManager::new(&config).await.unwrap();
        let lock = crate::storage::lock_file_path(tmp.path());
        let before = std::fs::read(&lock).unwrap();
        assert!(PersistentBlockHeaderStorage::open(tmp.path()).is_err());
        assert_eq!(std::fs::read(&lock).unwrap(), before, "the refusal changed the holder's lock");
        crate::storage::StorageManager::stop(&mut manager).await;
    }

    #[tokio::test]
    async fn test_get_tip() {
        let headers = hashed_batch(0..5);
        let tmp_dir = TempDir::new().unwrap();
        let mut storage = open(&tmp_dir);
        // Tip should be none before storing headers
        assert!(storage.get_tip().await.is_none());
        // Add one header and validate tip
        storage.store_headers(&headers[0..1]).await.unwrap();
        let tip = storage.get_tip().await.unwrap();
        let expected_tip = BlockHeaderTip::new(0, headers[0].clone());
        assert_eq!(tip, expected_tip);
        assert_eq!(storage.get_tip_height().await, Some(0));
        // Add multiple headers and validate tip
        storage.store_headers(&headers[1..]).await.unwrap();
        let tip = storage.get_tip().await.unwrap();
        let expected_tip = BlockHeaderTip::new(4, headers[4].clone());
        assert_eq!(tip, expected_tip);
        assert_eq!(storage.get_tip_height().await, Some(4));
    }

    #[tokio::test]
    async fn test_truncate_above_drops_index_entries_and_allows_restore() {
        let tmp_dir = TempDir::new().unwrap();
        let mut storage = open(&tmp_dir);

        let headers = hashed_batch(0..10);
        storage.store_headers(&headers).await.unwrap();

        let orphaned_hash = *headers[7].hash();
        assert_eq!(storage.get_header_height_by_hash(&orphaned_hash).await.unwrap(), Some(7));

        storage.truncate_above(100).await.unwrap();
        assert_eq!(storage.get_tip_height().await, Some(9));
        assert_eq!(storage.get_header_height_by_hash(headers[4].hash()).await.unwrap(), Some(4));

        storage.truncate_above(5).await.unwrap();

        assert_eq!(storage.get_tip_height().await, Some(5));
        assert_eq!(storage.get_header_height_by_hash(&orphaned_hash).await.unwrap(), None);

        let kept_hash = *headers[3].hash();
        assert_eq!(storage.get_header_height_by_hash(&kept_hash).await.unwrap(), Some(3));

        let replacement = hashed_batch(100..105);
        storage.store_headers_at_height(&replacement, 6).await.unwrap();
        assert_eq!(storage.get_tip_height().await, Some(10));

        let reloaded = storage.load_headers(6..11).await.unwrap();
        assert_eq!(reloaded, replacement);

        let new_hash = *replacement[0].hash();
        assert_eq!(storage.get_header_height_by_hash(&new_hash).await.unwrap(), Some(6));

        // Persisted and reopened, the truncated branch stays off the active chain. A drop alone
        // writes nothing (SB14 D8).
        storage.persist(tmp_dir.path()).await.unwrap();
        drop(storage);

        let reopened = open(&tmp_dir);
        assert_eq!(reopened.get_tip_height().await, Some(10));
        assert_eq!(reopened.get_header_height_by_hash(&orphaned_hash).await.unwrap(), None);
        assert_eq!(reopened.get_header_height_by_hash(&kept_hash).await.unwrap(), Some(3));
        assert_eq!(reopened.get_header_height_by_hash(&new_hash).await.unwrap(), Some(6));
    }
}
