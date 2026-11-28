//! `rpc/net.cpp`'s commands over the network the node provides ([`NodeNet`]):
//! `getconnectioncount` (DEC-1882), `getpeerinfo` (DEC-279), `getnetworkinfo` and
//! `setnetworkactive` (BL-391), `addnode` (BL-394), `disconnectnode` and `getaddednodeinfo`
//! (BL-427), `getnodeaddresses` and hidden `addconnection` and `addpeeraddress` (P8 S0b),
//! hidden `sendmsgtopeer`, `getaddrmaninfo` and `getrawaddrman` (BL-690a), `getnettotals`
//! (BL-690b), and `setmnthreadactive` (BL-755, [`register_mnthread_commands`]); and `rpc/node.cpp`'s
//! `mnauth` ([`register_mnauth_commands`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dash_netaddr::{NetAddr, Network, Service};
use dash_primitives::BlockHash;

use crate::UniValue;
use crate::protocol::{
    RPC_CLIENT_NODE_ALREADY_ADDED, RPC_CLIENT_NODE_CAPACITY_REACHED, RPC_CLIENT_NODE_NOT_ADDED,
    RPC_CLIENT_NODE_NOT_CONNECTED, RPC_CLIENT_P2P_DISABLED, RPC_INVALID_PARAMETER,
    RPC_INVALID_PARAMS, RPC_MISC_ERROR, RpcError,
};
use crate::request::{get_int_i32, get_int_i64, get_int_u16, param, parse_hex};
use crate::server::{Command, NodeControl, RpcServer};
use crate::warnings::NodeWarnings;

/// Core's `NODE_NETWORK` service flag (`protocol.h`).
const NODE_NETWORK: u64 = 1 << 0;

/// The four outbound connection types `addconnection` accepts (`node/connection_types.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddConnectionType {
    /// `"outbound-full-relay"` (`ConnectionType::OUTBOUND_FULL_RELAY`).
    OutboundFullRelay,
    /// `"block-relay-only"` (`ConnectionType::BLOCK_RELAY`).
    BlockRelay,
    /// `"addr-fetch"` (`ConnectionType::ADDR_FETCH`).
    AddrFetch,
    /// `"feeler"` (`ConnectionType::FEELER`).
    Feeler,
}

/// The network the commands read: Core's `NodeContext::connman` and `peerman`. `dashd-node`
/// implements it.
pub trait NodeNet: Send + Sync {
    /// `CConnman::GetNodeStats` joined with `PeerManager::GetNodeStateStats`: every connected
    /// peer not marked for disconnection, in the connection table's order, less those whose
    /// state stats fail (a peer gone between the two reads). `None` when the connection manager
    /// or the peer manager is missing (before init builds them, and after shutdown).
    fn peer_info(&self) -> Option<Vec<PeerInfo>>;

    /// What `getnetworkinfo` reads of the connection manager, the peer manager, the mempool and
    /// the reachability globals. `None` without the connection manager, as
    /// [`peer_info`](NodeNet::peer_info).
    fn network_info(&self) -> Option<NetworkInfo>;

    /// `CConnman::SetNetworkActive`, then `GetNetworkActive`. `None` without the connection
    /// manager.
    fn set_network_active(&self, active: bool) -> Option<bool>;

    /// `GetLocalServices() & NODE_P2P_V2`: whether we offer the v2 transport (`-v2transport`).
    /// `None` without the connection manager. The `addnode` calls below follow a `Some`.
    fn v2_transport(&self) -> Option<bool>;

    /// `OpenNetworkConnection(CAddress(), false, {}, node, MANUAL, use_v2)`: one manual
    /// connection attempt to `node`, without a slot, returning once the connect has ended,
    /// whatever its outcome.
    fn open_one_try(&self, node: &str, use_v2: bool);

    /// `CConnman::AddNode`: false when `node`, or the numeric address it names, is listed.
    fn add_node(&self, node: &str, use_v2: bool) -> bool;

    /// `CConnman::RemoveAddedNode`: false when `node` is not listed by that name.
    fn remove_added_node(&self, node: &str) -> bool;

    /// `CConnman::DisconnectNode(const std::string&)`: marks the first connected peer, not
    /// already marked, whose connection name is `address`. False when none is.
    fn disconnect_by_address(&self, address: &str) -> bool;

    /// `CConnman::DisconnectNode(NodeId)`: likewise, by id.
    fn disconnect_by_id(&self, id: i64) -> bool;

    /// `CConnman::TryMarkVerified`: stores both hashes on the peer with that id and answers true;
    /// false when no peer has it, or when it is inbound and at least
    /// `MAX_VERIFIED_INBOUND_PER_PROTX` other inbound peers not marked for disconnection carry
    /// `proreg_hash`. The hashes are in internal byte order (`uint256`'s). `None` without the
    /// connection manager.
    // Core: net.cpp:4514-4539
    fn try_mark_verified(
        &self,
        id: i64,
        proreg_hash: [u8; 32],
        pubkey_hash: [u8; 32],
    ) -> Option<bool>;

    /// `CConnman::SetMasternodeThreadActive`, then `GetMasternodeThreadActive`. `None` without
    /// the connection manager.
    // Core: net.h:1325-1326
    fn set_masternode_thread_active(&self, active: bool) -> Option<bool>;

    /// `CConnman::GetAddedNodeInfo(/*include_connected=*/true)`: every added node in the list's
    /// order. `None` without the connection manager.
    fn added_node_info(&self) -> Option<Vec<AddedNodeInfo>>;

    /// `EnsurePeerman`, then `PeerManager::FetchBlock(peer_id, *index)` for the indexed block
    /// `hash`: `None` without a peer manager (never attached, detached, or its processor
    /// stopped), `Some(Err(msg))` with `FetchBlock`'s exact string. Waits for the processor
    /// without a timeout; call it from an RPC worker only, holding no chain or network lock.
    // Core (v24.0.0): rpc/blockchain.cpp:507, :528-530; net_processing.cpp:2062-2094
    fn fetch_block(&self, hash: &BlockHash, peer_id: i64) -> Option<Result<(), String>>;

    /// `EnsurePeerman`, then `PeerManager::SendPings`: a ping queued on every peer in the peer
    /// manager's map. `None` without the peer manager.
    // Core: v24.0.2 src/net_processing.cpp:2472-2476
    fn send_pings(&self) -> Option<()>;

