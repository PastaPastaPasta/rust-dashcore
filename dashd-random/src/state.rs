//! Core's `RNGState` (random.cpp:404-491) and `ProcRand` with its seeders (:507-632), over a
//! chunk of the locked pool.

use std::ops::Range;
use std::sync::OnceLock;
use std::time::Duration;

use dashd_lockedpool::Secure;
use parking_lot::Mutex;
use sha2::{Digest as _, Sha512};
use zeroize::Zeroize as _;

use crate::{Entropy, EnvSink, RNG_STATE_SIZE, SystemEntropy, events, rand_failure};

/// The state's bytes, as they sit in the chunk:
/// - `0..32`: the state;
/// - `32..40`: the counter, native-endian;
/// - `40`: whether a strong mix has run (`m_strongly_seeded`);
/// - `48..152`: the events hasher (`events`).
///
/// The rest is zero padding; Core's mutexes take that room.
pub(crate) type Chunk = [u8; RNG_STATE_SIZE];

const STATE: Range<usize> = 0..32;
const COUNTER: Range<usize> = 32..40;
const SEEDED: usize = 40;
const _: () = assert!(events::END <= RNG_STATE_SIZE, "the chunk holds the state's layout");

/// `CSHA512` with its `Size()`: the bytes written so far.
#[derive(Clone)]
pub(crate) struct Hasher {
    sha: Sha512,
    size: u64,
}

impl Hasher {
    pub(crate) fn new() -> Self {
        Self {
            sha: Sha512::new(),
            size: 0,
        }
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    /// `Finalize` into `out`, then `Reset`.
    pub(crate) fn finalize_reset_into(&mut self, out: &mut [u8; 64]) {
        self.sha.finalize_into_reset(out.into());
        self.size = 0;
    }
}

impl EnvSink for Hasher {
    fn write(&mut self, bytes: &[u8]) {
        self.sha.update(bytes);
        self.size += bytes.len() as u64;
    }
}

/// A new state: zeros, with the events hasher at SHA-256's initial value (the `RNGState()`
/// member initializers).
pub(crate) fn init_chunk(c: &mut Chunk) {
    c.fill(0);
    events::reset(c);
}

/// `MixExtract`'s locked part: `strongly_seeded |= strong`, then
/// `H = SHA512(hasher ‖ state ‖ counter)` into `buf`, `++counter`, `state = H[32..64]`, and
/// `hasher` reset. Returns the new `strongly_seeded`; `buf`'s first bytes are the output.
// Core: random.cpp:462-480, 487
pub(crate) fn mix(c: &mut Chunk, hasher: &mut Hasher, strong: bool, buf: &mut [u8; 64]) -> bool {
    let seeded = c[SEEDED] != 0 || strong;
    c[SEEDED] = u8::from(seeded);
    hasher.write(&c[STATE]);
    let counter = u64::from_ne_bytes(c[COUNTER].try_into().expect("8 bytes"));
    hasher.write(&counter.to_ne_bytes());
    c[COUNTER].copy_from_slice(&counter.wrapping_add(1).to_ne_bytes());
    hasher.finalize_reset_into(buf);
    c[STATE].copy_from_slice(&buf[32..]);
    seeded
}

/// `MixExtract`'s copy, outside the lock: `out` (at most 32 bytes) from the front of `H`, then
/// `H` is wiped.
// Core: random.cpp:482-489
pub(crate) fn extract(out: &mut [u8], buf: &mut [u8; 64]) {
    out.copy_from_slice(&buf[..out.len()]);
    buf.zeroize();
}

/// `AddEvent`: the event, then the low 32 bits of the performance counter, into the events
/// hasher.
// Core: random.cpp:429-438
pub(crate) fn add_event(c: &mut Chunk, info: u32, perf: i64) {
    events::write(c, &info.to_ne_bytes());
    events::write(c, &(perf as u32).to_ne_bytes());
}

/// `SeedEvents`: the events hasher's hash into `hasher`, then the events hasher restarts from
/// that hash.
// Core: random.cpp:443-456
pub(crate) fn seed_events(c: &mut Chunk, hasher: &mut Hasher) {
    let mut hash = [0u8; 32];
    events::finalize(c, &mut hash);
    hasher.write(&hash);
    events::reset(c);
    events::write(c, &hash);
    hash.zeroize();
}

/// `Strengthen`: SHA-512 iterated on `seed` in blocks of 1000 until `dur` has passed (at least
/// one block), a performance counter sample into `hasher` after each block, then the inner
/// hash into `hasher`.
// Core: random.cpp:297-322
pub(crate) fn strengthen(
    seed: &[u8; 32],
    dur: Duration,
    hasher: &mut dyn EnvSink,
    entropy: &impl Entropy,
) {
    let mut inner = Sha512::new();
    inner.update(seed);
    let mut buffer: [u8; 64];
    let stop = entropy.steady_now() + dur;
    loop {
        for _ in 0..1000 {
            buffer = inner.finalize_reset().into();
            inner.update(buffer);
        }
        hasher.write(&entropy.perf_counter().to_ne_bytes());
        if entropy.steady_now() >= stop {
            break;
        }
    }
    buffer = inner.finalize_reset().into();
    hasher.write(&buffer);
    buffer.zeroize();
}

/// `RNGLevel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Fast,
    Slow,
    Periodic,
}

