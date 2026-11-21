//! `MemPoolAccept::Finalize` (`validation.cpp:1237-1287` at v24.0.0) and its seams for later
//! slices: the fee estimator (MP2), the address and spent indexes (IX2) and `LimitMempoolSize`
//! (MP1b), each a no-op in MP1a.

use dash_consensus::validation::{TxValidationError, TxValidationResult};

use super::prechecks::Workspace;
use super::view::InputView;
use super::{AcceptArgs, AcceptChain};
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

/// The fee estimator's `processTransaction(entry, validFeeEstimate)` from `addUnchecked`
/// (`txmempool.cpp:478-480`): MP2's.
fn process_for_fee_estimation(_entry: &PoolEntry, _valid_for_fee_estimation: bool) {}

/// `addAddressIndex` and `addSpentIndex` (`validation.cpp:1272-1277`): IX2's.
fn add_indexes(_entry: &PoolEntry, _view: &InputView) {}

/// `LimitMempoolSize` (`validation.cpp:396-410`): expiry and trimming are MP1b's, so nothing is
/// evicted and no coin is uncached here.
fn limit_mempool_size(_state: &mut PoolState) {}

/// `Finalize`: the fresher asset unlock evicts the other claimants of its index, the entry goes
/// in with the ancestors `PreChecks` computed, and `LimitMempoolSize` runs unless limits are
/// bypassed (`mempool full` if it evicted the entry).
// Core: validation.cpp:1237-1287 at v24.0.0
pub fn finalize(
    state: &mut PoolState,
    chain: &dyn AcceptChain,
    ws: Workspace,
    args: &AcceptArgs,
) -> Result<(), TxValidationError> {
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
        && chain.is_current_for_fee_estimation()
        && state.has_no_inputs_of(&tx);

    state.remove_asset_unlock_conflicts(&tx);
    state.add_unchecked_with_ancestors(entry, &ancestors, chain.mn_list());
    if let Some(entry) = state.get_iter(&hash) {
        process_for_fee_estimation(entry, valid_for_fee_estimation);
        add_indexes(entry, &view);
    }

    if !args.bypass_limits {
        limit_mempool_size(state);
        if !state.exists(&hash) {
            return Err(TxValidationError::new(TxValidationResult::MempoolPolicy, "mempool full"));
        }
    }
    Ok(())
}
