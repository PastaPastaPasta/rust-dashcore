//! Address relay in P2a-core's `sim` with `NodeAddressRelay` installed (P2a §3.6.4–3.6.6):
//! `RelayAddress` target choice, `IsAddrCompatible`, `MaybeSendAddr`'s self-advertisement, and
//! the `getaddr` reply with its response cache.

use std::sync::Arc;
use std::time::Duration;

use dash_clock::Clock;
use dash_p2p::conn::Permissions;
use dash_p2p::{Address, ConnectionType, NetAddr, Service, ServiceFlags};
use dash_p2p_core::{Addr, AddrV2Entry, AddrV2Msg, TimestampedAddr};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{AddrV1, AddrV2, ServiceV1};
use dash_types::codec::BaseCodec;
use dashd_net::NodeAddressRelay;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer};
use dashd_p2p::{Registry, Seams};

fn params() -> Arc<ChainParams> {
    Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap())
}

/// A node with the relay installed; `relay_chain` answers its `IsInitialBlockDownload()`.
fn sim_with(relay_chain: &Arc<SimChain>, hook: impl FnOnce(&mut dash_p2p::NetConfig)) -> Sim {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let seams = Seams {
        addr_relay: Box::new(NodeAddressRelay::new(relay_chain.clone())),
        ..Seams::default()
    };
    Sim::with(p, id, ProcessorOptions::default(), Registry::new(), seams, hook)
}

fn sim() -> (Sim, Arc<SimChain>) {
    let chain = SimChain::new(params());
    (sim_with(&chain, |_| {}), chain)
}

/// A handshaken peer at 10.0.0.`n`.
fn peer(sim: &mut Sim, n: u8, conn_type: ConnectionType, perms: Permissions) -> SimPeer {
    let p = sim.connect(n, conn_type, perms);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    p
}

fn routable(last: u8) -> Service {
    Service::new(NetAddr::ipv4([1, 2, 3, last]), 9999)
}

fn addr_payload(services: &[Service], time: i64) -> Vec<u8> {
    let mut buf = Vec::new();
    Addr {
        addrs: services
            .iter()
            .map(|s| TimestampedAddr {
                time: time as u32,
                services: ServiceFlags::NODE_NETWORK,
                addr: ServiceV1 {
                    addr: AddrV1(s.addr.v1_bytes()),
                    port: s.port,
                },
            })
            .collect(),
    }
    .encode(&mut buf);
    buf
}

/// The addresses in every `addr`/`addrv2` the node sent `p` since the last read.
fn received(sim: &Sim, p: &SimPeer) -> Vec<(String, Address)> {
    let mut out = Vec::new();
    for (cmd, payload) in sim.sent(p) {
        let mut data = &payload[..];
        match cmd.as_str() {
            "addr" => {
                for e in Addr::decode(&mut data).unwrap().addrs {
                    out.push((
                        "addr".to_owned(),
                        Address {
                            service: Service::from_v1(&e.addr),
                            services: e.services,
                            time: e.time,
                        },
                    ));
                }
            }
            "addrv2" => {
                for e in AddrV2Msg::decode(&mut data).unwrap().addrs {
                    out.push((
                        "addrv2".to_owned(),
                        Address {
                            service: Service::new(NetAddr::from_addrv2(&e.addr), e.port),
                            services: e.services,
                            time: e.time,
                        },
                    ));
                }
            }
            _ => {}
        }
    }
    out
}

/// The node's current time in seconds.
fn now(sim: &Sim) -> i64 {
    sim.clock.node_now().0
}

/// Lets the Exp(30 s) addr timers expire, then runs a pass.
fn later(sim: &mut Sim) {
    sim.clock.advance(Duration::from_secs(600));
    sim.pass();
}

