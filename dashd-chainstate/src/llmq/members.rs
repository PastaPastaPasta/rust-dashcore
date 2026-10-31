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
    compute_quorum_members, cycle_base_height, enabled_quorum_params, is_quorum_rotation_enabled,
    members_gate, members_route,
};
use dash_evo::{Effect, EffectSink};
use dash_primitives::{BlockHash, LlmqType};

use super::capability::Capability;
use super::facade::MembersFacade;
use super::rows::{RuntimeRows, SnapshotKey, key};

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

/// Route B's rows (EV addendum §2.6): a read is the runtime rows'; a store is Core's raw write,
/// made at once, outside any attempt. A failed write trips the node latch (D4).
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

    // Core: v24 llmq/snapshot.cpp:320 StoreSnapshotForBlock
    fn store(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        #[cfg(test)]
        self.facade.hooks.before_inline_write();
        let written = self.facade.runtime_rows().write(llmq_type, cycle_base, snapshot);
        written.inspect_err(|e| {
            self.facade.trip(format!("a quorum snapshot write: {e}"));
        })
    }
}

/// `pindex->GetBlockHash()`.
fn hash(ctx: &LlmqCtx<'_, StoreError>, id: BlockId) -> Result<BlockHash, LlmqError<StoreError>> {
    Ok(ctx.chain().header(id).map_err(MemberError::View)?.hash)
}

/// Core `GetAllQuorumMembers(type, base, reset_cache)` over the facade's caches, in Core's order:
/// the semantic gate before any cache; the base section (a reset clears the type's base cache);
/// the route; an index out of range before any indexed reset; the indexed section (a reset clears
/// the type's indexed cache, a hit is copied into the base cache); the rotation, then every index
/// of the cycle inserted; or the non-rotated computation; then the base insert. A rotation's row
/// store goes through `cap`'s rows.
///
/// # Errors
///
/// The view's fault, or an input missing for a connected block (D4); nothing is inserted.
// Core: v24 llmq/utils.cpp:634 GetAllQuorumMembers
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
            let quorums = compute_by_quarter_rotation(ctx, src, &mut *rows, llmq_type, cycle_base)?;
            let members = quorums.get(usize::from(index)).cloned().unwrap_or_default();
            let mut caches = facade.caches();
            for (i, quorum) in quorums.into_iter().enumerate() {
                // Core's index is a `size_t`; a cycle has at most 65536 quorums (`LlmqCtx::new`).
                if let Ok(i) = u16::try_from(i) {
                    caches.by_cycle_index.insert(llmq_type, (cycle_hash, i), quorum);
                }
            }
            members
        }
    };
    // :727-729
    facade.caches().by_base.insert(llmq_type, base_hash, members.clone());
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
// Core: v24 llmq/blockprocessor.cpp:34 PreComputeQuorumMembers
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
    for params in enabled_quorum_params(ctx, prev)? {
        if is_quorum_rotation_enabled(ctx, params, block)?
            && cycle_base_height(params, height) == height
        {
            let reset = mode == PrecomputeMode::Undo;
            members(cap, ctx, src, effects, params.llmq_type, block, reset)?;
        }
    }
    Ok(())
}
