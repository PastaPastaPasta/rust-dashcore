//! H4's step 10, `CheckCbTxBestChainlock` (`stm:139-215`): the best ChainLock's two shortcuts,
//! Core's static cache, [`CbTxClCell`], and the verify, [`AttemptClVerifier`].

use std::cell::RefCell;
use std::path::Path;
#[cfg(feature = "test-support")]
use std::path::PathBuf;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Arc;
#[cfg(feature = "test-support")]
use std::sync::Weak;
#[cfg(feature = "planted-bugs")]
use std::sync::atomic::{AtomicBool, Ordering};

use dash_chain::{ChainReader, DUMMY_ID};
use dash_consensus::{Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx};
use dash_dml::StoreError;
use dash_evo::EffectSink;
use dash_evo::cbtx::{CBTX_VERSION_CLSIG_AND_BALANCE, CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::llmq::BlsSigValue;
#[cfg(any(test, feature = "test-support"))]
use dash_evo::llmq::ScanEvent;
use dash_evo::locks::{
    LockError, LockReject, QuorumScanView, QuorumStatus, ScanLookup, VerifiedChainLock,
    verify_chainlock_sig,
};
use dash_params::LlmqType;
use dash_pkc::bls::{BlsEagerSig, BlsGlobalSig};
use dash_primitives::{BlockHash, CoinbaseCommitment, QuorumHash};
use parking_lot::Mutex;

use super::node::{AttemptBinding, store_failure};
#[cfg(feature = "planted-bugs")]
use crate::Planted;
use crate::chain::{ChainLockSig, ConnectCx, Fatal};
use crate::connect::{ClBest, SpecialTxFailure};

/// The capability [`NotBuiltClVerifier`] refuses for.
#[cfg(any(test, feature = "test-support"))]
pub const CHAINLOCK_VERIFY: &str = "ChainLock verification (CH7b)";

/// The attempt a step-10 verify runs in (CH7 §4.5.2): route C's binding over the attempt's
/// handle and its target's view, the attempt's effect sink, and the block's parent, the end of the
/// chain the verify reads. A test of the arms before the verify runs detached, with no attempt.
pub struct ClAttempt<'b, 'a> {
    /// The binding and the sink; `None` when detached.
    pub(crate) bound: Option<(&'b AttemptBinding<'a>, &'b mut dyn EffectSink)>,
    /// `pindex->pprev`.
    pub(crate) prev: BlockId,
}

impl<'b, 'a> ClAttempt<'b, 'a> {
    /// The attempt H4 binds at step 10.
    pub(crate) fn bound(
        binding: &'b AttemptBinding<'a>,
        sink: &'b mut dyn EffectSink,
        prev: BlockId,
    ) -> Self {
        ClAttempt {
            bound: Some((binding, sink)),
            prev,
        }
    }

    /// No attempt: a verifier that reads quorums cannot run.
    pub fn detached(prev: BlockId) -> Self {
        ClAttempt {
            bound: None,
            prev,
        }
    }
}

/// `chainlock::VerifyChainLock(params, chain, qman, clsig)` (`clsig:253-261`) for a CbTx's best
/// ChainLock: whether the ChainLock quorum's signature over `block_hash` at `height` is valid
/// (`Ok(Ok(_))`), or a store failure step 10 maps as D5 does (`Ok(Err(_))`: a read fault, or
/// D2's [`StoreError::NotBuilt`] for a store the check cannot read yet).
pub trait ClVerifier {
    /// # Errors
    ///
    /// `Fatal` where the check gives no verdict Core could give (X4).
    fn verify(
        &self,
        cx: &ConnectCx<'_>,
        attempt: &mut ClAttempt<'_, '_>,
        height: u32,
        block_hash: &BlockHash,
        sig: &BlsSigValue,
        bls: BlsCtx,
    ) -> Result<Result<bool, StoreError>, Fatal>;
}

/// D45's verifier, the node's until CH7b-3 wired [`AttemptClVerifier`]: every call refuses.
/// Tests keep it where step 10 never reaches the verify, and as the stub W-1's control reverts
/// the node to.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct NotBuiltClVerifier;

#[cfg(any(test, feature = "test-support"))]
impl ClVerifier for NotBuiltClVerifier {
    fn verify(
        &self,
        _: &ConnectCx<'_>,
        _: &mut ClAttempt<'_, '_>,
        _: u32,
        _: &BlockHash,
        _: &BlsSigValue,
        _: BlsCtx,
    ) -> Result<Result<bool, StoreError>, Fatal> {
        Ok(Err(StoreError::NotBuilt(CHAINLOCK_VERIFY)))
    }
}

/// Step 10's `chainlock::VerifyChainLock` (CH7b-3 C4; CH7 §4.5.2): dash-evo's
/// [`verify_chainlock_sig`] over the attempt's binding at the block's parent (`SigningView`),
/// its result read as X4 reads it ([`lock_verdict`]). Every read and build goes through the
/// attempt's handle, so it never dispatches to V.
#[derive(Debug, Default)]
pub struct AttemptClVerifier {
    #[cfg(any(test, feature = "test-support"))]
    census: Arc<VerifyCensus>,
    /// C7's census controls, each fired once.
    #[cfg(feature = "planted-bugs")]
    planted: Option<(Planted, AtomicBool)>,
}

impl AttemptClVerifier {
    /// A verifier and the census it records.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_census() -> (Self, Arc<VerifyCensus>) {
        let verifier = AttemptClVerifier::default();
        let census = Arc::clone(&verifier.census);
        (verifier, census)
    }

    /// Arms one of C7's census controls, [`Planted::ClCensusSuppressOnce`] or
    /// [`Planted::ClCensusDuplicateOnce`]; another bug plants nothing here.
    #[cfg(feature = "planted-bugs")]
    pub fn plant(&mut self, bug: Planted) {
        self.planted = Some((bug, AtomicBool::new(false)));
    }

    /// The checks this call makes: one, or once a planted control's none or two.
    fn checks(&self) -> usize {
        #[cfg(feature = "planted-bugs")]
        if let Some((bug, fired)) = &self.planted {
            let once = |n| {
                if fired.swap(true, Ordering::SeqCst) {
                    1
                } else {
                    n
                }
            };
            match bug {
                Planted::ClCensusSuppressOnce => return once(0),
                Planted::ClCensusDuplicateOnce => return once(2),
                _ => {}
            }
        }
        1
    }
}

/// The node's step-10 verifier for a chainstate at `data_dir`.
#[cfg(not(feature = "test-support"))]
pub fn node_cl_verifier(_data_dir: &Path) -> AttemptClVerifier {
    AttemptClVerifier::default()
}

/// The node's step-10 verifier for a chainstate at `data_dir`, its census registered under it
/// for [`census_under`] (CH7b-3 C7: the node's replays read it).
#[cfg(feature = "test-support")]
pub fn node_cl_verifier(data_dir: &Path) -> AttemptClVerifier {
    let (verifier, census) = AttemptClVerifier::with_census();
    let mut registry = CENSUSES.lock();
    registry.retain(|(_, c)| c.strong_count() > 0);
    registry.push((data_dir.to_owned(), Arc::downgrade(&census)));
    verifier
}

/// Each live node verifier's census by its chainstate's data directory, newest last.
#[cfg(feature = "test-support")]
static CENSUSES: Mutex<Vec<(PathBuf, Weak<VerifyCensus>)>> = Mutex::new(Vec::new());

/// The newest live census of a chainstate whose data directory is `dir` or below it.
#[cfg(feature = "test-support")]
pub fn census_under(dir: &Path) -> Option<Arc<VerifyCensus>> {
    let registry = CENSUSES.lock();
    registry.iter().rev().filter(|(d, _)| d.starts_with(dir)).find_map(|(_, c)| c.upgrade())
}

impl ClVerifier for AttemptClVerifier {
    /// A planted control's skipped check is answered valid.
    // Core: chainlock/clsig.cpp:253-261 VerifyChainLock, at evo/specialtxman.cpp:202-205
    fn verify(
        &self,
        cx: &ConnectCx<'_>,
        attempt: &mut ClAttempt<'_, '_>,
        height: u32,
        block_hash: &BlockHash,
        sig: &BlsSigValue,
        bls: BlsCtx,
    ) -> Result<Result<bool, StoreError>, Fatal> {
        let prev = attempt.prev;
        let Some((binding, sink)) = attempt.bound.as_mut() else {
            return Err(Fatal::assert("VerifyChainLock outside an attempt: no quorum view"));
        };
        let tip_height = cx
            .reader
            .entry(prev)
            .ok_or_else(|| Fatal::new("VerifyChainLock: an unindexed parent"))?
            .height;
        let cl_height = i32::try_from(height)
            .map_err(|_| Fatal::assert(format!("VerifyChainLock at height {height}")))?;
        let view = SigningView {
            binding,
            sink: RefCell::new(&mut **sink),
            reader: cx.reader,
            prev,
            tip_height,
            #[cfg(any(test, feature = "test-support"))]
            census: &self.census,
        };
        let mut verdict = Ok(Ok(true));
        for _ in 0..self.checks() {
            let r = verify_chainlock_sig(cx.params, &view, bls, cl_height, block_hash, sig);
            verdict = lock_verdict(r);
            #[cfg(any(test, feature = "test-support"))]
            self.census.checked(tip_height + 1, height, &verdict);
        }
        verdict
    }
}

/// X4: a ChainLock check's result as step 10 reads it. Core's statuses are `NoQuorum`
/// (`qm:821-823`) and `Invalid` (`qm:826`), and it maps each that is not `Valid` to
/// `bad-cbtx-invalid-clsig` (`stm:202-205`). The node's other rejections, an unknown type and a
/// signature outside the BLS domain have no status of Core's: by declared node policy they stop
/// the node. A view fault is a store failure, which step 10 maps as D5 does.
///
/// # Errors
///
/// `Fatal` for a result Core has no status for.
pub fn lock_verdict(
    r: Result<VerifiedChainLock, LockError<StoreError>>,
) -> Result<Result<bool, StoreError>, Fatal> {
    match r {
        Ok(_) => Ok(Ok(true)),
        Err(LockError::Rejected(LockReject::NoQuorum | LockReject::InvalidSignature)) => {
            Ok(Ok(false))
        }
        Err(LockError::View(e)) => Ok(Err(e)),
        Err(
            e
            @ (LockError::Rejected(_) | LockError::BlsUnsupported | LockError::UnknownLlmqType(_)),
        ) => Err(Fatal::abort_node(format!("VerifyChainLock: no verdict: {e}"))),
    }
}

/// Step 10's [`QuorumScanView`] (X1): the attempt's binding at the block's parent, Core's active
/// chain tip while the block connects. A ChainLock check reads only the tip's height and a scan.
struct SigningView<'v, 'a> {
    binding: &'v AttemptBinding<'a>,
    /// The attempt's sink, for the scan's builds.
    sink: RefCell<&'v mut dyn EffectSink>,
    reader: &'v ChainReader,
    prev: BlockId,
    tip_height: u32,
    #[cfg(any(test, feature = "test-support"))]
    census: &'v VerifyCensus,
}

impl QuorumScanView for SigningView<'_, '_> {
    type Error = StoreError;

    fn tip_height(&self) -> u32 {
        self.tip_height
    }

    fn scan(&self, llmq_type: LlmqType, start_height: u32) -> Result<ScanLookup, StoreError> {
        #[cfg(any(test, feature = "test-support"))]
        if *self.census.fault_at.lock() == Some(self.tip_height + 1) {
            return Err(StoreError::read("an injected read fault in step 10's scan"));
        }
        let start = scan_start(self.reader, self.prev, start_height)?;
        let mut sink = self.sink.borrow_mut();
        let (scan, event) = self.binding.signing_scan(self.prev, llmq_type, start, &mut **sink)?;
        #[cfg(any(test, feature = "test-support"))]
        self.census.scans.lock().push((start_height, event));
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = event;
        Ok(ScanLookup::Held(scan))
    }

    fn scan_at_tip(&self, _: LlmqType, _: usize) -> Result<ScanLookup, StoreError> {
        Err(not_read("active set"))
    }

    fn block_height(&self, _: &BlockHash) -> Result<Option<u32>, StoreError> {
        Err(not_read("cycle block"))
    }

    fn quorum(&self, _: LlmqType, _: &QuorumHash) -> Result<Option<QuorumStatus>, StoreError> {
        Err(not_read("named quorum"))
    }
}

/// Step 10's scan start, Core's `SelectQuorumForSigningStartBlock` (`qm:741-752`): the block at
/// `start_height` on the active chain, which ends at the block's parent while the block connects,
/// so the parent's ancestor there (CH7b-3; Opus r1 m-2). On a fork that is not the block the
/// reader's active chain or best header holds at that height.
///
/// # Errors
///
/// An invariant fault: the parent's chain has no block at `start_height`.
pub(crate) fn scan_start(
    reader: &ChainReader,
    prev: BlockId,
    start_height: u32,
) -> Result<BlockId, StoreError> {
    reader.ancestor(prev, start_height).ok_or_else(|| {
        StoreError::inconsistent(format!("step 10's scan: no block at {start_height}"))
    })
}

/// The invariant fault of a view method no ChainLock check calls: it selects at a start block.
fn not_read(what: &str) -> StoreError {
    StoreError::inconsistent(format!("step 10's quorum view: a ChainLock check reads no {what}"))
}

/// What [`AttemptClVerifier`] records (CH7b-3 C4; Sol r1 #6): each check as it is made, around
/// the `verify_chainlock_sig` call itself, and each scan it made.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct VerifyCensus {
    /// `(attempt height, cl_height, verdict)`, the verdict `None` where the check gave none.
    pub checks: Mutex<Vec<(u32, u32, Option<bool>)>>,
    /// Each scan's start height and cache event, in order.
    pub scans: Mutex<Vec<(u32, Option<ScanEvent>)>>,
    /// The attempt height whose scans fail with an injected read fault.
    pub fault_at: Mutex<Option<u32>>,
}

