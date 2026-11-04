//! [`EngineTuning`]: every engine-specific choice the chainstate makes, in one place that SB0's
//! storage ruling can swap (CH2 §2.5.1; DEC-85). The rest of the chainstate sees the engine only
//! through `KvEngine`, and reads an engine's numbers only through this trait.

use std::path::Path;

use dashd_store::{KvEngine, LiveVerdict, MemoryEngine, SchemaState, StoreError};

use crate::caches::CacheSizes;
use crate::store::{CHAINSTATE_SCHEMA, GuardedEngine};

/// EvoDB's LevelDB cache, fixed outside `-dbcache` (`evo/evodb.cpp:38`): part of E.
pub const EVODB_CACHE: u64 = 64 << 20;

/// `-dbbatchsize`'s default (`txdb.h:33`).
pub const DEFAULT_DB_BATCH_SIZE: u64 = 16 << 20;

/// The journal budget of each exact subscriber (§2.9 item 2).
pub const JOURNAL_BUDGET: u64 = 32 << 20;

/// [`RocksTuning`]'s R for sub-batches of `sub_batch` bytes: `RocksEngine::batch_with_capacity`
/// preallocates the C++ `WriteBatch` rep, and a sub-batch closes once it passes S with records
/// under 64 KiB, so the rep never reallocates (§2.5). Outside the `rocksdb` feature, for the
/// replays that size their budget as the node does (DEC-1005).
pub const fn rocks_batch_reserve(sub_batch: u64) -> u64 {
    sub_batch + (64 << 10)
}

/// The blind delete's span when no subscriber bounds it: one range tombstone costs the same at any
/// width (§2.5.1).
pub const UNBOUNDED_DELETE_SPAN: u32 = 1 << 24;

/// S, the sub-batch size: `min(-dbbatchsize, max(coins / 8, 64 KiB))` (§2.5). Static, from the
/// coins share rather than B(t), so the `coins` column family's write buffer can equal it.
pub fn sub_batch(db_batch_size: u64, sizes: &CacheSizes) -> u64 {
    db_batch_size.min((sizes.coins / 8).max(64 << 10))
}

/// E, the engine's memory target: Core's block-tree and chainstate database shares plus EvoDB's
/// fixed cache (§2.5). Engine-neutral; how an engine spends it is its tuning's.
pub fn engine_target(sizes: &CacheSizes) -> u64 {
    sizes.block_tree_db + sizes.coins_db + EVODB_CACHE
}

/// How [`RocksTuning`] spends E (§2.5; CH2c D27, DEC-383 A3; CH2c D31, BL-373): as Core's
/// `CDBWrapper`s spend their shares (a cache of n/2 and two memtables of n/4 per database), a
/// block cache of E/2, and memtables charged to it up to (block_tree_db + coins_db)/2, the shares
/// written without EvoDB traffic; the `coins` column family's write buffers are S each. EvoDB's
/// share sizes the cache now, before evo rows share the database (CH2c D22). A cache of only
/// (block_tree_db + coins_db)/2 (5 MiB at `-dbcache=300`) re-reads the coins replay's blocks
/// (+3–4% of its instructions). Core keeps such blocks in the page cache instead, through `mmap`,
/// and re-verifies each one it reads, which costs more than this cache's hits (D31).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineMemory {
    /// The block cache's capacity: E / 2.
    pub cache: u64,
    /// The write buffer manager's size, and `max_total_wal_size`: (block_tree_db + coins_db) / 2.
    pub wbm: u64,
    /// S: the `coins` column family's write buffer, so a sub-batch fits one memtable.
    pub coins_write_buffer: u64,
    /// The `coins` column family's write buffers: one active, two flushing.
    pub coins_write_buffers: i32,
    /// Every other column family's write buffer.
    pub other_write_buffer: u64,
}

impl EngineMemory {
    pub fn for_chainstate(sizes: &CacheSizes, sub_batch: u64) -> Self {
        Self {
            cache: engine_target(sizes) / 2,
            wbm: (sizes.block_tree_db + sizes.coins_db) / 2,
            coins_write_buffer: sub_batch,
            coins_write_buffers: 3,
            other_write_buffer: 4 << 20,
        }
    }
}

/// An engine's memory figures, for CH2c's `engine-memory` workload: named properties, in bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineMemoryReport(pub Vec<(&'static str, u64)>);

/// Every engine-specific number and hook (§2.5.1). The coins cache, its budget, the flush machine,
/// recovery and the subscriber driver are generic over `T: EngineTuning`.
pub trait EngineTuning: Send + Sync {
    type E: KvEngine;

    /// Opens (or creates) the chainstate database at `path` with the memory E derived from
    /// `sizes`, and `sub_batch` (S) for the `coins` column family's write buffers.
    ///
    /// # Errors
    ///
    /// The engine's, opening it or checking its schema.
    fn open_chainstate(
        &self,
        path: &Path,
        sizes: &CacheSizes,
        sub_batch: u64,
    ) -> Result<(Self::E, SchemaState), StoreError>;

