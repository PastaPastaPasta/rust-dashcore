//! `ChainCommand` (§2.2): V's messages, by Core lock class (§2.4).
//!
//! The enum grows with the commits that implement each command: `Mempool` comes with its owner.
//! CH5's VerifyDB runs inside [`ChainCommand::VerifyLoaded`] (CH1-node-LR).

use std::sync::Arc;

use dash_chain::{ChainReader, HeaderBatch, HeaderChain, HeadersResult, PeerId};
use dash_consensus::coins::CoinsView;
use dash_consensus::coins::stats::CoinStatsHashType;
use dash_consensus::ctx::{BlockId, Deployment, DeploymentPos, DeploymentView, VersionbitsAnswer};
use dash_consensus::deployments::info::DeploymentInfo;
use dash_consensus::deployments::{Deployments, EhfReadError};
use dash_consensus::{Block, BlockValidationError, BlsCtx};
use dash_dml::blockprocessor::QcCtx;
use dash_dml::blockprocessor::pool::MineablePool;
use dash_dml::creditpool::{CreditPoolInfo, PoolError, PoolState, RangesSet};
use dash_dml::mnhf::{EhfSignals, GetForBlockError};
use dash_dml::{DmlMemberSource, Dmn, MnList, StoreError};
use dash_evo::cbtx::nonnull_coinbase_chainlock;
use dash_evo::llmq::{
    LlmqCtx, LlmqError, LlmqSource, MemberError, ParamsOutOfDomain, PrecomputeMode,
    QuorumMaterializer as _, QuorumMemberSource, QuorumMembers, QuorumPrecompute, ScanOverload,
    SerialBlsChecks, scan_quorums,
};
use dash_evo::{Effect, EffectSink};
use dash_num::Hash256;
use dash_params::{BuriedDeployment, LlmqType};
use dash_pkc::bls::BlsEagerSig;
use dash_primitives::{BlockHash, QuorumHash};

use super::actor::OnV;
use super::deployments::ChainDeployments;
use super::dmn::list_at_or_empty;
use super::flush::{FlushSite, FlushStateMode};
use super::index::NodeBlockIndex;
use super::locks::{ChainLockSig, ClIntakeReply, ClReply, ClSigs, ClTrigger};
use super::reply::ReplySender;
use super::seams::{BlockConnector, EvoReadCx, EvoReads, Fatal, TipLlmq};
use super::verify::{VerifyDbArgs, VerifyDbResult, VerifyLoadError, VerifyLoadOptions};
use crate::coins::{CoinsDump, CoinsScan, CoinsScanJob, CoinsStatsJob};
use crate::llmq::facade::MembersFacade;
use crate::llmq::members::{TemplateRows, members, template_members};
use crate::llmq::quorum::{FacadeScans, Materializer, NoQuorumHook, QuorumPtr};
use crate::store::CoinsReadError;

/// A read with cross-entry consistency, run on V (class R; A1 §2.10.2).
pub struct ChainQuery<'a> {
    pub chain: &'a HeaderChain,
    pub idx: &'a NodeBlockIndex,
    pub cl: &'a ClSigs,
    /// Core `m_blockman.m_have_pruned`.
    pub have_pruned: bool,
    /// Core `ShouldCheckBlockIndex()`: `-checkblockindex` as V runs it.
    // Core: validation.h:966 at v24.0.0
    pub check_block_index: bool,
    /// V's deployments: CS4 over V's chain with `m_versionbitscache`, so a query reads
    /// `ComputeBlockVersion` and `DeploymentActiveAfter` as the connector does (CreateNewBlock,
    /// MN0-1).
    pub deployments: QueryDeployments<'a>,
    /// V's evo state, read through [`Self::evo_view`].
    pub(super) evo: EvoQuery<'a>,
    /// V's deployments themselves, for [`Self::ehf_signals_stage`].
    pub(super) chain_deployments: &'a ChainDeployments,
}

/// [`ChainQuery`]'s evo half (CH7 D17).
pub(super) struct EvoQuery<'a> {
    pub(super) reads: &'a dyn EvoReads,
    pub(super) reader: &'a ChainReader,
    pub(super) tip: Option<BlockId>,
    pub(super) dmn_tip: Option<BlockId>,
    /// `ChainView.bls`: the published scheme.
    pub(super) bls: BlsCtx,
    /// `CreditPoolPeriodBlocks()`.
    pub(super) credit_window: i32,
    /// The members facade V validates with, whose mineable pool a template reads.
    pub(super) facade: Option<&'a MembersFacade>,
    /// V's connector, whose active coins a template reads ([`EvoView::with_tip_coins`]).
    pub(super) connector: &'a dyn BlockConnector,
}

