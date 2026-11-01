//! The dial `sim` ladder scenarios (P2a §6.7, §3.8.2): SB6's dial loops driven as the node's
//! threads drive them, under the virtual clock, with in-memory connections.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use dash_p2p::{ConnectionType, DialLoop, NetAddr, Network, Service, Target};
use dash_params::{ChainParamsBuilder, ChainSetup, Network as CoreNetwork, Release};
use dashd_net::sim::{DialEvent, DialSim, SIM_START, SimPolicy, full_node};

fn params(net: CoreNetwork) -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(net, Release::V24_0).build().unwrap())
}

/// A source for AddrMan entries (one netgroup, so every address goes in).
fn source() -> NetAddr {
    NetAddr::ipv4([250, 1, 1, 1])
}

fn ipv4(a: u8, b: u8, c: u8, d: u8, port: u16) -> Service {
    Service::new(NetAddr::ipv4([a, b, c, d]), port)
}

/// `n` addresses on `port`, each in its own /16 (`11.n.0.1`, …).
fn distinct_groups(n: u8, port: u16) -> Vec<dash_p2p::Address> {
    (0..n).map(|i| full_node(ipv4(11, i, 0, 1, port), SIM_START)).collect()
}

/// A mainnet core without seeds, so only AddrMan feeds the ladder.
fn mainnet() -> DialSim {
    DialSim::new(params(CoreNetwork::Main), |cfg| {
        cfg.dns_seed = false;
        cfg.fixed_seeds = false;
    })
}

fn group(core: &dash_p2p::NetCore, s: &Service) -> Vec<u8> {
    core.netgroup().group(&s.addr)
}

fn opened(events: &[DialEvent], conn_type: ConnectionType) -> Vec<Service> {
    events
        .iter()
        .filter_map(|e| match e {
            DialEvent::Opened {
                service,
                conn_type: t,
                ..
            } if *t == conn_type => Some(service.clone()),
            _ => None,
        })
        .collect()
}

/// Slot limits: eight full-relay connections, then two block-relay ones, each to its own
/// netgroup; the eleventh slot only ever carries feelers.
#[test]
fn slot_limits() {
    let mut sim = mainnet();
    let flags = SimPolicy::default();
    sim.add_addrs(&distinct_groups(60, 9999), &source());
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(30 * 60));

    let types: Vec<ConnectionType> = sim
        .exec
        .events
        .iter()
        .filter_map(|e| match e {
            DialEvent::Opened {
                conn_type,
                ..
            } if *conn_type != ConnectionType::Feeler => Some(*conn_type),
            _ => None,
        })
        .collect();
    let mut want = vec![ConnectionType::OutboundFullRelay; 8];
    want.extend([ConnectionType::BlockRelay; 2]);
    assert_eq!(types, want, "full-relay slots fill first, then block-relay");
    assert_eq!(sim.of_type(ConnectionType::OutboundFullRelay).len(), 8);
    assert_eq!(sim.of_type(ConnectionType::BlockRelay).len(), 2);
    let feelers = opened(&sim.exec.events, ConnectionType::Feeler);
    // Exp(2 min) over ~30 min, so about 15; the bounds only rule out "none" and "every pass".
    assert!((3..60).contains(&feelers.len()), "{} feelers", feelers.len());
    let groups: BTreeSet<_> = sim
        .core
        .peers()
        .iter()
        .filter(|p| p.conn_type() != ConnectionType::Feeler)
        .map(|p| group(&sim.core, &p.addr().service))
        .collect();
    assert_eq!(groups.len(), 10, "one connection per netgroup");
}

