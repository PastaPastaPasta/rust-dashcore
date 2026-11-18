//! The chain selections and the case generators shared by the recorded corpus and the live
//! differential.

use dash_consensus::payments::PaymentParams;
use dash_consensus::pow::{PowConfig, PowParams};
use dash_consensus::subsidy::SubsidyParams;
use dash_oracle_sys::{Chain, Oracle};
use dash_types::Numeric;
use serde_json::Value;

use crate::common;
use crate::rng::Rng;

/// A chain selection: Core's chain parameters and the crate's views of them.
pub struct Variant {
    pub name: &'static str,
    pub chain: Chain,
    /// Core's chain parameters (`oracle_chainparams_json`) on the pin they were read from.
    pub cp: Value,
    pub pow: PowParams,
    pub subsidy: SubsidyParams,
    pub payments: PaymentParams,
}

impl Variant {
    /// The views of `dash-params`' parameters for the selection `name` at `pin`, built by the
    /// crate's own constructors; `common` checks they equal the views of Core's `cp`.
    pub fn new(name: &'static str, pin: &str, chain: Chain, cp: Value) -> Self {
        let recorded = common::Recorded {
            pin: pin.into(),
            name: name.into(),
            json: cp,
        };
        Self {
            name,
            pow: common::pow_params(&recorded),
            subsidy: common::subsidy_params(&recorded),
            payments: common::payment_params(&recorded),
            chain,
            cp: recorded.json,
        }
    }
}

/// (name, Core chain, args): every chain selection the corpus and the differential use, shared
/// with the tests (`dash-consensus/tests/common`, `tests/data/PROVENANCE.md`).
pub use crate::common::SELECTIONS as VARIANTS;

/// Every chain selection, with Core's parameters from `oracle`'s pin.
pub fn variants(oracle: &Oracle) -> Vec<Variant> {
    VARIANTS
        .iter()
        .map(|&(name, chain, args)| {
            let chain = Chain::new(chain, args);
            Variant::new(name, oracle.pin().name(), chain.clone(), chainparams(oracle, &chain))
        })
        .collect()
}

/// Core's chain parameters, parsed.
pub fn chainparams(oracle: &Oracle, chain: &Chain) -> Value {
    serde_json::from_str(&oracle.chainparams_json(chain).expect("chainparams_json"))
        .expect("Core's JSON")
}

pub struct NextWorkCase {
    pub first_height: u32,
    pub headers: Vec<(u32, u32)>,
    pub new_time: u32,
}

impl NextWorkCase {
    pub fn new(first_height: u32, headers: Vec<(u32, u32)>, new_time: u32) -> Self {
        Self {
            first_height,
            headers,
            new_time,
        }
    }

    pub fn last_height(&self) -> u32 {
        self.first_height + self.headers.len() as u32 - 1
    }
}

fn realistic_bits(rng: &mut Rng) -> u32 {
    let exp =
        *rng.pick(&[0x03u32, 0x04, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21]);
    let mant = rng.range(0x00_8000, 0x7f_ffff) as u32;
    (exp << 24) | mant
}

fn bits(rng: &mut Rng, pow_limit_bits: u32, base: u32) -> u32 {
    match rng.below(10) {
        0 => pow_limit_bits,
        1 => rng.next() as u32,
        2 => *rng.pick(&[
            0,
            1,
            0x0080_0000,
            0x0100_0000,
            0x1d00_ffff,
            0x207f_ffff,
            0x2100_ffff,
            0x2200_ffff,
            0xff7f_ffff,
            0x1e0f_fff0,
            0x0480_0001,
            0x1f7f_ffff,
        ]),
        3 | 4 => realistic_bits(rng),
        // a slowly drifting difficulty, like a real chain
        _ => {
            let mant = (base & 0x7f_ffff) as i64 + rng.range(-0x4000, 0x4000);
            (base & 0xff80_0000) | (mant.clamp(0x8000, 0x7f_ffff) as u32)
        }
    }
}

fn delta(rng: &mut Rng, spacing: i64) -> i64 {
    match rng.below(8) {
        0 => spacing,
        1 => 0,
        2 => -rng.range(1, 2 * spacing.min(1_000_000)),
        3 => rng.range(3 * spacing, 10 * spacing).min(10_000_000),
        4 => rng.range(7000, 7400),
        _ => rng.range(0, 3 * spacing.min(3_000_000)),
    }
}

