//! Core's `rpc_getchaintips.py` as a process test (CH3b b2b-2, F3): four nodes split into two
//! halves that mine chains of 10 and 20 blocks, then joined; `getchaintips` compared as whole
//! objects. It runs once with every node dashd v24 and once with every node rdashd; each run
//! asserts the FT's assertions in its order, and both record their tips, which must be equal in
//! full.
//!
//! Adaptations, on both binaries:
//! - A2: an own 200-block chain instead of the framework's cache, which rdashd refuses until
//!   CH5-4. It is built as `_initialize_chain` and `setup_nodes` build it
//!   (`test_framework.py:468-485, 902-906, 943-1002`): node 0 mines seven batches of 25 blocks
//!   and one of 24 to the framework's rotating addresses, mocktime advancing by each batch's
//!   size × 156 s first; every node's clock is set before it receives a batch, which the others
//!   take by `submitblock` as the framework copies the cache. Then mocktime goes to
//!   T + 199·156 and up by 200·156, node 0 mines block 200, every node submits it, and the nodes
//!   connect as `setup_network` does (node i+1 to node i). No step advances the scheduler, as in
//!   Core (`update_schedulers=False`).
//! - A3: no mempool sync: `sync_all` becomes `sync_blocks` (no step relays a transaction).
//! - A4, A6: the framework's flags, but `-blocksonly=1`, rdashd's only mode with no mempool.
//! - A7: every wait is scaled by one timeout factor.
//!
//! It cannot be skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use common::net::*;

/// The framework's cache addresses: the first three deterministic keys, then P2SH `OP_TRUE`
/// (`test_framework.py:991`).
const GEN_ADDRESSES: [&str; 4] =
    [NODE_ADDRESSES[0], NODE_ADDRESSES[1], NODE_ADDRESSES[2], ADDRESS_BCRT1_P2SH_OP_TRUE];

/// The cache's batches: 199 blocks, each batch after a mocktime bump of 156 s a block.
const BATCHES: [u64; 8] = [25, 25, 25, 25, 25, 25, 25, 24];

/// The cached chain's state at the start of `run_test`, on four connected nodes.
fn cached_chain(binary: &Binary) -> Net {
    let mut net = Net::start(binary, 4, TIME_GENESIS_BLOCK, &[]);
    let all = [0, 1, 2, 3];
    let mut mocktime = TIME_GENESIS_BLOCK;
    for (i, &n) in BATCHES.iter().enumerate() {
        mocktime += n * 156;
        net.mocktime(&all, mocktime, None);
        let batch = net[0].generate_to(n as u32, GEN_ADDRESSES[i % 4]);
        for hash in &batch {
            let raw = net[0].raw_block(hash);
            for dest in 1..4 {
                assert!(net[dest].submit(&raw).is_null());
            }
        }
    }
    assert_eq!(mocktime, TIME_GENESIS_BLOCK + 199 * 156);
    net.mocktime(&all, TIME_GENESIS_BLOCK + 199 * 156, None);
    net.mocktime(&all, TIME_GENESIS_BLOCK + 399 * 156, None);
    for i in all {
        assert_eq!(as_i64(field(&net[i].call("getblockchaininfo", "[]"), "blocks")), 199);
    }
    let block = net[0].raw_block(&net[0].generate_tip(1));
    for i in all {
        // Node 0 has it already.
        let result = net[i].submit(&block);
        assert!(i == 0 || result.is_null(), "{}", result.write());
        let info = net[i].call("getblockchaininfo", "[]");
        assert_eq!(as_i64(field(&info, "blocks")), 200);
        assert!(field(&info, "initialblockdownload").is_false());
    }
    for i in 0..3 {
        net.connect(i + 1, i);
    }
    net.sync_blocks(&all);
    net
}

fn scenario(binary: &Binary) -> Record {
    let mut rec = Record::default();
    let mut net = cached_chain(binary);
    assert_eq!(TIME_GENESIS_BLOCK + 399 * 156, 1_417_775_581);
    let best = net[0].best_hash();
    for i in 0..4 {
        let info = net[i].call("getblockchaininfo", "[]");
        assert_eq!(net[i].best_hash(), best);
        assert!(field(&info, "initialblockdownload").is_false());
    }
    rec.push("block 200", &best);

    // Lines 21-25.
    single_active_tip(&net[0], 200);

    // Split the network and build two chains of different lengths (lines 27-30).
    net.disconnect(1, 2);
    net.sync_blocks(&[0, 1]);
    net.sync_blocks(&[2, 3]);
    net[0].generate(10);
    net.sync_blocks(&[0, 1]);
    net[2].generate(20);
    net.sync_blocks(&[2, 3]);

    // Lines 32-44.
    let short_tip = single_active_tip(&net[1], 210);
    let long_tip = single_active_tip(&net[3], 220);
    rec.push("short tip", short_tip.write());
    rec.push("long tip", long_tip.write());

    // Join the halves: node 0, which had the short chain, now has two tips (lines 46-61).
    net.connect(1, 2);
    net.sync_blocks(&[0, 1, 2, 3]);
    let mut tips = rec.tips("joined tips", &net[0]);
    assert_eq!(tips.len(), 2);
    assert_eq!(tips[0], long_tip);
    assert_eq!(as_i64(field(&tips[1], "branchlen")), 10);
    assert_eq!(as_str(field(&tips[1], "status")), "valid-fork");
    // The long tip is the active one; make the short tip read as it did when it was active.
    let hash = field(&tips[1], "hash").clone();
    set(&mut tips[1], "branchlen", Json::Num("0".into()));
    set(&mut tips[1], "status", Json::Str("active".into()));
    set(&mut tips[1], "forkpoint", hash);
    assert_eq!(tips[1], short_tip);

    net.stop_all();
    rec
}

/// `node`'s one tip, asserted as the FT does: alone, `branchlen` 0, at `height`, `active`.
fn single_active_tip(node: &Node, height: i64) -> Json {
    let tips = node.chain_tips();
    assert_eq!(tips.len(), 1);
    assert_eq!(as_i64(field(&tips[0], "branchlen")), 0);
    assert_eq!(as_i64(field(&tips[0], "height")), height);
    assert_eq!(as_str(field(&tips[0], "status")), "active");
    tips[0].clone()
}

/// Sets `key` in the object `v`, as Python's `tips[1][key] = value`.
fn set(v: &mut Json, key: &str, value: Json) {
    let Json::Obj(fields) = v else {
        panic!("not an object")
    };
    let slot = fields.iter_mut().find(|(k, _)| k == key).unwrap_or_else(|| panic!("no {key}"));
    slot.1 = value;
}

#[test]
fn rpc_getchaintips_runs_on_dashd_v24_and_rdashd_alike() {
    run_on_both("getchaintips", scenario);
}
