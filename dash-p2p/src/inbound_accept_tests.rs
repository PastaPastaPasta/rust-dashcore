//! `accept` and `eviction_candidates` against a live table (SB6 review M2): Core's
//! `CreateNodeFromAcceptedSocket` limits and `GetEvictionCandidates` protections.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use dash_p2p_core::ServiceFlags;

use crate::conn::{ConnectionType, PeerHandle, Permissions};
use crate::dial::loop_tests::{Net, T0, regtest};
use crate::netaddr::{Address, NetAddr, Network, Service, SubNet};
use crate::netcore::NetConfig;

fn addr(octets: [u8; 4]) -> Address {
    Address {
        service: Service::new(NetAddr::ipv4(octets), 40000),
        services: ServiceFlags::NODE_NETWORK,
        time: 0,
    }
}

/// A core with `max_inbound` inbound slots: Core's `nMaxConnections - m_max_outbound`, the
/// outbound being 8 + 2 + 1.
fn inbound_slots(max_inbound: usize, f: impl FnOnce(&mut NetConfig)) -> Net {
    Net::new(regtest(), |cfg| {
        cfg.max_connections = 11 + max_inbound;
        f(cfg);
    })
}

/// Accepts a connection from `a`, the host's permissions `NONE` unless given.
fn accept_as(
    net: &Net,
    a: Address,
    permissions: Permissions,
    unsynced: bool,
) -> Option<PeerHandle> {
    net.core.accept(net.sock(), a, Service::default(), permissions, false, unsynced)
}

fn accept(net: &Net, a: Address) -> Option<PeerHandle> {
    accept_as(net, a, Permissions::NONE, false)
}

/// `n` inbound peers from distinct /16s, each accepted.
fn fill(net: &Net, n: u8) -> Vec<PeerHandle> {
    (0..n).map(|i| accept(net, addr([20 + i, 1, 1, 1])).unwrap()).collect()
}

fn marked(peers: &[PeerHandle]) -> usize {
    peers.iter().filter(|p| p.disconnect_requested()).count()
}

/// The inbound limit (net.cpp:1950-1958): with every slot held, one inbound is evicted per
/// accept; with too few candidates for `SelectNodeToEvict`, the new connection is dropped.
#[tokio::test]
async fn accept_evicts_down_to_the_limit() {
    let net = inbound_slots(40, |_| {});
    let peers = fill(&net, 40);
    assert_eq!(marked(&peers), 0);
    let p = accept(&net, addr([90, 1, 1, 1])).unwrap();
    assert_eq!(p.conn_type(), ConnectionType::Inbound);
    assert_eq!(marked(&peers), 1);
    // The marked peer still counts until it is removed (`m_nodes`), so the next accept, with
    // 41 inbound counted, evicts two more.
    accept(&net, addr([91, 1, 1, 1])).unwrap();
    assert_eq!(marked(&net.core.peers()), 3);

    let net = inbound_slots(3, |_| {});
    let peers = fill(&net, 3);
    assert!(accept(&net, addr([90, 1, 1, 1])).is_none(), "full, no candidate");
    assert_eq!(marked(&peers), 0);
    assert_eq!(net.core.peers().len(), 3);
}

/// Only inbound peers count toward the inbound limit (net.cpp:1895-1904): with every kind of
/// outbound peer attached, the one inbound slot is still free, and taken by the first accept.
/// Below 11 connections Core's `nMaxInbound` is negative, so nothing is admitted.
#[tokio::test]
async fn accept_counts_only_inbound_peers() {
    let net = inbound_slots(1, |_| {});
    let outbound = [
        ConnectionType::OutboundFullRelay,
        ConnectionType::BlockRelay,
        ConnectionType::Feeler,
        ConnectionType::Manual,
    ];
    for (i, conn_type) in outbound.into_iter().enumerate() {
        net.attach(addr([50 + i as u8, 1, 1, 1]), conn_type);
    }
    let p = accept(&net, addr([90, 1, 1, 1])).expect("the inbound slot is free");
    assert_eq!(marked(&net.core.peers()), 0);
    assert!(accept(&net, addr([91, 1, 1, 1])).is_none(), "full, no candidate");
    assert!(!p.disconnect_requested());

    let net = Net::new(regtest(), |cfg| cfg.max_connections = 5);
    assert!(accept(&net, addr([90, 1, 1, 1])).is_none());
}