#[cfg(any(test, feature = "test-support"))]
impl VerifyCensus {
    fn checked(&self, at: u32, cl_height: u32, verdict: &Result<Result<bool, StoreError>, Fatal>) {
        let verdict = match verdict {
            Ok(Ok(valid)) => Some(*valid),
            _ => None,
        };
        self.checks.lock().push((at, cl_height, verdict));
    }
}

pub(super) fn invalid(reason: &'static str) -> SpecialTxFailure {
    SpecialTxFailure::Invalid(BlockValidationError::new(BlockValidationResult::Consensus, reason))
}

/// Core's step-10 statics `cached_pindex` and `cached_chainlock` (`stm:147-149`; CH7 X5,
/// BL-179). Core's are process-wide; the node's is one per [`MembersFacade`], behind a leaf
/// mutex held for one read or one write. Step 10 writes it in the attempt, outside the evo
/// transaction, so an attempt that fails later keeps its write, as Core's does.
///
/// [`MembersFacade`]: crate::llmq::facade::MembersFacade
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CbTxClCell {
    /// `cached_pindex` and `cached_chainlock`: the block whose CbTx wrote the cell, and that
    /// CbTx's best-ChainLock signature and difference. `None` before any write, and after a
    /// proposal's ([`ClStep`]'s write).
    pub entry: Option<(BlockId, BlsEagerSig, u32)>,
}

