//! [`TipReads`] over V's tip in the template's query turn: `TestPackageTransactions`'
//! `CheckSpecialTx(tx, Tip(), GetSpecialTxRules(Tip()), CoinsTip(), check_sigs)`, which is
//! `CheckSpecialTxInner(chain = nullptr, …, indexes = nullopt)` (Core v24.0.2
//! src/evo/specialtxman.cpp:276-281), and the coins and quorum members the list trial reads.
//! Each call takes the coins cell, then the LLMQ view, for that call only.

use std::cell::Cell;
use std::sync::Arc;

use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{
    BlockId, ChainAncestry, CheckpointView, Deployment, DeploymentView, bls_ctx_for_block,
};
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_consensus::{ConsensusCtx, Tx};
use dash_dml::blockprocessor::{BlockLookup, QcBlockError, QcCtx};
use dash_dml::netinfo::NetInfoRules;
use dash_dml::specialtx::qc::CommitmentVerifier;
use dash_dml::specialtx::quorum::{
    MAX_QUORUM_SCAN, QuorumKey, QuorumLookup, QuorumScan, SeamError,
};
use dash_dml::specialtx::reasons::{BAD_TX_TYPE_DIP3_INACTIVE, FAILED_CHECK_SPECIAL_TX};
use dash_dml::specialtx::sig::{FixedScheme, SerialVerifier, VerifyScheme};
use dash_dml::specialtx::{
    QuorumSeams, SpecialTxCtx, SpecialTxFault, SpecialTxRules, check_special_tx_inner,
};
use dash_dml::{DmlMemberSource, MnList, NodeMemberError, StoreError};
use dash_evo::EffectSink;
use dash_evo::ctx::EvoCtx;
use dash_evo::llmq::{
    LlmqCommitment, LlmqCtx, LlmqError, MemberError, MinedCommitmentReader, QuorumMaterializer,
    ScanCacheStore, ScanEntry, ScanOverload, scan_quorums,
};
use dash_params::{BuriedDeployment, ChainSetup, LlmqType};
use dash_primitives::{BlockHash, QuorumHash, TxType};
use dashd_chainstate::chain::{ChainQuery, EvoView};
use dashd_chainstate::store::CoinsReadError;

use crate::evo::BlockError;
use crate::package::TipReads;
use crate::source::{coins_refusal, qc_refusal};

/// V's tip in one query turn.
pub(crate) struct QueryTip<'q> {
    pub q: &'q ChainQuery<'q>,
    pub view: EvoView<'q>,
    pub params: &'q ChainSetup,
    pub ancestry: &'q dyn ChainAncestry,
    /// The tip, `pindexPrev`.
    pub prev: BlockId,
    pub height: u32,
    /// `GetListForBlock(Tip())`.
    pub tip_list: &'q MnList,
    /// `GetSpecialTxRules(Tip())`.
    pub rules: SpecialTxRules,
    pub net_info: NetInfoRules,
    /// The effects the template's list builder emitted, and the snapshot rows its member
    /// computations stored, so far in the turn (A1 §2.13).
    pub effects: &'q Cell<u64>,
}

impl TipReads for QueryTip<'_> {
    type CoinsError = CoinsReadError;

    // Core: v24.0.2 src/evo/specialtxman.cpp:217-229, :276-281
    fn check_special_tx(
        &self,
        tx: &Tx,
        check_sigs: bool,
    ) -> Result<Result<(), TxValidationError>, BlockError> {
        if !tx.has_extra_payload_field() {
            return Ok(Ok(()));
        }
        let dip3 = Deployment::Buried(BuriedDeployment::Dip0003);
        if !self.q.deployments.active_after(Some(self.prev), dip3) {
            return Ok(Err(TxValidationError::new(
                TxValidationResult::BadSpecial,
                BAD_TX_TYPE_DIP3_INACTIVE,
            )));
        }
        let checked = self.view.with_tip_coins(|coins| {
            self.view.with_qc(self.q.bls(), |qc| {
                self.check_at(tx, check_sigs, coins, &MinedQuorums::new(qc, self.prev))
            })
        });
        checked
            .map_err(|f| BlockError::Fault(f.0))?
            .ok_or_else(|| coins_refusal(self.height))?
            .map_err(|e| qc_refusal(self.height, e))?
    }

    fn with_tip_coins<R>(
        &self,
        f: impl FnOnce(&dyn CoinsView<Error = CoinsReadError>) -> R,
    ) -> Result<R, BlockError> {
        self.view
            .with_tip_coins(f)
            .map_err(|f| BlockError::Fault(f.0))?
            .ok_or_else(|| coins_refusal(self.height))
    }

    fn with_list_inputs<R>(
        &self,
        f: impl FnOnce(
            &dyn CoinsView<Error = CoinsReadError>,
            &DmlMemberSource<'_>,
            &mut dyn EffectSink,
        ) -> R,
    ) -> Result<R, BlockError> {
        let bls = bls_ctx_for_block(self.params, self.height);
        let mut emitted = Vec::new();
        let (out, stored) = self
            .view
            .with_tip_coins(|coins| {
                self.view.with_template_counted(bls, |qc, _| f(coins, qc.members, &mut emitted))
            })
            .map_err(|f| BlockError::Fault(f.0))?
            .ok_or_else(|| coins_refusal(self.height))?
            .map_err(|e| qc_refusal(self.height, e))?;
        self.effects.update(|n| n + emitted.len() as u64 + stored);
        Ok(out)
    }
}

