use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use dash_kv::ErrorKind;
use dash_kv::codec::ValueCodec;

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

/// What a [`TestFs`] call did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Create(String),
    Write(String),
    Allocate(String),
    Truncate(String),
    SyncData(String),
    SyncDir(String),
    Copy(String),
    Rename(String),
    Remove(String),
}

/// [`OsFs`] that records mutating calls, can fail the n-th of them or every directory sync, and
/// can report a full disk.
#[derive(Default)]
struct TestFs {
    os: OsFs,
    ops: Mutex<Vec<Op>>,
    fail_at: Mutex<Option<usize>>,
    fail_sync_dir: AtomicBool,
    space: Mutex<Option<u64>>,
}

fn name(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

impl TestFs {
    fn record(&self, op: Op) -> io::Result<()> {
        let mut ops = self.ops.lock().unwrap();
        let dir = matches!(op, Op::SyncDir(_)) && self.fail_sync_dir.load(Ordering::Relaxed);
        ops.push(op);
        if dir || *self.fail_at.lock().unwrap() == Some(ops.len()) {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    }

    fn take_ops(&self) -> Vec<Op> {
        std::mem::take(&mut self.ops.lock().unwrap())
    }
}

impl FlatFs for TestFs {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.link_count(path)
    }
    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.len(path)
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        self.record(Op::Create(name(path)))?;
        self.os.create(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        self.os.open_read(path)
    }
    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.os.read_exact_at(path, offset, buf)
    }
    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        self.record(Op::Write(name(path)))?;
        self.os.write_all_at(path, offset, data)
    }
    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        let _ = self.record(Op::Allocate(name(path)));
        self.os.allocate(path, offset, len)
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.record(Op::Truncate(name(path)))?;
        self.os.truncate(path, len)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        self.record(Op::SyncData(name(path)))?;
        self.os.sync_data(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.record(Op::SyncDir(name(dir)))?;
        self.os.sync_dir(dir)
    }
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.record(Op::Copy(name(from)))?;
        self.os.copy(from, to)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.record(Op::Rename(name(to)))?;
        self.os.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<bool> {
        self.record(Op::Remove(name(path)))?;
        self.os.remove(path)
    }
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.os.create_dir_all(dir)
    }
    fn available_space(&self, dir: &Path) -> io::Result<u64> {
        match *self.space.lock().unwrap() {
            Some(n) => Ok(n),
            None => self.os.available_space(dir),
        }
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    fs: Arc<TestFs>,
    cfg: BlockFilesConfig,
}

impl Fixture {
    fn new(fastprune: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("blocks");
        let mut cfg = BlockFilesConfig::new(&dir, MAGIC);
        cfg.fastprune = fastprune;
        Self {
            _tmp: tmp,
            dir,
            fs: Arc::new(TestFs::default()),
            cfg,
        }
    }

    fn open(&self, state: BlockFilesState) -> BlockFiles<TestFs> {
        BlockFiles::with_fs(Arc::clone(&self.fs), self.cfg.clone(), state)
    }

    fn file(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.join(name)).unwrap()
    }

    fn len(&self, name: &str) -> Option<u64> {
        std::fs::metadata(self.dir.join(name)).ok().map(|m| m.len())
    }
}

/// A block-like payload of `len` bytes whose "previous hash" encodes `height - 1`. The tests
/// store it through `save_block_bytes`, the crate's byte path behind `save_block(&CoreBytes)`, so
/// they need no way to make a `CoreBytes` (CS0's has none outside its decoders).
fn block(height: u32, len: usize) -> Vec<u8> {
    let mut b = vec![height as u8; len];
    b[..4].copy_from_slice(&height.to_le_bytes());
    b
}

fn prev(height: u32) -> [u8; 32] {
    [height.wrapping_sub(1) as u8; 32]
}

fn undo(height: u32) -> CoreUndoBytes {
    CoreUndoBytes::from_raw(vec![height as u8; 100])
}

/// Saves `height` and writes its undo, as a node that connects each block on arrival.
fn save_and_connect(
    bf: &mut BlockFiles<TestFs>,
    height: u32,
    len: usize,
) -> (FlatFilePos, FlatFilePos) {
    let pos = bf.save_block_bytes(&block(height, len), height, u64::from(height), None).unwrap();
    let upos = bf.write_undo(&undo(height), pos.file, height, &prev(height)).unwrap();
    (pos, upos)
}

#[test]
fn records_positions_preallocation_and_reads() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    let (p1, u1) = save_and_connect(&mut bf, 1, 300);
    let (p2, u2) = save_and_connect(&mut bf, 2, 500);
    assert_eq!(p1, FlatFilePos::new(0, 8));
    assert_eq!(p2, FlatFilePos::new(0, 8 + 300 + 8));
    assert_eq!(u1, FlatFilePos::new(0, 8));
    assert_eq!(u2, FlatFilePos::new(0, 8 + 100 + 32 + 8));
    // Whole preallocation chunks, zero-filled past the records.
    assert_eq!(fx.len("blk00000.dat"), Some(u64::from(BLOCKFILE_CHUNK_SIZE)));
    assert_eq!(fx.len("rev00000.dat"), Some(u64::from(UNDOFILE_CHUNK_SIZE)));
    let blk = fx.file("blk00000.dat");
    assert_eq!(&blk[..4], &MAGIC);
    assert_eq!(&blk[4..8], &300u32.to_le_bytes());
    assert!(blk[816..].iter().all(|&b| b == 0));
    let rev = fx.file("rev00000.dat");
    assert_eq!(&rev[108..140], &undo_checksum(&prev(1), undo(1).as_slice()));

    let info = *bf.file_info(0).unwrap();
    assert_eq!((info.blocks, info.size, info.undo_size), (2, 816, 280));
    assert_eq!(
        (info.height_first, info.height_last, info.time_first, info.time_last),
        (1, 2, 1, 2)
    );
    let dirty = bf.sync_and_take_dirty_infos().unwrap();
    assert_eq!((dirty.infos, dirty.last_file), (vec![(0, info)], 0));
    assert!(bf.sync_and_take_dirty_infos().unwrap().infos.is_empty());

    let reader = bf.reader();
    let mut buf = Vec::new();
    reader.read_block(p2, &mut buf).unwrap();
    assert_eq!(buf, block(2, 500).as_slice());
    reader.read_undo(u2, &prev(2), &mut buf).unwrap();
    assert_eq!(buf, undo(2).as_slice());
    let err = reader.read_undo(u2, &prev(3), &mut buf).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Corruption);
    assert!(err.to_string().contains("Checksum mismatch"));
    assert_eq!(
        reader.read_block(FlatFilePos::new(0, 4), &mut buf).unwrap_err().kind(),
        ErrorKind::Corruption
    );
}

#[test]
fn chunks_follow_tinyblk_and_fastprune() {
    for (fastprune, tinyblk, blk_chunk, rev_chunk) in [
        (true, false, 0x4000u32, UNDOFILE_CHUNK_SIZE),
        (false, true, 0x1_0000, 0x1_0000),
        (true, true, 0x4000, 0x1_0000),
    ] {
        let mut fx = Fixture::new(fastprune);
        fx.cfg.tinyblk = tinyblk;
        let mut bf = fx.open(BlockFilesState::default());
        save_and_connect(&mut bf, 1, 100);
        assert_eq!(fx.len("blk00000.dat"), Some(u64::from(blk_chunk)));
        assert_eq!(fx.len("rev00000.dat"), Some(u64::from(rev_chunk)));
        // Crossing a chunk boundary allocates the next chunk.
        save_and_connect(&mut bf, 2, blk_chunk as usize);
        assert_eq!(fx.len("blk00000.dat"), Some(2 * u64::from(blk_chunk)));
    }
}

/// -fastprune files hold six 10 008-byte records: the seventh block rolls over.
fn six_per_file(bf: &mut BlockFiles<TestFs>) {
    for h in 1..=6 {
        save_and_connect(bf, h, 10_000);
    }
}

#[test]
fn rollover_finalizes_both_files_when_undo_keeps_up() {
    let fx = Fixture::new(true);
    let mut bf = fx.open(BlockFilesState::default());
    six_per_file(&mut bf);
    let (p7, _) = save_and_connect(&mut bf, 7, 10_000);
    assert_eq!(p7, FlatFilePos::new(1, 8));
    assert_eq!(bf.last_file(), 1);
    assert_eq!(fx.len("blk00000.dat"), Some(6 * 10_008));
    assert_eq!(fx.len("rev00000.dat"), Some(6 * 140));
    assert_eq!(fx.len("blk00001.dat"), Some(0x4000));
}

