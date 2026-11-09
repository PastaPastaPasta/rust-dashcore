//! Core's `ScopedBLSLegacyScheme` (`validation.cpp:147-175` at v24.0.0) around `NodeEvo`'s
//! published scheme (CH7 D9): `ConnectBlock`'s own guard when the block's log fails after its
//! commit (BL-786 (a)), `ConnectTip`'s guard (`:3416`, CH3a D192), and VerifyDB's walk guard
//! (`:5056`, CH5 D1) over a walk that crosses V19: its undo's catch and its reconnects.

use std::sync::{Arc, Mutex};

use dash_consensus::ctx::bls_ctx_for_block;
use dash_consensus::{Block, BlsCtx};
use dash_evo::Effect;
use dash_num::Hash256;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_primitives::{BlockHash, QuorumHash};

use super::evo_chain::node_config;
use super::node_evo::{Evo, NoCoins, at, attempt_facade, node_evo, node_evo_is_lock};
use super::node_evo_lists::Scratch;
use super::support::{
    Mined, Rig, RigConfig, branch, install_accept_log, levels_on, lines_on, with_tip_cx,
};
use crate::chain::{
    AttemptOrigin, ConnectCx, EvoTarget, Fatal, SchemeGuardHolder, VContext, VerifyDbArgs,
    VerifyDbResult, ViewTarget,
};
use crate::connect::{
    AttemptId, BlockAt, EvoAttempt as _, EvoHooks as _, NodeHelper, SpecialTxRules,
};
use crate::evo::cl::AttemptClVerifier;
use crate::evo::effects::IsLockApplier;
use crate::evo::ev1::DmlChecks;
use dash_consensus::coins::{BlockUndo, TxUndo};

/// The guard's revert line, back to legacy.
pub(super) const REVERTED_LEGACY: &str = "ScopedBLSLegacyScheme: reverted bls_legacy_scheme=1";
/// The guard's revert line, back to basic.
pub(super) const REVERTED_BASIC: &str = "ScopedBLSLegacyScheme: reverted bls_legacy_scheme=0";
/// The guard's enter line into legacy.
pub(super) const ENTERED_LEGACY: &str = "ScopedBLSLegacyScheme: entered bls_legacy_scheme=1";
/// `ProcessSpecialTxsInBlock`'s switch to basic (`evo/specialtxman.cpp:1043-1047` at v24.0.0).
const SWITCHED_BASIC: &str = "CSpecialTxProcessor::ProcessSpecialTxsInBlock -- bls_legacy_scheme=0";
/// VerifyDB's result line (`validation.cpp:5180` at v24.0.0).
const VERIFIED: &str = "Verification: No coin database inconsistencies";

/// Regtest with V19 at `height`.
fn v19_at(height: u32) -> Arc<ChainSetup> {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::TestActivationHeight, format!("v19@{height}"))
        .build()
        .expect("v19 activation height");
    Arc::new(p)
}

/// An effect of `kind`'s phase whose applier fails in [`node_evo`]'s hooks: the IS-lock and
/// mineable stand-ins fail closed, and a notify with no queued lists fails (D15).
fn failing(kind: &str) -> Effect {
    match kind {
        "is lock" => Effect::RemoveConflictingIsLock {
            lock_hash: Hash256::from([7; 32]),
        },
        "erase" => Effect::EraseMineableCommitment {
            llmq_type: LlmqType::Llmq50_60,
            quorum_hash: QuorumHash::from([8; 32]),
            commitment_hash: [9; 32],
        },
        "notify" => Effect::NotifyMnListChanged {
            at: BlockHash::from([10; 32]),
            undo: false,
        },
        _ => unreachable!("{kind}"),
    }
}

