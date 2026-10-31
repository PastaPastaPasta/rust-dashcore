//! CH3a a10-1: the lifecycle ops on `diff_chain`'s two sides, Core's session and `NodeSut`'s V
//! given the same ops: `RESTART` (clean, killed, refused), `FLUSH`, the restore, and the post's
//! `persisted`, `best_invalid` and `candidates` across them. Each op is held as Diff mode holds
//! it: every plane outside the gaps the same (`diff::GAPS`: records until a7, the flush counters until
//! a10-2), the reply with its `BlockChecked` list, the ordered events and a down post included.
//!
//! Two kinds of test need the oracle at ABI 33 and say so when it is older (no filter, no
//! ignore): a clean teardown's events (the M-4 shim fix, which delivers the first shutdown
//! flush's `ChainStateFlushed` and drops the second's, as dashd does) and the post's
//! `best_invalid` and `candidates` (OD-L3-15), which an older shim does not write.
//!
//! Each test counts the up posts holding more than one candidate on each side (OD-L3-19's
//! tripwire: Core prunes its candidates to the tip, so a post holds the tip alone).

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/chain_sides.rs"]
mod chain_sides;

use std::collections::VecDeque;

use chain_sides::{assert_planes, at_abi_33, core_step, node_ran, node_step, private_dir, sides};
use dash_chain::Status;
use dash_oracle_sys::{BlockAt, Candidate, ChainHash, ChainOp, RestartKind, SporkState};
use node_l1::chain::census::Census;
use node_l1::chain::core::{CoreSut, SPORK_19, SPORK_OFF, Schedule, T_OLD, T_RECENT, image_world};
use node_l1::chain::diff::{Facts, QUEUE_LIMIT, as_judged};
use node_l1::chain::generate::{Gen, due_enforce};
use node_l1::chain::intent::Program;
use node_l1::chain::node::{NodeSide, NodeStep};
use node_l1::chain::run::op_kind;
use node_l1::chain::step::Step;
use node_l1::chain::world::World;

/// What the gated tests need from the shim at ABI 33.
const NEEDS: &str = "the M-4 clean teardown's events, the post's best_invalid and candidates";

/// One op's two steps.
struct Ran {
    op: ChainOp,
    core: Step,
    node: Step,
}

/// A program run on both sides.
struct Lifecycle {
    w: World,
    generator: Gen,
    ran: Vec<Ran>,
}

impl Lifecycle {
    /// The step of the `n`-th op of `kind`.
    fn nth(&self, kind: &str, n: usize) -> &Ran {
        self.ran.iter().filter(|r| op_kind(&r.op) == kind).nth(n).expect("the op ran")
    }

    /// The `k`-th block the program built, as the oracle's hash.
    fn built(&self, k: usize) -> ChainHash {
        ChainHash(self.w.blocks[self.generator.built[k]].hash)
    }
}

/// The up posts with more than one candidate, on each side (OD-L3-19's tripwire).
fn several_candidates(ran: &[Ran]) -> (usize, usize) {
    let several = |s: &Step| s.post.candidates.as_ref().is_some_and(|c| c.len() > 1);
    let core = ran.iter().filter(|r| several(&r.core)).count();
    let node = ran.iter().filter(|r| several(&r.node)).count();
    (core, node)
}

/// Two steps held as Diff mode holds them: no plane outside `diff::GAPS` differs, the events as
/// `diff::judge` takes them (`as_judged`).
fn held(core: &Step, node: &NodeStep, what: &str) {
    let (core, node) = as_judged(core, node);
    assert_planes(&core, &node, what);
}

/// `text`'s ops on both sides from the image (restored at the program's clock), each held
/// ([`held`]), with each enforcement the generator finds due.
fn lifecycle(text: &str) -> Lifecycle {
    lifecycle_then(text, |_, _| Vec::new())
}

