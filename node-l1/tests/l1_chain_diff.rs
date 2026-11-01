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

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chain_sides::{
    assert_posts, assert_same, both, loaded_world, private_dir, same_program, sides,
};
use dash_chain::Status;
use dash_consensus::BlockId;
use dash_oracle_sys::{
    ChainOp, ChainReport, CheckResult, Event, FlushMode, Persisted, Pin, RawReport, RestartKind,
    Utxo,
};
use dash_statedigest::Record;
use dashd_chainstate::chain::ValidationInterface;
use node_l1::Divergence;
use node_l1::chain::census::Census;
use node_l1::chain::compare::differences;
use node_l1::chain::core::{CoreSut, SPORK_19, SPORK_OFF, Schedule, T_OLD, T_RECENT, image_world};
use node_l1::chain::diff::{
    Facts, GAPS, Judgement, NodeFacts, QUEUE_LIMIT, QUEUED, judge, refused,
};
use node_l1::chain::generate::Gen;
use node_l1::chain::intent::Program;
use node_l1::chain::node::{IndexEntry, NodeSide, NodeStep, index_digest};
use node_l1::chain::run::{End, Refused, Unsupported, op_kind};
use node_l1::chain::step::{ReplyClass, Step};
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
    let refused = Judgement::Refused(Refused::QueueDrain);
    assert_eq!(long.judge(&long.core, &long.core_as_node()), refused);
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

/// After a restore, the first header is notified as Core's is: Core's restore is a load, which
/// clears its last notified header tip. The node's replays the image, so V's latch stays at the
/// image's tip and a header that is not a new best one goes unnotified. `NodeSut`'s clean restart
/// stores the tip since BL-271, so it could give that load; the harness does not use it yet.
#[test]
#[ignore = "needs a NodeSut load of the image: V's header-tip latch survives the replay"]
fn the_first_header_tip_after_a_restore_is_notified_as_core_s() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let steps = same_program(&mut core, &mut node, "fork 1 1\n");
    let first = &steps.first().expect("an op").1;
    assert!(first.events.iter().any(|e| e.name == "NotifyHeaderTip"), "{:?}", first.events);
}

