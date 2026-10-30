//! The handshake: our `version`, the peer's `version` and `verack`, and `sendaddrv2` (P2a §3.3,
//! §3.3.7; v24 `net_processing.cpp:3935-4259`).
//!
//! The functions run Core's branch in Core's order. Where Core touches processor state (the
//! transaction-relay slot, `sendtxrcncl`, the preferred-download count, misbehaviour, MNAUTH,
//! `qwatch`) they call the processor through [`HandshakeHost`] at that point, so the order of
//! every pushed message is Core's whichever processor runs them. The node passes its
//! `PeerManager`; SPV passes a host whose defaults do nothing.

use dash_p2p_core::{
    CommandString, P2pDecodeError, ProtocolVersion, SendCmpct, SendDsq, ServiceFlags, UserAgent,
    Version, VersionAddr,
};
use dash_p2p_transport::OutMsg;
use dash_params::NetworkKind;
use dash_primitives::ServiceV1;
use dash_types::codec::BaseCodec;
use dash_types::{CompactSize, Numeric};

use crate::addr::{self, AddrRelay};
use crate::conn::{ConnectionType, PeerHandle, Why};
use crate::netaddr::Service;
use crate::netcore::NetCore;
use crate::ping::PingState;
use crate::policy::{Step, Throw};
use crate::rng::get_rand_hash;

/// Core's `MIN_PEER_PROTO_VERSION`.
const MIN_PEER_PROTO_VERSION: i32 = ProtocolVersion::MIN_PEER.0;
/// Core's `ADDRV2_PROTO_VERSION`.
const ADDRV2_PROTO_VERSION: i32 = ProtocolVersion::ADDRV2.0;
/// Core's `MAX_SUBVERSION_LENGTH`.
const MAX_SUBVERSION_LENGTH: u64 = 256;
/// Core's `CMPCTBLOCKS_VERSION`.
const CMPCTBLOCKS_VERSION: u64 = 1;

const VERSION: CommandString = CommandString::from_static("version");
const VERACK: CommandString = CommandString::from_static("verack");
const SENDADDRV2: CommandString = CommandString::from_static("sendaddrv2");
const GETADDR: CommandString = CommandString::from_static("getaddr");
const SENDHEADERS: CommandString = CommandString::from_static("sendheaders");
const SENDHEADERS2: CommandString = CommandString::from_static("sendheaders2");
const SENDCMPCT: CommandString = CommandString::from_static("sendcmpct");
const SENDDSQUEUE: CommandString = CommandString::from_static("senddsq");

/// Who we are on the wire (DEC-83: rdashd identifies as itself). Built once by the host; the
/// node's `fingerprint.rs` (P2a-core) builds it from the options.
#[derive(Clone, Debug)]
pub struct LocalIdentity {
    /// `PROTOCOL_VERSION` (70242 at v24, DEC-82): the cap of every common version.
    pub protocol_version: ProtocolVersion,
    /// The version we advertise: `-pushversion` on non-mainnet networks, else
    /// `protocol_version` (the common version always uses `protocol_version`).
    pub advertised_version: i32,
    /// Our user agent (at most 256 bytes).
    pub user_agent: UserAgent,
    /// Whether we run as a masternode (Core's `m_nodeman != nullptr`).
    pub masternode: bool,
}

/// What the processor supplies to our `version` (Core's `PushNodeVersion` inputs).
#[derive(Clone, Copy, Debug)]
pub struct VersionInputs {
    /// `peer->m_our_services`.
    pub our_services: ServiceFlags,
    /// `m_best_height`.
    pub best_height: i32,
    /// `!RejectIncomingTxs(node)`.
    pub relay: bool,
    /// `GetTime()`: mockable, not adjusted. The `version` handler also uses it for its other
    /// `GetTime()` reads (the time offset, `AddrMan::Good`).
    pub now: i64,
}

/// The fields of a peer's `version`, as Core's handler reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerVersion {
    /// `nVersion`.
    pub version: i32,
    /// `nServices`.
    pub services: ServiceFlags,
    /// `nTime`, with a negative value read as 0.
    pub time: i64,
    /// `addrMe`: our address as the peer sees it.
    pub addr_me: Service,
    /// `nNonce` (1 when absent).
    pub nonce: u64,
    /// `cleanSubVer`: the sanitized user agent.
    pub clean_subver: String,
    /// `starting_height` (−1 when absent).
    pub starting_height: i32,
    /// `fRelay` (true when absent).
    pub relay: bool,
}

