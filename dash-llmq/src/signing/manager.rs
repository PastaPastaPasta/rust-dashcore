//! Recovered-signature storage, notifications and verification batches.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use dash_evo::locks::LockSchemes;
use dash_evo::locks::wire::{RecoveredSig, SigRead, read_lazy_sig};
use dash_params::LlmqType;
use dash_types::Numeric;
use rand_core::Rng;

use super::db::{RecSigDb, RecSigKv};
use crate::dkg::bls::{BlsCtx, BlsPkValue, verify_insecure};
use crate::sigenv::{PeerId, QuorumKey, QuorumLookup, RecSigReads, SigningQuorum};

/// Core's pending queue bounds.
// Core: v24.0.0 src/llmq/signing.h:167-168 MAX_PENDING_RECSIGS_*
pub const MAX_PENDING_PER_PEER: usize = 1000;
pub const MAX_PENDING_TOTAL: usize = 10000;

/// A notification destination supplied by the runtime.
pub type ListenerId = u64;

/// A stored signature and its notifications; deliver listeners before relaying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredEvent {
    pub sig: Arc<RecoveredSig>,
    pub from_self: bool,
    pub proactive_relay: bool,
    pub listeners: Vec<ListenerId>,
}

/// Effects and failures returned in processing order; the runtime applies them.
#[derive(Debug)]
pub enum SigningEvent<D, L> {
    Recovered(RecoveredEvent),
    Ban {
        peer: PeerId,
        mark_shares_banned: bool,
    },
    QuorumUnavailable {
        peer: PeerId,
        key: QuorumKey,
        inactive: bool,
    },
    StorageError {
        hash: [u8; 32],
        peer: PeerId,
        error: D,
    },
    /// The signature is dropped deliberately: request completion already happened, and without
    /// a stored hash a later announcement can request it again.
    LookupError {
        peer: PeerId,
        key: QuorumKey,
        error: L,
    },
}

/// A fallible read of storage or the injected quorum view.
#[derive(Debug, PartialEq, Eq)]
pub enum SigningError<D, L> {
    Storage(D),
    Lookup(L),
}

/// A queued signature keeps the BLS context sampled when its bytes were read.
#[derive(Clone, Debug)]
pub struct PendingSig {
    pub sig: Arc<RecoveredSig>,
    pub read: BlsCtx,
}

/// Signatures in per-source collection order, bounded by unique `(peer, signHash)` pairs.
#[derive(Debug, Default)]
pub struct PendingBatch {
    pub by_source: BTreeMap<PeerId, Vec<PendingSig>>,
    pub more_work: bool,
}

pub struct SigningManager<K> {
    pub(super) db: RecSigDb<K>,
    platform_type: LlmqType,
    max_age: i64,
    pub(super) pending: BTreeMap<PeerId, VecDeque<PendingSig>>,
    pub(super) pending_count: usize,
    pub(super) reconstructed: BTreeMap<[u8; 32], Arc<RecoveredSig>>,
    listeners: Vec<ListenerId>,
    last_cleanup_ms: Option<i64>,
}

impl<K> SigningManager<K> {
    // Core: v24.0.0 src/llmq/signing.cpp:385-391 CSigningManager
    pub fn new(kv: K, platform_type: LlmqType, max_age: i64) -> Self {
        Self {
            db: RecSigDb::new(kv, platform_type),
            platform_type,
            max_age,
            pending: BTreeMap::new(),
            pending_count: 0,
            reconstructed: BTreeMap::new(),
            listeners: Vec::new(),
            last_cleanup_ms: None,
        }
    }

    pub fn db(&self) -> &RecSigDb<K> {
        &self.db
    }

    /// Drops all pending recovered signatures from a peer.
    ///
    /// Core requires this after every signing or sig-share ban and when a peer is finalized,
    /// so both managers cannot retain work for a disconnected source.
    // Core: v24.0.0 src/llmq/signing.cpp:477-486 CSigningManager::RemoveNode
    pub fn remove_node(&mut self, peer: PeerId) {
        if let Some(queue) = self.pending.remove(&peer) {
            self.pending_count -= queue.len();
        }
    }

