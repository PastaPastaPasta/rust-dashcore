//! The decoders: one pass over a byte slice gives the objects and every hash (CS0 §6).
//!
//! Each decoder works on a copy of the input cursor and advances the caller's cursor only on
//! success. The trailing-byte policy belongs to the caller, because Core's call sites differ:
//! `DecodeTx` rejects leftovers ([`decode_tx_exact`]), while `DecodeHexBlk`, P2P messages and
//! block-file reads accept them.

use std::fmt;
use std::sync::Arc;

use dash_primitives::{BlockHeader, Transaction};
use dash_types::CompactSize;
use dash_types::codec::{self, BaseCodec};

use super::block::{Block, Header};
use super::canonical;
use super::serialize::{
    CompareWriter, CoreBytes, CoreSerialize, core_bytes, header_bytes, write_compact_size,
};
use super::tx::Tx;

/// Core's `MAX_SIZE`: the largest count `ReadCompactSize` accepts.
// Core: serialize.h MAX_SIZE
pub const MAX_SIZE: usize = 0x0200_0000;

/// The smallest serialized transaction: version, two empty counts and the lock time.
const MIN_TX_SIZE: usize = 4 + 1 + 1 + 4;

/// A decoded object bound to the bytes it was decoded from.
///
/// Made only by the decoders: the fields are private and there are no setters, so the binding
/// between the value, the bytes and the canonicality flag cannot be changed.
///
/// ```compile_fail,E0616
/// let mut bytes: &[u8] = &[];
/// let decoded = dash_consensus::decode_header(&mut bytes).unwrap();
/// let _ = decoded.wire_is_canonical;
/// ```
pub struct Decoded<'a, T> {
    value: T,
    wire: &'a [u8],
    wire_is_canonical: bool,
}

impl<'a, T> Decoded<'a, T> {
    /// Crate-private: `wire_is_canonical` must be decided exactly, by comparing `wire` with Core's
    /// serialization of `value` (the coins module's undo decoder).
    pub(crate) fn new(value: T, wire: &'a [u8], wire_is_canonical: bool) -> Self {
        Self {
            value,
            wire,
            wire_is_canonical,
        }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    /// The bytes read from the input; `wire().len()` is the consumed length.
    pub fn wire(&self) -> &'a [u8] {
        self.wire
    }

    /// Whether the received bytes equal Core's serialization of the value, decided exactly.
    pub fn wire_is_canonical(&self) -> bool {
        self.wire_is_canonical
    }

    pub fn into_value(self) -> T {
        self.value
    }
}

/// The length and the flag only, never the bytes (CS0 §9.5).
impl<T> fmt::Debug for Decoded<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decoded")
            .field("wire_len", &self.wire.len())
            .field("wire_is_canonical", &self.wire_is_canonical)
            .finish_non_exhaustive()
    }
}

impl<T: CoreSerialize> Decoded<'_, T> {
    /// The received bytes when they equal Core's serialization, and Core's serialization of the
    /// value otherwise.
    pub fn core_bytes(&self) -> CoreBytes {
        if self.wire_is_canonical {
            CoreBytes::from_canonical(self.wire)
        } else {
            core_bytes(&self.value)
        }
    }
}

/// Why bytes did not decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// Where the failing item starts, relative to the slice passed to the call: the transaction,
    /// or for a block the header, the count or the failing transaction.
    pub offset: usize,
    /// The failing transaction's index in a block.
    pub tx_index: Option<usize>,
    pub kind: DecodeErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeErrorKind {
    /// base-sdk's decoder rejected the bytes.
    Codec(codec::DecodeError),
    /// [`decode_tx_exact`]: a transaction decoded but bytes were left.
    TrailingBytes {
        remaining: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            DecodeErrorKind::Codec(e) => write!(f, "decode error at offset {}: {e}", self.offset)?,
            DecodeErrorKind::TrailingBytes {
                remaining,
            } => write!(f, "{remaining} bytes left after the transaction")?,
        }
        if let Some(index) = self.tx_index {
            write!(f, " (transaction {index})")?;
        }
        Ok(())
    }
}

impl std::error::Error for DecodeError {}

/// The part of `start` a decoder has read once `rest` is left.
pub(crate) fn consumed<'a>(start: &'a [u8], rest: &[u8]) -> &'a [u8] {
    &start[..start.len() - rest.len()]
}

fn codec_error(offset: usize, error: codec::DecodeError) -> DecodeError {
    DecodeError {
        offset,
        tx_index: None,
        kind: DecodeErrorKind::Codec(error),
    }
}

