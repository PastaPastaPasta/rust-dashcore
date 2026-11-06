//! Block-file hot paths (A1 Appendix E): appending blocks with their undo records, and reading
//! them back by position. No durability barrier is timed (the flush protocol owns those); files
//! live in `ST2_BENCH_DIR` (default: the system temp dir), and a tmpfs keeps the numbers about
//! our code rather than the disk.
//!
//! Run with `cargo bench -p dashd-store --features raw-bytes --bench flatfile`.

// Benches measure production only (PROCESS.md): ST3a's power-loss model must never be built into
// one. `not(clippy)`: the gate's clippy builds every target with `--all-features`.
#[cfg(all(feature = "faulty", not(clippy)))]
compile_error!("dashd-store's benches must not build with the test-only `faulty` feature");

use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use dashd_store::flatfile::{
    BlockFiles, BlockFilesConfig, BlockFilesState, CoreBytes, CoreUndoBytes, FlatFilePos,
};

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
/// Blocks per fresh directory, so a long measurement does not fill the disk.
const BATCH: u64 = 2_000;

/// A block of exactly `len` bytes, as the writer takes it: a zero header and one coinbase whose
/// `OP_RETURN` output is padded to size, decoded by CS0 (the one place the bench makes a
/// `CoreBytes`).
fn block_of(len: usize) -> CoreBytes {
    // Header, tx count, version, input (count, prevout, 2-byte scriptSig, sequence), output count,
    // value, lock time: everything but the output script and its size.
    const FIXED: usize = 80 + 1 + 4 + 1 + 36 + 3 + 4 + 1 + 8 + 4;
    let (width, script_len) = [(1, 252), (3, 0xffff), (5, usize::MAX)]
        .into_iter()
        .map(|(w, max)| (w, len.saturating_sub(FIXED + w), max))
        .find(|&(_, n, max)| n <= max)
        .map(|(w, n, _)| (w, n))
        .unwrap();
    let mut b = vec![0u8; 80];
    b.push(1);
    b.extend(1u32.to_le_bytes());
    b.push(1);
    b.extend([0u8; 32]);
    b.extend(u32::MAX.to_le_bytes());
    b.extend([2, 0x51, 0x51]);
    b.extend(u32::MAX.to_le_bytes());
    b.push(1);
    b.extend(0i64.to_le_bytes());
    match width {
        1 => b.push(script_len as u8),
        3 => {
            b.push(0xfd);
            b.extend((script_len as u16).to_le_bytes());
        }
        _ => {
            b.push(0xfe);
            b.extend((script_len as u32).to_le_bytes());
        }
    }
    b.push(0x6a);
    b.resize(b.len() + script_len - 1, 0);
    b.extend(0u32.to_le_bytes());
    assert_eq!(b.len(), len);
    let mut input = b.as_slice();
    let bytes = dash_consensus::decode_block(&mut input).unwrap().core_bytes();
    assert_eq!(bytes.as_slice(), b.as_slice());
    bytes
}

fn scratch() -> tempfile::TempDir {
    let root = std::env::var("ST2_BENCH_DIR").map_or_else(|_| std::env::temp_dir(), PathBuf::from);
    tempfile::Builder::new().prefix("flatfile-bench-").tempdir_in(root).unwrap()
}

fn open(dir: &std::path::Path) -> BlockFiles {
    BlockFiles::open(BlockFilesConfig::new(dir.join("blocks"), MAGIC), BlockFilesState::default())
}

fn append(c: &mut Criterion) {
    let mut g = c.benchmark_group("flatfile_append");
    for (name, block_len, undo_len) in [("small", 2_000, 400), ("full_2mb", 2_000_000, 300_000)] {
        let block = block_of(block_len);
        let undo = CoreUndoBytes::from_raw(vec![9u8; undo_len]);
        g.throughput(Throughput::Bytes((block_len + undo_len) as u64));
        g.bench_function(name, |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                let mut done = 0;
                while done < iters {
                    let dir = scratch();
                    let mut bf = open(dir.path());
                    let n = (iters - done).min(BATCH * 2_000 / block_len as u64).max(1);
                    let start = Instant::now();
                    for h in 1..=n as u32 {
                        let pos = bf.save_block(&block, h, 0, None).unwrap();
                        black_box(bf.write_undo(&undo, pos.file, h, &[0; 32]).unwrap());
                    }
                    total += start.elapsed();
                    done += n;
                }
                total
            })
        });
    }
    g.finish();
}

fn read(c: &mut Criterion) {
    let mut g = c.benchmark_group("flatfile_read");
    let dir = scratch();
    let mut bf = open(dir.path());
    let block = block_of(20_000);
    let undo = CoreUndoBytes::from_raw(vec![9u8; 4_000]);
    let mut pos: Vec<(FlatFilePos, FlatFilePos)> = Vec::new();
    for h in 1..=BATCH as u32 {
        let p = bf.save_block(&block, h, 0, None).unwrap();
        pos.push((p, bf.write_undo(&undo, p.file, h, &[0; 32]).unwrap()));
    }
    let reader = bf.reader();
    let mut buf = Vec::new();
    let mut i = 0;
    g.throughput(Throughput::Bytes(20_000));
    g.bench_function("block_20kb", |b| {
        b.iter(|| {
            i = (i + 7919) % pos.len();
            reader.read_block(pos[i].0, &mut buf).unwrap();
            black_box(&buf);
        })
    });
    g.throughput(Throughput::Bytes(4_000));
    g.bench_function("undo_4kb_checksummed", |b| {
        b.iter(|| {
            i = (i + 7919) % pos.len();
            reader.read_undo(pos[i].1, &[0; 32], &mut buf).unwrap();
            black_box(&buf);
        })
    });
    g.finish();
}

criterion_group!(benches, append, read);
criterion_main!(benches);
