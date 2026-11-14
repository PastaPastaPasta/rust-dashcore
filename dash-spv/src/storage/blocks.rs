//! Block storage for persisting full blocks that contain wallet-relevant transactions.

use crate::error::StorageResult;
use crate::storage::handle::SpvStore;
use crate::storage::store::{Error as StoreError, Subscriber};
use crate::types::HashedBlock;
use async_trait::async_trait;
use dashcore::consensus::{deserialize, serialize};
use dashcore::prelude::CoreBlockHeight;

/// Trait for block storage operations.
#[async_trait]
pub trait BlockStorage: Send + Sync + 'static {
    /// Store a block at a specific height.
    async fn store_block(
        &mut self,
        height: CoreBlockHeight,
        block: HashedBlock,
    ) -> StorageResult<()>;

    /// Load a block by height, refusing hash or transaction-merkle mismatches (D54).
    async fn load_block(&self, height: CoreBlockHeight) -> StorageResult<Option<HashedBlock>>;

    /// Drop all blocks with `height > target_height`.
    ///
    /// Truncating above the current tip is a no-op, truncating below
    /// `start_height` drops everything. The truncation commits Buffered, with the
    /// subscriber's locator: a power loss before the next Durable barrier may undo
    /// it, never in part.
    async fn truncate_above(&mut self, target_height: CoreBlockHeight) -> StorageResult<()>;
}

/// The SPV store's `blocks` subscriber table (wallet-matched blocks), with its locator.
pub struct PersistentBlockStorage {
    store: SpvStore,
}

impl PersistentBlockStorage {
    const SUB: Subscriber = Subscriber::Blocks;

    pub(crate) fn new(store: SpvStore) -> Self {
        Self {
            store,
        }
    }
}

#[async_trait]
impl BlockStorage for PersistentBlockStorage {
    async fn store_block(&mut self, height: u32, hashed_block: HashedBlock) -> StorageResult<()> {
        let bytes = serialize(&hashed_block);
        self.store.run(move |s| s.put_sub(Self::SUB, height, &[&bytes])).await
    }

    async fn load_block(&self, height: u32) -> StorageResult<Option<HashedBlock>> {
        self.store
            .run(move |s| {
                let Some(bytes) = s.sub_get(Self::SUB, height)? else {
                    return Ok(None);
                };
                let block: HashedBlock = deserialize(&bytes)
                    .map_err(|e| StoreError::Corrupt(format!("stored block {height}: {e}")))?;
                if block.block().header.block_hash() != *block.hash() {
                    return Err(StoreError::Corrupt(format!(
                        "stored block {height}: header hash mismatch"
                    )));
                }
                if !block.block().check_merkle_root() {
                    return Err(StoreError::Corrupt(format!(
                        "stored block {height}: merkle root mismatch"
                    )));
                }
                Ok(Some(block))
            })
            .await
    }

    async fn truncate_above(&mut self, target_height: u32) -> StorageResult<()> {
        self.store.run(move |s| s.truncate_sub(Self::SUB, target_height)).await
    }
}

#[cfg(test)]
mod tests {
    use crate::error::StorageError;
    use dashcore::{Address, Block, Network, Transaction};
    use tempfile::TempDir;

    use super::*;

    fn three_transactions() -> HashedBlock {
        let address = Address::dummy(Network::Regtest, 0);
        let transactions = (0..3).map(|i| Transaction::dummy_coinbase(&address, 50 + i)).collect();
        let mut block = Block::dummy(100, transactions);
        block.header.merkle_root = block.compute_merkle_root().unwrap();
        HashedBlock::from(block)
    }

