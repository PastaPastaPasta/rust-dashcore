//! Grøstl-512 of one 64-byte message with AES-NI (Core: `src/crypto/x11/groestl.c`, whose
//! `groestl_big_close` pads the message to one 128-byte block, so the hash is one compression
//! `P(H ^ m) ^ Q(m) ^ H` and the output transform `P(H) ^ H`, truncated to columns 8..16).
//!
//! The 8×16 byte state is kept as eight vectors, one per row, byte `j` of row `i` being byte
//! `i` of sph's column word `j`; the message is transposed in and the digest out. A round is:
//!
//! - SubBytes and ShiftBytes: `aesenclast(pshufb(row, mask), key)`, the mask undoing AES's
//!   ShiftRows and rotating the row by Grøstl's shift (P: 0, 1, 2, 3, 4, 5, 6, 11; Q: 1, 3,
//!   5, 11, 0, 2, 4, 6);
//! - MixBytes (`circ(2, 2, 3, 4, 5, 3, 5, 7)`): 48 XORs and 16 doublings across the rows;
//! - AddRoundConstant: none. MixBytes is linear, so the next round's constant (`PC64`, or
//!   `QC64`, which complements every byte) is added before it as `B⁻¹ · constant` by the
//!   `aesenclast` key, which also cancels the constant offset of the doubling used here. The
//!   last round's key adds `H` (compression `P`) or the output transform's first constant.

use core::arch::x86_64::*;

// `x · y` in GF(2^8) mod `x^8 + x^4 + x^3 + x + 1`, as AES and Grøstl multiply.
use crate::portable::aes::gf8_mul;

/// A state as bytes: `[row][column]`.
type Rows = [[u8; 16]; 8];

/// The first row of the MixBytes matrix `B = circ(2, 2, 3, 4, 5, 3, 5, 7)`: output row `i` is
/// `Σ_k MIX[(k - i) mod 8] · row k` (sph's `T0[x]` holds `MIX[(0 - i) mod 8] · S(x)` in byte
/// `i`).
const MIX: [u8; 8] = [2, 2, 3, 4, 5, 3, 5, 7];

/// The product of the circulant matrices with first rows `a` and `b`, as its first row
/// (the product of `a` and `b` as polynomials mod `X^8 - 1`).
const fn circ_mul(a: [u8; 8], b: [u8; 8]) -> [u8; 8] {
    let mut c = [0; 8];
    let mut i = 0;
    while i < 8 {
        let mut j = 0;
        while j < 8 {
            c[(i + j) % 8] ^= gf8_mul(a[i], b[j]);
            j += 1;
        }
        i += 1;
    }
    c
}

/// The first row of `B⁻¹`: `B^(n - 1)`, `n = 255 · 256^7` being the order of the unit group of
/// `GF(2^8)[X] / (X + 1)^8`. Checked below.
const MIX_INV: [u8; 8] = {
    let mut e: u64 = 255 * (1 << 56) - 1;
    let mut base = MIX;
    let mut acc = [1, 0, 0, 0, 0, 0, 0, 0];
    while e != 0 {
        if e & 1 != 0 {
            acc = circ_mul(acc, base);
        }
        base = circ_mul(base, base);
        e >>= 1;
    }
    acc
};

const _: () = {
    let id = circ_mul(MIX, MIX_INV);
    let mut i = 0;
    while i < 8 {
        assert!(id[i] == (i == 0) as u8);
        i += 1;
    }
};

/// The constant [`double`] XORs into `2 · x`.
const SKEW: u8 = 0x1b;

/// What [`mix_bytes`] adds to every byte of the true MixBytes: its `2·(2·x ^ u') ^ u` adds
/// [`SKEW`] at each doubling.
const MIX_ERR: u8 = gf8_mul(2, SKEW) ^ SKEW;

/// `B⁻¹ · c`, column by column (`B⁻¹` is circulant too).
const fn unmix(c: Rows) -> Rows {
    let mut out = [[0; 16]; 8];
    let mut i = 0;
    while i < 8 {
        let mut k = 0;
        while k < 8 {
            let f = MIX_INV[(k + 8 - i) % 8];
            let mut j = 0;
            while j < 16 {
                out[i][j] ^= gf8_mul(f, c[k][j]);
                j += 1;
            }
            k += 1;
        }
        i += 1;
    }
    out
}

const fn xor(a: Rows, b: Rows) -> Rows {
    let mut out = a;
    let mut i = 0;
    while i < 8 {
        let mut j = 0;
        while j < 16 {
            out[i][j] ^= b[i][j];
            j += 1;
        }
        i += 1;
    }
    out
}

/// AddRoundConstant of round `r`: `PC64(j << 4, r)` puts `j << 4 | r` in row 0 of column `j`;
/// `QC64(j << 4, r)` complements every byte and adds `j << 4 | r` to row 7.
const fn round_const(q: bool, r: u8) -> Rows {
    let (fill, row) = match q {
        false => (0, 0),
        true => (0xff, 7),
    };
    let mut c = [[fill; 16]; 8];
    let mut j = 0;
    while j < 16 {
        c[row][j] ^= (j as u8) << 4 | r;
        j += 1;
    }
    c
}

