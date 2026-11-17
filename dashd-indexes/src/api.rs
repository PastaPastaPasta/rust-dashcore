// SPDX-License-Identifier: MIT

//! Index readers and the transaction lookup shared by node consumers.

use std::sync::Arc;

pub use dash_chain::BlockRef;
use dash_consensus::Block;
pub use dash_filters::{BlockFilter, FilterHash, FilterHeader};
pub use dash_primitives::{BlockHash, Transaction, TxHash as Txid};
use thiserror::Error;

/// The supported BIP157 filter type.
// Core: blockfilter.h:89-93
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FilterType {
    Basic = 0,
}

/// Index progress, with height zero before any block has been indexed.
// Core: index/base.cpp:531-538
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexSummary {
    pub name: &'static str,
    pub synced: bool,
    pub best_block_height: u32,
}

/// Reader failures; RPC handlers supply their own user-facing messages.
// Core: index/txindex.cpp:83-102
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum IndexError {
    #[error("index I/O: {0}")]
    Io(String),
    #[error("corrupt index: {0}")]
    Corrupt(String),
}

pub trait IndexStatus: Send + Sync {
    fn summary(&self) -> IndexSummary;

    /// Returns false during initial sync; otherwise waits for queued block notifications.
    /// Callers must not hold a chain lock while waiting.
    // Core: index/base.cpp:474-496
    fn block_until_synced(&self) -> bool;
}

/// A transaction and the hash of the header read from its block file.
pub type IndexedTx = (Arc<Transaction>, BlockHash);

pub trait TxIndexReader: IndexStatus {
    /// Finds and verifies the full txid, without checking active-chain membership.
    // Core: index/txindex.cpp:76-105
    fn find_tx(&self, txid: &Txid) -> Result<Option<IndexedTx>, IndexError>;

    /// Checks for an index entry, without a verification read of the block file.
    // Core: index/txindex.cpp:108-112
    fn has_tx(&self, txid: &Txid) -> Result<bool, IndexError>;
}

/// A collateral transaction with its active-chain location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedTx {
    pub tx: Arc<Transaction>,
    pub block_hash: BlockHash,
    pub active_height: Option<u32>,
    pub confirmations: u32,
}

impl LocatedTx {
    /// `active_height` must come from an active-chain membership check in the same view
    /// as `tip_height`. An unknown or stale block has no active height.
    // Core: governance/object.cpp:855-860
    pub fn new(
        tx: Arc<Transaction>,
        block_hash: BlockHash,
        active_height: Option<u32>,
        tip_height: u32,
    ) -> Self {
        let active_height = active_height.filter(|&height| height <= tip_height);
        let confirmations =
            active_height.map_or(0, |height| (tip_height - height).saturating_add(1));
        Self {
            tx,
            block_hash,
            active_height,
            confirmations,
        }
    }
}

/// Transaction indexing plus a coherent active-chain membership and confirmation check.
// Core: governance/object.cpp:798-801,855-860
pub trait TxLookup: Send + Sync {
    fn locate(&self, txid: &Txid) -> Result<Option<LocatedTx>, IndexError>;
}

/// An ancestor on the stop block's branch, including a stale branch.
pub trait Ancestry: Send + Sync {
    fn ancestor(&self, block: &BlockRef, height: u32) -> Option<BlockRef>;
}

/// Reads and verifies the block named by the supplied block-index reference.
pub trait BlockRead: Send + Sync {
    fn read_block(&self, block: &BlockRef) -> Result<Arc<Block>, IndexError>;
}

pub type MempoolLookup<'a> = dyn Fn(&Txid) -> Option<Arc<Transaction>> + 'a;
/// A transaction and its block, or no block for a mempool transaction.
pub type RetrievedTx = (Arc<Transaction>, Option<BlockHash>);

