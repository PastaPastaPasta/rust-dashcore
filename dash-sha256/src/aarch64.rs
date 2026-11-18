// Copyright (c) 2022 The Bitcoin Core developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or http://www.opensource.org/licenses/mit-license.php.
//
// Based on https://github.com/noloader/SHA-Intrinsics/blob/master/sha256-arm.c,
// Written and placed in public domain by Jeffrey Walton.
// Based on code from ARM, and by Johannes Schneiders, Skip Hovsmith and
// Barry O'Rourke for the mbedTLS project.
// Variant specialized for 64-byte inputs added by Pieter Wuille.

//! The ARMv8 SHA2 transforms (Core: `sha256_arm_shani::Transform` and
//! `sha256d64_arm_shani::Transform_2way`, `crypto/sha256_arm_shani.cpp`).

use core::arch::aarch64::*;

use crate::portable;

/// The round constants.
static K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Initial state.
static INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Precomputed message schedule for the 2nd transform.
static MIDS: [u32; 64] = [
    0xc28a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf374,
    0x649b69c1, 0xf0fe4786, 0x0fe1edc6, 0x240cf254, 0x4fe9346f, 0x6cc984be, 0x61b9411e, 0x16f988fa,
    0xf2c65152, 0xa88e5a6d, 0xb019fc65, 0xb9d99ec7, 0x9a1231c3, 0xe70eeaa0, 0xfdb1232b, 0xc7353eb0,
    0x3069bad5, 0xcb976d5f, 0x5a0f118f, 0xdc1eeefd, 0x0a35b689, 0xde0b7a04, 0x58f4ca9d, 0xe15d5b16,
    0x007f3e86, 0x37088980, 0xa507ea32, 0x6fab9537, 0x17406110, 0x0d8cd6f1, 0xcdaa3b6d, 0xc0bbbe37,
    0x83613bda, 0xdb48a363, 0x0b02e931, 0x6fd15ca7, 0x521afaca, 0x31338431, 0x6ed41a95, 0x6d437890,
    0xc39c91f2, 0x9eccabbd, 0xb5c9a0e6, 0x532fb63c, 0xd2c741c6, 0x07237ea3, 0xa4954b68, 0x4c191d76,
];

/// A few precomputed message schedule values for the 3rd transform.
static FINS: [u32; 12] = [
    0x5807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x80000000, 0x00000000, 0x00000000, 0x00000000,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf274,
];

/// Padding processed in the 3rd transform (byteswapped).
static FINAL: [u32; 8] = [0x80000000, 0, 0, 0, 0, 0, 0, 0x100];

/// The four words of `table` at `index` (Core: `vld1q_u32(&table[index])`).
#[target_feature(enable = "neon,sha2")]
#[inline]
fn ld(table: &[u32], index: usize) -> uint32x4_t {
    let words = &table[index..index + 4];
    // SAFETY: `words` is 4 words.
    unsafe { vld1q_u32(words.as_ptr()) }
}

/// The 16 bytes at `offset`, as big-endian words.
#[target_feature(enable = "neon,sha2")]
#[inline]
fn load_be(input: &[u8], offset: usize) -> uint32x4_t {
    let bytes = &input[offset..offset + 16];
    // SAFETY: `bytes` is 16 bytes.
    vreinterpretq_u32_u8(vrev32q_u8(unsafe { vld1q_u8(bytes.as_ptr()) }))
}

/// Stores `v` as big-endian words at `offset`.
#[target_feature(enable = "neon,sha2")]
#[inline]
fn store_be(output: &mut [u8], offset: usize, v: uint32x4_t) {
    let bytes = &mut output[offset..offset + 16];
    // SAFETY: `bytes` is 16 bytes.
    unsafe { vst1q_u8(bytes.as_mut_ptr(), vrev32q_u8(vreinterpretq_u8_u32(v))) }
}

