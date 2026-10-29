//! `EvoSnapshot::Validate` and the diff-chain replay it runs (`snapshot.cpp` v24:30-124,
//! 157-337), with `CDeterministicMNList::ApplyDiffForSnapshot` (`deterministicmns.cpp`
//! v24:395-437). Every refusal is Core's `what()`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_consensus::money_range;
use dash_primitives::BlockHash;
use dash_types::Numeric;

use super::codec::{
    commitment_key, history_key, is_indexed, is_strictly_sorted_by, modifier_key, quorum_key,
    rotation_key, sorted_by,
};
use super::{
    EVO_SNAPSHOT_VERSION, EvoSnapshot, MAX_MNHF_SIGNALS, MAX_MODIFIERS, MAX_QUORUM_SIZE,
    MAX_RANGES, MAX_RECONSTRUCTION_RECORDS, MAX_SKIPLIST_ENTRIES, MinedQuorumCommitment,
    QuorumSnapshotData, ROTATION_CYCLES, max_historical_mn_lists, max_historical_mn_operations,
    snapshot_commitment_count, snapshot_llmq_params,
};
use crate::codec::{
    CodecError, MAX_MNS, NetInfoValidity, canonical_mn_list_hash, check_canonical_invariants,
    check_canonical_list, check_mn_encoding, check_prefix_runs, check_state_diff_encoding,
};
use crate::diff::MnListDiff;
use crate::list::MnList;
use crate::types::Dmn;

/// The commitment versions Core defines (`LEGACY_BLS_NON_INDEXED` .. `BASIC_BLS_INDEXED`).
const COMMITMENT_VERSIONS: std::ops::RangeInclusive<u16> = 1..=4;

/// `ValidateCommitments`: one of a type's two commitment sets. `quorum_hashes` spans both sets;
/// the quorum indexes are distinct within a set only.
// Core: evo/snapshot.cpp:30 ValidateCommitments
fn validate_commitments(
    data: &QuorumSnapshotData,
    commitments: &[MinedQuorumCommitment],
    quorum_hashes: &mut BTreeSet<[u8; 32]>,
    require_canonical_order: bool,
) -> Result<(), CodecError> {
    if require_canonical_order && !is_strictly_sorted_by(commitments, commitment_key) {
        return Err(CodecError::new("noncanonical evo quorum commitments"));
    }
    let params = snapshot_llmq_params(data.llmq_type)?;
    let mut quorum_indexes = BTreeSet::new();
    for entry in commitments {
        let qc = &entry.commitment;
        let indexed = is_indexed(qc.version);
        if qc.signers.num_bits != qc.valid_members.num_bits
            || qc.signers.num_bits == 0
            || qc.signers.num_bits > MAX_QUORUM_SIZE as u64
        {
            return Err(CodecError::new("invalid evo quorum commitment sizes"));
        }
        if !COMMITMENT_VERSIONS.contains(&qc.version) {
            return Err(CodecError::new("unknown evo quorum commitment version"));
        }
        if entry.quorum_base_block_hash.is_null()
            || entry.work_block_hash.is_null()
            || entry.mined_block_hash.is_null()
        {
            return Err(CodecError::new("null evo quorum commitment block hash"));
        }
        if qc.llmq_type != data.llmq_type {
            return Err(CodecError::new("mismatched evo quorum commitment type"));
        }
        if qc.quorum_hash.as_bytes() != entry.quorum_base_block_hash.as_bytes() {
            return Err(CodecError::new("mismatched evo quorum commitment base hash"));
        }
        if indexed != data.rotation_enabled {
            return Err(CodecError::new("mismatched evo quorum rotation version"));
        }
        // Core's `quorumIndex` is 0 unless an indexed version read it.
        let index = qc.quorum_index.unwrap_or(0);
        if indexed && (index < 0 || i32::from(index) >= params.signing_active_quorum_count) {
            return Err(CodecError::new("invalid evo quorum index"));
        }
        if !quorum_hashes.insert(*entry.quorum_base_block_hash.as_bytes()) {
            return Err(CodecError::new("duplicate evo quorum base hash"));
        }
        if indexed && !quorum_indexes.insert(index) {
            return Err(CodecError::new("duplicate evo quorum index"));
        }
    }
    Ok(())
}