impl ChainQuery<'_> {
    /// Route B's tip binding (EV addendum §2.6, Q2): quorum members, quorums and scans through
    /// `on_v` at V's tip.
    pub fn bind<'a>(&'a self, on_v: &'a OnV<'a>) -> OnVBinding<'a> {
        OnVBinding {
            on_v,
            query: self,
        }
    }

    /// Core `GetSignalsStage(block)`, which `getdeploymentinfo` reads before its report, with
    /// `GetForBlock`'s rebuild of a block with no row and Core's exceptions as errors (RP1a D52,
    /// DEC-1028): the stage the report's versionbits then read, with no rebuild left to throw.
    // Core: evo/mnhftx.cpp:61-92, :285-321; rpc/blockchain.cpp:1733 at v24.0.0 (getdeploymentinfo)
    pub fn ehf_signals_stage(&self, block: BlockId) -> Result<EhfSignals, GetForBlockError> {
        self.chain_deployments.ehf_signals_stage(block)
    }

    /// `getcreditpoolinfo`'s reads for `at` ([`EvoReads::credit_pool_info`]) over a window of
    /// `CreditPoolPeriodBlocks()`, in this V turn, so the block, its pool and its window start
    /// are one state.
    // Core: rpc/blockchain.cpp:1795-1809 at v24.0.0
    pub fn credit_pool_info(
        &self,
        at: BlockId,
    ) -> Result<Result<CreditPoolInfo, PoolError>, StoreError> {
        self.evo.reads.credit_pool_info(self.read_cx(), at, self.evo.credit_window)
    }

    /// `GetCreditPool(at)`'s asset-unlock indexes, in this V turn: the empty set unless V20 is
    /// active after `at`; `Ok(Err(_))` where Core's construction throws.
    // Core: evo/creditpool.cpp:256-275 at v24.0.0
    pub fn credit_pool_indexes(
        &self,
        at: BlockId,
    ) -> Result<Result<RangesSet, PoolError>, StoreError> {
        let pool = self.evo.reads.credit_pool_after(self.read_cx(), at)?;
        Ok(pool.map_or_else(
            || Ok(RangesSet::default()),
            |p| (*p).as_ref().map(|p| p.indexes.clone()).map_err(Clone::clone),
        ))
    }

    /// [`EvoReads::coinbase_chainlock_at`] in this V turn, under `bls`.
    pub fn coinbase_chainlock_at(
        &self,
        at: BlockId,
        bls: BlsCtx,
    ) -> Result<Option<u32>, StoreError> {
        self.evo.reads.coinbase_chainlock_at(self.read_cx(), at, bls)
    }

    /// Core's process flag `bls::bls_legacy_scheme` in this V turn: the published, committed
    /// scheme (CH7 D9).
    pub fn bls(&self) -> BlsCtx {
        self.evo.bls
    }

    fn read_cx(&self) -> EvoReadCx<'_> {
        EvoReadCx {
            reader: self.evo.reader,
            deployments: &self.deployments,
        }
    }

    /// The evo state at V's tip, read in this V turn (CH7 D17): what MN0-2b's template reads
    /// (DEC-612), all from one state.
    pub fn evo_view(&self) -> EvoView<'_> {
        EvoView {
            reads: self.evo.reads,
            cx: self.read_cx(),
            tip: self.evo.tip,
            dmn_tip: self.evo.dmn_tip,
            facade: self.evo.facade,
            connector: Some(self.evo.connector),
            bls: self.evo.bls,
        }
    }
}

/// [`ChainQuery::bind`]: members over V's ancestry and deployments, the published BLS scheme and
/// the committed evo layer, with snapshot rows written inline; quorums mined on V's active chain.
/// Valid for the `OnV` command's V turn.
pub struct OnVBinding<'a> {
    on_v: &'a OnV<'a>,
    query: &'a ChainQuery<'a>,
}

impl<'a> OnVBinding<'a> {
    /// Core `GetAllQuorumMembers(type, base, reset_cache)` from outside validation (RPC, signing,
    /// DKG): the facade's caches, and a rotation's row written to the runtime rows at once.
    ///
    /// # Errors
    ///
    /// The call's, or parameters outside [`LlmqCtx::new`]'s domain.
    ///
    /// [`LlmqCtx::new`]: dash_evo::llmq::LlmqCtx::new
    pub fn members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        reset: bool,
    ) -> Result<QuorumMembers<Arc<Dmn>>, OnVMembersError> {
        self.at_tip(|at, effects| {
            members(self.on_v, at.ctx, at.src, effects, llmq_type, base, reset)
        })
    }

    /// Core `GetQuorum(type, quorumHash)` from outside validation: `LookupBlockIndex`, then
    /// `GetQuorum(type, base, populate_cache = true)`, which finds the commitment mined on the
    /// active chain, then reads the object cache, else builds the quorum through
    /// [`Self::members`]' call. Before V has a tip no block is on its chain, so no quorum is
    /// (`blockprocessor.cpp:672`).
    ///
    /// # Errors
    ///
    /// As [`Self::members`].
    // Core: v24.0.0 llmq/quorumsman.cpp:491 GetQuorum (hash), :527 GetQuorum (base), :533
    // HasQuorum (blockprocessor.cpp:644-647, the active chain)
    pub fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<QuorumPtr>, OnVMembersError> {
        let Some(tip) = self.query.evo.tip else {
            return Ok(None);
        };
        self.at_tip(|at, effects| {
            let Some((base, _)) = at.index.lookup(&BlockHash::from(*quorum_hash.as_bytes())) else {
                return Ok(None);
            };
            self.materializer(at, tip, effects).get_quorum(llmq_type, base, true)
        })
    }

    /// Core `ScanQuorums(type, start, count)` from outside validation, what
    /// `SelectQuorumForSigning` calls: the scan cache under the store block's hash, and each
    /// quorum through [`Self::quorum`]'s `GetQuorum`, so mined on the active chain. Before V has
    /// a tip no quorum is, so the scan returns none and writes no cache.
    ///
    /// # Errors
    ///
    /// As [`Self::members`].
    // Core: v24.0.0 llmq/quorumsman.cpp:164-169, :179-295 (`chain == nullptr`)
    pub fn scan_quorums(
        &self,
        llmq_type: LlmqType,
        start: BlockId,
        count: usize,
    ) -> Result<Vec<QuorumPtr>, OnVMembersError> {
        let Some(tip) = self.query.evo.tip else {
            return Ok(Vec::new());
        };
        self.at_tip(|at, effects| {
            let src = at.src;
            let made_up = || src.made_up_lists();
            let mut mat = self.materializer(at, tip, effects);
            let mut scans = FacadeScans::new(self.on_v, &made_up);
            let cached = ScanOverload::Cached;
            let scanned = scan_quorums(
                at.ctx, at.mined, &mut scans, &mut mat, llmq_type, start, count, cached,
            )?;
            Ok(scanned.quorums)
        })
    }

    /// `f` over the LLMQ context and inputs at V's tip, its effects to a sink that stays empty:
    /// route B writes its rows inline.
    fn at_tip<T>(
        &self,
        mut f: impl FnMut(TipLlmq<'_, '_>, &mut Vec<Effect>) -> Result<T, LlmqError<StoreError>>,
    ) -> Result<T, OnVMembersError> {
        let evo = &self.query.evo;
        let cx = EvoReadCx {
            reader: evo.reader,
            deployments: &self.query.deployments,
        };
        let mut out = None;
        evo.reads.with_llmq(cx, evo.bls, &mut |at| {
            let mut effects = Vec::new();
            out = Some(f(at, &mut effects)?);
            debug_assert!(effects.is_empty(), "route B writes its rows inline");
            Ok(())
        })?;
        let Some(out) = out else {
            unreachable!("with_llmq answers Ok only after its call");
        };
        Ok(out)
    }

    /// `GetQuorum(type, base, populate_cache)` over `on_v`, mined on the chain ending at `tip`.
    fn materializer<'m, 'c>(
        &'m self,
        at: TipLlmq<'m, 'c>,
        tip: BlockId,
        effects: &'m mut Vec<Effect>,
    ) -> Materializer<'m, 'c, OnV<'a>, MnList> {
        Materializer {
            cap: self.on_v,
            ctx: at.ctx,
            src: at.src,
            mined: at.mined,
            tip,
            effects,
            hook: &NoQuorumHook,
        }
    }
}