/// [`lifecycle`], then the ops `tail` gives from the program's world and generator, held the
/// same: ops the generator leaves out.
fn lifecycle_then(text: &str, tail: impl FnOnce(&World, &Gen) -> Vec<ChainOp>) -> Lifecycle {
    let program = Program::parse(text).unwrap();
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let mut w = image_world();
    if program.old {
        core.restore(T_OLD).unwrap();
        node.restore(T_OLD).unwrap();
        w.mocktime = T_OLD;
    }
    w.loaded();
    // The image holds spork 19 (D34), so spork intents are admitted here (O2b-4).
    let mut generator = Gen::default();
    generator.spork_admitted = true;
    let mut census = Census::default();
    let mut ran = Vec::new();
    let mut run = |op: ChainOp, w: &mut World| {
        let (c, n) = (core_step(&mut core, &op), node_ran(&mut node, &op));
        held(&c, &n, &format!("{text:?} op {} {}", ran.len(), op_kind(&op)));
        w.observe(&op, &c);
        ran.push(Ran {
            op,
            core: c,
            node: n.step,
        });
    };
    for intent in &program.intents {
        let mut ops: VecDeque<_> = generator.resolve(&mut w, intent, &mut census).into();
        while let Some(p) = ops.pop_front() {
            run(p.op, &mut w);
            match due_enforce(&w, &mut census) {
                Some(Some(enforce)) => ops.push_front(enforce),
                Some(None) => panic!("the program stays in the supported ops"),
                None => {}
            }
        }
    }
    for op in tail(&w, &generator) {
        run(op, &mut w);
    }
    let (on_core, on_node) = several_candidates(&ran);
    eprintln!(
        "{text:?}: {} ops; up posts with several candidates: Core {on_core}, node {on_node}",
        ran.len()
    );
    assert_eq!((on_core, on_node), (0, 0), "OD-L3-19: a post holds the tip alone");
    Lifecycle {
        w,
        generator,
        ran,
    }
}

/// The tests that return early at an oracle before ABI 33, where the harness lists them `ok`.
const ABI_33_GATED: [&str; 6] = [
    "l1_chain_diff::diff_mode_ends_each_input_unsupported_or_on_a_gap",
    "l1_chain_lifecycle::an_over_limit_clean_restart_holds",
    "l1_chain_lifecycle::best_invalid_and_candidates_follow_core_s_through_a_load",
    "l1_chain_lifecycle::clean_restarts_and_a_refused_load_hold_op_by_op",
    "l1_chain_lifecycle::the_spork_store_follows_the_teardown (its clean trial)",
    "l1_chain_locks::the_cons_11_cases_across_a_clean_restart",
];

/// Opus r1 n-2's tripwire: the pin move to ABI 33 fails it, so the gated tests are seen to run
/// (`--nocapture`, no `SKIPPED` line) before it is removed.
#[test]
fn the_abi_33_tests_wait_for_the_pin_move() {
    let abi = dash_oracle_sys::ffi::ABI_VERSION;
    assert!(
        abi < 33,
        "the oracle is at ABI {abi}: run these with --nocapture, see no SKIPPED line, then remove \
         this tripwire: {ABI_33_GATED:#?}"
    );
}

/// M-1: a clean restart whose load reconnects six blocks (the load's failure-flag reset lets
/// its ABC reconnect the six the invalidation disconnected), so Core queues more than
/// `diff::QUEUE_LIMIT` signals, its teardown's `ChainStateFlushed` first. Held as Diff mode
/// holds it: the teardown first on both sides, the load's events in a drain's order.
#[test]
fn an_over_limit_clean_restart_holds() {
    let program = "extend main 6\nflush always\ninvalidate main@-5\nflush always\nrestart clean\n";
    Program::parse(program).unwrap();
    if !at_abi_33("an_over_limit_clean_restart_holds", NEEDS) {
        return;
    }
    let l = lifecycle(program);
    let restart = l.ran.last().expect("the restart");
    assert_eq!(restart.op, ChainOp::Restart(RestartKind::Clean));
    assert!(Facts::core(&restart.core).queued > QUEUE_LIMIT, "{:?}", restart.core.events);
    assert_eq!(restart.core.events.first().map(|e| e.name.as_str()), Some("ChainStateFlushed"));
}

/// Killed restarts, which a shim before ABI 33 tears down as dashd does too: index entries that
/// were never flushed are lost and a flushed extension survives (`kill_unflushed`); flushed
/// header-only entries are stored (the persisted digest hashes them, D37); a start in IBD whose
/// clock leaves it, then a kill, where the persisted image shows no flush on leaving IBD (Core
/// has no such cause; `CoreCausesOnly` masks V's, M-3).
#[test]
fn killed_restarts_hold_op_by_op() {
    let kill = lifecycle(include_str!("chain_seeds/kill_unflushed.prog"));
    let restarts: Vec<_> = kill.ran.iter().filter(|r| op_kind(&r.op) == "restart").collect();
    assert_eq!(restarts.len(), 2);
    for r in &restarts {
        let persisted = r.node.post.persisted.as_ref().expect("a restart's persisted image");
        assert!(persisted.index_entries > 0, "{persisted:?}");
    }
    assert_ne!(
        restarts[0].node.post.persisted, restarts[1].node.post.persisted,
        "the flushed extension is stored"
    );

    let headers = lifecycle("extend main 2 headers hold\nflush always\nrestart kill\n");
    let restart = &headers.ran.last().expect("the restart").node.post;
    let tip = restart.tip.expect("up").0;
    let stored = restart.persisted.as_ref().expect("a restart's persisted image").index_entries;
    let entries = u64::try_from(tip).unwrap() + 1 + 2;
    assert_eq!(stored, entries, "the chain to the tip and two header-only entries");

    let ibd = lifecycle("start old\nextend main 1\nwarp 30\nextend main 1\nrestart kill\n");
    let restart = ibd.ran.last().expect("the restart");
    assert_eq!(restart.node.post.ibd, Some(false), "out of IBD");
    let left = &ibd.ran[2].node.post;
    assert_eq!(left.ibd, Some(false), "the second extend leaves IBD");
}

