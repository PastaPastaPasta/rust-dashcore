//! `NodeEvo` (CH7 §2.1): the production [`EvoHooks`], H1–H11 at Core's sites over the evo store,
//! its layer and the caches, with D2's readiness stubs where CH7b's answers belong and EV1's
//! checks behind [`Ev1Checks`] (D44).
//!
//! CH7a-2 keeps `COMPLETE` false, so readiness guard G1 still refuses every block an evo rule
//! could judge: in the node the hooks run below DIP0003, and the L0 tests drive the steps past it.

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Instant;

use dash_chain::{ChainReader, FlatFilePos, Status};
use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{Deployment, bls_ctx_for_block};
use dash_consensus::payments::{MnRewardEra, PaymentParams};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::{
    Amount, Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, ChainAncestry,
    HeaderCtx, Tx, TxValidationError,
};
use dash_dml::blockprocessor::{
    BlockLookup, QcBlockAt, QcBlockError, QcCtx, QcHashCache, QcMode, quorum_root,
};
use dash_dml::creditpool::{
    ConnectAt, CreditPool, PoolEras, check_block_diff, connect_records, credit_data_at,
    platform_reward,
};
use dash_dml::mnhf::{MnhfError, MnhfView};
use dash_dml::netinfo::{CoreNetInfoValidity, NetInfoConfig, NetInfoRules};
use dash_dml::payments::{BlockReward, masternode_payee_valid};
use dash_dml::store::{CacheHint, ListCache, resolve_list};
use dash_dml::{
    DmlBuildError, DmlParams, DmlRules, EvoDelta, EvoStore, MemEvoStore, MnList, MnListChange,
    NoTrace, PayeeRules, PrevBlock, StoreError, build_new_list_from_block,
};
use dash_evo::cbtx::{check_mn_list_root, check_quorums_root};
use dash_evo::llmq::{CompleteChain, LlmqCtx, SerialBlsChecks};
use dash_evo::{Effect, EffectSink};
use dash_num::CompactTarget;
use dash_params::{BuriedDeployment, ChainParams};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;
use dashd_store::KvEngine;

use super::caches::EvoCaches;
use super::cl::{ClStep, ClVerifier, check_cbtx_best_chainlock, invalid};
use super::effects::EffectAppliers;
use super::ev1::{Ev1Checks, Ev1NotBuilt, SpecialTxCtx};
use super::not_built::{NoMembers, PreDip3Precompute};
use super::{EvoCodec, EvoLayer, EvoWindowDelta, RocksEvoStore};
use crate::chain::{AttemptOrigin, BlockFiles, ConnectCx, EvoTarget, Fatal, VContext, ViewTarget};
use crate::connect::{
    AttemptId, BlockAt, EvoAttempt, EvoDigest, EvoHooks, SpecialTxFailure, SpecialTxRules,
};
use crate::flush::EvoWindow;
use crate::store::{CoinsReadError, GuardedSnapshot};

/// The capability a block proposal past DIP0003 is refused for: it has no index entry, which the
/// quorum processor and MNHF read through (CH7a-4 binds Core's dummy index).
pub const PROPOSAL_PAST_DIP3: &str = "block proposals past DIP0003 (CH7a-4)";

/// The capability of `VerifyDB`'s evo transaction, the scratch target (CH7a-3).
pub const VERIFY_DB_EVO: &str = "VerifyDB's evo transaction (CH7a-3)";

/// The evo layer V's connector and V's flush share (D48): the connector commits into it, the
/// flush takes its window. Both run on V, so the lock is never contended.
#[derive(Clone, Default)]
pub struct SharedEvoLayer(Arc<Mutex<EvoLayer>>);

impl SharedEvoLayer {
    pub fn new(layer: EvoLayer) -> Self {
        Self(Arc::new(Mutex::new(layer)))
    }

