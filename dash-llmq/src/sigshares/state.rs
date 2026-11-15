//! Session and peer state, including per-type announcement limits and receive-id lookup.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_evo::locks::sign_hash;
use dash_params::{LlmqParams, LlmqType};

use crate::dkg::wire::{QuorumHash, SigSesAnn, SigShare};
use crate::sigenv::SigningQuorum;

use super::inv::SigSharesInv;
use super::map::{SigShareKey, SigShareMap};

/// All other `u32` values are valid session IDs (`l/signing_shares.h:46`).
pub const UNINITIALIZED_SESSION_ID: u32 = u32::MAX;

// Core: v24.0.0 l/signing_shares.cpp:39-42 GetMaxSessionsForPeer
pub fn max_sessions_for_peer(params: &LlmqParams) -> usize {
    (usize::from(params.size) * 4).max(100)
}

// Core: v24.0.0 l/signing_shares.cpp:45-49 CSigShare::UpdateKey
pub fn sig_share_key(share: &SigShare) -> SigShareKey {
    (*share.sign_hash().as_bytes(), share.quorum_member())
}

/// A session snapshot that keeps its quorum alive across manager operations.
pub struct SessionInfo<Q: SigningQuorum> {
    pub llmq_type: LlmqType,
    pub quorum_hash: QuorumHash,
    pub id: [u8; 32],
    pub msg_hash: [u8; 32],
    pub sign_hash: [u8; 32],
    pub quorum: Option<Arc<Q>>,
}

/// One peer's knowledge of a signing session (`l/signing_shares.h:390-407`).
pub struct Session<Q: SigningQuorum> {
    pub recv_session_id: u32,
    pub send_session_id: u32,
    pub llmq_type: LlmqType,
    pub quorum_hash: QuorumHash,
    pub id: [u8; 32],
    pub msg_hash: [u8; 32],
    pub sign_hash: [u8; 32],
    pub quorum: Option<Arc<Q>>,
    pub announced: SigSharesInv,
    pub requested: SigSharesInv,
    pub knows: SigSharesInv,
    pub received_announcement: bool,
}

impl<Q: SigningQuorum> Session<Q> {
    // Core: v24.0.0 l/signing_shares.cpp:123-137 InitSession
    fn new(
        llmq_type: LlmqType,
        quorum_hash: QuorumHash,
        id: [u8; 32],
        msg_hash: [u8; 32],
        size: usize,
    ) -> Self {
        let mut inv = SigSharesInv::default();
        inv.init(size);
        Self {
            recv_session_id: UNINITIALIZED_SESSION_ID,
            send_session_id: UNINITIALIZED_SESSION_ID,
            llmq_type,
            quorum_hash,
            id,
            msg_hash,
            sign_hash: sign_hash(llmq_type, &quorum_hash, &id, &msg_hash),
            quorum: None,
            announced: inv.clone(),
            requested: inv.clone(),
            knows: inv,
            received_announcement: false,
        }
    }
}

/// Per-peer sessions and queued shares (`l/signing_shares.h:408-415`).
pub struct SigSharesNodeState<Q: SigningQuorum> {
    pub(crate) sessions: BTreeMap<[u8; 32], Session<Q>>,
    pub(crate) session_by_recv_id: BTreeMap<u32, [u8; 32]>,
    pub pending_incoming_sig_shares: SigShareMap<SigShare>,
    pub requested_sig_shares: SigShareMap<i64>,
    pub banned: bool,
}

impl<Q: SigningQuorum> Default for SigSharesNodeState<Q> {
    // Core: v24.0.0 l/signing_shares.h:408-415 CSigSharesNodeState
    fn default() -> Self {
        Self {
            sessions: BTreeMap::new(),
            session_by_recv_id: BTreeMap::new(),
            pending_incoming_sig_shares: SigShareMap::default(),
            requested_sig_shares: SigShareMap::default(),
            banned: false,
        }
    }
}

impl<Q: SigningQuorum> SigSharesNodeState<Q> {
    // Core: v24.0.0 l/signing_shares.cpp:139-146 CSigSharesNodeState::GetOrCreateSessionFromShare
    pub fn get_or_create_session_from_share(
        &mut self,
        share: &SigShare,
        params: &LlmqParams,
    ) -> &mut Session<Q> {
        self.sessions.entry(sig_share_key(share).0).or_insert_with(|| {
            Session::new(
                share.llmq_type(),
                share.quorum_hash(),
                *share.id().as_bytes(),
                *share.msg_hash().as_bytes(),
                usize::from(params.size),
            )
        })
    }

