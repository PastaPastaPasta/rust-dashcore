//! BL-753: `mnauth`'s `NodeNet::try_mark_verified` through a regtest node's own slot reaches
//! `CConnman::TryMarkVerified`: three inbound legs of one proTxHash are marked, the fourth is
//! refused and keeps both hashes zero, and an absent id, `-1` and both `i64` ends included, is
//! no peer. The cap itself is `dash-p2p`'s (`netcore_verify_tests`).
// Core: rpc/node.cpp:323; net.cpp:4514-4539

use std::sync::Arc;

use dash_p2p::{
    Address, Attach, ConnectionType, MemSocket, NetAddr, PeerHandle, Permissions, Service,
    ServiceFlags,
};
use dashd_rpc::peers::NodeNet;

use super::amb::node;
use crate::rpc::NetSlot;

const PRO_TX: [u8; 32] = [0x11; 32];
const KEY_HASH: [u8; 32] = [0x22; 32];
const ZERO: [u8; 32] = [0; 32];

/// An inbound leg from `20+i.1.1.1`; `far` keeps the other end open.
fn inbound(slot: &NetSlot, i: u8, far: &mut Vec<MemSocket>) -> PeerHandle {
    let (ours, theirs) = MemSocket::pair();
    far.push(theirs);
    slot.core().expect("attached").attach(Attach {
        sock: Arc::new(ours),
        addr: Address {
            service: Service::new(NetAddr::ipv4([20 + i, 1, 1, 1]), 40000),
            services: ServiceFlags::NODE_NETWORK,
            time: 0,
        },
        addr_bind: Service::default(),
        dest: None,
        conn_type: ConnectionType::Inbound,
        inbound_onion: false,
        permissions: Permissions::NONE,
        use_v2: false,
        grant: None,
        masternode: false,
        masternode_probe: false,
    })
}

fn hashes(p: &PeerHandle) -> ([u8; 32], [u8; 32]) {
    (p.verified_proreg_hash(), p.verified_pubkey_hash())
}

#[test]
fn the_slot_marks_three_inbound_legs_and_refuses_the_fourth() {
    let dir = tempfile::tempdir().unwrap();
    let node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0", "-listen=0"]);
    let slot = node.rpc_net();
    let mut far = Vec::new();
    let legs: Vec<_> = (0..4).map(|i| inbound(&slot, i, &mut far)).collect();
    let net: &dyn NodeNet = &*slot;
    for leg in &legs[..3] {
        assert_eq!(net.try_mark_verified(leg.id(), PRO_TX, KEY_HASH), Some(true));
        assert_eq!(hashes(leg), (PRO_TX, KEY_HASH));
    }
    assert_eq!(net.try_mark_verified(legs[3].id(), PRO_TX, KEY_HASH), Some(false));
    assert_eq!(hashes(&legs[3]), (ZERO, ZERO));
    for id in [-1, i64::MIN, i64::MAX] {
        assert!(legs.iter().all(|leg| leg.id() != id));
        assert_eq!(net.try_mark_verified(id, PRO_TX, KEY_HASH), Some(false), "{id}");
    }
    assert_eq!(
        NodeNet::try_mark_verified(&NetSlot::default(), legs[0].id(), PRO_TX, KEY_HASH),
        None
    );
}