    pub fn lock(&self) -> MutexGuard<'_, EvoLayer> {
        // A panic on V trips the node latch (CH3a D71); nothing reads the layer after it.
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl EvoWindow<EvoWindowDelta> for SharedEvoLayer {
    fn take(&mut self) -> EvoWindowDelta {
        self.lock().take()
    }

    fn bytes(&self) -> u64 {
        self.lock().bytes()
    }

    fn best_block(&self) -> Option<BlockHash> {
        self.lock().best_block()
    }

    fn release(&mut self) {
        self.lock().release();
    }
}

/// `ReadBlockFromDisk(pindex)` for step 10's parent read (`cbtx:181-198`): `None` where the read
/// fails, as `GetNonNullCoinbaseChainlock` answers then.
pub trait EvoBlocks: Send {
    fn read(&self, reader: &ChainReader, id: BlockId) -> Option<Arc<Block>>;
}

/// [`EvoBlocks`] over V's block files, read at the entry's position as `ReadBlockFromDisk` does
/// (`abc`'s `read_block`).
pub struct FileBlocks<B>(pub B);

impl<B: BlockFiles + Send> EvoBlocks for FileBlocks<B> {
    fn read(&self, reader: &ChainReader, id: BlockId) -> Option<Arc<Block>> {
        let e = reader.entry(id)?;
        let file = e.file.filter(|_| e.status.has(Status::HAVE_DATA))?;
        let pos = FlatFilePos {
            file,
            pos: e.data_pos,
        };
        self.0.read_block(pos, &e.hash).ok()
    }
}

/// The Active target's open evo transaction (D1). NodeEvo owns it, not the attempt, because
/// `finish_attempt` consumes the attempt before `commit_tx` runs.
#[derive(Default)]
struct PendingTx {
    /// H4 or H9 stages it; H6 sets its evo tip. `None` for genesis, which writes nothing (D7).
    delta: Option<EvoDelta>,
    /// The list after a connected block, offered to the list cache on commit (§3.7).
    list: Option<Arc<MnList>>,
    /// Step 12's scheme (D9).
    post_scheme: Option<BlsCtx>,
    /// The scheme the commit publishes, once H1's commit has run (D9).
    publish: Option<BlsCtx>,
}

/// D19: `ProcessSpecialTxsInBlock`'s bench statics (`stm:890-895`, `:1009-1011`), per NodeEvo and
/// so per chainstate.
#[derive(Default)]
struct BenchTotals {
    payload: i64,
    loop_: i64,
    credit_pool: i64,
    quorum: i64,
    dmn: i64,
    merkle_mnl: i64,
    merkle_quorums: i64,
    cbtx_cl: i64,
    mnehf: i64,
}

/// `LogPrint(BCLog::BENCHMARK, "      - %s: %.2fms [%.2fs]\n", …)`, adding the step's time to its
/// total; returns the step's end.
fn bench(name: &str, total: &mut i64, from: Instant) -> Instant {
    let now = Instant::now();
    let micros = i64::try_from(now.duration_since(from).as_micros()).unwrap_or(i64::MAX);
    *total = total.saturating_add(micros);
    tracing::debug!(
        target: "bench",
        "      - {name}: {:.2}ms [{:.2}s]",
        micros as f64 * 0.001,
        *total as f64 * 0.000_001
    );
    now
}

/// What [`NodeEvo::new`] takes (D12, D48).
pub struct NodeEvoParts<E: KvEngine, C: Ev1Checks = Ev1NotBuilt> {
    pub store: Arc<RocksEvoStore<E>>,
    pub layer: SharedEvoLayer,
    pub appliers: EffectAppliers,
    pub params: Arc<ChainParams>,
    /// `NetInfoRules::new(params, mainnet, config)`, with the node's `-cjdnsreachable`.
    pub net_info: NetInfoRules,
    pub ev1: C,
    pub cl: Box<dyn ClVerifier + Send>,
    pub blocks: Box<dyn EvoBlocks>,
    /// The scheme the tip committed, Core's startup `bls_legacy_scheme`: the node's is
    /// `bls_ctx_for_block(params, tip + 1)` (D9, §3.4).
    pub scheme: BlsCtx,
}

/// The production `EvoHooks` (CH7 §2.1), one per chainstate, owned by the connector.
pub struct NodeEvo<E: KvEngine, C: Ev1Checks = Ev1NotBuilt> {
    store: Arc<RocksEvoStore<E>>,
    layer: SharedEvoLayer,
    pending: Option<PendingTx>,
    caches: EvoCaches,
    qc_hashes: QcHashCache,
    appliers: EffectAppliers,
    params: Arc<ChainParams>,
    dml: DmlParams,
    net_info: NetInfoRules,
    payments: PaymentParams,
    ev1: C,
    cl: Box<dyn ClVerifier + Send>,
    blocks: Box<dyn EvoBlocks>,
    /// The committed scheme: Core's `bls::bls_legacy_scheme` between attempts (D9).
    scheme: BlsCtx,
    bench: BenchTotals,
}

/// [`NodeEvo`]'s attempt (CH7 §2.1): its log, H1's scheme and the connect context it is bound to
/// (§4.2.3's `AttemptBinding`, D48).
pub struct NodeEvoAttempt<'v> {
    id: AttemptId,
    log: Vec<Effect>,
    cx: &'v ConnectCx<'v>,
    /// H1's answer, fixed for the attempt (D9).
    bls: Option<BlsCtx>,
    /// H1's commit ran.
    committed: bool,
    _v: &'v mut VContext,
}

impl EvoAttempt for NodeEvoAttempt<'_> {
    fn id(&self) -> AttemptId {
        self.id
    }

    fn sink(&mut self) -> &mut dyn EffectSink {
        &mut self.log
    }
}

/// Core's `%d` of `bls_legacy_scheme`.
fn legacy(bls: BlsCtx) -> u8 {
    u8::from(bls == BlsCtx::LEGACY)
}

/// D5 for a store error: D2's readiness refusal, else a fault.
fn store_failure(what: &str, e: &StoreError) -> Result<SpecialTxFailure, Fatal> {
    match e.capability() {
        Some(capability) => Ok(SpecialTxFailure::NotReady(capability)),
        None => Err(Fatal::abort_node(format!("{what}: {e}"))),
    }
}