fn clamp_time(t: i64) -> u32 {
    t.clamp(0, u32::MAX as i64) as u32
}

/// A header window ending at a chosen height, sized to cover what GetNextWorkRequired walks.
/// `kgw` allows KGW windows (up to 4033 headers); without it KGW-region heights are re-drawn.
pub fn next_work_case(rng: &mut Rng, p: &PowParams, kgw: bool, max_len: u32) -> NextWorkCase {
    let c = *p.config();
    let spacing = c.target_spacing;
    let interval = c.target_timespan / spacing;
    let pow_limit_bits = p.pow_limit_bits().to_base();
    loop {
        // the last height
        let anchors = [
            0,
            1,
            2,
            13,
            14,
            15,
            23,
            24,
            25,
            interval - 2,
            interval - 1,
            interval,
            2 * interval - 1,
            3 * interval - 1,
            i64::from(c.kgw_height) - 2,
            i64::from(c.kgw_height) - 1,
            i64::from(c.kgw_height),
            i64::from(c.dgw_height) - 2,
            i64::from(c.dgw_height) - 1,
            i64::from(c.dgw_height),
            i64::from(c.minimum_difficulty_blocks) - 1,
            i64::from(c.minimum_difficulty_blocks),
            i64::from(c.minimum_difficulty_blocks) + 1,
            68588,
            68589,
            68590,
        ];
        let h: i64 = match rng.below(4) {
            0 => rng.range(0, 3_000_000),
            1 => rng.range(0, 200),
            _ => *rng.pick(&anchors) + rng.range(-1, 1),
        };
        if !(0..=4_000_000).contains(&h) {
            continue;
        }
        let h = h as u32;
        let last_time = rng.range(1_390_000_000, 4_200_000_000);
        let new_time = clamp_time(
            last_time
                + match rng.below(9) {
                    0 => 2 * spacing,
                    1 => 2 * spacing + 1,
                    2 => 4 * spacing,
                    3 => 4 * spacing + 1,
                    4 => 7200,
                    5 => 7201,
                    6 => -rng.range(0, 5000),
                    _ => rng.range(0, 3 * spacing.min(100_000)),
                },
        );
        let last_time = clamp_time(last_time);
        // the branch Core takes sizes the window
        let len: u32 = match rule(&c, h.into(), last_time.into(), new_time.into()) {
            Rule::Short => 1 + rng.below(3) as u32,
            Rule::BtcRetarget => interval as u32,
            // a run of powLimit blocks back to a stopper
            Rule::BtcWalk => 0,
            Rule::Kgw if !kgw => continue,
            Rule::Kgw => 4033,
            Rule::Dgw => 24 + rng.below(4) as u32,
        };

        if len > max_len {
            continue;
        }
        let mut headers: Vec<(u32, u32)>;
        if len == 0 {
            // run of powLimit bits ending at the last block, preceded by a stopper
            let run = rng.below(40) as u32;
            let total = (run + 1).min(h + 1);
            headers = Vec::with_capacity(total as usize);
            let mut t = i64::from(last_time);
            let mut base = realistic_bits(rng);
            for k in 0..total {
                let b = if k < run {
                    pow_limit_bits
                } else {
                    bits(rng, pow_limit_bits, base)
                };
                base = b;
                headers.push((clamp_time(t), b));
                t -= delta(rng, spacing);
            }
            headers.reverse();
        } else {
            let total = len.min(h + 1);
            headers = Vec::with_capacity(total as usize);
            let mut t = i64::from(last_time);
            let mut base = realistic_bits(rng);
            let steady = rng.chance(1, 2);
            for _ in 0..total {
                let b = if steady && !rng.chance(1, 20) {
                    base
                } else {
                    bits(rng, pow_limit_bits, base)
                };
                if !steady {
                    base = b;
                }
                headers.push((clamp_time(t), b));
                t -= if steady {
                    spacing + rng.range(-spacing / 3, spacing / 3)
                } else {
                    delta(rng, spacing)
                };
            }
            headers.reverse();
        }
        let first_height = h + 1 - headers.len() as u32;
        return NextWorkCase {
            first_height,
            headers,
            new_time,
        };
    }
}

