//! A-31 (SB4 §3.8): H-M from genesis in SPV at w = 10,000, under a counting allocator. Live
//! bytes after the last step stay within 1.25 × §3.8's bound, with no upward trend over the
//! last 2 million headers.
//!
//! What is counted is the chain's memory: the test's thread counts its allocations, except while
//! stage 3 writes into the `MemHeaderStore`, whose in-memory tables stand in for SB5's on-disk
//! store (§3.8 counts only SB5's 1,024-record cold LRU, which SB4 does not have). A block is
//! freed in the same mode it was allocated in: the store's tables only grow inside that write,
//! and the chain frees only what it allocated.
//!
//! `#[ignore]`d (nightly): `tests/data/headers/fetch.sh`, then
//! `cargo test --release -p dash-chain --features test-util --test spv_memory -- --ignored`.

#![cfg(not(loom))]

mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicI64, Ordering};

use dash_chain::{ChainConfig, Evicting, HeaderChain, MemHeaderStore};
use support::headers::{self, accepted, batches};

/// Live bytes allocated by counting threads.
static LIVE: AtomicI64 = AtomicI64::new(0);

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

struct Counting;

// SAFETY: every call goes to `System` unchanged; only the counter is added.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counting() {
            LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract, passed on.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if counting() {
            LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if counting() {
            LIVE.fetch_add(new_size as i64 - layout.size() as i64, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract, passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Runs `f` with this thread's allocations counted (`on`) or not.
fn with_counting<T>(on: bool, f: impl FnOnce() -> T) -> T {
    let prev = COUNTING.with(|c| c.replace(on));
    let out = f();
    COUNTING.with(|c| c.set(prev));
    out
}

/// §3.8's SPV total at w = 10,000 (an estimate; this test is its measurement).
const BOUND: i64 = 3_500_000;
/// The trend window: the last 2 million headers.
const TREND: u32 = 2_000_000;
/// Room for one sample per step, made before counting starts (H-M takes 1,276 steps).
const SAMPLES: usize = 2_000;

#[test]
#[ignore = "nightly: needs H-M (tests/data/headers/fetch.sh)"]
fn spv_memory_flat_mainnet() {
    let p = headers::params("mainnet");
    let mut store = headers::genesis_store(&p);

    // Made before counting starts: the services, the reader's buffers, and room for every
    // sample.
    let svc = headers::services(&p);
    let input = batches("mainnet");
    let mut samples: Vec<(u32, i64)> = Vec::with_capacity(SAMPLES);
    COUNTING.with(|c| c.set(true));
    let base = LIVE.load(Ordering::Relaxed);
    let mut chain = HeaderChain::<MemHeaderStore, Evicting>::new_genesis(
        p.clone(),
        ChainConfig::spv(&p, 10_000),
        svc,
        store.clone(),
    )
    .expect("genesis");
    with_counting(false, || chain.commit_mem_step(&mut store)).expect("commit");
    chain.records_committed();

    // (height after the step, live bytes) per step.
    let mut top = 0;
    for b in input {
        accepted(&chain.accept(b, None).expect("accept"));
        with_counting(false, || chain.commit_mem_step(&mut store)).expect("commit");
        let to = chain.best_header();
        chain.tip_plan(to).expect("a plan");
        chain.set_active_tip(to).expect("stage 4");
        chain.records_committed();
        top = chain.private_tip().expect("a tip").0.height;
        samples.push((top, LIVE.load(Ordering::Relaxed) - base));
    }
    COUNTING.with(|c| c.set(false));
    assert!(samples.len() <= SAMPLES, "a sample push reallocated while counting");

    let last = samples.last().expect("steps").1;
    let window: Vec<(u32, i64)> =
        samples.iter().copied().filter(|&(h, _)| h > top.saturating_sub(TREND)).collect();
    let peak = window.iter().map(|s| s.1).max().expect("samples");
    // Least squares over the window: bytes per header, times the window's length.
    let n = window.len() as f64;
    let (mx, my) =
        window.iter().fold((0.0, 0.0), |(x, y), &(h, b)| (x + f64::from(h), y + b as f64));
    let (mx, my) = (mx / n, my / n);
    let (sxy, sxx) = window.iter().fold((0.0, 0.0), |(sxy, sxx), &(h, b)| {
        let dx = f64::from(h) - mx;
        (sxy + dx * (b as f64 - my), sxx + dx * dx)
    });
    let growth = sxy / sxx * f64::from(TREND);
    println!(
        "A-31: last {last} B, peak over the last {TREND} headers {peak} B, fitted growth \
         {growth:.0} B over them (bound {BOUND} B × 1.25)"
    );
    assert!(last <= BOUND * 5 / 4, "live bytes {last} > 1.25 × {BOUND}");
    assert!(peak <= BOUND * 5 / 4, "peak {peak} > 1.25 × {BOUND}");
    // No upward trend: the fitted growth stays under one arena chunk (§3.8: 1,024 entries at
    // ~200 B), the granularity the window's edge moves in.
    assert!(
        growth < 1_024.0 * 200.0,
        "live bytes grow {growth:.0} B over the last {TREND} headers"
    );
}
