//! Handshake transcripts between two `NetCore`s over a `MemSocket` pair, driven as Core's
//! message handler drives them: one message per peer, then `SendMessages` (here only
//! `MaybeSendPing`). Each side records the commands it receives, which are the commands the
//! other side pushed, in order (v24.0.0-rc.3 `net_processing.cpp:3918-4310`, P2a §3.3.6).
//! Then the gates (P2a §3.2.1), the `version` read points and Core's `net_tests` version cases.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dash_clock::{Clock, MockClock};
use dash_p2p::conn::{ConnectionType, Permissions};
use dash_p2p::gates::{self, Gate};
use dash_p2p::handshake::{
    self, HandshakeHost, LocalIdentity, PeerNet, PeerVersion, VersionInputs,
};
use dash_p2p::rng::StreamRand;
use dash_p2p::store::NoStore;
use dash_p2p::{
    Address, Attach, CommandString, MemSocket, NetAddr, NetConfig, NetCore, NetEvents, OutMsg,
    PeerHandle, ProtocolVersion, Resolver, Service, ServiceFlags, Step, ping,
};
use dash_p2p_core::UserAgent;
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};

const NOW: i64 = 1_700_000_000;
/// `NODE_NETWORK | NODE_BLOOM | NODE_NETWORK_LIMITED | NODE_HEADERS_COMPRESSED` (0x0C05).
const SERVICES: ServiceFlags = ServiceFlags(0x0C05);

struct Events;

impl NetEvents for Events {
    fn wake(&self) {}

    fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {}

    fn finalize(&self, _core: &NetCore, _peer: &PeerHandle) {}
}

struct NoResolver;

impl Resolver for NoResolver {
    fn resolve<'a>(
        &'a self,
        _host: &'a str,
        _max: usize,
        _allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
        Box::pin(async { Vec::new() })
    }
}

fn regtest() -> Arc<ChainParams> {
    Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap())
}

fn new_core(
    params: Arc<ChainParams>,
    clock: &Arc<MockClock>,
    f: impl FnOnce(&mut NetConfig),
) -> Arc<NetCore> {
    core_from(config(params, f), clock)
}

fn config(params: Arc<ChainParams>, f: impl FnOnce(&mut NetConfig)) -> NetConfig {
    let mut cfg = NetConfig::new(params, ServiceFlags::NODE_NETWORK);
    cfg.deterministic = true;
    cfg.peer_timeout = 3600;
    cfg.local_services = SERVICES;
    f(&mut cfg);
    cfg
}

fn core_from(cfg: NetConfig, clock: &Arc<MockClock>) -> Arc<NetCore> {
    NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        Arc::new(NoResolver),
        clock.clone(),
        Arc::new(Events),
    )
    .unwrap()
}

fn clock() -> Arc<MockClock> {
    Arc::new(MockClock::at(NOW))
}

fn address(ip: [u8; 4], port: u16) -> Address {
    Address {
        service: Service::new(NetAddr::ipv4(ip), port),
        services: ServiceFlags::NODE_NETWORK,
        time: 0,
    }
}

struct PeerOpts {
    conn_type: ConnectionType,
    use_v2: bool,
    masternode: bool,
    probe: bool,
    addr: Address,
}

impl PeerOpts {
    fn new(conn_type: ConnectionType) -> Self {
        Self {
            conn_type,
            use_v2: false,
            masternode: false,
            probe: false,
            addr: address([1, 2, 3, 4], 19899),
        }
    }
}

fn attach(core: &NetCore, sock: MemSocket, o: &PeerOpts) -> PeerHandle {
    core.attach(Attach {
        sock: Arc::new(sock),
        addr: o.addr.clone(),
        addr_bind: Service::default(),
        dest: None,
        conn_type: o.conn_type,
        inbound_onion: false,
        permissions: Permissions::NONE,
        use_v2: o.use_v2,
        grant: None,
        masternode: o.masternode,
        masternode_probe: o.probe,
    })
}

fn identity(masternode: bool) -> LocalIdentity {
    LocalIdentity {
        protocol_version: ProtocolVersion::V24_0,
        advertised_version: ProtocolVersion::V24_0.0,
        user_agent: UserAgent::new(b"/Dash Core:24.0.0/".to_vec()).unwrap(),
        masternode,
    }
}

/// A host that records its hooks and pushes a marker `mnauth` (empty payload) when asked.
#[derive(Default)]
struct TestHost {
    core: Option<Arc<NetCore>>,
    blocks_only: bool,
    calls: Vec<String>,
}

impl HandshakeHost for TestHost {
    fn misbehaving(&mut self, score: i32, msg: &'static str) {
        self.calls.push(format!("misbehaving {score} {msg}"));
    }

    fn version_recorded(&mut self, _peer: &PeerHandle, v: &PeerVersion) {
        self.calls.push(format!("version_recorded {}", v.version));
    }

    fn verack_pushed(&mut self, _peer: &PeerHandle) {
        self.calls.push("verack_pushed".into());
    }

    fn push_mnauth(&mut self, peer: &PeerHandle) {
        self.calls.push("push_mnauth".into());
        if let Some(core) = &self.core {
            core.push(peer, OutMsg::new(CommandString::from_static("mnauth"), Vec::new()));
        }
    }

    // Core: net_processing.cpp:6273-6281 RejectIncomingTxs
    fn reject_incoming_txs(&self, peer: &PeerHandle) -> bool {
        matches!(peer.conn_type(), ConnectionType::BlockRelay | ConnectionType::Feeler)
            || (self.blocks_only && !peer.has_permission(Permissions::RELAY))
    }

