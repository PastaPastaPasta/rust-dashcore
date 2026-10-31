//! DIP24 rotation (LQ1 §3.6, quirk 28): the quarters of the three previous cycles rebuilt from
//! their snapshot rows, the new quarters with their skip list, the snapshot row Core stores, and
//! the v24 predictor.
//!
//! Every function is Core's line for line (v24 `llmq/utils.cpp`), including the integer widths
//! of the skip-list codec: its entries are `int`s narrowed from `size_t` arithmetic.

use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::ctx::Deployment;
use dash_params::LlmqType;
use dash_params::{BuriedDeployment, ChainSetup, LlmqParams, Network};

use super::ctx::{LlmqCtx, LlmqError, LlmqSource, SnapshotRows};
use super::members::{MemberError, QuorumMembers, WORK_DIFF_DEPTH};
use super::modifier::{hash_modifier, hash_modifier_from_work_block};
use super::params::dkg_interval;
use super::score::{QuorumModifier, calculate_quorum};
use super::snapshot::{QuorumSnapshot, SnapshotSkipMode};
use super::view::{MnEntryView, MnListView, ScratchMnList};

/// One cycle's quarters: quarter `i` belongs to the cycle's quorum index `i` (Core's
/// `QuorumQuarter::m_members`).
pub type CycleQuarters<X> = Vec<Vec<X>>;

/// The previous cycles a rotated quorum reads, newest first: H−C, H−2C, H−3C (Core's
/// `PreviousQuorumQuarters::GetCycles()`).
pub const PREVIOUS_CYCLES: usize = 3;

/// `signingActiveQuorumCount` as Core's `size_t`: positive in every context (`LlmqCtx::new`).
fn quorum_count(params: &LlmqParams) -> usize {
    usize::from(params.signing_active_quorum_count)
}

/// `size / 4` as Core's `size_t`.
fn quarter_size(params: &LlmqParams) -> usize {
    usize::from(params.size) / 4
}

/// `std::vector<QuorumMembers>(n)`: `n` empty quarters.
fn empty_quarters<X>(n: usize) -> CycleQuarters<X> {
    (0..n).map(|_| Vec::new()).collect()
}

/// `CalculateQuorum(list, modifier)`: every valid entry of `list`, sorted.
fn sorted_list<L: MnListView>(list: &L, modifier: &QuorumModifier) -> Vec<L::Entry> {
    let mut candidates = Vec::with_capacity(list.counts().total);
    list.for_each(true, &mut |e| candidates.push(e.clone()));
    calculate_quorum(candidates, modifier, 0, false)
}

/// `CalculateQuorum(scratch, modifier)`: every valid entry of a used set, sorted.
fn sorted_scratch<S: ScratchMnList>(set: &S, modifier: &QuorumModifier) -> Vec<S::Entry> {
    let mut candidates = Vec::with_capacity(set.total());
    set.for_each(true, &mut |e| candidates.push(e.clone()));
    calculate_quorum(candidates, modifier, 0, false)
}

/// The work block of `cycle_base`, `cycle_base − 8`. Core's is null below height 8 and
/// dereferenced; that is [`MemberError::Missing`] for `cycle_base` (LQ1a D5, LQ1b D17).
fn work_block<E>(ctx: &LlmqCtx<'_, E>, cycle_base: BlockId) -> Result<BlockId, LlmqError<E>> {
    let height = ctx.height(cycle_base)?;
    let work_height =
        height.checked_sub(WORK_DIFF_DEPTH).ok_or(MemberError::Missing(cycle_base))?;
    ctx.ancestor_below(cycle_base, work_height)
}

/// The skip-list encoder of Core's `BuildNewQuorumQuarterMembers` (quirk 28c): the first index
/// skipped at a non-zero position is pushed absolute and every later one relative to it; until
/// then each index is pushed absolute (a skip at 0 leaves the first unset). Core keeps the
/// indices in `size_t`s and narrows each entry to `int`, so entries wrap and may be negative.
/// [`decode_skip_list`] inverts it.
// Core: v24 llmq/utils.cpp:436 BuildNewQuorumQuarterMembers (skip encoding)
#[derive(Clone, Debug, Default)]
pub struct SkipListEncoder {
    first_skipped: usize,
    skip_list: Vec<i32>,
}

impl SkipListEncoder {
    /// Records a skip at `idx` of the combined list.
    pub fn push(&mut self, idx: usize) {
        if self.first_skipped == 0 {
            self.first_skipped = idx;
            self.skip_list.push(idx as i32);
        } else {
            self.skip_list.push(idx.wrapping_sub(self.first_skipped) as i32);
        }
    }

