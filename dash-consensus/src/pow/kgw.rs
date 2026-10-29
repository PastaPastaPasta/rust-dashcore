//! Kimoto Gravity Well, mainnet's retarget for next heights 15200..=34139 (CS2 §3.5). Only its
//! event horizon is floating point, in `fp`.

use dash_num::{Arith256, CompactTarget};
use dash_types::Numeric;

use super::{PowParams, div_u64};
use crate::fp::{self, KgwMass};
use crate::{BlockId, ChainAncestry};

// Core: pow.cpp:15 KimotoGravityWell
#[inline] // its one caller is get_next_work_required (see there)
pub(super) fn next_work(chain: &dyn ChainAncestry, last: BlockId, p: &PowParams) -> CompactTarget {
    let c = &p.config;
    let last_solved = chain.header(last);
    // positive (PowParams); Core's `uint64_t` arithmetic with the `int64_t` parameters
    let spacing = c.target_spacing as u64;
    let past_seconds_min = fp::kgw_past_seconds_min(c.target_timespan);
    let past_seconds_max = (c.target_timespan * 7) as u64;
    let past_blocks_min = past_seconds_min / spacing;
    // 1..=4032 (PowParams), so the mass always is a KgwMass
    let past_blocks_max = past_seconds_max / spacing;

    if last_solved.height == 0 || u64::from(last_solved.height) < past_blocks_min {
        return p.pow_limit_bits;
    }

    let mut reading = last;
    let mut mass: u64 = 0;
    let mut actual_seconds: u32 = 0;
    let mut target_seconds: i64 = 0;
    let mut average = Arith256::ZERO;
    let mut average_prev = Arith256::ZERO;
    let mut i: u64 = 1;
    loop {
        let header = chain.header(reading);
        if header.height == 0 || (past_blocks_max > 0 && i > past_blocks_max) {
            break;
        }
        mass += 1;

        average = header.bits.expand().value;
        if i > 1 {
            // handle negative arith_uint256: both differences are taken the non-negative way
            average = if average >= average_prev {
                div_u64(average - average_prev, i) + average_prev
            } else {
                average_prev - div_u64(average_prev - average, i)
            };
        }
        average_prev = average;

        // Core's `int64_t` difference of two `uint32_t` times, clamped to ≥ 0 after the target
        actual_seconds = last_solved.time.saturating_sub(header.time);
        // `int64_t * uint64_t`, assigned to `int64_t`; at most 7 · nPowTargetTimespan
        target_seconds = spacing.wrapping_mul(mass) as i64;
        if mass >= past_blocks_min {
            let Some(mass) = KgwMass::new(mass) else {
                return unreachable_mass(p);
            };
            if fp::kgw_horizon_crossed(mass, target_seconds, actual_seconds) {
                break;
            }
        }
        match chain.prev(reading) {
            Some(prev) => reading = prev,
            None => break,
        }
        i += 1;
    }

    let mut next = average;
    if actual_seconds != 0 && target_seconds != 0 {
        // `*= int64_t` is `operator*=(uint32_t)` (CS2 §3.7), and the actual seconds fit
        next *= actual_seconds;
        next = div_u64(next, target_seconds as u64);
    }
    if next > c.pow_limit {
        next = c.pow_limit;
    }
    next.compact(false)
}

/// A mass beyond KGW's 4032: unreachable, since `PowParams` bounds `PastBlocksMax` to 1..=4032
/// wherever KGW runs. Debug builds panic; release builds return the powLimit compact, as for a
/// broken chain (CS2 §3.8).
#[cold]
fn unreachable_mass(p: &PowParams) -> CompactTarget {
    debug_assert!(false, "KGW's mass left 1..=4032, which PowParams rules out");
    p.pow_limit_bits
}
