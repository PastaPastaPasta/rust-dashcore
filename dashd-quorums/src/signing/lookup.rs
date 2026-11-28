//! The signing quorum lookup over route B: [`SigningQuorums`] answers `CQuorumManager`'s
//! `GetQuorum`, `IsQuorumActive` and `SelectQuorumForSigning` for the signing runtime, each as a
//! [`QuorumHandle`].

use std::sync::{Arc, MutexGuard};

use dash_dml::StoreError;
use dash_evo::llmq::OperatorKeyView as _;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{BlsPkValue, BlsSkValue};
use dash_llmq::quorum::{QuorumKeys, QuorumView};
use dash_llmq::sigenv::{QuorumLookup, SigningQuorum};
use dash_params::{ChainSetup, LlmqParams, LlmqType};
use dash_primitives::QuorumHash;
use dashd_chainstate::chain::reply::no_wait_marked;
use dashd_chainstate::chain::{ChainQuery, OnV};
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumPtr};

use super::runtime::SigningLookup;
use crate::chain::{ChainQuorums, store_error};

const NO_WAIT_LOOKUP: &str = "signing quorum lookup from a no-wait task: it waits on V";

/// One quorum object as the signing code reads it: chainstate's [`NodeQuorum`] with its members'
/// proTxHashes and operator keys in commitment order, its type's parameters and its base height.
#[derive(Debug)]
pub struct QuorumHandle {
    quorum: Arc<NodeQuorum>,
    quorum_hash: QuorumHash,
    params: LlmqParams,
    members: Vec<ProTxHash>,
    operator_keys: Vec<BlsPkValue>,
    base_height: i32,
}

impl QuorumHandle {
    fn new(quorum: QuorumPtr, params: LlmqParams, base_height: i32) -> Self {
        let members = quorum.members.iter().map(|m| m.pro_tx_hash).collect();
        let operator_keys = quorum.members.iter().map(|m| m.operator_key()).collect();
        Self {
            quorum_hash: quorum.commitment.quorum_hash(),
            quorum: quorum.0,
            params,
            members,
            operator_keys,
            base_height,
        }
    }

    /// The chainstate quorum object, shared with route B's caches.
    pub fn quorum(&self) -> &Arc<NodeQuorum> {
        &self.quorum
    }

    /// `m_quorum_base_block_index->nHeight`.
    pub fn base_height(&self) -> i32 {
        self.base_height
    }

    fn keys(&self) -> MutexGuard<'_, QuorumKeys> {
        self.quorum.keys.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn view(&self) -> QuorumView<'_> {
        QuorumView {
            llmq: &self.params,
            commitment: &self.quorum.commitment,
            fields: self.quorum.commitment.bls(),
            members: &self.members,
            operator_keys: &self.operator_keys,
        }
    }
}

// Core: v24.0.2 src/llmq/quorums.cpp:139-185
impl SigningQuorum for QuorumHandle {
    fn llmq_type(&self) -> LlmqType {
        self.quorum.llmq_type
    }

    fn quorum_hash(&self) -> &QuorumHash {
        &self.quorum_hash
    }

    fn params(&self) -> &LlmqParams {
        &self.params
    }

    fn members(&self) -> &[ProTxHash] {
        &self.members
    }

    fn is_member(&self, p: &ProTxHash) -> bool {
        self.view().is_member(p)
    }

    fn is_valid_member(&self, p: &ProTxHash) -> bool {
        self.view().is_valid_member(p)
    }

    fn member_index(&self, p: &ProTxHash) -> Option<u16> {
        self.view().member_index(p).and_then(|i| u16::try_from(i).ok())
    }

    fn has_verification_vector(&self) -> bool {
        self.keys().has_vvec()
    }

    fn quorum_public_key(&self) -> &BlsPkValue {
        self.quorum.commitment.bls().quorum_public_key.value()
    }

    fn pub_key_share(&self, member: u16) -> Option<BlsPkValue> {
        let share = self.keys().pub_key_share(&self.view(), usize::from(member));
        share.is_valid().then_some(share)
    }

