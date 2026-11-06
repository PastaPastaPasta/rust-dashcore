//! The node's process RNG (BL-502): Dash Core's `RNGState` and `ProcRand` (`random.cpp`), its
//! seeders and `randomenv.cpp`'s environment, with the state in one chunk of the locked pool,
//! as Core's `GetRNGState` holds it through `secure_allocator`.
//!
//! [`PROC_RAND`] is the process's one state. The node seeds it in the serving process after
//! daemonizing ([`static_init`], [`random_init`]), and [`random_init`] installs it as
//! dash-p2p's process source, so the node's draws through a
//! [`RandSource`](dash_p2p::rng::RandSource) come from it. Each level is Core's:
//! - FAST ([`get_rand_bytes`], [`get_rand_hash`]): the stack pointer, hardware randomness and
//!   the performance counter, mixed with the state;
//! - SLOW ([`get_strong_rand_bytes`]): FAST, 32 bytes from the OS and the events;
//! - PERIODIC ([`rand_add_periodic`], every minute): FAST, the events, the dynamic environment
//!   and 10 ms of strengthening;
//! - STARTUP, at the state's first mix: hardware randomness, SLOW, both environments and
//!   100 ms of strengthening.
//!
//! A failed OS read logs "Failed to read randomness, aborting" and aborts, as Core's
//! `RandFailure` does.

mod entropy;
mod env;
mod events;
mod hw;
mod state;

pub use entropy::{Entropy, EnvSink, OsRandError, SystemEntropy};
pub use state::{RngState, SanityTimings};

/// Core's `sizeof(RNGState)` on this target: two `Mutex`es (the C library's
/// `pthread_mutex_t`) and 152 bytes of state. The pool rounds the chunk up to 16 bytes, so
/// `getmemoryinfo` shows Core's numbers. Only x86-64 Linux glibc is measured; the other rows
/// follow from their C library headers.
#[cfg(all(
    target_arch = "x86_64",
    target_os = "linux",
    any(target_env = "gnu", target_env = "musl")
))]
pub const RNG_STATE_SIZE: usize = 232;
/// See the x86-64 row.
#[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"))]
pub const RNG_STATE_SIZE: usize = 248;
/// See the x86-64 row.
#[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "musl"))]
pub const RNG_STATE_SIZE: usize = 232;
/// See the x86-64 row.
#[cfg(all(any(target_arch = "x86_64", target_arch = "aarch64"), target_os = "macos"))]
pub const RNG_STATE_SIZE: usize = 280;
/// See the x86-64 row.
#[cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(target_os = "freebsd", target_os = "openbsd")
))]
pub const RNG_STATE_SIZE: usize = 168;
/// A placeholder, not measured: the locked pool's Windows path is a stub (DEC-787 Q4).
#[cfg(target_os = "windows")]
pub const RNG_STATE_SIZE: usize = 232;
#[cfg(not(any(
    all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_os = "linux",
        any(target_env = "gnu", target_env = "musl")
    ),
    all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        any(target_os = "macos", target_os = "freebsd", target_os = "openbsd")
    ),
    target_os = "windows",
)))]
compile_error!("RNG_STATE_SIZE unknown for this target (BL-502 OQ-3)");

/// The process's RNG state.
pub static PROC_RAND: RngState<SystemEntropy> = RngState::new(SystemEntropy);

/// The first draw of the process, as Core's static initializers make it
/// (`StaticSaltedHasher::s`, `saltedhasher.cpp:8-10`): it makes the state and runs STARTUP,
/// without STARTUP's line on the environment's size. The node calls it in the serving process
/// after daemonizing, before [`random_init`].
pub fn static_init() {
    PROC_RAND.proc_rand(&mut [], state::Level::Fast, true);
}

/// `RandomInit`: a FAST draw, then [`PROC_RAND`] becomes dash-p2p's process source (a second
/// call leaves the first install), then `ReportHardwareRand`.
// Core: random.cpp:763-769; kernel/context.cpp:25
pub fn random_init() {
    PROC_RAND.proc_rand(&mut [], state::Level::Fast, false);
    let _ = dash_p2p::rng::install_process_source(&PROC_RAND);
    PROC_RAND.report_hardware_rand();
}

/// `Random_SanityCheck`'s checks, on the OS source and the performance counter; they touch no
/// state. `None` when a check fails.
// Core: random.cpp:697-732; kernel/checks.cpp:26-28
pub fn random_sanity_check() -> Option<SanityTimings> {
    PROC_RAND.sanity_check()
}

/// `Random_SanityCheck`'s mix of its samples into [`PROC_RAND`].
// Core: random.cpp:734-738
pub fn mix_sanity_timings(t: SanityTimings) {
    PROC_RAND.mix_sanity_timings(t);
}

/// `GetRandBytes`. A draw over 32 bytes does not compile:
///
/// ```compile_fail,E0080
/// dashd_random::get_rand_bytes(&mut [0u8; 33]);
/// ```
pub fn get_rand_bytes<const N: usize>(out: &mut [u8; N]) {
    PROC_RAND.rand_bytes(out);
}

/// `GetStrongRandBytes`. A draw over 32 bytes does not compile:
///
/// ```compile_fail,E0080
/// dashd_random::get_strong_rand_bytes(&mut [0u8; 33]);
/// ```
pub fn get_strong_rand_bytes<const N: usize>(out: &mut [u8; N]) {
    PROC_RAND.strong_rand_bytes(out);
}

/// `GetRandHash`: 32 bytes of `GetRandBytes`.
pub fn get_rand_hash() -> [u8; 32] {
    let mut out = [0; 32];
    get_rand_bytes(&mut out);
    out
}

/// `RandAddPeriodic`.
pub fn rand_add_periodic() {
    PROC_RAND.add_periodic();
}

/// `RandAddEvent`.
pub fn rand_add_event(info: u32) {
    PROC_RAND.add_event(info);
}

/// Whether [`PROC_RAND`]'s state exists (tests of the init order).
#[doc(hidden)]
pub fn state_created() -> bool {
    PROC_RAND.created()
}

/// Core's `RandFailure`: the line, unconditional, then the process aborts.
// Core: random.cpp:46-50
pub(crate) fn rand_failure() -> ! {
    tracing::info!("Failed to read randomness, aborting");
    std::process::abort()
}

/// Core's failed `assert(num <= 32)` in `ProcRand`: the message on stderr, then the process
/// aborts.
// Core: random.cpp:610; util/check.h:49-51
pub(crate) fn draw_too_large() -> ! {
    use std::io::Write as _;
    let _ = writeln!(
        std::io::stderr(),
        "{}:{}: ProcRand: Assertion `num <= 32' failed.",
        file!(),
        line!()
    );
    std::process::abort()
}

#[cfg(test)]
mod tests;
