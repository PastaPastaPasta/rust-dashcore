//! `try_mark_verified` against a live table: Core's `CConnman::TryMarkVerified` cap of
//! `MAX_VERIFIED_INBOUND_PER_PROTX` verified inbound legs per proRegTx hash, and its one-lock
//! count and grant.
// Core: net.cpp:4514-4539, net.h:114

use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use dash_p2p_core::ServiceFlags;

use super::MAX_VERIFIED_INBOUND_PER_PROTX;
use crate::conn::{ConnectionType, PeerHandle};
use crate::dial::loop_tests::{Net, regtest};
use crate::netaddr::{Address, NetAddr, Service};
use crate::{NetCore, Why};

const P: [u8; 32] = [0x11; 32];
const Q: [u8; 32] = [0x33; 32];
const K: [u8; 32] = [0x22; 32];
const K2: [u8; 32] = [0x44; 32];
const ZERO: [u8; 32] = [0; 32];

fn net() -> Net {
    Net::new(regtest(), |_| {})
}

fn attach(net: &Net, i: u8, conn_type: ConnectionType) -> PeerHandle {
    let addr = Address {
        service: Service::new(NetAddr::ipv4([20 + i, 1, 1, 1]), 40000),
        services: ServiceFlags::NODE_NETWORK,
        time: 0,
    };
    net.attach(addr, conn_type)
}

fn inbound(net: &Net, n: u8) -> Vec<PeerHandle> {
    (0..n).map(|i| attach(net, i, ConnectionType::Inbound)).collect()
}

fn hashes(p: &PeerHandle) -> ([u8; 32], [u8; 32]) {
    (p.verified_proreg_hash(), p.verified_pubkey_hash())
}

fn mark(core: &NetCore, p: &PeerHandle, proreg: [u8; 32], pubkey: [u8; 32]) -> bool {
    core.try_mark_verified(p.id(), proreg, pubkey)
}

/// Test 1: three inbound legs of P are marked with both hashes; the fourth is refused and keeps
/// both zero.
#[tokio::test]
async fn the_fourth_verified_inbound_leg_of_a_protx_is_refused() {
    let net = net();
    let legs = inbound(&net, 4);
    for leg in &legs[..3] {
        assert!(mark(&net.core, leg, P, K));
        assert_eq!(hashes(leg), (P, K));
    }
    assert!(!mark(&net.core, &legs[3], P, K));
    assert_eq!(hashes(&legs[3]), (ZERO, ZERO));
}

/// Test 2: a verified leg marked for disconnection is not counted, before the sweep removes it.
#[tokio::test]
async fn a_disconnecting_verified_leg_frees_its_slot() {
    let net = net();
    let legs = inbound(&net, 4);
    for leg in &legs[..3] {
        assert!(mark(&net.core, leg, P, K));
    }
    net.core.disconnect(legs[0].id(), Why::Processor("test"));
    assert!(legs[0].disconnect_requested());
    assert!(net.core.peer(legs[0].id()).is_some(), "still in the table");
    assert!(mark(&net.core, &legs[3], P, K));
    assert_eq!(hashes(&legs[3]), (P, K));
}

/// Test 3: an outbound target is never capped, whatever P's inbound legs hold.
#[tokio::test]
async fn an_outbound_target_is_not_capped() {
    let net = net();
    let legs = inbound(&net, 3);
    for leg in &legs {
        assert!(mark(&net.core, leg, P, K));
    }
    let out = attach(&net, 10, ConnectionType::OutboundFullRelay);
    assert!(mark(&net.core, &out, P, K));
    assert_eq!(hashes(&out), (P, K));
}

/// Test 4: the count is per hash: a fourth inbound leg of another protx Q is marked. A refused
/// grant for P leaves it as it was.
#[tokio::test]
async fn the_cap_counts_one_protx() {
    let net = net();
    let legs = inbound(&net, 4);
    for leg in &legs[..3] {
        assert!(mark(&net.core, leg, P, K));
    }
    assert!(mark(&net.core, &legs[3], Q, K));
    assert_eq!(hashes(&legs[3]), (Q, K));
    assert!(!mark(&net.core, &legs[3], P, K2));
    assert_eq!(hashes(&legs[3]), (Q, K));
}

/// Test 5: the target is not counted: a verified leg with two others re-marks, and a new key
/// replaces its pubkey hash, so both stores run.
#[tokio::test]
async fn a_verified_leg_is_not_counted_against_itself() {
    let net = net();
    let legs = inbound(&net, 3);
    for leg in &legs {
        assert!(mark(&net.core, leg, P, K));
    }
    assert!(mark(&net.core, &legs[2], P, K2));
    assert_eq!(hashes(&legs[2]), (P, K2));
}

