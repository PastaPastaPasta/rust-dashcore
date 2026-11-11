//! `NodeEvo` (CH7 §2.1) on a real V, its hooks called as the connector calls them: below
//! DIP0003 it gives `NoEvo`'s verdicts and stages the evo tip (D44, D7), from DIP0003 its steps
//! judge through the attempt's binding, where G1q refused until CH7b-2 (D3), and its stubs refuse
//! (D2); the transaction slot (D1), the published scheme (D9), H9 below DIP0003 and at a cycle
//! base, and step 10's arms with no best ChainLock (D45).

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
use dash_dml::{Dmn, EvoDelta, MnList, MnListChange, StoreError};
use dash_evo::EffectSink;
use dash_evo::llmq::{
    BlsSigValue, CompleteChain, LlmqCommitment, LlmqCtx, MemberError, PrecomputeMode,
    QuorumMemberSource, QuorumMembers, QuorumPrecompute, SerialBlsChecks, enabled_quorum_params,
};
use dash_params::LlmqType;
use dash_params::{
    BuriedDeployment, ChainOption, ChainParamsBuilder, ChainSetup, Network, Release,
};
use dash_pkc::bls::{BlsGlobalSig, BlsScIetf, BlsSecretKey};
use dash_primitives::{
    BlockHash, BlockHeader, CoinbaseCommitment, QuorumVvecHash, SpecialPayload, TxHash, TxType,
};
use dashd_store::MemoryEngine;

use super::llmq_members::World;
use super::scheme_guard::{ENTERED_LEGACY, REVERTED_BASIC};
use super::support::{Mined, Rig, branch, install_accept_log, levels_on, regtest, with_tip_cx};
use crate::chain::{
    AttemptOrigin, ChainLockSig, ConnectCx, EvoTarget, Fatal, SchemeGuardHolder, VContext,
    ValidationSignals, ViewTarget,
};
use crate::connect::{
    AttemptId, BlockAt, DummyIndex, EvoDigest, EvoHooks, SpecialTxFailure, SpecialTxRules,
    pre_dip3_special_txs,
};
use crate::evo::cl::{
    ClAttempt, ClPass, ClStep, ClVerifier, NotBuiltClVerifier, check_cbtx_best_chainlock,
};
use crate::evo::effects::{
    EffectAppliers, FacadeMineable, IsLockApplier, MemPlatformBans, MineableApplier, MnListSignal,
    PlatformBanApplier, SnapshotApplier, StoreSnapshots, UnreachableIsLock, UnreachableMineable,
};
use crate::evo::ev1::{DmlChecks, Ev1Checks, Ev1NotBuilt, SPECIAL_TX_CHECKS, SpecialTxCtx};
use crate::evo::node::{Ancestry, EvoBlocks, NodeEvo, NodeEvoParts, SharedEvoLayer};
use crate::evo::{RocksEvoStore, testkit};
use crate::flush::{EffectLedger, EvoWindow};
use crate::llmq::facade::MembersFacade;
use crate::llmq::rows::RuntimeRows;
use crate::store::{
    CHAINSTATE_SCHEMA, CoinsReadError, GlobalFailStop, GuardedEngine, RecordingSink,
};

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
fn net_info(p: &ChainSetup) -> NetInfoRules {
    let mainnet = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("mainnet");
    NetInfoRules::new(
        p,
        &mainnet,
        NetInfoConfig {
            cjdns_reachable: false,
        },
    )
}

pub(super) fn node_evo(p: &Arc<ChainSetup>, scheme: BlsCtx) -> (Evo, SharedEvoLayer, Store) {
    node_evo_with(p, scheme, Recording)
}

/// [`node_evo`] whose conflicting IS locks are removed by `is_lock`.
pub(super) fn node_evo_is_lock(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    is_lock: Box<dyn IsLockApplier>,
) -> (Evo, SharedEvoLayer, Store) {
    let overrides = Overrides {
        is_lock: Some(is_lock),
        ..Overrides::default()
    };
    let bans = Box::new(MemPlatformBans::default());
    let signals = ValidationSignals::default();
    build(p, scheme, Recording, bans, &signals, overrides, Box::new(NoBlocks))
}

/// [`node_evo`] whose store's engine reports to `sink`.
pub(super) fn node_evo_reporting(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    sink: Arc<RecordingSink>,
) -> (Evo, SharedEvoLayer, Store) {
    let overrides = Overrides {
        sink: Some(sink),
        ..Overrides::default()
    };
    let bans = Box::new(MemPlatformBans::default());
    let signals = ValidationSignals::default();
    build(p, scheme, Recording, bans, &signals, overrides, Box::new(NoBlocks))
}

/// [`node_evo`] over `ev1`.
fn node_evo_with<C: Ev1Checks>(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    ev1: C,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    node_evo_on(p, scheme, ev1, &ValidationSignals::default())
}

/// [`node_evo_with`], raising the MN-list signal on `signals`.
pub(super) fn node_evo_on<C: Ev1Checks>(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    ev1: C,
    signals: &ValidationSignals,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    build_node_evo(p, scheme, ev1, Box::new(MemPlatformBans::default()), signals, None)
}

/// [`node_evo_with`] reading its blocks from `blocks`: what the coinbase facts of route B and of
/// an attempt's members read. With `snapshot`, that is its snapshot applier.
pub(super) fn node_evo_over_blocks<C: Ev1Checks>(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    ev1: C,
    blocks: Box<dyn EvoBlocks>,
    snapshot: Option<Box<dyn SnapshotApplier>>,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    let bans = Box::new(MemPlatformBans::default());
    let signals = ValidationSignals::default();
    let overrides = Overrides {
        snapshot,
        ..Overrides::default()
    };
    build(p, scheme, ev1, bans, &signals, overrides, blocks)
}

/// [`node_evo`] with `platform_ban` as its platform-ban applier.
pub(super) fn node_evo_with_bans(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    platform_ban: Box<dyn PlatformBanApplier>,
) -> (Evo, SharedEvoLayer, Store) {
    build_node_evo(p, scheme, Recording, platform_ban, &ValidationSignals::default(), None)
}

/// [`node_evo_with`] with `mineable` as its mineable-pool applier.
pub(super) fn node_evo_with_mineable<C: Ev1Checks>(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    ev1: C,
    mineable: Box<dyn MineableApplier>,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    let bans = Box::new(MemPlatformBans::default());
    let overrides = Overrides {
        mineable: Some(mineable),
        ..Overrides::default()
    };
    let signals = ValidationSignals::default();
    build(p, scheme, ev1, bans, &signals, overrides, Box::new(NoBlocks))
}

/// [`node_evo_with`] wired to `world` as dashd-node wires the hooks to its facade: the facade's
/// rows take the snapshot writes, and its pool the mineable effects.
pub(super) fn node_evo_on_world<C: Ev1Checks>(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    ev1: C,
    world: &World,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    let overrides = Overrides {
        snapshot: Some(Box::new(StoreSnapshots {
            rows: Arc::clone(&world.rows),
        })),
        mineable: Some(Box::new(FacadeMineable(Arc::clone(&world.facade)))),
        ..Overrides::default()
    };
    let bans = Box::new(MemPlatformBans::default());
    build(p, scheme, ev1, bans, &ValidationSignals::default(), overrides, Box::new(NoBlocks))
}

/// [`node_evo`] with `snapshot` as its snapshot applier.
pub(super) fn node_evo_with_snapshots(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    snapshot: Box<dyn SnapshotApplier>,
) -> (Evo, SharedEvoLayer, Store) {
    let bans = Box::new(MemPlatformBans::default());
    build_node_evo(p, scheme, Recording, bans, &ValidationSignals::default(), Some(snapshot))
}

/// [`node_evo`] over `ev1`, with `platform_ban` as its platform-ban applier, raising the MN-list
/// signal on `signals`, and `snapshot`, if any, as its snapshot applier.
fn build_node_evo<C: Ev1Checks>(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    ev1: C,
    platform_ban: Box<dyn PlatformBanApplier>,
    signals: &ValidationSignals,
    snapshot: Option<Box<dyn SnapshotApplier>>,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    let overrides = Overrides {
        snapshot,
        ..Overrides::default()
    };
    build(p, scheme, ev1, platform_ban, signals, overrides, Box::new(NoBlocks))
}

/// [`build_node_evo`] for dashd's masternode chain: the node's EV1 checks, and the store's codec
/// under the chain's own NetInfo rules.
fn mn_chain_node_evo(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    platform_ban: Box<dyn PlatformBanApplier>,
    signals: &ValidationSignals,
) -> (NodeEvo<MemoryEngine, DmlChecks>, SharedEvoLayer, Store) {
    let overrides = Overrides {
        codec: Some(crate::evo::testkit::codec_for(p)),
        ..Overrides::default()
    };
    build(p, scheme, DmlChecks, platform_ban, signals, overrides, Box::new(NoBlocks))
}

/// The appliers a test sets in place of the fixture's: the runtime rows' snapshot writer and the
/// fail-closed IS-lock and mineable stand-ins; the store's codec in place of the context-free
/// one, for a chain dashd mined (its local addresses, base-sdk #114); and the sink the store's
/// engine reports to.
#[derive(Default)]
struct Overrides {
    snapshot: Option<Box<dyn SnapshotApplier>>,
    is_lock: Option<Box<dyn IsLockApplier>>,
    mineable: Option<Box<dyn MineableApplier>>,
    codec: Option<crate::evo::EvoCodec>,
    sink: Option<Arc<RecordingSink>>,
}

