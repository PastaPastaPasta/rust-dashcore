//! `MaybeUpdateMempoolForReorg`'s `filter_final_and_mature`, which `removeForReorg` applies to
//! every entry after a disconnect, over the held pool's [`ReorgCx`].

use dash_consensus::tx::COINBASE_MATURITY;
use dash_params::ChainSetup;
use dashd_chainstate::chain::{Fatal, ReorgCx};

use super::consensus::{
    calculate_lock_points_at_tip, calculate_prev_heights, check_final_tx_at_tip,
    check_sequence_locks_at_tip,
};
use super::view::mempool_view_coin;
use crate::api::MempoolRead;
use crate::pool::entry::PoolEntry;
use crate::types::LockPoints;

/// `TestLockPointValidity`: lock points with a `maxInputBlock` hold only while the active chain
/// contains it.
// Core: txmempool.cpp:40-55 at v24.0.0
pub fn test_lock_point_validity(cx: &dyn ReorgCx, lock_points: &LockPoints) -> bool {
    lock_points.max_input_block.is_none_or(|block| cx.chain_contains(block))
}

/// `filter_final_and_mature` for `entry` of `pool` at `cx`'s new tip: `(remove, lock points)`.
/// `remove` when the transaction is not final for the next block, its sequence locks fail (over
/// still-valid lock points, or over lock points recalculated through the mempool-overlaid
/// CoinsTip), or it spends an out-of-pool coinbase output immature at the tip's height plus one.
/// The lock points are `Some` exactly when Core recalculated them and updated the entry
/// (`mapTx.modify`); the caller applies them, then removes, as Core's order leaves them.
///
/// # Errors
///
/// A coins read fault (the reorg aborts, nothing applied), or Core's assert on a missing coin of
/// a coinbase-spending entry; no active tip.
// Core: validation.cpp:476-518 at v24.0.0
pub fn filter_final_and_mature(
    params: &ChainSetup,
    pool: &dyn MempoolRead,
    entry: &PoolEntry,
    cx: &mut dyn ReorgCx,
) -> Result<(bool, Option<LockPoints>), Fatal> {
    let tx = entry.tx();
    // The transaction must be final.
    if !check_final_tx_at_tip(&*cx, tx)? {
        return Ok((true, None));
    }

    let lock_points = entry.metadata().lock_points;
    let mut updated = None;
    // CheckSequenceLocksAtTip checks if the transaction will be final in the next block to be
    // created on top of the new chain.
    if test_lock_point_validity(&*cx, &lock_points) {
        if !check_sequence_locks_at_tip(params, &*cx, &lock_points)? {
            return Ok((true, None));
        }
    } else {
        let heights = calculate_prev_heights(tx, &mut |outpoint| {
            let coin = mempool_view_coin(pool, outpoint, &mut |o| cx.access_coin(o))?;
            Ok(coin.map(|coin| coin.height()))
        })?;
        let Some(heights) = heights else {
            return Ok((true, None));
        };
        let new_lock_points = calculate_lock_points_at_tip(params, &*cx, tx, heights)?;
        if !check_sequence_locks_at_tip(params, &*cx, &new_lock_points)? {
            return Ok((true, None));
        }
        updated = Some(new_lock_points);
    }

    // If the transaction spends any coinbase outputs, it must be mature.
    if entry.metadata().spends_coinbase {
        let tip = cx.tip().ok_or_else(|| Fatal::assert("filter_final_and_mature without a tip"))?;
        let spend_height = i64::from(cx.ancestry().header(tip).height) + 1;
        for input in tx.inputs() {
            let prevout = input.prevout();
            if pool.exists(&prevout.txid()) {
                continue;
            }
            let coin = cx.access_coin(&prevout)?.ok_or_else(|| {
                Fatal::assert("filter_final_and_mature: CoinsTip().AccessCoin is spent")
            })?;
            if coin.is_coinbase() && spend_height - i64::from(coin.height()) < COINBASE_MATURITY {
                return Ok((true, updated));
            }
        }
    }
    // Transaction is still valid and cached LockPoints are updated.
    Ok((false, updated))
}
