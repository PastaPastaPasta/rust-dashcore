//! Core's `allocator_tests` (arena_tests, lockedpool_tests_mock, lockedpool_tests_live), then the
//! port's own: multimap order, the first arena's cap, the locking-failed callback, and
//! [`Secure`].

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::arena::{Arena, INVALID_OR_DOUBLE_FREE};
use crate::page::PageAllocator;
use crate::pool::{ARENA_ALIGN, ARENA_SIZE, INVALID_ADDRESS, LockedPool, locking_failed, manager};
use crate::{Secure, malloc_info, stats};

/// The live manager is shared by the process's tests: those that read its stats run alone.
fn live() -> MutexGuard<'static, ()> {
    static LIVE: Mutex<()> = Mutex::new(());
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

// Core: test/allocator_tests.cpp:18-133
#[test]
fn arena_tests() {
    // A fake base address: the arena never touches its memory.
    let synth_base = 0x0800_0000;
    let synth_size = 1024 * 1024;
    let mut b = Arena::new(synth_base, synth_size, 16);
    let chunk = b.alloc(1000);
    assert!(chunk.is_some());
    assert_eq!(b.stats().used, 1008, "aligned to 16");
    assert_eq!(b.stats().total, synth_size);
    b.free(chunk).unwrap();
    assert_eq!(b.stats().used, 0);
    assert_eq!(b.stats().free, synth_size);
    assert_eq!(b.free(chunk), Err(INVALID_OR_DOUBLE_FREE));

    let a0 = b.alloc(128);
    let a1 = b.alloc(256);
    let a2 = b.alloc(512);
    assert_eq!(b.stats().used, 896);
    assert_eq!(b.stats().total, synth_size);
    b.free(a0).unwrap();
    assert_eq!(b.stats().used, 768);
    b.free(a1).unwrap();
    assert_eq!(b.stats().used, 512);
    let a3 = b.alloc(128);
    assert_eq!(b.stats().used, 640);
    b.free(a2).unwrap();
    assert_eq!(b.stats().used, 128);
    b.free(a3).unwrap();
    assert_eq!(b.stats().used, 0);
    assert_eq!(b.stats().chunks_used, 0);
    assert_eq!(b.stats().total, synth_size);
    assert_eq!(b.stats().free, synth_size);
    assert_eq!(b.stats().chunks_free, 1);

    let mut addr = Vec::with_capacity(2048);
    assert_eq!(b.alloc(0), None);
    // Sweeping: allocate all of it, then free it front to back...
    for _ in 0..1024 {
        addr.push(b.alloc(1024));
    }
    assert_eq!(b.stats().free, 0);
    assert_eq!(b.alloc(1024), None, "full");
    assert_eq!(b.alloc(0), None);
    for &a in &addr {
        b.free(a).unwrap();
    }
    addr.clear();
    assert_eq!(b.stats().total, synth_size);
    assert_eq!(b.stats().free, synth_size);
    // ... and back to front.
    for _ in 0..1024 {
        addr.push(b.alloc(1024));
    }
    for x in 0..1024 {
        b.free(addr[1023 - x]).unwrap();
    }
    addr.clear();

    // Smaller unequal chunks, freed haphazardly; not all of them fit, and freeing `None` is
    // allowed.
    for x in 0..2048 {
        addr.push(b.alloc(x + 1));
    }
    for x in 0..2048 {
        b.free(addr[((x * 23) % 2048) ^ 242]).unwrap();
    }
    addr.clear();

    // Interleaved frees and allocations driven by Core's LFSR.
    addr.resize(2048, None);
    let mut s: u32 = 0x1234_5678;
    for _ in 0..5000 {
        let idx = (s as usize) & (addr.len() - 1);
        if s & 0x8000_0000 != 0 {
            b.free(addr[idx]).unwrap();
            addr[idx] = None;
        } else if addr[idx].is_none() {
            addr[idx] = b.alloc(((s >> 16) & 2047) as usize);
        }
        let lsb = s & 1 != 0;
        s >>= 1;
        if lsb {
            s ^= 0xf00f_00f0; // LFSR period 0xf7ffffe0
        }
    }
    for &a in &addr {
        b.free(a).unwrap();
    }
    assert_eq!(b.stats().total, synth_size);
    assert_eq!(b.stats().free, synth_size);
}

