//! VerifyDB's levels 3 and 4 through V (CH5-1a; `validation.cpp:5042-5189` at v24.0.0): the
//! scratch disconnect walk, its UNCLEAN early return, the reconnect from the walk's last block,
//! the level-3 gate (DEC-981a) and the evo skip (BL-440, DEC-993), over the production scratch
//! view the connector builds. The process DIFF against dashd is `rdashd/tests/verifychain.rs`.

use super::disconnect::{
    UNDO_SPECIAL_FAILED, arm_undo_refused_after_an_assert, arm_undo_store_fault, assert_in_order,
    assert_nothing_after_the_assert, edit, record_len, reported_once, spent, undo_record, write_at,
};
use super::*;
use crate::chain::seams::ScratchParent;
use crate::chain::seams::{LocalShutdown, ShutdownFlag};
use crate::chain::tests::node_stores::{Seams, Stores as NodeStores};
use crate::chain::tests::support::{branch, install_accept_log, lines_on};
use crate::chain::{L3Skip, VerifyDbArgs, VerifyDbResult, WaitError};
use crate::connect::verify_evo_capability;
use dash_consensus::coins::BlockUndo;
use dashd_store::flatfile::undo_checksum;

/// `b1` spends seed `n` into `t1` (49 DASH); `b2` spends it into `t2` (48), `b3` into `t3` (47).
fn three(n: u8, setup: Setup) -> (World, [Mined; 3], [Arc<Tx>; 3]) {
    let (w, b1, t1) = spent(n, setup);
    let p = w.rig.p.clone();
    let t2 = spend(&[op(&t1, 0)], 48 * COIN);
    let b2 = block_on(&p, &b1, vec![t2.clone()]);
    w.rig.accept(&b2);
    let t3 = spend(&[op(&t2, 0)], 47 * COIN);
    let b3 = block_on(&p, &b2, vec![t3.clone()]);
    w.rig.accept(&b3);
    assert_eq!(w.rig.v.tip_watch().get(), Some(b3.hash()));
    (w, [b1, b2, b3], [t1, t2, t3])
}

fn verify(w: &World, level: i32, depth: i32) -> VerifyDbResult {
    install_accept_log();
    w.rig
        .v
        .verify_db(VerifyDbArgs {
            level,
            depth,
        })
        .recv()
        .expect("V answers")
}

/// The scratch connects so far: the harness records each connect's undo.
fn connects(w: &World) -> usize {
    w.seen.lock().expect("seen").undo.len()
}

/// The active coins' best block, membership at `ops`, and every block's status.
fn state(w: &World, ops: &[OutPoint], blocks: &[Mined]) -> impl PartialEq + std::fmt::Debug {
    let coins = w.coins.lock().expect("coins");
    let held: Vec<_> = ops.iter().map(|o| coins.have_in_cache(&OutPointKey::new(o))).collect();
    let best = coins.best_block().map(|b| b.hash);
    drop(coins);
    let status: Vec<_> = blocks.iter().map(|m| w.rig.status(m)).collect();
    (best, held, status)
}

/// The coinbase of `m`'s first output, rewritten in the active coins to `value + delta`: Core's
/// L3 fixture, a coin the range created with another value (CH5-1a, `fx/PROVENANCE.md`).
fn rewrite_coinbase(w: &World, m: &Mined, height: u32, delta: Amount) {
    let cb = &m.block.txs()[0];
    let k = OutPointKey::new(&op(cb, 0));
    let mut out = cb.outputs()[0].clone();
    out = dash_primitives::TxOut::from_duffs(out.value() + delta, out.script_pubkey().clone());
    edit(w, |ov| {
        ov.spend(&k).expect("reads").expect("unspent");
        let src = CoinSrc::Output {
            out: &out,
            height,
            coinbase: true,
        };
        ov.add(&k, src, true).expect("adds");
    });
}

/// Every level over a healthy chain and every depth: true, nothing in the active coins or the
/// index changes but the clean fills, and the walk's lines are Core's.
#[test]
fn a_healthy_chain_verifies_at_every_level_and_depth() {
    let (w, blocks, txs) = three(60, Setup::default());
    let ops: Vec<_> = txs.iter().map(|t| op(t, 0)).collect();
    let before = state(&w, &ops, &blocks);
    let evo = w.seen.lock().expect("seen").evo_best;
    for level in 0..=5 {
        for depth in [1, 2, 3, 0, 9, -1] {
            let got = verify(&w, level, depth);
            if got != VerifyDbResult::Success {
                let lines = lines_on(w.rig.v_thread());
                panic!("{level} {depth}: {got:?} {:#?}", &lines[lines.len().saturating_sub(12)..]);
            }
        }
    }
    assert_eq!(state(&w, &ops, &blocks), before);
    assert_eq!(w.seen.lock().expect("seen").evo_best, evo);
    assert!(w.sink.messages().is_empty());
    let lines = lines_on(w.rig.v_thread());
    for want in [
        "Verifying last 3 blocks at level 4",
        "Verification: No coin database inconsistencies in last 3 blocks (6 transactions)",
        "Verification: No coin database inconsistencies in last 2 blocks (0 transactions)",
    ] {
        assert!(lines.iter().any(|l| l == want), "{want}: {lines:#?}");
    }
}

/// CH5 D1 (BL-795): VerifyDB's scratch disconnects have no `DisconnectTip` around them; the
/// scheme guard is the walk's (`validation.cpp:5056` at v24.0.0), which exits once per walk, and
/// nothing else exits it.
#[test]
fn a_verifychain_disconnect_exits_its_scheme_guard_once() {
    let (w, _, _) = three(179, Setup::default());
    let v = w.rig.v_thread();
    assert_eq!(verify(&w, 3, 2), VerifyDbResult::Success);
    let exits = |log: bool| levels_on(v, &format!("{GUARD_EXIT}{log}")).len();
    assert_eq!((exits(true), exits(false)), (1, 0), "one exit per walk");
}

/// BL-795, OD-CP-182 (CH5 D1): VerifyDB's scheme guard (`validation.cpp:5056` at v24.0.0) is the
/// walk's, never committed, so it exits once per walk, after the walk's last line: `:5180`'s, an
/// UNCLEAN disconnect's (`:5142-5145`) or an unconnectable block's (`:5172-5174`), all at v24.0.0.
/// The rig's hooks log each uncommitted exit with its `log`.
#[test]
fn the_walk_exits_its_scheme_guard_once_after_its_line() {
    let logs = format!("{GUARD_EXIT}true");
    let (w, _, _) = three(184, Setup::default());
    assert_eq!(verify(&w, 3, 2), VerifyDbResult::Success);
    let done = "Verification: No coin database inconsistencies";
    let v = w.rig.v_thread();
    assert_in_order(v, &[done, logs.as_str()], "a clean walk");

    let (w, blocks, _) = three(185, Setup::default());
    rewrite_coinbase(&w, &blocks[1], 2, 1);
    assert_eq!(verify(&w, 3, 2), VerifyDbResult::CorruptedBlockDb);
    let unclean = "Verification error: coin database inconsistencies found";
    assert_in_order(w.rig.v_thread(), &[unclean, logs.as_str()], "an unclean disconnect");

    let setup = Setup {
        scratch_connect: Some(ScratchConnect::SystemError("a probe's system error")),
        ..Setup::default()
    };
    let (w, _, _) = three(186, setup);
    assert_eq!(verify(&w, 4, 1), VerifyDbResult::CorruptedBlockDb);
    let unconnectable = "Verification error: found unconnectable block";
    assert_in_order(w.rig.v_thread(), &[unconnectable, logs.as_str()], "an unconnectable block");
}

