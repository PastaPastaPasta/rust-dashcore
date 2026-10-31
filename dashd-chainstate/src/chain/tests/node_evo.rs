//! `NodeEvo` (CH7 §2.1) on a real V, its hooks called as the connector calls them: below
//! DIP0003 it gives `NoEvo`'s verdicts and stages the evo tip (D44, D7), from DIP0003 G1q and its
//! stubs refuse (D3, D2); the transaction slot (D1), the published scheme (D9), H9 below DIP0003,
//! and step 10's arms with no best ChainLock (D45).

use std::cell::Cell;
use std::sync::{Arc, Mutex};

use dash_consensus::coins::{CoinRef, CoinsView};
use dash_consensus::ctx::bls_ctx_for_block;
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::primitives::{
    Header, OutPoint, Transaction, TxIn, TxOut, TxRef, decode_tx_exact, write_commitment,
};
use dash_consensus::tx::TRANSACTION_QUORUM_COMMITMENT;
use dash_consensus::{Block, BlockId, BlsCtx, Tx, TxValidationError};
use dash_dml::blockprocessor::QcCtx;
use dash_dml::blockprocessor::pool::MineablePool;
use dash_dml::netinfo::{NetInfoConfig, NetInfoRules};
use dash_dml::specialtx::QuorumSeams;
use dash_dml::{EvoDelta, MnList, StoreError};
use dash_evo::llmq::{
    BlsSigValue, CompleteChain, LlmqCommitment, LlmqCtx, SerialBlsChecks, enabled_quorum_params,
};
use dash_params::{
    BuriedDeployment, ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release,
};
use dash_primitives::{
    BlockHash, BlockHeader, CoinbaseCommitment, QuorumVvecHash, SpecialPayload, TxHash, TxType,
};
use dashd_store::MemoryEngine;

use super::support::{Mined, Rig, branch, install_accept_log, levels_on, regtest, with_tip_cx};
use crate::chain::{
    AttemptOrigin, ConnectCx, EvoTarget, Fatal, VContext, ValidationSignals, ViewTarget,
};
use crate::connect::{
    AttemptId, BlockAt, EvoHooks, SpecialTxFailure, SpecialTxRules, pre_dip3_special_txs,
};
use crate::evo::cl::{
    CHAINLOCK_VERIFY, ClStep, ClVerifier, NotBuiltClVerifier, check_cbtx_best_chainlock,
};
use crate::evo::effects::{
    EffectAppliers, MemPlatformBans, MnListSignal, PlatformBanApplier, SnapshotApplier,
    StoreSnapshots, UnreachableIsLock, UnreachableMineable,
};
use crate::evo::ev1::{DmlChecks, Ev1Checks, Ev1NotBuilt, SpecialTxCtx};
use crate::evo::node::{EvoBlocks, NodeEvo, NodeEvoParts, SharedEvoLayer};
use crate::evo::not_built::QUORUM_VALIDATION;
use crate::evo::{RocksEvoStore, testkit};
use crate::flush::{EffectLedger, EvoWindow};
use crate::llmq::facade::MembersFacade;
use crate::llmq::rows::RuntimeRows;
use crate::store::{CHAINSTATE_SCHEMA, CoinsReadError};

/// No coin: the checks below DIP0003 read none.
pub(super) struct NoCoins;

impl CoinsView for NoCoins {
    type Error = CoinsReadError;

    fn coin(&self, _: &OutPoint) -> Result<Option<CoinRef<'_>>, CoinsReadError> {
        Ok(None)
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// The hooks under test: `NodeEvo` over [`Recording`].
pub(super) type Evo = NodeEvo<MemoryEngine, Recording>;

/// What H4 handed EV1's checks at a step-4 call: the block's context as H4 built it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    height: u32,
    prev: Option<BlockId>,
    bls: BlsCtx,
    median_time_past: i64,
    check_sigs: bool,
    net_info: NetInfoRules,
}

thread_local! {
    /// The last [`Seen`] on this thread: the hooks and the test's assertions both run on V.
    static SEEN: Cell<Option<Seen>> = const { Cell::new(None) };
}

/// [`Ev1NotBuilt`], recording the context of each step-4 call in [`SEEN`].
pub(super) struct Recording;

impl Ev1Checks for Recording {
    type Payloads<'b> = ();

    fn payloads(&self, block: &Block) {
        Ev1NotBuilt.payloads(block);
    }

    fn check_cbtx(
        &self,
        block: &Block,
        (): &(),
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal> {
        Ev1NotBuilt.check_cbtx(block, &(), cx)
    }

    fn check_tx<'a>(
        &self,
        block: &Block,
        (): &(),
        i: usize,
        cx: SpecialTxCtx<'a>,
        quorum: Option<QuorumSeams<'a>>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        let cons = cx.cons;
        SEEN.set(Some(Seen {
            height: cons.height,
            prev: cons.prev,
            bls: cons.bls,
            median_time_past: cons.median_time_past,
            check_sigs: cx.check_sigs,
            net_info: cx.net_info,
        }));
        Ev1NotBuilt.check_tx(block, &(), i, cx, quorum)
    }

    fn shared_collateral_spends(
        &self,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        Ev1NotBuilt.shared_collateral_spends(tx, view)
    }
}

/// No block data: step 10's parent read fails, as an unread block's does.
struct NoBlocks;

impl EvoBlocks for NoBlocks {
    fn read(&self, _: &dash_chain::ChainReader, _: BlockId) -> Option<Arc<Block>> {
        None
    }
}

/// The hooks' NetInfo rules: `p`'s, without `-cjdnsreachable`.
fn net_info(p: &ChainParams) -> NetInfoRules {
    let mainnet =
        ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().expect("mainnet");
    NetInfoRules::new(
        p,
        &mainnet,
        NetInfoConfig {
            cjdns_reachable: false,
        },
    )
}

fn node_evo(p: &Arc<ChainParams>, scheme: BlsCtx) -> (Evo, SharedEvoLayer, Store) {
    node_evo_with(p, scheme, Recording)
}

/// [`node_evo`] over `ev1`.
fn node_evo_with<C: Ev1Checks>(
    p: &Arc<ChainParams>,
    scheme: BlsCtx,
    ev1: C,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    node_evo_on(p, scheme, ev1, &ValidationSignals::default())
}

/// [`node_evo_with`], raising the MN-list signal on `signals`.
pub(super) fn node_evo_on<C: Ev1Checks>(
    p: &Arc<ChainParams>,
    scheme: BlsCtx,
    ev1: C,
    signals: &ValidationSignals,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    build_node_evo(p, scheme, ev1, Box::new(MemPlatformBans::default()), signals, None)
}

