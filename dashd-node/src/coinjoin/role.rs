//! What step 7c installs for CoinJoin, by the node's role, and the 1 s tasks step 10a schedules
//! for it. Core citations are v24.0.0.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_chain::SporkView;
use dash_coinjoin::dstx::PoolLimits;
use dash_coinjoin::queue::MixingMeta;
use dash_p2p::NetCore;
use dashd_chainstate::chain::{ChainHandle, ValidationInterface};
use dashd_coinjoin::adapters::{ActiveMnAdapter, MetaManMixing};
use dashd_coinjoin::dsq::DsqRuntime;
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::env::{ActiveMn, ChainView, Mempool, MnLists, PeerSink};
use dashd_coinjoin::objects::RuntimeObjects;
use dashd_coinjoin::server::{CoinJoinServer, RandomSource, ServerClock, ServerInfo, SharedServer};
use dashd_masternode::active::ActiveMasternode;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::sync::MasternodeSync;
use dashd_masternode::tip::TipMnList;
use dashd_p2p::processing::seams::CoinJoinObjects;
use dashd_p2p::{NetHandler, Shared};

use super::adapters::{
    NodeChainView, NodeMnLists, NodePeerSink, NodePin, NodeRandom, NodeServerClock,
};
use super::listener::DstxListener;
use super::mempool::NodeMempool;
use crate::p2p::active_mn::NodeLists;
use crate::scheduler::Scheduler;
use crate::shutdown::ShutdownState;

type Lists = NodeMnLists<NodeLists>;

/// The masternode's server over the node's adapters.
pub type Server = CoinJoinServer<
    NodeChainView,
    Lists,
    ActiveMnAdapter,
    NodeMempool,
    NodePeerSink,
    NodeServerClock,
    NodeRandom,
    MetaManMixing,
>;

/// `cj_walletman`'s queue manager, with no wallets loaded.
pub type Wallet = DsqRuntime<NodeChainView, Lists, MetaManMixing>;

/// Step 7c's CoinJoin inputs: Core's `node.dstxman`, `mn_metaman` and `active_ctx`.
pub struct CoinJoin {
    dstx: Arc<DstxRuntime>,
    limits: PoolLimits,
    metaman: Arc<MasternodeMetaMan>,
    active: Option<Arc<ActiveMasternode>>,
    mempool: NodeMempool,
}

/// The role and the DSTX listener [`CoinJoin::build`] gives `Net::attach` with the P2P objects.
pub(crate) struct Built {
    pub role: Role,
    pub listener: Arc<dyn ValidationInterface>,
}

impl CoinJoin {
    /// The one DSTX manager, with `limits` from the chain's parameters.
    // Core: init.cpp:2139-2140
    pub fn new(
        limits: PoolLimits,
        metaman: Arc<MasternodeMetaMan>,
        active: Option<Arc<ActiveMasternode>>,
    ) -> Self {
        Self {
            dstx: Arc::new(DstxRuntime::new(limits)),
            limits,
            metaman,
            active,
            mempool: NodeMempool::default(),
        }
    }

    /// The server's mempool; without one it admits nothing.
    pub fn with_mempool(mut self, mempool: NodeMempool) -> Self {
        self.mempool = mempool;
        self
    }

    #[cfg(test)]
    pub(crate) fn dstx(&self) -> &Arc<DstxRuntime> {
        &self.dstx
    }

    /// The server on a masternode, otherwise the wallet queue manager, which keeps queues only
    /// with `relay_txs`. Every counter is `mn_metaman`'s through [`MetaManMixing`]; the counters
    /// mutex is shared only by `msghand`'s users: the objects and the wallet queues (W2-r1 N3).
    /// The objects, the server and the listener share the DSTX manager.
    // Core: init.cpp:2174-2188, :2228-2233; coinjoin/walletman.cpp:98-110
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build(
        self,
        v: &ChainHandle,
        tip: &TipMnList,
        sync: &Arc<MasternodeSync>,
        sporks: Arc<dyn SporkView>,
        shared: &Arc<Shared>,
        core: &Arc<NetCore>,
        relay_txs: bool,
    ) -> (Box<dyn CoinJoinObjects>, Built) {
        let chain = NodeChainView::new(v.view_handle(), Arc::clone(sync), sporks);
        let lists = || {
            NodeMnLists::new(
                tip.clone(),
                v.view_handle(),
                NodeLists {
                    v: v.clone(),
                },
            )
        };
        let (objects_chain, mns) = (Arc::new(chain.clone()), Arc::new(lists()));
        let meta = MetaManMixing(Arc::clone(&self.metaman));
        let counters = Arc::new(Mutex::new(meta.clone()));
        let role = match self.active {
            Some(active) => Role::Server(NodeServer(SharedServer::new(CoinJoinServer::new(
                self.limits,
                chain.clone(),
                lists(),
                ActiveMnAdapter(active),
                self.mempool,
                NodePeerSink::new(Arc::clone(shared)),
                NodeServerClock::new(Arc::clone(core)),
                NodeRandom,
                meta,
                Arc::clone(&self.dstx),
            )))),
            None => Role::Wallet(DsqRuntime::new(
                relay_txs,
                Arc::clone(&objects_chain),
                Arc::clone(&mns),
                Arc::clone(&counters),
            )),
        };
        let queues = match &role {
            Role::Wallet(wallet) => Some(wallet.clone()),
            Role::Server(_) => None,
        };
        let objects =
            RuntimeObjects::new(Arc::clone(&self.dstx), objects_chain, mns, counters, queues);
        let listener = Arc::new(DstxListener::new(self.dstx, chain, v.view_handle()));
        let built = Built {
            role,
            listener,
        };
        (Box::new(objects.with_pin(NodePin)), built)
    }
}

