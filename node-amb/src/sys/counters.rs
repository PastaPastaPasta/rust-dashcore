//! One `instructions:u` counter per thread (rev3 N3, rev4): `perf_event_open` on the TID,
//! `exclude_kernel`, `exclude_hv`, `inherit = 0`, `pinned = 1`, created enabled, read as
//! (value, time enabled, time running). Counts are never scaled: [`crate::account::delta`]
//! refuses an interval whose running time is short of its enabled time, and a read that is not
//! the full 24 bytes (a pinned event in its error state reads 0) refuses here.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::account::Reading;
use crate::{Result, refuse};

#[repr(C)]
#[derive(Default)]
struct Attr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
}

const PERF_TYPE_HARDWARE: u32 = 0;
const PERF_COUNT_HW_INSTRUCTIONS: u64 = 1;
const FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
const FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;
const FLAG_INHERIT: u64 = 1 << 1;
const FLAG_PINNED: u64 = 1 << 2;
const FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
const FLAG_EXCLUDE_HV: u64 = 1 << 6;
const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;

/// The flags every gate counter is opened with: pinned, user only, no inheritance.
pub const FLAGS: u64 = FLAG_PINNED | FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV;
const _: () = assert!(FLAGS & FLAG_INHERIT == 0);

/// One thread's counter.
#[derive(Debug)]
pub struct Counter {
    pub tid: i32,
    fd: OwnedFd,
}

/// The raw read buffer: value, time enabled, time running.
pub type Raw = [u64; 3];

impl Counter {
    /// Opens `tid`'s counter with [`FLAGS`]; refuses on any failure (an attach is never dropped,
    /// F4).
    pub fn open(tid: i32) -> Result<Counter> {
        Counter::open_with(tid, FLAGS)
    }

    /// [`Counter::open`] with other flags (the contention test's extra events).
    pub fn open_with(tid: i32, flags: u64) -> Result<Counter> {
        let attr = Attr {
            type_: PERF_TYPE_HARDWARE,
            size: std::mem::size_of::<Attr>() as u32,
            config: PERF_COUNT_HW_INSTRUCTIONS,
            read_format: FORMAT_TOTAL_TIME_ENABLED | FORMAT_TOTAL_TIME_RUNNING,
            flags,
            ..Attr::default()
        };
        // SAFETY: `attr` is a valid perf_event_attr of the declared size.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr as *const Attr,
                tid,
                -1 as libc::c_int,
                -1 as libc::c_int,
                PERF_FLAG_FD_CLOEXEC,
            )
        };
        if fd < 0 {
            refuse!("perf_event_open on thread {tid}: {}", std::io::Error::last_os_error());
        }
        // SAFETY: the kernel returned a new descriptor we own.
        Ok(Counter {
            tid,
            fd: unsafe { OwnedFd::from_raw_fd(fd as i32) },
        })
    }

    pub fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    pub fn read(&self) -> Result<Reading> {
        let mut buf: Raw = [0; 3];
        let n = read_raw(self.raw_fd(), &mut buf);
        to_reading(self.tid, n, buf)
    }
}

/// One raw `read(2)` of a counter into `buf`; returns its result.
#[inline(always)]
pub fn read_raw(fd: i32, buf: &mut Raw) -> isize {
    // SAFETY: reads at most 24 bytes into a 24-byte buffer.
    unsafe { libc::read(fd, buf.as_mut_ptr().cast(), std::mem::size_of::<Raw>()) }
}

/// A raw read's reading; refuses a failed or short one.
pub fn to_reading(tid: i32, n: isize, buf: Raw) -> Result<Reading> {
    if n != std::mem::size_of::<Raw>() as isize {
        refuse!("a {n}-byte read of thread {tid}'s counter (a pinned event in error reads 0)");
    }
    Ok(Reading {
        value: buf[0],
        enabled: buf[1],
        running: buf[2],
    })
}

/// Whether this process may count its own threads (`perf_event_paranoid` ≤ 2 and a PMU).
pub fn available() -> bool {
    Counter::open(super::gettid()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_read_refuses() {
        assert!(to_reading(1, 0, [0; 3]).unwrap_err().0.contains("0-byte"));
        assert!(to_reading(1, -1, [0; 3]).is_err());
        assert_eq!(to_reading(1, 24, [5, 6, 6]).unwrap().value, 5);
    }

    #[test]
    fn a_counter_counts_its_thread_pinned() {
        if !available() {
            crate::sys::supervisor::no_pmu("a_counter_counts_its_thread_pinned");
            return;
        }
        let c = Counter::open(crate::sys::gettid()).unwrap();
        let a = c.read().unwrap();
        let mut x = 0u64;
        for i in 0..100_000u64 {
            x = std::hint::black_box(x.wrapping_add(i));
        }
        let b = c.read().unwrap();
        assert!(crate::account::delta(a, b).unwrap() > 100_000);
    }
}
