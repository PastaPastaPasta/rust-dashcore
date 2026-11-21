//! `CInstantSendManager`'s in-memory state (PLAN-LK1b §6, slice S2): the two pending-lock
//! queues, the non-locked transaction graph, the retry set and the islock timing map, with the
//! lookups that read them beside the IS store.
//!
//! Locks (Core: `instantsend/instantsend.h:57-93`). Each Core mutex is one `parking_lot` mutex
//! here, with the same nesting:
//! - `pending` is `cs_pendingLocks`, `non_locked` is `cs_nonLocked`, `pending_retry` is
//!   `cs_pendingRetry`, `timings` is `cs_timingsTxSeen`;
//! - the only nested holds are `non_locked` → `pending_retry` and `non_locked` → `timings`
//!   (`RemoveNonLockedTx`, `RetryUnminedAssetUnlocks`, `PrepareTxToRetry`), as in Core;
//! - no store call is made while one of these mutexes is held, as Core makes no `cs_db` call
//!   under them, so the store's attempt gate never waits under a manager lock.
//!
//! Core's maps are salted unordered maps. These are ordered by key; the iteration order of
//! `FetchPendingLocks`, `AttachISLockToTx` and `PrepareTxToRetry` is then one of Core's possible
//! schedules (§5 item 4).
//!
//! `CBlockIndex* pindexMined` is a [`MinedBlock`]. The block-height cache
//! (`instantsend.h:89-93`) stays where LK1b-s put it: the tip height is [`super::IsTipCache`],
//! and `m_cached_block_heights` is not ported (`listener.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use dash_clock::Clock;
use dash_consensus::primitives::{OutPoint, TxHash};
use dash_consensus::{BlockId, Tx};
use dash_evo::locks::isdlock_hash;
use dash_kv::Result;
use dash_num::Hash256;
use dash_primitives::BlockHash;
use parking_lot::Mutex;

use super::chain::GateSource;
use super::seams::IsManagerStore;
use super::{IsLockHash, LockedTx, lock_inputs};

/// Core's `NodeId`.
pub type NodeId = i64;

/// The cap on each pending queue. Core: `instantsend/instantsend.h:57-63`.
pub const MAX_PENDING_INSTANTSEND_LOCKS: usize = 1024;

/// The most locks one `FetchPendingLocks` hands out. Core: `instantsend/instantsend.cpp:65`.
pub const MAX_FETCHED_PENDING_LOCKS: usize = 32;

/// `instantsend::PendingISLockFromPeer`. Core: `instantsend/instantsend.h:34-37`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingIsLockFromPeer {
    pub node_id: NodeId,
    pub islock: Arc<LockedTx>,
}

/// `instantsend::PendingISLockEntry`. Core: `instantsend/instantsend.h:39-41`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingIsLockEntry {
    pub node_id: NodeId,
    pub islock: Arc<LockedTx>,
    pub islock_hash: IsLockHash,
}

/// `instantsend::PendingState`. Core: `instantsend/instantsend.h:43-46`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PendingState {
    /// More locks were left in the queue.
    pub pending_work: bool,
    pub pending_is: Vec<PendingIsLockEntry>,
}

/// The block a non-locked transaction was mined in: Core's `const CBlockIndex*`, whose identity
/// is `id` and whose `nHeight` and `GetBlockHash()` S4 reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MinedBlock {
    pub id: BlockId,
    pub height: u32,
    pub hash: BlockHash,
}

/// `CInstantSendManager::Counts`. Core: `instantsend/instantsend.h:152-157`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counts {
    pub verified: usize,
    pub unverified: usize,
    pub awaiting_tx: usize,
    pub unprotected_tx: usize,
}

/// `NonLockedTxInfo`. A parent that is only referenced by a child has no `tx`.
/// Core: `instantsend/instantsend.h:71-77`.
#[derive(Default)]
struct NonLockedTxInfo {
    mined: Option<MinedBlock>,
    tx: Option<Arc<Tx>>,
    children: BTreeSet<TxHash>,
}

