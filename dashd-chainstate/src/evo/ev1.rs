//! The EV1 seam (D44, DEC-581): H4's steps 0, 1 and 4 and hook H8, the checks EV1-2's payloads,
//! EV1-3's dispatcher and covenant and EV1-5's `check_cbtx` own. [`DmlChecks`] binds those that
//! have merged (CH7a-2b); [`Ev1NotBuilt`] gives Core's verdicts below DIP0003 and refuses past it.

use std::cell::OnceCell;
use std::sync::Arc;

use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{ConsensusCtx, Deployment};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::{Block, Tx, TxValidationError, TxValidationResult};
use dash_dml::creditpool::CreditPool;
use dash_dml::netinfo::NetInfoRules;
use dash_dml::specialtx::{
    self, BlockPayloads, SpecialTxFault, check_special_tx_inner, shared_collateral_template_outputs,
};
use dash_dml::{MnList, StoreError};
use dash_evo::ctx::EvoCtx;
use dash_params::BuriedDeployment;
use dash_primitives::{CoinbaseCommitment, TxType};
use dash_types::Numeric as _;

use super::node::store_failure;
use crate::chain::Fatal;
use crate::connect::{
    SpecialTxFailure, SpecialTxRules, pre_dip3_cbtx, pre_dip3_special_tx, read_failed, tx_reject,
};
use crate::store::CoinsReadError;

/// The capability [`Ev1NotBuilt`] refuses for.
pub const SPECIAL_TX_CHECKS: &str = "special-tx checks (EV1)";

/// What EV1's checks read of the attempt besides the block (EV1 §2's `SpecialTxCtx`): the
/// attempt's consensus context (D48), and the arguments of `CheckSpecialTxInner` (`stm:956`).
#[derive(Clone, Copy)]
pub struct SpecialTxCtx<'a> {
    /// The block's context: `pindexPrev`, the deployments after it, the block's height.
    pub cons: &'a ConsensusCtx<'a>,
    pub rules: SpecialTxRules,
    /// The pre-block view (`v:2713`).
    pub coins: &'a dyn CoinsView<Error = CoinsReadError>,
    /// Step 3's credit-pool indexes, `None` below V20 (Core's empty `CRangesSet`).
    pub indexes: Option<&'a CreditPool>,
    /// `check_sigs`: `fCheckCbTxMerkleRoots`.
    pub check_sigs: bool,
    /// `GetListForBlock(pindexPrev)`, which the ProTx checks read.
    pub list: &'a PrevList<'a>,
    /// What `CheckProviderNetworkFields` reads of `Params()`.
    pub net_info: NetInfoRules,
}

impl SpecialTxCtx<'_> {
    fn dip3_active(&self) -> bool {
        let cons = self.cons;
        cons.deployments.active_after(cons.prev, Deployment::Buried(BuriedDeployment::Dip0003))
    }
}

/// The parent's list as step 4 reads it: resolved at the first check that reads it, then kept for
/// the block, and step 7 builds on it. Core's ProTx checks each call `GetListForBlock(pindexPrev)`;
/// a block without one reads no list before step 7.
pub struct PrevList<'a> {
    resolve: &'a dyn Fn() -> Result<Arc<MnList>, StoreError>,
    list: OnceCell<Arc<MnList>>,
}

impl<'a> PrevList<'a> {
    pub fn new(resolve: &'a dyn Fn() -> Result<Arc<MnList>, StoreError>) -> PrevList<'a> {
        PrevList {
            resolve,
            list: OnceCell::new(),
        }
    }

    /// The list, resolved on the first call.
    ///
    /// # Errors
    ///
    /// The resolver's, each call until one succeeds.
    pub fn get(&self) -> Result<&Arc<MnList>, StoreError> {
        if let Some(list) = self.list.get() {
            return Ok(list);
        }
        let list = (self.resolve)()?;
        Ok(self.list.get_or_init(|| list))
    }
}

/// EV1's checks as H4 and H8 call them, in Core's order (`evo/specialtxman.cpp:881-1085`).
pub trait Ev1Checks {
    /// Step 0's decoded payloads, one slot per transaction: EV1-2's `BlockPayloads`.
    type Payloads<'b>;

    /// Step 0: the block's payloads, decoded once (EV1 §2.4, `BlockPayloads::new(&Block)`).
    fn payloads<'b>(&self, block: &'b Block) -> Self::Payloads<'b>;

