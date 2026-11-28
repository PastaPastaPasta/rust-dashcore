//! The recovered-signature message's request completion and cheap intake gates.

use std::sync::Arc;

use dash_evo::locks::wire::RecoveredSig;
use dash_params::ChainSetup;

use super::db::RecSigKv;
use super::manager::{
    MAX_PENDING_PER_PEER, MAX_PENDING_TOTAL, PendingSig, SigningError, SigningManager,
};
use crate::dkg::bls::BlsCtx;
use crate::sigenv::{PeerId, QuorumLookup};

/// Complete only this peer's announcement, even if the signature is unusable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestCompletion {
    pub peer: PeerId,
    pub hash: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntakeStatus {
    Unreadable,
    /// Emit a ban with `mark_shares_banned = false` after completing the request.
    BanUnknownType,
    InactiveQuorum,
    AlreadyKnown,
    QuorumUnavailable,
    PendingReconstruction,
    QueueFull,
    Queued,
}

/// Apply request completion even when `status` is a refusal or an error.
#[derive(Debug)]
pub struct IntakeOutcome<D, L> {
    pub request_completion: Option<RequestCompletion>,
    pub status: Result<IntakeStatus, SigningError<D, L>>,
}

/// Reads a QSIGREC: `Err(Unreadable)` without a completion, `Err(BanUnknownType)` for a type the
/// chain does not define, else the signature. The caller completes the request either way.
// Core: v24.0.2 src/llmq/net_signing.cpp:49-58 ProcessMessage (QSIGREC)
pub fn read_qsigrec(
    peer: PeerId,
    wire: &[u8],
    params: &ChainSetup,
) -> (Option<RequestCompletion>, Result<Arc<RecoveredSig>, IntakeStatus>) {
    let Some(sig) = RecoveredSig::decode(&mut &wire[..]) else {
        return (None, Err(IntakeStatus::Unreadable));
    };
    let completion = Some(RequestCompletion {
        peer,
        hash: sig.hash(),
    });
    if params.consensus.llmq(sig.llmq_type).is_none() {
        return (completion, Err(IntakeStatus::BanUnknownType));
    }
    (completion, Ok(Arc::new(sig)))
}

/// The gates Core runs before taking `cs_pending`: active quorum, then the stored hash through
/// `known`, then the quorum. `None` lets the signature through to [`SigningManager::enqueue`].
// Core: v24.0.2 src/llmq/signing.cpp:424-448 VerifyAndProcessRecoveredSig
pub fn intake_gates<L: QuorumLookup + ?Sized, D>(
    sig: &RecoveredSig,
    lookup: &L,
    known: impl FnOnce(&[u8; 32]) -> Result<bool, D>,
) -> Result<Option<IntakeStatus>, SigningError<D, L::Error>> {
    if !lookup.is_quorum_active(sig.llmq_type, &sig.quorum_hash).map_err(SigningError::Lookup)? {
        return Ok(Some(IntakeStatus::InactiveQuorum));
    }
    if known(&sig.hash()).map_err(SigningError::Storage)? {
        return Ok(Some(IntakeStatus::AlreadyKnown));
    }
    if lookup.get_quorum(sig.llmq_type, &sig.quorum_hash).map_err(SigningError::Lookup)?.is_none() {
        return Ok(Some(IntakeStatus::QuorumUnavailable));
    }
    Ok(None)
}

impl<K> SigningManager<K> {
    /// The `cs_pending` half: the reconstructed check and the queue caps.
    // Core: v24.0.2 src/llmq/signing.cpp:450-474 VerifyAndProcessRecoveredSig
    pub fn enqueue(&mut self, peer: PeerId, sig: Arc<RecoveredSig>, read: BlsCtx) -> IntakeStatus {
        if self.reconstructed.contains_key(&sig.hash()) {
            return IntakeStatus::PendingReconstruction;
        }
        if self.pending_count >= MAX_PENDING_TOTAL
            || self.pending.get(&peer).is_some_and(|q| q.len() >= MAX_PENDING_PER_PEER)
        {
            return IntakeStatus::QueueFull;
        }
        self.pending.entry(peer).or_default().push_back(PendingSig {
            sig,
            read,
        });
        self.pending_count += 1;
        IntakeStatus::Queued
    }
}

impl<K: RecSigKv> SigningManager<K> {
    // Core: v24.0.2 src/llmq/net_signing.cpp:47-62,307-319; src/net_processing.cpp:6930-6936 ProcessMessage
    pub fn on_qsigrec<L: QuorumLookup>(
        &mut self,
        peer: PeerId,
        wire: &[u8],
        params: &ChainSetup,
        lookup: &L,
        read: BlsCtx,
    ) -> IntakeOutcome<K::Error, L::Error> {
        let (request_completion, sig) = read_qsigrec(peer, wire, params);
        let status = match sig {
            Err(IntakeStatus::BanUnknownType) => {
                if peer != -1 {
                    self.remove_node(peer);
                }
                Ok(IntakeStatus::BanUnknownType)
            }
            Err(status) => Ok(status),
            Ok(sig) => match intake_gates(&sig, lookup, |h| self.db.has_for_hash(h)) {
                Ok(None) => Ok(self.enqueue(peer, sig, read)),
                Ok(Some(status)) => Ok(status),
                Err(e) => Err(e),
            },
        };
        IntakeOutcome {
            request_completion,
            status,
        }
    }
}
