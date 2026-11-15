//! The facade's members call and precompute loop (CH7 §4.2.1-§4.2.2; EV addendum §2.5): Core's
//! `GetAllQuorumMembers` over the facade's two caches, LQ1's pure steps between them, and
//! `PreComputeQuorumMembers`' loop over it. A call runs under a capability's gate, and takes the
//! cache mutex only around each cache access, never across a computation (D78).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_dml::{Dmn, StoreError};
use dash_evo::llmq::{
    LlmqCtx, LlmqError, LlmqSource, MemberError, MembersRoute, MnListView, PrecomputeMode,
    QuorumMembers, QuorumSnapshot, SnapshotRows, compute_by_quarter_rotation,
    compute_by_quarter_rotation_traced, compute_quorum_members, cycle_base_height,
    enabled_quorum_params, is_quorum_rotation_enabled, members_gate, members_route,
};
use dash_evo::{Effect, EffectSink};
use dash_params::LlmqType;
use dash_primitives::BlockHash;

use super::capability::Capability;
use super::facade::MembersFacade;
use super::rows::{RuntimeRows, SnapshotKey, key};
use crate::evo::node::MemberLines;

/// An attempt call's rows (EV addendum §2.6, route C): a read sees the attempt's overlay, then the
/// runtime rows; a store overwrites the overlay and goes to the attempt's log, which is applied
/// to the runtime rows before the attempt is released.
pub(crate) struct AttemptRows<'a> {
    pub(super) overlay: &'a RefCell<BTreeMap<SnapshotKey, QuorumSnapshot>>,
    pub(super) backing: &'a RuntimeRows,
    pub(super) sink: &'a mut dyn EffectSink,
}

impl SnapshotRows for AttemptRows<'_> {
    type Error = StoreError;

    fn get(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError> {
        let staged = self.overlay.borrow().get(&key(llmq_type, cycle_base)).cloned();
        match staged {
            Some(snapshot) => Ok(Some(snapshot)),
            None => self.backing.snapshot_for_block(llmq_type, cycle_base),
        }
    }

    fn store(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        self.overlay.borrow_mut().insert(key(llmq_type, cycle_base), snapshot.clone());
        self.sink.emit(Effect::StoreQuorumSnapshot {
            llmq_type,
            cycle_base: *cycle_base,
            snapshot: snapshot.clone(),
        });
        Ok(())
    }
}

/// A block template's rows (MN0-3 C0): each key read once per turn over the runtime rows, leaving
/// their cache as it was ([`RuntimeRows::snapshot_unstamped`]); a store lands in the memo only,
/// emits no effect and writes nothing. Dropped with the assembler's turn, so the rows Core's
/// `CreateNewBlock` stores while it assembles are discarded; those its `TestBlockValidity` stores
/// are written by V's proposal path (MN0-3 notes 3-D4).
pub(crate) struct TemplateRows<'a> {
    memo: RefCell<BTreeMap<SnapshotKey, Option<QuorumSnapshot>>>,
    backing: &'a RuntimeRows,
}

impl<'a> TemplateRows<'a> {
    pub(crate) fn new(backing: &'a RuntimeRows) -> Self {
        Self {
            memo: RefCell::new(BTreeMap::new()),
            backing,
        }
    }
}

impl SnapshotRows for &TemplateRows<'_> {
    type Error = StoreError;

    fn get(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError> {
        let k = key(llmq_type, cycle_base);
        if let Some(answer) = self.memo.borrow().get(&k) {
            return Ok(answer.clone());
        }
        let answer = self.backing.snapshot_unstamped(llmq_type, cycle_base)?;
        self.memo.borrow_mut().insert(k, answer.clone());
        Ok(answer)
    }

    fn store(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        self.memo.borrow_mut().insert(key(llmq_type, cycle_base), Some(snapshot.clone()));
        Ok(())
    }
}

