//! The SPV store's storage engine: [`KvEngine`] on SQLite (SB0 §9.4).
//!
//! A database is one file, `<dir>/kv.sqlite`, in WAL mode: the directory is the database, as with
//! RocksDB, so opening it with another schema fails. Each column family is a `WITHOUT ROWID`
//! table `cf_<name> (k BLOB PRIMARY KEY, v BLOB NOT NULL)`; SQLite orders BLOBs by `memcmp`, then
//! length, which is RocksDB's bytewise order. A private table `_dash_kv` holds the barrier
//! counter, so the visible column families match the other engines'.
//!
//! - **Writes.** One write connection, under a mutex. A batch is one `BEGIN IMMEDIATE … COMMIT`,
//!   at `synchronous=NORMAL` ([`Commit::Buffered`]: it survives a process kill, a power loss may
//!   lose a suffix) or `FULL` ([`Commit::Durable`]: it and every earlier commit survive, as the
//!   WAL is sequential). A transaction that writes no page writes no frame and so syncs nothing:
//!   a durable one with a buffered commit pending also bumps the barrier counter, and
//!   [`sync_wal`](KvEngine::sync_wal) is that commit alone.
//! - **One engine per directory.** The engine holds an exclusive lock on `<dir>/LOCK` while it is
//!   open, as RocksDB does, so a second open of the directory fails.
//! - **Reads.** A capped pool of read connections ([`SqliteConfig::readers`]): a live read takes
//!   one for the call, a scan for its life, a [`SqliteSnapshot`] until dropped. A snapshot pins
//!   its view by reading the schema row right after `BEGIN DEFERRED`: SQLite fixes a deferred
//!   transaction's view at its first read, not at `BEGIN` (DEC-104 M6).
//! - **Fail-stop.** A failed write or barrier latches the engine: every later one is refused
//!   until reopen, and so is every write after one that panicked. A snapshot that cannot be
//!   pinned latches the engine too, and every read of it fails; a client's [`LatchedEngine`]
//!   trips at that first read, so a poisoned snapshot dropped unread trips it at the next write.
//!   A failed live read latches nothing; the store's guard trips the process's latch (DEC-78).
//! - **Checkpoints.** A checkpoint copies WAL frames back only up to the oldest open read view,
//!   so a long-held snapshot or scan lets the WAL grow until it ends.
//! - **Threads.** SQLite starts none (`threads=0`, checkpoints inline), so the crate keeps SB0's
//!   L5. Blocking calls run on the host's [`BlockingIo`](dash_kv::BlockingIo). A snapshot or a
//!   live scan holds a pooled connection and stays on the thread that took it (it is not `Send`):
//!   the pool's guard against a thread waiting for its own connections (Q8) counts per thread.
//!
//! [`LatchedEngine`]: dash_kv::LatchedEngine

mod error;
mod pool;
mod scan;

use std::fs::{File, TryLockError};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dash_kv::fail_stop::FailStop;
use dash_kv::{
    Cf, Commit, DbSchema, ErrorKind, KvBatch, KvEngine, KvRead, Result, SCHEMA_KEY, SchemaState,
    StoreError, check_record, check_schema,
};
use rusqlite::{CachedStatement, Connection, OpenFlags, OptionalExtension};

use crate::error::{ENGINE, busy_recovery, engine_error};
use crate::pool::{Pool, Pooled, TakeError};
use crate::scan::{Scan, ScanConn};

/// The database file in its directory. It does not name the schema: a directory holds one
/// database, so another schema's open finds this one and fails.
pub const FILE: &str = "kv.sqlite";

/// The lock file in the database's directory: an engine holds it exclusively while open.
pub const LOCK: &str = "LOCK";

/// How many times a snapshot retries `SQLITE_BUSY_RECOVERY` before it gives up.
const BUSY_RECOVERY_RETRIES: u32 = 8;

/// The oldest SQLite library the engine opens a database with: 3.34.1, the floor of the bindings
/// libsqlite3-sys uses with a system library (`min_sqlite_version_3_34_1`, as on iOS). The engine
/// needs no more: its newest feature is UPSERT (3.24), besides WAL, `WITHOUT ROWID`, multi-row
/// `VALUES` and `query_only`. A bundled build carries a newer one. BL-303.
pub const MIN_SQLITE_VERSION: i32 = 3_034_001;