/// The netgroup rule: with addresses in only three /16s, three automatic connections open and
/// the ladder gives up on the rest (100 tries, then the next iteration).
#[test]
fn netgroup_rule() {
    let mut sim = mainnet();
    let flags = SimPolicy::default();
    let addrs: Vec<_> = (0..3u8)
        .flat_map(|g| (1..=40u8).map(move |h| full_node(ipv4(21 + g, 7, h, h, 9999), SIM_START)))
        .collect();
    sim.add_addrs(&addrs, &source());
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(5));

    let auto: Vec<_> = sim
        .core
        .peers()
        .iter()
        .filter(|p| p.conn_type().is_outbound_or_block_relay())
        .map(|p| group(&sim.core, &p.addr().service))
        .collect();
    assert_eq!(auto.len(), 3, "one per netgroup: {auto:?}");
    assert_eq!(auto.iter().collect::<BTreeSet<_>>().len(), 3);
}

/// One feeler: with every other slot taken and the feeler left open, exactly one feeler opens
/// and the loop then waits for a slot.
#[test]
fn one_feeler() {
    let mut sim = mainnet();
    let flags = SimPolicy::default();
    sim.add_addrs(&distinct_groups(60, 9999), &source());
    sim.exec.close_feelers = false;
    let blocked = sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(60 * 60));
    assert!(blocked, "the loop waits for the eleventh slot");
    assert_eq!(opened(&sim.exec.events, ConnectionType::Feeler).len(), 1);
    assert_eq!(sim.of_type(ConnectionType::Feeler).len(), 1);
    assert_eq!(sim.core.peers().len(), 11);

    // Closing the feeler frees the slot; the next feeler waits for its own timer.
    let feeler = sim.of_type(ConnectionType::Feeler)[0].id();
    sim.close(feeler);
    sim.exec.events.clear();
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(60 * 60));
    assert_eq!(opened(&sim.exec.events, ConnectionType::Feeler).len(), 1);
    let before_feeler: Vec<_> =
        sim.exec.events.iter().take_while(|e| !matches!(e, DialEvent::Opened { .. })).collect();
    assert!(before_feeler.iter().any(|e| matches!(e, DialEvent::Slept(_))));
}

/// The stale-tip flag and extra block-relay peers each add one connection above the limits.
#[test]
fn extra_peers() {
    let mut sim = mainnet();
    let flags = SimPolicy::default();
    sim.add_addrs(&distinct_groups(60, 9999), &source());
    sim.exec.close_feelers = true;
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(10));
    assert_eq!(sim.core.peers().len(), 10);

    flags.try_another.store(true, Ordering::SeqCst);
    sim.run(DialLoop::OpenCon, &flags, 1000);
    assert_eq!(sim.of_type(ConnectionType::OutboundFullRelay).len(), 9, "one extra full-relay");
    flags.try_another.store(false, Ordering::SeqCst);

    // The eleventh slot is taken, so an extra block-relay peer needs the full-relay extra gone.
    let extra = sim.of_type(ConnectionType::OutboundFullRelay)[8].id();
    sim.close(extra);
    flags.extra_block_relay.store(true, Ordering::SeqCst);
    sim.exec.close_feelers = false;
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(60 * 60));
    assert_eq!(
        sim.of_type(ConnectionType::BlockRelay).len() + sim.of_type(ConnectionType::Feeler).len(),
        3,
        "the free slot went to an extra block-relay peer or a feeler"
    );
    assert!(!sim.of_type(ConnectionType::BlockRelay).is_empty());
}

/// Regtest churn (PQ-12, core m-12): `AllowMultipleAddressesFromGroup` skips the accounting, so
/// every one of the eleven slots opens a full-relay connection, with no block-relay peers and
/// no feelers; a closed connection's slot is refilled the same way.
#[test]
fn regtest_churn() {
    let mut sim = DialSim::new(params(CoreNetwork::Regtest), |cfg| {
        cfg.dns_seed = false;
        cfg.fixed_seeds = false;
    });
    let flags = SimPolicy::default();
    let port = sim.core.config().params.default_port;
    let addrs: Vec<_> = (1..=30u8).map(|h| full_node(ipv4(31, 1, 1, h, port), SIM_START)).collect();
    sim.add_addrs(&addrs, &source());
    assert!(sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(60 * 60)));
    assert_eq!(sim.of_type(ConnectionType::OutboundFullRelay).len(), 11);
    assert_eq!(sim.core.peers().len(), 11, "no block-relay, feeler or extra peers");

    for _ in 0..3 {
        let first = sim.core.peers()[0].id();
        sim.close(first);
        sim.exec.events.clear();
        assert!(sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(60)));
        assert_eq!(opened(&sim.exec.events, ConnectionType::OutboundFullRelay).len(), 1);
        assert_eq!(sim.of_type(ConnectionType::OutboundFullRelay).len(), 11);
    }
}

