//! Core's runtime quorum manager (v24.0.0 `llmq/quorumsman.cpp` and `net_quorum.cpp`).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use dash_consensus::BlockId;
use dash_dml::StoreError;
use dash_evo::llmq::OperatorKeyView;
use dash_llmq::dkg::bls::{BlsPkValue, BlsSchemeKind};
use dash_llmq::quorum::{
    CleanupCache, QuorumKeys, QuorumView, build_contributions, cleanup_due, from_store,
    keys_to_keep, load_or_build_contributions, quorum_db_key, to_store, warm_members,
};
use dash_params::{ChainSetup, LlmqType};
use dash_primitives::QuorumHash;
use dash_types::Numeric;
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumBuildHook, QuorumPtr};
use tracing::debug;

use crate::traits::{
    CleanupFacade, LocalMasternode, PlatformQuorums, QuorumKeyStore, QuorumLookup,
    VerifiedContributions, WarmExecutor,
};

/// Core's `CQuorumManager` contribution and cleanup runtime.
pub struct QuorumManager<S, C, M, W> {
    params: Arc<ChainSetup>,
    scheme: BlsSchemeKind,
    store: Arc<S>,
    contributions: Arc<C>,
    local_masternode: Arc<M>,
    executor: Arc<W>,
    cleanup_facade: Option<Arc<dyn CleanupFacade>>,
    lookup: Option<Arc<dyn QuorumLookup>>,
    cleanup_cache: Arc<Mutex<CleanupCache>>,
}

impl<S, C, M, W> QuorumManager<S, C, M, W>
where
    S: QuorumKeyStore + Send + Sync + 'static,
    S::Error: Send + Sync + 'static,
    W: WarmExecutor + Send + Sync + 'static,
{
    pub fn new(
        params: Arc<ChainSetup>,
        scheme: BlsSchemeKind,
        store: S,
        contributions: C,
        local_masternode: M,
        executor: W,
    ) -> Self {
        Self {
            params,
            scheme,
            store: Arc::new(store),
            contributions: Arc::new(contributions),
            local_masternode: Arc::new(local_masternode),
            executor: Arc::new(executor),
            cleanup_facade: None,
            lookup: None,
            cleanup_cache: Arc::new(Mutex::new(CleanupCache::default())),
        }
    }

    pub fn with_cleanup_facade(mut self, facade: Arc<dyn CleanupFacade>) -> Self {
        self.cleanup_facade = Some(facade);
        self
    }

    pub fn with_quorum_lookup(mut self, lookup: Arc<dyn QuorumLookup>) -> Self {
        self.lookup = Some(lookup);
        self
    }

    /// Starts Core's old contribution cleanup at `tip`, the block at `height`. The height gate is
    /// inside this method; the job is always handed to the executor and never runs inline on the
    /// caller, and its scan is taken at `tip`, not at whatever the live tip is when the job runs.
    /// `height` must be `tip`'s height; Core reads both from one `pIndex`, so callers take both
    /// from the same index entry.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:711-764 StartCleanupOldQuorumDataThread
    pub fn start_cleanup_old_quorum_data(&self, tip: BlockId, height: u32) {
        if !i32::try_from(height).is_ok_and(cleanup_due) {
            return;
        }
        let Some(facade) = self.cleanup_facade.clone() else {
            return;
        };
        let started = Instant::now();
        debug!(target: "llmq", "NetQuorum::StartCleanupOldQuorumDataThread -- start");
        let params: Vec<_> = self.params.consensus.llmq_params().map(|(t, p)| (t, *p)).collect();
        let store = self.store.clone();
        let cache = self.cleanup_cache.clone();
        self.executor.as_ref().spawn_warm(Box::new(move || {
            let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
            let mut scan = |t, count| facade.scan(t, tip, count);
            // Shutdown interruption (`quorumThreadInterrupt`) arrives with the node wiring.
            let keep = keys_to_keep(
                &mut cache,
                &params,
                tip,
                &|id| facade.ancestors(id),
                &mut scan,
                &|| false,
            );
            // Core's job is a ctpl packaged_task (ctpl_stl.h:190-196): a storage error from
            // CleanupOldQuorumData lands in a discarded future, so the job ends silently, before
            // the done line.
            if let Some(keep) = keep
                && store.erase_except(&keep).is_err()
            {
                return;
            }
            debug!(target: "llmq", "NetQuorum::StartCleanupOldQuorumDataThread -- done. time={}", started.elapsed().as_millis());
        }));
    }

    fn quorum_view<'a>(
        &'a self,
        quorum: &'a QuorumPtr,
    ) -> Result<
        (dash_params::LlmqParams, Vec<dash_evo::types::ProTxHash>, Vec<BlsPkValue>),
        StoreError,
    > {
        let params =
            self.params.consensus.llmq(quorum.llmq_type).cloned().ok_or_else(|| {
                StoreError::read(format!("unknown LLMQ type {:?}", quorum.llmq_type))
            })?;
        let members = quorum.members.iter().map(|m| m.pro_tx_hash).collect::<Vec<_>>();
        let operator_keys = quorum.members.iter().map(|m| m.operator_key()).collect();
        Ok((params, members, operator_keys))
    }
}