/// `H` after `groestl_big_init`: column 15 holds the output size in bits, big-endian.
const IV: Rows = {
    let mut c = [[0; 16]; 8];
    let be = 512u64.to_be_bytes();
    let mut i = 0;
    while i < 8 {
        c[i][15] = be[i];
        i += 1;
    }
    c
};

/// The padding of the message (columns 0..8): `0x80`, zeros, the block count 1 big-endian.
const PAD: Rows = {
    let mut c = [[0; 16]; 8];
    c[0][8] = 0x80;
    let be = 1u64.to_be_bytes();
    let mut i = 0;
    while i < 8 {
        c[i][15] = be[i];
        i += 1;
    }
    c
};

/// The `aesenclast` keys of rounds 0 to 12, which add the next round's constant.
const fn round_keys(q: bool) -> [[__m128i; 8]; 13] {
    let mut k = [[vec([0; 16]); 8]; 13];
    let mut r = 0;
    while r < 13 {
        k[r] = key(round_const(q, r as u8 + 1));
        r += 1;
    }
    k
}

/// The `aesenclast` key that makes MixBytes' output `B · state ^ c`: `B⁻¹ · (c ^ MIX_ERR)`.
const fn key(c: Rows) -> [__m128i; 8] {
    rows(unmix(xor(c, [[MIX_ERR; 16]; 8])))
}

/// A row from its bytes.
const fn vec(b: [u8; 16]) -> __m128i {
    // SAFETY: `__m128i` is 16 bytes, all of whose values are valid.
    unsafe { core::mem::transmute(b) }
}

const fn rows(c: Rows) -> [__m128i; 8] {
    let mut v = [vec([0; 16]); 8];
    let mut i = 0;
    while i < 8 {
        v[i] = vec(c[i]);
        i += 1;
    }
    v
}

/// The `pshufb` masks: `aesenclast` moves byte `sr(b)` to `b` (AES ShiftRows), so taking byte
/// `(b + shift) mod 16` there rotates the row left by `shift`.
const fn shift_masks(shift: [usize; 8]) -> [__m128i; 8] {
    let mut m = [[0; 16]; 8];
    let mut i = 0;
    while i < 8 {
        let mut b = 0;
        while b < 16 {
            let (col, row) = (b / 4, b % 4);
            m[i][4 * ((col + row) % 4) + row] = ((b + shift[i]) % 16) as u8;
            b += 1;
        }
        i += 1;
    }
    rows(m)
}

static SHIFT_P: [__m128i; 8] = shift_masks([0, 1, 2, 3, 4, 5, 6, 11]);
static SHIFT_Q: [__m128i; 8] = shift_masks([1, 3, 5, 11, 0, 2, 4, 6]);
static KEYS_P: [[__m128i; 8]; 13] = round_keys(false);
static KEYS_Q: [[__m128i; 8]; 13] = round_keys(true);
/// The last key of the compression's `P`, which adds `H`.
static LAST_IV: [__m128i; 8] = key(IV);
/// The last key of `Q` and of the output transform's `P`, which adds `P`'s round-0
/// constant: the compression then yields `H ^ PC(0)`, the output transform's input.
static LAST_PC0: [__m128i; 8] = key(round_const(false, 0));
/// The message rows' padding, `H` and round 0's constant, for `P` and `Q`.
static INPUT_P: [__m128i; 8] = rows(xor(xor(PAD, IV), round_const(false, 0)));
static INPUT_Q: [__m128i; 8] = rows(xor(PAD, round_const(true, 0)));

/// `2 · x ^ SKEW` for each byte: `pshufb` of a [`SKEW`] splat gives `SKEW` where the top bit
/// is clear and 0 where it is set.
#[target_feature(enable = "ssse3")]
#[inline]
fn double(x: __m128i) -> __m128i {
    _mm_xor_si128(_mm_add_epi8(x, x), _mm_shuffle_epi8(_mm_set1_epi8(SKEW as i8), x))
}

/// MixBytes plus [`MIX_ERR`] in every byte, by the Grøstl submission's formula (indices mod 8):
/// `t_i = a_i ^ a_{i+1}`, `x_i = t_i ^ t_{i+3}`, `u_i = a_{i+2} ^ t_{i+4} ^ t_{i+6}`,
/// `b_i = 2·(2·x_{i+3} ^ u_{i+3}) ^ u_i`. The source order is the one that compiled to the
/// fewest register moves.
#[target_feature(enable = "ssse3")]
#[inline]
fn mix_bytes(a: [__m128i; 8]) -> [__m128i; 8] {
    let t: [__m128i; 8] = core::array::from_fn(|i| _mm_xor_si128(a[i], a[(i + 1) % 8]));
    let u: [__m128i; 8] = core::array::from_fn(|i| {
        _mm_xor_si128(_mm_xor_si128(a[(i + 2) % 8], t[(i + 4) % 8]), t[(i + 6) % 8])
    });
    let x: [__m128i; 8] = core::array::from_fn(|i| _mm_xor_si128(t[i], t[(i + 3) % 8]));
    core::array::from_fn(|i| {
        let j = (i + 3) % 8;
        _mm_xor_si128(double(_mm_xor_si128(double(x[j]), u[j])), u[i])
    })
}