/// Core's `TestLockedPageAllocator`: `count` fake arenas, the first `lockedcount` of them locked.
// Core: test/allocator_tests.cpp:135-164
struct TestLockedPageAllocator {
    count: usize,
    lockedcount: usize,
    limit: usize,
}

impl TestLockedPageAllocator {
    fn new(count: usize, lockedcount: usize) -> Box<Self> {
        Box::new(TestLockedPageAllocator {
            count,
            lockedcount,
            limit: usize::MAX,
        })
    }
}

impl PageAllocator for TestLockedPageAllocator {
    fn allocate_locked(&mut self, _len: usize) -> Option<(usize, bool)> {
        if self.count == 0 {
            return None;
        }
        self.count -= 1;
        let locked = self.lockedcount > 0;
        if locked {
            self.lockedcount -= 1;
        }
        // A fake address: nothing reads or writes it.
        Some((0x0800_0000 + (self.count << 24), locked))
    }

    unsafe fn free_locked(&mut self, _addr: usize, _len: usize) {}

    fn limit(&self) -> usize {
        self.limit
    }
}

// Core: test/allocator_tests.cpp:166-210
#[test]
fn lockedpool_tests_mock() {
    // Three virtual arenas, of which one locks.
    let pool = LockedPool::new(TestLockedPageAllocator::new(3, 1), None);
    assert_eq!(pool.stats().total, 0);
    assert_eq!(pool.stats().locked, 0);

    // Unreasonable requests are refused without allocating anything.
    assert_eq!(pool.alloc(0), None);
    assert_eq!((pool.stats().used, pool.stats().free), (0, 0));
    assert_eq!(pool.alloc(ARENA_SIZE + 1), None);
    assert_eq!((pool.stats().used, pool.stats().free), (0, 0));
    // The largest request is a whole arena (r1 m-2).
    let whole = LockedPool::new(TestLockedPageAllocator::new(1, 1), None);
    let at = whole.alloc(ARENA_SIZE).expect("ARENA_SIZE bytes fit an arena");
    assert_eq!(whole.stats().free, 0);
    whole.free(at).unwrap();

    let a0 = pool.alloc(ARENA_SIZE / 2).unwrap();
    assert_eq!(pool.stats().locked, ARENA_SIZE);
    let a1 = pool.alloc(ARENA_SIZE / 2).unwrap();
    let a2 = pool.alloc(ARENA_SIZE / 2).unwrap();
    let a3 = pool.alloc(ARENA_SIZE / 2).unwrap();
    let a4 = pool.alloc(ARENA_SIZE / 2).unwrap();
    let a5 = pool.alloc(ARENA_SIZE / 2).unwrap();
    // Past the count of three arenas.
    assert_eq!(pool.alloc(16), None);

    for a in [a0, a2, a4, a1, a3, a5] {
        pool.free(a).unwrap();
    }
    assert_eq!(pool.stats().total, 3 * ARENA_SIZE);
    assert_eq!(pool.stats().locked, ARENA_SIZE);
    assert_eq!(pool.stats().used, 0);
}

