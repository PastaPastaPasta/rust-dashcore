//! NetInstantSend's chain notifications over the IS store (PLAN-LK1b §3, slice S1d): the tip
//! cache, the mined marks of connected and disconnected blocks, and the confirmation of locks by
//! a ChainLock or, without ChainLocks, by depth.
//!
//! [`IsListener`] is a [`ValidationInterface`]. Core's `CacheTipHeight` runs on
//! `SynchronousUpdatedBlockTip` (on V) and on the queued `BlockConnected` and
//! `BlockDisconnected`; the store's writes run on the signals thread, where a store call waits
//! while a chain attempt holds the IS gate (§5.3 "Overlay"). `BlockConnected`'s non-locked
//! transaction tracking and `RemoveConfirmedInstantSendLocks`' untracking run on the manager
//! (S4).
//!
//! A store failure goes to [`IsFaults`], once per failure, and the notification stops there, as
//! Core's thrown `dbwrapper_error` ends the callback.
//!
//! The mempool halves (S5): `TransactionAddedToMempool` attaches a parked lock or tracks the
//! transaction, and removes the attached lock's pool conflicts through [`IsListener::with_mempool`]
//! ([`super::mempool`]); `TransactionRemovedFromMempool` releases the transaction's tracking or
//! removes its lock. Both read the sync state BlockConnected reads.
//!
//! With [`IsListener::with_signer`], the listener also drives the IS signer (LK1c-2b). Its checks
//! run inline in Core's order, but `ProcessTx` may wait on V, so each call is posted to
//! [`IsSignerQueue`] instead of running on the signals thread as Core's does
//! (`net_instantsend.cpp:453-459`, `:544-569`). The same [`IsListenerSigner`] is the
//! [`IsSignerSeam`] the lock processor, the conflict half and the pending worker call, so every
//! signer call of NetInstantSend goes through one queue in call order.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::OutPoint;
use dash_consensus::{Block, BlockId, Tx, TxRef};
use dash_evo::locks::{isdlock_hash, islock_request_id};
use dash_kv::StoreError;
use dash_params::{ChainSetup, LlmqType};
use dashd_chainstate::chain::{ChainLockSig, Cont, RemovalReason, ValidationInterface};
use dashd_mempool::is::IsMempool;

use super::chain::GateSource;
use super::conflicts::remove_conflicting_lock;
use super::manager::{IsManager, MinedBlock};
use super::mempool::remove_mempool_conflicts_for_lock;
use super::process::truncate_recovered_sigs_for_inputs;
use super::seams::{ChainLockReads, IsSignerSeam, IsStore, RecoveredSigs, SignRequestId, SyncView};
use super::signer::IsSigner;
use super::{IsLockHash, IsTipCache, LockedTx, core_height};

/// What the listener reads of V's block tree: Core's `CBlockIndex` fields of a block it is told
/// about.
pub trait BlockTree: Send + Sync {
    /// `pindex->nHeight`, or `None` for a block the tree does not hold.
    fn height(&self, id: BlockId) -> Option<u32>;
    /// `pindex->GetAncestor(height)`.
    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId>;
}

/// The node's one-report path for a listener store failure (§5.3 item 6, "Faults").
pub trait IsFaults: Send + Sync {
    /// `error` ended the notification `site`. Called once per failed store call.
    fn failed(&self, site: &'static str, error: &StoreError);
}

/// Core's `Chainlocks::IsEnabled()`, read at each notification.
pub type ChainLocksEnabled = Arc<dyn Fn() -> bool + Send + Sync>;

/// What [`IsListener`] is built from.
pub struct IsListenerParts {
    pub store: Arc<dyn IsStore>,
    pub tip: IsTipCache,
    pub gate: GateSource,
    pub tree: Arc<dyn BlockTree>,
    pub chainlocks_enabled: ChainLocksEnabled,
    pub recsigs: Arc<dyn RecoveredSigs>,
    pub faults: Arc<dyn IsFaults>,
    pub manager: Arc<IsManager>,
    pub sync: Arc<dyn SyncView>,
    pub chainlocks: Arc<dyn ChainLockReads>,
}

/// Runs posted jobs in FIFO order on a thread without the no-wait mark (W1's). A job never calls
/// a wait-until-synced API, and may post again.
///
/// Shutdown: after stop, `post` drops the job. The node stops the queue and joins its thread
/// before it stops the IS store and the tx source, so no queued job reads either after it stops.
pub trait IsSignerQueue: Send + Sync {
    /// Appends `job` to an unbounded FIFO. Never blocks: the signals thread posts, and a blocking
    /// post can deadlock through the block feed's back-pressure.
    fn post(&self, job: Box<dyn FnOnce() + Send>);
}

/// NetInstantSend's `m_signer`, with the queue its `ProcessTx` calls run on. The signer's
/// `IsReads` gives the hooks `IsBlockchainSynced` and `HasChainLock`.
pub struct IsListenerSigner {
    pub signer: Arc<IsSigner>,
    pub queue: Arc<dyn IsSignerQueue>,
}

impl IsListenerSigner {
    fn post_process_tx(&self, tx: &TxRef, retroactive: bool) {
        let signer = Arc::clone(&self.signer);
        let tx = Arc::clone(tx);
        self.queue.post(Box::new(move || signer.process_tx(&tx, retroactive)));
    }
}

/// `m_signer` as NetInstantSend's other callers reach it: each call is one job on the same queue
/// as the hooks' `ProcessTx`, so the signer sees Core's call order.
impl IsSignerSeam for IsListenerSigner {
    fn process_txs(&self, txs: Vec<Arc<Tx>>, retroactive: bool) {
        for tx in &txs {
            self.post_process_tx(tx, retroactive);
        }
    }