/// BL-786 (a), OD-CP-182 R1: a connect whose H1 committed but whose log then fails has not
/// committed, so `ConnectBlock`'s guard reverts the scheme in force where the failed effect is in
/// Core: `ProcessSpecialTxsInBlock`'s switch (`evo/specialtxman.cpp:1043-1047` at v24.0.0) comes
/// after the quorum erase and before the conflicting IS-lock's removal (`validation.cpp:2821`), and
/// `NotifyMnListChanged` follows `Commit()` (`:2927`, `:2929-2931`). With V19 at height 3, block 2
/// is processed under legacy, the published scheme, and switches to basic: a failed removal reverts
/// to legacy, a failed erase or notify reverts nothing. On default regtest (V19 at 1) from an empty
/// start's legacy (RP1a D68), block 1 enters basic, so a failed erase reverts to legacy there. The
/// published scheme stays legacy in every case.
// Core: validation.cpp:162-167, :2501-2502, :2927
#[test]
fn a_committed_connect_whose_post_switch_effect_fails_reverts_at_v19() {
    install_accept_log();
    let regtest = super::support::regtest();
    let genesis = Mined::genesis(&regtest);
    let blocks = branch(&regtest, &genesis, 2, 194);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let gid = rig.id(&genesis);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let thread = std::thread::current().id();
        let count = |line| levels_on(thread, line).len();
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut facts = Vec::new();
            // (V19's height, the block, the failed effect, the revert lines it adds)
            for (v19, n, kind, reverts) in
                [(3, 1, "is lock", 1), (3, 1, "erase", 0), (3, 1, "notify", 0), (1, 0, "erase", 1)]
            {
                let p = v19_at(v19);
                let (mut evo, _, _) = node_evo(&p, BlsCtx::LEGACY);
                let prev = if n == 0 {
                    gid
                } else {
                    ids[n - 1]
                };
                let at = at(&blocks[n], ids[n], prev);
                let before = (count(REVERTED_LEGACY), count(SWITCHED_BASIC));
                let mut target = EvoTarget::Active;
                let facade = attempt_facade(&p);
                let mut att = evo.begin_attempt(
                    &facade,
                    vcx,
                    cx,
                    AttemptId::next(),
                    AttemptOrigin::Tip,
                    &ViewTarget::Active,
                )?;
                evo.begin_tx(&mut att, &mut target)?;
                evo.bls_ctx(&mut att, &mut target, &p, &at);
                let h4 = evo.process_special_txs(
                    &mut att,
                    &mut target,
                    &blocks[n].block,
                    &at,
                    SpecialTxRules::default(),
                    &NoCoins,
                    0,
                    false,
                    true,
                    &mut || Ok(None),
                );
                let case = format!("v19@{v19}, block {}, {kind}", n + 1);
                facts.push((case.clone(), "H4 passes", matches!(h4, Ok(Ok(_)))));
                att.sink().emit(failing(kind));
                evo.set_best_block(&mut att, &mut target, &at.hash)?;
                evo.commit_bls_scheme(&mut att, &mut target);
                let finished = evo.finish_attempt(att);
                evo.rollback_tx(&mut target);
                facts.push((case.clone(), "the log fails", finished.is_err()));
                let switched = usize::from(v19 == 3);
                let after = (count(REVERTED_LEGACY), count(SWITCHED_BASIC));
                facts.push((
                    case.clone(),
                    "the lines",
                    after == (before.0 + reverts, before.1 + switched),
                ));
                facts.push((case, "legacy stays published", evo.scheme() == BlsCtx::LEGACY));
            }
            Ok(facts)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let facts = out.lock().expect("out").take().expect("V ran it");
    let failed: Vec<_> = facts.iter().filter(|(_, _, ok)| !ok).collect();
    assert!(failed.is_empty(), "{failed:?}");
}

/// An IS-lock remover whose removal fails with `failure`.
struct FailsIsLock(Fatal);

impl IsLockApplier for FailsIsLock {
    fn remove(&mut self, _: &Hash256) -> Result<(), Fatal> {
        Err(self.0.clone())
    }
}