    async fn corruption_witness(change_hash: bool) {
        let temp = TempDir::new().unwrap();
        let store = SpvStore::open(temp.path()).unwrap();
        let mut storage = PersistentBlockStorage::new(store.clone());
        let original = three_transactions();
        storage.store_block(100, original.clone()).await.unwrap();
        let loaded = storage.load_block(100).await.unwrap().unwrap();
        assert_eq!(loaded.block(), original.block());
        assert_eq!(loaded.hash(), original.hash());
        let mut bytes = serialize(&original);
        if change_hash {
            bytes[0] ^= 1;
        } else {
            let mut changed = original.block().clone();
            changed.txdata[1].output[0].value ^= 1;
            let mut expected = serialize(original.hash());
            expected.extend(serialize(&changed));
            let differences: Vec<_> = bytes
                .iter()
                .zip(&expected)
                .enumerate()
                .filter_map(|(i, (a, b))| (a != b).then_some(i))
                .collect();
            assert_eq!(differences.len(), 1);
            bytes[differences[0]] ^= 1;
            assert_eq!(bytes, expected);
        }
        let decoded: HashedBlock = deserialize(&bytes).unwrap();
        if change_hash {
            assert!(decoded.block().check_merkle_root());
            assert_ne!(*decoded.hash(), decoded.block().header.block_hash());
        } else {
            assert_eq!(*decoded.hash(), decoded.block().header.block_hash());
            assert!(!decoded.block().check_merkle_root());
        }
        store.run(move |s| s.put_sub(Subscriber::Blocks, 100, &[&bytes])).await.unwrap();
        assert!(matches!(storage.load_block(100).await, Err(StorageError::Corruption(_))));
    }

    #[tokio::test]
    async fn a_stored_block_that_is_not_its_stored_hash_is_not_served() {
        corruption_witness(true).await;
    }

    #[tokio::test]
    async fn a_stored_block_with_a_changed_transaction_is_not_served() {
        corruption_witness(false).await;
    }

    #[tokio::test]
    async fn test_store_and_load_block() {
        let temp_dir = TempDir::new().unwrap();
        let mut storage = PersistentBlockStorage::new(SpvStore::open(temp_dir.path()).unwrap());

        let hashed_block = HashedBlock::dummy(100, vec![]);
        storage.store_block(100, hashed_block.clone()).await.unwrap();

        let loaded = storage.load_block(100).await.unwrap();
        assert_eq!(loaded, Some(hashed_block));
    }

    #[tokio::test]
    async fn test_persistence_across_reopen() {
        let temp_dir = TempDir::new().unwrap();
        let hashed_block = HashedBlock::dummy(100, vec![]);

        {
            let mut storage = PersistentBlockStorage::new(SpvStore::open(temp_dir.path()).unwrap());
            storage.store_block(100, hashed_block.clone()).await.unwrap();
        }

        {
            let storage = PersistentBlockStorage::new(SpvStore::open(temp_dir.path()).unwrap());
            let loaded = storage.load_block(100).await.unwrap();
            assert_eq!(loaded, Some(hashed_block));
        }
    }

    #[tokio::test]
    async fn test_load_nonexistent_block() {
        let temp_dir = TempDir::new().unwrap();
        let storage = PersistentBlockStorage::new(SpvStore::open(temp_dir.path()).unwrap());

        let loaded = storage.load_block(999).await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn test_truncate_above_wrapper_smoke() {
        let temp_dir = TempDir::new().unwrap();
        let mut storage = PersistentBlockStorage::new(SpvStore::open(temp_dir.path()).unwrap());

        for height in 0..5 {
            storage.store_block(height, HashedBlock::dummy(height, vec![])).await.unwrap();
        }

        storage.truncate_above(2).await.unwrap();

        assert_eq!(storage.load_block(2).await.unwrap(), Some(HashedBlock::dummy(2, vec![])));
        assert_eq!(storage.load_block(3).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_returns_none_for_gaps() {
        let temp_dir = TempDir::new().unwrap();
        let mut storage = PersistentBlockStorage::new(SpvStore::open(temp_dir.path()).unwrap());

        // Store blocks at non-contiguous height
        let hashed_block_1 = HashedBlock::dummy(100, vec![]);
        let hashed_block_2 = HashedBlock::dummy(200, vec![]);

        storage.store_block(100, hashed_block_1.clone()).await.unwrap();
        storage.store_block(200, hashed_block_2.clone()).await.unwrap();

        // Stored blocks should load correctly
        assert_eq!(storage.load_block(100).await.unwrap(), Some(hashed_block_1));
        assert_eq!(storage.load_block(200).await.unwrap(), Some(hashed_block_2));

        // Height in between (gap) should return None, not a sentinel
        assert_eq!(storage.load_block(150).await.unwrap(), None);

        // Heights outside range should also return None
        assert_eq!(storage.load_block(50).await.unwrap(), None);
        assert_eq!(storage.load_block(250).await.unwrap(), None);
    }
}
