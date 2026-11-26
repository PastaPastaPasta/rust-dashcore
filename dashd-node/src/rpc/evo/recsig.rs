//! The recovered-signature commands' reads of the node's signing manager.

use std::sync::{Arc, Weak};

use dash_evo::locks::RecoveredSig;
use dash_params::LlmqType;
use dash_types::Numeric as _;
use dashd_quorums::signing::{QuorumSigning, SigningFault};
use dashd_rpc::evo::{EvoError, NodeRecSigs};

use crate::rpc::ChainSlot;

/// The signing manager's three reads the commands make.
pub(crate) trait RecSigReads: Send + Sync {
    fn has(&self, t: LlmqType, id: &[u8; 32], msg: &[u8; 32]) -> Result<bool, SigningFault>;
    fn get(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, SigningFault>;
    fn conflicting(&self, t: LlmqType, id: &[u8; 32], msg: &[u8; 32])
    -> Result<bool, SigningFault>;
}

impl<S: QuorumSigning> RecSigReads for S {
    fn has(&self, t: LlmqType, id: &[u8; 32], msg: &[u8; 32]) -> Result<bool, SigningFault> {
        self.has_recovered_sig(t, id, msg)
    }
    fn get(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, SigningFault> {
        self.get_recovered_sig(t, id, msg)
    }
    fn conflicting(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.is_conflicting(t, id, msg)
    }
}

/// The signing manager, held weakly: the LLMQ context's reset at shutdown drops it, and the
/// commands then find no context, as Core's would.
pub(crate) struct RpcSigning(Weak<dyn RecSigReads>);

impl ChainSlot {
    /// Step 7c's signing manager. A read's storage or lookup fault is the command's error and
    /// leaves the node running, as Core's handlers let the database's exception escape.
    pub(crate) fn set_signing(&self, runtime: Arc<dyn RecSigReads>) {
        let _ = self.signing.set(RpcSigning(Arc::downgrade(&runtime)));
    }

    // Core: v24.0.0 src/rpc/server.cpp:540-541
    fn sigman<T>(
        &self,
        read: impl FnOnce(&dyn RecSigReads) -> Result<T, SigningFault>,
    ) -> Result<T, EvoError> {
        let slot = self.signing.get().ok_or(EvoError::LlmqUnavailable)?;
        let runtime = slot.0.upgrade().ok_or(EvoError::LlmqUnavailable)?;
        read(runtime.as_ref()).map_err(|fault| match fault {
            SigningFault::Stopped => EvoError::LlmqUnavailable,
            fault => EvoError::Failed(fault.to_string()),
        })
    }
}

// Core: v24.0.0 src/llmq/signing.cpp:649-652, :670-690
impl NodeRecSigs for ChainSlot {
    // Core: v24.0.0 src/rpc/server_util.cpp:113-119 EnsureLLMQContext
    fn llmq_context(&self) -> Result<(), EvoError> {
        self.sigman(|_| Ok(()))
    }

    fn has_recovered_sig(&self, t: u8, id: &[u8; 32], msg: &[u8; 32]) -> Result<bool, EvoError> {
        self.sigman(|s| s.has(LlmqType::from_base(t), id, msg))
    }

    fn get_recovered_sig(
        &self,
        t: u8,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, EvoError> {
        self.sigman(|s| s.get(LlmqType::from_base(t), id, msg))
    }

    fn is_conflicting(&self, t: u8, id: &[u8; 32], msg: &[u8; 32]) -> Result<bool, EvoError> {
        self.sigman(|s| s.conflicting(LlmqType::from_base(t), id, msg))
    }
}
