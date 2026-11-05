//! Explicit byte codecs for stored keys and values (FR-STO-05).
//!
//! Every stored layout is written by hand and covered by a layout test. The helpers here are the
//! building blocks: Core's `VARINT` and fixed-width big-endian integers (big-endian so that key
//! order is numeric order).

/// A key or value that failed to decode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("unexpected end of input")]
    UnexpectedEnd,
    #[error("{0} trailing bytes")]
    TrailingBytes(usize),
    #[error("VARINT does not fit the target integer")]
    VarIntOverflow,
    #[error("expected {expected} bytes, got {got}")]
    Length {
        expected: usize,
        got: usize,
    },
    #[error("invalid encoding: {0}")]
    Invalid(&'static str),
}

/// A type usable as a key of a [`Table`](crate::Table).
///
/// The encoding defines the iteration order, so it must be order-preserving where callers rely on
/// ordered or prefix scans.
pub trait KeyCodec: Sized {
    fn encode_key(&self, out: &mut Vec<u8>);
    fn decode_key(bytes: &[u8]) -> Result<Self, CodecError>;

    fn to_key_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_key(&mut out);
        out
    }
}

/// A type usable as a value of a [`Table`](crate::Table).
pub trait ValueCodec: Sized {
    fn encode_value(&self, out: &mut Vec<u8>);
    fn decode_value(bytes: &[u8]) -> Result<Self, CodecError>;

    fn to_value_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_value(&mut out);
        out
    }
}

/// Appends `n` as Core's `VARINT` (`serialize.h` `WriteVarInt`, `VarIntMode::DEFAULT`).
///
/// MSB-first base-128 where every continuation group is stored minus one, so each value has
/// exactly one encoding.
// Core: serialize.h WriteVarInt
pub fn write_varint(out: &mut Vec<u8>, mut n: u64) {
    let mut tmp = [0u8; 10];
    let mut len = 0;
    loop {
        tmp[len] = (n & 0x7f) as u8
            | if len > 0 {
                0x80
            } else {
                0
            };
        if n <= 0x7f {
            break;
        }
        n = (n >> 7) - 1;
        len += 1;
    }
    out.extend(tmp[..=len].iter().rev());
}

/// Reads Core's `VARINT` into an integer of at most `bits` bits, with Core's overflow checks
/// (`serialize.h` `ReadVarInt<I>`).
// Core: serialize.h ReadVarInt
fn read_varint_bits(input: &mut &[u8], bits: u32) -> Result<u64, CodecError> {
    let max = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let mut n: u64 = 0;
    loop {
        let (&byte, rest) = input.split_first().ok_or(CodecError::UnexpectedEnd)?;
        *input = rest;
        if n > (max >> 7) {
            return Err(CodecError::VarIntOverflow);
        }
        n = (n << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 != 0 {
            if n == max {
                return Err(CodecError::VarIntOverflow);
            }
            n += 1;
        } else {
            return Ok(n);
        }
    }
}

/// Reads a Core `VARINT` as `u64`.
pub fn read_varint_u64(input: &mut &[u8]) -> Result<u64, CodecError> {
    read_varint_bits(input, 64)
}

/// Reads a Core `VARINT` as `u32` (Core's `VARINT(uint32_t)` fields: coin codes, vout).
pub fn read_varint_u32(input: &mut &[u8]) -> Result<u32, CodecError> {
    read_varint_bits(input, 32).map(|n| n as u32)
}

fn fixed<const N: usize>(bytes: &[u8]) -> Result<[u8; N], CodecError> {
    bytes.try_into().map_err(|_| CodecError::Length {
        expected: N,
        got: bytes.len(),
    })
}

/// Raw bytes, stored as is.
impl KeyCodec for Vec<u8> {
    fn encode_key(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self)
    }
    fn decode_key(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(bytes.to_vec())
    }
}

impl ValueCodec for Vec<u8> {
    fn encode_value(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(bytes.to_vec())
    }
}

/// Fixed-size byte strings (hashes), stored as is.
impl<const N: usize> KeyCodec for [u8; N] {
    fn encode_key(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self)
    }
    fn decode_key(bytes: &[u8]) -> Result<Self, CodecError> {
        fixed(bytes)
    }
}

