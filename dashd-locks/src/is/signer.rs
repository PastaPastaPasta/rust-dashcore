//! Core's `InstantSendSigner` (`instantsend/signing.{h,cpp}`): the lock checks, `ProcessTx`, the
//! input-lock votes, islock creation, the recovered-signature routing and the retry.
//!
//! Threads: `async_sign_if_member` and `select_quorum_for_signing` may wait on V (SG1a), so they
//! are never called on a thread with the no-wait mark. The signals-thread hooks post
//! [`IsSigner::process_tx`] to the listener's signer queue; the recovered-signature listener runs
//! on SG1a's unmarked listener thread.
//!
//! Deviations from Core:
//! - An asset unlock is never locked: `CheckCanLockAssetUnlock` is phase H (Q3), so every
//!   platform transfer abstains.
//! - The parent's mined height is read from the block index on every check; Core's IS block-height
//!   cache is phase H.
//! - A store or signing fault is reported once to `abort` and ends the call, where Core throws;
//!   [`SigningFault::Stopped`] ends it silently.
//! - One [`TxSource`] read stands in for Core's mempool lookup and `GetTransaction`
//!   (`signing.cpp:275-293`); a parent that enters the mempool between them gets the mempool
//!   line, not the mined-block one.
//! - `IsInstantSendMempoolSigningEnabled` is one adapter read, [`IsReads::mempool_signing_enabled`].
//! - A selected quorum whose base block has no ancestor at the cycle height creates no islock,
//!   where Core dereferences the missing ancestor.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dash_consensus::primitives::{OutPoint, TxHash, TxType};
use dash_consensus::{Tx, TxRef};
use dash_evo::locks::wire::RecoveredSig;
use dash_evo::locks::{isdlock_hash, islock_request_id};
use dash_kv::StoreError;
use dash_llmq::signing::api::{
    QuorumSigning, RecoveredSigListener, RecoveredSigResult, SelectedQuorum, SignRequest,
    SigningFault,
};
use dash_num::Hash256;
use dash_params::{ChainSetup, LlmqType};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::no_wait_marked;
use parking_lot::Mutex;

use super::listener::input_lock_request_id;
use super::seams::{IsStore, SignRequestId};
use super::{IsLockHash, IsTipCache, LockedTx, lock_inputs};
use crate::signing::{IsLockView, TxSource};

/// [`TxSource`] with the transaction body: Core's `node::GetTransaction(nullptr, &mempool, ..)`.
pub trait IsTxSource: TxSource {
    /// The transaction and the block it was mined in, `None` for the mempool. Never waits on V,
    /// the signals queue or txindex catch-up; the same ordering rule as [`TxSource`].
    fn get_tx(&self, txid: &TxHash) -> dash_kv::Result<Option<(TxRef, Option<BlockHash>)>>;
    /// Waits until the transaction index has caught up with the active chain. Called only from
    /// the recovered-signature listener, never on the signals thread.
    // Core: v24.0.2 src/instantsend/signing.cpp:127-129 g_txindex->BlockUntilSyncedToCurrentChain
    fn wait_synced(&self);
}

/// The node state the IS signer reads besides the IS store and [`IsLockView`]. Every method
/// returns promptly and never waits on V or the signals queue; the listener hooks read
/// `blockchain_synced` and `has_chain_lock` on the signals thread.
pub trait IsReads: Send + Sync {
    // Core: v24.0.2 src/masternode/sync.h:79 IsBlockchainSynced
    fn blockchain_synced(&self) -> bool;
    /// `!LoadingBlocks() && GetSporkValue(SPORK_2_INSTANTSEND_ENABLED) == 0`.
    // Core: v24.0.2 src/instantsend/signing.cpp:120-123 IsInstantSendMempoolSigningEnabled
    fn mempool_signing_enabled(&self) -> bool;
    /// `LookupBlockIndex(hash)->nHeight` for any block in the index, active or not.
    fn block_height(&self, hash: &BlockHash) -> Option<i32>;
    /// `false` while ChainLocks are disabled; `true` when `hash` at `height` is the best
    /// ChainLock's block or its ancestor.
    // Core: v24.0.2 src/chainlock/chainlock.cpp:49-71 Chainlocks::HasChainLock
    fn has_chain_lock(&self, height: i32, hash: &BlockHash) -> bool;
    /// `LookupBlockIndex(hash)->GetAncestor(height)->GetBlockHash()`.
    fn ancestor_hash(&self, hash: &BlockHash, height: i32) -> Option<BlockHash>;
}

