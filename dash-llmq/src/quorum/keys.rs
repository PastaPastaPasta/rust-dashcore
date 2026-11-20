use std::sync::Arc;

use dash_evo::llmq::LlmqCommitment;
use dash_evo::types::ProTxHash;
use dash_params::LlmqParams;

use crate::dkg::bls::{
    BlsPkValue, BlsSchemeKind, BlsSkValue, aggregate_secret_keys, build_pub_key_share,
    build_quorum_verification_vector, member_id, secret_to_public,
};
use crate::dkg::wire::vvec_hash;

/// The immutable quorum inputs used while building and checking key material.
#[derive(Clone, Copy, Debug)]
pub struct QuorumView<'a> {
    pub llmq: &'a LlmqParams,
    pub commitment: &'a LlmqCommitment,
    pub members: &'a [ProTxHash],
    pub operator_keys: &'a [BlsPkValue],
}

impl QuorumView<'_> {
    // Core: v24.0.0 src/llmq/quorums.cpp:139 CQuorum::IsMember
    pub fn is_member(&self, p: &ProTxHash) -> bool {
        self.members.iter().any(|m| m == p)
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:144 CQuorum::IsValidMember
    pub fn is_valid_member(&self, p: &ProTxHash) -> bool {
        self.member_index(p)
            .is_some_and(|i| self.commitment.valid_members().get(i as u64) == Some(true))
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:181 CQuorum::GetMemberIndex
    pub fn member_index(&self, p: &ProTxHash) -> Option<usize> {
        self.members.iter().position(|m| m == p)
    }

    pub(super) fn valid(&self, i: usize) -> bool {
        self.commitment.valid_members().get(i as u64) == Some(true)
    }
}

/// The key cache held by a quorum. Secret-key values wipe their scalar representation on drop.
#[derive(Debug, Default)]
pub struct QuorumKeys {
    pub(super) vvec: Option<Arc<Vec<BlsPkValue>>>,
    pub(super) sk_share: Option<BlsSkValue>,
    pk_shares: Vec<Option<BlsPkValue>>,
}

impl QuorumKeys {
    // Core: v24.0.0 src/llmq/quorums.cpp:117 CQuorum::SetVerificationVector
    pub fn set_verification_vector(
        &mut self,
        q: &QuorumView<'_>,
        vvec: Vec<BlsPkValue>,
        scheme: BlsSchemeKind,
    ) -> bool {
        let hash = vvec_hash(&vvec, scheme);
        if hash != *q.commitment.quorum_vvec_hash().as_bytes() {
            return false;
        }
        self.vvec = Some(Arc::new(vvec));
        true
    }

    // Core: v24.0.0 src/llmq/quorums.h:198 CQuorum::SetVerificationVector
    pub fn set_vvec_unchecked(&mut self, vvec: Arc<Vec<BlsPkValue>>) {
        self.vvec = Some(vvec);
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:129 CQuorum::SetSecretKeyShare
    pub fn set_secret_key_share(
        &mut self,
        q: &QuorumView<'_>,
        sk: BlsSkValue,
        pro_tx: &ProTxHash,
    ) -> bool {
        let Some(i) = q.member_index(pro_tx) else {
            return false;
        };
        if !sk.is_valid() || (pro_tx == &ProTxHash::from([0; 32])) {
            return false;
        }
        if !secret_to_public(&sk).core_eq(&self.pub_key_share(q, i)) {
            return false;
        }
        self.sk_share = Some(sk);
        true
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:155 CQuorum::GetPubKeyShare
    pub fn pub_key_share(&mut self, q: &QuorumView<'_>, i: usize) -> BlsPkValue {
        let Some(vvec) = &self.vvec else {
            return BlsPkValue::invalid();
        };
        if i >= q.members.len() || !q.valid(i) {
            return BlsPkValue::invalid();
        }
        if self.pk_shares.len() < q.members.len() {
            self.pk_shares.resize_with(q.members.len(), || None);
        }
        if let Some(pk) = &self.pk_shares[i] {
            return pk.clone();
        }
        let pk = build_pub_key_share(vvec, &member_id(&q.members[i]));
        self.pk_shares[i] = Some(pk.clone());
        pk
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:165 CQuorum::HasVerificationVector
    pub fn has_vvec(&self) -> bool {
        self.vvec.is_some()
    }
    // Core: v24.0.0 src/llmq/quorums.cpp:175 CQuorum::GetSkShare
    pub fn sk_share(&self) -> Option<&BlsSkValue> {
        self.sk_share.as_ref()
    }
}

// Core: v24.0.0 src/llmq/quorumsman.cpp:115 CQuorumManager::BuildQuorumContributions
pub fn build_contributions(
    keys: &mut QuorumKeys,
    q: &QuorumView<'_>,
    vvecs: &[Arc<Vec<BlsPkValue>>],
    sk_contributions: &[BlsSkValue],
    me: Option<&ProTxHash>,
    scheme: BlsSchemeKind,
) -> bool {
    let refs: Vec<&[BlsPkValue]> = vvecs.iter().map(|v| v.as_slice()).collect();
    keys.vvec = build_quorum_verification_vector(&refs, scheme).map(Arc::new);
    if !keys.has_vvec() {
        return false;
    }
    if let Some(me) = me {
        let refs: Vec<&BlsSkValue> = sk_contributions.iter().collect();
        let sk = aggregate_secret_keys(&refs, scheme);
        let _ = keys.set_secret_key_share(q, sk, me);
    }
    true
}

// Core: v24.0.0 src/llmq/quorumsman.cpp:650 CacheWarmingThreadMain.
pub fn warm_members(keys: &mut QuorumKeys, q: &QuorumView<'_>, stop: &dyn Fn() -> bool) {
    for i in 0..q.members.len() {
        if stop() {
            break;
        }
        if q.valid(i) {
            let _ = keys.pub_key_share(q, i);
        }
    }
}
