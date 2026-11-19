//! SIG-18's pure relay selector over subscriptions negotiated by simulated peers.
//! The live recovered-signature relay remains unwired.

use std::sync::Arc;

use dash_evo::locks::RecoveredSig;
use dash_p2p::ConnectionType;
use dash_p2p::conn::Permissions;
use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::recsig::{RecsigPush, recsig_relay};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim};

#[test]
fn non_mn_node_selects_negotiated_recsig_subscribers() {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut sim = Sim::new(params, identity);
    assert!(!sim.shared.identity().masternode);
    let peers: Vec<_> = (1..=3)
        .map(|n| {
            let peer = sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
            sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
            peer
        })
        .collect();
    for peer in &peers[..2] {
        sim.deliver(peer, &[("qsendrecsigs", vec![1])]);
    }
    while sim.pass() {}

    // The existing stub decodes a recsig and consumes its request; selection is still pure.
    let mut payload = vec![0x5a; RecoveredSig::LEN];
    payload[0] = 100; // LLMQ_TEST.
    sim.deliver(&peers[2], &[("qsigrec", payload)]);
    while sim.pass() {}

    let facts = || {
        peers.iter().map(|peer| {
            assert_eq!(peer.handle.verified_proreg_hash(), [0; 32]);
            let id = peer.handle.id();
            (id, sim.shared.with_state(id, |state| state.wants_recsigs).unwrap())
        })
    };
    for (proactive, push) in [(true, RecsigPush::QsigRec), (false, RecsigPush::Inv)] {
        let mut got = recsig_relay(facts(), proactive);
        got.sort_unstable_by_key(|&(id, _)| id);
        let mut expected = [(peers[0].handle.id(), push), (peers[1].handle.id(), push)];
        expected.sort_unstable_by_key(|&(id, _)| id);
        assert_eq!(got, expected);
    }

    sim.deliver(&peers[0], &[("qsendrecsigs", vec![0])]);
    while sim.pass() {}
    let facts = peers.iter().map(|peer| {
        let id = peer.handle.id();
        (id, sim.shared.with_state(id, |state| state.wants_recsigs).unwrap())
    });
    assert_eq!(recsig_relay(facts, false), [(peers[1].handle.id(), RecsigPush::Inv)]);
}