#[test]
fn restart_before_a_rollover_leaves_the_rev_file_unfinalized() {
    // After a restart no undo has been written into the current file yet, so the rollover does
    // not finalize the rev file.
    let fx = Fixture::new(true);
    let mut bf = fx.open(BlockFilesState::default());
    six_per_file(&mut bf);
    // Shutdown: the barrier (Core's FlushStateToDisk) and the state the index keeps.
    let state = bf.sync_and_state().unwrap();
    let mut bf = fx.open(state);
    save_and_connect(&mut bf, 7, 10_000);
    assert_eq!(fx.len("blk00000.dat"), Some(6 * 10_008));
    assert_eq!(fx.len("rev00000.dat"), Some(u64::from(UNDOFILE_CHUNK_SIZE)));
}

#[test]
fn lagging_undo_is_finalized_by_the_files_last_block() {
    let fx = Fixture::new(true);
    let mut bf = fx.open(BlockFilesState::default());
    let mut files = Vec::new();
    for h in 1..=8 {
        files.push(bf.save_block_bytes(&block(h, 10_000), h, 0, None).unwrap().file);
    }
    // The blk file was finalized at the rollover; the rev file did not exist yet.
    assert_eq!(fx.len("blk00000.dat"), Some(6 * 10_008));
    assert_eq!(fx.len("rev00000.dat"), None);
    for h in 1..=5 {
        bf.write_undo(&undo(h), files[h as usize - 1], h, &prev(h)).unwrap();
    }
    assert_eq!(fx.len("rev00000.dat"), Some(u64::from(UNDOFILE_CHUNK_SIZE)));
    bf.write_undo(&undo(6), 0, 6, &prev(6)).unwrap();
    assert_eq!(fx.len("rev00000.dat"), Some(6 * 140));
}

#[test]
fn known_positions_only_update_statistics() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    let b = block(5, 200);
    bf.save_block_bytes(&b, 5, 50, Some(FlatFilePos::new(2, 1000))).unwrap();
    bf.save_block_bytes(&b, 4, 40, Some(FlatFilePos::new(2, 8))).unwrap();
    assert_eq!(fx.len("blk00002.dat"), None, "nothing is written");
    let info = bf.file_info(2).unwrap();
    assert_eq!((info.blocks, info.size, info.height_first, info.height_last), (2, 1200, 4, 5));
    assert_eq!(bf.last_file(), 2);
}

#[test]
fn fastprune_blocks_of_64_kib_grow_the_file() {
    let fx = Fixture::new(true);
    let mut bf = fx.open(BlockFilesState::default());
    save_and_connect(&mut bf, 1, 100);
    let (p, _) = save_and_connect(&mut bf, 2, 70_000);
    assert_eq!(p, FlatFilePos::new(1, 8), "the big block gets a file of its own");
    let (p, _) = save_and_connect(&mut bf, 3, 100);
    assert_eq!(p, FlatFilePos::new(2, 8));
    assert_eq!(fx.len("blk00001.dat"), Some(70_008));
    assert!(!bf.has_failed());
}

#[test]
fn a_hardlinked_file_is_never_modified() {
    let fx = Fixture::new(true);
    let mut bf = fx.open(BlockFilesState::default());
    for h in 1..=3 {
        save_and_connect(&mut bf, h, 10_000);
    }
    let other = fx.dir.parent().unwrap().join("import");
    std::fs::create_dir(&other).unwrap();
    for f in ["blk00000.dat", "rev00000.dat"] {
        std::fs::hard_link(fx.dir.join(f), other.join(f)).unwrap();
    }
    let before: Vec<Vec<u8>> = ["blk00000.dat", "rev00000.dat"]
        .iter()
        .map(|f| std::fs::read(other.join(f)).unwrap())
        .collect();
    let nlink = |p: PathBuf| std::fs::metadata(p).unwrap().nlink();

    // A sync alone does not modify the file: it stays shared.
    bf.flush_block_file(false, false).unwrap();
    assert_eq!(nlink(fx.dir.join("blk00000.dat")), 2);

    // Appends, allocation, and the rollover's truncation (finalize) all go to private copies.
    for h in 4..=7 {
        save_and_connect(&mut bf, h, 10_000);
    }
    for (f, old) in ["blk00000.dat", "rev00000.dat"].iter().zip(&before) {
        assert_eq!(&std::fs::read(other.join(f)).unwrap(), old, "{f} changed through the link");
        assert_eq!(nlink(other.join(f)), 1);
        assert_eq!(nlink(fx.dir.join(f)), 1);
    }
    assert_eq!(fx.len("blk00000.dat"), Some(6 * 10_008));
    let mut buf = Vec::new();
    bf.reader().read_block(FlatFilePos::new(0, 5 * 10_008 + 8), &mut buf).unwrap();
    assert_eq!(buf, block(6, 10_000).as_slice());
    let ops = fx.fs.take_ops();
    for f in ["blk00000.dat", "rev00000.dat"] {
        let copy = ops.iter().position(|o| *o == Op::Copy(f.into())).unwrap();
        let rename = ops.iter().position(|o| *o == Op::Rename(f.into())).unwrap();
        assert!(
            copy < rename && matches!(ops[rename + 1], Op::SyncDir(_)),
            "copy, sync, rename, then sync the directory"
        );
    }
}

#[test]
fn an_io_failure_stops_all_writes() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    save_and_connect(&mut bf, 1, 100);
    *fx.fs.fail_at.lock().unwrap() = Some(fx.fs.ops.lock().unwrap().len() + 1);
    let err = bf.save_block_bytes(&block(2, 100), 2, 0, None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Io);
    assert!(err.to_string().starts_with("Failed to write block"));
    assert!(bf.has_failed());
    *fx.fs.fail_at.lock().unwrap() = None;
    assert!(matches!(bf.save_block_bytes(&block(2, 100), 2, 0, None), Err(FlatFileError::Failed)));
    assert!(matches!(bf.write_undo(&undo(2), 0, 2, &prev(2)), Err(FlatFileError::Failed)));
    assert!(matches!(bf.sync(), Err(FlatFileError::Failed)));
    // Reads still work.
    let mut buf = Vec::new();
    bf.reader().read_block(FlatFilePos::new(0, 8), &mut buf).unwrap();
}

#[test]
fn a_failed_data_sync_is_fatal_and_not_retried() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    save_and_connect(&mut bf, 1, 100);
    fx.fs.take_ops();
    *fx.fs.fail_at.lock().unwrap() = Some(1);
    let err = bf.sync().unwrap_err();
    assert!(err.to_string().starts_with("Flushing block file to disk failed."));
    *fx.fs.fail_at.lock().unwrap() = None;
    fx.fs.take_ops();
    assert!(matches!(bf.sync(), Err(FlatFileError::Failed)));
    assert!(fx.fs.take_ops().is_empty());
}

/// Core's `Allocate` opens the file after its space check (`flatfile.cpp:67-68`): a file that
/// cannot be created there is an open's failure, apart from a short disk.
#[test]
fn a_file_allocate_cannot_create_is_an_open_failure() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    *fx.fs.fail_at.lock().unwrap() = Some(1);
    let err = bf.save_block_bytes(&block(1, 100), 1, 1, None).unwrap_err();
    assert_eq!(fx.fs.take_ops(), [Op::Create("blk00000.dat".into())]);
    assert!(err.to_string().starts_with("Disk space is too low!: "), "{err}");
    assert_eq!(err.open_failed(), Some(fx.dir.join("blk00000.dat").as_path()));
    assert_eq!(err.kind(), ErrorKind::Io);
    // A failure already marked as an open's keeps its one mark, so its OS code still reads.
    let full = OpenFailed::wrap(io::Error::from_raw_os_error(libc::ENOSPC));
    let marked = FlatFileError::io("open", fx.dir.clone(), full).into_open_failure();
    assert_eq!(marked.kind(), ErrorKind::NoSpace);
}

#[test]
fn low_disk_space_fails_before_writing() {
    let fx = Fixture::new(false);
    *fx.fs.space.lock().unwrap() = Some(MIN_DISK_SPACE + u64::from(BLOCKFILE_CHUNK_SIZE) - 1);
    let mut bf = fx.open(BlockFilesState::default());
    let err = bf.save_block_bytes(&block(1, 100), 1, 0, None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NoSpace);
    assert_eq!(err.to_string(), "Disk space is too low!");
    assert_eq!(fx.len("blk00000.dat"), None);
    assert!(bf.has_failed());
}

