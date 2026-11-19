//! The node's `CMNAuth` handler ([`mnauth_handler`]) in its registry ([`node_registry`]), driven
//! in the processor's sim.
// Core: net_processing.cpp:5801; evo/mnauth.cpp:51-177 at v24.0.0

use std::collections::HashSet;
use std::sync::Arc;

use dash_args::ArgsManager;
use dash_consensus::hash::sha256d;
use dash_dml::testkit::dmn_with;
use dash_dml::{MnList, MnType};
use dash_netaddr::{NetAddr, Service};
use dash_p2p::{CommandString, ConnectionType, Permissions};
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey};
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, ServiceV1};
use dashd_masternode::active::state::{LocalFacts, TipCtx};
use dashd_masternode::active::{ActiveMasternode, OperatorKey, parse};
use dashd_masternode::mnauth::sign_hash;
use dashd_masternode::mnconn::deterministic_outbound;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::seams::{MasternodeSeams, NoPlatformBans, OperatorKeyRef};
use dashd_p2p::processing::serve_mnlistdiff::NoEvoServe;
use dashd_p2p::sim::masternode::{FakeMnList, FakeMnNet};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{ProcessorOptions, Seams};

use super::*;
use crate::test_log::logged;

const PRO: [u8; 32] = [7; 32];
const SECRET: [u8; 32] = [1; 32];
const PEER_VERSION: i32 = 70242;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The remote masternode's operator key, built at runtime.
fn key() -> OperatorKey {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", &hex(&SECRET));
    parse(&args).unwrap().unwrap()
}

fn stored_pubkey() -> [u8; 48] {
    BlsSecretKey::<BlsScIetf>::from_bytes(&SECRET).unwrap().public_key().to_bytes()
}

fn operator() -> OperatorKeyRef {
    let stored = stored_pubkey();
    OperatorKeyRef {
        stored,
        stored_legacy: false,
        pubkey_hash: sha256d(&stored),
    }
}

/// A synced node whose list holds `PRO` under the test key, with the node's own `mnauth`
/// handler over `active` and `quorum_nodes`.
fn sim(active: Option<Arc<ActiveMasternode>>, quorum_nodes: &Arc<QuorumNodes>) -> Sim {
    let list = Arc::new(FakeMnList::new().with(PRO, operator()));
    let meta = Arc::new(MasternodeMetaMan::new());
    let registry = node_registry(
        Box::new(NoEvoServe),
        Arc::new(NoPlatformBans),
        Some(mnauth_handler(&meta, active, quorum_nodes)),
    );
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let mn = MasternodeSeams {
        net: Arc::new(FakeMnNet::new(true, list)),
        signer: None,
    };
    Sim::with_masternode(p, id, ProcessorOptions::default(), registry, Seams::default(), mn, |_| {})
}

/// An inbound peer at 10.0.0.`n` past the handshake.
fn inbound(sim: &mut Sim, n: u8) -> SimPeer {
    let peer = sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
    sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    while sim.pass() {}
    assert!(peer.handle.successfully_connected());
    sim.sent(&peer);
    peer
}

/// Delivers the remote masternode's `mnauth` for `PRO`: the lines the loop logged.
fn auth(sim: &mut Sim, peer: &SimPeer) -> Vec<String> {
    let challenge = peer.handle.sent_mnauth_challenge();
    let hash = sign_hash(&stored_pubkey(), &challenge, !peer.handle.is_inbound(), PEER_VERSION);
    let mut payload = PRO.to_vec();
    payload.extend(key().sign_basic(&hash));
    sim.deliver(peer, &[("mnauth", payload)]);
    let ((), lines) = logged(|| while sim.pass() {});
    lines.into_iter().map(|(_, m)| m).collect()
}

fn valid_line(peer: &SimPeer) -> String {
    format!(
        "CMNAuth::ProcessMessage -- Valid MNAUTH for {}, peer={}",
        dash_evo::types::ProTxHash::from(PRO),
        peer.handle.id()
    )
}

#[test]
fn w2_a_node_without_a_key_verifies_an_inbound_mnauth() {
    let mut sim = sim(None, &Arc::new(QuorumNodes::default()));
    let peer = inbound(&mut sim, 1);
    let lines = auth(&mut sim, &peer);
    assert!(lines.contains(&valid_line(&peer)), "{lines:?}");
    assert_eq!(peer.handle.verified_proreg_hash(), PRO);
    assert_eq!(peer.handle.verified_pubkey_hash(), operator().pubkey_hash);
    assert!(!peer.handle.disconnect_requested());
    assert!(!peer.handle.masternode_iqr());
}

/// The relay-member answer reads the node's quorum registry alone, with or without an active
/// masternode: here a keyed one with no proTxHash yet.
// Core: evo/mnauth.cpp:165-173; net.cpp:4494-4506 at v24.0.0
#[test]
fn w2_the_relay_member_query_is_answered_from_the_registry() {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", &"02".repeat(32));
    let active = dashd_masternode::active::load(&args).unwrap();
    assert!(active.is_some());
    for member in [false, true] {
        let quorum_nodes = Arc::new(QuorumNodes::default());
        if member {
            quorum_nodes.set_relay_members(LlmqType::LlmqTest, [3; 32], HashSet::from([PRO]), &[]);
        }
        let mut sim = sim(active.clone(), &quorum_nodes);
        let peer = inbound(&mut sim, 1);
        let lines = auth(&mut sim, &peer);
        assert!(lines.contains(&valid_line(&peer)), "{lines:?}");
        assert_eq!(peer.handle.masternode_iqr(), member);
        sim.pass();
        let qsendrecsigs = (CommandString::QSENDRECSIGS, vec![1]);
        assert_eq!(sim.sent(&peer).contains(&qsendrecsigs), member);
    }
}

