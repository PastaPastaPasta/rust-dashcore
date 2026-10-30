//! CH3b b3: the ChainLock entry points of CH3 §4.5, its two CONS-11 restart cases, and the
//! disconnect pools' calls of §4.3 and §7.6, on `diff_chain`'s two sides, Core's session and
//! `NodeSut`'s V given the same submissions. Each op is held the same on both sides (reply,
//! events, and every post field the node reads: tip, chain tips, the UTXO digest, the index
//! digest with every block's status), but for two: a `CL_RECORD` is held with the `CL_ENFORCE`
//! after it as a pair (D193), and a `RESTART` by its reply's result and the posts.
//!
//! The entry points: a block that conflicts with a lock at connect, a proposal that conflicts,
//! and `FindMostWorkChain` over a conflicting ancestor. A lock on a fork and `RECONSIDER`'s
//! `ignore_chainlocks` are b2a's (`l1_chain_reorg`). The CONS-11 cases take each kind of mark
//! through `FLUSH`es and a killed `RESTART`, directed ops on the node; their clean twin waits
//! for BL-271.
//!
//! The pools: Core's session has no mempool, so `MaybeUpdateMempoolForReorg` returns at once
//! there (`validation.cpp:426` at the pin) and no plane sees a pool. V's calls on its pools, with
//! what each drain took ([`PoolCall`]), are held to the sequence Core's code makes at each call
//! site instead (BL-435 holds them to a live Core once the oracle has a mempool). The three
//! failure sites are reached on the node alone, by removing a block file Core's session has no
//! way to lose.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/chain_sides.rs"]
mod chain_sides;

use std::collections::VecDeque;

use chain_sides::{assert_posts, assert_steps, core_step, private_dir, sides, taken};
use dash_chain::Status;
use dash_oracle_sys::{
    ChainHash, ChainOp, CheckResult, Checked, Event, FlushMode, Post, RestartKind,
};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::FlushStateMode;
use dashd_chainstate::reorg::PoolCall::{self, Open, Update};
use node_l1::chain::build::Built;
use node_l1::chain::census::Census;
use node_l1::chain::core::{CoreSut, Schedule};
use node_l1::chain::diff::drained_at_end;
use node_l1::chain::generate::{Gen, due_enforce};
use node_l1::chain::intent::Program;
use node_l1::chain::node::{IndexEntry, NodeSide, V_STOPPED};
use node_l1::chain::run::op_kind;
use node_l1::chain::step::Step;
use node_l1::chain::world::{Block, World, display_hash};
use serde_json::Value;

/// One op on both sides: Core's step and the node's, and V's pool calls during it.
struct Ran {
    op: ChainOp,
    core: Step,
    node: Step,
    pools: Vec<PoolCall>,
}

/// A `CL_RECORD` whose comparison waits for the op after it, with V's index after it.
struct Recorded {
    at: usize,
    entries: Vec<IndexEntry>,
}

/// Both sides and the generator's world, so that a test runs a program in phases and acts
/// between them.
struct Sides {
    core: CoreSut,
    node: NodeSide,
    w: World,
    generator: Gen,
    census: Census,
    ran: Vec<Ran>,
    recorded: Option<Recorded>,
    _dir: tempfile::TempDir,
}

impl Sides {
    /// Both sides at the image, the image's pool calls dropped.
    fn new() -> Sides {
        let dir = private_dir();
        let (core, node) = sides(Schedule::S0, &dir);
        node.pools().take_calls();
        Sides {
            core,
            node,
            w: chain_sides::loaded_world(),
            generator: Gen::default(),
            census: Census::default(),
            ran: Vec::new(),
            recorded: None,
            _dir: dir,
        }
    }

    /// Both sides at the image, stored as Core saves its image, until a10's stored images: an
    /// ALWAYS flush, then a clean restart, whose spork store keeps spork 19 for a later kill's
    /// load. The node rebuilds its image in memory, and its stop writes nothing (BL-271), so the
    /// flush comes first. A second ALWAYS flush after the load takes V's once-only `LEFT_IBD`
    /// freeze with it (CH2 §3.3, ours; the load starts V's policy afresh): alone, its
    /// `ChainStateFlushed` would fall in the next op, where Core, which never flushes on leaving
    /// IBD, sends none. BL-442 removes that signal (DEC-535), and this flush with it.
    // Core: src/test/oracle/oracle_chain.cpp:1293-1308 (oracle_chain_save) at the pin
    fn stored() -> Sides {
        let mut s = Sides::new();
        s.run("flush always\nrestart clean\nflush always\n");
        s
    }

