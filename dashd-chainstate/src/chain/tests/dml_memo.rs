//! MN1-5's exact-block memo (A1 §2.13; PLAN-MN1 §3.6) through V's `TestBlockValidity` over the
//! node's connector and `NodeEvo`, on dashd's recorded masternode chain (`tests/data/ch7a5`).
//! Each memo is the template's: the list build of the block over V's tip coins and the template's
//! members in one query turn, made only when that build stored nothing. A hit answers as the
//! build does, with the same applied effects and, once the block connects, the same state; a
//! memo of another block, order, parent or tip stands for nothing.

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_consensus::ctx::{Deployment, DeploymentPos, DeploymentView as _, bls_ctx_for_block};
use dash_consensus::primitives::TxRef;
use dash_consensus::{Block, Tx};
use dash_dml::{
    BuiltList, DmlMemo, DmlMemoCounts, DmlMemoKey, DmlMemoTally, DmlParams, DmlRules, NoTrace,
    PrevBlock, build_new_list_from_block,
};
use dash_params::BuriedDeployment;
use dash_primitives::{BlockHash, Transaction, TxType};

use super::evo_chain::{
    EffectProbe, Kept, MN_CHAIN, MOCKTIME, OpenSeams, QC_CHAIN, StoredRow, decode, linked,
    node_config_with, qc_chain_params, recorded_config_on_probe, recorded_params,
};
use super::read_faults::Fault;
use super::support::{Mined, Rig, RigConfig};
use crate::chain::{BlockState, NotTested};
use crate::connect::NodeHelper;
use crate::evo::cl::AttemptClVerifier;
use crate::evo::codec::cf;
use crate::evo::ev1::DmlChecks;
use crate::evo::node::node_net_info;

/// The EvoNode's registration (internal collateral: created in its own block).
const EVO_REG: u32 = 116;
/// The regular masternode's registration (internal collateral).
const REG: u32 = 117;
/// A ProUpRegTx after three null commitments of `llmq_test`'s window.
const UP_REG: u32 = 161;
/// Two blocks of a coinbase each, outside any mining window: their keys differ by the parent only.
const BARE: u32 = 142;
/// Tips on setup-dashd's network whose next block's build stores no snapshot row: 229 mines a
/// signed `llmq_test_dip0024` commitment, 251 one of each type.
const QC_TIPS: [u32; 2] = [228, 250];
/// The regular masternode's collateral spent by an ordinary transaction.
const SPEND: u32 = 201;

/// The recorded chains: dashd's masternode chain (`ch7a5`), and setup-dashd's masternode
/// network (`mn0-qc`), whose `llmq_test_dip0024` rotates.
#[derive(Clone, Copy)]
enum Chain {
    Mn,
    Qc,
}

/// A recorded rig at `tip`, its probe, stores and the recorded chain; `cold` restarts it on its
/// flushed stores with the next block's inputs out of the coins cache, so V's member and snapshot
/// caches start empty.
struct Run {
    rig: Rig,
    kept: Kept,
    probe: EffectProbe,
    main: Vec<Mined>,
}

fn seams(probe: &EffectProbe) -> OpenSeams<DmlChecks> {
    OpenSeams {
        ev1: DmlChecks,
        cl: Box::new(AttemptClVerifier::default()),
        helper: Box::new(NodeHelper),
        blocks: Box::new(|b| b),
        bls_checks: Arc::new(dash_evo::llmq::SerialBlsChecks),
        script_threads: 0,
        effect_probe: Some(probe.clone()),
    }
}

impl Run {
    fn at(tip: u32, cold: bool) -> Run {
        Run::on(Chain::Mn, tip, cold)
    }