    // Core: v24.0.0 src/llmq/signing.cpp:619-623 CSigningManager::PushReconstructedRecoveredSig
    pub fn push_reconstructed(&mut self, sig: Arc<RecoveredSig>) {
        self.reconstructed.entry(sig.hash()).or_insert(sig);
    }

    /// Drain reconstructed signatures before collecting remote signatures. The caller processes
    /// every returned signature with `process_recovered(.., from_self = true, ..)`.
    // Core: v24.0.0 src/llmq/signing.cpp:573-578 CSigningManager::FetchPendingReconstructed
    pub fn take_reconstructed(&mut self) -> Vec<Arc<RecoveredSig>> {
        std::mem::take(&mut self.reconstructed).into_values().collect()
    }

    // Core: v24.0.0 src/llmq/signing.cpp:636-640 CSigningManager::RegisterRecoveredSigsListener
    pub fn register_listener(&mut self, listener: ListenerId) {
        self.listeners.push(listener);
    }

    // Core: v24.0.0 src/llmq/signing.cpp:642-647 CSigningManager::UnregisterRecoveredSigsListener
    pub fn unregister_listener(&mut self, listener: ListenerId) {
        self.listeners.retain(|id| *id != listener);
    }

    // Core: v24.0.0 src/llmq/signing.cpp:613-617 CSigningManager::GetListeners
    pub fn listeners(&self) -> &[ListenerId] {
        &self.listeners
    }

    fn is_platform(&self, t: LlmqType) -> bool {
        t.to_base() == self.platform_type.to_base()
    }
}

