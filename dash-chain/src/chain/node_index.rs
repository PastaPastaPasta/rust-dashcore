//! The node's index mutators (SB4 §4's "status and block data" and V's best-header hooks),
//! behind a view only a node-mode chain hands out (Opus r1 mi-3).

use dash_consensus::BlockId;
use dash_primitives::BlockHash;

use super::{FailedBlocks, HeaderChain, MutableGroup};
use crate::chainlock::{ChainLockRef, ClOutcome, ResetReport};
use crate::config::ChainMode;
use crate::error::ChainError;
use crate::record::IndexWrite;
use crate::residency::{Permanent, Residency};
use crate::store::{HeaderStore, NoStore};

/// The node's writes to its index outside acceptance and the tip writers: CH1-node's block
/// data and status writes, Core's `m_failed_blocks` and `m_dirty_blockindex`, CH2's B1 drain,
/// V's best-header hooks, `ResetBlockFailureFlags` and the ChainLock record.
/// [`HeaderChain::node_index`] hands one out in the node only.
///
/// SPV cannot reach these: its status changes go through the overlay (§3.7), its entries leave
/// the arena, which these writes assume resident, and its dirty set is the step's written set
/// (SB5-A13). An SPV `m_failed_blocks` entry would later turn a peer header into a panic.
///
/// The chain's own methods are crate-private:
///
/// ```compile_fail,E0624
/// fn f(chain: &mut dash_chain::HeaderChain) { chain.failed_blocks(); }
/// ```
/// ```compile_fail,E0624
/// fn f(chain: &mut dash_chain::HeaderChain) { chain.mark_dirty(dash_chain::BlockId(0)); }
/// ```
/// ```compile_fail,E0624
/// fn f(chain: &mut dash_chain::HeaderChain) { chain.take_dirty(); }
/// ```
/// ```compile_fail,E0624
/// fn f(chain: &mut dash_chain::HeaderChain) { chain.modify(dash_chain::BlockId(0), |_| ()); }
/// ```
/// ```compile_fail,E0624
/// fn f(chain: &mut dash_chain::HeaderChain) {
///     chain.reset_failure_flags(dash_chain::BlockId(0), false);
/// }
/// ```
/// ```compile_fail,E0624
/// fn f(chain: &mut dash_chain::HeaderChain, cl: dash_chain::ChainLockRef) {
///     chain.record_chainlock(cl);
/// }
/// ```
pub struct NodeIndex<'c, S: HeaderStore = NoStore, R: Residency = Permanent> {
    chain: &'c mut HeaderChain<S, R>,
}

impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    /// The node's index mutators; `ConfigError::WrongMode` in SPV.
    pub fn node_index(&mut self) -> Result<NodeIndex<'_, S, R>, ChainError> {
        self.require_mode(ChainMode::Node, "node_index")?;
        Ok(NodeIndex {
            chain: self,
        })
    }
}

