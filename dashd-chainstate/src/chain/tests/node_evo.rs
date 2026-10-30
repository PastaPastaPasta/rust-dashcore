//! `NodeEvo` (CH7 §2.1) on a real V, its hooks called as the connector calls them: below
//! DIP0003 it gives `NoEvo`'s verdicts and stages the evo tip (D44, D7), from DIP0003 its stubs
//! refuse (D2); the transaction slot (D1), the published scheme (D9), H9 below DIP0003, and step
//! 10's arms with no best ChainLock (D45).

use std::sync::{Arc, Mutex};

use dash_consensus::coins::{CoinRef, CoinsView};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::primitives::{
    Header, OutPoint, Transaction, TxIn, TxOut, TxRef, decode_tx_exact,
};
use dash_consensus::{Block, BlockId, BlsCtx, Tx};
use dash_dml::netinfo::{NetInfoConfig, NetInfoRules};
use dash_dml::{EvoDelta, MnList, StoreError};
use dash_evo::llmq::BlsSigValue;
use dash_evo::types::ProTxHash;
use dash_params::{
    BuriedDeployment, ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release,
};
use dash_primitives::{BlockHash, BlockHeader, CoinbaseCommitment, SpecialPayload, TxHash, TxType};
use dashd_store::MemoryEngine;

use super::support::{Mined, Rig, branch, install_accept_log, levels_on, regtest, with_tip_cx};
use crate::chain::{AttemptOrigin, ConnectCx, EvoTarget, Fatal, VContext, ViewTarget};
use crate::connect::{
    AttemptId, BlockAt, EvoHooks, SpecialTxFailure, SpecialTxRules, pre_dip3_special_txs,
};
use crate::evo::cl::{
    CHAINLOCK_VERIFY, ClStep, ClVerifier, NotBuiltClVerifier, check_cbtx_best_chainlock,
};
use crate::evo::effects::{
    EffectAppliers, MemPlatformBans, PlatformBanApplier, StoreSnapshots, UnreachableIsLock,
    UnreachableMineable, UnreachableNotify,
};
use crate::evo::ev1::{Ev1NotBuilt, SPECIAL_TX_CHECKS};
use crate::evo::node::{EvoBlocks, NodeEvo, NodeEvoParts, SharedEvoLayer};
use crate::evo::not_built::QUORUM_VALIDATION;
use crate::evo::{RocksEvoStore, testkit};
use crate::flush::{EffectLedger, EvoWindow};
use crate::store::{CHAINSTATE_SCHEMA, CoinsReadError};

/// No coin: the checks below DIP0003 read none.
struct NoCoins;

impl CoinsView for NoCoins {
    type Error = CoinsReadError;

