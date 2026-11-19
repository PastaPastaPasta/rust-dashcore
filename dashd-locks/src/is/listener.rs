//! NetInstantSend's chain notifications over the IS store (PLAN-LK1b §3, slice S1d): the tip
//! cache, the mined marks of connected and disconnected blocks, and the confirmation of locks by
//! a ChainLock or, without ChainLocks, by depth.
//!
//! [`IsListener`] is a [`ValidationInterface`]. Core's `CacheTipHeight` runs on
//! `SynchronousUpdatedBlockTip` (on V) and on the queued `BlockConnected` and
//! `BlockDisconnected`; the store's writes run on the signals thread, where a store call waits
//! while a chain attempt holds the IS gate (§5.3 "Overlay"). The non-locked transaction tracking
//! of `BlockConnected` and `RemoveConfirmedInstantSendLocks` is S2's and not ported.
//!
//! A store failure goes to [`IsFaults`], once per failure, and the notification stops there, as
//! Core's thrown `dbwrapper_error` ends the callback.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::OutPoint;
use dash_consensus::{Block, BlockId, Tx};
use dash_evo::locks::islock_request_id;
use dash_kv::StoreError;
use dash_params::{ChainSetup, LlmqType};
use dashd_chainstate::chain::{ChainLockSig, ValidationInterface};

use super::chain::GateSource;
use super::seams::{IsStore, RecoveredSigs, SignRequestId};
use super::{IsLockHash, IsTipCache, LockedTx};

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
}

/// NetInstantSend's `CValidationInterface` half (LK1b-s): see the module doc.
pub struct IsListener {
    parts: IsListenerParts,
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

impl IsListener {
    /// The listener for the chain `params`.
    pub fn new(params: &ChainSetup, parts: IsListenerParts) -> Self {
        let consensus = &params.consensus;
        IsListener {
            parts,
            dip0008_height: i64::from(consensus.dip0008_height.to_u32()),
            keep_lock: i64::from(consensus.instant_send_keep_lock),
            is_llmq: consensus.llmq_type_dip0024_instant_send,
        }
    }

    fn enabled(&self) -> bool {
        (self.parts.gate)().enabled()
    }

    fn height(&self, id: BlockId) -> Option<i32> {
        self.parts.tree.height(id).map(core_height)
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

    /// `HandleFullyConfirmedBlock(pindex)` at `height`: the confirmed locks removed (their cache
    /// entries kept), the archive pruned 100 blocks below, then each removed lock's recovered
    /// signatures truncated. Gated on `IsInstantSendEnabled`.
    // Core: instantsend/net_instantsend.cpp:656-675; instantsend/instantsend.cpp:518-544
    fn fully_confirmed(&self, height: i32) {
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
        // S2: `RemoveNonLockedTx` for every non-locked transaction this block confirms.
        for (hash, lock) in removed {
            tracing::debug!(
                target: "instantsend",
                "NetInstantSend::HandleFullyConfirmedBlock -- txid={}, islock={}: removed islock \
                 as it got fully confirmed",
                lock.txid(),
                dash_num::Hash256::from(hash),
            );
            let ids = std::iter::once(islock_request_id(lock.inputs()))
                .chain(lock.inputs().iter().map(input_lock_request_id));
            for id in ids {
                if let Err(e) = self.parts.recsigs.truncate(self.is_llmq, &id) {
                    return self.fail(CONFIRMED, &e);
                }
            }
        }
    }
}

/// Core's `int` height of a block.
fn core_height(height: u32) -> i32 {
    i32::try_from(height).expect("a block height fits Core's int")
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
        if self.parts.tree.ancestor(new, confirmed).is_some() {
            self.fully_confirmed(core_height(confirmed));
        }
    }

    // Core: instantsend/net_instantsend.cpp:536-570
    fn block_connected(&self, block: &Arc<Block>, id: BlockId) {
        if !self.enabled() {
            return;
        }
        self.cache_tip(Some(id));
        let Some(height) = self.height(id) else {
            return;
        };
        // S2: the non-locked transaction tracking while the blockchain is synced (:545-568).
        if let Err(e) = self.parts.store.block_connected(&Self::locking(block), height) {
            self.fail(CONNECTED, &e);
        }
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
            self.fully_confirmed(height);
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "listener_tests.rs"]
mod tests;
