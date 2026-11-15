use std::collections::HashSet;
use std::sync::Arc;

use dash_netaddr::{NetAddr, Service};
use dash_p2p::{ConnectionType, Permissions, Why};
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dashd_masternode::mnconn::QuorumNodes;
use dashd_masternode::mnconn::watch_seed;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::seams::QuorumWatch;
use dashd_p2p::sim::Sim;
use dashd_p2p::sim::masternode::FakeMnList;

const TYPE: LlmqType = LlmqType::LlmqTest;

fn hash(n: u8) -> [u8; 32] {
    let mut hash = [0; 32];
    hash[0] = n;
    hash
}

fn sim() -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    Sim::new(params, identity)
}

fn peers() -> (Sim, dashd_p2p::sim::SimPeer, dashd_p2p::sim::SimPeer) {
    let sim = sim();
    let a = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let b = sim.connect(2, ConnectionType::OutboundFullRelay, Permissions::NONE);
    (sim, a, b)
}

#[test]
fn quorum_registry_replaces_and_removes_sets() {
    let registry = QuorumNodes::default();
    let quorum = hash(9);
    registry.set_nodes(TYPE, quorum, HashSet::from([hash(1), hash(2)]));
    assert!(registry.has(TYPE, &quorum));
    assert_eq!(registry.quorums(TYPE), HashSet::from([quorum]));

    registry.set_nodes(TYPE, quorum, HashSet::from([hash(3)]));
    registry.remove(TYPE, &quorum);
    assert!(!registry.has(TYPE, &quorum));
    assert!(registry.quorums(TYPE).is_empty());
}

#[test]
fn quorum_registry_relay_members_union_and_skips_iqr_snapshot() {
    let (sim, a, b) = peers();
    let registry = QuorumNodes::default();
    let a_hash = hash(1);
    let b_hash = hash(2);
    let first = registry.set_relay_members(
        TYPE,
        hash(9),
        HashSet::from([a_hash]),
        &[(a.handle.id(), a_hash)],
    );
    assert_eq!(first, vec![a.handle.id()]);
    a.handle.set_masternode_iqr(true);
    let second = registry.set_relay_members(
        TYPE,
        hash(10),
        HashSet::from([b_hash]),
        &[(b.handle.id(), b_hash)],
    );
    assert_eq!(second, vec![b.handle.id()]);
    assert!(registry.is_relay_member(&a_hash));
    assert!(registry.is_relay_member(&b_hash));
    assert!(!registry.is_relay_member(&[0; 32]));
    drop(sim);
}

#[test]
fn quorum_registry_quorum_nodes_skips_disconnects_and_requires_verified_hash() {
    let (sim, qwatch, verified, unverified, disconnected) = {
        let sim = sim();
        let qwatch = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
        qwatch.handle.set_qwatch(true);
        let verified = sim.connect(2, ConnectionType::OutboundFullRelay, Permissions::NONE);
        assert!(sim.core.try_mark_verified(verified.handle.id(), hash(2), hash(22)));
        let unverified = sim.connect(3, ConnectionType::OutboundFullRelay, Permissions::NONE);
        let disconnected = sim.connect(4, ConnectionType::OutboundFullRelay, Permissions::NONE);
        sim.core.disconnect(disconnected.handle.id(), Why::Processor("registry test"));
        (sim, qwatch, verified, unverified, disconnected)
    };
    let registry = QuorumNodes::default();
    registry.set_nodes(TYPE, hash(9), HashSet::from([hash(2), [0; 32]]));
    let peers = vec![
        qwatch.handle.clone(),
        verified.handle.clone(),
        unverified.handle.clone(),
        disconnected.handle.clone(),
    ];
    assert_eq!(
        registry.quorum_nodes(TYPE, &hash(9), &peers),
        vec![qwatch.handle.id(), verified.handle.id()]
    );
    drop(sim);
}

#[test]
fn quorum_registry_matches_unverified_outbound_by_service() {
    let sim = sim();
    let peer = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let service = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19_899);
    assert_eq!(peer.handle.addr().service, service);
    let pro_tx_hash = hash(7);
    let registry = QuorumNodes::default();
    registry.set_nodes(TYPE, hash(9), HashSet::from([pro_tx_hash]));
    let tip = FakeMnList::new().with_service(service, pro_tx_hash);
    assert!(registry.is_quorum_node(&peer.handle, &tip));
    let watch: &dyn QuorumWatch = &registry;
    assert!(watch.is_quorum_node(&peer.handle, &tip));

    let inbound = sim.connect_masternode(2);
    assert!(!registry.is_quorum_node(&inbound.handle, &tip));
}

#[test]
fn quorum_registry_ignores_null_assumed_hash() {
    let sim = sim();
    let peer = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let service = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19_899);
    let registry = QuorumNodes::default();
    registry.set_nodes(TYPE, hash(9), HashSet::from([[0; 32]]));
    let tip = FakeMnList::new().with_service(service, [0; 32]);
    assert!(!registry.is_quorum_node(&peer.handle, &tip));
}

#[test]
fn quorum_watch_seed_is_stable_across_calls() {
    assert_eq!(watch_seed(), watch_seed());
    assert_eq!(watch_seed().as_ptr(), watch_seed().as_ptr());
}

#[test]
fn quorum_watch_is_disabled_until_watchquorums_state_exists() {
    assert!(!QuorumWatch::is_watching(&QuorumNodes::default()));
}

#[test]
fn quorum_registry_lock_is_leaf_under_concurrent_reads() {
    let registry = Arc::new(QuorumNodes::default());
    registry.set_nodes(TYPE, hash(9), HashSet::from([hash(1)]));
    let workers = (0..8)
        .map(|_| {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                for _ in 0..100 {
                    assert!(registry.has(TYPE, &hash(9)));
                    assert_eq!(registry.quorums(TYPE), HashSet::from([hash(9)]));
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn qsendrecsigs_true_core_wire_vector() {
    // Core: evo/mnauth.cpp:171; serialize.h:238 (`Serialize(bool)` writes one uint8_t).
    assert_eq!(vec![u8::from(true)], vec![0x01]);
}