impl QueryTip<'_> {
    /// `CheckSpecialTxInner` at the tip over `coins` (`CoinsTip()`) and the mined quorums.
    fn check_at(
        &self,
        tx: &Tx,
        check_sigs: bool,
        coins: &dyn CoinsView<Error = CoinsReadError>,
        quorums: &MinedQuorums<'_, '_>,
    ) -> Result<Result<(), TxValidationError>, BlockError> {
        // The mempool refuses a quorum commitment (`qc-not-allowed`), so none is selected.
        if tx.tx_type() == TxType::QuorumCommitment {
            return Err(BlockError::Fault("CheckSpecialTx of a pooled quorum commitment".into()));
        }
        let header = self.ancestry.header(self.prev);
        let bls = self.q.bls();
        let cons = ConsensusCtx {
            params: self.params,
            height: header.height + 1,
            prev: Some(self.prev),
            chain: self.ancestry,
            deployments: &self.q.deployments,
            checkpoints: &NoCheckpoints,
            bls,
            median_time_past: header.mtp,
            header_check_time: 0,
        };
        let verifier = FixedScheme {
            inner: &SerialVerifier,
            scheme: VerifyScheme::after_v19(!bls.is_legacy()),
        };
        let mut lookup = quorums.clone();
        let mut effects = Vec::new();
        let mut cx = SpecialTxCtx {
            evo: EvoCtx {
                cons: &cons,
                check_sigs,
            },
            rules: self.rules,
            list: self.tip_list,
            coins,
            net_info: self.net_info,
            verifier: &verifier,
            quorum: Some(QuorumSeams {
                llmq: *quorums.ctx,
                index: quorums.index,
                commitments: &NoCommitments,
                quorums: &mut lookup,
                effects: &mut effects,
            }),
            unlock_indexes: None,
        };
        let fault = |what: String| Err(BlockError::Fault(format!("CheckSpecialTx: {what}")));
        match check_special_tx_inner(tx, tx.payload(), &mut cx) {
            Ok(verdict) => Ok(verdict),
            Err(SpecialTxFault::Coins(e)) => fault(format!("coins read: {e}")),
            Err(SpecialTxFault::Quorum(SeamError::Store(e))) => store_throw(self.height, e),
            Err(fault_) => fault(format!("{fault_:?}")),
        }
    }
}

/// Core's catch in `CheckSpecialTxInner`: a quorum read that throws is `failed-check-special-tx`
/// (`TX_CONSENSUS`), logged as Core logs it; a read V has not built refuses the template.
// Core: v24.0.2 src/evo/specialtxman.cpp:268-271
fn store_throw(height: u32, e: StoreError) -> Result<Result<(), TxValidationError>, BlockError> {
    if e.capability().is_some() {
        return Err(BlockError::Refused(crate::source::store_refusal(height, e)));
    }
    tracing::info!("CheckSpecialTxInner -- failed: {e}");
    Ok(Err(TxValidationError::new(TxValidationResult::Consensus, FAILED_CHECK_SPECIAL_TX)))
}

/// The special-tx checks read no checkpoint.
struct NoCheckpoints;

impl CheckpointView for NoCheckpoints {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        None
    }
}

/// Type 6's seams, which a pooled transaction never reaches.
struct NoCommitments;

impl CommitmentVerifier for NoCommitments {
    fn verify_null(&self, _: &LlmqCommitment) -> bool {
        false
    }

