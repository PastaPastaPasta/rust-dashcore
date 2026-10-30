//! Core's `evo::EvoSnapshot` (`evo/snapshot.{h,cpp}` at v24.0.0-rc.4, unchanged since rc.3): the
//! Dash-derived state an assumeutxo snapshot carries (O9b S6a), with its canonical encoding, its
//! bounded decoder, `Validate` and `GetEvoSnapshotHash`.
//!
//! The decode domain is Core's: every bound, order and value rule refuses where Core's
//! `Unserialize` does, with the `what()` of the `std::ios_base::failure` Core throws (Core throws
//! nothing else on this path). [`EvoSnapshot::decode`] ends, as Core's does, with
//! [`EvoSnapshot::validate`] in canonical-order mode, which replays the historical diff chain
//! (`ReconstructHistoricalMNLists`), so a decoded snapshot's lists are all reconstructible.
//!
//! What `Validate` needs from the chain's parameters is Core's static LLMQ table
//! ([`AVAILABLE_LLMQS`], as Core's `SnapshotLLMQParams` reads it) and, for the masternodes'
//! network info, the [`NetInfoValidity`] the caller passes (Core's `netInfo->Validate()` under the
//! running chain's `Params()`).

use std::collections::BTreeMap;

use dash_num::Hash256;
use dash_params::{AVAILABLE_LLMQS, LlmqParams};
use dash_primitives::{BlockHash, Commitment, LlmqType};

use crate::codec::{CodecError, NetInfoValidity};
use crate::creditpool::CreditPool;
use crate::diff::MnListDiff;
use crate::list::MnList;

mod codec;
mod validate;

pub use dash_evo::llmq::QuorumSnapshot;

/// `EVO_SNAPSHOT_VERSION`.
pub const EVO_SNAPSHOT_VERSION: u16 = 3;
/// `EVO_SNAPSHOT_ROTATION_CYCLES`: the rotation snapshots a rotated type carries, H-C..H-4C.
pub const ROTATION_CYCLES: usize = 4;
/// `EVO_SNAPSHOT_MAX_RANGES`: the most ranges the credit pool's index set holds.
pub const MAX_RANGES: u64 = 100_000;
/// `EVO_SNAPSHOT_MAX_MODIFIERS`.
pub const MAX_MODIFIERS: usize = 4_096;
/// `EVO_SNAPSHOT_MAX_SKIPLIST_ENTRIES`: a decode ceiling on a rotation skip list.
pub const MAX_SKIPLIST_ENTRIES: usize = 1_000_000;
/// `EVO_SNAPSHOT_MAX_QUORUM_SIZE`: the allocation ceiling on a commitment's bitsets.
pub const MAX_QUORUM_SIZE: usize = 10_000;
/// `EVO_SNAPSHOT_MAX_RECONSTRUCTION_RECORDS`: the masternode records the diff-chain replay may
/// visit.
pub const MAX_RECONSTRUCTION_RECORDS: usize = 8_000_000;
/// `VERSIONBITS_NUM_BITS`: the MNHF signals a snapshot can hold, one per bit.
pub const MAX_MNHF_SIGNALS: usize = dash_consensus::deployments::VERSIONBITS_NUM_BITS as usize;

/// `MinedQuorumCommitment`: a commitment with the blocks that place it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinedQuorumCommitment {
    /// `quorum_base_block_hash`.
    pub quorum_base_block_hash: BlockHash,
    /// `work_block_hash`: the block whose masternode list built the quorum.
    pub work_block_hash: BlockHash,
    /// `commitment`.
    pub commitment: Commitment,
    /// `mined_block_hash`.
    pub mined_block_hash: BlockHash,
}

/// `QuorumSnapshotEntry`: one rotation cycle's `CQuorumSnapshot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuorumSnapshotEntry {
    /// `cycle_base_block_hash`.
    pub cycle_base_block_hash: BlockHash,
    /// `work_block_hash`.
    pub work_block_hash: BlockHash,
    /// `snapshot`.
    pub snapshot: QuorumSnapshot,
}

/// `HistoricalMNListDiff`: one link of the chain from the base list back to a work block's list.
#[derive(Clone, Debug, Default)]
pub struct HistoricalMnListDiff {
    /// `previous_block_hash`: the newer list's block.
    pub previous_block_hash: BlockHash,
    /// `block_hash`: the list this diff reconstructs.
    pub block_hash: BlockHash,
    /// `height`.
    pub height: i32,
    /// `total_registered_count` of the reconstructed list.
    pub total_registered_count: u32,
    /// `canonical_list_hash`: `CanonicalMNListHash` of the reconstructed list.
    pub canonical_list_hash: Hash256,
    /// `diff`: the newer list to the older one.
    pub diff: MnListDiff,
}

/// `QuorumModifier`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuorumModifier {
    /// `llmq_type`.
    pub llmq_type: LlmqType,
    /// `work_block_hash`.
    pub work_block_hash: BlockHash,
    /// `modifier`.
    pub modifier: Hash256,
}

