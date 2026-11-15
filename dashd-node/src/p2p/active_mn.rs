//! The node's half of Core's `CActiveMasternodeManager`: the historical list lookup and the
//! network facts its `UpdatedBlockTip` and `Init` read.
// Core: active/masternode.cpp:112-276 at v24.0.0

use std::cell::{Cell, OnceCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use dash_consensus::BlockId;
use dash_consensus::deployments::dip0003_enforced;
use dash_dml::store::StoreError;
use dash_dml::{Dmn, MnList};
use dash_p2p::{NetAddr, NetCore, Service};
use dash_params::ChainSetup;
use dashd_chainstate::chain::{
    AbortSink, ChainHandle, ChainViewHandle, ValidationInterface, WaitError, no_wait_marked,
};
use dashd_masternode::active::key::OperatorPubKey;
use dashd_masternode::active::state::{
    self, ListError, LocalFacts, LogLine, MnLookup, PeerLocal, TipCtx,
};
use dashd_masternode::active::worker::{
    CheckedListForBlock, ListForBlock, TipEvent, TipListener, spawn_tip_worker,
};
use dashd_masternode::active::{ActiveMasternode, State};

use crate::chain::MnStartup;

/// `CDeterministicMNManager::GetListForBlock` as a chain command. Only an unmarked thread may
/// issue it ([`node_lists`]).
// Core: evo/deterministicmns.h:796-799; active/masternode.cpp:132, :184-185 at v24.0.0
pub struct NodeLists {
    v: ChainHandle,
}

impl ListForBlock for NodeLists {
    fn list_for_block(&self, id: BlockId) -> Result<Arc<MnList>, ListError> {
        list_reply(self.v.query(move |q| q.evo_view().list_for_block(id)).recv())
    }
}

/// [`NodeLists`] over `v`, refusing a caller that carries the no-wait marker.
pub fn node_lists(v: ChainHandle) -> CheckedListForBlock<NodeLists> {
    CheckedListForBlock::new(
        NodeLists {
            v,
        },
        no_wait_marked,
    )
}

/// Only `ListUnavailable` is Core's `BlockDataUnavailableError`; every other store error, and a
/// chain actor that stopped before it answered, is a fault.
// Core: active/masternode.cpp:184-194 at v24.0.0
fn list_reply(
    reply: Result<Result<Arc<MnList>, StoreError>, WaitError>,
) -> Result<Arc<MnList>, ListError> {
    match reply {
        Ok(list) => list.map_err(ListError::from),
        Err(stopped) => Err(ListError::Fault(StoreError::read(stopped.to_string()))),
    }
}

/// What `GetLocalAddress` and `Init` read from `CConnman` and the local-host table, read
/// before the manager's lock is taken and never under it.
pub struct LocalFactsSource {
    core: Arc<NetCore>,
    rt: tokio::runtime::Handle,
}

/// Where [`ActiveTip`] reads its [`LocalFacts`]: [`LocalFactsSource`] in the node.
pub trait CollectFacts: Send + Sync {
    /// The facts, with the inbound check toward the service `planned` names.
    fn collect_facts(&self, planned: &dyn Fn(&LocalFacts) -> Option<Service>) -> LocalFacts;
}

impl CollectFacts for LocalFactsSource {
    fn collect_facts(&self, planned: &dyn Fn(&LocalFacts) -> Option<Service>) -> LocalFacts {
        self.collect(planned)
    }
}

impl LocalFactsSource {
    /// `rt` runs the inbound check's connect; the caller is never one of its threads.
    pub fn new(core: Arc<NetCore>, rt: tokio::runtime::Handle) -> Self {
        Self {
            core,
            rt,
        }
    }

    /// The facts, with the inbound check made toward the service `planned` names: the one
    /// `Init` would check with these facts, if it gets that far.
    // Core: active/masternode.cpp:157-161 at v24.0.0
    pub fn collect(&self, planned: impl FnOnce(&LocalFacts) -> Option<Service>) -> LocalFacts {
        let mut facts = assemble(&self.core, toward_v4(&self.core), peer_locals(&self.core));
        if let Some(service) = planned(&facts) {
            facts.self_connect_ok =
                self.rt.block_on(self.core.connect_directly_selectable(&service));
        }
        facts
    }
}

/// `GetLocal` toward a dummy IPv4 peer.
// Core: active/masternode.cpp:237-240 at v24.0.0
fn toward_v4(core: &NetCore) -> Option<Service> {
    core.local_toward(&NetAddr::ipv4([8, 8, 8, 8]))
}

/// `ForEachNodeContinueIf(AllNodes)`: every peer in the table, in its order, with
/// `GetLocalAddress(*pnode)` for the IPv4 ones.
// Core: active/masternode.cpp:250-259 at v24.0.0
fn peer_locals(core: &NetCore) -> Vec<PeerLocal> {
    core.peers()
        .iter()
        .map(|p| {
            let is_ipv4 = p.addr().service.addr.is_ipv4();
            PeerLocal {
                is_ipv4,
                local_addr: is_ipv4.then(|| core.local_address(p)),
            }
        })
        .collect()
}

// Core: active/masternode.cpp:119, :241-246, :270-276 at v24.0.0
fn assemble(core: &NetCore, toward: Option<Service>, peers: Vec<PeerLocal>) -> LocalFacts {
    let cfg = core.config();
    let require_routable = cfg.params.require_routable_external_ip;
    let mut valid = Vec::new();
    for addr in toward.iter().chain(peers.iter().filter_map(|p| p.local_addr.as_ref())) {
        if !valid.contains(addr) && is_valid_net_addr(core, addr, require_routable) {
            valid.push(addr.clone());
        }
    }
    LocalFacts {
        listen: cfg.listen,
        require_routable,
        local_toward_v4: toward,
        loopback_listen: Service::new(NetAddr::ipv4([127, 0, 0, 1]), cfg.listen_port),
        peers,
        valid,
        self_connect_ok: false,
    }
}

// Core: active/masternode.cpp:270-276 at v24.0.0
fn is_valid_net_addr(core: &NetCore, addr: &Service, require_routable: bool) -> bool {
    let allow_private = core.config().allow_private_net;
    addr.addr.is_valid()
        && addr.addr.is_ipv4()
        && (!require_routable
            || (core.is_reachable(addr.addr.network(allow_private))
                && addr.addr.is_routable(allow_private)))
}

/// The published index's facts the tip worker and startup read: `pindexNew->nHeight`, `pprev`
/// and `ActiveTip()`.
pub trait TipEntries: Send + Sync {
    /// The entry's height and parent.
    fn entry(&self, id: BlockId) -> Option<(u32, Option<BlockId>)>;
    /// The active tip, once there is one.
    fn tip(&self) -> Option<BlockId>;
}

impl TipEntries for ChainViewHandle {
    fn entry(&self, id: BlockId) -> Option<(u32, Option<BlockId>)> {
        ChainViewHandle::entry(self, id).map(|e| (e.height, e.prev))
    }

    fn tip(&self) -> Option<BlockId> {
        let snap = self.snapshot();
        snap.has_tip().then_some(snap.tip.tip)
    }
}

/// Core's `CActiveMasternodeManager` with what its `Init` and `UpdatedBlockTip` read: the lists
/// as chain commands, the published index, and the network facts, each read before the
/// manager's lock is taken. A list fault is Core's unhandled exception: `AbortNode`.
pub struct ActiveTip {
    pub active: Arc<ActiveMasternode>,
    pub lists: Box<dyn ListForBlock>,
    pub entries: Box<dyn TipEntries>,
    pub params: Arc<ChainSetup>,
    pub local: Box<dyn CollectFacts>,
    pub abort: Arc<dyn AbortSink>,
}

impl ActiveTip {
    // Core: active/masternode.cpp:116, :119 at v24.0.0
    fn tip_ctx(&self, height: u32) -> TipCtx {
        TipCtx {
            height,
            dip3_enforced: dip0003_enforced(height as i32, &self.params),
            require_routable: self.params.require_routable_external_ip,
        }
    }

    /// `ActiveContext::UpdatedBlockTip` then `CActiveMasternodeManager::UpdatedBlockTip`, on
    /// the `am1-tip` worker.
    // Core: active/context.cpp:92-98; active/masternode.cpp:175-228 at v24.0.0
    pub fn updated_block_tip(&self, event: TipEvent) {
        if event.ibd || event.fork == Some(event.new) {
            return;
        }
        let Some((height, prev)) = self.entries.entry(event.new) else {
            return;
        };
        let tip = self.tip_ctx(height);
        if !tip.dip3_enforced {
            return;
        }
        // Core branches on `m_state` as read before the lock (:179-180). Not READY there is
        // `Init(pindexNew)` whatever the state is once the lock is taken (:222-226), so this arm
        // calls `init`, never `updated_block_tip`, which would branch on the state again.
        if self.active.snapshot().state != State::Ready {
            let list = InitList::new(&*self.lists, event.new);
            let (facts, read) = self.init_facts(&tip, &list);
            self.run_init(&tip, read, &facts, "UpdatedBlockTip");
            return;
        }
        // Core reads both lists first when READY (:183-185).
        let lists = self.lists_for(event.new, prev);
        let facts = self.local.collect_facts(&|facts| {
            let mut st = self.active.snapshot();
            let log = state::updated_block_tip(
                &mut st,
                &self.active.pubkey(),
                &tip,
                Ok(lookups(&lists).ok()?),
                &planning(facts),
            );
            checked_service(&log.ok()?)
        });
        self.tip_lists(&tip, lists, &facts);
    }

    fn tip_lists(
        &self,
        tip: &TipCtx,
        lists: Result<(Arc<MnList>, Arc<MnList>), ListError>,
        facts: &LocalFacts,
    ) {
        match self.active.updated_block_tip(tip, lookups(&lists), facts) {
            Ok(log) => emit(&log),
            Err(e) => self
                .abort
                .abort_node(&format!("CActiveMasternodeManager::UpdatedBlockTip -- {e}"), None),
        }
    }

    /// `Init` under the lock with what [`init_facts`](Self::init_facts) read; a list error is
    /// Core's unhandled exception from `GetListForBlock`, raised as `AbortNode` from `function`.
    // Core: active/masternode.cpp:132 at v24.0.0
    fn run_init(
        &self,
        tip: &TipCtx,
        read: Option<&Result<Arc<MnList>, ListError>>,
        facts: &LocalFacts,
        function: &str,
    ) {
        let log = match read {
            None => self.active.init(tip, &ListProbe::default(), facts),
            Some(Ok(list)) => self.active.init(tip, &**list, facts),
            Some(Err(e)) => {
                let e = match e.clone() {
                    ListError::Unavailable(block) => StoreError::ListUnavailable(block),
                    ListError::Fault(e) => e,
                };
                self.abort
                    .abort_node(&format!("CActiveMasternodeManager::{function} -- {e}"), None);
                return;
            }
        };
        emit(&log);
    }

    // Core: active/masternode.cpp:183-185 at v24.0.0
    fn lists_for(
        &self,
        new: BlockId,
        prev: Option<BlockId>,
    ) -> Result<(Arc<MnList>, Arc<MnList>), ListError> {
        let old = match prev {
            Some(prev) => Some(self.lists.list_for_block(prev)?),
            None => None,
        };
        let new = self.lists.list_for_block(new)?;
        Ok((old.unwrap_or_else(|| Arc::clone(&new)), new))
    }

    /// Whether `Init` with `facts` passes the listen and `GetLocalAddress` checks and reaches
    /// `GetListForBlock`. Neither check reads the state or the list.
    // Core: active/masternode.cpp:119-132 at v24.0.0
    fn reaches_list(&self, tip: &TipCtx, facts: &LocalFacts) -> bool {
        // `state::init` makes its first list call, `by_operator_key`, right after those two
        // checks (dashd-masternode active/state.rs:148-168), so the probe hearing it is the run
        // reaching `GetListForBlock`.
        let probe = ListProbe::default();
        state::init(&mut self.active.snapshot(), &self.active.pubkey(), tip, &probe, facts);
        probe.0.get()
    }

    /// `Init`'s facts, and its list if a run with them reads it: the inbound check is planned
    /// with the list, which is read only once the checks before it pass.
    // Core: active/masternode.cpp:119-161 at v24.0.0
    fn init_facts<'l>(
        &self,
        tip: &TipCtx,
        list: &'l InitList<'_>,
    ) -> (LocalFacts, Option<&'l Result<Arc<MnList>, ListError>>) {
        let facts = self.local.collect_facts(&|facts| {
            let plan = planning(facts);
            if !self.reaches_list(tip, &plan) {
                return None;
            }
            let list = list.get().as_ref().ok()?;
            let mut st = self.active.snapshot();
            checked_service(&state::init(&mut st, &self.active.pubkey(), tip, &**list, &plan))
        });
        let read = self.reaches_list(tip, &facts).then(|| list.get());
        (facts, read)
    }

    /// `nodeman->Init(ActiveTip())` on `loadblk`, before the deferred broadcast.
    // Core: init.cpp:2538-2539; active/masternode.cpp:112-173 at v24.0.0
    pub fn init(&self) {
        let Some(id) = self.entries.tip() else {
            return;
        };
        let Some((height, _)) = self.entries.entry(id) else {
            return;
        };
        let tip = self.tip_ctx(height);
        if !tip.dip3_enforced {
            return;
        }
        let list = InitList::new(&*self.lists, id);
        let (facts, read) = self.init_facts(&tip, &list);
        self.run_init(&tip, read, &facts, "Init");
    }
}