#[test]
fn sync_covers_every_written_file_and_the_directory() {
    let fx = Fixture::new(true);
    let mut bf = fx.open(BlockFilesState::default());
    let mut files = Vec::new();
    for h in 1..=8 {
        files.push(bf.save_block_bytes(&block(h, 10_000), h, 0, None).unwrap().file);
    }
    // Undo lagging behind, into the older rev file.
    for h in 1..=3 {
        bf.write_undo(&undo(h), files[h as usize - 1], h, &prev(h)).unwrap();
    }
    fx.fs.take_ops();
    bf.sync().unwrap();
    let ops = fx.fs.take_ops();
    let parent = name(fx.dir.parent().unwrap());
    let dir = || Op::SyncDir("blocks".into());
    let data = |f: &str| Op::SyncData(f.into());
    // Core's FlushBlockFile() first: the current pair (creating rev00001), each with a directory
    // sync (and, the first time for the rev sequence, its parent). Then what Core misses: undo
    // written into the older, lagging rev00000. blk00000 was synced when it was finalized.
    let current_pair =
        vec![data("blk00001.dat"), dir(), Op::Create("rev00001.dat".into()), data("rev00001.dat")];
    let mut want = current_pair.clone();
    want.extend([dir(), Op::SyncDir(parent), data("rev00000.dat")]);
    assert_eq!(ops, want);
    // Afterwards a barrier syncs only the current pair, as Core's flush does.
    bf.sync().unwrap();
    let pair = vec![data("blk00001.dat"), dir(), data("rev00001.dat"), dir()];
    assert_eq!(fx.fs.take_ops(), pair);
    // An append to an older file adds its data sync, without a directory sync.
    bf.write_undo(&undo(4), 0, 4, &prev(4)).unwrap();
    bf.sync().unwrap();
    let mut want = vec![Op::Write("rev00000.dat".into())];
    want.extend(pair);
    want.push(data("rev00000.dat"));
    assert_eq!(fx.fs.take_ops(), want);
}

#[test]
fn the_barrier_creates_an_empty_rev_file_as_core_does() {
    // Core's FlushStateToDisk -> FlushBlockFile() opens the current pair, creating the rev file
    // when the current blk file has no undo yet (blocks ahead of the tip, an interrupted import).
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    bf.save_block_bytes(&block(1, 100), 1, 0, None).unwrap();
    assert_eq!(fx.len("rev00000.dat"), None);
    bf.sync().unwrap();
    assert_eq!(fx.len("rev00000.dat"), Some(0));
}

#[test]
fn pruning_selects_and_unlinks_old_files() {
    let mut fx = Fixture::new(true);
    fx.cfg.prune_mode = true;
    let mut bf = fx.open(BlockFilesState::default());
    for h in 1..=400 {
        save_and_connect(&mut bf, h, 10_000);
    }
    assert!(bf.take_check_for_pruning());
    let last = bf.last_file();
    let files = bf.find_files_to_prune_manual(30, Some(400));
    assert_eq!(files, (0..5).collect::<Vec<_>>(), "files whose last block is at or below 30");
    assert!(files.iter().all(|&f| bf.file_info(f).unwrap().size == 0));
    bf.unlink_pruned_files(&files).unwrap();
    assert_eq!(fx.len("blk00000.dat"), None);
    assert_eq!(fx.len("rev00004.dat"), None);
    assert!(fx.len("blk00005.dat").is_some());
    // Automatic pruning: everything up to MIN_BLOCKS_TO_KEEP below the tip, until under target.
    let files = bf.find_files_to_prune(1, 0, Some(400), i32::MAX, false);
    let keep_from = (400 - MIN_BLOCKS_TO_KEEP) / 6;
    assert_eq!(files, (5..keep_from).collect::<Vec<_>>());
    assert!(files.iter().all(|&f| f < last));
}

/// A file-info state for the prune planners: `n` files with seeded sizes, undo sizes and last
/// heights (some pruned, some out of height order), the last one current.
fn prune_state(n: u32, seed: u64) -> BlockFilesState {
    let mut s = seed;
    let mut next = |m: u64| {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (s >> 33) % m
    };
    let infos = (0..=n)
        .map(|f| {
            let pruned = next(5) == 0;
            BlockFileInfo {
                blocks: 10,
                size: if pruned {
                    0
                } else {
                    1 + next(u64::from(MAX_BLOCKFILE_SIZE)) as u32
                },
                undo_size: if pruned {
                    0
                } else {
                    next(8 << 20) as u32
                },
                height_first: f * 100,
                height_last: f * 100 + next(400) as u32,
                time_first: 0,
                time_last: 0,
            }
        })
        .collect();
    BlockFilesState {
        infos,
        last_file: n,
    }
}

/// The lines logged while `f` runs on this thread: (target, level, message).
fn logged<T>(f: impl FnOnce() -> T) -> (T, Vec<(String, tracing::Level, String)>) {
    struct Capture(Mutex<Vec<(String, tracing::Level, String)>>);
    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        self.0 = format!("{v:?}");
                    }
                }
            }
            let mut m = Message(String::new());
            event.record(&mut m);
            let meta = event.metadata();
            self.0.lock().unwrap().push((meta.target().to_owned(), *meta.level(), m.0));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    keep_callsites_registered();
    let capture = Arc::new(Capture(Mutex::new(Vec::new())));
    let out = tracing::subscriber::with_default(Arc::clone(&capture), f);
    let lines = std::mem::take(&mut *capture.0.lock().unwrap());
    (out, lines)
}

/// BL-479: registers a dispatcher that is never dropped, so a scoped capture is never the only
/// one. With a single registered dispatcher, tracing-core registers a callsite with the default
/// of whichever thread hits it first (`Rebuilder::JustOne`): a test thread without one caches
/// `Interest::never` for every thread, and the capture misses that callsite's lines until the
/// next dispatcher registers. With two, every registration asks all live dispatchers under the
/// dispatcher lock, and a live capture keeps the interest off `never`.
// tracing-core 0.1.36: callsite.rs `Dispatchers::register_dispatch`, `Rebuilder::for_each`
fn keep_callsites_registered() {
    static KEEP: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
}

/// BL-479: a callsite another thread hits first, while `logged` runs, still logs to the capture.
#[test]
fn logged_sees_a_callsite_another_thread_registers_first() {
    fn line() {
        tracing::debug!(target: "bl479", "seen");
    }
    let ((), lines) = logged(|| {
        std::thread::scope(|s| s.spawn(line).join().unwrap());
        line();
    });
    assert_eq!(lines, vec![("bl479".to_owned(), tracing::Level::DEBUG, "seen".to_owned())]);
}

/// Core's `FindFilesToPruneManual`, transcribed with its side effects on `infos`: the oracle the
/// plans and selectors are checked against.
// Core: node/blockstorage.cpp BlockManager::FindFilesToPruneManual
#[expect(clippy::needless_range_loop, reason = "Core's loop, as written")]
fn core_find_files_to_prune_manual(
    infos: &mut [BlockFileInfo],
    last_blockfile: u32,
    manual_prune_height: u32,
    chain_tip_height: i32,
) -> Vec<u32> {
    let mut set = Vec::new();
    if chain_tip_height < 0 {
        return set;
    }
    let last_we_can_prune =
        manual_prune_height.min((chain_tip_height as u32).wrapping_sub(MIN_BLOCKS_TO_KEEP));
    for n in 0..last_blockfile as usize {
        if infos[n].size == 0 || infos[n].height_last > last_we_can_prune {
            continue;
        }
        infos[n] = BlockFileInfo::default();
        set.push(n as u32);
    }
    set
}

/// Core's `FindFilesToPrune`, transcribed likewise; also its `nCurrentUsage` at the end (the
/// usage before any selection where it returns early).
// Core: node/blockstorage.cpp BlockManager::FindFilesToPrune
#[expect(clippy::needless_range_loop, reason = "Core's loop, as written")]
fn core_find_files_to_prune(
    infos: &mut [BlockFileInfo],
    last_blockfile: u32,
    prune_target: u64,
    prune_after_height: u64,
    chain_tip_height: i32,
    prune_height: i32,
    is_ibd: bool,
) -> (Vec<u32>, u64) {
    let mut current_usage: u64 =
        infos.iter().map(|i| u64::from(i.size) + u64::from(i.undo_size)).sum();
    let mut set = Vec::new();
    if chain_tip_height < 0 || prune_target == 0 || chain_tip_height as u64 <= prune_after_height {
        return (set, current_usage);
    }
    let last_we_can_prune = prune_height.min(chain_tip_height - MIN_BLOCKS_TO_KEEP as i32) as u32;
    let mut buffer = u64::from(BLOCKFILE_CHUNK_SIZE) + u64::from(UNDOFILE_CHUNK_SIZE);
    if current_usage + buffer >= prune_target {
        if is_ibd {
            buffer += prune_target / 10;
        }
        for n in 0..last_blockfile as usize {
            let bytes_to_prune = u64::from(infos[n].size) + u64::from(infos[n].undo_size);
            if infos[n].size == 0 {
                continue;
            }
            if current_usage + buffer < prune_target {
                break;
            }
            if infos[n].height_last > last_we_can_prune {
                continue;
            }
            infos[n] = BlockFileInfo::default();
            set.push(n as u32);
            current_usage -= bytes_to_prune;
        }
    }
    (set, current_usage)
}

