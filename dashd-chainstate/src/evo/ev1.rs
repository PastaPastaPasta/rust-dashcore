//! The EV1 seam (D44, DEC-581): H4's steps 0, 1 and 4 and hook H8, the checks EV1-2's payloads,
//! EV1-3's dispatcher and covenant and EV1-5's `check_cbtx` own. CH7a-2b binds each as it merges;
//! until then [`Ev1NotBuilt`] gives Core's verdicts below DIP0003 and refuses past it.

use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::{Block, BlockId, Tx, TxValidationError};
use dash_dml::creditpool::CreditPool;
use dash_params::BuriedDeployment;
use dash_primitives::CoinbaseCommitment;
use dash_types::Numeric as _;

use crate::chain::Fatal;
use crate::connect::{SpecialTxFailure, SpecialTxRules, pre_dip3_cbtx, pre_dip3_special_tx};
use crate::store::CoinsReadError;

/// The capability [`Ev1NotBuilt`] refuses for.
pub const SPECIAL_TX_CHECKS: &str = "special-tx checks (EV1)";

/// What EV1's checks read of the attempt besides the block (EV1 §2's `SpecialTxCtx`): the
/// deployments and `pindexPrev` of the attempt's connect context (D48), and the arguments of
/// `CheckSpecialTxInner` (`stm:956`).
#[derive(Clone, Copy)]
pub struct SpecialTxCtx<'a> {
    pub deployments: &'a dyn DeploymentView,
    pub prev: Option<BlockId>,
    pub rules: SpecialTxRules,
    /// The pre-block view (`v:2713`).
    pub coins: &'a dyn CoinsView<Error = CoinsReadError>,
    /// Step 3's credit-pool indexes, `None` below V20 (Core's empty `CRangesSet`).
    pub indexes: Option<&'a CreditPool>,
    /// `check_sigs`: `fCheckCbTxMerkleRoots`.
    pub check_sigs: bool,
}

impl SpecialTxCtx<'_> {
    fn dip3_active(&self) -> bool {
        self.deployments.active_after(self.prev, Deployment::Buried(BuriedDeployment::Dip0003))
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

/// The production stand-in until CH7a-2b (D44). Below DIP0003, the only place NodeEvo reaches it
/// while G1 holds, it is `NoEvo`'s checks exactly ([`pre_dip3_cbtx`], [`pre_dip3_special_tx`]);
/// past it, a readiness refusal.
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

#[cfg(test)]
#[path = "ev1_tests.rs"]
mod tests;
