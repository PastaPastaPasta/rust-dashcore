//! The shard tables' allocator (CH2c D29, DEC-388): a table of [`MAP_MIN`] bytes or more is mapped
//! from the kernel in whole pages and unmapped when it is freed, so the table a doubling replaces
//! leaves RssAnon at once. glibc would serve it by `mmap` too, but it raises its dynamic mmap
//! threshold to the size of each mapped block it frees, so after a few doublings it serves tables
//! from the heap and keeps them. Smaller tables (a shard's first) come from the global allocator
//! as before, and so does every table of a block overlay's child ([`Pages::heap`], CH2c D47). Only
//! the shard tables use it; nothing else changes allocator.
//!
//! The ledger counts a mapped table's whole pages ([`footprint`]), as does the tagging allocator
//! (through [`Charge`]), so the cache's arithmetic and the tagged sum stay equal.
//!
//! Under Miri, off Unix and with pages other than 4 KiB, every table comes from the global
//! allocator.

use std::alloc::Layout;
use std::ptr::NonNull;

use allocator_api2::alloc::{AllocError, Allocator, Global};

use crate::alloc_tag::Charge;

/// The smallest table that is mapped: 64 KiB, so a page's rounding wastes under 1/16 of it.
const MAP_MIN: usize = 64 << 10;
/// The page size mapping needs; with larger pages every table comes from the global allocator.
pub(super) const PAGE: usize = 4096;

/// The allocator of one shard table. It carries the domain and tag the table was created under,
/// so W dropping a frozen layer is counted where V made it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Pages {
    charge: Charge,
    /// Never map: every table from the global allocator (CH2c D47). A block overlay's child is
    /// emptied and refilled every block; mapping its tables would fault their pages in again each
    /// time, where the heap reuses them.
    heap: bool,
}

impl Pages {
    /// An allocator counting under the calling thread's domain and tag.
    pub(super) fn current() -> Self {
        Self {
            charge: Charge::current(),
            heap: false,
        }
    }

    /// [`Self::current`], never mapping.
    pub(super) fn heap() -> Self {
        Self {
            heap: true,
            ..Self::current()
        }
    }

    /// Whether this is [`Self::heap`].
    #[cfg(test)]
    pub(super) fn is_heap(&self) -> bool {
        self.heap
    }

    /// The bytes an allocation of `size` bytes holds ([`footprint`], or `size` for a heap table).
    #[inline]
    pub(super) fn footprint(&self, size: usize) -> usize {
        if self.heap {
            size
        } else {
            footprint(size)
        }
    }
}

/// Whether an allocation of `size` bytes goes to [`Pages::map`]: the one size test `allocate`,
/// `allocate_zeroed` and `deallocate` make inline, so each block is freed the way it was made.
#[inline(always)]
const fn large(size: usize) -> bool {
    size >= MAP_MIN
}

/// Whether an allocation of `size` bytes is mapped.
#[inline]
fn mapped(size: usize) -> bool {
    large(size) && sys::page() == Some(PAGE)
}

/// The bytes an allocation of `size` bytes holds: whole pages if it is mapped.
#[inline]
pub(super) fn footprint(size: usize) -> usize {
    if mapped(size) {
        charged(size)
    } else {
        size
    }
}

/// The bytes a table of `size` bytes is charged at its ceiling: whole pages from [`MAP_MIN`] up,
/// whether this platform maps it or not, so a presize has one decomposition everywhere. At least
/// [`footprint`].
pub(super) const fn charged(size: usize) -> usize {
    if size >= MAP_MIN {
        size.next_multiple_of(PAGE)
    } else {
        size
    }
}

