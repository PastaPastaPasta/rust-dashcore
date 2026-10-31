//! Core's `LockedPool` and `LockedPoolManager`.

use std::sync::{Mutex, OnceLock, PoisonError};

use crate::arena::Arena;
use crate::page::{self, PageAllocator};

/// `LockedPool::ARENA_SIZE`: each arena's size, and the largest allocation.
// Core: support/lockedpool.h:134
pub const ARENA_SIZE: usize = 256 * 1024;

/// `LockedPool::ARENA_ALIGN`.
// Core: support/lockedpool.h:138
pub(crate) const ARENA_ALIGN: usize = 16;

/// The error `LockedPool::free` throws for an address in no arena.
pub(crate) const INVALID_ADDRESS: &str = "LockedPool: invalid address not pointing to any arena";

/// `LockedPool::Stats`, as `getmemoryinfo "stats"` reports it.
// Core: support/lockedpool.h:145-153
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub used: usize,
    pub free: usize,
    pub total: usize,
    /// The bytes of the arenas whose pages locked: below `total` once locking failed.
    pub locked: usize,
    pub chunks_used: usize,
    pub chunks_free: usize,
}

/// `LockedPageArena`: an arena over pages its allocator returns when the pool goes.
// Core: support/lockedpool.h:186-196, support/lockedpool.cpp:367-375
struct PageArena {
    arena: Arena,
    base: usize,
    size: usize,
}

struct Inner {
    allocator: Box<dyn PageAllocator>,
    arenas: Vec<PageArena>,
    cumulative_bytes_locked: usize,
    /// `lf_cb`: whether to keep an arena whose pages did not lock.
    locking_failed: Option<fn() -> bool>,
}

/// `LockedPool`: arenas of locked pages, one more whenever none has room.
// Core: support/lockedpool.h:126-205, support/lockedpool.cpp:280-375
pub(crate) struct LockedPool {
    inner: Mutex<Inner>,
}

impl LockedPool {
    // Core: support/lockedpool.cpp:280-283
    pub(crate) fn new(
        allocator: Box<dyn PageAllocator>,
        locking_failed: Option<fn() -> bool>,
    ) -> LockedPool {
        LockedPool {
            inner: Mutex::new(Inner {
                allocator,
                arenas: Vec::new(),
                cumulative_bytes_locked: 0,
                locking_failed,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `LockedPool::alloc`: `size` bytes, 16-byte aligned, from the first arena with room or a
    /// new one; `None` for 0, for more than [`ARENA_SIZE`], or when no arena can be made.
    // Core: support/lockedpool.cpp:287-307
    pub(crate) fn alloc(&self, size: usize) -> Option<usize> {
        let mut inner = self.lock();
        if size == 0 || size > ARENA_SIZE {
            return None;
        }
        if let Some(addr) = inner.arenas.iter_mut().find_map(|a| a.arena.alloc(size)) {
            return Some(addr);
        }
        if inner.new_arena(ARENA_SIZE, ARENA_ALIGN) {
            return inner.arenas.last_mut().expect("just made").arena.alloc(size);
        }
        None
    }

    /// `LockedPool::free`.
    ///
    /// # Errors
    ///
    /// [`INVALID_ADDRESS`] when `ptr` is in no arena, or the arena's error.
    // Core: support/lockedpool.cpp:309-321
    pub(crate) fn free(&self, ptr: usize) -> Result<(), &'static str> {
        let mut inner = self.lock();
        let arena = inner.arenas.iter_mut().find(|a| a.arena.address_in_arena(ptr));
        arena.ok_or(INVALID_ADDRESS)?.arena.free(Some(ptr))
    }

    /// `LockedPool::stats`.
    // Core: support/lockedpool.cpp:323-336
    pub(crate) fn stats(&self) -> Stats {
        let inner = self.lock();
        let mut r = Stats {
            locked: inner.cumulative_bytes_locked,
            ..Stats::default()
        };
        for a in &inner.arenas {
            let i = a.arena.stats();
            r.used += i.used;
            r.free += i.free;
            r.total += i.total;
            r.chunks_used += i.chunks_used;
            r.chunks_free += i.chunks_free;
        }
        r
    }
}

impl Inner {
    /// `LockedPool::new_arena`. The first arena is capped at the process's locking limit, so
    /// that at least it locks (unless the limit is 0).
    // Core: support/lockedpool.cpp:338-365
    fn new_arena(&mut self, size: usize, align: usize) -> bool {
        let mut size = size;
        if self.arenas.is_empty() {
            let limit = self.allocator.limit();
            if limit > 0 {
                size = size.min(limit);
            }
        }
        let Some((addr, locked)) = self.allocator.allocate_locked(size) else {
            return false;
        };
        if locked {
            self.cumulative_bytes_locked += size;
        } else if let Some(keep) = self.locking_failed
            && !keep()
        {
            // SAFETY: the region was just returned for `size` and nothing has used it.
            unsafe { self.allocator.free_locked(addr, size) };
            return false;
        }
        self.arenas.push(PageArena {
            arena: Arena::new(addr, size, align),
            base: addr,
            size,
        });
        true
    }
}

impl Drop for Inner {
    // Core: support/lockedpool.cpp:371-375 (~LockedPageArena)
    fn drop(&mut self) {
        for a in self.arenas.drain(..) {
            // SAFETY: each arena's region came from this allocator, and the pool that handed
            // out its chunks is being dropped, so no chunk is in use.
            unsafe { self.allocator.free_locked(a.base, a.size) };
        }
    }
}

/// `LockedPoolManager::LockingFailed`: keep the arena. Core logs nothing here (its TODO).
// Core: support/lockedpool.cpp:384-388
pub(crate) fn locking_failed() -> bool {
    true
}

/// `LockedPoolManager::Instance()`: the process's pool, made on first use (`CreateInstance`
/// under `std::call_once`) and never dropped.
// Core: support/lockedpool.h:218-240, support/lockedpool.cpp:379-404
pub(crate) fn manager() -> &'static LockedPool {
    static INSTANCE: OnceLock<LockedPool> = OnceLock::new();
    INSTANCE.get_or_init(|| LockedPool::new(page::platform(), Some(locking_failed)))
}

/// The process's pool statistics, as `getmemoryinfo "stats"` reports them
/// (`LockedPoolManager::Instance().stats()`).
// Core: rpc/node.cpp:922-933
pub fn stats() -> Stats {
    manager().stats()
}