/// `cs_pendingLocks`' maps. Core: `instantsend/instantsend.h:65-69`.
#[derive(Debug, Default)]
struct PendingLocks {
    /// `pendingInstantSendLocks`: received, not verified yet.
    incoming: BTreeMap<IsLockHash, PendingIsLockFromPeer>,
    /// `pendingNoTxInstantSendLocks`: verified, the transaction not seen yet.
    no_tx: BTreeMap<IsLockHash, PendingIsLockFromPeer>,
}

/// `cs_nonLocked`'s maps. Core: `instantsend/instantsend.h:79-81`.
#[derive(Default)]
struct NonLocked {
    txs: BTreeMap<TxHash, NonLockedTxInfo>,
    by_outpoint: BTreeMap<OutPoint, TxHash>,
}

/// `CInstantSendManager` without its store-only pass-throughs (see the module doc).
pub struct IsManager {
    store: Arc<dyn IsManagerStore>,
    gate: GateSource,
    clock: Arc<dyn Clock>,
    pending: Mutex<PendingLocks>,
    non_locked: Mutex<NonLocked>,
    pending_retry: Mutex<BTreeSet<TxHash>>,
    timings: Mutex<BTreeMap<TxHash, Instant>>,
}

/// Core's `ShouldReportISLockTiming`: the stats client (not ported) or the `instantsend` debug
/// category. Core: `instantsend/instantsend.cpp:25`.
fn should_report_timing() -> bool {
    tracing::enabled!(target: "instantsend", tracing::Level::DEBUG)
}

/// `COutPoint::ToStringShort`.
fn short(outpoint: &OutPoint) -> String {
    format!("{}-{}", outpoint.txid(), outpoint.index())
}

impl IsManager {
    pub fn new(store: Arc<dyn IsManagerStore>, gate: GateSource, clock: Arc<dyn Clock>) -> Self {
        IsManager {
            store,
            gate,
            clock,
            pending: Mutex::default(),
            non_locked: Mutex::default(),
            pending_retry: Mutex::default(),
            timings: Mutex::default(),
        }
    }

    /// `IsInstantSendEnabled`. Core: `instantsend/instantsend.cpp:513-516`.
    pub fn is_instant_send_enabled(&self) -> bool {
        (self.gate)().enabled()
    }

    /// `EnqueueInstantSendLock`: queue a received lock, dropped while the queue holds 1024.
    /// Core: `instantsend/instantsend.cpp:27-56`.
    pub fn enqueue_instant_send_lock(&self, from: NodeId, hash: IsLockHash, islock: Arc<LockedTx>) {
        if should_report_timing() {
            let seen = self.timings.lock().remove(&islock.txid());
            let diff = seen
                .map_or(0, |seen| self.clock.steady().saturating_duration_since(seen).as_millis());
            tracing::debug!(
                target: "instantsend",
                "CInstantSendManager::EnqueueInstantSendLock -- txid={}, islock took {}ms",
                islock.txid(),
                diff,
            );
        }
        let mut pending = self.pending.lock();
        if pending.incoming.len() >= MAX_PENDING_INSTANTSEND_LOCKS {
            tracing::debug!(
                target: "instantsend",
                "CInstantSendManager::EnqueueInstantSendLock -- pending islock queue full ({}), \
                 dropping islock={} peer={}",
                pending.incoming.len(),
                Hash256::from(hash),
                from,
            );
            return;
        }
        pending.incoming.entry(hash).or_insert(PendingIsLockFromPeer {
            node_id: from,
            islock,
        });
    }

    /// `FetchPendingLocks`: take at most 32 received locks; `pending_work` when more remain.
    /// Core: `instantsend/instantsend.cpp:58-85`.
    pub fn fetch_pending_locks(&self) -> PendingState {
        let mut pending = self.pending.lock();
        let mut ret = PendingState::default();
        while ret.pending_is.len() < MAX_FETCHED_PENDING_LOCKS {
            let Some((islock_hash, entry)) = pending.incoming.pop_first() else {
                break;
            };
            ret.pending_is.push(PendingIsLockEntry {
                node_id: entry.node_id,
                islock: entry.islock,
                islock_hash,
            });
        }
        ret.pending_work = !pending.incoming.is_empty();
        ret
    }

