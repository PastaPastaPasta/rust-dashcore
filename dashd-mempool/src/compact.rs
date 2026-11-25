//! The `vTxHashes` walk a compact block's `InitData` makes over the pool (P5 wraps it as
//! dashd-p2p's `CompactTxSource`).

use dash_consensus::TxRef;
use dash_primitives::TxHash;

use crate::pool::{Mempool, PoolState};

impl Mempool {
    /// Calls `f(instance_hash, tx)` in `vTxHashes` order, under one read lock, until it returns
    /// `false`. The order is insertion order with swap-remove (`txmempool.cpp:475-476`,
    /// `:726-733`); an asset unlock whose instance is replaced keeps its slot, rekeyed
    /// (`:871`).
    // Core: blockencodings.cpp:112-139 at v24.0.0
    pub fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
        walk_vtx_hashes(&self.read(), f);
    }
}

/// [`Mempool::for_each_in_vtxhashes_order`]'s walk over an already locked pool.
pub(crate) fn walk_vtx_hashes(state: &PoolState, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
    for (instance, txid) in state.vtx_hashes() {
        let entry = state.get_iter(txid).expect("vTxHashes names a resident entry");
        if !f(instance, entry.tx()) {
            break;
        }
    }
}
