//! Luffa-512 (Core: `src/crypto/x11/luffa.c`), specialised to one 64-byte message: five
//! rounds of message injection `MI5` and permutation `P5`, over the two message blocks, the
//! padding block `80 00 .. 00` and two zero blocks; the digest is the XOR of the five
//! sub-states after the fourth and the fifth (`luffa5_close`).
//!
//! Message and digest words are big-endian (`sph_dec32be`, `sph_enc32be`). `MI5` runs on
//! pairs of 32-bit words packed in 64-bit words (see [`Sub`]); `P5` runs each `Q_j` on 32-bit
//! words with its eight rounds unrolled, as sph does without `SPH_LUFFA_PARALLEL` (Core sets
//! it; the result is the same, and sph's 2x32-bit `Q_j` pairs measured more instructions).

/// sph's `V_INIT`.
#[rustfmt::skip]
pub(crate) const V_INIT: [[u32; 8]; 5] = [
    [0x6d251e69, 0x44b051e0, 0x4eaa6fb4, 0xdbf78465, 0x6e292011, 0x90152df4, 0xee058139, 0xdef610bb],
    [0xc3b44b95, 0xd9d2f256, 0x70eee9a0, 0xde099fa3, 0x5d9b0557, 0x8fc944b3, 0xcf1ccf0e, 0x746cd581],
    [0xf7efc89d, 0x5dba5781, 0x04016ce5, 0xad659c05, 0x0306194f, 0x666d1836, 0x24aa230a, 0x8b264ae7],
    [0x858075d5, 0x36d79cce, 0xe571f7d7, 0x204b1f67, 0x35870c6a, 0x57e9e923, 0x14bcb808, 0x7cde72ce],
    [0x6c68e9be, 0x5ec41e22, 0xc825b7c7, 0xaffb4363, 0xf5df3999, 0x0fc688f1, 0xb07224cc, 0x03e86cea],
];

/// The round constants of `Q_j`: sph's `RCj0` (XORed into word 0).
#[rustfmt::skip]
pub(crate) const RC0: [[u32; 8]; 5] = [
    [0x303994a6, 0xc0e65299, 0x6cc33a12, 0xdc56983e, 0x1e00108f, 0x7800423d, 0x8f5b7882, 0x96e1db12],
    [0xb6de10ed, 0x70f47aae, 0x0707a3d4, 0x1c1e8f51, 0x707a3d45, 0xaeb28562, 0xbaca1589, 0x40a46f3e],
    [0xfc20d9d2, 0x34552e25, 0x7ad8818f, 0x8438764a, 0xbb6de032, 0xedb780c8, 0xd9847356, 0xa2c78434],
    [0xb213afa5, 0xc84ebe95, 0x4e608a22, 0x56d858fe, 0x343b138f, 0xd0ec4e3d, 0x2ceb4882, 0xb3ad2208],
    [0xf0d2e9e3, 0xac11d7fa, 0x1bcb66f2, 0x6f2d9bc9, 0x78602649, 0x8edae952, 0x3b6ba548, 0xedae9520],
];

/// The round constants of `Q_j`: sph's `RCj4` (XORed into word 4).
#[rustfmt::skip]
pub(crate) const RC4: [[u32; 8]; 5] = [
    [0xe0337818, 0x441ba90d, 0x7f34d442, 0x9389217f, 0xe5a8bce6, 0x5274baf4, 0x26889ba7, 0x9a226e9d],
    [0x01685f3d, 0x05a17cf4, 0xbd09caca, 0xf4272b28, 0x144ae5cc, 0xfaa7ae2b, 0x2e48f1c1, 0xb923c704],
    [0xe25e72c1, 0xe623bb72, 0x5c58a4a4, 0x1e38e2e7, 0x78e38b9d, 0x27586719, 0x36eda57f, 0x703aace7],
    [0xe028c9bf, 0x44756f91, 0x7e8fce32, 0x956548be, 0xfe191be2, 0x3cb226e5, 0x5944a28e, 0xa1c4c355],
    [0x5090d577, 0x2d1925ab, 0xb46496ac, 0xd1925ab0, 0x29131ab6, 0x0fc053c3, 0x3f014f0c, 0xfc053c31],
];