fn lookups(
    lists: &Result<(Arc<MnList>, Arc<MnList>), ListError>,
) -> Result<(&dyn MnLookup, &dyn MnLookup), ListError> {
    match lists {
        Ok((old, new)) => Ok((&**old as &dyn MnLookup, &**new as &dyn MnLookup)),
        Err(e) => Err(e.clone()),
    }
}

/// `GetListForBlock` for `Init`, read at most once and only when asked.
struct InitList<'a> {
    lists: &'a dyn ListForBlock,
    id: BlockId,
    read: OnceCell<Result<Arc<MnList>, ListError>>,
}

impl<'a> InitList<'a> {
    fn new(lists: &'a dyn ListForBlock, id: BlockId) -> Self {
        Self {
            lists,
            id,
            read: OnceCell::new(),
        }
    }

    fn get(&self) -> &Result<Arc<MnList>, ListError> {
        self.read.get_or_init(|| self.lists.list_for_block(self.id))
    }
}

/// An empty list that records whether `Init` looked itself up in it.
#[derive(Default)]
struct ListProbe(Cell<bool>);

impl MnLookup for ListProbe {
    fn by_operator_key(&self, _: &OperatorPubKey) -> Option<Arc<Dmn>> {
        self.0.set(true);
        None
    }

    fn is_valid(&self, _: &[u8; 32]) -> bool {
        false
    }

