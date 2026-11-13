# The guard's kernel audit (plan rev7/rev8, condition 2)

The window guard (`src/sys/guard.rs`) must cover every own-syscall route that can leave a
measured thread's signal mask reduced at userspace return. This file records the audit that
backs it, with the kernel tag.

- **Source audited:** upstream Linux **v7.0** (`linux-7.0.tar.xz`, sha256
  `bb7f6d80b387c757b7d14bb93028fcb90f793c5c0d367736ee815a100b3891f0`, Makefile
  `VERSION = 7`, `PATCHLEVEL = 0`).
- **Running kernel:** `uname -r` = **7.0.0-38-generic** (Ubuntu), x86-64, glibc 2.43.
  `node_amb::AUDITED_KERNEL` is `7.0.`: the comparison refuses a record from any other
  release.
- **Search:** every caller of `set_user_sigmask`, `set_restore_sigmask`, `set_current_blocked`,
  `__set_current_blocked`, `__set_task_blocked` and the kernel `sigprocmask`, every assignment
  to `->blocked` or `->real_blocked`, and every sigset helper (`sigemptyset`, `sigfillset`,
  `sigdelset`, `sigandsets`, `siginitsetinv`, …) applied to a task's `->blocked` or
  `->real_blocked`, across the whole tree (194 lines, with the command, in
  [`KERNEL-AUDIT-grep.txt`](KERNEL-AUDIT-grep.txt)). It covers every mask-write site of r8's
  full audit (`kernel-full-audit.txt`); the r8 lines it does not match read `->blocked` into a
  local (`sigorsets(&blocked, &current->blocked, …)`) that then reaches one of the setters, or
  are driver flags. Text matches that are not signal masks (`lu->blocked`, `fc->blocked`,
  `q->blocked` and similar driver flags) and reads (`sigequalsets`, `kernel/signal.c:3217`) are
  excluded. The other architectures' signal code (alpha … xtensa, UML) is outside the native
  x86-64 ABI the guard admits.

## Classification

Each site is one of: **denied** (the guard kills the syscall), **ABI-excluded** (not reachable
through the native x86-64 table the guard admits), **restored in-kernel** (the old mask is back
before the syscall returns), **fatal** (a forced signal whose disposition is reset, so the child
dies, a refusal), **no-exec** (the pass never execs), or **SIG_BLOCK only** (it can only add
signals).

