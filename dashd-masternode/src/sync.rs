//! Core's `CMasternodeSync` (`masternode/sync.{h,cpp}`): the Dash sync assets every node goes
//! through, BLOCKCHAIN → GOVERNANCE → FINISHED (FR-EVO-11, US-NODE-02). The
//! [`SyncManager`](crate::sync_manager::SyncManager) tick advances it; the chain's
//! notifications postpone or reset it; `IsBlockchainSynced` and `IsSynced` gate the other
//! services.
//!
//! The fields are atomics, as in Core: the chain's notifications, the tick and the RPC read and
//! write them from their own threads without a common lock. Times are `GetTime()`: the node
//! clock in seconds, mockable.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};

use dash_clock::Clock;

/// `MASTERNODE_SYNC_BLOCKCHAIN`.
pub const MASTERNODE_SYNC_BLOCKCHAIN: i32 = 1;
/// `MASTERNODE_SYNC_GOVERNANCE`.
pub const MASTERNODE_SYNC_GOVERNANCE: i32 = 4;
/// `MASTERNODE_SYNC_GOVOBJ`: the governance objects' `ssc` item.
pub const MASTERNODE_SYNC_GOVOBJ: i32 = 10;
/// `MASTERNODE_SYNC_GOVOBJ_VOTE`: the votes' `ssc` item.
pub const MASTERNODE_SYNC_GOVOBJ_VOTE: i32 = 11;
/// `MASTERNODE_SYNC_FINISHED`.
pub const MASTERNODE_SYNC_FINISHED: i32 = 999;

/// `MASTERNODE_SYNC_TICK_SECONDS`.
pub const MASTERNODE_SYNC_TICK_SECONDS: i64 = 6;
/// `MASTERNODE_SYNC_TIMEOUT_SECONDS`.
pub const MASTERNODE_SYNC_TIMEOUT_SECONDS: i64 = 30;
/// `MASTERNODE_SYNC_RESET_SECONDS`: an unforced `Reset` within this long of the last
/// `UpdatedBlockTip` does nothing.
pub const MASTERNODE_SYNC_RESET_SECONDS: i64 = 900;

/// Core's `NodeSyncNotifier`: what a reset and the end of the sync do outside the state.
pub trait NodeSyncNotifier: Send + Sync {
    /// `SyncReset()`.
    fn sync_reset(&self);
    /// `SyncFinished()`: every peer gets `"full-sync"` (`NodeSyncNotifierImpl`).
    fn sync_finished(&self);
}

/// Core's `CMasternodeSync`.
pub struct MasternodeSync {
    clock: Arc<dyn Clock>,
    /// `nCurrentAsset`.
    current_asset: AtomicI32,
    /// `nTriedPeerCount`: the peers asked for the asset.
    tried_peer_count: AtomicI32,
    /// `nTimeAssetSyncStarted`.
    time_asset_sync_started: AtomicI64,
    /// `nTimeLastBumped`.
    time_last_bumped: AtomicI64,
    /// `fReachedBestHeader`: set in `UpdatedBlockTip` when the tip is the best header.
    reached_best_header: AtomicBool,
    /// `nTimeLastUpdateBlockTip`.
    time_last_update_block_tip: AtomicI64,
    notifier: Box<dyn NodeSyncNotifier>,
}

/// A block index entry as the notifications pass it: its height and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRef {
    pub height: u32,
    pub hash: [u8; 32],
}

impl MasternodeSync {
    /// The state at construction: BLOCKCHAIN, started and bumped now.
    // Core: masternode/sync.cpp:17-23
    pub fn new(clock: Arc<dyn Clock>, notifier: Box<dyn NodeSyncNotifier>) -> MasternodeSync {
        let now = clock.node_now().0;
        MasternodeSync {
            clock,
            current_asset: AtomicI32::new(MASTERNODE_SYNC_BLOCKCHAIN),
            tried_peer_count: AtomicI32::new(0),
            time_asset_sync_started: AtomicI64::new(now),
            time_last_bumped: AtomicI64::new(now),
            reached_best_header: AtomicBool::new(false),
            time_last_update_block_tip: AtomicI64::new(0),
            notifier,
        }
    }

    fn now(&self) -> i64 {
        self.clock.node_now().0
    }

