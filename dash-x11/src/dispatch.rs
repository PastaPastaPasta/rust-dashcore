//! Backend selection (Core: `SapphireAutoDetect` in `crypto/x11/dispatch.cpp`).

use core::sync::atomic::{AtomicU32, Ordering};

use crate::{portable, self_test};

/// The first function of the chain, over any input. Unsafe only because a backend may need
/// CPU features that [`Implementation::detect`] checked.
type BlakeFn = unsafe fn(&[u8]) -> [u8; 64];
/// One of the other ten functions, over the previous one's 64-byte digest.
type ChainFn = unsafe fn(&[u8; 64]) -> [u8; 64];

/// Which optimized backends detection may use. The portable code is always allowed.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Use(u8);

impl Use {
    /// Only the portable code.
    pub const STANDARD: Use = Use(0);
    /// The 128-bit vector code (x86_64 with SSSE3 and SSE4.1). [`AES`](Self::AES) and
    /// [`AVX2`](Self::AVX2) build on it, so allowing either allows this too.
    pub const SSE: Use = Use(1 << 0);
    /// The AES-NI code (x86_64 with AES-NI): ECHO and SHAvite-3, as in Core, and Grøstl.
    pub const AES: Use = Use(1 << 1);
    /// The 256-bit vector code (x86_64 with AVX2 enabled by the OS).
    pub const AVX2: Use = Use(1 << 2);
    /// Everything.
    pub const ALL: Use = Use(Self::SSE.0 | Self::AES.0 | Self::AVX2.0);

    /// The backends allowed by either.
    pub const fn union(self, other: Use) -> Use {
        Use(self.0 | other.0)
    }

    /// Whether every backend in `other` is allowed.
    pub const fn contains(self, other: Use) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::fmt::Debug for Use {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let names = [(Use::SSE, "SSE"), (Use::AES, "AES"), (Use::AVX2, "AVX2")];
        let mut set = names.iter().filter(|(u, _)| self.contains(*u)).map(|(_, n)| *n);
        match set.next() {
            None => f.write_str("Use(STANDARD)"),
            Some(first) => {
                write!(f, "Use({first}")?;
                for n in set {
                    write!(f, " | {n}")?;
                }
                f.write_str(")")
            }
        }
    }
}

impl core::ops::BitOr for Use {
    type Output = Use;
    fn bitor(self, rhs: Use) -> Use {
        self.union(rhs)
    }
}

/// One of the eleven functions X11 chains, in chain order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// BLAKE-512, over the X11 input.
    Blake,
    /// BMW-512.
    Bmw,
    /// Grøstl-512.
    Groestl,
    /// Skein-512.
    Skein,
    /// JH-512.
    Jh,
    /// Keccak-512.
    Keccak,
    /// Luffa-512.
    Luffa,
    /// CubeHash-512.
    Cubehash,
    /// SHAvite-3-512.
    Shavite,
    /// SIMD-512.
    Simd,
    /// ECHO-512, whose first 32 bytes are the X11 hash.
    Echo,
}

impl Stage {
    /// Every stage, in chain order.
    pub const ALL: [Stage; 11] = [
        Stage::Blake,
        Stage::Bmw,
        Stage::Groestl,
        Stage::Skein,
        Stage::Jh,
        Stage::Keccak,
        Stage::Luffa,
        Stage::Cubehash,
        Stage::Shavite,
        Stage::Simd,
        Stage::Echo,
    ];

    /// The lower-case name, as in Core's `sph_<name>512` functions.
    pub const fn name(self) -> &'static str {
        match self {
            Stage::Blake => "blake",
            Stage::Bmw => "bmw",
            Stage::Groestl => "groestl",
            Stage::Skein => "skein",
            Stage::Jh => "jh",
            Stage::Keccak => "keccak",
            Stage::Luffa => "luffa",
            Stage::Cubehash => "cubehash",
            Stage::Shavite => "shavite",
            Stage::Simd => "simd",
            Stage::Echo => "echo",
        }
    }

    /// The index of a stage after BLAKE in [`Implementation`]'s `chain`.
    const fn chain_index(self) -> usize {
        self as usize - 1
    }
}

