//! The member-source trait: one trait, owned by EV0, whose text is LQ1 §2.6 (rev3) (DEC-50(10),
//! the LQ1 design-review M1 ruling; EV2 §2.11 reproduces it).

use std::fmt;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::ctx::Deployment;
use dash_params::BuriedDeployment;
use dash_primitives::LlmqType;

use super::ctx::{LlmqCtx, LlmqError, LlmqSource, SnapshotRows};
use super::modifier::hash_modifier;
use super::params::{dkg_interval, is_quorum_rotation_enabled, is_quorum_type_enabled};
use super::rotation::compute_by_quarter_rotation;
use super::score::calculate_quorum;
use super::view::{MnEntryView, MnListView};
use crate::effect::EffectSink;

/// A quorum's members in Core's order. Rotated quorums may hold duplicates (quirk 28f), which are
/// kept.
pub type QuorumMembers<E> = Arc<[E]>;

/// Core's `llmq::utils::GetAllQuorumMembers`, for EV2b's PoSe branch, LQ2's `Verify`, QM1, DK1
/// and LQ5. LQ1 implements it; CH7b's production implementation is the process-wide members
/// facade (LQ1 §3.7.4), used inside a validation attempt through the attempt's handle (EV
/// addendum §2.6).
///
/// No `Send + Sync` supertrait (EV addendum rev5): every runtime implementor carries a V-only
/// capability, so it is `!Send`. EV0 API review Q10's reason ("shared by validation, QM1, DK1, LQ5
/// and RPC threads") is superseded by DEC-117.
pub trait QuorumMemberSource {
    /// The entry the source's lists hold (the node's `Arc<Dmn>`, SPV's SML entry).
    type Entry: MnEntryView;
    /// The source's view error; the node's is `StoreError`, SPV's `MnViewError` (EV addendum
    /// §2.3).
    type Error;

    /// Core `llmq::utils::GetAllQuorumMembers(type, base)` (`llmq/utils.cpp` v24:634-730).
    ///
    /// Members in Core's order; empty when the type is not enabled at `base.prev`, `base` has no
    /// parent, `base` is not a valid quorum base for the type, or the rotation rules produce no
    /// members.
    ///
    /// A snapshot row the computation stores is written exactly as Core's `StoreSnapshotForBlock`
    /// (an unconditional overwrite, visible to the next read; LQ1 §3.7.2). Inside a connect
    /// attempt it is also recorded in `effects` (A1 §2.10.1), so it survives a later failure.
    ///
    /// Provenance (LQ1 B-m2): each returned entry carries the state of the list it was taken
    /// from; for rotated quorums that is a list up to three cycles old. Consumers (LQ2's
    /// `membersSig` keys, DK1/LQ4's encryption keys and member index, LQ5's services) read from
    /// the returned entry and never re-resolve it by `proTxHash` in a newer list, as Core's
    /// `Verify` reads `members[i]->pdmnState->pubKeyOperator` (`llmq/commitment.cpp` v24:76-81).
    fn members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        effects: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Self::Entry>, MemberError<Self::Error>>;
}

/// Why a member computation gave no answer (EV addendum §2.2). Shared code returns it; what it
/// means is the product's: the node maps it to `NodeMemberError` (D4), SPV to a request or
/// `Unavailable` (EV addendum §2.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberError<E> {
    /// A per-block input the computation needs is not in the view: `LlmqSource::mn_list` or
    /// `coinbase_facts` returned `Ok(None)` for this block.
    ///
    /// An absent snapshot row is **not** this error: the rows interface returns `Ok(None)` only
    /// when Core would see no row, and the rotation then breaks (quirks 28a, 28e, young chains),
    /// as in Core. Unavailable block data is not one either (`CoinbaseFacts::BlockDataUnavailable`,
    /// LQ1 §3.7.3), nor is a mined commitment missing in a scan (Core's `GetQuorum` nullptr, a
    /// value, LQ1 §3.8.1). A view never answers one of those values for input it merely lacks
    /// (EV addendum §2.1).
    Missing(BlockId),
    /// The view failed (a read error, history below an anchored client's floor, a latch already
    /// tripped).
    View(E),
    /// Core's `IsQuorumTypeEnabled` throws `runtime_error` for an unknown type
    /// (`validation.cpp` v24:6707-6708); Core's catch maps it to
    /// `failed-procspectxsinblock`. Unreachable from block validation: EV1/LQ2 reject unknown
    /// types first (`bad-qc-commitment-type`).
    UnknownLlmqType(u8),
}

