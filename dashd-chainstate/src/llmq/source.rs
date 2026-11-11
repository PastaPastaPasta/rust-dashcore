//! `NodeLlmqSource` (CH7 §4.2.5): the per-block state LQ1's member computations read, over V's
//! evo read view and block files.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dash_chain::ChainReader;
use dash_consensus::BlockId;
use dash_consensus::ChainAncestry;
use dash_consensus::ctx::DeploymentView;
use dash_dml::store::{DmlReader, ListCache, resolve_list_traced};
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
    /// The answers that stood on the initial snapshot ([`LlmqSource::made_up_lists`]).
    made_up: AtomicU64,
}

impl<'a> NodeLlmqSource<'a> {
    /// A source over one read of the evo state, having made up no list yet.
    pub fn new(
        chain: &'a dyn ChainAncestry,
        deployments: &'a dyn DeploymentView,
        view: &'a dyn DmlReader,
        lists: &'a dyn ListCache,
        blocks: &'a dyn EvoBlocks,
        reader: &'a ChainReader,
    ) -> Self {
        NodeLlmqSource {
            chain,
            deployments,
            view,
            lists,
            blocks,
            reader,
            made_up: AtomicU64::new(0),
        }
    }
}

impl LlmqSource<MnList> for NodeLlmqSource<'_> {
    type Error = StoreError;

    /// `GetListForBlock(at)`, never `None`: below DIP3 the default list; a DIP3-active block with
    /// no rows is Core's initial snapshot, or [`StoreError::ListUnavailable`] in a dual-chainstate
    /// run (`resolve_list`, DEC-1064); a row that does not decode, or a record that does not
    /// apply, is a fault (A1 I5; the decode case a declared deviation, DEC-1073). An answer over
    /// the initial snapshot counts in [`LlmqSource::made_up_lists`] (OD-CP-51).
    // Core: v24.0.0 evo/deterministicmns.cpp:826 GetListForBlockInternal
    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<MnList>>, StoreError> {
        let resolved =
            resolve_list_traced(at, self.chain, self.deployments, self.view, self.lists)?;
        if resolved.over_initial_snapshot {
            self.made_up.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Some(resolved.list))
    }

    fn made_up_lists(&self) -> u64 {
        self.made_up.load(Ordering::Relaxed)
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
