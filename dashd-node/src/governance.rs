//! GV1b in the node: Core's `CGovernanceManager` with its store, network handler, listener,
//! scheduled jobs and superblock back end, built over V, the transaction index, SP1's sync and
//! the processor.
//!
//! Order, as Core's: the handle and the back end exist before the connector; V is bound as it
//! spawns, before any block connects or anything posts; the listener registers after SP1's
//! mnsync listener; the store loads after the fulfilled-request and masternode caches; the jobs
//! are scheduled at step 10a while governance is valid; shutdown stores a loaded store after the
//! network and V have stopped.
//!
//! With `-disablegovernance` the manager stays unloaded: its file is neither read nor written,
//! the back end is absent (G2 refuses), and `NetGovernance` still answers `AlreadyHave`.
// Core: init.cpp v24.0.2:2149, :2242, :2260-2273; governance/governance.cpp v24.0.2:91-112

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;

use dash_dml::MnList;
use dash_evo::types::ProTxHash;
use dash_governance::GovParams;
use dash_governance::manager::{Effects, GovClock, MetaOp};
use dash_p2p::{NetCore, OutMsg};
use dash_p2p_core::Inventory;
use dash_params::ChainSetup;
use dashd_chainstate::chain::{ChainHandle, ValidationInterface};
use dashd_chainstate::connect::NodeHelper;
use dashd_governance::store::{self, GovernanceStore};
use dashd_governance::sync::{DmnTip, NodeGovernanceSync, apply_sync_bumps};
use dashd_governance::{
    ChainExec, CoreRng, EffectsSink, GovEnv, GovPeers, GovScheduler, GovernanceBackend,
    GovernanceHandle, GovernanceListener, NetGovernance, RelayInvs,
};
use dashd_indexes::{IndexError, IndexStatus, IndexSummary, IndexedTx, TxIndexReader, Txid};
use dashd_masternode::flatdb;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
use dashd_masternode::sync::MasternodeSync;
use dashd_masternode::sync_manager::{GovernanceSync, SyncNet};
use dashd_masternode::sync_net::CoreSyncNet;
use dashd_p2p::Shared;

use crate::InitFailure;
use crate::scheduler::Scheduler;

pub type Handle = GovernanceHandle<ChainExec>;

/// `GetTime()`, `GetAdjustedTime()` and mnsync's state.
struct NodeEnv {
    core: Arc<NetCore>,
    sync: Arc<MasternodeSync>,
}

impl GovEnv for NodeEnv {
    fn clock(&self) -> GovClock {
        GovClock::new(self.core.clock().node_now().0, self.core.adjusted_time())
    }

    fn synced(&self) -> bool {
        self.sync.is_synced()
    }

    fn blockchain_synced(&self) -> bool {
        self.sync.is_blockchain_synced()
    }
}

/// `g_txindex`, bound at step 7 once the indexes register: absent (`-txindex=0` on regtest) or
/// unbound, every collateral is a miss, as Core's `GetTransaction` without the index. A read
/// before the binding is a wiring fault: logged, and fatal in debug builds.
#[derive(Default)]
struct LateIndex(OnceLock<Option<Arc<dyn TxIndexReader>>>);

impl LateIndex {
    fn index(&self) -> Option<&Arc<dyn TxIndexReader>> {
        let Some(bound) = self.0.get() else {
            tracing::warn!(target: "gobject", "LateIndex -- a collateral read before the txindex was bound");
            debug_assert!(false, "LateIndex: a collateral read before the txindex was bound");
            return None;
        };
        bound.as_ref()
    }
}

impl IndexStatus for LateIndex {
    fn summary(&self) -> IndexSummary {
        match self.0.get() {
            Some(Some(index)) => index.summary(),
            _ => IndexSummary {
                name: "txindex",
                synced: false,
                best_block_height: 0,
            },
        }
    }

    fn block_until_synced(&self) -> bool {
        false
    }
}

impl TxIndexReader for LateIndex {
    fn find_tx(&self, txid: &Txid) -> Result<Option<IndexedTx>, IndexError> {
        self.index().map_or(Ok(None), |index| index.find_tx(txid))
    }

    fn has_tx(&self, txid: &Txid) -> Result<bool, IndexError> {
        self.index().map_or(Ok(false), |index| index.has_tx(txid))
    }
}