    fn on(chain: Chain, tip: u32, cold: bool) -> Run {
        let probe = EffectProbe::default();
        let cfg = RigConfig::default();
        let (cfg, kept, blocks) = match chain {
            Chain::Mn => {
                let p = recorded_params();
                let probed = Some(probe.clone());
                let (cfg, kept) = recorded_config_on_probe(cfg, p, MOCKTIME, None, probed);
                (cfg, kept, decode(MN_CHAIN))
            }
            Chain::Qc => {
                let p = qc_chain_params();
                let blocks = decode(QC_CHAIN);
                let last = blocks.iter().map(|b| i64::from(b.header().time())).max();
                let after_genesis = last.expect("blocks") - i64::from(Mined::genesis(&p).time());
                let (cfg, kept) = node_config_with(p, after_genesis + 86_400, cfg, seams(&probe));
                (cfg, kept, blocks)
            }
        };
        let mut rig = Rig::with(cfg);
        let main = linked(&rig.genesis, blocks);
        for m in &main[..tip as usize] {
            rig.accept(m);
        }
        if cold {
            let at = rig.id(&main[tip as usize - 1]);
            let exit = rig.stop();
            assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
            kept.flush_evo();
            kept.flush_coins();
            let mut cfg = kept.reopen(RigConfig::default(), tip + 1, seams(&probe));
            let files = cfg.files.take().expect("the node's block files");
            kept.uncache_inputs(&main[tip as usize].block);
            rig = Rig::restart_with(exit, files, at, cfg).expect("V starts");
            kept.assert_no_abort(&rig);
        }
        probe.snapshots.lock().expect("snapshots").clear();
        probe.bans.lock().expect("bans").clear();
        probe.notices.lock().expect("notices").clear();
        Run {
            rig,
            kept,
            probe,
            main,
        }
    }

    fn block(&self, height: u32) -> Arc<Block> {
        Arc::clone(&self.main[height as usize - 1].block)
    }

    /// The template's list build of `block` over V's tip, as `CreateNewBlock`'s builder makes
    /// it (one query turn: V's tip list and coins, the template's members), with the effects it
    /// emitted and the snapshot rows it stored, and the block's key under the build's rules.
    fn template(&self, block: &Arc<Block>) -> (BuiltList, usize, DmlMemoKey) {
        self.template_with(block, false)
    }

    /// [`Run::template`] under the NetInfo rules of `-cjdnsreachable=cjdns_reachable` (the
    /// node's are without it).
    fn template_with(
        &self,
        block: &Arc<Block>,
        cjdns_reachable: bool,
    ) -> (BuiltList, usize, DmlMemoKey) {
        let p = Arc::clone(&self.rig.p);
        let block = Arc::clone(block);
        let built = self.rig.v.view_handle().query(move |q| {
            let (tip, _) = q.chain.private_tip().expect("a tip");
            let prev = Some(tip.id);
            let after = |d| q.deployments.active_after(prev, Deployment::Buried(d));
            let v24 = q.deployments.active_after(prev, Deployment::Versionbits(DeploymentPos::V24));
            let view = q.evo_view();
            let list = view.tip_list().expect("the tip list");
            let dml = DmlParams::new(&p);
            let net_info = node_net_info(&p, cjdns_reachable).expect("regtest's NetInfo rules");
            let rules = DmlRules {
                params: &dml,
                net_info: &net_info,
                v19: after(BuriedDeployment::V19),
                mn_rr: after(BuriedDeployment::MnRr),
                v24,
            };
            let prev_block = PrevBlock {
                id: tip.id,
                hash: tip.hash,
                height: i32::try_from(tip.height).expect("a height"),
                chain: q.chain.complete().expect("a complete chain"),
            };
            let bls = bls_ctx_for_block(&p, tip.height + 1);
            let ((built, emitted), stored) = view
                .with_tip_coins(|coins| {
                    view.with_template_counted(bls, |qc, _| {
                        let mut effects = Vec::new();
                        let built = build_new_list_from_block(
                            &block,
                            prev_block,
                            &list,
                            coins,
                            qc.members,
                            &rules,
                            &mut effects,
                            &mut NoTrace,
                        );
                        (built.map_err(|e| e.to_string()), effects.len())
                    })
                })
                .expect("the coins cell")
                .expect("a node with coins")
                .expect("the template's LLMQ reads");
            let key = DmlMemoKey::of(&block, &rules).expect("a coinbase first");
            (built, emitted + usize::try_from(stored).expect("a count"), key)
        });
        let (built, effects, key) = built.recv().expect("V serves");
        (built.expect("the template's build"), effects, key)
    }

