//! Luffa-512 of one 64-byte message on vector registers (Core: `crypto/x11/luffa.c`, sph's
//! `luffa5`).
//!
//! Five rounds of `MI5` and `P5`, as in [`crate::portable::luffa`]. `P5` runs the
//! permutations `Q_j` of the five sub-states `V[j]` side by side, bitsliced: a vector holds
//! word `k` of several sub-states, one per 32-bit lane, so `SUB_CRUMB` on words `0..4` and
//! `5, 6, 7, 4` and `MIX_WORD` on the pairs `(k, k + 4)` are lane-wise, and the
//! per-sub-state round constants and `TWEAK5` rotations are vectors.
//!
//! - AVX2: eight vectors `w[k]` hold word `k` of the five sub-states in lanes `0..5` (lanes
//!   `5..8` are unused) for the whole hash. `MI5`'s mixing of sub-states is `vpermd` across
//!   lanes; `M2` is a renaming of the vectors and three XORs.
//! - SSE: the state is kept in rows (sub-state `j` in two vectors, words `0..4` and `4..8`),
//!   where `MI5` and `M2` are `palignr`s and XORs. `P5` transposes sub-states `0..4` into
//!   eight vectors of four lanes, and runs `Q_4` on its row, packing words `k` and `k + 5`
//!   into one vector for `SUB_CRUMB`.
//!
//! Two compile-time tricks shorten each round. `SUB_CRUMB`'s two NOTs are not computed: a
//! value is kept with a complement flag, which `xor` propagates and `and`/`or` absorb
//! (`!a & b` is `andnot`, `!a & !b` is `!(a | b)`); with words 2 and 6 stored complemented
//! ([`NEG`]) at the start of a round they are the complemented words again at its end, so
//! a `SUB_CRUMB` is 13 gates instead of 15. And with AVX2 a value is kept rotated by a
//! known amount (its frame, [`FR_X8`]), so that `MIX_WORD`'s rotations cost nothing until
//! two values of different frames are XORed.

use core::arch::x86_64::*;

use crate::portable::luffa::{RC0, RC4, V_INIT};

/// The words stored complemented between rounds of `P5`: a fixed point of the complement
/// flags of a round (with the round constants of words 0 and 4 stored complemented).
const NEG: [bool; 8] = [false, false, true, false, false, false, true, false];

/// The rotation frames of `P5`'s rounds with 256-bit vectors: before round `r`, words `0..4`
/// are stored rotated right by `FR_X8[r].0` and words `4..8` by `FR_X8[r].1`. `MIX_WORD`'s
/// rotations then only change the frame, and an XOR of two frames rotates one operand:
/// `CH_X8[r]` says which for its four XORs (`true`: the first). This schedule, found by
/// exhaustive search, costs 28 rotations (three instructions) and 2 rotations by 8 bits (a
/// `vpshufb`) per `MIX_WORD` pair and `P5` instead of 32 rotations, and ends in frame 0.
/// Any schedule that ends in frame 0 computes the same digests (all zeros is sph's order and
/// passes every test); this one saves about 200 instructions per call (0.5% of X11).
const FR_X8: [(u32, u32); 9] =
    [(0, 0), (12, 3), (24, 18), (4, 1), (16, 16), (28, 19), (8, 2), (20, 17), (0, 0)];

/// See [`FR_X8`].
const CH_X8: [[bool; 4]; 8] = [
    [false, false, true, false],
    [false; 4],
    [false; 4],
    [false; 4],
    [false, false, true, false],
    [false; 4],
    [false; 4],
    [false; 4],
];

/// The frames with 128-bit vectors: sph's order, which rotates only values that are dead
/// afterwards. Without three-operand instructions, rotating a live value costs a copy, and
/// no schedule does better.
const FR_X4: [(u32, u32); 9] = [(0, 0); 9];

/// See [`FR_X4`].
const CH_X4: [[bool; 4]; 8] = [[true; 4]; 8];

/// `x` stored in frame `f`: `rotl(stored, f) == x`.
const fn in_frame<const N: usize>(x: [u32; N], f: u32) -> [u32; N] {
    let mut r = x;
    let mut i = 0;
    while i < N {
        r[i] = x[i].rotate_right(f);
        i += 1;
    }
    r
}

/// sph's `M2` on one sub-state.
const fn m2(s: [u32; 8]) -> [u32; 8] {
    let t = s[7];
    [t, s[0] ^ t, s[1], s[2] ^ t, s[3] ^ t, s[4], s[5], s[6]]
}

/// Word-wise XOR.
const fn xor8(a: [u32; 8], b: [u32; 8]) -> [u32; 8] {
    let mut r = [0; 8];
    let mut i = 0;
    while i < 8 {
        r[i] = a[i] ^ b[i];
        i += 1;
    }
    r
}

