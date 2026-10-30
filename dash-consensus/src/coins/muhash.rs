//! Core's `MuHash3072` (`crypto/muhash.cpp`): a rolling hash of a set, the product modulo the
//! prime 2^3072 − 1103717 of each element's 3072-bit image, with removals kept as a separate
//! denominator. `gettxoutsetinfo muhash` and the coinstats index hash the UTXO set with it (A1
//! §2.16, FR-IDX-04).
//!
//! The arithmetic is Core's limb code, ported operation for operation with 64-bit limbs, so every
//! intermediate state (and [`MuHash3072::to_bytes`], which the coinstats index stores) equals
//! Core's on a 64-bit build.

use std::ops::{DivAssign, MulAssign};

use crate::hash;

const LIMBS: usize = 48;
const BYTE_SIZE: usize = LIMBS * 8;
/// 2^3072 − 1103717, the largest 3072-bit safe prime, is the modulus.
const MAX_PRIME_DIFF: u64 = 1_103_717;

/// Core's `Num3072`: a 3072-bit number in little-endian 64-bit limbs, kept below 2^3072 but not
/// always fully reduced.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Num3072 {
    limbs: [u64; LIMBS],
}

/// Extracts the lowest limb of `[c0, c1, c2]` and shifts the number right by one limb.
fn extract3(c: &mut [u64; 3]) -> u64 {
    let n = c[0];
    *c = [c[1], c[2], 0];
    n
}

/// `[c0, c1] = a * b`.
fn mul(a: u64, b: u64) -> [u64; 3] {
    let t = u128::from(a) * u128::from(b);
    [t as u64, (t >> 64) as u64, 0]
}

/// `[c0, c1, c2] += n * [d0, d1, d2]`, with `c2` 0 on entry.
fn mulnadd3(c: &mut [u64; 3], d: &[u64; 3], n: u64) {
    let mut t = u128::from(d[0]) * u128::from(n) + u128::from(c[0]);
    c[0] = t as u64;
    t >>= 64;
    t += u128::from(d[1]) * u128::from(n) + u128::from(c[1]);
    c[1] = t as u64;
    t >>= 64;
    c[2] = (t as u64).wrapping_add(d[2].wrapping_mul(n));
}

/// `[c0, c1] *= n`.
fn muln2(c: &mut [u64; 3], n: u64) {
    let mut t = u128::from(c[0]) * u128::from(n);
    c[0] = t as u64;
    t >>= 64;
    t += u128::from(c[1]) * u128::from(n);
    c[1] = t as u64;
}

/// `[c0, c1, c2] += a * b`.
fn muladd3(c: &mut [u64; 3], a: u64, b: u64) {
    let t = u128::from(a) * u128::from(b);
    let mut th = (t >> 64) as u64;
    let tl = t as u64;
    c[0] = c[0].wrapping_add(tl);
    th += u64::from(c[0] < tl);
    c[1] = c[1].wrapping_add(th);
    c[2] = c[2].wrapping_add(u64::from(c[1] < th));
}

/// `[c0, c1, c2] += 2 * a * b`.
fn muldbladd3(c: &mut [u64; 3], a: u64, b: u64) {
    let t = u128::from(a) * u128::from(b);
    let mut th = (t >> 64) as u64;
    let tl = t as u64;
    c[0] = c[0].wrapping_add(tl);
    let tt = th + u64::from(c[0] < tl);
    c[1] = c[1].wrapping_add(tt);
    c[2] = c[2].wrapping_add(u64::from(c[1] < tt));
    c[0] = c[0].wrapping_add(tl);
    th += u64::from(c[0] < tl);
    c[1] = c[1].wrapping_add(th);
    c[2] = c[2].wrapping_add(u64::from(c[1] < th));
}

/// Adds `a` to `[c0, c1]`, then extracts the lowest limb and shifts right by one limb.
fn addnextract2(c: &mut [u64; 3], a: u64) -> u64 {
    let mut c2 = 0;
    c[0] = c[0].wrapping_add(a);
    if c[0] < a {
        c[1] = c[1].wrapping_add(1);
        if c[1] == 0 {
            c2 = 1;
        }
    }
    let n = c[0];
    *c = [c[1], c2, 0];
    n
}

