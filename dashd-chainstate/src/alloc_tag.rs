//! The tagging allocator (CH2 §6.7, DEC-77): checks the coins budget's arithmetic to the byte.
//!
//! The coins cache computes its memory from its own allocation sizes (hash table allocations,
//! long-coin boxes); production has no allocator wrapper. In bench and test builds
//! (feature `alloc-tag`) a binary installs [`TaggingAllocator`] as its global allocator, and the
//! cache wraps every allocation site in [`coins`]'s scope guard. Each allocation then carries a
//! 16-byte header with its tag and requested size, and [`tagged_bytes`] sums the live requested
//! bytes per tag. A free subtracts by its header on whatever thread frees, so W dropping a frozen
//! layer is counted. Equality of the two sums catches both a coins allocation outside a declared
//! site (untagged, so the tagged sum falls short) and a foreign allocation inside one.
//!
//! Tests run in parallel inside one process, so each test's thread can take its own **domain**
//! ([`set_thread_domain`]); the header records it and [`tagged_bytes`] reads one domain.
//!
//! Without the feature the guards are empty and inlined away.

/// What an allocation is counted as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Tag {
    Untagged = 0,
    /// The coins budget (CH2 §2.5).
    Coins = 1,
}

/// Restores the thread's previous tag when dropped.
#[must_use]
pub struct TagGuard {
    #[cfg(feature = "alloc-tag")]
    prev: u8,
}

/// Tags the current thread's allocations as [`Tag::Coins`] until the guard drops.
#[inline]
pub fn coins() -> TagGuard {
    scope(Tag::Coins)
}

/// Leaves the current thread's allocations untagged until the guard drops: a site inside a coins
/// scope that allocates something not the cache's (the fail-stop path's records and messages).
#[inline]
pub fn untagged() -> TagGuard {
    scope(Tag::Untagged)
}

#[inline]
#[cfg_attr(not(feature = "alloc-tag"), allow(unused_variables))]
fn scope(tag: Tag) -> TagGuard {
    #[cfg(feature = "alloc-tag")]
    {
        TagGuard {
            prev: imp::set_tag(tag as u8),
        }
    }
    #[cfg(not(feature = "alloc-tag"))]
    TagGuard {}
}

impl Drop for TagGuard {
    #[inline]
    fn drop(&mut self) {
        #[cfg(feature = "alloc-tag")]
        imp::set_tag(self.prev);
    }
}

/// An allocation made outside the global allocator (the shard tables' mapped pages,
/// `coins::layer::pages`), counted under the domain and tag current where it was created. It is
/// carried by its owner, so a free on another thread is counted where the allocation was.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Charge {
    #[cfg(feature = "alloc-tag")]
    at: (u8, u8),
}

impl Charge {
    /// The calling thread's domain and tag.
    #[inline]
    pub(crate) fn current() -> Self {
        Self {
            #[cfg(feature = "alloc-tag")]
            at: imp::current(),
        }
    }

    /// Counts `bytes` allocated (positive) or freed (negative).
    #[inline]
    #[cfg_attr(not(feature = "alloc-tag"), allow(unused_variables))]
    pub(crate) fn add(self, bytes: i64) {
        #[cfg(feature = "alloc-tag")]
        imp::charge(self.at, bytes);
    }
}

#[cfg(feature = "alloc-tag")]
pub use imp::{
    DOMAINS, TaggingAllocator, new_domain, peak_bytes, reset_peak, set_thread_domain, tagged_bytes,
    thread_alloc_events, thread_domain,
};

#[cfg(feature = "alloc-tag")]
#[allow(unsafe_code)]
mod imp {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

    use super::Tag;

    /// How many separate counting domains there are; domain ids wrap, so a process should take at
    /// most this many at once (a driver running many inputs keeps its thread's one).
    pub const DOMAINS: usize = 256;
    const TAGS: usize = 2;
    const HEADER: usize = 16;

    static LIVE: [[AtomicI64; TAGS]; DOMAINS] =
        [const { [const { AtomicI64::new(0) }; TAGS] }; DOMAINS];
    static PEAK: [[AtomicI64; TAGS]; DOMAINS] =
        [const { [const { AtomicI64::new(0) }; TAGS] }; DOMAINS];
    static NEXT_DOMAIN: AtomicU32 = AtomicU32::new(1);

