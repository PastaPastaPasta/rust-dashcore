//! Native regression (ST3a design §6.4, DEC-62 (1)): RocksDB 11.8.1's WAL recovery is not
//! prefix-closed, which is why `FaultyEngine` keeps an arbitrary subset of pending batches.
//!
//! Every case writes through `RocksEngine` with production options, copies the **live**
//! directory (as a power loss would find it: every record handed to the OS, nothing synced
//! after the last barrier), damages one record in the copy, reopens it and reads the keys. A
//! RocksDB upgrade, or a BL-34 prefix check, that changes these outcomes fails here visibly; the
//! model's default then stays subset until ST3b rules otherwise.
//!
//! The record layout: a 32 KiB log block holds records back to back, each with a 7-byte header.
//! A batch is sized so that it ends exactly at the block boundary, so the next batch starts a
//! new block. Zeroing a whole record is what a lost page leaves behind: RocksDB skips a
//! zero-type, zero-length header (`db/log_reader.cc`) and replays the next block.

#![cfg(feature = "rocksdb")]

use std::fs;
use std::path::{Path, PathBuf};

use dashd_store::{
    Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead, RocksConfig, RocksEngine,
};

static SCHEMA: DbSchema = DbSchema {
    name: "walpin",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("a", CfProfile::Ordered)],
};
const A: Cf = Cf::new(1);
const BLOCK: usize = 32 * 1024;

/// A scratch directory on `/dev/shm` (TMPDIR if set).
fn scratch() -> tempfile::TempDir {
    let root = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| "/dev/shm".into());
    tempfile::Builder::new().prefix("st3a-walpin-").tempdir_in(root).unwrap()
}

fn wal(dir: &Path) -> PathBuf {
    let mut logs: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    logs.sort();
    logs.pop().expect("a WAL")
}

fn wal_len(dir: &Path) -> usize {
    fs::metadata(wal(dir)).unwrap().len() as usize
}

fn varint_len(mut v: usize) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

fn write(e: &RocksEngine, ops: &[(&[u8], Option<&[u8]>)], commit: Commit) {
    let mut b = e.batch();
    for (k, v) in ops {
        match v {
            Some(v) => b.put(A, k, v),
            None => b.delete(A, k),
        }
    }
    e.write(b, commit).unwrap();
}

/// Writes `key` with a value sized so that its record ends at `end` in the WAL. The record's
/// fixed overhead is measured with a probe of the same key length, so the layout of RocksDB's
/// column-family records need not be hard-coded. Returns the record's start.
fn put_ending_at(e: &RocksEngine, dir: &Path, key: &[u8], end: usize, commit: Commit) -> usize {
    let before = wal_len(dir);
    let probe = vec![b'p'; key.len()];
    write(e, &[(&probe, Some(&[0u8; 10]))], Commit::Buffered);
    let overhead = wal_len(dir) - before - 10 - 1;
    let start = wal_len(dir);
    let room = end - start;
    let mut v = room - overhead - 1;
    while overhead + varint_len(v) + v > room {
        v -= 1;
    }
    assert_eq!(overhead + varint_len(v) + v, room, "a value length that fills the block exactly");
    write(e, &[(key, Some(&vec![0x42u8; v]))], commit);
    assert_eq!(wal_len(dir), end, "the record ends at the block boundary");
    start
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        fs::copy(e.path(), to.join(e.file_name())).unwrap();
    }
}

/// Copies the live directory, applies `damage` to its WAL, reopens it and reports which of
/// `keys` are present.
fn crash_copy(
    live: &Path,
    crashed: &Path,
    keys: &[&[u8]],
    damage: impl FnOnce(&mut [u8]),
) -> Vec<bool> {
    copy_dir(live, crashed);
    let w = wal(crashed);
    let mut bytes = fs::read(&w).unwrap();
    damage(&mut bytes);
    fs::write(&w, &bytes).unwrap();
    let (e, _) = RocksEngine::open(crashed, &SCHEMA, &RocksConfig::default()).unwrap();
    keys.iter().map(|k| e.get(A, k).unwrap().is_some()).collect()
}

/// Case 1 (non-prefix) and case 2 (checksum mismatch): a Durable baseline, Buffered `b2` filling
/// the rest of the first block, Buffered `b3` and `b4` in the next.
#[test]
fn zeroed_record_is_skipped_and_a_bad_checksum_stops_at_a_prefix() {
    let tmp = scratch();
    let live = tmp.path().join("live");
    let (e, _) = RocksEngine::open(&live, &SCHEMA, &RocksConfig::default()).unwrap();
    write(&e, &[(b"base", Some(b"v"))], Commit::Durable);
    let b2 = put_ending_at(&e, &live, b"b2", BLOCK, Commit::Buffered);
    write(&e, &[(b"b3", Some(b"v"))], Commit::Buffered);
    write(&e, &[(b"b4", Some(b"v"))], Commit::Buffered);
    let keys: [&[u8]; 4] = [b"base", b"b2", b"b3", b"b4"];

    let zeroed = crash_copy(&live, &tmp.path().join("zeroed"), &keys, |w| w[b2..BLOCK].fill(0));
    assert_eq!(zeroed, [true, false, true, true], "RocksDB 11.8.1 recovers {{baseline, b3, b4}}");

    let flipped = crash_copy(&live, &tmp.path().join("flipped"), &keys, |w| w[b2 + 40] ^= 0xff);
    assert_eq!(flipped, [true, false, false, false], "a checksum mismatch stops at a prefix");
    drop(e);
}

/// Cases 3 and 4: a Buffered coin batch `bj`, then the finalizer `bk` (`tip`, the marker
/// erased). A Durable record is byte-identical to a Buffered one until its sync returns
/// (`walbk`), so a Buffered `bk` stands in for an interrupted Durable one.
#[test]
fn a_finalizer_needs_a_barrier_before_it() {
    for barrier in [false, true] {
        let tmp = scratch();
        let live = tmp.path().join("live");
        let (e, _) = RocksEngine::open(&live, &SCHEMA, &RocksConfig::default()).unwrap();
        write(&e, &[(b"marker", Some(b"new,old"))], Commit::Durable);
        let bj = put_ending_at(&e, &live, b"coin", BLOCK, Commit::Buffered);
        if barrier {
            e.sync_wal().unwrap();
        }
        let bk_start = wal_len(&live);
        write(&e, &[(b"tip", Some(b"new")), (b"marker", None)], Commit::Buffered);
        let bk_end = wal_len(&live);
        let keys: [&[u8]; 3] = [b"marker", b"coin", b"tip"];
        if !barrier {
            // Case 3: `bk` kept, `bj` lost: the finalizer presumes a batch that is gone (T26).
            let r = crash_copy(&live, &tmp.path().join("c3"), &keys, |w| w[bj..BLOCK].fill(0));
            assert_eq!(r, [false, false, true]);
        } else {
            // Case 4: behind a completed barrier `bj` is durable (A-DEV), whatever happens to
            // `bk`.
            for (name, hole) in [("kept", 0..0), ("lost", bk_start..bk_end)] {
                let r = crash_copy(&live, &tmp.path().join(name), &keys, |w| w[hole].fill(0));
                assert!(r[1], "bj retained when bk is {name}");
                assert_eq!(r[2], name == "kept");
            }
        }
        drop(e);
    }
}
