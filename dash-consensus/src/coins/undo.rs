//! Core's undo records (`undo.h`): [`TxUndo`] (`CTxUndo`) and [`BlockUndo`] (`CBlockUndo`), owned,
//! or borrowed from their bytes as [`TxUndoRef`] and [`BlockUndoRef`]; their decoders; and
//! [`CoreUndoBytes`], the bytes a rev file stores. Identical at v23.1.8 and v24.0.0-rc.3.
//!
//! The borrowed records hold [`CoinRef`]s, so a record read from a rev file, or built from coins
//! the cache holds as bytes, is written back without allocating scripts or decompressing keys
//! except where Core's re-serialization needs it (an entry of codes 4 and 5).
//!
//! Applying a record needs no reads: each entry is a whole coin. The exception is undo written
//! before the per-output format (Dash v0.12.2.2), whose entries for all but a transaction's last
//! spent output have height 0 and no coinbase flag; Core fills those in from another output of
//! the same transaction (`ApplyTxInUndo`, [`super::access_by_txid`]). The decoder reports a zero
//! height and leaves the decision to the caller.

use std::fmt;
use std::sync::Arc;

use super::coin::{Coin, CoinRef};
use crate::primitives::decode::consumed;
use crate::primitives::read::read_whole;
use crate::primitives::serialize::size_of;
use crate::primitives::{
    CompareWriter, CoreWrite, Decoded, ReadError, ReadErrorKind, read_compact_size,
    write_compact_size,
};

/// The smallest undo entry for a coin: a 1-byte code (height 0, so no version byte), a 1-byte
/// amount and a 1-byte empty raw script.
const MIN_COIN_UNDO_SIZE: usize = 3;
/// The smallest `CTxUndo`: its count.
const MIN_TX_UNDO_SIZE: usize = 1;

/// Core's `CTxUndo`: the coins a transaction's inputs spent, in input order.
// Core: undo.h:53 CTxUndo
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TxUndo {
    /// `vprevout`.
    pub coins: Vec<Coin>,
}

/// Core's `CBlockUndo`: one [`TxUndo`] per transaction after the coinbase, in block order.
// Core: undo.h:63 CBlockUndo
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockUndo {
    /// `vtxundo`.
    pub txs: Vec<TxUndo>,
}

/// Core's `MAX_VECTOR_ALLOCATE`: the most bytes a vector reserves before its elements are read.
// Core: serialize.h:45 MAX_VECTOR_ALLOCATE (v23.1.8 :41)
const MAX_VECTOR_ALLOCATE: usize = 5_000_000;

/// Reads a compact-size count and the elements behind it. The capacity reserved up front is
/// bounded by what the remaining input can hold and by Core's 5 MB batch, so a forged count
/// cannot force a large allocation.
// Core: serialize.h:847 VectorFormatter::Unser; :801 UnserializeVectorContents (v23.1.8 :824, :778)
fn read_vector<'a, T>(
    input: &mut &'a [u8],
    min_element_size: usize,
    mut read: impl FnMut(&mut &'a [u8]) -> Result<T, ReadErrorKind>,
) -> Result<Vec<T>, ReadErrorKind> {
    let count = read_compact_size(input, true)? as usize;
    let reserve = count
        .min(input.len() / min_element_size)
        .min(MAX_VECTOR_ALLOCATE / std::mem::size_of::<T>());
    let mut out = Vec::with_capacity(reserve);
    for _ in 0..count {
        out.push(read(input)?);
    }
    Ok(out)
}

// Core: undo.h:59 CTxUndo SERIALIZE_METHODS (VectorFormatter<TxInUndoFormatter>)
/// `CTxUndo`'s serialization: the count, then each entry (`TxInUndoFormatter::Ser`).
fn write_tx_undo<'c>(coins: impl ExactSizeIterator<Item = CoinRef<'c>>, w: &mut impl CoreWrite) {
    write_compact_size(coins.len() as u64, w);
    for coin in coins {
        coin.write_undo(w);
    }
}