/// [`build_node_evo`] with `overrides`, reading its blocks from `blocks`.
fn build<C: Ev1Checks>(
    p: &Arc<ChainSetup>,
    scheme: BlsCtx,
    ev1: C,
    platform_ban: Box<dyn PlatformBanApplier>,
    signals: &ValidationSignals,
    overrides: Overrides,
    blocks: Box<dyn EvoBlocks>,
) -> (NodeEvo<MemoryEngine, C>, SharedEvoLayer, Store) {
    let mn_list = MnListSignal::new(signals.clone());
    let genesis = Mined::genesis(p).hash();
    let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
    let engine = match overrides.sink {
        Some(sink) => Arc::new(GuardedEngine::new(engine, GlobalFailStop::new(), sink)),
        None => crate::evo::testkit::guarded(engine),
    };
    let codec = overrides.codec.unwrap_or_else(crate::evo::testkit::codec);
    let store = Arc::new(RocksEvoStore::new(engine, codec, genesis));
    let layer = SharedEvoLayer::new(store.resume_layer().expect("a fresh meta row"));
    let appliers = EffectAppliers {
        snapshot: overrides.snapshot.unwrap_or_else(|| {
            Box::new(StoreSnapshots {
                rows: Arc::new(RuntimeRows::new(store.clone(), Arc::new(EffectLedger::default()))),
            })
        }),
        is_lock: overrides.is_lock.unwrap_or_else(|| Box::new(UnreachableIsLock)),
        mineable: overrides.mineable.unwrap_or_else(|| Box::new(UnreachableMineable)),
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
        blocks,
        scheme,
        mn_list,
        bls_checks: Arc::new(SerialBlsChecks),
    };
    (NodeEvo::new(parts).expect("regtest's payment parameters"), layer, store)
}

/// A members facade for one attempt of the hooks under test: they ask it for no members, so
/// each attempt opens its own (EV addendum §2.6).
pub(super) fn attempt_facade(p: &ChainSetup) -> Arc<MembersFacade> {
    super::llmq_members::world(p).facade
}

/// The hooks' evo store.
pub(super) type Store = Arc<RocksEvoStore<MemoryEngine>>;

/// H4's answer, with whether it left list updates.
pub(super) type H4 = Result<Result<bool, SpecialTxFailure>, Fatal>;

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
    let facade = attempt_facade(cx.params);
    connect_on(evo, &facade, vcx, cx, block, at, false, check, commit)
}

/// The context a proposal's attempt runs in, as `TestBlockValidity`'s (chain/proposal.rs):
/// the dummy's reader and deployments, and the proposal's origin.
pub(super) fn proposal_cx<'s>(dummy: &'s DummyIndex<'s>, cx: &ConnectCx<'s>) -> ConnectCx<'s> {
    ConnectCx {
        origin: AttemptOrigin::Proposal,
        ..dummy.cx(cx)
    }
}

/// [`connect`] in an attempt on `facade`, with H4's `fJustCheck` `just_check`, under
/// `cx.origin`.
#[expect(clippy::too_many_arguments, reason = "connect's, the facade and fJustCheck")]
pub(super) fn connect_on<C: Ev1Checks + Send>(
    evo: &mut NodeEvo<MemoryEngine, C>,
    facade: &MembersFacade,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    just_check: bool,
    check: bool,
    commit: bool,
) -> Result<H4, Fatal> {
    let mut target = EvoTarget::Active;
    let mut att =
        evo.begin_attempt(facade, vcx, cx, AttemptId::next(), cx.origin, &ViewTarget::Active)?;
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
        just_check,
        check,
        &mut || Ok(None),
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

pub(super) fn tx(version: i16, tx_type: u16, coinbase: bool, n: u8) -> TxRef {
    let prevout = OutPoint::new(
        TxHash::from(
            [if coinbase {
                0
            } else {
                n
            }; 32],
        ),
        if coinbase {
            u32::MAX
        } else {
            0
        },
    );
    Arc::new(Tx::new(Transaction::new(
        version,
        TxType::new(tx_type),
        vec![TxIn::new(prevout, vec![1, n].into(), u32::MAX)],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        0,
        Vec::new(),
    )))
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
/// (DIP0024 from height 1), where G1q refused until CH7b-2. Its block now reaches the steps: with
/// the CbTx checks its classical coinbase is Core's `bad-cbtx-version` (`specialtxman.cpp:894-898`
/// at v24.0.0), and without them step 4's EV1 stand-in refuses it (D44). Neither moves the evo tip.
#[test]
fn node_evo_is_no_evo_below_dip3_and_judges_from_it() {
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
                    // A proposal below DIP0003, on the dummy index (MN0-2b-1): MNHF's one write
                    // is skipped under `fJustCheck` and no transaction reaches its checks (D51).
                    let dummy = DummyIndex::new(cx, &m.block, prev).expect("an indexed parent");
                    let (pcx, proposal) = (proposal_cx(&dummy, cx), dummy.at());
                    let facade = attempt_facade(cx.params);
                    let checked = connect_on(
                        &mut evo, &facade, vcx, &pcx, &m.block, &proposal, true, true, false,
                    )?;
                    assert_eq!(checked, Ok(Ok(false)), "a proposal at {}", m.height);
                    assert_eq!(
                        connect(&mut evo, vcx, cx, &m.block, &at, true, true)?,
                        Ok(Ok(false))
                    );
                    best.push(layer.best_block() == Some(m.hash()));
                } else {
                    let checked = connect(&mut evo, vcx, cx, &m.block, &at, true, true)?;
                    let reason = match &checked {
                        Ok(Err(SpecialTxFailure::Invalid(e))) => Some(e.reject_reason()),
                        _ => None,
                    };
                    assert_eq!(reason, Some("bad-cbtx-version"), "{checked:?}");
                    let refused = connect(&mut evo, vcx, cx, &m.block, &at, false, true)?;
                    assert_eq!(refused, Ok(Err(SpecialTxFailure::NotReady(SPECIAL_TX_CHECKS))));
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
pub(super) fn special_cbtx(version: u16, height: u32, mn_root: u8, quorum_root: u8) -> TxRef {
    let mut cb = tx(3, TRANSACTION_COINBASE, true, 0).to_mutable();
    cb = dash_primitives::Transaction::new(
        cb.version(),
        cb.tx_type(),
        cb.inputs().clone(),
        cb.outputs().clone(),
        cb.lock_time(),
        testkit::cbtx_bytes(version, height, mn_root, quorum_root),
    );
    Arc::new(Tx::new(cb))
}

/// D57 (DEC-730, BL-516): below DIP0003 `NodeEvo` over `DmlChecks` judges a special-version CbTx
/// as Core's `ProcessSpecialTxsInBlock` does (`stm:875-1029` at v24.0.0): step 1, then steps 8–10
/// over Core's default, empty `mn_list` and no mined commitment, so both roots must be zero (the
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
            // Undoing it reads no evo state below DIP0003 (`stm:1061-1094` at v24.0.0).
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

/// D9 at step 12 (`stm:1043-1048` at v24.0.0): with V19 at height 3, block 2 is processed under the
/// legacy scheme and switches to basic after it. An attempt that does not commit reverts it with
/// the guard's line and keeps legacy (`validation.cpp:164-169`); a committed one publishes basic.
#[test]
fn step_twelve_publishes_basic_only_on_commit() {
    install_accept_log();
    let p = regtest();
    let v19_at_3 = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
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
                    &mut || Ok(None),
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

/// CH3a D192 (BL-768, BL-786): a disconnect's scheme guard is `DisconnectTip`'s
/// (`validation.cpp:3283` at v24.0.0), which enters the block's scheme, so `finish_attempt` logs
/// nothing, and its exit reverts as Core's destructor does (`:162-167`) unless it committed
/// (`:3331`, after the flush): it logs the revert when the scheme the disconnect left is not the
/// one it saved, and publishes the saved one again (OD-CP-182 R5). With V19 at height 3, block 2's
/// undo enters legacy over the published basic: an uncommitted guard reverts, H1's commit or not,
/// the transaction's commit or not; an exit that does not log restores all the same; a committed
/// guard keeps the published legacy.
#[test]
fn a_disconnects_scheme_guard_exits_after_its_bracket() {
    install_accept_log();
    let p = regtest();
    let v19_at_3 = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
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
        let (mut evo, _, _) = node_evo(&p19, BlsCtx::BASIC);
        let thread = std::thread::current().id();
        let reverted = || levels_on(thread, REVERTED_BASIC);
        let entered = || levels_on(thread, ENTERED_LEGACY);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let at2 = at(&blocks[1], ids[1], ids[0]);
            let mut facts = Vec::new();
            // (H1's commit, the transaction commits, the guard commits, the exit logs, the revert
            // lines after it)
            for (n, (commit_bls, commit_tx, commit_guard, log, want)) in [
                (false, false, false, true, 1),
                (true, false, false, true, 2),
                (false, false, false, false, 2),
                (true, true, false, true, 3),
                (true, true, true, true, 3),
            ]
            .into_iter()
            .enumerate()
            {
                let mut target = EvoTarget::Active;
                let facade = attempt_facade(cx.params);
                evo.open_scheme_guard(SchemeGuardHolder::Tip);
                let mut att = evo.begin_attempt(
                    &facade,
                    vcx,
                    cx,
                    AttemptId::next(),
                    AttemptOrigin::Tip,
                    &ViewTarget::Active,
                )?;
                evo.mark_disconnect(&mut att);
                evo.begin_tx(&mut att, &mut target)?;
                let bls = evo.bls_ctx(&mut att, &mut target, &p19, &at2);
                facts.push((n, "the guard enters legacy", entered().len() == n + 1));
                if commit_bls {
                    evo.commit_bls_scheme(&mut att, &mut target);
                }
                let before = reverted().len();
                evo.finish_attempt(att)?;
                facts.push((n, "finish_attempt logs no revert", reverted().len() == before));
                if commit_tx {
                    evo.commit_tx(&mut target)?;
                } else {
                    evo.rollback_tx(&mut target);
                }
                if commit_guard {
                    evo.commit_scheme_guard();
                }
                evo.exit_scheme_guard(log);
                facts.push((n, "the revert lines", reverted().len() == want));
                let published = if commit_guard {
                    BlsCtx::LEGACY
                } else {
                    BlsCtx::BASIC
                };
                facts.push((n, "the scheme", bls == BlsCtx::LEGACY && evo.scheme() == published));
            }
            Ok(facts)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let facts = out.lock().expect("out").take().expect("V ran it");
    let failed: Vec<_> = facts.iter().filter(|(_, _, ok)| !ok).collect();
    assert!(failed.is_empty(), "{failed:?}");
    rig.assert_no_abort();
}

/// RP1a D68 (BL-701): the scheme an empty start begins from, legacy, though V19 is active at
/// height 1, is kept by a proposal of block 1, whose own rule is basic (a just-check never
/// commits), and by an attempt that does not commit; block 1's commit then publishes basic.
/// A VerifyDB scratch attempt keeps it too
/// (`a_verify_db_attempt_signals_through_its_log_and_commits_its_scratch_layer`, D61).
// Core: validation.cpp:162-167, :2894, :2927, :4979 at v24.0.0
#[test]
fn an_empty_starts_legacy_scheme_survives_attempts_that_do_not_commit() {
    let p = regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 1, 2);
    let rig = Rig::new();
    rig.headers(&blocks);
    let (id, gid) = (rig.id(&blocks[0]), rig.id(&genesis));
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, _, _) = node_evo(&p, BlsCtx::LEGACY);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut schemes = Vec::new();
            let dummy = DummyIndex::new(cx, &blocks[0].block, gid).expect("an indexed parent");
            let proposed =
                propose(&mut evo, vcx, &proposal_cx(&dummy, cx), &blocks[0].block, &dummy.at())?;
            schemes.push((proposed.is_ok(), evo.scheme()));
            let at1 = at(&blocks[0], id, gid);
            let rolled_back = connect(&mut evo, vcx, cx, &blocks[0].block, &at1, true, false)?;
            schemes.push((rolled_back == Ok(Ok(false)), evo.scheme()));
            let committed = connect(&mut evo, vcx, cx, &blocks[0].block, &at1, true, true)?;
            schemes.push((committed == Ok(Ok(false)), evo.scheme()));
            Ok(schemes)
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let schemes = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(schemes, [(true, BlsCtx::LEGACY), (true, BlsCtx::LEGACY), (true, BlsCtx::BASIC)]);
    rig.assert_no_abort();
}

/// BL-546 (CH3b D221): a failed evo snapshot write stops the node with one report (EV3 D21: store
/// faults are not consensus rejects): the engine's two lines, then one `AbortNode` with the flush
/// side's text and Core's default user message, which V's stop does not report again. A write
/// the latch then refuses reports nothing more.
// Core: dbwrapper.cpp:261-268
#[test]
fn a_failed_snapshot_write_is_reported_once() {
    use crate::evo::effects::SnapshotApplier as _;

    install_accept_log();
    let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
    engine.inject_failure(dashd_store::ErrorKind::Io);
    let sink = Arc::new(crate::chain::RecordingAbort::default());
    let latch = crate::store::GlobalFailStop::new();
    let db = Arc::new(crate::store::GuardedEngine::new(engine, latch.clone(), sink.clone()));
    let store = Arc::new(RocksEvoStore::new(db, testkit::codec(), testkit::genesis()));
    let mut applier = StoreSnapshots {
        rows: Arc::new(RuntimeRows::new(store, Arc::new(EffectLedger::default()))),
    };
    let snapshot = dash_evo::llmq::QuorumSnapshot {
        active_quorum_members: vec![true, false],
        skip_list_mode: 0,
        skip_list: vec![],
    };
    let (t, base) = (LlmqType::LlmqTestDip0024, testkit::genesis());
    let f = applier.write(t, &base, &snapshot).expect_err("the write fails");
    let latched = latch.tripped().expect("the guard latched it").clone();
    let text = format!("System error while flushing: {}", latched.message);
    assert_eq!((latched.site, f.0.as_str()), ("chainstate raw write", text.as_str()));
    assert!(f.caller_logs() && !f.is_read(), "a write, not a coins read (BL-781)");
    // `None`: Core's default user message, not the engine's text (Sol r1).
    assert_eq!(sink.reports(), [(text.clone(), None)]);
    assert!(
        f.reported_by_v(false).is_none() && f.reported_by_v(true).is_none(),
        "V's stop reports it no more"
    );
    let here = super::support::lines_on(std::thread::current().id());
    let debug = "You can use -debug=leveldb to get more complete diagnostic messages";
    super::support::assert_logged_before(&here, &latched.message, debug);
    let refused = applier.write(t, &base, &snapshot).expect_err("the latch refuses it");
    assert_eq!(refused.0, text);
    assert!(refused.caller_logs() && !refused.is_read(), "the latch's, not a coins read");
    assert_eq!(sink.reports(), [(text, None)], "reported once");
    let lines = super::support::lines_on(std::thread::current().id());
    assert_eq!(lines.iter().filter(|l| *l == debug).count(), 1, "{lines:#?}");
}

/// Mainnet's first ProUpServTx payload after DIP0003 (8e5bdc5f…, mined at 1,028,257;
/// dash-oracle-sys `contextual-mainnet.json` `proupserv`, provenance there).
const CORE_PROUPSERV_PAYLOAD: &str = "0100301ac037140fe382096b0f632f9f1ea91675fa12f5aeafead8cfb4e24a6e9aed00000000000000000000ffff258b15d1270f1976a9145434aec481a9ec2835c18b6273a49381a3f2b4ba88ac872cc9869de8887fbcbf15a386af74ae259a6fe17142b0f7c7d43dabd87e59311547b4abad19198e76d7da34e3f5d066e534944c3d658061f6a3e6327122748dd72e2a2a4f8a3cc6a7c5bcfeb4243faa01be396f5c892b7f53fbc0ed9effda2a5b177730ef24ccfd70f58345137e0371c7125d86ae05820e6670f1dd656f91e4";

/// A ProUpServTx carrying [`CORE_PROUPSERV_PAYLOAD`], for a masternode no fixture registers.
pub(super) fn core_pro_up_serv() -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::ProviderUpdateService,
        vec![TxIn::new(OutPoint::new(TxHash::from([9; 32]), 0), vec![1, 9].into(), u32::MAX)],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        0,
        unhex(CORE_PROUPSERV_PAYLOAD),
    )))
}