    /// `CConnman::AddConnection` (`addconnection` RPC): `None` without the connection manager,
    /// `Some(false)` when at capacity for the connection type or total outbound slots, and
    /// `Some(true)` once the attempt ends.
    // Core: v24.0.2 src/net.cpp:2040-2075
    fn add_connection(
        &self,
        address: &str,
        conn_type: AddConnectionType,
        use_v2: bool,
    ) -> Option<bool>;

    /// Whether `node.addrman` exists, and when it does, whether CJDNS is reachable
    /// (`g_reachable_nets.Contains(NET_CJDNS)`). `None` without an address manager.
    // Core: v24.0.2 src/rpc/net.cpp:1023-1025, src/netbase.cpp:833-840
    fn addrman_cjdns_reachable(&self) -> Option<bool>;

    /// `LookupHost(host, false)`: numeric host lookup (including Tor/I2P, never DNS, never
    /// internal). Defaults to `dash_netaddr`'s pure numeric lookup; the live node overrides it
    /// with `dash_p2p::lookup::lookup_host`.
    // Core: v24.0.2 src/netbase.cpp:176-192
    fn lookup_host(&self, host: &str) -> Option<NetAddr> {
        if host.is_empty() || host.contains('\0') {
            return None;
        }
        let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
        dash_netaddr::lookup_host_numeric(host)
    }

    /// `AddrMan::Add({address}, source)` with `time_penalty = 0`: `None` without an address
    /// manager, else whether the address was added.
    // Core: v24.0.2 src/rpc/net.cpp:1036-1042
    fn add_peer_address(
        &self,
        service: &Service,
        services: u64,
        time: i64,
        source: &NetAddr,
    ) -> Option<bool>;

    /// `AddrMan::Good(service)` (`addpeeraddress` with `tried = true`).
    // Core: v24.0.2 src/rpc/net.cpp:1043-1046
    fn good_peer_address(&self, service: &Service);

    /// `CConnman::GetAddresses(count, /*max_pct=*/0, network)`: known addresses from AddrMan
    /// minus banned and discouraged entries (`count == 0` returns all). `None` without the
    /// connection manager.
    // Core: v24.0.2 src/rpc/net.cpp:983, src/net.cpp:4277-4286
    fn node_addresses(&self, count: usize, network: Option<Network>) -> Option<Vec<NodeAddress>>;

    /// `CConnman::ForNode(peer_id, PushMessage)`: pushes a raw `(msg_type, payload)` message to
    /// `peer_id` when it is fully connected (`FullyConnectedOnly`). `None` without the connection
    /// manager; `Some(false)` when no fully connected peer has `peer_id`.
    // Core: v24.0.2 src/rpc/net.cpp:1082-1096; src/net.h:1379-1381
    fn send_msg_to_peer(&self, peer_id: i64, msg_type: &str, payload: Vec<u8>) -> Option<bool>;

    /// `AddrMan::Size` for each routable network and across all networks (`getaddrmaninfo`).
    /// `None` without an address manager.
    // Core: v24.0.2 src/rpc/net.cpp:1154-1173
    fn addrman_info(&self) -> Option<AddrmanInfo>;

    /// `AddrMan::GetEntries(false)` (`new`) and `AddrMan::GetEntries(true)` (`tried`) for
    /// `getrawaddrman`. `None` without an address manager.
    // Core: v24.0.2 src/rpc/net.cpp:1178-1241
    fn raw_addrman(&self) -> Option<RawAddrman>;

    /// `Lookup(address, /*portDefault=*/0, /*fAllowLookup=*/false)`: numeric host and optional
    /// port lookup (including Tor/I2P, never DNS, never internal; port 0 when omitted). Defaults
    /// to `dash_netaddr::lookup`; the live node overrides it with `dash_p2p::lookup::lookup`.
    // Core: v24.0.2 src/netbase.cpp:194-217
    fn lookup_service(&self, address: &str) -> Option<Service> {
        dash_netaddr::lookup(address, 0)
    }

    /// `CConnman::OpenMasternodeConnection(CAddress(service, NODE_NETWORK), use_v2)`, then
    /// `CConnman::IsConnected(CAddress(service, NODE_NETWORK), CConnman::AllNodes)`: blocks until
    /// the connect attempt ends and reports whether a peer with `service` is in the connection
    /// table. `None` without the connection manager.
    // Core: v24.0.2 src/rpc/masternode.cpp:79-82; src/net.h:1345; src/net.cpp:3687-3690
    fn open_masternode_connection(&self, service: &Service, use_v2: bool) -> Option<bool>;

    /// `CConnman::GetTotalBytesRecv`, `GetTotalBytesSent` and upload-target state (`getnettotals`).
    /// `None` without the connection manager.
    // Core: v24.0.2 src/rpc/net.cpp:593-608
    fn net_totals(&self) -> Option<NetTotals>;
}

/// Core's `MAX_UPLOAD_TIMEFRAME`: 1 day in seconds.
// Core: v24.0.2 src/net.cpp:92
pub const MAX_UPLOAD_TIMEFRAME: u64 = 60 * 60 * 24;

/// `getnettotals`'s `uploadtarget` object.
// Core: v24.0.2 src/rpc/net.cpp:600-607; src/net.cpp:4665-4728
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadTarget {
    /// `GetMaxOutboundTimeframe` in seconds.
    pub timeframe: u64,
    /// `GetMaxOutboundTarget` in bytes.
    pub target: u64,
    /// `OutboundTargetReached(false)`.
    pub target_reached: bool,
    /// `!OutboundTargetReached(true)`.
    pub serve_historical_blocks: bool,
    /// `GetOutboundTargetBytesLeft`.
    pub bytes_left_in_cycle: u64,
    /// `GetMaxOutboundTimeLeftInCycle` in seconds.
    pub time_left_in_cycle: u64,
}

impl Default for UploadTarget {
    /// Core's values when `-maxuploadtarget` is unset (`nMaxOutboundLimit == 0`).
    // Core: v24.0.2 src/net.cpp:92, :4665-4728
    fn default() -> Self {
        Self {
            timeframe: MAX_UPLOAD_TIMEFRAME,
            target: 0,
            target_reached: false,
            serve_historical_blocks: true,
            bytes_left_in_cycle: 0,
            time_left_in_cycle: 0,
        }
    }
}