/// Refuses a SQLite library older than [`MIN_SQLITE_VERSION`] (`version` as
/// `sqlite3_libversion_number` gives it).
fn check_sqlite_version(version: i32) -> Result<()> {
    if version >= MIN_SQLITE_VERSION {
        return Ok(());
    }
    let dotted = |v: i32| format!("{}.{}.{}", v / 1_000_000, v / 1_000 % 1_000, v % 1_000);
    Err(StoreError::engine(
        ENGINE,
        ErrorKind::Other,
        format!(
            "SQLite {} is older than {}, the oldest this engine runs on",
            dotted(version),
            dotted(MIN_SQLITE_VERSION)
        ),
    ))
}

/// The tuning of a [`SqliteEngine`].
#[derive(Debug, Clone)]
pub struct SqliteConfig {
    /// Read connections at most (each with its own page cache): 1 on mobile, 2 elsewhere.
    pub readers: usize,
    /// How long a call waits for another connection's lock (on the caller's thread).
    pub busy_timeout: Duration,
    /// The page cache of each connection, in KiB.
    pub cache_kib: u32,
    /// The size the WAL file is truncated back to after a checkpoint, in bytes.
    pub journal_size_limit: u64,
    /// How long a read by a thread that already holds a pooled connection (a snapshot or a live
    /// scan, against [`KvEngine::snapshot`]'s rule) waits for another before it fails with
    /// [`ErrorKind::Other`] (SB10 Q8). A thread that holds none waits without a bound: other
    /// threads' long scans only delay it.
    pub read_wait: Duration,
    /// The SQLite VFS to open the database through, by its registered name; `None` (the
    /// default) is the platform's own.
    pub vfs: Option<String>,
}

impl Default for SqliteConfig {
    fn default() -> Self {
        Self {
            readers: if cfg!(any(target_os = "android", target_os = "ios")) {
                1
            } else {
                2
            },
            busy_timeout: Duration::from_secs(5),
            cache_kib: 4096,
            journal_size_limit: 32 << 20,
            read_wait: Duration::from_secs(30),
            vfs: None,
        }
    }
}

/// Rows a put statement inserts at most: [`apply`] writes a run of puts to one table
/// [`PUT_ROWS`] at a time, so the rows share one statement's cursor (SB13 D49).
const PUT_ROWS: usize = 128;

/// The SQL of one column family's table, prepared once per connection (statement cache).
pub(crate) struct CfSql {
    put: String,
    /// [`PUT_ROWS`] rows a statement.
    put_rows: String,
    delete: String,
    delete_range: String,
    get: String,
    scan_from: String,
    scan_from_until: String,
    scan_after: String,
    scan_after_until: String,
}

impl CfSql {
    fn new(table: &str) -> Self {
        let page = scan::PAGE;
        let scan = |lower: &str, upper: &str| {
            format!("SELECT k, v FROM {table} WHERE k {lower} ?1{upper} ORDER BY k LIMIT {page}")
        };
        Self {
            put: format!("INSERT OR REPLACE INTO {table} (k, v) VALUES (?1, ?2)"),
            put_rows: format!(
                "INSERT OR REPLACE INTO {table} (k, v) VALUES {}",
                vec!["(?, ?)"; PUT_ROWS].join(", ")
            ),
            delete: format!("DELETE FROM {table} WHERE k = ?1"),
            delete_range: format!("DELETE FROM {table} WHERE k >= ?1 AND k < ?2"),
            get: format!("SELECT v FROM {table} WHERE k = ?1"),
            scan_from: scan(">=", ""),
            scan_from_until: scan(">=", " AND k < ?2"),
            scan_after: scan(">", ""),
            scan_after_until: scan(">", " AND k < ?2"),
        }
    }
}

/// The tables of a schema's column families.
pub(crate) struct Tables {
    names: Vec<String>,
    cfs: Vec<CfSql>,
}

impl Tables {
    fn new(schema: &DbSchema) -> Self {
        let names: Vec<String> = schema
            .cfs
            .iter()
            .map(|spec| {
                assert!(
                    !spec.name.is_empty()
                        && spec.name.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_')),
                    "column family `{}` of `{}`: a table name takes [a-z0-9_]",
                    spec.name,
                    schema.name
                );
                format!("cf_{}", spec.name)
            })
            .collect();
        let cfs = names.iter().map(|t| CfSql::new(t)).collect();
        Self {
            names,
            cfs,
        }
    }
}