/// LK1b's pending-lock queue, for the islocks this signer completes.
pub trait IsLockSink: Send + Sync {
    /// Queues `lock` under `hash` from no peer (`id = -1`), unless the store already knows
    /// `hash` or the queue already holds it.
    // Core: v24.0.2 src/instantsend/instantsend.cpp:294-302 TryEmplacePendingLock
    fn try_emplace_pending(&self, hash: IsLockHash, lock: LockedTx) -> dash_kv::Result<()>;
}

/// What [`IsSigner`] is built from.
pub struct IsSignerParts {
    pub view: Arc<dyn IsLockView>,
    pub store: Arc<dyn IsStore>,
    pub signing: Arc<dyn QuorumSigning>,
    pub txs: Arc<dyn IsTxSource>,
    pub reads: Arc<dyn IsReads>,
    pub sink: Arc<dyn IsLockSink>,
    pub tip: IsTipCache,
    pub abort: Arc<dyn Fn(&str) + Send + Sync>,
}

/// Core's `creatingInstantSendLocks` and `txToCreatingInstantSendLocks`, under `cs_creating`.
#[derive(Default)]
struct Creating {
    by_id: HashMap<SignRequestId, LockedTx>,
    by_txid: HashMap<TxHash, SignRequestId>,
}

/// A fault under a signer call: Core's throw.
enum Fault {
    Store(StoreError),
    Signing(SigningFault),
}

impl From<StoreError> for Fault {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<SigningFault> for Fault {
    fn from(e: SigningFault) -> Self {
        Self::Signing(e)
    }
}

/// Core's `InstantSendSigner`. No leaf mutex is held across a store, [`QuorumSigning`] or
/// [`TxSource`] call.
// Core: v24.0.2 src/instantsend/signing.h:34-114 InstantSendSigner
pub struct IsSigner {
    pub(super) parts: IsSignerParts,
    /// `llmqTypeDIP0024InstantSend`.
    llmq: LlmqType,
    /// The InstantSend quorum type's `dkgInterval`; `None` for `LLMQ_NONE`.
    dkg_interval: Option<i32>,
    /// `nInstantSendConfirmationsRequired`.
    confirmations: i32,
    /// `inputRequestIds` under `cs_input_requests`.
    input_request_ids: Mutex<HashSet<SignRequestId>>,
    creating: Mutex<Creating>,
}

// Core: v24.0.2 src/instantsend/signing.cpp:37-52 IsProviderTx
fn is_provider_tx(tx: &Tx) -> bool {
    tx.is_special_tx_version()
        && matches!(
            tx.tx_type(),
            TxType::ProviderRegister
                | TxType::ProviderUpdateService
                | TxType::ProviderUpdateRegistrar
                | TxType::ProviderUpdateRevoke
                | TxType::ProviderDissolve
                | TxType::ProviderUpdateShare
                | TxType::ProviderUpdateSharedRegistrar
        )
}

/// Core's `COutPoint::ToStringShort`.
fn short(outpoint: &OutPoint) -> String {
    format!("{}-{}", outpoint.txid(), outpoint.index())
}

impl IsSigner {
    /// The signer for the chain `params`.
    pub fn new(params: &ChainSetup, parts: IsSignerParts) -> Self {
        let consensus = &params.consensus;
        let llmq = consensus.llmq_type_dip0024_instant_send;
        Self {
            parts,
            llmq,
            dkg_interval: consensus.llmq(llmq).map(|p| i32::from(p.dkg_interval)),
            confirmations: consensus.instant_send_confirmations_required,
            input_request_ids: Mutex::default(),
            creating: Mutex::default(),
        }
    }

