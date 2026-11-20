//! The node's `MnListDiffSource`: Core's `BuildSimplifiedMNListDiff` under `cs_main`, in two
//! halves. [`capture`] runs in one `OnV` turn: the request checks, both lists, the quorum half
//! and the positions of every block the reply reads. [`build`] then reads only those positions
//! through a caller's [`DiffBlockFiles`] and builds the diff, so a reorg after the turn cannot
//! mix views. `protx diff` passes the RPC slot's block files; P2P `getmnlistd` passes its own.
//! Both halves speak dash-dml types and errors only.
// Core: evo/smldiff.cpp:154-247 BuildSimplifiedMNListDiff (v24.0.0); rpc/evo.cpp:1876-1918

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use dash_chain::FlatFilePos;
use dash_chain::{EntryView, Status};
use dash_consensus::ctx::{Deployment, DeploymentView as _};
use dash_consensus::primitives::{CoreSerialize as _, decode_tx_exact};
use dash_consensus::{Block, BlockId, BlsCtx, ChainAncestry as _};
use dash_dml::blockprocessor::QcCtx;
use dash_dml::mnlistdiff::{
    BlockStatus, BlsSignatureBytes, MnListDiffError, MnListDiffRequest, MnListDiffSource,
    QuorumBase, QuorumKey, SimplifiedMnListDiff, TargetBlock, build_mnlistdiff_rpc, check_request,
};
use dash_dml::{Dmn, MnList, StoreError};
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::llmq::{MemberError, MinedFamily, base_through, is_quorum_rotation_enabled};
use dash_params::{BuriedDeployment, LlmqType};
use dash_primitives::{BlockHash, Commitment, QuorumHash, SpecialPayload};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::{BlockFiles, ChainQuery, OnV, OnVMembersError};
use dashd_rpc::evo::{BlockArg, BlockFailure, EvoError, SmlDiffFacts, SmlEntryFacts};
use dashd_rpc::protocol::RpcError;

use super::{ChainSlot, block_hash};

/// `ReadBlockFromDisk` at a position the V turn captured, checked against the block's hash.
pub(crate) trait DiffBlockFiles {
    /// The block, or `None` for any failure (Core's read returns false for every one).
    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Option<Arc<Block>>;
}

impl DiffBlockFiles for Mutex<Box<dyn BlockFiles>> {
    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Option<Arc<Block>> {
        let read = self.lock().unwrap_or_else(|p| p.into_inner()).read_block(pos, hash);
        read.map_err(|e| tracing::error!("{e}")).ok()
    }
}

/// No block files: every read fails.
struct NoFiles;

impl DiffBlockFiles for NoFiles {
    fn read_block(&self, _: FlatFilePos, _: &BlockHash) -> Option<Arc<Block>> {
        None
    }
}

/// A block the reply reads, as the V turn saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CapturedBlock {
    pub(crate) id: BlockId,
    pub(crate) hash: BlockHash,
    pub(crate) height: u32,
    /// Its data position; `None` without `BLOCK_HAVE_DATA`.
    pub(crate) pos: Option<FlatFilePos>,
}

impl CapturedBlock {
    fn of(e: &EntryView) -> Self {
        let pos = e.file.filter(|_| e.status.has(Status::HAVE_DATA)).map(|file| FlatFilePos {
            file,
            pos: e.data_pos,
        });
        CapturedBlock {
            id: e.id,
            hash: e.hash,
            height: e.height,
            pos,
        }
    }
}

/// A new quorum's work block: the target's ancestor whose coinbase ChainLock the quorum's
/// members were selected under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorkBlock {
    pub(crate) block: CapturedBlock,
    /// `DeploymentActiveAt(work, V20)`.
    pub(crate) v20: bool,
}

/// The quorum half's reads in the builder's order, each kept as it answered; reading stops at
/// the first answer the builder fails on, as the builder does.
#[derive(Debug, Default)]
pub(crate) struct QuorumReads {
    /// `GetMinedAndActiveCommitmentsUntilBlock` at the base, then at the target.
    pub(crate) active: Vec<Result<BTreeSet<QuorumKey>, StoreError>>,
    /// `GetMinedCommitment` of each quorum new at the target, serialized.
    pub(crate) mined: BTreeMap<QuorumKey, Result<Option<Vec<u8>>, StoreError>>,
    /// `GetQuorum` of each new quorum, from V20.
    pub(crate) bases: BTreeMap<QuorumKey, Result<Option<QuorumBase>, StoreError>>,
    /// Each distinct work height's block, `None` below genesis (`GetAncestor`'s nullptr).
    pub(crate) work: BTreeMap<i32, Option<WorkBlock>>,
}

/// What one `OnV` turn read for a diff: everything but block data.
#[derive(Debug)]
pub(crate) struct DiffCapture {
    /// The base as requested (the null hash asks from genesis), echoed in the reply.
    pub(crate) requested_base: BlockHash,
    pub(crate) base_block: BlockHash,
    pub(crate) base_list: Arc<MnList>,
    pub(crate) list: Arc<MnList>,
    /// `DeploymentActiveAfter(target, V20)`.
    pub(crate) v20_active_after: bool,
    pub(crate) target: CapturedBlock,
    /// Core's process flag, under which a coinbase ChainLock is read and serialized.
    pub(crate) bls: BlsCtx,
    pub(crate) quorums: QuorumReads,
}

