//! CH5-5 §6.1: the live-copy profile at engine level (L1–L6, the raw purge option, the
//! calibration of §6.4's workloads).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;

use super::*;

/// The chainstate's column families and their profiles (`dashd-chainstate` `CHAINSTATE_SCHEMA`,
/// which this crate cannot name).
static ELEVEN: DbSchema = DbSchema {
    name: "live-eleven",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("block_index", CfProfile::PointLookup),
        CfSpec::new("block_files", CfProfile::Ordered),
        CfSpec::new("coins", CfProfile::PointLookup),
        CfSpec::new("mn_diff", CfProfile::Compressed),
        CfSpec::new("mn_snapshot", CfProfile::Compressed),
        CfSpec::new("credit_pool", CfProfile::Compressed),
        CfSpec::new("mnhf", CfProfile::Ordered),
        CfSpec::new("llmq_mined", CfProfile::Compressed),
        CfSpec::new("llmq_mined_h", CfProfile::Ordered),
        CfSpec::new("llmq_snapshot", CfProfile::Compressed),
    ],
};

/// One column family.
static ONE: DbSchema = DbSchema {
    name: "live-one",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered)],
};

const W: usize = LiveCopy::REGTEST.write_buffer_bytes;
const B: u64 = LiveCopy::REGTEST.bound_bytes;

fn live_config(live: LiveCopy) -> RocksConfig {
    RocksConfig {
        live_copy: Some(live),
        concurrent_memtable_write: false,
        ..RocksConfig::default()
    }
}

fn open_live(dir: &Path, schema: &'static DbSchema, live: LiveCopy) -> RocksEngine {
    RocksEngine::open(dir, schema, &live_config(live)).unwrap().0
}

/// One file of [`dir_state`]: its name, inode, and length except for logs.
type FileState = (OsString, u64, Option<u64>);

/// Every file's name and inode, and its length except for the WALs (`*.log`) and `LOG`, which
/// grow by appends in a valid interval (CH5-5 C1).
fn dir_state(dir: &Path) -> Vec<FileState> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let meta = e.metadata().unwrap();
            let name = e.file_name();
            let text = name.to_string_lossy();
            let appended = text.ends_with(".log") || text == "LOG";
            (name.clone(), meta.ino(), (!appended).then_some(meta.len()))
        })
        .collect();
    files.sort();
    files
}

/// The WAL files (`NNNNNN.log`).
fn wals(dir: &Path) -> Vec<OsString> {
    dir_state(dir)
        .into_iter()
        .map(|(name, ..)| name)
        .filter(|n| is_database_data(&n.to_string_lossy()) && n.to_string_lossy().ends_with(".log"))
        .collect()
}

fn prop(engine: &RocksEngine, cf: Cf, name: &str) -> u64 {
    engine.prop_u64(Some(engine.handle(cf)), name).unwrap()
}

fn allocation(engine: &RocksEngine, cf: Cf) -> u64 {
    prop(engine, cf, ACTIVE_MEMTABLE)
}

/// `v` is invalid because its column family's super version moved from `sv0`, the one the last
/// barrier left.
fn switched_from(v: &CfVerdict, sv0: u64) -> bool {
    matches!(v.invalidation, Some(Invalidation::SuperVersionChanged { from, to }) if from == sv0 && to != sv0)
}

fn verdict_of<'a>(verdict: &'a LiveVerdict, cf: &str) -> &'a CfVerdict {
    verdict.cfs.iter().find(|v| v.cf == cf).unwrap()
}

/// One batch of `n` values of `size` bytes into `cf`, under keys from `next`.
fn put_values(engine: &RocksEngine, cf: Cf, next: &mut u64, n: usize, size: usize, durable: bool) {
    let mut b = engine.batch();
    for _ in 0..n {
        let value = vec![(*next % 251) as u8; size];
        b.put(cf, &next.to_be_bytes(), &value);
        *next += 1;
    }
    let commit = if durable {
        Commit::Durable
    } else {
        Commit::Buffered
    };
    engine.write(b, commit).unwrap();
}

