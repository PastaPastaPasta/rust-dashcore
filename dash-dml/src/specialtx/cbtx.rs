//! Type 5, Core's `CheckCbTx` (EV1 §3.10), and the two rejects its callers make before it
//! ([`cbtx_payload`]). The merkle-root and ChainLock checks of `ProcessSpecialTxsInBlock` are not
//! here (EV1-5, CH7a).
//!
//! Core makes the same calls on both paths, in the same order:
//! - `CheckSpecialTxInner`'s type-5 arm (`specialtxman.cpp:247-255`), as `TX_CONSENSUS`;
//! - `ProcessSpecialTxsInBlock`'s coinbase step (`specialtxman.cpp:876-892`), when
//!   `fCheckCbTxMerkleRoots` and `vtx[0]` is of type 5, with no DIP0003 gate (D18). There the
//!   rejects are `BLOCK_CONSENSUS` with the same reason.
//!
//! So a caller runs [`cbtx_payload`], then [`check_cbtx`] on the payload it returns.

use dash_consensus::ConsensusCtx;
use dash_consensus::ctx::Deployment;
use dash_consensus::primitives::Tx;
use dash_consensus::validation::TxValidationError;
use dash_evo::cbtx::{
    CBTX_VERSION_CLSIG_AND_BALANCE, CBTX_VERSION_INVALID, CBTX_VERSION_MERKLE_ROOT_ASSETUNLOCKS,
    CBTX_VERSION_MERKLE_ROOT_QUORUMS, CBTX_VERSION_UNKNOWN,
};
use dash_params::BuriedDeployment;
use dash_primitives::{CoinbaseCommitment, SpecialPayload};

use super::consensus;
use super::payload::PayloadSlot;
use super::reasons::{BAD_CBTX_HEIGHT, BAD_CBTX_INVALID, BAD_CBTX_PAYLOAD, BAD_CBTX_VERSION};

/// The rejects a caller makes before `CheckCbTx`, `TX_CONSENSUS` and in Core's order: `tx` is
/// not a coinbase (`bad-cbtx-invalid`); `payload`, `tx`'s slot, is not a `CCbTx` that
/// `GetTxPayload` reads to its last byte (`bad-cbtx-payload`). Otherwise the payload, for
/// [`check_cbtx`].
///
/// The caller has already found `tx` to be of type 5 by Core's `nType`, [`Tx::tx_type`], whatever
/// the transaction's version: Core's block path keys on `vtx[0]->nType` alone
/// (`specialtxman.cpp:876`). A version-1 or version-2 transaction with type bits 5 has no payload
/// field, so its slot is `None`; `GetTxPayload` reads an empty payload and fails, so that is
/// `bad-cbtx-payload`. A slot of any other type is `bad-cbtx-payload` too, as `GetTxPayload<CCbTx>`
/// gives `nullopt` for it.
// Core: evo/specialtxman.cpp:247-255, 876-892; evo/specialtx.h:17-35
pub fn cbtx_payload<'a>(
    tx: &Tx,
    payload: PayloadSlot<'a>,
) -> Result<&'a CoinbaseCommitment, TxValidationError> {
    if !tx.is_coinbase() {
        return consensus(BAD_CBTX_INVALID);
    }
    match payload {
        Some(Ok(decode)) if decode.is_complete() => match &decode.payload {
            SpecialPayload::CoinbaseCommitment(cbtx) => Ok(cbtx),
            _ => consensus(BAD_CBTX_PAYLOAD),
        },
        _ => consensus(BAD_CBTX_PAYLOAD),
    }
}

/// Core's `CheckCbTx(cbTx, pindexPrev, is_v24_active, state)`. `cons.prev` is `pindexPrev`
/// (`pindex->pprev` on the block path) and `cons.height` its height plus one; `v24` is
/// `GetSpecialTxRules(pindexPrev).v24`, which the caller binds (D21). All rejects are
/// `TX_CONSENSUS`, in Core's order:
/// 1. the version is `INVALID` (0) or `UNKNOWN` (5) and above: `bad-cbtx-version`;
///
/// then, unless `pindexPrev` is null (genesis):
/// 2. the height is not `pindexPrev`'s plus one: `bad-cbtx-height`;
/// 3. DIP0008 is active at `pindexPrev` and the version is below `MERKLE_ROOT_QUORUMS` (2):
///    `bad-cbtx-version`;
/// 4. V20 is active after `pindexPrev` and the version is below `CLSIG_AND_BALANCE` (3), or V20
///    is not and the version is 3 or above: `bad-cbtx-version`;
/// 5. V24 and V20 are both active and the version is below `MERKLE_ROOT_ASSETUNLOCKS` (4), or
///    they are not and the version is 4 or above: `bad-cbtx-version`.
///
/// No step reads DIP0003: below it the same rules hold (DEC-730).
// Core: evo/cbtx.cpp:24-55
pub fn check_cbtx(
    cbtx: &CoinbaseCommitment,
    cons: &ConsensusCtx<'_>,
    v24: bool,
) -> Result<(), TxValidationError> {
    // Core: evo/cbtx.cpp:26-28
    if cbtx.version == CBTX_VERSION_INVALID || cbtx.version >= CBTX_VERSION_UNKNOWN {
        return consensus(BAD_CBTX_VERSION);
    }
    // Core: evo/cbtx.cpp:30
    let Some(prev) = cons.prev else {
        return Ok(());
    };
    // Core: evo/cbtx.cpp:31-33
    if i64::from(cons.height) != i64::from(cbtx.height) {
        return consensus(BAD_CBTX_HEIGHT);
    }
    // Core: evo/cbtx.cpp:35-38
    let dip8 = cons.deployments.active_at(prev, Deployment::Buried(BuriedDeployment::Dip0008));
    if dip8 && cbtx.version < CBTX_VERSION_MERKLE_ROOT_QUORUMS {
        return consensus(BAD_CBTX_VERSION);
    }
    // Core: evo/cbtx.cpp:40-43
    let v20 = cons.deployments.active_after(cons.prev, Deployment::Buried(BuriedDeployment::V20));
    if (v20 && cbtx.version < CBTX_VERSION_CLSIG_AND_BALANCE)
        || (!v20 && cbtx.version >= CBTX_VERSION_CLSIG_AND_BALANCE)
    {
        return consensus(BAD_CBTX_VERSION);
    }
    // Core: evo/cbtx.cpp:47-51
    let requires_unlock_root = v24 && v20;
    if (requires_unlock_root && cbtx.version < CBTX_VERSION_MERKLE_ROOT_ASSETUNLOCKS)
        || (!requires_unlock_root && cbtx.version >= CBTX_VERSION_MERKLE_ROOT_ASSETUNLOCKS)
    {
        return consensus(BAD_CBTX_VERSION);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