    /// The template's memo of `block`, counted on `counts`: it stored nothing.
    fn memo(&self, block: &Arc<Block>, counts: &Arc<DmlMemoCounts>) -> DmlMemo {
        let (built, effects, key) = self.template(block);
        assert_eq!(effects, 0, "a memo only of a build with no effect");
        DmlMemo::new(key, built, Arc::clone(counts))
    }

    /// `CreateNewBlock`'s `TestBlockValidity` of `block`, PoW and merkle off.
    fn test(&self, block: &Arc<Block>, memo: Option<DmlMemo>) -> Result<BlockState, NotTested> {
        let v = &self.rig.v;
        v.test_block_validity_memo(Arc::clone(block), false, false, memo.map(Arc::new))
            .recv()
            .expect("V replies")
    }

    /// The snapshot rows, platform-ban resets and list notices applied so far.
    fn effects(&self) -> Applied {
        let p = &self.probe;
        let snapshots = p.snapshots.lock().expect("snapshots").clone();
        let bans = p.bans.lock().expect("bans").clone();
        (snapshots, bans, p.notices.lock().expect("notices").clone())
    }

    /// The engine's rows and the evo layer's after its flush, and V's tip list hash.
    fn state(&self) -> (BTreeSet<StoredRow>, String, dash_num::Hash256) {
        self.rig.sync();
        let list = self.rig.v.view_handle().query(|q| q.evo_view().tip_list().expect("reads"));
        let list = dash_dml::codec::canonical_mn_list_hash(&list.recv().expect("V serves"));
        self.kept.flush_evo();
        let (rows, layer) = self.kept.stored();
        (rows, layer, list)
    }
}

/// [`Run::effects`].
type Applied = (Vec<BlockHash>, Vec<(dash_dml::ProTxHash, i32)>, Vec<(BlockHash, bool)>);

fn counts() -> Arc<DmlMemoCounts> {
    Arc::new(DmlMemoCounts::default())
}

fn tally(hits: u64, key: u64, tip: u64) -> DmlMemoTally {
    DmlMemoTally {
        hits,
        key,
        tip,
        effects: 0,
    }
}

/// `block` with `txs`.
fn with_txs(block: &Block, txs: Vec<TxRef>) -> Arc<Block> {
    Arc::new(Block::new(*block.header(), txs))
}

fn coinbase_only(block: &Block) -> Arc<Block> {
    with_txs(block, vec![Arc::clone(&block.txs()[0])])
}

/// 161 with its ProUpRegTx first and its three commitments in reverse.
fn reordered(block: &Block) -> Arc<Block> {
    let txs = block.txs();
    assert_eq!(txs[4].tx_type(), TxType::ProviderUpdateRegistrar);
    let order = [0, 4, 3, 2, 1];
    with_txs(block, order.iter().map(|&i| Arc::clone(&txs[i])).collect())
}

/// `tx` with the last byte of its payload, its signature's, flipped.
fn bad_sig(tx: &Tx) -> TxRef {
    let raw = tx.raw();
    let mut payload = raw.extra_payload().clone();
    *payload.last_mut().expect("a payload") ^= 1;
    Arc::new(Tx::new(Transaction::new(
        raw.version(),
        raw.tx_type(),
        raw.inputs().clone(),
        raw.outputs().clone(),
        raw.lock_time(),
        payload,
    )))
}

