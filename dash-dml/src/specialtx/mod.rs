//! Core's special-transaction contextual checks (EV1, `designs/EV1-specialtx.md`): what Core's
//! `CheckSpecialTx` decides before a block or the mempool accepts a special transaction.
//!
//! Every check returns `Result<(), TxValidationError>` (CS0) with Core's class and a reason from
//! [`reasons`]. EV1-1 lands the reasons and the provider network fields ([`network`]); EV1-2 the
//! validated payloads ([`payload`]), the trivial checks ([`trivial`]) and the signed preimages
//! ([`sig`]); EV1-3 the dispatcher ([`check_special_tx_inner`]), types 1 and 3 ([`provider`]),
//! types 10 to 12 ([`shared`]) and the shared-collateral covenant ([`covenant`]); EV1-4 types 2
//! and 4 ([`provider`]) and the node's BLS verifier ([`sig::SerialVerifier`]); EV1-5a type 5's
//! `CheckCbTx` ([`cbtx`], DEC-749); LQ2 type 6's `CheckLLMQCommitment` ([`qc`], LQ2 D3); EV1-5b-1
//! the dispatch of types 5, 6 (through [`QuorumSeams`]) and 8 ([`asset`]); types 7 and 9 follow
//! in EV1-5b-2 (EV1 §7).

pub mod asset;
pub mod cbtx;
pub mod covenant;
pub mod network;
pub mod payload;
pub mod provider;
pub mod qc;
pub mod reasons;
pub mod shared;
pub mod sig;
pub mod trivial;

use core::fmt::Debug;

use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::Deployment;
use dash_consensus::deployments::dip0003_enforced;
use dash_consensus::primitives::Tx;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_evo::ctx::EvoCtx;
use dash_evo::effect::EffectSink;
use dash_evo::llmq::LlmqCtx;
use dash_params::BuriedDeployment;
use dash_primitives::TxType;

use crate::MnList;
use crate::blockprocessor::{BlockLookup, QcBlockError};
use crate::netinfo::NetInfoRules;
use crate::store::{NodeMemberError, StoreError};

pub use asset::check_asset_lock_tx;
pub use cbtx::{cbtx_payload, check_cbtx};
pub use covenant::{shared_collateral_spends, shared_collateral_template_outputs};
pub use network::{
    ProviderNetworkFields, check_net_info, check_provider_network_fields,
    is_net_info_trivially_valid,
};
pub use payload::{BlockPayloads, PayloadSlot, ProTxPayload, validated_payload, version_max};
pub use provider::{
    check_pro_reg_tx, check_pro_up_reg_tx, check_pro_up_rev_tx, check_pro_up_serv_tx,
};
pub use shared::{
    check_pro_dis_tx, check_pro_dis_tx_for_list, check_pro_up_share_tx, check_pro_up_shared_reg_tx,
    pro_rata_floor,
};

use qc::{CommitmentVerifier, check_llmq_commitment};
use reasons::{BAD_TX_TYPE_CHECK, BAD_TX_TYPE_DIP3_INACTIVE, FAILED_CHECK_SPECIAL_TX};
use sig::{SigVerifier, VerifyScheme};

/// Core's `SpecialTxRules`, `GetSpecialTxRules(pindex)`: V24 and EVO_SHARES, active after
/// `pindex`. Block path: `pindex` is pprev (the block `cons.prev` names). Mempool path: `pindex`
/// is the tip, also when `cons.prev` is an earlier block. The caller binds it; EV1 never derives
/// it from `cons.prev` (D21).
// Core: evo/specialtxman.h:49-53; validation.cpp:6797-6803
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SpecialTxRules {
    /// `DEPLOYMENT_V24` active after `pindex`.
    pub v24: bool,
    /// `DEPLOYMENT_EVO_SHARES` active after `pindex`.
    pub evo_shares: bool,
}

/// What type 6's `CheckLLMQCommitment` reads beside the transaction (EV1 §2.3, D5): Core's
/// `util_params` and block index, and LQ2's verifier with the sink its member reads record to.
pub struct QuorumSeams<'a> {
    /// The chain, the deployments and the scheme `Verify` runs under (`util_params`).
    pub llmq: LlmqCtx<'a, StoreError>,
    /// `LookupBlockIndex`: any known header, whatever its chain.
    pub index: &'a dyn BlockLookup,
    /// `VerifyNull` and `Verify(base, checkSigs = false)` (LQ2/SB8).
    pub commitments: &'a dyn CommitmentVerifier,
    /// The attempt's effects, in Core's call order.
    pub effects: &'a mut dyn EffectSink,
}