/// `-connect`: every entry is dialled as MANUAL without a slot, each pass retries only the
/// missing ones, and no automatic connection is made.
#[test]
fn connect_only() {
    let mut sim = DialSim::new(params(CoreNetwork::Main), |cfg| {
        cfg.connect = Some(vec!["10.1.1.1:9999".into(), "10.1.1.2:9999".into()]);
        // Named targets dial V2 under the node's services; no V1 retry (SB6's), so every
        // MANUAL open here comes from the loop under test.
        cfg.v1_retry = false;
        cfg.dns_seed = false;
    });
    sim.add_addrs(&distinct_groups(20, 9999), &source());
    let flags = SimPolicy::default();
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(10));
    assert_eq!(opened(&sim.exec.events, ConnectionType::Manual).len(), 2);
    assert_eq!(sim.core.peers().len(), 2);
    assert!(
        sim.exec.events.iter().any(|e| matches!(
            e,
            DialEvent::Refused {
                err: dash_p2p::OpenError::AlreadyConnected,
                ..
            }
        )),
        "later passes find them connected"
    );
    let gone = sim.core.peers()[1].id();
    sim.close(gone);
    sim.exec.events.clear();
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(10));
    assert_eq!(opened(&sim.exec.events, ConnectionType::Manual), [ipv4(10, 1, 1, 2, 9999)]);
}

/// A `-connect` entry holding a NUL dials the name before it: Core passes `c_str()`.
// Core: net.cpp:2933
#[test]
fn connect_name_cut_at_nul() {
    let mut sim = DialSim::new(params(CoreNetwork::Main), |cfg| {
        cfg.connect = Some(vec!["10.1.1.1:9999\0junk".into()]);
        cfg.v1_retry = false;
        cfg.dns_seed = false;
    });
    sim.run_for(DialLoop::OpenCon, &SimPolicy::default(), Duration::from_secs(10));
    assert_eq!(opened(&sim.exec.events, ConnectionType::Manual), [ipv4(10, 1, 1, 1, 9999)]);
}

/// `-seednode`: an ADDR_FETCH connection on the first iteration, holding an outbound slot.
#[test]
fn seednode_addr_fetch() {
    let mut sim = DialSim::new(params(CoreNetwork::Main), |cfg| {
        cfg.seed_nodes = vec!["10.2.2.2:9999".into()];
        cfg.dns_seed = false;
        cfg.fixed_seeds = false;
    });
    let flags = SimPolicy::default();
    sim.run(DialLoop::OpenCon, &flags, 1);
    assert_eq!(opened(&sim.exec.events, ConnectionType::AddrFetch), [ipv4(10, 2, 2, 2, 9999)]);
}

/// `-addnode`: the `addcon` loop dials each unconnected entry as MANUAL from its own slots,
/// waits 60 s after a pass that tried something (2 s otherwise), and redials after a close.
#[test]
fn addnode_loop() {
    let mut sim = DialSim::new(params(CoreNetwork::Main), |cfg| {
        cfg.added_nodes = vec!["10.3.3.3:9999".into(), "10.3.3.4:9999".into()];
        // Named targets dial V2 under the node's services; no V1 retry (SB6's), so every
        // MANUAL open here comes from the loop under test.
        cfg.v1_retry = false;
        cfg.dns_seed = false;
    });
    let flags = SimPolicy::default();
    sim.run_for(DialLoop::AddCon, &flags, Duration::from_secs(61));
    assert_eq!(opened(&sim.exec.events, ConnectionType::Manual).len(), 2);
    assert!(sim.exec.events.contains(&DialEvent::Slept(Duration::from_secs(60))));
    sim.exec.events.clear();
    sim.run_for(DialLoop::AddCon, &flags, Duration::from_secs(120));
    assert!(opened(&sim.exec.events, ConnectionType::Manual).is_empty());
    assert!(sim.exec.events.contains(&DialEvent::Slept(Duration::from_secs(2))));

    let id = sim.core.peers()[0].id();
    sim.close(id);
    sim.exec.events.clear();
    sim.run_for(DialLoop::AddCon, &flags, Duration::from_secs(5));
    assert_eq!(opened(&sim.exec.events, ConnectionType::Manual), [ipv4(10, 3, 3, 3, 9999)]);
}

