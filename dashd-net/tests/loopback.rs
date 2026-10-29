//! Whole nodes over 127.0.0.1 TCP (P2a §6.7 loopback tier): SB6's core with P2a-core's
//! processor, `NodeAddressRelay`, `FileStore` and the [`ConnMan`] threads and listeners.
//! `-connect`, `-addnode`, `-seednode`, a feeler through `addconnection`, `addnode onetry`, and
//! the anchors round trip of Core's `feature_anchors.py`.
//!
//! Nothing here leaves the machine: every node dials through [`loopback_only`] (the harness's
//! transport boundary, [`ConnManOptions::open_filter`]), resolves numeric hosts only, and
//! checks at stop that each of its connections is on loopback. The routable addresses the
//! `-seednode` test plants in AddrMan stay addresses.

use std::net::TcpListener as StdListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dash_clock::SystemClock;
use dash_p2p::{
    Address, ConnectionType, NetAddr, NetConfig, NetCore, OpenRequest, PeerHandle, Service,
    ServiceFlags, Target,
};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dashd_net::binds::BindOptions;
use dashd_net::{ConnMan, ConnManOptions, FileStore, NodeAddressRelay, SystemResolver};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::SimChain;
use dashd_p2p::{MessageLoop, PeerManager, Registry, Seams, Shared};

fn params() -> Arc<ChainParams> {
    Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap())
}

