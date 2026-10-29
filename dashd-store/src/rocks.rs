//! The RocksDB engine (A1 §2.8).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rocksdb::{
    AsColumnFamilyRef, AsRawPtr, BlockBasedIndexType, BlockBasedOptions, Cache,
    ColumnFamilyDescriptor, DB, DBCompressionType, DBRecoveryMode, DataBlockIndexType, Direction,
    IteratorMode, LogLevel, Options, ReadOptions, WaitForCompactOptions, WriteBufferManager,
};

use dash_kv::fail_stop::FailStop;
use dash_kv::{Cf, Commit, KvBatch, KvEngine, KvRead};
use dash_kv::{CfProfile, CfSpec, DbSchema, SCHEMA_KEY, SchemaState, check_record, check_schema};
use dash_kv::{ErrorKind, Result, StoreError};

/// Memory and threading knobs of a [`RocksEngine`]. The defaults are A1 §2.8 / §3.4's values for
/// the default `-dbcache` of 300 MiB.
#[derive(Debug, Clone)]
pub struct RocksConfig {
    /// The LRU block cache shared by all column families (data, index and filter blocks).
    pub block_cache_bytes: usize,
    /// Size of one memtable.
    pub write_buffer_bytes: usize,
    /// Memtables per column family (one active, the rest being flushed). At most 3: with more,
    /// RocksDB slows writes down before it stops them, which DEC-164 rules out (the open panics).
    pub max_write_buffers: i32,
    /// Cap on memtable memory across all column families (`db_write_buffer_size`).
    pub total_write_buffer_bytes: usize,
    pub max_background_jobs: i32,
    /// Bits per key of the whole-key bloom filters on point-lookup column families.
    pub bloom_bits_per_key: f64,
    /// Incremental `sync_file_range` while writing table and log files, so a barrier does not
    /// have to flush a large dirty backlog at once.
    pub bytes_per_sync: u64,
    /// Read table files through `mmap` (as LevelDB does on 64-bit hosts) instead of `pread`.
    pub mmap_reads: bool,
    /// `O_DIRECT` for reads, flushes and compactions: data is cached once, in the block cache,
    /// instead of in both the block cache and the OS page cache.
    pub direct_io: bool,
    /// Partitioned index and filter blocks on point-lookup column families (only the top-level
    /// index is pinned; partitions are cached like data blocks, at high priority). Keeps a small
    /// block cache from churning whole per-file filter and index blocks: in the ST1 bake-off this
    /// made coin miss reads about 3x cheaper (`designs/ST1-bakeoff.md` §9).
    pub partitioned_filters: bool,
    /// Pin the filter and index blocks of level-0 files in the block cache. The chainstate turns
    /// it off (CH2 §2.5): the pinned set would grow with the number of L0 files.
    pub pin_l0_filter_and_index_blocks: bool,
    /// Charge memtables to the block cache through a `WriteBufferManager` of this many bytes,
    /// with `allow_stall`: memtables flush near 7/8 of it, and writers stall above it until a
    /// flush frees memory (CH2 §2.5). It replaces `total_write_buffer_bytes`. `None`: memtables
    /// are capped by `total_write_buffer_bytes` and not charged to the cache.
    pub charged_write_buffer_bytes: Option<usize>,
    /// Memtable sizes for named column families, overriding `write_buffer_bytes` and
    /// `max_write_buffers` for them: (column family, bytes, buffers), with at most 3 buffers.
    pub cf_write_buffers: Vec<(&'static str, usize, i32)>,
    /// `max_total_wal_size`: a memtable flush trigger once the live logs reach it, not a bound on
    /// replay. 0 is RocksDB's default (four times the memtable total).
    pub max_total_wal_bytes: u64,
    /// The directory of the write-ahead logs (`wal_dir`), if not the database's own.
    pub wal_dir: Option<std::path::PathBuf>,
    /// For benchmarks that count work rather than time it (PF0), never for a node: no automatic
    /// compactions, whose timing varies, so level 0 grows without bound and never stops writes,
    /// reads slow down, and [`RocksEngine::wait_for_compactions`] never returns once level 0
    /// needs a compaction; no statistics dump or persist timer; and only warnings and errors in
    /// the `LOG` (background errors are errors).
    pub quiet: bool,
}

impl Default for RocksConfig {
    fn default() -> Self {
        Self {
            block_cache_bytes: 24 << 20,
            write_buffer_bytes: 16 << 20,
            max_write_buffers: 2,
            total_write_buffer_bytes: 32 << 20,
            max_background_jobs: std::thread::available_parallelism()
                .map_or(2, |n| (n.get() / 4).max(2) as i32),
            bloom_bits_per_key: 10.0,
            bytes_per_sync: 1 << 20,
            mmap_reads: false,
            direct_io: false,
            partitioned_filters: true,
            pin_l0_filter_and_index_blocks: true,
            charged_write_buffer_bytes: None,
            cf_write_buffers: Vec::new(),
            max_total_wal_bytes: 0,
            wal_dir: None,
            quiet: false,
        }
    }
}

const ENGINE: &str = "rocksdb";

/// The status of a `no_slowdown` write that would have waited: on the write buffer manager
/// (`db_impl_write.cc:2173-2178` at 11.8.1) or on a write-controller stop (`:2865-2870`).
const WRITE_STALL: &str = "Result incomplete: Write stall";
/// A write refused as a stall pauses this long before it tries again (DEC-164), doubling up to
/// [`STALL_PAUSE_MAX`].
const STALL_PAUSE_MIN: Duration = Duration::from_millis(1);
const STALL_PAUSE_MAX: Duration = Duration::from_millis(64);
/// RocksDB's default level-0 stop trigger (`advanced_options.h:554` at 11.8.1), also our
/// slowdown trigger (DEC-164).
const L0_STOP_WRITES: i32 = 36;

/// The kind of a RocksDB error from its message, or `None` if the message does not tell. A
/// quota (`EDQUOT`) counts as disk full: the operator's remedy is the same.
fn kind_from_message(message: &str) -> Option<ErrorKind> {
    let lower = message.to_ascii_lowercase();
    if ["no space left", "nospace", "disk quota exceeded"].iter().any(|m| lower.contains(m)) {
        Some(ErrorKind::NoSpace)
    } else if lower.contains("read only") || lower.contains("read-only") {
        Some(ErrorKind::ReadOnly)
    } else if lower.contains("corruption") {
        Some(ErrorKind::Corruption)
    } else {
        None
    }
}

fn engine_error(e: rocksdb::Error) -> StoreError {
    status_error(e.into_string())
}

/// The error of a RocksDB status message (`Status::ToString`: "IO error: …").
fn status_error(message: String) -> StoreError {
    let kind = kind_from_message(&message).unwrap_or(match message.split(':').next() {
        Some("IO error") => ErrorKind::Io,
        Some("Corruption") => ErrorKind::Corruption,
        _ => ErrorKind::Other,
    });
    StoreError::engine(ENGINE, kind, message)
}

/// The last background flush or compaction error RocksDB logged in `log` (the database's `LOG`),
/// read from its last MiB.
fn last_background_error(log: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(log).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(1 << 20))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    // "[JOB 2] Waiting after background flush error: IO error: No space left on device: ..."
    tail.lines().rev().find_map(|line| {
        let (_, cause) = line.split_once("after background ")?;
        let cause = cause.split_once("error: ")?.1;
        Some(cause.split(", Accumulated background error counts").next()?.to_owned())
    })
}

/// A [`KvEngine`] backed by one RocksDB database, one RocksDB column family per schema column
/// family.
pub struct RocksEngine {
    db: Arc<DB>,
    /// The database directory (for its `LOG`).
    path: std::path::PathBuf,
    schema: &'static DbSchema,
    handles: Arc<CfHandles>,
    /// The first write, barrier or background failure; it stops all later writes.
    fail_stop: FailStop,
    /// Kept alive for the lifetime of the database.
    _cache: Cache,
    /// The memtables' charge to the block cache, if they are charged.
    wbm: Option<WriteBufferManager>,
}

