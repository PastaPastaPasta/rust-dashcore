//! SIMD-512 of one 64-byte message on vector registers (Core: `crypto/x11/simd.c`, the sph
//! full form). Same two compressions as `portable::simd`: the message zero-padded to 128 bytes
//! (twist `yoff_b_n`), then the bit-count block (twist `yoff_b_f`, whose `W` words are a
//! compile-time constant here).
//!
//! sph's `FFT256` and twist leave `q[k] = Σ_j x[j]·41^(j·k) + yoff[k]` (mod 257, `j < 64`
//! since the block's upper half is zero) in `-128..=128`, in natural order. The vector code
//! computes it as a 16 × 16 NTT on sixteen-lane `i16` vectors, with `j = j0 + 16·j1`,
//! `k = k0 + 16·k1`, `41^16 = 2` and `41^(j·k) = 41^(j0·k0) · 2^(j1·k0) · 2^(j0·k1)`:
//!
//! 1. lanes `j0`: `U[k0] = Σ_j1 x[j0 + 16·j1]·2^(j1·k0)`, shifts (Montgomery multiplications
//!    where a shift would overflow);
//! 2. a 16 × 16 transpose, to lanes `k0`;
//! 3. the twiddles `41^(j0·k0)` (Montgomery multiplications, which also reduce), and
//!    `yoff_b_n[k] = 41^(255·k)` as the input `j = 255`;
//! 4. lanes `k0`: `Y[k1] = Σ_j0 2^(j0·k1)·V[j0]`, two radix-4 stages, shifts only;
//! 5. the reduction to `-128..=128`: vector `k1` holds `q[16·k1..][..16]`.
//!
//! Every intermediate fits in `i16` for every input (the comments give the worst-case bounds,
//! by interval arithmetic over all inputs). sph's `W` words
//! of steps 0..16 are then `q[16·wbp..][..16] · 185` as they are; steps 16..32 pair `q[i]`
//! with `q[i + 128]`, times 233, with one shift and a blend.
//!
//! The state rows `A`, `B`, `C`, `D` (sph's `A0..A7` ...) are one 8-word vector each; a step
//! (`STEP2_BIG`) is lane-wise but for its `tA[ppb ^ n]`, a 32-bit lane shuffle.
//!
//! `hash_avx2` uses 256-bit vectors, `hash_sse` pairs of 128-bit ones; both expand the same
//! code (`simd512!`).

use core::arch::x86_64::*;

use crate::portable::simd::{ALPHA_TAB, PP8, WBP};

/// `T` on a 32-byte boundary: loads of these constants fold into SSE memory operands.
#[derive(Clone, Copy)]
#[repr(C, align(32))]
struct Al<T>(T);

/// sph's `IV512` ([`crate::portable::simd::IV512`]) as four aligned rows.
static IV512: [Al<[u32; 8]>; 4] = {
    let iv = crate::portable::simd::IV512;
    let mut rows = [Al([0u32; 8]); 4];
    let mut i = 0;
    while i < 32 {
        rows[i / 8].0[i % 8] = iv[i];
        i += 1;
    }
    rows
};

/// `STEP_OF[b]`: the step whose `W` comes from `wbp` block `b` (the inverse of `WBP`).
const STEP_OF: [usize; 32] = {
    let mut s = [0; 32];
    let mut i = 0;
    while i < 32 {
        s[WBP[i]] = i;
        i += 1;
    }
    s
};

/// `41^e mod 257`.
const fn alpha(e: usize) -> i32 {
    ALPHA_TAB[e % 256]
}

/// The representative of `v` mod 257 in `-128..=128`.
const fn canon(v: i32) -> i16 {
    let r = v.rem_euclid(257);
    (if r <= 128 {
        r
    } else {
        r - 257
    }) as i16
}

/// `257^-1 mod 2^16`, for the Montgomery multiplication.
const QINV: i16 = -255;

/// A Montgomery multiplier: the lanes `c` and `cq = c · 257^-1 mod 2^16`.
#[derive(Clone, Copy)]
#[repr(C, align(32))]
struct Mont {
    c: [i16; 16],
    cq: [i16; 16],
}

/// `mont(c)` for lanes `c[k0]`.
const fn mont(c: [i16; 16]) -> Mont {
    let mut cq = [0; 16];
    let mut i = 0;
    while i < 16 {
        cq[i] = c[i].wrapping_mul(QINV);
        i += 1;
    }
    Mont {
        c,
        cq,
    }
}

/// The twiddles of step 3: `TWIDDLE[j0][k0] = 41^(j0·k0)` (`j0 = 0` unused).
static TWIDDLE: [Mont; 16] = {
    let mut t = [mont([0; 16]); 16];
    let mut j0 = 0;
    while j0 < 16 {
        let mut c = [0; 16];
        let mut k0 = 0;
        while k0 < 16 {
            c[k0] = canon(alpha(j0 * k0));
            k0 += 1;
        }
        t[j0] = mont(c);
        j0 += 1;
    }
    t
};

