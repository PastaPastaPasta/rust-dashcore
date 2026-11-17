//! The pending quorum-node and relay-member registries.

use dash_params::LlmqType;
use dash_types::Numeric;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashSet};
#[cfg(feature = "net")]
use std::sync::OnceLock;

#[cfg(feature = "net")]
use dashd_p2p::processing::seams::{MnListView, QuorumWatch};

pub type QuorumKey = (u8, [u8; 32]);

/// Core's process-wide `qwatchConnectionSeed`, drawn once by `GetRandHash`.
// Core: llmq/utils.cpp:837-844
#[cfg(feature = "net")]
static WATCH_SEED: OnceLock<[u8; 32]> = OnceLock::new();

/// Returns the process-wide seed used by quorum watch selection.
#[cfg(feature = "net")]
pub fn watch_seed() -> &'static [u8; 32] {
    WATCH_SEED.get_or_init(|| dash_p2p::rng::get_rand_hash(&*dash_p2p::rng::process_source()))
}

fn type_key(llmq_type: LlmqType) -> u8 {
    llmq_type.to_base()
}

#[derive(Default)]
struct RegistryState {
    nodes: BTreeMap<QuorumKey, HashSet<[u8; 32]>>,
    relay_members: BTreeMap<QuorumKey, HashSet<[u8; 32]>>,
}

/// Core's `masternodeQuorumNodes` and `masternodeQuorumRelayMembers`.
///
/// The mutex is the only lock owned by this type. Its critical sections contain only map
/// operations: callers snapshot peers before [`QuorumNodes::set_relay_members`] and perform any
/// network pushes after it returns.
#[derive(Default)]
pub struct QuorumNodes {
    state: Mutex<RegistryState>,
}

impl QuorumNodes {
    /// Replaces one quorum's node set.
    // Core: net.cpp:4379-4386
    pub fn set_nodes(&self, llmq_type: LlmqType, quorum_hash: [u8; 32], nodes: HashSet<[u8; 32]>) {
        self.state.lock().nodes.insert((type_key(llmq_type), quorum_hash), nodes);
    }

    /// Replaces one quorum's relay set and returns eligible peer ids that now belong to the union
    /// of all relay sets. `eligible` is a caller-owned snapshot of fully connected peers with a
    /// verified proTxHash and no existing IQR flag.
    // Core: net.cpp:4388-4411
    #[cfg(feature = "net")]
    pub fn set_relay_members(
        &self,
        llmq_type: LlmqType,
        quorum_hash: [u8; 32],
        members: HashSet<[u8; 32]>,
        eligible: &[(dash_p2p::PeerId, [u8; 32])],
    ) -> Vec<dash_p2p::PeerId> {
        let state = &mut *self.state.lock();
        state.relay_members.insert((type_key(llmq_type), quorum_hash), members);
        let union: HashSet<[u8; 32]> =
            state.relay_members.values().flat_map(|set| set.iter().copied()).collect();
        eligible
            .iter()
            .filter_map(|(id, pro_tx_hash)| union.contains(pro_tx_hash).then_some(*id))
            .collect()
    }

    /// Whether a quorum node set exists.
    // Core: net.cpp:4413-4417
    pub fn has(&self, llmq_type: LlmqType, quorum_hash: &[u8; 32]) -> bool {
        self.state.lock().nodes.contains_key(&(type_key(llmq_type), *quorum_hash))
    }

    /// Returns the hashes of registered quorums of `llmq_type`.
    // Core: net.cpp:4419-4430
    pub fn quorums(&self, llmq_type: LlmqType) -> HashSet<[u8; 32]> {
        self.state
            .lock()
            .nodes
            .keys()
            .filter_map(|(ty, hash)| (*ty == type_key(llmq_type)).then_some(*hash))
            .collect()
    }

    /// Returns connected peers that are quorum nodes for the requested set. Disconnecting peers
    /// are skipped; a qwatch peer qualifies independently of its verified proTxHash.
    // Core: net.cpp:4432-4456
    #[cfg(feature = "net")]
    pub fn quorum_nodes(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &[u8; 32],
        peers: &[dash_p2p::PeerHandle],
    ) -> Vec<dash_p2p::PeerId> {
        let nodes = {
            let state = self.state.lock();
            let Some(nodes) = state.nodes.get(&(type_key(llmq_type), *quorum_hash)) else {
                return Vec::new();
            };
            nodes.clone()
        };
        peers
            .iter()
            .filter(|peer| !peer.disconnect_requested())
            .filter(|peer| {
                peer.qwatch()
                    || (peer.verified_proreg_hash() != [0; 32]
                        && nodes.contains(&peer.verified_proreg_hash()))
            })
            .map(|peer| peer.id())
            .collect()
    }

    /// Removes both node and relay sets for a quorum.
    // Core: net.cpp:4458-4463
    pub fn remove(&self, llmq_type: LlmqType, quorum_hash: &[u8; 32]) {
        let mut state = self.state.lock();
        let key = (type_key(llmq_type), *quorum_hash);
        state.nodes.remove(&key);
        state.relay_members.remove(&key);
    }

    /// Whether `peer` is in any registered quorum-node set. An unverified outbound peer is
    /// matched through the supplied tip's primary service index, as Core does.
    // Core: net.cpp:4465-4492
    #[cfg(feature = "net")]
    pub fn is_quorum_node(
        &self,
        peer: &dash_p2p::PeerHandle,
        tip: &dyn dashd_p2p::processing::seams::MnListView,
    ) -> bool {
        let verified = peer.verified_proreg_hash();
        let assumed = if verified == [0; 32] && !peer.is_inbound() {
            tip.by_service(&peer.addr().service).filter(|hash| *hash != [0; 32])
        } else {
            None
        };
        let candidate = if verified != [0; 32] {
            Some(verified)
        } else {
            assumed
        };
        let Some(candidate) = candidate else {
            return false;
        };
        self.state.lock().nodes.values().any(|nodes| nodes.contains(&candidate))
    }

    /// Whether a proTxHash is in the union of all relay-member sets.
    // Core: net.cpp:4494-4506
    pub fn is_relay_member(&self, pro_tx_hash: &[u8; 32]) -> bool {
        *pro_tx_hash != [0; 32]
            && self.state.lock().relay_members.values().any(|members| members.contains(pro_tx_hash))
    }

    /// Returns registered quorum groups in `(type, hash)` order for the masternode dial policy.
    pub fn quorum_groups(&self) -> Vec<Vec<[u8; 32]>> {
        self.state.lock().nodes.values().map(|nodes| nodes.iter().copied().collect()).collect()
    }
}

/// MNAUTH's `IsMasternodeQuorumRelayMember` over `registry`, for
/// [`MnAuthHandler::with_relay_member`](crate::mnauth::MnAuthHandler::with_relay_member).
// Core: net.cpp:4494-4506; evo/mnauth.cpp:165-173 at v24.0.0
#[cfg(feature = "net")]
pub fn relay_member(registry: std::sync::Arc<QuorumNodes>) -> crate::mnauth::RelayMember {
    std::sync::Arc::new(move |pro_tx_hash| registry.is_relay_member(pro_tx_hash))
}

#[cfg(feature = "net")]
impl QuorumWatch for QuorumNodes {
    // QM1b's `-watchquorums` state is not in integration yet.
    fn is_watching(&self) -> bool {
        false
    }

    fn is_quorum_node(&self, peer: &dash_p2p::PeerHandle, tip: &dyn MnListView) -> bool {
        QuorumNodes::is_quorum_node(self, peer, tip)
    }
}
