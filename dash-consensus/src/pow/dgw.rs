//! Dark Gravity Wave v3 (CS2 §3.6).

use dash_num::{Arith256, CompactTarget};
use dash_types::Numeric;

use super::{PowParams, broken_chain, div_u64};
use crate::{BlockId, ChainAncestry};

/// The window: the last block and the 23 before it.
const PAST_BLOCKS: u32 = 24;

// Core: pow.cpp:81 DarkGravityWave
#[inline] // its one caller is get_next_work_required (see there)
pub(super) fn next_work(chain: &dyn ChainAncestry, last: BlockId, p: &PowParams) -> CompactTarget {
    let c = &p.config;
    let tip = chain.header(last);
    // Make sure there are at least PAST_BLOCKS + 1 blocks, otherwise just return powLimit.
    if tip.height < PAST_BLOCKS {
        return p.pow_limit_bits;
    }

    let mut id = last;
    let mut past_target_avg = Arith256::ZERO;
    for count in 1..=PAST_BLOCKS {
        let target = chain.header(id).bits.expand().value;
        past_target_avg = if count == 1 {
            target
        } else {
            // Not an average (quirk 8): each step truncates, `* count` is
            // `operator*(uint32_t)` and `+` wraps mod 2^256, and the division goes through
            // `operator/(uint64_t)`.
            div_u64(past_target_avg * count + target, u64::from(count + 1))
        };
        if count != PAST_BLOCKS {
            match chain.prev(id) {
                Some(prev) => id = prev,
                None => return broken_chain(p),
            }
        }
    }

    // Quirk 8: the actual timespan covers the 23 intervals between the window's first and last
    // block, the target 24 spacings. Core keeps this off-by-one; it is consensus.
    let mut actual_timespan = i64::from(tip.time) - i64::from(chain.header(id).time);
    let target_timespan = i64::from(PAST_BLOCKS) * c.target_spacing;
    if actual_timespan < target_timespan / 3 {
        actual_timespan = target_timespan / 3;
    }
    if actual_timespan > target_timespan * 3 {
        actual_timespan = target_timespan * 3;
    }

    // Retarget. `*= int64_t` is `operator*=(uint32_t)` (CS2 §3.7): at the standard spacing the
    // clamped timespan always fits, but a devnet spacing of 2^29 or more truncates it for real.
    let mut next = past_target_avg * (actual_timespan as u32);
    next = div_u64(next, target_timespan as u64);
    if next > c.pow_limit {
        next = c.pow_limit;
    }
    next.compact(false)
}
