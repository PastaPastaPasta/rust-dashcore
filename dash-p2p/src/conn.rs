//! The connection table's entries: Core's `CNode` net-level state (P2a §2.2, §2.4 `NodeHandle`).
//!
//! A [`PeerHandle`] is shared by the connection table, the peer's reader and writer tasks and the
//! processors. Fields Core reads across threads are atomics or small mutexes, as in Core; the
//! processor's own per-peer state (Core's `Peer` and `CNodeState`) is not here.

use core::fmt;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};
use std::time::Instant;

use dash_p2p_core::{CommandString, ProtocolVersion, ServiceFlags};
use dash_p2p_transport::{OutMsg, Transport, TransportError, TransportInfo, sanitize_command};
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::dial::Grant;
use crate::netaddr::{Address, Network, Service};
use crate::netcore::{Polled, RawMsg};
use crate::sock::NetSocket;

/// Core's `INIT_PROTO_VERSION`: the version messages are framed at before `version`.
pub const INIT_PROTO_VERSION: i32 = 209;

/// Core's `NodeId` (`int64_t`): one counter shared by outbound and inbound connections, from 0.
pub type PeerId = i64;

/// Core's `ConnectionType` (`node/connection_types.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConnectionType {
    /// `INBOUND`.
    Inbound,
    /// `OUTBOUND_FULL_RELAY`.
    OutboundFullRelay,
    /// `MANUAL` (`-connect`, `-addnode`, `addnode` RPC).
    Manual,
    /// `FEELER`.
    Feeler,
    /// `BLOCK_RELAY`.
    BlockRelay,
    /// `ADDR_FETCH`.
    AddrFetch,
}

impl ConnectionType {
    /// Core's `ConnectionTypeAsString`.
    // Core: node/connection_types.cpp:11-22 ConnectionTypeAsString
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::OutboundFullRelay => "outbound-full-relay",
            Self::Manual => "manual",
            Self::Feeler => "feeler",
            Self::BlockRelay => "block-relay-only",
            Self::AddrFetch => "addr-fetch",
        }
    }

    /// Core's `CNode::IsOutboundOrBlockRelayConn`.
    pub const fn is_outbound_or_block_relay(self) -> bool {
        matches!(self, Self::OutboundFullRelay | Self::BlockRelay)
    }

    /// Core's `CNode::IsManualOrFullOutboundConn`.
    pub const fn is_manual_or_full_outbound(self) -> bool {
        matches!(self, Self::OutboundFullRelay | Self::Manual)
    }

    /// Core's `CNode::ExpectServicesFromConn`.
    pub const fn expects_services(self) -> bool {
        matches!(self, Self::OutboundFullRelay | Self::BlockRelay | Self::AddrFetch)
    }
}

/// Core's `NetPermissionFlags` (`net_permissions.h`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Permissions(pub u32);

impl Permissions {
    /// `None`.
    pub const NONE: Self = Self(0);
    /// `BloomFilter`.
    pub const BLOOM_FILTER: Self = Self(1 << 1);
    /// `Relay`.
    pub const RELAY: Self = Self(1 << 3);
    /// `ForceRelay` (implies `Relay`).
    pub const FORCE_RELAY: Self = Self((1 << 2) | (1 << 3));
    /// `Download`.
    pub const DOWNLOAD: Self = Self(1 << 6);
    /// `NoBan` (implies `Download`).
    pub const NO_BAN: Self = Self((1 << 4) | (1 << 6));
    /// `Mempool`.
    pub const MEMPOOL: Self = Self(1 << 5);
    /// `Addr`.
    pub const ADDR: Self = Self(1 << 7);
    /// `Implicit`: the flags came from `-whitelist` defaults.
    pub const IMPLICIT: Self = Self(1 << 31);

    /// Core's `NetPermissions::HasFlag`: every bit of `flag` is set.
    pub const fn has(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }
}

/// Core's `fPauseSend` after a push: whether the processor should stop polling or serving the
/// peer (SB0 §3.1). The push itself was always accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseState {
    /// The send buffer is within `-maxsendbuffer`.
    Clear,
    /// The send buffer is over `-maxsendbuffer`.
    Paused,
}

impl PauseState {
    /// Whether `fPauseSend` is set.
    pub const fn is_paused(self) -> bool {
        matches!(self, Self::Paused)
    }
}

