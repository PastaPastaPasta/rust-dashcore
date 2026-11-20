//! Core's `CDSNotificationInterface`, for its `m_dstxman` calls. It runs on the signals callback
//! thread, which carries the no-wait marker, so it reads only V's published view (`tip`,
//! `has_chain_lock`, the block index) and SP1's sync state, never a V query.

use std::sync::Arc;

use dash_consensus::{Block, BlockId};
use dash_primitives::{BlockHash, Transaction};
use dashd_chainstate::chain::{ChainLockSig, ChainViewHandle, ValidationInterface};
use dashd_coinjoin::dstx::DstxRuntime;

use super::adapters::NodeChainView;

/// The DSTX half of `g_ds_notification_interface`, installed right after its `mn_sync` half
/// so a new tip reaches the sync before the DSTX check. The gates are [`DstxRuntime`]'s.
// Core: dsnotificationinterface.cpp:55-104 at v24.0.0
pub struct DstxListener {
    dstx: Arc<DstxRuntime>,
    chain: NodeChainView,
    view: ChainViewHandle,
}

impl DstxListener {
    pub fn new(dstx: Arc<DstxRuntime>, chain: NodeChainView, view: ChainViewHandle) -> Self {
        Self {
            dstx,
            chain,
            view,
        }
    }

    fn at(&self, id: BlockId) -> Option<(i32, BlockHash)> {
        let e = self.view.entry(id)?;
        Some((i32::try_from(e.height).ok()?, e.hash))
    }
}

fn txs(block: &Block) -> Vec<Transaction> {
    block.txs().iter().map(|tx| tx.raw().clone()).collect()
}

impl ValidationInterface for DstxListener {
    /// `UpdatedBlockTip(pindexNew, nullptr, ibd)`.
    // Core: dsnotificationinterface.cpp:31-35
    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        if let Some(tip) = tip {
            self.updated_block_tip(tip, None, ibd);
        }
    }

    // Core: dsnotificationinterface.cpp:55-67
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        let fork = fork.and_then(|f| self.at(f));
        self.dstx.updated_block_tip(self.at(new), fork, ibd, &self.chain);
    }

    // Core: dsnotificationinterface.cpp:80-83
    fn block_connected(&self, block: &Arc<Block>, id: BlockId) {
        if let Some((height, _)) = self.at(id) {
            self.dstx.block_connected(&txs(block), height);
        }
    }

    // Core: dsnotificationinterface.cpp:85-88
    fn block_disconnected(&self, block: &Arc<Block>, _: BlockId) {
        self.dstx.block_disconnected(&txs(block));
    }

    // Core: dsnotificationinterface.cpp:98-104
    fn notify_chain_lock(&self, id: BlockId, _: &Arc<ChainLockSig>) {
        self.dstx.notify_chain_lock(self.at(id), &self.chain);
    }
}