/// SP1c-1 A3 (DEC-782): below DIP0003 Core's `ProcessBlock` does not run
/// (`specialtxman.cpp:973`, `deterministicmns.cpp:696-698` at v24.0.0), so a block holding a
/// ProUpServTx (Core's payload) resets no platform ban, with and without the checks, as a proposal
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
    let upserv = core_pro_up_serv();
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
                for check in [false, true] {
                    let want = Ok(pre_dip3_special_txs(&body, check));
                    let got = connect(&mut evo, vcx, cx, &body, &at, check, true)?;
                    verdicts.push(got.map(|r| r.map(drop)) == want);
                }
                // The proposal, on the dummy index (MN0-2b-1).
                let dummy = DummyIndex::new(cx, &body, prev).expect("an indexed parent");
                let got = propose(&mut evo, vcx, &proposal_cx(&dummy, cx), &body, &dummy.at())?;
                verdicts.push(got == pre_dip3_special_txs(&body, true));
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

/// One proposal attempt of `block` at `at` in the connector's order (CH3 §3.1): H10, H1, H4 with
/// `fJustCheck`, then the rollback, as a just-check never commits. H4's verdict.
fn propose<C: Ev1Checks + Send>(
    evo: &mut NodeEvo<MemoryEngine, C>,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
) -> Result<Result<(), SpecialTxFailure>, Fatal> {
    let mut target = EvoTarget::Active;
    let facade = attempt_facade(cx.params);
    let mut att = evo.begin_attempt(
        &facade,
        vcx,
        cx,
        AttemptId::next(),
        AttemptOrigin::Proposal,
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
        true,
        true,
        &mut || Ok(None),
    );
    evo.finish_attempt(att)?;
    evo.rollback_tx(&mut target);
    Ok(h4?.map(drop))
}

