//! The node's quorums as the `qdata` code reads them (PLAN-QM1b §3): [`ChainQuorumData`], Core's
//! `CQuorumManager` behind `QuorumDataSource` and the recovery scan, over route B
//! ([`ChainQuorums`]); and [`QuorumObject`], one `CQuorum` with its members' keys and its data
//! recovery claim.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use dash_consensus::BlockId;
use dash_dml::StoreError;
use dash_evo::llmq::OperatorKeyView as _;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{BlsPkValue, BlsSchemeKind, BlsSkValue};
use dash_llmq::dkg::wire::BlsIesBlobBytes;
use dash_llmq::qdata::source::{EncryptedContributions, QuorumData, QuorumDataSource};
use dash_llmq::quorum::{QuorumDbKey, QuorumView, StoredContributions, quorum_db_key, to_store};
use dash_params::{ChainSetup, LlmqParams, LlmqType};
use dash_primitives::{BlockHash, QuorumHash};
use dashd_chainstate::llmq::quorum::NodeQuorum;

use crate::chain::ChainQuorums;
use crate::net_quorum::{CaughtException, StoredQuorum};
use crate::tip::{RecoveryScan, ScanError};
use crate::traits::QuorumLookup;

/// `typeid(dbwrapper_error).name()`.
const DBWRAPPER_ERROR: &str = "15dbwrapper_error";

/// The quorum objects whose data recovery is claimed: Core's per-object
/// `fQuorumDataRecoveryThreadRunning`. An object a cache evicted and rebuilt is a new object
/// with no claim, as in Core.
// Core: v24.0.0 src/llmq/quorums.h:184, :220-222
#[derive(Default)]
pub struct RecoveryClaims(Mutex<Vec<Weak<NodeQuorum>>>);

impl RecoveryClaims {
    fn with<T>(&self, f: impl FnOnce(&mut Vec<Weak<NodeQuorum>>) -> T) -> T {
        let mut claims = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        claims.retain(|w| w.strong_count() > 0);
        f(&mut claims)
    }

    fn position(claims: &[Weak<NodeQuorum>], q: &Arc<NodeQuorum>) -> Option<usize> {
        claims.iter().position(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(q)))
    }
}

struct Object {
    quorum: Arc<NodeQuorum>,
    params: LlmqParams,
    members: Vec<ProTxHash>,
    operator_keys: Vec<BlsPkValue>,
    valid: Vec<bool>,
    claims: Arc<RecoveryClaims>,
}

/// One built quorum (`CQuorumCPtr`) as `qdata` reads and writes it. Clones share the object.
#[derive(Clone)]
pub struct QuorumObject(Arc<Object>);

impl QuorumObject {
    /// `quorum` with `params`, its type's parameters, under `claims`.
    pub fn new(quorum: Arc<NodeQuorum>, params: LlmqParams, claims: Arc<RecoveryClaims>) -> Self {
        let members: Vec<ProTxHash> = quorum.members.iter().map(|m| m.pro_tx_hash).collect();
        let operator_keys = quorum.members.iter().map(|m| m.operator_key()).collect();
        let bits = quorum.commitment.valid_members();
        let valid = (0..bits.num_bits()).map(|i| bits.get(i) == Some(true)).collect();
        Self(Arc::new(Object {
            quorum,
            params,
            members,
            operator_keys,
            valid,
            claims,
        }))
    }

    /// The quorum object.
    pub fn quorum(&self) -> &Arc<NodeQuorum> {
        &self.0.quorum
    }

    fn view(&self) -> QuorumView<'_> {
        QuorumView {
            llmq: &self.0.params,
            commitment: &self.0.quorum.commitment,
            fields: self.0.quorum.commitment.bls(),
            members: &self.0.members,
            operator_keys: &self.0.operator_keys,
        }
    }

    fn keys(&self) -> std::sync::MutexGuard<'_, dash_llmq::quorum::QuorumKeys> {
        self.0.quorum.keys.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl QuorumData for QuorumObject {
    fn threshold(&self) -> usize {
        usize::from(self.0.params.threshold)
    }

    fn valid_count(&self) -> usize {
        self.0.valid.iter().filter(|v| **v).count()
    }

    fn size(&self) -> usize {
        self.0.valid.len()
    }

    fn member_index(&self, p: &ProTxHash) -> Option<usize> {
        self.view().member_index(p)
    }

    fn is_valid_member(&self, p: &ProTxHash) -> bool {
        self.view().is_valid_member(p)
    }

    fn members(&self) -> &[ProTxHash] {
        &self.0.members
    }

    fn valid(&self) -> &[bool] {
        &self.0.valid
    }

    fn vvec(&self) -> Option<Arc<Vec<BlsPkValue>>> {
        self.keys().vvec().cloned()
    }

    fn has_sk_share(&self) -> bool {
        self.keys().sk_share().is_some_and(BlsSkValue::is_valid)
    }

    fn set_vvec(&self, v: Vec<BlsPkValue>, scheme: BlsSchemeKind) -> bool {
        let view = self.view();
        self.keys().set_verification_vector(&view, v, scheme)
    }

    fn set_sk_share(&self, sk: BlsSkValue, me: &ProTxHash) -> bool {
        let view = self.view();
        self.keys().set_secret_key_share(&view, sk, me)
    }

    // Core: v24.0.0 src/llmq/quorums.h:220 CQuorum::TryClaimRecovery
    fn try_claim_recovery(&self) -> bool {
        let q = &self.0.quorum;
        self.0.claims.with(|claims| {
            if RecoveryClaims::position(claims, q).is_some() {
                return false;
            }
            claims.push(Arc::downgrade(q));
            true
        })
    }

    // Core: v24.0.0 src/llmq/quorums.h:221 CQuorum::ReleaseRecovery
    fn release_recovery(&self) {
        let q = &self.0.quorum;
        self.0.claims.with(|claims| {
            if let Some(i) = RecoveryClaims::position(claims, q) {
                claims.swap_remove(i);
            }
        });
    }

    // Core: v24.0.0 src/llmq/quorums.h:222 CQuorum::IsRecoveryRunning
    fn recovery_running(&self) -> bool {
        let q = &self.0.quorum;
        self.0.claims.with(|claims| RecoveryClaims::position(claims, q).is_some())
    }
}