struct Writer {
    conn: Connection,
    /// The level set on `conn`; it cannot change inside a transaction, so it is set before
    /// `BEGIN`, and only when it differs.
    sync: Commit,
    /// A buffered commit is newer than the last durable commit.
    buffered: bool,
    /// A failure to inject into the next write or barrier.
    #[cfg(test)]
    inject: Option<ErrorKind>,
}

/// A [`KvEngine`] on one SQLite database file.
pub struct SqliteEngine {
    schema: &'static DbSchema,
    path: PathBuf,
    tables: Tables,
    writer: Mutex<Writer>,
    pool: Pool,
    fail_stop: FailStop,
    /// SQLite has the `dbstat` table (bundled builds do; iOS's system SQLite may not).
    dbstat: bool,
    /// `<dir>/LOCK`, locked exclusively until the engine drops. Declared after the connections
    /// (`writer`, `pool`), so they close before the lock is released.
    _lock: File,
    /// Failures for the next pinning reads to return, last first (the error-injection tests).
    #[cfg(test)]
    pin_faults: Mutex<Vec<std::ffi::c_int>>,
}

/// A connection to `path`; `create` makes the file if it is missing (the write connection).
fn open_connection(
    path: &Path,
    config: &SqliteConfig,
    create: bool,
) -> rusqlite::Result<Connection> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = match &config.vfs {
        Some(vfs) => Connection::open_with_flags_and_vfs(path, flags, vfs.as_str())?,
        None => Connection::open_with_flags(path, flags)?,
    };
    conn.busy_timeout(config.busy_timeout)?;
    Ok(conn)
}

/// The settings of every connection (SB0 §9.4's memory row). None of them writes the file.
fn configure(conn: &Connection, config: &SqliteConfig) -> rusqlite::Result<()> {
    // Room for every table's statements of a batch that writes many tables (`apply` takes up to
    // four a table), so none is prepared again per batch: rusqlite keeps 16 by default.
    conn.set_prepared_statement_cache_capacity(64);
    conn.pragma_update(None, "cache_size", -i64::from(config.cache_kib))?;
    conn.pragma_update(None, "mmap_size", 0)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    // No sorter helper threads (SB0 L5).
    conn.pragma_update(None, "threads", 0)?;
    if cfg!(target_vendor = "apple") {
        // `fsync` on Apple platforms does not reach the platter; `F_FULLFSYNC` does.
        conn.pragma_update(None, "fullfsync", 1)?;
        conn.pragma_update(None, "checkpoint_fullfsync", 1)?;
    }
    Ok(())
}

fn open_reader(path: &Path, config: &SqliteConfig) -> Result<Connection> {
    let conn = open_connection(path, config, false).map_err(|e| {
        StoreError::engine(ENGINE, ErrorKind::Io, format!("opening a read connection: {e}"))
    })?;
    configure(&conn, config)
        .and_then(|()| conn.pragma_update(None, "query_only", 1))
        .map_err(|e| engine_error(&conn, e))?;
    Ok(conn)
}