/// A copy point with nothing to snapshot.
fn barrier(engine: &RocksEngine) -> LiveVerdict {
    engine.live_barrier(&mut || {}).unwrap()
}

/// L1: writes up to B, in one column family and spread over all of them, buffered and durable,
/// on the 11-CF and 1-CF schemas. After every write no memtable is immutable, and the directory
/// changed only by the WAL's and `LOG`'s appends (the WAL's inode included); the barrier's
/// verdict is valid and within B.
#[test]
fn l1_a_quiet_interval_changes_no_file() {
    for schema in [&ELEVEN, &ONE] {
        for spread in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let engine = open_live(dir.path(), schema, LiveCopy::REGTEST);
            barrier(&engine);
            let before = dir_state(dir.path());
            let wal = wals(dir.path());
            assert_eq!(wal.len(), 1, "{wal:?}");
            let mut next = 0;
            let mut i = 0;
            loop {
                let cf = if spread {
                    Cf::new((i % schema.cfs.len()) as u16)
                } else {
                    Cf::new((schema.cfs.len() - 1) as u16)
                };
                // Stop a margin below B: one more batch must not pass it.
                if allocation(&engine, cf) + (2 << 20) > B {
                    break;
                }
                put_values(&engine, cf, &mut next, 256, 256, i % 7 == 0);
                for c in 0..schema.cfs.len() {
                    let cf = Cf::new(c as u16);
                    assert_eq!(prop(&engine, cf, "rocksdb.num-immutable-mem-table"), 0);
                }
                assert_eq!(dir_state(dir.path()), before, "{} write {i}", schema.name);
                i += 1;
                if spread && i > 2000 {
                    break;
                }
            }
            assert_eq!(wals(dir.path()), wal);
            let verdict = barrier(&engine);
            assert!(verdict.holds(), "{verdict:?}");
            assert_eq!(verdict.cfs.len(), schema.cfs.len());
            assert!(verdict.cfs.iter().any(|v| v.hwm > 0 && v.bytes > 0), "{verdict:?}");
            assert_eq!(engine.probe.c4_bound_lines.load(Ordering::SeqCst), 0);
            assert_eq!(engine.probe.c4_switch_lines.load(Ordering::SeqCst), 0);
        }
    }
}

/// L2a (M11): writes past W in one column family until RocksDB requests its flush, one more
/// byte to switch the memtable, and the automatic flush let finish: that column family's
/// interval is invalid, C4's second line is logged once, and it passed B on the way.
#[test]
fn l2a_a_switched_memtable_invalidates_the_interval() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_live(dir.path(), &ELEVEN, LiveCopy::REGTEST);
    let sv0 = verdict_of(&barrier(&engine), "coins").super_version;
    let coins = ELEVEN.cf("coins").unwrap();
    let mut next = 0;
    // Until RocksDB has requested the flush: past W plus 0.6 of an arena block (1 MiB) it always
    // has (`memtable.cc:322-339`); the switch may come earlier, at a write after the request.
    let switched =
        |engine: &RocksEngine| prop(engine, coins, "rocksdb.num-immutable-mem-table") > 0;
    let requested = W as u64 + (1 << 20) * 6 / 10;
    while allocation(&engine, coins) <= requested && !switched(&engine) {
        put_values(&engine, coins, &mut next, 64, 1024, false);
    }
    put_values(&engine, coins, &mut next, 1, 1, false);
    engine.wait_idle_purged().unwrap();
    let verdict = barrier(&engine);
    let v = verdict_of(&verdict, "coins");
    assert!(!v.valid && switched_from(v, sv0), "{verdict:?}");
    assert!(v.over_b, "{verdict:?}");
    let others = verdict.cfs.iter().filter(|v| v.cf != "coins");
    assert!(others.clone().all(|v| v.valid && v.invalidation.is_none()), "{verdict:?}");
    assert!(!verdict.holds());
    assert_eq!(engine.probe.c4_switch_lines.load(Ordering::SeqCst), 1);
    // The next interval begins valid.
    put_values(&engine, coins, &mut next, 1, 1, false);
    let verdict = barrier(&engine);
    assert!(verdict.holds(), "{verdict:?}");
}

