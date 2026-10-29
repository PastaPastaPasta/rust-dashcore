//! Core's randomness (`random.h`): [`RandSource`] is `GetRandBytes`, where every random byte comes
//! from, and [`FastRandom`] is `FastRandomContext`, a ChaCha20 keystream (RFC 8439, zero nonce,
//! counter from 0) read as little-endian words, with a 64-bit bit buffer for small draws. Each
//! draw site takes Core's kind of draw ([`get_rand`], [`get_rand_hash`], a fresh or long-lived
//! [`FastRandom::lazy`]), so the bytes it takes from the source and the values it gets from them
//! are Core's (P2a §3.7.2, D38).

use std::fmt;
use std::sync::Arc;

use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use parking_lot::Mutex;

/// Core's `GetRandBytes` and `GetStrongRandBytes`: the bytes every draw starts from.
pub trait RandSource: Send + Sync {
    /// Fills `out`; `strong` for `GetStrongRandBytes` (key material).
    fn rand_bytes(&self, out: &mut [u8], strong: bool);
}

/// The operating system's random source, for both of Core's levels: the production source.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsRand;

impl RandSource for OsRand {
    /// # Panics
    ///
    /// When the operating system's random source fails, as Core aborts in `GetOSRand`.
    fn rand_bytes(&self, out: &mut [u8], _strong: bool) {
        getrandom::fill(out).expect("the OS random source failed");
    }
}

/// A ChaCha20 stream serving `GetRandBytes` in order: `cfg.deterministic`'s source (the zero key),
/// and the oracle's (O1c's `DrawLog`, keyed by its session seed). Not for production secrets.
pub struct StreamRand(Mutex<FastRandom>);

impl StreamRand {
    /// The stream keyed with `key`.
    pub fn with_key(key: [u8; 32]) -> Self {
        Self(Mutex::new(FastRandom::with_seed(key)))
    }

    /// O1c's stream for a session's `rng_seed`: the seed's little-endian bytes, then zeros.
    // O1c: src/test/oracle/oracle_rng.cpp SeedKey, DrawLog::RandBytes (`fillrand`)
    pub fn from_seed(seed: u64) -> Self {
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(&seed.to_le_bytes());
        Self::with_key(key)
    }
}

impl RandSource for StreamRand {
    fn rand_bytes(&self, out: &mut [u8], _strong: bool) {
        self.0.lock().fill(out);
    }
}

/// Core's `GetRandHash()`: 32 bytes of `GetRandBytes`.
pub fn get_rand_hash(src: &dyn RandSource) -> [u8; 32] {
    let mut out = [0u8; 32];
    src.rand_bytes(&mut out, false);
    out
}

/// Core's `GetRand(max)`: the first `randrange(max)` of a fresh context, which takes 32 bytes
/// from `src` unless `max` is 1.
// Core: random.cpp:675-678 GetRandInternal
pub fn get_rand(src: &Arc<dyn RandSource>, max: u64) -> u64 {
    FastRandom::lazy(src.clone()).randrange(max)
}

/// Core's `FastRandomContext`.
pub struct FastRandom {
    rng: ChaCha20,
    bitbuf: u64,
    bitbuf_size: u32,
    /// Core's `requires_seed`: the source the key comes from at the first keystream use.
    seed_from: Option<Arc<dyn RandSource>>,
}

impl fmt::Debug for FastRandom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FastRandom")
            .field("keystream_bytes", &self.keystream_bytes())
            .field("bitbuf_size", &self.bitbuf_size)
            .field("seeded", &self.seed_from.is_none())
            .finish_non_exhaustive()
    }
}

impl FastRandom {
    /// A context keyed with `seed` (Core's `FastRandomContext(const uint256&)`).
    // Core: random.cpp:695 FastRandomContext::FastRandomContext(const uint256&)
    pub fn with_seed(seed: [u8; 32]) -> Self {
        Self {
            rng: ChaCha20::new(&seed.into(), &[0u8; 12].into()),
            bitbuf: 0,
            bitbuf_size: 0,
            seed_from: None,
        }
    }

    /// Core's `FastRandomContext()`: keyed with [`get_rand_hash`] of `src` at its first keystream
    /// use, so a context that never draws takes nothing from `src`.
    // Core: random.cpp:695-699 RandomSeed; :789-797 FastRandomContext(bool)
    pub fn lazy(src: Arc<dyn RandSource>) -> Self {
        Self {
            seed_from: Some(src),
            ..Self::deterministic()
        }
    }

    /// Core's deterministic context (`FastRandomContext(true)`: the zero key).
    pub fn deterministic() -> Self {
        Self::with_seed([0; 32])
    }

