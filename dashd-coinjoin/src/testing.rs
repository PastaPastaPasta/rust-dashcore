//! Scriptable in-memory host services. Answers are public fields; calls preserve invocation order.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::dstx::txid;
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_coinjoin::queue::{MixingCounters, MixingMasternode, MixingMeta, TipMasternodes};
use dash_num::Hash256;
use dash_p2p::{OutMsg, PeerId, Service};
use dash_p2p_core::{DsQueue, Inventory};
use dash_pkc::bls::{BlsScIetf, BlsSecretKey, BlsSigId};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash};

use crate::env::{ActiveMn, ChainView, IsLocks, Mempool, MempoolListener, MnLists, PeerSink};
use crate::server::{RandomSource, ServerClock};

fn record<T>(calls: &Mutex<Vec<T>>, call: T) {
    calls.lock().unwrap_or_else(PoisonError::into_inner).push(call);
}

/// Fixed Core time values for server-runtime tests.
#[derive(Clone, Debug)]
pub struct FakeServerClock {
    now: Arc<AtomicI64>,
    adjusted_time: Arc<AtomicI64>,
}

impl FakeServerClock {
    pub fn at(now: i64) -> Self {
        Self {
            now: Arc::new(AtomicI64::new(now)),
            adjusted_time: Arc::new(AtomicI64::new(now)),
        }
    }

    pub fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
        self.adjusted_time.store(now, Ordering::SeqCst);
    }
}

impl ServerClock for FakeServerClock {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }

    fn adjusted_time(&self) -> i64 {
        self.adjusted_time.load(Ordering::SeqCst)
    }
}

/// Scripted random draws for server-runtime tests; exhausted draws return zero.
#[derive(Debug, Default)]
pub struct FakeRandom {
    pub draws: Vec<u32>,
    pub calls: Mutex<Vec<u32>>,
}

impl RandomSource for FakeRandom {
    fn below(&mut self, upper: u32) -> u32 {
        record(&self.calls, upper);
        self.draws.pop().unwrap_or(0)
    }
}

/// An active-masternode query or signing request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActiveMnCall {
    Outpoint,
    ProTxHash,
    SignBasic(Hash256),
}

/// A public identity that answers fixed signature bytes, or signs with a test `key` when set.
#[derive(Debug)]
pub struct FakeActiveMn {
    pub outpoint: OutPoint,
    pub pro_tx_hash: TxHash,
    pub signature: Vec<u8>,
    pub key: Option<BlsSecretKey<BlsScIetf>>,
    pub calls: Mutex<Vec<ActiveMnCall>>,
}

impl Default for FakeActiveMn {
    fn default() -> Self {
        Self {
            outpoint: OutPoint::new(TxHash::default(), u32::MAX),
            pro_tx_hash: TxHash::default(),
            signature: Vec::new(),
            key: None,
            calls: Mutex::default(),
        }
    }
}

impl ActiveMn for FakeActiveMn {
    fn outpoint(&self) -> OutPoint {
        record(&self.calls, ActiveMnCall::Outpoint);
        self.outpoint
    }

    fn pro_tx_hash(&self) -> TxHash {
        record(&self.calls, ActiveMnCall::ProTxHash);
        self.pro_tx_hash
    }

    fn sign_basic(&mut self, hash: &Hash256) -> Vec<u8> {
        record(&self.calls, ActiveMnCall::SignBasic(*hash));
        match &self.key {
            Some(key) => key.sign_with(hash.as_bytes(), BlsSigId::Basic).to_bytes().to_vec(),
            None => self.signature.clone(),
        }
    }
}

/// A tip or historical masternode-list query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MnListsCall {
    ValidMn(TxHash),
    EnabledCount,
    ValidByCollateral(OutPoint),
    MnAtDepth(i32, TxHash),
}