    fn report(&self, fault: Fault) {
        let message = match fault {
            Fault::Signing(SigningFault::Stopped) => return,
            Fault::Signing(e) => e.to_string(),
            Fault::Store(e) => e.to_string(),
        };
        (self.parts.abort)(&format!("InstantSend signer: {message}"));
    }

    /// Core's `ProcessTx`: vote on the inputs of a lockable transaction, then attempt its islock.
    // Core: v24.0.2 src/instantsend/signing.cpp:360-396 ProcessTx
    pub fn process_tx(&self, tx: &Tx, retroactive: bool) {
        if let Err(fault) = self.try_process_tx(tx, retroactive) {
            self.report(fault);
        }
    }

    fn try_process_tx(&self, tx: &Tx, retroactive: bool) -> Result<(), Fault> {
        if !self.parts.view.is_enabled() || !self.parts.reads.blockchain_synced() {
            return Ok(());
        }
        if self.llmq == LlmqType::LlmqNone {
            return Ok(());
        }
        if !self.check_can_lock(tx, true)? {
            tracing::debug!(
                target: "instantsend",
                "ProcessTx -- txid={}: CheckCanLock returned false",
                tx.txid()
            );
            return Ok(());
        }
        if let Some((hash, lock)) = self.conflicting_lock(tx)? {
            tracing::info!(
                target: "instantsend",
                "ProcessTx -- txid={}: conflicts with islock {}, txid={}",
                tx.txid(),
                Hash256::from(hash),
                lock.txid()
            );
            return Ok(());
        }
        if !self.parts.reads.mempool_signing_enabled() && !retroactive {
            return Ok(());
        }
        if !self.try_sign_input_locks(tx, retroactive)? {
            return Ok(());
        }
        self.try_sign_islock(tx)
    }

    // Core: v24.0.2 src/instantsend/signing.cpp:451-502 TrySignInstantSendLock
    fn try_sign_islock(&self, tx: &Tx) -> Result<(), Fault> {
        if is_provider_tx(tx) {
            return Ok(());
        }
        let txid = tx.txid();
        let msg_hash = *txid.as_bytes();
        let inputs = lock_inputs(tx);
        for input in &inputs {
            let id = input_lock_request_id(input);
            if !self.parts.signing.has_recovered_sig(self.llmq, &id, &msg_hash)? {
                return Ok(());
            }
        }
        tracing::debug!(
            target: "instantsend",
            "TrySignInstantSendLock -- txid={txid}: got all recovered sigs, creating \
             InstantSendLock"
        );
        let id = islock_request_id(&inputs);
        if self.parts.signing.has_recovered_sig_for_id(self.llmq, &id)? {
            return Ok(());
        }
        let Some(interval) = self.dkg_interval else {
            return Ok(());
        };
        let Some(quorum) = self.select_quorum(&id)? else {
            tracing::debug!(
                target: "instantsend",
                "TrySignInstantSendLock -- failed to select quorum. islock id={}, txid={txid}",
                Hash256::from(id)
            );
            return Ok(());
        };
        let base = BlockHash::from(<[u8; 32]>::from(quorum.quorum_hash));
        let cycle_height = quorum.base_height - quorum.base_height % interval;
        let Some(cycle_hash) = self.parts.reads.ancestor_hash(&base, cycle_height) else {
            return Ok(());
        };
        {
            let mut creating = self.creating.lock();
            if creating.by_id.contains_key(&id) {
                return Ok(());
            }
            let lock = LockedTx::new(1, inputs, txid, cycle_hash, [0; 96].into());
            creating.by_id.insert(id, lock);
            creating.by_txid.entry(txid).or_insert(id);
        }
        self.sign(&SignRequest {
            llmq_type: self.llmq,
            id,
            msg_hash,
            quorum_hash: Some(quorum.quorum_hash),
            allow_resign: false,
            allow_diff_msg: false,
        })?;
        Ok(())
    }

