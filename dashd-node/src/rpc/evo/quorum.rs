use super::{ChainSlot, DkgStatusSource};
use dash_consensus::{BlockId, ChainAncestry};
use dash_evo::llmq::{
    MemberError, OperatorKeyView, enabled_quorum_params, is_quorum_rotation_enabled,
};
use dash_evo::types::ProTxHash;
use dash_llmq::quorum::QuorumView;
use dash_params::{LlmqParams, LlmqParamsExt, LlmqType};
use dash_primitives::{BlockHash, QuorumHash, commitment_bls_scheme};
use dash_types::Numeric;
use dashd_chainstate::chain::{ChainQuery, OnV, OnVMembersError};
use dashd_chainstate::llmq::quorum::QuorumPtr;
use dashd_masternode::mnconn::{LlmqTopology, QuorumTopology};
use dashd_rpc::UniValue;
use dashd_rpc::evo::{EvoError, QuorumFacts, QuorumListFacts};
use dashd_rpc::protocol::{RPC_INVALID_PARAMETER, RpcError};
use std::sync::PoisonError;

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
            scan(on_v, q, start, count, false)
        })
        .recv()
        .map_err(|_| EvoError::Failed(dashd_rpc::blockchain::ABORTED.into()))?
}

// Core: v24.0.2 src/rpc/quorums.cpp:487-511
pub(super) fn member_of(
    slot: &ChainSlot,
    pro_tx_hash: ProTxHash,
    count: Option<i32>,
) -> Result<Option<Vec<QuorumListFacts>>, EvoError> {
    let chain = slot.slot().as_ref().ok_or(EvoError::Unavailable)?.chain.clone();
    chain
        .on_v(move |on_v, q| {
            let (tip, _) = q.chain.private_tip().ok_or(EvoError::Unavailable)?;
            if q.evo_view().list_for_block(tip.id)?.get(&pro_tx_hash).is_none() {
                return Ok(None);
            }
            scan(on_v, q, tip.id, count, true).map(Some)
        })
        .recv()
        .map_err(|_| EvoError::Failed(dashd_rpc::blockchain::ABORTED.into()))?
}

/// `ScanQuorums(type, start, count)` for each type `GetEnabledQuorumTypes` gives at `start`, with
/// `previousConsecutiveDKGFailures` when `failures` is set.
// Core: v24.0.2 src/rpc/quorums.cpp:91-98
fn scan(
    on_v: &OnV<'_>,
    q: &ChainQuery<'_>,
    start: BlockId,
    count: Option<i32>,
    failures: bool,
) -> Result<Vec<QuorumListFacts>, EvoError> {
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
            let count = count
                .filter(|n| *n >= 0)
                .map_or_else(|| usize::from(params.signing_active_quorum_count), |n| n as usize);
            let quorums = q.bind(on_v).scan_quorums(llmq_type, start, count).map_err(read_error)?;
            let quorums = quorums
                .iter()
                .map(|quorum| {
                    let mut facts = facts(q, quorum, &params);
                    if failures {
                        facts.prev_failures = prev_failures(q, quorum, &params, &facts)?;
                    }
                    Ok(facts)
                })
                .collect::<Result<_, EvoError>>()?;
            Ok(QuorumListFacts {
                type_name: llmq_type.to_string(),
                quorums,
            })
        })
        .collect()
}

// Core: rpc/quorums.cpp:291
pub(super) fn info(
    slot: &ChainSlot,
    llmq_type: u8,
    hash: BlockHash,
    include_sk_share: bool,
) -> Result<QuorumFacts, EvoError> {
    let chain = slot.slot().as_ref().ok_or(EvoError::Unavailable)?.chain.clone();
    let (mut facts, quorum, params) = chain
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
            facts.prev_failures = prev_failures(q, &quorum, params, &facts)?;
            Ok::<_, EvoError>((facts, quorum, *params))
        })
        .recv()
        .map_err(|_| EvoError::Failed(dashd_rpc::blockchain::ABORTED.into()))??;
    add_shares(&mut facts, &quorum, &params, include_sk_share);
    Ok(facts)
}

