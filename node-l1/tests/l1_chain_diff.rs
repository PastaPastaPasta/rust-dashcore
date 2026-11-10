//! O2b-3b: `diff_chain`'s rdashd side (`NodeSut`'s V) against Core over the ops V serves today
//! (DEC-286): the image's chain on both sides, extend-only programs op by op (replies, events
//! and the post fields the node reads), reorg programs plane by plane, the ops V does not serve,
//! and Diff mode's wiring.
//!
//! Every post field the node reads is held equal to Core's, the UTXO and index digests
//! included: V connects and disconnects through ND1's `CoreConnector`, which writes each
//! block's undo data and reads it back, and the digest of V's own entries must be Core's, which
//! also ties the node's digest code to Core's on one chain.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/chain_sides.rs"]
mod chain_sides;
#[path = "support/witness.rs"]
mod witness;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chain_sides::{
    assert_posts, assert_same, at_abi_33, both, loaded_world, node_ran, private_dir, same_program,
    sides,
};
use dash_chain::Status;
use dash_consensus::BlockId;
use dash_oracle_sys::{BlockAt, ChainOp, CheckResult, Event, FlushMode, Persisted, Pin, Utxo};
use dash_statedigest::{Hash256, Kind, Record, Value as RecordValue};
use dashd_chainstate::chain::ValidationInterface;
use node_l1::Divergence;
use node_l1::chain::census::{Census, Totals};
use node_l1::chain::compare::differences;
use node_l1::chain::core::{CoreSut, SPORK_19, SPORK_OFF, Schedule, T_RECENT, image_world};
use node_l1::chain::diff::{Facts, Judgement, NodeFacts, QUEUE_LIMIT, QUEUED, judge, refused};
use node_l1::chain::generate::Gen;
use node_l1::chain::intent::Program;
use node_l1::chain::node::{IndexEntry, NodeSide, NodeStep, index_digest};
use node_l1::chain::run::{ChainPlant, End, Plant, Refused, Run, Unsupported, op_kind};
use node_l1::chain::step::{ReplyClass, Step};
use node_l1::chain::ties::{LOAD_BEST_INVALID, TieSite};
use node_l1::chain::{Mode, Runner};

/// V's entries without `BLOCK_HAVE_UNDO`.
fn without_undo(entries: &[IndexEntry]) -> Vec<IndexEntry> {
    let strip = |e: &IndexEntry| IndexEntry {
        status: e.status & !Status::HAVE_UNDO,
        ..*e
    };
    entries.iter().map(strip).collect()
}

/// One op run on both sides without holding them equal: the node's step with its facts, and
/// Core's step with the facts the runner reads of it.
struct Judged {
    op: ChainOp,
    node: NodeStep,
    core: Step,
    core_facts: Facts,
}

impl Judged {
    fn refused(&self) -> Option<Refused> {
        refused(&self.node.facts, &self.core_facts)
    }

    /// The runner's judgement of `core` against `node`.
    fn judge(&self, core: &Step, node: &NodeStep) -> Judgement {
        judge(core, node, &[])
    }

    /// Core's real step standing in for the node's: a planted Core step's difference is then
    /// the only one.
    fn core_as_node(&self) -> NodeStep {
        self.as_node(&self.core)
    }

    /// `step` as a serving node's, its queued signals counted.
    fn as_node(&self, step: &Step) -> NodeStep {
        NodeStep {
            step: step.clone(),
            facts: NodeFacts {
                queued: step.events.iter().filter(|e| queued(e)).count(),
                teardown: 0,
                late: 0,
                late_records: 0,
                failed_tie: false,
            },
            proposal_class: None,
        }
    }
}

fn queued(e: &Event) -> bool {
    queued_name(&e.name)
}

fn queued_name(name: &str) -> bool {
    QUEUED.contains(&name)
}

fn diverged(field: &'static str) -> Judgement {
    Judgement::Diverged(Divergence {
        field,
    })
}

/// The ops `text` plans on the image's world, on both sides as the Diff runner runs them (the
/// world follows Core).
fn judged(core: &mut CoreSut, node: &mut NodeSide, text: &str) -> Vec<Judged> {
    let mut w = loaded_world();
    let mut generator = Gen::default();
    let mut census = Census::default();
    let mut out = Vec::new();
    for intent in &Program::parse(text).unwrap().intents {
        for p in generator.resolve(&mut w, intent, &mut census) {
            let n = node.step(&p.op).unwrap_or_else(|u| panic!("{u:?}"));
            let c = core.step(&p.op).unwrap().step;
            w.observe(&p.op, &c);
            out.push(Judged {
                core_facts: Facts::core(&c),
                op: p.op,
                node: n,
                core: c,
            });
        }
    }
    out
}

/// A listener that holds V's signal thread at every `UpdatedBlockTip`, so that the queued
/// signals after one reach the side's listener only once the step's queue sync waited for them.
struct Slow;

impl ValidationInterface for Slow {
    fn updated_block_tip(&self, _: BlockId, _: Option<BlockId>, _: bool) {
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[test]
fn the_image_is_the_same_chain_on_both_sides() {
    for schedule in [Schedule::S0, Schedule::S1] {
        let dir = private_dir();
        let (mut core, mut node) = sides(schedule, &dir);
        let (c, n, entries) = both(&mut core, &mut node, &ChainOp::SetTime(T_RECENT));
        assert_eq!(entries.len(), image_world().blocks.len());
        assert_posts(&c.post, &n.post, &entries, schedule.name());
        // V's connector wrote each connected block's undo (Core: validation.cpp:2935-2950),
        // and the digest sees the bit: without it Core's differs.
        let connected = entries.iter().filter(|e| e.height > 0);
        assert!(connected.into_iter().all(|e| e.status & Status::HAVE_UNDO == Status::HAVE_UNDO));
        assert_ne!(c.post.index, Some(index_digest(&without_undo(&entries))));
    }
}

/// Blocks with spends and a fan-out, headers first, a contextual and a context-free rejection,
/// `ACTIVATE`, `CL_ENFORCE` and the clock; then a duplicate block and header, and spork 19 off
/// and on again. Every op is held equal on both sides.
#[test]
fn an_extend_only_program_is_the_same_on_both_sides() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let steps = same_program(
        &mut core,
        &mut node,
        "extend main 2 tx=spends\n\
         extend main 2 headers\n\
         mutate cb_height\n\
         extend main 1\n\
         mutate merkle_root\n\
         extend main 1\n\
         activate\n\
         cl_enforce\n\
         time 3\n\
         extend main 1 tx=fan\n",
    );
    let mut classes: Vec<_> = steps.iter().map(|(op, s)| (op_kind(op), s.class(op))).collect();
    let block = steps
        .iter()
        .rev()
        .find_map(|(op, s)| match op {
            ChainOp::Block(bytes) if s.class(op) == ReplyClass::Ok => Some(bytes.clone()),
            _ => None,
        })
        .expect("a block connected");
    let header: [u8; 80] = block[..80].try_into().unwrap();
    let off = ChainOp::Spork {
        id: SPORK_19,
        value: SPORK_OFF,
    };
    let on = ChainOp::Spork {
        id: SPORK_19,
        value: 0,
    };
    for op in [ChainOp::Block(block), ChainOp::Header(header), off, ChainOp::ClEnforce, on] {
        let step = assert_same(&mut core, &mut node, &op);
        classes.push((op_kind(&op), step.class(&op)));
    }
    let reached = |kind: &str, class: ReplyClass| classes.contains(&(kind, class));
    assert!(reached("header", ReplyClass::Ok), "{classes:?}");
    assert!(reached("block", ReplyClass::Reject("bad-cb-height".into())), "{classes:?}");
    assert!(reached("block", ReplyClass::Reject("bad-txnmrklroot".into())), "{classes:?}");
    assert!(reached("block", ReplyClass::Duplicate), "{classes:?}");
    for kind in ["activate", "cl_enforce", "settime", "spork"] {
        assert!(reached(kind, ReplyClass::Ok), "{kind}: {classes:?}");
    }
}

/// Held blocks delivered child first, so the last delivery connects all three in one op: its
/// synchronous signals first, then the queued ones in queue order, as Core lists them. Again
/// and again from a restore, since V delivers its queued signals on another thread, which a
/// slow listener holds back so that only the step's queue sync gets them all in.
#[test]
fn a_multi_block_connect_lists_its_events_as_core_does() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    for round in 0..5 {
        if round > 0 {
            core.restore(T_RECENT).unwrap();
            node.restore(T_RECENT).unwrap();
        }
        // The queue's thread lags: without the step's queue sync the queued half arrives late.
        assert!(node.register_listener(Arc::new(Slow)));
        let steps =
            same_program(&mut core, &mut node, "extend main 3 headers hold\ndeliver b1 rev\n");
        let connected = |s: &Step| s.events.iter().filter(|e| e.name == "BlockConnected").count();
        let most = steps.iter().map(|(_, s)| connected(s)).max();
        assert_eq!(most, Some(3), "round {round}: one op connects the three");
    }
}