/// sph's `MI5` without the message: the state after the first `MI5` is
/// `MI5(V_INIT, 0) ^ (M2^j(m) in sub-state j)`.
const fn mi5_zero(mut v: [[u32; 8]; 5]) -> [[u32; 8]; 5] {
    let a = m2(xor8(xor8(xor8(v[0], v[1]), xor8(v[2], v[3])), v[4]));
    let mut j = 0;
    while j < 5 {
        v[j] = xor8(v[j], a);
        j += 1;
    }
    let b = xor8(m2(v[0]), v[1]);
    v[1] = xor8(m2(v[1]), v[2]);
    v[2] = xor8(m2(v[2]), v[3]);
    v[3] = xor8(m2(v[3]), v[4]);
    v[4] = xor8(m2(v[4]), v[0]);
    v[0] = xor8(m2(b), v[4]);
    v[4] = xor8(m2(v[4]), v[3]);
    v[3] = xor8(m2(v[3]), v[2]);
    v[2] = xor8(m2(v[2]), v[1]);
    v[1] = xor8(m2(v[1]), b);
    v
}

/// `!a`, word-wise.
const fn not8(a: [u32; 8]) -> [u32; 8] {
    xor8(a, [!0; 8])
}

/// Lanes `0..5` of a 256-bit vector, lanes `5..8` zero.
const fn lanes5(a: [u32; 5]) -> [u32; 8] {
    [a[0], a[1], a[2], a[3], a[4], 0, 0, 0]
}

/// The state after the first `MI5` with a zero message, bitsliced (`[k][j]` = word `k` of
/// sub-state `j`).
static V1_X8: [[u32; 8]; 8] = {
    let v = mi5_zero(V_INIT);
    let mut r = [[0; 8]; 8];
    let mut k = 0;
    while k < 8 {
        r[k] = lanes5([v[0][k], v[1][k], v[2][k], v[3][k], v[4][k]]);
        k += 1;
    }
    r
};

/// `!RC0` and `!RC4` per round, bitsliced (lane `j` for sub-state `j`), in the frames
/// after the round ([`FR_X8`]).
static NRC_X8: [[[u32; 8]; 2]; 8] = {
    let mut r = [[[0; 8]; 2]; 8];
    let mut i = 0;
    while i < 8 {
        let rc0 = not8(lanes5([RC0[0][i], RC0[1][i], RC0[2][i], RC0[3][i], RC0[4][i]]));
        let rc4 = not8(lanes5([RC4[0][i], RC4[1][i], RC4[2][i], RC4[3][i], RC4[4][i]]));
        r[i] = [in_frame(rc0, FR_X8[i + 1].0), in_frame(rc4, FR_X8[i + 1].1)];
        i += 1;
    }
    r
};

/// The padding block's contribution to `MI5`, bitsliced: `M2^j(80 00 .. 00)` is
/// `0x80000000` in word `j`.
static PAD_X8: [[u32; 8]; 5] = {
    let mut r = [[0; 8]; 5];
    let mut k = 0;
    while k < 5 {
        r[k][k] = 0x8000_0000;
        k += 1;
    }
    r
};

/// `TWEAK5`'s left and right shift counts per sub-state lane (a rotation by 0 is `x | x`).
static TWEAK_X8: [[u32; 8]; 2] = [[0, 1, 2, 3, 4, 0, 0, 0], [0, 31, 30, 29, 28, 0, 0, 0]];

/// `vpermd` indices for `MI5` (lane `j` reads sub-state `j + 1`, `j - 1`, `j + 2` mod 5), for
/// `M2` on one sub-state (lane `k` reads `k - 1`, and the broadcast of word 7), and the
/// words that word 7 is XORed into by `M2`.
static IDX_X8: [[u32; 8]; 6] = [
    [1, 2, 3, 4, 0, 5, 6, 7],
    [4, 0, 1, 2, 3, 5, 6, 7],
    [2, 3, 4, 0, 1, 5, 6, 7],
    [7, 0, 1, 2, 3, 4, 5, 6],
    [7; 8],
    [0, !0, 0, !0, !0, 0, 0, 0],
];

/// The `pshufb` control reversing the bytes of each 32-bit word.
static BSWAP_X8: [u32; 8] = [
    0x00010203, 0x04050607, 0x08090a0b, 0x0c0d0e0f, 0x00010203, 0x04050607, 0x08090a0b, 0x0c0d0e0f,
];

