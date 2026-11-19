//! The three engines behind one harness interface (protocol §3.2–§4).
//!
//! RocksDB goes through `dashd-store` (the production engine). LevelDB (Google LevelDB 1.22, the
//! version Core's fork is based on) and MDBX (libmdbx 0.14) are thin adapters. Each engine flushes
//! in its natural atomic unit and ends it durable: the LSMs use Core's head-blocks sub-batch
//! protocol (RocksDB with A1 §3.5.4's commit kinds), MDBX one write transaction.

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;

use dashd_store::{
    CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead, RocksConfig, RocksEngine,
};
use serde_json::{Value, json};

/// Core's `-dbbatchsize` default.
pub const BATCH_BYTES: usize = 16 << 20;
const TIP: &[u8] = b"tip";
const HEAD_BLOCKS: &[u8] = b"head_blocks";

pub type Res<T> = Result<T, String>;

/// An engine key held inline: `txid ‖ VARINT(vout)` is at most 37 bytes.
#[derive(Clone, Copy)]
pub struct KeyBuf {
    len: u8,
    bytes: [u8; 37],
}

impl KeyBuf {
    pub fn new(key: &[u8]) -> Self {
        let mut bytes = [0; 37];
        bytes[..key.len()].copy_from_slice(key);
        Self {
            len: key.len() as u8,
            bytes,
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

/// One flush or commit operation: a put (`Some`) or a delete (`None`). Operations are streamed,
/// so the harness never materializes a second copy of a large flush.
pub type Op<'a> = (KeyBuf, Option<&'a [u8]>);
pub type Ops<'a, 'b> = &'b mut dyn Iterator<Item = Op<'a>>;

/// The engine memory budget `E` (protocol §3.1, §4).
#[derive(Debug, Clone, Copy)]
pub struct EngineBudget {
    /// RocksDB shared block cache.
    pub block_cache: usize,
    /// The whole engine budget `E`: LevelDB's `nCacheSize` (block cache E/2, write buffer E/4,
    /// Core's `GetOptions`) and MDBX's dirty-page limit per write transaction.
    pub total: usize,
}

pub trait Store {
    /// A point read on a cache miss.
    fn get(&mut self, key: &[u8], out: &mut Vec<u8>) -> Res<bool>;
    /// One cache flush (`ops` sorted by key), ending with the durability point.
    fn flush(&mut self, ops: Ops, tip: u32, old_tip: u32) -> Res<()>;
    /// Waits until background work (compaction) is done.
    fn quiesce(&mut self) -> Res<()>;
    /// A full manual compaction (for `db_bytes_compacted`).
    fn compact(&mut self) -> Res<()>;
    /// Engine-specific numbers for the results.
    fn extra(&mut self) -> Value;
    /// D1: one atomic commit, buffered or durable.
    fn commit(&mut self, ops: Ops, durable: bool) -> Res<()>;
    /// D2: the engine's barriers.
    fn barriers(&self) -> &'static [&'static str];
    fn barrier(&mut self, name: &str) -> Res<()>;
    /// D3: builds one batch without committing, calls `pending` while it is held, then commits it
    /// buffered.
    fn pending_batch(&mut self, ops: Ops, pending: &mut dyn FnMut()) -> Res<()>;
}

pub fn open(engine: &str, dir: &Path, budget: EngineBudget) -> Res<Box<dyn Store>> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    Ok(match engine {
        // `rocksdb` is dashd-store's default configuration; variants add `-nopf` (whole index and
        // filter blocks, A1 rev1's configuration), `-mmap` (mmap reads), `-direct` (direct I/O).
        // The runner sizes the block cache (`-bcN`: N MiB).
        name if name == "rocksdb" || name.starts_with("rocksdb-") => {
            let has = |flag: &str| name.split('-').any(|part| part == flag);
            Box::new(Rocks::open(
                dir,
                budget,
                RocksConfig {
                    partitioned_filters: !has("nopf"),
                    mmap_reads: has("mmap"),
                    direct_io: has("direct"),
                    ..RocksConfig::default()
                },
            )?)
        }
        "leveldb" => Box::new(Level::open(dir, budget)?),
        "mdbx" => Box::new(Mdbx::open(dir, budget)?),
        other => return Err(format!("unknown engine {other}")),
    })
}

