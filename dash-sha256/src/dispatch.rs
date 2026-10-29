// Copyright (c) 2014-2022 The Bitcoin Core developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or http://www.opensource.org/licenses/mit-license.php.

//! Backend selection (Core: `SHA256AutoDetect` and `SelfTest` in `crypto/sha256.cpp`).

use std::sync::OnceLock;

#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
use crate::x86_64::{avx2, shani, sse4, sse41};
use crate::{INIT, portable};

/// Transforms `blocks` into the state. Unsafe only because the backend may need CPU features
/// that [`Implementation::detect`] checked.
pub(crate) type TransformFn = unsafe fn(&mut [u32; 8], &[[u8; 64]]);
type D64Fn = unsafe fn(&mut [u8; 32], &[u8; 64]);
type D64WayFn<const N: usize> = unsafe fn(&mut [[u8; 32]; N], &[[u8; 64]; N]);

/// Which optimized backends detection may use (Core: `sha256_implementation::UseImplementation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Use(u8);

impl Use {
    /// Only the portable code.
    pub const STANDARD: Use = Use(0);
    /// The SSE4 single-stream transform and the SSE4.1 4-way code. On x86_64, required by
    /// [`AVX2`](Self::AVX2) and [`SHANI`](Self::SHANI), as in Core.
    pub const SSE4: Use = Use(1 << 0);
    /// The AVX2 8-way code.
    pub const AVX2: Use = Use(1 << 1);
    /// SHA-NI on x86_64, the SHA2 extensions on aarch64.
    pub const SHANI: Use = Use(1 << 2);
    /// Everything (Core's default).
    pub const ALL: Use = Use(Self::SSE4.0 | Self::AVX2.0 | Self::SHANI.0);

    /// The backends allowed by either.
    pub const fn union(self, other: Use) -> Use {
        Use(self.0 | other.0)
    }

    /// Whether every backend in `other` is allowed.
    pub const fn contains(self, other: Use) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for Use {
    type Output = Use;
    fn bitor(self, rhs: Use) -> Use {
        self.union(rhs)
    }
}

/// A set of SHA-256 backends selected for this CPU: the single-stream transform and the
/// 1/2/4/8-way double SHA-256 of 64-byte inputs.
///
/// Instances exist only as the statics [`Implementation::detect`] returns, after it checked
/// the CPU features each backend needs.
pub struct Implementation {
    name: &'static str,
    transform: TransformFn,
    d64: D64Fn,
    d64_2way: Option<D64WayFn<2>>,
    d64_4way: Option<D64WayFn<4>>,
    d64_8way: Option<D64WayFn<8>>,
}

impl core::fmt::Debug for Implementation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name)
    }
}

static STANDARD: Implementation = Implementation {
    name: "standard",
    transform: portable::transform,
    d64: portable::transform_d64,
    d64_2way: None,
    d64_4way: None,
    d64_8way: None,
};

#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
static SSE4: Implementation = Implementation {
    name: "sse4(1way),sse41(4way)",
    transform: sse4::transform,
    d64: sse4::transform_d64,
    d64_2way: None,
    d64_4way: Some(sse41::transform_d64_4way),
    d64_8way: None,
};

#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
static SSE4_AVX2: Implementation = Implementation {
    name: "sse4(1way),sse41(4way),avx2(8way)",
    transform: sse4::transform,
    d64: sse4::transform_d64,
    d64_2way: None,
    d64_4way: Some(sse41::transform_d64_4way),
    d64_8way: Some(avx2::transform_d64_8way),
};

#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
static X86_SHANI: Implementation = Implementation {
    name: "x86_shani(1way,2way)",
    transform: shani::transform,
    d64: shani::transform_d64,
    d64_2way: Some(shani::transform_d64_2way),
    d64_4way: None,
    d64_8way: None,
};

#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
static ARM_SHANI: Implementation = Implementation {
    name: "arm_shani(1way,2way)",
    transform: crate::aarch64::transform,
    d64: crate::aarch64::transform_d64,
    d64_2way: Some(crate::aarch64::transform_d64_2way),
    d64_4way: None,
    d64_8way: None,
};

/// Runs an `N`-way double SHA-256 over every whole group of `N` inputs and returns the rest.
/// `f` must come from an [`Implementation`], and `out` and `input` have the same length.
fn d64_ways<'o, 'i, const N: usize>(
    f: Option<D64WayFn<N>>,
    out: &'o mut [[u8; 32]],
    input: &'i [[u8; 64]],
) -> (&'o mut [[u8; 32]], &'i [[u8; 64]]) {
    let Some(f) = f else {
        return (out, input);
    };
    let (out_groups, out_rest) = out.as_chunks_mut::<N>();
    let (in_groups, in_rest) = input.as_chunks::<N>();
    for (o, i) in out_groups.iter_mut().zip(in_groups) {
        // SAFETY: `select` only returns implementations whose CPU features are present.
        unsafe { f(o, i) };
    }
    (out_rest, in_rest)
}

