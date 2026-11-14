//! The caller's read pair (rev4, rev5 calibration contract): one `#[inline(never)]` wrapper,
//! the same function and path for the calibration's empty pairs and every counted call. It
//! makes two raw `read(2)`s of the caller's counter into stack buffers and allocates nothing.
//! The supervisor runs each child with `LD_BIND_NOW=1` (and Rust links test binaries `-z now`),
//! and [`warm`] runs the wrapper during prewarm, so no lazy binding remains in the window. With
//! no handler able to run, the wrapper's retired user instructions are fixed, so the maximum
//! over N empty pairs bounds each counted pair.

use super::counters::{self, Raw, read_raw};
use crate::Result;
use crate::account::Reading;

/// One pair's raw reads and their results, and the wrapper that took them. Its fields are
/// private, so only [`read_pair`] makes one: a count taken by any other path has no `RawPair` to
/// show, and one taken by another wrapper names that wrapper.
#[derive(Clone, Copy, Debug)]
pub struct RawPair {
    before: Raw,
    before_n: isize,
    after: Raw,
    after_n: isize,
    wrapper: usize,
}

/// Reads `fd`, runs `between`, reads `fd` again.
#[inline(never)]
pub fn read_pair(fd: i32, between: &mut dyn FnMut()) -> RawPair {
    let mut before: Raw = [0; 3];
    let before_n = read_raw(fd, &mut before);
    between();
    let mut after: Raw = [0; 3];
    let after_n = read_raw(fd, &mut after);
    RawPair {
        before,
        before_n,
        after,
        after_n,
        wrapper: read_pair as fn(i32, &mut dyn FnMut()) -> RawPair as usize,
    }
}

impl RawPair {
    /// The two reads of `tid`'s counter; refuses a failed or short one.
    pub fn readings(&self, tid: i32) -> Result<(Reading, Reading)> {
        Ok((
            counters::to_reading(tid, self.before_n, self.before)?,
            counters::to_reading(tid, self.after_n, self.after)?,
        ))
    }

    /// The address of the wrapper that took the pair.
    pub fn wrapper(&self) -> usize {
        self.wrapper
    }
}

/// The calibration's empty body.
#[inline(never)]
pub fn nothing() {}

/// Runs the wrapper on `fd` before the guard, so its path is bound and paged in.
pub fn warm(fd: i32) {
    for _ in 0..16 {
        std::hint::black_box(read_pair(fd, &mut nothing));
    }
}
