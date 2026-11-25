//! [`RelayRead`] for P3's G5 relay path, on a locked [`PoolState`] and on the [`Mempool`], where
//! each read takes one short read lock.

use std::cmp::Ordering;

use dash_consensus::{Tx, TxRef};
use dash_dml::{MnList, ProTxHash};
use dash_primitives::{OutPoint, TxHash};
use dashd_chainstate::chain::mempool_turn::{LockOrderGuard, LockRank};

use super::{Mempool, PoolEntry, PoolState};
use crate::api::{MempoolRead, RelayRead, TxMempoolInfo};
use crate::compact::walk_vtx_hashes;

// Core: txmempool.cpp:1563-1565 (GetInfo) at v24.0.0
fn relay_info(entry: &PoolEntry) -> TxMempoolInfo {
    TxMempoolInfo {
        tx: entry.tx.clone(),
        time_s: entry.metadata.time,
    }
}

impl RelayRead for PoolState {
    // Core: txmempool.cpp:1572-1579 at v24.0.0
    fn info(&self, txid: &TxHash) -> Option<TxMempoolInfo> {
        self.entries.get(txid).map(relay_info)
    }

    // Core: txmempool.cpp:1515-1561 at v24.0.0
    fn info_all(&self) -> Vec<TxMempoolInfo> {
        self.get_sorted_depth_and_score().iter().map(|id| relay_info(&self.entries[id])).collect()
    }

    /// Core's `bool` "a sooner than b" asked both ways: neither is sooner only for two absent
    /// txids or a txid and itself.
    // Core: txmempool.cpp:1495-1513 at v24.0.0
    fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> Ordering {
        if PoolState::compare_depth_and_score(self, a, b) {
            Ordering::Less
        } else if PoolState::compare_depth_and_score(self, b, a) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }

    // Core: net_processing.cpp:2996-3010 at v24.0.0
    fn parents_entered_after(&self, txid: &TxHash, since_s: i64) -> Vec<TxHash> {
        let Some(entry) = self.entries.get(txid) else {
            return Vec::new();
        };
        entry
            .get_mem_pool_parents_const()
            .iter()
            .filter(|parent| self.entries[*parent].metadata.time > since_s)
            .copied()
            .collect()
    }

    /// Only version-2+ unlocks are in `m_asset_unlock_instances` (`txmempool.cpp:677-684`).
    // Core: txmempool.cpp:852-858 at v24.0.0
    fn asset_unlock_by_instance_hash(&self, instance_hash: &TxHash) -> Option<TxHash> {
        PoolState::asset_unlock_by_instance_hash(self, *instance_hash)
            .filter(|txid| self.entries.contains_key(txid))
    }

    // Core: txmempool.h:785-790 at v24.0.0
    fn unbroadcast(&self) -> Vec<TxHash> {
        self.unbroadcast_txids.iter().copied().collect()
    }

    // Core: txmempool.h:702-706 at v24.0.0
    fn size(&self) -> usize {
        PoolState::size(self)
    }

    // Core: txmempool.cpp:1984-1988 at v24.0.0
    fn dynamic_memory_usage(&self) -> usize {
        PoolState::dynamic_memory_usage(self)
    }

    // Core: blockencodings.cpp:112-139 at v24.0.0
    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
        walk_vtx_hashes(self, f);
    }
}

impl Mempool {
    /// `f` under one short read lock (`LOCK(cs)` in each of Core's const accessors).
    pub(crate) fn short_read<R>(&self, f: impl FnOnce(&PoolState) -> R) -> R {
        let _order = LockOrderGuard::enter(LockRank::Mempool);
        f(&self.read())
    }
}

impl MempoolRead for Mempool {
    fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.short_read(|s| s.get(txid))
    }
    fn exists(&self, txid: &TxHash) -> bool {
        self.short_read(|s| s.exists(txid))
    }
    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef> {
        self.short_read(|s| s.get_conflict_tx(outpoint))
    }
    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash> {
        self.short_read(|s| s.asset_unlock_txids_by_index(index))
    }
    fn pending_asset_unlock_amount(&self) -> i64 {
        self.short_read(PoolState::pending_asset_unlock_amount)
    }
    fn service_updates_before_key_change(
        &self,
        key_change: &TxHash,
        tip_list: &MnList,
    ) -> Vec<TxHash> {
        self.short_read(|s| s.service_updates_before_key_change(*key_change, tip_list))
    }
    fn is_unorderable_service_update(
        &self,
        tx: &Tx,
        ancestors: &[TxHash],
        tip_list: &MnList,
    ) -> bool {
        self.short_read(|s| s.is_unorderable_service_update(tx, ancestors, tip_list))
    }
    fn key_change_target(&self, tx: &Tx, tip_list: &MnList) -> Option<ProTxHash> {
        self.short_read(|s| s.key_change_target(tx, tip_list))
    }
}

impl RelayRead for Mempool {
    fn info(&self, txid: &TxHash) -> Option<TxMempoolInfo> {
        self.short_read(|s| RelayRead::info(s, txid))
    }
    fn info_all(&self) -> Vec<TxMempoolInfo> {
        self.short_read(RelayRead::info_all)
    }
    fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> Ordering {
        self.short_read(|s| RelayRead::compare_depth_and_score(s, a, b))
    }
    fn parents_entered_after(&self, txid: &TxHash, since_s: i64) -> Vec<TxHash> {
        self.short_read(|s| s.parents_entered_after(txid, since_s))
    }
    fn asset_unlock_by_instance_hash(&self, instance_hash: &TxHash) -> Option<TxHash> {
        self.short_read(|s| RelayRead::asset_unlock_by_instance_hash(s, instance_hash))
    }
    fn unbroadcast(&self) -> Vec<TxHash> {
        self.short_read(RelayRead::unbroadcast)
    }
    fn size(&self) -> usize {
        self.short_read(PoolState::size)
    }
    fn dynamic_memory_usage(&self) -> usize {
        self.short_read(PoolState::dynamic_memory_usage)
    }
    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
        self.short_read(|s| walk_vtx_hashes(s, f));
    }
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
