//! SP1 in the node: Core's `CDSNotificationInterface` hooks into `CMasternodeSync` on V's
//! signals, and the parts of the sync tick that step 7c and step 10a build and schedule. The
//! managers with files (`netfulfilledman`, `mn_metaman`) are [`crate::steps::ChainSteps`]', as
//! `sporkman` is.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dash_args::ArgsManager;
use dash_chain::{ChainPolicy, EntryView};
use dash_clock::Clock;
use dash_consensus::BlockId;
use dash_p2p::{ConnectionType, NetCore, PeerHandle, Why};
use dash_params::{ChainSetup, Network};
use dashd_chainstate::chain::{ChainViewHandle, ValidationInterface};
use dashd_masternode::mnconn::{MaintenancePeer, QuorumNodes, do_maintenance};
use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
use dashd_masternode::sync::{BlockRef, MasternodeSync};
use dashd_masternode::sync_manager::{
    EmptyGovernance, FullSyncNotifier, GovernanceSync, SyncConfig, SyncManager,
};
use dashd_masternode::sync_net::CoreSyncNet;

use crate::InitFailure;
use crate::coinjoin::role::Ticks;
use crate::logging::Logger;
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
    /// The tick's governance; [`EmptyGovernance`] until one is set.
    governance: Option<Arc<dyn GovernanceSync>>,
    /// [`SyncParts::schedule`] ran: the tick holds its governance.
    scheduled: AtomicBool,
    cfg: SyncConfig,
    clock: Arc<dyn Clock>,
    pub(crate) netfulfilled: Arc<NetFulfilledRequestManager>,
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
        chain: &ChainSetup,
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
            mockable_chain: ChainPolicy::for_setup(chain, false).is_mockable_chain,
            mainnet: chain.network == Network::Main,
            sync_mempool: args.get_bool_arg("-syncmempool", DEFAULT_SYNC_MEMPOOL)?,
        };
        Ok(SyncParts {
            sync,
            net,
            governance_valid,
            governance: None,
            scheduled: AtomicBool::new(false),
            cfg,
            clock,
            netfulfilled,
        })
    }

    /// The governance the tick reads, in place of [`EmptyGovernance`]. Call it before
    /// [`SyncParts::schedule`], which hands the tick its governance once; a later call would
    /// change nothing, and debug builds panic on it.
    pub fn set_governance(&mut self, governance: Arc<dyn GovernanceSync>) {
        debug_assert!(
            !self.scheduled.load(Ordering::SeqCst),
            "set_governance after schedule: the tick already holds its governance"
        );
        self.governance = Some(governance);
    }

    fn governance(&self) -> Arc<dyn GovernanceSync> {
        self.governance.clone().unwrap_or_else(|| {
            Arc::new(EmptyGovernance {
                valid: self.governance_valid,
            })
        })
    }

    /// Step 10a: netfulfilled's `DoMaintenance` and the masternode connections' `DoMaintenance`
    /// (`maintenance`) every minute, then every second `ScheduleHandlers` in handler order (the
    /// CoinJoin server's tick, then `SyncManager::Schedule`'s) and `cj_walletman`'s queue
    /// maintenance (`coinjoin`), each a no-op once shutdown was requested.
    // Core: init.cpp:2363-2366, :2372-2373; node/sync_manager.cpp:22-30
    pub fn schedule(
        &self,
        scheduler: &Scheduler,
        shutdown: &Arc<ShutdownState>,
        maintenance: MnMaintenance,
        coinjoin: &Ticks,
    ) {
        let (netfulfilled, down) = (Arc::clone(&self.netfulfilled), Arc::clone(shutdown));
        scheduler.schedule_every(
            move || netfulfilled.do_maintenance(down.shutdown_requested()),
            Duration::from_secs(60),
        );
        maintenance.schedule(scheduler);
        self.scheduled.store(true, Ordering::SeqCst);
        let tick = SyncManager::new(
            Arc::clone(&self.clock),
            self.cfg,
            Arc::clone(&self.sync),
            Arc::clone(&self.netfulfilled),
            self.net.clone(),
            self.governance(),
        );
        coinjoin.schedule(scheduler, shutdown, Arc::new(move |down| tick.scheduled_tick(down)));
    }
}

/// Core's `CMasternodeUtils::DoMaintenance` on the node's scheduler, for every node: P6's pass
/// over the connection table in `AllNodes` order, with its lines logged and its disconnects
/// applied.
pub struct MnMaintenance {
    core: Arc<NetCore>,
    sync: Arc<MasternodeSync>,
    quorum_nodes: Arc<QuorumNodes>,
    shutdown: Arc<ShutdownState>,
    logger: Arc<Logger>,
}

