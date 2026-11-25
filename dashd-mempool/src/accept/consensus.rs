//! `PreChecks`' and the script checks' consensus calls at the tip: finality, BIP68 lock points,
//! `Consensus::CheckTxInputs`, the sigop count, the V24 shared-collateral covenant, and
//! `PolicyScriptChecks`/`ConsensusScriptChecks` with their caches.

use dash_consensus::coins::{Coin, ViewOr};
use dash_consensus::ctx::{
    CheckpointView, ConsensusCtx, Deployment, DeploymentView, bls_ctx_for_block,
};
use dash_consensus::script::{CheckInputsError, ScriptFlags, ScriptPolicy};
use dash_consensus::tx::{
    calculate_sequence_locks, check_tx_inputs, evaluate_sequence_locks, is_final_tx,
    transaction_sigop_count,
};
use dash_consensus::validation::TxValidationError;
use dash_consensus::{Amount, BlockId, Tx};
use dash_dml::specialtx::{
    SpecialTxFault, shared_collateral_spends, shared_collateral_template_outputs,
};
use dash_params::ChainSetup;
use dash_primitives::OutPoint;
use dashd_chainstate::chain::seams::AtmpCoins;
use dashd_chainstate::chain::{Fatal, TipFacts};
use dashd_chainstate::checkqueue::script::{ScriptOptions, TxSlot, check_input_scripts};
use dashd_chainstate::sigcache::ScriptCaches;

use super::view::{InputView, MEMPOOL_HEIGHT};
use crate::api::MempoolRead;
use crate::policy::consts::{STANDARD_LOCKTIME_VERIFY_FLAGS, STANDARD_SCRIPT_VERIFY_FLAGS};
use crate::types::LockPoints;

/// The active tip, which every caller has: Core's `Assert(m_chain.Tip())`.
fn tip_of(facts: &dyn TipFacts) -> Result<BlockId, Fatal> {
    facts.tip().ok_or_else(|| Fatal::assert("the mempool's checks without an active tip"))
}

/// `CheckFinalTxAtTip`: `IsFinalTx` at the tip's height plus one and its median time past.
///
/// # Errors
///
/// No active tip (Core asserts).
// Core: validation.cpp:205-225 at v24.0.0
pub fn check_final_tx_at_tip(facts: &dyn TipFacts, tx: &Tx) -> Result<bool, Fatal> {
    let header = facts.ancestry().header(tip_of(facts)?);
    let height = i32::try_from(header.height + 1)
        .map_err(|_| Fatal::assert("CheckFinalTxAtTip: a tip height above int"))?;
    Ok(is_final_tx(tx, height, header.mtp))
}

/// Sequence locks read no deployment and no checkpoint (`tx/locks.rs`); the context's views are
/// never called.
struct LocksOnly;

impl DeploymentView for LocksOnly {
    fn active_after(&self, _: Option<BlockId>, _: Deployment) -> bool {
        unreachable!("sequence locks read no deployment")
    }

    fn active_at(&self, _: BlockId, _: Deployment) -> bool {
        unreachable!("sequence locks read no deployment")
    }
}

impl CheckpointView for LocksOnly {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        unreachable!("sequence locks read no checkpoint")
    }
}

