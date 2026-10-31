//! `NetCore::open` through a local mock SOCKS5 proxy: which proxy each network uses, the name
//! proxy for names that do not resolve, `-proxyrandomize`'s credentials, the `Attempt` rule and a
//! UNIX socket proxy. Core's `ConnectNode` (net.cpp:370-520) and feature_proxy.py's checks.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dash_clock::MockClock;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::dial::loop_tests::Silent;
use crate::proxy::{Proxy, ProxyAddr};
use crate::store::NoStore;
use crate::{
    Address, ConnectionType, NetAddr, NetConfig, NetCore, Network, OpenError, OpenRequest,
    Resolver, Service, ServiceFlags, Target,
};

const NOW: i64 = 1_700_000_000;
// Core: test/functional/feature_proxy.py:189
const TORV3: &str = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";

/// What a client asked the mock proxy for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Request {
    /// The methods offered.
    methods: Vec<u8>,
    /// User name and password, when sent.
    auth: Option<(String, String)>,
    /// `ATYP`, the address bytes and the port.
    atyp: u8,
    addr: Vec<u8>,
    port: u16,
}

/// A SOCKS5 server for tests: picks user/password when offered, else no authentication,
/// records each request and answers `REP` = `rep`. A connection it accepted stays open.
struct MockProxy {
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serves one client, recording its request in `log` before the reply, so a client that has
/// its reply finds the request recorded.
async fn serve_one<S: AsyncRead + AsyncWrite + Unpin>(
    mut s: S,
    rep: u8,
    log: &Mutex<Vec<Request>>,
) -> Option<S> {
    let mut head = [0u8; 2];
    s.read_exact(&mut head).await.ok()?;
    let mut methods = vec![0; usize::from(head[1])];
    s.read_exact(&mut methods).await.ok()?;
    let mut auth = None;
    if methods.contains(&2) {
        s.write_all(&[5, 2]).await.ok()?;
        let mut v = [0u8; 2];
        s.read_exact(&mut v).await.ok()?;
        let mut user = vec![0; usize::from(v[1])];
        s.read_exact(&mut user).await.ok()?;
        let mut n = [0u8; 1];
        s.read_exact(&mut n).await.ok()?;
        let mut pass = vec![0; usize::from(n[0])];
        s.read_exact(&mut pass).await.ok()?;
        auth = Some((String::from_utf8(user).ok()?, String::from_utf8(pass).ok()?));
        s.write_all(&[1, 0]).await.ok()?;
    } else {
        s.write_all(&[5, 0]).await.ok()?;
    }
    let mut req = [0u8; 4];
    s.read_exact(&mut req).await.ok()?;
    let addr = match req[3] {
        3 => {
            let mut n = [0u8; 1];
            s.read_exact(&mut n).await.ok()?;
            let mut a = vec![0; usize::from(n[0])];
            s.read_exact(&mut a).await.ok()?;
            a
        }
        1 => {
            let mut a = vec![0; 4];
            s.read_exact(&mut a).await.ok()?;
            a
        }
        _ => {
            let mut a = vec![0; 16];
            s.read_exact(&mut a).await.ok()?;
            a
        }
    };
    let mut port = [0u8; 2];
    s.read_exact(&mut port).await.ok()?;
    log.lock().push(Request {
        methods,
        auth,
        atyp: req[3],
        addr,
        port: u16::from_be_bytes(port),
    });
    s.write_all(&[5, rep, 0, 1, 0, 0, 0, 0, 0, 0]).await.ok()?;
    Some(s)
}

impl MockProxy {
    /// On `127.0.0.1` with an OS-chosen port.
    async fn tcp(rep: u8) -> (Self, Service) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let service = Service::from_socket_addr(listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((s, _)) = listener.accept().await {
                open.extend(serve_one(s, rep, &recorded).await);
            }
        });
        (
            Self {
                requests,
                task,
            },
            service,
        )
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().clone()
    }
}

fn proxy_at(service: &Service, randomize: bool) -> Proxy {
    Proxy {
        addr: ProxyAddr::Service(service.clone()),
        randomize_credentials: randomize,
    }
}

/// Records each lookup; a name resolves only with `allow_lookup`, as `getaddrinfo` with
/// `AI_NUMERICHOST` resolves none.
#[derive(Default)]
struct Dns(Mutex<Vec<(String, bool)>>);

