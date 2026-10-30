use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::{
    AddedNodeInfo, ConnectionCounts, LocalAddress, NetworkInfo, NetworkReach, NodeNet, PeerInfo,
    amount, register_net_commands, service_names,
};
use crate::UniValue;
use crate::protocol::{RPC_CLIENT_P2P_DISABLED, RpcError};
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

#[test]
fn without_a_network_the_network_commands_are_cores_p2p_disabled() {
    let server = server(FakeNet::default(), false);
    for (method, params) in [("getnetworkinfo", vec![]), ("setnetworkactive", vec![true.into()])] {
        let e = run(&server, method, params).unwrap_err();
        assert_eq!(e.code(), RPC_CLIENT_P2P_DISABLED, "{method}");
    }
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