    /// `PreVerifyIsLock`: false for a known lock or a transaction that already has one; a stored
    /// lock on one of the inputs is only logged. Core: `instantsend/instantsend.cpp:87-105`.
    pub fn pre_verify_is_lock(
        &self,
        hash: &IsLockHash,
        islock: &LockedTx,
        from: NodeId,
    ) -> Result<bool> {
        if self.store.known(hash)? {
            return Ok(false);
        }
        if self.store.by_txid(&islock.txid())?.is_some() {
            return Ok(false);
        }
        for input in islock.inputs() {
            if let Some(other) = self.store.by_input(input)? {
                tracing::info!(
                    "CInstantSendManager::PreVerifyIsLock -- txid={}, islock={}: conflicting \
                     outpoint in islock. input={}, other islock={}, peer={}",
                    islock.txid(),
                    Hash256::from(*hash),
                    short(input),
                    Hash256::from(isdlock_hash(&other)),
                    from,
                );
            }
        }
        Ok(true)
    }

    /// `AddPendingISLock`: park a verified lock whose transaction is missing, dropped while the
    /// no-tx queue holds 1024. Core: `instantsend/instantsend.cpp:116-129`.
    pub fn add_pending_is_lock(&self, hash: IsLockHash, islock: Arc<LockedTx>, from: NodeId) {
        let mut pending = self.pending.lock();
        if pending.no_tx.len() >= MAX_PENDING_INSTANTSEND_LOCKS {
            tracing::debug!(
                target: "instantsend",
                "CInstantSendManager::AddPendingISLock -- no-tx pending islock queue full ({}), \
                 dropping islock={} peer={}",
                pending.no_tx.len(),
                Hash256::from(hash),
                from,
            );
            return;
        }
        pending.no_tx.entry(hash).or_insert(PendingIsLockFromPeer {
            node_id: from,
            islock,
        });
    }

    /// `AttachISLockToTx`: move the first parked lock for `tx` back to the received queue (that
    /// queue's cap is not checked here) and return it. Core: `instantsend/instantsend.cpp:160-175`.
    pub fn attach_is_lock_to_tx(&self, tx: &Tx) -> Option<Arc<LockedTx>> {
        let txid = tx.txid();
        let mut pending = self.pending.lock();
        let hash = *pending.no_tx.iter().find(|(_, p)| p.islock.txid() == txid)?.0;
        tracing::debug!(
            target: "instantsend",
            "CInstantSendManager::AttachISLockToTx -- txid={}, islock={}",
            txid,
            Hash256::from(hash),
        );
        let entry = pending.no_tx.remove(&hash)?;
        let islock = Arc::clone(&entry.islock);
        pending.incoming.entry(hash).or_insert(entry);
        Some(islock)
    }

    /// `AddNonLockedTx`: track `tx` (mined in `mined`, or unmined), then attach a parked lock.
    /// A first insert links the transaction to its lock inputs' parents and outpoints; an
    /// existing entry, a parent placeholder included, only takes the new block and, when its
    /// instance differs, the new transaction. Core: `instantsend/instantsend.cpp:177-207`.
    pub fn add_non_locked_tx(&self, tx: &Arc<Tx>, mined: Option<MinedBlock>) {
        let txid = tx.txid();
        {
            let mut guard = self.non_locked.lock();
            let nl = &mut *guard;
            let did_insert = !nl.txs.contains_key(&txid);
            let info = nl.txs.entry(txid).or_default();
            info.mined = mined;
            if did_insert {
                info.tx = Some(Arc::clone(tx));
                for outpoint in lock_inputs(tx) {
                    nl.txs.entry(outpoint.txid()).or_default().children.insert(txid);
                    nl.by_outpoint.entry(outpoint).or_insert(txid);
                }
            } else if info.tx.as_ref().is_none_or(|t| t.instance_hash() != tx.instance_hash()) {
                info.tx = Some(Arc::clone(tx));
            }
        }
        self.attach_is_lock_to_tx(tx);
        if should_report_timing() {
            self.timings.lock().entry(txid).or_insert_with(|| self.clock.steady());
        }
        tracing::debug!(
            target: "instantsend",
            "CInstantSendManager::AddNonLockedTx -- txid={}, pindexMined={}",
            txid,
            mined.map(|m| m.hash.to_string()).unwrap_or_default(),
        );
    }

