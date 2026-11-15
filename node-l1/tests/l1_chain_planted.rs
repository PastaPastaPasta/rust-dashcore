//! O2b-4 4b: the planted searches' witnesses. Each planted chain target's node arms its bug
//! (P3a, P3b) once the session's image exists, and its witness program, which the unplanted
//! targets run `Done` (`l1_chain_diff`'s `w_p3a_unplanted`, `w_p3b_unplanted`), ends at the
//! bug's divergence. The witnesses hold the rule every directed witness does
//! (`support/witness.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/chain_sides.rs"]
mod chain_sides;
#[path = "support/witness.rs"]
mod witness;

use chain_sides::{node_ran, private_dir};
use dash_oracle_sys::{ChainOp, Pin};
use node_l1::chain::census::Census;
use node_l1::chain::core::{CoreSut, Schedule, T_RECENT, image_world};
use node_l1::chain::generate::Gen;
use node_l1::chain::node::NodeSide;
use node_l1::chain::run::{ChainPlant, DIFF_CHAIN_P3B, DIFF_CHAIN_PAR_P3A, End, fuzz_target};
use node_l1::{Divergence, Outcome};

/// W-P3a on `diff_chain_par_p3a`: the planted node reports the check queue's
/// `block-validation-failed` for the block Core rejects `bad-txns-nonfinal`, so the input ends
/// diverged on the reply at that block step.
#[test]
fn w_p3a_planted() {
    let t = DIFF_CHAIN_PAR_P3A;
    let run = witness::witness(t.schedule, t.plant, include_str!("chain_seeds/w_p3a.prog"));
    assert_eq!(
        run.end,
        End::Diverged(Divergence {
            field: "reply"
        })
    );
    let at = run.raws.len() - 1;
    assert_eq!(run.kinds[at], "block", "the divergence is at the block step");
    let core = witness::reply_text(&witness::core_report(&run, at).reply.outcome);
    assert!(core.starts_with("bad-txns-nonfinal"), "Core: {core}");

    let dir = private_dir();
    let (mut core, mut node) = planted_sides(t.schedule, t.plant, &dir);
    let ops = core_ops(&mut core, include_str!("chain_seeds/w_p3a.prog"));
    let block = ops.iter().map(|op| node_ran(&mut node, op)).nth(at - 1).expect("the block");
    let reply = witness::reply_text(&block.step.reply.outcome);
    assert_eq!(reply, "block-validation-failed", "the node: {reply}");
}

/// W-P3b on `diff_chain_p3b`: the planted node's invalidate leaves the two walked descendants
/// out of the dirty set, so the killed restart finds one stored `FAILED_VALID` record where
/// Core's has three, and the input ends diverged on `post.persisted` at the restart. The first
/// flush still empties V's dirty set (D42).
#[test]
fn w_p3b_planted() {
    let t = DIFF_CHAIN_P3B;
    let text = include_str!("chain_seeds/w_p3b.prog");
    let run = witness::witness(t.schedule, t.plant, text);
    assert_eq!(
        run.end,
        End::Diverged(Divergence {
            field: "post.persisted"
        })
    );
    let at = run.raws.len() - 1;
    assert_eq!(run.kinds[at], "restart", "the divergence is at the restart");
    let core = witness::core_report(&run, at).post.persisted.expect("a restart");
    assert_eq!(witness::stored_failed(&core.failed), 3, "Core's stored records");

    let dir = private_dir();
    let (mut core, mut node) = planted_sides(t.schedule, t.plant, &dir);
    let mut flushed = false;
    let mut last = None;
    for op in core_ops(&mut core, text) {
        last = Some(node_ran(&mut node, &op).step);
        if !flushed && matches!(op, ChainOp::Flush { .. }) {
            flushed = true;
            assert_eq!(node.dirty_index_len(), Some(0), "the first flush wrote V's dirty set");
        }
    }
    let persisted = last.unwrap().post.persisted.expect("a restart");
    assert_eq!(witness::stored_failed(&persisted.failed), 1, "the node's stored records");
}

