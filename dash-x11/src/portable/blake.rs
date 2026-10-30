//! BLAKE-512 (Core: `crypto/x11/blake.c`, `sph_blake512_init`/`sph_blake512`/
//! `sph_blake512_close`).
//!
//! The compression is sph's unrolled `COMPRESS64` (not `SPH_COMPACT_BLAKE_64`) with the zero
//! salt folded in. Blocking and padding follow `blake64` and `blake64_close`: a block is
//! compressed as soon as it is full, so a message whose length is a multiple of 128 ends with a
//! padding-only block whose counter is 0, and a tail of 112 bytes or more takes two final blocks.
//! The 80-byte input of X11 has its own path with the padding words as constants.

/// `IV512`.
#[rustfmt::skip]
const IV: [u64; 8] = [
    0x6A09_E667_F3BC_C908, 0xBB67_AE85_84CA_A73B,
    0x3C6E_F372_FE94_F82B, 0xA54F_F53A_5F1D_36F1,
    0x510E_527F_ADE6_82D1, 0x9B05_688C_2B3E_6C1F,
    0x1F83_D9AB_FB41_BD6B, 0x5BE0_CD19_137E_2179,
];

/// `CB0`..`CBF`.
#[rustfmt::skip]
const CB: [u64; 16] = [
    0x243F_6A88_85A3_08D3, 0x1319_8A2E_0370_7344,
    0xA409_3822_299F_31D0, 0x082E_FA98_EC4E_6C89,
    0x4528_21E6_38D0_1377, 0xBE54_66CF_34E9_0C6C,
    0xC0AC_29B7_C97C_50DD, 0x3F84_D5B5_B547_0917,
    0x9216_D5D9_8979_FB1B, 0xD131_0BA6_98DF_B5AC,
    0x2FFD_72DB_D01A_DFB7, 0xB8E1_AFED_6A26_7E96,
    0xBA7C_9045_F12C_7F99, 0x24A1_9947_B391_6CF7,
    0x0801_F2E2_858E_FC16, 0x6369_20D8_7157_4E69,
];

/// The message permutations (`Z00`..`Z9F`). Rounds 10 to 15 reuse rows 0 to 5.
const SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

/// `GB` on `v[a], v[b], v[c], v[d]` with message words `2i` and `2i + 1` of round `r`.
macro_rules! gb {
    ($m:ident, $v:ident, $r:literal, $i:literal, $a:literal, $b:literal, $c:literal, $d:literal) => {
        let x0 = const { SIGMA[$r][2 * $i] };
        let x1 = const { SIGMA[$r][2 * $i + 1] };
        $v[$a] = $v[$a].wrapping_add($v[$b]).wrapping_add($m[x0] ^ CB[x1]);
        $v[$d] = ($v[$d] ^ $v[$a]).rotate_right(32);
        $v[$c] = $v[$c].wrapping_add($v[$d]);
        $v[$b] = ($v[$b] ^ $v[$c]).rotate_right(25);
        $v[$a] = $v[$a].wrapping_add($v[$b]).wrapping_add($m[x1] ^ CB[x0]);
        $v[$d] = ($v[$d] ^ $v[$a]).rotate_right(16);
        $v[$c] = $v[$c].wrapping_add($v[$d]);
        $v[$b] = ($v[$b] ^ $v[$c]).rotate_right(11);
    };
}

/// `ROUND_B(r)`.
macro_rules! round {
    ($m:ident, $v:ident, $($r:literal)*) => {$(
        gb!($m, $v, $r, 0, 0, 4, 8, 12);
        gb!($m, $v, $r, 1, 1, 5, 9, 13);
        gb!($m, $v, $r, 2, 2, 6, 10, 14);
        gb!($m, $v, $r, 3, 3, 7, 11, 15);
        gb!($m, $v, $r, 4, 0, 5, 10, 15);
        gb!($m, $v, $r, 5, 1, 6, 11, 12);
        gb!($m, $v, $r, 6, 2, 7, 8, 13);
        gb!($m, $v, $r, 7, 3, 4, 9, 14);
    )*};
}

