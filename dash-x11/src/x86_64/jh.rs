//! JH-512 of one 64-byte message on vector registers (Core: `crypto/x11/jh.c`, built
//! small-footprint). Same computation as `portable::jh`: two compressions, of the message and of
//! the padding block, each `H ^= m` (first half), E8 (42 rounds), `H ^= m` (second half).
//!
//! The state is sph's eight 128-bit words `H0..H7` (`hNh`, `hNl` as little-endian 64-bit halves,
//! the constants after `C64e`). A round `r` is sph's `SLu(r, r % 7)`: `Sb` on the bit-slices of
//! the even words `H0, H2, H4, H6` and of the odd words `H1, H3, H5, H7` (the constant bit
//! selects the S-box), `Lb` mixing the two groups, then `W_{r % 7}` on the odd words: a swap of
//! adjacent 1-, 2-, 4-, 8-, 16-, 32- or 64-bit groups.
//!
//! - SSE: one register per word. `W0..W2` are shifts and masks, `W3`, `W4` byte shuffles,
//!   `W5`, `W6` dword shuffles. Words may be held complemented (tracked at compile time, see
//!   [`FLAGS`]), which removes `Sb`'s two `~x3` per round.
//! - AVX2: register `k` holds `[H2k.h, H2k+1.h, H2k.l, H2k+1.l]` (64-bit lanes), so one `Sb`
//!   covers both groups, `Lb` moves halves within 128-bit lanes by byte shifts, and `W` acts on
//!   the odd 64-bit lanes only (variable shifts, byte or dword shuffles, a qword permute).
//!   It keeps `~x3` (one xor per round): `Lb` would make a complement differ between the
//!   lanes of a register, where one instruction cannot absorb it.
//!
//! Both read each round constant from two copies of the table, so that LLVM folds both reads
//! into the instructions.

use core::arch::x86_64::*;

use crate::portable::jh::{C, IV512, PAD};

/// A table of 64-bit words aligned for vector loads.
#[repr(C, align(32))]
struct Aligned<const N: usize>([u64; N]);

/// `C64e` on little-endian CPUs.
const fn c64e<const N: usize>(mut a: [u64; N]) -> [u64; N] {
    let mut i = 0;
    while i < N {
        a[i] = a[i].swap_bytes();
        i += 1;
    }
    a
}

/// Reorders each group of four words `a, b, c, d` to `a, c, b, d`: from `[H2k, H2k+1]` (or a
/// round's even and odd constants) to the AVX2 layout.
const fn interleave<const N: usize>(mut a: [u64; N]) -> [u64; N] {
    let mut i = 0;
    while i < N {
        let t = a[i + 1];
        a[i + 1] = a[i + 2];
        a[i + 2] = t;
        i += 4;
    }
    a
}

/// Each group of `UNIT` words twice. The S-box reads each constant twice; two copies make two
/// loads, which LLVM folds into the instructions instead of loading one into a register.
/// Measured with rustc 1.98: 84 fewer instructions per call with AVX2, about 100 with SSE, for
/// 1.3 KiB more table each. Codegen steering only; drop it if a rustc upgrade stops paying.
const fn twice<const UNIT: usize>(a: [u64; 168]) -> [u64; 336] {
    let mut b = [0; 336];
    let mut i = 0;
    while i < 336 {
        b[i] = a[i / (2 * UNIT) * UNIT + i % UNIT];
        i += 1;
    }
    b
}

/// Round constants, round `r` at `8 * r`: the even words' `[h, l]` twice, then the odd words'.
static C_SSE: Aligned<336> = Aligned(twice::<2>(c64e(C)));
/// Round constants in the AVX2 layout, round `r` at `8 * r`, twice.
static C_AVX2: Aligned<336> = Aligned(twice::<4>(interleave(c64e(C))));
/// `H0..H7`, two words each, complemented as [`FLAGS`] says.
static IV_SSE: Aligned<16> = Aligned(complement_iv(c64e(IV512)));
/// The four AVX2 registers.
static IV_AVX2: Aligned<16> = Aligned(interleave(c64e(IV512)));

