use std::sync::{Arc, Mutex};

use dash_primitives::BlockHash;

use crate::blockchain::tests::{HASH_7, call_method, fetch_server, help_reply, method_err, reply};
use crate::peers::{AddedNodeInfo, NetworkInfo, NodeNet, PeerInfo};
use crate::protocol::{
    RPC_CLIENT_P2P_DISABLED, RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
    RPC_TYPE_ERROR, RpcError,
};

const METHOD: &str = "getblockfrompeer";
const P2P_DISABLED: &str = "Error: Peer-to-peer functionality missing or disabled";
const PRUNED_AHEAD: &str = "In prune mode, only blocks that the node has already synced previously can be fetched from a peer";

/// The peer manager: `v2` is `None` without one; `fetch_block` records each call and answers
/// `answer`.
struct FakeNet {
    v2: Option<bool>,
    answer: Mutex<Option<Result<(), String>>>,
    fetched: Mutex<Vec<(BlockHash, i64)>>,
}

impl FakeNet {
    fn new(v2: Option<bool>) -> Arc<FakeNet> {
        Arc::new(FakeNet {
            v2,
            answer: Mutex::new(Some(Ok(()))),
            fetched: Mutex::new(Vec::new()),
        })
    }

    fn fetched(&self) -> Vec<(BlockHash, i64)> {
        self.fetched.lock().unwrap().clone()
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
    fn added_node_info(&self) -> Option<Vec<AddedNodeInfo>> {
        unreachable!()
    }
    fn fetch_block(&self, hash: &BlockHash, peer_id: i64) -> Option<Result<(), String>> {
        self.fetched.lock().unwrap().push((*hash, peer_id));
        self.answer.lock().unwrap().clone()
    }
}

fn hash7() -> BlockHash {
    HASH_7.parse().unwrap()
}

fn params(hash: &str, peer: &str) -> String {
    format!(r#"["{hash}", {peer}]"#)
}

fn misc(message: &str) -> (i32, String) {
    reply(RPC_MISC_ERROR, message)
}

#[test]
fn the_chainman_comes_first_then_the_peer_manager() {
    let (unloaded, _) = fetch_server(FakeNet::new(None), false);
    assert_eq!(
        method_err(&unloaded, METHOD, &params("1234", "0")),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );

    let net = FakeNet::new(None);
    let (server, live) = fetch_server(Arc::clone(&net) as _, true);
    let disabled = reply(RPC_CLIENT_P2P_DISABLED, P2P_DISABLED);
    assert_eq!(method_err(&server, METHOD, &params("1234", "0")), disabled);
    assert_eq!(method_err(&server, METHOD, &params(HASH_7, "0")), disabled);
    assert!(live.lock().unwrap().calls.is_empty(), "no lookup without a peer manager");
    assert!(net.fetched().is_empty());
}

#[test]
fn the_hash_is_parsed_before_the_peer_id_and_both_before_the_lookup() {
    let net = FakeNet::new(Some(false));
    let (server, live) = fetch_server(Arc::clone(&net) as _, true);
    assert_eq!(
        method_err(&server, METHOD, &params("1234", "1.5")),
        reply(RPC_INVALID_PARAMETER, "blockhash must be of length 64 (not 4, for '1234')")
    );
    let g = "g".repeat(64);
    assert_eq!(
        method_err(&server, METHOD, &params(&g, "0")),
        reply(RPC_INVALID_PARAMETER, &format!("blockhash must be hexadecimal string (not '{g}')"))
    );
    for peer in ["1.5", "1e3", "1.0", "9223372036854775808"] {
        assert_eq!(
            method_err(&server, METHOD, &params(HASH_7, peer)),
            misc("JSON integer out of range"),
            "{peer}"
        );
    }
    assert!(live.lock().unwrap().calls.is_empty(), "no lookup before the arguments parse");
    assert!(net.fetched().is_empty());
}

#[test]
fn the_peer_id_reaches_the_peer_manager_as_given() {
    let net = FakeNet::new(Some(true));
    let (server, live) = fetch_server(Arc::clone(&net) as _, true);
    live.lock().unwrap().indexed = true;
    for peer in ["-9223372036854775808", "-0", "4294967296", "-1", "0"] {
        let got = call_method(&server, METHOD, &params(HASH_7, peer)).unwrap();
        assert_eq!(got.write(), "{}", "{peer}");
    }
    let want: Vec<_> =
        [i64::MIN, 0, 4_294_967_296, -1, 0].into_iter().map(|p| (hash7(), p)).collect();
    assert_eq!(net.fetched(), want);
    let named = format!(r#"{{"blockhash":"{HASH_7}","peer_id":7}}"#);
    assert_eq!(call_method(&server, METHOD, &named).unwrap().write(), "{}");
    assert_eq!(net.fetched().last(), Some(&(hash7(), 7)));
}

#[test]
fn a_missing_header_or_a_block_with_data_never_reaches_the_peer_manager() {
    let net = FakeNet::new(Some(false));
    let (server, live) = fetch_server(Arc::clone(&net) as _, true);
    assert_eq!(method_err(&server, METHOD, &params(HASH_7, "0")), misc("Block header missing"));
    assert_eq!(live.lock().unwrap().calls, [format!("lookup {HASH_7}")]);
    {
        let mut live = live.lock().unwrap();
        live.indexed = true;
        live.have_data = true;
    }
    for peer in ["-1", "0"] {
        assert_eq!(
            method_err(&server, METHOD, &params(HASH_7, peer)),
            misc("Block already downloaded"),
            "{peer}"
        );
    }
    assert!(net.fetched().is_empty());
}

/// rb:518-522 before the data check: only a header above the active tip is refused, and only
/// in prune mode.
#[test]
fn prune_mode_refuses_only_headers_above_the_tip() {
    const TIP: i32 = 10;
    for prune in [true, false] {
        for (height, have_data) in
            [(11, false), (11, true), (10, false), (10, true), (9, false), (9, true)]
        {
            let net = FakeNet::new(Some(false));
            let (server, live) = fetch_server(Arc::clone(&net) as _, true);
            {
                let mut live = live.lock().unwrap();
                live.indexed = true;
                live.height = height;
                live.have_data = have_data;
                live.prune_tip = prune.then_some(TIP);
            }
            let got = call_method(&server, METHOD, &params(HASH_7, "0"));
            let case = format!("prune {prune}, height {height}, data {have_data}");
            let want = if prune && height > TIP as u32 {
                Err(misc(PRUNED_AHEAD))
            } else if have_data {
                Err(misc("Block already downloaded"))
            } else {
                Ok("{}".to_owned())
            };
            assert_eq!(
                got.map(|v| v.write()).map_err(|e| (e.code(), e.to_object().write())),
                want,
                "{case}"
            );
            let fetched = usize::from(want.is_ok());
            assert_eq!(net.fetched().len(), fetched, "{case}");
        }
    }
}

#[test]
fn the_peer_managers_answer_is_the_reply() {
    let net = FakeNet::new(Some(false));
    let (server, live) = fetch_server(Arc::clone(&net) as _, true);
    live.lock().unwrap().indexed = true;
    *net.answer.lock().unwrap() = None;
    assert_eq!(
        method_err(&server, METHOD, &params(HASH_7, "0")),
        reply(RPC_CLIENT_P2P_DISABLED, P2P_DISABLED)
    );
    for msg in [
        "Loading blocks ...",
        "Peer does not exist",
        "Already requested from this peer",
        "Peer not fully connected",
        "Block header missing",
    ] {
        *net.answer.lock().unwrap() = Some(Err(msg.into()));
        assert_eq!(method_err(&server, METHOD, &params(HASH_7, "0")), misc(msg));
    }
    *net.answer.lock().unwrap() = Some(Ok(()));
    let got = call_method(&server, METHOD, &params(HASH_7, "0")).unwrap();
    assert_eq!(got.write(), "{}");
    assert_eq!(net.fetched().len(), 7);
}

/// The server's arity and type checks come before the handler: with no chainman and no peer
/// manager, a wrong count is the help and a wrong type is -3.
#[test]
fn argument_errors_come_before_the_chainman_and_the_peer_manager() {
    let (server, _) = fetch_server(FakeNet::new(None), false);
    let help = dash_rpc_spec::command(METHOD).unwrap().help;
    let h = format!(r#""{HASH_7}""#);
    for p in [
        "[]".to_owned(),
        format!("[{h}]"),
        format!("[{h}, 0, 0]"),
        format!(r#"{{"blockhash":{h}}}"#),
    ] {
        assert_eq!(method_err(&server, METHOD, &p), (RPC_MISC_ERROR, help_reply(help)), "{p}");
    }
    let hash_type = |got: &str| {
        format!(
            r#""Position 1 (blockhash)": "JSON value of type {got} is not of expected type string""#
        )
    };
    let peer_type = |got: &str| {
        format!(
            r#""Position 2 (peer_id)": "JSON value of type {got} is not of expected type number""#
        )
    };
    for (p, fields) in [
        ("[1234, 0]".to_owned(), vec![hash_type("number")]),
        (format!(r#"[{h}, "0"]"#), vec![peer_type("string")]),
        (r#"[1234, "0"]"#.to_owned(), vec![hash_type("number"), peer_type("string")]),
        ("[null, 0]".to_owned(), vec![hash_type("null")]),
        (r#"{"peer_id":0}"#.to_owned(), vec![hash_type("null")]),
    ] {
        let body = fields.iter().map(|f| format!("    {f}")).collect::<Vec<_>>().join(",\n");
        let want = RpcError::new(RPC_TYPE_ERROR, format!("Wrong type passed:\n{{\n{body}\n}}"));
        assert_eq!(call_method(&server, METHOD, &p), Err(want), "{p}");
    }
}
