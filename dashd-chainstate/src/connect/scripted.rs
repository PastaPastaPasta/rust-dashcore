//! [`ScriptedEvo`]: test-support evo hooks whose H4 emits scripted effects and whose finish applies
//! the log through given [`EffectAppliers`] (PLAN-LK1b §6 S1f). Every other answer is [`NoEvo`]'s,
//! so a block below every G1 rule connects as under `NoEvo`, with effects a test chooses ahead of
//! the IS loop: counterexample S's `StoreQuorumSnapshot` before a removal (§5.3).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use dash_consensus::coins::CoinsView;
use dash_consensus::payments::MnRewardEra;
use dash_consensus::{Amount, Block, BlockId, BlsCtx, Tx, TxValidationError};
use dash_evo::Effect;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;

use super::evo::{
    AttemptId, BlockAt, ClBest, EvoAttempt as _, EvoDigest, EvoHooks, FinishReport, NoEvo,
    NoEvoAttempt, SpecialTxFailure, SpecialTxRules,
};
use crate::chain::{AttemptOrigin, ConnectCx, EvoTarget, Fatal, VContext, ViewTarget};
use crate::evo::effects::EffectAppliers;
use crate::store::CoinsReadError;

/// What a test scripts and reads of a [`ScriptedEvo`] that has moved into a connector.
#[derive(Default)]
struct Script {
    /// Each H4 call emits the next entry; once they run out, nothing.
    h4: VecDeque<Vec<Effect>>,
    /// Each finish's report, in order.
    reports: Vec<FinishReport>,
}

/// A handle on a [`ScriptedEvo`]'s script, kept by the test after the hooks move.
#[derive(Clone, Default)]
pub struct ScriptedEvoHandle(Arc<Mutex<Script>>);

impl ScriptedEvoHandle {
    fn lock(&self) -> MutexGuard<'_, Script> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The next H4 call emits `effects` into its attempt's log, in order.
    pub fn push_h4(&self, effects: Vec<Effect>) {
        self.lock().h4.push_back(effects);
    }

    /// Every finish's report so far, in order.
    pub fn reports(&self) -> Vec<FinishReport> {
        self.lock().reports.clone()
    }
}

/// [`NoEvo`] with scripted H4 effects and real appliers.
pub struct ScriptedEvo {
    appliers: EffectAppliers,
    script: ScriptedEvoHandle,
}

impl ScriptedEvo {
    /// Hooks applying each attempt's log through `appliers`.
    pub fn new(appliers: EffectAppliers) -> ScriptedEvo {
        ScriptedEvo {
            appliers,
            script: ScriptedEvoHandle::default(),
        }
    }

    /// The handle a test scripts and reads the hooks through.
    pub fn handle(&self) -> ScriptedEvoHandle {
        self.script.clone()
    }
}

impl EvoHooks for ScriptedEvo {
    const COMPLETE: bool = false;

    type Facade = ();

    type Attempt<'v> = NoEvoAttempt<'v>;

    type ListUpdates = ();

    fn begin_attempt<'v>(
        &mut self,
        facade: &'v (),
        v: &'v mut VContext,
        cx: &'v ConnectCx<'_>,
        id: AttemptId,
        origin: AttemptOrigin,
        target: &ViewTarget<'_>,
    ) -> Result<NoEvoAttempt<'v>, Fatal> {
        NoEvo.begin_attempt(facade, v, cx, id, origin, target)
    }

    fn finish_attempt(&mut self, attempt: NoEvoAttempt<'_>) -> Result<(), Fatal> {
        self.finish_attempt_reported(attempt).result
    }

    /// The log through the appliers in emission order, stopping at the first failure. Unlike
    /// `NodeEvo`, it always applies as a connect (`undo = false`) and trips no node latch when
    /// the application fails.
    fn finish_attempt_reported(&mut self, attempt: NoEvoAttempt<'_>) -> FinishReport {
        let (id, emitted) = attempt.into_log();
        let report = match self.appliers.apply_log_as(id, &emitted, false) {
            Ok(_) => {
                let completed = emitted.len();
                FinishReport::known(Ok(()), emitted, completed)
            }
            Err((k, _, failure)) => FinishReport::known(Err(failure), emitted, k),
        };
        self.script.lock().reports.push(report.clone());
        report
    }

    fn bls_ctx(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        params: &ChainSetup,
        at: &BlockAt,
    ) -> BlsCtx {
        NoEvo.bls_ctx(attempt, evo, params, at)
    }

    fn commit_bls_scheme(&mut self, _: &mut NoEvoAttempt<'_>, _: &mut EvoTarget<'_>) {}

    fn verify_best_block(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<bool, Fatal> {
        NoEvo.verify_best_block(attempt, evo, hash)
    }

    /// The next scripted effects, then `NoEvo`'s checks.
    fn process_special_txs(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
        rules: SpecialTxRules,
        coins: &dyn CoinsView<Error = CoinsReadError>,
        fees: Amount,
        just_check: bool,
        check_cbtx_merkle_roots: bool,
        cl_best: &mut ClBest<'_>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        let effects = self.script.lock().h4.pop_front().unwrap_or_default();
        for e in effects {
            attempt.sink().emit(e);
        }
        NoEvo.process_special_txs(
            attempt,
            evo,
            block,
            at,
            rules,
            coins,
            fees,
            just_check,
            check_cbtx_merkle_roots,
            cl_best,
        )
    }

    fn mn_payment_valid(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        tx: &Tx,
        prev: Option<BlockId>,
        reward: Amount,
        fees: Amount,
        era: MnRewardEra,
        just_check: bool,
    ) -> Result<bool, Fatal> {
        NoEvo.mn_payment_valid(attempt, evo, tx, prev, reward, fees, era, just_check)
    }

    fn set_best_block(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<(), Fatal> {
        NoEvo.set_best_block(attempt, evo, hash)
    }

    fn list_changed(&mut self, _: &mut NoEvoAttempt<'_>, _: &(), _: bool) {}

    fn shared_collateral_spends(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        tx: &Tx,
        coins: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        NoEvo.shared_collateral_spends(attempt, tx, coins)
    }

    fn undo_special_txs(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
    ) -> Result<Option<()>, Fatal> {
        NoEvo.undo_special_txs(attempt, evo, block, at)
    }

    fn begin_tx(&mut self, _: &mut NoEvoAttempt<'_>, _: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn commit_tx(&mut self, _: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn rollback_tx(&mut self, _: &mut EvoTarget<'_>) {}

    fn digest_fields(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        at: &BlockAt,
    ) -> Result<EvoDigest, Fatal> {
        NoEvo.digest_fields(attempt, evo, at)
    }
}
