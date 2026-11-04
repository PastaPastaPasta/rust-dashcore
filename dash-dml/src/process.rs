//! The DML part of `CDeterministicMNManager::ProcessBlock` and `UndoBlock` (EV2 §3.8): the diff,
//! the snapshot cadence, the platform-ban resets in Core's position and the DIP3 enforcement pin.

use std::sync::Arc;

use dash_consensus::primitives::Block;
use dash_consensus::{BlockValidationError, BlockValidationResult};
use dash_evo::effect::{Effect, EffectSink};
use dash_primitives::{BlockHash, SpecialPayload, TxType};

use crate::build::{DmlParams, payload, pro_tx_of};
use crate::diff::{DmlDelta, MnListDiff, MnListDiffRecord};
use crate::list::MnList;

/// `DISK_SNAPSHOT_PERIOD`: a snapshot row every 576 blocks.
// Core: evo/deterministicmns.h DISK_SNAPSHOT_PERIOD
const DISK_SNAPSHOT_PERIOD: i32 = 576;

/// `bad-dip3-enf-block`.
const BAD_DIP3_ENF_BLOCK: &str = "bad-dip3-enf-block";

/// A connected block's list change: what the chainstate raises `MnListChanged` from (EV2 §3.8).
#[derive(Clone, Debug)]
pub struct MnListChange {
    pub old: Arc<MnList>,
    pub new: Arc<MnList>,
    pub diff: Arc<MnListDiff>,
}

/// The result of [`process_block`].
#[derive(Clone, Debug)]
pub struct ProcessedList {
    /// The block's list, its hash set.
    pub list: Arc<MnList>,
    /// `oldList.BuildDiff(newList)`.
    pub diff: Arc<MnListDiff>,
    /// The rows to persist with the block.
    pub delta: DmlDelta,
    /// The change for the listeners; v24 reports it whether or not the diff is empty.
    pub change: MnListChange,
}

/// `CDeterministicMNManager::ProcessBlock(block, pindex, newList)` once DIP3 is active at the
/// block and not in just-check mode (the chainstate's gates, `specialtxman.cpp` v24:913).
///
/// `new_list` is the build's list; its block hash is set here (`ProcessSpecialTxsInBlock`,
/// v24:907-911). In Core's order: the diff against `prev_list`, the diff and snapshot rows (a
/// snapshot at every 576th height and at the first DIP3 block), a `ResetPlatformBan` effect for
/// every ProUpServTx whose payload decodes, in block order, and only then the DIP3 enforcement
/// pin, so the resets survive `bad-dip3-enf-block`. Core's `failed-dmn-block` (an exception in
/// its `try`) has no counterpart: nothing here can fail.
// Core: evo/deterministicmns.cpp:689 CDeterministicMNManager::ProcessBlock
pub fn process_block(
    block: &Block,
    at: (BlockHash, i32),
    prev_list: &Arc<MnList>,
    new_list: MnList,
    params: &DmlParams,
    is_first_dip3_block: bool,
    effects: &mut dyn EffectSink,
) -> Result<ProcessedList, BlockValidationError> {
    let (hash, height) = at;
    let list = Arc::new(new_list.with_block_hash(hash));
    let record = Arc::new(MnListDiffRecord::build(prev_list, &list));
    let diff = Arc::new(record.diff.clone());
    let snapshot =
        (height % DISK_SNAPSHOT_PERIOD == 0 || is_first_dip3_block).then(|| Arc::clone(&list));

    for tx in block.txs().iter().skip(1) {
        if !tx.is_special_tx_version() || tx.tx_type() != TxType::ProviderUpdateService {
            continue;
        }
        if let Some(SpecialPayload::ProviderUpdateService(p)) = payload(tx) {
            effects.emit(Effect::ResetPlatformBan {
                pro_tx_hash: pro_tx_of(&p.pro_tx_hash),
                height,
            });
        }
    }

    if height == params.dip3_enforcement_height
        && params.dip3_enforcement_hash.is_some_and(|pinned| pinned != hash)
    {
        return Err(BlockValidationError::new(
            BlockValidationResult::Consensus,
            BAD_DIP3_ENF_BLOCK,
        ));
    }

    Ok(ProcessedList {
        list: Arc::clone(&list),
        diff: Arc::clone(&diff),
        delta: DmlDelta {
            block: hash,
            height,
            record,
            snapshot,
        },
        change: MnListChange {
            old: Arc::clone(prev_list),
            new: list,
            diff,
        },
    })
}

/// The DML part of `CDeterministicMNManager::UndoBlock`: the inverse change for the listeners
/// when the block's stored diff has changes, else nothing. Nothing is erased; the active list
/// becomes `prev`.
// Core: evo/deterministicmns.cpp:784 CDeterministicMNManager::UndoBlock
pub fn undo_block(
    cur: &Arc<MnList>,
    prev: &Arc<MnList>,
    stored: &MnListDiff,
) -> Option<MnListChange> {
    stored.has_changes().then(|| MnListChange {
        old: Arc::clone(cur),
        new: Arc::clone(prev),
        diff: Arc::new(cur.diff_to(prev)),
    })
}