/// Whether an `N`-way double SHA-256, if present, gives Core's self-test results.
fn d64_ways_ok<const N: usize>(f: Option<D64WayFn<N>>, blocks: &[[u8; 64]]) -> bool {
    let Some(f) = f else {
        return true;
    };
    let mut out = [[0; 32]; N];
    // SAFETY: as in `d64_ways`.
    unsafe { f(&mut out, blocks.first_chunk().unwrap()) };
    out[..] == SELF_TEST_D64[..N]
}

impl Implementation {
    /// The best backends this CPU supports among those `allow`ed, checked by Core's self-test
    /// (Core: `SHA256AutoDetect`). The self-test runs on every call; [`auto`](Self::auto)
    /// caches its result.
    ///
    /// # Panics
    ///
    /// If the self-test fails, where Core's `assert` aborts. A CPU that computes SHA-256
    /// wrongly must not validate anything.
    pub fn detect(allow: Use) -> &'static Implementation {
        let imp = Self::select(allow);
        assert!(imp.self_test(), "SHA-256 self-test failed for {}", imp.name);
        imp
    }

    /// [`detect`](Self::detect) with every backend allowed, computed on first use.
    pub fn auto() -> &'static Implementation {
        static AUTO: OnceLock<&'static Implementation> = OnceLock::new();
        AUTO.get_or_init(|| Self::detect(Use::ALL))
    }

    /// The portable implementation, which every CPU runs.
    pub fn standard() -> &'static Implementation {
        &STANDARD
    }

    /// Core's name for this selection, as `SHA256AutoDetect` returns it.
    pub fn name(&self) -> &'static str {
        self.name
    }

    #[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
    fn select(allow: Use) -> &'static Implementation {
        use std::arch::is_x86_feature_detected as has;
        // Core checks CPUID.1:ECX.SSE4_1, then SHA-NI and AVX2 only if SSE4 is allowed and
        // present. Its SSE4 assembly also uses SSSE3, which Core assumes with SSE4.1 and this
        // checks. For AVX2 Core also requires AVX and the OS's XSAVE support (`AVXEnabled`);
        // std's "avx" and "avx2" checks include XCR0.
        if !(allow.contains(Use::SSE4) && has!("sse4.1") && has!("ssse3")) {
            return &STANDARD;
        }
        if allow.contains(Use::SHANI) && has!("sha") {
            // Core disables the SSE4 and AVX2 code when SHA-NI is present.
            return &X86_SHANI;
        }
        if allow.contains(Use::AVX2) && has!("avx") && has!("avx2") {
            return &SSE4_AVX2;
        }
        &SSE4
    }

    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    fn select(allow: Use) -> &'static Implementation {
        if allow.contains(Use::SHANI) && std::arch::is_aarch64_feature_detected!("sha2") {
            return &ARM_SHANI;
        }
        &STANDARD
    }

    #[cfg(not(any(
        all(target_arch = "x86_64", target_pointer_width = "64"),
        all(target_arch = "aarch64", target_endian = "little")
    )))]
    fn select(_allow: Use) -> &'static Implementation {
        &STANDARD
    }

    /// Runs the single-stream transform on `blocks`.
    #[inline]
    pub(crate) fn transform(&self, s: &mut [u32; 8], blocks: &[[u8; 64]]) {
        // SAFETY: `select` only returns implementations whose CPU features are present.
        unsafe { (self.transform)(s, blocks) }
    }

    /// Double SHA-256 of each 64-byte input (Core: `SHA256D64`), using the widest backends
    /// first.
    ///
    /// # Panics
    ///
    /// If `out` and `input` differ in length.
    pub fn sha256d64(&self, out: &mut [[u8; 32]], input: &[[u8; 64]]) {
        assert_eq!(out.len(), input.len(), "sha256d64: output and input lengths differ");
        let (out, input) = d64_ways(self.d64_8way, out, input);
        let (out, input) = d64_ways(self.d64_4way, out, input);
        let (out, input) = d64_ways(self.d64_2way, out, input);
        for (o, i) in out.iter_mut().zip(input) {
            // SAFETY: `select` only returns implementations whose CPU features are present.
            unsafe { (self.d64)(o, i) };
        }
    }

    /// One merkle tree level in place: replaces the first half of `hashes` with the double
    /// SHA-256 of each pair, `hashes[2i] || hashes[2i + 1]`, and returns that count (Core:
    /// `ComputeMerkleRoot`'s `SHA256D64(hashes[0].begin(), hashes[0].begin(), size / 2)`).
    ///
    /// The rest of `ComputeMerkleRoot` is the caller's, in Core's order: first the mutation
    /// check over the level's pairs, then duplicating the last hash of an odd level, then this.
    /// Duplicating first would make every odd level look mutated.
    ///
    /// # Panics
    ///
    /// If `hashes` has an odd length.
    pub fn sha256d64_in_place(&self, hashes: &mut [[u8; 32]]) -> usize {
        assert!(hashes.len().is_multiple_of(2), "sha256d64_in_place: odd number of hashes");
        let pairs = hashes.len() / 2;
        // The first pairs' outputs overlap their own inputs, so they go through a copy.
        let first = pairs.min(8);
        let mut input = [[0; 64]; 8];
        let mut out = [[0; 32]; 8];
        input[..first].as_flattened_mut().copy_from_slice(hashes[..2 * first].as_flattened());
        self.sha256d64(&mut out[..first], &input[..first]);
        hashes[..first].copy_from_slice(&out[..first]);
        // From then on, the outputs of the next `n <= done` pairs, `done..done + n`, lie before
        // their inputs, which start at `2 * done`.
        let mut done = first;
        while done < pairs {
            let n = done.min(pairs - done);
            let (written, unread) = hashes.split_at_mut(2 * done);
            let input = unread[..2 * n].as_flattened().as_chunks::<64>().0;
            self.sha256d64(&mut written[done..done + n], input);
            done += n;
        }
        pairs
    }

    /// Core's `SelfTest`: the transform for 0 to 8 blocks, and every double SHA-256 width.
    fn self_test(&self) -> bool {
        let (blocks, _) = SELF_TEST_DATA.as_bytes()[1..].as_chunks::<64>();
        for (i, expected) in SELF_TEST_STATES.iter().enumerate() {
            let mut state = INIT;
            self.transform(&mut state, &blocks[..i]);
            if state != *expected {
                return false;
            }
        }
        let mut out = [0; 32];
        // SAFETY: as in `sha256d64`.
        unsafe { (self.d64)(&mut out, &blocks[0]) };
        out == SELF_TEST_D64[0]
            && d64_ways_ok(self.d64_2way, blocks)
            && d64_ways_ok(self.d64_4way, blocks)
            && d64_ways_ok(self.d64_8way, blocks)
    }
}

