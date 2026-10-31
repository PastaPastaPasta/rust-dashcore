//! Names and CJDNS against Core v24: `SplitHostPort`, `SetSpecial` before any DNS query
//! (`LookupIntern`), `MaybeFlipIPv6toCJDNS` on named, numeric and accepted addresses, CJDNS dials
//! and attempts, and `GetLocalAddress`, including its fallback.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dash_clock::MockClock;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use parking_lot::Mutex;

use crate::dial::AddedNode;
use crate::dial::loop_tests::Silent;
use crate::local::LocalScore;
use crate::netcore::split_host_port;
use crate::seeds::{Lookup, pre_lookup};
use crate::store::NoStore;
use crate::{
    Address, Attach, ConnectionType, MemSocket, NetAddr, NetConfig, NetCore, Network, OpenError,
    OpenRequest, PeerHandle, Permissions, Resolver, Service, ServiceFlags, Target,
};

const NOW: i64 = 1_700_000_000;
// Core: src/test/net_tests.cpp:557, :584
const TORV3: &str = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
const I2P: &str = "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna.b32.i2p";

/// Records every name it is asked for; answers `1.2.3.4` to each.
#[derive(Default)]
struct Recording(Mutex<Vec<String>>);

impl Recording {
    fn calls(&self) -> Vec<String> {
        self.0.lock().clone()
    }
}

impl Resolver for Recording {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        _max: usize,
        _allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
        self.0.lock().push(host.to_owned());
        Box::pin(async { vec![NetAddr::ipv4([1, 2, 3, 4])] })
    }
}

fn new_core(kind: NetworkKind, f: impl FnOnce(&mut NetConfig)) -> (Arc<NetCore>, Arc<Recording>) {
    let params = ChainParamsBuilder::new(kind, Release::V24_0).build().unwrap();
    let mut cfg = NetConfig::new(Arc::new(params), ServiceFlags::NODE_NETWORK);
    f(&mut cfg);
    let resolver = Arc::new(Recording::default());
    let core = NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        resolver.clone(),
        Arc::new(MockClock::at(NOW)),
        Arc::new(Silent),
    )
    .unwrap();
    (core, resolver)
}

fn cjdns_reachable(cfg: &mut NetConfig) {
    cfg.reachable_overlays = vec![Network::Cjdns];
}

fn v6(s: &str) -> NetAddr {
    NetAddr::from_ip(s.parse().unwrap())
}

fn cjdns(s: &str) -> NetAddr {
    NetAddr::new(Network::Cjdns, v6(s).bytes().to_vec()).unwrap()
}

fn attach(core: &NetCore, addr: Address, dest: Option<String>) -> PeerHandle {
    attach_as(core, addr, dest, ConnectionType::OutboundFullRelay)
}

fn attach_as(
    core: &NetCore,
    addr: Address,
    dest: Option<String>,
    conn_type: ConnectionType,
) -> PeerHandle {
    let (ours, _theirs) = MemSocket::pair();
    core.attach(Attach {
        sock: Arc::new(ours),
        addr,
        addr_bind: Service::default(),
        dest,
        conn_type,
        inbound_onion: false,
        permissions: Permissions::NONE,
        use_v2: false,
        grant: None,
        masternode: false,
        masternode_probe: false,
    })
}

fn at(service: Service) -> Address {
    Address {
        service,
        services: ServiceFlags::NONE,
        time: 0,
    }
}

