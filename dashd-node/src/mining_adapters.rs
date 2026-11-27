//! dashd-mining's mempool seam over the node's pool (plan MN1 §2.5): `CreateNewBlock`'s
//! `addPackageTxs` under one pool read lock, and `getblocktemplate`'s lock-free
//! `nTransactionsUpdated`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use dash_consensus::Tx;
use dash_dml::{MnList, ProTxHash};
use dash_primitives::TxHash;
use dashd_mempool::api::{MempoolRead, MiningEntry as PoolEntry};
use dashd_mempool::is::{IsAcceptHooks, IsMempool};
use dashd_mining::{
    EntryId, InstantSendHooks, MiningEntry, MiningMempool, MiningSafety, PackageView,
};

use crate::mempool::NodeMempool;

/// `CChainstateHelper::IsInstantSendEnabled` and `IsInstantSendLocked` for
/// `TestPackageTransactions`: the pool's accept-time view of the IS manager.
// Core: v24.0.2 src/node/miner.cpp:430-432
pub struct MiningInstantSend(Arc<dyn IsAcceptHooks>);

impl MiningInstantSend {
    pub fn new(hooks: Arc<dyn IsAcceptHooks>) -> MiningInstantSend {
        MiningInstantSend(hooks)
    }
}

impl InstantSendHooks for MiningInstantSend {
    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn is_locked(&self, txid: &TxHash) -> bool {
        self.0.is_locked(txid)
    }
}

/// `IsTxSafeForMining` until the node keeps a node-wide first-seen map (BL-996): never safe,
/// so with InstantSend on only locked transactions are mined (DEC-1935).
// Core: v24.0.2 src/node/miner.cpp:434-436, src/chainlock/handler.cpp:262-274
pub struct NotYetSafe;

impl MiningSafety for NotYetSafe {
    fn is_tx_safe_for_mining(&self, _: &TxHash) -> bool {
        false
    }
}

/// Each read guard's [`EntryId`]s carry its number in the high half, so an id from another
/// guard panics instead of naming one of this guard's entries.
static GUARDS: AtomicU32 = AtomicU32::new(0);

/// The node's pool as `BlockAssembler`'s `m_mempool`.
pub struct MiningPool(Arc<NodeMempool>);

impl MiningPool {
    pub fn new(mempool: Arc<NodeMempool>) -> MiningPool {
        MiningPool(mempool)
    }
}

impl MiningMempool for MiningPool {
    fn with_read(&self, f: &mut dyn FnMut(&dyn PackageView)) {
        IsMempool::with_read(&**self.0.pool(), &mut |pool| f(&GuardView::new(pool)));
    }

    /// The counter's SeqCst load, under no lock.
    // Core: v24.0.2 src/txmempool.cpp:417-420, src/txmempool.h:320
    fn transactions_updated(&self) -> u32 {
        self.0.pool().transactions_updated()
    }
}

/// One read guard's view: the entries in R2 order, an entry's [`EntryId`] the guard's number
/// and its position. Built once per guard, dropped with it.
struct GuardView<'a> {
    pool: &'a dyn MempoolRead,
    guard: u64,
    entries: Vec<PoolEntry>,
    ids: HashMap<TxHash, EntryId>,
}

impl<'a> GuardView<'a> {
    fn new(pool: &'a dyn MempoolRead) -> GuardView<'a> {
        let guard = u64::from(GUARDS.fetch_add(1, Ordering::Relaxed)) << 32;
        let entries: Vec<_> = pool
            .ancestor_score_iter()
            .map(|txid| pool.mining_entry(&txid).expect("R2 lists only resident entries"))
            .collect();
        let ids =
            entries.iter().enumerate().map(|(i, e)| (e.txid, EntryId(guard | i as u64))).collect();
        GuardView {
            pool,
            guard,
            entries,
            ids,
        }
    }

    fn entry(&self, id: EntryId) -> &PoolEntry {
        assert_eq!(id.0 >> 32, self.guard >> 32, "an EntryId from another read guard");
        &self.entries[(id.0 & u64::from(u32::MAX)) as usize]
    }

    fn ids(&self, txids: Vec<TxHash>) -> Vec<EntryId> {
        txids.iter().map(|txid| self.ids[txid]).collect()
    }
}

fn sigops(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl PackageView for GuardView<'_> {
    fn ancestor_score_iter(&self) -> Box<dyn Iterator<Item = EntryId> + '_> {
        Box::new((0..self.entries.len() as u64).map(|i| EntryId(self.guard | i)))
    }

    fn mining_entry(&self, id: EntryId) -> MiningEntry<'_> {
        let e = self.entry(id);
        MiningEntry {
            tx: &e.tx,
            txid: e.txid,
            fee: e.fee,
            modified_fee: e.modified_fee,
            tx_size: e.tx_size,
            sigops: sigops(e.sigops),
            size_with_ancestors: e.size_with_ancestors,
            mod_fees_with_ancestors: e.mod_fees_with_ancestors,
            sigops_with_ancestors: sigops(e.sigops_with_ancestors),
            count_with_ancestors: e.count_with_ancestors,
        }
    }

    fn ancestors_no_limits(&self, id: EntryId) -> Vec<EntryId> {
        self.ids(self.pool.ancestors_no_limits(&self.entry(id).txid))
    }

    fn descendants(&self, id: EntryId) -> Vec<EntryId> {
        self.ids(self.pool.descendants(&self.entry(id).txid))
    }

    fn service_updates_before_key_change(
        &self,
        key_change: EntryId,
        dmn_tip_list: &MnList,
    ) -> Vec<EntryId> {
        let txid = self.entry(key_change).txid;
        self.ids(self.pool.service_updates_before_key_change(&txid, dmn_tip_list))
    }

    fn is_unorderable_service_update(
        &self,
        tx: &Tx,
        ancestors: &[EntryId],
        dmn_tip_list: &MnList,
    ) -> bool {
        let ancestors: Vec<_> = ancestors.iter().map(|&id| self.entry(id).txid).collect();
        self.pool.is_unorderable_service_update(tx, &ancestors, dmn_tip_list)
    }

    fn key_change_target(&self, id: EntryId, dmn_tip_list: &MnList) -> Option<ProTxHash> {
        self.pool.key_change_target(&self.entry(id).tx, dmn_tip_list)
    }

    fn entry_id(&self, txid: &TxHash) -> Option<EntryId> {
        self.ids.get(txid).copied()
    }
}

#[cfg(test)]
#[path = "mining_adapters_tests.rs"]
mod tests;
