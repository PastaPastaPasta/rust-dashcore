//! The node's half of the LLMQ signers: the EHF handler's chain reads on V ([`NodeEhfChain`]),
//! its mempool and txindex calls ([`NodeEhfMempool`]) and its tip duty ([`EhfTipDuty`]), and
//! the signing runtime behind the node's seams: the P2P processor's [`SigningSeam`],
//! `NotifyRecoveredSig`'s relay, and NetInstantSend's [`RecoveredSigs`]. Each adapter is generic
//! over the signing API, not a concrete runtime.
//!
//! The seams' faults: a storage or lookup fault aborts the node; `Stopped` (the runtime is
//! shutting down) is dropped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use dash_consensus::{BlockId, TxRef};
use dash_dml::mnhf::EhfSignals;
use dash_evo::locks::RecoveredSig;
use dash_kv::{ErrorKind, StoreError};
use dash_p2p::PeerId;
use dash_params::{BuriedDeployment, LlmqType};
use dash_pkc::bls::BlsSigBytes;
use dash_primitives::QuorumHash;
use dashd_chainstate::chain::{ChainHandle, no_wait_marked};
use dashd_indexes::IndexStatus;
use dashd_locks::is::seams::{RecoveredSigs, SignRequestId};
use dashd_mempool::api::MempoolHandle;
use dashd_p2p::Shared;
use dashd_p2p::processing::seams::SigningSeam;
use dashd_quorums::signing::{
    EhfChain, EhfMempool, EhfSignalsHandler, EhfTip, QuorumSigning, RecSigNotify,
    RecoveredSigIntake, RecoveredSigNet, SigningFault,
};
use dashd_rpc::mempool::Processed;

use crate::abort::AbortSink;
use crate::p2p::active_mn::ActiveDuties;
use crate::rpc::mempool::map_accept_result;

pub mod shares;
#[cfg(test)]
mod tests;

/// [`EhfChain`] on V: each read is one V turn, so it waits on V's FIFO and never runs on V.
#[derive(Clone)]
pub struct NodeEhfChain {
    chain: ChainHandle,
    abort: Arc<dyn AbortSink>,
}

impl NodeEhfChain {
    pub fn new(chain: ChainHandle, abort: Arc<dyn AbortSink>) -> Self {
        Self {
            chain,
            abort,
        }
    }
}

/// `GetForBlock`'s exceptions, which Core lets escape the signal: a node fault.
fn stage_fault(e: dash_dml::mnhf::GetForBlockError) -> SigningFault {
    SigningFault::Storage(e.to_string())
}

impl EhfChain for NodeEhfChain {
    // Core: v24.0.2 src/llmq/ehf_signals.cpp:44-50, src/deploymentstatus.h:15-19
    fn tip(&self, block: BlockId) -> Result<Option<EhfTip>, SigningFault> {
        let reply = self.chain.query(move |q| {
            let Some(e) = q.chain.reader().entry(block) else {
                return Ok(None);
            };
            let v20 = q.chain.params().consensus.deployment_height(BuriedDeployment::V20);
            let signals = q.ehf_signals_stage(block).map_err(stage_fault)?;
            Ok(Some(EhfTip {
                height: e.height as i32,
                v20_active: e.height + 1 >= v20.to_u32(),
                mtp: e.mtp,
                signals,
            }))
        });
        reply.recv().unwrap_or(Err(SigningFault::Stopped))
    }

    // Core: v24.0.2 src/llmq/ehf_signals.cpp:111
    fn active_tip_signals(&self) -> Result<EhfSignals, SigningFault> {
        let reply = self.chain.query(|q| match q.chain.private_tip() {
            Some((tip, _)) => q.ehf_signals_stage(tip.id).map_err(stage_fault),
            None => Ok(EhfSignals::new()),
        });
        reply.recv().unwrap_or(Err(SigningFault::Stopped))
    }

    fn abort(&self, fault: &SigningFault) {
        self.abort.abort_node(&format!("CEHFSignalsHandler: {fault}"), None);
    }
}

/// [`EhfMempool`] over the node's pool and txindex, called on "recsigs": the admission and the
/// txindex wait may wait on V, so neither runs on V or a no-wait task.
pub struct NodeEhfMempool {
    pub mempool: Option<Arc<dyn MempoolHandle>>,
    /// Core's `g_txindex`: `None` with `-txindex=0`.
    pub txindex: Option<Arc<dyn IndexStatus>>,
}

