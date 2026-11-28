// SPDX-License-Identifier: MIT

//! The basic block filter index through the real data object and driver, against the recorded
//! `getblockfilter` answers of dashd v24.0.0, and its `fltr?????.dat` files.

mod common;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::blockfilter::*;
use dash_chain::BlockRef;
use dash_primitives::BlockHash;
use dashd_chainstate::subscriber::{
    FileAppend, FilePos, SubStep, SubscriberError, SyncProgress, height_key,
};
use dashd_indexes::IndexError;
use dashd_indexes::blockfilter::FilterFiles;
use dashd_indexes::blockfilter::data::{
    ARCHIVE, BEST_BLOCK, BLOCKFILTER_DB, BlockFilterData, CURRENT_VERSION, FILTER_POS, HEIGHT,
    META, VERSION, decode_pos,
};
use dashd_indexes::coinstats::data::COINSTATS_DB;
use dashd_store::flatfile::FlatFs;
use dashd_store::{Commit, KvBatch, KvEngine, KvRead, MemoryEngine};

/// Every block of g, genesis and the reorged-out ones included: the stored filter hash and
/// header, and the filter read back from its file, equal dashd v24.0.0's `getblockfilter`.
#[test]
fn recorded_g_filters_and_headers_equal_dashd() {
    let run = replay(None, None);
    let genesis = run.rec.genesis();
    assert_eq!(run.check(&genesis), (false, FilePos::default()), "genesis at the first position");
    let archived = run.check_all();
    assert!(archived > 0, "reorged-out blocks are served from the archive");
    assert_eq!(run.files().end().unwrap().file, 0, "one file at Core's size");
}

/// The same with a restart every few events: the header chains from the row at L.
#[test]
fn recorded_g_filters_survive_restarts() {
    let run = replay(None, Some(7));
    assert!(run.check_all() > 0);
}

/// Files of 600 bytes in 256-byte chunks: the index rolls over many times, each file is
/// truncated to its last entry when the next is created, and every reference reads back, also
/// with restarts (whose floor skips the last file's preallocated tail).
#[test]
fn small_files_roll_over_and_truncate() {
    let run = replay(Some((600, 256)), None);
    run.check_all();
    let files = run.files();
    let end = files.end().unwrap();
    assert!(end.file > 3, "{end:?}");
    let ends = run.entry_ends();
    for n in 0..end.file {
        let last = ends.iter().filter(|e| e.file == n).map(|e| e.offset).max();
        assert_eq!(files.len(n).unwrap(), last, "file {n} is truncated to its last entry");
        assert!(last <= Some(600), "file {n}: {last:?}");
    }
    let last = ends.iter().filter(|e| e.file == end.file).map(|e| e.offset).max().unwrap();
    assert_eq!(end.offset, last.div_ceil(256) * 256, "the last file keeps its chunk");
    let run = replay(Some((600, 256)), Some(11));
    run.check_all();
    assert!(run.files().end().unwrap().file > 3);
}

fn append(file: u32, offset: u64, len: usize) -> FileAppend {
    FileAppend {
        file,
        offset,
        bytes: vec![0xab; len],
    }
}

/// The rotation rule and Core's truncate-then-move, on the files alone.
#[test]
fn files_rotate_only_past_offset_zero_and_truncate_the_last_file() {
    let fs = Arc::new(MemFs::default());
    let mut files = FilterFiles::new(fs.clone(), PathBuf::from(DIR)).with_sizes(100, 64);
    let at = |file, offset| FilePos {
        file,
        offset,
    };
    assert_eq!(files.start(at(0, 0), 150), at(0, 0), "an entry at a file's start stays there");
    assert_eq!(files.start(at(0, 60), 40), at(0, 60));
    assert_eq!(files.start(at(0, 60), 41), at(1, 0));
    files.append(&append(0, 0, 60)).unwrap();
    assert_eq!(files.len(0).unwrap(), Some(64), "one chunk");
    files.append(&append(1, 0, 70)).unwrap();
    assert_eq!(files.len(0).unwrap(), Some(60), "truncated to its data");
    assert_eq!(files.len(1).unwrap(), Some(128));
    files.append(&append(1, 70, 30)).unwrap();
    assert!(files.append(&append(1, 90, 5)).is_err(), "an overwrite");
    assert!(files.append(&append(3, 0, 5)).is_err(), "a skipped file");
    assert!(files.append(&append(0, 60, 5)).is_err(), "an earlier file");
    files.append(&append(2, 0, 150)).unwrap();
    assert_eq!(files.len(1).unwrap(), Some(100));
    assert_eq!(files.end().unwrap(), at(2, 192));
    files.sync().unwrap();
    fs.fail_writes.store(true, Ordering::Relaxed);
    assert!(files.append(&append(2, 192, 10)).is_err());
    fs.fail_writes.store(false, Ordering::Relaxed);
    assert!(files.append(&append(2, 192, 10)).is_err(), "over the torn bytes");
    assert!(files.append(&append(2, 195, 10)).is_err(), "inside the preallocated chunk");
    files.append(&append(2, 256, 10)).unwrap();
}

