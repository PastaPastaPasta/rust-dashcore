//! The floating-point legs of CS2's verification (design §2.4, §6.2, §6.3): KGW's boundary
//! windows, `ConvertBitsToDouble` and the mainnet ≤ 68589 tolerance against Core's own doubles,
//! and the exhaustive sweeps over every `nBits`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use dash_consensus::fp::{Difficulty, KGW_MAX_MASS, within_pre_dgw_tolerance};
use dash_consensus::subsidy;
use dash_oracle_sys::{Chain, Oracle, Pin};
use serde_json::{Value, json};

use crate::cases::{self, NextWorkCase, Variant};
use crate::common::{
    self, ENCODINGS_OF_2_232, KGW_MASSES, KGW_SPACING, kgw_boundary_window, kgw_port_thresholds,
};
use crate::rng::Rng;
use crate::{NONE, Tally, both, check_next_work};

/// Which glibc `pow` this process calls, which is the one Core's KGW calls inside the oracle: the
/// process's table over KGW's 4032 masses must be the shipped table glibc's ifunc selection and
/// `GLIBC_TUNABLES` imply (`common::expected_glibc_pow`, as `dash-consensus`'s test
/// `kgw_host_libm_is_a_known_variant`). Returns the fixture's name; a run under a tunable spelling
/// glibc ignores fails here.
pub fn host_pow_variant() -> &'static str {
    let expected =
        common::expected_glibc_pow().expect("glibc's FMA4 pow has no fixture to check against");
    let path = format!("dash-consensus/tests/data/kgw_pow/{expected}.txt");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (host, fixture) = (common::host_kgw_pow(), common::parse_kgw_pow_table(&text));
    let off: Vec<usize> = (0..4032).filter(|&i| host[i] != fixture[i]).map(|i| i + 1).collect();
    assert!(off.is_empty(), "the process's pow is not {expected}: it differs at masses {off:?}");
    eprintln!("glibc pow in this process (and in the oracle's KGW): {expected}");
    expected
}

/// KGW's boundary windows: at every mass m, Core and the port at `A_fast(m)`, `A_fast(m) + 1`,
/// `A_slow(m) − 1` and `A_slow(m)`, the port's thresholds. Both pins, with the process's glibc
/// `pow` checked first; a run under `GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2,-FMA` checks glibc's
/// SSE2 variant. `main` is mainnet's [`Variant`]. Returns `[m, a, Core's nBits]` per case.
///
/// The generator's two invariants (CS2 §2.4): the regular spacing keeps every other mass inside
/// the horizon, and each pair is non-vacuous: Core's `nBits` differ across it. At m = 4032 the
/// loop ends either way, so whether it breaks there cannot show, and only that mass is exempt.
pub fn kgw_boundaries(main: &Variant, tally: &mut Tally) -> Vec<Value> {
    host_pow_variant();
    let mut cases = Vec::new();
    for m in KGW_MASSES {
        let (a_fast, a_slow) = kgw_port_thresholds(m);
        assert!(a_fast < KGW_SPACING * (i64::from(m) - 1), "regular spacing breaks at m = {m}");
        let mut bits = Vec::new();
        for a in [a_fast, a_fast + 1, a_slow - 1, a_slow] {
            let (first, headers, new_time) = kgw_boundary_window(m, a);
            let case = NextWorkCase::new(first, headers, new_time);
            assert!(cases::reaches_kgw(&main.pow, &case));
            let core = check_next_work(main, &case, tally).expect("a KGW window Core reads");
            cases.push(json!([m, a, core]));
            bits.push(core);
        }
        if m < KGW_MAX_MASS {
            assert_ne!(bits[0], bits[1], "m = {m}: the fast-side pair is vacuous");
            assert_ne!(bits[2], bits[3], "m = {m}: the slow-side pair is vacuous");
            tally.add("kgw: non-vacuous boundary pairs");
        }
    }
    cases
}

/// Core's `ConvertBitsToDouble(bits)` at the pin, as a bit pattern.
pub fn core_difficulty(bits: u32) -> u64 {
    both("ConvertBitsToDouble", NONE, |o| o.convert_bits_to_double(bits).map(f64::to_bits))
        .expect("ConvertBitsToDouble refuses nothing")
}

