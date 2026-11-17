//! The coins model's drivers on seeded inputs (CH2 §6.5, §6.7): `coins_cache_model` for every
//! f ∈ {1/4, 1/2, 3/4} with the tagging allocator checked after every op, `coins_overlay_diff`,
//! and the planted bugs the model must catch (X10, X13, X29, X30, X31, X34, X35).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashd_store::{KvEngine, MemoryEngine, SchemaState, StoreError};

use super::Rng;
use crate::caches::CacheSizes;
#[cfg(feature = "planted-bugs")]
use crate::coins::Planted;
#[cfg(feature = "planted-bugs")]
use crate::coins::model::Mismatch;
use crate::coins::model::{self, Config, ModelTuning, Stats};
use crate::store::GuardedEngine;
use crate::tuning::{EngineMemoryReport, EngineTuning, MemoryTuning};

/// Random op bytes: the drivers read any input.
fn input(r: &mut Rng, len: usize) -> Vec<u8> {
    (0..len).map(|_| r.next() as u8).collect()
}

/// Sums the runs' counts, for the coverage checks.
fn add(total: &mut Stats, s: &Stats) {
    total.ops += s.ops;
    total.blocks += s.blocks;
    total.dropped_blocks += s.dropped_blocks;
    total.disconnects += s.disconnects;
    total.freezes += s.freezes;
    total.releases += s.releases;
    total.admitted += s.admitted;
    total.stale_batches += s.stale_batches;
    total.boundaries += s.boundaries;
    total.always_flushes += s.always_flushes;
    total.logic_errors += s.logic_errors;
    total.read_failure |= s.read_failure;
    total.allocator_checked |= s.allocator_checked;
}

/// Runs `n` seeded inputs of `len` bytes under each `config`, every run passing (and the allocator
/// counted, with `alloc-tag`).
fn runs(configs: &[u8], n: u64, len: usize, diff: bool) -> Stats {
    let mut total = Stats::default();
    for &b in configs {
        for seed in 0..n {
            let ops = input(&mut Rng::new(seed << 8 | u64::from(b)), len);
            let s = model::run(Config::from_byte(b), &ops, diff)
                .unwrap_or_else(|m| panic!("config {b:#x}, seed {seed}: {m:?}"));
            let tagged = cfg!(feature = "alloc-tag");
            assert_eq!(s.allocator_checked, tagged, "the tagging allocator counted the run");
            add(&mut total, &s);
        }
    }
    total
}

/// `coins_cache_model` v0 for f = 1/4, 1/2 and 3/4 (with and without a mempool allowance, at 2, 4
/// and 8 MiB): no mismatch, the allocator equal to native Σ − R after every op (with `alloc-tag`),
/// and the op mix reached.
#[test]
fn coins_cache_model_for_every_f() {
    // f from bits 0–1, the share from bits 2–7, the allowance from bit 6, xcheck off at bit 7.
    let configs = [0x00, 0x01, 0x02, 0x44, 0x45, 0x46, 0x08, 0x49, 0x0a, 0x81, 0xc6];
    let total = runs(&configs, 24, 3_000, false);
    println!("coins_cache_model: {total:?}");
    assert!(total.blocks > 10_000 && total.dropped_blocks > 5_000 && total.disconnects > 5_000);
    assert!(total.freezes > 2_000 && total.releases > 2_000 && total.boundaries > 10_000);
    assert!(total.admitted > 2_000 && total.stale_batches > 2_000 && total.logic_errors > 50);
    assert!(total.always_flushes > 1_000);
}

/// One run over `tuning`'s engine in a new directory, which must pass.
#[cfg(feature = "rocksdb")]
fn run_over<T: EngineTuning>(tuning: T, b: u8, seed: u64, ops: &[u8], diff: bool) -> Stats {
    let dir = tempfile::tempdir().unwrap();
    model::run_on(tuning, dir.path(), Config::from_byte(b), ops, diff)
        .unwrap_or_else(|m| panic!("config {b:#x}, seed {seed}, diff {diff}: {m:?}"))
}

/// `coins_cache_model` and `coins_overlay_diff` over each engine's tuning (CH2 §7.1 a3):
/// `MemoryTuning` (R = 0) and `RocksTuning` (R = S + 64 KiB, the memtables flushed after each Bk)
/// pass, and under one reserve ([`ModelTuning`]) the two engines run every input alike.
#[cfg(feature = "rocksdb")]
#[test]
fn the_model_runs_over_memory_and_rocks_tuning() {
    use crate::tuning::RocksTuning;

    let mut total = Stats::default();
    for diff in [false, true] {
        let (n, len) = if diff {
            (12, 300)
        } else {
            (4, 3_000)
        };
        for b in [0x00, 0x45, 0x0a, 0x81] {
            for seed in 0..n {
                let ops = input(&mut Rng::new(seed << 8 | u64::from(b)), len);
                run_over(MemoryTuning, b, seed, &ops, diff);
                add(&mut total, &run_over(RocksTuning::default(), b, seed, &ops, diff));
                assert_eq!(
                    run_over(ModelTuning(RocksTuning::default()), b, seed, &ops, diff),
                    model::run(Config::from_byte(b), &ops, diff).unwrap(),
                    "config {b:#x}, seed {seed}, diff {diff}"
                );
            }
        }
    }
    println!("over RocksTuning: {total:?}");
    assert_eq!(total.allocator_checked, cfg!(feature = "alloc-tag"));
    assert!(total.releases > 50 && total.admitted > 50);
}