// SAFETY: a mapped block stays valid until `deallocate` unmaps it, and only that block's
// `deallocate` does. A block is freed by a `Pages` with the `heap` flag of the one that made it:
// copies keep the flag, and hashbrown frees each table with the allocator stored in it, which every
// table carries from its creation (a layer builds and rebuilds its tables with its own flag or the
// table's own allocator). A `Pages` of the other flag must never free it: a heap one would hand a
// mapping to the global allocator, a mapping one would unmap a large heap block. Given the flag,
// [`large`] and then `mapped` decide by the size and the process's fixed page size alone, and
// `deallocate` gets the layout `allocate` did, so each block goes back where it came from. A mapping
// is page-aligned, so it meets any alignment up to a page; a larger one is refused.
//
// Only the size test is inline; the rest is out of line (CH2c D31, BL-373). A block overlay makes
// 64 small tables per block, and with more inline hashbrown's table constructor took a heavier
// path for each of them.
unsafe impl Allocator for Pages {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if large(layout.size()) {
            self.map(layout, false)
        } else {
            Global.allocate(layout)
        }
    }

    #[inline]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if large(layout.size()) {
            self.map(layout, true)
        } else {
            Global.allocate_zeroed(layout)
        }
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if large(layout.size()) {
            // SAFETY: the caller's contract; `map` made this block for `layout`.
            unsafe { self.unmap(ptr, layout) }
        } else {
            // SAFETY: the caller's contract; `allocate` took this layout to `Global`.
            unsafe { Global.deallocate(ptr, layout) }
        }
    }
}

impl Pages {
    /// A block for `layout` of [`MAP_MIN`] bytes or more: mapped in whole pages if [`mapped`]
    /// holds, else from the global allocator (zeroed if `zeroed`; fresh pages already are).
    #[cold]
    #[inline(never)]
    fn map(&self, layout: Layout, zeroed: bool) -> Result<NonNull<[u8]>, AllocError> {
        if self.heap || !mapped(layout.size()) {
            return if zeroed {
                Global.allocate_zeroed(layout)
            } else {
                Global.allocate(layout)
            };
        }
        if layout.align() > PAGE {
            return Err(AllocError);
        }
        let len = charged(layout.size());
        let p = sys::map(len).ok_or(AllocError)?;
        self.charge.add(len as i64);
        Ok(NonNull::slice_from_raw_parts(p, len))
    }

    /// Frees a block [`Self::map`] made.
    ///
    /// # Safety
    ///
    /// `ptr` is a live block `map` made for `layout`, and nothing uses it after.
    #[cold]
    #[inline(never)]
    unsafe fn unmap(&self, ptr: NonNull<u8>, layout: Layout) {
        if self.heap || !mapped(layout.size()) {
            // SAFETY: the caller's contract; `map` took this layout to `Global`.
            return unsafe { Global.deallocate(ptr, layout) };
        }
        let len = charged(layout.size());
        // SAFETY: the caller's contract: `ptr` is a live mapping of `len` bytes.
        unsafe { sys::unmap(ptr, len) };
        self.charge.add(-(len as i64));
    }
}

#[cfg(all(unix, not(miri)))]
mod sys {
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The page size, read once.
    pub(super) fn page() -> Option<usize> {
        static PAGE: AtomicUsize = AtomicUsize::new(0);
        let p = match PAGE.load(Ordering::Relaxed) {
            0 => {
                // SAFETY: `sysconf` reads a constant of the system.
                let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
                let p = usize::try_from(p).unwrap_or(usize::MAX);
                PAGE.store(p, Ordering::Relaxed);
                p
            }
            p => p,
        };
        Some(p)
    }

    /// `len` bytes of fresh private anonymous pages (`len` a multiple of the page size).
    pub(super) fn map(len: usize) -> Option<NonNull<u8>> {
        // SAFETY: a new anonymous mapping touches no existing memory.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            None
        } else {
            NonNull::new(p.cast())
        }
    }

    /// Unmaps a mapping [`map`] made.
    ///
    /// # Safety
    ///
    /// `p` and `len` are a live mapping of [`map`], and nothing uses it after.
    pub(super) unsafe fn unmap(p: NonNull<u8>, len: usize) {
        // SAFETY: the caller's contract.
        let r = unsafe { libc::munmap(p.as_ptr().cast(), len) };
        // It fails only for a range `map` did not return; the pages would leak, not corrupt.
        debug_assert_eq!(r, 0, "munmap of a table");
    }
}