/// BL-526's just-check half past DIP0003 (OD-CP-21): Core's `ProcessBlock`, and with it the
/// ProUpServTx's `ResetPlatformBan` (`deterministicmns.cpp:736-749` at v24.0.0), runs only when
/// `!fJustCheck` (`specialtxman.cpp:973` at v24.0.0). On dashd's masternode chain, connected
/// through the hooks to 140 with the node's EV1 checks (`DmlChecks`), a just-check of 141 passes H4
/// and hands the platform-ban applier nothing; the connect of the same block then resets the
/// EvoNode's ban at 141, so the applier is watched.
///
/// The just-check runs on the dummy index, as the connector's `TestBlockValidity` builds it
/// ([`DummyIndex`], `validation.cpp:4954-4958` at v24.0.0; MN0-2b-1), so steps 6 to 11 run under
/// `fJustCheck`.
#[test]
fn a_just_check_past_dip3_resets_no_platform_ban() {
    use super::evo_chain::{MN_CHAIN, MOCKTIME, decode, linked, recorded_params};

    let p = recorded_params();
    let genesis = Mined::genesis(&p);
    let mut blocks = linked(&genesis, decode(MN_CHAIN));
    blocks.truncate(141);
    let upserv = blocks[140]
        .block
        .txs()
        .iter()
        .find_map(|tx| match dash_dml::payload(tx) {
            Some(SpecialPayload::ProviderUpdateService(p)) => Some(*p.pro_tx_hash().as_bytes()),
            _ => None,
        })
        .expect("141's ProUpServTx");
    let evo_node = dash_evo::types::ProTxHash::from(upserv);
    let rig = Rig::with(super::support::RigConfig {
        params: Some(p.clone()),
        after_genesis: Some(MOCKTIME - i64::from(genesis.time()) + 86_400),
        ..super::support::RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let gid = rig.id(&genesis);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let resets = MemPlatformBans::default();
        let scheme = bls_ctx_for_block(&p, 1);
        let signals = ValidationSignals::default();
        let bans = Box::new(resets.clone());
        let (mut evo, _, _) = mn_chain_node_evo(&p, scheme, bans, &signals);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut prev = gid;
            for (m, &id) in blocks[..140].iter().zip(&ids) {
                let h4 = connect(&mut evo, vcx, cx, &m.block, &at(m, id, prev), true, true)?;
                assert!(matches!(h4, Ok(Ok(_))), "{}: {h4:?}", m.height);
                prev = id;
            }
            let m = &blocks[140];
            let dummy = DummyIndex::new(cx, &m.block, prev).expect("140 is indexed");
            let proposed = propose(&mut evo, vcx, &proposal_cx(&dummy, cx), &m.block, &dummy.at())?;
            assert!(dummy.take_fault().is_none(), "no question about the dummy's child");
            let at = at(m, ids[140], prev);
            let after_proposal = resets.resets();
            let connected = connect(&mut evo, vcx, cx, &m.block, &at, true, true)?;
            Ok((proposed, after_proposal, connected))
        })?;
        *seen.lock().expect("seen") = Some((r, resets.resets()));
        Ok(())
    });
    let ((proposed, after_proposal, connected), resets) =
        out.lock().expect("out").take().expect("V ran it");
    assert!(proposed.is_ok(), "the just-check passes H4: {proposed:?}");
    assert!(after_proposal.is_empty(), "a just-check resets no platform ban: {after_proposal:?}");
    assert!(matches!(connected, Ok(Ok(true))), "the connect of 141: {connected:?}");
    assert_eq!(resets, [(evo_node, 141)], "the connect resets the EvoNode's ban at 141");
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
pub(super) fn undo_block<C: Ev1Checks + Send>(
    evo: &mut NodeEvo<MemoryEngine, C>,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    parent: &BlockHash,
) -> Result<Result<bool, Fatal>, Fatal> {
    let facade = attempt_facade(cx.params);
    undo_block_on(evo, &facade, vcx, cx, block, at, parent)
}

/// [`undo_block`] in an attempt on `facade`.
pub(super) fn undo_block_on<C: Ev1Checks + Send>(
    evo: &mut NodeEvo<MemoryEngine, C>,
    facade: &MembersFacade,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    parent: &BlockHash,
) -> Result<Result<bool, Fatal>, Fatal> {
    let r = undo_change_on(evo, facade, vcx, cx, block, at, parent)?;
    Ok(r.map(|r| r.is_some()))
}

/// [`undo_block_on`] with H9's own result: `None` when it did not undo the block, else the list
/// change it raises.
fn undo_change_on<C: Ev1Checks + Send>(
    evo: &mut NodeEvo<MemoryEngine, C>,
    facade: &MembersFacade,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    parent: &BlockHash,
) -> Result<Result<Option<Option<MnListChange>>, Fatal>, Fatal> {
    let mut target = EvoTarget::Active;
    let mut att = evo.begin_attempt(
        facade,
        vcx,
        cx,
        AttemptId::next(),
        AttemptOrigin::Tip,
        &ViewTarget::Active,
    )?;
    evo.begin_tx(&mut att, &mut target)?;
    evo.bls_ctx(&mut att, &mut target, cx.params, at);
    let r = evo.undo_special_txs(&mut att, &mut target, block, at);
    let undone = matches!(r, Ok(Some(_)));
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
    Ok(r)
}

/// H9 below DIP0003 (§3.3): MNHF and the list have nothing to undo, and the quorum processor's
/// undo finds no commitment, so the block's evo tip moves back with no row erased. Core's undo
/// computes the rotated members first even below DIP0003 (`bp:523` at v24.0.0, regtest's DIP0024
/// is at height 1), unlike its connect (`bp:218-221` at v24.0.0). H9 runs that precompute through
/// the attempt's binding at every height (X13): at a rotation cycle base it computes the empty set
/// the empty list gives (D53, DEC-591).
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

/// Guard: H9 reads no list diff at a block below DIP0003, even one a run under an earlier
/// DIP0003 height stored (D23). With DIP0003 and V20 at 5, blocks 3 and 4 hold rows built under
/// DIP0003 at 3: 3 registers a masternode with a snapshot, 4 updates it. Undoing 4 raises no list
/// change and moves the evo tip back to 3. Core reads the row ungated and applies the update over
/// the empty list below DIP0003, which throws (`dmn:793-805`, `:830`, `:389` at v24.0.0).
#[test]
fn the_undo_below_a_later_dip3_reads_no_row() {
    use dash_dml::testkit::numbered;
    let dip3_at_5 = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "5:5")
        .arg(ChainOption::TestActivationHeight, "v20@5")
        .build()
        .expect("DIP0003 and V20 at 5");
    let p = Arc::new(dip3_at_5);
    let rig = Rig::with(super::support::RigConfig {
        params: Some(p.clone()),
        ..super::support::RigConfig::default()
    });
    let blocks = branch(&p, &rig.genesis, 4, 3);
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let pro_tx = numbered(0).pro_tx_hash;
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, store) = node_evo(&p, BlsCtx::BASIC);
        let mut prev = MnList::default_pre_dip3();
        for m in &blocks[2..] {
            let mut next = prev.clone().with_block_hash(m.hash()).with_height(m.height as i32);
            if m.height == 3 {
                next.try_add(numbered(0), true).expect("adds");
            } else {
                let mut hot = next.get(&pro_tx).expect("registered").hot;
                hot.last_paid_height = 4;
                next.update_hot(&pro_tx, hot).expect("updates");
            }
            let delta = EvoDelta {
                block: m.hash(),
                height: m.height,
                dml: Some(testkit::dml(&prev, &next, m.height == 3)),
                mined_commitments: Vec::new(),
                credit_data: None,
                credit_pool: None,
                mnhf_signals: None,
                erased_mined: Vec::new(),
                evo_tip: Some(m.hash()),
            };
            store.commit(&mut layer.lock(), &delta).expect("the rows of the run at 3");
            prev = next;
        }
        let r = with_tip_cx(v, |vcx, cx| {
            let facade = attempt_facade(cx.params);
            let four = at(&blocks[3], ids[3], ids[2]);
            undo_change_on(&mut evo, &facade, vcx, cx, &blocks[3].block, &four, &blocks[2].hash())
        })?;
        let tip = layer.best_block() == Some(blocks[2].hash());
        *seen.lock().expect("seen") = Some((format!("{r:?}"), tip));
        Ok(())
    });
    let (r, tip) = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(r, "Ok(Some(None))");
    assert!(tip, "the evo tip is block 3");
    rig.assert_no_abort();
}

/// Regtest's DIP0003 block is also a DIP0024 cycle base, where G1q refused the undo until
/// CH7b-2 (D70). Undoing it, its list diff stored as its connect staged it, runs Core's
/// precompute with `reset_cache` (`bp:523`): the rotated members are computed again, from the
/// empty list of its work block below DIP0003. With fewer enabled masternodes than a quarter,
/// `BuildNewQuorumQuarterMembers` returns before it stores a snapshot (`llmq/utils.cpp:362-363`),
/// so no row is written. The evo tip moves back to the parent.
#[test]
fn the_undo_at_dip3_on_a_rotation_cycle_base_undoes_as_core() {
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
        let row = || store.quorum_snapshot(LlmqType::LlmqTestDip0024, &hash).map(|s| s.is_some());
        let before = row().map_err(|e| Fatal::new(e.to_string()))?;
        let r = with_tip_cx(v, |vcx, cx| undo(&mut evo, vcx, cx, &blocks, &ids, n))?;
        let snapshot = (before, row().map_err(|e| Fatal::new(e.to_string()))?);
        let tip = layer.best_block() == Some(blocks[n - 1].hash());
        *seen.lock().expect("seen") = Some((r.map_err(|e| e.to_string()), snapshot, tip));
        Ok(())
    });
    let (r, snapshot, tip) = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(r, Ok(true));
    assert_eq!(snapshot, (false, false), "no snapshot row for the cycle base");
    assert!(tip, "the evo tip is the parent");
    rig.assert_no_abort();
}

/// OD-CP-51 R2-N1: undoing a block whose stored `mn_diff` row has changes but another block's
/// height is the walk's corruption fail-stop (`StoreError::Inconsistent`, A1 I5), not an undo:
/// the row is checked against the block before it is applied. Core's `ApplyDiff(pindex, diff)`
/// takes the height from `pindex`, so the check adds only that fail-stop. The evo tip stays.
/// M12's killer.
#[test]
fn an_undo_over_a_diff_row_at_another_height_is_a_fault() {
    use dash_dml::testkit::numbered;
    let p = regtest();
    let dip3 = p.consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
    let n = usize::try_from(dip3).unwrap() - 1;
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, dip3, 3);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let damaged = blocks[n].hash();
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, store) = node_evo(&p, BlsCtx::BASIC);
        let hash = blocks[n].hash();
        // The row under the block's hash, written by a delta that says the next height: the
        // store checks a delta against itself, not against the chain.
        let wrong = dip3 + 1;
        let mut list = MnList::new(hash, i32::try_from(wrong).unwrap(), 0);
        list.try_add(numbered(0), true).expect("adds");
        let dml = testkit::dml(&MnList::default_pre_dip3(), &list, false);
        assert!(dml.record.diff.has_changes(), "a diff with changes");
        let delta = EvoDelta {
            block: hash,
            height: wrong,
            dml: Some(dml),
            mined_commitments: Vec::new(),
            credit_data: None,
            credit_pool: None,
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: Some(hash),
        };
        store.commit(&mut layer.lock(), &delta).expect("the damaged row");
        let r = with_tip_cx(v, |vcx, cx| undo(&mut evo, vcx, cx, &blocks, &ids, n))?;
        let tip = layer.best_block() == Some(hash);
        *seen.lock().expect("seen") = Some((r.map_err(|e| e.to_string()), tip));
        Ok(())
    });
    let (r, tip) = out.lock().expect("out").take().expect("V ran it");
    let want =
        format!("mn_diff row for block {} has height {}, the block {dip3}", damaged, dip3 + 1);
    let Err(got) = r else {
        panic!("undone: {r:?}")
    };
    assert!(got.contains("UndoBlock: Found EvoDB inconsistency"), "{got}");
    assert!(got.contains(&want), "{got}");
    assert!(tip, "the evo tip stays at the block");
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
    match tx.payload().expect("special").expect("decodes").payload() {
        SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.clone(),
        other => panic!("{other:?}"),
    }
}

