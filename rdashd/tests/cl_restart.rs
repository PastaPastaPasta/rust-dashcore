//! CH7b-3 C8: a datadir synced past V20 with step 10 live restarts, VerifyDB skipping level 3
//! with CH5-1a's marker. CH7b-2's C14 (`dashd-node`'s `recorded.rs`) restarts a datadir synced
//! under `-assumevalid`, where no block runs step 10; here every block of
//! `feature_llmq_chainlocks`'s run (dashd's node0, `test-vectors/statedigest/regtest-v24`) is
//! submitted under `-assumevalid=0`: the node's step 10 verifies each CbTx ChainLock, 44 from 309
//! (C7's census); under the stub, `submitblock` refuses 309 (W-1). Reopened with default flags,
//! the node loads at that tip. The startup check at level 3 skips with the readiness refusal
//! (DEC-993; `chain/verify.rs`, ALLOWED-DIFFS D18), and an explicit `-checklevel=4` is an init
//! error.
//!
//! This is not a level-4 verification of evo blocks: that is CH5-1b's (CH7b-3 erratum X9). The
//! census of the verifies is `dashd-node`'s (C7); this binary sees the process from outside. It
//! is a binary of its own so no other test shares its tracing (BL-479).
#![cfg(unix)]

mod common;

use std::path::Path;

use common::mine::hex;
use common::*;

/// The run's node0.
const RUN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../test-vectors/statedigest/regtest-v24/feature_llmq_chainlocks/node0"
);

/// The run's tip height.
const TIP_HEIGHT: u32 = 352;

/// The run's `chain_args` (`extract.json`, one argument a line).
fn chain_args() -> Vec<String> {
    let extract = read(&Path::new(RUN).join("extract.json"));
    let lines = extract.lines().skip_while(|l| l.trim() != r#""chain_args": ["#).skip(1);
    let args = lines.take_while(|l| !l.trim().starts_with(']'));
    let args: Vec<_> =
        args.map(|l| l.trim().trim_end_matches(',').trim_matches('"').to_owned()).collect();
    assert!(!args.is_empty(), "chain_args");
    args
}

/// The run's blocks after genesis, in hex, and the tip's hash in RPC hex. `blocks.dat` holds blk
/// records: the network's magic, the size, the block.
fn blocks() -> (Vec<String>, String) {
    let file = std::fs::File::open(Path::new(RUN).join("blocks.dat.zst")).unwrap();
    let raw = zstd::decode_all(file).unwrap();
    let (mut rest, mut blocks) = (raw.as_slice(), Vec::new());
    while let Some((record, tail)) = rest.split_at_checked(8) {
        let size = u32::from_le_bytes(record[4..].try_into().unwrap());
        let (block, tail) = tail.split_at(usize::try_from(size).unwrap());
        blocks.push(block);
        rest = tail;
    }
    assert_eq!(blocks.len(), TIP_HEIGHT as usize + 1, "genesis to the tip");
    let mut tip = *blocks.last().unwrap();
    let tip = dash_consensus::primitives::decode_block(&mut tip).expect("a block");
    let hex_blocks = blocks[1..].iter().map(|b| hex(b)).collect();
    (hex_blocks, tip.into_value().hash().to_string())
}

/// VerifyDB's readiness refusal over the tip (DEC-993): what it skips level 3 for, and what an
/// explicit check fails the load with.
fn evo_refusal(tip: &str) -> String {
    format!(
        "rdashd cannot validate block {tip} at height {TIP_HEIGHT} on regtest: VerifyDB's evo \
         checks (CH5-1b) for DIP0003 is not built (ALLOWED-DIFFS D18)"
    )
}

/// A log line's text: its time cut, and the ` (mocktime: N)` beside it.
fn unstamped(line: &str) -> &str {
    let text = line.split_once(' ').map_or(line, |(_, text)| text);
    text.strip_prefix("(mocktime: ").and_then(|t| t.split_once(") ")).map_or(text, |(_, t)| t)
}

#[test]
fn a_datadir_synced_past_v20_with_step_10_live_restarts_with_the_level_3_skip() {
    let (home, datadir) = home_and_datadir();
    let args = chain_args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (blocks, tip) = blocks();
    let best = format!(r#""{tip}""#);

    let checked = [&args[..], &["-assumevalid=0"]].concat();
    let node = RpcNode::start(home.path(), &datadir, &checked);
    for (h, block) in (1..).zip(&blocks) {
        let reply = node.call("submitblock", &format!(r#"["{block}"]"#));
        assert_eq!(reply.result(), "null", "block {h}");
    }
    assert_eq!(node.call("getbestblockhash", "[]").result(), best, "synced to the tip");
    node.stop();

    let before = read(&debug_log(&datadir)).len();
    let node = RpcNode::start(home.path(), &datadir, &args);
    assert_eq!(node.call("getbestblockhash", "[]").result(), best, "loaded at the tip");
    let log = read(&debug_log(&datadir));
    let started: Vec<_> = log[before..].lines().map(unstamped).collect();
    let verifying = started.iter().position(|l| l.starts_with("Verifying last"));
    let verifying = verifying.map(|i| started[i]);
    assert_eq!(verifying, Some("Verifying last 6 blocks at level 3"), "{started:#?}");
    let skip = format!("Skipped verification of level >=3: {}", evo_refusal(&tip));
    let refusals = log.lines().map(unstamped).filter(|l| l.contains("cannot validate"));
    let refusals: Vec<_> = refusals.collect();
    assert_eq!(refusals, [skip.as_str()], "the skip's, and no other");
    let aborts: Vec<_> = log.lines().filter(|l| unstamped(l).starts_with("*** ")).collect();
    assert!(aborts.is_empty(), "{aborts:#?}");
    node.stop();

    let control = [&args[..], &["-checklevel=4", "-printtoconsole=0"]].concat();
    let out = output(regtest(home.path(), &datadir, &control));
    let refused = (out.status.code(), stderr(&out));
    assert_eq!(refused, (Some(1), format!("Error: {}\n", evo_refusal(&tip))), "-checklevel=4");
}