impl CbTxClCell {
    /// `stm:162-164`: the cached ChainLock, if `prev`'s CbTx wrote it.
    fn after(&self, prev: BlockId) -> Option<(BlsEagerSig, u32)> {
        match &self.entry {
            Some((block, sig, diff)) if *block == prev => Some((sig.clone(), *diff)),
            _ => None,
        }
    }
}

/// What step 10 reads of the block and its parent.
pub struct ClStep<'a> {
    /// The block's height.
    pub height: u32,
    /// `pindex`: a proposal's is its dummy index, [`DUMMY_ID`].
    pub id: BlockId,
    /// `pindex->pprev`.
    pub prev: BlockId,
    /// `DeploymentActiveAt(*pindex->pprev, V20)`.
    pub v20_at_prev: bool,
    /// `ReadBlockFromDisk(pindex->pprev)`: `None` where the read fails.
    pub prev_block: &'a dyn Fn() -> Option<std::sync::Arc<Block>>,
    /// The attempt's scheme, Core's global flag (D9).
    pub bls: BlsCtx,
    /// Core's statics.
    pub cell: &'a Mutex<CbTxClCell>,
}

impl ClStep<'_> {
    /// `cached_chainlock = std::make_pair(sig, diff); cached_pindex = pindex;`. A proposal's
    /// `pindex` is its dummy index, a stack object no later block's `pprev` can be
    /// (`v:4955-4958`), so Core's write there leaves an entry nothing matches: the node's clears
    /// the cell, which keeps neither a dummy-keyed entry nor the one before.
    fn write(&self, sig: &BlsEagerSig, diff: u32) {
        let entry = (self.id != DUMMY_ID).then(|| (self.id, sig.clone(), diff));
        *self.cell.lock() = CbTxClCell {
            entry,
        };
    }
}

