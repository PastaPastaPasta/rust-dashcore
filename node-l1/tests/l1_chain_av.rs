//! O2b-4 4d: S2, `diff_chain_av`'s schedule (`node_l1::chain::av`), on Core and rdashd's `NodeSut`.
//!
//! The image is S1's with 8,100 headers above its tip and `-assumevalid` on the 30th. Core skips a
//! block's scripts when it is an ancestor of the assumevalid block and of the best header, more
//! than two weeks of proof-equivalent time below it (validation.cpp:2553-2579 at v24.0.0), so the
//! line's three script defects (heights 205, 215, 225) connect, and the same defect above the
//! assumevalid block, off its chain, or once a failure has reset the best header to the active
//! tip, is refused; both sides alike. The control runs the same
//! image with `-assumevalid=0`, where the defect is refused: the acceptance is the option's doing.
//!
//! Each witness is scored: the input ends done (every plane the same on both sides), with no tie
//! and no op the node does not serve, at the oracle's ABI 36 or later, which is checked, never
//! skipped.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/chain_sides.rs"]
mod chain_sides;

use std::collections::BTreeMap;
use std::path::Path;

use chain_sides::{assert_planes, assert_posts, assert_same, both, private_dir, sides};
use dash_oracle_sys::{ChainOp, ChainStart, Oracle, Pin, SporkState};
use dash_types::Numeric as _;
use node_l1::chain::av::{self, LINE, Line, TWO_WEEKS};
use node_l1::chain::core::{IMAGE_HEIGHT, SPORK_19, Schedule, T_RECENT, image_op};
use node_l1::chain::intent::{Encoding, Intent, Program, target_seeds};
use node_l1::chain::node::NodeSide;
use node_l1::chain::run::{End, Refused, Run, Unsupported};
use node_l1::chain::step::Step;
use node_l1::chain::world::World;
use node_l1::chain::{Mode, Runner};
use serde_json::Value;

/// The witness rule's ABI check: S2's records and teardown events need the shim at ABI 36.
fn abi_36() {
    let abi = dash_oracle_sys::ffi::ABI_VERSION;
    assert!(abi >= 36, "S2's witnesses need the oracle at ABI 36 or later; it declares {abi}");
}

fn line(w: &World) -> Line {
    w.line.expect("S2's world has its line")
}

/// The input ended done, every op the same on both sides, a queue drain both showed at most
/// passed (D49); never a tie, a divergence, an unexpected reply or an op the node refused.
fn scored(run: &Run, what: &str) {
    assert_eq!(run.end, End::Done, "{what}: after {:?}", run.kinds);
}

fn row(run: &Run, name: &str) -> u64 {
    run.census.rows.get(name).copied().unwrap_or(0)
}

fn rejects(run: &Run, reason: &str) -> u64 {
    run.census.rejects.get(reason).copied().unwrap_or(0)
}

/// Core's proof-equivalent time of the best header (the line's top) over line block `id`: every
/// header's work from its bits (`GetBlockProofEquivalentTime`), as `l3::leg::av` reads it.
fn proof_time(w: &World, id: usize) -> i64 {
    use dash_consensus::HeaderCtx;
    use dash_consensus::pow::{PowParams, block_proof, block_proof_equivalent_time};
    let params = node_l1::chain::params::node_params(&Schedule::S2.chain_args()).unwrap();
    let pow = PowParams::from_chain_params(&params).unwrap();
    let top = line(w).last();
    let ctx = |i: usize, chain_work| {
        let head = w.blocks[i].built.as_ref().unwrap().header;
        let word = |at: usize| u32::from_le_bytes(head[at..at + 4].try_into().unwrap());
        HeaderCtx {
            height: w.blocks[i].height,
            hash: dash_primitives::BlockHash::from(w.blocks[i].hash),
            time: word(68),
            bits: dash_num::CompactTarget::new(word(72)),
            version: word(0) as i32,
            mtp: 0,
            chain_work,
        }
    };
    let mut work = dash_num::Arith256::ZERO;
    let from = ctx(id, work);
    for i in id + 1..=top {
        work = work + block_proof(ctx(i, work).bits);
    }
    block_proof_equivalent_time(&ctx(top, work), &from, &ctx(top, work), &pow)
}