/// Discouraged peers are refused only when `nInbound + 1 >= nMaxInbound` (net.cpp:1942), and
/// never with `NoBan`; accepted ones are preferred for eviction. Banned ones are always refused
/// without `NoBan`, and nothing is accepted with the network inactive.
#[tokio::test]
async fn accept_refuses_discouraged_when_nearly_full() {
    let bad = addr([66, 1, 1, 1]);
    for (held, refused) in [(0u8, false), (1, false), (2, true)] {
        let net = inbound_slots(3, |_| {});
        net.core.banlist().discourage(&bad.service.addr);
        fill(&net, held);
        let p = accept(&net, bad.clone());
        assert_eq!(p.is_none(), refused, "{held} held");
        assert!(p.is_none_or(|p| p.prefer_evict()));
        let noban = accept_as(&net, bad.clone(), Permissions::NO_BAN, false);
        assert!(noban.unwrap().prefer_evict(), "{held} held, noban");
    }

    let net = inbound_slots(3, |_| {});
    net.core.ban(SubNet::single(bad.service.addr.clone()), 3600, false);
    assert!(accept(&net, bad.clone()).is_none());
    assert!(accept_as(&net, bad.clone(), Permissions::NO_BAN, false).is_some());
    net.core.set_network_active(false);
    assert!(accept(&net, addr([1, 2, 3, 4])).is_none());
}

/// Verified masternodes are not counted against the inbound limit (net.cpp:1950).
#[tokio::test]
async fn accept_does_not_count_verified_masternodes() {
    for verified in [false, true] {
        let net = inbound_slots(3, |_| {});
        let peers = fill(&net, 3);
        if verified {
            peers[0].set_verified_proreg_hash([7; 32]);
        }
        assert_eq!(accept(&net, addr([90, 1, 1, 1])).is_some(), verified);
        assert_eq!(marked(&peers), 0);
    }
}

/// `m_active_masternode && !IsBlockchainSynced()` refuses after the eviction loop (net.cpp:
/// 1960-1964): a full table still loses a peer.
#[tokio::test]
async fn accept_refuses_unsynced_after_evicting() {
    let net = inbound_slots(40, |_| {});
    let peers = fill(&net, 40);
    assert!(accept_as(&net, addr([90, 1, 1, 1]), Permissions::NONE, true).is_none());
    assert_eq!(marked(&peers), 1);
    assert_eq!(net.core.peers().len(), 40);

    let net = inbound_slots(40, |_| {});
    let peers = fill(&net, 2);
    assert!(accept_as(&net, addr([90, 1, 1, 1]), Permissions::NONE, true).is_none());
    assert_eq!(marked(&peers), 0);
}

/// `-whitelist` and the implicit flags (net.cpp:1885-1891): `Implicit` becomes `Mempool`,
/// `NoBan`, `Relay` with `-whitelistrelay` and `ForceRelay` with `-whitelistforcerelay`, from a
/// matching `-whitelist` subnet or from the host (a `-whitebind`).
#[tokio::test]
async fn accept_implicit_whitelist_flags() {
    let listed = SubNet::with_prefix(NetAddr::ipv4([1, 2, 0, 0]), 16);
    let implicit = |relay: bool, force: bool| {
        inbound_slots(10, |cfg| {
            cfg.whitelist = vec![(listed.clone(), Permissions::IMPLICIT)];
            cfg.whitelist_relay = relay;
            cfg.whitelist_force_relay = force;
        })
    };
    let base = Permissions::MEMPOOL.0 | Permissions::NO_BAN.0;

    let net = implicit(true, false);
    let p = accept(&net, addr([1, 2, 3, 4])).unwrap();
    assert_eq!(p.permissions().0, base | Permissions::RELAY.0);
    assert_eq!(accept(&net, addr([1, 3, 3, 4])).unwrap().permissions(), Permissions::NONE);
    let host = accept_as(&net, addr([1, 3, 3, 5]), Permissions::IMPLICIT, false).unwrap();
    assert_eq!(host.permissions().0, base | Permissions::RELAY.0);

    let net = implicit(false, true);
    let p = accept(&net, addr([1, 2, 3, 4])).unwrap();
    assert_eq!(p.permissions().0, base | Permissions::FORCE_RELAY.0);

    let net = implicit(false, false);
    let p = accept(&net, addr([1, 2, 3, 4])).unwrap();
    assert_eq!(p.permissions().0, base);
    assert!(!p.has_permission(Permissions::IMPLICIT));
}

/// An inbound peer speaks V2 iff we advertise `NODE_P2P_V2` (net.cpp:1980-1985).
#[tokio::test]
async fn accept_v2_iff_advertised() {
    for v2 in [false, true] {
        let net = inbound_slots(10, |cfg| {
            if v2 {
                cfg.local_services =
                    ServiceFlags(cfg.local_services.0 | ServiceFlags::NODE_P2P_V2.0);
            }
        });
        assert_eq!(accept(&net, addr([1, 2, 3, 4])).unwrap().use_v2(), v2);
    }
}

fn ids(net: &Net) -> Vec<i64> {
    net.core.eviction_candidates().iter().map(|c| c.id).collect()
}