/// DNS seeding with an empty AddrMan: the seed is queried at once (`x1.<seed>`), its answers
/// enter AddrMan, and the loop ends; an empty answer queues the seed as ADDR_FETCH.
#[test]
fn dns_seed() {
    let mut sim = DialSim::new(params(CoreNetwork::Main), |_| {});
    let seed = sim.core.config().params.dns_seeds[0].to_owned();
    let answers: Vec<NetAddr> = (1..=5u8).map(|i| NetAddr::ipv4([41, i, 0, 1])).collect();
    sim.resolver.0.lock().insert(format!("x1.{seed}"), answers);
    let flags = SimPolicy::default();
    sim.run(DialLoop::DnsSeed, &flags, 100);
    assert!(sim.exec.events.contains(&DialEvent::Resolved {
        seed: seed.clone(),
        found: 5
    }));
    assert_eq!(sim.core.addrman().size(None, None), 5);

    let mut sim = DialSim::new(params(CoreNetwork::Main), |cfg| cfg.fixed_seeds = false);
    sim.run(DialLoop::DnsSeed, &flags, 100);
    sim.run(DialLoop::OpenCon, &flags, 1);
    assert!(sim.exec.events.iter().any(|e| matches!(
        e,
        DialEvent::Refused { target: Target::Dest(d), conn_type: ConnectionType::AddrFetch, .. } if *d == seed
    )));
}

/// Fixed seeds: added at the first slot when `-dnsseed=0` and neither `-addnode` nor
/// `-seednode` is given, or once 60 s have passed with an empty reachable network; only the
/// reachable networks' entries (mainnet: the 170 IPv4 ones, not the onion ones).
#[test]
fn fixed_seeds() {
    let flags = SimPolicy::default();
    let mut sim = DialSim::new(params(CoreNetwork::Main), |cfg| cfg.dns_seed = false);
    sim.run(DialLoop::OpenCon, &flags, 3);
    // 170 are added; a few collide in AddrMan's new table.
    let added = sim.core.addrman().size(None, None);
    assert!(added > 150, "{added}");
    assert_eq!(sim.core.addrman().size(Some(Network::Ipv4), None), added);
    assert!(!opened(&sim.exec.events, ConnectionType::OutboundFullRelay).is_empty());

    let mut sim = DialSim::new(params(CoreNetwork::Main), |_| {});
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(59));
    assert_eq!(sim.core.addrman().size(None, None), 0, "not before 60 s");
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(3));
    assert_eq!(sim.core.addrman().size(None, None), added, "the same table, deterministically");
}

/// `setnetworkactive false`: no connection opens; the loop keeps ticking.
#[test]
fn network_inactive() {
    let mut sim = mainnet();
    sim.add_addrs(&distinct_groups(20, 9999), &source());
    sim.core.set_network_active(false);
    let flags = SimPolicy::default();
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(10));
    assert!(sim.core.peers().is_empty());
    assert!(sim.exec.events.iter().any(|e| matches!(
        e,
        DialEvent::Refused {
            err: dash_p2p::OpenError::Interrupted,
            ..
        }
    )));
    sim.core.set_network_active(true);
    sim.run_for(DialLoop::OpenCon, &flags, Duration::from_secs(10));
    assert_eq!(sim.core.peers().len(), 10);
}
