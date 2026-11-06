//! Core's two pure helpers beside the snapshot codec (O9b S6b-0): `EvoSnapshotReconstructionHeights`,
//! the quorum and work heights whose lists a snapshot at a base may need, and
//! `VerifyEvoSnapshotCbTx`, a snapshot's content checked against its base block's CbTx.

use dash_evo::cbtx::{CBTX_VERSION_CLSIG_AND_BALANCE, CBTX_VERSION_MERKLE_ROOT_QUORUMS};
use dash_evo::llmq::WORK_DIFF_DEPTH;
use dash_evo::sml::merkle_root_quorums_from_hashes;
use dash_params::LlmqParams;
use dash_params::LlmqType;
use dash_primitives::CoinbaseCommitment;

use super::{EvoSnapshot, ROTATION_CYCLES, snapshot_commitment_count};
use crate::blockprocessor::commitment_hash;
use crate::codec::{CodecError, NetInfoValidity};
use crate::sml::merkle_root_mn_list_full;

/// `QuorumReconstructionHeight`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuorumReconstructionHeight {
    /// `llmq_type`.
    pub llmq_type: LlmqType,
    /// `rotation`: the type's `useRotation`.
    pub rotation: bool,
    /// `quorum_height`.
    pub quorum_height: i32,
    /// `work_height`: `WORK_DIFF_DEPTH` below the quorum height.
    pub work_height: i32,
}

/// `EvoSnapshotReconstructionHeights`: for each type in `enabled_llmqs`, in the order given, the
/// quorum heights a snapshot at `base_height` may need lists for, each with its work height.
/// `H` is the base rounded down to the type's DKG interval. A rotated type gives the cycles
/// H-C..H-4C (`ROTATION_CYCLES` of them, from one cycle back); any other type gives
/// `SnapshotCommitmentCount` quorum heights from H itself. Heights below genesis are kept, as
/// Core keeps them.
///
/// Core throws `std::invalid_argument`; the error carries its text. Where Core's `int`
/// arithmetic would overflow (parameters no table holds), this refuses with the parameters text
/// (O9b-S6a-notes D25).
// Core: evo/snapshot.cpp:128 EvoSnapshotReconstructionHeights
pub fn reconstruction_heights<'a>(
    base_height: i32,
    enabled_llmqs: impl IntoIterator<Item = (LlmqType, &'a LlmqParams)>,
) -> Result<Vec<QuorumReconstructionHeight>, CodecError> {
    if base_height < 0 {
        return Err(CodecError::new("invalid reconstruction base height"));
    }
    let invalid = || CodecError::new("invalid reconstruction LLMQ parameters");
    let mut heights = Vec::new();
    for (llmq_type, params) in enabled_llmqs {
        if params.dkg_interval == 0 || params.signing_active_quorum_count == 0 {
            return Err(invalid());
        }
        let interval = i32::from(params.dkg_interval);
        let h = base_height - base_height % interval;
        let (first, count) = if params.use_rotation {
            (1, ROTATION_CYCLES)
        } else {
            // `SnapshotCommitmentCount`'s `signingActiveQuorumCount + 1`
            // (`evo/snapshot.h` rc.5:115) over Core's `int` (`llmq/params.h` rc.5:105); a `u16`
            // count (quirk 46) never overflows it.
            i32::from(params.signing_active_quorum_count).checked_add(1).ok_or_else(invalid)?;
            (0, snapshot_commitment_count(params, false)?)
        };
        for i in first..first + count {
            let quorum_height = i32::try_from(i)
                .ok()
                .and_then(|i| i.checked_mul(interval))
                .and_then(|back| h.checked_sub(back))
                .ok_or_else(invalid)?;
            let work_height =
                quorum_height.checked_sub(WORK_DIFF_DEPTH as i32).ok_or_else(invalid)?;
            heights.push(QuorumReconstructionHeight {
                llmq_type,
                rotation: params.use_rotation,
                quorum_height,
                work_height,
            });
        }
    }
    Ok(heights)
}

impl EvoSnapshot {
    /// `VerifyEvoSnapshotCbTx`: [`EvoSnapshot::validate`] (not in canonical mode), then the base
    /// block's CbTx against the content, refusing at the first mismatch with Core's text: the
    /// height is the list's; `merkleRootMNList` is the list's simplified-list root; from version
    /// 2, `merkleRootQuorums` is the root of the active commitments' hashes, byte-sorted; from
    /// version 3, the credit-pool balance is the pool's `locked`. A mutated tree is a mismatch,
    /// as in Core.
    ///
    /// A CbTx field its version lacks reads as Core's default, zero.
    // Core: evo/snapshot.cpp:349 VerifyEvoSnapshotCbTx
    pub fn verify_cbtx(
        &self,
        cbtx: &CoinbaseCommitment,
        net: &dyn NetInfoValidity,
    ) -> Result<(), CodecError> {
        self.validate(false, net)?;
        if cbtx.height != self.mn_list.height() {
            return Err(CodecError::new("evo snapshot coinbase height mismatch"));
        }
        // `mn_list.to_sml()->CalcMerkleRoot(&mutated)`; only a mutated tree is an error here.
        if merkle_root_mn_list_full(&self.mn_list).ok() != Some(cbtx.merkle_root_mn_list) {
            return Err(CodecError::new("evo snapshot masternode merkle root mismatch"));
        }
        if cbtx.version >= CBTX_VERSION_MERKLE_ROOT_QUORUMS {
            let hashes = self
                .quorums
                .iter()
                .flat_map(|data| &data.active_commitments)
                .map(|entry| commitment_hash(&entry.commitment))
                .collect();
            if merkle_root_quorums_from_hashes(hashes).ok()
                != Some(cbtx.merkle_root_quorums.unwrap_or_default())
            {
                return Err(CodecError::new("evo snapshot quorum merkle root mismatch"));
            }
        }
        if cbtx.version >= CBTX_VERSION_CLSIG_AND_BALANCE
            && self.credit_pool.locked != cbtx.credit_pool_balance.unwrap_or(0)
        {
            return Err(CodecError::new("evo snapshot credit pool balance mismatch"));
        }
        Ok(())
    }
}