impl RocksEngine {
    /// Opens (or creates) the database at `path` and checks its schema record.
    ///
    /// An existing database (a directory with a `CURRENT` file) is first inspected read-only: it
    /// must hold only this schema's column families (plus RocksDB's empty `default`) and this
    /// schema's record at a version no newer than ours, else nothing is written and the open
    /// fails ([`StoreError::SchemaTooNew`], [`StoreError::UnknownDatabase`]). A database that
    /// RocksDB cannot read fails with RocksDB's error (kind `Corruption` or `Io`).
    ///
    /// Without `CURRENT`, the directory becomes a new database if it is missing, empty, or holds
    /// only what a first creation that died early leaves behind (`LOG`, `LOCK`, `IDENTITY`,
    /// `OPTIONS-*`, `MANIFEST-*`) and a filesystem's `lost+found`. Numbered table or log files
    /// without `CURRENT` are a damaged database (kind `Corruption`); any other file means the
    /// directory is not a database (`UnknownDatabase`).
    pub fn open(
        path: &Path,
        schema: &'static DbSchema,
        config: &RocksConfig,
    ) -> Result<(Self, SchemaState)> {
        schema.validate();
        let has_current = path.join("CURRENT").try_exists().map_err(|e| io_error(path, e))?;
        if has_current {
            let existing = DB::list_cf(&Options::default(), path).map_err(engine_error)?;
            inspect_existing(path, schema, &existing)?;
        } else {
            classify_uncreated(path, schema)?;
        }
        let cache = Cache::new_lru_cache(config.block_cache_bytes);
        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        // RocksDB 11.8.1 writes `IDENTITY` durably before a new database's `CURRENT`, which is
        // how the node tells its own half-made database from Core's LevelDB (BL-147 (b),
        // `dashd_node::datadir`). It is the default, set here because RocksDB means to change it;
        // off, RocksDB would also delete the `IDENTITY` of a database it reopens.
        // SAFETY: the options are `db_opts`, alive for the call.
        unsafe { librocksdb_sys::rocksdb_options_set_write_identity_file(db_opts.as_raw_ptr(), 1) };
        db_opts.set_max_background_jobs(config.max_background_jobs);
        db_opts.set_bytes_per_sync(config.bytes_per_sync);
        db_opts.set_wal_bytes_per_sync(config.bytes_per_sync);
        let wbm = config.charged_write_buffer_bytes.map(|bytes| {
            WriteBufferManager::new_write_buffer_manager_with_cache(bytes, true, cache.clone())
        });
        match &wbm {
            Some(wbm) => db_opts.set_write_buffer_manager(wbm),
            None => db_opts.set_db_write_buffer_size(config.total_write_buffer_bytes),
        }
        db_opts.set_max_total_wal_size(config.max_total_wal_bytes);
        if let Some(dir) = &config.wal_dir {
            db_opts.set_wal_dir(dir);
        }
        // A `Durable` commit syncs every batch before it, which holds only while RocksDB writes
        // each batch to the log itself (ST3a item 12).
        db_opts.set_manual_wal_flush(false);
        db_opts.set_keep_log_file_num(4);
        // A1 §3.5.1: WAL recovery yields a prefix of the commit order, and a memtable flush never
        // leaves one column family ahead of the recovered WAL prefix.
        db_opts.set_atomic_flush(true);
        db_opts.set_wal_recovery_mode(DBRecoveryMode::PointInTime);
        db_opts.set_paranoid_checks(true);
        db_opts.set_allow_mmap_reads(config.mmap_reads);
        db_opts.set_use_direct_reads(config.direct_io);
        db_opts.set_use_direct_io_for_flush_and_compaction(config.direct_io);
        if config.quiet {
            db_opts.set_stats_dump_period_sec(0);
            db_opts.set_stats_persist_period_sec(0);
            db_opts.set_log_level(LogLevel::Warn);
        }
        let cfs = schema
            .cfs
            .iter()
            .map(|spec| ColumnFamilyDescriptor::new(spec.name, cf_options(spec, config, &cache)));
        let db = Arc::new(DB::open_cf_descriptors(&db_opts, path, cfs).map_err(engine_error)?);
        let ptrs = schema
            .cfs
            .iter()
            .map(|spec| CfPtr(db.cf_handle(spec.name).expect("opened with every CF").inner()))
            .collect();
        let handles = Arc::new(CfHandles {
            ptrs,
            _db: db.clone(),
        });
        let engine = Self {
            db,
            path: path.to_path_buf(),
            schema,
            handles,
            fail_stop: FailStop::default(),
            _cache: cache,
            wbm,
        };
        let state = check_schema(&engine)?;
        Ok((engine, state))
    }

    fn handle(&self, cf: Cf) -> &CfPtr {
        &self.handles.ptrs[cf.index()]
    }

    /// The underlying database, for engine-specific inspection (properties, manual compaction).
    /// Never write through it: such writes bypass the fail-stop latch, and may wait in RocksDB
    /// for ever (DEC-164).
    pub fn raw(&self) -> &DB {
        &self.db
    }

    /// Waits until no memtable flush or compaction is pending or running in any column family.
    ///
    /// Fails (and latches, as a write would) if a background flush or compaction fails, since
    /// the pending work may then never drain; fails without latching after `timeout`.
    pub fn wait_for_compactions(&self, poll: Duration, timeout: Duration) -> Result<()> {
        const BUSY: [&str; 4] = [
            "rocksdb.mem-table-flush-pending",
            "rocksdb.num-running-flushes",
            "rocksdb.compaction-pending",
            "rocksdb.num-running-compactions",
        ];
        let deadline = Instant::now() + timeout;
        loop {
            self.check_background().map_err(|e| self.fail_stop.latch(e))?;
            let mut busy = false;
            for cf in &self.handles.ptrs {
                for prop in BUSY {
                    busy |=
                        self.db.property_int_value_cf(cf, prop).map_err(engine_error)?.unwrap_or(0)
                            > 0;
                }
            }
            if !busy {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(StoreError::engine(
                    ENGINE,
                    ErrorKind::Other,
                    format!("flushes or compactions still pending after {timeout:?}"),
                ));
            }
            std::thread::sleep(poll);
        }
    }

    /// Waits, without polling, until no memtable flush or compaction is scheduled or running: the
    /// last one has then also freed its memtables, which a flush that
    /// [`flush_memtables`](KvEngine::flush_memtables) waited for may not have yet. A memtable
    /// RocksDB has marked full is switched, and its flush scheduled, only at the next write, and
    /// memtables a live iterator pins stay allocated.
    ///
    /// Fails (and latches, as a write would) with the background error RocksDB holds, even one it
    /// is recovering from.
    pub fn wait_idle(&self) -> Result<()> {
        self.db
            .wait_for_compact(&WaitForCompactOptions::default())
            .map_err(|e| self.fail_stop.latch(engine_error(e)))
    }

    /// Fails if a background flush or compaction has failed since the database was opened, even
    /// if RocksDB has recovered from it by itself since (it does after `ENOSPC` once space is
    /// free again).
    fn check_background(&self) -> Result<()> {
        let errors = self
            .db
            .property_int_value("rocksdb.background-errors")
            .map_err(engine_error)?
            .unwrap_or(0);
        if errors == 0 {
            return Ok(());
        }
        // While the database is stopped, every write returns the error that stopped it; an empty
        // batch commits nothing, and a write that would wait is refused as a stall instead.
        match self.write_once(&raw_batch(0), false) {
            Err(status) if status != WRITE_STALL => return Err(status_error(status)),
            _ => {}
        }
        // RocksDB did not stop writes: it has recovered by itself (after `ENOSPC`, once space was
        // freed), or the error was soft (a compaction that ran out of space). The cause is only
        // in the LOG.
        let cause = last_background_error(&self.path.join("LOG"));
        let kind = cause.as_deref().and_then(kind_from_message).unwrap_or(ErrorKind::Io);
        Err(StoreError::engine(
            ENGINE,
            kind,
            format!(
                "{errors} background flush or compaction error(s), for which RocksDB did not \
                 stop writes: {}",
                cause.as_deref().unwrap_or("the cause is in the database's LOG")
            ),
        ))
    }

    /// Writes `batch` (DEC-164). RocksDB never wakes a writer that waits on the write buffer
    /// manager when the flush that would free its memory fails, so a write never waits inside
    /// RocksDB: refused as a stall, it runs [`check_writable`](Self::check_writable) (nothing
    /// commits after a latched failure), and otherwise tries again after a pause, until RocksDB's
    /// own threads have cleared the stall.
    fn write_batch(
        &self,
        batch: &Raw<librocksdb_sys::rocksdb_writebatch_t>,
        sync: bool,
    ) -> Result<()> {
        retry_stalled(|| self.write_once(batch, sync), || self.check_writable())?;
        self.start_due_flush()
    }

    /// Starts the memtable flush that the write buffer manager is due, without waiting for it
    /// (CH2c D42). RocksDB starts it only at the next write (`PreprocessWrite`), which it then
    /// refuses as a stall until the flush ends, so a writer that builds its next batch between
    /// the two (W, between sub-batches) never overlaps the flush with that work. The test is
    /// RocksDB's own `WriteBufferManager::ShouldFlush`, with the active memtables' size for the
    /// manager's mutable memory; the flush is the same atomic flush of every column family, so
    /// memory stays within the manager's bound. If it cannot start, the write that committed
    /// fails all the same, as a barrier's failure does: the latch stops the engine.
    fn start_due_flush(&self) -> Result<()> {
        let Some(wbm) = &self.wbm else {
            return Ok(());
        };
        let (size, usage) = (wbm.get_buffer_size() as u64, wbm.get_usage() as u64);
        // A manager of size 0 is disabled, and RocksDB's `ShouldFlush` never holds for it. Each
        // property read below takes the database's mutex. The active memtables are part of the
        // manager's usage, so with usage at most half the buffer neither test can hold (a margin
        // of 3/8 of the buffer for the two counts' approximations).
        if size == 0 || usage <= size / 2 {
            return Ok(());
        }
        let mut mutable = 0;
        for cf in &self.handles.ptrs {
            let active = self.db.property_int_value_cf(cf, "rocksdb.cur-size-active-mem-table");
            mutable += active.map_err(engine_error)?.unwrap_or(0);
        }
        let due = mutable > size / 8 * 7 || (usage >= size && mutable >= size / 2);
        if due {
            self.flush_all(false)
        } else {
            Ok(())
        }
    }

