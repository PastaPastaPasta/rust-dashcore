use std::cell::Cell;
use std::sync::{Arc, Barrier};
use std::time::Instant;

use dash_kv::contract::{CONTRACT_SCHEMA, EngineFactory};

use super::*;

fn point() -> Cf {
    CONTRACT_SCHEMA.cf("point").unwrap()
}

/// Each database is a directory of its own under one temporary root.
struct Sqlite {
    root: tempfile::TempDir,
    db: Cell<u32>,
    config: SqliteConfig,
}

impl Sqlite {
    fn with(config: SqliteConfig) -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            db: Cell::new(0),
            config,
        }
    }

    /// Two readers: the contract reads through the engine while it holds a snapshot.
    fn new() -> Self {
        Self::with(SqliteConfig {
            readers: 2,
            ..SqliteConfig::default()
        })
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join(self.db.get().to_string())
    }

    fn engine(&self) -> SqliteEngine {
        self.fresh();
        self.open(&CONTRACT_SCHEMA).unwrap().0
    }
}

impl EngineFactory for Sqlite {
    type Engine = SqliteEngine;

    /// Durable commits and barriers fsync, which takes ~200 ms on a busy disk; as many as the
    /// RocksDB factory runs.
    const MODEL_SEEDS: u64 = 6;

    fn fresh(&self) {
        self.db.set(self.db.get() + 1);
    }

    fn open(&self, schema: &'static DbSchema) -> Result<(SqliteEngine, SchemaState)> {
        SqliteEngine::open(&self.dir(), schema, &self.config)
    }

    fn fail_next_barrier(&self, engine: &SqliteEngine) -> Box<dyn FnOnce() + '_> {
        engine.writer().inject = Some(ErrorKind::Io);
        Box::new(|| {})
    }
}

mod contract {
    dash_kv::engine_contract_tests!(super::Sqlite::new());
}

fn put(engine: &impl KvEngine, key: &[u8], value: &[u8], commit: Commit) -> Result<()> {
    let mut batch = engine.batch();
    batch.put(point(), key, value);
    engine.write(batch, commit)
}

/// The linked SQLite is new enough, and an older one is refused by name (BL-303).
#[test]
fn an_sqlite_older_than_the_minimum_is_refused() {
    check_sqlite_version(rusqlite::version_number()).unwrap();
    check_sqlite_version(MIN_SQLITE_VERSION).unwrap();
    let e = check_sqlite_version(3_034_000).unwrap_err();
    assert!(e.to_string().contains("SQLite 3.34.0 is older than 3.34.1"), "{e}");
}

/// SB0 §9.4's negative control: a read connection in `BEGIN DEFERRED` without the pinning read
/// has no view yet, and sees a commit made after its `BEGIN`. The snapshot's pinning read is what
/// makes it read A.
#[test]
fn without_the_pinning_read_a_deferred_transaction_sees_later_commits() {
    let f = Sqlite::new();
    let engine = f.engine();
    put(&engine, b"k", b"A", Commit::Buffered).unwrap();

    let bare = engine.pool.take().map_err(StoreError::from).unwrap();
    bare.execute_batch("BEGIN DEFERRED").unwrap();
    let snap = engine.snapshot();
    put(&engine, b"k", b"B", Commit::Buffered).unwrap();
    assert_eq!(get(&bare, &engine.tables, point(), b"k").unwrap().as_deref(), Some(&b"B"[..]));
    assert_eq!(snap.get(point(), b"k").unwrap().as_deref(), Some(&b"A"[..]));
}

/// More threads than read connections take snapshots at once: the extra ones wait, the pool never
/// opens beyond its cap, and every one completes.
#[test]
fn the_pool_never_opens_beyond_its_cap() {
    let f = Sqlite::new();
    let engine = f.engine();
    put(&engine, b"k", b"v", Commit::Durable).unwrap();
    let start = Barrier::new(6);
    std::thread::scope(|s| {
        for _ in 0..6 {
            s.spawn(|| {
                start.wait();
                for _ in 0..20 {
                    let snap = engine.snapshot();
                    assert!(engine.pool.opened() <= 2, "opened {}", engine.pool.opened());
                    assert_eq!(snap.get(point(), b"k").unwrap().as_deref(), Some(&b"v"[..]));
                }
            });
        }
    });
    assert_eq!(engine.pool.opened(), 2);
}

