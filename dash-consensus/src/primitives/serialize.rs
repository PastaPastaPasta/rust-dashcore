//! Core's serialization: byte sinks, the generic encoders, the [`CoreSerialize`] trait and
//! [`CoreBytes`] (CS0 §4.2, §4.5).
//!
//! Only generic encoders live here (little-endian integers, `WriteCompactSize`, byte vectors,
//! `DYNBITSET`). Everything that knows a transaction's layout is in [`super::canonical`], a final
//! commitment's in [`super::commitment`].

use std::fmt;
use std::sync::Arc;

use dash_primitives::{BlockHeader, DynBitset, Transaction};

use super::block::{Block, Header};
use super::canonical;
use super::tx::Tx;
pub use crate::hash::HashWriter;

/// A byte sink for Core's serialization.
pub trait CoreWrite {
    fn write(&mut self, bytes: &[u8]);
}

impl CoreWrite for Vec<u8> {
    fn write(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

impl CoreWrite for HashWriter {
    fn write(&mut self, bytes: &[u8]) {
        self.input(bytes);
    }
}

/// Counts what is written (Core's `GetSerializeSize`).
#[derive(Clone, Copy, Debug, Default)]
pub struct SizeWriter(usize);

impl SizeWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn size(&self) -> usize {
        self.0
    }
}

impl CoreWrite for SizeWriter {
    fn write(&mut self, bytes: &[u8]) {
        self.0 += bytes.len();
    }
}

/// Writes into a fixed `N`-byte buffer without allocating (an inline slot). Past the end it stops
/// copying, sets the overflow flag and keeps counting, so the caller learns the size it needs
/// without encoding twice.
#[derive(Clone, Copy)]
pub struct ArrayWriter<const N: usize> {
    buf: [u8; N],
    written: usize,
}

impl<const N: usize> ArrayWriter<N> {
    pub fn new() -> Self {
        Self {
            buf: [0; N],
            written: 0,
        }
    }

    /// The bytes written, or `None` when they did not fit.
    pub fn as_slice(&self) -> Option<&[u8]> {
        self.buf.get(..self.written)
    }

    /// The number of bytes written, including any past the end.
    pub fn written(&self) -> usize {
        self.written
    }

    /// Whether more than `N` bytes were written.
    pub fn overflowed(&self) -> bool {
        self.written > N
    }
}

impl<const N: usize> Default for ArrayWriter<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> CoreWrite for ArrayWriter<N> {
    fn write(&mut self, bytes: &[u8]) {
        if let Some(dst) = self.buf.get_mut(self.written..self.written + bytes.len()) {
            dst.copy_from_slice(bytes);
        }
        self.written += bytes.len();
    }
}

/// The counts only, never the bytes (CS0 §9.5).
impl<const N: usize> fmt::Debug for ArrayWriter<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ArrayWriter<{N}>({} bytes)", self.written)
    }
}

/// The size of what `write` writes: `GetSerializeSize` without a second size formula to keep
/// in step with the writer.
pub(crate) fn size_of(write: impl FnOnce(&mut SizeWriter)) -> usize {
    let mut size = SizeWriter::new();
    write(&mut size);
    size.size()
}

/// Compares what is written against `wire`, and counts it. Allocation-free.
///
/// It stops comparing at the first difference. Once the output runs past the end of `wire` it
/// stops comparing too, keeps counting and never indexes past the end; [`Self::matches`] then
/// also requires equal lengths, so a shorter output never matches either.
#[derive(Clone, Copy)]
pub struct CompareWriter<'a> {
    wire: &'a [u8],
    written: usize,
    equal: bool,
}

impl<'a> CompareWriter<'a> {
    pub fn new(wire: &'a [u8]) -> Self {
        Self {
            wire,
            written: 0,
            equal: true,
        }
    }

    /// The number of bytes written: `GetSerializeSize` of what was written.
    pub fn written(&self) -> usize {
        self.written
    }

    /// Every byte written equals `wire`, and exactly `wire.len()` bytes were written.
    pub fn matches(&self) -> bool {
        self.equal && self.written == self.wire.len()
    }
}

/// The counts only, never the bytes (CS0 §9.5).
impl fmt::Debug for CompareWriter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompareWriter")
            .field("wire_len", &self.wire.len())
            .field("written", &self.written)
            .field("equal", &self.equal)
            .finish()
    }
}