/// L2b (M10): below B, then one write that takes the high-water mark past it, then the barrier
/// with no further write: the verdict, computed before the reset, has `over_b` and `hwm > B`, the
/// batch bytes are reported, and C4's first line is logged exactly once.
#[test]
fn l2b_the_last_write_past_the_bound_is_in_the_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_live(dir.path(), &ELEVEN, LiveCopy::REGTEST);
    barrier(&engine);
    let coins = ELEVEN.cf("coins").unwrap();
    let mut next = 0;
    // A switch resets `allocation`, so with W ≤ B + 3 MiB the loop would never end; the super
    // version, which only a switch moves here, bounds it.
    let sv0 = prop(&engine, coins, SUPER_VERSION);
    let switched = |engine: &RocksEngine| prop(engine, coins, SUPER_VERSION) != sv0;
    while allocation(&engine, coins) + (3 << 20) < B && !switched(&engine) {
        put_values(&engine, coins, &mut next, 64, 1024, false);
    }
    assert!(!switched(&engine), "W ≤ B + 3 MiB: the memtable switched before the bound");
    assert_eq!(engine.probe.c4_bound_lines.load(Ordering::SeqCst), 0);
    put_values(&engine, coins, &mut next, 4096, 1024, false);
    let verdict = barrier(&engine);
    let v = verdict_of(&verdict, "coins");
    assert!(v.over_b && v.hwm > B, "{verdict:?}");
    assert!(v.valid, "no switch below W: {verdict:?}");
    assert!(v.bytes > B / 2, "{verdict:?}");
    // Only the column family written to is credited.
    assert_eq!(verdict_of(&verdict, "mn_diff").bytes, 0, "{verdict:?}");
    assert_eq!(engine.probe.c4_bound_lines.load(Ordering::SeqCst), 1);
    // The reset: the next interval starts at zero.
    let verdict = barrier(&engine);
    let v = verdict_of(&verdict, "coins");
    assert!(!v.over_b && v.hwm == 0 && v.bytes == 0 && v.valid, "{verdict:?}");
}

/// C4's two lines, as §3.1 words them.
#[test]
fn c4_lines() {
    assert_eq!(
        c4_bound_line("coins", 51, 48, 7),
        "live copy: column family coins memtable allocation 51 bytes (RocksDB's approximate arena \
         figure) exceeds the bound 48; 7 batch bytes were committed to it since the last copy \
         point; a copy of the running datadir is not promised to reopen"
    );
    assert_eq!(
        c4_switch_line("coins", 4, 6),
        "live copy: column family coins switched or flushed a memtable since the last copy point \
         (super version 4 → 6); a copy of the running datadir is not promised to reopen"
    );
}