    /// `text`'s ops on both sides, with each enforcement the generator finds due, as
    /// `same_program_in` runs them: the ops it ran.
    fn run(&mut self, text: &str) -> &[Ran] {
        let from = self.ran.len();
        for intent in Program::parse(text).unwrap().intents {
            let mut ops: VecDeque<_> =
                self.generator.resolve(&mut self.w, &intent, &mut self.census).into();
            while let Some(p) = ops.pop_front() {
                self.both(p.op);
                let ran = self.ran.last().unwrap();
                self.w.observe(&ran.op, &ran.core);
                match due_enforce(&self.w, &mut self.census) {
                    Some(Some(enforce)) => ops.push_front(enforce),
                    Some(None) => panic!("{text:?}: the tie model refuses an enforcement"),
                    None => {}
                }
            }
        }
        self.settle();
        &self.ran[from..]
    }

    /// `op` on both sides, held the same. On the node `CL_RECORD`, `FLUSH` and `RESTART` are
    /// the directed ops, which `NodeSide::step` does not serve. `CL_RECORD` is
    /// [`NodeSide::record_verified_chainlock`], which enforces a lock that moves the floor at
    /// once where Core's session waits for `CL_ENFORCE`: the record's reply is held at once, and
    /// the rest with the op after it, joined with it if that is the `CL_ENFORCE`, else alone.
    /// [`NodeSide::restart`] hears nothing of its load (the new V's listener comes after it), so
    /// a restart is held by its reply's result and the posts, without the load's signals and
    /// `BlockChecked` list, and its teardown by its flushes: a clean shutdown's two, a kill's
    /// none.
    // Core: src/init.cpp:379-385, :449-455 at the pin
    fn both(&mut self, op: ChainOp) {
        let core = core_step(&mut self.core, &op);
        let what = op_kind(&op);
        if let ChainOp::ClRecord {
            height,
            hash,
            sig,
        } = op
        {
            self.settle();
            let node = self.node.record_verified_chainlock(height, hash, sig);
            assert_eq!(core.reply.outcome, node.reply.outcome, "{what}: reply");
            self.recorded = Some(Recorded {
                at: self.ran.len(),
                entries: self.node.index().unwrap_or_default(),
            });
            self.push(op, core, node);
            return;
        }
        let node = match op {
            ChainOp::Flush {
                mode,
                ..
            } => self.node.flush(flush_mode(mode)),
            ChainOp::Restart(kind) => {
                let (step, flushes) = self.node.restart(kind);
                let shutdown = if kind == RestartKind::Clean {
                    2
                } else {
                    0
                };
                assert_eq!(flushes, shutdown, "{what}: the teardown's flushes");
                step
            }
            _ => self.node_step(&op),
        };
        let entries = self.node.index().expect("V serves");
        if op == ChainOp::ClEnforce
            && let Some(r) = self.recorded.take()
        {
            let record = &self.ran[r.at];
            assert_eq!(core.reply.outcome, node.reply.outcome, "{what}: reply");
            assert_eq!(
                joined(&record.node, &node),
                joined(&record.core, &core),
                "the record and CL_ENFORCE: signals and checks"
            );
            assert_posts(&core.post, &node.post, &entries, what);
        } else {
            self.settle();
            if matches!(op, ChainOp::Restart(_)) {
                assert_eq!(core.reply.outcome, node.reply.outcome, "{what}: reply");
                assert_posts(&core.post, &node.post, &entries, what);
            } else {
                held(&core, &node, &entries, what);
            }
        }
        self.push(op, core, node);
    }

    /// A record that no `CL_ENFORCE` followed, held alone.
    fn settle(&mut self) {
        if let Some(r) = self.recorded.take() {
            let record = &self.ran[r.at];
            held(&record.core, &record.node, &r.entries, "cl_record");
        }
    }

    fn push(&mut self, op: ChainOp, core: Step, node: Step) {
        let pools = self.node.pools().take_calls();
        self.ran.push(Ran {
            op,
            core,
            node,
            pools,
        });
    }

