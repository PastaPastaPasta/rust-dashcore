//! Core v24.0.0 `txmempool.cpp:57-161`: the in-pool descendants of transactions a reorg put back.

use std::collections::{BTreeMap, BTreeSet};

use dash_primitives::{OutPoint, TxHash};

use crate::types::MemPoolRemovalReason;

use super::PoolState;

/// `cacheMap`: each processed entry's in-pool descendants outside the update set.
type DescendantCache = BTreeMap<TxHash, BTreeSet<TxHash>>;

impl PoolState {
    /// `UpdateTransactionsFromBlock(vHashesToUpdate)`: in reverse, each re-added entry is linked
    /// to its in-pool children outside `hashes` (`mapNextTx` order), then its descendants'
    /// ancestor state and its own descendant state take each other in. A descendant left over the
    /// ancestor limits leaves recursively as SIZELIMIT, in txid order, once all are updated.
    // Core: txmempool.cpp:108-161 at v24.0.0
    pub fn update_transactions_from_block(&mut self, hashes: &[TxHash]) {
        let mut cache = DescendantCache::new();
        let included: BTreeSet<TxHash> = hashes.iter().copied().collect();
        let mut to_remove = BTreeSet::new();
        for hash in hashes.iter().rev() {
            if !self.entries.contains_key(hash) {
                continue;
            }
            let children: Vec<TxHash> = self
                .next_tx
                .range(OutPoint::new(*hash, 0)..)
                .take_while(|(outpoint, _)| outpoint.txid() == *hash)
                .map(|(_, child)| *child)
                .collect();
            let mut visited = BTreeSet::new();
            for child in children {
                assert!(self.entries.contains_key(&child), "mapNextTx names a held child");
                if visited.insert(child) && !included.contains(&child) {
                    self.update_child(*hash, child, true);
                    self.update_parent(child, *hash, true);
                }
            }
            self.update_for_descendants(*hash, &mut cache, &included, &mut to_remove);
        }
        for txid in to_remove {
            if let Some(tx) = self.get(&txid) {
                self.remove_recursive(&tx, MemPoolRemovalReason::SizeLimit);
            }
        }
    }

    /// `UpdateForDescendants(updateIt, cachedDescendants, setExclude, descendants_to_remove)`:
    /// every in-pool descendant of `update` (through `cache` where an entry was already walked)
    /// outside `exclude` takes `update` into its ancestor state and is cached for it; `update`
    /// takes them into its descendant state.
    // Core: txmempool.cpp:57-106 at v24.0.0
    fn update_for_descendants(
        &mut self,
        update: TxHash,
        cache: &mut DescendantCache,
        exclude: &BTreeSet<TxHash>,
        to_remove: &mut BTreeSet<TxHash>,
    ) {
        let mut stage = self.entries[&update].children.clone();
        let mut descendants = BTreeSet::new();
        while let Some(descendant) = stage.pop_first() {
            descendants.insert(descendant);
            for child in &self.entries[&descendant].children {
                if let Some(cached) = cache.get(child) {
                    descendants.extend(cached.iter().copied());
                } else if !descendants.contains(child) {
                    stage.insert(*child);
                }
            }
        }
        let parent = &self.entries[&update];
        let (size, fee, sigops) =
            (parent.vsize as i64, parent.modified_fee, parent.metadata.sigops);
        let limits = self.options.limits;
        let (mut modify_size, mut modify_fee, mut modify_count) = (0i64, 0i64, 0i64);
        for descendant in descendants {
            if exclude.contains(&descendant) {
                continue;
            }
            let entry = &self.entries[&descendant];
            modify_size += entry.vsize as i64;
            modify_fee = modify_fee.saturating_add(entry.modified_fee);
            modify_count += 1;
            cache.entry(update).or_default().insert(descendant);
            self.modify_entry(descendant, |entry| {
                entry.update_ancestor_state(size, fee, 1, sigops)
            });
            let ancestors = self.entries[&descendant].ancestors;
            if ancestors.count > limits.ancestor_count as u64
                || ancestors.size > limits.ancestor_size_vbytes as u64
            {
                to_remove.insert(descendant);
            }
        }
        self.modify_entry(update, |entry| {
            entry.descendants.update(modify_size, modify_fee, modify_count)
        });
    }
}