/// Independently scripted tip, collateral and historical lookups. Missing entries return `None`.
#[derive(Debug, Default)]
pub struct FakeMnLists {
    pub valid: BTreeMap<TxHash, MixingMasternode>,
    pub by_collateral: BTreeMap<OutPoint, MixingMasternode>,
    pub at_depth: BTreeMap<(i32, TxHash), MixingMasternode>,
    pub enabled: i32,
    pub calls: Mutex<Vec<MnListsCall>>,
}

impl TipMasternodes for FakeMnLists {
    fn valid_mn(&self, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        record(&self.calls, MnListsCall::ValidMn(*pro_tx_hash));
        self.valid.get(pro_tx_hash).cloned()
    }

    fn enabled_count(&self) -> i32 {
        record(&self.calls, MnListsCall::EnabledCount);
        self.enabled
    }
}

impl MnLists for FakeMnLists {
    fn valid_by_collateral(&self, outpoint: &OutPoint) -> Option<MixingMasternode> {
        record(&self.calls, MnListsCall::ValidByCollateral(*outpoint));
        self.by_collateral.get(outpoint).cloned()
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        record(&self.calls, MnListsCall::MnAtDepth(depth, *pro_tx_hash));
        self.at_depth.get(&(depth, *pro_tx_hash)).cloned()
    }
}

/// A masternode listed from block height `added` up to, but not including, `removed`.
#[derive(Clone, Debug)]
pub struct MnHistoryEntry {
    pub mn: MixingMasternode,
    pub added: i32,
    pub removed: Option<i32>,
}

impl MnHistoryEntry {
    fn listed_at(&self, height: i32) -> bool {
        height >= self.added && self.removed.is_none_or(|removed| height < removed)
    }
}

/// Masternode lists by block height: the tip list is the one at `tip`, and depth `d` reads the
/// list at `tip - d`. Every query is recorded.
#[derive(Debug, Default)]
pub struct FakeMnHistory {
    pub tip: i32,
    pub entries: BTreeMap<TxHash, MnHistoryEntry>,
    pub calls: Mutex<Vec<MnListsCall>>,
}

impl FakeMnHistory {
    fn listed_at(&self, height: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.entries
            .get(pro_tx_hash)
            .filter(|entry| entry.listed_at(height))
            .map(|entry| entry.mn.clone())
    }
}

impl TipMasternodes for FakeMnHistory {
    fn valid_mn(&self, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        record(&self.calls, MnListsCall::ValidMn(*pro_tx_hash));
        self.listed_at(self.tip, pro_tx_hash)
    }

    fn enabled_count(&self) -> i32 {
        record(&self.calls, MnListsCall::EnabledCount);
        let listed = self.entries.values().filter(|entry| entry.listed_at(self.tip)).count();
        i32::try_from(listed).unwrap_or(i32::MAX)
    }
}

impl MnLists for FakeMnHistory {
    fn valid_by_collateral(&self, outpoint: &OutPoint) -> Option<MixingMasternode> {
        record(&self.calls, MnListsCall::ValidByCollateral(*outpoint));
        self.entries
            .values()
            .find(|entry| entry.mn.collateral == *outpoint && entry.listed_at(self.tip))
            .map(|entry| entry.mn.clone())
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        record(&self.calls, MnListsCall::MnAtDepth(depth, *pro_tx_hash));
        if depth < 0 {
            return None;
        }
        self.listed_at(self.tip.checked_sub(depth)?, pro_tx_hash)
    }
}

/// A scripted InstantSend lock set, recording every query.
#[derive(Debug, Default)]
pub struct FakeIsLocks {
    pub locked: BTreeSet<TxHash>,
    pub calls: Mutex<Vec<TxHash>>,
}

impl IsLocks for FakeIsLocks {
    fn is_locked(&self, txid: &TxHash) -> bool {
        record(&self.calls, *txid);
        self.locked.contains(txid)
    }
}

/// A [`MixingMeta`] behind a cloneable shared handle, shaped like the node's meta-manager
/// adapter rather than the plain [`MixingCounters`] value.
#[derive(Clone, Debug, Default)]
pub struct SharedMeta(pub Arc<Mutex<MixingCounters>>);