/// W-P3b through a killed restart: the persisted image stores the three disconnected blocks
/// failed, and the load's failure-flag reset reconnects them, each op held. The reconnects run
/// in the import, so their signals say `ibd` true on both sides: V's flush site reads IBD
/// without latching (Core's `FlushStateToDisk` reads it only to prune, `validation.cpp:3076` at
/// the pin), so the coins attach's flush before the import cannot end IBD.
#[test]
fn w_p3b_holds_through_a_killed_restart() {
    let w_p3b = lifecycle(include_str!("chain_seeds/w_p3b.prog"));
    let restart = w_p3b.ran.last().expect("the restart");
    let failed = &restart.node.post.persisted.as_ref().expect("persisted").failed;
    let stored_failed = failed.iter().filter(|(_, bits)| bits & Status::FAILED_VALID != 0).count();
    assert_eq!(stored_failed, 3, "W-P3b: the node stores the three blocks failed");
}

/// W-P3b's persisted image at the killed restart held to Core's, apart from the op-by-op hold
/// ([`w_p3b_holds_through_a_killed_restart`]): the three disconnected blocks stored failed, read
/// before the load resets their flags (O2b D37). Core's steps give
/// the ops ([`ops_on_core`]); only the restart's `persisted` is compared.
#[test]
fn w_p3b_s_persisted_image_is_core_s() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let (ops, core_steps, ..) = ops_on_core(&mut core, include_str!("chain_seeds/w_p3b.prog"));
    let restart = ops.iter().map(|op| node_step(&mut node, op)).last().unwrap();
    let persisted = restart.post.persisted.expect("a restart's persisted image");
    assert_eq!(Some(&persisted), core_steps.last().unwrap().post.persisted.as_ref());
    let failed = persisted.failed.iter().filter(|(_, bits)| bits & Status::FAILED_VALID != 0);
    assert_eq!(failed.count(), 3, "the three disconnected blocks");
}

/// Clean restarts and a refused load: the seeds whose teardowns are clean, each op held, with
/// the clean teardown's events (needs ABI 33). `refused_load` is the future-tip refusal (M-2):
/// its `RESTART` replies `failure` with Core's message, its post is down with the persisted image,
/// and its events hold the refusal teardown's `ChainStateFlushed`, which the node hears because
/// its listener comes before the load verifies; `invalidate_restart`'s load reconnects the
/// invalidated tip (D32's reset), heard because the listener comes before the load's
/// `ActivateBestChain`.
#[test]
fn clean_restarts_and_a_refused_load_hold_op_by_op() {
    if !at_abi_33("clean_restarts_and_a_refused_load_hold_op_by_op", NEEDS) {
        return;
    }
    lifecycle(include_str!("chain_seeds/basic.prog"));

    let refused = lifecycle(include_str!("chain_seeds/refused_load.prog"));
    let restart = refused.nth("restart", 0);
    let result = restart.node.reply.outcome.as_ref().expect("a load reply");
    assert_eq!(result["status"], "failure", "{result}");
    assert!(restart.node.post.down && restart.node.post.persisted.is_some());
    let flushed = restart.node.events.iter().filter(|e| e.name == "ChainStateFlushed").count();
    assert!(flushed > 0, "the refusal's teardown is heard: {:?}", restart.node.events);
    let down = refused.nth("invalidate", 0);
    assert_eq!(down.node.reply.outcome.as_ref().unwrap_err().message, "session-down");
    let back = refused.nth("restart", 1);
    assert_eq!(back.node.reply.outcome.as_ref().unwrap()["status"], "success");

    let invalidate = lifecycle(include_str!("chain_seeds/invalidate_restart.prog"));
    let restart = invalidate.nth("restart", 0);
    let tips = restart.node.events.iter().filter(|e| e.name == "UpdatedBlockTip").count();
    assert_eq!(tips, 1, "the load reconnects the tip: {:?}", restart.node.events);
}