impl SqliteEngine {
    /// Opens (or creates) the database [`FILE`] in `dir` and checks its schema record,
    /// as [`check_schema`] does. An existing file is inspected before anything is written: it must
    /// be SQLite, hold only this schema's tables, and carry this schema's record at a version no
    /// newer than ours (or no record and no data), else the open fails
    /// ([`StoreError::UnknownDatabase`], [`StoreError::SchemaTooNew`]) and the file is unchanged.
    ///
    /// # Errors
    ///
    /// As above; [`ErrorKind::Io`] if another engine has `dir` open; [`ErrorKind::Other`] if the
    /// linked SQLite is older than [`MIN_SQLITE_VERSION`]; or SQLite's error opening or creating
    /// the file.
    pub fn open(
        dir: &Path,
        schema: &'static DbSchema,
        config: &SqliteConfig,
    ) -> Result<(Self, SchemaState)> {
        schema.validate();
        check_sqlite_version(rusqlite::version_number())?;
        let tables = Tables::new(schema);
        std::fs::create_dir_all(dir).map_err(|e| {
            StoreError::engine(ENGINE, ErrorKind::Io, format!("creating {}: {e}", dir.display()))
        })?;
        let lock = lock_dir(dir)?;
        let path = dir.join(FILE);
        let conn = open_connection(&path, config, true).map_err(|e| {
            StoreError::engine(ENGINE, ErrorKind::Io, format!("opening {}: {e}", path.display()))
        })?;
        inspect(&conn, schema, &tables)?;

        let wal: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .map_err(|e| engine_error(&conn, e))?;
        if !wal.eq_ignore_ascii_case("wal") {
            return Err(StoreError::engine(
                ENGINE,
                ErrorKind::Other,
                format!("{} cannot use a write-ahead log (journal mode {wal})", path.display()),
            ));
        }
        configure(&conn, config)
            .and_then(|()| conn.pragma_update(None, "wal_autocheckpoint", 1000))
            .and_then(|()| {
                conn.pragma_update(
                    None,
                    "journal_size_limit",
                    i64::try_from(config.journal_size_limit).unwrap_or(i64::MAX),
                )
            })
            .and_then(|()| conn.pragma_update(None, "synchronous", "FULL"))
            .map_err(|e| engine_error(&conn, e))?;
        let mut ddl = String::from("BEGIN IMMEDIATE;");
        for table in &tables.names {
            ddl.push_str(&format!(
                "CREATE TABLE IF NOT EXISTS {table} (k BLOB PRIMARY KEY, v BLOB NOT NULL) WITHOUT ROWID;"
            ));
        }
        ddl.push_str(
            "CREATE TABLE IF NOT EXISTS _dash_kv (k TEXT PRIMARY KEY, v INTEGER NOT NULL) \
             WITHOUT ROWID; COMMIT;",
        );
        conn.execute_batch(&ddl).map_err(|e| rolled_back(&conn, e))?;
        let dbstat = conn.prepare("SELECT 1 FROM dbstat LIMIT 0").is_ok();

        let reader = {
            let (path, config) = (path.clone(), config.clone());
            move || open_reader(&path, &config)
        };
        let engine = Self {
            schema,
            path,
            tables,
            writer: Mutex::new(Writer {
                conn,
                sync: Commit::Durable,
                buffered: false,
                #[cfg(test)]
                inject: None,
            }),
            pool: Pool::new(config.readers, config.read_wait, reader),
            fail_stop: FailStop::default(),
            dbstat,
            _lock: lock,
            #[cfg(test)]
            pin_faults: Mutex::default(),
        };
        let state = check_schema(&engine)?;
        Ok((engine, state))
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The kind of the failure that stopped writes, if one has.
    pub fn failed(&self) -> Option<ErrorKind> {
        self.fail_stop.failed()
    }

    /// The writer, if no write panicked while holding it.
    /// The writer. A write that panicked left it poisoned, but rolled back and latched (see
    /// [`guarded`](Self::guarded)), so its state is sound.
    fn writer(&self) -> MutexGuard<'_, Writer> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs a write or barrier under the fail-stop latch: refused once it has latched (checked
    /// again under the writer's lock, as another write may latch it while this one waits), and
    /// a failure latches it. A panic inside rolls back the open transaction and latches (its
    /// outcome is unknown) before it unwinds on.
    fn guarded(&self, f: impl FnOnce(&mut Writer, &Tables) -> Result<()>) -> Result<()> {
        self.fail_stop.check(ENGINE)?;
        let mut writer = self.writer();
        self.fail_stop.check(ENGINE)?;
        #[cfg(test)]
        if let Some(kind) = writer.inject.take() {
            return Err(self.fail_stop.latch(StoreError::engine(ENGINE, kind, "injected failure")));
        }
        match std::panic::catch_unwind(AssertUnwindSafe(|| f(&mut writer, &self.tables))) {
            Ok(result) => result.map_err(|e| self.fail_stop.latch(e)),
            Err(panic) => {
                if !writer.conn.is_autocommit() {
                    let _ = writer.conn.execute_batch("ROLLBACK");
                }
                self.fail_stop.latch(StoreError::engine(
                    ENGINE,
                    ErrorKind::Other,
                    "a write panicked: its outcome is unknown",
                ));
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// The pinned read view of a new snapshot, or why there is none.
    fn pin(&self) -> Result<Pooled<'_>, TakeError> {
        let conn = self.pool.take()?;
        let mut tries = 0;
        loop {
            let pinned = exec_cached(&conn, "BEGIN DEFERRED").and_then(|()| {
                #[cfg(test)]
                if let Some(code) = self.pin_faults.lock().unwrap().pop() {
                    return Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(code),
                        Some("injected pinning-read failure".into()),
                    ));
                }
                // The pinning read: it fixes the view (DEC-104 M6). Stepped to completion.
                conn.prepare_cached(&self.tables.cfs[DbSchema::META.index()].get)?
                    .query_row((SCHEMA_KEY,), |_| Ok(()))
                    .optional()
                    .map(drop)
            });
            match pinned {
                Ok(()) => return Ok(conn),
                Err(e) => {
                    let retry = busy_recovery(&e) && tries < BUSY_RECOVERY_RETRIES;
                    let err = rolled_back(&conn, e);
                    if retry {
                        tries += 1;
                        continue;
                    }
                    return Err(TakeError::Open(err));
                }
            }
        }
    }
}

/// Locks `<dir>/LOCK` exclusively, or fails if another engine holds it.
fn lock_dir(dir: &Path) -> Result<File> {
    let path = dir.join(LOCK);
    let io = |what: String| StoreError::engine(ENGINE, ErrorKind::Io, what);
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| io(format!("opening {}: {e}", path.display())))?;
    file.try_lock().map_err(|e| match e {
        TryLockError::WouldBlock => {
            io(format!("{} is in use: another engine has it open", dir.display()))
        }
        TryLockError::Error(e) => io(format!("locking {}: {e}", path.display())),
    })?;
    Ok(file)
}