/// Where a sub-batch sits in a flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    /// The first of several: replaces the tip marker by `head_blocks`.
    First,
    Middle,
    /// The last of several: restores the tip marker.
    Last,
    /// The flush fits in one batch (the marker changes cancel out inside it).
    Only,
}

/// A sub-batch of an LSM engine: coin operations, or meta operations.
pub(crate) trait SubBatch {
    fn op(&mut self, meta: bool, key: &[u8], value: Option<&[u8]>);
    fn size(&self) -> usize;
}

/// Core's partial-write protocol around a large flush (`txdb.cpp` `CCoinsViewDB::BatchWrite`), as
/// A1 §3.5.4 adopts it: the first sub-batch replaces the tip marker by `head_blocks = [new, old]`,
/// sub-batches hold at most `-dbbatchsize`, the last one restores the marker.
pub(crate) fn head_blocks_flush<B: SubBatch>(
    ops: Ops,
    tip: u32,
    old_tip: u32,
    mut new_batch: impl FnMut() -> B,
    mut write: impl FnMut(B, Part) -> Res<()>,
) -> Res<()> {
    let mut heads = tip.to_le_bytes().to_vec();
    heads.extend(old_tip.to_le_bytes());
    let mut batch = new_batch();
    batch.op(true, TIP, None);
    batch.op(true, HEAD_BLOCKS, Some(&heads));
    let mut first = true;
    for (key, value) in ops {
        batch.op(false, key.as_slice(), value);
        if batch.size() > BATCH_BYTES {
            let part = if first {
                Part::First
            } else {
                Part::Middle
            };
            write(std::mem::replace(&mut batch, new_batch()), part)?;
            first = false;
        }
    }
    batch.op(true, HEAD_BLOCKS, None);
    batch.op(true, TIP, Some(&tip.to_le_bytes()));
    write(
        batch,
        if first {
            Part::Only
        } else {
            Part::Last
        },
    )
}

// ---- RocksDB (dashd-store) ----

static ROCKS_SCHEMA: DbSchema = DbSchema {
    name: "bakeoff-chainstate",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("coins", CfProfile::PointLookup)],
};
const COINS: dashd_store::Cf = dashd_store::Cf::new(1);

struct Rocks {
    db: RocksEngine,
}

impl SubBatch for dashd_store::RocksBatch {
    fn op(&mut self, meta: bool, key: &[u8], value: Option<&[u8]>) {
        let cf = if meta {
            DbSchema::META
        } else {
            COINS
        };
        match value {
            Some(v) => self.put(cf, key, v),
            None => self.delete(cf, key),
        }
    }

    fn size(&self) -> usize {
        self.size_bytes()
    }
}

/// The lines of a RocksDB stats dump that report write stalls.
fn stall_lines(stats: Option<String>) -> Vec<String> {
    let stats = stats.unwrap_or_default();
    stats.lines().filter(|l| l.to_ascii_lowercase().contains("stall")).map(str::to_owned).collect()
}

fn store_err(e: dashd_store::StoreError) -> String {
    match e.kind() {
        Some(kind) => format!("[{kind:?}] {e}"),
        None => e.to_string(),
    }
}

impl Rocks {
    fn open(dir: &Path, budget: EngineBudget, base: RocksConfig) -> Res<Self> {
        let config = RocksConfig {
            block_cache_bytes: budget.block_cache,
            max_background_jobs: 2,
            ..base
        };
        let (db, _) = RocksEngine::open(dir, &ROCKS_SCHEMA, &config).map_err(store_err)?;
        Ok(Self {
            db,
        })
    }

