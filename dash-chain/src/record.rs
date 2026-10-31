//! The block index record and its codec (A1 §2.9 v1; SB4 §10.3, D16).

use dash_consensus::primitives::{ReadErrorKind, header_bytes, read_varint_u32, write_varint};
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::codec::BaseCodec as _;

use crate::error::CodecError;
use crate::status::Status;

/// One block index write for CH2's B1 (CH2 §2.7's type, owned here, D16): key = the block hash,
/// value = an encoded [`BlockIndexRecord`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexWrite {
    pub hash: BlockHash,
    pub bytes: Vec<u8>,
}

/// Core's `CDiskBlockIndex` without the client-version prefix and without the inline hash, which
/// is the key (`chain.h:391-413`). No per-record version: `DbSchema` versions the column family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockIndexRecord {
    pub height: u32,
    pub status: Status,
    pub n_tx: u32,
    /// `nFile`, present when `HAVE_DATA | HAVE_UNDO`.
    pub file: Option<i32>,
    /// `nDataPos`, present when `HAVE_DATA`.
    pub data_pos: Option<u32>,
    /// `nUndoPos`, present when `HAVE_UNDO`.
    pub undo_pos: Option<u32>,
    pub header: BlockHeader,
}

/// The largest `VARINT_MODE(.., NONNEGATIVE_SIGNED)` value of an `int` field.
const MAX_INT: u32 = i32::MAX as u32;

impl BlockIndexRecord {
    /// The longest [`encode`](Self::encode): six varints of a 32-bit value (five bytes each at
    /// most) and the header. CH2 sizes B1's batch from it (BL-511).
    pub const MAX_LEN: usize = 6 * 5 + 80;

    /// Appends the value encoding to `out`: `VARINT(height)`, `VARINT(status)`, `VARINT(n_tx)`,
    /// `VARINT(file)` if `HAVE_DATA | HAVE_UNDO`, `VARINT(data_pos)` if `HAVE_DATA`,
    /// `VARINT(undo_pos)` if `HAVE_UNDO`, then the 80-byte header. A position the status names
    /// but the record lacks is written as Core's default, 0.
    // Core: chain.h:391-413 CDiskBlockIndex::SERIALIZE_METHODS (no version prefix, no hash)
    pub fn encode(&self, out: &mut Vec<u8>) {
        debug_assert!(self.height <= MAX_INT, "nHeight is an int");
        write_varint(u64::from(self.height), out);
        write_varint(u64::from(self.status.bits()), out);
        write_varint(u64::from(self.n_tx), out);
        if self.status.has(Status::HAVE_MASK) {
            debug_assert!(self.file.is_some_and(|f| f >= 0), "nFile with HAVE_DATA|HAVE_UNDO");
            write_varint(self.file.unwrap_or(0).max(0) as u64, out);
        }
        if self.status.has(Status::HAVE_DATA) {
            debug_assert!(self.data_pos.is_some(), "nDataPos with HAVE_DATA");
            write_varint(u64::from(self.data_pos.unwrap_or(0)), out);
        }
        if self.status.has(Status::HAVE_UNDO) {
            debug_assert!(self.undo_pos.is_some(), "nUndoPos with HAVE_UNDO");
            write_varint(u64::from(self.undo_pos.unwrap_or(0)), out);
        }
        out.extend_from_slice(&header_bytes(&self.header));
    }

    /// The encoding as a new buffer.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96);
        self.encode(&mut out);
        out
    }

    /// Decodes one value; trailing bytes are an error. The `int` fields (`nHeight`, `nFile`)
    /// fail as Core's `ReadVarInt<int>` does above `i32::MAX`.
    // Core: chain.h:391-413 CDiskBlockIndex::SERIALIZE_METHODS; serialize.h ReadVarInt
    pub fn decode(bytes: &[u8]) -> Result<BlockIndexRecord, CodecError> {
        let mut input = bytes;
        let height = read_int(&mut input)?;
        let status = Status(read_u32(&mut input)?);
        let n_tx = read_u32(&mut input)?;
        let file = match status.has(Status::HAVE_MASK) {
            true => Some(read_int(&mut input)? as i32),
            false => None,
        };
        let data_pos = match status.has(Status::HAVE_DATA) {
            true => Some(read_u32(&mut input)?),
            false => None,
        };
        let undo_pos = match status.has(Status::HAVE_UNDO) {
            true => Some(read_u32(&mut input)?),
            false => None,
        };
        if input.len() < 80 {
            return Err(CodecError::Truncated);
        }
        let header = BlockHeader::decode(&mut input).map_err(|_| CodecError::Truncated)?;
        if !input.is_empty() {
            return Err(CodecError::Trailing(input.len()));
        }
        Ok(BlockIndexRecord {
            height,
            status,
            n_tx,
            file,
            data_pos,
            undo_pos,
            header,
        })
    }
}

fn read_u32(input: &mut &[u8]) -> Result<u32, CodecError> {
    read_varint_u32(input).map_err(|kind| match kind {
        ReadErrorKind::VarIntTooLarge => CodecError::VarIntOverflow,
        _ => CodecError::Truncated,
    })
}