/// Why a route-B call ([`OnVBinding`]) gave no answer.
#[derive(Debug, thiserror::Error)]
pub enum OnVMembersError {
    /// A configured type's parameters are outside [`LlmqCtx::new`]'s domain.
    ///
    /// [`LlmqCtx::new`]: dash_evo::llmq::LlmqCtx::new
    #[error("LLMQ parameters: {0}")]
    Params(ParamsOutOfDomain),
    #[error(transparent)]
    Members(#[from] LlmqError<StoreError>),
}

impl OnVMembersError {
    /// A node with no evo state, which has no LLMQ inputs to read.
    pub(crate) fn not_built() -> Self {
        OnVMembersError::Members(MemberError::View(StoreError::NotBuilt(
            "the LLMQ reads of a node without evo state",
        )))
    }
}

/// [`ChainQuery::evo_view`]: the lists, the credit pool and the mined commitments at V's tip.
/// Valid for the query's V turn.
pub struct EvoView<'a> {
    reads: &'a dyn EvoReads,
    cx: EvoReadCx<'a>,
    /// V's tip, the parent of the next block; `None` before it has one.
    pub tip: Option<BlockId>,
    /// Core's DMN tip (D16), as `ChainView.dmn_tip`.
    pub dmn_tip: Option<BlockId>,
    facade: Option<&'a MembersFacade>,
    connector: Option<&'a dyn BlockConnector>,
    /// The published scheme, Core's `bls_legacy_scheme` that reads a stored CbTx's ChainLock.
    bls: BlsCtx,
}

#[cfg(test)]
impl<'a> EvoView<'a> {
    /// The members facade the view reads, for tests that inspect it.
    pub(crate) fn facade(&self) -> Option<&'a MembersFacade> {
        self.facade
    }

    /// A view over `reads` as a query would make it, for a test that drives the hooks itself.
    pub(crate) fn over(reads: &'a dyn EvoReads, cx: EvoReadCx<'a>, tip: Option<BlockId>) -> Self {
        EvoView {
            reads,
            cx,
            tip,
            dmn_tip: tip,
            facade: None,
            connector: None,
            bls: BlsCtx::from_legacy(false),
        }
    }
}

