//! Core's generic deserializers that base-sdk does not provide: `VARINT` and `ReadCompactSize`
//! over a byte cursor, with Core's checks and exception messages.
//!
//! Each reader advances the cursor past what it read, also when it fails part-way; the
//! object-level decoders that use them work on a copy of the caller's cursor and write it back
//! only on success.

use std::fmt;

use super::decode::MAX_SIZE;

/// Why Core's deserializer throws, with the text of Core's `std::ios_base::failure`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadErrorKind {
    /// The input ended inside a value (`CDataStream::read(): end of data`, and the same for
    /// `ignore`).
    EndOfData,
    /// A compact size in a longer form than its value needs.
    NonCanonicalCompactSize,
    /// A compact size above `MAX_SIZE` where Core range-checks it.
    CompactSizeTooLarge,
    /// A `VARINT` that does not fit its integer type.
    VarIntTooLarge,
}

impl fmt::Display for ReadErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ReadErrorKind::EndOfData => "end of data",
            ReadErrorKind::NonCanonicalCompactSize => "non-canonical ReadCompactSize()",
            ReadErrorKind::CompactSizeTooLarge => "ReadCompactSize(): size too large",
            ReadErrorKind::VarIntTooLarge => "ReadVarInt(): size too large",
        })
    }
}

/// A failed read: what went wrong, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadError {
    /// The position in the input passed to the decoder at which reading stopped.
    pub offset: usize,
    pub kind: ReadErrorKind,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at offset {}", self.kind, self.offset)
    }
}

impl std::error::Error for ReadError {}

/// Runs `read` on a copy of `input` and advances `input` only on success. A failure reports the
/// position, relative to `input`, where reading stopped.
pub(crate) fn read_whole<'a, T>(
    input: &mut &'a [u8],
    read: impl FnOnce(&mut &'a [u8]) -> Result<T, ReadErrorKind>,
) -> Result<T, ReadError> {
    let mut cursor = *input;
    match read(&mut cursor) {
        Ok(value) => {
            *input = cursor;
            Ok(value)
        }
        Err(kind) => Err(ReadError {
            offset: input.len() - cursor.len(),
            kind,
        }),
    }
}

/// The next `n` bytes.
pub(crate) fn read_bytes<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8], ReadErrorKind> {
    if input.len() < n {
        *input = &input[input.len()..];
        return Err(ReadErrorKind::EndOfData);
    }
    let (bytes, rest) = input.split_at(n);
    *input = rest;
    Ok(bytes)
}

fn read_array<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], ReadErrorKind> {
    let mut out = [0u8; N];
    out.copy_from_slice(read_bytes(input, N)?);
    Ok(out)
}

pub(crate) fn read_u8(input: &mut &[u8]) -> Result<u8, ReadErrorKind> {
    Ok(read_bytes(input, 1)?[0])
}

// Core: serialize.h:292 ReadCompactSize (v23.1.8 :288)
/// Core's `ReadCompactSize`: rejects a non-minimal form and, with `range_check`, a value above
/// `MAX_SIZE`.
pub fn read_compact_size(input: &mut &[u8], range_check: bool) -> Result<u64, ReadErrorKind> {
    let size = match read_u8(input)? {
        prefix @ 0..253 => u64::from(prefix),
        253 => {
            let n = u64::from(u16::from_le_bytes(read_array(input)?));
            if n < 253 {
                return Err(ReadErrorKind::NonCanonicalCompactSize);
            }
            n
        }
        254 => {
            let n = u64::from(u32::from_le_bytes(read_array(input)?));
            if n < 0x1_0000 {
                return Err(ReadErrorKind::NonCanonicalCompactSize);
            }
            n
        }
        255 => {
            let n = u64::from_le_bytes(read_array(input)?);
            if n < 0x1_0000_0000 {
                return Err(ReadErrorKind::NonCanonicalCompactSize);
            }
            n
        }
    };
    if range_check && size > MAX_SIZE as u64 {
        return Err(ReadErrorKind::CompactSizeTooLarge);
    }
    Ok(size)
}

