//! `ProcessNewPackage` (`validation.cpp:1763-1790` at v24.0.2): `AcceptMultipleTransactions` for
//! a test accept, `AcceptPackage` for a child-with-parents submission, and their package checks.
//! A package runs in one class-M turn under one pool write guard, as Core runs it under
//! `cs_main` and `m_pool.cs`.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_consensus::{Amount, TxRef};
use dash_primitives::{OutPoint, TxHash};
use dashd_chainstate::chain::{AdmissionFault, Fatal, MempoolCompletion, MempoolOutcome};
use dashd_chainstate::checkqueue::script::TxSlot;

use super::consensus::{consensus_script_checks, policy_script_checks};
use super::finalize::{finalize, limit_mempool_size, uncache_all};
use super::prechecks::{StatelessRecord, Workspace, check_fee_rate, pre_checks};
use super::refresh::try_asset_unlock_refresh;
use super::single::{accept_single_transaction, failure, fee_rate, mempool_tx, success};
use super::special::asset_unlock_payload;
use super::view::PackageCoins;
use super::{AcceptArgs, AcceptChain, AcceptPool, AcceptSignals, announce_removals, with_coins};
use crate::pool::PoolState;
use crate::types::{
    MAX_PACKAGE_COUNT, MAX_PACKAGE_SIZE_KVB, MempoolAcceptResult, MempoolAcceptResultType,
    PackageMempoolAcceptResult, PackageValidationResult,
};

/// `CheckPackage`: at most [`MAX_PACKAGE_COUNT`] transactions, at most
/// [`MAX_PACKAGE_SIZE_KVB`] in total for more than one, parents before children, and no input
/// spent twice across transactions. `Err` is the `PCKG_POLICY` reason.
// Core: policy/packages.cpp:18-66 at v24.0.2
pub fn check_package(txns: &[TxRef]) -> Result<(), &'static str> {
    if txns.len() > MAX_PACKAGE_COUNT {
        return Err("package-too-many-transactions");
    }
    let total_size: i64 = txns.iter().map(|tx| i64::from(tx.size())).sum();
    if txns.len() > 1 && total_size > (MAX_PACKAGE_SIZE_KVB * 1_000) as i64 {
        return Err("package-too-large");
    }
    let mut later: BTreeSet<TxHash> = txns.iter().map(|tx| tx.txid()).collect();
    for tx in txns {
        if tx.inputs().iter().any(|input| later.contains(&input.prevout().txid())) {
            return Err("package-not-sorted");
        }
        later.remove(&tx.txid());
    }
    let mut seen = BTreeSet::new();
    for tx in txns {
        if tx.inputs().iter().any(|input| seen.contains(&input.prevout())) {
            return Err("conflict-in-package");
        }
        seen.extend(tx.inputs().iter().map(|input| input.prevout()));
    }
    Ok(())
}

/// `CheckPackageWithAssetUnlocks`: [`check_package`], then no two asset unlocks of one index.
// Core: validation.cpp:1468-1480 at v24.0.2
pub fn check_package_with_asset_unlocks(txns: &[TxRef]) -> Result<(), &'static str> {
    check_package(txns)?;
    let mut indexes = BTreeSet::new();
    for tx in txns.iter().filter(|tx| tx.is_platform_transfer()) {
        if asset_unlock_payload(tx).is_some_and(|payload| !indexes.insert(payload.index())) {
            return Err("assetunlock-conflicting-package");
        }
    }
    Ok(())
}

/// `IsChildWithParents`: at least two transactions, each before the last one of its parents.
// Core: policy/packages.cpp:68-83 at v24.0.2
pub fn is_child_with_parents(package: &[TxRef]) -> bool {
    let Some((child, parents)) = package.split_last() else {
        return false;
    };
    let input_txids: BTreeSet<TxHash> =
        child.inputs().iter().map(|input| input.prevout().txid()).collect();
    !parents.is_empty() && parents.iter().all(|tx| input_txids.contains(&tx.txid()))
}

