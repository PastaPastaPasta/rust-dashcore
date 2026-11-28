//! Pool core. Public handle methods lock once; all graph/index helpers operate on PoolState.

pub mod ancestors;
mod check;
mod dash;
mod deltas;
pub mod entry;
pub mod index;
mod limit;
mod relay;
mod remove;
mod update;
pub mod usage;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

use dash_clock::{Clock, SystemClock};
use dash_consensus::{Amount, TxRef};
use dash_primitives::{OutPoint, TxHash};
use dashd_chainstate::chain::ValidationSignals;
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::api::{MempoolRead, MiningEntry};
use crate::is::{IsAcceptHooks, NoIsHooks};
use crate::options::MemPoolOptions;
use crate::types::MemPoolRemovalReason;

pub use entry::{EntryMetadata, PackageStats, PoolEntry, TxMempoolInfo};
use index::{AncestorScore, DescendantScore, PoolIndex};
use limit::RollingFee;
pub use limit::{ROLLING_FEE_HALFLIFE, fee_rate_to_string};

/// Caller drains this under its write guard into the ordered signal stream ([`crate::events`]).
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
    descendant_score: BTreeSet<DescendantScore>,
    entry_time: BTreeSet<(i64, TxHash)>,
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
    /// Core's `GetTime()`, the node clock: expiry and the rolling fee's decay read it.
    clock: Arc<dyn Clock>,
    /// Core's `mutable` rolling-fee fields under `cs`: `GetMinFee` updates them from a `const`
    /// call, so a read guard reaches them through this leaf lock, taken last and never held
    /// across another.
    rolling: Mutex<RollingFee>,
    pub(crate) dash: dash::DashMaps,
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
            descendant_score: BTreeSet::new(),
            entry_time: BTreeSet::new(),
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
            clock: Arc::new(SystemClock),
            rolling: Mutex::new(RollingFee::default()),
            dash: dash::DashMaps::default(),
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
    pub fn set_clock(&mut self, clock: Arc<dyn Clock>) {
        self.clock = clock;
    }
    /// `GetTime()` in seconds.
    pub fn now(&self) -> i64 {
        self.clock.node_now().secs()
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

    /// Caller has validated admission. Like Core this assumes no existing children (a reorg's
    /// re-add relinks them, [`Self::update_transactions_from_block`]), and leaves the admission
    /// signal/sequence to Finalize.
    pub fn add_unchecked(&mut self, entry: PoolEntry, tip_list: &dash_dml::MnList) {
        self.add_unchecked_at(entry, tip_list);
    }

    /// Adds an entry and indexes Dash payload conflicts using the list supplied by this V turn.
    pub fn add_unchecked_at(&mut self, entry: PoolEntry, tip_list: &dash_dml::MnList) {
        let ancestors =
            self.assume_calculate_mem_pool_ancestors(&entry, &ancestors::NO_LIMITS, true);
        self.add_unchecked_with_ancestors_at(entry, &ancestors, tip_list);
    }

    pub fn add_unchecked_with_ancestors(
        &mut self,
        entry: PoolEntry,
        ancestors: &BTreeSet<TxHash>,
        tip_list: &dash_dml::MnList,
    ) {
        self.add_unchecked_with_ancestors_at(entry, ancestors, tip_list);
    }

    fn add_unchecked_with_ancestors_at(
        &mut self,
        mut entry: PoolEntry,
        ancestors: &BTreeSet<TxHash>,
        tip_list: &dash_dml::MnList,
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
        assert!(self.descendant_score.insert(DescendantScore::from(&entry)));
        assert!(self.entry_time.insert(index::entry_time(&entry)));
        for index in &mut self.indexes {
            index.on_insert(&entry);
        }
        self.entries.insert(id, entry);
        for parent in parents {
            self.update_parent(id, parent, true);
        }
        self.update_ancestors_of(true, id, ancestors);
        self.update_entry_for_ancestors(id, ancestors);
        self.on_dash_insert_at(id, tip_list);
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
    /// `mapTx.modify(it, UpdateLockPoints)` (`validation.cpp:496` at v24.0.0).
    pub fn update_lock_points(&mut self, txid: TxHash, lock_points: crate::types::LockPoints) {
        self.modify_entry(txid, |entry| entry.update_lock_points(lock_points));
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

    pub fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash> {
        self.dash.asset_unlock_indexes.get(&index).cloned().unwrap_or_default()
    }
    pub fn pending_asset_unlock_amount(&self) -> i64 {
        self.dash.pending_asset_unlock_amount.clamp(0, i64::MAX as i128) as i64
    }
}

impl MempoolRead for PoolState {
    fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.get(txid)
    }
    fn exists(&self, txid: &TxHash) -> bool {
        self.exists(txid)
    }
    fn ancestor_score_iter(&self) -> Box<dyn Iterator<Item = TxHash> + '_> {
        Box::new(self.ancestor_score_order())
    }
    fn mining_entry(&self, txid: &TxHash) -> Option<MiningEntry> {
        self.entries.get(txid).map(|e| MiningEntry {
            tx: e.tx.clone(),
            txid: e.txid(),
            fee: e.metadata.fee,
            modified_fee: e.modified_fee,
            tx_size: e.vsize,
            sigops: e.metadata.sigops,
            size_with_ancestors: e.ancestors.size,
            mod_fees_with_ancestors: e.ancestors.modified_fees,
            sigops_with_ancestors: e.ancestor_sigops,
            count_with_ancestors: e.ancestors.count,
        })
    }
    fn ancestors_no_limits(&self, txid: &TxHash) -> Vec<TxHash> {
        self.entries.get(txid).map_or_else(Vec::new, |e| {
            self.assume_calculate_mem_pool_ancestors(e, &ancestors::NO_LIMITS, false)
                .into_iter()
                .collect()
        })
    }
    fn descendants(&self, txid: &TxHash) -> Vec<TxHash> {
        self.calculate_descendants(*txid).into_iter().collect()
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
    fn service_updates_before_key_change(
        &self,
        key_change: &TxHash,
        tip_list: &dash_dml::MnList,
    ) -> Vec<TxHash> {
        self.service_updates_before_key_change(*key_change, tip_list)
    }
    fn is_unorderable_service_update(
        &self,
        tx: &dash_consensus::Tx,
        ancestors: &[TxHash],
        tip_list: &dash_dml::MnList,
    ) -> bool {
        self.is_unorderable_service_update(tx, ancestors, tip_list)
    }
    fn key_change_target(
        &self,
        tx: &dash_consensus::Tx,
        tip_list: &dash_dml::MnList,
    ) -> Option<dash_dml::ProTxHash> {
        self.key_change_target(tx, tip_list)
    }
}

/// The pool behind its lock (`m_pool.cs`, §3): every public method takes the lock once, and
/// publishes the usage telemetry before it releases a write guard.
pub struct Mempool {
    state: Arc<RwLock<PoolState>>,
    transactions_updated: Arc<AtomicU32>,
    usage: AtomicU64,
    signals: ValidationSignals,
    /// The admission FIFO and its worker, from [`Mempool::attach_chain`] to
    /// [`Mempool::detach_chain`].
    pub(crate) admissions: crate::handle::Admissions,
}

impl Default for Mempool {
    fn default() -> Self {
        Self::new(MemPoolOptions::default())
    }
}

impl Mempool {
    /// A pool whose off-V events go to a queue of its own, which no listener reads.
    pub fn new(options: MemPoolOptions) -> Self {
        Self::with_signals(options, ValidationSignals::default())
    }

    /// A pool whose off-V events (`remove_conflicts_for_lock`) go to the node's signals queue.
    pub fn with_signals(options: MemPoolOptions, signals: ValidationSignals) -> Self {
        let transactions_updated = Arc::new(AtomicU32::new(0));
        let state = PoolState::new_with_counter(options, transactions_updated.clone());
        Self {
            state: Arc::new(RwLock::new(state)),
            transactions_updated,
            usage: AtomicU64::new(0),
            signals,
            admissions: crate::handle::Admissions::default(),
        }
    }
    pub fn read(&self) -> RwLockReadGuard<'_, PoolState> {
        self.state.read()
    }
    /// The caller publishes the usage telemetry ([`Self::publish_usage`]) before it releases.
    pub fn write(&self) -> RwLockWriteGuard<'_, PoolState> {
        self.state.write()
    }
    /// For the owned guard of a hold ([`crate::sink::PoolSink`]).
    pub fn state_lock(&self) -> &Arc<RwLock<PoolState>> {
        &self.state
    }
    /// Records `state`'s usage as the telemetry copy. Telemetry only: the flush reads the pool.
    pub fn publish_usage(&self, state: &PoolState) {
        self.usage.store(state.dynamic_memory_usage() as u64, Ordering::Relaxed);
    }
    /// The usage last published under a write guard: telemetry, never a flush input (§3).
    pub fn usage_telemetry(&self) -> u64 {
        self.usage.load(Ordering::Relaxed)
    }
    /// The signals queue off-V removals are queued to.
    pub fn signals(&self) -> &ValidationSignals {
        &self.signals
    }
    pub fn set_is_hooks(&self, hooks: Arc<dyn IsAcceptHooks>) {
        self.state.write().is_hooks = hooks;
    }
    /// The node clock (`GetTime()`, mockable) that expiry and the rolling fee read; a pool
    /// starts on the system clock.
    pub fn set_clock(&self, clock: Arc<dyn Clock>) {
        self.state.write().clock = clock;
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
    pub(crate) fn add_transactions_updated(&self, n: u32) {
        self.transactions_updated.fetch_add(n, Ordering::SeqCst);
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

#[cfg(test)]
#[path = "mining_tests.rs"]
mod mining_tests;