/// The key shares Core's `BuildQuorumInfo` reads from the cached quorum, off V. The keys lock is
/// taken per read, as Core takes `cs_vvec_shShare` per `GetPubKeyShare`, so a cold share build
/// for a large quorum does not hold the signers off for the whole answer. Its presence rules are
/// `dash_llmq::quorum::quorum_info`'s (dash-llmq/src/quorum/info.rs), which takes the lock once;
/// a change to one is a change to both.
// Core: v24.0.0 src/rpc/quorums.cpp:225-244, src/llmq/quorums.cpp:155-179
pub(crate) fn add_shares(
    facts: &mut QuorumFacts,
    quorum: &QuorumPtr,
    params: &LlmqParams,
    include_sk: bool,
) {
    let scheme = facts.bls.scheme();
    let keys = || quorum.keys.lock().unwrap_or_else(PoisonError::into_inner);
    if !params.is_single_member() {
        let members: Vec<ProTxHash> = quorum.members.iter().map(|m| m.pro_tx_hash).collect();
        let operator_keys: Vec<_> = quorum.members.iter().map(|m| m.operator_key()).collect();
        let view = QuorumView {
            llmq: params,
            commitment: &quorum.commitment,
            members: &members,
            operator_keys: &operator_keys,
        };
        facts.pub_key_shares = (0..members.len())
            .map(|i| {
                let share = keys().pub_key_share(&view, i);
                share.is_valid().then(|| share.to_bytes(scheme))
            })
            .collect();
    }
    if include_sk {
        facts.sk_share = keys().sk_share().filter(|sk| sk.is_valid()).map(|sk| sk.to_bytes(scheme));
    }
}

// Core: v24.0.2 src/rpc/quorums.cpp:203-211
fn prev_failures(
    q: &ChainQuery<'_>,
    quorum: &QuorumPtr,
    params: &LlmqParams,
    facts: &QuorumFacts,
) -> Result<Option<i32>, EvoError> {
    if !params.use_rotation {
        return Ok(None);
    }
    let previous = q
        .evo_view()
        .with_qc(q.bls(), |qc| {
            qc.mined.nth_mined_by_index(quorum.llmq_type, quorum.base, facts.index, 0)
        })
        .map_err(read_error)?
        .map_err(EvoError::from)?;
    Ok(Some(previous.map_or(0, |base| {
        (facts.base_height - q.chain.header(base).height as i32) / i32::from(params.dkg_interval)
            - 1
    })))
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
        pub_key_shares: vec![],
        sk_share: None,
    }
}

fn read_error(error: OnVMembersError) -> EvoError {
    match error {
        OnVMembersError::Members(MemberError::View(error)) => error.into(),
        error => EvoError::Failed(error.to_string()),
    }
}

/// One `quorumConnections` entry as V read it: the type, and at an active masternode a base at
/// or below the tip with its members.
pub(crate) struct ConnRead {
    pub(crate) llmq_type: LlmqType,
    pub(crate) params: LlmqParams,
    pub(crate) base: Option<(BlockHash, Vec<[u8; 32]>)>,
}

/// What `quorum dkgstatus` reads in one V turn: the entries per enabled type and index, then the
/// mineable commitments as `(type, quorumHash, quorumPublicKey)`.
pub(crate) type DkgStatusRead = (Vec<ConnRead>, Vec<(u8, QuorumHash, String)>);

/// `quorum dkgstatus` with the fields CH5's `mine_quorum()` reads: the debug manager's sessions
/// ([`DkgStatusSource::sessions`]), then per enabled type and quorum index the type's name and, on
/// a masternode, the base's hash and each connection's `proTxHash`, `connected` and `outbound`,
/// then each mineable commitment's numeric type, hash and public key. Core's fields left out, as
/// phase H's:
/// - top level: `time`, `timeStr`;
/// - `session[]`: `quorumIndex`, and in `status` `llmqType`, `quorumHeight`,
///   `sentContributions`, `sentComplaint`, `sentJustification`, `sentPrematureCommitment`,
///   `aborted`, `badMembers`, `weComplain` and, at detail 2, `allMembers`;
/// - `quorumConnections[]`: `quorumIndex`, `pQuorumBaseBlockIndex`, `pindexTip`, and each
///   connection's `address`;
/// - `minableCommitments[]`: `version`, `quorumIndex`, `signersCount`, `signers`,
///   `validMembersCount`, `validMembers`, `quorumVvecHash`, `quorumSig` and `membersSig`.
///
/// A build without `dkg-runtime` does not serve the command: it answers as the `quorum` family
/// does for an unknown subcommand. With `dkg-runtime`, a node with neither role answers Core's
/// object without `session` (the miner node `mine_quorum` reads).
// Core: v24.0.0 src/rpc/quorums.cpp:336-454 quorum_dkgstatus
pub(super) fn dkg_status(slot: &ChainSlot, detail: i32) -> Result<UniValue, EvoError> {
    let source = slot.dkg.get().cloned();
    if cfg!(not(feature = "dkg-runtime")) && source.is_none() {
        return Err(EvoError::Param(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Must be a valid command",
        )));
    }
    let chain = slot.slot().as_ref().ok_or(EvoError::Unavailable)?.chain.clone();
    let active = source.as_ref().is_some_and(|s| s.active_pro_tx().is_some());
    let read: DkgStatusRead =
        chain
            .on_v(move |on_v, q| dkg_status_read(on_v, q, active))
            .recv()
            .map_err(|_| EvoError::Failed(dashd_rpc::blockchain::ABORTED.into()))??;
    Ok(write_dkg_status(source.as_deref(), detail, read))
}