/// A selection of X11 backends for this CPU: one function per stage.
///
/// Instances exist only as statics that [`Implementation::detect`] returns after it checked the
/// CPU features each function needs.
pub struct Implementation {
    name: &'static str,
    blake: BlakeFn,
    /// The other stages, at their [`Stage::chain_index`].
    chain: [ChainFn; 10],
}

impl core::fmt::Debug for Implementation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name)
    }
}

/// The implementation for a set of backends. Every stage without an optimized backend in
/// `backends` runs the portable code.
#[cfg_attr(
    not(all(target_arch = "x86_64", target_feature = "sse2")),
    allow(unused_mut, unused_variables)
)]
const fn build(name: &'static str, backends: Use) -> Implementation {
    let mut imp = Implementation {
        name,
        blake: portable::blake::hash,
        chain: [
            portable::bmw::hash,
            portable::groestl::hash,
            portable::skein::hash,
            portable::jh::hash,
            portable::keccak::hash,
            portable::luffa::hash,
            portable::cubehash::hash,
            portable::shavite::hash,
            portable::simd::hash,
            portable::echo::hash,
        ],
    };
    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    {
        use crate::x86_64 as x;
        let c = &mut imp.chain;
        if backends.contains(Use::SSE) {
            c[Stage::Jh.chain_index()] = x::jh::hash_sse;
            c[Stage::Luffa.chain_index()] = x::luffa::hash_sse;
            c[Stage::Cubehash.chain_index()] = x::cubehash::hash_sse2;
            c[Stage::Simd.chain_index()] = x::simd::hash_sse;
        }
        if backends.contains(Use::AVX2) {
            c[Stage::Jh.chain_index()] = x::jh::hash_avx2;
            c[Stage::Luffa.chain_index()] = x::luffa::hash_avx2;
            c[Stage::Cubehash.chain_index()] = x::cubehash::hash_avx2;
            c[Stage::Simd.chain_index()] = x::simd::hash_avx2;
        }
        if backends.contains(Use::AES) {
            c[Stage::Groestl.chain_index()] = x::groestl::hash_aes;
            c[Stage::Shavite.chain_index()] = x::shavite::hash;
            c[Stage::Echo.chain_index()] = x::echo::hash;
        }
    }
    imp
}

static STANDARD: Implementation = build("standard", Use::STANDARD);
#[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
static X86_SSE: Implementation = build("sse", Use::SSE);
#[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
static X86_SSE_AES: Implementation = build("sse,aes", Use::SSE.union(Use::AES));
#[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
static X86_SSE_AVX2: Implementation = build("sse,avx2", Use::SSE.union(Use::AVX2));
#[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
static X86_ALL: Implementation = build("sse,aes,avx2", Use::ALL);

/// Every implementation, at its index in a [`Selection`]: the reverse of [`CANDIDATES`]'s
/// order, so the portable one is first and [`Selection::failed`] lists in trial order.
#[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
static ALL_IMPLEMENTATIONS: [&Implementation; 5] =
    [&STANDARD, &X86_SSE, &X86_SSE_AVX2, &X86_SSE_AES, &X86_ALL];
#[cfg(not(all(target_arch = "x86_64", target_feature = "sse2")))]
static ALL_IMPLEMENTATIONS: [&Implementation; 1] = [&STANDARD];

// A Selection's `failed` has one bit per implementation.
const _: () = assert!(ALL_IMPLEMENTATIONS.len() <= 8);

/// The index of `imp` in [`ALL_IMPLEMENTATIONS`].
fn index_of(imp: &'static Implementation) -> u8 {
    ALL_IMPLEMENTATIONS
        .iter()
        .position(|i| core::ptr::eq(*i, imp))
        .expect("every implementation is listed") as u8
}

