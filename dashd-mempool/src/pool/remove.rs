//! Core v24.0.0 `txmempool.cpp:706-747,883-989,1999-2005`.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::{Tx, TxRef};
use dash_primitives::{OutPoint, TxHash};

use crate::types::MemPoolRemovalReason;

use super::index::AncestorScore;
use super::{PendingEvent, PoolState, usage};

impl PoolState {
    /// Includes the root if held; a missing root has no descendants through cached links.
    pub fn calculate_descendants(&self, id: TxHash) -> BTreeSet<TxHash> {
        let mut descendants = BTreeSet::new();
        let mut staged = BTreeSet::new();
        if self.entries.contains_key(&id) {
            staged.insert(id);
        }
        while let Some(id) = staged.pop_first() {
            descendants.insert(id);
            staged.extend(self.entries[&id].children.difference(&descendants).copied());
        }
        descendants
    }

    pub fn remove_recursive(&mut self, tx: &Tx, reason: MemPoolRemovalReason) {
        let roots = if self.entries.contains_key(&tx.txid()) {
            BTreeSet::from([tx.txid()])
        } else {
            (0..tx.outputs().len())
                .filter_map(|n| self.next_tx.get(&OutPoint::new(tx.txid(), n as u32)).copied())
                .collect()
        };
        let mut removed = BTreeSet::new();
        for root in roots {
            removed.extend(self.calculate_descendants(root));
        }
        self.remove_staged(&removed, false, reason);
    }

    pub fn remove_conflicts(&mut self, tx: &Tx) {
        for input in tx.inputs() {
            let Some(conflict) = self.get_conflict_tx(&input.prevout()) else {
                continue;
            };
            if conflict.txid() != tx.txid() {
                self.remove_dash_conflict_references(&conflict);
                self.clear_prioritisation(&conflict.txid());
                self.remove_recursive(&conflict, MemPoolRemovalReason::Conflict);
            }
        }
    }

    /// The caller supplies a descendant-closed set unless update_descendants is true.
    pub fn remove_staged(
        &mut self,
        removed: &BTreeSet<TxHash>,
        update_descendants: bool,
        reason: MemPoolRemovalReason,
    ) {
        assert!(removed.iter().all(|id| self.entries.contains_key(id)));
        if !update_descendants {
            assert!(removed.iter().all(|id| self.entries[id].children.is_subset(removed)));
        }
        self.update_for_remove_from_mempool(removed, update_descendants);
        for id in removed {
            self.remove_unchecked(*id, reason);
        }
    }

    /// Links and package statistics must already have been updated by RemoveStaged.
    pub fn remove_unchecked(&mut self, id: TxHash, reason: MemPoolRemovalReason) {
        let sequence = self.get_and_increment_sequence();
        let entry = &self.entries[&id];
        if reason != MemPoolRemovalReason::Block {
            self.pending_events.push(PendingEvent::TransactionRemovedFromMempool {
                tx: entry.tx.clone(),
                reason,
                sequence,
            });
        }
        for input in entry.tx.inputs() {
            assert_eq!(self.next_tx.remove(&input.prevout()), Some(id));
            self.usage -= usage::NEXT_TX_ALLOCATION;
        }
        let idx = entry.vtx_hashes_idx;
        self.remove_unbroadcast_tx(&id, true);
        self.vtx_hashes.swap_remove(idx);
        if let Some((_, moved)) = self.vtx_hashes.get(idx).copied() {
            self.modify_entry(moved, |entry| entry.vtx_hashes_idx = idx);
        }
        // Core keeps the last allocation when clearing a singleton vector.
        if !self.vtx_hashes.is_empty() && self.vtx_hashes.len() * 2 < self.vtx_capacity {
            self.resize_hashes_usage(self.vtx_hashes.len());
        }
        self.on_dash_remove(id);
        let entry = self.entries.remove(&id).expect("removed entry must exist");
        assert!(self.ancestor_score.remove(&AncestorScore::from(&entry)));
        for index in &mut self.indexes {
            index.on_remove(&entry);
        }
        self.total_vsize -= entry.vsize;
        self.total_fee -= entry.metadata.fee;
        self.usage -= usage::ENTRY_ALLOCATION
            + entry.usage
            + usage::LINK_ALLOCATION * (entry.parents.len() + entry.children.len());
        self.add_transactions_updated(1);
    }

    /// The original roots survive in this result even if an earlier root removes a later one.
    pub fn remove_conflicts_for_lock(&mut self, txid: &TxHash, inputs: &[OutPoint]) -> Vec<TxRef> {
        let roots: BTreeMap<_, _> = inputs
            .iter()
            .filter_map(|input| self.get_conflict_tx(input))
            .filter(|tx| tx.txid() != *txid)
            .map(|tx| (tx.txid(), tx))
            .collect();
        for root in roots.values() {
            self.remove_recursive(root, MemPoolRemovalReason::Conflict);
        }
        if let Some(tx) = self.get(txid) {
            self.remove_asset_unlock_conflicts(&tx);
        }
        roots.into_values().collect()
    }

    // MP1a-2 attaches Dash conflict maps at these under-lock hooks.
    pub(super) fn on_dash_insert(&mut self, _id: TxHash) {}
    fn on_dash_remove(&mut self, _id: TxHash) {}
    fn remove_dash_conflict_references(&mut self, _tx: &Tx) {}
    fn remove_asset_unlock_conflicts(&mut self, _tx: &Tx) {}
}