/// One round: SubBytes and ShiftBytes, the key, MixBytes.
#[target_feature(enable = "aes,ssse3")]
#[inline]
fn round(s: [__m128i; 8], mask: &[__m128i; 8], key: &[__m128i; 8]) -> [__m128i; 8] {
    mix_bytes(core::array::from_fn(|i| {
        _mm_aesenclast_si128(_mm_shuffle_epi8(s[i], mask[i]), key[i])
    }))
}

/// The 14 rounds of `P` or `Q` on a state that has round 0's constant. Unrolled by hand: as a
/// loop, LLVM adds register moves at the back edge.
#[target_feature(enable = "aes,ssse3")]
fn perm(
    mut s: [__m128i; 8],
    mask: &[__m128i; 8],
    keys: &[[__m128i; 8]; 13],
    last: &[__m128i; 8],
) -> [__m128i; 8] {
    s = round(s, mask, &keys[0]);
    s = round(s, mask, &keys[1]);
    s = round(s, mask, &keys[2]);
    s = round(s, mask, &keys[3]);
    s = round(s, mask, &keys[4]);
    s = round(s, mask, &keys[5]);
    s = round(s, mask, &keys[6]);
    s = round(s, mask, &keys[7]);
    s = round(s, mask, &keys[8]);
    s = round(s, mask, &keys[9]);
    s = round(s, mask, &keys[10]);
    s = round(s, mask, &keys[11]);
    s = round(s, mask, &keys[12]);
    round(s, mask, last)
}

/// Transposes the 8×8 byte matrix whose lines `2k` and `2k + 1` are the halves of `x[k]`.
#[target_feature(enable = "ssse3")]
#[inline]
fn transpose(x: [__m128i; 4]) -> [__m128i; 4] {
    let pairs = _mm_setr_epi8(0, 8, 1, 9, 2, 10, 3, 11, 4, 12, 5, 13, 6, 14, 7, 15);
    // 16-bit lane i of y[k]: byte i of lines 2k and 2k + 1.
    let y = x.map(|v| _mm_shuffle_epi8(v, pairs));
    // 32-bit lane i of z[0] and z[1]: byte i (z[1]: i + 4) of lines 0..4; z[2], z[3]: 4..8.
    let z = [
        _mm_unpacklo_epi16(y[0], y[1]),
        _mm_unpackhi_epi16(y[0], y[1]),
        _mm_unpacklo_epi16(y[2], y[3]),
        _mm_unpackhi_epi16(y[2], y[3]),
    ];
    [
        _mm_unpacklo_epi32(z[0], z[2]),
        _mm_unpackhi_epi32(z[0], z[2]),
        _mm_unpacklo_epi32(z[1], z[3]),
        _mm_unpackhi_epi32(z[1], z[3]),
    ]
}

/// Grøstl-512 of `input`.
///
/// # Safety
///
/// The CPU must support AES-NI, SSSE3 and SSE4.1.
#[target_feature(enable = "aes,ssse3,sse4.1")]
pub(crate) unsafe fn hash_aes(input: &[u8; 64]) -> [u8; 64] {
    // SAFETY: each load reads 16 bytes inside `input`.
    let cols: [__m128i; 4] =
        core::array::from_fn(|k| unsafe { _mm_loadu_si128(input.as_ptr().add(16 * k).cast()) });
    // Rows 2k and 2k + 1 of the message, columns 0..8.
    let m = transpose(cols);
    let m: [__m128i; 8] = core::array::from_fn(|i| match i % 2 {
        0 => _mm_move_epi64(m[i / 2]),
        _ => _mm_srli_si128::<8>(m[i / 2]),
    });
    let p = perm(
        core::array::from_fn(|i| _mm_xor_si128(m[i], INPUT_P[i])),
        &SHIFT_P,
        &KEYS_P,
        &LAST_IV,
    );
    let q = perm(
        core::array::from_fn(|i| _mm_xor_si128(m[i], INPUT_Q[i])),
        &SHIFT_Q,
        &KEYS_Q,
        &LAST_PC0,
    );
    // H ^ PC(0), H being the chaining value after the compression.
    let h: [__m128i; 8] = core::array::from_fn(|i| _mm_xor_si128(p[i], q[i]));
    let x = perm(h, &SHIFT_P, &KEYS_P, &LAST_PC0);
    // H ^ P(H) = (H ^ PC(0)) ^ (P(H) ^ PC(0)); the digest is columns 8..16.
    let f: [__m128i; 8] = core::array::from_fn(|i| _mm_xor_si128(x[i], h[i]));
    let out = transpose(core::array::from_fn(|k| _mm_unpackhi_epi64(f[2 * k], f[2 * k + 1])));
    let mut digest = [0u8; 64];
    for (k, v) in out.into_iter().enumerate() {
        // SAFETY: the store writes 16 bytes inside `digest`.
        unsafe { _mm_storeu_si128(digest.as_mut_ptr().add(16 * k).cast(), v) };
    }
    digest
}