/// The pinning read fails: the snapshot is poisoned (every read fails with that error, no
/// unpinned view is served) and the engine is latched (writes are refused).
#[test]
fn a_failed_pinning_read_poisons_the_snapshot_and_latches() {
    let f = Sqlite::new();
    let engine = f.engine();
    put(&engine, b"k", b"v", Commit::Durable).unwrap();
    engine.pin_faults.lock().unwrap().push(rusqlite::ffi::SQLITE_IOERR_READ);
    let snap = engine.snapshot();
    for err in [
        snap.get(point(), b"k").unwrap_err(),
        snap.multi_get(point(), &[b"k"]).unwrap_err(),
        snap.iter_prefix(point(), &[]).next().unwrap().unwrap_err(),
    ] {
        assert_eq!(err.kind(), Some(ErrorKind::Io), "{err}");
        assert!(err.to_string().contains("injected pinning-read failure"), "{err}");
    }
    assert_eq!(engine.failed(), Some(ErrorKind::Io));
    let refused = put(&engine, b"k", b"w", Commit::Buffered).unwrap_err();
    assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly));
    drop(snap);
    assert_eq!(engine.get(point(), b"k").unwrap().as_deref(), Some(&b"v"[..]), "reads work");
}

/// The same failure behind the client's latch (Q7): the poisoned snapshot's first read trips the
/// latch and reaches the sink, and the client's other store refuses writes.
#[test]
fn a_failed_pinning_read_trips_the_clients_latch() {
    use dash_kv::{AbortMark, GlobalFailStop, LatchedEngine, RecordingSink, Sites};
    const SITES: Sites = Sites {
        read: "spv test read",
        write: "spv test write",
        barrier: "spv test barrier",
    };
    let (f, g) = (Sqlite::new(), Sqlite::new());
    let latch = GlobalFailStop::new();
    let sink = Arc::new(RecordingSink::default());
    let headers = LatchedEngine::new(f.engine(), latch.clone(), sink.clone(), SITES);
    let filters = LatchedEngine::new(g.engine(), latch.clone(), sink.clone(), SITES);
    put(&headers, b"k", b"v", Commit::Durable).unwrap();

    headers.inner().pin_faults.lock().unwrap().push(rusqlite::ffi::SQLITE_IOERR_READ);
    let snap = headers.snapshot();
    assert!(latch.tripped().is_none(), "nothing has read the snapshot yet");
    let err = snap.get(point(), b"k").unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::Io), "{err}");
    let failure = latch.tripped().expect("the read tripped the latch");
    assert_eq!((failure.kind, failure.site), (ErrorKind::Io, "spv test read"));
    assert!(
        matches!(&sink.marks()[..], [AbortMark::ReadFailed(what)] if what.contains("injected pinning-read")),
        "{:?}",
        sink.marks()
    );
    for refused in [put(&filters, b"k", b"w", Commit::Buffered), filters.sync_wal()] {
        let err = refused.unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::ReadOnly), "{err}");
    }
    assert_eq!(filters.get(point(), b"k").unwrap(), None, "reads work");
}

/// `SQLITE_BUSY_RECOVERY` (another connection rebuilding the WAL index) is retried a bounded
/// number of times; past that the snapshot is poisoned, as for any pinning failure.
#[test]
fn busy_recovery_is_retried_then_gives_up() {
    let f = Sqlite::new();
    let engine = f.engine();
    put(&engine, b"k", b"v", Commit::Buffered).unwrap();
    let busy = rusqlite::ffi::SQLITE_BUSY_RECOVERY;
    *engine.pin_faults.lock().unwrap() = vec![busy; BUSY_RECOVERY_RETRIES as usize];
    let snap = engine.snapshot();
    assert_eq!(snap.get(point(), b"k").unwrap().as_deref(), Some(&b"v"[..]));
    assert!(engine.pin_faults.lock().unwrap().is_empty());
    drop(snap);
    assert_eq!(engine.failed(), None);

    *engine.pin_faults.lock().unwrap() = vec![busy; BUSY_RECOVERY_RETRIES as usize + 1];
    let err = engine.snapshot().get(point(), b"k").unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::Other), "{err}");
    assert_eq!(engine.failed(), Some(ErrorKind::Other));
}

