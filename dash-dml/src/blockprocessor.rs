//! Core's `CQuorumBlockProcessor` without its store and its pool (LQ2; EV addendum §1.5): the
//! in-block commitment rules, the rows a connected block writes to the mined-commitment index,
//! the undo, and the `merkleRootQuorums` inputs (`designs/LQ2-notes.md`).
//!
//! Every function reads through the caller's views: the chain and deployment state of an
//! [`LlmqCtx`], the block index ([`BlockLookup`]), the active chain's mined commitments
//! ([`MinedCommitmentReader`], resolved through the tip, LQ2 D4), the members facade and the
//! precompute. The rows go back in [`EvoDelta::mined_commitments`]; the mineable-pool actions are
//! [`Effect`]s, which the chainstate applies whatever the block's verdict (A1 §2.10.1).
//!
//! [`EvoDelta::mined_commitments`]: crate::store::EvoDelta::mined_commitments

pub mod pool;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use dash_consensus::ctx::Deployment;
use dash_consensus::primitives::{Block, MerkleRoot, Tx};
use dash_consensus::{BlockId, BlockValidationError, BlockValidationResult};
use dash_evo::effect::{Effect, EffectSink};
use dash_evo::llmq::{
    BlsCheckRunner, BlsVerdict, CommitmentError, LlmqCommitment, LlmqCtx, MemberError,
    MinedCommitmentReader, MinedFamily, MinedIndexEntry, PrecomputeMode, QuorumPrecompute,
    base_through, enabled_quorum_params, is_quorum_rotation_enabled, queue_mined_signatures,
    verify_commitment, verify_null,
};
use dash_evo::sml::{BlockQc, QcHashes, QuorumRootParams, merkle_root_quorums};
use dash_params::LlmqType;
use dash_params::{BuriedDeployment, LlmqParams};
use dash_primitives::{BlockHash, QuorumHash, SpecialPayload, TxType};
use dash_types::Numeric;

use crate::build::payload;
use crate::specialtx::reasons::{
    BAD_QC_COMMITMENT_TYPE, BAD_QC_HEIGHT, BAD_QC_INVALID, BAD_QC_INVALID_NULL, BAD_QC_PAYLOAD,
};
use crate::store::{DmlMemberSource, MinedCommitmentRecord, NodeMemberError, StoreError};

/// `bad-qc-dup`.
pub const BAD_QC_DUP: &str = "bad-qc-dup";
/// `bad-qc-premature`.
pub const BAD_QC_PREMATURE: &str = "bad-qc-premature";
/// `bad-qc-not-allowed`.
pub const BAD_QC_NOT_ALLOWED: &str = "bad-qc-not-allowed";
/// `bad-qc-missing`.
pub const BAD_QC_MISSING: &str = "bad-qc-missing";
/// `bad-qc-block`.
pub const BAD_QC_BLOCK: &str = "bad-qc-block";

/// Core's `LookupBlockIndex`: any header in the block index, whatever its chain, with its height
/// (EV1 §2.3's shape; LQ2 D3).
pub trait BlockLookup {
    /// The block `hash` names, if the index holds its header.
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)>;
}

/// What the quorum block processor reads: Core's `CQuorumBlockProcessor` members and globals.
#[derive(Clone, Copy)]
pub struct QcCtx<'a> {
    /// The chain parameters, the block index as a chain view, deployment state, and the scheme
    /// of the block being processed (`bls_ctx_for_block(h)`; Core's `ProcessBlock` runs under
    /// it).
    pub llmq: LlmqCtx<'a, StoreError>,
    /// `LookupBlockIndex`.
    pub index: &'a dyn BlockLookup,
    /// The mined commitments of the chain ending at the active tip (Core's `q_mc` and its height
    /// indexes).
    pub mined: &'a dyn MinedCommitmentReader<Error = StoreError>,
    /// The members facade (`GetAllQuorumMembers`).
    pub members: &'a DmlMemberSource<'a>,
    /// `PreComputeQuorumMembers`.
    pub precompute: &'a dyn QuorumPrecompute<Error = StoreError>,
    /// `m_bls_queue`: runs the block's queued signature checks (LQ2 D11, DEC-325).
    pub bls_checks: &'a dyn BlsCheckRunner,
}

