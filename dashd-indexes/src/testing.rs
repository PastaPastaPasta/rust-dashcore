// SPDX-License-Identifier: MIT

//! In-memory readers for consumer tests. Populate them before sharing through an `Arc`;
//! their status is supplied by the test, and they do not run subscriber threads.

use std::collections::HashMap;
use std::sync::Arc;

use crate::api::*;

pub struct MemTxIndex {
    pub status: IndexSummary,
    pub entries: HashMap<Txid, IndexedTx>,
}

impl Default for MemTxIndex {
    fn default() -> Self {
        Self {
            // Core: index/txindex.cpp:52-54
            status: IndexSummary {
                name: "txindex",
                synced: false,
                best_block_height: 0,
            },
            entries: HashMap::new(),
        }
    }
}

impl IndexStatus for MemTxIndex {
    fn summary(&self) -> IndexSummary {
        self.status.clone()
    }

    fn block_until_synced(&self) -> bool {
        self.status.synced
    }
}

impl TxIndexReader for MemTxIndex {
    fn find_tx(&self, txid: &Txid) -> Result<Option<IndexedTx>, IndexError> {
        Ok(self.entries.get(txid).cloned())
    }

    fn has_tx(&self, txid: &Txid) -> Result<bool, IndexError> {
        Ok(self.entries.contains_key(txid))
    }
}

pub struct MemFilterIndex {
    pub status: IndexSummary,
    pub entries: HashMap<BlockHash, (BlockFilter, FilterHeader)>,
}

impl Default for MemFilterIndex {
    fn default() -> Self {
        Self {
            // Core: index/blockfilterindex.cpp:106-109
            status: IndexSummary {
                name: "basic block filter index",
                synced: false,
                best_block_height: 0,
            },
            entries: HashMap::new(),
        }
    }
}

impl IndexStatus for MemFilterIndex {
    fn summary(&self) -> IndexSummary {
        self.status.clone()
    }

    fn block_until_synced(&self) -> bool {
        self.status.synced
    }
}

impl FilterIndexReader for MemFilterIndex {
    fn filter(&self, block: &BlockRef) -> Result<Option<BlockFilter>, IndexError> {
        Ok(self.entries.get(&block.hash).map(|(filter, _)| filter.clone()))
    }

    fn filter_header(&self, block: &BlockRef) -> Result<Option<FilterHeader>, IndexError> {
        Ok(self.entries.get(&block.hash).map(|(_, header)| *header))
    }

    fn filter_range(
        &self,
        start: u32,
        stop: &BlockRef,
        ancestry: &dyn Ancestry,
    ) -> Result<Option<Vec<BlockFilter>>, IndexError> {
        // Core: index/blockfilterindex.cpp:387-391,416-436
        if start > stop.height {
            return Ok(None);
        }
        let mut filters = Vec::new();
        for height in start..=stop.height {
            let block = if height == stop.height {
                *stop
            } else if let Some(block) = ancestry.ancestor(stop, height) {
                block
            } else {
                return Ok(None);
            };
            let Some(filter) = self.filter(&block)? else {
                return Ok(None);
            };
            filters.push(filter);
        }
        Ok(Some(filters))
    }

    fn filter_hash_range(
        &self,
        start: u32,
        stop: &BlockRef,
        ancestry: &dyn Ancestry,
    ) -> Result<Option<Vec<FilterHash>>, IndexError> {
        Ok(self
            .filter_range(start, stop, ancestry)?
            .map(|filters| filters.iter().map(BlockFilter::hash).collect()))
    }
}

pub struct MemCoinStats {
    pub status: IndexSummary,
    pub entries: HashMap<BlockHash, CoinStatsEntry>,
}

impl Default for MemCoinStats {
    fn default() -> Self {
        Self {
            // Core: index/coinstatsindex.cpp:108-109
            status: IndexSummary {
                name: "coinstatsindex",
                synced: false,
                best_block_height: 0,
            },
            entries: HashMap::new(),
        }
    }
}

impl IndexStatus for MemCoinStats {
    fn summary(&self) -> IndexSummary {
        self.status.clone()
    }

    fn block_until_synced(&self) -> bool {
        self.status.synced
    }
}

