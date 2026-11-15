// Copyright (c) 2018-2022 The Bitcoin Core developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or http://www.opensource.org/licenses/mit-license.php.
//
// Based on https://github.com/noloader/SHA-Intrinsics/blob/master/sha256-x86.c,
// Written and placed in public domain by Jeffrey Walton.
// Based on code from Intel, and by Sean Gulley for the miTLS project.

//! The SHA-NI transforms (Core: `sha256_x86_shani::Transform` and
//! `sha256d64_x86_shani::Transform_2way`, `crypto/sha256_x86_shani.cpp`).

use core::arch::x86_64::*;

use crate::portable;

#[repr(C, align(16))]
struct Align16([u8; 16]);

static MASK: Align16 = Align16([
    0x03, 0x02, 0x01, 0x00, 0x07, 0x06, 0x05, 0x04, 0x0b, 0x0a, 0x09, 0x08, 0x0f, 0x0e, 0x0d, 0x0c,
]);
static INIT0: Align16 = Align16([
    0x8c, 0x68, 0x05, 0x9b, 0x7f, 0x52, 0x0e, 0x51, 0x85, 0xae, 0x67, 0xbb, 0x67, 0xe6, 0x09, 0x6a,
]);
static INIT1: Align16 = Align16([
    0x19, 0xcd, 0xe0, 0x5b, 0xab, 0xd9, 0x83, 0x1f, 0x3a, 0xf5, 0x4f, 0xa5, 0x72, 0xf3, 0x6e, 0x3c,
]);

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn load_const(c: &Align16) -> __m128i {
    // SAFETY: `c` is 16 bytes, 16-byte aligned.
    unsafe { _mm_load_si128(c.0.as_ptr().cast()) }
}

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn init0() -> __m128i {
    load_const(&INIT0)
}

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn init1() -> __m128i {
    load_const(&INIT1)
}

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn set_epi64x(k1: u64, k0: u64) -> __m128i {
    _mm_set_epi64x(k1 as i64, k0 as i64)
}

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn quad_round(state0: &mut __m128i, state1: &mut __m128i, k1: u64, k0: u64) {
    let msg = set_epi64x(k1, k0);
    *state1 = _mm_sha256rnds2_epu32(*state1, *state0, msg);
    *state0 = _mm_sha256rnds2_epu32(*state0, *state1, _mm_shuffle_epi32::<0x0e>(msg));
}

/// Core's `QuadRound` overload with a message.
#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn quad_round_msg(state0: &mut __m128i, state1: &mut __m128i, m: __m128i, k1: u64, k0: u64) {
    let msg = _mm_add_epi32(m, set_epi64x(k1, k0));
    *state1 = _mm_sha256rnds2_epu32(*state1, *state0, msg);
    *state0 = _mm_sha256rnds2_epu32(*state0, *state1, _mm_shuffle_epi32::<0x0e>(msg));
}

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn shift_message_a(m0: &mut __m128i, m1: __m128i) {
    *m0 = _mm_sha256msg1_epu32(*m0, m1)
}

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn shift_message_c(m0: __m128i, m1: __m128i, m2: &mut __m128i) {
    *m2 = _mm_sha256msg2_epu32(_mm_add_epi32(*m2, _mm_alignr_epi8::<4>(m1, m0)), m1);
}

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn shift_message_b(m0: &mut __m128i, m1: __m128i, m2: &mut __m128i) {
    shift_message_c(*m0, m1, m2);
    shift_message_a(m0, m1);
}

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn shuffle(s0: &mut __m128i, s1: &mut __m128i) {
    let t1 = _mm_shuffle_epi32::<0xB1>(*s0);
    let t2 = _mm_shuffle_epi32::<0x1B>(*s1);
    *s0 = _mm_alignr_epi8::<0x08>(t1, t2);
    *s1 = _mm_blend_epi16::<0xF0>(t2, t1);
}

#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn unshuffle(s0: &mut __m128i, s1: &mut __m128i) {
    let t1 = _mm_shuffle_epi32::<0x1B>(*s0);
    let t2 = _mm_shuffle_epi32::<0xB1>(*s1);
    *s0 = _mm_blend_epi16::<0xF0>(t1, t2);
    *s1 = _mm_alignr_epi8::<0x08>(t2, t1);
}