/// One rig answers `block` at `tip` with the template's memo (cross-checked, as every memo is
/// unless a test opts out: the hit's list and its build's are compared, and the build must emit
/// nothing), another without: the same
/// answer and applied effects, one hit; and when `connect`, the same state once each connects
/// the block.
fn hit_answers_as_the_build(
    chain: Chain,
    tip: u32,
    block: impl Fn(&Run) -> Arc<Block>,
    cold: bool,
) {
    let with = Run::on(chain, tip, cold);
    let without = Run::on(chain, tip, cold);
    let b = block(&with);
    let n = counts();
    let memo = with.memo(&b, &n);
    let answer = with.test(&b, Some(memo));
    assert_eq!(n.tally(), tally(1, 0, 0), "{tip}: one hit");
    assert_eq!(answer, without.test(&b, None), "{tip}: the answer");
    assert_eq!(with.effects(), without.effects(), "{tip}: the applied effects");
    if Arc::ptr_eq(&b, &with.block(tip + 1)) {
        assert_eq!(answer, Ok(BlockState::Valid), "{tip}: dashd's block");
        with.rig.accept(&with.main[tip as usize]);
        without.rig.accept(&without.main[tip as usize]);
        assert_eq!(with.effects(), without.effects(), "{tip}: the connect's effects");
        assert!(with.state() == without.state(), "{tip}: the state after the connect");
    }
    with.kept.assert_no_abort(&with.rig);
    without.kept.assert_no_abort(&without.rig);
}

/// A hit answers as the build over dashd's template-shaped blocks: a registration whose
/// collateral the same block creates (an EvoNode's, then a regular one's), a ProUpRegTx after
/// a window's commitments, and the ordinary spend of a masternode's collateral.
#[test]
fn a_hit_answers_as_the_build_over_dashds_blocks() {
    for tip in [EVO_REG - 1, REG - 1, UP_REG - 1, SPEND - 1] {
        hit_answers_as_the_build(Chain::Mn, tip, |r| r.block(tip + 1), false);
    }
}

/// A hit answers as the build over setup-dashd's blocks with signed commitments, warm and cold:
/// a rotating type's (229), and both types' (251), whose cycle snapshots are already stored.
#[test]
fn a_hit_answers_as_the_build_over_signed_commitments() {
    for tip in QC_TIPS {
        for cold in [false, true] {
            hit_answers_as_the_build(Chain::Qc, tip, |r| r.block(tip + 1), cold);
        }
    }
}

/// The effects condition's count: where the template's build of a block with a rotating
/// commitment stores its cycle's snapshot rows (228, 300), it counts them, so no memo is made.
#[test]
fn a_template_build_that_stores_a_snapshot_counts_it() {
    for tip in [227, 299] {
        let r = Run::on(Chain::Qc, tip, true);
        let (_, effects, _) = r.template(&r.block(tip + 1));
        assert!(effects > 0, "{tip}: the snapshot rows stored");
    }
    for tip in QC_TIPS {
        let r = Run::on(Chain::Qc, tip, true);
        assert_eq!(r.template(&r.block(tip + 1)).1, 0, "{tip}: none stored");
    }
}

/// The same with the ProUpRegTx before the commitments and the commitments reversed: the key
/// is the new order's, and the answer is the build's.
#[test]
fn a_hit_answers_as_the_build_over_reordered_provider_and_commitment_txs() {
    hit_answers_as_the_build(Chain::Mn, UP_REG - 1, |r| reordered(&r.block(UP_REG)), false);
}

/// With V's member and snapshot caches cold before the turn, a hit answers as the build, and
/// connecting the block then leaves the same state.
#[test]
fn a_cold_hit_then_the_connect_leave_the_same_state() {
    for tip in [EVO_REG - 1, UP_REG - 1, SPEND - 1] {
        hit_answers_as_the_build(Chain::Mn, tip, |r| r.block(tip + 1), true);
    }
}