/// The spork store follows the teardown (O2b D34), from the spork-19 image: spork 19 turned off,
/// then a killed restart loads it on again (nothing stored), a clean one off (stored at
/// shutdown). Each trial from a fresh restore.
#[test]
fn the_spork_store_follows_the_teardown() {
    let on = SporkState(SPORK_19, 0, true);
    let off = SporkState(SPORK_19, SPORK_OFF, false);
    let sporks = |l: &Lifecycle| l.ran.last().unwrap().node.post.sporks.clone().unwrap();
    let kill = lifecycle("spork off\nrestart kill\n");
    let turned = kill.nth("spork", 0).node.post.sporks.clone().unwrap();
    assert!(turned.contains(&off), "the spork op ran: {turned:?}");
    assert!(sporks(&kill).contains(&on), "a kill stores nothing");
    if at_abi_33("the_spork_store_follows_the_teardown: the clean trial", NEEDS) {
        let clean = lifecycle("spork off\nrestart clean\n");
        assert!(sporks(&clean).contains(&off), "a clean shutdown stores the spork");
    }
}

/// The restore step (step 0, BL-283's check) the same on both sides: at both clocks, and after
/// ops that a restore must undo (a block, a spork, a flush that stored them).
#[test]
fn the_restore_step_is_the_same_on_both_sides() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let restore = |core: &mut CoreSut, node: &mut NodeSide, t: i64| {
        let c = core.restore(t).unwrap();
        assert!(!c.tainted);
        let n = node.restore(t).unwrap();
        held(&c.step, &n, &format!("restore at {t}"));
        assert!(n.step.post.persisted.is_some(), "a restore's persisted image");
        n.step
    };
    let first = restore(&mut core, &mut node, T_RECENT);
    restore(&mut core, &mut node, T_OLD);
    let w = image_world();
    let block = w.blocks.last().unwrap();
    for op in [
        ChainOp::Invalidate(block.hash),
        ChainOp::Spork {
            id: SPORK_19,
            value: SPORK_OFF,
        },
        ChainOp::Flush {
            mode: dash_oracle_sys::FlushMode::Always,
            label: String::new(),
        },
    ] {
        let (c, n) = (core_step(&mut core, &op), node_ran(&mut node, &op));
        held(&c, &n, op_kind(&op));
    }
    let again = restore(&mut core, &mut node, T_RECENT);
    assert_eq!(again.post, first.post, "the restore undid the ops");
    assert_eq!(node.restore(T_RECENT).map(|s| s.step.post), Ok(first.post), "and again");
}

/// The shim selftest's `best_invalid` and `candidates` sequence (OD-L3-15): a tip, its
/// invalidation and reconsideration, a fork whose longer branch takes the tip, the invalidation
/// of the fork's base; then a clean restart, whose load takes the most-work failed entry (the
/// branch's tip b2, not the run-time b1; shim r1 m-2, `validation.cpp:5362-5363` at rc.4) and
/// gives the tip a1 sequence id 0. [`BEST_INVALID_TAIL`] follows it.
const BEST_INVALID_PROGRAM: &str = "extend main 1\ninvalidate built#0\nreconsider built#0\n\
    fork 1 2\ninvalidate built#1\nrestart clean\n";

/// The sequence's end, stepped by hand: `PRECIOUS(a1)`, sequence id -1, then a killed restart.
/// The generator leaves the `PRECIOUS` out (a1 ties the failed b1's work, `ties::risky_from`).
fn best_invalid_tail(a1: ChainHash) -> [ChainOp; 2] {
    [ChainOp::Precious(a1.0), ChainOp::Restart(RestartKind::Kill)]
}

