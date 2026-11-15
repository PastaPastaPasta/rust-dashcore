use crate::dkg::bls::{BlsPkValue, BlsSkValue};
use crate::dkg::wire::QuorumHash;
use dash_consensus::primitives::BlockHash;
use dash_evo::types::ProTxHash;

use super::keys::{QuorumKeys, QuorumView};

#[derive(Clone, Debug)]
pub struct QuorumInfo {
    pub height: i32,
    pub type_name: String,
    pub quorum_hash: QuorumHash,
    pub quorum_index: i16,
    pub mined_block: BlockHash,
    pub previous_consecutive_dkg_failures: Option<i32>,
    pub members: Option<Vec<MemberInfo>>,
    pub quorum_public_key: BlsPkValue,
    pub secret_key_share: Option<BlsSkValue>,
}

#[derive(Clone, Debug)]
pub struct MemberInfo {
    pub pro_tx_hash: ProTxHash,
    pub pub_key_operator: BlsPkValue,
    pub valid: bool,
    pub pub_key_share: Option<BlsPkValue>,
}

/// Builds the quorum information projection.
///
/// `prev_same_index_base` is the base height of the previous commitment for this quorum index,
/// if one was mined at or before this quorum's base height. It comes from Core's
/// `GetLastMinedCommitmentsByQuorumIndexUntilBlock` (blockprocessor.cpp:731-777).
// Core: v24.0.0 src/rpc/quorums.cpp:192 BuildQuorumInfo.
pub fn quorum_info(
    q: &QuorumView<'_>,
    keys: &mut QuorumKeys,
    base_height: i32,
    mined_block: &BlockHash,
    prev_same_index_base: Option<i32>,
    include_members: bool,
    include_sk: bool,
) -> QuorumInfo {
    let previous = if q.llmq.use_rotation {
        Some(
            prev_same_index_base
                .map_or(0, |h| (base_height - h) / i32::from(q.llmq.dkg_interval) - 1),
        )
    } else {
        None
    };
    let members = include_members.then(|| {
        q.members
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let valid = q.valid(i);
                let pub_key_share = if !valid {
                    None
                } else if q.llmq.size == 1 {
                    q.operator_keys.get(i).cloned()
                } else {
                    let pk = keys.pub_key_share(q, i);
                    pk.is_valid().then_some(pk)
                };
                MemberInfo {
                    pro_tx_hash: *p,
                    pub_key_operator: q
                        .operator_keys
                        .get(i)
                        .cloned()
                        .unwrap_or_else(BlsPkValue::invalid),
                    valid,
                    pub_key_share,
                }
            })
            .collect()
    });
    QuorumInfo {
        height: base_height,
        type_name: q.commitment.llmq_type().to_string(),
        quorum_hash: q.commitment.quorum_hash(),
        quorum_index: q.commitment.quorum_index().unwrap_or(0),
        mined_block: *mined_block,
        previous_consecutive_dkg_failures: previous,
        members,
        quorum_public_key: q.commitment.quorum_public_key().value().clone(),
        secret_key_share: keys.sk_share().filter(|sk| include_sk && sk.is_valid()).cloned(),
    }
}