#[test]
fn prune_plans_select_cores_files_and_change_nothing() {
    // CH2b b2: a plan selects what Core's selector does and touches no file info; the selector
    // (the plan applied) leaves the infos as Core's does. Over seeded states and the selectors'
    // boundaries: tip − MIN_BLOCKS_TO_KEEP's unsigned wrap and signed min, the target and IBD
    // buffers, prune_after_height, pruned files and the current file.
    let fx = Fixture::new(false);
    let tips =
        [None, Some(0), Some(100), Some(287), Some(288), Some(289), Some(1_500), Some(9_000)];
    let room = u64::from(BLOCKFILE_CHUNK_SIZE) + u64::from(UNDOFILE_CHUNK_SIZE);
    let (mut selected, mut unmet) = (0, 0);
    for seed in 0..24 {
        let state = prune_state(12, seed);
        let total: u64 =
            state.infos.iter().map(|i| u64::from(i.size) + u64::from(i.undo_size)).sum();
        for tip in tips {
            let core_tip = tip.map_or(-1, |t| t as i32);
            for manual in [0, 1, 250, 700, 5_000, u32::MAX] {
                let mut core = state.infos.clone();
                let want = core_find_files_to_prune_manual(&mut core, 12, manual, core_tip);
                let planner = fx.open(state.clone());
                let plan = planner.plan_files_to_prune_manual(manual, tip);
                assert_eq!(planner.infos(), &state.infos[..], "a manual plan changed an info");
                assert_eq!((&plan.files, plan.met_target), (&want, true), "seed {seed} {tip:?}");
                // Step 7 (CH2 §4.2) re-tests each file with `eligible` at Core's bound.
                if let Some(t) = tip {
                    let bound = last_prunable_manual(manual, t);
                    let eligible: Vec<u32> =
                        (0..=12).filter(|&f| planner.eligible(f, bound)).collect();
                    assert_eq!(plan.files, eligible, "seed {seed} tip {t} manual {manual}");
                }
                let mut selector = fx.open(state.clone());
                assert_eq!(selector.find_files_to_prune_manual(manual, tip), want);
                assert_eq!(selector.infos(), &core[..]);
                selected += want.len();
            }
            // With the exact boundaries: the usage after each prefix of the files, plus the room.
            let prefixes = state.infos.iter().scan(total, |usage, i| {
                *usage -= u64::from(i.size) + u64::from(i.undo_size);
                Some(*usage + room)
            });
            // `total + room + 1` is met with the room alone and not with the IBD margin too, so
            // it pins where Core adds the margin (inside the prune branch).
            let targets =
                [0, 1, total / 3, total / 2, total, total * 2, total + room, total + room + 1];
            for target in targets.into_iter().chain(prefixes) {
                for (after, prune_height, ibd) in [
                    (0, i32::MAX, false),
                    (0, i32::MAX, true),
                    (1_000, i32::MAX, false),
                    (0, 600, true),
                    (0, 0, false),
                    (0, -5, false),
                ] {
                    let mut core = state.infos.clone();
                    let (want, usage) = core_find_files_to_prune(
                        &mut core,
                        12,
                        target,
                        after,
                        core_tip,
                        prune_height,
                        ibd,
                    );
                    let at = format!("seed {seed} tip {tip:?} target {target} after {after}");
                    let planner = fx.open(state.clone());
                    let plan = planner.plan_files_to_prune(target, after, tip, prune_height, ibd);
                    assert_eq!(planner.infos(), &state.infos[..], "{at}: the plan changed an info");
                    assert_eq!(plan.files, want, "{at}");
                    if let Some(t) = tip {
                        let bound = last_prunable(prune_height, t);
                        assert!(plan.files.iter().all(|&f| planner.eligible(f, bound)), "{at}");
                    }
                    assert_eq!(plan.met_target, usage + room < target, "{at}");
                    let mut selector = fx.open(state.clone());
                    let files = selector.find_files_to_prune(target, after, tip, prune_height, ibd);
                    assert_eq!((&files, selector.infos()), (&want, &core[..]), "{at}");
                    assert_eq!(selector.prune_target_met(target), plan.met_target, "{at}");
                    selected += files.len();
                    unmet += usize::from(!files.is_empty() && !plan.met_target);
                }
            }
        }
    }
    // The grid is not vacuous: it selects files, some short of their target.
    assert!(selected > 1_000 && unmet > 10, "{selected} files selected, {unmet} unmet");
}

#[test]
fn prune_selectors_log_cores_lines_and_plans_log_nothing() {
    let fx = Fixture::new(false);
    let state = BlockFilesState {
        infos: (0..4)
            .map(|f| BlockFileInfo {
                blocks: 1,
                size: 12 << 20,
                undo_size: 4 << 20,
                height_first: f * 10,
                height_last: f * 10 + 9,
                time_first: 0,
                time_last: 0,
            })
            .collect(),
        last_file: 3,
    };
    let manual_line = |n: usize| {
        let line = format!("Prune (Manual): prune_height=19 removed {n} blk/rev pairs");
        ("dashd_store::flatfile::blocks".to_owned(), tracing::Level::INFO, line)
    };
    let auto_line = |target: u64, actual: u64, diff: i64, n: usize| {
        let line = format!(
            "target={target}MiB actual={actual}MiB diff={diff}MiB max_prune_height=19 removed {n} blk/rev pairs"
        );
        ("prune".to_owned(), tracing::Level::DEBUG, line)
    };
    // Core's `chain_tip_height - MIN_BLOCKS_TO_KEEP` is 19 at a tip of 307.
    let tip = Some(19 + MIN_BLOCKS_TO_KEEP);
    let (plan, lines) = logged(|| fx.open(state.clone()).plan_files_to_prune_manual(u32::MAX, tip));
    assert_eq!((plan.files.as_slice(), lines.len()), (&[0, 1][..], 0));
    assert_eq!(logged(|| plan.log()).1, vec![manual_line(2)]);
    let (files, lines) =
        logged(|| fx.open(state.clone()).find_files_to_prune_manual(u32::MAX, tip));
    assert_eq!((files, lines), (vec![0, 1], vec![manual_line(2)]));
    // Automatic pruning to 40 MiB takes the two 16 MiB files at or below height 19 and stops
    // short of its target: 64 − 32 MiB plus the 17 MiB buffer.
    let target = 40 << 20;
    let (plan, lines) =
        logged(|| fx.open(state.clone()).plan_files_to_prune(target, 0, tip, i32::MAX, false));
    assert_eq!((plan.files.as_slice(), plan.met_target, lines.len()), (&[0, 1][..], false, 0));
    let (files, lines) =
        logged(|| fx.open(state.clone()).find_files_to_prune(target, 0, tip, i32::MAX, false));
    assert_eq!((files, lines), (vec![0, 1], vec![auto_line(40, 32, 8, 2)]));
    // Under its target with the buffer, it selects nothing and still logs.
    let (files, lines) =
        logged(|| fx.open(state.clone()).find_files_to_prune(100 << 20, 0, tip, i32::MAX, true));
    assert_eq!((files, lines), (Vec::new(), vec![auto_line(100, 64, 36, 0)]));
    // Core logs nothing where it returns before selecting: no tip, no target, a low tip.
    for (target, after, tip) in [(target, 0, None), (0, 0, tip), (target, 1_000, tip)] {
        let (files, lines) = logged(|| {
            fx.open(state.clone()).find_files_to_prune(target, after, tip, i32::MAX, false)
        });
        assert_eq!((files, lines), (Vec::new(), Vec::new()));
    }
    let (_, lines) = logged(|| fx.open(state.clone()).find_files_to_prune_manual(5, None));
    assert_eq!(lines, Vec::new());
    // A manual prune logs its line when it selects nothing.
    let (_, lines) = logged(|| fx.open(state.clone()).find_files_to_prune_manual(5, Some(5)));
    let line = "Prune (Manual): prune_height=5 removed 0 blk/rev pairs".to_owned();
    assert_eq!(
        lines,
        vec![("dashd_store::flatfile::blocks".to_owned(), tracing::Level::INFO, line)]
    );
}

