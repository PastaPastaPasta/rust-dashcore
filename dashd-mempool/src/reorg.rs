//! `MaybeUpdateMempoolForReorg` (`validation.cpp:425-525` at v24.0.0) on the held pool, in Core's
//! order: [`readd_disconnected`] puts each disconnected transaction back through
//! `AcceptToMemoryPool` or removes it with its descendants, then [`finish_reorg`] fixes the
//! re-added entries' in-pool descendants (`UpdateTransactionsFromBlock`), runs `removeForReorg`
//! with `filter_final_and_mature`, and `LimitMempoolSize`. Every removal and admission, and each
//! re-add's flush signal, reaches `signals` as it occurs.

use std::collections::BTreeSet;

use dash_consensus::TxRef;
use dash_params::ChainSetup;
use dash_primitives::TxHash;
use dashd_chainstate::chain::{AdmissionFault, Fatal, ReorgCx};

use crate::accept::finalize::{limit_mempool_size, uncache_all};
use crate::accept::reorg_filter::{filter_final_and_mature, test_lock_point_validity};
use crate::accept::single::{accept_single_transaction, log_refusal};
use crate::accept::{
    AcceptArgs, AcceptChain, AcceptSignals, StatelessRecord, announce_removals, with_coins,
};
use crate::pool::PoolState;
use crate::types::{MemPoolRemovalReason, MempoolAcceptResultType};

/// What the re-add leaves for [`finish_reorg`]: `vHashUpdate`, the re-added txids in re-add
/// order, and the fault that stopped it, if one did.
pub struct Readded {
    pub update: Vec<TxHash>,
    pub fault: Option<Fatal>,
}

/// The re-add loop over `queued` (`disconnectpool.take()`), in reverse: without `add`
/// (`fAddToMempool` false), for a coinbase, or for a transaction `AcceptToMemoryPool` at `add`'s
/// time with `bypass_limits` does not admit, `removeRecursive(tx, REORG)`; otherwise a
/// transaction now held joins `vHashUpdate`. A refused admission is logged as Core logs it and
/// its newly cached coins are uncached; its verdict is otherwise ignored. A fault stops the loop
/// and is returned with what was re-added before it, so the pool is still finished; a fault in
/// the re-add's PERIODIC flush comes after its admission, so that re-add is counted first.
// Core: validation.cpp:439-458, :1743-1754 at v24.0.0
pub fn readd_disconnected(
    state: &mut PoolState,
    queued: &[TxRef],
    mut add: Option<(&mut dyn AcceptChain, i64)>,
    signals: &mut dyn AcceptSignals,
) -> Readded {
    let mut update = Vec::new();
    for tx in queued.iter().rev() {
        let mut flush_fault = None;
        let admitted = match add.as_mut() {
            Some((chain, now)) if !tx.is_coinbase() => {
                match readd_one(state, &mut **chain, *now, tx, signals) {
                    Ok((admitted, fault)) => {
                        flush_fault = fault;
                        admitted
                    }
                    Err(fault) => {
                        return Readded {
                            update,
                            fault: Some(fault),
                        };
                    }
                }
            }
            _ => false,
        };
        if !admitted {
            state.remove_recursive(tx, MemPoolRemovalReason::Reorg);
            announce_removals(state, signals);
        } else if state.exists(&tx.txid()) {
            update.push(tx.txid());
        }
        if flush_fault.is_some() {
            return Readded {
                update,
                fault: flush_fault,
            };
        }
    }
    Readded {
        update,
        fault: None,
    }
}