    fn batch_of(&self, ops: Ops) -> dashd_store::RocksBatch {
        let mut batch = self.db.batch();
        for (k, v) in ops {
            batch.op(false, k.as_slice(), v);
        }
        batch
    }
}

impl Store for Rocks {
    fn get(&mut self, key: &[u8], out: &mut Vec<u8>) -> Res<bool> {
        match self.db.get(COINS, key).map_err(store_err)? {
            Some(v) => {
                *out = v;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// A1 §3.5.4's commit kinds: the marker batch and the last batch are Durable, the coin
    /// sub-batches between them Buffered.
    fn flush(&mut self, ops: Ops, tip: u32, old_tip: u32) -> Res<()> {
        let db = &self.db;
        head_blocks_flush(
            ops,
            tip,
            old_tip,
            || db.batch(),
            |b, part| {
                let commit = if part == Part::Middle {
                    Commit::Buffered
                } else {
                    Commit::Durable
                };
                db.write(b, commit).map_err(store_err)
            },
        )
    }

    fn quiesce(&mut self) -> Res<()> {
        use std::time::Duration;
        // The longest quiesce of any bake-off run took minutes; an hour means a hang.
        self.db
            .wait_for_compactions(Duration::from_millis(100), Duration::from_secs(3600))
            .map_err(store_err)
    }

    fn compact(&mut self) -> Res<()> {
        let raw = self.db.raw();
        let cf = raw.cf_handle("coins").expect("coins CF");
        raw.compact_range_cf(cf, None::<&[u8]>, None::<&[u8]>);
        self.quiesce()
    }

    fn extra(&mut self) -> Value {
        let raw = self.db.raw();
        let cf = raw.cf_handle("coins").expect("coins CF");
        let prop = |p: &str| raw.property_int_value_cf(cf, p).ok().flatten();
        json!({
            "estimate_live_data_size": prop("rocksdb.estimate-live-data-size"),
            "total_sst_files_size": prop("rocksdb.total-sst-files-size"),
            "block_cache_usage": prop("rocksdb.block-cache-usage"),
            "block_cache_pinned_usage": prop("rocksdb.block-cache-pinned-usage"),
            "estimate_table_readers_mem": prop("rocksdb.estimate-table-readers-mem"),
            "cur_size_all_mem_tables": prop("rocksdb.cur-size-all-mem-tables"),
            "num_files_at_level0": raw.property_value_cf(cf, "rocksdb.num-files-at-level0").ok().flatten(),
            // Write stalls (ST1 review r3 m8): the cumulative stall time of the whole run and the
            // per-cause stall counts of `coins`, as RocksDB reports them.
            "stall_lines": ([
                stall_lines(raw.property_value("rocksdb.dbstats").ok().flatten()),
                stall_lines(raw.property_value_cf(cf, "rocksdb.cfstats").ok().flatten()),
            ]
            .concat()),
            "actual_delayed_write_rate": raw.property_int_value("rocksdb.actual-delayed-write-rate").ok().flatten(),
            "is_write_stopped": raw.property_int_value("rocksdb.is-write-stopped").ok().flatten(),
        })
    }

    fn commit(&mut self, ops: Ops, durable: bool) -> Res<()> {
        let commit = if durable {
            Commit::Durable
        } else {
            Commit::Buffered
        };
        self.db.write(self.batch_of(ops), commit).map_err(store_err)
    }

    fn barriers(&self) -> &'static [&'static str] {
        &["sync_wal", "flush_memtables"]
    }

    fn barrier(&mut self, name: &str) -> Res<()> {
        match name {
            "sync_wal" => self.db.sync_wal().map_err(store_err),
            "flush_memtables" => self.db.flush_memtables().map_err(store_err),
            other => Err(format!("unknown barrier {other}")),
        }
    }

    fn pending_batch(&mut self, ops: Ops, pending: &mut dyn FnMut()) -> Res<()> {
        let batch = self.batch_of(ops);
        pending();
        self.db.write(batch, Commit::Buffered).map_err(store_err)
    }
}

// ---- LevelDB (Google LevelDB 1.22 through its C API) ----
//
// SAFETY (whole section): every pointer passed to the C API is either a live LevelDB object owned
// by `Level` (created in `open`, destroyed in `drop`) or a Rust slice valid for the call. Error
// strings and returned values are malloc'ed by LevelDB and released with `leveldb_free`.

use leveldb_sys as ldb;

const COIN_PREFIX: u8 = b'C';
const META_PREFIX: u8 = b'M';

pub(crate) struct Level {
    db: *mut ldb::leveldb_t,
    options: *mut ldb::leveldb_options_t,
    cache: *mut ldb::leveldb_cache_t,
    filter: *mut ldb::leveldb_filterpolicy_t,
    read: *mut ldb::leveldb_readoptions_t,
    write_nosync: *mut ldb::leveldb_writeoptions_t,
    write_sync: *mut ldb::leveldb_writeoptions_t,
    key: Vec<u8>,
}

fn take_err(err: *mut c_char) -> Res<()> {
    if err.is_null() {
        return Ok(());
    }
    let msg = unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned();
    unsafe { ldb::leveldb_free(err.cast()) };
    Err(msg)
}

pub(crate) struct LevelBatch {
    raw: *mut ldb::leveldb_writebatch_t,
    /// Core's `CDBBatch::SizeEstimate`-style running size.
    size: usize,
}

impl LevelBatch {
    pub(crate) fn new() -> Self {
        Self {
            raw: unsafe { ldb::leveldb_writebatch_create() },
            size: 12,
        }
    }
}

impl SubBatch for LevelBatch {
    /// Keys carry a one-byte prefix, as Core's do (LevelDB has no column families).
    fn op(&mut self, meta: bool, key: &[u8], value: Option<&[u8]>) {
        let mut k = Vec::with_capacity(key.len() + 1);
        k.push(if meta {
            META_PREFIX
        } else {
            COIN_PREFIX
        });
        k.extend_from_slice(key);
        unsafe {
            match value {
                Some(v) => ldb::leveldb_writebatch_put(
                    self.raw,
                    k.as_ptr().cast(),
                    k.len(),
                    v.as_ptr().cast(),
                    v.len(),
                ),
                None => ldb::leveldb_writebatch_delete(self.raw, k.as_ptr().cast(), k.len()),
            }
        }
        self.size += 3 + k.len() + value.map_or(0, |v| v.len() + 2);
    }

    fn size(&self) -> usize {
        self.size
    }
}

impl Drop for LevelBatch {
    fn drop(&mut self) {
        unsafe { ldb::leveldb_writebatch_destroy(self.raw) }
    }
}

impl Level {
    pub(crate) fn open(dir: &Path, budget: EngineBudget) -> Res<Self> {
        let path = CString::new(dir.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        unsafe {
            // Core: dbwrapper.cpp GetOptions, CDBWrapper::CDBWrapper
            let options = ldb::leveldb_options_create();
            let cache = ldb::leveldb_cache_create_lru(budget.total / 2);
            let filter = ldb::leveldb_filterpolicy_create_bloom(10);
            ldb::leveldb_options_set_cache(options, cache);
            ldb::leveldb_options_set_write_buffer_size(options, budget.total / 4);
            ldb::leveldb_options_set_filter_policy(options, filter);
            ldb::leveldb_options_set_compression(options, ldb::Compression::No);
            ldb::leveldb_options_set_paranoid_checks(options, 1);
            ldb::leveldb_options_set_create_if_missing(options, 1);
            let mut err = ptr::null_mut();
            let db = ldb::leveldb_open(options, path.as_ptr(), &mut err);
            if let Err(e) = take_err(err) {
                ldb::leveldb_options_destroy(options);
                ldb::leveldb_cache_destroy(cache);
                ldb::leveldb_filterpolicy_destroy(filter);
                return Err(e);
            }
            let read = ldb::leveldb_readoptions_create();
            ldb::leveldb_readoptions_set_verify_checksums(read, 1);
            let write_nosync = ldb::leveldb_writeoptions_create();
            let write_sync = ldb::leveldb_writeoptions_create();
            ldb::leveldb_writeoptions_set_sync(write_sync, 1);
            Ok(Self {
                db,
                options,
                cache,
                filter,
                read,
                write_nosync,
                write_sync,
                key: Vec::new(),
            })
        }
    }

    pub(crate) fn write(&self, batch: LevelBatch, sync: bool) -> Res<()> {
        let opts = if sync {
            self.write_sync
        } else {
            self.write_nosync
        };
        let mut err = ptr::null_mut();
        unsafe { ldb::leveldb_write(self.db, opts, batch.raw, &mut err) };
        take_err(err)
    }

    /// A point read of `key` as stored (prefix included).
    fn get_raw(&self, key: &[u8], out: &mut Vec<u8>) -> Res<bool> {
        let mut len = 0usize;
        let mut err = ptr::null_mut();
        let v = unsafe {
            ldb::leveldb_get(self.db, self.read, key.as_ptr().cast(), key.len(), &mut len, &mut err)
        };
        take_err(err)?;
        if v.is_null() {
            return Ok(false);
        }
        out.clear();
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(v.cast::<u8>(), len) });
        unsafe { ldb::leveldb_free(v.cast()) };
        Ok(true)
    }

    /// A coin's point read (`CCoinsViewDB::GetCoin`), by its engine key.
    pub(crate) fn get_coin(&self, key: &[u8], out: &mut Vec<u8>) -> Res<bool> {
        let mut k = Vec::with_capacity(key.len() + 1);
        k.push(COIN_PREFIX);
        k.extend_from_slice(key);
        self.get_raw(&k, out)
    }

    pub(crate) fn property(&self, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        let v = unsafe { ldb::leveldb_property_value(self.db, name.as_ptr()) };
        if v.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(v) }.to_string_lossy().into_owned();
        unsafe { ldb::leveldb_free(v.cast()) };
        Some(s)
    }