/// Core: `sha256_arm_shani::Transform`.
#[target_feature(enable = "neon,sha2")]
pub(crate) fn transform(s: &mut [u32; 8], blocks: &[[u8; 64]]) {
    // Load state
    let mut state0 = ld(s, 0);
    let mut state1 = ld(s, 4);

    for chunk in blocks {
        // Save state
        let abef_save = state0;
        let cdgh_save = state1;

        // Load and convert input chunk to Big Endian
        let mut msg0 = load_be(chunk, 0);
        let mut msg1 = load_be(chunk, 16);
        let mut msg2 = load_be(chunk, 32);
        let mut msg3 = load_be(chunk, 48);

        // Original implementation preloaded message and constant addition which was 1-3% slower.
        // Now included as first step in quad round code saving one Q Neon register
        // "tmp0 = vaddq_u32(msg0, ld(&K, 0));"

        // Rounds 1-4
        let mut tmp0 = vaddq_u32(msg0, ld(&K, 0));
        let mut tmp2 = state0;
        msg0 = vsha256su0q_u32(msg0, msg1);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg0 = vsha256su1q_u32(msg0, msg2, msg3);

        // Rounds 5-8
        tmp0 = vaddq_u32(msg1, ld(&K, 4));
        tmp2 = state0;
        msg1 = vsha256su0q_u32(msg1, msg2);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg1 = vsha256su1q_u32(msg1, msg3, msg0);

        // Rounds 9-12
        tmp0 = vaddq_u32(msg2, ld(&K, 8));
        tmp2 = state0;
        msg2 = vsha256su0q_u32(msg2, msg3);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg2 = vsha256su1q_u32(msg2, msg0, msg1);

        // Rounds 13-16
        tmp0 = vaddq_u32(msg3, ld(&K, 12));
        tmp2 = state0;
        msg3 = vsha256su0q_u32(msg3, msg0);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg3 = vsha256su1q_u32(msg3, msg1, msg2);

        // Rounds 17-20
        tmp0 = vaddq_u32(msg0, ld(&K, 16));
        tmp2 = state0;
        msg0 = vsha256su0q_u32(msg0, msg1);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg0 = vsha256su1q_u32(msg0, msg2, msg3);

        // Rounds 21-24
        tmp0 = vaddq_u32(msg1, ld(&K, 20));
        tmp2 = state0;
        msg1 = vsha256su0q_u32(msg1, msg2);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg1 = vsha256su1q_u32(msg1, msg3, msg0);

        // Rounds 25-28
        tmp0 = vaddq_u32(msg2, ld(&K, 24));
        tmp2 = state0;
        msg2 = vsha256su0q_u32(msg2, msg3);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg2 = vsha256su1q_u32(msg2, msg0, msg1);

        // Rounds 29-32
        tmp0 = vaddq_u32(msg3, ld(&K, 28));
        tmp2 = state0;
        msg3 = vsha256su0q_u32(msg3, msg0);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg3 = vsha256su1q_u32(msg3, msg1, msg2);

        // Rounds 33-36
        tmp0 = vaddq_u32(msg0, ld(&K, 32));
        tmp2 = state0;
        msg0 = vsha256su0q_u32(msg0, msg1);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg0 = vsha256su1q_u32(msg0, msg2, msg3);

        // Rounds 37-40
        tmp0 = vaddq_u32(msg1, ld(&K, 36));
        tmp2 = state0;
        msg1 = vsha256su0q_u32(msg1, msg2);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg1 = vsha256su1q_u32(msg1, msg3, msg0);

        // Rounds 41-44
        tmp0 = vaddq_u32(msg2, ld(&K, 40));
        tmp2 = state0;
        msg2 = vsha256su0q_u32(msg2, msg3);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg2 = vsha256su1q_u32(msg2, msg0, msg1);

        // Rounds 45-48
        tmp0 = vaddq_u32(msg3, ld(&K, 44));
        tmp2 = state0;
        msg3 = vsha256su0q_u32(msg3, msg0);
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);
        msg3 = vsha256su1q_u32(msg3, msg1, msg2);

        // Rounds 49-52
        tmp0 = vaddq_u32(msg0, ld(&K, 48));
        tmp2 = state0;
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);

        // Rounds 53-56
        tmp0 = vaddq_u32(msg1, ld(&K, 52));
        tmp2 = state0;
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);

        // Rounds 57-60
        tmp0 = vaddq_u32(msg2, ld(&K, 56));
        tmp2 = state0;
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);

        // Rounds 61-64
        tmp0 = vaddq_u32(msg3, ld(&K, 60));
        tmp2 = state0;
        state0 = vsha256hq_u32(state0, state1, tmp0);
        state1 = vsha256h2q_u32(state1, tmp2, tmp0);

        // Update state
        state0 = vaddq_u32(state0, abef_save);
        state1 = vaddq_u32(state1, cdgh_save);
    }

    // Save final state
    // SAFETY: `s` is 8 words.
    unsafe {
        vst1q_u32(s.as_mut_ptr(), state0);
        vst1q_u32(s.as_mut_ptr().add(4), state1);
    }
}