    thread_local! {
        static TAG: Cell<u8> = const { Cell::new(0) };
        static DOMAIN: Cell<u8> = const { Cell::new(0) };
        static EVENTS: Cell<u64> = const { Cell::new(0) };
    }

    /// How many allocations and frees this thread has made (a `realloc` counts both), so a test
    /// can assert a section makes none (CH1-node §9.6's growth test).
    pub fn thread_alloc_events() -> u64 {
        EVENTS.try_with(Cell::get).unwrap_or(0)
    }

    fn count_event() {
        let _ = EVENTS.try_with(|e| e.set(e.get() + 1));
    }

    pub(super) fn set_tag(tag: u8) -> u8 {
        TAG.try_with(|t| t.replace(tag)).unwrap_or(0)
    }

    pub(super) fn current() -> (u8, u8) {
        (DOMAIN.try_with(Cell::get).unwrap_or(0), TAG.try_with(Cell::get).unwrap_or(0))
    }

    /// Counts an allocation (`bytes` > 0) or a free made outside the global allocator.
    pub(super) fn charge((domain, tag): (u8, u8), bytes: i64) {
        count_event();
        bump(domain, tag, bytes);
    }

    /// Adds `bytes` to the live count of `domain` and `tag`, and raises its peak.
    fn bump(domain: u8, tag: u8, bytes: i64) {
        let (d, t) = (usize::from(domain), usize::from(tag));
        let live = LIVE[d][t].fetch_add(bytes, Ordering::SeqCst) + bytes;
        PEAK[d][t].fetch_max(live, Ordering::SeqCst);
    }

    /// A fresh counting domain id, never 0 (the default every thread starts in).
    pub fn new_domain() -> u8 {
        loop {
            let d = (NEXT_DOMAIN.fetch_add(1, Ordering::Relaxed) % DOMAINS as u32) as u8;
            if d != 0 {
                return d;
            }
        }
    }

    /// Counts this thread's later allocations in `domain`.
    pub fn set_thread_domain(domain: u8) {
        DOMAIN.with(|d| d.set(domain));
    }

    /// The domain this thread counts in: 0 until [`set_thread_domain`].
    pub fn thread_domain() -> u8 {
        DOMAIN.with(Cell::get)
    }

    /// The live requested bytes allocated under `tag` in `domain`.
    pub fn tagged_bytes(domain: u8, tag: Tag) -> i64 {
        LIVE[usize::from(domain)][tag as usize].load(Ordering::SeqCst)
    }

    /// The most live bytes under `tag` in `domain` since the last [`reset_peak`].
    pub fn peak_bytes(domain: u8, tag: Tag) -> i64 {
        PEAK[usize::from(domain)][tag as usize].load(Ordering::SeqCst)
    }

    /// Starts a new peak at the current live bytes.
    pub fn reset_peak(domain: u8, tag: Tag) {
        let (d, t) = (usize::from(domain), tag as usize);
        PEAK[d][t].store(LIVE[d][t].load(Ordering::SeqCst), Ordering::SeqCst);
    }

    /// The global allocator of bench and test binaries built with `alloc-tag`.
    pub struct TaggingAllocator;

    fn header_len(layout: Layout) -> usize {
        layout.align().max(HEADER)
    }

    /// The system allocation behind `layout`: its header, then its bytes. `None` where that
    /// overflows, which the allocator answers with null.
    fn outer(layout: Layout) -> Option<Layout> {
        let size = layout.size().checked_add(header_len(layout))?;
        Layout::from_size_align(size, layout.align().max(HEADER)).ok()
    }

    /// Writes the header in front of `user` and counts the allocation.
    ///
    /// # Safety
    ///
    /// `user` is `HEADER` bytes or more past the start of an allocation.
    unsafe fn stamp(user: *mut u8, size: usize) {
        let (domain, tag) = current();
        let mut h = [0u8; HEADER];
        h[0] = domain;
        h[1] = tag;
        h[8..].copy_from_slice(&(size as u64).to_le_bytes());
        // SAFETY: the caller's contract.
        unsafe { user.sub(HEADER).copy_from_nonoverlapping(h.as_ptr(), HEADER) };
        bump(domain, tag, size as i64);
    }

