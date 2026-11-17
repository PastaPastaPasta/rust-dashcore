//! Core's `p2p_invalid_block.py` as a process test (CH3b b2b-2, F4): one node with
//! `-whitelist=noban@127.0.0.1` and a scripted `P2PDataStore` peer, which sends a valid block, a
//! mutated block (CVE-2012-2459), blocks with duplicate inputs (CVE-2018-17144), a bad coinbase
//! amount, the original of the mutated block (which the node must request again), and a block
//! from the future that is accepted once the clock moves one second. It runs once with dashd v24
//! and once with rdashd; each run asserts the FT's assertions in its order, reading the P2P
//! node's real debug log in each step's window, and both record their block hashes and tips,
//! which must be equal in full.
//!
//! Beyond the FT, on both binaries: step 1's window holds the peer's score line
//! (`Misbehaving: peer=<id> (0 -> 100)`) and the noban warning, and the future block's
//! processing scores nothing, since Core never scores a block from the future
//! (`net_processing.cpp:1933-1954, 1969-2018, 5855`). A peer's messages are processed in turn, so
//! that processing runs from the block's first `received block` line to the peer's next message.
//!
//! A second node is an isolated `submitblock` mirror: it takes the exact accepted blocks in order
//! and the same clock, and each invalid block must get the FT's reason as its BIP22 reply there.
//! Its replies are recorded apart from the P2P node's log, which alone is the evidence for the
//! log rows.
//!
//! The FT's fixtures are kept (`blocktools.py:48-57, 175-213`; `messages.py:536-542`): block
//! version 4, transaction version 2, `OP_TRUE` input scripts, empty output scripts, 50-coin
//! spends and a 1000-coin coinbase, built here byte by byte. Adaptations, on both binaries: the
//! peer is ours (PLAN A5); the framework's flags, but `-blocksonly=1` (A4, A6); every wait,
//! the FT's 5 s and the log window's 2 s included, is scaled by one timeout factor (A7). It
//! cannot be skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use common::mine::{compact_size, height_push, merkle_root, txid};
use common::net::*;
use common::peer::{Block, Expect, Peer, hash_hex, header_hash, unhex_hash};

const COIN: i64 = 100_000_000;
/// `blocktools.py`'s `MAX_FUTURE_BLOCK_TIME`.
const MAX_FUTURE_BLOCK_TIME: u64 = 2 * 60 * 60;
const OP_TRUE: u8 = 0x51;
const REGTEST_BITS: u32 = 0x207f_ffff;

/// `script_BIP34_coinbase_height`: `OP_<n>` and `OP_TRUE` to 16, else the height's push.
fn bip34_height(height: u16) -> Vec<u8> {
    if height <= 16 {
        return vec![0x50 + height as u8, OP_TRUE];
    }
    height_push(height)
}

/// `create_coinbase(height, nValue=coins)`: version 2, the BIP34 script, one `OP_TRUE` output of
/// `coins` (halved every 150 blocks when it is the default 500).
fn create_coinbase(height: u16, coins: i64) -> Vec<u8> {
    let mut value = coins * COIN;
    if coins == 500 {
        value >>= height / 150;
    }
    let script = bip34_height(height);
    let mut tx = 2i32.to_le_bytes().to_vec();
    tx.push(1);
    tx.extend([0; 32]);
    tx.extend(u32::MAX.to_le_bytes());
    compact_size(&mut tx, script.len());
    tx.extend(script);
    tx.extend(u32::MAX.to_le_bytes());
    tx.push(1);
    tx.extend(value.to_le_bytes());
    tx.extend([1, OP_TRUE]);
    tx.extend(0u32.to_le_bytes());
    tx
}

/// `create_tx_with_script(..., script_sig=bytes([OP_TRUE]), amount)`, with `inputs` as its
/// inputs (two equal ones for a duplicated input): version 2, final sequences, one output with
/// an empty script.
fn create_tx(inputs: &[([u8; 32], u32)], amount: i64) -> Vec<u8> {
    let mut tx = 2i32.to_le_bytes().to_vec();
    compact_size(&mut tx, inputs.len());
    for (txid, n) in inputs {
        tx.extend(txid);
        tx.extend(n.to_le_bytes());
        tx.extend([1, OP_TRUE]);
        tx.extend(u32::MAX.to_le_bytes());
    }
    tx.push(1);
    tx.extend(amount.to_le_bytes());
    tx.push(0);
    tx.extend(0u32.to_le_bytes());
    tx
}

/// A header's 80 bytes.
fn header(prev: &[u8; 32], merkle: &[u8; 32], time: u32, nonce: u32) -> Vec<u8> {
    let mut h = 4i32.to_le_bytes().to_vec();
    h.extend(prev);
    h.extend(merkle);
    h.extend(time.to_le_bytes());
    h.extend(REGTEST_BITS.to_le_bytes());
    h.extend(nonce.to_le_bytes());
    h
}