/// L2c (D2-2), supplementary for M13: two writers released together, one a small put, the other
/// the batch that crosses the flush request, in either order. With one thread inserting each
/// write group the request-time allocation stays in the cache, so the interval's high-water
/// mark is never under W less one arena block, and the verdict always shows the bound passed.
#[test]
fn l2c_two_writers_across_the_flush_request() {
    const TEST_W: usize = 2 << 20;
    const TEST_ARENA: u64 = (TEST_W / 8) as u64;
    let live = LiveCopy {
        write_buffer_bytes: TEST_W,
        bound_bytes: (3 << 20) / 2,
    };
    let value = vec![9u8; 64 << 10];
    let small = vec![1u8; 4 << 10];
    let mut next = 0u64;
    let mut counts = [0u32; 4];
    // A fresh database every 50 iterations: each barrier leaves a level-0 file, never compacted,
    // and RocksDB keeps every table file open.
    for _ in 0..40 {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(open_live(dir.path(), &ONE, live));
        let meta = DbSchema::META;
        let mut sv0 = barrier(&engine).cfs[0].super_version;
        for i in 0..50 {
            while allocation(&engine, meta) < (5 << 20) / 4 {
                let mut b = engine.batch();
                b.put(meta, &next.to_be_bytes(), &value);
                next += 1;
                engine.write(b, Commit::Buffered).unwrap();
            }
            let start = Arc::new(std::sync::Barrier::new(2));
            let mut a = engine.batch();
            a.put(meta, b"a-small", &small);
            let mut b = engine.batch();
            for _ in 0..12 {
                b.put(meta, &next.to_be_bytes(), &value);
                next += 1;
            }
            let threads = [a, b].map(|batch| {
                let (engine, start) = (engine.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    engine.write(batch, Commit::Buffered).unwrap();
                })
            });
            threads.into_iter().for_each(|t| t.join().unwrap());
            let verdict = barrier(&engine);
            let v = &verdict.cfs[0];
            // OD-CH5-20: the bound passed, or the interval invalidated by this column family's
            // own super-version change. A later writer's switch can come between an earlier
            // writer's commit and its sample (`db_impl_write.cc:2145-2148`), leaving the
            // high-water mark short; the switch then moves the super version.
            let over = v.over_b && v.hwm >= TEST_W as u64 - TEST_ARENA;
            let switched = !v.valid && switched_from(v, sv0);
            counts[usize::from(over) + 2 * usize::from(switched)] += 1;
            assert!(over || switched, "iteration {i}: {verdict:?}");
            sv0 = v.super_version;
        }
    }
    eprintln!(
        "L2c: neither {} over {} switched {} both {}",
        counts[0], counts[1], counts[2], counts[3]
    );
}

/// L3 (M4, M5): 6 MiB into one column family, more than production's write buffer manager and
/// WAL cap at the default `-dbcache`, then a 1-byte write that would act on either, then the
/// purge wait: no file changed and level 0 is empty.
#[test]
fn l3_production_triggers_are_off() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_live(dir.path(), &ELEVEN, LiveCopy::REGTEST);
    barrier(&engine);
    let before = dir_state(dir.path());
    let l0 = |engine: &RocksEngine| {
        let cfs = ELEVEN.cfs.iter().map(|spec| ELEVEN.cf(spec.name).unwrap());
        cfs.map(|cf| prop(engine, cf, "rocksdb.num-files-at-level0")).collect::<Vec<_>>()
    };
    // The barrier itself flushed `meta`'s schema record.
    let l0_before = l0(&engine);
    let coins = ELEVEN.cf("coins").unwrap();
    let mut next = 0;
    for _ in 0..6 {
        put_values(&engine, coins, &mut next, 1024, 1024, false);
    }
    put_values(&engine, coins, &mut next, 1, 1, false);
    engine.wait_idle_purged().unwrap();
    assert_eq!(dir_state(dir.path()), before);
    assert_eq!(l0(&engine), l0_before);
    assert_eq!(prop(&engine, coins, "rocksdb.num-files-at-level0"), 0);
}

/// L4 (M12): right after a barrier the database has one WAL, no `*.trash` file, and no flush or
/// compaction pending or running; its directory then stays as it is (2 s once, then 200
/// repetitions of the immediate checks, supplementary).
#[test]
fn l4_the_barrier_leaves_nothing_outstanding() {
    let mut next = 0;
    let (mut dir, mut engine) = (tempfile::tempdir().unwrap(), None);
    for round in 0..200 {
        // A fresh database every 50 rounds: each barrier leaves a level-0 file per column family
        // written, never compacted, and RocksDB keeps every table file open.
        if round % 50 == 0 {
            drop(engine.take());
            dir = tempfile::tempdir().unwrap();
            engine = Some(open_live(dir.path(), &ELEVEN, LiveCopy::REGTEST));
        }
        let engine = engine.as_ref().unwrap();
        for c in 0..ELEVEN.cfs.len() {
            put_values(engine, Cf::new(c as u16), &mut next, 64, 512, round % 3 == 0);
        }
        barrier(engine);
        let state = dir_state(dir.path());
        assert_eq!(wals(dir.path()).len(), 1, "round {round}: {state:?}");
        let trash = state.iter().filter(|(n, ..)| n.to_string_lossy().ends_with(".trash"));
        assert_eq!(trash.count(), 0, "round {round}: {state:?}");
        for spec in ELEVEN.cfs {
            let cf = ELEVEN.cf(spec.name).unwrap();
            for name in [
                "rocksdb.num-running-flushes",
                "rocksdb.mem-table-flush-pending",
                "rocksdb.num-running-compactions",
            ] {
                assert_eq!(prop(engine, cf, name), 0, "round {round}: {} {name}", spec.name);
            }
        }
        let wait = if round == 0 {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(5)
        };
        std::thread::sleep(wait);
        assert_eq!(dir_state(dir.path()), state, "round {round}");
    }
}