    fn maybe_qwatch(&mut self, _peer: &PeerHandle) {
        self.calls.push("maybe_qwatch".into());
    }

    fn txrcncl_verack(&mut self, _peer: &PeerHandle) {
        self.calls.push("txrcncl_verack".into());
    }
}

/// One end of a connection and the processor state the driver keeps for it.
struct Side {
    core: Arc<NetCore>,
    clock: Arc<MockClock>,
    peer: PeerHandle,
    net: PeerNet,
    id: LocalIdentity,
    host: TestHost,
    /// Commands received, in order: what the other side pushed.
    got: Vec<String>,
    /// Steps of `version`, `verack`, `sendaddrv2`, `ping` and `pong`, by command.
    steps: Vec<(String, Step)>,
    gates: Vec<(String, Gate)>,
    /// A scripted peer runs no handler: it answers `version` with `sendaddrv2` and `verack`.
    scripted: bool,
}

impl Side {
    fn new(
        core: Arc<NetCore>,
        clock: &Arc<MockClock>,
        peer: PeerHandle,
        id: LocalIdentity,
    ) -> Self {
        let host = TestHost {
            core: Some(core.clone()),
            ..TestHost::default()
        };
        Self {
            net: PeerNet::new(clock.node_now_micros()),
            core,
            clock: clock.clone(),
            peer,
            id,
            host,
            got: Vec::new(),
            steps: Vec::new(),
            gates: Vec::new(),
            scripted: false,
        }
    }

    fn inputs(&self) -> VersionInputs {
        VersionInputs {
            our_services: self.core.config().local_services,
            best_height: 0,
            relay: !self.host.reject_incoming_txs(&self.peer),
            now: self.clock.node_now().0,
        }
    }

    /// `InitializeNode` for an outbound peer: push our `version`.
    fn send_version(&self) {
        let v = handshake::version_for(&self.core, &self.peer, &self.id, &self.inputs());
        self.core.push(&self.peer, v);
    }

    /// One `ProcessMessages` (one message) then `SendMessages`. Returns whether a message was
    /// taken.
    fn step(&mut self) -> bool {
        let Some(polled) = self.core.pop(&self.peer) else {
            return false;
        };
        let msg = polled.msg;
        let cmd = msg.cmd.as_str().to_owned();
        self.got.push(cmd.clone());
        if self.peer.disconnect_requested() {
            // Core never processes a disconnecting peer.
            return true;
        }
        if self.scripted {
            if cmd == "version" {
                for c in ["sendaddrv2", "verack"] {
                    self.core.push(&self.peer, OutMsg::new(CommandString::from_static(c), vec![]));
                }
            }
            return true;
        }
        let reject = self.host.reject_incoming_txs(&self.peer);
        let gate = gates::admit(&self.peer, &msg.cmd, self.clock.node_now().0, reject);
        self.gates.push((cmd.clone(), gate));
        if gate == Gate::Admit {
            let (core, peer) = (&self.core, &self.peer);
            let step = match cmd.as_str() {
                "version" => Some(handshake::on_version(
                    core,
                    peer,
                    &self.id,
                    &self.inputs(),
                    &mut self.net,
                    &msg.payload,
                    &mut self.host,
                )),
                "verack" => {
                    Some(handshake::on_verack(core, peer, &self.id, &self.net, &mut self.host))
                }
                "sendaddrv2" => Some(handshake::on_sendaddrv2(core, peer, &mut self.net)),
                "ping" => Some(ping::on_ping(core, peer, &msg.payload)),
                "pong" => Some(ping::on_pong(peer, &mut self.net.ping, &msg.payload, msg.recv_us)),
                _ => None,
            };
            if let Some(step) = step {
                self.steps.push((cmd, step));
            }
        }
        // `SendMessages` returns early before `verack` and for a disconnecting peer.
        if self.peer.successfully_connected() && !self.peer.disconnect_requested() {
            ping::maybe_send_ping(
                &self.core,
                &self.peer,
                &mut self.net.ping,
                self.clock.node_now_micros(),
            );
        }
        true
    }
}

