//! Core's `rpc_getchaintips.py` as a process test (CH3b b2b-2, F3): four nodes split into two
//! halves that mine chains of 10 and 20 blocks, then joined; `getchaintips` compared as whole
//! objects. It runs once with every node dashd v24 and once with every node rdashd; each run
//! asserts the FT's assertions in its order, and both record their tips, which must be equal in
//! full.
//!
//! Adaptations, on both binaries:
//! - A2: an own 200-block chain instead of the framework's cache, which rdashd refuses until
//!   CH5-4 ([`cached_chain`]); then the nodes connect as `setup_network` does (node i+1 to
//!   node i).
//! - A3: no mempool sync: `sync_all` becomes `sync_blocks` (no step relays a transaction).
//! - A4, A6: the framework's flags, but `-blocksonly=1`, rdashd's only mode with no mempool.
//! - A7: every wait is scaled by one timeout factor.
//!
//! It cannot be skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use common::net::*;

fn scenario(binary: &Binary) -> Record {
    let mut rec = Record::default();
    let mut net = cached_chain(binary, 4);
    for i in 0..3 {
        net.connect(i + 1, i);
    }
    net.sync_blocks(&[0, 1, 2, 3]);
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
