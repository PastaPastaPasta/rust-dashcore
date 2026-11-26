use std::sync::{Arc, Mutex};

use dash_dml::{Dmn, ProTxHash};
use dash_netaddr::{NetAddr, Service};
use dash_primitives::{OutPoint, TxHash};

use super::{ActiveMnStatus, NodeMasternode, register_masternode_commands};
use crate::UniValue;
use crate::core_write::dmn::state_to_json;
use crate::core_write::dmn::tests::{evo_dmn, params};
use crate::evo::EvoError;
use crate::protocol::{RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RpcError};
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

fn server(node: Arc<FakeNode>) -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    crate::commands::register_control_commands(&server);
    register_masternode_commands(&server, node, params(false));
    server.start();
    server.set_warmup_finished();
    server
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
    for sub in ["count", "connect", "outputs", "winners", "payments", "list", "current", "x"] {
        for params in [format!(r#"["{sub}"]"#), format!(r#"{{"command":"{sub}"}}"#)] {
            let err = call(&server, "masternode", &params).unwrap_err();
            assert_eq!(err.code(), RPC_INVALID_PARAMETER, "{params}");
            assert_eq!(message(&err), "Must be a valid command", "{params}");
        }
    }
}