/// The per-peer state the network core's functions keep for a processor, which embeds it in its
/// own per-peer state (Core keeps these fields in `Peer`).
#[derive(Debug)]
pub struct PeerNet {
    /// `m_starting_height` (−1 until the peer's `version`).
    pub starting_height: i32,
    /// Address relay (P2a §3.6.1).
    pub addr: AddrRelay,
    /// Ping (P2a §3.5).
    pub ping: PingState,
}

impl PeerNet {
    /// Core's `Peer` fields as `InitializeNode` creates them at `now_us` (`GetTime<µs>()`, the
    /// address token bucket's first stamp).
    pub fn new(now_us: i64) -> Self {
        Self {
            starting_height: -1,
            addr: AddrRelay::new(now_us),
            ping: PingState::default(),
        }
    }
}

/// The processor's state that Core's handshake branches touch, called at Core's points.
/// Every method has the default an SPV client needs. A host that pushes (MNAUTH, `qwatch`) holds
/// its own `Arc<NetCore>`. Time samples are the core's ([`NetCore::adjusted_time`]).
pub trait HandshakeHost {
    /// `Misbehaving(peer, score, msg)` (the devnet-name check).
    fn misbehaving(&mut self, score: i32, msg: &'static str) {
        let _ = (score, msg);
    }

    /// After `sendaddrv2` and the version fields are recorded, before `verack`: Core's
    /// `SetTxRelay` and `sendtxrcncl` (v24 `net_processing.cpp:4072-4101`). The core has already
    /// set [`Peer::relays_txs`](crate::Peer::relays_txs) where Core's `SetTxRelay` block does.
    fn version_recorded(&mut self, peer: &PeerHandle, v: &PeerVersion) {
        let _ = (peer, v);
    }

    /// After `verack` is pushed: Core's `fPreferredDownload` (v24 `:4106-4111`), which reads
    /// `CanServeBlocks` from [`Peer::version_services`](crate::Peer::version_services).
    fn verack_pushed(&mut self, peer: &PeerHandle) {
        let _ = peer;
    }

    /// In the `verack` branch, only when we are a masternode ([`LocalIdentity::masternode`]):
    /// push `mnauth` (`CMNAuth::PushMNAUTH`).
    fn push_mnauth(&mut self, peer: &PeerHandle) {
        let _ = peer;
    }

    /// Core's `RejectIncomingTxs(node)`.
    fn reject_incoming_txs(&self, peer: &PeerHandle) -> bool;

    /// Inside the `!RejectIncomingTxs` block after `senddsq`: push `qwatch` when watching and
    /// the peer is a quorum node.
    fn maybe_qwatch(&mut self, peer: &PeerHandle) {
        let _ = peer;
    }

    /// Txreconciliation's `verack` bookkeeping (off by default).
    fn txrcncl_verack(&mut self, peer: &PeerHandle) {
        let _ = peer;
    }
}

/// Core's `ProcessMessage` read stream (`CDataStream`) for the `version` payload: each read
/// throws Core's text where Core's would.
struct Stream<'a>(&'a [u8]);

impl Stream<'_> {
    fn read<T: BaseCodec>(&mut self) -> Result<T, Throw> {
        T::decode(&mut self.0).map_err(|e| Throw::from_decode(&P2pDecodeError::Consensus(e)))
    }

    /// `DataStream::ignore`.
    // Core: streams.h:298-311 DataStream::ignore
    fn ignore(&mut self, n: usize) -> Result<(), Throw> {
        if n > self.0.len() {
            return Err(Throw::ios_failure("DataStream::ignore(): end of data"));
        }
        self.0 = &self.0[n..];
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `LIMITED_STRING(s, limit)`: the size (range-checked against `MAX_SIZE`), refused above
    /// `limit` before any byte is read.
    // Core: serialize.h:773-790 LimitedStringFormatter::Unser
    fn read_limited_string(&mut self, limit: u64) -> Result<Vec<u8>, Throw> {
        let size = CompactSize::decode(&mut self.0)
            .map_err(|e| Throw::from_decode(&P2pDecodeError::Consensus(e)))?
            .to_base();
        if size > limit {
            return Err(Throw::ios_failure("String length limit exceeded"));
        }
        let size = size as usize;
        if size > self.0.len() {
            return Err(Throw::ios_failure("DataStream::read(): end of data"));
        }
        let (s, rest) = self.0.split_at(size);
        self.0 = rest;
        Ok(s.to_vec())
    }
}