    fn batch_of(ops: Ops) -> LevelBatch {
        let mut batch = LevelBatch::new();
        for (k, v) in ops {
            batch.op(false, k.as_slice(), v);
        }
        batch
    }
}

impl Drop for Level {
    fn drop(&mut self) {
        unsafe {
            ldb::leveldb_close(self.db);
            ldb::leveldb_options_destroy(self.options);
            ldb::leveldb_cache_destroy(self.cache);
            ldb::leveldb_filterpolicy_destroy(self.filter);
            ldb::leveldb_readoptions_destroy(self.read);
            ldb::leveldb_writeoptions_destroy(self.write_nosync);
            ldb::leveldb_writeoptions_destroy(self.write_sync);
        }
    }
}

impl Store for Level {
    fn get(&mut self, key: &[u8], out: &mut Vec<u8>) -> Res<bool> {
        self.key.clear();
        self.key.push(COIN_PREFIX);
        self.key.extend_from_slice(key);
        self.get_raw(&self.key, out)
    }

    fn flush(&mut self, ops: Ops, tip: u32, old_tip: u32) -> Res<()> {
        // Core writes the coins without sync; the last sub-batch is synced here so every engine
        // ends a flush durable. LevelDB syncs only its current log file.
        head_blocks_flush(ops, tip, old_tip, LevelBatch::new, |b, part| {
            self.write(b, matches!(part, Part::Last | Part::Only))
        })
    }

