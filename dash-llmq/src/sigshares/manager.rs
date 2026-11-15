//! Sig-share manager state and share reconstruction.

use std::collections::BTreeMap;

use crate::dkg::wire::{BatchedSigShare, SigShare};
use crate::sigenv::{PeerId, PendingSign, SigningQuorum};

use super::map::SigShareMap;
use super::state::{SessionInfo, SigSharesNodeState, SignedSession};

/// Core's cleanup deadline, advanced using injected milliseconds.
#[derive(Default)]
pub struct CleanupThrottler {
    pub(crate) next_cleanup_ms: i64,
}

impl CleanupThrottler {
    // Core: v24.0.0 util/time.h:50-64 CleanupThrottler::TryCleanup
    pub fn try_cleanup(&mut self, now_ms: i64, interval_ms: i64) -> bool {
        if now_ms < self.next_cleanup_ms {
            return false;
        }
        self.next_cleanup_ms = now_ms.saturating_add(interval_ms);
        true
    }
}

/// Pure state shared by the sig-share manager entry points.
// Core: v24.0.0 l/signing_shares.h:472-495 CSigSharesManager
#[allow(dead_code, reason = "state shared by the sig-share manager entry points")]
pub struct SigSharesManager<Q: SigningQuorum> {
    pub(crate) sig_shares: SigShareMap<SigShare>,
    pub(crate) signed_sessions: BTreeMap<[u8; 32], SignedSession<Q>>,
    pub(crate) time_seen_for_sessions: BTreeMap<[u8; 32], i64>,
    pub(crate) node_states: BTreeMap<PeerId, SigSharesNodeState<Q>>,
    pub(crate) sig_shares_requested: SigShareMap<(PeerId, i64)>,
    pub(crate) sig_shares_queued_to_announce: SigShareMap<bool>,
    pub(crate) pending_signs: Vec<PendingSign<Q>>,
    pub(crate) cleanup_throttler: CleanupThrottler,
    pub(crate) recovered_sigs_counter: u32,
}

impl<Q: SigningQuorum> Default for SigSharesManager<Q> {
    // Core: v24.0.0 l/signing_shares.cpp:227-237 CSigSharesManager::CSigSharesManager
    fn default() -> Self {
        Self::new()
    }
}

impl<Q: SigningQuorum> SigSharesManager<Q> {
    // Core: v24.0.0 l/signing_shares.cpp:227-237 CSigSharesManager::CSigSharesManager
    pub fn new() -> Self {
        Self {
            sig_shares: SigShareMap::default(),
            signed_sessions: BTreeMap::new(),
            time_seen_for_sessions: BTreeMap::new(),
            node_states: BTreeMap::new(),
            sig_shares_requested: SigShareMap::default(),
            sig_shares_queued_to_announce: SigShareMap::default(),
            pending_signs: Vec::new(),
            cleanup_throttler: CleanupThrottler::default(),
            recovered_sigs_counter: 0,
        }
    }

    // Core: v24.0.0 l/signing_shares.cpp:1303-1307 CSigSharesManager::GetSessionInfoByRecvId
    pub fn session_info_by_recv_id(
        &mut self,
        peer: PeerId,
        session_id: u32,
    ) -> Option<SessionInfo<Q>> {
        self.node_states.entry(peer).or_default().session_info_by_recv_id(session_id)
    }

    // Core: v24.0.0 l/signing_shares.cpp:510-516 CSigSharesManager::TryAddPendingIncomingSigShare
    pub fn pending_incoming_count(&self) -> usize {
        self.node_states.values().map(|state| state.pending_incoming_sig_shares.size()).sum()
    }

    // Core: v24.0.0 l/signing_shares.cpp:1309-1315 CSigSharesManager::RebuildSigShare
    pub fn rebuild_sig_share(session: &SessionInfo<Q>, input: &BatchedSigShare) -> SigShare {
        rebuild_sig_share(session, input)
    }
}

// Core: v24.0.0 l/signing_shares.cpp:1309-1315 CSigSharesManager::RebuildSigShare
pub fn rebuild_sig_share<Q: SigningQuorum>(
    session: &SessionInfo<Q>,
    input: &BatchedSigShare,
) -> SigShare {
    SigShare::new(
        session.llmq_type,
        session.quorum_hash,
        input.quorum_member(),
        session.id.into(),
        session.msg_hash.into(),
        input.sig_share(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sigenv::fakes::FakeQuorum;

    #[test]
    fn manager_initial_state_and_unknown_peer_lookup() {
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        assert!(manager.sig_shares.is_empty());
        assert!(manager.signed_sessions.is_empty());
        assert!(manager.time_seen_for_sessions.is_empty());
        assert!(manager.node_states.is_empty());
        assert!(manager.sig_shares_requested.is_empty());
        assert!(manager.sig_shares_queued_to_announce.is_empty());
        assert!(manager.pending_signs.is_empty());
        assert_eq!(manager.recovered_sigs_counter, 0);
        assert_eq!(manager.pending_incoming_count(), 0);
        assert!(manager.session_info_by_recv_id(7, 0).is_none());
        assert_eq!(manager.node_states.len(), 1);
        assert!(manager.cleanup_throttler.try_cleanup(0, 5000));
    }

    #[test]
    fn pending_count_sums_peers_without_counting_duplicates() {
        use crate::dkg::wire::{QuorumHash, SigShareBytes};
        use dash_params::LlmqType;
        let mut manager = SigSharesManager::<FakeQuorum>::new();
        for peer in [1, 2] {
            let state = manager.node_states.entry(peer).or_default();
            for member in [0, 1] {
                let share = SigShare::new(
                    LlmqType::LlmqTest,
                    QuorumHash::from([1; 32]),
                    member,
                    [2; 32].into(),
                    [3; 32].into(),
                    SigShareBytes::from_bytes([0; 96]),
                );
                let key = super::super::state::sig_share_key(&share);
                assert!(state.pending_incoming_sig_shares.add(&key, share.clone()));
                assert!(!state.pending_incoming_sig_shares.add(&key, share));
            }
        }
        assert_eq!(manager.pending_incoming_count(), 4);
        manager.node_states.get_mut(&1).unwrap().pending_incoming_sig_shares.clear();
        assert_eq!(manager.pending_incoming_count(), 2);
        manager.node_states.remove(&2);
        assert_eq!(manager.pending_incoming_count(), 0);
    }

    #[test]
    fn cleanup_throttle_matches_core_deadline() {
        let mut throttle = CleanupThrottler::default();
        assert!(!throttle.try_cleanup(-1, 5000));
        assert!(throttle.try_cleanup(0, 5000));
        assert!(!throttle.try_cleanup(4999, 5000));
        assert!(throttle.try_cleanup(5000, 5000));
        assert!(!throttle.try_cleanup(5000, 5000));
        assert!(!throttle.try_cleanup(4000, 5000));
        assert!(throttle.try_cleanup(12000, 5000));
        assert_eq!(throttle.next_cleanup_ms, 17000);
    }
}
