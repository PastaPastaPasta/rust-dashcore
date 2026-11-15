//! Quirk 40 (DEC-29 D2, text per DEC-33 a): Core's release builds evaluate KGW's horizon with
//! different `pow` implementations (the host glibc, per x86_64 ifunc; win64's static mingw-w64
//! x87 `pow`, whose result depends on the thread's x87 precision control; Apple's libm) and with
//! separate (x86_64 Linux, win64, x86_64 macOS) or fused (aarch64 and riscv64 Linux, arm64 macOS)
//! rounding. At least four distinct `pow` tables ship, and the horizon values differ between
//! builds at 41 masses. Decisions are identical for every `pow` within 10⁻⁹ relative error; the
//! nearest input is 35,751 ulps from a boundary.
//!
//! The proof of CS2 §2.4, against the port's `fp::kgw_horizon_crossed`. The threshold search
//! (`common::kgw_thresholds`) relies on the predicate's shape, which follows from the expression
//! (a ratio monotone in `a`, a horizon above 1); test 3 checks the port next to each threshold.
//!
//! KGW at mass m (14..=4032 on mainnet) with `a` actual seconds breaks iff `ratio <= s` or
//! `ratio >= d`, where `ratio = fl(150m / a)` (1.0 when `a` is 0), `d = horizon(p)`,
//! `s = fl(1 / d)` and `p = pow(fl(m / 28.2), fl(-1.228))`. `ratio` is non-increasing in `a`, so the
//! breaks are exactly `a <= A_fast(m)` and `a >= A_slow(m)`: two integers per mass, found by
//! bisection over `a` in `1..=2^32 - 1` (a difference of two `uint32` times).
//!
//! 1. [`kgw_decisions_invariant_under_pow_error`]: the port's thresholds equal those of every
//!    `p` within relative error 10⁻⁹ of the exact `pow`, under both roundings of the horizon. The
//!    horizon is non-decreasing in `p` under both, `s` non-increasing, and the thresholds monotone
//!    in `d` and `s`, so equality at the two ends of the interval proves it for every `p` in it.
//! 2. [`quirk_40_kgw_fp_platform_invariance`]: so does every measured shipped table, under the
//!    rounding it ships with (`tests/data/kgw_pow/PROVENANCE.md`).
//! 3. [`kgw_horizon_matches_thresholds`]: the port at and next to each threshold.
//! 4. [`kgw_host_libm_is_a_known_variant`]: this host's glibc `pow` is the shipped table it should
//!    be, and decides the same.
//!
//! The reference implementation is the evidence's `tools/kgw_proof.py`.

mod common;

use common::{
    A_MAX, KGW_MASSES, KGW_SPACING, kgw_crossed, kgw_port_thresholds, kgw_thresholds,
    parse_kgw_pow_table,
};

/// The relative `pow` error the proof covers. The measured limit is 1.534×10⁻⁹ (at m = 2712).
const EPS: f64 = 1e-9;
/// Each end of the interval is moved this many ulps outward, which absorbs the rounding of
/// `p · (1 ± EPS)` and of `p` itself.
const WIDEN_ULPS: i64 = 4;