/// N's effects worker: one thread that applies each command's effects in submission order, after
/// the governance lock is released. Submitting never waits.
struct EffectsWorker {
    tx: Mutex<Option<crossbeam_channel::Sender<Effects>>>,
    join: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    log: Arc<tests::Log>,
    /// Effects submitted, and applied.
    #[cfg(test)]
    counts: Arc<[std::sync::atomic::AtomicU64; 2]>,
}

impl EffectsWorker {
    fn spawn(
        relay: Arc<RelayInvs>,
        sync: Arc<MasternodeSync>,
        meta: Arc<MasternodeMetaMan>,
    ) -> std::io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<Effects>();
        #[cfg(test)]
        let counts: Arc<[std::sync::atomic::AtomicU64; 2]> = Arc::default();
        #[cfg(test)]
        let applied = Arc::clone(&counts);
        let join = std::thread::Builder::new().name("gov-effects".into()).spawn(move || {
            for effects in rx {
                relay.queue(&effects);
                apply_sync_bumps(&sync, &effects);
                for op in effects.meta {
                    match op {
                        MetaOp::AddGovernanceVote {
                            pro_tx_hash,
                            object,
                        } => meta.add_governance_vote(&ProTxHash::from(pro_tx_hash), object),
                        MetaOp::RemoveObject(object) => meta.remove_governance_object(&object),
                        MetaOp::AddVote(_) => {}
                    }
                }
                #[cfg(test)]
                applied[1].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        })?;
        Ok(Self {
            tx: Mutex::new(Some(tx)),
            join: Mutex::new(Some(join)),
            #[cfg(test)]
            log: Arc::default(),
            #[cfg(test)]
            counts,
        })
    }

    /// Applies what was submitted, then ends the thread.
    fn stop(&self) {
        drop(self.tx.lock().unwrap_or_else(PoisonError::into_inner).take());
        if let Some(join) = self.join.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = join.join();
        }
    }
}

impl EffectsSink for EffectsWorker {
    fn submit(&self, effects: Effects) {
        #[cfg(test)]
        self.log.note(tests::Event::Effects);
        if let Some(tx) = &*self.tx.lock().unwrap_or_else(PoisonError::into_inner) {
            #[cfg(test)]
            self.counts[0].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = tx.send(effects);
        }
    }
}

/// The processor's sends for the handler and the jobs.
struct NodePeers {
    shared: Arc<Shared>,
    net: Arc<CoreSyncNet>,
}

impl GovPeers for NodePeers {
    fn push_inventory(&self, peer: i64, inv: &Inventory) {
        self.shared.push_inventory(peer, inv);
    }

    fn relay_inv(&self, inv: &Inventory) {
        self.shared.relay_inv(inv);
    }

    fn forget_object_request(&self, inv: &Inventory) {
        self.shared.forget_object_request(inv);
    }

    fn relay_peers(&self) -> Vec<i64> {
        self.net.fully_connected().into_iter().filter(|p| p.can_relay).map(|p| p.id).collect()
    }

    fn push(&self, peer: i64, msg: OutMsg) {
        self.shared.push_to(peer, msg);
    }
}

impl GovScheduler for Scheduler {
    fn schedule_every(&self, job: Box<dyn Fn() + Send + Sync>, every: std::time::Duration) {
        Scheduler::schedule_every(self, job, every);
    }
}

/// The node's governance (`node.govman` and its wiring).
pub struct NodeGovernance {
    enabled: bool,
    exec: ChainExec,
    index: Arc<LateIndex>,
    handle: Arc<Handle>,
    relay: Arc<RelayInvs>,
    worker: Arc<EffectsWorker>,
    sync: Arc<MasternodeSync>,
    seam: Arc<NodeGovernanceSync<ChainExec>>,
    /// The store's path and network once `LoadCache` succeeded: shutdown stores it there.
    file: Mutex<Option<(PathBuf, [u8; 4])>>,
}

