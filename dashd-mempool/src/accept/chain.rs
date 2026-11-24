//! [`SpecialTxChain`] on V's [`MempoolTurn`]: Core's `CheckSpecialTx(tx, pindex, rules,
//! CoinsTip(), /*check_sigs=*/true)` on the mempool path, `CheckSpecialTxInner(chain = nullptr,
//! …, indexes = nullopt)` (`evo/specialtxman.cpp:276-281` at v24.0.0), over
//! [`MempoolTurn::with_llmq`].
//!
//! Its conditions (MP1a-5 review, item 3):
//! - (a) the BLS decode context and verify scheme are the tip's for every `pindex`: Core's
//!   process-global `bls_legacy_scheme` follows the tip (`evo/specialtxman.cpp:1043-1047`,
//!   `:1068-1072`), whichever block `CheckSpecialTxForMempool` checks at (`validation.cpp:831-836`);
//! - (b) `LookupBlockIndex` is [`TipLlmq::index`], any known header;
//! - (c) the DIP0003 gate (`evo/specialtxman.cpp:227`) and `ScanQuorums`' `IsQuorumTypeEnabled`
//!   (`llmq/quorumsman.cpp:183`) are evaluated at `pindex`, as is every other deployment the
//!   checks read; only `rules` and the scheme are the tip's;
//! - (d) faults are mapped in `store_throw` and `check_special_tx_at`.

use std::fmt::Debug;
use std::sync::Arc;

use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{ChainAncestry, CheckpointView, Deployment, DeploymentView};
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_consensus::{BlockId, ConsensusCtx, Tx};
use dash_dml::blockprocessor::QcBlockError;
use dash_dml::netinfo::NetInfoRules;
use dash_dml::specialtx::qc::CommitmentVerifier;
use dash_dml::specialtx::quorum::{
    MAX_QUORUM_SCAN, QuorumKey, QuorumLookup, QuorumScan, SeamError,
};
use dash_dml::specialtx::reasons::{BAD_TX_TYPE_DIP3_INACTIVE, FAILED_CHECK_SPECIAL_TX};
use dash_dml::specialtx::sig::{FixedScheme, SerialVerifier, VerifyScheme};
use dash_dml::specialtx::{
    self, QuorumSeams, SpecialTxCtx, SpecialTxFault, check_special_tx_inner,
};
use dash_dml::{MnList, NodeMemberError, StoreError};
use dash_evo::EffectSink;
use dash_evo::ctx::EvoCtx;
use dash_evo::llmq::{
    LlmqCommitment, LlmqCtx, LlmqError, MemberError, MinedCommitmentReader, QuorumMaterializer,
    ScanCacheStore, ScanEntry, ScanOverload, scan_quorums,
};
use dash_params::{BuriedDeployment, ChainSetup, LlmqType};
use dash_pkc::bls::{BlsCtx, BlsLazyPk};
use dash_primitives::{BlockHash, QuorumHash, TxType};
use dashd_chainstate::chain::{Fatal, MempoolTurn, OnVMembersError, TipFacts, TipLlmq};
use dashd_chainstate::connect::SpecialTxRules;

use super::special::SpecialTxChain;

/// What a check reads of V besides the transaction and the block it is checked at.
struct ChainInputs<'a> {
    pub params: &'a ChainSetup,
    pub deployments: &'a dyn DeploymentView,
    pub ancestry: &'a dyn ChainAncestry,
    /// The tip's published scheme, Core's `bls_legacy_scheme`.
    pub tip_legacy: bool,
    pub net_info: NetInfoRules,
}

