//! The block cache the stored index scan fills (2b-D17), on a mainnet-sized `block_index` under
//! the chainstate's own RocksDB tuning at the default `-dbcache`. Run by hand, each step its own
//! process so the scan starts on a cold cache:
//!
//! ```text
//! INDEX_SCAN_DIR=<dir> cargo test --release -p dashd-node --test index_scan_cache -- --ignored --exact build
//! INDEX_SCAN_DIR=<dir> cargo test --release -p dashd-node --test index_scan_cache -- --ignored --exact scan
//! ```
//!
//! `scan` reads as the index load does (`iter_prefix_uncached`), or through the cache with
//! `INDEX_SCAN_CACHED=1`, the control (2b-D19's two rows).
//!
//! Core reads `blocks/index` with `fill_cache = false` (`dbwrapper.cpp:136`), in its own LevelDB
//! with a 2 MiB cache; here the index shares E with the coins.

use std::path::PathBuf;

use dash_kv::{Commit, KvBatch, KvEngine, KvRead};
use dashd_chainstate::caches::{CacheSizes, DEFAULT_DB_CACHE, IndexArgs};
use dashd_chainstate::store::CHAINSTATE_SCHEMA;
use dashd_chainstate::tuning::{EngineTuning, RocksTuning};
use dashd_node::chain::index_records;

/// Mainnet's header count, about.
const RECORDS: u64 = 2_300_000;
/// A stored record: the varints and Core's 80-byte header.
const VALUE: usize = 92;

fn dir() -> PathBuf {
    PathBuf::from(std::env::var("INDEX_SCAN_DIR").expect("INDEX_SCAN_DIR names the store"))
}

fn sizes() -> CacheSizes {
    CacheSizes::calculate(DEFAULT_DB_CACHE, &IndexArgs::default())
}

/// Bytes a block hash or a header draws from: splitmix64, so keys land in hash order.
fn fill(mut seed: u64, out: &mut [u8]) {
    for chunk in out.chunks_mut(8) {
        seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
    }
}

/// `VmRSS` and `VmHWM`, in KiB.
fn rss() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kib = |name: &str| {
        let line = status.lines().find(|l| l.starts_with(name)).unwrap();
        line.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap()
    };
    (kib("VmRSS:"), kib("VmHWM:"))
}

#[test]
#[ignore = "by hand: writes a mainnet-sized index"]
fn build() {
    let (db, _) = RocksTuning::default().open_chainstate(&dir(), &sizes(), 16 << 20).unwrap();
    let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
    let (mut key, mut value) = ([0; 32], [0; VALUE]);
    for start in (0..RECORDS).step_by(50_000) {
        let mut batch = db.batch();
        for i in start..(start + 50_000).min(RECORDS) {
            fill(i, &mut key);
            fill(!i, &mut value);
            batch.put(cf, &key, &value);
        }
        db.write(batch, Commit::Durable).unwrap();
        db.flush_memtables().unwrap();
    }
    db.wait_idle().unwrap();
    eprintln!("BUILT {RECORDS} records in {}", dir().display());
}

#[test]
#[ignore = "by hand: scans the index `build` wrote"]
fn scan() {
    let (rss0, _) = rss();
    let (db, _) = RocksTuning::default().open_chainstate(&dir(), &sizes(), 16 << 20).unwrap();
    let before = db.memory_usage().unwrap();
    let (rss1, _) = rss();
    let started = std::time::Instant::now();
    let mut bytes = 0;
    let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
    let cached = std::env::var_os("INDEX_SCAN_CACHED").is_some();
    let rows: Box<dyn Iterator<Item = _>> = if cached {
        Box::new(db.iter_prefix(cf, &[]))
    } else {
        Box::new(db.iter_prefix_uncached(cf, &[]))
    };
    let n = index_records(rows).map(|r| bytes += r.unwrap().1.len()).count();
    let took = started.elapsed();
    let after = db.memory_usage().unwrap();
    let (rss2, hwm) = rss();
    assert_eq!(n as u64, RECORDS);
    eprintln!("SCAN cached={cached} records={n} value_bytes={bytes} took={took:?}");
    eprintln!("SCAN rss_kib start={rss0} opened={rss1} scanned={rss2} hwm={hwm}");
    eprintln!("SCAN cache before={before:?}");
    eprintln!("SCAN cache after={after:?}");
}
