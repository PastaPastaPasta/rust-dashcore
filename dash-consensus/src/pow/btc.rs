//! Bitcoin's retarget, which Dash runs below `nPowKGWHeight` (CS2 §3.4).

use dash_num::CompactTarget;

use super::{PowParams, broken_chain, div_u64};
use crate::{BlockId, ChainAncestry, HeaderCtx};

// Core: pow.cpp:134 GetNextWorkRequiredBTC (v23.1.8: 131)
#[inline] // its one caller is get_next_work_required (see there)
pub(super) fn next_work(
    chain: &dyn ChainAncestry,
    last: BlockId,
    new_time: u32,
    p: &PowParams,
) -> CompactTarget {
    let c = &p.config;
    // Not 0: PowParams rejects that wherever this rule is reachable.
    let interval = p.adjustment_interval();
    let tip = chain.header(last);

    // Only change once per interval.
    if (i64::from(tip.height) + 1) % interval != 0 {
        if c.allow_min_difficulty_blocks {
            // Core: pow.cpp:147 (v23.1.8: 144). More than 2 spacings since the last block (5
            // minutes, not Bitcoin's 20) allows a min-difficulty block.
            if i64::from(new_time) > i64::from(tip.time) + c.target_spacing * 2 {
                return p.pow_limit_bits;
            }
            // Core: pow.cpp:152 (v23.1.8: 149), the last block that is not a min-difficulty block
            let (mut id, mut header) = (last, tip);
            while let Some(prev) = chain.prev(id)
                && i64::from(header.height) % interval != 0
                && header.bits == p.pow_limit_bits
            {
                id = prev;
                header = chain.header(id);
            }
            return header.bits;
        }
        return tip.bits;
    }

    // Core: pow.cpp:162 (v23.1.8: 159). Go back by what we want to be 1 day worth of blocks. A
    // retarget height is a positive multiple of the interval, so this is at least 0, and the
    // ancestor exists.
    let Ok(first_height) = u32::try_from(i64::from(tip.height) - (interval - 1)) else {
        return broken_chain(p);
    };
    let Some(first) = chain.ancestor(last, first_height) else {
        return broken_chain(p);
    };
    calculate_next_work_required(&tip, i64::from(chain.header(first).time), p)
}

// Core: pow.cpp:214 CalculateNextWorkRequired (v23.1.8: 211)
fn calculate_next_work_required(
    last: &HeaderCtx,
    first_block_time: i64,
    p: &PowParams,
) -> CompactTarget {
    let c = &p.config;
    if c.no_retargeting {
        return last.bits;
    }

    // Limit adjustment step
    let mut actual_timespan = i64::from(last.time) - first_block_time;
    if actual_timespan < c.target_timespan / 4 {
        actual_timespan = c.target_timespan / 4;
    }
    if actual_timespan > c.target_timespan * 4 {
        actual_timespan = c.target_timespan * 4;
    }

    // Retarget. `*= int64_t` is `operator*=(uint32_t)` (CS2 §3.7). With Core's timespan of 86400
    // (every network's) the clamped value is at most 345600 and fits; a larger timespan would
    // truncate here exactly as Core does.
    let mut target = last.bits.expand().value * (actual_timespan as u32);
    target = div_u64(target, c.target_timespan as u64);
    if target > c.pow_limit {
        target = c.pow_limit;
    }
    target.compact(false)
}
