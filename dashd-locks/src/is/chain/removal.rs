//! `RemoveConflictingISLockByTx`: the conflict found again, staged as Core's chained removal,
//! observed on the attempt's ledger and emitted as the attempt's effect.
//!
//! A store fault on the way stages, observes and emits nothing. It is kept as the helper's
//! pending fault, which the IS loop's next `is_conflict` returns once (`ChainHelper`'s removal
//! answers no `Result`).
//!
//! Core's two `RemoveConflictingLock` log lines are kept. The first comes before staging, so a
//! staging fault follows it as Core's throw follows it; a second-lookup fault logs nothing.

use dash_consensus::primitives::TxHash;
use dash_evo::{Effect, EffectSink};
use dash_num::Hash256;
use dashd_chainstate::chain::Fatal;
use dashd_chainstate::connect::ChainHelper;

use super::IsHelper;
use crate::is::IsLockHash;
use crate::is::seams::{IsObservation, StageId};

/// One staged conflict removal, as the attempt's ledger observes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsRemoval {
    /// The removed root lock's hash.
    pub lock_hash: IsLockHash,
    /// The block transaction that overrides the lock.
    pub overriding_txid: TxHash,
    /// The root lock's transaction.
    pub locked_txid: TxHash,
    /// The height the removed locks are archived at: the cached tip.
    pub archive_height: i32,
    /// Whether the root lock was still stored when the removal read it.
    pub root_stored: bool,
    /// Every removed hash in Core's order, repeats included, the root last.
    pub removed: Vec<IsLockHash>,
    /// The overlay stage holding the removal's batch.
    pub stage: StageId,
}

/// The kept fault's text for a removal no `is_conflict` found a lock for.
pub(crate) const NO_PENDING_TX: &str = "IS removal without a conflicting transaction";

impl From<&IsRemoval> for IsObservation {
    fn from(removal: &IsRemoval) -> Self {
        IsObservation {
            lock_hash: removal.lock_hash,
            overriding_txid: removal.overriding_txid,
            locked_txid: removal.locked_txid,
            archive_height: removal.archive_height,
            root_stored: removal.root_stored,
            removed: removal.removed.clone(),
            stage: removal.stage,
        }
    }
}

impl<H: ChainHelper> IsHelper<H> {
    /// Core's `RemoveConflictingISLockByTx` for the pending transaction: `None` when the second
    /// lookup finds no lock or a store fault was kept.
    // Core: evo/chainhelper.cpp:93-99; instantsend/instantsend.cpp:342-353
    pub(crate) fn remove_conflicting(&mut self, sink: &mut dyn EffectSink) -> Option<IsRemoval> {
        let Some(tx) = self.pending_tx.clone() else {
            self.pending_fault = Some(Fatal::new(NO_PENDING_TX));
            return None;
        };
        let (lock_hash, root) = match self.live_conflict(&tx) {
            Ok(found) => found?,
            Err(fault) => {
                self.pending_fault = Some(fault);
                return None;
            }
        };
        let shown = Hash256::from(lock_hash);
        tracing::info!(
            "CInstantSendManager::RemoveConflictingLock -- txid={}, islock={shown}: Removing \
             ISLOCK and its chained children",
            root.txid()
        );
        let archive_height = self.tip.get();
        let mut ledger = self.ledger.lock();
        let staged =
            self.store.stage_remove_chained(ledger.overlay(), &lock_hash, &root, archive_height);
        let (chained, stage) = match staged {
            Ok(staged) => staged,
            Err(e) => {
                self.pending_fault = Some(Fatal::new(e.to_string()));
                return None;
            }
        };
        for removed in &chained.hashes {
            tracing::info!(
                "CInstantSendManager::RemoveConflictingLock -- txid={}, islock={shown}: removed \
                 (child) ISLOCK {}",
                root.txid(),
                Hash256::from(*removed)
            );
        }
        let removal = IsRemoval {
            lock_hash,
            overriding_txid: tx.txid(),
            locked_txid: root.txid(),
            archive_height,
            root_stored: chained.root_present,
            removed: chained.hashes,
            stage,
        };
        ledger.push(IsObservation::from(&removal));
        drop(ledger);
        sink.emit(Effect::RemoveConflictingIsLock {
            lock_hash: lock_hash.into(),
        });
        Some(removal)
    }
}