/// [`CoreUndoBytes`] of the record `write` writes, encoded once (sizing first would decompress
/// the keys of codes 4 and 5 twice).
fn undo_bytes(write: impl FnOnce(&mut Vec<u8>)) -> CoreUndoBytes {
    let mut out = Vec::new();
    write(&mut out);
    CoreUndoBytes(Arc::from(out))
}

impl TxUndo {
    pub fn write(&self, w: &mut impl CoreWrite) {
        write_tx_undo(self.coins.iter().map(Coin::as_ref), w);
    }

    pub fn serialized_size(&self) -> usize {
        size_of(|w| self.write(w))
    }

    fn read(input: &mut &[u8]) -> Result<TxUndo, ReadErrorKind> {
        Ok(TxUndo {
            coins: read_vector(input, MIN_COIN_UNDO_SIZE, |input| {
                CoinRef::read_undo(input).map(|coin| coin.to_coin())
            })?,
        })
    }

    /// One `CTxUndo` from the front of `input`, which advances past it on success and stays
    /// put on failure.
    pub fn decode(input: &mut &[u8]) -> Result<TxUndo, ReadError> {
        read_whole(input, TxUndo::read)
    }
}

impl BlockUndo {
    // Core: undo.h:68 CBlockUndo SERIALIZE_METHODS
    pub fn write(&self, w: &mut impl CoreWrite) {
        write_compact_size(self.txs.len() as u64, w);
        for tx in &self.txs {
            tx.write(w);
        }
    }

    pub fn serialized_size(&self) -> usize {
        size_of(|w| self.write(w))
    }

    fn read(input: &mut &[u8]) -> Result<BlockUndo, ReadErrorKind> {
        Ok(BlockUndo {
            txs: read_vector(input, MIN_TX_UNDO_SIZE, TxUndo::read)?,
        })
    }

    /// One `CBlockUndo` from the front of `input`, which advances past it on success and stays
    /// put on failure: Core's `UndoReadFromDisk` work (`verifier >> blockundo`), nothing more.
    /// [`decode_block_undo`] also binds the record to its bytes.
    pub fn decode(input: &mut &[u8]) -> Result<BlockUndo, ReadError> {
        read_whole(input, BlockUndo::read)
    }

    /// Core's serialization: what a rev record holds before its checksum.
    pub fn core_bytes(&self) -> CoreUndoBytes {
        undo_bytes(|w| self.write(w))
    }
}

/// A `CTxUndo` borrowed from its bytes, or built from borrowed coins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TxUndoRef<'a> {
    /// `vprevout`.
    pub coins: Vec<CoinRef<'a>>,
}

/// A `CBlockUndo` borrowed from its bytes, or built from borrowed coins: one [`TxUndoRef`] per
/// transaction after the coinbase, in block order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockUndoRef<'a> {
    /// `vtxundo`.
    pub txs: Vec<TxUndoRef<'a>>,
}

impl<'a> TxUndoRef<'a> {
    pub fn write(&self, w: &mut impl CoreWrite) {
        write_tx_undo(self.coins.iter().copied(), w);
    }