/// Core's `SanitizeString(str)` with `SAFE_CHARS_DEFAULT`.
// Core: util/strencodings.cpp:28-37 SanitizeString
fn sanitize_string(s: &[u8]) -> String {
    s.iter()
        .filter(|&&b| b.is_ascii_alphanumeric() || b" .,;-_/:?@()".contains(&b))
        .map(|&b| char::from(b))
        .collect()
}

fn service_v1(s: &Service) -> ServiceV1 {
    ServiceV1 {
        addr: dash_primitives::AddrV1(s.addr.v1_bytes()),
        port: s.port,
    }
}

fn encode(msg: &impl BaseCodec) -> Vec<u8> {
    let mut buf = Vec::new();
    msg.encode(&mut buf);
    buf
}

/// Core's `", mapped_as=%d"` log suffix (empty without an asmap).
fn mapped_as_suffix(core: &NetCore, peer: &PeerHandle) -> String {
    match core.netgroup().mapped_as(&peer.addr().service.addr) {
        0 => String::new(),
        asn => format!(", mapped_as={asn}"),
    }
}

/// Core's `CConnman::CheckIncomingNonce`: false when the nonce is one of our outbound
/// connections' still in their handshake.
// Core: net.cpp:348-356 CConnman::CheckIncomingNonce
fn check_incoming_nonce(core: &NetCore, nonce: u64) -> bool {
    !core
        .peers()
        .iter()
        .any(|p| !p.successfully_connected() && !p.is_inbound() && p.local_nonce() == nonce)
}

/// Core's `PushNodeVersion`: our `version`, with a new MNAUTH challenge for the peer (32 bytes of
/// [`NetCore::rand`], kept as its `sent_mnauth_challenge`) and its `nLocalHostNonce`. `addr_you`
/// is zeroed unless the peer's address is routable, not a proxy ([`NetCore::is_proxy`](crate::NetCore::is_proxy)) and
/// v1-compatible. The caller pushes the message (`InitializeNode` for outbound peers;
/// [`on_version`] for inbound ones).
// Core: net_processing.cpp:1561-1593 PeerManagerImpl::PushNodeVersion
pub fn version_for(
    core: &NetCore,
    peer: &PeerHandle,
    id: &LocalIdentity,
    inputs: &VersionInputs,
) -> OutMsg {
    let cfg = core.config();
    let my_services = inputs.our_services;
    let n_time = inputs.now;
    let nonce = peer.local_nonce();
    let starting_height = inputs.best_height;
    let addr = peer.addr();
    let ip = &addr.service.addr;
    let addr_you = if ip.is_routable(cfg.allow_private_net)
        && !core.is_proxy(ip)
        && ip.is_addr_v1_compatible()
    {
        addr.service.clone()
    } else {
        Service::default()
    };
    let your_services = addr.services;
    let challenge = get_rand_hash(core.rand().as_ref());
    peer.set_sent_mnauth_challenge(challenge);
    // `-pushversion` is the host's choice of `advertised_version` (non-mainnet only).
    let protocol_version = id.advertised_version;
    let tx_relay = inputs.relay;
    let version = Version {
        protocol_version: ProtocolVersion(protocol_version),
        services: my_services,
        timestamp: n_time,
        addr_recv: VersionAddr {
            services: your_services,
            addr: service_v1(&addr_you),
        },
        addr_send: Some(VersionAddr {
            services: my_services,
            addr: service_v1(&Service::default()),
        }),
        nonce: Some(nonce),
        user_agent: Some(id.user_agent.clone()),
        start_height: Some(starting_height),
        relay: Some(tx_relay),
        mnauth_challenge: Some(Numeric::from_lendian(challenge)),
        mn_connection: Some(peer.masternode_connection()),
    };
    tracing::debug!(
        target: "net",
        "send version message: version {protocol_version}, blocks={starting_height}, txrelay={}, peer={}",
        u8::from(tx_relay),
        peer.id()
    );
    OutMsg::new(VERSION, encode(&version))
}