/// The start of Core's self-test input: one byte, so the blocks after it are not aligned, then
/// 512 bytes of text (`crypto/sha256.cpp` `SelfTest`; Core's array is zero-filled to 641 bytes,
/// and the test reads only these).
const SELF_TEST_DATA: &str = concat!(
    "-",
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do ",
    "eiusmod tempor incididunt ut labore et dolore magna aliqua. Et m",
    "olestie ac feugiat sed lectus vestibulum mattis ullamcorper. Mor", // spellchecker:disable
    "bi blandit cursus risus at ultrices mi tempus imperdiet nulla. N",
    "unc congue nisi vita suscipit tellus mauris. Imperdiet proin fer",
    "mentum leo vel orci. Massa tempor nec feugiat nisl pretium fusce",
    " id velit. Telus in metus vulputate eu scelerisque felis. Mi tem",
    "pus imperdiet nulla malesuada pellentesque. Tristique magna sit.",
);

/// The state after transforming the first `i` blocks of the self-test input (Core: `result`).
const SELF_TEST_STATES: [[u32; 8]; 9] = [
    [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ],
    [
        0x91f8ec6b, 0x4da10fe3, 0x1c9c292c, 0x45e18185, 0x435cc111, 0x3ca26f09, 0xeb954cae,
        0x402a7069,
    ],
    [
        0xcabea5ac, 0x374fb97c, 0x182ad996, 0x7bd69cbf, 0x450ff900, 0xc1d2be8a, 0x6a41d505,
        0xe6212dc3,
    ],
    [
        0xbcff09d6, 0x3e76f36e, 0x3ecb2501, 0x78866e97, 0xe1c1e2fd, 0x32f4eaff, 0x8aa6c4e5,
        0xdfc024bc,
    ],
    [
        0xa08c5d94, 0x0a862f93, 0x6b7f2f40, 0x8f9fae76, 0x6d40439f, 0x79dcee0c, 0x3e39ff3a,
        0xdc3bdbb1,
    ],
    [
        0x216a0895, 0x9f1a3662, 0xe99946f9, 0x87ba4364, 0x0fb5db2c, 0x12bed3d3, 0x6689c0c7,
        0x292f1b04,
    ],
    [
        0xca3067f8, 0xbc8c2656, 0x37cb7e0d, 0x9b6b8b0f, 0x46dc380b, 0xf1287f57, 0xc42e4b23,
        0x3fefe94d,
    ],
    [
        0x3e4c4039, 0xbb6fca8c, 0x6f27d2f7, 0x301e44a4, 0x8352ba14, 0x5769ce37, 0x48a1155f,
        0xc0e1c4c6,
    ],
    [
        0xfe2fa9dd, 0x69d0862b, 0x1ae0db23, 0x471f9244, 0xf55c0145, 0xc30f9c3b, 0x40a84ea0,
        0x5b8a266c,
    ],
];

