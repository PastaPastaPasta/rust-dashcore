//! Reference block splitter, independent of both libraries under test.
//!
//! It mirrors Dash Core's `CBlock`/`CTransaction` deserialization just far enough to find
//! the byte boundaries of the header and of every transaction, so a decode failure in one
//! library can be pinned to an exact transaction while the rest of the block is still
//! checked. The rules follow `src/primitives/transaction.h` (`UnserializeTransaction`) and
//! `src/serialize.h` (`ReadCompactSize`):
//!
//! - the 32-bit version field is split into `nVersion` (low 16 bits, *signed*) and `nType`
//!   (high 16 bits);
//! - `vExtraPayload` is present iff `nVersion >= 3 && nType != TRANSACTION_NORMAL`;
//! - CompactSize must be canonical and at most `MAX_SIZE` (0x02000000).

use std::ops::Range;

/// Dash Core `MAX_SIZE` for CompactSize values.
const MAX_SIZE: u64 = 0x0200_0000;

/// Header length of a serialized block.
pub const HEADER_LEN: usize = 80;

/// When a transaction carries `vExtraPayload`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadRule {
    /// `nVersion >= 3 && nType != 0`: Core since v21 (`72d2008901`).
    Ge3,
    /// `nVersion == 3 && nType != 0`: the (de)serialization before v21.
    Eq3,
}

impl PayloadRule {
    fn has_payload(self, version: i16, tx_type: u16) -> bool {
        tx_type != 0
            && match self {
                PayloadRule::Ge3 => version >= 3,
                PayloadRule::Eq3 => version == 3,
            }
    }
}

/// Byte boundaries of one transaction inside a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxSpan {
    /// Range of the whole transaction within the block bytes.
    pub range: Range<usize>,
    /// `nVersion` as Core reads it (signed 16 bits).
    pub version: i16,
    /// `nType` as Core reads it.
    pub tx_type: u16,
    /// Range of the `vExtraPayload` bytes (excluding the length prefix), if present.
    pub payload: Option<Range<usize>>,
    /// Number of inputs.
    pub n_in: u64,
    /// Number of outputs.
    pub n_out: u64,
    /// Sum of the output values, saturating (`GetValueOut` before its range check).
    pub value_out: u64,
    /// Range of the first input's `scriptSig`, if there is an input.
    pub first_script_sig: Option<Range<usize>>,
}

/// Result of splitting a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSpans {
    pub txs: Vec<TxSpan>,
}

/// Why the splitter could not parse a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitError {
    pub offset: usize,
    pub tx_index: Option<usize>,
    pub reason: &'static str,
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.tx_index {
            Some(i) => write!(f, "{} at offset {} (tx {})", self.reason, self.offset, i),
            None => write!(f, "{} at offset {}", self.reason, self.offset),
        }
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let out = self.data.get(self.pos..end).ok_or("unexpected end of data")?;
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }

    fn compact_size(&mut self) -> Result<u64, &'static str> {
        let first = self.u8()?;
        let value = match first {
            0..=0xfc => u64::from(first),
            0xfd => {
                let v = u64::from(u16::from_le_bytes(self.take(2)?.try_into().expect("2 bytes")));
                if v < 0xfd {
                    return Err("non-canonical ReadCompactSize()");
                }
                v
            }
            0xfe => {
                let v = u64::from(self.u32()?);
                if v < 0x1_0000 {
                    return Err("non-canonical ReadCompactSize()");
                }
                v
            }
            0xff => {
                let v = u64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes"));
                if v < 0x1_0000_0000 {
                    return Err("non-canonical ReadCompactSize()");
                }
                v
            }
        };
        if value > MAX_SIZE {
            return Err("ReadCompactSize(): size too large");
        }
        Ok(value)
    }

    fn skip_bytes_with_len(&mut self) -> Result<Range<usize>, &'static str> {
        let len = self.compact_size()? as usize;
        let start = self.pos;
        self.take(len)?;
        Ok(start..self.pos)
    }
}

