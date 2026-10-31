//! Core's verdict on a `qsigrec` (LQ3a): a recovered signature checked against the quorum it
//! names, which must be one of the type's recent quorums at the tip (`IsQuorumActive`).
//!
//! Core's handling spans the message handler (the type check, `llmq/net_signing.cpp` v24:47-62),
//! the signing manager's intake (`VerifyAndProcessRecoveredSig`, `llmq/signing.cpp` v24:422-475)
//! and its worker (`CollectPendingRecoveredSigsToVerify`, :488-571, and
//! `NetSigning::ProcessPendingRecoveredSigs`, `llmq/net_signing.cpp` v24:220-281). This module
//! gives the verdict those steps reach for one message, in their order, with the signature
//! checked on its own (individual verification; possible security note: see manager).
//!
//! What depends on the node's state is not here (LK1): Core's skip of a recovered signature it
//! already holds or is already reconstructing, the pending queues and their caps (a full queue
//! drops a message unread), the database, its cleanup, the listeners, and what
//! `ProcessRecoveredSig` does with a signature that verifies (it drops one whose request id
//! already has a recovered signature). The view is one snapshot: it stands for both of Core's
//! reads of the quorum (at intake and again in the worker), which differ only when the chain moves
//! between them.

use std::convert::Infallible;
use std::fmt;

use dash_params::ChainParams;
use dash_primitives::{LlmqType, QuorumHash};

use super::wire::{RecoveredSig, SigRead, read_lazy_sig};
use super::{
    LockError, LockReject, LockSchemes, QuorumScanView, QuorumStatus, ScanEntry, ScanLookup,
    verify_with,
};
use crate::llmq::VerifiedQuorum;

/// Core's verdict on a recovered signature.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "returned and matched at once: a box would allocate per message"
)]
pub enum RecSigVerdict {
    /// The named quorum's key verifies the signature: Core hands it to `ProcessRecoveredSig`
    /// (whether that stores it depends on the node's state, LK1).
    Accept(VerifiedQuorum),
    /// Core drops the message without penalty.
    Ignore(RecSigIgnore),
    /// Core bans the sender (`BanNode(from, mark_shares_banned = false)`).
    Ban(RecSigBan),
}

/// Why Core drops a recovered signature without penalty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecSigIgnore {
    /// The named quorum is not among the type's recent quorums at the tip (`!IsQuorumActive`,
    /// `llmq/signing.cpp` v24:429, :556).
    ///
    /// Core's other drop before the check, `GetQuorum` null (:440, :550), happens only when the
    /// chain moves between Core's reads: at one tip every quorum `ScanQuorums` lists is one
    /// `GetQuorum` builds (`llmq/quorumsman.cpp` v24:264-268). A view that lists the quorum as
    /// active and has no quorum for it gives [`RecSigError::QuorumUnavailable`].
    InactiveQuorum,
}

/// Why Core bans the sender of a recovered signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecSigBan {
    /// The LLMQ type is not configured (`llmq/net_signing.cpp` v24:56-58).
    UnknownLlmqType,
    /// The signature's bytes do not read under the scheme (`!sig.Get().IsValid()`, :245).
    MalformedSignature,
    /// The named quorum's key rejects the signature.
    InvalidSignature,
}

/// No verdict on a recovered signature: what the view holds does not decide it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecSigError<E> {
    /// The view does not hold the type's recent quorums at the tip, holds no quorum for an active
    /// hash, or holds the named quorum not [`QuorumStatus::Verified`] (or another quorum). Not
    /// Core's answer: the signature may be valid.
    QuorumUnavailable,
    /// The signature check depends on arithmetic BS12 does not reproduce.
    BlsUnsupported,
    /// The view failed.
    View(E),
}

impl<E: fmt::Display> fmt::Display for RecSigError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecSigError::QuorumUnavailable => f.write_str("recovered sig: quorum unavailable"),
            RecSigError::BlsUnsupported => {
                f.write_str("recovered sig: signature outside the BLS domain")
            }
            RecSigError::View(e) => write!(f, "recovered sig view: {e}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for RecSigError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RecSigError::View(e) => Some(e),
            _ => None,
        }
    }
}

