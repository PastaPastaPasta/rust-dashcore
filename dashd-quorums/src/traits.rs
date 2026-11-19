//! Runtime seams around Core's quorum manager.

use std::sync::Arc;

use dash_consensus::BlockId;
use dash_dml::StoreError;
use dash_llmq::dkg::bls::{BlsPkValue, BlsSkValue};
use dash_llmq::quorum::{CleanupCounts, QuorumDbKey, StoredContributions};
use dash_params::LlmqType;
#[cfg(feature = "testkit-seams")]
use dash_pkc::bls::BlsSchemeKind;
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
///
/// `spawn_warm` must never block the caller and must never run the job inline on the caller's
/// thread. A cleanup job waits on V from the executor's thread, and V's build can call
/// `spawn_warm` on that same executor.
pub trait WarmExecutor {
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>);
}

/// A shared executor, so a node passes its own `Arc<dyn WarmExecutor>` and keeps a handle on it.
impl<T: WarmExecutor + ?Sized> WarmExecutor for Arc<T> {
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
        (**self).spawn_warm(job);
    }
}

/// Runtime chainstate access used by old contribution cleanup.
pub trait CleanupFacade: Send + Sync {
    fn ancestors(&self, id: BlockId) -> (Option<BlockId>, BlockHash);
    /// Core's `ScanQuorums(llmq_type, tip, count)`, taken at the tip the cleanup captured: each
    /// quorum's base hash and database key. `None` is a scan that failed, Core's throwing
    /// `ScanQuorums`, which ends the cleanup job before it erases anything (v24.0.0
    /// src/llmq/net_quorum.cpp:719-764); `Some` of an empty list is a scan that found none.
    fn scan(
        &self,
        llmq_type: LlmqType,
        tip: BlockId,
        count: usize,
    ) -> Option<Vec<(BlockHash, QuorumDbKey)>>;
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
    /// The view's BLS scheme, Core's `bls_legacy_scheme` as the read found it, which the seam
    /// decodes and checks the item's keys under.
    pub scheme: BlsSchemeKind,
}

/// The node's view the regtest share seam reads (`QuorumManager::load_test_quorum_shares`).
///
/// Neither method may build a quorum or run [`QuorumBuildHook`]: no
/// [`PlatformQuorums`] or [`QuorumLookup`] call. The node's is
/// [`ChainQuorums`](crate::chain::ChainQuorums), over route B's `OnVBinding::mined_quorum` and
/// `OnVBinding::cached_quorum`, which never build.
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