/// The `key=value` lines of every `OPTIONS-*` file in `dir`, newest last, as one map.
fn options(dir: &Path) -> BTreeMap<String, Vec<String>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("OPTIONS-"))
        .collect();
    files.sort();
    let newest = files.last().expect("RocksDB saves its options at the open");
    let mut set = BTreeMap::<String, Vec<String>>::new();
    // RocksDB's `default` column family keeps its own options; it is always empty.
    let mut default_cf = false;
    for line in std::fs::read_to_string(newest).unwrap().lines().map(str::trim) {
        if line.starts_with('[') {
            default_cf = line.contains("\"default\"");
        } else if let Some((k, v)) = line.split_once('=')
            && !default_cf
        {
            set.entry(k.to_owned()).or_default().push(v.to_owned());
        }
    }
    set
}

/// Every value of `key` in the options (one per column family for a CF option).
fn all(options: &BTreeMap<String, Vec<String>>, key: &str) -> Vec<String> {
    options.get(key).unwrap_or_else(|| panic!("no {key} in the OPTIONS file")).clone()
}

/// L5 (M6, M9, M13): the live profile compacts nothing by itself, as `quiet`; its OPTIONS file
/// pins every trigger of §4's table it closes, and its write path. The default and quiet profiles
/// keep RocksDB's concurrent memtable writes.
#[test]
fn l5_the_live_profiles_pins() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_live(dir.path(), &ONE, LiveCopy::REGTEST);
    let meta = DbSchema::META;
    for i in 0..5u8 {
        let mut b = engine.batch();
        b.put(meta, b"a", &[i]);
        b.put(meta, b"z", &[i]);
        engine.write(b, Commit::Buffered).unwrap();
        engine.flush_memtables().unwrap();
    }
    engine.wait_idle_purged().unwrap();
    assert_eq!(prop(&engine, meta, "rocksdb.num-running-compactions"), 0);
    assert_eq!(prop(&engine, meta, "rocksdb.compaction-pending"), 1);
    assert!(prop(&engine, meta, "rocksdb.num-files-at-level0") >= 4);
    drop(engine);
    let o = options(dir.path());
    let w = W.to_string();
    for (key, want) in [
        ("disable_auto_compactions", "true"),
        ("recycle_log_file_num", "0"),
        ("max_log_file_size", "0"),
        ("log_file_time_to_roll", "0"),
        ("persist_stats_to_disk", "false"),
        ("stats_dump_period_sec", "0"),
        ("stats_persist_period_sec", "0"),
        ("enable_blob_files", "false"),
        ("memtable_max_range_deletions", "0"),
        ("memtable_op_scan_flush_trigger", "0"),
        ("memtable_avg_op_scan_flush_trigger", "0"),
        ("experimental_mempurge_threshold", "0.000000"),
        ("db_write_buffer_size", "0"),
        ("max_total_wal_size", "18446744073709551615"),
        ("write_buffer_size", &w),
        ("max_write_buffer_number", "2"),
        ("async_wal_precreate", "false"),
        ("allow_concurrent_memtable_write", "false"),
        ("enable_pipelined_write", "false"),
        ("unordered_write", "false"),
        ("two_write_queues", "false"),
    ] {
        for value in all(&o, key) {
            assert_eq!(value, want, "{key}");
        }
    }
    for config in [
        RocksConfig::default(),
        RocksConfig {
            quiet: true,
            ..RocksConfig::default()
        },
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (engine, _) = RocksEngine::open(dir.path(), &ONE, &config).unwrap();
        assert!(!engine.is_live());
        drop(engine);
        assert_eq!(all(&options(dir.path()), "allow_concurrent_memtable_write"), ["true"]);
    }
}

