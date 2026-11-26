//! The `qrinfo` message (Core `CQuorumRotationInfo`, `llmq/snapshot.{h,cpp}`): its builder,
//! `BuildQuorumRotationInfo`, over one chain view, and its serialization.
//!
//! The builder only reads: the block index, the rotation snapshot rows, the active chain's
//! mined-commitment index and the inner `mnlistdiff`s, which the source builds
//! ([`QrInfoSource::mnlistdiff`]). Its diffs are the source's [`QrInfoSource::Diff`], so a node
//! that reads block files after its chain-view turn maps them afterwards
//! ([`QuorumRotationInfo::try_map`]) in Core's build order. Decisions: PLAN-LQ3c §1-§3.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::primitives::{CoreWrite, write_compact_size};
use dash_evo::llmq::{
    BrokenInvariant, MinedCommitmentReader, MinedFamily, QuorumSnapshot, WORK_DIFF_DEPTH,
    base_through,
};
use dash_params::{LlmqParams, LlmqType};
use dash_primitives::BlockHash;
use dash_types::Numeric;

use crate::mnlistdiff::{
    Hex, MnListDiffError, PartialMerkleTree, PeerOutcome, QuorumKey, SimplifiedMnListDiff,
};

#[cfg(test)]
mod tests;

/// `MAX_BASE_BLOCK_HASHES`: the most diff bases a request may carry.
// Core: v24.0.0 src/llmq/snapshot.h:100 MAX_BASE_BLOCK_HASHES
pub const MAX_BASE_BLOCK_HASHES: usize = 4096;

/// What `BuildQuorumRotationInfo` reads, from one chain view: Core holds `cs_main` from the
/// first lookup to the reply (`net_processing.cpp` v24.0.0:5689, `rpc/quorums.cpp` :995).
/// Every [`BlockId`] resolves against `mined().chain()`.
pub trait QrInfoSource {
    /// A store fault; a broken ancestry invariant is one.
    type Error: From<BrokenInvariant>;
    /// One inner `BuildSimplifiedMNListDiff` result: a built diff, or a node capture handle.
    type Diff;

    /// `LookupBlockIndex(hash)`.
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId>;
    /// `ActiveChain().Genesis()`.
    fn active_genesis(&self) -> Option<BlockId>;
    /// `ActiveChain().Tip()`.
    fn active_tip(&self) -> Option<BlockId>;
    /// `ActiveChain().Contains(id)`.
    fn in_active_chain(&self, id: BlockId) -> bool;
    /// `nStatus & BLOCK_HAVE_DATA`.
    fn have_data(&self, id: BlockId) -> bool;
    /// `GetSnapshotForBlock(type, cycle)`: `None` is Core's absence, `Err` a store fault.
    // Core: v24.0.0 src/llmq/snapshot.cpp:301-318 GetSnapshotForBlock
    fn quorum_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle: &BlockHash,
    ) -> Result<Option<Arc<QuorumSnapshot>>, Self::Error>;
    /// The active chain's mined-commitment index, and the block index (`chain()`) every
    /// [`BlockId`] resolves against. The builder reads the index at `chain_end = tip`.
    fn mined(&self) -> &dyn MinedCommitmentReader<Error = Self::Error>;
    /// `GetMinedCommitment(type, hash).first`: `mined().mined_commitment(type, hash, tip)` as the
    /// serialized `CFinalCommitment`, the first half of the `q_mc` value, the same answer as
    /// [`crate::mnlistdiff::MnListDiffSource::mined_commitment`]. `None` is `{null, ZERO}`.
    // Core: v24.0.0 src/llmq/blockprocessor.cpp:675-684 GetMinedCommitment
    fn commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, Self::Error>;
    /// `BuildSimplifiedMNListDiff(base, block, extended = false)`, with its own refusals. The
    /// base is never null here.
    // Core: v24.0.0 src/evo/smldiff.cpp:154-247 BuildSimplifiedMNListDiff
    fn mnlistdiff(
        &self,
        base: &BlockHash,
        block: &BlockHash,
    ) -> Result<Self::Diff, MnListDiffError<Self::Error>>;
}