    /// `text`'s ops on the node alone, the world following the node: the steps and V's pool
    /// calls during each.
    fn node_only(&mut self, text: &str) -> Vec<(Step, Vec<PoolCall>)> {
        let mut out = Vec::new();
        for intent in Program::parse(text).unwrap().intents {
            for p in self.generator.resolve(&mut self.w, &intent, &mut self.census) {
                let step = self.node_step(&p.op);
                self.w.observe(&p.op, &step);
                out.push((step, self.node.pools().take_calls()));
            }
        }
        out
    }

    fn node_step(&mut self, op: &ChainOp) -> Step {
        self.node.step(op).unwrap_or_else(|u| panic!("{u:?}")).step
    }

    /// The active chain's block `k` below the tip (`main@-k`).
    fn main(&self, k: u32) -> Block {
        let tip = &self.w.blocks[self.w.tip];
        self.w.blocks[self.w.ancestor(self.w.tip, tip.height - k).unwrap()].clone()
    }

    /// Fork branch `k`'s tip (`tip#k`).
    fn branch(&self, k: usize) -> Block {
        self.w.blocks[self.w.branches[k]].clone()
    }

    /// The `k`-th block the program built (`built#k`).
    fn built(&self, k: usize) -> Block {
        self.w.blocks[self.generator.built[k]].clone()
    }

    /// V's status of `block`.
    fn status(&self, block: &Block) -> Status {
        let hash = BlockHash::from(block.hash);
        let entries = self.node.index().expect("V serves");
        Status(entries.iter().find(|e| e.hash == hash).expect("indexed").status)
    }

    /// A `RESTART` of `kind` behind what the tie model puts first when the load would choose
    /// between equal-work tips (G-T1: two blocks on the active chain, and an ALWAYS flush before
    /// a kill): Core's post after the restart, and the last of those blocks.
    fn restart(&mut self, kind: RestartKind) -> (Post, Block) {
        let text = if kind == RestartKind::Clean {
            "restart clean\n"
        } else {
            "restart kill\n"
        };
        let ran = self.run(text);
        let restart = ran.last().expect("an op ran");
        assert_eq!(restart.op, ChainOp::Restart(kind), "the restart ran last");
        let post = restart.core.post.clone();
        (post, self.built(self.generator.built.len() - 1))
    }

    /// Removes the loaded node's block or undo file `name`.
    fn lose(&self, name: &str) {
        std::fs::remove_file(self.node.blocks_dir().join(name)).unwrap();
    }
}

/// The two sides' steps for one op held the same ([`assert_steps`]), Core's events taken as Diff
/// mode takes them past `LimitValidationInterfaceQueue`'s bound (`diff::drained_at_end`): a
/// drain inside the op only interleaves its two kinds of signal.
fn held(core: &Step, node: &Step, entries: &[IndexEntry], what: &str) {
    assert_steps(&drained_at_end(core), node, entries, what);
}

/// The `FLUSH` op's mode as V's, the shim's `MODES` (the generator makes no `INDEX_ONLY`).
// Core: src/test/oracle/oracle_chain.cpp:1012-1013 at the pin
fn flush_mode(mode: FlushMode) -> FlushStateMode {
    match mode {
        FlushMode::Always => FlushStateMode::Always,
        FlushMode::IfNeeded => FlushStateMode::IfNeeded,
        FlushMode::Periodic => FlushStateMode::Periodic,
        FlushMode::IndexOnly => unreachable!("INDEX_ONLY has no V counterpart"),
    }
}

/// Two ops' signals and `BlockChecked` lists, joined in order.
fn joined(a: &Step, b: &Step) -> (Vec<Event>, Vec<Checked>) {
    let events = [&a.events[..], &b.events[..]].concat();
    (events, [&a.reply.checked[..], &b.reply.checked[..]].concat())
}

/// `block`'s row's status in `post`'s `getchaintips`.
fn tip_status<'a>(post: &'a Post, block: &Block) -> &'a str {
    let tips = post.tips.as_ref().expect("serving");
    let row = tips.iter().find(|t| t["hash"].as_str().and_then(display_hash) == Some(block.hash));
    row.and_then(|t| t["status"].as_str()).unwrap_or_else(|| panic!("no row: {tips:?}"))
}

