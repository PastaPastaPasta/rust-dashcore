//! ChainLock types (SB4 §4, §7).

use dash_consensus::BlockId;
use dash_primitives::BlockHash;

/// A ChainLock whose signature has been verified upstream (SB8's `VerifiedChainLock` wraps it
/// later, D11).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChainLockRef {
    pub height: u32,
    pub hash: BlockHash,
}

/// The outcome of `apply_chainlock` and `record_chainlock` (Core `ProcessNewChainLock`'s index
/// side).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClOutcome {
    /// `height <= ` the best ChainLock's height (`chainlock/chainlock.cpp:110-113`).
    Stale,
    /// The header is unknown: the best ChainLock is recorded, the floor is unchanged
    /// (`:129-131`).
    Pending,
    /// The hash is known at a different height (`:119-125`).
    HeightMismatch,
    /// The floor moved to `id`. `enforced` is `None` when no `EnforceBlock` ran: always from the
    /// node's `record_chainlock` (V enforces in a later command), and from `apply_chainlock`
    /// when the scheduled `CheckActiveState` (`chainlock/handler.cpp:168-176`) finds the
    /// observed state off, or turns it on and so resets the floor: the live spork flipped
    /// during the call (§7.5).
    Floor {
        id: BlockId,
        enforced: Option<EnforceReport>,
    },
}

/// What one `EnforceBlock` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnforceReport {
    /// The floor it enforced: Core's `bestChainLockBlockIndex`, read once with the lock's
    /// signature (`chainlock/handler.cpp:283`; CH1-A7).
    pub floor: BlockId,
    /// The sibling roots `MarkConflictingBlock` was called on, in call order.
    pub marked: Vec<BlockId>,
    /// The tips disconnected, tip down (node only). SPV leaves it empty: the step's
    /// [`VirtualTip::disconnected`](crate::VirtualTip::disconnected) holds the list (DEC-187).
    pub disconnected: Vec<BlockId>,
    /// `ResetBlockFailureFlags` on the locked block (node only, D18).
    pub reset: Option<ResetReport>,
}

/// What the index half of `ResetBlockFailureFlags` did (SB4 §7.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResetReport {
    /// Every entry reconsidered (and dirtied), in visit order.
    pub reconsidered: Vec<BlockId>,
    /// Children promoted from `FAILED_CHILD` to `FAILED_CHILD | FAILED_VALID`.
    pub promoted: Vec<BlockId>,
}

/// SPV's fork-choice target (SB4 §6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForkTarget {
    pub id: BlockId,
    pub floor: FloorRelation,
}

/// How a fork target relates to the ChainLock floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloorRelation {
    /// There is no floor.
    NoFloor,
    /// ChainLocks are observed off: the floor is kept but not applied.
    Unenforced,
    /// The target is the floor or descends from it.
    Descends,
    /// The target is a strict ancestor of the floor (DEC-136: Core's trailing tip).
    Trails,
}

/// Core's `CheckActiveState` transition (`chainlock/handler.cpp:209-222`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClActiveState {
    Unchanged,
    /// Observed on → off; nothing is cleared.
    TurnedOff,
    /// Observed off → on: `ResetChainlock` ran (`chainlock/chainlock.cpp:98-104`).
    TurnedOnReset,
}

/// SPV's persisted ChainLock state (SB0 §4.3.1, §9.5; SB4 D30).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClPersisted {
    pub best: Option<ChainLockRef>,
    pub floor: Option<BlockId>,
    pub enabled_observed: bool,
}

/// `UpdatedBlockTip`'s guards (`chainlock/handler.cpp:187-189`): false when `new == fork` or
/// `ibd`. `fork: None` is Core's `nullptr` (`InvalidateBlock`, `MarkConflictingBlock`), which
/// always passes the first guard.
// Core: chainlock/handler.cpp:185-207 CChainLocksHandler::UpdatedBlockTip
pub fn updated_block_tip_schedules(new: BlockId, fork: Option<BlockId>, ibd: bool) -> bool {
    fork != Some(new) && !ibd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updated_block_tip_guards() {
        let (a, b) = (BlockId(1), BlockId(2));
        assert!(updated_block_tip_schedules(a, None, false));
        assert!(updated_block_tip_schedules(a, Some(b), false));
        assert!(!updated_block_tip_schedules(a, Some(a), false));
        assert!(!updated_block_tip_schedules(a, None, true));
    }
}