    fn sign(&self, request: &SignRequest) -> Result<bool, SigningFault> {
        debug_assert!(!no_wait_marked(), "async_sign_if_member may wait on V");
        self.parts.signing.async_sign_if_member(request)
    }

    fn select_quorum(&self, id: &SignRequestId) -> Result<Option<SelectedQuorum>, SigningFault> {
        debug_assert!(!no_wait_marked(), "select_quorum_for_signing may wait on V");
        self.parts.signing.select_quorum_for_signing(self.llmq, id)
    }

    // Core: v24.0.2 src/instantsend/signing.cpp:83-89 ClearInputsFromQueue
    pub fn clear_inputs(&self, ids: &[SignRequestId]) {
        let mut requests = self.input_request_ids.lock();
        for id in ids {
            requests.remove(id);
        }
    }

    // Core: v24.0.2 src/instantsend/signing.cpp:91-96 ClearLockFromQueue
    pub fn clear_lock(&self, lock: &LockedTx) {
        let mut creating = self.creating.lock();
        creating.by_id.remove(&islock_request_id(lock.inputs()));
        creating.by_txid.remove(&lock.txid());
    }

    /// Core's `ProcessPendingRetryLockTxs`, for LK1b's pending worker.
    // Core: v24.0.2 src/instantsend/signing.cpp:150-188 ProcessPendingRetryLockTxs
    pub fn process_pending_retry(&self, txs: &[TxRef]) {
        if let Err(fault) = self.try_process_pending_retry(txs) {
            self.report(fault);
        }
    }

    fn try_process_pending_retry(&self, txs: &[TxRef]) -> Result<(), Fault> {
        if !self.parts.view.is_enabled() {
            return Ok(());
        }
        let mut retried = 0;
        for tx in txs {
            let txid = tx.txid();
            if self.creating.lock().by_txid.contains_key(&txid) {
                continue;
            }
            if self.parts.view.is_locked(&txid)? || self.conflicting_lock(tx)?.is_some() {
                continue;
            }
            if tracing::enabled!(target: "instantsend", tracing::Level::DEBUG) {
                if !self.check_can_lock(tx, false)? {
                    continue;
                }
                tracing::debug!(
                    target: "instantsend",
                    "ProcessPendingRetryLockTxs -- txid={txid}: retrying to lock"
                );
            }
            self.try_process_tx(tx, false)?;
            retried += 1;
        }
        if retried != 0 {
            tracing::debug!(
                target: "instantsend",
                "ProcessPendingRetryLockTxs -- retried {retried} TXs."
            );
        }
        Ok(())
    }

    // Core: v24.0.2 src/instantsend/signing.cpp:125-148 HandleNewInputLockRecoveredSig
    fn handle_input_lock_sig(&self, sig: &RecoveredSig, txid: TxHash) -> Result<(), Fault> {
        debug_assert!(!no_wait_marked(), "wait_synced waits on the validation queue");
        self.parts.txs.wait_synced();
        let Some((tx, _)) = self.parts.txs.get_tx(&txid)? else {
            return Ok(());
        };
        if tracing::enabled!(target: "instantsend", tracing::Level::DEBUG)
            && let Some(input) =
                lock_inputs(&tx).iter().find(|i| input_lock_request_id(i) == sig.id)
        {
            tracing::debug!(
                target: "instantsend",
                "HandleNewInputLockRecoveredSig -- txid={txid}: got recovered sig for input {}",
                short(input)
            );
        }
        self.try_sign_islock(&tx)
    }