/// `e` as an engine error, read before rolling back any open transaction (a rollback would
/// replace the OS error number it reports).
fn rolled_back(conn: &Connection, e: rusqlite::Error) -> StoreError {
    let err = engine_error(conn, e);
    if !conn.is_autocommit() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    err
}

/// Runs a transaction-control statement (`BEGIN`, `COMMIT`, `ROLLBACK`) from the connection's
/// statement cache. `execute_batch` parses it again on each call, which was most of what a
/// point read's pin cost beyond its query.
pub(crate) fn exec_cached(conn: &Connection, sql: &str) -> rusqlite::Result<()> {
    conn.prepare_cached(sql)?.execute([]).map(drop)
}

/// Checks an existing file without writing to it: SQLite, only our tables, a schema record that
/// is ours and not newer, or no record and no data (as [`check_schema`] does, but before the
/// open writes the WAL mode and the tables).
fn inspect(conn: &Connection, schema: &'static DbSchema, tables: &Tables) -> Result<()> {
    let unknown = |what: String| StoreError::UnknownDatabase {
        db: schema.name,
        what,
    };
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .map_err(|e| {
            if e.sqlite_error_code() == Some(rusqlite::ErrorCode::NotADatabase) {
                unknown(format!("it is not a SQLite database ({e})"))
            } else {
                engine_error(conn, e)
            }
        })?;
    let ours = |name: &str| {
        name == "_dash_kv" || name.starts_with("sqlite_") || tables.names.iter().any(|t| t == name)
    };
    if let Some(foreign) = names.iter().find(|n| !ours(n)) {
        return Err(unknown(format!("it holds a table `{foreign}`")));
    }
    let meta = &tables.names[DbSchema::META.index()];
    if names.contains(meta) {
        let record: Option<Vec<u8>> = conn
            .query_row(&tables.cfs[DbSchema::META.index()].get, (SCHEMA_KEY,), |r| r.get(0))
            .optional()
            .map_err(|e| engine_error(conn, e))?;
        if let Some(record) = record {
            return check_record(schema, &record).map(drop);
        }
    }
    for table in tables.names.iter().filter(|t| names.contains(t)) {
        let rows: Option<i64> = conn
            .query_row(&format!("SELECT 1 FROM {table} LIMIT 1"), [], |r| r.get(0))
            .optional()
            .map_err(|e| engine_error(conn, e))?;
        if rows.is_some() {
            return Err(unknown("it holds data but no schema record".into()));
        }
    }
    Ok(())
}

impl Writer {
    fn set_sync(&mut self, sync: Commit) -> Result<()> {
        if self.sync != sync {
            let level = match sync {
                Commit::Durable => "FULL",
                Commit::Buffered => "NORMAL",
            };
            self.conn
                .pragma_update(None, "synchronous", level)
                .map_err(|e| engine_error(&self.conn, e))?;
            self.sync = sync;
        }
        Ok(())
    }