/// A sub-state (sph's `Vj0..Vj7`, written `x0..x7` here), as four 64-bit words
/// `p[i] = x[i] | x[i + 4] << 32` (the form `MI5` works on) stored little-endian, so that its
/// bytes are also the 32-bit words `x0, x4, x1, x5, x2, x6, x3, x7` (the form `P5` works on).
type Sub = [u8; 32];

/// The state: five sub-states.
type State = [Sub; 5];

/// Word `p[i]` of a sub-state.
#[inline(always)]
fn get64(s: &Sub, i: usize) -> u64 {
    u64::from_le_bytes(s[8 * i..8 * i + 8].try_into().unwrap())
}

/// Sets word `p[i]` of a sub-state.
#[inline(always)]
fn set64(s: &mut Sub, i: usize, w: u64) {
    s[8 * i..8 * i + 8].copy_from_slice(&w.to_le_bytes());
}

/// The byte offset of `x[k]` in a sub-state.
const fn off32(k: usize) -> usize {
    8 * (k % 4) + 4 * (k / 4)
}

/// sph's `M2` (multiplication by 2 in Luffa's ring) on the 64-bit form: `x[0] = x[7]`,
/// `x[k] = x[k - 1]` and `x[7]` XORed into `x[1]`, `x[3]` and `x[4]`.
#[inline(always)]
fn m2(p: [u64; 4]) -> [u64; 4] {
    let x7 = p[3] >> 32;
    [(p[3] ^ x7).rotate_left(32), p[0] ^ x7, p[1], p[2] ^ x7]
}

/// Word-wise XOR.
#[inline(always)]
fn xor(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    [a[0] ^ b[0], a[1] ^ b[1], a[2] ^ b[2], a[3] ^ b[3]]
}

/// The 64-bit form of a sub-state.
#[inline(always)]
fn load(s: &Sub) -> [u64; 4] {
    [get64(s, 0), get64(s, 1), get64(s, 2), get64(s, 3)]
}

/// Stores the 64-bit form of a sub-state.
#[inline(always)]
fn store(s: &mut Sub, p: [u64; 4]) {
    for (i, w) in p.into_iter().enumerate() {
        set64(s, i, w);
    }
}

/// sph's `MI5`: injects the message block `m` (64-bit form).
#[inline(never)]
fn mi5(v: &mut State, m: [u64; 4]) {
    let a = m2(xor(xor(xor(load(&v[0]), load(&v[1])), xor(load(&v[2]), load(&v[3]))), load(&v[4])));
    let v0 = xor(load(&v[0]), a);
    let v1 = xor(load(&v[1]), a);
    let b = xor(m2(v0), v1);
    let v2 = xor(load(&v[2]), a);
    let v1 = xor(m2(v1), v2);
    let v3 = xor(load(&v[3]), a);
    let v2 = xor(m2(v2), v3);
    let v4 = xor(load(&v[4]), a);
    let v3 = xor(m2(v3), v4);
    let v4 = xor(m2(v4), v0);
    let v0 = xor(m2(b), v4);
    let v4 = xor(m2(v4), v3);
    let v3 = xor(m2(v3), v2);
    let v2 = xor(m2(v2), v1);
    let v1 = xor(m2(v1), b);
    let mut m = m;
    for (s, vj) in v.iter_mut().zip([v0, v1, v2, v3, v4]) {
        store(s, xor(vj, m));
        m = m2(m);
    }
}

/// sph's `SUB_CRUMB`.
#[inline(always)]
fn sub_crumb(mut a0: u32, mut a1: u32, mut a2: u32, mut a3: u32) -> [u32; 4] {
    let mut tmp = a0;
    a0 |= a1;
    a2 ^= a3;
    a1 = !a1;
    a0 ^= a3;
    a3 &= tmp;
    a1 ^= a3;
    a3 ^= a2;
    a2 &= a0;
    a0 = !a0;
    a2 ^= a1;
    a1 |= a3;
    tmp ^= a1;
    a3 ^= a2;
    a2 &= a1;
    a1 ^= a0;
    [tmp, a1, a2, a3]
}

