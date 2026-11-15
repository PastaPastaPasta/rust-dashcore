//! P3's (A − B) counting harness (P3 §5.1, D35; the Core-leg plan rev8, OD-P3-8): one harness
//! for the node leg (dashd-node's `p2p::amb`) and the Core leg (node-l1's `amb_core`).
//!
//! A measured pass runs in its own child process ([`sys::supervisor`]). The child builds its
//! pass, then [`sys::window::open`]s the window:
//!
//! 1. the caller's kernel mask blocks every signal (a raw `rt_sigprocmask`), and every other
//!    thread's is set the same through a `uc_sigmask` handler; each TID's `SigBlk` in `/proc`
//!    must then read [`sys::mask::FULL_SIGBLK`] ([`sys::mask`]);
//! 2. a seccomp filter, synced to every thread, kills the process on thread or process
//!    creation, an unmask, a temporary mask, `rt_sigreturn`, signalfd, a self-queued or reserved
//!    signal, exec, and any other syscall ABI ([`sys::guard`], audited in `KERNEL-AUDIT.md`);
//! 3. one pinned, non-inherited `instructions:u` counter per pre-existing TID, read with its
//!    enabled and running times, never scaled ([`sys::counters`]).
//!
//! The pure accounting over those readings ([`account`]), the record a child writes
//! ([`record`]) and the comparison that refuses on any mismatch ([`compare`]) are unit-tested
//! with supplied counts. The blocks both legs measure are [`fixture`]'s.

pub mod account;
pub mod compare;
pub mod fixture;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod fnv;
pub mod record;
mod refusal;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod sys;

pub use refusal::{Refusal, Result};

/// The kernel release prefix `KERNEL-AUDIT.md` audited the guard against (upstream v7.0): the
/// comparison refuses a record from any other.
pub const AUDITED_KERNEL: &str = "7.0.";