fn synchronous(engine: &SqliteEngine) -> i64 {
    engine.writer().conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap()
}

fn sync_seq(engine: &SqliteEngine) -> Option<i64> {
    engine
        .writer()
        .conn
        .query_row("SELECT v FROM _dash_kv WHERE k = 'sync_seq'", [], |r| r.get(0))
        .optional()
        .unwrap()
}

/// A buffered commit runs at `synchronous=NORMAL` (1), a durable one at `FULL` (2); `sync_wal`
/// commits the counter durably only when a buffered commit is newer than the last durable one.
#[test]
fn commit_levels_and_the_barrier_counter() {
    let f = Sqlite::new();
    let engine = f.engine();
    let before = sync_seq(&engine);
    engine.sync_wal().unwrap();
    assert_eq!(sync_seq(&engine), before, "nothing buffered: no barrier commit");

    put(&engine, b"k", b"1", Commit::Buffered).unwrap();
    assert_eq!(synchronous(&engine), 1);
    engine.sync_wal().unwrap();
    assert_eq!(synchronous(&engine), 2);
    let first = sync_seq(&engine).expect("a barrier commit");
    engine.sync_wal().unwrap();
    assert_eq!(sync_seq(&engine), Some(first), "already durable");

    put(&engine, b"k", b"2", Commit::Buffered).unwrap();
    put(&engine, b"k", b"3", Commit::Durable).unwrap();
    assert_eq!(synchronous(&engine), 2);
    let second = sync_seq(&engine);
    assert!(second > Some(first), "a durable commit after a buffered one bumps the counter");
    engine.sync_wal().unwrap();
    assert_eq!(sync_seq(&engine), second, "the durable commit covered the buffered one");
    put(&engine, b"k", b"4", Commit::Durable).unwrap();
    assert_eq!(sync_seq(&engine), second, "nothing buffered: no counter commit");

    put(&engine, b"k", b"5", Commit::Buffered).unwrap();
    engine.flush_memtables().unwrap();
    assert!(sync_seq(&engine) > second, "flush_memtables syncs first");
}

/// SB14 D22: `write_barrier` bumps the counter, so its commit writes a frame and syncs the WAL,
/// even with nothing buffered in this process (an open after a kill) and an empty batch.
#[test]
fn write_barrier_always_bumps_the_counter() {
    let f = Sqlite::new();
    let engine = f.engine();
    let before = sync_seq(&engine).unwrap_or(0);
    engine.write_barrier(engine.batch()).unwrap();
    assert_eq!(synchronous(&engine), 2);
    let first = sync_seq(&engine).expect("a counter commit");
    assert!(first > before);
    engine.write_barrier(engine.batch()).unwrap();
    assert!(sync_seq(&engine) > Some(first), "forced again, nothing buffered");
    let mut b = engine.batch();
    b.put(point(), b"k", b"1");
    engine.write_barrier(b).unwrap();
    assert_eq!(engine.get(point(), b"k").unwrap().as_deref(), Some(&b"1"[..]));
}

/// A durable commit that writes no page (an empty batch, a delete of a missing key, an empty
/// range, a put of the value already stored) writes no WAL frame of its own: with a buffered
/// commit pending it commits the barrier counter, so its fsync covers the buffered one, and
/// `sync_wal` after it has nothing to do.
#[test]
fn a_durable_commit_that_changes_nothing_syncs_the_buffered_ones() {
    let f = Sqlite::new();
    let engine = f.engine();
    let mut seq = sync_seq(&engine);
    let changes_nothing: [fn(&mut SqliteBatch); 4] = [
        |_| {},
        |b| b.delete(point(), b"missing"),
        |b| b.delete_range(point(), b"x", b"y"),
        |b| b.put(point(), b"k", b"buffered"),
    ];
    for fill in changes_nothing {
        put(&engine, b"k", b"buffered", Commit::Buffered).unwrap();
        let mut batch = engine.batch();
        fill(&mut batch);
        engine.write(batch, Commit::Durable).unwrap();
        assert!(sync_seq(&engine) > seq, "the durable commit bumped the counter");
        seq = sync_seq(&engine);
        engine.sync_wal().unwrap();
        assert_eq!(sync_seq(&engine), seq, "nothing left to sync");

        let mut batch = engine.batch();
        fill(&mut batch);
        engine.write(batch, Commit::Durable).unwrap();
        assert_eq!(sync_seq(&engine), seq, "nothing buffered: no counter commit");
    }
}

