//! O2b-4 4c: leg B's witnesses on `diff_chain_prodflush`'s S0p (S0 at `-dbcache=4` on both
//! sides, the node in its Production flush regime, Core's size flushes off), run through the
//! Diff runner as the fuzz target runs an input. Each names the W schedule it runs under
//! (OD-L3-118): `drain` unless the program starts `start hold`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::VecDeque;

use dash_oracle_sys::{ChainOp, Pin, RestartKind};
use dashd_chainstate::chain::FlushArm;
use dashd_chainstate::flush::FlushCauses;
use dashd_node::sut::OpClass;
use node_l1::chain::census::Census;
use node_l1::chain::core::{CoreSut, Schedule, T_RECENT};
use node_l1::chain::generate::{Gen, due_enforce};
use node_l1::chain::intent::{Encoding, Program};
use node_l1::chain::legb::{self, LegBEnd};
use node_l1::chain::run::{End, LegBStep, Plant, Run, replays};
use node_l1::chain::{Mode, Runner};

/// The witness rule's ABI check: leg B's mirrors need the shim's labelled FLUSH and its
/// `-oracle-no-size-flush`, both at ABI 36.
const _: () = assert!(dash_oracle_sys::ffi::ABI_VERSION >= 36);

/// 64 fan-out blocks, one input's whole block budget.
const FAN_64: &str = "extend main 8 tx=fan\nextend main 8 tx=fan\nextend main 8 tx=fan\n\
                      extend main 8 tx=fan\nextend main 8 tx=fan\nextend main 8 tx=fan\n\
                      extend main 8 tx=fan\nextend main 8 tx=fan\n";

fn runner() -> Runner {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0p, Mode::Diff).expect("S0p");
    runner.record_legb(true);
    runner
}

/// `text` from the image, with `plant` if any: the m3 rule (done, no partial, nothing refused
/// or tainted) holds.
fn run(runner: &mut Runner, text: &str, plant: Option<Plant>) -> Run {
    runner.plant(plant);
    let run = runner.run(&Program::parse(text).expect("parses")).expect("a run");
    runner.plant(None);
    assert!(matches!(run.end, End::Done), "{text}: {:?}", run.end);
    assert!(run.partial.is_empty(), "{text}: partial {:?}", run.partial);
    run
}

/// Leg B's coins salt for `text` as the runner derives it (OD-L3-119): the first 8 bytes of the
/// SHA-256 of its `chain-v2` encoding, the W schedule bit clear.
fn salt(text: &str) -> u64 {
    let program = Program {
        w_hold: false,
        ..Program::parse(text).expect("parses")
    };
    let digest = bitcoin_hashes::sha256::Hash::hash(&program.encode(Encoding::V2));
    u64::from_le_bytes(digest.to_byte_array()[..8].try_into().expect("8 bytes"))
}

/// The leg-B step of report `at`.
fn step(run: &Run, at: usize) -> &LegBStep {
    run.legb.iter().find(|l| l.at == at).unwrap_or_else(|| panic!("no leg-B step at {at}"))
}

/// Every journal entry of the run, with the report it ended.
fn entries(run: &Run) -> Vec<(usize, &str, OpClass)> {
    run.legb
        .iter()
        .flat_map(|l| l.end.entries.iter().map(move |(j, c)| (l.at, j.cause.as_str(), *c)))
        .collect()
}

/// The raw kinds right after report `at`: its mirrors and reconcile.
fn aux_after(run: &Run, at: usize) -> Vec<&'static str> {
    run.kinds[at + 1..].iter().take_while(|k| k.starts_with("legb-")).copied().collect()
}

/// The `n`th block op's report (1-based).
fn nth_block(run: &Run, n: usize) -> usize {
    run.kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| **k == "block")
        .nth(n - 1)
        .map(|(at, _)| at)
        .expect("enough blocks")
}

/// A reduced step's posts agree on what the sides persisted and on their flush counts.
fn assert_persisted_equal(l: &LegBStep, what: &str) {
    assert!(l.core.post.persisted.is_some(), "{what}: Core persisted");
    assert_eq!(l.core.post.persisted, l.node.post.persisted, "{what}: post.persisted");
    assert_eq!(l.core.post.flush_delta, l.node.post.flush_delta, "{what}: post.flush_delta");
}