/// [`node_evo`] with `platform_ban` as its platform-ban applier.
pub(super) fn node_evo_with_bans(
    p: &Arc<ChainParams>,
    scheme: BlsCtx,
    platform_ban: Box<dyn PlatformBanApplier>,
) -> (Evo, SharedEvoLayer, Store) {
    build_node_evo(p, scheme, Recording, platform_ban, &ValidationSignals::default(), None)
}

/// [`node_evo`] with `snapshot` as its snapshot applier.
pub(super) fn node_evo_with_snapshots(
    p: &Arc<ChainParams>,
    scheme: BlsCtx,
    snapshot: Box<dyn SnapshotApplier>,
) -> (Evo, SharedEvoLayer, Store) {
    let bans = Box::new(MemPlatformBans::default());
    build_node_evo(p, scheme, Recording, bans, &ValidationSignals::default(), Some(snapshot))
}

/// [`node_evo`] over `ev1`, with `platform_ban` as its platform-ban applier, raising the MN-list
/// signal on `signals`, and `snapshot`, if any, as its snapshot applier.
fn build_node_evo<C: Ev1Checks>(
    p: &Arc<ChainParams>,
    scheme: BlsCtx,
    ev1: C,
    platform_ban: Box<dyn PlatformBanApplier>,
    signals: &ValidationSignals,
    snapshot: Option<Box<dyn SnapshotApplier>>,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    let mn_list = MnListSignal::new(signals.clone());
    let genesis = Mined::genesis(p).hash();
    let engine = crate::evo::testkit::guarded(MemoryEngine::new(&CHAINSTATE_SCHEMA));
    let store = Arc::new(RocksEvoStore::new(engine, crate::evo::testkit::codec(), genesis));
    let layer = SharedEvoLayer::new(store.resume_layer().expect("a fresh meta row"));
    let appliers = EffectAppliers {
        snapshot: snapshot.unwrap_or_else(|| {
            Box::new(StoreSnapshots {
                rows: Arc::new(RuntimeRows::new(store.clone(), Arc::new(EffectLedger::default()))),
            })
        }),
        is_lock: Box::new(UnreachableIsLock),
        mineable: Box::new(UnreachableMineable),
        platform_ban,
        notify: Box::new(mn_list.applier()),
    };
    let parts = NodeEvoParts {
        store: store.clone(),
        layer: layer.clone(),
        appliers,
        params: p.clone(),
        net_info: net_info(p),
        ev1,
        cl: Box::new(NotBuiltClVerifier),
        blocks: Box::new(NoBlocks),
        scheme,
        mn_list,
    };
    (NodeEvo::new(parts).expect("regtest's payment parameters"), layer, store)
}

/// A members facade for one attempt of the hooks under test: they ask it for no members, so
/// each attempt opens its own (EV addendum §2.6).
pub(super) fn attempt_facade(p: &ChainParams) -> Arc<MembersFacade> {
    super::llmq_members::world(p).facade
}

/// The hooks' evo store.
pub(super) type Store = Arc<RocksEvoStore<MemoryEngine>>;

/// H4's answer, with whether it left list updates.
type H4 = Result<Result<bool, SpecialTxFailure>, Fatal>;

/// One Active connect attempt in the connector's order (CH3 §3.1): H10, H1, H4, and on success
/// H6, H1's commit and H10's commit when `commit`, else the rollback.
fn connect<C: Ev1Checks + Send>(
    evo: &mut NodeEvo<MemoryEngine, C>,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    check: bool,
    commit: bool,
) -> Result<H4, Fatal> {
    let mut target = EvoTarget::Active;
    let facade = attempt_facade(cx.params);
    let mut att = evo.begin_attempt(
        &facade,
        vcx,
        cx,
        AttemptId::next(),
        AttemptOrigin::Tip,
        &ViewTarget::Active,
    )?;
    evo.begin_tx(&mut att, &mut target)?;
    evo.bls_ctx(&mut att, &mut target, cx.params, at);
    let h4 = evo.process_special_txs(
        &mut att,
        &mut target,
        block,
        at,
        SpecialTxRules::default(),
        &NoCoins,
        0,
        false,
        check,
    );
    let commit = commit && matches!(h4, Ok(Ok(_)));
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
    Ok(h4.map(|r| r.map(|updates| updates.is_some())))
}

fn tx(version: i16, tx_type: u16, coinbase: bool, n: u8) -> TxRef {
    let prevout = OutPoint {
        hash: TxHash::from(
            [if coinbase {
                0
            } else {
                n
            }; 32],
        ),
        index: if coinbase {
            u32::MAX
        } else {
            0
        },
    };
    Arc::new(Tx::new(Transaction {
        version,
        tx_type: TxType::new(tx_type),
        inputs: vec![TxIn {
            prevout,
            script_sig: vec![1, n].into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 1,
            script_pubkey: vec![0x51].into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

/// The bodies whose verdicts differ below DIP0003 (as `ev1`'s test): classical, a raw-type-5
/// coinbase at versions 1 and 2, a raw-type-5 `vtx[0]` that is no coinbase, special
/// transactions after the coinbase, and version-3 coinbases of types 1 and 8.
pub(super) fn bodies() -> Vec<Block> {
    let cb = tx(1, 0, true, 0);
    [
        vec![cb.clone(), tx(2, 0, false, 1)],
        vec![tx(1, TRANSACTION_COINBASE, true, 0)],
        vec![tx(2, TRANSACTION_COINBASE, true, 0)],
        vec![tx(2, TRANSACTION_COINBASE, false, 2)],
        vec![cb.clone(), tx(3, 1, false, 3)],
        vec![cb, tx(2, 1, false, 4), tx(3, 8, false, 5)],
        vec![tx(3, 1, true, 0)],
        vec![tx(3, 8, true, 0)],
    ]
    .into_iter()
    .map(|txs| Block::new(Header::new(BlockHeader::default()), txs))
    .collect()
}

pub(super) fn at(m: &Mined, id: BlockId, prev: BlockId) -> BlockAt {
    BlockAt {
        hash: m.hash(),
        height: m.height,
        prev: Some(prev),
        id: Some(id),
    }
}

/// D44 through NodeEvo (DEC-581): below DIP0003 every body gets `NoEvo`'s verdict with and
/// without the CbTx checks, never a refusal, and a committed attempt stages its block as the evo
/// tip (D7) with nothing in its log. DIP0003 (432) is a cycle base of regtest's rotated type
/// (DIP0024 from height 1), so G1q's arm (c) refuses it before step 1, with and without the CbTx
/// checks (D3's declared preemption).
#[test]
fn node_evo_is_no_evo_below_dip3_and_refuses_from_it() {
    let p = regtest();
    let dip3 = p.consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, dip3, 1);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let gid = rig.id(&genesis);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, _) = node_evo(&p, BlsCtx::BASIC);
        let rules = net_info(&p);
        let result = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut prev = gid;
            let mut best = Vec::new();
            let mut seen = 0;
            for (n, (m, &id)) in blocks.iter().zip(&ids).enumerate() {
                let at = at(m, id, prev);
                if m.height < dip3 {
                    // A few heights through the whole range keep the test quick.
                    if n < 3 || m.height + 2 >= dip3 || m.height % 97 == 0 {
                        // The context H4 builds for EV1's checks: the block's height and parent,
                        // H1's scheme, the parent's median time, `fCheckCbTxMerkleRoots`, the
                        // hooks' NetInfo rules.
                        let context = |check| Seen {
                            height: m.height,
                            prev: Some(prev),
                            bls: bls_ctx_for_block(&p, m.height),
                            median_time_past: crate::chain::index::entry(cx.reader, prev).mtp,
                            check_sigs: check,
                            net_info: rules,
                        };
                        for body in bodies() {
                            for check in [false, true] {
                                let want = Ok(pre_dip3_special_txs(&body, check));
                                SEEN.set(None);
                                let got = connect(&mut evo, vcx, cx, &body, &at, check, false)?;
                                assert_eq!(got.map(|r| r.map(drop)), want, "height {}", m.height);
                                if let Some(got) = SEEN.take() {
                                    assert_eq!(got, context(check), "height {}", m.height);
                                    seen += 1;
                                }
                            }
                        }
                    }
                    // A proposal (no index entry) below DIP0003: MNHF is skipped, as its one
                    // write `fJustCheck` skips and no transaction reaches its checks (D51).
                    let proposal = BlockAt {
                        id: None,
                        ..at
                    };
                    let checked = connect(&mut evo, vcx, cx, &m.block, &proposal, true, false)?;
                    assert_eq!(checked, Ok(Ok(false)), "a proposal at {}", m.height);
                    assert_eq!(
                        connect(&mut evo, vcx, cx, &m.block, &at, true, true)?,
                        Ok(Ok(false))
                    );
                    best.push(layer.best_block() == Some(m.hash()));
                } else {
                    for check in [false, true] {
                        let refused = connect(&mut evo, vcx, cx, &m.block, &at, check, true)?;
                        assert_eq!(refused, Ok(Err(SpecialTxFailure::NotReady(QUORUM_VALIDATION))));
                    }
                    // Neither refusal moved the evo tip.
                    assert_eq!(layer.best_block(), Some(blocks[m.height as usize - 2].hash()));
                }
                prev = id;
            }
            assert!(seen > 0, "step 4 ran");
            Ok(best)
        })?;
        *seen.lock().expect("seen") = Some(result);
        Ok(())
    });
    let best = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(best.len(), dip3 as usize - 1);
    assert!(best.iter().all(|b| *b), "each commit staged its block");
    rig.assert_no_abort();
}

