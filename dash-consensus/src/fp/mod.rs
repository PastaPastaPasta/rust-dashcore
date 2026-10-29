//! Core's consensus floating point (CS2 §2): the difficulty and the three formulas of the pre-V20
//! subsidy, the mainnet ≤ 68589 `nBits` tolerance, and KGW's event horizon. The only `f32`/`f64`
//! code in the crate (CS2 §2.6; `tests/fp_confinement.rs`).
//!
//! What the shipped Dash Core builds compute here is settled by their disassembly
//! (`CS2-fp-release-binaries.md` §0, all six platforms at both pins), and these rules keep the
//! port equal to it (CS2 §2.2):
//! - Only IEEE `+ − × ÷`, comparisons and exact integer conversions, in Core's order and
//!   grouping. No libm anywhere in the crate (`clippy.toml`), and Rust never contracts `a * b + c`
//!   into a fused multiply-add, so the subsidy and the tolerance are bit-identical to every build.
//! - KGW's one libm call, `pow`, is a table of correctly rounded values ([`kgw_pow`]). The shipped
//!   builds use at least four different `pow`s and two roundings of the horizon, yet every one of
//!   them, and this table, makes the same break decision for every input (CS2 §2.4, quirk 40).
//! - No `f64` leaves the module: callers get `bool`, `i64` and the opaque [`Difficulty`].
//! - Every validating thread runs in the IEEE default environment: round to nearest even, no
//!   flush to zero, no denormals-are-zero. Rust never changes it; it is a stated assumption
//!   (CS2 §2.2 rule 4). No value on the admissible domain is subnormal (§2.5).

mod kgw_pow;
#[cfg(test)]
mod tests;

use kgw_pow::KGW_POW;

// x87 arithmetic rounds to extended precision first; every Rust target but 32-bit x86 without
// SSE2 has IEEE binary64 (CS2 §2.2 rule 3).
#[cfg(all(target_arch = "x86", not(target_feature = "sse2")))]
compile_error!("dash-consensus needs IEEE binary64 arithmetic, which x87 does not provide");

/// The "difficulty" the pre-V20 subsidy reads: Core's `ConvertBitsToDouble` of the previous
/// block's raw `nBits`, or the early-mainnet variant. Opaque: its `f64` stays in `fp`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Difficulty(f64);

impl Difficulty {
    /// Core's `ConvertBitsToDouble(nBits)`: `65535 / (nBits & 0xffffff)`, then one `* 256` per
    /// size byte below 29, or one `/ 256` per size byte above, each rounded on its own.
    ///
    /// The mask keeps all 24 low bits, the sign bit included, unlike `SetCompact`'s 23, and a
    /// zero mantissa gives +∞. Encodings of one target that differ in their words (`0x1e010000`
    /// and `0x20000001` both decode to 2^232) give the same value here but not in
    /// [`Self::from_bits_mainnet_early`], so callers pass the raw word.
    // Core: validation.cpp:1784 ConvertBitsToDouble (v23.1.8: 1458)
    pub fn from_bits(bits: u32) -> Self {
        let mut shift = (bits >> 24) & 0xff;
        let mut difficulty = f64::from(0x0000_ffff_u32) / f64::from(bits & 0x00ff_ffff);
        while shift < 29 {
            difficulty *= 256.0;
            shift += 1;
        }
        while shift > 29 {
            difficulty /= 256.0;
            shift -= 1;
        }
        Self(difficulty)
    }

    /// The difficulty Core uses on mainnet while `nPrevHeight` ≤ 4500, "a bug which caused diff
    /// to not be correctly calculated": `65535 / (nBits & 0xffffff)`, without the size byte.
    // Core: validation.cpp:1814-1816 (v23.1.8: 1488-1490)
    pub fn from_bits_mainnet_early(bits: u32) -> Self {
        Self(f64::from(0x0000_ffff_u32) / f64::from(bits & 0x00ff_ffff))
    }

    /// `dDiff <= 75`: whether a block below height 24000 still takes the CPU-era formula.
    // Core: validation.cpp:1837 (v23.1.8: 1511)
    pub fn at_most_75(self) -> bool {
        self.0 <= 75.0
    }

    /// The early formula, `1111 / (dDiff + 1)²`, truncated toward zero. Core clamps it to
    /// [1, 500] after the truncation (quirk 11).
    // Core: validation.cpp:1834 (v23.1.8: 1508), `pow(x, 2.0)` being `x * x` in every build
    pub fn subsidy_base_early(self) -> i64 {
        let x = self.0 + 1.0;
        truncate(1111.0 / (x * x))
    }

    /// The CPU-era formula, `11111 / ((dDiff + 51) / 6)²`, truncated toward zero. Core clamps it to
    /// [25, 500].
    // Core: validation.cpp:1840 (v23.1.8: 1514)
    pub fn subsidy_base_cpu(self) -> i64 {
        let x = (self.0 + 51.0) / 6.0;
        truncate(11111.0 / (x * x))
    }

