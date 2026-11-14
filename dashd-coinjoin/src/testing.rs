//! Scriptable in-memory host services. Answers are public fields; calls preserve invocation order.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::dstx::txid;
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_coinjoin::queue::{MixingMasternode, TipMasternodes};
use dash_num::Hash256;
use dash_p2p::{OutMsg, PeerId, Service};
use dash_p2p_core::{DsQueue, Inventory};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash};

use crate::env::{ActiveMn, ChainView, IsLocks, Mempool, MempoolListener, MnLists, PeerSink};
use crate::server::{RandomSource, ServerClock};

fn record<T>(calls: &Mutex<Vec<T>>, call: T) {
    calls.lock().unwrap_or_else(PoisonError::into_inner).push(call);
}

/// Fixed Core time values for server-runtime tests.
#[derive(Clone, Debug, Default)]
pub struct FakeServerClock {
    pub now: i64,
    pub adjusted_time: i64,
}

impl ServerClock for FakeServerClock {
    fn now(&self) -> i64 {
        self.now
    }

    fn adjusted_time(&self) -> i64 {
        self.adjusted_time
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

/// A public identity and fixed signature bytes; no private key or signing logic.
#[derive(Debug)]
pub struct FakeActiveMn {
    pub outpoint: OutPoint,
    pub pro_tx_hash: TxHash,
    pub signature: Vec<u8>,
    pub calls: Mutex<Vec<ActiveMnCall>>,
}

impl Default for FakeActiveMn {
    fn default() -> Self {
        Self {
            outpoint: OutPoint::new(TxHash::default(), u32::MAX),
            pro_tx_hash: TxHash::default(),
            signature: Vec::new(),
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
        self.signature.clone()
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
}

impl MempoolAccept for FakeMempool {
    fn test_accept(&self, tx: &Transaction) -> Option<i64> {
        record(&self.calls, MempoolCall::TestAccept(tx.clone()));
        self.accept_fees.get(&txid(tx)).copied().or(self.default_accept_fee)
    }
}

impl CoinView for FakeMempool {
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        record(&self.calls, MempoolCall::Coin(*outpoint));
        self.coins.get(outpoint).cloned()
    }

    fn is_instantsend_locked(&self, txid: &TxHash) -> bool {
        record(&self.calls, MempoolCall::IsInstantSendLocked(*txid));
        self.locks.is_locked(txid)
    }
}

impl Mempool for FakeMempool {
    fn submit(&mut self, tx: &Transaction) -> bool {
        record(&self.calls, MempoolCall::Submit(tx.clone()));
        self.submit_results.get(&txid(tx)).copied().or(self.default_submit).unwrap_or(false)
    }

    fn prioritise(&mut self, txid: &TxHash, fee_delta: i64) {
        record(&self.calls, MempoolCall::Prioritise(*txid, fee_delta));
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