impl Resolver for Dns {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        _max: usize,
        allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
        self.0.lock().push((host.to_owned(), allow_lookup));
        Box::pin(async move {
            if allow_lookup {
                vec![NetAddr::ipv4([1, 2, 3, 4])]
            } else {
                Vec::new()
            }
        })
    }
}

fn new_core(f: impl FnOnce(&mut NetConfig)) -> (Arc<NetCore>, Arc<Dns>) {
    let params = ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().unwrap();
    let mut cfg = NetConfig::new(Arc::new(params), ServiceFlags::NODE_NETWORK);
    cfg.connect_timeout = Duration::from_millis(500);
    cfg.deterministic = true;
    f(&mut cfg);
    let dns = Arc::new(Dns::default());
    let core = NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        dns.clone(),
        Arc::new(MockClock::at(NOW)),
        Arc::new(Silent),
    )
    .unwrap();
    (core, dns)
}

fn request(target: Target) -> OpenRequest {
    OpenRequest {
        target,
        conn_type: ConnectionType::Manual,
        count_failure: true,
        grant: None,
        use_v2: false,
        masternode: false,
        masternode_probe: false,
    }
}

fn addr(service: &Service) -> Target {
    Target::Addr(Address {
        service: service.clone(),
        services: ServiceFlags::NODE_NETWORK,
        time: NOW as u32,
    })
}

fn v6(s: &str) -> NetAddr {
    NetAddr::from_ip(s.parse().unwrap())
}

fn domain(dest: &str, port: u16) -> (u8, Vec<u8>, u16) {
    (3, dest.as_bytes().to_vec(), port)
}

fn asked(r: &Request) -> (u8, Vec<u8>, u16) {
    (r.atyp, r.addr.clone(), r.port)
}

/// feature_proxy.py's `node_test` for `-proxy` plus `-onion` (its node 1) and `-cjdnsreachable`
/// (node 4): IPv4, IPv6 and CJDNS go through `-proxy`, Tor through `-onion`, each as a domain
/// name; a name that does not resolve goes through the name proxy, unresolved.
// Core: init.cpp:1925-1961 (SetProxy, SetNameProxy); net.cpp:401-484
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_network_uses_its_proxy() {
    let (main, main_at) = MockProxy::tcp(0).await;
    let (onion, onion_at) = MockProxy::tcp(0).await;
    let (core, dns) = new_core(|cfg| {
        for net in [Network::Ipv4, Network::Ipv6, Network::Cjdns] {
            cfg.proxies.set(net, proxy_at(&main_at, false));
        }
        cfg.proxies.set_name_proxy(proxy_at(&main_at, false));
        cfg.proxies.set(Network::Onion, proxy_at(&onion_at, false));
        cfg.reachable_overlays = vec![Network::Onion, Network::Cjdns];
    });
    let v4 = Service::new(NetAddr::ipv4([15, 61, 23, 23]), 1234);
    let v6s = Service::new(v6("1233:3432:2434:2343:3234:2345:6546:4534"), 5443);
    let tor = Service::new(NetAddr::set_special(TORV3).unwrap(), 8333);
    for target in [addr(&v4), addr(&v6s), addr(&tor)] {
        core.open(request(target)).await.unwrap();
    }
    core.open(request(Target::Dest("[fc00:1:2:3:4:5:6:7]:8888".into()))).await.unwrap();
    let named = core.open(request(Target::Dest("node.noumenon:8333".into()))).await.unwrap();
    assert_eq!(
        main.requests().iter().map(asked).collect::<Vec<_>>(),
        [
            domain("15.61.23.23", 1234),
            domain("1233:3432:2434:2343:3234:2345:6546:4534", 5443),
            domain("fc00:1:2:3:4:5:6:7", 8888),
            domain("node.noumenon", 8333),
        ]
    );
    assert_eq!(onion.requests().iter().map(asked).collect::<Vec<_>>(), [domain(TORV3, 8333)]);
    // No credentials offered without -proxyrandomize.
    assert!(main.requests().iter().chain(&onion.requests()).all(|r| r.methods == [0]));
    // With a name proxy a name is never looked up (`fNameLookup && !HaveNameProxy()`).
    assert_eq!(*dns.0.lock(), [("node.noumenon".to_owned(), false)]);
    // The named peer keeps Core's invalid `CAddress` and its name.
    assert_eq!(named.addr().service, Service::default());
    assert_eq!(named.addr_name(), "node.noumenon:8333");
    // feature_proxy's `network_test`: getpeerinfo's networks.
    let nets: Vec<Network> = core.peers().iter().map(|p| p.addr().service.addr.net()).collect();
    assert_eq!(nets, [Network::Ipv4, Network::Ipv6, Network::Onion, Network::Cjdns, Network::Ipv6]);
}