/// A coinbase with type bits 5 at version 3 carrying a CbTx of `version` at `height` with the
/// given roots (`testkit::cbtx_bytes`).
fn special_cbtx(version: u16, height: u32, mn_root: u8, quorum_root: u8) -> TxRef {
    let mut cb = tx(3, TRANSACTION_COINBASE, true, 0).to_mutable();
    cb.extra_payload = testkit::cbtx_bytes(version, height, mn_root, quorum_root);
    Arc::new(Tx::new(cb))
}

/// D57 (DEC-730, BL-516): below DIP0003 `NodeEvo` over `DmlChecks` judges a special-version CbTx
/// as Core's `ProcessSpecialTxsInBlock` does (`stm:899-1053`): step 1, then steps 8–10 over
/// Core's default, empty `mn_list` and no mined commitment, so both roots must be zero (the
/// empty SML's root is zero, quirk 17), the MN root before the quorum root, which version 1
/// lacks; step 4's rejects come first; without `fCheckCbTxMerkleRoots` none of it runs. A
/// judged block commits as any other, the published scheme is the committed one (D9), and H9
/// undoes it reading no evo state.
#[test]
fn node_evo_judges_a_special_version_cbtx_below_dip3() {
    let p = regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 2, 3);
    let (block1, genesis_hash) = (blocks[0].hash(), genesis.hash());
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let gid = rig.id(&genesis);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let genesis = genesis_hash;
        let (mut evo, layer, _) = node_evo_with(&p, BlsCtx::LEGACY, DmlChecks);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let (at1, at2) = (at(&blocks[0], ids[0], gid), at(&blocks[1], ids[1], ids[0]));
            let body = |txs: Vec<TxRef>| Block::new(Header::new(BlockHeader::default()), txs);
            let mut verdicts = Vec::new();
            let mut judge = |evo: &mut NodeEvo<MemoryEngine, DmlChecks>,
                             at: &BlockAt,
                             txs: Vec<TxRef>,
                             check: bool|
             -> Result<(), Fatal> {
                let got = connect(evo, vcx, cx, &body(txs), at, check, false)?;
                verdicts.push(match got {
                    Ok(Ok(updates)) => format!("ok {updates}"),
                    Ok(Err(SpecialTxFailure::Invalid(e))) => e.reject_reason().to_owned(),
                    other => format!("{other:?}"),
                });
                Ok(())
            };
            // Block 1: version 1 has no quorum root; block 2: version 2 does.
            judge(&mut evo, &at1, vec![special_cbtx(1, 1, 0, 0)], true)?;
            judge(&mut evo, &at1, vec![special_cbtx(1, 1, 1, 0)], true)?;
            judge(&mut evo, &at2, vec![special_cbtx(2, 2, 0, 0)], true)?;
            judge(&mut evo, &at2, vec![special_cbtx(2, 2, 1, 0)], true)?;
            judge(&mut evo, &at2, vec![special_cbtx(2, 2, 0, 1)], true)?;
            judge(&mut evo, &at2, vec![special_cbtx(2, 2, 1, 1)], true)?;
            judge(&mut evo, &at2, vec![special_cbtx(2, 3, 1, 1)], true)?;
            judge(&mut evo, &at2, vec![special_cbtx(2, 2, 1, 1), tx(3, 6, false, 1)], true)?;
            judge(&mut evo, &at2, vec![special_cbtx(2, 3, 1, 1)], false)?;
            judge(&mut evo, &at2, vec![special_cbtx(1, 2, 0, 0)], false)?;

            let committed = connect(
                &mut evo,
                vcx,
                cx,
                &body(vec![special_cbtx(1, 1, 0, 0)]),
                &at1,
                true,
                true,
            )?;
            let published = (committed, layer.best_block(), evo.published_bls());
            // Undoing it reads no evo state below DIP0003 (`stm:1087-1120`).
            let undone = undo_block(
                &mut evo,
                vcx,
                cx,
                &body(vec![special_cbtx(1, 1, 0, 0)]),
                &at1,
                &genesis,
            )?;
            Ok((verdicts, published, (undone, layer.best_block())))
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let (verdicts, published, undone) = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(
        verdicts,
        [
            "ok false",
            "bad-cbtx-mnmerkleroot",
            "ok false",
            "bad-cbtx-mnmerkleroot",
            "bad-cbtx-quorummerkleroot",
            "bad-cbtx-mnmerkleroot",
            "bad-cbtx-height",
            "bad-tx-type-dip3-inactive",
            "ok false",
            "ok false",
        ]
    );
    assert_eq!(published, (Ok(Ok(false)), Some(block1), Some(BlsCtx::BASIC)));
    assert_eq!(undone, (Ok(true), Some(genesis_hash)));
    rig.assert_no_abort();
}