    /// Flushes every column family's memtables, atomically across them, with
    /// `allow_write_stall` (see [`flush_memtables`](KvEngine::flush_memtables)); waits for the
    /// flush to end if `wait`.
    fn flush_all(&self, wait: bool) -> Result<()> {
        use librocksdb_sys as ffi;
        let mut cfs: Vec<_> = self.handles.ptrs.iter().map(|cf| cf.0).collect();
        let n = std::ffi::c_int::try_from(cfs.len()).expect("a schema's column families");
        // SAFETY: the options are created and destroyed here; the database and its column
        // family handles are open for as long as `self`; `err` is RocksDB's to set.
        unsafe {
            let opts = Raw(ffi::rocksdb_flushoptions_create(), ffi::rocksdb_flushoptions_destroy);
            ffi::rocksdb_flushoptions_set_wait(opts.0, u8::from(wait));
            ffi::rocksdb_flushoptions_set_allow_write_stall(opts.0, 1);
            let mut err = std::ptr::null_mut();
            ffi::rocksdb_flush_cfs(self.db.as_raw_ptr(), opts.0, cfs.as_mut_ptr(), n, &mut err);
            take_error(err).map_err(status_error)
        }
    }

    /// Writes `batch` once with `no_slowdown`: `Err` holds RocksDB's status message,
    /// [`WRITE_STALL`] if the write would have waited.
    fn write_once(
        &self,
        batch: &Raw<librocksdb_sys::rocksdb_writebatch_t>,
        sync: bool,
    ) -> std::result::Result<(), String> {
        use librocksdb_sys as ffi;
        // SAFETY: the options are created and destroyed here; the database is open for as long
        // as `self`, and the batch for as long as its guard; `err` is RocksDB's to set.
        unsafe {
            let opts = Raw(ffi::rocksdb_writeoptions_create(), ffi::rocksdb_writeoptions_destroy);
            ffi::rocksdb_writeoptions_set_sync(opts.0, u8::from(sync));
            ffi::rocksdb_writeoptions_set_no_slowdown(opts.0, 1);
            let mut err = std::ptr::null_mut();
            ffi::rocksdb_write(self.db.as_raw_ptr(), opts.0, batch.0, &mut err);
            take_error(err)
        }
    }

    /// Runs a write or barrier under the fail-stop latch: refused if a failure is latched, and
    /// latched if it fails.
    fn guarded(&self, f: impl FnOnce() -> Result<()>) -> Result<()> {
        self.check_writable().and_then(|()| f()).map_err(|e| self.fail_stop.latch(e))
    }

    /// Refused if a failure is latched; fails if a background flush or compaction has failed
    /// ([`check_background`](Self::check_background)). Every write and barrier runs it first, and
    /// a stalled write before each retry (DEC-164), so one check covers both.
    fn check_writable(&self) -> Result<()> {
        self.fail_stop.check(ENGINE)?;
        self.check_background()
    }

    /// The kind of the failure that stopped writes, if one has.
    pub fn failed(&self) -> Option<ErrorKind> {
        self.fail_stop.failed()
    }

    /// The engine's memory, from RocksDB's properties (CH2 §2.5's diagnostics). The block cache
    /// is shared, so its figures are read once; the others are summed over column families.
    ///
    /// # Errors
    ///
    /// RocksDB's, reading a property.
    pub fn memory_usage(&self) -> Result<RocksMemory> {
        let prop = |cf: &CfPtr, name: &str| -> Result<u64> {
            let value = self.db.property_int_value_cf(cf, name).map_err(engine_error)?;
            Ok(value.unwrap_or_default())
        };
        let sum = |name: &str| -> Result<u64> {
            self.handles.ptrs.iter().try_fold(0, |n, cf| Ok(n + prop(cf, name)?))
        };
        let first = &self.handles.ptrs[0];
        Ok(RocksMemory {
            block_cache_capacity: prop(first, "rocksdb.block-cache-capacity")?,
            block_cache_usage: prop(first, "rocksdb.block-cache-usage")?,
            block_cache_pinned_usage: prop(first, "rocksdb.block-cache-pinned-usage")?,
            cur_size_all_mem_tables: sum("rocksdb.cur-size-all-mem-tables")?,
            size_all_mem_tables: sum("rocksdb.size-all-mem-tables")?,
            estimate_table_readers_mem: sum("rocksdb.estimate-table-readers-mem")?,
            write_buffer_manager_usage: self.wbm.as_ref().map(|w| w.get_usage() as u64),
            write_buffer_manager_size: self.wbm.as_ref().map(|w| w.get_buffer_size() as u64),
        })
    }
}

/// [`RocksEngine::memory_usage`]: RocksDB's memory properties, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RocksMemory {
    /// `rocksdb.block-cache-capacity`.
    pub block_cache_capacity: u64,
    /// `rocksdb.block-cache-usage`: everything charged to the block cache, memtables included
    /// when a `WriteBufferManager` charges them.
    pub block_cache_usage: u64,
    /// `rocksdb.block-cache-pinned-usage`.
    pub block_cache_pinned_usage: u64,
    /// `rocksdb.cur-size-all-mem-tables`: active and unflushed immutable memtables.
    pub cur_size_all_mem_tables: u64,
    /// `rocksdb.size-all-mem-tables`: also the flushed memtables still pinned.
    pub size_all_mem_tables: u64,
    /// `rocksdb.estimate-table-readers-mem`: table readers outside the block cache.
    pub estimate_table_readers_mem: u64,
    /// The `WriteBufferManager`'s usage, if memtables are charged to the cache.
    pub write_buffer_manager_usage: Option<u64>,
    /// Its size: memtables flush near 7/8 of it, and writers stall above it.
    pub write_buffer_manager_size: Option<u64>,
}

impl Drop for RocksEngine {
    fn drop(&mut self) {
        // A live batch holds the database open, with its `LOCK`, and skips the WAL sync on close.
        if !std::thread::panicking() {
            debug_assert_eq!(
                Arc::strong_count(&self.handles),
                1,
                "a RocksBatch outlives its RocksEngine: the database stays open until it is dropped"
            );
        }
    }
}

fn io_error(path: &Path, e: std::io::Error) -> StoreError {
    let kind = match e.kind() {
        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded => ErrorKind::NoSpace,
        _ => ErrorKind::Io,
    };
    StoreError::engine(ENGINE, kind, format!("{}: {e}", path.display()))
}

/// Files RocksDB writes while creating a database before `CURRENT` exists: a creation that died
/// early (disk full, killed, bad option) leaves only these, and the directory can be created
/// again. (`*.dbtmp` is a `CURRENT` or `OPTIONS` file being written.)
fn is_creation_debris(name: &str) -> bool {
    matches!(name, "LOCK" | "LOG" | "IDENTITY")
        || ["LOG.old.", "OPTIONS-", "MANIFEST-"].iter().any(|p| name.starts_with(p))
        || name.ends_with(".dbtmp")
}

/// Table, log and blob files (`000123.sst`): they only exist in a database that was created
/// (`CURRENT` written) and used.
fn is_database_data(name: &str) -> bool {
    name.split_once('.').is_some_and(|(number, ext)| {
        !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
            && matches!(ext, "sst" | "log" | "ldb" | "blob")
    })
}

