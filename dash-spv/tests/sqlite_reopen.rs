//! An SPV store written by an older SQLite reopens with every row intact.
//!
//! `tests/data/sqlite_reopen/` holds an SPV store's database (`kv.sqlite` and its WAL) as the
//! build named in its `PROVENANCE.md` left it, and `expected.txt`, the per-column-family row
//! counts and digests read back from it at generation time. The store was written through
//! `DiskStorageManager` (headers, filter headers, filters, blocks, metadata, overwrites and
//! truncations), then through the engine for the column families that path leaves empty, and
//! closed. A last session made a large commit, checkpointed, and made a small one; its database
//! file and WAL were copied while it was still open, so the WAL holds the small commit's frames
//! past a checkpoint, then stale frames of the large one that recovery must stop before.
//!
//! [`an_older_sqlite_store_reopens`] opens a copy with this build's engine and checks every
//! key and value against `expected.txt`. [`write_the_fixture`] regenerates the fixture.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use dash_kv::{Commit, KvBatch, KvEngine, KvRead};
use dash_kv_sqlite::{SqliteConfig, SqliteEngine, FILE};
use dash_spv::test_utils::{
    BlockHeaderStorage, BlockStorage, DiskStorageManager, FilterHeaderStorage, FilterStorage,
    MetadataStorage, StorageManager, SPV_SCHEMA,
};
use dash_spv::types::{HashedBlock, HashedBlockHeader};
use dash_spv::ClientConfig;
use dashcore::hash_types::FilterHeader;
use dashcore::{Block, Header, Transaction};
use dashcore_hashes::sha256;

/// The WAL beside [`FILE`].
const WAL: &str = "kv.sqlite-wal";

/// The SQLite that last wrote the fixture's database file (its header, offset 96), as
/// `PROVENANCE.md` records: older than the SQLite this build bundles.
const WRITTEN_BY: u32 = 3_051_001;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/sqlite_reopen")
}

/// Per column family, in schema order: its name, its row count and the SHA-256 of its rows
/// (each key and value length-prefixed, in key order); then the SHA-256 of those lines.
fn dump(engine: &impl KvRead) -> String {
    let mut lines = String::new();
    for (i, spec) in SPV_SCHEMA.cfs.iter().enumerate() {
        let cf = SPV_SCHEMA.cf(spec.name).expect("in the schema");
        assert_eq!(cf.index(), i);
        let (mut rows, mut bytes) = (0u64, Vec::new());
        for entry in engine.iter_prefix(cf, &[]) {
            let (k, v) = entry.expect("reads");
            for part in [&k, &v] {
                bytes.extend_from_slice(&(part.len() as u32).to_be_bytes());
                bytes.extend_from_slice(part);
            }
            rows += 1;
        }
        writeln!(lines, "{} {rows} {}", spec.name, sha256::Hash::hash(&bytes)).unwrap();
    }
    let total = sha256::Hash::hash(lines.as_bytes());
    format!("{lines}total {total}\n")
}

/// The fixture's files copied into `dir`, writable whatever the checkout's modes: an open writes
/// to them (the WAL's recovery, `-shm`).
fn copy_fixture(from: &Path, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for name in [FILE, WAL] {
        std::fs::write(dir.join(name), std::fs::read(from.join(name)).unwrap()).unwrap();
    }
}

#[test]
fn an_older_sqlite_store_reopens() {
    let header = std::fs::read(fixture_dir().join(FILE)).unwrap();
    assert_eq!(u32::from_be_bytes(header[96..100].try_into().unwrap()), WRITTEN_BY);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("spv");
    copy_fixture(&fixture_dir(), &dir);
    let (engine, _) =
        SqliteEngine::open(&dir, &SPV_SCHEMA, &SqliteConfig::default()).expect("opens");
    let expected = std::fs::read_to_string(fixture_dir().join("expected.txt")).unwrap();
    assert_eq!(dump(&engine), expected);
}

/// A dummy block at `height` with `txs` empty transactions.
fn block(height: u32, txs: usize) -> HashedBlock {
    HashedBlock::from(Block::dummy(height, vec![Transaction::dummy_empty(); txs]))
}

