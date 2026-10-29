//! The evo rows (A1 §2.9; CH7 §6.1): their column families, keys and values, and
//! [`EvoDelta`] ⇄ rows.
//!
//! Every value is the node's own encoding (the store is ours, not Core's evodb; CH6b translates),
//! so a value that does not decode is a store fault, never a verdict (CH7 D5). The decoders are
//! strict: each value has one encoding, and nothing may follow it.

use std::sync::Arc;

use dash_dml::codec::{DmlRecord, NetInfoValidity, decode_record, encode_diff_record};
use dash_dml::creditpool::{CreditBlockData, CreditPool, PoolError, PoolState, UnlockDataReason};
use dash_dml::mnhf::EhfSignals;
use dash_dml::store::MinedErase;
use dash_dml::{EvoDelta, MnList, MnListDiffRecord, StoreError};
use dash_evo::llmq::{LlmqCommitment, QuorumSnapshot};
use dash_primitives::{BlockHash, LlmqType, QuorumHash};
use dash_types::codec::BaseCodec;
use dash_types::{CompactSize, Numeric};
use dashd_store::Cf;

use crate::store::CHAINSTATE_SCHEMA;

/// A by-hash evo row's kind (A1 §2.9): the rows a block hash names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EvoCf {
    /// `mn_diff`: the block's DML diff record.
    MnDiff,
    /// `mn_snapshot`: the list after the block, every 576 blocks and at the first DIP3 block.
    MnSnapshot,
    /// `credit_pool`: the pool after the block, or the error its construction throws (EV3 D8).
    CreditPool,
    /// The block's credit data (EV3 D9), in `credit_pool` under the hash and [`CREDIT_DATA_TAG`]
    /// (CH7 D10).
    CreditData,
    /// `mnhf`: the block's MNHF signals row (EV3 D15).
    Mnhf,
}

/// The suffix that sets a credit-data row apart from the pool row of the same block.
pub const CREDIT_DATA_TAG: u8 = b'd';

/// A by-hash key: the block hash, and [`CREDIT_DATA_TAG`] for credit data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HashKey {
    bytes: [u8; 33],
    len: usize,
}

impl HashKey {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl EvoCf {
    /// Every kind.
    pub const ALL: [EvoCf; 5] =
        [EvoCf::MnDiff, EvoCf::MnSnapshot, EvoCf::CreditPool, EvoCf::CreditData, EvoCf::Mnhf];

    /// The column family's name in [`CHAINSTATE_SCHEMA`].
    pub fn name(self) -> &'static str {
        match self {
            EvoCf::MnDiff => "mn_diff",
            EvoCf::MnSnapshot => "mn_snapshot",
            EvoCf::CreditPool | EvoCf::CreditData => "credit_pool",
            EvoCf::Mnhf => "mnhf",
        }
    }

    pub fn cf(self) -> Cf {
        cf(self.name())
    }

    /// The row's key for `block`.
    pub fn key(self, block: &BlockHash) -> HashKey {
        let mut bytes = [0; 33];
        bytes[..32].copy_from_slice(block.as_bytes());
        let len = if self == EvoCf::CreditData {
            bytes[32] = CREDIT_DATA_TAG;
            33
        } else {
            32
        };
        HashKey {
            bytes,
            len,
        }
    }
}

pub(crate) fn cf(name: &str) -> Cf {
    CHAINSTATE_SCHEMA.cf(name).expect("a chainstate column family")
}

/// The two mined-commitment column families, as the first byte of a [`MinedKey`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum MinedCf {
    /// `llmq_mined` (Core's `q_mc`): `u8 type ‖ quorumHash[32]`.
    Mined = 0,
    /// `llmq_mined_h` (Core's `q_mcih` and `q_mcihi`): `u8 type ‖ u8 family ‖ BE16 qIdx ‖
    /// BE32(!minedHeight)`.
    Index = 1,
}

impl MinedCf {
    pub fn cf(self) -> Cf {
        cf(match self {
            MinedCf::Mined => "llmq_mined",
            MinedCf::Index => "llmq_mined_h",
        })
    }