/// `VARINT_MODE(.., NONNEGATIVE_SIGNED)` into an `int`. Reading as `uint32_t` and then bounding
/// the value rejects exactly the inputs `ReadVarInt<int>` rejects: every digit check that fails
/// for `INT_MAX` either also fails for `UINT32_MAX` or leaves a final value above `INT_MAX`.
fn read_int(input: &mut &[u8]) -> Result<u32, CodecError> {
    match read_u32(input)? {
        n if n > MAX_INT => Err(CodecError::VarIntOverflow),
        n => Ok(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(nonce: u32) -> BlockHeader {
        BlockHeader {
            version: 0x2000_0000,
            time: 1_700_000_000,
            bits: 0x1e0f_fff0,
            nonce,
            ..BlockHeader::default()
        }
    }

    fn record(status: u32) -> BlockIndexRecord {
        let s = Status(status);
        BlockIndexRecord {
            height: 1_234_567,
            status: s,
            n_tx: 42,
            file: s.has(Status::HAVE_MASK).then_some(17),
            data_pos: s.has(Status::HAVE_DATA).then_some(0x00ab_cdef),
            undo_pos: s.has(Status::HAVE_UNDO).then_some(300),
            header: header(7),
        }
    }

    /// Every field at its widest: [`BlockIndexRecord::MAX_LEN`], which B1's sizing takes for a
    /// record (BL-511).
    #[test]
    fn the_widest_record_is_max_len() {
        let r = BlockIndexRecord {
            height: MAX_INT,
            status: Status(u32::MAX),
            n_tx: u32::MAX,
            file: Some(i32::MAX),
            data_pos: Some(u32::MAX),
            undo_pos: Some(u32::MAX),
            header: header(7),
        };
        assert_eq!(r.to_bytes().len(), BlockIndexRecord::MAX_LEN);
    }

    #[test]
    fn round_trips_every_position_combination() {
        for status in [
            Status::VALID_TREE,
            Status::VALID_SCRIPTS | Status::HAVE_DATA,
            Status::VALID_SCRIPTS | Status::HAVE_UNDO,
            Status::VALID_SCRIPTS | Status::HAVE_MASK,
            Status::VALID_TREE | Status::FAILED_VALID | Status::CONFLICT_CHAINLOCK,
        ] {
            let r = record(status);
            let bytes = r.to_bytes();
            assert_eq!(BlockIndexRecord::decode(&bytes), Ok(r), "status {status:#x}");
        }
    }

    #[test]
    fn layout_is_core_disk_index_without_version_and_hash() {
        let r = BlockIndexRecord {
            height: 1,
            status: Status(Status::VALID_SCRIPTS | Status::HAVE_MASK),
            n_tx: 1,
            file: Some(0),
            data_pos: Some(8),
            undo_pos: Some(8),
            header: header(0),
        };
        let bytes = r.to_bytes();
        // VARINT 1, VARINT 29, VARINT 1, VARINT 0, VARINT 8, VARINT 8, then the header.
        assert_eq!(bytes[..6], [1, 29, 1, 0, 8, 8]);
        assert_eq!(bytes[6..], header_bytes(&r.header));
    }

    #[test]
    fn rejects_truncation_trailing_bytes_and_wide_ints() {
        let bytes = record(Status::VALID_SCRIPTS | Status::HAVE_MASK).to_bytes();
        for len in 0..bytes.len() {
            assert_eq!(BlockIndexRecord::decode(&bytes[..len]), Err(CodecError::Truncated));
        }
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(BlockIndexRecord::decode(&long), Err(CodecError::Trailing(1)));

        // nHeight = INT_MAX + 1 decodes as a uint32_t but not as an int.
        let mut wide = Vec::new();
        write_varint(u64::from(MAX_INT) + 1, &mut wide);
        wide.extend_from_slice(&bytes[3..]);
        assert_eq!(BlockIndexRecord::decode(&wide), Err(CodecError::VarIntOverflow));

        // nStatus = UINT32_MAX + 1.
        let mut over = vec![1];
        write_varint(u64::from(u32::MAX) + 1, &mut over);
        assert_eq!(BlockIndexRecord::decode(&over), Err(CodecError::VarIntOverflow));
    }

    #[test]
    fn int_bound_matches_core_read_varint_for_int() {
        // Core's ReadVarInt<int>, written out for INT_MAX; compared with read_int on every
        // 1..=5 byte input whose digits sit near the boundary.
        fn core_int(mut input: &[u8]) -> Option<u32> {
            let max = u64::from(MAX_INT);
            let mut n: u64 = 0;
            loop {
                let (&byte, rest) = input.split_first()?;
                input = rest;
                if n > (max >> 7) {
                    return None;
                }
                n = (n << 7) | u64::from(byte & 0x7f);
                if byte & 0x80 == 0 {
                    return Some(n as u32);
                }
                if n == max {
                    return None;
                }
                n += 1;
            }
        }
        let digits = [0x00u8, 0x01, 0x06, 0x07, 0x08, 0x0f, 0x7e, 0x7f];
        let mut checked = 0;
        for len in 1..=5usize {
            let mut idx = vec![0usize; len];
            loop {
                let mut input: Vec<u8> = idx.iter().map(|&i| digits[i] | 0x80).collect();
                *input.last_mut().unwrap() &= 0x7f;
                let ours = read_int(&mut &input[..]).ok();
                assert_eq!(ours, core_int(&input), "{input:02x?}");
                checked += 1;
                let Some(pos) = idx.iter().rposition(|&i| i + 1 < digits.len()) else {
                    break;
                };
                idx[pos] += 1;
                idx[pos + 1..].fill(0);
            }
        }
        assert!(checked > 30_000);
    }
}