/// A memo stands only for the build of its own block: another block, another order of the same
/// transactions, or the same transactions (here none) on another parent build the list, counted
/// as key misses, and answer as without the memo.
#[test]
fn a_key_mismatch_builds_the_list() {
    let r = Run::at(UP_REG - 1, false);
    let b = r.block(UP_REG);
    let n = counts();
    let coinbase_only = coinbase_only(&b);
    let cases = [(&b, r.memo(&coinbase_only, &n)), (&b, r.memo(&reordered(&b), &n))];
    for (i, (block, memo)) in cases.into_iter().enumerate() {
        let built = r.test(block, None);
        assert_eq!(r.test(block, Some(memo)), built, "case {i}: the build's answer");
        assert_eq!(n.tally(), tally(0, i as u64 + 1, 0), "case {i}: a key miss");
    }
    let r = Run::at(BARE - 1, false);
    let memo = r.memo(&r.block(BARE), &n);
    r.rig.accept(&r.main[BARE as usize - 1]);
    let next = r.block(BARE + 1);
    let built = r.test(&next, None);
    assert_eq!(built, Ok(BlockState::Valid));
    assert_eq!(r.test(&next, Some(memo)), built, "another parent: the build's answer");
    assert_eq!(n.tally(), tally(0, 3, 0), "another parent: a key miss");
    r.kept.assert_no_abort(&r.rig);
}

/// A memo made before V's tip moved stands for nothing: its block answers `NotBestPrev`
/// before any build, counted as a tip miss, and the same block on the new tip is another key.
#[test]
fn a_moved_tip_answers_not_best_prev() {
    let r = Run::at(UP_REG - 1, false);
    let b = r.block(UP_REG);
    let n = counts();
    let memo = r.memo(&b, &n);
    let key = memo.key.clone();
    r.rig.accept(&r.main[UP_REG as usize - 1]);
    assert_eq!(r.test(&b, Some(memo)), Err(NotTested::NotBestPrev));
    assert_eq!(n.tally(), tally(0, 0, 1), "a tip miss");
    let next = r.block(UP_REG + 1);
    let (built, _, _) = r.template(&next);
    let built_answer = r.test(&next, None);
    let stale = DmlMemo::new(key, built, n.clone());
    assert_eq!(r.test(&next, Some(stale)), built_answer, "the build's answer");
    assert_eq!(n.tally(), tally(0, 1, 1), "then a key miss");
    r.kept.assert_no_abort(&r.rig);
}

/// The memo's list is what a hit uses: a memo of the block whose list is the parent's answers
/// the CbTx root check's verdict, where the build passes (the cross-check, which would stop V
/// first, off).
#[test]
fn a_hit_uses_the_memos_list() {
    let r = Run::at(REG - 1, false);
    let b = r.block(REG);
    let n = counts();
    let (_, _, key) = r.template(&b);
    let (parent, _, _) = r.template(&coinbase_only(&b));
    let memo = DmlMemo::new(key, parent, n.clone()).without_cross_check();
    let answer = r.test(&b, Some(memo));
    let reason = match &answer {
        Ok(BlockState::Invalid(e)) => e.reject_reason().to_owned(),
        other => panic!("{other:?}"),
    };
    assert_eq!(reason, "bad-cbtx-mnmerkleroot");
    assert_eq!(n.tally(), tally(1, 0, 0));
    assert_eq!(r.test(&b, None), Ok(BlockState::Valid));
    r.kept.assert_no_abort(&r.rig);
}

/// test-support's cross-check, on for every memo made, builds the list on each hit: a memo whose
/// list is not the build's stops V on the hit.
#[test]
fn the_cross_check_stops_a_hit_whose_list_is_not_the_build() {
    let mut r = Run::at(REG - 1, false);
    let b = r.block(REG);
    let n = counts();
    let (_, _, key) = r.template(&b);
    let (parent, _, _) = r.template(&coinbase_only(&b));
    let memo = DmlMemo::new(key, parent, n.clone());
    let v = &r.rig.v;
    let answer = v.test_block_validity_memo(b, false, false, Some(Arc::new(memo))).recv();
    assert!(answer.is_err(), "no answer: {answer:?}");
    let panic = r.rig.join_panicked();
    assert!(panic.contains("a memo hit's list differs from its build"), "{panic}");
}