fn split_tx(c: &mut Cursor<'_>, rule: PayloadRule) -> Result<TxSpan, &'static str> {
    let start = c.pos;
    let raw = c.u32()?;
    let version = (raw & 0xffff) as u16 as i16;
    let tx_type = (raw >> 16) as u16;
    let n_in = c.compact_size()?;
    let mut first_script_sig = None;
    for i in 0..n_in {
        c.take(36)?; // prevout
        let script = c.skip_bytes_with_len()?; // scriptSig
        if i == 0 {
            first_script_sig = Some(script);
        }
        c.take(4)?; // nSequence
    }
    let n_out = c.compact_size()?;
    let mut value_out = 0u64;
    for _ in 0..n_out {
        let value = u64::from_le_bytes(c.take(8)?.try_into().expect("8 bytes"));
        value_out = value_out.saturating_add(value);
        c.skip_bytes_with_len()?; // scriptPubKey
    }
    c.take(4)?; // nLockTime
    let payload = if rule.has_payload(version, tx_type) {
        Some(c.skip_bytes_with_len()?)
    } else {
        None
    };
    Ok(TxSpan {
        range: start..c.pos,
        version,
        tx_type,
        payload,
        n_in,
        n_out,
        value_out,
        first_script_sig,
    })
}

/// Splits a serialized block into its transactions, as Core does today.
pub fn split_block(data: &[u8]) -> Result<BlockSpans, SplitError> {
    split_block_with(data, PayloadRule::Ge3)
}

/// Splits a serialized block under the given payload rule.
pub fn split_block_with(data: &[u8], rule: PayloadRule) -> Result<BlockSpans, SplitError> {
    let mut c = Cursor {
        data,
        pos: 0,
    };
    let err = |c: &Cursor<'_>, tx_index, reason| SplitError {
        offset: c.pos,
        tx_index,
        reason,
    };
    c.take(HEADER_LEN).map_err(|r| err(&c, None, r))?;
    let n = c.compact_size().map_err(|r| err(&c, None, r))?;
    let mut txs = Vec::with_capacity(n.min(100_000) as usize);
    for i in 0..n as usize {
        let span = split_tx(&mut c, rule).map_err(|r| err(&c, Some(i), r))?;
        txs.push(span);
    }
    if c.pos != data.len() {
        return Err(err(&c, None, "trailing bytes after last transaction"));
    }
    Ok(BlockSpans {
        txs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits `tx` as a single transaction under today's rule.
    fn split_one(tx: &[u8]) -> TxSpan {
        split_tx(
            &mut Cursor {
                data: tx,
                pos: 0,
            },
            PayloadRule::Ge3,
        )
        .unwrap()
    }

    #[test]
    fn rejects_non_canonical_compact_size() {
        let mut c = Cursor {
            data: &[0xfd, 0x10, 0x00],
            pos: 0,
        };
        assert_eq!(c.compact_size(), Err("non-canonical ReadCompactSize()"));
    }

    #[test]
    fn payload_presence_follows_core() {
        // version 2, type 5: Core reads no payload even though nType != 0
        let mut tx = vec![0x02, 0x00, 0x05, 0x00];
        tx.extend_from_slice(&[0x00, 0x00]); // no inputs, no outputs
        tx.extend_from_slice(&[0, 0, 0, 0]); // lock time
        let span = split_one(&tx);
        assert_eq!(span.payload, None);
        assert_eq!(span.range, 0..tx.len());

        // negative version (0xffff) with type 1: still no payload (nVersion is signed)
        tx[0] = 0xff;
        tx[1] = 0xff;
        let span = split_one(&tx);
        assert_eq!(span.version, -1);
        assert_eq!(span.payload, None);

        // version 3, type 5: payload with length prefix
        tx[0] = 0x03;
        tx[1] = 0x00;
        tx.extend_from_slice(&[0x02, 0xaa, 0xbb]);
        let span = split_one(&tx);
        assert_eq!(span.payload, Some(11..13));
    }
}