impl MnMaintenance {
    pub fn new(
        core: Arc<NetCore>,
        sync: Arc<MasternodeSync>,
        quorum_nodes: Arc<QuorumNodes>,
        shutdown: Arc<ShutdownState>,
        logger: Arc<Logger>,
    ) -> MnMaintenance {
        MnMaintenance {
            core,
            sync,
            quorum_nodes,
            shutdown,
            logger,
        }
    }

    /// Every minute, the first one minute after step 10a.
    // Core: init.cpp:2364
    fn schedule(self, scheduler: &Scheduler) {
        scheduler.schedule_every(move || self.tick(), Duration::from_secs(60));
    }

    /// One pass. rdashd loads no wallet, so the mixing masternodes are none, as Core's
    /// `cj_walletman` has no client manager to name one without a loaded wallet
    /// (walletman.cpp:181-186).
    // Core: masternode/utils.cpp:16-83; net.h:1273; net.cpp:4570-4573
    pub(crate) fn tick(&self) {
        let core = &*self.core;
        let peers: Vec<MaintenancePeer> = core.peers().iter().map(maintenance_peer).collect();
        let cfg = core.config();
        let max_outbound =
            cfg.max_outbound_full_relay + cfg.max_outbound_block_relay + cfg.max_feeler;
        let result = do_maintenance(
            self.sync.is_blockchain_synced(),
            self.shutdown.shutdown_requested(),
            &peers,
            max_outbound,
            core.clock().node_now().0,
            &self.quorum_nodes,
            &[],
            self.logger.log_ips(),
        );
        for line in &result.log {
            tracing::debug!(target: "net|netconn", "{line}");
        }
        for id in result.disconnect {
            core.disconnect(id, Why::Processor("masternode maintenance"));
        }
    }
}