/// The fixture's coinbase at `height` with best-ChainLock difference `diff`: the payload is
/// version(2) height(4) roots(64) diff(1) sig(96) balance(8).
pub(super) fn coinbase_at(height: i32, diff: u8) -> TxRef {
    let mut raw = coinbase().to_mutable();
    {
        let mut extra_payload = raw.extra_payload().clone();
        extra_payload[2..6].copy_from_slice(&height.to_le_bytes());
        raw = dash_primitives::Transaction::new(
            raw.version(),
            raw.tx_type(),
            raw.inputs().clone(),
            raw.outputs().clone(),
            raw.lock_time(),
            extra_payload,
        );
    }
    {
        let mut extra_payload = raw.extra_payload().clone();
        extra_payload[70] = diff;
        raw = dash_primitives::Transaction::new(
            raw.version(),
            raw.tx_type(),
            raw.inputs().clone(),
            raw.outputs().clone(),
            raw.lock_time(),
            extra_payload,
        );
    }
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
        _: &mut ClAttempt<'_, '_>,
        height: u32,
        block_hash: &BlockHash,
        _: &BlsSigValue,
        _: BlsCtx,
    ) -> Result<Result<bool, StoreError>, Fatal> {
        self.asked.lock().expect("asked").push((height, *block_hash));
        Ok((self.answer)())
    }
}

/// D45: step 10 with the best ChainLock null and its cache missing. The previous CbTx's ChainLock
/// (read from the parent's block, V20 at the parent) bounds this one; a non-null signature in
/// range goes to the verifier (since CH7b-3 C5 the node's verifies), whose answer is the
/// verdict, whose `NotBuilt` is D2's refusal and whose fault is `Fatal`.
#[test]
fn step_ten_with_no_best_chainlock() {
    let p = regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 5, 4);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let (block3, block4) = (blocks[2].hash(), blocks[3].hash());
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let r = with_tip_cx(v, |_, cx| {
            let reason = |r: Result<Result<ClPass, SpecialTxFailure>, Fatal>| match r {
                Ok(Ok(_)) => "ok".to_owned(),
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
                let cell = parking_lot::Mutex::default();
                let step = ClStep {
                    height: 5,
                    id: ids[4],
                    prev: ids[3],
                    v20_at_prev,
                    prev_block: &read,
                    bls: BlsCtx::BASIC,
                    cell: &cell,
                };
                let mut attempt = ClAttempt::detached(ids[3]);
                let mut no_best = || Ok(None);
                reason(check_cbtx_best_chainlock(cbtx, &step, cx, &mut attempt, &mut no_best, v))
            };
            let run = |cbtx: &CoinbaseCommitment, parent: Option<u8>, v: &dyn ClVerifier| {
                run_at(cbtx, parent, v, true)
            };
            let base = cbtx_of(&coinbase());
            let with = |diff: u64, signed: bool, version: u16| {
                let mut c = base.clone();
                c = dash_primitives::CoinbaseCommitment::new(
                    c.version(),
                    c.height(),
                    c.merkle_root_mn_list(),
                    c.merkle_root_quorums(),
                    Some(diff),
                    c.best_cl_signature().clone(),
                    c.credit_pool_balance(),
                    c.merkle_root_asset_unlocks(),
                );
                if !signed {
                    c = dash_primitives::CoinbaseCommitment::new(
                        c.version(),
                        c.height(),
                        c.merkle_root_mn_list(),
                        c.merkle_root_quorums(),
                        c.best_cl_height_diff(),
                        None,
                        c.credit_pool_balance(),
                        c.merkle_root_asset_unlocks(),
                    );
                }
                c = dash_primitives::CoinbaseCommitment::new(
                    dash_primitives::CbCommitmentVersion::new(version),
                    c.height(),
                    c.merkle_root_mn_list(),
                    c.merkle_root_quorums(),
                    c.best_cl_height_diff(),
                    c.best_cl_signature().clone(),
                    c.credit_pool_balance(),
                    c.merkle_root_asset_unlocks(),
                );
                c
            };
            let asked = |answer| Answer {
                answer,
                asked: Mutex::new(Vec::new()),
            };
            // The arms before the verify never ask it.
            let unasked = asked(|| Ok(true));
            let (yes, no) = (asked(|| Ok(true)), asked(|| Ok(false)));
            let not_built = asked(|| Err(StoreError::NotBuilt("a store")));
            let broken = asked(|| Err(StoreError::inconsistent("a broken read")));
            let out = vec![
                // Below CLSIG_AND_BALANCE nothing is read.
                run(&with(9, false, 2), Some(0), &unasked),
                // No ChainLock at the parent (none read): null with 0 passes, with 1 does not.
                run(&with(0, false, 3), None, &unasked),
                run(&with(1, false, 3), None, &unasked),
                // The parent's ChainLock: a null one now, then one more than a block older.
                run(&with(0, false, 3), Some(0), &unasked),
                // Without V20 at the parent its ChainLock is not read (`cbtx:187-189`).
                run_at(&with(0, false, 3), Some(0), &unasked, false),
                run(&with(2, true, 3), Some(0), &unasked),
                // In range, to the verifier.
                run(&with(1, true, 3), Some(0), &yes),
                // Out of range: the difference reaches below genesis.
                run(&with(5, true, 3), None, &unasked),
                // No parent ChainLock: the verifier's answer, D2's refusal, a fault.
                run(&with(0, true, 3), None, &no),
                run(&with(0, true, 3), None, &yes),
                run(&with(0, true, 3), None, &not_built),
                run(&with(0, true, 3), None, &broken),
            ];
            assert!(unasked.asked.lock().expect("asked").is_empty(), "the verify is never asked");
            let requests = yes.asked.lock().expect("asked").clone();
            (out, requests)
        });
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let (out, requests) = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(
        out,
        [
            "ok",
            "ok",
            "bad-cbtx-cldiff",
            "bad-cbtx-null-clsig",
            "ok",
            "bad-cbtx-older-clsig",
            "ok",
            "bad-cbtx-cldiff",
            "bad-cbtx-invalid-clsig",
            "ok",
            "not ready: a store",
            "fatal",
        ]
    );
    // At height 5, the ChainLock of difference 1 locks block 3, of difference 0 block 4.
    assert_eq!(requests, [(3, block3), (4, block4)]);
    rig.assert_no_abort();
}

/// What `cl_best` answers in a [`TenCase`].
#[derive(Clone, Copy)]
enum Best {
    /// Core's null sig.
    Null,
    /// A lock at the height, with the signature.
    At(i32, [u8; 96]),
    /// A fault (a recorded replay's miss).
    Fails,
}

/// Step 10 at height 5 on the regtest branch's blocks 1 to 4, V20 at the parent (CH7b-3 C2).
struct TenCase {
    cbtx: CoinbaseCommitment,
    /// The parent's CbTx difference, or an unreadable parent.
    parent: Option<u8>,
    best: Best,
}

/// What one [`TenCase`] gave.
#[derive(Debug, PartialEq, Eq)]
struct TenRun {
    /// The pass, or the reject reason.
    out: Result<ClPass, String>,
    /// `cl_best`'s asks.
    asks: usize,
    /// Reads of the parent's block.
    reads: usize,
    /// The verifies, `(height, cl_height)`, each answered valid.
    verified: Vec<(u32, u32)>,
}

fn run_step_ten(cases: Vec<TenCase>) -> Vec<TenRun> {
    let p = regtest();
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 5, 4);
    let rig = Rig::new();
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let runs = with_tip_cx(v, |_, cx| {
            let run = |case: TenCase| {
                let header = Header::new(BlockHeader::default());
                let parent = case
                    .parent
                    .map(|diff| Arc::new(Block::new(header, vec![coinbase_at(4, diff)])));
                let reads = Cell::new(0);
                let read = || {
                    reads.set(reads.get() + 1);
                    parent.clone()
                };
                let cell = parking_lot::Mutex::default();
                let step = ClStep {
                    height: 5,
                    id: ids[4],
                    prev: ids[3],
                    v20_at_prev: true,
                    prev_block: &read,
                    bls: BlsCtx::BASIC,
                    cell: &cell,
                };
                let asks = Cell::new(0);
                let mut cl_best = || {
                    asks.set(asks.get() + 1);
                    match case.best {
                        Best::Null => Ok(None),
                        Best::At(height, sig) => Ok(Some(Arc::new(ChainLockSig {
                            height,
                            block_hash: BlockHash::from([7; 32]),
                            sig,
                        }))),
                        Best::Fails => Err(Fatal::new("a decision miss")),
                    }
                };
                let verifier = Answer {
                    answer: || Ok(true),
                    asked: Mutex::new(Vec::new()),
                };
                let mut attempt = ClAttempt::detached(ids[3]);
                let r = check_cbtx_best_chainlock(
                    &case.cbtx,
                    &step,
                    cx,
                    &mut attempt,
                    &mut cl_best,
                    &verifier,
                );
                let out = match r {
                    Ok(Ok(pass)) => Ok(pass),
                    Ok(Err(SpecialTxFailure::Invalid(err))) => Err(err.reject_reason().to_owned()),
                    Ok(Err(SpecialTxFailure::NotReady(c))) => Err(format!("not ready: {c}")),
                    Err(_) => Err("fatal".to_owned()),
                };
                let asked = verifier.asked.into_inner().expect("asked");
                TenRun {
                    out,
                    asks: asks.get(),
                    reads: reads.get(),
                    verified: asked.into_iter().map(|(cl_height, _)| (5, cl_height)).collect(),
                }
            };
            cases.into_iter().map(run).collect::<Vec<_>>()
        });
        *seen.lock().expect("seen") = Some(runs);
        Ok(())
    });
    rig.assert_no_abort();
    out.lock().expect("out").take().expect("V ran it")
}