/// Each op kind V does not serve yet is refused before anything runs.
#[test]
fn each_unsupported_op_kind_is_refused() {
    let dir = private_dir();
    let (mut node, world) = NodeSide::start(Pin::V24_0, Schedule::S0, dir.path()).unwrap();
    let hash = world.blocks.last().unwrap().hash;
    let before = node.step(&ChainOp::SetTime(T_RECENT)).unwrap().step.post;
    let unsupported = [
        ChainOp::ClRecord {
            height: 200,
            hash,
            sig: [0; 96],
        },
        ChainOp::Flush {
            mode: FlushMode::Always,
            label: String::new(),
        },
        ChainOp::Restart(RestartKind::Clean),
        ChainOp::Restart(RestartKind::Kill),
    ];
    for op in &unsupported {
        assert_eq!(node.step(op).err(), Some(Unsupported::new(op, Refused::Op)), "{op:?}");
    }
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
/// any other op: over every reorg program, V serves every op, and each op's steps differ in the
/// planes V lacks until a7 and a10 only ([`GAPS`]), the reorg's reply, `BlockChecked` list,
/// events, tip, best header, `index` digest (undo bits included) and chain tips included. The
/// one other difference is the declared replay-latch gap: an input's first op, whose header Core
/// notifies as a new header tip after its load where V's replay does not.
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
            assert!(differ.iter().all(|f| GAPS.contains(f)), "{what}: {differ:?}");
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

/// Each plane of V's real reorg step is compared, behind the gap planes it differs in: a signal
/// left out, moved or added, a check left out, and a planted tip, best header, `index` bit or
/// chain tips are each the divergence, by that plane's name.
#[test]
fn each_plane_of_a_reorg_is_compared() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = judged(&mut core, &mut node, "fork 1 2\n");
    let reorg = ops.last().expect("an op");
    let real = differing(reorg);
    assert!(real.iter().all(|f| GAPS.contains(f)), "{real:?}");
    let names = |s: &Step| s.events.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    assert!(names(&reorg.node.step).contains(&"BlockDisconnected".to_owned()));
    let entries = node.index().expect("V serves");
    let tip = entries.iter().map(|e| e.height).max().unwrap();
    let mut flipped = entries.clone();
    flipped.iter_mut().find(|e| e.height == tip).unwrap().status ^= Status::HAVE_UNDO;
    let flipped = Some(index_digest(&flipped));
    let plants: [(&str, &str, &Plant); 10] = [
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

/// A difference in a plane V lacks until CH3a masks none after it: on V's real step of a
/// connect, which differs in a gap plane only, a planted `index` or `tips` difference is
/// named by its own plane.
#[test]
fn a_difference_behind_a_gap_plane_is_named_by_its_own_plane() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let ops = judged(&mut core, &mut node, "extend main 1\n");
    let connect = ops.last().expect("an op");
    let Judgement::Diverged(gap) = connect.judge(&connect.core, &connect.node) else {
        panic!("V's step differs in its gap planes");
    };
    assert!(GAPS.contains(&gap.field), "{gap}");
    let entries = node.index().expect("V serves");
    let tip = entries.iter().map(|e| e.height).max().unwrap();
    let mut planted = entries.clone();
    planted.iter_mut().find(|e| e.height == tip).unwrap().status ^= Status::HAVE_UNDO;
    let mut index = connect.node.clone();
    index.step.post.index = Some(index_digest(&planted));
    assert_ne!(index.step.post.index, connect.core.post.index);
    assert_eq!(connect.judge(&connect.core, &index), diverged("post.index"), "behind {gap}");
    let mut tips = connect.node.clone();
    tips.step.post.tips = Some(Vec::new());
    assert_eq!(connect.judge(&connect.core, &tips), diverged("post.tips"), "behind {gap}");
}

/// A change to one step.
type Plant = dyn Fn(&mut Step);

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
    let planted = |plant: &Plant| {
        let mut node = connect.core_as_node();
        plant(&mut node.step);
        assert_ne!(node.step, connect.core, "the plant changes the step");
        connect.judge(&connect.core, &node)
    };
    let index = connect.core.post.index.expect("Core's index");
    let plants: [(&str, &Plant); 16] = [
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

/// Diff mode ends an input at the first difference, with its plane's or field's name (until a7
/// the node has no records, so `records` or `attempts` differs), and an unsupported op ends it
/// out of domain, counted in the census. (Not through `fuzz_one_in`: its session writes the
/// census file into the temporary directory when the thread ends.)
#[test]
fn diff_mode_reports_the_first_difference() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let run = runner.run(&Program::parse("extend main 1\n").unwrap()).unwrap();
    let End::Diverged(d) = run.end else {
        panic!("not a divergence: {:?}", run.end);
    };
    assert!(["records", "attempts"].contains(&d.field), "{d}");
    println!("diff mode: the first difference is {d}");

    let run = runner.run(&Program::parse("flush always\n").unwrap()).unwrap();
    let flush = Unsupported {
        kind: "flush",
        why: Refused::Op,
    };
    assert_eq!(run.end, End::Unsupported(flush));
    assert_eq!(run.census.ops.get(&("flush", "unsupported")), Some(&1));
}

/// The ops the node does not serve in Diff mode (O2b D12; `RESTART` and `FLUSH` until a10).
const UNSERVED: [&str; 3] = ["cl_record", "flush", "restart"];

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

/// The gap plane a served op's step ends on, read from Core's step: `records` when Core wrote an
/// outcome (`C`, `R` or `D`), which V writes from a7, else `post.flushes`, the flush counters V
/// keeps from a10. Every other plane is the same.
fn gap(core: &RawReport) -> &'static str {
    let core = ChainReport::decode(core.clone());
    if core.records.iter().any(|r| matches!(r, Record::Outcome(_))) {
        "records"
    } else {
        "post.flushes"
    }
}

