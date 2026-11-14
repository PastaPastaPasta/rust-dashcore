//! Role selection borrows secret setting text without allocating copies.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use dash_args::{ArgsManager, SettingsValue};
use dashd_node::p2p::MasternodeRole;

thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

struct CountingAllocator;

fn count_allocation() {
    let _ = ALLOCATIONS.try_with(|count| {
        if let Some(value) = count.get() {
            count.set(Some(value + 1));
        }
    });
}

// SAFETY: every allocation operation forwards its unchanged arguments to `System`.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation();
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn role_selection_does_not_allocate_string_or_numeric_key_copies() {
    for value in [
        SettingsValue::Str(
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20".into(),
        ),
        SettingsValue::Num("1".repeat(64)),
    ] {
        let args = ArgsManager::new();
        args.lock_settings(|s| s.rw_settings.insert("masternodeblsprivkey".into(), value));
        ALLOCATIONS.with(|count| count.set(Some(0)));
        let role = MasternodeRole::from_args(&args);
        let allocations = ALLOCATIONS.with(|count| count.replace(None)).unwrap();
        assert!(role.active());
        assert_eq!(allocations, 0);
    }
}
