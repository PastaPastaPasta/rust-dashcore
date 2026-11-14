//! ECHO-512 of one 64-byte message (Core: `crypto/x11/echo.cpp`, the `soft_echo` round
//! functions and `sph_echo512` init, one write, close).
//!
//! The 64-byte message fills half of ECHO's 128-byte block, so close pads it in place and runs
//! one compression with the bit counter at 512; the counter's carry never reaches the second
//! word (it ends at 512 + 160).

use super::aes;

/// The bit counter after the message, which is also the first round's key word.
pub(crate) const COUNTER: u32 = 512;

/// The 128-byte block close compresses: the message, the padding byte, the output size in bits
/// (512, at offset 110) and the bit counter (offset 112).
#[inline(always)]
pub(crate) fn final_block(input: &[u8; 64]) -> [u8; 128] {
    let mut buf = [0u8; 128];
    buf[..64].copy_from_slice(input);
    buf[64] = 0x80;
    buf[110..112].copy_from_slice(&512u16.to_le_bytes());
    buf[112..116].copy_from_slice(&COUNTER.to_le_bytes());
    buf
}

/// `x * 2` in GF(2^8) for each byte of `x`.
#[inline(always)]
fn mul2(x: u64) -> u64 {
    ((x & 0x8080_8080_8080_8080) >> 7).wrapping_mul(27) ^ ((x & 0x7f7f_7f7f_7f7f_7f7f) << 1)
}

/// Core: `soft_echo::MixColumn`, on each 64-bit half of the four cells.
#[inline(always)]
fn mix_column(w: &mut [[u64; 2]; 16], ia: usize, ib: usize, ic: usize, id: usize) {
    for n in [0, 1] {
        let (a, b, c, d) = (w[ia][n], w[ib][n], w[ic][n], w[id][n]);
        let ab = a ^ b;
        let bc = b ^ c;
        let cd = c ^ d;
        let abx = mul2(ab);
        let bcx = mul2(bc);
        let cdx = mul2(cd);
        w[ia][n] = abx ^ bc ^ d;
        w[ib][n] = bcx ^ a ^ cd;
        w[ic][n] = cdx ^ ab ^ d;
        w[id][n] = abx ^ bcx ^ cdx ^ ab ^ c;
    }
}

/// Core: `soft_echo::ShiftAndMix`.
#[inline(always)]
fn shift_and_mix(w: &mut [[u64; 2]; 16]) {
    // SHIFT_ROW1(1, 5, 9, 13), SHIFT_ROW2(2, 6, 10, 14), SHIFT_ROW3(3, 7, 11, 15).
    let t = w[1];
    w[1] = w[5];
    w[5] = w[9];
    w[9] = w[13];
    w[13] = t;
    w.swap(2, 10);
    w.swap(6, 14);
    let t = w[15];
    w[15] = w[11];
    w[11] = w[7];
    w[7] = w[3];
    w[3] = t;
    mix_column(w, 0, 1, 2, 3);
    mix_column(w, 4, 5, 6, 7);
    mix_column(w, 8, 9, 10, 11);
    mix_column(w, 12, 13, 14, 15);
}

/// Core: `soft_echo::FullStateRound`, with the counter's carry dropped (see the module docs).
#[inline(always)]
fn full_state_round(w: &mut [[u64; 2]; 16], k0: &mut u32) {
    for cell in w.iter_mut() {
        let x = [cell[0] as u32, (cell[0] >> 32) as u32, cell[1] as u32, (cell[1] >> 32) as u32];
        let y = aes::round_keyless(aes::round(x, [*k0, 0, 0, 0]));
        *cell = [y[0] as u64 | (y[1] as u64) << 32, y[2] as u64 | (y[3] as u64) << 32];
        *k0 += 1;
    }
}

/// ECHO-512 of `input`.
pub(crate) fn hash(input: &[u8; 64]) -> [u8; 64] {
    let buf = final_block(input);
    let le = |i: usize| u64::from_le_bytes(buf[8 * i..8 * i + 8].try_into().unwrap());
    // The chaining value starts as eight cells of (512, 0); the block fills the other eight.
    let mut w = [[0u64; 2]; 16];
    for (i, cell) in w.iter_mut().enumerate() {
        *cell = if i < 8 {
            [512, 0]
        } else {
            [le(2 * i - 16), le(2 * i - 15)]
        };
    }
    let mut k0 = COUNTER;
    for _ in 0..10 {
        full_state_round(&mut w, &mut k0);
        shift_and_mix(&mut w);
    }
    // FINAL_BIG for the four cells of output: V ^= block ^ W[cell] ^ W[cell + 8].
    let mut out = [0u8; 64];
    for (i, chunk) in out.as_chunks_mut::<16>().0.iter_mut().enumerate() {
        let lo = 512 ^ le(2 * i) ^ w[i][0] ^ w[i + 8][0];
        let hi = le(2 * i + 1) ^ w[i][1] ^ w[i + 8][1];
        chunk[..8].copy_from_slice(&lo.to_le_bytes());
        chunk[8..].copy_from_slice(&hi.to_le_bytes());
    }
    out
}
