//! In-memory basic-filter index built from simulation blocks.

use std::collections::HashMap;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_filters::{BlockFilter, FilterHeader};
use dash_num::Hash256;
use dash_primitives::{BlockHash, OutPoint};
use dash_types::Numeric;
use parking_lot::Mutex;

use super::SimChain;
use crate::processing::seams::{BlockFilterIndex, BlockRead, ChainAccess, TreeRead};

/// A retained filter and header per indexed block, including disconnected blocks.
pub struct SimFilterIndex {
    chain: Arc<SimChain>,
    entries: Mutex<HashMap<BlockId, (BlockFilter, FilterHeader)>>,
}

impl SimFilterIndex {
    /// An empty index attached to `chain`.
    pub fn new(chain: Arc<SimChain>) -> Self {
        Self {
            chain,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Indexes `id` after its parent. `script_for_coin` supplies spent scripts.
    pub fn index<S: AsRef<[u8]>, E>(
        &self,
        id: BlockId,
        script_for_coin: impl FnMut(&OutPoint) -> Result<S, E>,
    ) -> bool {
        let Some(entry) = self.chain.entry(id) else {
            return false;
        };
        let BlockRead::Block(block) = self.chain.read_block(id) else {
            return false;
        };
        let mut entries = self.entries.lock();
        let prev = match entry.prev {
            Some(prev) => match entries.get(&prev) {
                Some((_, header)) => *header,
                None => return false,
            },
            None => Hash256::ZERO,
        };
        let Ok(filter) =
            BlockFilter::build(&entry.hash, block.txs().iter().map(|tx| tx.raw()), script_for_coin)
        else {
            return false;
        };
        let header = filter.header(&prev);
        entries.insert(id, (filter, header));
        true
    }

    /// Indexes an ancestry of coinbase-only blocks, genesis first.
    pub fn index_coinbase_chain(&self, stop: BlockId) {
        let mut missing = Vec::new();
        let mut next = Some(stop);
        while let Some(id) = next {
            if self.entries.lock().contains_key(&id) {
                break;
            }
            missing.push(id);
            next = self.chain.entry(id).expect("a simulation entry").prev;
        }
        for id in missing.into_iter().rev() {
            assert!(self.index(id, |_| -> Result<Vec<u8>, ()> { panic!("coinbase-only chain") }));
        }
    }

    /// Removes an entry to simulate an unavailable index record.
    pub fn remove(&self, id: BlockId) {
        self.entries.lock().remove(&id);
    }

    fn range(&self, start: u32, stop: BlockId) -> Option<Vec<BlockId>> {
        let mut entry = self.chain.entry(stop)?;
        if start > entry.height {
            return None;
        }
        let mut ids = Vec::new();
        loop {
            ids.push(entry.id);
            if entry.height == start {
                break;
            }
            entry = self.chain.entry(entry.prev?)?;
        }
        ids.reverse();
        Some(ids)
    }
}

impl BlockFilterIndex for SimFilterIndex {
    fn filter(&self, id: BlockId) -> Option<Vec<u8>> {
        Some(self.entries.lock().get(&id)?.0.encoded().to_vec())
    }

    fn filter_header(&self, id: BlockId) -> Option<FilterHeader> {
        Some(self.entries.lock().get(&id)?.1)
    }

    fn filters(&self, start: u32, stop: BlockId) -> Option<Vec<(BlockHash, Vec<u8>)>> {
        self.range(start, stop)?
            .iter()
            .map(|&id| Some((self.chain.entry(id)?.hash, self.filter(id)?)))
            .collect()
    }

    fn filter_hashes(&self, start: u32, stop: BlockId) -> Option<Vec<Hash256>> {
        let ids = self.range(start, stop)?;
        let entries = self.entries.lock();
        ids.iter().map(|id| Some(entries.get(id)?.0.hash())).collect()
    }

    fn best_height(&self) -> Option<u32> {
        let snap = self.chain.snapshot();
        (0..=snap.tip.tip_height).rev().find(|&h| {
            self.chain
                .ancestor(snap.tip.tip, h)
                .is_some_and(|id| self.entries.lock().contains_key(&id))
        })
    }
}