/// The node's CoinJoin role: the `CCoinJoinServer` handler on a masternode, otherwise
/// `cj_walletman` as the `dsq` extension. One of them owns `dsq`.
pub enum Role {
    Server(NodeServer<Server>),
    Wallet(Wallet),
}

impl Role {
    /// The registry's handler, sharing this role's state.
    pub(crate) fn handler(&self) -> Box<dyn NetHandler> {
        match self {
            Role::Server(server) => Box::new(server.0.clone()),
            Role::Wallet(wallet) => Box::new(wallet.clone()),
        }
    }

    /// The server's tick, or the queue maintenance, which exists only with relay.
    // Core: coinjoin/walletman.cpp:133-137, :210-214
    pub fn ticks(&self, core: &Arc<NetCore>) -> Ticks {
        let Role::Wallet(wallet) = self else {
            return self.server().map_or_else(Ticks::default, NodeServer::ticks);
        };
        let wallet = wallet.schedule().map(|maintenance| {
            let core = Arc::clone(core);
            Arc::new(move |down| maintenance.tick(core.adjusted_time(), down)) as Tick
        });
        Ticks {
            server: None,
            wallet,
        }
    }

    /// The masternode's server, for `getcoinjoininfo` and the scheduler.
    pub fn server(&self) -> Option<&NodeServer<Server>> {
        match self {
            Role::Server(server) => Some(server),
            Role::Wallet(_) => None,
        }
    }
}

/// The scheduler's and `getcoinjoininfo`'s handle on the installed server. It has no raw
/// [`SharedServer`] to tick, so every tick relays its queues outside the server's lock.
pub struct NodeServer<T>(SharedServer<T>);

impl<T> Clone for NodeServer<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[cfg(test)]
impl<T> NodeServer<T> {
    pub(crate) fn new(server: T) -> Self {
        Self(SharedServer::new(server))
    }

    pub(crate) fn shared(&self) -> &SharedServer<T> {
        &self.0
    }
}

impl<C, M, A, P, S, Cl, R, Meta> NodeServer<CoinJoinServer<C, M, A, P, S, Cl, R, Meta>>
where
    C: ChainView + Send + 'static,
    M: MnLists + Send + 'static,
    A: ActiveMn + Send + 'static,
    P: Mempool + Send + 'static,
    S: PeerSink + Send + 'static,
    Cl: ServerClock + Send + 'static,
    R: RandomSource + Send + 'static,
    Meta: MixingMeta + Send + 'static,
{
    /// `getcoinjoininfo`'s server half.
    pub fn info(&self) -> ServerInfo {
        self.0.info()
    }

    /// The scheduler's tick. `msghand` holds a peer's state while it waits for the server's
    /// lock, so the queues the tick relays go out once the lock is released (W4-r1 m3).
    // Core: coinjoin/server.cpp:1421-1433
    pub fn tick(&self, shutdown_requested: bool) {
        NodePeerSink::outside_server(|| self.0.tick(shutdown_requested));
    }

    /// The server's [`Ticks`].
    pub fn ticks(&self) -> Ticks {
        let server = self.clone();
        Ticks {
            server: Some(Arc::new(move |down| server.tick(down))),
            wallet: None,
        }
    }
}

/// A 1 s task, given whether shutdown was requested.
pub type Tick = Arc<dyn Fn(bool) + Send + Sync>;

/// Which 1 s task, in [`Ticks::around`]'s order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Second {
    CoinJoinServer,
    SyncManager,
    CoinJoinWallet,
}

/// The role's 1 s tasks: the server's, which `ScheduleHandlers` schedules before SyncManager's
/// (handler order), and `cj_walletman->Schedule`'s queue maintenance, after.
#[derive(Clone, Default)]
pub struct Ticks {
    pub server: Option<Tick>,
    pub wallet: Option<Tick>,
}

impl Ticks {
    /// The 1 s tasks in Core's order, `sync` being SyncManager's. The wallet's 1 min
    /// `CheckPendingObservations` is left out: with no wallets it does nothing.
    // Core: init.cpp:2366, :2372-2373; coinjoin/walletman.cpp:121-137
    pub fn around(&self, sync: Tick) -> Vec<(Second, Tick)> {
        let server = self.server.iter().map(|t| (Second::CoinJoinServer, Arc::clone(t)));
        let wallet = self.wallet.iter().map(|t| (Second::CoinJoinWallet, Arc::clone(t)));
        server.chain([(Second::SyncManager, sync)]).chain(wallet).collect()
    }

    /// Schedules [`Ticks::around`] every second.
    pub fn schedule(&self, scheduler: &Scheduler, shutdown: &Arc<ShutdownState>, sync: Tick) {
        for (_, task) in self.around(sync) {
            let down = Arc::clone(shutdown);
            let every = move || task(down.shutdown_requested());
            scheduler.schedule_every(every, Duration::from_secs(1));
        }
    }
}
