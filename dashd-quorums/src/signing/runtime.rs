//! Core's `CSigningManager` behind `NetSigning`, without the worker thread: the cross-lane
//! signing API, the recovered-signature intake and one pending pass.

use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};

use dash_clock::Clock;
use dash_evo::locks::wire::RecoveredSig;
use dash_llmq::dkg::bls::BlsCtx;
use dash_llmq::sigenv::{
    PeerId, QuorumLookup, RecSigReads, SignRequest, SignerFacts, SigningQuorum,
    select_for_signing_default,
};
use dash_llmq::signing::api::{
    ListenerId, QuorumSigning, RecoveredSigIntake, RecoveredSigListener, RecoveredSigResult,
    SelectedQuorum, SigningFault,
};
use dash_llmq::signing::db::{RecSigDb, RecSigKv};
use dash_llmq::signing::intake::{IntakeStatus, RequestCompletion, intake_gates, read_qsigrec};
use dash_llmq::signing::manager::{
    BadSources, BatchKeys, PendingBatch, Settled, SigningError, SigningEvent, SigningManager,
    resolve_batch_keys, settle_verified, verify_pending,
};
use dash_llmq::signing::sign::{SignDecision, signing_quorum};
use dash_p2p_core::Inventory;
use dash_params::{ChainSetup, LlmqType};
use dash_primitives::TxHash;
use dashd_chainstate::chain::reply::no_wait_marked;
use rand_core::Rng;

use super::worker::RecSigsWork;
use crate::traits::LocalMasternode;

/// Core's recovered-signature batch size per pass.
// Core: v24.0.2 src/llmq/net_signing.cpp:231
const MAX_BATCH: usize = 32;
/// A lookup-bearing call from a no-wait task; the quorum lookup may wait on V.
const NO_WAIT_LOOKUP: &str = "signing quorum lookup from a no-wait task: it may wait on V";
/// Core's `BanNode` score.
// Core: v24.0.2 src/llmq/net_signing.cpp:311
const BAN_SCORE: i32 = 100;

/// The peer manager calls the runtime makes.
pub trait SigningNet: Send + Sync {
    /// Core's `PeerMisbehaving`.
    fn misbehaving(&self, peer: PeerId, score: i32);
    /// Core's `PeerRelayInv`.
    fn relay_inv(&self, inv: &Inventory);
    /// Core's `PeerRelayTransaction`.
    fn relay_transaction(&self, txid: &TxHash);
}

/// Core's queued `GetMainSignals().NotifyRecoveredSig`.
pub trait RecSigNotify: Send + Sync {
    fn notify_recovered_sig(&self, sig: Arc<RecoveredSig>, proactive_relay: bool);
}

/// The sig-share manager's half of `AsyncSignIfMember`, called with no signing lock held.
// Core: v24.0.2 src/llmq/signing_shares.cpp:869-873
pub trait SignQueue<Q>: Send + Sync {
    fn force_reannouncement(&self, quorum: &Arc<Q>, t: LlmqType, id: &[u8; 32], msg: &[u8; 32]);
    fn async_sign(&self, quorum: Arc<Q>, id: [u8; 32], msg: [u8; 32]);
}

/// The quorum source the runtime signs and verifies against. The runtime calls it with no
/// signing lock held.
pub trait SigningLookup: QuorumLookup + Send + Sync {
    /// The height of the quorum's base block.
    fn base_height(&self, quorum: &Self::Quorum) -> i32;
}

/// The runtime's handles into the rest of the node.
pub struct SigningHandles {
    pub net: Arc<dyn SigningNet>,
    pub notify: Arc<dyn RecSigNotify>,
    pub masternode: Arc<dyn LocalMasternode + Send + Sync>,
    /// Mock-aware node time, Core's `GetTime`.
    pub clock: Arc<dyn Clock>,
}

/// The node's signing manager.
///
/// One mutex serializes `ProcessRecoveredSig`, which Core runs concurrently. The mutex is
/// never held across a quorum lookup, BLS verification or a listener call, as Core keeps them
/// outside `cs_pending`, so every read, including [`RecSigReads`] from the sig-share workers,
/// waits at most for one storage operation.
pub struct SigningRuntime<K, L: SigningLookup> {
    manager: Mutex<SigningManager<K>>,
    listeners: RwLock<Vec<(ListenerId, Arc<dyn RecoveredSigListener>)>>,
    next_listener: AtomicU64,
    lookup: Arc<L>,
    chain: Arc<ChainSetup>,
    handles: SigningHandles,
    sign_queue: OnceLock<Arc<dyn SignQueue<L::Quorum>>>,
    signing_allowed: bool,
}

