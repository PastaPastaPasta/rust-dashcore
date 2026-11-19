//! Core v24.0.0 `txmempool.cpp:1984-1988`, logical x86_64/libstdc++ allocations.

use dash_consensus::Tx;
use dashd_chainstate::reorg::memusage::{malloc_usage, recursive_dynamic_usage};

use super::PoolState;

// kernel/mempool_entry.h: 304 bytes including the two 48-byte sets and Epoch::Marker.
pub const ENTRY_ALLOCATION: usize = malloc_usage(304 + 12 * 8);
// memusage.h: 32-byte tree header followed by an 8-byte reference.
pub const LINK_ALLOCATION: usize = malloc_usage(32 + 8);
// mapNextTx is an indirectmap: its stored key and value are both pointers.
pub const NEXT_TX_ALLOCATION: usize = malloc_usage(32 + 8 + 8);
pub const DELTA_ALLOCATION: usize = malloc_usage(32 + 32 + 8);

pub fn transaction_usage(tx: &Tx) -> usize {
    // RecursiveDynamicUsage(shared_ptr<CTransaction>): the 112-byte CTransaction, the
    // 24-byte stl_shared_counter, then RecursiveDynamicUsage(CTransaction).
    malloc_usage(112) + malloc_usage(24) + recursive_dynamic_usage(tx)
}

pub(super) const fn hashes_usage(capacity: usize) -> usize {
    malloc_usage(40 * capacity)
}

impl PoolState {
    /// Exact cached logical usage; safe inside a held pool scope without taking another lock.
    pub fn dynamic_memory_usage(&self) -> usize {
        self.usage
    }

    /// Independent walk for check() and property tests, never used by ordinary usage reads.
    pub fn recompute_dynamic_memory_usage(&self) -> usize {
        ENTRY_ALLOCATION * self.entries.len()
            + NEXT_TX_ALLOCATION * self.next_tx.len()
            + DELTA_ALLOCATION * self.deltas.len()
            + hashes_usage(self.vtx_capacity)
            + self
                .entries
                .values()
                .map(|entry| {
                    transaction_usage(&entry.tx)
                        + LINK_ALLOCATION * (entry.parents.len() + entry.children.len())
                })
                .sum::<usize>()
    }

    pub(super) fn resize_hashes_usage(&mut self, new_capacity: usize) {
        self.usage -= hashes_usage(self.vtx_capacity);
        self.vtx_capacity = new_capacity;
        self.usage += hashes_usage(self.vtx_capacity);
    }
}
