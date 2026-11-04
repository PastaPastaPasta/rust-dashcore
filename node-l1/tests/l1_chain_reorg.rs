//! CH3b b2a: the regtest reorg suites of CH3 §7.5 on `diff_chain`'s two sides, Core's session and
//! `NodeSut`'s V given the same submissions. Each op is held the same on both sides (reply,
//! events, and every post field the node reads: tip, chain tips, the UTXO digest, the index
//! digest with every block's status), except where a test says otherwise.
//!
//! The suites: reorgs of depth 1 to 30 over spent and restored coins, equal-work races and
//! `PRECIOUS`, `INVALIDATE` and `RECONSIDER` at the tip, mid-chain and on a fork, `RECONSIDER` of
//! a `BLOCK_RESULT_UNSET` failure, ChainLock enforcement onto a fork and `RECONSIDER`'s
//! `ignore_chainlocks` on the block it marked conflicting, and a reorg that runs into an invalid
//! block mid-batch, and a 300-block reorg whose disconnected transactions overflow V's disconnect
//! pool, which evicts (DEC-500). The legacy-undo reorg is `dashd-chainstate`'s, checked against
//! the connector fixture.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/chain_sides.rs"]
mod chain_sides;

use chain_sides::{
    assert_posts, assert_same, both, loaded_world, private_dir, same_program, same_program_in,
    sides, taken,
};
use dash_oracle_sys::{ChainOp, CheckResult, Checked, Event};
use dashd_chainstate::chain::RemovalReason;
use dashd_chainstate::reorg::PoolCall;
use node_l1::chain::build::{self, BlockPlan, Built, Input, Lock, REGTEST_BITS, VERSION_BITS_TOP};
use node_l1::chain::core::{CoreSut, Schedule};
use node_l1::chain::node::{IndexEntry, NodeSide};
use node_l1::chain::run::op_kind;
use node_l1::chain::step::Step;
use node_l1::chain::world::{Block, Coin, World};

/// `step`'s `BlockDisconnected` signals: the depth of its reorg.
fn disconnected(step: &Step) -> usize {
    step.events.iter().filter(|e| e.name == "BlockDisconnected").count()
}

/// `step`'s blocks out and in: its `BlockDisconnected` and `BlockConnected` signals.
fn moved(step: &Step) -> (usize, usize) {
    let connected = step.events.iter().filter(|e| e.name == "BlockConnected").count();
    (disconnected(step), connected)
}

/// The depth of each op's reorg, for the ops that reorg.
fn reorgs(steps: &[(ChainOp, Step)]) -> Vec<usize> {
    steps.iter().map(|(_, s)| disconnected(s)).filter(|&d| d > 0).collect()
}

/// Each `INVALIDATE` and `RECONSIDER` op's kind and blocks out and in ([`moved`]).
fn moves(steps: &[(ChainOp, Step)]) -> Vec<(&'static str, (usize, usize))> {
    let rpc = |op: &ChainOp| matches!(op_kind(op), "invalidate" | "reconsider");
    steps.iter().filter(|(op, _)| rpc(op)).map(|(op, s)| (op_kind(op), moved(s))).collect()
}

/// `text` on two fresh sides, each op held the same, with the world the generator built and
/// Core's steps.
fn run_in(text: &str) -> (World, Vec<(ChainOp, Step)>) {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    same_program_in(&mut core, &mut node, text)
}

/// [`run_in`]'s steps.
fn run(text: &str) -> Vec<(ChainOp, Step)> {
    run_in(text).1
}

/// Two ops' signals and `BlockChecked` lists, joined in order.
fn joined(a: &Step, b: &Step) -> (Vec<Event>, Vec<Checked>) {
    let events = [&a.events[..], &b.events[..]].concat();
    (events, [&a.reply.checked[..], &b.reply.checked[..]].concat())
}

/// A program whose last block reorgs `depth` blocks (1 to 31): one spending block on the image's
/// tip, a spending fork block beside it (equal work, no reorg), `depth - 1` more on the main
/// chain and `depth` on the fork, every block spending, at most 8 an intent (the generator's
/// bound). The first op extends the main chain: a fork's header as an input's first op is the
/// replay-latch gap (`l1_chain_diff`).
fn reorg_of_depth(depth: u8) -> String {
    let mut text = String::from("extend main 1 tx=spends\nfork 1 1 tx=spends\n");
    for (branch, mut n) in [("main", depth - 1), ("b1", depth)] {
        while n > 0 {
            let k = n.min(8);
            text += &format!("extend {branch} {k} tx=spends\n");
            n -= k;
        }
    }
    text
}

