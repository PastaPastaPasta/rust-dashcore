//! H4's step 10, `CheckCbTxBestChainlock` (`stm:156-239`), with the best ChainLock null and its
//! static cache always missing until CH7b (D45).

use dash_consensus::{Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx};
use dash_dml::StoreError;
use dash_evo::cbtx::{CBTX_VERSION_CLSIG_AND_BALANCE, CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::llmq::BlsSigValue;
use dash_primitives::{BlockHash, CoinbaseCommitment};

use crate::chain::{ConnectCx, Fatal};
use crate::connect::SpecialTxFailure;

/// The capability [`NotBuiltClVerifier`] refuses for.
pub const CHAINLOCK_VERIFY: &str = "ChainLock verification (CH7b)";

/// `chainlock::VerifyChainLock(params, chain, qman, clsig)` (`clsig:253-261`) for a CbTx's best
/// ChainLock: whether the ChainLock quorum's signature over `block_hash` at `height` is valid.
pub trait ClVerifier {
    /// # Errors
    ///
    /// [`StoreError::NotBuilt`] where this build cannot verify (D2), or a read fault.
    fn verify(
        &self,
        cx: &ConnectCx<'_>,
        height: u32,
        block_hash: &BlockHash,
        sig: &BlsSigValue,
        bls: BlsCtx,
    ) -> Result<bool, StoreError>;
}

/// The verifier until CH7b: every call refuses (D45).
#[derive(Clone, Copy, Debug, Default)]
pub struct NotBuiltClVerifier;

impl ClVerifier for NotBuiltClVerifier {
    fn verify(
        &self,
        _: &ConnectCx<'_>,
        _: u32,
        _: &BlockHash,
        _: &BlsSigValue,
        _: BlsCtx,
    ) -> Result<bool, StoreError> {
        Err(StoreError::NotBuilt(CHAINLOCK_VERIFY))
    }
}

pub(super) fn invalid(reason: &'static str) -> SpecialTxFailure {
    SpecialTxFailure::Invalid(BlockValidationError::new(BlockValidationResult::Consensus, reason))
}

/// What step 10 reads of the block and its parent.
pub struct ClStep<'a> {
    /// The block's height.
    pub height: u32,
    /// `pindex->pprev`.
    pub prev: BlockId,
    /// `DeploymentActiveAt(*pindex->pprev, V20)`.
    pub v20_at_prev: bool,
    /// `ReadBlockFromDisk(pindex->pprev)`: `None` where the read fails.
    pub prev_block: &'a dyn Fn() -> Option<std::sync::Arc<Block>>,
    /// The attempt's scheme, Core's global flag (D9).
    pub bls: BlsCtx,
}

/// `CheckCbTxBestChainlock` with no best ChainLock and an empty cache (D45): the previous CbTx's
/// ChainLock bounds this one, and a non-null signature is verified.
///
/// # Errors
///
/// `Fatal` for a verifier fault or an ancestor missing from V's reader.
// Core: evo/specialtxman.cpp:156-239 CheckCbTxBestChainlock
pub fn check_cbtx_best_chainlock(
    cbtx: &CoinbaseCommitment,
    step: &ClStep<'_>,
    cx: &ConnectCx<'_>,
    verifier: &dyn ClVerifier,
) -> Result<Result<(), SpecialTxFailure>, Fatal> {
    if cbtx.version < CBTX_VERSION_CLSIG_AND_BALANCE {
        return Ok(Ok(()));
    }
    // `best_clsig` is null (height -1): the matches at `:176` and `:211` need a height of
    // `nHeight - 1` and `nHeight - diff - 1`, never -1 past genesis and the range check.
    let diff = cbtx.best_cl_height_diff.unwrap_or(0);
    let sig = cbtx.best_cl_signature.as_ref().map(|sig| sig.get(step.bls));
    let valid = sig.as_ref().is_some_and(|sig| sig.is_valid());

    // `:185-191`: the cache misses, so the parent's block is read.
    let prev_height = step.height - 1;
    let prev_cl = if step.v20_at_prev {
        let facts = match (step.prev_block)() {
            None => CoinbaseFacts::BlockDataUnavailable,
            Some(block) => match block.txs().first() {
                None => CoinbaseFacts::NoTransactions,
                Some(tx) => CoinbaseFacts::First(tx.clone()),
            },
        };
        nonnull_coinbase_chainlock(&facts, prev_height, true, step.bls)
    } else {
        None
    };
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
            let cl_height = step.height - 1 - u32::try_from(diff).expect("below the height");
            let Some(ancestor) = cx.reader.ancestor(step.prev, cl_height) else {
                return Ok(Err(invalid("bad-cbtx-cldiff-ancestor")));
            };
            let hash = cx
                .reader
                .entry(ancestor)
                .ok_or_else(|| Fatal::new("CheckCbTxBestChainlock: an unindexed ancestor"))?
                .hash;
            match verifier.verify(cx, cl_height, &hash, sig.value(), step.bls) {
                Ok(true) => Ok(Ok(())),
                Ok(false) => Ok(Err(invalid("bad-cbtx-invalid-clsig"))),
                Err(e) => match e.capability() {
                    Some(capability) => Ok(Err(SpecialTxFailure::NotReady(capability))),
                    None => Err(Fatal::abort_node(format!("VerifyChainLock: {e}"))),
                },
            }
        }
        _ if diff != 0 => Ok(Err(invalid("bad-cbtx-cldiff"))),
        _ => Ok(Ok(())),
    }
}