/// The L3 fixture: a coin `b2` made, with another value, makes its disconnect UNCLEAN. Below
/// the damage the walk is clean; reaching it, level 3 is false with Core's line and level 4
/// never runs (`v:5142-5145` at v24.0.0, r12).
#[test]
fn an_unclean_disconnect_fails_level_3_and_returns_before_level_4() {
    let (w, blocks, _) = three(61, Setup::default());
    rewrite_coinbase(&w, &blocks[1], 2, 1);
    let start = connects(&w);
    assert_eq!(verify(&w, 3, 1), VerifyDbResult::Success);
    assert_eq!(verify(&w, 4, 1), VerifyDbResult::Success);
    assert_eq!(connects(&w), start + 1, "level 4 reconnects b3 from b2");
    for (level, depth) in [(3, 2), (3, 0), (4, 2), (4, 0)] {
        let before = connects(&w);
        assert_eq!(verify(&w, level, depth), VerifyDbResult::CorruptedBlockDb, "{level} {depth}");
        assert_eq!(connects(&w), before, "{level} {depth}: no level 4 after an UNCLEAN");
    }
    let lines = lines_on(w.rig.v_thread());
    let want = "Verification error: coin database inconsistencies found (last 2 blocks, 0 good \
                transactions before that)";
    assert!(lines.iter().any(|l| l == want), "{lines:#?}");
    let want = "Verification error: coin database inconsistencies found (last 2 blocks, 2 good \
                transactions before that)";
    assert!(lines.iter().any(|l| l == want), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.contains("unconnectable")));
    assert!(w.sink.messages().is_empty());
}

/// The L4-only fixture: `b3`'s undo record, its checksum valid, restores `t2`'s output with
/// another script. Level 3 is clean while `b2` is out of range; level 4 reconnects `b3` over it
/// and fails its script check (`v:5172-5174` at v24.0.0). With `b2` in range, level 3 finds the
/// output mismatch first, as Core's does.
#[test]
fn a_restored_input_with_another_script_fails_only_level_4() {
    let (w, blocks, _) = three(62, Setup::default());
    let (path, at) = undo_record(&w, &blocks[2]);
    let len = record_len(&path, at);
    let undo = BlockUndo {
        txs: vec![TxUndo {
            coins: vec![Coin::new(48 * COIN, &[0x00], 2, false)],
        }],
    };
    let bytes = undo.core_bytes();
    assert_eq!(bytes.len() as u64, len, "the same record size");
    let checksum = undo_checksum(blocks[1].hash().as_bytes(), bytes.as_slice());
    write_at(&path, at, &[bytes.as_slice(), &checksum].concat());

    assert_eq!(verify(&w, 2, 1), VerifyDbResult::Success);
    assert_eq!(verify(&w, 3, 1), VerifyDbResult::Success);
    let status = w.rig.status(&blocks[2]);
    assert_eq!(verify(&w, 4, 1), VerifyDbResult::CorruptedBlockDb);
    assert_eq!(verify(&w, 3, 2), VerifyDbResult::CorruptedBlockDb);
    // The tip's entry is not failed: VerifyDB marks nothing.
    assert_eq!(w.rig.status(&blocks[2]), status);
    let lines = lines_on(w.rig.v_thread());
    let unconnectable = format!(
        "Verification error: found unconnectable block at 3, hash={} (mandatory-script-verify-\
         flag-failed (Script evaluated without error but finished with a false/empty top stack \
         element))",
        blocks[2].hash()
    );
    assert!(lines.contains(&unconnectable), "{lines:#?}");
    assert!(w.sink.messages().is_empty());
}

/// Far over the coins share every level-3 check is skipped: `SKIPPED_L3_CHECKS` with Core's
/// line, and no level 4.
#[test]
fn over_the_coins_share_level_3_is_skipped_and_level_4_never_runs() {
    let (w, blocks, _) = three(63, Setup::default());
    rewrite_coinbase(&w, &blocks[2], 3, 1);
    w.coins.lock().expect("coins").resize(1, false);
    let before = connects(&w);
    for level in [3, 4] {
        let skipped = VerifyDbResult::SkippedL3Checks(L3Skip::Dbcache);
        assert_eq!(verify(&w, level, 0), skipped, "{level}");
    }
    assert_eq!(verify(&w, 2, 0), VerifyDbResult::Success);
    assert_eq!(connects(&w), before);
    let lines = lines_on(w.rig.v_thread());
    let skip = "Skipped verification of level >=3 (insufficient database cache size). Consider \
                increasing -dbcache.";
    assert_eq!(lines.iter().filter(|l| *l == skip).count(), 2);
}

/// DEC-981a's `<=` through V: with the coins share at the figure the first step measures, the
/// tip block is disconnected (and its damage found); one byte less skips it.
#[test]
fn the_gate_admits_the_first_block_at_equality_and_skips_one_byte_over() {
    let (w, blocks, _) = three(64, Setup::default());
    rewrite_coinbase(&w, &blocks[2], 3, 1);
    let figure = {
        let tip = w.coins.lock().expect("coins");
        let scratch = CoinsCache::scratch(&tip, Some(w.coins.clone()));
        tip.verify_usage(&scratch)
    };
    w.coins.lock().expect("coins").resize(figure, false);
    assert_eq!(verify(&w, 3, 1), VerifyDbResult::CorruptedBlockDb);
    w.coins.lock().expect("coins").resize(figure - 1, false);
    assert_eq!(verify(&w, 3, 1), VerifyDbResult::SkippedL3Checks(L3Skip::Dbcache));
}

/// DEC-993: a block the incomplete hooks cannot undo (G1, DIP0003 active for the disconnect
/// only) ends level 3 as `SKIPPED_L3_CHECKS` with the readiness refusal as its reason, not an
/// abort: no `AbortNode`, no failure mark, V keeps serving.
#[test]
fn a_readiness_refusal_at_level_3_is_a_skip_not_an_abort() {
    let (w, blocks, _) = three(
        65,
        Setup {
            disconnect_extra: Some(Deployment::Buried(BuriedDeployment::Dip0003)),
            ..Setup::default()
        },
    );
    let status = w.rig.status(&blocks[2]);
    let want = refusal(&w.rig.p, &blocks[2], &verify_evo_capability("DIP0003"));
    assert_eq!(verify(&w, 3, 1), VerifyDbResult::SkippedL3Checks(L3Skip::NotReady(want.clone())));
    assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
    assert_eq!(w.rig.status(&blocks[2]), status);
    assert_eq!(verify(&w, 2, 1), VerifyDbResult::Success, "V keeps serving");
    let lines = lines_on(w.rig.v_thread());
    let reason = format!("Skipped verification of level >=3: {want}");
    assert_eq!(lines.iter().filter(|l| **l == reason).count(), 1, "{lines:#?}");
}