/// How step 10 accepted a CbTx.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClPass {
    /// Below `CLSIG_AND_BALANCE` (`stm:143-145`): nothing read.
    BelowVersion,
    /// Shortcut 1 (`stm:152-158`): the best ChainLock is at the parent, with the CbTx's signature
    /// and a difference of 0.
    BestAtParent,
    /// Shortcut 2 (`stm:187-193`): the best ChainLock, older than the parent, is the CbTx's.
    OutdatedBest,
    /// `VerifyChainLock` passed (`stm:202-208`).
    Verified,
    /// A null signature with a difference of 0 (`stm:209-212`): nothing written.
    Null,
}

/// `best_clsig.getHeight() == height && best_clsig.getSig() == sig` (`stm:152-153`, `:187`).
/// With no best ChainLock its height is −1, which neither shortcut's height reaches. The
/// signatures compare as Core's objects (`src/bls/bls.h:77-80`), both read under the
/// attempt's scheme; a best signature Core could not have read never matches.
fn is_best(best: Option<&ChainLockSig>, height: i64, sig: &BlsEagerSig, bls: BlsCtx) -> bool {
    best.is_some_and(|best| {
        i64::from(best.height) == height
            && BlsGlobalSig::decode(&best.sig).is_ok_and(|best| best.get(bls) == *sig)
    })
}

