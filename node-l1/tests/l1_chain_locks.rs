//! CH3b b3: the ChainLock entry points of CH3 §4.5, its two CONS-11 restart cases, and the
//! disconnect pools' calls of §4.3 and §7.6, on `diff_chain`'s two sides, Core's session and
//! `NodeSut`'s V given the same submissions. Each op is held the same on both sides (reply,
//! events, and every post field the node reads: tip, chain tips, the UTXO digest, the index
//! digest with every block's status, the persisted image, the records).
//!
//! The entry points: a block that conflicts with a lock at connect, a proposal that conflicts,
//! and `FindMostWorkChain` over a conflicting ancestor. A lock on a fork and `RECONSIDER`'s
//! `ignore_chainlocks` are b2a's (`l1_chain_reorg`). `CL_RECORD`'s outcomes (`seen`, `stale`,
//! `not-better`, `accepted`) follow `ProcessNewChainLock`'s order, spork 19's flips and the
//! schedule seam included, with the seen set rebuilt at a restore and a `RESTART`. The CONS-11
//! cases take each kind of mark through `FLUSH`es and a killed `RESTART`, and their twin through
//! a clean one (BL-271), from both sides' saved images.
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

use chain_sides::{
    assert_steps, at_abi_33, core_step, node_ran, node_step, private_dir, sides, taken,
};
use dash_chain::Status;
use dash_oracle_sys::{ChainHash, ChainOp, CheckResult, Checked, Post, RestartKind};
use dash_primitives::BlockHash;
use dashd_chainstate::reorg::PoolCall::{self, Open, Update};
use node_l1::chain::build::Built;
use node_l1::chain::census::Census;
use node_l1::chain::core::{CoreSut, SPORK_19, SPORK_OFF, Schedule, T_RECENT};
use node_l1::chain::diff::as_judged;
use node_l1::chain::generate::{Gen, due_enforce};
use node_l1::chain::intent::Program;
use node_l1::chain::node::{IndexEntry, NodeSide, NodeStep, V_STOPPED};
use node_l1::chain::run::op_kind;
use node_l1::chain::step::Step;
use node_l1::chain::world::{Block, Hash, World, display_hash};
use serde_json::Value;