/// `ValidateHashPrefixRuns` over the leading 64 bits of `hashes`.
fn check_hash_prefix_runs(hashes: impl Iterator<Item = u64>) -> Result<(), CodecError> {
    let mut prefixes: Vec<u64> = hashes.collect();
    prefixes.sort_unstable();
    check_prefix_runs(prefixes.into_iter())
}

impl MnList {
    /// `ApplyDiffForSnapshot(block_hash, height, total_registered_count, diff)`: a historical
    /// diff spanning many blocks. The removals, then every updated masternode taken out before
    /// any is put back (surviving masternodes may have exchanged unique properties between the
    /// endpoints), then the additions, without bumping the counter; then the counter. On `Err`
    /// the list is in an unspecified state, as Core's is (the replay discards it).
    // Core: evo/deterministicmns.cpp:395 CDeterministicMNList::ApplyDiffForSnapshot
    fn apply_diff_for_snapshot(
        &mut self,
        block_hash: BlockHash,
        height: i32,
        total_registered_count: u32,
        diff: &MnListDiff,
    ) -> Result<(), CodecError> {
        if height < 0 {
            return Err(CodecError::new("negative historical MN-list height"));
        }
        if diff.added.iter().any(|dmn| {
            self.get_by_internal_id(dmn.internal_id).is_some()
                || self.get(&dmn.pro_tx_hash).is_some()
        }) {
            return Err(CodecError::new("historical MN-diff addition reuses an existing identity"));
        }
        self.set_position(block_hash, height);
        let list_error = |e: crate::list::MnListError| CodecError::new(e.core_message());
        for id in &diff.removed {
            let pro_tx = self
                .get_by_internal_id(*id)
                .ok_or_else(|| {
                    CodecError::new(format!(
                        "ApplyDiffForSnapshot: can't find a removed masternode, id={id}"
                    ))
                })?
                .pro_tx_hash;
            self.remove_mn(&pro_tx).map_err(list_error)?;
        }
        // Core walks `updatedMNs`, a salted `unordered_map` filled in ascending id order. In
        // libstdc++ a node inserted into an empty bucket goes to the front, so absent bucket
        // collisions and rehashes Core visits the updates in descending order. That order only
        // decides which refusal is reported when several apply (O9b-S6a-notes D22).
        let mut updated = Vec::with_capacity(diff.updated.len());
        for (id, state) in diff.updated.iter().rev() {
            let old = Arc::clone(self.get_by_internal_id(*id).ok_or_else(|| {
                CodecError::new(format!(
                    "ApplyDiffForSnapshot: can't find an updated masternode, id={id}"
                ))
            })?);
            let (hot, cold) = state.apply(&old.hot, &old.cold);
            self.remove_mn(&old.pro_tx_hash).map_err(list_error)?;
            updated.push(Arc::new(Dmn {
                hot,
                cold,
                ..Dmn::clone(&old)
            }));
        }
        for dmn in updated.into_iter().chain(diff.added.iter().cloned()) {
            self.add_mn(dmn, false).map_err(list_error)?;
        }
        self.set_total_registered(total_registered_count);
        self.mark_decoded();
        Ok(())
    }
}

