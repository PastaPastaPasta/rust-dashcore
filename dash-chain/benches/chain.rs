//! PF0 benches for `dash-chain` (SB4 §16). Bench IDs: `cargo bench -p dash-chain --bench chain
//! -- --list`.
//!
//! They run on `support::fixture`'s mainnet-shaped node index. The `accept/*` benches (A-25)
//! accept real H-M headers from committed slices (`support::real`, `benches/data/README.md`).

mod support;

use std::cell::RefCell;
use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_chain::BlockId;
use dash_primitives::BlockHash;
use support::fixture::{Lcg, MAINNET_HEADERS, Records, Setup, mainnet_chain, synthetic_hash};
use support::real::{self, Slice};

/// DGW's window (§16): Core's recorded `--bench-pnbh` is
/// `tests/data/core-scenarios/bench-pnbh-mainnet-dgw.json.gz`.
const DGW_FROM: u32 = 2_000_000;
/// KGW's window: `bench-pnbh-mainnet-kgw.json.gz`.
const KGW_FROM: u32 = 20_000;
/// The reference node's `chainwork` at `DGW_FROM − 1` (`benches/data/README.md`).
const DGW_ANCHOR_WORK: u128 = 0x8f6a_7c45_9d4e_1aea_a76b;

fn lookup(c: &mut Criterion) {
    let mut g = c.benchmark_group("lookup");
    // 10,000 probes of the mainnet-sized index, alternating hits at random heights and misses.
    let mut fx = None;
    g.bench_function("hit-miss-10k", |b| {
        let (chain, probes) = fx.get_or_insert_with(|| {
            let mut rng = Lcg(0x5b4_100c);
            let probes: Vec<BlockHash> = (0..10_000u32)
                .map(|k| match k % 2 {
                    0 => synthetic_hash(1 + rng.below(MAINNET_HEADERS - 1)),
                    _ => synthetic_hash(MAINNET_HEADERS + rng.below(u32::MAX - MAINNET_HEADERS)),
                })
                .collect();
            (mainnet_chain().0, probes)
        });
        let reader = chain.reader();
        b.iter(|| probes.iter().filter(|h| reader.lookup(black_box(h)).is_some()).count());
    });
    g.finish();
}

fn locator(c: &mut Criterion) {
    let mut g = c.benchmark_group("locator");
    // `GetLocator` at the mainnet-height tip.
    let mut fx = None;
    g.bench_function("mainnet-tip", |b| {
        let (chain, tip) = fx.get_or_insert_with(mainnet_chain);
        let (reader, tip) = (chain.reader(), *tip);
        b.iter(|| reader.locator(Some(black_box(tip))));
    });
    g.finish();
}

fn ancestor(c: &mut Criterion) {
    let mut g = c.benchmark_group("ancestor");
    // 1,000 `GetAncestor` walks from random heights to random lower heights.
    let mut fx = None;
    g.bench_function("mainnet-random-1k", |b| {
        let (chain, walks) = fx.get_or_insert_with(|| {
            let (chain, tip) = mainnet_chain();
            let ancestry = chain.complete().expect("a node chain is complete");
            let mut rng = Lcg(0x5b4_a4c);
            let walks: Vec<(BlockId, u32)> = (0..1_000)
                .map(|_| {
                    let from = 1 + rng.below(MAINNET_HEADERS - 1);
                    let id = ancestry.ancestor(tip, from).expect("on the chain");
                    (id, rng.below(from + 1))
                })
                .collect();
            (chain, walks)
        });
        let ancestry = chain.complete().expect("a node chain is complete");
        b.iter(|| walks.iter().filter_map(|&(id, h)| ancestry.ancestor(black_box(id), h)).count());
    });
    g.finish();
}

fn tip(c: &mut Criterion) {
    let mut g = c.benchmark_group("tip");
    // 1,000 `connect_tip` steps onto the mainnet-height chain, each publishing a snapshot. The
    // setup disconnects them again, outside the region.
    let mut fx = None;
    g.bench_function("connect-publish-1k", |b| {
        // Every call leaves the chain at its tip again.
        let (chain, ids) = fx.get_or_insert_with(|| {
            let (chain, tip) = mainnet_chain();
            let ancestry = chain.complete().expect("a node chain is complete");
            let ids: Vec<BlockId> = (MAINNET_HEADERS - 1_000..MAINNET_HEADERS)
                .map(|h| ancestry.ancestor(tip, h).expect("on the chain"))
                .collect();
            (RefCell::new(chain), ids)
        });
        b.iter_batched(
            || {
                let mut chain = chain.borrow_mut();
                for _ in ids.iter() {
                    chain.disconnect_tip().expect("above genesis");
                }
            },
            |()| {
                let mut chain = chain.borrow_mut();
                for &id in ids.iter() {
                    black_box(chain.connect_tip(id).expect("prev is the tip"));
                }
            },
            BatchSize::PerIteration,
        );
    });
    g.finish();
}

fn load(c: &mut Criterion) {
    let mut g = c.benchmark_group("load");
    // `LoadBlockIndex` over the first 262,144 records (four decode batches) in hash order, as the
    // block index's key order returns them: unrelated to height, so ids are mostly assigned at
    // a record's first mention as a prev.
    let mut fx = None;
    g.bench_function("records-256k", |b| {
        let (s, records) = fx.get_or_insert_with(|| {
            let s = Setup::mainnet();
            let mut records: Records = s.records(1 << 18).collect();
            records.sort_by_key(|(hash, _)| *hash);
            (s, records)
        });
        b.iter_batched(|| records.clone(), |r| s.load(r), BatchSize::PerIteration);
    });
    g.finish();
}

fn accept(c: &mut Criterion) {
    let mut g = c.benchmark_group("accept");
    // Every iteration needs a fresh pre-state, and a node's is a `load` of up to 2M records.
    g.sample_size(10);
    // A-25's BG gate: 2,000 headers from 2,000,000 (DGW) onto H-M's heights below them, hashed
    // and accepted, against Core's `ProcessNewBlockHeaders` on the same pre-state.
    for (id, slice, from) in [
        ("mainnet-dgw-2000", Slice::dgw as fn() -> Slice, DGW_FROM),
        // 2,000 headers from 20,000, under KGW's walk of up to 4,032 entries.
        ("mainnet-kgw-2000", Slice::kgw, KGW_FROM),
    ] {
        let mut fx = None;
        g.bench_function(id, |b| {
            let (s, slice) = fx.get_or_insert_with(|| (Setup::mainnet(), slice()));
            b.iter_batched_ref(
                || (real::node_chain(s, slice.node_records(s, from)), slice.batch(from)),
                |(chain, (headers, raw))| {
                    real::hash_and_accept(chain, std::mem::take(headers), raw)
                },
                BatchSize::PerIteration,
            );
        });
    }
    // The DGW batch in SPV, anchored at 1,999,999 with w = 10,000 (§11.1).
    let mut fx = None;
    g.bench_function("spv-anchored-w10000-2000", |b| {
        let (s, slice, pack) = fx.get_or_insert_with(|| {
            let slice = Slice::dgw();
            let pack = slice.pack(DGW_FROM, DGW_ANCHOR_WORK);
            (Setup::mainnet(), slice, pack)
        });
        b.iter_batched_ref(
            || (real::spv_anchored(s, pack), slice.batch(DGW_FROM)),
            |(chain, (headers, raw))| real::hash_and_accept(chain, std::mem::take(headers), raw),
            BatchSize::PerIteration,
        );
    });
    g.finish();
}

criterion_group!(benches, lookup, locator, ancestor, tip, load, accept);
criterion_main!(benches);
