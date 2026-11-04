//! Both sides of `diff_chain` for node-l1's chain tests: Core's session and `NodeSut`'s V on one
//! schedule, restored to the image, and ops held the same on both: every plane outside the
//! runner's gaps (`diff::GAPS`), as Diff mode compares them.

#![allow(dead_code, reason = "each includer uses a subset")]

use std::collections::{HashSet, VecDeque};
use std::os::unix::fs::PermissionsExt;

use dash_oracle_sys::{ChainOp, Pin, Post};
use dash_primitives::TxHash;
use node_l1::chain::build::Built;
use node_l1::chain::census::Census;
use node_l1::chain::compare::differences;
use node_l1::chain::core::{CoreSut, Schedule, T_RECENT, image_world};
use node_l1::chain::diff::GAPS;
use node_l1::chain::generate::{Gen, due_enforce};
use node_l1::chain::intent::Program;
use node_l1::chain::node::{IndexEntry, NodeSide, NodeStep, index_digest};
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
    assert_eq!(core.best_invalid, node.best_invalid, "{what}: best_invalid");
    assert_eq!(core.candidates, node.candidates, "{what}: candidates");
    assert_eq!(core.persisted, node.persisted, "{what}: persisted");
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

/// The node's step for `op`, which it serves.
pub fn node_step(node: &mut NodeSide, op: &ChainOp) -> Step {
    node_ran(node, op).step
}

/// [`node_step`] with the facts the Diff judge reads (`diff::as_judged`'s teardown prefix).
pub fn node_ran(node: &mut NodeSide, op: &ChainOp) -> NodeStep {
    node.step(op).unwrap_or_else(|u| panic!("{u:?}"))
}

/// `op` on both sides ([`core_step`]), with V's index after it (none while down).
pub fn both(
    core: &mut CoreSut,
    node: &mut NodeSide,
    op: &ChainOp,
) -> (Step, Step, Vec<IndexEntry>) {
    let c = core_step(core, op);
    let n = node_step(node, op);
    (c, n, node.index().unwrap_or_default())
}

/// Whether the linked oracle is at ABI 33 or later; if not, says why `what` does not run: it
/// `needs` what the shim writes from ABI 33.
pub fn at_abi_33(what: &str, needs: &str) -> bool {
    let abi = dash_oracle_sys::ffi::ABI_VERSION;
    if abi < 33 {
        eprintln!(
            "SKIPPED {what}: needs the oracle at ABI 33 ({needs}); dash-oracle-sys declares ABI \
             {abi}"
        );
    }
    abi >= 33
}

/// No plane outside [`GAPS`] differs between the two steps (the reply with its `BlockChecked`
/// list, the events, every post field, a down post's included).
pub fn assert_planes(core: &Step, node: &Step, what: &str) {
    let (_, differ) = differences(core, node, &[]);
    let real: Vec<_> = differ.into_iter().filter(|f| !GAPS.contains(f)).collect();
    assert!(real.is_empty(), "{what}: {real:?}\nCore {core:#?}\nnode {node:#?}");
}

/// Two steps held the same as the Diff runner holds them ([`assert_planes`]), and the node's
/// `index` is its own digest of `entries`.
pub fn assert_steps(core: &Step, node: &Step, entries: &[IndexEntry], what: &str) {
    assert_planes(core, node, what);
    assert!(node.records.is_empty(), "{what}: the node has no record tap until a7");
    if !node.post.down {
        assert_eq!(node.post.index, Some(index_digest(entries)), "{what}: the node's own digest");
    }
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
