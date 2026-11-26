//! The DKG runtime's seams (DK1 plan §3b, §3c). The traits ND1 implements (§3b) are proposals
//! ND1 may amend; the adapters behind the §3c traits are L3's, in `dashd-node`.

use std::sync::Arc;

use dash_dml::Dmn;
use dash_evo::llmq::{LlmqCommitment, QuorumMembers};
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::conn::ConnSnapshot;
pub use dash_llmq::dkg::session::{
    DkgBlsRunner, DkgStore, FinalVerifier, OperatorKey, RelayTopology, SerialRunner,
};
use dash_llmq::dkg::session::{DkgEnv, DkgInv, DkgMsgKind, DkgSession, Sender};
use dash_llmq::qdata::PeerId;
use dash_params::{LlmqParams, LlmqType};
use dash_primitives::{BlockHash, QuorumHash};

pub use super::types::{BaseFacts, QuorumPhase, SessionKey, TipFacts, TipRef};
pub use super::wait::PhaseClock;
use crate::traits::LocalMasternode;

/// The local masternode the phase threads share.
pub trait ActiveMn: LocalMasternode + Send + Sync {}

impl<T: LocalMasternode + Send + Sync + ?Sized> ActiveMn for T {}

/// ND1's four `CDKGPendingMessages` per handler (`l/dkgsessionhandler.h:51-86`).
pub trait PendingQueues: Send + Sync {
    /// `ClearPendingMessages` of handler `key`.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:853 NetDKG::HandleDKGRound
    fn clear(&self, key: SessionKey);
    /// `PopPendingMessages(max)` from handler `key`'s queue of `kind`.
    // Core: v24.0.0 src/llmq/dkgsessionhandler.cpp:58-69 CDKGPendingMessages::PopPendingMessages
    fn pop_batch(
        &self,
        key: SessionKey,
        kind: DkgMsgKind,
        max: usize,
    ) -> Vec<(Sender<PeerId>, Vec<u8>)>;
    /// `EnqueueOwn`: our own serialized message into handler `key`'s queue of `kind`.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:369-378 EnqueueOwn
    fn enqueue_own(&self, key: SessionKey, kind: DkgMsgKind, own: &ProTxHash, payload: Vec<u8>);
}

/// ND1's `ProcessPendingMessageBatch` around [`DkgSession::process_batch`]: the scores, the
/// relays and the outcome's `events`, written with [`log_events`](super::round::log_events)
/// and each score and relay at its `at` (the session keeps none of a batch's lines).
pub trait BatchProcessor: Send + Sync {
    /// Processes `batch` of `kind` on the phase thread.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:404-458 ProcessPendingMessageBatch
    fn process_batch(
        &self,
        session: &mut DkgSession<Arc<Dmn>>,
        env: &mut DkgEnv<'_>,
        kind: DkgMsgKind,
        batch: Vec<(Sender<PeerId>, Vec<u8>)>,
    );
}

/// ND1's 60 s `contributionsCache`.
pub trait ContributionsCache: Send + Sync {
    /// `CleanupCache`.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:43 CDKGSessionManager::UpdatedBlockTip
    fn cleanup_cache(&self);
}

/// What the DKG reads of the chain (route B, L3).
pub trait DkgChain: Send + Sync {
    /// `IsSnapshotActiveAndUnvalidated()`: always false on rdashd, which has no assumeutxo
    /// (owner ruling Q6).
    fn snapshot_unvalidated(&self) -> bool;
    /// `DeploymentDIP0003Enforced(height)`.
    fn dip3_enforced(&self, height: i32) -> bool;
    /// `LookupBlockIndex(hash)`.
    fn lookup(&self, hash: &QuorumHash) -> Option<BaseFacts>;
    /// `tip->GetAncestor(height)->GetBlockHash()`, for `height` in `0..=tip.height`. A handler
    /// asks for no other height: below its quorum index a rotated tip has rotation off.
    fn ancestor_hash(&self, tip: &TipFacts, height: i32) -> BlockHash;
    /// `IsQuorumRotationEnabled(params, block)`.
    fn rotation_enabled(&self, params: &LlmqParams, block: &TipRef) -> bool;
    /// `DeploymentActiveAfter(block, V19)`.
    fn v19_after(&self, block: &TipRef) -> bool;
    /// The active chain's tip when called, `None` before genesis. Core's
    /// `bls::bls_legacy_scheme` follows it, so the scheme of a call is `!v19_after(tip)`.
    // Core: v24.0.0 src/evo/specialtxman.cpp:1043-1047, 1068-1072 CSpecialTxProcessor::ProcessSpecialTxsInBlock, UndoSpecialTxsInBlock
    fn current_tip(&self) -> Option<TipFacts>;
}