/// The twiddles `2^e` of step 4, `e = n0·a` for `n0, a` in `1..4`, as `TWIDDLE4[e]`.
static TWIDDLE4: [Mont; 10] = {
    let mut t = [mont([0; 16]); 10];
    let mut e = 0;
    while e < 10 {
        t[e] = mont([canon(1 << e); 16]);
        e += 1;
    }
    t
};

/// `yoff_b_n[k] = 41^(255·k)`: the input `x[255] = 1`, which step 4 adds to `V[15]` (lanes
/// `k0`: `41^(255·k0)`).
static YOFF_N: Al<[i16; 16]> = {
    let mut y = [0; 16];
    let mut k0 = 0;
    while k0 < 16 {
        y[k0] = canon(alpha(255 * k0));
        k0 += 1;
    }
    Al(y)
};

/// The `W` words of the last block: `x = LE32(512)` gives `Σ_j x[j]·41^(j·k) = 2·41^k`, and
/// `yoff_b_f[k] = 41^(253·k) + 41^(255·k)`; then sph's `WBREAD`s (`INNER`, 185 then 233).
static W_FINAL: [Al<[u32; 8]>; 32] = {
    let mut q = [0i32; 256];
    let mut k = 0;
    while k < 256 {
        q[k] = canon(2 * alpha(k) + alpha(253 * k) + alpha(255 * k)) as i32;
        k += 1;
    }
    let mut w = [Al([0; 8]); 32];
    let mut step = 0;
    while step < 32 {
        let v = 16 * WBP[step];
        let mut n = 0;
        while n < 8 {
            let i = v + 2 * n;
            let (l, h, m) = match step / 8 {
                0 | 1 => (q[i], q[i + 1], 185),
                2 => (q[i - 256], q[i - 128], 233),
                _ => (q[i - 383], q[i - 255], 233),
            };
            w[step].0[n] = ((l * m) as u32 & 0xFFFF) | ((h * m) as u32) << 16;
            n += 1;
        }
        step += 1;
    }
    w
};

/// sph's `ONE_ROUND_BIG(ri, isp, p0, p1, p2, p3)`: eight `STEP2_BIG`, four `IF`, four `MAJ`;
/// `W` from `word::<$final>`.
macro_rules! one_round_big {
    (
        $s:ident, $w:ident, $final:ident,
        $i:literal, $p0:literal, $p1:literal, $p2:literal, $p3:literal
    ) => {
        step::<false, $p0, { 32 - $p0 }, $p1, { 32 - $p1 }, { PP8[$i % 7] }>(
            &mut $s,
            word::<$final>($w, 8 * $i),
        );
        step::<false, $p1, { 32 - $p1 }, $p2, { 32 - $p2 }, { PP8[($i + 1) % 7] }>(
            &mut $s,
            word::<$final>($w, 8 * $i + 1),
        );
        step::<false, $p2, { 32 - $p2 }, $p3, { 32 - $p3 }, { PP8[($i + 2) % 7] }>(
            &mut $s,
            word::<$final>($w, 8 * $i + 2),
        );
        step::<false, $p3, { 32 - $p3 }, $p0, { 32 - $p0 }, { PP8[($i + 3) % 7] }>(
            &mut $s,
            word::<$final>($w, 8 * $i + 3),
        );
        step::<true, $p0, { 32 - $p0 }, $p1, { 32 - $p1 }, { PP8[($i + 4) % 7] }>(
            &mut $s,
            word::<$final>($w, 8 * $i + 4),
        );
        step::<true, $p1, { 32 - $p1 }, $p2, { 32 - $p2 }, { PP8[($i + 5) % 7] }>(
            &mut $s,
            word::<$final>($w, 8 * $i + 5),
        );
        step::<true, $p2, { 32 - $p2 }, $p3, { 32 - $p3 }, { PP8[($i + 6) % 7] }>(
            &mut $s,
            word::<$final>($w, 8 * $i + 6),
        );
        step::<true, $p3, { 32 - $p3 }, $p0, { 32 - $p0 }, { PP8[($i + 7) % 7] }>(
            &mut $s,
            word::<$final>($w, 8 * $i + 7),
        );
    };
}

