//! CH7b-3 C8: a datadir synced past V20 with step 10 live restarts, VerifyDB skipping level 3
//! with CH5-1a's marker. CH7b-2's C14 (`dashd-node`'s `recorded.rs`) restarts a datadir synced
//! under `-assumevalid`, where no block runs step 10; here every block of
//! `feature_llmq_chainlocks`'s run (dashd's node0, `test-vectors/statedigest/regtest-v24`) is
//! submitted under `-assumevalid=0`: the node's step 10 verifies each CbTx ChainLock, 44 from 309
//! (C7's census); under the stub, `submitblock` refuses 309 (W-1). Reopened with default flags,
//! the node loads at that tip. The startup check at level 3 skips with the readiness refusal
//! (DEC-993; `chain/verify.rs`, AD-CH3-4), and an explicit `-checklevel=4` is an init
//! error.
//!
//! This is not a level-4 verification of evo blocks: that is CH5-1b's (CH7b-3 erratum X9). The
//! census of the verifies is `dashd-node`'s (C7); this binary sees the process from outside. It
//! is a binary of its own so no other test shares its tracing (BL-479).
#![cfg(unix)]

mod common;

use common::recorded::recorded_run;
use common::*;

/// The run's tip height.
const TIP_HEIGHT: u32 = 352;

/// VerifyDB's readiness refusal over the tip (DEC-993): what it skips level 3 for, and what an
/// explicit check fails the load with.
fn evo_refusal(tip: &str) -> String {
    format!(
        "rdashd cannot validate block {tip} at height {TIP_HEIGHT} on regtest: VerifyDB's evo \
         checks (CH5-1b) for DIP0003 is not built (ALLOWED-DIFFS AD-CH3-4)"
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
    let run = recorded_run("feature_llmq_chainlocks");
    let args = run.args();
    assert!(!args.is_empty(), "chain_args");
    assert_eq!(run.tip(), TIP_HEIGHT as usize, "genesis to the tip");
    let (blocks, tip) = (&run.blocks, run.hash(run.tip()));
    let best = format!(r#""{tip}""#);

    let checked = [&args[..], &["-assumevalid=0"]].concat();
    let node = RpcNode::start(home.path(), &datadir, &checked);
    for (h, block) in (1..).zip(blocks) {
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
    let skip = format!("Skipped verification of level >=3: {}", evo_refusal(tip));
    let refusals = log.lines().map(unstamped).filter(|l| l.contains("cannot validate"));
    let refusals: Vec<_> = refusals.collect();
    assert_eq!(refusals, [skip.as_str()], "the skip's, and no other");
    let aborts: Vec<_> = log.lines().filter(|l| unstamped(l).starts_with("*** ")).collect();
    assert!(aborts.is_empty(), "{aborts:#?}");
    node.stop();

    let control = [&args[..], &["-checklevel=4", "-printtoconsole=0"]].concat();
    let out = output(regtest(home.path(), &datadir, &control));
    let refused = (out.status.code(), stderr(&out));
    assert_eq!(refused, (Some(1), format!("Error: {}\n", evo_refusal(tip))), "-checklevel=4");
}
