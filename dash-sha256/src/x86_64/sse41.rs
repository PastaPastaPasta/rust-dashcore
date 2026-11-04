// Copyright (c) 2018-2019 The Bitcoin Core developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or http://www.opensource.org/licenses/mit-license.php.

//! The SSE4.1 4-way double SHA-256 of 64-byte inputs (Core: `sha256d64_sse41::Transform_4way`,
//! `crypto/sha256_sse41.cpp`).

use core::arch::x86_64::*;

use super::le32;
use crate::macros::{add, inc, lane_fns, round, transform_d64};

type V = __m128i;

#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn k(x: u32) -> V {
    _mm_set1_epi32(x as i32)
}

#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn add(x: V, y: V) -> V {
    _mm_add_epi32(x, y)
}

#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn xor(x: V, y: V) -> V {
    _mm_xor_si128(x, y)
}

#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn or(x: V, y: V) -> V {
    _mm_or_si128(x, y)
}

#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn and(x: V, y: V) -> V {
    _mm_and_si128(x, y)
}

#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn shr<const N: i32>(x: V) -> V {
    _mm_srli_epi32::<N>(x)
}

#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn shl<const N: i32>(x: V) -> V {
    _mm_slli_epi32::<N>(x)
}

lane_fns!(#[target_feature(enable = "sse4.1,ssse3")]);

/// Byte-swaps each 32-bit lane.
#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn bswap(v: V) -> V {
    _mm_shuffle_epi8(v, _mm_set_epi32(0x0C0D0E0F, 0x08090A0B, 0x04050607, 0x00010203))
}

/// The big-endian word at `offset` of each input; lane 3 holds input 0 (Core: `Read4`).
#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn read(input: &[[u8; 64]; 4], offset: usize) -> V {
    bswap(_mm_set_epi32(
        le32(&input[0], offset),
        le32(&input[1], offset),
        le32(&input[2], offset),
        le32(&input[3], offset),
    ))
}

/// Writes each lane's word big-endian at `offset` of its output (Core: `Write4`).
#[target_feature(enable = "sse4.1,ssse3")]
#[inline]
fn write(out: &mut [[u8; 32]; 4], offset: usize, v: V) {
    let v = bswap(v);
    out[0][offset..offset + 4].copy_from_slice(&_mm_extract_epi32::<3>(v).to_le_bytes());
    out[1][offset..offset + 4].copy_from_slice(&_mm_extract_epi32::<2>(v).to_le_bytes());
    out[2][offset..offset + 4].copy_from_slice(&_mm_extract_epi32::<1>(v).to_le_bytes());
    out[3][offset..offset + 4].copy_from_slice(&_mm_extract_epi32::<0>(v).to_le_bytes());
}

/// Core: `sha256d64_sse41::Transform_4way`.
#[target_feature(enable = "sse4.1,ssse3")]
pub(crate) fn transform_d64_4way(out: &mut [[u8; 32]; 4], input: &[[u8; 64]; 4]) {
    transform_d64!(input, out)
}
