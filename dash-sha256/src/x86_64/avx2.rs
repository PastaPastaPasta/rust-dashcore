// Copyright (c) 2017-2019 The Bitcoin Core developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or http://www.opensource.org/licenses/mit-license.php.

//! The AVX2 8-way double SHA-256 of 64-byte inputs (Core: `sha256d64_avx2::Transform_8way`,
//! `crypto/sha256_avx2.cpp`).

use core::arch::x86_64::*;

use super::le32;
use crate::macros::{add, inc, lane_fns, round, transform_d64};

type V = __m256i;

#[target_feature(enable = "avx2")]
#[inline]
fn k(x: u32) -> V {
    _mm256_set1_epi32(x as i32)
}

#[target_feature(enable = "avx2")]
#[inline]
fn add(x: V, y: V) -> V {
    _mm256_add_epi32(x, y)
}

#[target_feature(enable = "avx2")]
#[inline]
fn xor(x: V, y: V) -> V {
    _mm256_xor_si256(x, y)
}

#[target_feature(enable = "avx2")]
#[inline]
fn or(x: V, y: V) -> V {
    _mm256_or_si256(x, y)
}

#[target_feature(enable = "avx2")]
#[inline]
fn and(x: V, y: V) -> V {
    _mm256_and_si256(x, y)
}

#[target_feature(enable = "avx2")]
#[inline]
fn shr<const N: i32>(x: V) -> V {
    _mm256_srli_epi32::<N>(x)
}

#[target_feature(enable = "avx2")]
#[inline]
fn shl<const N: i32>(x: V) -> V {
    _mm256_slli_epi32::<N>(x)
}

lane_fns!(#[target_feature(enable = "avx2")]);

/// Byte-swaps each 32-bit lane.
#[target_feature(enable = "avx2")]
#[inline]
fn bswap(v: V) -> V {
    _mm256_shuffle_epi8(
        v,
        _mm256_set_epi32(
            0x0C0D0E0F, 0x08090A0B, 0x04050607, 0x00010203, 0x0C0D0E0F, 0x08090A0B, 0x04050607,
            0x00010203,
        ),
    )
}

/// The big-endian word at `offset` of each input; lane 7 holds input 0 (Core: `Read8`).
#[target_feature(enable = "avx2")]
#[inline]
fn read(input: &[[u8; 64]; 8], offset: usize) -> V {
    bswap(_mm256_set_epi32(
        le32(&input[0], offset),
        le32(&input[1], offset),
        le32(&input[2], offset),
        le32(&input[3], offset),
        le32(&input[4], offset),
        le32(&input[5], offset),
        le32(&input[6], offset),
        le32(&input[7], offset),
    ))
}

/// Writes each lane's word big-endian at `offset` of its output (Core: `Write8`).
#[target_feature(enable = "avx2")]
#[inline]
fn write(out: &mut [[u8; 32]; 8], offset: usize, v: V) {
    let v = bswap(v);
    out[0][offset..offset + 4].copy_from_slice(&_mm256_extract_epi32::<7>(v).to_le_bytes());
    out[1][offset..offset + 4].copy_from_slice(&_mm256_extract_epi32::<6>(v).to_le_bytes());
    out[2][offset..offset + 4].copy_from_slice(&_mm256_extract_epi32::<5>(v).to_le_bytes());
    out[3][offset..offset + 4].copy_from_slice(&_mm256_extract_epi32::<4>(v).to_le_bytes());
    out[4][offset..offset + 4].copy_from_slice(&_mm256_extract_epi32::<3>(v).to_le_bytes());
    out[5][offset..offset + 4].copy_from_slice(&_mm256_extract_epi32::<2>(v).to_le_bytes());
    out[6][offset..offset + 4].copy_from_slice(&_mm256_extract_epi32::<1>(v).to_le_bytes());
    out[7][offset..offset + 4].copy_from_slice(&_mm256_extract_epi32::<0>(v).to_le_bytes());
}

/// Core: `sha256d64_avx2::Transform_8way`.
#[target_feature(enable = "avx2")]
pub(crate) fn transform_d64_8way(out: &mut [[u8; 32]; 8], input: &[[u8; 64]; 8]) {
    transform_d64!(input, out)
}