impl CoinStatsReader for MemCoinStats {
    fn look_up_stats(&self, block: &BlockRef) -> Result<Option<CoinStatsEntry>, IndexError> {
        Ok(self.entries.get(&block.hash).cloned())
    }
}

#[derive(Default)]
pub struct MemRegistry {
    pub txindex: Option<Arc<dyn TxIndexReader>>,
    pub filters: HashMap<FilterType, Arc<dyn FilterIndexReader>>,
    pub coinstats: Option<Arc<dyn CoinStatsReader>>,
}

impl IndexRegistry for MemRegistry {
    fn txindex(&self) -> Option<Arc<dyn TxIndexReader>> {
        self.txindex.clone()
    }

    fn filters(&self, ty: FilterType) -> Option<Arc<dyn FilterIndexReader>> {
        self.filters.get(&ty).cloned()
    }

    fn coinstats(&self) -> Option<Arc<dyn CoinStatsReader>> {
        self.coinstats.clone()
    }

    fn summaries(&self) -> Vec<IndexSummary> {
        // Core: rpc/node.cpp:1207-1217
        let mut summaries = Vec::new();
        if let Some(index) = self.txindex() {
            summaries.push(index.summary());
        }
        if let Some(index) = self.coinstats() {
            summaries.push(index.summary());
        }
        if let Some(index) = self.filters(FilterType::Basic) {
            summaries.push(index.summary());
        }
        summaries
    }
}

#[cfg(test)]
mod tests {
    use dash_consensus::{BlockId, Tx};
    use dash_filters::{GcsParams, encode};
    use dash_primitives::TxType;
    use dash_types::Numeric as _;

    use super::*;

    fn block(seed: u8, height: u32) -> BlockRef {
        BlockRef {
            id: BlockId(u32::from(seed)),
            hash: BlockHash::from_lendian([seed; 32]),
            height,
        }
    }

    struct Branch(Vec<BlockRef>);

    impl Ancestry for Branch {
        fn ancestor(&self, stop: &BlockRef, height: u32) -> Option<BlockRef> {
            assert_eq!(Some(stop), self.0.last());
            self.0.iter().find(|block| block.height == height).copied()
        }
    }

    #[test]
    fn tx_fake_round_trip_and_missing_entry() {
        let tx = Arc::new(Transaction::new(2, TxType::Spend, vec![], vec![], 0, vec![]));
        let id = Tx::new(tx.as_ref().clone()).txid();
        let hash = block(1, 1).hash;
        let mut index = MemTxIndex::default();
        assert_eq!(index.find_tx(&id), Ok(None));
        assert_eq!(index.has_tx(&id), Ok(false));
        index.entries.insert(id, (tx.clone(), hash));
        assert_eq!(index.find_tx(&id), Ok(Some((tx, hash))));
        assert_eq!(index.has_tx(&id), Ok(true));
        assert!(!index.block_until_synced());
        index.status.synced = true;
        index.status.best_block_height = 42;
        assert!(index.block_until_synced());
        assert_eq!(index.summary().best_block_height, 42);
    }

    #[test]
    fn filter_fake_round_trips_in_branch_order_including_stale_blocks() {
        let genesis = block(1, 0);
        let active = block(2, 1);
        let stale = block(3, 1);
        let mut index = MemFilterIndex::default();
        let mut filter = |block: BlockRef| {
            let params = GcsParams::basic(&block.hash.to_lendian());
            let mut query = [[block.id.0 as u8]].into_iter().collect();
            let encoded = encode(&params, &mut query).unwrap();
            let filter = BlockFilter::from_encoded(&block.hash, encoded, false).unwrap();
            let header = FilterHeader::from_lendian([block.id.0 as u8; 32]);
            index.entries.insert(block.hash, (filter.clone(), header));
            (filter, header)
        };
        let (first, _) = filter(genesis);
        let (second, second_header) = filter(active);
        let (third, third_header) = filter(stale);
        assert_ne!(first.hash(), second.hash());
        assert_ne!(second.hash(), third.hash());
        assert_eq!(index.filter(&active), Ok(Some(second.clone())));
        assert_eq!(index.filter_header(&active), Ok(Some(second_header)));
        assert_eq!(index.filter_header(&stale), Ok(Some(third_header)));
        for (stop, filter) in [(active, second), (stale, third)] {
            let ancestry = Branch(vec![genesis, stop]);
            assert_eq!(
                index.filter_range(0, &stop, &ancestry),
                Ok(Some(vec![first.clone(), filter.clone()]))
            );
            assert_eq!(
                index.filter_hash_range(0, &stop, &ancestry),
                Ok(Some(vec![first.hash(), filter.hash()]))
            );
            assert_eq!(index.filter_range(1, &stop, &ancestry), Ok(Some(vec![filter])));
            assert_eq!(index.filter_range(2, &stop, &ancestry), Ok(None));
        }
        assert!(!index.block_until_synced());
        index.status.synced = true;
        assert!(index.block_until_synced());
    }