/// Steps both sides until neither receives anything for 100 ms.
async fn run(a: &mut Side, b: &mut Side) {
    let mut idle = 0;
    while idle < 50 {
        let busy = a.step() | b.step();
        idle = if busy {
            0
        } else {
            idle + 1
        };
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// An outbound side on core `a` and an inbound side on core `b`, connected by a `MemSocket` pair.
fn pair(
    out: PeerOpts,
    a_cfg: impl FnOnce(&mut NetConfig),
    b_cfg: impl FnOnce(&mut NetConfig),
) -> (Side, Side) {
    let clock = clock();
    let a = new_core(regtest(), &clock, a_cfg);
    // B draws from its own stream: on one stream both ends would take the same V2 keys and MNAUTH
    // challenge, and a mix-up of ours with theirs would pass.
    let b_rand = Arc::new(StreamRand::from_seed(2));
    let b = core_from(config(regtest(), b_cfg).with_rand_source(b_rand), &clock);
    let (sa, sb) = MemSocket::pair();
    let inbound = PeerOpts {
        use_v2: out.use_v2,
        addr: address([5, 6, 7, 8], 40000),
        ..PeerOpts::new(ConnectionType::Inbound)
    };
    let pa = attach(&a, sa, &out);
    let pb = attach(&b, sb, &inbound);
    let a = Side::new(a, &clock, pa, identity(false));
    let b = Side::new(b, &clock, pb, identity(false));
    (a, b)
}

fn cmds(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

/// The full outbound transcript of P2a §3.3.6 for a non-masternode with default options:
/// `version`; `sendaddrv2`, `verack`, `getaddr`; `sendheaders2`, `sendcmpct`, `senddsq`;
/// `ping`; and the `pong` for the peer's ping. The inbound side is shy: its `version` follows
/// ours.
async fn full_relay(use_v2: bool) {
    let (mut a, mut b) = pair(
        PeerOpts {
            use_v2,
            ..PeerOpts::new(ConnectionType::OutboundFullRelay)
        },
        |_| {},
        |_| {},
    );
    a.send_version();
    run(&mut a, &mut b).await;
    assert_eq!(
        b.got,
        cmds(&[
            "version",
            "sendaddrv2",
            "verack",
            "getaddr",
            "sendheaders2",
            "sendcmpct",
            "senddsq",
            "ping",
            "pong"
        ])
    );
    assert_eq!(
        a.got,
        cmds(&[
            "version",
            "sendaddrv2",
            "verack",
            "sendheaders2",
            "sendcmpct",
            "senddsq",
            "ping",
            "pong"
        ])
    );
    for side in [&a, &b] {
        let p = &side.peer;
        assert!(p.successfully_connected());
        assert!(!p.disconnect_requested());
        assert_eq!(p.version(), 70242);
        assert_eq!(p.common_version(), ProtocolVersion::V24_0);
        assert_eq!(p.version_services(), SERVICES);
        assert!(p.has_all_wanted_services());
        assert_eq!(p.clean_subver(), "/Dash Core:24.0.0/");
        assert!(p.version_relay_txs());
        assert!(p.relays_txs());
        assert!(side.net.addr.wants_addrv2);
        assert_eq!(side.net.starting_height, 0);
        assert!(p.last_ping_us() >= 0 && p.min_ping_us() == p.last_ping_us());
        assert_eq!(side.net.ping.nonce_sent, 0);
        let kind = p.transport_info().kind.as_str();
        assert_eq!(
            kind,
            if use_v2 {
                "v2"
            } else {
                "v1"
            }
        );
        assert_eq!(
            side.host.calls,
            cmds(&["version_recorded 70242", "verack_pushed", "maybe_qwatch", "txrcncl_verack"])
        );
    }
    // Outbound only: address relay with the getaddr allowance; the inbound peer's waits for
    // its first addr, addrv2 or getaddr.
    assert!(a.net.addr.enabled && a.net.addr.getaddr_sent && a.net.addr.known.is_some());
    assert_eq!(a.net.addr.token_bucket, 1001.0);
    assert!(!b.net.addr.enabled && !b.net.addr.getaddr_sent);
    // Each side sees the other's nonce: B received A's `local_nonce` (checked by the self-connect
    // rule) and its MNAUTH challenge.
    assert_eq!(b.peer.received_mnauth_challenge(), a.peer.sent_mnauth_challenge());
    assert_eq!(a.peer.received_mnauth_challenge(), b.peer.sent_mnauth_challenge());
    assert_ne!(a.peer.sent_mnauth_challenge(), b.peer.sent_mnauth_challenge());
    // Outbound only: the time sample; both record the offset (same mock clock: 0).
    assert_eq!(a.peer.time_offset(), 0);
    assert_eq!(b.peer.time_offset(), 0);
    // The addr_you each side reports is the other's address as it sees it: B's view of A
    // (inbound, 5.6.7.8:40000) is routable, so A's `version` from B carries it.
    assert_eq!(a.peer.addr_local(), Some(Service::new(NetAddr::ipv4([5, 6, 7, 8]), 40000)));
    assert_eq!(b.peer.addr_local(), Some(Service::new(NetAddr::ipv4([1, 2, 3, 4]), 19899)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_full_relay_v1() {
    full_relay(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_full_relay_v2() {
    full_relay(true).await;
}

/// After `version`, each side hands the common version to its transport (Core's
/// `CNode::SetCommonVersion`, net.h:1041-1047), so a V2 connection sends short message IDs
/// (`V2Transport::SetMessageToSend`, net.cpp:1577-1591). A `ping` is then 3 (length) + 1 (header)
/// + 1 (ID) + 8 (nonce) + 16 (tag) = 29 bytes on the wire; the long format's 13-byte type made
/// it 41. V1 is the 24-byte header and the nonce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ping_after_version_has_cores_wire_size() {
    for (use_v2, wire) in [(false, 32), (true, 29)] {
        let (mut a, mut b) = pair(
            PeerOpts {
                use_v2,
                ..PeerOpts::new(ConnectionType::OutboundFullRelay)
            },
            |_| {},
            |_| {},
        );
        a.send_version();
        run(&mut a, &mut b).await;
        for side in [&a, &b] {
            assert!(side.peer.successfully_connected());
            let before = side.peer.send_bytes();
            side.core.push(&side.peer, OutMsg::new(CommandString::from_static("ping"), vec![7; 8]));
            // V1 writes the header and the payload separately, so wait for the whole message.
            let mut sent = before;
            for _ in 0..5000 {
                sent = side.peer.send_bytes();
                if sent - before >= wire {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert_eq!(sent - before, wire, "v2={use_v2} {:?}", side.peer.conn_type());
        }
    }
}

/// Block-relay-only: our `version` says `relay=0`, no `getaddr` (`SetupAddressRelay` is false)
/// and no `senddsq` (`RejectIncomingTxs`). The inbound side still sends its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_block_relay_only() {
    let (mut a, mut b) = pair(PeerOpts::new(ConnectionType::BlockRelay), |_| {}, |_| {});
    a.send_version();
    run(&mut a, &mut b).await;
    assert_eq!(
        b.got,
        cmds(&["version", "sendaddrv2", "verack", "sendheaders2", "sendcmpct", "ping", "pong"])
    );
    assert_eq!(
        a.got,
        cmds(&[
            "version",
            "sendaddrv2",
            "verack",
            "sendheaders2",
            "sendcmpct",
            "senddsq",
            "ping",
            "pong"
        ])
    );
    assert!(!b.peer.version_relay_txs());
    assert!(!b.peer.relays_txs());
    // A received relay=1 but keeps no tx relay with a block-relay-only peer.
    assert!(a.peer.version_relay_txs());
    assert!(!a.peer.relays_txs());
    assert!(!a.net.addr.enabled && !a.net.addr.getaddr_sent);
    assert_eq!(a.host.calls, cmds(&["version_recorded 70242", "verack_pushed", "txrcncl_verack"]));
}

/// Feeler: `version`, then `sendaddrv2`, `verack`, `getaddr`, and the disconnect
/// (`"feeler connection completed"`); the linger flushes the queued messages. The inbound end is
/// a raw socket, as the sweep drops a removed peer's receive queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_feeler() {
    let clock = clock();
    let core = new_core(regtest(), &clock, |_| {});
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, &PeerOpts::new(ConnectionType::Feeler));
    let mut a = Side::new(core, &clock, peer, identity(false));
    a.send_version();
    let magic = regtest().message_start;
    theirs.inject(
        &[
            v1_frame(magic, "version", &version_payload(70242, SERVICES.0, 99)),
            v1_frame(magic, "sendaddrv2", &[]),
            v1_frame(magic, "verack", &[]),
        ]
        .concat(),
    );
    let mut sent = Vec::new();
    until(|| {
        a.step();
        sent.extend(theirs.drain());
        v1_cmds(&sent).len() >= 4
    })
    .await;
    assert_eq!(v1_cmds(&sent), cmds(&["version", "sendaddrv2", "verack", "getaddr"]));
    assert_eq!(a.steps.first(), Some(&("version".to_owned(), Step::Disconnect)));
    assert!(a.peer.disconnect_requested());
    assert!(!a.peer.successfully_connected());
}

/// Masternode connection between two masternodes: each pushes `mnauth` first in its `verack`
/// branch, and neither sends `sendcmpct` (`CanRelay` is false on a non-IQR masternode
/// connection). Gate 6 records the first message after `verack` as `mnauth` on both sides.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn masternode_connection() {
    let (mut a, mut b) = pair(
        PeerOpts {
            masternode: true,
            ..PeerOpts::new(ConnectionType::OutboundFullRelay)
        },
        |_| {},
        |_| {},
    );
    a.id = identity(true);
    b.id = identity(true);
    a.send_version();
    run(&mut a, &mut b).await;
    assert_eq!(
        b.got,
        cmds(&[
            "version",
            "sendaddrv2",
            "verack",
            "getaddr",
            "mnauth",
            "sendheaders2",
            "senddsq",
            "ping",
            "pong"
        ])
    );
    assert_eq!(
        a.got,
        cmds(&[
            "version",
            "sendaddrv2",
            "verack",
            "mnauth",
            "sendheaders2",
            "senddsq",
            "ping",
            "pong"
        ])
    );
    assert!(b.peer.masternode_connection());
    assert!(a.peer.first_message_is_mnauth());
    assert!(a.peer.first_message_time() == NOW && b.peer.first_message_time() == NOW);
    // The outbound side's `getaddr` (version branch) precedes its `mnauth` (verack branch), so
    // the inbound side's first message after `verack` is `getaddr`.
    assert!(!b.peer.first_message_is_mnauth());
}

/// An inbound masternode connection to a node that is not a masternode: Core logs and
/// disconnects after reading `fMasternode`, before pushing its own `version`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_masternode_to_non_masternode() {
    let (mut a, mut b) = pair(
        PeerOpts {
            masternode: true,
            ..PeerOpts::new(ConnectionType::OutboundFullRelay)
        },
        |_| {},
        |_| {},
    );
    a.scripted = true;
    a.send_version();
    run(&mut a, &mut b).await;
    assert_eq!(b.got, cmds(&["version"]));
    assert!(a.got.is_empty(), "{:?}", a.got);
    assert_eq!(b.steps, vec![("version".to_owned(), Step::Disconnect)]);
    assert!(b.peer.masternode_connection());
    assert_eq!(b.peer.version(), 0);
}

/// Inbound side over V2 with a scripted outbound peer: version, sendaddrv2, verack, then the
/// `verack` branch's pushes and the first ping.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_v2_transcript() {
    let (mut a, mut b) = pair(
        PeerOpts {
            use_v2: true,
            ..PeerOpts::new(ConnectionType::OutboundFullRelay)
        },
        |_| {},
        |_| {},
    );
    a.scripted = true;
    a.send_version();
    run(&mut a, &mut b).await;
    assert_eq!(
        a.got,
        cmds(&["version", "sendaddrv2", "verack", "sendheaders2", "sendcmpct", "senddsq", "ping"])
    );
    assert!(b.peer.successfully_connected());
    assert_eq!(b.peer.transport_info().kind.as_str(), "v2");
    assert_ne!(b.net.ping.nonce_sent, 0);
}

/// Self-connection: an inbound `version` whose nonce is one of our outbound peers' still in its
/// handshake is `"connected to self at %s, disconnecting"`, before our `version` is pushed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_connect_disconnects() {
    let clock = clock();
    let core = new_core(regtest(), &clock, |_| {});
    let (sa, sb) = MemSocket::pair();
    let out = attach(&core, sa, &PeerOpts::new(ConnectionType::OutboundFullRelay));
    let inb = attach(
        &core,
        sb,
        &PeerOpts {
            addr: address([1, 2, 3, 4], 50000),
            ..PeerOpts::new(ConnectionType::Inbound)
        },
    );
    let mut a = Side::new(core.clone(), &clock, out, identity(false));
    let mut b = Side::new(core.clone(), &clock, inb, identity(false));
    a.scripted = true;
    a.send_version();
    run(&mut a, &mut b).await;
    assert_eq!(b.got, cmds(&["version"]));
    assert!(a.got.is_empty(), "{:?}", a.got);
    assert_eq!(b.steps, vec![("version".to_owned(), Step::Disconnect)]);
    assert!(b.peer.disconnect_requested());
    assert_eq!(b.peer.version(), 0);
}

/// A `version` below `MIN_PEER_PROTO_VERSION` (70221): `"peer=%d using obsolete version %i;
/// disconnecting"`, nothing pushed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_old_version_disconnects() {
    let (mut a, mut b) = pair(PeerOpts::new(ConnectionType::OutboundFullRelay), |_| {}, |_| {});
    a.scripted = true;
    a.id.advertised_version = 70220;
    a.send_version();
    run(&mut a, &mut b).await;
    assert_eq!(b.got, cmds(&["version"]));
    assert!(a.got.is_empty(), "{:?}", a.got);
    assert_eq!(b.steps, vec![("version".to_owned(), Step::Disconnect)]);
    assert_eq!(b.peer.version(), 0);
}

/// An outbound peer without the desirable services (`NODE_NETWORK`, or `NODE_NETWORK_LIMITED`
/// after IBD): `"peer=%d does not offer the expected services (%08x offered, %08x expected);
/// disconnecting"`. The inbound side has already answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_missing_desirable_services_disconnects() {
    let (mut a, mut b) = pair(
        PeerOpts::new(ConnectionType::OutboundFullRelay),
        |_| {},
        |cfg| cfg.local_services = ServiceFlags(ServiceFlags::NODE_NETWORK_LIMITED.0 | 0x0800),
    );
    a.send_version();
    run(&mut a, &mut b).await;
    assert!(a.got.starts_with(&cmds(&["version"])), "{:?}", a.got);
    assert_eq!(a.steps.first(), Some(&("version".to_owned(), Step::Disconnect)));
    assert_eq!(b.got, cmds(&["version"]));
    assert_eq!(a.peer.version(), 0);
    // After IBD, NODE_NETWORK_LIMITED is enough (`GetDesirableServiceFlags`).
    let (mut a, mut b) = pair(
        PeerOpts::new(ConnectionType::OutboundFullRelay),
        |_| {},
        |cfg| cfg.local_services = ServiceFlags(ServiceFlags::NODE_NETWORK_LIMITED.0 | 0x0800),
    );
    a.core.set_ibd_complete(true);
    a.send_version();
    run(&mut a, &mut b).await;
    assert!(a.peer.successfully_connected());
    // Manual connections are not checked (`ExpectServicesFromConn`).
    let (mut a, mut b) = pair(
        PeerOpts::new(ConnectionType::Manual),
        |_| {},
        |cfg| cfg.local_services = ServiceFlags::NONE,
    );
    a.send_version();
    run(&mut a, &mut b).await;
    assert!(a.peer.successfully_connected());
    assert!(!a.peer.has_all_wanted_services());
}

