//! The readiness stubs (CH7 D2): what H4 runs on before CH7b. Each refuses with
//! [`StoreError::NotBuilt`] exactly where Core would ask the capability it stands for, and answers
//! as Core does everywhere else, so every member-free check runs in Core's order.

use dash_consensus::BlockId;
use dash_dml::{Dmn, StoreError};
use dash_evo::EffectSink;
use dash_evo::llmq::{
    LlmqCtx, MemberError, PrecomputeMode, QuorumMemberSource, QuorumMembers, QuorumPrecompute,
    enabled_quorum_params, is_quorum_rotation_enabled,
};
use dash_primitives::LlmqType;
use std::sync::Arc;

/// The capability `NoMembers` refuses for (D2): members, quorums and the precompute.
pub const QUORUM_VALIDATION: &str = "quorum validation (CH7b)";

/// The members facade until CH7b-2 (D2), bound to one attempt's chain, deployments and scheme.
pub struct NoMembers<'a> {
    pub llmq: LlmqCtx<'a, StoreError>,
}

impl QuorumMemberSource for NoMembers<'_> {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    /// `GetAllQuorumMembers` (`lu:653-761`): every call refuses.
    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        Err(not_built())
    }
}

impl QuorumPrecompute for NoMembers<'_> {
    type Error = StoreError;

    // Core: llmq/blockprocessor.cpp:35-43 PreComputeQuorumMembers
    /// Core's loop, which asks the members only of a rotated type at its cycle base (§4.3): it
    /// refuses there and does nothing elsewhere, as Core's loop does, so the blocks of every
    /// other height go on to the member-free checks.
    fn precompute(
        &self,
        block: BlockId,
        _: PrecomputeMode,
        _: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        let Some(prev) = self.llmq.chain().prev(block).map_err(MemberError::View)? else {
            return Err(MemberError::Missing(block));
        };
        let height = self.llmq.chain().header(block).map_err(MemberError::View)?.height;
        for params in enabled_quorum_params(&self.llmq, prev)? {
            if is_quorum_rotation_enabled(&self.llmq, params, block)?
                && i64::from(height) % i64::from(params.dkg_interval) == 0
            {
                return Err(not_built());
            }
        }
        Ok(())
    }
}

/// H9's precompute below DIP0003 (DEC-591, D53): Core's undo precomputes the rotated members
/// there too, but no masternode exists before DIP0003, so their set is empty and `NoMembers`'
/// refusal at a rotated cycle base is a no-op. Every other outcome is `NoMembers`' own.
// Core: llmq/blockprocessor.cpp:35-44, :562-566
pub struct PreDip3Precompute<'a, P: ?Sized = NoMembers<'a>>(pub &'a P);

impl<P: QuorumPrecompute<Error = StoreError> + ?Sized> QuorumPrecompute
    for PreDip3Precompute<'_, P>
{
    type Error = StoreError;

    fn precompute(
        &self,
        block: BlockId,
        mode: PrecomputeMode,
        sink: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        match self.0.precompute(block, mode, sink) {
            Err(MemberError::View(StoreError::NotBuilt(QUORUM_VALIDATION))) => Ok(()),
            other => other,
        }
    }
}

fn not_built() -> MemberError<StoreError> {
    MemberError::View(StoreError::NotBuilt(QUORUM_VALIDATION))
}

#[cfg(test)]
#[path = "not_built_tests.rs"]
mod tests;