#[cfg(not(all(unix, not(miri))))]
mod sys {
    use std::ptr::NonNull;

    pub(super) fn page() -> Option<usize> {
        None
    }

    pub(super) fn map(_: usize) -> Option<NonNull<u8>> {
        unreachable!("nothing is mapped")
    }

    pub(super) unsafe fn unmap(_: NonNull<u8>, _: usize) {
        unreachable!("nothing is mapped")
    }
}

#[cfg(all(test, target_os = "linux", not(miri)))]
mod tests {
    use super::*;

    /// Whether each page of `[p, p + len)` is resident; `None` once the range is unmapped
    /// (`mincore` fails with `ENOMEM`).
    fn resident(p: NonNull<u8>, len: usize) -> Option<Vec<bool>> {
        let mut v = vec![0u8; len.div_ceil(PAGE)];
        // SAFETY: `v` holds one byte per page of the range; `mincore` reads no user memory.
        let r = unsafe { libc::mincore(p.as_ptr().cast(), len, v.as_mut_ptr()) };
        (r == 0).then(|| v.iter().map(|b| b & 1 == 1).collect())
    }

    /// Whether this host maps tables at all; with other page sizes every table comes from the
    /// global allocator, and the tests of mappings have nothing to check.
    fn maps() -> bool {
        sys::page() == Some(PAGE)
    }

    /// Alone in a child process, so no other test maps into the freed range before `mincore`.
    #[test]
    fn a_freed_table_returns_its_pages() {
        if !maps()
            || !super::super::tests::alone(
                "coins::layer::pages::tests::a_freed_table_returns_its_pages",
            )
        {
            return;
        }
        let layout = Layout::from_size_align(3 * MAP_MIN + 16, 16).unwrap();
        let block = Pages::current().allocate(layout).unwrap();
        let (p, len) = (block.cast::<u8>(), block.len());
        assert_eq!(len % PAGE, 0);
        assert_eq!(len, footprint(layout.size()));
        assert!(len - layout.size() < PAGE, "rounded up to the next page only");
        assert!(resident(p, len).unwrap().iter().all(|r| !r), "untouched pages are not resident");
        // SAFETY: the block is `len` writable bytes.
        unsafe { p.as_ptr().write_bytes(0xAB, len) };
        assert!(resident(p, len).unwrap().iter().all(|r| *r));
        // SAFETY: the block and layout `allocate` returned.
        unsafe { Pages::current().deallocate(p, layout) };
        assert_eq!(resident(p, len), None, "unmapped");
    }

    /// Allocates a block of exactly [`MAP_MIN`] bytes (zeroed if `zeroed`), checks it is a fresh
    /// mapping (page-aligned, untouched, zeroed), frees it and checks the range is unmapped. A heap
    /// block freed by `munmap` would rarely be page-aligned and never be untouched and unmapped
    /// after `free`, so a disagreement between the inline size test and [`mapped`] fails here.
    fn boundary_block_is_a_mapping(zeroed: bool) {
        let layout = Layout::from_size_align(MAP_MIN, 16).unwrap();
        let pages = Pages::current();
        let block = if zeroed {
            pages.allocate_zeroed(layout)
        } else {
            pages.allocate(layout)
        };
        let p = block.unwrap().cast::<u8>();
        assert_eq!(p.as_ptr() as usize % PAGE, 0);
        assert!(resident(p, MAP_MIN).unwrap().iter().all(|r| !r), "untouched");
        // SAFETY: the block is `MAP_MIN` readable bytes.
        assert!(unsafe { std::slice::from_raw_parts(p.as_ptr(), MAP_MIN) }.iter().all(|b| *b == 0));
        // SAFETY: the block and layout `allocate` returned.
        unsafe { pages.deallocate(p, layout) };
        assert_eq!(resident(p, MAP_MIN), None, "unmapped");
    }