/// `CheckPackageAssetUnlockEvictions`: no package transaction is, or spends, an entry that
/// admitting one of the package's asset unlocks would evict (another claimant of its index, with
/// its descendants).
// Core: validation.cpp:1159-1184 at v24.0.2
pub fn check_package_asset_unlock_evictions(
    state: &PoolState,
    txns: &[TxRef],
) -> Result<(), &'static str> {
    let mut evicted = BTreeSet::new();
    for tx in txns.iter().filter(|tx| tx.is_platform_transfer()) {
        let Some(payload) = asset_unlock_payload(tx) else {
            continue;
        };
        for txid in state.asset_unlock_txids_by_index(payload.index()) {
            if txid != tx.txid() {
                evicted.extend(state.calculate_descendants(txid));
            }
        }
    }
    let conflicting = txns.iter().any(|tx| {
        evicted.contains(&tx.txid())
            || tx.inputs().iter().any(|input| evicted.contains(&input.prevout().txid()))
    });
    if conflicting {
        return Err("assetunlock-conflicting-package");
    }
    Ok(())
}

fn refused(
    result: PackageValidationResult,
    reason: &str,
    debug: String,
) -> PackageMempoolAcceptResult {
    let mut refused = PackageMempoolAcceptResult::default();
    refused.state.invalid(result, reason, debug);
    refused
}

fn policy(reason: &str) -> PackageMempoolAcceptResult {
    refused(PackageValidationResult::Policy, reason, String::new())
}

/// `std::map::emplace`: the first result of a txid stands.
fn emplace(
    results: &mut BTreeMap<TxHash, MempoolAcceptResult>,
    txid: TxHash,
    result: MempoolAcceptResult,
) {
    results.entry(txid).or_insert(result);
}

/// `PCKG_TX` "transaction failed" with `tx`'s failure, the other transactions unfinished.
fn tx_failed(
    mut results: PackageMempoolAcceptResult,
    tx: &TxRef,
    reject: TxValidationError,
) -> PackageMempoolAcceptResult {
    results.state.invalid(PackageValidationResult::Tx, "transaction failed", "");
    emplace(&mut results.tx_results, tx.txid(), failure(reject));
    results
}

/// The package feerate and txids under `package_feerates`, otherwise the transaction's own.
struct Effective<'a> {
    package_feerates: bool,
    package_feerate: i64,
    txids: &'a [TxHash],
}

impl Effective<'_> {
    fn success(
        &self,
        txid: TxHash,
        vsize: i64,
        base_fees: Amount,
        modified_fees: Amount,
    ) -> MempoolAcceptResult {
        if !self.package_feerates {
            return success(vsize, base_fees, fee_rate(modified_fees, vsize), txid);
        }
        MempoolAcceptResult {
            txids_fee_calculations: Some(self.txids.to_vec()),
            ..success(vsize, base_fees, self.package_feerate, txid)
        }
    }
}

/// `AcceptMultipleTransactions`: the package checks, every transaction's `PreChecks` (each reading
/// the earlier ones' outputs through the package's coins), the package feerate under
/// `package_feerates`, `CheckPackageLimits` for more than one transaction, the policy script
/// checks, then the results under `test_accept` or `submit_package`. The first failing
/// transaction ends it as `PCKG_TX` with only its result.
///
/// # Errors
///
/// A fault without a verdict.
// Core: validation.cpp:1482-1564 at v24.0.2
pub fn accept_multiple_transactions(
    state: &mut PoolState,
    chain: &mut dyn AcceptChain,
    signals: &mut dyn AcceptSignals,
    txns: &[TxRef],
    records: &[StatelessRecord],
    args: &AcceptArgs,
    coins_to_uncache: &mut Vec<OutPoint>,
) -> Result<PackageMempoolAcceptResult, AdmissionFault> {
    if let Err(reason) = check_package_with_asset_unlocks(txns) {
        return Ok(policy(reason));
    }
    if let Err(reason) = check_package_asset_unlock_evictions(state, txns) {
        return Ok(policy(reason));
    }
    let mut results = PackageMempoolAcceptResult::default();
    let mut package = PackageCoins::default();
    let mut workspaces = Vec::with_capacity(txns.len());
    for (tx, record) in txns.iter().zip(records) {
        match pre_checks(state, chain, signals, tx, record, args, &package, coins_to_uncache)? {
            Ok(ws) => workspaces.push(ws),
            Err(reject) => return Ok(tx_failed(results, tx, reject)),
        }
        package.add_transaction(tx);
    }

    let total_vsize: i64 = workspaces.iter().map(|ws| ws.vsize).sum();
    let total_modified_fees =
        workspaces.iter().fold(0, |sum: Amount, ws| sum.saturating_add(ws.modified_fees));
    if args.package_feerates && check_fee_rate(state, total_vsize, total_modified_fees).is_err() {
        return Ok(policy("package-fee-too-low"));
    }
    if txns.len() > 1 {
        let limits = state.options().limits;
        if let Err(error) = state.check_package_limits(txns, &limits) {
            return Ok(refused(PackageValidationResult::Policy, "package-mempool-limits", error.0));
        }
    }

    let txids: Vec<TxHash> = txns.iter().map(|tx| tx.txid()).collect();
    let effective = Effective {
        package_feerates: args.package_feerates,
        package_feerate: fee_rate(total_modified_fees, total_vsize),
        txids: &txids,
    };
    let caches = chain.script_caches();
    for (tx, ws) in txns.iter().zip(&workspaces) {
        if let Err(reject) = policy_script_checks(&TxSlot::new(tx), &ws.view, caches.as_deref())? {
            return Ok(tx_failed(results, tx, reject));
        }
        if args.test_accept {
            let result = effective.success(tx.txid(), ws.vsize, ws.base_fees, ws.modified_fees);
            emplace(&mut results.tx_results, tx.txid(), result);
        }
    }
    if !args.test_accept {
        submit_package(state, chain, signals, txns, workspaces, args, &effective, &mut results)?;
    }
    Ok(results)
}

