//! `NetInstantSend`'s batch verification of the received queue (PLAN-LK1b §5 item 4, §6 S3):
//! `BuildVerificationBatch`, `ProcessPendingInstantSendLocks`, `ApplyVerificationResults` and the
//! two passes of `ProcessPendingISLocks`, through the fork's weighted [`BlsBatchVerifier`] built
//! as Core builds its `CBLSBatchVerifier<NodeId, uint256>{false, true, 8}`.
//!
//! The fork's verifier keeps its bad sets to itself, so the two the build adds (a source whose
//! signature is unreadable or whose cycle is unknown, a message with no quorum) are kept beside
//! it, and every read takes the union. Core's `Verify` writes its sets but reads only
//! `badMessages`, to skip re-checking a message one by one; a message the build marked bad is
//! never pushed under its id (the queue holds each hash once, and the lock's bytes alone decide
//! its quorum lookup), so the skip changes no set and the union is Core's sets.
//!
//! A pass reads one quorum view and one sample of the BLS flag ([`IsPassView`]), taken when the
//! pass starts: Core reads the tip and quorums under `cs_main` per lock, and the worker owns the
//! schedule of a pass's reads, as LQ3a D6 and D22 have it for the two passes. Only those reads
//! ([`NetInstantSend::batch_targets`]) run in the view's turn; the verifier's pairings, the
//! recovered-signature reads and the processing of each lock run after it, as Core runs them
//! outside `cs_main`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_evo::locks::wire::{RecoveredSig, SigRead, read_lazy_sig};
use dash_evo::locks::{
    LockError, Pass, PrecheckFail, QuorumScanView, islock_precheck, islock_sign_target,
};
use dash_num::Hash256;
use dash_params::{ChainSetup, LlmqType};
use dash_pkc::bls::core::{BlsBatchVerifier, BlsPkValue, BlsSigValue};
use dash_primitives::QuorumHash;
use dash_types::Numeric;
use rand_core::CryptoRng;

use super::IsLockHash;
use super::manager::{NodeId, PendingIsLockEntry};
use super::net::{IsNetError, NetInstantSend};

/// The sub-batch size: the verifier checks and clears its messages at 8 distinct ids.
/// Core: `instantsend/net_instantsend.cpp:33`, `:58`.
pub const BATCH_VERIFIER_SOURCE_THRESHOLD: usize = 8;

/// The score of a source whose lock failed on the previous active set too.
/// Core: `instantsend/net_instantsend.cpp:36`.
pub const OLD_ACTIVE_SET_FAILURE_MISBEHAVIOR_SCORE: i32 = 20;

/// A pass's quorum view and BLS flag, which S6 builds as a [`super::verify::NodeQuorumView`] with
/// the InstantSend tip cache ([`super::verify::NodeQuorumView::with_is_tip`]) in one `OnV` turn.
/// `f` only reads the view, and owns what it reads besides, so the turn may run on V.
pub trait IsPassView: Send + Sync {
    /// The view's read error.
    type Error: fmt::Debug + Send + 'static;

    /// Runs `f` with a view and the flag sampled now.
    ///
    /// # Errors
    ///
    /// The view could not be built, or `f`'s error.
    fn with_pass<T: Send + 'static>(
        &self,
        f: impl PassBody<Self::Error, T>,
    ) -> Result<T, IsNetError<Self::Error>>;
}

/// A pass's body over its view and BLS flag, which the turn running it owns.
pub trait PassBody<E, T>:
    FnOnce(&dyn QuorumScanView<Error = E>, BlsCtx) -> Result<T, IsNetError<E>> + Send + 'static
{
}

impl<E, T, F> PassBody<E, T> for F where
    F: FnOnce(&dyn QuorumScanView<Error = E>, BlsCtx) -> Result<T, IsNetError<E>> + Send + 'static
{
}

/// `NetInstantSend::BatchVerificationData`. Core: `instantsend/net_instantsend.cpp:57-62`.
pub struct BatchVerificationData {
    verifier: BlsBatchVerifier<NodeId, IsLockHash>,
    /// The sources `BuildVerificationBatch` put in `badSources`.
    bad_sources: BTreeSet<NodeId>,
    /// The messages `BuildVerificationBatch` put in `badMessages`.
    bad_messages: BTreeSet<IsLockHash>,
    /// `recSigs`: the recovered signature to hand on for each pushed lock whose request id has
    /// none, first entry kept (`try_emplace`).
    rec_sigs: BTreeMap<IsLockHash, RecoveredSig>,
    /// `verifyCount`: the locks pushed.
    pub verify_count: usize,
    /// `alreadyVerified`: the locks whose recovered signature is already stored.
    pub already_verified: usize,
}