/// Complements the IV words as [`FLAGS`] says for the start of E8.
const fn complement_iv(mut iv: [u64; 16]) -> [u64; 16] {
    let (fe, fo) = FLAGS[0];
    let mut k = 0;
    while k < 4 {
        // H2k is words 4k, 4k + 1; H2k+1 is 4k + 2, 4k + 3.
        let (me, mo) = ((fe >> k & 1) as u64, (fo >> k & 1) as u64);
        iv[4 * k] ^= me.wrapping_neg();
        iv[4 * k + 1] ^= me.wrapping_neg();
        iv[4 * k + 2] ^= mo.wrapping_neg();
        iv[4 * k + 3] ^= mo.wrapping_neg();
        k += 1;
    }
    iv
}

/// Vector `i` of 128 bits of `t`.
#[inline]
#[target_feature(enable = "sse2")]
fn load128<const N: usize>(t: &Aligned<N>, i: usize) -> __m128i {
    let w: &[u64; 2] = &t.0.as_chunks().0[i];
    // SAFETY: `w` is 16 bytes at a multiple of 16 bytes from the 32-byte aligned start of `t`.
    unsafe { _mm_load_si128(w.as_ptr().cast()) }
}

/// Vector `i` of 256 bits of `t`.
#[inline]
#[target_feature(enable = "avx2")]
fn load256<const N: usize>(t: &Aligned<N>, i: usize) -> __m256i {
    let w: &[u64; 4] = &t.0.as_chunks().0[i];
    // SAFETY: `w` is 32 bytes at a multiple of 32 bytes from the 32-byte aligned start of `t`.
    unsafe { _mm256_load_si256(w.as_ptr().cast()) }
}

/// `a & b` of two values held complemented if `fa`, `fb`, as one instruction: the result and
/// whether it is held complemented.
#[inline]
#[target_feature(enable = "sse2")]
fn and128(a: __m128i, fa: bool, b: __m128i, fb: bool) -> (__m128i, bool) {
    match (fa, fb) {
        (false, false) => (_mm_and_si128(a, b), false),
        (true, false) => (_mm_andnot_si128(a, b), false),
        (false, true) => (_mm_andnot_si128(b, a), false),
        (true, true) => (_mm_or_si128(a, b), true),
    }
}

/// `a | b`, as [`and128`]: `!(!a & !b)`.
#[inline]
#[target_feature(enable = "sse2")]
fn or128(a: __m128i, fa: bool, b: __m128i, fb: bool) -> (__m128i, bool) {
    let (v, f) = and128(a, !fa, b, !fb);
    (v, !f)
}

/// sph's `Sb` on four 128-bit bit-slices, `x[k]` held complemented if bit `k` of `F` is set
/// (the output likewise, as [`sb_flags`] says). Every complement is folded into the choice of
/// and, and-not or or, so `x3 = ~x3` costs nothing.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn sb128<const F: u8>(
    [mut x0, mut x1, mut x2, mut x3]: [__m128i; 4],
    [c, c2]: [__m128i; 2],
) -> [__m128i; 4] {
    let [mut f0, mut f1, mut f2, mut f3] = [0, 1, 2, 3].map(|k| F >> k & 1 != 0);
    let xor = |x: &mut __m128i, f: &mut bool, (t, ft): (__m128i, bool)| {
        *x = _mm_xor_si128(*x, t);
        *f ^= ft;
    };
    f3 = !f3;
    xor(&mut x0, &mut f0, and128(c, false, x2, !f2));
    let (mut tmp, mut ftmp) = (c2, false);
    xor(&mut tmp, &mut ftmp, and128(x0, f0, x1, f1));
    xor(&mut x0, &mut f0, and128(x2, f2, x3, f3));
    xor(&mut x3, &mut f3, and128(x1, !f1, x2, f2));
    xor(&mut x1, &mut f1, and128(x0, f0, x2, f2));
    xor(&mut x2, &mut f2, and128(x0, f0, x3, !f3));
    xor(&mut x0, &mut f0, or128(x1, f1, x3, f3));
    xor(&mut x3, &mut f3, and128(x1, f1, x2, f2));
    xor(&mut x2, &mut f2, (tmp, ftmp));
    xor(&mut x1, &mut f1, and128(tmp, ftmp, x0, f0));
    [x0, x1, x2, x3]
}