/// One engine per directory: a second open fails while the first is open, and works after.
#[test]
fn a_second_open_of_the_directory_fails() {
    let f = Sqlite::new();
    let engine = f.engine();
    put(&engine, b"k", b"v", Commit::Durable).unwrap();
    let err = f.open(&CONTRACT_SCHEMA).map(drop).unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::Io), "{err}");
    assert!(err.to_string().contains("in use"), "{err}");
    put(&engine, b"k", b"w", Commit::Durable).unwrap();
    drop(engine);
    let (engine, state) = f.open(&CONTRACT_SCHEMA).unwrap();
    assert_eq!(state, SchemaState::Current);
    assert_eq!(engine.get(point(), b"k").unwrap().as_deref(), Some(&b"w"[..]));
}

/// A panic inside a write (its outcome unknown) latches the engine: later writes are refused, the
/// open transaction is rolled back, and reads work.
#[test]
fn a_write_that_panicked_latches() {
    let f = Sqlite::new();
    let engine = f.engine();
    put(&engine, b"k", b"v", Commit::Durable).unwrap();
    let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
        engine.guarded(|writer, tables| {
            writer.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            let mut batch = SqliteBatch::default();
            batch.put(point(), b"k", b"half");
            apply(&writer.conn, tables, &batch).unwrap();
            panic!("a bug inside a write");
        })
    }));
    assert!(panicked.is_err());
    assert_eq!(engine.failed(), Some(ErrorKind::Other), "latched as it unwound");
    assert!(engine.writer().conn.is_autocommit(), "rolled back");
    let err = put(&engine, b"k", b"w", Commit::Buffered).unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::ReadOnly), "{err}");
    assert!(err.to_string().contains("panicked"), "{err}");
    assert_eq!(engine.sync_wal().unwrap_err().kind(), Some(ErrorKind::ReadOnly));
    assert_eq!(engine.get(point(), b"k").unwrap().as_deref(), Some(&b"v"[..]));
}

/// A write that waited for the writer's lock while another write latched the engine is refused:
/// the latch is checked again under the lock.
#[test]
fn a_write_waiting_for_the_lock_sees_a_latch_set_meanwhile() {
    let f = Sqlite::new();
    let engine = f.engine();
    let writer = engine.writer();
    let started = Barrier::new(2);
    std::thread::scope(|s| {
        let waiting = s.spawn(|| {
            started.wait();
            put(&engine, b"k", b"late", Commit::Durable)
        });
        started.wait();
        // Let the write pass the first check and block on the lock.
        std::thread::sleep(Duration::from_millis(300));
        engine.fail_stop.latch(StoreError::engine(ENGINE, ErrorKind::Io, "latched meanwhile"));
        drop(writer);
        let err = waiting.join().unwrap().unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::ReadOnly), "{err}");
    });
    assert_eq!(engine.get(point(), b"k").unwrap(), None, "the late write wrote nothing");
}

/// A live scan reads the view as of `iter_prefix`, not of its first `next`.
#[test]
fn a_live_scan_reads_the_view_at_its_creation() {
    let f = Sqlite::new();
    let engine = f.engine();
    put(&engine, b"a", b"1", Commit::Buffered).unwrap();
    let scan = engine.iter_prefix(point(), &[]);
    put(&engine, b"b", b"2", Commit::Buffered).unwrap();
    let keys: Vec<Vec<u8>> = scan.map(|e| e.unwrap().0).collect();
    assert_eq!(keys, [b"a".to_vec()]);
}