    /// `RemoveNonLockedTx`: stop tracking `txid`, queueing its children for a retry when
    /// `retry_children`; a mined entry stays when `keep_mined`.
    /// Core: `instantsend/instantsend.cpp:209-255`.
    pub fn remove_non_locked_tx(&self, txid: &TxHash, retry_children: bool, keep_mined: bool) {
        let mut guard = self.non_locked.lock();
        let nl = &mut *guard;
        let Some(info) = nl.txs.get(txid) else {
            return;
        };
        if keep_mined && info.mined.is_some() {
            return;
        }
        let mut retry_children_count = 0usize;
        if retry_children {
            let mut retry = self.pending_retry.lock();
            for child in &info.children {
                retry.insert(*child);
                retry_children_count += 1;
            }
        }
        self.pending_retry.lock().remove(txid);
        if let Some(tx) = info.tx.clone() {
            for outpoint in lock_inputs(&tx) {
                let parent = outpoint.txid();
                if let Some(parent_info) = nl.txs.get_mut(&parent) {
                    parent_info.children.remove(txid);
                    if parent_info.tx.is_none() && parent_info.children.is_empty() {
                        nl.txs.remove(&parent);
                    }
                }
                if nl.by_outpoint.get(&outpoint) == Some(txid) {
                    nl.by_outpoint.remove(&outpoint);
                }
            }
        }
        nl.txs.remove(txid);
        self.timings.lock().remove(txid);
        tracing::debug!(
            target: "instantsend",
            "CInstantSendManager::RemoveNonLockedTx -- txid={}, retryChildren={}, \
             retryChildrenCount={}",
            txid,
            u8::from(retry_children),
            retry_children_count,
        );
    }

    /// `RetryUnminedAssetUnlocks`: queue every tracked unmined asset unlock for a retry.
    /// Core: `instantsend/instantsend.cpp:257-265`.
    pub fn retry_unmined_asset_unlocks(&self) {
        let nl = self.non_locked.lock();
        let mut retry = self.pending_retry.lock();
        for (txid, info) in &nl.txs {
            if info.mined.is_none() && info.tx.as_ref().is_some_and(|tx| tx.is_platform_transfer())
            {
                retry.insert(*txid);
            }
        }
    }

    /// `PrepareTxToRetry`: every queued transaction still tracked with its object. An asset
    /// unlock leaves the queue when handed out (once per trigger); the rest stay queued.
    /// Core: `instantsend/instantsend.cpp:267-292`.
    pub fn prepare_tx_to_retry(&self) -> Vec<Arc<Tx>> {
        let nl = self.non_locked.lock();
        let mut retry = self.pending_retry.lock();
        let mut txs = Vec::new();
        retry.retain(|txid| {
            let Some(tx) = nl.txs.get(txid).and_then(|info| info.tx.as_ref()) else {
                return true;
            };
            txs.push(Arc::clone(tx));
            !tx.is_platform_transfer()
        });
        txs
    }

    /// `TryEmplacePendingLock`: queue a lock unless the store knows it or it is already
    /// received; the queue cap is not checked here. Core: `instantsend/instantsend.cpp:294-302`.
    pub fn try_emplace_pending_lock(
        &self,
        hash: IsLockHash,
        id: NodeId,
        islock: Arc<LockedTx>,
    ) -> Result<()> {
        if self.store.known(&hash)? {
            return Ok(());
        }
        self.pending.lock().incoming.entry(hash).or_insert(PendingIsLockFromPeer {
            node_id: id,
            islock,
        });
        Ok(())
    }

