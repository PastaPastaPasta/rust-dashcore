//! [`OutPointKey`]: an outpoint as the coins cache holds it, and its key in the `coins` column
//! family.

use dash_consensus::primitives::{OutPoint, TxHash, read_varint_u32};
use dash_types::Numeric as _;

/// The longest [`OutPointKey::db_key`]: a txid and a 5-byte `VARINT` of a `u32`.
pub const DB_KEY_MAX: usize = 32 + 5;

/// `txid ‖ vout` (little-endian): Core's `COutPoint` as it lies in memory, the cache's key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct OutPointKey(pub [u8; 36]);

impl OutPointKey {
    pub fn new(op: &OutPoint) -> Self {
        let mut k = [0; 36];
        k[..32].copy_from_slice(op.hash.as_bytes());
        k[32..].copy_from_slice(&op.index.to_le_bytes());
        Self(k)
    }

    pub fn txid(&self) -> TxHash {
        let mut h = [0; 32];
        h.copy_from_slice(&self.0[..32]);
        TxHash::from_lendian(h)
    }

    pub fn vout(&self) -> u32 {
        u32::from_le_bytes([self.0[32], self.0[33], self.0[34], self.0[35]])
    }

    pub fn outpoint(&self) -> OutPoint {
        OutPoint {
            hash: self.txid(),
            index: self.vout(),
        }
    }

    // Core: txdb.cpp:62-68 CoinEntry (v23.1.8 :53-59)
    /// The `coins` column family's key: `txid ‖ VARINT(vout)`, Core's `CoinEntry` without its
    /// `'C'` prefix (A1 §2.9). The column family's order is the order of these bytes, Core's
    /// serialized-key order, not numeric vout order: `VARINT(16383)` = `fe7f` sorts after
    /// `VARINT(16512)` = `808000`. Returns the key's length.
    pub fn db_key(&self, buf: &mut [u8; DB_KEY_MAX]) -> usize {
        buf[..32].copy_from_slice(&self.0[..32]);
        32 + write_vout(self.vout(), &mut buf[32..])
    }

    /// The key [`OutPointKey::db_key`] wrote, or `None` for bytes that are not one: a key of
    /// another length, or a `VARINT` that is not canonical, overflows a `u32` or leaves bytes
    /// behind.
    pub fn from_db_key(key: &[u8]) -> Option<Self> {
        let (txid, mut rest) = key.split_at_checked(32)?;
        let vout = read_varint_u32(&mut rest).ok()?;
        if !rest.is_empty() {
            return None;
        }
        let mut k = [0; 36];
        k[..32].copy_from_slice(txid);
        k[32..].copy_from_slice(&vout.to_le_bytes());
        Some(Self(k))
    }
}

// Core: serialize.h:387 WriteVarInt (v23.1.8 :383)
/// `write_varint` of a `u32` into `out`, returning its length (5 at most). The key's own copy:
/// `write_varint` is an upstream generic, compiled into a codegen unit of its own, so whether the
/// key encoder inlines it depends on how this crate's code is partitioned (CH2b b1's PF0 lost it,
/// 12 more instructions per database read). A test pins the two encodings together.
#[inline]
fn write_vout(mut n: u32, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; DB_KEY_MAX - 32];
    let mut len = 0;
    loop {
        tmp[len] = (n & 0x7f) as u8
            | if len > 0 {
                0x80
            } else {
                0x00
            };
        if n <= 0x7f {
            break;
        }
        n = (n >> 7) - 1;
        len += 1;
    }
    for (o, t) in out.iter_mut().zip(tmp[..=len].iter().rev()) {
        *o = *t;
    }
    len + 1
}

impl std::fmt::Debug for OutPointKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.outpoint())
    }
}

#[cfg(test)]
mod tests {
    use dash_consensus::primitives::write_varint;

    use super::*;

    fn key(byte: u8, vout: u32) -> OutPointKey {
        OutPointKey::new(&OutPoint {
            hash: TxHash::from_lendian([byte; 32]),
            index: vout,
        })
    }

    fn db_key(k: &OutPointKey) -> Vec<u8> {
        let mut buf = [0; DB_KEY_MAX];
        let n = k.db_key(&mut buf);
        buf[..n].to_vec()
    }

    #[test]
    fn the_key_is_cores_outpoint_bytes() {
        let k = key(7, 0x0102_0304);
        assert_eq!(&k.0[..32], &[7; 32]);
        assert_eq!(&k.0[32..], &[4, 3, 2, 1]);
        assert_eq!(k.outpoint().index, 0x0102_0304);
        assert_eq!(OutPointKey::new(&k.outpoint()), k);
    }

    /// Core's `VARINT` of the vout, and the serialized-key order it gives (GPT r1 m3).
    #[test]
    fn db_keys_are_txid_then_varint_in_cores_serialized_order() {
        assert_eq!(&db_key(&key(1, 0))[32..], &[0x00]);
        assert_eq!(&db_key(&key(1, 127))[32..], &[0x7f]);
        assert_eq!(&db_key(&key(1, 128))[32..], &[0x80, 0x00]);
        assert_eq!(&db_key(&key(1, 16383))[32..], &[0xfe, 0x7f]);
        assert_eq!(&db_key(&key(1, 16512))[32..], &[0x80, 0x80, 0x00]);
        assert_eq!(&db_key(&key(1, u32::MAX))[32..], &[0x8e, 0xfe, 0xfe, 0xfe, 0x7f]);
        // 16512 sorts before 16383 in the column family.
        assert!(db_key(&key(1, 16512)) < db_key(&key(1, 16383)));
        for vout in [0, 1, 127, 128, 16383, 16384, 16511, 16512, 2_113_663, u32::MAX] {
            let k = key(9, vout);
            assert_eq!(OutPointKey::from_db_key(&db_key(&k)), Some(k));
        }
    }

    /// The key's `VARINT` is `write_varint`'s: at each length's first and last value, and on a
    /// stride through the whole range.
    #[test]
    fn the_keys_varint_is_write_varints() {
        let firsts = [0u32, 128, 16_512, 2_113_664, 270_549_120];
        let edges = firsts.iter().flat_map(|&f| [f.saturating_sub(1), f, f + 1]);
        let stride = (0..=u32::MAX).step_by(9_973);
        for vout in edges.chain(stride).chain([u32::MAX - 1, u32::MAX]) {
            let mut want = [1u8; 32].to_vec();
            write_varint(u64::from(vout), &mut want);
            assert_eq!(db_key(&key(1, vout)), want, "vout {vout}");
        }
    }

    #[test]
    fn malformed_db_keys_are_rejected() {
        let good = db_key(&key(3, 300));
        assert_eq!(OutPointKey::from_db_key(&good[..31]), None);
        assert_eq!(OutPointKey::from_db_key(&good[..32]), None);
        assert_eq!(OutPointKey::from_db_key(&[good.clone(), vec![0]].concat()), None);
        // A VARINT past u32::MAX.
        let mut big = good[..32].to_vec();
        big.extend([0x8e, 0xfe, 0xfe, 0xff, 0x00]);
        assert_eq!(OutPointKey::from_db_key(&big), None);
    }
}