/// A connect-time rejection, then the chain going on: V's `CoreConnector` checks the block as
/// Core's `ConnectBlock` does, so the reply, the `BlockChecked` state and the failed entry in
/// the index are Core's.
#[test]
fn a_connect_time_rejection_is_the_same_on_both_sides() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let steps = same_program(&mut core, &mut node, "mutate cb_amount\nextend main 1\n");
    let rejected = steps.iter().find(|(op, s)| s.class(op) != ReplyClass::Ok);
    assert!(
        rejected.is_some(),
        "a rejection: {:?}",
        steps.iter().map(|(op, s)| s.class(op)).collect::<Vec<_>>()
    );
}

/// CH3a a9 (DEC-450): V checks scripts on NodeSut's queue as Core does under each schedule. A
/// bad signature, then a bad signature before a BIP68 failure: under S0 (`-par=1`) both blocks
/// get the script's own reason; under S1 (`-par=4`, 3 threads) the first gets
/// `block-validation-failed` and the second the loop's `bad-txns-nonfinal`. Every op is held
/// equal on both sides.
// Core: validation.cpp:2731-2733, :2752-2758, :2778-2781
#[test]
fn script_failures_get_each_schedule_s_reasons_on_both_sides() {
    let program = "extend main 1 tx=spends\n\
                   mutate sig_bad\n\
                   extend main 1\n\
                   mutate queued_script_then_bip68\n\
                   extend main 1\n";
    let reasons = [Schedule::S0, Schedule::S1].map(|schedule| {
        let dir = private_dir();
        let (mut core, mut node) = sides(schedule, &dir);
        let steps = same_program(&mut core, &mut node, program);
        steps
            .iter()
            .flat_map(|(_, step)| &step.reply.checked)
            .filter(|c| c.1 == CheckResult::Invalid)
            .map(|c| c.3.clone().unwrap())
            .collect::<Vec<_>>()
    });
    let [serial, threaded] = &reasons;
    assert_eq!(serial.len(), 2, "{serial:?}");
    assert!(
        serial.iter().all(|r| r.starts_with("mandatory-script-verify-flag-failed")),
        "{serial:?}"
    );
    assert_eq!(threaded, &["block-validation-failed", "bad-txns-nonfinal"]);
}

/// Five held blocks connected in one op queue ten signals, which Core's queue holds without a
/// mid-op drain: the op is served, its events Core's (the refusal rule's boundary).
#[test]
fn ten_queued_signals_are_served() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let steps = same_program(&mut core, &mut node, "extend main 5 headers hold\ndeliver b1 rev\n");
    let connected = |s: &Step| s.events.iter().filter(|e| e.name == "BlockConnected").count();
    assert_eq!(steps.iter().map(|(_, s)| connected(s)).max(), Some(5));
}

/// An op that queues more than ten signals on both sides, the same count, is refused once the
/// steps are otherwise the same: Core may have drained its queue at an `ActivateBestChain`
/// iteration inside it, and where is not the node's to say. (Six blocks queue twelve; Core
/// drains only from the seventh, so the bound is one block conservative.) With Core's real step
/// standing in for the node's, a drain inside the op (queued signals moved ahead of the last
/// synchronous one) is still the same step.
#[test]
fn a_long_connect_on_both_sides_is_refused_as_a_queue_drain() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = judged(&mut core, &mut node, "extend main 6 headers hold\ndeliver b1 rev\n");
    let long: Vec<&Judged> = ops.iter().filter(|j| j.node.facts.queued > QUEUE_LIMIT).collect();
    assert_eq!(long.len(), 1, "one op connects the six");
    let long = long[0];
    assert_eq!(long.node.facts.queued, 12);
    assert_eq!(long.core_facts.queued, 12);
    assert_eq!(long.refused(), Some(Refused::QueueDrain));
    assert!(
        ops.iter().filter(|j| j.node.facts.queued <= QUEUE_LIMIT).all(|j| j.refused().is_none())
    );
    let refused = long.judge(&long.core, &long.core_as_node());
    assert!(
        matches!(refused, Judgement::Refused(Refused::QueueDrain, s) if s.c_paired == 6),
        "{refused:?}"
    );
    let mut drained = long.core.clone();
    let first = drained.events.iter().position(queued).unwrap();
    let last_sync = drained.events.iter().rposition(|e| !queued(e)).unwrap();
    assert!(last_sync < first);
    let early: Vec<_> = drained.events.drain(first..first + 2).collect();
    drained.events.splice(last_sync..last_sync, early);
    assert_ne!(drained.events, long.core.events);
    assert_eq!(long.judge(&drained, &long.core_as_node()), refused);
}

/// A long connect whose queued signals differ is compared, and so reported as a divergence:
/// with Core's step planted two queued signals short (a count on the node only), or with two
/// of its queued signals swapped (the same count, another order), the runner reports the
/// events.
#[test]
fn a_long_connect_that_queues_differently_is_a_divergence() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = judged(&mut core, &mut node, "extend main 6 headers hold\ndeliver b1 rev\n");
    let long = ops.iter().find(|j| j.node.facts.queued > QUEUE_LIMIT).expect("the long connect");
    let mut short = long.core.clone();
    let last = short.events.iter().rposition(|e| e.name == "UpdatedBlockTip").unwrap();
    short.events.drain(last - 1..=last);
    let short_facts = Facts::core(&short);
    assert_eq!(refused(&long.node.facts, &short_facts), None);
    assert_eq!(long.judge(&short, &long.core_as_node()), diverged("events"));
    let mut swapped = long.core.clone();
    let last = swapped.events.len() - 1;
    assert_ne!(swapped.events[last - 1], swapped.events[last]);
    swapped.events.swap(last - 1, last);
    assert_eq!(long.judge(&swapped, &long.core_as_node()), diverged("events"));
}