/// `COMPRESS64` with a zero salt, on the block's big-endian words and counter `t1:t0`.
#[inline(always)]
fn compress(h: &mut [u64; 8], m: &[u64; 16], t0: u64, t1: u64) {
    let mut v = [
        h[0],
        h[1],
        h[2],
        h[3],
        h[4],
        h[5],
        h[6],
        h[7],
        CB[0],
        CB[1],
        CB[2],
        CB[3],
        t0 ^ CB[4],
        t0 ^ CB[5],
        t1 ^ CB[6],
        t1 ^ CB[7],
    ];
    round!(m, v, 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5);
    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

#[inline(never)]
fn compress_block(h: &mut [u64; 8], block: &[u8; 128], t0: u64, t1: u64) {
    let mut m = [0; 16];
    for (w, b) in m.iter_mut().zip(block.as_chunks::<8>().0) {
        *w = u64::from_be_bytes(*b);
    }
    compress(h, &m, t0, t1);
}

fn output(h: &[u64; 8]) -> [u8; 64] {
    let mut out = [0; 64];
    for (o, w) in out.as_chunks_mut::<8>().0.iter_mut().zip(h) {
        *o = w.to_be_bytes();
    }
    out
}

/// BLAKE-512 of `data`.
pub(crate) fn hash(data: &[u8]) -> [u8; 64] {
    match data.try_into() {
        Ok(header) => hash80(header),
        Err(_) => hash_any(data),
    }
}

/// One block: 80 message bytes, `0x80`, the final `1` bit at byte 111, the 640-bit length.
fn hash80(data: &[u8; 80]) -> [u8; 64] {
    let mut m = [0; 16];
    for (w, b) in m.iter_mut().zip(data.as_chunks::<8>().0) {
        *w = u64::from_be_bytes(*b);
    }
    m[10] = 0x80 << 56;
    m[13] = 1;
    m[15] = 640;
    let mut h = IV;
    compress(&mut h, &m, 640, 0);
    output(&h)
}

fn hash_any(data: &[u8]) -> [u8; 64] {
    let mut h = IV;
    let (mut t0, mut t1) = (0u64, 0u64);
    let (blocks, tail) = data.as_chunks::<128>();
    for block in blocks {
        t0 = t0.wrapping_add(1024);
        if t0 < 1024 {
            t1 = t1.wrapping_add(1);
        }
        compress_block(&mut h, block, t0, t1);
    }
    // `blake64_close`: the bit count is `T0 + bit_len` without carry, and the last block's
    // counter is that count, or 0 if the block holds no message bits.
    let bit_len = (tail.len() as u64) << 3;
    let (tl, th) = (t0.wrapping_add(bit_len), t1);
    let mut buf = [0; 128];
    buf[..tail.len()].copy_from_slice(tail);
    buf[tail.len()] = 0x80;
    if bit_len <= 894 {
        buf[111] |= 1;
        buf[112..120].copy_from_slice(&th.to_be_bytes());
        buf[120..].copy_from_slice(&tl.to_be_bytes());
        let (c0, c1) = if tail.is_empty() {
            (0, 0)
        } else {
            (tl, th)
        };
        compress_block(&mut h, &buf, c0, c1);
    } else {
        compress_block(&mut h, &buf, tl, th);
        let mut buf = [0; 128];
        buf[111] = 1;
        buf[112..120].copy_from_slice(&th.to_be_bytes());
        buf[120..].copy_from_slice(&tl.to_be_bytes());
        compress_block(&mut h, &buf, 0, 0);
    }
    output(&h)
}

#[cfg(test)]
mod tests {
    #[test]
    fn header_path_matches_generic() {
        let mut data = [0u8; 80];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(151).wrapping_add(7);
        }
        assert_eq!(super::hash80(&data), super::hash_any(&data));
    }
}
