//! Small deterministic doubles for mining surface tests.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use dash_consensus::primitives::{Tx, TxHash, TxRef};
use dash_dml::MnList;
use dash_evo::types::ProTxHash;

use crate::select::AncestorScore;
use crate::view::{EntryId, MiningEntry, MiningMempool, MiningSafety, NodeStatus, PackageView};

/// A fake entry with explicit ancestry and Core's cached ancestor fields.
pub struct FakeEntry {
    pub tx: TxRef,
    pub fee: i64,
    pub modified_fee: i64,
    pub tx_size: u64,
    pub sigops: u32,
    pub parents: Vec<EntryId>,
    pub size_with_ancestors: u64,
    pub mod_fees_with_ancestors: i64,
    pub sigops_with_ancestors: u32,
    pub count_with_ancestors: u64,
}

impl FakeEntry {
    pub fn new(tx: TxRef, fee: i64, tx_size: u64, sigops: u32) -> Self {
        Self {
            tx,
            fee,
            modified_fee: fee,
            tx_size,
            sigops,
            parents: Vec::new(),
            size_with_ancestors: tx_size,
            mod_fees_with_ancestors: fee,
            sigops_with_ancestors: sigops,
            count_with_ancestors: 1,
        }
    }
}

/// A deliberately simple pool. Its counter is independent of the entries and is therefore safe to
/// read while a caller mutates the entries in a test.
pub struct FakePool {
    pub entries: Vec<FakeEntry>,
    updates: AtomicU32,
    service_updates: HashMap<EntryId, Vec<EntryId>>,
    key_targets: HashMap<EntryId, ProTxHash>,
    unorderable: HashSet<EntryId>,
}

impl Default for FakePool {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            updates: AtomicU32::new(0),
            service_updates: HashMap::new(),
            key_targets: HashMap::new(),
            unorderable: HashSet::new(),
        }
    }
}

impl FakePool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, entry: FakeEntry) -> EntryId {
        let id = EntryId(self.entries.len() as u64);
        self.entries.push(entry);
        id
    }

    pub fn bump_transactions_updated(&self, n: u32) {
        self.updates.fetch_add(n, AtomicOrdering::SeqCst);
    }

    pub fn set_service_updates(&mut self, id: EntryId, updates: Vec<EntryId>) {
        self.service_updates.insert(id, updates);
    }

    pub fn set_key_change_target(&mut self, id: EntryId, target: ProTxHash) {
        self.key_targets.insert(id, target);
    }

    pub fn set_unorderable(&mut self, id: EntryId, value: bool) {
        if value {
            self.unorderable.insert(id);
        } else {
            self.unorderable.remove(&id);
        }
    }
}

impl FakeEntry {
    fn txid(&self) -> TxHash {
        self.tx.txid()
    }
}

impl PackageView for FakePool {
    fn ancestor_score_iter(&self) -> Box<dyn Iterator<Item = EntryId> + '_> {
        let mut ids: Vec<_> = (0..self.entries.len()).map(|i| EntryId(i as u64)).collect();
        ids.sort_by_cached_key(|id| AncestorScore::of(&self.mining_entry(*id)));
        Box::new(ids.into_iter())
    }

    fn mining_entry(&self, id: EntryId) -> MiningEntry<'_> {
        let e = &self.entries[id.0 as usize];
        MiningEntry {
            tx: &e.tx,
            txid: e.txid(),
            fee: e.fee,
            modified_fee: e.modified_fee,
            tx_size: e.tx_size,
            sigops: e.sigops,
            size_with_ancestors: e.size_with_ancestors,
            mod_fees_with_ancestors: e.mod_fees_with_ancestors,
            sigops_with_ancestors: e.sigops_with_ancestors,
            count_with_ancestors: e.count_with_ancestors,
        }
    }

    fn ancestors_no_limits(&self, id: EntryId) -> Vec<EntryId> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut todo = self.entries[id.0 as usize].parents.clone();
        while let Some(parent) = todo.pop() {
            if seen.insert(parent) {
                todo.extend(self.entries[parent.0 as usize].parents.iter().copied());
                out.push(parent);
            }
        }
        out
    }

    fn descendants(&self, id: EntryId) -> Vec<EntryId> {
        (0..self.entries.len())
            .map(|i| EntryId(i as u64))
            .filter(|candidate| {
                *candidate == id || self.ancestors_no_limits(*candidate).contains(&id)
            })
            .collect()
    }

    fn service_updates_before_key_change(&self, key_change: EntryId, _: &MnList) -> Vec<EntryId> {
        self.service_updates.get(&key_change).cloned().unwrap_or_default()
    }

    fn is_unorderable_service_update(&self, tx: &Tx, _: &[EntryId], _: &MnList) -> bool {
        self.entry_id(&tx.txid()).is_some_and(|id| self.unorderable.contains(&id))
    }

    fn key_change_target(&self, id: EntryId, _: &MnList) -> Option<ProTxHash> {
        self.key_targets.get(&id).copied()
    }

    fn entry_id(&self, txid: &TxHash) -> Option<EntryId> {
        self.entries.iter().position(|e| e.txid() == *txid).map(|i| EntryId(i as u64))
    }
}

