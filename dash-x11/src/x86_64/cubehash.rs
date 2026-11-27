//! CubeHash-16/32-512 of one 64-byte message on vector registers (Core: `crypto/x11/cubehash.c`,
//! built small-footprint, which keeps the 32 state words in memory).
//!
//! The state is CubeHash's 32 words `x[0..32]`; a round is the specification's ten steps:
//! add `x[i]` into `x[i + 16]`, rotate `x[0..16]` left by 7, swap `x[i]` with `x[i + 8]`, xor
//! `x[i + 16]` into `x[i]`, swap the pairs `x[16 + i]`, `x[16 + i + 2]`, add again, rotate by 11,
//! swap `x[i]` with `x[i + 4]`, xor again, and swap the pairs `x[16 + i]`, `x[16 + i + 1]`. The
//! swaps within `x[0..16]` move whole vectors, so they are renamings; the others are shuffles.
//!
//! The message is two 32-byte blocks and the padding block `0x80, 0, ...`, sixteen rounds each;
//! then `x[31] ^= 1` and ten times sixteen finalisation rounds.

use core::arch::x86_64::*;

use crate::portable::cubehash::IV512 as IV;

/// Rotates each 32-bit lane left by `N` (SSE2).
#[inline]
#[target_feature(enable = "sse2")]
fn rotl128<const N: i32, const M: i32>(x: __m128i) -> __m128i {
    _mm_or_si128(_mm_slli_epi32::<N>(x), _mm_srli_epi32::<M>(x))
}

/// Rotates each 32-bit lane left by `N` (AVX2).
#[inline]
#[target_feature(enable = "avx2")]
fn rotl256<const N: i32, const M: i32>(x: __m256i) -> __m256i {
    _mm256_or_si256(_mm256_slli_epi32::<N>(x), _mm256_srli_epi32::<M>(x))
}

/// Reads 32 bytes of the message as two vectors of little-endian words.
#[inline]
#[target_feature(enable = "sse2")]
fn load2(block: &[u8; 32]) -> [__m128i; 2] {
    // SAFETY: each load reads 16 bytes inside `block`.
    unsafe {
        [_mm_loadu_si128(block.as_ptr().cast()), _mm_loadu_si128(block.as_ptr().add(16).cast())]
    }
}

/// CubeHash-512 of `input` with SSE2.
///
/// # Safety
///
/// None beyond x86_64 (SSE2 is part of it); `unsafe` for the backend function type.
#[target_feature(enable = "sse2")]
pub(crate) unsafe fn hash_sse2(input: &[u8; 64]) -> [u8; 64] {
    // x[0..4], x[4..8], ... x[28..32].
    // SAFETY: each load reads 4 words inside `IV`.
    let mut s: [__m128i; 8] =
        core::array::from_fn(|i| unsafe { _mm_loadu_si128(IV.as_ptr().add(4 * i).cast()) });

    #[inline]
    #[target_feature(enable = "sse2")]
    fn rounds(s: &mut [__m128i; 8], n: usize) {
        let [mut a0, mut a1, mut b0, mut b1, mut c0, mut c1, mut d0, mut d1] = *s;
        for _ in 0..n {
            c0 = _mm_add_epi32(c0, a0);
            c1 = _mm_add_epi32(c1, a1);
            d0 = _mm_add_epi32(d0, b0);
            d1 = _mm_add_epi32(d1, b1);
            // Rotate, then swap x[0..8] with x[8..16] by renaming.
            let (na0, na1) = (rotl128::<7, 25>(b0), rotl128::<7, 25>(b1));
            let (nb0, nb1) = (rotl128::<7, 25>(a0), rotl128::<7, 25>(a1));
            a0 = _mm_xor_si128(na0, c0);
            a1 = _mm_xor_si128(na1, c1);
            b0 = _mm_xor_si128(nb0, d0);
            b1 = _mm_xor_si128(nb1, d1);
            c0 = _mm_shuffle_epi32::<0x4e>(c0);
            c1 = _mm_shuffle_epi32::<0x4e>(c1);
            d0 = _mm_shuffle_epi32::<0x4e>(d0);
            d1 = _mm_shuffle_epi32::<0x4e>(d1);
            c0 = _mm_add_epi32(c0, a0);
            c1 = _mm_add_epi32(c1, a1);
            d0 = _mm_add_epi32(d0, b0);
            d1 = _mm_add_epi32(d1, b1);
            // Rotate, then swap x[i] with x[i + 4] by renaming.
            let (na0, na1) = (rotl128::<11, 21>(a1), rotl128::<11, 21>(a0));
            let (nb0, nb1) = (rotl128::<11, 21>(b1), rotl128::<11, 21>(b0));
            a0 = _mm_xor_si128(na0, c0);
            a1 = _mm_xor_si128(na1, c1);
            b0 = _mm_xor_si128(nb0, d0);
            b1 = _mm_xor_si128(nb1, d1);
            c0 = _mm_shuffle_epi32::<0xb1>(c0);
            c1 = _mm_shuffle_epi32::<0xb1>(c1);
            d0 = _mm_shuffle_epi32::<0xb1>(d0);
            d1 = _mm_shuffle_epi32::<0xb1>(d1);
        }
        *s = [a0, a1, b0, b1, c0, c1, d0, d1];
    }

    for block in input.as_chunks::<32>().0 {
        let [m0, m1] = load2(block);
        s[0] = _mm_xor_si128(s[0], m0);
        s[1] = _mm_xor_si128(s[1], m1);
        rounds(&mut s, 16);
    }
    s[0] = _mm_xor_si128(s[0], _mm_set_epi32(0, 0, 0, 0x80));
    rounds(&mut s, 16);
    s[7] = _mm_xor_si128(s[7], _mm_set_epi32(1, 0, 0, 0));
    rounds(&mut s, 160);
    let mut out = [0u8; 64];
    for (i, v) in s[..4].iter().enumerate() {
        // SAFETY: the store writes 16 bytes inside `out`.
        unsafe { _mm_storeu_si128(out.as_mut_ptr().add(16 * i).cast(), *v) };
    }
    out
}

