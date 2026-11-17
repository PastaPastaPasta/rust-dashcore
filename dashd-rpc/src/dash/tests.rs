use std::sync::{Arc, Mutex};

use super::{MnSync, NodeSync, register_dash_commands};
use crate::UniValue;
use crate::protocol::{RPC_MISC_ERROR, RPC_TYPE_ERROR, RpcError};
use crate::request::JsonRpcRequest;
use crate::server::RpcServer;
use crate::testutil::FakeControl;

/// A sync at BLOCKCHAIN that records the calls `next` and `reset` make.
#[derive(Default)]
struct FakeSync {
    asset: Mutex<i32>,
    calls: Mutex<Vec<String>>,
}

impl MnSync for FakeSync {
    fn asset_id(&self) -> i32 {
        *self.asset.lock().unwrap()
    }
    fn asset_name(&self) -> &'static str {
        match self.asset_id() {
            1 => "MASTERNODE_SYNC_BLOCKCHAIN",
            4 => "MASTERNODE_SYNC_GOVERNANCE",
            _ => "MASTERNODE_SYNC_FINISHED",
        }
    }
    fn asset_start_time(&self) -> i64 {
        1_700_000_036
    }
    fn attempt(&self) -> i32 {
        2
    }
    fn is_blockchain_synced(&self) -> bool {
        self.asset_id() > 1
    }
    fn is_synced(&self) -> bool {
        self.asset_id() == 999
    }
    fn switch_to_next_asset(&self) {
        self.calls.lock().unwrap().push("next".into());
        let mut asset = self.asset.lock().unwrap();
        *asset = if *asset == 1 {
            4
        } else {
            999
        };
    }
    fn reset(&self, force: bool) {
        self.calls.lock().unwrap().push(format!("reset({force})"));
        *self.asset.lock().unwrap() = 1;
    }
}

/// The node's slot: empty until init constructs the sync.
#[derive(Default)]
struct Slot(Option<Arc<FakeSync>>);

impl NodeSync for Slot {
    fn mn_sync(&self) -> Option<Arc<dyn MnSync>> {
        self.0.clone().map(|s| s as _)
    }
}

fn server(slot: Arc<Slot>) -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    register_dash_commands(&server, slot);
    server.start();
    server.set_warmup_finished();
    server
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

fn mnsync(server: &RpcServer, params: &str) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: "mnsync".into(),
        params: UniValue::read(params.as_bytes()).unwrap(),
        ..Default::default()
    })
}

/// `status` has Core's six keys in its order; `next` and `reset` answer Core's strings after
/// `SwitchToNextAsset()` and `Reset(true)`; any other mode is `"failure"` and changes nothing.
// Core: rpc/node.cpp:95-149
#[test]
fn mnsync_modes_answer_cores_values() {
    let slot = Arc::new(Slot(Some(Arc::new(FakeSync {
        asset: Mutex::new(1),
        ..FakeSync::default()
    }))));
    let server = server(Arc::clone(&slot));
    let status = |s: &RpcServer| mnsync(s, r#"["status"]"#).unwrap().write();
    assert_eq!(
        status(&server),
        r#"{"AssetID":1,"AssetName":"MASTERNODE_SYNC_BLOCKCHAIN","AssetStartTime":1700000036,"Attempt":2,"IsBlockchainSynced":false,"IsSynced":false}"#
    );
    let text = |params| mnsync(&server, params).unwrap().write();
    assert_eq!(text(r#"["next"]"#), r#""sync updated to MASTERNODE_SYNC_GOVERNANCE""#);
    assert_eq!(text(r#"["next"]"#), r#""sync updated to MASTERNODE_SYNC_FINISHED""#);
    assert!(status(&server).contains(r#""IsBlockchainSynced":true,"IsSynced":true"#));
    assert_eq!(text(r#"["STATUS"]"#), r#""failure""#);
    assert_eq!(text(r#"[""]"#), r#""failure""#);
    assert_eq!(text(r#"["reset"]"#), r#""success""#);
    assert_eq!(*slot.0.as_ref().unwrap().calls.lock().unwrap(), ["next", "next", "reset(true)"]);
    assert!(status(&server).starts_with(r#"{"AssetID":1,"#));
}

/// The server's checks: no mode or two arguments throw the help text, and a mode that is not a
/// string is a type error, before the handler runs.
// Core: rpc/util.cpp:525-551 RPCHelpMan::HandleRequest
#[test]
fn mnsync_argument_errors_are_cores() {
    let slot = Arc::new(Slot(Some(Arc::default())));
    let server = server(Arc::clone(&slot));
    let help = dash_rpc_spec::command("mnsync").unwrap().help;
    for params in ["[]", r#"["status","x"]"#] {
        let err = mnsync(&server, params).unwrap_err();
        assert_eq!(err.code(), RPC_MISC_ERROR, "{params}");
        assert_eq!(message(&err), help, "{params}");
    }
    let err = mnsync(&server, "[1]").unwrap_err();
    assert_eq!(err.code(), RPC_TYPE_ERROR);
    assert_eq!(
        message(&err),
        "Wrong type passed:\n{\n    \"Position 1 (mode)\": \"JSON value of type number is not of expected type string\"\n}"
    );
    assert!(slot.0.as_ref().unwrap().calls.lock().unwrap().is_empty());
}

/// Without the sync, `CHECK_NONFATAL(node.mn_sync)` fails whatever the mode.
#[test]
fn mnsync_without_the_sync_is_an_internal_bug() {
    let server = server(Arc::new(Slot(None)));
    for mode in ["status", "other"] {
        let err = mnsync(&server, &format!(r#"["{mode}"]"#)).unwrap_err();
        assert_eq!(err.code(), RPC_MISC_ERROR);
        let text = message(&err);
        assert!(text.starts_with("Internal bug detected: node.mn_sync\n"), "{text}");
        assert!(text.contains("(mnsync)\nrdashd v0.0.0\n"), "{text}");
    }
}
