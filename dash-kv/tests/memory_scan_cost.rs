//! BL-662: a prefix scan of the in-memory engine costs what the caller pulls, not the whole tail
//! after the start (D25's bound, as RocksDB's lazy iterators give it).
//!
//! Its own test binary, because it counts this thread's heap allocations through a global
//! allocator: wall-clock time is noise on a loaded machine.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use dash_kv::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead, MemoryEngine};

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: forwards every call to `System` unchanged; it only counts allocations made on this
// thread (a const thread-local `Cell` needs no allocation and no destructor).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        let _ = BYTES.try_with(|n| n.set(n.get() + layout.size() as u64));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        let _ = BYTES.try_with(|n| n.set(n.get() + new_size as u64));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The allocations (count, bytes) `f` makes on this thread.
fn allocations<T>(f: impl FnOnce() -> T) -> (u64, u64, T) {
    let (n0, b0) = (ALLOCS.get(), BYTES.get());
    let out = f();
    (ALLOCS.get() - n0, BYTES.get() - b0, out)
}

static SCHEMA: DbSchema = DbSchema {
    name: "bl662-scan-cost",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("index", CfProfile::Ordered)],
};

const ROWS: u32 = 100_000;
const PREFIX: &[u8] = b"q";

fn key(i: u32) -> Vec<u8> {
    [PREFIX, &i.to_be_bytes()].concat()
}

/// 100k rows under one prefix (and a few around it), all committed.
fn engine() -> (MemoryEngine, Cf) {
    let engine = MemoryEngine::new(&SCHEMA);
    let cf = SCHEMA.cf("index").unwrap();
    let mut batch = engine.batch();
    batch.put(cf, b"a", b"below");
    for i in 0..ROWS {
        batch.put(cf, &key(i), &[0x5a; 32]);
    }
    batch.put(cf, b"z", b"above");
    engine.write(batch, Commit::Durable).unwrap();
    (engine, cf)
}

/// Taking `take` rows of a scan allocates at most a few blocks per row, and nowhere near the
/// tail's bytes (100k rows of 5 + 32 bytes): the scan clones only what the caller pulls.
fn assert_lazy(what: &str, take: usize, scan: impl FnOnce() -> Vec<(Vec<u8>, Vec<u8>)>) {
    let (count, bytes, rows) = allocations(scan);
    assert_eq!(rows.len(), take, "{what}");
    let limit = 16 + 4 * take as u64;
    assert!(count <= limit, "{what}: {count} allocations ({bytes} bytes) for {take} rows");
    assert!(bytes < 64 * 1024, "{what}: {bytes} bytes for {take} rows");
}

fn pull(
    take: usize,
    iter: impl Iterator<Item = dash_kv::Result<(Vec<u8>, Vec<u8>)>>,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    iter.take(take).map(Result::unwrap).collect()
}

#[test]
fn a_prefix_scan_clones_only_the_rows_pulled() {
    let (engine, cf) = engine();
    let start = key(10);
    for take in [1, 3] {
        assert_lazy("engine iter_prefix", take, || pull(take, engine.iter_prefix(cf, PREFIX)));
        assert_lazy("engine iter_prefix_from", take, || {
            pull(take, engine.iter_prefix_from(cf, PREFIX, &start))
        });
        let snapshot = engine.snapshot();
        assert_lazy("snapshot iter_prefix", take, || pull(take, snapshot.iter_prefix(cf, PREFIX)));
        assert_lazy("snapshot iter_prefix_from", take, || {
            pull(take, snapshot.iter_prefix_from(cf, PREFIX, &start))
        });
    }
    // A live scan holds its column family's map only while it is alive: once it is dropped, part
    // way or exhausted, a write copies nothing (a held map would make the write copy all 100k
    // rows, copy-on-write). The first write after a barrier copies the map anyway (the durable
    // state shares it), so one goes first.
    let write = |value: &[u8]| {
        let mut batch = engine.batch();
        batch.put(cf, &key(ROWS + 1), value);
        engine.write(batch, Commit::Buffered).unwrap();
    };
    write(b"unshares the durable map");
    assert!(allocations(|| write(b"again")).0 <= 16, "a write with nothing holding the map");
    for take in [1, ROWS as usize + 2] {
        let rows = pull(take, engine.iter_prefix_from(cf, PREFIX, &start));
        assert!(!rows.is_empty());
        let rows = pull(take, engine.iter_prefix(cf, PREFIX));
        assert!(!rows.is_empty());
        let (count, bytes, ()) = allocations(|| write(b"later"));
        assert!(count <= 16 && bytes < 64 * 1024, "a write after a scan: {count} ({bytes} bytes)");
    }
    // The rows pulled are the right ones.
    assert_eq!(pull(1, engine.iter_prefix_from(cf, PREFIX, &start))[0].0, start);
    assert_eq!(pull(1, engine.snapshot().iter_prefix_from(cf, PREFIX, &start))[0].0, start);
}