/// After a restore, the first header is notified as Core's is: both restores are a load of the
/// saved image, which clears the last notified header tip, so a header that is not a new best
/// one is notified on both sides.
#[test]
fn the_first_header_tip_after_a_restore_is_notified_as_core_s() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let steps = same_program(&mut core, &mut node, "fork 1 1\n");
    let first = &steps.first().expect("an op").1;
    assert!(first.events.iter().any(|e| e.name == "NotifyHeaderTip"), "{:?}", first.events);
}

/// The one op kind V does not serve is refused before anything runs: `FLUSH INDEX_ONLY` (no V
/// counterpart, O2b D31).
#[test]
fn the_unsupported_op_kind_is_refused() {
    let dir = private_dir();
    let (mut node, _) = NodeSide::start(Pin::V24_0, Schedule::S0, dir.path()).unwrap();
    let before = node.step(&ChainOp::SetTime(T_RECENT)).unwrap().step.post;
    let op = ChainOp::Flush {
        mode: FlushMode::IndexOnly,
        label: String::new(),
    };
    assert_eq!(node.step(&op).err(), Some(Unsupported::new(&op, Refused::Op)), "{op:?}");
    assert_eq!(node.step(&ChainOp::SetTime(T_RECENT)).unwrap().step.post, before, "nothing ran");
}

/// `INVALIDATE`, `RECONSIDER` and `PRECIOUS` at the tip, mid-chain over spent coins, on a fork,
/// on an equal-work fork and back, and on a block neither side has: each op the same on both
/// sides, replies, events and post included.
#[test]
fn the_block_rpcs_are_the_same_on_both_sides() {
    const PROGRAMS: [&str; 6] = [
        "extend main 2\ninvalidate main@-0\n",
        "extend main 3 tx=spends\ninvalidate main@-2\nreconsider built#0\n",
        "extend main 2\nfork 1 2\ninvalidate tip#0\nreconsider tip#0\n",
        "extend main 2\nfork 1 1\nprecious tip#0\nprecious built#1\nextend main 1\n",
        "extend main 2\nfork 1 1\ninvalidate main@-0\nprecious built#1\n",
        "extend main 1\ninvalidate random\nreconsider random\nprecious random\n",
    ];
    for text in PROGRAMS {
        let dir = private_dir();
        let (mut core, mut node) = sides(Schedule::S0, &dir);
        let steps = same_program(&mut core, &mut node, text);
        let ran = steps.iter().any(|(op, _)| {
            matches!(op, ChainOp::Invalidate(_) | ChainOp::Reconsider { .. } | ChainOp::Precious(_))
        });
        assert!(ran, "{text:?}: a block RPC ran");
    }
}

/// Reorg programs: forks of depth 1 to 5, headers first and held data, each transaction shape,
/// a reorg back to the main chain, two reorgs in one input, and forks whose blocks fail to
/// connect, so that Core and V disconnect the fork again and reconnect the main chain.
const REORGS: [&str; 17] = [
    "fork 1 2\n",
    "fork 2 3\n",
    "fork 1 2 headers\n",
    "fork 2 3 hold\ndeliver b1\n",
    "fork 2 3 tx=spends\n",
    "fork 3 5 tx=fan\n",
    "fork 5 7\n",
    "extend main 1\nfork 1 1\nextend b1 1\n",
    "extend main 3 tx=spends\nfork 3 5 tx=spends\nextend b1 2\n",
    "extend main 2 tx=chain\nfork 2 4 tx=chain\nextend main 3 tx=fan\n",
    "fork 1 2\nfork 1 2\n",
    "fork 1 2\nactivate\ncl_enforce\n",
    "mutate cb_amount\nfork 1 2\n",
    "extend main 1\nfork 2 2\nmutate cb_amount\nextend b1 2\n",
    "extend main 2 tx=spends\nfork 2 2 tx=spends\nmutate cb_amount\nextend b1 1\nextend b1 1\n",
    "extend main 1 tx=spends\nmutate queued_script_then_bip68\nfork 2 3 tx=spends\n",
    "fork 1 1\n",
];

/// Where `step`'s first `name` signal is.
fn signal_at(step: &Step, name: &str) -> usize {
    step.events.iter().position(|e| e.name == name).unwrap()
}

/// Every plane in which one op's steps differ.
fn differing(j: &Judged) -> Vec<&'static str> {
    differences(&j.core, &j.node.step, &[]).1
}

/// V disconnects through CH3a a5's `DisconnectBlock`, and a reorg is compared plane by plane as
/// any other op: over every reorg program, V serves every op, and each op's steps are the same
/// in every plane, the reorg's records and decisions, reply, `BlockChecked` list, events, tip,
/// best header, `index` digest (undo bits included) and chain tips included. The one difference
/// is the declared replay-latch gap: an input's first op, whose header Core notifies as a new
/// header tip after its load where V's replay does not.
#[test]
fn a_reorg_is_compared_plane_by_plane() {
    for text in REORGS {
        let dir = private_dir();
        let (mut core, mut node) = sides(Schedule::S0, &dir);
        let ops = judged(&mut core, &mut node, text);
        let disconnected =
            |j: &&Judged| j.core.events.iter().any(|e| e.name == "BlockDisconnected");
        let reorgs = ops.iter().filter(disconnected).count();
        assert_eq!(reorgs > 0, text != "fork 1 1\n", "{text:?}: reorgs");
        for (k, j) in ops.iter().enumerate() {
            let what = format!("{text:?} op {k} ({})", op_kind(&j.op));
            assert!(!j.node.step.post.down, "{what}: V serves");
            let mut differ = differing(j);
            if k == 0 && differ.contains(&"events") {
                let mut notified = j.node.step.events.clone();
                let at = j.core.events.iter().position(|e| e.name == "NotifyHeaderTip");
                let at = at.unwrap_or_else(|| panic!("{what}: events"));
                notified.insert(at, j.core.events[at].clone());
                assert_eq!(notified, j.core.events, "{what}: the replay-latch gap only");
                differ.retain(|f| *f != "events");
            }
            assert!(differ.is_empty(), "{what}: {differ:?}");
        }
    }
}

/// A reorg on one side only is the divergence it is: Core's reorg step planted as no reorg (its
/// `BlockDisconnected` gone, its tip the one before) against V's real reorg, and Core's real
/// reorg against that planted step as a serving node's.
#[test]
fn a_reorg_on_one_side_only_is_a_divergence() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = judged(&mut core, &mut node, "fork 1 2\n");
    let [before, reorg] = &ops[..] else {
        panic!("two ops");
    };
    let mut no_reorg = reorg.core.clone();
    no_reorg.events.retain(|e| e.name != "BlockDisconnected");
    no_reorg.post.tip = before.core.post.tip;
    assert_ne!(no_reorg, reorg.core);
    assert_eq!(reorg.judge(&no_reorg, &reorg.node), diverged("events"), "the node's only");
    assert_eq!(
        reorg.judge(&reorg.core, &reorg.as_node(&no_reorg)),
        diverged("events"),
        "Core's only"
    );
    let mut tip_only = reorg.core.clone();
    tip_only.post.tip = before.core.post.tip;
    assert_eq!(reorg.judge(&tip_only, &reorg.node), diverged("post.tip"), "the tip only");
}