// Core: test/allocator_tests.cpp:215-236
#[test]
fn lockedpool_tests_live() {
    let _live = live();
    let pool = manager();
    let initial = pool.stats();
    let a0 = pool.alloc(16).unwrap();
    // SAFETY: a 16-byte chunk of the live pool, 16-byte aligned, held by this test alone.
    unsafe {
        (a0 as *mut u32).write(0x1234);
        assert_eq!((a0 as *const u32).read(), 0x1234);
    }
    pool.free(a0).unwrap();
    assert_eq!(pool.free(a0), Err(INVALID_OR_DOUBLE_FREE));
    assert!(pool.stats().total <= initial.total + ARENA_SIZE, "one arena at most");
    assert_eq!(pool.stats().used, initial.used);
}

/// Among free chunks of the chosen size, the one freed first is taken (`std::multimap` keeps
/// equal keys in insertion order), not the lowest address.
#[test]
fn equal_free_chunks_are_taken_in_the_order_they_were_freed() {
    let mut b = Arena::new(0, 1024, 16);
    // Cut from the end: 960, 896, 832, 768, 704.
    let chunks: Vec<_> = (0..5).map(|_| b.alloc(64).unwrap()).collect();
    assert_eq!(chunks, [960, 896, 832, 768, 704]);
    // Two lone free chunks of 64: 896 freed before 768.
    b.free(Some(896)).unwrap();
    b.free(Some(768)).unwrap();
    assert_eq!(b.stats().chunks_free, 3);
    assert_eq!(b.alloc(64), Some(896));
    assert_eq!(b.alloc(64), Some(768));
    // A smaller request takes the best fit, cut from its end.
    b.free(Some(960)).unwrap();
    assert_eq!(b.alloc(16), Some(1008));
}

/// Freeing coalesces with the free chunks on both sides.
#[test]
fn a_freed_chunk_joins_both_neighbours() {
    let mut b = Arena::new(0, 256, 16);
    let [x, y, z, w] = [0; 4].map(|_| b.alloc(64).unwrap());
    assert_eq!([x, y, z, w], [192, 128, 64, 0]);
    b.free(Some(x)).unwrap();
    b.free(Some(z)).unwrap();
    assert_eq!(b.stats().chunks_free, 2);
    b.free(Some(y)).unwrap();
    assert_eq!((b.stats().chunks_free, b.stats().free), (1, 192));
    b.free(Some(w)).unwrap();
    assert_eq!((b.stats().chunks_free, b.stats().free), (1, 256));
    assert_eq!(b.alloc(256), Some(0));
}

/// The first arena is capped at the locking limit; later ones are not.
// Core: support/lockedpool.cpp:340-350
#[test]
fn the_first_arena_is_capped_at_the_locking_limit() {
    let mut allocator = TestLockedPageAllocator::new(3, 3);
    allocator.limit = 4096;
    let pool = LockedPool::new(allocator, None);
    let a = pool.alloc(16).unwrap();
    assert_eq!((pool.stats().total, pool.stats().locked), (4096, 4096));
    // 4096 bytes cannot hold this: a second, full-size arena.
    let b = pool.alloc(8192).unwrap();
    assert_eq!(pool.stats().total, 4096 + ARENA_SIZE);
    assert_eq!(pool.stats().locked, 4096 + ARENA_SIZE);
    pool.free(a).unwrap();
    pool.free(b).unwrap();
    // A limit of 0 locks nothing and caps nothing.
    let mut allocator = TestLockedPageAllocator::new(1, 0);
    allocator.limit = 0;
    let pool = LockedPool::new(allocator, None);
    pool.alloc(16).unwrap();
    assert_eq!((pool.stats().total, pool.stats().locked), (ARENA_SIZE, 0));
}

/// An arena whose pages did not lock stays out of `locked`; the callback decides whether it
/// is kept.
// Core: support/lockedpool.cpp:351-362
#[test]
fn an_unlocked_arena_is_kept_only_if_the_callback_says_so() {
    let keep = LockedPool::new(TestLockedPageAllocator::new(1, 0), Some(|| true));
    assert!(keep.alloc(16).is_some());
    assert_eq!((keep.stats().total, keep.stats().locked), (ARENA_SIZE, 0));
    let refuse = LockedPool::new(TestLockedPageAllocator::new(1, 0), Some(|| false));
    assert_eq!(refuse.alloc(16), None);
    assert_eq!(refuse.stats().total, 0);
}

