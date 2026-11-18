//! Core's recovery sum (`src/dashbls/src/threshold.cpp:190-248`, v24.0.0) at LLMQ thresholds.

use std::env;
use std::hint::black_box;

use criterion::{BenchmarkId, Criterion};
use dash_llmq::recovery::{recover_msm, recover_naive};
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_pkc::bls::BlsSchemeKind;
use dash_pkc::bls::core::{BlsSigElem, BlsSigValue, BlsSkValue, sign};

type Recover = fn(&[&BlsSigValue], &[[u8; 32]]) -> Option<BlsSigValue>;

fn fixture(n: usize) -> (Vec<[u8; 32]>, Vec<BlsSigValue>) {
    let mut one = [0; 32];
    one[31] = 1;
    let key = BlsSkValue::from_canonical(&one, BlsSchemeKind::Basic).unwrap();
    let point = BlsSigElem::from_impl(&sign(&key, &[0x42; 32], BlsSchemeKind::Basic));
    let ids: Vec<_> = (1..=n)
        .map(|i| {
            let mut id = [0; 32];
            id[24..].copy_from_slice(&(i as u64).to_be_bytes());
            id
        })
        .collect();
    let sigs = (1..=n)
        .map(|i| {
            let mut scalar = [0; 32];
            scalar[24..].copy_from_slice(&(5 + 7 * i as u64).to_be_bytes());
            point.mul(&scalar).into()
        })
        .collect();
    (ids, sigs)
}

fn main() {
    let perf: Option<Recover> = match env::args().nth(1).as_deref() {
        Some("--perf-naive") => Some(recover_naive),
        Some("--perf-msm") => Some(recover_msm),
        _ => None,
    };
    if let Some(recover) = perf {
        let (ids, sigs) = fixture(240);
        let refs: Vec<_> = sigs.iter().collect();
        let iterations: usize = env::args().nth(2).map_or(1000, |n| n.parse().unwrap());
        for _ in 0..iterations {
            black_box(recover(black_box(&refs), black_box(&ids))).unwrap();
        }
        return;
    }
    let setup = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().unwrap();
    let mut criterion = Criterion::default().configure_from_args();
    let mut group = criterion.benchmark_group("recovery");
    for (name, t) in [
        ("50_60", LlmqType::Llmq50_60),
        ("60_75", LlmqType::Llmq60_75),
        ("100_67", LlmqType::Llmq100_67),
        ("400_60", LlmqType::Llmq400_60),
        ("400_85", LlmqType::Llmq400_85),
    ] {
        let (ids, sigs) = fixture(usize::from(setup.consensus.llmq(t).unwrap().threshold));
        let refs: Vec<_> = sigs.iter().collect();
        for (method, recover) in [("naive", recover_naive as Recover), ("msm", recover_msm)] {
            group.bench_with_input(BenchmarkId::new(method, name), &(), |b, _| {
                b.iter(|| black_box(recover(black_box(&refs), black_box(&ids))).unwrap());
            });
        }
    }
    group.finish();
    criterion.final_summary();
}
