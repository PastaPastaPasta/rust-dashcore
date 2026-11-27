//! `MemPoolAccept::PreChecks` (`validation.cpp:839-1136` at v24.0.0), in Core's order. Rows 1 and
//! 3-7 read a [`StatelessRecord`]: stage A computes it off V (MP1a-7), a serial caller in place.

use std::collections::BTreeSet;

use dash_consensus::coins::Coin;
use dash_consensus::ctx::{
    CheckpointView, ConsensusCtx, Deployment, DeploymentPos, DeploymentView, bls_ctx_for_block,
};
use dash_consensus::tx::{check_transaction, contextual_check_transaction};
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_consensus::{Amount, BlockId, Tx, TxRef};
use dash_params::BuriedDeployment;
use dash_primitives::{OutPoint, TxHash, TxType};
use dashd_chainstate::chain::{AdmissionFault, Fatal};

use super::consensus::{
    calculate_lock_points_at_tip, calculate_prev_heights, check_final_tx_at_tip,
    check_sequence_locks_at_tip, check_shared_collateral, check_tx_inputs_at_tip, sigop_count,
};
use super::special::{
    asset_unlock_payload, check_special_tx_for_mempool, check_unlock_index_unmined,
};
use super::view::{InputView, PackageCoins, fetch_inputs};
use super::{AcceptArgs, AcceptChain, AcceptSignals, announce_removals, with_coins};
use crate::options::{MemPoolLimits, MemPoolOptions};
use crate::policy::consts::{
    EXTRA_DESCENDANT_TX_SIZE_LIMIT, MAX_STANDARD_TX_SIGOPS, MIN_STANDARD_TX_SIZE,
};
use crate::policy::dust::get_fee;
use crate::policy::{are_inputs_standard, is_standard_special_tx, is_standard_tx};
use crate::pool::{EntryMetadata, PoolEntry, PoolState};

/// The results of `PreChecks`' stateless checks for one transaction instance under the pool's
/// (immutable) options: `CheckTransaction`, `qc-not-allowed`, `coinbase`, `IsStandardTx`,
/// `IsStandardSpecialTx` and `tx-size-small` (rows 1 and 3-7). Each is read only at its Core
/// position, so a contextual or earlier failure still wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatelessRecord {
    instance: TxHash,
    check_transaction: Result<(), TxValidationError>,
    quorum_commitment: bool,
    coinbase: bool,
    standard: Result<(), &'static str>,
    standard_special: Result<(), &'static str>,
    size_small: bool,
}

impl StatelessRecord {
    // Core: validation.cpp:857-887 at v24.0.0
    pub fn new(tx: &Tx, options: &MemPoolOptions) -> StatelessRecord {
        StatelessRecord {
            instance: tx.instance_hash(),
            check_transaction: check_transaction(tx),
            quorum_commitment: tx.is_special_tx_version()
                && tx.tx_type() == TxType::QuorumCommitment,
            coinbase: tx.is_coinbase(),
            standard: is_standard_tx(
                tx,
                options.max_datacarrier_bytes,
                options.permit_bare_multisig,
                options.dust_relay_feerate,
            ),
            standard_special: is_standard_special_tx(tx),
            size_small: tx.size() < MIN_STANDARD_TX_SIZE,
        }
    }

    /// Whether this record was computed for `tx`'s instance.
    pub fn is_for(&self, tx: &Tx) -> bool {
        self.instance == tx.instance_hash()
    }
}

/// `PreChecks`' `Workspace` after it passed: the entry not yet inserted, its ancestors and the
/// detached `m_view`.
pub struct Workspace {
    pub entry: PoolEntry,
    pub ancestors: BTreeSet<TxHash>,
    pub view: InputView,
    pub vsize: i64,
    pub base_fees: Amount,
    pub modified_fees: Amount,
}

fn invalid(result: TxValidationResult, reason: &'static str) -> TxValidationError {
    TxValidationError::new(result, reason)
}

/// `ContextualCheckTransaction` reads only DIP0001 and DIP0003 after the tip.
struct TipBuried {
    tip: BlockId,
    dip0001: bool,
    dip0003: bool,
}

