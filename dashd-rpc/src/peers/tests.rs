use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use dash_netaddr::{NetAddr, Network, Service};

use super::{
    AddConnectionType, AddedNodeInfo, AddrmanCounts, AddrmanInfo, ConnectionCounts, LocalAddress,
    NODE_NETWORK, NetTotals, NetworkInfo, NetworkReach, NodeAddress, NodeNet, PeerInfo, RawAddrman,
    RawAddrmanEntry, UploadTarget, amount, register_net_commands, service_names,
};
use crate::UniValue;
use crate::protocol::{
    RPC_CLIENT_NODE_CAPACITY_REACHED, RPC_CLIENT_P2P_DISABLED, RPC_INVALID_PARAMETER,
    RPC_MISC_ERROR, RPC_TYPE_ERROR, RpcError,
};
use crate::request::JsonRpcRequest;
use crate::server::RpcServer;
use crate::testutil::FakeControl;
use crate::warnings::NodeWarnings;

/// The peers and the network's state; `None` without a connection manager. `addnode` reads the
/// v2 bit of the state's services and records its calls. `disconnectnode` drops from `connected`
/// (id, name) the first entry it matches; `getaddednodeinfo` lists `added` with the connection
/// `links` gives a name.
#[derive(Default)]
struct FakeNet {
    peers: Option<Vec<PeerInfo>>,
    info: Mutex<Option<NetworkInfo>>,
    added: Mutex<Vec<(String, bool)>>,
    tried: Mutex<Vec<(String, bool)>>,
    connected: Mutex<Vec<(i64, String)>>,
    links: BTreeMap<String, (String, bool)>,
    pings: Mutex<usize>,
    add_conn_at_capacity: bool,
    add_conns: Mutex<Vec<(String, AddConnectionType, bool)>>,
    cjdns_reachable: bool,
    add_peer_reject: bool,
    added_peers: Mutex<Vec<(Service, u64, i64, NetAddr)>>,
    good_peers: Mutex<Vec<Service>>,
    node_addrs: Vec<NodeAddress>,
    node_addr_calls: Mutex<Vec<(usize, Option<Network>)>>,
    sent_msgs: Mutex<Vec<(i64, String, Vec<u8>)>>,
    send_peers: Vec<i64>,
    addrman_info_val: Option<AddrmanInfo>,
    raw_addrman_val: Option<RawAddrman>,
    totals: Option<NetTotals>,
}

impl FakeNet {
    fn with_peers(peers: Vec<PeerInfo>) -> FakeNet {
        FakeNet {
            peers: Some(peers),
            ..FakeNet::default()
        }
    }

    fn with_info(info: NetworkInfo) -> FakeNet {
        FakeNet {
            info: Mutex::new(Some(info)),
            ..FakeNet::default()
        }
    }
}

impl NodeNet for FakeNet {
    fn peer_info(&self) -> Option<Vec<PeerInfo>> {
        self.peers.clone()
    }

    fn network_info(&self) -> Option<NetworkInfo> {
        self.info.lock().unwrap().clone()
    }

    fn set_network_active(&self, active: bool) -> Option<bool> {
        let mut info = self.info.lock().unwrap();
        let info = info.as_mut()?;
        info.network_active = active;
        Some(info.network_active)
    }

    fn v2_transport(&self) -> Option<bool> {
        let info = self.info.lock().unwrap();
        info.as_ref().map(|info| info.local_services & NODE_P2P_V2 != 0)
    }

    fn open_one_try(&self, node: &str, use_v2: bool) {
        self.tried.lock().unwrap().push((node.to_owned(), use_v2));
    }

    fn add_node(&self, node: &str, use_v2: bool) -> bool {
        let mut added = self.added.lock().unwrap();
        let fresh = !added.iter().any(|(n, _)| n == node);
        if fresh {
            added.push((node.to_owned(), use_v2));
        }
        fresh
    }

    fn remove_added_node(&self, node: &str) -> bool {
        let mut added = self.added.lock().unwrap();
        let before = added.len();
        added.retain(|(n, _)| n != node);
        added.len() != before
    }

    fn disconnect_by_address(&self, address: &str) -> bool {
        let mut connected = self.connected.lock().unwrap();
        let found = connected.iter().position(|(_, name)| name == address);
        found.map(|i| connected.remove(i)).is_some()
    }

    fn disconnect_by_id(&self, id: i64) -> bool {
        let mut connected = self.connected.lock().unwrap();
        let found = connected.iter().position(|&(peer, _)| peer == id);
        found.map(|i| connected.remove(i)).is_some()
    }
    fn try_mark_verified(&self, _: i64, _: [u8; 32], _: [u8; 32]) -> Option<bool> {
        unreachable!()
    }
    fn set_masternode_thread_active(&self, _: bool) -> Option<bool> {
        unreachable!()
    }

    fn added_node_info(&self) -> Option<Vec<AddedNodeInfo>> {
        // No network state stands for no connection manager.
        self.info.lock().unwrap().as_ref()?;
        let added = self.added.lock().unwrap();
        let info = added.iter().map(|(node, _)| AddedNodeInfo {
            added_node: node.clone(),
            connected: self.links.get(node).cloned(),
        });
        Some(info.collect())
    }

    fn fetch_block(&self, _: &dash_primitives::BlockHash, _: i64) -> Option<Result<(), String>> {
        unreachable!("no net command fetches a block")
    }

    fn send_pings(&self) -> Option<()> {
        self.info.lock().unwrap().as_ref()?;
        *self.pings.lock().unwrap() += 1;
        Some(())
    }

    fn add_connection(
        &self,
        address: &str,
        conn_type: AddConnectionType,
        use_v2: bool,
    ) -> Option<bool> {
        self.info.lock().unwrap().as_ref()?;
        self.add_conns.lock().unwrap().push((address.to_owned(), conn_type, use_v2));
        Some(!self.add_conn_at_capacity)
    }

    fn addrman_cjdns_reachable(&self) -> Option<bool> {
        self.info.lock().unwrap().as_ref()?;
        Some(self.cjdns_reachable)
    }

    fn add_peer_address(
        &self,
        service: &Service,
        services: u64,
        time: i64,
        source: &NetAddr,
    ) -> Option<bool> {
        self.info.lock().unwrap().as_ref()?;
        self.added_peers.lock().unwrap().push((service.clone(), services, time, source.clone()));
        Some(!self.add_peer_reject)
    }

    fn good_peer_address(&self, service: &Service) {
        self.good_peers.lock().unwrap().push(service.clone());
    }

    fn node_addresses(&self, count: usize, network: Option<Network>) -> Option<Vec<NodeAddress>> {
        self.info.lock().unwrap().as_ref()?;
        self.node_addr_calls.lock().unwrap().push((count, network));
        let filtered = self
            .node_addrs
            .iter()
            .filter(|a| network.is_none_or(|n| a.network == n.name()))
            .cloned();
        let out = if count == 0 {
            filtered.collect()
        } else {
            filtered.take(count).collect()
        };
        Some(out)
    }

    fn send_msg_to_peer(&self, peer_id: i64, msg_type: &str, payload: Vec<u8>) -> Option<bool> {
        self.info.lock().unwrap().as_ref()?;
        if !self.send_peers.contains(&peer_id) {
            return Some(false);
        }
        self.sent_msgs.lock().unwrap().push((peer_id, msg_type.to_owned(), payload));
        Some(true)
    }

    fn addrman_info(&self) -> Option<AddrmanInfo> {
        self.addrman_info_val
    }

    fn raw_addrman(&self) -> Option<RawAddrman> {
        self.raw_addrman_val.clone()
    }

    fn open_masternode_connection(&self, _: &Service, _: bool) -> Option<bool> {
        unreachable!("masternode connect is tested in masternode::tests")
    }

    fn net_totals(&self) -> Option<NetTotals> {
        self.totals
    }
}

/// `NODE_P2P_V2` (protocol.h).
const NODE_P2P_V2: u64 = 1 << 12;

/// rc.3's warning: the pin is a release candidate.
const PRE_RELEASE: &str = "This is a pre-release test build - use at your own risk - do not use for mining or merchant applications";

struct FakeWarnings;

impl NodeWarnings for FakeWarnings {
    fn get_warnings(&self, verbose: bool) -> String {
        assert!(!verbose, "getnetworkinfo reads the concise text");
        PRE_RELEASE.to_owned()
    }
}

fn server(net: impl Into<Arc<FakeNet>>, banscore: bool) -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    crate::commands::register_control_commands(&server);
    register_net_commands(&server, net.into(), Arc::new(FakeWarnings), banscore);
    server.start();
    server.set_warmup_finished();
    server
}

fn run(server: &RpcServer, method: &str, params: Vec<UniValue>) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: method.into(),
        params: UniValue::Arr(params),
        ..Default::default()
    })
}

/// The reply's or the error's JSON.
fn reply(server: &RpcServer, method: &str, params: Vec<UniValue>) -> String {
    match run(server, method, params) {
        Ok(reply) => reply.write(),
        Err(e) => e.to_object().write(),
    }
}

/// `getpeerinfo` without arguments (DIFF-RPC compares the arity checks).
fn call(net: FakeNet, banscore: bool) -> Result<UniValue, RpcError> {
    run(&server(net, banscore), "getpeerinfo", vec![])
}

fn getpeerinfo(peers: Vec<PeerInfo>, banscore: bool) -> String {
    call(FakeNet::with_peers(peers), banscore).unwrap().write()
}

