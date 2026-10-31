//! Readiness guard G1q (CH7 D3): the well-formed blocks whose processing needs quorum members, a
//! quorum object or the precompute, which `NoMembers` cannot answer until CH7b-2. H4 asks it at
//! its entry, after step 0, and the startup check through `EvoHooks::startup_refusal`. Its arms
//! are a pure function of the block's (or a template's) transactions, its parent and the
//! deployments, so a template source can refuse the same blocks.

use dash_consensus::ctx::Deployment;
use dash_consensus::primitives::canonical::TRANSACTION_ASSET_UNLOCK;
use dash_consensus::tx::{TRANSACTION_MNHF_SIGNAL, TRANSACTION_QUORUM_COMMITMENT};
use dash_consensus::{BlockId, TxRef};
use dash_evo::llmq::{LlmqCtx, LlmqError, cycle_base_height, enabled_quorum_params};
use dash_params::BuriedDeployment;
use dash_primitives::SpecialPayload;
use dash_types::Numeric as _;

pub use super::not_built::QUORUM_VALIDATION;

/// The arm of G1q that holds for a block (D3's table, CH7a-4 to CH7b-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum G1qArm {
    /// (a) a quorum commitment that is not null: `Verify` asks its members.
    Commitment,
    /// (b) an MNHF signal or an asset unlock: their checks read a quorum.
    QuorumSigned,
    /// (c) a rotation cycle base: step 6's precompute asks the rotated members.
    CycleBase,
}

/// G1q (D3) for the block at `height` after `prev` holding `txs`: the arm that holds, or `None`.
///
/// - Only where DIP0003 is active for the block: below it no check asks a quorum, and the
///   precompute's refusal is the readiness stub's own (D53).
/// - **Decode first:** a special payload that does not decode completely bypasses G1q, so H4
///   returns Core's reject for it.
/// - Otherwise (a) and (b) by raw type, with QC nullness from the decoded payload, then (c).
///
/// # Errors
///
/// A chain read of `llmq` that fails.
pub fn g1q<E>(
    txs: &[TxRef],
    llmq: &LlmqCtx<'_, E>,
    prev: BlockId,
    height: u32,
) -> Result<Option<G1qArm>, LlmqError<E>> {
    if !llmq.deployments().active_after(Some(prev), Deployment::Buried(BuriedDeployment::Dip0003)) {
        return Ok(None);
    }
    let mut arm = None;
    for tx in txs {
        // Core reads these types only from special-version transactions (`HasExtraPayloadField`,
        // `specialtxman.cpp:224`).
        let Some(slot) = tx.payload() else {
            continue;
        };
        let Some(decoded) = slot.ok().filter(|d| d.is_complete()) else {
            return Ok(None);
        };
        let quorum = match (tx.tx_type().to_base(), &decoded.payload) {
            (TRANSACTION_QUORUM_COMMITMENT, SpecialPayload::QuorumCommitment(p)) => {
                (!p.commitment.is_null()).then_some(G1qArm::Commitment)
            }
            (TRANSACTION_MNHF_SIGNAL | TRANSACTION_ASSET_UNLOCK, _) => Some(G1qArm::QuorumSigned),
            _ => None,
        };
        arm = arm.or(quorum);
    }
    if arm.is_some() {
        return Ok(arm);
    }
    Ok(rotation_cycle_base(llmq, prev, height)?.then_some(G1qArm::CycleBase))
}

/// Arm (c): the precompute loop's predicate (`NoMembers::precompute`), a type enabled at `prev`
/// with `IsQuorumRotationEnabled` at a height `h % dkgInterval == 0`. At such a height the cycle
/// base is the block itself, so the ancestor `IsQuorumRotationEnabled` reads, at `cycleBase − 1`,
/// is `prev`: the arm needs no block of its own, and a template is judged as its block is.
// Core: llmq/blockprocessor.cpp:35-43, llmq/options.cpp:50-62
fn rotation_cycle_base<E>(
    llmq: &LlmqCtx<'_, E>,
    prev: BlockId,
    height: u32,
) -> Result<bool, LlmqError<E>> {
    let dip24 =
        llmq.deployments().active_after(Some(prev), Deployment::Buried(BuriedDeployment::Dip0024));
    Ok(enabled_quorum_params(llmq, prev)?
        .into_iter()
        .any(|p| p.use_rotation && dip24 && cycle_base_height(p, height) == height))
}

#[cfg(test)]
#[path = "guard_tests.rs"]
pub(crate) mod tests;