/// The program's `block` as it was built.
fn built(block: &Block) -> &Built {
    block.built.as_ref().expect("built by the program")
}

/// A reply over a V that stopped during the op.
fn stopped(step: &Step) -> bool {
    step.reply.outcome.as_ref().is_err_and(|e| e.message == V_STOPPED)
}

/// A fork block at the height of a lock on the active tip, its header and data in before the
/// lock: enforcement leaves it alone (the locked block is active, so `EnforceBlock` walks
/// nothing), and once the fork has more work, `ConnectBlock` refuses it against the lock
/// (`BLOCK_CHAINLOCK` `bad-chainlock`): `FAILED_VALID`, its child `FAILED_CHILD` at the next
/// `FindMostWorkChain`, and the main block disconnected and connected again, all in that one op.
/// The step that disconnected the main block drains its pool with `fAddToMempool`, though it
/// found an invalid block; the next only connects.
// Core: src/validation.cpp:2568-2571 (ConnectBlock), :3657-3683 (FindMostWorkChain), :3786-3790
// (the drain) at the pin
#[test]
fn a_block_that_conflicts_with_a_chainlock_fails_at_connect() {
    let mut s = Sides::new();
    s.run("extend main 1\nfork 1 1\ncl main@-0\n");
    let (main, fork) = (s.main(0), s.branch(0));
    assert_eq!(main.height, fork.height, "the fork block is at the locked height");
    let ran = &s.run("extend b1 1\n")[0];
    let (step, pools) = (ran.core.clone(), ran.pools.clone());
    let child = s.branch(0);
    let refused = (Some("chainlock".into()), Some("bad-chainlock".into()));
    assert_eq!(
        step.reply.checked,
        [
            Checked(ChainHash(fork.hash), CheckResult::Invalid, refused.0, refused.1),
            Checked(ChainHash(main.hash), CheckResult::Valid, None, None)
        ],
        "the fork block is refused against the lock, and the main block comes back"
    );
    let status = s.status(&fork);
    assert!(status.has(Status::FAILED_VALID), "{status:?}");
    assert!(!status.has(Status::CONFLICT_CHAINLOCK), "no conflict mark: {status:?}");
    let status = s.status(&child);
    assert!(status.has(Status::FAILED_CHILD), "{status:?}");
    assert_eq!(step.post.tip.map(|t| t.1.0), Some(main.hash), "the lock holds");
    assert_eq!(pools, [Open, Update(true, taken(&[built(&main)], &[])), Open]);
}

/// A proposal on the tip, valid until a lock on another block at its height, a block whose header
/// alone both sides hold (so the lock's block is indexed, which a conflict needs, and enforcing
/// it cannot activate it): then `TestBlockValidity` refuses it before any other check
/// (`BLOCK_INVALID_PREV` `bad-chainlock`).
// Core: src/validation.cpp:5086-5089 (TestBlockValidity), src/chainlock/chainlock.cpp:81-91
// (HasConflictingChainLock), src/rpc/mining.cpp:721-724 at the pin
#[test]
fn a_proposal_that_conflicts_with_a_chainlock_is_refused() {
    let mut s = Sides::new();
    let reply = |r: &Ran| (op_kind(&r.op), r.core.reply.outcome.clone());
    let before = s.run("propose main@-0\n").iter().map(reply).collect::<Vec<_>>();
    assert_eq!(before, [("proposal", Ok(Value::Null))], "valid without the lock");
    s.run("extend main 1 headers hold\ncl built#1\n");
    let locked = s.built(1);
    assert_eq!(locked.height, s.main(0).height + 1, "the lock is at the proposal's height");
    let after = s.run("propose main@-0\n").iter().map(reply).collect::<Vec<_>>();
    assert_eq!(after, [("proposal", Ok("bad-chainlock".into()))], "refused against the lock");
}