/// OD-CP-184 (CH3b D231): `ConnectBlock`'s guard logs no revert where Core's process ends in one
/// of its asserts or a coins read fault, which run no destructor (`util/check.h:49-51`,
/// `coins.cpp:356-369` at v24.0.0). With V19 at height 3, block 2 switches to basic in H4: an
/// uncommitted connect the connector marks (`EvoHooks::mark_unlogged`) logs no revert to legacy,
/// and nor does a committed one whose IS-lock removal is refused after an assert latched (a
/// reported assert). An unmarked uncommitted connect, and a removal that fails as an
/// `AbortNode`, still revert. The published scheme stays legacy in every case.
// Core: validation.cpp:162-167, :2501-2502, :2821
#[test]
fn a_connect_that_ends_in_an_assert_logs_no_revert_at_v19() {
    install_accept_log();
    let regtest = super::support::regtest();
    let genesis = Mined::genesis(&regtest);
    let blocks = branch(&regtest, &genesis, 2, 196);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let thread = std::thread::current().id();
        let count = || levels_on(thread, REVERTED_LEGACY).len();
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let p = v19_at(3);
            let at = at(&blocks[1], ids[1], ids[0]);
            let refused = Fatal::assert("an assert latched").into_reported();
            // (the case, whether the connector marks it, the IS-lock removal's failure if the
            // connect committed, the revert lines it adds)
            let cases = [
                ("an assert or a coins read in the connect", true, None, 0),
                ("an AbortNode in the connect", false, None, 1),
                ("a removal refused after an assert", false, Some(refused), 0),
                ("a removal that fails as an AbortNode", false, Some(Fatal::new("removal")), 1),
            ];
            let mut facts = Vec::new();
            for (case, marked, removal, reverts) in cases {
                let committed = removal.is_some();
                let is_lock = FailsIsLock(removal.unwrap_or_else(|| Fatal::new("unused")));
                let (mut evo, _, _) = node_evo_is_lock(&p, BlsCtx::LEGACY, Box::new(is_lock));
                let before = count();
                let mut target = EvoTarget::Active;
                let facade = attempt_facade(&p);
                let mut att = evo.begin_attempt(
                    &facade,
                    vcx,
                    cx,
                    AttemptId::next(),
                    AttemptOrigin::Tip,
                    &ViewTarget::Active,
                )?;
                evo.begin_tx(&mut att, &mut target)?;
                evo.bls_ctx(&mut att, &mut target, &p, &at);
                let h4 = evo.process_special_txs(
                    &mut att,
                    &mut target,
                    &blocks[1].block,
                    &at,
                    SpecialTxRules::default(),
                    &NoCoins,
                    0,
                    false,
                    true,
                    &mut || Ok(None),
                );
                facts.push((case, "H4 passes", matches!(h4, Ok(Ok(_)))));
                if committed {
                    att.sink().emit(failing("is lock"));
                    evo.set_best_block(&mut att, &mut target, &at.hash)?;
                    evo.commit_bls_scheme(&mut att, &mut target);
                }
                if marked {
                    evo.mark_unlogged(&mut att);
                }
                let finished = evo.finish_attempt(att);
                evo.rollback_tx(&mut target);
                facts.push((
                    case,
                    "the log fails iff it holds the removal",
                    finished.is_err() == committed,
                ));
                facts.push((case, "the revert lines", count() == before + reverts));
                facts.push((case, "legacy stays published", evo.scheme() == BlsCtx::LEGACY));
            }
            Ok(facts)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let facts = out.lock().expect("out").take().expect("V ran it");
    let failed: Vec<_> = facts.iter().filter(|(_, _, ok)| !ok).collect();
    assert!(failed.is_empty(), "{failed:?}");
}