/// The V-side half: in one `OnV` turn, `BuildSimplifiedMNListDiff` up to its block reads.
/// The block index lookups (a null base is genesis) and [`check_request`], both lists, then the
/// quorum half and the positions of the target block and of each new quorum's work block.
///
/// # Errors
///
/// The builder's refusals before its reads, and list or quorum storage faults, which
/// [`build`] reports in Core's order.
// Core: evo/smldiff.cpp:159-227, :76-93
pub(crate) fn capture(
    on_v: &OnV<'_>,
    q: &ChainQuery<'_>,
    requested_base: BlockHash,
    block: BlockHash,
) -> Result<DiffCapture, MnListDiffError<StoreError>> {
    let (base, target) = resolve(q, requested_base, block)?;

    // Core: evo/smldiff.cpp:195-207, :221
    let view = q.evo_view();
    let base_list = view.list_for_block(base.id).map_err(list_error)?;
    let list = view.list_for_block(target.id).map_err(list_error)?;
    let v20 = Deployment::Buried(BuriedDeployment::V20);
    let v20_active_after = q.deployments.active_after(Some(target.id), v20);
    let target = CapturedBlock::of(&target);

    let tip = view.tip.ok_or(MnListDiffError::Source(StoreError::NotBuilt("a chain tip")))?;
    let mut quorums = view
        .with_qc(q.bls(), |qc| commitment_reads(qc, base.id, target.id, tip))
        .map_err(|e| MnListDiffError::Source(store_error(e)))?;
    if v20_active_after && quorums.mined.values().all(|m| matches!(m, Ok(Some(_)))) {
        quorum_reads(on_v, q, target.id, &mut quorums);
    }
    Ok(DiffCapture {
        requested_base,
        base_block: base.hash,
        base_list,
        list,
        v20_active_after,
        target,
        bls: q.bls(),
        quorums,
    })
}

/// The lookups and [`check_request`]: the base and target entries, both known, in the active
/// chain, in order, and the target's data on disk.
// Core: evo/smldiff.cpp:159-193
fn resolve(
    q: &ChainQuery<'_>,
    requested_base: BlockHash,
    block: BlockHash,
) -> Result<(EntryView, EntryView), MnListDiffError<StoreError>> {
    let reader = q.chain.reader();
    let base = if requested_base == BlockHash::from([0; 32]) {
        q.chain.ancestor_private(0)
    } else {
        reader.lookup(&requested_base)
    }
    .and_then(|id| reader.entry(id));
    let target = reader.lookup(&block).and_then(|id| reader.entry(id));
    let status = |e: &EntryView| BlockStatus {
        height: e.height as i32,
        in_active_chain: q.chain.active_contains_private(e.id),
        have_data: e.status.has(Status::HAVE_DATA),
    };
    check_request(&requested_base, base.as_ref().map(status), &block, target.as_ref().map(status))?;
    base.zip(target).ok_or(MnListDiffError::UnknownBlock(block))
}

/// `BuildQuorumsDiff`'s reads: both active sets, then the commitment of each new quorum.
// Core: evo/smldiff.cpp:31-65
fn commitment_reads(qc: &QcCtx<'_>, base: BlockId, target: BlockId, tip: BlockId) -> QuorumReads {
    commitment_reads_with(
        [base, target],
        |at| active_quorums(qc, at),
        |key| {
            let llmq_type = LlmqType::from_base(key.0);
            let hash = QuorumHash::from(key.1);
            qc.mined.mined_commitment(llmq_type, &hash, tip).map(|found| {
                found.map(|(qc, _)| {
                    let mut bytes = Vec::new();
                    qc.encode(&mut bytes);
                    bytes
                })
            })
        },
    )
}

/// [`commitment_reads`] over its two readers: each answer is recorded, and reading stops after
/// the first one the builder fails on.
fn commitment_reads_with(
    at: [BlockId; 2],
    mut active: impl FnMut(BlockId) -> Result<BTreeSet<QuorumKey>, StoreError>,
    mut mined: impl FnMut(&QuorumKey) -> Result<Option<Vec<u8>>, StoreError>,
) -> QuorumReads {
    let mut reads = QuorumReads::default();
    for at in at {
        let set = active(at);
        let failed = set.is_err();
        reads.active.push(set);
        if failed {
            return reads;
        }
    }
    let [Ok(from), Ok(to)] = reads.active.as_slice() else {
        return reads;
    };
    for key in to.difference(from) {
        let answer = mined(key);
        let stop = !matches!(answer, Ok(Some(_)));
        reads.mined.insert(*key, answer);
        if stop {
            break;
        }
    }
    reads
}

/// `GetMinedAndActiveCommitmentsUntilBlock(at)` as `(type, quorumHash)` pairs: for every
/// configured type, the newest record of each quorum index when rotation is enabled at `at`,
/// else the newest `signingActiveQuorumCount` records.
// Core: llmq/blockprocessor.cpp:817-832
fn active_quorums(qc: &QcCtx<'_>, at: BlockId) -> Result<BTreeSet<QuorumKey>, StoreError> {
    let (llmq, mined) = (&qc.llmq, qc.mined);
    let chain = mined.chain();
    let height = chain.header(at).height;
    let mut out = BTreeSet::new();
    for (llmq_type, params) in llmq.params().consensus.llmq_params() {
        let count = usize::from(params.signing_active_quorum_count);
        let rows = if is_quorum_rotation_enabled(llmq, params, at).map_err(member_error)? {
            let mut rows = Vec::with_capacity(count);
            for index in 0..count {
                let family = MinedFamily::Rotated {
                    quorum_index: i16::try_from(index).unwrap_or(i16::MAX),
                };
                rows.extend(
                    mined.mined_index(llmq_type, family, at, height, 1)?.into_iter().take(1),
                );
            }
            rows
        } else {
            mined.mined_index(llmq_type, MinedFamily::NonRotated, at, height, count)?
        };
        for row in rows {
            let base = base_through::<StoreError>(chain, at, row.base_height)?;
            out.insert((llmq_type.to_base(), *chain.header(base).hash.as_bytes()));
        }
    }
    Ok(out)
}