| Site (v7.0) | Route | Class | Kill test (`tests/harness.rs`) |
|---|---|---|---|
| `kernel/signal.c:3233–3256` `sigprocmask`, via `:3333` `rt_sigprocmask` | `rt_sigprocmask` | SIG_BLOCK only (`how != SIG_BLOCK` killed) | `setmask`, `unblock` |
| `kernel/signal.c:3363` compat `rt_sigprocmask` | compat ABI | ABI-excluded | `x32`, `i386` |
| `arch/x86/kernel/signal_64.c:263` | `rt_sigreturn` | denied | `rt_sigreturn` |
| `arch/x86/kernel/signal_64.c:382` | x32 `rt_sigreturn` (513) | ABI-excluded | `x32` |
| `arch/x86/kernel/signal_32.c:163,189` | i386 `sigreturn`/`rt_sigreturn` | ABI-excluded | `i386` |
| `kernel/signal.c:3270–3283` `set_user_sigmask` ← `fs/select.c:755` | `pselect6` | denied unless arg 5 is NULL | `pselect6-desc` (and `allowed` for NULL) |
| ← `fs/select.c:1102` | `ppoll` | denied unless arg 3 is NULL | `ppoll-mask`, `ppoll-mask-high` |
| ← `fs/select.c:1128` | time32 `ppoll` | ABI-excluded | `x32`, `i386` |
| ← `fs/eventpoll.c:2492` | `epoll_pwait`, `epoll_pwait2` | denied unless arg 4 is NULL | `epoll_pwait-mask`, `epoll_pwait2-mask` |
| ← `fs/aio.c:2294` | `io_pgetevents` | denied | `io_pgetevents` (r7's adversary) |
| ← `fs/aio.c:2330` | time32 `io_pgetevents` | ABI-excluded | `x32`, `i386` |
| ← `io_uring/wait.c:243` | `io_uring_enter` | denied (the io_uring family whole) | `io_uring_setup`, `io_uring_enter`, `io_uring_register` |
| `kernel/signal.c:3290–3303` compat `set_user_sigmask` | compat ABI | ABI-excluded | `x32`, `i386` |
| `kernel/signal.c:4826–4840` `sigsuspend` | `rt_sigsuspend` | denied | `rt_sigsuspend` |
| `kernel/signal.c:3772–3784` (`real_blocked` saved, `sigandsets` on `blocked`, restored) | `rt_sigtimedwait` | denied | `rt_sigtimedwait` |
| `kernel/signal.c:4607` | legacy `sigprocmask` | ABI-excluded (not in `syscall_64.tbl`) | — |
| `kernel/signal.c:4793` | `ssetmask` | ABI-excluded (not in `syscall_64.tbl`) | — |
| `kernel/compat.c:70` | compat `sigprocmask` | ABI-excluded | `x32`, `i386` |
| `kernel/signal.c:3070` `signal_delivered` | after a handler is chosen | SIG_BLOCK only (adds `sa_mask`; no handler can be chosen with every signal blocked) | — |
| `include/linux/sched/signal.h:543` | restore of a saved mask | restores a mask a denied route saved | — |
| `kernel/signal.c:1306–1311` `force_sig_info_to_task` (`sigdelset(&t->blocked, sig)`) | synchronous faults | fatal (disposition reset to SIG_DFL when blocked) | r8 probe `combined-result.txt`; self-queued variant: `rt_tgsigqueueinfo`, `rt_sigqueueinfo` (denied, rev8) |
| `kernel/ptrace.c:1222` `PTRACE_SETSIGMASK` | an external tracer | outside the own-syscall audit (self-attach refused) | — |
| `security/selinux/hooks.c:2568` (`sigemptyset(&current->blocked)`) | exec domain transition | no-exec (`execve`, `execveat` denied) | `execve`, `execveat` |
| `fs/coda/upcall.c:621–633` (`sigfillset`, `sigdelset` on `current->blocked`) | Coda upcall | restored in-kernel (`:675–677`, `:691–693`) | — |
| `virt/kvm/kvm_main.c:3574,3582,3583` | `KVM_RUN` after `KVM_SET_SIGNAL_MASK` | restored in-kernel (x86 deactivates before return) | — |
| `fs/smb/client/transport.c:287,334`, `fs/ceph/addr.c:1936,1941`, `fs/ocfs2/super.c:2566,2572` | filesystem operations | restored in-kernel (they only add signals meanwhile) | — |
| `kernel/fork.c:2062` (`siginitsetinv(&p->blocked, …)` for a user worker) | a new io_uring or vhost worker thread, created fully masked | not a measured thread's mask (a new thread; io_uring is denied, and the census refuses any thread born in the window) | `io_uring_setup` |
| `fs/jffs2/background.c:92,152`, `drivers/mfd/ab8500-sysctrl.c:85,89`, `drivers/isdn/mISDN/stack.c:202` | kernel threads, platform drivers | not a measured userspace thread's syscall | — |

Other guard entries, outside the mask inventory: thread and process creation (`clone`,
`clone3`, `fork`, `vfork`: `thread`, `fork`, `raw-fork`, `vfork`, `clone3`), `signalfd` and
`signalfd4` (`signalfd`, `signalfd4`), and sends of glibc's reserved signals 32 and 33 by
`tgkill` and `tkill` (`setresuid`, `pthread_cancel`, `tgkill-32`, `tkill-33`). The arch check
kills any non-x86-64 `seccomp_data.arch` and any number with `__X32_SYSCALL_BIT` (`i386`, `x32`).
`pidfd_send_signal` takes a caller-built `siginfo` like `rt_sigqueueinfo` and is denied with it
(`pidfd_send_signal`). `kill` and `tgkill` of other signals stay allowed: what they send is
blocked, so it stays pending and the window's pending check refuses
(`external_signals_stay_pending_and_refuse`).

Workload compatibility (r8): mio's `epoll_wait`, glibc's `poll`/`epoll_wait` wrappers (NULL
masks), glibc's `select` (`pselect6` with a NULL sixth argument), condition variables (`futex`)
and allocation are allowed (`the_guard_allows_the_workloads_calls`). tokio's io_uring driver
needs `tokio_unstable` and its `io-uring` feature, both off; enabling it would make the harness
refuse.

A different kernel needs this audit redone against its tag before `AUDITED_KERNEL` changes.