    /// R: the memory a sub-batch of `sub_batch` bytes holds outside the tagging allocator. The
    /// coins budget takes this value; no other code knows an engine's batch cost.
    fn batch_reserve(&self, sub_batch: u64) -> u64;

    /// W's hook after each Bk, off V's path. It runs through the guard: a tripped latch refuses it,
    /// and its failure trips the latch (ST3a R8 across stores). It is generic over the engine
    /// under the guard so that a test's probe can wrap the tuning's engine.
    ///
    /// # Errors
    ///
    /// The engine's, or the refusal: the latch has tripped.
    fn after_bk<K: KvEngine>(&self, db: &GuardedEngine<K>) -> Result<(), StoreError>;

    /// Each exact subscriber's journal budget, in bytes (§2.9 item 2).
    fn journal_budget(&self) -> u64;

    /// The blind delete's span, in blocks, when no subscriber bounds it.
    fn unbounded_delete_span(&self) -> u32;

    /// The engine's memory figures, read past the guard: diagnostics only.
    fn memory_report(&self, db: &GuardedEngine<Self::E>) -> EngineMemoryReport;

    /// The tuning runs regtest's live-copy profile (CH5-5 §5).
    fn live_copy(&self) -> bool {
        false
    }

    /// A copy point's barrier at step 11, before its release (CH5-5 C3, §2.2): `snapshot` runs
    /// once the WAL is synced, while nothing else can write or count. The default is the memtable
    /// flush and the WAL sync, then `snapshot`, with no verdict. It runs through the guard, as
    /// [`after_bk`](Self::after_bk) does.
    ///
    /// # Errors
    ///
    /// The engine's, or the refusal: the latch has tripped. Nothing after a failed step runs.
    fn live_copy_barrier<K: KvEngine>(
        &self,
        db: &GuardedEngine<K>,
        snapshot: &mut dyn FnMut(),
    ) -> Result<Option<LiveVerdict>, StoreError> {
        plain_copy_barrier(db, snapshot)
    }
}

/// [`EngineTuning::live_copy_barrier`]'s default: the memtable flush and the WAL sync through
/// the guard, then `snapshot`.
fn plain_copy_barrier<K: KvEngine>(
    db: &GuardedEngine<K>,
    snapshot: &mut dyn FnMut(),
) -> Result<Option<LiveVerdict>, StoreError> {
    db.flush_memtables()?;
    db.sync_wal()?;
    snapshot();
    Ok(None)
}

/// The in-memory engine of tests. Its batches are Rust memory outside the `coins` tag, so R is 0,
/// and it has no memtables to flush after a Bk.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryTuning;

impl EngineTuning for MemoryTuning {
    type E = MemoryEngine;

    fn open_chainstate(
        &self,
        _path: &Path,
        _sizes: &CacheSizes,
        _sub_batch: u64,
    ) -> Result<(MemoryEngine, SchemaState), StoreError> {
        Ok((MemoryEngine::new(&CHAINSTATE_SCHEMA), SchemaState::Created))
    }

    fn batch_reserve(&self, _sub_batch: u64) -> u64 {
        0
    }

    fn after_bk<K: KvEngine>(&self, _db: &GuardedEngine<K>) -> Result<(), StoreError> {
        Ok(())
    }

    fn journal_budget(&self) -> u64 {
        JOURNAL_BUDGET
    }

    fn unbounded_delete_span(&self) -> u32 {
        UNBOUNDED_DELETE_SPAN
    }

    fn memory_report(&self, _db: &GuardedEngine<MemoryEngine>) -> EngineMemoryReport {
        EngineMemoryReport::default()
    }
}

/// The chainstate on RocksDB (§2.5.1): [`EngineMemory`]'s non-strict LRU cache with memtables
/// charged to it through a stalling write buffer manager, its per-column-family write buffers, no
/// L0 pinning, buffered `pread`, and `max_total_wal_size` the manager's size. R is S plus one
/// record's slack (ST1 D3), and W flushes the memtables after each Bk.
///
/// [`for_regtest`](Self::for_regtest) is CH5-5's live-copy profile instead: no write buffer
/// manager, no flush after a Bk, and the copy point's barrier of `RocksEngine::live_barrier`.
#[cfg(feature = "rocksdb")]
#[derive(Clone, Debug, Default)]
pub struct RocksTuning {
    /// What the chainstate does not set: ST1's background jobs, filters and sync pacing.
    pub base: dashd_store::RocksConfig,
}

#[cfg(feature = "rocksdb")]
impl RocksTuning {
    /// Regtest's tuning (CH5-5 §5): the live-copy profile at [`LiveCopy::REGTEST`], with one
    /// thread inserting each write group.
    ///
    /// [`LiveCopy::REGTEST`]: dashd_store::LiveCopy::REGTEST
    pub fn for_regtest() -> Self {
        Self {
            base: dashd_store::RocksConfig {
                live_copy: Some(dashd_store::LiveCopy::REGTEST),
                concurrent_memtable_write: false,
                ..dashd_store::RocksConfig::default()
            },
        }
    }