    // Core: v24.0.0 l/signing_shares.cpp:148-157 CSigSharesNodeState::GetOrCreateSessionFromAnn
    pub fn get_or_create_session_from_ann(
        &mut self,
        ann: &SigSesAnn,
        params: &LlmqParams,
    ) -> &mut Session<Q> {
        let hash = *ann.sign_hash().as_bytes();
        let session = self.sessions.entry(hash).or_insert_with(|| {
            Session::new(
                ann.llmq_type(),
                ann.quorum_hash(),
                *ann.id().as_bytes(),
                *ann.msg_hash().as_bytes(),
                usize::from(params.size),
            )
        });
        session.received_announcement = true;
        session
    }

    // Core: v24.0.0 l/signing_shares.cpp:159-162 CSigSharesNodeState::CanCreateSessionFromAnn
    pub fn can_create_session_from_ann(&self, ann: &SigSesAnn, max_sessions: usize) -> bool {
        let hash = *ann.sign_hash().as_bytes();
        self.sessions.contains_key(&hash)
            || self.announcement_session_count(ann.llmq_type()) < max_sessions
    }

    // Core: v24.0.0 l/signing_shares.cpp:164-167 CSigSharesNodeState::GetSessionCount
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    // Core: v24.0.0 l/signing_shares.cpp:169-172 CSigSharesNodeState::GetSessionCount
    pub fn session_count_for_type(&self, llmq_type: LlmqType) -> usize {
        self.sessions.values().filter(|session| session.llmq_type == llmq_type).count()
    }

    // Core: v24.0.0 l/signing_shares.cpp:174-179 CSigSharesNodeState::GetAnnouncementSessionCount
    pub fn announcement_session_count(&self, llmq_type: LlmqType) -> usize {
        self.sessions
            .values()
            .filter(|s| s.received_announcement && s.llmq_type == llmq_type)
            .count()
    }

    // Core: v24.0.0 l/signing_shares.cpp:181-188 CSigSharesNodeState::GetSessionBySignHash
    pub fn session_by_sign_hash(&self, sign_hash: &[u8; 32]) -> Option<&Session<Q>> {
        self.sessions.get(sign_hash)
    }

    // Core: v24.0.0 l/signing_shares.cpp:181-188 CSigSharesNodeState::GetSessionBySignHash
    pub fn session_by_sign_hash_mut(&mut self, sign_hash: &[u8; 32]) -> Option<&mut Session<Q>> {
        self.sessions.get_mut(sign_hash)
    }

    // Core: v24.0.0 l/signing_shares.cpp:190-197 CSigSharesNodeState::GetSessionByRecvId
    pub fn session_by_recv_id(&self, session_id: u32) -> Option<&Session<Q>> {
        self.sessions.get(self.session_by_recv_id.get(&session_id)?)
    }

    // Core: v24.0.0 l/signing_shares.cpp:190-197 CSigSharesNodeState::GetSessionByRecvId
    pub fn session_by_recv_id_mut(&mut self, session_id: u32) -> Option<&mut Session<Q>> {
        self.sessions.get_mut(self.session_by_recv_id.get(&session_id)?)
    }

    // Core: v24.0.0 l/signing_shares.cpp:199-213 CSigSharesNodeState::GetSessionInfoByRecvId
    pub fn session_info_by_recv_id(&self, session_id: u32) -> Option<SessionInfo<Q>> {
        let session = self.session_by_recv_id(session_id)?;
        Some(SessionInfo {
            llmq_type: session.llmq_type,
            quorum_hash: session.quorum_hash,
            id: session.id,
            msg_hash: session.msg_hash,
            sign_hash: session.sign_hash,
            quorum: session.quorum.clone(),
        })
    }

    // Core: v24.0.0 l/signing_shares.cpp:215-223 CSigSharesNodeState::RemoveSession
    pub fn remove_session(&mut self, sign_hash: &[u8; 32]) {
        if let Some(session) = self.sessions.remove(sign_hash) {
            self.session_by_recv_id.remove(&session.recv_session_id);
        }
        self.requested_sig_shares.erase_all_for_sign_hash(sign_hash);
        self.pending_incoming_sig_shares.erase_all_for_sign_hash(sign_hash);
    }
}

/// An own share awaiting concentrated delivery (`l/signing_shares.h:430-439`).
pub struct SignedSession<Q: SigningQuorum> {
    pub sig_share: SigShare,
    pub quorum: Arc<Q>,
    pub next_attempt_time: i64,
    pub attempt: i32,
    pub send_count: i32,
}