    fn read(input: &mut &'a [u8]) -> Result<TxUndoRef<'a>, ReadErrorKind> {
        Ok(TxUndoRef {
            coins: read_vector(input, MIN_COIN_UNDO_SIZE, CoinRef::read_undo)?,
        })
    }

    /// [`TxUndo::decode`] without copying a coin.
    pub fn decode(input: &mut &'a [u8]) -> Result<TxUndoRef<'a>, ReadError> {
        read_whole(input, TxUndoRef::read)
    }
}

impl<'a> BlockUndoRef<'a> {
    pub fn write(&self, w: &mut impl CoreWrite) {
        write_compact_size(self.txs.len() as u64, w);
        for tx in &self.txs {
            tx.write(w);
        }
    }

    pub fn serialized_size(&self) -> usize {
        size_of(|w| self.write(w))
    }

    /// [`BlockUndo::decode`] without copying a coin: each entry is a [`CoinRef`] into `input`.
    pub fn decode(input: &mut &'a [u8]) -> Result<BlockUndoRef<'a>, ReadError> {
        read_whole(input, |input| {
            Ok(BlockUndoRef {
                txs: read_vector(input, MIN_TX_UNDO_SIZE, TxUndoRef::read)?,
            })
        })
    }

    /// Core's serialization: what a rev record holds before its checksum. Every entry is written
    /// by [`CoinRef::write_undo`] and every count from the vectors, so these are Core's bytes for
    /// the coins Core would hold.
    pub fn core_bytes(&self) -> CoreUndoBytes {
        undo_bytes(|w| self.write(w))
    }

    /// The owned record.
    pub fn to_block_undo(&self) -> BlockUndo {
        BlockUndo {
            txs: self
                .txs
                .iter()
                .map(|tx| TxUndo {
                    coins: tx.coins.iter().map(CoinRef::to_coin).collect(),
                })
                .collect(),
        }
    }
}

/// Reads one `CBlockUndo` from the front of `input` and leaves the rest: in a rev file the
/// checksum follows, and Core's reader does not use the record's size field. On failure the
/// cursor does not move.
///
/// The result is bound to the bytes read, which costs a re-encoding into a comparison; a caller
/// that only applies the record uses [`BlockUndo::decode`]. [`Decoded::wire_is_canonical`] says
/// whether the bytes equal Core's serialization of the record, decided exactly. They differ
/// where Core reads a different coin than the bytes spell out (a non-zero legacy version, an
/// amount outside the money range, an off-curve key of codes 4 and 5, a raw script over
/// `MAX_SCRIPT_SIZE`, a special script stored raw): that is why Core checksums the bytes it
/// read, not a re-serialization (`UndoReadFromDisk`'s `CHashVerifier`).
// Core: node/blockstorage.cpp:540 UndoReadFromDisk, :561 verifier >> blockundo (v23.1.8 :516, :535)
pub fn decode_block_undo<'a>(input: &mut &'a [u8]) -> Result<Decoded<'a, BlockUndo>, ReadError> {
    let start: &'a [u8] = input;
    let undo = BlockUndo::decode(input)?;
    let wire = consumed(start, input);
    let mut comparison = CompareWriter::new(wire);
    undo.write(&mut comparison);
    Ok(Decoded::new(undo, wire, comparison.matches()))
}

impl Decoded<'_, BlockUndo> {
    /// The bytes read when they equal Core's serialization, and Core's serialization of the
    /// record otherwise.
    pub fn core_bytes(&self) -> CoreUndoBytes {
        if self.wire_is_canonical() {
            CoreUndoBytes(Arc::from(self.wire()))
        } else {
            self.value().core_bytes()
        }
    }
}

/// Bytes equal to Core's serialization of a `CBlockUndo`: what a rev file record holds before
/// its checksum. The rev-file writer takes this type, so its records come from this encoder.
///
/// There are exactly two ways to make one: [`BlockUndo::core_bytes`] and
/// [`Decoded::<BlockUndo>::core_bytes`](Decoded::core_bytes). It is a different type from
/// [`crate::CoreBytes`], so undo bytes and block bytes cannot be swapped.
///
/// ```compile_fail,E0423
/// let forged = dash_consensus::coins::CoreUndoBytes(std::sync::Arc::from(&b"bytes"[..]));
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct CoreUndoBytes(Arc<[u8]>);

impl CoreUndoBytes {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for CoreUndoBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// The length only, never the bytes (CS0 §9.5).
impl fmt::Debug for CoreUndoBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CoreUndoBytes({} bytes)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::write_varint;

    fn record() -> BlockUndo {
        let p2sh = {
            let mut s = vec![0xa9, 20];
            s.extend([4; 20]);
            s.push(0x87);
            s
        };
        BlockUndo {
            txs: vec![
                TxUndo {
                    coins: vec![
                        Coin::new(5_000, &p2sh, 10, false),
                        Coin::new(7, &[0x51], 11, true),
                    ],
                },
                TxUndo::default(),
                TxUndo {
                    coins: vec![Coin::new(123, &[0x6a; 300], 0, false)],
                },
            ],
        }
    }