impl<const N: usize> ValueCodec for [u8; N] {
    fn encode_value(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self, CodecError> {
        fixed(bytes)
    }
}

/// A `u32` stored as 4 big-endian bytes, so byte order is numeric order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BeU32(pub u32);

impl KeyCodec for BeU32 {
    fn encode_key(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.to_be_bytes())
    }
    fn decode_key(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(Self(u32::from_be_bytes(fixed(bytes)?)))
    }
}

impl ValueCodec for BeU32 {
    fn encode_value(&self, out: &mut Vec<u8>) {
        self.encode_key(out)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self, CodecError> {
        Self::decode_key(bytes)
    }
}

/// A `u64` stored as 8 big-endian bytes, so byte order is numeric order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BeU64(pub u64);

impl KeyCodec for BeU64 {
    fn encode_key(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.to_be_bytes())
    }
    fn decode_key(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(Self(u64::from_be_bytes(fixed(bytes)?)))
    }
}

impl ValueCodec for BeU64 {
    fn encode_value(&self, out: &mut Vec<u8>) {
        self.encode_key(out)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self, CodecError> {
        Self::decode_key(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        write_varint(&mut out, n);
        out
    }

    /// Core's own VARINT vectors: Dash Core `src/test/serialize_tests.cpp`, test case
    /// `varints_bitpatterns` (line 126 at v23.1.8, line 128 at v24.0.0-rc.3; the vectors are
    /// identical at both tags).
    #[test]
    fn varint_matches_core_bit_patterns() {
        let cases: &[(u64, &[u8])] = &[
            (0, &[0x00]),
            (0x7f, &[0x7f]),
            (0x80, &[0x80, 0x00]),
            (0x1234, &[0xa3, 0x34]),
            (0xffff, &[0x82, 0xfe, 0x7f]),
            (0x123456, &[0xc7, 0xe7, 0x56]),
            (0x80123456, &[0x86, 0xff, 0xc7, 0xe7, 0x56]),
            (0xffffffff, &[0x8e, 0xfe, 0xfe, 0xfe, 0x7f]),
            (0x7fffffffffffffff, &[0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0x7f]),
            (0xffffffffffffffff, &[0x80, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0xfe, 0x7f]),
        ];
        for &(n, bytes) in cases {
            assert_eq!(varint(n), bytes, "encode {n:#x}");
            let mut input = bytes;
            assert_eq!(read_varint_u64(&mut input), Ok(n), "decode {n:#x}");
            assert!(input.is_empty());
        }
    }

    #[test]
    fn varint_round_trips_and_rejects_overflow() {
        for n in (0..64).flat_map(|s| [1u64 << s, (1u64 << s) - 1, (1u64 << s) + 1]) {
            let bytes = varint(n);
            assert_eq!(read_varint_u64(&mut bytes.as_slice()), Ok(n));
            let as_u32 = read_varint_u32(&mut bytes.as_slice());
            if n <= u64::from(u32::MAX) {
                assert_eq!(as_u32, Ok(n as u32));
            } else {
                assert_eq!(as_u32, Err(CodecError::VarIntOverflow));
            }
        }
        // Past u64::MAX: Core's "size too large".
        let too_big = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
        assert_eq!(read_varint_u64(&mut &too_big[..]), Err(CodecError::VarIntOverflow));
        let five = [0x8f, 0xfe, 0xfe, 0xfe, 0x7f]; // encodes a value above u32::MAX
        assert_eq!(read_varint_u32(&mut &five[..]), Err(CodecError::VarIntOverflow));
        assert_eq!(read_varint_u64(&mut &[0x80][..]), Err(CodecError::UnexpectedEnd));
    }

    #[test]
    fn fixed_width_layouts() {
        assert_eq!(BeU32(0x01020304).to_key_bytes(), [1, 2, 3, 4]);
        assert_eq!(BeU64(0x0102030405060708).to_value_bytes(), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(BeU32::decode_key(&[1, 2, 3, 4]), Ok(BeU32(0x01020304)));
        assert_eq!(
            BeU32::decode_key(&[1, 2, 3]),
            Err(CodecError::Length {
                expected: 4,
                got: 3
            })
        );
        assert_eq!(<[u8; 2]>::decode_value(&[9, 8]), Ok([9, 8]));
        assert!(BeU32(1).to_key_bytes() < BeU32(256).to_key_bytes());
    }
}