/// Devnet: a peer whose user agent lacks `devnet.<name>` is refused after our `version` is
/// pushed (+1 inbound, `"wrong-devnet-name"`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_devnet_name() {
    let params = Arc::new(
        ChainParamsBuilder::new(NetworkKind::Devnet, Release::V24_0)
            .devnet_name("sb6")
            .build()
            .unwrap(),
    );
    let clock = clock();
    let core = new_core(params.clone(), &clock, |_| {});
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, &PeerOpts::new(ConnectionType::Inbound));
    let mut b = Side::new(core, &clock, peer, identity(false));
    theirs.inject(&v1_frame(params.message_start, "version", &version_payload(70242, 5, 99)));
    let mut sent = Vec::new();
    until(|| {
        b.step();
        sent.extend(theirs.drain());
        !b.steps.is_empty() && !v1_cmds(&sent).is_empty()
    })
    .await;
    assert_eq!(v1_cmds(&sent), cmds(&["version"]));
    assert_eq!(b.host.calls, cmds(&["misbehaving 1 wrong-devnet-name"]));
    assert_eq!(b.steps, vec![("version".to_owned(), Step::Disconnect)]);
}

/// Polls `cond` every 2 ms for up to five seconds.
async fn until(mut cond: impl FnMut() -> bool) {
    for _ in 0..2500 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("condition not reached in 5 s");
}