/// The selections [`init`] tries, best first: every backend, then each smaller set, then the
/// portable code. On a given CPU several may be the same implementation; it is tried once.
const CANDIDATES: [Use; 5] = [
    Use::ALL,
    Use(Use::SSE.0 | Use::AES.0),
    Use(Use::SSE.0 | Use::AVX2.0),
    Use::SSE,
    Use::STANDARD,
];

/// How [`init`] chose the implementation [`x11`](crate::x11) uses.
///
/// `preferred` is what CPU detection picks with every backend allowed; `implementation` is the
/// first candidate that passed the self-test; `failed` lists the candidates tried before it,
/// which failed the self-test. On a healthy CPU `failed` is empty and the two are the same.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    chosen: u8,
    preferred: u8,
    /// Bit `i`: [`ALL_IMPLEMENTATIONS`]`[i]` failed the self-test.
    failed: u8,
}

impl Selection {
    /// The implementation [`x11`](crate::x11) uses.
    pub fn implementation(self) -> &'static Implementation {
        ALL_IMPLEMENTATIONS[self.chosen as usize]
    }

    /// What CPU detection picked with every backend allowed, before the self-test
    /// ([`Implementation::detect`]`(Use::ALL)` without the check).
    pub fn preferred(self) -> &'static Implementation {
        ALL_IMPLEMENTATIONS[self.preferred as usize]
    }

    /// Whether a candidate failed the self-test, so a smaller selection than the CPU supports
    /// is in use. A node should warn prominently (see the README's adoption contract).
    pub fn fell_back(self) -> bool {
        self.failed != 0
    }

    /// The candidates that failed the self-test, in the order [`init`] tried them.
    pub fn failed(self) -> impl Iterator<Item = &'static Implementation> {
        let failed = self.failed;
        ALL_IMPLEMENTATIONS
            .iter()
            .enumerate()
            .rev()
            .filter(move |(i, _)| failed & (1 << i) != 0)
            .map(|(_, imp)| *imp)
    }

    /// Packs into the cache word: bit 31 set, then the three indices.
    fn pack(self) -> u32 {
        1 << 31 | (self.chosen as u32) | (self.preferred as u32) << 8 | (self.failed as u32) << 16
    }

    fn unpack(word: u32) -> Option<Selection> {
        (word & 1 << 31 != 0).then_some(Selection {
            chosen: word as u8,
            preferred: (word >> 8) as u8,
            failed: (word >> 16) as u8,
        })
    }
}

impl core::fmt::Debug for Selection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        struct Failed(Selection);
        impl core::fmt::Debug for Failed {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_list().entries(self.0.failed().map(|imp| imp.name)).finish()
            }
        }
        f.debug_struct("Selection")
            .field("implementation", &self.implementation().name)
            .field("preferred", &self.preferred().name)
            .field("failed", &Failed(*self))
            .finish()
    }
}

/// `'<implementation>'`, plus the failed candidates after a fallback: for the startup log line
/// (`Using the {selection} X11 implementation`).
impl core::fmt::Display for Selection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "'{}'", self.implementation().name)?;
        if self.fell_back() {
            f.write_str(" (fell back: ")?;
            for (n, imp) in self.failed().enumerate() {
                write!(
                    f,
                    "{}'{}'",
                    if n == 0 {
                        ""
                    } else {
                        ", "
                    },
                    imp.name
                )?;
            }
            f.write_str(" failed the self-test)")?;
        }
        Ok(())
    }
}

/// The first candidate `ok` accepts, recording the ones it rejects; panics if none is accepted
/// (the last candidate is always the portable code).
fn choose(
    candidates: impl IntoIterator<Item = &'static Implementation>,
    ok: impl Fn(&Implementation) -> bool,
) -> Selection {
    let mut failed = 0u8;
    for imp in candidates {
        let i = index_of(imp);
        if failed & (1 << i) != 0 {
            continue;
        }
        if ok(imp) {
            return Selection {
                chosen: i,
                preferred: 0,
                failed,
            };
        }
        failed |= 1 << i;
    }
    panic!("X11 self-test failed for every implementation, the portable one included");
}