/// A real V over `NodeEvo` (the node's EV1 checks, ChainLock verifier and helper) on regtest with
/// V19 at height 3, with blocks 1 to 4 connected: the published scheme is basic. Its blocks.
fn walk_rig(salt: u32) -> (Rig, super::evo_chain::Kept, Vec<Mined>) {
    let p = v19_at(3);
    let (cfg, kept) = node_config(
        p,
        86_400,
        RigConfig::default(),
        DmlChecks,
        Box::new(AttemptClVerifier::default()),
        Box::new(NodeHelper),
        |b| b,
    );
    let rig = Rig::with(cfg);
    let blocks = branch(&rig.p, &rig.genesis, 4, salt);
    for m in &blocks {
        rig.accept(m);
    }
    (rig, kept, blocks)
}

/// BL-795, OD-CP-182 (CH5 D1): VerifyDB's guard (`validation.cpp:5056` at v24.0.0) is one value
/// for the walk, never committed. With V19 at height 3, level 3 over blocks 4 to 1 undoes block 2,
/// the last without V19, into legacy (`evo/specialtxman.cpp:1068-1073`), which the walk keeps
/// below it: one enter into legacy, and one revert to basic, after VerifyDB's line. Level 4 then
/// reconnects blocks 1 to 4 from legacy: block 2's switch makes it basic again, so nothing reverts.
// Core: validation.cpp:162-167, :5056, :5180
#[test]
fn a_walk_across_v19_reverts_once_after_its_line() {
    install_accept_log();
    for (level, salt, reverts) in [(3, 187, 1), (4, 188, 0)] {
        let (rig, kept, _) = walk_rig(salt);
        let v = rig.v_thread();
        let start = super::support::lines_on(v).len();
        let args = VerifyDbArgs {
            level,
            depth: 4,
        };
        assert_eq!(rig.v.verify_db(args).recv(), Ok(VerifyDbResult::Success), "level {level}");
        let lines = super::support::lines_on(v).split_off(start);
        let at = |prefix: &str| -> Vec<usize> {
            lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.starts_with(prefix))
                .map(|(i, _)| i)
                .collect()
        };
        let verified = at(VERIFIED);
        assert_eq!(verified.len(), 1, "level {level}: {lines:#?}");
        assert_eq!(at(ENTERED_LEGACY).len(), 1, "level {level}: {lines:#?}");
        let reverted = at(REVERTED_BASIC);
        assert_eq!(reverted.len(), reverts, "level {level}: {lines:#?}");
        assert!(reverted.iter().all(|&i| i > verified[0]), "level {level}: {lines:#?}");
        assert!(at(REVERTED_LEGACY).is_empty(), "level {level}: {lines:#?}");
        kept.assert_no_abort(&rig);
    }
}

/// BL-795, OD-CP-182 (CH5 D1, BL-797): with V19 at height 3, a level-3 walk whose undo of block
/// 2, the last without V19, fails before H9, on its size check (`validation.cpp:2315-2317` at
/// v24.0.0): H1 logs its enter into legacy, which Core does not, and neither reverts, as the
/// walk's scheme never moved (`UndoSpecialTxsInBlock` did not run). The record reads back whole,
/// so VerifyDB's level 2 passes it; the walk then ends at block 2's irrecoverable line.
// Core: validation.cpp:2315-2317, :5126-5129, :162-167
#[test]
fn a_walk_whose_undo_fails_before_h9_at_v19_reverts_nothing() {
    install_accept_log();
    let (rig, kept, blocks) = walk_rig(197);
    let one_more = BlockUndo {
        txs: vec![TxUndo {
            coins: vec![],
        }],
    };
    kept.replace_undo(&rig, &blocks[1], &one_more);
    let v = rig.v_thread();
    let start = lines_on(v).len();
    let args = VerifyDbArgs {
        level: 3,
        depth: 4,
    };
    let result = rig.v.verify_db(args).recv();
    assert_eq!(result, Ok(VerifyDbResult::CorruptedBlockDb));
    let lines = lines_on(v).split_off(start);
    let count = |line: &str| lines.iter().filter(|l| l.starts_with(line)).count();
    assert_eq!(count(ENTERED_LEGACY), 1, "H1's enter at block 2: {lines:#?}");
    assert_eq!(count("Verification error: irrecoverable inconsistency in block data at 2"), 1);
    assert_eq!((count(REVERTED_BASIC), count(REVERTED_LEGACY)), (0, 0), "{lines:#?}");
    kept.assert_no_abort(&rig);
}