/// Core's `netbase_splithost` (the port is Core's in/out value, 0 before the call), and the
/// malformed bracket forms the review found.
// Core: src/test/netbase_tests.cpp:87-121
#[test]
fn split_host_port_is_cores() {
    let cases: &[(&str, &str, u16)] = &[
        ("www.bitcoin.org", "www.bitcoin.org", 0),
        ("[www.bitcoin.org]", "www.bitcoin.org", 0),
        ("www.bitcoin.org:80", "www.bitcoin.org", 80),
        ("[www.bitcoin.org]:80", "www.bitcoin.org", 80),
        ("127.0.0.1", "127.0.0.1", 0),
        ("127.0.0.1:9999", "127.0.0.1", 9999),
        ("[127.0.0.1]", "127.0.0.1", 0),
        ("[127.0.0.1]:9999", "127.0.0.1", 9999),
        ("::ffff:127.0.0.1", "::ffff:127.0.0.1", 0),
        ("[::ffff:127.0.0.1]:9999", "::ffff:127.0.0.1", 9999),
        ("[::]:9999", "::", 9999),
        ("::9999", "::9999", 0),
        (":9999", "", 9999),
        ("[]:9999", "", 9999),
        ("", "", 0),
        (":65535", "", 65535),
        (":65536", ":65536", 0),
        (":-1", ":-1", 0),
        ("[]:70001", "[]:70001", 0),
        ("[]:-1", "[]:-1", 0),
        ("[]:-0", "[]:-0", 0),
        ("[]:0", "", 0),
        ("[]:1/2", "[]:1/2", 0),
        ("[]:1E2", "[]:1E2", 0),
        ("127.0.0.1:65536", "127.0.0.1:65536", 0),
        ("127.0.0.1:0", "127.0.0.1", 0),
        ("127.0.0.1:", "127.0.0.1:", 0),
        ("127.0.0.1:1/2", "127.0.0.1:1/2", 0),
        ("127.0.0.1:1E2", "127.0.0.1:1E2", 0),
        ("www.bitcoincore.org:65536", "www.bitcoincore.org:65536", 0),
        ("www.bitcoincore.org:0", "www.bitcoincore.org", 0),
        ("www.bitcoincore.org:", "www.bitcoincore.org:", 0),
        // The review's malformed brackets: no port, and a host no lookup decodes.
        ("[::1]:abc", "[::1]:abc", 0),
        ("[::1]x:9999", "[::1]x:9999", 0),
        ("[::1]:99999", "[::1]:99999", 0),
        // `ParseUInt16`: one leading `+`, never `+-` or a second `+`.
        ("1.2.3.4:+80", "1.2.3.4", 80),
        ("1.2.3.4:++80", "1.2.3.4:++80", 0),
        ("1.2.3.4:+-80", "1.2.3.4:+-80", 0),
        ("1.2.3.4: 80", "1.2.3.4: 80", 0),
        ("[", "[", 0),
        ("[]", "", 0),
    ];
    for &(input, host, port) in cases {
        assert_eq!(split_host_port(input, 0), (host, port), "{input:?}");
    }
}

/// Core's `SetSpecial` cases from `cnetaddr_basic`.
// Core: src/test/net_tests.cpp:553-622
#[test]
fn set_special_is_cores() {
    let tor = NetAddr::set_special(TORV3).unwrap();
    assert_eq!(tor.net(), Network::Onion);
    assert!(tor.is_valid());
    assert_eq!(tor.to_string(), TORV3);
    let i2p_upper = "UDHDrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna.b32.I2P";
    let i2p = NetAddr::set_special(i2p_upper).unwrap();
    assert_eq!(i2p.net(), Network::I2p);
    assert!(i2p.is_valid());
    assert_eq!(i2p.to_string(), i2p_upper.to_lowercase());
    for bad in [
        "6hzph5hv6337r6p2.onion",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscsad.onion",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscrye.onion",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd\0wtf.onion",
        "mfrggzak.onion",
        "mf*g zak.onion",
        "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jn=.b32.i2p",
        "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna=.b32.i2p",
        "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v\0wtf.b32.i2p",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscsad.b32.i2p",
        "tp*szydbh4dp.b32.i2p",
        "totally bogus",
        // SetTor's suffix is case-sensitive.
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.ONION",
        ".onion",
    ] {
        assert_eq!(NetAddr::set_special(bad), None, "{bad:?}");
    }
}

/// `LookupIntern`'s order: a Tor v3 or I2P name and an IP literal never reach the resolver, nor
/// does an empty name, a NUL or an internal address. Every other name does, an invalid `.onion`
/// or `.i2p` name included, as Core passes it to `getaddrinfo`.
// Core: netbase.cpp:147-169 LookupIntern
#[test]
fn pre_lookup_keeps_special_names_from_dns() {
    let tor = NetAddr::set_special(TORV3).unwrap();
    assert_eq!(pre_lookup(TORV3), Lookup::Addr(tor));
    assert_eq!(pre_lookup(I2P), Lookup::Addr(NetAddr::set_special(I2P).unwrap()));
    assert_eq!(pre_lookup("1.2.3.4"), Lookup::Addr(NetAddr::ipv4([1, 2, 3, 4])));
    for refused in ["", "a\0b", "fd6b:88c0:8724:1:2:3:4:5"] {
        assert_eq!(pre_lookup(refused), Lookup::Refused, "{refused:?}");
    }
    for name in [
        "example.com",
        "bogus.onion",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscsad.onion",
        "Mixed.ONION",
        "onion",
        "abc.b32.i2p",
        "forum.i2p",
    ] {
        assert_eq!(pre_lookup(name), Lookup::Name, "{name:?}");
    }
}