/// Each plane of V's real reorg step, the same as Core's, is compared: a record's field, a
/// decision left out, a signal left out, moved or added, a check left out, and a planted tip,
/// best header, `index` bit or chain tips are each the divergence, by that plane's name.
#[test]
fn each_plane_of_a_reorg_is_compared() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = judged(&mut core, &mut node, "fork 1 2\n");
    let reorg = ops.last().expect("an op");
    assert_eq!(differing(reorg), Vec::<&str>::new());
    let names = |s: &Step| s.events.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    assert!(names(&reorg.node.step).contains(&"BlockDisconnected".to_owned()));
    let entries = node.index().expect("V serves");
    let tip = entries.iter().map(|e| e.height).max().unwrap();
    let mut flipped = entries.clone();
    flipped.iter_mut().find(|e| e.height == tip).unwrap().status ^= Status::HAVE_UNDO;
    let flipped = Some(index_digest(&flipped));
    let plants: [(&str, &str, &StepChange); 12] = [
        ("a connect's ntx", "records", &|s| {
            let c = s.records.iter_mut().find_map(|r| match r {
                Record::Outcome(o) if o.kind() == Kind::Connect => Some(o),
                _ => None,
            });
            c.unwrap().s_mut().set("ntx", RecordValue::Int(99)).unwrap();
        }),
        ("a decision left out", "attempts", &|s| {
            let at = s.records.iter().rposition(|r| matches!(r, Record::Attempt(_))).unwrap();
            s.records.remove(at);
        }),
        ("NotifyHeaderTip left out", "events", &|s| {
            s.events.remove(signal_at(s, "NotifyHeaderTip"));
        }),
        ("BlockDisconnected left out", "events", &|s| {
            s.events.remove(signal_at(s, "BlockDisconnected"));
        }),
        ("BlockDisconnected after a BlockConnected", "events", &|s| {
            let (d, c) = (signal_at(s, "BlockDisconnected"), signal_at(s, "BlockConnected"));
            s.events.swap(d, c);
        }),
        ("an extra BlockDisconnected", "events", &|s| {
            let d = s.events[signal_at(s, "BlockDisconnected")].clone();
            s.events.push(d);
        }),
        ("a check left out", "reply", &|s| {
            s.reply.checked.pop();
        }),
        ("the tip", "post.tip", &|s| s.post.tip = None),
        ("the best header", "post.best_header", &|s| s.post.best_header = Some(None)),
        ("an undo bit", "post.index", &move |s| s.post.index = flipped),
        ("the chain tips", "post.tips", &|s| s.post.tips = Some(Vec::new())),
        ("the deployments", "post.deployments", &|s| {
            s.post.deployments = Some(serde_json::json!({"planted": true}))
        }),
    ];
    for (what, field, plant) in plants {
        let mut planted = reorg.node.clone();
        plant(&mut planted.step);
        assert_ne!(planted.step, reorg.node.step, "{what}: the plant changes the step");
        assert_eq!(reorg.judge(&reorg.core, &planted), diverged(field), "{what}");
    }
}

/// V's real step of a connect is Core's in every plane, its `C` and `A` included, and a planted
/// difference is named by its own plane: the `C` missing `undo` (CH3a a10 M-1), an `index` bit
/// and the chain tips.
#[test]
fn a_connect_is_the_same_and_a_planted_difference_is_named_by_its_plane() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = judged(&mut core, &mut node, "extend main 1\n");
    let connect = ops.last().expect("an op");
    assert!(matches!(connect.judge(&connect.core, &connect.node), Judgement::Same(_)));
    let kinds = |s: &Step| s.records.iter().filter(|r| !matches!(r, Record::Header(_))).count();
    assert_eq!(kinds(&connect.node.step), 2, "a C and its A");
    let mut no_undo = connect.node.clone();
    for r in &mut no_undo.step.records {
        if let Record::Outcome(o) = r {
            o.s_mut().remove("undo").expect("the C has undo");
        }
    }
    assert_eq!(connect.judge(&connect.core, &no_undo), diverged("records"), "undo missing");
    let entries = node.index().expect("V serves");
    let tip = entries.iter().map(|e| e.height).max().unwrap();
    let mut planted = entries.clone();
    planted.iter_mut().find(|e| e.height == tip).unwrap().status ^= Status::HAVE_UNDO;
    let mut index = connect.node.clone();
    index.step.post.index = Some(index_digest(&planted));
    assert_ne!(index.step.post.index, connect.core.post.index);
    assert_eq!(connect.judge(&connect.core, &index), diverged("post.index"));
    let mut tips = connect.node.clone();
    tips.step.post.tips = Some(Vec::new());
    assert_eq!(connect.judge(&connect.core, &tips), diverged("post.tips"));
}

/// A change to one step.
type StepChange = dyn Fn(&mut Step);

/// Every plane of a step is compared, none excepted: with Core's real step of a connect
/// standing in for the node's, a node-only difference in any plane is the divergence, by that
/// plane's name.
#[test]
fn every_plane_of_a_step_is_compared() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = judged(&mut core, &mut node, "extend main 1\n");
    let connect = ops.last().expect("an op");
    assert!(!connect.core.records.is_empty() && connect.core.post.utxo.is_some());
    let same = connect.judge(&connect.core, &connect.core_as_node());
    assert!(matches!(same, Judgement::Same(_)), "{same:?}");
    let planted = |plant: &StepChange| {
        let mut node = connect.core_as_node();
        plant(&mut node.step);
        assert_ne!(node.step, connect.core, "the plant changes the step");
        connect.judge(&connect.core, &node)
    };
    let index = connect.core.post.index.expect("Core's index");
    let plants: [(&str, &StepChange); 18] = [
        ("reply", &|s| s.reply.outcome = Ok("planted".into())),
        ("records", &|s| s.records.clear()),
        ("events", &|s| {
            s.events.pop();
        }),
        ("post.down", &|s| s.post.down = !s.post.down),
        ("post.flush_delta", &|s| {
            s.post.flush_delta.full.insert("planted".into(), 1);
        }),
        ("post.persisted", &move |s| {
            s.post.persisted = Some(Persisted {
                best_block: None,
                evo_best: None,
                failed: Vec::new(),
                head_blocks: None,
                index,
                index_entries: 0,
            })
        }),
        ("post.tip", &|s| s.post.tip = None),
        ("post.best_header", &|s| s.post.best_header = Some(None)),
        ("post.best_invalid", &|s| {
            s.post.best_invalid = if s.post.best_invalid.is_some() {
                None
            } else {
                Some(None)
            }
        }),
        ("post.candidates", &|s| {
            s.post.candidates = if s.post.candidates.is_some() {
                None
            } else {
                Some(Vec::new())
            }
        }),
        ("post.utxo", &|s| s.post.utxo = None),
        ("post.index", &|s| s.post.index = None),
        ("post.flushes", &|s| s.post.flushes = None),
        ("post.ibd", &|s| s.post.ibd = s.post.ibd.map(|ibd| !ibd).or(Some(true))),
        ("post.best_cl", &|s| {
            s.post.best_cl = if s.post.best_cl == Some(None) {
                None
            } else {
                Some(None)
            }
        }),
        ("post.sporks", &|s| s.post.sporks = Some(Vec::new())),
        ("post.tips", &|s| s.post.tips = Some(Vec::new())),
        ("post.deployments", &|s| s.post.deployments = Some(serde_json::json!({"planted": true}))),
    ];
    for (field, plant) in plants {
        assert_eq!(planted(plant), diverged(field), "{field}");
    }
}