fn file_bytes(run: &Run) -> u64 {
    let files = run.files();
    let end = files.end().unwrap();
    (0..=end.file).map(|n| files.len(n).unwrap().unwrap_or(0)).sum()
}

/// A catch-up refused at `D_max` 2 writes no bytes for the refused entry, and its retry after
/// the commit writes at the same position; every offset is written once.
#[test]
fn a_rejected_entry_writes_no_bytes_and_its_retry_reuses_the_offset() {
    let rec = Recorded::load();
    let mut run = Run::new(&rec, Some((1 << 20, 1)));
    let path: Vec<BlockRef> =
        rec.events.iter().take_while(|e| e.0).map(|e| rec.index.by_hash(&e.1)).take(8).collect();
    let mut chain = vec![rec.genesis()];
    chain.extend(path);
    run.reopen(chain.clone(), Some(2));
    let mut refused = 0;
    for _ in 0..100 {
        let (before, writes) = (run.d().progress(), run.fs.writes().len());
        let bytes = file_bytes(&run);
        let step = run.d().sync_step().expect("syncs");
        if run.d().progress() == before && step != SyncProgress::Live {
            refused += 1;
            assert_eq!(run.fs.writes().len(), writes, "a refused entry wrote");
            assert_eq!(file_bytes(&run), bytes);
        }
        if step == SyncProgress::Live {
            break;
        }
    }
    assert!(refused > 0, "no entry was refused at D_max");
    assert_eq!(run.d().progress(), chain.last().copied());
    let writes = run.fs.writes();
    let offsets: HashSet<(PathBuf, u64)> = writes.iter().map(|w| (w.0.clone(), w.1)).collect();
    assert_eq!(offsets.len(), writes.len(), "an offset written twice: {writes:?}");
    for b in &chain {
        run.check(b);
    }
}

/// Above L and below it: after a disconnect, the next connect at that height writes past the
/// archived entry, which still reads back.
#[test]
fn disconnect_then_connect_never_reuses_an_archived_offset() {
    let run = replay(None, None);
    assert!(run.rec.events.iter().any(|e| !e.0), "g has disconnects");
    assert!(run.check_all() > 0);
    let rec = Recorded::load();
    let mut run = Run::new(&rec, None);
    for (i, &(connect, hash)) in rec.events.iter().enumerate() {
        if !connect && rec.events.get(i.wrapping_sub(1)).is_some_and(|e| e.0) {
            run.flush();
        }
        run.event(connect, &hash);
    }
    run.flush();
    assert!(run.check_all() > 0);
}

/// A failed filter write poisons the driver: Core's `FatalError`, then every step refused until
/// a reopen, whose floor skips the torn bytes.
#[test]
fn a_failed_file_write_poisons_the_subscriber_until_a_reopen() {
    let rec = Recorded::load();
    let mut run = Run::new(&rec, None);
    let connects: Vec<BlockHash> = rec.events.iter().take_while(|e| e.0).map(|e| e.1).collect();
    run.event(true, &connects[0]);
    run.flush();
    let durable = run.d().durable();
    let end = run.files().end().unwrap();
    run.fs.fail_writes.store(true, Ordering::Relaxed);
    run.feeds.connect(rec.index.by_hash(&connects[1]));
    let err = loop {
        match run.d().step() {
            Ok(SubStep::Progress) => {}
            Ok(s) => panic!("the connect was not applied: {s:?}"),
            Err(e) => break e,
        }
    };
    let SubscriberError::Fatal(fatal) = &err else {
        panic!("not the fatal class: {err:?}");
    };
    assert!(fatal.0.message.contains("filter file write"), "{fatal:?}");
    assert_eq!(run.files().end().unwrap().offset, end.offset.max(3), "the torn bytes");
    run.fs.fail_writes.store(false, Ordering::Relaxed);
    let writes = run.fs.writes().len();
    assert!(matches!(run.d().step(), Err(SubscriberError::Refused(_))));
    assert!(matches!(run.d().tick(), Err(SubscriberError::Refused(_))));
    assert_eq!(run.fs.writes().len(), writes, "a write after the failure");
    assert_eq!(run.d().durable(), durable);

    let torn = run.files().end().unwrap();
    drop(run.d.take());
    run.db = guarded(copy_store(&run.db));
    let chain = rec.index.path(rec.index.by_hash(&connects[1]));
    run.reopen(chain, None);
    run.sync();
    let b = rec.index.by_hash(&connects[1]);
    let (_, pos) = run.check(&b);
    assert!(pos >= torn, "{pos:?} below the torn end {torn:?}");
}