/// A memo made under other rules than `NodeEvo`'s stands for nothing: the template's NetInfo
/// rules with `-cjdnsreachable`, where the node's are without it, build the list, counted as a
/// key miss.
#[test]
fn a_memo_under_other_net_info_rules_builds_the_list() {
    let r = Run::at(REG - 1, false);
    let b = r.block(REG);
    let n = counts();
    let (built, effects, key) = r.template_with(&b, true);
    assert_eq!(effects, 0, "a memo only of a build with no effect");
    let answer = r.test(&b, Some(DmlMemo::new(key, built, n.clone())));
    assert_eq!(answer, r.test(&b, None), "the build's answer");
    assert_eq!(n.tally(), tally(0, 1, 0), "a key miss");
    r.kept.assert_no_abort(&r.rig);
}

/// A hit stands for the build only: the special-tx checks before it still run, so a ProUpRegTx
/// whose signature fails is refused with or without the memo of its block.
#[test]
fn a_hit_still_runs_the_special_tx_checks() {
    let r = Run::at(UP_REG - 1, false);
    let b = r.block(UP_REG);
    let mut txs = b.txs().to_vec();
    txs[4] = bad_sig(&txs[4]);
    let forged = with_txs(&b, txs);
    let n = counts();
    let memo = r.memo(&forged, &n);
    let answer = r.test(&forged, Some(memo));
    assert!(matches!(&answer, Ok(BlockState::Invalid(_))), "{answer:?}");
    assert_eq!(answer, r.test(&forged, None), "the build's answer");
    r.kept.assert_no_abort(&r.rig);
}

/// A store fault in a stage after the build surfaces on a hit through the path it takes
/// without the memo: the same answer, reports, latch and reads. The fault is on the mined
/// commitment row the attempt first reads last (the CbTx quorum root's, step 9), cold.
#[test]
fn a_store_fault_on_a_hit_surfaces_as_without_the_memo() {
    let tip = QC_TIPS[1];
    let r = Run::on(Chain::Qc, tip, true);
    r.kept.faults().watch(cf("llmq_mined"), &[]);
    assert_eq!(r.test(&r.block(tip + 1), None), Ok(BlockState::Valid));
    let mut keys = r.kept.faults().read_keys();
    keys.dedup();
    let last = keys.iter().rev().find(|k| keys.iter().filter(|o| o == k).count() == 1);
    let last = last.expect("a key read once").clone();
    for fault in [Fault::Io, Fault::Garbage] {
        let outcome = |memo: bool| {
            let r = Run::on(Chain::Qc, tip, true);
            let b = r.block(tip + 1);
            let n = counts();
            let memo = memo.then(|| Arc::new(r.memo(&b, &n)));
            r.kept.faults().arm(cf("llmq_mined"), &last, fault);
            let v = &r.rig.v;
            let answer = v.test_block_validity_memo(b, false, false, memo).recv().ok();
            let hits = r.kept.faults().hits();
            r.kept.faults().disarm();
            let reports = r.kept.reports().len();
            let seen = (answer, reports, r.kept.tripped(), r.kept.shutdown_requested(), hits);
            (seen, n.tally())
        };
        let (with, tallied) = outcome(true);
        let (without, _) = outcome(false);
        assert_eq!(with, without, "{fault:?}");
        assert_eq!(tallied, tally(1, 0, 0), "{fault:?}: on a hit");
        assert!(with.4 > 0 && with.0 != Some(Ok(BlockState::Valid)), "{fault:?}: met: {with:?}");
    }
}

/// `propose_block` passes no memo, and V keeps none across calls: after a hit on a block, the
/// same block proposed is built again.
#[test]
fn propose_block_never_hits() {
    let r = Run::at(REG - 1, false);
    let b = r.block(REG);
    let n = counts();
    let memo = r.memo(&b, &n);
    assert_eq!(r.test(&b, Some(memo)), Ok(BlockState::Valid));
    assert_eq!(n.tally(), tally(1, 0, 0));
    let proposed = r.rig.v.propose_block(Arc::clone(&b)).recv().expect("V replies");
    assert_eq!(proposed, Ok(BlockState::Valid));
    assert_eq!(r.test(&b, None), Ok(BlockState::Valid));
    assert_eq!(n.tally(), tally(1, 0, 0), "no hit after the memo's own call");
    r.kept.assert_no_abort(&r.rig);
}