impl BatchVerificationData {
    fn new(scheme: BlsCtx) -> Self {
        BatchVerificationData {
            verifier: BlsBatchVerifier::new(
                false,
                true,
                BATCH_VERIFIER_SOURCE_THRESHOLD,
                scheme.scheme(),
            ),
            bad_sources: BTreeSet::new(),
            bad_messages: BTreeSet::new(),
            rec_sigs: BTreeMap::new(),
            verify_count: 0,
            already_verified: 0,
        }
    }

    /// `batchVerifier.badSources.count(node)`.
    pub fn source_bad(&self, node: NodeId) -> bool {
        self.bad_sources.contains(&node) || self.verifier.bad_sources().contains(&node)
    }

    /// `batchVerifier.badMessages.count(hash)`.
    pub fn message_bad(&self, hash: &IsLockHash) -> bool {
        self.bad_messages.contains(hash) || self.verifier.bad_messages().contains(hash)
    }

    /// `batchVerifier.GetUniqueSourceCount()`: the sources of the last sub-batch.
    pub fn unique_source_count(&self) -> usize {
        self.verifier.unique_source_count()
    }

    /// The recovered signature kept for `hash`.
    pub fn rec_sig(&self, hash: &IsLockHash) -> Option<&RecoveredSig> {
        self.rec_sigs.get(hash)
    }
}

/// What one lock's view reads give its batch entry ([`NetInstantSend::batch_targets`]).
#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "one per lock of a batch of at most 32, consumed at once: a box would allocate per lock"
)]
pub enum BatchTarget {
    /// `badSources.emplace(nodeId)`: an unreadable signature or an unknown cycle (:122-134).
    BadSource,
    /// `badMessages.emplace(hash)`: no quorum (:148-151).
    BadMessage,
    /// The pass's quorum and what `PushMessage` takes (:153-161).
    Push {
        llmq_type: LlmqType,
        quorum_hash: QuorumHash,
        id: [u8; 32],
        sign_hash: [u8; 32],
        sig: BlsSigValue,
        pk: BlsPkValue,
    },
}

/// `badSources.emplace` for a build-time precheck failure. Core's build checks only the
/// signature (`:122-126`) and the cycle block (`:130-134`); a queued lock passed the message
/// checks, whose answers cannot change, so the other two are unreachable here and are also the
/// source's fault.
fn precheck_bad_source<E>(fail: PrecheckFail<E>) -> Result<(), IsNetError<E>> {
    match fail {
        PrecheckFail::MalformedSignature
        | PrecheckFail::UnknownCycle
        | PrecheckFail::Malformed
        | PrecheckFail::InvalidCycle => Ok(()),
        PrecheckFail::UnknownLlmqType(t) => Err(IsNetError::UnknownLlmqType(t)),
        PrecheckFail::View(e) => Err(IsNetError::View(e)),
    }
}

/// [`NetInstantSend::batch_targets`] over the chain's `params`.
fn batch_targets<V: QuorumScanView + ?Sized>(
    params: &ChainSetup,
    view: &V,
    pass: Pass,
    pend: &[PendingIsLockEntry],
) -> Result<Vec<BatchTarget>, IsNetError<V::Error>> {
    let mut targets = Vec::with_capacity(pend.len());
    for pending in pend {
        let islock = &pending.islock;
        let read = BlsCtx::from_legacy(pending.legacy);
        let pre = match islock_precheck(params, view, read, islock) {
            Ok(pre) => pre,
            Err(fail) => {
                precheck_bad_source(fail)?;
                targets.push(BatchTarget::BadSource);
                continue;
            }
        };
        let target = match islock_sign_target(params, view, &pre, islock, pass) {
            Ok((quorum, sign_hash, pk)) => BatchTarget::Push {
                llmq_type: pre.llmq_type,
                quorum_hash: *quorum.quorum_hash(),
                id: pre.id,
                sign_hash,
                sig: pre.sig,
                pk,
            },
            Err(LockError::View(e)) => return Err(IsNetError::View(e)),
            Err(LockError::UnknownLlmqType(t)) => return Err(IsNetError::UnknownLlmqType(t)),
            // `NoQuorum` is Core's (:148-151). A quorum the view cannot verify or reach has
            // no Core answer (the node's view always holds V's) and is the message's fault,
            // never the peer's.
            Err(LockError::Rejected(_) | LockError::BlsUnsupported) => BatchTarget::BadMessage,
        };
        targets.push(target);
    }
    Ok(targets)
}

impl NetInstantSend {
    /// The InstantSend type: Core asserts it is configured (`:274-276`, `:334-336`).
    pub(super) fn is_llmq_type<E>(&self) -> Result<LlmqType, IsNetError<E>> {
        let llmq_type = self.params.consensus.llmq_type_dip0024_instant_send;
        match self.params.consensus.llmq(llmq_type) {
            Some(_) => Ok(llmq_type),
            None => Err(IsNetError::UnknownLlmqType(llmq_type.to_base())),
        }
    }

