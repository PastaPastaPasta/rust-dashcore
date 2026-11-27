//! g6: `GbtStatus` reads its four sources: the network slot (attached, both directions), the
//! sync slot's `IsSynced` (not `IsBlockchainSynced`), and the slot's server's `IsRPCRunning`.
// Core: v24.0.2 src/rpc/mining.cpp:739-753, :802; src/net.cpp:4543-4563; src/masternode/sync.h:79-80

use std::sync::Arc;

use dash_clock::{MockClock, SystemClock};
use dash_p2p::{
    Address, Attach, ConnectionType, MemSocket, NetAddr, Permissions, Service, ServiceFlags, Why,
};
use dashd_masternode::sync::{MasternodeSync, NodeSyncNotifier};
use dashd_mining::NodeStatus;

use super::GbtStatus;
use crate::rpc::{ChainSlot, NetSlot, SyncSlot, command_table};
use crate::scheduler::Scheduler;
use crate::shutdown::ShutdownState;

struct Quiet;

impl NodeSyncNotifier for Quiet {
    fn sync_reset(&self) {}

    fn sync_finished(&self) {}
}

#[test]
fn detached_slots_read_as_absent() {
    let s = GbtStatus::new(&Arc::default(), &Arc::default(), &Arc::default());
    assert!(!s.has_connman());
    assert_eq!(s.peer_count(), 0);
    assert!(!s.mn_synced());
    assert!(!s.rpc_running());
}

/// `mn_synced` is FINISHED only: past BLOCKCHAIN (`IsBlockchainSynced`) is not enough.
#[test]
fn mn_synced_is_is_synced() {
    let sync = Arc::new(MasternodeSync::new(Arc::new(SystemClock), Box::new(Quiet)));
    let slot = Arc::new(SyncSlot::default());
    slot.attach(&sync);
    let s = GbtStatus::new(&Arc::default(), &Arc::default(), &slot);
    assert!(!s.mn_synced(), "BLOCKCHAIN");
    sync.switch_to_next_asset();
    assert!(sync.is_blockchain_synced());
    assert!(!s.mn_synced(), "GOVERNANCE: blockchain synced, not synced");
    sync.switch_to_next_asset();
    assert!(s.mn_synced(), "FINISHED");
}

/// `rpc_running` follows the server the slot's commands run on: not before it starts, then
/// until it is interrupted.
#[test]
fn rpc_running_is_the_servers() {
    let (logger, _) = crate::init::test_logger();
    let scheduler = Scheduler::new();
    let chain = Arc::new(ChainSlot::default());
    let (net, sync) = (Arc::default(), Arc::default());
    let s = GbtStatus::new(&chain, &net, &sync);
    let table = command_table(
        &crate::chain::tests::regtest(),
        &logger,
        &Arc::new(ShutdownState::new()),
        &Arc::new(MockClock::new()),
        (true, &scheduler),
        &chain,
        &net,
        &sync,
        &Arc::default(),
        Default::default(),
        std::env::temp_dir(),
    );
    assert!(table.list_commands().iter().any(|c| c == "getblocktemplate"));
    assert!(!s.rpc_running(), "not started");
    table.start();
    assert!(s.rpc_running());
    table.interrupt();
    assert!(!s.rpc_running(), "interrupted");
    table.stop();
    drop(table);
    assert!(!s.rpc_running(), "gone");
}

fn attach(net: &NetSlot, i: u8, conn_type: ConnectionType, far: &mut Vec<MemSocket>) {
    let (ours, theirs) = MemSocket::pair();
    far.push(theirs);
    net.core().expect("attached").attach(Attach {
        sock: Arc::new(ours),
        addr: Address {
            service: Service::new(NetAddr::ipv4([30 + i, 1, 1, 1]), 19899),
            services: ServiceFlags::NODE_NETWORK,
            time: 0,
        },
        addr_bind: Service::default(),
        dest: None,
        conn_type,
        inbound_onion: false,
        permissions: Permissions::NONE,
        use_v2: false,
        grant: None,
        masternode: false,
        masternode_probe: false,
    });
}

/// On a node's started network the connection manager is there, and `peer_count` counts an
/// inbound and an outbound peer: `GetNodeCount(ConnectionDirection::Both)`, which skips a peer
/// marked for disconnection.
#[test]
fn peer_count_counts_both_directions() {
    let dir = tempfile::tempdir().unwrap();
    let argv = ["-connect=0", "-dnsseed=0", "-fixedseeds=0", "-listen=0"];
    let node = crate::p2p::amb::node(dir.path(), &argv);
    let net = node.rpc_net();
    let s = GbtStatus::new(&node.rpc_chain(), &net, &Arc::default());
    assert!(s.has_connman());
    assert_eq!(s.peer_count(), 0);
    let mut far = Vec::new();
    attach(&net, 1, ConnectionType::Inbound, &mut far);
    attach(&net, 2, ConnectionType::OutboundFullRelay, &mut far);
    assert_eq!(s.peer_count(), 2);
    // `fDisconnect` peers are skipped; the sweep may remove the marked peer before the read.
    let core = net.core().expect("attached");
    let pinned = (3..53).any(|i| {
        let before: Vec<_> = core.peers().iter().map(|p| p.id()).collect();
        attach(&net, i, ConnectionType::Inbound, &mut far);
        let peer = core.peers().iter().map(|p| p.id()).find(|id| !before.contains(id));
        let peer = peer.expect("attached");
        core.disconnect(peer, Why::Processor("test"));
        let count = s.peer_count();
        if !core.peers().iter().any(|p| p.id() == peer) {
            return false;
        }
        assert_eq!(count, 2, "a peer marked for disconnection was counted");
        true
    });
    assert!(pinned, "the sweep removed the marked peer before every read");
}