/// After a restore both sides hold the same S2 image: the active chain at the image tip, the
/// line's top the best header, one headers-only tip, out of IBD; every post field the same, the
/// index digest over all 8,301 entries included. The line is past two weeks of Core's
/// proof-equivalent time exactly up to height 235.
#[test]
fn the_s2_image_is_the_same_on_both_sides() {
    abi_36();
    let w = Schedule::S2.world();
    let l = line(&w);
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S2, &dir);
    let (c, n, entries) = both(&mut core, &mut node, &ChainOp::SetTime(T_RECENT));
    assert_posts(&c.post, &n.post, &entries, "S2");
    assert_eq!(entries.len(), IMAGE_HEIGHT as usize + 1 + LINE as usize);
    let at = |id: usize| (w.blocks[id].height as i32, w.blocks[id].hash);
    let tip = c.post.tip.as_ref().expect("a tip");
    assert_eq!((tip.0, tip.1.0), at(w.image_tip));
    let best = c.post.best_header.flatten().expect("a best header");
    assert_eq!((best.0, best.1.0), at(l.last()));
    assert_eq!(c.post.ibd, Some(false));
    let tips = c.post.tips.as_ref().expect("tips");
    let status = |s: &str| tips.iter().filter(|t| t["status"] == s).count();
    assert_eq!((tips.len(), status("active"), status("headers-only")), (2, 1, 1), "{tips:?}");
    let first = l.first;
    assert!(proof_time(&w, first + 34) > TWO_WEEKS, "height 235");
    assert!(proof_time(&w, first + 35) <= TWO_WEEKS, "height 236");
    assert!(proof_time(&w, l.av) > TWO_WEEKS);
}

/// The line's first 25 blocks connect one by one on both sides, each op the same: the three
/// script defects among them are accepted (Core skips their scripts below the assumevalid block).
#[test]
fn a_skipped_script_block_connects_below_the_av_block_on_both_sides() {
    abi_36();
    let w = Schedule::S2.world();
    let l = line(&w);
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S2, &dir);
    for id in l.first..l.first + 25 {
        let c = assert_same(&mut core, &mut node, &ChainOp::Block(w.blocks[id].built_bytes()));
        assert_eq!(c.reply.outcome, Ok(Value::Null), "height {}", w.blocks[id].height);
        let tip = c.post.tip.as_ref().expect("a tip");
        assert_eq!(tip.1.0, w.blocks[id].hash, "connected at height {}", w.blocks[id].height);
        assert!(
            l.defects.contains(&id) == av::DEFECT_OFFSETS.contains(&((id - l.first + 1) as u32))
        );
    }
}

/// Past the assumevalid block, a script failure on a sibling of the line's block at 232 is checked
/// and refused on both sides (`-par=4`'s `block-validation-failed`), and the line goes on.
#[test]
fn the_same_defect_above_the_av_block_is_refused_on_both_sides() {
    abi_36();
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S2, Mode::Diff).unwrap();
    let text = include_str!("chain_seeds/diff_chain_av/av_check.prog");
    let run = runner.run(&Program::parse(text).unwrap()).unwrap();
    scored(&run, "av_check");
    assert!(run.partial.is_empty(), "{:?}", run.partial);
    assert_eq!(row(&run, "av_checked_above"), 1);
    assert_eq!(row(&run, "av_skipped_below"), 3);
    assert_eq!(rejects(&run, "block-validation-failed"), 1, "{:?}", run.census.rejects);
    assert_eq!(run.census.ops.get(&("block", "ok")).copied(), Some(31 + 1));
}

/// Below the assumevalid block but off its chain, the same script failure is checked: a fork's
/// block at 203 is refused on both sides.
#[test]
fn a_defect_off_the_av_chain_below_it_is_refused_on_both_sides() {
    abi_36();
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S2, Mode::Diff).unwrap();
    let text = include_str!("chain_seeds/diff_chain_av/av_fork.prog");
    let run = runner.run(&Program::parse(text).unwrap()).unwrap();
    scored(&run, "av_fork");
    assert_eq!(rejects(&run, "block-validation-failed"), 1, "{:?}", run.census.rejects);
    assert_eq!(row(&run, "av_checked_above"), 0, "below the assumevalid block");
    assert_eq!(row(&run, "av_skipped_below"), 1, "the line's 205");
}