/// A V1 frame.
fn v1_frame(magic: [u8; 4], cmd: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = magic.to_vec();
    out.extend_from_slice(CommandString::from_static(cmd).as_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&dash_sha256::sha256d(payload)[..4]);
    out.extend_from_slice(payload);
    out
}

/// The commands of the whole V1 frames in `bytes`.
fn v1_cmds(mut bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    while bytes.len() >= 24 {
        let len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        if bytes.len() < 24 + len {
            break;
        }
        let cmd = CommandString::from_bytes(bytes[4..16].try_into().unwrap());
        out.push(cmd.as_str().to_owned());
        bytes = &bytes[24 + len..];
    }
    out
}

/// A full v24 `version` payload with Core's fields.
fn version_payload(version: i32, services: u64, nonce: u64) -> Vec<u8> {
    let mut v = version_head(version, services, NOW, &Service::default());
    v.extend_from_slice(&services.to_le_bytes());
    v.extend_from_slice(&[0u8; 18]);
    v.extend_from_slice(&nonce.to_le_bytes());
    v.push(3);
    v.extend_from_slice(b"/x/");
    v.extend_from_slice(&0i32.to_le_bytes());
    v.push(1);
    v.extend_from_slice(&[0u8; 32]);
    v.push(0);
    v
}

