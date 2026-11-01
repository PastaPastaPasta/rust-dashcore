//! Core's `rpc_preciousblock.py` as a process test (CH3b b2b-2, F2): three nodes on a clean
//! chain, competing branches moved by RPC (`node_sync_via_rpc`), and `preciousblock`
//! tie-breaking, run once with every node dashd v24 and once with every node rdashd. Each run
//! asserts the FT's assertions in its order; both runs record their block hashes and tips, which
//! must be equal in full.
//!
//! Adaptations (PLAN A3, A4, A6, A7), on both binaries: no mempool sync (no step relays a
//! transaction, and rdashd serves no `getrawmempool` yet); the framework's flags but
//! `-blocksonly=1`, rdashd's only mode with no mempool; every wait scaled by one timeout factor.
//! It cannot be skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use common::net::*;

/// `unidirectional_node_sync_via_rpc` (FT lines 12-25): walk back from `src`'s tip to a block
/// `dest` has, then submit the missing blocks oldest first; each `submitblock` gives null or
/// `inconclusive`.
fn unidirectional_node_sync_via_rpc(src: &Node, dest: &Node) {
    let mut blocks_to_copy = Vec::new();
    let mut blockhash = src.best_hash();
    loop {
        match dest.try_call("getblock", &format!("[{},false]", json_str(&blockhash))) {
            Ok(raw) => {
                assert!(!as_str(&raw).is_empty());
                break;
            }
            Err(_) => {
                let header =
                    src.call("getblockheader", &format!("[{},true]", json_str(&blockhash)));
                blocks_to_copy.push(blockhash);
                blockhash = as_str(field(&header, "previousblockhash")).to_owned();
            }
        }
    }
    blocks_to_copy.reverse();
    for blockhash in blocks_to_copy {
        let blockdata = src.call("getblock", &format!("[{},false]", json_str(&blockhash)));
        let result = dest.submit(as_str(&blockdata));
        assert!(
            result.is_null() || result == Json::Str("inconclusive".into()),
            "{}",
            result.write()
        );
    }
}

/// `node_sync_via_rpc` (FT lines 27-32): every ordered pair of `nodes`.
fn node_sync_via_rpc(net: &Net, nodes: &[usize]) {
    for &src in nodes {
        for &dest in nodes {
            if src != dest {
                unidirectional_node_sync_via_rpc(&net[src], &net[dest]);
            }
        }
    }
}

fn scenario(binary: &Binary) -> Record {
    let mut rec = Record::default();
    let mut net = Net::start(binary, 3, TIME_GENESIS_BLOCK, &[]);

    // Ensure submitblock can in principle reorg to a competing chain.
    net[0].generate(1);
    assert_eq!(net[0].block_count(), 1);
    let hash_z = net[1].generate_tip(2);
    assert_eq!(net[1].block_count(), 2);
    node_sync_via_rpc(&net, &[0, 1, 2]);
    assert_eq!(net[0].best_hash(), hash_z);
    rec.push("Z", &hash_z);

    // Mine blocks A-B-C on node 0, competing blocks E-F-G on node 1.
    let hash_c = net[0].generate_tip(3);
    assert_eq!(net[0].block_count(), 5);
    let hash_g = net[1].generate_tip(3);
    assert_eq!(net[1].block_count(), 5);
    assert_ne!(hash_c, hash_g);
    rec.push("C", &hash_c);
    rec.push("G", &hash_g);

    // Connect nodes and check no reorg occurs: the competing blocks go by RPC first, so any
    // reorg happens before the check (no way to wait on inaction for P2P sync).
    node_sync_via_rpc(&net, &[0, 1]);
    net.connect(0, 1);
    assert_eq!(net[0].best_hash(), hash_c);
    assert_eq!(net[1].best_hash(), hash_g);

    // Node 0 prefers G, then C again.
    net[0].precious(&hash_g);
    assert_eq!(net[0].best_hash(), hash_g);
    net[0].precious(&hash_c);
    assert_eq!(net[0].best_hash(), hash_c);
    // Node 1 prefers C (and may first need to download it).
    net[1].precious(&hash_c);
    net.sync_blocks(&[0, 1]);
    assert_eq!(net[1].best_hash(), hash_c);
    // Node 1 prefers G again, node 0 G again, node 1 C again.
    net[1].precious(&hash_g);
    assert_eq!(net[1].best_hash(), hash_g);
    net[0].precious(&hash_g);
    assert_eq!(net[0].best_hash(), hash_g);
    net[1].precious(&hash_c);
    assert_eq!(net[1].best_hash(), hash_c);

    // Mine (E-F-G-)H on node 0 and reorg node 1.
    net[0].generate(1);
    assert_eq!(net[0].block_count(), 6);
    net.sync_blocks(&[0, 1]);
    let hash_h = net[0].best_hash();
    assert_eq!(net[1].best_hash(), hash_h);
    rec.push("H", &hash_h);
    // Node 1 can no longer prefer C.
    net[1].precious(&hash_c);
    assert_eq!(net[1].best_hash(), hash_h);

    // Competing blocks I-J-K-L on node 2.
    net[2].generate(4);
    assert_eq!(net[2].block_count(), 6);
    let hash_l = net[2].best_hash();
    rec.push("L", &hash_l);
    // Connect nodes and check no reorg occurs.
    node_sync_via_rpc(&net, &[1, 2]);
    net.connect(1, 2);
    net.connect(0, 2);
    assert_eq!(net[0].best_hash(), hash_h);
    assert_eq!(net[1].best_hash(), hash_h);
    assert_eq!(net[2].best_hash(), hash_l);
    // Node 1 prefers L, node 2 prefers H.
    net[1].precious(&hash_l);
    assert_eq!(net[1].best_hash(), hash_l);
    net[2].precious(&hash_h);
    assert_eq!(net[2].best_hash(), hash_h);

    // Node 0 fetches the equal-work I-L branch from node 2 in the background; the record waits
    // until L's entry leaves `headers-only` and records whatever status it then has.
    wait_for("node 0's L entry to leave headers-only", 60, || {
        net[0].chain_tips().iter().any(|tip| {
            as_str(field(tip, "hash")) == hash_l && as_str(field(tip, "status")) != "headers-only"
        })
    });
    for i in 0..3 {
        rec.tips(&format!("node {i} tips"), &net[i]);
    }
    net.stop_all();
    rec
}

#[test]
fn rpc_preciousblock_runs_on_dashd_v24_and_rdashd_alike() {
    run_on_both("preciousblock", scenario);
}