/// D1: the slot opens at `begin_tx` and closes at `commit_tx` or `rollback_tx`, H6 needs H4's
/// delta, no transaction outlives its attempt, and a transaction with no delta (genesis's) writes
/// nothing. D9: a committed attempt publishes its scheme, a rolled-back one keeps the old.
#[test]
fn the_transaction_slot_and_the_published_scheme() {
    let p = regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 2, 2);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let gid = rig.id(&genesis);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        // As if the tip were genesis before V19's height 1: legacy.
        let (mut evo, layer, _) = node_evo(&p, BlsCtx::LEGACY);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut target = EvoTarget::Active;
            let at1 = at(&blocks[0], ids[0], gid);
            let mut facts = Vec::new();

            let facade = attempt_facade(cx.params);
            let mut att = evo.begin_attempt(
                &facade,
                vcx,
                cx,
                AttemptId::next(),
                AttemptOrigin::Tip,
                &ViewTarget::Active,
            )?;
            // H6 outside the transaction, then before H4.
            facts.push(evo.set_best_block(&mut att, &mut target, &at1.hash).is_err());
            evo.begin_tx(&mut att, &mut target)?;
            facts.push(evo.set_best_block(&mut att, &mut target, &at1.hash).is_err());
            facts.push(evo.begin_tx(&mut att, &mut target).is_err());
            evo.finish_attempt(att)?;
            // The slot is still open: the next attempt refuses until it closes.
            let facade = attempt_facade(cx.params);
            let next = evo.begin_attempt(
                &facade,
                vcx,
                cx,
                AttemptId::next(),
                AttemptOrigin::Tip,
                &ViewTarget::Active,
            );
            facts.push(next.is_err());
            drop(next);
            evo.rollback_tx(&mut target);
            facts.push(evo.commit_tx(&mut target).is_err());

            // A transaction with no delta commits nothing.
            let facade = attempt_facade(cx.params);
            let att = evo.begin_attempt(
                &facade,
                vcx,
                cx,
                AttemptId::next(),
                AttemptOrigin::Tip,
                &ViewTarget::Active,
            )?;
            let mut att = att;
            evo.begin_tx(&mut att, &mut target)?;
            evo.finish_attempt(att)?;
            evo.commit_tx(&mut target)?;
            facts.push(layer.best_block().is_none());

            // H1 enters basic at height 1; a rollback keeps legacy, a commit publishes basic.
            let rolled_back = connect(&mut evo, vcx, cx, &blocks[0].block, &at1, true, false)?;
            facts.push(rolled_back == Ok(Ok(false)) && evo.scheme() == BlsCtx::LEGACY);
            let committed = connect(&mut evo, vcx, cx, &blocks[0].block, &at1, true, true)?;
            facts.push(committed == Ok(Ok(false)));
            facts.push(evo.scheme() == BlsCtx::BASIC);
            facts.push(layer.best_block() == Some(at1.hash));
            Ok(facts)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let facts = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(facts, vec![true; 10]);
    rig.assert_no_abort();
}

/// D9 at step 12 (`stm:1069-1074`): with V19 at height 3, block 2 is processed under the legacy
/// scheme and switches to basic after it. An attempt that does not commit reverts it with the
/// guard's line and keeps legacy (`validation.cpp:164-169`); a committed one publishes basic.
#[test]
fn step_twelve_publishes_basic_only_on_commit() {
    install_accept_log();
    let p = regtest();
    let v19_at_3 = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .arg(ChainOption::TestActivationHeight, "v19@3")
        .build()
        .expect("v19@3");
    let p19 = Arc::new(v19_at_3);
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 2, 2);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, _, _) = node_evo(&p19, BlsCtx::LEGACY);
        let thread = std::thread::current().id();
        let lines = || {
            let count = |line| levels_on(thread, line).len();
            (
                count("CSpecialTxProcessor::ProcessSpecialTxsInBlock -- bls_legacy_scheme=0"),
                count("ScopedBLSLegacyScheme: entered bls_legacy_scheme=0"),
                count("ScopedBLSLegacyScheme: reverted bls_legacy_scheme=1"),
            )
        };
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let at2 = at(&blocks[1], ids[1], ids[0]);
            let mut facts = Vec::new();
            for commit in [false, true] {
                let mut target = EvoTarget::Active;
                let facade = attempt_facade(cx.params);
                let mut att = evo.begin_attempt(
                    &facade,
                    vcx,
                    cx,
                    AttemptId::next(),
                    AttemptOrigin::Tip,
                    &ViewTarget::Active,
                )?;
                evo.begin_tx(&mut att, &mut target)?;
                let bls = evo.bls_ctx(&mut att, &mut target, &p19, &at2);
                let h4 = evo.process_special_txs(
                    &mut att,
                    &mut target,
                    &blocks[1].block,
                    &at2,
                    SpecialTxRules::default(),
                    &NoCoins,
                    0,
                    false,
                    true,
                );
                facts.push(bls == BlsCtx::LEGACY && matches!(h4, Ok(Ok(_))));
                if commit {
                    evo.set_best_block(&mut att, &mut target, &at2.hash)?;
                    evo.commit_bls_scheme(&mut att, &mut target);
                }
                evo.finish_attempt(att)?;
                if commit {
                    evo.commit_tx(&mut target)?;
                } else {
                    evo.rollback_tx(&mut target);
                }
                facts.push(lines() == (1 + usize::from(commit), 0, 1));
                let published = if commit {
                    BlsCtx::BASIC
                } else {
                    BlsCtx::LEGACY
                };
                facts.push(evo.scheme() == published);
            }
            Ok(facts)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let facts = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(facts, vec![true; 6]);
    rig.assert_no_abort();
}

/// Mainnet's first ProUpServTx payload after DIP0003 (8e5bdc5f…, mined at 1,028,257;
/// dash-oracle-sys `contextual-mainnet.json` `proupserv`, provenance there).
const CORE_PROUPSERV_PAYLOAD: &str = "0100301ac037140fe382096b0f632f9f1ea91675fa12f5aeafead8cfb4e24a6e9aed00000000000000000000ffff258b15d1270f1976a9145434aec481a9ec2835c18b6273a49381a3f2b4ba88ac872cc9869de8887fbcbf15a386af74ae259a6fe17142b0f7c7d43dabd87e59311547b4abad19198e76d7da34e3f5d066e534944c3d658061f6a3e6327122748dd72e2a2a4f8a3cc6a7c5bcfeb4243faa01be396f5c892b7f53fbc0ed9effda2a5b177730ef24ccfd70f58345137e0371c7125d86ae05820e6670f1dd656f91e4";