/// L5's source scan: nothing in this crate changes RocksDB's options at run time, makes a column
/// family after the open, or installs an `SstFileManager` (R15, R21).
#[test]
fn l5_no_runtime_option_or_file_manager_change() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    // Built from parts, so this file does not match itself.
    let calls = ["set_options(", "set_db_options(", "create_cf(", "set_sst_file_manager("]
        .map(|c| format!(".{c}"));
    let mut stack = vec![src];
    let mut scanned = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                scanned += 1;
                for call in &calls {
                    assert!(!text.contains(call.as_str()), "{} calls {call}", path.display());
                }
            }
        }
    }
    assert!(scanned > 5, "{scanned} files");
}

/// L6 (Q2, OD-CH5-15): a database with 16 overlapping level-0 files in two column families,
/// reopened live, is compacted inside the open: no level-0 file left, no background error, the
/// same rows; a barrier then leaves its directory stable.
#[test]
fn l6_the_open_compacts_a_deep_level_0() {
    let dir = tempfile::tempdir().unwrap();
    let cfs = [ELEVEN.cf("block_index").unwrap(), ELEVEN.cf("mn_diff").unwrap()];
    let rows = |engine: &RocksEngine| -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
        (0..ELEVEN.cfs.len())
            .map(|c| engine.iter_prefix(Cf::new(c as u16), &[]).map(Result::unwrap).collect())
            .collect()
    };
    let seeded = {
        let quiet = RocksConfig {
            quiet: true,
            ..RocksConfig::default()
        };
        let (engine, _) = RocksEngine::open(dir.path(), &ELEVEN, &quiet).unwrap();
        for i in 0..16u8 {
            let mut b = engine.batch();
            for cf in cfs {
                b.put(cf, b"a", &[i; 40]);
                b.put(cf, b"z", &[i; 40]);
            }
            engine.write(b, Commit::Buffered).unwrap();
            engine.flush_memtables().unwrap();
        }
        for cf in cfs {
            assert_eq!(prop(&engine, cf, "rocksdb.num-files-at-level0"), 16);
        }
        rows(&engine)
    };
    let engine = open_live(dir.path(), &ELEVEN, LiveCopy::REGTEST);
    for cf in cfs {
        assert_eq!(prop(&engine, cf, "rocksdb.num-files-at-level0"), 0);
    }
    assert_eq!(engine.prop_u64(None, "rocksdb.background-errors").unwrap(), 0);
    assert_eq!(rows(&engine), seeded);
    assert!(barrier(&engine).holds());
    let state = dir_state(dir.path());
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(dir_state(dir.path()), state);
}

/// A property read fails closed: an unknown name is an error, not a zero (OD-CH5-15).
#[test]
fn a_missing_property_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_live(dir.path(), &ONE, LiveCopy::REGTEST);
    let meta = engine.handle(DbSchema::META);
    let err = engine.prop_u64(Some(meta), "rocksdb.no-such-property").unwrap_err();
    assert!(err.to_string().contains("rocksdb.no-such-property"), "{err}");
    assert!(engine.prop_u64(None, "rocksdb.no-such-property").is_err());
}