/// The store's rows on a fresh engine with a fresh latch: a process restart.
fn copy_store(db: &Db) -> MemoryEngine {
    let copy = MemoryEngine::new(&BLOCKFILTER_DB);
    let mut b = copy.batch();
    for cf in BLOCKFILTER_DB.all_cfs() {
        for row in db.iter_prefix(cf, &[]) {
            let (k, v) = row.unwrap();
            if k != dashd_store::SCHEMA_KEY {
                b.put(cf, &k, &v);
            }
        }
    }
    copy.write(b, Commit::Durable).unwrap();
    copy
}

fn put_version(db: &MemoryEngine, v: Option<&[u8]>) {
    let mut b = db.batch();
    match v {
        Some(v) => b.put(META, VERSION, v),
        None => b.delete(META, VERSION),
    }
    db.write(b, Commit::Durable).unwrap();
}

/// `V` missing, unreadable or older wipes the rows and writes the current version; equal keeps
/// them; higher is refused.
// Core: index/blockfilterindex.cpp:119-146
#[test]
fn the_version_row_wipes_or_refuses() {
    let fs: Arc<dyn FlatFs> = Arc::new(MemFs::default());
    let filled = || {
        let db = MemoryEngine::new(&BLOCKFILTER_DB);
        BlockFilterData::<MemoryEngine>::new(&db, fs.clone(), PathBuf::from(DIR)).unwrap();
        let mut b = db.batch();
        b.put(META, BEST_BLOCK, b"best");
        b.put(META, FILTER_POS, &[0, 7]);
        b.put(HEIGHT, &height_key(3, &[]), b"row");
        b.put(ARCHIVE, &[0xff; 32], b"archived");
        db.write(b, Commit::Durable).unwrap();
        db
    };
    let rows = |db: &MemoryEngine| {
        [
            db.get(META, BEST_BLOCK).unwrap().is_some(),
            db.get(META, FILTER_POS).unwrap().is_some(),
            db.get(HEIGHT, &height_key(3, &[])).unwrap().is_some(),
            db.get(ARCHIVE, &[0xff; 32]).unwrap().is_some(),
        ]
    };
    let current = CURRENT_VERSION.to_le_bytes();
    for version in [None, Some(&1i32.to_le_bytes()[..]), Some(&[2][..]), Some(&current[..])] {
        let db = filled();
        put_version(&db, version);
        BlockFilterData::<MemoryEngine>::new(&db, fs.clone(), PathBuf::from(DIR)).unwrap();
        let kept = version == Some(&current[..]);
        assert_eq!(rows(&db), [kept; 4], "{version:?}");
        assert_eq!(db.get(META, VERSION).unwrap().as_deref(), Some(&current[..]));
        assert!(db.get(META, dashd_store::SCHEMA_KEY).unwrap().is_some(), "the schema record");
    }
    let db = filled();
    put_version(&db, Some(&3i32.to_le_bytes()));
    let err = BlockFilterData::<MemoryEngine>::new(&db, fs.clone(), PathBuf::from(DIR))
        .err()
        .expect("refused");
    assert!(matches!(&err, IndexError::Corrupt(m) if m.contains("version 3 is too high")), "{err}");
    assert_eq!(rows(&db), [true; 4]);

    let other = MemoryEngine::new(&COINSTATS_DB);
    assert!(BlockFilterData::<MemoryEngine>::new(&other, fs, PathBuf::from(DIR)).is_err());
}

/// `P` is Core's `FlatFilePos` serialization, and the state at L restores from it and the row
/// at L.
#[test]
fn p_is_cores_flat_file_pos_and_restores_at_l() {
    let run = replay(None, None);
    let p = run.db.get(META, FILTER_POS).unwrap().expect("P");
    let pos = decode_pos(&p).unwrap();
    assert_eq!(Some(pos), run.entry_ends().into_iter().max(), "P past the last entry");
    let o = pos.offset;
    assert!((0x80..0x4080).contains(&o) && pos.file == 0, "a two-byte offset: {pos:?}");
    let core = vec![0, 0x80 | ((o - 0x80) >> 7) as u8, ((o - 0x80) & 0x7f) as u8];
    assert_eq!(p, core, "Core's VARINT(nFile) ‖ VARINT(nPos) at {pos:?}");
}