/// Why the block processor did not accept a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QcBlockError {
    /// Core's `state.Invalid(BLOCK_CONSENSUS, reason)`.
    Invalid(BlockValidationError),
    /// Core returns `false` without a reason: the connect fails and the block is not marked
    /// invalid. No block that passed `CheckLLMQCommitment` reaches it (an unknown base on the
    /// signature path, `blockprocessor.cpp` v24:271-274, :400-404; an unconfigured type in
    /// `ProcessCommitment`, :332-336).
    NoReason(&'static str),
    /// A signature check outside the BLS domain BS12 reproduces: no verdict (SB8).
    BlsUnsupported,
    /// A store or members fault: fail-stop, never a verdict (EV addendum §2.3).
    Fault(NodeMemberError),
}

impl QcBlockError {
    fn invalid(reason: &'static str) -> Self {
        QcBlockError::Invalid(BlockValidationError::new(BlockValidationResult::Consensus, reason))
    }

    /// The reject reason of a verdict; `None` otherwise.
    pub fn reject_reason(&self) -> Option<&str> {
        match self {
            QcBlockError::Invalid(e) => Some(e.reject_reason()),
            _ => None,
        }
    }
}

impl fmt::Display for QcBlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QcBlockError::Invalid(e) => write!(f, "{e}"),
            QcBlockError::NoReason(what) => write!(f, "quorum block processing failed: {what}"),
            QcBlockError::BlsUnsupported => {
                f.write_str("commitment signature check outside the supported BLS domain")
            }
            QcBlockError::Fault(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for QcBlockError {}

impl From<MemberError<StoreError>> for QcBlockError {
    fn from(e: MemberError<StoreError>) -> Self {
        QcBlockError::Fault(e.into())
    }
}

impl From<StoreError> for QcBlockError {
    fn from(e: StoreError) -> Self {
        QcBlockError::Fault(NodeMemberError::Store(e))
    }
}

/// Maps a `Verify` outcome: `false` is the caller's `reason`.
fn verified(
    r: Result<(), CommitmentError<StoreError>>,
    reason: &'static str,
) -> Result<(), QcBlockError> {
    match r {
        Ok(()) => Ok(()),
        Err(CommitmentError::Rejected(_)) => Err(QcBlockError::invalid(reason)),
        Err(CommitmentError::BlsUnsupported) => Err(QcBlockError::BlsUnsupported),
        Err(CommitmentError::Members(e)) => Err(e.into()),
    }
}

/// `::SerializeHash(qc)`: Core hashes its re-serialization of the decoded commitment, not the
/// bytes it received, and dash-consensus's commitment writer gives those bytes (LQ2 D20). The
/// tests pin it to the received bytes of every Core-mined commitment in the vectors.
pub use dash_consensus::primitives::commitment_hash;

/// `qc.quorumIndex`: 0 for a version that carries none.
fn quorum_index(qc: &LlmqCommitment) -> i16 {
    qc.quorum_index.unwrap_or(0)
}

/// The commitments of a block, as `GetCommitmentsFromBlock` returns them: Core's multimap ordered
/// by type, commitments of one type in block order.
pub type BlockCommitments = Vec<LlmqCommitment>;

/// Core's `GetCommitmentsFromBlock`: every type-6 transaction's commitment, ordered by type (a
/// stable sort, as `std::multimap::emplace` keeps equal keys in insertion order).
///
/// Rejects, in Core's order: a payload that does not decode (`bad-qc-payload`), an unconfigured
/// type (`bad-qc-commitment-type`), a second commitment of a type for which rotation is not
/// enabled at `pindex` (`bad-qc-dup`, null or not; LQ2 D12), and any commitment before DIP3 is
/// active at `pindex` (`bad-qc-premature`).
// Core: llmq/blockprocessor.cpp:564 CQuorumBlockProcessor::GetCommitmentsFromBlock
pub fn commitments_from_block(
    llmq: &LlmqCtx<'_, StoreError>,
    block: &Block,
    pindex: BlockId,
) -> Result<BlockCommitments, QcBlockError> {
    let mut ret: Vec<LlmqCommitment> = Vec::new();
    for tx in block.txs() {
        if tx.tx_type() != TxType::QuorumCommitment {
            continue;
        }
        let Some(SpecialPayload::QuorumCommitment(qc)) = payload(tx) else {
            return Err(QcBlockError::invalid(BAD_QC_PAYLOAD));
        };
        let qc = &qc.commitment;
        let Some(params) = llmq.params().consensus.llmq(qc.llmq_type) else {
            return Err(QcBlockError::invalid(BAD_QC_COMMITMENT_TYPE));
        };
        if !is_quorum_rotation_enabled(llmq, params, pindex)?
            && ret.iter().any(|seen| seen.llmq_type == qc.llmq_type)
        {
            return Err(QcBlockError::invalid(BAD_QC_DUP));
        }
        ret.push(qc.clone());
    }
    let dip3 = Deployment::Buried(BuriedDeployment::Dip0003);
    if !llmq.deployments().active_at(pindex, dip3) && !ret.is_empty() {
        return Err(QcBlockError::invalid(BAD_QC_PREMATURE));
    }
    ret.sort_by_key(|qc| qc.llmq_type.to_base());
    Ok(ret)
}

/// Core's `IsMiningPhase(params, chain, nHeight)`: whether `height` is inside its cycle's mining
/// window, `[start, end]` past the cycle start.
// Core: llmq/blockprocessor.cpp:311 IsMiningPhase
pub fn is_mining_phase(params: &LlmqParams, height: u32) -> bool {
    let height = i64::from(height);
    let cycle_start = height - height % i64::from(params.dkg_interval);
    let start = cycle_start + i64::from(params.dkg_mining_window_start);
    let end = cycle_start + i64::from(params.dkg_mining_window_end);
    (start..=end).contains(&height)
}

/// Core's `GetQuorumBlockHash(params, active_chain, nHeight, quorumIndex)`: the active chain's
/// block at the cycle start plus `quorum_index`, or `None` (Core's null hash) where the chain has
/// no block, the block being connected included (LQ2 D5).
// Core: llmq/blockprocessor.cpp:629 CQuorumBlockProcessor::GetQuorumBlockHash
pub fn quorum_block_hash(
    llmq: &LlmqCtx<'_, StoreError>,
    params: &LlmqParams,
    tip: BlockId,
    height: u32,
    quorum_index: i64,
) -> Result<Option<BlockHash>, QcBlockError> {
    let height = i64::from(height);
    let start = height - height % i64::from(params.dkg_interval) + quorum_index;
    let Ok(start) = u32::try_from(start) else {
        return Ok(None);
    };
    let view = llmq.chain();
    match view.ancestor(tip, start).map_err(MemberError::View)? {
        Some(id) => Ok(Some(view.header(id).map_err(MemberError::View)?.hash)),
        None => Ok(None),
    }
}

/// Core's `HasMinedCommitment(type, quorumHash, chain)`: a block of the active chain mined a
/// commitment for the quorum (LQ2 D4).
// Core: llmq/blockprocessor.cpp:650 CQuorumBlockProcessor::HasMinedCommitment
pub fn has_mined_commitment(
    mined: &dyn MinedCommitmentReader<Error = StoreError>,
    llmq_type: LlmqType,
    quorum_hash: &QuorumHash,
    tip: BlockId,
) -> Result<bool, StoreError> {
    Ok(mined.mined_commitment(llmq_type, quorum_hash, tip)?.is_some())
}

/// Core's `GetNumCommitmentsRequired(params, chain, nHeight)`: in the mining window, the number
/// of the cycle's quorums (one, or `signingActiveQuorumCount` when rotation is enabled at
/// `pindex`) whose base block the chain holds and for which no commitment is mined yet. `pindex`
/// is the tip for a block above it, else the tip's ancestor at `height` (LQ2 D12).
// Core: llmq/blockprocessor.cpp:606 CQuorumBlockProcessor::GetNumCommitmentsRequired
pub fn num_commitments_required(
    ctx: &QcCtx<'_>,
    llmq_type: LlmqType,
    params: &LlmqParams,
    tip: BlockId,
    height: u32,
) -> Result<usize, QcBlockError> {
    if !is_mining_phase(params, height) {
        return Ok(0);
    }
    let pindex = cycle_pindex(ctx, tip, height)?;
    let rotation = is_quorum_rotation_enabled(&ctx.llmq, params, pindex)?;
    let mut ret = 0;
    for index in 0..cycle_quorum_count(params, rotation) {
        let Some(hash) = quorum_block_hash(&ctx.llmq, params, tip, height, index as i64)? else {
            continue;
        };
        if !has_mined_commitment(ctx.mined, llmq_type, &as_quorum_hash(&hash), tip)? {
            ret += 1;
        }
    }
    Ok(ret)
}

/// The block whose state `GetNumCommitmentsRequired` and `GetMineableCommitments` read at
/// `height`: `tip` for a block above it, else the tip's ancestor at `height`.
fn cycle_pindex(ctx: &QcCtx<'_>, tip: BlockId, height: u32) -> Result<BlockId, QcBlockError> {
    let view = ctx.llmq.chain();
    let tip_height = view.header(tip).map_err(MemberError::View)?.height;
    if tip_height < height {
        return Ok(tip);
    }
    let pindex =
        view.ancestor(tip, height).map_err(MemberError::View)?.ok_or(MemberError::Missing(tip))?;
    Ok(pindex)
}

/// `signingActiveQuorumCount`.
fn signing_active_count(params: &LlmqParams) -> usize {
    usize::from(params.signing_active_quorum_count)
}

/// The number of quorums of one cycle: `signingActiveQuorumCount` under rotation, else one.
fn cycle_quorum_count(params: &LlmqParams, rotation: bool) -> usize {
    if rotation {
        signing_active_count(params)
    } else {
        1
    }
}

fn as_quorum_hash(hash: &BlockHash) -> QuorumHash {
    QuorumHash::from(*hash.as_bytes())
}

pub(crate) fn as_block_hash(hash: &QuorumHash) -> BlockHash {
    BlockHash::from(*hash.as_bytes())
}

/// The block index entry of a non-null commitment's quorum hash; Core's `return false` where it
/// is unknown (no block that passed `CheckLLMQCommitment` lacks one).
fn quorum_base(ctx: &QcCtx<'_>, qc: &LlmqCommitment) -> Result<(BlockId, u32), QcBlockError> {
    ctx.index
        .lookup(&as_block_hash(&qc.quorum_hash))
        .ok_or(QcBlockError::NoReason("no known block index for a quorum hash"))
}

/// The block being processed: its block index entry, hash and height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QcBlockAt {
    /// `pindex`.
    pub id: BlockId,
    /// `pindex->nHeight`.
    pub height: u32,
}

