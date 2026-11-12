//! Core's `rpc_getblockfrompeer.py` to line 86 as a process test (BL-503): two nodes mine
//! competing branches of 4 and 3 blocks unconnected, then connect, so node 0 holds node 1's tip
//! as headers only and fetches it from node 1. It runs once with every node dashd v24 and once
//! with every node rdashd; each run asserts the FT's assertions in its order, and both record
//! them, which must be equal in full.
//!
//! Adaptations, on both binaries:
//! - A2: the framework's cache is [`cached_chain`], with no connection: the FT's
//!   `setup_network` is `setup_nodes` only (`:34-35`).
//! - Two nodes: the FT's node 2 (`-fastprune -prune=1`, `:27-31`) serves only its prune half
//!   (`:88-153`), which waits for `-prune` (CH8) and `-fastprune` (ST2).
//! - A7: every wait is scaled by one timeout factor.
//!
//! It cannot be skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use common::net::*;

/// `method params` on `node`, which must fail: its code and message, recorded under `label`.
fn refused(rec: &mut Record, label: &str, node: &Node, method: &str, params: &str) -> RpcError {
    let e = node.try_call(method, params).expect_err(label);
    rec.push(label, format!("{} {}", e.code, e.message));
    e
}

fn scenario(binary: &Binary) -> Record {
    let mut rec = Record::default();
    let mut net = cached_chain(binary, 2);
    let no_peers = |what: &str| {
        for i in 0..2 {
            assert!(net[i].peers().is_empty(), "node {i} {what}");
        }
    };
    no_peers("after the cache");

    // Lines 45-52.
    net[0].generate(4);
    assert_eq!(net[0].block_count(), 204);
    net[1].generate(3);
    assert_eq!(net[1].block_count(), 203);
    let short_tip = net[1].best_hash();
    rec.push("heights", format!("{} {}", net[0].block_count(), net[1].block_count()));
    no_peers("after mining");

    // Lines 54-56.
    net.connect(0, 1);
    net.sync_blocks(&[0, 1]);
    for i in 0..2 {
        assert_eq!(net[i].peers().len(), 1, "node {i}");
    }

    // Lines 58-61.
    let tips = rec.tips("node 0 tips", &net[0]);
    let short = tips.iter().find(|t| as_str(field(t, "hash")) == short_tip).expect("short tip");
    assert_eq!(as_str(field(short, "status")), "headers-only");
    let tip_param = json_str(&short_tip);
    let e = refused(&mut rec, "getblock short tip", &net[0], "getblock", &format!("[{tip_param}]"));
    assert_eq!((e.code, e.message.as_str()), (-1, "Block not found on disk"));

    // Lines 63-66.
    let peers = net[0].peers();
    assert_eq!(peers.len(), 1);
    let id = as_i64(field(&peers[0], "id"));
    assert_eq!(id, 0, "node 0's one peer");
    rec.push("peer id", id);

    // Lines 68-71.
    let fetch = |rec: &mut Record, label: &str, params: String| {
        refused(rec, label, &net[0], "getblockfrompeer", &params)
    };
    let e = fetch(&mut rec, "short hash", format!(r#"["1234",{id}]"#));
    assert_eq!(e.code, -8);
    assert!(e.message.contains("hash must be of length 64 (not 4, for '1234')"), "{e:?}");
    let e = fetch(&mut rec, "number hash", format!("[1234,{id}]"));
    assert_eq!(e.code, -3);
    assert!(e.message.contains("JSON value of type number is not of expected type string"));
    let e = fetch(&mut rec, "string peer", format!(r#"[{tip_param},"0"]"#));
    assert_eq!(e.code, -3);
    assert!(e.message.contains("JSON value of type string is not of expected type number"));

    // Lines 73-74.
    let e = fetch(&mut rec, "unknown header", format!(r#"["{}",0]"#, "00".repeat(32)));
    assert_eq!((e.code, e.message.as_str()), (-1, "Block header missing"));

    // Lines 76-78.
    for peer_id in [-1, id + 1] {
        let e = fetch(&mut rec, &format!("peer {peer_id}"), format!("[{tip_param},{peer_id}]"));
        assert_eq!((e.code, e.message.as_str()), (-1, "Peer does not exist"));
    }

    // Lines 80-83.
    let result = net[0].call("getblockfrompeer", &format!("[{tip_param},{id}]"));
    let stored = || net[0].try_call("getblock", &format!("[{tip_param}]")).is_ok();
    wait_for("node 0 to store the short tip", 1, stored);
    assert_eq!(result.write(), "{}");
    rec.push("fetched", result.write());

    // Lines 85-86.
    let e = fetch(&mut rec, "already downloaded", format!("[{tip_param},{id}]"));
    assert_eq!((e.code, e.message.as_str()), (-1, "Block already downloaded"));

    net.stop_all();
    rec
}

#[test]
fn rpc_getblockfrompeer_to_line_86_runs_on_dashd_v24_and_rdashd_alike() {
    run_on_both("getblockfrompeer", scenario);
}
