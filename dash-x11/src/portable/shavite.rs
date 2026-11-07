//! SHAvite-3-512 of one 64-byte message (Core: `crypto/x11/shavite.cpp`, `soft_shavite::Compress`
//! and `sph_shavite512` init, one write, close).
//!
//! The 64-byte message fits the 128-byte block with its padding, so close runs one compression
//! with the bit counter at 512.

use super::aes;

/// Core: `IV512`.
pub(crate) const IV: [u32; 16] = [
    0x72FCCDD8, 0x79CA4727, 0x128A077B, 0x40D55AEC, 0xD1901A06, 0x430AE307, 0xB29F5CD1, 0xDF07FBFC,
    0x8E45D73D, 0x681AB538, 0xBDE86578, 0xDD577E47, 0xE275EADE, 0x502D9FCD, 0xB9357178, 0x022A4B9A,
];

/// The bit counter (`count0`; `count1..3` are zero).
pub(crate) const COUNTER: u32 = 512;

/// The 128-byte block close compresses, as words: the message, the padding byte, the counter
/// (bytes 110..126) and the output size in bits (bytes 126..128: `512 as u8`, then `16 >> 3`).
#[inline(always)]
pub(crate) fn final_block(input: &[u8; 64]) -> [u32; 32] {
    let mut buf = [0u8; 128];
    buf[..64].copy_from_slice(input);
    buf[64] = 0x80;
    buf[110..114].copy_from_slice(&COUNTER.to_le_bytes());
    buf[126] = 0;
    buf[127] = 2;
    core::array::from_fn(|i| u32::from_le_bytes(buf[4 * i..4 * i + 4].try_into().unwrap()))
}

/// The 448-word key schedule of Core's `Compress` for a block with counter (512, 0, 0, 0).
#[inline(always)]
fn key_schedule(block: &[u32; 32]) -> [u32; 448] {
    let (c0, c1, c2, c3) = (COUNTER, 0u32, 0u32, 0u32);
    let mut rk = [0u32; 448];
    rk[..32].copy_from_slice(block);
    let mut u = 32;
    loop {
        for _ in 0..4 {
            for _ in 0..2 {
                let x = aes::round_keyless([rk[u - 31], rk[u - 30], rk[u - 29], rk[u - 32]]);
                for j in 0..4 {
                    rk[u + j] = x[j] ^ rk[u - 4 + j];
                }
                match u {
                    32 => {
                        rk[32] ^= c0;
                        rk[33] ^= c1;
                        rk[34] ^= c2;
                        rk[35] ^= !c3;
                    }
                    164 => {
                        rk[164] ^= c3;
                        rk[165] ^= c2;
                        rk[166] ^= c1;
                        rk[167] ^= !c0;
                    }
                    316 => {
                        rk[316] ^= c2;
                        rk[317] ^= c3;
                        rk[318] ^= c0;
                        rk[319] ^= !c1;
                    }
                    440 => {
                        rk[440] ^= c1;
                        rk[441] ^= c0;
                        rk[442] ^= c3;
                        rk[443] ^= !c2;
                    }
                    _ => {}
                }
                u += 4;
            }
        }
        if u == 448 {
            break;
        }
        for _ in 0..8 {
            for j in 0..4 {
                rk[u + j] = rk[u - 32 + j] ^ rk[u - 7 + j];
            }
            u += 4;
        }
    }
    rk
}

/// Core: `CompressElement`, `l ^= AES^4(r)` keyed by four 4-word keys.
#[inline(always)]
fn compress_element(l: &mut [u32; 4], r: [u32; 4], rk: &[u32]) {
    let mut x = r;
    for k in rk.as_chunks::<4>().0 {
        for (x, k) in x.iter_mut().zip(k) {
            *x ^= k;
        }
        x = aes::round_keyless(x);
    }
    for (l, x) in l.iter_mut().zip(x) {
        *l ^= x;
    }
}

/// SHAvite-3-512 of `input`.
pub(crate) fn hash(input: &[u8; 64]) -> [u8; 64] {
    let rk = key_schedule(&final_block(input));
    // p0..pF as four 4-word quarters.
    let mut p: [[u32; 4]; 4] = core::array::from_fn(|q| core::array::from_fn(|j| IV[4 * q + j]));
    for keys in rk.as_chunks::<32>().0 {
        let [a, b, c, d] = &mut p;
        compress_element(a, *b, &keys[..16]);
        compress_element(c, *d, &keys[16..]);
        // WROT(p0, p4, p8, pC) and so on: (a, b, c, d) = (d, a, b, c).
        p = [p[3], p[0], p[1], p[2]];
    }
    let mut out = [0u8; 64];
    for (i, chunk) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        chunk.copy_from_slice(&(IV[i] ^ p[i / 4][i % 4]).to_le_bytes());
    }
    out
}