// Core: serialize.h:405 ReadVarInt (v23.1.8 :401)
/// Core's `ReadVarInt<I>` for an unsigned integer whose largest value is `max`: rejects a digit
/// that would shift a set bit out, and an increment past `max`.
fn read_varint(input: &mut &[u8], max: u64) -> Result<u64, ReadErrorKind> {
    let mut n: u64 = 0;
    loop {
        let byte = read_u8(input)?;
        if n > (max >> 7) {
            return Err(ReadErrorKind::VarIntTooLarge);
        }
        n = (n << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok(n);
        }
        if n == max {
            return Err(ReadErrorKind::VarIntTooLarge);
        }
        n += 1;
    }
}

/// `VARINT` into a `uint32_t` (or `unsigned int`) field.
pub fn read_varint_u32(input: &mut &[u8]) -> Result<u32, ReadErrorKind> {
    read_varint(input, u64::from(u32::MAX)).map(|n| n as u32)
}

/// `VARINT` into a `uint64_t` field.
pub fn read_varint_u64(input: &mut &[u8]) -> Result<u64, ReadErrorKind> {
    read_varint(input, u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{varint_len, write_compact_size, write_varint};

    fn varint(n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        write_varint(n, &mut out);
        out
    }

    /// The examples in Core's comment above `VarIntMode` (serialize.h).
    #[test]
    fn varint_matches_cores_examples() {
        for (n, expected) in [
            (0u64, &[0x00][..]),
            (1, &[0x01]),
            (127, &[0x7f]),
            (128, &[0x80, 0x00]),
            (255, &[0x80, 0x7f]),
            (256, &[0x81, 0x00]),
            (16383, &[0xfe, 0x7f]),
            (16384, &[0xff, 0x00]),
            (16511, &[0xff, 0x7f]),
            (65535, &[0x82, 0xfe, 0x7f]),
            (1 << 32, &[0x8e, 0xfe, 0xfe, 0xff, 0x00]),
        ] {
            assert_eq!(varint(n), expected, "{n}");
            assert_eq!(varint_len(n), expected.len(), "{n}");
            let mut cursor = expected;
            assert_eq!(read_varint_u64(&mut cursor), Ok(n));
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn varint_round_trips_at_the_type_limits() {
        for n in [u64::from(u32::MAX), u64::MAX, u64::MAX - 1, 1 << 63] {
            let bytes = varint(n);
            assert_eq!(varint_len(n), bytes.len());
            assert_eq!(read_varint_u64(&mut &bytes[..]), Ok(n));
        }
        let max32 = varint(u64::from(u32::MAX));
        assert_eq!(read_varint_u32(&mut &max32[..]), Ok(u32::MAX));
        let over32 = varint(u64::from(u32::MAX) + 1);
        assert_eq!(read_varint_u32(&mut &over32[..]), Err(ReadErrorKind::VarIntTooLarge));
    }

    #[test]
    fn compact_size_rejects_non_canonical_and_oversized_forms() {
        for bytes in [
            &[253u8, 252, 0][..],
            &[254, 0xff, 0xff, 0, 0],
            &[255, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0],
        ] {
            assert_eq!(
                read_compact_size(&mut &bytes[..], false),
                Err(ReadErrorKind::NonCanonicalCompactSize)
            );
        }
        let mut big = Vec::new();
        write_compact_size(MAX_SIZE as u64 + 1, &mut big);
        assert_eq!(read_compact_size(&mut &big[..], true), Err(ReadErrorKind::CompactSizeTooLarge));
        assert_eq!(read_compact_size(&mut &big[..], false), Ok(MAX_SIZE as u64 + 1));
        assert_eq!(read_compact_size(&mut &[253u8, 0][..], true), Err(ReadErrorKind::EndOfData));
    }

    #[test]
    fn read_whole_restores_the_cursor_and_reports_where_it_stopped() {
        let bytes = [0x80u8, 0x80];
        let mut cursor = &bytes[..];
        let error = read_whole(&mut cursor, read_varint_u64);
        assert_eq!(
            error,
            Err(ReadError {
                offset: 2,
                kind: ReadErrorKind::EndOfData
            })
        );
        assert_eq!(cursor.len(), 2);
    }
}
