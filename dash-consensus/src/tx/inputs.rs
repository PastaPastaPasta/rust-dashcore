//! The checks that read the coins a transaction spends (CS5b; FR-CON-08): Core's
//! `Consensus::CheckTxInputs` and the sigop counts that need the spent outputs
//! (`GetP2SHSigOpCount`, `GetTransactionSigOpCount`).
//!
//! Both take any [`CoinsView`]. A read error comes back as [`ViewOr::View`], never as a
//! rejection (CH2 §2.4.4). Core always calls them through a `CCoinsViewCache`, whose `HaveCoin`
//! is "the coin reads", so they ask [`CoinsView::coin`] and never [`CoinsView::have_coin`].
//!
//! **Order.** Core's `ConnectBlock` runs the special-transaction checks
//! (`ProcessSpecialTxsInBlock`) before its per-transaction loop, and those reject an asset
//! unlock with any input (`bad-assetunlocktx-have-input`). [`check_tx_inputs`] accepts an asset
//! unlock without reading its inputs, as Core does, so a block's caller must run those checks
//! first. The mempool (`PreChecks`) runs its special-transaction checks last, but first rejects
//! any transaction with an input the view lacks (`bad-txns-inputs-missingorspent`), so a coin is
//! never missing there either. The sigop counts never panic on a missing coin: they return
//! [`MissingCoin`] where Core would assert.

use dash_primitives::SpecialPayload;

use crate::coins::{CoinsView, ViewOr};
use crate::script::parse::is_pay_to_script_hash;
use crate::script::{ScriptFlags, script_sigop_count_with_sig};
use crate::validation::{TxValidationError, TxValidationResult};
use crate::{Amount, Tx, format_money, money_range};

/// `COINBASE_MATURITY`: the depth a coinbase output needs before it can be spent.
// Core: consensus/consensus.h:24
pub const COINBASE_MATURITY: i64 = 100;

/// A sigop count found no coin for input `input`, where Core asserts `!coin.IsSpent()` on
/// `AccessCoin`'s answer: the caller broke the order the module describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingCoin {
    pub input: usize,
}

/// Core's `Consensus::CheckTxInputs` for `tx` in a block at `spend_height` (the mempool passes
/// the tip's height plus one): the fee when the inputs are available, mature and cover the
/// outputs.
///
/// An asset unlock's fee is its payload's, and its inputs are not read: in a block, the
/// special-transaction checks, which Core runs first, reject an asset unlock with inputs (see the
/// module's order).
///
/// Preconditions for any other transaction, as in Core, where `ConnectBlock` and the mempool run
/// `CheckTransaction` first:
/// - every output value and their sum are in the money range (`GetValueOut` throws otherwise);
/// - `tx` is not a coinbase (`HaveInputs` skips a coinbase's inputs, and Core then asserts on
///   the coin of its null prevout).
///
/// # Panics
///
/// When a precondition does not hold.
///
/// `spend_height` is Core's `int`: heights from 2^31 have no Core counterpart.
// Core: consensus/tx_verify.cpp:163 Consensus::CheckTxInputs
pub fn check_tx_inputs<V: CoinsView + ?Sized>(
    tx: &Tx,
    view: &V,
    spend_height: u32,
) -> Result<Amount, ViewOr<V::Error, TxValidationError>> {
    if tx.is_platform_transfer() {
        return asset_unlock_fee(tx).map_err(ViewOr::Reject);
    }
    assert!(!tx.is_coinbase(), "CheckTxInputs: AccessCoin of a coinbase's null prevout");

    // One read per input. Core's `HaveInputs` reads every coin before its loop looks at any, so a
    // missing input outranks the loop's first reject; that reject is kept until the reads end.
    let mut value_in: Amount = 0;
    let mut reject = None;
    for input in tx.inputs() {
        let Some(coin) = view.coin(&input.prevout).map_err(ViewOr::View)? else {
            return Err(ViewOr::Reject(
                TxValidationError::new(
                    TxValidationResult::MissingInputs,
                    "bad-txns-inputs-missingorspent",
                )
                .with_debug("CheckTxInputs: inputs missing/spent"),
            ));
        };
        if reject.is_some() {
            continue;
        }

        // `nSpendHeight - coin.nHeight`: both promote to `int`.
        let depth = i64::from(spend_height) - i64::from(coin.height());
        if coin.is_coinbase() && depth < COINBASE_MATURITY {
            reject = Some(
                TxValidationError::new(
                    TxValidationResult::PrematureSpend,
                    "bad-txns-premature-spend-of-coinbase",
                )
                .with_debug(format!("tried to spend coinbase at depth {depth}")),
            );
            continue;
        }

        // Core adds before it range-checks; a sum that would overflow has an out-of-range value
        // in it, so wrapping gives Core's verdict.
        value_in = value_in.wrapping_add(coin.value());
        if !money_range(coin.value()) || !money_range(value_in) {
            reject = Some(TxValidationError::new(
                TxValidationResult::Consensus,
                "bad-txns-inputvalues-outofrange",
            ));
        }
    }
    if let Some(reject) = reject {
        return Err(ViewOr::Reject(reject));
    }

    let value_out = tx.value_out();
    if value_in < value_out {
        return Err(ViewOr::Reject(
            TxValidationError::new(TxValidationResult::Consensus, "bad-txns-in-belowout")
                .with_debug(format!(
                    "value in ({}) < value out ({})",
                    format_money(value_in),
                    format_money(value_out)
                )),
        ));
    }

    let fee = value_in - value_out;
    if !money_range(fee) {
        return Err(ViewOr::Reject(TxValidationError::new(
            TxValidationResult::Consensus,
            "bad-txns-fee-outofrange",
        )));
    }
    Ok(fee)
}

