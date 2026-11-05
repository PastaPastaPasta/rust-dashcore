//! The stage [`ChainHandle::set_before_block_tail`]'s hook runs at (CH3 §4.6, D39; CH3b D223): a
//! submitted block accepted and indexed with its data, its coins not yet connected, V between
//! Core's two `cs_main` scopes. A process test makes the block's index record durable there, by
//! V's own flush, and then ends the process before the block's `ActivateBestChain`.
//!
//! [`ChainHandle::set_before_block_tail`]: super::ChainHandle::set_before_block_tail

use dash_primitives::BlockHash;

use super::actor::V;
use super::seams::Fatal;

/// A submitted block's ABC tail as it starts, which the hook is lent.
pub struct BlockTailStage<'v> {
    pub(super) v: &'v mut V,
    pub(super) hash: BlockHash,
}

impl BlockTailStage<'_> {
    /// The block's hash.
    pub fn hash(&self) -> BlockHash {
        self.hash
    }

    // Core: validation.cpp:3128-3134 ForceFlushStateToDisk
    /// D39 steps 1–2: Core's `ForceFlushStateToDisk()` on V, the copy point's ALWAYS flush, which
    /// waits for W's release (CH2; `FlushControl::copy_point_flush`), then this block's
    /// `block_index` record and the `meta` records read from a snapshot of the database at the
    /// epoch W released, not from V's index. `Ok(None)`: the site has no database.
    ///
    /// # Errors
    ///
    /// The flush failed. The block's tail then returns the same [`Fatal`] once the hook returns,
    /// as the `Flush` command's arm does: V stops on it.
    pub fn flush_and_read_back(&mut self) -> Result<Option<StagedRecord>, Fatal> {
        let hash = self.hash;
        // As `ChainCommand::Flush`'s arm: the flush, then the class M scope's end.
        let staged =
            self.v.with_flush(|f, host| f.staged_record(&hash, host)).inspect_err(|f| {
                self.v.stage_fatal = Some(f.clone());
            })?;
        self.v.scope_end();
        Ok(staged)
    }
}

/// What [`BlockTailStage::flush_and_read_back`] read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedRecord {
    /// V's tip at the hook (the block's parent).
    pub tip: Option<BlockHash>,
    /// The block's `block_index` value in the database just before the flush.
    pub before: Option<Vec<u8>>,
    /// The epoch W released the ALWAYS flush at (`None` with no tip: no copy point).
    pub released_epoch: Option<u64>,
    /// The read-back snapshot's epoch.
    pub snapshot_epoch: u64,
    /// `meta.tip` in the snapshot.
    pub coins_tip: Option<BlockHash>,
    /// `meta.head_blocks` in the snapshot.
    pub head_blocks: Option<(BlockHash, Option<BlockHash>)>,
    /// The block's `block_index` value in the snapshot.
    pub record: Option<Vec<u8>>,
}