/// The fixture's best-ChainLock signature, valid under the basic scheme.
fn fixture_sig() -> [u8; 96] {
    cbtx_of(&coinbase()).best_cl_signature().as_ref().expect("signed").to_bytes()
}

/// Another signature valid under the basic scheme.
fn other_sig() -> [u8; 96] {
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[3; 32]).expect("a key");
    key.sign_message(b"another lock").to_bytes()
}

/// The fixture's version-3 CbTx with difference `diff` and signature `sig`, `None` null.
fn cbtx_with(diff: u64, sig: Option<[u8; 96]>) -> CoinbaseCommitment {
    let mut c = cbtx_of(&coinbase());
    c = dash_primitives::CoinbaseCommitment::new(
        c.version(),
        c.height(),
        c.merkle_root_mn_list(),
        c.merkle_root_quorums(),
        Some(diff),
        c.best_cl_signature().clone(),
        c.credit_pool_balance(),
        c.merkle_root_asset_unlocks(),
    );
    c = dash_primitives::CoinbaseCommitment::new(
        c.version(),
        c.height(),
        c.merkle_root_mn_list(),
        c.merkle_root_quorums(),
        c.best_cl_height_diff(),
        sig.map(|s| BlsGlobalSig::decode(&s).expect("readable")),
        c.credit_pool_balance(),
        c.merkle_root_asset_unlocks(),
    );
    c
}

/// A run that passed, asking `cl_best` once.
fn passed(out: ClPass, reads: usize, verified: Vec<(u32, u32)>) -> TenRun {
    TenRun {
        out: Ok(out),
        asks: 1,
        reads,
        verified,
    }
}

/// CH7b-3 C2, shortcut 1 (`stm:152-158`): a CbTx of difference 0 whose signature is the best
/// ChainLock's, at the parent's height, passes before the parent's block is read and with no
/// verify, its parent ChainLock read or not.
#[test]
fn shortcut_1_takes_the_best_at_h_minus_1() {
    let s = fixture_sig();
    let runs = run_step_ten(vec![
        TenCase {
            cbtx: cbtx_with(0, Some(s)),
            parent: Some(0),
            best: Best::At(4, s),
        },
        TenCase {
            cbtx: cbtx_with(0, Some(s)),
            parent: None,
            best: Best::At(4, s),
        },
    ]);
    let taken = || passed(ClPass::BestAtParent, 0, vec![]);
    assert_eq!(runs, [taken(), taken()]);
}

/// CH7b-3 C2 (B-1 to B-3): shortcut 1 needs the best ChainLock at the parent, difference 0 and
/// the same signature; without any one of them the CbTx is verified.
#[test]
fn shortcut_1_needs_each_of_its_three_conditions() {
    let (s, t) = (fixture_sig(), other_sig());
    let case = |diff, best| TenCase {
        cbtx: cbtx_with(diff, Some(s)),
        parent: Some(0),
        best,
    };
    let runs = run_step_ten(vec![
        case(0, Best::At(4, s)),
        // The height: one below and one above the parent's.
        case(0, Best::At(3, s)),
        case(0, Best::At(5, s)),
        // The difference: the lock at the parent is not this CbTx's (it locks 3).
        case(1, Best::At(4, s)),
        // The signature.
        case(0, Best::At(4, t)),
    ]);
    let checked = |cl_height| passed(ClPass::Verified, 1, vec![(5, cl_height)]);
    assert_eq!(
        runs,
        [passed(ClPass::BestAtParent, 0, vec![]), checked(4), checked(4), checked(3), checked(4),]
    );
}

/// CH7b-3 C2, shortcut 2 (`stm:187-193`): a CbTx whose ChainLock is the best one, older than the
/// parent, passes with no verify, after the parent's ChainLock has bounded it (`:169-178`).
#[test]
fn shortcut_2_takes_the_outdated_best() {
    let s = fixture_sig();
    let case = |diff, parent, best| TenCase {
        cbtx: cbtx_with(diff, Some(s)),
        parent,
        best,
    };
    let runs = run_step_ten(vec![
        case(1, Some(0), Best::At(3, s)),
        case(2, Some(1), Best::At(2, s)),
        case(3, None, Best::At(1, s)),
        // The bound comes first: three is more than one block older than the parent's 0.
        case(3, Some(0), Best::At(1, s)),
    ]);
    let taken = || passed(ClPass::OutdatedBest, 1, vec![]);
    let older = TenRun {
        out: Err("bad-cbtx-older-clsig".to_owned()),
        asks: 1,
        reads: 1,
        verified: vec![],
    };
    assert_eq!(runs, [taken(), taken(), taken(), older]);
}

/// CH7b-3 C2 (B-4): shortcut 2 needs the best ChainLock at the CbTx's ChainLock height, not one
/// off it, and the same signature.
#[test]
fn shortcut_2_needs_height_and_sig() {
    let (s, t) = (fixture_sig(), other_sig());
    let case = |best| TenCase {
        cbtx: cbtx_with(2, Some(s)),
        parent: Some(1),
        best,
    };
    let runs = run_step_ten(vec![
        case(Best::At(2, s)),
        case(Best::At(3, s)),
        case(Best::At(1, s)),
        case(Best::At(2, t)),
    ]);
    let checked = || passed(ClPass::Verified, 1, vec![(5, 2)]);
    assert_eq!(runs, [passed(ClPass::OutdatedBest, 1, vec![]), checked(), checked(), checked()]);
}

/// CH7b-3 C2 (X2): `cl_best` is asked once per CbTx past the version gate (`stm:143-151`), and
/// never below it, so a fault there reaches no block below version 3.
#[test]
fn cl_best_is_asked_only_past_the_version_gate() {
    let s = fixture_sig();
    let mut v2 = cbtx_with(9, None);
    v2 = dash_primitives::CoinbaseCommitment::new(
        dash_primitives::CbCommitmentVersion::new(2),
        v2.height(),
        v2.merkle_root_mn_list(),
        v2.merkle_root_quorums(),
        v2.best_cl_height_diff(),
        v2.best_cl_signature().clone(),
        v2.credit_pool_balance(),
        v2.merkle_root_asset_unlocks(),
    );
    let case = |cbtx: &CoinbaseCommitment, best| TenCase {
        cbtx: cbtx.clone(),
        parent: Some(0),
        best,
    };
    let v3 = cbtx_with(0, Some(s));
    let null = cbtx_with(0, None);
    let runs = run_step_ten(vec![
        case(&v2, Best::Fails),
        case(&v2, Best::At(4, s)),
        case(&v3, Best::Fails),
        case(&null, Best::Null),
        case(&v3, Best::Null),
    ]);
    let below = || TenRun {
        out: Ok(ClPass::BelowVersion),
        asks: 0,
        reads: 0,
        verified: vec![],
    };
    let fatal = TenRun {
        out: Err("fatal".to_owned()),
        asks: 1,
        reads: 0,
        verified: vec![],
    };
    let null_after_a_lock = TenRun {
        out: Err("bad-cbtx-null-clsig".to_owned()),
        asks: 1,
        reads: 1,
        verified: vec![],
    };
    assert_eq!(
        runs,
        [below(), below(), fatal, null_after_a_lock, passed(ClPass::Verified, 1, vec![(5, 4)])]
    );
    let asks: usize = runs.iter().map(|r| r.asks).sum();
    assert_eq!(asks, 3, "one ask per version-3 CbTx");
}