/// The algorithm, over a backend module's `V` (sixteen `i16` or eight `u32` lanes) and its
/// primitives; `$feature` is the backend's `target_feature`.
macro_rules! simd512 {
    ($feature:literal) => {
        /// sph's `REDS1`: `x mod 257` in `-(x >> 8)..=255 - (x >> 8)`.
        #[inline]
        #[target_feature(enable = $feature)]
        fn reds1(x: V) -> V {
            sub16(and(x, splat16(255)), srai16::<8>(x))
        }

        /// `x · c mod 257` in `(x·c >> 16) - 128..=(x·c >> 16) + 129` (signed Montgomery,
        /// `2^16 ≡ 1`): `hi(x·c) - hi(m·257)`, `m = x·c·257^-1 mod 2^16`.
        #[inline]
        #[target_feature(enable = $feature)]
        fn mulmod(x: V, c: &Mont) -> V {
            let m = mullo16(x, ld16(&c.cq));
            sub16(mulhi16(x, ld16(&c.c)), mulhi16(m, splat16(257)))
        }

        /// One radix-4 butterfly with root `16 = 2^4`: `y[b] = Σ_n 16^(n·b)·v[n]`.
        #[inline]
        #[target_feature(enable = $feature)]
        fn dft4(v: [V; 4]) -> [V; 4] {
            let (ep, em) = (add16(v[0], v[2]), sub16(v[0], v[2]));
            let (op, om) = (add16(v[1], v[3]), slli16::<4>(sub16(v[1], v[3])));
            [add16(ep, op), add16(em, om), sub16(ep, op), sub16(em, om)]
        }

        /// sph's `FFT256` and twist of the message block (`yoff_b_n`), then its `W` words
        /// (`WBREAD(0, 0, 1, 185)`, `WBREAD(8, 0, 1, 185)`, `WBREAD(16, -256, -128, 233)`,
        /// `WBREAD(24, -383, -255, 233)`): `W` of step `i` is `w[i]`.
        #[inline]
        #[target_feature(enable = $feature)]
        fn expand(input: &[u8; 64]) -> [V; 32] {
            // Step 1, lanes j0. x[j0 + 16·j1] in 0..=255.
            let x = [widen(input, 0), widen(input, 1), widen(input, 2), widen(input, 3)];
            // a[m] (b[m]) = x[j0] + 2^(2m)·x[j0 + 32] (x[j0 + 16] + 2^(2m)·x[j0 + 48]), m < 8,
            // within -16320..=16575.
            let s2 = [x[2], slli16::<2>(x[2]), slli16::<4>(x[2]), slli16::<6>(x[2])];
            let s3 = [x[3], slli16::<2>(x[3]), slli16::<4>(x[3]), slli16::<6>(x[3])];
            let mut a = [x[0]; 8];
            let mut b = [x[1]; 8];
            for m in 0..4 {
                a[m] = add16(x[0], s2[m]);
                a[m + 4] = sub16(x[0], s2[m]);
                b[m] = add16(x[1], s3[m]);
                b[m + 4] = sub16(x[1], s3[m]);
            }
            // u[k0] = a[k0 % 8] ± 2^(k0 % 8)·b[k0 % 8], within -17340..=21675: the products
            // that would not fit are Montgomery ones.
            let p = &TWIDDLE4;
            let t = [
                b[0],
                slli16::<1>(b[1]),
                slli16::<2>(b[2]),
                mulmod(b[3], &p[3]),
                slli16::<4>(b[4]),
                mulmod(b[5], &p[5]),
                mulmod(b[6], &p[6]),
                mulmod(b[7], &p[7]),
            ];
            let mut u = [x[0]; 16];
            for m in 0..8 {
                u[m] = add16(a[m], t[m]);
                u[m + 8] = sub16(a[m], t[m]);
            }
            // Step 2: v[j0], lanes k0.
            let v = transpose(u);
            // Step 3: within -84..=323 (v[0], sph's REDS1), -168..=169, -237..=246 (v[15]).
            let t = &TWIDDLE;
            let v = [
                reds1(v[0]),
                mulmod(v[1], &t[1]),
                mulmod(v[2], &t[2]),
                mulmod(v[3], &t[3]),
                mulmod(v[4], &t[4]),
                mulmod(v[5], &t[5]),
                mulmod(v[6], &t[6]),
                mulmod(v[7], &t[7]),
                mulmod(v[8], &t[8]),
                mulmod(v[9], &t[9]),
                mulmod(v[10], &t[10]),
                mulmod(v[11], &t[11]),
                mulmod(v[12], &t[12]),
                mulmod(v[13], &t[13]),
                mulmod(v[14], &t[14]),
                add16(mulmod(v[15], &t[15]), ld16(&YOFF_N.0)),
            ];
            // Step 4. j0 = n0 + 4·n1, k1 = a + 4·b: the DFT4s over n1 (within -6551..=6514),
            // the twiddles 2^(n0·a) (now within -133..=134 but for n0 = 0 or a = 0), the
            // DFT4s over n0 (within -22223..=22223).
            let mut s = [[x[0]; 4]; 4];
            for n0 in 0..4 {
                s[n0] = dft4([v[n0], v[n0 + 4], v[n0 + 8], v[n0 + 12]]);
            }
            for n0 in 1..4 {
                for a in 1..4 {
                    s[n0][a] = mulmod(s[n0][a], &TWIDDLE4[n0 * a]);
                }
            }
            // Step 5, for the four vectors k1 = a + 4·b at a time.
            let mut w = [x[0]; 32];
            let col = |a: usize| [s[0][a], s[1][a], s[2][a], s[3][a]];
            finish(&mut w, 0, dft4(col(0)));
            finish(&mut w, 1, dft4(col(1)));
            finish(&mut w, 2, dft4(col(2)));
            finish(&mut w, 3, dft4(col(3)));
            w
        }

        /// Step 5 for `y[b] = Y[a + 4·b]`: sph's REDS1 (`-127..=383` for any input), minus 257
        /// above 128, then the `W` words. Word n of the 233 ones pairs q[16·t + 2n] (+ 1) with
        /// q[16·t + 128 + 2n] (+ 1): here t = a and a + 4.
        #[inline]
        #[target_feature(enable = $feature)]
        fn finish(w: &mut [V; 32], a: usize, mut y: [V; 4]) {
            for (b, q) in y.iter_mut().enumerate() {
                let z = reds1(*q);
                *q = sub16(z, and(cmpgt16(z, splat16(128)), splat16(257)));
                w[STEP_OF[a + 4 * b]] = mullo16(*q, splat16(185));
            }
            for (t, lo, hi) in [(a, y[0], y[2]), (a + 4, y[1], y[3])] {
                let (lo, hi) = (mullo16(lo, splat16(233)), mullo16(hi, splat16(233)));
                w[STEP_OF[t + 16]] = blend_hi16(lo, slli32::<16>(hi));
                w[STEP_OF[t + 24]] = blend_hi16(srli32::<16>(lo), hi);
            }
        }

        /// Step `i`'s `W`: `w[i]`, or the last block's if `FINAL`.
        #[inline]
        #[target_feature(enable = $feature)]
        fn word<const FINAL: bool>(w: &[V; 32], i: usize) -> V {
            if FINAL {
                ld32(&W_FINAL[i].0)
            } else {
                w[i]
            }
        }

        /// sph's `IF`.
        #[inline]
        #[target_feature(enable = $feature)]
        fn if_(x: V, y: V, z: V) -> V {
            xor(and(xor(y, z), x), z)
        }

        /// sph's `MAJ`.
        #[inline]
        #[target_feature(enable = $feature)]
        fn maj(x: V, y: V, z: V) -> V {
            or(and(x, y), and(or(x, y), z))
        }

        /// sph's `STEP2_BIG` on `[A, B, C, D]`: `fun` is `MAJ` if `MAJ` else `IF`, rotations
        /// `R` and `S` (`RC = 32 - R`, `SC = 32 - S`), `ppb` is `PP`.
        #[inline]
        #[target_feature(enable = $feature)]
        fn step<
            const MAJ: bool,
            const R: i32,
            const RC: i32,
            const S: i32,
            const SC: i32,
            const PP: usize,
        >(
            s: &mut [V; 4],
            w: V,
        ) {
            let [a, b, c, d] = *s;
            let ta = rotl::<R, RC>(a);
            let f = if MAJ {
                maj(a, b, c)
            } else {
                if_(a, b, c)
            };
            let tt = add32(add32(d, w), f);
            *s = [add32(rotl::<S, SC>(tt), perm::<PP>(ta)), ta, b, c];
        }

        /// `compress_big` after the message xor: sph's four `ONE_ROUND_BIG` and the
        /// feed-forward `STEP2_BIG`s with the state `saved` from before the xor. `W` is `w`, or
        /// the last block's (`W_FINAL`) if `FINAL`.
        #[inline]
        #[target_feature(enable = $feature)]
        fn compress<const FINAL: bool>(mut s: [V; 4], saved: [V; 4], w: &[V; 32]) -> [V; 4] {
            one_round_big!(s, w, FINAL, 0, 3, 23, 17, 27);
            one_round_big!(s, w, FINAL, 1, 28, 19, 22, 7);
            one_round_big!(s, w, FINAL, 2, 29, 9, 15, 5);
            one_round_big!(s, w, FINAL, 3, 4, 13, 10, 25);
            step::<false, 4, 28, 13, 19, { PP8[4] }>(&mut s, saved[0]);
            step::<false, 13, 19, 10, 22, { PP8[5] }>(&mut s, saved[1]);
            step::<false, 10, 22, 25, 7, { PP8[6] }>(&mut s, saved[2]);
            step::<false, 25, 7, 4, 28, { PP8[0] }>(&mut s, saved[3]);
            s
        }

        /// SIMD-512 of a 64-byte message.
        #[target_feature(enable = $feature)]
        pub(super) fn hash(input: &[u8; 64]) -> [u8; 64] {
            let iv = [ld32(&IV512[0].0), ld32(&IV512[1].0), ld32(&IV512[2].0), ld32(&IV512[3].0)];
            let msg = [load32(input, 0), load32(input, 1)];
            let s = [xor(iv[0], msg[0]), xor(iv[1], msg[1]), iv[2], iv[3]];
            let w = expand(input);
            let s = compress::<false>(s, iv, &w);
            let count = ld32(&[512, 0, 0, 0, 0, 0, 0, 0]);
            let s = compress::<true>([xor(s[0], count), s[1], s[2], s[3]], s, &w);
            let mut out = [0; 64];
            store(&mut out, s[0], s[1]);
            out
        }
    };
}