/// The bitsliced gates, `SUB_CRUMB`, `MIX_WORD` and a round of `P5` for one vector type.
macro_rules! bitslice {
    ($v:ty, $feat:literal, $and:ident, $andnot:ident, $or:ident, $xor:ident, $sll:ident,
     $srl:ident, $set1:ident, $fr:ident, $ch:ident) => {
        use core::arch::x86_64::*;

        use super::{NEG, $ch as CH, $fr as FR};

        /// The value `rotl(v, f)`, complemented if `n` (`n` and `f` are always compile-time
        /// constants).
        #[derive(Clone, Copy)]
        pub(super) struct L {
            v: $v,
            n: bool,
            f: u32,
        }

        /// The value `rotl(v, f)` (`!` that if `n`).
        #[inline]
        pub(super) fn lit(v: $v, n: bool, f: u32) -> L {
            L {
                v,
                n,
                f,
            }
        }

        /// `a` stored in frame `f`.
        #[inline]
        #[target_feature(enable = $feat)]
        pub(super) fn to(a: L, f: u32) -> L {
            let d = (a.f + 32 - f) % 32;
            if d == 0 {
                return a;
            }
            // A constant count: LLVM emits immediate shifts, or a byte shuffle for d % 8 == 0.
            let c = |d: u32| _mm_cvtsi32_si128(d as i32);
            lit($or($sll(a.v, c(d)), $srl(a.v, c(32 - d))), a.n, f)
        }

        /// The vector standing for `a` in frame 0 with complement flag `n`.
        #[inline]
        #[target_feature(enable = $feat)]
        pub(super) fn val(a: L, n: bool) -> $v {
            let a = to(a, 0);
            if a.n == n {
                a.v
            } else {
                $xor(a.v, $set1(-1))
            }
        }

        /// The stored vector of `a`, which must have flag `n`.
        #[inline]
        pub(super) fn raw(a: L, n: bool) -> $v {
            // A constant: no code.
            assert!(a.n == n);
            a.v
        }

        #[inline]
        #[target_feature(enable = $feat)]
        fn and(a: L, b: L) -> L {
            assert!(a.f == b.f);
            match (a.n, b.n) {
                (false, false) => lit($and(a.v, b.v), false, a.f),
                (true, false) => lit($andnot(a.v, b.v), false, a.f),
                (false, true) => lit($andnot(b.v, a.v), false, a.f),
                (true, true) => lit($or(a.v, b.v), true, a.f),
            }
        }

        #[inline]
        #[target_feature(enable = $feat)]
        fn or(a: L, b: L) -> L {
            assert!(a.f == b.f);
            match (a.n, b.n) {
                (false, false) => lit($or(a.v, b.v), false, a.f),
                (true, false) => lit($andnot(b.v, a.v), true, a.f),
                (false, true) => lit($andnot(a.v, b.v), true, a.f),
                (true, true) => lit($and(a.v, b.v), true, a.f),
            }
        }

        #[inline]
        #[target_feature(enable = $feat)]
        pub(super) fn xor(a: L, b: L) -> L {
            assert!(a.f == b.f);
            lit($xor(a.v, b.v), a.n ^ b.n, a.f)
        }

        /// `a ^ b`, rotating `a` into `b`'s frame if `second`, else `b` into `a`'s.
        #[inline]
        #[target_feature(enable = $feat)]
        fn xor_in(a: L, b: L, second: bool) -> L {
            if second {
                xor(to(a, b.f), b)
            } else {
                xor(a, to(b, a.f))
            }
        }

        #[inline]
        fn not(a: L) -> L {
            lit(a.v, !a.n, a.f)
        }

        /// `rotl(a, n)`: a frame change.
        #[inline]
        fn rotl(a: L, n: u32) -> L {
            lit(a.v, a.n, (a.f + n) % 32)
        }

        /// sph's `SUB_CRUMB`.
        #[inline]
        #[target_feature(enable = $feat)]
        pub(super) fn sub_crumb([a0, a1, a2, a3]: [L; 4]) -> [L; 4] {
            let tmp = a0;
            let a0 = or(a0, a1);
            let a2 = xor(a2, a3);
            let a1 = not(a1);
            let a0 = xor(a0, a3);
            let a3 = and(a3, tmp);
            let a1 = xor(a1, a3);
            let a3 = xor(a3, a2);
            let a2 = and(a2, a0);
            let a0 = not(a0);
            let a2 = xor(a2, a1);
            let a1 = or(a1, a3);
            let tmp = xor(tmp, a1);
            let a3 = xor(a3, a2);
            let a2 = and(a2, a1);
            let a1 = xor(a1, a0);
            [tmp, a1, a2, a3]
        }

        /// sph's `MIX_WORD` in round `R`, with the schedule's choice of operand to rotate.
        #[inline]
        #[target_feature(enable = $feat)]
        pub(super) fn mix_word<const R: usize>(u: L, v: L) -> (L, L) {
            let ch = CH[R];
            let v = xor_in(v, u, ch[0]);
            let u = xor_in(rotl(u, 2), v, ch[1]);
            let v = xor_in(rotl(v, 14), u, ch[2]);
            let u = xor_in(rotl(u, 10), v, ch[3]);
            (u, rotl(v, 1))
        }

        /// Round `R` of `Q_j` on the bitsliced words `w`, stored in the schedule's frames for
        /// round `R` with the [`NEG`] words complemented; `nrc0` and `nrc4` are the
        /// complemented round constants in the frames for round `R + 1`.
        #[inline]
        #[target_feature(enable = $feat)]
        pub(super) fn round<const R: usize>(w: [$v; 8], nrc0: $v, nrc4: $v) -> [$v; 8] {
            let (fu, fv) = FR[R];
            let x: [L; 8] = core::array::from_fn(|k| {
                lit(
                    w[k],
                    NEG[k],
                    if k < 4 {
                        fu
                    } else {
                        fv
                    },
                )
            });
            let [a0, a1, a2, a3] = sub_crumb([x[0], x[1], x[2], x[3]]);
            let [a5, a6, a7, a4] = sub_crumb([x[5], x[6], x[7], x[4]]);
            let (a0, a4) = mix_word::<R>(a0, a4);
            let (a1, a5) = mix_word::<R>(a1, a5);
            let (a2, a6) = mix_word::<R>(a2, a6);
            let (a3, a7) = mix_word::<R>(a3, a7);
            let (fu, fv) = FR[R + 1];
            let a0 = xor(to(a0, fu), lit(nrc0, true, fu));
            let a4 = xor(to(a4, fv), lit(nrc4, true, fv));
            let y = [a0, a1, a2, a3, a4, a5, a6, a7];
            core::array::from_fn(|k| {
                raw(
                    to(
                        y[k],
                        if k < 4 {
                            fu
                        } else {
                            fv
                        },
                    ),
                    NEG[k],
                )
            })
        }
    };
}