/// W-B1 (drain): THRESHOLD at the 64th fan-out block's end is mirrored to Core under the node's
/// label, then one reconcile on each side, as its take kept Core's dirt (BL-813, DEC-1447); the
/// explicit `flush always` after it is matched, not mirrored; the KILL after the mirrors
/// persists the same on both sides. The input is picked for its salt: the 64-block shape
/// reaches THRESHOLD only under some routings (OD-L3-119 item 4).
#[test]
fn w_b1_op_end_mirror() {
    let text = format!("{FAN_64}flush always\ntime 2\nrestart kill\n");
    assert_eq!(salt(&text), 0xf784_0ee2_7187_7d9f, "the input W-B1 was picked for");
    let run = run(&mut runner(), &text, None);
    let threshold: Vec<_> = entries(&run).into_iter().filter(|e| e.1 == "THRESHOLD").collect();
    let at = nth_block(&run, 64);
    assert_eq!(threshold, [(at, "THRESHOLD", OpClass::OpEnd)], "one, at the 64th block's end");
    let l = step(&run, at);
    let labels: Vec<_> = l.plan.mirrors.iter().map(|m| m.label.as_str()).collect();
    assert!(labels.contains(&"THRESHOLD"), "mirrored under the node's label: {labels:?}");
    assert!(l.plan.mirrors.iter().all(|m| m.class == OpClass::OpEnd && !m.labelled));
    let kept = l.plan.mirrors.iter().find(|m| m.label == "THRESHOLD").is_some_and(|m| m.kept);
    assert!(kept, "THRESHOLD's take kept Core's dirt");
    let mut aux = vec!["legb-mirror"; labels.len()];
    aux.push("legb-reconcile");
    assert_eq!(aux_after(&run, at), aux, "the mirrors, then one reconcile");
    assert_eq!(l.delta.full.get("reconcile"), Some(&1), "the node's reconcile booking");
    assert_eq!(l.core.post.flush_delta.full.get("THRESHOLD"), Some(&1), "Core books the label");
    assert_eq!(l.core.post.flush_delta, l.node.post.flush_delta);
    let flush = run.kinds.iter().position(|k| *k == "flush").expect("the flush op");
    let f = step(&run, flush);
    assert_eq!(f.end.entries.len(), 1, "the explicit flush books once");
    assert!(f.plan.mirrors.is_empty() && aux_after(&run, flush).is_empty(), "matched");
    let kill = run.kinds.iter().position(|k| *k == "restart").expect("the KILL");
    assert_persisted_equal(step(&run, kill), "restart kill");
}

/// W-B2 (drain): a mid-op freeze's delayed mirror. The first block's LEFT_IBD freezes at its
/// first decision point, before the op's last index write (`MidOp`): its mirror runs, then one
/// reconcile on each side, the per-cause counts agree, and a scored KILL persists the same.
/// THRESHOLD itself freezes only at an op's end on S0p (step 7 report).
#[test]
fn w_b2_delayed_mirror() {
    let run = run(&mut runner(), "extend main 1\nrestart kill\n", None);
    let at = nth_block(&run, 1);
    let l = step(&run, at);
    let journal: Vec<_> = l.end.entries.iter().map(|(j, c)| (j.cause.as_str(), *c)).collect();
    assert_eq!(journal, [("LEFT_IBD", OpClass::MidOp)]);
    let mirrors: Vec<_> = l.plan.mirrors.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(mirrors, ["LEFT_IBD"], "in journal order");
    assert_eq!(aux_after(&run, at), ["legb-mirror", "legb-reconcile"]);
    assert_eq!(l.delta.full.get("reconcile"), Some(&1), "the node's reconcile booking");
    assert_eq!(l.core.post.flush_delta, l.node.post.flush_delta, "per-cause counts");
    let kill = run.kinds.iter().position(|k| *k == "restart").expect("the KILL");
    assert_persisted_equal(step(&run, kill), "restart kill");
}

/// W-B3 (hold): CACHE_LARGE by one block's connect. Under hold, the block's second boundary
/// counts LEFT_IBD's frozen layer in flight beside active, and native Σ passes Core's LARGE line;
/// the mirror is under the node's label, which Core never books on its own here
/// (`-oracle-no-size-flush`).
#[test]
fn w_b3_native_cache_large() {
    let run = run(&mut runner(), "start hold\nextend main 1\n", None);
    let at = nth_block(&run, 1);
    let l = step(&run, at);
    let journal: Vec<_> = l.end.entries.iter().map(|(j, c)| (j.cause.as_str(), *c)).collect();
    assert_eq!(journal, [("LEFT_IBD", OpClass::MidOp), ("CACHE_LARGE", OpClass::OpEnd)]);
    let large = l.plan.mirrors.iter().find(|m| m.label == "CACHE_LARGE").expect("mirrored");
    assert!(large.causes.contains(FlushCauses::CACHE_LARGE) && !large.labelled);
    assert_eq!(l.core.post.flush_delta.full.get("CACHE_LARGE"), Some(&1));
    assert!(!l.core.post.flush_delta.full.contains_key("CACHE_CRITICAL"));
    assert_eq!(l.core.post.flush_delta, l.node.post.flush_delta);
}