    /// [`FastRandom::lazy`] on the operating system's random source.
    pub fn new() -> Self {
        Self::lazy(Arc::new(OsRand))
    }

    /// Fills `out` with keystream bytes (Core's `fillrand`, which seeds a lazy context even for
    /// an empty `out`).
    pub fn fill(&mut self, out: &mut [u8]) {
        if let Some(src) = self.seed_from.take() {
            self.rng = ChaCha20::new(&get_rand_hash(&*src).into(), &[0u8; 12].into());
        }
        out.fill(0);
        self.rng.apply_keystream(out);
    }

    /// The keystream bytes produced so far (the count Core's draw log keeps per context).
    pub fn keystream_bytes(&self) -> u64 {
        self.rng.current_pos()
    }

    /// The bit buffer: the bits left in it, and the buffer.
    pub fn bitbuf(&self) -> (u32, u64) {
        (self.bitbuf_size, self.bitbuf)
    }

    /// Core's `rand64()`: eight keystream bytes, little-endian. Does not touch the bit buffer.
    pub fn rand64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        self.fill(&mut buf);
        u64::from_le_bytes(buf)
    }

    /// Core's `randbits(bits)`, 0 ≤ bits ≤ 64. Above 32 bits it takes the top bits of a fresh
    /// `rand64`; otherwise it reads the bit buffer, refilling it (and discarding what was left)
    /// when it holds too few.
    pub fn randbits(&mut self, bits: u32) -> u64 {
        debug_assert!(bits <= 64);
        if bits == 0 {
            0
        } else if bits > 32 {
            self.rand64() >> (64 - bits)
        } else {
            if self.bitbuf_size < bits {
                self.bitbuf = self.rand64();
                self.bitbuf_size = 64;
            }
            let ret = self.bitbuf & (u64::MAX >> (64 - bits));
            self.bitbuf >>= bits;
            self.bitbuf_size -= bits;
            ret
        }
    }

    /// Core's `randrange(range)`: uniform in [0, range) by rejection over `bit_width(range − 1)`
    /// bits, so `range == 1` draws nothing.
    ///
    /// # Panics
    ///
    /// When `range` is 0 (Core asserts).
    pub fn randrange(&mut self, range: u64) -> u64 {
        assert!(range > 0, "randrange(0)");
        let max = range - 1;
        let bits = u64::BITS - max.leading_zeros();
        loop {
            let ret = self.randbits(bits);
            if ret <= max {
                return ret;
            }
        }
    }

    /// Core's `rand32()`.
    pub fn rand32(&mut self) -> u32 {
        self.randbits(32) as u32
    }

    /// Core's `randbool()`.
    pub fn randbool(&mut self) -> bool {
        self.randbits(1) == 1
    }

    /// Core's `rand256()`.
    pub fn rand256(&mut self) -> [u8; 32] {
        let mut out = [0u8; 32];
        self.fill(&mut out);
        out
    }

    /// Core's `rand_uniform_duration(range)` on a count: [0, range) for a positive range,
    /// (range, 0] for a negative one, 0 for 0.
    pub fn rand_uniform(&mut self, range: i64) -> i64 {
        match range.signum() {
            1 => self.randrange(range as u64) as i64,
            -1 => -(self.randrange(range.unsigned_abs()) as i64),
            _ => 0,
        }
    }

    /// Core's `Shuffle(first, last, rng)` (`random.h`), not `std::shuffle`: for each position, a
    /// swap with a uniformly chosen position at or after it.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        let len = items.len();
        for first in 0..len {
            let j = self.randrange((len - first) as u64) as usize;
            if j != 0 {
                items.swap(first, first + j);
            }
        }
    }
}

impl Default for FastRandom {
    fn default() -> Self {
        Self::new()
    }
}

/// 32 bytes from the operating system. Draw sites that are Core's take [`NetCore::rand`]'s source
/// instead, so a harness can serve them (D38).
///
/// [`NetCore::rand`]: crate::NetCore::rand
///
/// # Panics
///
/// When the operating system's random source fails, as Core aborts in `GetOSRand`.
pub fn os_rand256() -> [u8; 32] {
    let mut out = [0u8; 32];
    getrandom::fill(&mut out).expect("the OS random source failed");
    out
}