/// Reorgs of depth 1 to 30, both branches spending: each disconnect restores the coins the main
/// chain spent and removes the ones it created, and the fork's blocks spend again. The UTXO
/// digest is compared after every op, and every block the program built spends.
#[test]
fn reorgs_of_depth_1_to_30_restore_and_respend_the_coins() {
    for depth in [1, 2, 3, 8, 11, 20, 30] {
        let text = reorg_of_depth(depth);
        let (w, steps) = run_in(&text);
        assert_eq!(reorgs(&steps), [usize::from(depth)], "{text:?}");
        let built = &w.blocks[w.image_tip + 1..];
        assert_eq!(built.len(), 2 * usize::from(depth) + 1, "{text:?}: the blocks built");
        assert!(built.iter().all(|b| !b.spends.is_empty()), "{text:?}: every block spends");
    }
}

/// The eviction case's depth: the main chain's blocks, each holding a [`build::fat`] transaction.
const DEPTH: usize = 300;

/// A [`build::fat`] transaction's data outputs: about 99 kB, so that `DEPTH` of them pass the
/// disconnect pool's 20,000,000-byte budget by half again.
const FAT_DATA: usize = 10;

/// `n` blocks on the image's tip, each a second after its parent, their coinbases tagged from
/// `tag`. With `coin`, each holds a [`build::fat`] transaction spending the previous one's
/// change, the first spending `coin`.
fn branch(w: &World, n: usize, coin: Option<Coin>, tag: u64) -> Vec<Built> {
    let tip = &w.blocks[w.image_tip];
    let (mut prev, mut height, mut time) = (tip.hash, tip.height, tip.time);
    let mut change = coin.map(|c| (c.outpoint, c.value));
    (tag..)
        .take(n)
        .map(|tag| {
            (height, time) = (height + 1, time + 1);
            let fat = change.map(|(outpoint, value)| {
                let tx = build::fat(Input::new(outpoint, Vec::new()), value, FAT_DATA);
                change = Some(((tx.txid(), 0), value - build::FAT_FEE));
                tx
            });
            let fees = fat.as_ref().map_or(0, |_| build::FAT_FEE);
            let coinbase = build::coinbase(height, build::subsidy(height) + fees, tag);
            let built = BlockPlan {
                prev,
                version: VERSION_BITS_TOP,
                time,
                bits: REGTEST_BITS,
                txs: std::iter::once(coinbase).chain(fat).collect(),
                merkle_override: None,
            }
            .build();
            prev = built.hash;
            built
        })
        .collect()
}

