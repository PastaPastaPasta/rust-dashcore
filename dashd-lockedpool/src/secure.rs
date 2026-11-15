//! [`Secure`]: `N` bytes in the process's locked pool, as Core's `secure_allocator` and
//! `secure_unique_ptr` hold `CKey` data.

use std::alloc::{Layout, handle_alloc_error};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

use zeroize::Zeroize as _;

use crate::pool::{ARENA_SIZE, manager};

/// `N` bytes in one chunk of the process's locked pool. They are zeroed when made, and zeroed
/// before the chunk returns to the pool (`memory_cleanse` in `secure_allocator::deallocate`).
/// A clone takes a chunk of its own, as `CKey`'s copy does.
///
/// A pool that cannot give a chunk ends the process through [`handle_alloc_error`], as Core's
/// `secure_allocator::allocate` throws `std::bad_alloc` (RP1a D49, DEC-787 Q2).
// Core: support/allocators/secure.h:28-43, key.h:56-82
pub struct Secure<const N: usize> {
    ptr: NonNull<[u8; N]>,
}

// SAFETY: a `Secure` owns its chunk alone, and the pool it returns it to is shared behind a
// mutex, so it can move to and be read from another thread as an owned `[u8; N]` can.
unsafe impl<const N: usize> Send for Secure<N> {}
// SAFETY: as for `Send`: `&Secure` gives only `&[u8; N]`.
unsafe impl<const N: usize> Sync for Secure<N> {}

impl<const N: usize> Secure<N> {
    /// `N` zero bytes in a new chunk.
    pub fn zeroed() -> Self {
        const { assert!(N > 0 && N <= ARENA_SIZE, "a pool chunk holds 1 to ARENA_SIZE bytes") };
        let Some(addr) = manager().alloc(N) else {
            handle_alloc_error(Layout::new::<[u8; N]>())
        };
        let ptr = NonNull::new(addr as *mut [u8; N]).expect("the pool gives no null address");
        // SAFETY: the chunk at `addr` is at least `N` bytes of mapped, writable memory that
        // this `Secure` alone owns from here on; `[u8; N]` needs no alignment.
        unsafe { ptr.as_ptr().write_bytes(0, 1) };
        Secure {
            ptr,
        }
    }

    /// A new chunk holding `bytes`.
    ///
    /// # Panics
    ///
    /// When `bytes` is not `N` long.
    pub fn from_slice(bytes: &[u8]) -> Self {
        let mut s = Self::zeroed();
        s.copy_from_slice(bytes);
        s
    }
}

impl<const N: usize> Deref for Secure<N> {
    type Target = [u8; N];

    fn deref(&self) -> &[u8; N] {
        // SAFETY: the chunk is live and initialized until `drop`, and `&self` excludes writes.
        unsafe { self.ptr.as_ref() }
    }
}

impl<const N: usize> DerefMut for Secure<N> {
    fn deref_mut(&mut self) -> &mut [u8; N] {
        // SAFETY: as for `deref`, and `&mut self` makes this the only reference.
        unsafe { self.ptr.as_mut() }
    }
}

impl<const N: usize> Drop for Secure<N> {
    // Core: support/allocators/secure.h:37-43
    fn drop(&mut self) {
        (**self).zeroize();
        #[cfg(test)]
        LAST_DROP.with(|last| *last.borrow_mut() = Some(self.to_vec()));
        manager().free(self.ptr.as_ptr() as usize).expect("a live chunk of the pool");
    }
}

impl<const N: usize> Clone for Secure<N> {
    fn clone(&self) -> Self {
        Self::from_slice(&self[..])
    }
}

impl<const N: usize> PartialEq for Secure<N> {
    fn eq(&self, other: &Self) -> bool {
        self[..] == other[..]
    }
}

impl<const N: usize> Eq for Secure<N> {}

impl<const N: usize> fmt::Debug for Secure<N> {
    /// Never the bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secure<{N}>(..)")
    }
}

#[cfg(test)]
thread_local! {
    /// The bytes the last `Secure` dropped on this thread held when it freed its chunk.
    static LAST_DROP: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

/// What the last `Secure` dropped on this thread held as it returned its chunk.
#[cfg(test)]
pub(crate) fn last_drop() -> Option<Vec<u8>> {
    LAST_DROP.with(|last| last.borrow_mut().take())
}