/// Diff mode runs a connect and a ChainLock's record to the end, every plane the same, the
/// connect's `C` with its unwritten keys checked (OD-L3-74) and the lock's step without
/// records. A difference ends an input with its plane's name ([`judge`]'s tests). (Not through
/// `fuzz_target`: its session writes the census file into the temporary directory when the
/// thread ends.)
#[test]
fn diff_mode_runs_a_connect_and_a_lock_to_the_end() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    for (text, pairs) in [("extend main 1\n", Some(&1)), ("cl main@-0\n", None)] {
        let run = runner.run(&Program::parse(text).unwrap()).unwrap();
        assert_eq!(run.end, End::Done, "{text:?}");
        assert_eq!(run.census.rows.get("unwritten checked"), pairs, "{text:?}: the C's keys");
    }
}

/// Step 0 (BL-283, Q9): an input with no ops ends `Done` from either restore, the reset's at
/// `T_RECENT` (both sides' kept steps) and a fresh one at `T_OLD` (`start old`), and its
/// restores were judged: the `org=verifydb` records judging skips in them are counted.
#[test]
fn an_input_without_ops_ends_done_on_judged_restores() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    for text in ["", "start old\n", ""] {
        let run = runner.run(&Program::parse(text).unwrap()).unwrap();
        assert_eq!((&run.end, &run.kinds[..]), (&End::Done, &["restore"][..]), "{text:?}");
        let verifydb = run.census.rows.get("verifydb").copied().unwrap_or(0);
        println!("{text:?}: verifydb {verifydb}");
        assert!(verifydb > 0, "{text:?}: step 0 judged");
    }
}

/// S0/S1's domain on an input's record census (OD-L3-74 (3), OD-L3-77): every paired `C`
/// carried V's evo keys, and no evo value left its `null` era.
fn evo_domain_held(census: &Census) -> Result<(), String> {
    let row = |r| census.rows.get(r).copied().unwrap_or(0);
    let (cs, evo, non_null) = (row("paired Cs"), row("evo compared"), row("evo non-null"));
    if evo != cs {
        return Err(format!("{evo} of {cs} paired Cs with V's evo keys (OD-L3-77)"));
    }
    if non_null > 0 {
        return Err(format!("{non_null} evo values past their null era (OD-L3-74 (3))"));
    }
    Ok(())
}

/// Sol r1 m-1, D49: a queue drain's census keeps the records the refused step compared: the long
/// connect's six `C`s, paired with V's evo keys. The refused op is counted among the ops only as
/// out of domain, and the input runs to its end past it.
#[test]
fn a_queue_drain_s_compared_records_are_counted() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let text = "extend main 6 headers hold\ndeliver b1 rev\n";
    let run = runner.run(&Program::parse(text).unwrap()).unwrap();
    assert_eq!(run.end, End::Done);
    let [(at, kind)] = &run.partial[..] else {
        panic!("{:?}", run.partial);
    };
    assert_eq!((*at, *kind), (run.kinds.len() - 1, "block"), "the last delivery");
    let row = |r| run.census.rows.get(r).copied().unwrap_or(0);
    let counts = (row("unwritten checked"), row("paired Cs"), row("evo compared"));
    assert_eq!(counts, (6, 6, 6), "the six Cs");
    evo_domain_held(&run.census).unwrap();
    let of_kind = |key: Option<&str>| -> u64 {
        let wanted = |(k, c): &&(&str, &str)| k == kind && key.is_none_or(|key| *c == key);
        run.census.ops.iter().filter(|(k, _)| wanted(k)).map(|(_, n)| n).sum()
    };
    let ran = u64::try_from(run.kinds.iter().filter(|k| **k == *kind).count()).unwrap();
    assert_eq!(of_kind(Some("unsupported-queue-drain")), 1, "{:?}", run.census.ops);
    assert_eq!(of_kind(None), ran, "the refused op once, out of domain: {:?}", run.census.ops);
}

/// Sol r1 m-1: an equal non-null evo value on both sides of a queue drain's step is the same, so
/// the refusal stands, and the census it carries fails S0/S1's domain guard.
#[test]
fn a_non_null_evo_value_on_a_queue_drain_fails_the_domain_guard() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = judged(&mut core, &mut node, "extend main 6 headers hold\ndeliver b1 rev\n");
    let long = ops.iter().find(|j| j.node.facts.queued > QUEUE_LIMIT).expect("the long connect");
    let mut planted = long.core.clone();
    let c = planted
        .records
        .iter_mut()
        .find_map(|r| match r {
            Record::Outcome(o) if o.kind() == Kind::Connect => Some(o),
            _ => None,
        })
        .expect("a C");
    let mnl = RecordValue::hash(&Hash256::from_wire_bytes([9; 32]));
    c.s_mut().set("mnl", mnl).expect("a value of the key's shape");
    let judged = long.judge(&planted, &long.as_node(&planted));
    let Judgement::Refused(Refused::QueueDrain, skipped) = judged else {
        panic!("{judged:?}");
    };
    assert_eq!((skipped.c_paired, skipped.evo_non_null), (6, 1));
    let mut census = Census::default();
    census.skipped(skipped);
    assert!(evo_domain_held(&census).is_err());
}

/// `n` inputs from a fixed xorshift stream, 8 to 207 bytes each.
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

/// Diff mode over `diff_chain`'s public seeds and 50 generated inputs (DEC-504), the tier-1
/// check in place of a Diff-mode fuzz target: with no gap plane left for S0/S1's op domain (CH3a
/// a10-3), each input ends done, every op the same; tie-discarded, uncompared (O2b D30); or
/// refused as a queue drain both sides show. A divergence or an op the node does not serve
/// fails. An input may start with a clean `RESTART`, whose teardown's events need the oracle at
/// ABI 33 (the M-4 shim fix). Each input's end and the record pairs whose unwritten keys the
/// records plane checked (OD-L3-74) are printed, with its paired `C`s and how many carried V's
/// evo keys, which is every one (OD-L3-77). The corpus's aggregate is pinned, so that no input
/// end, tie or empty record stream passes unseen (Opus r1 m-1): a change to the corpus (the public
/// seeds, `generated`'s stream or its count) must update these numbers.
///
/// O2b-4 D49 changed the pin from {done 57, queue drain 8} and (506, 302): a queue drain both
/// sides show is now a partial refusal, not an end, so the eight inputs that ended on one run on
/// to `Done` past it (ten refused ops: three blocks, five `INVALIDATE`s, a `CL_ENFORCE` and a
/// `RESTART`; none diverges after its refusal), and the records their later steps compared join
/// the totals. The ends are unchanged otherwise.
///
/// OD-L3-97 changed it from {done 57, done past a queue drain 8}: `gen27`'s last report, a
/// `RESTART`, loads two failed entries at the most failed work, where Core's `best_invalid` is
/// its block map's order, so it is now discarded at harness site `h1`. Both sides happened to
/// hold the same entry, so it ended done before. The record totals are unchanged (an input's
/// rows count up to its end, and the tie was its last report).
#[test]
fn diff_mode_ends_each_input_done() {
    if !at_abi_33("diff_mode_ends_each_input_done", "the M-4 clean teardown's events") {
        return;
    }
    let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("../node-fuzz/public-seeds/diff_chain");
    let mut inputs: Vec<(String, Vec<u8>)> = std::fs::read_dir(&seeds)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())
        })
        .collect();
    inputs.sort();
    assert!(!inputs.is_empty(), "the public seeds");
    inputs.extend(generated(50));
    assert_eq!(inputs.len(), 65, "15 public seeds and 50 generated inputs");
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let mut ends: BTreeMap<String, usize> = BTreeMap::new();
    let mut partials: BTreeMap<&str, usize> = BTreeMap::new();
    let mut totals = (0, 0);
    let mut tie_sites = Vec::new();
    for (name, bytes) in &inputs {
        let program = Program::decode_v1(bytes);
        let run = runner.run(&program).unwrap();
        let end = match &run.end {
            End::Done if run.partial.is_empty() => "done",
            End::Done => "done past a queue drain",
            End::Tainted(op, sites) => {
                tie_sites.push((name.clone(), *op, sites.clone()));
                "tie"
            }
            End::Unsupported(Unsupported {
                why: Refused::QueueDrain,
                ..
            }) => "queue drain",
            other => panic!("{name}: {other:?} after {:?}\n{program}", run.kinds),
        };
        let row = |r| run.census.rows.get(r).copied().unwrap_or(0);
        let (checked, cs, evo) = (row("unwritten checked"), row("paired Cs"), row("evo compared"));
        println!(
            "diff mode input {name}: {end}, {checked} record pairs' unwritten keys checked, {evo} \
             of {cs} paired Cs with V's evo keys, partial refusals {:?} of {} reports",
            run.partial,
            run.kinds.len()
        );
        for &(_, kind) in &run.partial {
            *partials.entry(kind).or_default() += 1;
        }
        evo_domain_held(&run.census).unwrap_or_else(|e| panic!("{name}: {e}"));
        *ends.entry(end.to_owned()).or_default() += 1;
        totals = (totals.0 + checked, totals.1 + cs);
    }
    println!(
        "diff mode: {ends:?}, partial refusals {partials:?}, record pairs and paired Cs {totals:?}"
    );
    let h1 = vec![("gen27".to_owned(), "restart", vec![LOAD_BEST_INVALID])];
    assert_eq!(tie_sites, h1, "one input discarded, on harness site h1");
    let pinned = BTreeMap::from([
        ("done".to_owned(), 56),
        ("done past a queue drain".to_owned(), 8),
        ("tie".to_owned(), 1),
    ]);
    assert_eq!(ends, pinned, "the corpus's ends");
    let refused =
        BTreeMap::from([("block", 3), ("cl_enforce", 1), ("invalidate", 5), ("restart", 1)]);
    assert_eq!(partials, refused, "the corpus's partial refusals");
    assert_eq!(totals, (595, 328), "the corpus's record pairs and paired Cs");
}