/// CH3 §7.5's eviction case (DEC-500): 300 main blocks of a fat transaction each
/// ([`build::fat`], §7.5's `tx=fat`), then a fork of 301 empty blocks beside them, delivered in
/// order. The last fork block reorgs all 300 in one `ActivateBestChainStep`, which connects in ten
/// batches of at most 32 but returns only once the new tip has more work than the old, at the
/// fork's last block: one `UpdatedBlockTip`. The disconnected
/// transactions (about 30 MB of Core's accounting) overflow the pool's budget. V's pool evicts
/// and the reorg completes; each op is held the same as Core's, whose session has no mempool and
/// so keeps no pool. FR-CHS-07: no depth limit beyond Core's, and the pool evicts but never
/// refuses. The step's one pool loses nothing: what it evicted from its front, then what it
/// drained, are the main chain's transactions in the order it queued them (the fork's coinbases
/// are others, so no connect takes one back).
// Core: src/validation.cpp:3412-3418, :3743-3784, :3786-3790, :3938 at the pin,
// src/kernel/disconnected_transactions.h:50-61
#[test]
fn a_300_block_reorg_overflows_the_disconnect_pool_which_evicts() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let w = loaded_world();
    let height = w.blocks[w.image_tip].height + 1;
    let fees = i64::try_from(DEPTH).unwrap() * build::FAT_FEE;
    let coin = w
        .utxos(w.image_tip)
        .into_values()
        .find(|c| c.lock == Lock::True && c.mature_at(height) && c.value > fees)
        .expect("a mature coin that pays the main chain's fees");
    let main = branch(&w, DEPTH, Some(coin), 1_000);
    let fork = branch(&w, DEPTH + 1, None, 2_000);
    let (last, stored) = fork.split_last().unwrap();
    let block = |core: &mut CoreSut, node: &mut NodeSide, b: &Built| {
        assert_same(core, node, &ChainOp::Block(b.bytes.clone()))
    };
    for b in &main {
        assert_eq!(moved(&block(&mut core, &mut node, b)), (0, 1), "the main chain connects");
    }
    for b in stored {
        assert_eq!(moved(&block(&mut core, &mut node, b)), (0, 0), "no heavier: stored only");
    }
    assert_eq!(node.pools().evicted_bytes(), 0, "nothing disconnected yet");
    node.pools().take_calls();
    let reorg = block(&mut core, &mut node, last);
    assert_eq!(moved(&reorg), (DEPTH, DEPTH + 1));
    assert_eq!(reorg.post.tip.map(|t| t.1.0), Some(last.hash), "the fork is active");
    let tips = reorg.events.iter().filter(|e| e.name == "UpdatedBlockTip").count();
    assert_eq!(tips, 1, "one UpdatedBlockTip");
    let evicted = node.pools().evicted_bytes();
    eprintln!("eviction: {evicted} bytes evicted");
    assert!(evicted > 0, "V's pool evicted");
    let calls = node.pools().take_calls();
    let [PoolCall::Open, removed @ .., PoolCall::Update(true, drained)] = &calls[..] else {
        panic!("one step's pool: {calls:?}");
    };
    let removed = removed.iter().map(|c| match c {
        PoolCall::Remove(txid, RemovalReason::Reorg) => *txid,
        other => panic!("{other:?}"),
    });
    let main: Vec<_> = main.iter().rev().collect();
    assert_eq!(removed.chain(drained.iter().copied()).collect::<Vec<_>>(), taken(&main, &[]));
}

/// Equal-work races: two forks beside the main chain's tip extended in turn, each reaching the
/// other's work (a tie keeps the first seen) and then passing it, three reorgs of growing depth;
/// `PRECIOUS` between equal-work tips with spent coins, back and forth and then built on; and
/// `PRECIOUS` of a tip with less work, which changes nothing. (`extend main` extends the active
/// chain, so a race needs two forks.)
#[test]
fn equal_work_races_and_precious_are_the_same_on_both_sides() {
    let races: [(&str, &[usize]); 3] = [
        (
            "extend main 1\nfork 1 1 tx=spends\nfork 1 1 tx=spends\nextend b1 1 tx=spends\n\
             extend b2 2 tx=spends\nextend b1 2 tx=spends\n",
            &[1, 2, 3],
        ),
        (
            "extend main 2 tx=spends\nfork 1 1 tx=spends\nprecious tip#0\nprecious built#1\n\
             precious tip#0\nextend main 1 tx=spends\n",
            &[1, 1, 1],
        ),
        ("extend main 2\nfork 2 1\nprecious tip#0\n", &[]),
    ];
    for (text, want) in races {
        assert_eq!(reorgs(&run(text)), want, "{text:?}: reorg depths");
    }
}

/// `INVALIDATE` and `RECONSIDER` over spent coins: of the tip; mid-chain, with a new branch built
/// on the invalidated block's parent and the old one then reconsidered back; on a fork, twice, to
/// an equal-work tip, then reconsidered from above both.
#[test]
fn invalidate_and_reconsider_at_the_tip_mid_chain_and_on_a_fork() {
    type Moves = &'static [(&'static str, (usize, usize))];
    let programs: [(&str, Moves); 3] = [
        (
            "extend main 3 tx=spends\ninvalidate main@-0\nreconsider built#2\n",
            &[("invalidate", (1, 0)), ("reconsider", (0, 1))],
        ),
        (
            "extend main 5 tx=spends\ninvalidate main@-3\nextend main 2 tx=spends\n\
             reconsider built#4\n",
            &[("invalidate", (4, 0)), ("reconsider", (2, 4))],
        ),
        (
            "extend main 2 tx=spends\nfork 1 3 tx=spends\ninvalidate tip#0\ninvalidate built#3\n\
             reconsider built#4\n",
            &[("invalidate", (1, 0)), ("invalidate", (2, 1)), ("reconsider", (1, 3))],
        ),
    ];
    let got = programs.map(|(text, _)| moves(&run(text)));
    assert_eq!(got, programs.map(|(_, want)| want.to_vec()), "each op's blocks out and in");
}