/// One `getqrinfo` request (Core `CGetQuorumRotationInfo`).
#[derive(Clone, Copy, Debug)]
pub struct QrInfoRequest<'a> {
    /// `baseBlockHashes`, in request order.
    pub base_block_hashes: &'a [BlockHash],
    /// `blockRequestHash`.
    pub block_request_hash: BlockHash,
    /// `extraShare`.
    pub extra_share: bool,
}

/// The chain parameters the builder reads.
#[derive(Clone, Copy, Debug)]
pub struct QrInfoParams<'a> {
    /// `llmqTypeDIP0024InstantSend`.
    pub llmq_type: LlmqType,
    /// That type's parameters. Core divides by `dkgInterval`, which is positive for every
    /// v24 type; the builder reads an invalid zero interval as one (a non-Core guard against a
    /// division by zero, the only case where it differs from Core).
    pub params: &'a LlmqParams,
    /// `hashGenesisBlock`.
    pub genesis_hash: BlockHash,
}

/// A target cycle's snapshot and the diff to its work block (Core `CycleData`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CycleData<D> {
    /// `m_snap`.
    pub snapshot: Arc<QuorumSnapshot>,
    /// `m_diff`.
    pub diff: D,
}

/// A Core `CQuorumRotationInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuorumRotationInfo<D> {
    /// `extraShare`.
    pub extra_share: bool,
    /// `cycleHMinusC`, `cycleHMinus2C`, `cycleHMinus3C`, and `cycleHMinus4C` with `extraShare`.
    /// The builder fills all of them. The encoder writes Core's `CycleData{}` (a default snapshot
    /// and diff) for any of those it finds missing, as Core's `value_or(CycleData{})` does for the
    /// fourth, and ignores entries past them.
    pub cycles: Vec<CycleData<D>>,
    /// `mnListDiffTip`.
    pub diff_tip: D,
    /// `mnListDiffH`.
    pub diff_h: D,
    /// `lastCommitmentPerIndex`, serialized `CFinalCommitment`s.
    pub last_commitment_per_index: Vec<Vec<u8>>,
    /// `quorumSnapshotList`.
    pub quorum_snapshot_list: Vec<Arc<QuorumSnapshot>>,
    /// `mnListDiffList`, paired with `quorum_snapshot_list`.
    pub mn_list_diff_list: Vec<D>,
    legacy: bool,
}

impl<D> QuorumRotationInfo<D> {
    /// Maps every diff in Core's build order and stops at the first error.
    pub fn try_map<D2, E>(
        self,
        mut f: impl FnMut(D) -> Result<D2, E>,
    ) -> Result<QuorumRotationInfo<D2>, E> {
        let (snapshots, mut diffs): (Vec<_>, Vec<_>) =
            self.cycles.into_iter().map(|c| (c.snapshot, c.diff)).unzip();
        let (diff_tip, diff_h, mut mapped, mn_list_diff_list);
        if self.legacy {
            diff_tip = f(self.diff_tip)?;
            diff_h = f(self.diff_h)?;
            mapped = map_all(diffs, &mut f)?;
            mn_list_diff_list = map_all(self.mn_list_diff_list, &mut f)?;
        } else {
            mn_list_diff_list = map_all(self.mn_list_diff_list, &mut f)?;
            diffs.reverse();
            mapped = map_all(diffs, &mut f)?;
            mapped.reverse();
            diff_h = f(self.diff_h)?;
            diff_tip = f(self.diff_tip)?;
        }
        Ok(QuorumRotationInfo {
            extra_share: self.extra_share,
            cycles: cycle_data(snapshots, mapped),
            diff_tip,
            diff_h,
            last_commitment_per_index: self.last_commitment_per_index,
            quorum_snapshot_list: self.quorum_snapshot_list,
            mn_list_diff_list,
            legacy: self.legacy,
        })
    }
}