    fn quiesce(&mut self) -> Res<()> {
        // LevelDB exposes no pending-compaction counter: wait until its compaction statistics
        // stop changing for 2 s.
        let mut last = self.property("leveldb.stats");
        let mut stable_since = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let now = self.property("leveldb.stats");
            if now != last {
                last = now;
                stable_since = std::time::Instant::now();
            } else if stable_since.elapsed().as_secs_f64() >= 2.0 {
                return Ok(());
            }
        }
    }

    fn compact(&mut self) -> Res<()> {
        unsafe { ldb::leveldb_compact_range(self.db, ptr::null(), 0, ptr::null(), 0) };
        Ok(())
    }

    fn extra(&mut self) -> Value {
        json!({
            "stats": self.property("leveldb.stats"),
            "approximate_memory_usage": self.property("leveldb.approximate-memory-usage"),
        })
    }

    fn commit(&mut self, ops: Ops, durable: bool) -> Res<()> {
        self.write(Self::batch_of(ops), durable)
    }

    fn barriers(&self) -> &'static [&'static str] {
        &["sync_write"]
    }

    fn barrier(&mut self, name: &str) -> Res<()> {
        match name {
            "sync_write" => {
                let mut batch = LevelBatch::new();
                batch.op(true, b"barrier", Some(b"1"));
                self.write(batch, true)
            }
            other => Err(format!("unknown barrier {other}")),
        }
    }

    fn pending_batch(&mut self, ops: Ops, pending: &mut dyn FnMut()) -> Res<()> {
        let batch = Self::batch_of(ops);
        pending();
        self.write(batch, false)
    }
}