/// `QuorumSnapshotData`: one LLMQ type's commitments and rotation snapshots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuorumSnapshotData {
    /// `llmq_type`.
    pub llmq_type: LlmqType,
    /// `rotation_enabled`.
    pub rotation_enabled: bool,
    /// `active_commitments`.
    pub active_commitments: Vec<MinedQuorumCommitment>,
    /// `safety_commitments`.
    pub safety_commitments: Vec<MinedQuorumCommitment>,
    /// `rotation_snapshots`.
    pub rotation_snapshots: Vec<QuorumSnapshotEntry>,
}

/// `EvoSnapshot`. The collections may be held in any order; the encoder sorts them
/// ([`EvoSnapshot::encode`]), and only [`EvoSnapshot::validate`]'s canonical mode requires the
/// order.
#[derive(Clone, Debug)]
pub struct EvoSnapshot {
    /// `version`.
    pub version: u16,
    /// `base_block_hash`.
    pub base_block_hash: BlockHash,
    /// `mn_list`: the base block's list.
    pub mn_list: MnList,
    /// `quorums`.
    pub quorums: Vec<QuorumSnapshotData>,
    /// `historical_mn_list_diffs`.
    pub historical_mn_list_diffs: Vec<HistoricalMnListDiff>,
    /// `quorum_modifiers`.
    pub quorum_modifiers: Vec<QuorumModifier>,
    /// `credit_pool`.
    pub credit_pool: CreditPool,
    /// `mnhf_signals`: bit to mined height (Core's `std::map<uint8_t, int>`).
    pub mnhf_signals: BTreeMap<u8, i32>,
}

impl Default for EvoSnapshot {
    /// Core's default-constructed snapshot: the current version, a null base and the pre-DIP3
    /// default list.
    fn default() -> Self {
        EvoSnapshot {
            version: EVO_SNAPSHOT_VERSION,
            base_block_hash: BlockHash::default(),
            mn_list: MnList::default_pre_dip3(),
            quorums: Vec::new(),
            historical_mn_list_diffs: Vec::new(),
            quorum_modifiers: Vec::new(),
            credit_pool: CreditPool::default(),
            mnhf_signals: BTreeMap::new(),
        }
    }
}

impl EvoSnapshot {
    /// `GetEvoSnapshotHash`: [`EvoSnapshot::validate`] (not in canonical mode), then the single
    /// SHA256 of the encoding.
    // Core: evo/snapshot.cpp:339 GetEvoSnapshotHash
    pub fn hash(&self, net: &dyn NetInfoValidity) -> Result<Hash256, CodecError> {
        self.validate(false, net)?;
        let mut bytes = Vec::new();
        self.encode(&mut bytes);
        Ok(Hash256::from(dash_consensus::hash::sha256(&bytes)))
    }
}

/// `SnapshotLLMQParams`: Core's static table entry for `llmq_type`, whatever the running chain
/// overrides.
// Core: evo/snapshot.h:104 SnapshotLLMQParams
fn snapshot_llmq_params(llmq_type: LlmqType) -> Result<&'static LlmqParams, CodecError> {
    LlmqParams::available(llmq_type)
        .ok_or_else(|| CodecError::new("unknown evo snapshot LLMQ type"))
}

/// `SnapshotCommitmentCount`: the most commitments (active and safety) a type carries.
// Core: evo/snapshot.h:112 SnapshotCommitmentCount
fn snapshot_commitment_count(
    params: &LlmqParams,
    rotation_enabled: bool,
) -> Result<usize, CodecError> {
    let active = params.signing_active_quorum_count.max(0) as usize;
    if !rotation_enabled {
        return Ok(
            (params.signing_active_quorum_count + 1).max(params.keep_old_connections).max(0)
                as usize,
        );
    }
    if params.keep_old_connections.max(0) as usize > 2 * active {
        return Err(CodecError::new("rotated LLMQ retention exceeds two cycles"));
    }
    Ok(2 * active)
}

/// `EvoSnapshotMaxHistoricalMNLists`: the distinct work-block lists a snapshot can need.
// Core: evo/snapshot.h:131 EvoSnapshotMaxHistoricalMNLists
pub fn max_historical_mn_lists() -> usize {
    AVAILABLE_LLMQS
        .iter()
        .map(|params| {
            let count = snapshot_commitment_count(params, params.use_rotation)
                .expect("Core's static_assert: no table entry retains more than two cycles");
            if params.use_rotation {
                count + ROTATION_CYCLES
            } else {
                count
            }
        })
        .sum()
}

/// `EvoSnapshotMaxHistoricalMNOperations`: 4,096 operations per historical list.
// Core: evo/snapshot.h:149 EvoSnapshotMaxHistoricalMNOperations
pub fn max_historical_mn_operations() -> usize {
    max_historical_mn_lists() * 4_096
}

#[cfg(test)]
mod tests;