/// SP1c-1 A3 (DEC-782): below DIP0003 Core's `ProcessBlock` does not run
/// (`specialtxman.cpp:973`, `deterministicmns.cpp:696-698`), so a block holding a ProUpServTx
/// (Core's payload) resets no platform ban, with and without the checks, as a proposal
/// (`fJustCheck`) or connected: its verdict is `NoEvo`'s, and the applier is never reached.
#[test]
fn no_platform_ban_reset_below_dip3() {
    let p = regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 3, 4);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let gid = rig.id(&genesis);
    let upserv = Arc::new(Tx::new(Transaction {
        version: 3,
        tx_type: TxType::ProviderUpdateService,
        inputs: vec![TxIn {
            prevout: OutPoint {
                hash: TxHash::from([9; 32]),
                index: 0,
            },
            script_sig: vec![1, 9].into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 1,
            script_pubkey: vec![0x51].into(),
        }],
        lock_time: 0,
        extra_payload: unhex(CORE_PROUPSERV_PAYLOAD),
    }));
    let decoded = upserv.payload().expect("a payload field").expect("Core's payload decodes");
    assert!(decoded.is_complete());
    let body = Block::new(Header::new(BlockHeader::default()), vec![tx(1, 0, true, 0), upserv]);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let resets = MemPlatformBans::default();
        let (mut evo, _, _) = node_evo_with_bans(&p, BlsCtx::BASIC, Box::new(resets.clone()));
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut prev = gid;
            let mut verdicts = Vec::new();
            for (m, &id) in blocks.iter().zip(&ids) {
                let at = at(m, id, prev);
                let proposal = BlockAt {
                    id: None,
                    ..at
                };
                for (at, check) in [(&at, false), (&at, true), (&proposal, true)] {
                    let want = Ok(pre_dip3_special_txs(&body, check));
                    let got = connect(&mut evo, vcx, cx, &body, at, check, true)?;
                    verdicts.push(got.map(|r| r.map(drop)) == want);
                }
                prev = id;
            }
            Ok(verdicts)
        })?;
        assert!(resets.resets().is_empty(), "no reset below DIP0003");
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let verdicts = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(verdicts, [true; 9]);
    rig.assert_no_abort();
}

/// One Active undo attempt of `blocks[n]` in the connector's order (CH3 §3.1): H10, H1, H9, and
/// when H9 undid it H6 back at the parent, H1's commit and H10's commit, else the rollback.
/// Whether H9 undid the block, or its `Fatal`.
fn undo(
    evo: &mut Evo,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    blocks: &[Mined],
    ids: &[BlockId],
    n: usize,
) -> Result<Result<bool, Fatal>, Fatal> {
    let m = &blocks[n];
    undo_block(evo, vcx, cx, &m.block, &at(m, ids[n], ids[n - 1]), &blocks[n - 1].hash())
}

/// [`undo`] of `block` at `at`, whose parent is `parent`.
fn undo_block<C: Ev1Checks + Send>(
    evo: &mut NodeEvo<MemoryEngine, C>,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    parent: &BlockHash,
) -> Result<Result<bool, Fatal>, Fatal> {
    let mut target = EvoTarget::Active;
    let facade = attempt_facade(cx.params);
    let mut att = evo.begin_attempt(
        &facade,
        vcx,
        cx,
        AttemptId::next(),
        AttemptOrigin::Tip,
        &ViewTarget::Active,
    )?;
    evo.begin_tx(&mut att, &mut target)?;
    evo.bls_ctx(&mut att, &mut target, cx.params, at);
    let r = evo.undo_special_txs(&mut att, &mut target, block, at);
    let undone = matches!(r, Ok(Some(None)));
    if undone {
        evo.set_best_block(&mut att, &mut target, parent)?;
        evo.commit_bls_scheme(&mut att, &mut target);
    }
    evo.finish_attempt(att)?;
    if undone {
        evo.commit_tx(&mut target)?;
    } else {
        evo.rollback_tx(&mut target);
    }
    Ok(r.map(|r| r.is_some()))
}

/// H9 below DIP0003 (§3.3): MNHF and the list have nothing to undo, and the quorum processor's
/// undo finds no commitment, so the block's evo tip moves back with no row erased. Core's undo
/// computes the rotated members first even below DIP0003 (`bp:566`, regtest's DIP0024 is at
/// height 1), unlike its connect (`bp:229-232`). Their set is empty there, so at a rotation
/// cycle base `NoMembers`' refusal is a no-op (D53, DEC-591).
#[test]
fn the_undo_below_dip3_and_at_a_rotation_cycle_base() {
    let p = regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 24, 3);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let resets = MemPlatformBans::default();
        let (mut evo, layer, _) = node_evo_with_bans(&p, BlsCtx::BASIC, Box::new(resets.clone()));
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            // Height 24 is regtest's first DIP0024 cycle base; height 23 is not.
            let base = undo(&mut evo, vcx, cx, &blocks, &ids, 23)?;
            let tip = layer.best_block();
            let below = undo(&mut evo, vcx, cx, &blocks, &ids, 22)?;
            let back = layer.best_block();
            Ok((
                base.ok(),
                tip == Some(blocks[22].hash()),
                below.ok(),
                back == Some(blocks[21].hash()),
            ))
        })?;
        // Core's `UndoBlock` never touches `m_mn_metaman` (SP1c-1 A3).
        assert!(resets.resets().is_empty(), "an undo resets no platform ban");
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let r = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(r, (Some(true), true, Some(true), true));
    rig.assert_no_abort();
}

/// D53's carve-out ends at DIP0003: regtest's DIP0003 block is also a DIP0024 cycle base, and
/// undoing it, its list diff stored as its connect staged it, meets the same `NoMembers` refusal,
/// which stays `Fatal` (§3.1). The disconnect never gets here: `undo_refusal` holds for the block
/// first (D70).
#[test]
fn the_undo_at_dip3_on_a_rotation_cycle_base_is_fatal() {
    let p = regtest();
    let dip3 = p.consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
    let n = usize::try_from(dip3).unwrap() - 1;
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, dip3, 3);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, store) = node_evo(&p, BlsCtx::BASIC);
        let hash = blocks[n].hash();
        let list = MnList::new(hash, i32::try_from(dip3).unwrap(), 0);
        let delta = EvoDelta {
            block: hash,
            height: dip3,
            dml: Some(testkit::dml(&MnList::default_pre_dip3(), &list, false)),
            mined_commitments: Vec::new(),
            credit_data: None,
            credit_pool: None,
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: Some(hash),
        };
        store.commit(&mut layer.lock(), &delta).expect("the DIP0003 block's rows");
        let r = with_tip_cx(v, |vcx, cx| undo(&mut evo, vcx, cx, &blocks, &ids, n))?;
        *seen.lock().expect("seen") = Some(r.map_err(|e| e.to_string()));
        Ok(())
    });
    let r = out.lock().expect("out").take().expect("V ran it");
    let fatal = r.expect_err("Fatal");
    assert!(fatal.contains("CQuorumBlockProcessor::UndoBlock"), "{fatal}");
    assert!(fatal.contains(QUORUM_VALIDATION), "{fatal}");
}