fn map_all<D, D2, E>(v: Vec<D>, f: impl FnMut(D) -> Result<D2, E>) -> Result<Vec<D2>, E> {
    v.into_iter().map(f).collect()
}

fn cycle_data<D>(snapshots: Vec<Arc<QuorumSnapshot>>, diffs: Vec<D>) -> Vec<CycleData<D>> {
    snapshots
        .into_iter()
        .zip(diffs)
        .map(|(snapshot, diff)| CycleData {
            snapshot,
            diff,
        })
        .collect()
}

impl QuorumRotationInfo<SimplifiedMnListDiff> {
    /// The message payload for a peer whose common version is `proto_version`.
    // Core: v24.0.0 src/llmq/snapshot.h:144-183 CQuorumRotationInfo::Serialize
    pub fn encode(&self, proto_version: i32, w: &mut impl CoreWrite) {
        let default = CycleData {
            snapshot: Arc::default(),
            diff: default_diff(),
        };
        let cycle = |i: usize| self.cycles.get(i).unwrap_or(&default);
        for i in 0..3 {
            write_snapshot(&cycle(i).snapshot, w);
        }
        self.diff_tip.encode(proto_version, w);
        self.diff_h.encode(proto_version, w);
        for i in 0..3 {
            cycle(i).diff.encode(proto_version, w);
        }
        w.write(&[u8::from(self.extra_share)]);
        if self.extra_share {
            write_snapshot(&cycle(3).snapshot, w);
            cycle(3).diff.encode(proto_version, w);
        }
        write_compact_size(self.last_commitment_per_index.len() as u64, w);
        for commitment in &self.last_commitment_per_index {
            w.write(commitment);
        }
        write_compact_size(self.quorum_snapshot_list.len() as u64, w);
        for snapshot in &self.quorum_snapshot_list {
            write_snapshot(snapshot, w);
        }
        write_compact_size(self.mn_list_diff_list.len() as u64, w);
        for diff in &self.mn_list_diff_list {
            diff.encode(proto_version, w);
        }
    }

    /// [`QuorumRotationInfo::encode`] into a new buffer.
    pub fn to_bytes(&self, proto_version: i32) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(proto_version, &mut out);
        out
    }
}

/// A default `CSimplifiedMNListDiff`: null hashes, an empty merkle tree, a default
/// `CMutableTransaction` (version 2, normal type, no inputs or outputs, lock time 0) and no
/// entries.
// Core: v24.0.0 src/evo/smldiff.h:49-62 CSimplifiedMNListDiff
// Core: v24.0.0 src/primitives/transaction.cpp:71 CMutableTransaction
fn default_diff() -> SimplifiedMnListDiff {
    SimplifiedMnListDiff {
        base_block_hash: BlockHash::from([0; 32]),
        block_hash: BlockHash::from([0; 32]),
        cb_tx_merkle_tree: PartialMerkleTree {
            transactions: 0,
            hashes: Vec::new(),
            bits: Vec::new(),
        },
        cb_tx: vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        deleted_mns: Vec::new(),
        mn_list: Vec::new(),
        deleted_quorums: Vec::new(),
        new_quorums: Vec::new(),
        quorums_cl_sigs: Vec::new(),
    }
}

/// A `CQuorumSnapshot`: the skip mode, the member bits (`WriteFixedBitSet`), the skip list.
// Core: v24.0.0 src/llmq/snapshot.h:60-71 CQuorumSnapshot::Serialize
fn write_snapshot(snapshot: &QuorumSnapshot, w: &mut impl CoreWrite) {
    w.write(&snapshot.skip_list_mode.to_le_bytes());
    let members = &snapshot.active_quorum_members;
    write_compact_size(members.len() as u64, w);
    let mut bits = vec![0u8; members.len().div_ceil(8)];
    for (i, member) in members.iter().enumerate() {
        bits[i / 8] |= u8::from(*member) << (i % 8);
    }
    w.write(&bits);
    write_compact_size(snapshot.skip_list.len() as u64, w);
    for entry in &snapshot.skip_list {
        w.write(&entry.to_le_bytes());
    }
}

