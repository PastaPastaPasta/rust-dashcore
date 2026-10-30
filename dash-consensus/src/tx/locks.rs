//! Relative lock times (CS5b; BIP68, FR-CON-09): Core's `CalculateSequenceLocks`,
//! `EvaluateSequenceLocks` and `SequenceLocks`, and the lock-time flags `ConnectBlock` passes them.
//!
//! The absolute lock time (`IsFinalTx`, with BIP113's median time past as the cutoff) is
//! [`super::is_final_tx`]; the script-level check (BIP112's `OP_CHECKSEQUENCEVERIFY`) is the
//! interpreter's.

use crate::ConsensusCtx;
use crate::Tx;
use crate::script::interpreter::{
    SEQUENCE_LOCKTIME_DISABLE_FLAG, SEQUENCE_LOCKTIME_MASK, SEQUENCE_LOCKTIME_TYPE_FLAG,
};

/// `CTxIn::SEQUENCE_LOCKTIME_GRANULARITY`: a time-based lock counts units of 2^9 = 512 seconds.
// Core: primitives/transaction.h:151
pub const SEQUENCE_LOCKTIME_GRANULARITY: u32 = 9;

/// Core's lock-time verification flags (`int flags`): all 32 bits are kept, the undefined ones
/// ignored.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct LockTimeFlags(u32);

impl LockTimeFlags {
    pub const NONE: Self = Self(0);
    /// `LOCKTIME_VERIFY_SEQUENCE`: interpret sequence numbers as relative lock times (BIP68).
    // Core: consensus/consensus.h:28
    pub const VERIFY_SEQUENCE: Self = Self(1 << 0);

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Every bit of `f` is set.
    pub const fn contains(self, f: Self) -> bool {
        self.0 & f.0 == f.0
    }
}

/// The lock-time flags `ConnectBlock` checks a block's transactions with:
/// [`LockTimeFlags::VERIFY_SEQUENCE`] when CSV is active at the block
/// (`DeploymentActiveAt(*pindex, DEPLOYMENT_CSV)`, which the caller evaluates).
// Core: validation.cpp:2626-2629 ConnectBlock
pub const fn block_lock_time_flags(csv_active: bool) -> LockTimeFlags {
    if csv_active {
        LockTimeFlags::VERIFY_SEQUENCE
    } else {
        LockTimeFlags::NONE
    }
}

/// The height and time a transaction's relative locks require, with `nLockTime`'s semantics: the
/// last *invalid* height and median time past, −1 when nothing is required.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct SequenceLocks {
    pub min_height: i32,
    pub min_time: i64,
}

impl SequenceLocks {
    /// No requirement.
    pub const NONE: Self = Self {
        min_height: -1,
        min_time: -1,
    };
}

/// Core's `CalculateSequenceLocks` for `tx` in the block after `ctx.prev` (height `ctx.height`;
/// the mempool's "next block" is the tip's child). `prev_heights[i]` is the height of the coin
/// input `i` spends (the mempool's own outputs count at `ctx.height`).
///
/// As Core, it sets `prev_heights[i]` to 0 for every input whose relative lock is disabled; the
/// mempool reads the heights afterwards for its lock points.
///
/// # Panics
///
/// When `prev_heights` and the inputs differ in number (Core asserts); and, for an input with a
/// time-based lock, when `ctx.prev` is `None` or the coin's height is above `ctx.height`. Core
/// reads the block itself there; its callers never pass either (`ConnectBlock`'s coins are at
/// most the block's height, the mempool judges the tip's child).
// Core: consensus/tx_verify.cpp:41 CalculateSequenceLocks
pub fn calculate_sequence_locks(
    tx: &Tx,
    flags: LockTimeFlags,
    prev_heights: &mut [u32],
    ctx: &ConsensusCtx<'_>,
) -> SequenceLocks {
    assert_eq!(
        prev_heights.len(),
        tx.inputs().len(),
        "CalculateSequenceLocks: one height per input"
    );

    let mut min_height: i32 = -1;
    let mut min_time: i64 = -1;

    // Core: consensus/tx_verify.cpp:56
    let enforce_bip68 =
        (i32::from(tx.version()) as u32) >= 2 && flags.contains(LockTimeFlags::VERIFY_SEQUENCE);
    if !enforce_bip68 {
        return SequenceLocks::NONE;
    }

    for (input, prev_height) in tx.inputs().iter().zip(prev_heights.iter_mut()) {
        // A sequence number with the disable flag set has no relative lock, and its coin's
        // height does not matter.
        if input.sequence & SEQUENCE_LOCKTIME_DISABLE_FLAG != 0 {
            *prev_height = 0;
            continue;
        }
        let coin_height = *prev_height as i32;
        let value = input.sequence & SEQUENCE_LOCKTIME_MASK;

        if input.sequence & SEQUENCE_LOCKTIME_TYPE_FLAG != 0 {
            // Time locks run from the median time past of the block before the coin's block,
            // the earliest time the coin's block could have. One is subtracted for nLockTime's
            // "last invalid" semantics.
            let coin_time = ancestor_mtp(ctx, coin_height.saturating_sub(1).max(0) as u32);
            let lock = coin_time + (i64::from(value) << SEQUENCE_LOCKTIME_GRANULARITY) - 1;
            min_time = min_time.max(lock);
        } else {
            min_height = min_height.max(coin_height.wrapping_add(value as i32).wrapping_sub(1));
        }
    }
    SequenceLocks {
        min_height,
        min_time,
    }
}

/// `block.GetAncestor(height)->GetMedianTimePast()` for the block after `ctx.prev`.
fn ancestor_mtp(ctx: &ConsensusCtx<'_>, height: u32) -> i64 {
    let prev = ctx.prev.expect("CalculateSequenceLocks: the block has a parent");
    let ancestor =
        ctx.chain.ancestor(prev, height).expect("CalculateSequenceLocks: the coin is in the chain");
    ctx.chain.header(ancestor).mtp
}

/// Core's `EvaluateSequenceLocks`: whether the block after `ctx.prev` satisfies `locks`, its
/// height above `min_height` and its parent's median time past above `min_time`.
// Core: consensus/tx_verify.cpp:103 EvaluateSequenceLocks
pub fn evaluate_sequence_locks(ctx: &ConsensusCtx<'_>, locks: SequenceLocks) -> bool {
    // Core asserts `block.pprev`.
    assert!(ctx.prev.is_some(), "EvaluateSequenceLocks: the block has a parent");
    let block_time = ctx.median_time_past;
    !(i64::from(locks.min_height) >= i64::from(ctx.height) || locks.min_time >= block_time)
}

/// Core's `SequenceLocks`: [`calculate_sequence_locks`] then [`evaluate_sequence_locks`].
/// `ConnectBlock` rejects a transaction that fails it with `bad-txns-nonfinal`.
// Core: consensus/tx_verify.cpp:113 SequenceLocks
pub fn sequence_locks(
    tx: &Tx,
    flags: LockTimeFlags,
    prev_heights: &mut [u32],
    ctx: &ConsensusCtx<'_>,
) -> bool {
    evaluate_sequence_locks(ctx, calculate_sequence_locks(tx, flags, prev_heights, ctx))
}