/// `-proxyrandomize`: each connection offers user/password with a fresh counter value as both,
/// so every stream is isolated; without a proxy for its network an address dials directly.
// Core: netbase.cpp:745-751
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn randomized_credentials_are_unique() {
    let (main, main_at) = MockProxy::tcp(0).await;
    let (core, _) = new_core(|cfg| {
        cfg.proxies.set(Network::Ipv4, proxy_at(&main_at, true));
    });
    for last in 1..=3 {
        let s = Service::new(NetAddr::ipv4([15, 61, 23, last]), 1234);
        core.open(request(addr(&s))).await.unwrap();
    }
    let auth: Vec<_> = main.requests().into_iter().map(|r| (r.methods, r.auth.unwrap())).collect();
    let pair = |n: &str| (vec![0, 2], (n.to_owned(), n.to_owned()));
    assert_eq!(auth, [pair("0"), pair("1"), pair("2")]);
}

/// A proxy that cannot be reached takes no credentials: Core counts only after `Proxy::Connect`.
// Core: netbase.cpp:737-748
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_proxy_takes_no_credentials() {
    let (main, main_at) = MockProxy::tcp(0).await;
    // A port nothing listens on.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_at = Service::from_socket_addr(closed.local_addr().unwrap());
    drop(closed);
    let (core, _) = new_core(|cfg| {
        cfg.proxies.set(Network::Ipv4, proxy_at(&closed_at, true));
    });
    let target = Service::new(NetAddr::ipv4([15, 61, 23, 23]), 1234);
    core.open(request(addr(&target))).await.unwrap_err();
    assert!(core.set_proxy(Network::Ipv4, proxy_at(&main_at, true)));
    // An invalid proxy leaves the entry as it was.
    let invalid = Proxy {
        addr: ProxyAddr::Service(Service::default()),
        randomize_credentials: true,
    };
    assert!(!core.set_proxy(Network::Ipv4, invalid.clone()) && !core.set_name_proxy(invalid));
    assert_eq!(core.proxy(Network::Ipv4), Some(proxy_at(&main_at, true)));
    assert_eq!(core.name_proxy(), None);
    core.open(request(addr(&target))).await.unwrap();
    let auth: Vec<_> = main.requests().into_iter().map(|r| r.auth.unwrap()).collect();
    assert_eq!(auth, [("0".to_owned(), "0".to_owned())]);
}

/// Proxies are only for routable addresses: a loopback peer is `NET_UNROUTABLE`, so it is
/// dialled directly (feature_proxy.py's note), and an IPv6 address has no IPv4 proxy.
// Core: net.cpp:434 `GetProxy(addrConnect.GetNetwork(), proxy)`
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unroutable_addresses_dial_directly() {
    let (main, main_at) = MockProxy::tcp(0).await;
    let peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer_at = Service::from_socket_addr(peer.local_addr().unwrap());
    let (core, _) = new_core(|cfg| {
        cfg.proxies.set(Network::Ipv4, proxy_at(&main_at, false));
    });
    core.open(request(addr(&peer_at))).await.unwrap();
    assert!(main.requests().is_empty());
}

