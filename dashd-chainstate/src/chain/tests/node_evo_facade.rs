//! `NodeEvo`'s attempts on the members facade (EV addendum §2.6, CH7 §4.2.3), the hooks called
//! as the connector calls them: each attempt holds the gate exclusively from `begin_attempt`
//! until its log is applied and it is released, whatever its outcome and target; a log that fails
//! to apply trips the latch before the gate is released (D4); and an attempt's members binding
//! takes H1's scheme and its target's layer (Q2).

use std::sync::{Arc, Mutex};

use dash_consensus::{Block, BlsCtx};
use dash_dml::testkit::numbered;
use dash_dml::{EvoDelta, MnList};
use dash_evo::Effect;
use dash_evo::llmq::LlmqSource as _;
use dash_evo::types::ProTxHash;
use dash_kv::ErrorKind;
use dash_params::{ChainOption, ChainParamsBuilder, NetworkKind, Release};
use dashd_store::MemoryEngine;

use super::llmq_members::world;
use super::node_evo::{NoCoins, at, attempt_facade, bodies, node_evo_on, node_evo_with_bans};
use super::node_evo_lists::{Scratch, bls};
use super::support::{Rig, RigConfig, branch, regtest, with_tip_cx};
use crate::chain::{
    AttemptOrigin, ConnectCx, EvoTarget, Fatal, VContext, ValidationSignals, ViewTarget,
};
use crate::connect::{
    AttemptId, BlockAt, EvoAttempt as _, EvoHooks, SpecialTxFailure, SpecialTxRules,
    pre_dip3_special_txs,
};
use crate::evo::effects::PlatformBanApplier;
use crate::evo::ev1::{Ev1Checks, Ev1NotBuilt};
use crate::evo::node::NodeEvo;
use crate::evo::testkit;
use crate::flush::EvoWindow as _;
use crate::llmq::facade::MembersFacade;
use crate::store::{Failure, GlobalFailStop};

/// One attempt on `facade` in the connector's order (CH3 §3.1): H10, H1, H4, then, for a tip
/// connect H4 passed, H6, H1's commit and H10's commit, else the rollback. A scratch target is
/// VerifyDB's, a block without an index entry a proposal's. Whether the gate was held exclusively
/// while the attempt was open, and H4's verdict.
fn attempt<C: Ev1Checks + Send>(
    evo: &mut NodeEvo<MemoryEngine, C>,
    facade: &MembersFacade,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    mut scratch: Option<&mut Scratch>,
    block: &Block,
    at: &BlockAt,
) -> Result<(bool, Result<(), SpecialTxFailure>), Fatal> {
    let (origin, view) = match scratch.as_deref_mut() {
        Some(s) => (AttemptOrigin::VerifyDb, ViewTarget::Scratch(s)),
        None if at.id.is_none() => (AttemptOrigin::Proposal, ViewTarget::Active),
        None => (AttemptOrigin::Tip, ViewTarget::Active),
    };
    let mut att = evo.begin_attempt(facade, vcx, cx, AttemptId::next(), origin, &view)?;
    let held = facade.gate().is_locked_exclusive();
    let mut target = match scratch {
        Some(s) => EvoTarget::Scratch(&mut s.evo),
        None => EvoTarget::Active,
    };
    evo.begin_tx(&mut att, &mut target)?;
    evo.bls_ctx(&mut att, &mut target, cx.params, at);
    let just_check = origin == AttemptOrigin::Proposal;
    let rules = SpecialTxRules::default();
    let h4 = evo.process_special_txs(
        &mut att,
        &mut target,
        block,
        at,
        rules,
        &NoCoins,
        0,
        just_check,
        true,
    )?;
    let commit = origin == AttemptOrigin::Tip && h4.is_ok();
    if commit {
        evo.set_best_block(&mut att, &mut target, &at.hash)?;
        evo.commit_bls_scheme(&mut att, &mut target);
    }
    evo.finish_attempt(att)?;
    if commit {
        evo.commit_tx(&mut target)?;
    } else {
        evo.rollback_tx(&mut target);
    }
    Ok((held, h4.map(drop)))
}

