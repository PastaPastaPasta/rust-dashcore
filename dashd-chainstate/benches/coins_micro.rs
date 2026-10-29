//! `coins_micro` (CH2 §5's PF0 table; CH2a a2): the coins cache's per-op costs, gated on
//! instructions and allocations by PF0's counter gate (`crit:dashd-chainstate coins_micro`).
//!
//! - `packed/*`: [`PackedCoin`] encode from an output (inline P2PKH, a 200-byte long script) and
//!   decode from stored bytes (§2.2's encode cost).
//! - `tip/*`: [`CoinsCache`] `add`, `fetch` from active, from F and from the database, `spend`, and
//!   a freeze with its release (W's write is not in it).
//! - `overlay/block_1k`: a [`BlockOverlay`] spending 1,000 tip coins, adding 2,000 outputs and
//!   committing.
//! - `layer/*`: [`ShardedLayer`] `iter_dirty` over 100,000 entries, and growth from a presize for
//!   1,000 entries to 100,000 (the shard rehash).
//!
//! Each routine runs on state its setup builds, outside the measured region. The bench builds
//! without the crate's test features (`alloc-tag` scope guards, `planted-bugs` checks), as a
//! runtime build does.

// PROCESS.md: benches measure production only, so a test feature turned on here (by a self
// dev-dependency or `--all-features`) fails the build. Clippy's `--all-features` run is exempt.
#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

use std::hint::black_box;
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_consensus::BlockId;
use dash_consensus::primitives::{BlockHash, TxOut};
use dashd_chainstate::coins::layer::ShardedLayer;
use dashd_chainstate::coins::{
    BlockOverlay, BlockRef, CoinLayerOps as _, CoinSrc, CoinsBudget, CoinsCache, CoinsTable, Flags,
    OutPointKey, PackedCoin, Ratio, Salt,
};
use dashd_chainstate::store::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, RecordingSink};
use dashd_store::{Commit, MemoryEngine};

type Db = GuardedEngine<MemoryEngine>;

const MIB: u64 = 1 << 20;

/// Outpoint `i`: 64 outputs per txid.
fn key(i: u32) -> OutPointKey {
    let mut k = [0; 36];
    k[..4].copy_from_slice(&(i / 64).to_le_bytes());
    k[4..8].copy_from_slice(&0x5eed_c01d_u32.to_le_bytes());
    k[32..].copy_from_slice(&(i % 64).to_le_bytes());
    OutPointKey(k)
}

/// A P2PKH output paying `value`.
fn p2pkh(value: i64) -> TxOut {
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend_from_slice(&[0x42; 20]);
    script.extend_from_slice(&[0x88, 0xac]);
    TxOut {
        value,
        script_pubkey: script.into(),
    }
}

/// An output with a 200-byte script: a long coin, boxed on the heap.
fn long_output(value: i64) -> TxOut {
    let mut script = vec![0x4c, 198];
    script.extend_from_slice(&[0x17; 198]);
    TxOut {
        value,
        script_pubkey: script.into(),
    }
}

fn block(height: u32) -> BlockRef {
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    BlockRef {
        id: BlockId(height),
        hash: BlockHash::from(hash),
        height,
    }
}

fn db() -> Arc<Db> {
    Arc::new(GuardedEngine::new(
        MemoryEngine::new(&CHAINSTATE_SCHEMA),
        GlobalFailStop::new(),
        Arc::new(RecordingSink::default()),
    ))
}

/// A tip at `-dbcache=64` (f = 1/2, no allowance) over `db`.
fn tip(db: Arc<Db>) -> CoinsCache<MemoryEngine> {
    CoinsCache::new(db, CoinsBudget::new(64 * MIB, 0, Ratio::HALF, 0), Salt::fixed(7), None)
}

/// A tip holding `n` P2PKH coins in active, DIRTY and FRESH.
fn filled(n: u32) -> CoinsCache<MemoryEngine> {
    let mut t = tip(db());
    for i in 0..n {
        let out = p2pkh(i64::from(i) + 1);
        let src = CoinSrc::Output {
            out: &out,
            height: 1,
            coinbase: false,
        };
        t.add(&key(i), src, false).unwrap();
    }
    t.set_best_block(Some(block(1)));
    t
}

fn packed(c: &mut Criterion) {
    let (short, long) = (p2pkh(50_000), long_output(50_000));
    c.bench_function("packed/encode_p2pkh", |b| {
        b.iter(|| PackedCoin::from_output(black_box(&short), 1_000_000, false).unwrap());
    });
    c.bench_function("packed/encode_long", |b| {
        b.iter(|| PackedCoin::from_output(black_box(&long), 1_000_000, false).unwrap());
    });
    let stored = PackedCoin::from_output(&short, 1_000_000, false).unwrap().as_bytes().to_vec();
    c.bench_function("packed/decode_p2pkh", |b| {
        b.iter(|| {
            let coin = PackedCoin::from_db(black_box(&stored)).unwrap();
            black_box(coin.as_ref().value())
        });
    });
}

