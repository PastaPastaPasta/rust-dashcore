//! The window's seccomp guard (rev4–rev8), installed after the masks with `TSYNC` on every
//! thread, after `PR_SET_NO_NEW_PRIVS`, and never lifted: the child writes its record and exits
//! under it. `SECCOMP_RET_KILL_PROCESS` on (KERNEL-AUDIT.md lists the mechanism behind each):
//!
//! - any arch but `AUDIT_ARCH_X86_64`, and any syscall number with `__X32_SYSCALL_BIT`;
//! - thread or process creation: `clone` (glibc's `fork` is a `clone` without `CLONE_THREAD`),
//!   `clone3`, `fork`, `vfork`; and `execve`, `execveat` (the pass never execs);
//! - `rt_sigprocmask` with `how != SIG_BLOCK`; `rt_sigreturn`;
//! - every syscall that installs a temporary user mask: `rt_sigsuspend`, `rt_sigtimedwait`,
//!   `io_pgetevents`, `io_uring_setup`, `io_uring_enter`, `io_uring_register`; and `ppoll`,
//!   `epoll_pwait`, `epoll_pwait2` and `pselect6` unless their mask (`pselect6`: its sixth
//!   argument) is NULL in both 32-bit halves;
//! - `signalfd`, `signalfd4`; `rt_sigqueueinfo`, `rt_tgsigqueueinfo` (rev8), and
//!   `pidfd_send_signal`, which queues a caller-built `siginfo` the same way;
//! - `tgkill` and `tkill` of signal 32 or 33 (glibc's reserved signals).
//!
//! Everything else is allowed.

use crate::{Result, refuse};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

#[repr(C)]
struct Prog {
    len: libc::c_ushort,
    filter: *const Insn,
}

const LD_W_ABS: u16 = 0x20;
const JEQ_K: u16 = 0x15;
const JSET_K: u16 = 0x45;
const RET_K: u16 = 0x06;
const KILL_PROCESS: u32 = 0x8000_0000;
const ALLOW: u32 = 0x7fff_0000;
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
const X32_SYSCALL_BIT: u32 = 0x4000_0000;
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const SECCOMP_SET_MODE_FILTER: libc::c_uint = 1;
const SECCOMP_FILTER_FLAG_TSYNC: libc::c_uint = 1;

/// Native x86-64 syscall numbers (arch/x86/entry/syscalls/syscall_64.tbl, v7.0).
pub mod nr {
    pub const RT_SIGPROCMASK: u32 = 14;
    pub const RT_SIGRETURN: u32 = 15;
    pub const CLONE: u32 = 56;
    pub const FORK: u32 = 57;
    pub const VFORK: u32 = 58;
    pub const EXECVE: u32 = 59;
    pub const RT_SIGTIMEDWAIT: u32 = 128;
    pub const RT_SIGQUEUEINFO: u32 = 129;
    pub const RT_SIGSUSPEND: u32 = 130;
    pub const TKILL: u32 = 200;
    pub const TGKILL: u32 = 234;
    pub const PSELECT6: u32 = 270;
    pub const PPOLL: u32 = 271;
    pub const EPOLL_PWAIT: u32 = 281;
    pub const SIGNALFD: u32 = 282;
    pub const SIGNALFD4: u32 = 289;
    pub const RT_TGSIGQUEUEINFO: u32 = 297;
    pub const EXECVEAT: u32 = 322;
    pub const IO_PGETEVENTS: u32 = 333;
    pub const PIDFD_SEND_SIGNAL: u32 = 424;
    pub const IO_URING_SETUP: u32 = 425;
    pub const IO_URING_ENTER: u32 = 426;
    pub const IO_URING_REGISTER: u32 = 427;
    pub const CLONE3: u32 = 435;
    pub const EPOLL_PWAIT2: u32 = 441;
}

/// Killed whatever their arguments.
pub const DENIED: [u32; 18] = [
    nr::CLONE,
    nr::CLONE3,
    nr::FORK,
    nr::VFORK,
    nr::EXECVE,
    nr::EXECVEAT,
    nr::RT_SIGRETURN,
    nr::RT_SIGSUSPEND,
    nr::RT_SIGTIMEDWAIT,
    nr::IO_PGETEVENTS,
    nr::IO_URING_SETUP,
    nr::IO_URING_ENTER,
    nr::IO_URING_REGISTER,
    nr::SIGNALFD,
    nr::SIGNALFD4,
    nr::RT_SIGQUEUEINFO,
    nr::RT_TGSIGQUEUEINFO,
    nr::PIDFD_SEND_SIGNAL,
];

/// Allowed only with a NULL pointer argument: (syscall, argument index).
pub const NULL_MASK_ONLY: [(u32, u32); 4] =
    [(nr::PPOLL, 3), (nr::EPOLL_PWAIT, 4), (nr::EPOLL_PWAIT2, 4), (nr::PSELECT6, 5)];

/// Killed when the signal argument is 32 or 33: (syscall, argument index).
pub const RESERVED_SENDS: [(u32, u32); 2] = [(nr::TGKILL, 2), (nr::TKILL, 1)];

fn ld(off: u32) -> Insn {
    Insn {
        code: LD_W_ABS,
        jt: 0,
        jf: 0,
        k: off,
    }
}

fn jeq(k: u32, jt: u8, jf: u8) -> Insn {
    Insn {
        code: JEQ_K,
        jt,
        jf,
        k,
    }
}

fn ret(k: u32) -> Insn {
    Insn {
        code: RET_K,
        jt: 0,
        jf: 0,
        k,
    }
}

fn arg_lo(i: u32) -> u32 {
    16 + 8 * i
}