impl<K: RecSigKv> SigningManager<K> {
    /// Tests a recovered-signature inventory hash, including trusted reconstruction work.
    // Core: v24.0.0 src/llmq/signing.cpp:395-408 CSigningManager::AlreadyHave
    pub fn already_have(&self, hash: &[u8; 32]) -> Result<bool, K::Error> {
        if self.reconstructed.contains_key(hash) {
            return Ok(true);
        }
        self.db.has_for_hash(hash)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:410-420 CSigningManager::GetRecoveredSigForGetData
    pub fn recsig_for_getdata<L: QuorumLookup>(
        &self,
        hash: &[u8; 32],
        lookup: &L,
    ) -> Result<Option<RecoveredSig>, SigningError<K::Error, L::Error>> {
        let Some(sig) = self.db.get_by_hash(hash).map_err(SigningError::Storage)? else {
            return Ok(None);
        };
        if !lookup
            .is_quorum_active(sig.llmq_type, &sig.quorum_hash)
            .map_err(SigningError::Lookup)?
        {
            return Ok(None);
        }
        Ok(Some(sig))
    }

    /// Accepts an already verified signature from local recovery or a verified remote batch.
    /// Reconstructed signatures are treated as locally produced and use `from_self = true`
    /// (net_signing.cpp:225); that flag only controls proactive relay.
    // Core: v24.0.0 src/llmq/signing.cpp:580-611; src/llmq/net_signing.cpp:198-217 ProcessRecoveredSig
    pub fn process_recovered(
        &mut self,
        sig: Arc<RecoveredSig>,
        from_self: bool,
        now_s: i64,
    ) -> Result<Option<RecoveredEvent>, K::Error> {
        let hash = sig.hash();
        if self.db.has_for_hash(&hash)? {
            return Ok(None);
        }
        if self.is_platform(sig.llmq_type) {
            if self.db.has_recovered_sig(sig.llmq_type, &sig.id, &sig.msg_hash)? {
                return Ok(None);
            }
        } else if self.db.get_by_id(sig.llmq_type, &sig.id)?.is_some() {
            return Ok(None);
        }
        self.db.write(&sig, now_s)?;
        self.reconstructed.remove(&hash);
        let proactive_relay = from_self
            && ![LlmqType::Llmq100_67, LlmqType::Llmq400_60, LlmqType::Llmq400_85]
                .into_iter()
                .any(|t| t.to_base() == sig.llmq_type.to_base());
        Ok(Some(RecoveredEvent {
            sig,
            from_self,
            proactive_relay,
            listeners: self.listeners.clone(),
        }))
    }

    // Core: v24.0.0 src/llmq/signing.cpp:664-668 CSigningManager::HasRecoveredSigForSigning
    pub fn has_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, K::Error> {
        self.db.has_for_signing(t, id, msg)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:676-691 CSigningManager::IsConflicting
    pub fn is_conflicting(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, K::Error> {
        if self.is_platform(t) || !self.db.has_for_id(t, id)? {
            return Ok(false);
        }
        Ok(!self.db.has_recovered_sig(t, id, msg)?)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:693-696 CSigningManager::GetVoteForId
    pub fn vote_for(&self, t: LlmqType, id: &[u8; 32]) -> Result<Option<[u8; 32]>, K::Error> {
        self.db.vote_for(t, id)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:330-340 CRecoveredSigsDb::WriteVoteForId
    pub fn write_vote(
        &mut self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
        now_s: i64,
    ) -> Result<(), K::Error> {
        self.db.write_vote(t, id, msg, now_s)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:625-628 CSigningManager::TruncateRecoveredSig
    pub fn truncate(&mut self, t: LlmqType, id: &[u8; 32]) -> Result<(), K::Error> {
        self.db.truncate(t, id)
    }

    /// Runs at most once per five seconds; returns whether a pass ran.
    // Core: v24.0.0 src/llmq/signing.cpp:630-634; src/llmq/net_signing.cpp:288-291 Cleanup;
    // src/util/time.h:40-66 CleanupThrottler
    pub fn cleanup(&mut self, now_ms: i64) -> Result<bool, K::Error> {
        if self.last_cleanup_ms.is_some_and(|last| now_ms.saturating_sub(last) < 5000) {
            return Ok(false);
        }
        self.last_cleanup_ms = Some(now_ms);
        let now_s = now_ms.div_euclid(1000);
        self.db.cleanup_old(self.max_age, now_s)?;
        self.db.cleanup_old_votes(self.max_age, now_s)?;
        Ok(true)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:488-540 CollectPendingRecoveredSigsToVerify
    pub fn collect_pending(
        &mut self,
        max_unique_sessions: usize,
        rng: &mut impl Rng,
    ) -> Result<PendingBatch, K::Error> {
        let mut peers: Vec<_> = self.pending.keys().copied().collect();
        shuffle(&mut peers, rng);
        let mut batch = PendingBatch::default();
        let mut sessions = BTreeSet::new();
        let mut index = 0;
        while !peers.is_empty() && sessions.len() < max_unique_sessions {
            let peer = peers[index];
            let Some(item) = self.pending.get(&peer).and_then(|q| q.front()) else {
                peers.remove(index);
                if !peers.is_empty() {
                    index %= peers.len();
                }
                continue;
            };
            let known = match self.db.has_for_hash(&item.sig.hash()) {
                Ok(known) => known,
                Err(error) => {
                    for (source, items) in batch.by_source {
                        self.pending_count += items.len();
                        let queue = self.pending.entry(source).or_default();
                        for item in items.into_iter().rev() {
                            queue.push_front(item);
                        }
                    }
                    return Err(error);
                }
            };
            if !known {
                sessions.insert((peer, item.sig.sign_hash()));
                batch.by_source.entry(peer).or_default().push(item.clone());
            }
            if let Some(queue) = self.pending.get_mut(&peer) {
                queue.pop_front();
                self.pending_count -= 1;
                if queue.is_empty() {
                    self.pending.remove(&peer);
                    peers.remove(index);
                } else {
                    index += 1;
                }
            }
            if !peers.is_empty() {
                index %= peers.len();
            }
        }
        batch.more_work = !batch.by_source.is_empty()
            && (!self.pending.is_empty() || !self.reconstructed.is_empty());
        Ok(batch)
    }

    /// Rechecks quorum availability, attributes bad signatures to sources, then stores good ones.
    /// `check` is sampled from the loaded view for this pass; each item retains its read scheme.
    // Core: v24.0.0 src/llmq/signing.cpp:542-568; src/llmq/net_signing.cpp:220-281,307-319 ProcessPendingRecoveredSigs
    pub fn process_batch<L: QuorumLookup>(
        &mut self,
        mut batch: PendingBatch,
        lookup: &L,
        check: BlsCtx,
        now_s: i64,
    ) -> Vec<SigningEvent<K::Error, L::Error>> {
        let mut events = Vec::new();
        let mut keys: BTreeMap<QuorumKey, BlsPkValue> = BTreeMap::new();
        for (&peer, sigs) in &mut batch.by_source {
            sigs.retain(|item| {
                let sig = &item.sig;
                let key = (sig.llmq_type.to_base(), sig.quorum_hash);
                if keys.contains_key(&key) {
                    return true;
                }
                let quorum = match lookup.get_quorum(sig.llmq_type, &sig.quorum_hash) {
                    Ok(Some(quorum)) => quorum,
                    Ok(None) => {
                        events.push(SigningEvent::QuorumUnavailable {
                            peer,
                            key,
                            inactive: false,
                        });
                        return false;
                    }
                    Err(error) => {
                        events.push(SigningEvent::LookupError {
                            peer,
                            key,
                            error,
                        });
                        return false;
                    }
                };
                match lookup.is_quorum_active(sig.llmq_type, quorum.quorum_hash()) {
                    Ok(true) => {
                        keys.insert(key, quorum.quorum_public_key().clone());
                        true
                    }
                    Ok(false) => {
                        events.push(SigningEvent::QuorumUnavailable {
                            peer,
                            key,
                            inactive: true,
                        });
                        false
                    }
                    Err(error) => {
                        events.push(SigningEvent::LookupError {
                            peer,
                            key,
                            error,
                        });
                        false
                    }
                }
            });
        }

        let mut bad_sources = BTreeSet::new();
        for (&peer, sigs) in &batch.by_source {
            for item in sigs {
                let schemes = LockSchemes {
                    read: item.read,
                    check,
                };
                let SigRead::Valid(sig) = read_lazy_sig(&item.sig.sig, schemes.read) else {
                    bad_sources.insert(peer);
                    break;
                };
                let key = (item.sig.llmq_type.to_base(), item.sig.quorum_hash);
                if let Some(pk) = keys.get(&key)
                    && !verify_insecure(&sig, pk, &item.sig.sign_hash(), schemes.check.scheme())
                {
                    bad_sources.insert(peer);
                }
            }
        }

        let mut processed = BTreeSet::new();
        for (peer, sigs) in batch.by_source {
            if bad_sources.contains(&peer) {
                if peer != -1 {
                    self.remove_node(peer);
                    events.push(SigningEvent::Ban {
                        peer,
                        mark_shares_banned: false,
                    });
                }
                continue;
            }
            for item in sigs {
                if processed.insert(item.sig.hash()) {
                    let hash = item.sig.hash();
                    match self.process_recovered(item.sig, peer == -1, now_s) {
                        Ok(Some(event)) => events.push(SigningEvent::Recovered(event)),
                        Ok(None) => {}
                        Err(error) => events.push(SigningEvent::StorageError {
                            hash,
                            peer,
                            error,
                        }),
                    }
                }
            }
        }
        events
    }
}

impl<K: RecSigKv> RecSigReads for SigningManager<K>
where
    K::Error: core::fmt::Debug,
{
    type Error = K::Error;

    // Core: v24.0.0 src/llmq/signing.cpp:664-668 CSigningManager::HasRecoveredSigForSigning
    fn has_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, Self::Error> {
        SigningManager::has_for_signing(self, t, id, msg)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:659-662 CSigningManager::HasRecoveredSigForSession
    fn has_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, Self::Error> {
        self.db.has_for_session(sign_hash)
    }
}

// Core: v24.0.0 src/llmq/signing.h:257-285 IterateNodesRandom
fn shuffle(peers: &mut [PeerId], rng: &mut impl Rng) {
    for end in (1..peers.len()).rev() {
        let n = (end + 1) as u64;
        let zone = u64::MAX - u64::MAX % n;
        let index = loop {
            let value = rng.next_u64();
            if value < zone {
                break (value % n) as usize;
            }
        };
        peers.swap(end, index);
    }
}

#[cfg(test)]
mod tests;