#[test]
fn eligible_is_the_selectors_per_file_test() {
    let fx = Fixture::new(false);
    for seed in 0..24 {
        let state = prune_state(12, seed);
        let bf = fx.open(state.clone());
        for bound in [0, 150, 650, u32::MAX] {
            // The plan's own bound selects exactly the eligible files, so step 7 with an
            // unchanged bound keeps the whole plan (T39's subset is equality there).
            let tip = bound.saturating_add(MIN_BLOCKS_TO_KEEP);
            let want: Vec<u32> = (0..=12).filter(|&f| bf.eligible(f, bound)).collect();
            assert_eq!(bf.plan_files_to_prune_manual(bound, Some(tip)).files, want);
            for f in 0..=13 {
                let info = state.infos.get(f as usize);
                let expect = f < 12 && info.is_some_and(|i| i.size != 0 && i.height_last <= bound);
                assert_eq!(bf.eligible(f, bound), expect, "seed {seed} file {f} bound {bound}");
            }
        }
        assert!(!bf.eligible(12, u32::MAX), "the current file is never eligible");
        // The bound is inclusive.
        let last = state.infos[3].height_last;
        assert_eq!(bf.eligible(3, last), state.infos[3].size != 0);
        assert!(!bf.eligible(3, last - 1));
    }
}

#[test]
fn last_prunable_keeps_cores_signed_and_unsigned_arithmetic() {
    // Automatic: `int` arithmetic, so a tip below MIN_BLOCKS_TO_KEEP goes negative and the cast
    // to unsigned allows every height; a negative prune height does the same.
    assert_eq!(last_prunable(i32::MAX, 1_000), 1_000 - MIN_BLOCKS_TO_KEEP);
    assert_eq!(last_prunable(500, 1_000), 500);
    assert_eq!(last_prunable(i32::MAX, 287), u32::MAX);
    assert_eq!(last_prunable(-5, 1_000), -5i32 as u32);
    // Manual: unsigned arithmetic, so a tip below MIN_BLOCKS_TO_KEEP leaves the manual height.
    assert_eq!(last_prunable_manual(5, 287), 5);
    assert_eq!(last_prunable_manual(5_000, 1_000), 1_000 - MIN_BLOCKS_TO_KEEP);
    assert_eq!(last_prunable_manual(u32::MAX, 288), 0);
}

#[test]
fn a_rearmed_pruning_check_is_taken_once() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    assert!(!bf.take_check_for_pruning());
    bf.rearm_check_for_pruning();
    assert!(bf.take_check_for_pruning());
    assert!(!bf.take_check_for_pruning());
}

#[test]
fn the_prune_target_is_met_below_it_less_the_next_allocation() {
    let fx = Fixture::new(false);
    let one = |size: u32, undo_size: u32| BlockFilesState {
        infos: vec![BlockFileInfo {
            size,
            undo_size,
            ..BlockFileInfo::default()
        }],
        last_file: 0,
    };
    let room = u64::from(BLOCKFILE_CHUNK_SIZE) + u64::from(UNDOFILE_CHUNK_SIZE);
    let bf = fx.open(one(1_000, 24));
    assert!(!bf.prune_target_met(0), "no target is never met");
    assert!(!bf.prune_target_met(1_024 + room));
    assert!(bf.prune_target_met(1_025 + room));
}

#[test]
fn read_only_fs_refuses_writes() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    let (p, u) = save_and_connect(&mut bf, 1, 100);
    let reader = BlockFileReader::open_read_only(&fx.dir);
    let mut buf = Vec::new();
    reader.read_block(p, &mut buf).unwrap();
    reader.read_undo(u, &prev(1), &mut buf).unwrap();
    let ro = OsFs::read_only();
    let err = ro.write_all_at(&fx.dir.join("blk00000.dat"), 0, b"x").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        ro.remove(&fx.dir.join("blk00000.dat")).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn block_file_info_layout() {
    let info = BlockFileInfo {
        blocks: 1,
        size: 128,
        undo_size: 0,
        height_first: 300,
        height_last: 0x4000,
        time_first: 1,
        time_last: 1_700_000_000,
    };
    // Core VARINTs (serialize.h WriteVarInt): 128 = 80 00, 300 = 81 2c, 0x4000 = ff 00.
    let bytes = info.to_value_bytes();
    assert_eq!(&bytes[..9], &[0x01, 0x80, 0x00, 0x00, 0x81, 0x2c, 0xff, 0x00, 0x01]);
    assert_eq!(BlockFileInfo::decode_value(&bytes).unwrap(), info);
    let mut long = bytes.clone();
    long.push(0);
    assert!(BlockFileInfo::decode_value(&long).is_err());

    let mut out = Vec::new();
    FlatFilePos::new(3, 0x1_0000).encode(&mut out);
    assert_eq!(out, [0x03, 0x82, 0xff, 0x00]);
    assert_eq!(FlatFilePos::decode(&mut out.as_slice()).unwrap(), FlatFilePos::new(3, 0x1_0000));
}

#[test]
fn a_restarted_writer_does_not_trust_what_it_did_not_sync() {
    // Process A writes and is killed before syncing.
    let fx = Fixture::new(false);
    let mut a = fx.open(BlockFilesState::default());
    let (p1, _) = save_and_connect(&mut a, 1, 100);
    // What a durable index could hold; A never reached a barrier.
    let state = BlockFilesState {
        infos: a.infos().to_vec(),
        last_file: a.last_file(),
    };
    drop(a);
    // Process B appends to the file A created: the directory entry is synced too, with its parent.
    let mut b = fx.open(state);
    save_and_connect(&mut b, 2, 100);
    fx.fs.take_ops();
    b.sync().unwrap();
    let ops = fx.fs.take_ops();
    let parent = name(fx.dir.parent().unwrap());
    assert!(ops.contains(&Op::SyncDir("blocks".into())), "{ops:?}");
    assert!(ops.contains(&Op::SyncDir(parent)), "{ops:?}");
    // -reindex registering A's records: their file is synced before an index write needs them.
    let mut c = fx.open(BlockFilesState::default());
    c.save_block_bytes(&block(1, 100), 1, 0, Some(p1)).unwrap();
    fx.fs.take_ops();
    assert_eq!(c.sync_and_take_dirty_infos().unwrap().infos.len(), 1);
    assert!(fx.fs.take_ops().contains(&Op::SyncData("blk00000.dat".into())));
}

#[test]
fn flushing_an_unknown_undo_file_is_refused() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    assert_eq!(bf.flush_undo_file(3, true).unwrap_err().kind(), ErrorKind::Corruption);
    assert_eq!(fx.len("rev00003.dat"), None);
}

#[test]
fn adopted_files_are_synced_before_they_are_referenced() {
    let fx = Fixture::new(true);
    let mut a = fx.open(BlockFilesState::default());
    six_per_file(&mut a);
    save_and_connect(&mut a, 7, 10_000);
    let state = BlockFilesState {
        infos: a.infos().to_vec(),
        last_file: a.last_file(),
    };
    drop(a);
    let mut b = fx.open(state);
    b.adopt_existing_files();
    fx.fs.take_ops();
    b.sync().unwrap();
    let ops = fx.fs.take_ops();
    for f in ["blk00000.dat", "rev00000.dat", "blk00001.dat", "rev00001.dat"] {
        assert!(ops.contains(&Op::SyncData(f.into())), "{f}: {ops:?}");
    }
    assert!(ops.contains(&Op::SyncDir("blocks".into())));
}

#[test]
fn unsynced_handles_are_never_closed() {
    // -fastprune: about 70 blk/rev pairs touched between barriers, more than the handle cache.
    let fx = Fixture::new(true);
    let mut bf = fx.open(BlockFilesState::default());
    for h in 1..=6 * 70 {
        save_and_connect(&mut bf, h, 10_000);
    }
    let (open, dirty) = fx.fs.os.open_handles();
    // Every finalized file was synced at its rollover; the current pair is written, not synced.
    assert!(dirty >= 2 && open >= dirty, "{open} open, {dirty} dirty");
    bf.sync().unwrap();
    assert_eq!(fx.fs.os.open_handles().1, 0);
    // Writes to more files than the cache holds, all unsynced: none may be closed.
    let mut c = fx.open(BlockFilesState::default());
    for n in 0..100u32 {
        c.save_block_bytes(&block(n, 100), n, 0, Some(FlatFilePos::new(n, 8))).unwrap();
    }
    let os = OsFs::new();
    for n in 0..100u32 {
        os.write_all_at(&fx.dir.join(format!("x{n}.dat")), 0, b"x").unwrap();
    }
    assert_eq!(os.open_handles(), (100, 100));
}