impl EhfMempool for NodeEhfMempool {
    // Core: v24.0.3 src/llmq/ehf_signals.cpp:102-104
    fn block_until_txindex_synced(&self) {
        debug_assert!(!no_wait_marked(), "the txindex wait from a no-wait task");
        if let Some(txindex) = &self.txindex {
            txindex.block_until_synced();
        }
    }

    /// Without a pool, Core's `no-mempool` rejection. An `EvoDbMissingStateError` thrown out of
    /// the admission escapes "recsigs", which aborts. A dropped reply comes after V's `Fatal` or
    /// `AbortNode`, or once the pool stops at shutdown: `Stopped`.
    // Core: v24.0.3 src/llmq/ehf_signals.cpp:134-143, src/validation.cpp:4923-4935
    fn process_transaction(&self, tx: TxRef) -> Result<Result<(), String>, SigningFault> {
        debug_assert!(!no_wait_marked(), "an admission from a no-wait task");
        let Some(pool) = &self.mempool else {
            return Ok(Err("no-mempool".to_owned()));
        };
        let result = pool.accept(tx, false).map_err(|_| SigningFault::Stopped)?;
        match map_accept_result(result) {
            Processed::Valid(_) => Ok(Ok(())),
            Processed::Invalid(state) => Ok(Err(state.to_string())),
            Processed::MempoolEntry(_) => Ok(Err("Valid".to_owned())),
            Processed::Unavailable(what) => {
                Err(SigningFault::Storage(format!("ProcessTransaction: {what}")))
            }
        }
    }
}

/// `ehf_sighandler->UpdatedBlockTip` as one of `ActiveContext::UpdatedBlockTip`'s duties, on the
/// `am1-tip` worker. A fault is Core's `AbortNode` through [`EhfChain::abort`]; `Stopped` is
/// dropped.
// Core: v24.0.3 src/active/context.cpp:110
pub struct EhfTipDuty<S, C> {
    handler: Arc<EhfSignalsHandler<S, C>>,
    chain: C,
}

impl<S, C> EhfTipDuty<S, C> {
    pub fn new(handler: Arc<EhfSignalsHandler<S, C>>, chain: C) -> Self {
        Self {
            handler,
            chain,
        }
    }
}

impl<S: QuorumSigning, C: EhfChain> ActiveDuties for EhfTipDuty<S, C> {
    fn updated_block_tip(&self, new: BlockId, _: bool) {
        match self.handler.updated_block_tip(new) {
            Ok(()) | Err(SigningFault::Stopped) => {}
            Err(fault) => self.chain.abort(&fault),
        }
    }
}

/// The EHF duty from step 7c, when the active masternode's duties are fixed: nothing until
/// step 7d builds the handler over the signing runtime and binds it, before V publishes a tip
/// (`loadblk` starts at step 11).
#[derive(Default)]
pub struct LateEhf(OnceLock<Arc<dyn ActiveDuties>>);

impl LateEhf {
    pub fn bind(&self, duty: Arc<dyn ActiveDuties>) {
        assert!(self.0.set(duty).is_ok(), "the EHF handler is bound once");
    }
}

impl ActiveDuties for LateEhf {
    fn updated_block_tip(&self, new: BlockId, ibd: bool) {
        if let Some(duty) = self.0.get() {
            duty.updated_block_tip(new, ibd);
        }
    }
}

/// The P2P seam's fault policy: the first storage or lookup fault aborts the node.
pub struct SigningFaults {
    abort: Arc<dyn AbortSink>,
    aborted: AtomicBool,
}

impl SigningFaults {
    pub fn new(abort: Arc<dyn AbortSink>) -> SigningFaults {
        SigningFaults {
            abort,
            aborted: AtomicBool::new(false),
        }
    }

    fn check<T>(&self, site: &str, result: Result<T, SigningFault>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(SigningFault::Stopped) => None,
            Err(fault) => {
                if !self.aborted.swap(true, Ordering::SeqCst) {
                    self.abort.abort_node(&format!("{site}: {fault}"), None);
                }
                None
            }
        }
    }
}

