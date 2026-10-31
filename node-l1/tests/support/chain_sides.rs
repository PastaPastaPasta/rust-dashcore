//! Both sides of `diff_chain` for node-l1's chain tests: Core's session and `NodeSut`'s V on one
//! schedule, restored to the image, and ops held the same on both (replies, events, and every
//! post field the node reads).

#![allow(dead_code, reason = "each includer uses a subset")]

use std::collections::{HashSet, VecDeque};
use std::os::unix::fs::PermissionsExt;

use dash_oracle_sys::{ChainOp, Pin, Post};
use dash_primitives::TxHash;
use node_l1::chain::build::Built;
use node_l1::chain::census::Census;
use node_l1::chain::core::{CoreSut, Schedule, T_RECENT, image_world};
use node_l1::chain::generate::{Gen, due_enforce};
use node_l1::chain::intent::Program;
use node_l1::chain::node::{IndexEntry, NodeSide, index_digest};
use node_l1::chain::run::op_kind;
use node_l1::chain::step::Step;
use node_l1::chain::world::World;

/// A private directory under `$TMPDIR` (the box's umask would make it group-writable).
pub fn private_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("l1_chain")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

/// Both sides on `schedule`, the node first (D8's check needs the oracle free), each restored to
/// the image at [`T_RECENT`].
pub fn sides(schedule: Schedule, dir: &tempfile::TempDir) -> (CoreSut, NodeSide) {
    let (mut node, _) = NodeSide::start(Pin::V24_0, schedule, dir.path()).unwrap();
    let (mut core, _) = CoreSut::start(Pin::V24_0, schedule).unwrap();
    core.restore(T_RECENT).unwrap();
    node.restore(T_RECENT).unwrap();
    (core, node)
}

/// The node's post fields against Core's, `utxo` and `index` included.
pub fn assert_posts(core: &Post, node: &Post, entries: &[IndexEntry], what: &str) {
    assert!(!core.down && !node.down, "{what}: down");
    assert_eq!(core.tip, node.tip, "{what}: tip");
    assert_eq!(core.best_header, node.best_header, "{what}: best_header");
    assert!(node.utxo.is_some(), "{what}: the node reads its coins");
    assert_eq!(core.utxo, node.utxo, "{what}: utxo");
    assert_eq!(core.ibd, node.ibd, "{what}: ibd");
    assert_eq!(core.best_cl, node.best_cl, "{what}: best_cl");
    assert_eq!(core.sporks, node.sporks, "{what}: sporks");
    assert_eq!(core.tips, node.tips, "{what}: tips");
    assert_eq!(core.deployments, node.deployments, "{what}: deployments");
    assert_eq!(node.index, Some(index_digest(entries)), "{what}: the node's own digest");
    assert_eq!(core.index, node.index, "{what}: index");
}

/// Core's step for `op`, never one an address tie decided (O2b D30), which no comparison could
/// hold the node to.
pub fn core_step(core: &mut CoreSut, op: &ChainOp) -> Step {
    let done = core.step(op).unwrap();
    assert!(!done.tainted, "{}: tie-decided at {:?}", op_kind(op), done.step.post.tie);
    done.step
}

/// `op` on both sides ([`core_step`]), with V's index after it.
pub fn both(
    core: &mut CoreSut,
    node: &mut NodeSide,
    op: &ChainOp,
) -> (Step, Step, Vec<IndexEntry>) {
    let c = core_step(core, op);
    let n = node.step(op).unwrap_or_else(|u| panic!("{u:?}")).step;
    (c, n, node.index().expect("V serves"))
}

/// Two steps held the same: the replies (result and `BlockChecked` list), the events and the
/// post fields.
pub fn assert_steps(core: &Step, node: &Step, entries: &[IndexEntry], what: &str) {
    assert_eq!(core.reply, node.reply, "{what}: reply");
    assert_eq!(core.events, node.events, "{what}: events");
    assert!(node.records.is_empty(), "{what}: the node has no record tap until a7");
    assert_posts(&core.post, &node.post, entries, what);
}

/// One op held the same on both sides ([`assert_steps`]): Core's step.
pub fn assert_same(core: &mut CoreSut, node: &mut NodeSide, op: &ChainOp) -> Step {
    let (c, n, entries) = both(core, node, op);
    assert_steps(&c, &n, &entries, op_kind(op));
    c
}

/// What a disconnect pool's `take()` gives after the disconnects of `disconnected` (tip first)
/// and the connects of `connected`, when it evicted nothing: each disconnected block's
/// transactions in reverse, as `AddTransactionsFromBlock` queues them, less those a connect's
/// `removeForBlock` takes back out.
// Core: src/kernel/disconnected_transactions.h:91-100, :103-118, :130-135 at the pin
pub fn taken(disconnected: &[&Built], connected: &[&Built]) -> Vec<TxHash> {
    let back: HashSet<_> = connected.iter().flat_map(|b| &b.txids).collect();
    let queued = disconnected.iter().flat_map(|b| b.txids.iter().rev());
    queued.filter(|t| !back.contains(t)).map(|&t| TxHash::from(t)).collect()
}

/// The image's world as both sides hold it after a restore.
pub fn loaded_world() -> World {
    let mut w = image_world();
    w.loaded();
    w
}

/// `text`'s ops on both sides, each held the same ([`assert_same`]), with Core's steps.
pub fn same_program(core: &mut CoreSut, node: &mut NodeSide, text: &str) -> Vec<(ChainOp, Step)> {
    same_program_in(core, node, text).1
}

/// [`same_program`], with the world the generator built (its blocks' hashes and heights).
pub fn same_program_in(
    core: &mut CoreSut,
    node: &mut NodeSide,
    text: &str,
) -> (World, Vec<(ChainOp, Step)>) {
    let program = Program::parse(text).unwrap();
    let mut w = loaded_world();
    let mut generator = Gen::default();
    let mut census = Census::default();
    let mut steps = Vec::new();
    for intent in &program.intents {
        let mut ops: VecDeque<_> = generator.resolve(&mut w, intent, &mut census).into();
        while let Some(p) = ops.pop_front() {
            let step = assert_same(core, node, &p.op);
            w.observe(&p.op, &step);
            match due_enforce(&w, &mut census) {
                Some(Some(enforce)) => ops.push_front(enforce),
                Some(None) => panic!("the program stays in the supported ops"),
                None => {}
            }
            steps.push((p.op, step));
        }
    }
    (w, steps)
}
