//! Core's `CRollingBloomFilter` (`common/bloom.cpp`): the discouragement filter and each peer's
//! `m_addr_known`.

/// Core's `MurmurHash3` (x86_32, `hash.cpp`).
pub fn murmur3(seed: u32, data: &[u8]) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;
    let mut h1 = seed;
    let (blocks, tail) = data.as_chunks::<4>();
    for block in blocks {
        let mut k1 = u32::from_le_bytes(*block);
        k1 = k1.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h1 ^= k1;
        h1 = h1.rotate_left(13).wrapping_mul(5).wrapping_add(0xe654_6b64);
    }
    if !tail.is_empty() {
        let mut k1 = 0u32;
        for (i, &b) in tail.iter().enumerate() {
            k1 ^= u32::from(b) << (8 * i);
        }
        k1 = k1.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h1 ^= k1;
    }
    h1 ^= data.len() as u32;
    h1 ^= h1 >> 16;
    h1 = h1.wrapping_mul(0x85eb_ca6b);
    h1 ^= h1 >> 13;
    h1 = h1.wrapping_mul(0xc2b2_ae35);
    h1 ^ (h1 >> 16)
}

/// Core's `FastRange32`.
fn fast_range32(x: u32, n: u32) -> u32 {
    ((u64::from(x) * u64::from(n)) >> 32) as u32
}

/// Core's `CRollingBloomFilter`: about `elements` recent entries at false-positive rate
/// `fp_rate`, in three generations of `elements / 2`.
#[derive(Clone, Debug)]
pub struct RollingBloom {
    entries_per_generation: u32,
    entries_this_generation: u32,
    generation: u32,
    hash_funcs: u32,
    tweak: u32,
    data: Vec<u64>,
}

impl RollingBloom {
    /// Core's constructor, then `reset()` with `tweak` (Core draws it with `GetRand`).
    // Core: common/bloom.cpp CRollingBloomFilter::CRollingBloomFilter
    pub fn new(elements: u32, fp_rate: f64, tweak: u32) -> Self {
        let log_fp = fp_rate.ln();
        let hash_funcs = ((log_fp / 0.5f64.ln()).round() as i32).clamp(1, 50) as u32;
        let entries_per_generation = elements.div_ceil(2);
        let max_elements = entries_per_generation * 3;
        let filter_bits = (-f64::from(hash_funcs) * f64::from(max_elements)
            / (1.0 - (log_fp / f64::from(hash_funcs)).exp()).ln())
        .ceil() as u32;
        Self {
            entries_per_generation,
            entries_this_generation: 0,
            generation: 1,
            hash_funcs,
            tweak,
            data: vec![0; (filter_bits.div_ceil(64) as usize) << 1],
        }
    }

    fn hash(&self, n: u32, key: &[u8]) -> u32 {
        murmur3(n.wrapping_mul(0xFBA4_C795).wrapping_add(self.tweak), key)
    }

    /// Core's `insert`.
    pub fn insert(&mut self, key: &[u8]) {
        if self.entries_this_generation == self.entries_per_generation {
            self.entries_this_generation = 0;
            self.generation += 1;
            if self.generation == 4 {
                self.generation = 1;
            }
            let mask1 = 0u64.wrapping_sub(u64::from(self.generation & 1));
            let mask2 = 0u64.wrapping_sub(u64::from(self.generation >> 1));
            for pair in self.data.as_chunks_mut::<2>().0 {
                let mask = (pair[0] ^ mask1) | (pair[1] ^ mask2);
                pair[0] &= mask;
                pair[1] &= mask;
            }
        }
        self.entries_this_generation += 1;
        let len = self.data.len() as u32;
        for n in 0..self.hash_funcs {
            let h = self.hash(n, key);
            let bit = h & 0x3F;
            let pos = fast_range32(h, len);
            let (lo, hi) = ((pos & !1) as usize, (pos | 1) as usize);
            self.data[lo] =
                (self.data[lo] & !(1u64 << bit)) | (u64::from(self.generation & 1) << bit);
            self.data[hi] =
                (self.data[hi] & !(1u64 << bit)) | (u64::from(self.generation >> 1) << bit);
        }
    }

    /// Core's `contains`.
    pub fn contains(&self, key: &[u8]) -> bool {
        let len = self.data.len() as u32;
        (0..self.hash_funcs).all(|n| {
            let h = self.hash(n, key);
            let pos = fast_range32(h, len);
            ((self.data[(pos & !1) as usize] | self.data[(pos | 1) as usize]) >> (h & 0x3F)) & 1
                == 1
        })
    }

    /// Core's `reset()` with a new `tweak`.
    pub fn reset(&mut self, tweak: u32) {
        self.tweak = tweak;
        self.entries_this_generation = 0;
        self.generation = 1;
        self.data.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Core's `hash_tests/murmurhash3` vectors (a selection).
    #[test]
    fn murmur3_core_vectors() {
        assert_eq!(murmur3(0, &[]), 0x0000_0000);
        assert_eq!(murmur3(0xFBA4_C795, &[]), 0x6a39_6f08);
        assert_eq!(murmur3(0xffff_ffff, &[]), 0x81f1_6f39);
        assert_eq!(murmur3(0, &[0x00]), 0x514e_28b7);
        assert_eq!(murmur3(0xFBA4_C795, &[0x00]), 0xea3f_0b17);
        assert_eq!(murmur3(0, &[0xff]), 0xfd6c_f10d);
        assert_eq!(murmur3(0, &[0x00, 0x11]), 0x16c6_b7ab);
        assert_eq!(murmur3(0, &[0x00, 0x11, 0x22]), 0x8eb5_1c3d);
        assert_eq!(murmur3(0, &[0x00, 0x11, 0x22, 0x33]), 0xb4471bf8);
        assert_eq!(murmur3(0, &[0x00, 0x11, 0x22, 0x33, 0x44]), 0xe2301fa8);
        assert_eq!(murmur3(0, &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]), 0xfc2e4a15);
        assert_eq!(murmur3(0, &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]), 0xb074502c);
        assert_eq!(murmur3(0, &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77]), 0x8034d2a0);
        assert_eq!(murmur3(0, &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]), 0xb4698def);
    }

    /// Core's `bloom_tests/rolling_bloom` structure: the last `elements` inserts are always
    /// present, and older ones age out after three generations.
    #[test]
    fn rolling_generations() {
        let mut f = RollingBloom::new(100, 0.01, 7);
        for i in 0u32..100 {
            f.insert(&i.to_le_bytes());
        }
        assert!((0u32..100).all(|i| f.contains(&i.to_le_bytes())));
        for i in 100u32..400 {
            f.insert(&i.to_le_bytes());
        }
        assert!((300u32..400).all(|i| f.contains(&i.to_le_bytes())));
        let stale = (0u32..100).filter(|i| f.contains(&i.to_le_bytes())).count();
        assert!(stale < 10, "{stale} of the oldest generation survived");
        f.reset(8);
        assert!(!f.contains(&399u32.to_le_bytes()));
    }
}
