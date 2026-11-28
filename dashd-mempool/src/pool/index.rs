//! Core `txmempool.h:82-198` at v24.0.2: the descendant-score, entry-time and ancestor-fee
//! orderings.

use std::cmp::Ordering;

use dash_primitives::TxHash;

use super::{PoolEntry, PoolState};

/// Additional indexes must not re-enter the mempool. MP1b supplies eviction/time indexes here.
pub trait PoolIndex: Send + Sync {
    fn on_insert(&mut self, entry: &PoolEntry);
    fn on_update(&mut self, old: &PoolEntry, new: &PoolEntry);
    fn on_remove(&mut self, entry: &PoolEntry);
}

#[derive(Clone, Copy, Debug)]
pub(super) struct AncestorScore {
    txid: TxHash,
    fee: i64,
    size: u64,
}

impl From<&PoolEntry> for AncestorScore {
    fn from(entry: &PoolEntry) -> Self {
        let use_ancestors = product(entry.modified_fee, entry.ancestors.size)
            > product(entry.ancestors.modified_fees, entry.vsize);
        let (fee, size) = if use_ancestors {
            (entry.ancestors.modified_fees, entry.ancestors.size)
        } else {
            (entry.modified_fee, entry.vsize)
        };
        Self {
            txid: entry.txid(),
            fee,
            size,
        }
    }
}

impl PartialEq for AncestorScore {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for AncestorScore {}
impl PartialOrd for AncestorScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for AncestorScore {
    fn cmp(&self, other: &Self) -> Ordering {
        product(other.fee, self.size)
            .cmp(&product(self.fee, other.size))
            .then_with(|| self.txid.cmp(&other.txid))
    }
}

/// A fee × size cross product, exact. Core multiplies doubles, whose rounding above 2^53 can
/// make its score orders cyclic; the exact product keeps these orders total and equals Core's
/// wherever the doubles are exact.
fn product(fee: i64, size: u64) -> i128 {
    i128::from(fee) * i128::from(size)
}

/// `CompareTxMemPoolEntryByDescendantScore`: the lower of the entry's own feerate and its
/// package-with-descendants feerate first, compared as exact cross products; at an equal
/// score the later entry time first. Core leaves two entries equal in both in tree order; they
/// are in txid order here.
// Core: txmempool.h:82-122 at v24.0.2
#[derive(Clone, Copy, Debug)]
pub(super) struct DescendantScore {
    txid: TxHash,
    fee: i64,
    size: u64,
    time: i64,
}

impl From<&PoolEntry> for DescendantScore {
    fn from(entry: &PoolEntry) -> Self {
        let own = product(entry.modified_fee, entry.descendants.size);
        let package = product(entry.descendants.modified_fees, entry.vsize);
        let (fee, size) = if package > own {
            (entry.descendants.modified_fees, entry.descendants.size)
        } else {
            (entry.modified_fee, entry.vsize)
        };
        Self {
            txid: entry.txid(),
            fee,
            size,
            time: entry.metadata.time,
        }
    }
}

impl PartialEq for DescendantScore {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for DescendantScore {}
impl PartialOrd for DescendantScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for DescendantScore {
    fn cmp(&self, other: &Self) -> Ordering {
        product(self.fee, other.size)
            .cmp(&product(other.fee, self.size))
            .then_with(|| other.time.cmp(&self.time))
            .then_with(|| self.txid.cmp(&other.txid))
    }
}

/// `CompareTxMemPoolEntryByEntryTime`, with the txid making the key unique.
// Core: txmempool.h:145-152 at v24.0.2
pub(super) fn entry_time(entry: &PoolEntry) -> (i64, TxHash) {
    (entry.metadata.time, entry.txid())
}

impl PoolState {
    /// The `descendant_score` index from its start: the first entry `TrimToSize` evicts.
    pub fn descendant_score_order(&self) -> impl Iterator<Item = TxHash> + '_ {
        self.descendant_score.iter().map(|key| key.txid)
    }

    pub fn ancestor_score_order(&self) -> impl Iterator<Item = TxHash> + '_ {
        self.ancestor_score.iter().map(|key| key.txid)
    }

    pub fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> bool {
        let Some(b) = self.entries.get(b) else {
            return false;
        };
        let Some(a) = self.entries.get(a) else {
            return true;
        };
        depth_and_score(a, b) == Ordering::Less
    }

    pub fn get_sorted_depth_and_score(&self) -> Vec<TxHash> {
        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_by(|a, b| depth_and_score(a, b));
        entries.into_iter().map(PoolEntry::txid).collect()
    }

    pub(super) fn modify_entry(&mut self, id: TxHash, update: impl FnOnce(&mut PoolEntry)) {
        let entry = self.entries.get_mut(&id).expect("entry must exist");
        let old = entry.clone();
        assert!(self.ancestor_score.remove(&AncestorScore::from(&old)));
        assert!(self.descendant_score.remove(&DescendantScore::from(&old)));
        assert!(self.entry_time.remove(&entry_time(&old)));
        update(entry);
        assert!(self.ancestor_score.insert(AncestorScore::from(&*entry)));
        assert!(self.descendant_score.insert(DescendantScore::from(&*entry)));
        assert!(self.entry_time.insert(entry_time(entry)));
        for index in &mut self.indexes {
            index.on_update(&old, entry);
        }
    }

    /// Builds a newly registered index from the currently held entries.
    pub fn register_index(&mut self, mut index: Box<dyn PoolIndex>) {
        for entry in self.entries.values() {
            index.on_insert(entry);
        }
        self.indexes.push(index);
    }
}

fn depth_and_score(a: &PoolEntry, b: &PoolEntry) -> Ordering {
    a.ancestors.count.cmp(&b.ancestors.count).then_with(|| {
        product(b.metadata.fee, a.vsize)
            .cmp(&product(a.metadata.fee, b.vsize))
            .then_with(|| b.txid().cmp(&a.txid()))
    })
}

#[cfg(test)]
#[path = "score_cycle_tests.rs"]
mod score_cycle_tests;