impl CoreWrite for CompareWriter<'_> {
    fn write(&mut self, bytes: &[u8]) {
        if self.equal {
            self.equal = self
                .wire
                .get(self.written..)
                .and_then(|rest| rest.get(..bytes.len()))
                .is_some_and(|expected| expected == bytes);
        }
        self.written += bytes.len();
    }
}

// Core: serialize.h WriteCompactSize
/// Core's `WriteCompactSize`.
pub fn write_compact_size(n: u64, w: &mut impl CoreWrite) {
    if n < 253 {
        w.write(&[n as u8]);
    } else if n <= 0xffff {
        w.write(&[253]);
        w.write(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        w.write(&[254]);
        w.write(&(n as u32).to_le_bytes());
    } else {
        w.write(&[255]);
        w.write(&n.to_le_bytes());
    }
}

/// The size of `WriteCompactSize(n)`: Core's `GetSizeOfCompactSize`.
pub fn compact_size_len(n: u64) -> usize {
    match n {
        0..253 => 1,
        253..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

// Core: serialize.h:387 WriteVarInt (v23.1.8 :383)
/// Core's `VARINT` (`VarIntMode::DEFAULT`): MSB base-128 digits, one subtracted from every digit
/// but the last, so each number has exactly one encoding. The encoding does not depend on the
/// integer's width, so this one function writes Core's `uint32_t` and `uint64_t` fields alike.
pub fn write_varint(mut n: u64, w: &mut impl CoreWrite) {
    let mut tmp = [0u8; 10];
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
    tmp[..=len].reverse();
    w.write(&tmp[..=len]);
}

// Core: serialize.h:370 GetSizeOfVarInt (v23.1.8 :366)
/// The size of `write_varint(n)`: Core's `GetSizeOfVarInt`.
pub fn varint_len(mut n: u64) -> usize {
    let mut len = 1;
    while n > 0x7f {
        n = (n >> 7) - 1;
        len += 1;
    }
    len
}

/// A byte vector as Core serializes `std::vector<unsigned char>` and `CScript`: its length as a
/// compact size, then the bytes.
pub fn write_bytes(bytes: &[u8], w: &mut impl CoreWrite) {
    write_compact_size(bytes.len() as u64, w);
    w.write(bytes);
}

// Core: serialize.h:623 DynamicBitSetFormatter::Ser, :433 WriteFixedBitSet
/// Core's `DYNBITSET` of the `std::vector<bool>` the bitset holds: its size as a compact size,
/// then `ceil(size / 8)` bytes, bit `p` at bit `p % 8` of byte `p / 8`. The padding bits are zero,
/// and bytes missing from a bitset built by hand are zeros, as the vector's bits would be.
pub fn write_dyn_bitset(bits: &DynBitset, w: &mut impl CoreWrite) {
    write_compact_size(bits.num_bits, w);
    let Some(last) = bits.num_bits.div_ceil(8).checked_sub(1) else {
        return;
    };
    let last = usize::try_from(last).unwrap_or(usize::MAX);
    let held = bits.data.len().min(last);
    w.write(&bits.data[..held]);
    for _ in held..last {
        w.write(&[0]);
    }
    let tail = bits.data.get(last).copied().unwrap_or(0);
    w.write(&[match bits.num_bits % 8 {
        0 => tail,
        used => tail & ((1 << used) - 1),
    }]);
}

mod sealed {
    pub trait Sealed {}
}

/// Core's serialization of a consensus object: the only encoder node code uses for
/// transactions, headers and blocks (A1 §1.3).
///
/// Sealed: implemented only for [`Transaction`], [`Tx`], [`BlockHeader`], [`Header`] and
/// [`Block`], so no type outside this crate can supply its own bytes to [`core_bytes`].
///
/// ```compile_fail,E0277
/// use dash_consensus::{CoreSerialize, CoreWrite};
/// struct Forged;
/// impl CoreSerialize for Forged {
///     fn core_write(&self, w: &mut impl CoreWrite) {
///         w.write(b"not Core's serialization");
///     }
/// }
/// ```
pub trait CoreSerialize: sealed::Sealed {
    /// Writes Core's serialization to `w`.
    fn core_write(&self, w: &mut impl CoreWrite);

    /// `GetSerializeSize`.
    fn core_size(&self) -> usize {
        let mut size = SizeWriter::new();
        self.core_write(&mut size);
        size.size()
    }

    /// Core's serialization as a vector.
    fn core_serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.core_size());
        self.core_write(&mut out);
        out
    }
}

/// The A1 name for the sole consensus encoder.
pub fn core_serialize<T: CoreSerialize + ?Sized>(value: &T) -> Vec<u8> {
    value.core_serialize()
}

impl sealed::Sealed for Transaction {}
impl CoreSerialize for Transaction {
    fn core_write(&self, w: &mut impl CoreWrite) {
        canonical::write_tx(self, w);
    }
}

impl sealed::Sealed for Tx {}
impl CoreSerialize for Tx {
    fn core_write(&self, w: &mut impl CoreWrite) {
        canonical::write_tx(self.raw(), w);
    }

    fn core_size(&self) -> usize {
        self.size() as usize
    }
}

// Core: primitives/block.h:40 CBlockHeader SERIALIZE_METHODS: nVersion, hashPrevBlock,
// hashMerkleRoot, nTime, nBits, nNonce
/// The 80 bytes of Core's header serialization.
pub fn header_bytes(header: &BlockHeader) -> [u8; 80] {
    let mut out = [0u8; 80];
    out[0..4].copy_from_slice(&header.version.to_le_bytes());
    out[4..36].copy_from_slice(header.prev_hash.as_bytes());
    out[36..68].copy_from_slice(header.merkle_root.as_bytes());
    out[68..72].copy_from_slice(&header.time.to_le_bytes());
    out[72..76].copy_from_slice(&header.bits.to_le_bytes());
    out[76..80].copy_from_slice(&header.nonce.to_le_bytes());
    out
}

impl sealed::Sealed for BlockHeader {}
impl CoreSerialize for BlockHeader {
    fn core_write(&self, w: &mut impl CoreWrite) {
        w.write(&header_bytes(self));
    }

    fn core_size(&self) -> usize {
        80
    }
}

impl sealed::Sealed for Header {}
impl CoreSerialize for Header {
    fn core_write(&self, w: &mut impl CoreWrite) {
        self.raw().core_write(w);
    }

    fn core_size(&self) -> usize {
        80
    }
}

// Core: primitives/block.h:213 CBlock SERIALIZE_METHODS: the header, then vtx
impl sealed::Sealed for Block {}
impl CoreSerialize for Block {
    fn core_write(&self, w: &mut impl CoreWrite) {
        self.header().core_write(w);
        write_compact_size(self.txs().len() as u64, w);
        for tx in self.txs() {
            tx.core_write(w);
        }
    }

    fn core_size(&self) -> usize {
        self.size() as usize
    }
}

/// Bytes equal to Core's serialization of a transaction, header or block (CS0 §4.5).
///
/// Storage, relay and RPC hex take this type, so base-sdk's encoders cannot reach them. There
/// are exactly two ways to make one: [`core_bytes`] (CS0's encoder) and
/// [`Decoded::core_bytes`](super::Decoded::core_bytes) (the received bytes, when the decoder's
/// exact comparison found them equal to Core's serialization). Cheap to clone for relay fan-out.
///
/// ```compile_fail,E0423
/// let forged = dash_consensus::CoreBytes(std::sync::Arc::from(&b"bytes"[..]));
/// ```
///
/// ```compile_fail,E0277
/// let forged: dash_consensus::CoreBytes = vec![0u8; 4].into();
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct CoreBytes(Arc<[u8]>);

impl CoreBytes {
    /// Crate-private: `bytes` must be Core's serialization of an object (the decoder passes the
    /// received range only after its comparison matched).
    pub(crate) fn from_canonical(bytes: &[u8]) -> Self {
        Self(Arc::from(bytes))
    }

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

impl AsRef<[u8]> for CoreBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// The length only, never the bytes (CS0 §9.5).
impl fmt::Debug for CoreBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CoreBytes({} bytes)", self.0.len())
    }
}

