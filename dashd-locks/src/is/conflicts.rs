//! InstantSend's half of `NetInstantSend::ResolveBlockConflicts` (PLAN-LK1b §6 S4, D42): V's
//! [`IsConflicts`] over S2's non-locked graph and the IS store.
//!
//! V calls it inline in its `IsConflictInvalidate` turn (LO-5): it never waits on V or sends a
//! `ChainCommand`. It reads the lock object from [`ResolvingLocks`], which the waiting processor
//! filled. Locks: each manager call takes and releases its own mutexes, and the chained removal
//! takes the store's state and attempt gate with no manager mutex held, as Core takes `cs_db`
//! under no `cs_nonLocked` (`instantsend/instantsend.cpp:342-353`).

use std::sync::Arc;

use dash_consensus::Tx;
use dash_num::Hash256;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::IsConflicts;

use super::listener::IsFaults;
use super::manager::{IsManager, MinedBlock};
use super::process::{ResolvingLocks, clear_conflicting};
use super::seams::{IsSignerSeam, IsStore};
use super::{IsLockHash, IsTipCache, LockedTx};

/// The store site a failed removal names.
const REMOVE: &str = "InstantSend RemoveConflictingLock";

/// What [`IsBlockConflicts`] is built from.
pub struct IsConflictsParts {
    pub store: Arc<dyn IsStore>,
    pub manager: Arc<IsManager>,
    pub tip: IsTipCache,
    /// `m_signer`: none on a node that is not a masternode.
    pub signer: Option<Arc<dyn IsSignerSeam>>,
    pub resolving: Arc<ResolvingLocks>,
    pub faults: Arc<dyn IsFaults>,
}

/// V's [`IsConflicts`]: `RetrieveISConflicts`, `HasTxForLock`, `ClearConflicting` and
/// `RemoveConflictingLock`. Core: `instantsend/net_instantsend.cpp:584-643`.
pub struct IsBlockConflicts {
    parts: IsConflictsParts,
    /// The last `RetrieveISConflicts` answer, which `ClearConflicting` reads per block.
    conflicts: Vec<(MinedBlock, Vec<Arc<Tx>>)>,
}

impl IsBlockConflicts {
    /// V's InstantSend half over `parts`.
    pub fn new(parts: IsConflictsParts) -> Self {
        IsBlockConflicts {
            parts,
            conflicts: Vec::new(),
        }
    }

    fn resolving(&self, lock: &Hash256) -> Option<(IsLockHash, Arc<LockedTx>)> {
        let hash = *lock.as_bytes();
        let found = self.parts.resolving.get(&hash);
        if found.is_none() {
            tracing::error!(
                "NetInstantSend::ResolveBlockConflicts -- islock={lock}: not being processed"
            );
        }
        Some((hash, found?))
    }
}

impl IsConflicts for IsBlockConflicts {
    // Core: instantsend/instantsend.cpp:304-334
    fn retrieve_conflicts(&mut self, lock: &Hash256) -> Vec<BlockHash> {
        self.conflicts = match self.resolving(lock) {
            Some((hash, islock)) => self.parts.manager.retrieve_is_conflicts(&hash, &islock),
            None => Vec::new(),
        };
        self.conflicts.iter().map(|(block, _)| block.hash).collect()
    }

    // Core: instantsend/instantsend.cpp:336-340
    fn has_tx_for_lock(&mut self, lock: &Hash256) -> bool {
        self.parts.manager.has_tx_for_lock(lock.as_bytes())
    }

    // Core: instantsend/net_instantsend.cpp:614
    fn clear_conflicting(&mut self, _lock: &Hash256, block: &BlockHash) {
        if let Some((_, txs)) = self.conflicts.iter().find(|(mined, _)| mined.hash == *block) {
            clear_conflicting(&self.parts.manager, self.parts.signer.as_ref(), txs.iter());
        }
    }

    /// The lock and every lock chained on it removed by the free `remove_conflicting_lock`. A
    /// store failure goes to [`IsFaults`] once.
    fn remove_conflicting_lock(&mut self, lock: &Hash256) {
        let Some((hash, islock)) = self.resolving(lock) else {
            return;
        };
        let p = &self.parts;
        if let Err(e) = remove_conflicting_lock(&*p.store, &p.tip, &hash, &islock) {
            p.faults.failed(REMOVE, &e);
        }
    }
}

/// `RemoveConflictingLock(hash, islock)`: the lock and every lock chained on it removed and
/// archived at the cached tip, written at once (no chain attempt's stage is involved).
// Core: instantsend/instantsend.cpp:342-353
pub(super) fn remove_conflicting_lock(
    store: &dyn IsStore,
    tip: &IsTipCache,
    hash: &IsLockHash,
    islock: &LockedTx,
) -> dash_kv::Result<()> {
    let lock = Hash256::from(*hash);
    tracing::info!(
        "CInstantSendManager::RemoveConflictingLock -- txid={}, islock={lock}: Removing ISLOCK \
         and its chained children",
        islock.txid(),
    );
    for h in store.remove_chained(hash, islock, tip.get())?.hashes {
        tracing::info!(
            "CInstantSendManager::RemoveConflictingLock -- txid={}, islock={lock}: removed \
             (child) ISLOCK {}",
            islock.txid(),
            Hash256::from(h),
        );
    }
    Ok(())
}

#[cfg(all(test, feature = "test-support"))]
#[path = "conflicts_tests.rs"]
mod tests;