/// A failed build: the error and the diffs built before it, in build order, so a two-phase
/// source keeps Core's error order (a failure in a diff Core built earlier wins).
#[derive(Debug)]
pub struct QrInfoFailure<D, E> {
    /// The diffs built before the error, in build order.
    pub built: Vec<D>,
    /// Why the build stopped.
    pub error: QrInfoBuildError<E>,
}

/// Why a `getqrinfo` request was not answered. `Display` is Core's `errorRet`.
#[derive(Debug, PartialEq, Eq)]
pub enum QrInfoBuildError<E> {
    /// More than [`MAX_BASE_BLOCK_HASHES`] bases (only an in-memory request reaches it).
    TooManyBases(usize),
    /// No active genesis.
    GenesisNotFound,
    /// A request base the block index does not hold.
    BaseNotFound(BlockHash),
    /// A request base outside the active chain.
    BaseNotActive(BlockHash),
    /// No active tip.
    TipNotFound,
    /// A block whose data is not on disk.
    BlockDataUnavailable(BlockHash),
    /// The request block is unknown.
    RequestNotFound,
    /// A cycle block below genesis or above the block it is read from.
    CannotFindBlock,
    /// A cycle's work block below genesis.
    CannotFindWorkBlock,
    /// A cycle with no rotation snapshot row.
    NoSnapshot,
    /// A last-mined quorum with no mined commitment; Core's `errorRet` is empty.
    NoMinedCommitment(QuorumKey),
    /// An inner `mnlistdiff` refusal.
    Diff(MnListDiffError<E>),
    /// A store fault or a broken invariant, where Core throws or asserts.
    Source(E),
}

impl<E> QrInfoBuildError<E> {
    /// What Core does to the peer: an error carrying `BLOCK_DATA_UNAVAILABLE_SUFFIX`, or one Core
    /// throws out of the handler, is logged and dropped; every other refusal scores 1.
    // Core: v24.0.0 src/net_processing.cpp:5694-5703 GETQUORUMROTATIONINFO; src/evo/smldiff.cpp:249-252
    pub fn peer_outcome(&self) -> PeerOutcome {
        match self {
            QrInfoBuildError::BlockDataUnavailable(_) | QrInfoBuildError::Source(_) => {
                PeerOutcome::Unscored
            }
            QrInfoBuildError::Diff(e) => e.peer_outcome(),
            _ => PeerOutcome::Misbehaving(1),
        }
    }
}

impl<E: fmt::Display> fmt::Display for QrInfoBuildError<E> {
    // Core: v24.0.0 src/llmq/snapshot.cpp:26-32,35-65,77-257 errorRet
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QrInfoBuildError::TooManyBases(n) => {
                write!(f, "too many baseBlockHashes: {n} > {MAX_BASE_BLOCK_HASHES}")
            }
            QrInfoBuildError::GenesisNotFound => f.write_str("genesis block not found"),
            QrInfoBuildError::BaseNotFound(h) => write!(f, "block {} not found", Hex(h)),
            QrInfoBuildError::BaseNotActive(h) => {
                write!(f, "block {} is not in the active chain", Hex(h))
            }
            QrInfoBuildError::TipNotFound => f.write_str("tip block not found"),
            // snap:30 writes smld:191-192's text.
            QrInfoBuildError::BlockDataUnavailable(h) => {
                MnListDiffError::<E>::BlockDataUnavailable(*h).fmt(f)
            }
            QrInfoBuildError::RequestNotFound => f.write_str("block not found"),
            QrInfoBuildError::CannotFindBlock => f.write_str("Cannot find block"),
            QrInfoBuildError::CannotFindWorkBlock => f.write_str("Cannot find work block"),
            QrInfoBuildError::NoSnapshot => f.write_str("Cannot find quorum snapshot"),
            QrInfoBuildError::NoMinedCommitment(_) => Ok(()),
            QrInfoBuildError::Diff(e) => e.fmt(f),
            QrInfoBuildError::Source(e) => e.fmt(f),
        }
    }
}

