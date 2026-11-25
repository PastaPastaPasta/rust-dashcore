//! Core's runtime quorum manager (v24.0.0 `llmq/quorumsman.cpp` and `net_quorum.cpp`).

use std::cell::Cell;
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

#[cfg(feature = "testkit-seams")]
pub mod share_seam;

/// A quorum's parameters, member proTxHashes and operator keys, as [`QuorumView`] borrows them.
type MemberView = (dash_params::LlmqParams, Vec<dash_evo::types::ProTxHash>, Vec<BlsPkValue>);

/// Core's `CQuorumManager` contribution and cleanup runtime. It holds no BLS scheme: each build
/// takes its view's, as Core's reads the process flag at the call.
pub struct QuorumManager<S, C, M, W> {
    params: Arc<ChainSetup>,
    store: Arc<S>,
    contributions: Arc<C>,
    local_masternode: Arc<M>,
    executor: Arc<W>,
    cleanup_facade: Option<Arc<dyn CleanupFacade>>,
    lookup: Option<Arc<dyn QuorumLookup>>,
    #[cfg(feature = "testkit-seams")]
    share_seam_view: Option<Arc<dyn crate::traits::ShareSeamView>>,
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
        store: S,
        contributions: C,
        local_masternode: M,
        executor: W,
    ) -> Self {
        Self {
            params,
            store: Arc::new(store),
            contributions: Arc::new(contributions),
            local_masternode: Arc::new(local_masternode),
            executor: Arc::new(executor),
            cleanup_facade: None,
            lookup: None,
            #[cfg(feature = "testkit-seams")]
            share_seam_view: None,
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
            // A failed scan is Core's throwing `ScanQuorums`: the job ends before it erases.
            // Shutdown interruption (`quorumThreadInterrupt`) arrives with the node wiring.
            let failed = Cell::new(false);
            let mut scan = |t, count| {
                facade.scan(t, tip, count).unwrap_or_else(|| {
                    failed.set(true);
                    Vec::new()
                })
            };
            let keep = keys_to_keep(
                &mut cache,
                &params,
                tip,
                &|id| facade.ancestors(id),
                &mut scan,
                &|| failed.get(),
            );
            // A failed scan is Core's throwing `ScanQuorums` inside the ctpl packaged_task
            // (net_quorum.cpp:723-763 at v24.0.0): the task leaves before the done line. A later
            // shutdown interrupt is not a failed scan, so that path still reaches the line.
            if failed.get() {
                return;
            }
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

    fn quorum_view(&self, quorum: &QuorumPtr) -> Result<MemberView, StoreError> {
        self.member_view(quorum.llmq_type, &quorum.members)
            .ok_or_else(|| StoreError::read(format!("unknown LLMQ type {:?}", quorum.llmq_type)))
    }

    /// `llmq_type`'s configured parameters with `members`' proTxHashes and operator keys; `None`
    /// for a type this chain does not configure.
    fn member_view(
        &self,
        llmq_type: LlmqType,
        members: &[Arc<dash_dml::Dmn>],
    ) -> Option<MemberView> {
        let params = self.params.consensus.llmq(llmq_type).cloned()?;
        let pro_txs = members.iter().map(|m| m.pro_tx_hash).collect();
        let operator_keys = members.iter().map(|m| m.operator_key()).collect();
        Some((params, pro_txs, operator_keys))
    }

    /// Core's `QueueQuorumForWarming` (v24.0.0 src/llmq/quorumsman.cpp:670-677): every valid
    /// member's public key share, computed on the executor.
    fn queue_warm(&self, quorum: Arc<NodeQuorum>, view: MemberView) {
        let (params, members, operator_keys) = view;
        self.executor.as_ref().spawn_warm(Box::new(move || {
            let view = QuorumView {
                llmq: &params,
                commitment: &quorum.commitment,
                members: &members,
                operator_keys: &operator_keys,
            };
            let mut keys = quorum.keys.lock().unwrap_or_else(|p| p.into_inner());
            warm_members(&mut keys, &view, &|| false);
        }));
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
    fn built(
        &self,
        quorum: &QuorumPtr,
        populate_cache: bool,
        scheme: BlsSchemeKind,
    ) -> Result<(), StoreError> {
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
                let (_, vvecs, sk_contributions) = self.contributions.verified(
                    quorum.llmq_type,
                    &quorum.base_hash,
                    &members,
                    &valid,
                )?;
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
                    scheme,
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
            self.queue_warm(quorum.0.clone(), (params, members, operator_keys));
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

/// The share seam's decoding boundary: byte bags into the key values `QuorumKeys` takes, under
/// the scheme of the view the seam read the quorum through.
#[cfg(feature = "testkit-seams")]
impl<S, C, M, W> QuorumManager<S, C, M, W> {
    /// Core's `CBLSPublicKey::Unserialize(s, legacy)` as a `qdata` verification vector reads each
    /// key: canonical bytes under the scheme, else the invalid key for all-zero bytes or bytes
    /// canonical under the opposite scheme, else `None`, Core's "malleable BLS object" throw.
    // Core: v24.0.0 src/bls/bls.h:100-122 SetBytes, :184-202 Unserialize
    fn decode_vvec_key(
        bytes: &dash_pkc::bls::BlsPkBytes,
        scheme: BlsSchemeKind,
    ) -> Option<BlsPkValue> {
        let raw = bytes.as_bytes();
        if let Some(key) = BlsPkValue::from_canonical(raw, scheme) {
            return Some(key);
        }
        let opposite = BlsPkValue::from_canonical(raw, scheme.opposite()).is_some();
        (raw == &[0; 48] || opposite).then(BlsPkValue::invalid)
    }

    /// A secret key share's 32 bytes as `quorum info` prints `secretKeyShare`: all-zero bytes are
    /// the invalid key, which `SetSecretKeyShare` refuses; bytes that are not a canonical scalar
    /// are `None`.
    fn decode_sk_share(
        bytes: &dash_pkc::bls::BlsSkBytes,
        scheme: BlsSchemeKind,
    ) -> Option<dash_llmq::dkg::bls::BlsSkValue> {
        let raw = bytes.to_bytes();
        if *raw == [0; 32] {
            return Some(dash_llmq::dkg::bls::BlsSkValue::invalid());
        }
        dash_llmq::dkg::bls::BlsSkValue::from_canonical(&raw, scheme)
    }
}