/// A `BLOCK_RESULT_UNSET` failure (`bad-cb-amount`: `FAILED_VALID`, not punished) is cleared by
/// `reconsiderblock` like any other (CH3 §2.7, FR-CON-32): the block is connected again, fails
/// again with the same reason, and both sides end where they were.
#[test]
fn reconsider_clears_a_result_unset_failure() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let steps =
        same_program(&mut core, &mut node, "extend main 1\nmutate cb_amount\nextend main 1\n");
    let failed = |s: &Step| {
        let bad = s.reply.checked.iter().find(|c| c.1 == CheckResult::Invalid)?;
        (bad.3.as_deref() == Some("bad-cb-amount")).then_some(bad.0)
    };
    let bad = steps.iter().find_map(|(_, s)| failed(s)).expect("a bad-cb-amount block");
    let op = ChainOp::Reconsider {
        hash: bad.0,
        ignore_chainlocks: false,
    };
    let again = assert_same(&mut core, &mut node, &op);
    assert_eq!(failed(&again), Some(bad), "checked again, with the same reason");
    let before = &steps.last().unwrap().1.post;
    assert_eq!((&again.post.tip, &again.post.index), (&before.tip, &before.index), "where it was");
}

/// A ChainLock on the fork of `extend main 2, fork 2 1` (both spending), recorded and enforced:
/// Core through `CL_RECORD` and `CL_ENFORCE`; the node records the same lock on V as LK1a will
/// once the signature verifies ([`NodeSide::record_verified_chainlock`]), then `CL_ENFORCE`.
///
/// [`NodeSide::record_verified_chainlock`]: node_l1::chain::node::NodeSide::record_verified_chainlock
struct Locked {
    w: World,
    /// The record's steps, Core's then the node's.
    record: (Step, Step),
    /// `CL_ENFORCE`'s steps, Core's then the node's, and V's index after it.
    enforce: (Step, Step, Vec<IndexEntry>),
}

fn lock_the_fork(core: &mut CoreSut, node: &mut NodeSide) -> Locked {
    let (w, _) = same_program_in(core, node, "extend main 2 tx=spends\nfork 2 1 tx=spends\n");
    let fork = &w.blocks[w.branches[0]];
    let (height, hash, sig) = (i32::try_from(fork.height).unwrap(), fork.hash, [0; 96]);
    let record = ChainOp::ClRecord {
        height,
        hash,
        sig,
    };
    let core_record = core.step(&record).unwrap().step;
    let node_record = node.record_verified_chainlock(height, hash, sig);
    let enforce = both(core, node, &ChainOp::ClEnforce);
    Locked {
        w,
        record: (core_record, node_record),
        enforce,
    }
}

/// A ChainLock on a fork block with less work than the active chain, enforced (CH3 §4.5): the
/// conflicting main block is marked and the fork activated ([`lock_the_fork`]). V enforces it at
/// the record where Core's session waits for `CL_ENFORCE`, so the reorg's signals and
/// `BlockChecked` fall in the record's step on the node and in `CL_ENFORCE`'s on Core: the two
/// ops' signals and checks, joined in order, are held the same, as are their replies, the posts
/// after `CL_ENFORCE`, and every op before.
#[test]
fn a_chainlock_on_a_fork_is_enforced_onto_it() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let Locked {
        w,
        record: (core_record, node_record),
        enforce: (c, n, entries),
    } = lock_the_fork(&mut core, &mut node);
    let fork = &w.blocks[w.branches[0]];
    assert!(fork.height < w.blocks[w.tip].height, "the fork has less work");
    assert_eq!(core_record.reply.outcome, Ok("accepted".into()), "Core took the lock");
    assert_eq!(node_record.reply.outcome, core_record.reply.outcome, "cl_record: reply");
    assert_eq!(c.reply.outcome, n.reply.outcome, "cl_enforce: reply");
    assert_eq!((disconnected(&core_record), disconnected(&c)), (0, 2), "Core reorgs at CL_ENFORCE");
    assert_eq!(disconnected(&node_record), 2, "V reorgs at the record");
    assert_eq!(joined(&node_record, &n), joined(&core_record, &c), "the pair's signals");
    let checked = c.reply.checked.iter().map(|c| (c.0.0, c.1)).collect::<Vec<_>>();
    assert_eq!(checked, [(fork.hash, CheckResult::Valid)], "Core connects the fork at CL_ENFORCE");
    assert_posts(&c.post, &n.post, &entries, "cl_enforce");
    assert_eq!(c.post.tip.map(|t| t.1.0), Some(fork.hash), "the locked fork is active");
}