/// [`members`] for a block template (MN0-3 C0): Core's computation over `rows`, without the
/// facade's caches, which a template neither reads nor fills. Core's caches only memoise this
/// computation's answer, so the members are the same.
///
/// # Errors
///
/// As [`members`]'s.
// Core: v24.0.0-rc.5 llmq/utils.cpp:634 GetAllQuorumMembers
pub(crate) fn template_members<L: MnListView<Entry = Arc<Dmn>>>(
    ctx: &LlmqCtx<'_, StoreError>,
    src: &dyn LlmqSource<L, Error = StoreError>,
    rows: &TemplateRows<'_>,
    llmq_type: LlmqType,
    base: BlockId,
) -> Result<QuorumMembers<Arc<Dmn>>, LlmqError<StoreError>> {
    if !members_gate(ctx, llmq_type, base)? {
        return Ok(Arc::from([]));
    }
    Ok(match members_route(ctx, llmq_type, base)? {
        MembersRoute::NonRotated => compute_quorum_members(ctx, src, llmq_type, base)?,
        MembersRoute::RotatedIndexOutOfRange => Arc::from([]),
        MembersRoute::Rotated {
            cycle_base,
            index,
        } => {
            let mut rows = rows;
            let quorums = compute_by_quarter_rotation(ctx, src, &mut rows, llmq_type, cycle_base)?;
            quorums.get(usize::from(index)).cloned().unwrap_or_default()
        }
    })
}

/// Route B's rows (EV addendum §2.6): a read is the runtime rows'; a store is Core's raw write,
/// made at once, outside any attempt. A failed write trips the node latch (D4), and the store has
/// reported it once already (BL-546's `write_failed`).
pub(crate) struct InlineRows<'a> {
    pub(super) facade: &'a MembersFacade,
}

impl SnapshotRows for InlineRows<'_> {
    type Error = StoreError;

    fn get(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError> {
        self.facade.runtime_rows().snapshot_for_block(llmq_type, cycle_base)
    }

    // Core: v24.0.0 llmq/snapshot.cpp:320 StoreSnapshotForBlock
    fn store(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        #[cfg(test)]
        self.facade.hooks.before_inline_write();
        let written = self.facade.runtime_rows().write(llmq_type, cycle_base, snapshot);
        written.map_err(|f| {
            self.facade.trip(format!("a quorum snapshot write: {}", f.0));
            StoreError::read(f.0)
        })
    }
}

/// `pindex->GetBlockHash()`.
pub(super) fn hash(
    ctx: &LlmqCtx<'_, StoreError>,
    id: BlockId,
) -> Result<BlockHash, LlmqError<StoreError>> {
    Ok(ctx.chain().header(id).map_err(MemberError::View)?.hash)
}

