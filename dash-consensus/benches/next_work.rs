//! `crit next_work` (CS2 §3.9, §6.5): GetNextWorkRequired on DGW, a BTC retarget and the
//! min-difficulty walk, over windows from the recorded Core corpus, and on KGW over two of its
//! boundary windows (its worst case, all 4032 blocks, and a break at mass 2712); then the
//! subsidy (fixed and from a formula) and the masternode payment.

#[path = "../tests/common/mod.rs"]
mod common;

use std::hint::black_box;

use common::{
    VecChain, chain_params, kgw_boundary_window, kgw_port_thresholds, oracle_cases, payment_params,
    pow_params, subsidy_params, uint32,
};
use criterion::{Criterion, criterion_group, criterion_main};
use dash_consensus::HeaderCtx;
use dash_consensus::payments::{MnRewardEra, masternode_payment};
use dash_consensus::pow::{PowParams, get_next_work_required};
use dash_consensus::subsidy::block_subsidy_helper;

/// The first recorded window on `network` that takes the path `takes` describes.
fn window(
    network: &str,
    takes: impl Fn(&[HeaderCtx], u32, &PowParams) -> bool,
) -> (VecChain, u32, PowParams) {
    let p = pow_params(&chain_params("v24.0", network));
    oracle_cases("next_work.json")
        .iter()
        .filter(|case| case["chain"] == network)
        .map(|case| (VecChain::from_case(case), uint32(&case["new_time"])))
        .find(|(chain, new_time)| takes(chain.headers(), *new_time, &p))
        .map(|(chain, new_time)| (chain, new_time, p))
        .unwrap_or_else(|| panic!("no such {network} window"))
}

fn next_height(headers: &[HeaderCtx]) -> i64 {
    i64::from(headers[headers.len() - 1].height) + 1
}

fn bench(c: &mut Criterion) {
    let interval = |p: &PowParams| p.config().target_timespan / p.config().target_spacing;
    // KGW: no break before the 4032nd block (a = A_fast + 1 at mass 4031), and a break at 2712
    let main = pow_params(&chain_params("v24.0", "main"));
    let kgw = |m: u16, a: i64| {
        let (first, headers, new_time) = kgw_boundary_window(m, a);
        (VecChain::new(first, headers), new_time, main)
    };
    let windows = [
        ("dgw", window("main", |h, _, p| next_height(h) >= i64::from(p.config().dgw_height))),
        (
            "btc_retarget",
            window("main", |h, _, p| {
                next_height(h) < i64::from(p.config().kgw_height)
                    && next_height(h) % interval(p) == 0
            }),
        ),
        (
            "min_difficulty_walk",
            window("test", |h, new_time, p| {
                let last = h[h.len() - 1];
                next_height(h) < i64::from(p.config().kgw_height)
                    && next_height(h) % interval(p) != 0
                    && i64::from(new_time) <= i64::from(last.time) + 2 * p.config().target_spacing
                    && last.bits == p.pow_limit_bits()
                    && h.len() > 10
            }),
        ),
        // PF0's workload `kgw-next-work-4032` (benches/README.md): keep the ID
        ("kgw-next-work-4032", kgw(4031, kgw_port_thresholds(4031).0 + 1)),
        ("kgw_break_at_2712", kgw(2712, kgw_port_thresholds(2712).0)),
    ];
    let mut group = c.benchmark_group("next_work");
    for (name, (chain, new_time, p)) in &windows {
        group.bench_function(*name, |b| {
            b.iter(|| {
                get_next_work_required(black_box(chain), chain.tip(), black_box(*new_time), p)
            })
        });
    }
    group.finish();

    let sub = subsidy_params(&chain_params("v24.0", "main"));
    c.bench_function("subsidy_v20", |b| {
        b.iter(|| block_subsidy_helper(black_box(0x1903_a30c), black_box(2_400_000), true, &sub))
    });
    c.bench_function("subsidy_formula", |b| {
        b.iter(|| block_subsidy_helper(black_box(0x1b0f_ffff), black_box(30_000), false, &sub))
    });
    let pay = payment_params(&chain_params("v24.0", "main"));
    c.bench_function("masternode_payment", |b| {
        b.iter(|| {
            masternode_payment(
                black_box(2_400_000),
                black_box(361_111_111),
                MnRewardEra::EvoReward,
                &pay,
            )
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