/// A named target: Tor and I2P names become their addresses without a resolver call (an I2P
/// name defaults to port 0, `GetDefaultPort(dest)`); an invalid `.onion` name goes to the
/// resolver, as in Core.
// Core: net.cpp:401-415; netbase.cpp:147-159; chainparams.h:94-102
#[tokio::test]
async fn named_tor_and_i2p_targets_never_reach_dns() {
    let (core, resolver) = new_core(NetworkKind::Regtest, |_| {});
    let port = core.config().params.default_port;
    let tor = core.resolve_dest(TORV3).await.unwrap().unwrap();
    assert_eq!(tor.service, Service::new(NetAddr::set_special(TORV3).unwrap(), port));
    let tor = core.resolve_dest(&format!("{TORV3}:1234")).await.unwrap().unwrap();
    assert_eq!(tor.service.port, 1234);
    let i2p = core.resolve_dest(I2P).await.unwrap().unwrap();
    assert_eq!(i2p.service, Service::new(NetAddr::set_special(I2P).unwrap(), 0));
    assert_eq!(resolver.calls(), Vec::<String>::new());
    assert!(core.resolve_dest("bogus.onion:9999").await.is_ok());
    assert!(core.resolve_dest("example.com").await.is_ok());
    assert_eq!(resolver.calls(), ["bogus.onion", "example.com"]);
}

/// A DNS seed's query name, `x<services>.<seed>`, goes through `LookupIntern` to the resolver,
/// a seed under `.onion` included, as in Core.
// Core: net.cpp:2755-2790 (LookupHost → LookupIntern)
#[tokio::test]
async fn a_dns_seed_goes_through_lookup_intern() {
    let (core, resolver) = new_core(NetworkKind::Regtest, |_| {});
    assert_eq!(core.resolve_seed("seed.onion", 256).await, 1);
    assert_eq!(core.resolve_seed("seed.example", 256).await, 1);
    assert_eq!(resolver.calls(), ["x1.seed.onion", "x1.seed.example"]);
}

/// `LookupNumeric` takes Tor and I2P names, so `AddNode` sees `x.onion` and `x.onion:<default>`
/// as one entry, and `GetAddedNodeInfo` matches the onion peer by service.
// Core: net.cpp:3337, :4309-4321; netbase.cpp:219-227
#[tokio::test]
async fn lookup_numeric_takes_tor_names() {
    let (core, resolver) = new_core(NetworkKind::Regtest, |cfg| {
        cfg.reachable_overlays = vec![Network::Onion];
    });
    let port = core.config().params.default_port;
    let tor = Service::new(NetAddr::set_special(TORV3).unwrap(), port);
    assert_eq!(core.lookup_numeric(TORV3), tor);
    assert_eq!(core.lookup_numeric("bogus.onion"), Service::default());
    // Core's `netbase_lookupnumeric` internal case.
    assert_eq!(core.lookup_numeric("[fd6b:88c0:8724:1:2:3:4:5]"), Service::default());
    let add = |node: String| {
        core.add_node(AddedNode {
            node,
            use_v2: false,
        })
    };
    assert!(add(TORV3.to_owned()));
    assert!(!add(format!("{TORV3}:{port}")));
    attach(&core, at(tor), Some(format!("other-name:{port}")));
    assert!(core.unconnected_added_nodes().is_empty());
    assert_eq!(resolver.calls(), Vec::<String>::new());
}