mod x4 {
    bitslice!(
        __m128i,
        "ssse3,sse4.1",
        _mm_and_si128,
        _mm_andnot_si128,
        _mm_or_si128,
        _mm_xor_si128,
        _mm_sll_epi32,
        _mm_srl_epi32,
        _mm_set1_epi32,
        FR_X4,
        CH_X4
    );
}

mod x8 {
    bitslice!(
        __m256i,
        "avx2",
        _mm256_and_si256,
        _mm256_andnot_si256,
        _mm256_or_si256,
        _mm256_xor_si256,
        _mm256_sll_epi32,
        _mm256_srl_epi32,
        _mm256_set1_epi32,
        FR_X8,
        CH_X8
    );
}

/// Loads a constant vector.
#[inline]
#[target_feature(enable = "avx2")]
fn ld8(a: &[u32; 8]) -> __m256i {
    // SAFETY: the load reads the 32 bytes of `a`.
    unsafe { _mm256_loadu_si256(a.as_ptr().cast()) }
}

/// `P5` on the bitsliced state: `TWEAK5`, then the eight rounds of every `Q_j`.
#[inline]
#[target_feature(enable = "avx2")]
fn p5_x8(w: &mut [__m256i; 8]) {
    let (sl, sr) = (ld8(&TWEAK_X8[0]), ld8(&TWEAK_X8[1]));
    for (k, x) in w.iter_mut().enumerate() {
        if k >= 4 {
            *x = _mm256_or_si256(_mm256_sllv_epi32(*x, sl), _mm256_srlv_epi32(*x, sr));
        }
        *x = x8::val(x8::lit(*x, false, 0), NEG[k]);
    }
    // Unrolled: the rounds differ in their constants and frames.
    macro_rules! rounds {
        ($($r:literal)*) => { $(*w = x8::round::<$r>(*w, ld8(&NRC_X8[$r][0]), ld8(&NRC_X8[$r][1]));)* };
    }
    rounds!(0 1 2 3 4 5 6 7);
    for (k, x) in w.iter_mut().enumerate() {
        *x = x8::val(x8::lit(*x, NEG[k], 0), false);
    }
}