    /// `BuildVerificationBatch`'s view reads for `pass`, one [`BatchTarget`] per lock of `pend`:
    /// the signature read under the lock's read scheme, the cycle block, the sign height from the
    /// InstantSend tip, and the pass's quorum. Reads only the view.
    ///
    /// A lock [`Self::build_verification_batch`] will skip (its source found bad earlier in the
    /// build) is read too; the reads change nothing.
    ///
    /// # Errors
    ///
    /// A view failure, or an unconfigured InstantSend type.
    // Core: instantsend/net_instantsend.cpp:122-152
    pub fn batch_targets<V: QuorumScanView + ?Sized>(
        &self,
        view: &V,
        pass: Pass,
        pend: &[PendingIsLockEntry],
    ) -> Result<Vec<BatchTarget>, IsNetError<V::Error>> {
        batch_targets(&self.params, view, pass, pend)
    }

    /// The rest of `BuildVerificationBatch`, over `targets` ([`Self::batch_targets`]) under
    /// `scheme`, the pass's flag. Each lock of a source already bad is skipped; a
    /// [`BatchTarget::BadSource`] makes its source bad and a [`BatchTarget::BadMessage`] its
    /// message; a lock whose recovered signature is stored from the same quorum with the same
    /// signature is already verified; the rest are pushed, and a recovered signature is kept for
    /// each whose request id has none. The verifier checks every 8 distinct ids as they are
    /// pushed, so a source can turn bad part-way.
    // Core: instantsend/net_instantsend.cpp:104-174
    pub fn build_verification_batch(
        &self,
        scheme: BlsCtx,
        pend: &[PendingIsLockEntry],
        targets: Vec<BatchTarget>,
        rng: &mut impl CryptoRng,
    ) -> BatchVerificationData {
        let mut data = BatchVerificationData::new(scheme);
        for (pending, target) in pend.iter().zip(targets) {
            let (hash, node, islock) = (&pending.islock_hash, pending.node_id, &pending.islock);
            if data.source_bad(node) {
                continue;
            }
            let (llmq_type, quorum_hash, id, sign_hash, sig, pk) = match target {
                BatchTarget::BadSource => {
                    data.bad_sources.insert(node);
                    continue;
                }
                BatchTarget::BadMessage => {
                    data.bad_messages.insert(*hash);
                    continue;
                }
                BatchTarget::Push {
                    llmq_type,
                    quorum_hash,
                    id,
                    sign_hash,
                    sig,
                    pk,
                } => (llmq_type, quorum_hash, id, sign_hash, sig, pk),
            };
            // Core reads the stored signature into a fresh lazy field, so under the flag now.
            if let Some((stored_quorum, stored_sig)) =
                self.recsigs.get_recovered_sig(llmq_type, &id, islock.txid().as_bytes())
                && stored_quorum == quorum_hash
                && matches!(
                    read_lazy_sig(stored_sig.as_bytes(), scheme),
                    SigRead::Valid(ref s) if *s == sig
                )
            {
                data.already_verified += 1;
                continue;
            }
            if !data.verifier.push_message(node, *hash, sign_hash, &sig, &pk, &mut *rng) {
                // Core asserts a valid key (`bls_batchverifier.h:70`); a mined quorum's key
                // always is.
                tracing::error!(
                    "NetInstantSend::BuildVerificationBatch -- islock={}: quorum {} has an \
                     invalid key",
                    Hash256::from(*hash),
                    quorum_hash,
                );
                data.bad_messages.insert(*hash);
                continue;
            }
            data.verify_count += 1;
            if !self.recsigs.has_recovered_sig_for_id(llmq_type, &id) {
                data.rec_sigs.entry(*hash).or_insert_with(|| RecoveredSig {
                    llmq_type,
                    quorum_hash,
                    id,
                    msg_hash: *islock.txid().as_bytes(),
                    sig: *islock.sig().as_bytes(),
                });
            }
        }
        data
    }

