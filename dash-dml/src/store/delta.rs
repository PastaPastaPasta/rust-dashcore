//! `EvoDelta`: the transactional evo output of one block (A1 §2.5, §2.10).

use std::sync::Arc;

use dash_evo::llmq::LlmqCommitment;
use dash_primitives::{BlockHash, LlmqType, QuorumHash};

use crate::creditpool::{CreditBlockData, PoolState};
use crate::mnhf::EhfSignals;
use crate::{DmlDelta, SameRecord};

/// What connecting one block writes to the evo store: **transactional state only**.
///
/// Built into the block's overlay (A1 §2.10 "Overlay atomicity"): on success CH7a merges it and
/// writes it in the Durable B1 batch with `meta.evo_tip = block` (A1 §3.5.4); on failure, or in
/// proposal mode (`fJustCheck`), it is dropped. Core's non-transactional actions are never in
/// here: they are [`dash_evo::effect::Effect`]s, which survive a failure.
///
/// It carries no BLS context: Core's flag after a successful connect, a successful undo or a
/// restart is `!V19ActiveAfter(tip)`, which CH7a recomputes as [`dash_evo::bls::bls_ctx_after`]
/// after each commit (EV0 API review Q7). v24's `ScopedBLSLegacyScheme` commits the flag only
/// after a full connect, so nothing else moves it (A1 §2.25, DEC-82).
///
/// No `PartialEq` (the DML rows hold lazy keys); compare with [`SameRecord`].
#[derive(Clone, Debug)]
pub struct EvoDelta {
    /// The block.
    pub block: BlockHash,
    /// Its height.
    pub height: u32,
    /// The DML rows (EV2b's `process_block`). `None` below DIP3 (Core's
    /// `CDeterministicMNManager::ProcessBlock` returns before writing).
    pub dml: Option<DmlDelta>,
    /// The commitments the block mined (LQ2), in the block's order. Null commitments are not
    /// stored (Core returns before writing them).
    pub mined_commitments: Vec<MinedCommitmentRecord>,
    /// The block's credit data (EV3 D9): `Some` for every DIP3-active block whose data Core can
    /// read ([`crate::creditpool::connect_records`]); `None` where that read throws, which no
    /// connectable block holds (`bad-assetunlocktx-payload` and `CheckTransaction`'s output
    /// ranges reject it first, before V20 included), so reading it later is a store fault, never
    /// a verdict.
    pub credit_data: Option<CreditBlockData>,
    /// The credit pool after the block, or the exception Core's construction throws (EV3 D8):
    /// `Some` for every V20-active block.
    pub credit_pool: Option<Arc<PoolState>>,
    /// The block's MNHF signals row (EV3 D15, [`crate::mnhf::MnhfProcessed::row`]): `Some` for
    /// every V20-active block.
    pub mnhf_signals: Option<EhfSignals>,
    /// A disconnect's tombstones, in `GetCommitmentsFromBlock`'s order: Core erases the mined
    /// rows of every non-null commitment on undo, inside the evodb transaction
    /// (`llmq/blockprocessor.cpp` v24:573-601; CH7 Q1, §4.1.4). Empty for a connect.
    pub erased_mined: Vec<MinedErase>,
    /// The evo best block the attempt stages, Core's `b_b4` write (`validation.cpp`
    /// v24:2967 on a connect, :2389 on a disconnect; CH7 D1, D7): the block, or its parent for a
    /// disconnect. `None` when no evo best block is staged: [`MemEvoStore`] then moves its best
    /// block to [`EvoDelta::block`], as the dash-dml harnesses that model no disconnect expect.
    /// The node's store refuses `None` (dashd-chainstate's `EvoCodec::rows`, CH7 D40).
    ///
    /// [`MemEvoStore`]: super::MemEvoStore
    pub evo_tip: Option<BlockHash>,
}

