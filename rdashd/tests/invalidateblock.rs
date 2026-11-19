//! Core's `rpc_invalidateblock.py` as a process test (CH3b b2b-2, F1): three nodes on a clean
//! chain with competing branches; `invalidateblock` and `reconsiderblock`; the restart-time
//! failure-flag reset (`ResetBlockFailureFlags`) and recovery of the higher-work tip; and -5
//! "Block not found". It runs once with every node dashd v24 and once with every node rdashd;
//! each run asserts the FT's assertions in its order, and both record their heights, hashes and
//! tips, which must be equal in full.
//!
//! Adaptations, on both binaries:
//! - A1: the five restarts drop `-checkblocks=5`, which rdashd refuses until CH5. The failure-flag
//!   reset is gated on the check level, not the depth (Core `node/chainstate.cpp:436-465`), so
//!   the default level 3 still reaches it. Not proven here: levels 3 and 4's verification work
//!   (BL-524, CH5).
//! - A3: no mempool sync (the FT always passes `no_op` or `sync_blocks`).
//! - A4, A6: the framework's flags, but `-blocksonly=1`, rdashd's only mode with no mempool.
//! - A7: every wait, the FT's 5 s waits included, is scaled by one timeout factor.
//!
//! It cannot be skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use common::net::*;

/// Whether `node` has `hash`'s header.
fn knows(node: &Node, hash: &str) -> bool {
    node.try_call("getblockheader", &format!("[{}]", json_str(hash))).is_ok()
}

fn scenario(binary: &Binary) -> Record {
    let mut rec = Record::default();
    let mut net = Net::start(binary, 3, TIME_GENESIS_BLOCK, &[]);

    // Make sure we repopulate setBlockIndexCandidates after InvalidateBlock (lines 24-43).
    net[0].generate(4);
    assert_eq!(net[0].block_count(), 4);
    let besthash_n0 = net[0].best_hash();
    net[1].generate(6);
    assert_eq!(net[1].block_count(), 6);
    net.connect(0, 1);
    net.sync_blocks(&[0, 1]);
    assert_eq!(net[0].block_count(), 6);
    let badhash = net[1].block_hash(2);
    net[0].invalidate(&badhash);
    assert_eq!(net[0].block_count(), 4);
    assert_eq!(net[0].best_hash(), besthash_n0);
    rec.push("besthash_n0", &besthash_n0);
    rec.push("badhash", &badhash);

    // Make sure we won't reorg to a lower work chain (lines 45-61).
    net.connect(1, 2);
    net.sync_blocks(&[1, 2]);
    assert_eq!(net[2].block_count(), 6);
    net[1].invalidate(&net[1].block_hash(5));
    assert_eq!(net[1].block_count(), 4);
    net[2].invalidate(&net[2].block_hash(3));
    assert_eq!(net[2].block_count(), 2);
    net[2].generate(1);
    wait_for("node 2 at 3", 5, || net[2].block_count() == 3);
    wait_for("node 0 at 4", 5, || net[0].block_count() == 4);
    wait_for("node 1 at 4", 5, || net[1].block_count() == 4);
    // Node 1 hears of node 2's lower-work block in the background and never fetches it; its
    // recorded tips hold that header, so it must arrive before node 1's restart below.
    let node2_tip = net[2].best_hash();
    wait_for("node 1 to know node 2's tip", 60, || knows(&net[1], &node2_tip));
    for i in 0..3 {
        rec.push(&format!("node {i} best at line 61"), net[i].best_hash());
    }

    // Make sure ResetBlockFailureFlags does the job correctly (lines 63-77).
    net.restart(0);
    net.restart(1);
    net.connect(0, 1);
    net[0].generate(10);
    net.sync_blocks(&[0, 1]);
    let newheight = net[0].block_count();
    rec.push("newheight", newheight);
    for j in 0..2 {
        net.restart(0);
        let tip = net[0].generate_tip(10);
        let branch = net[1].generate_tip(9);
        net.connect(0, 1);
        net.sync_blocks(&[0, 1]);
        // Likewise node 0 and node 1's lower-work branch, before node 0's next restart or node
        // 1's (which ends their connection).
        wait_for("node 0 to know node 1's branch", 60, || knows(&net[0], &branch));
        assert_eq!(net[0].block_count(), newheight + 10 * (j + 1));
        assert_eq!(net[1].block_count(), newheight + 10 * (j + 1));
        assert_eq!(net[1].best_hash(), tip);
        rec.push(&format!("tip {j}"), &tip);
    }

    // Two lookups of one height, each right before its own invalidation: the first names node
    // 0's branch, the second, after it, node 1's competing branch (lines 79-86).
    let tip = net[1].best_hash();
    let first = net[1].block_hash(newheight + 1);
    net[1].invalidate(&first);
    let second = net[1].block_hash(newheight + 1);
    net[1].invalidate(&second);
    assert_ne!(first, second, "the two lookups name blocks on different branches");
    assert_eq!(net[1].block_count(), newheight);
    rec.push("first", &first);
    rec.push("second", &second);
    rec.tips("node 1 tips before the restart", &net[1]);
    net.restart(1);
    wait_for("node 1 back at the higher tip", 60, || net[1].block_count() == newheight + 20);
    assert_eq!(tip, net[1].best_hash());

    // Verify that we reconsider all ancestors as well (lines 88-98).
    let blocks = net[1].generate_to_descriptor(10, ADDRESS_BCRT1_UNSPENDABLE_DESCRIPTOR);
    assert_eq!(net[1].best_hash(), blocks[9]);
    net[1].invalidate(&blocks[9]);
    net[1].invalidate(&blocks[8]);
    assert_eq!(net[1].best_hash(), blocks[7]);
    net[1].reconsider(&blocks[9]);
    assert_eq!(net[1].best_hash(), blocks[9]);
    rec.push("ancestors leg", blocks.join(","));

    // Verify that we reconsider all descendants (lines 100-110).
    let blocks = net[1].generate_to_descriptor(10, ADDRESS_BCRT1_UNSPENDABLE_DESCRIPTOR);
    assert_eq!(net[1].best_hash(), blocks[9]);
    net[1].invalidate(&blocks[8]);
    net[1].invalidate(&blocks[6]);
    assert_eq!(net[1].best_hash(), blocks[5]);
    net[1].reconsider(&blocks[6]);
    assert_eq!(net[1].best_hash(), blocks[9]);
    rec.push("descendants leg", blocks.join(","));

    // Verify that invalidating an unknown block throws an error (lines 112-114).
    let err = net[1].try_call("invalidateblock", &format!("[{}]", json_str(&"00".repeat(32))));
    let err = err.expect_err("an unknown block");
    assert_eq!(err.code, -5);
    assert!(err.message.contains("Block not found"), "{err:?}");
    assert_eq!(net[1].best_hash(), blocks[9]);

    for i in 0..3 {
        rec.tips(&format!("node {i} tips"), &net[i]);
    }
    net.stop_all();
    rec
}

#[test]
fn rpc_invalidateblock_runs_on_dashd_v24_and_rdashd_alike() {
    run_on_both("invalidateblock", scenario);
}
