//! Core's mineable-commitment pool (`minableCommitmentsByQuorum`, `minableCommitments`), the
//! miner's read of it, and the `qfcommit` checks past the solicitation gate (LQ2 D8, D9).

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::ctx::Deployment;
use dash_consensus::primitives::{Transaction, Tx, TxRef, TxType, write_commitment};
use dash_evo::effect::{Effect, EffectSink};
use dash_evo::llmq::{
    CommitmentError, LlmqCommitment, LlmqCtx, MemberError, MinedCommitmentReader, SigPath,
    commitment_version, is_quorum_rotation_enabled, verify_commitment,
};
use dash_params::{BuriedDeployment, LlmqParams};
use dash_pkc::bls::{BlsEager, BlsValue};
use dash_primitives::{DynBitset, LlmqType, QuorumHash, QuorumVvecHash, commitment_bls_scheme};
use dash_types::Numeric;

use super::{
    BlockLookup, QcBlockError, QcCtx, as_block_hash, as_quorum_hash, commitment_hash, cycle_pindex,
    cycle_quorum_count, has_mined_commitment, num_commitments_required, quorum_block_hash,
    quorum_index,
};
use crate::store::{DmlMemberSource, StoreError};

/// `CFinalCommitmentTxPayload::CURRENT_VERSION`.
// Core: llmq/commitment.h:151
const QC_TX_PAYLOAD_VERSION: u16 = 1;

/// `CTransaction`'s version for a special transaction (`SetTxPayload`'s caller sets it).
const SPECIAL_TX_VERSION: i16 = 3;

/// The protocol version from which a stale `qfcommit` is punished (`version.h` v24:65).
pub const QFCOMMIT_STALE_REPROP_BAN_VERSION: i32 = 70239;

/// Core's `CQuorumBlockProcessor` pool: per (type, quorum hash) the hash of the best commitment
/// known, and the commitments by hash.
///
/// Reproduces `AddMineableCommitment`'s override (LQ2 D8): a commitment with more signers
/// replaces the quorum's entry, but the replaced commitment stays among the commitments by hash
/// until a block mines it, so [`MineablePool::has`] and [`MineablePool::get`] still see it.
#[derive(Clone, Debug, Default)]
pub struct MineablePool {
    by_quorum: BTreeMap<(u8, QuorumHash), [u8; 32]>,
    commitments: BTreeMap<[u8; 32], Arc<LlmqCommitment>>,
}

impl MineablePool {
    /// An empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Core's `AddMineableCommitment(qc)`: the commitment's hash when Core relays it (an inv),
    /// `None` otherwise.
    // Core: llmq/blockprocessor.cpp:840 CQuorumBlockProcessor::AddMineableCommitment
    pub fn add(&mut self, qc: &LlmqCommitment) -> Option<[u8; 32]> {
        let hash = commitment_hash(qc);
        let key = (qc.llmq_type.to_base(), qc.quorum_hash);
        match self.by_quorum.get_mut(&key) {
            None => {
                self.by_quorum.insert(key, hash);
                self.commitments.entry(hash).or_insert_with(|| Arc::new(qc.clone()));
                Some(hash)
            }
            Some(known) => {
                // `minableCommitments.at(insertedQuorumHash)`: present, as every entry's is.
                let old_signers =
                    self.commitments.get(known).map_or(0, |old| old.signers.count_ones());
                if qc.signers.count_ones() <= old_signers {
                    return None;
                }
                // Core points the entry at the new hash, then erases the *new* hash (absent), so
                // the old commitment stays (`:857-859`).
                *known = hash;
                self.commitments.remove(&hash);
                self.commitments.entry(hash).or_insert_with(|| Arc::new(qc.clone()));
                Some(hash)
            }
        }
    }

    /// Core's `HasMineableCommitment(hash)`.
    pub fn has(&self, hash: &[u8; 32]) -> bool {
        self.commitments.contains_key(hash)
    }

    /// Core's `GetMineableCommitmentByHash(hash)`.
    pub fn get(&self, hash: &[u8; 32]) -> Option<&Arc<LlmqCommitment>> {
        self.commitments.get(hash)
    }