    fn retry(&self, txs: Vec<Arc<Tx>>) {
        let signer = Arc::clone(&self.signer);
        self.queue.post(Box::new(move || signer.process_pending_retry(&txs)));
    }

    fn clear_inputs(&self, ids: Vec<[u8; 32]>) {
        let signer = Arc::clone(&self.signer);
        self.queue.post(Box::new(move || signer.clear_inputs(&ids)));
    }

    fn clear_lock(&self, lock: Arc<LockedTx>) {
        let signer = Arc::clone(&self.signer);
        self.queue.post(Box::new(move || signer.clear_lock(&lock)));
    }
}

/// NetInstantSend's `CValidationInterface` half (LK1b-s): see the module doc.
pub struct IsListener {
    parts: IsListenerParts,
    signer: Option<Arc<IsListenerSigner>>,
    /// `m_signer` as `BlockConnected` and `TruncateRecoveredSigsForInputs` call it: `signer`
    /// itself once set.
    seam: Option<Arc<dyn IsSignerSeam>>,
    /// `m_mempool`'s InstantSend surface: none until the node passes the pool, which is Core
    /// with an empty mempool.
    mempool: Option<Arc<dyn IsMempool>>,
    /// `DIP0008Height`.
    dip0008_height: i64,
    /// `nInstantSendKeepLock`.
    keep_lock: i64,
    /// `llmqTypeDIP0024InstantSend`, the recovered signatures' type.
    is_llmq: LlmqType,
}

/// The store sites a failure names.
const CONNECTED: &str = "InstantSend BlockConnected";
const DISCONNECTED: &str = "InstantSend BlockDisconnected";
const CONFIRMED: &str = "InstantSend HandleFullyConfirmedBlock";
const REMOVED: &str = "InstantSend TransactionRemovedFromMempool";

impl IsListener {
    /// The listener for the chain `params`.
    pub fn new(params: &ChainSetup, parts: IsListenerParts) -> Self {
        let consensus = &params.consensus;
        IsListener {
            parts,
            signer: None,
            seam: None,
            mempool: None,
            dip0008_height: i64::from(consensus.dip0008_height.to_u32()),
            keep_lock: i64::from(consensus.instant_send_keep_lock),
            is_llmq: consensus.llmq_type_dip0024_instant_send,
        }
    }

    /// The listener with NetInstantSend's `m_signer` set.
    pub fn with_signer(self, signer: IsListenerSigner) -> Self {
        let signer = Arc::new(signer);
        IsListener {
            seam: Some(Arc::clone(&signer) as _),
            signer: Some(signer),
            ..self
        }
    }

    /// The listener with `m_mempool`'s InstantSend surface.
    pub fn with_mempool(self, mempool: Arc<dyn IsMempool>) -> Self {
        IsListener {
            mempool: Some(mempool),
            ..self
        }
    }

    /// The listener calling `seam` for `m_signer`; the mempool hook's `ProcessTx` needs
    /// [`Self::with_signer`] (tests).
    #[cfg(test)]
    pub(super) fn with_seam(self, seam: Arc<dyn IsSignerSeam>) -> Self {
        IsListener {
            seam: Some(seam),
            ..self
        }
    }

    fn enabled(&self) -> bool {
        (self.parts.gate)().enabled()
    }

    fn height(&self, id: BlockId) -> Option<i32> {
        self.parts.tree.height(id).map(core_height)
    }

