//! The mempool's special-transaction calls: `CheckSpecialTxForMempool` with its expired asset
//! unlock window retry, `TryAssetUnlockRefresh`'s `CheckSpecialTx` at the tip, and `PreChecks`'
//! credit-pool mined-index check.

use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_consensus::{BlockId, Tx};
use dash_dml::creditpool::PoolState as CreditPoolState;
use dash_dml::specialtx::asset::HEIGHT_DIFF_EXPIRING;
use dash_dml::specialtx::reasons::BAD_ASSETUNLOCK_TOO_OLD_QUORUM;
use dash_primitives::{AssetUnlock, SpecialPayload};
use dashd_chainstate::chain::{Fatal, TipFacts};
use dashd_chainstate::connect::SpecialTxRules;

/// What the mempool's special-transaction checks read of V: the active chain, and Core's
/// `CChainstateHelper::special_tx->CheckSpecialTx(tx, pindex, rules, CoinsTip(),
/// /*check_sigs=*/true, state)`.
pub trait SpecialTxChain: TipFacts {
    /// `CheckSpecialTx` with `pindex` as `pindexPrev`, an active-chain block (the tip, or a block
    /// of an expired asset unlock's window), over `CoinsTip()` and with signatures checked;
    /// `rules` are the tip's. The quorum lookups are Core's mempool overloads (D21).
    ///
    /// # Errors
    ///
    /// A fault without a verdict: Core's abort or assert.
    fn check_special_tx(
        &mut self,
        tx: &Tx,
        pindex: BlockId,
        rules: SpecialTxRules,
    ) -> Result<Result<(), TxValidationError>, Fatal>;
}

/// `GetTxPayload<CAssetUnlockPayload>(tx)`: the payload when it decodes completely.
pub(crate) fn asset_unlock_payload(tx: &Tx) -> Option<&AssetUnlock> {
    match tx.payload()?.ok().filter(|p| p.is_complete())?.payload() {
        SpecialPayload::AssetUnlock(p) => Some(p),
        _ => None,
    }
}

fn active_tip(chain: &dyn SpecialTxChain) -> Result<BlockId, Fatal> {
    chain.tip().ok_or_else(|| Fatal::assert("CheckSpecialTxForMempool without an active tip"))
}

/// `chain[height]`, inside the active chain by the caller's window bounds.
fn active_at(chain: &dyn SpecialTxChain, tip: BlockId, height: i64) -> Result<BlockId, Fatal> {
    u32::try_from(height)
        .ok()
        .and_then(|h| chain.ancestry().ancestor(tip, h))
        .ok_or_else(|| Fatal::assert(format!("CheckSpecialTxForMempool: no chain[{height}]")))
}

/// `TryAssetUnlockRefresh`'s full check of a fresher instance: `CheckSpecialTx` at the tip with
/// the tip's rules.
///
/// # Errors
///
/// As [`SpecialTxChain::check_special_tx`]; no active tip.
// Core: validation.cpp:1408-1416 at v24.0.0
pub fn check_special_tx_at_tip(
    chain: &mut dyn SpecialTxChain,
    tx: &Tx,
    rules: SpecialTxRules,
) -> Result<Result<(), TxValidationError>, Fatal> {
    let tip = active_tip(chain)?;
    chain.check_special_tx(tx, tip, rules)
}

/// `CheckSpecialTxForMempool`: `CheckSpecialTx` at the tip, except for a stable-txid asset unlock
/// whose window `[requestedHeight, requestedHeight + HEIGHT_DIFF_EXPIRING - 1]` ended below the
/// tip and began no earlier than `expiry_secs / pow_target_spacing` blocks before it (the pool's
/// `m_expiry`). That one is checked at the window's first block and, only when that fails with
/// `bad-assetunlock-too-old-quorum`, again from a fresh state at its last block.
///
/// # Errors
///
/// As [`SpecialTxChain::check_special_tx`]; no active tip, a window block outside the chain
/// (Core asserts), or a zero `pow_target_spacing` (Core divides by it).
// Core: validation.cpp:813-837 at v24.0.0
pub fn check_special_tx_for_mempool(
    chain: &mut dyn SpecialTxChain,
    tx: &Tx,
    rules: SpecialTxRules,
    expiry_secs: i64,
    pow_target_spacing: i64,
) -> Result<Result<(), TxValidationError>, Fatal> {
    let tip = active_tip(chain)?;
    let payload = if tx.is_asset_unlock_with_stable_txid() {
        asset_unlock_payload(tx)
    } else {
        None
    };
    let Some(payload) = payload else {
        return chain.check_special_tx(tx, tip, rules);
    };

    let tip_height = i64::from(chain.ancestry().header(tip).height);
    let first_height = i64::from(payload.requested_height());
    let last_height = first_height + i64::from(HEIGHT_DIFF_EXPIRING) - 1;
    let spacing = (pow_target_spacing != 0)
        .then_some(pow_target_spacing)
        .ok_or_else(|| Fatal::assert("CheckSpecialTxForMempool: PowTargetSpacing() is 0"))?;
    let oldest_height = tip_height - expiry_secs / spacing;
    if last_height >= tip_height || first_height < oldest_height {
        return chain.check_special_tx(tx, tip, rules);
    }

    // Expired: admit it if it was minable at some height of its window. A quorum signing it is
    // active at the window's first height, or was mined after it and is active at the last one.
    let first = active_at(chain, tip, first_height)?;
    match chain.check_special_tx(tx, first, rules)? {
        Ok(()) => return Ok(Ok(())),
        Err(reject) if reject.reject_reason() != BAD_ASSETUNLOCK_TOO_OLD_QUORUM => {
            return Ok(Err(reject));
        }
        Err(_) => {}
    }
    let last = active_at(chain, tip, last_height)?;
    chain.check_special_tx(tx, last, rules)
}

/// `PreChecks`' mined-index check of an asset unlock against `GetCreditPool(tip)`: the pool's
/// `indexes.Contains(index)` is `txn-already-known` (`TX_CONFLICT`); a pool whose construction
/// failed is Core's caught exception, `failed-getcreditpool` (`TX_BAD_SPECIAL`). `credit_pool` is
/// V's `ChainView.credit_pool`, `None` without V20, where Core's pool is empty. A transaction
/// without a decodable asset-unlock payload is not checked.
///
/// Core's `EvoDbInconsistencyError` branch (`state.Error`, `v:945-947`) has no producer here: V
/// publishes the tip's pool already built, and an evo store fault in that build stops V.
// Core: validation.cpp:925-952 at v24.0.0; evo/creditpool.cpp:124, :256 GetCreditPool
pub fn check_unlock_index_unmined(
    tx: &Tx,
    credit_pool: Option<&CreditPoolState>,
) -> Result<(), TxValidationError> {
    let payload = if tx.is_platform_transfer() {
        asset_unlock_payload(tx)
    } else {
        None
    };
    let (Some(payload), Some(pool)) = (payload, credit_pool) else {
        return Ok(());
    };
    match pool {
        Ok(pool) if pool.indexes.contains(payload.index()) => {
            Err(TxValidationError::new(TxValidationResult::Conflict, "txn-already-known"))
        }
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::info!("PreChecks -- GetCreditPool failed: {e:?}");
            Err(TxValidationError::new(TxValidationResult::BadSpecial, "failed-getcreditpool"))
        }
    }
}