#[test]
fn an_inconsistent_state_is_resized_like_core() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState {
        infos: Vec::new(),
        last_file: 3,
    });
    assert_eq!(bf.infos().len(), 4);
    assert!(bf.find_files_to_prune_manual(1000, Some(5000)).is_empty());
}

#[test]
fn unlink_failures_are_logged_and_skipped() {
    let mut fx = Fixture::new(true);
    fx.cfg.prune_mode = true;
    let mut bf = fx.open(BlockFilesState::default());
    for h in 1..=30 {
        save_and_connect(&mut bf, h, 10_000);
    }
    fx.fs.take_ops();
    // The first remove (blk00000.dat) fails: it is left, the rest go, nothing latches.
    *fx.fs.fail_at.lock().unwrap() = Some(fx.fs.ops.lock().unwrap().len() + 1);
    bf.unlink_pruned_files(&[0, 1]).unwrap();
    *fx.fs.fail_at.lock().unwrap() = None;
    assert!(!bf.has_failed());
    assert!(fx.len("blk00000.dat").is_some());
    assert_eq!((fx.len("rev00000.dat"), fx.len("blk00001.dat")), (None, None));
    // The startup scan unlinks it.
    bf.prune_one_block_file(0);
    bf.scan_and_unlink_already_pruned_files(true).unwrap();
    assert_eq!(fx.len("blk00000.dat"), None);
}

/// The directory sync after the removals is best-effort too: its failure is logged, and nothing
/// latches.
#[test]
fn a_failed_directory_sync_after_the_unlink_is_logged_and_skipped() {
    let mut fx = Fixture::new(true);
    fx.cfg.prune_mode = true;
    let mut bf = fx.open(BlockFilesState::default());
    for h in 1..=30 {
        save_and_connect(&mut bf, h, 10_000);
    }
    bf.sync().unwrap();
    fx.fs.take_ops();
    // Four removals, then the directory's sync.
    *fx.fs.fail_at.lock().unwrap() = Some(5);
    let (r, lines) = logged(|| bf.unlink_pruned_files(&[0, 1]));
    *fx.fs.fail_at.lock().unwrap() = None;
    r.unwrap();
    assert_eq!(fx.fs.take_ops().last(), Some(&Op::SyncDir("blocks".into())));
    assert!(!bf.has_failed());
    let warned = |(target, level, line): &(String, tracing::Level, String)| {
        target == "blockstorage"
            && *level == tracing::Level::WARN
            && line.starts_with("Prune: Failed to sync the blocks directory")
    };
    assert!(lines.iter().any(warned), "{lines:?}");
    assert_eq!((fx.len("blk00000.dat"), fx.len("rev00001.dat")), (None, None));
    // The next barrier syncs the directory again.
    bf.sync().unwrap();
    assert_eq!(fx.fs.take_ops().last(), Some(&Op::SyncDir("blocks".into())));
}

/// DEC-212 (BL-124): each `Flush` at the files barrier makes one directory sync, Core's
/// `DirectoryCommit`, and a failure is logged and passed over, never retried. The barrier
/// completes with the files' data synced, and the next barrier syncs `blocks/` again.
#[test]
fn a_failed_directory_sync_at_the_barrier_is_logged_and_not_retried() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    save_and_connect(&mut bf, 1, 300);
    fx.fs.take_ops();
    fx.fs.fail_sync_dir.store(true, Ordering::Relaxed);
    let (r, lines) = logged(|| bf.sync_and_take_dirty_infos());
    fx.fs.fail_sync_dir.store(false, Ordering::Relaxed);
    assert_eq!(r.unwrap().last_file, 0);
    assert!(!bf.has_failed());
    let ops = fx.fs.take_ops();
    for data in ["blk00000.dat", "rev00000.dat"] {
        assert!(ops.contains(&Op::SyncData(data.into())), "{ops:?}");
    }
    // One attempt per `Flush` (the blk and the rev file), as Core's two `DirectoryCommit`s.
    let dir_syncs = ops.iter().filter(|o| **o == Op::SyncDir("blocks".into())).count();
    assert_eq!(dir_syncs, 2, "{ops:?}");
    // The datadir's sync, for `blocks/`'s own entry, is attempted whatever `blocks/`'s result:
    // once per `Flush` until it succeeds (Opus r2 2).
    let parent = Op::SyncDir(name(fx.dir.parent().unwrap()));
    assert_eq!(ops.iter().filter(|o| **o == parent).count(), 2, "{ops:?}");
    let warned: Vec<_> = lines
        .iter()
        .filter(|(target, level, _)| target == "blockstorage" && *level == tracing::Level::WARN)
        .collect();
    assert_eq!(warned.len(), 2, "{lines:?}");
    for w in warned {
        assert!(w.2.starts_with("Failed to sync the blocks directory"), "{lines:?}");
    }
    bf.sync().unwrap();
    let ops = fx.fs.take_ops();
    assert!(ops.contains(&Op::SyncDir("blocks".into())) && ops.contains(&parent), "{ops:?}");
    save_and_connect(&mut bf, 2, 300);
    fx.fs.take_ops();
    bf.sync().unwrap();
    assert!(!fx.fs.take_ops().contains(&parent), "the datadir is synced once");
}

/// A sync of `blocks/` before it exists (the startup scan's, after a power loss lost it) leaves
/// the datadir to sync again, though the datadir's own sync succeeds: the entry `blocks/` gets
/// when it is created is not durable yet.
#[test]
fn a_missing_blocks_directory_leaves_the_datadir_unsynced() {
    let fx = Fixture::new(false);
    let mut seq = super::seq::FlatFileSeq::new(Arc::clone(&fx.fs), fx.dir.clone(), "blk", 1024);
    let parent = Op::SyncDir(name(fx.dir.parent().unwrap()));
    assert!(seq.sync_dir("sync").is_err(), "blocks/ does not exist");
    assert_eq!(fx.fs.take_ops(), [Op::SyncDir("blocks".into()), parent.clone()]);
    seq.write(0, 0, &[&[1]], "write").unwrap();
    fx.fs.take_ops();
    seq.sync_dir("sync").unwrap();
    assert_eq!(fx.fs.take_ops(), [Op::SyncDir("blocks".into()), parent]);
}

/// A sync whose datadir sync alone fails leaves the datadir to sync again, though `blocks/`'s
/// own sync succeeds: the entry is synced until a sync of both succeeds (Opus r3 1).
#[test]
fn a_failed_datadir_sync_alone_is_retried() {
    let fx = Fixture::new(false);
    let mut seq = super::seq::FlatFileSeq::new(Arc::clone(&fx.fs), fx.dir.clone(), "blk", 1024);
    let parent = Op::SyncDir(name(fx.dir.parent().unwrap()));
    seq.write(0, 0, &[&[1]], "write").unwrap();
    fx.fs.take_ops();
    *fx.fs.fail_at.lock().unwrap() = Some(2);
    assert!(seq.sync_dir("sync").is_err(), "the datadir's sync fails");
    *fx.fs.fail_at.lock().unwrap() = None;
    assert_eq!(fx.fs.take_ops(), [Op::SyncDir("blocks".into()), parent.clone()]);
    seq.sync_dir("sync").unwrap();
    assert_eq!(fx.fs.take_ops(), [Op::SyncDir("blocks".into()), parent]);
}

/// Patches the size field of the record at data position `pos` in `path` and extends the file
/// sparsely to cover the claimed record, as a forged or damaged file would.
fn forge_size(path: &Path, pos: u64, size: u32, trailer: u64) {
    use std::os::unix::fs::FileExt;
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.write_all_at(&size.to_le_bytes(), pos - 4).unwrap();
    f.set_len(pos + u64::from(size) + trailer).unwrap();
}

#[test]
fn forged_record_sizes_are_errors_not_allocations() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    let (p, u) = save_and_connect(&mut bf, 1, 300);
    bf.sync().unwrap();
    let reader = bf.reader();
    let mut buf = Vec::new();
    // GPT r2's reproducer: size 0xffffffff over a sparse file that does extend that far.
    for (size, name, pos, trailer) in [
        (u32::MAX, "blk00000.dat", p, 0),
        (MAX_BLOCK_RECORD_SIZE + 1, "blk00000.dat", p, 0),
        (u32::MAX - 32, "rev00000.dat", u, 32),
        (MAX_UNDO_RECORD_SIZE + 1, "rev00000.dat", u, 32),
    ] {
        forge_size(&fx.dir.join(name), u64::from(pos.pos), size, trailer);
        let err = if name.starts_with("blk") {
            reader.read_block(pos, &mut buf).unwrap_err()
        } else {
            reader.read_undo(pos, &prev(1), &mut buf).unwrap_err()
        };
        assert_eq!(err.kind(), ErrorKind::Corruption, "{name} {size}: {err}");
        assert!(err.to_string().contains("exceeds"), "{err}");
        assert!(buf.capacity() < 1 << 20, "nothing near the claimed size was allocated");
    }
}

