//! `NetInstantSend`'s message intake (PLAN-LK1b §6, slice S3): the `isdlock` handler's checks and
//! scores before a lock is queued on S2's [`IsManager`]. The batch verification of the queue is in
//! [`super::batch`] and the worker in [`super::worker`].
//!
//! Every peer action goes through [`IsPeers`], and every verified lock through S4's
//! [`IsLockProcessor`]; S6 wires both in `dashd-node` and registers the handler with P6a.

use std::fmt;
use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_evo::locks::{
    ISLOCK_MAX_INPUTS, LockError, LockReject, QuorumScanView, isdlock_hash, islock_message_checks,
};
use dash_num::Hash256;
use dash_params::ChainSetup;
use dash_types::codec::BaseCodec;

use super::LockedTx;
use super::manager::{IsManager, NodeId};
use super::seams::{IsLockProcessor, IsPeers, IsSignerSeam, RecoveredSigs};

/// The score of an oversized, not trivially valid or off-cycle lock.
/// Core: `instantsend/net_instantsend.cpp:34`.
pub const INVALID_ISLOCK_MISBEHAVIOR_SCORE: i32 = 100;

/// The score of a lock whose cycle hash names no known block.
/// Core: `instantsend/net_instantsend.cpp:35`.
pub const UNKNOWN_CYCLE_HASH_MISBEHAVIOR_SCORE: i32 = 1;

/// What [`NetInstantSend::process_message`] did with a peer's `isdlock`. Every answer is the
/// message handled; the caller neither scores nor stops for any of them. A node fault is
/// [`IsNetError`] instead.
#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub enum IsdLockIntake {
    /// Ignored, scored, already had or queued, as Core's handler answers.
    Handled,
    /// The payload is not an `isdlock`: Core's `vRecv >> *islock` throws, and the exception
    /// boundary of `ProcessMessages` logs it at NET debug and drops the message, with no score
    /// and no disconnect. The line is already logged.
    // Core: net_processing.cpp:5958-5962
    Undecodable(String),
}

/// A node fault that stopped NetInstantSend short of Core's answer: the caller stops the node.
/// A peer's malformed message is never one ([`IsdLockIntake::Undecodable`]).
#[derive(Debug)]
pub enum IsNetError<E> {
    /// `llmqTypeDIP0024InstantSend` is not configured, where Core asserts
    /// (`net_instantsend.cpp:274-276`, `:334-336`).
    UnknownLlmqType(u8),
    /// The quorum view failed.
    View(E),
    /// An InstantSend store read or write failed.
    Store(dash_kv::StoreError),
}

