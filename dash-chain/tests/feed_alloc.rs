//! DEC-152's allocation contract (SB5 §4.5, §10.1): no slot critical section allocates or frees,
//! and §6.1's per-feed buffers are what `register` reserves. A counting global allocator sees
//! each allocation and free a thread makes while `dash_chain` reports it inside a slot section.
//! It lives here because `dash-chain` forbids `unsafe`.

#![cfg(not(loom))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dash_chain::test_util::{SlotHarness, in_slot_section};
use dash_chain::{
    BlockId, BlockRef, ChainEvent, ControlKind, ControlMsg, FEED_FIXED_BYTES, FeedConfig,
    FeedReader, Next, Notify, Parker, SubscriberFeed, TipStep, per_feed_cap,
};
use dash_primitives::BlockHash;

thread_local! {
    /// (allocations, frees) this thread made inside slot sections.
    static IN_SECTION: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    /// Bytes this thread allocated while `MEASURE` was set.
    static MEASURE: Cell<bool> = const { Cell::new(false) };
}

static MEASURED_BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;

#[global_allocator]
static COUNTING: Counting = Counting;

fn record(alloc: bool, bytes: usize) {
    if in_slot_section() {
        let _ = IN_SECTION.try_with(|c| {
            let (a, f) = c.get();
            c.set(if alloc {
                (a + 1, f)
            } else {
                (a, f + 1)
            });
        });
    }
    if alloc && MEASURE.try_with(Cell::get).unwrap_or(false) {
        MEASURED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

// SAFETY: every method forwards to `System` with the caller's arguments; the counting touches
// only const-initialized thread-locals and an atomic, which never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        record(true, l.size());
        unsafe { System.alloc(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        record(false, l.size());
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        record(true, l.size());
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        record(true, n);
        record(false, l.size());
        unsafe { System.realloc(p, l, n) }
    }
}

fn section_allocs() -> (u64, u64) {
    IN_SECTION.with(Cell::get)
}

fn hash(k: u64) -> BlockHash {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&k.to_le_bytes());
    BlockHash::from(h)
}

fn harness(q: u32) -> (SlotHarness, FeedReader) {
    SlotHarness::new(FeedConfig {
        capacity: q,
        ..FeedConfig::node()
    })
}

fn event(k: u64) -> ChainEvent {
    let block = BlockRef {
        id: BlockId((k % 4) as u32),
        hash: hash(k % 4),
        height: (k % 4) as u32,
    };
    if k.is_multiple_of(2) {
        TipStep::Connected {
            block,
            seq: k,
        }
    } else {
        TipStep::Disconnected {
            block,
            seq: k,
        }
    }
}

/// 10⁵ mixed operations on one thread: pushes, `next`, pops, compressions, queued and coalescing
/// sends (payloads boxed by the sender), waker replacement and the outstanding-kinds probe.
/// Every replaced waker and coalesced payload holds the last reference to its allocation, so a
/// drop inside the section would be counted.
fn drive(h: &SlotHarness, r: &mut FeedReader) {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    for i in 0..100_000u64 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        match x % 8 {
            0 | 1 => {
                let _ = h.push(event(i));
            }
            2 => {
                let _ = r.next();
            }
            3 => {
                if let Next::Event(_) = r.next() {
                    r.pop();
                }
            }
            4 => {
                let k = ControlKind::new((x >> 8) as u8 % 3);
                let _ = h.send_control(ControlMsg::new(k, i));
            }
            5 => {
                let _ = r.remove_round_trip(&hash((x >> 8) % 4));
            }
            6 => r.set_waker(Notify::Thread(Parker::new())),
            _ => {
                let _ = h.control_outstanding();
            }
        }
    }
}

#[test]
fn slot_sections_do_not_allocate() {
    let (h, mut r) = harness(8);
    let before = section_allocs();
    drive(&h, &mut r);
    let after = section_allocs();
    assert_eq!(after, before, "allocations, frees inside slot sections");
}

/// The planted negative control (L9's): an intake that reallocates inside its section is seen.
#[test]
fn slot_sections_do_not_allocate_negative_control() {
    let (h, mut r) = harness(8);
    h.plant_alloc_in_intake(true);
    let before = section_allocs();
    let _ = h.push(event(0));
    let _ = r.next();
    let after = section_allocs();
    assert!(after.0 > before.0 && after.1 > before.1, "the planted allocation is counted");
}

/// §6.1: a feed's allocations (the slot, its `Arc`, three Q-sized buffers) stay within the fixed
/// term plus the buffers, at both Q values; control payloads are counted separately.
#[test]
fn per_feed_reservation_within_cap() {
    for q in [1024u32, 256] {
        let (first, _r) = harness(q);
        MEASURED_BYTES.store(0, Ordering::SeqCst);
        MEASURE.with(|m| m.set(true));
        let (h, r) = first.sibling();
        let boxed: Box<dyn SubscriberFeed> = Box::new(r);
        MEASURE.with(|m| m.set(false));
        let bytes = MEASURED_BYTES.load(Ordering::SeqCst) as usize;
        let buffers = 3 * q as usize * size_of::<ChainEvent>();
        assert!(bytes >= buffers, "the three buffers are reserved up front: {bytes}");
        assert!(bytes <= buffers + FEED_FIXED_BYTES, "{bytes} over the fixed term");
        assert!(bytes <= per_feed_cap(q));
        drop((h, boxed));
    }
}

/// The section probe is per thread: another thread's open section does not count here.
#[test]
fn section_probe_is_per_thread() {
    let (h, mut r) = harness(4);
    let h = Arc::new(h);
    let _ = h.push(event(0));
    h.intake_pause().arm();
    let t = std::thread::spawn(move || {
        let _ = r.next();
    });
    h.intake_pause().wait_paused();
    assert!(!in_slot_section());
    let v: Vec<u8> = Vec::with_capacity(64);
    drop(v);
    h.intake_pause().release();
    t.join().unwrap();
}
