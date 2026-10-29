//! BL-504: the blocks a node connected since V's last flush survive a clean restart, whichever path
//! connected them (the miner's `generate*` or `submitblock`), because Core's two shutdown flushes
//! write V's state (BL-271). The first block at the current time ends initial block download, so
//! V's last flush is at block 1: a shutdown that only synced the WAL came back there. A killed
//! node comes back at its last flush, as Core's does.
// Core: rpc/mining.cpp generateBlocks → ProcessNewBlock; init.cpp:379-385, :449-456;
// validation.cpp:3128
#![cfg(unix)]

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use common::mine::{coinbase, mine};
use common::*;

/// The node's height and tip hash.
fn tip(node: &RpcNode) -> (u32, String) {
    let height = node.call("getblockcount", "[]").result().parse().unwrap();
    let hash = node.call("getbestblockhash", "[]").result().trim_matches('"').to_owned();
    (height, hash)
}

/// `n` blocks mined to `raw(51)`: their hashes.
fn generate(node: &RpcNode, n: u32) -> Vec<String> {
    let reply = node.call("generatetodescriptor", &format!(r#"[{n}, "raw(51)"]"#));
    let list = reply.result().trim_start_matches('[').trim_end_matches(']');
    list.split(',').map(|h| h.trim().trim_matches('"').to_owned()).collect()
}

fn now() -> u32 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    u32::try_from(secs).unwrap()
}

/// A block hash in RPC (display) order from its internal bytes, and back.
fn display(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

fn internal(hash: &str) -> [u8; 32] {
    let mut bytes = [0; 32];
    for (i, b) in bytes.iter_mut().rev().enumerate() {
        *b = u8::from_str_radix(&hash[2 * i..2 * i + 2], 16).unwrap();
    }
    bytes
}

/// Stops `node` with SIGTERM, as the report's repro, and starts it again on `datadir`.
fn restarted(node: RpcNode, home: &std::path::Path) -> RpcNode {
    let datadir = node.datadir.clone();
    node.node.stop_ok(libc::SIGTERM);
    RpcNode::start(home, &datadir, &[])
}

/// Blocks from `generatetodescriptor`, with and without `setmocktime`, then a clean restart: the
/// restart loads the same height and tip.
#[test]
fn generated_blocks_survive_a_clean_restart() {
    for (n, mocktime) in [(5, false), (5, true), (101, false)] {
        let (home, datadir) = home_and_datadir();
        let node = RpcNode::start(home.path(), &datadir, &[]);
        if mocktime {
            assert_eq!(node.call("setmocktime", &format!("[{}]", now())).result(), "null");
        }
        let mined = generate(&node, n);
        let before = tip(&node);
        assert_eq!(before, (n, mined.last().unwrap().clone()), "n {n} mocktime {mocktime}");
        let node = restarted(node, home.path());
        assert_eq!(tip(&node), before, "n {n} mocktime {mocktime}");
        node.stop();
    }
}

/// Blocks with current times through `submitblock`, then a clean restart: the first leaves
/// initial block download as the miner's does, and the restart loads the same height and tip.
#[test]
fn submitted_blocks_with_current_times_survive_a_clean_restart() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let mut prev = internal(&tip(&node).1);
    let start = now();
    for height in 1..=5u8 {
        let block = mine(&prev, start + u32::from(height), &[coinbase(height, 0)], None, true);
        let reply = node.call("submitblock", &format!(r#"["{}"]"#, block.hex));
        assert_eq!(reply.result(), "null", "block {height}");
        prev = block.hash;
    }
    let before = tip(&node);
    assert_eq!(before, (5, display(&prev)));
    let node = restarted(node, home.path());
    assert_eq!(tip(&node), before);
    node.stop();
}

/// A node killed after mining comes back at its last flush, not at its tip: V's flush on leaving
/// initial block download, at block 1 (its one coin), waited for in the bench log. The import
/// ends first: a block connected while `fImporting` is set is still in initial block download.
/// Core has no such flush and comes back at genesis there (BL-442).
#[test]
fn a_killed_node_comes_back_at_its_last_flush() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &["-debug=bench"]);
    let log = node.node.log.clone();
    wait_until("the import's end", || read(&log).contains("loadblk thread exit"));
    let mined = generate(&node, 5);
    assert_eq!(tip(&node).0, 5);
    wait_until("the flush on leaving initial block download", || {
        read(&log)
            .lines()
            .any(|l| l.contains("write coins cache to disk (1 coins,") && l.contains("completed"))
    });
    node.node.stop(libc::SIGKILL);
    let node = RpcNode::start(home.path(), &datadir, &[]);
    assert_eq!(tip(&node), (1, mined[0].clone()));
    node.stop();
}