/// A `PCKG_MEMPOOL_ERROR` "BUG!" outcome for `txid`, with the workspace's state as its result.
fn bug(
    results: &mut PackageMempoolAcceptResult,
    txid: TxHash,
    ws_state: &TxValidationError,
    what: &str,
) {
    emplace(&mut results.tx_results, txid, failure(ws_state.clone()));
    results.state.invalid(
        PackageValidationResult::MempoolError,
        format!("BUG! {what}: {txid}"),
        "",
    );
}

/// `SubmitPackage`: each transaction's consensus script checks, its ancestors recalculated and
/// `Finalize` without the trim, in package order; then `LimitMempoolSize` once, every removal
/// announced, and `TransactionAddedToMempool` with the next sequence for each transaction still
/// held, in package order. One the trim took is `mempool full` and the package `PCKG_TX`. The
/// three `PCKG_MEMPOOL_ERROR` "BUG!" outcomes record the failure and carry on, as Core does.
///
/// # Errors
///
/// A fault without a verdict, or Core's assert that no package transaction is held.
// Core: validation.cpp:1291-1370 at v24.0.2
#[allow(clippy::too_many_arguments)]
fn submit_package(
    state: &mut PoolState,
    chain: &mut dyn AcceptChain,
    signals: &mut dyn AcceptSignals,
    txns: &[TxRef],
    workspaces: Vec<Workspace>,
    args: &AcceptArgs,
    effective: &Effective<'_>,
    results: &mut PackageMempoolAcceptResult,
) -> Result<(), Fatal> {
    if txns.iter().any(|tx| state.exists(&tx.txid())) {
        return Err(Fatal::assert("SubmitPackage: a package transaction is already held"));
    }
    let caches = chain.script_caches();
    let block_flags = chain
        .block_script_flags()
        .ok_or_else(|| Fatal::assert("ConsensusScriptChecks without an active tip"))?;
    let limits = state.options().limits;
    let mut sizes = Vec::with_capacity(txns.len());
    for (tx, mut ws) in txns.iter().zip(workspaces) {
        let txid = tx.txid();
        let slot = TxSlot::new(tx);
        let consensus = with_coins(&mut *chain, |coins| {
            consensus_script_checks(&slot, &ws.view, &*state, coins, block_flags, caches.as_deref())
        })?;
        let ws_state = match consensus {
            Ok(()) => TxValidationError::new(TxValidationResult::Unset, ""),
            Err(reject) => {
                let what = "PolicyScriptChecks succeeded but ConsensusScriptChecks failed";
                bug(results, txid, &reject, what);
                reject
            }
        };
        match state.calculate_mem_pool_ancestors(&ws.entry, &limits, true) {
            Ok(ancestors) => ws.ancestors = ancestors,
            Err(_) => bug(
                results,
                txid,
                &ws_state,
                "Mempool ancestors or descendants were underestimated",
            ),
        }
        sizes.push((ws.vsize, ws.base_fees, ws.modified_fees));
        if let Err(reject) = finalize(state, &mut *chain, ws, args)? {
            bug(results, txid, &reject, "Adding to mempool failed");
        }
    }

    limit_mempool_size(state, |outpoints| {
        with_coins(&mut *chain, |coins| {
            uncache_all(coins, outpoints);
            Ok(())
        })
    })?;
    announce_removals(state, signals);
    for (tx, (vsize, base_fees, modified_fees)) in txns.iter().zip(sizes) {
        let txid = tx.txid();
        if state.exists(&txid) {
            emplace(
                &mut results.tx_results,
                txid,
                effective.success(txid, vsize, base_fees, modified_fees),
            );
            let sequence = state.get_and_increment_sequence();
            signals.added(tx.clone(), args.accept_time, sequence);
        } else {
            results.state.invalid(PackageValidationResult::Tx, "transaction failed", "");
            let full = TxValidationError::new(TxValidationResult::MempoolPolicy, "mempool full");
            emplace(&mut results.tx_results, txid, failure(full));
        }
    }
    Ok(())
}

