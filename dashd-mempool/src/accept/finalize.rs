//! `MemPoolAccept::Finalize` (`validation.cpp:1237-1287` at v24.0.0), `LimitMempoolSize`, and
//! the seams for later slices: the fee estimator (MP2) and the address and spent indexes (IX2),
//! each a no-op until its slice.

use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_primitives::OutPoint;
use dashd_chainstate::chain::Fatal;
use dashd_chainstate::chain::seams::AtmpCoins;

use super::prechecks::Workspace;
use super::view::InputView;
use super::{AcceptArgs, AcceptChain, with_coins};
use crate::pool::{PoolEntry, PoolState};

/// Core's `MAX_FEE_ESTIMATION_TIP_AGE` (`validation.cpp:108`): three hours.
pub const MAX_FEE_ESTIMATION_TIP_AGE_SECS: i64 = 3 * 60 * 60;

/// `IsCurrentForFeeEstimation`: out of IBD, a tip at most three hours older than `now`, and at
/// most one block behind the best header.
// Core: validation.cpp:412-423 at v24.0.0
pub fn is_current_for_fee_estimation(
    ibd: bool,
    tip_time: i64,
    now: i64,
    tip_height: u32,
    best_header_height: u32,
) -> bool {
    !ibd && tip_time >= now - MAX_FEE_ESTIMATION_TIP_AGE_SECS
        && i64::from(tip_height) >= i64::from(best_header_height) - 1
}

#[cfg(test)]
thread_local! {
    /// Each [`process_for_fee_estimation`] call on this thread: the txid and `validFeeEstimate`.
    pub(crate) static FEE_ESTIMATION_CALLS: std::cell::RefCell<Vec<(dash_primitives::TxHash, bool)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The fee estimator's `processTransaction(entry, validFeeEstimate)` from `addUnchecked`
/// (`txmempool.cpp:478-480`): MP2's.
fn process_for_fee_estimation(_entry: &PoolEntry, _valid_for_fee_estimation: bool) {
    #[cfg(test)]
    FEE_ESTIMATION_CALLS
        .with_borrow_mut(|calls| calls.push((_entry.tx().txid(), _valid_for_fee_estimation)));
}

/// `addAddressIndex` and `addSpentIndex` (`validation.cpp:1272-1277`): IX2's.
fn add_indexes(_entry: &PoolEntry, _view: &InputView) {}

/// `LimitMempoolSize(pool, coins_cache)`: [`PoolState::limit_size`], then each of its
/// `vNoSpendsRemaining` uncached through `uncache`, the caller's route to `CoinsTip()`.
///
/// # Errors
///
/// `uncache`'s coins fault.
// Core: validation.cpp:396-410 at v24.0.2
pub(crate) fn limit_mempool_size(
    state: &mut PoolState,
    uncache: impl FnOnce(&[OutPoint]) -> Result<(), Fatal>,
) -> Result<(), Fatal> {
    let no_spends_remaining = state.limit_size();
    if no_spends_remaining.is_empty() {
        return Ok(());
    }
    uncache(&no_spends_remaining)
}

/// `coins_cache.Uncache(removed)` for each of `outpoints`, in order.
pub(crate) fn uncache_all(coins: &mut dyn AtmpCoins, outpoints: &[OutPoint]) {
    for outpoint in outpoints {
        coins.uncache(outpoint);
    }
}

/// `Finalize`: the fresher asset unlock evicts the other claimants of its index, the entry goes
/// in with `ancestors`, and `LimitMempoolSize` runs over `CoinsTip()` (`mempool full` if it
/// evicted the entry) unless limits are bypassed or this is a package submission, which also
/// leaves the entry out of fee estimation.
///
/// # Errors
///
/// The outer error is the uncache's coins fault; the inner one the verdict.
// Core: validation.cpp:1237-1287 at v24.0.2
pub fn finalize(
    state: &mut PoolState,
    chain: &mut dyn AcceptChain,
    ws: Workspace,
    args: &AcceptArgs,
) -> Result<Result<(), TxValidationError>, Fatal> {
    let Workspace {
        entry,
        ancestors,
        view,
        modified_fees,
        ..
    } = ws;
    let tx = entry.tx().clone();
    let hash = tx.txid();
    let valid_for_fee_estimation = modified_fees != 0
        && !args.bypass_limits
        && !args.package_submission
        && chain.is_current_for_fee_estimation()
        && state.has_no_inputs_of(&tx);

    state.remove_asset_unlock_conflicts(&tx);
    state.add_unchecked_with_ancestors(entry, &ancestors, chain.mn_list());
    if let Some(entry) = state.get_iter(&hash) {
        process_for_fee_estimation(entry, valid_for_fee_estimation);
        add_indexes(entry, &view);
    }

    if !args.package_submission && !args.bypass_limits {
        limit_mempool_size(state, |outpoints| {
            with_coins(chain, |coins| {
                uncache_all(coins, outpoints);
                Ok(())
            })
        })?;
        if !state.exists(&hash) {
            return Ok(Err(TxValidationError::new(
                TxValidationResult::MempoolPolicy,
                "mempool full",
            )));
        }
    }
    Ok(Ok(()))
}
