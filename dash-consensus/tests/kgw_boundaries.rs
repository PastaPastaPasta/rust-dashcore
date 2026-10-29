//! KGW at both sides of every decision boundary, against Core (CS2 §2.4's differential, recorded):
//! for each mass m in 14..=4032, the mainnet windows of `common::kgw_boundary_window` that make
//! KGW evaluate m at `A_fast(m)`, `A_fast(m) + 1`, `A_slow(m) − 1` and `A_slow(m)` actual seconds,
//! with Core's `GetNextWorkRequired` on both pins (`tests/data/oracle/kgw_boundaries.json`,
//! recorded by `cs2-diff` with the host glibc's FMA `pow`; `cs2-diff kgw` reruns it under the SSE2
//! one). The thresholds are the port's, and Core's answers change across each pair, so Core
//! breaks exactly where the port does.
//!
//! A debug build replays the masses where the horizon moves fastest (14..=64), every 16th one
//! beyond, the binding mass 2712 (CS2-fp-release-binaries.md §4.2) and the last two: about 1,200
//! of the 16,076 windows. `cargo test --release` replays every one, and so does `cs2-diff`.

mod common;

use std::collections::BTreeMap;

use common::{
    KGW_MASSES, VecChain, chain_params, int, kgw_boundary_window, kgw_port_thresholds,
    oracle_cases, pow_params, uint32,
};
use dash_consensus::pow::get_next_work_required;
use dash_types::Numeric;

#[test]
fn kgw_breaks_where_core_does() {
    let p = pow_params(&chain_params("v24.0", "main"));
    let mut by_mass: BTreeMap<u16, Vec<(i64, u32)>> = BTreeMap::new();
    for case in oracle_cases("kgw_boundaries.json") {
        let m = u16::try_from(int(&case[0])).expect("a mass");
        by_mass.entry(m).or_default().push((int(&case[1]), uint32(&case[2])));
    }
    assert_eq!(by_mass.keys().copied().collect::<Vec<_>>(), KGW_MASSES.collect::<Vec<_>>());
    let replayed = |m: u16| {
        !cfg!(debug_assertions)
            || m <= 64
            || m.is_multiple_of(16)
            || [2712, 4031, 4032].contains(&m)
    };
    for (m, cases) in by_mass.into_iter().filter(|&(m, _)| replayed(m)) {
        // the recorded windows are at the port's thresholds
        let (a_fast, a_slow) = kgw_port_thresholds(m);
        let a: Vec<i64> = cases.iter().map(|c| c.0).collect();
        assert_eq!(a, [a_fast, a_fast + 1, a_slow - 1, a_slow], "m = {m}");
        for &(a, core) in &cases {
            let (first, headers, new_time) = kgw_boundary_window(m, a);
            let chain = VecChain::new(first, headers);
            let bits = get_next_work_required(&chain, chain.tip(), new_time, &p);
            assert_eq!(bits.to_base(), core, "m = {m}, a = {a}");
        }
        // Core's answer changes across each pair (at m = 4032 the loop ends either way)
        if m < 4032 {
            assert_ne!(cases[0].1, cases[1].1, "m = {m}: fast side");
            assert_ne!(cases[2].1, cases[3].1, "m = {m}: slow side");
        }
    }
}