/// An active masternode whose `Init` found it in the list as `pro`.
// Core: active/masternode.cpp:112-173 at v24.0.0
pub(super) fn active_as(pro: [u8; 32]) -> Arc<ActiveMasternode> {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", &"02".repeat(32));
    let active = dashd_masternode::active::load(&args).unwrap().unwrap();
    let service = Service::new(NetAddr::ipv4([127, 0, 0, 1]), 19899);
    let dmn = dmn_with(1, MnType::Regular, |dmn, cold| {
        dmn.pro_tx_hash = pro.into();
        cold.version = 2;
        cold.pub_key_operator =
            dash_dml::OperatorKey::from_value(active.pubkey(), BlsSchemeKind::Basic);
        cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
            AddrV1::from_bytes(service.addr.v1_bytes()),
            service.port,
        ))));
    });
    let mut list = MnList::default_pre_dip3();
    list.insert_for_tests(dmn);
    let tip = TipCtx {
        height: 1000,
        dip3_enforced: true,
        require_routable: false,
    };
    let local = LocalFacts {
        listen: true,
        require_routable: false,
        local_toward_v4: None,
        loopback_listen: service,
        peers: Vec::new(),
        valid: Vec::new(),
        self_connect_ok: false,
    };
    active.init(&tip, &list, &local);
    assert_eq!(active.pro_tx_hash(), Some(pro));
    active
}

/// The first `[k; 32]` for which `deterministic_outbound` with `PRO` is (or is not) itself.
fn me_where_outbound_is_me(is_me: bool) -> [u8; 32] {
    (1..=u8::MAX)
        .map(|k| [k; 32])
        .find(|me| *me != PRO && (deterministic_outbound(*me, PRO) == *me) == is_me)
        .unwrap()
}

/// A second inbound connection of `PRO` when the first is verified: a node with no proTxHash
/// drops the new one; a masternode keeps it, and marks the old inbound as a probe when it is the
/// deterministic outbound side of the pair.
// Core: evo/mnauth.cpp:114-149 at v24.0.0
#[test]
fn w2_a_second_inbound_of_a_verified_masternode_follows_the_handlers_active() {
    let dup = "has already verified as peer";
    for (me, old_probe, new_kept) in [
        (None, false, false),
        (Some(me_where_outbound_is_me(true)), true, true),
        (Some(me_where_outbound_is_me(false)), false, true),
    ] {
        let mut sim = sim(me.map(active_as), &Arc::new(QuorumNodes::default()));
        let old = inbound(&mut sim, 1);
        assert!(auth(&mut sim, &old).contains(&valid_line(&old)), "{me:?}");
        let new = inbound(&mut sim, 2);
        let lines = auth(&mut sim, &new);
        let mut walk = lines.iter().filter(|l| l.contains(dup));
        let line = walk.next().unwrap_or_else(|| panic!("{me:?}: {lines:?}"));
        assert!(walk.next().is_none(), "{me:?}: {lines:?}");
        assert!(line.contains(&format!("{dup} {}, ", old.handle.id())), "{line}");
        if let Some(me) = me {
            let outbound = dash_evo::types::ProTxHash::from(deterministic_outbound(me, PRO));
            let kept = format!("deterministicOutbound={outbound}. peer={}", new.handle.id());
            assert!(line.ends_with(&kept), "{line}");
        } else {
            let drop = format!("dropping new connection. peer={}", new.handle.id());
            assert!(line.ends_with(&drop), "{line}");
        }
        let marked = format!(
            "CMNAuth::ProcessMessage -- marking old inbound for dropping it later, peer={}",
            old.handle.id()
        );
        assert_eq!(lines.contains(&marked), old_probe, "{me:?}: {lines:?}");
        assert_eq!(old.handle.masternode_probe(), old_probe, "{me:?}");
        assert!(!new.handle.masternode_probe(), "{me:?}");
        assert!(!old.handle.disconnect_requested(), "{me:?}");
        assert_eq!(new.handle.disconnect_requested(), !new_kept, "{me:?}");
        assert_eq!(lines.contains(&valid_line(&new)), new_kept, "{me:?}: {lines:?}");
        let verified = if new_kept {
            PRO
        } else {
            [0; 32]
        };
        assert_eq!(new.handle.verified_proreg_hash(), verified, "{me:?}");
    }
}

#[test]
fn w2_the_registry_holds_one_mnauth_handler() {
    let mnauth = CommandString::MNAUTH;
    let quorum_nodes = Arc::new(QuorumNodes::default());
    let meta = Arc::new(MasternodeMetaMan::new());
    let without = node_registry(Box::new(NoEvoServe), Arc::new(NoPlatformBans), None);
    assert!(without.is_stubbed(&mnauth));
    assert_eq!(without.extension_count(), 1);
    let mut with = node_registry(
        Box::new(NoEvoServe),
        Arc::new(NoPlatformBans),
        Some(mnauth_handler(&meta, None, &quorum_nodes)),
    );
    assert!(!with.is_stubbed(&mnauth));
    assert_eq!(with.extension_count(), 2);
    let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with.register(Box::new(mnauth_handler(&meta, None, &quorum_nodes)))
    }));
    assert!(again.is_err(), "a second handler is refused");
}