/// In Diff mode, `INVALIDATE`, `RECONSIDER`, `PRECIOUS` and `CL_RECORD` as an input's first op
/// are served and the same on both sides: `INVALIDATE` of the tip with its disconnect's record,
/// the other two, which move nothing, and a lock on the tip. The generator reconsiders only the
/// input's own invalidations, so a first-op `RECONSIDER` names a block neither side has
/// (`random`).
#[test]
fn diff_mode_serves_the_block_rpcs_and_cl_record() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let mut end = |text: &str| runner.run(&Program::parse(text).unwrap()).unwrap().end;
    let texts =
        ["invalidate main@-0\n", "reconsider random\n", "precious main@-0\n", "cl main@-0\n"];
    assert_eq!(texts.map(&mut end), [End::Done, End::Done, End::Done, End::Done], "{texts:?}");
}

/// A difference in the UTXO set alone on V's real step, otherwise Core's, is named `post.utxo`:
/// on a connect and on a reorg (DEC-505).
#[test]
fn a_utxo_difference_is_named_post_utxo() {
    for (text, reorg) in [("extend main 1\n", false), ("fork 1 2\n", true)] {
        let dir = private_dir();
        let (mut core, mut node) = sides(Schedule::S0, &dir);
        let ops = judged(&mut core, &mut node, text);
        let step = ops.last().expect("an op");
        let disconnects = step.core.events.iter().any(|e| e.name == "BlockDisconnected");
        assert_eq!(disconnects, reorg, "{text:?}: a reorg");
        assert_eq!(differing(step), Vec::<&str>::new(), "{text:?}");
        let mut planted = step.node.clone();
        let utxo = planted.step.post.utxo.take().expect("the node reads its coins");
        planted.step.post.utxo = Some(Utxo {
            amount: utxo.amount + 1,
            ..utxo
        });
        assert_eq!(step.judge(&step.core, &planted), diverged("post.utxo"), "{text:?}");
    }
}

/// One Diff-mode run of `text` with `plant` (D49's injections), the runner left unplanted.
fn planted_run(runner: &mut Runner, text: &str, plant: Option<Plant>) -> Run {
    runner.plant(plant);
    let run = runner.run(&Program::parse(text).unwrap()).unwrap();
    runner.plant(None);
    run
}

/// A connect, the six-block connect both sides show as a queue drain, and a connect after it.
const AROUND_A_DRAIN: &str =
    "extend main 1\nextend main 6 headers hold\ndeliver b1 rev\nextend main 1\n";

/// The ops of `kind` the census counts, by class.
fn ops_of<'a>(census: &'a Census, kind: &str) -> BTreeMap<&'a str, u64> {
    census.ops.iter().filter(|((k, _), _)| *k == kind).map(|((_, c), n)| (*c, *n)).collect()
}

/// The single partial refusal of `run`: its report and kind.
fn the_refusal(run: &Run) -> (usize, &'static str) {
    let [refusal] = run.partial[..] else {
        panic!("one partial refusal: {:?}", run.partial);
    };
    refusal
}

/// D49 (§4.4): a long connect both sides show as a queue drain no longer ends the input. It is
/// counted only as `block:unsupported-queue-drain`, and the connect after it is scored: every
/// other block is counted by its class.
#[test]
fn a_refused_long_connect_is_followed_by_a_scored_op() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let run = planted_run(&mut runner, AROUND_A_DRAIN, None);
    assert_eq!(run.end, End::Done);
    let (at, kind) = the_refusal(&run);
    assert_eq!((kind, run.kinds[at]), ("block", "block"));
    assert_eq!(run.kinds.last(), Some(&"block"), "a connect after the refusal: {:?}", run.kinds);
    assert!(at < run.kinds.len() - 1);
    let blocks = u64::try_from(run.kinds.iter().filter(|k| **k == "block").count()).unwrap();
    let ops = ops_of(&run.census, "block");
    assert_eq!(ops.get("unsupported-queue-drain"), Some(&1), "{ops:?}");
    assert_eq!(ops.values().sum::<u64>(), blocks, "the refused op once, out of domain: {ops:?}");
    assert!(ops.get("ok").is_some_and(|&n| n >= 2), "the connects around it scored: {ops:?}");
}

/// D49: a `RESTART` both sides show as a queue drain (a generated input's, from
/// `diff_mode_ends_each_input_done`'s corpus, cut after it) is a partial refusal too, counted
/// only as `restart:unsupported-queue-drain`, and the connect after it is scored.
#[test]
fn a_refused_restart_is_followed_by_a_scored_op() {
    let text = "deliver b8\nwarp 0\ninvalidate main@-22\nflush #169\npropose tip#12 #100\nactivate\n\
                fork 167 218 headers signal tx=chain flags+6\nrestart #177\nextend main 1\n";
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let run = planted_run(&mut runner, text, None);
    assert_eq!(run.end, End::Done, "{:?}", run.kinds);
    let restart = run.partial.iter().find(|(_, k)| *k == "restart").expect("the restart refused");
    assert_eq!(run.kinds[restart.0], "restart");
    assert_eq!(run.kinds.last(), Some(&"block"), "a connect after the refusal: {:?}", run.kinds);
    let restarts = u64::try_from(run.kinds.iter().filter(|k| **k == "restart").count()).unwrap();
    let ops = ops_of(&run.census, "restart");
    assert_eq!(ops.get("unsupported-queue-drain"), Some(&1), "{ops:?}");
    assert_eq!(ops.values().sum::<u64>(), restarts, "{ops:?}");
    assert!(ops_of(&run.census, "block").contains_key("ok"), "{:?}", run.census.ops);
}