/// The 16 bytes at `offset`, as big-endian words.
#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn load(input: &[u8], offset: usize) -> __m128i {
    let bytes = &input[offset..offset + 16];
    // SAFETY: `bytes` is 16 bytes; the load is unaligned.
    _mm_shuffle_epi8(unsafe { _mm_loadu_si128(bytes.as_ptr().cast()) }, load_const(&MASK))
}

/// Stores `s` as big-endian words at `offset`.
#[target_feature(enable = "sha,sse4.1,ssse3")]
#[inline]
fn save(out: &mut [u8], offset: usize, s: __m128i) {
    let bytes = &mut out[offset..offset + 16];
    // SAFETY: `bytes` is 16 bytes; the store is unaligned.
    unsafe { _mm_storeu_si128(bytes.as_mut_ptr().cast(), _mm_shuffle_epi8(s, load_const(&MASK))) }
}

/// Core: `sha256_x86_shani::Transform`.
#[target_feature(enable = "sha,sse4.1,ssse3")]
pub(crate) fn transform(s: &mut [u32; 8], blocks: &[[u8; 64]]) {
    // Load state
    // SAFETY: `s` is 8 words; the loads are unaligned.
    let mut s0 = unsafe { _mm_loadu_si128(s.as_ptr().cast()) };
    let mut s1 = unsafe { _mm_loadu_si128(s.as_ptr().add(4).cast()) };
    shuffle(&mut s0, &mut s1);

    for chunk in blocks {
        // Remember old state
        let so0 = s0;
        let so1 = s1;

        // Load data and transform
        let mut m0 = load(chunk, 0);
        quad_round_msg(&mut s0, &mut s1, m0, 0xe9b5dba5b5c0fbcf, 0x71374491428a2f98);
        let mut m1 = load(chunk, 16);
        quad_round_msg(&mut s0, &mut s1, m1, 0xab1c5ed5923f82a4, 0x59f111f13956c25b);
        shift_message_a(&mut m0, m1);
        let mut m2 = load(chunk, 32);
        quad_round_msg(&mut s0, &mut s1, m2, 0x550c7dc3243185be, 0x12835b01d807aa98);
        shift_message_a(&mut m1, m2);
        let mut m3 = load(chunk, 48);
        quad_round_msg(&mut s0, &mut s1, m3, 0xc19bf1749bdc06a7, 0x80deb1fe72be5d74);
        shift_message_b(&mut m2, m3, &mut m0);
        quad_round_msg(&mut s0, &mut s1, m0, 0x240ca1cc0fc19dc6, 0xefbe4786e49b69c1);
        shift_message_b(&mut m3, m0, &mut m1);
        quad_round_msg(&mut s0, &mut s1, m1, 0x76f988da5cb0a9dc, 0x4a7484aa2de92c6f);
        shift_message_b(&mut m0, m1, &mut m2);
        quad_round_msg(&mut s0, &mut s1, m2, 0xbf597fc7b00327c8, 0xa831c66d983e5152);
        shift_message_b(&mut m1, m2, &mut m3);
        quad_round_msg(&mut s0, &mut s1, m3, 0x1429296706ca6351, 0xd5a79147c6e00bf3);
        shift_message_b(&mut m2, m3, &mut m0);
        quad_round_msg(&mut s0, &mut s1, m0, 0x53380d134d2c6dfc, 0x2e1b213827b70a85);
        shift_message_b(&mut m3, m0, &mut m1);
        quad_round_msg(&mut s0, &mut s1, m1, 0x92722c8581c2c92e, 0x766a0abb650a7354);
        shift_message_b(&mut m0, m1, &mut m2);
        quad_round_msg(&mut s0, &mut s1, m2, 0xc76c51a3c24b8b70, 0xa81a664ba2bfe8a1);
        shift_message_b(&mut m1, m2, &mut m3);
        quad_round_msg(&mut s0, &mut s1, m3, 0x106aa070f40e3585, 0xd6990624d192e819);
        shift_message_b(&mut m2, m3, &mut m0);
        quad_round_msg(&mut s0, &mut s1, m0, 0x34b0bcb52748774c, 0x1e376c0819a4c116);
        shift_message_b(&mut m3, m0, &mut m1);
        quad_round_msg(&mut s0, &mut s1, m1, 0x682e6ff35b9cca4f, 0x4ed8aa4a391c0cb3);
        shift_message_c(m0, m1, &mut m2);
        quad_round_msg(&mut s0, &mut s1, m2, 0x8cc7020884c87814, 0x78a5636f748f82ee);
        shift_message_c(m1, m2, &mut m3);
        quad_round_msg(&mut s0, &mut s1, m3, 0xc67178f2bef9a3f7, 0xa4506ceb90befffa);

        // Combine with old state
        s0 = _mm_add_epi32(s0, so0);
        s1 = _mm_add_epi32(s1, so1);
    }

    unshuffle(&mut s0, &mut s1);
    // SAFETY: as for the loads.
    unsafe {
        _mm_storeu_si128(s.as_mut_ptr().cast(), s0);
        _mm_storeu_si128(s.as_mut_ptr().add(4).cast(), s1);
    }
}