/// `AcceptPackage`: the package checks; one child with its parents whose other inputs are
/// confirmed (each child input CoinsTip had not cached joins `coins_to_uncache`); no asset-unlock
/// eviction conflict; then each transaction on its own (`SingleInPackageAccept`), a held one
/// reported as its entry or refreshed in place, and the ones refused only for a fee or a missing
/// input go to [`accept_multiple_transactions`] together. A refusal for any other reason ends
/// it as `PCKG_TX` with every transaction's result.
///
/// # Errors
///
/// A fault without a verdict.
// Core: validation.cpp:1566-1730 at v24.0.2
pub fn accept_package(
    state: &mut PoolState,
    chain: &mut dyn AcceptChain,
    signals: &mut dyn AcceptSignals,
    package: &[TxRef],
    records: &[StatelessRecord],
    args: &AcceptArgs,
    coins_to_uncache: &mut Vec<OutPoint>,
) -> Result<PackageMempoolAcceptResult, AdmissionFault> {
    if let Err(reason) = check_package_with_asset_unlocks(package) {
        return Ok(policy(reason));
    }
    if !is_child_with_parents(package) {
        return Ok(policy("package-not-child-with-parents"));
    }
    let Some((child, parents)) = package.split_last() else {
        return Err(Fatal::assert("AcceptPackage: package.size() > 1").into());
    };
    let unconfirmed_parent_txids: BTreeSet<TxHash> = parents.iter().map(|tx| tx.txid()).collect();
    let package_or_confirmed = with_coins(&mut *chain, |coins| {
        for input in child.inputs() {
            if !coins.have_in_cache(&input.prevout()) {
                coins_to_uncache.push(input.prevout());
            }
        }
        for input in child.inputs() {
            let prevout = input.prevout();
            if !unconfirmed_parent_txids.contains(&prevout.txid())
                && coins.fetch(&prevout)?.is_none()
            {
                return Ok(false);
            }
        }
        Ok(true)
    })?;
    if !package_or_confirmed {
        return Ok(policy("package-not-child-with-unconfirmed-parents"));
    }
    if let Err(reason) = check_package_asset_unlock_evictions(state, package) {
        return Ok(policy(reason));
    }

    let single_args = args.single_in_package_accept();
    let mut quit_early = PackageMempoolAcceptResult::default();
    let mut individual_results_nonfinal = BTreeMap::new();
    let (mut txns_package_eval, mut records_package_eval) = (Vec::new(), Vec::new());
    for (tx, record) in package.iter().zip(records) {
        let txid = tx.txid();
        let held_instance = state.get_iter(&txid).map(|entry| entry.tx().instance_hash());
        if let Some(held_instance) = held_instance {
            if tx.is_asset_unlock_with_stable_txid()
                && held_instance != tx.instance_hash()
                && let Some(refresh) =
                    try_asset_unlock_refresh(state, chain, signals, tx, &single_args)?
            {
                if refresh.result_type == MempoolAcceptResultType::Valid {
                    emplace(&mut quit_early.tx_results, txid, refresh);
                } else {
                    quit_early.state.invalid(PackageValidationResult::Tx, "transaction failed", "");
                    emplace(&mut individual_results_nonfinal, txid, refresh);
                }
                continue;
            }
            let entry = state
                .get_iter(&txid)
                .ok_or_else(|| Fatal::assert("AcceptPackage: iter != std::nullopt"))?;
            let held = mempool_tx(entry.vsize() as i64, entry.metadata().fee);
            emplace(&mut quit_early.tx_results, txid, held);
            continue;
        }
        let single = accept_single_transaction(
            state,
            chain,
            signals,
            tx,
            record,
            &single_args,
            coins_to_uncache,
        )?;
        if single.result_type == MempoolAcceptResultType::Valid {
            if !state.exists(&txid) {
                return Err(Fatal::assert("AcceptPackage: m_pool.exists(txid)").into());
            }
            emplace(&mut quit_early.tx_results, txid, single);
        } else if !matches!(
            single.validation_result,
            TxValidationResult::MempoolPolicy | TxValidationResult::MissingInputs
        ) {
            quit_early.state.invalid(PackageValidationResult::Tx, "transaction failed", "");
            emplace(&mut individual_results_nonfinal, txid, single);
        } else {
            emplace(&mut individual_results_nonfinal, txid, single);
            txns_package_eval.push(tx.clone());
            records_package_eval.push(record.clone());
        }
    }

    if quit_early.state.result != PackageValidationResult::Unset || txns_package_eval.is_empty() {
        for (txid, result) in individual_results_nonfinal {
            emplace(&mut quit_early.tx_results, txid, result);
        }
        return Ok(quit_early);
    }
    let mut submission = accept_multiple_transactions(
        state,
        chain,
        signals,
        &txns_package_eval,
        &records_package_eval,
        args,
        coins_to_uncache,
    )?;
    for (txid, result) in quit_early.tx_results {
        emplace(&mut submission.tx_results, txid, result);
    }
    if submission.state.result == PackageValidationResult::Tx {
        for (txid, result) in individual_results_nonfinal {
            emplace(&mut submission.tx_results, txid, result);
        }
    }
    Ok(submission)
}