    /// The GPU/ASIC-era formula, `2222222 / ((dDiff + 2600) / 9)²`, truncated toward zero. Core
    /// clamps it to [5, 25].
    // Core: validation.cpp:1846 (v23.1.8: 1520)
    pub fn subsidy_base_gpu(self) -> i64 {
        let x = (self.0 + 2600.0) / 9.0;
        truncate(2_222_222.0 / (x * x))
    }

    /// The value's bit pattern, for the differential sweep against Core's `ConvertBitsToDouble`
    /// (CS2 §6.3). Consensus code compares difficulties only through this module.
    pub fn to_bits(self) -> u64 {
        self.0.to_bits()
    }
}

/// Core's implicit conversion of a subsidy base from `double` to `CAmount`: truncation toward
/// zero (`cvttsd2si`, `fcvtzs`, `fcvt.l.d … rtz` in the shipped builds).
///
/// For every `nBits` the difficulty is ≥ 0 or +∞, so each formula's divisor is at least 1 and
/// its quotient lies in [0, 1111] (CS2 §2.3). There the saturating `as` truncates exactly as the
/// builds do; debug builds assert it, and the exhaustive sweep runs on every `nBits` with debug
/// assertions on (CS2 §6.3).
fn truncate(base: f64) -> i64 {
    debug_assert!((0.0..=1111.0).contains(&base), "a subsidy base outside [0, 1111]: {base}");
    base as i64
}

/// Core's `nBits` rule for mainnet blocks up to height 68589: whether `header_bits` is within 50%
/// of `expected_bits` in [`Difficulty::from_bits`] terms.
///
/// Core rejects when `|n1 − n2| > n1 · 0.5`. That comparison is false on NaN (`n1 = n2 = +∞`, two
/// zero mantissas) and with `n1 = +∞`, so those are accepted; the literal negation below keeps
/// that, where a `<=` would not (CS2 §2.5 F3). CS3 owns the reject (`bad-diffbits`).
// Core: validation.cpp:4479-4482 ContextualCheckBlockHeader (v23.1.8: 4113-4116)
#[expect(clippy::neg_cmp_op_on_partial_ord, reason = "Core's predicate, false on NaN (CS2 §2.3)")]
pub fn within_pre_dgw_tolerance(header_bits: u32, expected_bits: u32) -> bool {
    let n1 = Difficulty::from_bits(header_bits).0;
    let n2 = Difficulty::from_bits(expected_bits).0;
    !((n1 - n2).abs() > n1 * 0.5)
}

/// KGW's largest `PastBlocksMass`, mainnet's `7 · 86400 / 150`: the length of [`KGW_POW`]
/// ([`PowParams`](crate::pow::PowParams) refuses parameters that would reach a larger one).
pub const KGW_MAX_MASS: u16 = 4032;
const _: () = assert!(KGW_POW.len() == KGW_MAX_MASS as usize);

/// KGW's `PastBlocksMass` where Core evaluates its event horizon: 1..=[`KGW_MAX_MASS`], the
/// whole domain of its `pow` call (`CS2-fp-release-binaries.md` §4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KgwMass(u16);

impl KgwMass {
    /// `mass` if it is in 1..=[`KGW_MAX_MASS`].
    pub fn new(mass: u64) -> Option<Self> {
        let mass = u16::try_from(mass).ok()?;
        (1..=KGW_MAX_MASS).contains(&mass).then_some(Self(mass))
    }
}

/// KGW's event-horizon test: whether the loop stops at `mass` because the rate ratio
/// `target_secs / actual_secs` reached the horizon `1 + 0.7084 · (mass / 28.2)^−1.228` or fell to
/// its inverse. `target_secs` is `PastRateTargetSeconds`, `actual_secs` is
/// `PastRateActualSeconds` after Core's clamp to ≥ 0 (a difference of two `uint32_t` times, so a
/// `u32`); when either is 0 the ratio is 1, inside the horizon.
///
/// The `pow` value comes from [`KGW_POW`], the rest is Core's expression with the horizon rounded
/// in two steps, as x86_64 does. Every shipped `pow`, under either rounding, makes the same
/// decision for every input (CS2 §2.4; quirk 40).
// Core: pow.cpp:49-62 KimotoGravityWell (both pins)
pub fn kgw_horizon_crossed(mass: KgwMass, target_secs: i64, actual_secs: u32) -> bool {
    let mut ratio = 1.0;
    if actual_secs != 0 && target_secs != 0 {
        ratio = target_secs as f64 / f64::from(actual_secs);
    }
    // pow(double(PastBlocksMass) / double(28.2), -1.228)
    let p = f64::from_bits(KGW_POW[usize::from(mass.0) - 1]);
    let fast = 1.0 + 0.7084 * p;
    let slow = 1.0 / fast;
    ratio <= slow || ratio >= fast
}

/// KGW's `pastSecondsMin`, `uint64_t(nPowTargetTimespan * 0.025)`: 2160 at Core's 86400.
/// `target_timespan` is positive ([`PowParams`](crate::pow::PowParams)).
// Core: pow.cpp:28 KimotoGravityWell (both pins)
pub fn kgw_past_seconds_min(target_timespan: i64) -> u64 {
    (target_timespan as f64 * 0.025) as u64
}
