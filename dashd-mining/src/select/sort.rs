//! Core's two orders over a `setEntries`: `CompareIteratorByHash` and `SortForBlock`.

use std::cmp::Ordering;

use dash_consensus::primitives::core_hash_cmp;

use crate::view::{EntryId, PackageView};

#[derive(PartialEq, Eq)]
struct HashOrder([u8; 32]);

impl Ord for HashOrder {
    fn cmp(&self, other: &Self) -> Ordering {
        core_hash_cmp(&self.0, &other.0)
    }
}

impl PartialOrd for HashOrder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn hash_order<V: PackageView + ?Sized>(view: &V, id: EntryId) -> HashOrder {
    HashOrder(*view.mining_entry(id).txid.as_bytes())
}

/// A `setEntries` (Core v24.0.2 src/txmempool.h:405): distinct entries in txid order.
pub(crate) fn set_entries<V: PackageView + ?Sized>(view: &V, ids: &mut Vec<EntryId>) {
    ids.sort_by_cached_key(|id| hash_order(view, *id));
    ids.dedup();
}

/// Core v24.0.2 src/node/miner.cpp:490-499 SortForBlock, by `CompareTxIterByAncestorCount`
/// (src/node/miner.h:104-112).
pub(crate) fn sort_for_block<V: PackageView + ?Sized>(view: &V, package: &mut [EntryId]) {
    package.sort_by_cached_key(|id| {
        (view.mining_entry(*id).count_with_ancestors, hash_order(view, *id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::tests::{Dag, tx_where};

    #[test]
    fn parents_precede_children_and_ties_go_by_raw_txid_bytes() {
        let mut dag = Dag::default();
        // Raw bytes put `b` first; a numeric uint256 order (byte 31 first) would put `a` first.
        let a = dag.add_tx(tx_where(|h| h[0] >= 0x80 && h[31] < 0x80), &[], 100, 100, 1);
        let b = dag.add_tx(tx_where(|h| h[0] < 0x80 && h[31] >= 0x80), &[], 100, 100, 1);
        let c = dag.add(&[a], 100, 100, 1);
        let d = dag.add(&[c, b], 100, 100, 1);
        let pool = dag.pool();
        let mut ids = vec![d, c, a, b];
        sort_for_block(&pool, &mut ids);
        assert_eq!(ids, vec![b, a, c, d]);
        let mut entries = vec![a, b];
        set_entries(&pool, &mut entries);
        assert_eq!(entries, vec![b, a]);
    }
}