impl SharedMeta {
    pub fn counters(&self) -> MixingCounters {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl MixingMeta for SharedMeta {
    fn is_mixing_threshold_exceeded(&self, pro_tx_hash: &TxHash, mn_count: i32) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_mixing_threshold_exceeded(pro_tx_hash, mn_count)
    }

    fn allow_mixing(&mut self, pro_tx_hash: &TxHash) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).allow_mixing(pro_tx_hash);
    }

    fn disallow_mixing(&mut self, pro_tx_hash: &TxHash) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).disallow_mixing(pro_tx_hash);
    }

    fn is_valid_for_mixing_txes(&self, pro_tx_hash: &TxHash) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).is_valid_for_mixing_txes(pro_tx_hash)
    }
}

/// A mempool or coins-view call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MempoolCall {
    TestAccept(Transaction),
    Submit(Transaction),
    Prioritise(TxHash, i64),
    Coin(OutPoint),
    IsInstantSendLocked(TxHash),
}

/// Per-transaction admission answers and coins. Unscripted transactions are rejected;
/// prioritisation and submission only record calls and never change the scripted answers.
#[derive(Debug, Default)]
pub struct FakeMempool {
    pub default_accept_fee: Option<i64>,
    pub default_submit: Option<bool>,
    pub accept_fees: BTreeMap<TxHash, i64>,
    pub submit_results: BTreeMap<TxHash, bool>,
    pub coins: BTreeMap<OutPoint, ViewCoin>,
    pub locks: FakeIsLocks,
    pub calls: Mutex<Vec<MempoolCall>>,
    pub observer: Option<std::sync::Arc<Mutex<Vec<MempoolCall>>>>,
}

fn record_mempool_call(
    calls: &Mutex<Vec<MempoolCall>>,
    observer: &Option<std::sync::Arc<Mutex<Vec<MempoolCall>>>>,
    call: MempoolCall,
) {
    record(calls, call.clone());
    if let Some(observer) = observer {
        record(observer, call);
    }
}

impl MempoolAccept for FakeMempool {
    fn test_accept(&self, tx: &Transaction) -> Option<i64> {
        record_mempool_call(&self.calls, &self.observer, MempoolCall::TestAccept(tx.clone()));
        self.accept_fees.get(&txid(tx)).copied().or(self.default_accept_fee)
    }
}

impl CoinView for FakeMempool {
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        record_mempool_call(&self.calls, &self.observer, MempoolCall::Coin(*outpoint));
        self.coins.get(outpoint).cloned()
    }

    fn is_instantsend_locked(&self, txid: &TxHash) -> bool {
        record_mempool_call(&self.calls, &self.observer, MempoolCall::IsInstantSendLocked(*txid));
        self.locks.is_locked(txid)
    }
}

impl Mempool for FakeMempool {
    fn submit(&mut self, tx: &Transaction) -> bool {
        record_mempool_call(&self.calls, &self.observer, MempoolCall::Submit(tx.clone()));
        self.submit_results.get(&txid(tx)).copied().or(self.default_submit).unwrap_or(false)
    }

    fn prioritise(&mut self, txid: &TxHash, fee_delta: i64) {
        record_mempool_call(&self.calls, &self.observer, MempoolCall::Prioritise(*txid, fee_delta));
    }
}

/// Records transaction-added notifications without admitting transactions itself.
#[derive(Debug, Default)]
pub struct FakeMempoolListener {
    pub transactions: Mutex<Vec<Transaction>>,
}

impl MempoolListener for FakeMempoolListener {
    fn transaction_added_to_mempool(&self, tx: &Transaction) {
        record(&self.transactions, tx.clone());
    }
}

/// A chain or sync-status query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainViewCall {
    Tip,
    V24ActiveAtTip,
    V24ActiveAfterTip,
    HasChainLock(i32, BlockHash),
    BlockchainSynced,
}

