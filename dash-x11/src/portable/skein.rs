//! Skein-512-512 (Core: `crypto/x11/skein.c`, `sph_skein512_init`/`sph_skein512`/
//! `sph_skein512_close`).
//!
//! Specialised to one 64-byte message: `skein_big_core` only buffers it, then
//! `skein_big_close` runs two UBI calls: the message block (first and final, type 48, position
//! 64) from `IV512`, and the zero output block (first and final, type 63, position 8). The
//! Threefish rounds are sph's unrolled (non-small-footprint) `UBI_BIG`.

/// `IV512`.
#[rustfmt::skip]
const IV: [u64; 8] = [
    0x4903_ADFF_749C_51CE, 0x0D95_DE39_9746_DF03,
    0x8FD1_9341_27C7_9BCE, 0x9A25_5629_FF35_2CB1,
    0x5DB6_2599_DF6C_A7B0, 0xEABE_394C_A9D5_C3F4,
    0x9911_12C7_1A75_B523, 0xAE18_A40B_660F_CC33,
];

/// The message block's tweak: `et = 352 + (bcount == 0) << 7`, position `ptr = 64`.
const MSG_T0: u64 = 64;
const MSG_T1: u64 = (352 + 128) << 55;
/// The output block's tweak: `et = 510`, position `ptr = 8`.
const OUT_T0: u64 = 8;
const OUT_T1: u64 = 510 << 55;

/// `TFBIG_ADDKEY` for subkey `s`: `k[(s + i) % 9]`, tweak words `t[s % 3]`, `t[(s + 1) % 3]`.
macro_rules! addkey {
    ($p:ident, $k:ident, $t:ident, $s:literal) => {
        $p[0] = $p[0].wrapping_add($k[($s + 0) % 9]);
        $p[1] = $p[1].wrapping_add($k[($s + 1) % 9]);
        $p[2] = $p[2].wrapping_add($k[($s + 2) % 9]);
        $p[3] = $p[3].wrapping_add($k[($s + 3) % 9]);
        $p[4] = $p[4].wrapping_add($k[($s + 4) % 9]);
        $p[5] = $p[5].wrapping_add($k[($s + 5) % 9]).wrapping_add($t[$s % 3]);
        $p[6] = $p[6].wrapping_add($k[($s + 6) % 9]).wrapping_add($t[($s + 1) % 3]);
        $p[7] = $p[7].wrapping_add($k[($s + 7) % 9]).wrapping_add($s);
    };
}

/// `TFBIG_MIX8`.
macro_rules! mix8 {
    ($p:ident; $($x0:literal $x1:literal $rc:literal),*) => {$(
        $p[$x0] = $p[$x0].wrapping_add($p[$x1]);
        $p[$x1] = $p[$x1].rotate_left($rc) ^ $p[$x0];
    )*};
}

/// `TFBIG_4e(s)` then `TFBIG_4o(s + 1)`, for each even `s`.
macro_rules! rounds8 {
    ($p:ident, $k:ident, $t:ident; $($s:literal $s1:literal),*) => {$(
        addkey!($p, $k, $t, $s);
        mix8!($p; 0 1 46, 2 3 36, 4 5 19, 6 7 37);
        mix8!($p; 2 1 33, 4 7 27, 6 5 14, 0 3 42);
        mix8!($p; 4 1 17, 6 3 49, 0 5 36, 2 7 39);
        mix8!($p; 6 1 44, 0 7 9, 2 5 54, 4 3 56);
        addkey!($p, $k, $t, $s1);
        mix8!($p; 0 1 39, 2 3 30, 4 5 34, 6 7 24);
        mix8!($p; 2 1 13, 4 7 50, 6 5 10, 0 3 17);
        mix8!($p; 4 1 25, 6 3 29, 0 5 39, 2 7 43);
        mix8!($p; 6 1 8, 0 7 35, 2 5 56, 4 3 22);
    )*};
}

/// `UBI_BIG`: one block `m` with chaining value `h` and tweak `t0, t1`.
#[inline(always)]
fn ubi(h: &[u64; 8], m: &[u64; 8], t0: u64, t1: u64) -> [u64; 8] {
    let k = [
        h[0],
        h[1],
        h[2],
        h[3],
        h[4],
        h[5],
        h[6],
        h[7],
        h[0] ^ h[1] ^ h[2] ^ h[3] ^ h[4] ^ h[5] ^ h[6] ^ h[7] ^ 0x1BD1_1BDA_A9FC_1A22,
    ];
    let t = [t0, t1, t0 ^ t1];
    let mut p = *m;
    rounds8!(p, k, t; 0 1, 2 3, 4 5, 6 7, 8 9, 10 11, 12 13, 14 15, 16 17);
    addkey!(p, k, t, 18);
    core::array::from_fn(|i| m[i] ^ p[i])
}

/// Skein-512-512 of one 64-byte message.
pub(crate) fn hash(input: &[u8; 64]) -> [u8; 64] {
    let mut m = [0; 8];
    for (w, b) in m.iter_mut().zip(input.as_chunks::<8>().0) {
        *w = u64::from_le_bytes(*b);
    }
    let h = ubi(&IV, &m, MSG_T0, MSG_T1);
    let h = ubi(&h, &[0; 8], OUT_T0, OUT_T1);
    let mut out = [0; 64];
    for (o, w) in out.as_chunks_mut::<8>().0.iter_mut().zip(&h) {
        *o = w.to_le_bytes();
    }
    out
}
