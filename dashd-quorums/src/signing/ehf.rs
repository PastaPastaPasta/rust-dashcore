//! Core's `CEHFSignalsHandler`: signs the EHF signals of deployments in their window, and turns a
//! recovered EHF signature into an MNHF transaction for the mempool and relay.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dash_consensus::{BlockId, Tx, TxRef};
use dash_dml::mnhf::EhfSignals;
use dash_dml::mnhf::signer::{
    EhfSigner, TipFacts, TrySignOutcome, ehf_on_recovered, ehf_sign_requests, ehf_try_sign,
};
use dash_evo::locks::wire::RecoveredSig;
use dash_llmq::dkg::bls::BlsCtx;
use dash_llmq::sigenv::SignRequest;
use dash_llmq::signing::api::{
    QuorumSigning, RecoveredSigListener, RecoveredSigResult, SigningFault,
};
use dash_params::ChainSetup;
use dashd_chainstate::chain::reply::no_wait_marked;
use tracing::{debug, info};

/// A tip as `UpdatedBlockTip` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EhfTip {
    pub height: i32,
    /// `DeploymentActiveAfter(tip, V20)`.
    pub v20_active: bool,
    /// `tip->GetMedianTimePast()`.
    pub mtp: i64,
    /// `GetSignalsStage(tip)`.
    pub signals: EhfSignals,
}

/// The chain reads the handler makes, and the node's abort for a fault on the listener path,
/// which returns no error.
pub trait EhfChain: Send + Sync {
    /// `block`'s facts; `Ok(None)` if the block is not in the index.
    fn tip(&self, block: BlockId) -> Result<Option<EhfTip>, SigningFault>;
    /// `GetSignalsStage(ActiveTip())`.
    fn active_tip_signals(&self) -> Result<EhfSignals, SigningFault>;
    /// Core's `AbortNode` for a `Storage` or `Lookup` fault.
    fn abort(&self, fault: &SigningFault);
}

/// The mempool and txindex calls of `HandleNewRecoveredSig`; they may wait on V.
pub trait EhfMempool: Send + Sync {
    /// `g_txindex->BlockUntilSyncedToCurrentChain()` when a txindex runs; else nothing.
    fn block_until_txindex_synced(&self);
    /// `ProcessTransaction(tx)` under `cs_main`: `Ok(Ok(()))` for `VALID`, `Ok(Err(state))`
    /// for every other result.
    fn process_transaction(&self, tx: TxRef) -> Result<Result<(), String>, SigningFault>;
}

/// Core's `CEHFSignalsHandler`. Register it as a recovered-signature listener before the
/// ChainLock and InstantSend signers and the sig-share manager, as Core's `ActiveContext`
/// constructs it before starting them; unregister it before it drops.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:21-34, src/active/context.cpp:46, :56-64
pub struct EhfSignalsHandler<S, C> {
    params: Arc<ChainSetup>,
    signing: Arc<S>,
    chain: C,
    mempool: Arc<dyn EhfMempool>,
    ids: Mutex<EhfSigner>,
}

impl<S: QuorumSigning, C: EhfChain> EhfSignalsHandler<S, C> {
    pub fn new(
        params: Arc<ChainSetup>,
        signing: Arc<S>,
        chain: C,
        mempool: Arc<dyn EhfMempool>,
    ) -> Self {
        Self {
            params,
            signing,
            chain,
            mempool,
            ids: Mutex::default(),
        }
    }

    fn ids(&self) -> MutexGuard<'_, EhfSigner> {
        self.ids.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tries to sign every unmined EHF deployment in its signal window after `tip`. Its quorum
    /// lookups may wait on V, so it runs on an unmarked thread. A fault is returned at once.
    // Core: v24.0.2 src/llmq/ehf_signals.cpp:42-64 UpdatedBlockTip
    pub fn updated_block_tip(&self, tip: BlockId) -> Result<(), SigningFault> {
        debug_assert!(!no_wait_marked(), "EHF signing from a no-wait task: it may wait on V");
        let Some(tip) = self.chain.tip(tip)? else {
            return Ok(());
        };
        let facts = TipFacts {
            v20_active: tip.v20_active,
            mtp: tip.mtp,
            mined_bits: &tip.signals,
        };
        for (bit, id) in ehf_sign_requests(&self.params, facts) {
            self.try_sign(bit, &id, tip.height)?;
        }
        Ok(())
    }