impl<E: fmt::Display> fmt::Display for MemberError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemberError::Missing(at) => {
                write!(f, "quorum members: an input for block id {} is not in the view", at.0)
            }
            MemberError::View(e) => write!(f, "quorum members: {e}"),
            MemberError::UnknownLlmqType(t) => write!(f, "unknown LLMQ type {t}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for MemberError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MemberError::View(e) => Some(e),
            MemberError::Missing(_) | MemberError::UnknownLlmqType(_) => None,
        }
    }
}

/// Core `llmq::WORK_DIFF_DEPTH`: the work block is this many blocks below a cycle base.
// Core: v24 llmq/snapshot.h:40 WORK_DIFF_DEPTH
pub const WORK_DIFF_DEPTH: u32 = 8;

// Core `GetAllQuorumMembers` (v24 llmq/utils.cpp:653-762) is split at its cache accesses (LQ1
// §2.5): the gate before any cache, the route after the base cache, the computations after the
// indexed cache. A members facade (CH7b's, or `reference::ModelMembersFacade`) interleaves its two
// caches with these steps; `members_uncached` composes them with no cache.

/// The gate, before any cache access: `false` when `base` has no parent or the type is not enabled
/// at `base.prev`. Core then returns `{}` without touching a cache, even with `reset_cache`.
// Core: v24 llmq/utils.cpp:670 GetAllQuorumMembers (gate)
pub fn members_gate<E>(
    ctx: &LlmqCtx<'_, E>,
    llmq_type: LlmqType,
    base: BlockId,
) -> Result<bool, LlmqError<E>> {
    match ctx.chain().prev(base).map_err(MemberError::View)? {
        None => Ok(false),
        Some(prev) => is_quorum_type_enabled(ctx, llmq_type, Some(prev)),
    }
}

/// Which computation [`members_gate`]'s passing bases take, after the base-cache step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MembersRoute {
    /// [`compute_quorum_members`] at the base.
    NonRotated,
    /// A rotated base whose `height % dkgInterval` is at least `signingActiveQuorumCount`: Core
    /// returns `{}` and inserts nothing.
    RotatedIndexOutOfRange,
    /// A rotated base: the indexed-cache key is `(hash(cycle_base), index)`; on a miss,
    /// [`compute_by_quarter_rotation`] at `cycle_base`.
    Rotated {
        /// The cycle base, `height - index`.
        cycle_base: BlockId,
        /// `height % dkgInterval`, below `signingActiveQuorumCount`.
        index: u16,
    },
}

/// The route Core's `GetAllQuorumMembers` takes for `base` after its base-cache lookup.
// Core: v24 llmq/utils.cpp:694 GetAllQuorumMembers (route)
pub fn members_route<E>(
    ctx: &LlmqCtx<'_, E>,
    llmq_type: LlmqType,
    base: BlockId,
) -> Result<MembersRoute, LlmqError<E>> {
    let params = ctx.llmq(llmq_type)?;
    if !is_quorum_rotation_enabled(ctx, params, base)? {
        return Ok(MembersRoute::NonRotated);
    }
    let height = ctx.height(base)?;
    let index = height % dkg_interval(params);
    let active = params.signing_active_quorum_count.unsigned_abs();
    if index >= active {
        return Ok(MembersRoute::RotatedIndexOutOfRange);
    }
    let cycle_base = ctx.ancestor_below(base, height - index)?;
    // `index < signingActiveQuorumCount <= 65536` (`LlmqCtx::new`), so it fits.
    let index = u16::try_from(index).map_err(|_| MemberError::Missing(base))?;
    Ok(MembersRoute::Rotated {
        cycle_base,
        index,
    })
}

