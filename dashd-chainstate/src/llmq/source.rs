//! `NodeLlmqSource` (CH7 §4.2.5): the per-block state LQ1's member computations read, over V's
//! evo read view and block files.

use std::sync::Arc;

use dash_chain::ChainReader;
use dash_consensus::BlockId;
use dash_consensus::ChainAncestry;
use dash_consensus::ctx::DeploymentView;
use dash_dml::store::{DmlReader, ListCache, resolve_list};
use dash_dml::{MnList, StoreError};
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::LlmqSource;

use crate::evo::node::EvoBlocks;

/// [`LlmqSource`] over one read of the evo state: the binding of an attempt (the attempt's target
/// layer) or of an `OnV` command (the committed layer at V's tip) builds one per call.
pub struct NodeLlmqSource<'a> {
    pub chain: &'a dyn ChainAncestry,
    pub deployments: &'a dyn DeploymentView,
    pub view: &'a dyn DmlReader,
    pub lists: &'a dyn ListCache,
    pub blocks: &'a dyn EvoBlocks,
    pub reader: &'a ChainReader,
}

impl LlmqSource<MnList> for NodeLlmqSource<'_> {
    type Error = StoreError;

    /// `GetListForBlock(at)`. The node holds the list of every block it has connected, so a list
    /// it cannot build is [`StoreError::Inconsistent`], a fault (EV addendum §2.1), never `None`.
    // Core: v24 evo/deterministicmns.cpp:826 GetListForBlockInternal
    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<MnList>>, StoreError> {
        resolve_list(at, self.chain, self.deployments, self.view, self.lists).map(Some)
    }

    /// The block `GetNonNullCoinbaseChainlock` reads. Every failed `ReadBlockFromDisk` is Core's
    /// `nullopt` (`cbtx.cpp:192-195`), so every read [`EvoBlocks`] cannot answer, a pruned block
    /// or a failed read alike, is [`CoinbaseFacts::BlockDataUnavailable`]: a normal value, never
    /// the source's `None` (OD-CP-4).
    // Core: v24 evo/cbtx.cpp:181 GetNonNullCoinbaseChainlock
    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, StoreError> {
        let facts = match self.blocks.read(self.reader, at) {
            None => CoinbaseFacts::BlockDataUnavailable,
            Some(block) => match block.txs().first() {
                None => CoinbaseFacts::NoTransactions,
                Some(tx) => CoinbaseFacts::First(tx.clone()),
            },
        };
        Ok(Some(facts))
    }
}