    fn from_tag(tag: u8) -> Self {
        match tag {
            0 => MinedCf::Mined,
            1 => MinedCf::Index,
            _ => unreachable!("only this module makes mined keys"),
        }
    }
}

/// A key of the evo index's ordered map (CH7 D22): the [`MinedCf`] tag, then the column family's
/// key. The tag keeps the two families apart in one map, and the index rows of one type and family
/// adjacent and newest first, as a RocksDB iterator reads them.
pub type MinedKey = Vec<u8>;

/// The column family and the key within it of a [`MinedKey`].
pub fn split_mined_key(key: &[u8]) -> (MinedCf, &[u8]) {
    (MinedCf::from_tag(key[0]), &key[1..])
}

/// `llmq_mined`'s key: `u8 type ‖ quorumHash`.
pub fn mined_key(llmq_type: LlmqType, quorum_hash: &QuorumHash) -> MinedKey {
    let mut key = Vec::with_capacity(34);
    key.push(MinedCf::Mined as u8);
    key.push(llmq_type.to_base());
    key.extend_from_slice(quorum_hash.as_bytes());
    key
}

/// The height-index family byte: 0 for the non-rotated index (Core's `q_mcih`), 1 for the
/// indexed one (`q_mcihi`), followed by the quorum index (0 for the non-rotated index).
fn family_bytes(rotation_index: Option<i16>) -> [u8; 3] {
    match rotation_index {
        None => [0, 0, 0],
        Some(index) => {
            let [hi, lo] = index.to_be_bytes();
            [1, hi, lo]
        }
    }
}

/// The height index's prefix for one type and family: `tag ‖ u8 type ‖ u8 family ‖ BE16 qIdx`.
pub fn index_prefix(llmq_type: LlmqType, rotation_index: Option<i16>) -> [u8; 5] {
    let [f, hi, lo] = family_bytes(rotation_index);
    [MinedCf::Index as u8, llmq_type.to_base(), f, hi, lo]
}

/// `llmq_mined_h`'s key: [`index_prefix`] then `BE32(!minedHeight)`, so a forward scan reads the
/// newest row first (Core's `BuildInversedHeightKey`, `llmq/blockprocessor.cpp` v24:308-320).
pub fn index_key(llmq_type: LlmqType, rotation_index: Option<i16>, mined_height: u32) -> MinedKey {
    let mut key = Vec::with_capacity(9);
    key.extend_from_slice(&index_prefix(llmq_type, rotation_index));
    key.extend_from_slice(&(!mined_height).to_be_bytes());
    key
}

/// The mined height of an `llmq_mined_h` key, which must be [`index_key`]'s length: a key read back
/// in any other shape is a store fault (D5), never a panic.
///
/// # Errors
///
/// [`StoreError::Read`] for a key of another length.
pub fn index_key_height(key: &[u8]) -> Result<u32, StoreError> {
    let tail: [u8; 4] = match key {
        [_, _, _, _, _, tail @ ..] => tail.try_into().map_err(|_| ()),
        _ => Err(()),
    }
    .map_err(|()| corrupt("llmq_mined_h key", format!("{} bytes", key.len())))?;
    Ok(!u32::from_be_bytes(tail))
}

/// A decoded `llmq_mined` value: the commitment and the block that mined it.
#[derive(Clone, Debug, PartialEq)]
pub struct MinedRow {
    pub commitment: Arc<LlmqCommitment>,
    pub block: BlockHash,
    pub height: u32,
}

/// A decoded `llmq_mined_h` value: the base's height, and the mining block, which lets a read
/// check that the row is on the chain it resolves against (CH7 §6.1; Core's value is the base
/// height alone, as Core reads only the active chain's rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexRow {
    pub base_height: u32,
    pub block: BlockHash,
}

