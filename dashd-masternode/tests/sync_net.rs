#![cfg(feature = "net")]

use std::sync::Arc;

use dash_clock::Clock;
use dash_p2p::ConnectionType;
use dash_p2p::conn::Permissions;
use dash_p2p_core::{FilterLoad, GovSync};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_types::codec::BaseCodec;
use dashd_masternode::meta::GovHash;
use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
use dashd_masternode::sync::{MASTERNODE_SYNC_BLOCKCHAIN, MasternodeSync};
use dashd_masternode::sync_manager::{
    AskedRecently, FullSyncNotifier, GovernanceSync, SyncConfig, SyncManager, SyncNet,
    empty_bloom_filter,
};
use dashd_masternode::sync_net::CoreSyncNet;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{Registry, Seams};

struct Governance;

impl GovernanceSync for Governance {
    fn is_valid(&self) -> bool {
        true
    }

    fn vote_count(&self) -> i32 {
        0
    }

    fn enabled_masternode_count(&self) -> i32 {
        1
    }

    fn fetch_governance_object_votes(
        &self,
        _peers_per_hash_max: usize,
        _now: i64,
        _asked: &mut AskedRecently,
    ) -> (Vec<GovHash>, Vec<GovHash>) {
        (vec![[1; 32]], Vec::new())
    }

    fn vote_bloom_filter(&self, _hash: &GovHash) -> FilterLoad {
        empty_bloom_filter()
    }
}

fn node(active_masternode: bool) -> (Sim, Arc<CoreSyncNet>, SyncManager) {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let sim = Sim::with(
        params,
        identity,
        ProcessorOptions::default(),
        Registry::new(),
        Seams::default(),
        |cfg| cfg.active_masternode = active_masternode,
    );
    let clock: Arc<dyn Clock> = sim.clock.clone();
    let net = Arc::new(CoreSyncNet::new(Arc::clone(&sim.core), false));
    let nf = Arc::new(NetFulfilledRequestManager::new(Arc::clone(&clock), 300));
    let dir = tempfile::tempdir().unwrap();
    assert!(nf.load_cache(
        &dir.path().join("netfulfilled.dat"),
        sim.core.config().params.message_start,
        false
    ));
    let sync = Arc::new(MasternodeSync::new(
        Arc::clone(&clock),
        Box::new(FullSyncNotifier {
            net: net.clone(),
            netfulfilled: Arc::clone(&nf),
        }),
    ));
    let cfg = SyncConfig {
        mockable_chain: false,
        mainnet: false,
        sync_mempool: false,
    };
    let mgr = SyncManager::new(clock, cfg, sync, nf, net.clone(), Arc::new(Governance));
    (sim, net, mgr)
}

fn connect(sim: &mut Sim, id: u8, kind: ConnectionType) -> SimPeer {
    let peer = sim.connect(id, kind, Permissions::NONE);
    sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    while sim.pass() {}
    sim.sent(&peer);
    assert!(peer.handle.successfully_connected());
    assert!(peer.handle.can_relay());
    peer
}

fn requested_hashes(sim: &Sim, peer: &SimPeer) -> Vec<GovHash> {
    sim.sent(peer)
        .into_iter()
        .filter(|(cmd, _)| cmd.as_str() == "govsync")
        .map(|(_, payload)| {
            *GovSync::decode(&mut payload.as_slice()).unwrap().object_hash().as_bytes()
        })
        .collect()
}

// Core: node/sync_manager.cpp:186, :264-270, :281-286 (v24.0.0).
#[test]
fn core_flag_controls_inbound_governance_sync() {
    for active in [false, true] {
        let (mut sim, net, mgr) = node(active);
        assert_eq!(net.is_active_masternode(), active);
        let inbound = connect(&mut sim, 1, ConnectionType::Inbound);
        let outbound = connect(&mut sim, 2, ConnectionType::OutboundFullRelay);
        mgr.sync().switch_to_next_asset();
        mgr.process_tick();
        sim.clock.set_mock_time(SIM_START + 6);
        mgr.process_tick();
        let inbound_hashes = requested_hashes(&sim, &inbound);
        let outbound_hashes = requested_hashes(&sim, &outbound);
        assert_eq!(
            inbound_hashes,
            if active {
                vec![]
            } else {
                vec![[0; 32], [1; 32]]
            }
        );
        assert_eq!(
            outbound_hashes,
            if active {
                vec![[0; 32], [1; 32]]
            } else {
                vec![]
            }
        );
        assert_eq!(mgr.sync().attempt(), 1);
    }
}

// Core: node/sync_manager.cpp:112, :166-169 (v24.0.0).
#[test]
fn core_flag_controls_inbound_vote_requests_after_sync() {
    for active in [false, true] {
        let (mut sim, _, mgr) = node(active);
        let inbound = connect(&mut sim, 1, ConnectionType::Inbound);
        let outbound = connect(&mut sim, 2, ConnectionType::OutboundFullRelay);
        mgr.sync().switch_to_next_asset();
        mgr.sync().switch_to_next_asset();
        assert!(mgr.sync().is_synced());
        mgr.process_tick();
        sim.clock.set_mock_time(SIM_START + 6);
        mgr.process_tick();
        assert_eq!(
            requested_hashes(&sim, &inbound),
            if active {
                vec![]
            } else {
                vec![[1; 32]]
            }
        );
        assert_eq!(requested_hashes(&sim, &outbound), vec![[1; 32]]);
        assert!(!inbound.handle.disconnect_requested());
        assert!(!outbound.handle.disconnect_requested());
    }
}

// Core: node/sync_manager.cpp:150-155 (v24.0.0).
#[test]
fn core_flag_controls_restart_after_an_hour_without_ticks() {
    for active in [false, true] {
        let (sim, _, mgr) = node(active);
        mgr.sync().switch_to_next_asset();
        mgr.sync().switch_to_next_asset();
        mgr.process_tick();
        sim.clock.set_mock_time(SIM_START + 3600);
        mgr.process_tick();
        assert!(mgr.sync().is_synced(), "exactly an hour, active={active}");
        sim.clock.set_mock_time(SIM_START + 7201);
        mgr.process_tick();
        if active {
            assert!(mgr.sync().is_synced());
        } else {
            assert_eq!(mgr.sync().asset_id(), MASTERNODE_SYNC_BLOCKCHAIN);
        }
    }
}