/// m1: a named `fc00::/8` target is CJDNS when CJDNS is reachable (and IPv6 when not), so
/// `GetAddedNodeInfo` sees the addnode connected and the peer is offered our CJDNS address.
// Core: net.cpp:411; net.cpp:3337-3346; net.cpp:177-206
#[tokio::test]
async fn named_cjdns_target_is_cjdns() {
    let (core, _) = new_core(NetworkKind::Mainnet, cjdns_reachable);
    let dest = "[fc00::1]:19999";
    let addr = core.resolve_dest(dest).await.unwrap().unwrap();
    assert_eq!(addr.service, Service::new(cjdns("fc00::1"), 19999));

    assert!(core.add_node(AddedNode {
        node: dest.to_owned(),
        use_v2: false,
    }));
    assert_eq!(core.unconnected_added_nodes().len(), 1);
    // `open` names the peer by `dest`; a different name shows the match is by service.
    let peer = attach(&core, addr, Some("by-another-name".into()));
    assert!(core.unconnected_added_nodes().is_empty());
    assert_eq!(peer.network(), Network::Cjdns);

    let ours_v4 = Service::new(NetAddr::ipv4([1, 2, 3, 4]), 9999);
    let ours_cjdns = Service::new(cjdns("fc00::2"), 9999);
    assert!(core.add_local(ours_v4, LocalScore::Manual));
    assert!(core.add_local(ours_cjdns.clone(), LocalScore::If));
    assert_eq!(core.local_addr_for_peer(&peer), Some(ours_cjdns));

    // Without `-cjdnsreachable` the address stays IPv6.
    let (plain, _) = new_core(NetworkKind::Mainnet, |_| {});
    assert_eq!(
        plain.resolve_dest(dest).await.unwrap().unwrap().service,
        Service::new(v6("fc00::1"), 19999)
    );
}

/// A CJDNS service has the IPv6 socket address of its bytes (`GetSockAddr`).
// Core: netaddress.cpp:868-893
#[test]
fn cjdns_has_a_socket_address() {
    let sa = Service::new(cjdns("fc00::1"), 9999).to_socket_addr().unwrap();
    assert_eq!(sa, "[fc00::1]:9999".parse().unwrap());
    assert_eq!(
        Service::new(v6("2001:db9::1"), 1).to_socket_addr(),
        Some("[2001:db9::1]:1".parse().unwrap())
    );
    assert_eq!(Service::new(NetAddr::set_special(TORV3).unwrap(), 1).to_socket_addr(), None);
}

/// `open` dials a CJDNS address and records the `Attempt` even when the connect fails; so does
/// a Tor address, which has no socket address without a proxy (Core's `ConnectDirectly`
/// failing in `CreateSock`). A named `fc00::/8` target attempts the CJDNS entry.
// Core: net.cpp:416-492; netbase.cpp:629-650
#[tokio::test]
async fn open_attempts_cjdns_and_unsupported_networks() {
    // With a random addrman key, `add` left one of the three entries out of the table in about
    // 1 run in 100 (so `get_address_info` was `None`); a fixed key makes the placement repeatable.
    let (core, _) = new_core(NetworkKind::Mainnet, |cfg| {
        cfg.reachable_overlays = vec![Network::Cjdns, Network::Onion];
        cfg.connect_timeout = Duration::from_millis(200);
        cfg.deterministic = true;
    });
    let source = NetAddr::ipv4([5, 6, 7, 8]);
    let cj = Service::new(cjdns("fc00::1"), 9999);
    let cj_named = Service::new(cjdns("fc00::3"), 9999);
    let tor = Service::new(NetAddr::set_special(TORV3).unwrap(), 9999);
    let entries: Vec<Address> = [&cj, &cj_named, &tor]
        .into_iter()
        .map(|s| Address {
            service: s.clone(),
            services: ServiceFlags::NODE_NETWORK,
            time: NOW as u32,
        })
        .collect();
    core.addrman().add(&entries, &source, 0, NOW);
    let open = |target| {
        core.open(OpenRequest {
            target,
            conn_type: ConnectionType::OutboundFullRelay,
            count_failure: true,
            grant: None,
            use_v2: false,
            masternode: false,
            masternode_probe: false,
        })
    };
    for (target, service) in [
        (Target::Addr(entries[0].clone()), &cj),
        (Target::Dest("[fc00::3]:9999".into()), &cj_named),
        (Target::Addr(entries[2].clone()), &tor),
    ] {
        assert_eq!(core.addrman().get_address_info(service).unwrap().last_try, 0);
        let r = open(target).await;
        assert!(matches!(r, Err(OpenError::Connect(_))), "{service}: {r:?}");
        let info = core.addrman().get_address_info(service).unwrap();
        assert_eq!((info.last_try, info.attempts), (NOW, 1), "{service}");
    }
}

