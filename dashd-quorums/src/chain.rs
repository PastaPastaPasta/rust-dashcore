//! The manager's chain seams over the node's V: route B's quorum reads
//! (`dashd_chainstate::chain::OnVBinding`), through a chain handle bound once V has started.

use std::sync::{Arc, OnceLock};

use dash_consensus::BlockId;
use dash_dml::{NodeMemberError, StoreError};
use dash_evo::types::ProTxHash;
use dash_llmq::quorum::{QuorumDbKey, quorum_db_key};
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};
use dashd_chainstate::chain::{ChainHandle, ChainQuery, OnV, OnVMembersError};
use dashd_chainstate::llmq::quorum::NodeQuorum;
use tracing::debug;
#[cfg(feature = "testkit-seams")]
use {
    crate::traits::{MinedQuorum, ShareSeamView},
    dashd_chainstate::llmq::quorum::QuorumPtr,
};

use crate::tip::ScanError;
use crate::traits::{CleanupFacade, DkgChain, QuorumLookup};

/// Route B behind [`QuorumLookup`], [`CleanupFacade`] and the share seam's view. The manager is
/// built before V starts, so V's handle is bound later ([`Self::bind`]). Until then, and once V
/// has stopped, a lookup is an error, a cleanup scan fails (so its job erases nothing) and the
/// share seam reads nothing.
///
/// Its methods must not be called from the chain actor's own thread: they wait on that actor,
/// and a call from it never returns.
#[derive(Default)]
pub struct ChainQuorums {
    chain: OnceLock<ChainHandle>,
}

impl ChainQuorums {
    /// Seams with no V bound.
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds V's handle, once; a second bind is ignored.
    pub fn bind(&self, chain: ChainHandle) {
        if self.chain.set(chain).is_err() {
            debug!(target: "llmq", "ChainQuorums::bind -- already bound");
        }
    }

    /// `f` on V through route B; `None` when no V is bound or V has stopped.
    pub(crate) fn on_v<T: Send + 'static>(
        &self,
        f: impl for<'v> FnOnce(&OnV<'v>, &ChainQuery<'_>) -> T + Send + 'static,
    ) -> Option<T> {
        self.chain.get()?.on_v(f).recv().ok()
    }

    /// `LookupBlockIndex(hash)` on the active chain of V's published view; false with no V bound.
    pub fn on_active_chain(&self, hash: &BlockHash) -> bool {
        let Some(chain) = self.chain.get() else {
            return false;
        };
        let reads = chain.view_handle();
        reads.lookup(hash).is_some_and(|id| reads.contains(&reads.view(), id))
    }

    /// Route B's `cached_quorum`; `None` with no V running.
    pub fn cached(&self, llmq_type: LlmqType, quorum_hash: &QuorumHash) -> Option<Arc<NodeQuorum>> {
        let hash = *quorum_hash;
        self.on_v(move |o, q| q.bind(o).cached_quorum(llmq_type, &hash).map(|q| q.0)).flatten()
    }

    /// Route B's `ScanQuorums(type, tip, count)`, the quorum objects: [`ScanError::VGone`] when no
    /// V runs it, [`ScanError::Read`] with the store error when the scan fails.
    pub fn scan_objects(
        &self,
        llmq_type: LlmqType,
        tip: BlockId,
        count: usize,
    ) -> Result<Vec<Arc<NodeQuorum>>, ScanError> {
        let scanned = self
            .on_v(move |o, q| q.bind(o).scan_quorums(llmq_type, tip, count))
            .ok_or(ScanError::VGone)?
            .map_err(|e| ScanError::Read(store_error(e).to_string()))?;
        Ok(scanned.into_iter().map(|q| q.0).collect())
    }
}

/// A route-B call's fault as a store error, mapped as route C maps a seam's.
pub(crate) fn store_error(e: OnVMembersError) -> StoreError {
    match e {
        OnVMembersError::Params(e) => {
            StoreError::inconsistent(format!("LLMQ parameters out of domain: {e:?}"))
        }
        OnVMembersError::Members(e) => match NodeMemberError::from(e) {
            NodeMemberError::Store(e) => e,
            NodeMemberError::UnknownLlmqType(t) => {
                StoreError::inconsistent(format!("a quorum lookup of LLMQ type {t}"))
            }
        },
    }
}