/// `ProcessNewPackage(txns, test_accept)` at `accept_time` as a class-M closure body:
/// [`accept_multiple_transactions`] under `PackageTestAccept`, or [`accept_package`] under
/// `PackageChildWithParents`, in one call of `pool`'s write guard, every removal announced before
/// it is released. The completion uncaches the inputs CoinsTip had not cached when `test_accept`
/// or the package is invalid (the three "BUG!" outcomes included); `reply` receives the result
/// after MP1a-4's dispatch uncached, flushed (PERIODIC) and ended the scope. `txns` is non-empty,
/// with `records[i]` stage A's record of `txns[i]`. A missing MNHF row is Core's
/// `EvoDbMissingStateError` leaving `ProcessNewPackage` (`v:1772-1781` at v24.0.3), from
/// PreChecks or from `TryAssetUnlockRefresh`'s `GetSpecialTxRules` (`:1411`, reached from `:1434`
/// and `:1654`): the result carries only its text, nothing is uncached (`:1783-1788`), and
/// nothing is marked invalid; what the package admitted before it stays. Core's exception also skips the PERIODIC flush
/// (`:1789-1791`), which the dispatch still runs (CH7 D131).
///
/// # Errors
///
/// A fault without a verdict, or a precondition broken; `reply` is dropped.
// Core: validation.cpp:1763-1790 at v24.0.2
pub fn process_new_package(
    pool: &dyn AcceptPool,
    chain: &mut dyn AcceptChain,
    txns: &[TxRef],
    records: &[StatelessRecord],
    accept_time: i64,
    test_accept: bool,
    reply: Box<dyn FnOnce(PackageMempoolAcceptResult) + Send>,
) -> MempoolOutcome {
    if txns.is_empty() || records.len() != txns.len() {
        return Err(Fatal::assert(
            "ProcessNewPackage: an empty package or a record per tx missing",
        ));
    }
    let mut coins_to_uncache = Vec::new();
    let mut outcome = None;
    pool.with_write(&mut |state, signals| {
        let result = if test_accept {
            let args = AcceptArgs::package_test_accept(accept_time);
            accept_multiple_transactions(
                state,
                &mut *chain,
                signals,
                txns,
                records,
                &args,
                &mut coins_to_uncache,
            )
        } else {
            let args = AcceptArgs::package_child_with_parents(accept_time);
            accept_package(state, &mut *chain, signals, txns, records, &args, &mut coins_to_uncache)
        };
        announce_removals(state, signals);
        outcome = Some(result);
    });
    let result = match outcome.ok_or_else(|| Fatal::assert("AcceptPool::with_write did not run"))? {
        Ok(result) => result,
        Err(AdmissionFault::Unavailable(text)) => {
            let result = PackageMempoolAcceptResult {
                unavailable: Some(text),
                ..PackageMempoolAcceptResult::default()
            };
            return Ok(MempoolCompletion {
                uncache: Vec::new(),
                reply: Box::new(move || reply(result)),
            });
        }
        Err(AdmissionFault::Fatal(fatal)) => return Err(fatal),
    };
    if !test_accept && result.state.result == PackageValidationResult::Unset {
        coins_to_uncache.clear();
    }
    Ok(MempoolCompletion {
        uncache: coins_to_uncache,
        reply: Box::new(move || reply(result)),
    })
}