    /// The chainstate's configuration for `mem`. The live-copy profile charges no write buffer
    /// manager; the engine's open sets its memtables (`RocksConfig::live_copy`).
    pub fn config(&self, mem: &EngineMemory) -> dashd_store::RocksConfig {
        let bytes = |n: u64| usize::try_from(n).expect("a memory size fits usize");
        let charged = self.base.live_copy.is_none().then(|| bytes(mem.wbm));
        dashd_store::RocksConfig {
            block_cache_bytes: bytes(mem.cache),
            charged_write_buffer_bytes: charged,
            write_buffer_bytes: bytes(mem.other_write_buffer),
            cf_write_buffers: vec![(
                "coins",
                bytes(mem.coins_write_buffer),
                mem.coins_write_buffers,
            )],
            pin_l0_filter_and_index_blocks: false,
            mmap_reads: false,
            direct_io: false,
            max_total_wal_bytes: mem.wbm,
            ..self.base.clone()
        }
    }

    /// Waits until no memtable flush or compaction of `db` is scheduled or running
    /// ([`RocksEngine::wait_idle`](dashd_store::RocksEngine::wait_idle)): benchmarks end a
    /// region so, with no background job left.
    pub fn wait_idle(db: &GuardedEngine<dashd_store::RocksEngine>) -> Result<(), StoreError> {
        db.inner().wait_idle()
    }
}

#[cfg(feature = "rocksdb")]
impl EngineTuning for RocksTuning {
    type E = dashd_store::RocksEngine;

    fn open_chainstate(
        &self,
        path: &Path,
        sizes: &CacheSizes,
        sub_batch: u64,
    ) -> Result<(Self::E, SchemaState), StoreError> {
        let config = self.config(&EngineMemory::for_chainstate(sizes, sub_batch));
        dashd_store::RocksEngine::open(path, &CHAINSTATE_SCHEMA, &config)
    }

    /// [`rocks_batch_reserve`].
    fn batch_reserve(&self, sub_batch: u64) -> u64 {
        rocks_batch_reserve(sub_batch)
    }

    /// Returns the memtables' charge to the block cache between flushes (ST1 D2). The live-copy
    /// profile charges none, and flushes only at a copy point (CH5-5 R8).
    fn after_bk<K: KvEngine>(&self, db: &GuardedEngine<K>) -> Result<(), StoreError> {
        if self.live_copy() {
            return Ok(());
        }
        db.flush_memtables()
    }

    fn journal_budget(&self) -> u64 {
        JOURNAL_BUDGET
    }

    fn unbounded_delete_span(&self) -> u32 {
        UNBOUNDED_DELETE_SPAN
    }

    /// RocksDB's memory properties (§2.5); none if they cannot be read.
    fn memory_report(&self, db: &GuardedEngine<Self::E>) -> EngineMemoryReport {
        let Ok(m) = db.inner().memory_usage() else {
            return EngineMemoryReport::default();
        };
        let mut report = vec![
            ("block-cache-usage", m.block_cache_usage),
            ("block-cache-pinned-usage", m.block_cache_pinned_usage),
            ("cur-size-all-mem-tables", m.cur_size_all_mem_tables),
            ("size-all-mem-tables", m.size_all_mem_tables),
            ("estimate-table-readers-mem", m.estimate_table_readers_mem),
        ];
        report.extend(m.write_buffer_manager_usage.map(|n| ("write-buffer-manager-usage", n)));
        EngineMemoryReport(report)
    }

    fn live_copy(&self) -> bool {
        self.base.live_copy.is_some()
    }

    /// The live-copy profile's barrier, `RocksEngine::live_barrier` through the guard (CH5-5
    /// §3.2); otherwise the default's.
    fn live_copy_barrier<K: KvEngine>(
        &self,
        db: &GuardedEngine<K>,
        snapshot: &mut dyn FnMut(),
    ) -> Result<Option<LiveVerdict>, StoreError> {
        if self.live_copy() {
            db.live_barrier(snapshot)
        } else {
            plain_copy_barrier(db, snapshot)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caches::{DEFAULT_DB_CACHE, IndexArgs};

    /// At `-dbcache=300` with txindex on: S is Core's 16 MiB and E is 2 + 8 + 64 = 74 MiB (§2.5);
    /// at `-dbcache=4` S is coins / 8.
    #[test]
    fn the_default_sub_batch_and_engine_target() {
        let sizes = CacheSizes::calculate(DEFAULT_DB_CACHE, &IndexArgs::default());
        assert_eq!(sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes), 16 << 20);
        assert_eq!(engine_target(&sizes), 74 << 20);
        let small = CacheSizes::calculate(4, &IndexArgs::default());
        assert_eq!(sub_batch(DEFAULT_DB_BATCH_SIZE, &small), small.coins / 8);
        // `feature_dbcrash.py:57`: -dbcache=4 -dbbatchsize=200000.
        assert_eq!(sub_batch(200_000, &small), 200_000);
    }

    /// The default split (CH2c D27, D31): at `-dbcache=300` with txindex on, a cache of
    /// (2 + 8 + 64) / 2 = 37 MiB, a write buffer manager of (2 + 8) / 2 = 5 MiB, the `coins` write
    /// buffers of S = 16 MiB, and every other column family's of 4 MiB; at `-dbcache=4`, a cache of
    /// half of E and a manager of half of its two shares.
    #[test]
    fn the_default_engine_memory_is_cores_split() {
        let sizes = CacheSizes::calculate(DEFAULT_DB_CACHE, &IndexArgs::default());
        let s = sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes);
        assert_eq!(
            EngineMemory::for_chainstate(&sizes, s),
            EngineMemory {
                cache: 37 << 20,
                wbm: 5 << 20,
                coins_write_buffer: 16 << 20,
                coins_write_buffers: 3,
                other_write_buffer: 4 << 20,
            }
        );
        let small = CacheSizes::calculate(4, &IndexArgs::default());
        let half = (small.block_tree_db + small.coins_db) / 2;
        let m = EngineMemory::for_chainstate(&small, 1 << 20);
        let cache = engine_target(&small) / 2;
        assert_eq!((m.cache, m.wbm, m.coins_write_buffer), (cache, half, 1 << 20));
    }