impl NodeGovernance {
    /// The unloaded manager, before the chainstate loads; `enabled` is `is_governance_enabled`.
    ///
    /// # Errors
    ///
    /// The chain's governance parameters are invalid, or the effects worker cannot start.
    // Core: init.cpp v24.0.2:2149
    pub fn new(
        enabled: bool,
        chain: &ChainSetup,
        core: Arc<NetCore>,
        sync: Arc<MasternodeSync>,
        meta: Arc<MasternodeMetaMan>,
    ) -> Result<Arc<Self>, InitFailure> {
        let params = GovParams::from_chain_params(chain)
            .map_err(|e| InitFailure::Exception(format!("governance parameters: {e}")))?;
        let relay = Arc::new(RelayInvs::default());
        let worker = EffectsWorker::spawn(Arc::clone(&relay), Arc::clone(&sync), meta)
            .map_err(|e| InitFailure::Exception(e.to_string()))?;
        let worker = Arc::new(worker);
        let exec = ChainExec::default();
        let index = Arc::new(LateIndex::default());
        let env = Arc::new(NodeEnv {
            core,
            sync: Arc::clone(&sync),
        });
        let handle = GovernanceHandle::new(
            dash_governance::manager::Manager::new(),
            exec.clone(),
            env,
            Arc::clone(&worker) as _,
            Arc::clone(&index) as _,
            params.clone(),
        );
        let seam = Arc::new(if enabled {
            let dmn: Arc<dyn DmnTip> = Arc::new(exec.clone());
            let rng = Box::new(CoreRng::default());
            NodeGovernanceSync::new(Arc::clone(&handle), Arc::clone(&sync), dmn, rng, &params)
        } else {
            NodeGovernanceSync::disabled(Arc::clone(&sync))
        });
        Ok(Arc::new(Self {
            enabled,
            exec,
            index,
            handle,
            relay,
            worker,
            sync,
            seam,
            file: Mutex::new(None),
        }))
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn handle(&self) -> &Arc<Handle> {
        &self.handle
    }

    /// The connector's helper. Enabled, it holds the superblock back end and SP1's sync state,
    /// which lift G2; disabled, neither, so `gov_valid` is false and G2 refuses.
    pub fn helper(&self) -> NodeHelper {
        if !self.enabled {
            return NodeHelper::default();
        }
        let backend = GovernanceBackend::new(Arc::clone(&self.handle), self.dmn());
        let sync = Arc::clone(&self.sync);
        NodeHelper::with_governance(
            Some(Box::new(backend)),
            Some(Arc::new(move || sync.is_synced())),
        )
    }

    /// Binds V as it spawns, before anything reads or posts through the executor.
    ///
    /// # Errors
    ///
    /// V was already bound.
    pub fn bind(&self, v: ChainHandle) -> Result<(), InitFailure> {
        if !self.exec.bind(v) {
            return Err(InitFailure::Exception("governance: V is already bound".to_owned()));
        }
        #[cfg(test)]
        self.note(tests::Event::Bound);
        Ok(())
    }

    /// `g_txindex` for the collateral reads: IX1's index host, or none.
    pub fn bind_index(&self, index: Option<Arc<dyn TxIndexReader>>) {
        let _ = self.index.0.set(index);
    }

    /// SP1's tick seam.
    pub fn sync_seam(&self) -> Arc<dyn GovernanceSync> {
        Arc::clone(&self.seam) as _
    }

    /// `IsValid() && IsBlockchainSynced()`: governance inventory is requested.
    // Core: governance/net_governance.cpp v24.0.2:280
    pub fn ready(&self) -> bool {
        self.seam.governance_ready()
    }

    fn dmn(&self) -> Arc<dyn DmnTip> {
        Arc::new(self.exec.clone())
    }

    /// `LoadCache(fLoadCacheFiles)`, enabled only: `load` reads the store and runs `CheckAndRemove`
    /// and `InitOnLoad` at the active height; otherwise the empty store is written. Disabled,
    /// nothing is read or written and the manager stays unloaded.
    ///
    /// # Errors
    ///
    /// Core's init error for a store it refuses to load or cannot clear.
    // Core: init.cpp v24.0.2:2260-2267; governance/governance.cpp v24.0.2:99-112
    pub fn load_cache(
        &self,
        dir: PathBuf,
        message_start: [u8; 4],
        load: bool,
        tip_height: i32,
    ) -> Result<(), InitFailure> {
        if !self.enabled {
            return Ok(());
        }
        let path = dir.join(store::FILE);
        let loaded = if load {
            let read = store::load(&path, message_start).ok();
            if let Some(GovernanceStore {
                state,
                ..
            }) = read
            {
                let list: Arc<MnList> = self.dmn().dmn_tip_list();
                self.handle.load_cache(state, tip_height, &list);
                #[cfg(test)]
                self.note(tests::Event::Loaded);
                true
            } else {
                self.handle.set_loaded(false);
                false
            }
        } else {
            let written = flatdb::log_write(store::store(Default::default(), &path, message_start));
            self.handle.set_loaded(written);
            written
        };
        if !loaded {
            let path = path.display();
            return Err(InitFailure::Error(if load {
                format!("Failed to load governance cache from {path}")
            } else {
                format!("Failed to clear governance cache at {path}")
            }));
        }
        *self.file.lock().unwrap_or_else(PoisonError::into_inner) = Some((dir, message_start));
        Ok(())
    }

    /// `NetGovernance` over the processor, registered on every node.
    pub fn net_handler(
        &self,
        fulfilled: &Arc<NetFulfilledRequestManager>,
        shared: &Arc<Shared>,
        net: &Arc<CoreSyncNet>,
    ) -> NetGovernance<ChainExec> {
        let state: Arc<dyn dashd_governance::GovSyncState> = Arc::clone(&self.seam) as _;
        NetGovernance::new(
            Arc::clone(&self.handle),
            state,
            self.dmn(),
            Arc::clone(fulfilled),
            Arc::new(NodePeers {
                shared: Arc::clone(shared),
                net: Arc::clone(net),
            }),
            Arc::clone(&self.relay),
            Arc::clone(&self.worker) as _,
        )
    }

    /// The `CDSNotificationInterface` governance hooks, for every node: a disabled manager is
    /// never loaded, so they do nothing.
    ///
    /// # Errors
    ///
    /// V is not bound yet: a post would be dropped.
    pub fn listener(&self, v: &ChainHandle) -> Result<Arc<dyn ValidationInterface>, InitFailure> {
        if !self.exec.is_bound() {
            return Err(InitFailure::Exception(
                "governance: the listener registers before V is bound".to_owned(),
            ));
        }
        Ok(Arc::new(GovernanceListener::new(Arc::clone(&self.handle), self.dmn(), v.view_handle())))
    }

    /// `NetGovernance::Schedule` at step 10a: nothing unless governance is valid.
    // Core: governance/net_governance.cpp v24.0.2:39-78
    pub fn schedule(&self, handler: &NetGovernance<ChainExec>, scheduler: &Scheduler) {
        handler.schedule(scheduler, Box::new(CoreRng::default()));
    }

    /// Drains the effects worker and ends it, once V and the network have stopped.
    pub fn stop_effects(&self) {
        self.worker.stop();
    }

    /// `~CGovernanceManager` on a clean shutdown, after [`Self::stop_effects`]: a loaded store is
    /// written atomically.
    // Core: init.cpp v24.0.2:414; governance/governance.cpp v24.0.2:91-97
    pub fn flush(&self) {
        let Some((dir, message_start)) =
            self.file.lock().unwrap_or_else(PoisonError::into_inner).take()
        else {
            return;
        };
        let Some(state) = self.handle.stored_state() else {
            return;
        };
        // Core: flat-database.h v24.0.2:56, :65
        flatdb::log_write(store::store(state, &dir.join(store::FILE), message_start));
    }

    #[cfg(test)]
    pub(crate) fn note(&self, event: tests::Event) {
        self.worker.log.note(event);
    }

    #[cfg(test)]
    pub(crate) fn events(&self) -> Vec<tests::Event> {
        self.worker.log.events()
    }

    #[cfg(test)]
    pub(crate) fn exec(&self) -> &ChainExec {
        &self.exec
    }

    #[cfg(test)]
    pub(crate) fn sync(&self) -> &MasternodeSync {
        &self.sync
    }

    /// Whether the effects worker applied every effect submitted to it.
    #[cfg(test)]
    pub(crate) fn effects_applied(&self) -> bool {
        let [submitted, applied] = &*self.worker.counts;
        let ordering = std::sync::atomic::Ordering::SeqCst;
        submitted.load(ordering) == applied.load(ordering)
    }
}

#[cfg(test)]
pub(crate) mod tests;