/// An 80-byte header for the oracle: version 2, zero hashes, the given time and bits, nonce 0.
/// `GetNextWorkRequired` reads only the times and bits.
pub fn encode_header(time: u32, bits: u32) -> [u8; 80] {
    let mut h = [0u8; 80];
    h[0..4].copy_from_slice(&2u32.to_le_bytes());
    h[68..72].copy_from_slice(&time.to_le_bytes());
    h[72..76].copy_from_slice(&bits.to_le_bytes());
    h
}

/// Run one next-work case on a pin.
pub fn oracle_next_work(
    o: &Oracle,
    chain: &Chain,
    case: &NextWorkCase,
) -> Result<u32, dash_oracle_sys::Error> {
    let headers: Vec<u8> =
        case.headers.iter().flat_map(|&(time, bits)| encode_header(time, bits)).collect();
    o.next_work_required(
        chain,
        &headers,
        case.first_height as i32,
        &encode_header(case.new_time, 0),
    )
}

/// The branch of Core's `GetNextWorkRequired` for the block after `last_height`, in Core's order
/// (pow.cpp:170-211), as far as the window a case needs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// The answer reads the last block at most: powLimit, or the last block's bits.
    Short,
    /// BTC at a retarget height: the last `DifficultyAdjustmentInterval` blocks.
    BtcRetarget,
    /// BTC's min-difficulty walk back to the last non-powLimit block.
    BtcWalk,
    Kgw,
    Dgw,
}

pub fn rule(c: &PowConfig, last_height: i64, last_time: i64, new_time: i64) -> Rule {
    let next = last_height + 1;
    if last_height < i64::from(c.minimum_difficulty_blocks) {
        return Rule::Short;
    }
    if next < i64::from(c.kgw_height) {
        // not 0 where BTC is reachable (PowParams)
        let interval = c.target_timespan / c.target_spacing;
        return if next % interval == 0 {
            Rule::BtcRetarget
        } else if c.allow_min_difficulty_blocks && new_time <= last_time + c.target_spacing * 2 {
            Rule::BtcWalk
        } else {
            Rule::Short
        };
    }
    let min_difficulty = c.allow_min_difficulty_blocks
        && (new_time > last_time + 7200 || new_time > last_time + c.target_spacing * 4);
    if c.no_retargeting || min_difficulty {
        Rule::Short
    } else if next < i64::from(c.dgw_height) {
        Rule::Kgw
    } else {
        Rule::Dgw
    }
}

/// True when Core's GetNextWorkRequired runs KGW for this case.
pub fn reaches_kgw(p: &PowParams, case: &NextWorkCase) -> bool {
    let (last_time, new_time) = (case.headers.last().unwrap().0, case.new_time);
    let rule = rule(p.config(), case.last_height().into(), last_time.into(), new_time.into());
    rule == Rule::Kgw
}