/// The processor's [`SigningSeam`] over the runtime, called inline on the message thread as Core
/// calls `CSigningManager` there. The runtime's intake may wait on V for its quorum lookup, never
/// on the signing worker.
pub struct P2pSigning<R> {
    runtime: Arc<R>,
    faults: Arc<SigningFaults>,
}

impl<R> P2pSigning<R> {
    pub fn new(runtime: Arc<R>, faults: Arc<SigningFaults>) -> P2pSigning<R> {
        P2pSigning {
            runtime,
            faults,
        }
    }
}

impl<R: RecoveredSigNet> SigningSeam for P2pSigning<R> {
    // Core: v24.0.2 src/llmq/net_signing.cpp:52 Params().GetLLMQ(type).has_value()
    fn llmq_configured(&self, llmq_type: LlmqType) -> bool {
        self.runtime.chain().consensus.llmq(llmq_type).is_some()
    }

    fn verify_and_process(&self, peer: PeerId, sig: RecoveredSig, legacy: bool) {
        let result = self.runtime.on_recovered_sig(peer, Arc::new(sig), legacy);
        self.faults.check("qsigrec intake", result);
    }

    fn remove_node(&self, peer: PeerId) {
        self.runtime.remove_node(peer);
    }

    fn already_have(&self, hash: &[u8; 32]) -> bool {
        let result = self.runtime.already_have(hash);
        self.faults.check("recovered sig AlreadyHave", result).unwrap_or(false)
    }

    fn for_getdata(&self, hash: &[u8; 32]) -> Option<RecoveredSig> {
        let result = self.runtime.for_getdata(hash);
        self.faults.check("recovered sig getdata", result).flatten()
    }
}

/// The processor's signing seam from attach: [`NoSigning`](dashd_p2p::processing::seams::NoSigning)'s
/// answers until `AddExtraHandler(NetSigning)` binds the runtime's seam.
// Core: v24.0.2 src/init.cpp:2202
#[derive(Default)]
pub struct LateSigning(OnceLock<Arc<dyn SigningSeam>>);

impl LateSigning {
    pub fn bind(&self, seam: Arc<dyn SigningSeam>) {
        assert!(self.0.set(seam).is_ok(), "NetSigning added once");
    }
}

impl SigningSeam for LateSigning {
    fn enabled(&self) -> bool {
        self.0.get().is_some_and(|s| s.enabled())
    }

    fn llmq_configured(&self, llmq_type: LlmqType) -> bool {
        self.0.get().is_some_and(|s| s.llmq_configured(llmq_type))
    }

    fn verify_and_process(&self, peer: PeerId, sig: RecoveredSig, legacy: bool) {
        if let Some(seam) = self.0.get() {
            seam.verify_and_process(peer, sig, legacy);
        }
    }

    fn remove_node(&self, peer: PeerId) {
        if let Some(seam) = self.0.get() {
            seam.remove_node(peer);
        }
    }

    fn already_have(&self, hash: &[u8; 32]) -> bool {
        self.0.get().is_some_and(|s| s.already_have(hash))
    }

    fn for_getdata(&self, hash: &[u8; 32]) -> Option<RecoveredSig> {
        self.0.get()?.for_getdata(hash)
    }
}

/// `NotifyRecoveredSig` as `PeerManagerImpl`'s relay, called directly with no signing lock held.
/// Until [`Self::bind`], and after the processor is gone, a signature reaches no peer: Core's
/// signal reaches `PeerManager` only once `RegisterValidationInterface(peerman)` has run.
// Core: v24.0.2 src/llmq/net_signing.cpp:393-396; src/net_processing.cpp:7024-7027
#[derive(Default)]
pub struct RelayRecoveredSigs(OnceLock<Weak<Shared>>);

impl RelayRecoveredSigs {
    /// Binds the processor; a second bind is ignored.
    pub fn bind(&self, shared: &Arc<Shared>) {
        let _ = self.0.set(Arc::downgrade(shared));
    }
}

impl RecSigNotify for RelayRecoveredSigs {
    fn notify_recovered_sig(&self, sig: Arc<RecoveredSig>, proactive_relay: bool) {
        if let Some(shared) = self.0.get().and_then(Weak::upgrade) {
            shared.relay_recovered_sig(&sig, proactive_relay);
        }
    }
}

