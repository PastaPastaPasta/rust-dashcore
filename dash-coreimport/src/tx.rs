//! A zero-copy reader for blocks and transactions in Core's wire layout.

use bitcoin_hashes::{Hash, HashEngine, sha256d};

use crate::Hash256;
use crate::error::{Error, Result};

/// Core's `MAX_SIZE`: the largest compact size a deserializer accepts.
const MAX_SIZE: u64 = 0x0200_0000;

/// The most elements a `Vec` reserves before any has been read.
const MAX_PREALLOC: usize = 4096;

/// Transaction version from which typed (DIP2) transactions carry `vExtraPayload`.
// Core: primitives/transaction.h CTransaction::SPECIAL_VERSION
const SPECIAL_VERSION: i16 = 3;
// Core: primitives/transaction.h TRANSACTION_ASSET_UNLOCK, ASSET_UNLOCK_* constants
const TRANSACTION_ASSET_UNLOCK: u16 = 9;
const ASSET_UNLOCK_QUORUM_INFO_SIZE: usize = 4 + 32 + 96;
const ASSET_UNLOCK_PAYLOAD_SIZE: usize = 1 + 8 + 4 + ASSET_UNLOCK_QUORUM_INFO_SIZE;
const ASSET_UNLOCK_STABLE_TXID_VERSION: u8 = 2;

/// A cursor over a byte slice with Core's primitive decoders.
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
        }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    /// A `Vec` capacity for `n` elements of at least `min_size` bytes each that are still to be
    /// read: never more than the remaining bytes can hold, and at most `MAX_PREALLOC` elements
    /// (an element is several times its on-disk minimum in memory), so a corrupt count cannot
    /// reserve gigabytes. The `Vec` grows as items really decode.
    pub fn capacity_for(&self, n: u64, min_size: usize) -> usize {
        usize::try_from(n)
            .unwrap_or(usize::MAX)
            .min(self.remaining().len() / min_size)
            .min(MAX_PREALLOC)
    }

    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).ok_or_else(|| {
            Error::malformed("serialization", format!("need {n} bytes at offset {}", self.pos))
        })?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("length checked"))
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u32_le(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn i64_le(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    /// Core's `ReadCompactSize` (canonical encodings only, at most `MAX_SIZE`).
    // Core: serialize.h ReadCompactSize
    pub fn compact_size(&mut self) -> Result<u64> {
        let first = self.u8()?;
        let (n, min) = match first {
            0..=252 => return Ok(u64::from(first)),
            253 => (u64::from(u16::from_le_bytes(self.array()?)), 253),
            254 => (u64::from(self.u32_le()?), 0x1_0000),
            255 => (u64::from_le_bytes(self.array()?), 0x1_0000_0000),
        };
        if n < min {
            return Err(Error::malformed("compact size", "non-canonical encoding"));
        }
        if n > MAX_SIZE {
            return Err(Error::malformed("compact size", format!("{n} exceeds MAX_SIZE")));
        }
        Ok(n)
    }

    /// A compact-size length followed by that many bytes.
    pub fn var_bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.compact_size()?;
        self.bytes(n as usize)
    }

    fn varint_with(
        &mut self,
        read: fn(&mut &[u8]) -> Result<u64, dashd_store::CodecError>,
    ) -> Result<u64> {
        let mut rest = self.remaining();
        let before = rest.len();
        let n = read(&mut rest).map_err(|e| Error::malformed("VARINT", e.to_string()))?;
        self.pos += before - rest.len();
        Ok(n)
    }

    /// Core's `VARINT` into a `uint64_t` (amounts).
    pub fn varint(&mut self) -> Result<u64> {
        self.varint_with(dashd_store::codec::read_varint_u64)
    }

    /// Core's `VARINT` into an `unsigned int` (heights, script sizes), with Core's overflow
    /// rejection for 32 bits.
    pub fn varint_u32(&mut self) -> Result<u32> {
        self.varint_with(|r| dashd_store::codec::read_varint_u32(r).map(u64::from))
            .map(|n| n as u32)
    }
}

/// A transaction output reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutPoint {
    pub txid: Hash256,
    pub vout: u32,
}

/// A transaction output, borrowing its script from the block bytes.
#[derive(Debug, Clone, Copy)]
pub struct TxOut<'a> {
    pub value: i64,
    pub script: &'a [u8],
}