impl DeploymentView for TipBuried {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        assert_eq!(prev, Some(self.tip), "ContextualCheckTransaction at the tip");
        match deployment {
            Deployment::Buried(BuriedDeployment::Dip0001) => self.dip0001,
            Deployment::Buried(BuriedDeployment::Dip0003) => self.dip0003,
            _ => unreachable!("ContextualCheckTransaction reads DIP0001 and DIP0003 only"),
        }
    }

    fn active_at(&self, _: BlockId, _: Deployment) -> bool {
        unreachable!("ContextualCheckTransaction reads no deployment at a block")
    }
}

impl CheckpointView for TipBuried {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        unreachable!("ContextualCheckTransaction reads no checkpoint")
    }
}

/// `ContextualCheckTransaction(tx, state, consensus, m_chain.Tip())`.
// Core: validation.cpp:351-391, :861-864 at v24.0.0
fn contextual_at_tip(
    chain: &dyn AcceptChain,
    tip: BlockId,
    tx: &Tx,
) -> Result<Result<(), TxValidationError>, AdmissionFault> {
    let buried = TipBuried {
        tip,
        dip0001: chain.deployment_active_after(Deployment::Buried(BuriedDeployment::Dip0001))?,
        dip0003: chain.deployment_active_after(Deployment::Buried(BuriedDeployment::Dip0003))?,
    };
    let header = chain.ancestry().header(tip);
    let params = chain.params();
    let ctx = ConsensusCtx {
        params,
        height: header.height + 1,
        prev: Some(tip),
        chain: chain.ancestry(),
        deployments: &buried,
        checkpoints: &buried,
        bls: bls_ctx_for_block(params, header.height + 1),
        median_time_past: header.mtp,
        header_check_time: 0,
    };
    Ok(contextual_check_transaction(tx, &ctx))
}

/// `CheckFeeRate(package_size, package_fee)`: the pool's rolling minimum (`GetMinFee()`), then
/// the minimum relay feerate. `package_size` is Core's `size_t` narrowed to `uint32_t`.
// Core: validation.cpp:786-799 at v24.0.2
pub(crate) fn check_fee_rate(
    state: &PoolState,
    package_size: i64,
    package_fee: Amount,
) -> Result<(), TxValidationError> {
    let size = package_size as u32;
    let options = state.options();
    let min_fee_rate = state.get_min_fee();
    let mempool_reject_fee = get_fee(min_fee_rate, size);
    if mempool_reject_fee > 0 && package_fee < mempool_reject_fee {
        return Err(invalid(TxValidationResult::MempoolPolicy, "mempool min fee not met")
            .with_debug(format!("{package_fee} < {mempool_reject_fee}")));
    }
    let min_relay_fee = get_fee(options.min_relay_feerate, size);
    if package_fee < min_relay_fee {
        return Err(invalid(TxValidationResult::MempoolPolicy, "min relay fee not met")
            .with_debug(format!("{package_fee} < {min_relay_fee}")));
    }
    Ok(())
}

/// `CalculateMemPoolAncestors` under the pool's limits, then, for a transaction of at most
/// `EXTRA_DESCENDANT_TX_SIZE_LIMIT` vbytes, the CPFP carve-out: at most one in-pool ancestor and
/// one descendant over the limit. A failure keeps the first call's error string.
// Core: validation.cpp:1078-1106 at v24.0.0
fn ancestors_with_carve_out(
    state: &PoolState,
    entry: &PoolEntry,
    vsize: i64,
) -> Result<BTreeSet<TxHash>, TxValidationError> {
    let limits = state.options().limits;
    let error = match state.calculate_mem_pool_ancestors(entry, &limits, true) {
        Ok(ancestors) => return Ok(ancestors),
        Err(error) => error,
    };
    let too_long = || {
        invalid(TxValidationResult::MempoolPolicy, "too-long-mempool-chain")
            .with_debug(error.0.clone())
    };
    if vsize > i64::from(EXTRA_DESCENDANT_TX_SIZE_LIMIT) {
        return Err(too_long());
    }
    let carve_out = MemPoolLimits {
        ancestor_count: 2,
        ancestor_size_vbytes: limits.ancestor_size_vbytes,
        descendant_count: limits.descendant_count + 1,
        descendant_size_vbytes: limits.descendant_size_vbytes
            + i64::from(EXTRA_DESCENDANT_TX_SIZE_LIMIT),
    };
    state.calculate_mem_pool_ancestors(entry, &carve_out, true).map_err(|_| too_long())
}

