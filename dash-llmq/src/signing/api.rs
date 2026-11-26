//! The node's signing API for other lanes: Core's `CSigningManager` queries and listeners,
//! `NetSigning::ProcessRecoveredSig` and `CSigSharesManager::AsyncSignIfMember`.

use core::fmt;
use std::sync::Arc;

use dash_consensus::primitives::TxHash;
use dash_evo::locks::wire::RecoveredSig;
use dash_params::LlmqType;

pub use super::manager::ListenerId;
use crate::dkg::wire::{Inventory, QuorumHash};
pub use crate::sigenv::{PeerId, SignRequest};

/// A fault under a call: Core's thrown `dbwrapper_error` or lookup exception, never "absent".
///
/// `Storage` and `Lookup` abort the node on every path. `Stopped` only follows shutdown and is
/// dropped, never fatal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SigningFault {
    Storage(String),
    Lookup(String),
    Stopped,
}

impl fmt::Display for SigningFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(e) => write!(f, "recovered-signature storage fault: {e}"),
            Self::Lookup(e) => write!(f, "quorum lookup fault: {e}"),
            Self::Stopped => f.write_str("signing runtime stopped"),
        }
    }
}

impl std::error::Error for SigningFault {}

/// What `NetSigning` relays for a listener.
// Core: v24.0.2 src/llmq/signing.h:38 RecoveredSigResult
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveredSigResult {
    Nothing,
    RelayInv(Inventory),
    RelayTx(TxHash),
}

/// The quorum `SelectQuorumForSigning` picks at the tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedQuorum {
    pub quorum_hash: QuorumHash,
    pub base_height: i32,
}

/// Core's `CRecoveredSigsListener`.
///
/// Called on the signing runtime's own thread or the caller's intake thread, never on the
/// signals thread, with no signing lock held. It may call back into [`QuorumSigning`].
// Core: v24.0.2 src/llmq/signing.h:153-159
pub trait RecoveredSigListener: Send + Sync {
    fn handle_new_recovered_sig(&self, sig: &RecoveredSig) -> RecoveredSigResult;
}

/// The signing entry point and the recovered-signature queries.
///
/// The recovered-signature reads, `truncate_recovered_sig` and the listener registry never wait
/// on V. `async_sign_if_member` and `select_quorum_for_signing` may wait on V for the quorum
/// lookup, as Core takes `cs_main` in `SelectQuorumForSigning`, so they must not be called from
/// a no-wait-marked thread. No method waits on the signals thread or holds the signing lock
/// while waiting on V.
pub trait QuorumSigning: Send + Sync {
    // Core: v24.0.2 src/llmq/signing_shares.cpp:792-876 CSigSharesManager::AsyncSignIfMember
    fn async_sign_if_member(&self, req: &SignRequest) -> Result<bool, SigningFault>;
    // Core: v24.0.2 src/llmq/quorumsman.h:222-223 SelectQuorumForSigning (tip, SIGN_HEIGHT_OFFSET)
    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
    ) -> Result<Option<SelectedQuorum>, SigningFault>;
    // Core: v24.0.2 src/llmq/signing.cpp:649-652 CSigningManager::HasRecoveredSig
    fn has_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault>;
    // Core: v24.0.2 src/llmq/signing.cpp:654-657 CSigningManager::HasRecoveredSigForId
    fn has_recovered_sig_for_id(&self, t: LlmqType, id: &[u8; 32]) -> Result<bool, SigningFault>;
    // Core: v24.0.2 src/llmq/signing.cpp:659-662 CSigningManager::HasRecoveredSigForSession
    fn has_recovered_sig_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, SigningFault>;
    // Core: v24.0.2 src/llmq/signing.cpp:664-668 CSigningManager::HasRecoveredSigForSigning
    fn has_recovered_sig_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault>;
    // Core: v24.0.2 src/llmq/signing.cpp:670-674 CSigningManager::GetRecoveredSig
    fn get_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, SigningFault>;
    // Core: v24.0.2 src/llmq/signing.cpp:676-691 CSigningManager::IsConflicting
    fn is_conflicting(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault>;
    // Core: v24.0.2 src/llmq/signing.cpp:693-696 CSigningManager::GetVoteForId
    fn get_vote_for_id(&self, t: LlmqType, id: &[u8; 32])
    -> Result<Option<[u8; 32]>, SigningFault>;
    // Core: v24.0.2 src/llmq/signing.cpp:625-628 CSigningManager::TruncateRecoveredSig
    fn truncate_recovered_sig(&self, t: LlmqType, id: &[u8; 32]) -> Result<(), SigningFault>;
    /// Listeners are called in registration order.
    // Core: v24.0.2 src/llmq/signing.cpp:636-640 RegisterRecoveredSigsListener
    fn register_listener(&self, l: Arc<dyn RecoveredSigListener>) -> ListenerId;
    // Core: v24.0.2 src/llmq/signing.cpp:642-647 UnregisterRecoveredSigsListener
    fn unregister_listener(&self, id: ListenerId);
}

/// The recovered-signature intake used by the sig-share and InstantSend runtimes.
pub trait RecoveredSigIntake: Send + Sync {
    /// Stores an already verified signature; if stored, calls every listener in registration
    /// order and relays its result, then queues `NotifyRecoveredSig` with proactive relay when
    /// `consider_proactive_relay` holds and the type is not 100_67, 400_60 or 400_85.
    /// `Ok(false)`: already known, or a conflict. It does no quorum lookup, but its listeners may
    /// wait on V, so it must not be called from a no-wait-marked thread.
    // Core: v24.0.2 src/llmq/net_signing.cpp:198-218 NetSigning::ProcessRecoveredSig;
    // src/llmq/signing.cpp:581-611 CSigningManager::ProcessRecoveredSig
    fn process_recovered_sig(
        &self,
        sig: Arc<RecoveredSig>,
        consider_proactive_relay: bool,
    ) -> Result<bool, SigningFault>;
    /// Queues an already verified signature; the next pending pass processes it first, with
    /// proactive relay considered. Never waits on verification.
    // Core: v24.0.2 src/llmq/signing.cpp:619-623 CSigningManager::PushReconstructedRecoveredSig
    fn push_reconstructed_recovered_sig(&self, sig: Arc<RecoveredSig>);
    /// Drops every pending, unverified signature from `peer`.
    // Core: v24.0.2 src/llmq/signing.cpp:477-486 CSigningManager::RemoveNode
    fn remove_node(&self, peer: PeerId);
}