/// Core's `IsQuorumActive(type, quorumHash)`: whether the quorum is one of the type's newest
/// `keepOldConnections` quorums at the tip (`ScanQuorums(type, keepOldConnections)`), by hash. An
/// unconfigured type has none. A negative `keepOldConnections` (no network has one) is Core's
/// `size_t` conversion of it: every quorum.
// Core: llmq/signing.cpp:701-712 IsQuorumActive
pub fn is_quorum_active<V: QuorumScanView + ?Sized>(
    params: &ChainParams,
    view: &V,
    llmq_type: LlmqType,
    quorum_hash: &QuorumHash,
) -> Result<bool, RecSigError<V::Error>> {
    let Some(llmq) = params.consensus.llmq(llmq_type) else {
        return Ok(false);
    };
    let count = usize::try_from(llmq.keep_old_connections).unwrap_or(usize::MAX);
    match view.scan_at_tip(llmq_type, count).map_err(RecSigError::View)? {
        ScanLookup::Held(scan) => {
            Ok(scan.entries.iter().any(|entry| entry.quorum_hash() == *quorum_hash))
        }
        ScanLookup::Unavailable | ScanLookup::OutsideHorizon => Err(RecSigError::QuorumUnavailable),
    }
}

/// Core's check before it serves a recovered signature it holds (`GetRecoveredSigForGetData`):
/// only one whose quorum is still active. Whether the node holds it is the node's (LK1).
// Core: llmq/signing.cpp:410-420 CSigningManager::GetRecoveredSigForGetData
pub fn serves_recovered_sig<V: QuorumScanView + ?Sized>(
    params: &ChainParams,
    view: &V,
    recovered: &RecoveredSig,
) -> Result<bool, RecSigError<V::Error>> {
    is_quorum_active(params, view, recovered.llmq_type, &recovered.quorum_hash)
}

/// Core's verdict on a `qsigrec`: the type configured, else a ban; the named quorum active, else
/// an ignore; the signature readable under `schemes.read`, else a ban; then the named quorum's key
/// over the sign hash under `schemes.check`, a ban when it rejects. A named quorum that is not
/// [`QuorumStatus::Verified`] gives no verdict on a readable signature
/// ([`RecSigError::QuorumUnavailable`]); the earlier steps do not read the key.
///
/// `schemes.read` is the flag when the message was read: the lazy signature captures it when it
/// is built, just before the read, and decodes under it (`bls/bls.h` v24:410, :477-481, :500).
/// `schemes.check` is the flag when the signing worker checks the signature (`bls/bls.cpp`
/// v24:335; :309-311 is the individual check). Across the V19 switch a legacy signature read
/// before it is checked under the basic scheme and banned (LQ3a D18; [`LockSchemes`]).
// Core: llmq/net_signing.cpp:47-62, :220-281; llmq/signing.cpp:422-475, :488-571
pub fn verify_qsigrec<V: QuorumScanView + ?Sized>(
    params: &ChainParams,
    view: &V,
    schemes: LockSchemes,
    recovered: &RecoveredSig,
) -> Result<RecSigVerdict, RecSigError<V::Error>> {
    let llmq_type = recovered.llmq_type;
    if params.consensus.llmq(llmq_type).is_none() {
        return Ok(RecSigVerdict::Ban(RecSigBan::UnknownLlmqType));
    }
    if !is_quorum_active(params, view, llmq_type, &recovered.quorum_hash)? {
        return Ok(RecSigVerdict::Ignore(RecSigIgnore::InactiveQuorum));
    }
    let Some(status) = view.quorum(llmq_type, &recovered.quorum_hash).map_err(RecSigError::View)?
    else {
        return Err(RecSigError::QuorumUnavailable);
    };
    let SigRead::Valid(sig) = read_lazy_sig(&recovered.sig, schemes.read) else {
        return Ok(RecSigVerdict::Ban(RecSigBan::MalformedSignature));
    };
    let QuorumStatus::Verified(quorum) = status else {
        return Err(RecSigError::QuorumUnavailable);
    };
    // The view's answer must be the quorum the message names.
    if quorum.quorum_hash() != &recovered.quorum_hash {
        return Err(RecSigError::QuorumUnavailable);
    }
    match verify_with::<Infallible>(
        llmq_type,
        &quorum,
        &recovered.id,
        &recovered.msg_hash,
        &sig,
        schemes.check,
    ) {
        Ok(()) => Ok(RecSigVerdict::Accept(quorum)),
        Err(LockError::Rejected(LockReject::InvalidSignature)) => {
            Ok(RecSigVerdict::Ban(RecSigBan::InvalidSignature))
        }
        Err(LockError::BlsUnsupported) => Err(RecSigError::BlsUnsupported),
        // Another type's quorum; `verify_with` gives none of the others.
        Err(LockError::Rejected(
            LockReject::QuorumUnavailable
            | LockReject::NoQuorum
            | LockReject::OutsideHorizon
            | LockReject::MalformedSignature
            | LockReject::Malformed
            | LockReject::UnknownCycle
            | LockReject::InvalidCycle,
        ))
        | Err(LockError::UnknownLlmqType(_)) => Err(RecSigError::QuorumUnavailable),
        Err(LockError::View(never)) => match never {},
    }
}
