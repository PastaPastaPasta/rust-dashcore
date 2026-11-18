//! Core v24.0.0 `kernel/mempool_entry.h`: transaction and cached package statistics.

use std::collections::BTreeSet;

use dash_consensus::{Amount, Tx, TxRef};
use dash_primitives::TxHash;

use crate::options::MemPoolOptions;
use crate::types::LockPoints;

use super::usage;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryMetadata {
    pub fee: Amount,
    pub time: i64,
    pub height: u32,
    pub spends_coinbase: bool,
    pub sigops: i64,
    pub lock_points: LockPoints,
}

/// Includes the entry itself, as in Core's ancestor and descendant caches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageStats {
    pub count: u64,
    pub size: u64,
    pub modified_fees: Amount,
}

impl PackageStats {
    pub(super) fn update(&mut self, size: i64, fee: Amount, count: i64) {
        self.size = self.size.checked_add_signed(size).expect("package size underflow");
        self.count = self.count.checked_add_signed(count).expect("package count underflow");
        self.modified_fees = self.modified_fees.saturating_add(fee);
        assert!(self.size > 0 && self.count > 0);
    }
}

#[derive(Clone)]
pub struct PoolEntry {
    pub(super) tx: TxRef,
    pub(super) metadata: EntryMetadata,
    pub(super) vsize: u64,
    pub(super) usage: usize,
    pub(super) modified_fee: Amount,
    pub(super) ancestors: PackageStats,
    pub(super) descendants: PackageStats,
    pub(super) ancestor_sigops: i64,
    pub(super) parents: BTreeSet<TxHash>,
    pub(super) children: BTreeSet<TxHash>,
    pub(super) vtx_hashes_idx: usize,
}

impl PoolEntry {
    pub fn new(tx: TxRef, metadata: EntryMetadata, options: &MemPoolOptions) -> Self {
        assert!(metadata.sigops >= 0);
        let vsize = virtual_size(&tx, metadata.sigops, options.bytes_per_sigop);
        assert!(vsize > 0);
        let stats = PackageStats {
            count: 1,
            size: vsize,
            modified_fees: metadata.fee,
        };
        Self {
            usage: usage::transaction_usage(&tx),
            tx,
            metadata,
            vsize,
            modified_fee: metadata.fee,
            ancestors: stats,
            descendants: stats,
            ancestor_sigops: metadata.sigops,
            parents: BTreeSet::new(),
            children: BTreeSet::new(),
            vtx_hashes_idx: 0,
        }
    }

    pub fn tx(&self) -> &TxRef {
        &self.tx
    }
    pub fn txid(&self) -> TxHash {
        self.tx.txid()
    }
    pub fn metadata(&self) -> &EntryMetadata {
        &self.metadata
    }
    pub fn vsize(&self) -> u64 {
        self.vsize
    }
    pub fn modified_fee(&self) -> Amount {
        self.modified_fee
    }
    pub fn ancestors(&self) -> PackageStats {
        self.ancestors
    }
    pub fn descendants(&self) -> PackageStats {
        self.descendants
    }
    pub fn ancestor_sigops(&self) -> i64 {
        self.ancestor_sigops
    }
    /// Core v24.0.0 `kernel/mempool_entry.h:180`, sorted by CompareIteratorByHash (:43-50).
    pub fn get_mem_pool_parents_const(&self) -> &BTreeSet<TxHash> {
        &self.parents
    }
    pub fn parents(&self) -> &BTreeSet<TxHash> {
        self.get_mem_pool_parents_const()
    }
    pub fn children(&self) -> &BTreeSet<TxHash> {
        &self.children
    }
    pub fn dynamic_memory_usage(&self) -> usize {
        self.usage
    }
    pub fn vtx_hashes_index(&self) -> usize {
        self.vtx_hashes_idx
    }
    pub fn update_lock_points(&mut self, lock_points: LockPoints) {
        self.metadata.lock_points = lock_points;
    }

    pub(super) fn update_modified_fee(&mut self, delta: Amount) {
        self.modified_fee = self.modified_fee.saturating_add(delta);
        self.ancestors.modified_fees = self.ancestors.modified_fees.saturating_add(delta);
        self.descendants.modified_fees = self.descendants.modified_fees.saturating_add(delta);
    }

    pub(super) fn update_ancestor_state(
        &mut self,
        size: i64,
        fee: Amount,
        count: i64,
        sigops: i64,
    ) {
        self.ancestors.update(size, fee, count);
        self.ancestor_sigops += sigops;
        assert!(self.ancestor_sigops >= 0);
    }
}

#[derive(Clone)]
pub struct TxMempoolInfo {
    pub tx: TxRef,
    pub time: i64,
    pub fee: Amount,
    pub vsize: u64,
    pub fee_delta: Amount,
}

impl From<&PoolEntry> for TxMempoolInfo {
    fn from(entry: &PoolEntry) -> Self {
        Self {
            tx: entry.tx.clone(),
            time: entry.metadata.time,
            fee: entry.metadata.fee,
            vsize: entry.vsize,
            fee_delta: entry.modified_fee.saturating_sub(entry.metadata.fee),
        }
    }
}

pub(super) fn virtual_size(tx: &Tx, sigops: i64, bytes_per_sigop: u32) -> u64 {
    i64::from(tx.size())
        .max(sigops.checked_mul(i64::from(bytes_per_sigop)).expect("sigop virtual size overflow"))
        as u64
}