fn mobile() -> Sqlite {
    Sqlite::with(SqliteConfig {
        readers: 1,
        ..SqliteConfig::default()
    })
}

/// With one reader, a live read inside a snapshot on the same thread would wait for itself: a
/// release build returns an error instead (Q8), and the snapshot keeps working.
#[test]
fn a_nested_take_is_an_error_instead_of_a_hang() {
    let f = mobile();
    let engine = f.engine();
    let snap = engine.snapshot();
    let Err(TakeError::Misuse(err)) = engine.pool.take_checked(false).map(drop) else {
        panic!("a nested take on a full pool");
    };
    assert_eq!(err.kind(), Some(ErrorKind::Other));
    assert!(err.to_string().contains("nested pool take"), "{err}");
    assert_eq!(snap.get(point(), b"k").unwrap(), None);
    assert_eq!(engine.failed(), None, "a caller's bug latches nothing");
}

/// Across threads (SB10 Q8): two threads each hold a snapshot of a 2-reader pool and each read
/// live, so each waits for the other's connection. Each read fails with `Other` after the pool's
/// bound on waiting instead of hanging; the snapshots and later reads work.
#[test]
fn snapshot_holders_reading_live_fail_instead_of_hanging() {
    let f = Sqlite::with(SqliteConfig {
        readers: 2,
        read_wait: Duration::from_millis(300),
        ..SqliteConfig::default()
    });
    let engine = f.engine();
    put(&engine, b"k", b"v", Commit::Durable).unwrap();
    let (held, read) = (Barrier::new(2), Barrier::new(2));
    std::thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| {
                let snap = engine.snapshot();
                held.wait();
                let err = engine.get(point(), b"k").unwrap_err();
                assert_eq!(err.kind(), Some(ErrorKind::Other), "{err}");
                assert!(err.to_string().contains("came back within"), "{err}");
                read.wait();
                assert_eq!(snap.get(point(), b"k").unwrap().as_deref(), Some(&b"v"[..]));
            });
        }
    });
    assert_eq!(engine.get(point(), b"k").unwrap().as_deref(), Some(&b"v"[..]));
    assert_eq!(engine.failed(), None, "a caller's bug latches nothing");
}

/// A thread that holds no connection waits out another thread's scan however long it runs (SB10
/// Q8 r5): contention is not a caller's bug, so the read neither fails nor latches.
#[test]
fn a_read_waits_out_another_threads_long_scan() {
    let wait = Duration::from_millis(200);
    let f = Sqlite::with(SqliteConfig {
        readers: 1,
        read_wait: wait,
        ..SqliteConfig::default()
    });
    let engine = f.engine();
    put(&engine, b"k", b"v", Commit::Durable).unwrap();
    let scanning = Barrier::new(2);
    std::thread::scope(|s| {
        s.spawn(|| {
            let scan = engine.iter_prefix(point(), &[]);
            scanning.wait();
            std::thread::sleep(wait * 3);
            drop(scan);
        });
        scanning.wait();
        let started = Instant::now();
        assert_eq!(engine.get(point(), b"k").unwrap().as_deref(), Some(&b"v"[..]));
        assert!(started.elapsed() > wait, "the read waited for the scan's connection");
    });
    assert_eq!(engine.failed(), None);
}

/// The same nesting asserts in a debug build.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "nested pool take")]
fn a_nested_take_asserts_in_debug_builds() {
    let f = mobile();
    let engine = f.engine();
    let _snap = engine.snapshot();
    let _ = engine.get(point(), b"k");
}

/// The same nesting through the engine in a release build: the live read fails, the snapshot and
/// later reads work.
#[test]
#[cfg(not(debug_assertions))]
fn a_nested_read_fails_in_release_builds() {
    let f = mobile();
    let engine = f.engine();
    let snap = engine.snapshot();
    let err = engine.get(point(), b"k").unwrap_err();
    assert!(err.to_string().contains("nested pool take"), "{err}");
    let scan = engine.iter_prefix(point(), &[]).next().unwrap().unwrap_err();
    assert!(scan.to_string().contains("nested pool take"), "{scan}");
    assert_eq!(snap.get(point(), b"k").unwrap(), None);
    drop(snap);
    assert_eq!(engine.get(point(), b"k").unwrap(), None);
    assert_eq!(engine.failed(), None);
}