/// D5 for the quorum processor: a verdict, D2's refusal, or a fault (`NoReason` and SB8's
/// no-verdict domain included).
fn qc_failure(e: QcBlockError) -> Result<SpecialTxFailure, Fatal> {
    match e {
        QcBlockError::Invalid(err) => Ok(SpecialTxFailure::Invalid(err)),
        QcBlockError::NoReason(what) => Err(Fatal::new(format!("quorum block processor: {what}"))),
        QcBlockError::BlsUnsupported => {
            Err(Fatal::abort_node("quorum block processor: a signature outside the BLS domain"))
        }
        QcBlockError::Fault(e) => match e.capability() {
            Some(capability) => Ok(SpecialTxFailure::NotReady(capability)),
            None => Err(Fatal::abort_node(format!("quorum block processor: {e}"))),
        },
    }
}

/// Core's catch-all (`stm:1079-1083`): the FAILURE line and `failed-procspectxsinblock`.
fn exception(what: impl std::fmt::Display) -> SpecialTxFailure {
    tracing::error!("CSpecialTxProcessor::ProcessSpecialTxsInBlock -- FAILURE! {what}");
    invalid("failed-procspectxsinblock")
}

fn active(evo: &EvoTarget<'_>) -> Result<(), Fatal> {
    match evo {
        EvoTarget::Active => Ok(()),
        EvoTarget::Scratch(_) => Err(Fatal::new(format!("{VERIFY_DB_EVO} is not built"))),
    }
}

/// The delta of `at` before any step fills it.
fn empty_delta(at: &BlockAt) -> EvoDelta {
    EvoDelta {
        block: at.hash,
        height: at.height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: None,
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: Vec::new(),
        evo_tip: None,
    }
}

fn height_i32(h: u32) -> Result<i32, Fatal> {
    i32::try_from(h).map_err(|_| Fatal::new("a height past Core's int"))
}

/// The block index as the evo ports walk it, through V's reader: inside the hold every entry an
/// attempt asks about is published.
struct Ancestry<'a>(&'a ChainReader);

impl ChainAncestry for Ancestry<'_> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        let e = crate::chain::index::entry(self.0, id);
        HeaderCtx {
            height: e.height,
            hash: e.hash,
            time: e.header.time,
            bits: CompactTarget::new(e.header.bits),
            version: e.header.version,
            mtp: e.mtp,
            chain_work: e.chain_work,
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.0.ancestor(id, height)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        crate::chain::index::entry(self.0, id).prev
    }
}

impl BlockLookup for Ancestry<'_> {
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)> {
        let id = self.0.lookup(hash)?;
        Some((id, self.0.entry(id)?.height))
    }
}

/// The evo store below DIP0003, where no evo row exists and no port reads one: MNHF's stage reads
/// its cache's empty map below V20, and the quorum processor's undo finds no commitment.
static NO_ROWS: LazyLock<MemEvoStore> = LazyLock::new(MemEvoStore::default);

fn buried_after(cx: &ConnectCx<'_>, prev: Option<BlockId>, d: BuriedDeployment) -> bool {
    cx.deployments.active_after(prev, Deployment::Buried(d))
}

