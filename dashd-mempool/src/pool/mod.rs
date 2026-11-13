//! Pool core. Public handle methods lock once; all graph/index helpers operate on PoolState.

pub mod ancestors;
mod check;
mod deltas;
pub mod entry;
pub mod index;
mod remove;
pub mod usage;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use dash_consensus::{Amount, TxRef};
use dash_primitives::{OutPoint, TxHash};
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::api::MempoolRead;
use crate::is::{IsAcceptHooks, IsMempool, NoIsHooks};
use crate::options::MemPoolOptions;
use crate::types::MemPoolRemovalReason;

pub use entry::{EntryMetadata, PackageStats, PoolEntry, TxMempoolInfo};
use index::{AncestorScore, PoolIndex};

/// Caller drains this under its write guard; MP1a-4/8 splice into the ordered signal stream.
#[derive(Clone)]
pub enum PendingEvent {
    TransactionRemovedFromMempool {
        tx: TxRef,
        reason: MemPoolRemovalReason,
        sequence: u64,
    },
}

pub struct PoolState {
    options: MemPoolOptions,
    entries: BTreeMap<TxHash, PoolEntry>,
    next_tx: BTreeMap<OutPoint, TxHash>,
    vtx_hashes: Vec<(TxHash, TxHash)>,
    // Logical libstdc++ capacity, independent of Rust's allocator and Vec growth policy.
    vtx_capacity: usize,
    ancestor_score: BTreeSet<AncestorScore>,
    unbroadcast_txids: BTreeSet<TxHash>,
    deltas: BTreeMap<TxHash, Amount>,
    indexes: Vec<Box<dyn PoolIndex>>,
    total_vsize: u64,
    total_fee: Amount,
    usage: usize,
    transactions_updated: Arc<AtomicU32>,
    sequence: u64,
    pending_events: Vec<PendingEvent>,
    is_hooks: Arc<dyn IsAcceptHooks>,
}

impl Default for PoolState {
    fn default() -> Self {
        Self::new(MemPoolOptions::default())
    }
}

impl PoolState {
    pub fn new(options: MemPoolOptions) -> Self {
        Self::new_with_counter(options, Arc::new(AtomicU32::new(0)))
    }

    pub(crate) fn new_with_counter(
        options: MemPoolOptions,
        transactions_updated: Arc<AtomicU32>,
    ) -> Self {
        Self {
            options,
            entries: BTreeMap::new(),
            next_tx: BTreeMap::new(),
            vtx_hashes: Vec::new(),
            vtx_capacity: 0,
            ancestor_score: BTreeSet::new(),
            unbroadcast_txids: BTreeSet::new(),
            deltas: BTreeMap::new(),
            indexes: Vec::new(),
            total_vsize: 0,
            total_fee: 0,
            usage: 0,
            transactions_updated,
            sequence: 1,
            pending_events: Vec::new(),
            is_hooks: Arc::new(NoIsHooks),
        }
    }