impl SpecialTxChain for MempoolTurn<'_> {
    /// Core's first two steps of `CheckSpecialTxInner` run before any evo read, so a node
    /// without evo state still rejects below DIP0003 as Core does; past it, a check without evo
    /// state is a wiring fault, never a verdict.
    // Core: evo/specialtxman.cpp:217-229, :276-281 at v24.0.0
    fn check_special_tx(
        &mut self,
        tx: &Tx,
        pindex: BlockId,
        rules: SpecialTxRules,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        if !tx.has_extra_payload_field() {
            return Ok(Ok(()));
        }
        let turn: &MempoolTurn<'_> = self;
        let dip3 = Deployment::Buried(BuriedDeployment::Dip0003);
        if !turn.deployments().active_after(Some(pindex), dip3) {
            return Ok(Err(TxValidationError::new(
                TxValidationResult::BadSpecial,
                BAD_TX_TYPE_DIP3_INACTIVE,
            )));
        }
        let tip =
            turn.tip().ok_or_else(|| Fatal::assert("CheckSpecialTx without an active tip"))?;
        let net_info =
            turn.net_info().ok_or_else(|| Fatal::new("CheckSpecialTx without evo state"))?;
        let inputs = ChainInputs {
            params: turn.params(),
            deployments: turn.deployments(),
            ancestry: turn.ancestry(),
            tip_legacy: turn.bls().is_legacy(),
            net_info,
        };
        let list = || turn.try_list_at_block(pindex);
        let mut verdict = None;
        let coins = turn.with_coins_tip(&mut |coins| {
            let mut checked = None;
            let llmq = turn.with_llmq(&mut |at| {
                checked = Some(check_special_tx_at(
                    &inputs,
                    tx,
                    pindex,
                    rules,
                    &list,
                    coins,
                    MempoolQuorums::new(at, tip),
                    &MempoolTurn::coins_read_fault,
                ));
                Ok(())
            });
            verdict = Some(match llmq {
                Ok(()) => checked.expect("with_llmq answers Ok only after its call"),
                Err(e) => llmq_refusal(e),
            });
        })?;
        if coins.is_none() {
            return Err(Fatal::new("CheckSpecialTx over a connector without coins"));
        }
        verdict.expect("with_coins_tip answers Some only after its call")
    }
}

/// `with_llmq`'s own refusal: its parameters outside `LlmqCtx`'s domain, or no evo state. Neither
/// has a Core counterpart (Core asserts its parameters at startup and always has its evo DB), so
/// neither is a reject.
fn llmq_refusal(e: OnVMembersError) -> Result<Result<(), TxValidationError>, Fatal> {
    match e {
        OnVMembersError::Params(e) => {
            Err(Fatal::new(format!("CheckSpecialTx LLMQ parameters: {e}")))
        }
        OnVMembersError::Members(e) => store_throw(member_store_error(e)),
    }
}

/// Core's catch in `CheckSpecialTxInner`: an evo read that throws there is `failed-check-special-
/// tx` (`TX_CONSENSUS`), logged as Core logs it. A readiness refusal (no evo state) has no Core
/// counterpart and is a fault, never a reject.
// Core: evo/specialtxman.cpp:267-271 at v24.0.0
pub(crate) fn store_throw(e: StoreError) -> Result<Result<(), TxValidationError>, Fatal> {
    if let Some(capability) = e.capability() {
        return Err(Fatal::new(format!("CheckSpecialTx: {capability} is not built")));
    }
    tracing::info!("CheckSpecialTxInner -- failed: {e}");
    Ok(Err(TxValidationError::new(TxValidationResult::Consensus, FAILED_CHECK_SPECIAL_TX)))
}

/// An LLMQ computation's error as the store error Core would have thrown: `IsQuorumTypeEnabled`'s
/// throw for an unknown type included (`validation.cpp:6707-6708` at v24.0.0).
fn member_store_error(e: LlmqError<StoreError>) -> StoreError {
    match NodeMemberError::from(e) {
        NodeMemberError::Store(e) => e,
        NodeMemberError::UnknownLlmqType(t) => {
            StoreError::inconsistent(format!("a quorum lookup of LLMQ type {t}"))
        }
    }
}

/// Whether a type's checks read `GetListForBlock(pindexPrev)`: the provider checks alone.
fn reads_list(t: TxType) -> bool {
    matches!(
        t,
        TxType::ProviderRegister
            | TxType::ProviderUpdateService
            | TxType::ProviderUpdateRegistrar
            | TxType::ProviderUpdateRevoke
            | TxType::ProviderDissolve
            | TxType::ProviderUpdateShare
            | TxType::ProviderUpdateSharedRegistrar
    )
}