/// Decides whether a directory without `CURRENT` may become a new database. It may if it is
/// missing or empty, or holds only [creation debris](is_creation_debris) and a `lost+found`
/// directory (a filesystem dedicated to the database; `lost+found` belongs to the filesystem, is
/// often unreadable to the node, and is never part of a database). Table or log files without `CURRENT` are a
/// damaged database of ours (`Corruption`), never silently replaced; anything else is not a
/// database (`UnknownDatabase`). Nothing is written either way.
fn classify_uncreated(path: &Path, schema: &'static DbSchema) -> Result<()> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_error(path, e)),
    };
    let mut foreign = None;
    for entry in entries {
        let entry = entry.map_err(|e| io_error(path, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_database_data(&name) {
            return Err(StoreError::engine(
                ENGINE,
                ErrorKind::Corruption,
                format!(
                    "{} holds database file `{name}` but no CURRENT file: the database is damaged \
                     or incomplete",
                    path.display()
                ),
            ));
        }
        let lost_found = name == "lost+found" && entry.file_type().is_ok_and(|t| t.is_dir());
        if !is_creation_debris(&name) && !lost_found {
            foreign.get_or_insert(name);
        }
    }
    match foreign {
        Some(name) => Err(StoreError::UnknownDatabase {
            db: schema.name,
            what: format!("it holds `{name}`, which is not part of a database"),
        }),
        None => Ok(()),
    }
}

/// Read-only checks of an existing database before anything may write to it.
fn inspect_existing(path: &Path, schema: &'static DbSchema, existing: &[String]) -> Result<()> {
    let unknown = |what: &str| StoreError::UnknownDatabase {
        db: schema.name,
        what: what.into(),
    };
    let db = DB::open_cf_for_read_only(&Options::default(), path, existing, false)
        .map_err(engine_error)?;
    let non_empty = |name: &str| {
        db.cf_handle(name)
            .is_some_and(|cf| db.iterator_cf(cf, IteratorMode::Start).next().is_some())
    };
    let record = match db.cf_handle("meta") {
        Some(meta) => db.get_cf(meta, SCHEMA_KEY).map_err(engine_error)?,
        None => None,
    };
    match record {
        // Another database, or a newer version of ours (which may have more column families).
        Some(record) => {
            check_record(schema, &record)?;
        }
        // Created but never stamped (or not ours): only acceptable when completely empty.
        None if existing.iter().any(|name| non_empty(name)) => {
            return Err(unknown("it holds data but no schema record"));
        }
        None => {}
    }
    let foreign = existing.iter().find(|name| *name != "default" && schema.cf(name).is_none());
    if let Some(cf) = foreign {
        return Err(unknown(&format!("it has column family `{cf}`, which this schema lacks")));
    }
    if non_empty("default") {
        return Err(unknown("it holds data in RocksDB's `default` column family"));
    }
    Ok(())
}

fn cf_options(spec: &CfSpec, config: &RocksConfig, cache: &Cache) -> Options {
    let mut table = BlockBasedOptions::default();
    table.set_block_cache(cache);
    table.set_cache_index_and_filter_blocks(true);
    table.set_pin_l0_filter_and_index_blocks_in_cache(config.pin_l0_filter_and_index_blocks);
    let mut opts = Options::default();
    let (bytes, buffers) = config
        .cf_write_buffers
        .iter()
        .find(|(name, ..)| *name == spec.name)
        .map_or((config.write_buffer_bytes, config.max_write_buffers), |&(_, b, n)| (b, n));
    opts.set_write_buffer_size(bytes);
    opts.set_max_write_buffer_number(buffers);
    // DEC-164: the write controller stops writes and never slows them. A write never waits in
    // RocksDB (`RocksEngine::write_batch`), and a `no_slowdown` write refused during a slowdown
    // moves the controller's next refill a whole delay later (`write_controller.cc` `GetDelay`),
    // so retrying could keep writes out for ever. Level 0's slowdown is therefore its stop
    // trigger, and the pending-compaction slowdown its stop limit (0 is sanitized to the hard
    // limit, `column_family.cc:362`). More than three memtables would add a memtable slowdown
    // (`column_family.cc:1027`). This holds for every schema, since every write is retried so.
    // RocksDB's `default` column family keeps its own triggers, but it is always empty.
    assert!(buffers <= 3, "{}: {buffers} memtables slow writes down (DEC-164)", spec.name);
    opts.set_level_zero_slowdown_writes_trigger(L0_STOP_WRITES);
    opts.set_level_zero_stop_writes_trigger(L0_STOP_WRITES);
    opts.set_soft_pending_compaction_bytes_limit(0);
    opts.set_level_compaction_dynamic_level_bytes(true);
    opts.set_disable_auto_compactions(config.quiet);
    #[cfg(test)]
    tests::gate_compactions(spec, &mut opts);
    match spec.profile {
        CfProfile::PointLookup => {
            // Set explicitly rather than through `optimize_for_point_lookup`, which would install
            // a block cache of its own outside the `-dbcache` budget.
            table.set_bloom_filter(config.bloom_bits_per_key, false);
            table.set_whole_key_filtering(true);
            table.set_data_block_index_type(DataBlockIndexType::BinaryAndHash);
            table.set_data_block_hash_ratio(0.75);
            if config.partitioned_filters {
                table.set_index_type(BlockBasedIndexType::TwoLevelIndexSearch);
                table.set_partition_filters(true);
                table.set_pin_top_level_index_and_filter(true);
                table.set_cache_index_and_filter_blocks_with_high_priority(true);
                table.set_metadata_block_size(4096);
            }
            opts.set_memtable_whole_key_filtering(true);
            opts.set_memtable_prefix_bloom_ratio(0.02);
            opts.set_compression_type(DBCompressionType::None);
        }
        CfProfile::Ordered => opts.set_compression_type(DBCompressionType::Lz4),
        CfProfile::Compressed => {
            opts.set_compression_type(DBCompressionType::Lz4);
            opts.set_bottommost_compression_type(DBCompressionType::Zstd);
        }
    }
    opts.set_block_based_table_factory(&table);
    opts
}

/// A column family handle owned by the database in [`CfHandles::_db`].
#[derive(Clone, Copy)]
struct CfPtr(*mut librocksdb_sys::rocksdb_column_family_handle_t);

impl AsColumnFamilyRef for CfPtr {
    fn inner(&self) -> *mut librocksdb_sys::rocksdb_column_family_handle_t {
        self.0
    }
}

/// The column family handles of one open database, kept valid by holding the database itself.
struct CfHandles {
    ptrs: Vec<CfPtr>,
    _db: Arc<DB>,
}

// SAFETY: the handles point into `_db`, which this struct keeps alive; column families are never
// dropped while the database is open (the engine has no `drop_cf`). RocksDB only reads a handle's
// column family ID when a batch records an operation, which is thread-safe.
unsafe impl Send for CfHandles {}
unsafe impl Sync for CfHandles {}

/// DEC-164's loop: `write` until RocksDB does not refuse it as a stall, running `check` (for a
/// background error) before each pause.
fn retry_stalled(
    mut write: impl FnMut() -> std::result::Result<(), String>,
    mut check: impl FnMut() -> Result<()>,
) -> Result<()> {
    let mut pause = STALL_PAUSE_MIN;
    loop {
        match write() {
            Err(status) if status == WRITE_STALL => {
                check()?;
                std::thread::sleep(pause);
                pause = (pause * 2).min(STALL_PAUSE_MAX);
            }
            done => return done.map_err(status_error),
        }
    }
}

/// A RocksDB C object and its destructor, which runs when the guard drops. Locals drop in
/// reverse order, so guards made as their objects are created destroy an object before what it
/// refers to, on a panic too.
struct Raw<T>(*mut T, unsafe extern "C" fn(*mut T));

impl<T> Drop for Raw<T> {
    fn drop(&mut self) {
        // SAFETY: the object came from the create call that its destructor matches, and only
        // this guard destroys it.
        unsafe { (self.1)(self.0) }
    }
}

// SAFETY: a write batch has no thread affinity, and its one owner uses it.
unsafe impl Send for Raw<librocksdb_sys::rocksdb_writebatch_t> {}

/// An empty write batch with room for `bytes`.
fn raw_batch(bytes: usize) -> Raw<librocksdb_sys::rocksdb_writebatch_t> {
    // SAFETY: plain allocation; no protection bytes and no timestamps, as rust-rocksdb's
    // `WriteBatch::with_capacity_bytes`.
    let batch = unsafe { librocksdb_sys::rocksdb_writebatch_create_with_params(bytes, 0, 0, 0) };
    Raw(batch, librocksdb_sys::rocksdb_writebatch_destroy)
}

/// RocksDB's error string `err`, freed, or `Ok` if it is null.
///
/// # Safety
///
/// `err` is null or a string RocksDB allocated for an error (`SaveError`, with `strdup`).
unsafe fn take_error(err: *mut std::ffi::c_char) -> std::result::Result<(), String> {
    if err.is_null() {
        return Ok(());
    }
    // SAFETY: a NUL-terminated string, freed once, with the allocator `rocksdb_free` matches.
    let message = unsafe { std::ffi::CStr::from_ptr(err) }.to_string_lossy().into_owned();
    unsafe { librocksdb_sys::rocksdb_free(err.cast()) };
    Err(message)
}

/// A batch for [`RocksEngine`]: a RocksDB write batch that records operations directly (no
/// second copy of the batch), plus the handles of its database. A batch keeps that database open
/// (and its `LOCK` held) until the batch is dropped. It is built through RocksDB's C API: a write
/// refused as a stall is tried again with the same batch (DEC-164), and rust-rocksdb's write
/// takes its batch by value.
pub struct RocksBatch {
    batch: Raw<librocksdb_sys::rocksdb_writebatch_t>,
    handles: Arc<CfHandles>,
}

// SAFETY (every call below): the batch is live and this batch's alone; a column family handle is
// its database's, which `handles` keeps open; RocksDB copies the keys and values it is given.
impl KvBatch for RocksBatch {
    fn put(&mut self, cf: Cf, key: &[u8], value: &[u8]) {
        let (k, v) = (key.as_ptr().cast(), value.as_ptr().cast());
        let cf = self.handles.ptrs[cf.index()].0;
        unsafe {
            librocksdb_sys::rocksdb_writebatch_put_cf(
                self.batch.0,
                cf,
                k,
                key.len(),
                v,
                value.len(),
            )
        }
    }

    fn delete(&mut self, cf: Cf, key: &[u8]) {
        let cf = self.handles.ptrs[cf.index()].0;
        unsafe {
            librocksdb_sys::rocksdb_writebatch_delete_cf(
                self.batch.0,
                cf,
                key.as_ptr().cast(),
                key.len(),
            )
        }
    }

    fn delete_range(&mut self, cf: Cf, start: &[u8], end: &[u8]) {
        // RocksDB fails the whole write for a range that ends before it starts
        // (`write_batch.cc:2731` at 11.8.1), and skips an empty one.
        if start < end {
            let (s, e) = (start.as_ptr().cast(), end.as_ptr().cast());
            let cf = self.handles.ptrs[cf.index()].0;
            unsafe {
                librocksdb_sys::rocksdb_writebatch_delete_range_cf(
                    self.batch.0,
                    cf,
                    s,
                    start.len(),
                    e,
                    end.len(),
                )
            }
        }
    }

    fn len(&self) -> usize {
        let count = unsafe { librocksdb_sys::rocksdb_writebatch_count(self.batch.0) };
        usize::try_from(count).expect("a batch's count is not negative")
    }

    fn size_bytes(&self) -> usize {
        let mut size = 0;
        unsafe { librocksdb_sys::rocksdb_writebatch_data(self.batch.0, &mut size) };
        size
    }

    fn clear(&mut self) {
        unsafe { librocksdb_sys::rocksdb_writebatch_clear(self.batch.0) }
    }
}

/// The smallest key greater than every key starting with `prefix`, or `None` if there is none
/// (empty prefix or all `0xff`).
fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let end = prefix.iter().rposition(|&b| b != 0xff)?;
    let mut upper = prefix[..=end].to_vec();
    upper[end] += 1;
    Some(upper)
}