impl Num3072 {
    const ONE: Num3072 = {
        let mut limbs = [0; LIMBS];
        limbs[0] = 1;
        Num3072 {
            limbs,
        }
    };

    fn from_bytes(data: &[u8]) -> Num3072 {
        let mut limbs = [0; LIMBS];
        for (limb, chunk) in limbs.iter_mut().zip(data.as_chunks::<8>().0) {
            *limb = u64::from_le_bytes(*chunk);
        }
        Num3072 {
            limbs,
        }
    }

    fn write_bytes(&self, out: &mut [u8]) {
        for (limb, chunk) in self.limbs.iter().zip(out.as_chunks_mut::<8>().0) {
            chunk.copy_from_slice(&limb.to_le_bytes());
        }
    }

    /// Whether the number is at least the modulus.
    // Core: crypto/muhash.cpp:126 Num3072::IsOverflow
    fn is_overflow(&self) -> bool {
        self.limbs[0] > u64::MAX - MAX_PRIME_DIFF && self.limbs[1..].iter().all(|&l| l == u64::MAX)
    }

    // Core: crypto/muhash.cpp:135 Num3072::FullReduce
    fn full_reduce(&mut self) {
        let mut c = [MAX_PRIME_DIFF, 0, 0];
        for limb in &mut self.limbs {
            *limb = addnextract2(&mut c, *limb);
        }
    }

    /// The second reduction and the final ones, shared by multiply and square.
    fn reduce_into(&mut self, mut c: [u64; 3], tmp: &[u64; LIMBS]) {
        muln2(&mut c, MAX_PRIME_DIFF);
        for (limb, &t) in self.limbs.iter_mut().zip(tmp) {
            *limb = addnextract2(&mut c, t);
        }
        debug_assert!(c[1] == 0 && c[0] <= 1);
        if self.is_overflow() {
            self.full_reduce();
        }
        if c[0] != 0 {
            self.full_reduce();
        }
    }

    // Core: crypto/muhash.cpp:181 Num3072::Multiply
    fn multiply(&mut self, a: &Num3072) {
        let (s, a) = (&self.limbs, &a.limbs);
        let mut c = [0; 3];
        let mut tmp = [0; LIMBS];
        // Limbs 0..N-2 of self * a into tmp, including one reduction.
        for j in 0..LIMBS - 1 {
            let mut d = mul(s[1 + j], a[LIMBS - 1]);
            for i in 2 + j..LIMBS {
                muladd3(&mut d, s[i], a[LIMBS + j - i]);
            }
            mulnadd3(&mut c, &d, MAX_PRIME_DIFF);
            for i in 0..=j {
                muladd3(&mut c, s[i], a[j - i]);
            }
            tmp[j] = extract3(&mut c);
        }
        // Limb N-1.
        debug_assert_eq!(c[2], 0);
        for i in 0..LIMBS {
            muladd3(&mut c, s[i], a[LIMBS - 1 - i]);
        }
        tmp[LIMBS - 1] = extract3(&mut c);
        self.reduce_into(c, &tmp);
    }

    // Core: crypto/muhash.cpp:218 Num3072::Square
    fn square(&mut self) {
        let s = &self.limbs;
        let mut c = [0; 3];
        let mut tmp = [0; LIMBS];
        for j in 0..LIMBS - 1 {
            let mut d = [0; 3];
            for i in 0..(LIMBS - 1 - j) / 2 {
                muldbladd3(&mut d, s[i + j + 1], s[LIMBS - 1 - i]);
            }
            if (j + 1) & 1 == 1 {
                let i = (LIMBS - 1 - j) / 2;
                muladd3(&mut d, s[i + j + 1], s[LIMBS - 1 - i]);
            }
            mulnadd3(&mut c, &d, MAX_PRIME_DIFF);
            for i in 0..j.div_ceil(2) {
                muldbladd3(&mut c, s[i], s[j - i]);
            }
            if (j + 1) & 1 == 1 {
                muladd3(&mut c, s[j.div_ceil(2)], s[j - j.div_ceil(2)]);
            }
            tmp[j] = extract3(&mut c);
        }
        debug_assert_eq!(c[2], 0);
        for i in 0..LIMBS / 2 {
            muldbladd3(&mut c, s[i], s[LIMBS - 1 - i]);
        }
        tmp[LIMBS - 1] = extract3(&mut c);
        self.reduce_into(c, &tmp);
    }