    /// A tuning of its own (G-R3-m2): R and the journal budget differ from both shipped tunings,
    /// and the coins budget takes its R, not a constant. The journal budget's reader, the
    /// subscriber driver, and `FlushMachine`'s use of R and `after_bk` are CH2b's.
    struct Custom;

    impl EngineTuning for Custom {
        type E = MemoryEngine;

        fn open_chainstate(
            &self,
            path: &Path,
            sizes: &CacheSizes,
            sub_batch: u64,
        ) -> Result<(MemoryEngine, SchemaState), StoreError> {
            MemoryTuning.open_chainstate(path, sizes, sub_batch)
        }

        fn batch_reserve(&self, sub_batch: u64) -> u64 {
            3 * sub_batch + 12_345
        }

        fn after_bk<K: KvEngine>(&self, _db: &GuardedEngine<K>) -> Result<(), StoreError> {
            Ok(())
        }

        fn journal_budget(&self) -> u64 {
            7 << 20
        }

        fn unbounded_delete_span(&self) -> u32 {
            1 << 10
        }

        fn memory_report(&self, _db: &GuardedEngine<MemoryEngine>) -> EngineMemoryReport {
            EngineMemoryReport(vec![("custom", 1)])
        }
    }

    /// BL-72's driver legs: a subscriber driver takes its journal budget and its blind delete's
    /// span from the tuning it was given. With no locator, open deletes heights 0 to the span
    /// and keeps the row just above it.
    #[test]
    fn the_driver_takes_the_tunings_journal_budget_and_delete_span() {
        use std::sync::Arc;

        use dashd_store::{Commit, KvBatch};

        use crate::flush::SubscriberHub;
        use crate::store::{GlobalFailStop, ProbeEngine, RecordingSink};
        use crate::subscriber::{
            DriverConfig, NoBlocks, ScriptedFeeds, SubscriberDriver, TOY_DB, TOY_HEIGHT, ToyIndex,
            TreeIndex, height_key, toy_rows_above,
        };

        let custom = DriverConfig::from_tuning(&Custom, None, 1);
        assert_eq!((custom.journal_budget, custom.unbounded_delete_span), (7 << 20, 1 << 10));
        let memory = DriverConfig::from_tuning(&MemoryTuning, None, 1);
        assert_eq!(
            (memory.journal_budget, memory.unbounded_delete_span),
            (JOURNAL_BUDGET, UNBOUNDED_DELETE_SPAN)
        );
        let db = Arc::new(GuardedEngine::new(
            ProbeEngine::new(&TOY_DB),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        ));
        let mut b = db.batch();
        for h in [0, 1 << 10, (1 << 10) + 1] {
            b.put(TOY_HEIGHT, &height_key(h, &[]), b"stale");
        }
        db.write(b, Commit::Durable).unwrap();
        let tree = TreeIndex::new();
        let feeds = ScriptedFeeds::new(4, vec![tree.genesis()]);
        let hub = Arc::new(SubscriberHub::with_feeds(Arc::new(feeds), Arc::new(tree)));
        let toy = ToyIndex::mandatory();
        let _d =
            SubscriberDriver::open(toy, Arc::clone(&db), hub, Arc::new(NoBlocks), custom).unwrap();
        assert_eq!(toy_rows_above(&*db, 0), vec![(1 << 10) + 1]);
        assert_eq!(
            crate::store::GuardedRead::read(&*db, TOY_HEIGHT, &height_key(0, &[])).unwrap(),
            None
        );
    }

    #[test]
    fn the_budget_takes_the_tunings_reserve() {
        use crate::coins::{CoinsBudget, Ratio};
        let sizes = CacheSizes::calculate(DEFAULT_DB_CACHE, &IndexArgs::default());
        let s = sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes);
        let custom = CoinsBudget::for_engine(&sizes, 300_000_000, Ratio::HALF, s, &Custom);
        assert_eq!(custom.coins_share(), sizes.coins);
        assert_eq!(custom.reserve(true), 3 * s + 12_345);
        assert_eq!(custom.reserve(false), 0);
        let memory = CoinsBudget::for_engine(&sizes, 300_000_000, Ratio::HALF, s, &MemoryTuning);
        assert_eq!(memory.reserve(true), 0);
    }
}