fn prefix_read_options(prefix: &[u8]) -> ReadOptions {
    let mut opts = ReadOptions::default();
    if !prefix.is_empty() {
        opts.set_iterate_lower_bound(prefix);
    }
    if let Some(upper) = prefix_upper_bound(prefix) {
        opts.set_iterate_upper_bound(upper);
    }
    opts
}

fn map_entries<I>(iter: I) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>>
where
    I: Iterator<Item = Result<(Box<[u8]>, Box<[u8]>), rocksdb::Error>>,
{
    iter.map(|r| r.map(|(k, v)| (k.into_vec(), v.into_vec())).map_err(engine_error))
}

fn collect_multi(
    results: Vec<Result<Option<rocksdb::DBPinnableSlice<'_>>, rocksdb::Error>>,
) -> Result<Vec<Option<Vec<u8>>>> {
    results.into_iter().map(|r| r.map(|v| v.map(|v| v.to_vec())).map_err(engine_error)).collect()
}

impl KvRead for RocksEngine {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.db.get_pinned_cf(self.handle(cf), key).map_err(engine_error)?.map(|v| v.to_vec()))
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        collect_multi(self.db.batched_multi_get_cf(self.handle(cf), keys.iter().copied(), false))
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        map_entries(self.db.iterator_cf_opt(
            self.handle(cf),
            prefix_read_options(prefix),
            IteratorMode::Start,
        ))
    }

    // Core: dbwrapper.cpp:136 (`iteroptions.fill_cache = false`)
    fn iter_prefix_uncached<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        let mut opts = prefix_read_options(prefix);
        opts.fill_cache(false);
        map_entries(self.db.iterator_cf_opt(self.handle(cf), opts, IteratorMode::Start))
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        map_entries(self.db.iterator_cf_opt(
            self.handle(cf),
            prefix_read_options(prefix),
            IteratorMode::From(start.max(prefix), Direction::Forward),
        ))
    }
}

/// A snapshot of a [`RocksEngine`].
pub struct RocksSnapshot<'a> {
    engine: &'a RocksEngine,
    snapshot: rocksdb::Snapshot<'a>,
}

impl RocksSnapshot<'_> {
    fn read_options(&self) -> ReadOptions {
        let mut opts = ReadOptions::default();
        opts.set_snapshot(&self.snapshot);
        opts
    }
}

impl KvRead for RocksSnapshot<'_> {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self
            .snapshot
            .get_pinned_cf(self.engine.handle(cf), key)
            .map_err(engine_error)?
            .map(|v| v.to_vec()))
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        collect_multi(self.engine.db.batched_multi_get_cf_opt(
            self.engine.handle(cf),
            keys.iter().copied(),
            false,
            &self.read_options(),
        ))
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        map_entries(self.snapshot.iterator_cf_opt(
            self.engine.handle(cf),
            prefix_read_options(prefix),
            IteratorMode::Start,
        ))
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        map_entries(self.snapshot.iterator_cf_opt(
            self.engine.handle(cf),
            prefix_read_options(prefix),
            IteratorMode::From(start.max(prefix), Direction::Forward),
        ))
    }
}

impl KvEngine for RocksEngine {
    type Batch = RocksBatch;
    type Snapshot<'a> = RocksSnapshot<'a>;