// Core: v24.0.0 src/rpc/quorums.cpp:360-453
pub(crate) fn write_dkg_status(
    source: Option<&dyn DkgStatusSource>,
    detail: i32,
    (reads, mineable): DkgStatusRead,
) -> UniValue {
    let mut ret = UniValue::new_object();
    if let Some(source) = source {
        ret.push_kv_end("session", source.sessions(detail));
    }
    let me = source.and_then(|s| s.active_pro_tx()).unwrap_or([0; 32]);
    let connected = source.map(|s| s.connected_pro_txs()).unwrap_or_default();
    let mut connections = Vec::new();
    for read in reads {
        let mut obj = UniValue::new_object();
        obj.push_kv_end("llmqType", read.llmq_type.to_string().into());
        if let (Some((hash, members)), Some(source)) = (read.base, source) {
            obj.push_kv_end("quorumHash", hash.to_string().into());
            let all_connected = source.all_members_connected(read.llmq_type);
            let params = &read.params;
            let all =
                sorted(LlmqTopology.quorum_connections(params, &members, me, false, all_connected));
            let outbound =
                LlmqTopology.quorum_connections(params, &members, me, true, all_connected);
            let arr = all
                .into_iter()
                .map(|pro_tx| {
                    let mut ecj = UniValue::new_object();
                    ecj.push_kv_end("proTxHash", ProTxHash::from(pro_tx).to_string().into());
                    ecj.push_kv_end("connected", connected.contains(&pro_tx).into());
                    ecj.push_kv_end("outbound", outbound.contains(&pro_tx).into());
                    ecj
                })
                .collect();
            obj.push_kv_end("quorumConnections", UniValue::Arr(arr));
        }
        connections.push(obj);
    }
    ret.push_kv_end("quorumConnections", UniValue::Arr(connections));
    let mineable = mineable
        .into_iter()
        .map(|(llmq_type, hash, pk)| {
            let mut obj = UniValue::new_object();
            obj.push_kv_end("llmqType", i64::from(llmq_type).into());
            obj.push_kv_end("quorumHash", hash.to_string().into());
            obj.push_kv_end("quorumPublicKey", pk.into());
            obj
        })
        .collect();
    ret.push_kv_end("minableCommitments", UniValue::Arr(mineable));
    ret
}

/// `std::set<uint256>`'s order: the stored bytes, compared as `memcmp` compares them.
fn sorted(set: impl IntoIterator<Item = [u8; 32]>) -> Vec<[u8; 32]> {
    let mut v: Vec<_> = set.into_iter().collect();
    v.sort_unstable();
    v
}

// Core: v24.0.0 src/rpc/quorums.cpp:369-450
fn dkg_status_read(
    on_v: &OnV<'_>,
    q: &ChainQuery<'_>,
    active: bool,
) -> Result<DkgStatusRead, EvoError> {
    let (tip, _) = q.chain.private_tip().ok_or(EvoError::Unavailable)?;
    let tip_height = i64::from(tip.height);
    let pool = q.evo_view().mineable();
    let enabled = q
        .evo_view()
        .with_qc(q.bls(), |qc| -> Result<_, EvoError> {
            let types =
                enabled_quorum_params(&qc.llmq, tip.id).map_err(|e| read_error(e.into()))?;
            let mut out = Vec::new();
            for (llmq_type, params) in types {
                let rotation = is_quorum_rotation_enabled(&qc.llmq, params, tip.id)
                    .map_err(|e| read_error(e.into()))?;
                let mineable = pool
                    .mineable_commitments(qc, llmq_type, params, tip.id, tip.height + 1)
                    .map_err(|e| EvoError::Failed(format!("{e:?}")))?;
                out.push((llmq_type, *params, rotation, mineable.unwrap_or_default()));
            }
            Ok(out)
        })
        .map_err(read_error)??;
    let mut reads = Vec::new();
    let mut mineable = Vec::new();
    for (llmq_type, params, rotation, commitments) in enabled {
        let count = if rotation {
            i64::from(params.signing_active_quorum_count)
        } else {
            1
        };
        for quorum_index in 0..count {
            let mut read = ConnRead {
                llmq_type,
                params,
                base: None,
            };
            let height = tip_height - tip_height % i64::from(params.dkg_interval) + quorum_index;
            if active && height <= tip_height {
                let base = q.chain.ancestor(tip.id, height as u32).ok_or(EvoError::Unavailable)?;
                let hash = q.chain.header(base).hash;
                let members = q.bind(on_v).members(llmq_type, base, false).map_err(read_error)?;
                let members = members.iter().map(|m| *m.pro_tx_hash.as_bytes()).collect();
                read.base = Some((hash, members));
            }
            reads.push(read);
        }
        for qc in commitments {
            let pk = qc.quorum_public_key().to_bytes(commitment_bls_scheme(qc.version()));
            mineable.push((llmq_type.to_base(), qc.quorum_hash(), hex(&pk)));
        }
    }
    Ok((reads, mineable))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