/// CH7b-3 C2 (D45's regime, X6): with no best ChainLock no shortcut is taken, and every non-null
/// signature in range is verified at its ChainLock height, in order.
#[test]
fn no_best_chainlock_verifies_every_non_null_sig() {
    let s = fixture_sig();
    let case = |diff, sig, parent| TenCase {
        cbtx: cbtx_with(diff, sig),
        parent,
        best: Best::Null,
    };
    let runs = run_step_ten(vec![
        case(0, Some(s), None),
        case(0, Some(s), Some(0)),
        case(1, Some(s), Some(0)),
        case(2, Some(s), Some(1)),
        case(4, Some(s), None),
        case(0, None, None),
    ]);
    let census: Vec<(u32, u32)> = runs.iter().flat_map(|r| r.verified.clone()).collect();
    assert_eq!(census, [(5, 4), (5, 4), (5, 3), (5, 2), (5, 0)]);
    let outs: Vec<_> = runs.iter().map(|r| r.out.clone()).collect();
    let ok = Ok(ClPass::Verified);
    assert_eq!(outs, [ok.clone(), ok.clone(), ok.clone(), ok.clone(), ok, Ok(ClPass::Null)]);
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

/// D3's table, which G1q refused at H4's entry until CH7b-2, through `NodeEvo` over `DmlChecks`
/// on regtest with DIP0003 at 30 (DIP0024 from height 1, so 48 is the first rotation cycle
/// base). Each block now gets Core's verdict from the step that judges it, in block order:
/// - the quorum `[7; 32]` names no block, so a commitment is `bad-qc-quorum-hash`
///   (`llmq/commitment.cpp:282-285`), an MNHF signal `bad-mnhf-quorum-hash` (`mnhftx.cpp:128-131`,
///   BL-574) and an asset unlock `bad-assetunlock-quorum-hash` (`assetlocktx.cpp:218-220`):
///   step 4 binds every transaction's seams, types 7 and 9 included (D4);
/// - the versions Core rejects are `bad-qc-version` and `bad-mnhf-version`, and a registration
///   step 4 rejects ahead of a commitment is the registration's reject;
/// - with the CbTx checks, a classical coinbase is `bad-cbtx-version` (`specialtxman.cpp:894-898`
///   at v24.0.0) and a CbTx of the wrong height `bad-cbtx-height` (`cbtx.cpp:30-32`), at a cycle
///   base too;
/// - an empty block at the cycle base 48 connects: step 6's precompute reads the rotated members
///   over the stored lists.
///
/// Decode first still holds: a special payload that does not decode completely is its step's
/// `bad-*-payload`, whatever follows it.
#[test]
fn h4_gives_cores_verdicts_where_g1q_refused() {
    use super::payloads::{
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
        // The lists from DIP0003 to 47, Core's empty one at each, as connects store them: the
        // parents' lists, and the rotated members' at 48 read the list of its work block, 40.
        for parent in &blocks[DIP3 as usize - 1..47] {
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
                    Ok(Ok(_)) => "connects".to_owned(),
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
                {
                    let mut extra_payload = raw.extra_payload().clone();
                    extra_payload[0] = version;
                    raw = dash_primitives::Transaction::new(
                        raw.version(),
                        raw.tx_type(),
                        raw.inputs().clone(),
                        raw.outputs().clone(),
                        raw.lock_time(),
                        extra_payload,
                    );
                }
                Arc::new(Tx::new(raw))
            };
            // G1q's arms (a) and (b), and null commitments.
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
            // D3's interim differences: a registration step 4 rejects ahead of a commitment,
            // versions Core rejects, arm (c) alone, a step-1 reject at a cycle base.
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
    let want = [
        // Arms (a) and (b), null commitments.
        "bad-qc-quorum-hash",
        "bad-cbtx-version",
        "bad-mnhf-quorum-hash",
        "bad-assetunlock-quorum-hash",
        "bad-qc-quorum-hash",
        // Decode first.
        "bad-qc-payload",
        "bad-cbtx-payload",
        "bad-protx-payload",
        "bad-qc-quorum-hash",
        "bad-mnhf-quorum-hash",
        "bad-mnhf-payload",
        "bad-assetunlock-quorum-hash",
        // The interim differences.
        "bad-protx-netinfo-port",
        "bad-protx-netinfo-port",
        "bad-mnhf-version",
        "bad-qc-version",
        "connects",
        "bad-cbtx-height",
        "bad-cbtx-payload",
    ];
    assert_eq!(verdicts, want);
    rig.assert_no_abort();
}

/// BL-650 (OD-CP-51 F3): with fresh caches and no list rows, a connect of block 46 is the first
/// walk to reach its parent 45, which has no diff past DIP0003: the walk takes 45 as Core's
/// initial snapshot, and only then does the connect ask whether its parent is one, so 46 writes a
/// snapshot row with its diff, as Core's `GetListForBlockInternal(pprev)` comes before its check
/// (rc.5 `deterministicmns.cpp:710`, `:722`). 46's list stands on the made-up one, so its commit
/// offers it to no shared cache (OD-CP-51). 46 is outside both test quorums' mining windows, so an
/// empty block connects. M11's killer.
#[test]
fn a_connect_over_a_row_less_parent_writes_its_snapshot() {
    use crate::evo::codec::EvoCf;
    use dash_dml::store::ListCache as _;
    let p = Arc::new(super::payloads::regtest(&[]));
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 46, 0);
    let rig = Rig::with(super::support::RigConfig {
        params: Some(p.clone()),
        ..super::support::RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let (h45, h46) = (blocks[44].hash(), blocks[45].hash());
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, layer, _) = node_evo_with(&p, BlsCtx::BASIC, DmlChecks);
        assert!(!evo.list_cache().is_initial_snapshot(&h45), "fresh caches");
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let body = Block::new(Header::new(BlockHeader::default()), vec![tx(1, 0, true, 0)]);
            let at46 = at(&blocks[45], ids[45], ids[44]);
            let got = connect(&mut evo, vcx, cx, &body, &at46, false, true)?;
            let index = layer.lock().indexes().0;
            let row = |kind| index.get(kind, &h46).is_some();
            let lists = evo.list_cache();
            Ok((
                matches!(got, Ok(Ok(_))).then_some("connects").ok_or(format!("{got:?}")),
                row(EvoCf::MnDiff),
                row(EvoCf::MnSnapshot),
                lists.is_initial_snapshot(&h45),
                lists.get(&h46).is_some(),
            ))
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let got = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(
        got,
        (Ok("connects"), true, true, true, false),
        "(H4, 46's diff, 46's snapshot, 45 in the set, 46's list cached)"
    );
    rig.assert_no_abort();
}

/// H11 over [`a_connect_over_a_row_less_parent_writes_its_snapshot`]'s chain: 46's list stands on
/// 45's initial snapshot and enters no shared cache, yet it is still the block's list, the one
/// Core's `ProcessBlock` builds over its own initial snapshot, so `mnl` is its hash: an empty list
/// at 46 with counter 0. A record of such a block fails nothing.
#[test]
fn h11_over_a_row_less_parent_hashes_the_list_built_on_it() {
    use dash_dml::store::ListCache as _;
    let p = Arc::new(super::payloads::regtest(&[]));
    let genesis = Mined::genesis(&p);
    let blocks = branch(&p, &genesis, 46, 0);
    let rig = Rig::with(super::support::RigConfig {
        params: Some(p.clone()),
        ..super::support::RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let h46 = blocks[45].hash();
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let (mut evo, _layer, _) = node_evo_with(&p, BlsCtx::BASIC, DmlChecks);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let body = Block::new(Header::new(BlockHeader::default()), vec![tx(1, 0, true, 0)]);
            let at46 = at(&blocks[45], ids[45], ids[44]);
            let (h4, fields) =
                connect_digest(&mut evo, vcx, cx, &body, &at46, &ChainCoins::default(), false)?;
            let mnl = fields.map(|f| f.map(|f| f.mnl).map_err(|e| e.0));
            let h4 = matches!(h4, Ok(Ok(_))).then_some("connects").ok_or(format!("{h4:?}"));
            Ok((h4, mnl, evo.list_cache().get(&h46).is_some()))
        })?;
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let want = dash_dml::codec::canonical_mn_list_hash(&MnList::new(h46, 46, 0));
    let got = out.lock().expect("out").take().expect("V ran it");
    assert_eq!(
        got,
        (Ok("connects"), Some(Ok(Some(want))), false),
        "(H4, H11's mnl, 46's list cached)"
    );
    rig.assert_no_abort();
}

/// `qc` in a type-6 transaction for the block at `height` (`CFinalCommitmentTxPayload`).
pub(super) fn qc_tx(height: u32, qc: &LlmqCommitment) -> TxRef {
    let mut payload = Vec::new();
    payload.extend(1u16.to_le_bytes());
    payload.extend(height.to_le_bytes());
    write_commitment(qc, &mut payload);
    super::payloads::special(TRANSACTION_QUORUM_COMMITMENT, payload)
}

/// The miner's commitments for the block at `h` on `prev`, the evo store's at `layer`:
/// `GetMineableCommitments` with an empty pool, so a null one for each type whose window is
/// open.
pub(super) fn null_commitments(
    cx: &ConnectCx<'_>,
    store: &Store,
    layer: &SharedEvoLayer,
    prev: BlockId,
    h: u32,
) -> Vec<LlmqCommitment> {
    let chain = Ancestry(cx.reader);
    let complete = CompleteChain::<StoreError>::new(&chain);
    let bls = bls_ctx_for_block(cx.params, h);
    let llmq = LlmqCtx::new(cx.params, &complete, cx.deployments, bls).expect("in domain");
    let mined = store.view(&layer.lock(), &chain);
    let qc = QcCtx {
        llmq,
        index: &chain,
        mined: &mined,
        members: &Unasked,
        precompute: &Unasked,
        bls_checks: &SerialBlsChecks,
    };
    let pool = MineablePool::new();
    let enabled = enabled_quorum_params(&llmq, prev).expect("enabled types");
    let each = enabled.into_iter().map(|(llmq_type, params)| {
        pool.mineable_commitments(&qc, llmq_type, params, prev, h).expect("mineable")
    });
    each.flatten().flatten().collect()
}

/// The miner's member source for null commitments: `GetMineableCommitments` with an empty pool
/// reads no members and no precompute.
struct Unasked;

impl QuorumMemberSource for Unasked {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        unreachable!("a null commitment's miner reads no members")
    }
}

impl QuorumPrecompute for Unasked {
    type Error = StoreError;

    fn precompute(
        &self,
        _: BlockId,
        _: PrecomputeMode,
        _: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        unreachable!("a null commitment's miner runs no precompute")
    }
}

/// DEC-990: H4 binds type 6's seams, so a block's null commitments get `CheckLLMQCommitment`. On
/// regtest with DIP0003 at 30, at the first height past it whose block needs commitments, the
/// block of the null ones the miner builds (`GetMineableCommitments` with an empty pool)
/// connects; without them it misses them (step 6), and with one whose bitset is not the quorum's
/// size it gets Core's `bad-qc-invalid-null` from step 4, ahead of step 6's same check. A non-null
/// one with no valid member gets `Verify`'s `bad-qc-invalid` from step 4 before its member read,
/// where G1q refused it until CH7b-2. A caller that binds no seams still gets the refusal
/// (`ev1_tests`).
#[test]
fn null_commitments_connect_through_type_6s_seams() {
    use super::payloads::{DIP3, regtest as dip3_at_30};

    let p = Arc::new(dip3_at_30(&[]));
    let genesis = Mined::genesis(&p);
    // Short of 48, the first rotation cycle base.
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
            // The miner's commitments for the block at `h`: null ones, its pool being empty.
            let mineable = |h: u32| null_commitments(cx, &store, &layer, ids[h as usize - 2], h);
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
                    // Step 4's reject carries its transaction (`stm:933-934` at v24.0.0).
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
            let mut data = wrong_size[0].signers().data().clone();
            data.push(0);
            let signers =
                dash_primitives::DynBitset::new(wrong_size[0].signers().num_bits() + 8, data)
                    .expect("8 more zero bits");
            wrong_size[0] = dash_primitives::Commitment::new(
                wrong_size[0].version(),
                wrong_size[0].llmq_type(),
                wrong_size[0].quorum_hash(),
                wrong_size[0].quorum_index(),
                signers,
                wrong_size[0].valid_members().clone(),
                wrong_size[0].quorum_public_key().clone(),
                wrong_size[0].quorum_vvec_hash(),
                wrong_size[0].quorum_sig().clone(),
                wrong_size[0].members_sig().clone(),
            );
            assert!(wrong_size[0].is_null(), "still null");
            // No valid member: `Verify` rejects it before it reads the members.
            let mut non_null = qcs.clone();
            non_null[0] = dash_primitives::Commitment::new(
                non_null[0].version(),
                non_null[0].llmq_type(),
                non_null[0].quorum_hash(),
                non_null[0].quorum_index(),
                non_null[0].signers().clone(),
                non_null[0].valid_members().clone(),
                non_null[0].quorum_public_key().clone(),
                QuorumVvecHash::from([1; 32]),
                non_null[0].quorum_sig().clone(),
                non_null[0].members_sig().clone(),
            );
            let undecodable = super::payloads::truncated(&qc_tx(h, &qcs[0]));
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
    let want = [
        "connects",
        "bad-qc-missing",
        "step 4: bad-qc-invalid-null",
        "step 4: bad-qc-invalid",
        "step 4: bad-qc-invalid",
    ];
    assert_eq!(verdicts, want);
    rig.assert_no_abort();
}

/// The chain's unspent outputs, as the connected blocks left them: what H4's collateral checks
/// read from the first check that reads a coin on, the ProUpRegTx at 161 on dashd's masternode
/// chain (its payee-vs-collateral check).
#[derive(Default)]
struct ChainCoins(std::collections::HashMap<OutPoint, (TxOut, u32, bool)>);

impl ChainCoins {
    /// `block`'s spends and outputs at `height`, unspendable ones included (no test reads them).
    fn connect(&mut self, block: &Block, height: u32) {
        for tx in block.txs() {
            let coinbase = tx.is_coinbase();
            if !coinbase {
                for input in tx.inputs() {
                    self.0.remove(&input.prevout());
                }
            }
            let txid = tx.txid();
            for (index, out) in tx.outputs().iter().enumerate() {
                let index = u32::try_from(index).expect("an index");
                self.0.insert(OutPoint::new(txid, index), (out.clone(), height, coinbase));
            }
        }
    }
}

impl CoinsView for ChainCoins {
    type Error = CoinsReadError;

    fn coin(&self, op: &OutPoint) -> Result<Option<CoinRef<'_>>, CoinsReadError> {
        Ok(self.0.get(op).map(|(out, h, cb)| CoinRef::from_output(out, *h, *cb)))
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// One Active connect attempt as [`connect`] runs it, with hook H11 called where the connector
/// calls it (CH7a-5b X1): after H4 passed and before `finish_attempt`; H4 reads `coins`, and
/// checks the CbTx's merkle roots when `check`. H4's answer, and H11's when H4 passed.
fn connect_digest<C: Ev1Checks + Send>(
    evo: &mut NodeEvo<MemoryEngine, C>,
    vcx: &mut VContext,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    coins: &ChainCoins,
    check: bool,
) -> Result<(H4, Option<Result<EvoDigest, Fatal>>), Fatal> {
    let facade = attempt_facade(cx.params);
    let mut target = EvoTarget::Active;
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
        coins,
        0,
        false,
        check,
        &mut || Ok(None),
    );
    let passed = matches!(h4, Ok(Ok(_)));
    let fields = passed.then(|| evo.digest_fields(&mut att, &mut target, at));
    if passed {
        evo.set_best_block(&mut att, &mut target, &at.hash)?;
        evo.commit_bls_scheme(&mut att, &mut target);
    }
    evo.finish_attempt(att)?;
    if passed {
        evo.commit_tx(&mut target)?;
    } else {
        evo.rollback_tx(&mut target);
    }
    Ok((h4.map(|r| r.map(|updates| updates.is_some())), fields))
}

/// One block of dashd's masternode chain through [`connect_digest`]: its height, H11's fields
/// and the list the hooks committed for it, as V's reads serve it.
struct Digested {
    height: u32,
    fields: EvoDigest,
    list: Arc<MnList>,
}

/// dashd's masternode chain (`tests/data/ch7a5`), blocks 1 to `to`, each connected through
/// the hooks with the node's EV1 checks over the chain's coins ([`ChainCoins`]) and H11 called
/// in its attempt.
fn mn_chain_digests(to: usize) -> Vec<Digested> {
    use super::evo_chain::{MN_CHAIN, MOCKTIME, decode, linked, recorded_params};
    use crate::chain::{EvoReadCx, EvoReads as _};

    let p = recorded_params();
    let genesis = Mined::genesis(&p);
    let mut blocks = linked(&genesis, decode(MN_CHAIN));
    blocks.truncate(to);
    let rig = Rig::with(super::support::RigConfig {
        params: Some(p.clone()),
        after_genesis: Some(MOCKTIME - i64::from(genesis.time()) + 86_400),
        ..super::support::RigConfig::default()
    });
    rig.headers(&blocks);
    let ids = rig.ids(&blocks);
    let gid = rig.id(&genesis);
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let scheme = bls_ctx_for_block(&p, 1);
        let signals = ValidationSignals::default();
        let bans = Box::new(MemPlatformBans::default());
        let (mut evo, _, _) = mn_chain_node_evo(&p, scheme, bans, &signals);
        let r = with_tip_cx(v, |vcx, cx| -> Result<_, Fatal> {
            let mut prev = gid;
            let mut got = Vec::new();
            let mut coins = ChainCoins::default();
            for (m, &id) in blocks.iter().zip(&ids) {
                let (h4, fields) =
                    connect_digest(&mut evo, vcx, cx, &m.block, &at(m, id, prev), &coins, true)?;
                coins.connect(&m.block, m.height);
                assert!(matches!(h4, Ok(Ok(_))), "{}: {h4:?}", m.height);
                let fields = fields
                    .expect("H4 passed")
                    .map_err(|e| Fatal::new(format!("H11 at {}: {e}", m.height)))?;
                let read = EvoReadCx {
                    reader: cx.reader,
                    deployments: cx.deployments,
                };
                let list = evo.list_at(read, id).map_err(|e| Fatal::new(e.to_string()))?;
                got.push(Digested {
                    height: m.height,
                    fields,
                    list,
                });
                prev = id;
            }
            Ok(got)
        });
        *seen.lock().expect("seen") = Some(r);
        Ok(())
    });
    let got = out.lock().expect("out").take().expect("V ran it");
    rig.assert_no_abort();
    got.unwrap_or_else(|e| panic!("{e}"))
}