impl<S: HeaderStore, R: Residency> NodeIndex<'_, S, R> {
    /// A seqlocked write of `id`'s mutable group; does not mark it dirty (D15).
    ///
    /// # Panics
    ///
    /// For an id the index does not hold.
    pub fn modify(&mut self, id: BlockId, f: impl FnOnce(&mut MutableGroup)) {
        self.chain.modify(id, f);
    }

    /// Marks `id` dirty, at Core's `m_dirty_blockindex` sites (D15).
    pub fn mark_dirty(&mut self, id: BlockId) {
        self.chain.mark_dirty(id);
    }

    /// Core `m_failed_blocks` (id order, D9).
    pub fn failed_blocks(&mut self) -> &mut FailedBlocks {
        self.chain.failed_blocks()
    }

    /// The dirty entries' records for CH2's B1 (P10), encoded at take time; the dirty set is
    /// then empty.
    ///
    /// # Panics
    ///
    /// If an id marked dirty is not in the index.
    // Core: node/blockstorage.cpp WriteBlockIndexDB (CDiskBlockIndex per m_dirty_blockindex entry)
    pub fn take_dirty(&mut self) -> Vec<IndexWrite> {
        self.chain.take_dirty()
    }

    /// [`take_dirty`](Self::take_dirty)'s records, in its order, each handed to `put` (hash,
    /// encoded record) as it is encoded, so none is kept: CH2's freeze puts them straight into
    /// B1's batch (CH2c D40). The dirty set is then empty.
    ///
    /// # Panics
    ///
    /// If an id marked dirty is not in the index.
    pub fn take_dirty_into(&mut self, put: impl FnMut(&BlockHash, &[u8])) {
        self.chain.take_dirty_into(put)
    }

    /// `id`'s record as [`take_dirty`](Self::take_dirty) encodes it, now, whether or not it is
    /// dirty: CH2's step 7 clears for Bk or Bp, which stay dirty for the next B1.
    ///
    /// # Panics
    ///
    /// For an id the index does not hold.
    pub fn record(&self, id: BlockId) -> IndexWrite {
        self.chain.record(id)
    }

    /// `validation.cpp:3951`, `:3969`, `:4089`, `:4107`: if `marked` is the best header, its
    /// prev; the genesis entry stays (R5, D-C7-3). `true` when `marked` was the best header, so
    /// V knows Core's branch ran (its `m_best_invalid` update, `:3952`) even at genesis.
    pub fn best_header_walk_back(&mut self, marked: BlockId) -> bool {
        self.chain.best_header_walk_back(marked)
    }

    /// `validation.cpp:2043-2044`: if `invalid` is an ancestor-or-self of the best header, the
    /// writer's active tip (CH1-A2; R5 keeps it when there is none).
    pub fn best_header_invalid_chain(&mut self, invalid: BlockId) {
        self.chain.best_header_invalid_chain(invalid);
    }

    /// The index half of Core's `ResetBlockFailureFlags` (§7.4, D18): every descendant-or-self
    /// of `id` that is not `IsValid()`, then every ancestor with a FAILED or CONFLICT bit, loses
    /// its FAILED bits (and its CONFLICT mark only when `ignore_chainlocks`), is dirtied and
    /// leaves `m_failed_blocks`.
    ///
    /// # Panics
    ///
    /// For an id the index does not hold.
    // Core: validation.cpp:4152-4220 Chainstate::ResetBlockFailureFlags
    pub fn reset_failure_flags(&mut self, id: BlockId, ignore_chainlocks: bool) -> ResetReport {
        self.chain.reset_failure_flags(id, ignore_chainlocks)
    }

    /// Core `UpdateBestChainlock` and the block lookup, with no spork gate and no enforcement
    /// (CH1-A5): a moved floor is `Floor { id, enforced: None }`, and V enforces in a later
    /// command (CH1-node §5.1).
    // Core: chainlock/chainlock.cpp:106-132 Chainlocks::UpdateBestChainlock
    pub fn record_chainlock(&mut self, cl: ChainLockRef) -> ClOutcome {
        self.chain.record_chainlock(cl)
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::super::tests::{branch, node, spv};
    use super::*;
    use crate::error::ConfigError;
    use crate::status::Status;

    /// mi-3: SPV gets no view; the node's writes through it reach the chain.
    #[test]
    fn only_the_node_gets_the_index_mutators() {
        let mut chain = spv();
        let err = chain.node_index().err();
        assert_eq!(err, Some(ChainError::Config(ConfigError::WrongMode("node_index"))));

        let mut chain = node();
        let trunk = branch(&mut chain, BlockId(0), 1, 2, 1_700_000_000);
        chain.take_dirty();
        let mut ix = chain.node_index().expect("the node's");
        ix.modify(trunk[0], |g| g.n_tx = 7);
        ix.mark_dirty(trunk[0]);
        assert!(ix.failed_blocks().insert(trunk[1]));
        let record = ix.record(trunk[0]);
        let writes = ix.take_dirty();
        assert_eq!(writes, [record], "the dirty entry's record");
        assert!(ix.take_dirty().is_empty());
        assert!(ix.best_header_walk_back(trunk[1]));
        ix.best_header_invalid_chain(trunk[0]);
        assert_eq!(chain.best_header(), BlockId(0), "the walk-back, then the tip");
        assert!(chain.failed.contains(trunk[1]));
        assert_eq!(chain.reader().entry(trunk[0]).expect("held").n_tx, 7);
        assert_eq!(chain.writer_status(trunk[0]), Ok(Status(Status::VALID_TREE)));

        // D-L3: the reset and the ChainLock record, which SPV cannot reach either.
        let mut ix = chain.node_index().expect("the node's");
        let report = ix.reset_failure_flags(trunk[1], false);
        assert!(!ix.failed_blocks().contains(trunk[1]), "{report:?}");
        let hash = chain.reader().entry(trunk[1]).expect("held").hash;
        let cl = ChainLockRef {
            height: 2,
            hash,
        };
        let mut ix = chain.node_index().expect("the node's");
        assert!(matches!(ix.record_chainlock(cl), ClOutcome::Floor { id, .. } if id == trunk[1]));
        assert_eq!(chain.cl_floor(), Some(trunk[1]));
    }
}
