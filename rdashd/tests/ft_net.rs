//! CH3b b2b-2's harness checks, on dashd v24 and on rdashd: the scripted peer's handshake and its
//! initial header exchange (the node asks with `getheaders2`, since the peer advertises
//! `NODE_HEADERS_COMPRESSED`), blocks sent by headers and served on request, `connect`,
//! `sync_blocks` and `disconnect` on two nodes, `mocktime` with and without the scheduler, and
//! the binary-aware stop. The block both runs mine at the same clock must have the same hash. It
//! cannot be skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use common::mine::{GENESIS_TIME, coinbase, mine, parse_hex};
use common::net::*;
use common::peer::{Block, Expect, Peer, unhex_hash};

/// A block of `height` on `prev` at `time`, from the shared miner.
fn block_on(prev: &[u8; 32], height: u8, time: u32) -> Block {
    Block::from_bytes(parse_hex(&mine(prev, time, &[coinbase(height, 0)], None, true).hex))
}

fn harness(binary: &Binary) -> Record {
    let mut rec = Record::default();
    let mut net = Net::start(binary, 2, TIME_GENESIS_BLOCK, &["-whitelist=noban@127.0.0.1"]);

    // The handshake, then the node's first header request: compressed, answered from an empty
    // store with an empty `headers2`.
    let mut peer = Peer::connect(&net[0]);
    peer.answer_until("getheaders2", 60, |p| p.received.iter().any(|c| c == "getheaders2"));

    // Two blocks by headers, each requested and served; the store walk answers later requests.
    let genesis = unhex_hash(&net[0].block_hash(0));
    let time = GENESIS_TIME;
    let b1 = block_on(&genesis, 1, time + 1);
    let b2 = block_on(&b1.hash, 2, time + 2);
    for b in [&b1, &b2] {
        peer.send_blocks_and_test(&net[0], &[b], Expect::default());
    }
    assert_eq!(net[0].block_count(), 2);
    rec.push("b2", b2.hash_hex());

    // mocktime without, then with, the scheduler; node 1 mines at the new clock.
    net.mocktime(&[0, 1], TIME_GENESIS_BLOCK + 100, None);
    net.mocktime(&[0, 1], TIME_GENESIS_BLOCK + 101, Some(1));
    let mined = net[1].generate_tip(1);
    let header = net[1].call("getblockheader", &format!("[{}]", json_str(&mined)));
    assert_eq!(as_i64(field(&header, "time")), i64::from(GENESIS_TIME) + 101);
    rec.push("node 1's block", &mined);

    // Two nodes: node 1 follows node 0's longer chain, then they part.
    net.connect(1, 0);
    net.sync_blocks(&[0, 1]);
    assert_eq!(net[1].best_hash(), b2.hash_hex());
    net.disconnect(1, 0);
    assert!(net[0].peers().iter().all(|p| !as_str(field(p, "subver")).contains("testnode1")));
    rec.tips("node 1's tips", &net[1]);

    drop(peer);
    net.stop_all();
    rec
}

#[test]
fn the_ft_harness_drives_dashd_v24_and_rdashd_alike() {
    run_on_both("ft_net", harness);
}