    // Core: v24.0.2 src/instantsend/signing.cpp:334-358 HandleNewInstantSendLockRecoveredSig
    fn handle_islock_sig(&self, sig: &RecoveredSig) -> Result<(), Fault> {
        let lock = {
            let mut creating = self.creating.lock();
            let Some(lock) = creating.by_id.remove(&sig.id) else {
                return Ok(());
            };
            creating.by_txid.remove(&lock.txid());
            lock
        };
        if *lock.txid().as_bytes() != sig.msg_hash {
            tracing::info!(
                target: "instantsend",
                "HandleNewInstantSendLockRecoveredSig -- txid={}: islock conflicts with {}, \
                 dropping own version",
                lock.txid(),
                Hash256::from(sig.msg_hash)
            );
            return Ok(());
        }
        let lock = LockedTx::new(
            lock.version(),
            lock.inputs().clone(),
            lock.txid(),
            lock.cycle_hash(),
            sig.sig.into(),
        );
        Ok(self.parts.sink.try_emplace_pending(isdlock_hash(&lock), lock)?)
    }

    /// Core's `GetConflictingLock`: the committed store, behind `IsInstantSendEnabled`.
    // Core: v24.0.2 src/instantsend/instantsend.cpp:428-445 GetConflictingLock
    fn conflicting_lock(&self, tx: &Tx) -> Result<Option<(IsLockHash, LockedTx)>, Fault> {
        if !self.parts.view.is_enabled() {
            return Ok(None);
        }
        Ok(self.parts.store.conflicting_lock(tx)?)
    }

