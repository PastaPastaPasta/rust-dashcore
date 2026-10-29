//! The effect channel's application (CH7 §3.8, D12): one applier per `EffectKind`, all required at
//! construction, applied in the attempt log's emission order on every outcome. The appliers whose
//! owners have not landed are stand-ins (Q5, DEC-581).

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_evo::Effect;
use dash_evo::llmq::{LlmqCommitment, QuorumSnapshot};
use dash_evo::types::ProTxHash;
use dash_num::Hash256;
use dash_primitives::{BlockHash, LlmqType, QuorumHash};
use dashd_store::KvEngine;

use super::store::RocksEvoStore;
use crate::chain::Fatal;
use crate::flush::EffectLedger;

/// `StoreSnapshotForBlock` (`snap:322-338`): a presumed effect (§3.8).
pub trait SnapshotApplier: Send {
    fn write(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), Fatal>;
}

/// `RemoveConflictingISLockByTx`'s removal (`v:2852-2872`): LK1b's IS store.
pub trait IsLockApplier: Send {
    fn remove(&mut self, lock_hash: &Hash256) -> Result<(), Fatal>;
}

/// The mineable pool's erase (`bp:490-496`) and re-add (`bp:600`, `AddMineableCommitment`):
/// CH7b's pool host (D14).
pub trait MineableApplier: Send {
    fn erase(
        &mut self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        commitment_hash: &[u8; 32],
    ) -> Result<(), Fatal>;

    fn reinsert(&mut self, commitment: &Arc<LlmqCommitment>) -> Result<(), Fatal>;
}

/// `ResetPlatformBan` (`dmn:737-752`): SP1's mncache meta store (BL-425).
pub trait PlatformBanApplier: Send {
    fn reset(&mut self, pro_tx_hash: &ProTxHash, height: i32) -> Result<(), Fatal>;
}

/// The MN-list signal of a VerifyDB attempt (A1 §2.10.1 row 6; `v:2391-2395`, `v:2972-2976`).
pub trait NotifyApplier: Send {
    fn notify(&mut self, at: &BlockHash, undo: bool) -> Result<(), Fatal>;
}

/// Proof that an attempt's whole log was applied (EV addendum §2.6): minted only by
/// [`EffectAppliers::apply_log`], so a release cannot follow a partial application.
#[derive(Debug)]
pub struct EffectsApplied {
    _sealed: (),
}

/// One applier per `EffectKind` (D12). A missing owner is a construction error, not a runtime
/// refusal.
pub struct EffectAppliers {
    pub snapshot: Box<dyn SnapshotApplier>,
    pub is_lock: Box<dyn IsLockApplier>,
    pub mineable: Box<dyn MineableApplier>,
    pub platform_ban: Box<dyn PlatformBanApplier>,
    pub notify: Box<dyn NotifyApplier>,
}

impl EffectAppliers {
    /// One effect, by its owner. Exhaustive: a new `Effect` variant does not compile here until it
    /// has an applier.
    ///
    /// # Errors
    ///
    /// The applier's fault: D5's store fault, `AbortNode` (§3.8).
    pub fn apply(&mut self, e: &Effect) -> Result<(), Fatal> {
        match e {
            Effect::StoreQuorumSnapshot {
                llmq_type,
                cycle_base,
                snapshot,
            } => self.snapshot.write(*llmq_type, cycle_base, snapshot),
            Effect::RemoveConflictingIsLock {
                lock_hash,
            } => self.is_lock.remove(lock_hash),
            Effect::EraseMineableCommitment {
                llmq_type,
                quorum_hash,
                commitment_hash,
            } => self.mineable.erase(*llmq_type, quorum_hash, commitment_hash),
            Effect::ReinsertMineableCommitment {
                commitment,
            } => self.mineable.reinsert(commitment),
            Effect::ResetPlatformBan {
                pro_tx_hash,
                height,
            } => self.platform_ban.reset(pro_tx_hash, *height),
            Effect::NotifyMnListChanged {
                at,
                undo,
            } => self.notify.notify(at, *undo),
        }
    }

    /// The attempt's log in emission order (D12), every outcome's.
    ///
    /// # Errors
    ///
    /// The first applier fault; the effects after it are not applied, and no [`EffectsApplied`]
    /// is minted, so the attempt is not released (§3.8).
    pub fn apply_log(&mut self, log: &[Effect]) -> Result<EffectsApplied, Fatal> {
        for e in log {
            self.apply(e)?;
        }
        Ok(EffectsApplied {
            _sealed: (),
        })
    }
}

/// The production snapshot applier (§6.2, DEC-581): the row as a Buffered single-row batch, then
/// the presumed-effect ledger, so the next B1 syncs the WAL first (CH2 §4.1).
pub struct StoreSnapshots<E: KvEngine> {
    pub store: Arc<RocksEvoStore<E>>,
    pub ledger: Arc<EffectLedger>,
}

impl<E: KvEngine> SnapshotApplier for StoreSnapshots<E> {
    fn write(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), Fatal> {
        self.store
            .store_quorum_snapshot(llmq_type, cycle_base, snapshot)
            // The guarded engine has tripped the latch, so V does not report it twice.
            .map_err(|e| Fatal::new(e.to_string()))?;
        self.ledger.record_presumed();
        Ok(())
    }
}

/// The IS-lock applier until LK1b (Q5): `NodeHelper::is_conflict` reports no conflict before it,
/// so `RemoveConflictingIsLock` is never emitted; one that is fails closed.
pub struct UnreachableIsLock;

impl IsLockApplier for UnreachableIsLock {
    fn remove(&mut self, _: &Hash256) -> Result<(), Fatal> {
        Err(Fatal::new("RemoveConflictingIsLock without InstantSend (LK1b)"))
    }
}

/// The mineable applier until CH7b (Q5): only a processed or undone non-null commitment emits
/// one, and `NoMembers` refuses every non-null commitment first (D2); one emitted fails closed.
pub struct UnreachableMineable;

impl MineableApplier for UnreachableMineable {
    fn erase(&mut self, _: LlmqType, _: &QuorumHash, _: &[u8; 32]) -> Result<(), Fatal> {
        Err(Fatal::new("EraseMineableCommitment without the mineable pool (CH7b)"))
    }

    fn reinsert(&mut self, _: &Arc<LlmqCommitment>) -> Result<(), Fatal> {
        Err(Fatal::new("ReinsertMineableCommitment without the mineable pool (CH7b)"))
    }
}

/// The notify applier until CH7a-3: only a VerifyDB attempt emits the effect, and that origin
/// comes with CH7a-3; one emitted fails closed.
pub struct UnreachableNotify;

impl NotifyApplier for UnreachableNotify {
    fn notify(&mut self, _: &BlockHash, _: bool) -> Result<(), Fatal> {
        Err(Fatal::new("NotifyMnListChanged without VerifyDB's listeners (CH7a-3)"))
    }
}

/// The platform-ban state until SP1 (Q5): in memory, from empty at each start, as SP1's store
/// starts when it replaces it. Every ProUpServTx in a real connect emits a reset (`dmn:737-752`).
#[derive(Debug, Default)]
pub struct MemPlatformBans {
    /// Each masternode's last reset height.
    pub resets: BTreeMap<ProTxHash, i32>,
}

impl PlatformBanApplier for MemPlatformBans {
    fn reset(&mut self, pro_tx_hash: &ProTxHash, height: i32) -> Result<(), Fatal> {
        self.resets.insert(*pro_tx_hash, height);
        Ok(())
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
