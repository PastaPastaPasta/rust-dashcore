//! Types 8 and 9, Core's `CheckAssetLockTx` and `CheckAssetUnlockTx` (EV1 §3.12).

use core::fmt::Debug;

use dash_consensus::hash::sha256d;
use dash_consensus::money_range;
use dash_consensus::primitives::Tx;
use dash_consensus::validation::TxValidationError;
use dash_evo::locks::sign_hash;
use dash_primitives::{SpecialPayload, TxType};

use super::payload::PayloadSlot;
use super::quorum::QuorumScan;
use super::reasons::*;
use super::trivial::{p2pkh_hash, p2sh_hash};
use super::{CheckResult, SpecialTxCtx, SpecialTxFault, bad_special, consensus};
use crate::blockprocessor::as_block_hash;

/// `CAssetLockPayload::INITIAL_VERSION`.
// Core: evo/assetlocktx.h:35
pub const ASSET_LOCK_INITIAL_VERSION: u8 = 1;
/// `CAssetLockPayload::CURRENT_VERSION`.
// Core: evo/assetlocktx.h:36
pub const ASSET_LOCK_CURRENT_VERSION: u8 = 2;

/// `CAssetUnlockPayload::INITIAL_VERSION`.
// Core: evo/assetlocktx.h:78
pub const ASSET_UNLOCK_INITIAL_VERSION: u8 = 1;
/// `CAssetUnlockPayload::CURRENT_VERSION`.
// Core: evo/assetlocktx.h:82
pub const ASSET_UNLOCK_CURRENT_VERSION: u8 = 2;
/// `CAssetUnlockPayload::MAXIMUM_WITHDRAWALS`.
// Core: evo/assetlocktx.h:86
pub const MAXIMUM_WITHDRAWALS: usize = 32;
/// `CAssetUnlockPayload::HEIGHT_DIFF_EXPIRING`.
// Core: evo/assetlocktx.h:163
pub const HEIGHT_DIFF_EXPIRING: u32 = 48;

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