/// A fresh, routable, reachable address from an inbound peer goes to exactly two of the other
/// relay peers, never back to its source.
#[test]
fn reachable_address_goes_to_two_peers() {
    let (mut sim, _chain) = sim();
    let a = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE);
    let others: Vec<SimPeer> = (2..=5)
        .map(|n| peer(&mut sim, n, ConnectionType::OutboundFullRelay, Permissions::NONE))
        .collect();
    later(&mut sim);
    for p in &others {
        received(&sim, p);
    }
    let x = routable(7);
    sim.deliver(&a, &[("addr", addr_payload(std::slice::from_ref(&x), now(&sim)))]);
    sim.pass();
    later(&mut sim);

    let got: Vec<bool> =
        others.iter().map(|p| received(&sim, p).iter().any(|(_, ad)| ad.service == x)).collect();
    assert_eq!(got.iter().filter(|g| **g).count(), 2, "{got:?}");
    assert!(received(&sim, &a).is_empty(), "never back to the source");
}

/// A block-relay-only peer never takes part, and a peer that has not set up relay is never
/// chosen.
#[test]
fn only_relay_peers_are_targets() {
    let (mut sim, _chain) = sim();
    let a = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE);
    let block = peer(&mut sim, 2, ConnectionType::BlockRelay, Permissions::NONE);
    let quiet_inbound = peer(&mut sim, 3, ConnectionType::Inbound, Permissions::NONE);
    let full = peer(&mut sim, 4, ConnectionType::OutboundFullRelay, Permissions::NONE);
    later(&mut sim);
    received(&sim, &full);
    sim.deliver(&a, &[("addr", addr_payload(&[routable(8)], now(&sim)))]);
    sim.pass();
    later(&mut sim);
    assert!(received(&sim, &block).is_empty());
    assert!(received(&sim, &quiet_inbound).is_empty(), "inbound without addr or getaddr");
    assert_eq!(received(&sim, &full).len(), 1);
}

/// `IsAddrCompatible`: a Tor v3 address is relayed only to a peer that sent `sendaddrv2`, and
/// leaves as `addrv2`.
#[test]
fn bip155_addresses_only_to_addrv2_peers() {
    let (mut sim, _chain) = sim();
    let a = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
    sim.deliver(&a, &[("version", PeerVersionMsg::v24(SIM_START).payload())]);
    sim.pass();
    sim.deliver(&a, &[("sendaddrv2", Vec::new()), ("verack", Vec::new())]);
    sim.pass();
    let v2 = sim.connect(2, ConnectionType::OutboundFullRelay, Permissions::NONE);
    sim.deliver(&v2, &[("version", PeerVersionMsg::v24(SIM_START).payload())]);
    sim.pass();
    sim.deliver(&v2, &[("sendaddrv2", Vec::new()), ("verack", Vec::new())]);
    sim.pass();
    let v1 = peer(&mut sim, 3, ConnectionType::OutboundFullRelay, Permissions::NONE);
    later(&mut sim);
    received(&sim, &v2);
    received(&sim, &v1);

    let onion = AddrV2::TorV3([7; 32]);
    let mut payload = Vec::new();
    AddrV2Msg {
        addrs: vec![AddrV2Entry {
            time: now(&sim) as u32,
            services: ServiceFlags::NODE_NETWORK,
            addr: onion.clone(),
            port: 9999,
        }],
    }
    .encode(&mut payload);
    sim.deliver(&a, &[("addrv2", payload)]);
    sim.pass();
    later(&mut sim);
    let want = Service::new(NetAddr::from_addrv2(&onion), 9999);
    let got = received(&sim, &v2);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, "addrv2");
    assert_eq!(got[0].1.service, want);
    assert!(received(&sim, &v1).is_empty(), "not v1-compatible");
}

