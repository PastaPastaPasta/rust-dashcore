//! Synchronous interfaces between the mempool and InstantSend.

use dash_consensus::{Tx, TxRef};
use dash_primitives::{OutPoint, TxHash};

use crate::api::MempoolRead;
use crate::types::IsConflict;

/// Supplies IS state during acceptance, expiry and mining.
/// Methods answer their own enabled/gating state under the pool lock; ATMP adds no IBD gate.
pub trait IsAcceptHooks: Send + Sync {
    /// Returns `IsInstantSendEnabled` (`src/evo/chainhelper.cpp`, Dash Core v24.0.0).
    /// Answers its own gating state; never re-enters the pool lock or V.
    fn is_enabled(&self) -> bool;

    /// Returns a lock on a lock input for another txid (`src/instantsend/instantsend.cpp`,
    /// `GetConflictingLock`, Dash Core v24.0.0).
    /// Answers its own gating state; never re-enters the pool lock or V.
    fn conflicting_lock(&self, tx: &Tx) -> Option<IsConflict>;

    /// Returns `IsInstantSendWaitingForTx` (`src/evo/chainhelper.cpp`, Dash Core v24.0.0).
    /// Answers its own gating state; never re-enters the pool lock or V.
    fn is_waiting_for_tx(&self, txid: &TxHash) -> bool;

    /// Returns `IsInstantSendLocked` (`src/evo/chainhelper.cpp`, Dash Core v24.0.0).
    /// Answers its own gating state; never re-enters the pool lock or V.
    fn is_locked(&self, txid: &TxHash) -> bool;
}

/// Supplies the answers for disabled InstantSend: no locks and no waiting transactions.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoIsHooks;

impl IsAcceptHooks for NoIsHooks {
    fn is_enabled(&self) -> bool {
        false
    }

    fn conflicting_lock(&self, _: &Tx) -> Option<IsConflict> {
        None
    }

    fn is_waiting_for_tx(&self, _: &TxHash) -> bool {
        false
    }

    fn is_locked(&self, _: &TxHash) -> bool {
        false
    }
}

/// Supplies the mempool operations used by InstantSend.
/// Release IS manager locks before calling mutators; lock order is V, pool, then IS manager.
pub trait IsMempool: Send + Sync {
    /// Returns a shared transaction (`src/txmempool.cpp`, `get`, Dash Core v24.0.0).
    /// Never re-enters the pool lock or V.
    fn get(&self, txid: &TxHash) -> Option<TxRef>;

    /// Tests whether a transaction is present.
    /// Never re-enters the pool lock or V.
    fn exists(&self, txid: &TxHash) -> bool;

    /// Returns the `mapNextTx` spender (`src/txmempool.cpp`, `GetConflictTx`, Dash Core v24.0.0).
    /// Never re-enters the pool lock or V.
    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef>;

    /// Returns `GetAssetUnlockTxidsByIndex` (`src/txmempool.cpp`, Dash Core v24.0.0).
    /// Never re-enters the pool lock or V.
    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash>;

    /// Returns CAmount capped at i64::MAX (Core v24.0.0 `src/txmempool.h`, `GetPendingAssetUnlockAmount`).
    /// Internal bookkeeping must retain the exact total, using 256 bits or a proven bound.
    /// Never re-enters the pool lock or V.
    fn pending_asset_unlock_amount(&self) -> i64;

    /// Under one write lock, snapshots and deduplicates all input spenders whose txid differs from `txid`,
    /// then removes each snapshot root with removeRecursive(CONFLICT), then removes asset-unlock
    /// conflicts of the locked transaction still held in the pool. Returns the original spender
    /// snapshot, including roots removed as descendants of other roots; AU-only removals are excluded.
    /// Core v24.0.0 `src/instantsend/net_instantsend.cpp`, `RemoveMempoolConflictsForLock`.
    /// Never re-enters the pool lock or V.
    fn remove_conflicts_for_lock(&self, txid: &TxHash, inputs: &[OutPoint]) -> Vec<TxRef>;

    /// Increments `nTransactionsUpdated` (`src/txmempool.cpp`, `AddTransactionsUpdated`, v24.0.0).
    /// Never re-enters the pool lock or V.
    fn add_transactions_updated(&self, n: u32);

    /// Runs `f` under one read lock, for `CheckCanLock` within the caller's V turn.
    /// Do not re-enter the pool, wait for V, or hold the lock across an await.
    /// Never re-enters the pool lock or V.
    fn with_read(&self, f: &mut dyn FnMut(&dyn MempoolRead));
}