fn loopback(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// Whether `req` goes to a loopback address (a name only as a loopback literal).
fn is_loopback(req: &OpenRequest) -> bool {
    match &req.target {
        Target::Addr(a) => a.service.addr.to_ip().is_some_and(|ip| ip.is_loopback()),
        Target::Dest(d) => d.parse::<std::net::SocketAddr>().is_ok_and(|sa| sa.ip().is_loopback()),
    }
}

/// The open filter of every node here: loopback opens go through; any other is refused before a
/// socket exists, and recorded in `refused`.
fn loopback_only(refused: &Arc<Mutex<Vec<String>>>) -> dashd_net::driver::OpenFilter {
    let refused = Arc::clone(refused);
    Arc::new(move |req: &OpenRequest| {
        let ok = is_loopback(req);
        if !ok {
            refused.lock().unwrap().push(format!("{:?}", req.target));
        }
        ok
    })
}

/// A fresh data directory under the temporary directory, removed when the test passes.
struct DataDir(PathBuf);

impl DataDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("dashd-net-loopback-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for DataDir {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("node files kept in {}", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Polls `cond` every 10 ms for up to 60 s (the box is loaded).
fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A regtest core listening on 127.0.0.1:`port` with its files in `dir`, and the processor's
/// shared half and chain, before anything runs.
struct Parts {
    rt: tokio::runtime::Runtime,
    core: Arc<NetCore>,
    shared: Arc<Shared>,
    chain: Arc<SimChain>,
}

fn build(dir: &Path, port: u16, hook: impl FnOnce(&mut NetConfig)) -> Parts {
    let rt =
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let p = params();
    let chain = SimChain::new(Arc::clone(&p));
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let shared = Shared::new(id, ProcessorOptions::default(), chain.clone());
    let mut cfg = NetConfig::new(p, local_services(&IdentityOptions::new()));
    cfg.listen = true;
    cfg.listen_port = port;
    cfg.discover = false;
    cfg.dns_seed = false;
    cfg.connect_timeout = Duration::from_secs(1);
    // `-dns=0`: no name reaches DNS.
    cfg.name_lookup = false;
    hook(&mut cfg);
    let core = NetCore::new(
        cfg,
        rt.handle().clone(),
        Arc::new(FileStore::new(dir)),
        Arc::new(SystemResolver::default()),
        Arc::new(SystemClock),
        shared.clone(),
    )
    .unwrap();
    Parts {
        rt,
        core,
        shared,
        chain,
    }
}

/// `-bind=127.0.0.1:<port>`, dialing through [`loopback_only`].
fn conn_opts(port: u16, refused: &Arc<Mutex<Vec<String>>>) -> ConnManOptions {
    ConnManOptions {
        binds: BindOptions {
            binds: vec![Service::from_socket_addr(loopback(port).parse().unwrap())],
            ..BindOptions::default()
        },
        open_filter: Some(loopback_only(refused)),
        ..ConnManOptions::default()
    }
}

/// One node: its runtime, core, processor thread and connection manager.
struct Node {
    core: Arc<NetCore>,
    /// The port it listens on (the system's choice).
    port: u16,
    cm: Option<ConnMan>,
    pm: Option<PeerManager>,
    /// The opens [`loopback_only`] refused.
    refused: Arc<Mutex<Vec<String>>>,
    // Declared last: dropped after everything that spawned onto it.
    rt: tokio::runtime::Runtime,
}

impl Node {
    /// [`build`], then the processor with the relay, then `ConnMan::start` binding
    /// 127.0.0.1 on a port the system picks (no window in which another socket can take it).
    fn start(dir: &Path, hook: impl FnOnce(&mut NetConfig)) -> Self {
        let Parts {
            rt,
            core,
            shared,
            chain,
        } = build(dir, 0, hook);
        let seams = Seams {
            addr_relay: Box::new(NodeAddressRelay::new(chain)),
            ..Seams::default()
        };
        let lp = MessageLoop::new(
            Arc::clone(&core),
            Arc::clone(&shared),
            Registry::new(),
            seams,
            dash_p2p::rng::FastRandom::new(),
        );
        let pm = PeerManager::spawn(lp);
        let policy = shared.dial_policy();
        let refused = Arc::default();
        let opts = conn_opts(0, &refused);
        let cm = ConnMan::start(Arc::clone(&core), rt.handle().clone(), policy, opts).unwrap();
        let port = cm.listen_addrs()[0].port();
        Self {
            core,
            port,
            cm: Some(cm),
            pm: Some(pm),
            refused,
            rt,
        }
    }

    fn cm(&self) -> &ConnMan {
        self.cm.as_ref().unwrap()
    }

    /// Live connections of `conn_type` that completed the handshake.
    fn connected(&self, conn_type: ConnectionType) -> Vec<PeerHandle> {
        self.core
            .peers()
            .iter()
            .filter(|p| {
                p.conn_type() == conn_type
                    && p.successfully_connected()
                    && !p.disconnect_requested()
            })
            .cloned()
            .collect()
    }

    /// Whether a handshaken connection of `conn_type` goes to `port` on 127.0.0.1.
    fn connected_to(&self, conn_type: ConnectionType, port: u16) -> bool {
        self.connected(conn_type).iter().any(|p| p.addr().service.port == port)
    }

    /// The `ConnMan` threads, then the processor.
    fn halt(&mut self) {
        if let Some(cm) = self.cm.take() {
            cm.interrupt();
            cm.stop_threads();
        }
        if let Some(pm) = self.pm.take() {
            pm.stop();
        }
    }

    /// The host's shutdown order: [`Node::halt`], then `StopNodes`. Every connection the node
    /// still has is on loopback.
    fn stop(mut self) {
        self.halt();
        for p in self.core.peers().iter() {
            let ip = p.addr().service.addr.to_ip();
            assert!(ip.is_some_and(|ip| ip.is_loopback()), "connection to {}", p.addr().service);
        }
        self.rt.block_on(self.core.shutdown());
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.halt();
    }
}

/// `-connect`: the node opens a manual connection to each given address and to nothing else.
#[test]
fn connect_only() {
    let (da, db) = (DataDir::new("connect-a"), DataDir::new("connect-b"));
    let b = Node::start(&db.0, |_| {});
    let a = Node::start(&da.0, |c| c.connect = Some(vec![loopback(b.port)]));
    wait_for("manual connection", || a.connected_to(ConnectionType::Manual, b.port));
    wait_for("inbound at b", || b.connected(ConnectionType::Inbound).len() == 1);
    let stats = a.cm().node_stats();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].addr_name, loopback(b.port));
    assert!(!stats[0].inbound);
    a.stop();
    b.stop();
}

/// `-addnode`: `addcon` keeps a manual connection to the added node.
#[test]
fn addnode() {
    let (da, db) = (DataDir::new("addnode-a"), DataDir::new("addnode-b"));
    let b = Node::start(&db.0, |_| {});
    let a = Node::start(&da.0, |c| c.added_nodes = vec![loopback(b.port)]);
    wait_for("manual connection", || a.connected_to(ConnectionType::Manual, b.port));
    wait_for("inbound at b", || b.connected(ConnectionType::Inbound).len() == 1);
    a.stop();
    b.stop();
}

/// `addconnection` holds an outbound slot: with one slot (`-maxconnections=1`), the first
/// block-relay connection takes it and a second is refused before any attempt.
#[test]
fn add_connection_needs_a_slot() {
    let (da, db, dc) =
        (DataDir::new("addconn-a"), DataDir::new("addconn-b"), DataDir::new("addconn-c"));
    let b = Node::start(&db.0, |_| {});
    let c = Node::start(&dc.0, |_| {});
    let a = Node::start(&da.0, |c| c.max_connections = 1);
    let add = |port| {
        a.rt.block_on(a.cm().add_connection(&loopback(port), ConnectionType::BlockRelay, false))
    };
    assert!(add(b.port));
    wait_for("block-relay connection", || a.connected_to(ConnectionType::BlockRelay, b.port));
    assert!(!add(c.port), "no free outbound slot");
    assert!(c.connected(ConnectionType::Inbound).is_empty());
    a.stop();
    b.stop();
    c.stop();
}

