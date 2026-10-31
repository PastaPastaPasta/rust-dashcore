//! CS3's benchmarks on Core's benchmark block block813851 (mainnet), each matched to a Dash Core
//! `bench_dash` row:
//!
//! - `deserialize_and_check_block`: `decode_block` then `check_block` with the proof of work and
//!   merkle checks. Core: `DeserializeAndCheckBlockTest` (deserialize, `CheckBlock`).
//! - `check_block`: `check_block` alone on a block decoded outside the timed loop (no Core row:
//!   Core's bench always deserializes, as its `CBlock` caches the checked state).
//!
//! Wall-clock comparisons with Core run only in a manager-reserved quiet window (DEC-23(b)).

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use dash_consensus::block::check_block;
use dash_consensus::decode_block;
use dash_consensus::header::HeaderParams;
use dash_core_vectors::read_core_file;
use dash_params::{ChainParamsBuilder, Network, Release};

fn checks(c: &mut Criterion) {
    let raw = read_core_file("src/bench/data/block813851.raw");
    let chain = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("mainnet");
    let params = HeaderParams::from_chain_params(&chain).expect("pow params");
    let block = decode_block(&mut &raw[..]).expect("decodes").into_value();
    check_block(&block, &params, true, true).expect("Core's benchmark block passes");

    let mut group = c.benchmark_group("block813851");
    group.throughput(Throughput::Bytes(raw.len() as u64));
    group.bench_function("deserialize_and_check_block", |b| {
        b.iter(|| {
            let block = decode_block(&mut black_box(&raw[..])).expect("decodes").into_value();
            check_block(&block, &params, true, true).expect("passes");
            block
        })
    });
    group.bench_function("check_block", |b| {
        b.iter(|| check_block(black_box(&block), &params, true, true))
    });
    group.finish();
}

criterion_group!(benches, checks);
criterion_main!(benches);