    /// The encoded list.
    pub fn finish(self) -> Vec<i32> {
        self.skip_list
    }
}

/// Decodes a mode-1 skip list into absolute indices into the combined list (quirk 28c): the
/// first entry is absolute and every later one is relative to it, except that while the first
/// entry is 0 each entry is taken as a new first. Core keeps the first entry in a `size_t`
/// (sign-extended from the `int`) and narrows each sum back to `int`, so the sums wrap.
// Core: v24 llmq/utils.cpp:257 GetQuorumQuarterMembersBySnapshot (MODE_SKIPPING_ENTRIES)
pub fn decode_skip_list(skip_list: &[i32]) -> Vec<i32> {
    let mut first = 0i32;
    skip_list
        .iter()
        .map(|&s| {
            if first == 0 {
                first = s;
                s
            } else {
                first.wrapping_add(s)
            }
        })
        .collect()
}

/// Core `GetQuorumQuarterMembersBySnapshot`: the quarters of the cycle at `cycle_base` rebuilt
/// from its snapshot row (LQ1 §3.6.2).
///
/// The combined list is the cycle's sorted work list split by `active_quorum_members`: the unused
/// non-banned entries, then the used ones. A bit past the end of the bitset (a row Core did not
/// write; Core reads past its vector) counts as unused (LQ1b D18). Mode 0 fills each quarter by
/// one wrapping cursor (duplicates possible, 28f), mode 1 the same while skipping the decoded
/// indices (the skip cursor never rewinds, 28i), and modes 2, 3 and unknown values give empty
/// quarters (28g).
// Core: v24 llmq/utils.cpp:189 GetQuorumQuarterMembersBySnapshot
pub fn quarters_by_snapshot<E, L: MnListView>(
    ctx: &LlmqCtx<'_, E>,
    src: &dyn LlmqSource<L, Error = E>,
    llmq_type: LlmqType,
    params: &LlmqParams,
    cycle_base: BlockId,
    snapshot: &QuorumSnapshot,
) -> Result<CycleQuarters<L::Entry>, LlmqError<E>> {
    if !params.use_rotation || !ctx.height(cycle_base)?.is_multiple_of(dkg_interval(params)) {
        return Ok(Vec::new());
    }

    let work = work_block(ctx, cycle_base)?;
    let list = src.mn_list(work).map_err(MemberError::View)?.ok_or(MemberError::Missing(work))?;
    let modifier = hash_modifier(ctx, src, llmq_type, params, cycle_base)?;
    let mut combined = Vec::new();
    let mut used = Vec::new();
    for (i, e) in sorted_list(&*list, &modifier).into_iter().enumerate() {
        if snapshot.active_quorum_members.get(i).copied().unwrap_or(false) {
            used.push(e);
        } else if !e.is_banned() {
            combined.push(e);
        }
    }
    combined.append(&mut used);

    let quarter_size = quarter_size(params);
    let mut quarters = empty_quarters(quorum_count(params));
    if combined.is_empty() {
        return Ok(quarters);
    }
    match snapshot.skip_mode() {
        Some(SnapshotSkipMode::NoSkipping) => {
            let mut cursor = combined.iter().cycle();
            for quarter in &mut quarters {
                quarter.extend(cursor.by_ref().take(quarter_size).cloned());
            }
        }
        Some(SnapshotSkipMode::SkippingEntries) => {
            let mut skip = decode_skip_list(&snapshot.skip_list).into_iter().peekable();
            // Core's `int idx`, compared with the decoded `int`s; `combined` is a list's
            // entries, so its length fits.
            let len = i32::try_from(combined.len()).unwrap_or(i32::MAX);
            let mut idx = 0i32;
            for quarter in &mut quarters {
                while quarter.len() < quarter_size {
                    if skip.peek() == Some(&idx) {
                        skip.next();
                    } else {
                        quarter.push(combined[idx as usize].clone());
                    }
                    idx += 1;
                    if idx == len {
                        idx = 0;
                    }
                }
            }
        }
        Some(SnapshotSkipMode::NoSkippingEntries | SnapshotSkipMode::AllSkipped) | None => {}
    }
    Ok(quarters)
}

/// What [`new_quarters`] produced: the new quarters and, unless Core returned early (28e), the
/// inputs of the snapshot row.
pub struct NewQuarters<L: MnListView> {
    /// The new quarter of each quorum index.
    pub quarters: CycleQuarters<L::Entry>,
    /// `MnsUsedAtH` and the encoded skip list; `None` on either early return (28e-1, 28e-2),
    /// where Core stores no row.
    pub snapshot_inputs: Option<(L::Scratch, Vec<i32>)>,
}