/// Every attempt holds the facade's gate exclusively from `begin_attempt` until
/// `finish_attempt` has applied its log and released it, whatever its outcome and target: an
/// invalid block, a proposal, a VerifyDB attempt and a committed connect (EV addendum §2.6).
/// One D1 refuses opens no handle: the gate stays free and the latch untripped.
#[test]
fn every_attempt_holds_the_gate_until_its_release() {
    let p = regtest();
    let rig = Rig::new();
    let blocks = branch(&p, &rig.genesis, 1, 11);
    rig.headers(&blocks);
    let (id, gid) = (rig.id(&blocks[0]), rig.id(&rig.genesis));
    let w = world(&p);
    let (facade, latch) = (w.facade.clone(), w.latch.clone());
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, _) =
            node_evo_on(&p, bls(&p), Ev1NotBuilt, &ValidationSignals::default());
        let invalid =
            bodies().into_iter().find(|b| pre_dip3_special_txs(b, true).is_err()).expect("one");
        let at1 = at(&blocks[0], id, gid);
        let proposal = BlockAt {
            id: None,
            ..at1
        };
        let block = &blocks[0].block;
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let free = || !facade.gate().is_locked();
            let mut scratch = Scratch::over(layer.scratch());
            let mut facts = Vec::new();
            let (held, h4) = attempt(&mut evo, &facade, vcx, cx, None, &invalid, &at1)?;
            facts.push(held && h4.is_err() && free());
            let (held, h4) = attempt(&mut evo, &facade, vcx, cx, None, block, &proposal)?;
            facts.push(held && h4.is_ok() && free());
            let (held, h4) = attempt(&mut evo, &facade, vcx, cx, Some(&mut scratch), block, &at1)?;
            facts.push(held && h4.is_ok() && free() && scratch.evo.best_block().is_none());
            let (held, h4) = attempt(&mut evo, &facade, vcx, cx, None, block, &at1)?;
            facts.push(held && h4.is_ok() && free() && layer.best_block() == Some(at1.hash));

            // D1: an open evo transaction refuses the next attempt before it takes the gate.
            let (origin, mut target) = (AttemptOrigin::Tip, EvoTarget::Active);
            let id = AttemptId::next();
            let mut open = evo.begin_attempt(&facade, vcx, cx, id, origin, &ViewTarget::Active)?;
            evo.begin_tx(&mut open, &mut target)?;
            evo.finish_attempt(open)?;
            let id = AttemptId::next();
            let refused = evo.begin_attempt(&facade, vcx, cx, id, origin, &ViewTarget::Active);
            facts.push(refused.is_err() && free());
            drop(refused);
            evo.rollback_tx(&mut target);
            facts.push(latch.tripped().is_none());
            Ok(facts)
        })?;
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    assert_eq!(out.lock().expect("out").take(), Some(vec![true; 6]));
    rig.assert_no_abort();
}

/// What the failing applier answers.
const BANS: &str = "the platform-ban reset failed";

/// What the failing applier latches first when it stands for a store.
const STORE: &str = "the store's write failed";

/// A platform-ban applier that fails: with a latch it trips it first and reports, as a store's
/// fault does (D5); without one it reports nothing.
struct FailingBans(Option<GlobalFailStop>);

impl PlatformBanApplier for FailingBans {
    fn reset(&mut self, _: &ProTxHash, _: i32) -> Result<(), Fatal> {
        if let Some(latch) = &self.0 {
            latch.trip(Failure {
                kind: ErrorKind::Other,
                site: "test",
                message: STORE.to_owned(),
            });
        }
        Err(Fatal::new(BANS))
    }
}

/// What a failed application left: whether V reports the failure once the latch has tripped,
/// its message, the gate and the latch at the trip, then the gate and the latch after
/// `finish_attempt`.
type AfterFailure = (bool, String, Option<(bool, Option<String>)>, bool, Option<String>);