fn storage(e: impl Debug) -> SigningFault {
    SigningFault::Storage(format!("{e:?}"))
}

fn lookup_fault(e: impl Debug) -> SigningFault {
    SigningFault::Lookup(format!("{e:?}"))
}

fn intake_fault<D: Debug, E: Debug>(e: SigningError<D, E>) -> SigningFault {
    match e {
        SigningError::Storage(e) => storage(e),
        SigningError::Lookup(e) => lookup_fault(e),
    }
}

impl<K, L> SigningRuntime<K, L>
where
    K: RecSigKv + Send,
    K::Error: Debug,
    L: SigningLookup,
{
    /// Quorum signing is allowed until [`Self::with_signing_allowed`] says otherwise. No
    /// [`SignQueue`] is set until [`Self::set_sign_queue`].
    pub fn new(
        manager: SigningManager<K>,
        lookup: Arc<L>,
        chain: Arc<ChainSetup>,
        handles: SigningHandles,
    ) -> Self {
        Self {
            manager: Mutex::new(manager),
            listeners: RwLock::new(Vec::new()),
            next_listener: AtomicU64::new(0),
            lookup,
            chain,
            handles,
            sign_queue: OnceLock::new(),
            signing_allowed: true,
        }
    }

    pub fn chain(&self) -> &Arc<ChainSetup> {
        &self.chain
    }

    /// Sets the sig-share manager's queue, once, at composition. A second call is a programming
    /// error: it changes nothing and returns the rejected queue. Until a queue is set,
    /// [`QuorumSigning::async_sign_if_member`] returns false before any vote rule, so no vote is
    /// written.
    pub fn set_sign_queue(
        &self,
        queue: Arc<dyn SignQueue<L::Quorum>>,
    ) -> Result<(), Arc<dyn SignQueue<L::Quorum>>> {
        self.sign_queue.set(queue)
    }

    /// Core's `IsQuorumSigningAllowed`: false while snapshot background validation runs.
    // Core: v24.0.2 src/llmq/signing_shares.cpp:1561 IsQuorumSigningAllowed
    pub fn with_signing_allowed(mut self, allowed: bool) -> Self {
        self.signing_allowed = allowed;
        self
    }

    fn manager(&self) -> MutexGuard<'_, SigningManager<K>> {
        self.manager.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read<T>(
        &self,
        f: impl FnOnce(&RecSigDb<K>) -> Result<T, K::Error>,
    ) -> Result<T, SigningFault> {
        f(self.manager().db()).map_err(storage)
    }

    fn now_s(&self) -> i64 {
        self.handles.clock.node_now().secs()
    }

    // Core: v24.0.2 src/llmq/net_signing.cpp:47-62 ProcessMessage (QSIGREC), :307-320 BanNode;
    // src/llmq/signing.cpp:422-475 VerifyAndProcessRecoveredSig
    pub fn on_qsigrec(
        &self,
        peer: PeerId,
        wire: &[u8],
        legacy: bool,
    ) -> (Option<RequestCompletion>, Result<IntakeStatus, SigningFault>) {
        debug_assert!(!no_wait_marked(), "{NO_WAIT_LOOKUP}");
        let (completion, sig) = read_qsigrec(peer, wire, &self.chain);
        let status = match sig {
            Err(IntakeStatus::BanUnknownType) => {
                if peer != -1 {
                    self.handles.net.misbehaving(peer, BAN_SCORE);
                    self.remove_node(peer);
                }
                Ok(IntakeStatus::BanUnknownType)
            }
            Err(status) => Ok(status),
            Ok(sig) => self.on_recovered_sig(peer, sig, legacy),
        };
        (completion, status)
    }

    /// A `qsigrec` the P2P handler has already read, completed and type-checked.
    // Core: v24.0.2 src/llmq/net_signing.cpp:61; src/llmq/signing.cpp:422-475
    // VerifyAndProcessRecoveredSig
    pub fn on_recovered_sig(
        &self,
        peer: PeerId,
        sig: Arc<RecoveredSig>,
        legacy: bool,
    ) -> Result<IntakeStatus, SigningFault> {
        debug_assert!(!no_wait_marked(), "{NO_WAIT_LOOKUP}");
        let known = |h: &[u8; 32]| self.manager().db().has_for_hash(h);
        match intake_gates(&sig, &*self.lookup, known) {
            Ok(None) => Ok(self.manager().enqueue(peer, sig, BlsCtx::from_legacy(legacy))),
            Ok(Some(status)) => Ok(status),
            Err(e) => Err(intake_fault(e)),
        }
    }

    // Core: v24.0.2 src/llmq/signing.cpp:395-408 AlreadyHave
    pub fn already_have(&self, hash: &[u8; 32]) -> Result<bool, SigningFault> {
        self.manager().already_have(hash).map_err(storage)
    }

    // Core: v24.0.2 src/llmq/signing.cpp:410-420 GetRecoveredSigForGetData
    pub fn for_getdata(&self, hash: &[u8; 32]) -> Result<Option<RecoveredSig>, SigningFault> {
        debug_assert!(!no_wait_marked(), "{NO_WAIT_LOOKUP}");
        let Some(sig) = self.read(|db| db.get_by_hash(hash))? else {
            return Ok(None);
        };
        let active =
            self.lookup.is_quorum_active(sig.llmq_type, &sig.quorum_hash).map_err(lookup_fault)?;
        Ok(active.then_some(sig))
    }

    /// Returns whether a pass ran; at most one per five seconds.
    // Core: v24.0.2 src/llmq/signing.cpp:630-634 Cleanup; src/llmq/net_signing.cpp:288-291
    pub fn cleanup(&self) -> Result<bool, SigningFault> {
        let now_ms = self.handles.clock.node_now_micros().div_euclid(1000);
        self.manager().cleanup(now_ms).map_err(storage)
    }

    /// One pending pass; returns Core's `more_work`.
    // Core: v24.0.2 src/llmq/net_signing.cpp:220-281 ProcessPendingRecoveredSigs
    pub fn process_pending_recovered_sigs(
        &self,
        legacy: bool,
        rng: &mut impl Rng,
    ) -> Result<bool, SigningFault> {
        let check = BlsCtx::from_legacy(legacy);
        self.pending_pass(rng, |batch, keys| verify_pending(batch, keys, check))
    }

    fn pending_pass(
        &self,
        rng: &mut impl Rng,
        verify: impl FnOnce(&PendingBatch, &BatchKeys) -> BadSources,
    ) -> Result<bool, SigningFault> {
        debug_assert!(!no_wait_marked(), "{NO_WAIT_LOOKUP}");
        let reconstructed = self.manager().take_reconstructed();
        for sig in reconstructed {
            self.process_recovered_sig(sig, true)?;
        }
        let mut batch = self.manager().collect_pending(MAX_BATCH, rng).map_err(storage)?;
        if batch.by_source.is_empty() {
            return Ok(false);
        }
        let mut dropped = Vec::<SigningEvent<K::Error, L::Error>>::new();
        let keys = resolve_batch_keys(&mut batch, &*self.lookup, &mut dropped);
        for event in dropped {
            if let SigningEvent::LookupError {
                error,
                ..
            } = event
            {
                return Err(lookup_fault(error));
            }
            tracing::debug!(?event, "recovered sig dropped before verification");
        }
        let more_work = batch.more_work;
        let bad_sources = verify(&batch, &keys);
        for step in settle_verified(batch, &bad_sources) {
            match step {
                Settled::Ban(peer) => {
                    self.handles.net.misbehaving(peer, BAN_SCORE);
                    self.remove_node(peer);
                }
                Settled::Process {
                    sig,
                    from_self,
                    ..
                } => {
                    self.process_recovered_sig(sig, from_self)?;
                }
            }
        }
        Ok(more_work)
    }
}

