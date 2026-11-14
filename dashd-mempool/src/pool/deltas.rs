//! Core v24.0.0 `txmempool.cpp:1878-1919`: retained fee deltas and package updates.

use dash_consensus::Amount;
use dash_primitives::TxHash;

use super::ancestors::NO_LIMITS;
use super::{PoolState, usage};

impl PoolState {
    pub fn prioritise_transaction(&mut self, id: TxHash, fee_delta: Amount) {
        let delta = self.deltas.entry(id).or_insert_with(|| {
            self.usage += usage::DELTA_ALLOCATION;
            0
        });
        *delta = delta.saturating_add(fee_delta);
        let Some(entry) = self.entries.get(&id) else {
            return;
        };
        let ancestors = self.assume_calculate_mem_pool_ancestors(entry, &NO_LIMITS, false);
        let mut descendants = self.calculate_descendants(id);
        descendants.remove(&id);
        self.modify_entry(id, |entry| entry.update_modified_fee(fee_delta));
        for ancestor in ancestors {
            self.modify_entry(ancestor, |entry| entry.descendants.update(0, fee_delta, 0));
        }
        for descendant in descendants {
            self.modify_entry(descendant, |entry| entry.update_ancestor_state(0, fee_delta, 0, 0));
        }
        self.add_transactions_updated(1);
    }

    pub fn apply_delta(&self, id: &TxHash, fee_delta: &mut Amount) {
        if let Some(delta) = self.deltas.get(id) {
            *fee_delta = fee_delta.wrapping_add(*delta);
        }
    }

    /// Core erases only the retained delta; a resident entry's modified fees stay unchanged.
    pub fn clear_prioritisation(&mut self, id: &TxHash) {
        if self.deltas.remove(id).is_some() {
            self.usage -= usage::DELTA_ALLOCATION;
        }
    }

    pub fn deltas(&self) -> &std::collections::BTreeMap<TxHash, Amount> {
        &self.deltas
    }
}
