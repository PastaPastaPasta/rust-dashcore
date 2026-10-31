//! What drives [`SerialCore::connect`](super::SerialCore::connect) outside V's bracket needs and
//! cannot reach from outside it (CH3a a3): `benches/connect_block_micro.rs` (D95) and the CRASH
//! model's core mode (D91). Not an API: no runtime caller uses it.

#![doc(hidden)]

use std::cell::RefCell;

use dash_chain::HeaderChain;
use dash_consensus::coins::CoinsView;
use dash_consensus::payments::MnRewardEra;
use dash_consensus::{Amount, Block, BlockId, BlsCtx, Tx, TxValidationError};
use dash_evo::{Effect, EffectSink};
use dash_params::ChainParams;
use dash_primitives::BlockHash;

use super::connector::finished_outcome;
use super::evo::{
    BlockAt, EvoHooks, NoEvo, SpecialTxFailure, SpecialTxRules, no_evo, pre_dip3_special_txs,
};
use super::serial::AttemptSteps;
use crate::chain::{BlockStatusWriter, ConnectOutcome, Fatal};
use crate::store::CoinsReadError;

/// The status writer of `id` over a header chain the caller holds mutably, as V lends its own
/// (`chain/abc.rs`, `with_connector`).
pub fn status_writer<'a>(
    chain: &'a RefCell<&'a mut HeaderChain>,
    id: BlockId,
) -> BlockStatusWriter<'a> {
    BlockStatusWriter::new(chain, id)
}

/// [`NoEvo`]'s answers as the connector's bracket relays them to the core (CH3 §3.1's `NoEvo`
/// column), for callers that cannot open a `NoEvo` attempt: only V, which owns the `VContext`,
/// can. Its answers are `evo::no_evo`'s; [`finish`](Self::finish) is `finish_attempt` with the
/// connector's rule (CH3a D75).
#[derive(Default)]
pub struct NoEvoSteps(Vec<Effect>);

impl NoEvoSteps {
    /// The effects the attempt logged.
    pub fn effects(&self) -> &[Effect] {
        &self.0
    }

    /// The outcome after `NoEvo::finish_attempt`: no effect has an applier.
    pub fn finish(&self, out: Result<ConnectOutcome, Fatal>) -> Result<ConnectOutcome, Fatal> {
        finished_outcome(out, no_evo::finish(None, &self.0))
    }
}

impl AttemptSteps for NoEvoSteps {
    fn complete(&self) -> bool {
        <NoEvo as EvoHooks>::COMPLETE
    }

    fn bls_ctx(&mut self, params: &ChainParams, at: &BlockAt) -> BlsCtx {
        no_evo::bls_ctx(params, at)
    }

    fn verify_best_block(&mut self, _: &BlockHash) -> Result<bool, Fatal> {
        no_evo::verify_best_block()
    }

    fn process_special_txs(
        &mut self,
        block: &Block,
        _: &BlockAt,
        _: SpecialTxRules,
        _: &dyn CoinsView<Error = CoinsReadError>,
        _: Amount,
        _: bool,
        check_cbtx_merkle_roots: bool,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        Ok(pre_dip3_special_txs(block, check_cbtx_merkle_roots))
    }

    fn shared_collateral_spends(
        &mut self,
        _: &Tx,
        _: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        no_evo::shared_collateral_spends(None)
    }

    fn mn_payment_valid(
        &mut self,
        _: &Tx,
        _: Option<BlockId>,
        _: Amount,
        _: Amount,
        _: MnRewardEra,
        _: bool,
    ) -> Result<bool, Fatal> {
        no_evo::mn_payment_valid()
    }

    fn set_best_block(&mut self, _: &BlockHash) -> Result<(), Fatal> {
        no_evo::set_best_block()
    }

    fn commit_bls_scheme(&mut self) {}

    fn list_changed(&mut self, _: bool) {}

    fn undo_special_txs(&mut self, _: &Block, _: &BlockAt) -> Result<bool, Fatal> {
        Ok(true)
    }

    fn undo_refusal(&mut self, _: &BlockAt) -> Result<Option<&'static str>, Fatal> {
        Ok(None)
    }

    fn sink(&mut self) -> &mut dyn EffectSink {
        &mut self.0
    }
}