    /// `BlockConnected` while the blockchain is synced, per transaction with lock inputs: unless
    /// it is locked or the block ChainLocked, `ProcessTx(tx, true)` is posted (with a `vin`) and
    /// it is tracked as mined in the block; otherwise it is untracked. Then unmined asset unlocks
    /// are retried.
    // Core: instantsend/net_instantsend.cpp:544-568
    fn track(&self, block: &Block, mined: MinedBlock) -> dash_kv::Result<()> {
        let manager = &self.parts.manager;
        let has_chainlock = self.parts.chainlocks.has_chain_lock(mined.core_height(), &mined.hash);
        for tx in block.txs() {
            if !dash_consensus::connect::has_lock_inputs(tx) {
                continue;
            }
            if !manager.is_locked(&tx.txid())? && !has_chainlock {
                if let Some(seam) = &self.seam
                    && !tx.inputs().is_empty()
                {
                    seam.process_txs(vec![Arc::clone(tx)], true);
                }
                manager.add_non_locked_tx(tx, Some(mined));
            } else {
                manager.remove_non_locked_tx(&tx.txid(), true, false);
            }
        }
        manager.retry_unmined_asset_unlocks();
        Ok(())
    }

    /// Core's `CacheTipHeight(tip)`: a null tip caches -1, which [`IsTipCache::get`] reads as 0.
    // Core: instantsend/instantsend.cpp:490-499
    fn cache_tip(&self, tip: Option<BlockId>) {
        let height = tip.and_then(|id| self.height(id)).unwrap_or(-1);
        self.parts.tip.set(height);
    }

    fn fail(&self, site: &'static str, error: &StoreError) {
        self.parts.faults.failed(site, error);
    }

    /// The block's transactions with lock inputs: the only ones the store's mined-mark batch
    /// looks up (`db.cpp:206`, `:224`).
    fn locking(block: &Block) -> Vec<Tx> {
        block
            .txs()
            .iter()
            .filter(|tx| dash_consensus::connect::has_lock_inputs(tx))
            .map(|tx| Tx::new(tx.raw().clone()))
            .collect()
    }

    /// `HandleFullyConfirmedBlock(pindex)` for `id` at `height`: the confirmed locks removed
    /// (their cache entries kept), the archive pruned 100 blocks below, the non-locked
    /// transactions mined in `id`'s chain untracked, then each removed lock's recovered
    /// signatures truncated. Gated on `IsInstantSendEnabled`.
    // Core: instantsend/net_instantsend.cpp:656-675; instantsend/instantsend.cpp:518-544
    fn fully_confirmed(&self, id: BlockId, height: i32) {
        if !self.enabled() {
            return;
        }
        let removed: BTreeMap<IsLockHash, LockedTx> =
            match self.parts.store.remove_confirmed(height) {
                Ok(removed) => removed,
                Err(e) => return self.fail(CONFIRMED, &e),
            };
        if let Err(e) = self.parts.store.remove_archived(height - 100) {
            return self.fail(CONFIRMED, &e);
        }
        let tree = &self.parts.tree;
        self.parts
            .manager
            .remove_confirmed_non_locked(|mined| tree.ancestor(id, mined.height) == Some(mined.id));
        for (hash, lock) in removed {
            tracing::debug!(
                target: "instantsend",
                "NetInstantSend::HandleFullyConfirmedBlock -- txid={}, islock={}: removed islock \
                 as it got fully confirmed",
                lock.txid(),
                dash_num::Hash256::from(hash),
            );
            let truncated = self
                .parts
                .recsigs
                .truncate(self.is_llmq, &islock_request_id(lock.inputs()))
                .and_then(|()| {
                    truncate_recovered_sigs_for_inputs(
                        self.seam.as_deref(),
                        &*self.parts.recsigs,
                        self.is_llmq,
                        lock.inputs(),
                    )
                });
            if let Err(e) = truncated {
                return self.fail(CONFIRMED, &e);
            }
        }
    }
}

/// Core's `GenInputLockRequestId(outpoint)`: `SerializeHash(pair("inlock", outpoint))`.
// Core: instantsend/lock.cpp:41-44
pub fn input_lock_request_id(input: &OutPoint) -> SignRequestId {
    let mut bytes = Vec::with_capacity(1 + 6 + 36);
    bytes.push(6);
    bytes.extend_from_slice(b"inlock");
    bytes.extend_from_slice(input.txid().as_bytes());
    bytes.extend_from_slice(&input.index().to_le_bytes());
    sha256d(&bytes)
}

impl ValidationInterface for IsListener {
    /// A transaction with lock inputs, while synced: a parked lock for it goes back to the
    /// received queue and its pool conflicts are removed; with none, `ProcessTx(tx, false)` is
    /// posted and the transaction tracked unmined.
    // Core: instantsend/net_instantsend.cpp:447-463
    fn transaction_added_to_mempool(&self, tx: &TxRef, _accept_time: i64, _sequence: u64) {
        let manager = &self.parts.manager;
        if !self.enabled()
            || !self.parts.sync.is_blockchain_synced()
            || !dash_consensus::connect::has_lock_inputs(tx)
        {
            return;
        }
        match manager.attach_is_lock_to_tx(tx) {
            None => {
                if let Some(hooks) = &self.signer {
                    hooks.post_process_tx(tx, false);
                }
                manager.add_non_locked_tx(tx, None);
            }
            Some(islock) => {
                if let Some(mempool) = &self.mempool {
                    let hash = isdlock_hash(&islock);
                    let seam = self.seam.as_ref();
                    remove_mempool_conflicts_for_lock(&**mempool, manager, seam, &hash, &islock);
                }
            }
        }
    }