fn counts(entries: &[(&str, u64)]) -> BTreeMap<String, u64> {
    entries.iter().map(|&(m, n)| (m.to_owned(), n)).collect()
}

/// The keys of a reply's first peer, in order.
fn keys(reply: &str) -> Vec<String> {
    let Some(UniValue::Arr(peers)) = UniValue::read(reply.as_bytes()) else {
        panic!("{reply}")
    };
    let UniValue::Obj(peer) = &peers[0] else {
        panic!("{reply}")
    };
    peer.iter().map(|(k, _)| k.clone()).collect()
}

/// rc.3's reply for an inbound v2 peer (a second dashd), recorded live.
const LIVE: &str = r#"{"id":0,"addr":"127.0.0.1:56756","addrbind":"127.0.0.1:19871","network":"not_publicly_routable","services":"0000000000001c05","servicesnames":["NETWORK","BLOOM","NETWORK_LIMITED","HEADERS_COMPRESSED","P2P_V2"],"relaytxes":true,"lastsend":1792521385,"lastrecv":1792521385,"last_transaction":0,"last_block":0,"bytessent":4623,"bytesrecv":1160,"conntime":1792521378,"timeoffset":0,"pingtime":0.002608,"minping":0.002608,"version":70242,"subver":"/Dash Core:24.0.0/","inbound":true,"bip152_hb_to":false,"bip152_hb_from":false,"masternode":false,"startingheight":0,"synced_headers":-1,"synced_blocks":-1,"inflight":[],"addr_relay_enabled":true,"addr_processed":0,"addr_rate_limited":0,"permissions":[],"bytessent_per_msg":{"getheaders2":90,"getsporks":21,"headers2":22,"ping":29,"pong":29,"sendaddrv2":33,"sendcmpct":30,"senddsq":22,"sendheaders2":21,"verack":33,"version":170},"bytesrecv_per_msg":{"getaddr":33,"getheaders2":90,"getsporks":21,"headers2":22,"ping":29,"pong":29,"sendaddrv2":33,"sendcmpct":30,"senddsq":22,"sendheaders2":21,"verack":33,"version":170},"connection_type":"inbound","transport_protocol_type":"v2","session_id":"db4656524d9d4ccf4cfe56ea4bf3878e5ebcbd71f102f212c4f5c9eb4d6900ee"}"#;

/// The stats behind [`LIVE`]; the zero-count message types Core's maps start with are kept.
fn live() -> PeerInfo {
    let both = [
        ("getheaders2", 90),
        ("getsporks", 21),
        ("headers2", 22),
        ("ping", 29),
        ("pong", 29),
        ("sendaddrv2", 33),
        ("sendcmpct", 30),
        ("senddsq", 22),
        ("sendheaders2", 21),
        ("verack", 33),
        ("version", 170),
    ];
    let mut sent = counts(&both);
    sent.insert("getaddr".into(), 0);
    sent.insert("*other*".into(), 0);
    let mut recv = counts(&both);
    recv.insert("getaddr".into(), 33);
    recv.insert("tx".into(), 0);
    PeerInfo {
        id: 0,
        addr_name: "127.0.0.1:56756".into(),
        addr_bind: Some("127.0.0.1:19871".into()),
        addr_local: String::new(),
        network: "not_publicly_routable",
        mapped_as: 0,
        services: 0x1c05,
        relay_txs: true,
        verified_proregtx_hash: [0; 32],
        verified_pubkey_hash: [0; 32],
        last_send: 1792521385,
        last_recv: 1792521385,
        last_tx_time: 0,
        last_block_time: 0,
        send_bytes: 4623,
        recv_bytes: 1160,
        connected: 1792521378,
        time_offset: 0,
        last_ping_us: 2608,
        min_ping_us: 2608,
        ping_wait_us: 0,
        version: 70242,
        clean_subver: "/Dash Core:24.0.0/".into(),
        inbound: true,
        bip152_highbandwidth_to: false,
        bip152_highbandwidth_from: false,
        masternode_connection: false,
        misbehavior_score: 0,
        starting_height: 0,
        sync_height: -1,
        common_height: -1,
        heights_in_flight: vec![],
        addr_relay_enabled: true,
        addr_processed: 0,
        addr_rate_limited: 0,
        permissions: vec![],
        send_bytes_per_msg: sent,
        recv_bytes_per_msg: recv,
        conn_type: "inbound",
        transport_type: "v2",
        session_id: "db4656524d9d4ccf4cfe56ea4bf3878e5ebcbd71f102f212c4f5c9eb4d6900ee".into(),
    }
}

#[test]
fn a_peer_is_written_as_core_wrote_it() {
    assert_eq!(getpeerinfo(vec![live()], false), format!("[{LIVE}]"));
    assert_eq!(getpeerinfo(vec![], false), "[]");
}