/// What `getnettotals` reads from the connection manager.
// Core: v24.0.2 src/rpc/net.cpp:593-608
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetTotals {
    /// `CConnman::GetTotalBytesRecv`.
    pub total_bytes_recv: u64,
    /// `CConnman::GetTotalBytesSent`.
    pub total_bytes_sent: u64,
    /// `uploadtarget` fields.
    pub upload_target: UploadTarget,
}

/// One network's counts in `getaddrmaninfo` (`AddrMan::Size`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AddrmanCounts {
    /// `Size(net, true)`.
    pub new: usize,
    /// `Size(net, false)`.
    pub tried: usize,
    /// `Size(net)`.
    pub total: usize,
}

/// `getaddrmaninfo`'s counts: `ipv4`, `ipv6`, `onion`, `i2p`, `cjdns`, and `all_networks`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AddrmanInfo {
    pub ipv4: AddrmanCounts,
    pub ipv6: AddrmanCounts,
    pub onion: AddrmanCounts,
    pub i2p: AddrmanCounts,
    pub cjdns: AddrmanCounts,
    pub all_networks: AddrmanCounts,
}

/// One address manager entry, as `getrawaddrman` shows it (`AddrmanEntryToJSON` + `AddressPosition`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawAddrmanEntry {
    /// `AddressPosition::bucket`.
    pub bucket: i32,
    /// `AddressPosition::position`.
    pub position: i32,
    /// `AddrInfo::ToStringAddr()`.
    pub address: String,
    /// `AddrInfo::GetPort()`.
    pub port: u16,
    /// `AddrInfo::nServices`.
    pub services: u64,
    /// `AddrInfo::nTime` (seconds since the UNIX epoch).
    pub time: i64,
    /// `GetNetworkName(info.GetNetClass())`.
    pub network: &'static str,
    /// `info.source.ToStringAddr()`.
    pub source: String,
    /// `GetNetworkName(info.source.GetNetClass())`.
    pub source_network: &'static str,
}

/// `getrawaddrman`'s `new` and `tried` tables, each in `AddrMan::GetEntries` order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawAddrman {
    /// `AddrMan::GetEntries(false)`.
    pub new: Vec<RawAddrmanEntry>,
    /// `AddrMan::GetEntries(true)`.
    pub tried: Vec<RawAddrmanEntry>,
}

/// One known address, as `getnodeaddresses` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeAddress {
    /// `nTime` (seconds since the UNIX epoch).
    pub time: i64,
    /// `nServices`.
    pub services: u64,
    /// `CNetAddr::ToStringAddr()`: no port, IPv6 without brackets.
    pub address: String,
    /// `CService::GetPort()`.
    pub port: u16,
    /// `GetNetworkName(addr.GetNetClass())`.
    pub network: &'static str,
}

/// One added node, as `getaddednodeinfo` shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct AddedNodeInfo {
    /// `m_params.m_added_node`, as given to `addnode`.
    pub added_node: String,
    /// When connected: `resolvedAddress.ToStringAddrPort()` and `fInbound`.
    pub connected: Option<(String, bool)>,
}

/// The node's state that `getnetworkinfo` shows, its types already rendered with Core's
/// helpers where they have one.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkInfo {
    /// `CLIENT_VERSION`.
    pub version: i64,
    /// `FormatFullVersion()`.
    pub build_version: String,
    /// `strSubVersion`.
    pub subversion: String,
    /// `PROTOCOL_VERSION`.
    pub protocol_version: i32,
    /// `CConnman::GetLocalServices`.
    pub local_services: u64,
    /// `!PeerManager::IgnoresIncomingTxs()`.
    pub local_relay: bool,
    /// `GetTimeOffset`.
    pub time_offset: i64,
    /// `CConnman::GetNetworkActive`.
    pub network_active: bool,
    /// `GetNodeCount` of every connection.
    pub connections: ConnectionCounts,
    /// `GetNodeCount` of the MNAUTH-verified connections (`ConnectionDirection::Verified`).
    pub connections_mn: ConnectionCounts,
    /// `SEMToString(GetSocketEventsMode())`.
    pub socket_events: &'static str,
    /// `GetNetworksInfo`'s networks: ipv4, ipv6, onion, i2p and cjdns, in that order.
    pub networks: Vec<NetworkReach>,
    /// The mempool's `m_min_relay_feerate` and `m_incremental_relay_feerate`, duffs per kB.
    pub relay_fees: Option<(i64, i64)>,
    /// `mapLocalHost`, in the map's order.
    pub local_addresses: Vec<LocalAddress>,
}

/// `GetNodeCount` for `ConnectionDirection::In` and `Out`; `Both` is their sum.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ConnectionCounts {
    pub inbound: u64,
    pub outbound: u64,
}

impl ConnectionCounts {
    /// One more connection, of the given direction.
    pub fn add(&mut self, inbound: bool) {
        if inbound {
            self.inbound += 1;
        } else {
            self.outbound += 1;
        }
    }
}

/// One network's reachability and proxy.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkReach {
    /// `GetNetworkName`.
    pub name: &'static str,
    /// `g_reachable_nets.Contains`.
    pub reachable: bool,
    /// `GetProxy`: the proxy's `Proxy::ToString` (its `ToStringAddrPort`, or its path for a unix
    /// socket) and `m_randomize_credentials`.
    pub proxy: Option<(String, bool)>,
}

/// One `mapLocalHost` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalAddress {
    /// `ToStringAddr`.
    pub address: String,
    /// `LocalServiceInfo::nPort`.
    pub port: u16,
    /// `LocalServiceInfo::nScore`.
    pub score: i32,
}