/// W-B4 (drain): an index-only flush of the node's is mirrored as Core's `FLUSH INDEX_ONLY` under
/// its label, and a KILL after it persists the same index on both sides. A PERIODIC_FLUSH take
/// keeps Core's dirt (DEC-1500), so a reconcile follows the mirror.
#[test]
fn w_b4_index_only_mirror() {
    let mut runner = runner();
    let text = "extend main 2\nrestart kill\n";
    let first = run(&mut runner, text, None);
    let at = nth_block(&first, 2);
    let plant = Plant {
        arm: Some(FlushArm::IndexOnly(FlushCauses::PERIODIC_FLUSH)),
        ..Plant::at(at)
    };
    let run = run(&mut runner, text, Some(plant));
    let l = step(&run, at);
    let index_only: Vec<_> = l.plan.mirrors.iter().filter(|m| !m.full).collect();
    assert_eq!(index_only.len(), 1, "{:?}", l.plan.mirrors);
    assert_eq!(index_only[0].label, "PERIODIC_FLUSH");
    assert!(index_only[0].kept, "a PERIODIC_FLUSH take keeps");
    assert_eq!(aux_after(&run, at), ["legb-mirror", "legb-reconcile"]);
    assert_eq!(l.core.post.flush_delta.index_only.get("PERIODIC_FLUSH"), Some(&1));
    assert_eq!(l.core.post.flush_delta, l.node.post.flush_delta);
    let kill = run.kinds.iter().position(|k| *k == "restart").expect("the KILL");
    assert_persisted_equal(step(&run, kill), "restart kill");
}

/// W-B5 and W-B6 (i) (drain): W-B2's drain path with `HoldNextRelease` armed at the first
/// block. Under `drain` the arm is inert: the decision point's own drain takes the held release
/// at once, so the job books where an unheld one does (`MidOp`, then the mirror and one
/// reconcile under its own label), and the run replays the unarmed one. W-B3 is the end-to-end
/// witness of a release taken at the op's settle (under `hold`).
#[test]
fn w_b5_w_b6_settle_release() {
    let mut runner = runner();
    let text = "extend main 1\nrestart kill\n";
    let first = run(&mut runner, text, None);
    let at = nth_block(&first, 1);
    let plant = Plant {
        arm: Some(FlushArm::HoldNextRelease),
        ..Plant::at(at)
    };
    let run = run(&mut runner, text, Some(plant));
    assert!(replays(&first, &run), "the arm changes nothing under drain");
    let l = step(&run, at);
    let journal: Vec<_> = l.end.entries.iter().map(|(j, c)| (j.cause.as_str(), *c)).collect();
    assert_eq!(journal, [("LEFT_IBD", OpClass::MidOp)], "W-B5: the class at the freeze");
    assert_eq!(aux_after(&run, at), ["legb-mirror", "legb-reconcile"]);
    let delta: Vec<_> = l.delta.full.iter().map(|(k, n)| (k.as_str(), *n)).collect();
    assert_eq!(delta, [("LEFT_IBD", 1), ("reconcile", 1)], "W-B6: one reconcile, own label");
    assert_eq!(l.core.post.flush_delta, l.node.post.flush_delta);
}

/// W-B14 (hold), Sol r1 4: a reload with auxiliary steps after an image-bearing op step. The CLEAN
/// restart after 64 fan-out blocks and an invalidate of their first reactivates the chain in the
/// new open, which books THRESHOLD and CACHE_LARGE there; both are mirrored (with a reconcile),
/// and the reduced steps keep the reload's own persisted image, the same on both sides. The
/// queue-drain partials the large steps give are allowed; nothing else is.
#[test]
fn w_b14_reload_with_aux_keeps_its_image() {
    let text = format!("start hold\n{FAN_64}invalidate main@-63\nrestart clean\n");
    let run = runner().run(&Program::parse(&text).expect("parses")).expect("a run");
    assert!(matches!(run.end, End::Done), "{:?}", run.end);
    assert!(run.census.legb_refused_open.is_empty());
    let drains = run.census.ops.iter().filter(|((_, c), _)| *c == "unsupported-queue-drain");
    assert_eq!(drains.map(|(_, n)| *n as usize).sum::<usize>(), run.partial.len());
    let clean = run.kinds.iter().position(|k| *k == "restart").expect("the CLEAN");
    let l = step(&run, clean);
    assert_ne!(l.end.g0, l.end.g1, "a new open");
    let mirrored: Vec<_> = l.plan.mirrors.iter().map(|m| m.label.as_str()).collect();
    assert!(mirrored.contains(&"THRESHOLD"), "{mirrored:?}");
    assert!(!aux_after(&run, clean).is_empty(), "the reload has aux steps");
    assert_persisted_equal(l, "the reload with aux steps");
}