impl<E: KvEngine + 'static, C: Ev1Checks> NodeEvo<E, C>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    /// # Errors
    ///
    /// The network's payment parameters outside Core's domain.
    pub fn new(parts: NodeEvoParts<E, C>) -> Result<Self, Fatal> {
        let payments = PaymentParams::from_chain_params(&parts.params)
            .map_err(|e| Fatal::new(format!("payment parameters: {e:?}")))?;
        Ok(Self {
            dml: DmlParams::new(&parts.params),
            payments,
            store: parts.store,
            layer: parts.layer,
            pending: None,
            caches: EvoCaches::default(),
            qc_hashes: QcHashCache::new(),
            appliers: parts.appliers,
            params: parts.params,
            net_info: parts.net_info,
            ev1: parts.ev1,
            cl: parts.cl,
            blocks: parts.blocks,
            scheme: parts.scheme,
            bench: BenchTotals::default(),
        })
    }

    /// The scheme `ChainView.bls` publishes: the committed one (D9).
    pub fn scheme(&self) -> BlsCtx {
        self.scheme
    }

    fn pending(&mut self) -> Result<&mut PendingTx, Fatal> {
        self.pending.as_mut().ok_or_else(|| Fatal::new("an evo hook outside its transaction (D1)"))
    }

    /// H4's body, `ProcessSpecialTxsInBlock` (`stm:881-1085`), in Core's order. The block's rows
    /// go to the open transaction only once every step has passed.
    #[expect(clippy::too_many_arguments, reason = "H4's arguments")]
    #[expect(clippy::too_many_lines, reason = "Core's one function, step by step")]
    fn special_txs(
        &mut self,
        att: &mut NodeEvoAttempt<'_>,
        block: &Block,
        at: &BlockAt,
        rules: SpecialTxRules,
        coins: &dyn CoinsView<Error = CoinsReadError>,
        subsidy: Amount,
        just_check: bool,
        check: bool,
    ) -> Result<Result<Option<MnListChange>, SpecialTxFailure>, Fatal> {
        let cx = att.cx;
        let bls = att.bls.ok_or_else(|| Fatal::new("ProcessSpecialTxsInBlock before H1"))?;
        let prev = at.prev.ok_or_else(|| Fatal::new("ProcessSpecialTxsInBlock at genesis"))?;
        let prev_entry = crate::chain::index::entry(cx.reader, prev);
        let h = at.height;
        let after = |d| buried_after(cx, at.prev, d);
        let dip3 = after(BuriedDeployment::Dip0003);
        let v20 = after(BuriedDeployment::V20);
        let Self {
            store,
            layer,
            caches,
            qc_hashes,
            params,
            dml,
            net_info,
            payments,
            ev1,
            cl,
            blocks,
            bench: totals,
            ..
        } = self;
        let mut delta = empty_delta(at);
        let t1 = Instant::now();

        // Step 0 (EV1-2).
        let payloads = ev1.payloads(block);
        let mut stx = SpecialTxCtx {
            deployments: cx.deployments,
            prev: at.prev,
            rules,
            coins,
            indexes: None,
            check_sigs: check,
        };

        // Step 1 (`stm:899-917`) and step 2 (`:918-923`).
        let raw_cbtx =
            block.txs().first().is_some_and(|tx| tx.tx_type().to_base() == TRANSACTION_COINBASE);
        let cbtx = if check && raw_cbtx {
            match ev1.check_cbtx(block, &payloads, stx)? {
                Ok(cbtx) => Some(cbtx),
                Err(failure) => return Ok(Err(failure)),
            }
        } else {
            None
        };
        if check && dip3 && cbtx.is_none() {
            return Ok(Err(invalid("bad-cbtx-version")));
        }
        let t2 = bench("GetTxPayload", &mut totals.payload, t1);

        // Every read past here is from DIP0003 on; below it no view is pinned (§7.1).
        let chain = Ancestry(cx.reader);
        let view = dip3.then(|| store.view(&layer.lock(), &chain));

        // Step 3 (`stm:931-935`): the parent's pool, which step 5 and the block's records reuse
        // (§3.5.2). V20 at the block implies DIP0003, so the view is pinned.
        let prev_pool = match &view {
            Some(view) if v20 => {
                let v20_at_prev =
                    cx.deployments.active_at(prev, Deployment::Buried(BuriedDeployment::V20));
                match caches.pools.pool_after(view, &prev_entry.hash, v20_at_prev) {
                    Ok(Ok(pool)) => Some(pool),
                    // EV3 D8: Core's construction throws at the read.
                    Ok(Err(e)) => return Ok(Err(exception(format!("{e:?}")))),
                    Err(e) => return store_failure("GetCreditPool", &e).map(Err),
                }
            }
            _ => None,
        };
        stx.indexes = prev_pool.as_ref();

        // Step 4 (`stm:937-960`).
        for i in 0..block.txs().len() {
            if let Err(failure) = ev1.check_tx(block, &payloads, i, stx)? {
                return Ok(Err(failure));
            }
        }
        let t3 = bench("Loop", &mut totals.loop_, t2);

        // Step 5 (`stm:966-972`; `CheckCreditPoolDiffForBlock`, `:1127-1158`).
        if let Some(cbtx) = &cbtx {
            let dip8 = after(BuriedDeployment::Dip0008);
            if dip8 && v20 {
                let mn_rr = after(BuriedDeployment::MnRr);
                let reward = platform_reward(mn_rr, height_i32(h)?, subsidy, payments)
                    .map_err(|e| Fatal::new(format!("PlatformShare: {e:?}")))?;
                let pool = prev_pool.as_ref().ok_or_else(|| Fatal::new("step 5 without a pool"))?;
                let balance = cbtx.credit_pool_balance.unwrap_or(0);
                if let Err(err) = check_block_diff(block, balance, pool, reward, dip8, v20) {
                    tracing::error!(
                        "CSpecialTxProcessor: CheckCreditPoolDiffForBlock for block {} failed with {}",
                        at.hash,
                        err.reject_reason()
                    );
                    return Ok(Err(SpecialTxFailure::Invalid(err)));
                }
            }
        }
        let t4 = bench("CheckCreditPoolDiffForBlock", &mut totals.credit_pool, t3);

        // Steps 6–10 read the quorums: D2's `NoMembers` over the attempt's LLMQ context.
        let complete = CompleteChain::<StoreError>::new(&chain);
        let llmq = match &view {
            Some(_) => Some(
                LlmqCtx::new(params, &complete, cx.deployments, bls)
                    .map_err(|e| Fatal::new(format!("LLMQ parameters: {e:?}")))?,
            ),
            None => None,
        };

        // Step 6 (`stm:979-982`). Below DIP0003 `ProcessBlock` returns at once.
        if let (Some(view), Some(llmq)) = (&view, llmq) {
            let Some(id) = at.id else {
                return Ok(Err(SpecialTxFailure::NotReady(PROPOSAL_PAST_DIP3)));
            };
            let members = NoMembers {
                llmq,
            };
            let qc = QcCtx {
                llmq,
                index: &chain,
                mined: view,
                members: &members,
                precompute: &members,
                bls_checks: &SerialBlsChecks,
            };
            let mode = QcMode {
                just_check,
                bls_checks: check,
            };
            let at_qc = QcBlockAt {
                id,
                height: h,
            };
            let mined = dash_dml::blockprocessor::process_block(
                &qc,
                block,
                at_qc,
                Some(prev),
                mode,
                &mut att.log,
            );
            match mined {
                Ok(mined) => delta.mined_commitments = mined,
                Err(e) => return qc_failure(e).map(Err),
            }
        }
        let t5 = bench("m_qblockman.ProcessBlock", &mut totals.quorum, t4);

        // Step 7 (`stm:984-1001`).
        let mut list = None;
        let mut change = None;
        if let (Some(view), Some(llmq)) = (&view, llmq) {
            let prev_list = match resolve_list(prev, &chain, cx.deployments, view, &caches.lists) {
                Ok(list) => list,
                Err(e) => return store_failure("GetListForBlock", &e).map(Err),
            };
            let members = NoMembers {
                llmq,
            };
            let dml_rules = DmlRules {
                params: dml,
                net_info,
                v19: after(BuriedDeployment::V19),
                mn_rr: after(BuriedDeployment::MnRr),
                v24: rules.v24,
            };
            let prev_block = PrevBlock {
                id: prev,
                hash: prev_entry.hash,
                height: height_i32(prev_entry.height)?,
                chain: &chain,
            };
            let built = build_new_list_from_block(
                block,
                prev_block,
                &prev_list,
                coins,
                &members,
                &dml_rules,
                &mut att.log,
                &mut NoTrace,
            );
            let built = match built {
                Ok(built) => built.list.with_block_hash(at.hash),
                Err(DmlBuildError::Invalid(err)) => return Ok(Err(SpecialTxFailure::Invalid(err))),
                Err(DmlBuildError::Exception(e)) => return Ok(Err(exception(e))),
                Err(DmlBuildError::Store(e)) => {
                    return store_failure("BuildNewListFromBlock", &e).map(Err);
                }
            };
            if just_check {
                list = Some(Arc::new(built));
            } else {
                let height = height_i32(h)?;
                let first = height == dml.dip3_height;
                let processed = dash_dml::process_block(
                    block,
                    (at.hash, height),
                    &prev_list,
                    built,
                    dml,
                    first,
                    &mut att.log,
                );
                match processed {
                    Ok(processed) => {
                        delta.dml = Some(processed.delta);
                        list = Some(processed.list);
                        change = Some(processed.change);
                    }
                    Err(err) => return Ok(Err(SpecialTxFailure::Invalid(err))),
                }
            }
        }
        let t6 = bench("m_dmnman.ProcessBlock", &mut totals.dmn, t5);

        // Steps 8–10 (`stm:1003-1053`): a CbTx parses only from DIP0003 on.
        if let Some(cbtx) = &cbtx {
            let (Some(list), Some(view), Some(llmq)) = (&list, &view, llmq) else {
                return Err(Fatal::new("a parsed CbTx below DIP0003"));
            };
            match caches.sml.merkle_root(list) {
                Ok(root) => {
                    if let Err(err) = check_mn_list_root(cbtx, &root) {
                        return Ok(Err(SpecialTxFailure::Invalid(err)));
                    }
                }
                Err(err) => return Ok(Err(SpecialTxFailure::Invalid(err))),
            }
            let t6_1 = bench("CalcCbTxMerkleRootMNList", &mut totals.merkle_mnl, t6);

            let mut fault = None;
            let quorums = check_quorums_root(cbtx, || {
                match quorum_root(&llmq, view, qc_hashes, block, prev) {
                    Ok(root) => root,
                    Err(e) => {
                        fault = Some(e);
                        Err(BlockValidationError::new(BlockValidationResult::Consensus, ""))
                    }
                }
            });
            if let Some(e) = fault {
                return qc_failure(e).map(Err);
            }
            if let Err(err) = quorums {
                return Ok(Err(SpecialTxFailure::Invalid(err)));
            }
            let t6_2 = bench("CalcCbTxMerkleRootQuorums", &mut totals.merkle_quorums, t6_1);

            let step = ClStep {
                height: h,
                prev,
                v20_at_prev: cx
                    .deployments
                    .active_at(prev, Deployment::Buried(BuriedDeployment::V20)),
                prev_block: &|| blocks.read(cx.reader, prev),
                bls,
            };
            if let Err(failure) = check_cbtx_best_chainlock(cbtx, &step, cx, &**cl)? {
                return Ok(Err(failure));
            }
            bench("CheckCbTxBestChainlock", &mut totals.cbtx_cl, t6_2);
        }

        // Step 11 (`stm:1055-1067`). A proposal has no index entry to walk; below DIP0003, the
        // only proposals NodeEvo takes, its one write (`AddToCache`) is skipped under
        // `fJustCheck`, and no signal passes step 4 below V20.
        let t7 = Instant::now();
        if let Some(id) = at.id {
            let no_rows = NO_ROWS.view(&chain);
            let store: &(dyn EvoStore + Sync) = match &view {
                Some(view) => view,
                None => &no_rows,
            };
            let mnhf = MnhfView {
                manager: &caches.mnhf,
                params,
                chain: &chain,
                store,
            };
            match mnhf.process_block(block, id, just_check) {
                Ok(processed) => delta.mnhf_signals = processed.row,
                Err(MnhfError::Invalid(err)) => return Ok(Err(SpecialTxFailure::Invalid(err))),
                Err(MnhfError::Store(e)) => {
                    return store_failure("CMNHFManager::ProcessBlock", &e).map(Err);
                }
            }
        }
        bench("m_mnhfman.ProcessBlock", &mut totals.mnehf, t7);

        // The block's credit records (D10), written with it.
        if let (Some(view), false) = (&view, just_check) {
            let period = u32::try_from(params.credit_pool_period_blocks).ok();
            let window_start = match period.and_then(|p| h.checked_sub(p)) {
                Some(w) if v20 => {
                    let start = cx
                        .reader
                        .ancestor(prev, w)
                        .ok_or_else(|| Fatal::new("no credit window start in the index"))?;
                    let start_hash = crate::chain::index::entry(cx.reader, start).hash;
                    let dip3_at = cx
                        .deployments
                        .active_at(start, Deployment::Buried(BuriedDeployment::Dip0003));
                    match credit_data_at(view, &start_hash, dip3_at) {
                        Ok(data) => Some(data),
                        Err(e) => return store_failure("GetDataFromBlock", &e).map(Err),
                    }
                }
                _ => None,
            };
            let connect_at = ConnectAt {
                dip3,
                v20,
                eras: PoolEras {
                    v24: rules.v24,
                    withdrawals: after(BuriedDeployment::Withdrawals),
                },
            };
            let empty = CreditPool::default();
            let prev_pool = prev_pool.as_ref().unwrap_or(&empty);
            let records = connect_records(block, connect_at, prev_pool, window_start.as_ref());
            delta.credit_data = records.data;
            delta.credit_pool = records.pool.map(Arc::new);
        }

        // Step 12 (`stm:1069-1074`): V19 active after the block, under the legacy scheme.
        let mut post_scheme = None;
        if bls == BlsCtx::LEGACY && v19_after_block(params, h) {
            post_scheme = Some(BlsCtx::BASIC);
            tracing::info!(
                "CSpecialTxProcessor::ProcessSpecialTxsInBlock -- bls_legacy_scheme={}",
                legacy(BlsCtx::BASIC)
            );
        }

        // The view's snapshot ends before the transaction is touched.
        drop(view);
        let pending = self.pending()?;
        pending.delta = Some(delta);
        pending.list = list.filter(|_| !just_check);
        pending.post_scheme = post_scheme;
        Ok(Ok(change))
    }
}