    /// `HasTxForLock`: the lock is not parked waiting for its transaction.
    /// Core: `instantsend/instantsend.cpp:336-340`.
    pub fn has_tx_for_lock(&self, islock_hash: &IsLockHash) -> bool {
        !self.pending.lock().no_tx.contains_key(islock_hash)
    }

    /// `AlreadyHave(CInv{MSG_ISDLOCK, hash})`: true while IS is disabled, else queued in either
    /// pending map or known to the store. Core: `instantsend/instantsend.cpp:355-364`.
    pub fn already_have(&self, hash: &IsLockHash) -> Result<bool> {
        if !self.is_instant_send_enabled() {
            return Ok(true);
        }
        let queued = {
            let pending = self.pending.lock();
            pending.incoming.contains_key(hash) || pending.no_tx.contains_key(hash)
        };
        Ok(queued || self.store.known(hash)?)
    }

    /// `GetInstantSendLockByHash`: the store (through its cache), then the received queue, then
    /// the no-tx queue. Core: `instantsend/instantsend.cpp:366-389`.
    pub fn get_instant_send_lock_by_hash(&self, hash: &IsLockHash) -> Result<Option<LockedTx>> {
        if !self.is_instant_send_enabled() {
            return Ok(None);
        }
        if let Some(lock) = self.store.by_hash(hash, true)? {
            return Ok(Some(lock));
        }
        let pending = self.pending.lock();
        Ok(pending
            .incoming
            .get(hash)
            .or_else(|| pending.no_tx.get(hash))
            .map(|p| LockedTx::clone(&p.islock)))
    }

    /// `GetInstantSendLockByTxid`. Core: `instantsend/instantsend.cpp:391-398`.
    pub fn get_instant_send_lock_by_txid(&self, txid: &TxHash) -> Result<Option<LockedTx>> {
        if !self.is_instant_send_enabled() {
            return Ok(None);
        }
        self.store.by_txid(txid)
    }

    /// `IsLocked`: the txid's indexed hash is known (a cached, stored or archived lock).
    /// Core: `instantsend/instantsend.cpp:400-407`.
    pub fn is_locked(&self, txid: &TxHash) -> Result<bool> {
        if !self.is_instant_send_enabled() {
            return Ok(false);
        }
        let hash = self.store.hash_by_txid(txid)?;
        self.store.known(&hash)
    }

    /// `IsWaitingForTx`: a lock for `txid` is parked in the no-tx queue.
    /// Core: `instantsend/instantsend.cpp:409-426`.
    pub fn is_waiting_for_tx(&self, txid: &TxHash) -> bool {
        if !self.is_instant_send_enabled() {
            return false;
        }
        let pending = self.pending.lock();
        let Some((hash, _)) = pending.no_tx.iter().find(|(_, p)| p.islock.txid() == *txid) else {
            return false;
        };
        tracing::debug!(
            target: "instantsend",
            "CInstantSendManager::IsWaitingForTx -- txid={}, islock={}",
            txid,
            Hash256::from(*hash),
        );
        true
    }

    /// `GetInstantSendLockCount`. Core: `instantsend/instantsend.cpp:447-450`.
    pub fn instant_send_lock_count(&self) -> Result<usize> {
        self.store.count()
    }

    /// `GetCounts`. Core: `instantsend/instantsend.cpp:452-466`.
    pub fn counts(&self) -> Result<Counts> {
        let verified = self.store.count()?;
        let (unverified, awaiting_tx) = {
            let pending = self.pending.lock();
            (pending.incoming.len(), pending.no_tx.len())
        };
        let unprotected_tx = self.non_locked.lock().txs.len();
        Ok(Counts {
            verified,
            unverified,
            awaiting_tx,
            unprotected_tx,
        })
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