/// `MI5` without the message, on the bitsliced state. sph's sequence is the linear map
/// `Z_j = M2(M2(V_j + M2(S)) + X_j) + V_j`, with `S` the sum of the sub-states and
/// `X_j = V_j + V_{j+2} + V_{j-2}` (indices mod 5); the lane rotations are `vpermd`. It is
/// computed word by word (`M2` reads word `k - 1`, and word 7 for words 0, 1, 3, 4) to keep
/// few vectors live.
#[inline]
#[target_feature(enable = "avx2")]
fn mi5_x8(v: &mut [__m256i; 8]) {
    let (up1, down1, up2) = (ld8(&IDX_X8[0]), ld8(&IDX_X8[1]), ld8(&IDX_X8[2]));
    let x = _mm256_xor_si256;
    // S and X of a word.
    let sx = |w| {
        let d1 = _mm256_permutevar8x32_epi32(w, up1);
        let e1 = _mm256_permutevar8x32_epi32(w, down1);
        // V_{j+2} + V_{j+3}.
        let xs = x(w, _mm256_permutevar8x32_epi32(x(w, d1), up2));
        (x(x(xs, d1), e1), xs)
    };
    // Word k of M2(y), from word k - 1 and word 7.
    let m2k = |k: usize, prev, y7| match k {
        0 => y7,
        1 | 3 | 4 => x(prev, y7),
        _ => prev,
    };
    // A = V + M2(S), B = M2(A) + X, Z = M2(B) + V.
    let (s6, x6) = sx(v[6]);
    let (s7, x7) = sx(v[7]);
    let a7 = x(v[7], s6);
    let (mut s, mut a, mut b) = (s7, a7, a7);
    let mut z = *v;
    for k in 0..8 {
        let (sk, xk) = match k {
            6 => (s6, x6),
            7 => (s7, x7),
            _ => sx(v[k]),
        };
        let ak = if k == 7 {
            a7
        } else {
            x(v[k], m2k(k, s, s7))
        };
        let bk = x(m2k(k, a, a7), xk);
        if k > 0 {
            // Without B7 for words 1, 3, 4 (added below).
            z[k] = x(v[k], b);
        }
        (s, a, b) = (sk, ak, bk);
    }
    z[0] = x(z[0], b);
    z[1] = x(z[1], b);
    z[3] = x(z[3], b);
    z[4] = x(z[4], b);
    *v = z;
}

/// The message block's contribution to `MI5`, bitsliced: `M2^j(m)` in lane `j`.
#[inline]
#[target_feature(enable = "avx2")]
fn msg_x8(block: &[u8; 32]) -> [__m256i; 8] {
    // SAFETY: the load reads the 32 bytes of `block`.
    let m = unsafe { _mm256_loadu_si256(block.as_ptr().cast()) };
    let r0 = _mm256_shuffle_epi8(m, ld8(&BSWAP_X8));
    let (rot, b7, mask) = (ld8(&IDX_X8[3]), ld8(&IDX_X8[4]), ld8(&IDX_X8[5]));
    let m2r = |r| {
        _mm256_xor_si256(
            _mm256_permutevar8x32_epi32(r, rot),
            _mm256_and_si256(_mm256_permutevar8x32_epi32(r, b7), mask),
        )
    };
    let r1 = m2r(r0);
    let r2 = m2r(r1);
    let r3 = m2r(r2);
    let r4 = m2r(r3);
    // Transpose rows r0..r4 into columns; lane 4 of column k is r4[k].
    let t0 = _mm256_unpacklo_epi32(r0, r1);
    let t1 = _mm256_unpackhi_epi32(r0, r1);
    let t2 = _mm256_unpacklo_epi32(r2, r3);
    let t3 = _mm256_unpackhi_epi32(r2, r3);
    let t4 = _mm256_unpacklo_epi32(r4, r4);
    let t5 = _mm256_unpackhi_epi32(r4, r4);
    let u0 = _mm256_unpacklo_epi64(t0, t2);
    let u1 = _mm256_unpackhi_epi64(t0, t2);
    let u2 = _mm256_unpacklo_epi64(t1, t3);
    let u3 = _mm256_unpackhi_epi64(t1, t3);
    let u5 = _mm256_unpackhi_epi64(t4, t4);
    let u7 = _mm256_unpackhi_epi64(t5, t5);
    [
        _mm256_permute2x128_si256::<0x20>(u0, t4),
        _mm256_permute2x128_si256::<0x20>(u1, u5),
        _mm256_permute2x128_si256::<0x20>(u2, t5),
        _mm256_permute2x128_si256::<0x20>(u3, u7),
        _mm256_permute2x128_si256::<0x31>(u0, t4),
        _mm256_permute2x128_si256::<0x31>(u1, u5),
        _mm256_permute2x128_si256::<0x31>(u2, t5),
        _mm256_permute2x128_si256::<0x31>(u3, u7),
    ]
}

