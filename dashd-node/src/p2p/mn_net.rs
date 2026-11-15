//! The node's masternode network seam over the published tip and masternode sync state.

use std::sync::Arc;

use dash_p2p::PeerHandle;
use dashd_masternode::sync::MasternodeSync;
use dashd_masternode::tip::TipMnList;
use dashd_p2p::processing::seams::{MasternodeNet, MnListView, NoQuorumWatch, QuorumWatch};

/// The node-owned list and sync half of Core's masternode network state.
pub struct NodeMasternodeNet {
    tip_mn_list: TipMnList,
    sync: Arc<MasternodeSync>,
    watch: Arc<dyn QuorumWatch>,
}

impl NodeMasternodeNet {
    /// Creates a node seam with quorum watching disabled.
    pub fn new(tip_mn_list: TipMnList, sync: Arc<MasternodeSync>) -> Self {
        Self::with_watch(tip_mn_list, sync, Arc::new(NoQuorumWatch))
    }

    /// Creates a node seam with the supplied quorum-watch implementation.
    pub fn with_watch(
        tip_mn_list: TipMnList,
        sync: Arc<MasternodeSync>,
        watch: Arc<dyn QuorumWatch>,
    ) -> Self {
        Self {
            tip_mn_list,
            sync,
            watch,
        }
    }
}

impl MasternodeNet for NodeMasternodeNet {
    // Core: evo/mnauth.cpp:57-60; masternode/sync.h:79 at v24.0.0
    fn blockchain_synced(&self) -> bool {
        self.sync.is_blockchain_synced()
    }

    // Core: net_processing.cpp:4246-4247, :5801 at v24.0.0
    fn tip_list(&self) -> Arc<dyn MnListView> {
        Arc::new(self.tip_mn_list.load())
    }

    // Core: net_processing.cpp:4246-4248; net.cpp:4465-4490 at v24.0.0
    fn wants_qwatch(&self, peer: &PeerHandle) -> bool {
        if !self.watch.is_watching() {
            return false;
        }
        let tip = self.tip_list();
        self.watch.is_quorum_node(peer, &*tip)
    }
}

#[cfg(test)]
mod tests;
