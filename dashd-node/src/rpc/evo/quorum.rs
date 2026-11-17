use super::ChainSlot;
use dash_consensus::ChainAncestry;
use dash_evo::llmq::{MemberError, enabled_quorum_params};
use dash_params::{LlmqParams, LlmqParamsExt, LlmqType};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use dashd_chainstate::chain::{ChainQuery, OnV, OnVMembersError};
use dashd_chainstate::llmq::quorum::QuorumPtr;
use dashd_rpc::evo::{EvoError, QuorumFacts, QuorumListFacts};

// Core: rpc/quorums.cpp:80
pub(super) fn list(slot: &ChainSlot, count: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
    read_list(slot, None, count)
}

// Core: rpc/quorums.cpp:147
pub(super) fn list_extended(
    slot: &ChainSlot,
    height: Option<i32>,
) -> Result<Vec<QuorumListFacts>, EvoError> {
    read_list(slot, height, None)
}

fn read_list(
    slot: &ChainSlot,
    height: Option<i32>,
    count: Option<i32>,
) -> Result<Vec<QuorumListFacts>, EvoError> {
    let chain = slot.slot().as_ref().ok_or(EvoError::Unavailable)?.chain.clone();
    chain
        .on_v(move |on_v, q| {
            let (tip, _) = q.chain.private_tip().ok_or(EvoError::Unavailable)?;
            let start = match height {
                Some(h) if h < 0 || i64::from(h) > i64::from(tip.height) => {
                    return Err(EvoError::HeightOutOfRange);
                }
                Some(h) => q.chain.ancestor(tip.id, h as u32).ok_or(EvoError::HeightOutOfRange)?,
                None => tip.id,
            };
            let enabled = q
                .evo_view()
                .with_qc(q.bls(), |qc| {
                    enabled_quorum_params(&qc.llmq, start)
                        .map(|types| types.into_iter().map(|(t, p)| (t, *p)).collect::<Vec<_>>())
                })
                .map_err(read_error)?
                .map_err(|e| read_error(e.into()))?;
            enabled
                .into_iter()
                .map(|(llmq_type, params)| {
                    let count = count.filter(|n| *n >= 0).map_or_else(
                        || usize::from(params.signing_active_quorum_count),
                        |n| n as usize,
                    );
                    let quorums =
                        q.bind(on_v).scan_quorums(llmq_type, start, count).map_err(read_error)?;
                    let quorums = quorums.iter().map(|quorum| facts(q, quorum, &params)).collect();
                    Ok(QuorumListFacts {
                        type_name: llmq_type.to_string(),
                        quorums,
                    })
                })
                .collect()
        })
        .recv()
        .map_err(|_| EvoError::Failed(dashd_rpc::blockchain::ABORTED.into()))?
}

// Core: rpc/quorums.cpp:291
pub(super) fn info(
    slot: &ChainSlot,
    llmq_type: u8,
    hash: BlockHash,
) -> Result<QuorumFacts, EvoError> {
    let chain = slot.slot().as_ref().ok_or(EvoError::Unavailable)?.chain.clone();
    chain
        .on_v(move |on_v: &OnV<'_>, q: &ChainQuery<'_>| {
            let llmq_type = LlmqType::from_base(llmq_type);
            let params =
                q.chain.params().consensus.llmq(llmq_type).ok_or(EvoError::InvalidQuorumType)?;
            let hash = QuorumHash::from(*hash.as_bytes());
            let quorum = q
                .bind(on_v)
                .quorum(llmq_type, &hash)
                .map_err(read_error)?
                .ok_or(EvoError::QuorumNotFound)?;
            let mut facts = facts(q, &quorum, params);
            if params.use_rotation {
                let previous = q
                    .evo_view()
                    .with_qc(q.bls(), |qc| {
                        qc.mined.nth_mined_by_index(llmq_type, quorum.base, facts.index, 0)
                    })
                    .map_err(read_error)?
                    .map_err(EvoError::from)?;
                facts.prev_failures = Some(previous.map_or(0, |base| {
                    (facts.base_height - q.chain.header(base).height as i32)
                        / i32::from(params.dkg_interval)
                        - 1
                }));
            }
            Ok(facts)
        })
        .recv()
        .map_err(|_| EvoError::Failed(dashd_rpc::blockchain::ABORTED.into()))?
}

// Core: rpc/quorums.cpp:197
fn facts(q: &ChainQuery<'_>, quorum: &QuorumPtr, params: &LlmqParams) -> QuorumFacts {
    let base_height = q.chain.header(quorum.base).height as i32;
    let index = quorum.commitment.quorum_index().unwrap_or(0);
    let members: Vec<_> = quorum
        .members
        .iter()
        .enumerate()
        .map(|(i, member)| {
            (member.clone(), quorum.commitment.valid_members().get(i as u64).unwrap_or(false))
        })
        .collect();
    let valid_count = members.iter().filter(|(_, valid)| *valid).count();
    QuorumFacts {
        type_name: quorum.llmq_type.to_string(),
        rotated: params.use_rotation,
        base_height,
        hash: quorum.base_hash,
        index,
        mined_block: quorum.mined_block,
        prev_failures: None,
        members,
        valid_count,
        single_member: params.is_single_member(),
        bls: q.bls(),
        quorum_pk: quorum.commitment.quorum_public_key().to_bytes(q.bls().scheme()),
    }
}

fn read_error(error: OnVMembersError) -> EvoError {
    match error {
        OnVMembersError::Members(MemberError::View(error)) => error.into(),
        error => EvoError::Failed(error.to_string()),
    }
}