/// Test 6: an unknown id is refused; a target marked for disconnection is still found and
/// marked (`fExcludeDisconnecting=false`).
#[tokio::test]
async fn an_unknown_id_is_refused_and_a_disconnecting_target_is_marked() {
    let net = net();
    let leg = attach(&net, 0, ConnectionType::Inbound);
    assert!(!net.core.try_mark_verified(leg.id() + 100, P, K));
    net.core.disconnect(leg.id(), Why::Processor("test"));
    assert!(leg.disconnect_requested());
    assert!(mark(&net.core, &leg, P, K));
    assert_eq!(hashes(&leg), (P, K));
}

/// Test 7a: the grant runs under the table's write lock: inside it, not even a read lock can be
/// taken.
#[tokio::test]
async fn the_grant_holds_the_tables_write_lock() {
    let net = net();
    let legs = inbound(&net, 1);
    let out = attach(&net, 10, ConnectionType::OutboundFullRelay);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (core, s) = (Arc::downgrade(&net.core), Arc::clone(&seen));
    net.core.set_grant_hook(Some(Box::new(move || {
        let core = core.upgrade().expect("the core");
        s.lock().unwrap().push(core.nodes_try_read_for_tests());
    })));
    assert!(net.core.nodes_try_read_for_tests(), "free outside the grant");
    assert!(mark(&net.core, &legs[0], P, K));
    assert!(mark(&net.core, &out, P, K));
    net.core.set_grant_hook(None);
    assert_eq!(*seen.lock().unwrap(), [false, false]);
}

/// Up to two grants meeting: each waits until both have arrived or `patience` passes.
struct Rendezvous {
    arrived: Mutex<usize>,
    all: Condvar,
    patience: Duration,
}

impl Rendezvous {
    fn wait(&self) {
        let mut arrived = self.arrived.lock().unwrap();
        *arrived += 1;
        self.all.notify_all();
        let _ = self.all.wait_timeout_while(arrived, self.patience, |a| *a < 2).unwrap();
    }
}

/// Test 7b: two grants for P race while P holds two verified inbound legs. A grant that passed
/// the count waits in its hook (up to 2 s) for the other. Under the write lock the other cannot
/// count until the first has stored, so it counts three and is refused before its hook: exactly
/// one of X and Y is marked. Grants that counted under a shared lock would both pass and give P
/// four legs.
#[tokio::test]
async fn racing_grants_cannot_both_pass_the_count() {
    let net = net();
    let legs = inbound(&net, 4);
    for leg in &legs[..2] {
        assert!(mark(&net.core, leg, P, K));
    }
    let meet = Arc::new(Rendezvous {
        arrived: Mutex::new(0),
        all: Condvar::new(),
        patience: Duration::from_secs(2),
    });
    let hook = Arc::clone(&meet);
    net.core.set_grant_hook(Some(Box::new(move || hook.wait())));
    let granted = thread::scope(|s| {
        let x = s.spawn(|| mark(&net.core, &legs[2], P, K));
        let y = s.spawn(|| mark(&net.core, &legs[3], P, K));
        [x.join().unwrap(), y.join().unwrap()]
    });
    net.core.set_grant_hook(None);
    assert_eq!(granted.iter().filter(|g| **g).count(), 1, "{granted:?}");
    assert_eq!(*meet.arrived.lock().unwrap(), 1, "only the granted leg reaches the hook");
    let verified = legs.iter().filter(|p| p.verified_proreg_hash() == P).count();
    assert_eq!(verified, MAX_VERIFIED_INBOUND_PER_PROTX);
    for (leg, granted) in legs[2..].iter().zip(granted) {
        let want = if granted {
            (P, K)
        } else {
            (ZERO, ZERO)
        };
        assert_eq!(hashes(leg), want);
    }
}

/// Test 9: outbound legs are not counted: with two verified inbound legs and one verified
/// outbound leg of P, a third inbound leg is marked with both hashes.
#[tokio::test]
async fn verified_outbound_legs_are_not_counted() {
    let net = net();
    let legs = inbound(&net, 3);
    for leg in &legs[..2] {
        assert!(mark(&net.core, leg, P, K));
    }
    let out = attach(&net, 10, ConnectionType::OutboundFullRelay);
    assert!(mark(&net.core, &out, P, K));
    assert!(mark(&net.core, &legs[2], P, K2));
    assert_eq!(hashes(&legs[2]), (P, K2));
}