/// `GetAllQuorumMembers` through CH7b's facade (route B `OnVBinding::members`, reset = false).
pub trait MembersAccess: Send + Sync {
    /// Access #1 (LQ4 §3.4.1): the members of `t`'s quorum at `base`, in Core's order.
    // Core: v24.0.0 src/llmq/dkgsession.cpp:92 CDKGSession::Init
    fn all_members(&self, t: LlmqType, base: &BaseFacts) -> QuorumMembers<Arc<Dmn>>;
}

/// The quorum connections a DKG round asks for once its session is initialized (owner
/// amendment with SW-V24, P7).
pub trait DkgConnections: Send + Sync {
    /// Reads the tip masternode list once, then `EnsureQuorumConnections`
    /// (`net_quorum.cpp:766-834`, `is_masternode = true`), then `AddQuorumProbeConnections`
    /// (`net_dkg.cpp:800-844`) only when `we_are_member`. Called on a DKG phase thread, never
    /// on V's thread nor on a no-wait-marked thread (dashd-chainstate
    /// `src/chain/reply.rs:4-12`). May make bounded reads of V (Core takes `cs_main` briefly inside
    /// `GetAllQuorumMembers`); holds no NetCore or leaf lock across one. `llmq_type` is passed
    /// because regtest's `LLMQ_TEST`, `LLMQ_TEST_INSTANTSEND` and `LLMQ_TEST_V17` have
    /// identical parameters.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:880-888 NetDKG::HandleDKGRound
    fn on_initialized(
        &self,
        llmq_type: LlmqType,
        params: &LlmqParams,
        quorum_base: &TipRef,
        my_pro_tx: &ProTxHash,
        watch: bool,
        we_are_member: bool,
    );
}

/// [`DkgConnections`] that connects nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoDkgConnections;

impl DkgConnections for NoDkgConnections {
    fn on_initialized(
        &self,
        _: LlmqType,
        _: &LlmqParams,
        _: &TipRef,
        _: &ProTxHash,
        _: bool,
        _: bool,
    ) {
    }
}

/// The connection snapshot `VerifyAndComplain` reads when the Complain phase starts.
pub trait ConnFactsSource: Send + Sync {
    /// The sporks' verdicts for `t`, the fully connected peers in `m_nodes` order, and the
    /// masternode metadata of `members` (LQ4 §2.6).
    // Core: v24.0.0 src/active/dkgsession.cpp:226-290 VerifyConnectionAndMinProtoVersions
    fn snapshot(&self, t: LlmqType, members: &[ProTxHash]) -> ConnSnapshot;
}

/// The mineable-commitment pool and the relay of its inv.
pub trait MineableSink: Send + Sync {
    /// `AddMineableCommitment(qc)`: the commitment hash to relay, if the pool took it.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:869-873, 942-947 NetDKG::HandleDKGRound
    fn add(&self, qc: &LlmqCommitment) -> Option<[u8; 32]>;
    /// `PeerRelayInv(CInv(MSG_QUORUM_FINAL_COMMITMENT, hash))`.
    fn relay_inv(&self, hash: &[u8; 32]);
}