/// Sol r1 1: the queue facts are the reduced steps'. The CLEAN after eight fan-out blocks and an
/// invalidate retains more queued callbacks than [`QUEUE_LIMIT`](node_l1::chain::diff::QUEUE_LIMIT)
/// on both sides once the teardown's flush signals are dropped: a queue-drain partial, not a
/// scored `Done`.
#[test]
fn queue_reload_is_a_queue_drain_partial() {
    let text = "extend main 8 tx=fan\ninvalidate main@-6\nrestart clean\n";
    let run = runner().run(&Program::parse(text).expect("parses")).expect("a run");
    assert!(matches!(run.end, End::Done), "{:?}", run.end);
    let clean = run.kinds.iter().position(|k| *k == "restart").expect("the CLEAN");
    assert!(run.partial.contains(&(clean, "restart")), "{:?}", run.partial);
    let drains: u64 = run
        .census
        .ops
        .iter()
        .filter(|((_, c), _)| *c == "unsupported-queue-drain")
        .map(|(_, n)| *n)
        .sum();
    assert_eq!(drains as usize, run.partial.len());
}

/// Opus r1 5.2: a refused open whose cleanup flushes differ (a Core step planted with one more
/// ALWAYS) is a partial naming the label, with no `refused_load` credit; the input goes on.
#[test]
fn a_refused_open_s_cleanup_difference_is_a_partial() {
    let mut runner = runner();
    let text = "extend main 2\nwarp 100\nrestart kill\ntime 100\nrestart clean\nextend main 1\n";
    let first = run(&mut runner, text, None);
    let refused = first.kinds.iter().position(|k| *k == "restart").expect("the restart");
    let plant = Plant {
        core: |s| *s.post.flush_delta.full.entry("ALWAYS".into()).or_default() += 1,
        ..Plant::at(refused)
    };
    runner.plant(Some(plant));
    let planted = runner.run(&Program::parse(text).unwrap()).unwrap();
    runner.plant(None);
    assert!(matches!(planted.end, End::Done), "{:?}", planted.end);
    assert_eq!(planted.partial, [(refused, "restart")]);
    let want: std::collections::BTreeMap<String, u64> = [("ALWAYS".to_owned(), 1)].into();
    assert_eq!(planted.census.legb_refused_open, want);
    assert_eq!(planted.census.rows.get("refused_load"), None, "no credit");
    assert_eq!(first.census.rows.get("refused_load"), Some(&1), "the unplanted run's credit");
}

/// Opus r1 5.3: Core's session tainted at a leg-B mirror ends the input unexpected.
#[test]
fn a_tainted_mirror_is_unexpected() {
    let mut runner = runner();
    let text = "extend main 1\nrestart kill\n";
    let first = run(&mut runner, text, None);
    let at = nth_block(&first, 1);
    assert_eq!(first.kinds[at + 1], "legb-mirror");
    let plant = Plant {
        taint: true,
        ..Plant::at(at + 1)
    };
    runner.plant(Some(plant));
    let planted = runner.run(&Program::parse(text).unwrap()).unwrap();
    runner.plant(None);
    assert_eq!(planted.end, End::Unexpected("legb-mirror: the session tainted".into()));
}

/// W-B7 (drain): a CLEAN restart after the mirrors. The teardown's two ALWAYS bookings are the
/// old open's (`Retired`), matched against Core's pre-load bookings; the counts, the flushes
/// after the load and the sporks agree.
#[test]
fn w_b7_clean_after_mirrors() {
    let run = run(&mut runner(), "extend main 2\nrestart clean\n", None);
    let clean = run.kinds.iter().position(|k| *k == "restart").expect("the CLEAN");
    let l = step(&run, clean);
    let journal: Vec<_> = l
        .end
        .entries
        .iter()
        .map(|(j, c)| (j.cause.as_str(), *c, j.stamp.generation == l.end.g0))
        .collect();
    assert_eq!(journal, [("ALWAYS", OpClass::Retired, true); 2]);
    assert!(l.plan.mirrors.is_empty());
    assert_eq!(l.core.post.flush_delta, l.node.post.flush_delta);
    assert_eq!(l.core.post.flushes, l.node.post.flushes);
    assert!(l.core.post.sporks.is_some());
    assert_eq!(l.core.post.sporks, l.node.post.sporks);
}