    /// Stamps a fresh `outer(layout)` allocation and returns its user bytes; null stays null.
    ///
    /// # Safety
    ///
    /// `base` is null or a live allocation of `outer(layout)`.
    unsafe fn finish(base: *mut u8, layout: Layout) -> *mut u8 {
        if base.is_null() {
            return base;
        }
        // SAFETY: the header fits in front of the user bytes.
        unsafe {
            let user = base.add(header_len(layout));
            stamp(user, layout.size());
            user
        }
    }

    unsafe impl GlobalAlloc for TaggingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let Some(outer) = outer(layout) else {
                return std::ptr::null_mut();
            };
            count_event();
            // SAFETY: `outer` has a non-zero size, and `finish` gets what it returned.
            unsafe { finish(System.alloc(outer), layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let Some(outer) = outer(layout) else {
                return std::ptr::null_mut();
            };
            count_event();
            // SAFETY: as `alloc`.
            unsafe { finish(System.alloc_zeroed(outer), layout) }
        }

        unsafe fn dealloc(&self, user: *mut u8, layout: Layout) {
            count_event();
            let mut h = [0u8; HEADER];
            // SAFETY: `user` came from `alloc`, so its header lies in front of it.
            unsafe { h.as_mut_ptr().copy_from_nonoverlapping(user.sub(HEADER), HEADER) };
            let size = u64::from_le_bytes(h[8..].try_into().unwrap());
            debug_assert_eq!(size, layout.size() as u64);
            LIVE[usize::from(h[0])][usize::from(h[1])].fetch_sub(size as i64, Ordering::SeqCst);
            // SAFETY: the base and layout `alloc` used; it succeeded, so `outer(layout)` is `Some`.
            unsafe {
                System.dealloc(user.sub(header_len(layout)), outer(layout).unwrap_unchecked())
            };
        }

        unsafe fn realloc(&self, user: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // A new allocation under the current tag, so a coins allocation grown outside its
            // declared site leaves the tagged sum.
            // SAFETY: `GlobalAlloc::realloc`'s contract gives a valid new layout.
            unsafe {
                let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
                let new = self.alloc(new_layout);
                if !new.is_null() {
                    new.copy_from_nonoverlapping(user, layout.size().min(new_size));
                    self.dealloc(user, layout);
                }
                new
            }
        }
    }

    const _: () = assert!(Tag::Coins as usize + 1 == TAGS);
}

#[cfg(all(test, feature = "alloc-tag"))]
mod tests {
    use super::*;

    #[test]
    fn coins_allocations_are_counted_until_freed_on_any_thread() {
        let domain = new_domain();
        set_thread_domain(domain);
        let before = tagged_bytes(domain, Tag::Coins);
        let untagged = vec![0u8; 1000];
        assert_eq!(tagged_bytes(domain, Tag::Coins), before);
        let tagged = {
            let _t = coins();
            vec![0u64; 100]
        };
        assert_eq!(tagged_bytes(domain, Tag::Coins), before + 800);
        // Growth outside the scope leaves the tagged sum.
        let mut grown = tagged.clone();
        assert_eq!(tagged_bytes(domain, Tag::Coins), before + 800);
        {
            let _t = coins();
            grown.reserve_exact(1000);
        }
        assert_eq!(tagged_bytes(domain, Tag::Coins), before + 800 + 8 * grown.capacity() as i64);
        #[expect(
            clippy::disallowed_methods,
            reason = "a test thread; LO-9 governs node threads (CH1-node §8.3)"
        )]
        std::thread::spawn(move || drop((tagged, grown))).join().unwrap();
        assert_eq!(tagged_bytes(domain, Tag::Coins), before);
        drop(untagged);
    }

    #[test]
    fn large_alignments_keep_their_header() {
        #[repr(align(64))]
        struct Big(#[allow(dead_code)] [u8; 64]);
        let domain = new_domain();
        set_thread_domain(domain);
        let b = {
            let _t = coins();
            // Opaque, so an optimised build cannot keep it on the stack.
            std::hint::black_box(Box::new(Big([1; 64])))
        };
        assert_eq!(b.as_ref() as *const Big as usize % 64, 0);
        assert_eq!(tagged_bytes(domain, Tag::Coins), 64);
        drop(b);
        assert_eq!(tagged_bytes(domain, Tag::Coins), 0);
    }
}
