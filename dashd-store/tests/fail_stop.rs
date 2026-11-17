//! The engine's fail-stop contract (`KvEngine`, "Errors"): after a write, barrier or background
//! error the engine refuses every later write until it is reopened, even once the cause is gone.
//!
//! RocksDB alone does not do this: after `ENOSPC` its `SstFileManager` polls free space every 5 s
//! and resumes writes by itself (`db/error_handler.cc` `RecoverFromNoSpace`), which would let a
//! later commit land after a failed one (ST1 review r3, M1). `RocksEngine` latches the first error.

#![cfg(feature = "rocksdb")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dashd_store::{
    CfProfile, CfSpec, Commit, DbSchema, ErrorKind, KvBatch, KvEngine, KvRead, RocksConfig,
    RocksEngine, StoreError,
};

static SCHEMA: DbSchema = DbSchema {
    name: "fail-stop-test",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("coins", CfProfile::PointLookup)],
};

fn coins() -> dashd_store::Cf {
    SCHEMA.cf("coins").unwrap()
}

fn open(dir: &Path) -> RocksEngine {
    RocksEngine::open(dir, &SCHEMA, &RocksConfig::default()).unwrap().0
}

fn put(engine: &RocksEngine, key: &[u8], value: &[u8], commit: Commit) -> Result<(), StoreError> {
    let mut batch = engine.batch();
    batch.put(coins(), key, value);
    engine.write(batch, commit)
}

/// Every write path is refused with `ReadOnly`, and the message keeps the original failure.
fn assert_refuses_writes(engine: &RocksEngine, original: ErrorKind) {
    let refusals = [
        ("buffered write", put(engine, b"after", b"x", Commit::Buffered)),
        ("durable write", put(engine, b"after", b"x", Commit::Durable)),
        ("sync_wal", engine.sync_wal()),
        ("flush_memtables", engine.flush_memtables()),
    ];
    for (what, result) in refusals {
        let err = result.expect_err(what);
        assert_eq!(err.kind(), Some(ErrorKind::ReadOnly), "{what}: {err}");
        assert!(err.to_string().contains(&format!("{original:?}")), "{what}: {err}");
    }
    assert_eq!(engine.get(coins(), b"after").unwrap(), None, "a refused write is not visible");
}

fn chmod(dir: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A table-file write fails (the directory is not writable): the flush error latches, and the
/// engine keeps refusing writes after the directory is writable again. Portable (no mount).
#[test]
fn refuses_writes_after_a_flush_error() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open(dir.path());
    put(&engine, b"durable", b"1", Commit::Durable).unwrap();
    put(&engine, b"buffered", b"2", Commit::Buffered).unwrap();

    chmod(dir.path(), 0o555);
    let flush = engine.flush_memtables();
    chmod(dir.path(), 0o755);
    let err = flush.expect_err("a flush into a read-only directory fails (not running as root?)");
    let original = err.kind().unwrap();
    assert_ne!(original, ErrorKind::ReadOnly, "the first error is reported as itself: {err}");

    assert_refuses_writes(&engine, original);
    // Reads still work, and the latch does not hide what was committed.
    assert_eq!(engine.get(coins(), b"durable").unwrap(), Some(b"1".to_vec()));
    drop(engine);
    let engine = open(dir.path());
    assert_eq!(engine.get(coins(), b"durable").unwrap(), Some(b"1".to_vec()));
    assert_eq!(engine.get(coins(), b"buffered").unwrap(), Some(b"2".to_vec()));
    put(&engine, b"after-reopen", b"3", Commit::Durable).unwrap();
}