/// One peer's `CNodeStats` and `CNodeStateStats`, as `getpeerinfo` reads them. The network's
/// types are already rendered with Core's functions (`GetNetworkName`, `ToStringAddrPort`,
/// `NetPermissions::ToStrings`, `ConnectionTypeAsString`, `TransportTypeAsString`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeerInfo {
    /// `nodeid`.
    pub id: i64,
    /// `m_addr_name`.
    pub addr_name: String,
    /// `addrBind.ToStringAddrPort()`, `None` unless `addrBind.IsValid()`.
    pub addr_bind: Option<String>,
    /// `addrLocal`: empty until the peer reported a valid address.
    pub addr_local: String,
    /// `GetNetworkName(m_network)`.
    pub network: &'static str,
    /// `m_mapped_as`: 0 when unmapped.
    pub mapped_as: u32,
    /// `their_services`.
    pub services: u64,
    /// `m_relay_txs`.
    pub relay_txs: bool,
    /// `verifiedProRegTxHash`, a `uint256`'s bytes: all zero unless MNAUTH verified the peer.
    pub verified_proregtx_hash: [u8; 32],
    /// `verifiedPubKeyHash`, likewise.
    pub verified_pubkey_hash: [u8; 32],
    /// `m_last_send`, seconds.
    pub last_send: i64,
    /// `m_last_recv`, seconds.
    pub last_recv: i64,
    /// `m_last_tx_time`, seconds.
    pub last_tx_time: i64,
    /// `m_last_block_time`, seconds.
    pub last_block_time: i64,
    /// `nSendBytes`.
    pub send_bytes: u64,
    /// `nRecvBytes`.
    pub recv_bytes: u64,
    /// `m_connected`, seconds.
    pub connected: i64,
    /// `nTimeOffset`.
    pub time_offset: i64,
    /// `m_last_ping_time`, µs: 0 before the first pong.
    pub last_ping_us: i64,
    /// `m_min_ping_time`, µs: `i64::MAX` before the first pong.
    pub min_ping_us: i64,
    /// `m_ping_wait`, µs: 0 when no ping is outstanding.
    pub ping_wait_us: i64,
    /// `nVersion`.
    pub version: i32,
    /// `cleanSubVer`.
    pub clean_subver: String,
    /// `fInbound`.
    pub inbound: bool,
    /// `m_bip152_highbandwidth_to`.
    pub bip152_highbandwidth_to: bool,
    /// `m_bip152_highbandwidth_from`.
    pub bip152_highbandwidth_from: bool,
    /// `m_masternode_connection`.
    pub masternode_connection: bool,
    /// `m_misbehavior_score`.
    pub misbehavior_score: i32,
    /// `m_starting_height`.
    pub starting_height: i32,
    /// `nSyncHeight`.
    pub sync_height: i32,
    /// `nCommonHeight`.
    pub common_height: i32,
    /// `vHeightInFlight`.
    pub heights_in_flight: Vec<i32>,
    /// `m_addr_relay_enabled`.
    pub addr_relay_enabled: bool,
    /// `m_addr_processed`.
    pub addr_processed: u64,
    /// `m_addr_rate_limited`.
    pub addr_rate_limited: u64,
    /// `NetPermissions::ToStrings(m_permission_flags)`.
    pub permissions: Vec<&'static str>,
    /// `mapSendBytesPerMsgType`, zero entries included.
    pub send_bytes_per_msg: BTreeMap<String, u64>,
    /// `mapRecvBytesPerMsgType`, likewise.
    pub recv_bytes_per_msg: BTreeMap<String, u64>,
    /// `ConnectionTypeAsString(m_conn_type)`.
    pub conn_type: &'static str,
    /// `TransportTypeAsString(m_transport_type)`.
    pub transport_type: &'static str,
    /// `m_session_id`: the BIP324 session ID's hex, empty without one.
    pub session_id: String,
}

/// Registers every command of this module on `server`. `banscore` is
/// `IsDeprecatedRPCEnabled("banscore")`.
pub fn register_net_commands(
    server: &RpcServer,
    net: Arc<dyn NodeNet>,
    warnings: Arc<dyn NodeWarnings>,
    banscore: bool,
) {
    let count = Arc::clone(&net);
    server.append_command(Command::new("getconnectioncount", move |_| getconnectioncount(&*count)));
    let pinger = Arc::clone(&net);
    server.append_command(Command::new("ping", move |_| ping(&*pinger)));
    let peers = Arc::clone(&net);
    server.append_command(Command::new("getpeerinfo", move |_| getpeerinfo(&*peers, banscore)));
    let info = Arc::clone(&net);
    server.append_command(Command::new("getnetworkinfo", move |_| {
        getnetworkinfo(&*info, &*warnings)
    }));
    let active = Arc::clone(&net);
    server.append_command(Command::new("setnetworkactive", move |r| {
        setnetworkactive(&*active, &r.params)
    }));
    let add = Arc::clone(&net);
    server.append_command(Command::new("addnode", move |r| addnode(&*add, &r.params)));
    let disconnect = Arc::clone(&net);
    server.append_command(Command::new("disconnectnode", move |r| {
        disconnectnode(&*disconnect, &r.params)
    }));
    let added = Arc::clone(&net);
    server.append_command(Command::new("getaddednodeinfo", move |r| {
        getaddednodeinfo(&*added, &r.params)
    }));
    // Core: v24.0.2 src/rpc/net.cpp:1255
    let totals = Arc::clone(&net);
    server.append_command(Command::new("getnettotals", move |_| getnettotals(&*totals)));
    let addrs = Arc::clone(&net);
    server.append_command(Command::new("getnodeaddresses", move |r| {
        getnodeaddresses(&*addrs, &r.params)
    }));
    // Hidden commands (category "hidden" in dash-rpc-spec).
    // Core: v24.0.2 src/rpc/net.cpp:1263-1268
    let control = Arc::clone(server.control());
    let add_conn = Arc::clone(&net);
    server.append_command(Command::new("addconnection", move |r| {
        addconnection(&*control, &*add_conn, &r.params)
    }));
    let control = Arc::clone(server.control());
    let add_peer = Arc::clone(&net);
    server.append_command(Command::new("addpeeraddress", move |r| {
        addpeeraddress(&*control, &*add_peer, &r.params)
    }));
    let send_msg = Arc::clone(&net);
    server.append_command(Command::new("sendmsgtopeer", move |r| {
        sendmsgtopeer(&*send_msg, &r.params)
    }));
    let am_info = Arc::clone(&net);
    server.append_command(Command::new("getaddrmaninfo", move |_| getaddrmaninfo(&*am_info)));
    server.append_command(Command::new("getrawaddrman", move |_| getrawaddrman(&*net)));
}

/// `EnsureConnman`'s and `EnsurePeerman`'s error.
// Core: rpc/server_util.cpp:134-148
pub(crate) fn p2p_disabled() -> RpcError {
    RpcError::new(RPC_CLIENT_P2P_DISABLED, "Error: Peer-to-peer functionality missing or disabled")
}

/// `addpeeraddress`'s missing-addrman error.
// Core: v24.0.2 src/rpc/net.cpp:1023-1025
fn addrman_disabled() -> RpcError {
    RpcError::new(
        RPC_CLIENT_P2P_DISABLED,
        "Error: Address manager functionality missing or disabled",
    )
}

