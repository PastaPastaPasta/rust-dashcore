//! `rpc/net.cpp`'s commands over the network the node provides ([`NodeNet`]): `getpeerinfo`
//! (DEC-279), `getnetworkinfo` and `setnetworkactive` (BL-391), `addnode` (BL-394), and
//! `disconnectnode` and `getaddednodeinfo` (BL-427).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::UniValue;
use crate::protocol::{
    RPC_CLIENT_NODE_ALREADY_ADDED, RPC_CLIENT_NODE_NOT_ADDED, RPC_CLIENT_NODE_NOT_CONNECTED,
    RPC_CLIENT_P2P_DISABLED, RPC_INVALID_PARAMETER, RPC_INVALID_PARAMS, RpcError,
};
use crate::request::{get_int_i64, param};
use crate::server::{Command, RpcServer};
use crate::warnings::NodeWarnings;

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

    /// `CConnman::GetAddedNodeInfo(/*include_connected=*/true)`: every added node in the list's
    /// order. `None` without the connection manager.
    fn added_node_info(&self) -> Option<Vec<AddedNodeInfo>>;
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
    server.append_command(Command::new("getaddednodeinfo", move |r| {
        getaddednodeinfo(&*net, &r.params)
    }));
}

/// `EnsureConnman`'s and `EnsurePeerman`'s error.
// Core: rpc/server_util.cpp:134-148
fn p2p_disabled() -> RpcError {
    RpcError::new(RPC_CLIENT_P2P_DISABLED, "Error: Peer-to-peer functionality missing or disabled")
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

#[cfg(test)]
mod tests;