impl QuorumLookup for ChainQuorums {
    /// Core's `GetQuorum(type, quorumHash)` through route B, which builds a missing object through
    /// the facade's quorum hook. It runs on V's queue, behind V's other work: there is no fast
    /// path for a cached object yet.
    // Core: v24.0.0 src/llmq/quorumsman.cpp:491-512 GetQuorum
    fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<Arc<NodeQuorum>>, StoreError> {
        let hash = *quorum_hash;
        let got = self.on_v(move |o, q| {
            q.bind(o).quorum(llmq_type, &hash).map(|q| q.map(|q| q.0)).map_err(store_error)
        });
        got.unwrap_or(Err(StoreError::NotBuilt(
            "the quorum lookup of a node whose V is not running",
        )))
    }
}

impl CleanupFacade for ChainQuorums {
    /// The entry's predecessor and hash from V's block index; with no V bound, none and the null
    /// hash, which no cache entry has.
    fn ancestors(&self, id: BlockId) -> (Option<BlockId>, BlockHash) {
        let entry = self.chain.get().and_then(|chain| chain.reader().entry(id));
        entry.map_or((None, BlockHash::from([0; 32])), |e| (e.prev, e.hash))
    }

    /// Core's `ScanQuorums(type, pIndex, count)` through route B (the cached overload, which
    /// builds each quorum through the facade's hook), each quorum keyed as `MakeQuorumKey` keys
    /// it; `None` when the scan fails or no V runs it.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:747-752, src/llmq/quorums.cpp:20-29 MakeQuorumKey
    fn scan(
        &self,
        llmq_type: LlmqType,
        tip: BlockId,
        count: usize,
    ) -> Option<Vec<(BlockHash, QuorumDbKey)>> {
        let scanned =
            self.on_v(move |o, q| q.bind(o).scan_quorums(llmq_type, tip, count).ok())??;
        let keyed = scanned.iter().map(|q| {
            let members: Vec<_> = q.members.iter().map(|m| m.pro_tx_hash).collect();
            (q.base_hash, quorum_db_key(llmq_type, &q.commitment.quorum_hash(), &members))
        });
        Some(keyed.collect())
    }
}

impl DkgChain for ChainQuorums {
    /// Route B's `GetAllQuorumMembers(type, base, false)` for the indexed block `base`; an error
    /// for a block the index does not hold or with no V running. It waits on V, so a quorum build
    /// hook, which runs on V, must not reach it.
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:147, :196
    fn members(&self, llmq_type: LlmqType, base: &BlockHash) -> Result<Vec<ProTxHash>, StoreError> {
        let id =
            self.chain.get().and_then(|chain| chain.reader().lookup(base)).ok_or_else(|| {
                StoreError::inconsistent(format!("a DKG base block {base} not indexed"))
            })?;
        let got = self.on_v(move |o, q| {
            let members = q.bind(o).members(llmq_type, id, false).map_err(store_error)?;
            Ok(members.iter().map(|m| m.pro_tx_hash).collect())
        });
        got.unwrap_or(Err(StoreError::NotBuilt("the DKG members of a node whose V is not running")))
    }

    fn height(&self, hash: &BlockHash) -> Option<u32> {
        let reader = self.chain.get()?.reader();
        reader.entry(reader.lookup(hash)?).map(|e| e.height)
    }

    /// The published tip's height; `None` with no V bound.
    fn active_height(&self) -> Option<i32> {
        let chain = self.chain.get()?;
        Some(i32::try_from(chain.reader().snapshot().tip.tip_height).unwrap_or(i32::MAX))
    }
}

#[cfg(feature = "testkit-seams")]
impl ShareSeamView for ChainQuorums {
    /// Route B's `mined_quorum` with the published scheme of the same V turn; `Ok(None)` with no
    /// V running.
    fn mined_quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<MinedQuorum>, StoreError> {
        let hash = *quorum_hash;
        let got = self.on_v(move |o, q| {
            let scheme = q.bls().scheme();
            let mined = q.bind(o).mined_quorum(llmq_type, &hash).map_err(store_error)?;
            Ok(mined.map(|(commitment, members)| MinedQuorum {
                commitment,
                members,
                scheme,
            }))
        });
        got.unwrap_or(Ok(None))
    }

    /// Route B's `cached_quorum`; `None` with no V running.
    fn cached_quorum(&self, llmq_type: LlmqType, quorum_hash: &QuorumHash) -> Option<QuorumPtr> {
        let hash = *quorum_hash;
        self.on_v(move |o, q| q.bind(o).cached_quorum(llmq_type, &hash)).flatten()
    }
}