/// Every optional key present, in rc.3's order (net.cpp:216-302): `addrbind` left out,
/// `addrlocal` and `mapped_as` by the address, the MNAUTH hashes after `relaytxes`
/// (`uint256::ToString`, reversed), the pings as seconds, and `banscore` only with
/// `-deprecatedrpc=banscore`.
#[test]
fn the_optional_keys_appear_in_cores_order() {
    let mut proreg = [0; 32];
    proreg[0] = 0xab;
    let mut pubkey = [0; 32];
    pubkey[31] = 0xcd;
    let peer = PeerInfo {
        id: 7,
        addr_bind: None,
        addr_local: "1.2.3.4:9999".into(),
        mapped_as: 4_294_967_295,
        verified_proregtx_hash: proreg,
        verified_pubkey_hash: pubkey,
        last_ping_us: 1,
        min_ping_us: 123_456_789,
        ping_wait_us: 1_500_000,
        misbehavior_score: 20,
        heights_in_flight: vec![5, 3],
        permissions: vec!["noban", "download"],
        masternode_connection: true,
        ..live()
    };
    let written = getpeerinfo(vec![peer.clone()], true);
    let expected = [
        "id",
        "addr",
        "addrlocal",
        "network",
        "mapped_as",
        "services",
        "servicesnames",
        "relaytxes",
        "verified_proregtx_hash",
        "verified_pubkey_hash",
        "lastsend",
        "lastrecv",
        "last_transaction",
        "last_block",
        "bytessent",
        "bytesrecv",
        "conntime",
        "timeoffset",
        "pingtime",
        "minping",
        "pingwait",
        "version",
        "subver",
        "inbound",
        "bip152_hb_to",
        "bip152_hb_from",
        "masternode",
        "banscore",
        "startingheight",
        "synced_headers",
        "synced_blocks",
        "inflight",
        "addr_relay_enabled",
        "addr_processed",
        "addr_rate_limited",
        "permissions",
        "bytessent_per_msg",
        "bytesrecv_per_msg",
        "connection_type",
        "transport_protocol_type",
        "session_id",
    ];
    assert_eq!(keys(&written), expected);
    for part in [
        r#""addrlocal":"1.2.3.4:9999","network""#,
        r#""mapped_as":4294967295,"services""#,
        &format!(r#""verified_proregtx_hash":"{}ab""#, "00".repeat(31)),
        &format!(r#""verified_pubkey_hash":"cd{}""#, "00".repeat(31)),
        r#""pingtime":1e-06,"minping":123.456789,"pingwait":1.5,"version""#,
        r#""masternode":true,"banscore":20,"startingheight""#,
        r#""inflight":[5,3]"#,
        r#""permissions":["noban","download"]"#,
    ] {
        assert!(written.contains(part), "{part} in {written}");
    }
    assert!(!getpeerinfo(vec![peer], false).contains("banscore"));
}

/// `serviceFlagsToStr`: every set bit, lowest first, unnamed ones as `UNKNOWN[2^n]`.
#[test]
fn service_names_are_cores() {
    assert_eq!(service_names(0).count(), 0);
    let names: Vec<String> = service_names(1 | 1 << 1 | 1 << 6 | 1 << 12 | 1 << 63).collect();
    assert_eq!(names, ["NETWORK", "UNKNOWN[2^1]", "COMPACT_FILTERS", "P2P_V2", "UNKNOWN[2^63]"]);
    let peer = PeerInfo {
        services: u64::MAX,
        ..live()
    };
    assert!(getpeerinfo(vec![peer], false).contains(r#""services":"ffffffffffffffff""#));
}

/// The per-message maps keep the non-zero entries in byte order (`*other*` first).
#[test]
fn the_message_maps_skip_zero_counts_in_byte_order() {
    let peer = PeerInfo {
        recv_bytes_per_msg: counts(&[("version", 1), ("*other*", 2), ("addr", 0), ("Z", 3)]),
        ..live()
    };
    assert!(
        getpeerinfo(vec![peer], false)
            .contains(r#""bytesrecv_per_msg":{"*other*":2,"Z":3,"version":1},"connection_type""#)
    );
}

#[test]
fn without_a_network_it_is_cores_p2p_disabled() {
    let e = call(FakeNet::default(), false).unwrap_err();
    assert_eq!(e.code(), RPC_CLIENT_P2P_DISABLED);
    assert_eq!(
        e.to_object().write(),
        r#"{"code":-31,"message":"Error: Peer-to-peer functionality missing or disabled"}"#
    );
}

/// rc.3's `getnetworkinfo` on a fresh regtest node with no peers: the `result` of the raw HTTP
/// body `contrib/node/getnetworkinfo-fixtures.py` records from `dashd` v24.0.0-rc.3 (config
/// `default`: `-regtest -printtoconsole=0 -disablewallet -rpcuser=u -rpcpassword=p -rpcport=P
/// -port=P+1 -dnsseed=0 -fixedseeds=0 -bind=127.0.0.1 -listenonion=0`), Core's own text with its
/// amounts in eight decimals. `listen0` and `connect0` give the same bytes.
const DEFAULT: &str = r#"{"version":240000,"buildversion":"v24.0.0-rc.3","subversion":"/Dash Core:24.0.0/","protocolversion":70242,"localservices":"0000000000001c05","localservicesnames":["NETWORK","BLOOM","NETWORK_LIMITED","HEADERS_COMPRESSED","P2P_V2"],"localrelay":true,"timeoffset":0,"networkactive":true,"connections":0,"connections_in":0,"connections_out":0,"connections_mn":0,"connections_mn_in":0,"connections_mn_out":0,"socketevents":"epoll","networks":[{"name":"ipv4","limited":false,"reachable":true,"proxy":"","proxy_randomize_credentials":false},{"name":"ipv6","limited":false,"reachable":true,"proxy":"","proxy_randomize_credentials":false},{"name":"onion","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false},{"name":"i2p","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false},{"name":"cjdns","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false}],"relayfee":0.00001000,"incrementalfee":0.00001000,"localaddresses":[],"warnings":"This is a pre-release test build - use at your own risk - do not use for mining or merchant applications"}"#;

/// The state behind [`DEFAULT`].
fn rc3() -> NetworkInfo {
    NetworkInfo {
        version: 240_000,
        build_version: "v24.0.0-rc.3".into(),
        subversion: "/Dash Core:24.0.0/".into(),
        protocol_version: 70242,
        local_services: 0x1c05,
        local_relay: true,
        time_offset: 0,
        network_active: true,
        connections: ConnectionCounts::default(),
        connections_mn: ConnectionCounts::default(),
        socket_events: "epoll",
        networks: networks(&["ipv4", "ipv6"], &[]),
        relay_fees: Some((1000, 1000)),
        local_addresses: vec![],
    }
}

/// The five networks, `reachable` those named, with `proxies` (network, randomize).
fn networks(reachable: &[&str], proxies: &[(&str, bool)]) -> Vec<NetworkReach> {
    ["ipv4", "ipv6", "onion", "i2p", "cjdns"]
        .into_iter()
        .map(|name| NetworkReach {
            name,
            reachable: reachable.contains(&name),
            proxy: proxies
                .iter()
                .find(|(net, _)| *net == name)
                .map(|&(_, randomize)| ("127.0.0.1:9".to_owned(), randomize)),
        })
        .collect()
}

fn getnetworkinfo(info: NetworkInfo) -> String {
    run(&server(FakeNet::with_info(info), false), "getnetworkinfo", vec![]).unwrap().write()
}

#[test]
fn the_network_is_written_as_core_wrote_it() {
    assert_eq!(getnetworkinfo(rc3()), DEFAULT);
}

/// rc.3's `networks` under `-proxy=127.0.0.1:9` (with and without `-proxyrandomize=0`),
/// `-onlynet=onion -onion=127.0.0.1:9` and `-onlynet=ipv4`, recorded as [`DEFAULT`] (the
/// script's `proxy`, `proxy_norand`, `onlynet_onion` and `onlynet_ipv4`, each with
/// `-listenonion=0`):
/// `-proxy` sets every network's proxy but i2p's, cjdns staying unreachable; `limited` is
/// `!reachable`. rdashd refuses these options (P2a-net) and reports no proxy, but the
/// rendering is Core's for whichever state the node reaches.
#[test]
fn the_networks_are_rendered_as_core_rendered_them() {
    let proxy = |randomize| {
        format!(
            r#""networks":[{{"name":"ipv4","limited":false,"reachable":true,"proxy":"127.0.0.1:9","proxy_randomize_credentials":{randomize}}},{{"name":"ipv6","limited":false,"reachable":true,"proxy":"127.0.0.1:9","proxy_randomize_credentials":{randomize}}},{{"name":"onion","limited":false,"reachable":true,"proxy":"127.0.0.1:9","proxy_randomize_credentials":{randomize}}},{{"name":"i2p","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false}},{{"name":"cjdns","limited":true,"reachable":false,"proxy":"127.0.0.1:9","proxy_randomize_credentials":{randomize}}}],"relayfee""#
        )
    };
    let onlynet_onion = r#""networks":[{"name":"ipv4","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false},{"name":"ipv6","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false},{"name":"onion","limited":false,"reachable":true,"proxy":"127.0.0.1:9","proxy_randomize_credentials":true},{"name":"i2p","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false},{"name":"cjdns","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false}],"relayfee""#;
    let onlynet_ipv4 = r#""networks":[{"name":"ipv4","limited":false,"reachable":true,"proxy":"","proxy_randomize_credentials":false},{"name":"ipv6","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false},{"name":"onion","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false},{"name":"i2p","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false},{"name":"cjdns","limited":true,"reachable":false,"proxy":"","proxy_randomize_credentials":false}],"relayfee""#;
    for randomize in [true, false] {
        let proxied =
            [("ipv4", randomize), ("ipv6", randomize), ("onion", randomize), ("cjdns", randomize)];
        let info = NetworkInfo {
            networks: networks(&["ipv4", "ipv6", "onion"], &proxied),
            ..rc3()
        };
        let written = getnetworkinfo(info);
        assert!(written.contains(&proxy(randomize)), "{written}");
    }
    let info = NetworkInfo {
        networks: networks(&["onion"], &[("onion", true)]),
        ..rc3()
    };
    assert!(getnetworkinfo(info).contains(onlynet_onion));
    let info = NetworkInfo {
        networks: networks(&["ipv4"], &[]),
        ..rc3()
    };
    assert!(getnetworkinfo(info).contains(onlynet_ipv4));
}

/// `-blocksonly=1` (rc.3, the script's `blocksonly`) clears `localrelay`; `-networkactive=0` clears
/// `networkactive`. The counts are `GetNodeCount`'s, the totals the sums; the local addresses
/// follow the map.
#[test]
fn the_state_fields_follow_the_node() {
    let info = NetworkInfo {
        local_relay: false,
        network_active: false,
        time_offset: -3,
        connections: ConnectionCounts {
            inbound: 2,
            outbound: 3,
        },
        connections_mn: ConnectionCounts {
            inbound: 0,
            outbound: 1,
        },
        local_addresses: vec![
            LocalAddress {
                address: "1.2.3.4".into(),
                port: 9999,
                score: 4,
            },
            LocalAddress {
                address: "2001:db8::1".into(),
                port: 19899,
                score: 1,
            },
        ],
        ..rc3()
    };
    let written = getnetworkinfo(info);
    for part in [
        r#""localrelay":false,"timeoffset":-3,"networkactive":false,"connections":5,"connections_in":2,"connections_out":3,"connections_mn":1,"connections_mn_in":0,"connections_mn_out":1,"socketevents""#,
        r#""localaddresses":[{"address":"1.2.3.4","port":9999,"score":4},{"address":"2001:db8::1","port":19899,"score":1}],"warnings""#,
    ] {
        assert!(written.contains(part), "{part} in {written}");
    }
    // Core leaves the fees out without a mempool.
    let info = NetworkInfo {
        relay_fees: None,
        ..rc3()
    };
    assert!(
        getnetworkinfo(info).contains(r#""proxy_randomize_credentials":false}],"localaddresses""#)
    );
}

/// `setnetworkactive` answers the state the connection manager reports (rc.3, the script's
/// `networkactive0`: `true` after `-networkactive=0`), which `getnetworkinfo` then shows.
#[test]
fn setnetworkactive_sets_and_answers_the_state() {
    let server = server(FakeNet::with_info(rc3()), false);
    let info = |server: &RpcServer| run(server, "getnetworkinfo", vec![]).unwrap().write();
    assert_eq!(run(&server, "setnetworkactive", vec![false.into()]).unwrap().write(), "false");
    assert!(info(&server).contains(r#""networkactive":false"#));
    assert_eq!(run(&server, "setnetworkactive", vec![true.into()]).unwrap().write(), "true");
    assert_eq!(info(&server), DEFAULT);
    // Core's arity error: the help, as a plain runtime error.
    let help = dash_rpc_spec::command("setnetworkactive").unwrap().help;
    let e = run(&server, "setnetworkactive", vec![]).unwrap_err();
    assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{e:?}");
}

/// `ValueFromAmount`: `%d.%08d` with the sign in front.
#[test]
fn amounts_are_cores() {
    for (duffs, text) in [
        (1000, "0.00001000"),
        (0, "0.00000000"),
        (100_000_000, "1.00000000"),
        (-1, "-0.00000001"),
        (-150_000_000, "-1.50000000"),
        (i64::MIN, "-92233720368.54775808"),
        (i64::MAX, "92233720368.54775807"),
    ] {
        assert_eq!(amount(duffs).write(), text);
    }
}

/// `getconnectioncount` is `GetNodeCount(Both)`: `getnetworkinfo`'s `connections`, both
/// directions summed. It takes no argument: Core's arity error is the help.
#[test]
fn getconnectioncount_counts_both_directions() {
    let info = NetworkInfo {
        connections: ConnectionCounts {
            inbound: 2,
            outbound: 3,
        },
        connections_mn: ConnectionCounts {
            inbound: 0,
            outbound: 1,
        },
        ..rc3()
    };
    let fresh = server(FakeNet::with_info(rc3()), false);
    assert_eq!(run(&fresh, "getconnectioncount", vec![]).unwrap().write(), "0");
    let server = server(FakeNet::with_info(info), false);
    assert_eq!(run(&server, "getconnectioncount", vec![]).unwrap().write(), "5");
    assert!(
        run(&server, "getnetworkinfo", vec![]).unwrap().write().contains(r#""connections":5,"#)
    );
    let help = dash_rpc_spec::command("getconnectioncount").unwrap().help;
    let e = run(&server, "getconnectioncount", vec![1.into()]).unwrap_err();
    assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{e:?}");
}

#[test]
fn without_a_network_the_network_commands_are_cores_p2p_disabled() {
    let server = server(FakeNet::default(), false);
    for (method, params) in [
        ("getconnectioncount", vec![]),
        ("getnetworkinfo", vec![]),
        ("setnetworkactive", vec![true.into()]),
        ("ping", vec![]),
    ] {
        let e = run(&server, method, params).unwrap_err();
        assert_eq!(e.code(), RPC_CLIENT_P2P_DISABLED, "{method}");
    }
}

/// `ping` queues a ping on every peer through `SendPings` and answers null. It takes no
/// argument: Core's arity error is the help, and nothing is queued.
#[test]
fn ping_queues_pings_and_answers_null() {
    let net = Arc::new(FakeNet::with_info(rc3()));
    let server = server(Arc::clone(&net), false);
    assert_eq!(run(&server, "ping", vec![]).unwrap().write(), "null");
    assert_eq!(*net.pings.lock().unwrap(), 1);
    let help = dash_rpc_spec::command("ping").unwrap().help;
    let e = run(&server, "ping", vec![1.into()]).unwrap_err();
    assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{e:?}");
    assert_eq!(*net.pings.lock().unwrap(), 1);
}

fn args(node: &str, command: &str) -> Vec<UniValue> {
    vec![node.into(), command.into()]
}

/// `addnode`'s replies and errors, Core's codes and texts (rpc/net.cpp:317-378; rc.3 recorded
/// live, the DIFF-RPC cases): null for each command that succeeds, `-23` for a node already
/// listed, `-24` for one that is not, and an unknown command's help as a plain runtime error.
#[test]
fn addnode_answers_as_core() {
    let server = server(FakeNet::with_info(rc3()), false);
    assert_eq!(reply(&server, "addnode", args("127.0.0.1:1", "add")), "null");
    assert_eq!(
        reply(&server, "addnode", args("127.0.0.1:1", "add")),
        r#"{"code":-23,"message":"Error: Node already added"}"#
    );
    assert_eq!(reply(&server, "addnode", args("127.0.0.1:1", "remove")), "null");
    assert_eq!(
        reply(&server, "addnode", args("127.0.0.1:1", "remove")),
        r#"{"code":-24,"message":"Error: Node could not be removed. It has not been added previously."}"#
    );
    assert_eq!(reply(&server, "addnode", args("127.0.0.1:2", "onetry")), "null");
    let help = dash_rpc_spec::command("addnode").unwrap().help;
    let bad = run(&server, "addnode", args("127.0.0.1:1", "Add")).unwrap_err();
    assert!(matches!(&bad, RpcError::Runtime(m) if m == help), "{bad:?}");
}

/// The v2 argument defaults to whether we offer v2; asking for v2 when we do not is `-8`, for
/// `remove` too, as Core checks it before any command.
#[test]
fn addnode_takes_v2_as_core() {
    let net = Arc::new(FakeNet::with_info(rc3()));
    let offering = server(Arc::clone(&net), false);
    for params in [args("a:1", "add"), args("b:1", "onetry")] {
        assert_eq!(reply(&offering, "addnode", params), "null");
    }
    let mut v1 = args("c:1", "add");
    v1.push(false.into());
    assert_eq!(reply(&offering, "addnode", v1), "null");
    assert_eq!(*net.added.lock().unwrap(), [("a:1".to_owned(), true), ("c:1".to_owned(), false)]);
    assert_eq!(*net.tried.lock().unwrap(), [("b:1".to_owned(), true)]);

    let no_v2 = NetworkInfo {
        local_services: 0x1c05 & !NODE_P2P_V2,
        ..rc3()
    };
    let refusing = server(FakeNet::with_info(no_v2), false);
    let refused = r#"{"code":-8,"message":"Error: v2transport requested but not enabled (see -v2transport)"}"#;
    for command in ["add", "remove", "onetry"] {
        let mut params = args("a:1", command);
        params.push(true.into());
        assert_eq!(reply(&refusing, "addnode", params), refused, "{command}");
    }
    assert_eq!(reply(&refusing, "addnode", args("a:1", "add")), "null");
}

/// The command is checked before the connection manager (rpc/net.cpp:338-345).
#[test]
fn addnode_checks_the_command_first() {
    let server = server(FakeNet::default(), false);
    let bad = run(&server, "addnode", args("a:1", "nope")).unwrap_err();
    assert_eq!(bad.code(), -1);
    let e = run(&server, "addnode", args("a:1", "add")).unwrap_err();
    assert_eq!(e.code(), RPC_CLIENT_P2P_DISABLED);
}

/// Two connected peers: `0` by the name `a:1`, `7` by `b:1`.
fn connected_net() -> FakeNet {
    FakeNet {
        connected: Mutex::new(vec![(0, "a:1".into()), (7, "b:1".into())]),
        ..FakeNet::with_info(rc3())
    }
}

const NOT_CONNECTED: &str = r#"{"code":-29,"message":"Node not found in connected nodes"}"#;
const ONLY_ONE: &str =
    r#"{"code":-32602,"message":"Only one of address and nodeid should be provided."}"#;

/// `disconnectnode`'s forms (rpc/net.cpp:466-484): the address alone, or the id with the address
/// missing or empty; both, or neither, are `-32602`. A peer not found is `-29`, the address
/// `""` included.
#[test]
fn disconnectnode_picks_its_form_as_core() {
    let net = Arc::new(connected_net());
    let by_id = server(Arc::clone(&net), false);
    let disconnect = |params: Vec<UniValue>| reply(&by_id, "disconnectnode", params);
    assert_eq!(disconnect(vec!["a:1".into(), 7.into()]), ONLY_ONE);
    assert_eq!(disconnect(vec![]), ONLY_ONE);
    assert_eq!(disconnect(vec![UniValue::Null, UniValue::Null]), ONLY_ONE);
    assert_eq!(disconnect(vec!["".into()]), NOT_CONNECTED);
    assert_eq!(disconnect(vec!["7".into()]), NOT_CONNECTED, "an id as the address is a name");
    assert_eq!(disconnect(vec!["".into(), 7.into()]), "null");
    assert_eq!(*net.connected.lock().unwrap(), [(0, "a:1".to_owned())]);
    assert_eq!(disconnect(vec![UniValue::Null, 7.into()]), NOT_CONNECTED);
    assert_eq!(disconnect(vec![UniValue::Null, 0.into()]), "null");
    assert!(net.connected.lock().unwrap().is_empty());

    let net = Arc::new(connected_net());
    let by_address = server(Arc::clone(&net), false);
    assert_eq!(reply(&by_address, "disconnectnode", vec!["b:1".into()]), "null");
    assert_eq!(reply(&by_address, "disconnectnode", vec!["b:1".into()]), NOT_CONNECTED);
    assert_eq!(*net.connected.lock().unwrap(), [(0, "a:1".to_owned())]);
}

/// The id is read with `getInt<int64_t>`: a fraction is `-1`, an id no peer has is `-29`.
#[test]
fn disconnectnode_reads_the_id_as_core() {
    let server = server(connected_net(), false);
    let fraction = UniValue::from_num_str("1.5").unwrap();
    assert_eq!(
        reply(&server, "disconnectnode", vec!["".into(), fraction]),
        r#"{"code":-1,"message":"JSON integer out of range"}"#
    );
    for id in [-1i64, i64::MAX] {
        assert_eq!(reply(&server, "disconnectnode", vec!["".into(), id.into()]), NOT_CONNECTED);
    }
}

/// `getaddednodeinfo` lists every added node in order, connected or not, each with an
/// `addresses` array (empty while not connected); with a node, that one entry, matched by the
/// whole name (rpc/net.cpp:522-555).
#[test]
fn getaddednodeinfo_answers_as_core() {
    let net = FakeNet {
        added: Mutex::new(vec![
            ("a:1".into(), true),
            ("name".into(), false),
            ("127.0.0.1:5".into(), false),
        ]),
        links: [("name".to_owned(), ("127.0.0.1:9".to_owned(), true))].into(),
        ..FakeNet::with_info(rc3())
    };
    let listed = server(net, false);
    let a = r#"{"addednode":"a:1","connected":false,"addresses":[]}"#;
    let name = r#"{"addednode":"name","connected":true,"addresses":[{"address":"127.0.0.1:9","connected":"inbound"}]}"#;
    let numeric = r#"{"addednode":"127.0.0.1:5","connected":false,"addresses":[]}"#;
    assert_eq!(reply(&listed, "getaddednodeinfo", vec![]), format!("[{a},{name},{numeric}]"));
    assert_eq!(reply(&listed, "getaddednodeinfo", vec!["name".into()]), format!("[{name}]"));
    for other in ["nam", "[::ffff:127.0.0.1]:5"] {
        assert_eq!(
            reply(&listed, "getaddednodeinfo", vec![other.into()]),
            r#"{"code":-24,"message":"Error: Node has not been added."}"#,
            "{other}: by the whole name, not the address"
        );
    }
    let empty = server(FakeNet::with_info(rc3()), false);
    assert_eq!(reply(&empty, "getaddednodeinfo", vec![]), "[]");
}

/// Both commands check the connection manager before their arguments.
#[test]
fn without_a_network_disconnectnode_and_getaddednodeinfo_are_p2p_disabled() {
    let server = server(FakeNet::default(), false);
    for (method, params) in [
        ("disconnectnode", vec!["a:1".into(), 1.into()]),
        ("disconnectnode", vec!["a:1".into()]),
        ("getaddednodeinfo", vec!["a:1".into()]),
    ] {
        let e = run(&server, method, params).unwrap_err();
        assert_eq!(e.code(), RPC_CLIENT_P2P_DISABLED, "{method}");
    }
}

fn server_on_chain(net: impl Into<Arc<FakeNet>>, regtest: bool) -> Arc<RpcServer> {
    let control = FakeControl::default();
    control.main_chain.store(!regtest, Ordering::SeqCst);
    let server = Arc::new(RpcServer::new(Arc::new(control)));
    crate::commands::register_control_commands(&server);
    register_net_commands(&server, net.into(), Arc::new(FakeWarnings), false);
    server.start();
    server.set_warmup_finished();
    server
}

fn run_json(server: &RpcServer, method: &str, params: &str) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: method.into(),
        params: UniValue::read(params.as_bytes()).unwrap(),
        ..Default::default()
    })
}

/// In `dash-rpc-spec`, `addconnection` and `addpeeraddress` carry category `"hidden"` (so the
/// `help` overview omits them while `help <cmd>` returns Core's text), whereas `getnodeaddresses`
/// is in category `"network"` and appears in the overview (Core: v24.0.2 src/rpc/net.cpp:1260-1264).
#[test]
fn addconnection_and_addpeeraddress_are_hidden_from_help() {
    let server = server(FakeNet::with_info(rc3()), false);
    let overview = run(&server, "help", vec![]).unwrap().get_str().unwrap().to_owned();
    assert!(!overview.contains("addconnection"), "{overview}");
    assert!(!overview.contains("addpeeraddress"), "{overview}");
    assert!(overview.contains("getnodeaddresses"), "{overview}");
    for cmd in ["addconnection", "addpeeraddress", "getnodeaddresses"] {
        let help = run(&server, "help", vec![cmd.into()]).unwrap().get_str().unwrap().to_owned();
        assert_eq!(help, dash_rpc_spec::command(cmd).unwrap().help, "{cmd}");
    }
}

/// `addconnection` argument arity, type checks and named arguments (Core: v24.0.2
/// src/rpc/net.cpp:380-405).
#[test]
fn addconnection_argument_errors() {
    let net = Arc::new(FakeNet::with_info(rc3()));
    let server = server(Arc::clone(&net), false);
    let help = dash_rpc_spec::command("addconnection").unwrap().help;
    for params in [
        "[]",
        r#"["127.0.0.1:1"]"#,
        r#"["127.0.0.1:1","feeler"]"#,
        r#"["127.0.0.1:1","feeler",false,1]"#,
    ] {
        let e = run_json(&server, "addconnection", params).unwrap_err();
        assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{params}: {e:?}");
        assert_eq!(e.code(), RPC_MISC_ERROR);
    }
    for params in [
        r#"[1,"feeler",false]"#,
        r#"[null,"feeler",false]"#,
        r#"["127.0.0.1:1",1,false]"#,
        r#"["127.0.0.1:1",null,false]"#,
        r#"["127.0.0.1:1","feeler","false"]"#,
        r#"["127.0.0.1:1","feeler",null]"#,
    ] {
        let e = run_json(&server, "addconnection", params).unwrap_err();
        assert_eq!(e.code(), RPC_TYPE_ERROR, "{params}: {e:?}");
    }
    assert!(net.add_conns.lock().unwrap().is_empty());

    // Named arguments map to the three positions.
    let named = run_json(
        &server,
        "addconnection",
        r#"{"address":"127.0.0.1:9999","connection_type":"feeler","v2transport":false}"#,
    )
    .unwrap();
    assert_eq!(named.write(), r#"{"address":"127.0.0.1:9999","connection_type":"feeler"}"#);
}

/// `addconnection` refuses non-regtest networks before checking `connection_type` or the
/// connection manager (Core: v24.0.2 src/rpc/net.cpp:401-403).
#[test]
fn addconnection_is_regtest_only() {
    let server = server_on_chain(FakeNet::default(), false);
    let e = run_json(&server, "addconnection", r#"["127.0.0.1:1","bogus",false]"#).unwrap_err();
    assert_eq!(
        e.to_object().write(),
        r#"{"code":-1,"message":"addconnection is for regression testing (-regtest mode) only."}"#
    );
}

/// All four connection types, `TrimString` trimming, and `-8` with the help text for any other
/// type (checked before `EnsureConnman`; Core: v24.0.2 src/rpc/net.cpp:406-418, :433-436).
#[test]
fn addconnection_types_and_bad_type() {
    let net = Arc::new(FakeNet::with_info(rc3()));
    let server = server(Arc::clone(&net), false);
    let cases = [
        ("outbound-full-relay", "outbound-full-relay", AddConnectionType::OutboundFullRelay, true),
        ("block-relay-only", "block-relay-only", AddConnectionType::BlockRelay, false),
        ("addr-fetch", "addr-fetch", AddConnectionType::AddrFetch, true),
        (" \t\r\n\x0b\x0cfeeler \t", "feeler", AddConnectionType::Feeler, false),
    ];
    for (input, trimmed, expected_type, use_v2) in cases {
        let res = run(
            &server,
            "addconnection",
            vec!["127.0.0.1:9999".into(), input.into(), use_v2.into()],
        )
        .unwrap();
        assert_eq!(
            res.write(),
            format!(r#"{{"address":"127.0.0.1:9999","connection_type":"{trimmed}"}}"#)
        );
        let calls = net.add_conns.lock().unwrap();
        assert_eq!(calls.last().unwrap(), &("127.0.0.1:9999".to_owned(), expected_type, use_v2));
    }

    // Bad connection type returns RPC_INVALID_PARAMETER (-8) with the help text, before
    // EnsureConnman.
    let help = dash_rpc_spec::command("addconnection").unwrap().help;
    let no_net = server_on_chain(FakeNet::default(), true);
    for bad in ["manual", "inbound", "unknown", "", "OUTBOUND-FULL-RELAY"] {
        let e = run(&no_net, "addconnection", vec!["127.0.0.1:1".into(), bad.into(), false.into()])
            .unwrap_err();
        assert_eq!(e.code(), RPC_INVALID_PARAMETER, "{bad}");
        assert!(
            matches!(&e, RpcError::Json { code: RPC_INVALID_PARAMETER, message } if message == help),
            "{bad}: {e:?}"
        );
    }
}

/// `v2transport` check against `NODE_P2P_V2`, capacity-reached `-34`, and P2P-disabled `-31`
/// (Core: v24.0.2 src/rpc/net.cpp:422-431, src/rpc/server_util.cpp:134-140).
#[test]
fn addconnection_v2_capacity_and_p2p_disabled() {
    // Missing connection manager -> EnsureConnman's -31.
    let no_net = server(FakeNet::default(), false);
    assert_eq!(
        reply(&no_net, "addconnection", vec!["127.0.0.1:1".into(), "feeler".into(), false.into()]),
        r#"{"code":-31,"message":"Error: Peer-to-peer functionality missing or disabled"}"#
    );

    // Without NODE_P2P_V2: v2transport=true refused with -8, v2transport=false accepted.
    let no_v2_info = NetworkInfo {
        local_services: 0x1c05 & !NODE_P2P_V2,
        ..rc3()
    };
    let no_v2_net = Arc::new(FakeNet::with_info(no_v2_info));
    let no_v2 = server(Arc::clone(&no_v2_net), false);
    assert_eq!(
        reply(&no_v2, "addconnection", vec!["127.0.0.1:1".into(), "feeler".into(), true.into()]),
        r#"{"code":-8,"message":"Error: Adding v2transport connections requires -v2transport init flag to be set."}"#
    );
    assert!(no_v2_net.add_conns.lock().unwrap().is_empty());
    assert_eq!(
        reply(&no_v2, "addconnection", vec!["127.0.0.1:1".into(), "feeler".into(), false.into()]),
        r#"{"address":"127.0.0.1:1","connection_type":"feeler"}"#
    );

    // Capacity reached -> RPC_CLIENT_NODE_CAPACITY_REACHED (-34).
    let full = FakeNet {
        add_conn_at_capacity: true,
        ..FakeNet::with_info(rc3())
    };
    let full_server = server(full, false);
    let e = run(
        &full_server,
        "addconnection",
        vec!["127.0.0.1:1".into(), "outbound-full-relay".into(), true.into()],
    )
    .unwrap_err();
    assert_eq!(e.code(), RPC_CLIENT_NODE_CAPACITY_REACHED);
    assert_eq!(
        e.to_object().write(),
        r#"{"code":-34,"message":"Error: Already at capacity for specified connection type."}"#
    );
}

/// `addpeeraddress` arity, JSON type checks, `uint16_t` port range, and named arguments
/// (Core: v24.0.2 src/rpc/net.cpp:1000-1030, src/univalue/include/univalue.h:136-147).
#[test]
fn addpeeraddress_argument_errors() {
    let net = Arc::new(FakeNet::with_info(rc3()));
    let server = server(Arc::clone(&net), false);
    let help = dash_rpc_spec::command("addpeeraddress").unwrap().help;
    for params in ["[]", r#"["1.2.3.4"]"#, r#"["1.2.3.4",9999,false,1]"#] {
        let e = run_json(&server, "addpeeraddress", params).unwrap_err();
        assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{params}: {e:?}");
        assert_eq!(e.code(), RPC_MISC_ERROR);
    }
    for params in [
        r#"[1,9999]"#,
        r#"[null,9999]"#,
        r#"["1.2.3.4","9999"]"#,
        r#"["1.2.3.4",null]"#,
        r#"["1.2.3.4",9999,"true"]"#,
        r#"["1.2.3.4",9999,1]"#,
    ] {
        let e = run_json(&server, "addpeeraddress", params).unwrap_err();
        assert_eq!(e.code(), RPC_TYPE_ERROR, "{params}: {e:?}");
    }
    for params in
        [r#"["1.2.3.4",-1]"#, r#"["1.2.3.4",65536]"#, r#"["1.2.3.4",1.5]"#, r#"["1.2.3.4",1e2]"#]
    {
        let e = run_json(&server, "addpeeraddress", params).unwrap_err();
        assert_eq!(
            e.to_object().write(),
            r#"{"code":-1,"message":"JSON integer out of range"}"#,
            "{params}"
        );
    }
    assert!(net.added_peers.lock().unwrap().is_empty());

    // Boundary ports and named arguments succeed.
    for params in [
        r#"["1.2.3.4",0]"#,
        r#"["1.2.3.4",65535]"#,
        r#"{"address":"1.2.3.4","port":9999,"tried":true}"#,
    ] {
        assert_eq!(
            run_json(&server, "addpeeraddress", params).unwrap().write(),
            r#"{"success":true}"#,
            "{params}"
        );
    }
}

/// `addpeeraddress` success, failure, `tried`, source == address, `NODE_NETWORK`, CJDNS flip,
/// and unresolvable hosts (Core: v24.0.2 src/rpc/net.cpp:1027-1052).
#[test]
fn addpeeraddress_success_failure_and_tried() {
    let net = Arc::new(FakeNet::with_info(rc3()));
    // Also works off regtest (not regtest-only in Core).
    let server = server_on_chain(Arc::clone(&net), false);

    // Default tried (omitted and explicit null) and tried=false: adds with source == addr and
    // NODE_NETWORK, does not call Good.
    for params in [r#"["1.2.3.4",9999]"#, r#"["1.2.3.4",9999,null]"#, r#"["1.2.3.4",9999,false]"#] {
        assert_eq!(
            run_json(&server, "addpeeraddress", params).unwrap().write(),
            r#"{"success":true}"#,
            "{params}"
        );
    }
    assert!(net.good_peers.lock().unwrap().is_empty());
    let expected_addr = NetAddr::ipv4([1, 2, 3, 4]);
    let expected_service = Service::new(expected_addr.clone(), 9999);
    {
        let added = net.added_peers.lock().unwrap();
        assert_eq!(added.len(), 3);
        for (service, services, time, source) in added.iter() {
            assert_eq!(service, &expected_service);
            assert_eq!(*services, NODE_NETWORK);
            assert_eq!(*time, 1_000_100);
            assert_eq!(source, &service.addr);
        }
    }

    // tried=true moves the added address to the tried table via Good.
    assert_eq!(
        run_json(&server, "addpeeraddress", r#"["[2001:db8::1]",8333,true]"#).unwrap().write(),
        r#"{"success":true}"#
    );
    let v6_service = Service::new(NetAddr::from_ip("2001:db8::1".parse().unwrap()), 8333);
    assert_eq!(*net.good_peers.lock().unwrap(), std::slice::from_ref(&v6_service));
    let last_added = net.added_peers.lock().unwrap().last().unwrap().clone();
    assert_eq!(last_added, (v6_service.clone(), NODE_NETWORK, 1_000_100, v6_service.addr));

    // Unresolvable / hostname / internal address -> {"success":false}, neither Add nor Good.
    let before = net.added_peers.lock().unwrap().len();
    for bad in ["example.com", "", "not-an-ip", "fd6b:88c0:8724:1:2:3:4:5"] {
        assert_eq!(
            reply(&server, "addpeeraddress", vec![bad.into(), 9999.into(), true.into()]),
            r#"{"success":false}"#,
            "{bad}"
        );
    }
    assert_eq!(net.added_peers.lock().unwrap().len(), before);
    assert_eq!(net.good_peers.lock().unwrap().len(), 1);

    // AddrMan::Add returning false -> {"success":false}, Good not called even when tried=true.
    let rejecting = Arc::new(FakeNet {
        add_peer_reject: true,
        ..FakeNet::with_info(rc3())
    });
    let reject_server = server_on_chain(Arc::clone(&rejecting), true);
    assert_eq!(
        run_json(&reject_server, "addpeeraddress", r#"["1.2.3.4",9999,true]"#).unwrap().write(),
        r#"{"success":false}"#
    );
    assert_eq!(rejecting.added_peers.lock().unwrap().len(), 1);
    assert!(rejecting.good_peers.lock().unwrap().is_empty());

    // CJDNS flip when CJDNS is unreachable: fc00::1 stays IPv6 (no flip; Core: netbase.cpp:833-840).
    let no_cjdns_net = Arc::new(FakeNet {
        cjdns_reachable: false,
        ..FakeNet::with_info(rc3())
    });
    let no_cjdns_server = server_on_chain(Arc::clone(&no_cjdns_net), true);
    assert_eq!(
        run_json(&no_cjdns_server, "addpeeraddress", r#"["fc00::1",9999]"#).unwrap().write(),
        r#"{"success":true}"#
    );
    let (svc, services, _, source) = no_cjdns_net.added_peers.lock().unwrap()[0].clone();
    assert_eq!(svc.addr.net(), Network::Ipv6);
    assert_eq!(source, svc.addr);
    assert_eq!(services, NODE_NETWORK);

    // CJDNS flip when CJDNS is reachable: both service.addr and source flip to CJDNS.
    let cjdns_net = Arc::new(FakeNet {
        cjdns_reachable: true,
        ..FakeNet::with_info(rc3())
    });
    let cjdns_server = server_on_chain(Arc::clone(&cjdns_net), true);
    assert_eq!(
        run_json(&cjdns_server, "addpeeraddress", r#"["fc00::1",9999,true]"#).unwrap().write(),
        r#"{"success":true}"#
    );
    let (svc, services, _, source) = cjdns_net.added_peers.lock().unwrap()[0].clone();
    assert_eq!(svc.addr.net(), Network::Cjdns);
    assert_eq!(source, svc.addr);
    assert_eq!(services, NODE_NETWORK);
    assert_eq!(*cjdns_net.good_peers.lock().unwrap(), [svc]);
}

/// Missing address manager returns `-31` with Core's addrman message, checked before port range
/// validation (Core: v24.0.2 src/rpc/net.cpp:1023-1028).
#[test]
fn addpeeraddress_without_addrman_is_p2p_disabled() {
    let server = server(FakeNet::default(), false);
    for params in [r#"["1.2.3.4",9999]"#, r#"["1.2.3.4",70000]"#] {
        let e = run_json(&server, "addpeeraddress", params).unwrap_err();
        assert_eq!(e.code(), RPC_CLIENT_P2P_DISABLED, "{params}");
        assert_eq!(
            e.to_object().write(),
            r#"{"code":-31,"message":"Error: Address manager functionality missing or disabled"}"#,
            "{params}"
        );
    }
}

fn sample_node_addresses() -> Vec<NodeAddress> {
    vec![
        NodeAddress {
            time: 1_700_000_001,
            services: NODE_NETWORK,
            address: "1.2.3.4".into(),
            port: 9999,
            network: "ipv4",
        },
        NodeAddress {
            time: 1_700_000_002,
            services: NODE_NETWORK | NODE_P2P_V2,
            address: "2001:db8::1".into(),
            port: 8333,
            network: "ipv6",
        },
        NodeAddress {
            time: 1_700_000_003,
            services: NODE_NETWORK,
            address: "fc00::1".into(),
            port: 9999,
            network: "cjdns",
        },
        NodeAddress {
            time: 1_700_000_004,
            services: NODE_NETWORK,
            address: "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion".into(),
            port: 9999,
            network: "onion",
        },
    ]
}

/// `getnodeaddresses` default count (1), 0 = all, result fields (IPv4, unbracketed IPv6, CJDNS,
/// onion), and named arguments (Core: v24.0.2 src/rpc/net.cpp:939-998).
#[test]
fn getnodeaddresses_counts_networks_and_fields() {
    let net = Arc::new(FakeNet {
        node_addrs: sample_node_addresses(),
        ..FakeNet::with_info(rc3())
    });
    let server = server(Arc::clone(&net), false);

    // Default count is 1 (omitted and explicit null).
    let expected_first =
        r#"[{"time":1700000001,"services":1,"address":"1.2.3.4","port":9999,"network":"ipv4"}]"#;
    for params in ["[]", "[null]", "[null,null]"] {
        assert_eq!(
            run_json(&server, "getnodeaddresses", params).unwrap().write(),
            expected_first,
            "{params}"
        );
    }
    assert_eq!(*net.node_addr_calls.lock().unwrap(), [(1, None), (1, None), (1, None)]);

    // count = 0 returns all entries with exact field order and formatting.
    let all = run_json(&server, "getnodeaddresses", "[0]").unwrap().write();
    assert_eq!(
        all,
        r#"[{"time":1700000001,"services":1,"address":"1.2.3.4","port":9999,"network":"ipv4"},{"time":1700000002,"services":4097,"address":"2001:db8::1","port":8333,"network":"ipv6"},{"time":1700000003,"services":1,"address":"fc00::1","port":9999,"network":"cjdns"},{"time":1700000004,"services":1,"address":"pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion","port":9999,"network":"onion"}]"#
    );

    // Each network name, including case-insensitive inputs and deprecated "tor" -> Onion.
    let cases = [
        ("ipv4", Network::Ipv4, 1),
        ("IPV4", Network::Ipv4, 1),
        ("ipv6", Network::Ipv6, 1),
        ("IPv6", Network::Ipv6, 1),
        ("onion", Network::Onion, 1),
        ("ONION", Network::Onion, 1),
        ("tor", Network::Onion, 1),
        ("TOR", Network::Onion, 1),
        ("i2p", Network::I2p, 0),
        ("I2P", Network::I2p, 0),
        ("cjdns", Network::Cjdns, 1),
        ("Cjdns", Network::Cjdns, 1),
    ];
    for (input, expected_net, expected_len) in cases {
        let res = run(&server, "getnodeaddresses", vec![0.into(), input.into()]).unwrap();
        let UniValue::Arr(items) = res else {
            panic!("expected array");
        };
        assert_eq!(items.len(), expected_len, "{input}");
        assert_eq!(net.node_addr_calls.lock().unwrap().last().unwrap(), &(0, Some(expected_net)));
    }

    // Named arguments.
    let named =
        run_json(&server, "getnodeaddresses", r#"{"network":"cjdns","count":0}"#).unwrap().write();
    assert_eq!(
        named,
        r#"[{"time":1700000003,"services":1,"address":"fc00::1","port":9999,"network":"cjdns"}]"#
    );
    let named_default_count =
        run_json(&server, "getnodeaddresses", r#"{"network":"onion"}"#).unwrap().write();
    assert_eq!(
        named_default_count,
        r#"[{"time":1700000004,"services":1,"address":"pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion","port":9999,"network":"onion"}]"#
    );
    assert_eq!(net.node_addr_calls.lock().unwrap().last().unwrap(), &(1, Some(Network::Onion)));
}

/// `getnodeaddresses` argument errors, negative/out-of-range counts, unrecognised networks, and
/// `EnsureConnman` ordering (Core: v24.0.2 src/rpc/net.cpp:971-981).
#[test]
fn getnodeaddresses_errors_and_p2p_disabled() {
    let net = Arc::new(FakeNet::with_info(rc3()));
    let server = server(Arc::clone(&net), false);
    let help = dash_rpc_spec::command("getnodeaddresses").unwrap().help;

    // Arity (> 2 args) -> help as -1.
    let e = run_json(&server, "getnodeaddresses", r#"[1,"ipv4",true]"#).unwrap_err();
    assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{e:?}");
    assert_eq!(e.code(), RPC_MISC_ERROR);

    // JSON type errors -> -3.
    for params in [r#"["1"]"#, r#"[true]"#, r#"[1,123]"#, r#"[1,false]"#] {
        let e = run_json(&server, "getnodeaddresses", params).unwrap_err();
        assert_eq!(e.code(), RPC_TYPE_ERROR, "{params}: {e:?}");
    }

    // Count out of i32 range or non-integer -> -1 "JSON integer out of range".
    for params in ["[2147483648]", "[-2147483649]", "[1.5]", "[1e2]"] {
        let e = run_json(&server, "getnodeaddresses", params).unwrap_err();
        assert_eq!(
            e.to_object().write(),
            r#"{"code":-1,"message":"JSON integer out of range"}"#,
            "{params}"
        );
    }

    // Negative count -> -8 "Address count out of range" (checked before network).
    for params in ["[-1]", r#"[-1,"BadNet"]"#] {
        let e = run_json(&server, "getnodeaddresses", params).unwrap_err();
        assert_eq!(
            e.to_object().write(),
            r#"{"code":-8,"message":"Address count out of range"}"#,
            "{params}"
        );
    }

    // Unrecognised network -> -8 "Network not recognized: <original string>".
    for bad in ["BadNet", "", "internal", "not_publicly_routable"] {
        let e = run(&server, "getnodeaddresses", vec![1.into(), bad.into()]).unwrap_err();
        assert_eq!(
            e.to_object().write(),
            format!(r#"{{"code":-8,"message":"Network not recognized: {bad}"}}"#),
            "{bad}"
        );
    }
    assert!(net.node_addr_calls.lock().unwrap().is_empty());

    // EnsureConnman runs before count and network checks.
    let no_net = server_on_chain(FakeNet::default(), true);
    for params in ["[]", "[-1]", "[2147483648]", r#"[1,"BadNet"]"#] {
        let e = run_json(&no_net, "getnodeaddresses", params).unwrap_err();
        assert_eq!(
            e.to_object().write(),
            r#"{"code":-31,"message":"Error: Peer-to-peer functionality missing or disabled"}"#,
            "{params}"
        );
    }
}

/// `sendmsgtopeer`: argument checks in Core's order (msg_type length before hex before
/// EnsureConnman before ForNode), empty msg, odd/non-hex msg, unknown peer, and exact payload
/// delivery (Core: v24.0.2 src/rpc/net.cpp:1056-1101).
#[test]
fn sendmsgtopeer_check_order_errors_and_delivery() {
    let net = Arc::new(FakeNet {
        send_peers: vec![0, 7],
        ..FakeNet::with_info(rc3())
    });
    let server = server(Arc::clone(&net), false);
    let help = dash_rpc_spec::command("sendmsgtopeer").unwrap().help;

    // Arity errors -> help as -1.
    for params in ["[]", r#"[0,"addr"]"#, r#"[0,"addr","ff",1]"#] {
        let e = run_json(&server, "sendmsgtopeer", params).unwrap_err();
        assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{params}: {e:?}");
        assert_eq!(e.code(), RPC_MISC_ERROR, "{params}");
    }

    // JSON type errors -> -3.
    for params in [r#"["0","addr","ff"]"#, r#"[0,1,"ff"]"#, r#"[0,"addr",1]"#] {
        let e = run_json(&server, "sendmsgtopeer", params).unwrap_err();
        assert_eq!(e.code(), RPC_TYPE_ERROR, "{params}: {e:?}");
    }

    // peer_id parsed with getInt<int>() -> i32 range check before msg_type length.
    for params in [r#"[2147483648,"long_msg_type","zz"]"#, r#"[0.5,"long_msg_type","zz"]"#] {
        let e = run_json(&server, "sendmsgtopeer", params).unwrap_err();
        assert_eq!(
            e.to_object().write(),
            r#"{"code":-1,"message":"JSON integer out of range"}"#,
            "{params}"
        );
    }

    // Check 1 (msg_type > 12 bytes) precedes check 2 (invalid hex) and check 3 (EnsureConnman).
    let no_net = server_on_chain(FakeNet::default(), true);
    for srv in [&server, &no_net] {
        let e = run_json(srv, "sendmsgtopeer", r#"[0,"long_msg_type","not-hex!"]"#).unwrap_err();
        assert_eq!(
            e.to_object().write(),
            r#"{"code":-8,"message":"Error: msg_type too long, max length is 12"}"#
        );
    }

    // Check 2 (non-empty invalid hex: odd length, non-hex digits, whitespace, 0x prefix) precedes
    // EnsureConnman.
    for bad_hex in ["f", "abc", "zz", "0x00", "ff "] {
        for srv in [&server, &no_net] {
            let e = run(srv, "sendmsgtopeer", vec![0.into(), "addr".into(), bad_hex.into()])
                .unwrap_err();
            assert_eq!(
                e.to_object().write(),
                r#"{"code":-8,"message":"Error parsing input for msg"}"#,
                "{bad_hex}"
            );
        }
    }

    // Check 3: EnsureConnman when both msg_type and msg are valid.
    let e = run_json(&no_net, "sendmsgtopeer", r#"[0,"addr",""]"#).unwrap_err();
    assert_eq!(
        e.to_object().write(),
        r#"{"code":-31,"message":"Error: Peer-to-peer functionality missing or disabled"}"#
    );

    // Check 4: unknown peer -> RPC_MISC_ERROR (-1).
    let e = run_json(&server, "sendmsgtopeer", r#"[100,"addr","ff"]"#).unwrap_err();
    assert_eq!(
        e.to_object().write(),
        r#"{"code":-1,"message":"Error: Could not send message to peer"}"#
    );

    // Success: exact bytes, empty msg allowed, empty msg_type allowed, 12-byte msg_type allowed,
    // and named arguments.
    assert_eq!(
        run_json(&server, "sendmsgtopeer", r#"[0,"addr","aAbB01"]"#).unwrap(),
        UniValue::Null
    );
    assert_eq!(run_json(&server, "sendmsgtopeer", r#"[0,"addr",""]"#).unwrap(), UniValue::Null);
    assert_eq!(run_json(&server, "sendmsgtopeer", r#"[7,"","ff"]"#).unwrap(), UniValue::Null);
    assert_eq!(
        run_json(&server, "sendmsgtopeer", r#"[7,"123456789012","00"]"#).unwrap(),
        UniValue::Null
    );
    assert_eq!(
        run_json(
            &server,
            "sendmsgtopeer",
            r#"{"msg":"deadbeef","msg_type":"unknown","peer_id":0}"#
        )
        .unwrap(),
        UniValue::Null
    );
    assert_eq!(
        *net.sent_msgs.lock().unwrap(),
        vec![
            (0, "addr".into(), vec![0xaa, 0xbb, 0x01]),
            (0, "addr".into(), vec![]),
            (7, "".into(), vec![0xff]),
            (7, "123456789012".into(), vec![0x00]),
            (0, "unknown".into(), vec![0xde, 0xad, 0xbe, 0xef]),
        ]
    );
}

/// `getaddrmaninfo`: missing addrman returns `-31`; exact network key order and inner
/// `{new, tried, total}` key order (Core: v24.0.2 src/rpc/net.cpp:1129-1176).
#[test]
fn getaddrmaninfo_disabled_order_and_counts() {
    let disabled = server(FakeNet::default(), false);
    let e = run_json(&disabled, "getaddrmaninfo", "[]").unwrap_err();
    assert_eq!(e.code(), RPC_CLIENT_P2P_DISABLED);
    assert_eq!(
        e.to_object().write(),
        r#"{"code":-31,"message":"Error: Address manager functionality missing or disabled"}"#
    );

    let net = Arc::new(FakeNet {
        addrman_info_val: Some(AddrmanInfo {
            ipv4: AddrmanCounts {
                new: 2,
                tried: 1,
                total: 3,
            },
            ipv6: AddrmanCounts {
                new: 1,
                tried: 0,
                total: 1,
            },
            onion: AddrmanCounts {
                new: 0,
                tried: 4,
                total: 4,
            },
            i2p: AddrmanCounts::default(),
            cjdns: AddrmanCounts {
                new: 1,
                tried: 1,
                total: 2,
            },
            all_networks: AddrmanCounts {
                new: 4,
                tried: 6,
                total: 10,
            },
        }),
        ..FakeNet::with_info(rc3())
    });
    let server = server(net, false);
    let reply = run_json(&server, "getaddrmaninfo", "[]").unwrap();
    assert_eq!(
        reply.write(),
        r#"{"ipv4":{"new":2,"tried":1,"total":3},"ipv6":{"new":1,"tried":0,"total":1},"onion":{"new":0,"tried":4,"total":4},"i2p":{"new":0,"tried":0,"total":0},"cjdns":{"new":1,"tried":1,"total":2},"all_networks":{"new":4,"tried":6,"total":10}}"#
    );

    // Extra argument -> help as -1.
    let help = dash_rpc_spec::command("getaddrmaninfo").unwrap().help;
    let e = run_json(&server, "getaddrmaninfo", "[1]").unwrap_err();
    assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{e:?}");
}

/// `getrawaddrman`: missing addrman returns `-31`; `{new, tried}` order, `"<bucket>/<position>"`
/// keys in `GetEntries` order, and every entry field in Core's order
/// (Core: v24.0.2 src/rpc/net.cpp:1178-1244).
#[test]
fn getrawaddrman_disabled_keys_and_entry_fields() {
    let disabled = server(FakeNet::default(), false);
    let e = run_json(&disabled, "getrawaddrman", "[]").unwrap_err();
    assert_eq!(e.code(), RPC_CLIENT_P2P_DISABLED);
    assert_eq!(
        e.to_object().write(),
        r#"{"code":-31,"message":"Error: Address manager functionality missing or disabled"}"#
    );

    let net = Arc::new(FakeNet {
        raw_addrman_val: Some(RawAddrman {
            new: vec![
                RawAddrmanEntry {
                    bucket: 5,
                    position: 12,
                    address: "2.0.0.0".into(),
                    port: 8333,
                    services: NODE_NETWORK,
                    time: 1_700_000_000,
                    network: "ipv4",
                    source: "2.0.0.0".into(),
                    source_network: "ipv4",
                },
                RawAddrmanEntry {
                    bucket: 1023,
                    position: 63,
                    address: "2803:0:1234:abcd::1".into(),
                    port: 9999,
                    services: NODE_NETWORK | NODE_P2P_V2,
                    time: 1_700_000_010,
                    network: "ipv6",
                    source: "1.2.3.4".into(),
                    source_network: "ipv4",
                },
            ],
            tried: vec![RawAddrmanEntry {
                bucket: 42,
                position: 7,
                address: "nrfj6inpyf73gpkyool35hcmne5zwfmse3jl3aw23vk7chdemalyaqad.onion".into(),
                port: 19999,
                services: NODE_NETWORK,
                time: 1_700_000_020,
                network: "onion",
                source: "nrfj6inpyf73gpkyool35hcmne5zwfmse3jl3aw23vk7chdemalyaqad.onion".into(),
                source_network: "onion",
            }],
        }),
        ..FakeNet::with_info(rc3())
    });
    let server = server(net, false);
    let reply = run_json(&server, "getrawaddrman", "[]").unwrap();
    assert_eq!(
        reply.write(),
        r#"{"new":{"5/12":{"address":"2.0.0.0","port":8333,"services":1,"time":1700000000,"network":"ipv4","source":"2.0.0.0","source_network":"ipv4"},"1023/63":{"address":"2803:0:1234:abcd::1","port":9999,"services":4097,"time":1700000010,"network":"ipv6","source":"1.2.3.4","source_network":"ipv4"}},"tried":{"42/7":{"address":"nrfj6inpyf73gpkyool35hcmne5zwfmse3jl3aw23vk7chdemalyaqad.onion","port":19999,"services":1,"time":1700000020,"network":"onion","source":"nrfj6inpyf73gpkyool35hcmne5zwfmse3jl3aw23vk7chdemalyaqad.onion","source_network":"onion"}}}"#
    );

    // Extra argument -> help as -1.
    let help = dash_rpc_spec::command("getrawaddrman").unwrap().help;
    let e = run_json(&server, "getrawaddrman", "[1]").unwrap_err();
    assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{e:?}");
}

/// The three BL-690a hidden commands are absent from plain `help` and answer `help <name>` with
/// Core's text (Core: v24.0.2 src/rpc/net.cpp:1265-1268).
#[test]
fn bl690a_hidden_commands_absent_from_help_overview_but_answer_detailed_help() {
    let server = server(FakeNet::with_info(rc3()), false);
    let overview = run(&server, "help", vec![]).unwrap();
    let overview_str = overview.get_str().unwrap();
    for name in ["sendmsgtopeer", "getaddrmaninfo", "getrawaddrman"] {
        assert!(!overview_str.contains(name), "{name} must be hidden from plain help");
        let detailed = run(&server, "help", vec![name.into()]).unwrap();
        assert_eq!(
            detailed.get_str().unwrap(),
            dash_rpc_spec::command(name).unwrap().help,
            "{name}"
        );
    }
}

/// `getnettotals`: missing connman returns `-31`; exact key order, default `uploadtarget`
/// values, real-clock `timemillis`, arity error, and listing in `help`.
// Core: v24.0.2 src/rpc/net.cpp:563-611, :1255; src/net.cpp:92, :4665-4740
#[test]
fn getnettotals_disabled_order_uploadtarget_and_help() {
    let disabled = server(FakeNet::default(), false);
    let e = run_json(&disabled, "getnettotals", "[]").unwrap_err();
    assert_eq!(e.code(), RPC_CLIENT_P2P_DISABLED);
    assert_eq!(
        e.to_object().write(),
        r#"{"code":-31,"message":"Error: Peer-to-peer functionality missing or disabled"}"#
    );

    let net = Arc::new(FakeNet {
        totals: Some(NetTotals {
            total_bytes_recv: 12_345,
            total_bytes_sent: 67_890,
            upload_target: UploadTarget::default(),
        }),
        ..FakeNet::with_info(rc3())
    });
    let server = server(net, false);

    let before_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    let reply = run_json(&server, "getnettotals", "[]").unwrap();
    let after_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;

    let UniValue::Obj(fields) = &reply else {
        panic!("expected object, got {reply:?}");
    };
    let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["totalbytesrecv", "totalbytessent", "timemillis", "uploadtarget"]);
    assert_eq!(fields[0].1.get_int64().unwrap(), 12_345);
    assert_eq!(fields[1].1.get_int64().unwrap(), 67_890);
    let timemillis = fields[2].1.get_int64().unwrap();
    assert!(
        (before_ms..=after_ms).contains(&timemillis),
        "timemillis {timemillis} outside [{before_ms}, {after_ms}]"
    );
    assert_eq!(
        fields[3].1.write(),
        r#"{"timeframe":86400,"target":0,"target_reached":false,"serve_historical_blocks":true,"bytes_left_in_cycle":0,"time_left_in_cycle":0}"#
    );

    // Extra argument -> help as -1.
    let help = dash_rpc_spec::command("getnettotals").unwrap().help;
    let e = run_json(&server, "getnettotals", "[1]").unwrap_err();
    assert!(matches!(&e, RpcError::Runtime(m) if m == help), "{e:?}");

    // Listed in plain `help` under Network, and `help getnettotals` matches Core.
    let overview = run(&server, "help", vec![]).unwrap();
    assert!(overview.get_str().unwrap().contains("getnettotals"));
    let detailed = run(&server, "help", vec!["getnettotals".into()]).unwrap();
    assert_eq!(detailed.get_str().unwrap(), help);
}