/// A change to a node's step ([`Plant::node`]).
type NodeChange = fn(&mut NodeStep);

/// D49: past a partial refusal the input is still compared. A difference planted in the node's
/// step of the connect before the refused one, or of the connect after it, is the divergence,
/// by its plane: a reply, a record, an event within the step's sequence, a post field.
#[test]
fn differences_around_a_partial_refusal_diverge() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let run = planted_run(&mut runner, AROUND_A_DRAIN, None);
    let (refused, _) = the_refusal(&run);
    let before = run.kinds.iter().position(|k| *k == "block").unwrap();
    let after = run.kinds.len() - 1;
    assert!(before < refused && refused < after && run.kinds[after] == "block");
    let plants: [(&str, NodeChange); 4] = [
        ("reply", |n| n.step.reply.outcome = Ok("planted".into())),
        ("records", |n| n.step.records.clear()),
        ("events", |n| {
            assert!(n.step.events.len() >= 3, "an event within the sequence");
            n.step.events.remove(n.step.events.len() / 2);
        }),
        ("post.tip", |n| n.step.post.tip = None),
    ];
    for at in [before, after] {
        for (field, node) in plants {
            let plant = Plant {
                node,
                ..Plant::at(at)
            };
            let run = planted_run(&mut runner, AROUND_A_DRAIN, Some(plant));
            let want = End::Diverged(Divergence {
                field,
            });
            assert_eq!(run.end, want, "{field} at report {at} (refusal at {refused})");
            assert_eq!(run.kinds.len(), at + 1, "the input ends at the planted step");
        }
    }
}

/// Twelve queued signals added to a step: past [`QUEUE_LIMIT`].
fn queue_twelve(step: &mut Step) {
    let name = QUEUED[0].to_owned();
    step.events.extend((0..12).map(|i| Event {
        name: name.clone(),
        args: vec![serde_json::Value::from(i)],
    }));
}

/// [`queue_twelve`] on a node's step, its facts counting them.
fn node_queue_twelve(node: &mut NodeStep) {
    queue_twelve(&mut node.step);
    node.facts.queued = node.step.events.iter().filter(|e| queued(e)).count();
}

/// D49: a queue drain both sides show at step 0 (planted: twelve queued signals on both
/// restores) is a partial refusal as an op's, of kind `restore`; the input goes on and its
/// connect is scored.
#[test]
fn a_refused_step_0_is_a_partial_refusal() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let plant = Plant {
        core: queue_twelve,
        node: node_queue_twelve,
        ..Plant::at(0)
    };
    let run = planted_run(&mut runner, "extend main 1\n", Some(plant));
    assert_eq!(run.end, End::Done);
    assert_eq!(run.partial, [(0, "restore")]);
    assert_eq!(ops_of(&run.census, "restore"), BTreeMap::from([("unsupported-queue-drain", 1)]));
    assert!(ops_of(&run.census, "block").contains_key("ok"), "{:?}", run.census.ops);
    let unplanted = planted_run(&mut runner, "extend main 1\n", None);
    assert!(unplanted.partial.is_empty() && ops_of(&unplanted.census, "restore").is_empty());
}

/// D49 leaves the other ends as they were: past a partial refusal, an op the node refuses before
/// running it ([`Refused::Op`]) still ends the input, and so does a tie, even on the step a queue
/// drain would refuse (the taint is read first).
#[test]
fn a_pre_execution_refusal_and_a_tie_still_end_the_input() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let run = planted_run(&mut runner, AROUND_A_DRAIN, None);
    let (refused, _) = the_refusal(&run);
    let after = run.kinds.len() - 1;
    let plant = Plant {
        refuse: true,
        ..Plant::at(after)
    };
    let unserved = planted_run(&mut runner, AROUND_A_DRAIN, Some(plant));
    let want = End::Unsupported(Unsupported {
        kind: "block",
        why: Refused::Op,
    });
    assert_eq!(unserved.end, want);
    assert_eq!(unserved.kinds.len(), after, "nothing ran the op");
    assert_eq!(unserved.partial, [(refused, "block")]);
    assert_eq!(ops_of(&unserved.census, "block").get("unsupported"), Some(&1));
    let plant = Plant {
        taint: true,
        ..Plant::at(refused)
    };
    let tied = planted_run(&mut runner, AROUND_A_DRAIN, Some(plant));
    assert!(matches!(tied.end, End::Tainted("block", _)), "{:?}", tied.end);
    assert_eq!(tied.kinds.len(), refused + 1);
    assert!(tied.partial.is_empty(), "the tie, not the drain: {:?}", tied.partial);
}

/// The node's post `best_invalid` as its tip, which is never a failed entry: a planted
/// difference in that plane alone.
fn best_invalid_as_tip(node: &mut NodeStep) {
    node.step.post.best_invalid = Some(node.step.post.tip);
}

/// A planted difference in the `tip` plane, which is read before `best_invalid`.
fn tip_one_higher(node: &mut NodeStep) {
    node.step.post.tip = node.step.post.tip.map(|BlockAt(height, hash)| BlockAt(height + 1, hash));
}

/// The last report of `run`, which must be its `RESTART`'s.
fn the_restart(run: &Run) -> usize {
    let at = run.kinds.len() - 1;
    assert_eq!(run.kinds[at], "restart", "{:?}", run.kinds);
    at
}

/// OD-L3-97 (a): the BL-223 trial's two inputs, whose `RESTART` loads two failed entries at the
/// most failed work, end tie-discarded at harness site `h1`, never diverged, and the census
/// counts them under `h1` alone, no numeric (Core) site with it. A difference in another plane
/// of that step still diverges by its name: then nothing is a tie, and the first plane that
/// differs is named.
#[test]
fn a_load_with_tied_failed_entries_is_discarded_at_harness_site_h1() {
    let text = include_str!("data/chain-v2-load-tie.txt");
    let inputs: Vec<Vec<u8>> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            (0..l.len()).step_by(2).map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap()).collect()
        })
        .collect();
    assert_eq!(inputs.len(), 2);
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let mut totals = Totals::default();
    for bytes in &inputs {
        let program = Program::decode_v2(bytes);
        let run = runner.run(&program).unwrap();
        assert_eq!(run.end, End::Tainted("restart", vec![LOAD_BEST_INVALID]), "{program}");
        assert!(run.partial.is_empty());
        let at = the_restart(&run);
        totals.input(run.census.clone(), run.end.discard());
        runner.plant(Some(Plant {
            node: tip_one_higher,
            ..Plant::at(at)
        }));
        let planted = runner.run(&program).unwrap();
        runner.plant(None);
        assert_eq!(
            planted.end,
            End::Diverged(Divergence {
                field: "post.tip"
            }),
            "{program}"
        );
    }
    assert_eq!(LOAD_BEST_INVALID.to_string(), "h1");
    assert_eq!(totals.discard_sites, BTreeMap::from([(LOAD_BEST_INVALID, 2)]), "h1 only");
    assert!(!totals.discard_sites.keys().any(|s| matches!(s, TieSite::Core(_))));
    assert_eq!(totals.discard_ops, BTreeMap::from([("restart", 2)]));
}

