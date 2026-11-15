use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use dash_primitives::BlockHash;

use super::register_mnthread_commands;
use crate::UniValue;
use crate::peers::{AddedNodeInfo, NetworkInfo, NodeNet, PeerInfo};
use crate::protocol::{RPC_CLIENT_P2P_DISABLED, RPC_MISC_ERROR, RPC_TYPE_ERROR, RpcError};
use crate::request::JsonRpcRequest;
use crate::server::RpcServer;
use crate::testutil::FakeControl;

const REGTEST_ONLY: &str = "setmnthreadactive is for regression testing (-regtest mode) only.";
const P2P_DISABLED: &str = "Error: Peer-to-peer functionality missing or disabled";

/// The connection manager: `None` without one, else whether its read-back inverts the flag
/// (a stand-in whose answer differs from the argument). Each set is recorded.
struct FakeNet {
    invert: Option<bool>,
    sets: Mutex<Vec<bool>>,
}

impl FakeNet {
    fn new(invert: Option<bool>) -> Arc<FakeNet> {
        Arc::new(FakeNet {
            invert,
            sets: Mutex::new(Vec::new()),
        })
    }

    fn sets(&self) -> Vec<bool> {
        self.sets.lock().unwrap().clone()
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
        unreachable!()
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
    fn set_masternode_thread_active(&self, active: bool) -> Option<bool> {
        let invert = self.invert?;
        self.sets.lock().unwrap().push(active);
        Some(active ^ invert)
    }
    fn added_node_info(&self) -> Option<Vec<AddedNodeInfo>> {
        unreachable!()
    }
    fn fetch_block(&self, _: &BlockHash, _: i64) -> Option<Result<(), String>> {
        unreachable!()
    }
}

fn server(net: &Arc<FakeNet>, regtest: bool) -> Arc<RpcServer> {
    let control = FakeControl::default();
    control.main_chain.store(!regtest, Ordering::SeqCst);
    let server = Arc::new(RpcServer::new(Arc::new(control)));
    register_mnthread_commands(&server, Arc::clone(net) as _);
    server.start();
    server.set_warmup_finished();
    server
}

fn call(server: &RpcServer, params: &str) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: "setmnthreadactive".into(),
        params: UniValue::read(params.as_bytes()).unwrap(),
        ..Default::default()
    })
}

/// The error's code and the object the reply writes.
fn wire(e: RpcError) -> (i32, String) {
    (e.code(), e.to_object().write())
}

fn err(server: &RpcServer, params: &str) -> (i32, String) {
    wire(call(server, params).unwrap_err())
}

fn reply(code: i32, message: &str) -> (i32, String) {
    (code, format!(r#"{{"code":{code},"message":"{message}"}}"#))
}

#[test]
fn the_wrapper_checks_arity_then_types_on_every_network() {
    for regtest in [false, true] {
        let net = FakeNet::new(Some(false));
        let s = server(&net, regtest);
        // `["true", false]` is both: arity is checked first.
        for params in ["[]", "[true, false]", r#"["true", false]"#] {
            let (code, msg) = err(&s, params);
            assert_eq!(code, RPC_MISC_ERROR, "{params}");
            assert!(msg.contains(r#""message":"setmnthreadactive state\n"#), "{msg}");
        }
        for params in [r#"["true"]"#, "[1]", "[null]"] {
            let (code, msg) = err(&s, params);
            assert_eq!(code, RPC_TYPE_ERROR, "{params}");
            assert!(msg.contains("Wrong type passed:"), "{msg}");
            assert!(msg.contains("Position 1 (state)"), "{msg}");
        }
        assert!(net.sets().is_empty());
    }
}

#[test]
fn off_regtest_a_bool_is_refused_before_the_connection_manager() {
    for invert in [Some(false), None] {
        let net = FakeNet::new(invert);
        let s = server(&net, false);
        for params in ["[true]", "[false]"] {
            assert_eq!(err(&s, params), reply(RPC_MISC_ERROR, REGTEST_ONLY), "{params}");
        }
        assert!(net.sets().is_empty());
    }
}

#[test]
fn without_a_connection_manager_regtest_is_p2p_disabled() {
    let s = server(&FakeNet::new(None), true);
    for params in ["[true]", "[false]"] {
        assert_eq!(err(&s, params), reply(RPC_CLIENT_P2P_DISABLED, P2P_DISABLED), "{params}");
    }
}

#[test]
fn the_flag_reaches_the_network_and_the_reply_is_its_read_back() {
    for invert in [false, true] {
        let net = FakeNet::new(Some(invert));
        let s = server(&net, true);
        for active in [false, true, false] {
            let got = call(&s, &format!("[{active}]")).map_err(wire);
            assert_eq!(got, Ok(UniValue::from(active ^ invert)), "{active} {invert}");
        }
        assert_eq!(net.sets(), [false, true, false]);
    }
}