/// A file that is not SQLite is another database, and stays as it was.
#[test]
fn a_file_that_is_not_sqlite_is_unknown() {
    let f = Sqlite::new();
    f.fresh();
    std::fs::create_dir_all(f.dir()).unwrap();
    let file = f.dir().join(FILE);
    let garbage = vec![0x5a; 8192];
    std::fs::write(&file, &garbage).unwrap();
    match f.open(&CONTRACT_SCHEMA).map(drop) {
        Err(StoreError::UnknownDatabase {
            ..
        }) => {}
        other => panic!("a garbage file opens as {other:?}"),
    }
    assert_eq!(std::fs::read(&file).unwrap(), garbage);
}

/// Another program's SQLite file is another database: the open fails before writing anything.
#[test]
fn a_file_with_a_foreign_table_is_unknown() {
    let f = Sqlite::new();
    f.fresh();
    std::fs::create_dir_all(f.dir()).unwrap();
    let file = f.dir().join(FILE);
    let tables = |conn: &Connection| -> Vec<String> {
        conn.prepare("SELECT name FROM sqlite_master ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let conn = Connection::open(&file).unwrap();
    conn.execute_batch("CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    let before = tables(&conn);
    drop(conn);
    match f.open(&CONTRACT_SCHEMA).map(drop) {
        Err(StoreError::UnknownDatabase {
            what,
            ..
        }) => assert!(what.contains("accounts"), "{what}"),
        other => panic!("a foreign table opens as {other:?}"),
    }
    let conn = Connection::open(&file).unwrap();
    assert_eq!(tables(&conn), before);
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "delete", "not switched to WAL");
}

/// A file with our tables, data and no schema record is someone else's: the open fails before
/// writing anything (no WAL mode, no new tables), as `check_schema` would after it.
#[test]
fn data_without_a_record_is_refused_before_anything_is_written() {
    let f = Sqlite::new();
    f.fresh();
    std::fs::create_dir_all(f.dir()).unwrap();
    let file = f.dir().join(FILE);
    let conn = Connection::open(&file).unwrap();
    conn.execute_batch(
        "CREATE TABLE cf_point (k BLOB PRIMARY KEY, v BLOB NOT NULL) WITHOUT ROWID; \
         INSERT INTO cf_point VALUES (x'6b', x'76');",
    )
    .unwrap();
    drop(conn);
    let before = std::fs::read(&file).unwrap();
    match f.open(&CONTRACT_SCHEMA).map(drop) {
        Err(StoreError::UnknownDatabase {
            what,
            ..
        }) => assert!(what.contains("no schema record"), "{what}"),
        other => panic!("data without a record opens as {other:?}"),
    }
    assert_eq!(std::fs::read(&file).unwrap(), before, "the file is unchanged");
    assert!(!f.dir().join(format!("{FILE}-wal")).exists());
}

/// A database of ours with a smashed table page reads as `Corruption`.
#[test]
fn a_smashed_page_is_corruption() {
    let f = Sqlite::new();
    let engine = f.engine();
    let mut batch = engine.batch();
    for i in 0..2000u32 {
        batch.put(point(), &i.to_be_bytes(), &[7; 100]);
    }
    engine.write(batch, Commit::Durable).unwrap();
    engine.flush_memtables().unwrap();
    let path = engine.path().to_owned();
    drop(engine);
    let mut bytes = std::fs::read(&path).unwrap();
    let page = 4096;
    for b in &mut bytes[page * 4..page * 40] {
        *b = 0xa5;
    }
    std::fs::write(&path, bytes).unwrap();
    let (engine, _) = f.open(&CONTRACT_SCHEMA).unwrap();
    let err = engine
        .iter_prefix(point(), &[])
        .find_map(Result::err)
        .expect("a scan over smashed pages fails");
    assert_eq!(err.kind(), Some(ErrorKind::Corruption), "{err}");
}

/// A real `SQLITE_FULL` (a page-count cap) is `NoSpace`, and latches.
#[test]
fn a_full_database_is_no_space_and_latches() {
    let f = Sqlite::new();
    let engine = f.engine();
    let pages: i64 = engine.writer().conn.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    engine.writer().conn.pragma_update(None, "max_page_count", pages + 2).unwrap();
    let mut batch = engine.batch();
    for i in 0..5000u32 {
        batch.put(point(), &i.to_be_bytes(), &[7; 100]);
    }
    let err = engine.write(batch, Commit::Buffered).unwrap_err();
    assert_eq!(err.kind(), Some(ErrorKind::NoSpace), "{err}");
    assert_eq!(engine.failed(), Some(ErrorKind::NoSpace));
    assert_eq!(engine.get(point(), &0u32.to_be_bytes()).unwrap(), None, "rolled back");
    let refused = engine.sync_wal().unwrap_err();
    assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly), "{refused}");
}

