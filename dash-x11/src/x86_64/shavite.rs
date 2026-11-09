//! SHAvite-3-512 of one 64-byte message with AES-NI (Core: `x86_aesni_shavite::Compress`).
//!
//! Core computes the key schedule a word at a time and moves each AES result through
//! general-purpose registers; here every step works on whole 4-word blocks: the rotated input
//! of a nonlinear step is one shuffle, the `rk[u - 7..u - 3]` window of a linear step one
//! `palignr`, and each `aes_round(x, 0) ^ k` of `CompressElement` one `aesenc` keyed by `k`.

use core::arch::x86_64::*;

use crate::portable::shavite::{COUNTER, IV, final_block};

/// SHAvite-3-512 of `input`.
///
/// # Safety
///
/// The CPU must support AES-NI and SSSE3.
#[target_feature(enable = "aes,ssse3")]
pub(crate) unsafe fn hash(input: &[u8; 64]) -> [u8; 64] {
    let block = final_block(input);
    let zero = _mm_setzero_si128();
    let c0 = COUNTER as i32;
    // The counter words xored in at rk[32], rk[164], rk[316] and rk[440] (count1..3 are zero).
    let ctr = |j: usize| match j {
        8 => Some(_mm_set_epi32(-1, 0, 0, c0)),
        41 => Some(_mm_set_epi32(!c0, 0, 0, 0)),
        79 => Some(_mm_set_epi32(-1, c0, 0, 0)),
        110 => Some(_mm_set_epi32(-1, 0, c0, 0)),
        _ => None,
    };
    // rk as 112 blocks of four words.
    let mut rk = [zero; 112];
    for (j, k) in rk.iter_mut().take(8).enumerate() {
        // SAFETY: the load reads 4 words inside `block`.
        *k = unsafe { _mm_loadu_si128(block.as_ptr().add(4 * j).cast()) };
    }
    let mut j = 8;
    loop {
        for _ in 0..8 {
            // (rk[u - 31], rk[u - 30], rk[u - 29], rk[u - 32]) through a keyless AES round.
            let x = _mm_aesenc_si128(_mm_shuffle_epi32::<0x39>(rk[j - 8]), zero);
            rk[j] = _mm_xor_si128(x, rk[j - 1]);
            if let Some(c) = ctr(j) {
                rk[j] = _mm_xor_si128(rk[j], c);
            }
            j += 1;
        }
        if j == 112 {
            break;
        }
        for _ in 0..8 {
            // rk[u..u + 4] = rk[u - 32..u - 28] ^ rk[u - 7..u - 3].
            rk[j] = _mm_xor_si128(rk[j - 8], _mm_alignr_epi8::<4>(rk[j - 1], rk[j - 2]));
            j += 1;
        }
    }
    // SAFETY: each load reads 4 words inside `IV`.
    let iv: [__m128i; 4] =
        core::array::from_fn(|q| unsafe { _mm_loadu_si128(IV.as_ptr().add(4 * q).cast()) });
    let mut p = iv;
    for keys in rk.as_chunks::<8>().0 {
        let f = |r: __m128i, k: &[__m128i]| {
            let mut x = _mm_xor_si128(r, k[0]);
            x = _mm_aesenc_si128(x, k[1]);
            x = _mm_aesenc_si128(x, k[2]);
            x = _mm_aesenc_si128(x, k[3]);
            _mm_aesenc_si128(x, zero)
        };
        let a = _mm_xor_si128(p[0], f(p[1], &keys[..4]));
        let c = _mm_xor_si128(p[2], f(p[3], &keys[4..]));
        // WROT: (p0, p1, p2, p3) = (p3, p0, p1, p2), with p0 and p2 updated.
        p = [p[3], a, p[1], c];
    }
    let mut out = [0u8; 64];
    for q in 0..4 {
        // SAFETY: the store writes 16 bytes inside `out`.
        unsafe {
            _mm_storeu_si128(out.as_mut_ptr().add(16 * q).cast(), _mm_xor_si128(iv[q], p[q]))
        };
    }
    out
}
