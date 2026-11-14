//! Header storage operations for DiskStorageManager.

use std::ops::Range;

use crate::error::{StorageError, StorageResult};
use crate::storage::handle::SpvStore;
use crate::storage::store::legacy::RawHeader;
use crate::types::HashedBlockHeader;
use async_trait::async_trait;
use dashcore::block::Header as BlockHeader;
use dashcore::consensus::{deserialize, encode::Error as DecodeError, Encodable};
use dashcore::prelude::CoreBlockHeight;
use dashcore::BlockHash;

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

    /// Prepends when `height + headers.len()` equals the stored start.
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
    #[cfg(any(test, feature = "test-utils"))]
    pub fn open(dir: &std::path::Path) -> StorageResult<Self> {
        Ok(Self::new(SpvStore::open(dir)?))
    }
}

/// v2's bench-facing API (D9). `persist` keeps v2's contract: when it returns, every stored
/// header survives a power cut, so it is `sync_wal` (a raise with every SQLite commit Durable),
/// not the Buffered raise the worker's tick may take (SB14 D40).
#[cfg(any(test, feature = "test-utils"))]
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

#[derive(Debug, thiserror::Error)]
enum Mismatch {
    #[error("stored header {height}: does not decode: {error}")]
    Undecodable {
        height: u32,
        error: DecodeError,
    },
    #[error("stored header {height}: the stored hash {stored} is not its bytes' {computed}")]
    Hash {
        height: u32,
        stored: BlockHash,
        computed: BlockHash,
    },
}