/// What `CheckSpecialTxInner` reads beside the transaction (EV1 §2.3), for the types EV1 has
/// landed. `evo.cons.prev` is Core's `pindexPrev`, the check height minus one.
pub struct SpecialTxCtx<'a, E> {
    /// The consensus context and `check_sigs`.
    pub evo: EvoCtx<'a>,
    pub rules: SpecialTxRules,
    /// `GetListForBlock(pindexPrev)`, resolved by the caller.
    pub list: &'a MnList,
    /// Block path: the view Core passes to `ProcessSpecialTxsInBlock`; mempool path:
    /// `CoinsTip()`, never the mempool input cache (D13).
    pub coins: &'a dyn CoinsView<Error = E>,
    /// What `CheckProviderNetworkFields` and the migration step read of `Params()`.
    pub net_info: NetInfoRules,
    /// BLS `VerifyInsecure` (D2).
    pub verifier: &'a dyn SigVerifier,
    /// Type 6's seams; `None` where the caller has none to bind yet, which faults a type-6
    /// transaction with [`SpecialTxFault::Unbound`].
    pub quorum: Option<QuorumSeams<'a>>,
}

impl<E> SpecialTxCtx<'_, E> {
    /// `DeploymentActiveAfter(pindexPrev, V19)`.
    fn v19(&self) -> bool {
        self.active_after(BuriedDeployment::V19)
    }

    /// The scheme a BLS signature is verified under: Core's global flag, basic once V19 is active
    /// after `pindexPrev` (the block's parent, or the tip on the mempool path).
    fn verify_scheme(&self) -> VerifyScheme {
        VerifyScheme::after_v19(self.v19())
    }

    fn active_after(&self, deployment: BuriedDeployment) -> bool {
        let cons = self.evo.cons;
        cons.deployments.active_after(cons.prev, Deployment::Buried(deployment))
    }

    /// `pindexPrev->nHeight`. The dispatcher's DIP3 gate rejects a check without a
    /// `pindexPrev` (genesis's), so the per-type checks always have one.
    fn prev_height(&self) -> i32 {
        self.evo.cons.height as i32 - 1
    }

    /// `DeploymentDIP0003Enforced(pindexPrev->nHeight)`.
    fn dip3_enforced(&self) -> bool {
        dip0003_enforced(self.prev_height(), self.evo.cons.params)
    }
}

/// A check that ends without a verdict. It aborts the attempt: never an acceptance, never a
/// reject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecialTxFault<E> {
    /// A type whose checks have not landed (EV1-5b-2: types 7 and 9). Each arm goes when its type
    /// lands (DEC-663).
    Unported(TxType),
    /// A type whose seams the caller did not bind ([`SpecialTxCtx::quorum`]).
    Unbound(TxType),
    /// The coins view failed a read.
    Coins(E),
    /// The verifier gave no verdict on a BLS signature (D2).
    VerifyUnavailable,
    /// Type 6's seams gave no verdict: a store fault, or a signature outside the BLS domain
    /// (LQ2's [`QcBlockError`], which the caller maps as its quorum processor's). An unknown
    /// LLMQ type is Core's throw instead, `failed-check-special-tx` (D25).
    Commitment(QcBlockError),
}

/// A verdict, or a fault.
pub type CheckResult<E> = Result<Result<(), TxValidationError>, SpecialTxFault<E>>;

/// Core's catch around the type checks: an exception a ported function raises where Core's
/// throws (§2.2) is TX_CONSENSUS `failed-check-special-tx`.
// Core: evo/specialtxman.cpp:267-271
pub(crate) fn core_throw() -> TxValidationError {
    TxValidationError::new(TxValidationResult::Consensus, FAILED_CHECK_SPECIAL_TX)
}

/// A TX_CONSENSUS reject.
pub(crate) fn consensus<T>(reason: &'static str) -> Result<T, TxValidationError> {
    Err(TxValidationError::new(TxValidationResult::Consensus, reason))
}