/// The block of `header` and `txs`.
fn assemble(header: &[u8], txs: &[Vec<u8>]) -> Block {
    let mut bytes = header.to_vec();
    compact_size(&mut bytes, txs.len());
    for tx in txs {
        bytes.extend(tx);
    }
    Block::from_bytes(bytes)
}

/// `create_block(prev, coinbase, time, txlist)` then `solve()` from nonce `nonce`: the first
/// nonce upward whose hash is at most regtest's target (`0x7fffff << 232`).
fn create_block(prev: &[u8; 32], time: u64, txs: &[Vec<u8>], mut nonce: u32) -> Block {
    let merkle = merkle_root(txs);
    let time = u32::try_from(time).unwrap();
    let mut target = [0u8; 32];
    target[..3].copy_from_slice(&[0x7f, 0xff, 0xff]);
    loop {
        let h = header(prev, &merkle, time, nonce);
        let mut hash = header_hash(&h);
        hash.reverse();
        if hash <= target {
            return assemble(&h, txs);
        }
        nonce += 1;
    }
}

/// The node's best block: hash (internal order), height and time.
fn best(node: &Node) -> ([u8; 32], u16, u64) {
    let hash = node.best_hash();
    let block = node.call("getblock", &format!("[{}]", json_str(&hash)));
    let tip = unhex_hash(&hash);
    let height = u16::try_from(as_i64(field(&block, "height"))).unwrap();
    (tip, height, as_i64(field(&block, "time")) as u64)
}

/// The lines of `log` from the first `received block <hash> peer=<id>` to the next line
/// ending `received: ... peer=<id>`, exclusive: the block's processing. The test fails
/// without either line.
fn processing_of<'a>(log: &'a str, hash: &str, id: i64) -> Vec<&'a str> {
    let lines: Vec<&str> = log.lines().collect();
    let first = format!("received block {hash} peer={id}");
    let start = lines
        .iter()
        .position(|l| l.ends_with(&first))
        .unwrap_or_else(|| panic!("no {first:?} in the window:\n{log}"));
    let suffix = format!(" peer={id}");
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.contains("received: ") && l.ends_with(&suffix))
        .unwrap_or_else(|| panic!("no later message from peer {id} in the window:\n{log}"));
    lines[start..start + 1 + end].to_vec()
}

/// The mirror accepts `block`: `submitblock` gives null.
fn accepted(mirror: &Node, block: &Block) {
    let result = mirror.submit(&block.hex());
    assert!(result.is_null(), "{}", result.write());
}

/// The mirror rejects `block` with `reason`: `submitblock` gives exactly that string.
fn rejected(mirror: &Node, block: &Block, reason: &str) {
    assert_eq!(mirror.submit(&block.hex()), Json::Str(reason.into()));
}