// --- The version read points, the gates and Core's net_tests version cases --------------------

/// A peer on `core` whose other end the test drives by hand.
fn lone_peer(core: &NetCore, o: &PeerOpts) -> (PeerHandle, MemSocket) {
    let (ours, theirs) = MemSocket::pair();
    (attach(core, ours, o), theirs)
}

/// The fields Core reads unconditionally: version, services, time, addr_recv (46 bytes).
fn version_head(version: i32, services: u64, time: i64, addr_recv: &Service) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&version.to_le_bytes());
    v.extend_from_slice(&services.to_le_bytes());
    v.extend_from_slice(&time.to_le_bytes());
    v.extend_from_slice(&services.to_le_bytes());
    v.extend_from_slice(&addr_recv.addr.v1_bytes());
    v.extend_from_slice(&addr_recv.port.to_be_bytes());
    v
}

fn inputs(now: i64) -> VersionInputs {
    VersionInputs {
        our_services: SERVICES,
        best_height: 7,
        relay: true,
        now,
    }
}

fn throw_text(step: Step) -> String {
    match step {
        Step::Throw(t) => t.what.into_owned(),
        other => panic!("expected a throw, got {other:?}"),
    }
}

/// A failed read leaves the peer before `version` with Core's exception text: the head, the
/// `ignore(26)` (its own `DataStream::ignore` text), and `LIMITED_STRING`'s length check. A
/// throw in the tail keeps the head's effects and any field read before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_read_points() {
    let clock = clock();
    let core = new_core(regtest(), &clock, |_| {});
    let id = identity(false);
    let mut host = TestHost::default();
    let me = Service::new(NetAddr::ipv4([9, 9, 9, 9]), 1);
    let head = version_head(70242, 5, NOW, &me);

    let cases: Vec<(Vec<u8>, &str)> = vec![
        (head[..10].to_vec(), "DataStream::read(): end of data: iostream error"),
        (head[..24].to_vec(), "DataStream::ignore(): end of data: iostream error"),
        (head[..30].to_vec(), "DataStream::read(): end of data: iostream error"),
        ([&head[..], &[0u8; 10]].concat(), "DataStream::ignore(): end of data: iostream error"),
        ([&head[..], &[0u8; 30]].concat(), "DataStream::read(): end of data: iostream error"),
        (
            [&head[..], &[0u8; 34], &[0xFD, 0x01, 0x01]].concat(),
            "String length limit exceeded: iostream error",
        ),
        (
            [&head[..], &[0u8; 34], &[0xFD, 0x01, 0x00]].concat(),
            "non-canonical ReadCompactSize(): iostream error",
        ),
        (
            [&head[..], &[0u8; 34], &[0x05, b'a']].concat(),
            "DataStream::read(): end of data: iostream error",
        ),
    ];
    for (payload, what) in cases {
        let (peer, _theirs) = lone_peer(&core, &PeerOpts::new(ConnectionType::Inbound));
        let mut net = PeerNet::new(0);
        let step =
            handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &payload, &mut host);
        assert_eq!(throw_text(step), what, "payload {payload:02x?}");
        assert_eq!(peer.version(), 0);
    }

    // A tail cut inside fMasternode, after the MNAUTH challenge: the challenge is kept.
    let mut payload = head.clone();
    payload.extend_from_slice(&[0u8; 26]);
    payload.extend_from_slice(&42u64.to_le_bytes());
    payload.extend_from_slice(&[3, b'/', b'x', b'/']);
    payload.extend_from_slice(&5i32.to_le_bytes());
    payload.push(1);
    payload.extend_from_slice(&[0xAB; 32]);
    let (peer, _theirs) = lone_peer(&core, &PeerOpts::new(ConnectionType::Inbound));
    let mut net = PeerNet::new(0);
    let step =
        handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &payload, &mut host);
    assert_eq!(step, Step::Done, "an absent fMasternode is not a throw");
    assert_eq!(peer.received_mnauth_challenge(), [0xAB; 32]);
    assert_eq!(peer.clean_subver(), "/x/");
    assert_eq!(net.starting_height, 5);
}