/// One op's kind with the `best_invalid` and `candidates` after it.
type Read = (&'static str, Option<BlockAt>, Vec<Candidate>);

/// The sequence's values after each op, as Core gives them (the shim selftest's); `built` are
/// the program's blocks a1, b1 and b2.
fn assert_best_invalid_and_candidates(read: &[Read], built: [ChainHash; 3]) {
    let [a1, b1, b2] = built;
    let nth = |kind: &str, n: usize| {
        let r = read.iter().filter(|r| r.0 == kind).nth(n).expect("the op ran");
        (r.1, r.2.clone())
    };
    // Regtest work is 2 per block: 2 * (201 + 1) at a1.
    let a1_work = format!("{:064x}", 2 * (201 + 1));
    let connected = Status::VALID_SCRIPTS | Status::HAVE_DATA | Status::HAVE_UNDO;
    let candidate = |seq| vec![Candidate(201, a1, seq, connected, a1_work.clone())];

    assert_eq!(nth("block", 0), (None, candidate(1)), "a1 connected");
    assert_eq!(nth("invalidate", 0).0, Some(BlockAt(201, a1)));
    assert_eq!(nth("reconsider", 0).0, None);
    assert_eq!(nth("invalidate", 1).0, Some(BlockAt(201, b1)), "run time: b1");
    let clean = nth("restart", 0);
    assert_eq!(clean, (Some(BlockAt(202, b2)), candidate(0)), "the load: b2, a1 at 0");
    assert_eq!(nth("precious", 0), (Some(BlockAt(202, b2)), candidate(-1)));
    assert_eq!(nth("restart", 1), clean, "the killed restart loads the same");
}

/// The sequence on both sides, every step comparing both fields with Core's, and the node's
/// posts holding Core's values.
#[test]
fn best_invalid_and_candidates_follow_core_s_through_a_load() {
    if !at_abi_33("best_invalid_and_candidates_follow_core_s_through_a_load", NEEDS) {
        return;
    }
    let l = lifecycle_then(BEST_INVALID_PROGRAM, |w, g| {
        best_invalid_tail(ChainHash(w.blocks[g.built[0]].hash)).into()
    });
    let read: Vec<Read> = l
        .ran
        .iter()
        .map(|r| {
            let post = &r.node.post;
            (op_kind(&r.op), post.best_invalid.unwrap(), post.candidates.clone().unwrap())
        })
        .collect();
    assert_best_invalid_and_candidates(&read, [l.built(0), l.built(1), l.built(2)]);
}

/// `text`'s ops as the generator resolves them against Core's steps from the image, with Core's
/// steps and the world and generator after them: Core ran them, the node did not.
fn ops_on_core(core: &mut CoreSut, text: &str) -> (Vec<ChainOp>, Vec<Step>, World, Gen) {
    let program = Program::parse(text).unwrap();
    let mut w = image_world();
    w.loaded();
    let mut generator = Gen::default();
    let mut census = Census::default();
    let (mut ops, mut steps) = (Vec::new(), Vec::new());
    for intent in &program.intents {
        for p in generator.resolve(&mut w, intent, &mut census) {
            let step = core_step(core, &p.op);
            w.observe(&p.op, &step);
            assert!(due_enforce(&w, &mut census).is_none(), "no enforcement is due");
            ops.push(p.op);
            steps.push(step);
        }
    }
    (ops, steps, w, generator)
}

/// The sequence at any ABI: V's `best_invalid` and candidates, read after each op, hold the
/// values Core gives. Core's steps give the ops ([`ops_on_core`]), but nothing else is compared:
/// before ABI 33 a clean teardown's events differ.
#[test]
fn the_node_s_best_invalid_and_candidates_hold_core_s_values_through_a_load() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let (mut ops, _, w, generator) = ops_on_core(&mut core, BEST_INVALID_PROGRAM);
    let built = |k: usize| ChainHash(w.blocks[generator.built[k]].hash);
    let built = [built(0), built(1), built(2)];
    ops.extend(best_invalid_tail(built[0]));
    let mut read = Vec::new();
    for op in &ops {
        node_step(&mut node, op);
        let best_invalid = node.best_invalid().expect("V serves");
        read.push((op_kind(op), best_invalid, node.candidates().expect("V serves")));
    }
    assert_best_invalid_and_candidates(&read, built);
}

/// A clean restart's events list what the old V delivered as it stopped (its first shutdown
/// flush's `ChainStateFlushed`) before the new V's load, at any ABI: Core drains the old
/// session's queue in its teardown, before the next load (M-4). The load reconnects the
/// invalidated tip, so it queues signals of its own, which a drain at the op's end would
/// otherwise put first.
#[test]
fn a_clean_restart_lists_the_old_v_s_teardown_first() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let program = "extend main 1\ninvalidate built#0\nflush always\nrestart clean\n";
    let (ops, ..) = ops_on_core(&mut core, program);
    let restart = ops.iter().map(|op| node_step(&mut node, op)).last().unwrap();
    let names: Vec<_> = restart.events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names.first(), Some(&"ChainStateFlushed"), "{names:?}");
    assert!(names[1..].contains(&"BlockConnected"), "the load reconnects the tip: {names:?}");
}