/// One connect attempt of `block` at `at` in the connector's order (CH3 §3.1), on `scratch` or
/// else the active target, under `p`'s scheme rules: H10, H1, H4, and on success H6, H1's commit
/// and H10's commit when `commit`, else the rollback.
#[expect(clippy::too_many_arguments, reason = "an attempt's context and its block")]
fn connect(
    evo: &mut Evo,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    p: &ChainSetup,
    mut scratch: Option<&mut Scratch>,
    block: &Block,
    at: &BlockAt,
    commit: bool,
) -> Result<(), Fatal> {
    let facade = attempt_facade(p);
    let view = match scratch.as_deref_mut() {
        Some(s) => &ViewTarget::Scratch(s),
        None => &ViewTarget::Active,
    };
    let mut att =
        evo.begin_attempt(&facade, vcx, cx, AttemptId::next(), AttemptOrigin::VerifyDb, view)?;
    let mut target = match scratch {
        Some(s) => EvoTarget::Scratch(&mut s.evo),
        None => EvoTarget::Active,
    };
    evo.begin_tx(&mut att, &mut target)?;
    evo.bls_ctx(&mut att, &mut target, p, at);
    let h4 = evo.process_special_txs(
        &mut att,
        &mut target,
        block,
        at,
        SpecialTxRules::default(),
        &NoCoins,
        0,
        false,
        true,
        &mut || Ok(None),
    );
    let commit = commit && matches!(h4, Ok(Ok(_)));
    if commit {
        evo.set_best_block(&mut att, &mut target, &at.hash)?;
        evo.commit_bls_scheme(&mut att, &mut target);
    }
    evo.finish_attempt(att)?;
    if commit {
        evo.commit_tx(&mut target)
    } else {
        evo.rollback_tx(&mut target);
        Ok(())
    }
}

/// BL-786 (e), OD-CP-182 R2 and R5 (CH3a D192): `ConnectTip`'s guard (`validation.cpp:3416` at
/// v24.0.0) saves the published scheme, and its exit before `Commit()` (`:3488`, after the flush)
/// reverts what the connect published and logs it (`:162-167`). With V19 at height 3, block 2
/// publishes basic over legacy: an uncommitted guard reverts to legacy, a committed one keeps
/// basic.
#[test]
fn a_tip_guard_reverts_a_published_connect_it_did_not_commit() {
    install_accept_log();
    let p = super::support::regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 2, 189);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let thread = std::thread::current().id();
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let p19 = v19_at(3);
            let at2 = at(&blocks[1], ids[1], ids[0]);
            let mut facts = Vec::new();
            for (commit, reverts, published) in
                [(false, 1, BlsCtx::LEGACY), (true, 0, BlsCtx::BASIC)]
            {
                let (mut evo, _, _) = node_evo(&p19, BlsCtx::LEGACY);
                let before = levels_on(thread, REVERTED_LEGACY).len();
                evo.open_scheme_guard(SchemeGuardHolder::Tip);
                connect(&mut evo, vcx, cx, &p19, None, &blocks[1].block, &at2, true)?;
                facts.push((commit, "the connect publishes basic", evo.scheme() == BlsCtx::BASIC));
                if commit {
                    evo.commit_scheme_guard();
                }
                evo.exit_scheme_guard(true);
                let after = levels_on(thread, REVERTED_LEGACY).len();
                facts.push((commit, "the revert lines", after == before + reverts));
                facts.push((commit, "the published scheme", evo.scheme() == published));
            }
            Ok(facts)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let facts = out.lock().expect("out").take().expect("V ran it");
    let failed: Vec<_> = facts.iter().filter(|(_, _, ok)| !ok).collect();
    assert!(failed.is_empty(), "{failed:?}");
}

