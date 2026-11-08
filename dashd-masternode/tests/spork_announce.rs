//! v24.0.0's spork announcement at `verack` (Core a56ed47c98a): on a test chain the node queues
//! one `inv(MSG_SPORK)` per active spork for every new peer with `PushInv`, in `ActiveSporks`
//! order, and section 8 of `SendMessages` sends them with its filters. SP0's handler answers
//! the `getdata` that follows.
//!
//! These tests take the order from the node's own store; that the store's order is Core's
//! `unordered_map` order is what `spork_transcripts` shows against dashd.
// Core: net_processing.cpp:4233-4240, 1228-1257, 6744-6766 (v24.0.0), spork.cpp:127-136

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dash_clock::{AdjustedTime, Clock, NodeSeconds};
use dash_evo::spork::{SporkId, spork_hash};
use dash_netaddr::netaddr::Service;
use dash_num::Hash256;
use dash_p2p::conn::Permissions;
use dash_p2p::{CommandString, ConnectionType, NetAddr, ServiceFlags};
use dash_p2p_core::{GetData, Inv, InvType, Inventory, Spork};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_masternode::sporks::{SporkHandler, SporkManager};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::{MasternodeSeams, MnAuthSigner};
use dashd_p2p::sim::masternode::FakeSigner;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, VirtualClock};
use dashd_p2p::{Registry, Seams};

mod common;

/// The sim's clock as the adjusted time; set once the sim exists.
#[derive(Default)]
struct SimTime(OnceLock<Arc<VirtualClock>>);

impl AdjustedTime for SimTime {
    fn now(&self) -> NodeSeconds {
        self.0.get().expect("the sim's clock").node_now()
    }
}

/// Sporks signed on regtest with Core's functional-test key, in signing order: five at
/// `SIM_START`, then SPORK_2 again a second later. A node that stores them in this order keeps
/// the first SPORK_2 by hash but not as active (`ProcessSpork`), so it is not announced.
fn signed() -> Vec<Spork> {
    let (sim, sporks) = node(Network::Regtest, false);
    sporks.set_priv_key(common::REGTEST_WIF).unwrap();
    let ids = [
        SporkId::SPORK_2_INSTANTSEND_ENABLED,
        SporkId::SPORK_17_QUORUM_DKG_ENABLED,
        SporkId::SPORK_19_CHAINLOCKS_ENABLED,
        SporkId::SPORK_21_QUORUM_ALL_CONNECTED,
        SporkId::SPORK_23_QUORUM_POSE,
    ];
    let mut all = Vec::new();
    for id in ids {
        let hash = sporks.update_spork(id, 0).unwrap();
        all.push(sporks.spork_by_hash(&hash).unwrap());
    }
    sim.clock.set_mock_time(SIM_START + 1);
    let hash = sporks.update_spork(ids[0], 4_070_908_800).unwrap();
    all.push(sporks.spork_by_hash(&hash).unwrap());
    all
}