/// The non-rotated branch of Core's `GetAllQuorumMembers` with `ComputeQuorumMembers`: the list of
/// the work block (`base - 8` once V20 is active after `base`, else `base`), then, at a cycle
/// base of a non-rotated type, `CalculateQuorum` with the base's modifier, `size` members, Evo
/// only for the Platform type from V19. Empty for any other `base`, as in Core, which reads the
/// list first.
///
/// With V20 active after a base below height 8, Core's work block is null; that is
/// [`MemberError::Missing`] for `base`.
// Core: v24 llmq/utils.cpp:745 GetAllQuorumMembers (non-rotated), :292 ComputeQuorumMembers
pub fn compute_quorum_members<E, L: MnListView>(
    ctx: &LlmqCtx<'_, E>,
    src: &dyn LlmqSource<L, Error = E>,
    llmq_type: LlmqType,
    base: BlockId,
) -> Result<QuorumMembers<L::Entry>, LlmqError<E>> {
    let height = ctx.height(base)?;
    let deployments = ctx.deployments();
    let work = if deployments.active_after(Some(base), Deployment::Buried(BuriedDeployment::V20)) {
        let work_height = height.checked_sub(WORK_DIFF_DEPTH).ok_or(MemberError::Missing(base))?;
        ctx.ancestor_below(base, work_height)?
    } else {
        base
    };
    let list = src.mn_list(work).map_err(MemberError::View)?.ok_or(MemberError::Missing(work))?;

    let consensus = &ctx.params().consensus;
    let evo_only = consensus.llmq_type_platform == llmq_type
        && deployments.active_after(Some(base), Deployment::Buried(BuriedDeployment::V19));
    let params = ctx.llmq(llmq_type)?;
    if params.use_rotation || height % dkg_interval(params) != 0 {
        return Ok(Arc::from([]));
    }
    let modifier = hash_modifier(ctx, src, params, base)?;
    let mut candidates = Vec::with_capacity(list.counts().total);
    list.for_each(true, &mut |e| candidates.push(e.clone()));
    let size = usize::try_from(params.size).unwrap_or(0);
    Ok(calculate_quorum(candidates, &modifier, size, evo_only).into())
}

/// Core's `GetAllQuorumMembers` with no cache: the gate, the route, and the computation it
/// selects, a rotated one storing its snapshot row through `rows` (EV addendum §4.2). For
/// consumers without Core's caches: SPV's `membersSig` input and mnxcheck's miss path.
///
/// Node consensus paths reach members only through CH7b's facade (LQ1 §3.7.4): a bypass changes
/// when snapshot rows are written. Effects are the rows' concern: an attempt's `SnapshotRows`
/// records `Effect::StoreQuorumSnapshot` itself (EV addendum §2.6).
// Core: v24 llmq/utils.cpp:653 GetAllQuorumMembers
pub fn members_uncached<E, L: MnListView>(
    ctx: &LlmqCtx<'_, E>,
    src: &dyn LlmqSource<L, Error = E>,
    rows: &mut dyn SnapshotRows<Error = E>,
    llmq_type: LlmqType,
    base: BlockId,
) -> Result<QuorumMembers<L::Entry>, LlmqError<E>> {
    if !members_gate(ctx, llmq_type, base)? {
        return Ok(Arc::from([]));
    }
    match members_route(ctx, llmq_type, base)? {
        MembersRoute::NonRotated => compute_quorum_members(ctx, src, llmq_type, base),
        MembersRoute::RotatedIndexOutOfRange => Ok(Arc::from([])),
        MembersRoute::Rotated {
            cycle_base,
            index,
        } => Ok(compute_by_quarter_rotation(ctx, src, rows, llmq_type, cycle_base)?
            .get(usize::from(index))
            .cloned()
            .unwrap_or_default()),
    }
}

/// How Core's `PreComputeQuorumMembers` runs (its `reset_cache` argument).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrecomputeMode {
    /// `reset_cache` false: the quorum `ProcessBlock` on connect, template checks, VerifyDB 4 and
    /// roll-forward.
    Connect,
    /// `reset_cache` true: `UndoBlock` on disconnect and VerifyDB 3.
    Undo,
}

/// Core's `PreComputeQuorumMembers(pindex, reset_cache)` (LQ1 §2.5, EV addendum §2.5): CH7b
/// implements it over the process-wide members facade, LQ2 calls it first in `process_block`
/// (after its DIP3 gate) and in `undo_block`. The implementation is Core's loop, line for line:
///
/// ```text
/// for p in enabled_quorum_params(ctx, prev)? {
///     if is_quorum_rotation_enabled(ctx, p, block)? && h % p.dkg_interval == 0 {
///         facade.members(p.llmq_type, block, mode == Undo, effects)?; // discarded
///     }
/// }
/// ```
///
/// `reference::ModelMembersFacade::precompute` is the model. The first error is returned; a store
/// error is fail-stop.
// Core: v24 llmq/blockprocessor.cpp:34 PreComputeQuorumMembers
pub trait QuorumPrecompute {
    /// The facade's view error.
    type Error;

    /// Computes the members of every rotated type with a cycle base at `block`, warming (or, on
    /// `Undo`, resetting and refilling) the members caches and writing the rows Core writes.
    fn precompute(
        &self,
        block: BlockId,
        mode: PrecomputeMode,
        effects: &mut dyn EffectSink,
    ) -> Result<(), MemberError<Self::Error>>;
}
