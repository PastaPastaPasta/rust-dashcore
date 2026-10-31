// Copyright (c) 2014-2022 The Bitcoin Core developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or http://www.opensource.org/licenses/mit-license.php.

//! Core's SHA-256 round code, shared by the portable, SSE4.1 4-way and AVX2 8-way backends.
//!
//! Each backend module defines, for its lane type, the primitives `k` (broadcast a constant),
//! `add`, `xor`, `or`, `and`, `shr::<N>` and `shl::<N>`, invokes [`lane_fns!`] for the SHA-256
//! functions built from them, and supplies `read(input, offset)` (big-endian message words) and
//! `write(out, offset, value)` (big-endian output words). The macros below then expand to
//! Core's straight-line code. Addition is modulo 2^32 in every lane; the grouping of the
//! `add!` operands follows Core's `Add` overloads.

/// Core's variadic `Add`.
macro_rules! add {
    ($x:expr, $y:expr) => {
        add($x, $y)
    };
    ($x:expr, $y:expr, $z:expr) => {
        add(add($x, $y), $z)
    };
    ($x:expr, $y:expr, $z:expr, $w:expr) => {
        add(add($x, $y), add($z, $w))
    };
    ($x:expr, $y:expr, $z:expr, $w:expr, $v:expr) => {
        add(add!($x, $y, $z), add($w, $v))
    };
}

/// Core's `Inc`: `x = Add(x, ...)`, evaluating to the new `x`.
macro_rules! inc {
    ($x:ident, $($y:expr),+) => {{
        $x = add!($x, $($y),+);
        $x
    }};
}

/// One round of SHA-256 (Core: `Round`). Updates `d` and `h`.
macro_rules! round {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $k:expr) => {{
        let t1 = add!($h, big_sigma1($e), ch($e, $f, $g), $k);
        let t2 = add!(big_sigma0($a), maj($a, $b, $c));
        $d = add($d, t1);
        $h = add(t1, t2);
    }};
}