    /// `IsBlockchainSynced()`: past BLOCKCHAIN.
    pub fn is_blockchain_synced(&self) -> bool {
        self.asset_id() > MASTERNODE_SYNC_BLOCKCHAIN
    }

    /// `IsSynced()`: FINISHED.
    pub fn is_synced(&self) -> bool {
        self.asset_id() == MASTERNODE_SYNC_FINISHED
    }

    /// `GetAssetID()`.
    pub fn asset_id(&self) -> i32 {
        self.current_asset.load(Ordering::SeqCst)
    }

    /// `GetAttempt()`.
    pub fn attempt(&self) -> i32 {
        self.tried_peer_count.load(Ordering::SeqCst)
    }

    /// `BumpAttempt()`.
    pub fn bump_attempt(&self) {
        self.tried_peer_count.fetch_add(1, Ordering::SeqCst);
    }

    /// `GetLastBump()`.
    pub fn last_bump(&self) -> i64 {
        self.time_last_bumped.load(Ordering::SeqCst)
    }

    /// `GetAssetStartTime()`.
    pub fn asset_start_time(&self) -> i64 {
        self.time_asset_sync_started.load(Ordering::SeqCst)
    }

    /// `IsReachedBestHeader()`.
    pub fn is_reached_best_header(&self) -> bool {
        self.reached_best_header.load(Ordering::SeqCst)
    }

    /// `BumpAssetLastTime(strFuncName)`: postpones the asset's timeout, until FINISHED.
    // Core: masternode/sync.cpp:46-51
    pub fn bump_asset_last_time(&self, func: &str) {
        if self.is_synced() {
            return;
        }
        self.time_last_bumped.store(self.now(), Ordering::SeqCst);
        tracing::debug!(target: "mnsync", "CMasternodeSync::BumpAssetLastTime -- {func}");
    }

