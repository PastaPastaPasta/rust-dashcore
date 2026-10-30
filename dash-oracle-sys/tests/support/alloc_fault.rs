//! C++ allocation failure on demand (BL-61). Including this module replaces the test binary's
//! `operator new(std::size_t)` (`_Znwm`), which the oracle archives and libstdc++ call: unarmed
//! it is `malloc`, as libstdc++'s own (whose `operator delete` frees); under [`failing`] the
//! chosen call on the calling thread throws `std::bad_alloc`. Other threads, aligned `new`,
//! `malloc` itself (prevector, relic) and Rust's allocator are untouched, and the nothrow `new`
//! that calls this one turns the failure into a null pointer. Core itself catches some of these
//! failures (`CBLSWrapper::SetBytes`, qdata's error byte), so a faulted call may also return its
//! unfaulted result.

#![allow(dead_code)]

use std::backtrace::Backtrace;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;

thread_local! {
    /// Calls until the one that fails (0: none armed).
    static LEFT: Cell<u32> = const { Cell::new(0) };
    /// Whether the armed call failed.
    static FIRED: Cell<bool> = const { Cell::new(false) };
    /// Under [`outside`]: the frames looked for, and for each call whether its stack lacked them.
    static OUTSIDE: RefCell<Option<(&'static [&'static str], Vec<bool>)>> = const { RefCell::new(None) };
}

unsafe extern "C-unwind" {
    /// `std::__throw_bad_alloc()`, exported by libstdc++.
    #[link_name = "_ZSt17__throw_bad_allocv"]
    fn throw_bad_alloc() -> !;
}

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
}

/// `operator new(std::size_t)`.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
extern "C-unwind" fn _Znwm(size: usize) -> *mut c_void {
    match LEFT.get() {
        0 => {}
        1 => {
            LEFT.set(0);
            FIRED.set(true);
            unsafe { throw_bad_alloc() }
        }
        left => LEFT.set(left - 1),
    }
    // Rust's allocator is `malloc`, so the capture does not come back here.
    let frames = OUTSIDE.try_with(|o| o.try_borrow().ok()?.as_ref().map(|(frames, _)| *frames));
    if let Ok(Some(frames)) = frames {
        let stack = Backtrace::force_capture().to_string();
        let outside = !frames.iter().any(|frame| stack.contains(frame));
        let _ = OUTSIDE.try_with(|o| {
            o.try_borrow_mut().map(|mut o| o.as_mut().map(|(_, calls)| calls.push(outside)))
        });
    }
    let p = unsafe { malloc(size.max(1)) };
    if p.is_null() {
        unsafe { throw_bad_alloc() }
    }
    p
}

/// Disarms on drop, so a panicking `f` leaves nothing armed.
struct Disarm;

impl Drop for Disarm {
    fn drop(&mut self) {
        LEFT.set(0);
    }
}

/// Runs `f` with the calling thread's `nth` `operator new(std::size_t)` call (from 1) failing:
/// `None` if `f` made fewer, so none failed.
pub fn failing<T>(nth: u32, f: impl FnOnce() -> T) -> Option<T> {
    assert!(nth > 0);
    LEFT.set(nth);
    FIRED.set(false);
    let disarm = Disarm;
    let out = f();
    drop(disarm);
    FIRED.get().then_some(out)
}

/// Runs `f`, counting the calling thread's `operator new(std::size_t)` calls (none fails).
pub fn counting<T>(f: impl FnOnce() -> T) -> (T, u32) {
    LEFT.set(u32::MAX);
    FIRED.set(false);
    let disarm = Disarm;
    let out = f();
    let left = LEFT.get();
    drop(disarm);
    (out, u32::MAX - left)
}

/// Runs `f`, recording for each of the calling thread's `operator new(std::size_t)` calls (none
/// fails) whether all of `frames`, parts of (mangled) symbol names, were absent from its stack.
pub fn outside<T>(frames: &'static [&'static str], f: impl FnOnce() -> T) -> (T, Vec<bool>) {
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            OUTSIDE.set(None);
        }
    }
    OUTSIDE.set(Some((frames, Vec::new())));
    let stop = Stop;
    let out = f();
    let calls = OUTSIDE.take().map(|(_, calls)| calls).unwrap_or_default();
    drop(stop);
    (out, calls)
}