    /// The commitment the quorum's entry names (`minableCommitments.at(byQuorum[k])`).
    pub fn best_for(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Option<&Arc<LlmqCommitment>> {
        let hash = self.by_quorum.get(&(llmq_type.to_base(), *quorum_hash))?;
        self.commitments.get(hash)
    }

    /// Applies a pool effect of the block processor: `ProcessCommitment`'s erasure (`:448-453`)
    /// or `UndoBlock`'s re-insertion (`:556`, whose inv Core discards). Other effects are not the
    /// pool's and are ignored.
    pub fn apply(&mut self, effect: &Effect) {
        match effect {
            Effect::EraseMineableCommitment {
                llmq_type,
                quorum_hash,
                commitment_hash,
            } => {
                self.by_quorum.remove(&(llmq_type.to_base(), *quorum_hash));
                self.commitments.remove(commitment_hash);
            }
            Effect::ReinsertMineableCommitment {
                commitment,
            } => {
                self.add(commitment);
            }
            Effect::StoreQuorumSnapshot {
                ..
            }
            | Effect::RemoveConflictingIsLock {
                ..
            }
            | Effect::ResetPlatformBan {
                ..
            }
            | Effect::NotifyMnListChanged {
                ..
            } => {}
        }
    }

    /// Core's `GetMineableCommitments(params, nHeight)` for the miner (MN1): `None` when the
    /// block needs no commitment of the type; otherwise, for each of the cycle's quorums in
    /// index order up to the first the chain does not hold, unless already mined, the pool's
    /// commitment or a null one (`CFinalCommitment(params, quorumHash)` with the index and the
    /// version for rotation and V19 at `pindex`). `pindex` is `tip` for a block above it.
    // Core: llmq/blockprocessor.cpp:882 CQuorumBlockProcessor::GetMineableCommitments
    pub fn mineable_commitments(
        &self,
        ctx: &QcCtx<'_>,
        params: &LlmqParams,
        tip: BlockId,
        height: u32,
    ) -> Result<Option<Vec<LlmqCommitment>>, QcBlockError> {
        if num_commitments_required(ctx, params, tip, height)? == 0 {
            return Ok(None);
        }
        let pindex = cycle_pindex(ctx, tip, height)?;
        let rotation = is_quorum_rotation_enabled(&ctx.llmq, params, pindex)?;
        let v19 = Deployment::Buried(BuriedDeployment::V19);
        let basic = ctx.llmq.deployments().active_after(Some(pindex), v19);
        let mut ret = Vec::new();
        for index in 0..cycle_quorum_count(params, rotation) {
            let Some(hash) = quorum_block_hash(&ctx.llmq, params, tip, height, index as i64)?
            else {
                break;
            };
            let hash = as_quorum_hash(&hash);
            if has_mined_commitment(ctx.mined, params.llmq_type, &hash, tip)? {
                continue;
            }
            match self.best_for(params.llmq_type, &hash) {
                Some(qc) => ret.push((**qc).clone()),
                None => {
                    let version = commitment_version(rotation, basic);
                    ret.push(null_commitment(params, hash, version, index as i16));
                }
            }
        }
        Ok((!ret.is_empty()).then_some(ret))
    }

    /// Core's `GetMineableCommitmentsTx(params, nHeight)` for the miner: each of
    /// [`MineablePool::mineable_commitments`]'s commitments in its own version-3,
    /// `TRANSACTION_QUORUM_COMMITMENT` transaction with no inputs or outputs, whose payload is
    /// `CFinalCommitmentTxPayload { CURRENT_VERSION, nHeight, commitment }`; `None` where it has
    /// none (Core's `false`).
    // Core: llmq/blockprocessor.cpp:938 CQuorumBlockProcessor::GetMineableCommitmentsTx
    pub fn mineable_commitments_tx(
        &self,
        ctx: &QcCtx<'_>,
        params: &LlmqParams,
        tip: BlockId,
        height: u32,
    ) -> Result<Option<Vec<TxRef>>, QcBlockError> {
        let Some(qcs) = self.mineable_commitments(ctx, params, tip, height)? else {
            return Ok(None);
        };
        let txs = qcs
            .iter()
            .map(|qc| {
                let mut payload = Vec::new();
                payload.extend(QC_TX_PAYLOAD_VERSION.to_le_bytes());
                payload.extend(height.to_le_bytes());
                write_commitment(qc, &mut payload);
                Arc::new(Tx::new(Transaction {
                    version: SPECIAL_TX_VERSION,
                    tx_type: TxType::QuorumCommitment,
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    lock_time: 0,
                    extra_payload: payload,
                }))
            })
            .collect();
        Ok(Some(txs))
    }
}

/// `CFinalCommitment(params, quorumHash)` with `quorumIndex` and `nVersion` set: bitsets of the
/// quorum's size, every other field null.
// Core: llmq/commitment.cpp:27 CFinalCommitment::CFinalCommitment
pub fn null_commitment(
    params: &LlmqParams,
    quorum_hash: QuorumHash,
    version: u16,
    quorum_index: i16,
) -> LlmqCommitment {
    let size = u64::try_from(params.size).unwrap_or(0);
    let bitset = || DynBitset {
        num_bits: size,
        data: vec![0; usize::try_from(size.div_ceil(8)).unwrap_or(0)],
    };
    let scheme = commitment_bls_scheme(version);
    LlmqCommitment {
        version,
        llmq_type: params.llmq_type,
        quorum_hash,
        quorum_index: (version == 2 || version == 4).then_some(quorum_index),
        signers: bitset(),
        valid_members: bitset(),
        quorum_public_key: BlsEager::new(BlsValue::invalid(), scheme),
        quorum_vvec_hash: QuorumVvecHash::from([0; 32]),
        quorum_sig: BlsEager::new(BlsValue::invalid(), scheme),
        members_sig: BlsEager::new(BlsValue::invalid(), scheme),
    }
}

/// What Core does with a solicited `qfcommit` (`ProcessMessage` past the gate).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QfcommitOutcome {
    /// `MisbehavingError{score}`.
    Misbehaving(i32),
    /// Dropped without a score.
    Ignored,
    /// Verified and offered to the pool; `Some(hash)` is the inv Core relays.
    Accepted(Option<[u8; 32]>),
}