    fn is_pose_banned(&self, _: &[u8; 32]) -> bool {
        false
    }

    fn get(&self, _: &[u8; 32]) -> Option<Arc<Dmn>> {
        None
    }
}

/// The facts a dry run plans with: the inbound check counts as passed, so the run reaches it.
fn planning(facts: &LocalFacts) -> LocalFacts {
    LocalFacts {
        self_connect_ok: true,
        ..facts.clone()
    }
}

/// The service a run checked inbound, if it got that far.
// Core: active/masternode.cpp:157-160 at v24.0.0
fn checked_service(log: &[LogLine]) -> Option<Service> {
    log.iter().find_map(|line| match line {
        LogLine::CheckingInbound(service) => Some(service.clone()),
        _ => None,
    })
}

/// `LogPrintf`'s lines.
fn emit(log: &[LogLine]) {
    for line in log {
        tracing::info!("{}", line.render(describe).trim_end());
    }
}

/// `CDeterministicMN::ToString` up to its state's numeric fields.
// Core: evo/deterministicmns.cpp:52-55; evo/dmnstate.cpp:25-31 at v24.0.0
fn describe(dmn: &Dmn) -> String {
    format!(
        "CDeterministicMN(proTxHash={}, collateralOutpoint={}-{}, nOperatorReward={:.6}, state=CDeterministicMNState(nVersion={}, nRegisteredHeight={}, nLastPaidHeight={}, nPoSePenalty={}, nPoSeRevivedHeight={}, nPoSeBanHeight={}, nRevocationReason={})",
        dmn.pro_tx_hash,
        dmn.collateral.txid(),
        dmn.collateral.index(),
        f64::from(dmn.operator_reward) / 100.0,
        dmn.cold.version,
        dmn.cold.registered_height,
        dmn.hot.last_paid_height,
        dmn.hot.pose_penalty,
        dmn.hot.pose_revived_height,
        dmn.hot.banned_height(),
        dmn.cold.revocation_reason,
    )
}