type BuildResult<T, E> = Result<T, QrInfoBuildError<E>>;

/// What the build has made so far, each diff in its build order.
struct Parts<D> {
    legacy: bool,
    diff_tip: Option<D>,
    diff_h: Option<D>,
    cycle_snapshots: Vec<Arc<QuorumSnapshot>>,
    /// Ascending (`H−C` first) on the legacy path, oldest first otherwise.
    cycle_diffs: Vec<D>,
    commitments: Vec<Vec<u8>>,
    list_snapshots: Vec<Arc<QuorumSnapshot>>,
    list_diffs: Vec<D>,
}

impl<D> Parts<D> {
    fn into_built(self) -> Vec<D> {
        let (tip, h) = (self.diff_tip.into_iter(), self.diff_h.into_iter());
        if self.legacy {
            tip.chain(h).chain(self.cycle_diffs).chain(self.list_diffs).collect()
        } else {
            self.list_diffs.into_iter().chain(self.cycle_diffs).chain(h).chain(tip).collect()
        }
    }

    fn finish(mut self, extra_share: bool) -> QuorumRotationInfo<D> {
        if !self.legacy {
            self.cycle_diffs.reverse();
        }
        QuorumRotationInfo {
            extra_share,
            cycles: cycle_data(self.cycle_snapshots, self.cycle_diffs),
            diff_tip: self.diff_tip.expect("both paths build the tip diff"),
            diff_h: self.diff_h.expect("both paths build the H diff"),
            last_commitment_per_index: self.commitments,
            quorum_snapshot_list: self.list_snapshots,
            mn_list_diff_list: self.list_diffs,
            legacy: self.legacy,
        }
    }
}

/// `BuildQuorumRotationInfo(request, use_legacy_construction)`: the reply, or Core's refusal
/// with the diffs built before it. RPC callers pass `use_legacy_construction = false`.
// Core: v24.0.0 src/llmq/snapshot.cpp:77-257 BuildQuorumRotationInfo
#[expect(
    clippy::type_complexity,
    reason = "The reply and the failure both carry the source's diff type"
)]
pub fn build_qrinfo<S: QrInfoSource>(
    src: &S,
    p: &QrInfoParams<'_>,
    req: &QrInfoRequest<'_>,
    use_legacy_construction: bool,
) -> Result<QuorumRotationInfo<S::Diff>, QrInfoFailure<S::Diff, S::Error>> {
    let mut parts = Parts {
        legacy: use_legacy_construction,
        diff_tip: None,
        diff_h: None,
        cycle_snapshots: Vec::new(),
        cycle_diffs: Vec::new(),
        commitments: Vec::new(),
        list_snapshots: Vec::new(),
        list_diffs: Vec::new(),
    };
    match build(src, p, req, &mut parts) {
        Ok(()) => Ok(parts.finish(req.extra_share)),
        Err(error) => Err(QrInfoFailure {
            built: parts.into_built(),
            error,
        }),
    }
}

/// A base block list entry: height and block.
type Base = (u32, BlockId);