impl<S, C, M, W> QuorumBuildHook for QuorumManager<S, C, M, W>
where
    S: QuorumKeyStore + Send + Sync + 'static,
    S::Error: Send + Sync + 'static,
    C: VerifiedContributions + Send + Sync + 'static,
    M: LocalMasternode + Send + Sync + 'static,
    W: WarmExecutor + Send + Sync + 'static,
{
    /// Core: v24.0.0 src/llmq/quorumsman.cpp:67-113 BuildQuorumFromCommitment.
    fn built(&self, quorum: &QuorumPtr, populate_cache: bool) -> Result<(), StoreError> {
        let (params, members, operator_keys) = self.quorum_view(quorum)?;
        let key = quorum_db_key(quorum.llmq_type, &quorum.commitment.quorum_hash(), &members);
        let mut keys = quorum.keys.lock().unwrap_or_else(|p| p.into_inner());
        let loaded = load_or_build_contributions(
            || {
                let stored = self
                    .store
                    .read(&key)
                    .map_err(|e| StoreError::read(format!("quorum contributions: {e}")))?;
                stored
                    .map(|stored| {
                        from_store(stored)
                            .map_err(|e| StoreError::read(format!("quorum vvec: {e:?}")))
                    })
                    .transpose()
                    .map(|loaded| loaded.flatten())
            },
            || {
                let valid: Vec<bool> = (0..members.len())
                    .map(|i| quorum.commitment.valid_members().get(i as u64) == Some(true))
                    .collect();
                let (_, vvecs, sk_contributions) =
                    self.contributions.verified(quorum.llmq_type, &quorum.base_hash, &valid)?;
                let view = QuorumView {
                    llmq: &params,
                    commitment: &quorum.commitment,
                    members: &members,
                    operator_keys: &operator_keys,
                };
                let mut built = QuorumKeys::default();
                let started = Instant::now();
                let has_valid_vvec = build_contributions(
                    &mut built,
                    &view,
                    &vvecs,
                    &sk_contributions,
                    self.local_masternode.pro_tx_hash().as_ref(),
                    self.scheme,
                );
                if !has_valid_vvec {
                    debug!(target: "llmq", "CQuorumManager::BuildQuorumContributions -- failed to build quorumVvec");
                    return None;
                }
                if built.sk_share().is_none() {
                    debug!(target: "llmq", "CQuorumManager::BuildQuorumContributions -- failed to build skShare");
                }
                debug!(target: "llmq", "CQuorumManager::BuildQuorumContributions -- built quorum vvec and skShare. time={}", started.elapsed().as_millis());
                Some(built)
            },
            |built| {
                self.store
                    .write(&key, &to_store(built))
                    .map_err(|e| StoreError::read(format!("quorum contributions write: {e}")))
            },
        )?;
        let has_valid_vvec = loaded.is_some();
        if !has_valid_vvec {
            debug!(target: "llmq", "CQuorumManager::BuildQuorumFromCommitment -- llmqType[{}] quorumIndex[{}] quorum.ReadContributions and BuildQuorumContributions for quorumHash[{}] failed", quorum.llmq_type.to_base(), quorum.commitment.quorum_index().unwrap_or(0), quorum.commitment.quorum_hash());
        }
        if let Some(loaded) = loaded {
            *keys = loaded;
        }
        drop(keys);
        if has_valid_vvec && populate_cache {
            let q = quorum.0.clone();
            let members = members.clone();
            let operator_keys = operator_keys.clone();
            self.executor.as_ref().spawn_warm(Box::new(move || {
                let view = QuorumView {
                    llmq: &params,
                    commitment: &q.commitment,
                    members: &members,
                    operator_keys: &operator_keys,
                };
                let mut keys = q.keys.lock().unwrap_or_else(|p| p.into_inner());
                warm_members(&mut keys, &view, &|| false);
            }));
        }
        Ok(())
    }
}

impl<S, C, M, W> PlatformQuorums for QuorumManager<S, C, M, W> {
    fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<Arc<NodeQuorum>>, StoreError> {
        match self.lookup.as_ref() {
            Some(lookup) => lookup.quorum(llmq_type, quorum_hash),
            None => Ok(None),
        }
    }
}