/// The cached [`Selection`]; 0 until [`init`] ran.
static SELECTION: AtomicU32 = AtomicU32::new(0);

/// Detects the CPU's backends and self-tests them, once, and returns the choice; later calls
/// return the cached choice. A node calls this at startup, logs the result and warns on
/// [`Selection::fell_back`]; [`x11`](crate::x11) uses the cached choice (and runs this itself
/// if nobody did).
///
/// The best selection that passes the self-test wins: every backend, or if that fails, the
/// next smaller set, down to the portable code. A vector path that fails the check thus costs
/// speed, not availability. For a strict check, use [`Implementation::detect`].
///
/// # Panics
///
/// If even the portable code fails the self-test.
pub fn init() -> Selection {
    if let Some(sel) = selection() {
        return sel;
    }
    let mut sel = choose(CANDIDATES.into_iter().map(Implementation::select), self_test::passes);
    sel.preferred = index_of(Implementation::select(Use::ALL));
    // The first thread to finish wins, so every caller sees the selection x11() uses, even if
    // a flaky CPU made racing self-tests disagree.
    #[cfg(target_has_atomic = "32")]
    match SELECTION.compare_exchange(0, sel.pack(), Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => sel,
        Err(won) => Selection::unpack(won).expect("a stored selection"),
    }
    // Without compare-and-swap (thumbv6m) the last store wins; call init once at startup.
    #[cfg(not(target_has_atomic = "32"))]
    {
        SELECTION.store(sel.pack(), Ordering::Release);
        sel
    }
}

/// The cached [`Selection`], if [`init`] (or a first hash) has run.
pub fn selection() -> Option<Selection> {
    Selection::unpack(SELECTION.load(Ordering::Acquire))
}