/// Core: `TransformD64Wrapper<sha256_arm_shani::Transform>`.
#[target_feature(enable = "neon,sha2")]
pub(crate) fn transform_d64(out: &mut [u8; 32], input: &[u8; 64]) {
    // SAFETY: this function has `transform`'s features.
    unsafe { portable::d64_wrapper(transform, out, input) }
}

/// Core: `sha256d64_arm_shani::Transform_2way`.
#[target_feature(enable = "neon,sha2")]
pub(crate) fn transform_d64_2way(output: &mut [[u8; 32]; 2], input: &[[u8; 64]; 2]) {
    let input = input.as_flattened();
    let output = output.as_flattened_mut();

    // Transform 1: Load state
    let mut state0a = ld(&INIT, 0);
    let mut state0b = state0a;
    let mut state1a = ld(&INIT, 4);
    let mut state1b = state1a;

    // Transform 1: Load and convert input chunk to Big Endian
    let mut msg0a = load_be(input, 0);
    let mut msg1a = load_be(input, 16);
    let mut msg2a = load_be(input, 32);
    let mut msg3a = load_be(input, 48);
    let mut msg0b = load_be(input, 64);
    let mut msg1b = load_be(input, 80);
    let mut msg2b = load_be(input, 96);
    let mut msg3b = load_be(input, 112);

    // Transform 1: Rounds 1-4
    let mut tmp = ld(&K, 0);
    let mut tmp0a = vaddq_u32(msg0a, tmp);
    let mut tmp0b = vaddq_u32(msg0b, tmp);
    let mut tmp2a = state0a;
    let mut tmp2b = state0b;
    msg0a = vsha256su0q_u32(msg0a, msg1a);
    msg0b = vsha256su0q_u32(msg0b, msg1b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg0a = vsha256su1q_u32(msg0a, msg2a, msg3a);
    msg0b = vsha256su1q_u32(msg0b, msg2b, msg3b);

    // Transform 1: Rounds 5-8
    tmp = ld(&K, 4);
    tmp0a = vaddq_u32(msg1a, tmp);
    tmp0b = vaddq_u32(msg1b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg1a = vsha256su0q_u32(msg1a, msg2a);
    msg1b = vsha256su0q_u32(msg1b, msg2b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg1a = vsha256su1q_u32(msg1a, msg3a, msg0a);
    msg1b = vsha256su1q_u32(msg1b, msg3b, msg0b);

    // Transform 1: Rounds 9-12
    tmp = ld(&K, 8);
    tmp0a = vaddq_u32(msg2a, tmp);
    tmp0b = vaddq_u32(msg2b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg2a = vsha256su0q_u32(msg2a, msg3a);
    msg2b = vsha256su0q_u32(msg2b, msg3b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg2a = vsha256su1q_u32(msg2a, msg0a, msg1a);
    msg2b = vsha256su1q_u32(msg2b, msg0b, msg1b);

    // Transform 1: Rounds 13-16
    tmp = ld(&K, 12);
    tmp0a = vaddq_u32(msg3a, tmp);
    tmp0b = vaddq_u32(msg3b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg3a = vsha256su0q_u32(msg3a, msg0a);
    msg3b = vsha256su0q_u32(msg3b, msg0b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg3a = vsha256su1q_u32(msg3a, msg1a, msg2a);
    msg3b = vsha256su1q_u32(msg3b, msg1b, msg2b);

    // Transform 1: Rounds 17-20
    tmp = ld(&K, 16);
    tmp0a = vaddq_u32(msg0a, tmp);
    tmp0b = vaddq_u32(msg0b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg0a = vsha256su0q_u32(msg0a, msg1a);
    msg0b = vsha256su0q_u32(msg0b, msg1b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg0a = vsha256su1q_u32(msg0a, msg2a, msg3a);
    msg0b = vsha256su1q_u32(msg0b, msg2b, msg3b);

    // Transform 1: Rounds 21-24
    tmp = ld(&K, 20);
    tmp0a = vaddq_u32(msg1a, tmp);
    tmp0b = vaddq_u32(msg1b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg1a = vsha256su0q_u32(msg1a, msg2a);
    msg1b = vsha256su0q_u32(msg1b, msg2b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg1a = vsha256su1q_u32(msg1a, msg3a, msg0a);
    msg1b = vsha256su1q_u32(msg1b, msg3b, msg0b);

    // Transform 1: Rounds 25-28
    tmp = ld(&K, 24);
    tmp0a = vaddq_u32(msg2a, tmp);
    tmp0b = vaddq_u32(msg2b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg2a = vsha256su0q_u32(msg2a, msg3a);
    msg2b = vsha256su0q_u32(msg2b, msg3b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg2a = vsha256su1q_u32(msg2a, msg0a, msg1a);
    msg2b = vsha256su1q_u32(msg2b, msg0b, msg1b);

    // Transform 1: Rounds 29-32
    tmp = ld(&K, 28);
    tmp0a = vaddq_u32(msg3a, tmp);
    tmp0b = vaddq_u32(msg3b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg3a = vsha256su0q_u32(msg3a, msg0a);
    msg3b = vsha256su0q_u32(msg3b, msg0b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg3a = vsha256su1q_u32(msg3a, msg1a, msg2a);
    msg3b = vsha256su1q_u32(msg3b, msg1b, msg2b);

    // Transform 1: Rounds 33-36
    tmp = ld(&K, 32);
    tmp0a = vaddq_u32(msg0a, tmp);
    tmp0b = vaddq_u32(msg0b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg0a = vsha256su0q_u32(msg0a, msg1a);
    msg0b = vsha256su0q_u32(msg0b, msg1b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg0a = vsha256su1q_u32(msg0a, msg2a, msg3a);
    msg0b = vsha256su1q_u32(msg0b, msg2b, msg3b);

    // Transform 1: Rounds 37-40
    tmp = ld(&K, 36);
    tmp0a = vaddq_u32(msg1a, tmp);
    tmp0b = vaddq_u32(msg1b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg1a = vsha256su0q_u32(msg1a, msg2a);
    msg1b = vsha256su0q_u32(msg1b, msg2b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg1a = vsha256su1q_u32(msg1a, msg3a, msg0a);
    msg1b = vsha256su1q_u32(msg1b, msg3b, msg0b);

    // Transform 1: Rounds 41-44
    tmp = ld(&K, 40);
    tmp0a = vaddq_u32(msg2a, tmp);
    tmp0b = vaddq_u32(msg2b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg2a = vsha256su0q_u32(msg2a, msg3a);
    msg2b = vsha256su0q_u32(msg2b, msg3b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg2a = vsha256su1q_u32(msg2a, msg0a, msg1a);
    msg2b = vsha256su1q_u32(msg2b, msg0b, msg1b);

    // Transform 1: Rounds 45-48
    tmp = ld(&K, 44);
    tmp0a = vaddq_u32(msg3a, tmp);
    tmp0b = vaddq_u32(msg3b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg3a = vsha256su0q_u32(msg3a, msg0a);
    msg3b = vsha256su0q_u32(msg3b, msg0b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg3a = vsha256su1q_u32(msg3a, msg1a, msg2a);
    msg3b = vsha256su1q_u32(msg3b, msg1b, msg2b);

    // Transform 1: Rounds 49-52
    tmp = ld(&K, 48);
    tmp0a = vaddq_u32(msg0a, tmp);
    tmp0b = vaddq_u32(msg0b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);

    // Transform 1: Rounds 53-56
    tmp = ld(&K, 52);
    tmp0a = vaddq_u32(msg1a, tmp);
    tmp0b = vaddq_u32(msg1b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);

    // Transform 1: Rounds 57-60
    tmp = ld(&K, 56);
    tmp0a = vaddq_u32(msg2a, tmp);
    tmp0b = vaddq_u32(msg2b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);

    // Transform 1: Rounds 61-64
    tmp = ld(&K, 60);
    tmp0a = vaddq_u32(msg3a, tmp);
    tmp0b = vaddq_u32(msg3b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);

    // Transform 1: Update state
    tmp = ld(&INIT, 0);
    state0a = vaddq_u32(state0a, tmp);
    state0b = vaddq_u32(state0b, tmp);
    tmp = ld(&INIT, 4);
    state1a = vaddq_u32(state1a, tmp);
    state1b = vaddq_u32(state1b, tmp);

    // Transform 2: Save state
    let abef_savea = state0a;
    let abef_saveb = state0b;
    let cdgh_savea = state1a;
    let cdgh_saveb = state1b;

    // Transform 2: Rounds 1-4
    tmp = ld(&MIDS, 0);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 5-8
    tmp = ld(&MIDS, 4);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 9-12
    tmp = ld(&MIDS, 8);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 13-16
    tmp = ld(&MIDS, 12);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 17-20
    tmp = ld(&MIDS, 16);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 21-24
    tmp = ld(&MIDS, 20);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 25-28
    tmp = ld(&MIDS, 24);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 29-32
    tmp = ld(&MIDS, 28);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 33-36
    tmp = ld(&MIDS, 32);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 37-40
    tmp = ld(&MIDS, 36);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 41-44
    tmp = ld(&MIDS, 40);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 45-48
    tmp = ld(&MIDS, 44);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 49-52
    tmp = ld(&MIDS, 48);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 53-56
    tmp = ld(&MIDS, 52);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 57-60
    tmp = ld(&MIDS, 56);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Rounds 61-64
    tmp = ld(&MIDS, 60);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);

    // Transform 2: Update state
    state0a = vaddq_u32(state0a, abef_savea);
    state0b = vaddq_u32(state0b, abef_saveb);
    state1a = vaddq_u32(state1a, cdgh_savea);
    state1b = vaddq_u32(state1b, cdgh_saveb);

    // Transform 3: Pad previous output
    msg0a = state0a;
    msg0b = state0b;
    msg1a = state1a;
    msg1b = state1b;
    msg2a = ld(&FINAL, 0);
    msg2b = msg2a;
    msg3a = ld(&FINAL, 4);
    msg3b = msg3a;

    // Transform 3: Load state
    state0a = ld(&INIT, 0);
    state0b = state0a;
    state1a = ld(&INIT, 4);
    state1b = state1a;

    // Transform 3: Rounds 1-4
    tmp = ld(&K, 0);
    tmp0a = vaddq_u32(msg0a, tmp);
    tmp0b = vaddq_u32(msg0b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg0a = vsha256su0q_u32(msg0a, msg1a);
    msg0b = vsha256su0q_u32(msg0b, msg1b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg0a = vsha256su1q_u32(msg0a, msg2a, msg3a);
    msg0b = vsha256su1q_u32(msg0b, msg2b, msg3b);

    // Transform 3: Rounds 5-8
    tmp = ld(&K, 4);
    tmp0a = vaddq_u32(msg1a, tmp);
    tmp0b = vaddq_u32(msg1b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg1a = vsha256su0q_u32(msg1a, msg2a);
    msg1b = vsha256su0q_u32(msg1b, msg2b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg1a = vsha256su1q_u32(msg1a, msg3a, msg0a);
    msg1b = vsha256su1q_u32(msg1b, msg3b, msg0b);

    // Transform 3: Rounds 9-12
    tmp = ld(&FINS, 0);
    tmp2a = state0a;
    tmp2b = state0b;
    msg2a = ld(&FINS, 4);
    msg2b = msg2a;
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);
    msg2a = vsha256su1q_u32(msg2a, msg0a, msg1a);
    msg2b = vsha256su1q_u32(msg2b, msg0b, msg1b);

    // Transform 3: Rounds 13-16
    tmp = ld(&FINS, 8);
    tmp2a = state0a;
    tmp2b = state0b;
    msg3a = vsha256su0q_u32(msg3a, msg0a);
    msg3b = vsha256su0q_u32(msg3b, msg0b);
    state0a = vsha256hq_u32(state0a, state1a, tmp);
    state0b = vsha256hq_u32(state0b, state1b, tmp);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp);
    msg3a = vsha256su1q_u32(msg3a, msg1a, msg2a);
    msg3b = vsha256su1q_u32(msg3b, msg1b, msg2b);

    // Transform 3: Rounds 17-20
    tmp = ld(&K, 16);
    tmp0a = vaddq_u32(msg0a, tmp);
    tmp0b = vaddq_u32(msg0b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg0a = vsha256su0q_u32(msg0a, msg1a);
    msg0b = vsha256su0q_u32(msg0b, msg1b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg0a = vsha256su1q_u32(msg0a, msg2a, msg3a);
    msg0b = vsha256su1q_u32(msg0b, msg2b, msg3b);

    // Transform 3: Rounds 21-24
    tmp = ld(&K, 20);
    tmp0a = vaddq_u32(msg1a, tmp);
    tmp0b = vaddq_u32(msg1b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg1a = vsha256su0q_u32(msg1a, msg2a);
    msg1b = vsha256su0q_u32(msg1b, msg2b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg1a = vsha256su1q_u32(msg1a, msg3a, msg0a);
    msg1b = vsha256su1q_u32(msg1b, msg3b, msg0b);

    // Transform 3: Rounds 25-28
    tmp = ld(&K, 24);
    tmp0a = vaddq_u32(msg2a, tmp);
    tmp0b = vaddq_u32(msg2b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg2a = vsha256su0q_u32(msg2a, msg3a);
    msg2b = vsha256su0q_u32(msg2b, msg3b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg2a = vsha256su1q_u32(msg2a, msg0a, msg1a);
    msg2b = vsha256su1q_u32(msg2b, msg0b, msg1b);

    // Transform 3: Rounds 29-32
    tmp = ld(&K, 28);
    tmp0a = vaddq_u32(msg3a, tmp);
    tmp0b = vaddq_u32(msg3b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg3a = vsha256su0q_u32(msg3a, msg0a);
    msg3b = vsha256su0q_u32(msg3b, msg0b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg3a = vsha256su1q_u32(msg3a, msg1a, msg2a);
    msg3b = vsha256su1q_u32(msg3b, msg1b, msg2b);

    // Transform 3: Rounds 33-36
    tmp = ld(&K, 32);
    tmp0a = vaddq_u32(msg0a, tmp);
    tmp0b = vaddq_u32(msg0b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg0a = vsha256su0q_u32(msg0a, msg1a);
    msg0b = vsha256su0q_u32(msg0b, msg1b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg0a = vsha256su1q_u32(msg0a, msg2a, msg3a);
    msg0b = vsha256su1q_u32(msg0b, msg2b, msg3b);

    // Transform 3: Rounds 37-40
    tmp = ld(&K, 36);
    tmp0a = vaddq_u32(msg1a, tmp);
    tmp0b = vaddq_u32(msg1b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg1a = vsha256su0q_u32(msg1a, msg2a);
    msg1b = vsha256su0q_u32(msg1b, msg2b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg1a = vsha256su1q_u32(msg1a, msg3a, msg0a);
    msg1b = vsha256su1q_u32(msg1b, msg3b, msg0b);

    // Transform 3: Rounds 41-44
    tmp = ld(&K, 40);
    tmp0a = vaddq_u32(msg2a, tmp);
    tmp0b = vaddq_u32(msg2b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg2a = vsha256su0q_u32(msg2a, msg3a);
    msg2b = vsha256su0q_u32(msg2b, msg3b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg2a = vsha256su1q_u32(msg2a, msg0a, msg1a);
    msg2b = vsha256su1q_u32(msg2b, msg0b, msg1b);

    // Transform 3: Rounds 45-48
    tmp = ld(&K, 44);
    tmp0a = vaddq_u32(msg3a, tmp);
    tmp0b = vaddq_u32(msg3b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    msg3a = vsha256su0q_u32(msg3a, msg0a);
    msg3b = vsha256su0q_u32(msg3b, msg0b);
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);
    msg3a = vsha256su1q_u32(msg3a, msg1a, msg2a);
    msg3b = vsha256su1q_u32(msg3b, msg1b, msg2b);

    // Transform 3: Rounds 49-52
    tmp = ld(&K, 48);
    tmp0a = vaddq_u32(msg0a, tmp);
    tmp0b = vaddq_u32(msg0b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);

    // Transform 3: Rounds 53-56
    tmp = ld(&K, 52);
    tmp0a = vaddq_u32(msg1a, tmp);
    tmp0b = vaddq_u32(msg1b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);

    // Transform 3: Rounds 57-60
    tmp = ld(&K, 56);
    tmp0a = vaddq_u32(msg2a, tmp);
    tmp0b = vaddq_u32(msg2b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);

    // Transform 3: Rounds 61-64
    tmp = ld(&K, 60);
    tmp0a = vaddq_u32(msg3a, tmp);
    tmp0b = vaddq_u32(msg3b, tmp);
    tmp2a = state0a;
    tmp2b = state0b;
    state0a = vsha256hq_u32(state0a, state1a, tmp0a);
    state0b = vsha256hq_u32(state0b, state1b, tmp0b);
    state1a = vsha256h2q_u32(state1a, tmp2a, tmp0a);
    state1b = vsha256h2q_u32(state1b, tmp2b, tmp0b);

    // Transform 3: Update state
    tmp = ld(&INIT, 0);
    state0a = vaddq_u32(state0a, tmp);
    state0b = vaddq_u32(state0b, tmp);
    tmp = ld(&INIT, 4);
    state1a = vaddq_u32(state1a, tmp);
    state1b = vaddq_u32(state1b, tmp);

    // Store result
    store_be(output, 0, state0a);
    store_be(output, 16, state1a);
    store_be(output, 32, state0b);
    store_be(output, 48, state1b);
}