/// The fuzz targets' tuning, counting its `after_bk` calls.
struct CountingAfterBk(Arc<AtomicU64>);

impl EngineTuning for CountingAfterBk {
    type E = MemoryEngine;

    fn open_chainstate(
        &self,
        path: &Path,
        sizes: &CacheSizes,
        sub_batch: u64,
    ) -> Result<(MemoryEngine, SchemaState), StoreError> {
        ModelTuning(MemoryTuning).open_chainstate(path, sizes, sub_batch)
    }

    fn batch_reserve(&self, sub_batch: u64) -> u64 {
        ModelTuning(MemoryTuning).batch_reserve(sub_batch)
    }

    fn after_bk<K: KvEngine>(&self, db: &GuardedEngine<K>) -> Result<(), StoreError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        ModelTuning(MemoryTuning).after_bk(db)
    }

    fn journal_budget(&self) -> u64 {
        ModelTuning(MemoryTuning).journal_budget()
    }

    fn unbounded_delete_span(&self) -> u32 {
        ModelTuning(MemoryTuning).unbounded_delete_span()
    }

    fn memory_report(&self, db: &GuardedEngine<MemoryEngine>) -> EngineMemoryReport {
        ModelTuning(MemoryTuning).memory_report(db)
    }
}

/// Step 11: the model calls its tuning's `after_bk` once at each release of a written F, and the
/// runs are the fuzz targets' own.
#[test]
fn the_model_calls_after_bk_at_each_release() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut releases = 0;
    for seed in 0..8 {
        let ops = input(&mut Rng::new(seed), 3_000);
        let config = Config::from_byte(0x01);
        let s = model::run_on(CountingAfterBk(calls.clone()), Path::new(""), config, &ops, false);
        let s = s.unwrap_or_else(|m| panic!("seed {seed}: {m:?}"));
        assert_eq!(s, model::run(config, &ops, false).unwrap());
        releases += s.releases;
    }
    assert!(releases > 10, "{releases}");
    assert_eq!(calls.load(Ordering::Relaxed), releases);
}

/// `coins_overlay_diff`: random parents and one overlay each.
#[test]
fn coins_overlay_diff_smoke() {
    let total = runs(&[0x00, 0x45, 0x0a], 400, 300, true);
    println!("coins_overlay_diff: {total:?}");
    assert!(total.blocks > 100 && total.freezes > 100);
}

/// An injected read failure ends the run through fail-stop.
#[test]
fn an_injected_read_failure_is_fail_stop() {
    // Fetch a coin into the cache, flush it, inject, then fetch a coin only the database holds.
    let ops = [
        0x01, 0, 0x80, // tip add, overwrite: a coin at key 0
        10, 11, 11, 12, // freeze, both W steps, release
        19, 0xfa, // inject a read failure
        3, 0, // fetch key 0: from the database
    ];
    let s = model::run(Config::from_byte(0), &ops, false).unwrap();
    assert!(s.read_failure && s.releases == 1, "{s:?}");
}

/// A spend of a key F holds spent writes no tombstone into active, which would outlive the release
/// (the review of a2's first draft).
#[test]
fn a_spent_f_entry_is_not_spent_again() {
    let ops = [
        0x01, 0, 0x80, // tip add, overwrite: DIRTY, not FRESH, at key 0
        2, 0,  // spend it: a tombstone in active
        10, // freeze: the tombstone is in F
        2, 0, // spend again: nothing to spend
        11, 11, 12, // both W steps, release
        2, 0, // and again
    ];
    let s = model::run(Config::from_byte(0), &ops, false).unwrap();
    assert_eq!((s.ops, s.freezes, s.releases), (8, 1, 1), "{s:?}");
}

/// A block's FRESH add over a key F holds spent keeps FRESH at the commit, as §3.2's table has it
/// (F's tombstone is no parent entry): spending it again removes it, with F in flight and after
/// the release (the review of f1586ceb).
#[test]
fn a_fresh_add_over_an_f_tombstone_stays_fresh() {
    let block = [
        0x01, 0, 0x80, // tip add, overwrite: DIRTY, not FRESH, at key 0
        2, 0,  // spend it: a tombstone in active
        10, // freeze: the tombstone is in F
        6,  // a block
        0, 0, 0, // add key 0, no overwrite: FRESH in the child
        8, // commit
    ];
    let twice = [2, 0, 2, 0]; // spend, then spend again: nothing left
    for release in [&[][..], &[11, 11, 12]] {
        let ops = [&block[..], release, &twice].concat();
        let s = model::run(Config::from_byte(1), &ops, false).unwrap();
        assert_eq!((s.blocks, s.freezes, s.releases), (1, 1, release.len() as u64 / 3), "{s:?}");
    }
}