/// The complements of [`sb128`]'s output (bit `k` for `x[k]`) given those of its input: the
/// same gates on flags (`a & b` is complemented if both are, `a | b` if either is).
const fn sb_flags(f: u8) -> u8 {
    let [mut f0, mut f1, mut f2, mut f3] = [f & 1, f >> 1 & 1, f >> 2 & 1, f >> 3 & 1];
    // `x3 = ~x3`; `x0 ^= c & ~x2` adds no complement (`c` is held plain).
    f3 ^= 1;
    let tmp = f0 & f1;
    f0 ^= f2 & f3;
    f3 ^= (f1 ^ 1) & f2;
    f1 ^= f0 & f2;
    f2 ^= f0 & (f3 ^ 1);
    f0 ^= f1 | f3;
    f3 ^= f1 & f2;
    f2 ^= tmp;
    f1 ^= tmp & f0;
    f0 | f1 << 1 | f2 << 2 | f3 << 3
}

/// `Lb`'s map `(v1, v2, v3 ^ v0, v0)` on the complement bits of four words.
const fn lb_m(v: u8) -> u8 {
    v >> 1 & 3 | ((v >> 3 ^ v) & 1) << 2 | (v & 1) << 3
}

/// The complements of the even and odd words after one round, from those before (`W` keeps
/// them).
const fn round_flags((fe, fo): (u8, u8)) -> (u8, u8) {
    let (ge, go) = (sb_flags(fe), sb_flags(fo));
    let o = go ^ lb_m(ge);
    (ge ^ lb_m(o), o)
}

/// The complements of the even and odd words at the start of each of seven rounds: the one
/// state (found by search) that returns to itself after seven rounds, so that every
/// iteration of the seven-round loop runs the same code.
const FLAGS: [(u8, u8); 7] = {
    let mut s = 0;
    loop {
        assert!(s < 256, "no seven-round cycle");
        let mut f = [(s as u8 & 15, (s >> 4) as u8); 8];
        let mut r = 0;
        while r < 7 {
            f[r + 1] = round_flags(f[r]);
            r += 1;
        }
        if f[7].0 == f[0].0 && f[7].1 == f[0].1 {
            break [f[0], f[1], f[2], f[3], f[4], f[5], f[6]];
        }
        s += 1;
    }
};

/// sph's `Sb` on four 256-bit bit-slices.
#[inline]
#[target_feature(enable = "avx2")]
fn sb256(
    [mut x0, mut x1, mut x2, mut x3]: [__m256i; 4],
    [c, c2]: [__m256i; 2],
    ones: __m256i,
) -> [__m256i; 4] {
    x3 = _mm256_xor_si256(x3, ones);
    x0 = _mm256_xor_si256(x0, _mm256_andnot_si256(x2, c));
    let tmp = _mm256_xor_si256(c2, _mm256_and_si256(x0, x1));
    x0 = _mm256_xor_si256(x0, _mm256_and_si256(x2, x3));
    x3 = _mm256_xor_si256(x3, _mm256_andnot_si256(x1, x2));
    x1 = _mm256_xor_si256(x1, _mm256_and_si256(x0, x2));
    x2 = _mm256_xor_si256(x2, _mm256_andnot_si256(x3, x0));
    x0 = _mm256_xor_si256(x0, _mm256_or_si256(x1, x3));
    x3 = _mm256_xor_si256(x3, _mm256_and_si256(x1, x2));
    x1 = _mm256_xor_si256(x1, _mm256_and_si256(tmp, x0));
    x2 = _mm256_xor_si256(x2, tmp);
    [x0, x1, x2, x3]
}

/// The masks of `W0..W2` and the byte shuffles of `W3`, `W4`, hidden from LLVM by
/// `black_box` so that it keeps one `pshufb` for `W4` and the plain mask-and-shift form of `Wz`
/// (its rewrites cost register copies with two-operand SSE). Measured with rustc 1.98: about
/// 200 fewer instructions per call. Codegen steering only; it never changes a value.
#[derive(Clone, Copy)]
struct WConsts([__m128i; 5]);

