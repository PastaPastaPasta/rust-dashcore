//! Type 7, Core's `CheckMNHFTx` (EV1 §3.12, EV1-5b-2).

use core::fmt::Debug;

use dash_consensus::deployments::VERSIONBITS_NUM_BITS;
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::Tx;
use dash_evo::locks::sign_hash;
use dash_primitives::{SpecialPayload, TxType};

use super::reasons::*;
use super::{CheckResult, SpecialTxCtx, SpecialTxFault, consensus};
use crate::blockprocessor::as_block_hash;
use crate::mnhf::is_valid_mn_activation;

/// `MNHFTxPayload::CURRENT_VERSION`.
// Core: evo/mnhftx.h:62
pub const MNHF_CURRENT_VERSION: u8 = 1;

/// Core's `CheckMNHFTx(blockman, qman[, chain], tx, pindexPrev, state)`. All rejects are
/// `TX_CONSENSUS`, in Core's order:
/// 1. not a special-transaction version, or not type 7 (`bad-mnhf-type`);
/// 2. the payload does not decode to its last byte (`bad-mnhf-payload`);
/// 3. `IsTriviallyValid`: version 0 or above [`MNHF_CURRENT_VERSION`] (`bad-mnhf-version`), a
///    version bit from `VERSIONBITS_NUM_BITS` on (`bad-mnhf-nbit-out-of-bounds`);
/// 4. `IsValidMNActivation(bit, pindexPrev->GetMedianTimePast())` fails (`bad-mnhf-non-ehf`);
/// 5. the quorum hash names no known block, or one off the chain at `pindexPrev`
///    (`bad-mnhf-quorum-hash`);
/// 6. `GetQuorum(llmqTypeMnhf, quorumHash)` has none (`bad-mnhf-missing-quorum`);
/// 7. the signature does not verify under the quorum's key over `SignHash(llmqTypeMnhf,
///    quorumHash, GetRequestId(), msgHash)`, where `msgHash` is the txid of the transaction with
///    a null signature (`bad-mnhf-invalid`).
///
/// The signature verifies whatever `check_sigs` says (Core passes no flag). Which `GetQuorum`
/// overload runs (with the active chain or without) is the binding's ([`super::QuorumSeams`]).
// Core: evo/mnhftx.cpp:94-174
pub(super) fn check_mnhf_tx<E: Debug>(
    tx: &Tx,
    payload: super::PayloadSlot<'_>,
    ctx: &mut SpecialTxCtx<'_, E>,
) -> CheckResult<E> {
    // Core: evo/mnhftx.cpp:110-112
    if !tx.is_special_tx_version() || tx.tx_type() != TxType::MnhfSignal {
        return Ok(consensus(BAD_MNHF_TYPE));
    }
    // Core: evo/mnhftx.cpp:114-118
    let decoded = payload.and_then(Result::ok).filter(|d| d.is_complete()).map(|d| &d.payload);
    let Some(SpecialPayload::MnhfSignal(mnhf)) = decoded else {
        return Ok(consensus(BAD_MNHF_PAYLOAD));
    };
    // Core: evo/mnhftx.cpp:94-104, 120-122
    if mnhf.version == 0 || mnhf.version > MNHF_CURRENT_VERSION {
        return Ok(consensus(BAD_MNHF_VERSION));
    }
    if i32::from(mnhf.version_bit) >= VERSIONBITS_NUM_BITS {
        return Ok(consensus(BAD_MNHF_NBIT_OUT_OF_BOUNDS));
    }
    // Core dereferences `pindexPrev` from here on; a null one has no verdict.
    let cons = ctx.evo.cons;
    let Some(prev) = cons.prev else {
        return Err(SpecialTxFault::NoPrev(TxType::MnhfSignal));
    };
    // Core: evo/mnhftx.cpp:124-126
    if !is_valid_mn_activation(cons.params, mnhf.version_bit, cons.median_time_past) {
        return Ok(consensus(BAD_MNHF_NON_EHF));
    }
    let scheme = ctx.verify_scheme();
    let Some(q) = ctx.quorum.as_mut() else {
        return Err(SpecialTxFault::Unbound(TxType::MnhfSignal));
    };
    // Core: evo/mnhftx.cpp:128-136
    let Some((quorum_block, quorum_height)) = q.index.lookup(&as_block_hash(&mnhf.quorum_hash))
    else {
        return Ok(consensus(BAD_MNHF_QUORUM_HASH));
    };
    if cons.chain.ancestor(prev, quorum_height) != Some(quorum_block) {
        return Ok(consensus(BAD_MNHF_QUORUM_HASH));
    }
    // Core: evo/mnhftx.cpp:139-143
    let msg_hash = sha256d(&mnhf.sig_hash_bytes(tx.raw()));
    // Core: evo/mnhftx.cpp:145-149
    let llmq_type = cons.params.consensus.llmq_type_mnhf;
    let quorum = q
        .quorums
        .get_quorum(llmq_type, &mnhf.quorum_hash, &mut *q.effects)
        .map_err(SpecialTxFault::Quorum)?;
    let Some(quorum) = quorum else {
        return Ok(consensus(BAD_MNHF_MISSING_QUORUM));
    };
    // Core: evo/mnhftx.cpp:151-154
    let hash = sign_hash(llmq_type, &quorum.quorum_hash, mnhf.request_id().as_bytes(), &msg_hash);
    let valid = ctx
        .verifier
        .bls_verify_insecure(&mnhf.sig, &quorum.public_key, &hash, scheme)
        .map_err(|_| SpecialTxFault::VerifyUnavailable)?;
    if !valid {
        return Ok(consensus(BAD_MNHF_INVALID));
    }
    Ok(Ok(()))
}

#[cfg(test)]
pub(crate) mod tests;