/// Core `GetAllQuorumMembers(type, base, reset_cache)` over the facade's caches, in Core's order:
/// the semantic gate before any cache; the base section (a reset clears the type's base cache);
/// the route; an index out of range before any indexed reset; the indexed section (a reset clears
/// the type's indexed cache, a hit is copied into the base cache); the rotation, then every index
/// of the cycle inserted; or the non-rotated computation; then the base insert. A rotation's row
/// store goes through `cap`'s rows.
///
/// Members computed while `src` made up a list ([`LlmqSource::made_up_lists`] grew: a work block
/// with no rows took the initial snapshot) are returned but enter neither cache: the facade is
/// shared by every view, and another view with that block's rows would read them (OD-CP-51,
/// DEC-1073). The quorum snapshot rows a rotation stores go through `cap`'s rows as Core's do.
///
/// A rotation logs Core's member-computation lines ([`MemberLines`], LQ2 D28), whoever called:
/// only a computation logs, never a cache hit.
///
/// # Errors
///
/// The view's fault, or an input missing for a connected block (D4); nothing is inserted.
// Core: v24.0.0 llmq/utils.cpp:634 GetAllQuorumMembers
pub(crate) fn members<C: Capability + ?Sized, L: MnListView<Entry = Arc<Dmn>>>(
    cap: &C,
    ctx: &LlmqCtx<'_, StoreError>,
    src: &dyn LlmqSource<L, Error = StoreError>,
    effects: &mut dyn EffectSink,
    llmq_type: LlmqType,
    base: BlockId,
    reset: bool,
) -> Result<QuorumMembers<Arc<Dmn>>, LlmqError<StoreError>> {
    // :644-647
    if !members_gate(ctx, llmq_type, base)? {
        return Ok(Arc::from([]));
    }
    let facade = cap.facade();
    let base_hash = hash(ctx, base)?;
    // :650-660
    {
        let mut caches = facade.caches();
        if reset {
            caches.by_base.clear(llmq_type);
        } else if let Some(members) = caches.by_base.get(llmq_type, &base_hash) {
            return Ok(members.clone());
        }
    }
    let made_up = src.made_up_lists();
    let members = match members_route(ctx, llmq_type, base)? {
        // :715-725
        MembersRoute::NonRotated => compute_quorum_members(ctx, src, llmq_type, base)?,
        // :679-682
        MembersRoute::RotatedIndexOutOfRange => return Ok(Arc::from([])),
        MembersRoute::Rotated {
            cycle_base,
            index,
        } => {
            let cycle_hash = hash(ctx, cycle_base)?;
            // :691-699
            {
                let mut caches = facade.caches();
                if reset {
                    caches.by_cycle_index.clear(llmq_type);
                } else if let Some(members) =
                    caches.by_cycle_index.get(llmq_type, &(cycle_hash, index)).cloned()
                {
                    caches.by_base.insert(llmq_type, base_hash, members.clone());
                    return Ok(members);
                }
            }
            // :701-714
            let mut rows = cap.rows(effects);
            let quorums = compute_by_quarter_rotation_traced(
                ctx,
                src,
                &mut *rows,
                llmq_type,
                cycle_base,
                &MemberLines,
            )?;
            let members = quorums.get(usize::from(index)).cloned().unwrap_or_default();
            if src.made_up_lists() == made_up {
                let mut caches = facade.caches();
                for (i, quorum) in quorums.into_iter().enumerate() {
                    // Core's index is a `size_t`; a cycle has at most 65536 quorums
                    // (`LlmqCtx::new`).
                    if let Ok(i) = u16::try_from(i) {
                        caches.by_cycle_index.insert(llmq_type, (cycle_hash, i), quorum);
                    }
                }
            }
            members
        }
    };
    // :727-729
    if src.made_up_lists() == made_up {
        facade.caches().by_base.insert(llmq_type, base_hash, members.clone());
    }
    Ok(members)
}

/// Core `PreComputeQuorumMembers(block, reset_cache)`: [`members`] for every type enabled at
/// `block`'s parent, in configured order, that rotates at `block` with `block` a cycle base, the
/// members discarded. The enabled types are read first, as Core builds its vector; the rotation
/// predicate is read before the height test, as Core's `&&` evaluates them. A parentless block
/// enables no type.
///
/// # Errors
///
/// The first member call's.
// Core: v24.0.0 llmq/blockprocessor.cpp:34 PreComputeQuorumMembers
pub(crate) fn precompute<C: Capability + ?Sized, L: MnListView<Entry = Arc<Dmn>>>(
    cap: &C,
    ctx: &LlmqCtx<'_, StoreError>,
    src: &dyn LlmqSource<L, Error = StoreError>,
    effects: &mut dyn EffectSink,
    block: BlockId,
    mode: PrecomputeMode,
) -> Result<(), LlmqError<StoreError>> {
    let Some(prev) = ctx.chain().prev(block).map_err(MemberError::View)? else {
        return Ok(());
    };
    let height = ctx.chain().header(block).map_err(MemberError::View)?.height;
    for (llmq_type, params) in enabled_quorum_params(ctx, prev)? {
        if is_quorum_rotation_enabled(ctx, params, block)?
            && cycle_base_height(params, height) == height
        {
            let reset = mode == PrecomputeMode::Undo;
            members(cap, ctx, src, effects, llmq_type, block, reset)?;
        }
    }
    Ok(())
}