/// Writes the XOR of the five sub-states, big-endian, to `out`.
#[inline]
#[target_feature(enable = "avx2")]
fn put_x8(w: &[__m256i; 8], out: &mut [u8; 32]) {
    let x = _mm256_xor_si256;
    // Per four words: lanes 0..4 summed over sub-states 0..4, and sub-state 4 alone.
    let half = |a: &[__m256i]| {
        let t0 = _mm256_unpacklo_epi32(a[0], a[1]);
        let t1 = _mm256_unpackhi_epi32(a[0], a[1]);
        let t2 = _mm256_unpacklo_epi32(a[2], a[3]);
        let t3 = _mm256_unpackhi_epi32(a[2], a[3]);
        let (q01, q23) = (x(t0, t1), x(t2, t3));
        let s = x(_mm256_unpacklo_epi64(q01, q23), _mm256_unpackhi_epi64(q01, q23));
        (s, _mm256_unpacklo_epi64(t0, t2))
    };
    let (s0, l0) = half(&w[..4]);
    let (s1, l1) = half(&w[4..]);
    let s = x(_mm256_permute2x128_si256::<0x20>(s0, s1), _mm256_permute2x128_si256::<0x31>(l0, l1));
    let s = _mm256_shuffle_epi8(s, ld8(&BSWAP_X8));
    // SAFETY: the store writes the 32 bytes of `out`.
    unsafe { _mm256_storeu_si256(out.as_mut_ptr().cast(), s) };
}

/// A state in rows: sub-state `j` is `[words 0..4, words 4..8]`.
type Row = [[__m128i; 2]; 5];

/// The state after the first `MI5` with a zero message, in rows.
static V1_ROW: [[u32; 8]; 5] = mi5_zero(V_INIT);

/// The padding block's contribution to `MI5`, in rows: `0x80000000` in word `j` of
/// sub-state `j` (`PAD_X4[j]` is XORed into words `0..4` of sub-states `0..4`, and into
/// words `4..8` of sub-state 4).
static PAD_X4: [[u32; 4]; 5] = [
    [0x8000_0000, 0, 0, 0],
    [0, 0x8000_0000, 0, 0],
    [0, 0, 0x8000_0000, 0],
    [0, 0, 0, 0x8000_0000],
    [0x8000_0000, 0, 0, 0],
];

/// `!RC0` and `!RC4` per round for sub-states `0..4`, bitsliced (lane `j` for sub-state `j`),
/// in the frames after the round ([`FR_X4`]).
static NRC_X4: [[[u32; 4]; 2]; 8] = {
    let mut r = [[[0; 4]; 2]; 8];
    let mut i = 0;
    while i < 8 {
        let mut j = 0;
        while j < 4 {
            r[i][0][j] = (!RC0[j][i]).rotate_right(FR_X4[i + 1].0);
            r[i][1][j] = (!RC4[j][i]).rotate_right(FR_X4[i + 1].1);
            j += 1;
        }
        i += 1;
    }
    r
};

/// The round constants of sub-state 4 per round, as XORed into its words `0..4` and
/// (complemented) `4..8`.
static RC_S4: [[[u32; 4]; 2]; 8] = {
    let mut r = [[[0; 4]; 2]; 8];
    let mut i = 0;
    while i < 8 {
        r[i] = [[RC0[4][i], 0, 0, 0], [!RC4[4][i], !0, !0, !0]];
        i += 1;
    }
    r
};

/// Loads a constant vector.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn ld4(a: &[u32; 4]) -> __m128i {
    // SAFETY: the load reads the 16 bytes of `a`.
    unsafe { _mm_loadu_si128(a.as_ptr().cast()) }
}

/// Loads one sub-state.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn ld_row(a: &[u32; 8]) -> [__m128i; 2] {
    [ld4(a.first_chunk().unwrap()), ld4(a.last_chunk().unwrap())]
}

/// Sub-state-wise XOR.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn xor_row(a: [__m128i; 2], b: [__m128i; 2]) -> [__m128i; 2] {
    [_mm_xor_si128(a[0], b[0]), _mm_xor_si128(a[1], b[1])]
}

/// sph's `M2` on one sub-state: the words move up by one (`palignr`), word 7 is XORed into
/// words 1, 3 and 4.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn m2_row([l, h]: [__m128i; 2]) -> [__m128i; 2] {
    // [w7, 0, 0, 0].
    let t = _mm_srli_si128::<12>(h);
    [
        _mm_xor_si128(_mm_alignr_epi8::<12>(l, h), _mm_shuffle_epi32::<0x11>(t)),
        _mm_xor_si128(_mm_alignr_epi8::<12>(h, l), t),
    ]
}