/// NetInstantSend's `m_sigman` from the listener's construction at step 7: the signing manager
/// once step 7c binds it. Nothing calls it before then: no block connects first, and no lock
/// reaches the batch before step 12 connects a peer.
#[derive(Default)]
pub struct LateRecSigs(OnceLock<Arc<dyn RecoveredSigs>>);

impl LateRecSigs {
    pub fn bind(&self, recsigs: Arc<dyn RecoveredSigs>) {
        assert!(self.0.set(recsigs).is_ok(), "bound once");
    }

    #[cfg(test)]
    pub(crate) fn is_bound(&self) -> bool {
        self.0.get().is_some()
    }
}

impl RecoveredSigs for LateRecSigs {
    fn truncate(&self, llmq_type: LlmqType, request_id: &SignRequestId) -> dash_kv::Result<()> {
        self.0.get().map_or(Ok(()), |r| r.truncate(llmq_type, request_id))
    }

    fn get_recovered_sig(
        &self,
        llmq_type: LlmqType,
        id: &SignRequestId,
        msg_hash: &[u8; 32],
    ) -> Option<(QuorumHash, BlsSigBytes)> {
        self.0.get()?.get_recovered_sig(llmq_type, id, msg_hash)
    }

    fn has_recovered_sig_for_id(&self, llmq_type: LlmqType, id: &SignRequestId) -> bool {
        self.0.get().is_some_and(|r| r.has_recovered_sig_for_id(llmq_type, id))
    }

    fn push_reconstructed_recovered_sig(&self, recsig: Arc<RecoveredSig>) {
        if let Some(r) = self.0.get() {
            r.push_reconstructed_recovered_sig(recsig);
        }
    }
}

/// NetInstantSend's [`RecoveredSigs`] over the signing API. Truncation's storage or lookup fault
/// is returned as a store error, so the IS listener's fault path aborts the node and stops there.
/// A read's aborts the node through the P2P seam's [`SigningFaults`] and answers as nothing
/// stored.
pub struct NodeRecSigs<Q> {
    runtime: Arc<Q>,
    faults: Arc<SigningFaults>,
}

impl<Q> NodeRecSigs<Q> {
    pub fn new(runtime: Arc<Q>, faults: Arc<SigningFaults>) -> NodeRecSigs<Q> {
        NodeRecSigs {
            runtime,
            faults,
        }
    }
}

impl<Q: QuorumSigning + RecoveredSigIntake> RecoveredSigs for NodeRecSigs<Q> {
    // Core: v24.0.2 src/llmq/signing.cpp:625-628 CSigningManager::TruncateRecoveredSig
    fn truncate(&self, llmq_type: LlmqType, request_id: &SignRequestId) -> dash_kv::Result<()> {
        match self.runtime.truncate_recovered_sig(llmq_type, request_id) {
            Ok(()) | Err(SigningFault::Stopped) => Ok(()),
            Err(fault) => Err(StoreError::Engine {
                engine: "llmq signing",
                kind: ErrorKind::Other,
                message: fault.to_string(),
            }),
        }
    }

    // Core: v24.0.2 src/llmq/signing.cpp:133-144, :670-674
    fn get_recovered_sig(
        &self,
        llmq_type: LlmqType,
        id: &SignRequestId,
        msg_hash: &[u8; 32],
    ) -> Option<(QuorumHash, BlsSigBytes)> {
        let result = self.runtime.get_recovered_sig(llmq_type, id, msg_hash);
        let sig = self.faults.check("recovered sig read", result).flatten()?;
        Some((sig.quorum_hash, BlsSigBytes::from(sig.sig)))
    }

    // Core: v24.0.2 src/llmq/signing.cpp:654-657
    fn has_recovered_sig_for_id(&self, llmq_type: LlmqType, id: &SignRequestId) -> bool {
        let result = self.runtime.has_recovered_sig_for_id(llmq_type, id);
        self.faults.check("recovered sig read", result).unwrap_or(false)
    }

    // Core: v24.0.2 src/llmq/signing.cpp:619-623
    fn push_reconstructed_recovered_sig(&self, recsig: Arc<RecoveredSig>) {
        self.runtime.push_reconstructed_recovered_sig(recsig);
    }
}