    /// `TransactionIsRemoved`: a locked transaction's lock is removed with its chained locks; an
    /// unlocked one stops being tracked. A store failure goes to [`IsFaults`] once.
    // Core: instantsend/net_instantsend.cpp:530-534; instantsend/instantsend.cpp:131-148
    fn transaction_removed_from_mempool(&self, tx: &TxRef, _reason: RemovalReason, _seq: u64) {
        let p = &self.parts;
        let removed = p.manager.transaction_is_removed(tx).and_then(|lock| match lock {
            Some((hash, islock)) => remove_conflicting_lock(&*p.store, &p.tip, &hash, &islock),
            None => Ok(()),
        });
        if let Err(e) = removed {
            self.fail(REMOVED, &e);
        }
    }

    // Core: instantsend/net_instantsend.cpp:507-511
    fn synchronous_updated_block_tip(&self, new: BlockId, _fork: Option<BlockId>, _ibd: bool) {
        self.cache_tip(Some(new));
    }

    /// The tip cache seeded after the import's `InitializeCurrentBlockTip`, where Core's
    /// `loadblk` calls `CacheTipHeight` on the active tip.
    // Core: init.cpp:2470-2473
    fn initialize_current_block_tip(&self, tip: Option<BlockId>, _ibd: bool) {
        self.cache_tip(tip);
    }

    /// Without ChainLocks (SPORK_19 off, or DIP0008 not active at the new tip's parent), the
    /// block `nInstantSendKeepLock` below the new tip confirms its locks.
    // Core: instantsend/net_instantsend.cpp:513-528
    fn updated_block_tip(&self, new: BlockId, _fork: Option<BlockId>, _ibd: bool) {
        let Some(height) = self.parts.tree.height(new) else {
            return;
        };
        let dip0008_active =
            height.checked_sub(1).is_some_and(|parent| i64::from(parent) >= self.dip0008_height);
        if (self.parts.chainlocks_enabled)() && dip0008_active {
            return;
        }
        let confirmed = i64::from(height) - self.keep_lock;
        let Ok(confirmed) = u32::try_from(confirmed) else {
            return;
        };
        if let Some(ancestor) = self.parts.tree.ancestor(new, confirmed) {
            self.fully_confirmed(ancestor, core_height(confirmed));
        }
    }

    // Core: instantsend/net_instantsend.cpp:536-570
    fn block_connected(&self, block: &Arc<Block>, id: BlockId) -> Cont {
        if !self.enabled() {
            return Cont::Done;
        }
        self.cache_tip(Some(id));
        let Some(height) = self.parts.tree.height(id) else {
            return Cont::Done;
        };
        if self.parts.sync.is_blockchain_synced() {
            let mined = MinedBlock {
                id,
                height,
                hash: block.hash(),
            };
            if let Err(e) = self.track(block, mined) {
                self.fail(CONNECTED, &e);
                return Cont::Done;
            }
        }
        let height = core_height(height);
        if let Err(e) = self.parts.store.block_connected(&Self::locking(block), height) {
            self.fail(CONNECTED, &e);
        }
        Cont::Done
    }

    // Core: instantsend/net_instantsend.cpp:572-577
    fn block_disconnected(&self, block: &Arc<Block>, id: BlockId) {
        let Some(height) = self.height(id) else {
            return;
        };
        // `CacheTipHeight(pindexDisconnected->pprev)`; Core's block-height cache is not ported
        // (`verify.rs`), so `CacheDisconnectBlock` has nothing to erase.
        self.parts.tip.set(if height > 0 {
            height - 1
        } else {
            -1
        });
        if let Err(e) = self.parts.store.block_disconnected(&Self::locking(block), height) {
            self.fail(DISCONNECTED, &e);
        }
    }

    // Core: instantsend/net_instantsend.cpp:579-582
    fn notify_chain_lock(&self, id: BlockId, _sig: &Arc<ChainLockSig>) {
        if let Some(height) = self.height(id) {
            self.fully_confirmed(id, height);
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "listener_tests.rs"]
mod tests;