/// W-B8 and W-B9 (drain): the reloads, a KILL and a CLEAN after the image's restore at
/// `T_RECENT`, and the restore itself. None books LEFT_IBD at T_RECENT (the node leaves IBD at
/// its first block, W-B2), and each persists the same on both sides; a record dropped from the
/// node's captured image is seen at `post.persisted`.
#[test]
fn w_b8_w_b9_reloads() {
    let mut runner = runner();
    let text = "restart kill\nrestart clean\n";
    let run_ = run(&mut runner, text, None);
    assert!(entries(&run_).iter().all(|e| e.1 != "LEFT_IBD"), "{:?}", entries(&run_));
    for (at, kind) in run_.kinds.iter().enumerate() {
        if *kind == "restore" || *kind == "restart" {
            assert_persisted_equal(step(&run_, at), kind);
            assert!(step(&run_, at).plan.mirrors.is_empty(), "{kind}");
        }
    }
    let kill = run_.kinds.iter().position(|k| *k == "restart").expect("the KILL");
    let plant = Plant {
        node: |n| {
            let persisted = n.step.post.persisted.as_mut().expect("persisted");
            persisted.best_block = None;
        },
        ..Plant::at(kill)
    };
    runner.plant(Some(plant));
    let planted = runner.run(&Program::parse(text).unwrap()).unwrap();
    runner.plant(None);
    match planted.end {
        End::Diverged(d) => assert_eq!(d.field, "post.persisted"),
        end => panic!("μ-img: {end:?}"),
    }
}

/// W-B10 (b) and (c) (drain): a restart the future tip refuses, then the clock back and a CLEAN
/// that recovers. The refused load's cleanup flushes (the teardown's two ALWAYS, and for a CLEAN
/// the refused open's two) match Core's exactly: scored, `refused_load` credited, nothing
/// mirrored; the CLEAN after it recovers, scored, and the node serves again. Leg A's S0 scores
/// and credits the same refusal.
#[test]
fn w_b10_refused_open() {
    let mut runner = runner();
    for (kill, retired) in [(false, 4), (true, 2)] {
        let text = format!(
            "extend main 2\nwarp 100\nrestart {}\ntime 100\nrestart clean\nextend main 1\n",
            if kill {
                "kill"
            } else {
                "clean"
            }
        );
        let run = run(&mut runner, &text, None);
        let refused = run.kinds.iter().position(|k| *k == "restart").expect("the restart");
        let l = step(&run, refused);
        assert!(l.core.post.down && l.node.post.down, "{text}: refused on both");
        assert_eq!(l.end.entries.len(), retired, "{text}: {:?}", l.end.entries);
        assert!(l.end.entries.iter().all(|(j, c)| j.cause == "ALWAYS" && *c == OpClass::Retired));
        assert!(l.plan.mirrors.is_empty(), "{text}: nothing mirrored");
        assert_eq!(l.core.post.flush_delta, l.node.post.flush_delta);
        assert_eq!(run.census.rows.get("refused_load"), Some(&1), "{text}: credited");
        let recovery = refused
            + 1
            + run.kinds[refused + 1..].iter().position(|k| *k == "restart").expect("CLEAN");
        let r = step(&run, recovery);
        assert!(!r.core.post.down && !r.node.post.down, "{text}: recovered");
        assert_persisted_equal(r, "the recovery");
        let block = run.kinds.iter().rposition(|k| *k == "block").expect("a block");
        assert!(block > recovery && !step(&run, block).node.post.down, "{text}: serves again");
    }
    drop(runner);
    let text = "extend main 2\nwarp 100\nrestart clean\ntime 100\nrestart clean\nextend main 1\n";
    let mut s0 = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).expect("S0");
    let leg_a = s0.run(&Program::parse(text).expect("parses")).expect("a run");
    assert!(matches!(leg_a.end, End::Done), "S0: {:?}", leg_a.end);
    assert_eq!(leg_a.census.rows.get("refused_load"), Some(&1), "S0: scored and credited");
}

