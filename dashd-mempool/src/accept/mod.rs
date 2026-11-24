//! AcceptToMemoryPool: the consensus binding (MP1a-5) and `MemPoolAccept`'s stage B (MP1a-6).
//! Each function runs under V's turn or hold, with the pool lock held by the caller, as Core runs
//! them under `cs_main` and `m_pool.cs` (`validation.cpp:735-775` at v24.0.0).
//!
//! Stage B reaches V and the pool's owner through three narrow seams: [`AcceptChain`] (V's facts
//! in the class-M turn, on `MempoolTurn`), [`AcceptSignals`] (the validation queue, under the pool
//! write guard) and [`AcceptPool`] (that guard, on the `Mempool`).

pub mod chain;
pub mod consensus;
pub mod finalize;
pub mod prechecks;
pub mod refresh;
pub mod reorg_filter;
pub mod single;
pub mod special;
mod turn;
pub mod view;

#[cfg(test)]
pub(crate) mod tests;

#[cfg(all(test, feature = "test-support"))]
pub(crate) mod stage_b_tests;

use std::sync::Arc;

use dash_consensus::TxRef;
use dash_consensus::ctx::Deployment;
use dash_consensus::script::ScriptFlags;
use dash_dml::MnList;
use dash_dml::creditpool::PoolState as CreditPoolState;
use dash_params::ChainSetup;
use dashd_chainstate::chain::seams::{AtmpCoins, AtmpCoinsCall};
use dashd_chainstate::chain::{Fatal, Queued};
use dashd_chainstate::connect::SpecialTxRules;
use dashd_chainstate::sigcache::ScriptCaches;

use crate::pool::{PendingEvent, PoolState};

pub use prechecks::StatelessRecord;
pub use single::accept_to_memory_pool;
pub use special::SpecialTxChain;

/// Core's `ATMPArgs::SingleAccept` (`validation.cpp:610-621` at v24.0.0). Single transactions
/// are never a package submission and never use package feerates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptArgs {
    pub accept_time: i64,
    /// Reorg re-add (MP1b) and mempool load: no fee floors, no size limiting.
    pub bypass_limits: bool,
    pub test_accept: bool,
}

/// What stage B reads of V in its class-M turn. Every method answers for the active tip of this
/// turn; MP1a-8 implements it for `MempoolTurn` (with [`SpecialTxChain`]).
pub trait AcceptChain: SpecialTxChain {
    /// `chainparams` (`Params()`).
    fn params(&self) -> &ChainSetup;
    /// `DeploymentActiveAfter(m_chain.Tip(), deployment)`.
    ///
    /// # Errors
    ///
    /// A deployment-state fault (EHF).
    fn deployment_active_after(&self, deployment: Deployment) -> Result<bool, Fatal>;
    /// `GetSpecialTxRules(m_chain.Tip(), chainman)` (`validation.cpp:6817-6824`).
    ///
    /// # Errors
    ///
    /// A deployment-state fault (EHF).
    fn special_tx_rules(&self) -> Result<SpecialTxRules, Fatal>;
    /// `GetBlockScriptFlags(m_chain.Tip())`; `None` without a tip.
    fn block_script_flags(&self) -> Option<ScriptFlags>;
    /// `GetListAtChainTip()`, V's DMN tip.
    fn mn_list(&self) -> &Arc<MnList>;
    /// `GetCreditPool(m_chain.Tip())`, V's `ChainView.credit_pool`; `None` without V20.
    fn credit_pool(&self) -> Option<&Arc<CreditPoolState>>;
    /// The process script caches (signature and script execution).
    fn script_caches(&self) -> Option<Arc<ScriptCaches>>;
    /// `IsCurrentForFeeEstimation` (`validation.cpp:412-423`), through
    /// [`finalize::is_current_for_fee_estimation`].
    fn is_current_for_fee_estimation(&self) -> bool;
    /// Caching access to `CoinsTip()`; `Ok(None)` when the connector has no coins.
    ///
    /// # Errors
    ///
    /// The coins cell's or `f`'s fault.
    fn with_atmp_coins(&mut self, f: &mut AtmpCoinsCall<'_>) -> Result<Option<()>, Fatal>;
    /// `AcceptToMemoryPool`'s closing `FlushStateToDisk(PERIODIC)` for an admission inside V's
    /// hold (the reorg re-add), with `mempool_usage` read from the held pool and the flush's
    /// signals handed to `emit` in order.
    ///
    /// # Errors
    ///
    /// The flush's fault.
    // Core: validation.cpp:1757-1759 at v24.0.2
    fn flush_periodic_in_hold(
        &mut self,
        mempool_usage: u64,
        emit: &mut dyn FnMut(Queued),
    ) -> Result<(), Fatal>;
}

/// The validation-queue events stage B raises. Core queues each into `GetMainSignals()` under
/// `m_pool.cs` when it occurs; MP1a-8 enqueues them under the pool write guard.
pub trait AcceptSignals {
    /// `TransactionRemovedFromMempool`, raised by `removeUnchecked` (`txmempool.cpp:710-717`).
    fn removed(&mut self, event: PendingEvent);
    /// `TransactionAddedToMempool(ptx, accept_time, sequence)` (`validation.cpp:1423, :1455`).
    fn added(&mut self, tx: TxRef, accept_time: i64, sequence: u64);
    /// `NotifyInstantSendDoubleSpendAttempt(ptx, txConflict)` (`validation.cpp:902-907`).
    fn double_spend_attempt(&mut self, tx: TxRef, conflict: TxRef);
    /// A signal the hold's flush released (`ChainStateFlushed`), in its place in the stream.
    fn flushed(&mut self, event: Queued);
}

/// The pool's write guard for one admission (`LOCK(m_pool.cs)`, `validation.cpp:1432`). MP1a-8
/// implements it for its `Mempool` wrapper.
pub trait AcceptPool {
    /// Runs `f` once under the pool write guard, with the signals it enqueues before releasing
    /// the guard.
    fn with_write(&self, f: &mut dyn FnMut(&mut PoolState, &mut dyn AcceptSignals));
}

/// Forwards the pool's pending removals, in the order they occurred, to `signals`.
pub(crate) fn announce_removals(state: &mut PoolState, signals: &mut dyn AcceptSignals) {
    for event in state.take_pending_events() {
        signals.removed(event);
    }
}

/// `f` over `CoinsTip()` through [`AcceptChain::with_atmp_coins`].
///
/// # Errors
///
/// `f`'s or the coins cell's fault; a connector without coins (a wiring error).
pub(crate) fn with_coins<R>(
    chain: &mut dyn AcceptChain,
    f: impl FnOnce(&mut dyn AtmpCoins) -> Result<R, Fatal>,
) -> Result<R, Fatal> {
    let mut f = Some(f);
    let mut out = None;
    let ran = chain.with_atmp_coins(&mut |coins| {
        let f = f.take().ok_or_else(|| Fatal::assert("with_atmp_coins called twice"))?;
        out = Some(f(coins)?);
        Ok(())
    })?;
    ran.ok_or_else(|| Fatal::assert("AcceptToMemoryPool without CoinsTip"))?;
    out.ok_or_else(|| Fatal::assert("with_atmp_coins did not run its call"))
}
