//! [`StableBytes`]: an append-only store of byte strings behind `&self` whose addresses never
//! move (CH2 §2.4.2). The block overlay's `&self` reads keep the database hits they cannot cache
//! here, and return borrows of them.
//!
//! Each value is boxed once and turned into a raw pointer; borrows handed out derive from that
//! pointer. Growing the list moves only the pointers, never a `Box`, so no uniqueness is asserted
//! again over memory a borrow points into (a `Vec<Box<[u8]>>` would be undefined behaviour under
//! Stacked Borrows when it reallocates). Miri checks this module under Stacked and Tree Borrows.

#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::ptr::NonNull;

use crate::alloc_tag;

#[derive(Default)]
pub struct StableBytes {
    values: UnsafeCell<Vec<NonNull<[u8]>>>,
    /// Bytes of the values, for the budget.
    bytes: std::cell::Cell<u64>,
}

// `push`'s exclusive borrow of `values` is sound only while the store is `!Sync` (O10). This fails
// to compile if `StableBytes` becomes `Sync`: both impls would then apply, and `_` is ambiguous.
const _: fn() = || {
    trait AmbiguousIfSync<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    struct IsSync;
    impl<T: ?Sized + Sync> AmbiguousIfSync<IsSync> for T {}
    let _ = <StableBytes as AmbiguousIfSync<_>>::some_item;
};

impl StableBytes {
    /// Keeps a copy of `bytes` until the store drops, and borrows it.
    pub fn push(&self, bytes: &[u8]) -> &[u8] {
        let _tag = alloc_tag::coins();
        let raw = NonNull::from(Box::leak(Box::<[u8]>::from(bytes)));
        // SAFETY: `values` is touched only here, in `allocated_bytes` and in `drop`, none of which
        // hands out a reference into the `Vec` itself or runs while another of them does (the
        // type is `!Sync`, and none re-enters), so this exclusive borrow is unique.
        unsafe { (*self.values.get()).push(raw) };
        self.bytes.set(self.bytes.get() + bytes.len() as u64);
        // SAFETY: the allocation lives until `drop`, which needs `&mut self`, so after every
        // borrow returned here has ended; nothing writes it after this point.
        unsafe { raw.as_ref() }
    }

    /// The bytes the store holds: its values and its pointer list.
    pub fn allocated_bytes(&self) -> u64 {
        // SAFETY: as in `push`; a shared read of the length.
        let cap = unsafe { (*self.values.get()).capacity() };
        self.bytes.get() + (cap * size_of::<NonNull<[u8]>>()) as u64
    }

    pub fn len(&self) -> usize {
        // SAFETY: as in `allocated_bytes`.
        unsafe { (*self.values.get()).len() }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for StableBytes {
    fn drop(&mut self) {
        for raw in self.values.get_mut().drain(..) {
            // SAFETY: each pointer came from `Box::leak` in `push` and is freed once, here; no
            // borrow of it outlives `&mut self`.
            drop(unsafe { Box::from_raw(raw.as_ptr()) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Borrows taken before the pointer list reallocates stay valid and unchanged (the case a
    /// `Vec<Box<[u8]>>` gets wrong under Stacked Borrows).
    #[test]
    fn borrows_survive_growth() {
        let s = StableBytes::default();
        let first = s.push(&[1, 2, 3]);
        let held: Vec<&[u8]> = (0..100u8).map(|i| s.push(&[i; 7])).collect();
        assert_eq!(first, &[1, 2, 3]);
        for (i, h) in held.iter().enumerate() {
            assert_eq!(*h, &[i as u8; 7]);
        }
        assert_eq!(s.len(), 101);
        assert!(s.allocated_bytes() >= 3 + 700);
    }

    #[test]
    fn empty_values_and_drop() {
        let s = StableBytes::default();
        assert!(s.is_empty());
        assert_eq!(s.push(&[]), &[] as &[u8]);
        assert_eq!(s.allocated_bytes() % size_of::<NonNull<[u8]>>() as u64, 0);
    }
}