/// Looks in the mempool, txindex, then the explicitly requested block, in that order.
/// Read failures act as misses, as Core's logged `false` read results do.
/// The result uses the same error type as the other reader APIs.
// Core: node/transaction.cpp:126-156
pub fn get_transaction(
    block: Option<&BlockRef>,
    mempool: Option<&MempoolLookup<'_>>,
    txindex: Option<&dyn TxIndexReader>,
    blocks: &dyn BlockRead,
    txid: &Txid,
) -> Result<Option<RetrievedTx>, IndexError> {
    // Core: node/transaction.cpp:128-131
    if block.is_none()
        && let Some(tx) = mempool.and_then(|lookup| lookup(txid))
    {
        return Ok(Some((tx, None)));
    }
    // Core: node/transaction.cpp:132-144
    if let Some(index) = txindex
        && let Ok(Some((tx, hash))) = index.find_tx(txid)
        && block.is_none_or(|block| block.hash == hash)
    {
        return Ok(Some((tx, Some(hash))));
    }
    // Core: node/transaction.cpp:145-156
    if let Some(block) = block
        && let Ok(data) = blocks.read_block(block)
        && let Some(tx) = data.txs().iter().find(|tx| &tx.txid() == txid)
    {
        return Ok(Some((Arc::new(tx.raw().clone()), Some(block.hash))));
    }
    Ok(None)
}

/// BIP157 and `getblockfilter` reads. Ranges include both endpoints and follow `stop`'s
/// ancestry, returning filters in ascending height order. A missing entry returns `None`.
/// Readers fall back to the hash archive when a height row is absent or names another block.
// Core: index/blockfilterindex.cpp:439-540
pub trait FilterIndexReader: IndexStatus {
    fn filter(&self, block: &BlockRef) -> Result<Option<BlockFilter>, IndexError>;
    fn filter_header(&self, block: &BlockRef) -> Result<Option<FilterHeader>, IndexError>;
    fn filter_range(
        &self,
        start: u32,
        stop: &BlockRef,
        ancestry: &dyn Ancestry,
    ) -> Result<Option<Vec<BlockFilter>>, IndexError>;
    fn filter_hash_range(
        &self,
        start: u32,
        stop: &BlockRef,
        ancestry: &dyn Ancestry,
    ) -> Result<Option<Vec<FilterHash>>, IndexError>;
}

/// Coin statistics for a single block; monetary amounts are in duffs and may be negative.
// Core: index/coinstatsindex.cpp:31-62
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoinStatsEntry {
    pub muhash: [u8; 32],
    pub txouts: u64,
    pub bogo_size: u64,
    pub total_amount: i64,
    pub total_subsidy: i64,
    pub total_unspendable: i64,
    pub prevout_spent: i64,
    pub new_outputs_ex_coinbase: i64,
    pub coinbase: i64,
    pub unspendables_genesis: i64,
    pub unspendables_bip30: i64,
    pub unspendables_scripts: i64,
    pub unspendables_unclaimed_rewards: i64,
}

/// Reads active or stale blocks, falling back to the hash archive when the height row
/// is absent or names another block.
// Core: index/coinstatsindex.cpp:304-321
pub trait CoinStatsReader: IndexStatus {
    fn look_up_stats(&self, block: &BlockRef) -> Result<Option<CoinStatsEntry>, IndexError>;
}

pub trait IndexRegistry: Send + Sync {
    fn txindex(&self) -> Option<Arc<dyn TxIndexReader>>;
    fn filters(&self, ty: FilterType) -> Option<Arc<dyn FilterIndexReader>>;
    fn coinstats(&self) -> Option<Arc<dyn CoinStatsReader>>;

    /// Enabled indexes in Core's `getindexinfo` order: txindex, coinstatsindex, filters.
    // Core: rpc/node.cpp:1207-1217
    fn summaries(&self) -> Vec<IndexSummary>;
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use std::slice;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use dash_consensus::{BlockId, Header, Tx};
    use dash_primitives::{BlockHeader, TxType};
    use dash_types::Numeric as _;