    /// `ApplyVerificationResults(llmq_params, ban, data, pend)`: each lock of a bad source or a
    /// bad message is dropped, its source scored +20 once when `ban`, and returned when its
    /// message is bad; every other lock is processed (S4's
    /// [`IsLockProcessor`](super::seams::IsLockProcessor)), then its kept recovered signature is
    /// handed to the signing manager unless one is stored for its id by then.
    ///
    /// # Errors
    ///
    /// The processor's store failure, which stops the batch there.
    // Core: instantsend/net_instantsend.cpp:176-224
    pub fn apply_verification_results<E>(
        &self,
        llmq_type: LlmqType,
        ban: bool,
        mut data: BatchVerificationData,
        pend: &[PendingIsLockEntry],
    ) -> Result<BTreeSet<IsLockHash>, IsNetError<E>> {
        let mut bad_is_locks = BTreeSet::new();
        let mut penalized = BTreeSet::new();
        for pending in pend {
            let (hash, node, islock) = (&pending.islock_hash, pending.node_id, &pending.islock);
            let source_bad = data.source_bad(node);
            let message_bad = data.message_bad(hash);
            if source_bad || message_bad {
                tracing::debug!(
                    target: "instantsend",
                    "NetInstantSend::ApplyVerificationResults -- txid={}, islock={}: \
                     verification failed, peer={}",
                    islock.txid(),
                    Hash256::from(*hash),
                    node,
                );
                if ban && source_bad && penalized.insert(node) {
                    self.peers.misbehaving(node, OLD_ACTIVE_SET_FAILURE_MISBEHAVIOR_SCORE);
                }
                if message_bad {
                    bad_is_locks.insert(*hash);
                }
                continue;
            }
            self.processor.process_instant_send_lock(node, hash, islock, pending.legacy)?;
            if let Some(rec_sig) = data.rec_sigs.remove(hash)
                && !self.recsigs.has_recovered_sig_for_id(llmq_type, &rec_sig.id)
            {
                tracing::debug!(
                    target: "instantsend",
                    "NetInstantSend::ApplyVerificationResults -- txid={}, islock={}: passing \
                     reconstructed recSig to signing mgr, peer={}",
                    islock.txid(),
                    Hash256::from(*hash),
                    node,
                );
                self.recsigs.push_reconstructed_recovered_sig(Arc::new(rec_sig));
            }
        }
        Ok(bad_is_locks)
    }

    /// `ProcessPendingInstantSendLocks(llmq_params, signOffset, ban, pend)`: `pass`'s view reads
    /// in one turn of `views`, then the batch built and verified (what the last sub-batch left)
    /// and applied. Returns the bad messages.
    ///
    /// # Errors
    ///
    /// As [`Self::batch_targets`] and [`Self::apply_verification_results`], or the view could not
    /// be built.
    // Core: instantsend/net_instantsend.cpp:313-329
    pub fn process_pending_instant_send_locks<Q: IsPassView>(
        &self,
        views: &Q,
        pass: Pass,
        ban: bool,
        pend: &[PendingIsLockEntry],
        rng: &mut impl CryptoRng,
    ) -> Result<BTreeSet<IsLockHash>, IsNetError<Q::Error>> {
        let llmq_type = self.is_llmq_type()?;
        let (params, owned) = (Arc::clone(&self.params), pend.to_vec());
        let (targets, scheme) = views.with_pass(move |view, scheme| {
            Ok((batch_targets(&params, view, pass, &owned)?, scheme))
        })?;
        let mut batch = self.build_verification_batch(scheme, pend, targets, rng);
        let _ = batch.verifier.verify();
        tracing::debug!(
            target: "instantsend",
            "NetInstantSend::ProcessPendingInstantSendLocks -- verified locks. count={}, \
             alreadyVerified={}, nodes={}",
            batch.verify_count,
            batch.already_verified,
            batch.unique_source_count(),
        );
        self.apply_verification_results(llmq_type, ban, batch, pend)
    }

    /// `ProcessPendingISLocks(locks)`: the current active set's pass without scoring, then, for
    /// the locks whose message it found bad, the previous active set's pass (offset
    /// `dkgInterval`) with scoring. Each pass takes its own turn of `views`.
    ///
    /// # Errors
    ///
    /// As [`Self::process_pending_instant_send_locks`].
    // Core: instantsend/net_instantsend.cpp:331-357
    pub fn process_pending_is_locks<Q: IsPassView>(
        &self,
        views: &Q,
        locks: Vec<PendingIsLockEntry>,
        rng: &mut impl CryptoRng,
    ) -> Result<(), IsNetError<Q::Error>> {
        self.is_llmq_type::<Q::Error>()?;
        let bad_is_locks = self.process_pending_instant_send_locks(
            views,
            Pass::Current,
            false,
            &locks,
            &mut *rng,
        )?;
        if !bad_is_locks.is_empty() {
            tracing::debug!(
                target: "instantsend",
                "NetInstantSend::ProcessPendingISLocks -- doing verification on old active set",
            );
            let still_pending: Vec<_> =
                locks.into_iter().filter(|p| bad_is_locks.contains(&p.islock_hash)).collect();
            self.process_pending_instant_send_locks(
                views,
                Pass::Previous,
                true,
                &still_pending,
                &mut *rng,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod tests;