    /// `GetAssetName()`.
    // Core: masternode/sync.cpp:53-62
    pub fn asset_name(&self) -> &'static str {
        match self.asset_id() {
            MASTERNODE_SYNC_BLOCKCHAIN => "MASTERNODE_SYNC_BLOCKCHAIN",
            MASTERNODE_SYNC_GOVERNANCE => "MASTERNODE_SYNC_GOVERNANCE",
            MASTERNODE_SYNC_FINISHED => "MASTERNODE_SYNC_FINISHED",
            _ => "UNKNOWN",
        }
    }

    /// `GetSyncStatus()`, untranslated.
    // Core: masternode/sync.cpp:86-94
    pub fn sync_status(&self) -> &'static str {
        match self.asset_id() {
            MASTERNODE_SYNC_BLOCKCHAIN => "Synchronizing blockchain…",
            MASTERNODE_SYNC_GOVERNANCE => "Synchronizing governance objects…",
            MASTERNODE_SYNC_FINISHED => "Synchronization finished",
            _ => "",
        }
    }

    /// `Reset(fForce, fNotifyReset)`: back to BLOCKCHAIN; unforced, only when no
    /// `UpdatedBlockTip` came for [`MASTERNODE_SYNC_RESET_SECONDS`]. The `mnsync reset` RPC and
    /// the tick force it.
    // Core: masternode/sync.cpp:27-44
    pub fn reset(&self, force: bool, notify: bool) {
        let now = self.now();
        if !force
            && now - self.time_last_update_block_tip.load(Ordering::SeqCst)
                < MASTERNODE_SYNC_RESET_SECONDS
        {
            return;
        }
        self.current_asset.store(MASTERNODE_SYNC_BLOCKCHAIN, Ordering::SeqCst);
        self.tried_peer_count.store(0, Ordering::SeqCst);
        self.time_asset_sync_started.store(now, Ordering::SeqCst);
        self.time_last_bumped.store(now, Ordering::SeqCst);
        self.time_last_update_block_tip.store(0, Ordering::SeqCst);
        self.reached_best_header.store(false, Ordering::SeqCst);
        if notify {
            self.notifier.sync_reset();
        }
    }

    /// `SwitchToNextAsset()` (also the `mnsync next` RPC): BLOCKCHAIN → GOVERNANCE → FINISHED,
    /// whose notifier marks every peer `"full-sync"`. FINISHED stays FINISHED. The attempts and
    /// the start time reset either way.
    // Core: masternode/sync.cpp:64-84
    pub fn switch_to_next_asset(&self) {
        let elapsed = self.now() - self.asset_start_time();
        match self.asset_id() {
            MASTERNODE_SYNC_BLOCKCHAIN => {
                tracing::info!(
                    "CMasternodeSync::SwitchToNextAsset -- Completed {} in {elapsed}s",
                    self.asset_name()
                );
                self.current_asset.store(MASTERNODE_SYNC_GOVERNANCE, Ordering::SeqCst);
                tracing::info!(
                    "CMasternodeSync::SwitchToNextAsset -- Starting {}",
                    self.asset_name()
                );
            }
            MASTERNODE_SYNC_GOVERNANCE => {
                tracing::info!(
                    "CMasternodeSync::SwitchToNextAsset -- Completed {} in {elapsed}s",
                    self.asset_name()
                );
                self.current_asset.store(MASTERNODE_SYNC_FINISHED, Ordering::SeqCst);
                self.notifier.sync_finished();
                tracing::info!("CMasternodeSync::SwitchToNextAsset -- Sync has finished");
            }
            _ => {}
        }
        self.tried_peer_count.store(0, Ordering::SeqCst);
        self.time_asset_sync_started.store(self.now(), Ordering::SeqCst);
        self.bump_asset_last_time("CMasternodeSync::SwitchToNextAsset");
    }

    /// `AcceptedBlockHeader(pindexNew)`: a new header postpones BLOCKCHAIN's timeout.
    // Core: masternode/sync.cpp:96-104
    pub fn accepted_block_header(&self, new: BlockRef) {
        tracing::debug!(
            target: "mnsync",
            "CMasternodeSync::AcceptedBlockHeader -- pindexNew->nHeight: {}",
            new.height
        );
        if !self.is_blockchain_synced() {
            self.bump_asset_last_time("CMasternodeSync::AcceptedBlockHeader");
        }
    }

    /// `NotifyHeaderTip(pindexNew, fInitialDownload)`: so does a new best header.
    // Core: masternode/sync.cpp:106-119
    pub fn notify_header_tip(&self, new: Option<BlockRef>, initial_download: bool) {
        let Some(new) = new else {
            return;
        };
        tracing::debug!(
            target: "mnsync",
            "CMasternodeSync::NotifyHeaderTip -- pindexNew->nHeight: {} fInitialDownload={}",
            new.height,
            u8::from(initial_download)
        );
        if self.is_synced() {
            return;
        }
        if !self.is_blockchain_synced() {
            self.bump_asset_last_time("CMasternodeSync::NotifyHeaderTip");
        }
    }

    /// `UpdatedBlockTip(pindexTip, pindexNew, fInitialDownload)`, `pindexTip` being the best
    /// header: postpones BLOCKCHAIN, resets a sync that switched in IBD, and records whether the
    /// new tip is the best header (resetting when that falls back to false).
    // Core: masternode/sync.cpp:121-158
    pub fn updated_block_tip(
        &self,
        best_header: Option<BlockRef>,
        new: BlockRef,
        initial_download: bool,
    ) {
        tracing::debug!(
            target: "mnsync",
            "CMasternodeSync::UpdatedBlockTip -- pindexNew->nHeight: {} fInitialDownload={}",
            new.height,
            u8::from(initial_download)
        );
        self.time_last_update_block_tip.store(self.now(), Ordering::SeqCst);
        if self.is_synced() {
            return;
        }
        if !self.is_blockchain_synced() {
            self.bump_asset_last_time("CMasternodeSync::UpdatedBlockTip");
        }
        if initial_download {
            // Switched too early.
            if self.is_blockchain_synced() {
                self.reset(true, true);
            }
            return;
        }
        let Some(tip) = best_header else {
            return;
        };
        let reached_new = new.hash == tip.hash;
        if self.is_reached_best_header() && !reached_new {
            // Stuck syncing headers before: there is no way to update the tip without the best
            // header.
            self.reset(true, true);
        }
        self.reached_best_header.store(reached_new, Ordering::SeqCst);
        tracing::debug!(
            target: "mnsync",
            "CMasternodeSync::UpdatedBlockTip -- pindexNew->nHeight: {} pindexTip->nHeight: {} fInitialDownload={} fReachedBestHeader={}",
            new.height,
            tip.height,
            u8::from(initial_download),
            u8::from(reached_new)
        );
    }
}