/// 256-bit vectors: `V` is one `__m256i`.
mod avx2 {
    use super::*;

    type V = __m256i;

    macro_rules! lanewise {
        ($($name:ident = $op:ident;)*) => {$(
            #[inline]
            #[target_feature(enable = "avx2")]
            fn $name(a: V, b: V) -> V {
                $op(a, b)
            }
        )*};
    }

    lanewise! {
        add16 = _mm256_add_epi16;
        sub16 = _mm256_sub_epi16;
        mullo16 = _mm256_mullo_epi16;
        mulhi16 = _mm256_mulhi_epi16;
        cmpgt16 = _mm256_cmpgt_epi16;
        add32 = _mm256_add_epi32;
        and = _mm256_and_si256;
        or = _mm256_or_si256;
        xor = _mm256_xor_si256;
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn slli16<const N: i32>(a: V) -> V {
        _mm256_slli_epi16::<N>(a)
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn srai16<const N: i32>(a: V) -> V {
        _mm256_srai_epi16::<N>(a)
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn slli32<const N: i32>(a: V) -> V {
        _mm256_slli_epi32::<N>(a)
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn srli32<const N: i32>(a: V) -> V {
        _mm256_srli_epi32::<N>(a)
    }

    /// Rotates each 32-bit lane left by `N` (`M = 32 - N`).
    #[inline]
    #[target_feature(enable = "avx2")]
    fn rotl<const N: i32, const M: i32>(a: V) -> V {
        _mm256_or_si256(_mm256_slli_epi32::<N>(a), _mm256_srli_epi32::<M>(a))
    }

    /// The low 16 bits of each 32-bit lane of `a`, the high 16 bits of `b`.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn blend_hi16(a: V, b: V) -> V {
        _mm256_blend_epi16::<0xAA>(a, b)
    }

    /// Lane `n` is `a[n ^ PP]` (32-bit lanes).
    #[inline]
    #[target_feature(enable = "avx2")]
    fn perm<const PP: usize>(a: V) -> V {
        match PP {
            1 => _mm256_shuffle_epi32::<0xB1>(a),
            2 => _mm256_shuffle_epi32::<0x4E>(a),
            3 => _mm256_shuffle_epi32::<0x1B>(a),
            4 => _mm256_permute4x64_epi64::<0x4E>(a),
            5 => _mm256_permutevar8x32_epi32(a, _mm256_setr_epi32(5, 4, 7, 6, 1, 0, 3, 2)),
            6 => _mm256_permute4x64_epi64::<0x1B>(a),
            _ => _mm256_permutevar8x32_epi32(a, _mm256_setr_epi32(7, 6, 5, 4, 3, 2, 1, 0)),
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn splat16(c: i16) -> V {
        _mm256_set1_epi16(c)
    }

    /// The lanes `c`.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn ld16(c: &[i16; 16]) -> V {
        // SAFETY: the load reads the 32 bytes of `c`.
        unsafe { _mm256_loadu_si256(c.as_ptr().cast()) }
    }

    /// The words `c`.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn ld32(c: &[u32; 8]) -> V {
        // SAFETY: the load reads the 32 bytes of `c`.
        unsafe { _mm256_loadu_si256(c.as_ptr().cast()) }
    }

    /// Bytes `16·i..16·i + 16` of `x` as `i16` lanes.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn widen(x: &[u8; 64], i: usize) -> V {
        let p = x[16 * i..][..16].as_ptr();
        // SAFETY: the load reads 16 bytes inside `x`.
        _mm256_cvtepu8_epi16(unsafe { _mm_loadu_si128(p.cast()) })
    }

    /// Words `8·i..8·i + 8` of `x`, little-endian.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn load32(x: &[u8; 64], i: usize) -> V {
        let p = x[32 * i..][..32].as_ptr();
        // SAFETY: the load reads 32 bytes inside `x`.
        unsafe { _mm256_loadu_si256(p.cast()) }
    }

    /// `a` then `b`, little-endian.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn store(out: &mut [u8; 64], a: V, b: V) {
        // SAFETY: the stores write 64 bytes inside `out`.
        unsafe {
            _mm256_storeu_si256(out.as_mut_ptr().cast(), a);
            _mm256_storeu_si256(out.as_mut_ptr().add(32).cast(), b);
        }
    }