pub fn subsidy_heights(rng: &mut Rng, v: &Variant) -> i32 {
    let c = v.subsidy.config();
    let iv = c.halving_interval as i64;
    let anchors = [
        -2,
        -1,
        0,
        1,
        2,
        c.high_subsidy_blocks as i64 - 1,
        c.high_subsidy_blocks as i64,
        c.high_subsidy_blocks as i64 + 1,
        c.budget_payments_start as i64 - 1,
        c.budget_payments_start as i64,
        c.budget_payments_start as i64 + 1,
        4500,
        4501,
        5464,
        5465,
        16999,
        17000,
        23999,
        24000,
        c.v20_height as i64 - 1,
        c.v20_height as i64,
        iv * rng.range(1, 20) - 1,
        iv * rng.range(1, 20),
        iv * rng.range(20, 2000),
        (i32::MAX as i64 / iv) * iv - 1,
        (i32::MAX as i64 / iv) * iv,
        i32::MAX as i64,
    ];
    let h = match rng.below(3) {
        0 => rng.range(-10, 3_500_000),
        _ => *rng.pick(&anchors) + rng.range(-1, 1),
    };
    h.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

pub fn prev_bits(rng: &mut Rng, v: &Variant) -> u32 {
    match rng.below(5) {
        0 => v.pow.pow_limit_bits().to_base(),
        1 => 1,
        2 => rng.next() as u32,
        _ => realistic_bits(rng),
    }
}

pub fn payment_height(rng: &mut Rng, v: &Variant) -> i32 {
    let c = v.payments.config();
    let cycle = c.superblocks.cycle() as i64;
    let brr = c.brr_height as i64;
    let realloc_start = brr - brr % cycle + cycle;
    let anchors = [
        c.increase_block as i64 + c.increase_period as i64 * rng.range(0, 10),
        brr,
        realloc_start,
        realloc_start + 3 * cycle * rng.range(0, 20),
        0,
        1,
    ];
    let h = match rng.below(3) {
        0 => rng.range(0, 3_000_000),
        _ => *rng.pick(&anchors) + rng.range(-1, 1),
    };
    h.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

pub fn block_value(rng: &mut Rng) -> i64 {
    match rng.below(4) {
        0 => rng.range(i64::MIN / 600, i64::MAX / 600),
        1 => rng.range(-1000, 1000),
        _ => rng.range(0, 600 * 100_000_000),
    }
}

/// The devnets whose spacing is above the timespan, shared with the tests
/// (`dash-consensus/tests/common`, `tests/data/PROVENANCE.md`).
pub use crate::common::LARGE_SPACING;

/// The [`LARGE_SPACING`] chain selections, with Core's parameters from `oracle`'s pin.
pub fn large_spacing_variants(oracle: &Oracle) -> Vec<Variant> {
    LARGE_SPACING
        .iter()
        .map(|&(name, chain, args)| {
            let chain = Chain::new(chain, args);
            Variant::new(name, oracle.pin().name(), chain.clone(), chainparams(oracle, &chain))
        })
        .collect()
}

/// A window for a [`LARGE_SPACING`] devnet: mostly DGW (a last height from 4000), a few around
/// `nMinimumDifficultyBlocks`; arbitrary gaps between the `uint32` times (where the clamped
/// timespan then lands is in [`LARGE_SPACING`]); the candidate within the 2 h min-difficulty
/// rule mostly.
pub fn large_spacing_case(rng: &mut Rng, p: &PowParams) -> NextWorkCase {
    let pow_limit_bits = p.pow_limit_bits().to_base();
    let last_height = if rng.chance(1, 10) {
        rng.range(3990, 4010)
    } else {
        rng.range(4000, 3_000_000)
    } as u32;
    let len = (last_height + 1).min(25 + rng.below(3) as u32);
    let mut t = rng.range(1_000_000_000, 4_000_000_000);
    let mut base = realistic_bits(rng);
    let mut headers = Vec::with_capacity(len as usize);
    for _ in 0..len {
        base = bits(rng, pow_limit_bits, base);
        headers.push((clamp_time(t), base));
        t -= match rng.below(4) {
            0 => rng.range(0, 600),
            1 => rng.range(0, 1 << 31),
            2 => -rng.range(0, 3600),
            _ => rng.range(0, 1 << 24),
        };
    }
    headers.reverse();
    let last_time = i64::from(headers[headers.len() - 1].0);
    let new_time = clamp_time(last_time + rng.range(-100, 7300));
    NextWorkCase::new(last_height + 1 - len, headers, new_time)
}

/// Collateral shares for `SplitAmountByShares` (CS2 §6.2 `diff_payout_split`): 1 to 8 positive
/// amounts, most of a real collateral's size (products with the total above 2^64), and a total
/// from a masternode reward's range, sometimes with an awkward remainder.
pub fn split_case(rng: &mut Rng) -> (i64, Vec<i64>) {
    let n = 1 + rng.below(8) as usize;
    let coin = 100_000_000;
    let shares = (0..n)
        .map(|_| match rng.below(3) {
            0 => rng.range(1, 4000 * coin),
            1 => rng.range(1, 1000),
            _ => coin * rng.range(1, 4000),
        })
        .collect();
    let total = match rng.below(3) {
        0 => rng.range(0, 1000 * coin),
        1 => rng.range(0, 1000),
        _ => coin * rng.range(1, 500) + rng.range(1, 99),
    };
    (total, shares)
}