/// `CheckCbTxBestChainlock`: `cl_best` is asked once past the version gate, its two shortcuts
/// are taken as Core's, the previous CbTx's ChainLock (from the cell when the parent wrote it,
/// else from the parent's block) bounds this one, and a non-null signature is verified. Each
/// pass but the null one writes the cell.
///
/// # Errors
///
/// `Fatal` for a `cl_best` miss, a verify that gives no verdict, a store fault, or an ancestor
/// missing from V's reader.
// Core: evo/specialtxman.cpp:139-215 CheckCbTxBestChainlock
pub fn check_cbtx_best_chainlock(
    cbtx: &CoinbaseCommitment,
    step: &ClStep<'_>,
    cx: &ConnectCx<'_>,
    attempt: &mut ClAttempt<'_, '_>,
    cl_best: &mut ClBest<'_>,
    verifier: &dyn ClVerifier,
) -> Result<Result<ClPass, SpecialTxFailure>, Fatal> {
    if cbtx.version < CBTX_VERSION_CLSIG_AND_BALANCE {
        return Ok(Ok(ClPass::BelowVersion));
    }
    let best = cl_best()?;
    let best = best.as_deref();
    let diff = cbtx.best_cl_height_diff.unwrap_or(0);
    let sig = cbtx.best_cl_signature.as_ref().map(|sig| sig.get(step.bls));
    if diff == 0
        && let Some(sig) = &sig
        && is_best(best, i64::from(step.height) - 1, sig, step.bls)
    {
        step.write(sig, 0);
        return Ok(Ok(ClPass::BestAtParent));
    }

    // `:160-167`: the cell, else the parent's block.
    let prev_height = step.height - 1;
    let mut prev_cl = step.cell.lock().after(step.prev);
    if prev_cl.is_none() && step.v20_at_prev {
        let facts = match (step.prev_block)() {
            None => CoinbaseFacts::BlockDataUnavailable,
            Some(block) => match block.txs().first() {
                None => CoinbaseFacts::NoTransactions,
                Some(tx) => CoinbaseFacts::First(tx.clone()),
            },
        };
        prev_cl = nonnull_coinbase_chainlock(&facts, prev_height, true, step.bls);
    }
    let valid = sig.as_ref().is_some_and(|sig| sig.is_valid());
    if let Some((_, prev_diff)) = prev_cl {
        if !valid {
            return Ok(Err(invalid("bad-cbtx-null-clsig")));
        }
        if diff > u64::from(prev_diff) + 1 {
            return Ok(Err(invalid("bad-cbtx-older-clsig")));
        }
    }

    match sig {
        Some(sig) if valid => {
            if diff >= u64::from(step.height) {
                return Ok(Err(invalid("bad-cbtx-cldiff")));
            }
            let diff = u32::try_from(diff).expect("below the height");
            let cl_height = step.height - 1 - diff;
            if is_best(best, i64::from(cl_height), &sig, step.bls) {
                step.write(&sig, diff);
                return Ok(Ok(ClPass::OutdatedBest));
            }
            let Some(ancestor) = cx.reader.ancestor(step.prev, cl_height) else {
                return Ok(Err(invalid("bad-cbtx-cldiff-ancestor")));
            };
            let hash = cx
                .reader
                .entry(ancestor)
                .ok_or_else(|| Fatal::new("CheckCbTxBestChainlock: an unindexed ancestor"))?
                .hash;
            match verifier.verify(cx, attempt, cl_height, &hash, sig.value(), step.bls)? {
                Ok(true) => {
                    step.write(&sig, diff);
                    Ok(Ok(ClPass::Verified))
                }
                Ok(false) => Ok(Err(invalid("bad-cbtx-invalid-clsig"))),
                Err(e) => store_failure("VerifyChainLock", &e).map(Err),
            }
        }
        _ if diff != 0 => Ok(Err(invalid("bad-cbtx-cldiff"))),
        _ => Ok(Ok(ClPass::Null)),
    }
}