impl Mismatch {
    fn height(&self) -> u32 {
        match self {
            Self::Undecodable {
                height,
                ..
            }
            | Self::Hash {
                height,
                ..
            } => *height,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirmation {
    Uncommitted,
    Sound,
    Damaged,
    Io,
}

fn should_mark(read: u64, current: u64, marked: Option<u64>, verdict: Confirmation) -> bool {
    marked != Some(read) && read == current && verdict == Confirmation::Damaged
}

fn mark_if_confirmed(
    s: &crate::storage::handle::SpvStoreInner,
    generation: u64,
    mismatch: &Mismatch,
) {
    let mut marked = s.clearing();
    if !should_mark(generation, s.generation(), *marked, Confirmation::Damaged) {
        tracing::warn!(
            "header mismatch already marked or stale at generation {generation}: {mismatch}"
        );
        return;
    }
    let verdict = match s.committed_header_from_file(mismatch.height()) {
        Ok(Some(raw)) if hashed(mismatch.height(), raw).is_err() => Confirmation::Damaged,
        Ok(Some(_)) => Confirmation::Sound,
        Ok(None) => Confirmation::Uncommitted,
        Err(e) => {
            tracing::error!("cannot confirm header mismatch: {e}");
            Confirmation::Io
        }
    };
    if !should_mark(generation, s.generation(), *marked, verdict) {
        tracing::warn!("header mismatch is not confirmed on disk: {mismatch}");
        return;
    }
    let write = || {
        #[cfg(test)]
        if s.integrity.fail_mark.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return Err(std::io::Error::other("injected intent write failure"));
        }
        crate::storage::migrator::mark_mismatch_sync(s.storage_path(), generation)
    };
    match write() {
        Ok(()) => {
            #[cfg(test)]
            s.integrity.writes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *marked = Some(generation);
        }
        Err(e) => tracing::error!("the clearing intent was not written: {e}"),
    }
}

fn hashed(height: u32, (stored, bytes): RawHeader) -> Result<HashedBlockHeader, Mismatch> {
    let header: BlockHeader = deserialize(&bytes).map_err(|error| Mismatch::Undecodable {
        height,
        error,
    })?;
    let pair = HashedBlockHeader::from(header);
    if pair.hash().as_byte_array() != &stored {
        return Err(Mismatch::Hash {
            height,
            stored: BlockHash::from_byte_array(stored),
            computed: *pair.hash(),
        });
    }
    Ok(pair)
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
        let start = range.start;
        self.store
            .run(move |s| {
                let (generation, raw) = s.headers_at(range)?;
                #[cfg(test)]
                s.integrity.after_copy();
                let result = raw
                    .into_iter()
                    .zip(start..)
                    .map(|(raw, height)| hashed(height, raw))
                    .collect::<Result<Vec<_>, _>>();
                if let Err(mismatch) = &result {
                    mark_if_confirmed(s, generation, mismatch);
                }
                Ok(result)
            })
            .await?
            .map_err(|mismatch| {
                tracing::error!("{mismatch}");
                StorageError::Corruption(mismatch.to_string())
            })
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
    #[test]
    fn mismatch_mark_decision_table() {
        use super::{should_mark, Confirmation};
        for (read, current, marked, verdict, expected) in [
            (1, 2, None, Confirmation::Damaged, false),
            (2, 2, None, Confirmation::Uncommitted, false),
            (2, 2, None, Confirmation::Sound, false),
            (2, 2, None, Confirmation::Io, false),
            (2, 2, None, Confirmation::Damaged, true),
            (2, 2, Some(2), Confirmation::Damaged, false),
        ] {
            assert_eq!(should_mark(read, current, marked, verdict), expected);
        }
    }

    use std::fs::OpenOptions;
    use std::io::{Read, Seek, SeekFrom, Write};

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

    fn real_chain(n: usize) -> Vec<HashedBlockHeader> {
        BlockHeader::dummy_chain(n, BlockHash::all_zeros())
            .into_iter()
            .map(HashedBlockHeader::from)
            .collect()
    }

    async fn persist_headers(tmp: &TempDir, headers: &[HashedBlockHeader]) {
        let mut storage = open(tmp);
        storage.store_headers(headers).await.unwrap();
        storage.persist(tmp.path()).await.unwrap();
    }

    fn poke(tmp: &TempDir, at: u64) {
        let segment = tmp.path().join("block_headers/segment_000000.dat");
        let mut file = OpenOptions::new().read(true).write(true).open(segment).unwrap();
        file.seek(SeekFrom::Start(at)).unwrap();
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        file.seek(SeekFrom::Start(at)).unwrap();
        file.write_all(&[byte[0] ^ 1]).unwrap();
    }

    fn assert_same(actual: &HashedBlockHeader, expected: &HashedBlockHeader) {
        assert_eq!(actual.header(), expected.header());
        assert_eq!(actual.hash(), expected.hash());
    }

    #[tokio::test]
    async fn a_body_that_is_not_its_stored_hash_is_not_served() {
        let tmp = TempDir::new().unwrap();
        let headers = real_chain(8);
        persist_headers(&tmp, &headers).await;
        poke(&tmp, 5 * 112 + 40);

        let reopened = PersistentBlockHeaderStorage::open(tmp.path()).expect("open admits it");
        let point = reopened.get_header(5).await;
        let range = reopened.load_headers(0..8).await;
        assert!(matches!(point, Err(StorageError::Corruption(_))), "point read served {point:?}");
        assert!(matches!(range, Err(StorageError::Corruption(_))), "range read served {range:?}");
        assert_same(&reopened.get_header(4).await.unwrap().unwrap(), &headers[4]);
        assert_same(&reopened.get_header(6).await.unwrap().unwrap(), &headers[6]);
    }

    #[tokio::test]
    async fn a_body_changed_after_a_good_read_is_not_served() {
        let tmp = TempDir::new().unwrap();
        let headers = real_chain(8);
        persist_headers(&tmp, &headers).await;
        let reopened = open(&tmp);
        assert_same(&reopened.get_header(5).await.unwrap().unwrap(), &headers[5]);
        let all = reopened.load_headers(0..8).await.unwrap();
        assert_eq!(all.len(), headers.len());
        for (actual, expected) in all.iter().zip(&headers) {
            assert_same(actual, expected);
        }

        poke(&tmp, 5 * 112 + 40);
        let point = reopened.get_header(5).await;
        let range = reopened.load_headers(0..8).await;
        assert!(matches!(point, Err(StorageError::Corruption(_))), "point read served {point:?}");
        assert!(matches!(range, Err(StorageError::Corruption(_))), "range read served {range:?}");
        assert_same(&reopened.get_header(4).await.unwrap().unwrap(), &headers[4]);
        assert_same(&reopened.get_header(6).await.unwrap().unwrap(), &headers[6]);
    }

    #[tokio::test]
    async fn a_stored_hash_that_is_not_its_body_is_not_served() {
        let tmp = TempDir::new().unwrap();
        let headers = hashed_batch(0..8);
        persist_headers(&tmp, &headers).await;
        poke(&tmp, 5 * 112 + 80 + 20);
        let hash = *headers[5].hash();
        let mut flipped = *hash.as_byte_array();
        flipped[20] ^= 1;
        let damaged = BlockHash::from_byte_array(flipped);

        let reopened = PersistentBlockHeaderStorage::open(tmp.path()).expect("open admits it");
        let served = reopened.get_header(5).await;
        assert!(matches!(served, Err(StorageError::Corruption(_))), "served {served:?}");
        assert_eq!(reopened.get_header_height_by_hash(&hash).await.unwrap(), None);
        assert_eq!(reopened.get_header_height_by_hash(&damaged).await.unwrap(), Some(5));
        assert_same(&reopened.get_header(4).await.unwrap().unwrap(), &headers[4]);
        assert_same(&reopened.get_header(6).await.unwrap().unwrap(), &headers[6]);
    }

    #[tokio::test]
    async fn a_damaged_tip_body_is_not_served() {
        let tmp = TempDir::new().unwrap();
        let headers = real_chain(8);
        persist_headers(&tmp, &headers).await;
        poke(&tmp, 7 * 112 + 40);

        let reopened = PersistentBlockHeaderStorage::open(tmp.path()).expect("open admits it");
        assert!(reopened.get_tip().await.is_none());
        assert!(matches!(reopened.get_header(7).await, Err(StorageError::Corruption(_))));
        assert_eq!(reopened.get_tip_height().await, Some(7));
        assert_same(&reopened.get_header(6).await.unwrap().unwrap(), &headers[6]);
    }

    #[tokio::test]
    async fn a_wrong_pair_written_through_the_trait_is_refused_on_read() {
        let tmp = TempDir::new().unwrap();
        let mut storage = open(&tmp);
        let header = BlockHeader::dummy(0);
        let wrong_hash = BlockHeader::dummy(1).block_hash();
        assert_ne!(header.block_hash(), wrong_hash);
        let pair = HashedBlockHeader::with_trusted_hash(header, wrong_hash);
        storage.store_headers(&[pair]).await.unwrap();
        assert!(matches!(storage.get_header(0).await, Err(StorageError::Corruption(_))));
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