/// `ActiveContext` on V's signals: it only posts the tip to the `am1-tip` worker, in order, and
/// never waits. Until `Init` has run it ignores startup's `InitializeCurrentBlockTip`, which
/// Core sends only to `CDSNotificationInterface` on a masternode.
// Core: active/context.cpp:88-98; init.cpp:2452-2466, :2538-2547 at v24.0.0
pub struct ActiveTipListener {
    posted: TipListener,
    armed: Arc<AtomicBool>,
}

impl ValidationInterface for ActiveTipListener {
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        let _ = self.posted.updated_block_tip(new, fork, ibd);
    }

    // Core: active/context.cpp:88-91 at v24.0.0
    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        if let Some(tip) = tip.filter(|_| self.armed.load(Ordering::SeqCst)) {
            let _ = self.posted.updated_block_tip(tip, None, ibd);
        }
    }
}

/// Startup's `Init`, then the listener hears the deferred broadcast.
struct ActiveStartup {
    tip: Arc<ActiveTip>,
    armed: Arc<AtomicBool>,
}

impl MnStartup for ActiveStartup {
    fn init(&self) {
        self.tip.init();
        self.armed.store(true, Ordering::SeqCst);
    }
}

/// The active masternode's wiring: its listener for V's signals, the `am1-tip` worker it posts
/// to, and the startup `Init` for `loadblk`.
pub struct ActiveWiring {
    pub listener: Arc<dyn ValidationInterface>,
    pub worker: JoinHandle<()>,
    pub startup: Box<dyn MnStartup>,
    #[cfg(test)]
    pub finished: Arc<WorkerExit>,
}