/// Core's `getconnectioncount`: `GetNodeCount(ConnectionDirection::Both)`, the count
/// `getnetworkinfo` shows as `connections`.
// Core: rpc/net.cpp:54-74
fn getconnectioncount(net: &dyn NodeNet) -> Result<UniValue, RpcError> {
    let counts = net.network_info().ok_or_else(p2p_disabled)?.connections;
    Ok((counts.inbound + counts.outbound).into())
}

/// Core's `ping`: `EnsurePeerman`, `SendPings`, null.
// Core: v24.0.2 src/rpc/net.cpp:76-98
fn ping(net: &dyn NodeNet) -> Result<UniValue, RpcError> {
    net.send_pings().ok_or_else(p2p_disabled)?;
    Ok(UniValue::Null)
}

/// Core's `getpeerinfo`: one object per peer, its keys in Core's order.
// Core: rpc/net.cpp:209-305
fn getpeerinfo(net: &dyn NodeNet, banscore: bool) -> Result<UniValue, RpcError> {
    let peers = net.peer_info().ok_or_else(p2p_disabled)?;
    Ok(array(peers.iter().map(|peer| peer_object(peer, banscore))))
}

fn peer_object(peer: &PeerInfo, banscore: bool) -> UniValue {
    let mut obj = UniValue::new_object();
    obj.push_kv_end("id", peer.id.into());
    obj.push_kv_end("addr", peer.addr_name.as_str().into());
    if let Some(addr_bind) = &peer.addr_bind {
        obj.push_kv_end("addrbind", addr_bind.as_str().into());
    }
    if !peer.addr_local.is_empty() {
        obj.push_kv_end("addrlocal", peer.addr_local.as_str().into());
    }
    obj.push_kv_end("network", peer.network.into());
    if peer.mapped_as != 0 {
        obj.push_kv_end("mapped_as", u64::from(peer.mapped_as).into());
    }
    obj.push_kv_end("services", format!("{:016x}", peer.services).into());
    obj.push_kv_end("servicesnames", array(service_names(peer.services)));
    obj.push_kv_end("relaytxes", peer.relay_txs.into());
    if peer.verified_proregtx_hash != [0; 32] {
        obj.push_kv_end("verified_proregtx_hash", uint256_hex(&peer.verified_proregtx_hash).into());
    }
    if peer.verified_pubkey_hash != [0; 32] {
        obj.push_kv_end("verified_pubkey_hash", uint256_hex(&peer.verified_pubkey_hash).into());
    }
    obj.push_kv_end("lastsend", peer.last_send.into());
    obj.push_kv_end("lastrecv", peer.last_recv.into());
    obj.push_kv_end("last_transaction", peer.last_tx_time.into());
    obj.push_kv_end("last_block", peer.last_block_time.into());
    obj.push_kv_end("bytessent", peer.send_bytes.into());
    obj.push_kv_end("bytesrecv", peer.recv_bytes.into());
    obj.push_kv_end("conntime", peer.connected.into());
    obj.push_kv_end("timeoffset", peer.time_offset.into());
    if peer.last_ping_us > 0 {
        obj.push_kv_end("pingtime", seconds(peer.last_ping_us));
    }
    if peer.min_ping_us < i64::MAX {
        obj.push_kv_end("minping", seconds(peer.min_ping_us));
    }
    if peer.ping_wait_us > 0 {
        obj.push_kv_end("pingwait", seconds(peer.ping_wait_us));
    }
    obj.push_kv_end("version", peer.version.into());
    obj.push_kv_end("subver", peer.clean_subver.as_str().into());
    obj.push_kv_end("inbound", peer.inbound.into());
    obj.push_kv_end("bip152_hb_to", peer.bip152_highbandwidth_to.into());
    obj.push_kv_end("bip152_hb_from", peer.bip152_highbandwidth_from.into());
    obj.push_kv_end("masternode", peer.masternode_connection.into());
    if banscore {
        obj.push_kv_end("banscore", peer.misbehavior_score.into());
    }
    obj.push_kv_end("startingheight", peer.starting_height.into());
    obj.push_kv_end("synced_headers", peer.sync_height.into());
    obj.push_kv_end("synced_blocks", peer.common_height.into());
    obj.push_kv_end("inflight", array(peer.heights_in_flight.iter().copied()));
    obj.push_kv_end("addr_relay_enabled", peer.addr_relay_enabled.into());
    obj.push_kv_end("addr_processed", peer.addr_processed.into());
    obj.push_kv_end("addr_rate_limited", peer.addr_rate_limited.into());
    obj.push_kv_end("permissions", array(peer.permissions.iter().copied()));
    obj.push_kv_end("bytessent_per_msg", per_msg(&peer.send_bytes_per_msg));
    obj.push_kv_end("bytesrecv_per_msg", per_msg(&peer.recv_bytes_per_msg));
    obj.push_kv_end("connection_type", peer.conn_type.into());
    obj.push_kv_end("transport_protocol_type", peer.transport_type.into());
    obj.push_kv_end("session_id", peer.session_id.as_str().into());
    obj
}

/// Core's `getnettotals`: node-wide byte counters, real system clock in milliseconds, and the
/// upload-target state.
// Core: v24.0.2 src/rpc/net.cpp:563-611
fn getnettotals(net: &dyn NodeNet) -> Result<UniValue, RpcError> {
    let totals = net.net_totals().ok_or_else(p2p_disabled)?;
    let mut obj = UniValue::new_object();
    obj.push_kv_end("totalbytesrecv", totals.total_bytes_recv.into());
    obj.push_kv_end("totalbytessent", totals.total_bytes_sent.into());
    obj.push_kv_end("timemillis", system_time_millis().into());

    let u = totals.upload_target;
    let mut outbound_limit = UniValue::new_object();
    outbound_limit.push_kv_end("timeframe", u.timeframe.into());
    outbound_limit.push_kv_end("target", u.target.into());
    outbound_limit.push_kv_end("target_reached", u.target_reached.into());
    outbound_limit.push_kv_end("serve_historical_blocks", u.serve_historical_blocks.into());
    outbound_limit.push_kv_end("bytes_left_in_cycle", u.bytes_left_in_cycle.into());
    outbound_limit.push_kv_end("time_left_in_cycle", u.time_left_in_cycle.into());
    obj.push_kv_end("uploadtarget", outbound_limit);
    Ok(obj)
}

