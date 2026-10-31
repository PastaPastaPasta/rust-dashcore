//! SP1 in the node: Core's `CDSNotificationInterface` hooks into `CMasternodeSync` on V's
//! signals, and the parts of the sync tick that step 7c and step 10a build and schedule. The
//! managers with files (`netfulfilledman`, `mn_metaman`) are [`crate::steps::ChainSteps`]', as
//! `sporkman` is.

use std::sync::Arc;
use std::time::Duration;

use dash_args::ArgsManager;
use dash_chain::EntryView;
use dash_clock::Clock;
use dash_consensus::BlockId;
use dash_p2p::NetCore;
use dash_params::{ChainParams, NetworkKind};
use dashd_chainstate::chain::{ChainViewHandle, ValidationInterface};
use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
use dashd_masternode::sync::{BlockRef, MasternodeSync};
use dashd_masternode::sync_manager::{EmptyGovernance, FullSyncNotifier, SyncConfig, SyncManager};
use dashd_masternode::sync_net::CoreSyncNet;

use crate::InitFailure;
use crate::scheduler::Scheduler;
use crate::shutdown::ShutdownState;

/// Core's `DEFAULT_SYNC_MEMPOOL`.
// Core: masternode/sync.h:13-14
pub const DEFAULT_SYNC_MEMPOOL: bool = true;

fn block_ref(e: &EntryView) -> BlockRef {
    BlockRef {
        height: e.height,
        hash: *e.hash.as_bytes(),
    }
}

/// Core's `CDSNotificationInterface`, for its `m_mn_sync` calls (the other managers it calls are
/// not ported yet).
pub struct SyncListener {
    sync: Arc<MasternodeSync>,
    view: ChainViewHandle,
}

impl SyncListener {
    pub fn new(sync: Arc<MasternodeSync>, view: ChainViewHandle) -> SyncListener {
        SyncListener {
            sync,
            view,
        }
    }
}

impl ValidationInterface for SyncListener {
    // Core: dsnotificationinterface.cpp:37-40
    fn accepted_block_header(&self, id: BlockId) {
        if let Some(e) = self.view.entry(id) {
            self.sync.accepted_block_header(block_ref(&e));
        }
    }

    // Core: dsnotificationinterface.cpp:42-45
    fn notify_header_tip(&self, id: BlockId, ibd: bool) {
        self.sync.notify_header_tip(self.view.entry(id).as_ref().map(block_ref), ibd);
    }

    /// `UpdatedBlockTip(pindexNew, nullptr, ibd)`.
    // Core: dsnotificationinterface.cpp:31-35
    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        if let Some(tip) = tip {
            self.updated_block_tip(tip, None, ibd);
        }
    }

    /// The best header is the published view's, which is at least as new as the tip the signal
    /// is about (SP1 D12).
    // Core: dsnotificationinterface.cpp:56-65
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        if Some(new) == fork {
            return;
        }
        let Some(e) = self.view.entry(new) else {
            return;
        };
        let tip = self.view.snapshot().tip;
        let best = BlockRef {
            height: tip.best_header_height,
            hash: *tip.best_header_hash.as_bytes(),
        };
        self.sync.updated_block_tip(Some(best), block_ref(&e), ibd);
    }
}

/// The sync and the tick's network view, from step 7c: `mn_sync` with its notifier, and what
/// `Net::attach` registers and binds.
pub struct SyncParts {
    pub sync: Arc<MasternodeSync>,
    pub net: Arc<CoreSyncNet>,
    /// `CGovernanceManager::IsValid()` (SP1 D9).
    pub governance_valid: bool,
    cfg: SyncConfig,
    clock: Arc<dyn Clock>,
    netfulfilled: Arc<NetFulfilledRequestManager>,
}

impl SyncParts {
    /// `mn_sync` over `core`; `governance_valid` is `is_governance_enabled` (SP1 D8, D9).
    ///
    /// # Errors
    ///
    /// A `-syncmempool` that is not a boolean (from `settings.json`; Core reads it at each tick,
    /// SP1 D12).
    // Core: init.cpp:2086
    pub fn new(
        args: &ArgsManager,
        chain: &ChainParams,
        core: Arc<NetCore>,
        clock: Arc<dyn Clock>,
        netfulfilled: Arc<NetFulfilledRequestManager>,
        governance_valid: bool,
    ) -> Result<SyncParts, InitFailure> {
        let blocks_only = core.config().blocks_only;
        let net = Arc::new(CoreSyncNet::new(core, blocks_only));
        let notifier = FullSyncNotifier {
            net: net.clone(),
            netfulfilled: Arc::clone(&netfulfilled),
        };
        let sync = Arc::new(MasternodeSync::new(Arc::clone(&clock), Box::new(notifier)));
        let cfg = SyncConfig {
            mockable_chain: chain.is_mockable_chain,
            mainnet: chain.network == NetworkKind::Mainnet,
            sync_mempool: args.get_bool_arg("-syncmempool", DEFAULT_SYNC_MEMPOOL)?,
        };
        Ok(SyncParts {
            sync,
            net,
            governance_valid,
            cfg,
            clock,
            netfulfilled,
        })
    }

    /// Step 10a: netfulfilled's `DoMaintenance` every minute and the sync tick every second
    /// (`ScheduleHandlers`, `SyncManager::Schedule`), each a no-op once shutdown was requested.
    // Core: init.cpp:2394-2396; node/sync_manager.cpp:22-30
    pub fn schedule(&self, scheduler: &Scheduler, shutdown: &Arc<ShutdownState>) {
        let (netfulfilled, down) = (Arc::clone(&self.netfulfilled), Arc::clone(shutdown));
        scheduler.schedule_every(
            move || netfulfilled.do_maintenance(down.shutdown_requested()),
            Duration::from_secs(60),
        );
        let gov = Arc::new(EmptyGovernance {
            valid: self.governance_valid,
        });
        let tick = SyncManager::new(
            Arc::clone(&self.clock),
            self.cfg,
            Arc::clone(&self.sync),
            Arc::clone(&self.netfulfilled),
            self.net.clone(),
            gov,
        );
        let down = Arc::clone(shutdown);
        scheduler.schedule_every(
            move || tick.scheduled_tick(down.shutdown_requested()),
            Duration::from_secs(1),
        );
    }
}