/// One op held the same on both sides: Core's step, and V's pool calls during it.
struct Ran {
    op: ChainOp,
    core: Step,
    pools: Vec<PoolCall>,
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
            _dir: dir,
        }
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
        &self.ran[from..]
    }

    /// `op` on both sides, held the same. A `RESTART` is held as any op: its teardown's and
    /// load's signals, its load's checks and the persisted image.
    fn both(&mut self, op: ChainOp) {
        let core = core_step(&mut self.core, &op);
        let node = node_ran(&mut self.node, &op);
        let entries = self.node.index().unwrap_or_default();
        held(&core, &node, &entries, op_kind(&op));
        let pools = self.node.pools().take_calls();
        self.ran.push(Ran {
            op,
            core,
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
        node_step(&mut self.node, op)
    }

    /// `op` on both sides, held the same, the world following: Core's step.
    fn direct(&mut self, op: ChainOp) -> Step {
        self.both(op);
        let ran = self.ran.last().unwrap();
        self.w.observe(&ran.op, &ran.core);
        ran.core.clone()
    }

    /// `CL_RECORD` of a lock on `block` at `height`, signed [`SIGS`]`[sig]`, on both sides:
    /// Core's step.
    fn lock(&mut self, height: i32, block: &Block, sig: usize) -> Step {
        self.direct(ChainOp::ClRecord {
            height,
            hash: block.hash,
            sig: signature(sig),
        })
    }

    /// Step 0 again on both sides (the image at [`T_RECENT`]), held the same, and the image's
    /// world.
    fn restore(&mut self) {
        let core = self.core.restore(T_RECENT).unwrap().step;
        let node = self.node.restore(T_RECENT).unwrap();
        held(&core, &node, &self.node.index().unwrap_or_default(), "restore");
        self.node.pools().take_calls();
        self.w = chain_sides::loaded_world();
        self.generator = Gen::default();
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

/// The two sides' steps for one op held the same ([`assert_steps`]), the events taken as Diff
/// mode takes them (`diff::as_judged`): past `LimitValidationInterfaceQueue`'s bound a drain
/// inside the op only interleaves its two kinds of signal.
fn held(core: &Step, node: &NodeStep, entries: &[IndexEntry], what: &str) {
    let (core, node) = as_judged(core, node);
    assert_steps(&core, &node, entries, what);
}

/// `block`'s row's status in `post`'s `getchaintips`.
fn tip_status<'a>(post: &'a Post, block: &Block) -> &'a str {
    let tips = post.tips.as_ref().expect("serving");
    let row = tips.iter().find(|t| t["hash"].as_str().and_then(display_hash) == Some(block.hash));
    row.and_then(|t| t["status"].as_str()).unwrap_or_else(|| panic!("no row: {tips:?}"))
}

/// `block`'s height as a lock names it.
fn height(block: &Block) -> i32 {
    i32::try_from(block.height).unwrap()
}

/// Two valid BLS signatures (mainnet's ChainLocks at 2243495 and 2243496): the shim reads a
/// lock's signature as a `CBLSSignature`, so an invalid encoding comes out the null signature,
/// and two invalid ones are one lock to Core.
const SIGS: [&str; 2] = [
    "a6bc4dcf7afb042e0b0258a994f5a77856971a32a3ad3ee89d21e1011a77211070bec7c2ef50c293722cbae135b904640b482479f836120e0be7d42ce332a7c58096d8d8006920ef3dbcc47b5f7ed00aeb68d58bc514f4401bd72b247bf23699",
    "88270e60bee7dd9cea3c0a1b85e51d52f01e55a35033ef0434979b9121bc07ed8e45adae1f99e4d8fa2ea760920d844e1383030103b1c503cee45a2fcddc5cd7e73d1823d199e8231fadee2b3cadb1c6fc2ea255b988334b47d35ce865275699",
];

/// [`SIGS`]`[k]`'s bytes.
fn signature(k: usize) -> [u8; 96] {
    let hex = SIGS[k].as_bytes();
    std::array::from_fn(|i| {
        u8::from_str_radix(std::str::from_utf8(&hex[2 * i..2 * i + 2]).unwrap(), 16).unwrap()
    })
}

/// Spork 19 on or off.
fn spork_19(on: bool) -> ChainOp {
    ChainOp::Spork {
        id: SPORK_19,
        value: if on {
            0
        } else {
            SPORK_OFF
        },
    }
}

/// `CL_RECORD`'s outcome in `step`'s reply.
fn outcome(step: &Step) -> &str {
    let text = step.reply.outcome.as_ref().ok().and_then(Value::as_str);
    text.unwrap_or_else(|| panic!("{:?}", step.reply))
}

/// The best lock in `step`'s post (`GetBestChainLock`): its height and block.
fn best_lock(step: &Step) -> Option<(i32, Hash)> {
    step.post.best_cl.expect("serving").map(|b| (b.0, b.1.0))
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
    let mut s = Sides::new();
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
    let mut s = Sides::new();
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
/// through a kill, and the marks come back as through a kill. The node's shutdown flushes run on
/// what V hands back as it stops (DEC-613), so its load matches. The clean teardown's signals
/// are held too, which needs the oracle at ABI 33 (the M-4 shim fix).
// Core: src/init.cpp:379-385, :449-455 at the pin
#[test]
fn the_cons_11_cases_across_a_clean_restart() {
    if !at_abi_33("the_cons_11_cases_across_a_clean_restart", "the M-4 clean teardown's events") {
        return;
    }
    a_conflict_mark_find_most_work_chain_made_across(RestartKind::Clean);
    a_mark_conflicting_block_mark_across(RestartKind::Clean);
}

/// `CL_RECORD` in `ProcessNewChainLock`'s order on both sides: a lock seen before is `seen` (the
/// key is the whole lock, its signature included, and a stale lock is remembered too), one at or
/// below the best lock's height `stale`, a better one `accepted`, and nothing enforced until
/// `CL_ENFORCE`. A lock on a known block at another height raises the best lock's height but is
/// `not-better`: the lock with a known block stays the one `CL_ENFORCE` enforces, so the fork
/// block it names is left alone.
// Core: src/chainlock/handler.cpp:139-206, src/chainlock/chainlock.cpp:106-131 at the pin
#[test]
fn cl_record_answers_as_core_s_handler() {
    let mut s = Sides::new();
    s.run("extend main 2\nfork 1 1\n");
    let (a0, a1, a2, fork) = (s.main(2), s.main(1), s.main(0), s.branch(0));
    assert_eq!(fork.height, a2.height, "the fork block beside the tip");
    let mut lock = |b: &Block, sig| outcome(&s.lock(height(b), b, sig)).to_owned();
    assert_eq!(lock(&a1, 0), "accepted");
    assert_eq!(lock(&a1, 0), "seen", "the same lock again");
    assert_eq!(lock(&a1, 1), "stale", "another signature: another lock");
    assert_eq!(lock(&a1, 1), "seen", "a stale lock is remembered");
    assert_eq!(lock(&a0, 0), "stale", "below the best lock");
    let taken = s.lock(height(&a2), &a2, 0);
    assert_eq!((outcome(&taken), best_lock(&taken)), ("accepted", Some((height(&a2), a2.hash))));
    assert!(taken.events.is_empty(), "nothing enforced at CL_RECORD: {:?}", taken.events);
    let wrong = s.lock(height(&fork) + 1, &fork, 0);
    assert_eq!(outcome(&wrong), "not-better", "the fork block is at another height");
    assert_eq!(best_lock(&wrong), Some((height(&fork) + 1, fork.hash)), "bestChainLock moved");
    let enforced = s.direct(ChainOp::ClEnforce);
    assert_eq!(enforced.post.tip.map(|t| t.1.0), Some(a2.hash), "the known block's lock holds");
    assert!(!s.status(&fork).has(Status::CONFLICT_CHAINLOCK), "the fork is not marked");
    assert_eq!(outcome(&s.lock(height(&a2) + 1, &a2, 0)), "stale", "below the moved height");
}

/// A lock on a block not yet seen, header or data: `accepted` with no block to check the height
/// against, the best lock without a known block. Its header makes it the known block's
/// (`AcceptedBlockHeader`), and once the data is in it is active.
// Core: src/chainlock/chainlock.cpp:106-131, :152-170 at the pin
#[test]
fn a_lock_before_its_header_is_taken_and_holds_once_the_block_is_in() {
    let mut s = Sides::new();
    s.run("extend main 1 hold\n");
    let late = s.built(0);
    let taken = s.lock(height(&late), &late, 0);
    assert_eq!(outcome(&taken), "accepted", "no block to check the height against");
    assert_eq!(best_lock(&taken), Some((height(&late), late.hash)));
    s.direct(ChainOp::Header(built(&late).header));
    s.direct(ChainOp::ClEnforce);
    let connected = s.direct(ChainOp::Block(late.built_bytes()));
    assert_eq!(connected.post.tip.map(|t| t.1.0), Some(late.hash), "the locked block is active");
    assert_eq!(best_lock(&connected), Some((height(&late), late.hash)));
}

/// The seen set goes with the process: Core's handler is rebuilt at a restore (step 0) and at a
/// `RESTART`, so the same lock is `accepted` again after each.
// Core: src/test/oracle/oracle_chain.cpp:1311-1328 (the restore) at the pin
#[test]
fn the_seen_locks_go_with_the_process_at_a_restore_and_a_restart() {
    let mut s = Sides::new();
    let tip = s.main(0);
    assert_eq!(outcome(&s.lock(height(&tip), &tip, 0)), "accepted");
    assert_eq!(outcome(&s.lock(height(&tip), &tip, 0)), "seen");
    s.restore();
    assert_eq!(outcome(&s.lock(height(&tip), &tip, 0)), "accepted", "after a restore");
    s.direct(ChainOp::ClEnforce);
    s.direct(ChainOp::Restart(RestartKind::Kill));
    assert_eq!(outcome(&s.lock(height(&tip), &tip, 0)), "accepted", "after a RESTART");
}

/// Spork 19 off, then on again: `CL_RECORD`'s `CheckActiveState` runs before the seen check, so a
/// seen lock after the flip is still `seen`, but the handler first resets the best lock, as it
/// does when ChainLocks come back on. Off, the state is kept.
// Core: src/chainlock/handler.cpp:142-149, :249-262 at the pin
#[test]
fn a_seen_lock_after_a_spork_19_flip_resets_the_best_lock_first() {
    let mut s = Sides::new();
    let tip = s.main(0);
    let at = Some((height(&tip), tip.hash));
    assert_eq!(outcome(&s.lock(height(&tip), &tip, 0)), "accepted");
    s.direct(spork_19(false));
    let off = s.lock(height(&tip), &tip, 0);
    assert_eq!((outcome(&off), best_lock(&off)), ("seen", at), "off: the lock is kept");
    s.direct(spork_19(true));
    let on = s.lock(height(&tip), &tip, 0);
    assert_eq!((outcome(&on), best_lock(&on)), ("seen", None), "on again: reset, then seen");
}

/// The schedule seam (V's `ClSchedule::OnEnforceOnly`): Core's session registers no handler and
/// schedules no tip task, so a tip change while spork 19 is off runs no `CheckActiveState` (only
/// `CL_RECORD` and `CL_ENFORCE` do; the lock is enforced first, so none is due after the tip
/// change). The handler never sees the spork off, so the seen lock after it comes back on resets
/// nothing.
// Core: src/chainlock/handler.cpp:207-216, :225-247, src/test/oracle/oracle_chain.cpp:999-1003
// at the pin
#[test]
fn a_tip_change_while_spork_19_is_off_checks_nothing() {
    let mut s = Sides::new();
    let tip = s.main(0);
    let at = Some((height(&tip), tip.hash));
    assert_eq!(outcome(&s.lock(height(&tip), &tip, 0)), "accepted");
    s.direct(ChainOp::ClEnforce);
    s.direct(spork_19(false));
    let moved = s.run("extend main 1\n").last().unwrap().core.post.tip.map(|t| t.1.0);
    assert_ne!(moved, Some(tip.hash), "the tip changed");
    s.direct(spork_19(true));
    let again = s.lock(height(&tip), &tip, 0);
    assert_eq!((outcome(&again), best_lock(&again)), ("seen", at), "no reset");
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
    assert_eq!(pools, [vec![], vec![Open, Update(true, walk), Open]], "at CL_ENFORCE");
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
    let record = ChainOp::ClRecord {
        height,
        hash: fork.hash,
        sig: [0; 96],
    };
    let recorded = s.node_step(&record);
    assert_eq!(recorded.reply.outcome, Ok(Value::from("accepted")));
    assert_eq!(s.node.pools().take_calls(), [], "enforced at CL_ENFORCE only");
    let enforced = s.node_step(&ChainOp::ClEnforce);
    assert_eq!(s.node.pools().take_calls(), [Open, Update(false, vec![])]);
    assert!(enforced.post.down, "the node stopped");
}