/// A readiness refusal level 4's reconnect meets ends as the same skip, unreported (DEC-993).
/// Defensive: the walk's G1 gate keeps evo blocks out of level 4, so here only the scratch
/// connect sees DIP0003.
#[test]
fn a_readiness_refusal_at_level_4_is_the_same_skip() {
    let dip3 = Deployment::Buried(BuriedDeployment::Dip0003);
    let (w, blocks, _) = three(
        67,
        Setup {
            scratch_connect: Some(ScratchConnect::Extra(dip3)),
            ..Setup::default()
        },
    );
    let status = w.rig.status(&blocks[2]);
    let want = refusal(&w.rig.p, &blocks[2], &g1_capability("DIP0003"));
    assert_eq!(verify(&w, 4, 1), VerifyDbResult::SkippedL3Checks(L3Skip::NotReady(want.clone())));
    assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
    assert_eq!(w.rig.status(&blocks[2]), status);
    assert_eq!(verify(&w, 3, 1), VerifyDbResult::Success, "level 3 reconnects nothing");
    let lines = lines_on(w.rig.v_thread());
    let reason = format!("Skipped verification of level >=3: {want}");
    assert_eq!(lines.iter().filter(|l| **l == reason).count(), 1, "{lines:#?}");
}

/// A system error at level 4's reconnect that is not a readiness refusal: Core's "found
/// unconnectable block", CORRUPTED_BLOCK_DB, the block's status unchanged.
// Core: validation.cpp:5172-5175 at v24.0.0
#[test]
fn a_system_error_at_level_4_is_an_unconnectable_block() {
    let (w, blocks, _) = three(
        68,
        Setup {
            scratch_connect: Some(ScratchConnect::SystemError("a probe's system error")),
            ..Setup::default()
        },
    );
    let status = w.rig.status(&blocks[2]);
    assert_eq!(verify(&w, 4, 1), VerifyDbResult::CorruptedBlockDb);
    assert_eq!(w.rig.status(&blocks[2]), status);
    assert_eq!(verify(&w, 3, 1), VerifyDbResult::Success);
    let lines = lines_on(w.rig.v_thread());
    let unconnectable = format!(
        "Verification error: found unconnectable block at 3, hash={} (a probe's system error)",
        blocks[2].hash()
    );
    assert!(lines.contains(&unconnectable), "{lines:#?}");
}

/// DEC-993 under complete hooks, which would undo the block: VerifyDB's walk skips level 3 at
/// the first block past DIP0003, before H9 runs, as positive evo verification is CH5-1b's (R10),
/// and every deeper block skips it too, as the scratch view stays at the skipped block; level 4
/// does not run. Levels 0 to 2 still check every block in the depth: block 5's damaged undo
/// record is found under the evo tip. Below level 3 the chain verifies.
#[test]
fn a_complete_build_skips_level_3_from_an_evo_block_down() {
    let p = dip3_at_20();
    let w = world(Setup {
        params: Some(p.clone()),
        steps: StepsCfg {
            complete: true,
            ..StepsCfg::default()
        },
        ..Setup::default()
    });
    let blocks = empty_chain(&p, &w.rig.genesis, 21);
    for m in &blocks {
        w.rig.accept(m);
    }
    let last = blocks.last().expect("a block");
    let status = w.rig.status(last);
    let want = refusal(&p, last, &verify_evo_capability("DIP0003"));
    let skipped = VerifyDbResult::SkippedL3Checks(L3Skip::NotReady(want.clone()));
    let before = connects(&w);
    for (level, depth) in [(3, 1), (4, 1), (3, 0), (4, 0)] {
        assert_eq!(verify(&w, level, depth), skipped, "{level} {depth}");
    }
    assert_eq!(connects(&w), before, "no level 4");
    assert_eq!(verify(&w, 2, 0), VerifyDbResult::Success);
    assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
    assert_eq!(w.rig.status(last), status);
    let lines = lines_on(w.rig.v_thread());
    let reason = format!("Skipped verification of level >=3: {want}");
    assert_eq!(lines.iter().filter(|l| **l == reason).count(), 4, "{lines:#?}");
    let memory = "Skipped verification of level >=3 (insufficient";
    assert!(!lines.iter().any(|l| l.starts_with(memory)), "{lines:#?}");
    let all = "Verification: No coin database inconsistencies in last 21 blocks (0 transactions)";
    assert!(lines.iter().any(|l| l == all), "{lines:#?}");

    let (path, at) = undo_record(&w, &blocks[4]);
    write_at(&path, at, &[0xff]);
    assert_eq!(verify(&w, 3, 0), VerifyDbResult::CorruptedBlockDb);
    let bad = format!("Verification error: found bad undo data at 5, hash={}", blocks[4].hash());
    assert!(lines_on(w.rig.v_thread()).contains(&bad));
}

/// OD-CP-30 (CH7 D103), `verifychain`: level 3's scratch disconnect of b1 applies its log, whose
/// undo writes a snapshot row to the node's rows, and the write is the first store fault (the
/// guard latches it, the store only logs). Core's VerifyDB logs its "irrecoverable inconsistency"
/// and returns `CORRUPTED_BLOCK_DB` with no report (`v:5126-5128` at v24.0.0), but the latch
/// keeps the fault and the node's flush side then takes it as reported; so V stops on it in the
/// walk after Core's line, and that stop is its one report, the store's text.
#[test]
fn a_verifychain_undo_write_that_fails_first_has_the_stores_report() {
    verifychain_undo_write_fault(58, false);
}

/// BL-775 (CH7 D102), `verifychain`: another route's snapshot write fails first, reported by
/// their store, and the undo write of level 3's scratch disconnect of b1 is then refused. Core's
/// write throws into the undo's catch (`specialtxman.cpp:1086-1090` at v24.0.0), so VerifyDB logs
/// its "irrecoverable inconsistency" (`v:5126-5128` at v24.0.0). V stops on the refused write's
/// reported failure in the walk and adds nothing: the store's report is the one.
#[test]
fn a_verifychain_undo_write_that_was_refused_has_the_stores_report() {
    verifychain_undo_write_fault(161, true);
}

