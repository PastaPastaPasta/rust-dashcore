//! [`IsMempool`] on the real pool: what InstantSend asks of the mempool, each under one lock.

use dash_consensus::TxRef;
use dash_primitives::{OutPoint, TxHash};
use dashd_chainstate::chain::mempool_turn::{LockOrderGuard, LockRank};

use crate::api::MempoolRead;
use crate::events;
use crate::is::IsMempool;
use crate::pool::Mempool;

impl IsMempool for Mempool {
    fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.read().get(txid)
    }
    fn exists(&self, txid: &TxHash) -> bool {
        self.read().exists(txid)
    }
    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef> {
        self.read().get_conflict_tx(outpoint)
    }
    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash> {
        self.read().asset_unlock_txids_by_index(index)
    }
    fn pending_asset_unlock_amount(&self) -> i64 {
        self.read().pending_asset_unlock_amount()
    }

    /// The locked section of `RemoveMempoolConflictsForLock`, off V under `m_pool.cs` alone: the
    /// spender snapshot, each root's `removeRecursive(CONFLICT)`, then
    /// `removeAssetUnlockConflicts` of the locked transaction if held. The removals are queued
    /// before the guard is released.
    // Core: instantsend/net_instantsend.cpp:474-505 at v24.0.0
    fn remove_conflicts_for_lock(&self, txid: &TxHash, inputs: &[OutPoint]) -> Vec<TxRef> {
        let _order = LockOrderGuard::enter(LockRank::Mempool);
        let mut state = self.write();
        let roots = state.remove_conflicts_for_lock(txid, inputs);
        events::enqueue(&mut state, self.signals());
        self.publish_usage(&state);
        roots
    }

    fn add_transactions_updated(&self, n: u32) {
        Mempool::add_transactions_updated(self, n);
    }

    fn with_read(&self, f: &mut dyn FnMut(&dyn MempoolRead)) {
        let _order = LockOrderGuard::enter(LockRank::Mempool);
        f(&*self.read());
    }
}