/// The chainstate at `-dbcache=300` with txindex on, in a new directory, through
/// [`RocksTuning`]: the engine, its memory (CH2c D27, D31) and S (16 MiB).
#[cfg(all(test, feature = "rocksdb"))]
pub(crate) fn open_default_rocks(dir: &Path) -> (dashd_store::RocksEngine, EngineMemory, u64) {
    use crate::caches::{DEFAULT_DB_CACHE, IndexArgs};
    let sizes = CacheSizes::calculate(DEFAULT_DB_CACHE, &IndexArgs::default());
    let s = sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes);
    let (e, state) = RocksTuning::default().open_chainstate(dir, &sizes, s).unwrap();
    assert_eq!(state, SchemaState::Created);
    (e, EngineMemory::for_chainstate(&sizes, s), s)
}

#[cfg(all(test, feature = "rocksdb"))]
mod rocks_tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use dash_consensus::primitives::TxOut;
    use dashd_store::{Commit, KvBatch, RocksEngine};

    use super::*;
    use crate::coins::{CoinsTable, OutPointKey, PackedCoin};
    use crate::store::{GlobalFailStop, RecordingSink};

    /// [`open_default_rocks`]'s engine behind its own latch.
    fn open(dir: &Path) -> (GuardedEngine<RocksEngine>, EngineMemory, u64) {
        let (e, mem, s) = open_default_rocks(dir);
        let sink = Arc::new(RecordingSink::default());
        (GuardedEngine::new(e, GlobalFailStop::new(), sink), mem, s)
    }

    /// The `key=value` lines of each section of the newest `OPTIONS-*` file in `dir`.
    fn options(dir: &Path) -> HashMap<String, HashMap<String, String>> {
        let newest = std::fs::read_dir(dir)
            .unwrap()
            .map(|f| f.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("OPTIONS-"))
            .max()
            .expect("RocksDB saves its options at the open");
        let mut sections = HashMap::<String, HashMap<String, String>>::new();
        let mut section = String::new();
        for line in std::fs::read_to_string(newest).unwrap().lines().map(str::trim) {
            if line.starts_with('[') {
                section = line.to_owned();
            } else if let Some((k, v)) = line.split_once('=') {
                sections.entry(section.clone()).or_default().insert(k.into(), v.into());
            }
        }
        sections
    }

    /// §2.5's configuration, read back from the `OPTIONS` file RocksDB saved at the open, and from
    /// the engine for the figures that file leaves out: a cache of 37 MiB and a write buffer manager
    /// of 5 MiB (CH2c D27, D31) whose memtables are charged to it. Neither shows the cache's non-strict capacity or the
    /// manager's `allow_stall`, literals in `dashd-store`'s open, whose tests check them by
    /// behaviour (`the_block_cache_keeps_a_charge_beyond_its_capacity`,
    /// `writers_stall_at_the_write_buffer_managers_size`), as they check the stalled write's wait
    /// and its failure on a background error (`a_stalled_write_waits_until_memory_is_freed`,
    /// `a_stalled_write_fails_when_its_flush_fails`; DEC-164). The file does show DEC-164's
    /// triggers: no column family slows writes down before it stops them.
    #[test]
    fn the_chainstate_config_is_in_its_options_file() {
        let dir = tempfile::tempdir().unwrap();
        let (e, mem, s) = open(dir.path());
        let m = e.inner().memory_usage().unwrap();
        assert_eq!((mem.cache, mem.wbm), (37 << 20, 5 << 20));
        assert_eq!(m.block_cache_capacity, mem.cache);
        assert_eq!(m.write_buffer_manager_size, Some(mem.wbm));
        // A buffered write's memtable is charged to the cache, which has read nothing yet.
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let mut b = e.batch();
        for i in 0u32..20_000 {
            b.put(coins, &i.to_be_bytes(), &[7; 64]);
        }
        e.write(b, Commit::Buffered).unwrap();
        let m = e.inner().memory_usage().unwrap();
        let charged = m.write_buffer_manager_usage.unwrap();
        assert!(charged > 1 << 20 && m.block_cache_usage >= charged, "{m:?}");
        drop(e);
        let options = options(dir.path());
        let get = |section: &str, key: &str| {
            options.get(section).and_then(|s| s.get(key)).cloned().unwrap_or_else(|| {
                panic!("{section} has no {key}");
            })
        };
        for (key, want) in [
            ("max_total_wal_size", mem.wbm.to_string()),
            ("manual_wal_flush", "false".into()),
            ("allow_mmap_reads", "false".into()),
            ("use_direct_reads", "false".into()),
            ("use_direct_io_for_flush_and_compaction", "false".into()),
            // The write buffer manager replaces it.
            ("db_write_buffer_size", "0".into()),
        ] {
            assert_eq!(get("[DBOptions]", key), want, "{key}");
        }
        for spec in CHAINSTATE_SCHEMA.cfs {
            let cf = format!("[CFOptions \"{}\"]", spec.name);
            let table = format!("[TableOptions/BlockBasedTable \"{}\"]", spec.name);
            let (buffer, buffers) = if spec.name == "coins" {
                (s, 3)
            } else {
                (4 << 20, 2)
            };
            for (section, key, want) in [
                (&cf, "write_buffer_size", buffer.to_string()),
                (&cf, "max_write_buffer_number", buffers.to_string()),
                (&cf, "min_write_buffer_number_to_merge", "1".into()),
                (&cf, "level0_slowdown_writes_trigger", "36".into()),
                (&cf, "level0_stop_writes_trigger", "36".into()),
                (
                    &cf,
                    "soft_pending_compaction_bytes_limit",
                    get(&cf, "hard_pending_compaction_bytes_limit"),
                ),
                (&table, "pin_l0_filter_and_index_blocks_in_cache", "false".into()),
                (&table, "cache_index_and_filter_blocks", "true".into()),
                (&table, "pin_top_level_index_and_filter", "true".into()),
            ] {
                assert_eq!(get(section, key), want, "{section} {key}");
            }
        }
    }

    /// The batch's peak that R states (§2.5): a sub-batch preallocated at R, filled with the
    /// largest coins until it passes S as W closes it, never outgrows R, so its rep never
    /// reallocates. At the default S and at the least one.
    #[test]
    fn a_sub_batch_stays_within_r() {
        let dir = tempfile::tempdir().unwrap();
        let (e, _, default_s) = open(dir.path());
        let largest = PackedCoin::from_output(
            &TxOut {
                value: 1,
                script_pubkey: vec![0x51; dash_consensus::script::interpreter::MAX_SCRIPT_SIZE]
                    .into(),
            },
            u32::MAX >> 1,
            true,
        )
        .unwrap();
        let table = CoinsTable::default();
        for s in [default_s, 64 << 10] {
            let r = RocksTuning::default().batch_reserve(s);
            let mut b = e.batch_with_capacity(usize::try_from(r).unwrap());
            let mut vout = u32::MAX;
            while b.size_bytes() as u64 <= s {
                let mut k = [0xab; 36];
                k[32..].copy_from_slice(&vout.to_le_bytes());
                table.put(&mut b, &OutPointKey(k), largest.as_bytes());
                vout -= 1;
            }
            assert!(b.size_bytes() as u64 <= r, "S {s}: {} > R {r}", b.size_bytes());
        }
    }

    /// `after_bk` moves the memtables into table files: their charge goes back to the cache. It
    /// runs through the guard, so a tripped latch refuses it.
    #[test]
    fn after_bk_flushes_the_memtables() {
        let dir = tempfile::tempdir().unwrap();
        let (e, ..) = open(dir.path());
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let mut b = e.batch();
        for i in 0u32..20_000 {
            b.put(coins, &i.to_be_bytes(), &[7; 64]);
        }
        e.write(b, Commit::Buffered).unwrap();
        let memtables = |e: &GuardedEngine<RocksEngine>| {
            let report = RocksTuning::default().memory_report(e).0;
            report.iter().find(|(name, _)| *name == "cur-size-all-mem-tables").unwrap().1
        };
        let before = memtables(&e);
        assert!(before > 1 << 20, "{before}");
        RocksTuning::default().after_bk(&e).unwrap();
        let after = memtables(&e);
        assert!(after < before / 8, "{before} -> {after}");
        e.latch().trip(crate::store::Failure {
            kind: dashd_store::ErrorKind::Io,
            site: "block files write",
            message: "a failed fsync".into(),
        });
        let refused = RocksTuning::default().after_bk(&e).unwrap_err();
        assert!(refused.to_string().contains("block files write"), "{refused}");
    }
}
/// CH5-5 phase 2: the tunings' configurations (T7), and regtest's live-copy barrier.
#[cfg(all(test, feature = "rocksdb"))]
mod live_copy_tests {
    use std::sync::Arc;

