use std::sync::Arc;
use std::time::Duration;

use dash_clock::Clock;
use dash_evo::bls::BlsCtx;
use dash_evo::locks::{ClsigHashSchemes, clsig_hash};
use dash_num::Hash256;
use dash_p2p::{CommandString, ConnectionType, PeerId, Permissions};
use dash_p2p_core::{ClSig, GetData, Inv, InvType, Inventory};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::registry::MisbehavingError;
use dashd_p2p::processing::seams::{ChainLocks, MasternodeSeams, Read};
use dashd_p2p::sim::masternode::FakeSigner;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{MessageProcessingResult, Registry, Seams};

struct FixedChainLocks(Option<ClSig>);

impl ChainLocks for FixedChainLocks {
    fn is_enabled(&self) -> bool {
        true
    }

    fn already_have(&self, _hash: &[u8; 32]) -> bool {
        false
    }

    fn process_new_chainlock(
        &mut self,
        _from: PeerId,
        _clsig: Read<ClSig>,
        _hash: &[u8; 32],
    ) -> MessageProcessingResult {
        MessageProcessingResult::default()
    }

    fn chainlock_by_hash(&self, _hash: &[u8; 32], _write: BlsCtx) -> Option<ClSig> {
        None
    }

    fn best_chainlock(&self, _write: BlsCtx) -> Option<ClSig> {
        self.0.clone()
    }
}

fn encode(m: &impl BaseCodec) -> Vec<u8> {
    let mut bytes = Vec::new();
    m.encode(&mut bytes);
    bytes
}

fn lock() -> ClSig {
    ClSig::new(10, [0xab; 32].into(), [0; 96].into())
}

fn lock_inv() -> Inventory {
    let hash = clsig_hash(&lock(), ClsigHashSchemes::both(BlsCtx::BASIC)).unwrap();
    Inventory::new(InvType::ClSig, Hash256::from_lendian(hash))
}

fn sim(best: Option<ClSig>, bloom: bool, masternode: bool) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let options = IdentityOptions {
        peer_bloom_filters: bloom,
        ..IdentityOptions::new()
    };
    let identity = local_identity(&params, &options).unwrap();
    Sim::with_masternode(
        params,
        identity,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            chainlocks: Box::new(FixedChainLocks(best)),
            ..Seams::default()
        },
        MasternodeSeams {
            signer: masternode.then(|| Arc::new(FakeSigner::new(None)) as _),
            ..MasternodeSeams::default()
        },
        |cfg| cfg.local_services = local_services(&options),
    )
}

fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

fn peer(sim: &mut Sim, n: u8, connection: ConnectionType, permissions: Permissions) -> SimPeer {
    let peer = sim.connect(n, connection, permissions);
    sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    settle(sim);
    sim.sent(&peer);
    peer
}

fn inventory(sim: &Sim, peer: &SimPeer, command: CommandString, frames: usize) -> Vec<Inventory> {
    sim.sent_at_least(peer, frames)
        .into_iter()
        .filter(|(cmd, _)| *cmd == command)
        .flat_map(|(_, payload)| {
            if command == CommandString::GETDATA {
                GetData::decode(&mut &payload[..]).unwrap().inventory().clone()
            } else {
                Inv::decode(&mut &payload[..]).unwrap().inventory().clone()
            }
        })
        .collect()
}

fn request_mempool(sim: &mut Sim, peer: &SimPeer) {
    sim.deliver(peer, &[("mempool", Vec::new())]);
    settle(sim);
}

fn next_trickle(sim: &mut Sim, peer: &SimPeer) {
    let next = sim.shared.with_state(peer.handle.id(), |st| st.next_inv_send_us).unwrap();
    let delta = (next - sim.clock.node_now_micros()).max(0) + 1;
    sim.clock.advance(Duration::from_micros(delta as u64));
    settle(sim);
}

#[test]
fn mempool_clsig_waits_for_the_next_trickle_and_repeated_requests_bypass_known() {
    let mut sim = sim(Some(lock()), true, false);
    let peer = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE);
    request_mempool(&mut sim, &peer);
    assert_eq!(
        sim.shared.with_state(peer.handle.id(), |st| (st.send_mempool, st.last_mempool_req)),
        Some((true, 0))
    );
    assert!(inventory(&sim, &peer, CommandString::INV, 0).is_empty());

    let next = sim.shared.with_state(peer.handle.id(), |st| st.next_inv_send_us).unwrap();
    sim.clock.advance(Duration::from_micros((next - sim.clock.node_now_micros()) as u64));
    settle(&mut sim);
    assert!(inventory(&sim, &peer, CommandString::INV, 0).is_empty());
    sim.clock.advance(Duration::from_micros(1));
    settle(&mut sim);
    assert_eq!(inventory(&sim, &peer, CommandString::INV, 1), [lock_inv()]);
    assert_eq!(
        sim.shared.with_state(peer.handle.id(), |st| (st.send_mempool, st.last_mempool_req)),
        Some((false, sim.clock.node_now().0))
    );
    next_trickle(&mut sim, &peer);
    assert!(inventory(&sim, &peer, CommandString::INV, 0).is_empty());

    request_mempool(&mut sim, &peer);
    next_trickle(&mut sim, &peer);
    assert_eq!(inventory(&sim, &peer, CommandString::INV, 1), [lock_inv()]);
}

#[test]
fn mempool_without_a_chainlock_still_completes_the_request() {
    let mut sim = sim(None, true, false);
    let peer = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE);
    request_mempool(&mut sim, &peer);
    next_trickle(&mut sim, &peer);
    assert!(inventory(&sim, &peer, CommandString::INV, 0).is_empty());
    assert_eq!(
        sim.shared.with_state(peer.handle.id(), |st| (st.send_mempool, st.last_mempool_req)),
        Some((false, sim.clock.node_now().0))
    );
}

