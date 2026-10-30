//! SIMD-512 (Core: `src/crypto/x11/simd.c`, the full form), specialised to one 64-byte message.
//! `finalize_big` with `ptr == 64` compresses two 128-byte blocks: the message padded with 64
//! zero bytes (`compress_big(sc, 0)`, twist `yoff_b_n`), then the bit count block
//! `LE32(512) || 0..` (`compress_big(sc, 1)`, twist `yoff_b_f`). The second block is fixed, so
//! its message expansion is computed at compile time. Both blocks end in 64 zero bytes, so
//! sph's `FFT8` only ever sees two nonzero inputs.
//!
//! The FFT keeps sph's `q[256]` order: sixteen `FFT16` rows, then sph's `FFT_LOOP` butterflies
//! level by level. The state is sph's `A0..A7, B0..B7, C0..C7, D0..D7`.

/// sph's `alpha_tab`: the powers of 41 modulo 257.
#[rustfmt::skip]
pub(crate) const ALPHA_TAB: [i32; 256] = [
      1,  41, 139,  45,  46,  87, 226,  14,  60, 147, 116, 130, 190,  80, 196,  69,
      2,  82,  21,  90,  92, 174, 195,  28, 120,  37, 232,   3, 123, 160, 135, 138,
      4, 164,  42, 180, 184,  91, 133,  56, 240,  74, 207,   6, 246,  63,  13,  19,
      8,  71,  84, 103, 111, 182,   9, 112, 223, 148, 157,  12, 235, 126,  26,  38,
     16, 142, 168, 206, 222, 107,  18, 224, 189,  39,  57,  24, 213, 252,  52,  76,
     32,  27,  79, 155, 187, 214,  36, 191, 121,  78, 114,  48, 169, 247, 104, 152,
     64,  54, 158,  53, 117, 171,  72, 125, 242, 156, 228,  96,  81, 237, 208,  47,
    128, 108,  59, 106, 234,  85, 144, 250, 227,  55, 199, 192, 162, 217, 159,  94,
    256, 216, 118, 212, 211, 170,  31, 243, 197, 110, 141, 127,  67, 177,  61, 188,
    255, 175, 236, 167, 165,  83,  62, 229, 137, 220,  25, 254, 134,  97, 122, 119,
    253,  93, 215,  77,  73, 166, 124, 201,  17, 183,  50, 251,  11, 194, 244, 238,
    249, 186, 173, 154, 146,  75, 248, 145,  34, 109, 100, 245,  22, 131, 231, 219,
    241, 115,  89,  51,  35, 150, 239,  33,  68, 218, 200, 233,  44,   5, 205, 181,
    225, 230, 178, 102,  70,  43, 221,  66, 136, 179, 143, 209,  88,  10, 153, 105,
    193, 203,  99, 204, 140,  86, 185, 132,  15, 101,  29, 161, 176,  20,  49, 210,
    129, 149, 198, 151,  23, 172, 113,   7,  30, 202,  58,  65,  95,  40,  98, 163,
];

/// sph's `yoff_b_n`, the twist of every block but the last.
#[rustfmt::skip]
const YOFF_B_N: [u16; 256] = [
      1, 163,  98,  40,  95,  65,  58, 202,  30,   7, 113, 172,  23, 151, 198, 149,
    129, 210,  49,  20, 176, 161,  29, 101,  15, 132, 185,  86, 140, 204,  99, 203,
    193, 105, 153,  10,  88, 209, 143, 179, 136,  66, 221,  43,  70, 102, 178, 230,
    225, 181, 205,   5,  44, 233, 200, 218,  68,  33, 239, 150,  35,  51,  89, 115,
    241, 219, 231, 131,  22, 245, 100, 109,  34, 145, 248,  75, 146, 154, 173, 186,
    249, 238, 244, 194,  11, 251,  50, 183,  17, 201, 124, 166,  73,  77, 215,  93,
    253, 119, 122,  97, 134, 254,  25, 220, 137, 229,  62,  83, 165, 167, 236, 175,
    255, 188,  61, 177,  67, 127, 141, 110, 197, 243,  31, 170, 211, 212, 118, 216,
    256,  94, 159, 217, 162, 192, 199,  55, 227, 250, 144,  85, 234, 106,  59, 108,
    128,  47, 208, 237,  81,  96, 228, 156, 242, 125,  72, 171, 117,  53, 158,  54,
     64, 152, 104, 247, 169,  48, 114,  78, 121, 191,  36, 214, 187, 155,  79,  27,
     32,  76,  52, 252, 213,  24,  57,  39, 189, 224,  18, 107, 222, 206, 168, 142,
     16,  38,  26, 126, 235,  12, 157, 148, 223, 112,   9, 182, 111, 103,  84,  71,
      8,  19,  13,  63, 246,   6, 207,  74, 240,  56, 133,  91, 184, 180,  42, 164,
      4, 138, 135, 160, 123,   3, 232,  37, 120,  28, 195, 174,  92,  90,  21,  82,
      2,  69, 196,  80, 190, 130, 116, 147,  60,  14, 226,  87,  46,  45, 139,  41,
];