    fn schema(&self) -> &'static DbSchema {
        self.schema
    }

    fn batch(&self) -> RocksBatch {
        self.batch_with_capacity(0)
    }

    fn batch_with_capacity(&self, bytes: usize) -> RocksBatch {
        RocksBatch {
            batch: raw_batch(bytes),
            handles: self.handles.clone(),
        }
    }

    fn write(&self, batch: RocksBatch, commit: Commit) -> Result<()> {
        assert!(Arc::ptr_eq(&batch.handles, &self.handles), "batch from another database");
        self.guarded(|| self.write_batch(&batch.batch, commit == Commit::Durable))
    }

    fn sync_wal(&self) -> Result<()> {
        self.guarded(|| self.db.flush_wal(true).map_err(engine_error))
    }

    /// Flushes every column family's memtables, atomically across them (`atomic_flush`), with
    /// `allow_write_stall` (DEC-164). Without it, RocksDB first waits until the flush would not
    /// stall writes, and that wait outlasts a soft background error, such as compactions that
    /// cannot run on a full disk (`db_impl_compaction_flush.cc:3098-3170`). The flush itself
    /// ends when it is done or a background error stops the database (`:3183-3250`).
    /// rust-rocksdb's `FlushOptions` cannot set it.
    fn flush_memtables(&self) -> Result<()> {
        self.guarded(|| self.flush_all(true))
    }

    fn snapshot(&self) -> RocksSnapshot<'_> {
        RocksSnapshot {
            engine: self,
            snapshot: self.db.snapshot(),
        }
    }

    fn approximate_size(&self, cf: Cf) -> Result<u64> {
        let handle = self.handle(cf);
        let mut total = 0;
        for prop in ["rocksdb.estimate-live-data-size", "rocksdb.cur-size-all-mem-tables"] {
            total +=
                self.db.property_int_value_cf(handle, prop).map_err(engine_error)?.unwrap_or(0);
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use rocksdb::{WriteBatch, WriteOptions};

    use super::*;

    static SCHEMA: DbSchema = DbSchema {
        name: "rocks-test",
        version: 1,
        cfs: &[
            CfSpec::new("meta", CfProfile::Ordered),
            CfSpec::new("coins", CfProfile::PointLookup),
            CfSpec::new("index", CfProfile::Compressed),
        ],
    };

    fn open(dir: &Path) -> (RocksEngine, SchemaState) {
        RocksEngine::open(dir, &SCHEMA, &RocksConfig::default()).unwrap()
    }

    #[test]
    fn upper_bounds() {
        assert_eq!(prefix_upper_bound(b""), None);
        assert_eq!(prefix_upper_bound(&[0xff, 0xff]), None);
        assert_eq!(prefix_upper_bound(&[1, 2]), Some(vec![1, 3]));
        assert_eq!(prefix_upper_bound(&[1, 0xff]), Some(vec![2]));
    }

    #[test]
    fn batches_record_into_the_right_column_family() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, _) = open(dir.path());
        let coins = SCHEMA.cf("coins").unwrap();
        let index = SCHEMA.cf("index").unwrap();
        let mut batch = engine.batch();
        batch.put(coins, b"k", b"coins");
        batch.put(index, b"k", b"index");
        batch.delete(index, b"gone");
        assert_eq!(batch.len(), 3);
        assert!(batch.size_bytes() > 12);
        engine.write(batch, Commit::Durable).unwrap();
        assert_eq!(engine.get(coins, b"k").unwrap(), Some(b"coins".to_vec()));
        assert_eq!(engine.get(index, b"k").unwrap(), Some(b"index".to_vec()));
        assert_eq!(engine.get(DbSchema::META, b"k").unwrap(), None);
    }

    #[test]
    #[should_panic(expected = "batch from another database")]
    fn refuses_a_batch_of_another_database() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (ea, _) = open(a.path());
        let (eb, _) = open(b.path());
        let _ = eb.write(ea.batch(), Commit::Buffered);
    }

    #[test]
    fn reopen_keeps_data_and_schema() {
        let dir = tempfile::tempdir().unwrap();
        let coins = SCHEMA.cf("coins").unwrap();
        {
            let (engine, state) = open(dir.path());
            assert_eq!(state, SchemaState::Created);
            let mut batch = engine.batch();
            batch.put(coins, b"a", b"1");
            engine.write(batch, Commit::Buffered).unwrap();
            engine.sync_wal().unwrap();
        }
        let (engine, state) = open(dir.path());
        assert_eq!(state, SchemaState::Current);
        assert_eq!(engine.get(coins, b"a").unwrap(), Some(b"1".to_vec()));
        engine.flush_memtables().unwrap();
        assert_eq!(engine.get(coins, b"a").unwrap(), Some(b"1".to_vec()));
    }

    #[test]
    fn refuses_newer_schema_without_writing() {
        static NEWER: DbSchema = DbSchema {
            name: "rocks-test",
            version: 2,
            cfs: SCHEMA.cfs,
        };
        let dir = tempfile::tempdir().unwrap();
        drop(RocksEngine::open(dir.path(), &NEWER, &RocksConfig::default()).unwrap());
        let err = RocksEngine::open(dir.path(), &SCHEMA, &RocksConfig::default()).err().unwrap();
        assert!(matches!(
            err,
            StoreError::SchemaTooNew {
                found: 2,
                supported: 1,
                ..
            }
        ));
        let (_, state) = RocksEngine::open(dir.path(), &NEWER, &RocksConfig::default()).unwrap();
        assert_eq!(state, SchemaState::Current);
    }

    /// Names, sizes and modification times of a directory's files: unchanged after a refused open.
    fn dir_state(dir: &Path) -> Vec<(std::ffi::OsString, u64, std::time::SystemTime)> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let meta = e.metadata().unwrap();
                (e.file_name(), meta.len(), meta.modified().unwrap())
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn refuses_a_foreign_database_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        {
            // A database with data only in `default`, as a LevelDB-format directory opens.
            let db = DB::open_default(dir.path()).unwrap();
            db.put(b"\x00obfuscate_key", b"x").unwrap();
        }
        let before = dir_state(dir.path());
        let err = RocksEngine::open(dir.path(), &SCHEMA, &RocksConfig::default()).err().unwrap();
        assert!(matches!(err, StoreError::UnknownDatabase { .. }), "{err}");
        assert_eq!(dir_state(dir.path()), before);
    }

    /// Another database whose column families all exist in this schema, at a version that is not
    /// newer: only the name in the schema record tells it apart.
    #[test]
    fn refuses_another_database_with_fitting_column_families() {
        static OTHER: DbSchema = DbSchema {
            name: "other",
            version: 1,
            cfs: &[
                CfSpec::new("meta", CfProfile::Ordered),
                CfSpec::new("coins", CfProfile::PointLookup),
            ],
        };
        let dir = tempfile::tempdir().unwrap();
        drop(RocksEngine::open(dir.path(), &OTHER, &RocksConfig::default()).unwrap());
        let before = dir_state(dir.path());
        let err = RocksEngine::open(dir.path(), &SCHEMA, &RocksConfig::default()).err().unwrap();
        assert!(matches!(err, StoreError::UnknownDatabase { .. }), "{err}");
        assert!(err.to_string().contains("`other`"), "{err}");
        assert_eq!(dir_state(dir.path()), before);
    }

    #[test]
    fn refuses_a_non_empty_directory_that_is_not_a_database() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blk00000.dat"), b"not a database").unwrap();
        let before = dir_state(dir.path());
        let err = RocksEngine::open(dir.path(), &SCHEMA, &RocksConfig::default()).err().unwrap();
        assert!(matches!(err, StoreError::UnknownDatabase { .. }), "{err}");
        assert!(err.to_string().contains("blk00000.dat"), "{err}");
        assert_eq!(dir_state(dir.path()), before);
    }

    /// A first creation that died before writing `CURRENT` (disk full, killed) leaves `LOG`,
    /// `LOCK`, `IDENTITY`, `OPTIONS-*` and `MANIFEST-*`: the next start creates the database.
    #[test]
    fn creates_over_the_debris_of_a_failed_first_creation() {
        let created = tempfile::tempdir().unwrap();
        drop(open(created.path()));
        let dir = tempfile::tempdir().unwrap();
        let mut copied = Vec::new();
        for entry in std::fs::read_dir(created.path()).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if is_creation_debris(&name) {
                std::fs::copy(created.path().join(&name), dir.path().join(&name)).unwrap();
                copied.push(name);
            }
        }
        assert!(copied.iter().any(|n| n.starts_with("MANIFEST-")), "{copied:?}");
        assert!(copied.iter().any(|n| n.starts_with("OPTIONS-")), "{copied:?}");
        let (engine, state) = open(dir.path());
        assert_eq!(state, SchemaState::Created);
        drop(engine);
        assert_eq!(open(dir.path()).1, SchemaState::Current);
    }

    #[test]
    fn creates_on_a_dedicated_filesystem_with_lost_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("lost+found")).unwrap();
        assert_eq!(open(dir.path()).1, SchemaState::Created);
        // As mke2fs makes it (mode 0700, owned by root): unreadable to the node.
        let dir = tempfile::tempdir().unwrap();
        let lost_found = dir.path().join("lost+found");
        std::fs::create_dir(&lost_found).unwrap();
        std::fs::write(lost_found.join("#123"), b"x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&lost_found, std::fs::Permissions::from_mode(0o000)).unwrap();
        let opened = RocksEngine::open(dir.path(), &SCHEMA, &RocksConfig::default());
        std::fs::set_permissions(&lost_found, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(opened.unwrap().1, SchemaState::Created);
    }

    #[test]
    fn only_numbered_table_and_log_files_are_database_data() {
        assert!(is_database_data("000012.sst") && is_database_data("000004.log"));
        assert!(is_database_data("000007.ldb") && is_database_data("000009.blob"));
        for name in ["debug.log", "LOG", "peers.dat", ".log", "12.sst.tmp", "x12.sst"] {
            assert!(!is_database_data(name), "{name}");
        }
        // A foreign directory with a `debug.log` is not one of ours, not a damaged one.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("debug.log"), b"x").unwrap();
        let err = RocksEngine::open(dir.path(), &SCHEMA, &RocksConfig::default()).err().unwrap();
        assert!(matches!(err, StoreError::UnknownDatabase { .. }), "{err}");
    }

    /// Damage to a database of ours is reported as corruption, not as "not our database", and
    /// nothing is replaced.
    #[test]
    fn reports_a_damaged_database_as_corruption() {
        let coins = SCHEMA.cf("coins").unwrap();
        let damaged = |damage: &dyn Fn(&Path)| {
            let dir = tempfile::tempdir().unwrap();
            {
                let (engine, _) = open(dir.path());
                let mut batch = engine.batch();
                batch.put(coins, b"k", b"v");
                engine.write(batch, Commit::Durable).unwrap();
            }
            damage(dir.path());
            let before = dir_state(dir.path());
            let err =
                RocksEngine::open(dir.path(), &SCHEMA, &RocksConfig::default()).err().unwrap();
            assert_eq!(dir_state(dir.path()), before);
            err
        };
        // `CURRENT` names a MANIFEST that does not exist.
        let err = damaged(&|dir| std::fs::write(dir.join("CURRENT"), "MANIFEST-999999\n").unwrap());
        assert!(matches!(err.kind(), Some(ErrorKind::Corruption | ErrorKind::Io)), "{err}");
        // `CURRENT` is gone, the log holding the data is not.
        let err = damaged(&|dir| std::fs::remove_file(dir.join("CURRENT")).unwrap());
        assert_eq!(err.kind(), Some(ErrorKind::Corruption), "{err}");
    }

    #[test]
    fn refuses_a_newer_schema_with_more_column_families() {
        static NEWER: DbSchema = DbSchema {
            name: "rocks-test",
            version: 2,
            cfs: &[
                CfSpec::new("meta", CfProfile::Ordered),
                CfSpec::new("coins", CfProfile::PointLookup),
                CfSpec::new("index", CfProfile::Compressed),
                CfSpec::new("added", CfProfile::Ordered),
            ],
        };
        let dir = tempfile::tempdir().unwrap();
        drop(RocksEngine::open(dir.path(), &NEWER, &RocksConfig::default()).unwrap());
        let before = DB::list_cf(&Options::default(), dir.path()).unwrap();
        let err = RocksEngine::open(dir.path(), &SCHEMA, &RocksConfig::default()).err().unwrap();
        assert!(
            matches!(
                err,
                StoreError::SchemaTooNew {
                    found: 2,
                    ..
                }
            ),
            "{err}"
        );
        assert_eq!(DB::list_cf(&Options::default(), dir.path()).unwrap(), before);
    }

    /// Buffered batches survive a process exit without a barrier (they are in the OS's hands).
    #[test]
    fn buffered_batches_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let coins = SCHEMA.cf("coins").unwrap();
        {
            let (engine, _) = open(dir.path());
            for i in 0..100u8 {
                let mut b = engine.batch();
                b.put(coins, &[i], &[i; 40]);
                engine.write(b, Commit::Buffered).unwrap();
            }
        }
        let (engine, _) = open(dir.path());
        assert_eq!(engine.iter_prefix(coins, &[]).count(), 100);
    }

    /// An uncached scan reads every entry and leaves the block cache as it found it; the cached
    /// scan, the control, fills it.
    #[test]
    fn an_uncached_scan_leaves_the_block_cache() {
        let dir = tempfile::tempdir().unwrap();
        let coins = SCHEMA.cf("coins").unwrap();
        {
            let (engine, _) = open(dir.path());
            write_values(&engine, CHARGED_VALUES);
            engine.flush_memtables().unwrap();
        }
        let (engine, _) = open(dir.path());
        let usage = || engine.memory_usage().unwrap().block_cache_usage;
        let before = usage();
        assert_eq!(engine.iter_prefix_uncached(coins, &[]).count(), CHARGED_VALUES as usize);
        assert_eq!(usage(), before, "the uncached scan filled the cache");
        assert_eq!(engine.iter_prefix(coins, &[]).count(), CHARGED_VALUES as usize);
        assert!(usage() > before + (1 << 20), "the cached scan left {} bytes", usage());
    }

    /// An engine over [`SCHEMA`] whose memtables are charged, through a write buffer manager of
    /// `wbm` bytes, to a block cache of `cache` bytes.
    fn open_charged(dir: &Path, cache: usize, wbm: usize) -> RocksEngine {
        let config = RocksConfig {
            block_cache_bytes: cache,
            charged_write_buffer_bytes: Some(wbm),
            ..RocksConfig::default()
        };
        RocksEngine::open(dir, &SCHEMA, &config).unwrap().0
    }

    /// The value [`write_values`] writes under each key, about 100 bytes in a memtable.
    const VALUE: [u8; 64] = [7; 64];
    /// The number of values that charge about 4 MiB of memtables to the cache.
    const CHARGED_VALUES: u32 = 40_000;
    /// The cache and the write buffer manager they are charged to and through.
    const CHARGED_CACHE: usize = 1 << 20;
    const CHARGED_WBM: usize = 32 << 20;

    /// One batch of `n` values into `coins`, under the keys `0..n`.
    fn write_values(engine: &RocksEngine, n: u32) {
        let coins = SCHEMA.cf("coins").unwrap();
        let mut b = engine.batch();
        for i in 0..n {
            b.put(coins, &i.to_be_bytes(), &VALUE);
        }
        engine.write(b, Commit::Buffered).unwrap();
    }

    /// The block cache's capacity is not strict (CH2 §2.5): the memtables' charge stays in it
    /// beyond its capacity. The control is a strict cache, which rust-rocksdb cannot build, so it
    /// is opened through RocksDB's C API: the same charge stays within its capacity.
    #[test]
    fn the_block_cache_keeps_a_charge_beyond_its_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let engine = open_charged(dir.path(), CHARGED_CACHE, CHARGED_WBM);
        write_values(&engine, CHARGED_VALUES);
        let m = engine.memory_usage().unwrap();
        assert_eq!(m.block_cache_capacity, CHARGED_CACHE as u64);
        assert!(m.block_cache_usage > 2 << 20, "{m:?}");
        // The strict cache holds some of the charge, up to its capacity.
        let strict = raw_charged_cache_usage(true);
        assert!(0 < strict && strict <= CHARGED_CACHE, "strict: {strict}");
        let not_strict = raw_charged_cache_usage(false);
        assert!(not_strict > 2 << 20, "not strict: {not_strict}");
    }

    /// The block cache's usage after [`CHARGED_VALUES`] values into a database opened through
    /// RocksDB's C API, whose write buffer manager of [`CHARGED_WBM`] charges its memtables to an
    /// LRU cache of [`CHARGED_CACHE`] with a strict capacity or not.
    fn raw_charged_cache_usage(strict: bool) -> usize {
        use librocksdb_sys as ffi;
        let dir = tempfile::tempdir().unwrap();
        let path = std::ffi::CString::new(dir.path().to_str().unwrap()).unwrap();
        // SAFETY: `err` is null or RocksDB's error string.
        let check = |err| unsafe { take_error(err) }.unwrap_or_else(|e| panic!("{e}"));
        // SAFETY: each object is owned by a `Raw` guard from its creation (the database from its
        // open), so on every path it is destroyed after the last use of anything that refers to
        // it: the database before its options, write buffer manager and cache. `path` and the
        // batch's keys and values outlive the calls.
        unsafe {
            let cache = Raw(
                if strict {
                    ffi::rocksdb_cache_create_lru_with_strict_capacity_limit(CHARGED_CACHE)
                } else {
                    ffi::rocksdb_cache_create_lru(CHARGED_CACHE)
                },
                ffi::rocksdb_cache_destroy,
            );
            let wbm = Raw(
                ffi::rocksdb_write_buffer_manager_create_with_cache(CHARGED_WBM, cache.0, false),
                ffi::rocksdb_write_buffer_manager_destroy,
            );
            let opts = Raw(ffi::rocksdb_options_create(), ffi::rocksdb_options_destroy);
            ffi::rocksdb_options_set_create_if_missing(opts.0, 1);
            ffi::rocksdb_options_set_write_buffer_manager(opts.0, wbm.0);
            let mut err = std::ptr::null_mut();
            let db = ffi::rocksdb_open(opts.0, path.as_ptr(), &mut err);
            check(err);
            let db = Raw(db, ffi::rocksdb_close);
            let batch = raw_batch(0);
            for i in 0..CHARGED_VALUES {
                let key = i.to_be_bytes();
                ffi::rocksdb_writebatch_put(
                    batch.0,
                    key.as_ptr().cast(),
                    key.len(),
                    VALUE.as_ptr().cast(),
                    VALUE.len(),
                );
            }
            let write_opts =
                Raw(ffi::rocksdb_writeoptions_create(), ffi::rocksdb_writeoptions_destroy);
            ffi::rocksdb_write(db.0, write_opts.0, batch.0, &mut err);
            check(err);
            ffi::rocksdb_cache_get_usage(cache.0)
        }
    }

    /// The write buffer manager stalls writers once its memory reaches its size (`allow_stall`,
    /// CH2 §2.5): a write that may not wait then fails as a stall. The control turns
    /// `allow_stall` off, and the same write goes through.
    #[test]
    fn writers_stall_at_the_write_buffer_managers_size() {
        let dir = tempfile::tempdir().unwrap();
        let engine = open_charged(dir.path(), 24 << 20, 1 << 20);
        let coins = *engine.handle(SCHEMA.cf("coins").unwrap());
        // The iterator pins the memtable the values go into, so the flush that the write starts
        // (D42) cannot free its memory.
        let _pin = engine.db.raw_iterator_cf(&coins);
        write_values(&engine, 20_000);
        let write = || {
            let mut no_wait = WriteOptions::default();
            no_wait.set_no_slowdown(true);
            let mut b = WriteBatch::default();
            b.put_cf(&coins, b"k", b"v");
            engine.db.write_opt(b, &no_wait)
        };
        let err = write().expect_err("a stall");
        assert!(err.to_string().contains("Write stall"), "{err}");
        engine.wbm.as_ref().unwrap().set_allow_stall(false);
        write().unwrap();
    }

    /// An engine write held by the write buffer manager waits, outside RocksDB (DEC-164), until
    /// a flush has freed the memory, then goes through: CH2 §2.5's bound holds on the engine's
    /// own write path. The memory stays charged while an iterator pins the flushed memtable.
    #[test]
    fn a_stalled_write_waits_until_memory_is_freed() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(open_charged(dir.path(), 24 << 20, 1 << 20));
        let coins = SCHEMA.cf("coins").unwrap();
        let pin = engine.db.raw_iterator_cf(engine.handle(coins));
        write_values(&engine, 20_000);
        let written = stalled_write(&engine, coins);
        let early = written.recv_timeout(Duration::from_millis(200));
        assert!(early.is_err(), "the write waits while the memory is pinned: {early:?}");
        drop(pin);
        let late = written.recv_timeout(Duration::from_secs(60));
        late.expect("the write goes through once the memory is freed").unwrap();
        assert_eq!(engine.get(coins, b"k").unwrap(), Some(b"v".to_vec()));
    }

    /// A write waiting on a stall ends when another write or barrier latches a failure
    /// (DEC-164): it does not outlast the failure, and nothing commits after it.
    #[test]
    fn a_stalled_write_ends_at_another_writes_failure() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(open_charged(dir.path(), 24 << 20, 1 << 20));
        let coins = SCHEMA.cf("coins").unwrap();
        let pin = engine.db.raw_iterator_cf(engine.handle(coins));
        write_values(&engine, 20_000);
        let written = stalled_write(&engine, coins);
        engine.fail_stop.latch(StoreError::engine(ENGINE, ErrorKind::Io, "another write failed"));
        let refused = written.recv_timeout(Duration::from_secs(10));
        let refused = refused.expect("the stalled write ends").unwrap_err();
        assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly), "{refused}");
        drop(pin);
        assert_eq!(engine.get(coins, b"k").unwrap(), None);
    }

    /// A write that leaves the write buffer manager past its flush point starts the flush itself
    /// (CH2c D42), so once it has run the next write is not refused: RocksDB alone would start
    /// it only at that next write, refusing it until the flush ends. The control, the same write
    /// below a larger manager's flush point, starts none, and neither does a disabled manager
    /// (size 0).
    #[test]
    fn a_write_past_the_flush_point_starts_the_flush() {
        for (wbm, flushed) in [(CHARGED_WBM, 0), (1 << 20, 1), (0, 0)] {
            let dir = tempfile::tempdir().unwrap();
            let engine = open_charged(dir.path(), 24 << 20, wbm);
            let coins = SCHEMA.cf("coins").unwrap();
            write_values(&engine, 20_000);
            engine.wait_idle().unwrap();
            let l0 = engine
                .db
                .property_int_value_cf(engine.handle(coins), "rocksdb.num-files-at-level0");
            assert_eq!(l0.unwrap(), Some(flushed), "manager of {wbm} bytes");
            let mut b = engine.batch();
            b.put(coins, b"k", b"v");
            assert_eq!(engine.write_once(&b.batch, false), Ok(()), "manager of {wbm} bytes");
        }
    }

    /// Writes `k` = `v` into `cf` on a thread of its own, once the write buffer manager stalls
    /// writers, and returns once RocksDB has refused it as a stall at least once.
    fn stalled_write(engine: &Arc<RocksEngine>, cf: Cf) -> std::sync::mpsc::Receiver<Result<()>> {
        let stops = || {
            // "Write Stall (count): …, write-buffer-manager-limit-stops: N, …"
            let stats = engine.db.property_value("rocksdb.db-write-stall-stats").unwrap().unwrap();
            let key = "write-buffer-manager-limit-stops: ";
            let at = stats.find(key).unwrap_or_else(|| panic!("{stats}")) + key.len();
            let n = stats[at..].split(|c: char| !c.is_ascii_digit()).next().unwrap();
            n.parse::<u64>().unwrap()
        };
        let before = stops();
        let (done, written) = std::sync::mpsc::channel();
        std::thread::spawn({
            let engine = engine.clone();
            move || {
                let mut b = engine.batch();
                b.put(cf, b"k", b"v");
                done.send(engine.write(b, Commit::Buffered)).unwrap();
            }
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while stops() == before {
            assert!(Instant::now() < deadline, "the write never met the stall");
            std::thread::sleep(Duration::from_millis(5));
        }
        written
    }

    /// The column family whose compactions wait while the gate is closed ([`GATE_CLOSED`]).
    const GATED: &str = "gated";
    static GATE_CLOSED: AtomicBool = AtomicBool::new(false);

    /// Gives [`GATED`] a compaction filter that waits at the gate. Flushes do not run it.
    pub(super) fn gate_compactions(spec: &CfSpec, opts: &mut Options) {
        if spec.name == GATED {
            opts.set_compaction_filter("gate", |_, _, _| {
                while GATE_CLOSED.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                rocksdb::compaction_filter::Decision::Keep
            });
        }
    }

    /// Closes the gate until dropped.
    struct ClosedGate;

    impl ClosedGate {
        fn new() -> Self {
            GATE_CLOSED.store(true, Ordering::SeqCst);
            Self
        }
    }

    impl Drop for ClosedGate {
        fn drop(&mut self) {
            GATE_CLOSED.store(false, Ordering::SeqCst);
        }
    }

    /// `flush_memtables` does not first wait until its flush would not stall writes (DEC-164).
    /// That wait outlasts compactions that cannot run, as on a full disk, a soft background
    /// error. Here the compactions wait at a closed gate, so level 0 grows by one file per flush
    /// (each holds both ends of the key range, so none is moved down whole). At 35 files the next
    /// flush's file reaches the stop trigger, and RocksDB's default flush would wait for ever.
    #[test]
    fn a_flush_does_not_wait_for_compactions() {
        static GATED_SCHEMA: DbSchema = DbSchema {
            name: "rocks-gated",
            version: 1,
            cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new(GATED, CfProfile::Ordered)],
        };
        let dir = tempfile::tempdir().unwrap();
        // One compaction at a time: each one held at the gate holds a thread of the process's
        // shared pool, which other tests' databases use too.
        let config = RocksConfig {
            max_background_jobs: 2,
            ..RocksConfig::default()
        };
        let engine = Arc::new(RocksEngine::open(dir.path(), &GATED_SCHEMA, &config).unwrap().0);
        let gated = GATED_SCHEMA.cf(GATED).unwrap();
        let l0 = || {
            let prop = "rocksdb.num-files-at-level0";
            engine.db.property_int_value_cf(engine.handle(gated), prop).unwrap().unwrap()
        };
        let gate = ClosedGate::new();
        for i in 0..L0_STOP_WRITES {
            let mut b = engine.batch();
            b.put(gated, b"a", &i.to_be_bytes());
            b.put(gated, b"z", &i.to_be_bytes());
            engine.write(b, Commit::Buffered).unwrap();
            let (done, flushed) = std::sync::mpsc::channel();
            std::thread::spawn({
                let engine = engine.clone();
                move || done.send(engine.flush_memtables()).unwrap()
            });
            let flushed = flushed.recv_timeout(Duration::from_secs(30));
            flushed.unwrap_or_else(|_| panic!("flush {i} waits at {} L0 files", l0())).unwrap();
        }
        assert_eq!(l0(), L0_STOP_WRITES as u64, "every flushed file is still in level 0");
        drop(gate);
    }

    /// A quiet database compacts nothing by itself, runs no statistics timer and logs only warnings
    /// and errors; [`RocksEngine::wait_idle`] returns once no flush or compaction is left. Each flush
    /// adds a level-0 file holding both ends of the key range, so five reach the compaction
    /// trigger (4) of a database that is not quiet.
    #[test]
    fn a_quiet_database_does_not_compact() {
        for quiet in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let config = RocksConfig {
                quiet,
                ..RocksConfig::default()
            };
            let (engine, _) = RocksEngine::open(dir.path(), &SCHEMA, &config).unwrap();
            let meta = SCHEMA.cf("meta").unwrap();
            for i in 0..5u8 {
                let mut b = engine.batch();
                b.put(meta, b"a", &[i]);
                b.put(meta, b"z", &[i]);
                engine.write(b, Commit::Buffered).unwrap();
                engine.flush_memtables().unwrap();
            }
            engine.wait_idle().unwrap();
            let prop = |name: &str| {
                engine.db.property_int_value_cf(engine.handle(meta), name).unwrap().unwrap()
            };
            assert_eq!(prop("rocksdb.num-running-compactions"), 0);
            // A quiet database's level 0 needs a compaction that never runs.
            assert_eq!(prop("rocksdb.compaction-pending"), u64::from(quiet));
            let l0 = prop("rocksdb.num-files-at-level0");
            assert_eq!(l0 >= 4, quiet, "quiet {quiet}: {l0} level-0 files");
            let options: String = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.file_name().unwrap().to_string_lossy().starts_with("OPTIONS-"))
                .map(|path| std::fs::read_to_string(path).unwrap())
                .collect();
            let quiet_options = [
                "disable_auto_compactions=true",
                "stats_dump_period_sec=0",
                "stats_persist_period_sec=0",
                "WARN_LEVEL",
            ];
            for set in quiet_options {
                assert_eq!(options.contains(set), quiet, "quiet {quiet}: {set}");
            }
        }
    }

    /// DEC-164's loop retries a stall until the write goes through, and ends at a background
    /// error. A fatal one stops RocksDB's writes, so the retried write reports it itself
    /// (`fail_stop`'s `a_stalled_write_fails_when_its_flush_fails`); a soft one, such as a
    /// compaction out of space, does not, and only the check sees it.
    #[test]
    fn a_stall_is_retried_until_a_background_error() {
        let mut writes = 0;
        retry_stalled(
            || {
                writes += 1;
                if writes < 3 {
                    Err(WRITE_STALL.into())
                } else {
                    Ok(())
                }
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(writes, 3);

        let (mut writes, mut checks) = (0, 0);
        let err = retry_stalled(
            || {
                writes += 1;
                assert!(writes < 100, "the stall never ended");
                Err(WRITE_STALL.into())
            },
            || {
                checks += 1;
                if checks < 3 {
                    Ok(())
                } else {
                    Err(StoreError::engine(ENGINE, ErrorKind::NoSpace, "a background error"))
                }
            },
        )
        .unwrap_err();
        assert_eq!((err.kind(), writes, checks), (Some(ErrorKind::NoSpace), 3, 3));

        let err = retry_stalled(|| Err("IO error: a write error".into()), || panic!("not a stall"));
        assert_eq!(err.unwrap_err().kind(), Some(ErrorKind::Io));
    }

    #[test]
    fn delete_range() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, _) =
            RocksEngine::open(dir.path(), &dash_kv::model::MODEL_SCHEMA, &RocksConfig::default())
                .unwrap();
        dash_kv::model::check_delete_range(&engine);
    }

    #[test]
    fn model_check() {
        use dash_kv::model::{self, Hooks, MODEL_SCHEMA, Op};
        let mut reopens = 0;
        for seed in 0..6u64 {
            let dir = tempfile::tempdir().unwrap();
            let open =
                || RocksEngine::open(dir.path(), &MODEL_SCHEMA, &RocksConfig::default()).unwrap();
            let (engine, state) = open();
            assert_eq!(state, SchemaState::Created);
            let reopen = |engine: RocksEngine| {
                drop(engine);
                let (engine, state) = open();
                assert_eq!(state, SchemaState::Current);
                engine
            };
            let ops = model::decode_ops(&model::pseudo_random_bytes(seed, 4096));
            reopens += ops.iter().filter(|op| matches!(op, Op::Reopen)).count();
            let hooks = Hooks {
                reopen: Some(&reopen),
                ..Hooks::default()
            };
            model::check_with(engine, &ops, hooks).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        }
        assert!(reopens > 6, "the seeds exercise reopening ({reopens})");
    }
}