/// Core's `skipRemovedMNs` for the cycle based at `cycle_base_height` (28d): from V19 on, and on
/// testnet always. V19 is buried: Core tests the cycle base's height, which also serves a
/// predicted cycle.
// Core: v24 llmq/utils.cpp:371 BuildNewQuorumQuarterMembers
pub(crate) fn skip_removed_mns(chain: &ChainSetup, cycle_base_height: u32) -> bool {
    u64::from(cycle_base_height) + 1
        >= u64::from(chain.consensus.deployment_height(BuriedDeployment::V19).to_u32())
        || chain.network == Network::Testnet3
}

/// Core `BuildNewQuorumQuarterMembers` without the store (LQ1 §3.6.3): the new quarters of a
/// cycle whose work list is `all` and whose previous quarters are `previous` (H−C, H−2C, H−3C;
/// a cycle the rotation did not reach holds empty quarters).
///
/// `skip_removed` is Core's `skipRemovedMNs`: the previous quarters' masternodes no longer in
/// `all` stay in the used sets before V19 on every network but testnet (28d).
// Core: v24 llmq/utils.cpp:344 BuildNewQuorumQuarterMembers
pub fn new_quarters<L: MnListView>(
    params: &LlmqParams,
    all: &L,
    previous: &[CycleQuarters<L::Entry>; PREVIOUS_CYCLES],
    modifier: &QuorumModifier,
    skip_removed: bool,
) -> NewQuarters<L> {
    let n = quorum_count(params);
    let quarter_size = quarter_size(params);
    let mut quarters = empty_quarters(n);
    if all.counts().enabled < quarter_size {
        return NewQuarters {
            quarters,
            snapshot_inputs: None,
        };
    }

    let mut used = all.new_scratch();
    let mut used_by_index: Vec<L::Scratch> = (0..n).map(|_| all.new_scratch()).collect();
    for (idx, used_at_index) in used_by_index.iter_mut().enumerate() {
        for cycle in previous {
            for mn in cycle.get(idx).into_iter().flatten() {
                let pro_tx_hash = mn.pro_tx_hash();
                if (skip_removed && !all.contains(pro_tx_hash)) || all.is_pose_banned(pro_tx_hash) {
                    continue;
                }
                // Core swallows `AddMN`'s exceptions (28b).
                let _ = used.try_add(mn);
                let _ = used_at_index.try_add(mn);
            }
        }
    }

    let mut not_used = Vec::new();
    all.for_each(false, &mut |e| {
        if !used.contains(e.pro_tx_hash()) && !e.is_banned() {
            not_used.push(e.clone());
        }
    });
    let mut combined = calculate_quorum(not_used, modifier, 0, false);
    combined.extend(sorted_scratch(&used, modifier));

    let mut skips = SkipListEncoder::default();
    let mut idx = 0usize;
    for (quarter, used_at_index) in quarters.iter_mut().zip(&mut used_by_index) {
        let used_count = used_at_index.total();
        let mut updated = false;
        let initial = idx;
        while quarter.len() < quarter_size && used_count + quarter.len() < combined.len() {
            let candidate = &combined[idx];
            if !used_at_index.contains(candidate.pro_tx_hash())
                && used_at_index.try_add(candidate).is_ok()
            {
                quarter.push(candidate.clone());
                updated = true;
            } else {
                skips.push(idx);
            }
            idx += 1;
            if idx == combined.len() {
                idx = 0;
            }
            if idx == initial {
                if !updated {
                    return NewQuarters {
                        quarters: empty_quarters(n),
                        snapshot_inputs: None,
                    };
                }
                updated = false;
            }
        }
    }
    NewQuarters {
        quarters,
        snapshot_inputs: Some((used, skips.finish())),
    }
}

/// Core `BuildQuorumSnapshot` (LQ1 §3.6.4): one bit per masternode of `all` (`GetCounts().total`),
/// set at the sorted positions of the used ones; mode 0 with an empty skip list, else mode 1.
// Core: v24 llmq/utils.cpp:308 BuildQuorumSnapshot
pub fn build_quorum_snapshot<L: MnListView>(
    all: &L,
    used: &L::Scratch,
    skip_list: Vec<i32>,
    modifier: &QuorumModifier,
) -> QuorumSnapshot {
    let mut active_quorum_members = vec![false; all.counts().total];
    for (bit, e) in active_quorum_members.iter_mut().zip(sorted_list(all, modifier)) {
        *bit = used.contains(e.pro_tx_hash());
    }
    QuorumSnapshot {
        active_quorum_members,
        skip_list_mode: i32::from(!skip_list.is_empty()),
        skip_list,
    }
}