/// D4: a log that fails to apply trips the latch before the gate is released, so a reader
/// waiting on the gate wakes to the failure. An applier's own failure, which nothing reported,
/// is the latched one, and V reports it although the latch has tripped; a failure a store
/// latched as it reported it stays the first, and V does not report it again.
#[test]
fn a_log_that_fails_to_apply_trips_the_latch_before_the_gate_opens() {
    let p = regtest();
    let rig = Rig::new();
    let out = Arc::new(Mutex::new(Vec::new()));
    let put = out.clone();
    rig.on_v(move |v| {
        for stored in [false, true] {
            let w = world(&p);
            let at_trip = Arc::new(Mutex::new(None));
            {
                let (facade, latch, at_trip) =
                    (Arc::downgrade(&w.facade), w.latch.clone(), at_trip.clone());
                *w.facade.hooks.after_trip.lock() = Some(Box::new(move || {
                    let facade = facade.upgrade().expect("the facade lives");
                    let latched = latch.tripped().map(|f| f.message.clone());
                    *at_trip.lock().expect("at trip") =
                        Some((facade.gate().is_locked_exclusive(), latched));
                }));
            }
            let bans = FailingBans(stored.then(|| w.latch.clone()));
            let (mut evo, _, _) = node_evo_with_bans(&p, bls(&p), Box::new(bans));
            let failed = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
                let (id, origin) = (AttemptId::next(), AttemptOrigin::Tip);
                let mut att =
                    evo.begin_attempt(&w.facade, vcx, cx, id, origin, &ViewTarget::Active)?;
                att.sink().emit(Effect::ResetPlatformBan {
                    pro_tx_hash: ProTxHash::default(),
                    height: 1,
                });
                Ok(evo.finish_attempt(att).err())
            })?;
            let failed = failed.ok_or_else(|| Fatal::new("the log applied"))?;
            let after: AfterFailure = (
                failed.reported_by_v(true),
                failed.0,
                at_trip.lock().expect("at trip").take(),
                w.facade.gate().is_locked(),
                w.latch.tripped().map(|f| f.message.clone()),
            );
            put.lock().expect("out").push(after);
        }
        Ok(())
    });
    let at_trip = |m: &str| Some((true, Some(m.to_owned())));
    let want: Vec<AfterFailure> = vec![
        (true, BANS.to_owned(), at_trip(BANS), false, Some(BANS.to_owned())),
        (false, BANS.to_owned(), at_trip(STORE), false, Some(STORE.to_owned())),
    ];
    assert_eq!(*out.lock().expect("out"), want);
    rig.assert_no_abort();
}

/// Q2's regression (a), M29: an attempt's binding takes H1's scheme. Before H1 there is no
/// binding; at a block whose scheme is not the published one, its scheme is the block's.
#[test]
fn the_binding_takes_h1s_scheme_and_there_is_none_before_h1() {
    let p = regtest();
    let rig = Rig::new();
    let blocks = branch(&p, &rig.genesis, 1, 12);
    rig.headers(&blocks);
    let (id, gid) = (rig.id(&blocks[0]), rig.id(&rig.genesis));
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        // As if the tip were genesis before V19's height 1: the published scheme is legacy.
        let signals = ValidationSignals::default();
        let (mut evo, _, _) = node_evo_on(&p, BlsCtx::LEGACY, Ev1NotBuilt, &signals);
        let at1 = at(&blocks[0], id, gid);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let facade = attempt_facade(cx.params);
            let (id, origin) = (AttemptId::next(), AttemptOrigin::Tip);
            let mut att = evo.begin_attempt(&facade, vcx, cx, id, origin, &ViewTarget::Active)?;
            let mut target = EvoTarget::Active;
            let before = evo.bind_attempt(&mut att, &mut target).err().map(|f| f.0);
            let h1 = evo.bls_ctx(&mut att, &mut target, cx.params, &at1);
            let bound = {
                let (binding, _) = evo.bind_attempt(&mut att, &mut target)?;
                binding.with_llmq(|ctx, _| ctx.bls())
            };
            evo.finish_attempt(att)?;
            Ok((before, h1, bound, evo.published_bls()))
        })?;
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    let (before, h1, bound, published) = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(before.as_deref(), Some("members before H1's scheme"));
    assert_eq!((h1, bound, published), (BlsCtx::BASIC, BlsCtx::BASIC, Some(BlsCtx::LEGACY)));
    rig.assert_no_abort();
}