impl<Q: SigningQuorum> SignedSession<Q> {
    // Core: v24.0.0 l/signing_shares.h:430-439 CSignedSession
    pub fn new(sig_share: SigShare, quorum: Arc<Q>) -> Self {
        Self {
            sig_share,
            quorum,
            next_attempt_time: 0,
            attempt: 0,
            send_count: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dkg::bls::BlsPkValue;
    use crate::dkg::wire::{BatchedSigShare, SigShareBytes};
    use crate::sigenv::fakes::FakeQuorum;
    use crate::sigshares::manager::rebuild_sig_share;
    use dash_params::{ChainParamsBuilder, Network, Release};

    const T: LlmqType = LlmqType::LlmqTest;

    fn params() -> LlmqParams {
        *ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .build()
            .unwrap()
            .consensus
            .llmq(T)
            .unwrap()
    }

    fn ann(t: LlmqType, n: u32) -> SigSesAnn {
        let mut id = [0; 32];
        id[..4].copy_from_slice(&n.to_le_bytes());
        SigSesAnn::new(n, t, QuorumHash::from([1; 32]), id.into(), [2; 32].into())
    }

    fn share(a: &SigSesAnn, member: u16) -> SigShare {
        SigShare::new(
            a.llmq_type(),
            a.quorum_hash(),
            member,
            a.id(),
            a.msg_hash(),
            SigShareBytes::from_bytes([0; 96]),
        )
    }

    fn quorum() -> Arc<FakeQuorum> {
        Arc::new(FakeQuorum {
            llmq_type: T,
            quorum_hash: QuorumHash::from([1; 32]),
            params: params(),
            members: Vec::new(),
            valid_members: Default::default(),
            has_verification_vector: false,
            quorum_public_key: BlsPkValue::invalid(),
            pk_shares: Default::default(),
            sk_share: None,
        })
    }

    #[test]
    fn session_initialization_preserves_existing_state() {
        let params = params();
        let a = ann(T, 5);
        let s = share(&a, 1);
        let key = sig_share_key(&s);
        assert_eq!(key, (*a.sign_hash().as_bytes(), 1));
        assert_ne!(key, sig_share_key(&share(&a, 2)));
        let mut state = SigSharesNodeState::<FakeQuorum>::default();
        let session = state.get_or_create_session_from_share(&s, &params);
        assert_eq!(session.recv_session_id, UNINITIALIZED_SESSION_ID);
        assert_eq!(session.send_session_id, UNINITIALIZED_SESSION_ID);
        assert_eq!(session.llmq_type, T);
        assert_eq!(session.quorum_hash, a.quorum_hash());
        assert_eq!(session.id, *a.id().as_bytes());
        assert_eq!(session.msg_hash, *a.msg_hash().as_bytes());
        assert_eq!(session.sign_hash, key.0);
        assert!(session.quorum.is_none());
        assert!(!session.received_announcement);
        for inv in [&session.announced, &session.requested, &session.knows] {
            assert_eq!(inv.inv.len(), usize::from(params.size));
            assert_eq!(inv.count_set(), 0);
            assert_eq!(inv.session_id, UNINITIALIZED_SESSION_ID);
        }
        session.announced.set(1, true);
        session.send_session_id = 3;
        let session = state.get_or_create_session_from_ann(&a, &params);
        assert!(session.received_announcement);
        assert_eq!(session.announced.count_set(), 1);
        assert_eq!(session.send_session_id, 3);
        assert_eq!(state.session_count(), 1);
        assert_eq!(state.session_count_for_type(T), 1);
        assert_eq!(state.announcement_session_count(T), 1);
        state.get_or_create_session_from_share(&s, &params);
        assert_eq!(state.announcement_session_count(T), 1);
    }

    #[test]
    fn announcement_cap_at_limit_and_one_over_is_per_type() {
        for size in [3, 25, 26, 50] {
            let mut params = params();
            params.size = size;
            let max = max_sessions_for_peer(&params);
            assert_eq!(max, (usize::from(size) * 4).max(100));
            let mut state = SigSharesNodeState::<FakeQuorum>::default();
            for n in 0..max {
                let a = ann(T, n as u32);
                assert!(state.can_create_session_from_ann(&a, max));
                state.get_or_create_session_from_ann(&a, &params);
            }
            assert_eq!(state.announcement_session_count(T), max);
            assert!(!state.can_create_session_from_ann(&ann(T, max as u32), max));
            assert!(state.can_create_session_from_ann(&ann(T, 0), max));
            assert!(state.can_create_session_from_ann(&ann(LlmqType::LlmqTestPlatform, 0), max));
            state.get_or_create_session_from_ann(&ann(LlmqType::LlmqTestPlatform, 0), &params);
            assert_eq!(state.session_count_for_type(LlmqType::LlmqTestPlatform), 1);
            let extra = ann(T, max as u32);
            state.get_or_create_session_from_share(&share(&extra, 0), &params);
            assert_eq!(state.announcement_session_count(T), max);
            assert!(state.can_create_session_from_ann(&extra, max));
            state.get_or_create_session_from_ann(&extra, &params);
            assert_eq!(state.announcement_session_count(T), max + 1);
            assert!(!state.can_create_session_from_ann(&ann(T, max as u32 + 1), max));
            state.remove_session(ann(T, 0).sign_hash().as_bytes());
            assert_eq!(state.announcement_session_count(T), max);
            assert!(!state.can_create_session_from_ann(&ann(T, max as u32 + 1), max));
            state.remove_session(ann(T, 1).sign_hash().as_bytes());
            assert!(state.can_create_session_from_ann(&ann(T, max as u32 + 1), max));
        }
    }

    #[test]
    fn receive_lookup_snapshot_rebuild_and_removal() {
        let mut state = SigSharesNodeState::<FakeQuorum>::default();
        let a = ann(T, 7);
        let b = ann(T, 8);
        let hash = *a.sign_hash().as_bytes();
        let other_hash = *b.sign_hash().as_bytes();
        let q = quorum();
        assert!(state.session_info_by_recv_id(7).is_none());
        let session = state.get_or_create_session_from_ann(&a, &params());
        session.recv_session_id = 7;
        session.quorum = Some(q.clone());
        state.session_by_recv_id.insert(7, hash);
        state.get_or_create_session_from_ann(&b, &params());
        let snapshot = state.session_info_by_recv_id(7).unwrap();
        assert_eq!(snapshot.llmq_type, T);
        assert_eq!(snapshot.quorum_hash, a.quorum_hash());
        assert_eq!(snapshot.id, *a.id().as_bytes());
        assert_eq!(snapshot.msg_hash, *a.msg_hash().as_bytes());
        assert_eq!(snapshot.sign_hash, hash);
        assert!(Arc::ptr_eq(snapshot.quorum.as_ref().unwrap(), &q));
        state.session_by_recv_id_mut(7).unwrap().knows.set(0, true);
        assert_eq!(state.session_by_sign_hash(&hash).unwrap().knows.count_set(), 1);
        state.session_by_sign_hash_mut(&hash).unwrap().send_session_id = 12;
        let own = SignedSession::new(share(&a, 0), q.clone());
        assert_eq!(own.next_attempt_time, 0);
        assert_eq!(own.attempt, 0);
        assert_eq!(own.send_count, 0);
        assert!(Arc::ptr_eq(&own.quorum, &q));
        let bytes = SigShareBytes::from_bytes([9; 96]);
        let rebuilt = rebuild_sig_share(&snapshot, &BatchedSigShare::new(2, bytes));
        assert_eq!(rebuilt, SigShare::new(T, a.quorum_hash(), 2, a.id(), a.msg_hash(), bytes));
        assert_eq!(sig_share_key(&rebuilt), (hash, 2));
        for member in [0, 1] {
            state.pending_incoming_sig_shares.add(&(hash, member), share(&a, member));
            state.requested_sig_shares.add(&(hash, member), 123);
        }
        state.pending_incoming_sig_shares.add(&(other_hash, 0), share(&b, 0));
        state.requested_sig_shares.add(&(other_hash, 0), 456);
        state.remove_session(&hash);
        assert!(state.session_by_recv_id(7).is_none());
        assert!(state.session_by_sign_hash(&hash).is_none());
        assert_eq!(state.session_count(), 1);
        assert_eq!(state.pending_incoming_sig_shares.size(), 1);
        assert_eq!(state.requested_sig_shares.size(), 1);
        assert!(Arc::ptr_eq(snapshot.quorum.as_ref().unwrap(), &q));
        state.remove_session(&hash);
        assert_eq!(state.requested_sig_shares.size(), 1);
        state.sessions.clear();
        state.remove_session(&other_hash);
        assert!(state.requested_sig_shares.is_empty());
        assert!(state.pending_incoming_sig_shares.is_empty());
    }
}
