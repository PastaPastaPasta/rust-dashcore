//! NetInstantSend's mempool half (PLAN-LK1b §6 S5) over MP1a-0's surface in `dashd_mempool::is`:
//! the accept-time answers ATMP asks under the pool lock, and `RemoveMempoolConflictsForLock`.
//! The listener's `TransactionAddedToMempool` and `TransactionRemovedFromMempool` halves are in
//! [`super::listener`].
//!
//! Lock order (MP1a §3): V turn, then the pool lock, then the manager's mutexes and the store's.
//! [`IsAcceptView`] runs under the pool lock and calls neither the pool nor V. Every
//! [`IsMempool`] call here is made with no manager, store or [`super::process::ResolvingLocks`]
//! mutex held: each manager call returns before it, and `ClearConflicting` runs after the pool
//! call returns, as Core releases `m_mempool.cs` before `ClearConflicting`
//! (`instantsend/net_instantsend.cpp:479-505`).

use std::sync::Arc;

use dash_consensus::Tx;
use dash_consensus::primitives::TxHash;
use dash_num::Hash256;
use dashd_mempool::is::{IsAcceptHooks, IsMempool};
use dashd_mempool::types::IsConflict;

use super::listener::IsFaults;
use super::manager::IsManager;
use super::process::clear_conflicting;
use super::seams::IsSignerSeam;
use super::{IsLockHash, LockedTx};

/// The store site a failed accept-time read names.
const ACCEPT: &str = "InstantSend AcceptToMemoryPool";

/// The mempool's [`IsAcceptHooks`]: `CChainstateHelper`'s InstantSend reads over the manager.
/// A store failure goes to [`IsFaults`] once and answers "no lock".
/// Core: `evo/chainhelper.cpp:79-91`.
pub struct IsAcceptView {
    pub manager: Arc<IsManager>,
    pub faults: Arc<dyn IsFaults>,
}

impl IsAcceptHooks for IsAcceptView {
    // Core: evo/chainhelper.cpp:87; instantsend/instantsend.cpp:513-516
    fn is_enabled(&self) -> bool {
        self.manager.is_instant_send_enabled()
    }

    /// `ConflictingISLockIfAny`, which ATMP rejects as `tx-txlock-conflict`
    /// (`validation.cpp:902-911`).
    // Core: evo/chainhelper.cpp:79-85; instantsend/instantsend.cpp:428-445
    fn conflicting_lock(&self, tx: &Tx) -> Option<IsConflict> {
        match self.manager.get_conflicting_lock(tx) {
            Ok(found) => found.map(|(hash, lock)| IsConflict {
                lock_hash: Hash256::from(hash),
                txid: lock.txid(),
            }),
            Err(e) => {
                self.faults.failed(ACCEPT, &e);
                None
            }
        }
    }

    // Core: evo/chainhelper.cpp:91; instantsend/instantsend.cpp:409-426
    fn is_waiting_for_tx(&self, txid: &TxHash) -> bool {
        self.manager.is_waiting_for_tx(txid)
    }

    // Core: evo/chainhelper.cpp:89; instantsend/instantsend.cpp:400-407
    fn is_locked(&self, txid: &TxHash) -> bool {
        self.manager.is_locked(txid).unwrap_or_else(|e| {
            self.faults.failed(ACCEPT, &e);
            false
        })
    }
}

/// `RemoveMempoolConflictsForLock(hash, islock)`: the pool's spenders of the lock's inputs that
/// are not its transaction, removed with their descendants (and the locked asset unlock's other
/// instances), then the spenders' tracking cleared.
// Core: instantsend/net_instantsend.cpp:479-505
pub(super) fn remove_mempool_conflicts_for_lock(
    mempool: &dyn IsMempool,
    manager: &IsManager,
    signer: Option<&Arc<dyn IsSignerSeam>>,
    hash: &IsLockHash,
    islock: &LockedTx,
) {
    let removed = mempool.remove_conflicts_for_lock(&islock.txid(), islock.inputs());
    for input in islock.inputs() {
        let spends = |tx: &&Arc<Tx>| tx.inputs().iter().any(|i| i.prevout() == *input);
        if let Some(tx) = removed.iter().find(spends) {
            tracing::info!(
                "RemoveMempoolConflictsForLock -- txid={}, mempool TX {} with input {}-{} \
                 conflicts with islock={}",
                islock.txid(),
                tx.txid(),
                input.txid(),
                input.index(),
                Hash256::from(*hash),
            );
        }
    }
    clear_conflicting(manager, signer, removed.iter());
}

#[cfg(test)]
#[path = "mempool_tests.rs"]
pub(super) mod tests;