#[test]
fn errno_full_or_quota_is_no_space() {
    use rusqlite::ErrorCode::{DatabaseBusy, SystemIoFailure};
    assert_eq!(kind_of_errno(SystemIoFailure, std::io::ErrorKind::StorageFull), ErrorKind::NoSpace);
    assert_eq!(
        kind_of_errno(SystemIoFailure, std::io::ErrorKind::QuotaExceeded),
        ErrorKind::NoSpace
    );
    assert_eq!(kind_of_errno(SystemIoFailure, std::io::ErrorKind::PermissionDenied), ErrorKind::Io);
    assert_eq!(crate::error::kind_of(DatabaseBusy, 0), ErrorKind::Other);
}

/// The OS error number of `kind` on this platform, through [`error::kind_of`].
fn kind_of_errno(code: rusqlite::ErrorCode, kind: std::io::ErrorKind) -> ErrorKind {
    let errno = (1..200)
        .find(|&n| std::io::Error::from_raw_os_error(n).kind() == kind)
        .expect("the platform has that error");
    crate::error::kind_of(code, errno)
}

/// `approximate_size` from `dbstat`, and from the row sum a build without it uses (iOS's system
/// SQLite may lack it, Q9).
#[test]
fn approximate_size_with_and_without_dbstat() {
    let f = Sqlite::new();
    let mut engine = f.engine();
    assert!(engine.dbstat, "bundled SQLite has dbstat");
    let mut batch = engine.batch();
    for i in 0..1000u32 {
        batch.put(point(), &i.to_be_bytes(), &[7; 96]);
    }
    engine.write(batch, Commit::Durable).unwrap();
    let pages = engine.approximate_size(point()).unwrap();
    assert!(pages >= 100_000, "dbstat: {pages}");
    engine.dbstat = false;
    assert_eq!(engine.approximate_size(point()).unwrap(), 100_000, "the row sum");
}

/// Apple's `fsync` does not reach the platter, so syncs use `F_FULLFSYNC` (SB0 §9.4); elsewhere
/// the pragmas are left alone.
#[test]
fn fullfsync_on_apple_only() {
    let f = Sqlite::new();
    let engine = f.engine();
    for pragma in ["fullfsync", "checkpoint_fullfsync"] {
        let on: i64 =
            engine.writer().conn.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0)).unwrap();
        assert_eq!(on == 1, cfg!(target_vendor = "apple"), "{pragma}");
    }
}