/// Core's `qfcommit` handling after the solicitation check, in Core's order (LQ2 D9): a null
/// commitment or an unconfigured type is punished; an unknown base, one off the active chain
/// (`tip`'s ancestry) is ignored; a base that is not its interval's first block plus the index is
/// punished; a base older than `tip - dkgInterval` is punished only for a peer whose common
/// version is at least [`QFCOMMIT_STALE_REPROP_BAN_VERSION`]; an already mined quorum, and one
/// for which the pool's commitment has no more signers than this one (Core's comparison,
/// `:182-189`), are ignored; then `Verify(checkSigs = true)` ([`SigPath::Sync`]) with `ctx`'s
/// scheme (the tip's), whose failure is punished; then [`MineablePool::add`].
// Core: llmq/blockprocessor.cpp:98 CQuorumBlockProcessor::ProcessMessage
#[allow(clippy::too_many_arguments)]
pub fn qfcommit_verdict(
    pool: &mut MineablePool,
    llmq: &LlmqCtx<'_, StoreError>,
    index: &dyn BlockLookup,
    mined: &dyn MinedCommitmentReader<Error = StoreError>,
    members: &DmlMemberSource<'_>,
    tip: BlockId,
    peer_common_version: i32,
    qc: &LlmqCommitment,
    effects: &mut dyn EffectSink,
) -> Result<QfcommitOutcome, QcBlockError> {
    if qc.is_null() {
        return Ok(QfcommitOutcome::Misbehaving(100));
    }
    let Some(params) = llmq.params().consensus.llmq(qc.llmq_type) else {
        return Ok(QfcommitOutcome::Misbehaving(100));
    };
    let Some((base, base_height)) = index.lookup(&as_block_hash(&qc.quorum_hash)) else {
        return Ok(QfcommitOutcome::Ignored);
    };
    let view = llmq.chain();
    if view.ancestor(tip, base_height).map_err(MemberError::View)? != Some(base) {
        return Ok(QfcommitOutcome::Ignored);
    }
    let height = i64::from(base_height);
    let interval = i64::from(params.dkg_interval);
    if height - height % interval + i64::from(quorum_index(qc)) != height {
        return Ok(QfcommitOutcome::Misbehaving(100));
    }
    let tip_height = i64::from(view.header(tip).map_err(MemberError::View)?.height);
    if height < tip_height - interval {
        return Ok(if peer_common_version >= QFCOMMIT_STALE_REPROP_BAN_VERSION {
            QfcommitOutcome::Misbehaving(100)
        } else {
            QfcommitOutcome::Ignored
        });
    }
    if has_mined_commitment(mined, qc.llmq_type, &qc.quorum_hash, tip)? {
        return Ok(QfcommitOutcome::Ignored);
    }
    if pool
        .best_for(qc.llmq_type, &qc.quorum_hash)
        .is_some_and(|known| known.signers.count_ones() <= qc.signers.count_ones())
    {
        return Ok(QfcommitOutcome::Ignored);
    }
    match verify_commitment(llmq, members, qc, base, Some(SigPath::Sync), effects) {
        Ok(()) => Ok(QfcommitOutcome::Accepted(pool.add(qc))),
        Err(CommitmentError::Rejected(_)) => Ok(QfcommitOutcome::Misbehaving(100)),
        Err(CommitmentError::BlsUnsupported) => Err(QcBlockError::BlsUnsupported),
        Err(CommitmentError::Members(e)) => Err(e.into()),
    }
}
