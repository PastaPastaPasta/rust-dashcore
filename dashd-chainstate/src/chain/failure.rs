//! `InvalidateBlock`, the node half of `ResetBlockFailureFlags`, `PreciousBlock` (§4.6, §4.7),
//! and InstantSend's block conflicts (§5.6).
//!
//! `InvalidateBlock` is a class C operation: each iteration is one hold, with a release point
//! and Core's `LimitValidationInterfaceQueue` before it, and its final scope is one more hold.
//! `reconsiderblock`'s reset and `PreciousBlock`'s first scope are class M scopes: status and
//! table changes, then [`V::scope_end`]; their `ActivateBestChain` tails are class C.

use std::collections::BTreeMap;

use dash_chain::{ResetReport, Status, Work};
use dash_consensus::BlockId;
use dash_num::Hash256;

use super::abc::InHold;
use super::actor::V;
use super::command::IsResolution;
use super::index::entry;
use super::seams::Fatal;

/// `InvalidateBlock`'s `candidate_blocks_by_work` (`validation.cpp:3916-3933`), a multimap.
type ByWork = BTreeMap<Work, Vec<BlockId>>;

impl InHold<'_, '_> {
    /// One `InvalidateBlock` iteration's `cs_main` scope (`validation.cpp:3942-4003`) while `id`
    /// is in the active chain: the tip disconnected and marked failed, with the candidates Core
    /// adds as it goes. `Ok(None)` is Core's `false` (the disconnect failed); otherwise the
    /// disconnected block, the next `to_mark_failed`.
    fn invalidate_iteration(
        &mut self,
        id: BlockId,
        to_mark_failed: BlockId,
        by_work: &mut ByWork,
        disconnected: &mut u32,
    ) -> Result<Option<BlockId>, Fatal> {
        let walk_tip = self.tip().expect("id is in the active chain");
        // :3951-3954
        if self.hold.chain().node_index()?.best_header_walk_back(id) {
            self.idx.set_best_invalid(Some(id));
        }
        let mut pool = self.mempool.new_disconnect_pool();
        let ret = match self.disconnect_tip(Some(pool.as_mut())) {
            Ok(ret) => ret,
            Err(f) => {
                self.mempool.maybe_update_for_reorg(pool, false);
                return Err(f);
            }
        };
        // :3965
        *disconnected += 1;
        self.mempool.maybe_update_for_reorg(pool, *disconnected <= 10 && ret);
        if !ret {
            return Ok(None);
        }
        let prev = entry(self.reader, walk_tip).prev.expect("genesis is never disconnected");
        assert_eq!(self.tip(), Some(prev), "validation.cpp:3967");
        // :3969-3972
        if self.hold.chain().node_index()?.best_header_walk_back(walk_tip) {
            self.idx.set_best_invalid(Some(walk_tip));
        }
        // :3979-3988
        let mut ix = self.hold.chain().node_index()?;
        ix.modify(walk_tip, |g| g.status = Status(g.status.bits() | Status::FAILED_VALID));
        ix.mark_dirty(walk_tip);
        let marked = entry(self.reader, to_mark_failed).status;
        if prev == to_mark_failed && marked.has(Status::FAILED_VALID) {
            // Verbatim, though it holds at most in the first iteration (§4.6).
            let swapped = (marked.bits() ^ Status::FAILED_VALID) | Status::FAILED_CHILD;
            ix.modify(to_mark_failed, |g| g.status = Status(swapped));
            ix.mark_dirty(to_mark_failed);
        }
        self.idx.candidates_erase(walk_tip);
        self.idx.candidates_insert(self.reader, prev);
        // :3991-3999: the equal-or-more-work blocks no worse than the new tip.
        let floor = self.key(prev);
        for ids in by_work.range_mut(floor.work..).map(|(_, ids)| ids) {
            ids.retain(|&c| {
                let admit = self.idx.key_of(self.reader, c) >= floor;
                if admit {
                    self.idx.candidates_insert(self.reader, c);
                }
                !admit
            });
        }
        by_work.retain(|_, ids| !ids.is_empty());
        Ok(Some(walk_tip))
    }