/// How `ProcessBlock` is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QcMode {
    /// `fJustCheck`: no row is written and no effect beyond the members' is emitted.
    pub just_check: bool,
    /// `fBLSChecks` (`fCheckCbTxMerkleRoots`): the queued signature checks run.
    pub bls_checks: bool,
}

/// Core's `CQuorumBlockProcessor::ProcessBlock(chainstate, block, pindex, state, fJustCheck,
/// fBLSChecks)`: the block's mined-commitment rows, in Core's write order, or why it fails.
///
/// `tip` is the active chain's tip (`chainstate.m_chain.Tip()`): `pindex`'s parent on every
/// connect, `None` only in crash replay, which skips the count checks and takes each
/// commitment's own quorum hash (LQ2 D5). Mined commitments are read through `tip`, or through
/// `pindex`'s parent when it is `None`.
///
/// In Core's order (LQ2 notes, "Block-level order"): below DIP3 nothing (Core writes only its
/// best-block marker); `PreComputeQuorumMembers`; [`commitments_from_block`]; for each type
/// enabled at the parent, the count checks (`bad-qc-not-allowed`, `bad-qc-missing`); v24's
/// duplicate pre-pass (`bad-qc-dup`, row 24, LQ2 D10); with `bls_checks`, every non-null
/// commitment's queued signature checks, one `bad-qc-invalid` for any failure (LQ2 D11); then
/// each commitment's `ProcessCommitment`. Effects (the members' snapshot rows, and per written
/// commitment an [`Effect::EraseMineableCommitment`]) are emitted in Core's order and stay
/// emitted when a later step fails.
// Core: llmq/blockprocessor.cpp:212 CQuorumBlockProcessor::ProcessBlock
pub fn process_block(
    ctx: &QcCtx<'_>,
    block: &Block,
    at: QcBlockAt,
    tip: Option<BlockId>,
    mode: QcMode,
    effects: &mut dyn EffectSink,
) -> Result<Vec<MinedCommitmentRecord>, QcBlockError> {
    let llmq = &ctx.llmq;
    let dip3 = Deployment::Buried(BuriedDeployment::Dip0003);
    if !llmq.deployments().active_at(at.id, dip3) {
        return Ok(Vec::new());
    }
    ctx.precompute.precompute(at.id, PrecomputeMode::Connect, effects)?;
    let qcs = commitments_from_block(llmq, block, at.id)?;
    let prev =
        llmq.chain().prev(at.id).map_err(MemberError::View)?.ok_or(MemberError::Missing(at.id))?;

    if let Some(tip) = tip {
        for (llmq_type, params) in enabled_quorum_params(llmq, prev)? {
            let required = num_commitments_required(ctx, llmq_type, params, tip, at.height)?;
            let in_block = qcs.iter().filter(|qc| qc.llmq_type == llmq_type).count();
            if required < in_block {
                return Err(QcBlockError::invalid(BAD_QC_NOT_ALLOWED));
            }
            if required > in_block {
                return Err(QcBlockError::invalid(BAD_QC_MISSING));
            }
        }
    }

    // v24's pre-pass (row 24): before any signature check and any `ProcessCommitment`.
    let mut seen = BTreeSet::new();
    for qc in &qcs {
        if !qc.is_null() && !seen.insert((qc.llmq_type.to_base(), qc.quorum_hash)) {
            return Err(QcBlockError::invalid(BAD_QC_DUP));
        }
    }

    if mode.bls_checks {
        // Core queues every commitment's checks, then `Wait()`s (v24:267-284): an unknown base
        // returns at once, before the queue's verdict; a failed check anywhere is
        // `bad-qc-invalid`, and a check without a BLS verdict decides only where none failed.
        let mut queue = Vec::new();
        for qc in qcs.iter().filter(|qc| !qc.is_null()) {
            let (base, _) = quorum_base(ctx, qc)?;
            queue_mined_signatures(llmq, ctx.members, qc, base, effects, &mut queue)?;
        }
        match ctx.bls_checks.run(queue) {
            BlsVerdict::Valid => {}
            BlsVerdict::Invalid => return Err(QcBlockError::invalid(BAD_QC_INVALID)),
            BlsVerdict::Unsupported => return Err(QcBlockError::BlsUnsupported),
        }
    }

    let read_at = tip.unwrap_or(prev);
    let mut rows = Vec::new();
    for qc in &qcs {
        if let Some(row) = process_commitment(ctx, at, tip, read_at, qc, mode.just_check, effects)?
        {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Core's `ProcessCommitment(nHeight, blockHash, qc, state, fJustCheck)`: the commitment's row
/// when it is non-null and written (`fJustCheck` false), after its checks.
// Core: llmq/blockprocessor.cpp:326 CQuorumBlockProcessor::ProcessCommitment
fn process_commitment(
    ctx: &QcCtx<'_>,
    at: QcBlockAt,
    tip: Option<BlockId>,
    read_at: BlockId,
    qc: &LlmqCommitment,
    just_check: bool,
    effects: &mut dyn EffectSink,
) -> Result<Option<MinedCommitmentRecord>, QcBlockError> {
    let llmq = &ctx.llmq;
    let Some(params) = llmq.params().consensus.llmq(qc.llmq_type) else {
        return Err(QcBlockError::NoReason("invalid commitment type"));
    };
    let quorum_hash = match tip {
        Some(tip) => quorum_block_hash(llmq, params, tip, at.height, i64::from(quorum_index(qc)))?
            .map(|h| as_quorum_hash(&h)),
        // Crash replay takes the commitment's own hash; a null one is still `bad-qc-block`
        // (v24:347-357).
        None => (*qc.quorum_hash.as_bytes() != [0; 32]).then_some(qc.quorum_hash),
    };
    if quorum_hash != Some(qc.quorum_hash) {
        return Err(QcBlockError::invalid(BAD_QC_BLOCK));
    }
    if qc.is_null() {
        return if verify_null(llmq.params(), qc) {
            Ok(None)
        } else {
            Err(QcBlockError::invalid(BAD_QC_INVALID_NULL))
        };
    }
    // A stored commitment is one an active-chain block mined, never this block (LQ2 D4), so it
    // is never Core's byte-identical re-derivation.
    if has_mined_commitment(ctx.mined, qc.llmq_type, &qc.quorum_hash, read_at)? {
        return Err(QcBlockError::invalid(BAD_QC_DUP));
    }
    if !is_mining_phase(params, at.height) {
        return Err(QcBlockError::invalid(BAD_QC_HEIGHT));
    }
    let (base, base_height) = quorum_base(ctx, qc)?;
    verified(verify_commitment(llmq, ctx.members, qc, base, None, effects), BAD_QC_INVALID)?;
    if just_check {
        return Ok(None);
    }
    let record = mined_record(llmq, params, qc, base, base_height)?;
    effects.emit(Effect::EraseMineableCommitment {
        llmq_type: qc.llmq_type,
        quorum_hash: qc.quorum_hash,
        commitment_hash: commitment_hash(qc),
    });
    Ok(Some(record))
}

/// The rows `ProcessCommitment` writes for a checked non-null commitment of a quorum based at
/// `base`: `q_mc`, and the height-index row, in the indexed family when rotation is enabled for
/// the type at the **base** block (v24:421, LQ2 D12), else the plain one.
// Core: llmq/blockprocessor.cpp:421-441 (rotation at the base, WriteDerived of q_mc, the index row)
pub fn mined_record(
    llmq: &LlmqCtx<'_, StoreError>,
    params: &LlmqParams,
    qc: &LlmqCommitment,
    base: BlockId,
    base_height: u32,
) -> Result<MinedCommitmentRecord, QcBlockError> {
    let rotation = is_quorum_rotation_enabled(llmq, params, base)?;
    Ok(MinedCommitmentRecord {
        llmq_type: qc.llmq_type,
        quorum_hash: qc.quorum_hash,
        base_height,
        rotation_index: rotation.then(|| quorum_index(qc)),
        commitment: Arc::new(qc.clone()),
    })
}

/// Core's `CQuorumBlockProcessor::UndoBlock(chainstate, block, pindex)`: nothing to erase in the
/// append-only store (LQ2 D6). `PreComputeQuorumMembers(reset_cache = true)`, then, for each
/// non-null commitment in [`commitments_from_block`]'s order, an
/// [`Effect::ReinsertMineableCommitment`]. A block whose commitments Core cannot read again is
/// Core's `return false` ([`QcBlockError::NoReason`]); no connected block is one.
// Core: llmq/blockprocessor.cpp:519 CQuorumBlockProcessor::UndoBlock
pub fn undo_block(
    ctx: &QcCtx<'_>,
    block: &Block,
    pindex: BlockId,
    effects: &mut dyn EffectSink,
) -> Result<(), QcBlockError> {
    ctx.precompute.precompute(pindex, PrecomputeMode::Undo, effects)?;
    let qcs = match commitments_from_block(&ctx.llmq, block, pindex) {
        Ok(qcs) => qcs,
        Err(QcBlockError::Invalid(_)) => {
            return Err(QcBlockError::NoReason("commitments of a disconnected block"));
        }
        Err(e) => return Err(e),
    };
    for qc in qcs.into_iter().filter(|qc| !qc.is_null()) {
        effects.emit(Effect::ReinsertMineableCommitment {
            commitment: Arc::new(qc),
        });
    }
    Ok(())
}

/// A mined record of the active set: its type, the block that mined it and its quorum base. A
/// block mines at most one commitment of a quorum, so the record fixes the commitment and its
/// hash on every chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MinedRecordId {
    llmq_type: u8,
    mined: BlockId,
    base: BlockId,
}

/// One configured type's part of the active set at `prev`, in `llmqs` order: whether rotation is
/// enabled, and its records in Core's order.
type ActiveRecords = (bool, Vec<MinedRecordId>);

/// Core's `GetQcHashes` caches (LQ2 D22, DEC-325): `m_qc_hashes_lru`, each mined record's
/// `SerializeHash` and quorum index, and `m_quorums_cached` with the hashes it gave, returned
/// whole while the active set is unchanged (`llmq/blockprocessor.cpp` v24:470-516).
/// [`QcHashCache::apply`] drops both where Core calls `DropQcHashesCache` (:461): for each
/// commitment `ProcessCommitment` writes (:446) and each one `UndoBlock` undoes (:551).
///
/// Transparent over a consistent store whose `BlockId`s name the same blocks for the cache's
/// life: a record is named by the blocks that fix its commitment, and the set by its records and
/// each type's rotation, so [`qc_hashes`] through any `prev` gives what it gives with an empty
/// cache, whether or not the caller applies the effects (which keeps Core's drops). Like Core's
/// LRU, a hit does not see a commitment row lost after it was hashed. It holds the records of the
/// last set read only, so alternating reads across a fork cost reads, not answers.
#[derive(Clone, Debug, Default)]
pub struct QcHashCache {
    active: Option<(Vec<ActiveRecords>, QcHashes)>,
    hashes: BTreeMap<MinedRecordId, ([u8; 32], i16)>,
}

impl QcHashCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// `DropQcHashesCache`.
    // Core: llmq/blockprocessor.cpp:461 CQuorumBlockProcessor::DropQcHashesCache
    pub fn clear(&mut self) {
        self.active = None;
        self.hashes.clear();
    }

    /// Applies an effect of the block processor: Core drops the caches with each pool erasure
    /// of `ProcessCommitment` (v24:446-452) and each re-insertion of `UndoBlock` (:551-556).
    /// Other effects leave them.
    pub fn apply(&mut self, effect: &Effect) {
        match effect {
            Effect::EraseMineableCommitment {
                ..
            }
            | Effect::ReinsertMineableCommitment {
                ..
            } => self.clear(),
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
}

/// Core's `GetMinedAndActiveCommitmentsUntilBlock(pindexPrev)`: for **every configured type**
/// (quirk 18, LQ2 D13), the newest `signingActiveQuorumCount` records of a type for which
/// rotation is not enabled at `prev`, newest first, or the newest record of each quorum index of
/// a rotated type.
// Core: llmq/blockprocessor.cpp:818 GetMinedAndActiveCommitmentsUntilBlock
fn active_records(
    llmq: &LlmqCtx<'_, StoreError>,
    mined: &dyn MinedCommitmentReader<Error = StoreError>,
    prev: BlockId,
) -> Result<Vec<ActiveRecords>, QcBlockError> {
    let chain = mined.chain();
    let height = chain.header(prev).height;
    let record = |llmq_type: LlmqType, row: &MinedIndexEntry| -> Result<_, QcBlockError> {
        Ok(MinedRecordId {
            llmq_type: llmq_type.to_base(),
            mined: chain
                .ancestor(prev, row.mined_height)
                .ok_or_else(|| StoreError::inconsistent("mined record above the queried block"))?,
            base: base_through::<StoreError>(chain, prev, row.base_height)?,
        })
    };
    let mut active = Vec::with_capacity(llmq.params().consensus.llmqs.len());
    for (llmq_type, params) in llmq.params().consensus.llmq_params() {
        let count = signing_active_count(params);
        let rotation = is_quorum_rotation_enabled(llmq, params, prev)?;
        let mut records = Vec::with_capacity(count);
        if rotation {
            for index in 0..count {
                let family = MinedFamily::Rotated {
                    quorum_index: i16::try_from(index).unwrap_or(i16::MAX),
                };
                if let Some(row) = mined.mined_index(llmq_type, family, prev, height, 1)?.first() {
                    records.push(record(llmq_type, row)?);
                }
            }
        } else {
            for row in
                &mined.mined_index(llmq_type, MinedFamily::NonRotated, prev, height, count)?
            {
                records.push(record(llmq_type, row)?);
            }
        }
        active.push((rotation, records));
    }
    Ok(active)
}

/// Core's `GetQcHashes(pindexPrev)`: the [`active_records`] at `prev`, each as `SerializeHash` of
/// the mined commitment read through `prev` (LQ2 D4), the hashes of `cache` reused (D22). A row
/// of the index without its commitment is corruption (Core's "commitment-not-found").
// Core: llmq/blockprocessor.cpp:470 GetQcHashes
pub fn qc_hashes(
    llmq: &LlmqCtx<'_, StoreError>,
    mined: &dyn MinedCommitmentReader<Error = StoreError>,
    cache: &mut QcHashCache,
    prev: BlockId,
) -> Result<QcHashes, QcBlockError> {
    let active = active_records(llmq, mined, prev)?;
    if let Some((cached, hashes)) = &cache.active
        && *cached == active
    {
        return Ok(hashes.clone());
    }
    let mut out = QcHashes::default();
    let mut hashes = BTreeMap::new();
    let types = llmq.params().consensus.llmq_params().map(|(t, _)| t);
    for (llmq_type, (rotation, records)) in types.zip(&active) {
        for record in records {
            let (qc_hash, index) = match cache.hashes.get(record) {
                Some(&cached) => cached,
                None => {
                    let hash = as_quorum_hash(&mined.chain().header(record.base).hash);
                    let Some((qc, _)) = mined.mined_commitment(llmq_type, &hash, prev)? else {
                        return Err(StoreError::inconsistent("commitment-not-found").into());
                    };
                    (commitment_hash(&qc), quorum_index(&qc))
                }
            };
            hashes.insert(*record, (qc_hash, index));
            if *rotation {
                out.insert_indexed(llmq_type, index, qc_hash);
            } else {
                out.push_plain(llmq_type, qc_hash);
            }
        }
    }
    cache.hashes = hashes;
    cache.active = Some((active, out.clone()));
    Ok(out)
}

/// A block's quorum-commitment transaction as `CalcCbTxMerkleRootQuorums` reads it.
fn block_qc(tx: &Tx) -> BlockQc {
    match payload(tx) {
        Some(SpecialPayload::QuorumCommitment(p)) if p.commitment.is_null() => BlockQc::Null,
        Some(SpecialPayload::QuorumCommitment(p)) => BlockQc::Commitment {
            llmq_type: p.commitment.llmq_type,
            quorum_index: quorum_index(&p.commitment),
            hash: commitment_hash(&p.commitment),
        },
        _ => BlockQc::BadPayload,
    }
}

/// Core's `CalcCbTxMerkleRootQuorums(block, pindexPrev)`: [`qc_hashes`] at `prev` (through
/// `cache`) plus the block's own commitments (from `vtx[1]`, special-version type-6
/// transactions), through SB7's [`merkle_root_quorums`] with every configured type's parameters
/// and rotation at `prev` (quirk 18). The outer error is a fault; the inner one Core's reason.
// Core: evo/cbtx.cpp:65 CalcCbTxMerkleRootQuorums
pub fn quorum_root(
    llmq: &LlmqCtx<'_, StoreError>,
    mined: &dyn MinedCommitmentReader<Error = StoreError>,
    cache: &mut QcHashCache,
    block: &Block,
    prev: BlockId,
) -> Result<Result<MerkleRoot, BlockValidationError>, QcBlockError> {
    let active = qc_hashes(llmq, mined, cache, prev)?;
    let mut types = BTreeMap::new();
    for (llmq_type, params) in llmq.params().consensus.llmq_params() {
        let root_params = QuorumRootParams {
            signing_active_quorum_count: signing_active_count(params),
            rotation: is_quorum_rotation_enabled(llmq, params, prev)?,
        };
        types.insert(llmq_type.to_base(), root_params);
    }
    let qcs = block
        .txs()
        .iter()
        .skip(1)
        .filter(|tx| tx.is_special_tx_version() && tx.tx_type() == TxType::QuorumCommitment)
        .map(|tx| block_qc(tx));
    Ok(merkle_root_quorums(active, qcs, |t| types.get(&t.to_base()).copied()))
}

#[cfg(test)]
#[path = "blockprocessor/tests.rs"]
mod tests;