/// The rows one delta writes, encoded: what [`EvoIndex`](super::EvoIndex) merges.
pub struct DeltaRows {
    /// By-hash rows. Each is block-derived: a rewrite must equal the row it meets (Core's
    /// `WriteDerived`, `evo/evodb.h` v24:159-205).
    pub by_hash: Vec<(EvoCf, BlockHash, Arc<[u8]>)>,
    /// `llmq_mined` puts, which are block-derived too (the "mined-commitment pair").
    pub mined: Vec<(MinedKey, Arc<[u8]>)>,
    /// `llmq_mined_h` puts: Core's plain `Write`.
    pub index: Vec<(MinedKey, Arc<[u8]>)>,
    /// The keys a disconnect erases: both rows of each [`MinedErase`].
    pub erased: Vec<MinedKey>,
    /// The evo best block the delta stages.
    pub evo_tip: BlockHash,
}

/// The evo rows' codec: the DML records need the chain's `NetInfoValidity` (EV2 §1.3); the other
/// rows are context-free.
#[derive(Clone)]
pub struct EvoCodec {
    net: Arc<dyn NetInfoValidity + Send + Sync>,
}

impl std::fmt::Debug for EvoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvoCodec").finish_non_exhaustive()
    }
}

fn shared(bytes: Vec<u8>) -> Arc<[u8]> {
    Arc::from(bytes.into_boxed_slice())
}

fn corrupt(what: &str, e: impl std::fmt::Display) -> StoreError {
    StoreError::read(format!("undecodable {what} row: {e}"))
}

impl EvoCodec {
    pub fn new(net: Arc<dyn NetInfoValidity + Send + Sync>) -> Self {
        Self {
            net,
        }
    }

    /// The rows `delta` writes, checked as `MemEvoStore::apply` checks them: one block, one height,
    /// a snapshot stamped with its own block, no base above its mining block; and an erase only of
    /// the block's own mined rows, in a delta that mines nothing. Unlike `MemEvoStore`, which
    /// defaults it to the delta's block, the node's delta must name its evo tip (D40): a
    /// disconnect's is its parent (`validation.cpp:2389`).
    ///
    /// # Errors
    ///
    /// [`StoreError::Inconsistent`] for a delta that disagrees with itself or a DML record the
    /// canonical codec refuses.
    pub fn rows(&self, delta: &EvoDelta) -> Result<DeltaRows, StoreError> {
        let block = delta.block;
        let height = delta.height;
        let mut by_hash = Vec::new();
        if let Some(dml) = &delta.dml {
            let h = i64::from(height);
            if dml.block != block || i64::from(dml.height) != h || i64::from(dml.record.height) != h
            {
                return Err(StoreError::inconsistent("DML delta for another block"));
            }
            let diff = encode_diff_record(&dml.record, &*self.net)
                .map_err(|e| StoreError::inconsistent(format!("unencodable mn_diff: {e}")))?;
            by_hash.push((EvoCf::MnDiff, block, shared(diff.as_bytes().to_vec())));
            if let Some(snapshot) = &dml.snapshot {
                if *snapshot.block_hash() != block || i64::from(snapshot.height()) != h {
                    return Err(StoreError::inconsistent("mn_snapshot of another block"));
                }
                let bytes =
                    dash_dml::codec::encode_snapshot_record(snapshot, &*self.net).map_err(|e| {
                        StoreError::inconsistent(format!("unencodable mn_snapshot: {e}"))
                    })?;
                by_hash.push((EvoCf::MnSnapshot, block, shared(bytes.as_bytes().to_vec())));
            }
        }
        if let Some(data) = &delta.credit_data {
            by_hash.push((EvoCf::CreditData, block, shared(encode_credit_data(data))));
        }
        if let Some(pool) = &delta.credit_pool {
            by_hash.push((EvoCf::CreditPool, block, shared(encode_pool(pool))));
        }
        if let Some(signals) = &delta.mnhf_signals {
            by_hash.push((EvoCf::Mnhf, block, shared(encode_mnhf(signals))));
        }
        let mut mined = Vec::with_capacity(delta.mined_commitments.len());
        let mut index = Vec::with_capacity(delta.mined_commitments.len());
        for record in &delta.mined_commitments {
            if record.base_height > height {
                return Err(StoreError::inconsistent("commitment base above its mining block"));
            }
            let value = encode_mined(&record.commitment, &block, height);
            mined.push((mined_key(record.llmq_type, &record.quorum_hash), shared(value)));
            index.push((
                index_key(record.llmq_type, record.rotation_index, height),
                shared(encode_index(record.base_height, &block)),
            ));
        }
        // A disconnect erases its own block's rows and mines nothing (CH7 §4.1.4).
        if !delta.erased_mined.is_empty() && !delta.mined_commitments.is_empty() {
            return Err(StoreError::inconsistent("a delta that both mines and erases"));
        }
        if delta.erased_mined.iter().any(|e| e.mined_height != height) {
            return Err(StoreError::inconsistent("an erase of another block's mined rows"));
        }
        let evo_tip =
            delta.evo_tip.ok_or_else(|| StoreError::inconsistent("a delta naming no evo tip"))?;
        let erased = delta.erased_mined.iter().flat_map(erased_keys).collect();
        Ok(DeltaRows {
            by_hash,
            mined,
            index,
            erased,
            evo_tip,
        })
    }