    /// `InvalidateBlock`'s final `cs_main` scope (`validation.cpp:4015-4044`) once
    /// `to_mark_failed` has left the active chain: it is marked failed, the candidates are
    /// rescanned, and `UpdatedBlockTip` is signalled. The UI notification waits for the
    /// release (N1).
    fn invalidate_final(
        &mut self,
        to_mark_failed: BlockId,
        was_in_chain: bool,
    ) -> Result<(), Fatal> {
        let mut ix = self.hold.chain().node_index()?;
        ix.modify(to_mark_failed, |g| g.status = Status(g.status.bits() | Status::FAILED_VALID));
        ix.mark_dirty(to_mark_failed);
        ix.failed_blocks().insert(to_mark_failed);
        self.idx.candidates_erase(to_mark_failed);
        // :4028-4032
        let tip = self.tip().expect("genesis is never invalidated");
        self.readd_candidates(tip);
        self.invalid_chain_found(to_mark_failed)?;
        // :4035-4038
        let ibd = self.is_ibd();
        self.updated_block_tip(tip, None, ibd);
        // :4042-4044
        if was_in_chain {
            let prev = entry(self.reader, to_mark_failed).prev.expect("not genesis");
            self.ui.push((self.loading.sync_state(ibd), prev));
        }
        Ok(())
    }
}

impl V {
    /// Core `InvalidateBlock` (`validation.cpp:3889-4046`), inside a class C operation. `false` is
    /// Core's `false` with a valid state (genesis, a failed disconnect, or the block back in the
    /// active chain); `Err` is `AbortNode`, which V has stopped on before its caller logs (CH3b
    /// D213), or one of Core's asserts.
    pub(super) fn invalidate_block(&mut self, id: BlockId) -> Result<bool, Fatal> {
        let r = self.invalidate(id);
        self.abort_at_fault(r)
    }

    fn invalidate(&mut self, id: BlockId) -> Result<bool, Fatal> {
        debug_assert!(self.in_composite(), "m_chainstate_mutex (:3908)");
        let Some(parent) = entry(&self.reader, id).prev else {
            return Ok(false);
        };
        let mut to_mark_failed = id;
        let mut was_in_chain = false;
        let mut disconnected = 0;
        // :3916-3933
        let floor = self.idx.key_of(&self.reader, parent);
        let mut by_work = ByWork::new();
        for c in self.chain.ids() {
            let e = entry(&self.reader, c);
            if !self.chain.active_contains_private(c)
                && self.idx.key(&e) >= floor
                && self.idx.is_candidate_eligible(&e)
            {
                by_work.entry(e.chain_work).or_default().push(c);
            }
        }
        loop {
            // :3937-3940: Core's `cs_main` is free here (§2.4).
            if self.shutdown_requested() {
                break;
            }
            self.release_point()?;
            self.limit_queue()?;
            let mut h = self.hold();
            if !h.chain().active_contains_private(id) {
                let r = h.release();
                self.after_release(r);
                break;
            }
            was_in_chain = true;
            let walked =
                h.invalidate_iteration(id, to_mark_failed, &mut by_work, &mut disconnected)?;
            let r = h.release();
            self.after_release(r);
            let Some(walked) = walked else {
                return Ok(false);
            };
            to_mark_failed = walked;
        }
        // :4006
        self.check_block_index()?;
        let mut h = self.hold();
        // :4010-4013
        let interfered = h.chain().active_contains_private(to_mark_failed);
        if !interfered {
            h.invalidate_final(to_mark_failed, was_in_chain)?;
        }
        let r = h.release();
        self.after_release(r);
        Ok(!interfered)
    }

    /// `reconsiderblock`'s `cs_main` scope (`rpc:2048-2056`): Core `ResetBlockFailureFlags`.
    pub(super) fn reconsider_block(
        &mut self,
        id: BlockId,
        ignore_chainlocks: bool,
    ) -> Result<(), Fatal> {
        let report = self.chain.node_index()?.reset_failure_flags(id, ignore_chainlocks);
        self.after_reset(&report);
        self.scope_end();
        Ok(())
    }

    /// The node half of Core's `ResetBlockFailureFlags` (`validation.cpp:4186-4230`), after
    /// SB4's index half: `m_best_invalid` cleared if it was reset, the promoted children out of
    /// the candidates, and each reconsidered entry with its transactions offered to
    /// `TryAddBlockIndexCandidate`.
    pub(super) fn after_reset(&mut self, report: &ResetReport) {
        if self.idx.best_invalid().is_some_and(|b| report.reconsidered.contains(&b)) {
            self.idx.set_best_invalid(None);
        }
        for &p in &report.promoted {
            self.idx.candidates_erase(p);
        }
        let tip = self.chain.private_tip().map(|(t, _)| t.id);
        for &r in &report.reconsidered {
            if self.idx.is_candidate_eligible(&entry(&self.reader, r)) {
                self.idx.try_add_candidate(&self.reader, r, tip);
            }
        }
    }