/// `-bind=…=onion`: a connection Tor forwards to the onion bind (a plain loopback socket here)
/// is an inbound onion peer, counted on the onion network (Core's `inbound_onion`, the
/// accepted socket's local address among `m_onion_binds`); one on the plain bind is not.
#[test]
fn onion_bind_marks_inbound_onion() {
    use dash_p2p::Network;

    let dir = DataDir::new("onion");
    // The onion bind needs a known port (Core compares the accepted socket's local address,
    // port included): take a free one, retrying if another socket grabs it first.
    let (rt, core, cm, onion_port) = (0..10)
        .find_map(|_| {
            let port = StdListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let Parts {
                rt,
                core,
                shared,
                ..
            } = build(&dir.0, 0, |_| {});
            let mut opts = conn_opts(0, &Arc::default());
            opts.binds.onion_binds =
                vec![Service::from_socket_addr(loopback(port).parse().unwrap())];
            let cm =
                ConnMan::start(Arc::clone(&core), rt.handle().clone(), shared.dial_policy(), opts);
            match cm {
                Ok(cm) => Some((rt, core, cm, port)),
                Err(_) => {
                    rt.block_on(core.shutdown());
                    None
                }
            }
        })
        .expect("a free port for the onion bind");
    let plain_port = cm.listen_addrs()[0].port();
    assert_eq!(cm.listen_addrs()[1].port(), onion_port);

    let _tor = std::net::TcpStream::connect(loopback(onion_port)).unwrap();
    let _clear = std::net::TcpStream::connect(loopback(plain_port)).unwrap();
    wait_for("two inbound peers", || core.peers().len() == 2);
    let stats = cm.node_stats();
    let by_bind = |port: u16| stats.iter().find(|s| s.addr_bind.port == port).unwrap();
    assert_eq!(by_bind(onion_port).network, Network::Onion);
    assert_ne!(by_bind(plain_port).network, Network::Onion);

    cm.interrupt();
    cm.stop_threads();
    rt.block_on(core.shutdown());
}

/// `-seednode`: an `ADDR_FETCH` connection asks for addresses, learns them and goes away.
#[test]
fn seednode_addr_fetch() {
    let (da, db) = (DataDir::new("seed-a"), DataDir::new("seed-b"));
    let b = Node::start(&db.0, |_| {});
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let known: Vec<Address> = (0..100u8)
        .map(|i| Address {
            service: Service::new(NetAddr::ipv4([31 + i / 4, 1 + i % 4, 1, 1]), 9999),
            services: ServiceFlags::NODE_NETWORK,
            time: now as u32,
        })
        .collect();
    b.core.addrman().add(&known, &NetAddr::ipv4([250, 1, 1, 1]), 0, now as i64);
    assert!(b.core.addrman().size(None, None) > 50);

    let a = Node::start(&da.0, |c| c.seed_nodes = vec![loopback(b.port)]);
    wait_for("addresses from the seed node", || a.core.addrman().size(None, None) > 1);
    wait_for("the addr-fetch connection to close", || {
        !a.core.peers().iter().any(|p| p.conn_type() == ConnectionType::AddrFetch)
    });
    // The learned addresses are routable: `opencon` picks them, and the boundary refuses them.
    wait_for("a refused routable pick", || !a.refused.lock().unwrap().is_empty());
    a.stop();
    b.stop();
}

/// `addconnection … feeler` completes its handshake and disconnects; `addnode … onetry` opens a
/// manual connection once.
#[test]
fn feeler_and_onetry() {
    let (da, db) = (DataDir::new("feeler-a"), DataDir::new("feeler-b"));
    let b = Node::start(&db.0, |_| {});
    let a = Node::start(&da.0, |_| {});
    let rt = a.rt.handle().clone();

    let feelers = |a: &Node| a.core.peers().iter().any(|p| p.conn_type() == ConnectionType::Feeler);
    assert!(rt.block_on(a.cm().add_connection(&loopback(b.port), ConnectionType::Feeler, false)));
    wait_for("the addconnection feeler to go", || !feelers(&a));
    // The same open with its handle kept (`addconnection` returns only `true`): the feeler goes
    // because its handshake ran, as Core disconnects a feeler on its `version`.
    let feeler = rt
        .block_on(a.core.open(OpenRequest {
            target: Target::Dest(loopback(b.port)),
            conn_type: ConnectionType::Feeler,
            count_failure: false,
            grant: None,
            use_v2: false,
            masternode: false,
            masternode_probe: false,
        }))
        .unwrap();
    wait_for("the feeler to go", || a.core.peer(feeler.id()).is_none());
    assert!(feeler.version() > 0, "the feeler got no version");

    let peer = rt.block_on(a.cm().open_one_try(&loopback(b.port), false)).unwrap();
    assert_eq!(peer.conn_type(), ConnectionType::Manual);
    wait_for("onetry handshake", || peer.successfully_connected());
    // A second try to the same name is refused while connected.
    assert!(rt.block_on(a.cm().open_one_try(&loopback(b.port), false)).is_err());
    a.stop();
    b.stop();
}