/// `fp::Difficulty::from_bits` against Core's `ConvertBitsToDouble`, bit for bit; Core's value.
pub fn check_difficulty(bits: u32) -> u64 {
    let core = core_difficulty(bits);
    assert_eq!(Difficulty::from_bits(bits).to_bits(), core, "ConvertBitsToDouble({bits:#010x})");
    core
}

/// A random word, half of them with a valid header's shape (size byte 1..=34).
pub fn random_bits(rng: &mut Rng) -> u32 {
    if rng.chance(1, 2) {
        rng.next() as u32
    } else {
        (rng.range(1, 34) as u32) << 24 | rng.below(1 << 24) as u32
    }
}

/// Words for `ConvertBitsToDouble`'s edges: every size byte with a zero, one, small, sign-bit
/// and all-ones mantissa, and the three encodings of 2^232.
pub fn difficulty_edges() -> Vec<u32> {
    let mut words = Vec::new();
    for size in 0..=255u32 {
        for mantissa in [0, 1, 0xffff, 0x7f_ffff, 0x80_0000, 0x80_0001, 0xff_ffff] {
            words.push(size << 24 | mantissa);
        }
    }
    words.extend(ENCODINGS_OF_2_232);
    words
}

/// The literal tolerance predicate on Core's own doubles: `!(|n1 − n2| > n1 · 0.5)`.
#[expect(clippy::neg_cmp_op_on_partial_ord, reason = "Core's predicate, false on NaN")]
fn core_tolerance(n1: u64, n2: u64) -> bool {
    let (n1, n2) = (f64::from_bits(n1), f64::from_bits(n2));
    !((n1 - n2).abs() > n1 * 0.5)
}

/// `fp::within_pre_dgw_tolerance` against Core's predicate applied to Core's
/// `ConvertBitsToDouble` (design §6.2 `diff_nbits_acceptable`); Core's verdict.
pub fn check_tolerance(header: u32, expected: u32) -> bool {
    let core = core_tolerance(core_difficulty(header), core_difficulty(expected));
    let ours = within_pre_dgw_tolerance(header, expected);
    assert_eq!(ours, core, "tolerance({header:#010x}, {expected:#010x})");
    core
}

/// A random tolerance pair: unrelated words, or neighbours of a word.
pub fn tolerance_pair(rng: &mut Rng) -> (u32, u32) {
    let header = random_bits(rng);
    let expected = match rng.below(3) {
        0 => random_bits(rng),
        1 => header.wrapping_add(rng.range(-0x40_0000, 0x40_0000) as u32),
        _ => header ^ (1 << rng.below(32)),
    };
    (header, expected)
}

/// Labelled tolerance pairs at the boundary (design §6.2): Core's `|n1 − n2| > n1 · 0.5` at
/// equality, which accepts, and one mantissa step beyond, which rejects, on both sides
/// (`n2 = n1 / 2`, `n2 = 1.5 · n1`; with mantissas 0xffff, 0x1fffe and 0xaaaa the doubles are
/// exact); zero mantissas (+∞, and NaN for two); the size bytes Core's loop scales most.
pub fn tolerance_edges() -> Vec<(&'static str, u32, u32)> {
    let mut pairs = Vec::new();
    for size in [0x01u32, 0x1d, 0x1e, 0x20, 0x22] {
        let w = |mantissa: u32| size << 24 | mantissa;
        pairs.extend([
            ("half: at the boundary", w(0xffff), w(0x1_fffe)),
            ("half: beyond", w(0xffff), w(0x1_ffff)),
            ("half: inside", w(0xffff), w(0x1_fffd)),
            ("one and a half: at the boundary", w(0xffff), w(0xaaaa)),
            ("one and a half: beyond", w(0xffff), w(0xaaa9)),
            ("one and a half: inside", w(0xffff), w(0xaaab)),
            ("equal", w(0xffff), w(0xffff)),
            ("zero expected target", w(0xffff), w(0)),
            ("zero header mantissa (no PoW)", w(0), w(0xffff)),
            ("zero mantissas (NaN, no PoW)", w(0), w(0)),
        ]);
    }
    for word in [0x1f00_0100, 0x2000_0001] {
        pairs.push(("non-canonical, same target", word, 0x1e01_0000));
    }
    pairs
}