impl WConsts {
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn new() -> Self {
        core::hint::black_box(WConsts([
            _mm_set1_epi64x(0x5555_5555_5555_5555),
            _mm_set1_epi64x(0x3333_3333_3333_3333),
            _mm_set1_epi64x(0x0F0F_0F0F_0F0F_0F0F),
            _mm_setr_epi8(1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14),
            _mm_setr_epi8(2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13),
        ]))
    }
}

/// sph's `Wz`: swaps the adjacent `N`-bit groups selected by `c`.
#[inline]
#[target_feature(enable = "sse2")]
fn wz128<const N: i32>(x: __m128i, c: __m128i) -> __m128i {
    _mm_or_si128(_mm_and_si128(_mm_srli_epi64::<N>(x), c), _mm_slli_epi64::<N>(_mm_and_si128(x, c)))
}

/// `W_RO` on one word.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn w128<const RO: u32>(x: __m128i, k: &WConsts) -> __m128i {
    match RO {
        0 => wz128::<1>(x, k.0[0]),
        1 => wz128::<2>(x, k.0[1]),
        2 => wz128::<4>(x, k.0[2]),
        3 => _mm_shuffle_epi8(x, k.0[3]),
        4 => _mm_shuffle_epi8(x, k.0[4]),
        5 => _mm_shuffle_epi32::<0xb1>(x),
        _ => _mm_shuffle_epi32::<0x4e>(x),
    }
}

/// sph's `Wz` on the odd 64-bit lanes: swaps the adjacent `N`-bit groups selected by `c` (the
/// even lanes shift by 0 under an all-ones mask).
#[inline]
#[target_feature(enable = "avx2")]
fn wz256<const N: i64>(x: __m256i, c: i64) -> __m256i {
    let n = _mm256_set_epi64x(N, 0, N, 0);
    let c = _mm256_set_epi64x(c, -1, c, -1);
    _mm256_or_si256(
        _mm256_and_si256(_mm256_srlv_epi64(x, n), c),
        _mm256_sllv_epi64(_mm256_and_si256(x, c), n),
    )
}

/// `W_RO` on the odd 64-bit lanes (the odd words' halves).
#[inline]
#[target_feature(enable = "avx2")]
fn w256<const RO: u32>(x: __m256i) -> __m256i {
    #[rustfmt::skip]
    let swap8 = _mm256_setr_epi8(
        0, 1, 2, 3, 4, 5, 6, 7, 9, 8, 11, 10, 13, 12, 15, 14,
        0, 1, 2, 3, 4, 5, 6, 7, 9, 8, 11, 10, 13, 12, 15, 14,
    );
    #[rustfmt::skip]
    let swap16 = _mm256_setr_epi8(
        0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 8, 9, 14, 15, 12, 13,
        0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 8, 9, 14, 15, 12, 13,
    );
    match RO {
        0 => wz256::<1>(x, 0x5555_5555_5555_5555),
        1 => wz256::<2>(x, 0x3333_3333_3333_3333),
        2 => wz256::<4>(x, 0x0F0F_0F0F_0F0F_0F0F),
        3 => _mm256_shuffle_epi8(x, swap8),
        4 => _mm256_shuffle_epi8(x, swap16),
        // Dwords 0, 1, 3, 2 of each 128-bit lane.
        5 => _mm256_shuffle_epi32::<0xb4>(x),
        // Qwords 0, 3, 2, 1.
        _ => _mm256_permute4x64_epi64::<0x6c>(x),
    }
}

