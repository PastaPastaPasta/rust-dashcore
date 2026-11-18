//! Inbound signature-share handling and recovery.
#![allow(deprecated)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_evo::locks::wire::RecoveredSig;
use dash_types::Numeric;
use rand_core::RngCore;

use crate::dkg::bls::{self, BlsCtx, BlsSigValue};
use crate::dkg::wire::{BatchedSigShares, QuorumHash, SigSesAnn, SigShare, SigSharesInv};
use crate::sigenv::{
    BatchVerdict, CapturedShare, Collected, PeerId, QuorumLookup, RecSigReads, Recovered,
    ShareBatch, ShareRecovery, SharesEffect, SharesEnv, SigSharesNet, SigningQuorum,
};

use super::manager::SigSharesManager;
use super::state::{UNINITIALIZED_SESSION_ID, max_sessions_for_peer, sig_share_key};

const MAX_PENDING_PER_NODE: usize = 1000;
const MAX_PENDING_TOTAL: usize = 10_000;
const MAX_SIG_SES_ANNS: usize = 100;
const MAX_SIG_SHARES_INVS: usize = 200;
const MAX_SIG_SHARES: usize = 32;
const MAX_BATCHED_SIGS: usize = dash_params::MAX_LLMQ_SIZE as usize;

fn is_known_type(t: dash_params::LlmqType) -> bool {
    use dash_params::LlmqTypeExt;
    dash_params::LlmqType::AVAILABLE.contains(&t)
}

fn is_zero_hash(hash: &QuorumHash) -> bool {
    *hash == QuorumHash::from([0; 32])
}
fn id_bytes(share: &SigShare) -> [u8; 32] {
    *share.id().as_bytes()
}
fn msg_bytes(share: &SigShare) -> [u8; 32] {
    *share.msg_hash().as_bytes()
}

type RecoveredKey = (u8, [u8; 32], Option<[u8; 32]>);

// Core: v24.0.0 l/signing.cpp:664-668 CSigningManager::HasRecoveredSigForSigning
fn recovered_key(
    t: dash_params::LlmqType,
    id: [u8; 32],
    msg: [u8; 32],
    platform: dash_params::LlmqType,
) -> RecoveredKey {
    (t.to_base(), id, (t.to_base() == platform.to_base()).then_some(msg))
}

fn sig_value(share: &SigShare, read: BlsCtx) -> Option<BlsSigValue> {
    BlsSigValue::from_canonical(share.sig_share().as_bytes(), read.scheme())
        .filter(|s| s.is_valid())
}