/// `GetEvictionCandidates` (net.cpp:1775-1820) without an active masternode: every peer not
/// being disconnected, with `m_is_local` for loopback and the network it connected through.
#[tokio::test]
async fn eviction_candidates_fields() {
    let net = inbound_slots(10, |_| {});
    let local = accept(&net, addr([127, 0, 0, 1])).unwrap();
    let public = accept(&net, addr([1, 2, 3, 4])).unwrap();
    let onion = net
        .core
        .accept(
            net.sock(),
            addr([127, 0, 0, 2]),
            Service::default(),
            Permissions::NONE,
            true,
            false,
        )
        .unwrap();
    let gone = accept(&net, addr([5, 6, 7, 8])).unwrap();
    gone.mark_disconnect();
    let c = net.core.eviction_candidates();
    assert_eq!(ids(&net), [local.id(), public.id(), onion.id()]);
    assert!(c[0].is_local && !c[1].is_local && c[2].is_local);
    assert_eq!(
        c.iter().map(|c| c.network).collect::<Vec<_>>(),
        [Network::Unroutable, Network::Ipv4, Network::Onion]
    );
    assert_eq!(c[1].connected, T0);
}

/// With an active masternode (net.cpp:1784-1800): a peer is protected for its first second
/// unless its first message was not `mnauth`, and a verified masternode always.
#[tokio::test]
async fn eviction_candidates_masternode_protections() {
    let net = inbound_slots(10, |cfg| cfg.active_masternode = true);
    let fresh = accept(&net, addr([1, 1, 1, 1])).unwrap();
    let chatty = accept(&net, addr([2, 1, 1, 1])).unwrap();
    chatty.set_first_message(T0, false);
    let mnauth = accept(&net, addr([3, 1, 1, 1])).unwrap();
    mnauth.set_first_message(T0, true);
    let verified = accept(&net, addr([4, 1, 1, 1])).unwrap();
    verified.set_first_message(T0, false);
    verified.set_verified_proreg_hash([7; 32]);
    assert_eq!(ids(&net), [chatty.id()]);

    net.clock.set_mocktime(T0 + 1);
    assert_eq!(ids(&net), [fresh.id(), chatty.id(), mnauth.id()]);
    net.clock.set_mocktime(T0 + 100_000);
    assert_eq!(ids(&net), [fresh.id(), chatty.id(), mnauth.id()]);

    // Without an active masternode none of this applies.
    let net = inbound_slots(10, |_| {});
    let fresh = accept(&net, addr([1, 1, 1, 1])).unwrap();
    let verified = accept(&net, addr([4, 1, 1, 1])).unwrap();
    verified.set_verified_proreg_hash([7; 32]);
    assert_eq!(ids(&net), [fresh.id(), verified.id()]);
}

/// Up to `n` callers meeting: each waits until all `n` have arrived or `patience` passes.
struct Rendezvous {
    arrived: Mutex<usize>,
    all: Condvar,
    n: usize,
    patience: Duration,
}

impl Rendezvous {
    fn wait(&self) {
        let mut arrived = self.arrived.lock().unwrap();
        *arrived += 1;
        self.all.notify_all();
        let _ = self.all.wait_timeout_while(arrived, self.patience, |a| *a < self.n).unwrap();
    }
}

/// BL-327 D20: two listener tasks accept at once for the last inbound slot. Each `accept`
/// that reaches the attach waits there for the other (up to 2 s). Without the admission lock
/// both have counted no inbound peer by then and both attach, one past the limit. With it, the
/// second counts the first, finds no eviction candidate and drops, as Core's one socket thread
/// decides them in turn.
// Core: net.cpp:1881-1883, :1948-1961, :2009-2012, :2579-2589
#[tokio::test]
async fn concurrent_accepts_take_one_slot() {
    let net = inbound_slots(1, |_| {});
    let meet = Arc::new(Rendezvous {
        arrived: Mutex::new(0),
        all: Condvar::new(),
        n: 2,
        patience: Duration::from_secs(2),
    });
    let hook = Arc::clone(&meet);
    assert!(net.core.admission_hook.set(Box::new(move || hook.wait())).is_ok());
    let admitted = std::thread::scope(|s| {
        let a = s.spawn(|| accept(&net, addr([20, 1, 1, 1])).is_some());
        let b = s.spawn(|| accept(&net, addr([21, 1, 1, 1])).is_some());
        [a.join().unwrap(), b.join().unwrap()]
    });
    let inbound = net.core.peers().iter().filter(|p| p.is_inbound()).count();
    assert_eq!((admitted.iter().filter(|a| **a).count(), inbound), (1, 1), "{admitted:?}");
    assert_eq!(*meet.arrived.lock().unwrap(), 1, "only the admitted accept reaches the attach");
}