/// An INVALIDATE of a line header resets Core's best header to the active tip (204), so the line's
/// first defect, delivered after it, is no longer an ancestor of the best header: its scripts are
/// checked and it is refused on both sides; the model expected it (no unexpected reply).
#[test]
fn a_defect_is_checked_once_the_best_header_leaves_the_line() {
    abi_36();
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S2, Mode::Diff).unwrap();
    let text = include_str!("chain_seeds/diff_chain_av/av_reset.prog");
    let run = runner.run(&Program::parse(text).unwrap()).unwrap();
    scored(&run, "av_reset");
    assert!(run.partial.is_empty(), "{:?}", run.partial);
    assert_eq!(rejects(&run, "block-validation-failed"), 1, "{:?}", run.census.rejects);
    assert_eq!(row(&run, "av_skipped_below"), 0, "the defect is checked");
    assert_eq!(run.census.ops.get(&("block", "ok")).copied(), Some(4));
}

/// DEC-1368: with the line connected through 206 (its first defect skipped) and the best header
/// reset to the tip, a fork from 202 with more work and an operator on its first block is not
/// sent (`av_reconnect`): Core would have to reconnect the defect, now checked, when the fork
/// fails. The input ends done, and a clean extension after it still runs.
#[test]
fn a_reorg_that_would_reconnect_a_checked_defect_is_not_sent() {
    abi_36();
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S2, Mode::Diff).unwrap();
    let text = "extend main 6\nextend main 2 headers hold\ninvalidate tip#0\nmutate script_false\n\
                fork 4 6\nextend main 1\n";
    let run = runner.run(&Program::parse(text).unwrap()).unwrap();
    scored(&run, "av_reconnect");
    assert_eq!(run.census.avoided.get("av_reconnect").copied(), Some(1), "{:?}", run.census);
    assert_eq!(row(&run, "av_skipped_below"), 1, "the line's 205");
    assert_eq!(run.census.ops.get(&("block", "ok")).copied(), Some(7), "201..206, then 207");
}

/// The control: the same S2 image with `-assumevalid=0` on both sides. The line's first defect
/// is refused on both, the same; so the acceptance above is `-assumevalid`'s.
#[test]
fn the_line_defect_is_refused_without_assumevalid() {
    abi_36();
    let w = Schedule::S2.world();
    let l = line(&w);
    let mut args: Vec<&str> = Schedule::S2.args();
    let av = args.iter().position(|a| a.starts_with("-assumevalid=")).expect("S2's option");
    args[av] = "-assumevalid=0";
    let dir = private_dir();
    let owned: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let mut node = NodeSide::with_argv(&owned, T_RECENT, dir.path(), None).unwrap();
    assert_eq!(node.down(), None);
    let mut core = match Oracle::new(Pin::V24_0).chain_session(&args, T_RECENT, None).unwrap() {
        ChainStart::Started(s) => s,
        ChainStart::Refused(m) => panic!("refused: {m}"),
    };
    let spork = ChainOp::Spork {
        id: SPORK_19,
        value: 0,
    };
    let image = w.blocks[1..].iter().map(image_op).chain(std::iter::once(spork));
    for op in image {
        let c: Step = core.step(&op).unwrap().into();
        let n = node.step(&op).unwrap_or_else(|u| panic!("{u:?}")).step;
        assert_eq!(c.reply, n.reply, "{}", node_l1::chain::run::op_kind(&op));
        if matches!(op, ChainOp::Spork { .. }) {
            assert!(
                c.post.sporks.as_ref().is_some_and(|s| s.contains(&SporkState(SPORK_19, 0, true)))
            );
        } else {
            assert_eq!(c.reply.outcome, Ok(Value::Null));
        }
    }
    for id in l.first..l.defects[0] {
        let op = ChainOp::Block(w.blocks[id].built_bytes());
        let c: Step = core.step(&op).unwrap().into();
        let n = node.step(&op).unwrap_or_else(|u| panic!("{u:?}")).step;
        assert_planes(&c, &n, &format!("height {}", w.blocks[id].height));
    }
    let op = ChainOp::Block(w.blocks[l.defects[0]].built_bytes());
    let c: Step = core.step(&op).unwrap().into();
    let n = node.step(&op).unwrap_or_else(|u| panic!("{u:?}")).step;
    assert_planes(&c, &n, "the defect");
    assert_eq!(
        c.reply.outcome,
        Ok(Value::String("block-validation-failed".into())),
        "the defect, checked"
    );
    let tip = c.post.tip.as_ref().expect("a tip");
    assert_eq!(tip.0, w.blocks[l.defects[0]].height as i32 - 1);
}

