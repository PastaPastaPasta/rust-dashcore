//! The masking protocol (rev5 N3(b), rev6): every thread's kernel signal mask blocks every
//! signal but SIGKILL and SIGSTOP before the guard goes in, checked against the kernel.
//!
//! 1. The caller: a raw `rt_sigprocmask(SIG_BLOCK, all 64 bits)`; glibc's wrapper would filter
//!    its reserved signals 32 (SIGCANCEL) and 33 (SIGSETXID).
//! 2. Every other TID: an `SA_SIGINFO` handler for [`mask_signal`] (which must have had none)
//!    sets the interrupted context's `uc_sigmask` to all ones, so the thread's own
//!    `rt_sigreturn` installs the full mask, with no code in its loops. The acks are progress
//!    only; the completion condition is step 3.
//! 3. Each TID's `SigBlk` in `/proc/self/task/<tid>/status` must read [`FULL_SIGBLK`]. It also
//!    reads full while the handler still runs (its `sa_mask` is full), so step 3 does not prove
//!    the handler returned: a thread whose `rt_sigreturn` comes after the guard dies by SIGSYS
//!    and the pass is refused, never counted. Held threads are awaited asleep first
//!    ([`await_asleep`]).
//!
//! At the window's end every TID's `SigBlk` is read again (unchanged), and any pending signal,
//! the thread's (`SigPnd`) or the process's (`ShdPnd`), refuses ([`pending`]). The census also
//! refuses an open signalfd ([`no_signalfd`]), which could consume a blocked signal.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::{Result, refuse};

/// Every signal but 9 (SIGKILL) and 19 (SIGSTOP), which the kernel never blocks.
pub const FULL_SIGBLK: u64 = 0xffff_ffff_fffb_feff;
/// How long the protocol waits for a thread to take its mask.
const PATIENCE: Duration = Duration::from_secs(20);

/// `SIGRTMIN + 8`: the mask signal.
pub fn mask_signal() -> libc::c_int {
    libc::SIGRTMIN() + 8
}

static ACKS: AtomicUsize = AtomicUsize::new(0);

type Handler = extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void);

fn handler() -> usize {
    on_mask as Handler as usize
}

extern "C" fn on_mask(_: libc::c_int, _: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    let uc = ctx.cast::<libc::ucontext_t>();
    // SAFETY: the kernel passes the interrupted context; the first 8 bytes of its
    // `uc_sigmask` are the kernel's mask, which `rt_sigreturn` installs.
    unsafe { (&raw mut (*uc).uc_sigmask).cast::<u64>().write_unaligned(!0) };
    ACKS.fetch_add(1, Ordering::SeqCst);
}