/// An accepted `fc00::/8` peer and our end are CJDNS before `-whitelist` matches, as Core's
/// `AcceptConnection` flips both.
// Core: net.cpp:1864-1867
#[cfg(feature = "inbound")]
#[tokio::test]
async fn accepted_cjdns_peer_is_cjdns() {
    let (core, _) = new_core(NetworkKind::Mainnet, |cfg| {
        cjdns_reachable(cfg);
        cfg.whitelist = vec![(crate::SubNet::single(cjdns("fc00::5")), Permissions::NO_BAN)];
    });
    let (ours, _theirs) = MemSocket::pair();
    let peer = core
        .accept(
            Arc::new(ours),
            at(Service::new(v6("fc00::5"), 5555)),
            Service::new(v6("fc00::6"), 9999),
            Permissions::NONE,
            false,
            false,
        )
        .unwrap();
    assert_eq!(peer.addr().service, Service::new(cjdns("fc00::5"), 5555));
    assert_eq!(*peer.addr_bind(), Service::new(cjdns("fc00::6"), 9999));
    assert_eq!(peer.network(), Network::Cjdns);
    assert!(peer.has_permission(Permissions::NO_BAN));
}

/// `getnetworkinfo`'s `localaddresses` (BL-391): `mapLocalHost` in `CNetAddr` order (IPv4
/// before IPv6, whatever order they were learnt in), each with the port and score `AddLocal`
/// last kept: a repeat at no lower score raises it by one and takes the new port.
// Core: net.cpp:284-310 AddLocal, rpc/net.cpp:723-733
#[tokio::test]
async fn local_addresses_are_the_map_in_cores_order() {
    let (core, _) = new_core(NetworkKind::Mainnet, |_| {});
    let ipv6 = v6("2a00:1450::1");
    let ipv4 = NetAddr::ipv4([1, 2, 3, 4]);
    assert!(core.add_local(Service::new(ipv6.clone(), 9999), LocalScore::If));
    assert!(core.add_local(Service::new(ipv4.clone(), 9999), LocalScore::Manual));
    assert!(core.add_local(Service::new(ipv6.clone(), 19999), LocalScore::If));
    assert_eq!(
        core.local_addresses(),
        [(ipv4, 9999, LocalScore::Manual as i32), (ipv6, 19999, LocalScore::If as i32 + 1)]
    );
}

/// Core's `advertise_local_address`, and n1: with no local address for the peer's network,
/// `GetLocalAddress` is `CNetAddr()` (IPv6 `::`) on the listen port.
// Core: src/test/net_tests.cpp:1267-1380; net.cpp:232-235
#[tokio::test]
async fn advertise_local_address_is_cores() {
    let (core, _) = new_core(NetworkKind::Mainnet, |cfg| {
        cfg.reachable_overlays = vec![Network::Onion, Network::I2p, Network::Cjdns];
        cfg.listen_port = 9999;
    });
    let svc = |a: NetAddr| Service::new(a, 8333);
    let ipv4 = svc(NetAddr::ipv4([1, 2, 3, 4]));
    let ipv6 = svc(v6("1122:3344:5566:7788:9900:aabb:ccdd:eeff"));
    let ipv6_tunnel = svc(v6("2002:3344:5566:7788:9900:aabb:ccdd:eeff"));
    let teredo = svc(v6("2001:0000:5566:7788:9900:aabb:ccdd:eeff"));
    let onion = svc(NetAddr::set_special(TORV3).unwrap());
    let i2p = svc(NetAddr::set_special(I2P).unwrap());
    let cjdns = core.maybe_flip_ipv6_to_cjdns(svc(v6("fc00:3344:5566:7788:9900:aabb:ccdd:eeff")));
    assert_eq!(cjdns.addr.net(), Network::Cjdns);
    let all = [&ipv4, &ipv6, &ipv6_tunnel, &teredo, &onion, &i2p, &cjdns];
    let peers: Vec<PeerHandle> =
        all.iter().map(|s| attach(&core, at((*s).clone()), None)).collect();
    let [p_ipv4, p_ipv6, p_tunnel, p_teredo, p_onion, p_i2p, p_cjdns] = &peers[..] else {
        unreachable!()
    };
    let none = Service::new(NetAddr::default(), 9999);
    assert_eq!(none.addr.net(), Network::Ipv6);
    assert!(!none.addr.is_valid());

    // One local clearnet address: advertised to all but privacy peers.
    core.add_local(ipv4.clone(), LocalScore::None);
    for p in [p_ipv4, p_ipv6, p_tunnel, p_teredo, p_cjdns] {
        assert_eq!(core.local_address(p), ipv4);
    }
    assert_eq!(core.local_address(p_onion), none);
    assert_eq!(core.local_address(p_i2p), none);
    core.remove_local(&ipv4);

    // Local privacy addresses: not advertised to clearnet peers.
    core.add_local(onion.clone(), LocalScore::None);
    core.add_local(i2p.clone(), LocalScore::None);
    for p in [p_ipv4, p_ipv6, p_tunnel, p_teredo, p_cjdns] {
        assert_eq!(core.local_address(p), none);
    }
    assert_eq!(core.local_address(p_onion), onion);
    assert_eq!(core.local_address(p_i2p), i2p);
    core.remove_local(&onion);
    core.remove_local(&i2p);

    // Local addresses from all networks.
    for s in all {
        core.add_local(s.clone(), LocalScore::None);
    }
    assert_eq!(core.local_address(p_ipv4), ipv4);
    assert_eq!(core.local_address(p_ipv6), ipv6);
    assert_eq!(core.local_address(p_tunnel), ipv6);
    assert_eq!(core.local_address(p_teredo), ipv4);
    assert_eq!(core.local_address(p_onion), onion);
    assert_eq!(core.local_address(p_i2p), i2p);
    assert_eq!(core.local_address(p_cjdns), cjdns);
}