    /// Transposes the 8 × 8 `i16` matrix in each 128-bit half of `r`.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn transpose8(r: [V; 8]) -> [V; 8] {
        let b = [
            _mm256_unpacklo_epi16(r[0], r[1]),
            _mm256_unpackhi_epi16(r[0], r[1]),
            _mm256_unpacklo_epi16(r[2], r[3]),
            _mm256_unpackhi_epi16(r[2], r[3]),
            _mm256_unpacklo_epi16(r[4], r[5]),
            _mm256_unpackhi_epi16(r[4], r[5]),
            _mm256_unpacklo_epi16(r[6], r[7]),
            _mm256_unpackhi_epi16(r[6], r[7]),
        ];
        let c = [
            _mm256_unpacklo_epi32(b[0], b[2]),
            _mm256_unpackhi_epi32(b[0], b[2]),
            _mm256_unpacklo_epi32(b[1], b[3]),
            _mm256_unpackhi_epi32(b[1], b[3]),
            _mm256_unpacklo_epi32(b[4], b[6]),
            _mm256_unpackhi_epi32(b[4], b[6]),
            _mm256_unpacklo_epi32(b[5], b[7]),
            _mm256_unpackhi_epi32(b[5], b[7]),
        ];
        [
            _mm256_unpacklo_epi64(c[0], c[4]),
            _mm256_unpackhi_epi64(c[0], c[4]),
            _mm256_unpacklo_epi64(c[1], c[5]),
            _mm256_unpackhi_epi64(c[1], c[5]),
            _mm256_unpacklo_epi64(c[2], c[6]),
            _mm256_unpackhi_epi64(c[2], c[6]),
            _mm256_unpacklo_epi64(c[3], c[7]),
            _mm256_unpackhi_epi64(c[3], c[7]),
        ]
    }

    /// Transposes the 16 × 16 `i16` matrix `r` (row `i` is `r[i]`).
    #[inline]
    #[target_feature(enable = "avx2")]
    fn transpose(r: [V; 16]) -> [V; 16] {
        // d[j] = column j of rows 0..8 (low half: lanes 0..8; high half: lanes 8..16), e[j]
        // the same of rows 8..16.
        let d = transpose8([r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]]);
        let e = transpose8([r[8], r[9], r[10], r[11], r[12], r[13], r[14], r[15]]);
        let mut t = [d[0]; 16];
        for j in 0..8 {
            t[j] = _mm256_permute2x128_si256::<0x20>(d[j], e[j]);
            t[j + 8] = _mm256_permute2x128_si256::<0x31>(d[j], e[j]);
        }
        t
    }

    simd512!("avx2");
}