impl EvoView<'_> {
    /// `GetListForBlock(pindexPrev)` for a block on the tip: the list `GetMasternodeTxOuts` pays
    /// from. Core's empty list before the node has a tip.
    // Core: masternode/payments.cpp:152
    pub fn tip_list(&self) -> Result<Arc<MnList>, StoreError> {
        list_at_or_empty(self.reads, self.cx, self.tip)
    }

    /// `GetListAtChainTip()`: the list at the DMN tip, which the superblock arms read. It equals
    /// [`Self::tip_list`] except before init's `InitializeCurrentBlockTip` and after a
    /// disconnect-only step without a mempool (D16).
    // Core: masternode/payments.cpp:366, :423, :450 at v24.0.0
    pub fn dmn_list(&self) -> Result<Arc<MnList>, StoreError> {
        list_at_or_empty(self.reads, self.cx, self.dmn_tip)
    }

    /// `GetListForBlock(block)` for any block in V's index: the list `masternode payments` pays
    /// a block from is its parent's (CH7b-3 C9's render).
    // Core: evo/deterministicmns.h:796-799
    #[cfg(any(test, feature = "test-support"))]
    pub fn list_for_block(&self, block: BlockId) -> Result<Arc<MnList>, StoreError> {
        list_at_or_empty(self.reads, self.cx, Some(block))
    }

    /// `GetCreditPool(Tip())`: `None` before a tip, or unless V20 is active after it.
    // Core: evo/creditpool.cpp:256, node/miner.cpp:520 at v24.0.0
    pub fn credit_pool(&self) -> Result<Option<Arc<PoolState>>, StoreError> {
        match self.tip {
            Some(tip) => self.reads.credit_pool_after(self.cx, tip),
            None => Ok(None),
        }
    }

    /// `HasMinedCommitment(llmqType, quorumHash)` on the active chain; `false` before a tip.
    // Core: llmq/blockprocessor.cpp:644-647
    pub fn has_mined_commitment(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<bool, StoreError> {
        match self.tip {
            Some(tip) => self.reads.has_mined_commitment(self.cx, llmq_type, quorum_hash, tip),
            None => Ok(false),
        }
    }

    /// A snapshot of the mineable pool (`minableCommitments`), taken in this V turn under
    /// `minableCommitmentsCs`, a leaf lock released before the caller's next read; an empty pool
    /// for a node without the members facade. Core takes the lock per quorum, for the map lookup
    /// only; its entries are `Arc`, so the copy shares them.
    // Core: v24.0.0 llmq/blockprocessor.cpp:882-936 GetMineableCommitments
    pub fn mineable(&self) -> MineablePool {
        match self.facade {
            Some(facade) => facade.mineable().clone(),
            None => MineablePool::new(),
        }
    }

    /// `f`, once, over V's active coins as `BuildNewListFromBlock` reads them in a template
    /// (`m_chainstate.CoinsTip()`): an overlay dropped unwritten, so nothing is written, dirtied
    /// or flushed (MN0-3 C0). `Ok(None)`, `f` not called, for a node whose connector keeps no
    /// coins.
    ///
    /// # Errors
    ///
    /// The coins cell held or poisoned: a wiring error.
    // Core: v24.0.0 node/miner.cpp:225-226
    pub fn with_tip_coins<T>(
        &self,
        f: impl FnOnce(&dyn CoinsView<Error = CoinsReadError>) -> T,
    ) -> Result<Option<T>, Fatal> {
        let Some(connector) = self.connector else {
            return Ok(None);
        };
        let mut f = Some(f);
        let mut out = None;
        connector.with_tip_coins(&mut |coins| {
            if let Some(f) = f.take() {
                out = Some(f(coins));
            }
        })?;
        Ok(out)
    }

    /// `f`, once, over the block processor's context at V's tip under `bls`, the scheme of the
    /// block being built: V's ancestry as `LookupBlockIndex` and the mined commitments of one
    /// committed view, pinned for the call ([`EvoReads::with_llmq`]'s). Its members and precompute
    /// are the template's (`TemplateLlmq`): Core's computation over that view and the facade's
    /// rows read once per key, filling no cache and keeping no row past the call (MN0-3 C0); a
    /// node without the facade refuses them (`TemplateMembers`). Its signature checks are the
    /// serial runner's, and a template verifies none (OD-CP-19 C2).
    ///
    /// # Errors
    ///
    /// Without evo state, the readiness refusal, with `f` not called; or parameters outside the
    /// LLMQ context's domain. `f`'s own errors are in its answer.
    // Core: v24.0.0 node/miner.cpp:194 (cs_main), :229 `GetMineableCommitmentsTx`, :308
    // `CalcCbTxMerkleRootQuorums` (evo/cbtx.cpp:65): CreateNewBlock's quorum reads
    pub fn with_qc<T>(
        &self,
        bls: BlsCtx,
        f: impl FnOnce(&QcCtx<'_>) -> T,
    ) -> Result<T, OnVMembersError> {
        self.with_template(bls, |qc, _| f(qc))
    }

    /// [`Self::with_qc`], with `GetNonNullCoinbaseChainlock(pindexPrev)` at V's tip over the same
    /// pinned view: a reader `f` calls only where it needs it, which answers the tip CbTx's
    /// non-null ChainLock and its height difference, `None` before a tip or below V20 at it
    /// (MN0-3 C3).
    ///
    /// # Errors
    ///
    /// [`Self::with_qc`]'s.
    // Core: v24.0.0 node/miner.cpp:132 (evo/cbtx.cpp GetNonNullCoinbaseChainlock)
    pub fn with_template<T>(
        &self,
        bls: BlsCtx,
        f: impl FnOnce(&QcCtx<'_>, &ParentChainlock<'_>) -> T,
    ) -> Result<T, OnVMembersError> {
        let mut f = Some(f);
        let mut out = None;
        let facade = self.facade;
        let (tip, published) = (self.tip, self.bls);
        self.reads.with_llmq(self.cx, bls, &mut |at| {
            let parent = || -> Result<Option<(BlsEagerSig, u32)>, StoreError> {
                let Some(tip) = tip else {
                    return Ok(None);
                };
                let Some(facts) = at.src.coinbase_facts(tip)? else {
                    return Err(StoreError::NotBuilt(
                        "the tip's coinbase in a block template's reads",
                    ));
                };
                let height = at.ctx.chain().header(tip)?.height;
                let v20 =
                    at.ctx.deployments().active_at(tip, Deployment::Buried(BuriedDeployment::V20));
                Ok(nonnull_coinbase_chainlock(&facts, height, v20, published))
            };
            let template = facade.map(|facade| TemplateLlmq {
                ctx: at.ctx,
                src: at.src,
                rows: facade.template_rows(),
            });
            let members: &DmlMemberSource<'_> = match &template {
                Some(t) => t,
                None => &TemplateMembers,
            };
            let qc = QcCtx {
                llmq: *at.ctx,
                index: at.index,
                mined: at.mined,
                members,
                precompute: &TemplateMembers,
                bls_checks: &SerialBlsChecks,
                trace: &dash_dml::NoQcTrace,
            };
            if let Some(f) = f.take() {
                out = Some(f(&qc, &parent));
            }
            Ok(())
        })?;
        let Some(out) = out else {
            unreachable!("with_llmq answers Ok only after its call");
        };
        Ok(out)
    }
}

