//! [`AcceptChain`] on V's [`MempoolTurn`]: what `MemPoolAccept` reads of `cs_main` state, for the
//! active tip of one class-M turn.

use std::sync::Arc;

use dash_consensus::ctx::Deployment;
use dash_consensus::script::ScriptFlags;
use dash_dml::MnList;
use dash_dml::creditpool::PoolState as CreditPoolState;
use dash_params::ChainSetup;
use dashd_chainstate::chain::seams::AtmpCoinsCall;
use dashd_chainstate::chain::{Fatal, MempoolTurn, Queued, TipFacts};
use dashd_chainstate::connect::SpecialTxRules;
use dashd_chainstate::sigcache::ScriptCaches;

use super::AcceptChain;
use super::finalize::is_current_for_fee_estimation;

impl AcceptChain for MempoolTurn<'_> {
    fn params(&self) -> &ChainSetup {
        MempoolTurn::params(self)
    }

    fn deployment_active_after(&self, deployment: Deployment) -> Result<bool, Fatal> {
        MempoolTurn::deployment_active_after(self, deployment)
    }

    fn special_tx_rules(&self) -> Result<SpecialTxRules, Fatal> {
        MempoolTurn::special_tx_rules(self)
    }

    fn block_script_flags(&self) -> Option<ScriptFlags> {
        MempoolTurn::block_script_flags(self)
    }

    fn mn_list(&self) -> &Arc<MnList> {
        MempoolTurn::mn_list(self)
    }

    fn credit_pool(&self) -> Option<&Arc<CreditPoolState>> {
        MempoolTurn::credit_pool(self)
    }

    fn script_caches(&self) -> Option<Arc<ScriptCaches>> {
        MempoolTurn::script_caches(self)
    }

    /// The tip's `GetBlockTime()` against `GetTime()`, and the tip's height against
    /// `m_best_header`'s; false without a tip, which Core never meets after genesis.
    // Core: validation.cpp:412-423 at v24.0.0
    fn is_current_for_fee_estimation(&self) -> bool {
        let Some(tip) = self.tip() else {
            return false;
        };
        let tip = self.ancestry().header(tip);
        is_current_for_fee_estimation(
            self.is_ibd(),
            i64::from(tip.time),
            self.clock().node_now().secs(),
            tip.height,
            self.best_header().height,
        )
    }

    fn with_atmp_coins(&mut self, f: &mut AtmpCoinsCall<'_>) -> Result<Option<()>, Fatal> {
        MempoolTurn::with_atmp_coins(self, f)
    }

    fn flush_periodic_in_hold(
        &mut self,
        mempool_usage: u64,
        emit: &mut dyn FnMut(Queued),
    ) -> Result<(), Fatal> {
        MempoolTurn::flush_periodic_in_hold(self, mempool_usage, emit)
    }
}
