//! The node's quorum contribution store and its absent DKG and masternode seams: what a node that
//! is neither a masternode nor watching quorums gives Core's `CQuorumManager`.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::sync::Mutex;

use dash_evo::types::ProTxHash;
use dash_llmq::quorum::cleanup::MemQuorumStore;
use dash_llmq::quorum::{CleanupCounts, QuorumDbKey, StoredContributions};
use dash_params::LlmqType;
use dash_primitives::BlockHash;

use crate::traits::{Contributions, LocalMasternode, QuorumKeyStore, VerifiedContributions};

/// Core's quorum database, `CQuorumManager::db` (`q_Qqvvec` and `q_Qsk` rows), in memory for the
/// process's lifetime.
///
/// Core keeps it in LevelDB at `<datadir>/llmq/quorumdb`, wiped under `-reindex` and
/// `-reindex-chainstate` (v24.0.0 src/llmq/quorumsman.cpp:39, src/node/chainstate.cpp:159-160),
/// and writes it when a build makes contributions (quorumsman.cpp:96-97) and when a `qdata`
/// answer installs them (`WriteContributions`, quorumsman.cpp:626-630). A node that is neither a
/// masternode nor watching quorums does neither, so its database stays as it opened. The
/// persistent equivalent, with the reindex wipe, comes with the first product writer of quorum
/// rows: QM1b's `qdata` path or DK1.
#[derive(Default)]
pub struct MemKeyStore(Mutex<MemQuorumStore>);

impl MemKeyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl QuorumKeyStore for MemKeyStore {
    type Error = Infallible;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, Infallible> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).read(key)
    }

    fn write(&self, key: &QuorumDbKey, c: &StoredContributions) -> Result<(), Infallible> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).write(key, c)
    }

    fn erase_except(&self, keep: &BTreeSet<QuorumDbKey>) -> Result<CleanupCounts, Infallible> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).erase_except(keep)
    }
}

/// No DKG session manager: Core's null `m_qdkgsman`, which only a masternode's or a quorum
/// watcher's `NetDKG` connects (v24.0.0 src/llmq/net_dkg.cpp:475, :496; src/init.cpp:2213-2226).
/// `BuildQuorumContributions` then fails at once (quorumsman.cpp:118-122), so a build only reads
/// the store and writes nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoDkgSessions;

impl VerifiedContributions for NoDkgSessions {
    // Core: v24.0.0 src/llmq/quorumsman.cpp:118-122 BuildQuorumContributions
    fn verified(&self, _: LlmqType, _: &BlockHash, _: &[bool]) -> Option<Contributions> {
        None
    }
}

/// Not a masternode: Core's null `m_handler`, whose `QuorumRole::GetProTxHash` default is the
/// null hash (v24.0.0 src/llmq/quorumsman.h:74-75, :251).
#[derive(Clone, Copy, Debug, Default)]
pub struct NotMasternode;

impl LocalMasternode for NotMasternode {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        None
    }
}