/// The "recsigs" worker's work over a runtime. Each pass verifies under the scheme `legacy`
/// gives at its start, Core's global at the lazy `sig.Get()`, and draws `IterateNodesRandom`'s
/// order from `rng`.
// Core: v24.0.2 src/llmq/net_signing.cpp:243-247; src/llmq/signing.cpp:488-540
pub struct RuntimeWork<K, L: SigningLookup, R> {
    runtime: Arc<SigningRuntime<K, L>>,
    legacy: Box<dyn Fn() -> bool + Send + Sync>,
    rng: Mutex<R>,
}

impl<K, L: SigningLookup, R> RuntimeWork<K, L, R> {
    pub fn new(
        runtime: Arc<SigningRuntime<K, L>>,
        legacy: Box<dyn Fn() -> bool + Send + Sync>,
        rng: R,
    ) -> Self {
        Self {
            runtime,
            legacy,
            rng: Mutex::new(rng),
        }
    }
}

impl<K, L, R> RecSigsWork for RuntimeWork<K, L, R>
where
    K: RecSigKv + Send + 'static,
    K::Error: Debug,
    L: SigningLookup + 'static,
    R: Rng + Send + 'static,
{
    fn process_pending(&self) -> Result<bool, SigningFault> {
        let mut rng = self.rng.lock().unwrap_or_else(PoisonError::into_inner);
        self.runtime.process_pending_recovered_sigs((self.legacy)(), &mut *rng)
    }

    fn cleanup(&self) -> Result<bool, SigningFault> {
        self.runtime.cleanup()
    }
}