/// A lock on a fork below a main block with a sibling of its own: `MarkConflictingBlock` marks
/// the main chain from the fork point (two blocks), and `FindMostWorkChain`, trying the sibling
/// (the most work left), finds its marked parent and marks it too. The fork is activated.
// Core: src/validation.cpp:4239-4245 (MarkConflictingBlock), :3657-3683 (FindMostWorkChain) at
// the pin
#[test]
fn find_most_work_chain_marks_the_descendants_of_a_conflicting_block() {
    let mut s = Sides::new();
    s.run("extend main 2\nfork 1 1\nfork 2 1\n");
    let (a1, a2) = (s.main(1), s.main(0));
    let (sibling, fork) = (s.branch(0), s.branch(1));
    assert_eq!((sibling.parent, sibling.height), (a2.parent, a2.height), "a2's sibling");
    assert_eq!((fork.parent, fork.height), (a1.parent, a1.height), "a1's sibling");
    let ran = s.run("cl tip#1\n");
    assert_eq!(ran.iter().map(|r| op_kind(&r.op)).collect::<Vec<_>>(), ["cl_record", "cl_enforce"]);
    assert_eq!(ran[1].core.post.tip.map(|t| t.1.0), Some(fork.hash), "the locked fork is active");
    for b in [&a1, &a2, &sibling] {
        let status = s.status(b);
        assert!(status.has(Status::CONFLICT_CHAINLOCK), "{}: {status:?}", b.height);
    }
    assert!(!s.status(&fork).has(Status::CONFLICT_CHAINLOCK));
}

/// CONS-11 case 1 through a `RESTART` of `kind`: [`find_most_work_chain_marks_the_descendants_of_a_conflicting_block`]
/// between two `FLUSH`es (the first so that only marks after it are dirty), then the restart.
/// `FindMostWorkChain` dirties its mark, so the sibling comes back `conflicting` in
/// `getchaintips` on both sides, though the load knows no lock; `MarkConflictingBlock` dirties
/// none of its own, so main's tip comes back a `valid-fork`. The load is at the tie model's last
/// block, which a kill's flush and a clean stop's shutdown flushes both store.
// Core: src/validation.cpp:3670-3673 (the mark and its dirty entry), :4237-4246 (the marks, no
// dirty entry), src/rpc/blockchain.cpp:1921 at the pin
fn a_conflict_mark_find_most_work_chain_made_across(kind: RestartKind) {
    let mut s = Sides::stored();
    s.run("extend main 2\nfork 1 1\nfork 2 1\nflush always\n");
    let (a2, sibling) = (s.main(0), s.branch(0));
    s.run("cl tip#1\nflush always\n");
    let (post, last) = s.restart(kind);
    assert_eq!(post.tip.map(|t| t.1.0), Some(last.hash), "{kind:?}: loaded at the last block");
    assert_eq!(tip_status(&post, &sibling), "conflicting", "{kind:?}");
    assert!(s.status(&sibling).has(Status::CONFLICT_CHAINLOCK), "{kind:?}: stored");
    assert_eq!(tip_status(&post, &a2), "valid-fork", "{kind:?}");
    assert_eq!(post.best_cl, Some(None), "{kind:?}: no lock after the load");
}

/// CONS-11 case 2 through a `RESTART` of `kind`, not persisted, as Core: a lock on a fork,
/// enforced between two `FLUSH`es, then the restart. `MarkConflictingBlock`'s marks are never
/// dirtied (nor are the disconnected entries), so the load has the main blocks without
/// `CONFLICT_CHAINLOCK`: main's tip is a `valid-fork` in `getchaintips` again on both sides. The
/// tie model extends the locked branch before the restart, so neither side reorganises.
// Core: src/validation.cpp:4237-4246 (the marks, no dirty entry), :3353-3420 (DisconnectTip) at
// the pin
fn a_mark_conflicting_block_mark_across(kind: RestartKind) {
    let mut s = Sides::stored();
    s.run("extend main 2\nfork 2 1\nflush always\n");
    let (a1, a2) = (s.main(1), s.main(0));
    let fork = s.branch(0);
    assert_eq!(fork.parent, a1.parent, "the fork is a1's sibling: a1 and a2 are marked");
    s.run("cl tip#0\nflush always\n");
    for b in [&a1, &a2] {
        assert!(s.status(b).has(Status::CONFLICT_CHAINLOCK), "{}: marked", b.height);
    }
    let (post, last) = s.restart(kind);
    assert_eq!(post.tip.map(|t| t.1.0), Some(last.hash), "{kind:?}: loaded at the last block");
    for b in [&a1, &a2] {
        assert!(!s.status(b).has(Status::CONFLICT_CHAINLOCK), "{kind:?}: {}: loaded", b.height);
    }
    assert_eq!(tip_status(&post, &a2), "valid-fork", "{kind:?}");
}