/// A scan pages through more rows than a page, keeps to its prefix, and handles empty keys and
/// values, an all-`0xff` prefix, and keys that end in `0xff`.
#[test]
fn scans_cross_pages_and_prefix_edges() {
    let f = Sqlite::new();
    let engine = f.engine();
    let mut batch = engine.batch();
    batch.put(point(), b"", b"");
    for i in 0..700u16 {
        batch.put(point(), &[&[0x01][..], &i.to_be_bytes()].concat(), &[]);
    }
    batch.put(point(), &[0x01, 0xff, 0xff, 0xff], b"edge");
    batch.put(point(), &[0x02], b"after");
    batch.put(point(), &[0xff], b"ff");
    batch.put(point(), &[0xff, 0xff, 0x00], b"ffff00");
    engine.write(batch, Commit::Buffered).unwrap();

    let keys = |db: &dyn Fn(&[u8]) -> Vec<Vec<u8>>, prefix: &[u8]| db(prefix);
    let live = |prefix: &[u8]| -> Vec<Vec<u8>> {
        engine.iter_prefix(point(), prefix).map(|e| e.unwrap().0).collect()
    };
    let snap = engine.snapshot();
    let pinned = |prefix: &[u8]| -> Vec<Vec<u8>> {
        snap.iter_prefix(point(), prefix).map(|e| e.unwrap().0).collect()
    };
    for scan in [&live as &dyn Fn(&[u8]) -> Vec<Vec<u8>>, &pinned] {
        let ones = keys(scan, &[0x01]);
        assert_eq!(ones.len(), 701);
        assert!(ones.windows(2).all(|w| w[0] < w[1]), "in key order across pages");
        assert_eq!(ones.last().unwrap(), &[0x01, 0xff, 0xff, 0xff]);
        assert_eq!(keys(scan, &[0xff]), [vec![0xff], vec![0xff, 0xff, 0x00]]);
        assert_eq!(keys(scan, &[0xff, 0xff]), [vec![0xff, 0xff, 0x00]]);
        assert_eq!(keys(scan, &[]).len(), 705);
        assert_eq!(keys(scan, &[]).first().unwrap(), &Vec::<u8>::new(), "the empty key");
    }
    assert_eq!(engine.get(point(), b"").unwrap(), Some(Vec::new()), "an empty value");
}

#[test]
fn upper_bounds() {
    assert_eq!(scan::upper_bound(b""), None);
    assert_eq!(scan::upper_bound(&[0xff, 0xff]), None);
    assert_eq!(scan::upper_bound(&[1, 2]), Some(vec![1, 3]));
    assert_eq!(scan::upper_bound(&[1, 0xff]), Some(vec![2]));
}

/// A pooled connection that comes back holds no read view: a live read after a snapshot is
/// dropped sees the latest commit (the contract's `reads_after_snapshot_reuse`, on one reader).
#[test]
fn a_returned_connection_holds_no_view() {
    let f = mobile();
    let engine = f.engine();
    for round in 0..3u8 {
        drop(engine.snapshot());
        put(&engine, b"k", &[round], Commit::Buffered).unwrap();
        assert_eq!(engine.get(point(), b"k").unwrap(), Some(vec![round]));
        let scan: Vec<_> = engine.iter_prefix(point(), b"k").map(|e| e.unwrap().1).collect();
        assert_eq!(scan, [vec![round]]);
    }
    assert_eq!(engine.pool.opened(), 1);
}

/// SB13 D50: runs of puts to one table go through the multi-row statement (`PUT_ROWS` a
/// statement, the tail one by one), mixed with other ops, with empty values and a key put twice
/// in one run (the later value wins, as in sequential puts).
#[test]
fn multi_row_puts_match_sequential_puts() {
    let f = Sqlite::new();
    let engine = f.engine();
    let n = 3 * PUT_ROWS + 5;
    let key = |i: usize| format!("k{i:05}").into_bytes();
    let mut b = engine.batch();
    for i in 0..n {
        let value = if i % 7 == 0 {
            Vec::new()
        } else {
            vec![i as u8; i % 50]
        };
        b.put(point(), &key(i), &value);
    }
    b.put(point(), &key(3), b"again");
    b.delete(point(), &key(10));
    for i in (0..n).step_by(5) {
        b.put(point(), &key(i), b"third");
    }
    engine.write(b, Commit::Buffered).unwrap();
    for i in 0..n {
        let want: Option<Vec<u8>> = match i {
            _ if i % 5 == 0 => Some(b"third".to_vec()),
            3 => Some(b"again".to_vec()),
            10 => None,
            _ if i % 7 == 0 => Some(Vec::new()),
            _ => Some(vec![i as u8; i % 50]),
        };
        assert_eq!(engine.get(point(), &key(i)).unwrap(), want, "key {i}");
    }
}