/// Real system clock in milliseconds since the UNIX epoch (never mocktime).
// Core: v24.0.2 src/rpc/net.cpp:598
fn system_time_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

/// Core's `getnetworkinfo`. Core leaves out the connection manager's and the peer manager's
/// keys without them; the node has both whenever a command runs (from step 13 until shutdown
/// stops the server), so their absence is the P2P-disabled error, as for `getpeerinfo`.
// Core: rpc/net.cpp:632-739
fn getnetworkinfo(net: &dyn NodeNet, warnings: &dyn NodeWarnings) -> Result<UniValue, RpcError> {
    let info = net.network_info().ok_or_else(p2p_disabled)?;
    let mut obj = UniValue::new_object();
    obj.push_kv_end("version", info.version.into());
    obj.push_kv_end("buildversion", info.build_version.as_str().into());
    obj.push_kv_end("subversion", info.subversion.as_str().into());
    obj.push_kv_end("protocolversion", info.protocol_version.into());
    obj.push_kv_end("localservices", format!("{:016x}", info.local_services).into());
    obj.push_kv_end("localservicesnames", array(service_names(info.local_services)));
    obj.push_kv_end("localrelay", info.local_relay.into());
    obj.push_kv_end("timeoffset", info.time_offset.into());
    obj.push_kv_end("networkactive", info.network_active.into());
    for (key, counts) in
        [("connections", info.connections), ("connections_mn", info.connections_mn)]
    {
        obj.push_kv_end(key, (counts.inbound + counts.outbound).into());
        obj.push_kv_end(format!("{key}_in"), counts.inbound.into());
        obj.push_kv_end(format!("{key}_out"), counts.outbound.into());
    }
    obj.push_kv_end("socketevents", info.socket_events.into());
    obj.push_kv_end("networks", array(info.networks.iter().map(network_object)));
    if let Some((relay, incremental)) = info.relay_fees {
        obj.push_kv_end("relayfee", amount(relay));
        obj.push_kv_end("incrementalfee", amount(incremental));
    }
    obj.push_kv_end("localaddresses", array(info.local_addresses.iter().map(local_address_object)));
    obj.push_kv_end("warnings", warnings.get_warnings(false).into());
    Ok(obj)
}

// Core: rpc/net.cpp:613-630 GetNetworksInfo
fn network_object(reach: &NetworkReach) -> UniValue {
    let (proxy, randomize) = reach.proxy.as_ref().map_or(("", false), |(p, r)| (p.as_str(), *r));
    let mut obj = UniValue::new_object();
    obj.push_kv_end("name", reach.name.into());
    obj.push_kv_end("limited", (!reach.reachable).into());
    obj.push_kv_end("reachable", reach.reachable.into());
    obj.push_kv_end("proxy", proxy.into());
    obj.push_kv_end("proxy_randomize_credentials", randomize.into());
    obj
}

fn local_address_object(local: &LocalAddress) -> UniValue {
    let mut obj = UniValue::new_object();
    obj.push_kv_end("address", local.address.as_str().into());
    obj.push_kv_end("port", i64::from(local.port).into());
    obj.push_kv_end("score", local.score.into());
    obj
}

/// Core's `setnetworkactive`: the state it was given, as the connection manager now reports it.
// Core: rpc/net.cpp:917-935, net.cpp:3890-3910 CConnman::SetNetworkActive
fn setnetworkactive(net: &dyn NodeNet, params: &UniValue) -> Result<UniValue, RpcError> {
    let active = param(params, 0).get_bool()?;
    Ok(net.set_network_active(active).ok_or_else(p2p_disabled)?.into())
}

fn array<T: Into<UniValue>>(items: impl IntoIterator<Item = T>) -> UniValue {
    let mut arr = UniValue::new_array();
    for item in items {
        arr.push_back(item.into());
    }
    arr
}

/// `Ticks<SecondsDouble>(µs)`: `count() / 1e6` as a double, which `setFloat` writes with
/// `%.16g`.
fn seconds(us: i64) -> UniValue {
    UniValue::from_f64(us as f64 / 1e6).expect("a finite duration")
}

/// Core's `addnode`: the command is checked before the connection manager, and the
/// v2transport argument (default: whether we offer v2) before any command runs, `remove`
/// included. `onetry` answers null whatever the attempt did.
// Core: rpc/net.cpp:317-378
fn addnode(net: &dyn NodeNet, params: &UniValue) -> Result<UniValue, RpcError> {
    let command = param(params, 1).get_str()?;
    if !matches!(command, "onetry" | "add" | "remove") {
        // `self.ToString()`: the command's help.
        let help = dash_rpc_spec::command("addnode").expect("Core's command").help;
        return Err(RpcError::Runtime(help.to_owned()));
    }
    let node_v2 = net.v2_transport().ok_or_else(p2p_disabled)?;
    let node = param(params, 0).get_str()?;
    let v2_arg = param(params, 2);
    let use_v2 = if v2_arg.is_null() {
        node_v2
    } else {
        v2_arg.get_bool()?
    };
    if use_v2 && !node_v2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Error: v2transport requested but not enabled (see -v2transport)",
        ));
    }
    match command {
        "onetry" => net.open_one_try(node, use_v2),
        "add" if !net.add_node(node, use_v2) => {
            return Err(RpcError::new(RPC_CLIENT_NODE_ALREADY_ADDED, "Error: Node already added"));
        }
        "remove" if !net.remove_added_node(node) => {
            return Err(RpcError::new(
                RPC_CLIENT_NODE_NOT_ADDED,
                "Error: Node could not be removed. It has not been added previously.",
            ));
        }
        _ => {}
    }
    Ok(UniValue::Null)
}

/// Core's `disconnectnode`: by address when only the address is given, by id when the id is
/// given and the address is missing or empty; otherwise `-32602`. The connection manager is
/// checked first ([`NodeNet::v2_transport`] is `None` without it).
// Core: rpc/net.cpp:442-487
fn disconnectnode(net: &dyn NodeNet, params: &UniValue) -> Result<UniValue, RpcError> {
    net.v2_transport().ok_or_else(p2p_disabled)?;
    let (address, id) = (param(params, 0), param(params, 1));
    let disconnected = if !address.is_null() && id.is_null() {
        net.disconnect_by_address(address.get_str()?)
    } else if !id.is_null() && (address.is_null() || address.get_str().is_ok_and(str::is_empty)) {
        net.disconnect_by_id(get_int_i64(id)?)
    } else {
        return Err(RpcError::new(
            RPC_INVALID_PARAMS,
            "Only one of address and nodeid should be provided.",
        ));
    };
    if !disconnected {
        return Err(RpcError::new(
            RPC_CLIENT_NODE_NOT_CONNECTED,
            "Node not found in connected nodes",
        ));
    }
    Ok(UniValue::Null)
}