/// Scripted chain, activation and sync status. By default there is no tip, lock or sync.
#[derive(Debug, Default)]
pub struct FakeChainView {
    pub tip: Option<(i32, BlockHash)>,
    pub v24_at_tip: bool,
    pub v24_after_tip: bool,
    pub chain_locks: BTreeSet<(i32, BlockHash)>,
    pub synced: bool,
    pub calls: Mutex<Vec<ChainViewCall>>,
}

impl ChainView for FakeChainView {
    fn tip(&self) -> Option<(i32, BlockHash)> {
        record(&self.calls, ChainViewCall::Tip);
        self.tip
    }

    fn v24_active_at_tip(&self) -> bool {
        record(&self.calls, ChainViewCall::V24ActiveAtTip);
        self.v24_at_tip
    }

    fn v24_active_after_tip(&self) -> bool {
        record(&self.calls, ChainViewCall::V24ActiveAfterTip);
        self.v24_after_tip
    }

    fn has_chain_lock(&self, height: i32, hash: &BlockHash) -> bool {
        record(&self.calls, ChainViewCall::HasChainLock(height, *hash));
        self.chain_locks.contains(&(height, *hash))
    }

    fn blockchain_synced(&self) -> bool {
        record(&self.calls, ChainViewCall::BlockchainSynced);
        self.synced
    }
}

/// A peer effect, retained in order across pushes, scores, requests and relays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerCall {
    PushTo(PeerId, OutMsg),
    PushToAddr(Service, OutMsg),
    Misbehaving(PeerId, i32, String),
    EraseObjectRequest(PeerId, Inventory),
    ForgetObjectRequest(Inventory),
    RelayInv(Inventory),
    RelayTxInv(Inventory),
    RelayDsq(DsQueue),
    RelayTransaction(TxHash),
}

/// Records every attempted peer effect, and every connectivity query apart in `queries`.
/// Address pushes and queries succeed only for `connected` addresses.
#[derive(Debug, Default)]
pub struct FakePeerSink {
    pub connected: BTreeSet<Service>,
    pub calls: Vec<PeerCall>,
    pub queries: Mutex<Vec<Service>>,
    pub observer: Option<std::sync::Arc<Mutex<Vec<PeerCall>>>>,
}

impl FakePeerSink {
    fn record_call(&mut self, call: PeerCall) {
        self.calls.push(call.clone());
        if let Some(observer) = &self.observer {
            record(observer, call);
        }
    }
}

impl PeerSink for FakePeerSink {
    fn push_to(&mut self, peer: PeerId, msg: OutMsg) {
        self.record_call(PeerCall::PushTo(peer, msg));
    }

    fn push_to_addr(&mut self, addr: &Service, msg: OutMsg) -> bool {
        self.record_call(PeerCall::PushToAddr(addr.clone(), msg));
        self.connected.contains(addr)
    }

    fn is_connected(&self, addr: &Service) -> bool {
        record(&self.queries, addr.clone());
        self.connected.contains(addr)
    }

    fn misbehaving(&mut self, peer: PeerId, score: i32, reason: &str) {
        self.record_call(PeerCall::Misbehaving(peer, score, reason.to_owned()));
    }

    fn erase_object_request(&mut self, peer: PeerId, inv: Inventory) {
        self.record_call(PeerCall::EraseObjectRequest(peer, inv));
    }

    fn forget_object_request(&mut self, inv: Inventory) {
        self.record_call(PeerCall::ForgetObjectRequest(inv));
    }

    fn relay_inv(&mut self, inv: Inventory) {
        self.record_call(PeerCall::RelayInv(inv));
    }

    fn relay_tx_inv(&mut self, inv: Inventory) {
        self.record_call(PeerCall::RelayTxInv(inv));
    }

    fn relay_dsq(&mut self, dsq: &DsQueue) {
        self.record_call(PeerCall::RelayDsq(dsq.clone()));
    }

    fn relay_transaction(&mut self, txid: &TxHash) {
        self.record_call(PeerCall::RelayTransaction(*txid));
    }
}

#[cfg(test)]
mod tests;