/// `DeploymentActiveAfter(pindex, V19)`: V19 is buried at `v19_height`.
fn v19_after_block(params: &ChainParams, h: u32) -> bool {
    h.checked_add(1).is_some_and(|next| next >= params.consensus.v19_height.to_u32())
}

impl<E: KvEngine + 'static, C: Ev1Checks + Send> EvoHooks for NodeEvo<E, C>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    const COMPLETE: bool = false;

    type Facade = ();

    type Attempt<'v>
        = NodeEvoAttempt<'v>
    where
        Self: 'v;

    type ListUpdates = Option<MnListChange>;

    /// D1: no transaction outlives its attempt.
    fn begin_attempt<'v>(
        &mut self,
        _: &'v (),
        v: &'v mut VContext,
        cx: &'v ConnectCx<'_>,
        id: AttemptId,
        _: AttemptOrigin,
        _: &ViewTarget<'_>,
    ) -> Result<NodeEvoAttempt<'v>, Fatal> {
        if self.pending.is_some() {
            return Err(Fatal::new("an evo transaction outlived its attempt (D1)"));
        }
        Ok(NodeEvoAttempt {
            id,
            log: Vec::new(),
            cx,
            bls: None,
            committed: false,
            _v: v,
        })
    }

    /// D9's guard exit, then the log (D12): an uncommitted attempt whose scheme, H1's or step
    /// 12's, is not the committed one reverts it.
    // Core: validation.cpp:164-169 ~ScopedBLSLegacyScheme
    fn finish_attempt(&mut self, attempt: NodeEvoAttempt<'_>) -> Result<(), Fatal> {
        let post = self.pending.as_ref().and_then(|p| p.post_scheme);
        let current = post.or(attempt.bls).unwrap_or(self.scheme);
        if !attempt.committed && current != self.scheme {
            tracing::info!(
                "ScopedBLSLegacyScheme: reverted bls_legacy_scheme={}",
                legacy(self.scheme)
            );
        }
        self.appliers.apply_log(&attempt.log).map(drop)
    }

    /// H1 (D9): the block's scheme, with the guard's "entered" line where it is not the committed
    /// one.
    fn bls_ctx(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        _: &mut EvoTarget<'_>,
        params: &ChainParams,
        at: &BlockAt,
    ) -> BlsCtx {
        let bls = bls_ctx_for_block(params, at.height);
        if bls != self.scheme {
            tracing::info!("ScopedBLSLegacyScheme: entered bls_legacy_scheme={}", legacy(bls));
        }
        attempt.bls = Some(bls);
        bls
    }

    fn commit_bls_scheme(&mut self, attempt: &mut NodeEvoAttempt<'_>, _: &mut EvoTarget<'_>) {
        attempt.committed = true;
        if let Some(pending) = self.pending.as_mut() {
            pending.publish = pending.post_scheme.or(attempt.bls);
        }
    }

    /// H2 (D7): the evo best block through the layer, genesis read as absent.
    fn verify_best_block(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<bool, Fatal> {
        active(evo)?;
        let chain = Ancestry(attempt.cx.reader);
        let view = self.store.view(&self.layer.lock(), &chain);
        let best = EvoStore::best_block(&view)
            .map_err(|e| Fatal::abort_node(format!("VerifyBestBlock: {e}")))?;
        Ok(best == Some(*hash))
    }

    fn process_special_txs(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
        rules: SpecialTxRules,
        view: &dyn CoinsView<Error = CoinsReadError>,
        subsidy: Amount,
        just_check: bool,
        check_cbtx_merkle_roots: bool,
    ) -> Result<Result<Option<MnListChange>, SpecialTxFailure>, Fatal> {
        active(evo)?;
        self.special_txs(
            attempt,
            block,
            at,
            rules,
            view,
            subsidy,
            just_check,
            check_cbtx_merkle_roots,
        )
    }

    /// H5 (§3.6): `IsTransactionValid` over the parent's list, and `IsBlockPayeeValid`'s line.
    fn mn_payment_valid(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        coinbase: &Tx,
        prev: Option<BlockId>,
        subsidy: Amount,
        fees: Amount,
        era: MnRewardEra,
        strict: bool,
    ) -> Result<bool, Fatal> {
        active(evo)?;
        let cx = attempt.cx;
        let prev = prev.ok_or_else(|| Fatal::new("IsTransactionValid at genesis"))?;
        let height = height_i32(crate::chain::index::entry(cx.reader, prev).height + 1)?;
        let chain = Ancestry(cx.reader);
        let view = self.store.view(&self.layer.lock(), &chain);
        let list = resolve_list(prev, &chain, cx.deployments, &view, &self.caches.lists)
            .map_err(|e| Fatal::abort_node(format!("GetListForBlock: {e}")))?;
        let payee = PayeeRules {
            v19: buried_after(cx, Some(prev), BuriedDeployment::V19),
            mn_rr: buried_after(cx, Some(prev), BuriedDeployment::MnRr),
        };
        let reward = BlockReward {
            height,
            block_subsidy: subsidy,
            fee_reward: fees,
            era,
        };
        let valid = masternode_payee_valid(coinbase, &list, payee, &reward, strict, &self.payments)
            .map_err(|e| Fatal::assert(format!("IsTransactionValid: {e:?}")))?;
        if !valid {
            tracing::info!(
                "CMNPaymentsProcessor::IsBlockPayeeValid -- ERROR! Invalid masternode payment \
                 detected at height {height}: {}",
                coinbase.txid()
            );
        }
        Ok(valid)
    }

    /// H6 (D7): the transaction's evo tip.
    fn set_best_block(
        &mut self,
        _: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<(), Fatal> {
        active(evo)?;
        let delta = self.pending()?.delta.as_mut().ok_or_else(|| {
            Fatal::new("WriteBestBlock before ProcessSpecialTxsInBlock or UndoSpecialTxsInBlock")
        })?;
        delta.evo_tip = Some(*hash);
        Ok(())
    }

    /// H7: the listeners come with CH7a-3 (D15).
    fn list_changed(&mut self, _: &mut NodeEvoAttempt<'_>, _: &Option<MnListChange>, _: bool) {}

    /// H8 (EV1-3).
    fn shared_collateral_spends(
        &mut self,
        _: &mut NodeEvoAttempt<'_>,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        self.ev1.shared_collateral_spends(tx, view)
    }

    /// H9, `UndoSpecialTxsInBlock` (`stm:1087-1120`): MNHF, the list, the quorum processor. Its
    /// switch back to the legacy scheme below V19 never fires on V's paths, as H1 has entered the
    /// block's scheme, legacy there (D9). A store fault is `Fatal`: Core's undo reads no verdict.
    fn undo_special_txs(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
    ) -> Result<Option<Option<MnListChange>>, Fatal> {
        active(evo)?;
        let cx = attempt.cx;
        let bls = attempt.bls.ok_or_else(|| Fatal::new("UndoSpecialTxsInBlock before H1"))?;
        let id = at.id.ok_or_else(|| Fatal::new("UndoSpecialTxsInBlock without an index entry"))?;
        let prev = at.prev.ok_or_else(|| Fatal::new("UndoSpecialTxsInBlock at genesis"))?;
        let dip3 = buried_after(cx, at.prev, BuriedDeployment::Dip0003);
        let fault = |what: &str, e: StoreError| Fatal::abort_node(format!("{what}: {e}"));
        let chain = Ancestry(cx.reader);
        let view = dip3.then(|| self.store.view(&self.layer.lock(), &chain));
        let no_rows = NO_ROWS.view(&chain);
        let store: &(dyn EvoStore + Sync) = match &view {
            Some(view) => view,
            None => &no_rows,
        };

        let mnhf = MnhfView {
            manager: &self.caches.mnhf,
            params: &self.params,
            chain: &chain,
            store,
        };
        if !mnhf.undo_block(block, id).map_err(|e| fault("CMNHFManager::UndoBlock", e))? {
            return Ok(None);
        }

        // `CDeterministicMNManager::UndoBlock` (`dmn:787-820`): no row is erased (D6).
        let mut change = None;
        if dip3 {
            let lists = &self.caches.lists;
            let cur = resolve_list(id, &chain, cx.deployments, store, lists)
                .map_err(|e| fault("GetListForBlock", e))?;
            let prev_list = resolve_list(prev, &chain, cx.deployments, store, lists)
                .map_err(|e| fault("GetListForBlock", e))?;
            let stored = store
                .mn_list_diff(&at.hash)
                .map_err(|e| fault("GetListDiff", e))?
                .ok_or_else(|| Fatal::abort_node("UndoBlock: no list diff for a DIP0003 block"))?;
            change = dash_dml::undo_block(&cur, &prev_list, &stored.diff);
        }

        // `CQuorumBlockProcessor::UndoBlock` over `NoMembers`: a refusal undoes a block it never
        // refused to connect, so it is `Fatal` (D2), except the precompute's below DIP0003 (D53).
        let complete = CompleteChain::<StoreError>::new(&chain);
        let llmq = LlmqCtx::new(&self.params, &complete, cx.deployments, bls)
            .map_err(|e| Fatal::new(format!("LLMQ parameters: {e:?}")))?;
        let members = NoMembers {
            llmq,
        };
        let pre_dip3 = PreDip3Precompute(&members);
        let qc = QcCtx {
            llmq,
            index: &chain,
            mined: store,
            members: &members,
            precompute: if dip3 {
                &members
            } else {
                &pre_dip3
            },
            bls_checks: &SerialBlsChecks,
        };
        match dash_dml::blockprocessor::undo_block(&qc, block, id, &mut attempt.log) {
            Ok(()) => {}
            Err(QcBlockError::NoReason(_)) => return Ok(None),
            Err(e) => {
                return Err(Fatal::abort_node(format!("CQuorumBlockProcessor::UndoBlock: {e:?}")));
            }
        }

        drop(view);
        self.pending()?.delta = Some(empty_delta(at));
        Ok(Some(change))
    }

    /// H10: opens the Active target's transaction (D1).
    fn begin_tx(
        &mut self,
        _: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
    ) -> Result<(), Fatal> {
        active(evo)?;
        if self.pending.is_some() {
            return Err(Fatal::new("an evo transaction began twice (D1)"));
        }
        self.pending = Some(PendingTx::default());
        Ok(())
    }

    /// H10, `CommitCurTransaction`: the delta into the layer, then the published scheme (D9) and
    /// the connected list into the cache (§3.7).
    fn commit_tx(&mut self, evo: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        active(evo)?;
        let pending =
            self.pending.take().ok_or_else(|| Fatal::new("commit_tx without begin_tx (D1)"))?;
        if let Some(delta) = &pending.delta {
            self.store
                .commit(&mut self.layer.lock(), delta)
                .map_err(|e| Fatal::abort_node(format!("CommitCurTransaction: {e}")))?;
        }
        if let Some(scheme) = pending.publish {
            self.scheme = scheme;
        }
        if let Some(list) = &pending.list {
            self.caches.lists.offer(list, CacheHint::Tip);
        }
        Ok(())
    }

    fn rollback_tx(&mut self, _: &mut EvoTarget<'_>) {
        self.pending = None;
    }

    /// H11 (O8 §5.2): below DIP0003 every field is outside its era; past it the fields come with
    /// CH7a-5's L2 runs.
    fn digest_fields(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        at: &BlockAt,
    ) -> Result<EvoDigest, Fatal> {
        active(evo)?;
        if buried_after(attempt.cx, at.prev, BuriedDeployment::Dip0003) {
            return Err(Fatal::new("the evo digest fields past DIP0003 are not built (CH7a-5)"));
        }
        Ok(EvoDigest::default())
    }
}

/// The node's NetInfo rules (`SetStateVersion`, `Validate()`): `MainParams()` at `params`' release
/// and `-cjdnsreachable` as step 6 leaves it (D51).
///
/// # Errors
///
/// Mainnet's parameters at the release that do not build.
pub fn node_net_info(params: &ChainParams, cjdns_reachable: bool) -> Result<NetInfoRules, Fatal> {
    let mainnet =
        dash_params::ChainParamsBuilder::new(dash_params::NetworkKind::Mainnet, params.release)
            .build()
            .map_err(|e| Fatal::new(format!("mainnet parameters: {e:?}")))?;
    Ok(NetInfoRules::new(
        params,
        &mainnet,
        NetInfoConfig {
            cjdns_reachable,
        },
    ))
}

/// The node's evo codec: Core's `Validate()` under `net_info` (EV2 §1.3).
pub fn node_codec(net_info: NetInfoRules) -> EvoCodec {
    EvoCodec::new(Arc::new(CoreNetInfoValidity {
        rules: net_info,
    }))
}