    /// One transaction at `sync`: `BEGIN IMMEDIATE`, `body`, `COMMIT`. Any error, `COMMIT`'s
    /// included (its outcome is then unknown), rolls back what it can and is returned.
    ///
    /// A durable transaction with a buffered commit pending also bumps the barrier counter: one
    /// that writes no page (an empty batch, a delete of a missing key, a put of the value already
    /// stored, which SQLite does not rewrite) writes no frame, so its `COMMIT` would sync nothing
    /// and leave the buffered commits unsynced. Row counts cannot tell: the identical put counts
    /// as a change.
    ///
    /// With `force` (a Durable [`write_barrier`](KvEngine::write_barrier)), the counter is bumped
    /// whatever `buffered` says: after an open, buffered frames a killed process left in the page
    /// cache are synced too (SB14 D17, D22).
    fn transaction(
        &mut self,
        sync: Commit,
        force: bool,
        body: impl FnOnce(&Connection) -> rusqlite::Result<()>,
    ) -> Result<()> {
        self.set_sync(sync)?;
        let barrier = sync == Commit::Durable && (self.buffered || force);
        let conn = &self.conn;
        exec_cached(conn, "BEGIN IMMEDIATE")
            .and_then(|()| body(conn))
            .and_then(|()| {
                if barrier {
                    conn.prepare_cached(
                        "INSERT INTO _dash_kv (k, v) VALUES ('sync_seq', 1) \
                         ON CONFLICT (k) DO UPDATE SET v = v + 1",
                    )?
                    .execute([])?;
                }
                exec_cached(conn, "COMMIT")
            })
            .map_err(|e| rolled_back(conn, e))?;
        self.buffered = sync == Commit::Buffered;
        Ok(())
    }

    /// The barrier: an empty durable commit, which commits the counter if a buffered commit is
    /// newer and does nothing otherwise.
    fn sync(&mut self) -> Result<()> {
        if !self.buffered {
            return Ok(());
        }
        self.transaction(Commit::Durable, false, |_| Ok(()))
    }
}

/// What an operation of a [`SqliteBatch`] does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Put,
    Delete,
    DeleteRange,
}

/// One operation of a [`SqliteBatch`]: its table, and where its key and value (a range's start
/// and end) are in the batch's buffer.
#[derive(Clone, Copy)]
struct Op {
    cf: Cf,
    kind: Kind,
    key: (u32, u32),
    value: (u32, u32),
}

/// A batch for [`SqliteEngine`]: its operations, applied in order in one transaction. Keys and
/// values are copied into one buffer, not an allocation each (SB13 D49).
#[derive(Default)]
pub struct SqliteBatch {
    ops: Vec<Op>,
    buf: Vec<u8>,
    size: usize,
}

impl SqliteBatch {
    fn push(&mut self, cf: Cf, kind: Kind, key: &[u8], value: &[u8]) {
        // Offsets are u32: a batch is far below 4 GiB (SQLite's own limit on a value is 1 GB).
        let mut stash = |bytes: &[u8]| {
            let at = u32::try_from(self.buf.len()).expect("a batch under 4 GiB");
            self.buf.extend_from_slice(bytes);
            let len = u32::try_from(bytes.len()).expect("a value under 4 GiB");
            at.checked_add(len).expect("a batch under 4 GiB");
            (at, len)
        };
        let key = stash(key);
        let value = stash(value);
        self.ops.push(Op {
            cf,
            kind,
            key,
            value,
        });
    }

    fn bytes(&self, (at, len): (u32, u32)) -> &[u8] {
        &self.buf[at as usize..(at + len) as usize]
    }
}

impl KvBatch for SqliteBatch {
    fn put(&mut self, cf: Cf, key: &[u8], value: &[u8]) {
        self.size += key.len() + value.len() + 8;
        self.push(cf, Kind::Put, key, value);
    }

    fn delete(&mut self, cf: Cf, key: &[u8]) {
        self.size += key.len() + 8;
        self.push(cf, Kind::Delete, key, &[]);
    }

    fn delete_range(&mut self, cf: Cf, start: &[u8], end: &[u8]) {
        if start < end {
            self.size += start.len() + end.len() + 8;
            self.push(cf, Kind::DeleteRange, start, end);
        }
    }

    fn len(&self) -> usize {
        self.ops.len()
    }

    fn size_bytes(&self) -> usize {
        self.size
    }

    fn clear(&mut self) {
        self.ops.clear();
        self.buf.clear();
        self.size = 0;
    }
}