/// `CheckSpecialTxInner` at `pindex` on the mempool path over `quorums`, `coins` (CoinsTip) and
/// the list `list` resolves (read only for the provider types).
///
/// # Errors
///
/// A fault without a verdict: a coins read fault (`coins_fault`), a readiness refusal, a
/// signature outside the BLS domain, or one of Core's asserts.
#[allow(clippy::too_many_arguments)]
fn check_special_tx_at<E: Debug>(
    inputs: &ChainInputs<'_>,
    tx: &Tx,
    pindex: BlockId,
    rules: SpecialTxRules,
    list: &dyn Fn() -> Result<Arc<MnList>, StoreError>,
    coins: &dyn CoinsView<Error = E>,
    mut quorums: MempoolQuorums<'_, '_>,
    coins_fault: &dyn Fn(E) -> Fatal,
) -> Result<Result<(), TxValidationError>, Fatal> {
    // PreChecks rejects a quorum commitment (`qc-not-allowed`) before any special-tx check.
    // Core: validation.cpp:866-869 at v24.0.0
    if tx.tx_type() == TxType::QuorumCommitment {
        return Err(Fatal::assert("CheckSpecialTx of a quorum commitment in the mempool"));
    }
    let list = if reads_list(tx.tx_type()) {
        match list() {
            Ok(list) => list,
            Err(e) => return store_throw(e),
        }
    } else {
        Arc::new(MnList::default_pre_dip3())
    };
    let header = inputs.ancestry.header(pindex);
    let cons = ConsensusCtx {
        params: inputs.params,
        height: header.height + 1,
        prev: Some(pindex),
        chain: inputs.ancestry,
        deployments: inputs.deployments,
        checkpoints: &NoCheckpoints,
        bls: BlsCtx::from_legacy(inputs.tip_legacy),
        median_time_past: header.mtp,
        header_check_time: 0,
    };
    let verifier = FixedScheme {
        inner: &SerialVerifier,
        scheme: VerifyScheme::after_v19(!inputs.tip_legacy),
    };
    let mut effects = Vec::new();
    let mut cx = SpecialTxCtx {
        evo: EvoCtx {
            cons: &cons,
            check_sigs: true,
        },
        rules: specialtx::SpecialTxRules {
            v24: rules.v24,
            evo_shares: rules.evo_shares,
            ipv4_optional: rules.ipv4_optional,
        },
        list: &list,
        coins,
        net_info: inputs.net_info,
        verifier: &verifier,
        quorum: Some(QuorumSeams {
            llmq: *quorums.ctx,
            index: quorums.index,
            commitments: &NoCommitments,
            quorums: &mut quorums,
            effects: &mut effects,
        }),
        unlock_indexes: None,
    };
    let verdict = match check_special_tx_inner(tx, tx.payload(), &mut cx) {
        Ok(verdict) => Ok(verdict),
        Err(SpecialTxFault::Coins(e)) => Err(coins_fault(e)),
        Err(SpecialTxFault::Quorum(SeamError::Store(e))) => store_throw(e),
        Err(SpecialTxFault::Quorum(SeamError::ScanCapacity)) => {
            Err(Fatal::assert("CheckSpecialTx: a quorum scan above MAX_QUORUM_SCAN"))
        }
        // EV1 D2: no verdict, so never a reject.
        Err(SpecialTxFault::VerifyUnavailable) => {
            Err(Fatal::abort_node("CheckSpecialTx: a signature outside the BLS domain"))
        }
        Err(SpecialTxFault::NoLlmq(t)) => {
            Err(Fatal::assert(format!("CheckSpecialTx: no LLMQ {t:?} in the params")))
        }
        Err(SpecialTxFault::Unbound(t) | SpecialTxFault::NoPrev(t)) => {
            Err(Fatal::assert(format!("CheckSpecialTx: type {t:?} without its seams")))
        }
        Err(SpecialTxFault::Commitment(e)) => {
            Err(Fatal::assert(format!("CheckSpecialTx: a commitment check: {e:?}")))
        }
    };
    debug_assert!(effects.is_empty(), "the mempool's quorum reads write no rows");
    verdict
}

