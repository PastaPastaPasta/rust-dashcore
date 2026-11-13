//! Core v24.0.0 `txmempool.cpp:163-370`: bounded walks and package/link maintenance.

use std::collections::BTreeSet;
use std::fmt;

use dash_consensus::Tx;
use dash_primitives::TxHash;

use crate::options::MemPoolLimits;

use super::{PoolEntry, PoolState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AncestorLimitError(pub String);

impl fmt::Display for AncestorLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for AncestorLimitError {}

pub const NO_LIMITS: MemPoolLimits = MemPoolLimits {
    ancestor_count: i64::MAX,
    ancestor_size_vbytes: i64::MAX,
    descendant_count: i64::MAX,
    descendant_size_vbytes: i64::MAX,
};

impl PoolState {
    pub fn calculate_ancestors_and_check_limits(
        &self,
        entry_size: i64,
        entry_count: usize,
        mut staged: BTreeSet<TxHash>,
        limits: &MemPoolLimits,
    ) -> Result<BTreeSet<TxHash>, AncestorLimitError> {
        let mut size = entry_size;
        let mut ancestors = BTreeSet::new();
        while let Some(id) = staged.pop_first() {
            let entry = &self.entries[&id];
            ancestors.insert(id);
            size += entry.vsize as i64;
            let message = if entry.descendants.size as i64 + entry_size
                > limits.descendant_size_vbytes
            {
                Some(format!(
                    "exceeds descendant size limit for tx {id} [limit: {}]",
                    limits.descendant_size_vbytes
                ))
            } else if entry.descendants.count + entry_count as u64 > limits.descendant_count as u64
            {
                Some(format!(
                    "too many descendants for tx {id} [limit: {}]",
                    limits.descendant_count
                ))
            } else if size > limits.ancestor_size_vbytes {
                Some(format!(
                    "exceeds ancestor size limit [limit: {}]",
                    limits.ancestor_size_vbytes
                ))
            } else {
                None
            };
            if let Some(message) = message {
                return Err(AncestorLimitError(message));
            }
            for parent in &entry.parents {
                if !ancestors.contains(parent) {
                    staged.insert(*parent);
                }
                if staged.len() + ancestors.len() + entry_count > limits.ancestor_count as usize {
                    return Err(AncestorLimitError(format!(
                        "too many unconfirmed ancestors [limit: {}]",
                        limits.ancestor_count
                    )));
                }
            }
        }
        Ok(ancestors)
    }

    pub fn calculate_mem_pool_ancestors(
        &self,
        entry: &PoolEntry,
        limits: &MemPoolLimits,
        search_for_parents: bool,
    ) -> Result<BTreeSet<TxHash>, AncestorLimitError> {
        let mut staged = BTreeSet::new();
        if search_for_parents {
            for input in entry.tx.inputs() {
                let id = input.prevout().txid();
                if self.entries.contains_key(&id) {
                    staged.insert(id);
                    if staged.len() + 1 > limits.ancestor_count as usize {
                        return Err(AncestorLimitError(format!(
                            "too many unconfirmed parents [limit: {}]",
                            limits.ancestor_count
                        )));
                    }
                }
            }
        } else {
            assert!(self.entries.contains_key(&entry.txid()));
            staged.clone_from(&entry.parents);
        }
        self.calculate_ancestors_and_check_limits(entry.vsize as i64, 1, staged, limits)
    }

    /// Core's AssumeCalculateMemPoolAncestors uses an empty set after a failed assumption.
    pub fn assume_calculate_mem_pool_ancestors(
        &self,
        entry: &PoolEntry,
        limits: &MemPoolLimits,
        search_for_parents: bool,
    ) -> BTreeSet<TxHash> {
        self.calculate_mem_pool_ancestors(entry, limits, search_for_parents).unwrap_or_default()
    }

    pub fn has_no_inputs_of(&self, tx: &Tx) -> bool {
        tx.inputs().iter().all(|input| !self.entries.contains_key(&input.prevout().txid()))
    }

    pub fn update_ancestors_of(&mut self, add: bool, id: TxHash, ancestors: &BTreeSet<TxHash>) {
        let entry = &self.entries[&id];
        let parents = entry.parents.clone();
        let sign = if add {
            1
        } else {
            -1
        };
        let size = sign * entry.vsize as i64;
        let fee = if add {
            entry.modified_fee
        } else {
            entry.modified_fee.saturating_neg()
        };
        for parent in parents {
            self.update_child(parent, id, add);
        }
        for ancestor in ancestors {
            self.modify_entry(*ancestor, |entry| entry.descendants.update(size, fee, sign));
        }
    }

    pub fn update_entry_for_ancestors(&mut self, id: TxHash, ancestors: &BTreeSet<TxHash>) {
        let (mut size, mut fees, mut sigops) = (0, 0i64, 0);
        for ancestor in ancestors {
            let entry = &self.entries[ancestor];
            size += entry.vsize as i64;
            fees = fees.saturating_add(entry.modified_fee);
            sigops += entry.metadata.sigops;
        }
        self.modify_entry(id, |entry| {
            entry.update_ancestor_state(size, fees, ancestors.len() as i64, sigops)
        });
    }

    pub fn update_children_for_removal(&mut self, id: TxHash) {
        let children = self.entries[&id].children.clone();
        for child in children {
            self.update_parent(child, id, false);
        }
    }

    pub fn update_for_remove_from_mempool(
        &mut self,
        remove: &BTreeSet<TxHash>,
        update_descendants: bool,
    ) {
        if update_descendants {
            for id in remove {
                let entry = &self.entries[id];
                let (size, fee, sigops) = (
                    -(entry.vsize as i64),
                    entry.modified_fee.saturating_neg(),
                    -entry.metadata.sigops,
                );
                let mut descendants = self.calculate_descendants(*id);
                descendants.remove(id);
                for descendant in descendants {
                    self.modify_entry(descendant, |entry| {
                        entry.update_ancestor_state(size, fee, -1, sigops)
                    });
                }
            }
        }
        for id in remove {
            let ancestors =
                self.assume_calculate_mem_pool_ancestors(&self.entries[id], &NO_LIMITS, false);
            self.update_ancestors_of(false, *id, &ancestors);
        }
        for id in remove {
            self.update_children_for_removal(*id);
        }
    }

    pub fn update_parent(&mut self, id: TxHash, parent: TxHash, add: bool) {
        self.update_link(id, parent, add, true);
    }

    pub fn update_child(&mut self, id: TxHash, child: TxHash, add: bool) {
        self.update_link(id, child, add, false);
    }

    fn update_link(&mut self, id: TxHash, other: TxHash, add: bool, parent: bool) {
        let mut changed = false;
        self.modify_entry(id, |entry| {
            let links = if parent {
                &mut entry.parents
            } else {
                &mut entry.children
            };
            changed = if add {
                links.insert(other)
            } else {
                links.remove(&other)
            };
        });
        if changed {
            if add {
                self.usage += super::usage::LINK_ALLOCATION;
            } else {
                self.usage -= super::usage::LINK_ALLOCATION;
            }
        }
    }
}