/// `f` over the context of the block after the tip, Core's `next_tip`/`index` with `pprev = tip`
/// and `nHeight = tip + 1`.
fn at_next_block<R>(
    params: &ChainSetup,
    facts: &dyn TipFacts,
    f: impl FnOnce(&ConsensusCtx<'_>) -> R,
) -> Result<R, Fatal> {
    let tip = tip_of(facts)?;
    let header = facts.ancestry().header(tip);
    let height = header.height + 1;
    let ctx = ConsensusCtx {
        params,
        height,
        prev: Some(tip),
        chain: facts.ancestry(),
        deployments: &LocksOnly,
        checkpoints: &LocksOnly,
        bls: bls_ctx_for_block(params, height),
        median_time_past: header.mtp,
        header_check_time: 0,
    };
    Ok(f(&ctx))
}

/// `CalculatePrevHeights`' reads: each input's coin height through `coin_height`, in order, until
/// the first missing input (`None`, Core's `nullopt`).
///
/// # Errors
///
/// `coin_height`'s fault.
// Core: validation.cpp:227-258 at v24.0.0 CalculatePrevHeights
pub fn calculate_prev_heights(
    tx: &Tx,
    coin_height: &mut dyn FnMut(&OutPoint) -> Result<Option<u32>, Fatal>,
) -> Result<Option<Vec<u32>>, Fatal> {
    let mut prev_heights = Vec::with_capacity(tx.inputs().len());
    for input in tx.inputs() {
        let Some(height) = coin_height(&input.prevout())? else {
            tracing::info!(
                "ERROR: CalculatePrevHeights: Missing input {} in transaction '{}'",
                prev_heights.len(),
                tx.txid()
            );
            return Ok(None);
        };
        prev_heights.push(height);
    }
    Ok(Some(prev_heights))
}

/// `CalculateLockPointsAtTip` over the heights [`calculate_prev_heights`] read: an in-pool coin's
/// [`MEMPOOL_HEIGHT`] counts as the tip's height plus one, `CalculateSequenceLocks` runs under
/// `STANDARD_LOCKTIME_VERIFY_FLAGS` for the block after the tip, and `maxInputBlock` is the tip's
/// ancestor at the highest input height below the tip's height plus one, after
/// `CalculateSequenceLocks` zeroed the heights of the inputs without a relative lock.
///
/// # Errors
///
/// No active tip, or an input height above the tip (Core asserts).
// Core: validation.cpp:244-306 at v24.0.0 CalculatePrevHeights, CalculateLockPointsAtTip
pub fn calculate_lock_points_at_tip(
    params: &ChainSetup,
    facts: &dyn TipFacts,
    tx: &Tx,
    mut prev_heights: Vec<u32>,
) -> Result<LockPoints, Fatal> {
    let tip = tip_of(facts)?;
    let next_height = facts.ancestry().header(tip).height + 1;
    for height in &mut prev_heights {
        if *height == MEMPOOL_HEIGHT {
            *height = next_height;
        }
    }
    let locks = at_next_block(params, facts, |ctx| {
        calculate_sequence_locks(tx, STANDARD_LOCKTIME_VERIFY_FLAGS, &mut prev_heights, ctx)
    })?;
    // Can ignore mempool inputs since we'll fail if they had non-zero locks
    let max_input_height =
        prev_heights.iter().copied().filter(|&h| h != next_height).max().unwrap_or(0);
    let max_input_block = facts.ancestry().ancestor(tip, max_input_height).ok_or_else(|| {
        Fatal::assert(format!("CalculateLockPointsAtTip: no ancestor at {max_input_height}"))
    })?;
    Ok(LockPoints {
        height: locks.min_height,
        time: locks.min_time,
        max_input_block: Some(max_input_block),
    })
}

/// `CheckSequenceLocksAtTip`: `EvaluateSequenceLocks` of `lock_points` for the block after the
/// tip.
///
/// # Errors
///
/// No active tip (Core asserts).
// Core: validation.cpp:308-324 at v24.0.0
pub fn check_sequence_locks_at_tip(
    params: &ChainSetup,
    facts: &dyn TipFacts,
    lock_points: &LockPoints,
) -> Result<bool, Fatal> {
    let locks = dash_consensus::tx::SequenceLocks {
        min_height: lock_points.height,
        min_time: lock_points.time,
    };
    at_next_block(params, facts, |ctx| evaluate_sequence_locks(ctx, locks))
}

/// `Consensus::CheckTxInputs` over `m_view` at the tip's height plus one: the fee, or Core's
/// reject.
// Core: validation.cpp:1014-1018 at v24.0.0; consensus/tx_verify.cpp:163
pub fn check_tx_inputs_at_tip(
    tx: &Tx,
    view: &InputView,
    tip_height: u32,
) -> Result<Amount, TxValidationError> {
    check_tx_inputs(tx, view, tip_height + 1).map_err(|e| match e {
        ViewOr::Reject(reject) => reject,
        ViewOr::View(never) => match never {},
    })
}

/// `GetTransactionSigOpCount(tx, m_view, STANDARD_SCRIPT_VERIFY_FLAGS)`.
///
/// # Errors
///
/// A coin `m_view` lacks, where Core asserts.
// Core: validation.cpp:1036 at v24.0.0; consensus/tx_verify.cpp:149
pub fn sigop_count(tx: &Tx, view: &InputView) -> Result<u32, Fatal> {
    transaction_sigop_count(tx, view, STANDARD_SCRIPT_VERIFY_FLAGS).map_err(|e| match e {
        ViewOr::Reject(missing) => Fatal::assert(format!(
            "GetTransactionSigOpCount: input {} not in m_view",
            missing.input
        )),
        ViewOr::View(never) => match never {},
    })
}

/// With V24 active after the tip, `CheckSharedCollateralSpends` over `m_view`, then
/// `CheckSharedCollateralTemplateOutputs`.
///
/// # Errors
///
/// A fault the covenant cannot raise over `m_view` (it reads no quorum and no signature).
// Core: validation.cpp:1019-1029 at v24.0.0
pub fn check_shared_collateral(
    tx: &Tx,
    view: &InputView,
    v24_active_after_tip: bool,
) -> Result<Result<(), TxValidationError>, Fatal> {
    if !v24_active_after_tip {
        return Ok(Ok(()));
    }
    match shared_collateral_spends(tx, view) {
        Ok(Ok(())) => {}
        Ok(Err(reject)) => return Ok(Err(reject)),
        Err(SpecialTxFault::Coins(never)) => match never {},
        Err(fault) => {
            return Err(Fatal::new(format!("CheckSharedCollateralSpends: {fault:?}")));
        }
    }
    Ok(shared_collateral_template_outputs(tx, tx.payload()))
}

/// `CheckInputScripts` serially (no `pvChecks`) over `m_view`, the first failing input
/// classified under v24's policy.
fn input_scripts<'s>(
    slot: &'s TxSlot<'s>,
    view: &'s InputView,
    flags: ScriptFlags,
    caches: Option<&'s ScriptCaches>,
    cache_full_script_store: bool,
) -> Result<Result<(), TxValidationError>, Fatal> {
    let inputs = slot.tx().inputs();
    let opts = ScriptOptions {
        flags,
        policy: ScriptPolicy::V24_0,
        caches,
        cache_sig_store: true,
        cache_full_script_store,
    };
    let spent = |n: usize| -> &'s [u8] {
        view.get(&inputs[n].prevout())
            .map_or(&[][..], |coin: &'s Coin| coin.script_pubkey().as_slice())
    };
    if let Some(n) = inputs.iter().position(|input| view.get(&input.prevout()).is_none()) {
        return Err(Fatal::assert(format!("CheckInputScripts: input {n} not in m_view")));
    }
    match check_input_scripts(slot, spent, &opts, None) {
        Ok(()) => Ok(Ok(())),
        Err(CheckInputsError::Script(failure)) => Ok(Err(failure.into_tx_error())),
        Err(CheckInputsError::Flags(e)) => {
            Err(Fatal::assert(format!("CheckInputScripts: invalid flags {e:?}")))
        }
    }
}