/// Core's `GetExponentialRand(now, average_interval)` in microseconds: `now` plus an
/// exponentially distributed delay drawn from a 48-bit uniform `u`, with Core's literal (not
/// exactly −2⁻⁴⁸). As in Core, `unscaled * average_interval` is a `double` in seconds; adding
/// `0.5us` converts it to `long double` microseconds, so the ×10⁶ and the +0.5 each round to a
/// 64-bit mantissa (x86-64's 80-bit format), which [`round_to_u64_mantissa`] reproduces.
// Core: random.cpp:771-775 GetExponentialRand
pub fn exponential_rand(now_us: i64, average_secs: i64, u: u64) -> i64 {
    let unscaled = -((u & ((1 << 48) - 1)) as f64 * -3.552_713_678_8e-15).ln_1p();
    now_us + long_double_micros_plus_half(unscaled * average_secs as f64)
}

/// `duration_cast<microseconds>(duration<double>(secs) + 0.5us)` with `long double` arithmetic,
/// for `secs` ≥ 0. The product with 10⁶ is exact in a `u128` before its single rounding.
fn long_double_micros_plus_half(secs: f64) -> i64 {
    if !secs.is_finite() || secs <= 0.0 {
        return (secs * 1e6 + 0.5) as i64;
    }
    // secs = m·2^e exactly, m < 2^53.
    let bits = secs.to_bits();
    let (m, e) = match ((bits >> 52) & 0x7ff) as i32 {
        0 => (bits & ((1 << 52) - 1), -1074),
        x => ((bits & ((1 << 52) - 1)) | (1 << 52), x - 1075),
    };
    let (n, e) = round_to_u64_mantissa(u128::from(m) * 1_000_000, e);
    // Below 0.25 µs the sum stays below 1, whatever the rounding.
    if e + 64 <= -2 {
        return 0;
    }
    // The sum with 0.5 = 2^-1, exact at the smaller exponent, then rounded again.
    let (n, e) = if e < -1 {
        round_to_u64_mantissa(n + (1u128 << (-1 - e)), e)
    } else if e < 62 {
        round_to_u64_mantissa((n << (e + 1)) + 1, -1)
    } else {
        return i64::MAX;
    };
    if e >= 0 {
        i64::try_from(n << e.min(63)).unwrap_or(i64::MAX)
    } else {
        (n >> (-e).min(127)) as i64
    }
}

