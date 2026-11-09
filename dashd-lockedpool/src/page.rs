//! `LockedPageAllocator`: where the pool's arenas come from.

/// `LockedPageAllocator`: page-granular memory, locked against swapping where the OS allows.
// Core: support/lockedpool.h:19-45
pub(crate) trait PageAllocator: Send {
    /// `AllocateLocked`: `len` bytes, page-aligned and writable, with whether locking succeeded;
    /// `None` when nothing could be mapped.
    fn allocate_locked(&mut self, len: usize) -> Option<(usize, bool)>;

    /// `FreeLocked`: zeroes, unlocks and unmaps what [`allocate_locked`](Self::allocate_locked)
    /// returned at `addr` for `len`.
    ///
    /// # Safety
    ///
    /// `addr` and `len` are a region this allocator returned and has not freed, and nothing
    /// reads or writes it any more.
    unsafe fn free_locked(&mut self, addr: usize, len: usize);

    /// `GetLimit`: how much memory the process may lock.
    fn limit(&self) -> usize;
}

/// The allocator this platform's pool uses.
pub(crate) fn platform() -> Box<dyn PageAllocator> {
    #[cfg(unix)]
    {
        Box::new(posix::PosixLockedPageAllocator::new())
    }
    #[cfg(not(unix))]
    {
        Box::new(unlocked::UnlockedPageAllocator)
    }
}

#[cfg(unix)]
mod posix {
    use super::PageAllocator;
    use crate::arena::align_up;

    /// `PosixLockedPageAllocator`.
    // Core: support/lockedpool.cpp:217-275
    pub(crate) struct PosixLockedPageAllocator {
        page_size: usize,
    }

    impl PosixLockedPageAllocator {
        // Core: support/lockedpool.cpp:228-236
        pub(crate) fn new() -> Self {
            // SAFETY: sysconf reads a constant of the system and has no preconditions.
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            PosixLockedPageAllocator {
                page_size: usize::try_from(page_size).expect("the page size is positive"),
            }
        }
    }

    impl PageAllocator for PosixLockedPageAllocator {
        // Core: support/lockedpool.cpp:238-255
        fn allocate_locked(&mut self, len: usize) -> Option<(usize, bool)> {
            let len = align_up(len, self.page_size);
            // SAFETY: an anonymous private mapping at an address the kernel picks touches no
            // existing memory; the result is checked against MAP_FAILED before any use.
            let addr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if addr == libc::MAP_FAILED || addr.is_null() {
                return None;
            }
            // SAFETY: `addr..addr + len` is the mapping just made, owned by this call.
            let locked = unsafe { libc::mlock(addr, len) } == 0;
            // Kept out of core dumps (Linux), or FreeBSD's equivalent; macOS has neither.
            #[cfg(any(target_os = "linux", target_os = "android"))]
            // SAFETY: as for mlock; madvise only changes how the kernel treats the mapping.
            unsafe {
                libc::madvise(addr, len, libc::MADV_DONTDUMP);
            }
            #[cfg(target_os = "freebsd")]
            // SAFETY: as above.
            unsafe {
                libc::madvise(addr, len, libc::MADV_NOCORE);
            }
            Some((addr as usize, locked))
        }

        // Core: support/lockedpool.cpp:256-262
        unsafe fn free_locked(&mut self, addr: usize, len: usize) {
            let len = align_up(len, self.page_size);
            let ptr = addr as *mut u8;
            // SAFETY: the caller passes a live mapping of ours that nothing else uses
            // (`free_locked`'s contract), so it is valid for writes of `len` bytes.
            unsafe {
                zeroize::Zeroize::zeroize(std::slice::from_raw_parts_mut(ptr, len));
                libc::munlock(ptr.cast(), len);
                libc::munmap(ptr.cast(), len);
            }
        }

        // Core: support/lockedpool.cpp:263-275
        fn limit(&self) -> usize {
            let mut rlim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: getrlimit writes one `rlimit` through a pointer to a live local.
            if unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut rlim) } == 0
                && rlim.rlim_cur != libc::RLIM_INFINITY
            {
                return usize::try_from(rlim.rlim_cur).unwrap_or(usize::MAX);
            }
            usize::MAX
        }
    }
}

/// Where rdashd has no page-locking port (Windows: RP1a D50): heap pages that report locking as
/// failed. Core's pool then works on, unlocked, with the arena out of `locked`.
#[cfg(not(unix))]
mod unlocked {
    use std::alloc::{Layout, alloc_zeroed, dealloc};

    use super::PageAllocator;

    const PAGE: usize = 4096;

    pub(crate) struct UnlockedPageAllocator;

    impl PageAllocator for UnlockedPageAllocator {
        fn allocate_locked(&mut self, len: usize) -> Option<(usize, bool)> {
            let layout = Layout::from_size_align(len, PAGE).ok()?;
            // SAFETY: the pool never asks for 0 bytes (`LockedPool::alloc` refuses size 0).
            let ptr = unsafe { alloc_zeroed(layout) };
            (!ptr.is_null()).then_some((ptr as usize, false))
        }

        unsafe fn free_locked(&mut self, addr: usize, len: usize) {
            let layout = Layout::from_size_align(len, PAGE).expect("allocated with it");
            let ptr = addr as *mut u8;
            // SAFETY: `free_locked`'s contract: our live allocation with this layout.
            unsafe {
                zeroize::Zeroize::zeroize(std::slice::from_raw_parts_mut(ptr, len));
                dealloc(ptr, layout);
            }
        }

        fn limit(&self) -> usize {
            usize::MAX
        }
    }
}