/// Step 1, on the calling thread.
pub fn block_caller() -> Result<()> {
    let all: u64 = !0;
    // SAFETY: an 8-byte set, no old set.
    let r = unsafe {
        libc::syscall(
            libc::SYS_rt_sigprocmask,
            libc::SIG_BLOCK,
            &all as *const u64,
            std::ptr::null_mut::<u64>(),
            8usize,
        )
    };
    if r != 0 {
        refuse!("rt_sigprocmask: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// Step 2 for every TID in `tids` but `me` (and `skip`, the mask-gap test's), then step 3 for
/// those it signalled.
pub fn mask_others(tids: &BTreeSet<i32>, me: i32, skip: Option<i32>) -> Result<()> {
    let m = mask_signal();
    // SAFETY: sigaction is plain data; the calls read and set `m`'s disposition.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(m, std::ptr::null(), &mut old) != 0 {
            refuse!("sigaction: {}", std::io::Error::last_os_error());
        }
        if old.sa_sigaction != libc::SIG_DFL && old.sa_sigaction != handler() {
            refuse!("signal {m} already has a handler");
        }
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler();
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigfillset(&mut sa.sa_mask);
        if libc::sigaction(m, &sa, std::ptr::null_mut()) != 0 {
            refuse!("sigaction: {}", std::io::Error::last_os_error());
        }
    }
    let base = ACKS.load(Ordering::SeqCst);
    // SAFETY: getpid has no preconditions.
    let pid = unsafe { libc::getpid() };
    let sent: Vec<i32> = tids.iter().copied().filter(|&t| t != me && Some(t) != skip).collect();
    for &tid in &sent {
        // SAFETY: a signal to a thread of this process.
        if unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, m) } != 0 {
            refuse!("tgkill {tid}: {}", std::io::Error::last_os_error());
        }
    }
    let deadline = Instant::now() + PATIENCE;
    while ACKS.load(Ordering::SeqCst) - base < sent.len() {
        if Instant::now() > deadline {
            refuse!(
                "{} of {} threads acknowledged the mask",
                ACKS.load(Ordering::SeqCst) - base,
                sent.len()
            );
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    // The ack comes before the handler returns, so the kernel mask is what is awaited (step 3,
    // with its handler-tail caveat).
    for &tid in &sent {
        while sigblk(tid)? != FULL_SIGBLK {
            if Instant::now() > deadline {
                refuse!("thread {tid}'s SigBlk is {:016x}", sigblk(tid)?);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

/// Waits until `tid` sleeps (`S`): out of its mask handler and back in its own wait.
pub fn await_asleep(tid: i32) -> Result<()> {
    let deadline = Instant::now() + PATIENCE;
    while super::state(tid)? != 'S' {
        if Instant::now() > deadline {
            refuse!("thread {tid} is not asleep after its mask");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

/// A hex field of a `/proc` status file.
fn status_field(path: &str, key: &str) -> Result<u64> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => refuse!("{path}: {e}"),
    };
    let Some(v) = text.lines().find_map(|l| l.strip_prefix(key)) else {
        refuse!("{path} has no {key}");
    };
    match u64::from_str_radix(v.trim(), 16) {
        Ok(v) => Ok(v),
        Err(e) => refuse!("{path} {key} {v:?}: {e}"),
    }
}

/// `tid`'s kernel blocked mask.
pub fn sigblk(tid: i32) -> Result<u64> {
    status_field(&format!("/proc/self/task/{tid}/status"), "SigBlk:")
}

/// Step 3: every TID blocks every maskable signal.
pub fn verify(tids: &BTreeSet<i32>) -> Result<()> {
    for &tid in tids {
        let blk = sigblk(tid)?;
        if blk != FULL_SIGBLK {
            refuse!("thread {tid}'s SigBlk is {blk:016x}, not {FULL_SIGBLK:016x}");
        }
    }
    Ok(())
}

/// Step 6's pending-at-end check: no thread's signal and no process signal is pending.
pub fn pending(tids: &BTreeSet<i32>) -> Result<()> {
    for &tid in tids {
        let p = status_field(&format!("/proc/self/task/{tid}/status"), "SigPnd:")?;
        if p != 0 {
            refuse!("thread {tid} has signals {p:016x} pending at the window's end");
        }
    }
    let shared = status_field("/proc/self/status", "ShdPnd:")?;
    if shared != 0 {
        refuse!("process signals {shared:016x} pending at the window's end");
    }
    Ok(())
}

/// rev6: no open descriptor is a signalfd.
pub fn no_signalfd() -> Result<()> {
    let dir = match std::fs::read_dir("/proc/self/fd") {
        Ok(d) => d,
        Err(e) => refuse!("/proc/self/fd: {e}"),
    };
    for e in dir {
        let path = match e {
            Ok(e) => e.path(),
            Err(e) => refuse!("/proc/self/fd: {e}"),
        };
        match std::fs::read_link(&path) {
            Ok(l) if l.as_os_str() == "anon_inode:[signalfd]" => {
                refuse!("descriptor {path:?} is a signalfd")
            }
            Ok(_) => {}
            // A descriptor another thread closed after the listing named it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => refuse!("{path:?}: {e}"),
        }
    }
    Ok(())
}