/// `n·2^e` rounded to a 64-bit mantissa, ties to even (x87 `long double` rounding).
fn round_to_u64_mantissa(n: u128, e: i32) -> (u128, i32) {
    let len = 128 - n.leading_zeros() as i32;
    if len <= 64 {
        return (n, e);
    }
    let shift = len - 64;
    let (mut q, rem, half) = (n >> shift, n & ((1u128 << shift) - 1), 1u128 << (shift - 1));
    if rem > half || (rem == half && q & 1 == 1) {
        q += 1;
    }
    (q, e + shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that counts the bytes taken from it.
    struct Counted(StreamRand, Mutex<usize>);

    impl Counted {
        fn new() -> Arc<Self> {
            Arc::new(Self(StreamRand::from_seed(7), Mutex::new(0)))
        }

        fn taken(&self) -> usize {
            *self.1.lock()
        }
    }

    impl RandSource for Counted {
        fn rand_bytes(&self, out: &mut [u8], strong: bool) {
            *self.1.lock() += out.len();
            self.0.rand_bytes(out, strong);
        }
    }

    /// Core's lazy `FastRandomContext()`: nothing until the first keystream use, then 32 bytes
    /// once, keyed with them; `fillrand` seeds even for no bytes, `randbits(0)` does not.
    #[test]
    fn a_lazy_context_seeds_at_its_first_draw() {
        let src = Counted::new();
        let mut ctx = FastRandom::lazy(src.clone());
        assert_eq!((ctx.randbits(0), ctx.randrange(1)), (0, 0));
        assert_eq!(src.taken(), 0);
        let got = ctx.rand64();
        assert_eq!(src.taken(), 32);
        ctx.rand256();
        assert_eq!(src.taken(), 32);
        let key = get_rand_hash(&StreamRand::from_seed(7));
        assert_eq!(got, FastRandom::with_seed(key).rand64());

        FastRandom::lazy(src.clone()).fill(&mut []);
        assert_eq!(src.taken(), 64);
    }

    /// Core's `GetRand(max)`: a fresh context each call (32 bytes), none for `max == 1`.
    #[test]
    fn get_rand_is_a_fresh_context_per_call() {
        let counted = Counted::new();
        let src: Arc<dyn RandSource> = counted.clone();
        assert_eq!(get_rand(&src, 1), 0);
        assert_eq!(counted.taken(), 0);
        let (a, b) = (get_rand(&src, u64::MAX), get_rand(&src, u64::MAX));
        assert_eq!(counted.taken(), 64);
        let stream = StreamRand::from_seed(7);
        for want in [a, b] {
            let key = get_rand_hash(&stream);
            assert_eq!(want, FastRandom::with_seed(key).randrange(u64::MAX));
        }
    }

    /// O1c's `DrawLog` stream: `FastRandomContext(uint256(LE(seed)))`'s keystream, in order.
    #[test]
    fn stream_rand_is_o1cs_stream() {
        let src = StreamRand::from_seed(0x0102_0304_0506_0708);
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(&[8, 7, 6, 5, 4, 3, 2, 1]);
        let mut want = [0u8; 40];
        FastRandom::with_seed(key).fill(&mut want);
        let mut got = [0u8; 40];
        src.rand_bytes(&mut got[..8], false);
        src.rand_bytes(&mut got[8..], true);
        assert_eq!(got, want);
    }

    /// Core's `random_tests.cpp` (`osrandom_tests`/`fastrandom_tests_deterministic`), v24.
    #[test]
    fn core_deterministic_vectors() {
        assert_eq!(FastRandom::deterministic().rand64(), 10_393_729_187_455_219_830);
        assert_eq!(FastRandom::deterministic().rand64(), 0x903d_f1a0_ade0_b876);

        let mut ctx = FastRandom::deterministic();
        assert_eq!(1 + ctx.rand_uniform(9), 7);
        assert_eq!(1 + ctx.rand_uniform(-9), -6);
        assert_eq!(1 + ctx.rand_uniform(0), 1);
        assert_eq!(1 + ctx.rand_uniform(i64::MAX), 1_467_825_113_502_396_065);
        assert_eq!(1 + ctx.rand_uniform(-i64::MAX), -970_181_367_944_767_837);
        assert_eq!(1 + ctx.rand_uniform(9 * 3600), 24_761);

        let mut ctx = FastRandom::deterministic();
        assert_eq!(ctx.rand_uniform(1_000_000), 47_222);
        assert_eq!(ctx.rand_uniform(9 * 3600), 2_782);
    }

    #[test]
    fn randrange_one_draws_nothing() {
        let mut a = FastRandom::deterministic();
        let mut b = FastRandom::deterministic();
        assert_eq!(a.randrange(1), 0);
        assert_eq!(a.randbits(7), b.randbits(7));
    }

    #[test]
    fn randbits_refill_discards_leftover() {
        let mut ctx = FastRandom::deterministic();
        let first = FastRandom::deterministic().rand64();
        let second = {
            let mut c = FastRandom::deterministic();
            c.rand64();
            c.rand64()
        };
        assert_eq!(ctx.randbits(30), first & ((1 << 30) - 1));
        assert_eq!(ctx.randbits(30), (first >> 30) & ((1 << 30) - 1));
        // 4 bits remain; a 30-bit draw refills from the next word and drops them.
        assert_eq!(ctx.randbits(30), second & ((1 << 30) - 1));
    }

    /// Core's expression (random.cpp:773-774), compiled with g++ 15 -O2 on x86-64 (80-bit
    /// `long double`), at `now` = 1.7e15 µs. (400,000 random draws at 120 s, 300 s, 1 d and
    /// 7 d also agreed when this was written.)
    #[test]
    fn exponential_rand_matches_core() {
        const NOW: i64 = 1_700_000_000_000_000;
        for (avg, u, want) in [
            (120, 0, 1_700_000_000_000_000),
            (120, 1, 1_700_000_000_000_000),
            (120, 0x1234_5678_9abc, 1_700_000_008_851_938),
            (120, 0x8000_0000_0000, 1_700_000_083_177_662),
            (120, 0xffff_ffff_fffe, 1_700_003_544_903_601),
            (120, 0xffff_ffff_ffff, 1_700_003_547_817_250),
            (120, 0x5a5a_5a5a_5a5a, 1_700_000_052_238_169),
            (300, 0x0123_4567_89ab, 1_700_000_001_336_305),
            (300, 0xfedc_ba98_7654, 1_700_001_624_830_121),
            // The f64 product rounds to …500.5 here; Core's long double keeps …500.49999.
            (86_400, 0xfd0b_7db6_ae7f, 1_700_385_485_743_500),
            (86_400, 0x1234_5678_9abc, 1_700_006_373_395_381),
            (86_400, 0xffff_ffff_ffff, 1_702_554_428_419_733),
            (86_400, 0xffff, 1_700_000_000_000_020),
        ] {
            assert_eq!(exponential_rand(NOW, avg, u), want, "avg={avg} u={u:#x}");
        }
    }
}
