//! The node's locked memory (RP1a-3b): a port of Dash Core's `LockedPool` (support/lockedpool),
//! the arenas of non-swappable pages that `secure_allocator` hands out for key data, and
//! glibc's `malloc_info`. `getmemoryinfo` reports both.
//!
//! Secrets the node handles live in a [`Secure`] chunk, as Core's `CKey` data does, so the
//! pool's [`stats`] move as Core's do. The unsafe code of the node's RPC layer is here, behind
//! that safe handle.

mod arena;
mod page;
mod pool;
mod secure;

pub use arena::ArenaStats;
pub use pool::{ARENA_SIZE, Stats, stats};
pub use secure::Secure;

/// glibc's `malloc_info(0, ...)` XML, as `getmemoryinfo "mallocinfo"` returns it: `Some("")`
/// when the memory stream does not open, and `None` where the C library has no `malloc_info`
/// (Core without `HAVE_MALLOC_INFO`). The node sets no global allocator, so this is the heap
/// Rust allocates from.
// Core: rpc/node.cpp:935-952
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn malloc_info() -> Option<String> {
    let mut ptr: *mut libc::c_char = std::ptr::null_mut();
    let mut size: libc::size_t = 0;
    // SAFETY: open_memstream stores its buffer and size through the two pointers to live
    // locals, which outlive the stream (closed below).
    let f = unsafe { libc::open_memstream(&mut ptr, &mut size) };
    if f.is_null() {
        return Some(String::new());
    }
    // SAFETY: `f` is the open stream; fclose flushes it and sets `ptr`/`size`, after which it
    // is not used again.
    unsafe {
        libc::malloc_info(0, f);
        libc::fclose(f);
    }
    if ptr.is_null() {
        return Some(String::new());
    }
    // SAFETY: after fclose, `ptr` is a malloc'd buffer of `size` bytes (plus a terminator)
    // owned by this function, freed once copied.
    let xml = unsafe {
        let xml = String::from_utf8_lossy(std::slice::from_raw_parts(ptr.cast::<u8>(), size))
            .into_owned();
        libc::free(ptr.cast());
        xml
    };
    Some(xml)
}

/// No `malloc_info` here (Core without `HAVE_MALLOC_INFO`).
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn malloc_info() -> Option<String> {
    None
}

#[cfg(test)]
mod tests;