/// Core's `Ch`, `Maj`, `Sigma0`, `Sigma1`, `sigma0` and `sigma1` over the caller's lane type `V`,
/// built from its `xor`, `or`, `and`, `shr` and `shl`. The attributes (a `target_feature` for the
/// SIMD backends) apply to every function.
macro_rules! lane_fns {
    ($(#[$attr:meta])*) => {
        $(#[$attr])*
        #[inline]
        fn ch(x: V, y: V, z: V) -> V {
            xor(z, and(x, xor(y, z)))
        }

        $(#[$attr])*
        #[inline]
        fn maj(x: V, y: V, z: V) -> V {
            or(and(x, y), and(z, or(x, y)))
        }

        $(#[$attr])*
        #[inline]
        fn big_sigma0(x: V) -> V {
            xor(xor(or(shr::<2>(x), shl::<30>(x)), or(shr::<13>(x), shl::<19>(x))), or(shr::<22>(x), shl::<10>(x)))
        }

        $(#[$attr])*
        #[inline]
        fn big_sigma1(x: V) -> V {
            xor(xor(or(shr::<6>(x), shl::<26>(x)), or(shr::<11>(x), shl::<21>(x))), or(shr::<25>(x), shl::<7>(x)))
        }

        $(#[$attr])*
        #[inline]
        fn small_sigma0(x: V) -> V {
            xor(xor(or(shr::<7>(x), shl::<25>(x)), or(shr::<18>(x), shl::<14>(x))), shr::<3>(x))
        }

        $(#[$attr])*
        #[inline]
        fn small_sigma1(x: V) -> V {
            xor(xor(or(shr::<17>(x), shl::<15>(x)), or(shr::<19>(x), shl::<13>(x))), shr::<10>(x))
        }
    };
}

/// The 64 rounds of one 64-byte block (Core: the body of `sha256::Transform`), reading the
/// message through `read($input, offset)` and updating the working variables `$a`..`$h`.
macro_rules! block_rounds {
    ($input:expr; $a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident) => {{
        let mut w0 = read($input, 0);
        round!($a, $b, $c, $d, $e, $f, $g, $h, add!(k(0x428a2f98), w0));
        let mut w1 = read($input, 4);
        round!($h, $a, $b, $c, $d, $e, $f, $g, add!(k(0x71374491), w1));
        let mut w2 = read($input, 8);
        round!($g, $h, $a, $b, $c, $d, $e, $f, add!(k(0xb5c0fbcf), w2));
        let mut w3 = read($input, 12);
        round!($f, $g, $h, $a, $b, $c, $d, $e, add!(k(0xe9b5dba5), w3));
        let mut w4 = read($input, 16);
        round!($e, $f, $g, $h, $a, $b, $c, $d, add!(k(0x3956c25b), w4));
        let mut w5 = read($input, 20);
        round!($d, $e, $f, $g, $h, $a, $b, $c, add!(k(0x59f111f1), w5));
        let mut w6 = read($input, 24);
        round!($c, $d, $e, $f, $g, $h, $a, $b, add!(k(0x923f82a4), w6));
        let mut w7 = read($input, 28);
        round!($b, $c, $d, $e, $f, $g, $h, $a, add!(k(0xab1c5ed5), w7));
        let mut w8 = read($input, 32);
        round!($a, $b, $c, $d, $e, $f, $g, $h, add!(k(0xd807aa98), w8));
        let mut w9 = read($input, 36);
        round!($h, $a, $b, $c, $d, $e, $f, $g, add!(k(0x12835b01), w9));
        let mut w10 = read($input, 40);
        round!($g, $h, $a, $b, $c, $d, $e, $f, add!(k(0x243185be), w10));
        let mut w11 = read($input, 44);
        round!($f, $g, $h, $a, $b, $c, $d, $e, add!(k(0x550c7dc3), w11));
        let mut w12 = read($input, 48);
        round!($e, $f, $g, $h, $a, $b, $c, $d, add!(k(0x72be5d74), w12));
        let mut w13 = read($input, 52);
        round!($d, $e, $f, $g, $h, $a, $b, $c, add!(k(0x80deb1fe), w13));
        let mut w14 = read($input, 56);
        round!($c, $d, $e, $f, $g, $h, $a, $b, add!(k(0x9bdc06a7), w14));
        let mut w15 = read($input, 60);
        round!($b, $c, $d, $e, $f, $g, $h, $a, add!(k(0xc19bf174), w15));
        round!(
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            add!(k(0xe49b69c1), inc!(w0, small_sigma1(w14), w9, small_sigma0(w1)))
        );
        round!(
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            add!(k(0xefbe4786), inc!(w1, small_sigma1(w15), w10, small_sigma0(w2)))
        );
        round!(
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            add!(k(0x0fc19dc6), inc!(w2, small_sigma1(w0), w11, small_sigma0(w3)))
        );
        round!(
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            add!(k(0x240ca1cc), inc!(w3, small_sigma1(w1), w12, small_sigma0(w4)))
        );
        round!(
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            add!(k(0x2de92c6f), inc!(w4, small_sigma1(w2), w13, small_sigma0(w5)))
        );
        round!(
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            add!(k(0x4a7484aa), inc!(w5, small_sigma1(w3), w14, small_sigma0(w6)))
        );
        round!(
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            add!(k(0x5cb0a9dc), inc!(w6, small_sigma1(w4), w15, small_sigma0(w7)))
        );
        round!(
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            add!(k(0x76f988da), inc!(w7, small_sigma1(w5), w0, small_sigma0(w8)))
        );
        round!(
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            add!(k(0x983e5152), inc!(w8, small_sigma1(w6), w1, small_sigma0(w9)))
        );
        round!(
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            add!(k(0xa831c66d), inc!(w9, small_sigma1(w7), w2, small_sigma0(w10)))
        );
        round!(
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            add!(k(0xb00327c8), inc!(w10, small_sigma1(w8), w3, small_sigma0(w11)))
        );
        round!(
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            add!(k(0xbf597fc7), inc!(w11, small_sigma1(w9), w4, small_sigma0(w12)))
        );
        round!(
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            add!(k(0xc6e00bf3), inc!(w12, small_sigma1(w10), w5, small_sigma0(w13)))
        );
        round!(
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            add!(k(0xd5a79147), inc!(w13, small_sigma1(w11), w6, small_sigma0(w14)))
        );
        round!(
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            add!(k(0x06ca6351), inc!(w14, small_sigma1(w12), w7, small_sigma0(w15)))
        );
        round!(
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            add!(k(0x14292967), inc!(w15, small_sigma1(w13), w8, small_sigma0(w0)))
        );
        round!(
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            add!(k(0x27b70a85), inc!(w0, small_sigma1(w14), w9, small_sigma0(w1)))
        );
        round!(
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            add!(k(0x2e1b2138), inc!(w1, small_sigma1(w15), w10, small_sigma0(w2)))
        );
        round!(
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            add!(k(0x4d2c6dfc), inc!(w2, small_sigma1(w0), w11, small_sigma0(w3)))
        );
        round!(
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            add!(k(0x53380d13), inc!(w3, small_sigma1(w1), w12, small_sigma0(w4)))
        );
        round!(
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            add!(k(0x650a7354), inc!(w4, small_sigma1(w2), w13, small_sigma0(w5)))
        );
        round!(
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            add!(k(0x766a0abb), inc!(w5, small_sigma1(w3), w14, small_sigma0(w6)))
        );
        round!(
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            add!(k(0x81c2c92e), inc!(w6, small_sigma1(w4), w15, small_sigma0(w7)))
        );
        round!(
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            add!(k(0x92722c85), inc!(w7, small_sigma1(w5), w0, small_sigma0(w8)))
        );
        round!(
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            add!(k(0xa2bfe8a1), inc!(w8, small_sigma1(w6), w1, small_sigma0(w9)))
        );
        round!(
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            add!(k(0xa81a664b), inc!(w9, small_sigma1(w7), w2, small_sigma0(w10)))
        );
        round!(
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            add!(k(0xc24b8b70), inc!(w10, small_sigma1(w8), w3, small_sigma0(w11)))
        );
        round!(
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            add!(k(0xc76c51a3), inc!(w11, small_sigma1(w9), w4, small_sigma0(w12)))
        );
        round!(
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            add!(k(0xd192e819), inc!(w12, small_sigma1(w10), w5, small_sigma0(w13)))
        );
        round!(
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            add!(k(0xd6990624), inc!(w13, small_sigma1(w11), w6, small_sigma0(w14)))
        );
        round!(
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            add!(k(0xf40e3585), inc!(w14, small_sigma1(w12), w7, small_sigma0(w15)))
        );
        round!(
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            add!(k(0x106aa070), inc!(w15, small_sigma1(w13), w8, small_sigma0(w0)))
        );
        round!(
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            add!(k(0x19a4c116), inc!(w0, small_sigma1(w14), w9, small_sigma0(w1)))
        );
        round!(
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            add!(k(0x1e376c08), inc!(w1, small_sigma1(w15), w10, small_sigma0(w2)))
        );
        round!(
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            add!(k(0x2748774c), inc!(w2, small_sigma1(w0), w11, small_sigma0(w3)))
        );
        round!(
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            add!(k(0x34b0bcb5), inc!(w3, small_sigma1(w1), w12, small_sigma0(w4)))
        );
        round!(
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            add!(k(0x391c0cb3), inc!(w4, small_sigma1(w2), w13, small_sigma0(w5)))
        );
        round!(
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            add!(k(0x4ed8aa4a), inc!(w5, small_sigma1(w3), w14, small_sigma0(w6)))
        );
        round!(
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            add!(k(0x5b9cca4f), inc!(w6, small_sigma1(w4), w15, small_sigma0(w7)))
        );
        round!(
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            add!(k(0x682e6ff3), inc!(w7, small_sigma1(w5), w0, small_sigma0(w8)))
        );
        round!(
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            add!(k(0x748f82ee), inc!(w8, small_sigma1(w6), w1, small_sigma0(w9)))
        );
        round!(
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            add!(k(0x78a5636f), inc!(w9, small_sigma1(w7), w2, small_sigma0(w10)))
        );
        round!(
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            $f,
            add!(k(0x84c87814), inc!(w10, small_sigma1(w8), w3, small_sigma0(w11)))
        );
        round!(
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            $e,
            add!(k(0x8cc70208), inc!(w11, small_sigma1(w9), w4, small_sigma0(w12)))
        );
        round!(
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            $d,
            add!(k(0x90befffa), inc!(w12, small_sigma1(w10), w5, small_sigma0(w13)))
        );
        round!(
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            $c,
            add!(k(0xa4506ceb), inc!(w13, small_sigma1(w11), w6, small_sigma0(w14)))
        );
        round!(
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            $b,
            add!(k(0xbef9a3f7), add!(w14, small_sigma1(w12), w7, small_sigma0(w15)))
        );
        round!(
            $b,
            $c,
            $d,
            $e,
            $f,
            $g,
            $h,
            $a,
            add!(k(0xc67178f2), add!(w15, small_sigma1(w13), w8, small_sigma0(w0)))
        );
    }};
}

/// Double SHA-256 of 64-byte inputs, one per lane (Core: `sha256::TransformD64`,
/// `sha256d64_sse41::Transform_4way` and `sha256d64_avx2::Transform_8way`, which run the same
/// rounds). Transform 1 hashes the input, transform 2 the constant padding block with its
/// message schedule folded into the round constants, and transform 3 the 32-byte digest.
macro_rules! transform_d64 {
    ($input:expr, $out:expr) => {{
        // Transform 1
        let mut a = k(0x6a09e667);
        let mut b = k(0xbb67ae85);
        let mut c = k(0x3c6ef372);
        let mut d = k(0xa54ff53a);
        let mut e = k(0x510e527f);
        let mut f = k(0x9b05688c);
        let mut g = k(0x1f83d9ab);
        let mut h = k(0x5be0cd19);

        let mut w0 = read($input, 0);
        round!(a, b, c, d, e, f, g, h, add!(k(0x428a2f98), w0));
        let mut w1 = read($input, 4);
        round!(h, a, b, c, d, e, f, g, add!(k(0x71374491), w1));
        let mut w2 = read($input, 8);
        round!(g, h, a, b, c, d, e, f, add!(k(0xb5c0fbcf), w2));
        let mut w3 = read($input, 12);
        round!(f, g, h, a, b, c, d, e, add!(k(0xe9b5dba5), w3));
        let mut w4 = read($input, 16);
        round!(e, f, g, h, a, b, c, d, add!(k(0x3956c25b), w4));
        let mut w5 = read($input, 20);
        round!(d, e, f, g, h, a, b, c, add!(k(0x59f111f1), w5));
        let mut w6 = read($input, 24);
        round!(c, d, e, f, g, h, a, b, add!(k(0x923f82a4), w6));
        let mut w7 = read($input, 28);
        round!(b, c, d, e, f, g, h, a, add!(k(0xab1c5ed5), w7));
        let mut w8 = read($input, 32);
        round!(a, b, c, d, e, f, g, h, add!(k(0xd807aa98), w8));
        let mut w9 = read($input, 36);
        round!(h, a, b, c, d, e, f, g, add!(k(0x12835b01), w9));
        let mut w10 = read($input, 40);
        round!(g, h, a, b, c, d, e, f, add!(k(0x243185be), w10));
        let mut w11 = read($input, 44);
        round!(f, g, h, a, b, c, d, e, add!(k(0x550c7dc3), w11));
        let mut w12 = read($input, 48);
        round!(e, f, g, h, a, b, c, d, add!(k(0x72be5d74), w12));
        let mut w13 = read($input, 52);
        round!(d, e, f, g, h, a, b, c, add!(k(0x80deb1fe), w13));
        let mut w14 = read($input, 56);
        round!(c, d, e, f, g, h, a, b, add!(k(0x9bdc06a7), w14));
        let mut w15 = read($input, 60);
        round!(b, c, d, e, f, g, h, a, add!(k(0xc19bf174), w15));
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0xe49b69c1), inc!(w0, small_sigma1(w14), w9, small_sigma0(w1)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0xefbe4786), inc!(w1, small_sigma1(w15), w10, small_sigma0(w2)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0x0fc19dc6), inc!(w2, small_sigma1(w0), w11, small_sigma0(w3)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0x240ca1cc), inc!(w3, small_sigma1(w1), w12, small_sigma0(w4)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0x2de92c6f), inc!(w4, small_sigma1(w2), w13, small_sigma0(w5)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0x4a7484aa), inc!(w5, small_sigma1(w3), w14, small_sigma0(w6)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0x5cb0a9dc), inc!(w6, small_sigma1(w4), w15, small_sigma0(w7)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x76f988da), inc!(w7, small_sigma1(w5), w0, small_sigma0(w8)))
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0x983e5152), inc!(w8, small_sigma1(w6), w1, small_sigma0(w9)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0xa831c66d), inc!(w9, small_sigma1(w7), w2, small_sigma0(w10)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0xb00327c8), inc!(w10, small_sigma1(w8), w3, small_sigma0(w11)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0xbf597fc7), inc!(w11, small_sigma1(w9), w4, small_sigma0(w12)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0xc6e00bf3), inc!(w12, small_sigma1(w10), w5, small_sigma0(w13)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0xd5a79147), inc!(w13, small_sigma1(w11), w6, small_sigma0(w14)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0x06ca6351), inc!(w14, small_sigma1(w12), w7, small_sigma0(w15)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x14292967), inc!(w15, small_sigma1(w13), w8, small_sigma0(w0)))
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0x27b70a85), inc!(w0, small_sigma1(w14), w9, small_sigma0(w1)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0x2e1b2138), inc!(w1, small_sigma1(w15), w10, small_sigma0(w2)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0x4d2c6dfc), inc!(w2, small_sigma1(w0), w11, small_sigma0(w3)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0x53380d13), inc!(w3, small_sigma1(w1), w12, small_sigma0(w4)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0x650a7354), inc!(w4, small_sigma1(w2), w13, small_sigma0(w5)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0x766a0abb), inc!(w5, small_sigma1(w3), w14, small_sigma0(w6)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0x81c2c92e), inc!(w6, small_sigma1(w4), w15, small_sigma0(w7)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x92722c85), inc!(w7, small_sigma1(w5), w0, small_sigma0(w8)))
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0xa2bfe8a1), inc!(w8, small_sigma1(w6), w1, small_sigma0(w9)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0xa81a664b), inc!(w9, small_sigma1(w7), w2, small_sigma0(w10)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0xc24b8b70), inc!(w10, small_sigma1(w8), w3, small_sigma0(w11)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0xc76c51a3), inc!(w11, small_sigma1(w9), w4, small_sigma0(w12)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0xd192e819), inc!(w12, small_sigma1(w10), w5, small_sigma0(w13)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0xd6990624), inc!(w13, small_sigma1(w11), w6, small_sigma0(w14)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0xf40e3585), inc!(w14, small_sigma1(w12), w7, small_sigma0(w15)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x106aa070), inc!(w15, small_sigma1(w13), w8, small_sigma0(w0)))
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0x19a4c116), inc!(w0, small_sigma1(w14), w9, small_sigma0(w1)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0x1e376c08), inc!(w1, small_sigma1(w15), w10, small_sigma0(w2)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0x2748774c), inc!(w2, small_sigma1(w0), w11, small_sigma0(w3)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0x34b0bcb5), inc!(w3, small_sigma1(w1), w12, small_sigma0(w4)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0x391c0cb3), inc!(w4, small_sigma1(w2), w13, small_sigma0(w5)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0x4ed8aa4a), inc!(w5, small_sigma1(w3), w14, small_sigma0(w6)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0x5b9cca4f), inc!(w6, small_sigma1(w4), w15, small_sigma0(w7)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x682e6ff3), inc!(w7, small_sigma1(w5), w0, small_sigma0(w8)))
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0x748f82ee), inc!(w8, small_sigma1(w6), w1, small_sigma0(w9)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0x78a5636f), inc!(w9, small_sigma1(w7), w2, small_sigma0(w10)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0x84c87814), inc!(w10, small_sigma1(w8), w3, small_sigma0(w11)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0x8cc70208), inc!(w11, small_sigma1(w9), w4, small_sigma0(w12)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0x90befffa), inc!(w12, small_sigma1(w10), w5, small_sigma0(w13)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0xa4506ceb), inc!(w13, small_sigma1(w11), w6, small_sigma0(w14)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0xbef9a3f7), inc!(w14, small_sigma1(w12), w7, small_sigma0(w15)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0xc67178f2), inc!(w15, small_sigma1(w13), w8, small_sigma0(w0)))
        );

        a = add!(a, k(0x6a09e667));
        b = add!(b, k(0xbb67ae85));
        c = add!(c, k(0x3c6ef372));
        d = add!(d, k(0xa54ff53a));
        e = add!(e, k(0x510e527f));
        f = add!(f, k(0x9b05688c));
        g = add!(g, k(0x1f83d9ab));
        h = add!(h, k(0x5be0cd19));

        let t0 = a;
        let t1 = b;
        let t2 = c;
        let t3 = d;
        let t4 = e;
        let t5 = f;
        let t6 = g;
        let t7 = h;

        // Transform 2
        round!(a, b, c, d, e, f, g, h, k(0xc28a2f98));
        round!(h, a, b, c, d, e, f, g, k(0x71374491));
        round!(g, h, a, b, c, d, e, f, k(0xb5c0fbcf));
        round!(f, g, h, a, b, c, d, e, k(0xe9b5dba5));
        round!(e, f, g, h, a, b, c, d, k(0x3956c25b));
        round!(d, e, f, g, h, a, b, c, k(0x59f111f1));
        round!(c, d, e, f, g, h, a, b, k(0x923f82a4));
        round!(b, c, d, e, f, g, h, a, k(0xab1c5ed5));
        round!(a, b, c, d, e, f, g, h, k(0xd807aa98));
        round!(h, a, b, c, d, e, f, g, k(0x12835b01));
        round!(g, h, a, b, c, d, e, f, k(0x243185be));
        round!(f, g, h, a, b, c, d, e, k(0x550c7dc3));
        round!(e, f, g, h, a, b, c, d, k(0x72be5d74));
        round!(d, e, f, g, h, a, b, c, k(0x80deb1fe));
        round!(c, d, e, f, g, h, a, b, k(0x9bdc06a7));
        round!(b, c, d, e, f, g, h, a, k(0xc19bf374));
        round!(a, b, c, d, e, f, g, h, k(0x649b69c1));
        round!(h, a, b, c, d, e, f, g, k(0xf0fe4786));
        round!(g, h, a, b, c, d, e, f, k(0x0fe1edc6));
        round!(f, g, h, a, b, c, d, e, k(0x240cf254));
        round!(e, f, g, h, a, b, c, d, k(0x4fe9346f));
        round!(d, e, f, g, h, a, b, c, k(0x6cc984be));
        round!(c, d, e, f, g, h, a, b, k(0x61b9411e));
        round!(b, c, d, e, f, g, h, a, k(0x16f988fa));
        round!(a, b, c, d, e, f, g, h, k(0xf2c65152));
        round!(h, a, b, c, d, e, f, g, k(0xa88e5a6d));
        round!(g, h, a, b, c, d, e, f, k(0xb019fc65));
        round!(f, g, h, a, b, c, d, e, k(0xb9d99ec7));
        round!(e, f, g, h, a, b, c, d, k(0x9a1231c3));
        round!(d, e, f, g, h, a, b, c, k(0xe70eeaa0));
        round!(c, d, e, f, g, h, a, b, k(0xfdb1232b));
        round!(b, c, d, e, f, g, h, a, k(0xc7353eb0));
        round!(a, b, c, d, e, f, g, h, k(0x3069bad5));
        round!(h, a, b, c, d, e, f, g, k(0xcb976d5f));
        round!(g, h, a, b, c, d, e, f, k(0x5a0f118f));
        round!(f, g, h, a, b, c, d, e, k(0xdc1eeefd));
        round!(e, f, g, h, a, b, c, d, k(0x0a35b689));
        round!(d, e, f, g, h, a, b, c, k(0xde0b7a04));
        round!(c, d, e, f, g, h, a, b, k(0x58f4ca9d));
        round!(b, c, d, e, f, g, h, a, k(0xe15d5b16));
        round!(a, b, c, d, e, f, g, h, k(0x007f3e86));
        round!(h, a, b, c, d, e, f, g, k(0x37088980));
        round!(g, h, a, b, c, d, e, f, k(0xa507ea32));
        round!(f, g, h, a, b, c, d, e, k(0x6fab9537));
        round!(e, f, g, h, a, b, c, d, k(0x17406110));
        round!(d, e, f, g, h, a, b, c, k(0x0d8cd6f1));
        round!(c, d, e, f, g, h, a, b, k(0xcdaa3b6d));
        round!(b, c, d, e, f, g, h, a, k(0xc0bbbe37));
        round!(a, b, c, d, e, f, g, h, k(0x83613bda));
        round!(h, a, b, c, d, e, f, g, k(0xdb48a363));
        round!(g, h, a, b, c, d, e, f, k(0x0b02e931));
        round!(f, g, h, a, b, c, d, e, k(0x6fd15ca7));
        round!(e, f, g, h, a, b, c, d, k(0x521afaca));
        round!(d, e, f, g, h, a, b, c, k(0x31338431));
        round!(c, d, e, f, g, h, a, b, k(0x6ed41a95));
        round!(b, c, d, e, f, g, h, a, k(0x6d437890));
        round!(a, b, c, d, e, f, g, h, k(0xc39c91f2));
        round!(h, a, b, c, d, e, f, g, k(0x9eccabbd));
        round!(g, h, a, b, c, d, e, f, k(0xb5c9a0e6));
        round!(f, g, h, a, b, c, d, e, k(0x532fb63c));
        round!(e, f, g, h, a, b, c, d, k(0xd2c741c6));
        round!(d, e, f, g, h, a, b, c, k(0x07237ea3));
        round!(c, d, e, f, g, h, a, b, k(0xa4954b68));
        round!(b, c, d, e, f, g, h, a, k(0x4c191d76));

        w0 = add!(t0, a);
        w1 = add!(t1, b);
        w2 = add!(t2, c);
        w3 = add!(t3, d);
        w4 = add!(t4, e);
        w5 = add!(t5, f);
        w6 = add!(t6, g);
        w7 = add!(t7, h);

        // Transform 3
        a = k(0x6a09e667);
        b = k(0xbb67ae85);
        c = k(0x3c6ef372);
        d = k(0xa54ff53a);
        e = k(0x510e527f);
        f = k(0x9b05688c);
        g = k(0x1f83d9ab);
        h = k(0x5be0cd19);

        round!(a, b, c, d, e, f, g, h, add!(k(0x428a2f98), w0));
        round!(h, a, b, c, d, e, f, g, add!(k(0x71374491), w1));
        round!(g, h, a, b, c, d, e, f, add!(k(0xb5c0fbcf), w2));
        round!(f, g, h, a, b, c, d, e, add!(k(0xe9b5dba5), w3));
        round!(e, f, g, h, a, b, c, d, add!(k(0x3956c25b), w4));
        round!(d, e, f, g, h, a, b, c, add!(k(0x59f111f1), w5));
        round!(c, d, e, f, g, h, a, b, add!(k(0x923f82a4), w6));
        round!(b, c, d, e, f, g, h, a, add!(k(0xab1c5ed5), w7));
        round!(a, b, c, d, e, f, g, h, k(0x5807aa98));
        round!(h, a, b, c, d, e, f, g, k(0x12835b01));
        round!(g, h, a, b, c, d, e, f, k(0x243185be));
        round!(f, g, h, a, b, c, d, e, k(0x550c7dc3));
        round!(e, f, g, h, a, b, c, d, k(0x72be5d74));
        round!(d, e, f, g, h, a, b, c, k(0x80deb1fe));
        round!(c, d, e, f, g, h, a, b, k(0x9bdc06a7));
        round!(b, c, d, e, f, g, h, a, k(0xc19bf274));
        round!(a, b, c, d, e, f, g, h, add!(k(0xe49b69c1), inc!(w0, small_sigma0(w1))));
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0xefbe4786), inc!(w1, k(0xa00000), small_sigma0(w2)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0x0fc19dc6), inc!(w2, small_sigma1(w0), small_sigma0(w3)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0x240ca1cc), inc!(w3, small_sigma1(w1), small_sigma0(w4)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0x2de92c6f), inc!(w4, small_sigma1(w2), small_sigma0(w5)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0x4a7484aa), inc!(w5, small_sigma1(w3), small_sigma0(w6)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0x5cb0a9dc), inc!(w6, small_sigma1(w4), k(0x100), small_sigma0(w7)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x76f988da), inc!(w7, small_sigma1(w5), w0, k(0x11002000)))
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0x983e5152), {
                w8 = add!(k(0x80000000), small_sigma1(w6), w1);
                w8
            })
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0xa831c66d), {
                w9 = add!(small_sigma1(w7), w2);
                w9
            })
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0xb00327c8), {
                w10 = add!(small_sigma1(w8), w3);
                w10
            })
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0xbf597fc7), {
                w11 = add!(small_sigma1(w9), w4);
                w11
            })
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0xc6e00bf3), {
                w12 = add!(small_sigma1(w10), w5);
                w12
            })
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0xd5a79147), {
                w13 = add!(small_sigma1(w11), w6);
                w13
            })
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0x06ca6351), {
                w14 = add!(small_sigma1(w12), w7, k(0x400022));
                w14
            })
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x14292967), {
                w15 = add!(k(0x100), small_sigma1(w13), w8, small_sigma0(w0));
                w15
            })
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0x27b70a85), inc!(w0, small_sigma1(w14), w9, small_sigma0(w1)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0x2e1b2138), inc!(w1, small_sigma1(w15), w10, small_sigma0(w2)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0x4d2c6dfc), inc!(w2, small_sigma1(w0), w11, small_sigma0(w3)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0x53380d13), inc!(w3, small_sigma1(w1), w12, small_sigma0(w4)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0x650a7354), inc!(w4, small_sigma1(w2), w13, small_sigma0(w5)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0x766a0abb), inc!(w5, small_sigma1(w3), w14, small_sigma0(w6)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0x81c2c92e), inc!(w6, small_sigma1(w4), w15, small_sigma0(w7)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x92722c85), inc!(w7, small_sigma1(w5), w0, small_sigma0(w8)))
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0xa2bfe8a1), inc!(w8, small_sigma1(w6), w1, small_sigma0(w9)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0xa81a664b), inc!(w9, small_sigma1(w7), w2, small_sigma0(w10)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0xc24b8b70), inc!(w10, small_sigma1(w8), w3, small_sigma0(w11)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0xc76c51a3), inc!(w11, small_sigma1(w9), w4, small_sigma0(w12)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0xd192e819), inc!(w12, small_sigma1(w10), w5, small_sigma0(w13)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0xd6990624), inc!(w13, small_sigma1(w11), w6, small_sigma0(w14)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0xf40e3585), inc!(w14, small_sigma1(w12), w7, small_sigma0(w15)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x106aa070), inc!(w15, small_sigma1(w13), w8, small_sigma0(w0)))
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0x19a4c116), inc!(w0, small_sigma1(w14), w9, small_sigma0(w1)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0x1e376c08), inc!(w1, small_sigma1(w15), w10, small_sigma0(w2)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0x2748774c), inc!(w2, small_sigma1(w0), w11, small_sigma0(w3)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0x34b0bcb5), inc!(w3, small_sigma1(w1), w12, small_sigma0(w4)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0x391c0cb3), inc!(w4, small_sigma1(w2), w13, small_sigma0(w5)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0x4ed8aa4a), inc!(w5, small_sigma1(w3), w14, small_sigma0(w6)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0x5b9cca4f), inc!(w6, small_sigma1(w4), w15, small_sigma0(w7)))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0x682e6ff3), inc!(w7, small_sigma1(w5), w0, small_sigma0(w8)))
        );
        round!(
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            add!(k(0x748f82ee), inc!(w8, small_sigma1(w6), w1, small_sigma0(w9)))
        );
        round!(
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            add!(k(0x78a5636f), inc!(w9, small_sigma1(w7), w2, small_sigma0(w10)))
        );
        round!(
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            f,
            add!(k(0x84c87814), inc!(w10, small_sigma1(w8), w3, small_sigma0(w11)))
        );
        round!(
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            e,
            add!(k(0x8cc70208), inc!(w11, small_sigma1(w9), w4, small_sigma0(w12)))
        );
        round!(
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            d,
            add!(k(0x90befffa), inc!(w12, small_sigma1(w10), w5, small_sigma0(w13)))
        );
        round!(
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            c,
            add!(k(0xa4506ceb), inc!(w13, small_sigma1(w11), w6, small_sigma0(w14)))
        );
        round!(
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            b,
            add!(k(0xbef9a3f7), w14, small_sigma1(w12), w7, small_sigma0(w15))
        );
        round!(
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            a,
            add!(k(0xc67178f2), w15, small_sigma1(w13), w8, small_sigma0(w0))
        );

        // Output
        write($out, 0, add!(a, k(0x6a09e667)));
        write($out, 4, add!(b, k(0xbb67ae85)));
        write($out, 8, add!(c, k(0x3c6ef372)));
        write($out, 12, add!(d, k(0xa54ff53a)));
        write($out, 16, add!(e, k(0x510e527f)));
        write($out, 20, add!(f, k(0x9b05688c)));
        write($out, 24, add!(g, k(0x1f83d9ab)));
        write($out, 28, add!(h, k(0x5be0cd19)));
    }};
}

pub(crate) use {add, block_rounds, inc, lane_fns, round, transform_d64};