/// Core's `version` branch from the redundancy check to the feeler disconnect, reading `payload`
/// at Core's read points. `inputs` builds the `version` Core pushes to inbound peers inside this
/// branch. A read that throws returns [`Step::Throw`] with every effect before it kept, as in
/// Core (the head checks, `SetServices`, a received MNAUTH challenge).
// Core: net_processing.cpp:3935-4176 ProcessMessage (VERSION)
pub fn on_version(
    core: &NetCore,
    peer: &PeerHandle,
    id: &LocalIdentity,
    inputs: &VersionInputs,
    net: &mut PeerNet,
    payload: &[u8],
    host: &mut dyn HandshakeHost,
) -> Step {
    let pid = peer.id();
    if peer.version() != 0 {
        tracing::debug!(target: "net", "redundant version message from peer={pid}");
        return Step::Ignored;
    }
    let cfg = core.config();
    let inbound = peer.is_inbound();

    let mut s = Stream(payload);
    let mut nonce: u64 = 1;
    let mut clean_subver = String::new();
    let mut starting_height: i32 = -1;
    let mut relay = true;

    macro_rules! read {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(t) => return Step::Throw(t),
            }
        };
    }

    let version: i32 = read!(s.read());
    let services: ServiceFlags = read!(s.read());
    let mut time: i64 = read!(s.read());
    if time < 0 {
        time = 0;
    }
    read!(s.ignore(8)); // Ignore the addrMe service bits sent by the peer.
    let addr_me = Service::from_v1(&read!(s.read::<ServiceV1>()));
    if !inbound {
        // Overwrites potentially existing services. In contrast to this, unvalidated services
        // received via gossip relay in ADDR/ADDRV2 messages are only ever added but cannot
        // replace existing ones.
        core.addrman().set_services(&peer.addr().service, services);
    }
    if peer.conn_type().expects_services() && !core.has_all_desirable_services(services) {
        tracing::debug!(
            target: "net",
            "peer={pid} does not offer the expected services ({:08x} offered, {:08x} expected); disconnecting",
            services.0,
            core.desirable_services(services).0
        );
        core.disconnect(pid, Why::Handshake("does not offer the expected services"));
        return Step::Disconnect;
    }

    if version < MIN_PEER_PROTO_VERSION {
        // Disconnect from peers older than this proto version.
        tracing::debug!(
            target: "net",
            "peer={pid} using obsolete version {version}; disconnecting"
        );
        core.disconnect(pid, Why::Handshake("obsolete version"));
        return Step::Disconnect;
    }

    if !s.is_empty() {
        // The version message includes information about the sending node which we don't use:
        // 8 bytes (service bits), 16 bytes (ipv6 address), 2 bytes (port).
        read!(s.ignore(26));
        nonce = read!(s.read());
    }
    if !s.is_empty() {
        let sub_ver = read!(s.read_limited_string(MAX_SUBVERSION_LENGTH));
        clean_subver = sanitize_string(&sub_ver);
    }
    if !s.is_empty() {
        starting_height = read!(s.read());
    }
    if !s.is_empty() {
        relay = read!(s.read::<bool>());
    }
    if !s.is_empty() {
        let challenge: [u8; 32] = read!(s.read());
        peer.set_received_mnauth_challenge(challenge);
    }
    if !s.is_empty() {
        let other_masternode = read!(s.read::<bool>());
        if inbound {
            peer.set_masternode_connection(other_masternode);
            if other_masternode {
                tracing::debug!(
                    target: "net",
                    "peer={pid} is an inbound masternode connection, not relaying anything to it"
                );
                if !id.masternode {
                    tracing::debug!(target: "net", "but we're not a masternode, disconnecting");
                    core.disconnect(pid, Why::Handshake("inbound masternode connection"));
                    return Step::Disconnect;
                }
            }
        }
    }
    // Disconnect if we connected to ourself.
    if inbound && !check_incoming_nonce(core, nonce) {
        tracing::info!(
            target: "net",
            "connected to self at {}, disconnecting",
            peer.addr().service
        );
        core.disconnect(pid, Why::Handshake("connected to self"));
        return Step::Disconnect;
    }

    if inbound && addr_me.addr.is_routable(cfg.allow_private_net) {
        core.seen_local(&addr_me);
    }

    // Be shy and don't send version until we hear.
    if inbound {
        core.push(peer, version_for(core, peer, id, inputs));
    }

    if cfg.params.network == NetworkKind::Devnet {
        let devnet = cfg.params.devnet_name.as_deref().unwrap_or("devnet");
        if !clean_subver.contains(&format!("devnet.{devnet}")) {
            tracing::info!(
                target: "net",
                "connected to wrong devnet. Reported version is {clean_subver}, expected devnet name is {devnet}"
            );
            // Don't try to connect again; whoever connected might just have made a mistake,
            // don't ban him immediately.
            host.misbehaving(
                if inbound {
                    1
                } else {
                    100
                },
                "wrong-devnet-name",
            );
            core.disconnect(pid, Why::Handshake("wrong devnet"));
            return Step::Disconnect;
        }
    }

    // Change version.
    let greatest_common_version = version.min(id.protocol_version.0);
    // `SetCommonVersion`, `nVersion`, and below `m_version_services`, `m_version_relay_txs` and
    // `cleanSubVer`: nothing reads them between the two points.
    peer.record_version(
        version,
        ProtocolVersion(greatest_common_version),
        services,
        relay,
        clean_subver.clone(),
    );

    // Signal ADDRv2 support (BIP155).
    if greatest_common_version >= ADDRV2_PROTO_VERSION {
        // BIP155 defines addrv2 and sendaddrv2 for all protocol versions, but some
        // implementations reject messages they don't know. As a courtesy, don't send it to nodes
        // with a version before ADDRV2_PROTO_VERSION.
        core.push(peer, OutMsg::new(SENDADDRV2, Vec::new()));
    }

    peer.set_has_all_wanted_services(core.has_all_desirable_services(services));
    // Core: net.cpp:569-577 CNode::SetAddrLocal
    match peer.addr_local() {
        Some(local) if local.addr.is_valid() => tracing::error!(
            target: "net",
            "Addr local already set for node: {pid}. Refusing to change from {local} to {addr_me}"
        ),
        _ => peer.set_addr_local(addr_me.clone()),
    }
    net.starting_height = starting_height;

    // `SetTxRelay` (for `fRelay` or our `NODE_BLOOM`) is the host's; inside it Core sets the
    // connection's `m_relays_txs` when `fRelay`.
    let conn_type = peer.conn_type();
    if conn_type != ConnectionType::BlockRelay && conn_type != ConnectionType::Feeler && relay {
        peer.set_relays_txs(true);
    }
    let v = PeerVersion {
        version,
        services,
        time,
        addr_me: addr_me.clone(),
        nonce,
        clean_subver: clean_subver.clone(),
        starting_height,
        relay,
    };
    host.version_recorded(peer, &v);

    core.push(peer, OutMsg::new(VERACK, Vec::new()));

    // Potentially mark this peer as a preferred download peer.
    host.verack_pushed(peer);

    // Attempt to initialize address relay for outbound peers and use result to decide whether to
    // send GETADDR, so that we don't send it to inbound or outbound block-relay-only peers.
    let send_getaddr = !inbound && addr::setup_address_relay(core, peer, &mut net.addr);
    if send_getaddr {
        // Do a one-time address fetch to help populate/update our addrman.
        core.push(peer, OutMsg::new(GETADDR, Vec::new()));
        net.addr.getaddr_sent = true;
        // When requesting a getaddr, accept an additional MAX_ADDR_TO_SEND addresses in response
        // (bypassing the MAX_ADDR_PROCESSING_TOKEN_BUCKET limit).
        net.addr.token_bucket += addr::MAX_ADDR_TO_SEND as f64;
    }

    if !inbound {
        // For non-inbound connections, record connection success in addrman (moves the address
        // from New to Tried, resolves tried-table collisions).
        core.addrman().good(&peer.addr().service, inputs.now);
    }

    // `fLogIPs` (`-logips`) is not an option of the core: no `, peeraddr=` part.
    tracing::debug!(
        target: "net",
        "receive version message: {clean_subver}: version {version}, blocks={starting_height}, us={addr_me}, txrelay={}, peer={pid}{}",
        u8::from(relay),
        mapped_as_suffix(core, peer)
    );

    let time_offset = time - inputs.now;
    peer.set_time_offset(time_offset);
    if !inbound {
        // Don't use timedata samples from inbound peers to make it harder for others to tamper
        // with our adjusted time.
        core.add_time_data(&peer.addr().service.addr, time_offset);
    }

    // Feeler connections exist only to verify if address is online.
    if conn_type == ConnectionType::Feeler {
        tracing::debug!(target: "net", "feeler connection completed peer={pid}; disconnecting");
        core.disconnect(pid, Why::Processor("feeler connection completed"));
        return Step::Disconnect;
    }
    Step::Done
}