    // Core: v24.0.2 src/instantsend/signing.cpp:190-210 CheckCanLock
    fn check_can_lock(&self, tx: &Tx, print_debug: bool) -> Result<bool, Fault> {
        if tx.is_platform_transfer() {
            if print_debug {
                tracing::debug!(
                    target: "instantsend",
                    "CheckCanLockAssetUnlock -- txid={}: asset unlock not lockable: phase H",
                    tx.txid()
                );
            }
            return Ok(false);
        }
        if is_provider_tx(tx) {
            if print_debug {
                tracing::debug!(
                    target: "instantsend",
                    "CheckCanLock -- txid={}: provider txs are not locked",
                    tx.txid()
                );
            }
            return Ok(false);
        }
        if tx.inputs().is_empty() {
            return Ok(false);
        }
        let txid = tx.txid();
        for input in tx.inputs() {
            if !self.check_can_lock_outpoint(&input.prevout(), print_debug, &txid)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // Core: v24.0.2 src/instantsend/signing.cpp:265-332 CheckCanLock
    fn check_can_lock_outpoint(
        &self,
        outpoint: &OutPoint,
        print_debug: bool,
        txid: &TxHash,
    ) -> Result<bool, Fault> {
        let parent = outpoint.txid();
        if self.parts.view.is_locked(&parent)? {
            return Ok(true);
        }
        let block = match self.parts.txs.get_transaction(&parent)? {
            Some(Some(block)) => block,
            Some(None) => {
                if print_debug {
                    tracing::debug!(
                        target: "instantsend",
                        "CheckCanLock -- txid={txid}: parent mempool TX {parent} is not locked"
                    );
                }
                return Ok(false);
            }
            None => {
                if print_debug {
                    tracing::debug!(
                        target: "instantsend",
                        "CheckCanLock -- txid={txid}: failed to find parent TX {parent} in mined \
                         block"
                    );
                }
                return Ok(false);
            }
        };
        let Some(block_height) = self.parts.reads.block_height(&block) else {
            if print_debug {
                tracing::debug!(
                    target: "instantsend",
                    "CheckCanLock -- txid={txid}: failed to determine mined height for parent TX \
                     {parent}"
                );
            }
            return Ok(false);
        };
        let tip_height = self.parts.tip.get();
        if tip_height < block_height {
            if print_debug {
                tracing::debug!(
                    target: "instantsend",
                    "CheckCanLock -- txid={txid}: cached tip height {tip_height} is below block \
                     height {block_height} for parent TX {parent}"
                );
            }
            return Ok(false);
        }
        let age = tip_height - block_height + 1;
        if age < self.confirmations && !self.parts.reads.has_chain_lock(block_height, &block) {
            if print_debug {
                tracing::debug!(
                    target: "instantsend",
                    "CheckCanLock -- txid={txid}: outpoint {} too new and not ChainLocked. \
                     nTxAge={age}, nInstantSendConfirmationsRequired={}",
                    short(outpoint),
                    self.confirmations
                );
            }
            return Ok(false);
        }
        Ok(true)
    }

    // Core: v24.0.2 src/instantsend/signing.cpp:398-449 TrySignInputLocks
    fn try_sign_input_locks(&self, tx: &Tx, retroactive: bool) -> Result<bool, Fault> {
        let txid = tx.txid();
        let msg_hash = *txid.as_bytes();
        let inputs = lock_inputs(tx);
        let ids: Vec<SignRequestId> = inputs.iter().map(input_lock_request_id).collect();
        let mut already_voted = 0;
        for (outpoint, id) in inputs.iter().zip(&ids) {
            if let Some(other) = self.parts.signing.get_vote_for_id(self.llmq, id)? {
                if other != msg_hash {
                    tracing::info!(
                        target: "instantsend",
                        "TrySignInputLocks -- txid={txid}: input {} is conflicting with previous \
                         vote for tx {}",
                        short(outpoint),
                        TxHash::from(other)
                    );
                    return Ok(false);
                }
                already_voted += 1;
            }
            if self.parts.signing.is_conflicting(self.llmq, id, &msg_hash)? {
                tracing::info!(
                    target: "instantsend",
                    "TrySignInputLocks -- txid={txid}: m_sigman.IsConflicting returned true. id={}",
                    Hash256::from(*id)
                );
                return Ok(false);
            }
        }
        if !retroactive && already_voted == ids.len() {
            tracing::debug!(
                target: "instantsend",
                "TrySignInputLocks -- txid={txid}: already voted on all inputs, bailing out"
            );
            return Ok(true);
        }
        tracing::debug!(
            target: "instantsend",
            "TrySignInputLocks -- txid={txid}: trying to vote on {} inputs",
            inputs.len()
        );
        for (outpoint, id) in inputs.iter().zip(&ids) {
            self.input_request_ids.lock().insert(*id);
            tracing::debug!(
                target: "instantsend",
                "TrySignInputLocks -- txid={txid}: trying to vote on input {} with id {}. \
                 fRetroactive={}",
                short(outpoint),
                Hash256::from(*id),
                u8::from(retroactive)
            );
            let request = SignRequest {
                llmq_type: self.llmq,
                id: *id,
                msg_hash,
                quorum_hash: None,
                allow_resign: retroactive,
                allow_diff_msg: false,
            };
            if self.sign(&request)? {
                tracing::debug!(
                    target: "instantsend",
                    "TrySignInputLocks -- txid={txid}: voted on input {} with id {}",
                    short(outpoint),
                    Hash256::from(*id)
                );
            }
        }
        Ok(true)
    }
}

/// Every result is `Nothing`: the completed islock goes to LK1b's pending queue.
// Core: v24.0.2 src/instantsend/signing.cpp:98-118 HandleNewRecoveredSig
impl RecoveredSigListener for IsSigner {
    fn handle_new_recovered_sig(&self, sig: &RecoveredSig) -> RecoveredSigResult {
        if !self.parts.view.is_enabled() || self.llmq == LlmqType::LlmqNone {
            return RecoveredSigResult::Nothing;
        }
        let input_txid = self
            .input_request_ids
            .lock()
            .contains(&sig.id)
            .then_some(sig.msg_hash)
            .filter(|txid| *txid != [0; 32]);
        let result = if let Some(txid) = input_txid {
            self.handle_input_lock_sig(sig, TxHash::from(txid))
        } else if self.creating.lock().by_id.contains_key(&sig.id) {
            self.handle_islock_sig(sig)
        } else {
            Ok(())
        };
        if let Err(fault) = result {
            self.report(fault);
        }
        RecoveredSigResult::Nothing
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "signer_tests.rs"]
pub(crate) mod tests;
