//! ECHO-512 of one 64-byte message with AES-NI (Core: `x86_aesni_echo::FullStateRound` and
//! `ssse3_echo::ShiftAndMix`). The sixteen cells stay in vector registers for all ten rounds,
//! where Core reloads them from memory for each half round.
//!
//! Unlike the other backends, the helpers here are `#[inline(always)]` functions that call the
//! SSE2 intrinsics in `unsafe` blocks: rustc rejects `#[inline(always)]` together with
//! `#[target_feature]`, and SSE2 is part of x86_64, so no feature check is needed.

use core::arch::x86_64::*;

use crate::portable::echo::{COUNTER, final_block};

/// `x * 2` in GF(2^8) for each byte.
#[inline(always)]
fn mul2(x: __m128i) -> __m128i {
    // SAFETY: SSE2 is part of x86_64.
    unsafe {
        let msb = _mm_cmplt_epi8(x, _mm_setzero_si128());
        _mm_xor_si128(_mm_add_epi8(x, x), _mm_and_si128(msb, _mm_set1_epi8(0x1b)))
    }
}

/// Core: `ssse3_echo::MixColumn`.
#[inline(always)]
fn mix_column(a: __m128i, b: __m128i, c: __m128i, d: __m128i) -> [__m128i; 4] {
    // SAFETY: SSE2 is part of x86_64.
    unsafe {
        let ab = _mm_xor_si128(a, b);
        let bc = _mm_xor_si128(b, c);
        let cd = _mm_xor_si128(c, d);
        let abx = mul2(ab);
        let bcx = mul2(bc);
        let cdx = mul2(cd);
        [
            _mm_xor_si128(_mm_xor_si128(abx, bc), d),
            _mm_xor_si128(_mm_xor_si128(bcx, a), cd),
            _mm_xor_si128(_mm_xor_si128(cdx, ab), d),
            _mm_xor_si128(_mm_xor_si128(_mm_xor_si128(abx, bcx), _mm_xor_si128(cdx, ab)), c),
        ]
    }
}

/// ECHO-512 of `input`.
///
/// # Safety
///
/// The CPU must support AES-NI.
#[target_feature(enable = "aes")]
pub(crate) unsafe fn hash(input: &[u8; 64]) -> [u8; 64] {
    let buf = final_block(input);
    let iv = _mm_set_epi32(0, 0, 0, 512);
    // SAFETY: each load reads 16 bytes inside `buf`.
    let block: [__m128i; 8] =
        core::array::from_fn(|i| unsafe { _mm_loadu_si128(buf.as_ptr().add(16 * i).cast()) });
    let mut w: [__m128i; 16] = [
        iv, iv, iv, iv, iv, iv, iv, iv, block[0], block[1], block[2], block[3], block[4], block[5],
        block[6], block[7],
    ];
    let zero = _mm_setzero_si128();
    let one = _mm_set_epi32(0, 0, 0, 1);
    // The counter's low word never carries (it ends at 512 + 160), so one 32-bit add steps it.
    let mut key = _mm_set_epi32(0, 0, 0, COUNTER as i32);
    for _ in 0..10 {
        for cell in w.iter_mut() {
            *cell = _mm_aesenc_si128(_mm_aesenc_si128(*cell, key), zero);
            key = _mm_add_epi32(key, one);
        }
        // ShiftRows moves whole cells: row r of each column rotates by r.
        let [c0, c1, c2, c3] = mix_column(w[0], w[5], w[10], w[15]);
        let [c4, c5, c6, c7] = mix_column(w[4], w[9], w[14], w[3]);
        let [c8, c9, c10, c11] = mix_column(w[8], w[13], w[2], w[7]);
        let [c12, c13, c14, c15] = mix_column(w[12], w[1], w[6], w[11]);
        w = [c0, c1, c2, c3, c4, c5, c6, c7, c8, c9, c10, c11, c12, c13, c14, c15];
    }
    let mut out = [0u8; 64];
    for i in 0..4 {
        let v = _mm_xor_si128(_mm_xor_si128(iv, block[i]), _mm_xor_si128(w[i], w[i + 8]));
        // SAFETY: the store writes 16 bytes inside `out`.
        unsafe { _mm_storeu_si128(out.as_mut_ptr().add(16 * i).cast(), v) };
    }
    out
}