/// Applies `batch`'s operations in order, through one prepared statement per table and kind for
/// the whole batch: taking each from the cache per operation costs a hash and a lookup of its
/// SQL. A run of puts to one table goes [`PUT_ROWS`] rows a statement: the rows share its cursor,
/// so a key above the table's last is appended without a descent from the root, and the
/// statement's per-execution cost is paid once for them (SB13 D49). Rows of one statement are
/// inserted in order, so a later put of a key still replaces an earlier one.
fn apply(conn: &Connection, tables: &Tables, batch: &SqliteBatch) -> rusqlite::Result<()> {
    // Per table: put, delete, delete_range, put_rows.
    let mut stmts: Vec<Option<CachedStatement<'_>>> = Vec::new();
    stmts.resize_with(tables.cfs.len() * 4, || None);
    let ops = &batch.ops;
    let mut i = 0;
    while i < ops.len() {
        let op = ops[i];
        let cf = op.cf.index();
        let rows = ops[i..]
            .iter()
            .take(PUT_ROWS)
            .take_while(|o| o.kind == Kind::Put && o.cf == op.cf)
            .count();
        if rows == PUT_ROWS {
            let s = statement(conn, tables, &mut stmts, cf, 3)?;
            for (n, o) in ops[i..i + PUT_ROWS].iter().enumerate() {
                s.raw_bind_parameter(2 * n + 1, batch.bytes(o.key))?;
                s.raw_bind_parameter(2 * n + 2, batch.bytes(o.value))?;
            }
            s.raw_execute()?;
            i += PUT_ROWS;
            continue;
        }
        let (key, value) = (batch.bytes(op.key), batch.bytes(op.value));
        match op.kind {
            Kind::Put => statement(conn, tables, &mut stmts, cf, 0)?.execute((key, value)),
            Kind::Delete => statement(conn, tables, &mut stmts, cf, 1)?.execute((key,)),
            Kind::DeleteRange => statement(conn, tables, &mut stmts, cf, 2)?.execute((key, value)),
        }?;
        i += 1;
    }
    Ok(())
}

/// `apply`'s statement `which` (put, delete, delete_range, put_rows) of table `cf`, taken from
/// the cache on first use.
fn statement<'s, 'c>(
    conn: &'c Connection,
    tables: &Tables,
    stmts: &'s mut [Option<CachedStatement<'c>>],
    cf: usize,
    which: usize,
) -> rusqlite::Result<&'s mut CachedStatement<'c>> {
    let slot = &mut stmts[cf * 4 + which];
    if slot.is_none() {
        let sql = &tables.cfs[cf];
        *slot = Some(
            conn.prepare_cached([&sql.put, &sql.delete, &sql.delete_range, &sql.put_rows][which])?,
        );
    }
    Ok(slot.as_mut().expect("just set"))
}

/// A consistent read view of a [`SqliteEngine`], pinned when it was taken; it holds a read
/// connection until dropped, on the thread that took it:
///
/// ```compile_fail,E0277
/// fn send<T: Send>(_: T) {}
/// fn moved(engine: &dash_kv_sqlite::SqliteEngine) {
///     send(dash_kv::KvEngine::snapshot(engine));
/// }
/// ```
pub struct SqliteSnapshot<'a> {
    view: View<'a>,
    tables: &'a Tables,
}

enum View<'a> {
    Pinned(Pooled<'a>),
    /// The view could not be pinned: every read fails with this.
    Poisoned(ErrorKind, String),
}

impl SqliteSnapshot<'_> {
    fn conn(&self) -> Result<&Connection> {
        match &self.view {
            View::Pinned(conn) => Ok(conn),
            View::Poisoned(kind, message) => {
                Err(StoreError::engine(ENGINE, *kind, message.clone()))
            }
        }
    }
}

fn get(conn: &Connection, tables: &Tables, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
    conn.prepare_cached(&tables.cfs[cf.index()].get)
        .and_then(|mut s| s.query_row((key,), |r| r.get(0)).optional())
        .map_err(|e| engine_error(conn, e))
}

/// Every key through one prepared statement, taken from the cache once.
fn multi_get(
    conn: &Connection,
    tables: &Tables,
    cf: Cf,
    keys: &[&[u8]],
) -> Result<Vec<Option<Vec<u8>>>> {
    let mut stmt =
        conn.prepare_cached(&tables.cfs[cf.index()].get).map_err(|e| engine_error(conn, e))?;
    keys.iter()
        .map(|k| stmt.query_row((k,), |r| r.get(0)).optional().map_err(|e| engine_error(conn, e)))
        .collect()
}

