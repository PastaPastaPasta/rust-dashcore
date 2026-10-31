//! `NodeEvo`'s CH7a-3 half on a real V: H7's MN-list signal (D15), inline for an Active attempt
//! and through the log for VerifyDB's scratch one; the scratch target, which reads and commits
//! its own layer (§6.3 step 5); and the reads V publishes and serves (D16, D17).

use std::sync::{Arc, Mutex};

use dash_consensus::ctx::DeploymentView as _;
use dash_dml::testkit::numbered;
use dash_dml::{EvoDelta, MnList, MnListChange, MnListDiff};
use dash_params::{BuriedDeployment, ChainOption, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, LlmqType, QuorumHash};
use dash_types::Numeric as _;

use super::node_evo::{NoCoins, at, node_evo_on};
use super::support::{Rig, RigConfig, branch, regtest, with_tip_cx};
use crate::chain::{
    AttemptOrigin, EvoReadCx, EvoReads, EvoTarget, EvoTxn, Fatal, ScratchCoins, ScratchView,
    ValidationInterface, ValidationSignals, ViewTarget,
};
use crate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use crate::connect::{AttemptId, EvoHooks, SpecialTxRules};
use crate::evo::EvoLayer;
use crate::evo::effects::MnListSignal;
use crate::evo::ev1::Ev1NotBuilt;
use crate::evo::node::SCRATCH_WITHOUT_LAYER;
use crate::flush::EvoWindow as _;
use crate::store::{GuardedEngine, ProbeEngine, RecordingSink};

/// One MN-list signal: `undo`, the old list and the diff.
type Signal = (bool, Arc<MnList>, Arc<MnListDiff>);

/// The MN-list signals a listener got.
#[derive(Default)]
struct Lists(Mutex<Vec<Signal>>);

impl Lists {
    fn take(&self) -> Vec<Signal> {
        std::mem::take(&mut *self.0.lock().expect("lists"))
    }
}

impl ValidationInterface for Lists {
    fn notify_masternode_list_changed(
        &self,
        undo: bool,
        old: &Arc<MnList>,
        diff: &Arc<MnListDiff>,
    ) {
        self.0.lock().expect("lists").push((undo, old.clone(), diff.clone()));
    }
}

/// A change with a masternode added, and one whose diff has none.
fn changes() -> (MnListChange, MnListChange) {
    let old = Arc::new(MnList::default_pre_dip3());
    let mut next = MnList::default_pre_dip3();
    next.try_add(numbered(0), true).expect("adds");
    let new = Arc::new(next);
    let added = MnListChange {
        diff: Arc::new(old.diff_to(&new)),
        old: old.clone(),
        new,
    };
    let none = MnListChange {
        diff: Arc::new(old.diff_to(&old)),
        old: old.clone(),
        new: old,
    };
    assert!(added.diff.has_changes() && !none.diff.has_changes());
    (added, none)
}

/// Whether `got` is `change`'s signal, by identity: the listener gets H7's own lists.
fn is_signal(
    got: &(bool, Arc<MnList>, Arc<MnListDiff>),
    undo: bool,
    change: &MnListChange,
) -> bool {
    got.0 == undo && Arc::ptr_eq(&got.1, &change.old) && Arc::ptr_eq(&got.2, &change.diff)
}

/// `VerifyDB`'s scratch view: a coins layer of its own and an evo layer copied from the active one.
struct Scratch {
    coins: CoinsCache<ProbeEngine>,
    evo: EvoLayer,
}

impl Scratch {
    fn over(evo: EvoLayer) -> Scratch {
        let db = Arc::new(GuardedEngine::probe(Arc::new(RecordingSink::default())));
        let budget = CoinsBudget::new(8 << 20, 0, Ratio::HALF, 0);
        Scratch {
            coins: CoinsCache::new(db, budget, Salt::fixed(9), None),
            evo,
        }
    }
}

impl ScratchView for Scratch {
    fn split(&mut self) -> (ScratchCoins<'_>, &mut dyn EvoTxn) {
        (ScratchCoins::new(&mut self.coins), &mut self.evo)
    }
}

/// An evo transaction that keeps no layer.
struct NoLayer;

impl EvoTxn for NoLayer {}