// ---- MDBX (libmdbx 0.14 through the `libmdbx` crate) ----

use libmdbx::{
    Database, DatabaseOptions, Mode, NoWriteMap, RO, ReadWriteOptions, SyncMode, Table, TableFlags,
    Transaction, WriteFlags,
};

struct Mdbx {
    // Fields drop in declaration order: the read transaction and table handles before the
    // environment they borrow.
    /// Renewed after every write, so reads always see the latest commit and never pin old pages
    /// across a write transaction.
    reader: Option<Transaction<'static, RO, NoWriteMap>>,
    coins: Table<'static>,
    meta: Table<'static>,
    db: &'static Database<NoWriteMap>,
    _env: Box<Database<NoWriteMap>>,
}

fn mdbx_err(e: libmdbx::Error) -> String {
    format!("{e:?}: {e}")
}

impl Mdbx {
    fn open(dir: &Path, budget: EngineBudget) -> Res<Self> {
        let options = DatabaseOptions {
            max_tables: Some(4),
            // Dirty pages of a write transaction live in anonymous memory until commit. Bound them
            // by the engine budget; beyond it MDBX spills pages to the file.
            txn_dp_limit: Some((budget.total / 4096) as u64),
            no_rdahead: true,
            mode: Mode::ReadWrite(ReadWriteOptions {
                sync_mode: SyncMode::SafeNoSync,
                max_size: Some(64 << 30),
                growth_step: Some(16 << 20),
                ..ReadWriteOptions::default()
            }),
            ..DatabaseOptions::default()
        };
        let env = Box::new(Database::open_with_options(dir, options).map_err(mdbx_err)?);
        // SAFETY: the box's contents never move and `_env` outlives every borrower (field order
        // above), so the reference is valid for the struct's lifetime.
        let db: &'static Database<NoWriteMap> = unsafe { &*(env.as_ref() as *const _) };
        let txn = db.begin_rw_txn().map_err(mdbx_err)?;
        let coins = txn.create_table(Some("coins"), TableFlags::empty()).map_err(mdbx_err)?;
        let meta = txn.create_table(Some("meta"), TableFlags::empty()).map_err(mdbx_err)?;
        txn.prime_for_permaopen(coins);
        txn.prime_for_permaopen(meta);
        let (_, tables) = txn.commit_and_rebind_open_dbs().map_err(mdbx_err)?;
        let [coins, meta]: [Table<'static>; 2] =
            tables.try_into().map_err(|_| "expected two table handles".to_string())?;
        // `primed_dbis` is a set: restore creation order by DBI.
        let (coins, meta) = if coins.dbi() < meta.dbi() {
            (coins, meta)
        } else {
            (meta, coins)
        };
        Ok(Self {
            reader: None,
            coins,
            meta,
            db,
            _env: env,
        })
    }

    fn write_txn(
        &mut self,
        f: impl FnOnce(
            &Transaction<'static, libmdbx::RW, NoWriteMap>,
            &Table<'static>,
            &Table<'static>,
        ) -> Res<()>,
        sync: bool,
    ) -> Res<()> {
        self.reader = None;
        let txn = self.db.begin_rw_txn().map_err(mdbx_err)?;
        f(&txn, &self.coins, &self.meta)?;
        txn.commit().map_err(mdbx_err)?;
        if sync {
            self.db.sync(true).map_err(mdbx_err)?;
        }
        Ok(())
    }

    fn put_ops(
        txn: &Transaction<'static, libmdbx::RW, NoWriteMap>,
        coins: &Table<'static>,
        ops: Ops,
    ) -> Res<()> {
        for (k, v) in ops {
            match v {
                Some(v) => {
                    txn.put(coins, k.as_slice(), v, WriteFlags::empty()).map_err(mdbx_err)?
                }
                None => {
                    txn.del(coins, k.as_slice(), None).map_err(mdbx_err)?;
                }
            }
        }
        Ok(())
    }
}

impl Store for Mdbx {
    fn get(&mut self, key: &[u8], out: &mut Vec<u8>) -> Res<bool> {
        if self.reader.is_none() {
            self.reader = Some(self.db.begin_ro_txn().map_err(mdbx_err)?);
        }
        let txn = self.reader.as_ref().expect("just set");
        match txn.get::<std::borrow::Cow<[u8]>>(&self.coins, key).map_err(mdbx_err)? {
            Some(v) => {
                out.clear();
                out.extend_from_slice(&v);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn flush(&mut self, ops: Ops, tip: u32, _old_tip: u32) -> Res<()> {
        // One transaction is atomic, so no head-blocks marker is needed.
        self.write_txn(
            |txn, coins, meta| {
                Self::put_ops(txn, coins, ops)?;
                txn.put(meta, TIP, tip.to_le_bytes(), WriteFlags::empty()).map_err(mdbx_err)
            },
            true,
        )
    }

    fn quiesce(&mut self) -> Res<()> {
        Ok(())
    }

    fn compact(&mut self) -> Res<()> {
        Ok(())
    }

    fn extra(&mut self) -> Value {
        self.reader = None;
        let info = self.db.info().ok();
        let stat = self.db.stat().ok();
        let page = stat.as_ref().map_or(4096, |s| s.page_size() as u64);
        json!({
            "page_size": page,
            "used_bytes": info.as_ref().map(|i| (i.last_pgno() as u64 + 1) * page),
            // Pages in use by the tree: allocated pages minus the free list.
            "live_bytes": info.as_ref().zip(self.db.freelist().ok())
                .map(|(i, free)| (i.last_pgno() as u64 + 1 - free as u64) * page),
            "map_size": info.as_ref().map(|i| i.map_size()),
            "freelist_pages": self.db.freelist().ok(),
            "entries": stat.as_ref().map(|s| s.entries()),
            "depth": stat.as_ref().map(|s| s.depth()),
        })
    }

    fn commit(&mut self, ops: Ops, durable: bool) -> Res<()> {
        self.write_txn(|txn, coins, _| Self::put_ops(txn, coins, ops), durable)
    }

    fn barriers(&self) -> &'static [&'static str] {
        &["env_sync"]
    }

    fn barrier(&mut self, name: &str) -> Res<()> {
        match name {
            "env_sync" => self.db.sync(true).map(|_| ()).map_err(mdbx_err),
            other => Err(format!("unknown barrier {other}")),
        }
    }

    fn pending_batch(&mut self, ops: Ops, pending: &mut dyn FnMut()) -> Res<()> {
        self.write_txn(
            |txn, coins, _| {
                Self::put_ops(txn, coins, ops)?;
                pending();
                Ok(())
            },
            false,
        )
    }
}
