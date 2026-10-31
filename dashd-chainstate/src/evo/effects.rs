//! The effect channel's application (CH7 §3.8, D12): one applier per `EffectKind`, all required at
//! construction, applied in the attempt log's emission order on every outcome. The appliers whose
//! owners have not landed are stand-ins (Q5, DEC-581).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use dash_dml::{MnList, MnListChange, MnListDiff};
use dash_evo::Effect;
use dash_evo::llmq::{LlmqCommitment, QuorumSnapshot};
use dash_evo::types::ProTxHash;
use dash_num::Hash256;
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};
use dashd_store::KvEngine;

use super::store::RocksEvoStore;
use crate::chain::{Fatal, ValidationSignals};
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

/// The MN-list signal `NodeEvo` raises (CH7 D15) on the node's [`ValidationSignals`]: H7 runs
/// the listeners of an Active attempt inline; a VerifyDB attempt's signal goes through the effect
/// log (§3.8), its lists queued here for [`SignalsNotify`] to dispatch. `Effect` carries only the
/// block and the direction, as `dash-evo` cannot name `dash-dml`'s lists (CH7-notes D61).
#[derive(Clone)]
#[cfg_attr(test, derive(Default))]
pub struct MnListSignal {
    signals: ValidationSignals,
    queued: Arc<Mutex<VecDeque<Notice>>>,
}

/// A queued VerifyDB signal: its effect's fields and the lists it carries.
struct Notice {
    at: BlockHash,
    undo: bool,
    old: Arc<MnList>,
    diff: Arc<MnListDiff>,
}

impl MnListSignal {
    pub fn new(signals: ValidationSignals) -> Self {
        Self {
            signals,
            queued: Arc::default(),
        }
    }

    /// The applier for [`EffectAppliers::notify`].
    pub fn applier(&self) -> SignalsNotify {
        SignalsNotify(self.clone())
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, VecDeque<Notice>> {
        // Pushes and pops are whole: a panic cannot leave one half-done.
        self.queued.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drops the notices of a log that was never applied: a new attempt's log starts empty.
    pub(crate) fn reset(&self) {
        self.queue().clear();
    }

    /// An Active attempt's signal, inline.
    pub(crate) fn notify(&self, undo: bool, change: &MnListChange) {
        self.signals.notify_masternode_list_changed(undo, &change.old, &change.diff);
    }

    /// A VerifyDB attempt's signal: queues its lists and returns the effect that dispatches
    /// them.
    pub(crate) fn defer(&self, at: BlockHash, undo: bool, change: &MnListChange) -> Effect {
        self.queue().push_back(Notice {
            at,
            undo,
            old: Arc::clone(&change.old),
            diff: Arc::clone(&change.diff),
        });
        Effect::NotifyMnListChanged {
            at,
            undo,
        }
    }
}

/// The notify applier: a VerifyDB attempt's signal to the listeners registered when its log is
/// applied, which for startup's VerifyDB are none (D15; Core registers `CDSNotificationInterface`
/// at `init.cpp:2204`, after `VerifyLoadedChainstate` at `:2106`).
pub struct SignalsNotify(MnListSignal);

impl NotifyApplier for SignalsNotify {
    /// # Errors
    ///
    /// An effect with no queued lists, or another block's: the log and the queue disagree.
    fn notify(&mut self, at: &BlockHash, undo: bool) -> Result<(), Fatal> {
        let notice = self.0.queue().pop_front();
        let Some(n) = notice.filter(|n| n.at == *at && n.undo == undo) else {
            return Err(Fatal::new("NotifyMnListChanged without its queued lists (D15)"));
        };
        self.0.signals.notify_masternode_list_changed(n.undo, &n.old, &n.diff);
        Ok(())
    }
}

/// A platform-ban applier for this crate's tests: it records each reset it is handed and applies
/// none. Core's rule (`ResetPlatformBan` only for a masternode with an entry, `SetPlatformBan`'s
/// heights; `meta.cpp:187-195`, `meta.h:73-84`) has one port, SP1's masternode meta store, which
/// the node applies resets to (`dashd-node`'s `MetaPlatformBans`, BL-425; CH7-notes D55). Every
/// ProUpServTx in a real connect emits a reset (`dmn:737-752`).
#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub struct MemPlatformBans {
    /// Every reset handed over, with its masternode and height; the clones share it.
    resets: Arc<std::sync::Mutex<Vec<(ProTxHash, i32)>>>,
}

#[cfg(test)]
impl MemPlatformBans {
    /// The resets handed over so far, in order.
    pub fn resets(&self) -> Vec<(ProTxHash, i32)> {
        self.resets.lock().expect("resets").clone()
    }
}

#[cfg(test)]
impl PlatformBanApplier for MemPlatformBans {
    fn reset(&mut self, pro_tx_hash: &ProTxHash, height: i32) -> Result<(), Fatal> {
        self.resets.lock().expect("resets").push((*pro_tx_hash, height));
        Ok(())
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