    /// The lookups run before the tracked ids are locked, as Core takes `cs` only to insert.
    // Core: v24.0.2 src/llmq/ehf_signals.cpp:66-98 trySignEHFSignal
    fn try_sign(&self, bit: u8, id: &[u8; 32], height: i32) -> Result<(), SigningFault> {
        let t = self.params.consensus.llmq_type_mnhf;
        if self.params.consensus.llmq(t).is_none() {
            return Ok(());
        }
        let recovered = self.signing.has_recovered_sig_for_id(t, id)?;
        let selected = if recovered {
            None
        } else {
            self.signing.select_quorum_for_signing(t, id)?.map(|q| q.quorum_hash)
        };
        let outcome =
            ehf_try_sign(&self.params, bit, &mut self.ids(), |_| selected, |_, _| recovered);
        let id_hex = hex_id(id);
        let request = match outcome {
            TrySignOutcome::Sign(request) => request,
            TrySignOutcome::AlreadyRecovered => {
                debug!(
                    target: "ehf",
                    "CEHFSignalsHandler::trySignEHFSignal: already signed bit={bit} at \
                     height={height} id={id_hex}"
                );
                return Ok(());
            }
            TrySignOutcome::NoQuorum => {
                info!("CEHFSignalsHandler::trySignEHFSignal no quorum for id={id_hex}");
                return Ok(());
            }
            TrySignOutcome::UnconfiguredQuorumType => return Ok(()),
        };
        debug!(
            target: "ehf",
            "CEHFSignalsHandler::trySignEHFSignal: bit={bit} at height={height} id={id_hex}"
        );
        self.signing.async_sign_if_member(&SignRequest {
            llmq_type: request.llmq_type,
            id: request.id,
            msg_hash: request.msg_hash,
            quorum_hash: Some(request.quorum_hash),
            allow_resign: false,
            allow_diff_msg: request.allow_diff_msg,
        })?;
        Ok(())
    }

    // Core: v24.0.2 src/llmq/ehf_signals.cpp:100-144 HandleNewRecoveredSig
    fn handle(&self, sig: &RecoveredSig) -> Result<RecoveredSigResult, SigningFault> {
        self.mempool.block_until_txindex_synced();
        if !self.ids().tracks(&sig.id) {
            return Ok(RecoveredSigResult::Nothing);
        }
        let signals = self.chain.active_tip_signals()?;
        let tx = {
            let ids = self.ids();
            // An id is tracked only after V20, where the scheme is basic.
            ehf_on_recovered(&self.params, sig, &ids, &signals, BlsCtx::from_legacy(false))
        };
        let Some(tx) = tx else {
            return Ok(RecoveredSigResult::Nothing);
        };
        let tx: TxRef = Arc::new(Tx::new(tx));
        let txid = tx.txid();
        info!("CEHFSignalsHandler::HandleNewRecoveredSig Special EHF TX is created hash={txid}");
        match self.mempool.process_transaction(tx)? {
            Ok(()) => Ok(RecoveredSigResult::RelayTx(txid)),
            Err(state) => {
                info!(
                    "CEHFSignalsHandler::HandleNewRecoveredSig -- AcceptToMemoryPool failed: {state}"
                );
                Ok(RecoveredSigResult::Nothing)
            }
        }
    }
}

/// A fault aborts the node through [`EhfChain::abort`], except `Stopped`, which is dropped.
impl<S: QuorumSigning, C: EhfChain> RecoveredSigListener for EhfSignalsHandler<S, C> {
    fn handle_new_recovered_sig(&self, sig: &RecoveredSig) -> RecoveredSigResult {
        self.handle(sig).unwrap_or_else(|fault| {
            if fault != SigningFault::Stopped {
                self.chain.abort(&fault);
            }
            RecoveredSigResult::Nothing
        })
    }
}

/// Core's `uint256::ToString`: the bytes reversed, in hex.
fn hex_id(id: &[u8; 32]) -> String {
    id.iter().rev().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