/// sph's `yoff_b_f`, the twist of the last block.
#[rustfmt::skip]
const YOFF_B_F: [u16; 256] = [
      2, 203, 156,  47, 118, 214, 107, 106,  45,  93, 212,  20, 111,  73, 162, 251,
     97, 215, 249,  53, 211,  19,   3,  89,  49, 207, 101,  67, 151, 130, 223,  23,
    189, 202, 178, 239, 253, 127, 204,  49,  76, 236,  82, 137, 232, 157,  65,  79,
     96, 161, 176, 130, 161,  30,  47,   9, 189, 247,  61, 226, 248,  90, 107,  64,
      0,  88, 131, 243, 133,  59, 113, 115,  17, 236,  33, 213,  12, 191, 111,  19,
    251,  61, 103, 208,  57,  35, 148, 248,  47, 116,  65, 119, 249, 178, 143,  40,
    189, 129,   8, 163, 204, 227, 230, 196, 205, 122, 151,  45, 187,  19, 227,  72,
    247, 125, 111, 121, 140, 220,   6, 107,  77,  69,  10, 101,  21,  65, 149, 171,
    255,  54, 101, 210, 139,  43, 150, 151, 212, 164,  45, 237, 146, 184,  95,   6,
    160,  42,   8, 204,  46, 238, 254, 168, 208,  50, 156, 190, 106, 127,  34, 234,
     68,  55,  79,  18,   4, 130,  53, 208, 181,  21, 175, 120,  25, 100, 192, 178,
    161,  96,  81, 127,  96, 227, 210, 248,  68,  10, 196,  31,   9, 167, 150, 193,
      0, 169, 126,  14, 124, 198, 144, 142, 240,  21, 224,  44, 245,  66, 146, 238,
      6, 196, 154,  49, 200, 222, 109,   9, 210, 141, 192, 138,   8,  79, 114, 217,
     68, 128, 249,  94,  53,  30,  27,  61,  52, 135, 106, 212,  70, 238,  30, 185,
     10, 132, 146, 136, 117,  37, 251, 150, 180, 188, 247, 156, 236, 192, 108,  86,
];

/// sph's `IV512`.
#[rustfmt::skip]
pub(crate) const IV512: [u32; 32] = [
    0x0BA16B95, 0x72F999AD, 0x9FECC2AE, 0xBA3264FC,
    0x5E894929, 0x8E9F30E5, 0x2F1DAA37, 0xF0F2C558,
    0xAC506643, 0xA90635A5, 0xE25B878B, 0xAAB7878F,
    0x88817F7A, 0x0A02892B, 0x559A7550, 0x598F657E,
    0x7EEF60A1, 0x6B70E3E8, 0x9C1714D1, 0xB958E2A8,
    0xAB02675E, 0xED1C014F, 0xCD8D65BB, 0xFDB7A257,
    0x09254899, 0xD699C7BC, 0x9019B6DC, 0x2B9022E4,
    0x8FA14956, 0x21BF9BD3, 0xB94D0943, 0x6FFDDC22,
];

/// sph's `wbp` (without the `<< 4`): which 16-entry slice of `q` feeds each step's `W`.
#[rustfmt::skip]
pub(crate) const WBP: [usize; 32] = [
     4,  6,  0,  2,  7,  5,  3,  1,
    15, 11, 12,  8,  9, 13, 10, 14,
    17, 18, 23, 20, 22, 21, 16, 19,
    30, 24, 25, 31, 27, 29, 28, 26,
];

/// sph's `PP8_k_n` is `PP8[k] ^ n`; step `j` of round `i` uses `PP8[(i + j) % 7]` (`M7_j_i`).
pub(crate) const PP8: [usize; 7] = [1, 6, 2, 3, 5, 7, 4];

/// sph's `REDS1`.
const fn reds1(x: i32) -> i32 {
    (x & 0xFF) - (x >> 8)
}

/// sph's `REDS2`.
const fn reds2(x: i32) -> i32 {
    (x & 0xFFFF) + (x >> 16)
}

