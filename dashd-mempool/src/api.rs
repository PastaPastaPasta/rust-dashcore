//! Read access within a single mempool lock hold.

use dash_consensus::TxRef;
use dash_primitives::{OutPoint, TxHash};

/// A borrowed pool view, already read-locked by [`IsMempool::with_read`](crate::is::IsMempool::with_read).
/// Callers must not re-enter the pool, wait for V, or hold its lock across an await.
pub trait MempoolRead {
    /// Returns a shared transaction (`src/txmempool.cpp`, `get`, Dash Core v24.0.0).
    fn get(&self, txid: &TxHash) -> Option<TxRef>;

    fn exists(&self, txid: &TxHash) -> bool {
        self.get(txid).is_some()
    }

    /// Returns the `mapNextTx` spender (`src/txmempool.cpp`, `GetConflictTx`, Dash Core v24.0.0).
    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef>;

    /// Returns withdrawals claiming an index (`src/txmempool.cpp`, `GetAssetUnlockTxidsByIndex`, v24.0.0).
    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash>;

    /// Returns CAmount capped at i64::MAX (Core v24.0.0 `src/txmempool.h`, `GetPendingAssetUnlockAmount`).
    /// Internal bookkeeping retains the exact total separately, using 256 bits or a proven bound.
    fn pending_asset_unlock_amount(&self) -> i64;
}