/// The special-tx checks read no checkpoint.
struct NoCheckpoints;

impl CheckpointView for NoCheckpoints {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        None
    }
}

/// Type 6's seams, which the mempool never reaches (`qc-not-allowed`).
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
        Err(QcBlockError::NoReason("a quorum commitment in the mempool"))
    }
}

/// The quorum manager's overloads without a chain, which the mempool path calls:
/// `GetQuorum(type, quorumHash)` and `ScanQuorums(type, pindexStart, n)`. Both answer from the
/// mined commitments on V's active chain (`HasQuorum` → `HasMinedCommitment`, the active-chain
/// overload, `llmq/blockprocessor.cpp:644-648`). The key a check reads is the commitment's
/// `quorumPublicKey`, so the quorum's members are not built and the object and scan caches are
/// neither read nor written: a cache answers what a fresh scan from the same store block does.
// Core: llmq/quorumsman.cpp:164-169, :179-295, :491-527 at v24.0.0
struct MempoolQuorums<'a, 'c> {
    ctx: &'a LlmqCtx<'c, StoreError>,
    mined: &'a dyn MinedCommitmentReader<Error = StoreError>,
    index: &'a dyn dash_dml::blockprocessor::BlockLookup,
    tip: BlockId,
}

impl<'a, 'c> MempoolQuorums<'a, 'c> {
    fn new(at: TipLlmq<'a, 'c>, tip: BlockId) -> Self {
        MempoolQuorums {
            ctx: at.ctx,
            mined: at.mined,
            index: at.index,
            tip,
        }
    }

    fn materializer(&self) -> MinedOnly<'_> {
        MinedOnly {
            mined: self.mined,
            tip: self.tip,
        }
    }
}

fn seam_error(e: LlmqError<StoreError>) -> SeamError {
    SeamError::Store(member_store_error(e))
}

impl QuorumLookup for MempoolQuorums<'_, '_> {
    // Core: llmq/quorumsman.cpp:491-512, :529-546 at v24.0.0
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        _: &mut dyn EffectSink,
    ) -> Result<Option<QuorumKey>, SeamError> {
        let Some((base, _)) = self.index.lookup(&BlockHash::from(*quorum_hash.as_bytes())) else {
            return Ok(None);
        };
        let quorum = self.materializer().get_quorum(llmq_type, base, true).map_err(seam_error)?;
        Ok(quorum.map(|q| {
            let key = q.commitment.quorum_public_key();
            QuorumKey {
                quorum_hash: q.commitment.quorum_hash(),
                public_key: BlsLazyPk::from_value(key.value().clone(), key.scheme()),
            }
        }))
    }

    // Core: llmq/quorumsman.cpp:164-169, :179-295 (`chain == nullptr`) at v24.0.0
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
        let mut mat = self.materializer();
        let scanned = scan_quorums(
            self.ctx,
            self.mined,
            &mut ColdScanCache,
            &mut mat,
            llmq_type,
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

/// A quorum as the mempool's checks read it: its base and its mined commitment.
#[derive(Clone)]
struct MinedQuorum {
    base: BlockId,
    commitment: Arc<LlmqCommitment>,
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

/// `GetQuorum(type, base)`: `nullptr` unless the active chain mined the quorum's commitment
/// (`HasQuorum`, then `BuildQuorumFromCommitment`'s `GetMinedCommitment`).
// Core: llmq/quorumsman.cpp:72-78, :529-546 at v24.0.0
struct MinedOnly<'a> {
    mined: &'a dyn MinedCommitmentReader<Error = StoreError>,
    tip: BlockId,
}

impl QuorumMaterializer for MinedOnly<'_> {
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

/// A scan cache that is always cold: [`MempoolQuorums`]' scans recompute what a hit holds.
struct ColdScanCache;

impl ScanCacheStore<MinedQuorum> for ColdScanCache {
    fn get(&mut self, _: LlmqType, _: &BlockHash) -> Option<Vec<MinedQuorum>> {
        None
    }

    fn insert(&mut self, _: LlmqType, _: BlockHash, _: Vec<MinedQuorum>) {}
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod tests;