/// D15 for an Active attempt: H7 runs the listeners inline with its own lists, in either
/// direction, only when the diff has changes, and leaves nothing for the log.
#[test]
fn h7_signals_an_active_attempts_change_inline() {
    let p = regtest();
    let rig = Rig::new();
    let (lists, signals) = (Arc::new(Lists::default()), ValidationSignals::default());
    signals.register(lists.clone());
    let seen = lists.clone();
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        let (mut evo, _, _) = node_evo_on(&p, bls(&p), Ev1NotBuilt, &signals);
        let (added, none) = changes();
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut att = evo.begin_attempt(
                &(),
                vcx,
                cx,
                AttemptId::next(),
                AttemptOrigin::Tip,
                &ViewTarget::Active,
            )?;
            let mut facts = Vec::new();
            evo.list_changed(&mut att, &Some(added.clone()), false);
            evo.list_changed(&mut att, &Some(added.clone()), true);
            let got = seen.take();
            facts.push(got.len() == 2);
            facts.push(is_signal(&got[0], false, &added) && is_signal(&got[1], true, &added));
            evo.list_changed(&mut att, &Some(none.clone()), false);
            evo.list_changed(&mut att, &None, true);
            evo.finish_attempt(att)?;
            facts.push(seen.take().is_empty());
            Ok(facts)
        })?;
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    assert_eq!(out.lock().expect("out").take(), Some(vec![true; 3]));
    rig.assert_no_abort();
}

/// D15 and §6.3 step 5 for a VerifyDB attempt: its hooks read and commit the scratch layer,
/// never the active one; H7's signal waits in the log and reaches the listeners, with H7's
/// lists, when the log is applied; its committed scheme is not published (D61).
#[test]
fn a_verify_db_attempt_signals_through_its_log_and_commits_its_scratch_layer() {
    let p = regtest();
    let rig = Rig::new();
    let blocks = branch(&p, &rig.genesis, 1, 4);
    rig.headers(&blocks);
    let (id, gid) = (rig.id(&blocks[0]), rig.id(&rig.genesis));
    let (lists, signals) = (Arc::new(Lists::default()), ValidationSignals::default());
    signals.register(lists.clone());
    let seen = lists.clone();
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        // A scheme other than block 1's, so its commit would publish one.
        let block1 = bls(&p);
        let before = dash_consensus::BlsCtx::from_legacy(
            block1 == dash_consensus::BlsCtx::from_legacy(false),
        );
        let (mut evo, layer, _) = node_evo_on(&p, before, Ev1NotBuilt, &signals);
        let (added, _) = changes();
        let at1 = at(&blocks[0], id, gid);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut scratch = Scratch::over(layer.scratch());
            let mut att = evo.begin_attempt(
                &(),
                vcx,
                cx,
                AttemptId::next(),
                AttemptOrigin::VerifyDb,
                &ViewTarget::Scratch(&mut scratch),
            )?;
            let mut facts = Vec::new();
            {
                let mut target = EvoTarget::Scratch(&mut scratch.evo);
                evo.begin_tx(&mut att, &mut target)?;
                facts.push(evo.bls_ctx(&mut att, &mut target, cx.params, &at1) != before);
                let h4 = evo.process_special_txs(
                    &mut att,
                    &mut target,
                    &blocks[0].block,
                    &at1,
                    SpecialTxRules::default(),
                    &NoCoins,
                    0,
                    false,
                    true,
                )?;
                facts.push(matches!(h4, Ok(None)));
                evo.set_best_block(&mut att, &mut target, &at1.hash)?;
                evo.list_changed(&mut att, &Some(added.clone()), false);
                facts.push(seen.take().is_empty());
                evo.commit_bls_scheme(&mut att, &mut target);
                evo.finish_attempt(att)?;
                let got = seen.take();
                facts.push(got.len() == 1 && is_signal(&got[0], false, &added));
                evo.commit_tx(&mut target)?;
            }
            facts.push(evo.published_bls() == Some(before));
            facts.push(scratch.evo.best_block() == Some(at1.hash));
            facts.push(layer.best_block().is_none());
            // H2 reads the layer its target names: the scratch one has the block, the active one not.
            let mut check = evo.begin_attempt(
                &(),
                vcx,
                cx,
                AttemptId::next(),
                AttemptOrigin::VerifyDb,
                &ViewTarget::Scratch(&mut scratch),
            )?;
            let mut target = EvoTarget::Scratch(&mut scratch.evo);
            facts.push(evo.verify_best_block(&mut check, &mut target, &at1.hash)?);
            let mut active = EvoTarget::Active;
            facts.push(!evo.verify_best_block(&mut check, &mut active, &at1.hash)?);
            evo.finish_attempt(check)?;
            Ok(facts)
        })?;
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    assert_eq!(out.lock().expect("out").take(), Some(vec![true; 9]));
    rig.assert_no_abort();
}

