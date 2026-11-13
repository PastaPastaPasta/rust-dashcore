//! The node's source for the already-published masternode tip and BLS context.

use std::sync::Arc;

use dashd_chainstate::chain::ChainViewHandle;
use dashd_masternode::tip::{TipListRef, TipMnList, TipSource};

/// Core's `GetListAtChainTip()` follows `tipIndex`, set by `UpdatedBlockTip`.
/// Chainstate publishes that list at `dmn_tip` (`chain/view.rs:88-97`), which can
/// differ from the active tip during initialization and after a disconnect-only step.
// Core: evo/deterministicmns.cpp:819-824, :941-947 at v24.0.0
struct NodeTipSource(ChainViewHandle);

impl TipSource for NodeTipSource {
    fn load(&self) -> TipListRef {
        let view = self.0.view();
        TipListRef {
            list: Arc::clone(&view.mn_list),
            bls: view.bls,
        }
    }
}

pub(super) fn new(chain: ChainViewHandle) -> TipMnList {
    TipMnList::new(Arc::new(NodeTipSource(chain)))
}

#[cfg(test)]
mod tests;