/// sph's `FFT_LOOP` twiddles of each level, contiguous: `TWIDDLE[l][u]` is
/// `alpha_tab[u * (8 >> l)]` for the level with half-size `16 << l`.
const TWIDDLE: [[i32; 128]; 4] = {
    let mut t = [[0; 128]; 4];
    let mut l = 0;
    while l < 4 {
        let mut u = 0;
        while u < 16 << l {
            t[l][u] = ALPHA_TAB[u * (8 >> l)];
            u += 1;
        }
        l += 1;
    }
    t
};

/// sph's `FFT256` of a 128-byte block whose last 64 bytes are zero, then the twist and
/// reduction of `compress_big`: every `q[i]` ends in `-128..=128`.
const fn fft256(x: &[u8; 64], yoff: &[u16; 256]) -> [i32; 256] {
    let mut q = [0i32; 256];
    // The sixteen `FFT16`s: row `r` of `fft64` number `p` (input bytes `x[j + 4k]`, `j` from
    // `FFT256`'s call order) reads bytes `b` and `b + 16`, `b = j + [0, 8, 4, 12][r]`.
    let mut row = 0;
    while row < 16 {
        let b = [0, 2, 1, 3][row >> 2] + [0, 8, 4, 12][row & 3];
        let d1 = fft8(x[b] as i32, x[b + 32] as i32);
        let d2 = fft8(x[b + 16] as i32, x[b + 48] as i32);
        let mut k = 0;
        while k < 8 {
            let t = d2[k] << k;
            q[16 * row + k] = d1[k] + t;
            q[16 * row + 8 + k] = d1[k] - t;
            k += 1;
        }
        row += 1;
    }
    // sph's `FFT_LOOP`s, level by level: half-sizes 16 (in `FFT32`), 32 (`fft64`), 64 and
    // 128 (`FFT256`). The first butterfly of each skips `REDS2` (its twiddle is 1).
    let mut l = 0;
    while l < 4 {
        let hk = 16 << l;
        let mut base = 0;
        while base < 256 {
            let (m0, n0) = (q[base], q[base + hk]);
            let mut u = 0;
            while u < hk {
                let m = q[base + u];
                let t = reds2(q[base + u + hk].wrapping_mul(TWIDDLE[l][u]));
                q[base + u] = m + t;
                q[base + u + hk] = m - t;
                u += 1;
            }
            q[base] = m0 + n0;
            q[base + hk] = m0 - n0;
            base += 2 * hk;
        }
        l += 1;
    }
    let mut i = 0;
    while i < 256 {
        let tq = reds1(reds1(reds2(q[i] + yoff[i] as i32)));
        q[i] = if tq <= 128 {
            tq
        } else {
            tq - 257
        };
        i += 1;
    }
    q
}

/// sph's `FFT8` with `x2 = x3 = 0`.
const fn fft8(x0: i32, x1: i32) -> [i32; 8] {
    let b1 = reds1(x1 << 2);
    let b2 = x1 << 4;
    let b3 = reds1(x1 << 6);
    [x0 + x1, x0 + b1, x0 + b2, x0 + b3, x0 - x1, x0 - b1, x0 - b2, x0 - b3]
}

/// The `W_BIG` words of the 32 steps of sph's four `ONE_ROUND_BIG`.
type W = [[u32; 8]; 32];

/// sph's `W_BIG` (`WB_ri_j`) for every step.
const fn expand(q: &[i32; 256]) -> W {
    /// sph's `INNER`.
    const fn inner(l: i32, h: i32, mm: i32) -> u32 {
        ((l.wrapping_mul(mm) as u32) & 0xFFFF).wrapping_add((h.wrapping_mul(mm) as u32) << 16)
    }
    let mut w = [[0; 8]; 32];
    let mut step = 0;
    while step < 32 {
        let v = 16 * WBP[step];
        let mut n = 0;
        while n < 8 {
            let i = v + 2 * n;
            w[step][n] = match step / 8 {
                0 | 1 => inner(q[i], q[i + 1], 185),
                2 => inner(q[i - 256], q[i - 128], 233),
                _ => inner(q[i - 383], q[i - 255], 233),
            };
            n += 1;
        }
        step += 1;
    }
    w
}

/// The `W` words of the last block, `LE32(512)` then zeros.
const W_FINAL: W = {
    let mut x = [0; 64];
    x[1] = 0x02;
    expand(&fft256(&x, &YOFF_B_F))
};

/// sph's `IF`.
#[inline(always)]
fn if_(x: u32, y: u32, z: u32) -> u32 {
    ((y ^ z) & x) ^ z
}