/// sph's `MI5` without the message, in rows.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn mi5_row(v: &mut Row) {
    let x = xor_row;
    let a = m2_row(x(x(x(v[0], v[1]), x(v[2], v[3])), v[4]));
    for vj in v.iter_mut() {
        *vj = x(*vj, a);
    }
    let b = x(m2_row(v[0]), v[1]);
    v[1] = x(m2_row(v[1]), v[2]);
    v[2] = x(m2_row(v[2]), v[3]);
    v[3] = x(m2_row(v[3]), v[4]);
    v[4] = x(m2_row(v[4]), v[0]);
    v[0] = x(m2_row(b), v[4]);
    v[4] = x(m2_row(v[4]), v[3]);
    v[3] = x(m2_row(v[3]), v[2]);
    v[2] = x(m2_row(v[2]), v[1]);
    v[1] = x(m2_row(v[1]), b);
}

/// The message block's contribution to `MI5`, in rows: `M2^j(m)` in sub-state `j`.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn msg_row(block: &[u8; 32]) -> Row {
    let bswap = ld4(BSWAP_X8.first_chunk().unwrap());
    // SAFETY: the loads read the 32 bytes of `block`.
    let m = unsafe {
        [_mm_loadu_si128(block.as_ptr().cast()), _mm_loadu_si128(block.as_ptr().add(16).cast())]
    };
    let mut r = [m.map(|x| _mm_shuffle_epi8(x, bswap)); 5];
    for j in 1..5 {
        r[j] = m2_row(r[j - 1]);
    }
    r
}

/// Transposes four rows of four words.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn tr4([a, b, c, d]: [__m128i; 4]) -> [__m128i; 4] {
    let t0 = _mm_unpacklo_epi32(a, b);
    let t1 = _mm_unpacklo_epi32(c, d);
    let t2 = _mm_unpackhi_epi32(a, b);
    let t3 = _mm_unpackhi_epi32(c, d);
    [
        _mm_unpacklo_epi64(t0, t1),
        _mm_unpackhi_epi64(t0, t1),
        _mm_unpacklo_epi64(t2, t3),
        _mm_unpackhi_epi64(t2, t3),
    ]
}

/// Round `R` of `Q_4` on sub-state 4 in a row `l` (words `0..4`), `h` (words `4..8`).
/// `SUB_CRUMB` runs on four vectors holding words `k` and `k + 5` (`k + 1` for `k = 3`) in
/// lanes 0 and 1; `MIX_WORD` on `l` and `h`.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn round_s4<const R: usize>(l: __m128i, h: __m128i) -> (__m128i, __m128i) {
    use x4::{lit, mix_word, raw, sub_crumb, to, val, xor};
    // w5 w6 w7 w4.
    let hr = _mm_shuffle_epi32::<0x39>(h);
    // w0 w5 w1 w6, w2 w7 w3 w4.
    let x = _mm_unpacklo_epi32(l, hr);
    let y = _mm_unpackhi_epi32(l, hr);
    let [x, x1, y, y1] = [x, _mm_shuffle_epi32::<0x4e>(x), y, _mm_shuffle_epi32::<0x4e>(y)];
    let o = sub_crumb([x, x1, y, y1].map(|v| lit(v, false, 0)));
    // Three of the four come out complemented: complement the fourth.
    let [o0, o1, o2, o3] = o.map(|o| _mm_castsi128_ps(val(o, true)));
    let x = _mm_movelh_ps(o0, o1);
    let y = _mm_movelh_ps(o2, o3);
    let l = _mm_castps_si128(_mm_shuffle_ps::<0x88>(x, y));
    let h = _mm_shuffle_epi32::<0x93>(_mm_castps_si128(_mm_shuffle_ps::<0xdd>(x, y)));
    let (u, v) = mix_word::<R>(lit(l, true, 0), lit(h, true, 0));
    let u = xor(to(u, 0), lit(ld4(&RC_S4[R][0]), false, 0));
    let v = xor(to(v, 0), lit(ld4(&RC_S4[R][1]), true, 0));
    (raw(u, false), raw(v, false))
}