/// Reads one transaction and leaves the rest of the input (P2P `tx`, block contents).
pub fn decode_tx<'a>(input: &mut &'a [u8]) -> Result<Decoded<'a, Tx>, DecodeError> {
    let start: &'a [u8] = input;
    let mut cursor = start;
    let raw = Transaction::decode(&mut cursor).map_err(|e| codec_error(0, e))?;
    let wire = consumed(start, cursor);
    let (tx, wire_is_canonical) = tx_from_wire(raw, wire);
    *input = cursor;
    Ok(Decoded {
        value: tx,
        wire,
        wire_is_canonical,
    })
}

/// Hashes the received range when it equals Core's serialization, and Core's serialization
/// otherwise, so the hashes are always over Core's bytes (A1 §1.3).
fn tx_from_wire(raw: Transaction, wire: &[u8]) -> (Tx, bool) {
    // EMBARGO-1
    let mut comparison = CompareWriter::new(wire);
    canonical::write_tx(&raw, &mut comparison);
    if comparison.matches() {
        (Tx::from_serialized(raw, wire), true)
    } else {
        let serialized = raw.core_serialize();
        (Tx::from_serialized(raw, &serialized), false)
    }
}

/// Core's `DecodeTx`: one transaction, and trailing bytes are an error (`sendrawtransaction`,
/// `decoderawtransaction`).
// Core: core_read.cpp DecodeTx
pub fn decode_tx_exact(bytes: &[u8]) -> Result<Decoded<'_, Tx>, DecodeError> {
    let mut cursor = bytes;
    let decoded = decode_tx(&mut cursor)?;
    if !cursor.is_empty() {
        return Err(DecodeError {
            offset: decoded.wire.len(),
            tx_index: None,
            kind: DecodeErrorKind::TrailingBytes {
                remaining: cursor.len(),
            },
        });
    }
    Ok(decoded)
}

/// Reads one 80-byte header and computes its hash (CH1, `headers`).
pub fn decode_header<'a>(input: &mut &'a [u8]) -> Result<Decoded<'a, Header>, DecodeError> {
    let start: &'a [u8] = input;
    let mut cursor = start;
    let raw = BlockHeader::decode(&mut cursor).map_err(|e| codec_error(0, e))?;
    let wire = consumed(start, cursor);
    let wire_is_canonical = header_bytes(&raw)[..] == *wire;
    *input = cursor;
    Ok(Decoded {
        value: Header::new(raw),
        wire,
        wire_is_canonical,
    })
}