/// 128-bit vectors: `V` is a pair of `__m128i`, lanes `0..8` (words `0..4`) then the rest.
mod sse {
    use super::*;

    type V = [__m128i; 2];

    macro_rules! lanewise {
        ($($name:ident = $op:ident;)*) => {$(
            #[inline]
            #[target_feature(enable = "ssse3,sse4.1")]
            fn $name(a: V, b: V) -> V {
                [$op(a[0], b[0]), $op(a[1], b[1])]
            }
        )*};
    }

    lanewise! {
        add16 = _mm_add_epi16;
        sub16 = _mm_sub_epi16;
        mullo16 = _mm_mullo_epi16;
        mulhi16 = _mm_mulhi_epi16;
        cmpgt16 = _mm_cmpgt_epi16;
        add32 = _mm_add_epi32;
        and = _mm_and_si128;
        or = _mm_or_si128;
        xor = _mm_xor_si128;
    }

    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn slli16<const N: i32>(a: V) -> V {
        [_mm_slli_epi16::<N>(a[0]), _mm_slli_epi16::<N>(a[1])]
    }

    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn srai16<const N: i32>(a: V) -> V {
        [_mm_srai_epi16::<N>(a[0]), _mm_srai_epi16::<N>(a[1])]
    }

    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn slli32<const N: i32>(a: V) -> V {
        [_mm_slli_epi32::<N>(a[0]), _mm_slli_epi32::<N>(a[1])]
    }

    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn srli32<const N: i32>(a: V) -> V {
        [_mm_srli_epi32::<N>(a[0]), _mm_srli_epi32::<N>(a[1])]
    }

    /// Rotates each 32-bit lane left by `N` (`M = 32 - N`).
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn rotl<const N: i32, const M: i32>(a: V) -> V {
        or(slli32::<N>(a), srli32::<M>(a))
    }

    /// The low 16 bits of each 32-bit lane of `a`, the high 16 bits of `b`.
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn blend_hi16(a: V, b: V) -> V {
        [_mm_blend_epi16::<0xAA>(a[0], b[0]), _mm_blend_epi16::<0xAA>(a[1], b[1])]
    }

    /// Lane `n` is `a[n ^ PP]` (32-bit lanes): `PP & 4` swaps the halves.
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn perm<const PP: usize>(a: V) -> V {
        let a = if PP & 4 == 0 {
            a
        } else {
            [a[1], a[0]]
        };
        match PP & 3 {
            0 => a,
            1 => [_mm_shuffle_epi32::<0xB1>(a[0]), _mm_shuffle_epi32::<0xB1>(a[1])],
            2 => [_mm_shuffle_epi32::<0x4E>(a[0]), _mm_shuffle_epi32::<0x4E>(a[1])],
            _ => [_mm_shuffle_epi32::<0x1B>(a[0]), _mm_shuffle_epi32::<0x1B>(a[1])],
        }
    }

    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn splat16(c: i16) -> V {
        [_mm_set1_epi16(c); 2]
    }

    /// The lanes `c`.
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn ld16(c: &[i16; 16]) -> V {
        let p = c.as_ptr();
        // SAFETY: the loads read the 32 bytes of `c`.
        unsafe { [_mm_loadu_si128(p.cast()), _mm_loadu_si128(p.add(8).cast())] }
    }

    /// The words `c`.
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn ld32(c: &[u32; 8]) -> V {
        let p = c.as_ptr();
        // SAFETY: the loads read the 32 bytes of `c`.
        unsafe { [_mm_loadu_si128(p.cast()), _mm_loadu_si128(p.add(4).cast())] }
    }

    /// Bytes `16·i..16·i + 16` of `x` as `i16` lanes.
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn widen(x: &[u8; 64], i: usize) -> V {
        let p = x[16 * i..][..16].as_ptr();
        // SAFETY: each load reads 8 bytes inside `x`.
        let (lo, hi) = unsafe { (_mm_loadl_epi64(p.cast()), _mm_loadl_epi64(p.add(8).cast())) };
        [_mm_cvtepu8_epi16(lo), _mm_cvtepu8_epi16(hi)]
    }

