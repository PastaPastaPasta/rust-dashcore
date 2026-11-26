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
//! Core's maps are salted unordered maps. The received queue iterates in a salted SipHash order
//! ([`FetchSalt`]), so no peer can choose which locks one `FetchPendingLocks` takes; the others
//! are ordered by key, and the iteration order of `AttachISLockToTx` and `PrepareTxToRetry` is
//! then one of Core's possible schedules (§5 item 4).
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
///
/// `legacy` is Core's `CBLSLazyWrapper::bufLegacyScheme` for the lock's signature: true when the
/// lock was constructed and read under the legacy scheme (`bls/bls.h:411-414`, `:481-484`,
/// `:496-503`), which travels with the lock object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingIsLockFromPeer {
    pub node_id: NodeId,
    pub islock: Arc<LockedTx>,
    pub legacy: bool,
}

/// `instantsend::PendingISLockEntry`, with the lock's `legacy` flag as in
/// [`PendingIsLockFromPeer`]. Core: `instantsend/instantsend.h:39-41`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingIsLockEntry {
    pub node_id: NodeId,
    pub islock: Arc<LockedTx>,
    pub islock_hash: IsLockHash,
    pub legacy: bool,
}

/// `instantsend::PendingState`. Core: `instantsend/instantsend.h:43-46`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PendingState {
    /// More locks were left in the queue.
    pub pending_work: bool,
    pub pending_is: Vec<PendingIsLockEntry>,
}

/// The block a non-locked transaction was mined in: Core's `const CBlockIndex*`, whose identity
/// is `id` and whose `nHeight` and `GetBlockHash()` S4 reads. `height` is V's; Core's `int` is
/// [`MinedBlock::core_height`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MinedBlock {
    pub id: BlockId,
    pub height: u32,
    pub hash: BlockHash,
}

impl MinedBlock {
    /// `pindexMined->nHeight` as Core's `int`, through [`super::core_height`].
    pub fn core_height(&self) -> i32 {
        super::core_height(self.height)
    }
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

/// `StaticSaltedHasher`'s salt `(k0, k1)`, drawn per process in Core and per manager here.
/// Core: `saltedhasher.cpp:8-10`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FetchSalt(pub u64, pub u64);

impl FetchSalt {
    /// A salt from the OS RNG, as Core's `GetRand<uint64_t>()` pair.
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("the OS RNG is available");
        let (k0, k1) = bytes.split_at(8);
        FetchSalt(
            u64::from_le_bytes(k0.try_into().expect("8 bytes")),
            u64::from_le_bytes(k1.try_into().expect("8 bytes")),
        )
    }

    /// `SipHashUint256(k0, k1, hash)`. Core: `saltedhasher.h:47-52`.
    fn order(self, hash: &IsLockHash) -> u64 {
        bitcoin_hashes::siphash24::Hash::hash_with_keys(self.0, self.1, hash).to_u64()
    }
}

/// `pendingInstantSendLocks`, a `Uint256HashMap`: iterated in salted SipHash order, not by hash.
/// Core: `instantsend/instantsend.h:66`, `saltedhasher.h:88-89`.
#[derive(Debug)]
struct SaltedLocks {
    salt: FetchSalt,
    map: BTreeMap<(u64, IsLockHash), PendingIsLockFromPeer>,
}

impl SaltedLocks {
    fn key(&self, hash: &IsLockHash) -> (u64, IsLockHash) {
        (self.salt.order(hash), *hash)
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn contains_key(&self, hash: &IsLockHash) -> bool {
        self.map.contains_key(&self.key(hash))
    }

    fn get(&self, hash: &IsLockHash) -> Option<&PendingIsLockFromPeer> {
        self.map.get(&self.key(hash))
    }

    /// `emplace`: an existing entry is kept.
    fn emplace(&mut self, hash: IsLockHash, entry: PendingIsLockFromPeer) {
        self.map.entry(self.key(&hash)).or_insert(entry);
    }

    fn pop_first(&mut self) -> Option<(IsLockHash, PendingIsLockFromPeer)> {
        self.map.pop_first().map(|((_, hash), entry)| (hash, entry))
    }

    #[cfg(test)]
    fn hashes(&self) -> impl Iterator<Item = IsLockHash> + '_ {
        self.map.keys().map(|(_, hash)| *hash)
    }
}

/// `cs_pendingLocks`' maps. Core: `instantsend/instantsend.h:65-69`.
#[derive(Debug)]
struct PendingLocks {
    /// `pendingInstantSendLocks`: received, not verified yet.
    incoming: SaltedLocks,
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
    /// The manager over `store`, its received queue salted from the OS RNG.
    pub fn new(store: Arc<dyn IsManagerStore>, gate: GateSource, clock: Arc<dyn Clock>) -> Self {
        Self::with_salt(store, gate, clock, FetchSalt::random())
    }