    /// Core `PreciousBlock`'s `cs_main` scope (`validation.cpp:3862-3884`): `false` is its early
    /// return for a block with less work than the tip (`:3864-3867`), which changes nothing and
    /// skips ABC.
    pub(super) fn precious_block(&mut self, id: BlockId) -> bool {
        let tip = self.chain.private_tip().expect("init connected genesis").0.id;
        let tip_work = entry(&self.reader, tip).chain_work;
        let e = entry(&self.reader, id);
        if e.chain_work < tip_work {
            return false;
        }
        let seq = self.idx.take_precious_sequence_id(tip_work);
        self.idx.candidates_erase(id);
        self.idx.set_sequence_id(&self.reader, id, seq);
        if self.idx.is_candidate_eligible(&e) {
            self.idx.candidates_insert(&self.reader, id);
            self.idx.prune_block_index_candidates(&self.reader, tip);
        }
        self.scope_end();
        true
    }

    /// `NetInstantSend::ResolveBlockConflicts` (`is:583-646`) inside a class C operation, with
    /// InstantSend's half asked through the [`IsConflicts`](super::seams::IsConflicts) seam (D42):
    /// a ChainLocked conflict removes the lock (ChainLocks win); otherwise each conflicting block
    /// is invalidated, then reset, or passed by `ActivateBestChain` when the lock's transaction is
    /// known. Serializing Core's per-block scopes in one command is one of Core's schedules.
    pub(super) fn resolve_block_conflicts(
        &mut self,
        lock: &Hash256,
        txid: &Hash256,
    ) -> Result<IsResolution, Fatal> {
        let conflicts = self.is.retrieve_conflicts(lock);
        if conflicts.is_empty() {
            return Ok(IsResolution::NoConflict);
        }
        let mut blocks = Vec::with_capacity(conflicts.len());
        for hash in conflicts {
            let id = self.reader.lookup(&hash).ok_or_else(|| {
                Fatal::new(format!("an InstantSend conflict in unknown block {hash}"))
            })?;
            blocks.push((hash, id));
        }
        // is:587-606
        for &(hash, id) in &blocks {
            if self.chain.has_chainlock(entry(&self.reader, id).height, &hash)? {
                tracing::info!(
                    "NetInstantSend::ResolveBlockConflicts -- txid={txid}, islock={lock}: at \
                     least one conflicted TX already got a ChainLock"
                );
                self.is.remove_conflicting_lock(lock);
                return Ok(IsResolution::LockRemoved);
            }
        }
        // is:608-641
        let has_tx = self.is.has_tx_for_lock(lock);
        for (hash, id) in blocks {
            self.is.clear_conflicting(lock, &hash);
            tracing::info!("NetInstantSend::ResolveBlockConflicts -- invalidating block {hash}");
            // is:620-624
            let invalidated = self.invalidate_block(id);
            assert_succeeds(
                "NetInstantSend::ResolveBlockConflicts -- InvalidateBlock",
                invalidated,
            )?;
            if !has_tx {
                tracing::info!("NetInstantSend::ResolveBlockConflicts -- resetting block {hash}");
                self.reconsider_block(id, false)?;
            }
        }
        // is:634-641
        if has_tx {
            let activated = self.activate_best_chain(None).map(|()| true);
            assert_succeeds(
                "NetInstantSend::ResolveBlockConflicts -- ActivateBestChain",
                activated,
            )?;
        }
        Ok(IsResolution::Invalidated {
            activated: has_tx,
        })
    }
}

/// Core's answer to a failed call it cannot go on from (CH3b D214): `what` logs its state and
/// then `assert(false)`s (`is:621-623`, `:637-639`, `v:4065-4067`), for an `AbortNode`'s `false`
/// too, so the process ends with no shutdown flush (D62a). A `false` with a valid state reads
/// "Valid", and V's stop reports the line, as at its other asserts. An `AbortNode`'s reads its
/// message (`v:2239-2243`, `consensus/validation.h:92-115`). `r`'s `AbortNode` must be one V has
/// stopped on where it arose, which reported it ([`V::abort_at_fault`], D213), so the line is
/// logged here and the assert is marked reported, and `run`'s stop only trips the latch (L-3).
/// Other asserts pass through as they are.
pub(super) fn assert_succeeds(what: &str, r: Result<bool, Fatal>) -> Result<(), Fatal> {
    match r {
        Ok(true) => Ok(()),
        Ok(false) => Err(Fatal::assert(format!("{what} failed: Valid"))),
        Err(f) if f.is_abort_node() => {
            let line = format!("{what} failed: {}", f.0);
            tracing::info!("{line}");
            Err(Fatal::assert(line).into_reported())
        }
        Err(f) => Err(f),
    }
}