// The testnet coinbase at 1,569,000: a version-3 CbTx with a valid best ChainLock, difference 0.
const COINBASE: &str = "03000500010000000000000000000000000000000000000000000000000000000000000000ffffffff1903e8f0172f5032506f6f6c2d74444153482f6332706f6f6c2fffffffff05ba578c03000000001976a9140a7bf49abc7d07ade1826202b39a2b05a6d413ea88ac2ed5fd0300000000016a0132a706000000001976a91464f2b2b84f62d68a2cd7f7f5fb2b5aa75ef716d788ac01000000000000001976a91420cb5c22b1e4d5947e5c112c7696b51ad9af3c6188ac00000000000000002a6a280000000000000000000000000000000000000000000000000000000000000000000000000000000000000000af0300e8f0170025fb1b7b552062944faf4459f21bb5d9eae59060a60823a5107dc1f22a2cd697d6138f64f0bc8c9280379e0dd5d5f9b9d0a58dc9100960254c939edec7526af700a624ae0c90fbebc4a320804621d71119f6acce534c97a356558a0f390a8e9fd3d87e416f9941914f9a6381056185d9f716316911f9a0531f746d1e37700b013abc881cf1fa508b090890600afb60e4fd51e14f6a6a6b994804c9d733b0910ad4c0fee884ec210000";

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

fn coinbase() -> TxRef {
    Arc::new(decode_tx_exact(&unhex(COINBASE)).expect("the fixture decodes").into_value())
}

fn cbtx_of(tx: &Tx) -> CoinbaseCommitment {
    match &tx.payload().expect("special").expect("decodes").payload {
        SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.clone(),
        other => panic!("{other:?}"),
    }
}

/// The fixture's coinbase at `height` with best-ChainLock difference `diff`: the payload is
/// version(2) height(4) roots(64) diff(1) sig(96) balance(8).
fn coinbase_at(height: i32, diff: u8) -> TxRef {
    let mut raw = coinbase().to_mutable();
    raw.extra_payload[2..6].copy_from_slice(&height.to_le_bytes());
    raw.extra_payload[70] = diff;
    Arc::new(Tx::new(raw))
}

/// A verifier that records each request and answers `answer`.
struct Answer {
    answer: fn() -> Result<bool, StoreError>,
    asked: Mutex<Vec<(u32, BlockHash)>>,
}

impl ClVerifier for Answer {
    fn verify(
        &self,
        _: &ConnectCx<'_>,
        height: u32,
        block_hash: &BlockHash,
        _: &BlsSigValue,
        _: BlsCtx,
    ) -> Result<bool, StoreError> {
        self.asked.lock().expect("asked").push((height, *block_hash));
        (self.answer)()
    }
}

/// D45: step 10 with the best ChainLock null and its cache missing. The previous CbTx's ChainLock
/// (read from the parent's block, V20 at the parent) bounds this one; a non-null signature in
/// range goes to the verifier, whose `NotBuilt` is D2's refusal and whose fault is `Fatal`.
#[test]
fn step_ten_with_no_best_chainlock() {
    let p = regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 5, 4);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let block4 = blocks[3].hash();
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let r = with_tip_cx(v, |_, cx| {
            let reason = |r: Result<Result<(), SpecialTxFailure>, Fatal>| match r {
                Ok(Ok(())) => "ok".to_owned(),
                Ok(Err(SpecialTxFailure::Invalid(err))) => err.reject_reason().to_owned(),
                Ok(Err(SpecialTxFailure::NotReady(c))) => format!("not ready: {c}"),
                Err(_) => "fatal".to_owned(),
            };
            let header = || Header::new(BlockHeader::default());
            // The parent's block: its CbTx at height 4 with difference `diff`, or unreadable; V20
            // at the parent or not.
            let run_at = |cbtx: &CoinbaseCommitment,
                          parent: Option<u8>,
                          v: &dyn ClVerifier,
                          v20_at_prev: bool| {
                let parent_block =
                    parent.map(|diff| Arc::new(Block::new(header(), vec![coinbase_at(4, diff)])));
                let read = || parent_block.clone();
                let step = ClStep {
                    height: 5,
                    prev: ids[3],
                    v20_at_prev,
                    prev_block: &read,
                    bls: BlsCtx::BASIC,
                };
                reason(check_cbtx_best_chainlock(cbtx, &step, cx, v))
            };
            let run = |cbtx: &CoinbaseCommitment, parent: Option<u8>, v: &dyn ClVerifier| {
                run_at(cbtx, parent, v, true)
            };
            let base = cbtx_of(&coinbase());
            let with = |diff: u64, signed: bool, version: u16| {
                let mut c = base.clone();
                c.best_cl_height_diff = Some(diff);
                if !signed {
                    c.best_cl_signature = None;
                }
                c.version = version;
                c
            };
            let not_built = NotBuiltClVerifier;
            let mut out = vec![
                // Below CLSIG_AND_BALANCE nothing is read.
                run(&with(9, false, 2), Some(0), &not_built),
                // No ChainLock at the parent (none read): null with 0 passes, with 1 does not.
                run(&with(0, false, 3), None, &not_built),
                run(&with(1, false, 3), None, &not_built),
                // The parent's ChainLock: a null one now, then one more than a block older.
                run(&with(0, false, 3), Some(0), &not_built),
                // Without V20 at the parent its ChainLock is not read (`cbtx:187-189`).
                run_at(&with(0, false, 3), Some(0), &not_built, false),
                run(&with(2, true, 3), Some(0), &not_built),
                // In range, to the verifier: refused until CH7b.
                run(&with(1, true, 3), Some(0), &not_built),
                // Out of range: the difference reaches below genesis.
                run(&with(5, true, 3), None, &not_built),
            ];
            let asked = |answer| Answer {
                answer,
                asked: Mutex::new(Vec::new()),
            };
            let no = asked(|| Ok(false));
            out.push(run(&with(0, true, 3), None, &no));
            let yes = asked(|| Ok(true));
            out.push(run(&with(0, true, 3), None, &yes));
            let broken = asked(|| Err(StoreError::inconsistent("a broken read")));
            out.push(run(&with(0, true, 3), None, &broken));
            let requests = yes.asked.lock().expect("asked").clone();
            (out, requests)
        });
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let (out, requests) = out.lock().expect("out").take().expect("V ran it");
    let not_ready = format!("not ready: {CHAINLOCK_VERIFY}");
    assert_eq!(
        out,
        [
            "ok",
            "ok",
            "bad-cbtx-cldiff",
            "bad-cbtx-null-clsig",
            "ok",
            "bad-cbtx-older-clsig",
            not_ready.as_str(),
            "bad-cbtx-cldiff",
            "bad-cbtx-invalid-clsig",
            "ok",
            "fatal",
        ]
    );
    // The ChainLock of difference 0 at height 5 locks block 4.
    assert_eq!(requests, [(4, block4)]);
    rig.assert_no_abort();
}