impl EvoSnapshot {
    /// `ReconstructHistoricalMNLists`: the diff chain replayed newest first from the base list,
    /// each list checked as Core checks it and compared with its `canonical_list_hash`. The
    /// lists are keyed by their block hash's bytes. `max_records` is Core's record budget
    /// ([`MAX_RECONSTRUCTION_RECORDS`] by default).
    // Core: evo/snapshot.cpp:157 ReconstructHistoricalMNLists
    pub fn reconstruct_historical_mn_lists(
        &self,
        net: &dyn NetInfoValidity,
        max_records: usize,
    ) -> Result<BTreeMap<[u8; 32], MnList>, CodecError> {
        let mut lists = BTreeMap::new();
        let mut current = self.mn_list.clone();
        let mut previous_hash = self.base_block_hash;
        let mut previous_height = current.height();
        let mut records_processed = 0usize;
        let history = sorted_by(&self.historical_mn_list_diffs, history_key);
        for entry in history {
            // Newest first: every target is a block not yet seen, and the registration counter,
            // which only grows on-chain, does not grow.
            if entry.previous_block_hash != previous_hash
                || entry.block_hash.is_null()
                || entry.block_hash == self.base_block_hash
                || lists.contains_key(entry.block_hash.as_bytes())
                || entry.height < 0
                || entry.height >= previous_height
                || entry.total_registered_count > current.total_registered()
                || entry.canonical_list_hash.is_null()
            {
                return Err(CodecError::new("broken historical MN-list diff chain"));
            }
            records_processed += current.len() + entry.diff.added.len();
            if records_processed > max_records {
                return Err(CodecError::new(
                    "historical MN-list reconstruction record budget exceeded",
                ));
            }
            check_hash_prefix_runs(
                current.iter().chain(&entry.diff.added).map(|dmn| dmn.pro_tx_hash.immer_hash()),
            )?;
            for dmn in &entry.diff.added {
                check_mn_encoding(dmn)?;
            }
            for (_, state) in &entry.diff.updated {
                check_state_diff_encoding(state)?;
            }
            current.apply_diff_for_snapshot(
                entry.block_hash,
                entry.height,
                entry.total_registered_count,
                &entry.diff,
            )?;
            check_canonical_invariants(&current, net)?;
            // An update can combine individually bounded fields into an oversized masternode.
            for (id, _) in &entry.diff.updated {
                let dmn =
                    current.get_by_internal_id(*id).expect("ApplyDiffForSnapshot put it back");
                check_mn_encoding(dmn)?;
            }
            if canonical_mn_list_hash(&current) != entry.canonical_list_hash {
                return Err(CodecError::new("historical MN-list diff hash mismatch"));
            }
            lists.insert(*entry.block_hash.as_bytes(), current.clone());
            previous_hash = entry.block_hash;
            previous_height = entry.height;
        }
        Ok(lists)
    }