impl SameRecord for EvoDelta {
    fn same_record(&self, other: &Self) -> bool {
        self.block == other.block
            && self.height == other.height
            && self.dml.same_record(&other.dml)
            && self.mined_commitments == other.mined_commitments
            && self.credit_data == other.credit_data
            && self.credit_pool.same_record(&other.credit_pool)
            && self.mnhf_signals == other.mnhf_signals
            && self.erased_mined == other.erased_mined
            && self.evo_tip == other.evo_tip
    }
}

/// One erased mined commitment: the tombstone of the two rows a connect wrote for it (Core's
/// `q_mc` row and its height-index row, `llmq/blockprocessor.cpp` v24:573-601; CH7 §4.1.4).
///
/// It names the rows by the fields their keys are built from, so applying it never reads the old
/// row; the store's codec builds both keys from them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MinedErase {
    /// The commitment's type.
    pub llmq_type: LlmqType,
    /// Its quorum hash: the `q_mc` key.
    pub quorum_hash: QuorumHash,
    /// The height of the disconnected block that mined it: the height-index key.
    pub mined_height: u32,
    /// The height-index family, as [`MinedCommitmentRecord::rotation_index`] chose it when the
    /// row was written: Core's undo decides it at the mining block (v24:579-590), the write at
    /// the base, and both agree because a commitment is mined in its base's cycle.
    pub rotation_index: Option<i16>,
}

/// One mined commitment (Core's `q_mc` row and its height-index row,
/// `llmq/blockprocessor.cpp` v24:432-441).
///
/// `PartialEq` is Core's `SerializedEqual` (`llmq/blockprocessor.cpp` v24:381-382) for decoded
/// commitments: [`LlmqCommitment`]'s BLS fields are eager and compare by their decode (LQ2 D1).
#[derive(Clone, Debug, PartialEq)]
pub struct MinedCommitmentRecord {
    /// The commitment's type.
    pub llmq_type: LlmqType,
    /// Its quorum hash: the base block's hash.
    pub quorum_hash: QuorumHash,
    /// The height of the base block (the height-index row's value).
    pub base_height: u32,
    /// `Some(quorumIndex)` when rotation is enabled for the type, which puts the record in the
    /// indexed family that
    /// [`dash_evo::llmq::MinedCommitmentReader::nth_mined_by_index`] reads; `None` puts it in the
    /// family [`dash_evo::llmq::MinedCommitmentReader::mined_until`] reads. LQ2 decides it as Core
    /// does: `IsQuorumRotationEnabled` at the **base** block when writing
    /// (`llmq/blockprocessor.cpp` v24:421) and at the mining block when undoing (v24:543); the
    /// two agree because both blocks are in one DKG cycle.
    pub rotation_index: Option<i16>,
    /// The commitment.
    pub commitment: Arc<LlmqCommitment>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creditpool::PoolError;

    /// Two deltas are the same record only when every field is.
    #[test]
    fn a_delta_is_the_same_record_only_field_for_field() {
        let delta = EvoDelta {
            block: BlockHash::from([1; 32]),
            height: 7,
            dml: None,
            mined_commitments: Vec::new(),
            credit_data: None,
            credit_pool: None,
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: None,
        };
        assert!(delta.same_record(&delta.clone()));
        let changes: [fn(&mut EvoDelta); 7] = [
            |d| d.block = BlockHash::from([3; 32]),
            |d| d.height = 8,
            |d| {
                d.credit_data = Some(CreditBlockData {
                    balance: 1,
                    unlocked: 0,
                })
            },
            |d| d.credit_pool = Some(Arc::new(Err(PoolError::PrevLocked))),
            |d| d.mnhf_signals = Some(EhfSignals::from_iter([(1, 2)])),
            |d| {
                d.erased_mined = vec![MinedErase {
                    llmq_type: LlmqType::Llmq50_60,
                    quorum_hash: QuorumHash::from([2; 32]),
                    mined_height: 7,
                    rotation_index: None,
                }]
            },
            |d| d.evo_tip = Some(BlockHash::from([4; 32])),
        ];
        for (i, change) in changes.iter().enumerate() {
            let mut other = delta.clone();
            change(&mut other);
            assert!(!delta.same_record(&other) && !other.same_record(&delta), "field {i}");
        }
    }
}