/// `LoadBlockIndexDB`'s check: a blk file that exists opens, also one only allocated; a missing
/// one, or one that cannot be read, is an open failure naming the file.
#[test]
fn open_block_file_is_cores_open_of_a_blk_file() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    save_and_connect(&mut bf, 1, 300);
    bf.sync().unwrap();
    let reader = bf.reader();
    reader.open_block_file(0).unwrap();
    let err = reader.open_block_file(1).unwrap_err();
    assert_eq!(err.open_failed(), Some(fx.dir.join("blk00001.dat").as_path()), "{err}");
    let blk = fx.dir.join("blk00000.dat");
    std::fs::set_permissions(&blk, std::fs::Permissions::from_mode(0o000)).unwrap();
    // A fresh reader holds no cached handle, so the open is the file system's.
    let fresh = BlockFileReader::open_read_only(&fx.dir);
    let unreadable = fresh.open_block_file(0);
    std::fs::set_permissions(&blk, std::fs::Permissions::from_mode(0o600)).unwrap();
    if !nix_is_root() {
        assert_eq!(unreadable.unwrap_err().open_failed(), Some(blk.as_path()));
    }
}

/// Root opens a file whatever its mode.
fn nix_is_root() -> bool {
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

#[test]
fn a_read_tells_a_failed_open_from_a_failed_read() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    let (p, _) = save_and_connect(&mut bf, 1, 300);
    bf.sync().unwrap();
    let mut buf = Vec::new();
    let err = bf.reader().read_block(FlatFilePos::new(1, p.pos), &mut buf).unwrap_err();
    assert_eq!(err.open_failed(), Some(fx.dir.join("blk00001.dat").as_path()), "{err}");
    let os = OsFs::new();
    let missing = os.read_exact_at(&fx.dir.join("blk00001.dat"), 0, &mut [0]).unwrap_err();
    assert!(OpenFailed::is(&missing));
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert_eq!(missing.to_string(), io::Error::from_raw_os_error(libc::ENOENT).to_string());
    let full = OpenFailed::wrap(io::Error::from_raw_os_error(libc::ENOSPC));
    assert_eq!(FlatFileError::io("open", fx.dir.clone(), full).kind(), ErrorKind::NoSpace);
    // Past the end of a file that opened.
    let blk = fx.dir.join("blk00000.dat");
    let end = std::fs::metadata(&blk).unwrap().len();
    let short = os.read_exact_at(&blk, end, &mut [0]).unwrap_err();
    assert_eq!(short.kind(), io::ErrorKind::UnexpectedEof);
    assert!(!OpenFailed::is(&short));
    bf.reader().read_block(p, &mut buf).unwrap();
}

#[test]
fn oversized_framing_within_the_limit_still_reads() {
    // Core's -reindex accepts a size field larger than the block it holds; the record reads back
    // whole and the block is its prefix.
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    let (p, _) = save_and_connect(&mut bf, 1, 300);
    bf.sync().unwrap();
    forge_size(&fx.dir.join("blk00000.dat"), u64::from(p.pos), MAX_BLOCK_RECORD_SIZE, 0);
    let mut buf = Vec::new();
    bf.reader().read_block(p, &mut buf).unwrap();
    assert_eq!(buf.len(), MAX_BLOCK_RECORD_SIZE as usize);
    assert_eq!(&buf[..300], block(1, 300).as_slice());
}

/// A forged undo size within [`MAX_UNDO_RECORD_SIZE`] (400 MB here) is an error either way: under
/// an address-space limit the fallible reservation fails (`Io`), otherwise the checksum does.
/// Ignored by default (it may allocate 400 MB); run under `prlimit --as=268435456`.
#[test]
#[ignore]
fn forged_undo_size_within_the_bound_is_an_error_not_an_abort() {
    let fx = Fixture::new(false);
    let mut bf = fx.open(BlockFilesState::default());
    let (_, u) = save_and_connect(&mut bf, 1, 300);
    bf.sync().unwrap();
    forge_size(&fx.dir.join("rev00000.dat"), u64::from(u.pos), 400_000_000, 32);
    let err = bf.reader().read_undo(u, &prev(1), &mut Vec::new()).unwrap_err();
    eprintln!("{err}");
    assert!(matches!(err.kind(), ErrorKind::Io | ErrorKind::Corruption), "{err}");
}

/// The slice path writes what the `CoreUndoBytes` path writes: the same records, positions,
/// finalization and file infos, and the same refusal after a failure.
#[test]
fn write_undo_slice_matches_write_undo() {
    let (owned, sliced) = (Fixture::new(false), Fixture::new(false));
    let (mut a, mut b) =
        (owned.open(BlockFilesState::default()), sliced.open(BlockFilesState::default()));
    for h in 1..=4 {
        let pa = a.save_block_bytes(&block(h, 200), h, u64::from(h), None).unwrap();
        let pb = b.save_block_bytes(&block(h, 200), h, u64::from(h), None).unwrap();
        assert_eq!(pa, pb);
        let ua = a.write_undo(&undo(h), pa.file, h, &prev(h)).unwrap();
        let ub = b.write_undo_slice(undo(h).as_slice(), pb.file, h, &prev(h)).unwrap();
        assert_eq!(ua, ub);
    }
    assert_eq!(owned.file("rev00000.dat"), sliced.file("rev00000.dat"));
    assert_eq!(a.file_info(0), b.file_info(0));

    *sliced.fs.fail_at.lock().unwrap() = Some(sliced.fs.ops.lock().unwrap().len() + 1);
    let err = b.write_undo_slice(undo(5).as_slice(), 0, 5, &prev(5)).unwrap_err();
    assert!(err.to_string().starts_with("Failed to write undo data"), "{err}");
    *sliced.fs.fail_at.lock().unwrap() = None;
    assert!(matches!(
        b.write_undo_slice(undo(5).as_slice(), 0, 5, &prev(5)),
        Err(FlatFileError::Failed)
    ));
}

// ---- read_undo_record (CH3 §2.10.2, D45, D46) ----------------------------------------------