/// A write that the write buffer manager holds (`allow_stall`) fails, and latches, when the flush
/// that would release it fails (DEC-164). RocksDB alone never wakes such a writer: the node would
/// hang where Core's LevelDB writer returns the background error and the node aborts. The table
/// files cannot be created (the database directory is read-only) while the write-ahead log can
/// (`wal_dir` elsewhere). Portable (no mount).
#[test]
fn a_stalled_write_fails_when_its_flush_fails() {
    let dir = tempfile::tempdir().unwrap();
    let wal = tempfile::tempdir().unwrap();
    let config = RocksConfig {
        block_cache_bytes: 24 << 20,
        charged_write_buffer_bytes: Some(1 << 20),
        wal_dir: Some(wal.path().to_owned()),
        ..RocksConfig::default()
    };
    let engine = Arc::new(RocksEngine::open(dir.path(), &SCHEMA, &config).unwrap().0);
    // About 2 MiB of memtable, past the write buffer manager's 1 MiB: the next write flushes it
    // and is held until the flush frees the memory. Written through RocksDB itself, so that no
    // flush starts before the directory is read-only: the engine's own write would start it at
    // once (CH2c D42), and one that creates its table file before the `chmod` succeeds, freeing
    // the memory, so the next write goes through, held at most until that flush ends (BL-366).
    // `raw`'s rule against writing through it does not bite here: nothing is latched yet, and
    // with the manager nearly empty this write cannot wait.
    let db = engine.raw();
    let coins_cf = db.cf_handle("coins").unwrap();
    let mut batch = rocksdb::WriteBatch::default();
    for i in 0..20_000u32 {
        batch.put_cf(coins_cf, i.to_be_bytes(), [7; 64]);
    }
    db.write(batch).unwrap();
    let stops_before = write_buffer_manager_stops(&engine);

    chmod(dir.path(), 0o555);
    let (done, written) = std::sync::mpsc::channel();
    let writer = std::thread::spawn({
        let engine = engine.clone();
        move || done.send(put(&engine, b"held", b"x", Commit::Buffered))
    });
    let result = written.recv_timeout(Duration::from_secs(60));
    chmod(dir.path(), 0o755);
    let result = result.expect("the held write returns");
    // The writer's clone of the engine is gone before the test ends. If the writer dropped it
    // last, the database would close on a thread that can outlive the test, as the process
    // exits, and RocksDB aborts on a mutex already destroyed ("pthread lock: Invalid argument").
    writer.join().unwrap().unwrap();
    assert!(write_buffer_manager_stops(&engine) > stops_before, "the write was never held");
    let err = result.expect_err("a flush into a read-only directory fails (not running as root?)");
    // The flush's own error (EACCES on its table file), not the stall or a refusal.
    let original = err.kind().unwrap();
    assert_eq!(original, ErrorKind::Io, "{err}");
    assert_eq!(engine.failed(), Some(original), "the write latched");
    assert_refuses_writes(&engine, original);
}

/// The times a write met the write buffer manager's stall: the engine writes with `no_slowdown`,
/// so each attempt it refused adds one.
fn write_buffer_manager_stops(engine: &RocksEngine) -> u64 {
    // "Write Stall (count): …, write-buffer-manager-limit-stops: N, …"
    let stats = engine.raw().property_value("rocksdb.db-write-stall-stats").unwrap().unwrap();
    let key = "write-buffer-manager-limit-stops: ";
    let at = stats.find(key).unwrap_or_else(|| panic!("{stats}")) + key.len();
    stats[at..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
}

/// A small dedicated filesystem from `DASHD_STORE_SMALL_FS` (the tests fill it), e.g.
/// `sudo mount -t tmpfs -o size=200m,mode=1777 tmpfs <dir>`. The tests using it run one at a
/// time.
fn small_fs() -> (std::sync::MutexGuard<'static, ()>, PathBuf, PathBuf, PathBuf) {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let root =
        PathBuf::from(std::env::var_os("DASHD_STORE_SMALL_FS").expect("DASHD_STORE_SMALL_FS"));
    let db = root.join("fail-stop-db");
    let ballast = root.join("fail-stop-ballast");
    let _ = std::fs::remove_dir_all(&db);
    let _ = std::fs::remove_file(&ballast);
    let free = free_bytes(&root);
    assert!(free > 160 << 20, "the filesystem needs more than 160 MiB free ({free})");
    (guard, root, db, ballast)
}

/// The reviewer's scenario on a real full filesystem: fill a small mount until a write fails,
/// free space well beyond RocksDB's recovery reserve (the largest column family write buffer:
/// 64 MiB, from RocksDB's unconfigured `default` column family), wait past two of its 5-second
/// free-space polls, and the engine still refuses writes. Reopening recovers every acknowledged
/// durable batch.
#[test]
#[ignore = "needs a small dedicated filesystem in DASHD_STORE_SMALL_FS"]
fn refuses_writes_after_disk_full_even_once_space_is_freed() {
    let (_guard, root, db, ballast) = small_fs();
    // Leave ~80 MiB for the database, so the 4 MiB batches fill it within a few seconds.
    fill(&ballast, 80 << 20);

    let engine = open(&db);
    let value = vec![0x5a; 4 << 20];
    let mut acknowledged_durable = Vec::new();
    let mut pending_durable = Vec::new();
    let original = loop {
        let i = acknowledged_durable.len() + pending_durable.len();
        assert!(i < 10_000, "the filesystem never filled");
        let key = (i as u32).to_be_bytes();
        let commit = if i % 8 == 7 {
            Commit::Durable
        } else {
            Commit::Buffered
        };
        match put(&engine, &key, &value, commit) {
            Ok(()) => {
                pending_durable.push(key);
                if commit == Commit::Durable {
                    acknowledged_durable.append(&mut pending_durable);
                }
            }
            Err(e) => break e,
        }
    };
    eprintln!("first failure: {original}");
    assert_eq!(original.kind(), Some(ErrorKind::NoSpace), "{original}");

    std::fs::remove_file(&ballast).unwrap();
    let freed_at = Instant::now();
    assert!(free_bytes(&root) >= 64 << 20, "RocksDB's recovery reserve is free");
    std::thread::sleep(Duration::from_secs(12));
    let refused = put(&engine, b"after", b"x", Commit::Durable).expect_err(&format!(
        "a write {:.1} s after freeing space is accepted without a reopen",
        freed_at.elapsed().as_secs_f64()
    ));
    assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly), "{refused}");
    assert_refuses_writes(&engine, ErrorKind::NoSpace);
    drop(engine);

    let engine = open(&db);
    for key in &acknowledged_durable {
        assert!(engine.get(coins(), key).unwrap().is_some(), "durable batch {key:?} lost");
    }
    assert_eq!(engine.get(coins(), b"after").unwrap(), None);
    put(&engine, b"after-reopen", b"x", Commit::Durable).unwrap();
    drop(engine);
    std::fs::remove_dir_all(&db).unwrap();
}