/// Why a connection is closed, for the log line Core prints at that site.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Why {
    /// A processor decision, with Core's log text (e.g. `"feeler connection completed"`).
    Processor(&'static str),
    /// The handshake functions refused the peer.
    Handshake(&'static str),
    /// A gate refused the peer (blocks-only violation, …).
    Gate(&'static str),
    /// Misbehaviour reached the discouragement threshold.
    Discouraged,
    /// The address is banned.
    Banned,
    /// `setnetworkactive false`: `"Network not active, dropping peer=%d"`.
    NetworkInactive,
    /// The socket-level inactivity rules (`InactivityCheck`), with Core's log text.
    Inactivity(String),
    /// The peer closed the connection or the socket failed.
    Socket(String),
    /// The transport refused the peer's bytes.
    Transport(String),
    /// Outbound eviction, inbound eviction or a manual `disconnectnode`.
    Evicted,
    /// Shutdown.
    Shutdown,
}

/// The connection table's entry for one peer (Core's `CNode`), shared by reference count.
pub type PeerHandle = Arc<Peer>;

/// The connection table in insertion order (Core's `m_nodes`, `ForEachNode` order). Rebuilt on
/// every insert and removal, so readers hold a stable copy.
pub type PeerSnapshot = Arc<[PeerHandle]>;

/// The send queue (Core's `vSendMsg` and `m_send_memusage`, under `cs_vSend`).
#[derive(Default)]
pub(crate) struct SendQueue {
    pub(crate) msgs: VecDeque<OutMsg>,
    pub(crate) memusage: usize,
}

/// The two receive queues (Core's `m_msg_quorum_queue` and `m_msg_process_queue`, under
/// `m_msg_process_queue_mutex`).
#[derive(Default)]
pub(crate) struct RecvQueues {
    quorum: VecDeque<RawMsg>,
    normal: VecDeque<RawMsg>,
    quorum_size: usize,
    normal_size: usize,
    quorum_since_normal: usize,
}

/// Bytes per message type (Core's `mapSendBytesPerMsgType` and `mapRecvBytesPerMsgType`).
#[derive(Clone, Debug, Default)]
pub struct ByteStats {
    /// Bytes sent per type, V2 handshake bytes excluded.
    pub sent: HashMap<CommandString, u64>,
    /// Bytes received per known type; unknown types and rejected frames under `*other*`.
    pub recv: HashMap<CommandString, u64>,
}

/// Core's `NET_MESSAGE_TYPE_OTHER`.
pub const MSG_TYPE_OTHER: CommandString = CommandString::from_static("*other*");

/// How [`Peer::send_data`] left the socket (Core's `SocketSendData` return).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sent {
    /// Bytes the socket took.
    pub(crate) bytes: usize,
    /// Bytes remain that the socket did not take (`data_left`).
    pub(crate) data_left: bool,
    /// A send failed with an error other than would-block; `fDisconnect` is set.
    pub(crate) failed: bool,
}

/// Everything a new connection's entry is built from.
pub(crate) struct PeerInit {
    pub(crate) id: PeerId,
    pub(crate) addr: Address,
    pub(crate) addr_bind: Service,
    pub(crate) addr_name: String,
    pub(crate) dest: Option<String>,
    pub(crate) conn_type: ConnectionType,
    pub(crate) inbound_onion: bool,
    pub(crate) permissions: Permissions,
    pub(crate) network: Network,
    pub(crate) keyed_net_group: u64,
    pub(crate) local_nonce: u64,
    pub(crate) connected: i64,
    pub(crate) masternode: bool,
    pub(crate) masternode_probe: bool,
    pub(crate) use_v2: bool,
    pub(crate) transport: Transport,
    pub(crate) sock: Option<Arc<dyn NetSocket>>,
    pub(crate) grant: Option<Grant>,
    pub(crate) prefer_evict: bool,
}

/// Core's `CNode` at the net level. Created by [`NetCore`](crate::NetCore) for each connection.
pub struct Peer {
    id: PeerId,
    addr: Address,
    addr_bind: Service,
    addr_name: String,
    dest: Option<String>,
    conn_type: ConnectionType,
    inbound_onion: bool,
    permissions: Permissions,
    network: Network,
    keyed_net_group: u64,
    local_nonce: u64,
    connected: i64,
    /// Atomic: the `version` handler sets it for inbound peers (`fOtherMasternode`).
    masternode: AtomicBool,
    masternode_probe: bool,
    /// Core's `sentMNAuthChallenge`: zero until our `version` draws it (`PushNodeVersion`).
    sent_mnauth_challenge: Mutex<[u8; 32]>,
    use_v2: bool,
    prefer_evict: bool,

    version: AtomicI32,
    common_version: AtomicI32,
    successfully_connected: AtomicBool,
    disconnect: AtomicBool,
    other_side_disconnected: AtomicBool,
    pause_send: AtomicBool,
    pause_recv: AtomicBool,
    version_services: AtomicU64,
    version_relay_txs: AtomicBool,
    has_all_wanted_services: AtomicBool,
    relays_txs: AtomicBool,
    time_offset: AtomicI64,
    last_send: AtomicI64,
    last_recv: AtomicI64,
    send_bytes: AtomicU64,
    recv_bytes: AtomicU64,
    first_message_time: AtomicI64,
    first_message_is_mnauth: AtomicBool,
    masternode_iqr: AtomicBool,
    bip152_hb_to: AtomicBool,
    bip152_hb_from: AtomicBool,
    send_dsq: AtomicBool,
    qwatch: AtomicBool,
    bloom_filter_loaded: AtomicBool,
    last_block_time: AtomicI64,
    last_tx_time: AtomicI64,
    last_ping_us: AtomicI64,
    min_ping_us: AtomicI64,
    clean_subver: Mutex<String>,
    addr_local: Mutex<Option<Service>>,
    received_mnauth_challenge: Mutex<[u8; 32]>,
    verified_proreg_hash: Mutex<[u8; 32]>,
    verified_pubkey_hash: Mutex<[u8; 32]>,
    stats: Mutex<ByteStats>,

    /// Lock order: `send` before `transport`; the reader takes `transport` alone.
    pub(crate) send: Mutex<SendQueue>,
    pub(crate) transport: Mutex<Transport>,
    pub(crate) recv: Mutex<RecvQueues>,
    pub(crate) sock: Mutex<Option<Arc<dyn NetSocket>>>,
    pub(crate) grant: Mutex<Option<Grant>>,
    /// Core's `nDisconnectLingerTime` (steady clock).
    pub(crate) linger_until: Mutex<Option<Instant>>,
    pub(crate) socket_shutdown: AtomicBool,
    /// A lingering peer's socket is readable (bytes, or their FIN): Core's `fHasRecvData` on a
    /// marked node, which keeps its socket loop polling without a timeout.
    pub(crate) linger_readable: AtomicBool,
    send_backlogged: AtomicBool,
    /// Wakes the writer: a push to an idle queue, or new bytes from the transport.
    pub(crate) writer_wake: Notify,
    /// Wakes the reader when `fPauseRecv` clears or the socket closes.
    pub(crate) reader_wake: Notify,
}

fn load(a: &AtomicBool) -> bool {
    a.load(Ordering::SeqCst)
}

fn store(a: &AtomicBool, v: bool) {
    a.store(v, Ordering::SeqCst);
}

impl Peer {
    pub(crate) fn new(init: PeerInit) -> Self {
        Self {
            id: init.id,
            addr: init.addr,
            addr_bind: init.addr_bind,
            addr_name: init.addr_name,
            dest: init.dest,
            conn_type: init.conn_type,
            inbound_onion: init.inbound_onion,
            permissions: init.permissions,
            network: init.network,
            keyed_net_group: init.keyed_net_group,
            local_nonce: init.local_nonce,
            prefer_evict: init.prefer_evict,
            connected: init.connected,
            masternode: AtomicBool::new(init.masternode),
            masternode_probe: init.masternode_probe,
            sent_mnauth_challenge: Mutex::new([0; 32]),
            use_v2: init.use_v2,
            version: AtomicI32::new(0),
            common_version: AtomicI32::new(INIT_PROTO_VERSION),
            successfully_connected: AtomicBool::new(false),
            disconnect: AtomicBool::new(false),
            other_side_disconnected: AtomicBool::new(false),
            pause_send: AtomicBool::new(false),
            pause_recv: AtomicBool::new(false),
            version_services: AtomicU64::new(0),
            version_relay_txs: AtomicBool::new(false),
            has_all_wanted_services: AtomicBool::new(false),
            relays_txs: AtomicBool::new(false),
            time_offset: AtomicI64::new(0),
            last_send: AtomicI64::new(0),
            last_recv: AtomicI64::new(0),
            send_bytes: AtomicU64::new(0),
            recv_bytes: AtomicU64::new(0),
            first_message_time: AtomicI64::new(0),
            first_message_is_mnauth: AtomicBool::new(false),
            masternode_iqr: AtomicBool::new(false),
            bip152_hb_to: AtomicBool::new(false),
            bip152_hb_from: AtomicBool::new(false),
            send_dsq: AtomicBool::new(false),
            qwatch: AtomicBool::new(false),
            bloom_filter_loaded: AtomicBool::new(false),
            last_block_time: AtomicI64::new(0),
            last_tx_time: AtomicI64::new(0),
            last_ping_us: AtomicI64::new(0),
            min_ping_us: AtomicI64::new(i64::MAX),
            clean_subver: Mutex::new(String::new()),
            addr_local: Mutex::new(None),
            received_mnauth_challenge: Mutex::new([0; 32]),
            verified_proreg_hash: Mutex::new([0; 32]),
            verified_pubkey_hash: Mutex::new([0; 32]),
            stats: Mutex::new(ByteStats::default()),
            send: Mutex::new(SendQueue::default()),
            transport: Mutex::new(init.transport),
            recv: Mutex::new(RecvQueues::default()),
            sock: Mutex::new(init.sock),
            grant: Mutex::new(init.grant),
            linger_until: Mutex::new(None),
            socket_shutdown: AtomicBool::new(false),
            linger_readable: AtomicBool::new(false),
            send_backlogged: AtomicBool::new(false),
            writer_wake: Notify::new(),
            reader_wake: Notify::new(),
        }
    }

    /// Core's `GetId()`.
    pub fn id(&self) -> PeerId {
        self.id
    }

    /// Core's `CNode::addr`: the remote address, with the services AddrMan recorded for it.
    pub fn addr(&self) -> &Address {
        &self.addr
    }

    /// Core's `addrBind`: our end of the socket.
    pub fn addr_bind(&self) -> &Service {
        &self.addr_bind
    }

    /// Core's `m_addr_name`: the destination string, or the address for inbound peers.
    pub fn addr_name(&self) -> &str {
        &self.addr_name
    }

    /// Core's `m_dest`: the destination string an outbound connection was opened with.
    pub fn dest(&self) -> Option<&str> {
        self.dest.as_deref()
    }

    /// Core's `m_conn_type`.
    pub fn conn_type(&self) -> ConnectionType {
        self.conn_type
    }

    /// Core's `IsInboundConn()`.
    pub fn is_inbound(&self) -> bool {
        self.conn_type == ConnectionType::Inbound
    }

    /// Core's `m_inbound_onion`.
    pub fn inbound_onion(&self) -> bool {
        self.inbound_onion
    }

    /// Core's `m_permission_flags`.
    pub fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Core's `HasPermission(flag)`.
    pub fn has_permission(&self, flag: Permissions) -> bool {
        self.permissions.has(flag)
    }

    /// Core's `ConnectedThroughNetwork()`: onion for inbound onion peers, else the address's
    /// net class.
    pub fn network(&self) -> Network {
        self.network
    }

    /// Core's `nKeyedNetGroup`.
    pub fn keyed_net_group(&self) -> u64 {
        self.keyed_net_group
    }

    /// Core's `nLocalHostNonce`, sent in our `version`.
    pub fn local_nonce(&self) -> u64 {
        self.local_nonce
    }

    /// Core's `m_connected`: whole seconds on the node clock.
    pub fn connected(&self) -> i64 {
        self.connected
    }

    /// Whether the connection was opened with the V2 transport (Core's `m_use_v2transport`).
    pub fn use_v2(&self) -> bool {
        self.use_v2
    }

    /// The transport's protocol and session ID (`m_transport->GetInfo()`).
    pub fn transport_info(&self) -> TransportInfo {
        self.transport.lock().info()
    }

    /// Core's `nVersion`: the peer's advertised version, 0 before its `version`.
    pub fn version(&self) -> i32 {
        self.version.load(Ordering::SeqCst)
    }

    /// Core's `GetCommonVersion()`: `INIT_PROTO_VERSION` (209) before `version`.
    pub fn common_version(&self) -> ProtocolVersion {
        ProtocolVersion(self.common_version.load(Ordering::SeqCst))
    }

    /// Core's `fSuccessfullyConnected`.
    pub fn successfully_connected(&self) -> bool {
        load(&self.successfully_connected)
    }

    /// Core's `fDisconnect`.
    pub fn disconnect_requested(&self) -> bool {
        load(&self.disconnect)
    }

    /// Core's `fOtherSideDisconnected`: no linger on removal.
    pub fn other_side_disconnected(&self) -> bool {
        load(&self.other_side_disconnected)
    }

    /// Core's `fPauseSend`.
    pub fn pause_send(&self) -> bool {
        load(&self.pause_send)
    }

    /// Core's `fPauseRecv`.
    pub fn pause_recv(&self) -> bool {
        load(&self.pause_recv)
    }

    /// Core's `m_version_services` (zero before `version`).
    pub fn version_services(&self) -> ServiceFlags {
        ServiceFlags(self.version_services.load(Ordering::SeqCst))
    }

    /// Core's `m_version_relay_txs`.
    pub fn version_relay_txs(&self) -> bool {
        load(&self.version_relay_txs)
    }

    /// Core's `m_has_all_wanted_services`.
    pub fn has_all_wanted_services(&self) -> bool {
        load(&self.has_all_wanted_services)
    }

    /// Core's `m_relays_txs`.
    pub fn relays_txs(&self) -> bool {
        load(&self.relays_txs)
    }

    /// Sets Core's `m_relays_txs` (the processor's `SetTxRelay` path).
    pub fn set_relays_txs(&self, relays: bool) {
        store(&self.relays_txs, relays);
    }

    /// Core's `cleanSubVer`.
    pub fn clean_subver(&self) -> String {
        self.clean_subver.lock().clone()
    }

    /// Core's `GetAddrLocal()`: our address as the peer reported it in its `version`.
    pub fn addr_local(&self) -> Option<Service> {
        self.addr_local.lock().clone()
    }

    /// Core's `nTimeOffset`.
    pub fn time_offset(&self) -> i64 {
        self.time_offset.load(Ordering::SeqCst)
    }

    /// Core's `m_masternode_connection`.
    pub fn masternode_connection(&self) -> bool {
        load(&self.masternode)
    }

    /// Sets Core's `m_masternode_connection` (an inbound peer's `fOtherMasternode`).
    pub(crate) fn set_masternode_connection(&self, v: bool) {
        store(&self.masternode, v);
    }

    /// Core's `m_masternode_probe_connection`.
    pub fn masternode_probe(&self) -> bool {
        self.masternode_probe
    }

    /// Core's `m_prefer_evict`: an inbound peer accepted while discouraged.
    pub fn prefer_evict(&self) -> bool {
        self.prefer_evict
    }

    /// Core's `m_masternode_iqr_connection`.
    pub fn masternode_iqr(&self) -> bool {
        load(&self.masternode_iqr)
    }

    /// Sets Core's `m_masternode_iqr_connection`.
    pub fn set_masternode_iqr(&self, v: bool) {
        store(&self.masternode_iqr, v);
    }

    /// Core's `GetReceivedMNAuthChallenge()`: all zero (Core's null `uint256`) until set.
    pub fn received_mnauth_challenge(&self) -> [u8; 32] {
        *self.received_mnauth_challenge.lock()
    }

    /// Core's `SetReceivedMNAuthChallenge`.
    pub fn set_received_mnauth_challenge(&self, c: [u8; 32]) {
        *self.received_mnauth_challenge.lock() = c;
    }

    /// Core's `GetSentMNAuthChallenge()`.
    pub fn sent_mnauth_challenge(&self) -> [u8; 32] {
        *self.sent_mnauth_challenge.lock()
    }

    /// Core's `SetSentMNAuthChallenge`.
    pub(crate) fn set_sent_mnauth_challenge(&self, c: [u8; 32]) {
        *self.sent_mnauth_challenge.lock() = c;
    }

    /// Core's `GetVerifiedProRegTxHash()`: all zero until MNAUTH verifies.
    pub fn verified_proreg_hash(&self) -> [u8; 32] {
        *self.verified_proreg_hash.lock()
    }

    /// Core's `SetVerifiedProRegTxHash`.
    pub fn set_verified_proreg_hash(&self, h: [u8; 32]) {
        *self.verified_proreg_hash.lock() = h;
    }

    /// Core's `GetVerifiedPubKeyHash()`.
    pub fn verified_pubkey_hash(&self) -> [u8; 32] {
        *self.verified_pubkey_hash.lock()
    }

    /// Core's `SetVerifiedPubKeyHash`.
    pub fn set_verified_pubkey_hash(&self, h: [u8; 32]) {
        *self.verified_pubkey_hash.lock() = h;
    }

    /// Core's `m_last_send`, seconds on the node clock.
    pub fn last_send(&self) -> i64 {
        self.last_send.load(Ordering::SeqCst)
    }

    /// Core's `m_last_recv`, seconds on the node clock.
    pub fn last_recv(&self) -> i64 {
        self.last_recv.load(Ordering::SeqCst)
    }

    /// Core's `nSendBytes`.
    pub fn send_bytes(&self) -> u64 {
        self.send_bytes.load(Ordering::SeqCst)
    }

    /// Core's `nRecvBytes`.
    pub fn recv_bytes(&self) -> u64 {
        self.recv_bytes.load(Ordering::SeqCst)
    }

    /// Bytes per message type, for `getpeerinfo`.
    pub fn byte_stats(&self) -> ByteStats {
        self.stats.lock().clone()
    }

    /// Core's `nTimeFirstMessageReceived` (0 until the first message after `verack`).
    pub fn first_message_time(&self) -> i64 {
        self.first_message_time.load(Ordering::SeqCst)
    }

    /// Core's `fFirstMessageIsMNAUTH`.
    pub fn first_message_is_mnauth(&self) -> bool {
        load(&self.first_message_is_mnauth)
    }

    /// Core's `CanRelay()`: not a masternode connection, or an intra-quorum relay one.
    pub fn can_relay(&self) -> bool {
        !self.masternode_connection() || self.masternode_iqr()
    }

    /// Core's `m_bip152_highbandwidth_to`.
    pub fn bip152_highbandwidth_to(&self) -> bool {
        load(&self.bip152_hb_to)
    }

    /// Sets Core's `m_bip152_highbandwidth_to`.
    pub fn set_bip152_highbandwidth_to(&self, v: bool) {
        store(&self.bip152_hb_to, v);
    }

    /// Core's `m_bip152_highbandwidth_from`.
    pub fn bip152_highbandwidth_from(&self) -> bool {
        load(&self.bip152_hb_from)
    }

    /// Sets Core's `m_bip152_highbandwidth_from`.
    pub fn set_bip152_highbandwidth_from(&self, v: bool) {
        store(&self.bip152_hb_from, v);
    }

    /// Core's `fSendDSQueue`.
    pub fn send_dsq(&self) -> bool {
        load(&self.send_dsq)
    }

    /// Sets Core's `fSendDSQueue`.
    pub fn set_send_dsq(&self, v: bool) {
        store(&self.send_dsq, v);
    }

    /// Core's `qwatch`.
    pub fn qwatch(&self) -> bool {
        load(&self.qwatch)
    }

    /// Sets Core's `qwatch`.
    pub fn set_qwatch(&self, v: bool) {
        store(&self.qwatch, v);
    }

    /// Core's `m_bloom_filter_loaded`.
    pub fn bloom_filter_loaded(&self) -> bool {
        load(&self.bloom_filter_loaded)
    }

    /// Sets Core's `m_bloom_filter_loaded`.
    pub fn set_bloom_filter_loaded(&self, v: bool) {
        store(&self.bloom_filter_loaded, v);
    }

    /// Core's `m_last_block_time` (seconds).
    pub fn last_block_time(&self) -> i64 {
        self.last_block_time.load(Ordering::SeqCst)
    }

    /// Sets Core's `m_last_block_time`.
    pub fn set_last_block_time(&self, t: i64) {
        self.last_block_time.store(t, Ordering::SeqCst);
    }

    /// Core's `m_last_tx_time` (seconds).
    pub fn last_tx_time(&self) -> i64 {
        self.last_tx_time.load(Ordering::SeqCst)
    }

    /// Sets Core's `m_last_tx_time`.
    pub fn set_last_tx_time(&self, t: i64) {
        self.last_tx_time.store(t, Ordering::SeqCst);
    }

    /// Core's `m_last_ping_time` in microseconds (0 before the first pong).
    pub fn last_ping_us(&self) -> i64 {
        self.last_ping_us.load(Ordering::SeqCst)
    }

    /// Core's `m_min_ping_time` in microseconds (`i64::MAX` before the first pong).
    pub fn min_ping_us(&self) -> i64 {
        self.min_ping_us.load(Ordering::SeqCst)
    }

    /// Core's `CNode::PongReceived`.
    pub(crate) fn pong_received(&self, ping_us: i64) {
        self.last_ping_us.store(ping_us, Ordering::SeqCst);
        self.min_ping_us.fetch_min(ping_us, Ordering::SeqCst);
    }

    /// The handshake's record of the peer's `version` fields. The common version also goes to
    /// the transport, which a V2 connection needs to send short message IDs.
    // Core: net.h:1041-1047 CNode::SetCommonVersion; net.cpp:1601-1606 V2Transport::SetPeerVersion
    pub(crate) fn record_version(
        &self,
        version: i32,
        common: ProtocolVersion,
        services: ServiceFlags,
        relay: bool,
        subver: String,
    ) {
        self.version.store(version, Ordering::SeqCst);
        self.common_version.store(common.0, Ordering::SeqCst);
        // Takes the transport lock alone, as the reader does (the caller holds no send lock).
        self.transport.lock().set_peer_version(common);
        self.version_services.store(services.0, Ordering::SeqCst);
        store(&self.version_relay_txs, relay);
        *self.clean_subver.lock() = subver;
    }

    pub(crate) fn set_has_all_wanted_services(&self, v: bool) {
        store(&self.has_all_wanted_services, v);
    }

    pub(crate) fn set_addr_local(&self, addr: Service) {
        *self.addr_local.lock() = Some(addr);
    }

    pub(crate) fn set_time_offset(&self, offset: i64) {
        self.time_offset.store(offset, Ordering::SeqCst);
    }

    pub(crate) fn set_successfully_connected(&self) {
        store(&self.successfully_connected, true);
    }

    #[cfg(test)]
    pub(crate) fn set_last_io(&self, send: i64, recv: i64) {
        self.last_send.store(send, Ordering::SeqCst);
        self.last_recv.store(recv, Ordering::SeqCst);
    }

    pub(crate) fn set_first_message(&self, time: i64, is_mnauth: bool) {
        self.first_message_time.store(time, Ordering::SeqCst);
        store(&self.first_message_is_mnauth, is_mnauth);
    }

    /// Sets `fDisconnect`; the sweep removes the peer. Returns whether it was newly set.
    pub(crate) fn mark_disconnect(&self) -> bool {
        let newly = !self.disconnect.swap(true, Ordering::SeqCst);
        // Core skips receiving for a marked peer: stop the reader now.
        self.reader_wake.notify_one();
        newly
    }

    /// Whether the last send made progress but left bytes queued (Core then skips receiving
    /// in that round).
    pub(crate) fn send_backlogged(&self) -> bool {
        self.send_backlogged.load(Ordering::SeqCst)
    }

    /// Sets the send backlog; clearing it resumes the reader.
    pub(crate) fn set_send_backlogged(&self, on: bool) {
        if self.send_backlogged.swap(on, Ordering::SeqCst) && !on {
            self.reader_wake.notify_one();
        }
    }

    /// Drops the receive queues of a removed peer.
    pub(crate) fn clear_recv(&self) {
        *self.recv.lock() = RecvQueues::default();
    }

    /// Core's `CloseSocketDisconnect`: marks the peer, drops the socket and stops its tasks.
    /// `other_side` is Core's `fOtherSideDisconnected` (no linger).
    pub(crate) fn close_socket(&self, other_side: bool) {
        if other_side {
            store(&self.other_side_disconnected, true);
        }
        store(&self.disconnect, true);
        if let Some(sock) = self.sock.lock().take() {
            tracing::debug!(target: "net", "disconnecting peer={}", self.id);
            sock.close();
        }
        self.writer_wake.notify_one();
        self.reader_wake.notify_one();
    }

    /// Core's `CConnman::PushMessage` under `cs_vSend`: charges the message, sets `fPauseSend`
    /// when over `send_max`, appends. Returns `fPauseSend` and whether to wake the writer
    /// (the queue was empty and the transport now has bytes: Core's optimistic-write wake).
    // Core: net.cpp:4878-4904 CConnman::PushMessage
    pub(crate) fn push_msg(&self, msg: OutMsg, send_max: usize) -> (PauseState, bool) {
        tracing::trace!(
            target: "net",
            "sending {} ({} bytes) peer={}",
            sanitize_command(msg.command.as_bytes()),
            msg.payload.len(),
            self.id
        );
        let mut q = self.send.lock();
        let t = self.transport.lock();
        let to_send = t.bytes_to_send(true);
        let queue_was_empty = to_send.data.is_empty() && q.msgs.is_empty();
        let more = to_send.more;
        q.memusage += msg.memory_usage();
        if q.memusage + t.send_memory_usage() > send_max {
            store(&self.pause_send, true);
        }
        drop(t);
        q.msgs.push_back(msg);
        let pause = if self.pause_send() {
            PauseState::Paused
        } else {
            PauseState::Clear
        };
        (pause, queue_was_empty && more)
    }

    /// Core's `CConnman::SocketSendData`: hands queued messages to the transport and writes its
    /// bytes through `write` (`send` with `more` as `MSG_MORE`) until the queue is drained or
    /// the socket takes less than offered, then recomputes `fPauseSend` from both charges.
    /// A write error other than would-block marks the peer for disconnection.
    // Core: net.cpp:1694-1773 CConnman::SocketSendData
    pub(crate) fn send_data(
        &self,
        send_max: usize,
        now: i64,
        mut write: impl FnMut(&[u8], bool) -> Option<io::Result<usize>>,
    ) -> Sent {
        let mut q = self.send.lock();
        let mut t = self.transport.lock();
        let mut sent = 0;
        let mut data_left;
        let mut expected_more: Option<bool> = None;
        let mut failed = false;
        loop {
            if let Some(msg) = q.msgs.pop_front() {
                let memusage = msg.memory_usage();
                match t.set_message_to_send(msg) {
                    Ok(()) => q.memusage -= memusage,
                    Err(msg) => q.msgs.push_front(msg),
                }
            }
            let to_send = t.bytes_to_send(!q.msgs.is_empty());
            if let Some(expected) = expected_more {
                debug_assert_eq!(!to_send.data.is_empty(), expected, "GetBytesToSend more");
            }
            expected_more = Some(to_send.more);
            data_left = !to_send.data.is_empty();
            if to_send.data.is_empty() {
                break;
            }
            let (len, command) = (to_send.data.len(), to_send.command);
            // `None`: no socket (already disconnected); leave everything queued.
            let Some(result) = write(to_send.data, to_send.more) else {
                break;
            };
            match result {
                Ok(n) if n > 0 => {
                    self.last_send.store(now, Ordering::SeqCst);
                    self.send_bytes.fetch_add(n as u64, Ordering::SeqCst);
                    t.mark_bytes_sent(n);
                    if command != CommandString::EMPTY {
                        *self.stats.lock().sent.entry(command).or_default() += n as u64;
                    }
                    sent += n;
                    if n != len {
                        break;
                    }
                }
                Ok(_) => break,
                Err(e) => {
                    // Core: net.cpp:1756 (`WSAEMSGSIZE` and `WSAEINPROGRESS` are transient too)
                    if !crate::netcore::is_transient(&e) {
                        tracing::debug!(target: "net", "socket send error for peer={}: {e}", self.id);
                        store(&self.disconnect, true);
                        failed = true;
                    }
                    break;
                }
            }
        }
        store(&self.pause_send, q.memusage + t.send_memory_usage() > send_max);
        if q.msgs.is_empty() {
            debug_assert_eq!(q.memusage, 0);
        }
        Sent {
            bytes: sent,
            data_left,
            failed,
        }
    }

    /// Core's `GetBytesToSend(nSendMsgSize != 0)` emptiness check in `DisconnectNodes`: nothing
    /// queued and nothing in the transport.
    pub fn send_queue_empty(&self) -> bool {
        let q = self.send.lock();
        let t = self.transport.lock();
        let to_send = t.bytes_to_send(!q.msgs.is_empty());
        to_send.data.is_empty() && !to_send.more
    }

    /// Core's `CNode::ReceiveMsgBytes` then `MarkReceivedMsgsForProcessing`: feeds `bytes` to the
    /// transport, counts them, and queues every completed message (quorum-priority ones on their
    /// own queue unless `priority` is off), updating `fPauseRecv` against `flood`. Returns
    /// whether a message was queued.
    ///
    /// # Errors
    ///
    /// The transport refused the bytes. Messages completed before them are dropped (BL-73):
    /// Core queues them but never processes a disconnecting peer.
    // Core: net.cpp:649-692 CNode::ReceiveMsgBytes; net.cpp:4773-4804 MarkReceivedMsgsForProcessing
    pub(crate) fn receive_bytes(
        &self,
        bytes: &[u8],
        now_us: i64,
        flood: usize,
        priority: bool,
    ) -> Result<bool, TransportError> {
        self.last_recv.store(now_us.div_euclid(1_000_000), Ordering::SeqCst);
        self.recv_bytes.fetch_add(bytes.len() as u64, Ordering::SeqCst);
        let mut done = Vec::new();
        let result = {
            let mut t = self.transport.lock();
            t.receive(bytes, now_us, |m| done.push(m))
        };
        // Counted under the queue lock, so a message the stats show is already queued (or
        // dropped with a refused transport), never in between.
        let mut recv = self.recv.lock();
        {
            let mut stats = self.stats.lock();
            for m in &done {
                let (cmd, size) = match m {
                    Ok(m) if is_known_command(&m.command) => (m.command, m.raw_size),
                    Ok(m) => (MSG_TYPE_OTHER, m.raw_size),
                    Err(r) => (MSG_TYPE_OTHER, r.raw_size),
                };
                *stats.recv.entry(cmd).or_default() += size as u64;
            }
        }
        result?;
        let mut queued = false;
        for msg in done.into_iter().flatten() {
            let msg = RawMsg {
                cmd: msg.command,
                payload: msg.payload,
                recv_us: msg.time_us,
                raw_size: msg.raw_size,
            };
            if priority && crate::policy::is_quorum_priority(&msg.cmd) {
                recv.quorum_size += msg.raw_size;
                recv.quorum.push_back(msg);
            } else {
                recv.normal_size += msg.raw_size;
                recv.normal.push_back(msg);
            }
            queued = true;
        }
        if queued {
            store(&self.pause_recv, recv.quorum_size + recv.normal_size > flood);
        }
        Ok(queued)
    }

    /// Core's `CNode::PollMessage`.
    // Core: net.cpp:4806-4850 CNode::PollMessage
    pub(crate) fn poll_message(&self, flood: usize) -> Option<Polled> {
        const QUORUM_TO_NORMAL_RATIO: usize = 100;
        let mut recv = self.recv.lock();
        let r = &mut *recv;
        let skip_quorum = !r.normal.is_empty() && r.quorum_since_normal >= QUORUM_TO_NORMAL_RATIO;
        let msg = if !r.quorum.is_empty() && !skip_quorum {
            let msg = r.quorum.pop_front().expect("non-empty");
            r.quorum_size -= msg.raw_size;
            if !r.normal.is_empty() {
                r.quorum_since_normal += 1;
            }
            msg
        } else {
            let msg = r.normal.pop_front()?;
            r.normal_size -= msg.raw_size;
            r.quorum_since_normal = 0;
            msg
        };
        let was_paused =
            self.pause_recv.swap(r.quorum_size + r.normal_size > flood, Ordering::SeqCst);
        if was_paused && !self.pause_recv() {
            self.reader_wake.notify_one();
        }
        Some(Polled {
            msg,
            more: !r.quorum.is_empty() || !r.normal.is_empty(),
        })
    }
}

/// Whether Core's `mapRecvBytesPerMsgType` has a slot for `cmd` (`getAllNetMessageTypes`).
fn is_known_command(cmd: &CommandString) -> bool {
    crate::policy::is_known(cmd)
}

#[cfg(any(test, feature = "test-utils"))]
impl Peer {
    /// Test hook: sets `fSuccessfullyConnected` without a VERACK. **Not Core's path**: Core sets
    /// it only at the end of `ProcessMessage(VERACK)` ([`crate::handshake::on_verack`]), after the
    /// `sendheaders`/`sendcmpct`/`senddsq` pushes this skips. For loop-level tests of
    /// `SendMessages` that start past the handshake.
    pub fn set_successfully_connected_for_tests(&self) {
        self.set_successfully_connected();
    }

    /// Test hook, the receive side of [`Peer::send_queue_empty`]: no received message waits for
    /// `PollMessage`. A message counted in [`Peer::byte_stats`] is queued by then, so after the stats
    /// show it this is false until the processor has polled it. Bytes still in the transport (a
    /// partial frame) are not counted.
    pub fn recv_queue_empty(&self) -> bool {
        let recv = self.recv.lock();
        recv.quorum.is_empty() && recv.normal.is_empty()
    }

    /// Test hook (D40): a copy of the send queue and the memory Core charges for it, read under
    /// the send lock then the transport's, as [`Peer::send_queue_empty`] reads them.
    pub fn queued_sends(&self) -> QueuedSends {
        let q = self.send.lock();
        let t = self.transport.lock();
        QueuedSends {
            msgs: q.msgs.iter().cloned().collect(),
            memusage: q.memusage,
            transport_memusage: t.send_memory_usage(),
        }
    }

    /// Test hook (D41): Core's `nDisconnectLingerTime`, the steady-clock deadline the sweep sets
    /// the first time it sees this peer marked (`DisconnectNodes`: now + 100 ms); `None` before,
    /// and for a peer whose other side disconnected, which does not linger.
    pub fn linger_until(&self) -> Option<Instant> {
        *self.linger_until.lock()
    }
}

/// A peer's send side as [`Peer::queued_sends`] copies it (test hook, D40).
#[cfg(any(test, feature = "test-utils"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedSends {
    /// `vSendMsg`, oldest first: the messages not yet handed to the transport.
    pub msgs: Vec<OutMsg>,
    /// `m_send_memusage`: what Core charges for `msgs`.
    pub memusage: usize,
    /// The transport's `GetSendMemoryUsage`: a message it is sending, and its buffers.
    pub transport_memusage: usize,
}

impl fmt::Debug for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Peer")
            .field("id", &self.id)
            .field("addr", &self.addr.service)
            .field("conn_type", &self.conn_type)
            .finish_non_exhaustive()
    }
}