/// Core's `verack` branch.
// Core: net_processing.cpp:4187-4259 ProcessMessage (VERACK)
pub fn on_verack(
    core: &NetCore,
    peer: &PeerHandle,
    id: &LocalIdentity,
    net: &PeerNet,
    host: &mut dyn HandshakeHost,
) -> Step {
    let pid = peer.id();
    if peer.successfully_connected() {
        tracing::debug!(target: "net", "ignoring redundant verack message from peer={pid}");
        return Step::Ignored;
    }

    // Log successful connections unconditionally for outbound, but not for inbound as those can
    // be triggered by an attacker at high rate (Core: only with `-debug=net`).
    let line = format!(
        "New {} {} peer connected: version: {}, blocks={}, peer={pid}{}",
        peer.conn_type().as_str(),
        peer.transport_info().kind.as_str(),
        peer.version(),
        net.starting_height,
        mapped_as_suffix(core, peer)
    );
    if peer.is_inbound() {
        tracing::debug!(target: "net", "{line}");
    } else {
        tracing::info!(target: "net", "{line}");
    }

    if id.masternode && !peer.masternode_probe() {
        host.push_mnauth(peer);
    }

    // Tell our peer we prefer to receive headers rather than inv's (`UsesCompressedHeaders`:
    // `m_their_services & NODE_HEADERS_COMPRESSED`, v24 `:1196-1199`).
    let compressed = peer.version_services().0 & ServiceFlags::NODE_HEADERS_COMPRESSED.0 != 0;
    core.push(
        peer,
        OutMsg::new(
            if compressed {
                SENDHEADERS2
            } else {
                SENDHEADERS
            },
            Vec::new(),
        ),
    );

    if peer.can_relay() {
        // Tell our peer we are willing to provide version 1 cmpctblocks.
        let msg = SendCmpct {
            high_bandwidth: false,
            version: CMPCTBLOCKS_VERSION,
        };
        core.push(peer, OutMsg::new(SENDCMPCT, encode(&msg)));
    }

    if !host.reject_incoming_txs(peer) {
        // Tell our peer that he should send us CoinJoin queue messages.
        let msg = SendDsq {
            send_dsq: true,
        };
        core.push(peer, OutMsg::new(SENDDSQUEUE, encode(&msg)));
        // Tell our peer that he should send us intra-quorum messages.
        host.maybe_qwatch(peer);
    }

    host.txrcncl_verack(peer);

    peer.set_successfully_connected();
    Step::Done
}

/// Core's `sendaddrv2` branch, before or after `verack`.
// Core: net_processing.cpp:4292-4305 ProcessMessage (SENDADDRV2)
pub fn on_sendaddrv2(core: &NetCore, peer: &PeerHandle, net: &mut PeerNet) -> Step {
    if peer.common_version().0 < ADDRV2_PROTO_VERSION {
        // Ignore previous implementations.
        return Step::Ignored;
    }
    if peer.successfully_connected() {
        // Disconnect peers that send a SENDADDRV2 message after VERACK.
        tracing::debug!(
            target: "net",
            "sendaddrv2 received after verack from peer={}; disconnecting",
            peer.id()
        );
        core.disconnect(peer.id(), Why::Handshake("sendaddrv2 received after verack"));
        return Step::Disconnect;
    }
    net.addr.wants_addrv2 = true;
    Step::Done
}