// Core: v24.0.0 src/llmq/snapshot.cpp:77-257 BuildQuorumRotationInfo
fn build<S: QrInfoSource>(
    src: &S,
    p: &QrInfoParams<'_>,
    req: &QrInfoRequest<'_>,
    out: &mut Parts<S::Diff>,
) -> BuildResult<(), S::Error> {
    let legacy = out.legacy;
    let chain = src.mined().chain();
    let hash = |id: BlockId| chain.header(id).hash;
    let base_of = |id: BlockId| -> Base { (chain.header(id).height, id) };
    let diff = |base: BlockHash, target: BlockId| {
        src.mnlistdiff(&base, &hash(target)).map_err(QrInfoBuildError::Diff)
    };

    if req.base_block_hashes.len() > MAX_BASE_BLOCK_HASHES {
        return Err(QrInfoBuildError::TooManyBases(req.base_block_hashes.len()));
    }
    let mut bases: Vec<Base> = Vec::new();
    if req.base_block_hashes.is_empty() {
        let genesis = src.active_genesis().ok_or(QrInfoBuildError::<S::Error>::GenesisNotFound)?;
        bases.push(base_of(genesis));
    } else {
        for requested in req.base_block_hashes {
            let id = src
                .lookup(requested)
                .ok_or(QrInfoBuildError::<S::Error>::BaseNotFound(*requested))?;
            if !src.in_active_chain(id) {
                return Err(QrInfoBuildError::BaseNotActive(*requested));
            }
            bases.push(base_of(id));
        }
        bases.sort_by_key(|base| base.0);
        if !legacy {
            bases.dedup_by_key(|base| base.1);
        }
    }
    let base_for = |bases: &[Base], target: BlockId| last_base(chain, bases, target, p);

    let tip = src.active_tip().ok_or(QrInfoBuildError::<S::Error>::TipNotFound)?;
    check_data(src, tip)?;
    if legacy {
        let back = bases.last().map_or(p.genesis_hash, |base| hash(base.1));
        out.diff_tip = Some(diff(back, tip)?);
    }

    let block =
        src.lookup(&req.block_request_hash).ok_or(QrInfoBuildError::<S::Error>::RequestNotFound)?;
    check_data(src, block)?;

    let cycle_length = i64::from(p.params.dkg_interval.max(1));
    let block_height = i64::from(chain.header(block).height);
    let (cycle_base, work_h) =
        cycle_blocks(src, legacy, block_height - block_height % cycle_length, block)?;
    if legacy {
        out.diff_h = Some(diff(base_for(&bases, work_h), work_h)?);
    }

    let cycle_base_height = i64::from(chain.header(cycle_base).height);
    let mut targets = Vec::new();
    for idx in 0..3 + i64::from(req.extra_share) {
        let height = cycle_base_height - cycle_length * (idx + 1);
        let (cycle, work, snapshot) = construct_cycle(src, p, legacy, height, tip)?;
        if legacy {
            out.cycle_diffs.push(diff(base_for(&bases, work), work)?);
        }
        out.cycle_snapshots.push(snapshot);
        targets.push((cycle, work));
    }

    // Core: v24.0.0 src/llmq/blockprocessor.cpp:779-794 GetLastMinedCommitmentsPerQuorumIndexUntilBlock
    let mut last_mined = Vec::new();
    let quorum_count = i16::try_from(p.params.signing_active_quorum_count).unwrap_or(i16::MAX);
    for quorum_index in 0..quorum_count {
        let family = MinedFamily::Rotated {
            quorum_index,
        };
        let rows = src
            .mined()
            .mined_index(p.llmq_type, family, tip, chain.header(block).height, 1)
            .map_err(QrInfoBuildError::Source)?;
        if let Some(row) = rows.first() {
            let base = base_through::<S::Error>(chain, block, row.base_height)
                .map_err(QrInfoBuildError::Source)?;
            last_mined.push(base);
        }
    }

    let mut needed = BTreeSet::new();
    for base in last_mined {
        let key = (p.llmq_type.to_base(), *hash(base).as_bytes());
        let commitment = src
            .commitment(&key)
            .map_err(QrInfoBuildError::Source)?
            .ok_or(QrInfoBuildError::<S::Error>::NoMinedCommitment(key))?;
        out.commitments.push(commitment);
        let base_height = i64::from(chain.header(base).height);
        let cycle_start = base_height - base_height % cycle_length;
        needed.insert(cycle_start - cycle_length);
        needed.insert(cycle_start - 2 * cycle_length);
        needed.insert(cycle_start - 3 * cycle_length);
    }
    for (cycle, _) in &targets {
        needed.remove(&i64::from(chain.header(*cycle).height));
    }

    for height in needed {
        let (_, work, snapshot) = construct_cycle(src, p, legacy, height, tip)?;
        out.list_snapshots.push(snapshot);
        out.list_diffs.push(diff(base_for(&bases, work), work)?);
        if !legacy {
            insert_sorted(&mut bases, base_of(work));
        }
    }

    if !legacy {
        for (_, work) in targets.iter().rev() {
            out.cycle_diffs.push(diff(base_for(&bases, *work), *work)?);
            insert_sorted(&mut bases, base_of(*work));
        }
        out.diff_h = Some(diff(base_for(&bases, work_h), work_h)?);
        insert_sorted(&mut bases, base_of(work_h));
        out.diff_tip = Some(diff(base_for(&bases, tip), tip)?);
    }
    Ok(())
}