    /// Words `8·i..8·i + 8` of `x`, little-endian.
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn load32(x: &[u8; 64], i: usize) -> V {
        let p = x[32 * i..][..32].as_ptr();
        // SAFETY: each load reads 16 bytes inside `x`.
        unsafe { [_mm_loadu_si128(p.cast()), _mm_loadu_si128(p.add(16).cast())] }
    }

    /// `a` then `b`, little-endian.
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn store(out: &mut [u8; 64], a: V, b: V) {
        for (i, v) in [a[0], a[1], b[0], b[1]].into_iter().enumerate() {
            // SAFETY: the store writes 16 bytes inside `out`.
            unsafe { _mm_storeu_si128(out[16 * i..][..16].as_mut_ptr().cast(), v) };
        }
    }

    /// Transposes the 8 × 8 `i16` matrix `r` (row `i` is `r[i]`).
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn transpose8(r: [__m128i; 8]) -> [__m128i; 8] {
        let b = [
            _mm_unpacklo_epi16(r[0], r[1]),
            _mm_unpackhi_epi16(r[0], r[1]),
            _mm_unpacklo_epi16(r[2], r[3]),
            _mm_unpackhi_epi16(r[2], r[3]),
            _mm_unpacklo_epi16(r[4], r[5]),
            _mm_unpackhi_epi16(r[4], r[5]),
            _mm_unpacklo_epi16(r[6], r[7]),
            _mm_unpackhi_epi16(r[6], r[7]),
        ];
        let c = [
            _mm_unpacklo_epi32(b[0], b[2]),
            _mm_unpackhi_epi32(b[0], b[2]),
            _mm_unpacklo_epi32(b[1], b[3]),
            _mm_unpackhi_epi32(b[1], b[3]),
            _mm_unpacklo_epi32(b[4], b[6]),
            _mm_unpackhi_epi32(b[4], b[6]),
            _mm_unpacklo_epi32(b[5], b[7]),
            _mm_unpackhi_epi32(b[5], b[7]),
        ];
        [
            _mm_unpacklo_epi64(c[0], c[4]),
            _mm_unpackhi_epi64(c[0], c[4]),
            _mm_unpacklo_epi64(c[1], c[5]),
            _mm_unpackhi_epi64(c[1], c[5]),
            _mm_unpacklo_epi64(c[2], c[6]),
            _mm_unpackhi_epi64(c[2], c[6]),
            _mm_unpacklo_epi64(c[3], c[7]),
            _mm_unpackhi_epi64(c[3], c[7]),
        ]
    }

    /// Transposes the 16 × 16 `i16` matrix `r` (row `i` is `r[i]`): four 8 × 8 blocks.
    #[inline]
    #[target_feature(enable = "ssse3,sse4.1")]
    fn transpose(r: [V; 16]) -> [V; 16] {
        let block =
            |h: usize, rows: usize| -> [__m128i; 8] { core::array::from_fn(|i| r[rows + i][h]) };
        let (ll, lh) = (transpose8(block(0, 0)), transpose8(block(0, 8)));
        let (hl, hh) = (transpose8(block(1, 0)), transpose8(block(1, 8)));
        let mut t = [r[0]; 16];
        for j in 0..8 {
            t[j] = [ll[j], lh[j]];
            t[j + 8] = [hl[j], hh[j]];
        }
        t
    }

    simd512!("ssse3,sse4.1");
}

/// SIMD-512 of `input` with 128-bit vectors.
///
/// # Safety
///
/// The CPU must support SSSE3 and SSE4.1.
#[target_feature(enable = "ssse3,sse4.1")]
pub(crate) unsafe fn hash_sse(input: &[u8; 64]) -> [u8; 64] {
    sse::hash(input)
}

/// SIMD-512 of `input` with 256-bit vectors.
///
/// # Safety
///
/// The CPU must support AVX2.
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn hash_avx2(input: &[u8; 64]) -> [u8; 64] {
    avx2::hash(input)
}

#[cfg(test)]
mod tests {
    use crate::portable;

    /// An interval of `i16` lane values, as `i32`.
    type R = (i32, i32);

    /// `r`, after checking it fits an `i16` lane.
    fn fit(r: R) -> R {
        assert!(r.0 >= i16::MIN as i32 && r.1 <= i16::MAX as i32, "{r:?} overflows i16");
        r
    }

    /// The hull of `rs`.
    fn span(rs: &[R]) -> R {
        rs.iter().fold((i32::MAX, i32::MIN), |s, r| (s.0.min(r.0), s.1.max(r.1)))
    }

