// SPDX-License-Identifier: MIT

//! Core v24.0.0 block filter index values, in Core's uint256 byte order.

use dash_consensus::primitives::{read_varint_u32, read_varint_u64, write_varint};
use dash_filters::{FilterHash, FilterHeader};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use crate::IndexError;

/// A filter-file position with Core's full-width offset.
// Core: flatfile.h:14-22
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterPos {
    pub file: i32,
    pub pos: u64,
}

/// Core's `DBVal`, also used directly as a hash archive row's value.
// Core: index/blockfilterindex.cpp:50-60,326-339
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DbVal {
    pub filter_hash: FilterHash,
    pub header: FilterHeader,
    pub pos: FilterPos,
}

impl DbVal {
    /// `filter_hash ‖ header ‖ VARINT(file) ‖ VARINT(pos)`.
    // Core: index/blockfilterindex.cpp:55-60; flatfile.h:19-22
    pub fn encode(&self) -> Result<Vec<u8>, IndexError> {
        let file = u32::try_from(self.pos.file)
            .map_err(|_| IndexError::Corrupt("negative filter file number".into()))?;
        let mut bytes = Vec::with_capacity(79);
        bytes.extend_from_slice(self.filter_hash.as_bytes());
        bytes.extend_from_slice(self.header.as_bytes());
        write_varint(u64::from(file), &mut bytes);
        write_varint(self.pos.pos, &mut bytes);
        Ok(bytes)
    }

    /// Decodes one complete archive value, rejecting truncation, overflow and trailing bytes.
    // Core: index/blockfilterindex.cpp:55-60; flatfile.h:19-22
    pub fn decode(mut bytes: &[u8]) -> Result<Self, IndexError> {
        let filter_hash = FilterHash::from_lendian(read_hash(&mut bytes)?);
        let header = FilterHeader::from_lendian(read_hash(&mut bytes)?);
        // Core: flatfile.h:19-22; serialize.h:404-424
        let file = read_varint_u32(&mut bytes)
            .map_err(|err| IndexError::Corrupt(format!("filter file number: {err}")))?;
        let file = i32::try_from(file)
            .map_err(|_| IndexError::Corrupt("filter file number exceeds int32".into()))?;
        let pos = read_varint_u64(&mut bytes)
            .map_err(|err| IndexError::Corrupt(format!("filter file offset: {err}")))?;
        if !bytes.is_empty() {
            return Err(IndexError::Corrupt("trailing filter index bytes".into()));
        }
        Ok(Self {
            filter_hash,
            header,
            pos: FilterPos {
                file,
                pos,
            },
        })
    }
}

/// Core's height row: `pair<uint256, DBVal>`.
// Core: index/blockfilterindex.cpp:299-305
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeightValue {
    pub block_hash: BlockHash,
    pub filter: DbVal,
}

impl HeightValue {
    /// `block_hash ‖ filter_hash ‖ header ‖ FlatFilePos`.
    // Core: index/blockfilterindex.cpp:299-305; serialize.h:1157-1169
    pub fn encode(&self) -> Result<Vec<u8>, IndexError> {
        let mut bytes = self.block_hash.as_bytes().to_vec();
        bytes.extend_from_slice(&self.filter.encode()?);
        Ok(bytes)
    }

    /// Decodes a complete height value.
    // Core: index/blockfilterindex.cpp:279-285,299-305
    pub fn decode(mut bytes: &[u8]) -> Result<Self, IndexError> {
        let block_hash = BlockHash::from_lendian(read_hash(&mut bytes)?);
        let filter = DbVal::decode(bytes)?;
        Ok(Self {
            block_hash,
            filter,
        })
    }
}

// Core: uint256.h:78-87
fn read_hash(bytes: &mut &[u8]) -> Result<[u8; 32], IndexError> {
    let (hash, rest) = bytes
        .split_first_chunk::<32>()
        .ok_or_else(|| IndexError::Corrupt("truncated filter index hash".into()))?;
    *bytes = rest;
    Ok(*hash)
}

#[cfg(test)]
mod tests {
    use dash_core_vectors::hex::parse_hex;

    use super::*;

    // Core: index/blockfilterindex.cpp:50-60,299-305; flatfile.h:19-22
    // Serialized with v24.0.0's C++ pair<uint256, DBVal> serializer. Each hash's
    // memory bytes increase, so a reversed hash or a field swap changes the vector.
    const CORE_HASHES: &str = concat!(
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
        "404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f",
    );

    fn value(file: i32, pos: u64) -> HeightValue {
        HeightValue {
            block_hash: BlockHash::from_lendian(std::array::from_fn(|i| i as u8)),
            filter: DbVal {
                filter_hash: FilterHash::from_lendian(std::array::from_fn(|i| i as u8 + 32)),
                header: FilterHeader::from_lendian(std::array::from_fn(|i| i as u8 + 64)),
                pos: FilterPos {
                    file,
                    pos,
                },
            },
        }
    }

    #[test]
    fn codec_round_trips_and_equals_core_serialization() {
        for (file, pos, suffix) in [
            (0, 0, "0000"),
            (127, 128, "7f8000"),
            (128, 16_383, "8000fe7f"),
            (16_384, 16_777_215, "ff0086fefe7f"),
            (i32::MAX, u64::from(u32::MAX), "86fefefe7f8efefefe7f"),
            (0, u64::from(u32::MAX) + 1, "008efefeff00"),
            (i32::MAX, u64::MAX, "86fefefe7f80fefefefefefefefe7f"),
        ] {
            let value = value(file, pos);
            let core = parse_hex(&format!("{CORE_HASHES}{suffix}"));
            assert_eq!(value.encode().unwrap(), core);
            assert_eq!(HeightValue::decode(&core).unwrap(), value);
            assert_eq!(value.filter.encode().unwrap(), core[32..]);
            assert_eq!(DbVal::decode(&core[32..]).unwrap(), value.filter);
        }
    }

    #[test]
    fn codec_rejects_truncation_trailing_bytes_and_overflow() {
        let bytes = value(i32::MAX, u64::MAX).encode().unwrap();
        for end in 0..bytes.len() {
            assert!(HeightValue::decode(&bytes[..end]).is_err(), "height length {end}");
        }
        for end in 32..bytes.len() {
            assert!(DbVal::decode(&bytes[32..end]).is_err(), "archive length {}", end - 32);
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(HeightValue::decode(&trailing).is_err());
        assert!(DbVal::decode(&trailing[32..]).is_err());

        for (file, pos) in
            [(i32::MAX as u64 + 1, 0), (u32::MAX as u64 + 1, 0), (u64::MAX, u64::MAX)]
        {
            let mut bytes = parse_hex(CORE_HASHES);
            write_varint(file, &mut bytes);
            write_varint(pos, &mut bytes);
            assert!(HeightValue::decode(&bytes).is_err());
            assert!(DbVal::decode(&bytes[32..]).is_err());
        }
        for prefix in [&[][..], &[0][..]] {
            let mut bytes = parse_hex(CORE_HASHES);
            bytes.extend_from_slice(prefix);
            bytes.extend([0xff; 20]);
            assert!(HeightValue::decode(&bytes).is_err());
            assert!(DbVal::decode(&bytes[32..]).is_err());
        }
    }

    #[test]
    fn codec_rejects_negative_file_numbers() {
        for file in [-1, -2, i32::MIN] {
            let value = value(file, 0);
            assert!(value.encode().is_err());
            assert!(value.filter.encode().is_err());
        }
    }
}