    /// `Validate(require_canonical_order)`: every invariant that needs no chainstate, in Core's
    /// order. The decoder runs it in canonical-order mode, which also requires every collection
    /// in its canonical order.
    // Core: evo/snapshot.cpp:222 EvoSnapshot::Validate
    pub fn validate(
        &self,
        require_canonical_order: bool,
        net: &dyn NetInfoValidity,
    ) -> Result<(), CodecError> {
        if self.version != EVO_SNAPSHOT_VERSION {
            return Err(CodecError::new("unsupported evo snapshot version"));
        }
        if self.base_block_hash.is_null() || *self.mn_list.block_hash() != self.base_block_hash {
            return Err(CodecError::new("evo snapshot base block mismatch"));
        }
        check_canonical_list(&self.mn_list, net)?;
        if self.quorums.len() > dash_params::AVAILABLE_LLMQS.len()
            || self.historical_mn_list_diffs.len() > max_historical_mn_lists()
            || self.quorum_modifiers.len() > MAX_MODIFIERS
            || self.mnhf_signals.len() > MAX_MNHF_SIGNALS
        {
            return Err(CodecError::new("oversized evo snapshot collection"));
        }
        // The decoder charges the same amounts while streaming: without this a snapshot could
        // validate, be hashed and be written, yet not decode from its own bytes.
        let operations: usize = self
            .historical_mn_list_diffs
            .iter()
            .map(|h| h.diff.added.len() + h.diff.updated.len() + h.diff.removed.len())
            .sum();
        if operations > max_historical_mn_operations() {
            return Err(CodecError::new("historical MN-diff operation budget exceeded"));
        }
        let pool = &self.credit_pool;
        if !money_range(pool.locked)
            || !money_range(pool.current_limit)
            || !money_range(pool.lately_unlocked)
            || pool.current_limit > pool.locked
        {
            return Err(CodecError::new("invalid evo snapshot credit pool amounts"));
        }
        if pool.indexes.range_count() as u64 > MAX_RANGES {
            return Err(CodecError::new("oversized CRangesSet range count"));
        }
        for (&bit, &height) in &self.mnhf_signals {
            if usize::from(bit) >= MAX_MNHF_SIGNALS || height < 0 || height > self.mn_list.height()
            {
                return Err(CodecError::new("invalid evo snapshot MNHF signal"));
            }
        }
        if require_canonical_order
            && (!is_strictly_sorted_by(&self.quorums, quorum_key)
                || !is_strictly_sorted_by(&self.historical_mn_list_diffs, history_key)
                || !is_strictly_sorted_by(&self.quorum_modifiers, modifier_key))
        {
            return Err(CodecError::new("noncanonical evo snapshot top-level order"));
        }

        let historical = self.reconstruct_historical_mn_lists(net, MAX_RECONSTRUCTION_RECORDS)?;
        let mut required_modifiers = BTreeSet::new();
        let mut quorum_types = BTreeSet::new();
        for data in &self.quorums {
            let params = snapshot_llmq_params(data.llmq_type)?;
            if !quorum_types.insert(data.llmq_type.to_base())
                || (data.rotation_enabled && !params.use_rotation)
            {
                return Err(CodecError::new("invalid evo quorum type"));
            }
            let active = params.signing_active_quorum_count.max(0) as usize;
            let total = snapshot_commitment_count(params, data.rotation_enabled)?;
            let max_snapshots = if data.rotation_enabled {
                ROTATION_CYCLES
            } else {
                0
            };
            if data.active_commitments.len() > active
                || data.safety_commitments.len() > total.saturating_sub(active)
                || data.rotation_snapshots.len() > max_snapshots
            {
                return Err(CodecError::new("invalid params-derived evo per-type quorum counts"));
            }
            let mut quorum_hashes = BTreeSet::new();
            for commitments in [&data.active_commitments, &data.safety_commitments] {
                validate_commitments(
                    data,
                    commitments,
                    &mut quorum_hashes,
                    require_canonical_order,
                )?;
            }
            for entry in data.active_commitments.iter().chain(&data.safety_commitments) {
                required_modifiers
                    .insert((data.llmq_type.to_base(), *entry.work_block_hash.as_bytes()));
            }
            if require_canonical_order
                && !is_strictly_sorted_by(&data.rotation_snapshots, rotation_key)
            {
                return Err(CodecError::new("noncanonical evo quorum rotation snapshots"));
            }
            let mut cycle_hashes = BTreeSet::new();
            for entry in &data.rotation_snapshots {
                let snapshot = &entry.snapshot;
                // Only the first skip-list entry is an absolute index; later ones are deltas.
                if entry.cycle_base_block_hash.is_null()
                    || entry.work_block_hash.is_null()
                    || !cycle_hashes.insert(*entry.cycle_base_block_hash.as_bytes())
                    || !historical.contains_key(entry.work_block_hash.as_bytes())
                    || snapshot.skip_mode().is_none()
                    || snapshot.active_quorum_members.len() > MAX_MNS
                    || snapshot.skip_list.len() > MAX_SKIPLIST_ENTRIES
                    || snapshot.skip_list.first().is_some_and(|&first| first < 0)
                {
                    return Err(CodecError::new("invalid evo quorum rotation snapshot"));
                }
                required_modifiers
                    .insert((data.llmq_type.to_base(), *entry.work_block_hash.as_bytes()));
            }
        }
        // Every commitment and rotation snapshot needs its work block's list, and no other.
        let required_work_hashes: BTreeSet<&[u8; 32]> =
            required_modifiers.iter().map(|(_, hash)| hash).collect();
        if !historical.keys().eq(required_work_hashes) {
            return Err(CodecError::new("missing or extra historical MN-list diff target"));
        }
        let mut actual_modifiers = BTreeSet::new();
        for entry in &self.quorum_modifiers {
            snapshot_llmq_params(entry.llmq_type)?;
            if entry.work_block_hash.is_null()
                || entry.modifier.is_null()
                || !actual_modifiers
                    .insert((entry.llmq_type.to_base(), *entry.work_block_hash.as_bytes()))
            {
                return Err(CodecError::new("invalid or duplicate evo quorum modifier"));
            }
        }
        if actual_modifiers != required_modifiers {
            return Err(CodecError::new("missing or extra evo quorum modifier"));
        }
        Ok(())
    }
}