/// A `version` with only the mandatory fields is accepted with Core's defaults (nonce 1, empty
/// user agent, height −1, relay true), and the subversion is sanitized.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_defaults_and_sanitize() {
    let clock = clock();
    let core = new_core(regtest(), &clock, |_| {});
    let id = identity(false);
    let me = Service::new(NetAddr::ipv4([9, 9, 9, 9]), 1);
    let mut host = TestHost::default();
    let (peer, _t) = lone_peer(&core, &PeerOpts::new(ConnectionType::Inbound));
    let mut net = PeerNet::new(0);
    let head = version_head(70242, 5, -5, &me);
    let step = handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &head, &mut host);
    assert_eq!(step, Step::Done);
    assert_eq!(net.starting_height, -1);
    assert!(peer.version_relay_txs());
    assert_eq!(peer.clean_subver(), "");
    // nTime < 0 reads as 0.
    assert_eq!(peer.time_offset(), -NOW);
    // A repeat is redundant.
    let step = handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &head, &mut host);
    assert_eq!(step, Step::Ignored);

    let (peer, _t) = lone_peer(&core, &PeerOpts::new(ConnectionType::Inbound));
    let mut payload = head.clone();
    payload.extend_from_slice(&[0u8; 26]);
    payload.extend_from_slice(&7u64.to_le_bytes());
    let ua = b"/Sat\x00oshi:<b>0.1</b>\n/";
    payload.push(ua.len() as u8);
    payload.extend_from_slice(ua);
    let step =
        handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &payload, &mut host);
    assert_eq!(step, Step::Done);
    assert_eq!(peer.clean_subver(), "/Satoshi:b0.1/b/");
}

/// Core's `net_tests/initial_advertise_from_version_message`, transcribed up to Core's
/// address choice: an outbound peer reports our address as 2.3.4.5:20002 in a `version` with
/// only the mandatory fields; after `verack`, the address we advertise to it is 2.3.4.5 on our
/// listen port 20001 (`GetLocalAddrForPeer`; `MaybeSendAddr` itself is the processor's).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_advertise_from_version_message() {
    let clock = clock();
    let core = new_core(regtest(), &clock, |cfg| cfg.listen_port = 20001);
    let id = identity(false);
    let mut host = TestHost::default();
    let peer_us = Service::new(NetAddr::ipv4([2, 3, 4, 5]), 20002);
    let (peer, _t) = lone_peer(
        &core,
        &PeerOpts {
            addr: address([1, 2, 3, 4], 8333),
            ..PeerOpts::new(ConnectionType::OutboundFullRelay)
        },
    );
    let mut net = PeerNet::new(0);
    let msg = version_head(70242, 1, 0, &peer_us);
    let step = handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &msg, &mut host);
    assert_eq!(step, Step::Done);
    assert_eq!(handshake::on_verack(&core, &peer, &id, &net, &mut host), Step::Done);
    assert!(peer.successfully_connected());
    assert_eq!(peer.addr_local(), Some(peer_us));
    assert_eq!(
        core.local_addr_for_peer(&peer),
        Some(Service::new(NetAddr::ipv4([2, 3, 4, 5]), 20001))
    );
    // AddTimeData took the outbound sample (time 0): the offset needs five samples.
    assert_eq!(peer.time_offset(), -NOW);
    assert_eq!(core.time_offset(), 0);
}

/// `version_for`: `addr_you` is the peer's address when routable, not a proxy and
/// v1-compatible, else `[::]:0`; the relay flag is `!RejectIncomingTxs`; the nonce is the peer's
/// `nLocalHostNonce`; the challenge is the one recorded as sent. 137 bytes with our user agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_for_fields() {
    let clock = clock();
    let id = identity(false);
    for (addr, proxied, expect_you) in
        [([1, 2, 3, 4], false, true), ([10, 0, 0, 1], false, false), ([1, 2, 3, 4], true, false)]
    {
        let core = new_core(regtest(), &clock, |cfg| {
            if proxied {
                cfg.proxies.set(
                    dash_p2p::Network::Ipv4,
                    dash_p2p::proxy::Proxy {
                        addr: dash_p2p::proxy::ProxyAddr::Service(Service::new(
                            NetAddr::ipv4([1, 2, 3, 4]),
                            9050,
                        )),
                        randomize_credentials: false,
                    },
                );
            }
        });
        let (peer, _t) = lone_peer(
            &core,
            &PeerOpts {
                addr: address(addr, 9999),
                ..PeerOpts::new(ConnectionType::BlockRelay)
            },
        );
        let mut inp = inputs(NOW);
        inp.relay = false;
        let msg = handshake::version_for(&core, &peer, &id, &inp);
        assert_eq!(msg.command.as_str(), "version");
        let p = &msg.payload;
        assert_eq!(p.len(), 137);
        assert_eq!(&p[0..4], &70242i32.to_le_bytes());
        assert_eq!(&p[4..12], &SERVICES.0.to_le_bytes());
        assert_eq!(&p[12..20], &NOW.to_le_bytes());
        assert_eq!(&p[20..28], &1u64.to_le_bytes(), "addr_recv services: AddrMan's belief");
        let you = Service::new(NetAddr::ipv4(addr), 9999);
        if expect_you {
            assert_eq!(&p[28..44], &you.addr.v1_bytes());
            assert_eq!(&p[44..46], &9999u16.to_be_bytes());
        } else {
            assert_eq!(&p[28..46], &[0u8; 18]);
        }
        assert_eq!(&p[46..54], &SERVICES.0.to_le_bytes());
        assert_eq!(&p[54..72], &[0u8; 18]);
        assert_eq!(&p[72..80], &peer.local_nonce().to_le_bytes());
        assert_eq!(p[80] as usize, 18);
        assert_eq!(&p[81..99], b"/Dash Core:24.0.0/");
        assert_eq!(&p[99..103], &7i32.to_le_bytes());
        assert_eq!(p[103], 0, "relay");
        assert_eq!(&p[104..136], &peer.sent_mnauth_challenge());
        assert_eq!(p[136], 0, "fMasternode");
    }
}