    /// `self^(2^sq) * mul`.
    fn square_n_mul(&mut self, sq: usize, mul: &Num3072) {
        for _ in 0..sq {
            self.square();
        }
        self.multiply(mul);
    }

    /// The inverse by exponentiation to p − 2: a sliding window with repunit precomputation
    /// (Brumley, Järvinen 2008).
    // Core: crypto/muhash.cpp:144 Num3072::GetInverse
    fn inverse(&self) -> Num3072 {
        // p[i] = a^(2^(2^i) - 1)
        let mut p = [*self; 12];
        for i in 0..11 {
            p[i + 1] = p[i];
            for _ in 0..(1 << i) {
                p[i + 1].square();
            }
            let pi = p[i];
            p[i + 1].multiply(&pi);
        }
        let mut out = p[11];
        for (sq, k) in [
            (512, 9),
            (256, 8),
            (128, 7),
            (64, 6),
            (32, 5),
            (8, 3),
            (2, 1),
            (1, 0),
            (5, 2),
            (3, 0),
            (2, 0),
            (4, 0),
            (4, 1),
            (3, 0),
        ] {
            out.square_n_mul(sq, &p[k]);
        }
        out
    }

    // Core: crypto/muhash.cpp:261 Num3072::Divide
    fn divide(&mut self, a: &Num3072) {
        if self.is_overflow() {
            self.full_reduce();
        }
        let inv = if a.is_overflow() {
            let mut b = *a;
            b.full_reduce();
            b.inverse()
        } else {
            a.inverse()
        };
        self.multiply(&inv);
        if self.is_overflow() {
            self.full_reduce();
        }
    }
}

/// ChaCha20's keystream for `key`, with nonce and block counter zero: Core's
/// `ChaCha20Aligned{key}.Keystream`.
fn chacha20_keystream(key: &[u8; 32], out: &mut [u8; BYTE_SIZE]) {
    fn quarter(x: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
        x[a] = x[a].wrapping_add(x[b]);
        x[d] = (x[d] ^ x[a]).rotate_left(16);
        x[c] = x[c].wrapping_add(x[d]);
        x[b] = (x[b] ^ x[c]).rotate_left(12);
        x[a] = x[a].wrapping_add(x[b]);
        x[d] = (x[d] ^ x[a]).rotate_left(8);
        x[c] = x[c].wrapping_add(x[d]);
        x[b] = (x[b] ^ x[c]).rotate_left(7);
    }
    let mut input = [0u32; 16];
    // "expand 32-byte k"
    input[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    for (word, chunk) in input[4..12].iter_mut().zip(key.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*chunk);
    }
    for (counter, block) in out.as_chunks_mut::<64>().0.iter_mut().enumerate() {
        input[12] = counter as u32;
        let mut x = input;
        for _ in 0..10 {
            quarter(&mut x, 0, 4, 8, 12);
            quarter(&mut x, 1, 5, 9, 13);
            quarter(&mut x, 2, 6, 10, 14);
            quarter(&mut x, 3, 7, 11, 15);
            quarter(&mut x, 0, 5, 10, 15);
            quarter(&mut x, 1, 6, 11, 12);
            quarter(&mut x, 2, 7, 8, 13);
            quarter(&mut x, 3, 4, 9, 14);
        }
        for ((word, init), chunk) in x.iter().zip(&input).zip(block.as_chunks_mut::<4>().0) {
            chunk.copy_from_slice(&word.wrapping_add(*init).to_le_bytes());
        }
    }
}

/// Core's `MuHash3072`: a numerator and a denominator, both 1 when empty. There is no equality,
/// as in Core: equal sets built in different orders hold different fractions; compare the
/// finalized hashes.
#[derive(Clone)]
pub struct MuHash3072 {
    numerator: Num3072,
    denominator: Num3072,
}

impl Default for MuHash3072 {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MuHash3072 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MuHash3072")
    }
}

impl MuHash3072 {
    /// The serialized size: the numerator's and the denominator's 384 bytes.
    pub const SERIALIZED_SIZE: usize = 2 * BYTE_SIZE;