/// Core's well-formed registration (`trivially_valid.json`), which step 4 rejects on regtest
/// (`bad-protx-netinfo-port`).
fn core_pro_reg() -> TxRef {
    let file = "trivially_valid.json";
    let text = dash_core_vectors::read_core_text(&format!("src/test/data/{file}"));
    let cases = dash_core_vectors::cases::load_trivial(file, &text).expect(file);
    let case = cases.into_iter().find(|c| c.tx_type == "proregtx").expect("a registration");
    Arc::new(decode_tx_exact(&case.tx).expect("decodes").into_value())
}

/// D3's table at H4's entry, through `NodeEvo` over `DmlChecks` on regtest with DIP0003 at 30
/// (DIP0024 from height 1, so 48 is the first rotation cycle base). A well-formed block holding
/// a non-null commitment, an MNHF signal or an asset unlock, or at a cycle base, is refused before
/// any step judges it; the declared interim differences (D3: an MNHF signal or a commitment of a
/// version Core rejects, a step-1 reject at a cycle base) get the refusal too. Decode first: one
/// special payload that does not decode completely bypasses G1q, so the steps answer, Core's
/// reject where its check is ported and the stub's refusal where it is not (D2). Null commitments
/// alone pass G1q, to step 4's `CheckLLMQCommitment` (DEC-990): these name no block of the chain.
/// H4 binds the seams for type 6 alone (OD-CP-11): an asset unlock that G1q leaves to the steps
/// gets the quorum manager's refusal, never the index's `bad-assetunlock-quorum-hash`.
#[test]
fn g1q_refuses_at_h4_entry_after_decoding_every_payload() {
    use crate::evo::guard::tests::{
        DIP3, asset_unlock, mnhf, qc, regtest as dip3_at_30, special, trailing, truncated,
    };

    let p = Arc::new(dip3_at_30(&[]));
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 48, 0);
    let rig = Rig::with(super::support::RigConfig {
        params: Some(p.clone()),
        ..super::support::RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, store) = node_evo_with(&p, BlsCtx::BASIC, DmlChecks);
        // The parents' lists, Core's empty one at each, as connects from DIP0003 store them.
        for parent in [&blocks[DIP3 as usize + 1], &blocks[46]] {
            let empty = MnList::default_pre_dip3();
            let list =
                empty.clone().with_block_hash(parent.hash()).with_height(parent.height as i32);
            let delta = EvoDelta {
                block: parent.hash(),
                height: parent.height,
                dml: Some(testkit::dml(&empty, &list, true)),
                mined_commitments: Vec::new(),
                credit_data: None,
                credit_pool: None,
                mnhf_signals: None,
                erased_mined: Vec::new(),
                evo_tip: Some(parent.hash()),
            };
            store.commit(&mut layer.lock(), &delta).map_err(|e| Fatal::new(e.to_string()))?;
        }
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let at_h =
                |h: u32| at(&blocks[h as usize - 1], ids[h as usize - 1], ids[h as usize - 2]);
            let mut verdicts = Vec::new();
            let mut judge = |h: u32, txs: Vec<TxRef>, check: bool| -> Result<(), Fatal> {
                // A classical coinbase unless the block brings its own.
                let mut all = vec![tx(1, 0, true, 0)];
                if txs.first().is_some_and(|t| t.is_coinbase()) {
                    all.clear();
                }
                all.extend(txs);
                let body = Block::new(Header::new(BlockHeader::default()), all);
                let got = connect(&mut evo, vcx, cx, &body, &at_h(h), check, false)?;
                verdicts.push(match got {
                    Ok(Err(SpecialTxFailure::NotReady(QUORUM_VALIDATION))) => "G1q".to_owned(),
                    Ok(Err(SpecialTxFailure::NotReady(stub))) => format!("stub: {stub}"),
                    Ok(Err(SpecialTxFailure::Invalid(e))) => e.reject_reason().to_owned(),
                    other => format!("{other:?}"),
                });
                Ok(())
            };
            let h = DIP3 + 3;
            let non_null = qc(&p, h, false);
            let null = || qc(&p, h, true);
            let bad_protx = special(1, vec![1, 0]);
            let versioned = |tx: &Tx, version: u8| {
                let mut raw = tx.to_mutable();
                raw.extra_payload[0] = version;
                Arc::new(Tx::new(raw))
            };
            // (a), (b) and null commitments.
            judge(h, vec![non_null.clone()], false)?;
            judge(h, vec![null(), non_null.clone()], true)?;
            judge(h, vec![mnhf()], false)?;
            judge(h, vec![asset_unlock()], false)?;
            judge(h, vec![null(), null()], false)?;
            // Decode first.
            judge(h, vec![truncated(&non_null)], false)?;
            judge(h, vec![truncated(&special_cbtx(2, h, 0, 0)), non_null.clone()], true)?;
            judge(h, vec![bad_protx.clone(), non_null.clone()], false)?;
            judge(h, vec![non_null.clone(), bad_protx], false)?;
            judge(h, vec![mnhf(), truncated(&non_null)], false)?;
            judge(h, vec![trailing(&mnhf()), non_null.clone()], false)?;
            judge(h, vec![asset_unlock(), truncated(&non_null)], false)?;
            // The interim differences: a registration step 4 rejects ahead of a commitment,
            // versions Core rejects, a step-1 reject at a cycle base.
            judge(h, vec![core_pro_reg()], false)?;
            judge(h, vec![core_pro_reg(), non_null.clone()], false)?;
            judge(h, vec![versioned(&mnhf(), 2)], false)?;
            judge(h, vec![versioned(&non_null, 9)], false)?;
            judge(48, vec![], false)?;
            judge(48, vec![special_cbtx(2, 47, 0, 0)], true)?;
            judge(48, vec![truncated(&special_cbtx(2, 48, 0, 0))], true)?;
            Ok(verdicts)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let verdicts = out.lock().expect("out").take().expect("V ran it");
    let quorum_stub = "stub: the quorum manager (QM1a)";
    let want = [
        // (a), (b), null commitments.
        "G1q",
        "G1q",
        "G1q",
        "G1q",
        "bad-qc-quorum-hash",
        // Decode first.
        "bad-qc-payload",
        "bad-cbtx-payload",
        "bad-protx-payload",
        "bad-qc-quorum-hash",
        quorum_stub,
        "bad-mnhf-payload",
        quorum_stub,
        // The interim differences.
        "bad-protx-netinfo-port",
        "G1q",
        "G1q",
        "G1q",
        "G1q",
        "G1q",
        "bad-cbtx-payload",
    ];
    assert_eq!(verdicts, want);
    rig.assert_no_abort();
}