/// Core's `RNGState` with its seeders, as an instance: [`PROC_RAND`](crate::PROC_RAND) is the
/// process's, and tests make their own over a fake [`Entropy`].
///
/// The state lives in one chunk of the locked pool, made at the first use and never freed, as
/// `GetRNGState`'s `secure_allocator` vector. One mutex guards it; it is never held over the
/// OS read, the environment, `Strengthen` or logging.
pub struct RngState<E: Entropy = SystemEntropy> {
    chunk: OnceLock<Mutex<Secure<RNG_STATE_SIZE>>>,
    entropy: E,
}

/// `Random_SanityCheck`'s two performance counter samples, mixed into the state later
/// ([`RngState::mix_sanity_timings`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SanityTimings {
    start: u64,
    stop: u64,
}

impl<E: Entropy> RngState<E> {
    /// A state over `entropy`; its chunk is made at the first use.
    pub const fn new(entropy: E) -> Self {
        Self {
            chunk: OnceLock::new(),
            entropy,
        }
    }

    /// The entropy sources.
    pub fn entropy(&self) -> &E {
        &self.entropy
    }

    /// Whether the state's chunk exists.
    pub(crate) fn created(&self) -> bool {
        self.chunk.get().is_some()
    }

    /// `GetRNGState`: the chunk, made at the first call.
    fn chunk(&self) -> &Mutex<Secure<RNG_STATE_SIZE>> {
        self.chunk.get_or_init(|| {
            let mut s = Secure::zeroed();
            init_chunk(&mut s);
            Mutex::new(s)
        })
    }

    /// `MixExtract`: `out` (at most 32 bytes) from the mix, outside the lock.
    // Core: random.cpp:462-490
    fn mix_extract(&self, out: &mut [u8], hasher: &mut Hasher, strong: bool) -> bool {
        let mut buf = [0u8; 64];
        let ret = mix(&mut self.chunk().lock(), hasher, strong, &mut buf);
        extract(out, &mut buf);
        ret
    }

    fn seed_events(&self, hasher: &mut Hasher) {
        seed_events(&mut self.chunk().lock(), hasher);
    }

    // Core: random.cpp:507-511
    fn seed_timestamp(&self, hasher: &mut Hasher) {
        hasher.write(&self.entropy.perf_counter().to_ne_bytes());
    }

    /// The stack pointer, 64 bits of hardware randomness, the performance counter.
    // Core: random.cpp:513-526
    fn seed_fast(&self, hasher: &mut Hasher) {
        let buffer = [0u8; 32];
        let ptr = std::hint::black_box(&buffer) as *const [u8; 32] as usize;
        hasher.write(&ptr.to_ne_bytes());
        if let Some(hw) = self.entropy.hw_fast() {
            hasher.write(&hw.to_ne_bytes());
        }
        self.seed_timestamp(hasher);
    }

    /// FAST, 32 bytes from the OS, the events, the performance counter.
    // Core: random.cpp:528-547
    fn seed_slow(&self, hasher: &mut Hasher) {
        let mut buffer = [0u8; 32];
        self.seed_fast(hasher);
        if self.entropy.os_rand(&mut buffer).is_err() {
            rand_failure();
        }
        hasher.write(&buffer);
        buffer.zeroize();
        self.seed_events(hasher);
        self.seed_timestamp(hasher);
    }

    /// 32 bytes mixed from the state and a copy of `hasher`, strengthened for `dur` into
    /// `hasher`.
    // Core: random.cpp:550-557
    fn seed_strengthen(&self, hasher: &mut Hasher, dur: Duration) {
        let mut seed = [0u8; 32];
        self.mix_extract(&mut seed, &mut hasher.clone(), false);
        strengthen(&seed, dur, hasher, &self.entropy);
        seed.zeroize();
    }

    /// FAST, the performance counter, the events, the dynamic environment, 10 ms of
    /// strengthening.
    // Core: random.cpp:559-577
    fn seed_periodic(&self, hasher: &mut Hasher) {
        self.seed_fast(hasher);
        self.seed_timestamp(hasher);
        self.seed_events(hasher);
        let old_size = hasher.size();
        self.entropy.dynamic_env(hasher);
        tracing::debug!(
            target: "rand",
            "Feeding {} bytes of dynamic environment data into RNG",
            hasher.size() - old_size
        );
        self.seed_strengthen(hasher, Duration::from_millis(10));
    }