/// A scan of either kind: a [`Scan`], or one that fails at once.
type Entries<'a> = Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a>;

/// `scan`'s entries, or its error once.
fn entries(scan: Result<Scan<'_>>) -> Entries<'_> {
    match scan {
        Ok(scan) => Box::new(scan),
        Err(e) => Box::new(std::iter::once(Err(e))),
    }
}

impl KvRead for SqliteSnapshot<'_> {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        get(self.conn()?, self.tables, cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        multi_get(self.conn()?, self.tables, cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        entries(
            self.conn()
                .and_then(|c| Scan::new(ScanConn::Borrowed(c), self.tables, cf.index(), prefix)),
        )
    }
}

impl KvRead for SqliteEngine {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let conn = self.pool.take()?;
        get(&conn, &self.tables, cf, key)
    }

    /// One read transaction, so every key comes from one view.
    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        let conn = self.pool.take()?;
        exec_cached(&conn, "BEGIN DEFERRED").map_err(|e| engine_error(&conn, e))?;
        multi_get(&conn, &self.tables, cf, keys)
        // Dropping `conn` ends the transaction.
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        entries(
            self.pool
                .take()
                .map_err(StoreError::from)
                .and_then(|c| Scan::new(ScanConn::Owned(c), &self.tables, cf.index(), prefix)),
        )
    }
}

impl KvEngine for SqliteEngine {
    type Batch = SqliteBatch;
    type Snapshot<'a> = SqliteSnapshot<'a>;

    fn schema(&self) -> &'static DbSchema {
        self.schema
    }

    fn batch(&self) -> SqliteBatch {
        SqliteBatch::default()
    }

    fn write(&self, batch: SqliteBatch, commit: Commit) -> Result<()> {
        self.guarded(|writer, tables| {
            writer.transaction(commit, false, |conn| apply(conn, tables, &batch))
        })
    }

    fn write_barrier(&self, batch: SqliteBatch) -> Result<()> {
        self.guarded(|writer, tables| {
            writer.transaction(Commit::Durable, true, |conn| apply(conn, tables, &batch))
        })
    }

    fn sync_wal(&self) -> Result<()> {
        self.guarded(|writer, _| writer.sync())
    }

    /// [`sync_wal`](KvEngine::sync_wal), then a passive checkpoint, which an open reader can leave
    /// partial (it is not a durability barrier).
    fn flush_memtables(&self) -> Result<()> {
        self.guarded(|writer, _| {
            writer.sync()?;
            let conn = &writer.conn;
            conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))
                .map_err(|e| engine_error(conn, e))
        })
    }

    /// The pinned view, or, if it cannot be pinned, a poisoned one whose every read fails; that
    /// failure latches the engine (Q7). A take the pool refuses as a bug of the caller (nested, or
    /// past its bound on waiting) poisons the snapshot without latching.
    fn snapshot(&self) -> SqliteSnapshot<'_> {
        let poisoned =
            |e: &StoreError| View::Poisoned(e.kind().unwrap_or(ErrorKind::Other), e.to_string());
        let view = match self.pin() {
            Ok(conn) => View::Pinned(conn),
            Err(TakeError::Misuse(e)) => poisoned(&e),
            Err(TakeError::Open(e)) => poisoned(&self.fail_stop.latch(e)),
        };
        SqliteSnapshot {
            view,
            tables: &self.tables,
        }
    }

    /// From SQLite's `dbstat` table, which visits every page of the table; without it, the sum of
    /// the key and value lengths, which reads every row. Either is O(table): off any hot path.
    fn approximate_size(&self, cf: Cf) -> Result<u64> {
        let conn = self.pool.take()?;
        let table = &self.tables.names[cf.index()];
        let size: Option<i64> = if self.dbstat {
            conn.query_row("SELECT SUM(pgsize) FROM dbstat WHERE name = ?1", (table,), |r| r.get(0))
        } else {
            conn.query_row(&format!("SELECT SUM(length(k) + length(v)) FROM {table}"), [], |r| {
                r.get(0)
            })
        }
        .map_err(|e| engine_error(&conn, e))?;
        Ok(size.map_or(0, |s| s.max(0) as u64))
    }
}

#[cfg(test)]
mod tests;
