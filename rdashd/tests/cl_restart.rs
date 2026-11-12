//! CH7b-3 C8: a datadir synced past V20 with step 10 live restarts, VerifyDB walking its evo
//! blocks. CH7b-2's C14 (`dashd-node`'s `recorded.rs`) restarts a datadir synced
//! under `-assumevalid`, where no block runs step 10; here every block of
//! `feature_llmq_chainlocks`'s run (dashd's node0, `test-vectors/statedigest/regtest-v24`) is
//! submitted under `-assumevalid=0`: the node's step 10 verifies each CbTx ChainLock, 44 from 309
//! (C7's census); under the stub, `submitblock` refuses 309 (W-1). Reopened with default flags,
//! the node loads at that tip. Since CH5-1b (BL-581) the startup check at level 3 undoes the
//! window's evo blocks and passes with Core's summary, with no readiness refusal; an explicit
//! `-checklevel=4` also reconnects them and starts.
//!
//! CH5-1b's Leg C compares the same run's walks with dashd v24.0.0's: both nodes take blocks
//! 1-338 under `-assumevalid=0` and restart, the default window (333-338) holding the type-103
//! cycle base 336, past V20, MN_RR and the first CbTx ChainLock (C1); `verifychain` at (3,6),
//! (4,6), (3,0) and (4,0), the last reconnecting every commitment and the MNHF signal at 332 (C2);
//! after each depth-0 walk the tips and UTXO sets unchanged and equal, then both take 339-352,
//! walking at 345 and 352 as the pool's observations do, and agree on the tip (C3). The census
//! of the verifies is `dashd-node`'s (C7, and CH5-1b's C4); this binary sees the process from
//! outside. It is a binary of its own so no other test shares its tracing (BL-479). The
//! comparison cannot be skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::path::PathBuf;

use common::recorded::{RecordedRun, recorded_run};
use common::verifydb::*;
use common::*;

/// The run's tip height.
const TIP_HEIGHT: u32 = 352;

#[test]
fn a_datadir_synced_past_v20_with_step_10_live_restarts_verifying_its_evo_blocks() {
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

    for (more, level) in [(&[][..], 3), (&["-checklevel=4"][..], 4)] {
        let before = read(&debug_log(&datadir)).len();
        let node = RpcNode::start(home.path(), &datadir, &[&args[..], more].concat());
        assert_eq!(node.call("getbestblockhash", "[]").result(), best, "loaded at the tip");
        let log = read(&debug_log(&datadir));
        let started: Vec<_> = log[before..].lines().map(unstamped).collect();
        let verifying = started.iter().position(|l| l.starts_with("Verifying last"));
        let verifying = verifying.map(|i| started[i]);
        let want = format!("Verifying last 6 blocks at level {level}");
        assert_eq!(verifying, Some(want.as_str()), "{started:#?}");
        let summary = "Verification: No coin database inconsistencies in last 6 blocks (";
        assert!(started.iter().any(|l| l.starts_with(summary)), "{level}: {started:#?}");
        let refused = started.iter().filter(|l| l.contains("cannot validate")).count();
        assert_eq!(refused, 0, "{level}: no readiness refusal: {started:#?}");
        let skip = "Skipped verification of level >=3";
        assert!(!started.iter().any(|l| l.starts_with(skip)), "{level}: {started:#?}");
        let aborts: Vec<_> = started.iter().filter(|l| l.starts_with("*** ")).collect();
        assert!(aborts.is_empty(), "{level}: {aborts:#?}");
        node.stop();
    }
}

/// Leg C's restart tip (OD-CP-270): its default window, 333-338, holds the cycle base 336.
const RESTART_AT: usize = 338;
/// The pool's observation tips (CH5-1b R3-1): 345 inside cycle 336's mining window, nothing
/// mined; 352 after 347 and 348 mined theirs. The walks there are the probe's.
const OBSERVED: [usize; 2] = [345, 352];
const OBSERVED_CELLS: [(i32, i32); 3] = [(3, 0), (3, 8), (4, 0)];

/// One node of Leg C's pair.
struct Side {
    binary: PathBuf,
    home: tempfile::TempDir,
    datadir: PathBuf,
    args: Vec<String>,
    rpc: Option<RpcNode>,
}

