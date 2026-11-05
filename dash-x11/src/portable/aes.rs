//! The software AES round ECHO and SHAvite-3 use (Core: `crypto/x11/aes.cpp` and the tables of
//! `crypto/x11/util/consts_aes.hpp`, computed at compile time the same way).

/// `x * 2` in GF(2^8) with AES's polynomial (Core: `util::gf8_mul2`).
const fn gf8_mul2(x: u8) -> u8 {
    (x << 1)
        ^ if x & 0x80 != 0 {
            0x1b
        } else {
            0
        }
}

/// Core: `util::gf8_log`. `x` steps through the powers of 3, so the last write sets
/// `log[1] = 255`, as in Core.
const GF8_LOG: [u8; 256] = {
    let mut ret = [0u8; 256];
    let mut x: u8 = 1;
    let mut idx = 0;
    while idx < 256 {
        ret[x as usize] = idx as u8;
        x ^= gf8_mul2(x);
        idx += 1;
    }
    ret
};

/// Core: `util::gf8_pow`.
const GF8_POW: [u8; 256] = {
    let mut ret = [0u8; 256];
    let mut x: u8 = 1;
    let mut idx = 0;
    while idx < 256 {
        ret[idx] = x;
        x ^= gf8_mul2(x);
        idx += 1;
    }
    ret
};

/// Core: `util::gf8_mul`.
pub(crate) const fn gf8_mul(x: u8, y: u8) -> u8 {
    if x != 0 && y != 0 {
        GF8_POW[(GF8_LOG[x as usize] as usize + GF8_LOG[y as usize] as usize) % 255]
    } else {
        0
    }
}

/// The AES S-box (Core: `consts::aes_sbox_f`).
const SBOX: [u8; 256] = {
    let mut ret = [0u8; 256];
    ret[0] = 0x63;
    let mut idx = 1;
    while idx < 256 {
        let mut x = GF8_POW[255 - GF8_LOG[idx] as usize];
        let mut y = x.rotate_left(1);
        x ^= y;
        y = y.rotate_left(1);
        x ^= y;
        y = y.rotate_left(1);
        x ^= y;
        y = y.rotate_left(1);
        x ^= y ^ 0x63;
        ret[idx] = x;
        idx += 1;
    }
    ret
};

/// SubBytes and MixColumns of one byte in each column position, little-endian (Core:
/// `consts::aes_tbox_le`).
const TBOX: [[u32; 256]; 4] = {
    let mut ret = [[0u32; 256]; 4];
    let mut idx = 0;
    while idx < 256 {
        let byte = SBOX[idx];
        let mut word = ((gf8_mul(byte, 3) as u32) << 24)
            | ((byte as u32) << 16)
            | ((byte as u32) << 8)
            | gf8_mul(byte, 2) as u32;
        let mut j = 0;
        while j < 4 {
            ret[j][idx] = word;
            word = word.rotate_left(8);
            j += 1;
        }
        idx += 1;
    }
    ret
};

// Core's static_assert on the tables.
const _: () = assert!(
    TBOX[0][0] == 0xa563_63c6
        && TBOX[0][255] == 0x3a16_162c
        && TBOX[3][0] == 0xc6a5_6363
        && TBOX[3][255] == 0x2c3a_1616
);

/// One AES encryption round of the column words `x` with round key `k` (Core:
/// `soft_aes::Round`), the same as x86's `AESENC` on the little-endian words.
#[inline(always)]
pub(crate) fn round(x: [u32; 4], k: [u32; 4]) -> [u32; 4] {
    let t = |a: u32, b: u32, c: u32, d: u32| {
        TBOX[0][(a & 0xff) as usize]
            ^ TBOX[1][((b >> 8) & 0xff) as usize]
            ^ TBOX[2][((c >> 16) & 0xff) as usize]
            ^ TBOX[3][(d >> 24) as usize]
    };
    [
        t(x[0], x[1], x[2], x[3]) ^ k[0],
        t(x[1], x[2], x[3], x[0]) ^ k[1],
        t(x[2], x[3], x[0], x[1]) ^ k[2],
        t(x[3], x[0], x[1], x[2]) ^ k[3],
    ]
}

/// [`round`] with a zero key (Core: `soft_aes::RoundKeyless`).
#[inline(always)]
pub(crate) fn round_keyless(x: [u32; 4]) -> [u32; 4] {
    round(x, [0; 4])
}