/// A parsed transaction. Inputs keep only their outpoints.
#[derive(Debug, Clone)]
pub struct Tx<'a> {
    pub txid: Hash256,
    /// The low 16 bits of the 32-bit version field, as Core's `int16_t nVersion`.
    pub version: i16,
    /// The high 16 bits of the version field (`nType`).
    pub tx_type: u16,
    pub inputs: Vec<OutPoint>,
    pub outputs: Vec<TxOut<'a>>,
    pub payload: Option<&'a [u8]>,
}

/// Reads one transaction. The txid is SHA256d over the transaction's bytes as stored, except for
/// asset unlocks with a stable txid, which Core hashes with the quorum signing info zeroed.
///
/// The version field is split as Core's nVersion/nType serialization does (see PLAN.md §7):
/// `nVersion` is the sign-extended low 16 bits and the payload is present only when
/// `nVersion >= 3 && nType != 0`.
// Core: primitives/transaction.h CMutableTransaction::SERIALIZE_METHODS, ComputeHash
pub fn read_tx<'a>(c: &mut Cursor<'a>) -> Result<Tx<'a>> {
    let start = c.position();
    let raw_version = c.u32_le()?;
    let version = (raw_version & 0xffff) as u16 as i16;
    let tx_type = (raw_version >> 16) as u16;
    let n_in = c.compact_size()?;
    // An input is at least 41 bytes (outpoint, empty script, sequence), an output 9.
    let mut inputs = Vec::with_capacity(c.capacity_for(n_in, 41));
    for _ in 0..n_in {
        let txid = c.array::<32>()?;
        let vout = c.u32_le()?;
        c.var_bytes()?; // scriptSig
        c.u32_le()?; // nSequence
        inputs.push(OutPoint {
            txid,
            vout,
        });
    }
    let n_out = c.compact_size()?;
    let mut outputs = Vec::with_capacity(c.capacity_for(n_out, 9));
    for _ in 0..n_out {
        let value = c.i64_le()?;
        let script = c.var_bytes()?;
        outputs.push(TxOut {
            value,
            script,
        });
    }
    c.u32_le()?; // nLockTime
    let payload = if version >= SPECIAL_VERSION && tx_type != 0 {
        Some(c.var_bytes()?)
    } else {
        None
    };
    let raw = &c.data[start..c.position()];
    let stable_unlock = tx_type == TRANSACTION_ASSET_UNLOCK
        && version >= SPECIAL_VERSION
        && payload.is_some_and(|p| {
            p.len() == ASSET_UNLOCK_PAYLOAD_SIZE && p[0] >= ASSET_UNLOCK_STABLE_TXID_VERSION
        });
    let txid = if stable_unlock {
        // The payload ends the serialization, so its quorum info is the last bytes.
        let (head, _) = raw.split_at(raw.len() - ASSET_UNLOCK_QUORUM_INFO_SIZE);
        let mut engine = sha256d::Hash::engine();
        engine.input(head);
        engine.input(&[0u8; ASSET_UNLOCK_QUORUM_INFO_SIZE]);
        sha256d::Hash::from_engine(engine).to_byte_array()
    } else {
        sha256d::Hash::hash(raw).to_byte_array()
    };
    Ok(Tx {
        txid,
        version,
        tx_type,
        inputs,
        outputs,
        payload,
    })
}

/// A parsed block.
pub struct Block<'a> {
    pub header: &'a [u8; 80],
    pub txs: Vec<Tx<'a>>,
}

/// The X11 hash of an 80-byte block header (the block hash).
pub fn block_hash(header: &[u8]) -> Hash256 {
    rs_x11_hash::get_x11_hash(header)
}

/// The merkle root of a block's transaction ids (the last id of an odd level is paired with
/// itself).
// Core: consensus/merkle.cpp BlockMerkleRoot / ComputeMerkleRoot
pub fn merkle_root(txids: impl IntoIterator<Item = Hash256>) -> Hash256 {
    let mut level: Vec<Hash256> = txids.into_iter().collect();
    if level.is_empty() {
        return [0; 32];
    }
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().expect("non-empty"));
        }
        level = level
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let mut engine = sha256d::Hash::engine();
                engine.input(&pair[0]);
                engine.input(&pair[1]);
                sha256d::Hash::from_engine(engine).to_byte_array()
            })
            .collect();
    }
    level[0]
}

