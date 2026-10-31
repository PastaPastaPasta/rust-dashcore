//! BMW-512 (Core: `crypto/x11/bmw.c`, `sph_bmw512_init`/`sph_bmw512`/`sph_bmw512_close`).
//!
//! Specialised to one 64-byte message: `bmw64_close` pads it to a single block (`0x80`, zeros,
//! the 512-bit length in the last little-endian word), compresses it from `IV512`, then
//! compresses the result as a message under `final_b` and keeps the upper eight words.
//! `compress_big` is sph's non-small-footprint form.

use core::num::Wrapping;

type W = Wrapping<u64>;

/// `IV512`.
#[rustfmt::skip]
const IV: [u64; 16] = [
    0x8081_8283_8485_8687, 0x8889_8A8B_8C8D_8E8F,
    0x9091_9293_9495_9697, 0x9899_9A9B_9C9D_9E9F,
    0xA0A1_A2A3_A4A5_A6A7, 0xA8A9_AAAB_ACAD_AEAF,
    0xB0B1_B2B3_B4B5_B6B7, 0xB8B9_BABB_BCBD_BEBF,
    0xC0C1_C2C3_C4C5_C6C7, 0xC8C9_CACB_CCCD_CECF,
    0xD0D1_D2D3_D4D5_D6D7, 0xD8D9_DADB_DCDD_DEDF,
    0xE0E1_E2E3_E4E5_E6E7, 0xE8E9_EAEB_ECED_EEEF,
    0xF0F1_F2F3_F4F5_F6F7, 0xF8F9_FAFB_FCFD_FEFF,
];

/// `final_b`.
#[rustfmt::skip]
const FINAL: [u64; 16] = [
    0xAAAA_AAAA_AAAA_AAA0, 0xAAAA_AAAA_AAAA_AAA1,
    0xAAAA_AAAA_AAAA_AAA2, 0xAAAA_AAAA_AAAA_AAA3,
    0xAAAA_AAAA_AAAA_AAA4, 0xAAAA_AAAA_AAAA_AAA5,
    0xAAAA_AAAA_AAAA_AAA6, 0xAAAA_AAAA_AAAA_AAA7,
    0xAAAA_AAAA_AAAA_AAA8, 0xAAAA_AAAA_AAAA_AAA9,
    0xAAAA_AAAA_AAAA_AAAA, 0xAAAA_AAAA_AAAA_AAAB,
    0xAAAA_AAAA_AAAA_AAAC, 0xAAAA_AAAA_AAAA_AAAD,
    0xAAAA_AAAA_AAAA_AAAE, 0xAAAA_AAAA_AAAA_AAAF,
];

#[inline(always)]
fn rotl(x: W, n: u32) -> W {
    Wrapping(x.0.rotate_left(n))
}

#[inline(always)]
fn sb0(x: W) -> W {
    (x >> 1) ^ (x << 3) ^ rotl(x, 4) ^ rotl(x, 37)
}

#[inline(always)]
fn sb1(x: W) -> W {
    (x >> 1) ^ (x << 2) ^ rotl(x, 13) ^ rotl(x, 43)
}

#[inline(always)]
fn sb2(x: W) -> W {
    (x >> 2) ^ (x << 1) ^ rotl(x, 19) ^ rotl(x, 53)
}

#[inline(always)]
fn sb3(x: W) -> W {
    (x >> 2) ^ (x << 2) ^ rotl(x, 28) ^ rotl(x, 59)
}

#[inline(always)]
fn sb4(x: W) -> W {
    (x >> 1) ^ x
}

#[inline(always)]
fn sb5(x: W) -> W {
    (x >> 2) ^ x
}

#[inline(always)]
fn rb1(x: W) -> W {
    rotl(x, 5)
}

#[inline(always)]
fn rb2(x: W) -> W {
    rotl(x, 11)
}

#[inline(always)]
fn rb3(x: W) -> W {
    rotl(x, 27)
}

#[inline(always)]
fn rb4(x: W) -> W {
    rotl(x, 32)
}

#[inline(always)]
fn rb5(x: W) -> W {
    rotl(x, 37)
}

#[inline(always)]
fn rb6(x: W) -> W {
    rotl(x, 43)
}