fn scenario(binary: &Binary) -> Record {
    let mut rec = Record::default();
    let mut net = Net::start(binary, 2, TIME_GENESIS_BLOCK, &["-whitelist=noban@127.0.0.1"]);
    let mut peer = Peer::connect(&net[0]);
    let (node, mirror) = (&net[0], &net[1]);

    let (tip, height, mut block_time) = best(node);
    let height = height + 1;
    block_time += 1;

    // Step 0: a block with an anyone-can-spend coinbase, then 100 blocks to mature it.
    let cb1 = create_coinbase(height, 500);
    let block1 = create_block(&tip, block_time, std::slice::from_ref(&cb1), 0);
    peer.send_blocks_and_test(node, &[&block1], Expect::default());
    accepted(mirror, &block1);
    rec.push("block 1", block1.hash_hex());
    for hash in node.generate_to(100, NODE_ADDRESSES[0]) {
        assert!(mirror.submit(&node.raw_block(&hash)).is_null());
    }
    peer.answer();

    let (tip, height, mut block_time) = best(node);
    let mut height = height + 1;
    block_time += 1;
    rec.push("matured tip", hash_hex(&tip));

    // Step 1: merkle-root malleability (CVE-2012-2459). The third transaction repeated keeps the
    // merkle root and header but invalidates the block.
    let tx1 = create_tx(&[(txid(&cb1), 0)], 50 * COIN);
    let tx2 = create_tx(&[(txid(&tx1), 0)], 50 * COIN);
    let txs2 = vec![create_coinbase(height, 500), tx1.clone(), tx2.clone()];
    let block2_time = block_time;
    let block2_orig = create_block(&tip, block2_time, &txs2, 0);
    block_time += 1;
    let mutated_txs = [txs2.clone(), vec![tx2.clone()]].concat();
    let block2 = assemble(&block2_orig.bytes[..80], &mutated_txs);
    assert_eq!(merkle_root(&mutated_txs)[..], block2.bytes[36..68]);
    assert_eq!(block2.hash, block2_orig.hash);
    assert_ne!(txs2, mutated_txs);
    let scored = format!("Misbehaving: peer={} (0 -> 100)", peer.id);
    let noban = format!("Warning: not punishing noban peer {}!", peer.id);
    let window = peer.send_blocks_and_test(
        node,
        &[&block2],
        Expect::rejected(&[
            "bad-txns-duplicate",
            "Block mutated: bad-txns-duplicate",
            &scored,
            &noban,
        ]),
    );
    assert!(window.contains("bad-txns-duplicate"));
    rejected(mirror, &block2, "bad-txns-duplicate");
    rec.push("block 2", block2_orig.hash_hex());

    // Step 2: duplicate inputs (CVE-2018-17144), re-rooted and solved on from the original's
    // nonce.
    let tx2_dup = create_tx(&[(txid(&tx1), 0), (txid(&tx1), 0)], 50 * COIN);
    let nonce = u32::from_le_bytes(block2_orig.bytes[76..80].try_into().unwrap());
    let block2_dup =
        create_block(&tip, block2_time, &[txs2[0].clone(), tx1.clone(), tx2_dup], nonce);
    let reason = "bad-txns-inputs-duplicate";
    peer.send_blocks_and_test(node, &[&block2_dup], Expect::rejected(&[reason]));
    rejected(mirror, &block2_dup, reason);
    rec.push("block 2 dup", block2_dup.hash_hex());

    // Step 3: a coinbase paying 1000 coins.
    let block3 = create_block(&tip, block_time, &[create_coinbase(height, 1000)], 0);
    block_time += 1;
    peer.send_blocks_and_test(node, &[&block3], Expect::rejected(&["bad-cb-amount"]));
    assert_eq!(mirror.best_hash(), hash_hex(&tip), "the mirror is on block 3's parent");
    rejected(mirror, &block3, "bad-cb-amount");
    rec.push("block 3", block3.hash_hex());

    // Step 4: the original block 2, with the mutated block's hash: the store's entry is replaced
    // and the node must request it again.
    peer.send_blocks_and_test(
        node,
        &[&block2_orig],
        Expect {
            timeout: 5,
            ..Expect::default()
        },
    );
    accepted(mirror, &block2_orig);
    height += 1;
    block_time += 1;
    let tip = block2_orig.hash;

    // Step 5: a block spending tx2's output twice (the inflation bug).
    let tx3 = create_tx(&[(txid(&tx2), 0), (txid(&tx2), 0)], 50 * COIN);
    let block4 = create_block(&tip, block_time, &[create_coinbase(height, 500), tx3], 0);
    peer.send_blocks_and_test(node, &[&block4], Expect::rejected(&[reason]));
    assert_eq!(mirror.best_hash(), hash_hex(&tip), "the mirror is on block 4's parent");
    rejected(mirror, &block4, reason);
    rec.push("block 4", block4.hash_hex());

    // Step 6: a block one second past the future limit, sent unsolicited, is rejected unscored;
    // a second later the same bytes are accepted.
    let t = TIME_GENESIS_BLOCK;
    let future =
        create_block(&tip, t + MAX_FUTURE_BLOCK_TIME + 1, &[create_coinbase(height, 500)], 0);
    let window = peer.send_blocks_and_test(
        node,
        &[&future],
        Expect {
            force_send: true,
            ..Expect::rejected(&["time-too-new"])
        },
    );
    let misbehaving = format!("Misbehaving: peer={} ", peer.id);
    let stretch = processing_of(&window, &future.hash_hex(), peer.id);
    assert!(
        !stretch.iter().any(|l| l.contains(&misbehaving)),
        "the future block was scored: {stretch:#?}"
    );
    if binary.kind == Kind::Dashd {
        // The stretch is the future block's: dashd names it there with its reason.
        let named = |l: &&str| l.contains(&future.hash_hex()) && l.contains("time-too-new");
        assert!(stretch.iter().any(named), "{stretch:#?}");
    }
    assert_eq!(mirror.best_hash(), hash_hex(&tip), "the mirror is on the future block's parent");
    rejected(mirror, &future, "time-too-new");
    rec.push("future block", future.hash_hex());
    net.mocktime(&[0, 1], t + 1, Some(1));
    let (node, mirror) = (&net[0], &net[1]);
    peer.send_blocks_and_test(node, &[&future], Expect::default());
    accepted(mirror, &future);

    rec.tips("node tips", node);
    rec.tips("mirror tips", mirror);
    drop(peer);
    net.stop_all();
    rec
}

/// The four ledger rows' proof (`asserted_log_lines.txt`): `bad-txns-duplicate` with its
/// `Block mutated:` line, `bad-txns-inputs-duplicate`, `bad-cb-amount` and `time-too-new`, each
/// in the P2P node's real debug log in its step's window, on rdashd as on dashd v24.
#[test]
fn p2p_invalid_block_runs_on_dashd_v24_and_rdashd_alike() {
    run_on_both("p2p_invalid_block", scenario);
}