    /// The `i16` bounds `expand` states, recomputed lane by lane with interval arithmetic from
    /// the constants it uses (`TWIDDLE`, `TWIDDLE4`, `YOFF_N`), for every input: no
    /// intermediate of the NTT overflows a lane. (A port of the XH1 r1 review's
    /// `simd_bounds_lanes.py`.)
    #[test]
    fn ntt_intermediates_fit_i16() {
        use super::{TWIDDLE, TWIDDLE4, YOFF_N};
        let add = |a: R, b: R| fit((a.0 + b.0, a.1 + b.1));
        let sub = |a: R, b: R| fit((a.0 - b.1, a.1 - b.0));
        let shl = |a: R, n: u32| fit((a.0 << n, a.1 << n));
        // mulmod is hi(x·c) - hi(m·257) for some i16 m.
        const HM: R = ((-32768 * 257) >> 16, (32767 * 257) >> 16);
        let mulmod = |x: R, c: i16| {
            let (p, q) = (x.0 * c as i32, x.1 * c as i32);
            fit(((p.min(q) >> 16) - HM.1, (p.max(q) >> 16) - HM.0))
        };
        let reds1 = |x: R| fit((-(x.1 >> 8), 255 - (x.0 >> 8)));
        let dft4 = |v: [R; 4]| {
            let (ep, em) = (add(v[0], v[2]), sub(v[0], v[2]));
            let (op, om) = (add(v[1], v[3]), shl(sub(v[1], v[3]), 4));
            [add(ep, op), add(em, om), sub(ep, op), sub(em, om)]
        };

        // Step 1: every lane alike.
        let x: R = (0, 255);
        let (mut a, mut b) = ([x; 8], [x; 8]);
        for m in 0..4 {
            let s = shl(x, 2 * m as u32);
            (a[m], a[m + 4]) = (add(x, s), sub(x, s));
            (b[m], b[m + 4]) = (add(x, s), sub(x, s));
        }
        let p = |e: usize| TWIDDLE4[e].c[0];
        let t = [
            b[0],
            shl(b[1], 1),
            shl(b[2], 2),
            mulmod(b[3], p(3)),
            shl(b[4], 4),
            mulmod(b[5], p(5)),
            mulmod(b[6], p(6)),
            mulmod(b[7], p(7)),
        ];
        let u: [R; 16] = core::array::from_fn(|k| {
            if k < 8 {
                add(a[k], t[k])
            } else {
                sub(a[k - 8], t[k - 8])
            }
        });
        assert_eq!(span(&u), (-17340, 21675));

        // Step 3: v[j0] lane k0 holds u[k0].
        let v: [[R; 16]; 16] = core::array::from_fn(|j0| {
            core::array::from_fn(|k0| match j0 {
                0 => reds1(u[k0]),
                15 => add(
                    mulmod(u[k0], TWIDDLE[15].c[k0]),
                    (YOFF_N.0[k0] as i32, YOFF_N.0[k0] as i32),
                ),
                _ => mulmod(u[k0], TWIDDLE[j0].c[k0]),
            })
        });
        assert_eq!(span(&v[0]), (-84, 323));
        assert_eq!(span(v[1..15].as_flattened()), (-168, 169));
        assert_eq!(span(&v[15]), (-237, 246));

        // Steps 4 and 5, lane by lane.
        let (mut first, mut twiddled, mut second) = ((0, 0), (0, 0), (0, 0));
        #[allow(clippy::needless_range_loop)] // k0 indexes lanes of v's rows
        for k0 in 0..16 {
            let mut s: [[R; 4]; 4] =
                core::array::from_fn(|n0| dft4([0, 4, 8, 12].map(|n1| v[n0 + n1][k0])));
            first = span(&[first, span(s.as_flattened())]);
            for n0 in 1..4 {
                for a in 1..4 {
                    s[n0][a] = mulmod(s[n0][a], TWIDDLE4[n0 * a].c[k0]);
                    twiddled = span(&[twiddled, s[n0][a]]);
                }
            }
            for a in 0..4 {
                let y = dft4(s.map(|row| row[a]));
                second = span(&[second, span(&y)]);
                // Step 5's reduction of each output (fit checks it).
                let _ = y.map(reds1);
            }
        }
        assert_eq!(first, (-6551, 6514));
        assert_eq!(twiddled, (-133, 134));
        assert!(second.0 >= -22223 && second.1 <= 22223, "{second:?}");
    }

    /// Bytes 0 and 255 only, where the `i16` bounds of the NTT are tightest; the other inputs
    /// are `backends_match_portable`'s.
    #[test]
    fn extreme_bytes_match_portable() {
        let has = super::super::cpu_features();
        let mut s = 0x5eed_u64;
        for i in 0..20_000u32 {
            let input: [u8; 64] = core::array::from_fn(|j| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                match i {
                    0 => 0,
                    1 => 255,
                    2 => [0, 255][j & 1],
                    _ => [0, 255][(s >> 63) as usize],
                }
            });
            let want = portable::simd::hash(&input);
            if has.contains(crate::Use::SSE) {
                // SAFETY: the CPU has SSSE3 and SSE4.1.
                assert_eq!(unsafe { super::hash_sse(&input) }, want, "sse case {i}");
            }
            if has.contains(crate::Use::AVX2) {
                // SAFETY: the CPU has AVX2.
                assert_eq!(unsafe { super::hash_avx2(&input) }, want, "avx2 case {i}");
            }
        }
    }
}
