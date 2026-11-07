use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use dash_consensus::BlsCtx;
use dash_pkc::bls::BlsSchemeKind;
use dash_pkc::bls::core::BlsPkValue;
use dash_primitives::BlockHash;

use super::register_mnauth_commands;
use crate::UniValue;
use crate::blockchain::{Chainman, NodeChain, PublishedChain};
use crate::descriptor::hex;
use crate::peers::{AddedNodeInfo, NetworkInfo, NodeNet, PeerInfo};
use crate::protocol::{
    RPC_CLIENT_P2P_DISABLED, RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RPC_TYPE_ERROR, RpcError,
};
use crate::request::{JsonRpcRequest, parse_hex};
use crate::server::RpcServer;
use crate::testutil::FakeControl;

/// A non-palindromic proTxHash, as the RPC takes it (`GetHex`, so the bytes reversed).
const PRO_TX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// v24.0.0 dashd's `bls fromsecret 0102…3132 false` (basic) and `… true` (legacy) public key.
const KEY_BASIC: &str = "a131afc4ccbaaf78ee0624129f4325f42288cd82dc538969c63f5507bdb80bb5607492353b33172fe1d96a9c90b791e5";
const KEY_LEGACY: &str = "8131afc4ccbaaf78ee0624129f4325f42288cd82dc538969c63f5507bdb80bb5607492353b33172fe1d96a9c90b791e5";

/// SHA-256d of [`KEY_BASIC`]'s and [`KEY_LEGACY`]'s bytes (Python `hashlib`), the raw digest.
const HASH_BASIC: &str = "554a80562a3a92f35c7b2035b38bcd0596c9e3fe0b57df6111e074144e22b572";
const HASH_LEGACY: &str = "e653113e7cf7dee2feb45075121597b8655797c98b71fc0c32aa2c140f4d9fb0";

const REGTEST_ONLY: &str = "mnauth for regression testing (-regtest mode) only";
const OUT_OF_RANGE: &str = "JSON integer out of range";

type Call = (i64, [u8; 32], [u8; 32]);

/// The connection manager: `answer` is `None` without one; each call is recorded.
struct FakeNet {
    answer: Option<bool>,
    calls: Mutex<Vec<Call>>,
}