    pub fn options(&self) -> &MemPoolOptions {
        &self.options
    }
    pub fn entries(&self) -> &BTreeMap<TxHash, PoolEntry> {
        &self.entries
    }
    pub fn next_tx(&self) -> &BTreeMap<OutPoint, TxHash> {
        &self.next_tx
    }
    /// (instance hash, txid), in compact-block sampling order.
    pub fn vtx_hashes(&self) -> &[(TxHash, TxHash)] {
        &self.vtx_hashes
    }
    pub fn total_vsize(&self) -> u64 {
        self.total_vsize
    }
    pub fn total_fee(&self) -> Amount {
        self.total_fee
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Core v24.0.0 `txmempool.h:702-706`: the number of resident transactions.
    pub fn size(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn is_hooks(&self) -> &dyn IsAcceptHooks {
        &*self.is_hooks
    }
    pub fn new_entry(&self, tx: TxRef, metadata: EntryMetadata) -> PoolEntry {
        PoolEntry::new(tx, metadata, &self.options)
    }

    pub fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.entries.get(txid).map(|entry| entry.tx.clone())
    }
    pub fn exists(&self, txid: &TxHash) -> bool {
        self.entries.contains_key(txid)
    }
    pub fn get_iter(&self, txid: &TxHash) -> Option<&PoolEntry> {
        self.entries.get(txid)
    }
    pub fn get_iter_set(&self, txids: &BTreeSet<TxHash>) -> BTreeSet<TxHash> {
        txids.iter().filter(|id| self.entries.contains_key(id)).copied().collect()
    }
    pub fn get_conflict_tx(&self, outpoint: &OutPoint) -> Option<TxRef> {
        self.next_tx.get(outpoint).and_then(|id| self.get(id))
    }
    pub fn info(&self, txid: &TxHash) -> Option<TxMempoolInfo> {
        self.entries.get(txid).map(TxMempoolInfo::from)
    }
    pub fn info_all(&self) -> Vec<TxMempoolInfo> {
        self.get_sorted_depth_and_score()
            .iter()
            .map(|id| TxMempoolInfo::from(&self.entries[id]))
            .collect()
    }

    /// Caller has validated admission. Like Core this assumes no existing children (reorg
    /// relinking belongs to MP1b), and leaves the admission signal/sequence to Finalize.
    pub fn add_unchecked(&mut self, entry: PoolEntry) {
        let ancestors =
            self.assume_calculate_mem_pool_ancestors(&entry, &ancestors::NO_LIMITS, true);
        self.add_unchecked_with_ancestors(entry, &ancestors);
    }

    pub fn add_unchecked_with_ancestors(
        &mut self,
        mut entry: PoolEntry,
        ancestors: &BTreeSet<TxHash>,
    ) {
        let id = entry.txid();
        assert!(!self.entries.contains_key(&id));
        assert!(entry.parents.is_empty() && entry.children.is_empty());
        assert_eq!(entry.modified_fee, entry.metadata.fee);
        assert_eq!(
            entry.vsize,
            entry::virtual_size(&entry.tx, entry.metadata.sigops, self.options.bytes_per_sigop),
            "entry must use the pool's bytes_per_sigop",
        );
        let mut inputs = BTreeSet::new();
        for input in entry.tx.inputs() {
            assert!(!self.next_tx.contains_key(&input.prevout()));
            assert!(inputs.insert(input.prevout()));
        }
        let parents: BTreeSet<_> = entry
            .tx
            .inputs()
            .iter()
            .map(|input| input.prevout().txid())
            .filter(|id| self.entries.contains_key(id))
            .collect();
        let mut delta = 0;
        self.apply_delta(&id, &mut delta);
        entry.update_modified_fee(delta);
        entry.vtx_hashes_idx = self.vtx_hashes.len();
        for input in inputs {
            self.next_tx.insert(input, id);
            self.usage += usage::NEXT_TX_ALLOCATION;
        }
        if self.vtx_hashes.len() == self.vtx_capacity {
            self.resize_hashes_usage((self.vtx_capacity * 2).max(1));
        }
        self.vtx_hashes.push((entry.tx.instance_hash(), id));
        self.usage += usage::ENTRY_ALLOCATION + entry.usage;
        self.total_vsize += entry.vsize;
        self.total_fee += entry.metadata.fee;
        assert!(self.ancestor_score.insert(AncestorScore::from(&entry)));
        for index in &mut self.indexes {
            index.on_insert(&entry);
        }
        self.entries.insert(id, entry);
        for parent in parents {
            self.update_parent(id, parent, true);
        }
        self.update_ancestors_of(true, id, ancestors);
        self.update_entry_for_ancestors(id, ancestors);
        self.on_dash_insert(id);
        self.add_transactions_updated(1);
    }

    pub fn transactions_updated(&self) -> u32 {
        self.transactions_updated.load(Ordering::SeqCst)
    }
    pub fn add_transactions_updated(&self, n: u32) {
        self.transactions_updated.fetch_add(n, Ordering::SeqCst);
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn get_and_increment_sequence(&mut self) -> u64 {
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        sequence
    }
    pub fn take_pending_events(&mut self) -> Vec<PendingEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// Core v24.0.0 `txmempool.h:773-780`: adds a resident transaction to the retry set.
    pub fn add_unbroadcast_tx(&mut self, txid: TxHash) {
        if self.entries.contains_key(&txid) {
            self.unbroadcast_txids.insert(txid);
        }
    }

    /// Core v24.0.0 `txmempool.cpp:1990-1997`: unchecked removal adds a log annotation.
    pub fn remove_unbroadcast_tx(&mut self, txid: &TxHash, unchecked: bool) {
        if self.unbroadcast_txids.remove(txid) {
            tracing::debug!(%txid, "Removed transaction from set of unbroadcast txns{}",
                if unchecked { " before confirmation that txn was sent out" } else { "" });
        }
    }

    /// Core v24.0.0 `txmempool.h:785-790`: returns a snapshot ordered by txid.
    pub fn get_unbroadcast_txs(&self) -> BTreeSet<TxHash> {
        self.unbroadcast_txids.clone()
    }

    pub fn is_unbroadcast_tx(&self, txid: &TxHash) -> bool {
        self.unbroadcast_txids.contains(txid)
    }

    /// Capacity limiting and rolling minimum fees are MP1b's responsibility.
    pub fn min_fee(&self, _size_limit: usize) -> i64 {
        0
    }

    // MP1a-2 supplies these map-backed answers.
    pub fn asset_unlock_txids_by_index(&self, _index: u64) -> Vec<TxHash> {
        Vec::new()
    }
    pub fn pending_asset_unlock_amount(&self) -> i64 {
        0
    }
}

impl MempoolRead for PoolState {
    fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.get(txid)
    }
    fn exists(&self, txid: &TxHash) -> bool {
        self.exists(txid)
    }
    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef> {
        self.get_conflict_tx(outpoint)
    }
    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash> {
        self.asset_unlock_txids_by_index(index)
    }
    fn pending_asset_unlock_amount(&self) -> i64 {
        self.pending_asset_unlock_amount()
    }
}

pub struct Mempool {
    state: Arc<RwLock<PoolState>>,
    transactions_updated: Arc<AtomicU32>,
}

impl Default for Mempool {
    fn default() -> Self {
        Self::new(MemPoolOptions::default())
    }
}

impl Mempool {
    pub fn new(options: MemPoolOptions) -> Self {
        let transactions_updated = Arc::new(AtomicU32::new(0));
        let state = PoolState::new_with_counter(options, transactions_updated.clone());
        Self {
            state: Arc::new(RwLock::new(state)),
            transactions_updated,
        }
    }
    pub fn read(&self) -> RwLockReadGuard<'_, PoolState> {
        self.state.read()
    }
    pub fn write(&self) -> RwLockWriteGuard<'_, PoolState> {
        self.state.write()
    }
    /// For the owned HeldMempool guard introduced by MP1a-4/8.
    pub fn state_lock(&self) -> &Arc<RwLock<PoolState>> {
        &self.state
    }
    pub fn set_is_hooks(&self, hooks: Arc<dyn IsAcceptHooks>) {
        self.state.write().is_hooks = hooks;
    }
    pub fn take_pending_events(&self) -> Vec<PendingEvent> {
        self.state.write().take_pending_events()
    }
    pub fn size(&self) -> usize {
        self.state.read().size()
    }
    pub fn transactions_updated(&self) -> u32 {
        self.transactions_updated.load(Ordering::SeqCst)
    }
    pub fn info(&self, txid: &TxHash) -> Option<TxMempoolInfo> {
        self.state.read().info(txid)
    }
    pub fn info_all(&self) -> Vec<TxMempoolInfo> {
        self.state.read().info_all()
    }
    pub fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> bool {
        self.state.read().compare_depth_and_score(a, b)
    }
    pub fn get_sorted_depth_and_score(&self) -> Vec<TxHash> {
        self.state.read().get_sorted_depth_and_score()
    }
    pub fn add_unbroadcast_tx(&self, txid: TxHash) {
        self.state.write().add_unbroadcast_tx(txid);
    }
    pub fn remove_unbroadcast_tx(&self, txid: &TxHash, unchecked: bool) {
        self.state.write().remove_unbroadcast_tx(txid, unchecked);
    }
    pub fn get_unbroadcast_txs(&self) -> BTreeSet<TxHash> {
        self.state.read().get_unbroadcast_txs()
    }
    pub fn is_unbroadcast_tx(&self, txid: &TxHash) -> bool {
        self.state.read().is_unbroadcast_tx(txid)
    }
}

impl IsMempool for Mempool {
    fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.state.read().get(txid)
    }
    fn exists(&self, txid: &TxHash) -> bool {
        self.state.read().exists(txid)
    }
    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef> {
        self.state.read().get_conflict_tx(outpoint)
    }
    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash> {
        self.state.read().asset_unlock_txids_by_index(index)
    }
    fn pending_asset_unlock_amount(&self) -> i64 {
        self.state.read().pending_asset_unlock_amount()
    }
    fn remove_conflicts_for_lock(&self, txid: &TxHash, inputs: &[OutPoint]) -> Vec<TxRef> {
        self.state.write().remove_conflicts_for_lock(txid, inputs)
    }
    fn add_transactions_updated(&self, n: u32) {
        self.transactions_updated.fetch_add(n, Ordering::SeqCst);
    }
    fn with_read(&self, f: &mut dyn FnMut(&dyn MempoolRead)) {
        f(&*self.state.read());
    }
}
