//! Core's `Arena`: a best-fit allocator over an address range it never reads or writes.
//!
//! Addresses are plain numbers here, as Core's `arena_tests` use a synthetic base that maps no
//! memory. A chunk's placement decides which neighbours coalesce, and so the chunk counts
//! `getmemoryinfo` reports: allocations are cut from the end of the chosen free chunk, and among
//! free chunks of the chosen size the one inserted first is taken, as `std::multimap`'s
//! `lower_bound` finds it (an insert goes after the equal keys already there).

use std::collections::BTreeMap;

/// `align_up`: `x` rounded up to a multiple of the power of two `align`, wrapping as Core's
/// `size_t` arithmetic does.
// Core: support/lockedpool.cpp:31-35
pub(crate) fn align_up(x: usize, align: usize) -> usize {
    x.wrapping_add(align - 1) & !(align - 1)
}

/// A free chunk's key in `size_to_free_chunk`: its size, then the insertion sequence that orders
/// equal sizes as the multimap does.
type FreeKey = (usize, u64);

/// `Arena::Stats`.
// Core: support/lockedpool.h:58-65
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArenaStats {
    pub used: usize,
    pub free: usize,
    pub total: usize,
    pub chunks_used: usize,
    pub chunks_free: usize,
}

/// The error `Arena::free` throws (`std::runtime_error`).
pub(crate) const INVALID_OR_DOUBLE_FREE: &str = "Arena: invalid or double free";

/// `Arena`.
// Core: support/lockedpool.h:48-123, support/lockedpool.cpp:40-143
#[derive(Debug)]
pub(crate) struct Arena {
    base: usize,
    end: usize,
    alignment: usize,
    /// The next insertion's sequence number in `size_to_free_chunk`.
    seq: u64,
    /// `size_to_free_chunk`: free chunk size (and insertion order) to its start.
    size_to_free_chunk: BTreeMap<FreeKey, usize>,
    /// `chunks_free`: a free chunk's start to its key.
    chunks_free: BTreeMap<usize, FreeKey>,
    /// `chunks_free_end`: a free chunk's end to its key.
    chunks_free_end: BTreeMap<usize, FreeKey>,
    /// `chunks_used`: a used chunk's start to its size.
    chunks_used: BTreeMap<usize, usize>,
}

impl Arena {
    /// One free chunk covering the whole range.
    // Core: support/lockedpool.cpp:40-47
    pub(crate) fn new(base: usize, size: usize, alignment: usize) -> Arena {
        let mut arena = Arena {
            base,
            end: base + size,
            alignment,
            seq: 0,
            size_to_free_chunk: BTreeMap::new(),
            chunks_free: BTreeMap::new(),
            chunks_free_end: BTreeMap::new(),
            chunks_used: BTreeMap::new(),
        };
        arena.insert_free(size, base);
        arena
    }

    /// `size_to_free_chunk.emplace(size, start)`, and the free chunk's start and end mapped to
    /// it (`chunks_free`, `chunks_free_end`).
    fn insert_free(&mut self, size: usize, start: usize) {
        let key = (size, self.seq);
        self.seq += 1;
        self.size_to_free_chunk.insert(key, start);
        self.chunks_free.insert(start, key);
        self.chunks_free_end.insert(start + size, key);
    }

    /// `Arena::alloc`: `size` rounded up to the alignment, taken best-fit from the end of a free
    /// chunk; `None` for 0 or when no free chunk is large enough.
    // Core: support/lockedpool.cpp:53-87
    pub(crate) fn alloc(&mut self, size: usize) -> Option<usize> {
        let size = align_up(size, self.alignment);
        if size == 0 {
            return None;
        }
        let (&key, &free_chunk) = self.size_to_free_chunk.range((size, 0)..).next()?;
        let size_remaining = key.0 - size;
        let allocated = free_chunk + size_remaining;
        self.chunks_used.entry(allocated).or_insert(size);
        self.chunks_free_end.remove(&(free_chunk + key.0));
        if key.0 == size {
            self.chunks_free.remove(&free_chunk);
        } else {
            self.insert_free(size_remaining, free_chunk);
        }
        self.size_to_free_chunk.remove(&key);
        Some(allocated)
    }

    /// `Arena::free`: the used chunk at `ptr` freed and coalesced with the free chunks on both
    /// sides. `None` (`nullptr`) is a no-op.
    ///
    /// # Errors
    ///
    /// [`INVALID_OR_DOUBLE_FREE`] when no used chunk starts at `ptr`.
    // Core: support/lockedpool.cpp:89-125
    pub(crate) fn free(&mut self, ptr: Option<usize>) -> Result<(), &'static str> {
        let Some(ptr) = ptr else {
            return Ok(());
        };
        let mut size = self.chunks_used.remove(&ptr).ok_or(INVALID_OR_DOUBLE_FREE)?;
        let mut start = ptr;
        if let Some(prev) = self.chunks_free_end.remove(&start) {
            start -= prev.0;
            size += prev.0;
            self.size_to_free_chunk.remove(&prev);
        }
        if let Some(next) = self.chunks_free.remove(&(start + size)) {
            size += next.0;
            self.size_to_free_chunk.remove(&next);
        }
        self.insert_free(size, start);
        Ok(())
    }

    /// `Arena::stats`.
    // Core: support/lockedpool.cpp:127-136
    pub(crate) fn stats(&self) -> ArenaStats {
        let used: usize = self.chunks_used.values().sum();
        let free: usize = self.chunks_free.values().map(|key| key.0).sum();
        ArenaStats {
            used,
            free,
            total: used + free,
            chunks_used: self.chunks_used.len(),
            chunks_free: self.chunks_free.len(),
        }
    }

    /// `addressInArena`.
    // Core: support/lockedpool.h:90
    pub(crate) fn address_in_arena(&self, ptr: usize) -> bool {
        ptr >= self.base && ptr < self.end
    }
}