/// [`EvoView::with_template`]'s reader of the tip CbTx's non-null ChainLock.
pub type ParentChainlock<'a> = dyn Fn() -> Result<Option<(BlsEagerSig, u32)>, StoreError> + 'a;

/// [`EvoView::with_qc`]'s members (MN0-3 C0): Core's computation over the call's LLMQ context
/// and source, with the facade's rows read once per key and never written ([`TemplateRows`]), no
/// members cache read or filled, and nothing emitted. Dropped with the call. The assembler never
/// precomputes (only `ProcessBlock` and `UndoBlock` do), so the precompute beside it is
/// [`TemplateMembers`]' refusal.
struct TemplateLlmq<'a, 'c> {
    ctx: &'a LlmqCtx<'c, StoreError>,
    src: &'a dyn LlmqSource<MnList, Error = StoreError>,
    rows: TemplateRows<'a>,
}

impl QuorumMemberSource for TemplateLlmq<'_, '_> {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        template_members(self.ctx, self.src, &self.rows, llmq_type, base)
    }
}

/// [`EvoView::with_qc`]'s members and precompute on a node without the members facade: a call
/// refuses with `NotBuilt` and the template fails, not a panic.
struct TemplateMembers;

impl TemplateMembers {
    fn refused() -> MemberError<StoreError> {
        MemberError::View(StoreError::NotBuilt("quorum members in a block template's reads"))
    }
}

impl QuorumMemberSource for TemplateMembers {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        Err(Self::refused())
    }
}

impl QuorumPrecompute for TemplateMembers {
    type Error = StoreError;

    fn precompute(
        &self,
        _: BlockId,
        _: PrecomputeMode,
        _: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        Err(Self::refused())
    }
}

/// [`ChainQuery::deployments`]: V's deployment answers without the cache behind them, so a read
/// can fill `m_versionbitscache` as Core's `CreateNewBlock` does but never clear it.
#[derive(Clone, Copy)]
pub struct QueryDeployments<'a>(&'a Deployments<'a>);

impl<'a> QueryDeployments<'a> {
    pub(super) fn new(deployments: &'a Deployments<'a>) -> Self {
        Self(deployments)
    }

    /// Whether `m_versionbitscache` holds a state for the block after `prev`
    /// ([`VersionBitsCache::holds`](dash_consensus::deployments::VersionBitsCache::holds)).
    pub fn versionbits_cache_holds(&self, prev: Option<BlockId>) -> bool {
        self.0.cache.holds(prev)
    }

    /// `ComputeBlockVersion(pindexPrev)` for the block after `prev`, or `GetForBlock`'s error.
    pub fn compute_block_version(&self, prev: Option<BlockId>) -> Result<i32, EhfReadError> {
        self.0.compute_block_version(prev)
    }

    /// `DeploymentInfo(block, GetSignalsStage(block))`: `getdeploymentinfo`'s entries, or
    /// `GetForBlock`'s error from a read of the report's walk.
    pub fn deployment_info(&self, block: BlockId) -> Result<Vec<DeploymentInfo>, EhfReadError> {
        self.0.deployment_info(block)
    }
}

impl DeploymentView for QueryDeployments<'_> {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        self.0.active_after(prev, deployment)
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.0.active_at(block, deployment)
    }

    fn versionbits(&self, prev: Option<BlockId>, pos: DeploymentPos) -> Option<VersionbitsAnswer> {
        self.0.versionbits(prev, pos)
    }

    fn try_active_after(
        &self,
        prev: Option<BlockId>,
        deployment: Deployment,
    ) -> Result<bool, EhfReadError> {
        self.0.try_active_after(prev, deployment)
    }

    fn try_versionbits(
        &self,
        prev: Option<BlockId>,
        pos: DeploymentPos,
    ) -> Result<Option<VersionbitsAnswer>, EhfReadError> {
        self.0.try_versionbits(prev, pos)
    }
}

/// The coins side V takes once the coins open, after `LoadGenesisBlock` (D51): Core's
/// `CanFlushToDisk()` turns true (`validation.cpp:4865-4867` at v24.0.0).
pub struct AttachCoins {
    pub connector: Box<dyn BlockConnector>,
    pub flush: Box<dyn FlushSite>,
    /// The connector's members facade, which replaces V's with it ([`VParts::facade`]).
    ///
    /// [`VParts::facade`]: super::VParts::facade
    pub facade: Option<Arc<MembersFacade>>,
}

/// Core's `BlockValidationState`: valid, invalid with a result class, or a system error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockState {
    Valid,
    Invalid(BlockValidationError),
    /// `state.Error(msg)`.
    Error(String),
}

impl BlockState {
    pub fn is_valid(&self) -> bool {
        matches!(self, BlockState::Valid)
    }
}

// Core: consensus/validation.h:105 ValidationState::ToString
impl std::fmt::Display for BlockState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockState::Valid => f.write_str("Valid"),
            BlockState::Invalid(e) => e.fmt(f),
            BlockState::Error(msg) => f.write_str(msg),
        }
    }
}

/// The block RPCs' lookup failure (`rpc/blockchain.cpp:1966-1972`, `:2007-2011`, `:2053-2057` at
/// v24.0.0): `RPC_INVALID_ADDRESS_OR_KEY`, before any other work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Block not found")]
pub struct BlockNotFound;