    /// The size test inline and `mapped` out of line agree at the boundary, for `allocate`.
    /// Alone in a child process, as [`a_freed_table_returns_its_pages`].
    #[test]
    fn a_table_of_map_min_bytes_is_mapped() {
        if maps()
            && super::super::tests::alone(
                "coins::layer::pages::tests::a_table_of_map_min_bytes_is_mapped",
            )
        {
            boundary_block_is_a_mapping(false);
        }
    }

    /// The same for `allocate_zeroed`.
    #[test]
    fn a_zeroed_table_of_map_min_bytes_is_mapped() {
        if maps()
            && super::super::tests::alone(
                "coins::layer::pages::tests::a_zeroed_table_of_map_min_bytes_is_mapped",
            )
        {
            boundary_block_is_a_mapping(true);
        }
    }

    /// A heap table of [`MAP_MIN`] bytes or more comes from the global allocator (CH2c D47).
    #[test]
    fn heap_tables_are_never_mapped() {
        let layout = Layout::from_size_align(3 * MAP_MIN + 16, 16).unwrap();
        let pages = Pages::heap();
        assert_eq!(pages.footprint(layout.size()), layout.size());
        for zeroed in [false, true] {
            let block = if zeroed {
                pages.allocate_zeroed(layout)
            } else {
                pages.allocate(layout)
            };
            let block = block.unwrap();
            assert_eq!(block.len(), layout.size(), "not rounded to pages");
            // SAFETY: the block and layout `allocate` returned.
            unsafe { pages.deallocate(block.cast(), layout) };
        }
    }

    #[test]
    fn small_zeroed_tables_come_from_the_global_allocator_zeroed() {
        let small = Layout::from_size_align(MAP_MIN - 16, 16).unwrap();
        let block = Pages::current().allocate_zeroed(small).unwrap();
        assert_eq!(block.len(), small.size());
        // SAFETY: the block is `small.size()` readable bytes.
        assert!(unsafe { block.as_ref() }.iter().all(|b| *b == 0));
        // SAFETY: the block and layout `allocate_zeroed` returned.
        unsafe { Pages::current().deallocate(block.cast(), small) };
    }

    #[test]
    fn small_tables_come_from_the_global_allocator() {
        let small = Layout::from_size_align(MAP_MIN - 16, 16).unwrap();
        assert_eq!(footprint(small.size()), small.size());
        let block = Pages::current().allocate(small).unwrap();
        assert_eq!(block.len(), small.size());
        // SAFETY: the block and layout `allocate` returned.
        unsafe { Pages::current().deallocate(block.cast(), small) };
    }

    #[cfg(feature = "alloc-tag")]
    #[test]
    fn mapped_pages_are_counted_where_the_table_was_made() {
        use crate::alloc_tag::{self, Tag};
        let d = alloc_tag::new_domain();
        alloc_tag::set_thread_domain(d);
        let layout = Layout::from_size_align(MAP_MIN + 1, 16).unwrap();
        let (pages, block) = {
            let _t = alloc_tag::coins();
            let pages = Pages::current();
            (pages, pages.allocate(layout).unwrap())
        };
        assert_eq!(alloc_tag::tagged_bytes(d, Tag::Coins), block.len() as i64);
        let p = block.cast::<u8>().as_ptr() as usize;
        #[expect(
            clippy::disallowed_methods,
            reason = "a test thread; LO-9 governs node threads (CH1-node §8.3)"
        )]
        std::thread::spawn(move || {
            // SAFETY: the block and layout `allocate` returned.
            unsafe { pages.deallocate(NonNull::new(p as *mut u8).unwrap(), layout) }
        })
        .join()
        .unwrap();
        assert_eq!(alloc_tag::tagged_bytes(d, Tag::Coins), 0);
    }
}