/// `BuildQuorumChainlockInfo`'s reads: each new quorum's object, then its work block, the
/// target's ancestor at `baseHeight − quorumIndex − 8`, captured once per height.
// Core: evo/smldiff.cpp:69-93
fn quorum_reads(on_v: &OnV<'_>, q: &ChainQuery<'_>, target: BlockId, reads: &mut QuorumReads) {
    let binding = q.bind(on_v);
    let quorum = |key: &QuorumKey| {
        binding
            .quorum(LlmqType::from_base(key.0), &QuorumHash::from(key.1))
            .map_err(store_error)
            .map(|quorum| {
                quorum.map(|quorum| QuorumBase {
                    base_height: q.chain.header(quorum.base).height as i32,
                    quorum_index: quorum.commitment.quorum_index().unwrap_or(0),
                })
            })
    };
    let work = |height: u32| {
        let id = q.chain.ancestor(target, height)?;
        let entry = q.chain.reader().entry(id)?;
        Some(WorkBlock {
            block: CapturedBlock::of(&entry),
            v20: q.deployments.active_at(id, Deployment::Buried(BuriedDeployment::V20)),
        })
    };
    quorum_reads_with(reads, quorum, work);
}

/// [`quorum_reads`] over its two readers: each quorum answer is recorded, reading stops after
/// the first one the builder fails on, and each distinct work height is read once; one below
/// genesis is `None` unread.
fn quorum_reads_with(
    reads: &mut QuorumReads,
    mut quorum: impl FnMut(&QuorumKey) -> Result<Option<QuorumBase>, StoreError>,
    mut work: impl FnMut(u32) -> Option<WorkBlock>,
) {
    let keys: Vec<QuorumKey> = reads.mined.keys().copied().collect();
    for key in keys {
        let answer = quorum(&key);
        let found = answer.as_ref().ok().copied().flatten();
        reads.bases.insert(key, answer);
        let Some(base) = found else {
            return;
        };
        let height = base.base_height - i32::from(base.quorum_index) - 8;
        reads.work.entry(height).or_insert_with(|| u32::try_from(height).ok().and_then(&mut work));
    }
}

/// A list read's failure: Core's `BlockDataUnavailableError`, or the node's own storage fault.
fn list_error(e: StoreError) -> MnListDiffError<StoreError> {
    match e {
        StoreError::ListUnavailable(hash) => MnListDiffError::ListUnavailable(hash),
        e => MnListDiffError::Source(e),
    }
}

fn member_error(e: MemberError<StoreError>) -> StoreError {
    match e {
        MemberError::View(e) => e,
        e => StoreError::read(e.to_string()),
    }
}

fn store_error(e: OnVMembersError) -> StoreError {
    match e {
        OnVMembersError::Members(e) => member_error(e),
        e => StoreError::read(e.to_string()),
    }
}

/// The file-read half's source: the capture's answers, and block data read only at the
/// captured positions.
pub(crate) struct CapturedSource<'a, F: ?Sized> {
    capture: &'a DiffCapture,
    files: &'a F,
    active_served: Cell<usize>,
}

/// A read the V turn did not make: the builder asked out of the capture's order.
fn not_captured() -> StoreError {
    StoreError::inconsistent("a diff read outside its V capture")
}

impl<F: DiffBlockFiles + ?Sized> MnListDiffSource for CapturedSource<'_, F> {
    type Error = StoreError;

    fn active_quorums(&self, _: &BlockHash) -> Result<BTreeSet<QuorumKey>, StoreError> {
        let served = self.active_served.get();
        self.active_served.set(served + 1);
        self.capture.quorums.active.get(served).cloned().unwrap_or_else(|| Err(not_captured()))
    }

    fn mined_commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, StoreError> {
        self.capture.quorums.mined.get(quorum).cloned().unwrap_or_else(|| Err(not_captured()))
    }

    fn quorum(&self, quorum: &QuorumKey) -> Result<Option<QuorumBase>, StoreError> {
        self.capture.quorums.bases.get(quorum).cloned().unwrap_or_else(|| Err(not_captured()))
    }

    /// `GetNonNullCoinbaseChainlock(work)`: none below genesis, before V20 at the work block,
    /// or when its data cannot be read.
    // Core: evo/cbtx.cpp:181-198
    fn coinbase_chainlock(
        &self,
        _: &BlockHash,
        height: i32,
    ) -> Result<Option<BlsSignatureBytes>, StoreError> {
        let work = self.capture.quorums.work.get(&height).ok_or_else(not_captured)?;
        let Some(work) = work.filter(|w| w.v20) else {
            return Ok(None);
        };
        let block = work.block.pos.and_then(|pos| self.files.read_block(pos, &work.block.hash));
        let facts = CoinbaseFacts::of(block.as_deref());
        let bls = self.capture.bls;
        Ok(nonnull_coinbase_chainlock(&facts, work.block.height, true, bls)
            .and_then(|(sig, _)| Vec::from(sig.to_bytes(bls.scheme())).try_into().ok()))
    }

    // Core: evo/smldiff.cpp:228-244
    fn target_block(&self, block: &BlockHash) -> Option<TargetBlock> {
        let target = &self.capture.target;
        let read = target.pos.and_then(|pos| self.files.read_block(pos, block))?;
        let coinbase = read.txs().first()?;
        Some(TargetBlock {
            coinbase: coinbase.raw().core_serialize(),
            txids: read.txs().iter().map(|tx| *tx.txid().as_bytes()).collect(),
        })
    }
}

/// The file-read half: the diff from `capture`, reading block data only at its positions.
///
/// # Errors
///
/// The builder's, in Core's order.
// Core: evo/smldiff.cpp:208-244
pub(crate) fn build<F: DiffBlockFiles + ?Sized>(
    capture: &DiffCapture,
    files: &F,
    extended: bool,
) -> Result<(SimplifiedMnListDiff, Vec<Arc<Dmn>>), MnListDiffError<StoreError>> {
    let request = MnListDiffRequest {
        requested_base: capture.requested_base,
        base_block: capture.base_block,
        base_list: &capture.base_list,
        block: capture.target.hash,
        list: &capture.list,
        v20_active_after: capture.v20_active_after,
    };
    let source = CapturedSource {
        capture,
        files,
        active_served: Cell::new(0),
    };
    build_mnlistdiff_rpc(&request, &source, extended)
}