// Core: v24.0.0 l/signing_shares.cpp:365-386 PreVerifyBatchedSigShares
fn preverify_batched<Q: SigningQuorum>(quorum: &Q, batch: &BatchedSigShares) -> bool {
    let mut seen = BTreeSet::new();
    batch.sig_shares().iter().all(|item| {
        let member = usize::from(item.quorum_member());
        seen.insert(item.quorum_member())
            && member < quorum.members().len()
            && quorum.is_valid_member(&quorum.members()[member])
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SigSharesInvKind {
    Announced,
    Requested,
}

impl<Q: SigningQuorum> SigSharesManager<Q> {
    /// Handle session announcements.
    // Core: v24.0.0 l/signing_shares.cpp:251-300 ProcessMessageSigSesAnn
    pub fn on_ses_ann<L, R, N, S>(
        &mut self,
        peer: PeerId,
        msgs: Vec<SigSesAnn>,
        env: &SharesEnv<'_, L, R, N, S>,
    ) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        if msgs.len() > MAX_SIG_SES_ANNS {
            return vec![SharesEffect::Ban {
                peer,
                mark_shares_banned: true,
            }];
        }
        for ann in msgs {
            if !is_known_type(ann.llmq_type())
                || ann.session_id() == UNINITIALIZED_SESSION_ID
                || is_zero_hash(&ann.quorum_hash())
                || ann.id().as_bytes() == &[0; 32]
                || ann.msg_hash().as_bytes() == &[0; 32]
            {
                return vec![SharesEffect::Ban {
                    peer,
                    mark_shares_banned: true,
                }];
            }
            let Ok(true) = env.lookup.is_quorum_active(ann.llmq_type(), &ann.quorum_hash()) else {
                continue;
            };
            let Ok(Some(quorum)) = env.lookup.get_quorum(ann.llmq_type(), &ann.quorum_hash())
            else {
                continue;
            };
            let state = self.node_states.entry(peer).or_default();
            if !state.can_create_session_from_ann(&ann, max_sessions_for_peer(quorum.params())) {
                continue;
            }
            let hash = *ann.sign_hash().as_bytes();
            state.session_by_recv_id.retain(|_, v| *v != hash);
            state.session_by_recv_id.insert(ann.session_id(), hash);
            let session = state.get_or_create_session_from_ann(&ann, quorum.params());
            session.recv_session_id = ann.session_id();
            session.quorum = Some(quorum);
            self.time_seen_for_sessions.entry(hash).or_insert(env.now.secs());
        }
        Vec::new()
    }

    /// Handle a signature-share inventory.
    // Core: v24.0.0 l/signing_shares.cpp:302-349 ProcessMessageSigShares
    pub fn on_inv<L, R, N, S>(
        &mut self,
        peer: PeerId,
        msgs: Vec<SigSharesInv>,
        env: &SharesEnv<'_, L, R, N, S>,
    ) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        self.on_sig_shares_inv(peer, msgs, SigSharesInvKind::Announced, env)
    }

    /// Handle a request for signature-share inventory.
    // Core: v24.0.0 l/signing_shares.cpp:302-349 ProcessMessageSigShares
    pub fn on_get_sig_shares<L, R, N, S>(
        &mut self,
        peer: PeerId,
        msgs: Vec<SigSharesInv>,
        env: &SharesEnv<'_, L, R, N, S>,
    ) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        self.on_sig_shares_inv(peer, msgs, SigSharesInvKind::Requested, env)
    }

    fn on_sig_shares_inv<L, R, N, S>(
        &mut self,
        peer: PeerId,
        msgs: Vec<SigSharesInv>,
        kind: SigSharesInvKind,
        env: &SharesEnv<'_, L, R, N, S>,
    ) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        if msgs.len() > MAX_SIG_SHARES_INVS {
            return vec![SharesEffect::Ban {
                peer,
                mark_shares_banned: true,
            }];
        }
        for inv in msgs {
            let Some(info) = self.session_info_by_recv_id(peer, inv.session_id()) else {
                continue;
            };
            let Some(quorum) = info.quorum.as_ref() else {
                continue;
            };
            if inv.inv().num_bits() as usize != usize::from(quorum.params().size) {
                return vec![SharesEffect::Ban {
                    peer,
                    mark_shares_banned: true,
                }];
            }
            if env.recsigs.has_for_session(&info.sign_hash).unwrap_or(true) {
                continue;
            }
            if kind == SigSharesInvKind::Announced && !quorum.has_verification_vector() {
                continue;
            }
            let state = self.node_states.entry(peer).or_default();
            if let Some(session) = state.session_by_recv_id_mut(inv.session_id()) {
                let incoming = super::inv::SigSharesInv::from_wire(&inv);
                match kind {
                    SigSharesInvKind::Announced => session.announced.merge(&incoming),
                    SigSharesInvKind::Requested => session.requested.merge(&incoming),
                }
                session.knows.merge(&incoming);
            }
        }
        Vec::new()
    }

    /// Handle concentrated batches of shares.
    // Core: v24.0.0 l/signing_shares.cpp:350-450 ProcessMessageBatchedSigShares
    pub fn on_batched<L, R, N, S>(
        &mut self,
        peer: PeerId,
        msgs: Vec<BatchedSigShares>,
        env: &SharesEnv<'_, L, R, N, S>,
    ) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        if msgs.len() > MAX_BATCHED_SIGS
            || msgs.iter().map(|batch| batch.sig_shares().len()).sum::<usize>() > MAX_BATCHED_SIGS
        {
            return vec![SharesEffect::Ban {
                peer,
                mark_shares_banned: true,
            }];
        }
        for batch in msgs {
            let Some(info) = self.session_info_by_recv_id(peer, batch.session_id()) else {
                continue;
            };
            let Some(quorum) = info.quorum.clone() else {
                continue;
            };
            if !self.preverify_quorum(&quorum, env) {
                continue;
            }
            if !preverify_batched(quorum.as_ref(), &batch) {
                return vec![SharesEffect::Ban {
                    peer,
                    mark_shares_banned: true,
                }];
            }
            for item in batch.sig_shares() {
                let share = Self::rebuild_sig_share(&info, item);
                let key = sig_share_key(&share);
                if let Some(state) = self.node_states.get_mut(&peer) {
                    state.requested_sig_shares.erase(&key);
                }
                if self.sig_shares.has(&key)
                    || env
                        .recsigs
                        .has_for_signing(share.llmq_type(), &id_bytes(&share), &msg_bytes(&share))
                        .unwrap_or(true)
                {
                    continue;
                }
                self.accept_pending(peer, share, env.bls);
            }
        }
        Vec::new()
    }

    /// Handle direct shares, gated by SPORK_21.
    // Core: v24.0.0 l/net_signing.cpp:62-95; l/signing_shares.cpp:451-509 ProcessMessageSigShare
    pub fn on_sig_share<L, R, N, S>(
        &mut self,
        peer: PeerId,
        msgs: Vec<SigShare>,
        env: &SharesEnv<'_, L, R, N, S>,
    ) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        if !env.sporks.spork21_active {
            return Vec::new();
        }
        if msgs.len() > MAX_SIG_SHARES {
            return vec![SharesEffect::Ban {
                peer,
                mark_shares_banned: true,
            }];
        }
        let mut effects = Vec::new();
        for share in msgs {
            if !is_known_type(share.llmq_type()) {
                effects.push(SharesEffect::Ban {
                    peer,
                    mark_shares_banned: true,
                });
                return effects;
            }
            let Ok(true) = env.lookup.is_quorum_active(share.llmq_type(), &share.quorum_hash())
            else {
                continue;
            };
            let Ok(Some(quorum)) = env.lookup.get_quorum(share.llmq_type(), &share.quorum_hash())
            else {
                continue;
            };
            if !self.preverify_quorum(&quorum, env) {
                continue;
            }
            if usize::from(share.quorum_member()) >= quorum.members().len()
                || !quorum.is_valid_member(&quorum.members()[usize::from(share.quorum_member())])
            {
                // Core's BanNode marks at once; the loop then goes on with the next share.
                self.mark_banned(peer);
                effects.push(SharesEffect::Ban {
                    peer,
                    mark_shares_banned: false,
                });
                continue;
            }
            if env
                .recsigs
                .has_for_signing(share.llmq_type(), &id_bytes(&share), &msg_bytes(&share))
                .unwrap_or(true)
                || env.recsigs.has_for_session(share.sign_hash().as_bytes()).unwrap_or(true)
            {
                continue;
            }
            if self.sig_shares.has(&sig_share_key(&share)) {
                continue;
            }
            self.accept_pending(peer, share, env.bls);
        }
        effects
    }

    fn preverify_quorum<L, R, N, S>(&self, quorum: &Arc<Q>, env: &SharesEnv<'_, L, R, N, S>) -> bool
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        let Ok(active) = env.lookup.is_quorum_active(quorum.llmq_type(), quorum.quorum_hash())
        else {
            return false;
        };
        active
            && env.signer.my_pro_tx.as_ref().is_some_and(|p| quorum.is_member(p))
            && quorum.has_verification_vector()
    }

    fn accept_pending(&mut self, peer: PeerId, share: SigShare, read: BlsCtx) -> bool {
        let key = sig_share_key(&share);
        if self.node_states.entry(peer).or_default().banned {
            return false;
        }
        if self.node_states[&peer].pending_incoming_sig_shares.size() >= MAX_PENDING_PER_NODE
            || self.pending_incoming_count() >= MAX_PENDING_TOTAL
        {
            return false;
        }
        if self.node_states.get_mut(&peer).unwrap().pending_incoming_sig_shares.add(&key, share) {
            self.pending_reads.insert((peer, key), read);
            true
        } else {
            false
        }
    }

    /// Drain a bounded randomized batch and resolve all of its quorums.
    // Core: v24.0.0 l/signing_shares.cpp:537-605 CollectPendingSigSharesToVerify
    pub fn collect_to_verify<L: QuorumLookup<Quorum = Q>>(
        &mut self,
        max_shares: usize,
        lookup: &L,
        rng: &mut impl RngCore,
    ) -> Result<Collected<Q>, L::Error> {
        if max_shares == 0 || self.node_states.is_empty() {
            return Ok(Collected::Empty);
        }
        let mut peers: Vec<PeerId> = self.node_states.keys().copied().collect();
        for i in (1..peers.len()).rev() {
            peers.swap(i, (rng.next_u64() as usize) % (i + 1));
        }
        let mut by_source = BTreeMap::<PeerId, Vec<CapturedShare>>::new();
        let mut added = 0;
        let mut idx = 0;
        // Core: v24.0.0 l/signing.h:258-284 IterateNodesRandom
        while !peers.is_empty() && added < max_shares {
            let peer = peers[idx];
            let keep = match self.node_states.get_mut(&peer) {
                Some(state) => match state.pending_incoming_sig_shares.get_first().cloned() {
                    Some(share) => {
                        let key = sig_share_key(&share);
                        state.pending_incoming_sig_shares.erase(&key);
                        let read = self.pending_reads.remove(&(peer, key)).unwrap_or(BlsCtx::BASIC);
                        if !self.sig_shares.has(&key) {
                            by_source.entry(peer).or_default().push(CapturedShare {
                                share,
                                read,
                            });
                            added += 1;
                        }
                        !state.pending_incoming_sig_shares.is_empty()
                    }
                    None => false,
                },
                None => false,
            };
            if keep {
                idx = (idx + 1) % peers.len();
            } else {
                peers.remove(idx);
                if peers.is_empty() {
                    break;
                }
                idx %= peers.len();
            }
        }
        if by_source.is_empty() {
            return Ok(Collected::Empty);
        }
        let more_work =
            self.node_states.values().any(|s| !s.pending_incoming_sig_shares.is_empty());
        let mut quorums = BTreeMap::new();
        for shares in by_source.values() {
            for captured in shares {
                let key = (captured.share.llmq_type().to_base(), captured.share.quorum_hash());
                if quorums.contains_key(&key) {
                    continue;
                }
                match lookup
                    .get_quorum(captured.share.llmq_type(), &captured.share.quorum_hash())?
                {
                    Some(quorum) => {
                        quorums.insert(key, quorum);
                    }
                    None => {
                        return Ok(Collected::MissingQuorum {
                            key,
                        });
                    }
                }
            }
        }
        Ok(Collected::Ready {
            batch: ShareBatch {
                by_source,
                quorums,
            },
            more_work,
        })
    }

    /// Process a verified batch and recover signatures at threshold.
    // Core: v24.0.0 l/net_signing.cpp:448-461; l/signing_shares.cpp:609-762 ProcessPendingSigShares
    pub fn process_verified<L, R, N, S>(
        &mut self,
        batch: &ShareBatch<Q>,
        verdict: &BatchVerdict,
        env: &SharesEnv<'_, L, R, N, S>,
    ) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        let mut effects = Vec::new();
        let mut recovered = BTreeSet::new();
        for (peer, shares) in &batch.by_source {
            if verdict.bad_sources.contains(peer) {
                effects.push(SharesEffect::Ban {
                    peer: *peer,
                    mark_shares_banned: true,
                });
                continue;
            }
            let first = effects.len();
            for captured in shares {
                let key = (captured.share.llmq_type().to_base(), captured.share.quorum_hash());
                if let Some(quorum) = batch.quorums.get(&key).cloned() {
                    self.process_share(
                        captured.share.clone(),
                        captured.read,
                        quorum,
                        env,
                        &recovered,
                        &mut effects,
                    );
                }
            }
            // Core applies each source's recovered sigs before the next source, keyed as
            // l/signing.cpp:664-668: by msg hash for the platform type, by id otherwise.
            for effect in &effects[first..] {
                if let SharesEffect::Recovered(rs) = effect {
                    recovered.insert(recovered_key(
                        rs.llmq_type,
                        rs.id,
                        rs.msg_hash,
                        env.chain.consensus.llmq_type_platform,
                    ));
                }
            }
        }
        effects
    }

    /// Process a share created by the local signing worker.
    // Core: v24.0.0 l/signing_shares.cpp:1539-1548 SignAndProcessSingleShare
    pub fn process_own_share<L, R, N, S>(
        &mut self,
        share: SigShare,
        quorum: Arc<Q>,
        env: &SharesEnv<'_, L, R, N, S>,
    ) -> Vec<SharesEffect>
    where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        let mut effects = Vec::new();
        self.process_share(
            share.clone(),
            env.bls,
            quorum.clone(),
            env,
            &BTreeSet::new(),
            &mut effects,
        );
        if env.sporks.all_members_connected(share.llmq_type()) {
            self.signed_sessions.insert(
                *share.sign_hash().as_bytes(),
                super::state::SignedSession::new(share, quorum),
            );
        }
        effects
    }

    fn process_share<L, R, N, S>(
        &mut self,
        share: SigShare,
        read: BlsCtx,
        quorum: Arc<Q>,
        env: &SharesEnv<'_, L, R, N, S>,
        recovered: &BTreeSet<RecoveredKey>,
        effects: &mut Vec<SharesEffect>,
    ) where
        L: QuorumLookup<Quorum = Q>,
        R: RecSigReads,
        N: SigSharesNet,
        S: ShareRecovery,
    {
        if recovered.contains(&recovered_key(
            share.llmq_type(),
            id_bytes(&share),
            msg_bytes(&share),
            env.chain.consensus.llmq_type_platform,
        )) || env
            .recsigs
            .has_for_signing(share.llmq_type(), &id_bytes(&share), &msg_bytes(&share))
            .unwrap_or(true)
        {
            return;
        }
        let key = sig_share_key(&share);
        if !self.sig_shares.add(&key, share.clone()) {
            return;
        }
        self.share_reads.insert(key, read);
        let all_connected = env.sporks.all_members_connected(share.llmq_type());
        if !all_connected {
            self.sig_shares_queued_to_announce.add(&key, true);
        }
        let sign_hash = *share.sign_hash().as_bytes();
        self.time_seen_for_sessions.insert(sign_hash, env.now.secs());
        if !all_connected
            && env
                .signer
                .my_pro_tx
                .as_ref()
                .and_then(|p| quorum.member_index(p))
                .is_some_and(|i| i == share.quorum_member())
        {
            for node in env.net.quorum_nodes(share.llmq_type(), quorum.quorum_hash()) {
                let session = self
                    .node_states
                    .entry(node)
                    .or_default()
                    .get_or_create_session_from_share(&share, quorum.params());
                session.quorum = Some(quorum.clone());
                session.requested.set(share.quorum_member(), true);
                session.knows.set(share.quorum_member(), true);
            }
        }
        if self.sig_shares.count_for_sign_hash(&sign_hash) < usize::from(quorum.params().threshold)
        {
            return;
        }
        let Some(all) = self.sig_shares.get_all_for_sign_hash(&sign_hash) else {
            return;
        };
        // Core takes shares in session-map order (:719-723); any threshold set recovers the same sig.
        let order = env.recovery.recovery_member_order(quorum.members(), &id_bytes(&share));
        let mut selected = BTreeMap::new();
        for member in order {
            let member = u16::try_from(member).unwrap_or(u16::MAX);
            let stored_read = all
                .get(&member)
                .and_then(|s| self.share_reads.get(&sig_share_key(s)).copied())
                .unwrap_or(read);
            if let Some(stored) = all.get(&member).and_then(|s| sig_value(s, stored_read)) {
                selected.insert(member, stored);
            }
            if selected.len() >= usize::from(quorum.params().threshold) {
                break;
            }
        }
        if selected.len() < usize::from(quorum.params().threshold) {
            return;
        }
        let recovered = env.recovery.try_recover(
            quorum.params(),
            quorum.members(),
            &selected,
            &mut self.recovered_sigs_counter,
        );
        let Recovered::Sig {
            sig,
            self_verify,
        } = recovered
        else {
            return;
        };
        if self_verify
            && !bls::verify_insecure(&sig, quorum.quorum_public_key(), &sign_hash, env.bls.scheme())
        {
            return;
        }
        effects.push(SharesEffect::Recovered(RecoveredSig {
            llmq_type: share.llmq_type(),
            quorum_hash: *quorum.quorum_hash(),
            id: id_bytes(&share),
            msg_hash: msg_bytes(&share),
            sig: sig.to_bytes(env.bls.scheme()),
        }));
    }

    /// Remove all state belonging to a recovered session.
    // Core: v24.0.0 l/signing_shares.cpp:1678-1684 HandleNewRecoveredSig; :1457-1470 RemoveSigSharesForSession
    pub fn on_recovered_sig(&mut self, recovered: &RecoveredSig) {
        let sign_hash = recovered.sign_hash();
        for state in self.node_states.values_mut() {
            state.remove_session(&sign_hash);
        }
        self.sig_shares_requested.erase_all_for_sign_hash(&sign_hash);
        self.sig_shares_queued_to_announce.erase_all_for_sign_hash(&sign_hash);
        self.sig_shares.erase_all_for_sign_hash(&sign_hash);
        self.share_reads.retain(|(hash, _), _| hash != &sign_hash);
        self.pending_reads.retain(|(_, (hash, _)), _| hash != &sign_hash);
        self.signed_sessions.remove(&sign_hash);
        self.time_seen_for_sessions.remove(&sign_hash);
    }

    /// Mark a peer banned and release its requested and pending shares.
    // Core: v24.0.0 l/signing_shares.cpp:1491-1514 MarkAsBanned
    pub fn mark_banned(&mut self, peer: PeerId) {
        if peer == -1 {
            return;
        }
        if let Some(state) = self.node_states.get_mut(&peer) {
            let mut keys = Vec::new();
            state.requested_sig_shares.for_each(|key, _| keys.push(*key));
            for key in keys {
                self.sig_shares_requested.erase(&key);
            }
            state.requested_sig_shares.clear();
            state.pending_incoming_sig_shares.for_each(|key, _| {
                self.pending_reads.remove(&(peer, *key));
            });
            state.pending_incoming_sig_shares.clear();
            state.banned = true;
        }
    }

    /// Remove peer states selected by a predicate.
    // Core: v24.0.0 l/signing_shares.cpp:1472-1489 RemoveNodesIf
    pub fn remove_peers_if(&mut self, mut predicate: impl FnMut(PeerId) -> bool) {
        let peers: Vec<PeerId> =
            self.node_states.keys().copied().filter(|p| predicate(*p)).collect();
        for peer in peers {
            if let Some(mut state) = self.node_states.remove(&peer) {
                state.requested_sig_shares.for_each(|key, _| self.sig_shares_requested.erase(key));
                state.pending_incoming_sig_shares.for_each(|key, _| {
                    self.pending_reads.remove(&(peer, *key));
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dkg::bls::{self, BlsSchemeKind, BlsSkValue};
    use crate::dkg::rng::SeededRng;
    use crate::dkg::wire::{SigShareBytes, SigSharesBits};
    use crate::sigenv::fakes::{FakeLookup, FakeNet, FakeQuorum, FakeRecSigs, FakeRecovery};
    use crate::sigenv::{Now, SignerFacts, SporkFacts};
    use dash_evo::types::ProTxHash;
    use dash_params::{ChainParamsBuilder, ChainSetup, LlmqParams, LlmqType, Network, Release};
    use dash_types::codec::BaseCodec;
    use std::sync::LazyLock;

    fn params(t: LlmqType) -> LlmqParams {
        [Network::Regtest, Network::Main]
            .into_iter()
            .find_map(|network| {
                ChainParamsBuilder::new(network, Release::V24_0)
                    .build()
                    .unwrap()
                    .consensus
                    .llmq(t)
                    .copied()
            })
            .unwrap()
    }

    const PEER: PeerId = 1;

    fn member_hash(i: usize) -> ProTxHash {
        let mut bytes = [0xA5; 32];
        bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
        ProTxHash::from(bytes)
    }

    fn quorum_custom(
        hash: u8,
        members: Vec<ProTxHash>,
        valid_members: BTreeSet<u16>,
        has_verification_vector: bool,
    ) -> Arc<FakeQuorum> {
        let sk = test_sk();
        let mut p = params(LlmqType::LlmqTest);
        p.size = u16::try_from(members.len()).unwrap();
        Arc::new(FakeQuorum {
            llmq_type: LlmqType::LlmqTest,
            quorum_hash: QuorumHash::from([hash; 32]),
            params: p,
            members,
            valid_members,
            has_verification_vector,
            quorum_public_key: bls::secret_to_public(&sk),
            pk_shares: BTreeMap::new(),
            sk_share: Some(sk),
        })
    }

    fn three_members() -> Vec<ProTxHash> {
        (1..=3).map(|i| ProTxHash::from([i; 32])).collect()
    }

    fn signer(pro_tx: ProTxHash) -> SignerFacts {
        SignerFacts {
            my_pro_tx: Some(pro_tx),
            signing_allowed: true,
        }
    }

    fn share_with_id(q: &FakeQuorum, member: u16, id: u8) -> SigShare {
        SigShare::new(
            q.llmq_type,
            q.quorum_hash,
            member,
            [id; 32].into(),
            [id.wrapping_add(1); 32].into(),
            SigShareBytes::from_bytes([0; 96]),
        )
    }

    fn is_ban(effects: &[SharesEffect], peer: PeerId) -> bool {
        matches!(
            effects,
            [SharesEffect::Ban {
                peer: p,
                mark_shares_banned: true
            }] if *p == peer
        )
    }

    fn is_ban_marked(effects: &[SharesEffect], peer: PeerId, mark: bool) -> bool {
        matches!(
            effects,
            [SharesEffect::Ban {
                peer: p,
                mark_shares_banned: m
            }] if *p == peer && *m == mark
        )
    }

    fn recovered_effects(effects: &[SharesEffect]) -> Vec<&RecoveredSig> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                SharesEffect::Recovered(rs) => Some(rs),
                _ => None,
            })
            .collect()
    }

    #[derive(Default)]
    struct ErrLookup;

    impl QuorumLookup for ErrLookup {
        type Quorum = FakeQuorum;
        type Error = &'static str;
        fn get_quorum(
            &self,
            _: LlmqType,
            _: &QuorumHash,
        ) -> Result<Option<Arc<FakeQuorum>>, &'static str> {
            Err("lookup")
        }
        fn is_quorum_active(&self, _: LlmqType, _: &QuorumHash) -> Result<bool, &'static str> {
            Err("lookup")
        }
        fn select_quorum_for_signing(
            &self,
            _: LlmqType,
            _: &[u8; 32],
            _: i32,
            _: i32,
        ) -> Result<Option<Arc<FakeQuorum>>, &'static str> {
            Err("lookup")
        }
    }

    struct ErrRecSigs;

    impl RecSigReads for ErrRecSigs {
        type Error = &'static str;
        fn has_for_signing(
            &self,
            _: LlmqType,
            _: &[u8; 32],
            _: &[u8; 32],
        ) -> Result<bool, &'static str> {
            Err("recsigs")
        }
        fn has_for_session(&self, _: &[u8; 32]) -> Result<bool, &'static str> {
            Err("recsigs")
        }
    }

    fn apply_bans(manager: &mut SigSharesManager<FakeQuorum>, effects: &[SharesEffect]) {
        for effect in effects {
            if let SharesEffect::Ban {
                peer,
                mark_shares_banned: true,
            } = effect
            {
                manager.mark_banned(*peer);
            }
        }
    }

    fn sessions(manager: &SigSharesManager<FakeQuorum>, peer: PeerId) -> usize {
        manager.node_states.get(&peer).map_or(0, |state| state.session_count())
    }

    fn quorum(t: LlmqType, hash: u8) -> Arc<FakeQuorum> {
        quorum_with_vvec(t, hash, true)
    }

    fn test_sk() -> BlsSkValue {
        BlsSkValue::from_canonical(&[7; 32], BlsSchemeKind::Basic).unwrap()
    }

    fn quorum_with_vvec(t: LlmqType, hash: u8, has_verification_vector: bool) -> Arc<FakeQuorum> {
        let sk = test_sk();
        Arc::new(FakeQuorum {
            llmq_type: t,
            quorum_hash: QuorumHash::from([hash; 32]),
            params: params(t),
            members: vec![
                ProTxHash::from([1; 32]),
                ProTxHash::from([2; 32]),
                ProTxHash::from([3; 32]),
            ],
            valid_members: [0, 1, 2].into(),
            has_verification_vector,
            quorum_public_key: bls::secret_to_public(&sk),
            pk_shares: BTreeMap::new(),
            sk_share: Some(sk),
        })
    }

    fn quorum_five(hash: u8) -> Arc<FakeQuorum> {
        let sk = test_sk();
        let mut p = params(LlmqType::LlmqTest);
        p.size = 5;
        p.threshold = 3;
        Arc::new(FakeQuorum {
            llmq_type: LlmqType::LlmqTest,
            quorum_hash: QuorumHash::from([hash; 32]),
            params: p,
            members: (1..=5).map(|i| ProTxHash::from([i; 32])).collect(),
            valid_members: (0..5).collect(),
            has_verification_vector: true,
            quorum_public_key: bls::secret_to_public(&sk),
            pk_shares: BTreeMap::new(),
            sk_share: Some(sk),
        })
    }

    fn share(q: &FakeQuorum, member: u16) -> SigShare {
        SigShare::new(
            q.llmq_type,
            q.quorum_hash,
            member,
            [3; 32].into(),
            [4; 32].into(),
            SigShareBytes::from_bytes([0; 96]),
        )
    }

    fn signed_share_for(q: &FakeQuorum, member: u16, id: u8, sk: &BlsSkValue) -> SigShare {
        signed_share_msg(q, member, id, id.wrapping_add(1), sk)
    }

    fn signed_share_msg(q: &FakeQuorum, member: u16, id: u8, msg: u8, sk: &BlsSkValue) -> SigShare {
        let id_hash = [id; 32].into();
        let msg_hash = [msg; 32].into();
        let unsigned = SigShare::new(
            q.llmq_type,
            q.quorum_hash,
            member,
            id_hash,
            msg_hash,
            SigShareBytes::from_bytes([0; 96]),
        );
        let sig = bls::sign(sk, unsigned.sign_hash().as_bytes(), false);
        SigShare::new(
            q.llmq_type,
            q.quorum_hash,
            member,
            id_hash,
            msg_hash,
            SigShareBytes::from_bytes(sig.to_bytes(BlsCtx::BASIC.scheme())),
        )
    }

    fn signed_share(q: &FakeQuorum, member: u16, sk: &BlsSkValue) -> SigShare {
        signed_share_for(q, member, q.quorum_hash.as_bytes()[0], sk)
    }

    #[test]
    fn mixed_batch_drops_every_drained_share_when_one_quorum_is_missing() {
        let qa = quorum(LlmqType::LlmqTest, 1);
        let qb = quorum(LlmqType::LlmqTest, 2);
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((qa.llmq_type.to_base(), *qa.quorum_hash()), qa.clone());
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        for (peer, q) in [(1, &qa), (2, &qb)] {
            let s = share(q, 0);
            let key = sig_share_key(&s);
            manager.node_states.entry(peer).or_default().pending_incoming_sig_shares.add(&key, s);
        }
        let mut rng = SeededRng::new(0);
        let result = manager.collect_to_verify(32, &lookup, &mut rng).unwrap();
        let Collected::MissingQuorum {
            key,
        } = result
        else {
            panic!("expected a missing quorum");
        };
        assert_eq!(key, (qb.llmq_type.to_base(), *qb.quorum_hash()));
        assert_eq!(manager.pending_incoming_count(), 0);
        assert!(manager.sig_shares.is_empty());
    }

    #[test]
    fn lookup_error_propagates_instead_of_missing_quorum() {
        let q = quorum(LlmqType::LlmqTest, 1);
        let s = share(&q, 0);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        manager
            .node_states
            .entry(1)
            .or_default()
            .pending_incoming_sig_shares
            .add(&sig_share_key(&s), s);
        let mut rng = SeededRng::new(0);
        assert_eq!(manager.collect_to_verify(32, &ErrLookup, &mut rng).err(), Some("lookup"));
    }

    #[test]
    fn collection_keeps_draining_a_peer_past_an_already_held_share() {
        let q = quorum(LlmqType::LlmqTest, 1);
        let lookup = lookup_with(&q);
        let (k1, k2) = (share(&q, 0), share(&q, 1));
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        manager.sig_shares.add(&sig_share_key(&k1), k1.clone());
        for s in [&k1, &k2] {
            assert!(manager.accept_pending(4, s.clone(), BlsCtx::BASIC));
        }
        let mut rng = SeededRng::new(3);
        let Collected::Ready {
            batch,
            more_work,
        } = manager.collect_to_verify(32, &lookup, &mut rng).unwrap()
        else {
            panic!("expected a ready collection");
        };
        let keys: Vec<_> = batch.by_source[&4].iter().map(|c| sig_share_key(&c.share)).collect();
        assert_eq!(keys, [sig_share_key(&k2)]);
        assert!(!more_work);
        assert_eq!(manager.pending_incoming_count(), 0);

        let mut manager = SigSharesManager::<FakeQuorum>::new();
        for peer in [5, 6] {
            for member in 0..3 {
                assert!(manager.accept_pending(peer, share_with_id(&q, member, 40), BlsCtx::BASIC));
            }
        }
        let Collected::Ready {
            batch,
            more_work,
        } = manager.collect_to_verify(4, &lookup, &mut rng).unwrap()
        else {
            panic!("expected a ready collection");
        };
        assert_eq!(batch.by_source.values().map(Vec::len).sum::<usize>(), 4);
        assert_eq!(batch.by_source.values().map(Vec::len).collect::<Vec<_>>(), [2, 2]);
        assert!(more_work);
        assert_eq!(manager.pending_incoming_count(), 2);
    }

    #[test]
    fn an_empty_collection_does_not_consume_existing_shares_when_limit_is_zero() {
        let q = quorum(LlmqType::LlmqTest, 1);
        let s = share(&q, 0);
        let key = sig_share_key(&s);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        manager.node_states.entry(1).or_default().pending_incoming_sig_shares.add(&key, s);
        let lookup = FakeLookup::default();
        let mut rng = SeededRng::new(0);
        assert!(matches!(
            manager.collect_to_verify(0, &lookup, &mut rng).unwrap(),
            Collected::Empty
        ));
        assert_eq!(manager.pending_incoming_count(), 1);
    }

    #[test]
    fn collection_has_at_most_one_share_key_per_source() {
        let q = quorum(LlmqType::LlmqTest, 1);
        let first = share(&q, 0);
        let key = sig_share_key(&first);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        let state = manager.node_states.entry(7).or_default();
        assert!(state.pending_incoming_sig_shares.add(&key, first.clone()));
        assert!(!state.pending_incoming_sig_shares.add(&key, first));
        let lookup = FakeLookup {
            quorums: [((q.llmq_type.to_base(), *q.quorum_hash()), q.clone())].into_iter().collect(),
            ..FakeLookup::default()
        };
        let mut rng = SeededRng::new(1);
        let Collected::Ready {
            batch,
            ..
        } = manager.collect_to_verify(32, &lookup, &mut rng).unwrap()
        else {
            panic!("expected a ready collection");
        };
        let shares = &batch.by_source[&7];
        assert_eq!(shares.len(), 1);
        assert_eq!(
            shares.iter().map(|s| sig_share_key(&s.share)).collect::<BTreeSet<_>>().len(),
            1
        );
    }

    #[test]
    fn collection_keeps_the_intake_read_context() {
        let q = quorum(LlmqType::LlmqTest, 1);
        let lookup = FakeLookup {
            quorums: [((q.llmq_type.to_base(), *q.quorum_hash()), q.clone())].into_iter().collect(),
            ..FakeLookup::default()
        };
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        assert!(manager.accept_pending(9, share(&q, 0), BlsCtx::LEGACY));
        let mut rng = SeededRng::new(2);
        let Collected::Ready {
            batch,
            ..
        } = manager.collect_to_verify(32, &lookup, &mut rng).unwrap()
        else {
            panic!("expected a ready collection");
        };
        assert_eq!(batch.by_source[&9][0].read, BlsCtx::LEGACY);
    }
    type NoRecovery = FakeRecovery<
        fn(&LlmqParams, &[ProTxHash], &BTreeMap<u16, BlsSigValue>, &mut u32) -> Recovered,
        fn(&[ProTxHash], &[u8; 32]) -> Vec<usize>,
    >;

    fn order_all(members: &[ProTxHash], _: &[u8; 32]) -> Vec<usize> {
        (0..members.len()).collect()
    }

    fn no_recovery() -> NoRecovery {
        fn recover(
            _: &LlmqParams,
            _: &[ProTxHash],
            _: &BTreeMap<u16, BlsSigValue>,
            _: &mut u32,
        ) -> Recovered {
            Recovered::NotEnough
        }
        FakeRecovery {
            recover,
            order: order_all,
        }
    }

    fn chain(platform_type: LlmqType) -> ChainSetup {
        let mut setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        setup.consensus.llmq_type_platform = platform_type;
        setup
    }

    static CHAIN: LazyLock<ChainSetup> = LazyLock::new(|| chain(LlmqType::LlmqTestPlatform));

    fn env<'a, D: RecSigReads, R: ShareRecovery>(
        lookup: &'a FakeLookup,
        recsigs: &'a D,
        net: &'a FakeNet,
        recovery: &'a R,
        signer: &'a SignerFacts,
        spork21_value: i64,
        spork21_active: bool,
    ) -> SharesEnv<'a, FakeLookup, D, FakeNet, R> {
        SharesEnv {
            lookup,
            recsigs,
            net,
            recovery,
            signer,
            sporks: SporkFacts {
                spork21_active,
                spork21_value,
            },
            now: Now {
                ms: 42_000,
            },
            bls: BlsCtx::BASIC,
            chain: &CHAIN,
        }
    }

    fn announcement(t: LlmqType, hash: u8, session_id: u32, id: u8) -> SigSesAnn {
        SigSesAnn::new(
            session_id,
            t,
            QuorumHash::from([hash; 32]),
            [id; 32].into(),
            [id.wrapping_add(1); 32].into(),
        )
    }

    fn inventory(session_id: u32, size: usize, set: &[usize]) -> SigSharesInv {
        let mut data = vec![0; size.div_ceil(8)];
        for index in set {
            data[index / 8] |= 1 << (index % 8);
        }
        SigSharesInv::new(
            session_id,
            SigSharesBits::new(size as u64, data).expect("valid test inventory"),
        )
    }

    fn batched(session_id: u32, members: &[u16]) -> BatchedSigShares {
        let mut bytes = vec![session_id as u8, members.len() as u8];
        for member in members {
            bytes.extend_from_slice(&member.to_le_bytes());
            bytes.extend_from_slice(&[0; 96]);
        }
        let mut input = bytes.as_slice();
        BatchedSigShares::decode(&mut input).expect("valid test batch")
    }

    fn lookup_with(q: &Arc<FakeQuorum>) -> FakeLookup {
        FakeLookup {
            quorums: [((q.llmq_type.to_base(), *q.quorum_hash()), q.clone())].into_iter().collect(),
            active: [(q.llmq_type.to_base(), *q.quorum_hash())].into_iter().collect(),
            ..FakeLookup::default()
        }
    }

    fn assert_ban(effect: &[SharesEffect], peer: PeerId) {
        assert!(matches!(
            effect,
            [SharesEffect::Ban {
                peer: p,
                mark_shares_banned: true
            }] if *p == peer
        ));
    }

    #[test]
    fn inventory_kinds_preserve_core_session_state_and_vvec_exception() {
        let q = quorum(LlmqType::LlmqTest, 10);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery = no_recovery();
        let signer = SignerFacts {
            my_pro_tx: Some(ProTxHash::from([1; 32])),
            signing_allowed: true,
        };
        let e = env(&lookup, &recsigs, &net, &recovery, &signer, 0, true);
        let ann = announcement(q.llmq_type, 10, 7, 4);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        assert!(manager.on_ses_ann(8, vec![ann.clone()], &e).is_empty());
        let inv = inventory(7, 3, &[0]);
        assert!(manager.on_inv(8, vec![inv.clone()], &e).is_empty());
        let session = manager.node_states[&8].session_by_recv_id(7).unwrap();
        assert_eq!(session.announced.count_set(), 1);
        assert_eq!(session.requested.count_set(), 0);
        assert_eq!(session.knows.count_set(), 1);
        assert!(manager.on_get_sig_shares(8, vec![inventory(7, 3, &[1])], &e).is_empty());
        let session = manager.node_states[&8].session_by_recv_id(7).unwrap();
        assert_eq!(session.announced.count_set(), 1);
        assert_eq!(session.requested.count_set(), 1);
        assert_eq!(session.knows.count_set(), 2);

        let no_vvec = quorum_with_vvec(LlmqType::LlmqTest, 10, false);
        let lookup = lookup_with(&no_vvec);
        let e = env(&lookup, &recsigs, &net, &recovery, &signer, 0, true);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        assert!(manager.on_ses_ann(8, vec![announcement(q.llmq_type, 10, 9, 5)], &e).is_empty());
        assert!(manager.on_inv(8, vec![inventory(9, 3, &[0])], &e).is_empty());
        let session = manager.node_states[&8].session_by_recv_id(9).unwrap();
        assert_eq!(session.announced.count_set(), 0);
        assert_eq!(session.requested.count_set(), 0);
        assert_eq!(session.knows.count_set(), 0);
        assert!(manager.on_get_sig_shares(8, vec![inventory(9, 3, &[0])], &e).is_empty());
        let session = manager.node_states[&8].session_by_recv_id(9).unwrap();
        assert_eq!(session.announced.count_set(), 0);
        assert_eq!(session.requested.count_set(), 1);
        assert_eq!(session.knows.count_set(), 1);
    }

    #[test]
    fn intake_caps_and_invalid_inputs_ban_with_share_marking() {
        let q = quorum(LlmqType::LlmqTest, 11);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery = no_recovery();
        let signer = SignerFacts {
            my_pro_tx: Some(ProTxHash::from([1; 32])),
            signing_allowed: true,
        };
        let e = env(&lookup, &recsigs, &net, &recovery, &signer, 0, true);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        assert_ban(
            &manager.on_ses_ann(
                1,
                vec![announcement(q.llmq_type, 11, 1, 1); MAX_SIG_SES_ANNS + 1],
                &e,
            ),
            1,
        );
        assert_ban(&manager.on_inv(1, vec![inventory(1, 3, &[]); MAX_SIG_SHARES_INVS + 1], &e), 1);
        assert_ban(
            &manager.on_get_sig_shares(1, vec![inventory(1, 3, &[]); MAX_SIG_SHARES_INVS + 1], &e),
            1,
        );
        assert!(manager.on_ses_ann(1, vec![announcement(q.llmq_type, 11, 1, 2)], &e).is_empty());
        assert_ban(&manager.on_inv(1, vec![inventory(1, 2, &[])], &e), 1);
        let duplicate = batched(1, &[0, 0]);
        assert_ban(&manager.on_batched(1, vec![duplicate], &e), 1);
        let direct = share(&q, 0);
        assert_ban(&manager.on_sig_share(1, vec![direct; MAX_SIG_SHARES + 1], &e), 1);
        let unknown = SigShare::new(
            LlmqType::Unknown(250),
            *q.quorum_hash(),
            0,
            [3; 32].into(),
            [4; 32].into(),
            SigShareBytes::from_bytes([0; 96]),
        );
        assert_ban(&manager.on_sig_share(1, vec![unknown], &e), 1);

        assert_ban(&manager.on_ses_ann(1, vec![announcement(q.llmq_type, 11, 2, 0)], &e), 1);
    }

    #[test]
    fn every_cap_accepts_exactly_n_and_bans_n_plus_one() {
        let q = quorum(LlmqType::LlmqTest, 12);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery = no_recovery();
        let member_signer = signer(ProTxHash::from([1; 32]));
        let e = env(&lookup, &recsigs, &net, &recovery, &member_signer, 0, true);
        let with_session = |e: &SharesEnv<'_, _, _, _, _>| {
            let mut manager = SigSharesManager::<FakeQuorum>::new();
            assert!(
                manager.on_ses_ann(PEER, vec![announcement(q.llmq_type, 12, 1, 3)], e).is_empty()
            );
            manager
        };

        for (n, ban) in [(MAX_SIG_SES_ANNS, false), (MAX_SIG_SES_ANNS + 1, true)] {
            let case = format!("QSIGSESANN x{n}");
            let anns = (1..=n).map(|i| announcement(q.llmq_type, 12, i as u32, i as u8)).collect();
            let mut manager = SigSharesManager::<FakeQuorum>::new();
            let effects = manager.on_ses_ann(PEER, anns, &e);
            assert_eq!(is_ban(&effects, PEER), ban, "{case}");
            assert_eq!(effects.len(), usize::from(ban), "{case}");
            assert_eq!(
                sessions(&manager, PEER),
                if ban {
                    0
                } else {
                    n
                },
                "{case}"
            );
        }

        for (kind, n, ban) in [
            (SigSharesInvKind::Announced, MAX_SIG_SHARES_INVS, false),
            (SigSharesInvKind::Announced, MAX_SIG_SHARES_INVS + 1, true),
            (SigSharesInvKind::Requested, MAX_SIG_SHARES_INVS, false),
            (SigSharesInvKind::Requested, MAX_SIG_SHARES_INVS + 1, true),
        ] {
            let name = match kind {
                SigSharesInvKind::Announced => "QSIGSHARESINV",
                SigSharesInvKind::Requested => "QGETSIGSHARES",
            };
            let case = format!("{name} x{n}");
            let mut manager = with_session(&e);
            let invs = vec![inventory(1, 3, &[0]); n];
            let effects = match kind {
                SigSharesInvKind::Announced => manager.on_inv(PEER, invs, &e),
                SigSharesInvKind::Requested => manager.on_get_sig_shares(PEER, invs, &e),
            };
            assert_eq!(is_ban(&effects, PEER), ban, "{case}");
            assert_eq!(effects.len(), usize::from(ban), "{case}");
            let session = manager.node_states[&PEER].session_by_recv_id(1).unwrap();
            assert_eq!(session.knows.count_set(), usize::from(!ban), "{case}");
        }

        for (n, ban) in [(MAX_BATCHED_SIGS, false), (MAX_BATCHED_SIGS + 1, true)] {
            let case = format!("QBSIGSHARES x{n} empty batches");
            let mut manager = with_session(&e);
            let effects = manager.on_batched(PEER, vec![batched(1, &[]); n], &e);
            assert_eq!(is_ban(&effects, PEER), ban, "{case}");
            assert_eq!(effects.len(), usize::from(ban), "{case}");
        }

        let members: Vec<ProTxHash> = (0..MAX_BATCHED_SIGS).map(member_hash).collect();
        let big = quorum_custom(13, members, (0..MAX_BATCHED_SIGS as u16).collect(), true);
        let big_lookup = lookup_with(&big);
        let big_signer = signer(member_hash(0));
        let big_env = env(&big_lookup, &recsigs, &net, &recovery, &big_signer, 0, true);
        let half = (MAX_BATCHED_SIGS / 2) as u16;
        let first: Vec<u16> = (0..half).collect();
        let second: Vec<u16> = (half..MAX_BATCHED_SIGS as u16).collect();
        for (extra, ban) in [(false, false), (true, true)] {
            let total = MAX_BATCHED_SIGS + usize::from(extra);
            let case = format!("QBSIGSHARES {total} inner shares over valid members");
            let mut manager = SigSharesManager::<FakeQuorum>::new();
            assert!(
                manager
                    .on_ses_ann(PEER, vec![announcement(big.llmq_type, 13, 1, 3)], &big_env)
                    .is_empty()
            );
            let mut msgs = vec![batched(1, &first), batched(1, &second)];
            if extra {
                msgs.push(batched(1, &[0]));
            }
            let effects = manager.on_batched(PEER, msgs, &big_env);
            assert_eq!(is_ban(&effects, PEER), ban, "{case}");
            assert_eq!(effects.len(), usize::from(ban), "{case}");
            assert_eq!(
                manager.pending_incoming_count(),
                if ban {
                    0
                } else {
                    MAX_BATCHED_SIGS
                },
                "{case}"
            );
        }

        for (n, ban) in [(MAX_SIG_SHARES, false), (MAX_SIG_SHARES + 1, true)] {
            let case = format!("QSIGSHARE x{n}");
            let shares = (0..n).map(|i| share_with_id(&q, 0, 10 + i as u8)).collect();
            let mut manager = SigSharesManager::<FakeQuorum>::new();
            let effects = manager.on_sig_share(PEER, shares, &e);
            assert_eq!(is_ban(&effects, PEER), ban, "{case}");
            assert_eq!(effects.len(), usize::from(ban), "{case}");
            assert_eq!(
                manager.pending_incoming_count(),
                if ban {
                    0
                } else {
                    n
                },
                "{case}"
            );
        }
    }

    #[test]
    fn ses_ann_return_paths() {
        let q = quorum(LlmqType::LlmqTest, 31);
        let qh = *q.quorum_hash();
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery = no_recovery();
        let member_signer = signer(ProTxHash::from([1; 32]));
        let e = env(&lookup, &recsigs, &net, &recovery, &member_signer, 0, true);
        let good = |sid: u32, id: u8| announcement(LlmqType::LlmqTest, 31, sid, id);
        let ann = |sid: u32, t: LlmqType, hash: QuorumHash, id: u8, msg: u8| {
            SigSesAnn::new(sid, t, hash, [id; 32].into(), [msg; 32].into())
        };
        let malformed = [
            ("unknown type", ann(2, LlmqType::Unknown(250), qh, 5, 6)),
            ("uninitialized session id", ann(UNINITIALIZED_SESSION_ID, q.llmq_type, qh, 5, 6)),
            ("null quorum hash", ann(2, q.llmq_type, QuorumHash::from([0; 32]), 5, 6)),
            ("null id", ann(2, q.llmq_type, qh, 0, 6)),
            ("null msg hash", ann(2, q.llmq_type, qh, 5, 0)),
        ];
        for (case, bad) in malformed {
            let mut manager = SigSharesManager::<FakeQuorum>::new();
            let effects = manager.on_ses_ann(PEER, vec![bad.clone()], &e);
            assert!(is_ban(&effects, PEER), "{case}: ban");
            assert_eq!(sessions(&manager, PEER), 0, "{case}: no session");

            let mut manager = SigSharesManager::<FakeQuorum>::new();
            let effects = manager.on_ses_ann(PEER, vec![good(1, 1), bad], &e);
            assert!(is_ban(&effects, PEER), "{case} after a valid announcement: ban");
            assert_eq!(sessions(&manager, PEER), 1, "{case}: earlier announcement kept");
        }

        let inactive_lookup = FakeLookup {
            quorums: lookup.quorums.clone(),
            ..FakeLookup::default()
        };
        let missing_lookup = FakeLookup {
            active: lookup.active.clone(),
            ..FakeLookup::default()
        };
        for (case, lookup) in
            [("inactive quorum", &inactive_lookup), ("missing quorum", &missing_lookup)]
        {
            let e = env(lookup, &recsigs, &net, &recovery, &member_signer, 0, true);
            let mut manager = SigSharesManager::<FakeQuorum>::new();
            assert!(manager.on_ses_ann(PEER, vec![good(1, 1)], &e).is_empty(), "{case}");
            assert_eq!(sessions(&manager, PEER), 0, "{case}: no session");
            assert!(manager.time_seen_for_sessions.is_empty(), "{case}: no time seen");
        }

        let max = max_sessions_for_peer(q.params());
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        let anns = (1..=max).map(|i| good(i as u32, i as u8)).collect();
        assert!(manager.on_ses_ann(PEER, anns, &e).is_empty());
        assert_eq!(sessions(&manager, PEER), max);
        let over = max as u32 + 1;
        assert!(
            manager.on_ses_ann(PEER, vec![good(over, over as u8)], &e).is_empty(),
            "session cap: ignored"
        );
        assert_eq!(sessions(&manager, PEER), max, "session cap: no new session");
        assert!(manager.node_states[&PEER].session_by_recv_id(over).is_none(), "session cap");
        assert!(manager.on_ses_ann(PEER, vec![good(over + 1, 1)], &e).is_empty());
        assert_eq!(sessions(&manager, PEER), max, "known session at the cap");
        assert!(
            manager.node_states[&PEER].session_by_recv_id(over + 1).is_some(),
            "known session at the cap is re-bound"
        );
    }

    #[test]
    fn batched_return_paths() {
        let q = quorum_custom(32, three_members(), [0, 1].into(), true);
        let qh = *q.quorum_hash();
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery = no_recovery();
        let member_signer = signer(ProTxHash::from([1; 32]));
        let e = env(&lookup, &recsigs, &net, &recovery, &member_signer, 0, true);
        let rebuilt = |member: u16| {
            SigShare::new(
                q.llmq_type,
                qh,
                member,
                [7; 32].into(),
                [8; 32].into(),
                SigShareBytes::from_bytes([0; 96]),
            )
        };
        let setup = |e: &SharesEnv<'_, _, _, _, _>| {
            let mut manager = SigSharesManager::<FakeQuorum>::new();
            assert!(
                manager.on_ses_ann(PEER, vec![announcement(q.llmq_type, 32, 1, 7)], e).is_empty()
            );
            for member in [0, 1] {
                let key = sig_share_key(&rebuilt(member));
                manager.node_states.get_mut(&PEER).unwrap().requested_sig_shares.add(&key, 123);
                manager.sig_shares_requested.add(&key, (PEER, 123));
            }
            manager
        };
        let requested =
            |m: &SigSharesManager<FakeQuorum>| m.node_states[&PEER].requested_sig_shares.size();

        for (case, members) in [
            ("out-of-range member", vec![3]),
            ("invalid member", vec![2]),
            ("duplicate member", vec![1, 1]),
            ("out-of-range member after a valid one", vec![0, 3]),
            ("invalid member after a valid one", vec![0, 2]),
        ] {
            let mut manager = setup(&e);
            let effects = manager.on_batched(PEER, vec![batched(1, &members)], &e);
            assert!(is_ban(&effects, PEER), "{case}: ban");
            assert_eq!(manager.pending_incoming_count(), 0, "{case}: nothing queued");
            assert_eq!(requested(&manager), 2, "{case}: requests untouched");
            apply_bans(&mut manager, &effects);
            assert_eq!(manager.sig_shares_requested.size(), 0, "{case}: requests released");
        }

        let mut manager = setup(&e);
        assert!(manager.on_batched(PEER, vec![batched(1, &[0, 1])], &e).is_empty(), "accepted");
        assert_eq!(manager.pending_incoming_count(), 2, "accepted: queued");
        assert_eq!(requested(&manager), 0, "accepted: requests erased");

        let mut manager = setup(&e);
        assert!(manager.on_batched(PEER, vec![batched(9, &[0])], &e).is_empty(), "no session");
        assert_eq!(manager.pending_incoming_count(), 0, "no session: nothing queued");
        assert_eq!(requested(&manager), 2, "no session: requests untouched");

        let inactive_lookup = FakeLookup {
            quorums: lookup.quorums.clone(),
            ..FakeLookup::default()
        };
        let outsider = signer(ProTxHash::from([99; 32]));
        let inactive = env(&inactive_lookup, &recsigs, &net, &recovery, &member_signer, 0, true);
        let not_member = env(&lookup, &recsigs, &net, &recovery, &outsider, 0, true);
        for (case, ignored_env) in [("inactive quorum", &inactive), ("not a member", &not_member)] {
            let mut manager = setup(&e);
            assert!(
                manager.on_batched(PEER, vec![batched(1, &[0])], ignored_env).is_empty(),
                "{case}"
            );
            assert_eq!(manager.pending_incoming_count(), 0, "{case}: nothing queued");
            assert_eq!(requested(&manager), 2, "{case}: requests untouched");
        }

        let no_vvec = quorum_custom(32, three_members(), [0, 1].into(), false);
        let no_vvec_lookup = lookup_with(&no_vvec);
        let no_vvec_env = env(&no_vvec_lookup, &recsigs, &net, &recovery, &member_signer, 0, true);
        let mut manager = setup(&no_vvec_env);
        assert!(manager.on_batched(PEER, vec![batched(1, &[0])], &no_vvec_env).is_empty());
        assert_eq!(manager.pending_incoming_count(), 0, "no verification vector: nothing queued");
        assert_eq!(requested(&manager), 2, "no verification vector: requests untouched");

        let mut manager = setup(&e);
        manager.sig_shares.add(&sig_share_key(&rebuilt(0)), rebuilt(0));
        assert!(manager.on_batched(PEER, vec![batched(1, &[0, 1])], &e).is_empty());
        assert_eq!(manager.pending_incoming_count(), 1, "already have: only the new share");
        assert_eq!(requested(&manager), 0, "already have: requests erased");

        let recovered = FakeRecSigs {
            signatures: [(q.llmq_type.to_base(), [7; 32], [8; 32])].into(),
            ..FakeRecSigs::default()
        };
        let recovered_env = env(&lookup, &recovered, &net, &recovery, &member_signer, 0, true);
        let mut manager = setup(&e);
        assert!(manager.on_batched(PEER, vec![batched(1, &[0, 1])], &recovered_env).is_empty());
        assert_eq!(manager.pending_incoming_count(), 0, "recovered for signing: nothing queued");
        assert_eq!(requested(&manager), 0, "recovered for signing: requests erased");

        let mut manager = setup(&e);
        manager.node_states.get_mut(&PEER).unwrap().banned = true;
        assert!(manager.on_batched(PEER, vec![batched(1, &[0])], &e).is_empty());
        assert_eq!(manager.pending_incoming_count(), 0, "banned peer: nothing queued");
    }

    #[test]
    fn sig_share_return_paths() {
        let q = quorum_custom(33, three_members(), [0, 1].into(), true);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery = no_recovery();
        let member_signer = signer(ProTxHash::from([1; 32]));
        let e = env(&lookup, &recsigs, &net, &recovery, &member_signer, 0, true);
        let unknown = SigShare::new(
            LlmqType::Unknown(250),
            *q.quorum_hash(),
            0,
            [3; 32].into(),
            [4; 32].into(),
            SigShareBytes::from_bytes([0; 96]),
        );
        for (case, msgs, mark) in [
            ("unknown type", vec![unknown.clone()], true),
            ("out-of-range member", vec![share(&q, 3)], false),
            ("invalid member", vec![share(&q, 2)], false),
            ("unknown type after a valid share", vec![share(&q, 0), unknown.clone()], true),
            ("out-of-range member after a valid share", vec![share(&q, 0), share(&q, 3)], false),
            ("invalid member after a valid share", vec![share(&q, 0), share(&q, 2)], false),
        ] {
            let mut manager = SigSharesManager::<FakeQuorum>::new();
            let effects = manager.on_sig_share(PEER, msgs, &e);
            assert!(is_ban_marked(&effects, PEER, mark), "{case}: ban");
            apply_bans(&mut manager, &effects);
            assert_eq!(manager.pending_incoming_count(), 0, "{case}: nothing left pending");
        }

        let mut manager = SigSharesManager::<FakeQuorum>::new();
        let effects = manager.on_sig_share(PEER, vec![share(&q, 2), share(&q, 0)], &e);
        assert!(is_ban_marked(&effects, PEER, false), "no state: one ban");
        assert_eq!(manager.pending_incoming_count(), 1, "no state: later share queued");
        assert!(!manager.node_states[&PEER].banned, "no state: not marked");

        let mut manager = SigSharesManager::<FakeQuorum>::new();
        manager.node_states.entry(PEER).or_default();
        let effects = manager.on_sig_share(PEER, vec![share(&q, 2), share(&q, 0)], &e);
        assert!(is_ban_marked(&effects, PEER, false), "state: one ban");
        assert_eq!(manager.pending_incoming_count(), 0, "state: nothing queued");
        assert!(manager.node_states[&PEER].banned, "state: marked at once");

        let mut manager = SigSharesManager::<FakeQuorum>::new();
        let effects = manager.on_sig_share(PEER, vec![share(&q, 2), share(&q, 3)], &e);
        assert_eq!(effects.len(), 2, "one ban per failing share");

        let mut manager = SigSharesManager::<FakeQuorum>::new();
        let effects = manager.on_sig_share(PEER, vec![unknown, share(&q, 0)], &e);
        assert!(is_ban_marked(&effects, PEER, true), "unknown type: stops");
        assert_eq!(manager.pending_incoming_count(), 0, "unknown type: nothing after it");

        let mut manager = SigSharesManager::<FakeQuorum>::new();
        assert!(manager.on_sig_share(PEER, vec![share(&q, 0)], &e).is_empty(), "accepted");
        assert_eq!(manager.pending_incoming_count(), 1, "accepted: queued");

        let inactive_lookup = FakeLookup {
            quorums: lookup.quorums.clone(),
            ..FakeLookup::default()
        };
        let missing_lookup = FakeLookup {
            active: lookup.active.clone(),
            ..FakeLookup::default()
        };
        let no_vvec = quorum_custom(33, three_members(), [0, 1].into(), false);
        let no_vvec_lookup = lookup_with(&no_vvec);
        let outsider = signer(ProTxHash::from([99; 32]));
        let for_signing = FakeRecSigs {
            signatures: [(q.llmq_type.to_base(), [3; 32], [4; 32])].into(),
            ..FakeRecSigs::default()
        };
        let for_session = FakeRecSigs {
            sign_hashes: [*share(&q, 0).sign_hash().as_bytes()].into(),
            ..FakeRecSigs::default()
        };
        let rows = [
            (
                "spork 21 inactive",
                env(&lookup, &recsigs, &net, &recovery, &member_signer, 0, false),
            ),
            (
                "inactive quorum",
                env(&inactive_lookup, &recsigs, &net, &recovery, &member_signer, 0, true),
            ),
            (
                "missing quorum",
                env(&missing_lookup, &recsigs, &net, &recovery, &member_signer, 0, true),
            ),
            ("not a member", env(&lookup, &recsigs, &net, &recovery, &outsider, 0, true)),
            (
                "no verification vector",
                env(&no_vvec_lookup, &recsigs, &net, &recovery, &member_signer, 0, true),
            ),
            (
                "recovered for signing",
                env(&lookup, &for_signing, &net, &recovery, &member_signer, 0, true),
            ),
            (
                "recovered for session",
                env(&lookup, &for_session, &net, &recovery, &member_signer, 0, true),
            ),
        ];
        for (case, ignored_env) in &rows {
            let mut manager = SigSharesManager::<FakeQuorum>::new();
            assert!(
                manager.on_sig_share(PEER, vec![share(&q, 0)], ignored_env).is_empty(),
                "{case}"
            );
            assert_eq!(manager.pending_incoming_count(), 0, "{case}: nothing queued");
        }

        let mut manager = SigSharesManager::<FakeQuorum>::new();
        manager.sig_shares.add(&sig_share_key(&share(&q, 0)), share(&q, 0));
        assert!(manager.on_sig_share(PEER, vec![share(&q, 0)], &e).is_empty(), "already have");
        assert_eq!(manager.pending_incoming_count(), 0, "already have: nothing queued");

        let mut manager = SigSharesManager::<FakeQuorum>::new();
        manager.node_states.entry(PEER).or_default().banned = true;
        assert!(manager.on_sig_share(PEER, vec![share(&q, 0)], &e).is_empty(), "banned peer");
        assert_eq!(manager.pending_incoming_count(), 0, "banned peer: nothing queued");
    }
    #[test]
    fn ignored_inbound_paths_leave_no_session_or_pending_share() {
        let q = quorum(LlmqType::LlmqTest, 13);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery = no_recovery();
        let signer = SignerFacts {
            my_pro_tx: Some(ProTxHash::from([1; 32])),
            signing_allowed: true,
        };
        let inactive_lookup = FakeLookup {
            quorums: lookup.quorums.clone(),
            ..FakeLookup::default()
        };
        let inactive = env(&inactive_lookup, &recsigs, &net, &recovery, &signer, 0, false);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        assert!(
            manager.on_ses_ann(1, vec![announcement(q.llmq_type, 13, 1, 1)], &inactive).is_empty()
        );
        assert!(!manager.node_states.contains_key(&1));
        assert!(manager.on_sig_share(1, vec![share(&q, 0)], &inactive).is_empty());
        assert_eq!(manager.pending_incoming_count(), 0);

        let active = env(&lookup, &recsigs, &net, &recovery, &signer, 0, true);
        assert!(
            manager.on_ses_ann(1, vec![announcement(q.llmq_type, 13, 2, 2)], &active).is_empty()
        );
        let mut recovered = FakeRecSigs::default();
        let ann = announcement(q.llmq_type, 13, 2, 2);
        recovered.sign_hashes.insert(*ann.sign_hash().as_bytes());
        let recovered_env = env(&lookup, &recovered, &net, &recovery, &signer, 0, true);
        assert!(manager.on_inv(1, vec![inventory(2, 3, &[0])], &recovered_env).is_empty());
        assert_eq!(manager.node_states[&1].session_by_recv_id(2).unwrap().knows.count_set(), 0);

        let missing = announcement(LlmqType::LlmqTest, 99, 3, 3);
        assert!(manager.on_ses_ann(2, vec![missing], &active).is_empty());
        assert!(!manager.node_states.contains_key(&2));
        let no_vvec = quorum_with_vvec(LlmqType::LlmqTest, 14, false);
        let no_vvec_lookup = lookup_with(&no_vvec);
        let no_vvec_env = env(&no_vvec_lookup, &recsigs, &net, &recovery, &signer, 0, true);
        assert!(
            manager
                .on_ses_ann(3, vec![announcement(no_vvec.llmq_type, 14, 4, 4)], &no_vvec_env)
                .is_empty()
        );
        assert!(manager.on_batched(3, vec![batched(4, &[0])], &no_vvec_env).is_empty());
        assert_eq!(manager.pending_incoming_count(), 0);
    }

    #[test]
    fn spork_matrix_accepts_direct_shares_and_marks_quorum_nodes_only_when_not_connected() {
        const OFF: i64 = 4_070_908_800;
        let member_signer = signer(ProTxHash::from([1; 32]));
        let recsigs = FakeRecSigs::default();
        let recovery = no_recovery();
        for (t, excluded) in [(LlmqType::Llmq400_60, true), (LlmqType::LlmqTest, false)] {
            let q = quorum(t, 15);
            let lookup = lookup_with(&q);
            for (raw, active, connected) in
                [(0, true, true), (1, true, !excluded), (2, true, false), (OFF, false, false)]
            {
                let case = format!("{t:?} raw {raw} active {active}");
                let net = FakeNet {
                    quorum_peers: [((q.llmq_type.to_base(), *q.quorum_hash()), vec![11, 12])]
                        .into(),
                    ..FakeNet::default()
                };
                let e = env(&lookup, &recsigs, &net, &recovery, &member_signer, raw, active);
                let mut manager = SigSharesManager::<FakeQuorum>::new();
                assert!(manager.on_sig_share(PEER, vec![share(&q, 0)], &e).is_empty(), "{case}");
                assert_eq!(
                    manager.pending_incoming_count(),
                    usize::from(active),
                    "{case}: direct share"
                );
                let mut own = SigSharesManager::<FakeQuorum>::new();
                let effects = own.process_own_share(share(&q, 0), q.clone(), &e);
                assert_eq!(!own.signed_sessions.is_empty(), connected, "{case}: signed session");
                assert_eq!(
                    own.sig_shares_queued_to_announce.has(&sig_share_key(&share(&q, 0))),
                    !connected,
                    "{case}: queued to announce"
                );
                assert!(effects.is_empty(), "{case}: nothing sent");
                let marked: Vec<PeerId> = own
                    .node_states
                    .iter()
                    .filter(|(_, state)| {
                        state.sessions.values().any(|s| s.requested.inv[0] && s.knows.inv[0])
                    })
                    .map(|(peer, _)| *peer)
                    .collect();
                let expected: &[PeerId] = if connected {
                    &[]
                } else {
                    &[11, 12]
                };
                assert_eq!(marked, expected, "{case}: concentrated routing");
            }
        }
    }

    #[test]
    fn non_member_direct_share_is_dropped_before_pending() {
        let q = quorum(LlmqType::LlmqTest, 16);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery = no_recovery();
        let signer = SignerFacts {
            my_pro_tx: Some(ProTxHash::from([99; 32])),
            signing_allowed: true,
        };
        let e = env(&lookup, &recsigs, &net, &recovery, &signer, 0, true);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        assert!(manager.on_sig_share(7, vec![share(&q, 0)], &e).is_empty());
        assert_eq!(manager.pending_incoming_count(), 0);
    }

    #[test]
    fn own_share_marks_exact_quorum_nodes_without_sending() {
        let q = quorum(LlmqType::LlmqTest, 17);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet {
            quorum_peers: [((q.llmq_type.to_base(), *q.quorum_hash()), vec![8, 4, 12])].into(),
            ..FakeNet::default()
        };
        let recovery = no_recovery();
        let signer = SignerFacts {
            my_pro_tx: Some(ProTxHash::from([1; 32])),
            signing_allowed: true,
        };
        let e = env(&lookup, &recsigs, &net, &recovery, &signer, 2, true);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        manager.node_states.entry(5).or_default();
        let effects = manager.process_own_share(share(&q, 0), q.clone(), &e);
        assert!(effects.is_empty(), "nothing sent");
        assert_eq!(manager.node_states.keys().copied().collect::<Vec<_>>(), [4, 5, 8, 12]);
        assert_eq!(manager.node_states[&5].session_count(), 0, "other peer untouched");
        for peer in [4, 8, 12] {
            let state = &manager.node_states[&peer];
            assert_eq!(state.session_count(), 1);
            let session = state.sessions.values().next().unwrap();
            assert!(session.requested.inv[0]);
            assert!(session.knows.inv[0]);
            assert_eq!(session.requested.count_set(), 1);
            assert_eq!(session.knows.count_set(), 1);
        }
    }

    #[test]
    fn process_verified_recovers_a_session_once_across_sources() {
        let q = quorum_five(21);
        recovers_once_across_sources(LlmqType::LlmqTestPlatform);
        recovers_once_across_sources(q.llmq_type);
    }

    fn recovers_once_across_sources(platform_type: LlmqType) {
        use std::cell::Cell;
        let q = quorum_five(21);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let signer = signer(ProTxHash::from([1; 32]));
        let calls = Cell::new(0u32);
        let recovery = FakeRecovery {
            recover: |_: &LlmqParams,
                      _: &[ProTxHash],
                      _: &BTreeMap<u16, BlsSigValue>,
                      _: &mut u32| {
                calls.set(calls.get() + 1);
                Recovered::Sig {
                    sig: BlsSigValue::invalid(),
                    self_verify: false,
                }
            },
            order: order_all,
        };
        let chain = chain(platform_type);
        let mut e = env(&lookup, &recsigs, &net, &recovery, &signer, 0, true);
        e.chain = &chain;
        let sk = q.sk_share.clone().unwrap();
        let signed = |member: u16| signed_share(&q, member, &sk);
        let key = (q.llmq_type.to_base(), *q.quorum_hash());
        let captured = |share: SigShare| CapturedShare {
            share,
            read: BlsCtx::BASIC,
        };
        let held = |manager: &mut SigSharesManager<FakeQuorum>| {
            for member in [0, 1] {
                let s = signed(member);
                manager.sig_shares.add(&sig_share_key(&s), s);
            }
        };
        let verdict = BatchVerdict {
            bad_sources: BTreeSet::new(),
            pushed: 3,
        };

        let batch = ShareBatch {
            by_source: [
                (1, vec![captured(signed(2))]),
                (2, vec![captured(signed(3))]),
                (3, vec![captured(signed(4))]),
            ]
            .into(),
            quorums: [(key, q.clone())].into(),
        };
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        held(&mut manager);
        let effects = manager.process_verified(&batch, &verdict, &e);
        assert_eq!(calls.get(), 1, "across sources: one recovery");
        assert_eq!(recovered_effects(&effects).len(), 1, "across sources: one effect");

        calls.set(0);
        let batch = ShareBatch {
            by_source: [(1, vec![captured(signed(2)), captured(signed(3))])].into(),
            quorums: [(key, q.clone())].into(),
        };
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        held(&mut manager);
        let effects = manager.process_verified(&batch, &verdict, &e);
        assert_eq!(calls.get(), 2, "within a source: Core recovers again");
        assert_eq!(recovered_effects(&effects).len(), 2, "within a source: two effects");
    }

    /// Source A recovers (id 30, msg 31); a later source B in the same batch sends member 3's
    /// share for (id 30, msg 32). Returns B's share, the manager and the effects.
    fn later_source_other_msg(
        platform_type: LlmqType,
    ) -> (SigShare, SigSharesManager<FakeQuorum>, Vec<SharesEffect>) {
        let q = quorum_five(23);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let signer = signer(ProTxHash::from([9; 32]));
        let recovery = FakeRecovery {
            recover: |_: &LlmqParams,
                      _: &[ProTxHash],
                      _: &BTreeMap<u16, BlsSigValue>,
                      _: &mut u32| Recovered::Sig {
                sig: BlsSigValue::invalid(),
                self_verify: false,
            },
            order: order_all,
        };
        let chain = chain(platform_type);
        let mut e = env(&lookup, &recsigs, &net, &recovery, &signer, 2, true);
        e.chain = &chain;
        let sk = q.sk_share.clone().unwrap();
        let captured = |share: SigShare| CapturedShare {
            share,
            read: BlsCtx::BASIC,
        };
        let later = signed_share_msg(&q, 3, 30, 32, &sk);
        let batch = ShareBatch {
            by_source: [
                (1, (0..3).map(|m| captured(signed_share_msg(&q, m, 30, 31, &sk))).collect()),
                (2, vec![captured(later.clone())]),
            ]
            .into(),
            quorums: [((q.llmq_type.to_base(), *q.quorum_hash()), q.clone())].into(),
        };
        let verdict = BatchVerdict {
            bad_sources: BTreeSet::new(),
            pushed: 4,
        };
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        let effects = manager.process_verified(&batch, &verdict, &e);
        let [rs] = recovered_effects(&effects)[..] else {
            panic!("expected source A's recovery");
        };
        assert_eq!((rs.id, rs.msg_hash), ([30; 32], [31; 32]));
        (later, manager, effects)
    }

    #[test]
    fn later_source_skips_a_recovered_id_under_another_msg_for_non_platform_types() {
        let (later, manager, effects) = later_source_other_msg(LlmqType::LlmqTestPlatform);
        let key = sig_share_key(&later);
        assert!(!manager.sig_shares.has(&key), "not stored");
        assert!(!manager.sig_shares_queued_to_announce.has(&key), "not queued to announce");
        assert_eq!(effects.len(), 1, "no effects from the later source");
    }

    #[test]
    fn later_source_keeps_a_recovered_id_under_another_msg_for_the_platform_type() {
        let (later, manager, effects) = later_source_other_msg(LlmqType::LlmqTest);
        let key = sig_share_key(&later);
        assert!(manager.sig_shares.has(&key), "stored");
        assert!(manager.sig_shares_queued_to_announce.has(&key), "queued to announce");
        assert_eq!(effects.len(), 1, "below threshold: no recovery");
    }

    #[test]
    fn recsig_read_errors_count_as_recovered() {
        let q = quorum(LlmqType::LlmqTest, 22);
        let lookup = lookup_with(&q);
        let net = FakeNet::default();
        let signer = signer(ProTxHash::from([1; 32]));
        let recovery = no_recovery();
        let e = env(&lookup, &ErrRecSigs, &net, &recovery, &signer, 0, true);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        assert!(manager.process_own_share(share(&q, 0), q.clone(), &e).is_empty());
        assert!(manager.sig_shares.is_empty(), "own share skipped");
        assert!(manager.on_sig_share(PEER, vec![share(&q, 0)], &e).is_empty());
        assert_eq!(manager.pending_incoming_count(), 0, "direct share skipped");
        assert!(manager.on_ses_ann(PEER, vec![announcement(q.llmq_type, 22, 1, 3)], &e).is_empty());
        assert!(manager.on_batched(PEER, vec![batched(1, &[0])], &e).is_empty());
        assert_eq!(manager.pending_incoming_count(), 0, "batched share skipped");
        assert!(manager.on_inv(PEER, vec![inventory(1, 3, &[0])], &e).is_empty());
        let session = manager.node_states[&PEER].session_by_recv_id(1).unwrap();
        assert_eq!(session.knows.count_set(), 0, "inventory skipped");
    }

    #[test]
    fn process_verified_bans_bad_sources_and_processes_every_good_share() {
        let q = quorum(LlmqType::LlmqTest, 18);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let recovery = no_recovery();
        let signer = SignerFacts {
            my_pro_tx: Some(ProTxHash::from([1; 32])),
            signing_allowed: true,
        };
        let e = env(&lookup, &recsigs, &net, &recovery, &signer, 0, true);
        let bad = share_with_id(&q, 0, 3);
        let good: [(PeerId, Vec<SigShare>); 2] = [
            (6, vec![share_with_id(&q, 1, 3), share_with_id(&q, 2, 3)]),
            (7, vec![share_with_id(&q, 0, 9)]),
        ];
        let captured = |shares: &[SigShare]| {
            shares
                .iter()
                .map(|share| CapturedShare {
                    share: share.clone(),
                    read: BlsCtx::BASIC,
                })
                .collect::<Vec<_>>()
        };
        let key = (q.llmq_type.to_base(), *q.quorum_hash());
        let batch = ShareBatch {
            by_source: std::iter::once((5, captured(std::slice::from_ref(&bad))))
                .chain(good.iter().map(|(peer, shares)| (*peer, captured(shares))))
                .collect(),
            quorums: [(key, q.clone())].into_iter().collect(),
        };
        let verdict = BatchVerdict {
            bad_sources: [5].into(),
            pushed: 4,
        };
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        let effects = manager.process_verified(&batch, &verdict, &e);
        assert_ban(&effects, 5);
        for (peer, shares) in &good {
            for share in shares {
                assert!(manager.sig_shares.has(&sig_share_key(share)), "source {peer}: processed");
            }
        }
        assert!(!manager.sig_shares.has(&sig_share_key(&bad)), "bad source: not processed");
        assert_eq!(manager.sig_shares.size(), 3);
        assert!(!manager.node_states.contains_key(&5));
    }

    #[test]
    fn three_of_five_recovery_uses_all_selected_shares() {
        use std::cell::Cell;
        let q = quorum_five(19);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let signer = SignerFacts {
            my_pro_tx: Some(ProTxHash::from([1; 32])),
            signing_allowed: true,
        };
        let sizes = Cell::new(Vec::new());
        let recovery = FakeRecovery {
            recover: |_: &LlmqParams,
                      _: &[ProTxHash],
                      shares: &BTreeMap<u16, BlsSigValue>,
                      _: &mut u32| {
                let mut seen = sizes.take();
                seen.push(shares.len());
                sizes.set(seen);
                Recovered::Sig {
                    sig: BlsSigValue::invalid(),
                    self_verify: false,
                }
            },
            order: order_all,
        };
        let e = env(&lookup, &recsigs, &net, &recovery, &signer, 0, true);
        let sk = q.sk_share.clone().unwrap();
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        for member in 0..2 {
            assert!(
                manager.process_own_share(signed_share(&q, member, &sk), q.clone(), &e).is_empty()
            );
        }
        assert!(sizes.take().is_empty(), "no recovery below threshold");
        let third = signed_share(&q, 2, &sk);
        let effects = manager.process_own_share(third.clone(), q.clone(), &e);
        assert_eq!(sizes.take(), [3], "one recovery with 3 shares");
        let [rs] = recovered_effects(&effects)[..] else {
            panic!("expected one recovered sig");
        };
        assert_eq!(rs.id, id_bytes(&third));
        assert_eq!(rs.msg_hash, msg_bytes(&third));
        assert_eq!(rs.quorum_hash, *q.quorum_hash());
        assert_eq!(manager.sig_shares.count_for_sign_hash(third.sign_hash().as_bytes()), 3);
    }

    #[test]
    fn recovery_self_verify_runs_at_first_and_one_hundred_first_recovery() {
        use std::cell::Cell;
        let q = quorum(LlmqType::LlmqTest, 20);
        let lookup = lookup_with(&q);
        let recsigs = FakeRecSigs::default();
        let net = FakeNet::default();
        let signer = SignerFacts {
            my_pro_tx: Some(ProTxHash::from([1; 32])),
            signing_allowed: true,
        };
        let sk = q.sk_share.clone().unwrap();
        let calls = Cell::new(0u32);
        let self_verify_calls = Cell::new(0u32);
        let recovery = FakeRecovery {
            recover: |_: &LlmqParams,
                      _: &[ProTxHash],
                      _: &BTreeMap<u16, BlsSigValue>,
                      counter: &mut u32| {
                *counter += 1;
                calls.set(calls.get() + 1);
                let self_verify = *counter == 1 || *counter == 101;
                if self_verify {
                    self_verify_calls.set(self_verify_calls.get() + 1);
                }
                Recovered::Sig {
                    sig: BlsSigValue::invalid(),
                    self_verify,
                }
            },
            order: order_all,
        };
        let e = env(&lookup, &recsigs, &net, &recovery, &signer, 0, true);
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        for id in 0..101u8 {
            manager.process_own_share(signed_share_for(&q, 0, id, &sk), q.clone(), &e);
            let effects =
                manager.process_own_share(signed_share_for(&q, 1, id, &sk), q.clone(), &e);
            let recovery_number = u32::from(id) + 1;
            let dropped = recovery_number == 1 || recovery_number == 101;
            assert_eq!(
                recovered_effects(&effects).len(),
                usize::from(!dropped),
                "recovery {recovery_number}"
            );
        }
        assert_eq!(calls.get(), 101);
        assert_eq!(self_verify_calls.get(), 2);
        assert_eq!(manager.recovered_sigs_counter, 101);
    }
}