// Core: masternode/utils.cpp:25-31, :41-74
fn maintenance_peer(peer: &PeerHandle) -> MaintenancePeer {
    let verified = peer.verified_proreg_hash();
    MaintenancePeer {
        id: peer.id(),
        inbound: peer.is_inbound(),
        feeler: peer.conn_type() == ConnectionType::Feeler,
        manual: peer.conn_type() == ConnectionType::Manual,
        mn_conn: peer.masternode_connection(),
        probe: peer.masternode_probe(),
        verified: (verified != [0; 32]).then_some(verified),
        connected_at: peer.connected(),
        qwatch: peer.qwatch(),
        addr: peer.addr().service.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dash_p2p::{Address, Attach, MemSocket, Permissions, Service, ServiceFlags};
    use dash_params::{ChainParamsBuilder, Network as CoreNetwork, Release};
    use dashd_masternode::sync::NodeSyncNotifier;
    use dashd_net::sim::{DialSim, SIM_START};
    use dashd_p2p::sim::VirtualClock;
    use tracing::Level;

    use super::*;

    struct Quiet;

    impl NodeSyncNotifier for Quiet {
        fn sync_reset(&self) {}

        fn sync_finished(&self) {}
    }

    /// A core whose `GetMaxOutboundNodeCount` is `max_outbound`: full-relay slots only.
    fn sim(max_outbound: usize) -> DialSim {
        sim_with(max_outbound, 0, 0)
    }

    /// A core with these full-relay, block-relay and feeler limits.
    fn sim_with(full_relay: usize, block_relay: usize, feeler: usize) -> DialSim {
        let params =
            Arc::new(ChainParamsBuilder::new(CoreNetwork::Main, Release::V24_0).build().unwrap());
        DialSim::new(params, |cfg| {
            cfg.dns_seed = false;
            cfg.fixed_seeds = false;
            cfg.max_outbound_full_relay = full_relay;
            cfg.max_outbound_block_relay = block_relay;
            cfg.max_feeler = feeler;
        })
    }

    /// An unverified outbound masternode connection; the far end is kept open.
    fn masternode_connection(sim: &DialSim) -> (PeerHandle, MemSocket) {
        let (peer, far) = outbound(sim, 1, true);
        assert_eq!(peer.connected(), SIM_START);
        (peer, far)
    }

    /// An unverified outbound full-relay connection from `31.9.0.<host>`, a masternode
    /// connection when `masternode`; the far end is kept open.
    fn outbound(sim: &DialSim, host: u8, masternode: bool) -> (PeerHandle, MemSocket) {
        let (ours, theirs) = MemSocket::pair();
        let peer = sim.core.attach(Attach {
            sock: Arc::new(ours),
            addr: Address {
                service: Service::new(dash_p2p::NetAddr::ipv4([31, 9, 0, host]), 9999),
                services: ServiceFlags::NODE_NETWORK,
                time: 0,
            },
            addr_bind: Service::default(),
            dest: None,
            conn_type: ConnectionType::OutboundFullRelay,
            inbound_onion: false,
            permissions: Permissions::NONE,
            use_v2: false,
            grant: None,
            masternode,
            masternode_probe: false,
        });
        (peer, theirs)
    }

    fn maintenance(sim: &DialSim) -> MnMaintenance {
        maintenance_with(sim, true, false)
    }

    /// The task with the blockchain `synced` or not, and with a shutdown requested or not.
    fn maintenance_with(sim: &DialSim, synced: bool, shutdown: bool) -> MnMaintenance {
        let sync = Arc::new(MasternodeSync::new(VirtualClock::at(SIM_START), Box::new(Quiet)));
        if synced {
            sync.switch_to_next_asset();
        }
        let down = Arc::new(ShutdownState::new());
        if shutdown {
            down.start_shutdown();
        }
        MnMaintenance::new(
            Arc::clone(&sim.core),
            sync,
            Arc::new(QuorumNodes::default()),
            down,
            Arc::new(Logger::new()),
        )
    }

    fn closing(lines: &[(Level, String)]) -> Vec<String> {
        lines
            .iter()
            .filter(|(level, line)| {
                *level == Level::DEBUG && line.starts_with("Closing Masternode connection")
            })
            .map(|(_, line)| line.clone())
            .collect()
    }

    /// S3c: the parts hold the governance set on them, and Core's empty manager with the parts'
    /// validity until one is set.
    #[test]
    fn s3c_the_parts_hold_the_governance_set_on_them() {
        let sim = sim(1);
        let mut parts = sync_parts(&sim);
        assert!(parts.governance().is_valid());
        let gov: Arc<dyn GovernanceSync> = Arc::new(EmptyGovernance {
            valid: false,
        });
        parts.set_governance(Arc::clone(&gov));
        assert!(Arc::ptr_eq(&parts.governance(), &gov));
        assert!(!parts.governance().is_valid());
    }

    /// A governance that records whether the tick asked if it is valid, and answers no.
    #[derive(Default)]
    struct Recording(AtomicBool);

    impl GovernanceSync for Recording {
        fn is_valid(&self) -> bool {
            self.0.store(true, Ordering::SeqCst);
            false
        }

        fn vote_count(&self) -> i32 {
            0
        }

        fn enabled_masternode_count(&self) -> i32 {
            0
        }

        fn fetch_governance_object_votes(
            &self,
            _peers_per_hash_max: usize,
            _now: i64,
            _asked: &mut dashd_masternode::sync_manager::AskedRecently,
        ) -> (Vec<dashd_masternode::meta::GovHash>, Vec<dashd_masternode::meta::GovHash>) {
            (Vec::new(), Vec::new())
        }

        fn vote_bloom_filter(
            &self,
            _hash: &dashd_masternode::meta::GovHash,
        ) -> dash_p2p_core::FilterLoad {
            dashd_masternode::sync_manager::empty_bloom_filter()
        }
    }

    /// S3c: the tick `schedule` builds reads the governance set on the parts. In GOVERNANCE with
    /// one fully connected peer, the second tick (6 s on) asks it `IsValid()`.
    // Core: init.cpp:2394-2396; node/sync_manager.cpp:22-30, :239-243
    #[test]
    fn s3c_the_scheduled_tick_reads_the_set_governance() {
        let sim = sim(1);
        let (peer, _far) = outbound(&sim, 2, false);
        peer.set_successfully_connected_for_tests();
        let mut parts = sync_parts(&sim);
        let dir = tempfile::tempdir().unwrap();
        let chain = ChainParamsBuilder::new(CoreNetwork::Regtest, Release::V24_0).build().unwrap();
        assert!(parts.netfulfilled.load_cache(
            &dir.path().join("netfulfilled.dat"),
            chain.message_start,
            false
        ));
        let gov = Arc::new(Recording::default());
        parts.set_governance(gov.clone());
        parts.sync.switch_to_next_asset();
        let scheduler = Scheduler::new();
        parts.schedule(
            &scheduler,
            &Arc::new(ShutdownState::new()),
            maintenance(&sim),
            &Ticks::default(),
        );
        scheduler.mock_forward(Duration::from_secs(1));
        let clock = Arc::clone(&sim.clock);
        scheduler.schedule_from_now(
            move || clock.advance(Duration::from_secs(6)),
            Duration::from_millis(500),
        );
        let stop = scheduler.clone();
        scheduler.schedule_from_now(move || stop.stop(), Duration::from_millis(1500));
        scheduler.service_queue();
        assert!(gov.0.load(Ordering::SeqCst), "the tick read the set governance");
        assert!(parts.sync.is_synced(), "an invalid governance ends GOVERNANCE");
    }

    /// Regtest parts over `sim`'s core, governance enabled.
    fn sync_parts(sim: &DialSim) -> SyncParts {
        let clock: Arc<dyn Clock> = sim.clock.clone();
        let chain = ChainParamsBuilder::new(CoreNetwork::Regtest, Release::V24_0).build().unwrap();
        let netfulfilled = Arc::new(NetFulfilledRequestManager::new(Arc::clone(&clock), 300));
        let args = crate::init::test_args(&[]);
        SyncParts::new(&args, &chain, Arc::clone(&sim.core), clock, netfulfilled, true).unwrap()
    }

    /// S3c: the tick takes its governance at `schedule`, so setting one afterwards is a
    /// startup-order bug that debug builds stop.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "set_governance after schedule")]
    fn s3c_set_governance_after_schedule_panics_in_debug() {
        let sim = sim(1);
        let mut parts = sync_parts(&sim);
        parts.schedule(
            &Scheduler::new(),
            &Arc::new(ShutdownState::new()),
            maintenance(&sim),
            &Ticks::default(),
        );
        parts.set_governance(Arc::new(EmptyGovernance {
            valid: false,
        }));
    }

    /// P7 W4: the scheduler's minute task runs P6 over the node's connection table. With the
    /// node at its outbound count, an unverified masternode connection 5 s old is closed with
    /// Core's line; under the count, or at 4 s, nothing is.
    // Core: init.cpp:2364; masternode/utils.cpp:22-38, :62-64, :75-81 at v24.0.0
    #[test]
    fn p7_w4_scheduled_maintenance_closes_unverified_masternode_connections_at_the_count() {
        let sim = sim(1);
        let (peer, _far) = masternode_connection(&sim);
        sim.clock.advance(Duration::from_secs(4));
        let ((), lines) = crate::test_log::logged(|| maintenance(&sim).tick());
        assert!(closing(&lines).is_empty() && !peer.disconnect_requested(), "4 s");

        sim.clock.advance(Duration::from_secs(1));
        let scheduler = Scheduler::new();
        maintenance(&sim).schedule(&scheduler);
        scheduler.mock_forward(Duration::from_secs(60));
        let stop = scheduler.clone();
        scheduler.schedule_from_now(move || stop.stop(), Duration::from_millis(1));
        let ((), lines) = crate::test_log::logged(|| scheduler.service_queue());
        assert_eq!(closing(&lines), [format!("Closing Masternode connection: peer={}", peer.id())]);
        assert!(peer.disconnect_requested());

        let under = self::sim(2);
        let (peer, _far) = masternode_connection(&under);
        under.clock.advance(Duration::from_secs(5));
        let ((), lines) = crate::test_log::logged(|| maintenance(&under).tick());
        assert!(closing(&lines).is_empty() && !peer.disconnect_requested(), "under the count");
    }

    /// P7 W4: the tick's outbound count is `GetMaxOutboundNodeCount`, the sum of the full-relay,
    /// block-relay and feeler limits (1 + 1 + 1 here). Two counted peers close nothing; three
    /// close the masternode connection.
    // Core: net.h:1273; masternode/utils.cpp:22-38 at v24.0.0
    #[test]
    fn p7_w4_maintenance_count_is_the_sum_of_the_outbound_limits() {
        let sim = sim_with(1, 1, 1);
        let (mn, _far) = masternode_connection(&sim);
        let (_second, _far2) = outbound(&sim, 2, false);
        sim.clock.advance(Duration::from_secs(5));
        let ((), lines) = crate::test_log::logged(|| maintenance(&sim).tick());
        assert!(closing(&lines).is_empty() && !mn.disconnect_requested(), "two of three");

        let (_third, _far3) = outbound(&sim, 3, false);
        let ((), lines) = crate::test_log::logged(|| maintenance(&sim).tick());
        assert_eq!(closing(&lines), [format!("Closing Masternode connection: peer={}", mn.id())]);
        assert!(mn.disconnect_requested());
    }

    /// P7 W4: an unsynced tick and a tick after a shutdown request close nothing, at the count.
    // Core: masternode/utils.cpp:19-20 at v24.0.0
    #[test]
    fn p7_w4_maintenance_skips_unsynced_and_shutdown_ticks() {
        let sim = sim(1);
        let (peer, _far) = masternode_connection(&sim);
        sim.clock.advance(Duration::from_secs(5));
        for (synced, shutdown, why) in [(false, false, "unsynced"), (true, true, "shutdown")] {
            let ((), lines) =
                crate::test_log::logged(|| maintenance_with(&sim, synced, shutdown).tick());
            assert!(closing(&lines).is_empty() && !peer.disconnect_requested(), "{why}");
        }
        maintenance(&sim).tick();
        assert!(peer.disconnect_requested(), "synced and running");
    }
}