/// One of VerifyDB's scratch disconnects of `block` at `at` on `scratch`, under `p`'s scheme
/// rules, in the connector's order: H10, H1, H9, then, when its `try` caught a failure, the catch's
/// restore of the scheme the undo was entered with, the next block's; then the log and the
/// rollback. Whatever H9 answers, its scheme switch has happened (`evo/specialtxman.cpp:1068-1073`
/// at v24.0.0).
#[expect(clippy::too_many_arguments, reason = "an attempt's context, its block and the catch")]
fn walk_undo(
    evo: &mut Evo,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    p: &ChainSetup,
    scratch: &mut Scratch,
    block: &Block,
    at: &BlockAt,
    caught: bool,
) -> Result<(), Fatal> {
    let facade = attempt_facade(p);
    let (origin, view) = (AttemptOrigin::VerifyDb, ViewTarget::Scratch(&mut *scratch));
    let mut att = evo.begin_attempt(&facade, vcx, cx, AttemptId::next(), origin, &view)?;
    evo.mark_disconnect(&mut att);
    let mut target = EvoTarget::Scratch(&mut scratch.evo);
    evo.begin_tx(&mut att, &mut target)?;
    evo.bls_ctx(&mut att, &mut target, p, at);
    let _ = evo.undo_special_txs(&mut att, &mut target, block, at);
    if caught {
        evo.undo_caught(bls_ctx_for_block(p, at.height + 1));
    }
    let _ = evo.finish_attempt(att);
    evo.rollback_tx(&mut target);
    Ok(())
}

/// BL-795 (d), OD-CP-182 (CH5 D23): `UndoSpecialTxsInBlock`'s catch restores the scheme the undo
/// was entered with (`evo/specialtxman.cpp:1065`, `:1086-1088` at v24.0.0), which in VerifyDB's
/// walk is the walk's. With V19 at height 2, the walk's first undo, of block 1 (B), is entered
/// with the published basic and switches to legacy; its catch restores basic, so the walk's guard
/// reverts nothing (`validation.cpp:162-167`, `:5056`). Below B (V19 at height 3, blocks 2 then
/// 1), block 1's undo is entered with the legacy block 2's left, which its catch restores, so
/// the guard reverts to basic once.
#[test]
fn a_walk_undo_catch_at_v19_logs_no_revert() {
    install_accept_log();
    let p = super::support::regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 2, 190);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let gid = rig.id(&genesis);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let thread = std::thread::current().id();
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut facts = Vec::new();
            // (V19's height, the blocks the walk undoes, from the tip, the revert lines)
            for (v19, undone, reverts) in [(2, vec![0], 0), (3, vec![1, 0], 1)] {
                let p = v19_at(v19);
                let (mut evo, layer, _) = node_evo(&p, BlsCtx::BASIC);
                let mut scratch = Scratch::over(layer.scratch());
                let before = levels_on(thread, REVERTED_BASIC).len();
                evo.open_scheme_guard(SchemeGuardHolder::Walk);
                for &n in &undone {
                    let prev = if n == 0 {
                        gid
                    } else {
                        ids[n - 1]
                    };
                    let at = at(&blocks[n], ids[n], prev);
                    walk_undo(&mut evo, vcx, cx, &p, &mut scratch, &blocks[n].block, &at, n == 0)?;
                }
                evo.exit_scheme_guard(true);
                let after = levels_on(thread, REVERTED_BASIC).len();
                facts.push((v19, "the revert lines", after == before + reverts));
                facts.push((v19, "basic stays published", evo.scheme() == BlsCtx::BASIC));
            }
            Ok(facts)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let facts = out.lock().expect("out").take().expect("V ran it");
    let failed: Vec<_> = facts.iter().filter(|(_, _, ok)| !ok).collect();
    assert!(failed.is_empty(), "{failed:?}");
}

