//! `getblocktemplate` on the node (PLAN-MN1 §3, §12.5): RP3a-3's slot answered by the node's one
//! [`GbtEngine`], and the engine's reads of the node beside V.

use std::sync::{Arc, Weak};

use dashd_mining::gbt::{GbtError, GbtReply, GbtRequest};
use dashd_mining::{GbtEngine, NodeStatus, TemplateError};
use dashd_rpc::mining::{BlockTemplates, GbtAnswer};

use super::{ChainSlot, NetSlot, SyncSlot, validation_state};

impl ChainSlot {
    /// Step 7's engine over the attached chain, until shutdown detaches the slot.
    pub fn attach_gbt(&self, engine: Arc<GbtEngine>) {
        let mut slot = self.slot();
        let writer = slot.as_mut().expect("the engine runs on the attached chain");
        assert!(writer.gbt.replace(engine).is_none(), "one engine per node");
    }

    fn gbt(&self) -> Option<Arc<GbtEngine>> {
        self.slot().as_ref()?.gbt.clone()
    }

    /// `IsRPCRunning()` of the server whose commands use the slot.
    pub(crate) fn rpc_running(&self) -> bool {
        self.server
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upgrade()
            .is_some_and(|s| s.is_running())
    }

    /// `OnRPCStopped`'s `g_best_block_cv.notify_all()`: the attached chain's tip waiters
    /// re-evaluate their predicates, which read [`Self::rpc_running`], already false.
    // Core: v24.0.2 src/init.cpp:564-569
    pub(crate) fn wake_tip_waiters(&self) {
        if let Some(writer) = self.slot().as_ref() {
            writer.chain.tip_watch().wake_all();
        }
    }
}

impl BlockTemplates for ChainSlot {
    fn get_block_template(&self, request: GbtRequest) -> Result<GbtAnswer, GbtError> {
        let engine = self.gbt().ok_or(GbtError::Template(TemplateError::NoChain))?;
        Ok(match engine.handle(request)? {
            GbtReply::Template(t) => GbtAnswer::Template(t),
            GbtReply::Proposal(outcome) => {
                GbtAnswer::Proposal(outcome.map(|state| validation_state(&state)))
            }
        })
    }
}

/// `getblocktemplate`'s reads of `node`: each a lock-free or slot read, never a V turn or the
/// pool's lock.
pub struct GbtStatus {
    chain: Weak<ChainSlot>,
    net: Arc<NetSlot>,
    sync: Arc<SyncSlot>,
}

impl GbtStatus {
    pub fn new(chain: &Arc<ChainSlot>, net: &Arc<NetSlot>, sync: &Arc<SyncSlot>) -> GbtStatus {
        GbtStatus {
            chain: Arc::downgrade(chain),
            net: Arc::clone(net),
            sync: Arc::clone(sync),
        }
    }
}

impl NodeStatus for GbtStatus {
    // Core: v24.0.2 src/rpc/mining.cpp:739, src/rpc/server_util.cpp:134-140
    fn has_connman(&self) -> bool {
        self.net.slot().is_some()
    }

    /// `GetNodeCount(ConnectionDirection::Both)`: inbound and outbound, the peers marked for
    /// disconnection skipped.
    // Core: v24.0.2 src/rpc/mining.cpp:742, src/net.cpp:4543-4563
    fn peer_count(&self) -> usize {
        self.net.core().map_or(0, |core| {
            core.peers().iter().filter(|peer| !peer.disconnect_requested()).count()
        })
    }

    // Core: v24.0.2 src/rpc/mining.cpp:750, src/masternode/sync.h:80
    fn mn_synced(&self) -> bool {
        self.sync.slot().as_ref().is_some_and(|sync| sync.0.is_synced())
    }

    // Core: v24.0.2 src/rpc/mining.cpp:787, :801-802
    fn rpc_running(&self) -> bool {
        self.chain.upgrade().is_some_and(|chain| chain.rpc_running())
    }
}

#[cfg(test)]
#[path = "gbt_tests.rs"]
mod tests;
