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

impl<K: RecSigKv> SigningManager<K> {
    // Core: v24.0.0 src/llmq/net_signing.cpp:47-62,307-319; src/net_processing.cpp:6930-6936 ProcessMessage
    pub fn on_qsigrec<L: QuorumLookup>(
        &mut self,
        peer: PeerId,
        wire: &[u8],
        params: &ChainSetup,
        lookup: &L,
        read: BlsCtx,
    ) -> IntakeOutcome<K::Error, L::Error> {
        let Some(sig) = RecoveredSig::decode(&mut &wire[..]) else {
            return IntakeOutcome {
                request_completion: None,
                status: Ok(IntakeStatus::Unreadable),
            };
        };
        let request_completion = Some(RequestCompletion {
            peer,
            hash: sig.hash(),
        });
        let status = if params.consensus.llmq(sig.llmq_type).is_none() {
            if peer != -1 {
                self.remove_node(peer);
            }
            Ok(IntakeStatus::BanUnknownType)
        } else {
            self.queue_recovered(peer, Arc::new(sig), lookup, read)
        };
        IntakeOutcome {
            request_completion,
            status,
        }
    }

    // Core: v24.0.0 src/llmq/signing.cpp:422-475 CSigningManager::VerifyAndProcessRecoveredSig
    fn queue_recovered<L: QuorumLookup>(
        &mut self,
        peer: PeerId,
        sig: Arc<RecoveredSig>,
        lookup: &L,
        read: BlsCtx,
    ) -> Result<IntakeStatus, SigningError<K::Error, L::Error>> {
        if !lookup
            .is_quorum_active(sig.llmq_type, &sig.quorum_hash)
            .map_err(SigningError::Lookup)?
        {
            return Ok(IntakeStatus::InactiveQuorum);
        }
        if self.db.has_for_hash(&sig.hash()).map_err(SigningError::Storage)? {
            return Ok(IntakeStatus::AlreadyKnown);
        }
        if lookup
            .get_quorum(sig.llmq_type, &sig.quorum_hash)
            .map_err(SigningError::Lookup)?
            .is_none()
        {
            return Ok(IntakeStatus::QuorumUnavailable);
        }
        if self.reconstructed.contains_key(&sig.hash()) {
            return Ok(IntakeStatus::PendingReconstruction);
        }
        if self.pending_count >= MAX_PENDING_TOTAL
            || self.pending.get(&peer).is_some_and(|q| q.len() >= MAX_PENDING_PER_PEER)
        {
            return Ok(IntakeStatus::QueueFull);
        }
        self.pending.entry(peer).or_default().push_back(PendingSig {
            sig,
            read,
        });
        self.pending_count += 1;
        Ok(IntakeStatus::Queued)
    }
}
