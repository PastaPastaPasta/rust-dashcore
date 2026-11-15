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
