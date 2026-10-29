//! Migrator v3's memory is flat in the chain's length (SB0 §9.5, SB13 D10): a version-2 store
//! ten times longer migrates within the same peak heap and (on Linux) the same peak resident
//! set, which counts SQLite's own allocations too, and a mainnet-sized one (`#[ignore]`d, run
//! for the record) too.

// The serialising lock is held by synchronous tests, with no runtime.
#![allow(clippy::disallowed_types)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use dash_spv::storage::{BlockHeaderStorage, DiskStorageManager, FilterHeaderStorage};
use dash_spv::test_utils::v2_fixture::{synthetic_hash, V2Fixture};
use dash_spv::ClientConfig;
use dashcore_hashes::Hash;

/// Counts the live heap and its peak.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// One measurement at a time: the counters are process-wide.
static SERIAL: Mutex<()> = Mutex::new(());

/// The peak resident set (`VmHWM`) above the resident set at its reset, in bytes: SQLite
/// allocates outside the counting allocator. Linux only.
struct PeakRss(Option<u64>);

impl PeakRss {
    fn status(field: &str) -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with(field))?;
        line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb << 10)
    }

    /// Resets the peak to the current resident set (`clear_refs` 5).
    fn reset() -> Self {
        let reset = std::fs::write("/proc/self/clear_refs", "5").ok();
        Self(reset.and_then(|()| Self::status("VmRSS:")))
    }

    fn peak(&self) -> Option<u64> {
        Some(Self::status("VmHWM:")?.saturating_sub(self.0?))
    }
}

/// What one migration took above its start: the peak heap, and the peak resident set where
/// the platform reports it.
#[derive(Clone, Copy, Debug)]
struct Peaks {
    heap: usize,
    rss: Option<u64>,
}

/// SQLite's page caches, which fill up to their cap in a larger store: the writer's and each
/// reader's (`SqliteConfig::default`: 4 MiB each, two readers off mobile).
const SQLITE_CACHES: u64 = 3 * (4 << 20);

impl Peaks {
    /// Asserts `self` (a larger migration) stays within `slack` of `small`'s peak heap, and
    /// within `slack` plus [`SQLITE_CACHES`] of its peak resident set.
    fn within(self, small: Peaks, slack: u64) {
        assert!(
            (self.heap as u64) < small.heap as u64 + slack,
            "peak heap grew from {} to {} bytes",
            small.heap,
            self.heap
        );
        if let (Some(small), Some(large)) = (small.rss, self.rss) {
            let bound = small + slack + SQLITE_CACHES;
            assert!(large < bound, "peak RSS grew from {small} to {large} bytes");
        }
    }
}

/// A version-2 store of `n` headers, from height 0.
fn v2_store(n: u32) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    V2Fixture {
        lo: 0,
        hi: n,
        filter_every: 10,
        blocks: vec![1, n / 2],
    }
    .write(dir.path())
    .unwrap();
    dir
}

/// Writes a version-2 store of `n` headers (from height 0) and opens it, which migrates it.
fn migrate(n: u32) -> Peaks {
    let dir = v2_store(n);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let rss = PeakRss::reset();
    let started = Instant::now();
    let config = ClientConfig::mainnet().with_storage_path(dir.path());
    let peak = rt.block_on(async {
        let mut storage = DiskStorageManager::new(&config).await.unwrap();
        let peak = Peaks {
            heap: PEAK.load(Ordering::Relaxed) - base,
            rss: rss.peak(),
        };
        assert_eq!(storage.get_tip_height().await, Some(n - 1));
        assert_eq!(storage.get_start_height().await, Some(0));
        let tip = storage.get_tip().await.unwrap();
        assert_eq!(*tip.hash().as_byte_array(), synthetic_hash(n - 1));
        assert_eq!(storage.get_filter_tip_height().await.unwrap(), Some(n - 1));
        dash_spv::storage::StorageManager::stop(&mut storage).await;
        peak
    });
    for folder in ["filter_headers", "filters", "blocks", "masternodes"] {
        assert!(!dir.path().join(folder).exists(), "{folder} removed");
    }
    // The store's own headers: version 2's segments under the marker (SB14 D11).
    assert!(dir.path().join("block_headers/tip").exists(), "the marker");
    let mib = |b: u64| b as f64 / (1 << 20) as f64;
    eprintln!(
        "migrated {n} headers in {:.1} s, peak heap {:.1} MiB, peak RSS {}",
        started.elapsed().as_secs_f64(),
        mib(peak.heap as u64),
        peak.rss.map_or("n/a".into(), |b| format!("{:.1} MiB", mib(b)))
    );
    peak
}

#[test]
fn migration_memory_does_not_grow_with_the_chain() {
    let _serial = SERIAL.lock().unwrap();
    // The large one first: its baseline holds no memory the allocator kept from another run,
    // and the small one's peak can only come out lower, which makes the comparison stricter.
    let large = migrate(200_000);
    let small = migrate(20_000);
    // The large store's headers alone are 22 MB of segment files (and its filter headers
    // 6 MB more): a migration that held them would exceed the small peaks by that much.
    large.within(small, 8 << 20);
}

/// Mainnet's height (about 2.4 M headers), for the record: SB13's evidence.
#[test]
#[ignore = "mainnet-sized: minutes and ~400 MB of fixture"]
fn mainnet_sized_migration() {
    let _serial = SERIAL.lock().unwrap();
    let mainnet = migrate(2_400_000);
    let small = migrate(20_000);
    mainnet.within(small, 8 << 20);
}

/// The BG row `spv-startup` (open-to-ready with 2.55 M stored headers, SB0 §8), until SB17
/// adds the cell: a migrated store of 2.55 M headers reopens and reads its tips, three times,
/// for the record: SB13's evidence.
#[test]
#[ignore = "mainnet-sized: minutes and ~400 MB of fixture"]
fn startup_with_mainnet_headers() {
    let _serial = SERIAL.lock().unwrap();
    let n = 2_550_000;
    let dir = v2_store(n);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let config = ClientConfig::mainnet().with_storage_path(dir.path());
    rt.block_on(async {
        let mut storage = DiskStorageManager::new(&config).await.unwrap();
        dash_spv::storage::StorageManager::stop(&mut storage).await;
    });
    for _ in 0..3 {
        let base = LIVE.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let started = Instant::now();
        let ready = rt.block_on(async {
            let mut storage = DiskStorageManager::new(&config).await.unwrap();
            assert_eq!(storage.get_tip_height().await, Some(n - 1));
            assert_eq!(storage.get_filter_tip_height().await.unwrap(), Some(n - 1));
            let ready = started.elapsed();
            dash_spv::storage::StorageManager::stop(&mut storage).await;
            ready
        });
        eprintln!(
            "spv-startup: {n} headers ready in {:.3} s, peak heap {:.1} MiB",
            ready.as_secs_f64(),
            (PEAK.load(Ordering::Relaxed) - base) as f64 / (1 << 20) as f64
        );
    }
}