    fn coin(&self, _: &OutPoint) -> Result<Option<CoinRef<'_>>, CoinsReadError> {
        Ok(None)
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// No block data: step 10's parent read fails, as an unread block's does.
struct NoBlocks;

impl EvoBlocks for NoBlocks {
    fn read(&self, _: &dash_chain::ChainReader, _: BlockId) -> Option<Arc<Block>> {
        None
    }
}

fn node_evo(
    p: &Arc<ChainParams>,
    scheme: BlsCtx,
) -> (NodeEvo<MemoryEngine>, SharedEvoLayer, Store) {
    node_evo_with_bans(p, scheme, Box::new(MemPlatformBans::default()))
}

/// [`node_evo`] with `platform_ban` as its platform-ban applier.
fn node_evo_with_bans(
    p: &Arc<ChainParams>,
    scheme: BlsCtx,
    platform_ban: Box<dyn PlatformBanApplier>,
) -> (NodeEvo<MemoryEngine>, SharedEvoLayer, Store) {
    let genesis = Mined::genesis(p).hash();
    let engine = crate::evo::testkit::guarded(MemoryEngine::new(&CHAINSTATE_SCHEMA));
    let store = Arc::new(RocksEvoStore::new(engine, crate::evo::testkit::codec(), genesis));
    let layer = SharedEvoLayer::new(store.resume_layer().expect("a fresh meta row"));
    let mainnet =
        ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().expect("mainnet");
    let appliers = EffectAppliers {
        snapshot: Box::new(StoreSnapshots {
            store: store.clone(),
            ledger: Arc::new(EffectLedger::default()),
        }),
        is_lock: Box::new(UnreachableIsLock),
        mineable: Box::new(UnreachableMineable),
        platform_ban,
        notify: Box::new(UnreachableNotify),
    };
    let parts = NodeEvoParts {
        store: store.clone(),
        layer: layer.clone(),
        appliers,
        params: p.clone(),
        net_info: NetInfoRules::new(
            p,
            &mainnet,
            NetInfoConfig {
                cjdns_reachable: false,
            },
        ),
        ev1: Ev1NotBuilt,
        cl: Box::new(NotBuiltClVerifier),
        blocks: Box::new(NoBlocks),
        scheme,
    };
    (NodeEvo::new(parts).expect("regtest's payment parameters"), layer, store)
}

/// The hooks' evo store.
type Store = Arc<RocksEvoStore<MemoryEngine>>;

/// H4's answer, with whether it left list updates.
type H4 = Result<Result<bool, SpecialTxFailure>, Fatal>;

/// One Active connect attempt in the connector's order (CH3 §3.1): H10, H1, H4, and on success
/// H6, H1's commit and H10's commit when `commit`, else the rollback.
fn connect(
    evo: &mut NodeEvo<MemoryEngine>,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    check: bool,
    commit: bool,
) -> Result<H4, Fatal> {
    let mut target = EvoTarget::Active;
    let mut att = evo.begin_attempt(
        &(),
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
/// transactions after the coinbase.
fn bodies() -> Vec<Block> {
    let cb = tx(1, 0, true, 0);
    [
        vec![cb.clone(), tx(2, 0, false, 1)],
        vec![tx(1, TRANSACTION_COINBASE, true, 0)],
        vec![tx(2, TRANSACTION_COINBASE, true, 0)],
        vec![tx(2, TRANSACTION_COINBASE, false, 2)],
        vec![cb.clone(), tx(3, 1, false, 3)],
        vec![cb, tx(2, 1, false, 4), tx(3, 8, false, 5)],
    ]
    .into_iter()
    .map(|txs| Block::new(Header::new(BlockHeader::default()), txs))
    .collect()
}

fn at(m: &Mined, id: BlockId, prev: BlockId) -> BlockAt {
    BlockAt {
        hash: m.hash(),
        height: m.height,
        prev: Some(prev),
        id: Some(id),
    }
}

/// D44 through NodeEvo (DEC-581): below DIP0003 every body gets `NoEvo`'s verdict with and
/// without the CbTx checks, never a refusal, and a committed attempt stages its block as the evo
/// tip (D7) with nothing in its log; at DIP0003 Core's step 2 still judges, and the stubs refuse.
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
        let result = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut prev = gid;
            let mut best = Vec::new();
            for (n, (m, &id)) in blocks.iter().zip(&ids).enumerate() {
                let at = at(m, id, prev);
                if m.height < dip3 {
                    // A few heights through the whole range keep the test quick.
                    if n < 3 || m.height + 2 >= dip3 || m.height % 97 == 0 {
                        for body in bodies() {
                            for check in [false, true] {
                                let want = pre_dip3_special_txs(&body, check)
                                    .map(|r| r.map_err(SpecialTxFailure::Invalid));
                                let got = connect(&mut evo, vcx, cx, &body, &at, check, false)?;
                                assert_eq!(got.map(|r| r.map(drop)), want, "height {}", m.height);
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
                    let refused = connect(&mut evo, vcx, cx, &m.block, &at, false, true)?;
                    assert_eq!(refused, Ok(Err(SpecialTxFailure::NotReady(SPECIAL_TX_CHECKS))));
                    let judged = connect(&mut evo, vcx, cx, &m.block, &at, true, true)?;
                    let Ok(Err(SpecialTxFailure::Invalid(err))) = judged else {
                        panic!("{judged:?}");
                    };
                    assert_eq!(err.reject_reason(), "bad-cbtx-version");
                    // Neither refusal moved the evo tip.
                    assert_eq!(layer.best_block(), Some(blocks[m.height as usize - 2].hash()));
                }
                prev = id;
            }
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

            let mut att = evo.begin_attempt(
                &(),
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
            let next = evo.begin_attempt(
                &(),
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
            let att = evo.begin_attempt(
                &(),
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
                let mut att = evo.begin_attempt(
                    &(),
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

/// Every reset the platform-ban applier is handed.
#[derive(Clone, Default)]
struct Resets(Arc<Mutex<Vec<(ProTxHash, i32)>>>);

impl Resets {
    fn is_empty(&self) -> bool {
        self.0.lock().expect("resets").is_empty()
    }
}

impl PlatformBanApplier for Resets {
    fn reset(&mut self, pro_tx_hash: &ProTxHash, height: i32) -> Result<(), Fatal> {
        self.0.lock().expect("resets").push((*pro_tx_hash, height));
        Ok(())
    }
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
        let resets = Resets::default();
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
                    let want = pre_dip3_special_txs(&body, check)
                        .map(|r| r.map_err(SpecialTxFailure::Invalid));
                    let got = connect(&mut evo, vcx, cx, &body, at, check, true)?;
                    verdicts.push(got.map(|r| r.map(drop)) == want);
                }
                prev = id;
            }
            Ok(verdicts)
        })?;
        assert!(resets.is_empty(), "no reset below DIP0003");
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
    evo: &mut NodeEvo<MemoryEngine>,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    blocks: &[Mined],
    ids: &[BlockId],
    n: usize,
) -> Result<Result<bool, Fatal>, Fatal> {
    let m = &blocks[n];
    let at = at(m, ids[n], ids[n - 1]);
    let mut target = EvoTarget::Active;
    let mut att = evo.begin_attempt(
        &(),
        vcx,
        cx,
        AttemptId::next(),
        AttemptOrigin::Tip,
        &ViewTarget::Active,
    )?;
    evo.begin_tx(&mut att, &mut target)?;
    evo.bls_ctx(&mut att, &mut target, cx.params, &at);
    let r = evo.undo_special_txs(&mut att, &mut target, &m.block, &at);
    let undone = matches!(r, Ok(Some(None)));
    if undone {
        evo.set_best_block(&mut att, &mut target, &blocks[n - 1].hash())?;
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
        let resets = Resets::default();
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
        assert!(resets.is_empty(), "an undo resets no platform ban");
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let r = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(r, (Some(true), true, Some(true), true));
    rig.assert_no_abort();
}

/// D53's carve-out ends at DIP0003: regtest's DIP0003 block is also a DIP0024 cycle base, and
/// undoing it, its list diff stored as its connect staged it, meets the same `NoMembers` refusal,
/// which stays `Fatal` (§3.1).
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
