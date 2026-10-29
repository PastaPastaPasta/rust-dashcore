//! SP1 on the network (feature `net`): the `ssc` branch of Core's `SyncManager::ProcessMessage`
//! as P2a-core's registry handler, and [`CoreSyncNet`], the sync tick's [`SyncNet`] over the
//! network core.

use std::sync::{Arc, OnceLock};

use dash_netaddr::netaddr::Service;
use dash_num::Hash256;
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, ConnectionType, NetCore, OutMsg, PeerHandle, Why};
use dash_p2p_core::{GovSync, Ssc};
use dashd_p2p::processing::logic::{RequestedObjects, out_msg};
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::{HandlerCx, NetHandler};

use crate::meta::GovHash;
use crate::sync::MasternodeSync;
use crate::sync_manager::{SyncMsg, SyncNet, SyncPeer};

const COMMANDS: [CommandString; 1] = [CommandString::SSC];

/// Core's `SyncManager` as a registry handler: its `ProcessMessage` (`ssc`). The tick is
/// [`crate::sync_manager::SyncManager`]'s, on the scheduler.
pub struct SyncStatusHandler {
    sync: Arc<MasternodeSync>,
}

impl SyncStatusHandler {
    pub fn new(sync: Arc<MasternodeSync>) -> SyncStatusHandler {
        SyncStatusHandler {
            sync,
        }
    }
}

impl NetHandler for SyncStatusHandler {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::SyncManager)
    }

    /// Synced, Core returns before the read, so a short payload does not throw then.
    // Core: node/sync_manager.cpp:319-332 SyncManager::ProcessMessage
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        debug_assert_eq!(*cmd, CommandString::SSC, "the registry routes only ssc here");
        if self.sync.is_synced() {
            return Ok(());
        }
        let mut data = payload;
        let ssc: Ssc = cx.read(&mut data)?;
        tracing::debug!(
            target: "mnsync",
            "SYNCSTATUSCOUNT -- got inventory count: nItemID={}  nCount={}  peer={}",
            ssc.item_id,
            ssc.count,
            cx.id()
        );
        Ok(())
    }
}

/// The sync tick's view of the network core. The object tracker's counts arrive once the
/// processor exists ([`CoreSyncNet::bind_requests`]); before, no peer has announced anything.
pub struct CoreSyncNet {
    core: Arc<NetCore>,
    /// `-blocksonly`, for `IsBlockRelayOnly()`.
    blocks_only: bool,
    requests: OnceLock<RequestedObjects>,
}

impl CoreSyncNet {
    pub fn new(core: Arc<NetCore>, blocks_only: bool) -> CoreSyncNet {
        CoreSyncNet {
            core,
            blocks_only,
            requests: OnceLock::new(),
        }
    }

    /// The processor's object tracker (P2a-core's `m_object_request`), once.
    pub fn bind_requests(&self, requests: RequestedObjects) {
        let _ = self.requests.set(requests);
    }

    fn peer(&self, p: &PeerHandle) -> SyncPeer {
        SyncPeer {
            id: p.id(),
            addr: p.addr().service.clone(),
            can_relay: p.can_relay(),
            inbound: p.is_inbound(),
            unrestricted: p.has_permission(Permissions::NO_BAN)
                || p.conn_type() == ConnectionType::Manual,
            // Core: net.cpp:554-560 CNode::IsBlockRelayOnly
            block_relay_only: (self.blocks_only && !p.has_permission(Permissions::RELAY))
                || p.conn_type() == ConnectionType::BlockRelay,
        }
    }
}

impl SyncNet for CoreSyncNet {
    // Core: net.cpp:4852-4855 NodeFullyConnected, 4943-4959 NodesSnapshot
    fn fully_connected(&self) -> Vec<SyncPeer> {
        self.core
            .peers()
            .iter()
            .filter(|p| p.successfully_connected() && !p.disconnect_requested())
            .map(|p| self.peer(p))
            .collect()
    }

    fn all_addrs(&self) -> Vec<Service> {
        self.core.peers().iter().map(|p| p.addr().service.clone()).collect()
    }

    fn push(&self, peer: i64, msg: SyncMsg) {
        let Some(p) = self.core.peer(peer) else {
            return;
        };
        let out = match msg {
            SyncMsg::GovSync {
                hash,
                filter,
            } => out_msg(
                CommandString::GOVSYNC,
                &GovSync {
                    hash: Hash256::from(hash),
                    bloom_filter: filter,
                },
            ),
            SyncMsg::Mempool => OutMsg::new(CommandString::MEMPOOL, Vec::new()),
        };
        self.core.push(&p, out);
    }

    fn disconnect(&self, peer: i64) {
        self.core.disconnect(peer, Why::Processor("recently synced"));
    }

    fn requested_object_count(&self, peer: i64) -> usize {
        self.requests.get().map_or(0, |r| r.count(peer))
    }

    /// rdashd has no masternode mode yet (AM1).
    fn is_active_masternode(&self) -> bool {
        false
    }

    fn shuffle(&self, hashes: &mut [GovHash]) {
        self.core.with_rng(|rng| rng.shuffle(hashes));
    }
}