/// The gates in Core's order (P2a §3.2.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gates_in_core_order() {
    let clock = clock();
    let core = new_core(regtest(), &clock, |_| {});
    let id = identity(false);
    let mut host = TestHost::default();
    let c = CommandString::from_static;
    let me = Service::new(NetAddr::ipv4([9, 9, 9, 9]), 1);
    let (peer, _t) = lone_peer(&core, &PeerOpts::new(ConnectionType::Inbound));
    // Gate 2: anything but version before version.
    assert_eq!(gates::admit(&peer, &c("ping"), NOW, false), Gate::BeforeVersion);
    assert_eq!(gates::admit(&peer, &c("verack"), NOW, false), Gate::BeforeVersion);
    assert_eq!(gates::admit(&peer, &c("version"), NOW, false), Gate::Admit);
    let mut net = PeerNet::new(0);
    let head = version_head(70242, 5, NOW, &me);
    handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &head, &mut host);
    // Gate 1: a repeated version.
    assert_eq!(gates::admit(&peer, &c("version"), NOW, false), Gate::RedundantVersion);
    // Gates 3 and 4 admit before verack; gate 5 refuses the rest without a score.
    for cmd in ["verack", "sendheaders", "sendheaders2", "sendcmpct", "sendaddrv2", "sendtxrcncl"] {
        assert_eq!(gates::admit(&peer, &c(cmd), NOW, false), Gate::Admit, "{cmd}");
    }
    for cmd in ["ping", "addr", "mnauth", "tx", "nosuchcmd"] {
        assert_eq!(gates::admit(&peer, &c(cmd), NOW, false), Gate::BeforeVerack, "{cmd}");
    }
    assert_eq!(handshake::on_verack(&core, &peer, &id, &net, &mut host), Step::Done);
    assert_eq!(handshake::on_verack(&core, &peer, &id, &net, &mut host), Step::Ignored);
    // `sendaddrv2` after verack disconnects.
    assert_eq!(handshake::on_sendaddrv2(&core, &peer, &mut net), Step::Disconnect);
    assert!(!net.addr.wants_addrv2);

    // Gate 6 records the first message; gate 7 refuses the blocks-only set.
    let (peer, _t) = lone_peer(&core, &PeerOpts::new(ConnectionType::Inbound));
    let mut net = PeerNet::new(0);
    handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &head, &mut host);
    handshake::on_verack(&core, &peer, &id, &net, &mut host);
    assert_eq!(gates::admit(&peer, &c("ping"), NOW + 3, true), Gate::Admit);
    assert_eq!(peer.first_message_time(), NOW + 3);
    assert!(!peer.first_message_is_mnauth());
    assert_eq!(gates::admit(&peer, &c("mnauth"), NOW + 4, true), Gate::Admit);
    assert_eq!(peer.first_message_time(), NOW + 3, "only the first message counts");
    assert_eq!(gates::admit(&peer, &c("clsig"), NOW, true), Gate::Admit, "not in the set");
    assert_eq!(gates::admit(&peer, &c("tx"), NOW, false), Gate::Admit);
    assert!(!peer.disconnect_requested());
    let g = gates::admit(&peer, &c("tx"), NOW, true);
    assert_eq!(g, Gate::BlocksOnlyViolation);
    assert!(g.disconnects() && peer.disconnect_requested());

    // Gate 6 on a masternode probe whose first message is not mnauth.
    for (first, verdict) in [("ping", Gate::ProbeWithoutMnauth), ("mnauth", Gate::Admit)] {
        let (peer, _t) = lone_peer(
            &core,
            &PeerOpts {
                probe: true,
                masternode: true,
                ..PeerOpts::new(ConnectionType::OutboundFullRelay)
            },
        );
        let mut net = PeerNet::new(0);
        let head = version_head(70242, 5, NOW, &me);
        handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &head, &mut host);
        handshake::on_verack(&core, &peer, &id, &net, &mut host);
        assert_eq!(gates::admit(&peer, &c(first), NOW, false), verdict);
        assert_eq!(peer.disconnect_requested(), verdict == Gate::ProbeWithoutMnauth);
        assert_eq!(peer.first_message_is_mnauth(), first == "mnauth");
    }
}

/// `sendaddrv2` from a peer below `ADDRV2_PROTO_VERSION` is ignored; before verack it sets
/// `m_wants_addrv2`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sendaddrv2_rules() {
    let clock = clock();
    let core = new_core(regtest(), &clock, |_| {});
    let id = identity(false);
    let mut host = TestHost::default();
    let me = Service::new(NetAddr::ipv4([9, 9, 9, 9]), 1);
    let (peer, _t) = lone_peer(&core, &PeerOpts::new(ConnectionType::Inbound));
    let mut net = PeerNet::new(0);
    let head = version_head(70222, 5, NOW, &me);
    handshake::on_version(&core, &peer, &id, &inputs(NOW), &mut net, &head, &mut host);
    assert_eq!(peer.common_version().0, 70222);
    assert_eq!(handshake::on_sendaddrv2(&core, &peer, &mut net), Step::Ignored);
    assert!(!net.addr.wants_addrv2);
    handshake::on_verack(&core, &peer, &id, &net, &mut host);
    assert_eq!(handshake::on_sendaddrv2(&core, &peer, &mut net), Step::Ignored);
    assert!(!peer.disconnect_requested());
}