/// A decodable `CBlockUndo` of `txs` transactions with `coins` coins each; a script of
/// `script_len` bytes makes the record as large as a test needs.
fn block_undo(txs: usize, coins: usize, script_len: usize) -> dash_consensus::coins::BlockUndo {
    use dash_consensus::coins::{BlockUndo, Coin, TxUndo};
    BlockUndo {
        txs: (0..txs)
            .map(|t| TxUndo {
                coins: (0..coins)
                    .map(|c| {
                        Coin::new(1_000 + c as i64, &vec![0x6a; script_len], 7 + t as u32, c == 0)
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// One rev record of `undo` for block 1, synced; returns its position.
fn undo_record(
    fx: &Fixture,
    undo: &dash_consensus::coins::BlockUndo,
) -> (BlockFiles<TestFs>, FlatFilePos) {
    let mut bf = fx.open(BlockFilesState::default());
    let p = bf.save_block_bytes(&block(1, 300), 1, 1, None).unwrap();
    let u = bf.write_undo_slice(undo.core_bytes().as_slice(), p.file, 1, &prev(1)).unwrap();
    bf.sync().unwrap();
    (bf, u)
}

/// `read_undo_record`'s outcome: the record as owned, or the error's text after Core's
/// `"UndoReadFromDisk: "`.
fn read_record(
    bf: &BlockFiles<TestFs>,
    pos: FlatFilePos,
    buf: &mut Vec<u8>,
) -> Result<dash_consensus::coins::BlockUndo, String> {
    bf.reader()
        .read_undo_record(pos, &prev(1), buf, |u| u.to_block_undo())
        .map_err(|e| e.to_string())
}

fn patch(path: &Path, at: u64, bytes: &[u8]) {
    use std::os::unix::fs::FileExt;
    std::fs::OpenOptions::new().write(true).open(path).unwrap().write_all_at(bytes, at).unwrap();
}

/// D45: the size field is a hint. In or out of bounds, short or long, the record decodes from its
/// position as Core's stream reads it, and the buffer never outgrows the file.
#[test]
fn an_undo_record_reads_whatever_its_size_field_says() {
    let fx = Fixture::new(false);
    let undo = block_undo(3, 4, 30);
    let (bf, u) = undo_record(&fx, &undo);
    let size = undo.core_bytes().len() as u32;
    let rev = fx.dir.join("rev00000.dat");
    let file_len = fx.len("rev00000.dat").unwrap();
    let mut buf = Vec::new();
    assert_eq!(read_record(&bf, u, &mut buf), Ok(undo.clone()));
    assert_eq!(buf.len(), size as usize + 32, "one read of the record and its checksum");
    for forged in [0, 1, size - 1, size + 1, size + 1_000, u32::MAX, MAX_UNDO_RECORD_SIZE] {
        patch(&rev, u64::from(u.pos) - 4, &forged.to_le_bytes());
        assert_eq!(read_record(&bf, u, &mut buf), Ok(undo.clone()), "size field {forged}");
        assert!(buf.capacity() as u64 <= file_len.max(64 * 1024), "size field {forged}");
    }
}

/// The slow path doubles past 64 KiB until the decode ends.
#[test]
fn a_record_past_the_first_read_is_read_by_doubling() {
    let fx = Fixture::new(false);
    let undo = block_undo(40, 25, 200);
    assert!(undo.core_bytes().len() > 3 * 64 * 1024);
    let (bf, u) = undo_record(&fx, &undo);
    patch(&fx.dir.join("rev00000.dat"), u64::from(u.pos) - 4, &u32::MAX.to_le_bytes());
    let mut buf = Vec::new();
    assert_eq!(read_record(&bf, u, &mut buf), Ok(undo));
}

/// A size field shorter than the record Core reads: Core's checksum is the one right after the
/// decoded record, wherever the size field points.
#[test]
fn the_checksum_follows_the_decoded_record_not_the_size_field() {
    let fx = Fixture::new(false);
    let undo = block_undo(2, 2, 10);
    let (bf, u) = undo_record(&fx, &undo);
    let rev = fx.dir.join("rev00000.dat");
    let size = undo.core_bytes().len() as u64;
    // A forged record: a size field of size + 40, with the checksum at size + 40 valid over the
    // longer span. Core never reads it: its checksum is at size, and still valid.
    let mut longer = undo.core_bytes().as_slice().to_vec();
    longer.extend(undo_checksum(&prev(1), undo.core_bytes().as_slice()));
    longer.extend([0x55; 8]);
    patch(&rev, u64::from(u.pos) - 4, &(size as u32 + 40).to_le_bytes());
    patch(&rev, u64::from(u.pos) + size + 40, &undo_checksum(&prev(1), &longer));
    let mut buf = Vec::new();
    assert_eq!(read_record(&bf, u, &mut buf), Ok(undo.clone()));
    // And a valid checksum at the size field's end only is Core's mismatch.
    patch(&rev, u64::from(u.pos) + size, &[0; 32]);
    assert_eq!(read_record(&bf, u, &mut buf), Err("Checksum mismatch".to_owned()));
}

/// §2.10.2's rows: each failure is one class with Core's text.
#[test]
fn undo_read_failures_have_cores_lines() {
    const EOF: &str = "Deserialize or I/O error - AutoFile::read: end of file: iostream error";
    let undo = block_undo(2, 3, 20);
    let size = undo.core_bytes().len() as u64;
    let fresh = || {
        let fx = Fixture::new(false);
        let (bf, u) = undo_record(&fx, &undo);
        (fx, bf, u)
    };
    let mut buf = Vec::new();
    type Damage<'a> = &'a dyn Fn(&Path, u64);
    let cases: [(&str, Damage<'_>, &str); 7] = [
        (
            "a flipped record byte",
            &|rev, at| patch(rev, at + size - 1, &[0x6b]),
            "Checksum mismatch",
        ),
        (
            "a flipped checksum byte",
            &|rev, at| patch(rev, at + size + 5, &[0xee]),
            "Checksum mismatch",
        ),
        (
            "the file cut inside the record",
            &|rev, at| {
                std::fs::File::options().write(true).open(rev).unwrap().set_len(at + 7).unwrap()
            },
            EOF,
        ),
        (
            "the file cut inside the checksum",
            &|rev, at| {
                std::fs::File::options()
                    .write(true)
                    .open(rev)
                    .unwrap()
                    .set_len(at + size + 31)
                    .unwrap()
            },
            EOF,
        ),
        (
            "a non-canonical count",
            &|rev, at| patch(rev, at, &[0xfd, 0x02, 0x00]),
            "Deserialize or I/O error - non-canonical ReadCompactSize(): iostream error",
        ),
        (
            "a count above MAX_SIZE",
            &|rev, at| patch(rev, at, &[0xfe, 0x01, 0x00, 0x00, 0x04]),
            "Deserialize or I/O error - ReadCompactSize(): size too large: iostream error",
        ),
        (
            "a coin height's VARINT past 32 bits",
            // tx 0's count, then its first coin's code: five 0xff bytes overflow nCode.
            &|rev, at| patch(rev, at + 2, &[0xff; 6]),
            "Deserialize or I/O error - ReadVarInt(): size too large: iostream error",
        ),
    ];
    for (name, damage, want) in cases {
        let (fx, bf, u) = fresh();
        damage(&fx.dir.join("rev00000.dat"), u64::from(u.pos));
        assert_eq!(read_record(&bf, u, &mut buf), Err(want.to_owned()), "{name}");
    }
}

/// A read that fails short of the end (`EIO`) is `AutoFile::read`'s throw (`streams.cpp:27`):
/// one more of §2.10.2's failures, logged with Core's text, not a storage fault.
#[test]
fn a_failed_undo_read_is_freads() {
    struct Eio;
    impl FlatRead for Eio {
        fn len(&self) -> io::Result<u64> {
            Ok(1 << 20)
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(5))
        }
    }
    let mut buf = Vec::new();
    let out = read_undo_stream(&Eio, 8, &prev(1), &mut buf, UNDO_STREAM_FIRST_READ, |_| ());
    let e = out.expect_err("no byte reads");
    assert!(matches!(e, UndoReadError::Read(UndoReadFailure::Io(_))), "{e:?}");
    assert_eq!(
        e.to_string(),
        "Deserialize or I/O error - AutoFile::read: fread failed: iostream error"
    );
}

/// D46: the read opens the rev file by path. A file unlinked under the reader's cached handle
/// does not open, with Core's two lines' material; a replaced one reads the new file.
#[test]
fn an_undo_read_opens_the_file_by_path() {
    let fx = Fixture::new(false);
    let undo = block_undo(1, 1, 5);
    let (bf, u) = undo_record(&fx, &undo);
    let rev = fx.dir.join("rev00000.dat");
    let reader = bf.reader();
    let mut buf = Vec::new();
    // The old read caches a handle, which still reads the unlinked file.
    reader.read_undo(u, &prev(1), &mut buf).unwrap();
    let saved = fx.file("rev00000.dat");
    std::fs::remove_file(&rev).unwrap();
    reader.read_undo(u, &prev(1), &mut buf).unwrap();
    let err = reader.read_undo_record(u, &prev(1), &mut buf, |_| ()).unwrap_err();
    assert_eq!(err.open_failed(), Some(rev.as_path()));
    assert_eq!(err.to_string(), "OpenUndoFile failed");
    // Replaced by a file whose record differs: the new file is read.
    let other = block_undo(1, 1, 6);
    let mut bytes = saved;
    let at = u.pos as usize;
    bytes.truncate(at - 4);
    bytes.extend((other.core_bytes().len() as u32).to_le_bytes());
    bytes.extend(other.core_bytes().as_slice());
    bytes.extend(undo_checksum(&prev(1), other.core_bytes().as_slice()));
    let tmp = fx.dir.join("rev.tmp");
    std::fs::write(&tmp, &bytes).unwrap();
    std::fs::rename(&tmp, &rev).unwrap();
    assert_eq!(read_record(&bf, u, &mut buf), Ok(other));
}

/// The record at a position past the end, or inside the file header, reads as Core's stream
/// does: from that byte.
#[test]
fn odd_positions_read_as_a_stream() {
    let fx = Fixture::new(false);
    let undo = block_undo(1, 1, 5);
    let (bf, u) = undo_record(&fx, &undo);
    let end = fx.len("rev00000.dat").unwrap() as u32;
    let mut buf = Vec::new();
    let eof = "Deserialize or I/O error - AutoFile::read: end of file: iostream error";
    assert_eq!(
        read_record(&bf, FlatFilePos::new(u.file, end + 100), &mut buf),
        Err(eof.to_owned())
    );
    assert!(read_record(&bf, FlatFilePos::new(u.file, 0), &mut buf).is_err());
}