#[inline(always)]
fn rb7(x: W) -> W {
    rotl(x, 53)
}

/// `add_elt_b` for `j = i - 16`: `M16_i` is `j, j+1, j+3, j+4, j+7, j+10, j+11` (mod 16, with
/// the rotation counts one more than the word index).
macro_rules! add_elt {
    ($m:ident, $h:ident, $i:literal) => {{
        const J: usize = $i - 16;
        const J3: usize = (J + 3) % 16;
        const J10: usize = (J + 10) % 16;
        (rotl($m[J], J as u32 + 1) + rotl($m[J3], J3 as u32 + 1) - rotl($m[J10], J10 as u32 + 1)
            + Wrapping(($i as u64).wrapping_mul(0x0555_5555_5555_5555)))
            ^ $h[(J + 7) % 16]
    }};
}

/// `expand1b(Qb, M, H, i)`.
macro_rules! expand1 {
    ($q:ident, $m:ident, $h:ident, $($i:literal)*) => {$(
        $q[$i] = sb1($q[$i - 16]) + sb2($q[$i - 15]) + sb3($q[$i - 14]) + sb0($q[$i - 13])
            + sb1($q[$i - 12]) + sb2($q[$i - 11]) + sb3($q[$i - 10]) + sb0($q[$i - 9])
            + sb1($q[$i - 8]) + sb2($q[$i - 7]) + sb3($q[$i - 6]) + sb0($q[$i - 5])
            + sb1($q[$i - 4]) + sb2($q[$i - 3]) + sb3($q[$i - 2]) + sb0($q[$i - 1])
            + add_elt!($m, $h, $i);
    )*};
}

/// `expand2b(Qb, M, H, i)`. Its plain terms `q[i - 16] + q[i - 14] + .. + q[i - 4]` are kept as
/// a running sum `s` per parity of `i`, slid by two words after each use.
macro_rules! expand2 {
    ($q:ident, $m:ident, $h:ident, $($i:literal $s:ident)*) => {$(
        $q[$i] = $s + rb1($q[$i - 15]) + rb2($q[$i - 13]) + rb3($q[$i - 11]) + rb4($q[$i - 9])
            + rb5($q[$i - 7]) + rb6($q[$i - 5]) + rb7($q[$i - 3]) + sb4($q[$i - 2])
            + sb5($q[$i - 1]) + add_elt!($m, $h, $i);
        $s = $s - $q[$i - 16] + $q[$i - 2];
    )*};
}

