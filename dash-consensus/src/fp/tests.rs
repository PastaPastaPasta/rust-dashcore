//! `fp`'s own invariants. The KGW decision proof is `tests/quirk_40_kgw_fp_platform_invariance.rs`,
//! and Core's values for the subsidy and the tolerance come from the oracle (`tests/`).

use super::*;

/// The generated table is the evidence's correctly rounded one (`tests/data/kgw_pow/cr.txt`,
/// `m hex(x) hex(pow)`), and that table's `x` column is KGW's `fl(m / 28.2)`.
#[test]
fn kgw_pow_is_the_correctly_rounded_table() {
    let cr = include_str!("../../tests/data/kgw_pow/cr.txt");
    let hex = |s: &str| u64::from_str_radix(s, 16).expect("hex");
    assert_eq!(cr.lines().count(), KGW_POW.len());
    for (line, m) in cr.lines().zip(1u32..) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields[0], m.to_string(), "{line}");
        assert_eq!(hex(fields[1]), (f64::from(m) / 28.2).to_bits(), "x at m = {m}");
        assert_eq!(hex(fields[2]), KGW_POW[m as usize - 1], "pow at m = {m}");
    }
}

#[test]
fn kgw_mass_is_the_pow_domain() {
    assert_eq!(KgwMass::new(0), None);
    assert_eq!(KgwMass::new(1), Some(KgwMass(1)));
    assert_eq!(KgwMass::new(4032), Some(KgwMass(KGW_MAX_MASS)));
    for mass in [4033, 65_536, u64::MAX] {
        assert_eq!(KgwMass::new(mass), None, "{mass}");
    }
}

/// `86400 * 0.025` rounds to exactly 2160.0 in binary64, so every build converts it to 2160
/// (`CS2-fp-release-binaries.md` §3.2).
#[test]
fn kgw_past_seconds_min_at_core_timespan() {
    assert_eq!(kgw_past_seconds_min(86_400), 2160);
}

/// The difficulty is ≥ 0 or +∞ for every word, so each base lies in [0, 1111] (CS2 §2.3): the
/// extremes are a zero mantissa (+∞, so a base of 0) and the smallest difficulty, which
/// underflows to 0 (a base of 1111 from the early formula). The expectations are hand-written;
/// Core's subsidy for every word is `cs2-diff exhaustive subsidy` (`tests/data/PROVENANCE.md`).
#[test]
fn subsidy_bases_at_the_extremes() {
    for bits in [0, 0x0100_0000, 0x1d00_0000, 0xff00_0000] {
        let d = Difficulty::from_bits(bits);
        assert_eq!(d.to_bits(), f64::INFINITY.to_bits(), "{bits:#x}");
        assert_eq!((d.subsidy_base_early(), d.subsidy_base_cpu(), d.subsidy_base_gpu()), (0, 0, 0));
    }
    // mantissa 2^23 - 1, then 226 divisions by 256
    let d = Difficulty::from_bits(0xff7f_ffff);
    assert_eq!(d.to_bits(), 0);
    assert_eq!(d.subsidy_base_early(), 1111);
}

/// The tolerance accepts where Core's `>` is false: NaN (both mantissas zero) and `n1 = +∞`. The
/// expectations are hand-written; quirk 9's test has Core's doubles at the boundary.
#[test]
fn tolerance_accepts_nan_and_infinite_header_difficulty() {
    assert!(within_pre_dgw_tolerance(0x1d00_0000, 0x1d00_0000)); // ∞ − ∞ = NaN
    assert!(within_pre_dgw_tolerance(0x1d00_0000, 0x1d00_ffff)); // n1 = ∞
    assert!(!within_pre_dgw_tolerance(0x1d00_ffff, 0x1d00_0000)); // n2 = ∞, n1 finite
    assert!(within_pre_dgw_tolerance(0x1d00_ffff, 0x1d00_ffff));
}