/// A planted session is built as an unplanted one: its image is the unplanted node's (the same
/// stored state on restore), its first input's step 0 is the same as Core's, and each planted
/// witness runs the same as Core up to its bug's op.
#[test]
fn a_planted_target_is_armed_only_after_its_image() {
    for t in [DIFF_CHAIN_PAR_P3A, DIFF_CHAIN_P3B] {
        let (plain_dir, planted_dir) = (private_dir(), private_dir());
        let (mut plain, _) = NodeSide::start(Pin::V24_0, t.schedule, plain_dir.path()).unwrap();
        let (mut planted, _) = NodeSide::start(Pin::V24_0, t.schedule, planted_dir.path()).unwrap();
        planted.with_plant(t.plant.planted().expect("a planted target"));
        let (a, b) = (plain.restore(T_RECENT).unwrap(), planted.restore(T_RECENT).unwrap());
        assert_eq!(a.step.post.persisted, b.step.post.persisted, "{}: the image", t.name);
        assert_eq!(a.step.reply, b.step.reply, "{}: step 0", t.name);
    }
    let text = include_str!("chain_seeds/w_p3b.prog");
    let run = witness::witness(Schedule::S0, ChainPlant::P3b, text);
    let unplanted = witness::witness(Schedule::S0, ChainPlant::None, text);
    assert_eq!(run.raws.len(), unplanted.raws.len(), "the divergence is at the last op");
    let a = include_str!("chain_seeds/w_p3a.prog");
    let p3a = witness::witness(Schedule::S1, ChainPlant::P3a, a);
    let unplanted = witness::witness(Schedule::S1, ChainPlant::None, a);
    let block = witness::rejected_block(&unplanted, "bad-txns-nonfinal").expect("the block");
    assert_eq!(p3a.raws.len() - 1, block, "the divergence is at P3a's block");
}

/// Both sides on `schedule`, the node with `plant` armed: [`chain_sides::sides`], then a
/// restore of each, which arms it.
fn planted_sides(
    schedule: Schedule,
    plant: ChainPlant,
    dir: &tempfile::TempDir,
) -> (CoreSut, NodeSide) {
    let (mut core, mut node) = chain_sides::sides(schedule, dir);
    node.with_plant(plant.planted().expect("a planted target"));
    core.restore(T_RECENT).unwrap();
    node.restore(T_RECENT).unwrap();
    (core, node)
}

/// The ops Core runs for `text` from the image, in order.
fn core_ops(core: &mut CoreSut, text: &str) -> Vec<ChainOp> {
    let program = witness::program(text);
    let mut w = image_world();
    w.loaded();
    let mut generator = Gen::default();
    let mut census = Census::default();
    let mut ops = Vec::new();
    for intent in &program.intents {
        for p in generator.resolve(&mut w, intent, &mut census) {
            let step = chain_sides::core_step(core, &p.op);
            w.observe(&p.op, &step);
            ops.push(p.op);
        }
    }
    ops
}

/// Each planted search's public seeds, those left once its witnesses are excluded, don't reveal
/// its plant (review r1 m3): each one, run through the fuzz target itself on its planted target,
/// ends `Done` or out of domain, never diverged and never tie-discarded, so the search has the
/// plant still to find.
#[test]
fn planted_public_seeds_do_not_reveal_the_plant() {
    let started = std::time::Instant::now();
    for t in [DIFF_CHAIN_PAR_P3A, DIFF_CHAIN_P3B] {
        let dir = format!("{}/../node-fuzz/public-seeds/{}", env!("CARGO_MANIFEST_DIR"), t.name);
        let mut files: Vec<_> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        assert_eq!(
            files.len(),
            104,
            "{}: the 15 shared seeds less the two witnesses, and chain-v2's 92 (4e1's 84, 4f2's 8) \
             less the one that reaches P3a",
            t.name
        );
        for f in &files {
            let got = fuzz_target(&std::fs::read(f).unwrap(), Pin::V24_0, t);
            assert!(
                matches!(got, Ok(Outcome::BothAccept | Outcome::OutOfDomain)),
                "{}: {}: {got:?}",
                t.name,
                f.display()
            );
        }
    }
    eprintln!("planted public seeds: {:.1} s", started.elapsed().as_secs_f64());
}