/// `MaybeSendAddr`: a listening node out of IBD advertises its `-externalip` with its services
/// in the first pass after the handshake; in IBD it does not.
#[test]
fn self_advertisement_outside_ibd() {
    let external = Service::new(NetAddr::ipv4([9, 9, 9, 9]), 19899);
    for ibd in [true, false] {
        let chain = SimChain::new(params());
        chain.set_ibd(ibd);
        let mut sim = sim_with(&chain, |cfg| cfg.external_ip = vec![external.clone()]);
        let p = sim.connect(2, ConnectionType::OutboundFullRelay, Permissions::NONE);
        let sent = sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
        let ours: Vec<_> = sent
            .iter()
            .filter(|(c, _)| c.as_str() == "addr")
            .flat_map(|(_, payload)| Addr::decode(&mut &payload[..]).unwrap().addrs)
            .collect();
        if ibd {
            assert!(ours.is_empty());
        } else {
            assert_eq!(ours.len(), 1);
            assert_eq!(Service::from_v1(&ours[0].addr), external);
            assert_eq!(ours[0].services, sim.core.config().local_services);
            assert_eq!(i64::from(ours[0].time), SIM_START);
        }
    }
}

/// `getaddr` from an inbound peer: 23 % of AddrMan, answered once per connection, from the
/// response cache shared by peers on the same network and local socket; a peer with the `addr`
/// permission bypasses the cache.
#[test]
fn getaddr_reply_and_cache() {
    let (mut sim, _chain) = sim();
    let now = SIM_START;
    let batch = |from: u8, n: u8| -> Vec<Address> {
        (0..n)
            .map(|i| Address {
                service: Service::new(NetAddr::ipv4([31 + i, from, 1, 1]), 9999),
                services: ServiceFlags::NODE_NETWORK,
                time: now as u32,
            })
            .collect()
    };
    let src = NetAddr::ipv4([250, 1, 1, 1]);
    sim.core.addrman().add(&batch(1, 100), &src, 0, now);
    let total = sim.core.addrman().size(None, None);

    let a = peer(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE);
    received(&sim, &a);
    sim.deliver(&a, &[("getaddr", Vec::new())]);
    sim.pass();
    let first: Vec<Service> = received(&sim, &a).into_iter().map(|(_, ad)| ad.service).collect();
    assert_eq!(first.len(), total * 23 / 100);

    sim.deliver(&a, &[("getaddr", Vec::new())]);
    sim.pass();
    later(&mut sim);
    assert!(received(&sim, &a).is_empty(), "one reply per connection");

    // More addresses arrive; a second inbound peer on the same network and socket gets the
    // cached reply.
    sim.core.addrman().add(&batch(2, 100), &src, 0, now);
    let b = peer(&mut sim, 2, ConnectionType::Inbound, Permissions::NONE);
    received(&sim, &b);
    sim.deliver(&b, &[("getaddr", Vec::new())]);
    sim.pass();
    let mut second: Vec<Service> =
        received(&sim, &b).into_iter().map(|(_, ad)| ad.service).collect();
    let mut want = first.clone();
    want.sort_by_key(|s| s.to_string());
    second.sort_by_key(|s| s.to_string());
    assert_eq!(second, want);

    let c = peer(&mut sim, 3, ConnectionType::Inbound, Permissions::ADDR);
    received(&sim, &c);
    sim.deliver(&c, &[("getaddr", Vec::new())]);
    sim.pass();
    let fresh = received(&sim, &c);
    assert_eq!(fresh.len(), sim.core.addrman().size(None, None) * 23 / 100);
}

/// An outbound peer's `getaddr` is ignored (P2a-core's branch), so nothing is sent back.
#[test]
fn outbound_getaddr_is_ignored() {
    let (mut sim, _chain) = sim();
    sim.core.addrman().add(
        &[Address {
            service: routable(9),
            services: ServiceFlags::NODE_NETWORK,
            time: SIM_START as u32,
        }],
        &NetAddr::ipv4([250, 1, 1, 1]),
        0,
        SIM_START,
    );
    let p = peer(&mut sim, 1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    later(&mut sim);
    received(&sim, &p);
    sim.deliver(&p, &[("getaddr", Vec::new())]);
    sim.pass();
    later(&mut sim);
    assert!(received(&sim, &p).is_empty());
}