/// C3 step 3's raw options: the purge wait on, no flush first, the database left open.
#[test]
fn the_purge_wait_options() {
    use librocksdb_sys as ffi;
    let opts = purge_wait_options();
    // SAFETY: the options are alive until the guard drops.
    unsafe {
        assert_eq!(ffi::rocksdb_wait_for_compact_options_get_wait_for_purge(opts.0), 1);
        assert_eq!(ffi::rocksdb_wait_for_compact_options_get_flush(opts.0), 0);
        assert_eq!(ffi::rocksdb_wait_for_compact_options_get_close_db(opts.0), 0);
    }
}

/// The live profile is refused without its write path, or with a write buffer manager.
#[test]
#[should_panic(expected = "inserts memtables on one thread")]
fn the_live_profile_needs_one_insert_thread() {
    let dir = tempfile::tempdir().unwrap();
    let config = RocksConfig {
        concurrent_memtable_write: true,
        ..live_config(LiveCopy::REGTEST)
    };
    let _ = RocksEngine::open(dir.path(), &ONE, &config);
}

#[test]
#[should_panic(expected = "no write buffer manager")]
fn the_live_profile_has_no_write_buffer_manager() {
    let dir = tempfile::tempdir().unwrap();
    let config = RocksConfig {
        charged_write_buffer_bytes: Some(1 << 20),
        ..live_config(LiveCopy::REGTEST)
    };
    let _ = RocksEngine::open(dir.path(), &ONE, &config);
}

/// A barrier is refused once a failure is latched, as any other barrier.
#[test]
fn a_latched_engine_refuses_the_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open_live(dir.path(), &ONE, LiveCopy::REGTEST);
    engine.fail_stop.latch(StoreError::engine(ENGINE, ErrorKind::Io, "an earlier failure"));
    let mut ran = false;
    let err = engine.live_barrier(&mut || ran = true).unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::ReadOnly), "{err}");
    assert!(!ran);
}

/// §6.4's calibration: the workloads' allocations fall where the process tests assert them,
/// measured on this engine's live profile. 570 000 `raw(51)` coins rows (a 36-byte outpoint key;
/// Core's 5-byte `Coin` of 1 000 duffs at a height below 8 192, `coins.h` `Coin::Serialize`) land
/// between S + 1 MiB (S = 16 MiB, the default sub-batch) and B; 50 000 headers-only `block_index`
/// records (a 32-byte key, values of 83–110 bytes, `dash-chain/src/record.rs:38-64`) between
/// 5 MiB and B.
#[test]
fn calibration_of_the_workloads() {
    let measure = |cf: &str, rows: u32, key_len: usize, value: &[u8]| {
        let dir = tempfile::tempdir().unwrap();
        let engine = open_live(dir.path(), &ELEVEN, LiveCopy::REGTEST);
        barrier(&engine);
        let cf = ELEVEN.cf(cf).unwrap();
        let start = allocation(&engine, cf);
        let mut key = vec![0x5a; key_len];
        for chunk in 0..(rows / 10_000) {
            let mut b = engine.batch();
            for i in 0..10_000u32 {
                key[..4].copy_from_slice(&(chunk * 10_000 + i).to_le_bytes());
                key[key_len - 4..].copy_from_slice(&i.wrapping_mul(2_654_435_761).to_be_bytes());
                b.put(cf, &key, value);
            }
            engine.write(b, Commit::Buffered).unwrap();
        }
        let verdict = barrier(&engine);
        assert!(verdict_of(&verdict, ELEVEN.cfs[cf.index()].name).valid);
        verdict_of(&verdict, ELEVEN.cfs[cf.index()].name).hwm - start
    };
    let coin = [0x82, 0x54, 0x04, 0x07, 0x51];
    let coins = measure("coins", 570_000, 36, &coin);
    assert!(((16 << 20) + (1 << 20)..=B).contains(&coins), "coins: {coins}");
    let small = measure("block_index", 50_000, 32, &[0x33; 83]);
    let large = measure("block_index", 50_000, 32, &[0x33; 110]);
    assert!(5 << 20 <= small && large <= B, "block_index: {small}..{large}");
    eprintln!("calibration: coins {coins} B, block_index {small}..{large} B");
}