/// `RECONSIDER` of the main block the lock marked conflicting ([`lock_the_fork`]), each op the same
/// on both sides. Without `ignore_chainlocks` the mark stays (`ResetBlockFailureFlags` clears it
/// only with the flag) and nothing is tried. With it the mark goes and the main block, of equal
/// work and seen first, is tried, but `ConnectBlock` still refuses it against the lock
/// (`bad-chainlock`): the fork goes out and comes back in, and the lock holds.
// Core: src/validation.cpp:4321-4323, :4338-4340 (the flag), :2568-2571 (ConnectBlockImpl) at the pin
#[test]
fn reconsider_clears_a_chainlock_conflict_only_with_ignore_chainlocks() {
    let dir = private_dir();
    let (mut core, mut node) = sides(Schedule::S0, &dir);
    let w = lock_the_fork(&mut core, &mut node).w;
    let fork = &w.blocks[w.branches[0]];
    let beside =
        |b: &&Block| b.hash != fork.hash && (b.height, b.parent) == (fork.height, fork.parent);
    let conflicted = w.blocks.iter().find(beside).expect("the main block beside the fork").hash;
    let reconsider = |ignore_chainlocks| ChainOp::Reconsider {
        hash: conflicted,
        ignore_chainlocks,
    };
    let kept = assert_same(&mut core, &mut node, &reconsider(false));
    assert_eq!((moved(&kept), kept.reply.checked.len()), ((0, 0), 0), "the mark stays");
    let tried = assert_same(&mut core, &mut node, &reconsider(true));
    let checked = tried.reply.checked.iter().map(|c| (c.0.0, c.1, c.3.as_deref()));
    assert_eq!(
        checked.collect::<Vec<_>>(),
        [
            (conflicted, CheckResult::Invalid, Some("bad-chainlock")),
            (fork.hash, CheckResult::Valid, None)
        ],
        "the main block is tried and refused against the lock"
    );
    assert_eq!(moved(&tried), (1, 1), "the fork out and back in");
    assert_eq!(tried.post.tip.map(|t| t.1.0), Some(fork.hash), "the lock holds");
}

/// A reorg whose batch runs into an invalid block: the fork's headers first, then its blocks
/// highest first, so the lowest one's arrival connects the fork in one batch. Its second block
/// fails (`bad-cb-amount`), both sides mark the same blocks failed (the index digest holds
/// every status; CH3 §4.4's batch-top marking), disconnect the fork's first block again and
/// reconnect the main chain, all in that one op.
#[test]
fn a_reorg_into_an_invalid_block_mid_batch() {
    let text = "extend main 1\nfork 2 1 headers hold\nmutate cb_amount\nextend b1 1 headers hold\n\
                extend b1 2 headers hold\ndeliver b1 rev\n";
    let steps = run(text);
    let failed = steps.iter().flat_map(|(_, s)| &s.reply.checked);
    let failed = failed.filter(|c| c.3.as_deref() == Some("bad-cb-amount")).count();
    assert_eq!(failed, 1, "{text:?}: one failure");
    assert_eq!(reorgs(&steps), [3], "{text:?}: two main blocks out, the fork's first out again");
    let tip = |k: usize| steps[k].1.post.tip;
    assert_eq!(tip(steps.len() - 1), tip(0), "{text:?}: back on the main chain");
}