/// Round `r` (sph `SLu(r, RO)`, `RO = r % 7`) on the even words `e` and the odd words `o`.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn round128<const RO: u32, const FE: u8, const FO: u8>(
    e: &mut [__m128i; 4],
    o: &mut [__m128i; 4],
    r: usize,
    k: &WConsts,
) {
    let c = |i| [load128(&C_SSE, i), load128(&C_SSE, i + 1)];
    let [e0, e1, e2, e3] = sb128::<FE>(*e, c(4 * r));
    let [mut o0, mut o1, mut o2, mut o3] = sb128::<FO>(*o, c(4 * r + 2));
    // Lb(e0, e1, e2, e3, o0, o1, o2, o3).
    o0 = _mm_xor_si128(o0, e1);
    o1 = _mm_xor_si128(o1, e2);
    o2 = _mm_xor_si128(o2, _mm_xor_si128(e3, e0));
    o3 = _mm_xor_si128(o3, e0);
    *e = [
        _mm_xor_si128(e0, o1),
        _mm_xor_si128(e1, o2),
        _mm_xor_si128(e2, _mm_xor_si128(o3, o0)),
        _mm_xor_si128(e3, o0),
    ];
    *o = [w128::<RO>(o0, k), w128::<RO>(o1, k), w128::<RO>(o2, k), w128::<RO>(o3, k)];
}

/// Round `r` (sph `SLu(r, RO)`, `RO = r % 7`) in the AVX2 layout.
#[inline]
#[target_feature(enable = "avx2")]
fn round256<const RO: u32>(x: &mut [__m256i; 4], r: usize, ones: __m256i) {
    let [mut x0, mut x1, mut x2, mut x3] =
        sb256(*x, [load256(&C_AVX2, 2 * r), load256(&C_AVX2, 2 * r + 1)], ones);
    // Lb: the odd words take the even ones (low to high 64 bits of each 128-bit lane), then the
    // even words take the new odd ones. Each step reads only the halves the other leaves.
    let up = |v| _mm256_bslli_epi128::<8>(v);
    x0 = _mm256_xor_si256(x0, up(x1));
    x1 = _mm256_xor_si256(x1, up(x2));
    x2 = _mm256_xor_si256(x2, up(_mm256_xor_si256(x3, x0)));
    x3 = _mm256_xor_si256(x3, up(x0));
    let down = |v| _mm256_bsrli_epi128::<8>(v);
    x0 = _mm256_xor_si256(x0, down(x1));
    x1 = _mm256_xor_si256(x1, down(x2));
    x2 = _mm256_xor_si256(x2, down(_mm256_xor_si256(x3, x0)));
    x3 = _mm256_xor_si256(x3, down(x0));
    *x = [w256::<RO>(x0), w256::<RO>(x1), w256::<RO>(x2), w256::<RO>(x3)];
}

/// One block with 128-bit vectors: sph `INPUT_BUF1`, `E8`, `INPUT_BUF2`.
#[inline]
#[target_feature(enable = "ssse3,sse4.1")]
fn compress128(e: &mut [__m128i; 4], o: &mut [__m128i; 4], block: &[u8; 64], k: &WConsts) {
    // SAFETY: each load reads 16 bytes inside `block`.
    let m: [__m128i; 4] =
        core::array::from_fn(|i| unsafe { _mm_loadu_si128(block.as_ptr().add(16 * i).cast()) });
    // H0..H3 are e0, o0, e1, o1; H4..H7 are e2, o2, e3, o3.
    e[0] = _mm_xor_si128(e[0], m[0]);
    o[0] = _mm_xor_si128(o[0], m[1]);
    e[1] = _mm_xor_si128(e[1], m[2]);
    o[1] = _mm_xor_si128(o[1], m[3]);
    for r in (0..42).step_by(7) {
        round128::<0, { FLAGS[0].0 }, { FLAGS[0].1 }>(e, o, r, k);
        round128::<1, { FLAGS[1].0 }, { FLAGS[1].1 }>(e, o, r + 1, k);
        round128::<2, { FLAGS[2].0 }, { FLAGS[2].1 }>(e, o, r + 2, k);
        round128::<3, { FLAGS[3].0 }, { FLAGS[3].1 }>(e, o, r + 3, k);
        round128::<4, { FLAGS[4].0 }, { FLAGS[4].1 }>(e, o, r + 4, k);
        round128::<5, { FLAGS[5].0 }, { FLAGS[5].1 }>(e, o, r + 5, k);
        round128::<6, { FLAGS[6].0 }, { FLAGS[6].1 }>(e, o, r + 6, k);
    }
    e[2] = _mm_xor_si128(e[2], m[0]);
    o[2] = _mm_xor_si128(o[2], m[1]);
    e[3] = _mm_xor_si128(e[3], m[2]);
    o[3] = _mm_xor_si128(o[3], m[3]);
}