/// `PolicyScriptChecks`: `CheckInputScripts` under `STANDARD_SCRIPT_VERIFY_FLAGS`, storing
/// signatures, not the script execution.
///
/// # Errors
///
/// An input `m_view` lacks (Core asserts).
// Core: validation.cpp:1186-1202 at v24.0.0
pub fn policy_script_checks<'s>(
    slot: &'s TxSlot<'s>,
    view: &'s InputView,
    caches: Option<&'s ScriptCaches>,
) -> Result<Result<(), TxValidationError>, Fatal> {
    input_scripts(slot, view, STANDARD_SCRIPT_VERIFY_FLAGS, caches, false)
}

/// `ConsensusScriptChecks` through `CheckInputsFromMempoolAndCache`: every input's coin in
/// `m_view` must be its in-pool parent's output, or CoinsTip's coin; then `CheckInputScripts`
/// under the tip's block flags, storing signatures and the script execution. A failure here is
/// Core's "BUG!" line and its `false`, with the state `CheckInputScripts` set.
///
/// # Errors
///
/// A coins read fault; a coin-equality assert; a coin `m_view` lacks (Core's `Assume`).
// Core: validation.cpp:532-567, :1204-1235 at v24.0.0
pub fn consensus_script_checks<'s>(
    slot: &'s TxSlot<'s>,
    view: &'s InputView,
    pool: &dyn MempoolRead,
    coins_tip: &mut dyn AtmpCoins,
    block_flags: ScriptFlags,
    caches: Option<&'s ScriptCaches>,
) -> Result<Result<(), TxValidationError>, Fatal> {
    let tx = slot.tx();
    if tx.is_coinbase() {
        return Err(Fatal::assert("CheckInputsFromMempoolAndCache: a coinbase"));
    }
    for input in tx.inputs() {
        let prevout = input.prevout();
        let Some(coin) = view.get(&prevout) else {
            return Err(Fatal::assert("CheckInputsFromMempoolAndCache: a spent coin in m_view"));
        };
        let same = |value: i64, script: &[u8]| {
            coin.value() == value && coin.script_pubkey().as_slice() == script
        };
        if let Some(from) = pool.get(&prevout.txid()) {
            let out = from.outputs().get(prevout.index() as usize).ok_or_else(|| {
                Fatal::assert("CheckInputsFromMempoolAndCache: txFrom->vout.size() > n")
            })?;
            if !same(out.value(), out.script_pubkey().as_bytes()) {
                return Err(Fatal::assert(
                    "CheckInputsFromMempoolAndCache: txFrom->vout[n] == coin.out",
                ));
            }
        } else {
            let utxo = coins_tip.access(&prevout)?.ok_or_else(|| {
                Fatal::assert("CheckInputsFromMempoolAndCache: !coinFromUTXOSet.IsSpent()")
            })?;
            if !same(utxo.value(), utxo.script_pubkey().as_slice()) {
                return Err(Fatal::assert(
                    "CheckInputsFromMempoolAndCache: coinFromUTXOSet.out == coin.out",
                ));
            }
        }
    }
    let verdict = input_scripts(slot, view, block_flags, caches, true)?;
    if let Err(reject) = &verdict {
        tracing::error!(
            "BUG! PLEASE REPORT THIS! CheckInputScripts failed against latest-block but not STANDARD flags {}, {}",
            tx.txid(),
            reject
        );
    }
    Ok(verdict)
}