/// `protx diff`'s adapter: ParseBlock's range checks for both arguments, then `extended`, then
/// the builder in one `OnV` turn; the block reads after it from the slot's block files.
/// `protx list`/`listdiff` (P1) must answer in this order too when their `OnV` turn is dropped.
// Core: rpc/evo.cpp:1898-1915
pub(super) fn diff(
    slot: &ChainSlot,
    base: BlockArg,
    block: BlockArg,
    extended: Result<bool, RpcError>,
) -> Result<SmlDiffFacts, EvoError> {
    let writer = slot.slot().clone().ok_or(EvoError::Unavailable)?;
    let args = (base.clone(), block.clone(), extended.clone());
    let captured = writer
        .chain
        .on_v(move |on_v, q| {
            let (base_hash, block_hash, extended) = arguments(q, &base, &block, extended)?;
            let capture = capture(on_v, q, base_hash, block_hash)
                .map_err(|e| request_error(e, base_hash, &base, &block))?;
            Ok::<_, EvoError>((capture, extended))
        })
        .recv();
    let Ok(captured) = captured else {
        // A V without evo state drops the OnV turn (D77): its arguments still answer first.
        let (base, block, extended) = args;
        let checked = writer.chain.query(move |q| {
            let (base_hash, block_hash, _) = arguments(q, &base, &block, extended)?;
            resolve(q, base_hash, block_hash)
                .map_err(|e| request_error(e, base_hash, &base, &block))
        });
        checked.recv().map_err(|_| EvoError::Unavailable)??;
        return Err(EvoError::Unavailable);
    };
    let (capture, extended) = captured?;
    let built = match writer.files.as_deref() {
        Some(files) => build(&capture, files, extended),
        None => build(&capture, &NoFiles, extended),
    };
    let (diff, dmns) = built.map_err(diff_error)?;
    facts(&capture, diff, dmns)
}

/// `ParseBlock` for both arguments, then `ParseBoolV(extended)`.
// Core: rpc/evo.cpp:1899-1903
fn arguments(
    q: &ChainQuery<'_>,
    base: &BlockArg,
    block: &BlockArg,
    extended: Result<bool, RpcError>,
) -> Result<(BlockHash, BlockHash, bool), EvoError> {
    let base_hash = block_hash(q, base)?;
    let block_hash = block_hash(q, block)?;
    Ok((base_hash, block_hash, extended.map_err(EvoError::Param)?))
}

/// An unknown block names the argument it came from.
fn request_error(
    e: MnListDiffError<StoreError>,
    base_hash: BlockHash,
    base: &BlockArg,
    block: &BlockArg,
) -> EvoError {
    match e {
        MnListDiffError::UnknownBlock(hash) if hash == base_hash => {
            base.failure(BlockFailure::NotFound(hash))
        }
        MnListDiffError::UnknownBlock(hash) => block.failure(BlockFailure::NotFound(hash)),
        e => diff_error(e),
    }
}