/// `Discover` adds each interface address the host found, and nothing with
/// `-discover=0`. On regtest an unroutable address counts (`AddLocal` refuses one only on a chain
/// that requires a routable external IP).
// Core: net.cpp:3843-3887 Discover, :284-310 AddLocal
#[tokio::test]
async fn discover_adds_the_interfaces() {
    let interfaces: Vec<(String, std::net::IpAddr)> = vec![
        ("eth0".into(), "10.0.0.5".parse().unwrap()),
        ("eth0".into(), "2001:db8::5".parse().unwrap()),
        ("wg0".into(), "192.168.7.1".parse().unwrap()),
    ];
    let (off, _) = new_core(NetworkKind::Regtest, |cfg| cfg.discover = false);
    off.discover(&interfaces);
    let (on, _) = new_core(NetworkKind::Regtest, |_| {});
    on.discover(&interfaces);
    for (_, ip) in &interfaces {
        let addr = NetAddr::from_ip(*ip);
        assert!(!off.is_local(&addr), "{addr}");
        assert!(on.is_local(&addr), "{addr}");
    }
}

/// A peer connected by the name `seed.test:9999`, at the address the name resolved to then.
async fn named_peer() -> (Arc<NetCore>, Arc<Recording>, impl Fn(&str) -> OpenRequest) {
    let (core, resolver) = new_core(NetworkKind::Regtest, |cfg| {
        cfg.connect_timeout = Duration::from_millis(200);
    });
    let then = at(Service::new(NetAddr::ipv4([5, 6, 7, 8]), 9999));
    attach(&core, then, Some("seed.test:9999".into()));
    let request = |dest: &str| OpenRequest {
        target: Target::Dest(dest.to_owned()),
        conn_type: ConnectionType::Manual,
        count_failure: false,
        grant: None,
        use_v2: false,
        masternode: false,
        masternode_probe: false,
    };
    (core, resolver, request)
}

/// Opening a name already connected by that name is refused before any lookup, even when the
/// name now resolves elsewhere (here to `1.2.3.4`, which no peer has), so that the address
/// check after the lookup cannot be what refuses it (BL-394 review r2 B-4).
// Core: net.cpp:3619 OpenNetworkConnection (`ExistsNode(std::string(pszDest))`)
#[tokio::test]
async fn open_refuses_a_name_already_connected_before_resolving_it() {
    let (core, resolver, request) = named_peer().await;
    let r = core.open(request("seed.test:9999")).await;
    assert!(matches!(r, Err(OpenError::AlreadyConnected)), "{r:?}");
    assert_eq!(resolver.calls(), Vec::<String>::new());
}

/// A name with a NUL is opened as the name before it (Core's `c_str()`): the by-name check
/// finds the peer connected by that name (BL-394 review r2 B-2).
// Core: rpc/net.cpp:358; net.cpp:3385, 3619
#[tokio::test]
async fn open_cuts_a_name_at_its_first_nul() {
    let (core, resolver, request) = named_peer().await;
    let r = core.open(request("seed.test:9999\0junk")).await;
    assert!(matches!(r, Err(OpenError::AlreadyConnected)), "{r:?}");
    assert_eq!(resolver.calls(), Vec::<String>::new());
    let r = core.open(request("other.test:9999\0seed.test:9999")).await;
    assert!(!matches!(r, Err(OpenError::AlreadyConnected)), "{r:?}");
    assert_eq!(resolver.calls(), ["other.test"]);
}