    #[test]
    fn filter_fake_missing_filter_or_ancestor_returns_none() {
        let first = block(1, 0);
        let stop = block(2, 1);
        let mut index = MemFilterIndex::default();
        let ancestry = Branch(vec![first, stop]);
        assert_eq!(index.filter(&stop), Ok(None));
        assert_eq!(index.filter_header(&stop), Ok(None));
        assert_eq!(index.filter_range(0, &stop, &ancestry), Ok(None));
        assert_eq!(index.filter_hash_range(0, &stop, &ancestry), Ok(None));
        index.entries.insert(
            stop.hash,
            (
                BlockFilter::from_encoded(&stop.hash, vec![0], false).unwrap(),
                FilterHeader::default(),
            ),
        );
        assert_eq!(index.filter_range(0, &stop, &ancestry), Ok(None));
        let incomplete = Branch(vec![stop]);
        assert_eq!(index.filter_range(0, &stop, &incomplete), Ok(None));
    }

    #[test]
    fn coinstats_fake_round_trips_all_fields_and_stale_blocks() {
        let mut index = MemCoinStats::default();
        let active = block(1, 10);
        let stale = block(2, 10);
        let stats = CoinStatsEntry {
            muhash: [42; 32],
            txouts: 2,
            bogo_size: 3,
            total_amount: 4,
            total_subsidy: 5,
            total_unspendable: 6,
            prevout_spent: 7,
            new_outputs_ex_coinbase: 8,
            coinbase: 9,
            unspendables_genesis: 10,
            unspendables_bip30: 11,
            unspendables_scripts: 12,
            unspendables_unclaimed_rewards: -13,
        };
        assert_eq!(index.look_up_stats(&active), Ok(None));
        index.entries.insert(active.hash, stats.clone());
        index.entries.insert(stale.hash, CoinStatsEntry::default());
        assert_eq!(index.look_up_stats(&active), Ok(Some(stats)));
        assert_eq!(index.look_up_stats(&stale), Ok(Some(CoinStatsEntry::default())));
        assert!(!index.block_until_synced());
        index.status.synced = true;
        assert!(index.block_until_synced());
    }

    #[test]
    fn registry_lists_only_enabled_indexes_in_core_order() {
        for enabled in 0..8 {
            let mut registry = MemRegistry::default();
            let mut expected = Vec::new();
            if enabled & 1 != 0 {
                let index = Arc::new(MemTxIndex::default());
                expected.push(index.summary());
                registry.txindex = Some(index);
            }
            if enabled & 2 != 0 {
                let index = Arc::new(MemCoinStats::default());
                expected.push(index.summary());
                registry.coinstats = Some(index);
            }
            if enabled & 4 != 0 {
                let index = Arc::new(MemFilterIndex::default());
                expected.push(index.summary());
                registry.filters.insert(FilterType::Basic, index);
            }
            assert_eq!(registry.txindex().is_some(), enabled & 1 != 0);
            assert_eq!(registry.coinstats().is_some(), enabled & 2 != 0);
            assert_eq!(registry.filters(FilterType::Basic).is_some(), enabled & 4 != 0);
            assert_eq!(registry.summaries(), expected);
        }
        assert_eq!(
            [
                MemTxIndex::default().summary().name,
                MemCoinStats::default().summary().name,
                MemFilterIndex::default().summary().name
            ],
            ["txindex", "coinstatsindex", "basic block filter index"]
        );
    }
}