/// A proposal `getblocktemplate` answers before `TestBlockValidity`, under the same `cs_main`
/// hold (`rpc/mining.cpp:707-723`), each displayed as Core's string. V answers them before the
/// connector, which only sees a block whose parent is V's tip (CH3a D123).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NotTested {
    /// GBT's lookup: the block is indexed and valid up to its scripts (`rpc/mining.cpp:710-711`).
    #[error("duplicate")]
    Duplicate,
    /// GBT's lookup: the block is indexed and failed (`rpc/mining.cpp:712-713`).
    #[error("duplicate-invalid")]
    DuplicateInvalid,
    /// GBT's lookup: the block is indexed, neither (`rpc/mining.cpp:714`).
    #[error("duplicate-inconclusive")]
    DuplicateInconclusive,
    /// The block's parent is not the tip (`rpc/mining.cpp:717-720`).
    #[error("inconclusive-not-best-prevblk")]
    NotBestPrev,
}

/// The reply of `invalidateblock`, `reconsiderblock` and `preciousblock`. Their other failures
/// follow an `AbortNode`, which stops V, so the reply disconnects instead.
pub type BlockRpcReply = ReplySender<Result<(), BlockNotFound>>;

/// `ProcessNewBlock`'s outcome (§2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockResult {
    /// `*new_block`: the block's data was written now.
    pub new_block: bool,
    /// The state Core passes to `BlockChecked` on failure (`validation.cpp:4902` at v24.0.0).
    pub state: BlockState,
    /// `ProcessNewBlock`'s return before its ABC: `CheckBlock` and `AcceptBlock` passed.
    pub accepted: bool,
}

impl BlockResult {
    /// A failed `CheckBlock` or `AcceptBlock`: no write, not accepted.
    pub fn rejected(state: BlockState) -> BlockResult {
        BlockResult {
            new_block: false,
            state,
            accepted: false,
        }
    }
}

/// How V resolved an InstantSend lock's block conflicts (§5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsResolution {
    /// No block holds a conflicting transaction.
    NoConflict,
    /// A conflict is ChainLocked: the lock was removed, no block was invalidated (`is:602-607` at
    /// v24.0.0).
    LockRemoved,
    /// The conflicting blocks were invalidated, then reset, or activated past with
    /// `ActivateBestChain` when the lock's transaction is known (`is:609-642` at v24.0.0).
    Invalidated {
        activated: bool,
    },
}

/// Which completion a `ProcessBlock` caller waits for (rev2, R2-M1).
pub enum BlockReply {
    /// Core's `ProcessNewBlock` return: after the block's own `ActivateBestChain`
    /// (`validation.cpp:4910-4917` at v24.0.0). Every Core caller's choice (OH-m4). C-dependent.
    Activated(ReplySender<BlockResult>),
    /// Sent when the accept scope ends, before the ABC tail runs. R/M-complete; no Core caller.
    Accepted(ReplySender<BlockResult>),
}