impl Block<'_> {
    /// Whether the header commits to these transactions (its merkle root matches): a copy of a
    /// block whose header is intact but whose body was overwritten or torn fails this. (Core's
    /// duplicate-transaction mutation check, CVE-2012-2459, is not repeated: stored blocks were
    /// accepted by Core.)
    pub fn merkle_root_matches(&self) -> bool {
        merkle_root(self.txs.iter().map(|tx| tx.txid))[..] == self.header[36..68]
    }
}

/// Reads a whole block; fails on trailing bytes.
pub fn read_block(bytes: &[u8]) -> Result<Block<'_>> {
    let (block, len) = read_block_prefix(bytes)?;
    if len < bytes.len() {
        return Err(Error::malformed("block", format!("{} trailing bytes", bytes.len() - len)));
    }
    Ok(block)
}

/// Reads a block from the start of `bytes`, as Core's `ReadBlockFromDisk` does: bytes after the
/// block are ignored. Returns the block and its length.
pub fn read_block_prefix(bytes: &[u8]) -> Result<(Block<'_>, usize)> {
    let mut c = Cursor::new(bytes);
    let header: &[u8; 80] = c.bytes(80)?.try_into().expect("80 bytes");
    let n = c.compact_size()?;
    // A transaction is at least 10 bytes (version, two counts, lock time).
    let mut txs = Vec::with_capacity(c.capacity_for(n, 10));
    for _ in 0..n {
        txs.push(read_tx(&mut c)?);
    }
    Ok((
        Block {
            header,
            txs,
        },
        c.position(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash_to_hex;

    /// Dash mainnet genesis block (Core `chainparams.cpp` `CreateGenesisBlock`), as it appears in
    /// `blk00000.dat` of the reference oracle.
    const MAINNET_GENESIS: &str = "010000000000000000000000000000000000000000000000000000000000000000000000c762a6567f3cc092f0684bb62b7e00a84890b990f07cc71a6bb58d64b98e02e0022ddb52f0ff0f1ec23fb9010101000000010000000000000000000000000000000000000000000000000000000000000000ffffffff6204ffff001d01044c5957697265642030392f4a616e2f3230313420546865204772616e64204578706572696d656e7420476f6573204c6976653a204f76657273746f636b2e636f6d204973204e6f7720416363657074696e6720426974636f696e73ffffffff0100f2052a010000004341040184710fa689ad5023690c80f3a49c8f13f8d45b8c857fbcbc8bc4a8e4d3eb4b10f4d4604fa08dce601aaf0f470216fe1b51850b4acf21b179c45070ac7b03a9ac00000000";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn mainnet_genesis() {
        let bytes = unhex(MAINNET_GENESIS);
        let block = read_block(&bytes).unwrap();
        assert_eq!(
            hash_to_hex(&block_hash(block.header)),
            "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6"
        );
        assert_eq!(block.txs.len(), 1);
        let cb = &block.txs[0];
        assert_eq!(
            hash_to_hex(&cb.txid),
            "e0028eb9648db56b1ac77cf090b99048a8007e2bb64b68f092c03c7f56a662c7"
        );
        assert_eq!(cb.outputs[0].value, 50 * 100_000_000);
        assert_eq!(cb.payload, None);
    }

    /// A corrupt count reserves no more than the remaining bytes can hold.
    #[test]
    fn counts_do_not_reserve_beyond_the_input() {
        let c = Cursor::new(&[0u8; 100]);
        assert_eq!(c.capacity_for(0x0200_0000, 41), 2);
        assert_eq!(c.capacity_for(u64::MAX, 1), 100);
        assert_eq!(c.capacity_for(3, 1), 3);
        // A transaction claiming 32M inputs fails on its missing bytes, not on allocation.
        let mut tx = 1u32.to_le_bytes().to_vec();
        tx.extend([0xfe, 0x00, 0x00, 0x00, 0x02]);
        assert!(read_tx(&mut Cursor::new(&tx)).is_err());
    }

    #[test]
    fn compact_size_is_canonical() {
        assert_eq!(Cursor::new(&[0xfc]).compact_size().unwrap(), 0xfc);
        assert_eq!(Cursor::new(&[0xfd, 0xfd, 0x00]).compact_size().unwrap(), 0xfd);
        assert!(Cursor::new(&[0xfd, 0x10, 0x00]).compact_size().is_err());
        assert!(Cursor::new(&[0xfe, 0x00, 0x00, 0x00, 0x03]).compact_size().is_err());
    }
}
