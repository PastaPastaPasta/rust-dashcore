//! Type 8, Core's `CheckAssetLockTx` (EV1 §3.12). EV1-5b-2 adds type 9.

use dash_consensus::money_range;
use dash_consensus::primitives::Tx;
use dash_consensus::validation::TxValidationError;
use dash_primitives::{SpecialPayload, TxType};

use super::bad_special;
use super::payload::PayloadSlot;
use super::reasons::*;
use super::trivial::{p2pkh_hash, p2sh_hash};

/// `CAssetLockPayload::INITIAL_VERSION`.
// Core: evo/assetlocktx.h:35
pub const ASSET_LOCK_INITIAL_VERSION: u8 = 1;
/// `CAssetLockPayload::CURRENT_VERSION`.
// Core: evo/assetlocktx.h:36
pub const ASSET_LOCK_CURRENT_VERSION: u8 = 2;

const OP_RETURN: u8 = 0x6a;

/// Core's `CheckAssetLockTx(tx, state, is_v24_active)`. `payload` is `tx`'s slot, and `v24` is
/// `GetSpecialTxRules(pindexPrev).v24`, which the caller binds (D21). All rejects are
/// `TX_BAD_SPECIAL`, in Core's order:
/// 1. the type is not 8 (`bad-assetlocktx-type`);
/// 2. per output whose script starts with `OP_RETURN`: a script other than `OP_RETURN OP_0`
///    (`bad-assetlocktx-non-empty-return`); a value of 0 or outside `MoneyRange`
///    (`bad-assetlocktx-opreturn-outofrange`); a second such output
///    (`bad-assetlocktx-multiple-return`);
/// 3. no such output (`bad-assetlocktx-no-return`);
/// 4. the payload does not decode to its last byte (`bad-assetlocktx-payload`);
/// 5. the version is 0 or above [`ASSET_LOCK_CURRENT_VERSION`] (`bad-assetlocktx-version`);
/// 6. without V24, a version above [`ASSET_LOCK_INITIAL_VERSION`] (`bad-assetlocktx-version-2`);
/// 7. no credit output (`bad-assetlocktx-emptycreditoutputs`);
/// 8. per credit output: a value of 0, or the value or the running sum outside `MoneyRange`
///    (`bad-assetlocktx-credit-outofrange`); at version 2 and above a script other than P2PKH or
///    P2SH (`bad-assetlocktx-script-pubkey`), below it one other than P2PKH
///    (`bad-assetlocktx-pubKeyHash`);
/// 9. the credit outputs' sum is not the `OP_RETURN` output's value
///    (`bad-assetlocktx-creditamount`).
// Core: evo/assetlocktx.cpp:29-89
pub fn check_asset_lock_tx(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    v24: bool,
) -> Result<(), TxValidationError> {
    // Core: evo/assetlocktx.cpp:31-33
    if tx.tx_type() != TxType::AssetLock {
        return bad_special(BAD_ASSETLOCKTX_TYPE);
    }

    // Core: evo/assetlocktx.cpp:35-47
    let mut return_amount = 0i64;
    for out in tx.outputs() {
        let script = out.script_pubkey.as_bytes();
        if script.first() != Some(&OP_RETURN) {
            continue;
        }
        if script != [OP_RETURN, 0] {
            return bad_special(BAD_ASSETLOCKTX_NON_EMPTY_RETURN);
        }
        if out.value == 0 || !money_range(out.value) {
            return bad_special(BAD_ASSETLOCKTX_OPRETURN_OUTOFRANGE);
        }
        if return_amount > 0 {
            return bad_special(BAD_ASSETLOCKTX_MULTIPLE_RETURN);
        }
        return_amount = out.value;
    }
    // Core: evo/assetlocktx.cpp:49
    if return_amount == 0 {
        return bad_special(BAD_ASSETLOCKTX_NO_RETURN);
    }

    // Core: evo/assetlocktx.cpp:51-54
    // A type-8 slot that decodes whole holds an asset-lock payload.
    let decoded = payload.and_then(Result::ok).filter(|d| d.is_complete()).map(|d| &d.payload);
    let Some(SpecialPayload::AssetLock(lock)) = decoded else {
        return bad_special(BAD_ASSETLOCKTX_PAYLOAD);
    };

    // Core: evo/assetlocktx.cpp:56-61
    if lock.version == 0 || lock.version > ASSET_LOCK_CURRENT_VERSION {
        return bad_special(BAD_ASSETLOCKTX_VERSION);
    }
    if !v24 && lock.version > ASSET_LOCK_INITIAL_VERSION {
        return bad_special(BAD_ASSETLOCKTX_VERSION_2);
    }

    // Core: evo/assetlocktx.cpp:63-65
    if lock.credit_outputs.is_empty() {
        return bad_special(BAD_ASSETLOCKTX_EMPTYCREDITOUTPUTS);
    }

    // Core: evo/assetlocktx.cpp:67-83
    let mut credit_amount = 0i64;
    for out in &lock.credit_outputs {
        // Both terms are in `MoneyRange` when the sum is taken, so it cannot overflow.
        if out.value == 0 || !money_range(out.value) || !money_range(credit_amount + out.value) {
            return bad_special(BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE);
        }
        credit_amount += out.value;
        let script = out.script_pubkey.as_bytes();
        if lock.version >= 2 {
            if p2pkh_hash(script).is_none() && p2sh_hash(script).is_none() {
                return bad_special(BAD_ASSETLOCKTX_SCRIPT_PUBKEY);
            }
        } else if p2pkh_hash(script).is_none() {
            return bad_special(BAD_ASSETLOCKTX_PUBKEYHASH);
        }
    }
    // Core: evo/assetlocktx.cpp:84-86
    if credit_amount != return_amount {
        return bad_special(BAD_ASSETLOCKTX_CREDITAMOUNT);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
