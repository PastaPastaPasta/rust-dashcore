//! The quorum modifier (LQ1 §3.3, quirk 29).

use dash_consensus::BlockId;
use dash_consensus::ctx::Deployment;
use dash_consensus::hash::sha256d;
use dash_params::BuriedDeployment;
use dash_params::{LlmqParams, LlmqType};
use dash_types::Numeric;

use super::ctx::{LlmqCtx, LlmqError, LlmqSource};
use super::members::{MemberError, WORK_DIFF_DEPTH};
use super::score::QuorumModifier;
use super::view::MnListView;
use crate::cbtx::nonnull_coinbase_chainlock;

/// Core `GetHashModifier(llmqParams, consensus, pCycleQuorumBaseBlockIndex)`.
///
/// The work block is `cycle_base - 8`. With V20 active after it, the modifier comes from the work
/// block ([`hash_modifier_from_work_block`]); before, it hashes the work block for a rotated type
/// and the cycle base for a non-rotated one.
///
/// Below height 8 Core's work block is null. Before V20 the non-rotated modifier does not read it;
/// a rotated one, or V20 active from genesis, is [`MemberError::Missing`] for `cycle_base`, as
/// Core has no value there.
// Core: v24 llmq/utils.cpp:98 GetHashModifier
pub fn hash_modifier<E, L: MnListView>(
    ctx: &LlmqCtx<'_, E>,
    src: &dyn LlmqSource<L, Error = E>,
    llmq_type: LlmqType,
    params: &LlmqParams,
    cycle_base: BlockId,
) -> Result<QuorumModifier, LlmqError<E>> {
    let base_height = ctx.height(cycle_base)?;
    let work = match base_height.checked_sub(WORK_DIFF_DEPTH) {
        Some(h) => Some(ctx.ancestor_below(cycle_base, h)?),
        None => None,
    };
    if ctx.deployments().active_after(work, Deployment::Buried(BuriedDeployment::V20)) {
        let work = work.ok_or(MemberError::Missing(cycle_base))?;
        return hash_modifier_from_work_block(ctx, src, llmq_type, work);
    }
    let hashed = if params.use_rotation {
        work.ok_or(MemberError::Missing(cycle_base))?
    } else {
        cycle_base
    };
    type_and_hash(ctx, llmq_type, hashed)
}

/// Core `GetHashModifierFromWorkBlock(llmqParams, pWorkBlockIndex)`: with a non-null coinbase
/// ChainLock, `SerializeHash(type, work height, signature)`; without one, `SerializeHash(type,
/// work hash)`.
///
/// Core's `GetNonNullCoinbaseChainlock` reads the block only when V20 is active at it, so the
/// source is consulted only then. A work block whose data the view does not hold is
/// [`MemberError::Missing`], never the hash form (EV addendum §2.1). The signature is serialized
/// under the caller's scheme, as `SerializeHash` uses Core's global flag.
// Core: v24 llmq/utils.cpp:86 GetHashModifierFromWorkBlock, evo/cbtx.cpp:181
// GetNonNullCoinbaseChainlock
pub fn hash_modifier_from_work_block<E, L: MnListView>(
    ctx: &LlmqCtx<'_, E>,
    src: &dyn LlmqSource<L, Error = E>,
    llmq_type: LlmqType,
    work: BlockId,
) -> Result<QuorumModifier, LlmqError<E>> {
    let chainlock = if ctx.deployments().active_at(work, Deployment::Buried(BuriedDeployment::V20))
    {
        let facts = src
            .coinbase_facts(work)
            .map_err(MemberError::View)?
            .ok_or(MemberError::Missing(work))?;
        nonnull_coinbase_chainlock(&facts, ctx.height(work)?, true, ctx.bls())
    } else {
        None
    };
    match chainlock {
        Some((signature, _)) => {
            let mut data = [0u8; 1 + 4 + 96];
            data[0] = llmq_type.to_base();
            data[1..5].copy_from_slice(&ctx.height(work)?.to_le_bytes());
            data[5..].copy_from_slice(&signature.to_bytes(ctx.bls().scheme()));
            Ok(QuorumModifier(sha256d(&data)))
        }
        None => type_and_hash(ctx, llmq_type, work),
    }
}

/// `SerializeHash(std::make_pair(type, block hash))`.
fn type_and_hash<E>(
    ctx: &LlmqCtx<'_, E>,
    llmq_type: LlmqType,
    block: BlockId,
) -> Result<QuorumModifier, LlmqError<E>> {
    let hash = ctx.hash(block)?;
    let mut data = [0u8; 33];
    data[0] = llmq_type.to_base();
    data[1..].copy_from_slice(hash.as_bytes());
    Ok(QuorumModifier(sha256d(&data)))
}
