//! Every input the state reads from outside the process, behind [`Entropy`]; [`SystemEntropy`]
//! is the real one.

use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::{env, hw};

/// Takes the bytes the environment functions write: the state's SHA-512 in production, a
/// recorder in tests.
pub trait EnvSink {
    /// Core's `hasher.Write(bytes)`.
    fn write(&mut self, bytes: &[u8]);

    /// A mark before each item [`Entropy::dynamic_env`] writes (a clock, `rusage`, a file's
    /// path, the stack and heap addresses), for recorders. Hashers ignore it.
    fn label(&mut self, _item: &'static str) {}
}

/// The operating system's random source failed (`GetOSRand`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OsRandError;

impl fmt::Display for OsRandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the operating system's random source failed")
    }
}

impl std::error::Error for OsRandError {}

/// The outside inputs of Core's RNG (`random.cpp`, `randomenv.cpp`).
pub trait Entropy: Send + Sync {
    /// `GetOSRand`: 32 bytes from the operating system. An error ends the process
    /// (`RandFailure`).
    // Core: random.cpp:348-400
    fn os_rand(&self, out: &mut [u8; 32]) -> Result<(), OsRandError>;

    /// `SeedHardwareFast`'s 64 bits (RDRAND, RNDR), or `None` without such hardware.
    // Core: random.cpp:247-261
    fn hw_fast(&self) -> Option<u64>;

    /// `SeedHardwareSlow`'s 256 bits (RDSEED, RDRAND folded 1024 times, RNDRRS) into `out`;
    /// `false` without such hardware.
    // Core: random.cpp:264-294
    fn hw_slow(&self, out: &mut [u8; 32]) -> bool;

    /// `GetPerformanceCounter`.
    // Core: random.cpp:52-70
    fn perf_counter(&self) -> i64;

    /// `SteadyClock::now()`, the clock `Strengthen` stops on.
    fn steady_now(&self) -> Duration;

    /// `RandAddDynamicEnv`.
    // Core: randomenv.cpp:230-309
    fn dynamic_env(&self, sink: &mut dyn EnvSink);

    /// `RandAddStaticEnv`, reduced (see [`SystemEntropy`]).
    // Core: randomenv.cpp:311-513
    fn static_env(&self, sink: &mut dyn EnvSink);

    /// The lines `ReportHardwareRand` logs, in its order.
    // Core: random.cpp:97-107, :191-198
    fn hardware_report(&self) -> &'static [&'static str];
}

/// The real sources:
/// - the OS: `getrandom`;
/// - hardware: RDRAND and RDSEED on x86-64, RNDR and RNDRRS on aarch64 Linux with
///   `HWCAP2_RNG`;
/// - the performance counter: the time stamp counter on x86-64, else the nanoseconds of the
///   monotonic clock;
/// - the dynamic environment: Core's Linux one in full (clocks, `rusage`, nine `/proc` files
///   read as Core reads them, a stack and a heap address); the clocks, and `rusage` on unix,
///   elsewhere;
/// - the static environment, reduced: `uname`, the host name, the process, parent, user and
///   group ids, the thread id, `AT_HWCAP`, `AT_HWCAP2` and `AT_RANDOM`, and `/proc/self/stat`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemEntropy;

/// The instant the monotonic clocks below count from.
pub(crate) fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

impl Entropy for SystemEntropy {
    fn os_rand(&self, out: &mut [u8; 32]) -> Result<(), OsRandError> {
        getrandom::fill(out).map_err(|_| OsRandError)
    }

    fn hw_fast(&self) -> Option<u64> {
        hw::fast()
    }

    fn hw_slow(&self, out: &mut [u8; 32]) -> bool {
        hw::slow(out)
    }

    fn perf_counter(&self) -> i64 {
        hw::time_stamp_counter().unwrap_or_else(|| epoch().elapsed().as_nanos() as i64)
    }

    fn steady_now(&self) -> Duration {
        epoch().elapsed()
    }

    fn dynamic_env(&self, sink: &mut dyn EnvSink) {
        env::dynamic_env(sink);
    }

    fn static_env(&self, sink: &mut dyn EnvSink) {
        env::static_env(sink);
    }

    fn hardware_report(&self) -> &'static [&'static str] {
        hw::report()
    }
}