/// Core's `CheckAssetUnlockTx(blockman, qman[, chain], tx, pindexPrev, indexes, is_v24_active,
/// state)` with `VerifyAssetUnlockSig`, in Core's order (class S is `TX_BAD_SPECIAL`, C
/// `TX_CONSENSUS`):
/// 1. S, the type is not 9 (`bad-assetunlocktx-type`);
/// 2. S, the transaction has inputs (`bad-assetunlocktx-have-input`);
/// 3. S, more than [`MAXIMUM_WITHDRAWALS`] outputs (`bad-assetunlocktx-too-many-outs`);
/// 4. S, the payload does not decode to its last byte (`bad-assetunlocktx-payload`);
/// 5. S, version 0 or above [`ASSET_UNLOCK_CURRENT_VERSION`] (`bad-assetunlocktx-version`);
/// 6. S, without V24 (`rules.v24`), a version above [`ASSET_UNLOCK_INITIAL_VERSION`]
///    (`bad-assetunlocktx-version-2`);
/// 7. C, the index is in [`SpecialTxCtx::unlock_indexes`], the credit pool's at `pindexPrev` on
///    the block path; the mempool path passes none (`bad-assetunlock-duplicated-index`);
/// 8. C, the quorum hash names no known block (`bad-assetunlock-quorum-hash`);
/// 9. S, the quorum hash is not among `ScanQuorums(llmqTypePlatform, pindexPrev,
///    signingActiveQuorumCount + 1)` (`bad-assetunlock-too-old-quorum`);
/// 10. S, `pindexPrev`'s height is below the requested height, or not below the requested
///     height plus [`HEIGHT_DIFF_EXPIRING`], wrapped as Core's `int` (`bad-assetunlock-too-late`);
/// 11. C, `GetQuorum(llmqTypePlatform, quorumHash)` has none
///     (`bad-assetunlock-quorum-internal-error`);
/// 12. C, the signature does not verify under the quorum's key over `SignHash(llmqTypePlatform,
///     quorumHash, requestId, msgHash)`, where `msgHash` is `SerializeHash` of the whole
///     transaction with a null signature, not its txid (`bad-assetunlock-not-verified`).
///
/// The signature verifies whatever `check_sigs` says (Core passes no flag), decoded under the
/// global scheme (`cons.bls`). Which quorum-manager overloads run (with the active chain or
/// without) is the binding's ([`super::QuorumSeams`]).
// Core: evo/assetlocktx.cpp:105-254; evo/assetlocktx.h:163-167
pub(super) fn check_asset_unlock_tx<E: Debug>(
    tx: &Tx,
    payload: PayloadSlot<'_>,
    ctx: &mut SpecialTxCtx<'_, E>,
) -> CheckResult<E> {
    // `pindexPrev` is a `gsl::not_null` parameter: a null one terminates at the call, before
    // any check.
    // Core: evo/assetlocktx.cpp:234-236; gsl/pointers.h:126-132
    let cons = ctx.evo.cons;
    let Some(prev) = cons.prev else {
        return Err(SpecialTxFault::NoPrev(TxType::AssetUnlock));
    };
    // Core: evo/assetlocktx.cpp:184-194
    if tx.tx_type() != TxType::AssetUnlock {
        return Ok(bad_special(BAD_ASSETUNLOCKTX_TYPE));
    }
    if !tx.inputs().is_empty() {
        return Ok(bad_special(BAD_ASSETUNLOCKTX_HAVE_INPUT));
    }
    if tx.outputs().len() > MAXIMUM_WITHDRAWALS {
        return Ok(bad_special(BAD_ASSETUNLOCKTX_TOO_MANY_OUTS));
    }
    // Core: evo/assetlocktx.cpp:201-204
    let decoded = payload.and_then(Result::ok).filter(|d| d.is_complete()).map(|d| &d.payload);
    let Some(SpecialPayload::AssetUnlock(unlock)) = decoded else {
        return Ok(bad_special(BAD_ASSETUNLOCKTX_PAYLOAD));
    };
    // Core: evo/assetlocktx.cpp:207-212
    if unlock.version == 0 || unlock.version > ASSET_UNLOCK_CURRENT_VERSION {
        return Ok(bad_special(BAD_ASSETUNLOCKTX_VERSION));
    }
    if !ctx.rules.v24 && unlock.version > ASSET_UNLOCK_INITIAL_VERSION {
        return Ok(bad_special(BAD_ASSETUNLOCKTX_VERSION_2));
    }
    // Core: evo/assetlocktx.cpp:214-216
    if ctx.unlock_indexes.is_some_and(|indexes| indexes.contains(unlock.index)) {
        return Ok(consensus(BAD_ASSETUNLOCK_DUPLICATED_INDEX));
    }
    let scheme = ctx.verify_scheme();
    let Some(q) = ctx.quorum.as_mut() else {
        return Err(SpecialTxFault::Unbound(TxType::AssetUnlock));
    };
    // Core: evo/assetlocktx.cpp:218-220
    if q.index.lookup(&as_block_hash(&unlock.quorum_hash)).is_none() {
        return Ok(consensus(BAD_ASSETUNLOCK_QUORUM_HASH));
    }
    // Core: evo/assetlocktx.cpp:222-229
    let msg_hash = sha256d(&unlock.sig_hash_bytes(tx.raw()));

    // Core: evo/assetlocktx.cpp:115-123
    let llmq_type = cons.params.consensus.llmq_type_platform;
    let Some(llmq) = cons.params.consensus.llmqs.iter().find(|p| p.llmq_type == llmq_type) else {
        // Core asserts the type is in the chain's table.
        return Err(SpecialTxFault::NoLlmq(llmq_type));
    };
    let to_scan = usize::try_from(llmq.signing_active_quorum_count + 1).unwrap_or(usize::MAX);
    let mut quorums = QuorumScan::default();
    q.quorums
        .scan_quorums(llmq_type, prev, to_scan, &mut quorums, &mut *q.effects)
        .map_err(SpecialTxFault::Quorum)?;
    // Core: evo/assetlocktx.cpp:125-130
    if !quorums.as_slice().contains(&unlock.quorum_hash) {
        return Ok(bad_special(BAD_ASSETUNLOCK_TOO_OLD_QUORUM));
    }
    if unlock_too_late(cons.height as i32 - 1, unlock.requested_height) {
        return Ok(bad_special(BAD_ASSETUNLOCK_TOO_LATE));
    }
    // Core: evo/assetlocktx.cpp:138-143
    let quorum = q
        .quorums
        .get_quorum(llmq_type, &unlock.quorum_hash, &mut *q.effects)
        .map_err(SpecialTxFault::Quorum)?;
    let Some(quorum) = quorum else {
        return Ok(consensus(BAD_ASSETUNLOCK_QUORUM_INTERNAL_ERROR));
    };
    // Core: evo/assetlocktx.cpp:145-152
    let hash = sign_hash(llmq_type, &quorum.quorum_hash, unlock.request_id().as_bytes(), &msg_hash);
    let sig = unlock.quorum_sig.get(cons.bls);
    let valid = ctx
        .verifier
        .bls_verify_insecure(&sig, &quorum.public_key, &hash, scheme)
        .map_err(|_| SpecialTxFault::VerifyUnavailable)?;
    if !valid {
        return Ok(consensus(BAD_ASSETUNLOCK_NOT_VERIFIED));
    }
    Ok(Ok(()))
}

/// `VerifyAssetUnlockSig`'s height window at `pindexTip` (`pindexPrev`): too late when its height
/// is below `requested_height`, or not below `getHeightToExpiry()`, the `uint32_t` sum
/// `requested_height + HEIGHT_DIFF_EXPIRING` returned as an `int` (wrapped).
// Core: evo/assetlocktx.cpp:132-136; evo/assetlocktx.h:164-167
pub(crate) fn unlock_too_late(prev_height: i32, requested_height: u32) -> bool {
    let expiry = requested_height.wrapping_add(HEIGHT_DIFF_EXPIRING) as i32;
    (prev_height as u32) < requested_height || prev_height >= expiry
}

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
pub(crate) mod unlock_tests;