/// `PreChecks`: every check before the script checks, in Core's order, against this turn's
/// state, reading the inputs through `package`'s coins first. Its side effects stand whatever it
/// returns: the IS-waiting branch's removals, the double-spend event, and the inputs listed in
/// `coins_to_uncache`.
///
/// # Errors
///
/// A fault without a verdict (a coins or deployment fault, or Core's assert), or
/// [`AdmissionFault::Unavailable`] for a missing MNHF row.
// Core: validation.cpp:839-1136 at v24.0.2
#[allow(clippy::too_many_arguments)]
pub fn pre_checks(
    state: &mut PoolState,
    chain: &mut dyn AcceptChain,
    signals: &mut dyn AcceptSignals,
    tx: &TxRef,
    record: &StatelessRecord,
    args: &AcceptArgs,
    package: &PackageCoins,
    coins_to_uncache: &mut Vec<OutPoint>,
) -> Result<Result<Workspace, TxValidationError>, AdmissionFault> {
    macro_rules! reject {
        ($verdict:expr) => {
            match $verdict {
                Ok(value) => value,
                Err(reject) => return Ok(Err(reject)),
            }
        };
    }
    if !record.is_for(tx) {
        return Err(Fatal::assert("PreChecks: a stateless record of another instance").into());
    }
    let hash = tx.txid();
    let options = *state.options();
    let tip = chain.tip().ok_or_else(|| Fatal::assert("PreChecks without an active tip"))?;
    let tip_header = chain.ancestry().header(tip);

    reject!(record.check_transaction.clone());
    if let Err(reject) = contextual_at_tip(&*chain, tip, tx)? {
        tracing::error!("PreChecks: ContextualCheckTransaction: {hash}, {reject}");
        return Ok(Err(reject));
    }
    if record.quorum_commitment {
        return Ok(Err(invalid(TxValidationResult::Consensus, "qc-not-allowed")));
    }
    if record.coinbase {
        return Ok(Err(invalid(TxValidationResult::Consensus, "coinbase")));
    }
    if options.require_standard {
        reject!(record.standard.map_err(|reason| invalid(TxValidationResult::NotStandard, reason)));
        reject!(
            record
                .standard_special
                .map_err(|reason| invalid(TxValidationResult::NotStandard, reason))
        );
    }
    if record.size_small {
        return Ok(Err(invalid(TxValidationResult::NotStandard, "tx-size-small")));
    }
    if !check_final_tx_at_tip(&*chain, tx)? {
        return Ok(Err(invalid(TxValidationResult::PrematureSpend, "non-final")));
    }
    if state.exists(&hash) {
        return Ok(Err(invalid(TxValidationResult::Conflict, "txn-already-in-mempool")));
    }

    if let Some(conflict) = state.is_hooks().conflicting_lock(tx) {
        // Mempool only: the txindex half of GetTransaction joins with IX1 (plan Q10).
        if let Some(conflicting) = state.get(&conflict.txid) {
            announce_removals(state, signals);
            signals.double_spend_attempt(tx.clone(), conflicting);
        }
        tracing::info!(
            "ERROR: AcceptToMemoryPool : Transaction {hash} conflicts with locked TX {}",
            conflict.txid
        );
        return Ok(Err(invalid(TxValidationResult::ConflictLock, "tx-txlock-conflict")));
    }
    if state.is_hooks().is_waiting_for_tx(&hash) {
        state.remove_conflicts(tx);
        state.remove_pro_tx_conflicts(tx, chain.mn_list());
        state.remove_asset_unlock_conflicts(tx);
    } else if tx.inputs().iter().any(|input| state.get_conflict_tx(&input.prevout()).is_some()) {
        // Transaction conflicts with mempool and RBF doesn't exist in Dash
        return Ok(Err(invalid(TxValidationResult::Conflict, "txn-mempool-conflict")));
    }

    reject!(check_unlock_index_unmined(tx, chain.credit_pool().map(|pool| &**pool)));
    let unlock = tx.is_platform_transfer().then(|| asset_unlock_payload(tx)).flatten();
    if let Some(unlock) = unlock {
        for other in state.asset_unlock_txids_by_index(unlock.index()) {
            let other_height = state
                .get(&other)
                .and_then(|other| asset_unlock_payload(&other).map(|p| p.requested_height()));
            if other_height.is_none_or(|held| unlock.requested_height() <= held) {
                return Ok(Err(invalid(
                    TxValidationResult::Conflict,
                    "assetunlock-stale-instance",
                )));
            }
        }
    }

    let view = reject!(with_coins(&mut *chain, |coins| {
        fetch_inputs(tx, package, &*state, coins, tip_header.hash, coins_to_uncache)
    })?);

    let lock_points = match calculate_prev_heights(tx, &mut |o| Ok(view.get(o).map(Coin::height)))?
    {
        Some(heights) => Some(calculate_lock_points_at_tip(chain.params(), &*chain, tx, heights)?),
        None => None,
    };
    let lock_points = match lock_points {
        Some(lp) if check_sequence_locks_at_tip(chain.params(), &*chain, &lp)? => lp,
        _ => return Ok(Err(invalid(TxValidationResult::PrematureSpend, "non-BIP68-final"))),
    };

    let base_fees = reject!(check_tx_inputs_at_tip(tx, &view, tip_header.height));
    let v24 = chain.deployment_active_after(Deployment::Versionbits(DeploymentPos::V24))?;
    reject!(check_shared_collateral(tx, &view, v24)?);
    if options.require_standard && !are_inputs_standard(tx, &view).unwrap_or_else(|e| match e {}) {
        return Ok(Err(invalid(
            TxValidationResult::InputsNotStandard,
            "bad-txns-nonstandard-inputs",
        )));
    }

    let sigops = sigop_count(tx, &view)?;
    let mut modified_fees = base_fees;
    state.apply_delta(&hash, &mut modified_fees);
    let spends_coinbase =
        tx.inputs().iter().any(|input| view.get(&input.prevout()).is_some_and(Coin::is_coinbase));
    let entry = state.new_entry(
        tx.clone(),
        EntryMetadata {
            fee: base_fees,
            time: args.accept_time,
            height: tip_header.height,
            spends_coinbase,
            sigops: i64::from(sigops),
            lock_points,
        },
    );
    let vsize = entry.vsize() as i64;

    if sigops > MAX_STANDARD_TX_SIGOPS {
        return Ok(Err(invalid(TxValidationResult::NotStandard, "bad-txns-too-many-sigops")
            .with_debug(sigops.to_string())));
    }
    // Checking of fee for MNHF_SIGNAL should be skipped: mnhf does not have inputs, outputs, or
    // fee
    let mnhf = tx.is_special_tx_version() && tx.tx_type() == TxType::MnhfSignal;
    if !mnhf && !args.bypass_limits {
        let min_relay_fee = get_fee(options.min_relay_feerate, vsize as u32);
        if modified_fees < min_relay_fee {
            return Ok(Err(invalid(TxValidationResult::MempoolPolicy, "min relay fee not met")
                .with_debug(format!("{modified_fees} < {min_relay_fee}"))));
        }
        if !args.package_feerates {
            reject!(check_fee_rate(state, vsize, modified_fees));
        }
    }

    let ancestors = reject!(ancestors_with_carve_out(state, &entry, vsize));

    // check special TXs after all the other checks. If we'd do this before the other checks, we
    // might end up DoS scoring a node for non-critical errors, e.g. duplicate keys because a TX
    // is received that was already mined
    let rules = chain.special_tx_rules()?;
    let spacing = chain.params().consensus.pow_target_spacing;
    reject!(check_special_tx_for_mempool(&mut *chain, tx, rules, options.expiry_secs, spacing)?);

    let list = chain.mn_list();
    let ancestor_list: Vec<TxHash> = ancestors.iter().copied().collect();
    if state.exists_provider_tx_conflict(tx, list)
        || state.is_unorderable_service_update(tx, &ancestor_list, list)
    {
        return Ok(Err(invalid(TxValidationResult::Conflict, "protx-dup")));
    }
    if state.exists_provider_tx_cross_scheme_conflict(tx, list) {
        return Ok(Err(invalid(TxValidationResult::Conflict, "protx-dup")));
    }

    Ok(Ok(Workspace {
        entry,
        ancestors,
        view,
        vsize,
        base_fees,
        modified_fees,
    }))
}