/// The sporks the DKG reads.
pub trait Sporks: Send + Sync {
    /// `IsQuorumDKGEnabled`: SPORK_17 is active.
    fn quorum_dkg_enabled(&self) -> bool;
    /// SPORK_21's value (`IsAllMembersConnectedEnabled`).
    fn spork21_value(&self) -> i64;
    /// SPORK_23's value (`IsQuorumPoseEnabled`).
    fn spork23_value(&self) -> i64;
}

/// The `CDKGDebugManager` calls a handler makes outside the session.
pub trait PhaseMarks: Send + Sync {
    /// `ResetLocalSessionStatus(type, quorumIndex)`.
    fn reset_local_session_status(&self, t: LlmqType, quorum_index: i32);
    /// `MarkPhaseAdvanced(type, quorumIndex, phase)`.
    fn mark_phase_advanced(&self, t: LlmqType, quorum_index: i32, phase: QuorumPhase);
}

/// `PeerConsumeGetDataResponse`'s answer: whether we asked the peer for an object.
// Core: v24.0.0 src/net_processing.h:57-67 GetDataResponse
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GetDataResponse {
    /// A `getdata` of ours was in flight; it is now consumed.
    Requested,
    /// None in flight, but we asked this peer recently: a late answer.
    Late,
    /// We have no record of asking this peer.
    Unrequested,
}

/// A fully connected peer as `CConnman::ForEachNode` visits it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayPeer {
    /// `GetId()`.
    pub id: PeerId,
    /// `m_addr_name`.
    pub addr_name: String,
    /// `GetVerifiedProRegTxHash()`; `None` for Core's null.
    pub verified_pro_tx: Option<ProTxHash>,
    /// `qwatch`.
    pub qwatch: bool,
}

/// What `RelayInvToParticipants` reads of the connection manager, at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelayFacts {
    /// `GetNodeCount(ConnectionDirection::Both)`.
    pub node_count: usize,
    /// `GetNetworkActive()`.
    pub network_active: bool,
    /// `HasMasternodeQuorumNodes(type, base)`.
    pub has_quorum_nodes: bool,
    /// The fully connected peers, in `m_nodes` order.
    pub peers: Vec<RelayPeer>,
}

/// What ND1's DKG intake asks of the node: the peer table, the scores and the relay
/// (`PeerManagerInternal`, `CConnman`), and one chain read. L3's adapter is in `dashd-node`.
pub trait IntakeHost: Send + Sync {
    /// `pfrom.GetVerifiedProRegTxHash()`: the proTxHash `peer` proved with MNAuth, `None` for
    /// Core's null.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:537-541 NetDKG::ProcessMessage
    fn verified_pro_tx(&self, peer: PeerId) -> Option<ProTxHash>;
    /// `PeerConsumeGetDataResponse(peer, inv)` under `cs_main`: consumes the record of our
    /// `getdata` for `inv` to `peer`.
    // Core: v24.0.0 src/net_processing.cpp:6943-6972 PeerManagerImpl::PeerConsumeGetDataResponse
    fn consume_getdata_response(&self, peer: PeerId, inv: DkgInv) -> GetDataResponse;
    /// `IsQuorumTypeEnabled(t, base->pprev)`: at the parent of `base`, which has one. `None`
    /// when the local read fails, which is never the peer's fault (DEC-1881).
    // Core: v24.0.0 src/llmq/net_dkg.cpp:587-591 NetDKG::ProcessMessage
    fn quorum_type_enabled(&self, t: LlmqType, base: &BaseFacts) -> Option<bool>;
    /// `PeerMisbehaving(peer, score)`.
    fn misbehaving(&self, peer: PeerId, score: i32);
    /// The connection manager's facts for a relay of a `t` quorum at `base`.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:336-368 RelayInvToParticipants
    fn relay_facts(&self, t: LlmqType, base: &QuorumHash) -> RelayFacts;
    /// `PeerPushInventory(peer, inv)`.
    fn push_inventory(&self, peer: PeerId, inv: DkgInv);
}