fn tip_ops(c: &mut Criterion) {
    const N: u32 = 10_000;
    c.bench_function("tip/add_10k", |b| {
        let outs: Vec<_> = (0..N).map(|i| p2pkh(i64::from(i) + 1)).collect();
        b.iter_batched(
            || tip(db()),
            |mut t| {
                for (i, out) in (0..N).zip(&outs) {
                    let src = CoinSrc::Output {
                        out,
                        height: 1,
                        coinbase: false,
                    };
                    t.add(&key(i), src, false).unwrap();
                }
                t
            },
            BatchSize::PerIteration,
        );
    });
    c.bench_function("tip/fetch_active_10k", |b| {
        b.iter_batched_ref(
            || filled(N),
            |t| {
                for i in 0..N {
                    black_box(t.fetch(&key(i)).unwrap().is_some());
                }
            },
            BatchSize::PerIteration,
        );
    });
    c.bench_function("tip/fetch_frozen_10k", |b| {
        b.iter_batched_ref(
            || {
                let mut t = filled(N);
                drop(t.freeze(0));
                t
            },
            |t| {
                for i in 0..N {
                    black_box(t.fetch(&key(i)).unwrap().is_some());
                }
            },
            BatchSize::PerIteration,
        );
    });
    c.bench_function("tip/fetch_db_1k", |b| {
        const M: u32 = 1_000;
        b.iter_batched_ref(
            || {
                let db = db();
                let (table, mut batch) = (CoinsTable::default(), db.batch());
                for i in 0..M {
                    let coin = PackedCoin::from_output(&p2pkh(i64::from(i) + 1), 1, false).unwrap();
                    table.put(&mut batch, &key(i), coin.as_bytes());
                }
                db.write(batch, Commit::Buffered).unwrap();
                tip(db)
            },
            |t| {
                for i in 0..M {
                    black_box(t.fetch(&key(i)).unwrap().is_some());
                }
            },
            BatchSize::PerIteration,
        );
    });
    c.bench_function("tip/spend_10k", |b| {
        b.iter_batched_ref(
            || filled(N),
            |t| {
                for i in 0..N {
                    black_box(t.spend(&key(i)).unwrap());
                }
            },
            BatchSize::PerIteration,
        );
    });
    c.bench_function("tip/freeze_release_100k", |b| {
        b.iter_batched(
            || filled(100_000),
            |mut t| {
                let frozen = t.freeze(0);
                let generation = frozen.generation();
                drop(frozen);
                t.release(generation);
                t
            },
            BatchSize::PerIteration,
        );
    });
}

fn overlay(c: &mut Criterion) {
    const INPUTS: u32 = 1_000;
    let outs: Vec<_> = (0..2 * INPUTS).map(|i| p2pkh(i64::from(i) + 1)).collect();
    c.bench_function("overlay/block_1k", |b| {
        b.iter_batched_ref(
            || filled(INPUTS),
            |t| {
                let mut o = BlockOverlay::new(t);
                for i in 0..INPUTS {
                    black_box(o.spend(&key(i)).unwrap());
                }
                for (i, out) in (0..).zip(&outs) {
                    let src = CoinSrc::Output {
                        out,
                        height: 2,
                        coinbase: false,
                    };
                    o.add(&key(INPUTS + i), src, false).unwrap();
                }
                o.commit(block(2)).unwrap();
            },
            BatchSize::PerIteration,
        );
    });
}

fn layer(c: &mut Criterion) {
    let coin = PackedCoin::from_output(&p2pkh(1), 1, false).unwrap();
    let fill = |l: &mut ShardedLayer, n: u32| {
        for i in 0..n {
            l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, coin.as_bytes());
        }
    };
    // Table bytes for about `n` entries: 73-byte buckets at most 7/8 full, rounded up.
    let entries = |n: u64| n * 128;
    c.bench_function("layer/iter_dirty_100k", |b| {
        let mut l = ShardedLayer::new(entries(100_000), Salt::fixed(7));
        fill(&mut l, 100_000);
        b.iter(|| l.iter_dirty().map(|(_, _, c)| c.len()).sum::<usize>());
    });
    c.bench_function("layer/grow_1k_to_100k", |b| {
        b.iter_batched(
            || ShardedLayer::new(entries(1_000), Salt::fixed(7)),
            |mut l| {
                fill(&mut l, 100_000);
                l
            },
            BatchSize::PerIteration,
        );
    });
}

criterion_group!(benches, packed, tip_ops, overlay, layer);
criterion_main!(benches);