/// `P5` on a state in rows: `TWEAK5`, then the eight rounds of `Q_0..Q_3` on the transposed
/// sub-states `0..4` (lane `j` for sub-state `j`) and of `Q_4` on the row.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn p5_row(v: &mut Row) {
    let c = |n: usize| _mm_cvtsi32_si128(n as i32);
    for (j, [_, h]) in v.iter_mut().enumerate().skip(1) {
        *h = _mm_or_si128(_mm_sll_epi32(*h, c(j)), _mm_srl_epi32(*h, c(32 - j)));
    }
    let t0 = tr4([v[0][0], v[1][0], v[2][0], v[3][0]]);
    let t1 = tr4([v[0][1], v[1][1], v[2][1], v[3][1]]);
    let mut w = [t0[0], t0[1], t0[2], t0[3], t1[0], t1[1], t1[2], t1[3]];
    for (k, x) in w.iter_mut().enumerate() {
        *x = x4::val(x4::lit(*x, false, 0), NEG[k]);
    }
    // Unrolled: the rounds differ in their constants and frames.
    macro_rules! rounds {
        ($($r:literal)*) => { $(w = x4::round::<$r>(w, ld4(&NRC_X4[$r][0]), ld4(&NRC_X4[$r][1]));)* };
    }
    rounds!(0 1 2 3 4 5 6 7);
    for (k, x) in w.iter_mut().enumerate() {
        *x = x4::val(x4::lit(*x, NEG[k], 0), false);
    }
    let t0 = tr4([w[0], w[1], w[2], w[3]]);
    let t1 = tr4([w[4], w[5], w[6], w[7]]);
    for j in 0..4 {
        v[j] = [t0[j], t1[j]];
    }
    let (mut l, mut h) = (v[4][0], v[4][1]);
    macro_rules! rounds_s4 {
        ($($r:literal)*) => { $((l, h) = round_s4::<$r>(l, h);)* };
    }
    rounds_s4!(0 1 2 3 4 5 6 7);
    v[4] = [l, h];
}

/// Writes the XOR of the five sub-states, big-endian, to `out`.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn put_row(v: &Row, out: &mut [u8; 32]) {
    let x = xor_row;
    let s = x(x(x(v[0], v[1]), x(v[2], v[3])), v[4]);
    let bswap = ld4(BSWAP_X8.first_chunk().unwrap());
    // SAFETY: the stores write the 32 bytes of `out`.
    unsafe {
        _mm_storeu_si128(out.as_mut_ptr().cast(), _mm_shuffle_epi8(s[0], bswap));
        _mm_storeu_si128(out.as_mut_ptr().add(16).cast(), _mm_shuffle_epi8(s[1], bswap));
    }
}

/// Luffa-512 of `input` with 128-bit vectors: `MI5` on the sub-states as rows of two
/// vectors, `P5` on sub-states `0..4` bitsliced in four lanes and on sub-state 4 as a row.
///
/// # Safety
///
/// The CPU must support SSSE3 and SSE4.1.
#[target_feature(enable = "ssse3,sse4.1")]
pub(crate) unsafe fn hash_sse(input: &[u8; 64]) -> [u8; 64] {
    let blocks = input.as_chunks::<32>().0;
    let m0 = msg_row(&blocks[0]);
    let mut v: Row = core::array::from_fn(|j| xor_row(ld_row(&V1_ROW[j]), m0[j]));
    let m1 = msg_row(&blocks[1]);
    let mut out = [0; 64];
    // One loop, so that P5 is inlined once. The messages of rounds 2 to 5 are the second
    // block, the padding block and two zero blocks.
    for i in 0..5 {
        if i > 0 {
            mi5_row(&mut v);
            if i == 1 {
                for j in 0..5 {
                    v[j] = xor_row(v[j], m1[j]);
                }
            } else if i == 2 {
                for j in 0..5 {
                    v[j][j / 4] = _mm_xor_si128(v[j][j / 4], ld4(&PAD_X4[j]));
                }
            }
        }
        p5_row(&mut v);
        if i == 3 {
            put_row(&v, out.first_chunk_mut().unwrap());
        }
    }
    put_row(&v, out.last_chunk_mut().unwrap());
    out
}

/// Luffa-512 of `input` with 256-bit vectors: the five sub-states bitsliced in lanes
/// `0..5` of eight vectors.
///
/// # Safety
///
/// The CPU must support AVX2.
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn hash_avx2(input: &[u8; 64]) -> [u8; 64] {
    let blocks = input.as_chunks::<32>().0;
    let m0 = msg_x8(&blocks[0]);
    let mut w: [__m256i; 8] = core::array::from_fn(|k| _mm256_xor_si256(ld8(&V1_X8[k]), m0[k]));
    let m1 = msg_x8(&blocks[1]);
    let mut out = [0; 64];
    // One loop, so that P5 is inlined once. The messages of rounds 2 to 5 are the second
    // block, the padding block and two zero blocks.
    for i in 0..5 {
        if i > 0 {
            mi5_x8(&mut w);
            if i == 1 {
                for k in 0..8 {
                    w[k] = _mm256_xor_si256(w[k], m1[k]);
                }
            } else if i == 2 {
                for k in 0..5 {
                    w[k] = _mm256_xor_si256(w[k], ld8(&PAD_X8[k]));
                }
            }
        }
        p5_x8(&mut w);
        if i == 3 {
            put_x8(&w, out.first_chunk_mut().unwrap());
        }
    }
    put_x8(&w, out.last_chunk_mut().unwrap());
    out
}