/// sph's `MIX_WORD`.
#[inline(always)]
fn mix_word(mut u: u32, mut v: u32) -> (u32, u32) {
    v ^= u;
    u = u.rotate_left(2) ^ v;
    v = v.rotate_left(14) ^ u;
    u = u.rotate_left(10) ^ v;
    v = v.rotate_left(1);
    (u, v)
}

/// The permutation `Q_j`: `TWEAK5` and the eight rounds of sph's `P5` on sub-state `j`.
#[inline(always)]
fn q<const J: usize>(s: &mut Sub) {
    let mut x: [u32; 8] =
        core::array::from_fn(|k| u32::from_le_bytes(s[off32(k)..off32(k) + 4].try_into().unwrap()));
    for xk in &mut x[4..] {
        *xk = xk.rotate_left(J as u32);
    }
    let round = |x: [u32; 8], r: usize| {
        let [a0, a1, a2, a3] = sub_crumb(x[0], x[1], x[2], x[3]);
        let [a5, a6, a7, a4] = sub_crumb(x[5], x[6], x[7], x[4]);
        let (a0, a4) = mix_word(a0, a4);
        let (a1, a5) = mix_word(a1, a5);
        let (a2, a6) = mix_word(a2, a6);
        let (a3, a7) = mix_word(a3, a7);
        [a0 ^ RC0[J][r], a1, a2, a3, a4 ^ RC4[J][r], a5, a6, a7]
    };
    x = round(x, 0);
    x = round(x, 1);
    x = round(x, 2);
    x = round(x, 3);
    x = round(x, 4);
    x = round(x, 5);
    x = round(x, 6);
    x = round(x, 7);
    for (k, xk) in x.into_iter().enumerate() {
        s[off32(k)..off32(k) + 4].copy_from_slice(&xk.to_le_bytes());
    }
}

/// sph's `P5`.
#[inline(never)]
fn p5(v: &mut State) {
    q::<0>(&mut v[0]);
    q::<1>(&mut v[1]);
    q::<2>(&mut v[2]);
    q::<3>(&mut v[3]);
    q::<4>(&mut v[4]);
}

/// `MI5` then `P5`.
#[inline(always)]
fn round(v: &mut State, m: [u64; 4]) {
    mi5(v, m);
    p5(v);
}

/// The digest words: the XOR of the five sub-states, big-endian.
#[inline(always)]
fn put(out: &mut [u8], v: &State) {
    let p = xor(xor(xor(load(&v[0]), load(&v[1])), xor(load(&v[2]), load(&v[3]))), load(&v[4]));
    for (i, w) in p.into_iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&(w as u32).to_be_bytes());
        out[16 + 4 * i..20 + 4 * i].copy_from_slice(&((w >> 32) as u32).to_be_bytes());
    }
}

/// The 64-bit form of eight 32-bit words.
const fn pack(x: &[u32; 8]) -> [u64; 4] {
    let mut p = [0; 4];
    let mut i = 0;
    while i < 4 {
        p[i] = x[i] as u64 | (x[i + 4] as u64) << 32;
        i += 1;
    }
    p
}

/// `V_INIT` in the stored form.
const INIT: State = {
    let mut v = [[0; 32]; 5];
    let mut j = 0;
    while j < 5 {
        let p = pack(&V_INIT[j]);
        let mut b = 0;
        while b < 32 {
            v[j][b] = p[b / 8].to_le_bytes()[b % 8];
            b += 1;
        }
        j += 1;
    }
    v
};

/// `sph_luffa512_init`, `sph_luffa512(input, 64)`, `sph_luffa512_close`.
pub(crate) fn hash(input: &[u8; 64]) -> [u8; 64] {
    let mut v = INIT;
    for block in input.as_chunks::<32>().0 {
        let w = |k: usize| u32::from_be_bytes(block.as_chunks::<4>().0[k]) as u64;
        round(&mut v, [w(0) | w(4) << 32, w(1) | w(5) << 32, w(2) | w(6) << 32, w(3) | w(7) << 32]);
    }
    round(&mut v, pack(&[0x8000_0000, 0, 0, 0, 0, 0, 0, 0]));
    let mut out = [0; 64];
    round(&mut v, [0; 4]);
    put(&mut out[..32], &v);
    round(&mut v, [0; 4]);
    put(&mut out[32..], &v);
    out
}
