//! `MemPoolAccept::AcceptSingleTransaction` and `AcceptToMemoryPool` (`validation.cpp:1428-1466`,
//! `:1732-1761` at v24.0.0): stage B of one ticket, in one class-M turn.

use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_consensus::{Amount, TxRef};
use dash_primitives::{OutPoint, TxHash};
use dashd_chainstate::chain::{Fatal, MempoolCompletion, MempoolOutcome};
use dashd_chainstate::checkqueue::script::TxSlot;

use super::consensus::{consensus_script_checks, policy_script_checks};
use super::finalize::finalize;
use super::prechecks::{StatelessRecord, pre_checks};
use super::refresh::try_asset_unlock_refresh;
use super::{AcceptArgs, AcceptChain, AcceptPool, AcceptSignals, announce_removals, with_coins};
use crate::pool::PoolState;
use crate::types::{MempoolAcceptResult, MempoolAcceptResultType};

/// `CFeeRate(fee, static_cast<uint32_t>(vsize))`: duffs per kB, 0 for a zero size.
// Core: policy/feerate.cpp:12-21 at v24.0.0
pub fn fee_rate(fee: Amount, vsize: i64) -> i64 {
    let size = i64::from(vsize as u32);
    if size > 0 {
        fee.wrapping_mul(1_000) / size
    } else {
        0
    }
}

/// `MempoolAcceptResult::Failure(state)`.
pub fn failure(reject: TxValidationError) -> MempoolAcceptResult {
    MempoolAcceptResult {
        result_type: MempoolAcceptResultType::Invalid,
        validation_result: reject.result(),
        reason: reject.reject_reason().to_owned(),
        debug_message: reject.debug_message().to_owned(),
        vsize: None,
        base_fee: None,
        effective_feerate: None,
        txids_fee_calculations: None,
    }
}

/// `MempoolAcceptResult::Success(vsize, fees, effective_feerate, {txid})`.
pub fn success(
    vsize: i64,
    fees: Amount,
    effective_feerate: i64,
    txid: TxHash,
) -> MempoolAcceptResult {
    MempoolAcceptResult {
        result_type: MempoolAcceptResultType::Valid,
        validation_result: TxValidationResult::Unset,
        reason: String::new(),
        debug_message: String::new(),
        vsize: Some(vsize),
        base_fee: Some(fees),
        effective_feerate: Some(effective_feerate),
        txids_fee_calculations: Some(vec![txid]),
    }
}

/// `AcceptSingleTransaction` under the pool write guard: `TryAssetUnlockRefresh`, `PreChecks`,
/// `PolicyScriptChecks`, `ConsensusScriptChecks`, then (unless `test_accept`) `Finalize` and
/// `TransactionAddedToMempool` with the next sequence. Every removal the pool made is announced
/// to `signals` before this returns, whatever the outcome, and before the admission event.
/// `coins_to_uncache` collects the inputs CoinsTip had not cached.
///
/// # Errors
///
/// A fault without a verdict: a coins or deployment fault, or Core's assert.
// Core: validation.cpp:1428-1466 at v24.0.0
pub fn accept_single_transaction(
    state: &mut PoolState,
    chain: &mut dyn AcceptChain,
    signals: &mut dyn AcceptSignals,
    tx: &TxRef,
    record: &StatelessRecord,
    args: &AcceptArgs,
    coins_to_uncache: &mut Vec<OutPoint>,
) -> Result<MempoolAcceptResult, Fatal> {
    let result = stage_b(state, chain, signals, tx, record, args, coins_to_uncache);
    announce_removals(state, signals);
    result
}