/// sph's `MAJ`.
#[inline(always)]
fn maj(x: u32, y: u32, z: u32) -> u32 {
    (x & y) | ((x | y) & z)
}

/// sph's state `A0..A7, B0..B7, C0..C7, D0..D7`.
#[derive(Clone, Copy)]
struct State([[u32; 8]; 4]);

/// sph's `ONE_ROUND_BIG(ri, isp, p0, p1, p2, p3)`: eight `STEP_BIG`s, `IF` then `MAJ`.
macro_rules! one_round_big {
    ($s:ident, $w:ident, $i:literal, $p0:literal, $p1:literal, $p2:literal, $p3:literal) => {
        $s.step::<false, $p0, $p1, { PP8[$i % 7] }>(&$w[8 * $i]);
        $s.step::<false, $p1, $p2, { PP8[($i + 1) % 7] }>(&$w[8 * $i + 1]);
        $s.step::<false, $p2, $p3, { PP8[($i + 2) % 7] }>(&$w[8 * $i + 2]);
        $s.step::<false, $p3, $p0, { PP8[($i + 3) % 7] }>(&$w[8 * $i + 3]);
        $s.step::<true, $p0, $p1, { PP8[($i + 4) % 7] }>(&$w[8 * $i + 4]);
        $s.step::<true, $p1, $p2, { PP8[($i + 5) % 7] }>(&$w[8 * $i + 5]);
        $s.step::<true, $p2, $p3, { PP8[($i + 6) % 7] }>(&$w[8 * $i + 6]);
        $s.step::<true, $p3, $p0, { PP8[($i + 7) % 7] }>(&$w[8 * $i + 7]);
    };
}

impl State {
    /// sph's `STEP_BIG`: `fun` is `MAJ` if `MAJ` else `IF`, rotations `R` and `S`, `PP8`
    /// constant `PP`.
    #[inline(always)]
    fn step<const MAJ: bool, const R: u32, const S: u32, const PP: usize>(&mut self, w: &[u32; 8]) {
        let [a, b, c, d] = &mut self.0;
        let ta = a.map(|x| x.rotate_left(R));
        for n in 0..8 {
            let f = if MAJ {
                maj(a[n], b[n], c[n])
            } else {
                if_(a[n], b[n], c[n])
            };
            let tt = d[n].wrapping_add(w[n]).wrapping_add(f);
            a[n] = tt.rotate_left(S).wrapping_add(ta[n ^ PP]);
            d[n] = c[n];
            c[n] = b[n];
            b[n] = ta[n];
        }
    }

    /// `compress_big` after the FFT: the message xor, sph's four `ONE_ROUND_BIG`, the
    /// feed-forward steps.
    #[inline(always)]
    fn compress(&mut self, msg: &[u32; 32], w: &W) {
        let saved = *self;
        for (i, m) in msg.iter().enumerate() {
            self.0[i / 8][i % 8] ^= m;
        }
        one_round_big!(self, w, 0, 3, 23, 17, 27);
        one_round_big!(self, w, 1, 28, 19, 22, 7);
        one_round_big!(self, w, 2, 29, 9, 15, 5);
        one_round_big!(self, w, 3, 4, 13, 10, 25);
        let [a, b, c, d] = &saved.0;
        self.step::<false, 4, 13, { PP8[4] }>(a);
        self.step::<false, 13, 10, { PP8[5] }>(b);
        self.step::<false, 10, 25, { PP8[6] }>(c);
        self.step::<false, 25, 4, { PP8[0] }>(d);
    }
}

/// SIMD-512 of a 64-byte message, as `sph_simd512_init`, `sph_simd512(.., 64)`,
/// `sph_simd512_close` compute it.
pub(crate) fn hash(input: &[u8; 64]) -> [u8; 64] {
    let mut s = State([[0; 8]; 4]);
    for (i, v) in IV512.iter().enumerate() {
        s.0[i / 8][i % 8] = *v;
    }
    // The message block, `dec32le`; its upper half is zero.
    let mut msg = [0; 32];
    for (m, c) in msg.iter_mut().zip(input.as_chunks::<4>().0) {
        *m = u32::from_le_bytes(*c);
    }
    s.compress(&msg, &expand(&fft256(input, &YOFF_B_N)));
    let mut count = [0; 32];
    count[0] = 512;
    s.compress(&count, &W_FINAL);
    // `A0..A7, B0..B7`, `enc32le`.
    let mut out = [0; 64];
    for (i, w) in s.0[..2].as_flattened().iter().enumerate() {
        out[4 * i..][..4].copy_from_slice(&w.to_le_bytes());
    }
    out
}
