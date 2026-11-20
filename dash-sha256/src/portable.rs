// Copyright (c) 2014-2022 The Bitcoin Core developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or http://www.opensource.org/licenses/mit-license.php.

//! The portable code (Core: `sha256::Transform`, `sha256::TransformD64` and
//! `TransformD64Wrapper` in `crypto/sha256.cpp`).

#[cfg(any(
    all(target_arch = "x86_64", target_pointer_width = "64"),
    all(target_arch = "aarch64", target_endian = "little")
))]
use crate::dispatch::TransformFn;
use crate::macros::{add, block_rounds, inc, lane_fns, round, transform_d64};

type V = u32;

#[inline(always)]
fn k(x: u32) -> V {
    x
}

#[inline(always)]
fn add(x: V, y: V) -> V {
    x.wrapping_add(y)
}

#[inline(always)]
fn xor(x: V, y: V) -> V {
    x ^ y
}

#[inline(always)]
fn or(x: V, y: V) -> V {
    x | y
}

#[inline(always)]
fn and(x: V, y: V) -> V {
    x & y
}

#[inline(always)]
fn shr<const N: u32>(x: V) -> V {
    x >> N
}

#[inline(always)]
fn shl<const N: u32>(x: V) -> V {
    x << N
}

lane_fns!();

/// The big-endian word at `offset` (Core: `ReadBE32`).
#[inline(always)]
fn read(input: &[u8; 64], offset: usize) -> V {
    u32::from_be_bytes([input[offset], input[offset + 1], input[offset + 2], input[offset + 3]])
}

/// Core: `WriteBE32`.
#[inline(always)]
fn write(out: &mut [u8; 32], offset: usize, v: V) {
    out[offset..offset + 4].copy_from_slice(&v.to_be_bytes());
}

/// Core: `sha256::Transform`.
pub(crate) fn transform(s: &mut [u32; 8], blocks: &[[u8; 64]]) {
    for chunk in blocks {
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *s;
        block_rounds!(chunk; a, b, c, d, e, f, g, h);
        for (s, x) in s.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(x);
        }
    }
}

/// Core: `sha256::TransformD64`.
pub(crate) fn transform_d64(out: &mut [u8; 32], input: &[u8; 64]) {
    transform_d64!(input, out)
}

/// Double SHA-256 of one 64-byte input through a single-stream transform
/// (Core: `TransformD64Wrapper`).
///
/// # Safety
///
/// The CPU must support `transform`'s target features.
#[cfg(any(
    all(target_arch = "x86_64", target_pointer_width = "64"),
    all(target_arch = "aarch64", target_endian = "little")
))]
#[inline(always)]
pub(crate) unsafe fn d64_wrapper(transform: TransformFn, out: &mut [u8; 32], input: &[u8; 64]) {
    use crate::{INIT, write_state};
    // The padding of a 64-byte message: 0x80, then the length, 512 bits.
    const PADDING1: [u8; 64] = {
        let mut p = [0; 64];
        p[0] = 0x80;
        p[62] = 2;
        p
    };
    // The 32-byte first digest, then its padding and length, 256 bits.
    let mut buffer2 = [0; 64];
    buffer2[32] = 0x80;
    buffer2[62] = 1;

    let mut s = INIT;
    // SAFETY: the caller guarantees `transform`'s features.
    unsafe {
        transform(&mut s, core::slice::from_ref(input));
        transform(&mut s, core::slice::from_ref(&PADDING1));
    }
    write_state(buffer2.first_chunk_mut().unwrap(), s);
    s = INIT;
    // SAFETY: as above.
    unsafe { transform(&mut s, core::slice::from_ref(&buffer2)) };
    write_state(out, s);
}
