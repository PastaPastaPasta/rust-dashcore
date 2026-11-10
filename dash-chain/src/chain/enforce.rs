//! ChainLocks on the index (SB4 §6.2, §7): Core's `Chainlocks` state, the handler's observed
//! SPORK_19 flag, `EnforceBlock`, `MarkConflictingBlock`, the index half of
//! `ResetBlockFailureFlags`, and SPV's fork target.

use std::cmp::Reverse;

use dash_consensus::BlockId;

use super::{HeaderChain, HoldGuard};
use crate::Work;
use crate::chainlock::{
    ChainLockRef, ClActiveState, ClOutcome, ClPersisted, EnforceReport, FloorRelation, ForkTarget,
    ResetReport,
};
use crate::config::ChainMode;
use crate::error::{ApplyError, ChainError};
use crate::feed::Fanout;
use crate::host::{TipHost, VirtualTip};
use crate::residency::Residency;
use crate::status::Status;
use crate::store::HeaderStore;
use crate::tip::BlockRef;
use crate::walk;

impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    // ---- Core's Chainlocks state and the handler's isEnabled (§7.1, §7.5) --------------------

    /// The index side of `ProcessNewChainLock` (`chainlock/handler.cpp:133-183`), after
    /// signature verification (§7.1): `CheckActiveState`, `UpdateBestChainlock` with the block
    /// lookup, and for a known block the scheduled `CheckActiveState` + `EnforceBestChainLock`.
    /// SPV's composition (CH1-A5); the node runs the three in separate commands.
    // Core: chainlock/handler.cpp:133-183 ProcessNewChainLock
    pub fn apply_chainlock<H: TipHost<S, R>>(
        &mut self,
        cl: ChainLockRef,
        host: &mut H,
    ) -> Result<ClOutcome, ApplyError<H::Error>> {
        self.check_cl_active_state();
        let outcome = self.update_best_chainlock(cl)?;
        let ClOutcome::Floor {
            id,
            ..
        } = outcome
        else {
            return Ok(outcome);
        };
        self.check_cl_active_state();
        let enforced = self.enforce_best_chainlock(host)?;
        Ok(ClOutcome::Floor {
            id,
            enforced,
        })
    }

    /// Node: `UpdateBestChainlock` and the block lookup (`chainlock/chainlock.cpp:106-132`), with
    /// no spork gate and no enforcement (CH1-A5): a moved floor is `Floor { id, enforced: None }`,
    /// and V enforces in a later command, as Core does (CH1-node §5.1). `apply_chainlock` stays
    /// SPV's composition. Public through [`NodeIndex`](super::NodeIndex).
    ///
    /// # Panics
    ///
    /// In SPV, whose lookups can fail: it uses `apply_chainlock`.
    pub(crate) fn record_chainlock(&mut self, cl: ChainLockRef) -> ClOutcome {
        assert_eq!(self.cfg.mode, ChainMode::Node, "record_chainlock is the node's (CH1-A5)");
        self.update_best_chainlock(cl).expect("node lookups are resident")
    }

    /// Core `UpdateBestChainlock` with `ProcessNewChainLock`'s precheck and lookup before it
    /// (`handler.cpp:147-151`, `:166-168`): a lock at or below the best one is `Stale` before any
    /// read; otherwise it becomes the best lock, and the floor too when its block is known at the
    /// lock's height.
    // Core: chainlock/chainlock.cpp:106-132 Chainlocks::UpdateBestChainlock
    pub(super) fn update_best_chainlock(
        &mut self,
        cl: ChainLockRef,
    ) -> Result<ClOutcome, ChainError> {
        // A cleared best lock has height −1 (`chainlock/chainlock.h:25`).
        if self.cl_best.is_some_and(|best| cl.height <= best.height) {
            return Ok(ClOutcome::Stale);
        }
        let found = match self.shared.try_lookup(&cl.hash)? {
            Some(id) => Some(self.block_ref_of(id)?),
            None => None,
        };
        self.cl_best = Some(cl);
        let Some(at) = found else {
            return Ok(ClOutcome::Pending);
        };
        if at.height != cl.height {
            return Ok(ClOutcome::HeightMismatch);
        }
        self.cl_floor = Some(at);
        Ok(ClOutcome::Floor {
            id: at.id,
            enforced: None,
        })
    }

    /// Core `CheckActiveState` (`chainlock/handler.cpp:209-222`, §7.5): refreshes the observed
    /// flag from the live spork; off → on runs `ResetChainlock` (`chainlock/chainlock.cpp:98-104`),
    /// which clears the best lock and the floor and touches no index entry. On → off clears
    /// nothing.
    pub fn check_cl_active_state(&mut self) -> ClActiveState {
        let live = self.svc.sporks.chainlocks_enabled();
        match (std::mem::replace(&mut self.cl_enabled_observed, live), live) {
            (false, true) => {
                self.cl_best = None;
                self.cl_floor = None;
                ClActiveState::TurnedOnReset
            }
            (true, false) => ClActiveState::TurnedOff,
            _ => ClActiveState::Unchanged,
        }
    }

    /// Core `bestChainLockBlockIndex`.
    pub fn cl_floor(&self) -> Option<BlockId> {
        self.cl_floor.map(|f| f.id)
    }

    /// The ChainLock handler's cached `isEnabled`.
    pub fn cl_enabled_observed(&self) -> bool {
        self.cl_enabled_observed
    }

    /// SPV: the ChainLock rows and the observed spork state for the WriteSet (§7.5).
    pub fn cl_state(&self) -> ClPersisted {
        ClPersisted {
            best: self.cl_best,
            floor: self.cl_floor(),
            enabled_observed: self.cl_enabled_observed,
        }
    }

    /// SPV: restores the persisted ChainLock rows and observed flag at `open` (D30; SB5 §4's
    /// meta step), and publishes them in a status snapshot. The floor must be a held entry.
    pub fn restore_cl_state(&mut self, cl: ClPersisted) -> Result<(), ChainError> {
        self.require_mode(ChainMode::Spv, "restore_cl_state (SPV only)")?;
        let mut st = self.lock_tip("restore_cl_state inside a hold")?;
        let floor = match cl.floor {
            Some(id) if id.0 < self.next_id => Some(self.block_ref_of(id)?),
            Some(id) => return Err(ChainError::UnknownBlock(id)),
            None => None,
        };
        (self.cl_best, self.cl_floor, self.cl_enabled_observed) =
            (cl.best, floor, cl.enabled_observed);
        self.publish_locked(&mut st, Fanout::None);
        Ok(())
    }

    // ---- EnforceBestChainLock, EnforceBlock, MarkConflictingBlock (§7.2, §7.3) ---------------

    /// `EnforceBestChainLock`'s `EnforceBlock` half (`chainlock/handler.cpp:273-297`): `None`
    /// only when skipped, while the observed state is off or there is no floor (`:275`,
    /// `:284`); an enforcement that changed nothing is `Some` (CH1-A7).
    pub fn enforce_best_chainlock<H: TipHost<S, R>>(
        &mut self,
        host: &mut H,
    ) -> Result<Option<EnforceReport>, ApplyError<H::Error>> {
        if !self.cl_enabled_observed {
            return Ok(None);
        }
        let Some(floor) = self.cl_floor else {
            return Ok(None);
        };
        self.enforce_block(floor.id, host).map(Some)
    }

    /// Core `EnforceBlock`. The node runs it as one critical section of the tip mutex, as Core
    /// holds `cs_main` from `validation.cpp:4053` (§9.1, R4-M2): the tip state moves into the
    /// writer for the walk (D-C6-1), so the host's `chain.disconnect_tip()` steps on it, each
    /// step with its own `seq`, its feed handoff and no snapshot; one snapshot is stored when the
    /// walk ends, also after an error, before the mutex is released. A walk that unwinds puts the
    /// state back and trips the latch ([`Composite`]). SPV moves only the host's cursor.
    // Core: validation.cpp:4048-4078 Chainstate::EnforceBlock
    fn enforce_block<H: TipHost<S, R>>(
        &mut self,
        locked: BlockId,
        host: &mut H,
    ) -> Result<EnforceReport, ApplyError<H::Error>> {
        if self.cfg.mode == ChainMode::Spv {
            return self.enforce_walk(locked, host);
        }
        debug_assert!(!self.held, "enforce_block inside a hold (CH1-A1)");
        let mut guard = self.lock_tip("enforce_block inside a hold")?;
        self.feed_shared().hold_opened();
        let vacant = guard.vacant();
        self.composite = Some(std::mem::replace(&mut *guard, vacant));
        let mut c = Composite {
            chain: self,
            guard: HoldGuard::new(guard),
            done: false,
        };
        let report = c.chain.enforce_walk(locked, host);
        **c.guard = c.chain.composite.take().expect("the walk keeps the composite state");
        c.chain.publish_locked(&mut c.guard, Fanout::None);
        c.done = true;
        report
    }

    /// `EnforceBlock`'s walk: from the locked block down to the host's chain, every sibling of
    /// the path is marked (id order, D9), then a locked block that is not `IsValid()` has its
    /// failure flags reset (node only, D18).
    fn enforce_walk<H: TipHost<S, R>>(
        &mut self,
        locked: BlockId,
        host: &mut H,
    ) -> Result<EnforceReport, ApplyError<H::Error>> {
        let mut report = EnforceReport {
            floor: locked,
            marked: Vec::new(),
            disconnected: Vec::new(),
            reset: None,
        };
        let (mut walk, mut n) = (locked, self.node(locked)?);
        while !host.contains(self, walk)? {
            // An unconnected walk ends at the lowest held entry: the host's chain always holds it.
            let (prev, pn) = self.parent(&n)?.ok_or(ChainError::Internal(
                "EnforceBlock walked below every chain the host describes",
            ))?;
            let mut sibs = self.children(prev)?;
            while let Some(sib) = sibs.next(self)? {
                if sib != walk {
                    self.mark_conflicting_block(sib, host, &mut report.disconnected)?;
                    report.marked.push(sib);
                }
            }
            (walk, n) = (prev, pn);
        }
        // Core: validation.cpp:4075 IsValid() with the default nUpTo
        let status = self.writer_status(locked)?;
        match self.cfg.mode {
            ChainMode::Node if !status.is_valid(Status::VALID_TRANSACTIONS) => {
                report.reset = Some(self.reset_failure_flags(locked, false));
            }
            ChainMode::Node => {}
            // D18: SPV entries never carry a FAILED bit, so Core's reset would change nothing.
            ChainMode::Spv => debug_assert!(!status.has(Status::FAILED_MASK), "a FAILED SPV entry"),
        }
        Ok(report)
    }

    /// Core `MarkConflictingBlock`: disconnects the host's chain down past `b`, walking
    /// `best_header` back at Core's two sites, marks the disconnected entries and `b`
    /// `CONFLICT_CHAINLOCK`, then hands Core's remaining per-call work to the host (O-M2).
    /// Statuses are written as §3.7 says: seqlocked and not dirty in the node (D15), the
    /// overlay in SPV.
    // Core: validation.cpp:4080-4150 Chainstate::MarkConflictingBlock
    pub(super) fn mark_conflicting_block<H: TipHost<S, R>>(
        &mut self,
        b: BlockId,
        host: &mut H,
        disconnected: &mut Vec<BlockId>,
    ) -> Result<(), ApplyError<H::Error>> {
        let mut walk_tip = host.tip(self);
        self.walk_back_best_header(b)?;
        let mut was_in_chain = false;
        while host.contains(self, b)? {
            let old_tip = host.tip(self);
            was_in_chain = true;
            host.disconnect_tip(self).map_err(ApplyError::Host)?;
            // DEC-187: SPV's list is the `VirtualTip`'s; only the node's host keeps none.
            if self.cfg.mode == ChainMode::Node {
                disconnected.push(old_tip);
            }
            self.walk_back_best_header(old_tip)?;
        }
        if was_in_chain {
            let (b_height, mut n) = (self.height_of(b)?, self.node(walk_tip)?);
            while walk_tip != b {
                // Down past `b`'s height without meeting it: a parent on another branch.
                if n.height <= b_height {
                    return Err(self.off_height(walk_tip).into());
                }
                self.set_status(walk_tip, mark)?;
                (walk_tip, n) = self.parent(&n)?.ok_or_else(|| self.off_height(walk_tip))?;
            }
        }
        self.set_status(b, mark)?;
        host.conflicting_marked(self, b, was_in_chain).map_err(ApplyError::Host)?;
        Ok(())
    }

    /// `if (marked == m_best_header) m_best_header = m_best_header->pprev`, resident or cold. The
    /// lowest held entry stays, as R5 keeps `best_header` non-null (D-C7-3). `true` when
    /// `marked` was the best header.
    // Core: validation.cpp:3951, :3969, :4089, :4107
    pub(super) fn walk_back_best_header(&mut self, marked: BlockId) -> Result<bool, ChainError> {
        if self.best_header() != marked {
            return Ok(false);
        }
        if let Some(prev) = self.node(marked)?.prev {
            self.best_header = Some(self.facts_of(prev)?);
        }
        Ok(true)
    }

    /// The index half of `ResetBlockFailureFlags` (`validation.cpp:4152-4220`, §7.4), node only
    /// (D18). Loop 1 scans the whole index in id order (D9): every descendant-or-self that is not
    /// `IsValid()` loses its FAILED bits (and its CONFLICT mark only when `ignore_chainlocks`),
    /// is dirtied and leaves `m_failed_blocks`. Loop 2 walks to genesis: every entry with a
    /// FAILED or CONFLICT bit does the same, and its `FAILED_CHILD` children become
    /// `FAILED_VALID` failed blocks. `m_best_invalid` and the candidates are CH1-node's. Public
    /// through [`NodeIndex`](super::NodeIndex).
    ///
    /// # Panics
    ///
    /// In SPV, or for an id the index does not hold.
    // Core: validation.cpp:4152-4220 Chainstate::ResetBlockFailureFlags
    pub(crate) fn reset_failure_flags(
        &mut self,
        id: BlockId,
        ignore_chainlocks: bool,
    ) -> ResetReport {
        assert_eq!(self.cfg.mode, ChainMode::Node, "SPV never resets failure flags (D18)");
        let height = self.height_of(id).expect("reset_failure_flags: no entry");
        let status = |c: &Self, e| c.writer_status(e).expect("resident");
        let cleared = Status::FAILED_MASK
            | if ignore_chainlocks {
                Status::CONFLICT_CHAINLOCK
            } else {
                0
            };
        let mut report = ResetReport::default();
        let reader = self.reader();
        let descendants: Vec<BlockId> = self
            .ids()
            .filter(|&e| {
                !status(self, e).is_valid(Status::VALID_TRANSACTIONS)
                    && reader.ancestor(e, height) == Some(id)
            })
            .collect();
        for e in descendants {
            self.reconsider(e, cleared, &mut report);
        }
        let mut walk = Some(id);
        while let Some(p) = walk {
            if status(self, p).has(Status::FAILED_MASK | Status::CONFLICT_CHAINLOCK) {
                self.reconsider(p, cleared, &mut report);
                for c in self.links.children(p) {
                    let child = status(self, c);
                    if child.has(Status::FAILED_CHILD) {
                        self.modify(c, |g| g.status = Status(child.bits() | Status::FAILED_VALID));
                        self.failed.insert(c);
                        self.mark_dirty(c);
                        report.promoted.push(c);
                    }
                }
            }
            walk = self.shared.arena.imm(p, |e| e.prev).expect("resident");
        }
        report
    }

    /// One reconsidered entry: its `cleared` bits dropped, dirty, out of `m_failed_blocks`.
    fn reconsider(&mut self, e: BlockId, cleared: u32, report: &mut ResetReport) {
        self.modify(e, |g| g.status = Status(g.status.bits() & !cleared));
        self.mark_dirty(e);
        report.reconsidered.push(e);
        self.failed.remove(e);
    }

    // ---- SPV's enforcement view and fork target (§6.2, §7.3) ---------------------------------

    /// The step's enforcement view (§9.1 stage 1, O-m4): a cursor at the writer's tip.
    ///
    /// # Panics
    ///
    /// Before the node has a tip (CH1-A11).
    pub fn virtual_tip(&self) -> VirtualTip {
        debug_assert_eq!(self.cfg.mode, ChainMode::Spv, "the node's host is V (§9.1)");
        VirtualTip {
            cursor: self.tip.expect("a tip to enforce against").at.id,
            disconnected: Vec::new(),
            notified_tips: Vec::new(),
        }
    }

    /// Whether `id` is on `cursor`'s chain: `height(id) <= height(cursor)` and
    /// `ancestor(cursor, height(id)) == id`. A cursor that only ever moved down from the tip is
    /// on the active chain, which answers at once. The skip-link walk (resident or cold) answers
    /// a cursor off the active chain, or a height below SPV's window over it (DEC-174).
    pub(crate) fn on_cursor_chain(&self, cursor: BlockId, id: BlockId) -> Result<bool, ChainError> {
        let (h, top) = (self.height_of(id)?, self.height_of(cursor)?);
        if h > top {
            return Ok(false);
        }
        if self.active.get(top) == Some(cursor)
            && let Some(at) = self.active.get(h)
        {
            return Ok(at == id);
        }
        Ok(self.ancestor_of(cursor, h)? == Some(id))
    }

    /// The cursor's parent, for `VirtualTip::disconnect_tip`.
    pub(crate) fn cursor_prev(&self, cursor: BlockId) -> Result<BlockId, ChainError> {
        self.node(cursor)?
            .prev
            .ok_or(ChainError::Internal("disconnect below the lowest held entry"))
    }

    /// `GetAncestor` over skip links, resident or cold.
    pub(super) fn ancestor_of(
        &self,
        id: BlockId,
        height: u32,
    ) -> Result<Option<BlockId>, ChainError> {
        walk::ancestor(id, height, |x| self.node(x).map(|n| n.link()), |x| self.off_height(x))
    }

    /// `LastCommonAncestor`, resident or cold.
    fn lca(&self, a: BlockId, b: BlockId) -> Result<BlockId, ChainError> {
        walk::last_common_ancestor(
            a,
            b,
            |x| self.node(x).map(|n| n.link()),
            |x| self.off_height(x),
        )?
        .ok_or(ChainError::Internal("two held entries with no common ancestor"))
    }

    /// Whether `c` conflicts with `floor`: it is neither the floor, an ancestor of it, nor a
    /// descendant of it (DEC-136).
    fn conflicts(&self, c: BlockId, floor: BlockRef) -> Result<bool, ChainError> {
        let h = self.height_of(c)?;
        Ok(self.ancestor_of(floor.id, h)? != Some(c)
            && self.ancestor_of(c, floor.height)? != Some(floor.id))
    }

    /// SPV's fork-choice target against the step's cursor (§6.2): the most-work accepted header
    /// with (a) no FAILED or CONFLICT entry between it and the cursor's chain and (b′) no
    /// conflict with the floor while ChainLocks are observed on; the lower id wins a tie. The
    /// resident leaves are walked from most work down; the store's leaves are read only when a
    /// leaf that left the window could still win (D19). `ChainError::Config` in the node.
    pub fn fork_target(&self, vt: &VirtualTip) -> Result<ForkTarget, ChainError> {
        self.require_mode(ChainMode::Spv, "fork_target (SPV only)")?;
        let floor = self.observed_floor();
        let cursor = vt.cursor;
        let mut best = self.key(self.floor_bounded(cursor, floor)?)?;
        for &(work, Reverse(leaf)) in self.leaves.iter().rev() {
            if (work, Reverse(leaf)) < best {
                break;
            }
            if let Some(c) = self.leaf_candidate(leaf, cursor, floor)? {
                best = best.max(c);
            }
        }
        if *self.max_cold_leaf_work.lock() >= best.0 {
            self.cold_leaves(|id, work| {
                if (work, Reverse(id)) > best
                    && let Some(c) = self.leaf_candidate(id, cursor, floor)?
                {
                    best = best.max(c);
                }
                Ok(())
            })?;
        }
        let Reverse(id) = best.1;
        let relation = match (self.cl_floor, floor) {
            (None, _) => FloorRelation::NoFloor,
            (Some(_), None) => FloorRelation::Unenforced,
            (_, Some(f)) if self.ancestor_of(id, f.height)? == Some(f.id) => {
                FloorRelation::Descends
            }
            (_, Some(_)) => FloorRelation::Trails,
        };
        Ok(ForkTarget {
            id,
            floor: relation,
        })
    }

    /// One leaf's candidate (§6.2 steps 2-3): the leaf, or the parent of the lowest FAILED or
    /// CONFLICT entry on its path down to the cursor's chain unless that parent is on it, then
    /// bounded by the floor; `None` when the walk reaches the cursor's chain with no good entry.
    fn leaf_candidate(
        &self,
        leaf: BlockId,
        cursor: BlockId,
        floor: Option<BlockRef>,
    ) -> Result<Option<Key>, ChainError> {
        let fork = self.lca(leaf, cursor)?;
        let fork_height = self.height_of(fork)?;
        let (mut c, mut x, mut n) = (Some(leaf), leaf, self.node(leaf)?);
        while x != fork {
            // Down past the fork without meeting it: a skip `lca` took was on another branch.
            if n.height <= fork_height {
                return Err(self.off_height(x));
            }
            let (prev, pn) = self.parent(&n)?.ok_or_else(|| self.off_height(x))?;
            if self.writer_status(x)?.has(Status::FAILED_MASK | Status::CONFLICT_CHAINLOCK) {
                c = (prev != fork).then_some(prev);
            }
            (x, n) = (prev, pn);
        }
        match c {
            Some(c) => self.key(self.floor_bounded(c, floor)?).map(Some),
            None => Ok(None),
        }
    }

    /// (b′): `c`, or `lca(c, floor)` when it conflicts with the floor.
    fn floor_bounded(&self, c: BlockId, floor: Option<BlockRef>) -> Result<BlockId, ChainError> {
        match floor {
            Some(f) if self.conflicts(c, f)? => self.lca(c, f.id),
            _ => Ok(c),
        }
    }

    /// `id`'s [`Key`].
    fn key(&self, id: BlockId) -> Result<Key, ChainError> {
        Ok((self.node(id)?.chain_work, Reverse(id)))
    }

    /// The floor (b′) and the refusal apply: `cl_floor` while ChainLocks are observed on (§7.5).
    fn observed_floor(&self) -> Option<BlockRef> {
        self.cl_floor.filter(|_| self.cl_enabled_observed)
    }

    /// SPV `set_active_tip`'s refusal (§6.2, DEC-136): a target that conflicts with the floor
    /// while ChainLocks are observed on is a driver bug.
    pub(super) fn refuse_floor_conflict(&self, to: BlockId) -> Result<(), ChainError> {
        let Some(floor) = self.observed_floor() else {
            return Ok(());
        };
        if self.conflicts(to, floor)? {
            debug_assert!(false, "set_active_tip({to:?}) conflicts with the floor {floor:?}");
            return Err(ChainError::ConflictsWithFloor {
                to,
                floor: floor.id,
            });
        }
        Ok(())
    }
}