/// D15's applier dispatches a queued notice only to the effect it was queued for: one for
/// another block, or the other direction, is `Fatal` and reaches no listener.
#[test]
fn the_notify_applier_refuses_another_blocks_or_directions_notice() {
    use crate::evo::effects::NotifyApplier as _;

    let (lists, signals) = (Arc::new(Lists::default()), ValidationSignals::default());
    signals.register(lists.clone());
    let signal = MnListSignal::new(signals);
    let (added, _) = changes();
    let (a, b) = (BlockHash::from([1; 32]), BlockHash::from([2; 32]));
    let mut applier = signal.applier();
    for (at, undo) in [(b, false), (a, true)] {
        let _ = signal.defer(a, false, &added);
        assert!(applier.notify(&at, undo).is_err(), "{at:?} {undo}");
    }
    let _ = signal.defer(a, true, &added);
    applier.notify(&a, true).expect("its own notice");
    let got = lists.take();
    assert!(got.len() == 1 && is_signal(&got[0], true, &added));
}

/// A VerifyDB attempt that stops before its log is applied leaves no notice behind: the next
/// attempt's signal reaches the listeners with its own lists (D15).
#[test]
fn an_unapplied_logs_notice_does_not_reach_the_next_attempt() {
    let p = regtest();
    let rig = Rig::new();
    let blocks = branch(&p, &rig.genesis, 1, 8);
    rig.headers(&blocks);
    let (id, gid) = (rig.id(&blocks[0]), rig.id(&rig.genesis));
    let (lists, signals) = (Arc::new(Lists::default()), ValidationSignals::default());
    signals.register(lists.clone());
    let seen = lists.clone();
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, _) = node_evo_on(&p, bls(&p), Ev1NotBuilt, &signals);
        let (first, _) = changes();
        let (second, _) = changes();
        let at1 = at(&blocks[0], id, gid);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut scratch = Scratch::over(layer.scratch());
            for (change, apply) in [(&first, false), (&second, true)] {
                let mut att = evo.begin_attempt(
                    &(),
                    vcx,
                    cx,
                    AttemptId::next(),
                    AttemptOrigin::VerifyDb,
                    &ViewTarget::Scratch(&mut scratch),
                )?;
                let mut target = EvoTarget::Scratch(&mut scratch.evo);
                evo.begin_tx(&mut att, &mut target)?;
                evo.bls_ctx(&mut att, &mut target, cx.params, &at1);
                let h4 = evo.process_special_txs(
                    &mut att,
                    &mut target,
                    &blocks[0].block,
                    &at1,
                    SpecialTxRules::default(),
                    &NoCoins,
                    0,
                    false,
                    true,
                )?;
                assert!(matches!(h4, Ok(None)));
                evo.set_best_block(&mut att, &mut target, &at1.hash)?;
                evo.list_changed(&mut att, &Some(change.clone()), false);
                evo.rollback_tx(&mut target);
                if apply {
                    evo.finish_attempt(att)?;
                }
            }
            let got = seen.take();
            Ok(got.len() == 1 && is_signal(&got[0], false, &second))
        })?;
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    assert_eq!(out.lock().expect("out").take(), Some(true));
    rig.assert_no_abort();
}

/// A scratch target whose transaction keeps no layer fails closed at every hook that reads,
/// stages or opens.
#[test]
fn a_scratch_target_without_a_layer_fails_closed() {
    let p = regtest();
    let rig = Rig::new();
    let blocks = branch(&p, &rig.genesis, 1, 5);
    rig.headers(&blocks);
    let (id, gid) = (rig.id(&blocks[0]), rig.id(&rig.genesis));
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        let (mut evo, _, _) = node_evo_on(&p, bls(&p), Ev1NotBuilt, &ValidationSignals::default());
        let at1 = at(&blocks[0], id, gid);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut att = evo.begin_attempt(
                &(),
                vcx,
                cx,
                AttemptId::next(),
                AttemptOrigin::VerifyDb,
                &ViewTarget::Active,
            )?;
            let mut no_layer = NoLayer;
            let mut target = EvoTarget::Scratch(&mut no_layer);
            let refused = |r: Result<(), Fatal>| r.err().map(|f| f.0);
            let mut facts = vec![
                refused(evo.begin_tx(&mut att, &mut target)),
                refused(evo.verify_best_block(&mut att, &mut target, &at1.hash).map(drop)),
            ];
            let mut active = EvoTarget::Active;
            evo.begin_tx(&mut att, &mut active)?;
            facts.push(refused(evo.set_best_block(&mut att, &mut target, &at1.hash)));
            evo.finish_attempt(att)?;
            evo.rollback_tx(&mut active);
            Ok(facts)
        })?;
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    let facts = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(facts, vec![Some(SCRATCH_WITHOUT_LAYER.to_owned()); 3]);
    rig.assert_no_abort();
}