    /// The hash of the empty set.
    pub fn new() -> Self {
        Self {
            numerator: Num3072::ONE,
            denominator: Num3072::ONE,
        }
    }

    /// `MuHash3072(in)`: the hash of the set holding `data` alone.
    // Core: crypto/muhash.cpp:309 MuHash3072::MuHash3072
    pub fn from_data(data: &[u8]) -> Self {
        Self {
            numerator: Self::to_num3072(data),
            denominator: Num3072::ONE,
        }
    }

    /// An element's image: the ChaCha20 keystream keyed by its SHA256.
    // Core: crypto/muhash.cpp:298 MuHash3072::ToNum3072
    fn to_num3072(data: &[u8]) -> Num3072 {
        let mut tmp = [0; BYTE_SIZE];
        chacha20_keystream(&hash::sha256(data), &mut tmp);
        Num3072::from_bytes(&tmp)
    }

    /// `Insert`: adds `data` to the set.
    // Core: crypto/muhash.cpp:339 MuHash3072::Insert
    pub fn insert(&mut self, data: &[u8]) {
        self.numerator.multiply(&Self::to_num3072(data));
    }

    /// `Remove`: removes `data` from the set.
    // Core: crypto/muhash.cpp:344 MuHash3072::Remove
    pub fn remove(&mut self, data: &[u8]) {
        self.denominator.multiply(&Self::to_num3072(data));
    }

    /// `Finalize`: divides out the denominator (leaving it 1, as Core does) and returns the SHA256
    /// of the numerator's 384 little-endian bytes, in Core's internal byte order (RPC prints it
    /// reversed).
    // Core: crypto/muhash.cpp:314 MuHash3072::Finalize
    pub fn finalize(&mut self) -> [u8; 32] {
        self.numerator.divide(&self.denominator);
        self.denominator = Num3072::ONE;
        let mut data = [0; BYTE_SIZE];
        self.numerator.write_bytes(&mut data);
        hash::sha256(&data)
    }

    /// Core's serialization: the numerator's limbs, then the denominator's, each little-endian.
    pub fn to_bytes(&self) -> [u8; Self::SERIALIZED_SIZE] {
        let mut out = [0; Self::SERIALIZED_SIZE];
        let (numerator, denominator) = out.split_at_mut(BYTE_SIZE);
        self.numerator.write_bytes(numerator);
        self.denominator.write_bytes(denominator);
        out
    }

    /// Reads [`MuHash3072::to_bytes`]' form. Core reads any limbs, values at or above the modulus
    /// included.
    pub fn from_bytes(bytes: &[u8; Self::SERIALIZED_SIZE]) -> Self {
        let (numerator, denominator) = bytes.split_at(BYTE_SIZE);
        Self {
            numerator: Num3072::from_bytes(numerator),
            denominator: Num3072::from_bytes(denominator),
        }
    }
}

/// `operator*=`: the union of two disjoint sets.
// Core: crypto/muhash.cpp:325 MuHash3072::operator*=
impl MulAssign<&MuHash3072> for MuHash3072 {
    fn mul_assign(&mut self, mul: &MuHash3072) {
        self.numerator.multiply(&mul.numerator);
        self.denominator.multiply(&mul.denominator);
    }
}