fn loopback(port: u16) -> Address {
    at(Service::new(NetAddr::ipv4([127, 0, 0, 1]), port))
}

/// `disconnectnode`'s lookups: by connection name (not by address) or by id, each marking the
/// first peer in the table not already marked, so a second call finds the next peer of that
/// name, then none.
// Core: net.cpp:4578-4613 CConnman::DisconnectNode; net.h:1712-1731 FindNodeMutable
#[tokio::test]
async fn disconnect_marks_the_first_unmarked_match() {
    let (core, _) = new_core(NetworkKind::Regtest, |_| {});
    let first = attach(&core, loopback(1), Some("twice".into()));
    let second = attach(&core, loopback(2), Some("twice".into()));
    let inbound = attach_as(&core, loopback(3), None, ConnectionType::Inbound);
    assert_eq!(inbound.addr_name(), "127.0.0.1:3");

    assert!(!core.disconnect_by_name("127.0.0.1:1"), "an outbound peer goes by its name");
    assert!(core.disconnect_by_name("twice"));
    assert!(first.disconnect_requested() && !second.disconnect_requested());
    assert!(core.disconnect_by_name("twice"));
    assert!(second.disconnect_requested());
    assert!(!core.disconnect_by_name("twice"));

    assert!(!core.disconnect_by_id(first.id()), "a marked peer is not found");
    assert!(!core.disconnect_by_id(-1));
    assert!(core.disconnect_by_id(inbound.id()));
    assert!(inbound.disconnect_requested());
    assert!(!core.disconnect_by_name("127.0.0.1:3"));
}

/// `GetAddedNodeInfo`: a numeric entry matches a peer's address (an inbound one too), a name
/// matches a peer's connection name and takes the peer's address, and a name holding a NUL
/// matches no peer (the dial cuts it, the entry keeps it), nor does the empty name; peers being
/// disconnected count, and of two peers on one address, or of one name, the later one gives
/// the connection. Without `include_connected`, only the unconnected entries remain.
// Core: net.cpp:3308-3366 CConnman::GetAddedNodeInfo
#[tokio::test]
async fn added_node_info_is_cores() {
    let (core, _) = new_core(NetworkKind::Regtest, |_| {});
    for node in ["127.0.0.1:1", "by-name", "127.0.0.1:9", "by-name\0cut", ""] {
        assert!(core.add_node(AddedNode {
            node: node.into(),
            use_v2: false,
        }));
    }
    let inbound = attach_as(&core, loopback(1), None, ConnectionType::Inbound);
    attach(&core, at(Service::new(NetAddr::ipv4([10, 0, 0, 4]), 7)), Some("by-name".into()));
    attach(&core, at(Service::new(NetAddr::ipv4([10, 0, 0, 5]), 7)), Some("by-name".into()));
    attach(&core, at(Service::new(NetAddr::ipv4([10, 0, 0, 6]), 7)), Some(String::new()));
    let connections = |include| -> Vec<(String, Option<(String, bool)>)> {
        let infos = core.added_node_info(include).into_iter();
        let shown =
            infos.map(|i| (i.node.node, i.connection.map(|c| (c.service.to_string(), c.inbound))));
        shown.collect()
    };
    let numeric = ("127.0.0.1:1".to_owned(), Some(("127.0.0.1:1".to_owned(), true)));
    let named = ("by-name".to_owned(), Some(("10.0.0.5:7".to_owned(), false)));
    let unconnected = ("127.0.0.1:9".to_owned(), None);
    let nul = ("by-name\0cut".to_owned(), None);
    let empty = (String::new(), None);
    assert_eq!(
        connections(true),
        [numeric.clone(), named.clone(), unconnected.clone(), nul.clone(), empty.clone()]
    );
    assert_eq!(connections(false), [unconnected.clone(), nul.clone(), empty.clone()]);

    assert!(core.disconnect_by_id(inbound.id()));
    assert_eq!(connections(true)[0], numeric, "a peer being disconnected counts");
    attach(&core, loopback(1), Some("another-name".into()));
    let later = ("127.0.0.1:1".to_owned(), Some(("127.0.0.1:1".to_owned(), false)));
    assert_eq!(connections(true), [later, named, unconnected, nul, empty]);
}