/// Diff mode over `diff_chain`'s public seeds and 50 generated inputs (DEC-504), the tier-1
/// check in place of a Diff-mode fuzz target until a7 and a10 close the records and flush-counter
/// gaps. Each input ends at its first op in exactly one of three ways: an op kind the node does
/// not serve, refused before it ran; a served op on the one gap plane Core's step decides
/// ([`gap`]); or, for a header or a block (whose header it accepts first), the declared
/// replay-latch shape (O2b-diff-chain.md "Restore on the node until a10", BL-271): Core notifies
/// a header that is not a new best one after its load where V's replay does not. The test replays
/// each latch on fresh sides to hold it exact: that one event, with the replies and every post
/// field the same. Any other end fails, except an input a tie decided, which the runner discards
/// uncompared (O2b D30).
#[test]
fn diff_mode_ends_each_input_unsupported_on_a_gap_or_in_the_latch() {
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
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let mut ends: BTreeMap<String, usize> = BTreeMap::new();
    let mut latched = Vec::new();
    for (name, bytes) in &inputs {
        let program = Program::decode(bytes);
        let run = runner.run(&program).unwrap();
        let end = match (&run.end, &run.kinds[1..]) {
            (
                End::Unsupported(Unsupported {
                    kind,
                    why: Refused::Op,
                }),
                [],
            ) if UNSERVED.contains(kind) => format!("{kind} unsupported"),
            (End::Diverged(d), [kind])
                if !UNSERVED.contains(kind) && d.field == gap(run.raws.last().unwrap()) =>
            {
                format!("{kind} on {}", d.field)
            }
            (End::Diverged(d), [kind @ ("header" | "block")]) if d.field == "events" => {
                latched.push((name, program));
                format!("{kind} latch")
            }
            (End::Tainted(..), [_]) => "tie".to_owned(),
            (other, kinds) => panic!("{name}: {other:?} after {kinds:?}\n{program}"),
        };
        *ends.entry(end).or_default() += 1;
    }
    drop(runner);
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    for (name, program) in latched {
        let t = if program.old {
            T_OLD
        } else {
            T_RECENT
        };
        core.restore(t).unwrap();
        node.restore(t).unwrap();
        let mut w = image_world();
        w.mocktime = t;
        w.loaded();
        let (mut generator, mut census) = (Gen::default(), Census::default());
        let first = program
            .intents
            .iter()
            .find_map(|i| generator.resolve(&mut w, i, &mut census).into_iter().next());
        let first = first.expect("a first op").op;
        assert!(matches!(first, ChainOp::Header(_) | ChainOp::Block(_)), "{name}: {first:?}");
        let (c, n, entries) = both(&mut core, &mut node, &first);
        let at = c.events.iter().position(|e| e.name == "NotifyHeaderTip");
        let at = at.unwrap_or_else(|| panic!("{name}: no header tip in {:?}", c.events));
        let mut notified = n.events.clone();
        notified.insert(at, c.events[at].clone());
        assert_eq!(notified, c.events, "{name}: the latch is the one difference in the events");
        assert_eq!(c.reply, n.reply, "{name}: reply");
        assert_posts(&c.post, &n.post, &entries, name);
    }
    println!("diff mode: {ends:?}");
}

/// In Diff mode, `INVALIDATE`, `RECONSIDER` and `PRECIOUS` as an input's first op are served, each
/// ending on a gap plane: `INVALIDATE` of the tip on `records` (its disconnect's record, which V
/// lacks until a7), the other two, which move nothing, on `post.flushes`. The generator
/// reconsiders only the input's own invalidations, so a first-op `RECONSIDER` names a block
/// neither side has (`random`). `CL_RECORD` stays refused (DEC-505).
#[test]
fn diff_mode_serves_the_block_rpcs_and_refuses_cl_record() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).unwrap();
    let mut end = |text: &str| runner.run(&Program::parse(text).unwrap()).unwrap().end;
    let texts = ["invalidate main@-0\n", "reconsider random\n", "precious main@-0\n"];
    let gap = |field| {
        End::Diverged(Divergence {
            field,
        })
    };
    let want = [gap("records"), gap("post.flushes"), gap("post.flushes")];
    assert_eq!(texts.map(&mut end), want, "{texts:?}");
    let refused = Unsupported {
        kind: "cl_record",
        why: Refused::Op,
    };
    assert_eq!(end("cl main@-0\n"), End::Unsupported(refused));
}

/// A difference in the UTXO set alone on V's real step, which differs in a gap plane too, is
/// named `post.utxo`: on a connect and on a reorg (DEC-505).
#[test]
fn a_utxo_difference_behind_a_gap_plane_is_named_post_utxo() {
    for (text, reorg) in [("extend main 1\n", false), ("fork 1 2\n", true)] {
        let dir = private_dir();
        let (mut core, mut node) = sides(Schedule::S0, &dir);
        let ops = judged(&mut core, &mut node, text);
        let step = ops.last().expect("an op");
        let disconnects = step.core.events.iter().any(|e| e.name == "BlockDisconnected");
        assert_eq!(disconnects, reorg, "{text:?}: a reorg");
        let real = differing(step);
        assert!(!real.is_empty() && real.iter().all(|f| GAPS.contains(f)), "{text:?}: {real:?}");
        let mut planted = step.node.clone();
        let utxo = planted.step.post.utxo.take().expect("the node reads its coins");
        planted.step.post.utxo = Some(Utxo {
            amount: utxo.amount + 1,
            ..utxo
        });
        assert_eq!(step.judge(&step.core, &planted), diverged("post.utxo"), "{text:?}");
    }
}