/// `GetAssetUnlockFee`: the payload's fee, which must be positive (`GetTxPayload` reads the
/// whole payload, whatever the BLS scheme: whether the signature decodes does not depend on it).
// Core: evo/assetlocktx.cpp:255 GetAssetUnlockFee
fn asset_unlock_fee(tx: &Tx) -> Result<Amount, TxValidationError> {
    let fee = asset_unlock_payload_fee(tx).ok_or_else(|| {
        TxValidationError::new(TxValidationResult::BadSpecial, "bad-assetunlocktx-payload")
    })?;
    if fee == 0 || !money_range(fee) {
        return Err(TxValidationError::new(
            TxValidationResult::Consensus,
            "bad-txns-assetunlock-fee-outofrange",
        ));
    }
    Ok(fee)
}

/// `GetTxPayload<CAssetUnlockPayload>(tx)->getFee()`: the fee of an asset unlock's payload, `None`
/// when the payload does not decode completely (`GetTxPayload` returns nothing).
pub(crate) fn asset_unlock_payload_fee(tx: &Tx) -> Option<Amount> {
    let decoded = tx.payload()?.ok().filter(|decoded| decoded.is_complete())?;
    // base-sdk decodes type 9 as an asset unlock or not at all.
    let SpecialPayload::AssetUnlock(payload) = &decoded.payload else {
        return None;
    };
    Some(Amount::from(payload.fee))
}

/// Core's `GetP2SHSigOpCount`: the accurate count of the redeem scripts of the inputs that spend
/// P2SH outputs; 0 for a coinbase. Inputs spending anything else add nothing.
///
/// Callers count after [`check_tx_inputs`] accepted, and an asset unlock only after the block's
/// special-transaction checks or the mempool's input-existence check (see the module's order):
/// then every coin is in the view. A coin the view lacks is [`MissingCoin`], where Core asserts.
// Core: consensus/tx_verify.cpp:132 GetP2SHSigOpCount
pub fn p2sh_sigop_count<V: CoinsView + ?Sized>(
    tx: &Tx,
    view: &V,
) -> Result<u32, ViewOr<V::Error, MissingCoin>> {
    if tx.is_coinbase() {
        return Ok(0);
    }
    let mut n: u32 = 0;
    let mut buf = [0; crate::coins::compress::MAX_SPECIAL_SCRIPT_LEN];
    for (index, input) in tx.inputs().iter().enumerate() {
        let Some(coin) = view.coin(&input.prevout).map_err(ViewOr::View)? else {
            return Err(ViewOr::Reject(MissingCoin {
                input: index,
            }));
        };
        let script = coin.script_pubkey();
        let prevout = script.expand(&mut buf);
        if is_pay_to_script_hash(prevout) {
            n = n.wrapping_add(script_sigop_count_with_sig(prevout, input.script_sig.as_bytes()));
        }
    }
    Ok(n)
}

/// Core's `GetTransactionSigOpCount`: [`Tx::legacy_sigop_count`], plus [`p2sh_sigop_count`] when
/// `flags` has P2SH and `tx` is not a coinbase.
///
/// Errors as [`p2sh_sigop_count`].
// Core: consensus/tx_verify.cpp:149 GetTransactionSigOpCount
pub fn transaction_sigop_count<V: CoinsView + ?Sized>(
    tx: &Tx,
    view: &V,
    flags: ScriptFlags,
) -> Result<u32, ViewOr<V::Error, MissingCoin>> {
    let n = tx.legacy_sigop_count();
    if tx.is_coinbase() {
        return Ok(n);
    }
    if flags.contains(ScriptFlags::P2SH) {
        return Ok(n.wrapping_add(p2sh_sigop_count(tx, view)?));
    }
    Ok(n)
}