/// BL-775 (Sol r1, CH3b D231), `verifychain`: W panics after level 3's scratch disconnect of b1
/// logs its undo's snapshot write, and the latch then refuses the write with W's assert, reported.
/// Core's assert ended the process before the undo wrote: VerifyDB logs no "irrecoverable
/// inconsistency" and the undo no catch lines, and V stops on the assert, W's report the one.
#[test]
fn a_verifychain_undo_write_refused_after_an_assert_logs_nothing_after_it() {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1, _) = spent(166, setup);
    let v = w.rig.v_thread();
    arm_undo_refused_after_an_assert(&w);
    let args = VerifyDbArgs {
        level: 3,
        depth: 1,
    };
    let replied = w.rig.v.verify_db(args).recv();
    let exit = w.rig.stop();
    assert_eq!(replied, Err(WaitError::Disconnected), "V stops in the walk");
    assert_nothing_after_the_assert(&w, &exit, v, &b1, "verifychain");
    let exits = |log: bool| levels_on(v, &format!("{GUARD_EXIT}{log}")).len();
    assert_eq!((exits(true), exits(false)), (0, 1), "the walk's exit logs nothing (CH3b D231)");
}

/// `verifychain` at level 3 over b1, whose scratch disconnect's undo snapshot write fails in the
/// node's rows: first, or refused after another route's write failed first if `refused`.
fn verifychain_undo_write_fault(n: u8, refused: bool) {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1, _) = spent(n, setup);
    let v = w.rig.v_thread();
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    w.seen.lock().expect("seen").undo_after_a_report = refused;
    let args = VerifyDbArgs {
        level: 3,
        depth: 1,
    };
    let replied = w.rig.v.verify_db(args).recv();
    let exit = w.rig.stop();
    let text = reported_once(&latch, &w.sink);
    assert_eq!(fatal_text(&exit), Some(text.as_str()), "refused {refused}: the store's text");
    assert_eq!(replied, Err(WaitError::Disconnected), "refused {refused}: V stops in the walk");
    let line = format!(
        "Verification error: irrecoverable inconsistency in block data at {}, hash={}",
        b1.height,
        b1.hash()
    );
    assert_eq!(levels_on(v, &line), [tracing::Level::ERROR], "refused {refused}: Core's line");
    // CH5 D1: the walk's revert comes after the catch's lines and the walk's last.
    let exit_line = format!("{GUARD_EXIT}true");
    let order = [UNDO_FAILURE, UNDO_SPECIAL_FAILED, line.as_str(), exit_line.as_str()];
    assert_in_order(v, &order, &format!("refused {refused}"));
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
}

/// BL-629 (DEC-1050), `verifychain`: level 3's scratch disconnect fails in H9 after its log holds
/// the undo's snapshot write, and the write is the first store fault. Core's write comes first, so
/// the disconnect fails at the write and keeps the fault: V stops on it in the walk, and that stop
/// is its one report, the store's text.
#[test]
fn a_verifychain_undo_failure_after_its_log_write_failed_has_the_stores_report() {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1, _) = spent(151, setup);
    let v = w.rig.v_thread();
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    w.seen.lock().expect("seen").fail_next_undo = true;
    let args = VerifyDbArgs {
        level: 3,
        depth: 1,
    };
    let replied = w.rig.v.verify_db(args).recv();
    let exit = w.rig.stop();
    let text = reported_once(&latch, &w.sink);
    assert_eq!(fatal_text(&exit), Some(text.as_str()), "the store's text");
    assert_eq!(replied, Err(WaitError::Disconnected), "V stops in the walk");
    assert_eq!(levels_on(v, UNDO_CAUGHT_SCHEME), [tracing::Level::INFO], "the catch's scheme line");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
}

/// BL-764, `verifychain`: VerifyDB's guard enters no scheme (`validation.cpp:5056` at v24.0.0), so
/// the catch's scheme line, just before its FAILURE, is the one its walk entered the undo with, the
/// next block's. With V19 at height 2 the tip b1's undo is entered with the committed basic, not
/// b1's legacy. (Core's undo logs its own switch to legacy before that, `stm:1068-1072` at
/// v24.0.0, as `NodeEvo`'s H9 does; this rig's hooks log no scheme line.)
/// The connector hands the evo hooks that scheme as the one the catch restores (CH5 D1), so the
/// walk's guard sees no change to revert.
#[test]
fn a_verifychain_undo_failure_logs_the_next_blocks_scheme() {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        params: Some(v19_at_2()),
        ..Setup::default()
    };
    let (mut w, b1, _) = spent(155, setup);
    let v = w.rig.v_thread();
    arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    w.seen.lock().expect("seen").fail_next_undo = true;
    let args = VerifyDbArgs {
        level: 3,
        depth: 1,
    };
    let replied = w.rig.v.verify_db(args).recv();
    let exit = w.rig.stop();
    assert_eq!(replied, Err(WaitError::Disconnected), "V stops in the walk");
    assert_eq!(levels_on(v, UNDO_CAUGHT_SCHEME), [tracing::Level::INFO], "the committed basic");
    let lines = lines_on(v);
    let failure = lines.iter().position(|l| l.starts_with(UNDO_FAILURE)).expect("the FAILURE line");
    assert_eq!(lines[failure - 1], UNDO_CAUGHT_SCHEME, "the committed basic, then FAILURE");
    let restored = format!("{UNDO_CAUGHT_HOOK}0");
    assert_eq!(lines[failure + 1], restored, "the hooks restore the committed basic");
    assert_eq!(levels_on(v, &restored).len(), 1, "once");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
}