/// One rotated cycle as Core's `ComputeQuorumMembersByQuarterRotation` builds it.
struct Cycle<X> {
    /// Quorum `i` = H−3C[i] ‖ H−2C[i] ‖ H−C[i] ‖ new[i].
    quorums: Vec<QuorumMembers<X>>,
    /// The row Core stores, if it stores one.
    snapshot: Option<QuorumSnapshot>,
}

/// Core `ComputeQuorumMembersByQuarterRotation(params, work, cycleBaseHeight, modifier,
/// predicting)` (LQ1 §3.6.5), with the row it stores returned rather than stored.
///
/// `None` is Core's `{}`: parameters that do not rotate at this height, or, `predicting`, a
/// previous cycle below genesis or without a row. Otherwise the loop over previous cycles stops
/// at the first such cycle (28a, 28e), computed with no view call for a negative height (EV
/// addendum §2.5); a cycle the view does not hold is an error.
// Core: v24 llmq/utils.cpp:472 ComputeQuorumMembersByQuarterRotation
#[allow(clippy::too_many_arguments)]
fn quarter_rotation<E, L: MnListView>(
    ctx: &LlmqCtx<'_, E>,
    src: &dyn LlmqSource<L, Error = E>,
    rows: &dyn SnapshotRows<Error = E>,
    llmq_type: LlmqType,
    params: &LlmqParams,
    work: BlockId,
    cycle_base_height: u32,
    modifier: &QuorumModifier,
    predicting: bool,
) -> Result<Option<Cycle<L::Entry>>, LlmqError<E>> {
    let interval = dkg_interval(params);
    if !params.use_rotation || !cycle_base_height.is_multiple_of(interval) {
        return Ok(None);
    }
    let n = quorum_count(params);
    let all = src.mn_list(work).map_err(MemberError::View)?.ok_or(MemberError::Missing(work))?;

    let mut previous: [CycleQuarters<L::Entry>; PREVIOUS_CYCLES] =
        std::array::from_fn(|_| empty_quarters(n));
    let work_height = ctx.height(work)?;
    for (k, quarters) in (1..).zip(previous.iter_mut()) {
        // `GetAncestor` is null below genesis and above the work block.
        let height = i64::from(cycle_base_height) - i64::from(interval) * k;
        let found = match u32::try_from(height) {
            Ok(h) if h <= work_height => {
                let cycle = ctx.ancestor_below(work, h)?;
                rows.get(llmq_type, &ctx.hash(cycle)?)
                    .map_err(MemberError::View)?
                    .map(|snapshot| (cycle, snapshot))
            }
            _ => None,
        };
        let Some((cycle, snapshot)) = found else {
            if predicting {
                return Ok(None);
            }
            break;
        };
        *quarters = quarters_by_snapshot(ctx, src, llmq_type, params, cycle, &snapshot)?;
    }

    let skip_removed = skip_removed_mns(ctx.params(), cycle_base_height);
    let mut new = new_quarters(params, &*all, &previous, modifier, skip_removed);
    let snapshot = match new.snapshot_inputs {
        Some((used, skip_list)) if !predicting => {
            Some(build_quorum_snapshot(&*all, &used, skip_list, modifier))
        }
        _ => None,
    };

    let quorums = (0..n)
        .map(|i| {
            let mut quorum = Vec::new();
            for cycle in previous.iter_mut().rev() {
                if let Some(quarter) = cycle.get_mut(i) {
                    quorum.append(quarter);
                }
            }
            quorum.append(&mut new.quarters[i]);
            Arc::from(quorum)
        })
        .collect();
    Ok(Some(Cycle {
        quorums,
        snapshot,
    }))
}