/// The process manager's callback (`LockedPoolManager::LockingFailed`, `lockedpool.cpp:384-388`)
/// keeps an arena whose pages did not lock: the allocation succeeds with nothing locked, where
/// refusing it would end the process through `Secure`'s `handle_alloc_error` (r1 Sol n-1).
#[test]
fn the_managers_callback_keeps_an_unlocked_arena() {
    let pool = LockedPool::new(TestLockedPageAllocator::new(1, 0), Some(locking_failed));
    assert!(pool.alloc(32).is_some());
    let stats = pool.stats();
    assert_eq!((stats.total, stats.locked, stats.used), (ARENA_SIZE, 0, 32));
}

#[test]
fn an_address_in_no_arena_is_refused() {
    let pool = LockedPool::new(TestLockedPageAllocator::new(1, 1), None);
    let a = pool.alloc(16).unwrap();
    assert_eq!(pool.free(a + ARENA_SIZE), Err(INVALID_ADDRESS));
    // The arena's end (`a` is its last 16 bytes) is past it, as `ptr < end` (r1 m-2).
    assert_eq!(pool.free(a + ARENA_ALIGN), Err(INVALID_ADDRESS));
    // Inside the arena, but no used chunk starts there (`a` was cut from the arena's end).
    assert_eq!(pool.free(a - ARENA_ALIGN), Err(INVALID_OR_DOUBLE_FREE));
    pool.free(a).unwrap();
}

/// A `Secure` takes one chunk, its clone another, and dropping both returns the pool to where
/// it was; the bytes are zero when made and zeroed before the free.
#[test]
fn secure_bytes_live_in_the_pool_and_are_zeroed_before_the_free() {
    let _live = live();
    // A chunk dirtied and freed without a cleanse, which the next 32 bytes reuse (best fit, cut
    // from the same end), so "zero when made" does not rest on fresh pages (r1 n-4).
    let dirty = manager().alloc(32).unwrap();
    // SAFETY: a 32-byte chunk of the live pool, held by this test alone until the free.
    unsafe { (dirty as *mut [u8; 32]).write([0xa5; 32]) };
    manager().free(dirty).unwrap();
    let before = stats();
    let mut key = Secure::<32>::zeroed();
    assert_eq!(&*key as *const [u8; 32] as usize, dirty, "the dirtied chunk again");
    assert_eq!(*key, [0; 32]);
    key.copy_from_slice(&[7; 32]);
    let after_one = stats();
    assert_eq!(after_one.used, before.used + 32);
    assert_eq!(after_one.chunks_used, before.chunks_used + 1);
    let copy = key.clone();
    assert_eq!(copy, key);
    assert_eq!(stats().used, before.used + 64);
    assert_eq!(format!("{copy:?}"), "Secure<32>(..)");
    drop(copy);
    assert_eq!(crate::secure::last_drop(), Some(vec![0; 32]));
    drop(key);
    assert_eq!(crate::secure::last_drop(), Some(vec![0; 32]));
    let after = stats();
    assert_eq!((after.used, after.chunks_used), (before.used, before.chunks_used));
    // A size off the alignment rounds up, as Core's `align_up`: 1 takes 16.
    let odd = Secure::<64>::from_slice(&[1; 64]);
    let s = Secure::<1>::zeroed();
    assert_eq!(stats().used, before.used + 64 + 16);
    drop((odd, s));
    assert_eq!(stats().used, before.used);
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn malloc_info_is_glibcs_xml() {
    let xml = malloc_info().expect("glibc");
    assert!(xml.starts_with("<malloc version=\"1\">"), "{xml}");
    assert!(xml.trim_end().ends_with("</malloc>"), "{xml}");
}