    /// 256 bits of hardware randomness, SLOW, both environments, 100 ms of strengthening.
    /// `quiet` skips the line on the environment's size.
    // Core: random.cpp:579-597
    fn seed_startup(&self, hasher: &mut Hasher, quiet: bool) {
        let mut hw = [0u8; 32];
        if self.entropy.hw_slow(&mut hw) {
            hasher.write(&hw);
        }
        hw.zeroize();
        self.seed_slow(hasher);
        let old_size = hasher.size();
        self.entropy.dynamic_env(hasher);
        self.entropy.static_env(hasher);
        if !quiet {
            tracing::debug!(
                target: "rand",
                "Feeding {} bytes of environment data into RNG",
                hasher.size() - old_size
            );
        }
        self.seed_strengthen(hasher, Duration::from_millis(100));
    }

    /// `ProcRand`: the level's seeder, then the mix; the first mix of the state also runs
    /// STARTUP and a strong mix. `quiet` is for STARTUP (see [`Self::seed_startup`]).
    // Core: random.cpp:605-632
    pub(crate) fn proc_rand(&self, out: &mut [u8], level: Level, quiet: bool) {
        debug_assert!(out.len() <= 32);
        let _ = self.chunk();
        let mut hasher = Hasher::new();
        match level {
            Level::Fast => self.seed_fast(&mut hasher),
            Level::Slow => self.seed_slow(&mut hasher),
            Level::Periodic => self.seed_periodic(&mut hasher),
        }
        if !self.mix_extract(out, &mut hasher, false) {
            let mut startup = Hasher::new();
            self.seed_startup(&mut startup, quiet);
            self.mix_extract(out, &mut startup, true);
        }
    }

    /// `GetRandBytes`: a FAST draw.
    pub fn rand_bytes<const N: usize>(&self, out: &mut [u8; N]) {
        const { assert!(N <= 32, "a draw takes at most 32 bytes (random.cpp:610)") };
        self.proc_rand(out, Level::Fast, false);
    }

    /// `GetStrongRandBytes`: a SLOW draw, with 32 fresh bytes from the OS.
    pub fn strong_rand_bytes<const N: usize>(&self, out: &mut [u8; N]) {
        const { assert!(N <= 32, "a draw takes at most 32 bytes (random.cpp:610)") };
        self.proc_rand(out, Level::Slow, false);
    }

    /// `RandAddPeriodic`.
    pub fn add_periodic(&self) {
        self.proc_rand(&mut [], Level::Periodic, false);
    }

    /// `RandAddEvent`.
    pub fn add_event(&self, info: u32) {
        let mut c = self.chunk().lock();
        add_event(&mut c, info, self.entropy.perf_counter());
    }

    /// `Random_SanityCheck`'s checks: OS reads until every byte of the 32 was nonzero at least
    /// once (at most 1024 reads), then the performance counter must move across a 1 ms sleep.
    /// `None` when either fails. Touches no state; [`Self::mix_sanity_timings`] mixes the two
    /// samples in.
    // Core: random.cpp:697-732
    pub fn sanity_check(&self) -> Option<SanityTimings> {
        const MAX_TRIES: u32 = 1024;
        let start = self.entropy.perf_counter() as u64;
        let mut overwritten = [false; 32];
        let mut tries = 0;
        let mut num_overwritten;
        loop {
            let mut data = [0u8; 32];
            if self.entropy.os_rand(&mut data).is_err() {
                rand_failure();
            }
            for (o, d) in overwritten.iter_mut().zip(data) {
                *o |= d != 0;
            }
            num_overwritten = overwritten.iter().filter(|&&o| o).count();
            tries += 1;
            if !(num_overwritten < 32 && tries < MAX_TRIES) {
                break;
            }
        }
        if num_overwritten != 32 {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
        let stop = self.entropy.perf_counter() as u64;
        (stop != start).then_some(SanityTimings {
            start,
            stop,
        })
    }

    /// `Random_SanityCheck`'s mix of its two samples into the state.
    // Core: random.cpp:734-738
    pub fn mix_sanity_timings(&self, t: SanityTimings) {
        let mut to_add = Hasher::new();
        to_add.write(&t.start.to_ne_bytes());
        to_add.write(&t.stop.to_ne_bytes());
        self.mix_extract(&mut [], &mut to_add, false);
    }

    /// `ReportHardwareRand`'s lines, unconditional (`LogPrintf`).
    pub(crate) fn report_hardware_rand(&self) {
        for line in self.entropy.hardware_report() {
            tracing::info!("{line}");
        }
    }
}

impl<E: Entropy> dash_p2p::rng::RandSource for RngState<E> {
    /// `GetStrongRandBytes` when `strong`, else `GetRandBytes`. A draw over 32 bytes ends the
    /// process, as Core's `assert(num <= 32)` does.
    fn rand_bytes(&self, out: &mut [u8], strong: bool) {
        if out.len() > 32 {
            crate::draw_too_large();
        }
        self.proc_rand(
            out,
            if strong {
                Level::Slow
            } else {
                Level::Fast
            },
            false,
        );
    }

    fn add_event(&self, info: u32) {
        RngState::add_event(self, info);
    }
}