/// A `pow` table of `tests/data/kgw_pow/`, index `m - 1`.
fn table(name: &str) -> Vec<f64> {
    let path = format!("{}/tests/data/kgw_pow/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    parse_kgw_pow_table(&text).into_iter().map(f64::from_bits).collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Rounding {
    /// `fl(1 + fl(0.7084 · p))`: x86_64 Linux, win64, x86_64 macOS (and the port).
    Separate,
    /// `fl(1 + 0.7084 · p)`, one rounding: aarch64 and riscv64 Linux, arm64 macOS.
    Fused,
}

fn horizon(p: f64, rounding: Rounding) -> f64 {
    match rounding {
        Rounding::Separate => 1.0 + 0.7084 * p,
        Rounding::Fused => fused_one_plus(0.7084, p),
    }
}

/// `x = significand · 2^exponent` for a positive normal `x`.
fn decompose(x: f64) -> (u128, i32) {
    assert!(x.is_normal() && x > 0.0, "{x}");
    let bits = x.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1075;
    (u128::from((bits & ((1 << 52) - 1)) | (1 << 52)), exponent)
}

/// `2^k` for a normal power of two.
fn pow2(k: i32) -> f64 {
    assert!((-1022..=1023).contains(&k), "{k}");
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// `fl(1 + c · p)` with one rounding (round to nearest, ties to even), in exact integer
/// arithmetic: a fused multiply-add without `mul_add`, which `clippy.toml` bans
/// ([`fused_one_plus_equals_mul_add`] checks the two agree).
fn fused_one_plus(c: f64, p: f64) -> f64 {
    let ((sc, ec), (sp, ep)) = (decompose(c), decompose(p));
    // c · p = sc · sp · 2^e exactly (a 106-bit product); 1 = 2^(-e) · 2^e
    let e = ec + ep;
    assert!((-127..0).contains(&e), "the exponent {e} does not fit the sum in a u128");
    let sum = sc * sp + (1u128 << -e);
    let length = 128 - sum.leading_zeros() as i32;
    let shift = length - 53;
    assert!(shift > 0);
    let (mut q, rest) = (sum >> shift, sum & ((1u128 << shift) - 1));
    let half = 1u128 << (shift - 1);
    if rest > half || (rest == half && q & 1 == 1) {
        q += 1;
    }
    // q < 2^54, and an exact double times a power of two is exact here
    q as f64 * pow2(e + shift)
}

/// `(A_fast, A_slow)` for mass `m` and horizon `d`: the largest `a` with `fl(150m / a) >= d`
/// (0 if none) and the smallest with `fl(150m / a) <= s`, `s = fl(1 / d)` (`2^32` if none).
fn thresholds(m: u16, d: f64) -> (i64, i64) {
    let (t, s) = ((KGW_SPACING * i64::from(m)) as f64, 1.0 / d);
    kgw_thresholds(m, |a| {
        let ratio = t / a as f64;
        ratio <= s || ratio >= d
    })
}

/// `x` moved `n` ulps (positive `x`).
fn step(x: f64, n: i64) -> f64 {
    f64::from_bits(x.to_bits().checked_add_signed(n).expect("a step"))
}

/// The ends of the interval the proof covers around `p`: `p · (1 ± EPS)`, each moved
/// [`WIDEN_ULPS`] outward.
fn eps_interval(p: f64) -> (f64, f64) {
    (step(p * (1.0 - EPS), -WIDEN_ULPS), step(p * (1.0 + EPS), WIDEN_ULPS))
}

/// Test 1: the port's thresholds are those of every `pow` within 10⁻⁹ of the exact value, under
/// either rounding of the horizon.
#[test]
fn kgw_decisions_invariant_under_pow_error() {
    let cr = table("cr");
    let mut evaluations = 0;
    for m in KGW_MASSES {
        let p = cr[usize::from(m) - 1];
        let port = kgw_port_thresholds(m);
        // the port decides as the correctly rounded table with separate rounding
        assert_eq!(port, thresholds(m, horizon(p, Rounding::Separate)), "m = {m}");
        let (lo, hi) = eps_interval(p);
        for rounding in [Rounding::Separate, Rounding::Fused] {
            for end in [lo, hi] {
                let d = horizon(end, rounding);
                assert!(d > 1.0, "m = {m}: the threshold search needs a horizon above 1");
                let at_end = thresholds(m, d);
                assert_eq!(at_end, port, "m = {m}, {rounding:?}, pow = {end:e}");
                evaluations += 1;
            }
        }
    }
    assert_eq!(evaluations, 16_076);
}

/// Test 2, the register's test for quirk 40 (`docs/node-program/quirks.toml`): every measured
/// shipped table, under the rounding each ships with, decides as the port at every mass.
#[test]
fn quirk_40_kgw_fp_platform_invariance() {
    use Rounding::{Fused, Separate};
    let cr = table("cr");
    // (fixture, the roundings it ships with, masses where it differs from correctly rounded)
    let shipped = [
        ("glibc-fma", &[Separate, Fused][..], 6), // x86_64 FMA ifunc; aarch64, riscv64
        ("glibc-sse2", &[Separate][..], 5),       // x86_64 SSE2 ifunc
        ("mingw-win64-pc64", &[Separate][..], 4), // win64 at x87 64-bit precision
        ("mingw-win64-pc53", &[Separate][..], 3073), // win64 at 53-bit precision
    ];
    let mut horizons = vec![std::collections::BTreeSet::new(); 4032];
    for (name, roundings, differing) in shipped {
        let pow = table(name);
        let off = (0..4032).filter(|&i| pow[i] != cr[i]).count();
        assert_eq!(off, differing, "{name}: the fixture is not the measured one");
        for &rounding in roundings {
            for m in KGW_MASSES {
                let d = horizon(pow[usize::from(m) - 1], rounding);
                assert_eq!(thresholds(m, d), kgw_port_thresholds(m), "{name} {rounding:?} m = {m}");
                horizons[usize::from(m) - 1].insert((d.to_bits(), (1.0 / d).to_bits()));
            }
        }
    }
    // the shipped builds' horizons do differ (CS2-fp-release-binaries.md §4.1)
    let differing = horizons.iter().filter(|set| set.len() > 1).count();
    assert_eq!(differing, 41);
}

/// Test 3: the port breaks exactly at its thresholds: at 0, at and one past `A_fast`, and one
/// before and at `A_slow`.
#[test]
fn kgw_horizon_matches_thresholds() {
    let cr = table("cr");
    for m in KGW_MASSES {
        let (a_fast, a_slow) = thresholds(m, horizon(cr[usize::from(m) - 1], Rounding::Separate));
        assert!(!kgw_crossed(m, 0), "m = {m}: a ratio of 1 never breaks");
        for a in [a_fast, a_fast + 1, a_slow - 1, a_slow] {
            if (1..=A_MAX).contains(&a) {
                assert_eq!(kgw_crossed(m, a), a <= a_fast || a >= a_slow, "m = {m}, a = {a}");
            }
        }
    }
}

/// The exact fused rounding equals the hardware's (or libm's) `fma` on every value the proof
/// feeds it.
#[test]
#[expect(clippy::disallowed_methods, reason = "the reference for the exact fused rounding")]
fn fused_one_plus_equals_mul_add() {
    let cr = table("cr");
    for &p in &cr {
        let (lo, hi) = eps_interval(p);
        for x in [p, lo, hi, step(p, 1), step(p, -1)] {
            assert_eq!(fused_one_plus(0.7084, x).to_bits(), 0.7084f64.mul_add(x, 1.0).to_bits());
        }
    }
}

/// Test 4: this host's glibc `pow` (what an x86_64, aarch64 or riscv64 Linux dashd calls here) is
/// the shipped table it must be ([`common::expected_glibc_pow`]), so a run says which variant it
/// measured.
#[test]
#[cfg(all(
    target_os = "linux",
    target_env = "gnu",
    any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64")
))]
fn kgw_host_libm_is_a_known_variant() {
    let host: Vec<f64> = common::host_kgw_pow().into_iter().map(f64::from_bits).collect();
    let cr = table("cr");
    match common::expected_glibc_pow() {
        Some(name) => {
            let fixture = table(name);
            let off: Vec<usize> =
                (0..4032).filter(|&i| host[i] != fixture[i]).map(|i| i + 1).collect();
            assert!(off.is_empty(), "the host's pow is not {name}: it differs at masses {off:?}");
            println!("host glibc pow: {name}");
        }
        None => println!("host glibc pow: the FMA4 variant (no fixture)"),
    }
    for m in KGW_MASSES {
        let (p, exact) = (host[usize::from(m) - 1], cr[usize::from(m) - 1]);
        let (lo, hi) = eps_interval(exact);
        assert!((lo..=hi).contains(&p), "m = {m}: the host's pow is outside the 10⁻⁹ interval");
        assert_eq!(
            thresholds(m, horizon(p, Rounding::Separate)),
            kgw_port_thresholds(m),
            "m = {m}"
        );
    }
}