    use dashd_store::{Commit, KvBatch, LiveCopy, RocksConfig, RocksEngine};

    use super::*;
    use crate::caches::IndexArgs;
    use crate::store::{GlobalFailStop, ProbeEngine, RecordingSink};

    const OFF: IndexArgs = IndexArgs {
        txindex: false,
        addressindex: false,
        timestampindex: false,
        spentindex: false,
        filters: &[],
    };
    const ON: IndexArgs = IndexArgs {
        txindex: true,
        addressindex: true,
        timestampindex: true,
        spentindex: true,
        filters: &["basic"],
    };

    fn config(tuning: &RocksTuning, dbcache: i64, indexes: &IndexArgs) -> RocksConfig {
        let sizes = CacheSizes::calculate(dbcache, indexes);
        let s = sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes);
        tuning.config(&EngineMemory::for_chainstate(&sizes, s))
    }

    /// Every field of `c` against the literal values of a chainstate configuration: the cache,
    /// the write buffer manager (also `max_total_wal_size`) and the `coins` write buffer vary.
    fn assert_config(c: &RocksConfig, cache: usize, wbm: Option<usize>, wal: u64, coins: usize) {
        let RocksConfig {
            block_cache_bytes,
            write_buffer_bytes,
            max_write_buffers,
            total_write_buffer_bytes,
            max_background_jobs,
            bloom_bits_per_key,
            bytes_per_sync,
            mmap_reads,
            direct_io,
            partitioned_filters,
            pin_l0_filter_and_index_blocks,
            charged_write_buffer_bytes,
            cf_write_buffers,
            max_total_wal_bytes,
            wal_dir,
            quiet,
            live_copy: _,
            concurrent_memtable_write: _,
        } = c;
        assert_eq!(*block_cache_bytes, cache);
        assert_eq!(*write_buffer_bytes, 4 << 20);
        assert_eq!(*max_write_buffers, 2);
        assert_eq!(*total_write_buffer_bytes, 32 << 20);
        // ST1's: a quarter of the host's threads, at least 2.
        assert_eq!(*max_background_jobs, RocksConfig::default().max_background_jobs);
        assert_eq!(*bloom_bits_per_key, 10.0);
        assert_eq!(*bytes_per_sync, 1 << 20);
        assert!(
            !mmap_reads && !direct_io && *partitioned_filters && !pin_l0_filter_and_index_blocks
        );
        assert_eq!(*charged_write_buffer_bytes, wbm);
        assert_eq!(*cf_write_buffers, vec![("coins", coins, 3)]);
        assert_eq!(*max_total_wal_bytes, wal);
        assert_eq!(*wal_dir, None);
        assert!(!quiet);
    }

    /// T7: the default tuning's configuration at `-dbcache` 4, 300 and 1000 with the indexes off
    /// and on is today's, and it keeps RocksDB's concurrent memtable writes and no live copy.
    #[test]
    fn t7_the_default_configurations() {
        let goldens = [
            (4, &OFF, 34_734_080, 1_179_648, 229_376),
            (4, &ON, 34_345_996, 791_564, 132_355),
            (300, &OFF, 38_797_312, 5_242_880, 16 << 20),
            (300, &ON, 38_797_312, 5_242_880, 16 << 20),
            (1000, &OFF, 38_797_312, 5_242_880, 16 << 20),
            (1000, &ON, 38_797_312, 5_242_880, 16 << 20),
        ];
        for (dbcache, indexes, cache, wbm, coins) in goldens {
            let c = config(&RocksTuning::default(), dbcache, indexes);
            assert_config(&c, cache, Some(wbm), wbm as u64, coins);
            assert_eq!(c.live_copy, None);
            assert!(c.concurrent_memtable_write);
        }
        assert!(!RocksTuning::default().live_copy());
    }

    /// T7: regtest's tuning is the default's with the live-copy profile at W = 64 MiB and
    /// B = 48 MiB, one insert thread, and no write buffer manager.
    #[test]
    fn t7_the_regtest_configuration() {
        let c = config(&RocksTuning::for_regtest(), 300, &IndexArgs::default());
        assert_config(&c, 38_797_312, None, 5_242_880, 16 << 20);
        assert_eq!(
            c.live_copy,
            Some(LiveCopy {
                write_buffer_bytes: 64 << 20,
                bound_bytes: 48 << 20,
            })
        );
        assert!(!c.concurrent_memtable_write);
        assert!(RocksTuning::for_regtest().live_copy());
    }

    fn probe_db() -> GuardedEngine<ProbeEngine> {
        GuardedEngine::new(
            ProbeEngine::new(&CHAINSTATE_SCHEMA),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        )
    }

    /// CH5-5 C3 (M2, M2b): regtest's barrier is the memtable flush, the purge wait and the WAL
    /// sync, then the snapshot; the default tuning's is today's flush and sync, then the snapshot.
    #[test]
    fn the_barriers_call_order() {
        for (tuning, steps) in [
            (RocksTuning::for_regtest(), &["flush_memtables", "wait_idle_purged", "sync_wal"][..]),
            (RocksTuning::default(), &["flush_memtables", "sync_wal"][..]),
        ] {
            let db = probe_db();
            let mut at_snapshot = None;
            let verdict =
                tuning.live_copy_barrier(&db, &mut || at_snapshot = Some(db.inner().barriers()));
            assert_eq!(verdict.unwrap(), None, "a memory engine measures no interval");
            assert_eq!(db.inner().barriers(), steps);
            assert_eq!(at_snapshot.as_deref(), Some(steps), "the snapshot runs last");
        }
    }

    /// CH5-5 §6.7's failure propagation: a failed purge wait fails the barrier, the WAL sync and
    /// the snapshot do not run, the latch trips, and a tripped latch refuses the next barrier.
    #[test]
    fn a_failed_purge_wait_fails_the_barrier_and_trips_the_latch() {
        let db = probe_db();
        db.inner().fail_next_barrier("wait_idle_purged");
        let mut ran = false;
        let err =
            RocksTuning::for_regtest().live_copy_barrier(&db, &mut || ran = true).unwrap_err();
        assert_eq!(err.kind(), Some(dashd_store::ErrorKind::Io), "{err}");
        assert!(err.to_string().contains("barrier failed"), "{err}");
        assert!(!ran);
        assert_eq!(db.inner().barriers(), ["flush_memtables", "wait_idle_purged"]);
        assert!(db.latch().tripped().is_some());
        let refused = RocksTuning::for_regtest().live_copy_barrier(&db, &mut || ran = true);
        assert_eq!(refused.unwrap_err().kind(), Some(dashd_store::ErrorKind::ReadOnly));
        assert_eq!(db.inner().barriers().len(), 2, "refused before the engine");
    }

    /// On RocksDB, regtest's barrier is the engine's live barrier, whose verdict covers the
    /// chainstate's 11 column families; regtest's post-Bk hook flushes nothing (R8).
    #[test]
    fn regtest_on_rocksdb() {
        let dir = tempfile::tempdir().unwrap();
        let sizes = CacheSizes::calculate(crate::caches::DEFAULT_DB_CACHE, &IndexArgs::default());
        let s = sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes);
        let tuning = RocksTuning::for_regtest();
        let (e, _) = tuning.open_chainstate(dir.path(), &sizes, s).unwrap();
        assert!(e.is_live());
        let db = GuardedEngine::new(e, GlobalFailStop::new(), Arc::new(RecordingSink::default()));
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let mut b = db.batch();
        for i in 0u32..20_000 {
            b.put(coins, &i.to_be_bytes(), &[7; 64]);
        }
        db.write(b, Commit::Buffered).unwrap();
        let memtables = |db: &GuardedEngine<RocksEngine>| db.inner().memory_usage().unwrap();
        let before = memtables(&db).cur_size_all_mem_tables;
        tuning.after_bk(&db).unwrap();
        assert_eq!(memtables(&db).cur_size_all_mem_tables, before, "no flush after a Bk");
        let mut snapshots = 0;
        let verdict = tuning.live_copy_barrier(&db, &mut || snapshots += 1).unwrap().unwrap();
        assert_eq!(snapshots, 1);
        assert_eq!(verdict.cfs.len(), CHAINSTATE_SCHEMA.cfs.len());
        assert!(verdict.holds(), "{verdict:?}");
        assert!(memtables(&db).cur_size_all_mem_tables < before / 8, "the barrier flushed");
    }

    /// §3.2's synthetic snapshot-writer workload, part of the measurement: an hour of a peer's
    /// rotation-info requests in one interval, each `StoreSnapshotForBlock` of the up to five
    /// cycle bases a `getqrinfo` reaches (h−c … h−4c, plus the extra share), one request a
    /// second: 18 000 `Buffered` writes of mainnet-sized snapshots (400 members of LLMQ_60_75,
    /// a 16-entry skip list) through [`RocksEvoStore::store_quorum_snapshot`] (R17). The
    /// interval stays valid and `llmq_snapshot`'s allocation within B / 8.
    // Core: llmq/snapshot.cpp:322 StoreSnapshotForBlock; llmq/snapshot.cpp BuildQuorumRotationInfo
    #[test]
    fn the_synthetic_snapshot_writer_stays_within_the_margin() {
        use dash_evo::llmq::QuorumSnapshot;
        use dash_params::LlmqType;
        use dash_primitives::BlockHash;

        use crate::evo::RocksEvoStore;
        use crate::evo::node::{node_codec, node_net_info};

        let dir = tempfile::tempdir().unwrap();
        let sizes = CacheSizes::calculate(crate::caches::DEFAULT_DB_CACHE, &OFF);
        let s = sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes);
        let tuning = RocksTuning::for_regtest();
        let (e, _) = tuning.open_chainstate(dir.path(), &sizes, s).unwrap();
        let db = Arc::new(GuardedEngine::new(
            e,
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        ));
        let params = dash_params::ChainParamsBuilder::new(
            dash_params::Network::Regtest,
            dash_params::Release::V24_0,
        )
        .build()
        .expect("regtest");
        let codec = node_codec(node_net_info(&params, false).expect("regtest's NetInfo rules"));
        let store = RocksEvoStore::new(Arc::clone(&db), codec, BlockHash::from([0; 32]));
        tuning.live_copy_barrier(&db, &mut || ()).unwrap();
        let snapshot = QuorumSnapshot {
            active_quorum_members: (0..400).map(|i| i % 3 != 0).collect(),
            skip_list_mode: 1,
            skip_list: (0..16).collect(),
        };
        for i in 0u32..18_000 {
            let mut base = [0x5a; 32];
            base[..4].copy_from_slice(&i.to_le_bytes());
            store
                .store_quorum_snapshot(LlmqType::Llmq60_75, &BlockHash::from(base), &snapshot)
                .unwrap();
        }
        let verdict = tuning.live_copy_barrier(&db, &mut || ()).unwrap().unwrap();
        let cf = verdict.cfs.iter().find(|v| v.cf == "llmq_snapshot").expect("llmq_snapshot");
        eprintln!("CH5-5 synthetic snapshot writer: llmq_snapshot {cf:?}");
        assert!(verdict.holds() && cf.valid, "{verdict:?}");
        assert!(8 * cf.hwm <= LiveCopy::REGTEST.bound_bytes, "8 × {} > B", cf.hwm);
    }
}