impl<K, L> QuorumSigning for SigningRuntime<K, L>
where
    K: RecSigKv + Send,
    K::Error: Debug,
    L: SigningLookup,
{
    // Core: v24.0.2 src/llmq/signing_shares.cpp:792-876 CSigSharesManager::AsyncSignIfMember
    fn async_sign_if_member(&self, req: &SignRequest) -> Result<bool, SigningFault> {
        debug_assert!(!no_wait_marked(), "{NO_WAIT_LOOKUP}");
        let facts = SignerFacts {
            my_pro_tx: self.handles.masternode.pro_tx_hash(),
            signing_allowed: self.signing_allowed,
        };
        let quorum = match signing_quorum(&*self.lookup, &facts, req).map_err(lookup_fault)? {
            Ok(quorum) => quorum,
            Err(_) => return Ok(false),
        };
        // Core: v24.0.2 src/llmq/signing_shares.cpp:862-873: the vote is written only on the path
        // that goes on to AsyncSign.
        let Some(queue) = self.sign_queue.get() else {
            return Ok(false);
        };
        let now_s = self.now_s();
        let decision = self.manager().vote_decision(quorum, req, now_s).map_err(storage)?;
        let SignDecision::Sign {
            quorum,
            force_reannounce,
        } = decision
        else {
            return Ok(decision.core_result());
        };
        if force_reannounce {
            queue.force_reannouncement(&quorum, req.llmq_type, &req.id, &req.msg_hash);
        }
        queue.async_sign(quorum, req.id, req.msg_hash);
        Ok(true)
    }

    // Core: v24.0.2 src/llmq/quorumsman.h:222-223 SelectQuorumForSigning
    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
    ) -> Result<Option<SelectedQuorum>, SigningFault> {
        debug_assert!(!no_wait_marked(), "{NO_WAIT_LOOKUP}");
        let quorum = select_for_signing_default(&*self.lookup, t, id).map_err(lookup_fault)?;
        Ok(quorum.map(|q| SelectedQuorum {
            quorum_hash: *q.quorum_hash(),
            base_height: self.lookup.base_height(&q),
        }))
    }

    fn has_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.read(|db| db.has_recovered_sig(t, id, msg))
    }

    fn has_recovered_sig_for_id(&self, t: LlmqType, id: &[u8; 32]) -> Result<bool, SigningFault> {
        self.read(|db| db.has_for_id(t, id))
    }

    fn has_recovered_sig_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, SigningFault> {
        self.read(|db| db.has_for_session(sign_hash))
    }

    fn has_recovered_sig_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.read(|db| db.has_for_signing(t, id, msg))
    }

    fn get_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, SigningFault> {
        self.read(|db| db.get(t, id, msg))
    }

    fn is_conflicting(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.manager().is_conflicting(t, id, msg).map_err(storage)
    }

    fn get_vote_for_id(
        &self,
        t: LlmqType,
        id: &[u8; 32],
    ) -> Result<Option<[u8; 32]>, SigningFault> {
        self.read(|db| db.vote_for(t, id))
    }

    fn truncate_recovered_sig(&self, t: LlmqType, id: &[u8; 32]) -> Result<(), SigningFault> {
        self.manager().truncate(t, id).map_err(storage)
    }

    fn register_listener(&self, l: Arc<dyn RecoveredSigListener>) -> ListenerId {
        let id = self.next_listener.fetch_add(1, Ordering::Relaxed);
        self.listeners.write().unwrap_or_else(PoisonError::into_inner).push((id, l));
        id
    }

    fn unregister_listener(&self, id: ListenerId) {
        self.listeners.write().unwrap_or_else(PoisonError::into_inner).retain(|(l, _)| *l != id);
    }
}