/// W-B10 (a) (drain): a CLEAN right after the restore. The teardown's two ALWAYS are stamped
/// in the old open at the same `mutation_seq` as the new open's chain at the op's end (a1), so
/// only their generation keeps them off `OpEnd`; they are matched against Core's pre-load
/// bookings. One more `g0` booking injected into that op's end is an unmatched pre-load flush
/// (a2).
#[test]
fn w_b10_a_generation_boundary() {
    let run = run(&mut runner(), "restart clean\n", None);
    let clean = run.kinds.iter().position(|k| *k == "restart").expect("the CLEAN");
    let l = step(&run, clean);
    assert_ne!(l.end.g0, l.end.g1, "a new open");
    let seq = l.seq.expect("the new open serves");
    let journal: Vec<_> = l
        .end
        .entries
        .iter()
        .map(|(j, c)| (j.cause.as_str(), j.stamp.generation, j.stamp.mutation_seq, *c))
        .collect();
    assert_eq!(journal, [("ALWAYS", l.end.g0, seq, OpClass::Retired); 2], "a1: an equal seq");
    assert!(l.plan.mirrors.is_empty());
    assert_eq!(legb::boundaries(true, &l.core, false, &l.end), Ok(l.plan.clone()));
    let mut injected = l.end.clone();
    injected.entries.push(injected.entries[0].clone());
    assert_eq!(
        legb::boundaries(true, &l.core, false, &injected),
        Err(LegBEnd::Unexpected("legb: unmatched pre-load flush".into())),
        "a2"
    );
}

/// W-B11 (hold): `extend main 2` under hold mirrors the same CACHE_LARGE at its first block's
/// end in every run, and every run replays the one before it.
#[test]
fn w_b11_hold_is_deterministic() {
    let mut runner = runner();
    let text = "start hold\nextend main 2\n";
    let mut prev: Option<Run> = None;
    for _ in 0..10 {
        let run = run(&mut runner, text, None);
        let natural: Vec<_> = entries(&run).into_iter().filter(|e| e.1 != "LEFT_IBD").collect();
        assert_eq!(natural, [(nth_block(&run, 1), "CACHE_LARGE", OpClass::OpEnd)]);
        if let Some(p) = &prev {
            assert!(replays(p, &run), "a hold run replays");
        }
        prev = Some(run);
    }
}

/// The program's one KILL, which the tie model defers (generate.rs `restart`): it runs last,
/// after an extension of the active chain and its ALWAYS, never skipped as a `restart_tie`.
fn deferred_kill(run: &Run, what: &str) -> usize {
    let kill = run.kinds.iter().rposition(|k| *k == "restart").expect("the KILL");
    assert_eq!(kill, run.kinds.len() - 1, "{what}: the KILL ends the run");
    assert_eq!(run.census.deferred_restarts, 1, "{what}: one deferred restart");
    assert!(!run.census.avoided.contains_key("restart_tie"), "{what}: not skipped");
    kill
}

/// W-B12 (drain and hold): an op-end node-own flush that kept Core's dirt, then a ChainLock
/// mark on the block its take wrote. THRESHOLD at a fan-out block's end (CACHE_LARGE beside it
/// under hold), with no mid-op booking in that op, is mirrored, then one reconcile on each side
/// (rule 3, DEC-1447). The header-only fork keeps the enforce's tip on main, below that block, so
/// the enforce disconnects and marks it; the reconcile left it clean on both sides, so neither
/// disk stores its mark, while the later blocks' marks are stored on both (a kill writes
/// nothing, hence the explicit flush). The deferred KILL persists the same on both sides. 56
/// fan-out blocks leave the fork its block; the input is picked for its salt (OD-L3-119 item 4).
#[test]
fn w_b12_op_end_kept_dirt() {
    let fan56 = &FAN_64[..FAN_64.len() - "extend main 8 tx=fan\n".len()];
    let mut runner = runner();
    for prefix in ["", "start hold\n"] {
        let text = format!(
            "{prefix}{fan56}time 2\nfork 4 1 headers\ncl tip#1\nflush always\nrestart kill\n"
        );
        assert_eq!(salt(&text), 0xb255_4eb3_c8be_24d8, "the input W-B12 was picked for");
        let run = run(&mut runner, &text, None);
        let at = entries(&run).iter().find(|e| e.1 == "THRESHOLD").expect("THRESHOLD").0;
        assert_eq!(run.kinds[at], "block", "{prefix:?}");
        let l = step(&run, at);
        let classes: Vec<_> = l.end.entries.iter().map(|(j, c)| (*c, j.kept)).collect();
        assert!(classes.iter().all(|e| *e == (OpClass::OpEnd, true)), "{prefix:?}: {classes:?}");
        assert!(l.plan.mirrors.iter().all(|m| m.class == OpClass::OpEnd && m.kept));
        let mut aux = vec!["legb-mirror"; l.plan.mirrors.len()];
        aux.push("legb-reconcile");
        assert_eq!(aux_after(&run, at), aux, "{prefix:?}: the mirrors, then one reconcile");
        let taken = l.node.post.tip.as_ref().expect("the block's tip");
        let enforce = run.kinds.iter().position(|k| *k == "cl_enforce").expect("the enforce");
        let e = step(&run, enforce).node.post.tip.as_ref().expect("the enforce's tip");
        assert!(e.0 <= taken.0 && e.1 != taken.1, "{prefix:?}: the enforce disconnects the block");
        let k = step(&run, deferred_kill(&run, prefix));
        let failed = &k.core.post.persisted.as_ref().expect("Core persisted").failed;
        assert!(!failed.is_empty(), "{prefix:?}: the marks reached Core's disk");
        assert!(failed.iter().all(|(h, _)| *h != taken.1), "{prefix:?}: the reconciled block");
        assert_persisted_equal(k, "restart kill");
    }
}

