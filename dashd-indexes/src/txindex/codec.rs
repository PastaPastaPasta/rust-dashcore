// SPDX-License-Identifier: MIT

//! The transaction index's row key, its sequence value and the epoch row.

use dash_consensus::primitives::{read_varint_u64, write_varint};

use crate::{FlatFilePos, IndexError, Txid};

/// The txid bytes a key keeps.
pub const PREFIX_LEN: usize = 8;

/// `txid[0..8] ‖ BE32 file ‖ BE32 data_pos ‖ BE32 tx_offset`.
pub const KEY_LEN: usize = PREFIX_LEN + 12;

/// Core's `CDiskTxPos`: the block's data position and the transaction's offset after the header.
// Core: index/disktxpos.h:11-25
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxPos {
    pub block: FlatFilePos,
    pub tx_offset: u32,
}

/// One row's key: a txid prefix and the transaction's full position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxKey {
    pub prefix: [u8; PREFIX_LEN],
    pub pos: TxPos,
}

/// The first [`PREFIX_LEN`] bytes of `txid`, in Core's uint256 byte order.
pub fn prefix(txid: &Txid) -> [u8; PREFIX_LEN] {
    let mut out = [0; PREFIX_LEN];
    out.copy_from_slice(&txid.as_bytes()[..PREFIX_LEN]);
    out
}

impl TxKey {
    /// Rejects Core's null file number (negative).
    pub fn encode(&self) -> Result<[u8; KEY_LEN], IndexError> {
        let file = u32::try_from(self.pos.block.file)
            .map_err(|_| IndexError::Corrupt("negative block file number".into()))?;
        let mut key = [0; KEY_LEN];
        key[..PREFIX_LEN].copy_from_slice(&self.prefix);
        key[PREFIX_LEN..PREFIX_LEN + 4].copy_from_slice(&file.to_be_bytes());
        key[PREFIX_LEN + 4..PREFIX_LEN + 8].copy_from_slice(&self.pos.block.pos.to_be_bytes());
        key[PREFIX_LEN + 8..].copy_from_slice(&self.pos.tx_offset.to_be_bytes());
        Ok(key)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, IndexError> {
        let bytes: &[u8; KEY_LEN] = bytes.try_into().map_err(|_| {
            IndexError::Corrupt(format!("txindex key length {}, expected {KEY_LEN}", bytes.len()))
        })?;
        let (prefix, rest) = bytes.split_first_chunk::<PREFIX_LEN>().expect("the key's length");
        let field = |i: usize| u32::from_be_bytes(rest[4 * i..4 * i + 4].try_into().expect("4"));
        let file = i32::try_from(field(0))
            .map_err(|_| IndexError::Corrupt("block file number exceeds int32".into()))?;
        Ok(Self {
            prefix: *prefix,
            pos: TxPos {
                block: FlatFilePos {
                    file,
                    pos: field(1),
                },
                tx_offset: field(2),
            },
        })
    }
}

pub fn encode_seq(seq: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(10);
    write_varint(seq, &mut bytes);
    bytes
}

/// Decodes one complete `VARINT`, rejecting truncation, overflow and trailing bytes.
pub fn decode_seq(mut bytes: &[u8]) -> Result<u64, IndexError> {
    let seq = read_varint_u64(&mut bytes)
        .map_err(|err| IndexError::Corrupt(format!("txindex sequence: {err}")))?;
    if !bytes.is_empty() {
        return Err(IndexError::Corrupt("trailing txindex sequence bytes".into()));
    }
    Ok(seq)
}

pub fn encode_epoch(epoch: u32) -> [u8; 4] {
    epoch.to_be_bytes()
}

pub fn decode_epoch(bytes: &[u8]) -> Result<u32, IndexError> {
    bytes.try_into().map(u32::from_be_bytes).map_err(|_| {
        IndexError::Corrupt(format!("txindex epoch length {}, expected 4", bytes.len()))
    })
}

#[cfg(test)]
mod tests {
    use dash_core_vectors::hex::parse_hex;

