//! `MemPoolAccept::TryAssetUnlockRefresh` (`validation.cpp:1376-1426` at v24.0.0): a re-signed
//! instance of a held stable-txid asset unlock replaces the held instance in place.

use dash_consensus::TxRef;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dashd_chainstate::chain::{AdmissionFault, Fatal};

use super::single::{fee_rate, success};
use super::special::{asset_unlock_payload, check_special_tx_at_tip};
use super::{AcceptArgs, AcceptChain, AcceptSignals};
use crate::pool::PoolState;
use crate::types::MempoolAcceptResult;
use dash_dml::specialtx::asset::HEIGHT_DIFF_EXPIRING;

fn failure(reason: &'static str) -> Option<MempoolAcceptResult> {
    Some(super::single::failure(TxValidationError::new(TxValidationResult::Conflict, reason)))
}

/// `TryAssetUnlockRefresh`: `None` unless `tx` is a stable-txid asset unlock whose txid the pool
/// holds. The same instance is `txn-already-in-mempool`; one that is not fresher, or whose
/// window does not hold the tip, is `assetunlock-stale-instance`; otherwise `CheckSpecialTx` at
/// the tip decides, and a pass replaces the held instance (not under `test_accept`) and reports
/// the held entry's size and fees.
///
/// # Errors
///
/// A fault in `CheckSpecialTx` or the tip's rules.
// Core: validation.cpp:1376-1426 at v24.0.0
pub fn try_asset_unlock_refresh(
    state: &mut PoolState,
    chain: &mut dyn AcceptChain,
    signals: &mut dyn AcceptSignals,
    tx: &TxRef,
    args: &AcceptArgs,
) -> Result<Option<MempoolAcceptResult>, AdmissionFault> {
    if !tx.is_asset_unlock_with_stable_txid() {
        return Ok(None);
    }
    let Some(held) = state.get_iter(&tx.txid()) else {
        return Ok(None);
    };
    if held.tx().instance_hash() == tx.instance_hash() {
        return Ok(failure("txn-already-in-mempool"));
    }

    // Reject cheaply, before any signature work, unless this is a plausibly minable fresher
    // instance: its requestedHeight must strictly exceed the held instance's and place the tip
    // inside the withdrawal's validity window.
    let tip_height = chain.tip_height().ok_or_else(|| Fatal::assert("refresh without a tip"))?;
    let tip_height = tip_height as i32;
    let fresh = match (asset_unlock_payload(held.tx()), asset_unlock_payload(tx)) {
        (Some(held), Some(new)) => {
            let height_to_expiry = new.requested_height().wrapping_add(HEIGHT_DIFF_EXPIRING) as i32;
            new.requested_height() > held.requested_height()
                && i64::from(new.requested_height()) <= i64::from(tip_height)
                && height_to_expiry > tip_height
        }
        _ => false,
    };
    if !fresh {
        return Ok(failure("assetunlock-stale-instance"));
    }
    let vsize = held.vsize() as i64;
    let fee = held.metadata().fee;
    let effective_feerate = fee_rate(held.modified_fee(), vsize);

    // Full consensus validation of the fresh instance: payload, quorum recency, height window
    // and quorum signature.
    let rules = chain.special_tx_rules()?;
    if let Err(reject) = check_special_tx_at_tip(&mut *chain, tx, rules)? {
        return Ok(Some(super::single::failure(reject)));
    }

    if !args.test_accept {
        state.replace_asset_unlock_instance(tx.txid(), tx.clone());
        let sequence = state.get_and_increment_sequence();
        signals.added(tx.clone(), args.accept_time, sequence);
    }
    Ok(Some(success(vsize, fee, effective_feerate, tx.txid())))
}