// Core: v24.0.0 src/llmq/snapshot.cpp:26-32 CheckBlockDataAvailable
fn check_data<S: QrInfoSource>(src: &S, id: BlockId) -> BuildResult<(), S::Error> {
    if src.have_data(id) {
        return Ok(());
    }
    Err(QrInfoBuildError::BlockDataUnavailable(src.mined().chain().header(id).hash))
}

/// `ConstructCycle` up to its snapshot read (`skip_snap = true`): the cycle block, `from`'s
/// ancestor at `height`, and its work block.
// Core: v24.0.0 src/llmq/snapshot.cpp:35-55 ConstructCycle
fn cycle_blocks<S: QrInfoSource>(
    src: &S,
    need_block_data: bool,
    height: i64,
    from: BlockId,
) -> BuildResult<(BlockId, BlockId), S::Error> {
    let chain = src.mined().chain();
    let ancestor = |id: BlockId, h: i64| u32::try_from(h).ok().and_then(|h| chain.ancestor(id, h));
    let cycle = ancestor(from, height).ok_or(QrInfoBuildError::<S::Error>::CannotFindBlock)?;
    let work_height = i64::from(chain.header(cycle).height) - i64::from(WORK_DIFF_DEPTH);
    let work =
        ancestor(cycle, work_height).ok_or(QrInfoBuildError::<S::Error>::CannotFindWorkBlock)?;
    if need_block_data {
        check_data(src, work)?;
    }
    Ok((cycle, work))
}

/// `ConstructCycle` with `skip_snap = false`: the cycle block, its work block and its snapshot.
// Core: v24.0.0 src/llmq/snapshot.cpp:35-65 ConstructCycle
fn construct_cycle<S: QrInfoSource>(
    src: &S,
    p: &QrInfoParams<'_>,
    need_block_data: bool,
    height: i64,
    from: BlockId,
) -> BuildResult<(BlockId, BlockId, Arc<QuorumSnapshot>), S::Error> {
    let (cycle, work) = cycle_blocks(src, need_block_data, height, from)?;
    let cycle_hash = src.mined().chain().header(cycle).hash;
    let snapshot = src
        .quorum_snapshot(p.llmq_type, &cycle_hash)
        .map_err(QrInfoBuildError::Source)?
        .ok_or(QrInfoBuildError::<S::Error>::NoSnapshot)?;
    Ok((cycle, work, snapshot))
}

// Core: v24.0.0 src/llmq/snapshot.cpp:68-73 InsertBaseBlockSorted
fn insert_sorted(bases: &mut Vec<Base>, base: Base) {
    let pos = bases.partition_point(|b| b.0 <= base.0);
    bases.insert(pos, base);
}

/// The highest base at or below `target`, or the genesis hash.
// Core: v24.0.0 src/llmq/snapshot.cpp:259-265 GetLastBaseBlockHash
fn last_base(
    chain: &dyn dash_consensus::ChainAncestry,
    bases: &[Base],
    target: BlockId,
    p: &QrInfoParams<'_>,
) -> BlockHash {
    let height = chain.header(target).height;
    match bases.partition_point(|b| b.0 <= height) {
        0 => p.genesis_hash,
        pos => chain.header(bases[pos - 1].1).hash,
    }
}