/// Core's `getaddednodeinfo`: every added node, or the one whose name is the argument (`-24`
/// when none is), each with its connection's address and direction while connected.
// Core: rpc/net.cpp:489-559
fn getaddednodeinfo(net: &dyn NodeNet, params: &UniValue) -> Result<UniValue, RpcError> {
    let mut infos = net.added_node_info().ok_or_else(p2p_disabled)?;
    let node = param(params, 0);
    if !node.is_null() {
        let node = node.get_str()?;
        let Some(info) = infos.into_iter().find(|info| info.added_node == node) else {
            return Err(RpcError::new(
                RPC_CLIENT_NODE_NOT_ADDED,
                "Error: Node has not been added.",
            ));
        };
        infos = vec![info];
    }
    Ok(array(infos.iter().map(|info| {
        let mut obj = UniValue::new_object();
        obj.push_kv_end("addednode", info.added_node.as_str().into());
        obj.push_kv_end("connected", info.connected.is_some().into());
        let addresses = info.connected.iter().map(|(address, inbound)| {
            let direction = if *inbound {
                "inbound"
            } else {
                "outbound"
            };
            let mut obj = UniValue::new_object();
            obj.push_kv_end("address", address.as_str().into());
            obj.push_kv_end("connected", direction.into());
            obj
        });
        obj.push_kv_end("addresses", array(addresses));
        obj
    })))
}

/// Core's `TrimString(str, " \f\n\r\t\v")`.
// Core: v24.0.2 src/util/string.h:31-44
fn trim_string(s: &str) -> &str {
    s.trim_matches(|c: char| matches!(c, ' ' | '\x0c' | '\n' | '\r' | '\t' | '\x0b'))
}

/// Core's `addconnection` (hidden, regtest only): opens one outbound connection of the named type.
// Core: v24.0.2 src/rpc/net.cpp:380-440
fn addconnection(
    control: &dyn NodeControl,
    net: &dyn NodeNet,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    if !control.is_mockable_chain() {
        return Err(RpcError::Runtime(
            "addconnection is for regression testing (-regtest mode) only.".into(),
        ));
    }
    let address = param(params, 0).get_str()?;
    let conn_type_in = trim_string(param(params, 1).get_str()?);
    let conn_type = match conn_type_in {
        "outbound-full-relay" => AddConnectionType::OutboundFullRelay,
        "block-relay-only" => AddConnectionType::BlockRelay,
        "addr-fetch" => AddConnectionType::AddrFetch,
        "feeler" => AddConnectionType::Feeler,
        _ => {
            let help = dash_rpc_spec::command("addconnection").expect("Core's command").help;
            return Err(RpcError::new(RPC_INVALID_PARAMETER, help));
        }
    };
    let use_v2 = !param(params, 2).is_null() && param(params, 2).get_bool()?;

    let node_v2 = net.v2_transport().ok_or_else(p2p_disabled)?;
    if use_v2 && !node_v2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Error: Adding v2transport connections requires -v2transport init flag to be set.",
        ));
    }
    let success = net.add_connection(address, conn_type, use_v2).ok_or_else(p2p_disabled)?;
    if !success {
        return Err(RpcError::new(
            RPC_CLIENT_NODE_CAPACITY_REACHED,
            "Error: Already at capacity for specified connection type.",
        ));
    }
    let mut info = UniValue::new_object();
    info.push_kv_end("address", address.into());
    info.push_kv_end("connection_type", conn_type_in.into());
    Ok(info)
}

/// Core's `addpeeraddress` (hidden): adds a numeric address to AddrMan with `NODE_NETWORK` and
/// itself as source, optionally moving it to the tried table.
// Core: v24.0.2 src/rpc/net.cpp:1000-1054
fn addpeeraddress(
    control: &dyn NodeControl,
    net: &dyn NodeNet,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let cjdns_reachable = net.addrman_cjdns_reachable().ok_or_else(addrman_disabled)?;
    let addr_string = param(params, 0).get_str()?;
    let port = get_int_u16(param(params, 1))?;
    let tried_arg = param(params, 2);
    let tried = if tried_arg.is_null() {
        false
    } else {
        tried_arg.get_bool()?
    };

    let mut success = false;
    if let Some(net_addr) = net.lookup_host(addr_string) {
        let service = Service::new(net_addr.maybe_flip_to_cjdns(cjdns_reachable), port);
        let now = control.now();
        // The source address is set equal to the address. This is equivalent to the peer
        // announcing itself.
        let added = net
            .add_peer_address(&service, NODE_NETWORK, now, &service.addr)
            .ok_or_else(addrman_disabled)?;
        if added {
            success = true;
            if tried {
                // Attempt to move the address to the tried addresses table.
                net.good_peer_address(&service);
            }
        }
    }

    let mut obj = UniValue::new_object();
    obj.push_kv_end("success", success.into());
    Ok(obj)
}

/// Core's `ParseNetwork`: case-insensitive; `"tor"` maps to `Onion` with Core's deprecation line.
// Core: v24.0.2 src/netbase.cpp:99-115 ParseNetwork
fn parse_network(net_in: &str) -> Network {
    match net_in.to_ascii_lowercase().as_str() {
        "ipv4" => Network::Ipv4,
        "ipv6" => Network::Ipv6,
        "onion" => Network::Onion,
        "tor" => {
            tracing::info!(
                "Warning: net name 'tor' is deprecated and will be removed in the future. You should use 'onion' instead."
            );
            Network::Onion
        }
        "i2p" => Network::I2p,
        "cjdns" => Network::Cjdns,
        _ => Network::Unroutable,
    }
}