/// A TX_BAD_SPECIAL reject.
pub(crate) fn bad_special<T>(reason: &'static str) -> Result<T, TxValidationError> {
    Err(TxValidationError::new(TxValidationResult::BadSpecial, reason))
}

/// Type 6's fault for a check without a `pindexPrev` past DIP0003's gate.
pub const NULL_PREV: &str = "CheckLLMQCommitment without a pindexPrev";

/// Core's `CheckSpecialTxInner` (§3.2): accepts a transaction without a payload field, rejects
/// any other before DIP3, then the checks of its type. `payload` is the transaction's slot
/// ([`BlockPayloads`] on the block path, [`Tx::payload`] otherwise).
///
/// Types 7 and 9 fault with [`SpecialTxFault::Unported`] until their slice lands; the block
/// path's coinbase rule is the caller's (§3.2 "Block path", D18), so a type-5 transaction here is
/// the mempool's or one past the block's first.
// Core: evo/specialtxman.cpp:217-274
pub fn check_special_tx_inner<E: Debug>(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    ctx: &mut SpecialTxCtx<'_, E>,
) -> CheckResult<E> {
    if !tx.has_extra_payload_field() {
        return Ok(Ok(()));
    }
    if !ctx.active_after(BuriedDeployment::Dip0003) {
        return Ok(bad_special(BAD_TX_TYPE_DIP3_INACTIVE));
    }
    match tx.tx_type() {
        TxType::ProviderRegister => check_pro_reg_tx(tx, payload, ctx),
        TxType::ProviderUpdateService => check_pro_up_serv_tx(tx, payload, ctx),
        TxType::ProviderUpdateRegistrar => check_pro_up_reg_tx(tx, payload, ctx),
        TxType::ProviderUpdateRevoke => check_pro_up_rev_tx(tx, payload, ctx),
        TxType::ProviderDissolve => Ok(check_pro_dis_tx(tx, payload, ctx)),
        TxType::ProviderUpdateShare => Ok(check_pro_up_share_tx(tx, payload, ctx)),
        TxType::ProviderUpdateSharedRegistrar => Ok(check_pro_up_shared_reg_tx(tx, payload, ctx)),
        // Core: evo/specialtxman.cpp:247-256
        TxType::CoinbaseCommitment => {
            Ok(cbtx_payload(tx, payload).and_then(|c| check_cbtx(c, ctx.evo.cons, ctx.rules.v24)))
        }
        // Core: evo/specialtxman.cpp:257-258
        TxType::QuorumCommitment => {
            // Core: llmq/utils.h:36; gsl/pointers.h:126-132
            // Core builds `UtilParameters` from `pindexPrev`, whose `m_base_index` is a
            // `gsl::not_null` (`llmq/utils.h:36`): a null one terminates before any check, so
            // there is no verdict and Core's catch never runs.
            let Some(prev) = ctx.evo.cons.prev else {
                return Err(SpecialTxFault::Commitment(QcBlockError::NoReason(NULL_PREV)));
            };
            let Some(q) = ctx.quorum.as_mut() else {
                return Err(SpecialTxFault::Unbound(TxType::QuorumCommitment));
            };
            match check_llmq_commitment(tx, prev, &q.llmq, q.index, q.commitments, &mut *q.effects)
            {
                // D25: `IsQuorumTypeEnabled`'s throw, which Core's catch turns into a reject.
                // Core: evo/specialtxman.cpp:267-271
                Err(QcBlockError::Fault(NodeMemberError::UnknownLlmqType(_))) => {
                    Ok(Err(core_throw()))
                }
                verdict => verdict.map_err(SpecialTxFault::Commitment),
            }
        }
        // Core: evo/specialtxman.cpp:262-263
        TxType::AssetLock => Ok(check_asset_lock_tx(tx, payload, ctx.rules.v24)),
        t @ (TxType::MnhfSignal | TxType::AssetUnlock) => Err(SpecialTxFault::Unported(t)),
        _ => Ok(bad_special(BAD_TX_TYPE_CHECK)),
    }
}

#[cfg(test)]
pub(crate) mod testctx;
#[cfg(test)]
mod tests;