/// A reconnect plan: its case, the blocks reconnected with whether each commits, and the revert
/// lines it adds, its own and the walk's.
type Plan = (&'static str, &'static [(usize, bool)], (usize, usize));

/// BL-795 (f), OD-CP-182 R3 (CH5 D1): a reconnect's `ConnectBlock` guard (`validation.cpp:2501` at
/// v24.0.0) saves the walk's scheme, not the published one. With V19 at height 3, the walk
/// undoes blocks 2 and 1 into legacy (one enter, at block 2); level 4's reconnect of block 1 then
/// enters nothing, and when it fails it reverts nothing. A kept reconnect leaves its scheme for the
/// walk: blocks 1 and 2 reconnected leave basic, so the walk's guard reverts nothing. Block 2's
/// reconnect failing after its switch to basic (`evo/specialtxman.cpp:1043-1047`) reverts twice:
/// its own guard back to the walk's legacy, then the walk's back to basic (`:162-167`).
#[test]
fn a_failed_reconnect_below_v19_reverts_nothing() {
    install_accept_log();
    let p = super::support::regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 2, 191);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let gid = rig.id(&genesis);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let thread = std::thread::current().id();
        let count = |line| levels_on(thread, line).len();
        let lines = || (count(ENTERED_LEGACY), count(REVERTED_BASIC), count(REVERTED_LEGACY));
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let p = v19_at(3);
            let at = |n: usize| {
                at(
                    &blocks[n],
                    ids[n],
                    if n == 0 {
                        gid
                    } else {
                        ids[n - 1]
                    },
                )
            };
            let mut facts = Vec::new();
            let plans: [Plan; 3] = [
                ("block 1 fails", &[(0, false)], (0, 1)),
                ("blocks 1 and 2 kept", &[(0, true), (1, true)], (0, 0)),
                ("block 2 fails", &[(0, true), (1, false)], (1, 1)),
            ];
            for (case, reconnects, (own, walks)) in plans {
                let (mut evo, layer, _) = node_evo(&p, BlsCtx::BASIC);
                let mut scratch = Scratch::over(layer.scratch());
                let start = lines();
                evo.open_scheme_guard(SchemeGuardHolder::Walk);
                for n in [1, 0] {
                    walk_undo(
                        &mut evo,
                        vcx,
                        cx,
                        &p,
                        &mut scratch,
                        &blocks[n].block,
                        &at(n),
                        false,
                    )?;
                }
                let walked = lines();
                facts.push((
                    case,
                    "one enter, at block 2",
                    walked == (start.0 + 1, start.1, start.2),
                ));
                for &(n, commit) in reconnects {
                    let block = &blocks[n].block;
                    connect(&mut evo, vcx, cx, &p, Some(&mut scratch), block, &at(n), commit)?;
                }
                let reconnected = (walked.0, walked.1, walked.2 + own);
                facts.push((case, "the reconnects enter nothing", lines() == reconnected));
                evo.exit_scheme_guard(true);
                let exited = (walked.0, walked.1 + walks, walked.2 + own);
                facts.push((case, "the walk's revert", lines() == exited));
                let order = lines_on(thread);
                let last = |line| order.iter().rposition(|l| l == line);
                let ordered = own == 0 || last(REVERTED_LEGACY) < last(REVERTED_BASIC);
                facts.push((case, "the reconnect's revert, then the walk's", ordered));
            }
            Ok(facts)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let facts = out.lock().expect("out").take().expect("V ran it");
    let failed: Vec<_> = facts.iter().filter(|(_, _, ok)| !ok).collect();
    assert!(failed.is_empty(), "{failed:?}");
}