/// `operator/=`: removes a set's elements.
// Core: crypto/muhash.cpp:332 MuHash3072::operator/=
impl DivAssign<&MuHash3072> for MuHash3072 {
    fn div_assign(&mut self, div: &MuHash3072) {
        self.numerator.multiply(&div.denominator);
        self.denominator.multiply(&div.numerator);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_core_vectors::hex::{hex_str, uint256_from_hex};

    /// Core's `FromInt`: the set holding the 32-byte element `{i, 0, ..}`.
    fn from_int(i: u8) -> MuHash3072 {
        let mut element = [0; 32];
        element[0] = i;
        MuHash3072::from_data(&element)
    }

    // Core: test/crypto_tests.cpp:1214 muhash_tests (v24.0.0-rc.3), the fixed vectors.
    #[test]
    fn core_vectors() {
        let mut acc = from_int(0);
        acc *= &from_int(1);
        acc /= &from_int(2);
        let expected =
            uint256_from_hex("10d312b100cbd32ada024a6646e40d3482fcff103668d2625f10002a607d5863");
        assert_eq!(acc.finalize(), expected);

        let mut acc2 = from_int(0);
        let (mut one, mut two) = ([0; 32], [0; 32]);
        one[0] = 1;
        two[0] = 2;
        acc2.insert(&one);
        acc2.remove(&two);
        assert_eq!(acc2.finalize(), expected);

        let mut serchk = from_int(1);
        serchk *= &from_int(2);
        let serialized = serchk.to_bytes();
        let expected_prefix = "1fa093295ea30a6a3acdc7b3f770fa538eff537528e990e2910e40bbcfd7f6696b1256901929094694b56316de342f593303dd12ac43e06dce1be1ff8301c845beb15468fff0ef002dbf80c29f26e6452bccc91b5cb9437ad410d2a67ea847887fa3c6a6553309946880fe20db2c73fe0641adbd4e86edfee0d9f8cd0ee1230898873dc13ed8ddcaf045c80faa082774279007a2253f8922ee3ef361d378a6af3ddaf180b190ac97e556888c36b3d1fb1c85aab9ccd46e3deaeb7b7cf5db067a7e9ff86b658cf3acd6662bbcce37232daa753c48b794356c020090c831a8304416e2aa7ad633c0ddb2f11be1be316a81be7f7e472071c042cb68faef549c221ebff209273638b741aba5a81675c45a5fa92fea4ca821d7a324cb1e1a2ccd3b76c4228ec8066dad2a5df6e1bd0de45c7dd5de8070bdb46db6c554cf9aefc9b7b2bbf9f75b1864d9f95005314593905c0109b71f703d49944ae94477b51dac10a816bb6d1c700bafabc8bd86fac8df24be519a2f2836b16392e18036cb13e48c5c01";
        let expected_ser =
            format!("{expected_prefix}{}", "0".repeat(2 * 768 - expected_prefix.len()));
        assert_eq!(hex_str(&serialized), expected_ser);
        let mut deserchk = MuHash3072::from_bytes(&serialized);
        assert_eq!(deserchk.finalize(), serchk.finalize());

        // Overflow: a numerator above the modulus (every bit set), denominator 1.
        let mut max = [0xff; MuHash3072::SERIALIZED_SIZE];
        max[BYTE_SIZE..].fill(0);
        max[BYTE_SIZE] = 1;
        let mut overflow = MuHash3072::from_bytes(&max);
        assert_eq!(
            hex_str(&overflow.finalize()),
            "3a31e6903aff0de9f62f9a9f7f8b861de76ce2cda09822b90014319ae5dc2271"
        );
    }

    /// Core's randomized part: the order of multiplications and divisions does not matter, and
    /// X·Y / (Y·X) is the empty set.
    #[test]
    fn order_independence() {
        for seed in 0u8..10 {
            let table: Vec<u8> =
                (0..4).map(|i| seed.wrapping_mul(37).wrapping_add(i * 11) % 8).collect();
            let mut results = Vec::new();
            for order in 0..4 {
                let mut acc = MuHash3072::new();
                for i in 0..4 {
                    let t = table[i ^ order];
                    if t & 4 != 0 {
                        acc /= &from_int(t & 3);
                    } else {
                        acc *= &from_int(t & 3);
                    }
                }
                results.push(acc.finalize());
            }
            assert!(results.windows(2).all(|w| w[0] == w[1]));

            let x = from_int(seed);
            let mut y = from_int(seed.wrapping_mul(7) % 16);
            let mut z = MuHash3072::new();
            z *= &x;
            z *= &y;
            y *= &x;
            z /= &y;
            assert_eq!(z.finalize(), MuHash3072::new().finalize());
        }
    }

    /// RFC 8439 §2.3.2's block function vector at counter 0 is not ChaCha20Aligned's input (it
    /// has a nonce); the all-zero key's first block is the classic ChaCha20 test vector.
    #[test]
    fn chacha20_zero_key_block() {
        let mut out = [0; BYTE_SIZE];
        chacha20_keystream(&[0; 32], &mut out);
        assert_eq!(
            hex_str(&out[..64]),
            "76b8e0ada0f13d90405d6ae55386bd28bdd219b8a08ded1aa836efcc8b770dc7da41597c5157488d7724e03fb8d84a376a43b8f41518a11cc387b669b2ee6586"
        );
    }
}