/// Spawns the `am1-tip` worker over `tip`, unmarked whatever the caller carries. The first panic
/// in a tip's work is `AbortNode`, with V's signals thread's text; the worker then processes no
/// more tips, as that thread stops, but drains its queue so posts never block and the join
/// returns.
// Core: active/masternode.cpp:175-228; validationinterface.cpp:193 at v24.0.0
pub fn wire(tip: ActiveTip) -> std::io::Result<ActiveWiring> {
    let tip = Arc::new(tip);
    let worker_tip = Arc::clone(&tip);
    #[cfg(test)]
    let finished = Arc::new(WorkerExit::default());
    #[cfg(test)]
    let guard = Finished(Arc::clone(&finished));
    let mut panicked = false;
    let (posted, worker) = spawn_tip_worker(move |event| {
        #[cfg(test)]
        let _ = &guard;
        if panicked {
            return;
        }
        let Err(p) = catch_unwind(AssertUnwindSafe(|| worker_tip.updated_block_tip(event))) else {
            return;
        };
        panicked = true;
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let what = listener_panic_message(p.as_ref());
            worker_tip
                .abort
                .abort_node(&format!("a validation interface listener panicked: {what}"), None);
        }));
    })?;
    let armed = Arc::new(AtomicBool::new(false));
    Ok(ActiveWiring {
        listener: Arc::new(ActiveTipListener {
            posted,
            armed: Arc::clone(&armed),
        }),
        worker,
        startup: Box::new(ActiveStartup {
            tip,
            armed,
        }),
        #[cfg(test)]
        finished,
    })
}

/// A panic payload's message as V's signals thread reports it (dashd-chainstate
/// chain/reply.rs:220): the text alone.
fn listener_panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

/// The worker's exit as the tests see it: `done` is set as its closure is dropped, `delay_ms`
/// after it ends, so that a caller that does not join sees it unset.
#[cfg(test)]
#[derive(Default)]
pub struct WorkerExit {
    pub done: AtomicBool,
    pub delay_ms: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
struct Finished(Arc<WorkerExit>);

#[cfg(test)]
impl Drop for Finished {
    fn drop(&mut self) {
        let delay = self.0.delay_ms.load(Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(delay));
        self.0.done.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;
