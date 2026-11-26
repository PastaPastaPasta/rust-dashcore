use std::sync::{Arc, Mutex};

use dash_dml::{Dmn, ProTxHash};
use dash_netaddr::{NetAddr, Network, Service};
use dash_primitives::{BlockHash, OutPoint, TxHash};

use super::{ActiveMnStatus, NodeMasternode, register_masternode_commands};
use crate::UniValue;
use crate::core_write::dmn::state_to_json;
use crate::core_write::dmn::tests::{evo_dmn, params};
use crate::evo::EvoError;
use crate::peers::{
    AddConnectionType, AddedNodeInfo, AddrmanInfo, NetworkInfo, NodeAddress, NodeNet, PeerInfo,
    RawAddrman,
};
use crate::protocol::{
    RPC_CLIENT_P2P_DISABLED, RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
    RPC_TYPE_ERROR, RpcError,
};
use crate::request::JsonRpcRequest;
use crate::server::RpcServer;
use crate::testutil::FakeControl;

/// The node's active masternode, if any, over a tip list holding `listed`.
#[derive(Default)]
struct FakeNode {
    active: Option<ActiveMnStatus>,
    listed: Option<Arc<Dmn>>,
    list_error: Option<EvoError>,
    lookups: Mutex<Vec<ProTxHash>>,
}

impl NodeMasternode for FakeNode {
    fn active_mn(&self) -> Option<ActiveMnStatus> {
        self.active.clone()
    }
    fn tip_mn(&self, pro_tx_hash: &ProTxHash) -> Result<Option<Arc<Dmn>>, EvoError> {
        self.lookups.lock().unwrap().push(*pro_tx_hash);
        if let Some(error) = &self.list_error {
            return Err(error.clone());
        }
        Ok(self.listed.clone().filter(|dmn| dmn.pro_tx_hash == *pro_tx_hash))
    }
}

/// Fake `NodeNet` for `masternode connect`.
#[derive(Default)]
struct FakeNet {
    v2: Option<bool>,
    connect_ok: bool,
    connects: Mutex<Vec<(Service, bool)>>,
}

impl FakeNet {
    fn new(v2: bool, connect_ok: bool) -> Arc<Self> {
        Arc::new(Self {
            v2: Some(v2),
            connect_ok,
            connects: Mutex::default(),
        })
    }
}

impl NodeNet for FakeNet {
    fn peer_info(&self) -> Option<Vec<PeerInfo>> {
        unreachable!()
    }
    fn network_info(&self) -> Option<NetworkInfo> {
        unreachable!()
    }
    fn set_network_active(&self, _: bool) -> Option<bool> {
        unreachable!()
    }
    fn v2_transport(&self) -> Option<bool> {
        self.v2
    }
    fn open_one_try(&self, _: &str, _: bool) {
        unreachable!()
    }
    fn add_node(&self, _: &str, _: bool) -> bool {
        unreachable!()
    }
    fn remove_added_node(&self, _: &str) -> bool {
        unreachable!()
    }
    fn disconnect_by_address(&self, _: &str) -> bool {
        unreachable!()
    }
    fn disconnect_by_id(&self, _: i64) -> bool {
        unreachable!()
    }
    fn try_mark_verified(&self, _: i64, _: [u8; 32], _: [u8; 32]) -> Option<bool> {
        unreachable!()
    }
    fn set_masternode_thread_active(&self, _: bool) -> Option<bool> {
        unreachable!()
    }
    fn added_node_info(&self) -> Option<Vec<AddedNodeInfo>> {
        unreachable!()
    }
    fn fetch_block(&self, _: &BlockHash, _: i64) -> Option<Result<(), String>> {
        unreachable!()
    }
    fn send_pings(&self) -> Option<()> {
        unreachable!()
    }
    fn add_connection(&self, _: &str, _: AddConnectionType, _: bool) -> Option<bool> {
        unreachable!()
    }
    fn addrman_cjdns_reachable(&self) -> Option<bool> {
        unreachable!()
    }
    fn add_peer_address(&self, _: &Service, _: u64, _: i64, _: &NetAddr) -> Option<bool> {
        unreachable!()
    }
    fn good_peer_address(&self, _: &Service) {
        unreachable!()
    }
    fn node_addresses(&self, _: usize, _: Option<Network>) -> Option<Vec<NodeAddress>> {
        unreachable!()
    }
    fn send_msg_to_peer(&self, _: i64, _: &str, _: Vec<u8>) -> Option<bool> {
        unreachable!()
    }
    fn addrman_info(&self) -> Option<AddrmanInfo> {
        unreachable!()
    }
    fn raw_addrman(&self) -> Option<RawAddrman> {
        unreachable!()
    }
    fn open_masternode_connection(&self, service: &Service, use_v2: bool) -> Option<bool> {
        self.v2?;
        self.connects.lock().unwrap().push((service.clone(), use_v2));
        Some(self.connect_ok)
    }
}