/// `AcceptToMemoryPool(*this, tx, GetTime(), /*bypass_limits=*/true, /*test_accept=*/false)`:
/// whether it was VALID. As every `AcceptToMemoryPool` call, it ends with the uncache of a
/// refusal's coins and `FlushStateToDisk(PERIODIC)`, here the hold's, which reads the usage of
/// the pool this re-add holds. The flush's fault is returned beside the verdict, as the
/// admission it follows stands. A missing MNHF row is not admitted, with no log line, uncache or
/// flush, and the drain goes on. Core's `EvoDbMissingStateError` leaves the drain and the step
/// that called it instead (`validation.cpp:451`, `:3691`, `:3979`, `:4140` at v24.0.3; a
/// departure, CH7 D131).
// Core: validation.cpp:449-451, :1732-1761 at v24.0.2
fn readd_one(
    state: &mut PoolState,
    chain: &mut dyn AcceptChain,
    now: i64,
    tx: &TxRef,
    signals: &mut dyn AcceptSignals,
) -> Result<(bool, Option<Fatal>), Fatal> {
    let record = StatelessRecord::new(tx, state.options());
    let args = AcceptArgs::single_accept(now, true, false);
    let mut uncache = Vec::new();
    let result = match accept_single_transaction(
        state,
        &mut *chain,
        signals,
        tx,
        &record,
        &args,
        &mut uncache,
    ) {
        Ok(result) => result,
        Err(AdmissionFault::Unavailable(_)) => return Ok((false, None)),
        Err(AdmissionFault::Fatal(fatal)) => return Err(fatal),
    };
    let valid = result.result_type == MempoolAcceptResultType::Valid;
    if !valid {
        log_refusal(tx, &result);
        if !uncache.is_empty() {
            with_coins(chain, |coins| {
                uncache_all(coins, &uncache);
                Ok(())
            })?;
        }
    }
    let usage = state.dynamic_memory_usage() as u64;
    let flushed = chain.flush_periodic_in_hold(usage, &mut |event| signals.flushed(event));
    Ok((valid, flushed.err()))
}

/// The rest of `MaybeUpdateMempoolForReorg` after the re-add: `UpdateTransactionsFromBlock`
/// over `readded.update`, which runs even after a re-add fault so the pool's links and package
/// state stay consistent; then, without a fault, `removeForReorg(filter_final_and_mature)` over
/// `cx` and `LimitMempoolSize`, which uncaches through `cx`.
///
/// # Errors
///
/// The re-add's fault, the filter's (before it applies anything) or its final assert, or the
/// uncache's coins fault.
// Core: validation.cpp:460-524 at v24.0.2
pub fn finish_reorg(
    state: &mut PoolState,
    params: &ChainSetup,
    readded: Readded,
    cx: &mut dyn ReorgCx,
    signals: &mut dyn AcceptSignals,
) -> Result<(), Fatal> {
    state.update_transactions_from_block(&readded.update);
    announce_removals(state, signals);
    if let Some(fault) = readded.fault {
        return Err(fault);
    }
    let filtered = remove_for_reorg(state, params, cx);
    announce_removals(state, signals);
    filtered?;
    let limited = limit_mempool_size(state, |outpoints| cx.uncache(outpoints));
    announce_removals(state, signals);
    limited
}

/// `removeForReorg(chain, filter_final_and_mature)`: every entry filtered before any is removed,
/// recalculated lock points applied to the entries they belong to, then each removed entry with
/// its descendants, then Core's assert that every remaining entry's lock points are valid.
///
/// # Errors
///
/// The filter's fault, before anything is applied, or the final assert.
// Core: txmempool.cpp:937-956, validation.cpp:476-522 at v24.0.0
fn remove_for_reorg(
    state: &mut PoolState,
    params: &ChainSetup,
    cx: &mut dyn ReorgCx,
) -> Result<(), Fatal> {
    let mut remove = BTreeSet::new();
    let mut updates = Vec::new();
    for (txid, entry) in state.entries() {
        let (removed, lock_points) = filter_final_and_mature(params, &*state, entry, cx)?;
        if let Some(lock_points) = lock_points {
            updates.push((*txid, lock_points));
        }
        if removed {
            remove.insert(*txid);
        }
    }
    for (txid, lock_points) in updates {
        state.update_lock_points(txid, lock_points);
    }
    let mut all = BTreeSet::new();
    for txid in remove {
        all.extend(state.calculate_descendants(txid));
    }
    state.remove_staged(&all, false, MemPoolRemovalReason::Reorg);
    for entry in state.entries().values() {
        if !test_lock_point_validity(&*cx, &entry.metadata().lock_points) {
            return Err(Fatal::assert("removeForReorg: TestLockPointValidity of a kept entry"));
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "test-support"))]
#[path = "reorg_tests.rs"]
mod tests;