    #[test]
    fn records_round_trip_and_are_canonical() {
        let undo = record();
        let bytes = undo.core_bytes();
        assert_eq!(bytes.len(), undo.serialized_size());
        let mut input = bytes.as_slice().to_vec();
        input.extend([0xee; 32]);
        let mut cursor = &input[..];
        let decoded = decode_block_undo(&mut cursor).unwrap();
        assert_eq!(decoded.value(), &undo);
        assert!(decoded.wire_is_canonical());
        assert_eq!(decoded.wire().len(), bytes.len());
        assert_eq!(decoded.core_bytes(), bytes);
        assert_eq!(cursor, &[0xee; 32]);
    }

    #[test]
    fn a_non_zero_version_byte_is_not_canonical() {
        let undo = record();
        let mut bytes = undo.core_bytes().as_slice().to_vec();
        // Three transactions, two coins; the first coin's code is VARINT(20) = 0x14, and the
        // version byte follows it.
        assert_eq!(&bytes[..4], &[0x03, 0x02, 0x14, 0x00]);
        bytes[3] = 0x05;
        let decoded = decode_block_undo(&mut &bytes[..]).unwrap();
        assert_eq!(decoded.value(), &undo);
        assert!(!decoded.wire_is_canonical());
        assert_eq!(decoded.core_bytes(), undo.core_bytes());
    }

    #[test]
    fn a_forged_count_fails_without_a_large_allocation() {
        let mut bytes = Vec::new();
        write_compact_size(0x0200_0000, &mut bytes);
        let mut cursor = &bytes[..];
        let error = decode_block_undo(&mut cursor).unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::EndOfData);
        assert_eq!(cursor.len(), bytes.len());

        let mut too_many = Vec::new();
        write_compact_size(0x0200_0001, &mut too_many);
        assert_eq!(
            decode_block_undo(&mut &too_many[..]).unwrap_err().kind,
            ReadErrorKind::CompactSizeTooLarge
        );
    }

    #[test]
    fn a_borrowed_record_reads_and_writes_core_bytes() {
        let undo = record();
        let bytes = undo.core_bytes();
        let mut cursor = bytes.as_slice();
        let borrowed = BlockUndoRef::decode(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        assert_eq!(borrowed.to_block_undo(), undo);
        assert_eq!(borrowed.core_bytes(), bytes);
        assert_eq!(borrowed.serialized_size(), bytes.len());

        // Built from borrowed coins, as a connect builds it.
        let built = BlockUndoRef {
            txs: undo
                .txs
                .iter()
                .map(|tx| TxUndoRef {
                    coins: tx.coins.iter().map(Coin::as_ref).collect(),
                })
                .collect(),
        };
        assert_eq!(built.core_bytes(), bytes);
        assert_eq!(built, borrowed);

        // A failure leaves the cursor.
        let short = &bytes.as_slice()[..bytes.len() - 1];
        let mut cursor = short;
        assert!(BlockUndoRef::decode(&mut cursor).is_err());
        assert_eq!(cursor.len(), short.len());
    }

    #[test]
    fn a_tx_undo_decodes_alone() {
        let tx = record().txs.remove(0);
        let mut bytes = Vec::new();
        tx.write(&mut bytes);
        assert_eq!(bytes.len(), tx.serialized_size());
        let mut cursor = &bytes[..];
        assert_eq!(TxUndo::decode(&mut cursor), Ok(tx));
        assert!(cursor.is_empty());
        // A legacy version is a VARINT: a long one is read whole.
        let mut legacy = vec![0x01, 0x02];
        write_varint(u64::from(u32::MAX), &mut legacy);
        legacy.extend([0x01, 0x07, 0x51]);
        assert!(TxUndo::decode(&mut &legacy[..]).is_ok());
    }
}