/// CONS-11 case 1, persisted through a killed `RESTART`.
#[test]
fn a_conflict_mark_find_most_work_chain_made_survives_a_restart() {
    a_conflict_mark_find_most_work_chain_made_across(RestartKind::Kill);
}

/// CONS-11 case 2, not persisted through a killed `RESTART`.
#[test]
fn a_mark_conflicting_block_mark_does_not_survive_a_restart() {
    a_mark_conflicting_block_mark_across(RestartKind::Kill);
}

/// CONS-11's clean twin, BL-271's acceptance (DEC-535): Core's shutdown flushes store the tie
/// model's two blocks, which no flush before a clean restart stores, so Core loads at 203 as
/// through a kill, and the marks come back as through a kill. V's stop writes nothing until
/// BL-271: the node's load lacks the two blocks and activates main's tip, stored unmarked, at 202.
// Core: src/init.cpp:379-385, :449-455 at the pin
#[test]
#[ignore = "BL-271"]
fn the_cons_11_cases_across_a_clean_restart() {
    a_conflict_mark_find_most_work_chain_made_across(RestartKind::Clean);
    a_mark_conflicting_block_mark_across(RestartKind::Clean);
}

/// `ActivateBestChainStep`'s pools on V's real connector: one per step, drained with
/// `fAddToMempool` only by a step that disconnected (two blocks here), never by one that only
/// connects. The drain takes the disconnected blocks' transactions, the tip's first, less any
/// the fork's connects took back out.
// Core: src/validation.cpp:3723 (the step's pool), :3786-3790 at the pin
#[test]
fn an_activate_best_chain_step_drains_its_pool_only_after_a_disconnect() {
    let mut s = Sides::new();
    let ran = s.run("extend main 2 tx=spends\nfork 2 3 tx=spends\n");
    let pools: Vec<_> = ran.iter().map(|r| r.pools.clone()).collect();
    let [a1, a2, b1, b2, b3] = [0, 1, 2, 3, 4].map(|k| s.built(k));
    let drained = taken(&[built(&a2), built(&a1)], &[built(&b1), built(&b2), built(&b3)]);
    let reorg = vec![Open, Update(true, drained)];
    let want = [vec![Open], vec![Open], vec![], vec![], reorg];
    assert_eq!(pools, want, "two connects, two forks kept, a reorg");
}

/// `InvalidateBlock`'s pools: one per disconnected block, which it drains, with `fAddToMempool`
/// for the first ten only (`++disconnected <= 10`), the twelve disconnects in the one op. (Core's
/// loop drains its signal queue past ten callbacks, which `held` allows for.)
// Core: src/validation.cpp:4056 (LimitValidationInterfaceQueue), :4074-4081 at the pin
#[test]
fn a_deep_invalidation_restores_the_first_ten_blocks_only() {
    let mut s = Sides::new();
    let pools =
        s.run("extend main 8\nextend main 4\ninvalidate built#0\n").last().unwrap().pools.clone();
    let want: Vec<_> = (0..12)
        .rev()
        .zip(1..)
        .flat_map(|(k, n)| [Open, Update(n <= 10, taken(&[built(&s.built(k))], &[]))])
        .collect();
    assert_eq!(pools, want);
}

/// `MarkConflictingBlock`'s pool: one for the mark's whole walk (two disconnects), drained with
/// `fAddToMempool` after the marks; then the step that connects the locked fork opens its own.
// Core: src/validation.cpp:4219-4250 (MarkConflictingBlock), :3723 (the step) at the pin
#[test]
fn a_chainlock_mark_drains_one_pool_for_its_walk() {
    let mut s = Sides::new();
    s.run("extend main 2 tx=spends\nfork 2 1 tx=spends\n");
    let (a1, a2) = (s.main(1), s.main(0));
    let pools: Vec<_> = s.run("cl tip#0\n").iter().map(|r| r.pools.clone()).collect();
    let walk = taken(&[built(&a2), built(&a1)], &[]);
    // V enforces at the record (D193).
    assert_eq!(pools, [vec![Open, Update(true, walk), Open], vec![]]);
}