fn stage_b(
    state: &mut PoolState,
    chain: &mut dyn AcceptChain,
    signals: &mut dyn AcceptSignals,
    tx: &TxRef,
    record: &StatelessRecord,
    args: &AcceptArgs,
    coins_to_uncache: &mut Vec<OutPoint>,
) -> Result<MempoolAcceptResult, Fatal> {
    if let Some(refresh) = try_asset_unlock_refresh(state, chain, signals, tx, args)? {
        return Ok(refresh);
    }
    let ws = match pre_checks(state, chain, signals, tx, record, args, coins_to_uncache)? {
        Ok(ws) => ws,
        Err(reject) => return Ok(failure(reject)),
    };

    // Perform the inexpensive checks first and avoid hashing and signature verification unless
    // those checks pass, to mitigate CPU exhaustion denial-of-service attacks.
    let slot = TxSlot::new(tx);
    let caches = chain.script_caches();
    if let Err(reject) = policy_script_checks(&slot, &ws.view, caches.as_deref())? {
        return Ok(failure(reject));
    }
    let block_flags = chain
        .block_script_flags()
        .ok_or_else(|| Fatal::assert("ConsensusScriptChecks without an active tip"))?;
    let consensus = with_coins(&mut *chain, |coins| {
        consensus_script_checks(&slot, &ws.view, &*state, coins, block_flags, caches.as_deref())
    })?;
    if let Err(reject) = consensus {
        return Ok(failure(reject));
    }

    let effective_feerate = fee_rate(ws.modified_fees, ws.vsize);
    let (vsize, base_fees) = (ws.vsize, ws.base_fees);
    // Tx was accepted, but not added
    if args.test_accept {
        return Ok(success(vsize, base_fees, effective_feerate, tx.txid()));
    }
    if let Err(reject) = finalize(state, &mut *chain, ws, args)? {
        return Ok(failure(reject));
    }
    announce_removals(state, signals);
    let sequence = state.get_and_increment_sequence();
    signals.added(tx.clone(), args.accept_time, sequence);
    Ok(success(vsize, base_fees, effective_feerate, tx.txid()))
}

/// `AcceptToMemoryPool` as a class-M closure body: [`accept_single_transaction`] under `pool`'s
/// write guard, released before this returns; then the completion MP1a-4's dispatch runs. Its
/// `uncache` lists the inputs CoinsTip had not cached when the result is a rejection or
/// `test_accept`, and is empty for an admission; `reply` receives the result after the dispatch
/// uncached, flushed (PERIODIC) and ended the scope.
///
/// # Errors
///
/// A fault without a verdict; `reply` is dropped and the caller's reply disconnects.
// Core: validation.cpp:1732-1761 at v24.0.0
pub fn accept_to_memory_pool(
    pool: &dyn AcceptPool,
    chain: &mut dyn AcceptChain,
    tx: &TxRef,
    record: &StatelessRecord,
    args: AcceptArgs,
    reply: Box<dyn FnOnce(MempoolAcceptResult) + Send>,
) -> MempoolOutcome {
    let mut coins_to_uncache = Vec::new();
    let mut outcome = None;
    pool.with_write(&mut |state, signals| {
        outcome = Some(accept_single_transaction(
            state,
            &mut *chain,
            signals,
            tx,
            record,
            &args,
            &mut coins_to_uncache,
        ));
    });
    let result = outcome.ok_or_else(|| Fatal::assert("AcceptPool::with_write did not run"))??;
    let valid = result.result_type == MempoolAcceptResultType::Valid;
    if !valid {
        log_refusal(tx, &result);
    }
    if valid && !args.test_accept {
        coins_to_uncache.clear();
    }
    Ok(MempoolCompletion {
        uncache: coins_to_uncache,
        reply: Box::new(move || reply(result)),
    })
}

/// `AcceptToMemoryPool`'s log line for a result that is not VALID.
// Core: validation.cpp:1745-1747 at v24.0.0
pub(crate) fn log_refusal(tx: &TxRef, result: &MempoolAcceptResult) {
    tracing::debug!(
        target: "mempool",
        "AcceptToMemoryPool: {} {} ({})",
        tx.txid(),
        result.reason,
        result.debug_message
    );
}