/// One block, committed at once: the tip's best block is that block, as the twin's is (Sol's S1).
/// With X29 planted (`merge` drops the child's best block, Sol's mutation) the model catches it.
#[test]
fn a_committed_block_is_the_best_block() {
    let ops = [6, 8]; // a block, committed
    let s = model::run(Config::from_byte(0), &ops, false).unwrap();
    assert_eq!((s.ops, s.blocks), (2, 1), "{s:?}");
    #[cfg(feature = "planted-bugs")]
    {
        let mut config = Config::from_byte(0);
        config.planted = Some(Planted::X29);
        let caught = model::run(config, &ops, false);
        assert_eq!(caught, Err(Mismatch("the best block after a commit")));
    }
}

/// The checks that compare what the cache and its twins read: a lookup mismatch.
#[cfg(all(feature = "planted-bugs", feature = "alloc-tag"))]
const LOOKUPS: [&str; 10] = [
    "fetch",
    "spend",
    "the view",
    "have_inputs",
    "overlay coin",
    "overlay have_coin",
    "overlay spend",
    "disconnect have_coin",
    "disconnect spend",
    "add: membership",
];

/// Why each of 100 seeded runs of `config` with `bug` planted was caught, with the count.
#[cfg(all(feature = "planted-bugs", feature = "alloc-tag"))]
fn catches(bug: Planted, mut config: Config) -> std::collections::BTreeMap<&'static str, u32> {
    config.planted = Some(bug);
    let mut reasons = std::collections::BTreeMap::new();
    for seed in 0..100 {
        let ops = input(&mut Rng::new(seed), 3_000);
        if let Err(Mismatch(what)) = model::run(config, &ops, false) {
            *reasons.entry(what).or_default() += 1;
        }
    }
    reasons
}

/// X10 (a stale prefetch admitted), X13 (F released before W finishes), X29 (`merge` drops the
/// child's best block) and CH4-1's X34 (the generation read at admission) and X35 (a release keeps
/// the prefetch map) are caught, each for its documented reason (O2): X10 and X35 by xcheck, and
/// with xcheck off in the view, where the resurrected coin shows; X13 by the meter, and with the
/// meter off by a lookup that misses F's changes; X29 at the best block; X34 by the epoch check (a
/// planted build keeps debug assertions, `lib.rs`).
#[test]
#[cfg(all(feature = "planted-bugs", feature = "alloc-tag"))]
fn the_model_catches_x10_x13_x29_x34_and_x35_for_their_reasons() {
    let on = Config::from_byte(0x01);
    let no_xcheck = Config::from_byte(0x81);
    let no_meter = Config {
        meter: false,
        ..on
    };
    let cases: [(Planted, Config, &str, &[&str]); 7] = [
        (Planted::X10, on, "an unexpected read failure", &[]),
        (Planted::X10, no_xcheck, "the view", &LOOKUPS),
        (Planted::X13, on, "tagged != sigma - R", &[]),
        (Planted::X13, no_meter, "the view", &LOOKUPS),
        (Planted::X29, on, "the best block after a commit", &[]),
        (Planted::X35, on, "an unexpected read failure", &[]),
        (Planted::X35, no_xcheck, "the view", &LOOKUPS),
    ];
    super::assert_the_epoch_check_refuses(|| catches(Planted::X34, on));
    for (bug, config, reason, also) in cases {
        let reasons = catches(bug, config);
        let field =
            format!("{bug:?} (xcheck {}, meter {}): {reasons:?}", config.xcheck, config.meter);
        assert!(reasons.contains_key(reason), "{field}");
        assert!(reasons.keys().all(|r| *r == reason || also.contains(r)), "{field}");
    }
}

/// X30 (`have_inputs` does not cache an input in active) and X31 (`fetch` caches a hit as DIRTY),
/// Opus high's MB and MI: the twin is consulted on every fetch and `have_inputs`, so both are
/// caught as the tip's entry, on the model's runs and on the overlay diff's. The diff's single
/// overlay meets X30 rarely (33 of 2,000 seeds), so the search runs to 4,000 seeds and stops at the
/// first catch.
#[test]
#[cfg(feature = "planted-bugs")]
fn the_model_catches_x30_and_x31() {
    for (bug, field) in [(Planted::X30, "x30"), (Planted::X31, "x31")] {
        for diff in [false, true] {
            let caught = (0..4_000).find_map(|seed| {
                let mut config = Config::from_byte(0x01);
                config.planted = Some(bug);
                let ops = input(
                    &mut Rng::new(seed),
                    if diff {
                        300
                    } else {
                        3_000
                    },
                );
                model::run(config, &ops, diff).err()
            });
            assert_eq!(caught, Some(Mismatch("the tip's entry")), "{field}, diff {diff}");
        }
    }
}
