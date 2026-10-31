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

use common::mine::{Mined, coinbase, mine};
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

/// `block` through `submitblock`: the reply's result.
fn submit(node: &RpcNode, block: &Mined) -> String {
    node.call("submitblock", &format!(r#"["{}"]"#, block.hex)).result().to_owned()
}

/// `getblock <hash> 0`: the block's hex, or the error when the node does not serve it.
fn served(node: &RpcNode, block: &Mined) -> Result<String, String> {
    let reply = node.call("getblock", &format!(r#"["{}", 0]"#, display(&block.hash)));
    match reply.status {
        200 => Ok(reply.result().trim_matches('"').to_owned()),
        _ => Err(reply.body),
    }
}

/// BL-517: blocks a node stored but did not connect survive a clean restart, as Core's do (their
/// entries keep `BLOCK_HAVE_DATA` in the block index): a valid block on a side branch with less
/// work than the tip, and a block refused at connect, which Core marks `BLOCK_FAILED_VALID`.
/// `getblock` serves both before and after the restart.
// Core: validation.cpp AcceptBlock (ReceivedBlockTransactions sets BLOCK_HAVE_DATA) and
// ConnectBlock's bad-cb-amount (InvalidBlockFound sets BLOCK_FAILED_VALID); rpc/blockchain.cpp
// getblock reads any block with data.
#[test]
fn stored_blocks_off_the_active_chain_survive_a_clean_restart() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let start = now();
    let mut prev = internal(&tip(&node).1);
    let mut chain = Vec::new();
    for height in 1..=3u8 {
        let block = mine(&prev, start + u32::from(height), &[coinbase(height, 0)], None, true);
        assert_eq!(submit(&node, &block), "null", "block {height}");
        prev = block.hash;
        chain.push(block);
    }
    let side = mine(&chain[0].hash, start + 10, &[coinbase(2, 1)], None, true);
    let side_reply = submit(&node, &side);
    let bad = mine(&prev, start + 4, &[coinbase(4, 21_000_000 * 100_000_000)], None, true);
    let bad_reply = submit(&node, &bad);
    let before = tip(&node);
    assert_eq!(before, (3, display(&prev)), "side {side_reply} bad {bad_reply}");
    assert_eq!(bad_reply, r#""bad-cb-amount""#);
    for (name, block) in [("side", &side), ("bad", &bad)] {
        assert_eq!(served(&node, block), Ok(block.hex.clone()), "{name} before the restart");
    }
    let node = restarted(node, home.path());
    assert_eq!(tip(&node), before);
    for (name, block) in [("side", &side), ("bad", &bad)] {
        assert_eq!(served(&node, block), Ok(block.hex.clone()), "{name} after the restart");
    }
    node.stop();
}

/// The debug log's messages after byte `from`, without their timestamps.
fn messages(log: &std::path::Path, from: usize) -> Vec<String> {
    read(log)
        .get(from..)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, rest)| rest.to_owned()))
        .collect()
}

/// Core's `LoadChainTip` line at each restart: none on a fresh datadir (no coins), then the
/// stored tip with its height, date and progress, before VerifyDB's first line. At genesis
/// VerifyDB logs nothing: it returns before its first line for a tip with no parent (BL-275 (b)).
/// `feature_init.py` waits for the line on every start (BL-122, asserted-log row 142).
// Core: validation.cpp:5161-5165, :5189-5191, :5204; node/chainstate.cpp:272-277;
// test/functional/feature_init.py:88
#[test]
fn a_restart_logs_core_s_loaded_best_chain_line() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let log = node.node.log.clone();
    let loaded = |from: usize| -> Vec<String> {
        messages(&log, from).into_iter().filter(|l| l.starts_with("Loaded best chain: ")).collect()
    };
    assert_eq!(loaded(0), Vec::<String>::new(), "a fresh datadir's coins are empty");
    let mut from = read(&log).len();
    let node = restarted(node, home.path());
    let (height, hash) = tip(&node);
    assert_eq!(height, 0);
    let at_genesis = messages(&log, from);
    let lines = loaded(from);
    assert_eq!(lines.len(), 1, "{at_genesis:?}");
    assert!(
        lines[0].starts_with(&format!("Loaded best chain: hashBestChain={hash} height=0 date=")),
        "{lines:?}"
    );
    assert!(
        !at_genesis
            .iter()
            .any(|l| l.starts_with("Verifying last ") || l.starts_with("Verification")),
        "{at_genesis:?}"
    );
    let mined = generate(&node, 5);
    from = read(&log).len();
    let node = restarted(node, home.path());
    let after = messages(&log, from);
    let lines = loaded(from);
    assert_eq!(lines.len(), 1, "{after:?}");
    let tail = lines[0]
        .strip_prefix(&format!("Loaded best chain: hashBestChain={} height=5 date=", mined[4]))
        .unwrap_or_else(|| panic!("{lines:?}"));
    // `FormatISO8601DateTime`, then `%f`.
    let (date, progress) = tail.split_once(" progress=").expect("progress");
    let shape: String = date
        .chars()
        .map(|c| {
            if c.is_ascii_digit() {
                '9'
            } else {
                c
            }
        })
        .collect();
    assert_eq!(shape, "9999-99-99T99:99:99Z", "{date}");
    let (whole, places) = progress.split_once('.').expect("%f");
    assert!(whole.chars().all(|c| c.is_ascii_digit()) && places.len() == 6, "{progress}");
    let at = |p: &str| after.iter().position(|l| l.starts_with(p));
    assert!(at("Loaded best chain: ") < at("Verifying last 5 blocks at level 3"), "{after:?}");
    assert!(at("Verifying last 5 blocks at level 3").is_some(), "{after:?}");
    node.stop();
}