/// `CoreBytes` from CS0's encoder.
pub fn core_bytes<T: CoreSerialize + ?Sized>(value: &T) -> CoreBytes {
    CoreBytes(Arc::from(value.core_serialize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_size_matches_core_layout() {
        for (n, expected) in [
            (0u64, &[0x00][..]),
            (252, &[0xfc]),
            (253, &[0xfd, 0xfd, 0x00]),
            (0xffff, &[0xfd, 0xff, 0xff]),
            (0x1_0000, &[0xfe, 0x00, 0x00, 0x01, 0x00]),
            (0xffff_ffff, &[0xfe, 0xff, 0xff, 0xff, 0xff]),
            (0x1_0000_0000, &[0xff, 0, 0, 0, 0, 1, 0, 0, 0]),
        ] {
            let mut out = Vec::new();
            write_compact_size(n, &mut out);
            assert_eq!(out, expected, "{n}");
            assert_eq!(compact_size_len(n), expected.len(), "{n}");
        }
    }

    /// `WriteFixedBitSet` writes `ceil(size / 8)` bytes, bit `p` at bit `p % 8` of byte `p / 8`,
    /// and its padding bits are zero: a bitset built with padding bits set, or with fewer bytes
    /// than its size, serializes as Core's `std::vector<bool>` of that size would.
    #[test]
    fn bitsets_take_cores_length_and_padding() {
        for (num_bits, data, want) in [
            (0, vec![], vec![0]),
            (0, vec![0xff], vec![0]),
            (3, vec![0xff], vec![3, 0b111]),
            (8, vec![0xff], vec![8, 0xff]),
            (9, vec![0xff, 0xff], vec![9, 0xff, 0b1]),
            (13, vec![0xa5, 0xff, 0xff], vec![13, 0xa5, 0b1_1111]),
            (17, vec![0x01], vec![17, 0x01, 0, 0]),
            (16, vec![], vec![16, 0, 0]),
            (400, vec![0xff; 50], [vec![0xfd, 0x90, 0x01], vec![0xff; 50]].concat()),
        ] {
            let bits = DynBitset {
                num_bits,
                data,
            };
            let mut out = Vec::new();
            write_dyn_bitset(&bits, &mut out);
            assert_eq!(out, want, "{num_bits} bits of {:?}", bits.data);
        }
    }

    #[test]
    fn compare_writer_requires_equal_bytes_and_length() {
        let wire = [1u8, 2, 3, 4];
        let mut equal = CompareWriter::new(&wire);
        equal.write(&[1, 2]);
        equal.write(&[3, 4]);
        assert!(equal.matches());
        assert_eq!(equal.written(), 4);

        let mut shorter = CompareWriter::new(&wire);
        shorter.write(&[1, 2, 3]);
        assert!(!shorter.matches());
        assert_eq!(shorter.written(), 3);

        let mut different = CompareWriter::new(&wire);
        different.write(&[1, 9]);
        different.write(&[3, 4]);
        assert!(!different.matches());
        assert_eq!(different.written(), 4);
    }

    #[test]
    fn compare_writer_counts_past_the_end_without_indexing_it() {
        let wire = [1u8, 2];
        let mut longer = CompareWriter::new(&wire);
        longer.write(&[1, 2]);
        longer.write(&[3, 4, 5]);
        longer.write(&[6]);
        assert!(!longer.matches());
        assert_eq!(longer.written(), 6);

        let mut straddling = CompareWriter::new(&wire);
        straddling.write(&[1]);
        straddling.write(&[2, 3]);
        assert!(!straddling.matches());
        assert_eq!(straddling.written(), 3);
    }

    #[test]
    fn array_writer_fills_its_buffer_and_reports_overflow() {
        let mut fits = ArrayWriter::<4>::new();
        fits.write(&[1, 2]);
        fits.write(&[3, 4]);
        assert_eq!(fits.as_slice(), Some(&[1, 2, 3, 4][..]));
        assert!(!fits.overflowed());

        let mut over = ArrayWriter::<4>::new();
        over.write(&[1, 2, 3]);
        over.write(&[4, 5]);
        over.write(&[6]);
        assert_eq!(over.as_slice(), None);
        assert!(over.overflowed());
        assert_eq!(over.written(), 6);
    }

    #[test]
    fn core_bytes_debug_prints_the_length_only() {
        let header = BlockHeader::default();
        assert_eq!(format!("{:?}", core_bytes(&header)), "CoreBytes(80 bytes)");
    }
}