    use super::*;

    fn key(file: i32, pos: u32, tx_offset: u32) -> TxKey {
        TxKey {
            prefix: std::array::from_fn(|i| 0xa0 + i as u8),
            pos: TxPos {
                block: FlatFilePos {
                    file,
                    pos,
                },
                tx_offset,
            },
        }
    }

    #[test]
    fn key_layout_is_prefix_then_big_endian_fields() {
        for (k, hex) in [
            (key(0, 0, 0), "a0a1a2a3a4a5a6a7000000000000000000000000"),
            (key(1, 0x0203_0405, 0x0607_0809), "a0a1a2a3a4a5a6a7000000010203040506070809"),
            (key(i32::MAX, u32::MAX, u32::MAX), "a0a1a2a3a4a5a6a77fffffffffffffffffffffff"),
        ] {
            let bytes = k.encode().unwrap();
            assert_eq!(bytes.to_vec(), parse_hex(hex));
            assert_eq!(TxKey::decode(&bytes).unwrap(), k);
        }
    }

    #[test]
    fn keys_sort_by_prefix_then_position() {
        let mut keys = [key(1, 0, 0), key(0, 1, 0), key(0, 0, 1), key(0, 0x100, 0)];
        keys.sort_by_key(|k| k.encode().unwrap());
        assert_eq!(keys, [key(0, 0, 1), key(0, 1, 0), key(0, 0x100, 0), key(1, 0, 0)]);
    }

    #[test]
    fn prefix_is_the_first_txid_bytes_in_core_order() {
        let txid: Txid =
            "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".parse().unwrap();
        assert_eq!(prefix(&txid), [0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88]);
    }

    #[test]
    fn key_rejects_bad_lengths_and_file_numbers() {
        let bytes = key(3, 4, 5).encode().unwrap();
        for end in 0..KEY_LEN {
            assert!(TxKey::decode(&bytes[..end]).is_err(), "length {end}");
        }
        let mut long = bytes.to_vec();
        long.push(0);
        assert!(TxKey::decode(&long).is_err());
        for file in [-1, i32::MIN] {
            assert!(key(file, 0, 0).encode().is_err());
        }
        let mut high = bytes;
        high[PREFIX_LEN] = 0x80;
        assert!(TxKey::decode(&high).is_err());
    }

    #[test]
    fn seq_is_core_varint_and_round_trips() {
        for (seq, hex) in [
            (0u64, "00"),
            (127, "7f"),
            (128, "8000"),
            (1 << 40, "9efefefeff00"),
            ((2 << 40) + 5, "befefefeff05"),
            (u64::MAX, "80fefefefefefefefe7f"),
        ] {
            let bytes = encode_seq(seq);
            assert_eq!(bytes, parse_hex(hex), "{seq}");
            assert_eq!(decode_seq(&bytes).unwrap(), seq);
        }
    }

    #[test]
    fn seq_rejects_truncation_trailing_bytes_and_overflow() {
        let bytes = encode_seq(1 << 40);
        for end in 0..bytes.len() {
            assert!(decode_seq(&bytes[..end]).is_err(), "length {end}");
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode_seq(&trailing).is_err());
        assert!(decode_seq(&[0xff; 11]).is_err());
    }

    #[test]
    fn epoch_is_big_endian_u32() {
        for epoch in [0, 1, 0x0102_0304, u32::MAX] {
            let bytes = encode_epoch(epoch);
            assert_eq!(bytes, epoch.to_be_bytes());
            assert_eq!(decode_epoch(&bytes).unwrap(), epoch);
        }
        assert_eq!(encode_epoch(0x0102_0304), [1, 2, 3, 4]);
        for bad in [&[][..], &[0; 3], &[0; 5]] {
            assert!(decode_epoch(bad).is_err());
        }
    }
}