fn server_with(node: Arc<FakeNode>, net: Arc<FakeNet>, permissive_bool: bool) -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    crate::commands::register_control_commands(&server);
    register_masternode_commands(&server, node, net, params(false), permissive_bool);
    server.start();
    server.set_warmup_finished();
    server
}

fn server(node: Arc<FakeNode>) -> Arc<RpcServer> {
    server_with(node, Arc::default(), false)
}

fn call(server: &RpcServer, method: &str, params: &str) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: method.into(),
        params: UniValue::read(params.as_bytes()).unwrap(),
        ..Default::default()
    })
}

fn message(err: &RpcError) -> String {
    match err {
        RpcError::Json {
            message,
            ..
        }
        | RpcError::Type(message)
        | RpcError::Runtime(message) => message.clone(),
    }
}

fn keys(v: &UniValue) -> Vec<&str> {
    match v {
        UniValue::Obj(fields) => fields.iter().map(|(k, _)| k.as_str()).collect(),
        other => panic!("{}", other.write()),
    }
}

fn ready(dmn: &Dmn) -> ActiveMnStatus {
    ActiveMnStatus {
        outpoint: dmn.collateral,
        service: Service {
            addr: NetAddr::ipv4([1, 2, 3, 4]),
            port: 19999,
        },
        pro_tx_hash: Some(dmn.pro_tx_hash),
        state: "READY",
        status: "Ready".into(),
    }
}

fn waiting() -> ActiveMnStatus {
    ActiveMnStatus {
        outpoint: OutPoint::new(TxHash::from([0; 32]), u32::MAX),
        service: Service::default(),
        pro_tx_hash: None,
        state: "WAITING_FOR_PROTX",
        status: "Waiting for ProTx to appear on-chain".into(),
    }
}

/// Without `active_ctx`, every call form throws Core's internal error.
// Core: v24.0.2 src/rpc/masternode.cpp:202-204
#[test]
fn status_without_an_active_masternode_is_an_internal_error() {
    let server = server(Arc::default());
    for params in [r#"["status"]"#, r#"{"command":"status"}"#] {
        let err = call(&server, "masternode", params).unwrap_err();
        assert_eq!(err.code(), RPC_INTERNAL_ERROR, "{params}");
        assert_eq!(message(&err), "This node does not run an active masternode.", "{params}");
    }
}