/// W-B13 (drain and hold; DEC-1451 Q5): a ChainLock mark after a mid-op LEFT_IBD freeze. The
/// node, still in IBD on an old chain, takes a header fork's ChainLock; the enforce's first
/// disconnect is its first decision point out of IBD, so LEFT_IBD freezes there, mid-op, before
/// the marks; its take kept Core's dirt (CH3b D242), the mirror and one reconcile follow, the
/// explicit flush writes the marks, and the deferred KILL persists the same on both sides.
#[test]
fn w_b13_mid_op_left_ibd_mark() {
    let mut runner = runner();
    for prefix in ["", "start hold\n"] {
        let text = format!(
            "start old\n{prefix}extend main 3\nfork 2 2 headers\nwarp 40\ncl tip#1\n\
             flush always\nrestart kill\n"
        );
        assert_eq!(salt(&text), 0x997d_3428_c122_dea6, "the input W-B13 was written for");
        let run = run(&mut runner, &text, None);
        let enforce = run.kinds.iter().position(|k| *k == "cl_enforce").expect("the enforce");
        let l = step(&run, enforce);
        let left_ibd: Vec<_> = l
            .end
            .entries
            .iter()
            .filter(|(j, _)| j.cause == "LEFT_IBD")
            .map(|(j, c)| (*c, j.kept))
            .collect();
        assert_eq!(left_ibd, [(OpClass::MidOp, true)], "{prefix:?}");
        assert_eq!(aux_after(&run, enforce).last(), Some(&"legb-reconcile"), "{prefix:?}");
        let before = run.legb.iter().rfind(|s| s.at < enforce).expect("a step before");
        let old = before.node.post.tip.as_ref().expect("the old tip");
        let e = l.node.post.tip.as_ref().expect("the enforce's tip");
        assert!(e.1 != old.1, "{prefix:?}: the enforce disconnects the old tip");
        let k = step(&run, deferred_kill(&run, prefix));
        let failed = &k.core.post.persisted.as_ref().expect("Core persisted").failed;
        assert!(failed.iter().any(|(h, _)| *h == old.1), "{prefix:?}: the old tip's mark on disk");
        assert_persisted_equal(k, "restart kill");
    }
}

/// Core alone on S0p's image at `T_RECENT`: `prelude`'s restart, if any, then [`FAN_64`]. The
/// size causes Core books, by the 1-based number of the block that books them, and the number
/// of blocks connected.
fn core_fan(core: &mut CoreSut, prelude: Option<RestartKind>) -> (Vec<(usize, String)>, usize) {
    core.restore(T_RECENT).expect("restored");
    let mut w = Schedule::S0p.world();
    w.loaded();
    if let Some(kind) = prelude {
        let op = ChainOp::Restart(kind);
        let done = core.step(&op).expect("restarted");
        assert!(!done.step.post.down, "{kind:?}: Core serves");
        w.observe(&op, &done.step);
    }
    let (mut generator, mut census) = (Gen::default(), Census::default());
    let (mut size, mut blocks) = (Vec::new(), 0);
    for intent in &Program::parse(FAN_64).expect("parses").intents {
        let mut ops: VecDeque<_> = generator.resolve(&mut w, intent, &mut census).into();
        while let Some(p) = ops.pop_front() {
            let done = core.step(&p.op).expect("a step");
            w.observe(&p.op, &done.step);
            if matches!(p.op, ChainOp::Block(_)) {
                assert!(done.step.reply.outcome.is_ok(), "block {}", blocks + 1);
                blocks += 1;
            }
            let fd = &done.step.post.flush_delta;
            for cause in fd.full.keys().chain(fd.index_only.keys()) {
                if cause.starts_with("CACHE_") {
                    size.push((blocks, cause.clone()));
                }
            }
            if let Some(Some(e)) = due_enforce(&w, &mut census) {
                ops.push_front(e);
            }
        }
    }
    (size, blocks)
}

