//! The snapshot's encoding (`EvoSnapshot::Serialize`, `snapshot.h` v24:566-663) and its bounded
//! decoder (`Unserialize`, v24:409-449, 582-643, 665-718), field for field.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use dash_consensus::primitives::{write_commitment, write_compact_size};
use dash_num::Hash256;
use dash_params::LlmqType;
use dash_pkc::bls::{BlsEager, BlsSchemeSource, BlsWire};
use dash_primitives::{
    BlockHash, Commitment, DynBitset, QuorumHash, QuorumVvecHash, commitment_bls_scheme,
};
use dash_types::Numeric;

use super::{
    EVO_SNAPSHOT_VERSION, EvoSnapshot, HistoricalMnListDiff, MAX_MNHF_SIGNALS, MAX_MODIFIERS,
    MAX_QUORUM_SIZE, MAX_RANGES, MAX_SKIPLIST_ENTRIES, MinedQuorumCommitment, QuorumModifier,
    QuorumSnapshot, QuorumSnapshotData, QuorumSnapshotEntry, ROTATION_CYCLES,
    max_historical_mn_lists, max_historical_mn_operations, snapshot_commitment_count,
    snapshot_llmq_params,
};
use crate::codec::{
    CodecError, MAX_MNS, NetInfoValidity, Reader, decode_canonical_diff, decode_canonical_list,
    encode_canonical_diff, encode_canonical_list_unvalidated,
};
use crate::creditpool::{CreditPool, RangesSet};

/// `LEGACY_BLS_INDEXED_QUORUM_VERSION`.
const LEGACY_BLS_INDEXED_QUORUM_VERSION: u16 = 2;
/// `BASIC_BLS_INDEXED_QUORUM_VERSION`.
const BASIC_BLS_INDEXED_QUORUM_VERSION: u16 = 4;

/// Whether a commitment of `version` carries `quorumIndex`.
pub(super) fn is_indexed(version: u16) -> bool {
    matches!(version, LEGACY_BLS_INDEXED_QUORUM_VERSION | BASIC_BLS_INDEXED_QUORUM_VERSION)
}

// ----------------------------------------------------------------------------------------------
// The canonical orders (`IsCanonicallyBefore`, `snapshot.h` v24:526-550): one sort key per
// collection, shared by the encoder and both order checks. `uint256`'s `operator<` compares the
// bytes in storage order.

pub(super) fn commitment_key(c: &MinedQuorumCommitment) -> ([u8; 32], [u8; 32]) {
    (*c.quorum_base_block_hash.as_bytes(), *c.mined_block_hash.as_bytes())
}

pub(super) fn rotation_key(e: &QuorumSnapshotEntry) -> [u8; 32] {
    *e.cycle_base_block_hash.as_bytes()
}

/// Historical diffs descend by `(height, block_hash)`: the chain is replayed newest first.
pub(super) fn history_key(h: &HistoricalMnListDiff) -> Reverse<(i32, [u8; 32])> {
    Reverse((h.height, *h.block_hash.as_bytes()))
}

pub(super) fn modifier_key(m: &QuorumModifier) -> (u8, [u8; 32]) {
    (m.llmq_type.to_base(), *m.work_block_hash.as_bytes())
}

pub(super) fn quorum_key(q: &QuorumSnapshotData) -> u8 {
    q.llmq_type.to_base()
}

/// `IsCanonicallySorted`: strictly ascending under `key` (equal neighbours are not sorted).
pub(super) fn is_strictly_sorted_by<T, K: Ord>(values: &[T], key: impl Fn(&T) -> K) -> bool {
    values.windows(2).all(|w| key(&w[0]) < key(&w[1]))
}

/// `CanonicallySortedCopy`.
pub(super) fn sorted_by<T, K: Ord>(values: &[T], key: impl Fn(&T) -> K) -> Vec<&T> {
    let mut sorted: Vec<&T> = values.iter().collect();
    sorted.sort_by_key(|v| key(v));
    sorted
}