/// `qc` in a type-6 transaction for the block at `height` (`CFinalCommitmentTxPayload`).
fn qc_tx(height: u32, qc: &LlmqCommitment) -> TxRef {
    let mut payload = Vec::new();
    payload.extend(1u16.to_le_bytes());
    payload.extend(height.to_le_bytes());
    write_commitment(qc, &mut payload);
    crate::evo::guard::tests::special(TRANSACTION_QUORUM_COMMITMENT, payload)
}

/// DEC-990: H4 binds type 6's seams, so a block's null commitments get `CheckLLMQCommitment`. On
/// regtest with DIP0003 at 30, at the first height past it whose block needs commitments, the
/// block of the null ones the miner builds (`GetMineableCommitments` with an empty pool)
/// connects; without them it misses them (step 6), and with one whose bitset is not the quorum's
/// size it gets Core's `bad-qc-invalid-null` from step 4, ahead of step 6's same check. A non-null
/// one is G1q's (arm (a)), unless a payload that does not decode leaves the block to the steps:
/// then `Verify` answers, here before its member read. A caller that binds no seams still gets the
/// refusal (`ev1_tests`).
#[test]
fn null_commitments_connect_through_type_6s_seams() {
    use crate::evo::guard::tests::{DIP3, regtest as dip3_at_30};
    use crate::evo::node::Ancestry;
    use crate::evo::not_built::NoMembers;

    let p = Arc::new(dip3_at_30(&[]));
    let genesis = Mined::genesis(&p);
    // Short of 48, the first rotation cycle base, which G1q's arm (c) holds.
    let blocks = branch(&p, &genesis, 47, 0);
    let rig = Rig::with(super::support::RigConfig {
        params: Some(p.clone()),
        ..super::support::RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, store) = node_evo_with(&p, BlsCtx::BASIC, DmlChecks);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let chain = Ancestry(cx.reader);
            let complete = CompleteChain::<StoreError>::new(&chain);
            // The miner's commitments for the block at `h`: null ones, its pool being empty.
            let mineable = |h: u32| -> Vec<LlmqCommitment> {
                let prev = ids[h as usize - 2];
                let bls = bls_ctx_for_block(&p, h);
                let llmq = LlmqCtx::new(&p, &complete, cx.deployments, bls).expect("in domain");
                let mined = store.view(&layer.lock(), &chain);
                let members = NoMembers {
                    llmq,
                };
                let qc = QcCtx {
                    llmq,
                    index: &chain,
                    mined: &mined,
                    members: &members,
                    precompute: &members,
                    bls_checks: &SerialBlsChecks,
                };
                let pool = MineablePool::new();
                let enabled = enabled_quorum_params(&llmq, prev).expect("enabled types");
                let each = enabled.into_iter().map(|params| {
                    pool.mineable_commitments(&qc, params, prev, h).expect("mineable")
                });
                each.flatten().flatten().collect()
            };
            let h = (DIP3 + 2..48).find(|&h| !mineable(h).is_empty()).expect("a mining window");
            let qcs = mineable(h);
            // The parent's list, Core's empty one, as a connect from DIP0003 stores it.
            let parent = &blocks[h as usize - 2];
            let empty = MnList::default_pre_dip3();
            let list =
                empty.clone().with_block_hash(parent.hash()).with_height(parent.height as i32);
            let delta = EvoDelta {
                block: parent.hash(),
                height: parent.height,
                dml: Some(testkit::dml(&empty, &list, true)),
                mined_commitments: Vec::new(),
                credit_data: None,
                credit_pool: None,
                mnhf_signals: None,
                erased_mined: Vec::new(),
                evo_tip: Some(parent.hash()),
            };
            store.commit(&mut layer.lock(), &delta).map_err(|e| Fatal::new(e.to_string()))?;

            let at = at(&blocks[h as usize - 1], ids[h as usize - 1], ids[h as usize - 2]);
            let mut judge = |qcs: &[LlmqCommitment], extra: Option<TxRef>| -> Result<_, Fatal> {
                let mut txs = vec![tx(1, 0, true, 0)];
                txs.extend(qcs.iter().map(|qc| qc_tx(h, qc)));
                txs.extend(extra);
                let body = Block::new(Header::new(BlockHeader::default()), txs);
                Ok(match connect(&mut evo, vcx, cx, &body, &at, false, false)? {
                    Ok(Ok(true)) => "connects".to_owned(),
                    Ok(Err(SpecialTxFailure::NotReady(what))) => format!("not ready: {what}"),
                    // Step 4's reject carries its transaction (`stm:957-958`).
                    Ok(Err(SpecialTxFailure::Invalid(e)))
                        if e.debug_message().starts_with("Special Transaction check failed") =>
                    {
                        format!("step 4: {}", e.reject_reason())
                    }
                    Ok(Err(SpecialTxFailure::Invalid(e))) => e.reject_reason().to_owned(),
                    other => format!("{other:?}"),
                })
            };
            let mut wrong_size = qcs.clone();
            let signers = &mut wrong_size[0].signers;
            signers.num_bits += 8;
            signers.data.push(0);
            assert!(wrong_size[0].is_null(), "still null");
            // No valid member: `Verify` rejects it before it reads the members.
            let mut non_null = qcs.clone();
            non_null[0].quorum_vvec_hash = QuorumVvecHash::from([1; 32]);
            let undecodable = crate::evo::guard::tests::truncated(&qc_tx(h, &qcs[0]));
            let verdicts = [
                judge(&qcs, None)?,
                judge(&[], None)?,
                judge(&wrong_size, None)?,
                judge(&non_null, None)?,
                judge(&non_null, Some(undecodable))?,
            ];
            Ok((h, verdicts))
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let (h, verdicts) = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(h, 34, "cycle base 24's mining window");
    let held = format!("not ready: {QUORUM_VALIDATION}");
    let want = [
        "connects",
        "bad-qc-missing",
        "step 4: bad-qc-invalid-null",
        &held,
        "step 4: bad-qc-invalid",
    ];
    assert_eq!(verdicts, want);
    rig.assert_no_abort();
}