/// A step's `DisconnectTip` that fails (the undo file is gone, so `DisconnectBlock` cannot read
/// the record) drains the pool, empty, without `fAddToMempool`, then aborts: "Failed to
/// disconnect block".
// Core: src/validation.cpp:3725-3735 at the pin
#[test]
fn a_failed_disconnect_in_a_step_drains_its_pool_then_aborts() {
    let mut s = Sides::new();
    s.run("extend main 1\nfork 1 2 hold\n");
    s.lose("rev00000.dat");
    let ran = s.node_only("deliver b1\n");
    let [(kept, none), (reorg, pools)] = &ran[..] else {
        panic!("{} ops", ran.len());
    };
    assert_eq!(kept.reply.outcome, Ok("inconclusive".into()), "equal work, stored");
    assert!(none.is_empty(), "a store opens no pool");
    assert_eq!(pools, &[Open, Update(false, vec![])]);
    assert!(stopped(reorg), "{:?}", reorg.reply.outcome);
    assert!(reorg.post.down, "the node aborted");
}

/// A step's `ConnectTip` whose block cannot be read (the block file was lost after the block was
/// stored, before its parent arrived) is a system error: the pool drained, empty, without
/// `fAddToMempool`, then the abort. The parent connects in a step of its own (a better tip ends
/// a step), so the failed read is the second step's.
// Core: src/validation.cpp:3516 (ConnectTip's "Failed to read block"), :3766-3773, :3776-3781
// at the pin
#[test]
fn a_system_error_in_a_step_drains_its_pool_then_aborts() {
    let mut s = Sides::new();
    s.run("extend main 2 headers hold\n");
    let [first, second] = [s.built(0), s.built(1)].map(|b| b.built_bytes());
    let stored = s.node_step(&ChainOp::Block(second));
    assert_eq!(stored.reply.outcome, Ok("inconclusive".into()), "no parent data");
    assert!(s.node.pools().take_calls().is_empty(), "a store opens no pool");
    s.lose("blk00000.dat");
    let failed = s.node_step(&ChainOp::Block(first));
    assert_eq!(s.node.pools().take_calls(), [Open, Open, Update(false, vec![])]);
    assert!(stopped(&failed), "{:?}", failed.reply.outcome);
}

/// `InvalidateBlock`'s `DisconnectTip` that fails: the pool drained, empty, without
/// `fAddToMempool` (`ret` is false), and `InvalidateBlock` returns false with a valid state, so the RPC runs
/// `ActivateBestChain` and returns null; nothing was disconnected or marked, and the node serves.
// Core: src/validation.cpp:4074-4082 at the pin, src/rpc/blockchain.cpp:2010-2018
#[test]
fn a_failed_disconnect_in_an_invalidation_drains_its_pool_and_the_node_serves() {
    let mut s = Sides::new();
    s.run("extend main 2\n");
    let tip = s.main(0);
    s.lose("rev00000.dat");
    let ran = s.node_only("invalidate main@-0\n");
    let [(step, pools)] = &ran[..] else {
        panic!("{} ops", ran.len());
    };
    assert_eq!(pools, &[Open, Update(false, vec![])]);
    assert_eq!(step.reply.outcome, Ok(Value::Null));
    assert_eq!(step.post.tip.map(|t| t.1.0), Some(tip.hash), "nothing disconnected");
    assert!(!s.status(&tip).has(Status::FAILED_VALID), "nothing marked");
}

/// `MarkConflictingBlock`'s `DisconnectTip` that fails: the mark's pool drained, empty, without
/// `fAddToMempool`, then `EnforceBlock`'s assert stops the node.
// Core: src/validation.cpp:4223-4230 (MarkConflictingBlock), :4188-4192 (EnforceBlock) at the pin
#[test]
fn a_failed_disconnect_in_a_mark_drains_its_pool_then_aborts() {
    let mut s = Sides::new();
    s.run("extend main 2\nfork 2 1\n");
    let fork = s.branch(0);
    s.lose("rev00000.dat");
    let height = i32::try_from(fork.height).unwrap();
    let recorded = s.node.record_verified_chainlock(height, fork.hash, [0; 96]);
    assert_eq!(s.node.pools().take_calls(), [Open, Update(false, vec![])]);
    assert!(recorded.post.down, "the node stopped");
}