/// `compress_big`: `FOLDb` of message `m` under chaining value `h`.
#[inline(always)]
// The last slide of each running sum is never read (older compilers warn about it).
#[allow(unused_assignments)]
fn compress(m: &[W; 16], h: &[W; 16]) -> [W; 16] {
    let w = |i: usize| m[i] ^ h[i];
    let mut q = [Wrapping(0); 32];
    // MAKE_Qab
    q[0] = sb0(w(5) - w(7) + w(10) + w(13) + w(14)) + h[1];
    q[1] = sb1(w(6) - w(8) + w(11) + w(14) - w(15)) + h[2];
    q[2] = sb2(w(0) + w(7) + w(9) - w(12) + w(15)) + h[3];
    q[3] = sb3(w(0) - w(1) + w(8) - w(10) + w(13)) + h[4];
    q[4] = sb4(w(1) + w(2) + w(9) - w(11) - w(14)) + h[5];
    q[5] = sb0(w(3) - w(2) + w(10) - w(12) + w(15)) + h[6];
    q[6] = sb1(w(4) - w(0) - w(3) - w(11) + w(13)) + h[7];
    q[7] = sb2(w(1) - w(4) - w(5) - w(12) - w(14)) + h[8];
    q[8] = sb3(w(2) - w(5) - w(6) + w(13) - w(15)) + h[9];
    q[9] = sb4(w(0) - w(3) + w(6) - w(7) + w(14)) + h[10];
    q[10] = sb0(w(8) - w(1) - w(4) - w(7) + w(15)) + h[11];
    q[11] = sb1(w(8) - w(0) - w(2) - w(5) + w(9)) + h[12];
    q[12] = sb2(w(1) + w(3) - w(6) - w(9) + w(10)) + h[13];
    q[13] = sb3(w(2) + w(4) + w(7) + w(10) + w(11)) + h[14];
    q[14] = sb4(w(3) - w(5) + w(8) - w(11) - w(12)) + h[15];
    q[15] = sb0(w(12) - w(4) - w(6) - w(9) + w(13)) + h[0];
    // MAKE_Qbb
    expand1!(q, m, h, 16 17);
    let mut even = q[2] + q[4] + q[6] + q[8] + q[10] + q[12] + q[14];
    let mut odd = q[3] + q[5] + q[7] + q[9] + q[11] + q[13] + q[15];
    expand2!(q, m, h, 18 even 19 odd 20 even 21 odd 22 even 23 odd 24 even 25 odd 26 even 27 odd
        28 even 29 odd 30 even 31 odd);
    // FOLD
    let mut d = [Wrapping(0); 16];
    let xl = q[16] ^ q[17] ^ q[18] ^ q[19] ^ q[20] ^ q[21] ^ q[22] ^ q[23];
    let xh = xl ^ q[24] ^ q[25] ^ q[26] ^ q[27] ^ q[28] ^ q[29] ^ q[30] ^ q[31];
    d[0] = ((xh << 5) ^ (q[16] >> 5) ^ m[0]) + (xl ^ q[24] ^ q[0]);
    d[1] = ((xh >> 7) ^ (q[17] << 8) ^ m[1]) + (xl ^ q[25] ^ q[1]);
    d[2] = ((xh >> 5) ^ (q[18] << 5) ^ m[2]) + (xl ^ q[26] ^ q[2]);
    d[3] = ((xh >> 1) ^ (q[19] << 5) ^ m[3]) + (xl ^ q[27] ^ q[3]);
    d[4] = ((xh >> 3) ^ q[20] ^ m[4]) + (xl ^ q[28] ^ q[4]);
    d[5] = ((xh << 6) ^ (q[21] >> 6) ^ m[5]) + (xl ^ q[29] ^ q[5]);
    d[6] = ((xh >> 4) ^ (q[22] << 6) ^ m[6]) + (xl ^ q[30] ^ q[6]);
    d[7] = ((xh >> 11) ^ (q[23] << 2) ^ m[7]) + (xl ^ q[31] ^ q[7]);
    d[8] = rotl(d[4], 9) + (xh ^ q[24] ^ m[8]) + ((xl << 8) ^ q[23] ^ q[8]);
    d[9] = rotl(d[5], 10) + (xh ^ q[25] ^ m[9]) + ((xl >> 6) ^ q[16] ^ q[9]);
    d[10] = rotl(d[6], 11) + (xh ^ q[26] ^ m[10]) + ((xl << 6) ^ q[17] ^ q[10]);
    d[11] = rotl(d[7], 12) + (xh ^ q[27] ^ m[11]) + ((xl << 4) ^ q[18] ^ q[11]);
    d[12] = rotl(d[0], 13) + (xh ^ q[28] ^ m[12]) + ((xl >> 3) ^ q[19] ^ q[12]);
    d[13] = rotl(d[1], 14) + (xh ^ q[29] ^ m[13]) + ((xl >> 4) ^ q[20] ^ q[13]);
    d[14] = rotl(d[2], 15) + (xh ^ q[30] ^ m[14]) + ((xl >> 7) ^ q[21] ^ q[14]);
    d[15] = rotl(d[3], 16) + (xh ^ q[31] ^ m[15]) + ((xl >> 2) ^ q[22] ^ q[15]);
    d
}

/// BMW-512 of one 64-byte message.
pub(crate) fn hash(input: &[u8; 64]) -> [u8; 64] {
    let mut m = [Wrapping(0); 16];
    for (w, b) in m.iter_mut().zip(input.as_chunks::<8>().0) {
        *w = Wrapping(u64::from_le_bytes(*b));
    }
    m[8] = Wrapping(0x80);
    m[15] = Wrapping(512);
    let h = compress(&m, &IV.map(Wrapping));
    let h = compress(&h, &FINAL.map(Wrapping));
    let mut out = [0; 64];
    for (o, w) in out.as_chunks_mut::<8>().0.iter_mut().zip(&h[8..]) {
        *o = w.0.to_le_bytes();
    }
    out
}