/// Q2's regression (b), M30: an attempt's binding reads its target's layer. A VerifyDB attempt's
/// binding finds the list its scratch layer holds; a tip attempt's finds no row for the block in
/// the active layer, which lacks it.
#[test]
fn the_binding_reads_its_targets_layer() {
    let dip3_at_3 = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "3:3")
        .build()
        .expect("DIP0003 at 3");
    let p = Arc::new(dip3_at_3);
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        ..RigConfig::default()
    });
    let blocks = branch(&p, &rig.genesis, 3, 13);
    rig.headers(&blocks);
    let [_, a2, a3] = rig.ids_n::<3>(&blocks);
    let (h3, height3) = (blocks[2].hash(), blocks[2].height);
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        let signals = ValidationSignals::default();
        let (mut evo, layer, store) = node_evo_on(&p, bls(&p), Ev1NotBuilt, &signals);
        let mut list = MnList::default_pre_dip3();
        list.try_add(numbered(0), true).expect("adds");
        let height = i32::try_from(height3).expect("a small height");
        let list = list.with_block_hash(h3).with_height(height);
        let delta = EvoDelta {
            block: h3,
            height: height3,
            dml: Some(testkit::dml(&MnList::default_pre_dip3(), &list, true)),
            mined_commitments: Vec::new(),
            credit_data: None,
            credit_pool: None,
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: Some(h3),
        };
        let mut scratch = Scratch::over(layer.scratch());
        store.commit(&mut scratch.evo, &delta).map_err(|e| Fatal::new(e.to_string()))?;
        let at3 = at(&blocks[2], a3, a2);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            // The active layer first: a read through the scratch one offers its list to the
            // hooks' list cache.
            let facade = attempt_facade(cx.params);
            let (id, origin) = (AttemptId::next(), AttemptOrigin::Tip);
            let mut att = evo.begin_attempt(&facade, vcx, cx, id, origin, &ViewTarget::Active)?;
            let mut target = EvoTarget::Active;
            evo.bls_ctx(&mut att, &mut target, cx.params, &at3);
            let active = {
                let (binding, _) = evo.bind_attempt(&mut att, &mut target)?;
                binding.with_llmq(|_, src| src.mn_list(a3).map(|l| l.map(|l| l.height())))
            };
            evo.finish_attempt(att)?;

            let facade = attempt_facade(cx.params);
            let (id, origin) = (AttemptId::next(), AttemptOrigin::VerifyDb);
            let view = ViewTarget::Scratch(&mut scratch);
            let mut att = evo.begin_attempt(&facade, vcx, cx, id, origin, &view)?;
            let mut target = EvoTarget::Scratch(&mut scratch.evo);
            evo.bls_ctx(&mut att, &mut target, cx.params, &at3);
            let read = {
                let (binding, _) = evo.bind_attempt(&mut att, &mut target)?;
                binding.with_llmq(|_, src| {
                    src.mn_list(a3).map(|l| l.map(|l| (*l.block_hash(), l.height())))
                })
            };
            evo.finish_attempt(att)?;
            Ok((active.map_err(|e| e.to_string()), read.map_err(|e| e.to_string())))
        })?;
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    let (active, read) = out.lock().expect("out").take().expect("V ran it");
    assert!(active.as_ref().is_err_and(|e| e.contains("no mn_diff row")), "{active:?}");
    let height = i32::try_from(height3).expect("a small height");
    assert_eq!(read, Ok(Some((h3, height))));
    rig.assert_no_abort();
}