// ----------------------------------------------------------------------------------------------
// Encoders.

/// `WriteFixedBitSet(s, bits, bits.size())`: the bits LSB-first, padded with zeros.
fn write_fixed_bitset(bits: &[bool], out: &mut Vec<u8>) {
    for chunk in bits.chunks(8) {
        out.push(chunk.iter().enumerate().fold(0, |b, (i, &set)| b | (u8::from(set) << i)));
    }
}

pub(super) fn write_mined_commitment(c: &MinedQuorumCommitment, out: &mut Vec<u8>) {
    out.extend_from_slice(c.quorum_base_block_hash.as_bytes());
    out.extend_from_slice(c.work_block_hash.as_bytes());
    write_commitment(&c.commitment, out);
    out.extend_from_slice(c.mined_block_hash.as_bytes());
}

/// `WriteRotationSnapshot`.
// Core: evo/snapshot.h:574 WriteRotationSnapshot
pub(super) fn write_rotation_snapshot(entry: &QuorumSnapshotEntry, out: &mut Vec<u8>) {
    out.extend_from_slice(entry.cycle_base_block_hash.as_bytes());
    out.extend_from_slice(entry.work_block_hash.as_bytes());
    out.extend_from_slice(&entry.snapshot.skip_list_mode.to_le_bytes());
    let members = &entry.snapshot.active_quorum_members;
    write_compact_size(members.len() as u64, out);
    write_fixed_bitset(members, out);
    write_compact_size(entry.snapshot.skip_list.len() as u64, out);
    for v in &entry.snapshot.skip_list {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

// Core: evo/snapshot.h:605 QuorumSnapshotData::Serialize
fn write_quorum_data(data: &QuorumSnapshotData, out: &mut Vec<u8>) {
    out.push(data.llmq_type.to_base());
    out.push(u8::from(data.rotation_enabled));
    for commitments in [&data.active_commitments, &data.safety_commitments] {
        write_compact_size(commitments.len() as u64, out);
        for c in sorted_by(commitments, commitment_key) {
            write_mined_commitment(c, out);
        }
    }
    write_compact_size(data.rotation_snapshots.len() as u64, out);
    for entry in sorted_by(&data.rotation_snapshots, rotation_key) {
        write_rotation_snapshot(entry, out);
    }
}

impl EvoSnapshot {
    /// `Serialize` at `SER_DISK`: the collections in their canonical orders, whatever order they
    /// are held in. Like Core's, it validates nothing; [`EvoSnapshot::hash`] validates first.
    // Core: evo/snapshot.h:646 EvoSnapshot::Serialize
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(self.base_block_hash.as_bytes());
        encode_canonical_list_unvalidated(&self.mn_list, out);
        write_compact_size(self.quorums.len() as u64, out);
        for data in sorted_by(&self.quorums, quorum_key) {
            write_quorum_data(data, out);
        }
        write_compact_size(self.historical_mn_list_diffs.len() as u64, out);
        for entry in sorted_by(&self.historical_mn_list_diffs, history_key) {
            out.extend_from_slice(entry.previous_block_hash.as_bytes());
            out.extend_from_slice(entry.block_hash.as_bytes());
            out.extend_from_slice(&entry.height.to_le_bytes());
            out.extend_from_slice(&entry.total_registered_count.to_le_bytes());
            out.extend_from_slice(entry.canonical_list_hash.as_bytes());
            encode_canonical_diff(&entry.diff, out);
        }
        write_compact_size(self.quorum_modifiers.len() as u64, out);
        for m in sorted_by(&self.quorum_modifiers, modifier_key) {
            out.push(m.llmq_type.to_base());
            out.extend_from_slice(m.work_block_hash.as_bytes());
            out.extend_from_slice(m.modifier.as_bytes());
        }
        self.credit_pool.encode(out);
        write_compact_size(self.mnhf_signals.len() as u64, out);
        for (bit, height) in &self.mnhf_signals {
            out.push(*bit);
            out.extend_from_slice(&height.to_le_bytes());
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Decoders.

/// `ReadBoundedCompactSize`: a range-checked CompactSize no greater than `limit`.
// Core: evo/snapshot.h:90 ReadBoundedCompactSize
fn read_bounded(r: &mut Reader<'_, '_>, limit: usize, field: &str) -> Result<usize, CodecError> {
    let n = r.compact()?;
    if n > limit as u64 {
        return Err(CodecError::new(format!("oversized evo snapshot {field}")));
    }
    Ok(n as usize)
}

fn hash(r: &mut Reader<'_, '_>) -> Result<[u8; 32], CodecError> {
    r.bytes::<32>()
}

/// `ReadFixedBitSet(s, vec, size)` (`serialize.h` rc.4:451-477): the declared size bounded by
/// the bytes left before anything is read, and no padding bit set.
fn read_fixed_bitset(r: &mut Reader<'_, '_>, size: usize) -> Result<Vec<u8>, CodecError> {
    let nbytes = size.div_ceil(8);
    if nbytes > r.remaining() {
        return Err(CodecError::new("ReadFixedBitSet(): declared size exceeds remaining bytes"));
    }
    let bytes = r.slice(nbytes)?.to_vec();
    if nbytes * 8 != size && bytes[nbytes - 1] & !(0xffu8 >> (nbytes * 8 - size)) != 0 {
        return Err(CodecError::new("Out-of-range bits set"));
    }
    Ok(bytes)
}

fn bits_of(bytes: &[u8], size: usize) -> Vec<bool> {
    (0..size).map(|p| bytes[p / 8] & (1 << (p % 8)) != 0).collect()
}

/// One BLS field of a commitment as `CBLSWrapper::Unserialize` reads it under `scheme`
/// (`malleable BLS object` where the bytes are canonical in neither scheme), with the bytes read.
fn read_bls<K: BlsWire<N>, const N: usize>(
    r: &mut Reader<'_, '_>,
    commitment_version: u16,
) -> Result<(BlsEager<K, N>, [u8; N]), CodecError> {
    let bytes = r.bytes::<N>()?;
    let source = BlsSchemeSource::Version(commitment_bls_scheme(commitment_version));
    let value = BlsEager::decode(&bytes, source).map_err(|e| CodecError::new(e.to_string()))?;
    Ok((value, bytes))
}

/// `ReadMinedQuorumCommitment`: the commitment with both bitset sizes bounded before they are
/// read, and its BLS fields required to re-encode to the bytes they were read from.
// Core: evo/snapshot.h:409 ReadMinedQuorumCommitment
pub(super) fn read_mined_commitment(
    r: &mut Reader<'_, '_>,
) -> Result<MinedQuorumCommitment, CodecError> {
    let quorum_base_block_hash = BlockHash::from(hash(r)?);
    let work_block_hash = BlockHash::from(hash(r)?);
    let version = r.u16()?;
    let llmq_type = LlmqType::from_base(r.u8()?);
    let quorum_hash = QuorumHash::from(hash(r)?);
    let quorum_index = if is_indexed(version) {
        Some(r.u16()? as i16)
    } else {
        None
    };
    let signers_size = read_bounded(r, MAX_QUORUM_SIZE, "commitment signers")?;
    if signers_size == 0 {
        return Err(CodecError::new("empty evo snapshot commitment signers"));
    }
    let signers = read_fixed_bitset(r, signers_size)?;
    let valid_members_size = read_bounded(r, MAX_QUORUM_SIZE, "commitment valid members")?;
    if valid_members_size != signers_size {
        return Err(CodecError::new("inconsistent evo snapshot commitment bitset sizes"));
    }
    let valid_members = read_fixed_bitset(r, valid_members_size)?;
    let (quorum_public_key, pk_bytes) = read_bls(r, version)?;
    let quorum_vvec_hash = QuorumVvecHash::from(hash(r)?);
    let (quorum_sig, sig_bytes) = read_bls(r, version)?;
    let (members_sig, members_bytes) = read_bls(r, version)?;
    let mined_block_hash = BlockHash::from(hash(r)?);
    // Core compares the hash of the consumed BLS bytes with the hash of their re-encoding.
    let scheme = commitment_bls_scheme(version);
    if quorum_public_key.to_bytes(scheme) != pk_bytes
        || quorum_sig.to_bytes(scheme) != sig_bytes
        || members_sig.to_bytes(scheme) != members_bytes
    {
        return Err(CodecError::new("noncanonical evo snapshot commitment encoding"));
    }
    let bitset = |data: Vec<u8>| DynBitset {
        num_bits: signers_size as u64,
        data,
    };
    Ok(MinedQuorumCommitment {
        quorum_base_block_hash,
        work_block_hash,
        commitment: Commitment {
            version,
            llmq_type,
            quorum_hash,
            quorum_index,
            signers: bitset(signers),
            valid_members: bitset(valid_members),
            quorum_public_key,
            quorum_vvec_hash,
            quorum_sig,
            members_sig,
        },
        mined_block_hash,
    })
}

/// `ReadRotationSnapshot`: the bitset bounded by the masternode ceiling (it is sized to the
/// whole work-block list, not the quorum), the skip list by its own ceiling.
// Core: evo/snapshot.h:583 ReadRotationSnapshot
pub(super) fn read_rotation_snapshot(
    r: &mut Reader<'_, '_>,
) -> Result<QuorumSnapshotEntry, CodecError> {
    let cycle_base_block_hash = BlockHash::from(hash(r)?);
    let work_block_hash = BlockHash::from(hash(r)?);
    let skip_list_mode = r.i32()?;
    let bit_count = read_bounded(r, MAX_MNS, "rotation bitset")?;
    let active_quorum_members = bits_of(&read_fixed_bitset(r, bit_count)?, bit_count);
    let skip_count = read_bounded(r, MAX_SKIPLIST_ENTRIES, "rotation skip list")?;
    let skip_list = (0..skip_count).map(|_| r.i32()).collect::<Result<_, _>>()?;
    Ok(QuorumSnapshotEntry {
        cycle_base_block_hash,
        work_block_hash,
        snapshot: QuorumSnapshot {
            active_quorum_members,
            skip_list_mode,
            skip_list,
        },
    })
}

// Core: evo/snapshot.h:615 QuorumSnapshotData::Unserialize
fn read_quorum_data(r: &mut Reader<'_, '_>) -> Result<QuorumSnapshotData, CodecError> {
    let llmq_type = LlmqType::from_base(r.u8()?);
    let rotation_flag = r.u8()?;
    if rotation_flag > 1 {
        return Err(CodecError::new("noncanonical evo quorum rotation flag"));
    }
    let rotation_enabled = rotation_flag != 0;
    let params = snapshot_llmq_params(llmq_type)?;
    let total = snapshot_commitment_count(&params, rotation_enabled)?;
    let expected_active = usize::from(params.signing_active_quorum_count);
    let active_count = read_bounded(r, expected_active, "active commitments")?;
    let active_commitments =
        (0..active_count).map(|_| read_mined_commitment(r)).collect::<Result<_, _>>()?;
    let safety_count =
        read_bounded(r, total.saturating_sub(expected_active), "safety commitments")?;
    let safety_commitments =
        (0..safety_count).map(|_| read_mined_commitment(r)).collect::<Result<_, _>>()?;
    let snapshot_count = read_bounded(r, ROTATION_CYCLES, "rotation snapshots")?;
    let rotation_snapshots =
        (0..snapshot_count).map(|_| read_rotation_snapshot(r)).collect::<Result<_, _>>()?;
    Ok(QuorumSnapshotData {
        llmq_type,
        rotation_enabled,
        active_commitments,
        safety_commitments,
        rotation_snapshots,
    })
}

impl EvoSnapshot {
    /// `Unserialize` at `SER_DISK`, then `Validate(/*require_canonical_order=*/true)`: only a
    /// canonical, valid snapshot decodes. `net` is Core's `netInfo->Validate()` under the
    /// chain's parameters. Bytes after the snapshot are left in `input`.
    // Core: evo/snapshot.h:666 EvoSnapshot::Unserialize
    pub fn decode(input: &mut &[u8], net: &dyn NetInfoValidity) -> Result<Self, CodecError> {
        let mut r = Reader::new(input, None);
        let version = r.u16()?;
        if version != EVO_SNAPSHOT_VERSION {
            return Err(CodecError::new("unsupported evo snapshot version"));
        }
        let base_block_hash = BlockHash::from(hash(&mut r)?);
        let mn_list = decode_canonical_list(input, net)?;
        let mut r = Reader::new(input, None);
        let quorum_count = read_bounded(&mut r, LlmqType::AVAILABLE.len(), "quorum-type count")?;
        let quorums =
            (0..quorum_count).map(|_| read_quorum_data(&mut r)).collect::<Result<_, _>>()?;
        let history_count =
            read_bounded(&mut r, max_historical_mn_lists(), "historical MN-list count")?;
        let mut remaining_operations = max_historical_mn_operations();
        let mut historical_mn_list_diffs = Vec::with_capacity(history_count);
        for _ in 0..history_count {
            let mut r = Reader::new(input, None);
            let previous_block_hash = BlockHash::from(hash(&mut r)?);
            let block_hash = BlockHash::from(hash(&mut r)?);
            let height = r.i32()?;
            let total_registered_count = r.u32()?;
            let canonical_list_hash = Hash256::from(hash(&mut r)?);
            let diff = decode_canonical_diff(input, &mut remaining_operations, net)?;
            historical_mn_list_diffs.push(HistoricalMnListDiff {
                previous_block_hash,
                block_hash,
                height,
                total_registered_count,
                canonical_list_hash,
                diff,
            });
        }
        let mut r = Reader::new(input, None);
        let modifier_count = read_bounded(&mut r, MAX_MODIFIERS, "quorum modifier count")?;
        let quorum_modifiers = (0..modifier_count)
            .map(|_| {
                Ok(QuorumModifier {
                    llmq_type: LlmqType::from_base(r.u8()?),
                    work_block_hash: BlockHash::from(hash(&mut r)?),
                    modifier: Hash256::from(hash(&mut r)?),
                })
            })
            .collect::<Result<_, CodecError>>()?;
        let locked = r.i64()?;
        let current_limit = r.i64()?;
        let lately_unlocked = r.i64()?;
        let indexes = RangesSet::decode_bounded(input, MAX_RANGES)
            .map_err(|e| CodecError::new(e.to_string()))?;
        let mut r = Reader::new(input, None);
        let signal_count = read_bounded(&mut r, MAX_MNHF_SIGNALS, "MNHF signals")?;
        // The map normalizes order, so the wire's is checked here: strictly ascending bits, as
        // the encoder writes them (which also refuses a repeated bit).
        let mut mnhf_signals = BTreeMap::new();
        for _ in 0..signal_count {
            let bit = r.u8()?;
            let height = r.i32()?;
            if mnhf_signals.last_key_value().is_some_and(|(&previous, _)| previous >= bit) {
                return Err(CodecError::new("noncanonical MNHF signal order"));
            }
            mnhf_signals.insert(bit, height);
        }
        let snapshot = EvoSnapshot {
            version,
            base_block_hash,
            mn_list,
            quorums,
            historical_mn_list_diffs,
            quorum_modifiers,
            credit_pool: CreditPool {
                locked,
                current_limit,
                lately_unlocked,
                indexes,
            },
            mnhf_signals,
        };
        snapshot.validate(true, net)?;
        Ok(snapshot)
    }
}