    use super::*;
    use crate::testing::MemTxIndex;

    fn transaction(lock_time: u32) -> Arc<Transaction> {
        Arc::new(Transaction::new(2, TxType::Spend, vec![], vec![], lock_time, vec![]))
    }

    fn txid(tx: &Transaction) -> Txid {
        Tx::new(tx.clone()).txid()
    }

    struct Blocks {
        data: Result<Arc<Block>, IndexError>,
        reads: AtomicUsize,
    }

    impl Blocks {
        fn new(txs: &[Arc<Transaction>]) -> Self {
            let header = Header::new(BlockHeader::default());
            Self {
                data: Ok(Arc::new(Block::new(
                    header,
                    txs.iter().map(|tx| Arc::new(Tx::new(tx.as_ref().clone()))).collect(),
                ))),
                reads: AtomicUsize::new(0),
            }
        }

        fn reference(&self) -> BlockRef {
            BlockRef {
                id: BlockId(1),
                hash: self.data.as_ref().unwrap().hash(),
                height: 1,
            }
        }
    }

    impl BlockRead for Blocks {
        fn read_block(&self, block: &BlockRef) -> Result<Arc<Block>, IndexError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if let Ok(data) = &self.data {
                assert_eq!(data.hash(), block.hash);
            }
            self.data.clone()
        }
    }

    #[test]
    fn mempool_precedes_index_without_block() {
        let tx = transaction(1);
        let id = txid(&tx);
        let hash = BlockHash::from_lendian([1; 32]);
        let mut index = MemTxIndex::default();
        index.entries.insert(id, (tx.clone(), hash));
        let blocks = Blocks::new(&[]);
        let pool = |_: &Txid| Some(tx.clone());
        assert_eq!(
            get_transaction(None, Some(&pool), Some(&index), &blocks, &id),
            Ok(Some((tx, None)))
        );
        assert_eq!(blocks.reads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn index_after_mempool_miss_or_without_mempool() {
        let tx = transaction(2);
        let id = txid(&tx);
        let hash = BlockHash::from_lendian([2; 32]);
        let mut index = MemTxIndex::default();
        index.entries.insert(id, (tx.clone(), hash));
        let blocks = Blocks::new(&[]);
        let empty_pool = |_: &Txid| None;
        for pool in [None, Some(&empty_pool as &MempoolLookup<'_>)] {
            assert_eq!(
                get_transaction(None, pool, Some(&index), &blocks, &id),
                Ok(Some((tx.clone(), Some(hash))))
            );
        }
        assert_eq!(blocks.reads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn given_block_skips_mempool_and_matching_index_skips_disk() {
        let tx = transaction(3);
        let id = txid(&tx);
        let blocks = Blocks::new(slice::from_ref(&tx));
        let block = blocks.reference();
        let mut index = MemTxIndex::default();
        index.entries.insert(id, (tx.clone(), block.hash));
        let pool = |_: &Txid| panic!("mempool queried with a block");
        assert_eq!(
            get_transaction(Some(&block), Some(&pool), Some(&index), &blocks, &id),
            Ok(Some((tx, Some(block.hash))))
        );
        assert_eq!(blocks.reads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn given_block_is_read_without_index_after_miss_or_after_block_mismatch() {
        let tx = transaction(4);
        let id = txid(&tx);
        let blocks = Blocks::new(&[transaction(40), tx.clone()]);
        let block = blocks.reference();
        let empty = MemTxIndex::default();
        let mut mismatched = MemTxIndex::default();
        mismatched.entries.insert(id, (tx.clone(), BlockHash::from_lendian([4; 32])));
        for index in [None, Some(&empty as &dyn TxIndexReader), Some(&mismatched)] {
            assert_eq!(
                get_transaction(Some(&block), None, index, &blocks, &id),
                Ok(Some((tx.clone(), Some(block.hash))))
            );
        }
        assert_eq!(blocks.reads.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn no_result_for_absent_tx_or_mismatching_index_block() {
        let tx = transaction(5);
        let id = txid(&tx);
        let blocks = Blocks::new(&[transaction(50)]);
        let block = blocks.reference();
        let mut index = MemTxIndex::default();
        index.entries.insert(id, (tx, BlockHash::from_lendian([5; 32])));
        assert_eq!(get_transaction(Some(&block), None, Some(&index), &blocks, &id), Ok(None));
        let empty = MemTxIndex::default();
        assert_eq!(get_transaction(None, None, Some(&empty), &blocks, &id), Ok(None));
        assert_eq!(get_transaction(None, None, None, &blocks, &id), Ok(None));
        assert_eq!(blocks.reads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn block_lookup_uses_stable_asset_unlock_txid() {
        let mut payload = vec![2];
        payload.extend(7u64.to_le_bytes());
        payload.extend(1_000u32.to_le_bytes());
        payload.extend(100u32.to_le_bytes());
        payload.extend([0xab; 32]);
        payload.extend([0; 96]);
        let tx = Arc::new(Transaction::new(3, TxType::AssetUnlock, vec![], vec![], 0, payload));
        let canonical = Tx::new(tx.as_ref().clone());
        assert_ne!(canonical.txid(), canonical.instance_hash());
        let blocks = Blocks::new(slice::from_ref(&tx));
        let block = blocks.reference();
        assert_eq!(
            get_transaction(Some(&block), None, None, &blocks, &canonical.txid()),
            Ok(Some((tx, Some(block.hash))))
        );
        assert_eq!(
            get_transaction(Some(&block), None, None, &blocks, &canonical.instance_hash()),
            Ok(None)
        );
    }

    struct FailedIndex;

    impl IndexStatus for FailedIndex {
        fn summary(&self) -> IndexSummary {
            MemTxIndex::default().summary()
        }

        fn block_until_synced(&self) -> bool {
            false
        }
    }

    impl TxIndexReader for FailedIndex {
        fn find_tx(&self, _: &Txid) -> Result<Option<IndexedTx>, IndexError> {
            Err(IndexError::Corrupt("unreadable transaction".into()))
        }

        fn has_tx(&self, _: &Txid) -> Result<bool, IndexError> {
            Err(IndexError::Io("unreadable position".into()))
        }
    }

    #[test]
    fn failed_reads_fall_through_as_in_core() {
        let tx = transaction(6);
        let id = txid(&tx);
        let mut blocks = Blocks::new(slice::from_ref(&tx));
        let block = blocks.reference();
        assert_eq!(
            get_transaction(Some(&block), None, Some(&FailedIndex), &blocks, &id),
            Ok(Some((tx, Some(block.hash))))
        );
        assert_eq!(get_transaction(None, None, Some(&FailedIndex), &blocks, &id), Ok(None));
        blocks.data = Err(IndexError::Io("unreadable block".into()));
        assert_eq!(get_transaction(Some(&block), None, Some(&FailedIndex), &blocks, &id), Ok(None));
        assert_eq!(blocks.reads.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn located_tx_confirmations_count_active_block_inclusively() {
        let tx = transaction(7);
        let hash = BlockHash::from_lendian([7; 32]);
        for (height, tip, active, confirmations) in [
            (Some(7), 10, Some(7), 4),
            (Some(10), 10, Some(10), 1),
            (Some(0), 10, Some(0), 11),
            (None, 10, None, 0),
            (Some(11), 10, None, 0),
        ] {
            let located = LocatedTx::new(tx.clone(), hash, height, tip);
            assert_eq!(located.tx, tx);
            assert_eq!(located.block_hash, hash);
            assert_eq!(located.active_height, active);
            assert_eq!(located.confirmations, confirmations);
        }
    }
}