/// A masternode in the tip list prints Core's ten keys in order, its `dmnState` as
/// `CDeterministicMNState::ToJson`, positionally and named alike.
// Core: v24.0.2 src/rpc/masternode.cpp:206-222
#[test]
fn status_of_a_listed_masternode_prints_every_field_in_cores_order() {
    let dmn = Arc::new(evo_dmn(7, 9));
    let node = Arc::new(FakeNode {
        active: Some(ready(&dmn)),
        listed: Some(Arc::clone(&dmn)),
        ..Default::default()
    });
    let server = server(Arc::clone(&node));
    let reply = call(&server, "masternode", r#"["status"]"#).unwrap();
    assert_eq!(call(&server, "masternode", r#"{"command":"status"}"#).unwrap(), reply);
    assert_eq!(
        keys(&reply),
        [
            "outpoint",
            "service",
            "proTxHash",
            "type",
            "collateralHash",
            "collateralIndex",
            "dmnState",
            "state",
            "status",
            "quorumParticipation"
        ]
    );
    let state = state_to_json(dmn.mn_type.0, &dmn.hot, &dmn.cold, &params(false)).unwrap();
    let collateral = dmn.collateral.txid().to_string();
    let expected = format!(
        r#"{{"outpoint":"{collateral}-7","service":"1.2.3.4:19999","proTxHash":"{}","type":"Evo","collateralHash":"{collateral}","collateralIndex":7,"dmnState":{},"state":"READY","status":"Ready","quorumParticipation":true}}"#,
        dmn.pro_tx_hash,
        state.write()
    );
    assert_eq!(reply.write(), expected);
    assert_eq!(*node.lookups.lock().unwrap(), [dmn.pro_tx_hash, dmn.pro_tx_hash]);
}

/// A proTxHash the tip list lacks, and the null one, print no masternode fields; the null one
/// is not looked up, as Core's `GetMN(uint256())` finds nothing.
// Core: v24.0.2 src/rpc/masternode.cpp:208-215
#[test]
fn status_of_an_unlisted_masternode_omits_the_list_fields() {
    let removed = evo_dmn(7, 9);
    let node = Arc::new(FakeNode {
        active: Some(ActiveMnStatus {
            state: "REMOVED",
            status: "Masternode removed from list".into(),
            ..ready(&removed)
        }),
        listed: Some(Arc::new(evo_dmn(8, 9))),
        ..Default::default()
    });
    let reply = call(&server(Arc::clone(&node)), "masternode", r#"["status"]"#).unwrap();
    assert_eq!(keys(&reply), ["outpoint", "service", "state", "status", "quorumParticipation"]);
    assert_eq!(*node.lookups.lock().unwrap(), [removed.pro_tx_hash]);

    let node = Arc::new(FakeNode {
        active: Some(waiting()),
        ..Default::default()
    });
    let reply = call(&server(Arc::clone(&node)), "masternode", r#"["status"]"#).unwrap();
    assert_eq!(
        reply.write(),
        format!(
            r#"{{"outpoint":"{}-4294967295","service":"[::]:0","state":"WAITING_FOR_PROTX","status":"Waiting for ProTx to appear on-chain","quorumParticipation":true}}"#,
            "0".repeat(64)
        )
    );
    assert!(node.lookups.lock().unwrap().is_empty());
}

/// Each `GetStateString()` and `GetStatus()` the node reports is printed as given.
#[test]
fn status_prints_each_state_and_status() {
    for (state, status) in [
        ("WAITING_FOR_PROTX", "Waiting for ProTx to appear on-chain"),
        ("POSE_BANNED", "Masternode was PoSe banned"),
        ("REMOVED", "Masternode removed from list"),
        ("OPERATOR_KEY_CHANGED", "Operator key changed or revoked"),
        ("PROTX_IP_CHANGED", "IP address specified in ProTx changed"),
        ("READY", "Ready"),
        ("ERROR", "Error. some error"),
    ] {
        let node = Arc::new(FakeNode {
            active: Some(ActiveMnStatus {
                state,
                status: status.into(),
                ..waiting()
            }),
            ..Default::default()
        });
        let reply = call(&server(node), "masternode", r#"["status"]"#).unwrap().write();
        let tail = format!(r#""state":"{state}","status":"{status}","quorumParticipation":true}}"#);
        assert!(reply.ends_with(&tail), "{reply}");
    }
}

/// A failed tip-list read answers as `protx info`'s does.
#[test]
fn status_reports_a_failed_list_read() {
    let dmn = evo_dmn(7, 9);
    let node = Arc::new(FakeNode {
        active: Some(ready(&dmn)),
        list_error: Some(EvoError::Unavailable),
        ..Default::default()
    });
    let err = call(&server(node), "masternode", r#"["status"]"#).unwrap_err();
    assert_eq!(err.code(), RPC_INTERNAL_ERROR);
    assert_eq!(message(&err), "Node evo context unavailable");
}

/// `help` serves Core's texts; the arity checks throw them: no subcommand throws `masternode`'s,
/// an argument to `status` its own.
// Core: v24.0.2 src/rpc/util.cpp:525-551 RPCHelpMan::HandleRequest
#[test]
fn masternode_help_and_arity_are_cores() {
    let server = server(Arc::default());
    let family = dash_rpc_spec::command("masternode").unwrap().help;
    let status = dash_rpc_spec::command("masternode status").unwrap().help;
    assert!(status.starts_with("masternode status\n\nPrint masternode status information\n"));
    let help = |params| call(&server, "help", params).unwrap();
    assert_eq!(help(r#"["masternode"]"#), UniValue::from(family));
    assert_eq!(help(r#"["masternode","status"]"#), UniValue::from(status));
    for (params, text) in [("[]", family), (r#"["status","x"]"#, status)] {
        let err = call(&server, "masternode", params).unwrap_err();
        assert_eq!(err.code(), RPC_MISC_ERROR, "{params}");
        assert_eq!(message(&err), text, "{params}");
    }
}

/// A subcommand the node does not serve reaches `masternode` itself, which throws Core's
/// `masternode_help` error, as `protx` and `quorum` answer one they do not serve.
// Core: v24.0.2 src/rpc/masternode.cpp:538-540; src/rpc/server.cpp:503-517
#[test]
fn unserved_subcommands_answer_must_be_a_valid_command() {
    let server = server(Arc::default());
    // BL-690a: `connect` is now served.
    for sub in ["count", "outputs", "winners", "payments", "list", "current", "x"] {
        for params in [format!(r#"["{sub}"]"#), format!(r#"{{"command":"{sub}"}}"#)] {
            let err = call(&server, "masternode", &params).unwrap_err();
            assert_eq!(err.code(), RPC_INVALID_PARAMETER, "{params}");
            assert_eq!(message(&err), "Must be a valid command", "{params}");
        }
    }
}

/// `masternode connect`: help, arity, numeric-only lookup (port 0 when omitted, no DNS),
/// `EnsureConnman`, `v2transport` default & refusal, `ParseBoolV` (strict and permissive),
/// connect failure and success (Core: v24.0.2 src/rpc/masternode.cpp:47-87).
#[test]
fn connect_lookup_v2_parse_bool_and_outcome() {
    let connect_help = dash_rpc_spec::command("masternode connect").unwrap().help;
    let net_v1 = FakeNet::new(false, true);
    let srv_v1 = server_with(Arc::default(), Arc::clone(&net_v1), false);

    // Help and arity checks.
    assert_eq!(
        call(&srv_v1, "help", r#"["masternode","connect"]"#).unwrap(),
        UniValue::from(connect_help)
    );
    for params in [r#"["connect"]"#, r#"["connect","1.2.3.4:9999",false,"extra"]"#] {
        let err = call(&srv_v1, "masternode", params).unwrap_err();
        assert_eq!(err.code(), RPC_MISC_ERROR, "{params}");
        assert_eq!(message(&err), connect_help, "{params}");
    }
    // Non-string address -> JSON type error (-3).
    let err = call(&srv_v1, "masternode", r#"["connect",123]"#).unwrap_err();
    assert_eq!(err.code(), RPC_TYPE_ERROR);

    // Bad addresses and hostnames (never DNS), checked BEFORE EnsureConnman.
    let srv_no_net = server_with(Arc::default(), Arc::default(), false);
    for bad in ["", "not-an-ip", "localhost:9999", "example.com:9999", "1.2.3.4:99999"] {
        for srv in [&srv_v1, &srv_no_net] {
            let err = call(srv, "masternode", &format!(r#"["connect","{bad}"]"#)).unwrap_err();
            assert_eq!(err.code(), RPC_INTERNAL_ERROR, "{bad}");
            assert_eq!(message(&err), format!("Incorrect masternode address {bad}"), "{bad}");
        }
    }
    assert!(net_v1.connects.lock().unwrap().is_empty());

    // Valid address without connman -> RPC_CLIENT_P2P_DISABLED (-31).
    let err = call(&srv_no_net, "masternode", r#"["connect","1.2.3.4:9999"]"#).unwrap_err();
    assert_eq!(err.code(), RPC_CLIENT_P2P_DISABLED);
    assert_eq!(message(&err), "Error: Peer-to-peer functionality missing or disabled");

    // Missing port gives port 0; default v2transport follows local services (false here).
    assert_eq!(
        call(&srv_v1, "masternode", r#"["connect","127.0.0.1"]"#).unwrap(),
        UniValue::from("successfully connected")
    );
    assert_eq!(
        call(&srv_v1, "masternode", r#"["connect","::1",null]"#).unwrap(),
        UniValue::from("successfully connected")
    );
    assert_eq!(
        *net_v1.connects.lock().unwrap(),
        vec![
            (Service::new(NetAddr::ipv4([127, 0, 0, 1]), 0), false),
            (
                Service::new(
                    NetAddr::from_legacy_ipv6([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]),
                    0
                ),
                false
            ),
        ]
    );

    // v2transport=true refused when node lacks NODE_P2P_V2.
    let err = call(&srv_v1, "masternode", r#"["connect","1.2.3.4:9999",true]"#).unwrap_err();
    assert_eq!(err.code(), RPC_INVALID_PARAMETER);
    assert_eq!(
        message(&err),
        "Error: Adding v2transport connections requires -v2transport init flag to be set."
    );

    // ParseBoolV strict mode (without -deprecatedrpc=permissive_bool): rejects strings & numbers.
    for bad in [r#""true""#, r#""false""#, r#""yes""#, r#""1""#, "1", "0"] {
        let err = call(&srv_v1, "masternode", &format!(r#"["connect","1.2.3.4:9999",{bad}]"#))
            .unwrap_err();
        assert_eq!(err.code(), RPC_INVALID_PARAMETER, "{bad}");
        assert_eq!(
            message(&err),
            "v2transport must be a JSON boolean. Pass -deprecatedrpc=permissive_bool to allow legacy boolean parsing.",
            "{bad}"
        );
    }

    // Default v2transport=true when node has NODE_P2P_V2; explicit false still allowed.
    let net_v2 = FakeNet::new(true, true);
    let srv_v2 = server_with(Arc::default(), Arc::clone(&net_v2), true);
    assert_eq!(
        call(&srv_v2, "masternode", r#"["connect","1.2.3.4:9999"]"#).unwrap(),
        UniValue::from("successfully connected")
    );
    assert_eq!(
        call(&srv_v2, "masternode", r#"["connect","1.2.3.4:9999",false]"#).unwrap(),
        UniValue::from("successfully connected")
    );
    // ParseBoolV permissive mode (-deprecatedrpc=permissive_bool).
    for (raw, want) in [
        (r#""true""#, true),
        (r#""YES""#, true),
        (r#""1""#, true),
        ("1", true),
        (r#""false""#, false),
        (r#""No""#, false),
        (r#""0""#, false),
        ("0", false),
    ] {
        assert_eq!(
            call(&srv_v2, "masternode", &format!(r#"["connect","1.2.3.4:9999",{raw}]"#)).unwrap(),
            UniValue::from("successfully connected"),
            "{raw}"
        );
        assert_eq!(net_v2.connects.lock().unwrap().last().unwrap().1, want, "{raw}");
    }
    let err = call(&srv_v2, "masternode", r#"["connect","1.2.3.4:9999","maybe"]"#).unwrap_err();
    assert_eq!(err.code(), RPC_INVALID_PARAMETER);
    assert_eq!(message(&err), "v2transport must be true, false, yes, no, 1 or 0 (not 'maybe')");

    // Connect failure -> RPC_INTERNAL_ERROR (-32603).
    let net_fail = FakeNet::new(false, false);
    let srv_fail = server_with(Arc::default(), net_fail, false);
    let err = call(&srv_fail, "masternode", r#"["connect","1.2.3.4:9999"]"#).unwrap_err();
    assert_eq!(err.code(), RPC_INTERNAL_ERROR);
    assert_eq!(message(&err), "Couldn't connect to masternode 1.2.3.4:9999");
}