/// V's handle on the rig's node flag: it tells the test when V's stop sets the flag, which V's
/// stop does last on V's thread, after any report V makes (`StopTail`). OD-CP-31's barrier.
struct StopBarrier {
    flag: Arc<LocalShutdown>,
    stopped: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl ShutdownFlag for StopBarrier {
    fn start_shutdown(&self) {
        self.flag.start_shutdown();
        if let Some(stopped) = self.stopped.lock().expect("barrier").take() {
            let _ = stopped.send(());
        }
    }

    fn shutdown_requested(&self) -> bool {
        self.flag.shutdown_requested()
    }
}

/// The rig's evo hooks, with `steps`, over `seen`.
fn test_evo(steps: StepsCfg, seen: &Arc<Mutex<Seen>>) -> TestEvo<false> {
    TestEvo {
        cfg: steps,
        seen: seen.clone(),
        staged: None,
        attempts: 0,
        scratch: false,
        h8_calls: 0,
        guard_committed: false,
    }
}

/// What the harness sees in a world on the stores `s`, whose latch is the node's.
fn node_seen(s: &NodeStores) -> Arc<Mutex<Seen>> {
    Arc::new(Mutex::new(Seen {
        latch: s.latch.clone(),
        ..Seen::default()
    }))
}

/// Startup's VerifyDB through the production flush side: b1 connected and made durable by an
/// ALWAYS flush on W's thread (node_stores' stores, `NodeFlush` with V's bell on the node's
/// latch), then a restart whose connector's evo side is `steps` and whose undo snapshot write
/// fails first if `store_fault`. Init is the test: it asks for `VerifyLoadedChainstate` at level
/// 3 over one block, then, if `barrier`, waits for V's own stop before its report gate, which
/// shows the load's error only if no shutdown was requested (`init.cpp:2109-2115` at v24.0.0,
/// rdashd's `node.rs` `Recoverable` arm). Nothing else is sent to V before that gate, so V stops
/// there only on what it read itself: its flush bell's read of the latch, or its own stop on the
/// fault. The rig's init ABC, sent after V has stopped, is answered `Ok` (D241). The reports: the
/// abort sink's, then init's. The verify reply, and V's exit.
fn startup_verify(
    steps: StepsCfg,
    store_fault: bool,
    barrier: bool,
) -> (Vec<(String, Option<String>)>, VerifyReply, VExit, NodeStores) {
    use crate::chain::{ChainHandle, VerifyLoadError, VerifyLoadOptions};

    let s = NodeStores::new();
    let (mut rig, b1) = flushed_b1(&s, 0);
    let (tip, genesis) = (rig.id(&b1), rig.genesis.hash());
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let second = node_seen(&s);
    if store_fault {
        arm_undo_store_fault(&second, &s.sink, genesis);
    }
    let steps = StepsCfg {
        published: BlsCtx::BASIC,
        ..steps
    };
    let mut cfg = s.config_with_hooks(Seams::default(), test_evo(steps, &second), Arc::new(()));
    let flag = Arc::new(LocalShutdown::default());
    let (stopped, v_stopped) = std::sync::mpsc::channel();
    cfg.shutdown = Some(flag.clone());
    cfg.node_shutdown = Some(Arc::new(StopBarrier {
        flag: flag.clone(),
        stopped: Mutex::new(Some(stopped)),
    }));
    cfg.stops_at_init = barrier;
    let (sent, verified) = std::sync::mpsc::channel();
    let init_flag = flag.clone();
    cfg.before_init = Some(Box::new(move |v: &ChainHandle| {
        let options = VerifyLoadOptions {
            check_blocks: 1,
            check_level: 3,
            ..VerifyLoadOptions::default()
        };
        let r = v.verify_loaded(options).recv();
        if barrier {
            // The barrier: V's stop, whatever read the latch first, comes before init's gate.
            let stop = v_stopped.recv_timeout(std::time::Duration::from_secs(300));
            stop.expect("V stops on its own after a latched store fault");
        }
        let corrupted = Ok(Err(VerifyLoadError::Failure(CORRUPTED_BLOCK_DB)));
        let shown = r == corrupted && !init_flag.shutdown_requested();
        sent.send((r, shown)).expect("the test waits");
    }));
    let files = cfg.files.take().expect("the node's block files");
    let mut rig = Rig::restart_with(exit, files, tip, cfg).expect("V starts");
    let (r, shown) = verified.recv().expect("sent before init's ABC");
    let exit = rig.stop();
    let mut reports = s.sink.reports();
    if shown {
        reports.push((CORRUPTED_BLOCK_DB.to_owned(), None));
    }
    (reports, r, exit, s)
}

type VerifyReply = Result<Result<(), crate::chain::VerifyLoadError>, WaitError>;

/// OD-CP-31 (DEC-1050, CH7 D103), startup: `VerifyLoadedChainstate`'s VerifyDB meets the first
/// store fault in level 3's scratch disconnect over the database (the undo's snapshot write;
/// the guard latches it, the store only logs). V stops on it there, as on the `verifychain`
/// path: its one report, the store's text, made before init has any result, so init shows no
/// CORRUPTED_BLOCK_DB. Through the production flush side, V's flush bell would read the latch
/// after an init error and stop V with no report, and init skips its error once V has asked for
/// the shutdown: the barrier holds init's gate until V has stopped on its own, so that order is
/// the one run here.
// Core: v24.0.0 init.cpp:2109-2115; node/blockstorage.cpp:950-962; validation.cpp:3789-3791,
// :3823
#[test]
fn a_startup_undo_write_that_fails_first_stops_v_with_the_stores_report() {
    let (reports, verified, exit, s) = startup_verify(StepsCfg::default(), true, true);
    let failure = s.latch.tripped().expect("the store latched the fault").clone();
    assert_eq!(failure.site, "chainstate raw write");
    let text = format!("System error while flushing: {}", failure.message);
    let stopped = s.latch.tripped().is_some() && exit.fatal.is_some();
    assert_eq!(
        reports,
        [(text.clone(), None)],
        "the store's report, and only it (init's reply {verified:?}, V stopped {stopped})"
    );
    assert_eq!(verified, Err(WaitError::Disconnected), "init has no CORRUPTED_BLOCK_DB");
    assert_eq!(fatal_text(&exit), Some(text.as_str()), "the store's text");
}

/// OD-CP-31 (DEC-1050), startup's regression pin: a failed scratch disconnect with no store
/// fault (H9's `false`, nothing in the undo's log) is still `VerifyLoadedChainstate`'s
/// CORRUPTED_BLOCK_DB, init's one report: V reports nothing and goes on (init's ABC succeeds).
#[test]
fn a_startup_failed_disconnect_without_a_store_fault_is_init_error_alone() {
    let steps = StepsCfg {
        undo_special: false,
        ..StepsCfg::default()
    };
    let (reports, verified, exit, s) = startup_verify(steps, false, false);
    assert!(s.latch.tripped().is_none(), "no store fault");
    assert_eq!(reports, [(CORRUPTED_BLOCK_DB.to_owned(), None)], "init's report, and only it");
    let corrupted = crate::chain::VerifyLoadError::Failure(CORRUPTED_BLOCK_DB);
    assert_eq!(verified, Ok(Err(corrupted)));
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
}

/// BL-787's worlds: b1 connected on the stores `s` and made durable by an ALWAYS flush, its
/// coins in the database alone (`salt` makes the block). The rig and b1.
fn flushed_b1(s: &NodeStores, salt: u32) -> (Rig, Mined) {
    use crate::chain::FlushStateMode;

    install_accept_log();
    let seen = node_seen(s);
    let cfg =
        s.config_with_hooks(Seams::default(), test_evo(StepsCfg::default(), &seen), Arc::new(()));
    let rig = Rig::with(cfg);
    let b1 = branch(&rig.p, &rig.genesis, 1, salt).remove(0);
    rig.accept(&b1);
    rig.on_v(|v| v.flush_now(FlushStateMode::Always));
    (rig, b1)
}

/// Init's view of BL-787's startup read: `VerifyLoadedChainstate`'s reply, whether V had
/// stopped on its own within 5 s of it, and whether init's report gate then saw no shutdown
/// request (`init.cpp:2109-2121` at v24.0.0, rdashd's `node.rs` `Recoverable` arm).
struct InitSaw {
    verified: VerifyReply,
    v_stopped: bool,
    gate_open: bool,
}

/// The read that fails in [`startup_read`]'s walk.
#[derive(Clone, Copy, PartialEq)]
enum StartupRead {
    /// Every coins read (the probe's I/O error).
    Coins,
    /// H9's evo read ([`Seen::evo_read_in_next_undo`]), inside `UndoSpecialTxsInBlock`'s `try`.
    Evo,
    /// Every coins read, and the disconnect attempt's log then fails to apply
    /// ([`Seen::fail_next_apply`]) on the latch the read tripped.
    CoinsThenApply,
    /// [`Self::Evo`], and the attempt's log then fails to apply on the latch the read tripped.
    EvoThenApply,
    /// H4's evo read ([`Seen::evo_read_in_next_h4`]) in level 4's reconnect, inside
    /// `ProcessSpecialTxsInBlock`'s `try`; the walk runs at level 4.
    EvoInConnect,
    /// [`Self::EvoInConnect`], and the attempt's log then fails to apply.
    EvoInConnectThenApply,
    /// A coins read through H4's view in level 4's reconnect, inside the same `try`.
    CoinsInConnect,
    /// H4's evo read inside a scope nested in H4's, as `NodeEvo`'s `CMNHFManager::ProcessBlock`
    /// call, and the attempt's log then fails to apply.
    NestedInConnectThenApply,
}

impl StartupRead {
    /// Whether the read is H4's, in level 4's reconnect.
    fn in_connect(self) -> bool {
        matches!(
            self,
            Self::EvoInConnect
                | Self::EvoInConnectThenApply
                | Self::CoinsInConnect
                | Self::NestedInConnectThenApply
        )
    }
}

/// BL-787: startup's `VerifyLoadedChainstate` at level 3 over b1 (4 for H4's read), after
/// [`flushed_b1`] and a restart, with `read` failing in its walk, armed just before init asks.
/// V's flush bell rings on the read's latch trip; nothing else is sent to V before init's gate,
/// as in [`startup_verify`].
/// What init saw, V's thread, its exit, the stores and what the harness saw.
fn startup_read(
    salt: u32,
    read: StartupRead,
) -> (InitSaw, std::thread::ThreadId, VExit, NodeStores, Arc<Mutex<Seen>>) {
    use crate::chain::{ChainHandle, VerifyLoadOptions};

    let s = NodeStores::new();
    let (mut rig, b1) = flushed_b1(&s, salt);
    let tip = rig.id(&b1);
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let seen = node_seen(&s);
    let steps = StepsCfg {
        published: BlsCtx::BASIC,
        ..StepsCfg::default()
    };
    let mut cfg = s.config_with_hooks(Seams::default(), test_evo(steps, &seen), Arc::new(()));
    let harness = seen.clone();
    let flag = Arc::new(LocalShutdown::default());
    let (stopped, v_stopped) = std::sync::mpsc::channel();
    cfg.shutdown = Some(flag.clone());
    cfg.node_shutdown = Some(Arc::new(StopBarrier {
        flag: flag.clone(),
        stopped: Mutex::new(Some(stopped)),
    }));
    // The read's latch refuses init's ABC, which answers `Ok` as one that took no step (CH3b
    // D240), and V stops there.
    cfg.stops_at_init = true;
    let (sent, saw) = std::sync::mpsc::channel();
    let db = s.db();
    cfg.before_init = Some(Box::new(move |v: &ChainHandle| {
        let mut armed = seen.lock().expect("seen");
        match read {
            StartupRead::Evo | StartupRead::EvoThenApply => armed.evo_read_in_next_undo = Some(db),
            // The apply's failure is armed by H4, as level 3's disconnect attempt applies first.
            StartupRead::EvoInConnect
            | StartupRead::EvoInConnectThenApply
            | StartupRead::CoinsInConnect
            | StartupRead::NestedInConnectThenApply => {
                let nested = read == StartupRead::NestedInConnectThenApply;
                armed.read_in_next_h4 = Some(H4Read {
                    db,
                    coins: read == StartupRead::CoinsInConnect,
                    nested,
                    refuse_apply: nested || read == StartupRead::EvoInConnectThenApply,
                });
            }
            StartupRead::Coins | StartupRead::CoinsThenApply => {
                db.inner().fail_reads(Some(dashd_store::ErrorKind::Io));
            }
        }
        armed.fail_next_apply =
            matches!(read, StartupRead::CoinsThenApply | StartupRead::EvoThenApply);
        drop(armed);
        let options = VerifyLoadOptions {
            check_blocks: 1,
            check_level: if read.in_connect() {
                4
            } else {
                3
            },
            ..VerifyLoadOptions::default()
        };
        let verified = v.verify_loaded(options).recv();
        let v_stopped = v_stopped.recv_timeout(std::time::Duration::from_secs(5)).is_ok();
        let gate_open = !flag.shutdown_requested();
        let saw = InitSaw {
            verified,
            v_stopped,
            gate_open,
        };
        sent.send(saw).expect("the test waits");
    }));
    let files = cfg.files.take().expect("the node's block files");
    let mut rig = Rig::restart_with(exit, files, tip, cfg).expect("V starts");
    let v = rig.v_thread();
    let saw = saw.recv().expect("sent before init's ABC");
    let exit = rig.stop();
    (saw, v, exit, s, harness)
}

/// [`startup_read`]'s init error: Core's throw, caught by init, with the engine's text; the read
/// latched at the store's read site and nothing reported it as an abort (Core's walk is past the
/// coins error catcher). V held its flush bell, so init's gate saw no shutdown request.
fn assert_inits_read_error(saw: &InitSaw, s: &NodeStores, case: &str) {
    use crate::chain::VerifyLoadError;

    let Ok(Err(VerifyLoadError::Read(what))) = &saw.verified else {
        panic!("{case}: init's read error, not {:?}", saw.verified);
    };
    assert!(what.ends_with("injected read failure"), "{case}: the engine's text, not {what:?}");
    let read = s.latch.tripped().expect("the read latched the store").clone();
    assert_eq!((read.site, read.message.as_str()), (READ_SITE, what.as_str()), "{case}");
    let reports = s.sink.reports();
    assert!(reports.is_empty(), "{case}: no abort, no V report: {reports:?}");
    assert!(!saw.v_stopped, "{case}: V holds its flush bell after the reply");
    assert!(saw.gate_open, "{case}: init's gate sees no shutdown request");
}

/// BL-787 (OD-CP-177): a coins read that fails in startup's VerifyDB is init's error opening the
/// block database, as Core's throw from `CDBWrapper::Read` reaches init's `catch_exceptions`
/// (`dbwrapper.cpp:261-268`, `init.cpp:2022-2028`, `:2096` at v24.0.0). The throw unwinds the
/// walk's `ScopedBLSLegacyScheme`, whose destructor logs its revert (`v:162-167`, `:5056` at
/// v24.0.0), on V's thread, before the reply.
#[test]
fn a_startup_verify_coins_read_that_fails_is_inits_error_opening_the_block_database() {
    let (saw, v, _, s, _) = startup_read(787, StartupRead::Coins);
    assert_inits_read_error(&saw, &s, "a coins read");
    let exits = |log: bool| levels_on(v, &format!("{GUARD_EXIT}{log}")).len();
    assert_eq!((exits(true), exits(false)), (1, 0), "the scheme guard's exit logs its revert");
}

/// [`startup_read`]'s caught read (OD-CP-178, CH5 D23): an evo hook's `try` caught it, so the
/// walk took the hook's failure path to `CORRUPTED_BLOCK_DB`, init's corrupted-database question.
/// The read latched at the store's read site and nothing reported it as an abort. V held its
/// flush bell, so init's gate saw no shutdown request. The engine's text, Core's `e.what()`.
fn assert_caught(saw: &InitSaw, s: &NodeStores, case: &str) -> String {
    use crate::chain::VerifyLoadError;

    let corrupted = Ok(Err(VerifyLoadError::Failure(CORRUPTED_BLOCK_DB)));
    assert_eq!(saw.verified, corrupted, "{case}: the walk's CORRUPTED_BLOCK_DB");
    let read = s.latch.tripped().expect("the read latched the store").clone();
    assert_eq!(read.site, READ_SITE, "{case}");
    assert!(read.message.ends_with("injected read failure"), "{case}: {:?}", read.message);
    let reports = s.sink.reports();
    assert!(reports.is_empty(), "{case}: no abort, no V report: {reports:?}");
    assert!(!saw.v_stopped, "{case}: V holds its flush bell after the reply");
    assert!(saw.gate_open, "{case}: init's gate sees no shutdown request");
    read.message
}

/// BL-787 (OD-CP-178): an evo read that fails in H9 there is caught by `UndoSpecialTxsInBlock`'s
/// `try` (`evo/specialtxman.cpp:1086-1090` at v24.0.0): its catch restores the scheme the undo was
/// entered with and logs it, then FAILURE; `DisconnectBlock` logs `validation.cpp:2322` at v24.0.0
/// and VerifyDB its irrecoverable line (`validation.cpp:5126-5129` at v24.0.0) and gives
/// `CORRUPTED_BLOCK_DB`, before the walk guard's exit (CH5 D1). With the attempt's log then refused
/// on the read's latch (`refused`), the same lines once, the refusal at DEBUG.
#[test]
fn a_startup_verify_evo_read_that_fails_in_the_undo_is_caught_as_a_corrupted_database() {
    for (salt, read) in [(7871, StartupRead::Evo), (7874, StartupRead::EvoThenApply)] {
        let refused = read == StartupRead::EvoThenApply;
        let case = format!("refused {refused}");
        let (saw, v, exit, s, seen) = startup_read(salt, read);
        let what = assert_caught(&saw, &s, &case);
        let b1 = exit.chain.private_tip().expect("b1 is the tip").0.hash;
        let failure = format!("{UNDO_FAILURE} {what}");
        let line = format!(
            "Verification error: irrecoverable inconsistency in block data at 1, hash={b1}"
        );
        let exit_line = format!("{GUARD_EXIT}true");
        let restored = format!("{UNDO_CAUGHT_HOOK}0");
        let order =
            [UNDO_CAUGHT_SCHEME, &failure, &restored, UNDO_SPECIAL_FAILED, &line, &exit_line];
        assert_in_order(v, &order, &case);
        assert_eq!(levels_on(v, &restored).len(), 1, "{case}: the hooks restore it once");
        let (info, error) = (tracing::Level::INFO, tracing::Level::ERROR);
        assert_eq!(levels_on(v, UNDO_CAUGHT_SCHEME), [info], "{case}");
        assert_eq!(levels_on(v, &failure), [error], "{case}");
        assert_eq!(levels_on(v, &line), [error], "{case}");
        let refusal = levels_on(v, &format!("finish_attempt: {APPLY_FAILED}"));
        assert_eq!(
            refusal,
            if refused {
                vec![tracing::Level::DEBUG]
            } else {
                vec![]
            },
            "{case}"
        );
        assert!(!seen.lock().expect("seen").fail_next_apply, "{case}: the apply ran");
    }
}

/// BL-787 (OD-CP-178): an evo read that fails in H4 during level 4's reconnect is caught by
/// `ProcessSpecialTxsInBlock`'s `std::exception` arm (`evo/specialtxman.cpp:1053-1056` at
/// v24.0.0; a read's `dbwrapper_error` is no `EvoDbInconsistencyError`): FAILURE, then
/// `failed-procspectxsinblock`, `ConnectBlock`'s line (`validation.cpp:2677` at v24.0.0) and
/// VerifyDB's unconnectable block (`:5172-5174` at v24.0.0), `CORRUPTED_BLOCK_DB`. With the
/// attempt's log then refused (`refused`), the same lines once, the refusal at DEBUG. A coins read
/// through H4's view is caught the same way: the scope is the hook's `try`, whatever the store. One
/// caught by a scope nested in H4's (`nested`, as `CMNHFManager::ProcessBlock`'s,
/// `mnhftx.cpp:257-260`) is that scope's verdict, passed on with no FAILURE of H4's
/// (`stm:1033-1036` at v24.0.0), and a refused log keeps it too.
#[test]
fn a_startup_verify_read_that_fails_in_the_reconnects_h4_is_caught_as_a_corrupted_database() {
    let reads = [
        (7875, StartupRead::EvoInConnect),
        (7876, StartupRead::EvoInConnectThenApply),
        (7877, StartupRead::CoinsInConnect),
        (7878, StartupRead::NestedInConnectThenApply),
    ];
    for (salt, read) in reads {
        let nested = read == StartupRead::NestedInConnectThenApply;
        let refused = nested || read == StartupRead::EvoInConnectThenApply;
        let coins = read == StartupRead::CoinsInConnect;
        let case = format!("refused {refused}, coins {coins}, nested {nested}");
        let (saw, v, exit, s, seen) = startup_read(salt, read);
        let what = assert_caught(&saw, &s, &case);
        let b1 = exit.chain.private_tip().expect("b1 is the tip").0.hash;
        let h4 = format!("CSpecialTxProcessor::ProcessSpecialTxsInBlock -- FAILURE! {what}");
        let mnhf = format!("CMNHFManager::ProcessBlock -- failed: {what}");
        let (failure, other) = if nested {
            (&mnhf, &h4)
        } else {
            (&h4, &mnhf)
        };
        let reason = if nested {
            "failed-proc-mnhf-inblock"
        } else {
            "failed-procspectxsinblock"
        };
        let connect = format!(
            "ConnectBlock(DASH): ProcessSpecialTxsInBlock for block {b1} failed with {reason}"
        );
        let line =
            format!("Verification error: found unconnectable block at 1, hash={b1} ({reason})");
        let exit_line = format!("{GUARD_EXIT}true");
        assert_in_order(v, &[failure, &connect, &line, &exit_line], &case);
        let (info, error) = (tracing::Level::INFO, tracing::Level::ERROR);
        assert_eq!(levels_on(v, failure), [info], "{case}");
        assert!(levels_on(v, other).is_empty(), "{case}: the other catch logs nothing");
        assert_eq!(levels_on(v, &connect), [error], "{case}");
        assert_eq!(levels_on(v, &line), [error], "{case}");
        let refusal = levels_on(v, &format!("finish_attempt: {APPLY_FAILED}"));
        let want = if refused {
            vec![tracing::Level::DEBUG]
        } else {
            vec![]
        };
        assert_eq!(refusal, want, "{case}");
        assert!(!seen.lock().expect("seen").fail_next_apply, "{case}: the apply ran");
    }
}

/// BL-787: Core's throw skips every line until init's catch, the revert aside, so a log the
/// latch then refuses to apply is no ERROR line on V (`finish_attempt`'s refusal).
#[test]
fn a_startup_verify_read_that_fails_logs_no_refused_apply() {
    let (saw, v, _, s, seen) = startup_read(7873, StartupRead::CoinsThenApply);
    assert_inits_read_error(&saw, &s, "a coins read, then a refused apply");
    assert!(!seen.lock().expect("seen").fail_next_apply, "the attempt's apply failed");
    let refusal = format!("finish_attempt: {APPLY_FAILED}");
    assert_eq!(levels_on(v, &refusal), [tracing::Level::DEBUG], "a debug line");
}

/// BL-787, CH5 D1: after a read that failed in startup's walk, VerifyDB's guard logs its revert
/// whatever the attempt that read returned, as Core's exception unwinds it
/// (`validation.cpp:162-167`, `:5056` at v24.0.0), but not after one of Core's asserts, which
/// ends Core's process. Without such a read it is the tip's rule, which logs nothing after a coins
/// read fault either.
#[test]
fn the_walk_guard_logs_after_a_startup_read_but_not_after_an_assert() {
    use crate::chain::seams::scheme_guard_logs;

    let read = Fatal::read_failed("coins read");
    let (store, assert) = (Fatal::new("evo store"), Fatal::assert("assert"));
    let cases = [
        (None, true, true),
        (Some(&read), true, true),
        (Some(&store), true, true),
        (Some(&assert), true, false),
        (None, false, true),
        (Some(&read), false, false),
        (Some(&store), false, true),
        (Some(&assert), false, false),
    ];
    for (failure, threw, logs) in cases {
        assert_eq!(scheme_guard_logs(failure, threw), logs, "{failure:?}, threw {threw}");
    }
}

/// BL-787's guard: `verifychain`'s walk over the tip opens no region, so a coins read that fails
/// in it ends in its store as before, reported as Core's catcher aborts (`coins.cpp:356-369` at
/// v24.0.0), and V stops.
#[test]
fn a_verifychain_coins_read_that_fails_still_aborts_in_its_store() {
    let s = NodeStores::new();
    let (mut rig, _) = flushed_b1(&s, 7872);
    s.db().inner().fail_reads(Some(dashd_store::ErrorKind::Io));
    let args = VerifyDbArgs {
        level: 3,
        depth: 1,
    };
    let replied = rig.v.verify_db(args).recv();
    let exit = rig.stop();
    assert_eq!(replied, Err(WaitError::Disconnected), "V stops in the walk");
    let read = s.latch.tripped().expect("the read latched the store").clone();
    assert_eq!(read.site, READ_SITE);
    let abort = format!("Error reading from database: {}", read.message);
    assert_eq!(s.sink.messages(), [abort], "the store's abort, once");
    assert!(exit.fatal.is_some(), "V's exit");
}

/// The scratch view the connector builds: over the tip on the RPC path, at its best block; over
/// the database at startup, at its durable block. Nothing is flushed here, so only the RPC one
/// sees the tip's unflushed coins.
#[test]
fn the_rpc_scratch_reads_the_tip_and_startups_the_database() {
    let (w, _, txs) = three(66, Setup::default());
    let k = OutPointKey::new(&op(&txs[2], 0));
    let got = super::disconnect::on_v_cx(&w, AttemptOrigin::VerifyDb, move |_, connector, _| {
        let mut out = Vec::new();
        for parent in [ScratchParent::Tip, ScratchParent::Db] {
            let mut view = connector.verify_scratch(parent).expect("builds").expect("a view");
            let (coins, _) = view.split();
            let mut ov = coins.overlay::<ProbeEngine>().expect("the engine");
            out.push(ov.spend(&k).expect("reads").is_some());
        }
        out
    });
    assert_eq!(got, vec![true, false]);
}

/// The active coins' best block and membership at `ops`, read directly (no V query after V's
/// thread panicked).
fn active_coins(w: &World, ops: &[OutPoint]) -> (Option<BlockHash>, Vec<bool>) {
    let coins = w.coins.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let held = ops.iter().map(|o| coins.have_in_cache(&OutPointKey::new(o))).collect();
    (coins.best_block().map(|b| b.hash), held)
}

/// `verifychain` whose scratch attempt unwinds, as `steps` arm it, at `level` over the last three
/// blocks: V's reply disconnects and its thread ends with the hook's panic. The attempt has no tip
/// hold, so its guard reports the unwind itself, once, with Core's default user message, and V's
/// own report finds the latch tripped (CH3a D71, BL-601). The active coins are as they were.
fn unwound_verify(n: u8, steps: StepsCfg, level: i32) {
    let (mut w, blocks, txs) = three(
        n,
        Setup {
            steps,
            ..Setup::default()
        },
    );
    let ops: Vec<_> = txs.iter().map(|t| op(t, 0)).collect();
    let before = active_coins(&w, &ops);
    assert_eq!(before.0, Some(blocks[2].hash()));
    let reply = w.rig.v.verify_db(VerifyDbArgs {
        level,
        depth: 3,
    });
    assert_eq!(reply.recv(), Err(WaitError::Disconnected), "{level}");
    assert_eq!(w.rig.join_panicked(), HOOK_PANICKED, "{level}");
    assert_eq!(w.sink.reports(), [(UNWOUND.to_owned(), None)], "{level}: the guard's one report");
    let latch = w.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).latch.clone();
    let f = latch.tripped().expect("tripped");
    assert_eq!((f.site, f.message.as_str()), (UNWIND_SITE, UNWOUND), "{level}");
    assert_eq!(active_coins(&w, &ops), before, "{level}: the active coins unchanged");
}

/// BL-601: an unwind in VerifyDB's level-4 scratch connect reports itself once.
#[test]
fn a_hook_panicking_in_verifydbs_level_4_reports_the_unwind_once() {
    let steps = StepsCfg {
        panic_in_scratch_h4: true,
        ..StepsCfg::default()
    };
    unwound_verify(67, steps, 4);
}

/// BL-601: an unwind in VerifyDB's level-3 scratch disconnect (H9) reports itself once.
#[test]
fn a_hook_panicking_in_verifydbs_level_3_reports_the_unwind_once() {
    let steps = StepsCfg {
        panic_in_scratch_undo: true,
        ..StepsCfg::default()
    };
    unwound_verify(68, steps, 3);
}