/// Core: `TransformD64Wrapper<sha256_x86_shani::Transform>`.
#[target_feature(enable = "sha,sse4.1,ssse3")]
pub(crate) fn transform_d64(out: &mut [u8; 32], input: &[u8; 64]) {
    // SAFETY: this function has `transform`'s features.
    unsafe { portable::d64_wrapper(transform, out, input) }
}

/// Core: `sha256d64_x86_shani::Transform_2way`.
#[target_feature(enable = "sha,sse4.1,ssse3")]
pub(crate) fn transform_d64_2way(out: &mut [[u8; 32]; 2], input: &[[u8; 64]; 2]) {
    let input = input.as_flattened();
    let out = out.as_flattened_mut();

    // Transform 1
    let mut as0 = init0();
    let mut bs0 = as0;
    let mut as1 = init1();
    let mut bs1 = as1;
    let mut am0 = load(input, 0);
    let mut bm0 = load(input, 64);
    quad_round_msg(&mut as0, &mut as1, am0, 0xe9b5dba5b5c0fbcf, 0x71374491428a2f98);
    quad_round_msg(&mut bs0, &mut bs1, bm0, 0xe9b5dba5b5c0fbcf, 0x71374491428a2f98);
    let mut am1 = load(input, 16);
    let mut bm1 = load(input, 80);
    quad_round_msg(&mut as0, &mut as1, am1, 0xab1c5ed5923f82a4, 0x59f111f13956c25b);
    quad_round_msg(&mut bs0, &mut bs1, bm1, 0xab1c5ed5923f82a4, 0x59f111f13956c25b);
    shift_message_a(&mut am0, am1);
    shift_message_a(&mut bm0, bm1);
    let mut am2 = load(input, 32);
    let mut bm2 = load(input, 96);
    quad_round_msg(&mut as0, &mut as1, am2, 0x550c7dc3243185be, 0x12835b01d807aa98);
    quad_round_msg(&mut bs0, &mut bs1, bm2, 0x550c7dc3243185be, 0x12835b01d807aa98);
    shift_message_a(&mut am1, am2);
    shift_message_a(&mut bm1, bm2);
    let mut am3 = load(input, 48);
    let mut bm3 = load(input, 112);
    quad_round_msg(&mut as0, &mut as1, am3, 0xc19bf1749bdc06a7, 0x80deb1fe72be5d74);
    quad_round_msg(&mut bs0, &mut bs1, bm3, 0xc19bf1749bdc06a7, 0x80deb1fe72be5d74);
    shift_message_b(&mut am2, am3, &mut am0);
    shift_message_b(&mut bm2, bm3, &mut bm0);
    quad_round_msg(&mut as0, &mut as1, am0, 0x240ca1cc0fc19dc6, 0xefbe4786e49b69c1);
    quad_round_msg(&mut bs0, &mut bs1, bm0, 0x240ca1cc0fc19dc6, 0xefbe4786e49b69c1);
    shift_message_b(&mut am3, am0, &mut am1);
    shift_message_b(&mut bm3, bm0, &mut bm1);
    quad_round_msg(&mut as0, &mut as1, am1, 0x76f988da5cb0a9dc, 0x4a7484aa2de92c6f);
    quad_round_msg(&mut bs0, &mut bs1, bm1, 0x76f988da5cb0a9dc, 0x4a7484aa2de92c6f);
    shift_message_b(&mut am0, am1, &mut am2);
    shift_message_b(&mut bm0, bm1, &mut bm2);
    quad_round_msg(&mut as0, &mut as1, am2, 0xbf597fc7b00327c8, 0xa831c66d983e5152);
    quad_round_msg(&mut bs0, &mut bs1, bm2, 0xbf597fc7b00327c8, 0xa831c66d983e5152);
    shift_message_b(&mut am1, am2, &mut am3);
    shift_message_b(&mut bm1, bm2, &mut bm3);
    quad_round_msg(&mut as0, &mut as1, am3, 0x1429296706ca6351, 0xd5a79147c6e00bf3);
    quad_round_msg(&mut bs0, &mut bs1, bm3, 0x1429296706ca6351, 0xd5a79147c6e00bf3);
    shift_message_b(&mut am2, am3, &mut am0);
    shift_message_b(&mut bm2, bm3, &mut bm0);
    quad_round_msg(&mut as0, &mut as1, am0, 0x53380d134d2c6dfc, 0x2e1b213827b70a85);
    quad_round_msg(&mut bs0, &mut bs1, bm0, 0x53380d134d2c6dfc, 0x2e1b213827b70a85);
    shift_message_b(&mut am3, am0, &mut am1);
    shift_message_b(&mut bm3, bm0, &mut bm1);
    quad_round_msg(&mut as0, &mut as1, am1, 0x92722c8581c2c92e, 0x766a0abb650a7354);
    quad_round_msg(&mut bs0, &mut bs1, bm1, 0x92722c8581c2c92e, 0x766a0abb650a7354);
    shift_message_b(&mut am0, am1, &mut am2);
    shift_message_b(&mut bm0, bm1, &mut bm2);
    quad_round_msg(&mut as0, &mut as1, am2, 0xc76c51a3c24b8b70, 0xa81a664ba2bfe8a1);
    quad_round_msg(&mut bs0, &mut bs1, bm2, 0xc76c51a3c24b8b70, 0xa81a664ba2bfe8a1);
    shift_message_b(&mut am1, am2, &mut am3);
    shift_message_b(&mut bm1, bm2, &mut bm3);
    quad_round_msg(&mut as0, &mut as1, am3, 0x106aa070f40e3585, 0xd6990624d192e819);
    quad_round_msg(&mut bs0, &mut bs1, bm3, 0x106aa070f40e3585, 0xd6990624d192e819);
    shift_message_b(&mut am2, am3, &mut am0);
    shift_message_b(&mut bm2, bm3, &mut bm0);
    quad_round_msg(&mut as0, &mut as1, am0, 0x34b0bcb52748774c, 0x1e376c0819a4c116);
    quad_round_msg(&mut bs0, &mut bs1, bm0, 0x34b0bcb52748774c, 0x1e376c0819a4c116);
    shift_message_b(&mut am3, am0, &mut am1);
    shift_message_b(&mut bm3, bm0, &mut bm1);
    quad_round_msg(&mut as0, &mut as1, am1, 0x682e6ff35b9cca4f, 0x4ed8aa4a391c0cb3);
    quad_round_msg(&mut bs0, &mut bs1, bm1, 0x682e6ff35b9cca4f, 0x4ed8aa4a391c0cb3);
    shift_message_c(am0, am1, &mut am2);
    shift_message_c(bm0, bm1, &mut bm2);
    quad_round_msg(&mut as0, &mut as1, am2, 0x8cc7020884c87814, 0x78a5636f748f82ee);
    quad_round_msg(&mut bs0, &mut bs1, bm2, 0x8cc7020884c87814, 0x78a5636f748f82ee);
    shift_message_c(am1, am2, &mut am3);
    shift_message_c(bm1, bm2, &mut bm3);
    quad_round_msg(&mut as0, &mut as1, am3, 0xc67178f2bef9a3f7, 0xa4506ceb90befffa);
    quad_round_msg(&mut bs0, &mut bs1, bm3, 0xc67178f2bef9a3f7, 0xa4506ceb90befffa);
    as0 = _mm_add_epi32(as0, init0());
    bs0 = _mm_add_epi32(bs0, init0());
    as1 = _mm_add_epi32(as1, init1());
    bs1 = _mm_add_epi32(bs1, init1());

    // Transform 2
    let aso0 = as0;
    let bso0 = bs0;
    let aso1 = as1;
    let bso1 = bs1;
    quad_round(&mut as0, &mut as1, 0xe9b5dba5b5c0fbcf, 0x71374491c28a2f98);
    quad_round(&mut bs0, &mut bs1, 0xe9b5dba5b5c0fbcf, 0x71374491c28a2f98);
    quad_round(&mut as0, &mut as1, 0xab1c5ed5923f82a4, 0x59f111f13956c25b);
    quad_round(&mut bs0, &mut bs1, 0xab1c5ed5923f82a4, 0x59f111f13956c25b);
    quad_round(&mut as0, &mut as1, 0x550c7dc3243185be, 0x12835b01d807aa98);
    quad_round(&mut bs0, &mut bs1, 0x550c7dc3243185be, 0x12835b01d807aa98);
    quad_round(&mut as0, &mut as1, 0xc19bf3749bdc06a7, 0x80deb1fe72be5d74);
    quad_round(&mut bs0, &mut bs1, 0xc19bf3749bdc06a7, 0x80deb1fe72be5d74);
    quad_round(&mut as0, &mut as1, 0x240cf2540fe1edc6, 0xf0fe4786649b69c1);
    quad_round(&mut bs0, &mut bs1, 0x240cf2540fe1edc6, 0xf0fe4786649b69c1);
    quad_round(&mut as0, &mut as1, 0x16f988fa61b9411e, 0x6cc984be4fe9346f);
    quad_round(&mut bs0, &mut bs1, 0x16f988fa61b9411e, 0x6cc984be4fe9346f);
    quad_round(&mut as0, &mut as1, 0xb9d99ec7b019fc65, 0xa88e5a6df2c65152);
    quad_round(&mut bs0, &mut bs1, 0xb9d99ec7b019fc65, 0xa88e5a6df2c65152);
    quad_round(&mut as0, &mut as1, 0xc7353eb0fdb1232b, 0xe70eeaa09a1231c3);
    quad_round(&mut bs0, &mut bs1, 0xc7353eb0fdb1232b, 0xe70eeaa09a1231c3);
    quad_round(&mut as0, &mut as1, 0xdc1eeefd5a0f118f, 0xcb976d5f3069bad5);
    quad_round(&mut bs0, &mut bs1, 0xdc1eeefd5a0f118f, 0xcb976d5f3069bad5);
    quad_round(&mut as0, &mut as1, 0xe15d5b1658f4ca9d, 0xde0b7a040a35b689);
    quad_round(&mut bs0, &mut bs1, 0xe15d5b1658f4ca9d, 0xde0b7a040a35b689);
    quad_round(&mut as0, &mut as1, 0x6fab9537a507ea32, 0x37088980007f3e86);
    quad_round(&mut bs0, &mut bs1, 0x6fab9537a507ea32, 0x37088980007f3e86);
    quad_round(&mut as0, &mut as1, 0xc0bbbe37cdaa3b6d, 0x0d8cd6f117406110);
    quad_round(&mut bs0, &mut bs1, 0xc0bbbe37cdaa3b6d, 0x0d8cd6f117406110);
    quad_round(&mut as0, &mut as1, 0x6fd15ca70b02e931, 0xdb48a36383613bda);
    quad_round(&mut bs0, &mut bs1, 0x6fd15ca70b02e931, 0xdb48a36383613bda);
    quad_round(&mut as0, &mut as1, 0x6d4378906ed41a95, 0x31338431521afaca);
    quad_round(&mut bs0, &mut bs1, 0x6d4378906ed41a95, 0x31338431521afaca);
    quad_round(&mut as0, &mut as1, 0x532fb63cb5c9a0e6, 0x9eccabbdc39c91f2);
    quad_round(&mut bs0, &mut bs1, 0x532fb63cb5c9a0e6, 0x9eccabbdc39c91f2);
    quad_round(&mut as0, &mut as1, 0x4c191d76a4954b68, 0x07237ea3d2c741c6);
    quad_round(&mut bs0, &mut bs1, 0x4c191d76a4954b68, 0x07237ea3d2c741c6);
    as0 = _mm_add_epi32(as0, aso0);
    bs0 = _mm_add_epi32(bs0, bso0);
    as1 = _mm_add_epi32(as1, aso1);
    bs1 = _mm_add_epi32(bs1, bso1);

    // Extract hash
    unshuffle(&mut as0, &mut as1);
    unshuffle(&mut bs0, &mut bs1);
    am0 = as0;
    bm0 = bs0;
    am1 = as1;
    bm1 = bs1;

    // Transform 3
    as0 = init0();
    bs0 = as0;
    as1 = init1();
    bs1 = as1;
    quad_round_msg(&mut as0, &mut as1, am0, 0xe9b5dba5b5c0fbcf, 0x71374491428a2f98);
    quad_round_msg(&mut bs0, &mut bs1, bm0, 0xe9b5dba5b5c0fbcf, 0x71374491428a2f98);
    quad_round_msg(&mut as0, &mut as1, am1, 0xab1c5ed5923f82a4, 0x59f111f13956c25b);
    quad_round_msg(&mut bs0, &mut bs1, bm1, 0xab1c5ed5923f82a4, 0x59f111f13956c25b);
    shift_message_a(&mut am0, am1);
    shift_message_a(&mut bm0, bm1);
    am2 = set_epi64x(0x0, 0x80000000);
    bm2 = am2;
    quad_round(&mut as0, &mut as1, 0x550c7dc3243185be, 0x12835b015807aa98);
    quad_round(&mut bs0, &mut bs1, 0x550c7dc3243185be, 0x12835b015807aa98);
    shift_message_a(&mut am1, am2);
    shift_message_a(&mut bm1, bm2);
    am3 = set_epi64x(0x10000000000, 0x0);
    bm3 = am3;
    quad_round(&mut as0, &mut as1, 0xc19bf2749bdc06a7, 0x80deb1fe72be5d74);
    quad_round(&mut bs0, &mut bs1, 0xc19bf2749bdc06a7, 0x80deb1fe72be5d74);
    shift_message_b(&mut am2, am3, &mut am0);
    shift_message_b(&mut bm2, bm3, &mut bm0);
    quad_round_msg(&mut as0, &mut as1, am0, 0x240ca1cc0fc19dc6, 0xefbe4786e49b69c1);
    quad_round_msg(&mut bs0, &mut bs1, bm0, 0x240ca1cc0fc19dc6, 0xefbe4786e49b69c1);
    shift_message_b(&mut am3, am0, &mut am1);
    shift_message_b(&mut bm3, bm0, &mut bm1);
    quad_round_msg(&mut as0, &mut as1, am1, 0x76f988da5cb0a9dc, 0x4a7484aa2de92c6f);
    quad_round_msg(&mut bs0, &mut bs1, bm1, 0x76f988da5cb0a9dc, 0x4a7484aa2de92c6f);
    shift_message_b(&mut am0, am1, &mut am2);
    shift_message_b(&mut bm0, bm1, &mut bm2);
    quad_round_msg(&mut as0, &mut as1, am2, 0xbf597fc7b00327c8, 0xa831c66d983e5152);
    quad_round_msg(&mut bs0, &mut bs1, bm2, 0xbf597fc7b00327c8, 0xa831c66d983e5152);
    shift_message_b(&mut am1, am2, &mut am3);
    shift_message_b(&mut bm1, bm2, &mut bm3);
    quad_round_msg(&mut as0, &mut as1, am3, 0x1429296706ca6351, 0xd5a79147c6e00bf3);
    quad_round_msg(&mut bs0, &mut bs1, bm3, 0x1429296706ca6351, 0xd5a79147c6e00bf3);
    shift_message_b(&mut am2, am3, &mut am0);
    shift_message_b(&mut bm2, bm3, &mut bm0);
    quad_round_msg(&mut as0, &mut as1, am0, 0x53380d134d2c6dfc, 0x2e1b213827b70a85);
    quad_round_msg(&mut bs0, &mut bs1, bm0, 0x53380d134d2c6dfc, 0x2e1b213827b70a85);
    shift_message_b(&mut am3, am0, &mut am1);
    shift_message_b(&mut bm3, bm0, &mut bm1);
    quad_round_msg(&mut as0, &mut as1, am1, 0x92722c8581c2c92e, 0x766a0abb650a7354);
    quad_round_msg(&mut bs0, &mut bs1, bm1, 0x92722c8581c2c92e, 0x766a0abb650a7354);
    shift_message_b(&mut am0, am1, &mut am2);
    shift_message_b(&mut bm0, bm1, &mut bm2);
    quad_round_msg(&mut as0, &mut as1, am2, 0xc76c51a3c24b8b70, 0xa81a664ba2bfe8a1);
    quad_round_msg(&mut bs0, &mut bs1, bm2, 0xc76c51a3c24b8b70, 0xa81a664ba2bfe8a1);
    shift_message_b(&mut am1, am2, &mut am3);
    shift_message_b(&mut bm1, bm2, &mut bm3);
    quad_round_msg(&mut as0, &mut as1, am3, 0x106aa070f40e3585, 0xd6990624d192e819);
    quad_round_msg(&mut bs0, &mut bs1, bm3, 0x106aa070f40e3585, 0xd6990624d192e819);
    shift_message_b(&mut am2, am3, &mut am0);
    shift_message_b(&mut bm2, bm3, &mut bm0);
    quad_round_msg(&mut as0, &mut as1, am0, 0x34b0bcb52748774c, 0x1e376c0819a4c116);
    quad_round_msg(&mut bs0, &mut bs1, bm0, 0x34b0bcb52748774c, 0x1e376c0819a4c116);
    shift_message_b(&mut am3, am0, &mut am1);
    shift_message_b(&mut bm3, bm0, &mut bm1);
    quad_round_msg(&mut as0, &mut as1, am1, 0x682e6ff35b9cca4f, 0x4ed8aa4a391c0cb3);
    quad_round_msg(&mut bs0, &mut bs1, bm1, 0x682e6ff35b9cca4f, 0x4ed8aa4a391c0cb3);
    shift_message_c(am0, am1, &mut am2);
    shift_message_c(bm0, bm1, &mut bm2);
    quad_round_msg(&mut as0, &mut as1, am2, 0x8cc7020884c87814, 0x78a5636f748f82ee);
    quad_round_msg(&mut bs0, &mut bs1, bm2, 0x8cc7020884c87814, 0x78a5636f748f82ee);
    shift_message_c(am1, am2, &mut am3);
    shift_message_c(bm1, bm2, &mut bm3);
    quad_round_msg(&mut as0, &mut as1, am3, 0xc67178f2bef9a3f7, 0xa4506ceb90befffa);
    quad_round_msg(&mut bs0, &mut bs1, bm3, 0xc67178f2bef9a3f7, 0xa4506ceb90befffa);
    as0 = _mm_add_epi32(as0, init0());
    bs0 = _mm_add_epi32(bs0, init0());
    as1 = _mm_add_epi32(as1, init1());
    bs1 = _mm_add_epi32(bs1, init1());

    // Extract hash into out
    unshuffle(&mut as0, &mut as1);
    unshuffle(&mut bs0, &mut bs1);
    save(out, 0, as0);
    save(out, 16, as1);
    save(out, 32, bs0);
    save(out, 48, bs1);
}