/// D16, D17 through `NodeEvo`'s reads: below DIP0003 every list is Core's empty one; the pool
/// after a block is `None` until V20 is active after it, then the pool after the block (the
/// empty one before V20's own height); a commitment is mined only on chains that hold its block.
#[test]
fn node_evo_reads_lists_pools_and_mined_commitments() {
    let v20_at_3 = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "3:3")
        .arg(ChainOption::TestActivationHeight, "v20@3")
        .build()
        .expect("DIP0003 and V20 at 3");
    let p = Arc::new(v20_at_3);
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        ..RigConfig::default()
    });
    let main = branch(&p, &rig.genesis, 2, 6);
    let side = branch(&p, &rig.genesis, 1, 7);
    rig.headers(&main);
    rig.headers(&side);
    let [a1, a2] = rig.ids_n::<2>(&main);
    let b1 = rig.id(&side[0]);
    let (h1, height1) = (main[0].hash(), main[0].height);
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        let (evo, layer, store) =
            node_evo_on(&p, bls(&p), Ev1NotBuilt, &ValidationSignals::default());
        let deployments = v.deployments.view();
        let cx = EvoReadCx {
            reader: &v.reader,
            deployments: &deployments,
        };
        let v20 = dash_consensus::ctx::Deployment::Buried(BuriedDeployment::V20);
        let q = QuorumHash::from([3; 32]);
        let llmq = LlmqType::Llmq50_60;
        let mined = dash_dml::store::MinedCommitmentRecord {
            llmq_type: llmq,
            quorum_hash: q,
            base_height: 0,
            rotation_index: None,
            commitment: Arc::new(crate::evo::testkit::commitment(llmq, q, 1)),
        };
        let delta = EvoDelta {
            block: h1,
            height: height1,
            dml: None,
            mined_commitments: vec![mined],
            credit_data: None,
            credit_pool: None,
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: Some(h1),
        };
        store.commit(&mut layer.lock(), &delta).map_err(fatal)?;
        let list = evo.list_at(cx, a2).map_err(fatal)?;
        let facts = vec![
            *list.block_hash() == BlockHash::ZERO && list.height() == -1,
            !deployments.active_after(Some(a1), v20)
                && evo.credit_pool_after(cx, a1).map_err(fatal)?.is_none(),
            deployments.active_after(Some(a2), v20)
                && !deployments.active_at(a2, v20)
                && evo
                    .credit_pool_after(cx, a2)
                    .map_err(fatal)?
                    .is_some_and(|pool| (*pool).as_ref().is_ok_and(|p| p.locked == 0)),
            evo.has_mined_commitment(cx, llmq, &q, a2).map_err(fatal)?,
            evo.has_mined_commitment(cx, llmq, &q, a1).map_err(fatal)?,
            !evo.has_mined_commitment(cx, llmq, &q, b1).map_err(fatal)?,
            !evo.has_mined_commitment(cx, llmq, &QuorumHash::from([4; 32]), a2).map_err(fatal)?,
        ];
        *put.lock().expect("out") = Some(facts);
        Ok(())
    });
    assert_eq!(out.lock().expect("out").take(), Some(vec![true; 7]));
    rig.assert_no_abort();
}

fn fatal(e: dash_dml::StoreError) -> Fatal {
    Fatal::new(e.to_string())
}

/// The scheme at the regtest tip's next block, as the node starts `NodeEvo` (D9).
fn bls(p: &dash_params::ChainParams) -> dash_consensus::BlsCtx {
    dash_consensus::ctx::bls_ctx_for_block(p, 1)
}