/// The filter program.
pub fn program() -> Vec<Insn> {
    let mut p = vec![
        ld(OFF_ARCH),
        jeq(AUDIT_ARCH_X86_64, 1, 0),
        ret(KILL_PROCESS),
        ld(OFF_NR),
        Insn {
            code: JSET_K,
            jt: 0,
            jf: 1,
            k: X32_SYSCALL_BIT,
        },
        ret(KILL_PROCESS),
    ];
    for &n in &DENIED {
        p.extend([ld(OFF_NR), jeq(n, 0, 1), ret(KILL_PROCESS)]);
    }
    // rt_sigprocmask: allowed only with how == SIG_BLOCK (an int: the low half).
    p.extend([
        ld(OFF_NR),
        jeq(nr::RT_SIGPROCMASK, 0, 3),
        ld(arg_lo(0)),
        jeq(libc::SIG_BLOCK as u32, 1, 0),
        ret(KILL_PROCESS),
    ]);
    for &(n, i) in &NULL_MASK_ONLY {
        p.extend([
            ld(OFF_NR),
            jeq(n, 0, 5),
            ld(arg_lo(i)),
            jeq(0, 0, 2),
            ld(arg_lo(i) + 4),
            jeq(0, 1, 0),
            ret(KILL_PROCESS),
        ]);
    }
    for &(n, i) in &RESERVED_SENDS {
        p.extend([
            ld(OFF_NR),
            jeq(n, 0, 4),
            ld(arg_lo(i)),
            jeq(32, 1, 0),
            jeq(33, 0, 1),
            ret(KILL_PROCESS),
        ]);
    }
    p.push(ret(ALLOW));
    p
}

/// seccomp's `TSYNC` return: 0, or the TID of a thread that could not sync.
pub fn check_tsync(ret: libc::c_long, errno: i32) -> Result<()> {
    match ret {
        0 => Ok(()),
        tid if tid > 0 => refuse!("seccomp TSYNC failed on thread {tid}"),
        _ => refuse!("seccomp: {}", std::io::Error::from_raw_os_error(errno)),
    }
}

/// Installs the guard on every thread of the process. A refusal leaves the process unfiltered
/// only if `PR_SET_NO_NEW_PRIVS` or the filter itself failed.
pub fn install() -> Result<()> {
    let p = program();
    let prog = Prog {
        len: p.len() as libc::c_ushort,
        filter: p.as_ptr(),
    };
    // SAFETY: prctl with integer arguments.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        refuse!("PR_SET_NO_NEW_PRIVS: {}", std::io::Error::last_os_error());
    }
    // SAFETY: `prog` points at `p`, alive for the call; the kernel copies it.
    let r = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            SECCOMP_FILTER_FLAG_TSYNC,
            &prog as *const Prog,
        )
    };
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    check_tsync(r, errno)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thread that could not sync refuses (an injected return: a live TSYNC failure needs a
    /// thread with a diverging filter).
    #[test]
    fn a_non_zero_tsync_return_refuses() {
        assert!(check_tsync(0, 0).is_ok());
        assert!(check_tsync(4242, 0).unwrap_err().0.contains("thread 4242"));
        assert!(check_tsync(-1, libc::EINVAL).is_err());
    }

    /// Every jump lands inside the program, and the last instruction allows.
    #[test]
    fn the_program_is_well_formed() {
        let p = program();
        assert!(p.len() < 256);
        for (i, insn) in p.iter().enumerate() {
            if insn.code & 0x07 == 0x05 {
                assert!(i + 1 + (insn.jt.max(insn.jf) as usize) < p.len(), "jump at {i}");
            }
        }
        assert_eq!(*p.last().unwrap(), ret(ALLOW));
    }

    /// The numbers are the native ABI's (libc's view of x86-64; libc has no `io_pgetevents`,
    /// 333 in `syscall_64.tbl`).
    #[test]
    fn the_numbers_are_x86_64_s() {
        let n = |x: libc::c_long| x as u32;
        assert_eq!(
            [
                nr::RT_SIGPROCMASK,
                nr::RT_SIGRETURN,
                nr::CLONE,
                nr::FORK,
                nr::VFORK,
                nr::EXECVE,
                nr::RT_SIGTIMEDWAIT,
                nr::RT_SIGQUEUEINFO,
                nr::RT_SIGSUSPEND,
                nr::TKILL,
                nr::TGKILL,
                nr::PSELECT6,
                nr::PPOLL,
                nr::EPOLL_PWAIT,
                nr::SIGNALFD,
                nr::SIGNALFD4,
                nr::RT_TGSIGQUEUEINFO,
                nr::EXECVEAT,
                nr::IO_URING_SETUP,
                nr::IO_URING_ENTER,
                nr::IO_URING_REGISTER,
                nr::PIDFD_SEND_SIGNAL,
                nr::CLONE3,
            ],
            [
                n(libc::SYS_rt_sigprocmask),
                n(libc::SYS_rt_sigreturn),
                n(libc::SYS_clone),
                n(libc::SYS_fork),
                n(libc::SYS_vfork),
                n(libc::SYS_execve),
                n(libc::SYS_rt_sigtimedwait),
                n(libc::SYS_rt_sigqueueinfo),
                n(libc::SYS_rt_sigsuspend),
                n(libc::SYS_tkill),
                n(libc::SYS_tgkill),
                n(libc::SYS_pselect6),
                n(libc::SYS_ppoll),
                n(libc::SYS_epoll_pwait),
                n(libc::SYS_signalfd),
                n(libc::SYS_signalfd4),
                n(libc::SYS_rt_tgsigqueueinfo),
                n(libc::SYS_execveat),
                n(libc::SYS_io_uring_setup),
                n(libc::SYS_io_uring_enter),
                n(libc::SYS_io_uring_register),
                n(libc::SYS_pidfd_send_signal),
                n(libc::SYS_clone3),
            ]
        );
    }
}