impl<E: fmt::Display> fmt::Display for IsNetError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IsNetError::UnknownLlmqType(t) => write!(f, "InstantSend LLMQ type {t} not configured"),
            IsNetError::View(e) => write!(f, "quorum view: {e}"),
            IsNetError::Store(e) => write!(f, "InstantSend store: {e}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for IsNetError<E> {}

impl<E> From<dash_kv::StoreError> for IsNetError<E> {
    fn from(e: dash_kv::StoreError) -> Self {
        IsNetError::Store(e)
    }
}

/// `NetInstantSend`: the `isdlock` handler, the batch verification of its queue and the worker.
pub struct NetInstantSend {
    pub(super) params: Arc<ChainSetup>,
    pub(super) manager: Arc<IsManager>,
    pub(super) peers: Arc<dyn IsPeers>,
    pub(super) recsigs: Arc<dyn RecoveredSigs>,
    pub(super) processor: Arc<dyn IsLockProcessor>,
    /// `m_signer`: none on a node that is not a masternode.
    pub(super) signer: Option<Arc<dyn IsSignerSeam>>,
}

impl NetInstantSend {
    pub fn new(
        params: Arc<ChainSetup>,
        manager: Arc<IsManager>,
        peers: Arc<dyn IsPeers>,
        recsigs: Arc<dyn RecoveredSigs>,
        processor: Arc<dyn IsLockProcessor>,
        signer: Option<Arc<dyn IsSignerSeam>>,
    ) -> Self {
        NetInstantSend {
            params,
            manager,
            peers,
            recsigs,
            processor,
            signer,
        }
    }

    /// The manager whose queue the handler fills.
    pub fn manager(&self) -> &Arc<IsManager> {
        &self.manager
    }

    /// `ProcessMessage(pfrom, ISDLOCK, vRecv)`: nothing while InstantSend is disabled; else the
    /// lock is read under `read` (the flag now, which its lazy signature keeps), scored and
    /// dropped if oversized (+100), its request from `from` erased, scored and dropped if not
    /// trivially valid (+100), naming no known cycle block (+1) or a block off the cycle (+100),
    /// and queued when not already had, with every peer's request for it forgotten.
    ///
    /// `view` answers the cycle block's height (`ResolveCycleHeight`).
    ///
    /// A payload Core's read throws on is [`IsdLockIntake::Undecodable`], never an error.
    ///
    /// # Errors
    ///
    /// A view or store failure, or an unconfigured InstantSend type.
    // Core: instantsend/net_instantsend.cpp:238-289 (ProcessMessage), :64-72
    // (ValidateIncomingISLock), :74-88 (ResolveCycleHeight), :90-102
    // (ValidateDeterministicCycleHeight)
    pub fn process_message<V: QuorumScanView + ?Sized>(
        &self,
        view: &V,
        from: NodeId,
        payload: &[u8],
        read: BlsCtx,
    ) -> Result<IsdLockIntake, IsNetError<V::Error>> {
        if !self.manager.is_instant_send_enabled() {
            return Ok(IsdLockIntake::Handled);
        }
        let islock = match LockedTx::decode(&mut &payload[..]) {
            Ok(islock) => islock,
            Err(e) => {
                tracing::debug!(
                    target: "net",
                    "ProcessMessages(isdlock, {} bytes): Exception '{}' caught",
                    payload.len(),
                    e,
                );
                return Ok(IsdLockIntake::Undecodable(e.to_string()));
            }
        };
        if islock.inputs().len() > ISLOCK_MAX_INPUTS {
            self.peers.misbehaving(from, INVALID_ISLOCK_MISBEHAVIOR_SCORE);
            return Ok(IsdLockIntake::Handled);
        }
        let hash = isdlock_hash(&islock);
        self.peers.erase_object_request(from, &hash);
        match islock_message_checks(&self.params, view, &islock) {
            Ok(_) => {}
            Err(LockError::Rejected(LockReject::Malformed | LockReject::InvalidCycle)) => {
                self.peers.misbehaving(from, INVALID_ISLOCK_MISBEHAVIOR_SCORE);
                return Ok(IsdLockIntake::Handled);
            }
            Err(LockError::Rejected(LockReject::UnknownCycle)) => {
                self.peers.misbehaving(from, UNKNOWN_CYCLE_HASH_MISBEHAVIOR_SCORE);
                return Ok(IsdLockIntake::Handled);
            }
            Err(LockError::UnknownLlmqType(t)) => return Err(IsNetError::UnknownLlmqType(t)),
            Err(LockError::View(e)) => return Err(IsNetError::View(e)),
            // `islock_message_checks` gives no other answer; were it to, the lock is dropped.
            Err(LockError::Rejected(_) | LockError::BlsUnsupported) => {
                tracing::error!(
                    "NetInstantSend -- ISDLOCK islock={}: unexpected check answer, peer={}",
                    Hash256::from(hash),
                    from,
                );
                return Ok(IsdLockIntake::Handled);
            }
        }
        if !self.manager.already_have(&hash)? {
            tracing::debug!(
                target: "instantsend",
                "NetInstantSend -- ISDLOCK txid={}, islock={}: received islock, peer={}",
                islock.txid(),
                Hash256::from(hash),
                from,
            );
            self.manager.enqueue_instant_send_lock(from, hash, Arc::new(islock), read.is_legacy());
            self.peers.forget_object_request(&hash);
        }
        Ok(IsdLockIntake::Handled)
    }
}

#[cfg(test)]
#[path = "net_tests.rs"]
mod tests;