/// Every quorum of the rotated cycle at `cycle_base`, by quorum index, as Core's rotated
/// `GetAllQuorumMembers` computes them on a cache miss (LQ1 §3.6.6): the work block
/// `cycle_base − 8`, the cycle base's modifier, the rotation, and the snapshot row stored
/// through `rows`, keyed by the cycle base, when Core stores one. The store is the call's last
/// step, as in Core, where nothing after it reads a row or fails.
///
/// The caller has routed `cycle_base` as a rotated cycle base
/// ([`members_route`](super::members_route)), as Core's `GetAllQuorumMembers` has: like Core's,
/// this reads the work block and the modifier first and checks neither DIP0024's activation nor
/// the route, so a type that does not rotate, or a block that is not a cycle base, gives no
/// quorums (Core's `{}`) only after those reads. Callers reach it through a members facade or
/// [`super::members::members_uncached`].
// Core: v24 llmq/utils.cpp:731 GetAllQuorumMembers (rotated miss)
pub fn compute_by_quarter_rotation<E, L: MnListView>(
    ctx: &LlmqCtx<'_, E>,
    src: &dyn LlmqSource<L, Error = E>,
    rows: &mut dyn SnapshotRows<Error = E>,
    llmq_type: LlmqType,
    cycle_base: BlockId,
) -> Result<Vec<QuorumMembers<L::Entry>>, LlmqError<E>> {
    let params = ctx.llmq(llmq_type)?;
    let cycle_base_height = ctx.height(cycle_base)?;
    let work = work_block(ctx, cycle_base)?;
    let modifier = hash_modifier(ctx, src, llmq_type, params, cycle_base)?;
    let Some(cycle) = quarter_rotation(
        ctx,
        src,
        &*rows,
        llmq_type,
        params,
        work,
        cycle_base_height,
        &modifier,
        false,
    )?
    else {
        return Ok(Vec::new());
    };
    if let Some(snapshot) = &cycle.snapshot {
        rows.store(llmq_type, &ctx.hash(cycle_base)?, snapshot).map_err(MemberError::View)?;
    }
    Ok(cycle.quorums)
}

/// Core v24 `ComputeQuorumMembersFromWorkBlock(type, work, quorumHeight)` (LQ1 §3.6.8; `quorum
/// dkginfo`): the members of the quorum at `quorum_height` predicted from its work block, which
/// may precede the quorum's own blocks. Reads rows, never stores one, and is uncached in Core too.
///
/// `None` where Core returns `nullopt`: an unconfigured type, an index out of range, a `work` that
/// is not `cycle base − 8`, V20 not active after `work`, or a rotated cycle missing a previous
/// cycle or its row.
// Core: v24 llmq/utils.cpp:590 ComputeQuorumMembersFromWorkBlock
pub fn compute_quorum_members_from_work_block<E, L: MnListView>(
    ctx: &LlmqCtx<'_, E>,
    src: &dyn LlmqSource<L, Error = E>,
    rows: &dyn SnapshotRows<Error = E>,
    llmq_type: LlmqType,
    work: BlockId,
    quorum_height: i32,
) -> Result<Option<QuorumMembers<L::Entry>>, LlmqError<E>> {
    let Some(params) = ctx.params().consensus.llmq(llmq_type) else {
        return Ok(None);
    };
    // Core's `int` arithmetic; `dkgInterval` is positive (`LlmqCtx::new`).
    let quorum_index = quorum_height % i32::from(params.dkg_interval);
    let cycle_base_height = quorum_height - quorum_index;
    let index_limit = if params.use_rotation {
        i32::from(params.signing_active_quorum_count)
    } else {
        1
    };
    if quorum_index < 0
        || quorum_index >= index_limit
        || i64::from(ctx.height(work)?) != i64::from(cycle_base_height) - i64::from(WORK_DIFF_DEPTH)
    {
        return Ok(None);
    }
    if !ctx.deployments().active_after(Some(work), Deployment::Buried(BuriedDeployment::V20)) {
        return Ok(None);
    }
    let modifier = hash_modifier_from_work_block(ctx, src, llmq_type, work)?;

    if !params.use_rotation {
        let consensus = &ctx.params().consensus;
        let evo_only = llmq_type == consensus.llmq_type_platform
            && i64::from(quorum_height) + 1
                >= i64::from(consensus.deployment_height(BuriedDeployment::V19).to_u32());
        let list =
            src.mn_list(work).map_err(MemberError::View)?.ok_or(MemberError::Missing(work))?;
        let mut candidates = Vec::with_capacity(list.counts().total);
        list.for_each(true, &mut |e| candidates.push(e.clone()));
        let size = usize::from(params.size);
        return Ok(Some(calculate_quorum(candidates, &modifier, size, evo_only).into()));
    }

    // The checks above make `cycle_base_height` at least 8 and `quorum_index` a valid index.
    let (cycle_base_height, index) =
        (cycle_base_height.unsigned_abs(), quorum_index.unsigned_abs() as usize);
    let cycle = quarter_rotation(
        ctx,
        src,
        rows,
        llmq_type,
        params,
        work,
        cycle_base_height,
        &modifier,
        true,
    )?;
    Ok(cycle.and_then(|c| c.quorums.get(index).cloned()))
}