    /// The manager with a given received-queue salt (tests).
    pub fn with_salt(
        store: Arc<dyn IsManagerStore>,
        gate: GateSource,
        clock: Arc<dyn Clock>,
        salt: FetchSalt,
    ) -> Self {
        IsManager {
            store,
            gate,
            clock,
            pending: Mutex::new(PendingLocks {
                incoming: SaltedLocks {
                    salt,
                    map: BTreeMap::new(),
                },
                no_tx: BTreeMap::new(),
            }),
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
    pub fn enqueue_instant_send_lock(
        &self,
        from: NodeId,
        hash: IsLockHash,
        islock: Arc<LockedTx>,
        legacy: bool,
    ) {
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
        pending.incoming.emplace(
            hash,
            PendingIsLockFromPeer {
                node_id: from,
                islock,
                legacy,
            },
        );
    }

    /// `FetchPendingLocks`: take at most 32 received locks, in the queue's salted order;
    /// `pending_work` when more remain. Core: `instantsend/instantsend.cpp:58-85`.
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
                legacy: entry.legacy,
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

    /// `WriteNewISLock`: store the lock, mined at `mined_height` when given.
    /// Core: `instantsend/instantsend.cpp:107-114`.
    pub fn write_new_is_lock(
        &self,
        hash: &IsLockHash,
        islock: &LockedTx,
        mined_height: Option<i32>,
    ) -> Result<()> {
        self.store.write_new(hash, islock)?;
        if let Some(height) = mined_height {
            self.store.write_mined(hash, height)?;
        }
        Ok(())
    }

    /// `AddPendingISLock`: park a verified lock whose transaction is missing, dropped while the
    /// no-tx queue holds 1024. Core: `instantsend/instantsend.cpp:116-129`.
    pub fn add_pending_is_lock(
        &self,
        hash: IsLockHash,
        islock: Arc<LockedTx>,
        from: NodeId,
        legacy: bool,
    ) {
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
            legacy,
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
        pending.incoming.emplace(hash, entry);
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

    /// `RemoveConfirmedInstantSendLocks`' untracking: every tracked transaction whose block
    /// `confirmed` holds (the confirmed block's ancestor at its height) leaves the graph, its
    /// children queued for a retry. Core: `instantsend/instantsend.cpp:525-541`.
    pub fn remove_confirmed_non_locked(&self, confirmed: impl Fn(&MinedBlock) -> bool) {
        let to_remove: Vec<TxHash> = self
            .non_locked
            .lock()
            .txs
            .iter()
            .filter(|(_, info)| info.mined.as_ref().is_some_and(&confirmed))
            .map(|(txid, _)| *txid)
            .collect();
        for txid in to_remove {
            self.remove_non_locked_tx(&txid, true, false);
        }
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
        legacy: bool,
    ) -> Result<()> {
        if self.store.known(&hash)? {
            return Ok(());
        }
        self.pending.lock().incoming.emplace(
            hash,
            PendingIsLockFromPeer {
                node_id: id,
                islock,
                legacy,
            },
        );
        Ok(())
    }

    /// `RetrieveISConflicts(islockHash, islock)`: for each lock input, the tracked transaction
    /// that spends it under another txid, if mined and held with its object, grouped by block in
    /// first-seen order (Core's map order is unspecified), each transaction once.
    /// Core: `instantsend/instantsend.cpp:304-334`.
    pub fn retrieve_is_conflicts(
        &self,
        hash: &IsLockHash,
        islock: &LockedTx,
    ) -> Vec<(MinedBlock, Vec<Arc<Tx>>)> {
        let nl = self.non_locked.lock();
        let mut conflicts: Vec<(MinedBlock, Vec<Arc<Tx>>)> = Vec::new();
        for input in islock.inputs() {
            let Some(&conflict) = nl.by_outpoint.get(input) else {
                continue;
            };
            if conflict == islock.txid() {
                continue;
            }
            let Some(NonLockedTxInfo {
                mined: Some(mined),
                tx: Some(tx),
                ..
            }) = nl.txs.get(&conflict)
            else {
                continue;
            };
            tracing::info!(
                "CInstantSendManager::RetrieveISConflicts -- txid={}, islock={}: mined TX {} with \
                 input {} and mined in block {} conflicts with islock",
                islock.txid(),
                Hash256::from(*hash),
                conflict,
                short(input),
                mined.hash,
            );
            match conflicts.iter_mut().find(|(block, _)| block.id == mined.id) {
                Some((_, txs)) if txs.iter().any(|t| t.txid() == conflict) => {}
                Some((_, txs)) => txs.push(Arc::clone(tx)),
                None => conflicts.push((*mined, vec![Arc::clone(tx)])),
            }
        }
        conflicts
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

    /// `GetConflictingLock`: the first of `tx`'s lock inputs locked for another txid, through the
    /// store's caches. Core: `instantsend/instantsend.cpp:428-445`.
    pub fn get_conflicting_lock(&self, tx: &Tx) -> Result<Option<(IsLockHash, LockedTx)>> {
        if !self.is_instant_send_enabled() {
            return Ok(None);
        }
        self.store.conflicting_lock(tx)
    }

    /// `TransactionIsRemoved`, up to `RemoveConflictingLock`: an unlocked transaction stops
    /// being tracked (a mined one stays unless it is a platform transfer); a locked one's lock is
    /// returned for the caller to remove. Core: `instantsend/instantsend.cpp:131-148`.
    pub fn transaction_is_removed(&self, tx: &Tx) -> Result<Option<(IsLockHash, LockedTx)>> {
        if !dash_consensus::connect::has_lock_inputs(tx) {
            return Ok(None);
        }
        let txid = tx.txid();
        self.timings.lock().remove(&txid);
        let Some(islock) = self.get_instant_send_lock_by_txid(&txid)? else {
            self.remove_non_locked_tx(&txid, false, !tx.is_platform_transfer());
            return Ok(None);
        };
        tracing::debug!(
            target: "instantsend",
            "CInstantSendManager::TransactionIsRemoved -- transaction {} was removed from mempool",
            txid,
        );
        Ok(Some((isdlock_hash(&islock), islock)))
    }

    /// Whether any manager mutex is held (lock-order tests).
    #[cfg(test)]
    pub(super) fn any_lock_held(&self) -> bool {
        self.pending.is_locked()
            || self.non_locked.is_locked()
            || self.pending_retry.is_locked()
            || self.timings.is_locked()
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