/// Writes the fixture into `$SQLITE_REOPEN_OUT` (then copied into `tests/data/sqlite_reopen`
/// with a new `PROVENANCE.md`).
#[tokio::test]
#[ignore = "writes the fixture; run with SQLITE_REOPEN_OUT=<dir> and --ignored"]
async fn write_the_fixture() {
    let out = PathBuf::from(std::env::var_os("SQLITE_REOPEN_OUT").expect("SQLITE_REOPEN_OUT"));
    let root = tempfile::tempdir().unwrap();
    let storage = root.path().join("storage");

    // The store's own write path; every commit Durable at `stop`.
    {
        let config = ClientConfig::testnet().with_storage_path(&storage);
        let mut s = DiskStorageManager::new(&config).await.unwrap();
        let headers: Vec<HashedBlockHeader> =
            Header::dummy_batch(0..48).into_iter().map(HashedBlockHeader::from).collect();
        s.store_headers(&headers).await.unwrap();
        s.store_filter_headers_at_height(&FilterHeader::dummy_batch(0..48), 0).await.unwrap();
        let filters = (0..48u32).map(|h| format!("filter {h}").into_bytes()).collect();
        s.store_filters(0, filters).await.unwrap();
        s.store_filter(7, b"filter 7, rewritten").await.unwrap();
        for (height, txs) in [(10, 1), (20, 2), (30, 1), (44, 3)] {
            s.store_block(height, block(height, txs)).await.unwrap();
        }
        s.store_block(20, block(20, 4)).await.unwrap();
        s.store_metadata("fixture_a", b"first").await.unwrap();
        s.store_metadata("fixture_b", b"kept").await.unwrap();
        s.store_metadata("fixture_a", b"second").await.unwrap();
        s.store_last_target_height(100).await.unwrap();
        s.store_last_target_height(120).await.unwrap();
        StorageManager::truncate_above(&mut s, 40).await.unwrap();
        s.stop().await;
    }

    // The column families that path leaves empty, through the engine: puts, an overwrite, a
    // delete and a range delete, each its own commit.
    let spv = storage.join("spv");
    let config = SqliteConfig::default();
    {
        let (engine, _) = SqliteEngine::open(&spv, &SPV_SCHEMA, &config).unwrap();
        let empty: Vec<_> = SPV_SCHEMA
            .cfs
            .iter()
            .map(|spec| SPV_SCHEMA.cf(spec.name).unwrap())
            .filter(|&cf| engine.iter_prefix(cf, &[]).next().is_none())
            .collect();
        let mut b = engine.batch();
        for &cf in &empty {
            for k in 0u8..6 {
                b.put(cf, &[k, cf.index() as u8], format!("row {k} of {}", cf.index()).as_bytes());
            }
        }
        engine.write(b, Commit::Durable).unwrap();
        let mut b = engine.batch();
        for &cf in &empty {
            b.put(cf, &[2, cf.index() as u8], b"rewritten");
            b.delete(cf, &[1, cf.index() as u8]);
            b.delete_range(cf, &[4], &[5]);
        }
        engine.write(b, Commit::Durable).unwrap();
    }
    assert!(!spv.join(WAL).exists(), "the close checkpointed and removed the WAL");

    // A last session: a large commit (a row of every column family rewritten, same length), a
    // checkpoint, a small commit; copied while open. The small one restarts the WAL over the large
    // one's first frames and leaves the rest stale.
    let (engine, _) = SqliteEngine::open(&spv, &SPV_SCHEMA, &config).unwrap();
    let cf = |name| SPV_SCHEMA.cf(name).unwrap();
    let first = |name| engine.iter_prefix(cf(name), &[]).next().unwrap().unwrap().0;
    let mut b = engine.batch();
    for spec in SPV_SCHEMA.cfs {
        let (k, v) = engine.iter_prefix(cf(spec.name), &[]).next().unwrap().unwrap();
        b.put(cf(spec.name), &k, &v.iter().map(|x| x ^ 0x5a).collect::<Vec<_>>());
    }
    engine.write(b, Commit::Durable).unwrap();
    engine.flush_memtables().unwrap();
    let mut b = engine.batch();
    b.put(cf("blocks"), &first("blocks"), b"in the WAL");
    b.delete(cf("cfhdr"), &first("cfhdr"));
    b.put(cf("bans"), b"in the WAL", b"after the checkpoint");
    engine.write(b, Commit::Durable).unwrap();

    std::fs::create_dir_all(&out).unwrap();
    for name in [FILE, WAL] {
        std::fs::copy(spv.join(name), out.join(name)).unwrap();
    }
    let frames = (std::fs::metadata(out.join(WAL)).unwrap().len() - 32) / (4096 + 24);
    assert!(frames > 8, "stale frames follow the last commit's: {frames} frames");
    let expected = dump(&engine);
    drop(engine);
    assert!(
        expected.lines().filter(|l| !l.starts_with("total")).all(|l| !l.contains(" 0 ")),
        "every column family holds rows:\n{expected}"
    );

    // The copy reopens as written.
    let check = root.path().join("check");
    copy_fixture(&out, &check);
    let (reopened, _) = SqliteEngine::open(&check, &SPV_SCHEMA, &config).unwrap();
    assert_eq!(dump(&reopened), expected);
    std::fs::write(out.join("expected.txt"), &expected).unwrap();
}
