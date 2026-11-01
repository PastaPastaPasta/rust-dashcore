//! VerifyDB's levels 3 and 4 through V (CH5-1a; `validation.cpp:5041-5188` at v24.0.0-rc.5): the
//! scratch disconnect walk, its UNCLEAN early return, the reconnect from the walk's last block,
//! the level-3 gate (DEC-981a) and the evo skip (BL-440, DEC-993), over the production scratch
//! view the connector builds. The process DIFF against dashd is `rdashd/tests/verifychain.rs`.

use super::disconnect::{
    arm_undo_store_fault, edit, record_len, reported_once, spent, undo_record, write_at,
};
use super::*;
use crate::chain::seams::ScratchParent;
use crate::chain::tests::support::{install_accept_log, lines_on};
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
    out.value += delta;
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

/// The L3 fixture: a coin `b2` made, with another value, makes its disconnect UNCLEAN. Below
/// the damage the walk is clean; reaching it, level 3 is false with Core's line and level 4
/// never runs (`v:5140-5143`, r12).
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
/// and fails its script check (`v:5170-5173`). With `b2` in range, level 3 finds the output
/// mismatch first, as Core's does.
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
// Core: validation.cpp:5171-5174
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
/// guard latches it, the store only logs). Core's VerifyDB returns `false` with no report
/// (`v:5121`), but the latch keeps the fault and the node's flush side then takes it as reported;
/// so V stops on it in the walk, and that stop is its one report, the store's text.
#[test]
fn a_verifychain_undo_write_that_fails_first_has_the_stores_report() {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1, _) = spent(58, setup);
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    let args = VerifyDbArgs {
        level: 3,
        depth: 1,
    };
    let replied = w.rig.v.verify_db(args).recv();
    let exit = w.rig.stop();
    let text = reported_once(&latch, &w.sink);
    assert_eq!(fatal_text(&exit), Some(text.as_str()), "the store's text");
    assert_eq!(replied, Err(WaitError::Disconnected), "V stops in the walk");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
}

/// OD-CP-30 (CH7 D103), startup: `VerifyLoadedChainstate`'s VerifyDB meets the same first store
/// fault in level 3's scratch disconnect over the database. Its `false` is init's "Corrupted
/// block database detected" (`node/chainstate.cpp:441-454`), the one report there, which init
/// makes: V reports nothing through the abort sink. The database's best block is b1, as a W
/// release leaves it; its coins are not written, so the disconnect is unclean, and its log is
/// applied on every outcome (D33).
#[test]
fn a_startup_undo_write_that_fails_first_keeps_the_init_error_as_the_one_report() {
    use crate::chain::{ChainHandle, VerifyLoadError, VerifyLoadOptions};

    install_accept_log();
    let (mut w, b1, _) = spent(59, Setup::default());
    let (tip, genesis) = (w.rig.id(&b1), w.rig.genesis.hash());
    let exit = w.rig.stop();
    {
        let mut coins = w.coins.lock().expect("coins");
        let generation = coins.freeze(0).generation();
        coins.release(generation);
        assert_eq!(coins.durable().map(|b| b.hash), Some(b1.hash()), "released at b1");
    }
    let (mut cfg, _, sink, seen) = node(Setup {
        stores: Some(w.stores.clone()),
        ..Setup::default()
    });
    let latch = arm_undo_store_fault(&seen, &sink, genesis);
    let (sent, verified) = std::sync::mpsc::channel();
    cfg.before_init = Some(Box::new(move |v: &ChainHandle| {
        let options = VerifyLoadOptions {
            check_blocks: 1,
            check_level: 3,
            ..VerifyLoadOptions::default()
        };
        let r = v.verify_loaded(options).recv_timeout(std::time::Duration::from_secs(120));
        sent.send(r).expect("the test waits");
    }));
    let files = cfg.files.take().expect("the world's block and undo files");
    let mut rig = Rig::restart_with(exit, files, tip, cfg).expect("V starts");
    let verified = verified.recv().expect("sent before init's ABC");
    let exit = rig.stop();
    assert!(latch.tripped().is_some(), "the store latched the fault");
    assert_eq!(sink.reports(), [], "the init error is the one report");
    let corrupted = VerifyLoadError::Failure(CORRUPTED_BLOCK_DB);
    assert_eq!(verified, Some(Ok(Err(corrupted))), "init's error");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
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