    /// An `mn_diff` value. Its read names the block by hash only, so the diff's height is not
    /// checked against it, as Core reads `DB_LIST_DIFF` unchecked (D41).
    ///
    /// # Errors
    ///
    /// [`StoreError::Read`] for bytes the canonical reader refuses or another record kind.
    pub fn decode_diff(&self, bytes: &[u8]) -> Result<Arc<MnListDiffRecord>, StoreError> {
        match decode_record(bytes, &*self.net).map_err(|e| corrupt("mn_diff", e))? {
            DmlRecord::Diff(record) => Ok(Arc::new(record)),
            DmlRecord::Snapshot(_) => Err(StoreError::read("a snapshot record in mn_diff")),
        }
    }

    /// An `mn_snapshot` value, which must be the list after `block`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Read`] for bytes the canonical reader refuses or another record kind, and
    /// [`StoreError::Inconsistent`] for the list of another block.
    pub fn decode_snapshot(
        &self,
        bytes: &[u8],
        block: &BlockHash,
    ) -> Result<Arc<MnList>, StoreError> {
        match decode_record(bytes, &*self.net).map_err(|e| corrupt("mn_snapshot", e))? {
            DmlRecord::Snapshot(list) if list.block_hash() == block => Ok(list),
            DmlRecord::Snapshot(_) => Err(StoreError::inconsistent("mn_snapshot of another block")),
            DmlRecord::Diff(_) => Err(StoreError::read("a diff record in mn_snapshot")),
        }
    }
}

/// Both keys a [`MinedErase`] names.
fn erased_keys(erase: &MinedErase) -> [MinedKey; 2] {
    [
        mined_key(erase.llmq_type, &erase.quorum_hash),
        index_key(erase.llmq_type, erase.rotation_index, erase.mined_height),
    ]
}

/// A strict reader over a value.
struct Reader<'a> {
    input: &'a [u8],
    what: &'static str,
}