/// W-B3's baseline (Core alone): started without `-oracle-no-size-flush`, Core books CACHE_LARGE
/// on its own at FAN_64's 21st block; S0p's Core (`CoreSut::start`) books no size cause there,
/// after the restore, after a KILL and after a CLEAN, so the flag holds across each reload.
#[test]
fn w_b3_baseline_core_size_flush() {
    let (mut bare, _) =
        CoreSut::start_with_args(Pin::V24_0, Schedule::S0p, &Schedule::S0p.args()).expect("bare");
    let (size, blocks) = core_fan(&mut bare, None);
    assert_eq!(blocks, 64);
    assert_eq!(size.first(), Some(&(21, "CACHE_LARGE".to_string())), "P: {size:?}");
    assert!(size.iter().all(|(_, c)| c == "CACHE_LARGE"), "{size:?}");
    drop(bare);
    let (mut core, _) = CoreSut::start(Pin::V24_0, Schedule::S0p).expect("S0p's Core");
    for prelude in [None, Some(RestartKind::Kill), Some(RestartKind::Clean)] {
        let (size, blocks) = core_fan(&mut core, prelude);
        assert_eq!(blocks, 64, "{prelude:?}");
        assert!(size.is_empty(), "{prelude:?}: {size:?}");
    }
}

/// O2b-4 4f2's spork programs on leg B (OD-L3-152): each of `chain-v2`'s spork seeds, run in the
/// target's encoding on S0p under both W schedules, ends done with no partial after a spork op,
/// takes leg-B steps with both sides' spork state on each spork op, and reaches the rows 4f2's S0
/// tests 18, 19 and 30 name for it.
#[test]
fn spork_seeds_reach_their_rows_on_leg_b() {
    const SPORK_ROWS: [(&str, &str); 3] = [
        ("spork_reset", "spork_on_then_cl_record"),
        ("spork_kill", "spork_then_kill_reverts"),
        ("spork_clean", "spork_then_clean_keeps"),
    ];
    const TIMER_SEEDS: [&str; 2] = ["spork_off_conflict", "spork_off_reorg"];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chain_seeds/chain-v2");
    let mut seeds: Vec<(String, Program)> = std::fs::read_dir(dir)
        .expect("chain-v2's programs")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "prog"))
        .map(|p| {
            let name = p.file_stem().expect("a stem").to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).expect("a program");
            (name, Program::parse(&text).expect("parses"))
        })
        .collect();
    seeds.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(seeds.len(), 8, "chain-v2's spork programs");
    let mut runner = runner();
    runner.encoding = Encoding::V2;
    for (name, program) in &seeds {
        for w_hold in [false, true] {
            let program = Program {
                w_hold,
                ..program.clone()
            };
            let run = runner.run(&program).expect("a run");
            let rows = &run.census.rows;
            let at = format!("{name} (w_hold {w_hold}): {:?}, rows {rows:?}", run.kinds);
            assert!(matches!(run.end, End::Done), "{at}: {:?}", run.end);
            assert!(run.partial.is_empty(), "{at}: partial {:?}", run.partial);
            assert!(run.kinds.contains(&"spork"), "{at}: no spork op ran");
            assert!(rows.get("legb_steps").is_some_and(|&n| n > 0), "{at}: no leg-B step");
            assert_eq!(rows.get("legb_hold").is_some(), w_hold, "{at}: the W schedule");
            let spork_steps: Vec<_> =
                run.legb.iter().filter(|s| run.kinds.get(s.at) == Some(&"spork")).collect();
            assert!(!spork_steps.is_empty(), "{at}: no leg-B step on a spork op");
            for s in spork_steps {
                assert!(
                    s.core.post.sporks.is_some() && s.node.post.sporks.is_some(),
                    "{at}: report {}: a side's spork state is missing",
                    s.at
                );
            }
            if let Some(&(_, row)) = SPORK_ROWS.iter().find(|(seed, _)| seed == name) {
                assert_eq!(rows.get(row), Some(&1), "{at}");
            }
            if TIMER_SEEDS.contains(&name.as_str()) {
                assert_eq!(rows.get("cl_timer_enforce"), Some(&1), "{at}");
            }
            let avoided = &run.census.avoided;
            match name.as_str() {
                "spork_reset_after_ignore" => {
                    assert_eq!(rows.get("cl_dropped_any"), Some(&1), "{at}");
                    assert_eq!(run.kinds.last(), Some(&"block"), "{at}: the delivery ran");
                }
                "spork_reset_keeps_marks" => {
                    assert_eq!(avoided.get("cl_flag_unsure_ignore"), Some(&1), "{at}: {avoided:?}");
                    assert_eq!(run.kinds.last(), Some(&"block"), "{at}: the delivery ran");
                }
                "spork_no_reset" => {
                    assert_eq!(rows.get("spork_on_then_cl_record"), None, "{at}");
                }
                _ => {}
            }
        }
    }
}