impl Side {
    fn new(binary: PathBuf, args: Vec<String>) -> Side {
        let (home, datadir) = home_and_datadir();
        Side {
            binary,
            home,
            datadir,
            args,
            rpc: None,
        }
    }

    /// A start: VerifyDB's window in the log it writes.
    fn start(&mut self) -> Vec<String> {
        let before = std::fs::metadata(debug_log(&self.datadir)).map_or(0, |m| m.len() as usize);
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        self.rpc = Some(RpcNode::start_on(&self.binary, self.home.path(), &self.datadir, &args));
        verifydb_lines(&read(&debug_log(&self.datadir))[before..])
    }

    fn rpc(&self) -> &RpcNode {
        self.rpc.as_ref().expect("running")
    }

    fn stop(&mut self) {
        let rpc = self.rpc.take().expect("running");
        assert!(rpc.call("stop", "[]").result().contains("stopping"));
        let out = rpc.node.wait();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    }

    /// Blocks `heights` of `run`, each accepted.
    fn submit(&self, run: &RecordedRun, heights: std::ops::RangeInclusive<usize>) {
        for h in heights {
            let reply = self.rpc().call("submitblock", &format!(r#"["{}"]"#, run.blocks[h - 1]));
            assert_eq!(reply.result(), "null", "{}: block {h}", self.binary.display());
        }
    }
}

/// CH5-1b (BL-581) Leg C, C1-C3: rdashd's startup check and `verifychain` over the chainlocked
/// window and the whole run against dashd's, with the tips, UTXO sets and next blocks after.
// Core: validation.cpp:5042-5189; node/chainstate.cpp:441-465 at v24.0.0
#[test]
fn verifydb_over_the_chainlocked_run_matches_dashd() {
    let run = recorded_run("feature_llmq_chainlocks");
    assert_eq!(run.tip(), TIP_HEIGHT as usize, "genesis to the tip");
    let base = run.args().into_iter().chain(["-assumevalid=0", "-dnsseed=0", "-fixedseeds=0"]);
    let args: Vec<String> = base.map(str::to_owned).collect();
    let dashd = [&args[..], &["-disablewallet".to_owned()]].concat();
    let mut nodes = [Side::new(PathBuf::from(RDASHD), args), Side::new(require_dashd(), dashd)];
    for n in &mut nodes {
        n.start();
        n.submit(&run, 1..=RESTART_AT);
        n.stop();
    }

    // C1: the default restart's walk, 338 down to 333.
    let mut exclusions = Exclusions::new(&[
        &WROTE_SNAPSHOT,
        &APPLY_TX_TO_LIST,
        &MNHF_EXCLUDED,
        &MNHF_ADDED,
        &MNHF_VALID,
    ]);
    let [ours, core] = nodes.each_mut().map(Side::start);
    let [ours, core] = exclusions.cell("the restart at 338", &ours, &core);
    assert_eq!(ours, core, "the restart at 338");
    let summary = "Verification: No coin database inconsistencies in last 6 blocks (";
    assert!(ours.iter().any(|l| l.starts_with(summary)), "{ours:#?}");
    let tip = format!(r#""{}""#, run.hash(RESTART_AT));
    for n in &nodes {
        assert_eq!(n.rpc().call("getbestblockhash", "[]").result(), tip, "loaded at 338");
    }

    // C2, C3.
    walks(nodes.each_ref().map(Side::rpc), "338", &EVO_CELLS, &mut exclusions);
    let mut at = RESTART_AT;
    for tip in OBSERVED {
        for n in &nodes {
            n.submit(&run, at + 1..=tip);
        }
        walks(nodes.each_ref().map(Side::rpc), &tip.to_string(), &OBSERVED_CELLS, &mut exclusions);
        at = tip;
    }
    let best = format!(r#""{}""#, run.hash(TIP_HEIGHT as usize));
    for n in &nodes {
        assert_eq!(n.rpc().call("getbestblockhash", "[]").result(), best, "the run's tip");
    }
    exclusions.assert_matched();
    for n in &mut nodes {
        n.stop();
    }
}
