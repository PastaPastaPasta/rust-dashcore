//! The salted hash of an [`OutPointKey`], which places it in a coins layer.
//!
//! Core salts its coins map's hash too (`SaltedOutpointHasher`, SipHash with random keys), so a
//! peer cannot choose outpoints that collide. Txids are double-SHA256 outputs, so their bits are
//! already uniform; the salt is what an attacker cannot predict. The mix is three folded
//! 64×64→128-bit multiplies over the key's words and the salt.

use std::hash::BuildHasher;

use super::key::OutPointKey;

/// A layer's hash salt: random in production, fixed under the seam and in benches, so a
/// salt-dependent order (a layer's iteration) is reproducible there (ST3a §5.6
/// item 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Salt([u64; 4]);

impl Salt {
    /// A salt from the operating system's randomness (through `std`'s randomly keyed
    /// `RandomState`, used for its keys only).
    pub fn random() -> Self {
        let rs = std::collections::hash_map::RandomState::new();
        Self(std::array::from_fn(|i| rs.hash_one(i)))
    }

    /// A salt derived from `seed` (SplitMix64).
    pub const fn fixed(seed: u64) -> Self {
        let mut x = seed;
        Self([splitmix64(&mut x), splitmix64(&mut x), splitmix64(&mut x), splitmix64(&mut x)])
    }
}

/// One SplitMix64 step: advances `state` and returns its output.
pub(crate) const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[inline]
fn folded_mul(a: u64, b: u64) -> u64 {
    let r = u128::from(a) * u128::from(b);
    (r as u64) ^ ((r >> 64) as u64)
}

/// Hashes keys under one [`Salt`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct SaltedHasher([u64; 4]);

impl SaltedHasher {
    pub(crate) fn new(salt: Salt) -> Self {
        Self(salt.0)
    }

    #[inline]
    pub(crate) fn hash(&self, k: &OutPointKey) -> u64 {
        let w = |i: usize| u64::from_le_bytes(k.0[i * 8..i * 8 + 8].try_into().unwrap());
        let vout = u64::from(k.vout());
        let a = folded_mul(w(0) ^ self.0[0], w(1) ^ self.0[1]);
        let b = folded_mul(w(2) ^ self.0[2], w(3) ^ self.0[3]);
        folded_mul(a ^ vout.rotate_left(32), b ^ 0x2d35_8dcc_aa6c_78a5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u64, vout: u32) -> OutPointKey {
        let mut k = [0u8; 36];
        let s = Salt::fixed(seed).0;
        for (i, w) in s.iter().enumerate() {
            k[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        k[32..].copy_from_slice(&vout.to_le_bytes());
        OutPointKey(k)
    }

    #[test]
    fn the_hash_depends_on_the_salt_and_every_key_part() {
        let a = SaltedHasher::new(Salt::fixed(1));
        let b = SaltedHasher::new(Salt::fixed(2));
        let k = key(7, 0);
        assert_ne!(a.hash(&k), b.hash(&k));
        assert_ne!(a.hash(&k), a.hash(&key(7, 1)));
        for byte in 0..36 {
            let mut k2 = k;
            k2.0[byte] ^= 1;
            assert_ne!(a.hash(&k), a.hash(&k2), "byte {byte}");
        }
        assert_eq!(Salt::fixed(5), Salt::fixed(5));
        assert_ne!(Salt::random(), Salt::random());
    }

    /// Every bit of the hash is used (a layer routes shards on the middle bits, hashbrown takes the
    /// low bits and the top seven): over keys that differ only in their vout, each bit is set
    /// about half the time.
    #[test]
    fn every_hash_bit_is_balanced_over_vouts() {
        let h = SaltedHasher::new(Salt::fixed(3));
        let n = 1 << 14;
        let mut ones = [0u32; 64];
        for vout in 0..n {
            let x = h.hash(&key(11, vout));
            for (bit, c) in ones.iter_mut().enumerate() {
                *c += (x >> bit) as u32 & 1;
            }
        }
        for (bit, c) in ones.iter().enumerate() {
            let p = f64::from(*c) / f64::from(n);
            assert!((0.47..0.53).contains(&p), "bit {bit}: {p}");
        }
    }
}