impl StoredQuorum for QuorumObject {
    // Core: v24.0.0 src/llmq/quorums.cpp:20-29 MakeQuorumKey
    fn db_key(&self) -> QuorumDbKey {
        let q = &self.0.quorum;
        quorum_db_key(q.llmq_type, &q.commitment.quorum_hash(), &self.0.members)
    }

    fn contributions(&self) -> StoredContributions {
        to_store(&self.keys())
    }

    // Core: v24.0.0 src/llmq/quorumsman.cpp:632-676 CacheWarmingThreadMain / QueueQuorumForWarming
    fn warm_job(&self) -> Box<dyn FnOnce() + Send> {
        let this = self.clone();
        Box::new(move || {
            let view = this.view();
            dash_llmq::quorum::warm_members(&mut this.keys(), &view, &|| false);
        })
    }
}

/// A store error a quorum build throws out of the handler, as `ProcessMessages` catches it.
#[derive(Debug)]
pub struct QuorumReadError(pub StoreError);

impl fmt::Display for QuorumReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl CaughtException for QuorumReadError {
    fn type_name(&self) -> &'static str {
        DBWRAPPER_ERROR
    }
}

/// The dkgdb reader before DK1: no row, so a member answers ENCRYPTED_CONTRIBUTIONS_MISSING.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEncryptedContributions;

impl EncryptedContributions for NoEncryptedContributions {
    fn get(
        &self,
        _: LlmqType,
        _: &BlockHash,
        _: &[bool],
        _: &ProTxHash,
    ) -> Option<Vec<BlsIesBlobBytes>> {
        None
    }
}

/// `m_qman` as `NetQuorum` reads it, over route B.
#[derive(Clone)]
pub struct ChainQuorumData {
    params: Arc<ChainSetup>,
    chain: Arc<ChainQuorums>,
    claims: Arc<RecoveryClaims>,
}

impl ChainQuorumData {
    pub fn new(params: Arc<ChainSetup>, chain: Arc<ChainQuorums>) -> Self {
        Self {
            params,
            chain,
            claims: Arc::default(),
        }
    }

    fn object(&self, quorum: Arc<NodeQuorum>) -> Option<QuorumObject> {
        let params = *self.params.consensus.llmq(quorum.llmq_type)?;
        Some(QuorumObject::new(quorum, params, Arc::clone(&self.claims)))
    }
}

impl QuorumDataSource for ChainQuorumData {
    type Q = QuorumObject;
    type Error = QuorumReadError;

    // Core: v24.0.0 src/llmq/net_quorum.cpp:123 Params().GetLLMQ
    fn llmq_registered(&self, t: LlmqType) -> bool {
        self.params.consensus.llmq(t).is_some()
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:131-137
    fn active_block(&self, h: &BlockHash) -> bool {
        self.chain.on_active_chain(h)
    }

    // Core: v24.0.0 src/llmq/quorumsman.cpp:491-512 GetQuorum
    fn quorum(&self, t: LlmqType, h: &BlockHash) -> Result<Option<QuorumObject>, QuorumReadError> {
        let hash = QuorumHash::from(*h.as_bytes());
        let quorum = QuorumLookup::quorum(&*self.chain, t, &hash).map_err(QuorumReadError)?;
        Ok(quorum.and_then(|q| self.object(q)))
    }

    // Core: v24.0.0 src/llmq/quorumsman.cpp:618-624 GetCachedMutableQuorum
    fn cached_quorum(&self, t: LlmqType, h: &BlockHash) -> Option<QuorumObject> {
        let hash = QuorumHash::from(*h.as_bytes());
        self.chain.cached(t, &hash).and_then(|q| self.object(q))
    }
}

impl RecoveryScan for ChainQuorumData {
    type Q = QuorumObject;

    // Core: v24.0.0 src/llmq/net_quorum.cpp:510 ScanQuorums
    fn scan(
        &self,
        llmq_type: LlmqType,
        tip: BlockId,
        count: usize,
    ) -> Result<Vec<(BlockHash, QuorumObject)>, ScanError> {
        let scanned = self.chain.scan_objects(llmq_type, tip, count)?;
        let objects = scanned.into_iter().filter_map(|q| {
            let hash = BlockHash::from(*q.commitment.quorum_hash().as_bytes());
            self.object(q).map(|o| (hash, o))
        });
        Ok(objects.collect())
    }
}