    /// Step 1: the CbTx parse and `CheckCbTx` (`stm:899-917`, EV1-5's `check_cbtx`), called with
    /// `fCheckCbTxMerkleRoots` for a `vtx[0]` of raw type `TRANSACTION_COINBASE`: the parsed
    /// CbTx, or Core's reject.
    ///
    /// # Errors
    ///
    /// `Fatal` for a state the readiness guard rules out.
    fn check_cbtx(
        &self,
        block: &Block,
        payloads: &Self::Payloads<'_>,
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal>;

    /// Step 4's body for `block.txs()[i]` (`stm:937-960`, EV1-3's dispatcher): the shared
    /// collateral template check under `rules.v24`, the skip of a raw-type-5 `vtx[0]`, then
    /// `CheckSpecialTxInner`.
    ///
    /// # Errors
    ///
    /// `Fatal` for a state the readiness guard rules out.
    fn check_tx(
        &self,
        block: &Block,
        payloads: &Self::Payloads<'_>,
        i: usize,
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal>;

    /// H8: `CheckSharedCollateralSpends(tx, view)` with V24 active (`validation.cpp:2708-2716`,
    /// EV1-3's covenant).
    ///
    /// # Errors
    ///
    /// `Fatal` for a state the readiness guard rules out.
    fn shared_collateral_spends(
        &self,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal>;
}

/// The stand-in for the checks that have not merged (D44), and `DmlChecks`' step 1 until EV1-5a.
/// Below DIP0003, the only place NodeEvo reaches it while G1 holds, it is `NoEvo`'s checks exactly
/// ([`pre_dip3_cbtx`], [`pre_dip3_special_tx`]); past it, a readiness refusal.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ev1NotBuilt;

impl Ev1Checks for Ev1NotBuilt {
    type Payloads<'b> = ();

    fn payloads(&self, _: &Block) {}

    /// Below DIP0003 no raw-type-5 coinbase parses (Core reads no payload below version 3), so
    /// every call is a reject.
    fn check_cbtx(
        &self,
        block: &Block,
        (): &(),
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal> {
        if cx.dip3_active() {
            return Ok(Err(SpecialTxFailure::NotReady(SPECIAL_TX_CHECKS)));
        }
        match pre_dip3_cbtx(block)? {
            Err(err) => Ok(Err(SpecialTxFailure::Invalid(err))),
            Ok(()) => Err(Fatal::new("step 1 without a raw-type-5 vtx[0]")),
        }
    }

    fn check_tx(
        &self,
        block: &Block,
        (): &(),
        i: usize,
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        if cx.dip3_active() || cx.rules.v24 {
            return Ok(Err(SpecialTxFailure::NotReady(SPECIAL_TX_CHECKS)));
        }
        let tx = block.txs().get(i).ok_or_else(|| Fatal::new("step 4 past the block's txs"))?;
        // we validated CCbTx above, starts from the 2nd transaction
        if i == 0 && tx.tx_type().to_base() == TRANSACTION_COINBASE {
            return Ok(Ok(()));
        }
        Ok(pre_dip3_special_tx(tx).map_err(SpecialTxFailure::Invalid))
    }

    /// Called only with V24 active, which G1 refuses under `NodeEvo`.
    fn shared_collateral_spends(
        &self,
        _: &Tx,
        _: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        Err(Fatal::new("CheckSharedCollateralSpends reached past G1 before EV1"))
    }
}

/// EV1's checks as dash-dml has them (CH7a-2b-1): EV1-2's payloads, EV1-3's dispatcher and
/// covenant. Step 1 is [`Ev1NotBuilt`]'s until EV1-5's `check_cbtx` (2b-2), so a block that reaches
/// it past DIP0003 is still refused.
#[derive(Clone, Copy, Debug, Default)]
pub struct DmlChecks;

impl Ev1Checks for DmlChecks {
    type Payloads<'b> = BlockPayloads<'b>;

    fn payloads<'b>(&self, block: &'b Block) -> BlockPayloads<'b> {
        BlockPayloads::new(block)
    }

    fn check_cbtx(
        &self,
        block: &Block,
        _: &BlockPayloads<'_>,
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal> {
        Ev1NotBuilt.check_cbtx(block, &(), cx)
    }

    fn check_tx(
        &self,
        block: &Block,
        payloads: &BlockPayloads<'_>,
        i: usize,
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        let tx = block.txs().get(i).ok_or_else(|| Fatal::new("step 4 past the block's txs"))?;
        // The shared-collateral creation rule applies to every transaction, including the
        // coinbase and non-special transactions, which CheckSpecialTxInner never sees
        if cx.rules.v24
            && let Err(tx_state) = shared_collateral_template_outputs(tx, payloads.get(i))
        {
            let err = tx_reject("Shared collateral check failed", tx, &tx_state);
            return Ok(Err(SpecialTxFailure::Invalid(err)));
        }
        // we validated CCbTx above, starts from the 2nd transaction
        if i == 0 && tx.tx_type().to_base() == TRANSACTION_COINBASE {
            return Ok(Ok(()));
        }
        // The dispatcher's verdict below DIP0003, without its context.
        if !cx.dip3_active() {
            return Ok(pre_dip3_special_tx(tx).map_err(SpecialTxFailure::Invalid));
        }
        if !tx.has_extra_payload_field() {
            return Ok(Ok(()));
        }
        let list = match cx.list.get() {
            Ok(list) => list,
            Err(e) => return store_failure("GetListForBlock", &e).map(Err),
        };
        match check_special_tx_inner(tx, payloads.get(i), &dml_ctx(cx, list)) {
            Ok(Ok(())) => Ok(Ok(())),
            Ok(Err(tx_state)) => {
                // At this moment CheckSpecialTx() may fail by 2 possible ways:
                // consensus failures and "TX_BAD_SPECIAL"
                if !matches!(
                    tx_state.result(),
                    TxValidationResult::Consensus | TxValidationResult::BadSpecial
                ) {
                    return Err(Fatal::assert(format!(
                        "CheckSpecialTxInner: a {:?} reject",
                        tx_state.result()
                    )));
                }
                let err = tx_reject("Special Transaction check failed", tx, &tx_state);
                Ok(Err(SpecialTxFailure::Invalid(err)))
            }
            Err(SpecialTxFault::Unported(t)) => Ok(Err(SpecialTxFailure::NotReady(unported(t)))),
            Err(SpecialTxFault::Coins(e)) => Err(read_failed(e)),
        }
    }

    fn shared_collateral_spends(
        &self,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        match specialtx::shared_collateral_spends(tx, view) {
            Ok(verdict) => Ok(verdict),
            Err(SpecialTxFault::Coins(e)) => Err(read_failed(e)),
            Err(SpecialTxFault::Unported(t)) => {
                Err(Fatal::new(format!("CheckSharedCollateralSpends: unported type {t:?}")))
            }
        }
    }
}

/// dash-dml's dispatcher context for the attempt's `cx`, over the parent's `list`.
fn dml_ctx<'a>(
    cx: SpecialTxCtx<'a>,
    list: &'a MnList,
) -> specialtx::SpecialTxCtx<'a, CoinsReadError> {
    specialtx::SpecialTxCtx {
        evo: EvoCtx {
            cons: cx.cons,
            check_sigs: cx.check_sigs,
        },
        rules: specialtx::SpecialTxRules {
            v24: cx.rules.v24,
            evo_shares: cx.rules.evo_shares,
        },
        list,
        coins: cx.coins,
        net_info: cx.net_info,
    }
}

/// The check an [`SpecialTxFault::Unported`] type waits for (DEC-663): EV1-4 lands types 2 and 4,
/// EV1-5 types 5 to 9.
fn unported(t: TxType) -> &'static str {
    match t {
        TxType::ProviderUpdateService => "the ProUpServTx check (EV1-4)",
        TxType::ProviderUpdateRevoke => "the ProUpRevTx check (EV1-4)",
        TxType::CoinbaseCommitment => "the CbTx check (EV1-5)",
        TxType::QuorumCommitment => "the quorum commitment check (EV1-5)",
        TxType::MnhfSignal => "the MNHF signal check (EV1-5)",
        TxType::AssetLock => "the asset lock check (EV1-5)",
        TxType::AssetUnlock => "the asset unlock check (EV1-5)",
        _ => SPECIAL_TX_CHECKS,
    }
}

#[cfg(test)]
#[path = "ev1_tests.rs"]
mod tests;
