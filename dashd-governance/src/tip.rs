//! Governance's validation listener, Core's `CDSNotificationInterface` governance hooks: the
//! synchronous masternode-list hook, the queued tip hook and the init hook. The node adds it
//! after SP1's masternode-sync listener, as Core updates `m_mn_sync` before `m_govman` in the
//! same `UpdatedBlockTip`.
//!
//! Core has no governance `BlockDisconnected`: a reorg reaches governance as the tip hook alone.
//! A normal reorg moves the cached height to the new tip, down or up. `InvalidateBlock` and
//! `EnforceBlock` with no replacement send the tip signals with a null fork, so outside IBD the
//! tip work runs at the post-invalidate tip, over the DMN-tip list V moved there; only a fork
//! equal to the new tip leaves both unchanged. The tip work never reverts `Executed`, keeps an
//! executed trigger to its cycle expiry, restores no erased trigger, and is idempotent.
// Core: validation.cpp v24.0.2:3833-3836, :4049-4051, :4154-4156; governance/superblock.cpp
// v24.0.2:339-375, :668-677

use std::sync::Arc;

use dash_consensus::BlockId;
use dash_dml::{MnList, MnListDiff};
use dashd_chainstate::chain::{ChainViewHandle, NoWaitGuard, ValidationInterface, no_wait_marked};

use crate::exec::GovExec;
use crate::handle::GovernanceHandle;
use crate::sync::DmnTip;

pub struct GovernanceListener<E> {
    handle: Arc<GovernanceHandle<E>>,
    dmn: Arc<dyn DmnTip>,
    index: ChainViewHandle,
}

impl<E: GovExec + 'static> GovernanceListener<E> {
    /// The hooks read the DMN-tip list through `dmn`, and a signalled block's height from V's
    /// block index through `index`.
    pub fn new(
        handle: Arc<GovernanceHandle<E>>,
        dmn: Arc<dyn DmnTip>,
        index: ChainViewHandle,
    ) -> Self {
        Self {
            handle,
            dmn,
            index,
        }
    }

    /// `m_govman.UpdatedBlockTip(new)` while governance is valid: posts the tip command at
    /// `new`'s height, on V's marked lane, and drops its reply.
    fn post_tip(&self, new: BlockId) {
        debug_assert!(no_wait_marked(), "the tip post never waits for V's lane");
        if !self.handle.published().is_loaded {
            return;
        }
        let Some(height) = self.index.entry(new).and_then(|e| i32::try_from(e.height).ok()) else {
            return;
        };
        drop(self.handle.tip(height));
    }
}

impl<E: GovExec + 'static> ValidationInterface for GovernanceListener<E> {
    /// `UpdatedBlockTip(tip, nullptr, ibd)`, on V itself: the post must not block on V's own
    /// bounded lane, so it takes the marked lane and nothing waits for its reply. On a restart the
    /// load has already set the cached height to the active chain's (dashpay/dash#8097), and this
    /// runs the tip work there. V has moved its DMN tip to `tip` first.
    ///
    /// Bound on that unbounded lane: each call adds one tip command, whose effects reach the
    /// sink once, when it runs. Core sends this twice on a masternode's startup.
    // Core: dsnotificationinterface.cpp v24.0.2:31-35; init.cpp v24.0.2:2456-2470, :2541-2551
    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        // A null tip is a null fork too: Core's `pindexNew == pindexFork` return.
        let Some(tip) = tip else {
            return;
        };
        let _marked = NoWaitGuard::set();
        self.updated_block_tip(tip, None, ibd);
    }

    /// Queued, on the signals thread, which never waits for V. The tip command reads the DMN-tip
    /// list when it runs, never the signal's.
    // Core: dsnotificationinterface.cpp v24.0.2:55-72; governance/governance.cpp v24.0.2:1123-1147
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        if fork == Some(new) || ibd {
            return;
        }
        self.post_tip(new);
    }

    /// Inline on V, after the block's superblock answers, on connect and undo alike:
    /// `CheckAndRemove` over the DMN-tip list, never the signal's lists.
    // Core: dsnotificationinterface.cpp v24.0.2:90-96; validation.cpp v24.0.2:2379-2382,
    // :2929-2932
    fn notify_masternode_list_changed(
        &self,
        _undo: bool,
        _old: &Arc<MnList>,
        _diff: &Arc<MnListDiff>,
    ) {
        self.handle.mn_list_changed(&self.dmn.dmn_tip_list());
    }
}