/// `n` inputs from a fixed xorshift stream, 8 to 207 bytes each (`l1_chain_diff`'s).
fn generated(n: usize) -> Vec<(String, Vec<u8>)> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    (0..n)
        .map(|i| {
            let len = 8 + usize::try_from(next() % 200).unwrap();
            (format!("gen{i:02}"), (0..len).map(|_| next().to_le_bytes()[0]).collect())
        })
        .collect()
}

/// `diff_chain_av`'s public seeds (the shared programs and its own) and 50 generated inputs, all
/// `chain-v2`, in Diff mode on S2: each ends done, or done past a queue drain both sides showed
/// (D49), or tie-discarded (D30). A divergence, an unexpected reply or an op the node does not
/// serve fails. The corpus reaches S2's rows: a defect skipped below the assumevalid block, a
/// script failure checked above it, a `-par=4` multi-failure (W-P3a) and testdummy's activation
/// (the line signals). The aggregate is pinned, so that no input's end passes unseen: a change
/// to the corpus or to S2's image must update it.
#[test]
fn s2_seeds_run_clean() {
    abi_36();
    let progs = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chain_seeds");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../node-fuzz/public-seeds/diff_chain_av");
    let mut inputs = target_seeds(&progs, "diff_chain_av", Encoding::V2).unwrap();
    assert_eq!(inputs.len(), 27, "15 shared programs, chain-v2's 8 and 4 of its own");
    for (name, bytes) in &inputs {
        assert_eq!(&std::fs::read(dir.join(name)).unwrap(), bytes, "{name}: the public seed");
    }
    inputs.extend(generated(50));
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S2, Mode::Diff).unwrap();
    let mut ends: BTreeMap<&str, usize> = BTreeMap::new();
    let mut rows: BTreeMap<&str, u64> = BTreeMap::new();
    let mut ties = Vec::new();
    // G-T1 on S2 (a question for L3): inputs with a RESTART intent, with one run, and its drops.
    let (mut with_restart, mut restarted, mut restart_tie) = (0, 0, 0);
    for (name, bytes) in &inputs {
        let program = Program::decode(bytes, Encoding::V2);
        let run = runner.run(&program).unwrap();
        with_restart +=
            usize::from(program.intents.iter().any(|i| matches!(i, Intent::Restart(_))));
        restarted += usize::from(run.census.ops.keys().any(|(k, _)| *k == "restart"));
        restart_tie += run.census.avoided.get("restart_tie").copied().unwrap_or(0);
        let end = match &run.end {
            End::Done if run.partial.is_empty() => "done",
            End::Done => "done past a queue drain",
            End::Tainted(op, sites) => {
                ties.push(format!("{name}: {op} {sites:?}"));
                "tie"
            }
            End::Unsupported(Unsupported {
                why: Refused::QueueDrain,
                ..
            }) => "queue drain",
            other => panic!("{name}: {other:?} after {:?}\n{program}", run.kinds),
        };
        println!(
            "S2 input {name}: {end}, rows {:?}, avoided {:?}",
            run.census.rows, run.census.avoided
        );
        *ends.entry(end).or_default() += 1;
        for (r, n) in &run.census.rows {
            *rows.entry(r).or_default() += n;
        }
    }
    println!("S2: {ends:?}, ties {ties:?}, rows {rows:?}");
    println!(
        "S2 restarts: {with_restart} of {} inputs hold a RESTART intent, {restarted} ran one, \
         {restart_tie} dropped as restart_tie",
        inputs.len()
    );
    for r in ["av_skipped_below", "av_checked_above", "multi_failure_s1", "testdummy_active"] {
        assert!(rows.get(r).copied().unwrap_or(0) >= 1, "{r}: {rows:?}");
    }
    // Nine run on past a queue drain both sides showed (D49); none is tie-discarded. `chain-v2`'s
    // eight spork seeds (4f2) each end done; every other input's end is as before them.
    let pinned = BTreeMap::from([("done", 68), ("done past a queue drain", 9)]);
    assert_eq!(ends, pinned, "the corpus's ends; ties {ties:?}");
}