/// A fork-target candidate's order (§6.2): more work, then the lower id.
type Key = (Work, Reverse<BlockId>);

/// The node's enforcement hold (D-C6-1): the tip mutex's guard, and the writer whose `composite`
/// holds the tip state for the walk. Dropped before `done` (a host callback, a step or the final
/// publish panicked), it moves the state back into the mutex if the walk still holds it and
/// trips the latch: the steps taken are in the state, no snapshot shows them, and the client
/// fail-stops (P6) instead of leaving the vacant state. `disconnect_tip` puts a step's state
/// back before unwinding.
struct Composite<'a, S: HeaderStore, R: Residency> {
    chain: &'a mut HeaderChain<S, R>,
    guard: HoldGuard,
    done: bool,
}

impl<S: HeaderStore, R: Residency> Drop for Composite<'_, S, R> {
    fn drop(&mut self) {
        self.chain.feed_shared().hold_closing();
        if !self.done {
            if let Some(st) = self.chain.composite.take() {
                **self.guard = st;
            }
            self.chain.svc.fail.read_failed("enforce_block unwound inside the tip mutex");
        }
        self.guard.release();
        self.chain.feed_shared().fire_release_wakes();
    }
}

/// `nStatus |= BLOCK_CONFLICT_CHAINLOCK`.
fn mark(s: Status) -> Status {
    Status(s.bits() | Status::CONFLICT_CHAINLOCK)
}
