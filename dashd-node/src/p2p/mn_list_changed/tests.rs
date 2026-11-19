use std::sync::Arc;

use dash_consensus::hash::sha256d;
use dash_dml::testkit::{dmn, dmn_with, operator_key, pro_tx};
use dash_dml::{Dmn, MnList, MnType};
use dash_p2p::{ConnectionType, Permissions};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_pkc::bls::BlsSchemeKind;
use dashd_chainstate::chain::{NoWaitGuard, ValidationInterface};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::sim::{Sim, SimPeer};

use super::MnAuthListChanged;
use crate::test_log::logged;

fn sim() -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    Sim::new(params, identity)
}

fn list(entries: impl IntoIterator<Item = Arc<Dmn>>) -> Arc<MnList> {
    let mut list = MnList::default_pre_dip3();
    for entry in entries {
        list.insert_for_tests(entry);
    }
    Arc::new(list)
}

fn key_hash(dmn: &Dmn) -> [u8; 32] {
    sha256d(&dmn.cold.pub_key_operator.clone().to_bytes())
}

fn rekeyed(n: u8, key: u8) -> Arc<Dmn> {
    dmn_with(n, MnType::Regular, |_, cold| {
        cold.version = 2;
        cold.pub_key_operator = operator_key(key, BlsSchemeKind::Basic);
    })
}

/// A fully connected peer, verified as `pro` with `key_hash` when given.
fn peer(sim: &Sim, n: u8, verified: Option<(u8, [u8; 32])>) -> SimPeer {
    let peer = sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
    peer.handle.set_successfully_connected_for_tests();
    if let Some((pro, hash)) = verified {
        assert!(sim.core.try_mark_verified(peer.handle.id(), *pro_tx(pro).as_bytes(), hash));
    }
    peer
}

fn notify(sim: &Sim, old: &Arc<MnList>, new: &Arc<MnList>) {
    let listener: Arc<dyn ValidationInterface> =
        Arc::new(MnAuthListChanged::new(Arc::clone(&sim.core)));
    // The signal runs inside V's attempt; the listener never waits on V.
    let _marked = NoWaitGuard::set();
    listener.notify_masternode_list_changed(false, old, &Arc::new(old.diff_to(new)));
}

// Core: evo/mnauth.cpp:197-198, :206-207 at v24.0.0
#[test]
fn core_removed_masternode_disconnects_its_verified_peer() {
    let sim = sim();
    let one = dmn(1, MnType::Regular);
    let old = list([Arc::clone(&one), dmn(2, MnType::Regular)]);
    let new = list([dmn(2, MnType::Regular)]);
    let removed = peer(&sim, 1, Some((1, key_hash(&one))));
    let kept = peer(&sim, 2, Some((2, key_hash(&one))));
    let ((), log) = logged(|| notify(&sim, &old, &new));
    assert!(removed.handle.disconnect_requested());
    assert!(!kept.handle.disconnect_requested());
    let line = format!(
        "CMNAuth::NotifyMasternodeListChanged -- Disconnecting MN {} due to key changed/removed, peer={}",
        one.pro_tx_hash,
        removed.handle.id()
    );
    assert!(log.iter().any(|(level, l)| *level == tracing::Level::DEBUG && *l == line), "{log:?}");
}

// Core: evo/mnauth.cpp:199-202 at v24.0.0
#[test]
fn core_operator_key_change_disconnects_the_verified_peer() {
    let sim = sim();
    let old_dmn = rekeyed(1, 2);
    let old = list([Arc::clone(&old_dmn)]);
    let new = list([rekeyed(1, 3)]);
    let verified = peer(&sim, 1, Some((1, key_hash(&old_dmn))));
    notify(&sim, &old, &new);
    assert!(verified.handle.disconnect_requested());
}

// Core: evo/mnauth.cpp:199-201 at v24.0.0
#[test]
fn core_update_without_the_operator_key_field_disconnects_nothing() {
    let sim = sim();
    let old_dmn = dmn(1, MnType::Regular);
    let old = list([Arc::clone(&old_dmn)]);
    let new = list([dmn_with(1, MnType::Regular, |_, cold| cold.key_id_voting = [9; 20].into())]);
    assert!(!old.diff_to(&new).updated.is_empty());
    let verified = peer(&sim, 1, Some((1, [7; 32])));
    notify(&sim, &old, &new);
    assert!(!verified.handle.disconnect_requested());
}

// Core: evo/mnauth.cpp:200 at v24.0.0
#[test]
fn core_operator_key_change_to_the_verified_hash_disconnects_nothing() {
    let sim = sim();
    let new_dmn = rekeyed(1, 3);
    let old = list([rekeyed(1, 2)]);
    let new = list([Arc::clone(&new_dmn)]);
    let verified = peer(&sim, 1, Some((1, key_hash(&new_dmn))));
    notify(&sim, &old, &new);
    assert!(!verified.handle.disconnect_requested());
}

// Core: evo/mnauth.cpp:187-193; net.h:1432; net.cpp:4874 at v24.0.0
#[test]
fn core_unverified_unknown_and_not_fully_connected_peers_are_skipped() {
    let sim = sim();
    let old = list([dmn(1, MnType::Regular)]);
    let new = list([]);
    let unverified = peer(&sim, 1, None);
    let unknown = peer(&sim, 2, Some((3, [7; 32])));
    let connecting = sim.connect(3, ConnectionType::Inbound, Permissions::NONE);
    assert!(sim.core.try_mark_verified(connecting.handle.id(), *pro_tx(1).as_bytes(), [7; 32]));
    notify(&sim, &old, &new);
    assert!(!unverified.handle.disconnect_requested());
    assert!(!unknown.handle.disconnect_requested());
    assert!(!connecting.handle.disconnect_requested());
}

// Core: evo/mnauth.cpp:181-184 at v24.0.0
#[test]
fn core_diff_with_only_additions_disconnects_nothing() {
    let sim = sim();
    let old = list([dmn(1, MnType::Regular)]);
    let new = list([dmn(1, MnType::Regular), dmn(2, MnType::Regular)]);
    let verified = peer(&sim, 1, Some((1, [7; 32])));
    notify(&sim, &old, &new);
    assert!(!verified.handle.disconnect_requested());
}

/// A peer already marked to disconnect is not fully connected: no second line for it.
// Core: net.h:1432; net.cpp:4874-4877; evo/mnauth.cpp:204-207 at v24.0.0
#[test]
fn core_peer_already_disconnecting_is_skipped() {
    let sim = sim();
    let one = dmn(1, MnType::Regular);
    let old = list([Arc::clone(&one)]);
    let new = list([]);
    let marked = peer(&sim, 1, Some((1, key_hash(&one))));
    sim.core.disconnect(marked.handle.id(), dash_p2p::Why::Processor("earlier"));
    assert!(marked.handle.disconnect_requested());
    let ((), log) = logged(|| notify(&sim, &old, &new));
    assert!(
        !log.iter().any(|(_, l)| l.starts_with("CMNAuth::NotifyMasternodeListChanged")),
        "{log:?}"
    );
}