/// Core's `feature_anchors.py`: only block-relay-only peers go to `anchors.dat` at shutdown, the
/// file is removed when read, and a restart reconnects to an anchor as block-relay-only.
#[test]
fn anchors_round_trip() {
    let (da, db, dc) =
        (DataDir::new("anchors-a"), DataDir::new("anchors-b"), DataDir::new("anchors-c"));
    let b = Node::start(&db.0, |_| {});
    let pb = b.port;
    let dir_a = &da.0;
    let a = Node::start(dir_a, |_| {});
    let anchors = dir_a.join("anchors.dat");
    assert!(!anchors.exists());
    let c = Node::start(&dc.0, |c| c.added_nodes = vec![loopback(a.port)]);

    let rt = a.rt.handle().clone();
    assert!(rt.block_on(a.cm().add_connection(
        &loopback(b.port),
        ConnectionType::BlockRelay,
        false
    )));
    wait_for("block-relay peer", || a.connected_to(ConnectionType::BlockRelay, pb));
    wait_for("inbound from c", || a.connected(ConnectionType::Inbound).len() == 1);
    let inbound_port = a.connected(ConnectionType::Inbound)[0].addr().service.port;
    a.stop();

    // 127.0.0.1 and the big-endian port, as the Python test searches for them.
    let bytes = std::fs::read(&anchors).unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert!(hex.contains(&format!("7f000001{pb:04x}")));
    assert!(!hex.contains(&format!("7f000001{inbound_port:04x}")));

    // The anchor was written with services NONE (its `CAddress` from `addconnection`); give it
    // NODE_NETWORK and re-seal it, as the Python test does, so the restart may use it.
    let services_index = 4 + 1 + 4 + 4;
    assert_eq!(bytes[services_index], 0);
    let mut patched = bytes[..bytes.len() - 32].to_vec();
    patched[services_index] = ServiceFlags::NODE_NETWORK.0 as u8;
    let hash = dash_sha256::sha256d(&patched);
    patched.extend_from_slice(&hash);
    std::fs::write(&anchors, &patched).unwrap();

    let a = Node::start(dir_a, |_| {});
    assert!(!anchors.exists(), "read and removed at start");
    wait_for("the anchor reconnected", || a.connected_to(ConnectionType::BlockRelay, pb));
    a.stop();
    c.stop();
    b.stop();
}

/// A port another socket holds fails startup with Core's error, and leaves `anchors.dat` as it
/// was: Core returns from `Start` before `ReadAnchors` and `fAddressesInitialized`.
#[test]
fn bind_failure_is_fatal() {
    let held = StdListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let dir = DataDir::new("bindfail");
    let anchors = dir.0.join("anchors.dat");
    std::fs::write(&anchors, b"left alone").unwrap();
    let Parts {
        rt,
        core,
        shared,
        ..
    } = build(&dir.0, port, |_| {});
    // `LoadAddrman` writes a fresh `peers.dat` at once; a dump would replace the file (the
    // same bytes, so compare the inode).
    #[cfg(unix)]
    let inode = || {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir.0.join("peers.dat")).unwrap().ino()
    };
    #[cfg(unix)]
    let before = inode();
    let policy = shared.dial_policy();
    let Err(e) = ConnMan::start(
        Arc::clone(&core),
        rt.handle().clone(),
        policy,
        conn_opts(port, &Arc::default()),
    ) else {
        panic!("bind on a held port succeeded");
    };
    assert_eq!(e.to_string(), "Failed to listen on any port. Use -listen=0 if you want this.");
    assert!(core.interrupted());
    rt.block_on(core.shutdown());
    assert_eq!(std::fs::read(&anchors).unwrap(), b"left alone");
    #[cfg(unix)]
    assert_eq!(inode(), before, "no dump after a failed start");
    drop(held);
}