impl MiningMempool for FakePool {
    fn with_read(&self, f: &mut dyn FnMut(&dyn PackageView)) {
        f(self);
    }
    fn transactions_updated(&self) -> u32 {
        self.updates.load(AtomicOrdering::SeqCst)
    }
}

/// A configurable safety fake.
#[derive(Default)]
pub struct FakeSafety {
    pub safe: bool,
}

impl MiningSafety for FakeSafety {
    fn is_tx_safe_for_mining(&self, _: &TxHash) -> bool {
        self.safe
    }
}

/// A safety fake for tests where every transaction is safe.
#[derive(Clone, Copy, Debug, Default)]
pub struct AlwaysSafe;

impl MiningSafety for AlwaysSafe {
    fn is_tx_safe_for_mining(&self, _: &TxHash) -> bool {
        true
    }
}

/// A safety fake for tests where no transaction is safe.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverSafe;

impl MiningSafety for NeverSafe {
    fn is_tx_safe_for_mining(&self, _: &TxHash) -> bool {
        false
    }
}

/// A configurable node-status fake.
#[derive(Clone, Debug)]
pub struct FakeNodeStatus {
    pub connman: bool,
    pub peers: usize,
    pub masternodes_synced: bool,
    pub rpc: bool,
}

impl Default for FakeNodeStatus {
    fn default() -> Self {
        Self {
            connman: true,
            peers: 1,
            masternodes_synced: true,
            rpc: true,
        }
    }
}

impl NodeStatus for FakeNodeStatus {
    fn has_connman(&self) -> bool {
        self.connman
    }
    fn peer_count(&self) -> usize {
        self.peers
    }
    fn mn_synced(&self) -> bool {
        self.masternodes_synced
    }
    fn rpc_running(&self) -> bool {
        self.rpc
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::sync::Arc;

    use dash_consensus::primitives::core_hash_cmp;

    use dash_consensus::primitives::{Transaction, TxType};

    use super::*;

    fn tx(lock_time: u32) -> TxRef {
        Arc::new(Tx::new(Transaction::new(
            1,
            TxType::new(0),
            Vec::new(),
            Vec::new(),
            lock_time,
            Vec::new(),
        )))
    }

    fn child(pool: &mut FakePool, parent: EntryId, tx: TxRef, fee: i64, size: u64) -> EntryId {
        let p = &pool.entries[parent.0 as usize];
        let mut entry = FakeEntry::new(tx, fee, size, 1);
        entry.parents = vec![parent];
        entry.size_with_ancestors = p.size_with_ancestors + size;
        entry.mod_fees_with_ancestors = p.mod_fees_with_ancestors + fee;
        entry.sigops_with_ancestors = p.sigops_with_ancestors + 1;
        entry.count_with_ancestors = p.count_with_ancestors + 1;
        pool.push(entry)
    }

    /// Hand-computed scores, min(own rate, package rate) as `GetModFeeAndSize` picks it:
    /// `low_parent` 1.0; `rich_child` own 3.0, package 400/200 = 2.0; `rival` 2.0, tied with
    /// `rich_child` and ordered by txid alone; `lean` 0.75; `poor_child` own 0.5, package
    /// 150/200 = 0.75, so 0.5; `between` 2.5, above `rich_child`'s package rate and below its own.
    // Core: txmempool.h:158-195 at v24.0.2 CompareTxMemPoolEntryByAncestorFee
    #[test]
    fn ancestor_score_order_is_cores() {
        let mut pool = FakePool::new();
        let low_parent = pool.push(FakeEntry::new(tx(1), 100, 100, 1));
        let rich_child = child(&mut pool, low_parent, tx(2), 300, 100);
        let poor_child = child(&mut pool, low_parent, tx(3), 50, 100);
        let lean = pool.push(FakeEntry::new(tx(4), 75, 100, 1));
        // A rival whose txid sorts after the child's, so only Core's tie-break (the txid) puts
        // the child first: a fee or size tie-break orders them the other way.
        let rich_txid = pool.entries[rich_child.0 as usize].txid();
        let rival_tx = (5..)
            .map(tx)
            .find(|t| core_hash_cmp(rich_txid.as_bytes(), t.txid().as_bytes()) == Ordering::Less)
            .unwrap();
        let rival = pool.push(FakeEntry::new(rival_tx, 200, 100, 1));
        let between = pool.push(FakeEntry::new(tx(1000), 250, 100, 1));
        let actual: Vec<_> = pool.ancestor_score_iter().collect();
        assert_eq!(actual, vec![between, rich_child, rival, low_parent, lean, poor_child]);
    }

    #[test]
    fn equal_scores_order_by_raw_txid_bytes() {
        let mut pool = FakePool::new();
        let ids: Vec<_> = (1..=6)
            .map(|n| {
                pool.push(FakeEntry::new(tx(n), 100 * u64::from(n) as i64, 100 * u64::from(n), 1))
            })
            .collect();
        let mut expected = ids.clone();
        expected.sort_by(|a, b| {
            let txid = |id: &EntryId| pool.entries[id.0 as usize].txid();
            txid(a).as_bytes().cmp(txid(b).as_bytes())
        });
        assert_eq!(pool.ancestor_score_iter().collect::<Vec<_>>(), expected);
    }
}