/// CubeHash-512 of `input` with AVX2.
///
/// # Safety
///
/// The CPU must support AVX2.
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn hash_avx2(input: &[u8; 64]) -> [u8; 64] {
    // x[0..8], x[8..16], x[16..24], x[24..32].
    // SAFETY: each load reads 8 words inside `IV`.
    let mut s: [__m256i; 4] =
        core::array::from_fn(|i| unsafe { _mm256_loadu_si256(IV.as_ptr().add(8 * i).cast()) });

    #[inline]
    #[target_feature(enable = "avx2")]
    fn rounds(s: &mut [__m256i; 4], n: usize) {
        let [mut a, mut b, mut c, mut d] = *s;
        for _ in 0..n {
            c = _mm256_add_epi32(c, a);
            d = _mm256_add_epi32(d, b);
            // Rotate, then swap x[0..8] with x[8..16] by renaming.
            let (na, nb) = (rotl256::<7, 25>(b), rotl256::<7, 25>(a));
            a = _mm256_xor_si256(na, c);
            b = _mm256_xor_si256(nb, d);
            c = _mm256_shuffle_epi32::<0x4e>(c);
            d = _mm256_shuffle_epi32::<0x4e>(d);
            c = _mm256_add_epi32(c, a);
            d = _mm256_add_epi32(d, b);
            // Rotate, then swap x[i] with x[i + 4]: the 128-bit halves of each vector.
            a = _mm256_xor_si256(_mm256_permute4x64_epi64::<0x4e>(rotl256::<11, 21>(a)), c);
            b = _mm256_xor_si256(_mm256_permute4x64_epi64::<0x4e>(rotl256::<11, 21>(b)), d);
            c = _mm256_shuffle_epi32::<0xb1>(c);
            d = _mm256_shuffle_epi32::<0xb1>(d);
        }
        *s = [a, b, c, d];
    }

    for block in input.as_chunks::<32>().0 {
        // SAFETY: the load reads the 32 bytes of `block`.
        let m = unsafe { _mm256_loadu_si256(block.as_ptr().cast()) };
        s[0] = _mm256_xor_si256(s[0], m);
        rounds(&mut s, 16);
    }
    s[0] = _mm256_xor_si256(s[0], _mm256_set_epi32(0, 0, 0, 0, 0, 0, 0, 0x80));
    rounds(&mut s, 16);
    s[3] = _mm256_xor_si256(s[3], _mm256_set_epi32(1, 0, 0, 0, 0, 0, 0, 0));
    rounds(&mut s, 160);
    let mut out = [0u8; 64];
    // SAFETY: the stores write 64 bytes inside `out`.
    unsafe {
        _mm256_storeu_si256(out.as_mut_ptr().cast(), s[0]);
        _mm256_storeu_si256(out.as_mut_ptr().add(32).cast(), s[1]);
    }
    out
}