impl<K, L> RecoveredSigIntake for SigningRuntime<K, L>
where
    K: RecSigKv + Send,
    K::Error: Debug,
    L: SigningLookup,
{
    fn process_recovered_sig(
        &self,
        sig: Arc<RecoveredSig>,
        consider_proactive_relay: bool,
    ) -> Result<bool, SigningFault> {
        let now_s = self.now_s();
        let stored = self.manager().process_recovered(sig, consider_proactive_relay, now_s);
        let Some(event) = stored.map_err(storage)? else {
            return Ok(false);
        };
        debug_assert!(
            !no_wait_marked(),
            "recovered-sig listener dispatch from a no-wait task: listeners may wait on V"
        );
        let listeners: Vec<_> = self
            .listeners
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect();
        for listener in listeners {
            match listener.handle_new_recovered_sig(&event.sig) {
                RecoveredSigResult::Nothing => {}
                RecoveredSigResult::RelayInv(inv) => self.handles.net.relay_inv(&inv),
                RecoveredSigResult::RelayTx(txid) => self.handles.net.relay_transaction(&txid),
            }
        }
        self.handles.notify.notify_recovered_sig(event.sig, event.proactive_relay);
        Ok(true)
    }

    fn push_reconstructed_recovered_sig(&self, sig: Arc<RecoveredSig>) {
        self.manager().push_reconstructed(sig);
    }

    fn remove_node(&self, peer: PeerId) {
        self.manager().remove_node(peer);
    }
}

/// What the P2P processor's signing seam reaches on the runtime, beyond [`RecoveredSigIntake`].
pub trait RecoveredSigNet: RecoveredSigIntake {
    fn chain(&self) -> &ChainSetup;
    /// [`SigningRuntime::on_recovered_sig`].
    fn on_recovered_sig(
        &self,
        peer: PeerId,
        sig: Arc<RecoveredSig>,
        legacy: bool,
    ) -> Result<IntakeStatus, SigningFault>;
    /// [`SigningRuntime::already_have`].
    fn already_have(&self, hash: &[u8; 32]) -> Result<bool, SigningFault>;
    /// [`SigningRuntime::for_getdata`].
    fn for_getdata(&self, hash: &[u8; 32]) -> Result<Option<RecoveredSig>, SigningFault>;
}

impl<K, L> RecoveredSigNet for SigningRuntime<K, L>
where
    K: RecSigKv + Send,
    K::Error: Debug,
    L: SigningLookup,
{
    fn chain(&self) -> &ChainSetup {
        &self.chain
    }

    fn on_recovered_sig(
        &self,
        peer: PeerId,
        sig: Arc<RecoveredSig>,
        legacy: bool,
    ) -> Result<IntakeStatus, SigningFault> {
        SigningRuntime::on_recovered_sig(self, peer, sig, legacy)
    }

    fn already_have(&self, hash: &[u8; 32]) -> Result<bool, SigningFault> {
        SigningRuntime::already_have(self, hash)
    }

    fn for_getdata(&self, hash: &[u8; 32]) -> Result<Option<RecoveredSig>, SigningFault> {
        SigningRuntime::for_getdata(self, hash)
    }
}

impl<K, L> RecSigReads for SigningRuntime<K, L>
where
    K: RecSigKv + Send,
    K::Error: Debug,
    L: SigningLookup,
{
    type Error = SigningFault;

    // Core: v24.0.2 src/llmq/signing.cpp:664-668 CSigningManager::HasRecoveredSigForSigning
    fn has_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.read(|db| db.has_for_signing(t, id, msg))
    }

    // Core: v24.0.2 src/llmq/signing.cpp:659-662 CSigningManager::HasRecoveredSigForSession
    fn has_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, SigningFault> {
        self.read(|db| db.has_for_session(sign_hash))
    }
}

#[cfg(test)]
mod tests;
