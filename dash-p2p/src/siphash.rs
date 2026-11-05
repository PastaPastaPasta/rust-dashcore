//! Core's `CSipHasher` (`crypto/siphash.cpp`): SipHash-2-4 over a byte stream, for
//! `GetDeterministicRandomizer` (keyed net groups, local nonces, the address cache).

/// Core's `CSipHasher`.
#[derive(Clone, Debug)]
pub struct SipHasher {
    v: [u64; 4],
    tmp: u64,
    count: u8,
}

#[inline]
fn round(v: &mut [u64; 4]) {
    v[0] = v[0].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(13) ^ v[0];
    v[0] = v[0].rotate_left(32);
    v[2] = v[2].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(16) ^ v[2];
    v[0] = v[0].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(21) ^ v[0];
    v[2] = v[2].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(17) ^ v[2];
    v[2] = v[2].rotate_left(32);
}

impl SipHasher {
    /// A hasher keyed with `(k0, k1)`.
    pub fn new(k0: u64, k1: u64) -> Self {
        Self {
            v: [
                0x736f_6d65_7073_6575 ^ k0,
                0x646f_7261_6e64_6f6d ^ k1,
                0x6c79_6765_6e65_7261 ^ k0,
                0x7465_6462_7974_6573 ^ k1,
            ],
            tmp: 0,
            count: 0,
        }
    }

    fn compress(&mut self, m: u64) {
        self.v[3] ^= m;
        round(&mut self.v);
        round(&mut self.v);
        self.v[0] ^= m;
    }

    /// Core's `Write(uint64_t)`: eight little-endian bytes.
    pub fn write_u64(mut self, data: u64) -> Self {
        self = self.write(&data.to_le_bytes());
        self
    }

    /// Core's `Write(Span<const unsigned char>)`.
    pub fn write(mut self, data: &[u8]) -> Self {
        for &b in data {
            self.tmp |= u64::from(b) << (8 * (self.count & 7));
            self.count = self.count.wrapping_add(1);
            if self.count & 7 == 0 {
                let m = self.tmp;
                self.compress(m);
                self.tmp = 0;
            }
        }
        self
    }

    /// Core's `Finalize()`.
    pub fn finish(&self) -> u64 {
        let mut v = self.v;
        let t = self.tmp | (u64::from(self.count) << 56);
        v[3] ^= t;
        round(&mut v);
        round(&mut v);
        v[0] ^= t;
        v[2] ^= 0xFF;
        for _ in 0..4 {
            round(&mut v);
        }
        v[0] ^ v[1] ^ v[2] ^ v[3]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Core's `hash_tests/siphash`.
    #[test]
    fn core_vectors() {
        let k0 = 0x0706_0504_0302_0100;
        let k1 = 0x0F0E_0D0C_0B0A_0908;
        let h = SipHasher::new(k0, k1);
        assert_eq!(h.finish(), 0x726f_db47_dd0e_0e31);
        let h = h.write(&[0]);
        assert_eq!(h.finish(), 0x74f8_39c5_93dc_67fd);
        let h = h.write(&[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(h.finish(), 0x93f5_f579_9a93_2462);
        let h = h.write_u64(0x0F0E_0D0C_0B0A_0908);
        assert_eq!(h.finish(), 0x3f2a_cc7f_57c2_9bdb);
        let h = h.write(&[16, 17]);
        assert_eq!(h.finish(), 0x4bc1_b3f0_968d_d39c);
        let h = h.write(&[18, 19, 20, 21, 22, 23, 24, 25, 26]);
        assert_eq!(h.finish(), 0x2f2e_6163_076b_cfad);
        let h = h.write(&[27, 28, 29, 30, 31]);
        assert_eq!(h.finish(), 0x7127_512f_72f2_7cce);
        let h = h.write_u64(0x2726_2524_2322_2120);
        assert_eq!(h.finish(), 0x0e3e_a96b_5304_a7d0);
        let h = h.write_u64(0x2F2E_2D2C_2B2A_2928);
        assert_eq!(h.finish(), 0xe612_a3cb_9ecb_a951);
    }
}