fn diff_error(e: MnListDiffError<StoreError>) -> EvoError {
    match e {
        MnListDiffError::Source(e) => e.into(),
        e @ MnListDiffError::ListUnavailable(_) => EvoError::ListUnavailable(e.to_string()),
        e => EvoError::Failed(e.to_string()),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What `CSimplifiedMNListDiff::ToJson` prints of the built diff: the commitments decoded under
/// their versions' schemes, and the roots of the coinbase's CbTx payload when it reads whole.
// Core: evo/core_write.cpp:316-376
fn facts(
    capture: &DiffCapture,
    diff: SimplifiedMnListDiff,
    dmns: Vec<Arc<Dmn>>,
) -> Result<SmlDiffFacts, EvoError> {
    let mut tree = Vec::new();
    diff.cb_tx_merkle_tree.encode(&mut tree);
    let new_quorums = diff
        .new_quorums
        .iter()
        .map(|bytes| {
            let mut input = bytes.as_slice();
            Commitment::decode(&mut input)
                .ok()
                .filter(|_| input.is_empty())
                .ok_or_else(|| EvoError::Store("a mined commitment does not decode".into()))
        })
        .collect::<Result<_, _>>()?;
    Ok(SmlDiffFacts {
        base_hash_echo: diff.base_block_hash,
        block_hash: diff.block_hash,
        cbtx_hex: hex(&diff.cb_tx),
        merkle_tree_hex: hex(&tree),
        roots: cbtx_roots(&diff.cb_tx),
        deleted: diff.deleted_mns,
        entries: diff
            .mn_list
            .into_iter()
            .zip(dmns)
            .map(|(entry, dmn)| SmlEntryFacts {
                entry,
                dmn,
            })
            .collect(),
        deleted_quorums: diff.deleted_quorums,
        new_quorums,
        cl_sigs: diff.quorums_cl_sigs,
        bls: capture.bls,
    })
}

/// `GetTxPayload<CCbTx>(cbTx, false)`'s roots: `merkleRootQuorums` from version 2.
// Core: evo/core_write.cpp:358-363; evo/specialtx.h:17-35
fn cbtx_roots<H: From<[u8; 32]>>(cb_tx: &[u8]) -> Option<(H, Option<H>)> {
    let tx = decode_tx_exact(cb_tx).ok()?.into_value();
    let decoded = tx.payload()?.ok()?;
    let SpecialPayload::CoinbaseCommitment(cbtx) = decoded.payload() else {
        return None;
    };
    if !decoded.is_complete() {
        return None;
    }
    let root = |r: &dyn AsRef<[u8; 32]>| H::from(*r.as_ref());
    Some((root(&cbtx.merkle_root_mn_list()), cbtx.merkle_root_quorums().as_ref().map(|r| root(r))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_consensus::Tx;
    use dash_consensus::primitives::Header;
    use dash_dml::ProTxHash;
    use dash_dml::mnlistdiff::{build_mnlistdiff, write_sml_entry};
    use dash_dml::testkit::numbered;
    use dash_primitives::BlockHeader;
    use std::cell::RefCell;

    /// The testnet coinbase at 1,569,000: a version-3 CbTx with a valid best ChainLock under the
    /// basic scheme, difference 0. Its payload is version(2) height(4) roots(64) diff(1) sig(96)
    /// balance(8).
    const COINBASE: &str = "03000500010000000000000000000000000000000000000000000000000000000000000000ffffffff1903e8f0172f5032506f6f6c2d74444153482f6332706f6f6c2fffffffff05ba578c03000000001976a9140a7bf49abc7d07ade1826202b39a2b05a6d413ea88ac2ed5fd0300000000016a0132a706000000001976a91464f2b2b84f62d68a2cd7f7f5fb2b5aa75ef716d788ac01000000000000001976a91420cb5c22b1e4d5947e5c112c7696b51ad9af3c6188ac00000000000000002a6a280000000000000000000000000000000000000000000000000000000000000000000000000000000000000000af0300e8f0170025fb1b7b552062944faf4459f21bb5d9eae59060a60823a5107dc1f22a2cd697d6138f64f0bc8c9280379e0dd5d5f9b9d0a58dc9100960254c939edec7526af700a624ae0c90fbebc4a320804621d71119f6acce534c97a356558a0f390a8e9fd3d87e416f9941914f9a6381056185d9f716316911f9a0531f746d1e37700b013abc881cf1fa508b090890600afb60e4fd51e14f6a6a6b994804c9d733b0910ad4c0fee884ec210000";

    fn unhex(hex: &str) -> Vec<u8> {
        (0..hex.trim().len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// A block whose coinbase is the fixture's CbTx at `height`.
    fn block_at(height: i32) -> Arc<Block> {
        let raw = decode_tx_exact(&unhex(COINBASE)).expect("the fixture decodes").into_value();
        let raw = raw.to_mutable();
        let mut payload = raw.extra_payload().clone();
        payload[2..6].copy_from_slice(&height.to_le_bytes());
        let raw = dash_primitives::Transaction::new(
            raw.version(),
            raw.tx_type(),
            raw.inputs().clone(),
            raw.outputs().clone(),
            raw.lock_time(),
            payload,
        );
        let coinbase = Arc::new(Tx::new(raw));
        Arc::new(Block::new(Header::new(BlockHeader::default()), vec![coinbase]))
    }

    /// The bare v3 commitment fixture `build_mnlistdiff` serializes.
    fn commitment() -> Vec<u8> {
        unhex(include_str!("../../../../dashd-rpc/tests/data/evo/bare-commitment-v3.hex"))
    }

    /// Block files holding blocks by file number; each read is logged.
    #[derive(Default)]
    struct Files {
        blocks: BTreeMap<i32, Arc<Block>>,
        reads: RefCell<Vec<(i32, BlockHash)>>,
    }

    impl DiffBlockFiles for Files {
        fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Option<Arc<Block>> {
            self.reads.borrow_mut().push((pos.file, *hash));
            self.blocks.get(&pos.file).cloned()
        }
    }

    fn at(file: i32, height: u32) -> CapturedBlock {
        CapturedBlock {
            id: BlockId(height),
            hash: BlockHash::from([u8::try_from(height).expect("small"); 32]),
            height,
            pos: Some(FlatFilePos {
                file,
                pos: 8,
            }),
        }
    }

    const TARGET: u32 = 60;

    fn capture(quorums: QuorumReads, v20: bool) -> DiffCapture {
        DiffCapture {
            requested_base: BlockHash::from([0; 32]),
            base_block: BlockHash::from([1; 32]),
            base_list: Arc::new(MnList::default_pre_dip3()),
            list: Arc::new(MnList::default_pre_dip3()),
            v20_active_after: v20,
            target: at(0, TARGET),
            bls: BlsCtx::BASIC,
            quorums,
        }
    }

    const Q1: QuorumKey = (100, [0x11; 32]);
    const Q2: QuorumKey = (100, [0x22; 32]);
    const Q3: QuorumKey = (100, [0x33; 32]);

    fn base(base_height: i32) -> Result<Option<QuorumBase>, StoreError> {
        Ok(Some(QuorumBase {
            base_height,
            quorum_index: 0,
        }))
    }

    /// Three new quorums, two on the work block at 22 and one on 32.
    fn three_quorums() -> QuorumReads {
        QuorumReads {
            active: vec![Ok(BTreeSet::new()), Ok([Q1, Q2, Q3].into())],
            mined: [Q1, Q2, Q3].map(|q| (q, Ok(Some(commitment())))).into(),
            bases: [(Q1, base(30)), (Q2, base(30)), (Q3, base(40))].into(),
            work: [
                (
                    22,
                    Some(WorkBlock {
                        block: at(1, 22),
                        v20: true,
                    }),
                ),
                (
                    32,
                    Some(WorkBlock {
                        block: at(2, 32),
                        v20: true,
                    }),
                ),
            ]
            .into(),
        }
    }

    fn files(work: &[i32]) -> Files {
        let mut blocks: BTreeMap<i32, Arc<Block>> = [(0, block_at(TARGET as i32))].into();
        for &file in work {
            let height = if file == 1 {
                22
            } else {
                32
            };
            blocks.insert(file, block_at(height));
        }
        Files {
            blocks,
            ..Files::default()
        }
    }

    fn signature(height: i32) -> BlsSignatureBytes {
        let facts = CoinbaseFacts::of(Some(&block_at(height)));
        let (sig, _) = nonnull_coinbase_chainlock(&facts, height as u32, true, BlsCtx::BASIC)
            .expect("the fixture carries a ChainLock");
        Vec::from(sig.to_bytes(BlsCtx::BASIC.scheme())).try_into().unwrap()
    }

    /// P3 R3-1: each distinct work block is read once at its captured position, the target
    /// last. Both work blocks carry the fixture's one signature, so their groups merge.
    // Core: evo/smldiff.cpp:95-116
    #[test]
    fn each_captured_work_block_is_read_once_at_its_position() {
        let capture = capture(three_quorums(), true);
        let files = files(&[1, 2]);
        let (diff, _) = build(&capture, &files, false).unwrap();
        assert_eq!(
            *files.reads.borrow(),
            [(1, at(1, 22).hash), (2, at(2, 32).hash), (0, at(0, TARGET).hash)]
        );
        assert_eq!(signature(22), signature(32));
        assert_eq!(diff.quorums_cl_sigs, [(signature(22), vec![0, 1, 2])]);
    }

    /// A failed work-block read is that work block's null signature only; a work block before
    /// V20 or below genesis reads nothing and is null too.
    // Core: evo/cbtx.cpp:181-198
    #[test]
    fn a_failed_work_block_read_nulls_only_its_quorums() {
        let captured = capture(three_quorums(), true);
        let one = files(&[1]);
        let (diff, _) = build(&captured, &one, false).unwrap();
        let mut sigs = diff.quorums_cl_sigs.clone();
        sigs.sort();
        let mut want = vec![(signature(22), vec![0, 1]), ([0; 96], vec![2])];
        want.sort();
        assert_eq!(sigs, want);

        let mut reads = three_quorums();
        reads.work.get_mut(&22).unwrap().as_mut().unwrap().v20 = false;
        *reads.work.get_mut(&32).unwrap() = None;
        let both = files(&[1, 2]);
        let (diff, _) = build(&capture(reads, true), &both, false).unwrap();
        assert_eq!(diff.quorums_cl_sigs, [([0; 96], vec![0, 1, 2])]);
        assert_eq!(*both.reads.borrow(), [(0, at(0, TARGET).hash)], "only the target");
    }

    /// The source answers only from the capture: a second build reads the same captured
    /// positions and gives the same diff, and a read the turn did not make is a storage fault,
    /// not a guess.
    #[test]
    fn the_source_reads_only_the_capture() {
        let files = files(&[1, 2]);
        let captured = capture(three_quorums(), true);
        let first = build(&captured, &files, false).unwrap().0;
        files.reads.borrow_mut().clear();
        assert_eq!(build(&captured, &files, false).unwrap().0, first);
        assert!(files.reads.borrow().iter().all(|(file, _)| [0, 1, 2].contains(file)));

        let mut reads = three_quorums();
        reads.work.clear();
        let error = build(&capture(reads, true), &files, false).unwrap_err();
        assert!(matches!(error, MnListDiffError::Source(StoreError::Inconsistent { .. })));
        assert_eq!(diff_error(error), EvoError::Store(not_captured().to_string()));
    }

    /// The quorum half's refusals and faults, in Core's order, each before any block read.
    // Core: evo/smldiff.cpp:58-61, :79-86, :216-225
    #[test]
    fn quorum_refusals_and_faults_come_before_the_reads() {
        let missing_commitment = || {
            let mut reads = three_quorums();
            reads.mined.insert(Q1, Ok(None));
            reads
        };
        let missing_object = || {
            let mut reads = three_quorums();
            reads.bases.insert(Q2, Ok(None));
            reads
        };
        let store = |e: &str| StoreError::read(e.to_owned());
        let failed_active = || {
            let mut reads = three_quorums();
            reads.active = vec![Err(store("active"))];
            reads
        };
        let failed_mined = || {
            let mut reads = three_quorums();
            reads.mined.insert(Q1, Err(store("mined")));
            reads
        };
        let failed_object = || {
            let mut reads = three_quorums();
            reads.bases.insert(Q1, Err(store("object")));
            reads
        };
        for (reads, want) in [
            (missing_commitment(), EvoError::Failed("failed to build quorums diff".into())),
            (missing_object(), EvoError::Failed("failed to build quorum chainlock info".into())),
            (failed_active(), EvoError::Store(store("active").to_string())),
            (failed_mined(), EvoError::Store(store("mined").to_string())),
            (failed_object(), EvoError::Store(store("object").to_string())),
        ] {
            let files = files(&[1, 2]);
            let error = build(&capture(reads, true), &files, false).unwrap_err();
            assert_eq!(diff_error(error), want);
            assert!(files.reads.borrow().is_empty(), "no block is read: {want:?}");
        }

        // Before V20 the quorum object and the work blocks are not read.
        let mut reads = missing_object();
        reads.work.clear();
        let files = files(&[]);
        let (diff, _) = build(&capture(reads, false), &files, false).unwrap();
        assert!(diff.quorums_cl_sigs.is_empty());
        assert_eq!(diff.new_quorums.len(), 3);
        assert_eq!(*files.reads.borrow(), [(0, at(0, TARGET).hash)]);
    }

    /// The target's data: missing from the index at the check, unreadable at the read.
    // Core: evo/smldiff.cpp:186-193, :228-233
    #[test]
    fn target_data_missing_or_unreadable() {
        let hash = at(0, TARGET).hash;
        let read_failed = EvoError::Failed(format!("failed to read block {hash} from disk"));
        let mut missing = capture(three_quorums(), true);
        missing.target.pos = None;
        let error = build(&missing, &files(&[1, 2]), false).map_err(diff_error).unwrap_err();
        assert_eq!(error, read_failed);
        let mut unreadable = files(&[1, 2]);
        unreadable.blocks.remove(&0);
        let capture = capture(three_quorums(), true);
        let error = build(&capture, &unreadable, false).map_err(diff_error).unwrap_err();
        assert_eq!(error, read_failed);
        assert_eq!(
            build(&capture, &NoFiles, false).map_err(diff_error).unwrap_err(),
            EvoError::Failed(format!("failed to read block {hash} from disk"))
        );
        let unavailable = check_request::<StoreError>(
            &hash,
            Some(BlockStatus {
                height: 1,
                in_active_chain: true,
                have_data: true,
            }),
            &hash,
            Some(BlockStatus {
                height: 1,
                in_active_chain: true,
                have_data: false,
            }),
        );
        assert_eq!(
            unavailable.map_err(diff_error),
            Err(EvoError::Failed(format!(
                "block data for block {hash} is not available (pruned or below an unvalidated \
                 snapshot base)"
            )))
        );
    }

    /// List read failures: Core's `BlockDataUnavailableError` text and the node's own faults,
    /// each an RPC error (never `AbortNode`); a readiness refusal stays one.
    #[test]
    fn list_failures_map_to_rpc_errors() {
        let hash = BlockHash::from([9; 32]);
        let unavailable = diff_error(list_error(StoreError::ListUnavailable(hash)));
        let EvoError::ListUnavailable(text) = &unavailable else {
            panic!("{unavailable:?}")
        };
        assert_eq!(*text, StoreError::ListUnavailable(hash).to_string());
        assert!(text.ends_with("is not available (pruned or below an unvalidated snapshot base)"));
        for (e, want) in [
            (StoreError::read("row"), EvoError::Store(StoreError::read("row").to_string())),
            (
                StoreError::inconsistent("row"),
                EvoError::Store(StoreError::inconsistent("row").to_string()),
            ),
            (StoreError::NotBuilt("lists"), EvoError::Unavailable),
        ] {
            assert_eq!(diff_error(list_error(e)), want);
        }
    }

    fn lists() -> (MnList, MnList, ProTxHash) {
        let (base, target) = (BlockHash::from([1; 32]), at(0, TARGET).hash);
        let mut from = MnList::new(base, 50, 0);
        let mut to = MnList::new(target, TARGET as i32, 0);
        for i in 0..8 {
            from.try_add(numbered(i), true).unwrap();
            to.try_add(numbered(i), true).unwrap();
        }
        to.try_add(numbered(8), true).unwrap();
        let payee = numbered(3);
        let mut cold = (*payee.cold).clone();
        cold.script_payout = numbered(5).cold.script_payout.clone();
        to.update(&payee.pro_tx_hash, payee.hot, Arc::new(cold)).unwrap();
        (from, to, payee.pro_tx_hash)
    }

    /// `extended = false` prints the P2P builder's diff byte for byte: the coinbase, its proof,
    /// every entry and every new quorum `protx diff` prints; `extended` adds the payout-only change with its target
    /// masternode.
    #[test]
    fn extended_false_is_the_p2p_diff() {
        let (from, to, payee) = lists();
        let mut capture = capture(three_quorums(), true);
        capture.base_list = Arc::new(from);
        capture.list = Arc::new(to);
        let files = files(&[1, 2]);
        let request = MnListDiffRequest {
            requested_base: capture.requested_base,
            base_block: capture.base_block,
            base_list: &capture.base_list,
            block: capture.target.hash,
            list: &capture.list,
            v20_active_after: true,
        };
        let source = CapturedSource {
            capture: &capture,
            files: &files,
            active_served: Cell::new(0),
        };
        let p2p = build_mnlistdiff(&request, &source).unwrap();
        let (rpc, dmns) = build(&capture, &files, false).unwrap();
        assert_eq!(rpc.mn_list.len(), 1, "only the new masternode");
        let facts = facts(&capture, rpc, dmns).unwrap();
        assert_eq!(facts.cbtx_hex, hex(&p2p.cb_tx));
        let mut tree = Vec::new();
        p2p.cb_tx_merkle_tree.encode(&mut tree);
        assert_eq!(facts.merkle_tree_hex, hex(&tree));
        let wire = |e: &dash_evo::sml::SmlEntry| {
            let mut out = Vec::new();
            write_sml_entry(e, 70_242, &mut out);
            out
        };
        assert!(facts.entries.iter().map(|e| wire(&e.entry)).eq(p2p.mn_list.iter().map(wire)));
        for (facts, entry) in facts.entries.iter().zip(&p2p.mn_list) {
            assert!(Arc::ptr_eq(&facts.dmn, capture.list.get(&entry.pro_reg_tx_hash).unwrap()));
        }
        let encoded: Vec<Vec<u8>> = facts
            .new_quorums
            .iter()
            .map(|q| {
                let mut out = Vec::new();
                q.encode(&mut out);
                out
            })
            .collect();
        assert_eq!(encoded, p2p.new_quorums);
        assert_eq!(facts.cl_sigs, p2p.quorums_cl_sigs);
        let block = block_at(TARGET as i32);
        let cbtx = match block.txs()[0].payload().unwrap().unwrap().payload() {
            SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.clone(),
            other => panic!("{other:?}"),
        };
        let bytes = |root: &dyn AsRef<[u8; 32]>| *root.as_ref();
        let roots = facts.roots.map(|(mn, q)| (*mn.as_bytes(), q.map(|q| *q.as_bytes())));
        let want =
            (bytes(&cbtx.merkle_root_mn_list()), Some(bytes(&cbtx.merkle_root_quorums().unwrap())));
        assert_eq!(roots, Some(want));

        let (extended, dmns) = build(&capture, &files, true).unwrap();
        let listed: Vec<ProTxHash> = extended.mn_list.iter().map(|e| e.pro_reg_tx_hash).collect();
        assert_eq!(listed.len(), 2);
        assert!(listed.contains(&payee));
        let at_payee = listed.iter().position(|p| *p == payee).unwrap();
        assert!(Arc::ptr_eq(&dmns[at_payee], capture.list.get(&payee).unwrap()));
    }

    /// Roots only for a coinbase whose CbTx payload reads whole.
    // Core: evo/core_write.cpp:358-363
    #[test]
    fn roots_need_a_whole_cbtx_payload() {
        let mut bytes = Vec::new();
        bytes.extend(block_at(5).txs()[0].raw().core_serialize());
        assert!(cbtx_roots::<BlockHash>(&bytes).is_some());
        let raw = block_at(5).txs()[0].to_mutable();
        let mut payload = raw.extra_payload().clone();
        payload.push(0);
        let trailing = dash_primitives::Transaction::new(
            raw.version(),
            raw.tx_type(),
            raw.inputs().clone(),
            raw.outputs().clone(),
            raw.lock_time(),
            payload,
        );
        let bytes = Tx::new(trailing).raw().core_serialize();
        assert!(cbtx_roots::<BlockHash>(&bytes).is_none());
    }

    fn work_at(height: u32) -> Option<WorkBlock> {
        let file = match height {
            22 => 1,
            32 => 2,
            _ => 9,
        };
        Some(WorkBlock {
            block: at(file, height),
            v20: true,
        })
    }

    /// The commitment reads record each answer and stop after the first the builder fails on,
    /// so a missing commitment is Core's refusal and a failed read the storage fault, never a
    /// read outside the capture.
    // Core: evo/smldiff.cpp:31-65
    #[test]
    fn commitment_reads_stop_at_the_failing_answer() {
        let store = |e: &str| StoreError::read(e.to_owned());
        for (q2, want) in [
            (Ok(None), EvoError::Failed("failed to build quorums diff".into())),
            (Err(store("mined")), EvoError::Store(store("mined").to_string())),
        ] {
            let asked = RefCell::new(Vec::new());
            let reads = commitment_reads_with(
                [BlockId(50), BlockId(TARGET)],
                |at| {
                    Ok(if at == BlockId(50) {
                        BTreeSet::new()
                    } else {
                        [Q1, Q2, Q3].into()
                    })
                },
                |key| {
                    asked.borrow_mut().push(*key);
                    if *key == Q2 {
                        q2.clone()
                    } else {
                        Ok(Some(commitment()))
                    }
                },
            );
            assert_eq!(*asked.borrow(), [Q1, Q2]);
            assert_eq!(reads.mined.keys().copied().collect::<Vec<_>>(), [Q1, Q2]);
            let error = build(&capture(reads, true), &files(&[1, 2]), false).unwrap_err();
            assert_eq!(diff_error(error), want);
        }

        let asked = Cell::new(0);
        let reads = commitment_reads_with(
            [BlockId(50), BlockId(TARGET)],
            |_| {
                asked.set(asked.get() + 1);
                Err(store("active"))
            },
            |_| panic!("no commitment is read after a failed active set"),
        );
        assert_eq!(asked.get(), 1);
        let error = build(&capture(reads, true), &files(&[1, 2]), false).unwrap_err();
        assert_eq!(diff_error(error), EvoError::Store(store("active").to_string()));
    }

    /// The quorum object reads stop after the first the builder fails on: a missing object is
    /// Core's refusal and a failed read the storage fault.
    // Core: evo/smldiff.cpp:69-86
    #[test]
    fn quorum_reads_stop_at_the_failing_answer() {
        let store = |e: &str| StoreError::read(e.to_owned());
        for (q2, want) in [
            (Ok(None), EvoError::Failed("failed to build quorum chainlock info".into())),
            (Err(store("object")), EvoError::Store(store("object").to_string())),
        ] {
            let mut reads = three_quorums();
            reads.bases.clear();
            reads.work.clear();
            let asked = RefCell::new(Vec::new());
            quorum_reads_with(
                &mut reads,
                |key| {
                    asked.borrow_mut().push(*key);
                    if *key == Q2 {
                        q2.clone()
                    } else {
                        base(30)
                    }
                },
                work_at,
            );
            assert_eq!(*asked.borrow(), [Q1, Q2]);
            assert_eq!(reads.bases.keys().copied().collect::<Vec<_>>(), [Q1, Q2]);
            let files = files(&[1, 2]);
            let error = build(&capture(reads, true), &files, false).unwrap_err();
            assert_eq!(diff_error(error), want);
            assert!(files.reads.borrow().is_empty());
        }
    }

    /// A rotated quorum's work block is at `baseHeight − quorumIndex − 8`: one at base 31 with
    /// index 1 shares the work block at 22 with a non-rotated one at base 30, which is read
    /// once; a height below genesis is `None` and unread.
    // Core: evo/smldiff.cpp:76-93
    #[test]
    fn rotated_quorums_group_under_their_work_block() {
        let mut reads = three_quorums();
        reads.bases.clear();
        reads.work.clear();
        let rotated = QuorumBase {
            base_height: 31,
            quorum_index: 1,
        };
        let heights = RefCell::new(Vec::new());
        quorum_reads_with(
            &mut reads,
            |key| match *key {
                Q1 => Ok(Some(rotated)),
                Q2 => base(30),
                _ => base(40),
            },
            |height| {
                heights.borrow_mut().push(height);
                work_at(height)
            },
        );
        assert_eq!(*heights.borrow(), [22, 32]);
        assert_eq!(reads.work.keys().copied().collect::<Vec<_>>(), [22, 32]);
        let files = files(&[1]);
        let (diff, _) = build(&capture(reads, true), &files, false).unwrap();
        let mut sigs = diff.quorums_cl_sigs;
        sigs.sort();
        let mut want = vec![(signature(22), vec![0, 1]), ([0; 96], vec![2])];
        want.sort();
        assert_eq!(sigs, want);

        let mut reads = three_quorums();
        reads.bases.clear();
        reads.work.clear();
        quorum_reads_with(&mut reads, |_| base(5), |h| panic!("no block below genesis: {h}"));
        assert_eq!(reads.work, [(-3, None)].into());
    }

    /// A captured block's position only under `BLOCK_HAVE_DATA` and with a file.
    #[test]
    fn a_captured_block_has_a_position_only_with_its_data() {
        let entry = |status: u32, file: Option<i32>| EntryView {
            id: BlockId(7),
            hash: BlockHash::from([7; 32]),
            header: BlockHeader::default(),
            prev: None,
            height: 7,
            chain_work: dash_chain::Work::ZERO,
            mtp: 0,
            time_max: 0,
            status: Status(status),
            n_tx: 1,
            file,
            data_pos: 80,
            undo_pos: 0,
        };
        let block = CapturedBlock::of(&entry(Status::HAVE_DATA | Status::VALID_SCRIPTS, Some(3)));
        assert_eq!(
            block,
            CapturedBlock {
                id: BlockId(7),
                hash: BlockHash::from([7; 32]),
                height: 7,
                pos: Some(FlatFilePos {
                    file: 3,
                    pos: 80,
                }),
            }
        );
        assert_eq!(CapturedBlock::of(&entry(Status::VALID_SCRIPTS, Some(3))).pos, None);
        assert_eq!(CapturedBlock::of(&entry(Status::HAVE_DATA, None)).pos, None);
    }
}