impl<'a> Reader<'a> {
    fn new(input: &'a [u8], what: &'static str) -> Self {
        Self {
            input,
            what,
        }
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], StoreError> {
        let input: &'a [u8] = self.input;
        let (head, rest) =
            input.split_first_chunk::<N>().ok_or_else(|| corrupt(self.what, "end of data"))?;
        self.input = rest;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, StoreError> {
        Ok(self.bytes::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, StoreError> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }

    fn i32(&mut self) -> Result<i32, StoreError> {
        Ok(i32::from_le_bytes(self.bytes()?))
    }

    fn i64(&mut self) -> Result<i64, StoreError> {
        Ok(i64::from_le_bytes(self.bytes()?))
    }

    fn hash(&mut self) -> Result<BlockHash, StoreError> {
        Ok(BlockHash::from(self.bytes::<32>()?))
    }

    /// A compact size no larger than the bytes left, so no length allocates beyond the value.
    fn len(&mut self) -> Result<usize, StoreError> {
        CompactSize::decode_size(&mut self.input)
            .and_then(|n| n.into_len(self.input.len()))
            .map_err(|e| corrupt(self.what, e))
    }

    fn end<T>(self, value: T) -> Result<T, StoreError> {
        if self.input.is_empty() {
            Ok(value)
        } else {
            Err(corrupt(self.what, "trailing bytes"))
        }
    }
}

fn write_len(n: usize, out: &mut Vec<u8>) {
    CompactSize::from(n).encode(out);
}

/// `llmq_mined`'s value: the commitment (Core's serialization, whose BLS scheme its version
/// selects), the mining block's hash and height. Core's value is the commitment and the hash
/// (`llmq/blockprocessor.cpp` v24:471-476); the height lets a read check the mining block's ancestry.
pub fn encode_mined(commitment: &LlmqCommitment, block: &BlockHash, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    commitment.encode(&mut out);
    out.extend_from_slice(block.as_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out
}

pub fn decode_mined(bytes: &[u8]) -> Result<MinedRow, StoreError> {
    let mut input = bytes;
    let commitment = LlmqCommitment::decode(&mut input).map_err(|e| corrupt("llmq_mined", e))?;
    let mut r = Reader::new(input, "llmq_mined");
    let block = r.hash()?;
    let height = r.u32()?;
    r.end(MinedRow {
        commitment: Arc::new(commitment),
        block,
        height,
    })
}

/// `llmq_mined_h`'s value: the base height (Core's value), then the mining block's hash.
pub fn encode_index(base_height: u32, block: &BlockHash) -> Vec<u8> {
    let mut out = Vec::with_capacity(36);
    out.extend_from_slice(&base_height.to_le_bytes());
    out.extend_from_slice(block.as_bytes());
    out
}

pub fn decode_index(bytes: &[u8]) -> Result<IndexRow, StoreError> {
    let mut r = Reader::new(bytes, "llmq_mined_h");
    let base_height = r.u32()?;
    let block = r.hash()?;
    r.end(IndexRow {
        base_height,
        block,
    })
}

/// The credit data row: the CbTx balance and the unlocked amount, little-endian `int64`s.
pub fn encode_credit_data(data: &CreditBlockData) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&data.balance.to_le_bytes());
    out.extend_from_slice(&data.unlocked.to_le_bytes());
    out
}

pub fn decode_credit_data(bytes: &[u8]) -> Result<CreditBlockData, StoreError> {
    let mut r = Reader::new(bytes, "credit data");
    let balance = r.i64()?;
    let unlocked = r.i64()?;
    r.end(CreditBlockData {
        balance,
        unlocked,
    })
}

/// The pool row (BL-259): a tag, then Core's `CCreditPool` serialization for a pool, or the
/// error's operand. Every [`PoolError`] is stored, so a child reads the exception Core would throw
/// (EV3 D8).
pub fn encode_pool(state: &PoolState) -> Vec<u8> {
    let mut out = Vec::new();
    match state {
        Ok(pool) => {
            out.push(0);
            pool.encode(&mut out);
        }
        Err(PoolError::PrevLocked) => out.push(1),
        Err(PoolError::PrevIndexes) => out.push(2),
        Err(PoolError::IndexDuplicated) => out.push(3),
        Err(PoolError::NegativeLimit(limit)) => {
            out.push(4);
            out.extend_from_slice(&limit.to_le_bytes());
        }
        Err(PoolError::UnlockData(reason)) => {
            out.push(5);
            out.push(match reason {
                UnlockDataReason::Payload => 0,
                UnlockDataReason::TxoutOutOfRange => 1,
                UnlockDataReason::Overflow => 2,
            });
        }
    }
    out
}

pub fn decode_pool(bytes: &[u8]) -> Result<PoolState, StoreError> {
    let mut r = Reader::new(bytes, "credit pool");
    let state = match r.u8()? {
        0 => Ok(CreditPool::decode(&mut r.input).map_err(|e| corrupt("credit pool", e))?),
        1 => Err(PoolError::PrevLocked),
        2 => Err(PoolError::PrevIndexes),
        3 => Err(PoolError::IndexDuplicated),
        4 => Err(PoolError::NegativeLimit(r.i64()?)),
        5 => Err(PoolError::UnlockData(match r.u8()? {
            0 => UnlockDataReason::Payload,
            1 => UnlockDataReason::TxoutOutOfRange,
            2 => UnlockDataReason::Overflow,
            n => return Err(corrupt("credit pool", format!("unlock reason {n}"))),
        })),
        n => return Err(corrupt("credit pool", format!("tag {n}"))),
    };
    r.end(state)
}

/// The MNHF row: Core's `std::map<uint8_t, int>` serialization (a compact size, then each bit and
/// its height in bit order).
pub fn encode_mnhf(signals: &EhfSignals) -> Vec<u8> {
    let mut out = Vec::new();
    write_len(signals.len(), &mut out);
    for (bit, height) in signals.iter() {
        out.push(bit);
        out.extend_from_slice(&height.to_le_bytes());
    }
    out
}

pub fn decode_mnhf(bytes: &[u8]) -> Result<EhfSignals, StoreError> {
    let bits = dash_consensus::deployments::VERSIONBITS_NUM_BITS;
    let mut r = Reader::new(bytes, "mnhf");
    let n = r.len()?;
    let mut signals = EhfSignals::new();
    let mut last = None;
    for _ in 0..n {
        let bit = r.u8()?;
        // A map's keys are unique and in order; only bits below VERSIONBITS_NUM_BITS signal.
        if i32::from(bit) >= bits || last.is_some_and(|l| l >= bit) {
            return Err(corrupt("mnhf", format!("bit {bit} out of order or range")));
        }
        last = Some(bit);
        signals.insert(bit, r.i32()?);
    }
    r.end(signals)
}

/// `llmq_snapshot`'s value: Core's `CQuorumSnapshot` serialization (`llmq/snapshot.h` v24:61-71):
/// the skip mode, the member bits as a fixed bit set after their count, and the skip list. The
/// padding bits of the last byte are zero.
pub fn encode_quorum_snapshot(snapshot: &QuorumSnapshot) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&snapshot.skip_list_mode.to_le_bytes());
    let members = &snapshot.active_quorum_members;
    write_len(members.len(), &mut out);
    let mut bits = vec![0u8; members.len().div_ceil(8)];
    for (i, _) in members.iter().enumerate().filter(|(_, m)| **m) {
        bits[i / 8] |= 1 << (i % 8);
    }
    out.extend_from_slice(&bits);
    write_len(snapshot.skip_list.len(), &mut out);
    for entry in &snapshot.skip_list {
        out.extend_from_slice(&entry.to_le_bytes());
    }
    out
}

