//! Runtime seams around Core's quorum manager.

use std::sync::Arc;

use dash_consensus::BlockId;
use dash_dml::StoreError;
use dash_llmq::dkg::bls::{BlsPkValue, BlsSkValue};
use dash_llmq::quorum::{CleanupCounts, QuorumDbKey, StoredContributions};
use dash_params::LlmqType;
use dash_primitives::BlockHash;
use dash_primitives::QuorumHash;
use dashd_chainstate::llmq::quorum::NodeQuorum;
#[cfg(feature = "testkit-seams")]
use {
    dash_dml::Dmn,
    dash_evo::llmq::{LlmqCommitment, QuorumMembers},
    dashd_chainstate::llmq::quorum::QuorumPtr,
};

/// Core's `GetVerifiedContributions` out-parameters: member indexes, verification vectors and
/// secret key contributions.
pub type Contributions = (Vec<u16>, Vec<Arc<Vec<BlsPkValue>>>, Vec<BlsSkValue>);

/// DK1's `CDKGSessionManager::GetVerifiedContributions` (v24.0.0 `llmq/dkgsessionmgr.cpp`).
pub trait VerifiedContributions {
    fn verified(
        &self,
        llmq_type: LlmqType,
        base: &BlockHash,
        valid: &[bool],
    ) -> Option<Contributions>;
}

/// AM1's active masternode (`GetProTxHash`).
pub trait LocalMasternode {
    fn pro_tx_hash(&self) -> Option<dash_evo::types::ProTxHash>;
}

/// The quorum contribution database (`q_Qqvvec` and `q_Qsk`).
pub trait QuorumKeyStore {
    type Error: std::fmt::Display;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, Self::Error>;
    fn write(&self, key: &QuorumDbKey, c: &StoredContributions) -> Result<(), Self::Error>;
    fn erase_except(
        &self,
        keep: &std::collections::BTreeSet<QuorumDbKey>,
    ) -> Result<CleanupCounts, Self::Error>;
}

/// Runs quorum warming away from the caller's thread.
pub trait WarmExecutor {
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>);
}

/// Runtime chainstate access used by old contribution cleanup.
pub trait CleanupFacade: Send + Sync {
    fn ancestors(&self, id: BlockId) -> (Option<BlockId>, BlockHash);
    /// Core's `ScanQuorums(llmq_type, tip, count)`, taken at the tip the cleanup captured.
    fn scan(
        &self,
        llmq_type: LlmqType,
        tip: BlockId,
        count: usize,
    ) -> Vec<(BlockHash, QuorumDbKey)>;
}

/// Lookup used by Platform's P2P path. It deliberately returns all mined quorums by hash.
pub trait QuorumLookup: Send + Sync {
    fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<Arc<NodeQuorum>>, StoreError>;
}

/// Platform's `qman->GetQuorum(llmqTypePlatform, hash)` seam (v24.0.0 src/net_processing.cpp:3898).
/// `Ok(None)` is Core's nullptr. The caller must treat `Err` as Core's exception path (a fault
/// caught in `ProcessMessages`, net_processing.cpp:5959), not as a missing quorum.
pub trait PlatformQuorums {
    fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<Arc<NodeQuorum>>, StoreError>;
}

/// A quorum mined in the node's own chain, as the share seam reads it: the commitment and the
/// member list, read without building the quorum object or running the build hook.
#[cfg(feature = "testkit-seams")]
#[derive(Clone, Debug)]
pub struct MinedQuorum {
    /// The mined commitment (`qc`), with its `quorumVvecHash` and valid-member bits.
    pub commitment: Arc<LlmqCommitment>,
    /// `GetAllQuorumMembers(type, base)`, the members a build of this quorum would take.
    pub members: QuorumMembers<Arc<Dmn>>,
}

/// The node's view the regtest share seam reads (`QuorumManager::load_test_quorum_shares`).
///
/// Neither method may build a quorum or run [`QuorumBuildHook`]: no
/// [`PlatformQuorums`] or [`QuorumLookup`] call. In `dashd-chainstate`,
/// `OnVBinding::members(type, base, false)` reads the members without a build; the mined
/// commitment (`MinedCommitmentReader::mined_commitment` at the active tip) and the object cache
/// (`QuorumCaches::objects`) are crate-private there, so the wiring slice that installs the
/// manager in rdashd exposes them and implements this trait.
///
/// [`QuorumBuildHook`]: dashd_chainstate::llmq::quorum::QuorumBuildHook
#[cfg(feature = "testkit-seams")]
pub trait ShareSeamView: Send + Sync {
    /// Core's `GetMinedCommitment(type, quorumHash)` on the active chain, with the quorum's
    /// members; `Ok(None)` for a quorum this chain has not mined under `llmq_type`.
    fn mined_quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<MinedQuorum>, StoreError>;

    /// Core's `GetCachedMutableQuorum(type, quorumHash)` (v24.0.0 src/llmq/quorumsman.cpp:618-624):
    /// the cached quorum object, if any. Never builds one.
    fn cached_quorum(&self, llmq_type: LlmqType, quorum_hash: &QuorumHash) -> Option<QuorumPtr>;
}