impl Implementation {
    /// The best backends this CPU supports among those `allow`ed (Core: `SapphireAutoDetect`),
    /// checked by the self-test (`src/self_test`: X11 of four inputs and every chain function of
    /// two). The check runs on every call; [`init`] caches its choice.
    ///
    /// # Panics
    ///
    /// If the check fails. Core has no X11 self-test; this one exists because a CPU (or an
    /// emulator) that computes X11 wrongly must not validate with the backends that fail it.
    /// Callers that want a failed fast path to be fatal check [`Selection::fell_back`] after
    /// [`init`], or call this in addition to it ([`x11`](crate::x11) still uses `init`'s choice).
    pub fn detect(allow: Use) -> &'static Implementation {
        let imp = Self::select(allow);
        assert!(self_test::passes(imp), "X11 self-test failed for {}", imp.name);
        imp
    }

    /// [`init`]'s implementation: the one [`x11`](crate::x11) uses.
    ///
    /// # Panics
    ///
    /// As [`init`].
    pub fn auto() -> &'static Implementation {
        init().implementation()
    }

    /// The portable implementation, which every CPU runs.
    pub fn standard() -> &'static Implementation {
        &STANDARD
    }

    /// The selected backends: `standard`, or the comma-separated [`Use`] backends.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The X11 hash of `data` (Core: `HashX11`), in Core's byte order (that of `uint256`'s
    /// data, the order block hashes are serialized in).
    pub fn x11(&self, data: &[u8]) -> [u8; 32] {
        // SAFETY: `select` only returns implementations whose CPU features are present.
        let mut h = unsafe { (self.blake)(data) };
        for f in self.chain {
            // SAFETY: as above.
            h = unsafe { f(&h) };
        }
        let mut out = [0; 32];
        out.copy_from_slice(&h[..32]);
        out
    }

    /// One function of the chain over `input`, as `HashX11` calls it: `sph_<name>512_init`,
    /// one `sph_<name>512` write of `input`, `sph_<name>512_close`.
    ///
    /// # Panics
    ///
    /// If `stage` is not [`Stage::Blake`] and `input` is not 64 bytes long: inside X11 every
    /// stage after the first hashes the previous stage's 64-byte digest.
    pub fn stage(&self, stage: Stage, input: &[u8]) -> [u8; 64] {
        if stage == Stage::Blake {
            // SAFETY: as in `x11`.
            return unsafe { (self.blake)(input) };
        }
        let input: &[u8; 64] = input.try_into().expect("X11 chain stages hash 64 bytes");
        // SAFETY: as in `x11`.
        unsafe { self.chain[stage.chain_index()](input) }
    }

    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    fn select(allow: Use) -> &'static Implementation {
        // AES and AVX2 build on SSE.
        let allow = if allow.0 != 0 {
            allow | Use::SSE
        } else {
            allow
        };
        let usable = Use(allow.0 & crate::x86_64::cpu_features().0);
        if !usable.contains(Use::SSE) {
            return &STANDARD;
        }
        match (usable.contains(Use::AES), usable.contains(Use::AVX2)) {
            (false, false) => &X86_SSE,
            (true, false) => &X86_SSE_AES,
            (false, true) => &X86_SSE_AVX2,
            (true, true) => &X86_ALL,
        }
    }

    #[cfg(not(all(target_arch = "x86_64", target_feature = "sse2")))]
    fn select(_allow: Use) -> &'static Implementation {
        &STANDARD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `choose` takes the first accepted candidate, skips repeats and records the rejected.
    #[test]
    fn choose_records_failed_candidates() {
        let all = || ALL_IMPLEMENTATIONS.iter().rev().copied();
        let sel = choose(all(), |_| true);
        assert!(!sel.fell_back());
        assert_eq!(sel.failed().count(), 0);
        let last = ALL_IMPLEMENTATIONS.len() - 1;
        let sel = choose(all(), |imp| core::ptr::eq(imp, &STANDARD));
        assert_eq!(sel.implementation().name(), "standard");
        assert_eq!(sel.fell_back(), last > 0);
        assert_eq!(sel.failed().count(), last);
        // A candidate listed twice is tried once.
        let sel = choose([&STANDARD, &STANDARD], |_| true);
        assert_eq!(sel.implementation().name(), "standard");
        assert_eq!(Selection::unpack(sel.pack()), Some(sel));
        assert_eq!(Selection::unpack(0), None);
    }

    #[test]
    #[should_panic(expected = "every implementation")]
    fn choose_panics_when_nothing_passes() {
        choose([&STANDARD], |_| false);
    }

    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    #[test]
    fn fallback_order_and_report() {
        extern crate std;
        use std::format;
        // Reject every selection with AES-NI: the next candidate is SSE with AVX2 (or SSE).
        let sel = choose(CANDIDATES.into_iter().map(Implementation::select), |imp| {
            !imp.name().contains("aes")
        });
        let has = crate::x86_64::cpu_features();
        if has.contains(Use::AES) {
            assert!(sel.fell_back());
            assert!(sel.failed().all(|imp| imp.name().contains("aes")));
            assert!(format!("{sel}").contains("fell back"));
        }
        assert!(!sel.implementation().name().contains("aes"));
        // Everything but the portable code failing: `failed` lists the trial order.
        extern crate alloc;
        use alloc::vec::Vec;
        let tried = CANDIDATES.into_iter().map(Implementation::select);
        let sel = choose(tried.clone(), |imp| core::ptr::eq(imp, &STANDARD));
        let mut want: Vec<&str> = Vec::new();
        for imp in tried.filter(|imp| !core::ptr::eq(*imp, &STANDARD)) {
            if !want.contains(&imp.name()) {
                want.push(imp.name());
            }
        }
        assert_eq!(sel.failed().map(Implementation::name).collect::<Vec<_>>(), want);
        assert_eq!(format!("{:?}", Use::SSE | Use::AES), "Use(SSE | AES)");
        assert_eq!(format!("{:?}", Use::STANDARD), "Use(STANDARD)");
    }
}