/// A background flush fails with `ENOSPC` while every write succeeds (they go into the
/// preallocated WAL), and RocksDB recovers by itself once space is freed, before the engine is
/// asked to write again. No caller has seen an error, yet the next write is still refused: a
/// background failure stops writes like a foreground one.
#[test]
#[ignore = "needs a small dedicated filesystem in DASHD_STORE_SMALL_FS"]
fn refuses_writes_after_a_background_error_rocksdb_recovered_from() {
    let (_guard, _, db, ballast) = small_fs();
    let engine = open(&db);
    let value = vec![0x5a; 4 << 20];
    // 24 MiB of the WAL's 32 MiB preallocated block, 12 MiB in each column family's memtable.
    for i in 0..3u32 {
        let mut batch = engine.batch();
        batch.put(DbSchema::META, &i.to_be_bytes(), &value);
        engine.write(batch, Commit::Buffered).unwrap();
        put(&engine, &i.to_be_bytes(), &value, Commit::Buffered).unwrap();
    }
    // Full but for 1 MiB: enough for small log writes, not for a 16 MiB table file.
    fill(&ballast, 1 << 20);
    // Fills the `coins` memtable (the write lands in the preallocated WAL); the next write
    // switches memtables, which frees the WAL's 4 MiB unused tail, and schedules the flush, which
    // cannot write its 16 MiB `coins` table.
    put(&engine, b"fills the memtable", &value, Commit::Buffered)
        .expect("the write fits in the preallocated WAL");
    put(&engine, b"switches the memtable", b"x", Commit::Buffered)
        .expect("a small write fits in the space left");
    let background_errors =
        || engine.raw().property_int_value("rocksdb.background-errors").unwrap().unwrap_or(0);
    let deadline = Instant::now() + Duration::from_secs(30);
    while background_errors() == 0 {
        assert!(Instant::now() < deadline, "the background flush did not fail");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::remove_file(&ballast).unwrap();
    // RocksDB re-checks free space every 5 s and resumes once its reserve is free.
    let deadline = Instant::now() + Duration::from_secs(30);
    let resumed = loop {
        let log = std::fs::read_to_string(db.join("LOG")).unwrap();
        if log.contains("Successfully resumed DB") {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    eprintln!("RocksDB resumed writes by itself: {resumed}");

    // The first write reports the background failure itself; every later one is refused.
    let first = put(&engine, b"after", b"x", Commit::Durable).expect_err("write accepted");
    eprintln!("first write after the background error: {first}");
    assert!(first.to_string().contains("background") || !resumed, "{first}");
    // The cause is read back from RocksDB's LOG: the disk was full.
    assert_eq!(first.kind(), Some(ErrorKind::NoSpace), "{first}");
    assert_refuses_writes(&engine, first.kind().unwrap());
    drop(engine);
    let engine = open(&db);
    assert_eq!(engine.get(coins(), b"fills the memtable").unwrap(), Some(value));
    assert_eq!(engine.get(coins(), b"after").unwrap(), None);
    drop(engine);
    std::fs::remove_dir_all(&db).unwrap();
}

/// Writes `ballast` until the filesystem is full, then frees `leave` bytes of it.
fn fill(ballast: &Path, leave: u64) {
    use std::io::Write;
    let mut file = std::fs::File::create(ballast).unwrap();
    let chunk = vec![0xa5u8; 1 << 20];
    while file.write_all(&chunk).is_ok() {}
    let len = file.metadata().unwrap().len();
    file.set_len(len.saturating_sub(leave)).unwrap();
}

fn free_bytes(path: &Path) -> u64 {
    let out = std::process::Command::new("df")
        .args(["--output=avail", "-B1"])
        .arg(path)
        .output()
        .expect("df");
    let text = String::from_utf8(out.stdout).unwrap();
    text.lines().nth(1).and_then(|l| l.trim().parse().ok()).expect("df output")
}