/// OD-L3-97 (b), (c): a load whose failed entries have no tie at the most failed work (none, one,
/// or two at different work) is compared in `best_invalid` as before: the run ends done, and a
/// planted difference there diverges. `failed` is how many failed records Core's storage held
/// before the load, which leaves them all failed (`best_invalid` is set exactly when any is).
#[test]
fn a_load_without_a_failed_entry_tie_compares_best_invalid() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let cases: [(&str, usize); 3] = [
        ("extend main 2\nflush always\nrestart clean\n", 0),
        ("extend main 3\ninvalidate main@-0\nextend main 2\nflush always\nrestart clean\n", 1),
        ("extend main 3\ninvalidate main@-1\nextend main 3\nflush always\nrestart clean\n", 2),
    ];
    for (text, failed) in cases {
        let run = planted_run(&mut runner, text, None);
        assert_eq!(run.end, End::Done, "{text:?}: {:?}", run.kinds);
        let mut totals = Totals::default();
        totals.input(run.census.clone(), run.end.discard());
        assert_eq!(totals.discard_sites.get(&LOAD_BEST_INVALID), None, "{text:?}: no h1");
        let at = the_restart(&run);
        runner.plant(Some(Plant {
            core: |step| {
                let failed = step.post.persisted.as_ref().expect("a load").failed.len();
                let best_invalid = step.post.best_invalid.expect("ABI 33");
                LOADED.with(|l| l.set(Some((failed, best_invalid.is_some()))));
            },
            node: best_invalid_as_tip,
            ..Plant::at(at)
        }));
        let planted = runner.run(&Program::parse(text).unwrap()).unwrap();
        runner.plant(None);
        let core = LOADED.with(std::cell::Cell::take);
        assert_eq!(core, Some((failed, failed > 0)), "{text:?}: failed records, best_invalid");
        assert_eq!(
            planted.end,
            End::Diverged(Divergence {
                field: "post.best_invalid"
            }),
            "{text:?}"
        );
    }
}

/// OD-L3-97's scope (review r1 m1): `h1` is a load's only. Two failed entries at the greatest
/// failed height in one session, the second failed by a step that loads nothing, are compared in
/// `best_invalid`: both sides keep the first failed entry there, so the run ends done with no
/// `h1`, and a planted difference at that step diverges by its name.
#[test]
fn a_failed_entry_tie_off_a_load_compares_best_invalid() {
    let text = "extend main 2\nfork 1 1\ninvalidate main@-0\ninvalidate main@-0\n";
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let run = planted_run(&mut runner, text, None);
    assert_eq!(run.end, End::Done, "{:?}", run.kinds);
    assert!(run.partial.is_empty(), "{:?}", run.partial);
    let mut totals = Totals::default();
    totals.input(run.census.clone(), run.end.discard());
    assert_eq!(totals.discard_sites.get(&LOAD_BEST_INVALID), None, "no h1");
    let at = run.kinds.len() - 1;
    assert_eq!(run.kinds[at], "invalidate", "{:?}", run.kinds);
    runner.plant(Some(Plant {
        core: |step| OFF_LOAD.with(|l| l.set(Some(step.post.persisted.is_some()))),
        node: |node| {
            assert!(node.facts.failed_tie, "two failed entries at the greatest failed height");
            best_invalid_as_tip(node);
        },
        ..Plant::at(at)
    }));
    let planted = runner.run(&Program::parse(text).unwrap()).unwrap();
    runner.plant(None);
    assert_eq!(OFF_LOAD.with(std::cell::Cell::take), Some(false), "the step loads nothing");
    assert_eq!(
        planted.end,
        End::Diverged(Divergence {
            field: "post.best_invalid"
        })
    );
}

thread_local! {
    /// What a plant's Core hook read of a load: its failed records, and whether `best_invalid`
    /// is set.
    static LOADED: std::cell::Cell<Option<(usize, bool)>> = const { std::cell::Cell::new(None) };
    /// Whether a plant's Core hook saw the step load (`persisted`).
    static OFF_LOAD: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// The replies a witness's plant hook saw at its block step, Core's then the node's.
static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn seen(outcome: &Result<serde_json::Value, dash_oracle_sys::RpcError>) {
    SEEN.lock().unwrap().push(witness::reply_text(outcome));
}

/// O2b-4 T4, W-P3a unplanted: on S1 the block with a queued script failure and a BIP68 spend
/// in the loop is rejected `bad-txns-nonfinal` on both sides, the loop's reason first (the
/// queue's verdict is ignored, `validation.cpp:2742-2744`). The operator ran (the census's
/// `multi_failure_s1` row), and the input ends `Done` under the witness rule.
#[test]
fn w_p3a_unplanted() {
    let text = include_str!("chain_seeds/w_p3a.prog");
    let run = witness::witness(Schedule::S1, ChainPlant::None, text);
    assert_eq!(run.end, End::Done);
    assert_eq!(run.census.rejects.len(), 1, "{:?}", run.census.rejects);
    let (reason, n) = run.census.rejects.iter().next().unwrap();
    assert!(reason.starts_with("bad-txns-nonfinal") && *n == 1, "{reason} ×{n}");
    assert_eq!(run.census.rows.get("multi_failure_s1"), Some(&1), "{:?}", run.census.rows);
    let at = witness::rejected_block(&run, "bad-txns-nonfinal").expect("the rejected block");
    let mut runner =
        Runner::with_plant(Pin::V24_0, Schedule::S1, Mode::Diff, ChainPlant::None).unwrap();
    SEEN.lock().unwrap().clear();
    let hooked = planted_run(
        &mut runner,
        text,
        Some(Plant {
            core: |s| seen(&s.reply.outcome),
            node: |n| seen(&n.step.reply.outcome),
            ..Plant::at(at)
        }),
    );
    assert_eq!(hooked.end, End::Done);
    let replies = std::mem::take(&mut *SEEN.lock().unwrap());
    assert_eq!(replies.len(), 2, "{replies:?}");
    for reply in &replies {
        assert!(reply.contains("bad-txns-nonfinal"), "Core's, then the node's: {replies:?}");
    }
}

/// O2b-4 T5, W-P3b unplanted on S0: after the first `flush always` V's dirty set is empty
/// (D42), and the killed restart finds the same stored records on both sides, with the three
/// disconnected blocks `FAILED_VALID`. The input ends `Done` under the witness rule.
#[test]
fn w_p3b_unplanted() {
    let text = include_str!("chain_seeds/w_p3b.prog");
    let run = witness::witness(Schedule::S0, ChainPlant::None, text);
    assert_eq!(run.end, End::Done);
    let last = run.raws.len() - 1;
    assert_eq!(run.kinds[last], "restart");
    let persisted = witness::core_report(&run, last).post.persisted.expect("a restart");
    assert_eq!(witness::stored_failed(&persisted.failed), 3, "Core's stored records");

    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = core_ops(&mut core, text);
    let mut flushed = false;
    let mut steps = Vec::new();
    for op in &ops {
        steps.push(node_ran(&mut node, op).step);
        if !flushed && matches!(op, ChainOp::Flush { .. }) {
            flushed = true;
            assert_eq!(node.dirty_index_len(), Some(0), "the first flush wrote V's dirty set");
        }
    }
    let node_persisted = steps.last().unwrap().post.persisted.clone().expect("a restart");
    assert_eq!(node_persisted, persisted, "the restart's stored records");
}

/// The ops Core runs for `text` from the image, in order.
fn core_ops(core: &mut CoreSut, text: &str) -> Vec<ChainOp> {
    let program = Program::parse(text).unwrap();
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