/// Every word in `[from, to)` through `fp::Difficulty::from_bits` against Core's
/// `ConvertBitsToDouble` at the pin, bit for bit, on `threads` threads (design §6.3). Each
/// word's difficulty and early-mainnet difficulty also go through the three subsidy formulas,
/// whose debug assertions check that every base lies in [0, 1111] (design §2.3).
pub fn exhaustive_bits(threads: usize, (from, to): (u64, u64)) {
    assert!(from < to && to <= 1 << 32 && threads > 0);
    const CHUNK: u64 = 1 << 22;
    let next = AtomicU64::new(from);
    let done = AtomicU64::new(0);
    let start = Instant::now();
    let pins: Vec<Oracle> = Oracle::all().collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let lo = next.fetch_add(CHUNK, Ordering::Relaxed);
                    if lo >= to {
                        break;
                    }
                    for bits in lo..(lo + CHUNK).min(to) {
                        let bits = bits as u32;
                        let d = Difficulty::from_bits(bits);
                        for o in &pins {
                            let core = o.convert_bits_to_double(bits).expect("refuses nothing");
                            assert_eq!(d.to_bits(), core.to_bits(), "{}: {bits:#010x}", o.pin());
                        }
                        for d in [d, Difficulty::from_bits_mainnet_early(bits)] {
                            let _ = (
                                d.subsidy_base_early(),
                                d.subsidy_base_cpu(),
                                d.subsidy_base_gpu(),
                            );
                        }
                    }
                    let n = done.fetch_add((lo + CHUNK).min(to) - lo, Ordering::Relaxed);
                    progress(n + CHUNK.min(to - lo), to - from, start);
                }
            });
        }
    });
    let n = done.load(Ordering::Relaxed);
    assert_eq!(n, to - from);
    println!("{n:>12}  ConvertBitsToDouble: equal on both pins, words [{from:#x}, {to:#x})");
    println!("{n:>12}  subsidy bases in [0, 1111] (debug assertions), six per word");
}

/// Every word in `[from, to)` as the previous block's `nBits` at `prev_height` on `chain` (main,
/// test or regtest), before V20: the port's `GetBlockSubsidyInner` against Core's at the pin
/// (design §6.3), one thread per pin.
pub fn exhaustive_subsidy(chain: &str, prev_height: i32, (from, to): (u64, u64)) {
    assert!(from < to && to <= 1 << 32);
    let name = cases::VARIANTS.iter().find(|s| s.0 == chain).expect("main, test or regtest").0;
    let chain = Chain::new(chain, &[]);
    let cp = cases::chainparams(&Oracle::new(Pin::V24_0), &chain);
    let v = Variant::new(name, Pin::V24_0.name(), chain.clone(), cp);
    let start = Instant::now();
    std::thread::scope(|s| {
        for o in Oracle::all() {
            let (chain, sub) = (&chain, &v.subsidy);
            s.spawn(move || {
                for bits in from..to {
                    let bits = bits as u32;
                    let core =
                        o.block_subsidy_inner(chain, bits, prev_height, false).expect("a subsidy");
                    let ours = subsidy::block_subsidy_inner(bits, prev_height, false, sub);
                    assert_eq!(
                        ours,
                        Ok(core),
                        "{}: {} {prev_height} {bits:#010x}",
                        o.pin(),
                        chain.name
                    );
                    if o.pin() == Pin::V24_0 && bits & 0x3ff_ffff == 0x3ff_ffff {
                        progress(u64::from(bits) + 1 - from, to - from, start);
                    }
                }
            });
        }
    });
    println!(
        "{:>12}  subsidy: equal on both pins, {} prev_height {prev_height}, words [{from:#x}, {to:#x})",
        to - from,
        chain.name
    );
}

fn progress(done: u64, total: u64, start: Instant) {
    let secs = start.elapsed().as_secs_f64();
    let rate = done as f64 / secs.max(1e-9);
    eprintln!(
        "  {:5.1}%  {done}/{total}  {:.1} M/s  eta {:.0} s",
        100.0 * done as f64 / total as f64,
        rate / 1e6,
        (total - done) as f64 / rate.max(1e-9)
    );
}