/// H11's eras on dashd's masternode chain (DIP0003 at 48, V20 past its end): to 47 every field
/// is outside its era, at 48 `mnl` is set and `qc` is the empty array, and `cp`/`ehf` stay
/// `None` throughout. Before CH7a-5b, H11 refused at 48 with "not built (CH7a-5)".
#[test]
fn h11_is_null_below_dip3_and_set_from_the_dip3_block() {
    let got = mn_chain_digests(60);
    for d in &got[..47] {
        assert_eq!(d.fields, EvoDigest::default(), "{}", d.height);
    }
    let at48 = &got[47];
    assert_eq!(at48.height, 48);
    assert!(at48.fields.mnl.is_some(), "mnl at 48");
    assert_eq!(at48.fields.qc, Some(Vec::new()), "qc at 48");
    for d in &got[47..] {
        assert_eq!((d.fields.cp, &d.fields.ehf), (None, &None), "{}", d.height);
    }
}

/// `mnl` is `CanonicalMNListHash` of the block's own new list (`snapshot.cpp:150-155`): at every
/// height from DIP0003 to 300 it is the hash of the list the hooks committed for that block, and
/// the EvoNode's registration at 116 changes it.
#[test]
fn h11_hashes_the_blocks_new_list() {
    let got = mn_chain_digests(300);
    for d in &got[47..] {
        let want = dash_dml::codec::canonical_mn_list_hash(&d.list);
        assert_eq!(d.fields.mnl, Some(want), "{}", d.height);
        assert_eq!(d.list.height(), d.height as i32, "the block's own list");
    }
    assert_ne!(got[115].fields.mnl, got[114].fields.mnl, "116's registration");
}

/// M2's independent value (Sol r1 #5): at 201, after the collateral spend removed one of the
/// chain's two registrations, `mnl` is sha256d over the canonical bytes assembled here: the block
/// hash, the height, the registration counter **2** (the EvoNode at 116 and the regular MN at
/// 117: a literal, not read from the list) and one entry, then the remaining entry's own bytes.
/// Only the entry bytes come from the encoder, past its fixed 41-byte header.
#[test]
fn h11_mnl_after_the_removal_is_the_hand_assembled_hash() {
    let got = mn_chain_digests(201);
    let at201 = &got[200];
    assert_eq!(at201.height, 201);
    assert_eq!(at201.list.len(), 1, "one masternode left after the collateral spend");
    let mut encoded = Vec::new();
    dash_dml::codec::encode_canonical_list_unvalidated(&at201.list, &mut encoded);
    let entry = &encoded[32 + 4 + 4 + 1..];
    let mut bytes = Vec::new();
    bytes.extend_from_slice(at201.list.block_hash().as_bytes());
    bytes.extend_from_slice(&201i32.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.push(1);
    bytes.extend_from_slice(entry);
    let want = dash_num::Hash256::from(dash_consensus::hash::sha256d(&bytes));
    assert_eq!(at201.fields.mnl, Some(want));
}