    fn verify(
        &self,
        _: &LlmqCommitment,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<bool, QcBlockError> {
        Err(QcBlockError::NoReason("a quorum commitment in a template's package"))
    }
}

/// The quorum manager's overloads without a chain: `GetQuorum(type, quorumHash)` and
/// `ScanQuorums(type, pindexStart, n)`, answered from the commitments mined on V's active chain.
/// The checks read only `quorumPublicKey`, so no member is built and no cache is read or
/// written: a cache answers what a fresh scan does.
// Core: v24.0.2 src/llmq/quorumsman.cpp:72-78, :164-169, :179-295, :491-546
#[derive(Clone)]
struct MinedQuorums<'a, 'c> {
    ctx: &'a LlmqCtx<'c, StoreError>,
    mined: &'a dyn MinedCommitmentReader<Error = StoreError>,
    /// `LookupBlockIndex`: any known header.
    index: &'a dyn BlockLookup,
    tip: BlockId,
}

impl<'a, 'c> MinedQuorums<'a, 'c> {
    fn new(qc: &'a QcCtx<'c>, tip: BlockId) -> Self {
        MinedQuorums {
            ctx: &qc.llmq,
            mined: qc.mined,
            index: qc.index,
            tip,
        }
    }
}

fn seam_error(e: LlmqError<StoreError>) -> SeamError {
    SeamError::Store(match NodeMemberError::from(e) {
        NodeMemberError::Store(e) => e,
        NodeMemberError::UnknownLlmqType(t) => {
            StoreError::inconsistent(format!("a quorum lookup of LLMQ type {t}"))
        }
    })
}

impl QuorumLookup for MinedQuorums<'_, '_> {
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        _: &mut dyn EffectSink,
    ) -> Result<Option<QuorumKey>, SeamError> {
        let hash = BlockHash::from(*quorum_hash.as_bytes());
        let Some((base, _)) = self.index.lookup(&hash) else {
            return Ok(None);
        };
        let quorum = QuorumMaterializer::get_quorum(&mut self.clone(), llmq_type, base, true)
            .map_err(seam_error)?;
        Ok(quorum.map(|q| crate::evo::quorum_key(&q.commitment)))
    }

    fn scan_quorums(
        &mut self,
        llmq_type: LlmqType,
        at: BlockId,
        n: usize,
        out: &mut QuorumScan,
        _: &mut dyn EffectSink,
    ) -> Result<(), SeamError> {
        if n > MAX_QUORUM_SCAN {
            return Err(SeamError::ScanCapacity);
        }
        out.clear();
        let mut mat = self.clone();
        let scanned = scan_quorums(
            self.ctx,
            self.mined,
            &mut ColdScanCache,
            &mut mat,
            llmq_type,
            self.tip,
            at,
            n,
            ScanOverload::Cached,
        )
        .map_err(seam_error)?;
        for quorum in &scanned.quorums {
            out.push(quorum.commitment.quorum_hash())?;
        }
        Ok(())
    }
}

/// A quorum as the checks read it.
#[derive(Clone)]
struct MinedQuorum {
    base: BlockId,
    commitment: Arc<dash_consensus::primitives::P2pCommitment>,
}

impl ScanEntry for MinedQuorum {
    fn base(&self) -> BlockId {
        self.base
    }

    fn quorum_hash(&self) -> QuorumHash {
        self.commitment.quorum_hash()
    }

    fn quorum_index(&self) -> i16 {
        self.commitment.quorum_index().unwrap_or(0)
    }
}

impl QuorumMaterializer for MinedQuorums<'_, '_> {
    type Quorum = MinedQuorum;
    type Error = StoreError;

    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        base: BlockId,
        _populate_cache: bool,
    ) -> Result<Option<MinedQuorum>, LlmqError<StoreError>> {
        let hash = QuorumHash::from(*self.mined.chain().header(base).hash.as_bytes());
        let mined = self.mined.mined_commitment(llmq_type, &hash, self.tip);
        Ok(mined.map_err(MemberError::View)?.map(|(commitment, _)| MinedQuorum {
            base,
            commitment,
        }))
    }
}

struct ColdScanCache;

impl ScanCacheStore<MinedQuorum> for ColdScanCache {
    fn get(&mut self, _: LlmqType, _: &BlockHash) -> Option<Vec<MinedQuorum>> {
        None
    }

    fn insert(&mut self, _: LlmqType, _: BlockHash, _: Vec<MinedQuorum>) {}
}