/// V's commands.
pub enum ChainCommand {
    // ---- class M: Core runs these under cs_main only
    /// Core `ProcessNewBlockHeaders` (P2a R7).
    ProcessHeaders {
        batch: HeaderBatch,
        from: Option<PeerId>,
        reply: ReplySender<HeadersResult>,
    },
    /// Core `ProcessNewBlock`: the accept half is class M, its ABC tail class C.
    ProcessBlock {
        block: Arc<Block>,
        hash: BlockHash,
        force: bool,
        from: Option<PeerId>,
        /// The caller checked the merkle root (`IsBlockMutated`, `v:4445-4468` at v24.0.0): Core's
        /// `m_checked_merkle_root`, so `CheckBlock` skips that pass (`v:4337`, `v:4373` at v24.0.0;
        /// A-4b).
        merkle_checked: bool,
        reply: Option<BlockReply>,
    },
    /// `fImporting` / `fReindex` (CH5).
    SetLoading {
        importing: Option<bool>,
        reindex: Option<bool>,
    },
    /// The coins, attached once, before V's first class C operation (D51).
    AttachCoins {
        coins: AttachCoins,
        reply: ReplySender<()>,
    },
    /// `reconsiderblock` (`rpc:2059-2063` at v24.0.0): `ResetBlockFailureFlags` is class M, its ABC
    /// tail class C.
    ReconsiderBlock {
        hash: BlockHash,
        ignore_chainlocks: bool,
        reply: BlockRpcReply,
    },
    /// Core `PreciousBlock` (`validation.cpp:3869-3898` at v24.0.0): its `cs_main` scope is class
    /// M, its ABC tail class C.
    PreciousBlock {
        hash: BlockHash,
        reply: BlockRpcReply,
    },
    /// `ProcessNewChainLock`'s `CheckActiveState` (`h:136`), before any early return (§5.1).
    ClIntake {
        reply: ReplySender<ClIntakeReply>,
    },
    /// `UpdateBestChainlock` for a lock LK1a has verified (§5.1). `from` is the peer, `None` for
    /// a coinbase lock.
    RecordChainLock {
        sig: Arc<ChainLockSig>,
        sig_hash: Hash256,
        from: Option<PeerId>,
        reply: ReplySender<ClReply>,
    },
    /// Core `VerifyLoadedChainstate` (CH1-node-LR, D64): after the load and the coins' attach,
    /// before init's ABC.
    VerifyLoaded {
        options: VerifyLoadOptions,
        reply: ReplySender<Result<(), VerifyLoadError>>,
    },
    /// Core's startup `InitializeCurrentBlockTip(tip, ibd)` (CH7 §6.3 step 6), sent once init's
    /// block import, its `ActivateBestChain` and (with MP1) the mempool load are done, with
    /// `fImporting` cleared: V reads its tip and `IsInitialBlockDownload()` then, moves the DMN
    /// tip to the tip, publishes the view, then runs the listeners (D16). After V stops on an
    /// `AbortNode` or a shutdown, V's reaper answers it the same way with V's tip, but
    /// publishes no view (CH7-notes D72).
    InitializeCurrentBlockTip {
        reply: ReplySender<()>,
    },
    /// Core `CVerifyDB::VerifyDB` on the active chainstate under `cs_main`, as `verifychain`
    /// calls it (`rpc/blockchain.cpp:1436-1440` at v24.0.0): the load path's VerifyDB point.
    VerifyDb {
        args: VerifyDbArgs,
        reply: ReplySender<VerifyDbResult>,
    },
    /// Core `TestBlockValidity(…, block, Tip(), fCheckPOW, fCheckMerkleRoot)` under `cs_main`
    /// (`validation.cpp:4934-4985` at v24.0.0; CH3 §2.14), as `getblocktemplate` proposal mode
    /// (`rpc/mining.cpp:717-723`) and `CreateNewBlock` (`node/miner.cpp:352` at v24.0.0) call it:
    /// on the tip only, nothing committed or inserted. The reply is Core's `BlockValidationState`,
    /// or [`NotTested`]: GBT's duplicate answers when `lookup`, then a parent that is not the tip
    /// (CH3a D123).
    TestBlockValidity {
        block: Arc<Block>,
        check_pow: bool,
        check_merkle: bool,
        /// GBT's duplicate lookup first (`rpc/mining.cpp:707-715`); `CreateNewBlock` makes none.
        lookup: bool,
        reply: ReplySender<Result<BlockState, NotTested>>,
    },
    /// Core `FlushStateToDisk(state, mode)` from a caller with no tip step, under `cs_main`
    /// (`validation.cpp:3024-3029` at the pin): [`FlushSite::flush`]
    /// (CH1-node §2.4, D45; DEC-525). The reply comes once that returns: after the write for
    /// ALWAYS, while W may still be writing what IF_NEEDED or PERIODIC decided (CH2's
    /// asynchronous writes, as at V's tip sites), where Core's returns after every write. Its
    /// failure follows an `AbortNode`, which stops V, so the reply disconnects instead. The
    /// shutdown flushes run on V's [`VExit`](super::VExit) instead, after V stops (BL-271,
    /// DEC-613); `pruneblockchain`'s manual height comes with that RPC's slice.
    Flush {
        mode: FlushStateMode,
        /// A test's FLUSH op's label, which its jobs are booked under (CH3a a10-2).
        #[cfg(feature = "test-support")]
        label: Option<std::sync::Arc<str>>,
        reply: ReplySender<()>,
    },
    /// `gettxoutsetinfo` with no index (`rpc:1235-1247`, `:1276` at v24.0.0, RP1a D2), and
    /// `dumptxoutset`'s `CreateUTXOSnapshot` with a `dump` (`rpc:3171-3180` at v24.0.0):
    /// `ForceFlushStateToDisk()`, then the coins' snapshot, so the scan reads the coins at the tip
    /// it flushed. `dumptxoutset` does both under one `cs_main` scope; `gettxoutsetinfo` flushes
    /// before its `cs_main` scope and takes its cursor after it (`rpc:1238`, `:1243-1247`, `:1276`
    /// at v24.0.0). The reply is the scan, started, or `None` with no coins database. A failed
    /// flush is an `AbortNode`, which stops V, so the reply disconnects instead.
    CoinsStats {
        hash_type: CoinStatsHashType,
        /// Core's `rpc_interruption_point`, checked at each coin.
        interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        /// `dumptxoutset`'s file, written from the same snapshot.
        dump: Option<CoinsDump>,
        reply: ReplySender<Option<CoinsStatsJob>>,
    },
    /// `scantxoutset`'s `ForceFlushStateToDisk()` and `CoinsDB().Cursor()` under one `cs_main`
    /// scope (`rpc:2767-2774` at v24.0.0): as [`CoinsStats`](Self::CoinsStats), the walk started on
    /// the coins at the tip it flushed, or `None` with no coins database.
    CoinsScan {
        scan: CoinsScan,
        reply: ReplySender<Option<CoinsScanJob>>,
    },
    /// Route B (EV addendum §2.6): a closure run on V between attempts, under the members
    /// facade's gate held shared, with V's tip state. Class M (OD-CP-5): a release point serves
    /// it, as Core's other threads run between `cs_main` holds. Dropped unrun, which closes its
    /// caller's reply, when V has no facade (D77) or the node latch has tripped (D74).
    OnV(OnVCommand),
    // ---- class C: Core takes m_chainstate_mutex
    /// Core `ActivateBestChain(state)` with no block (init, the RPCs). Its `false` follows an
    /// `AbortNode`, which V has stopped on where it arose (CH3b D213): the reply is `Err` with
    /// Core's `state.ToString()`, which `ThreadImport`'s second `AbortNode` shows (BL-472). One of
    /// Core's asserts ends the process instead, and the reply disconnects.
    ActivateBestChain {
        reply: Option<ReplySender<Result<(), String>>>,
    },
    /// `invalidateblock` (`rpc:2015-2018` at v24.0.0): Core `InvalidateBlock`, then
    /// `ActivateBestChain`.
    InvalidateBlock {
        hash: BlockHash,
        reply: BlockRpcReply,
    },
    /// `NetInstantSend::ResolveBlockConflicts` for the lock `lock` (`is:584-643` at v24.0.0, §5.6,
    /// D42).
    IsConflictInvalidate {
        lock: Hash256,
        /// The lock's `islock.txid`, for Core's log line.
        txid: Hash256,
        reply: ReplySender<IsResolution>,
    },
    /// One of the ChainLock handler's scheduler tasks (`h:63-72`, `h:170-176`, `h:196-205`,
    /// §5.2).
    ClEnforce {
        source: ClTrigger,
    },
    /// A step's end in a test (CH3a a10 Q2): [`FlushSite::settle`] on
    /// V, after the class C work queued ahead of it. No Core operation: it publishes no view.
    #[cfg(feature = "test-support")]
    Settle {
        reply: ReplySender<super::Quiet>,
    },
    /// A test arms one of leg B's flush seams ([`FlushSite::arm`]), in order with V's class C
    /// work. No Core operation.
    #[cfg(feature = "test-support")]
    ArmFlush {
        arm: super::FlushArm,
        reply: ReplySender<()>,
    },
    // ---- class R
    Query(Box<dyn FnOnce(&ChainQuery<'_>) + Send>),
    /// [`ChainHandle::query_or_stop`](super::ChainHandle::query_or_stop): a read whose `Err` is
    /// V's fault.
    QueryOrStop(Box<QueryOrStopRun>),
    // ---- control
    Shutdown,
    /// A class C operation running `f` on V (tests drive release points and waits by hand).
    #[cfg(test)]
    #[expect(private_interfaces, reason = "test-only")]
    TestUnderV(super::actor::tests::UnderV),
    /// A class M command running `f` on V, which a release point serves as it would Core's other
    /// threads' `cs_main` work (tests raise a release-point command's failure, CH3b D213).
    #[cfg(test)]
    #[expect(private_interfaces, reason = "test-only")]
    TestUnderMain(super::actor::tests::UnderV),
}

/// What an `OnV` command runs: the capability, and V's state as a class R read sees it.
type OnVRun = dyn for<'v> FnOnce(&OnV<'v>, &ChainQuery<'_>) + Send;

type QueryOrStopRun = dyn FnOnce(&ChainQuery<'_>) -> Result<(), Fatal> + Send;

/// [`ChainCommand::OnV`]'s closure.
pub struct OnVCommand {
    run: Box<OnVRun>,
}

impl OnVCommand {
    pub fn new(run: impl for<'v> FnOnce(&OnV<'v>, &ChainQuery<'_>) + Send + 'static) -> Self {
        OnVCommand {
            run: Box::new(run),
        }
    }

    pub(super) fn run(self, on_v: &OnV<'_>, query: &ChainQuery<'_>) {
        (self.run)(on_v, query);
    }
}

/// The Core lock a command's first scope takes (§2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// `cs_main` (read).
    R,
    /// `cs_main`.
    M,
    /// `m_chainstate_mutex`.
    C,
    /// Shutdown.
    Control,
}

impl ChainCommand {
    pub fn class(&self) -> Class {
        match self {
            ChainCommand::ProcessHeaders {
                ..
            }
            | ChainCommand::ProcessBlock {
                ..
            }
            | ChainCommand::SetLoading {
                ..
            }
            | ChainCommand::AttachCoins {
                ..
            }
            | ChainCommand::ReconsiderBlock {
                ..
            }
            | ChainCommand::PreciousBlock {
                ..
            }
            | ChainCommand::ClIntake {
                ..
            }
            | ChainCommand::RecordChainLock {
                ..
            }
            | ChainCommand::VerifyLoaded {
                ..
            }
            | ChainCommand::InitializeCurrentBlockTip {
                ..
            }
            | ChainCommand::VerifyDb {
                ..
            }
            | ChainCommand::TestBlockValidity {
                ..
            }
            | ChainCommand::Flush {
                ..
            }
            | ChainCommand::CoinsStats {
                ..
            }
            | ChainCommand::CoinsScan {
                ..
            }
            | ChainCommand::OnV(_) => Class::M,
            ChainCommand::ActivateBestChain {
                ..
            }
            | ChainCommand::InvalidateBlock {
                ..
            }
            | ChainCommand::IsConflictInvalidate {
                ..
            }
            | ChainCommand::ClEnforce {
                ..
            } => Class::C,
            #[cfg(feature = "test-support")]
            ChainCommand::Settle {
                ..
            }
            | ChainCommand::ArmFlush {
                ..
            } => Class::C,
            ChainCommand::Query(_) | ChainCommand::QueryOrStop(_) => Class::R,
            ChainCommand::Shutdown => Class::Control,
            #[cfg(test)]
            ChainCommand::TestUnderV(_) => Class::C,
            #[cfg(test)]
            ChainCommand::TestUnderMain(_) => Class::M,
        }
    }

    /// The commands V serves while it holds the rest until startup's ABC completes after
    /// DEC-196 (2) (D38): that ABC, the coins it needs (D51), the step before it in Core's init
    /// (`VerifyLoadedChainstate`, D64), the loading flags its driver sets, and control. A startup
    /// driver must not wait on any other command before it sends that ABC.
    pub fn exempt_from_hold(&self) -> bool {
        self.class() == Class::Control
            || matches!(
                self,
                ChainCommand::ActivateBestChain { .. }
                    | ChainCommand::AttachCoins { .. }
                    | ChainCommand::VerifyLoaded { .. }
                    | ChainCommand::SetLoading { .. }
            )
    }
}