impl FakeNet {
    fn new(answer: Option<bool>) -> Arc<FakeNet> {
        Arc::new(FakeNet {
            answer,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
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
    fn try_mark_verified(&self, id: i64, proreg: [u8; 32], pubkey: [u8; 32]) -> Option<bool> {
        self.calls.lock().unwrap().push((id, proreg, pubkey));
        self.answer
    }
    fn added_node_info(&self) -> Option<Vec<AddedNodeInfo>> {
        unreachable!()
    }
    fn fetch_block(&self, _: &BlockHash, _: i64) -> Option<Result<(), String>> {
        unreachable!()
    }
}

/// A loaded chain whose process flag is `.0`.
struct FakeChain(BlsCtx);

impl PublishedChain for FakeChain {
    fn height(&self) -> i32 {
        1
    }
    fn block_hash(&self, _: i32) -> BlockHash {
        unreachable!()
    }
    fn bls(&self) -> BlsCtx {
        self.0
    }
}

impl NodeChain for FakeChain {
    fn published(&self) -> Option<Box<dyn PublishedChain>> {
        Some(Box::new(FakeChain(self.0)))
    }
    fn chainman(&self) -> Option<Box<dyn Chainman>> {
        unreachable!()
    }
}

fn server_with(net: &Arc<FakeNet>, regtest: bool, bls: BlsCtx) -> Arc<RpcServer> {
    let control = FakeControl::default();
    control.main_chain.store(!regtest, Ordering::SeqCst);
    let server = Arc::new(RpcServer::new(Arc::new(control)));
    register_mnauth_commands(&server, Arc::clone(net) as _, Arc::new(FakeChain(bls)));
    server.start();
    server.set_warmup_finished();
    server
}

fn server(net: &Arc<FakeNet>, regtest: bool) -> Arc<RpcServer> {
    server_with(net, regtest, BlsCtx::BASIC)
}

fn call(server: &RpcServer, params: &str) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: "mnauth".into(),
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

fn bytes(hex: &str) -> [u8; 32] {
    parse_hex(hex).unwrap().try_into().unwrap()
}

fn reversed(hex: &str) -> [u8; 32] {
    let mut b = bytes(hex);
    b.reverse();
    b
}

fn args(id: &str, hash: &str, key: &str) -> String {
    format!(r#"[{id}, "{hash}", "{key}"]"#)
}

#[test]
fn the_wrapper_checks_arity_then_types_on_every_network() {
    for regtest in [false, true] {
        let net = FakeNet::new(Some(true));
        let s = server(&net, regtest);
        for params in ["[]", "[1]", r#"[1, "a"]"#, r#"[1, "a", "b", "c"]"#] {
            let (code, msg) = err(&s, params);
            assert_eq!(code, RPC_MISC_ERROR, "{params}");
            assert!(msg.contains(r#"mnauth nodeId \"proTxHash\" \"publicKey\""#), "{msg}");
        }
        // Every mismatch is listed together; a non-string key beats a fractional id and a
        // malformed hash, which the handler would refuse first.
        for (params, positions) in [
            (r#"["1", 2, 3]"#, &[1, 2, 3][..]),
            (r#"[1.5, "zz", 7]"#, &[3]),
            (r#"[1, 5, "k"]"#, &[2]),
            (r#"[1, "a", null]"#, &[3]),
        ] {
            let (code, msg) = err(&s, params);
            assert_eq!(code, RPC_TYPE_ERROR, "{params}");
            assert!(msg.contains("Wrong type passed:"), "{msg}");
            for (pos, name) in [(1, "nodeId"), (2, "proTxHash"), (3, "publicKey")] {
                let listed = msg.contains(&format!("Position {pos} ({name})"));
                assert_eq!(listed, positions.contains(&pos), "{params}: {msg}");
            }
        }
        assert!(net.calls().is_empty());
    }
}

#[test]
fn off_regtest_every_typed_call_is_refused_first() {
    let net = FakeNet::new(Some(true));
    let s = server(&net, false);
    for params in [
        args("1", PRO_TX, KEY_BASIC),
        args("1.5", "zz", "zz"),
        args("9223372036854775808", PRO_TX, KEY_BASIC),
        args("-0", PRO_TX, KEY_BASIC),
        args("1", &"0".repeat(64), "zz"),
    ] {
        assert_eq!(err(&s, &params), reply(RPC_MISC_ERROR, REGTEST_ONLY), "{params}");
    }
    assert!(net.calls().is_empty());
}

#[test]
fn the_node_id_is_core_getint_and_passes_through_unchanged() {
    let net = FakeNet::new(Some(false));
    let s = server(&net, true);
    for id in ["9223372036854775808", "-9223372036854775809", "1.5", "1.0", "0e0", "1e3"] {
        assert_eq!(err(&s, &args(id, "zz", "zz")), reply(RPC_MISC_ERROR, OUT_OF_RANGE), "{id}");
    }
    assert!(net.calls().is_empty());
    for (id, want) in [
        ("-0", 0),
        ("-1", -1),
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
    ] {
        assert_eq!(call(&s, &args(id, PRO_TX, KEY_BASIC)).unwrap(), false.into());
        assert_eq!(net.calls().last().unwrap().0, want, "{id}");
    }
}

#[test]
fn contents_are_checked_in_cores_order() {
    let net = FakeNet::new(Some(true));
    let s = server(&net, true);
    let zero = "0".repeat(64);
    let non_hex = format!("{}g", "0".repeat(63));
    let both = "z".repeat(63);
    for (params, want) in [
        (args("1.5", "zz", "zz"), reply(RPC_MISC_ERROR, OUT_OF_RANGE)),
        (
            args("1", "zz", "zz"),
            reply(RPC_INVALID_PARAMETER, "proTxHash must be of length 64 (not 2, for 'zz')"),
        ),
        (
            args("1", &non_hex, "zz"),
            reply(
                RPC_INVALID_PARAMETER,
                &format!("proTxHash must be hexadecimal string (not '{non_hex}')"),
            ),
        ),
        (args("1", &zero, "zz"), reply(RPC_INVALID_PARAMETER, "proTxHash invalid")),
        (args("1", PRO_TX, "zz"), reply(RPC_INVALID_PARAMETER, "publicKey invalid")),
        // An absent id is not looked up before the contents are read.
        (args("-1", &zero, KEY_BASIC), reply(RPC_INVALID_PARAMETER, "proTxHash invalid")),
        // Both wrong-length and non-hex: the length error.
        (
            args("1", &both, KEY_BASIC),
            reply(
                RPC_INVALID_PARAMETER,
                &format!("proTxHash must be of length 64 (not 63, for '{both}')"),
            ),
        ),
    ] {
        assert_eq!(err(&s, &params), want, "{params}");
    }
    assert!(net.calls().is_empty());
}

/// Each refused key was checked against v24.0.0 dashd's `mnauth -1 <hash> <key>`.
#[test]
fn keys_are_read_as_cores_basic_set_hex_str() {
    let net = FakeNet::new(Some(true));
    let s = server(&net, true);
    let infinity = format!("c0{}", "0".repeat(94));
    let not_in_subgroup = format!("80{}04", "0".repeat(92));
    // The key's legacy encoding with the sign bit clear: a valid legacy key, but no basic one.
    let legacy_only = format!("01{}", &KEY_BASIC[2..]);
    for key in [
        String::new(),
        "0".repeat(64),
        "0".repeat(96),
        infinity,
        not_in_subgroup,
        legacy_only,
        format!("{KEY_BASIC}ab"),
        KEY_BASIC[1..].to_owned(),
        format!("0x{}", &KEY_BASIC[2..]),
        KEY_BASIC.replace('a', "g"),
    ] {
        assert_eq!(
            err(&s, &args("1", PRO_TX, &key)),
            reply(RPC_INVALID_PARAMETER, "publicKey invalid"),
            "{key}"
        );
    }
    assert!(net.calls().is_empty());
    assert_eq!(call(&s, &args("1", PRO_TX, &KEY_BASIC.to_uppercase())).unwrap(), true.into());
    assert_eq!(net.calls(), [(1, reversed(PRO_TX), bytes(HASH_BASIC))]);
}

#[test]
fn the_key_hash_follows_the_process_flag_not_the_parse_scheme() {
    for (bls, hash) in [(BlsCtx::BASIC, HASH_BASIC), (BlsCtx::LEGACY, HASH_LEGACY)] {
        let net = FakeNet::new(Some(true));
        let s = server_with(&net, true, bls);
        assert_eq!(call(&s, &args("7", PRO_TX, KEY_BASIC)).unwrap(), true.into());
        // The proTxHash in `uint256`'s internal order; the key hash the raw digest.
        assert_eq!(net.calls(), [(7, reversed(PRO_TX), bytes(hash))], "{bls:?}");
    }
    // The legacy hash is of dashd's legacy encoding of the same key.
    let key = BlsPkValue::from_set_bytes_basic(&parse_hex(KEY_BASIC).unwrap());
    assert_eq!(hex(&key.to_bytes(BlsSchemeKind::Legacy)), KEY_LEGACY);
}

#[test]
fn the_answer_passes_through_and_no_connection_manager_is_p2p_disabled() {
    let disabled =
        reply(RPC_CLIENT_P2P_DISABLED, "Error: Peer-to-peer functionality missing or disabled");
    for (answer, want) in
        [(Some(true), Ok(true.into())), (Some(false), Ok(false.into())), (None, Err(disabled))]
    {
        let net = FakeNet::new(answer);
        let s = server(&net, true);
        let got = call(&s, &args("3", PRO_TX, KEY_BASIC)).map_err(wire);
        assert_eq!(got, want, "{answer:?}");
        assert_eq!(net.calls().len(), 1);
        // Validation still comes first without a connection manager.
        assert_eq!(
            err(&s, &args("3", PRO_TX, "zz")),
            reply(RPC_INVALID_PARAMETER, "publicKey invalid")
        );
        assert_eq!(net.calls().len(), 1);
    }
}