/// Core's attempt rule: a proxy that cannot be reached is not the peer's failure, so AddrMan
/// records no attempt; a proxy that reports the peer unreachable (`REP` 5) does count.
// Core: net.cpp:476-481, netbase.cpp:737-742
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_failures_and_the_attempt_rule() {
    let (refusing, refusing_at) = MockProxy::tcp(5).await;
    let (main, main_at) = MockProxy::tcp(0).await;
    // A port nothing listens on.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_at = Service::from_socket_addr(closed.local_addr().unwrap());
    drop(closed);
    let mut cases =
        vec![(proxy_at(&closed_at, true), false), (proxy_at(&refusing_at, false), true)];
    // A UNIX socket proxy at a path nothing listens on fails as a down TCP proxy does.
    #[cfg(unix)]
    cases.push((
        Proxy {
            addr: ProxyAddr::Unix(format!(
                "unix:{}",
                std::env::temp_dir().join(format!("p8-dead-{}.sock", std::process::id())).display()
            )),
            randomize_credentials: true,
        },
        false,
    ));
    for (proxy, attempted) in cases {
        let (core, _) = new_core(|cfg| assert!(cfg.proxies.set(Network::Ipv4, proxy.clone())));
        let peer = Service::new(NetAddr::ipv4([15, 61, 23, 23]), 1234);
        let entry = Address {
            service: peer.clone(),
            services: ServiceFlags::NODE_NETWORK,
            time: NOW as u32,
        };
        core.addrman().add(std::slice::from_ref(&entry), &NetAddr::ipv4([5, 6, 7, 8]), 0, NOW);
        let r = core.open(request(Target::Addr(entry.clone()))).await;
        assert!(matches!(r, Err(OpenError::Connect(_))), "{r:?}");
        let info = core.addrman().get_address_info(&peer).unwrap();
        assert_eq!(info.last_try == NOW, attempted, "{proxy}");
        // A proxy that could not be reached took no `-proxyrandomize` number: the next
        // connection's credentials are this core's first.
        if proxy.randomize_credentials {
            assert!(core.set_proxy(Network::Ipv4, proxy_at(&main_at, true)));
            core.open(request(Target::Addr(entry))).await.unwrap();
            let auth = main.requests().pop().unwrap().auth;
            assert_eq!(auth, Some(("0".to_owned(), "0".to_owned())), "{proxy}");
        }
    }
    assert_eq!(refusing.requests().len(), 1);
}

/// Without a name proxy, a name that does not resolve fails as before, and the resolver may
/// look it up (`-dns`).
#[tokio::test]
async fn without_a_name_proxy_names_are_looked_up() {
    let (core, dns) = new_core(|cfg| cfg.name_lookup = false);
    let r = core.open(request(Target::Dest("node.noumenon".into()))).await;
    assert_eq!(r.unwrap_err(), OpenError::Resolve("node.noumenon".into()));
    assert_eq!(*dns.0.lock(), [("node.noumenon".to_owned(), false)]);
}

/// With a name proxy the DNS seeds are not looked up: each seed becomes an ADDR_FETCH
/// destination for the proxy to resolve.
// Core: net.cpp:2758-2762
#[tokio::test]
async fn dns_seeds_go_to_the_name_proxy() {
    let (_main, main_at) = MockProxy::tcp(0).await;
    let (core, dns) =
        new_core(|cfg| assert!(cfg.proxies.set_name_proxy(proxy_at(&main_at, false))));
    assert_eq!(core.resolve_seed("seed.example", 32).await, 0);
    assert!(dns.0.lock().is_empty());
    assert_eq!(core.dial.lock().addr_fetches.iter().collect::<Vec<_>>(), ["seed.example"]);
}

/// `-proxy=unix:<path>`: the same negotiation over a UNIX socket (feature_proxy.py's nodes 5
/// and 6); our end has no IP address.
// Core: netbase.cpp:652-682
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_socket_proxy() {
    let dir = std::env::temp_dir().join(format!("p8-unix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("socks.sock");
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            open.extend(serve_one(s, 0, &recorded).await);
        }
    });
    let unix = Proxy {
        addr: ProxyAddr::Unix(format!("unix:{}", path.display())),
        randomize_credentials: true,
    };
    let (core, _) = new_core(|cfg| {
        cfg.proxies.set(Network::Ipv4, unix.clone());
        cfg.proxies.set(Network::Onion, unix);
        cfg.reachable_overlays = vec![Network::Onion];
    });
    let tor = Service::new(NetAddr::set_special(TORV3).unwrap(), 8333);
    let peer = core.open(request(addr(&tor))).await.unwrap();
    assert_eq!(peer.addr_bind(), &Service::default());
    let got: Vec<_> = requests.lock().iter().map(asked).collect();
    assert_eq!(got, [domain(TORV3, 8333)]);
    task.abort();
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
}
