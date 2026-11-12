//! `CheckBlockIndex`, the node half (§4.10): Core's block-data, sequence, candidate and
//! `m_blocks_unlinked` asserts, run per entry through SB4's walk (CH1-A9), which has already
//! checked the header half with the same `pindexFirst*` state.

use dash_chain::{
    ChainReader, EntryView, FirstAncestors, HeaderChain, IndexInconsistency, IndexVisitor, Status,
};
use dash_consensus::BlockId;

use super::index::NodeBlockIndex;

/// The node half of Core's walk (`validation.cpp:5607-5855`).
struct NodeHalf<'a> {
    idx: &'a NodeBlockIndex,
    reader: &'a ChainReader,
    /// Core `m_blockman.m_have_pruned`.
    have_pruned: bool,
    /// The active tip; Core skips the candidate rules without one.
    tip: Option<BlockId>,
}

impl IndexVisitor for NodeHalf<'_> {
    fn visit(&mut self, e: &EntryView, f: &FirstAncestors) -> Result<(), IndexInconsistency> {
        let fail = |what| {
            Err(IndexInconsistency {
                id: e.id,
                what,
            })
        };
        let have_data = e.status.has(Status::HAVE_DATA);
        let have_txs = self.idx.have_txs_downloaded(e.id);
        // :5691
        if !have_txs && self.idx.sequence_id(e.id) > 0 {
            return fail("nSequenceId can't be set positive for blocks that aren't linked");
        }
        // :5696-5709
        if !self.have_pruned {
            if have_data != (e.n_tx != 0) {
                return fail("if we've never pruned, HAVE_DATA should be equivalent to nTx > 0");
            }
            if f.missing != f.never_processed {
                return fail("pindexFirstMissing == pindexFirstNeverProcessed");
            }
        } else if have_data && e.n_tx == 0 {
            return fail("HAVE_DATA implies nTx > 0");
        }
        // :5719 (HAVE_UNDO ⇒ HAVE_DATA is SB4's)
        if (e.status.bits() & Status::VALID_MASK >= Status::VALID_TRANSACTIONS) != (e.n_tx > 0) {
            return fail("VALID_TRANSACTIONS is equivalent to nTx > 0");
        }
        // :5722-5723
        if f.never_processed.is_none() != have_txs {
            return fail("no never-processed ancestor is equivalent to HaveTxsDownloaded()");
        }
        if f.not_transactions_valid.is_none() != have_txs {
            return fail("all parents TRANSACTIONS valid is equivalent to HaveTxsDownloaded()");
        }
        // :5741-5763: setBlockIndexCandidates.
        let member = self.idx.is_candidate(e.id);
        // `!CBlockIndexWorkComparator()(pindex, m_chain.Tip())`
        let tip_key = self.tip.map(|t| self.idx.key_of(self.reader, t));
        let not_worse = tip_key.is_some_and(|k| self.idx.key(e) >= k);
        if let Some(tip) = self.tip {
            if not_worse && f.never_processed.is_none() {
                if f.invalid.is_none()
                    && f.conflicting.is_none()
                    && (f.missing.is_none() || e.id == tip)
                    && !member
                {
                    return fail("a valid block at least as good as the tip must be a candidate");
                }
            } else if member {
                return fail(
                    "a block worse than the tip, or with an unseen parent, is a candidate",
                );
            }
        }
        // :5765-5803: m_blocks_unlinked.
        let in_unlinked = e.prev.is_some_and(|p| self.idx.unlinked_children(p).contains(&e.id));
        if e.prev.is_some()
            && have_data
            && f.never_processed.is_some()
            && f.invalid.is_none()
            && !in_unlinked
        {
            return fail("data, an unprocessed parent and no invalid parent: must be unlinked");
        }
        if !have_data && in_unlinked {
            return fail("can't be in m_blocks_unlinked without HAVE_DATA");
        }
        if f.missing.is_none() && in_unlinked {
            return fail("no parent missing data: cannot be in m_blocks_unlinked");
        }
        if e.prev.is_some() && have_data && f.never_processed.is_none() && f.missing.is_some() {
            if !self.have_pruned {
                return fail("a parent's data went missing without pruning");
            }
            if not_worse
                && !member
                && f.invalid.is_none()
                && f.conflicting.is_none()
                && !in_unlinked
            {
                return fail("better than the tip, not a candidate: must be unlinked");
            }
        }
        Ok(())
    }
}

/// Core's `CheckBlockIndex()`: SB4's header half and this node half in one walk. An
/// inconsistency is a bug; the caller aborts the node with it, as Core's `assert` does.
pub(crate) fn check_block_index(
    chain: &HeaderChain,
    idx: &NodeBlockIndex,
    have_pruned: bool,
) -> Result<(), IndexInconsistency> {
    let reader = chain.reader();
    let mut half = NodeHalf {
        idx,
        reader: &reader,
        have_pruned,
        tip: chain.private_tip().map(|(t, _)| t.id),
    };
    chain.check_index_with(&mut half)
}
