//! The parameter predicates (LQ1 §3.2): which types are enabled, which rotate, and the cycle
//! arithmetic. Every ancestry read is fallible and an error is never read as `false` (EV addendum
//! §2.5).

use dash_consensus::BlockId;
use dash_consensus::ctx::{Deployment, DeploymentPos};
use dash_params::BuriedDeployment;
use dash_params::LlmqType;
use dash_params::{LlmqParams, Network};
use dash_types::Numeric;

use super::ctx::{LlmqCtx, LlmqError};
use super::members::MemberError;

/// v24's two optional arguments of `IsQuorumTypeEnabled` (`quorum dkginfo`, `rpc/quorums.cpp`
/// v24:1078); `None` reads the chain, as Core's `value_or`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TypeEnabledOverrides {
    /// `optDIP0024IsActive`.
    pub dip24_active: Option<bool>,
    /// `optHaveDIP0024Quorums`.
    pub have_dip24_quorums: Option<bool>,
}

/// Core `ChainstateManager::IsQuorumTypeEnabled(type, pindexPrev)`; `None` is a null
/// `pindexPrev` (genesis's), for which nothing is enabled.
pub fn is_quorum_type_enabled<E>(
    ctx: &LlmqCtx<'_, E>,
    llmq_type: LlmqType,
    prev: Option<BlockId>,
) -> Result<bool, LlmqError<E>> {
    is_quorum_type_enabled_with(ctx, llmq_type, prev, TypeEnabledOverrides::default())
}

/// [`is_quorum_type_enabled`] with v24's optional arguments.
// Core: v24 validation.cpp:6658 ChainstateManager::IsQuorumTypeEnabled
pub fn is_quorum_type_enabled_with<E>(
    ctx: &LlmqCtx<'_, E>,
    llmq_type: LlmqType,
    prev: Option<BlockId>,
    overrides: TypeEnabledOverrides,
) -> Result<bool, LlmqError<E>> {
    let Some(prev) = prev else {
        return Ok(false);
    };
    let deployments = ctx.deployments();
    let consensus = &ctx.params().consensus;
    let prev_height = ctx.height(prev)?;
    let dip24_active = overrides.dip24_active.unwrap_or_else(|| {
        deployments.active_after(Some(prev), Deployment::Buried(BuriedDeployment::Dip0024))
    });
    let have_dip24_quorums = overrides
        .have_dip24_quorums
        .unwrap_or(prev_height >= consensus.dip0024_quorums_height.to_u32());
    let network = ctx.params().network;
    Ok(match llmq_type {
        LlmqType::LlmqDevnet => true,
        LlmqType::Llmq50_60 => {
            !dip24_active
                || !have_dip24_quorums
                || network == Network::Testnet3
                || network == Network::Devnet
        }
        LlmqType::LlmqTestInstantsend => {
            !dip24_active
                || !have_dip24_quorums
                || consensus.llmq_type_dip0024_instant_send == LlmqType::LlmqTestInstantsend
        }
        LlmqType::LlmqTest
        | LlmqType::LlmqTestPlatform
        | LlmqType::Llmq400_60
        | LlmqType::Llmq400_85
        | LlmqType::LlmqDevnetPlatform => true,
        LlmqType::LlmqTestV17 => {
            deployments.active_after(Some(prev), Deployment::Versionbits(DeploymentPos::TestDummy))
        }
        LlmqType::Llmq100_67 => {
            deployments.active_after(Some(prev), Deployment::Buried(BuriedDeployment::Dip0020))
        }
        LlmqType::Llmq60_75 | LlmqType::LlmqDevnetDip0024 | LlmqType::LlmqTestDip0024 => {
            dip24_active
        }
        LlmqType::Llmq25_67 => {
            prev_height >= ctx.params().pins.llmq_25_67_activation_height.to_u32()
        }
        other => return Err(MemberError::UnknownLlmqType(other.to_base())),
    })
}

/// Core `IsQuorumRotationEnabled(llmqParams, pindex)`: DIP0024 active at least one block before
/// `at`'s cycle base. A cycle base below 1 is `false` with no read, as in Core.
// Core: v24 llmq/options.cpp:50 IsQuorumRotationEnabled
pub fn is_quorum_rotation_enabled<E>(
    ctx: &LlmqCtx<'_, E>,
    params: &LlmqParams,
    at: BlockId,
) -> Result<bool, LlmqError<E>> {
    if !params.use_rotation {
        return Ok(false);
    }
    let cycle_base = cycle_base_height(params, ctx.height(at)?);
    if cycle_base < 1 {
        return Ok(false);
    }
    let before = ctx.ancestor_below(at, cycle_base - 1)?;
    Ok(ctx.deployments().active_after(Some(before), Deployment::Buried(BuriedDeployment::Dip0024)))
}

/// Core `GetEnabledQuorumParams(chainman, pindex)`: the configured types enabled at `prev`, in
/// configured order, built eagerly as Core's vector is. CH7b's precompute is Core's loop over it
/// (EV addendum §2.5).
// Core: v24 llmq/options.cpp:125 GetEnabledQuorumParams
pub fn enabled_quorum_params<'a, E>(
    ctx: &LlmqCtx<'a, E>,
    prev: BlockId,
) -> Result<Vec<(LlmqType, &'a LlmqParams)>, LlmqError<E>> {
    let mut enabled = Vec::with_capacity(ctx.params().consensus.llmqs.len());
    for (llmq_type, params) in ctx.params().consensus.llmq_params() {
        if is_quorum_type_enabled(ctx, llmq_type, Some(prev))? {
            enabled.push((llmq_type, params));
        }
    }
    Ok(enabled)
}

/// Core's `EvalSpork(llmqType, spork_value)` (SPORK_21 all-connected, SPORK_23 PoSe).
// Core: v24 llmq/options.cpp:26 EvalSpork
pub fn eval_spork(llmq_type: LlmqType, spork_value: i64) -> bool {
    if spork_value == 0 {
        return true;
    }
    spork_value == 1
        && !matches!(llmq_type, LlmqType::Llmq100_67 | LlmqType::Llmq400_60 | LlmqType::Llmq400_85)
}

/// `int n = std::log2(signingActiveQuorumCount)` in rotated selection (DEC-29 D4, LQ1 §3.10):
/// the floor of the binary logarithm, which `std::log2` gives exactly on the domain
/// [`LlmqCtx::new`] checks (`1..=65536`).
///
/// # Panics
///
/// If `signing_active_quorum_count` is 0, outside that domain.
pub fn signer_index_bits(signing_active_quorum_count: u32) -> u32 {
    signing_active_quorum_count.ilog2()
}

/// `h - h % dkgInterval`: the height of the cycle base at or below `h`.
pub fn cycle_base_height(params: &LlmqParams, height: u32) -> u32 {
    height - height % dkg_interval(params)
}

/// `dkgInterval`, positive in every context ([`LlmqCtx::new`]).
pub(crate) fn dkg_interval(params: &LlmqParams) -> u32 {
    u32::from(params.dkg_interval).max(1)
}