/// Double SHA-256 of each of the 8 self-test blocks (Core: `result_d64`).
const SELF_TEST_D64: [[u8; 32]; 8] = [
    [
        0x09, 0x3a, 0xc4, 0xd0, 0x0f, 0xf7, 0x57, 0xe1, 0x72, 0x85, 0x79, 0x42, 0xfe, 0xe7, 0xe0,
        0xa0, 0xfc, 0x52, 0xd7, 0xdb, 0x07, 0x63, 0x45, 0xfb, 0x53, 0x14, 0x7d, 0x17, 0x22, 0x86,
        0xf0, 0x52,
    ],
    [
        0x48, 0xb6, 0x11, 0x9e, 0x6e, 0x48, 0x81, 0x6d, 0xcc, 0x57, 0x1f, 0xb2, 0x97, 0xa8, 0xd5,
        0x25, 0x9b, 0x82, 0xaa, 0x89, 0xe2, 0xfd, 0x2d, 0x56, 0xe8, 0x28, 0x83, 0x0b, 0xe2, 0xfa,
        0x53, 0xb7,
    ],
    [
        0xd6, 0x6b, 0x07, 0x85, 0x83, 0xb0, 0x10, 0xa2, 0xf5, 0x51, 0x3c, 0xf9, 0x60, 0x03, 0xab,
        0x45, 0x6c, 0x15, 0x6e, 0xef, 0xb5, 0xac, 0x3e, 0x6c, 0xdf, 0xb4, 0x92, 0x22, 0x2d, 0xce,
        0xbf, 0x3e,
    ],
    [
        0xe9, 0xe5, 0xf6, 0x29, 0x0e, 0x01, 0x4f, 0xd2, 0xd4, 0x45, 0x65, 0xb3, 0xbb, 0xf2, 0x4c,
        0x16, 0x37, 0x50, 0x3c, 0x6e, 0x49, 0x8c, 0x5a, 0x89, 0x2b, 0x1b, 0xab, 0xc4, 0x37, 0xd1,
        0x46, 0xe9,
    ],
    [
        0x3d, 0x0e, 0x85, 0xa2, 0x50, 0x73, 0xa1, 0x5e, 0x54, 0x37, 0xd7, 0x94, 0x17, 0x56, 0xc2,
        0xd8, 0xe5, 0x9f, 0xed, 0x4e, 0xae, 0x15, 0x42, 0x06, 0x0d, 0x74, 0x74, 0x5e, 0x24, 0x30,
        0xce, 0xd1,
    ],
    [
        0x9e, 0x50, 0xa3, 0x9a, 0xb8, 0xf0, 0x4a, 0x57, 0x69, 0x78, 0x67, 0x12, 0x84, 0x58, 0xbe,
        0xc7, 0x36, 0xaa, 0xee, 0x7c, 0x64, 0xa3, 0x76, 0xec, 0xff, 0x55, 0x41, 0x00, 0x2a, 0x44,
        0x68, 0x4d,
    ],
    [
        0xb6, 0x53, 0x9e, 0x1c, 0x95, 0xb7, 0xca, 0xdc, 0x7f, 0x7d, 0x74, 0x27, 0x5c, 0x8e, 0xa6,
        0x84, 0xb5, 0xac, 0x87, 0xa9, 0xf3, 0xff, 0x75, 0xf2, 0x34, 0xcd, 0x1a, 0x3b, 0x82, 0x2c,
        0x2b, 0x4e,
    ],
    [
        0x6a, 0x46, 0x30, 0xa6, 0x89, 0x86, 0x23, 0xac, 0xf8, 0xa5, 0x15, 0xe9, 0x0a, 0xaa, 0x1e,
        0x9a, 0xd7, 0x93, 0x6b, 0x28, 0xe4, 0x3b, 0xfd, 0x59, 0xc6, 0xed, 0x7c, 0x5f, 0xa5, 0x41,
        0xcb, 0x51,
    ],
];