    fn sk_share(&self) -> Option<BlsSkValue> {
        self.keys().sk_share().filter(|sk| sk.is_valid()).cloned()
    }
}

/// [`QuorumLookup`] over route B ([`ChainQuorums`]). Every call waits on V, so none may run on a
/// no-wait-marked thread or on V's own. A route-B fault, or V not running, is `Err`, never "no
/// quorum".
pub struct SigningQuorums {
    chain: Arc<ChainQuorums>,
    setup: Arc<ChainSetup>,
}

impl SigningQuorums {
    pub fn new(chain: Arc<ChainQuorums>, setup: Arc<ChainSetup>) -> Self {
        Self {
            chain,
            setup,
        }
    }

    fn params(&self, t: LlmqType) -> Result<LlmqParams, StoreError> {
        let params = self.setup.consensus.llmq(t).copied();
        params.ok_or_else(|| StoreError::inconsistent(format!("a signing lookup of LLMQ type {t}")))
    }

    fn on_v<T: Send + 'static>(
        &self,
        f: impl for<'v> FnOnce(&OnV<'v>, &ChainQuery<'_>) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        debug_assert!(!no_wait_marked(), "{NO_WAIT_LOOKUP}");
        self.chain.on_v(f).unwrap_or(Err(StoreError::NotBuilt(
            "the signing quorum lookup of a node whose V is not running",
        )))
    }
}

/// `quorum` as a handle, with its base height from V's index.
fn handle(
    q: &ChainQuery<'_>,
    quorum: QuorumPtr,
    params: LlmqParams,
) -> Result<QuorumHandle, StoreError> {
    let entry = q.chain.reader().entry(quorum.base);
    let height = entry.and_then(|e| i32::try_from(e.height).ok()).ok_or_else(|| {
        StoreError::inconsistent(format!("quorum base {} outside V's index", quorum.base_hash))
    })?;
    Ok(QuorumHandle::new(quorum, params, height))
}

impl QuorumLookup for SigningQuorums {
    type Quorum = QuorumHandle;
    type Error = StoreError;

    // Core: v24.0.2 src/llmq/quorumsman.h:142 GetQuorum(type, quorumHash)
    fn get_quorum(
        &self,
        t: LlmqType,
        h: &QuorumHash,
    ) -> Result<Option<Arc<QuorumHandle>>, StoreError> {
        let (params, hash) = (self.params(t)?, *h);
        self.on_v(move |o, q| {
            let Some(quorum) = q.bind(o).quorum(t, &hash).map_err(store_error)? else {
                return Ok(None);
            };
            handle(q, quorum, params).map(|h| Some(Arc::new(h)))
        })
    }

    // Core: v24.0.2 src/llmq/signing.cpp:701-712 IsQuorumActive
    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, StoreError> {
        let Some(params) = self.setup.consensus.llmq(t) else {
            return Ok(false);
        };
        let (count, hash) = (usize::from(params.keep_old_connections), *h);
        self.on_v(move |o, q| {
            let snapshot = q.chain.snapshot();
            if !snapshot.has_tip() {
                return Ok(false);
            }
            let scanned = q.bind(o).scan_quorums(t, snapshot.active.tip(), count);
            Ok(scanned.map_err(store_error)?.iter().any(|q| q.commitment.quorum_hash() == hash))
        })
    }

    // Core: v24.0.2 src/llmq/quorumsman.cpp:741-812, quorumsman.h:214-223 SelectQuorumForSigning
    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        selection_hash: &[u8; 32],
        sign_height: i32,
        sign_offset: i32,
    ) -> Result<Option<Arc<QuorumHandle>>, StoreError> {
        let (params, selection_hash) = (self.params(t)?, *selection_hash);
        self.on_v(move |o, q| {
            let chosen =
                q.bind(o).select_quorum_for_signing(t, &selection_hash, sign_height, sign_offset);
            let Some(quorum) = chosen.map_err(store_error)? else {
                return Ok(None);
            };
            handle(q, quorum, params).map(|h| Some(Arc::new(h)))
        })
    }
}

impl SigningLookup for SigningQuorums {
    fn base_height(&self, quorum: &QuorumHandle) -> i32 {
        quorum.base_height
    }
}