/// A node on `network` whose registry holds SP0's handler; a masternode (one with a signer,
/// whose proTxHash is null, so it pushes no `mnauth`) when `masternode`.
fn node(network: Network, masternode: bool) -> (Sim, Arc<SporkManager>) {
    let p = Arc::new(ChainParamsBuilder::new(network, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let time = Arc::new(SimTime::default());
    let sporks = Arc::new(SporkManager::new(&p, Arc::clone(&time) as _));
    let mut registry = Registry::new();
    registry.register(Box::new(SporkHandler::new(Arc::clone(&sporks))));
    let mn = MasternodeSeams {
        signer: masternode.then(|| Arc::new(FakeSigner::new(None)) as Arc<dyn MnAuthSigner>),
        ..MasternodeSeams::default()
    };
    let opts = ProcessorOptions::default();
    let sim = Sim::with_masternode(p, id, opts, registry, Seams::default(), mn, |_| {});
    assert!(time.0.set(Arc::clone(&sim.clock)).is_ok(), "one clock");
    (sim, sporks)
}

/// A node on `network` that stored `sporks` in order as the `spork` branch stores a valid one
/// (`ProcessSpork` does not check the signer: on mainnet a node keeps and relays sporks too), and
/// its active sporks' invs in `ActiveSporks` order.
fn node_with(network: Network, masternode: bool, sporks: &[Spork]) -> (Sim, Vec<Inventory>) {
    let (sim, manager) = node(network, masternode);
    for s in sporks {
        manager.process_spork(s.clone(), None);
    }
    let active = manager.active_sporks();
    assert_eq!(active.len(), 5);
    assert!(manager.spork_by_hash(&spork_hash(&sporks[0])).is_some(), "the replaced SPORK_2");
    assert!(!active.contains(&sporks[0]));
    (sim, active.iter().map(spork_inv).collect())
}

fn spork_inv(s: &Spork) -> Inventory {
    Inventory::new(InvType::Spork, Hash256::from_lendian(spork_hash(s)))
}

/// The node's `inv` items among `sent`, in order.
fn invs(sent: &[(CommandString, Vec<u8>)]) -> Vec<Inventory> {
    sent.iter()
        .filter(|(cmd, _)| cmd.as_str() == "inv")
        .flat_map(|(_, p)| Inv::decode(&mut &p[..]).unwrap().inventory().clone())
        .collect()
}

/// The commands among `sent`.
fn cmds(sent: &[(CommandString, Vec<u8>)]) -> Vec<&str> {
    sent.iter().map(|(cmd, _)| cmd.as_str()).collect()
}

/// `peer`'s `version`, then a pass, so the node's `verack` branch is next.
fn version(sim: &mut Sim, peer: &SimPeer) {
    sim.deliver(peer, &[("version", PeerVersionMsg::v24(SIM_START).payload())]);
    sim.pass();
}

/// The handshake from `peer`'s side, then passes until the node is idle: what it sent meanwhile.
/// The node has processed the `verack` (it sent `senddsq`, the branch's last push here).
fn handshake(sim: &mut Sim, peer: &SimPeer) -> Vec<(CommandString, Vec<u8>)> {
    let mut sent = sim.handshake(peer, PeerVersionMsg::v24(SIM_START));
    while sim.pass() {}
    sent.extend(sim.sent(peer));
    assert!(peer.handle.successfully_connected());
    assert!(cmds(&sent).contains(&"senddsq"), "{:?}", cmds(&sent));
    sent
}

/// Test 1: on a test chain, an outbound and an inbound peer each get one `inv(MSG_SPORK)` per
/// active spork at `verack`, in `ActiveSporks` order, queued by `PushInv`: nothing is on the
/// wire when the branch returns, and the invs go out with the next send pass.
#[test]
fn verack_queues_active_spork_invs_for_the_next_send_pass() {
    let sporks = signed();
    let (mut sim, want) = node_with(Network::Regtest, false, &sporks);
    let outbound = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let inbound = sim.connect(2, ConnectionType::Inbound, Permissions::NONE);
    for p in [&outbound, &inbound] {
        version(&mut sim, p);
        sim.sent(p);
        sim.admitted(p, "verack", Vec::new()).unwrap();
        let at_verack = sim.sent(p);
        assert_eq!(cmds(&at_verack), ["sendheaders2", "sendcmpct", "senddsq"]);
        assert_eq!(invs(&at_verack), []);
        sim.pass();
        let next = sim.sent_at_least(p, 1);
        assert_eq!(invs(&next), want, "peer {}", p.handle.id());
        assert_eq!(cmds(&next).iter().filter(|c| **c == "inv").count(), 1, "one batch");
    }
}

/// Test 2: only test chains announce. A mainnet node holding the same sporks sends no spork
/// `inv` at `verack`; a testnet node does.
#[test]
fn only_test_chains_announce_sporks_at_verack() {
    let sporks = signed();
    for (network, announces) in [(Network::Main, false), (Network::Testnet3, true)] {
        let (mut sim, active) = node_with(network, false, &sporks);
        let p = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
        let got = invs(&handshake(&mut sim, &p));
        let want = if announces {
            active
        } else {
            Vec::new()
        };
        assert_eq!(got, want, "{network:?}");
    }
}

/// Test 3: a test-chain node with no active spork announces nothing.
#[test]
fn no_active_sporks_no_inv() {
    let (mut sim, manager) = node(Network::Regtest, false);
    assert!(manager.active_sporks().is_empty());
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    assert_eq!(invs(&handshake(&mut sim, &p)), []);
}

/// Test 4: section 8's known filter. A spork queued at `verack` and again by `RelayInv` before
/// the send pass goes out once, and a later `RelayInv` of it sends nothing.
#[test]
fn a_spork_the_peer_knows_is_not_announced_twice() {
    let sporks = signed();
    let (mut sim, active) = node_with(Network::Regtest, false, &sporks);
    let first = active[0];
    let p = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
    version(&mut sim, &p);
    sim.admitted(&p, "verack", Vec::new()).unwrap();
    sim.shared.relay_inv(&first);
    sim.pass();
    let got = invs(&sim.sent_at_least(&p, 1));
    assert_eq!(got, active);
    sim.shared.relay_inv(&first);
    while sim.pass() {}
    assert_eq!(invs(&sim.sent(&p)), []);
}

/// Test 5: masternode connections, inbound and outbound, get the invs. Core drops every message
/// before `verack`, so MNAUTH comes after the announcement: at `verack` the connection is not
/// relay-capable (`CanRelay`), and MNAUTH may make it an intra-quorum relay connection before the
/// send pass. Section 8's non-tx loop has no `CanRelay` gate (its gates are the blocks-only
/// types, the known filter and ISDLOCK), so the invs go out either way, as Core's comment at the
/// announcement says: "this includes masternode connections, which only become relay-capable
/// after MNAUTH".
// Core: net_processing.cpp:4233-4240, 4381-4383, 6744-6766 (v24.0.0); evo/mnauth.cpp
// (`m_masternode_iqr_connection`)
#[test]
fn masternode_connections_get_the_invs_before_and_after_mnauth() {
    let sporks = signed();
    for inbound in [true, false] {
        for iqr_after_mnauth in [false, true] {
            let (mut sim, want) = node_with(Network::Regtest, true, &sporks);
            let mn = if inbound {
                sim.connect_masternode(9)
            } else {
                let service = Service {
                    addr: NetAddr::ipv4([10, 0, 0, 9]),
                    port: 19899,
                };
                let conn = ConnectionType::OutboundFullRelay;
                let services = ServiceFlags::NODE_NETWORK;
                sim.attach_at(service, conn, Permissions::NONE, services, true, false)
            };
            // An inbound peer's `version` says it is a masternode (`fOtherMasternode`, its last
            // byte); a node that is not one would disconnect it.
            let mut v = PeerVersionMsg::v24(SIM_START).payload();
            *v.last_mut().unwrap() = 1;
            sim.deliver(&mn, &[("version", v)]);
            sim.pass();
            sim.admitted(&mn, "verack", Vec::new()).unwrap();
            let case = format!("inbound: {inbound}, IQR after MNAUTH: {iqr_after_mnauth}");
            assert!(mn.handle.successfully_connected(), "{case}");
            assert!(mn.handle.masternode_connection(), "{case}");
            assert!(!mn.handle.can_relay(), "not relay-capable at verack; {case}");
            // MNAUTH's outcome, before the send pass that carries the queued invs.
            mn.handle.set_masternode_iqr(iqr_after_mnauth);
            // Masternode connections are sent to once per 100 ms of the steady clock.
            let mut got = Vec::new();
            for _ in 0..4 {
                sim.clock.advance(Duration::from_millis(100));
                while sim.pass() {}
                got.extend(invs(&sim.sent(&mn)));
            }
            assert_eq!(mn.handle.can_relay(), iqr_after_mnauth, "{case}");
            assert_eq!(got, want, "{case}");
        }
    }
}

/// Test 6: the peer's `getdata(MSG_SPORK)` for an announced hash gets the `spork` through SP0's
/// handler (`ProcessGetData`'s `MSG_SPORK` branch).
// Core: net_processing.cpp:3022-3026 (v24.0.0)
#[test]
fn getdata_for_an_announced_spork_gets_the_spork() {
    let sporks = signed();
    let (mut sim, active) = node_with(Network::Regtest, false, &sporks);
    let p = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
    let announced = invs(&handshake(&mut sim, &p));
    assert_eq!(announced, active);
    let mut req = Vec::new();
    GetData::new(announced.clone()).encode(&mut req);
    sim.deliver(&p, &[("getdata", req)]);
    while sim.pass() {}
    let sent = sim.sent_at_least(&p, announced.len());
    let served: Vec<Inventory> = sent
        .iter()
        .filter(|(cmd, _)| cmd.as_str() == "spork")
        .map(|(_, payload)| spork_inv(&Spork::decode(&mut &payload[..]).unwrap()))
        .collect();
    assert_eq!(served, announced);
}