/// Reads one block and leaves the rest of the input. Core's `DecodeHexBlk`, P2P `block` and
/// block-file reads all accept trailing bytes.
///
/// A block with zero transactions decodes, as in Core; `CheckBlock` rejects it.
pub fn decode_block<'a>(input: &mut &'a [u8]) -> Result<Decoded<'a, Block>, DecodeError> {
    let start: &'a [u8] = input;
    let mut cursor = start;
    let header = BlockHeader::decode(&mut cursor).map_err(|e| codec_error(0, e))?;
    let mut wire_is_canonical = header_bytes(&header)[..] == *consumed(start, cursor);

    let count_offset = consumed(start, cursor).len();
    // Core: serialize.h ReadCompactSize (range_check): sizes above MAX_SIZE are rejected
    let count = CompactSize::decode(&mut cursor)
        .and_then(|n| n.into_len(MAX_SIZE))
        .map_err(|e| codec_error(count_offset, e))?;
    let mut count_bytes = CompareWriter::new(&consumed(start, cursor)[count_offset..]);
    write_compact_size(count as u64, &mut count_bytes);
    wire_is_canonical &= count_bytes.matches();

    // Each transaction takes at least MIN_TX_SIZE bytes, so a hostile count cannot force a
    // large allocation.
    let mut txs = Vec::with_capacity(count.min(cursor.len() / MIN_TX_SIZE));
    for index in 0..count {
        let offset = consumed(start, cursor).len();
        let decoded = decode_tx(&mut cursor).map_err(|e| DecodeError {
            offset,
            tx_index: Some(index),
            kind: e.kind,
        })?;
        wire_is_canonical &= decoded.wire_is_canonical;
        txs.push(Arc::new(decoded.into_value()));
    }

    let wire = consumed(start, cursor);
    *input = cursor;
    Ok(Decoded {
        value: Block::new(Header::new(header), txs),
        wire,
        wire_is_canonical,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_decode_leaves_the_cursor() {
        let bytes = [0u8; 3];
        let mut cursor = &bytes[..];
        assert!(decode_tx(&mut cursor).is_err());
        assert_eq!(cursor.len(), 3);
        assert!(decode_header(&mut cursor).is_err());
        assert!(decode_block(&mut cursor).is_err());
        assert_eq!(cursor.len(), 3);
    }

    #[test]
    fn a_large_count_does_not_allocate_up_front() {
        // A header and a count of MAX_SIZE with no transactions behind it.
        let mut bytes = vec![0u8; 80];
        write_compact_size(MAX_SIZE as u64, &mut bytes);
        let mut cursor = &bytes[..];
        let error = decode_block(&mut cursor).err();
        // The failing transaction starts after the header and the 5-byte count.
        assert!(matches!(
            error,
            Some(DecodeError {
                offset: 85,
                tx_index: Some(0),
                ..
            })
        ));
    }

    /// A version-1 transaction with no inputs, no outputs and lock time 0.
    const EMPTY_TX: [u8; 10] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    #[test]
    fn exact_decoding_reports_the_trailing_bytes() {
        let mut bytes = EMPTY_TX.to_vec();
        assert!(decode_tx_exact(&bytes).is_ok());
        bytes.extend_from_slice(&[7, 7, 7]);
        assert_eq!(
            decode_tx_exact(&bytes).err(),
            Some(DecodeError {
                offset: 10,
                tx_index: None,
                kind: DecodeErrorKind::TrailingBytes {
                    remaining: 3,
                },
            })
        );
        // The streaming decoder leaves them.
        let mut cursor = &bytes[..];
        assert!(decode_tx(&mut cursor).is_ok());
        assert_eq!(cursor, [7, 7, 7]);
    }

    #[test]
    fn a_failing_transaction_reports_its_block_offset() {
        let mut bytes = vec![0u8; 80];
        write_compact_size(2, &mut bytes);
        bytes.extend_from_slice(&EMPTY_TX);
        bytes.extend_from_slice(&EMPTY_TX[..4]);
        let error = decode_block(&mut &bytes[..]).err();
        assert!(matches!(
            error,
            Some(DecodeError {
                offset: 91,
                tx_index: Some(1),
                ..
            })
        ));
    }

    #[test]
    fn debug_prints_no_bytes() {
        let bytes = [0x55u8; 80];
        let Ok(decoded) = decode_header(&mut &bytes[..]) else {
            panic!("an 80-byte header decodes");
        };
        let printed = format!("{decoded:?}");
        assert!(!printed.contains("85"), "{printed}");
        assert!(printed.contains("wire_len: 80"));
    }

    #[test]
    fn counts_above_max_size_are_rejected() {
        let mut bytes = vec![0u8; 80];
        write_compact_size(MAX_SIZE as u64 + 1, &mut bytes);
        let mut cursor = &bytes[..];
        let error = decode_block(&mut cursor).err();
        assert!(matches!(
            error,
            Some(DecodeError {
                offset: 80,
                tx_index: None,
                ..
            })
        ));
        assert!(error.as_ref().is_some_and(above_max_size));
    }

    /// Core's `ReadCompactSize` refusal: the value is above `MAX_SIZE`, whatever follows.
    fn above_max_size(error: &DecodeError) -> bool {
        matches!(
            &error.kind,
            DecodeErrorKind::Codec(codec::DecodeError::CompactSizeExceedsLimit { limit, value })
                if *limit == MAX_SIZE && *value == MAX_SIZE as u64 + 1
        )
    }

    /// Every length prefix of a transaction is range-checked against `MAX_SIZE` before the bytes
    /// behind it are read, as Core's `ReadCompactSize` does: the input and output counts, both
    /// scripts and the extra payload. One over is refused at the prefix with nothing behind it;
    /// `MAX_SIZE` itself is refused only for the missing bytes.
    #[test]
    fn transaction_length_prefixes_are_bounded_by_max_size() {
        let fields: [(&str, Vec<u8>); 5] = [
            ("input count", vec![1, 0, 0, 0]),
            ("output count", vec![1, 0, 0, 0, 0]),
            ("script_sig", [&[1, 0, 0, 0, 1][..], &[7; 36]].concat()),
            ("script_pubkey", [&[1, 0, 0, 0, 0, 1][..], &[0; 8]].concat()),
            // Version 3, type 5, no inputs or outputs, lock time 0, then the payload.
            ("extra payload", vec![3, 0, 5, 0, 0, 0, 0, 0, 0, 0]),
        ];
        for (field, before) in fields {
            for (length, at_the_prefix) in [(MAX_SIZE, false), (MAX_SIZE + 1, true)] {
                let mut bytes = before.clone();
                write_compact_size(length as u64, &mut bytes);
                let error = decode_tx(&mut &bytes[..]).err();
                assert!(error.is_some(), "{field} of {length}: decoded");
                assert_eq!(
                    error.as_ref().is_some_and(above_max_size),
                    at_the_prefix,
                    "{field} of {length}"
                );
            }
        }
    }
}