/// Core's `getnodeaddresses`: known addresses from AddrMan minus banned/discouraged entries,
/// filtered by optional `count` (default 1, 0 = all) and optional `network`.
// Core: v24.0.2 src/rpc/net.cpp:939-998
fn getnodeaddresses(net: &dyn NodeNet, params: &UniValue) -> Result<UniValue, RpcError> {
    net.v2_transport().ok_or_else(p2p_disabled)?;
    let count_arg = param(params, 0);
    let count = if count_arg.is_null() {
        1
    } else {
        get_int_i32(count_arg)?
    };
    if count < 0 {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Address count out of range"));
    }
    let net_arg = param(params, 1);
    let network = if net_arg.is_null() {
        None
    } else {
        let raw = net_arg.get_str()?;
        let parsed = parse_network(raw);
        if parsed == Network::Unroutable {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Network not recognized: {raw}"),
            ));
        }
        Some(parsed)
    };
    let addrs = net.node_addresses(count as usize, network).ok_or_else(p2p_disabled)?;
    Ok(array(addrs.into_iter().map(|addr| {
        let mut obj = UniValue::new_object();
        obj.push_kv_end("time", addr.time.into());
        obj.push_kv_end("services", addr.services.into());
        obj.push_kv_end("address", addr.address.into());
        obj.push_kv_end("port", i64::from(addr.port).into());
        obj.push_kv_end("network", addr.network.into());
        obj
    })))
}

/// Core's `sendmsgtopeer` (hidden): sends a raw `(msg_type, payload)` message to a fully
/// connected peer by id.
// Core: v24.0.2 src/rpc/net.cpp:1056-1101
fn sendmsgtopeer(net: &dyn NodeNet, params: &UniValue) -> Result<UniValue, RpcError> {
    const COMMAND_SIZE: usize = 12;
    let peer_id = i64::from(get_int_i32(param(params, 0))?);
    let msg_type = param(params, 1).get_str()?;
    if msg_type.len() > COMMAND_SIZE {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!("Error: msg_type too long, max length is {COMMAND_SIZE}"),
        ));
    }
    let msg = param(params, 2).get_str()?;
    let payload = if msg.is_empty() {
        Vec::new()
    } else {
        parse_hex(msg)
            .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "Error parsing input for msg"))?
    };
    let sent = net.send_msg_to_peer(peer_id, msg_type, payload).ok_or_else(p2p_disabled)?;
    if !sent {
        return Err(RpcError::new(RPC_MISC_ERROR, "Error: Could not send message to peer"));
    }
    Ok(UniValue::Null)
}

/// Core's `getaddrmaninfo` (hidden): `new`, `tried` and `total` address counts per routable
/// network in enum order, then `all_networks`.
// Core: v24.0.2 src/rpc/net.cpp:1129-1176
fn getaddrmaninfo(net: &dyn NodeNet) -> Result<UniValue, RpcError> {
    let info = net.addrman_info().ok_or_else(addrman_disabled)?;
    let counts = |c: AddrmanCounts| {
        let mut obj = UniValue::new_object();
        obj.push_kv_end("new", (c.new as u64).into());
        obj.push_kv_end("tried", (c.tried as u64).into());
        obj.push_kv_end("total", (c.total as u64).into());
        obj
    };
    let mut ret = UniValue::new_object();
    ret.push_kv_end("ipv4", counts(info.ipv4));
    ret.push_kv_end("ipv6", counts(info.ipv6));
    ret.push_kv_end("onion", counts(info.onion));
    ret.push_kv_end("i2p", counts(info.i2p));
    ret.push_kv_end("cjdns", counts(info.cjdns));
    ret.push_kv_end("all_networks", counts(info.all_networks));
    Ok(ret)
}

/// Core's `getrawaddrman` (hidden): `new` and `tried` address manager tables keyed by
/// `"<bucket>/<position>"` in `GetEntries` order.
// Core: v24.0.2 src/rpc/net.cpp:1178-1244
fn getrawaddrman(net: &dyn NodeNet) -> Result<UniValue, RpcError> {
    let raw = net.raw_addrman().ok_or_else(addrman_disabled)?;
    let table = |entries: Vec<RawAddrmanEntry>| {
        let mut obj = UniValue::new_object();
        for entry in entries {
            let mut item = UniValue::new_object();
            item.push_kv_end("address", entry.address.into());
            item.push_kv_end("port", i64::from(entry.port).into());
            item.push_kv_end("services", entry.services.into());
            item.push_kv_end("time", entry.time.into());
            item.push_kv_end("network", entry.network.into());
            item.push_kv_end("source", entry.source.into());
            item.push_kv_end("source_network", entry.source_network.into());
            obj.push_kv_end(format!("{}/{}", entry.bucket, entry.position), item);
        }
        obj
    };
    let mut ret = UniValue::new_object();
    ret.push_kv_end("new", table(raw.new));
    ret.push_kv_end("tried", table(raw.tried));
    Ok(ret)
}

/// Core's `ValueFromAmount`: the amount in DASH with eight decimals, as a number.
// Core: core_write.cpp:35-46
pub(crate) fn amount(duffs: i64) -> UniValue {
    let sign = if duffs < 0 {
        "-"
    } else {
        ""
    };
    let abs = duffs.unsigned_abs();
    let text = format!("{sign}{}.{:08}", abs / 100_000_000, abs % 100_000_000);
    UniValue::from_num_str(&text).expect("a decimal number")
}

/// A message-type map's non-zero entries, in the map's (`std::map`'s) order.
fn per_msg(map: &BTreeMap<String, u64>) -> UniValue {
    let mut obj = UniValue::new_object();
    for (msg, &bytes) in map {
        if bytes > 0 {
            obj.push_kv_end(msg.as_str(), bytes.into());
        }
    }
    obj
}

/// `uint256::ToString`: the bytes in reverse, as hex.
fn uint256_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// Core's `serviceFlagsToStr`: each set bit's name, lowest first.
// Core: protocol.cpp:345-373
fn service_names(services: u64) -> impl Iterator<Item = String> {
    (0..64).filter(move |bit| services & (1u64 << bit) != 0).map(|bit| {
        let name = match bit {
            0 => "NETWORK",
            2 => "BLOOM",
            6 => "COMPACT_FILTERS",
            10 => "NETWORK_LIMITED",
            11 => "HEADERS_COMPRESSED",
            12 => "P2P_V2",
            _ => return format!("UNKNOWN[2^{bit}]"),
        };
        name.to_owned()
    })
}

mod mnauth;
mod mnthread;

pub use mnauth::register_mnauth_commands;
pub use mnthread::register_mnthread_commands;

#[cfg(test)]
mod tests;