#[test]
fn mempool_without_tx_relay_is_ignored() {
    let mut sim = sim(Some(lock()), true, false);
    let peer = peer(&mut sim, 1, ConnectionType::BlockRelay, Permissions::NONE);
    assert_eq!(sim.shared.with_state(peer.handle.id(), |st| st.tx_relay), Some(false));
    request_mempool(&mut sim, &peer);
    next_trickle(&mut sim, &peer);
    assert!(inventory(&sim, &peer, CommandString::INV, 0).is_empty());
    assert_eq!(
        sim.shared.with_state(peer.handle.id(), |st| (st.send_mempool, st.last_mempool_req)),
        Some((false, 0))
    );
    assert!(!peer.handle.disconnect_requested());
}

#[test]
fn mempool_requires_node_bloom_or_mempool_permission() {
    for (permissions, disconnect, reply) in [
        (Permissions::NONE, true, false),
        (Permissions::NO_BAN, false, false),
        (Permissions::MEMPOOL, false, true),
    ] {
        let mut sim = sim(Some(lock()), false, false);
        let peer = peer(&mut sim, 1, ConnectionType::Inbound, permissions);
        request_mempool(&mut sim, &peer);
        next_trickle(&mut sim, &peer);
        assert_eq!(peer.handle.disconnect_requested(), disconnect);
        let got = inventory(&sim, &peer, CommandString::INV, usize::from(reply));
        assert_eq!(
            got,
            if reply {
                vec![lock_inv()]
            } else {
                vec![]
            }
        );
        assert_eq!(sim.shared.misbehavior(peer.handle.id()), Some((0, false)));
    }
}

#[test]
fn mempool_tail_marks_clsig_known_before_ordinary_relay() {
    let mut sim = sim(Some(lock()), true, false);
    let peer = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE);
    request_mempool(&mut sim, &peer);
    sim.shared.relay_inv(&lock_inv());
    next_trickle(&mut sim, &peer);
    assert_eq!(inventory(&sim, &peer, CommandString::INV, 1), [lock_inv()]);
    sim.shared.relay_inv(&lock_inv());
    settle(&mut sim);
    assert!(inventory(&sim, &peer, CommandString::INV, 0).is_empty());
}

#[test]
fn noban_and_masternode_pairs_bypass_the_trickle_delay() {
    for masternode in [false, true] {
        let mut sim = sim(Some(lock()), true, masternode);
        let permissions = if masternode {
            Permissions::NONE
        } else {
            Permissions::NO_BAN
        };
        let peer = peer(&mut sim, 1, ConnectionType::Inbound, permissions);
        let next = sim.shared.with_state(peer.handle.id(), |st| st.next_inv_send_us).unwrap();
        assert!(next > sim.clock.node_now_micros());
        if masternode {
            assert!(sim.core.try_mark_verified(peer.handle.id(), [1; 32], [2; 32]));
        }
        request_mempool(&mut sim, &peer);
        assert_eq!(inventory(&sim, &peer, CommandString::INV, 1), [lock_inv()]);
        assert_eq!(sim.shared.with_state(peer.handle.id(), |st| st.next_inv_send_us), Some(next));
    }
}

#[test]
fn external_results_forget_requests_relay_to_all_inv_peers_and_score_nobody() {
    let mut sim = sim(None, true, false);
    sim.chain.set_ibd(false);
    let source = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE);
    let other = peer(&mut sim, 2, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let blocks = peer(&mut sim, 3, ConnectionType::BlockRelay, Permissions::NONE);
    assert_eq!(sim.shared.with_state(blocks.handle.id(), |st| st.tx_relay), Some(false));
    sim.deliver(&source, &[("inv", encode(&Inv::new(vec![lock_inv()])))]);
    settle(&mut sim);
    assert_eq!(inventory(&sim, &source, CommandString::GETDATA, 1), [lock_inv()]);
    let requests = sim.lp.logic().requested_objects();
    assert_eq!(requests.count(source.handle.id()), 1);
    sim.shared.post_process_external(MessageProcessingResult {
        error: Some(MisbehavingError {
            score: 100,
            message: "no sending peer".into(),
        }),
        to_erase: Some(lock_inv()),
        inventory: vec![lock_inv()],
    });
    assert_eq!(requests.count(source.handle.id()), 0);
    settle(&mut sim);
    assert!(inventory(&sim, &source, CommandString::INV, 0).is_empty());
    for peer in [&other, &blocks] {
        assert_eq!(inventory(&sim, peer, CommandString::INV, 1), [lock_inv()]);
    }
    for peer in [&source, &other, &blocks] {
        assert_eq!(sim.shared.misbehavior(peer.handle.id()), Some((0, false)));
        assert!(!peer.handle.disconnect_requested());
    }
    sim.clock.advance(Duration::from_secs(6));
    settle(&mut sim);
    for peer in [&source, &other, &blocks] {
        assert!(inventory(&sim, peer, CommandString::GETDATA, 0).is_empty());
    }
}

#[test]
fn mempool_clsig_is_sent_even_when_the_peer_requests_no_transaction_relay() {
    let mut sim = sim(Some(lock()), true, false);
    let peer = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
    sim.handshake(
        &peer,
        PeerVersionMsg {
            relay: false,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    settle(&mut sim);
    sim.sent(&peer);
    assert_eq!(
        sim.shared.with_state(peer.handle.id(), |st| (st.tx_relay, st.relay_txs)),
        Some((true, false))
    );
    request_mempool(&mut sim, &peer);
    next_trickle(&mut sim, &peer);
    assert_eq!(inventory(&sim, &peer, CommandString::INV, 1), [lock_inv()]);
}