pub fn decode_quorum_snapshot(bytes: &[u8]) -> Result<QuorumSnapshot, StoreError> {
    let mut r = Reader::new(bytes, "llmq_snapshot");
    let skip_list_mode = r.i32()?;
    let count = CompactSize::decode_size(&mut r.input)
        .and_then(|n| n.into_len(r.input.len().saturating_mul(8)))
        .map_err(|e| corrupt("llmq_snapshot", e))?;
    // The limit keeps the bit set within the bytes left.
    let nbytes = count.div_ceil(8);
    let (bits, rest) = r.input.split_at(nbytes);
    r.input = rest;
    let active_quorum_members: Vec<bool> =
        (0..count).map(|i| bits[i / 8] & (1 << (i % 8)) != 0).collect();
    if count % 8 != 0 && bits[nbytes - 1] >> (count % 8) != 0 {
        return Err(corrupt("llmq_snapshot", "padding bits set"));
    }
    let n = r.len()?;
    let skip_list = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>, _>>()?;
    r.end(QuorumSnapshot {
        active_quorum_members,
        skip_list_mode,
        skip_list,
    })
}

/// `llmq_snapshot`'s key: `u8 type ‖ cycleBaseHash`.
pub fn quorum_snapshot_key(llmq_type: LlmqType, cycle_base: &BlockHash) -> [u8; 33] {
    let mut key = [0; 33];
    key[0] = llmq_type.to_base();
    key[1..].copy_from_slice(cycle_base.as_bytes());
    key
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