/// One block in the AVX2 layout: sph `INPUT_BUF1`, `E8`, `INPUT_BUF2`.
#[inline]
#[target_feature(enable = "avx2")]
fn compress256(x: &mut [__m256i; 4], block: &[u8; 64], ones: __m256i) {
    // `[H2k, H2k+1]` to the AVX2 layout: qwords 0, 2, 1, 3.
    // SAFETY: each load reads 32 bytes inside `block`.
    let m: [__m256i; 2] = core::array::from_fn(|i| {
        _mm256_permute4x64_epi64::<0xd8>(unsafe {
            _mm256_loadu_si256(block.as_ptr().add(32 * i).cast())
        })
    });
    x[0] = _mm256_xor_si256(x[0], m[0]);
    x[1] = _mm256_xor_si256(x[1], m[1]);
    for r in (0..42).step_by(7) {
        round256::<0>(x, r, ones);
        round256::<1>(x, r + 1, ones);
        round256::<2>(x, r + 2, ones);
        round256::<3>(x, r + 3, ones);
        round256::<4>(x, r + 4, ones);
        round256::<5>(x, r + 5, ones);
        round256::<6>(x, r + 6, ones);
    }
    x[2] = _mm256_xor_si256(x[2], m[0]);
    x[3] = _mm256_xor_si256(x[3], m[1]);
}

/// JH-512 of `input` with 128-bit vectors.
///
/// # Safety
///
/// The CPU must support SSSE3 and SSE4.1.
#[target_feature(enable = "ssse3,sse4.1")]
pub(crate) unsafe fn hash_sse(input: &[u8; 64]) -> [u8; 64] {
    let mut e: [__m128i; 4] = core::array::from_fn(|k| load128(&IV_SSE, 2 * k));
    let mut o: [__m128i; 4] = core::array::from_fn(|k| load128(&IV_SSE, 2 * k + 1));
    let k = WConsts::new();
    compress128(&mut e, &mut o, input, &k);
    compress128(&mut e, &mut o, &PAD, &k);
    // Undo the complements of H4..H7.
    let (fe, fo) = FLAGS[0];
    let fix = |x, f: u8, k: u8| _mm_xor_si128(x, _mm_set1_epi64x(-((f >> k & 1) as i64)));
    let mut out = [0u8; 64];
    for (i, v) in [fix(e[2], fe, 2), fix(o[2], fo, 2), fix(e[3], fe, 3), fix(o[3], fo, 3)]
        .into_iter()
        .enumerate()
    {
        // SAFETY: the store writes 16 bytes inside `out`.
        unsafe { _mm_storeu_si128(out.as_mut_ptr().add(16 * i).cast(), v) };
    }
    out
}

/// JH-512 of `input` with 256-bit vectors.
///
/// # Safety
///
/// The CPU must support AVX2.
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn hash_avx2(input: &[u8; 64]) -> [u8; 64] {
    let mut x: [__m256i; 4] = core::array::from_fn(|k| load256(&IV_AVX2, k));
    // Opaque, so that LLVM keeps `Sb`'s complement as one xor instead of folding it into the
    // following and-nots at the cost of an extra xor (58 fewer instructions per call with
    // rustc 1.98; codegen steering only).
    let ones = core::hint::black_box(_mm256_set1_epi64x(-1));
    compress256(&mut x, input, ones);
    compress256(&mut x, &PAD, ones);
    let mut out = [0u8; 64];
    for (i, v) in x[2..].iter().enumerate() {
        // SAFETY: the store writes 32 bytes inside `out`.
        unsafe {
            _mm256_storeu_si256(
                out.as_mut_ptr().add(32 * i).cast(),
                _mm256_permute4x64_epi64::<0xd8>(*v),
            )
        };
    }
    out
}
