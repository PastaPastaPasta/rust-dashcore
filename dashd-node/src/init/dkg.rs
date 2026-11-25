//! DK1-L3: the DKG runtime composed into the node (PLAN-DK1 §3c, §4 DK1-L3). The adapters the
//! runtime reaches the node through, and what step 7c builds from them: on an active masternode
//! the sessions, the phase threads and the debug manager; with `-watchquorums` alone the
//! observer's sessions, which start no thread; otherwise nothing.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dash_consensus::BlockId;
use dash_consensus::deployments::dip0003_enforced;
use dash_dml::Dmn;
use dash_evo::llmq::{LlmqCommitment, QuorumMembers, cycle_base_height};
use dash_evo::spork::SporkId;
use dash_evo::types::ProTxHash;
use dash_kv::{AbortSink, GlobalFailStop};
use dash_llmq::dkg::conn::{ConnFacts, ConnSnapshot};
use dash_p2p::NetCore;
use dash_p2p_core::{InvType, Inventory};
use dash_params::{BuriedDeployment, ChainSetup, LlmqParams, LlmqType};
use dash_primitives::{BlockHash, QuorumHash};
use dashd_chainstate::chain::{ChainHandle, ValidationInterface, ValidationSignals};
use dashd_masternode::active::ActiveMasternode;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::mnconn::QuorumNodes;
use dashd_masternode::mnconn::sporks::{all_members_connected, quorum_pose_enabled};
use dashd_masternode::sporks::SporkManager;
use dashd_p2p::processing::peerman::Shared;
use dashd_quorums::dkg::intake::DkgIntake;
use dashd_quorums::dkg::seams::{
    ConnFactsSource, ContributionsCache, DkgChain, DkgConnections, DkgStore, FinalVerifier,
    MembersAccess, MineableSink, OperatorKey, RelayTopology, Sporks, TipFacts, TipRef,
};
use dashd_quorums::dkg::{
    DebugJsonSource, DebugManager, DkgFailStop, DkgPorts, DkgRuntime, DkgSessions, MnOperatorKey,
    PhaseClock, RoundSeams, SessionKey, SessionSeams, SimRates, TICK,
};
use dashd_quorums::tip::TipBarrier;
use dashd_quorums::{ChainQuorums, DKGDB_SCHEMA, DkgDb, LocalMasternode};
use dashd_rpc::UniValue;
use dashd_store::RocksEngine;
use serde_json::Value;

use crate::InitFailure;
use crate::init::quorums::DkgManagers;
use crate::p2p::active_mn::ActiveDuties;
use crate::p2p::net_dkg::{Installed, NetDkgSlot, NodeIntakeHost};
use crate::rpc::evo::DkgStatusSource;
use crate::scheduler::Scheduler;

/// The dkgdb's directory under the network data directory. DK1-db's `dkgdb_path` names Core's
/// `llmq/dkgdb`, but rdashd refuses a data directory holding `llmq/` as Core's
/// (`datadir::probe_core_data_dir`), so the dkgdb sits beside the quorum database instead.
pub const DKGDB_DIR: &str = "dkgdb";

/// The dkgdb this node opens.
pub type NodeDkgDb = DkgDb<RocksEngine, ChainQuorums>;

/// Core's `GetOptions(1 << 20)` for the dkgdb, as the quorum database's.
// Core: v24.0.0 src/dbwrapper.cpp:113-119, src/llmq/dkgsessionmgr.cpp:35
fn engine_config() -> dashd_store::RocksConfig {
    super::quorums::engine_config()
}

/// The DKG reads of the chain (§3c `DkgChain`): the published block index and the chain
/// parameters. Every read is a lookup on the index reader, never a V turn, so a tip update on the
/// active masternode's `am1-tip` worker, the one thread that delivers tips, never waits on V.
pub struct RouteBChain {
    pub chain: ChainHandle,
    pub params: Arc<ChainSetup>,
}

impl RouteBChain {
    fn buried_height(&self, dep: BuriedDeployment) -> i64 {
        i64::from(self.params.consensus.deployment_height(dep).to_u32())
    }
}

impl DkgChain for RouteBChain {
    fn snapshot_unvalidated(&self) -> bool {
        false
    }

    // Core: v24.0.0 src/deploymentstatus.h:65-68 DeploymentDIP0003Enforced
    fn dip3_enforced(&self, height: i32) -> bool {
        dip0003_enforced(height, &self.params)
    }

    // Core: v24.0.0 src/node/blockstorage.cpp:39-44 BlockManager::LookupBlockIndex
    fn lookup(&self, hash: &QuorumHash) -> Option<TipRef> {
        let reader = self.chain.reader();
        let entry = reader.entry(reader.lookup(&BlockHash::from(*hash.as_bytes()))?)?;
        Some(TipRef {
            height: entry.height as i32,
            hash: entry.hash,
        })
    }

    /// The ancestor through the index's links from the tip's own entry, found by its hash.
    ///
    /// # Panics
    ///
    /// When the tip or its ancestor is not in the index: the handler asks only for heights in
    /// `0..=tip.height` of a tip it was given.
    // Core: v24.0.0 src/chain.cpp:85-107 CBlockIndex::GetAncestor
    fn ancestor_hash(&self, tip: &TipFacts, height: i32) -> BlockHash {
        let reader = self.chain.reader();
        reader
            .lookup(&tip.hash)
            .and_then(|id| reader.ancestor(id, u32::try_from(height).ok()?))
            .and_then(|id| reader.entry(id))
            .map(|e| e.hash)
            .unwrap_or_else(|| panic!("no ancestor at {height} of the DKG tip {}", tip.hash))
    }

    // Core: v24.0.0 src/llmq/options.cpp:50-62 IsQuorumRotationEnabled
    fn rotation_enabled(&self, params: &LlmqParams, block: &TipRef) -> bool {
        if !params.use_rotation {
            return false;
        }
        let Ok(height) = u32::try_from(block.height) else {
            return false;
        };
        let cycle_base = cycle_base_height(params, height);
        cycle_base >= 1 && i64::from(cycle_base) >= self.buried_height(BuriedDeployment::Dip0024)
    }

    // Core: v24.0.0 src/deploymentstatus.h:14-18 DeploymentActiveAfter
    fn v19_after(&self, block: &TipRef) -> bool {
        i64::from(block.height) + 1 >= self.buried_height(BuriedDeployment::V19)
    }

    fn current_tip(&self) -> Option<TipFacts> {
        let snap = self.chain.reader().snapshot();
        snap.has_tip().then(|| TipRef {
            height: snap.tip.tip_height as i32,
            hash: snap.tip.tip_hash,
        })
    }
}

/// One `GetAllQuorumMembers(type, base, reset = false)`.
pub trait MembersRead: Send + Sync {
    /// The members of `t`'s quorum at the block `base`; `Err` with the read's text when the
    /// block is not indexed or the read fails, `Ok(None)` when V no longer runs.
    fn members(
        &self,
        t: LlmqType,
        base: &BlockHash,
    ) -> Result<Option<QuorumMembers<Arc<Dmn>>>, String>;
}

/// Route B: the lookup and the members in one `OnV` turn.
impl MembersRead for ChainHandle {
    fn members(
        &self,
        t: LlmqType,
        base: &BlockHash,
    ) -> Result<Option<QuorumMembers<Arc<Dmn>>>, String> {
        let base = *base;
        match self
            .on_v(move |on_v, q| {
                let id =
                    q.chain.reader().lookup(&base).ok_or_else(|| format!("{base} not indexed"))?;
                q.bind(on_v).members(t, id, false).map_err(|e| e.to_string())
            })
            .recv()
        {
            Ok(read) => read.map(Some),
            Err(_) => Ok(None),
        }
    }
}

/// Access #1 (§3c `MembersAccess`): one CH7b facade access per call.
pub struct FacadeMembers<R>(pub R);

impl<R: MembersRead> MembersAccess for FacadeMembers<R> {
    /// The members; none once V has stopped, which happens only after the phase threads are
    /// joined.
    ///
    /// # Panics
    ///
    /// On a failed read, Core's exception out of `GetAllQuorumMembers`: the phase thread's
    /// fault.
    // Core: v24.0.0 src/llmq/dkgsession.cpp:92 CDKGSession::Init
    fn all_members(&self, t: LlmqType, base: &TipRef) -> QuorumMembers<Arc<Dmn>> {
        match self.0.members(t, &base.hash) {
            Ok(Some(members)) => members,
            Ok(None) => Arc::from([]),
            Err(e) => panic!("GetAllQuorumMembers({t}, {}) -- {e}", base.hash),
        }
    }
}

/// Access #2: `GetQuorumRelayMembers(params, base, me, onlyOutbound = true)`.
struct NodeRelayTopology<'a> {
    chain: &'a dyn DkgChain,
    members: &'a dyn MembersAccess,
}

impl RelayTopology for NodeRelayTopology<'_> {
    // Core: v24.0.0 src/llmq/utils.cpp:778-831 GetQuorumRelayMembers
    fn relay_members(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        me: &ProTxHash,
    ) -> BTreeSet<ProTxHash> {
        let Some(base) = self.chain.lookup(base) else {
            return BTreeSet::new();
        };
        let members: Vec<ProTxHash> =
            self.members.all_members(t, &base).iter().map(|m| m.pro_tx_hash).collect();
        dash_llmq::topology::quorum_relay_members(&members, me, true)
    }
}

/// Accesses #5 and #6: route B's `verify_commitment`, signatures checked synchronously.
struct NodeVerifier<'a> {
    chain: &'a ChainHandle,
}

impl FinalVerifier for NodeVerifier<'_> {
    /// # Panics
    ///
    /// On a failed member read, as [`FacadeMembers`].
    // Core: v24.0.0 src/llmq/dkgsession.cpp:1239-1300 CDKGSession::FinalizeCommitments
    fn verify(&mut self, base: &QuorumHash, qc: &LlmqCommitment) -> bool {
        let (base, qc) = (BlockHash::from(*base.as_bytes()), qc.clone());
        let read = self
            .chain
            .on_v(move |on_v, q| {
                let id =
                    q.chain.reader().lookup(&base).ok_or_else(|| format!("{base} not indexed"))?;
                q.bind(on_v).verify_commitment(&qc, id).map_err(|e| e.to_string())
            })
            .recv();
        match read {
            Ok(Ok(valid)) => valid,
            Ok(Err(e)) => panic!("CFinalCommitment::Verify -- {e}"),
            Err(_) => false,
        }
    }
}

/// The session's per-call environment objects (`DkgPorts`).
struct NodePorts {
    chain: Arc<dyn DkgChain>,
    members: Arc<dyn MembersAccess>,
    handle: ChainHandle,
    db: Arc<NodeDkgDb>,
    key: MnOperatorKey,
}

impl DkgPorts for NodePorts {
    fn topology(&self) -> Box<dyn RelayTopology + '_> {
        Box::new(NodeRelayTopology {
            chain: self.chain.as_ref(),
            members: self.members.as_ref(),
        })
    }

    fn store(&self) -> Box<dyn DkgStore + '_> {
        Box::new(&*self.db)
    }

    fn verifier(&self) -> Box<dyn FinalVerifier + '_> {
        Box::new(NodeVerifier {
            chain: &self.handle,
        })
    }

    fn operator_key(&self) -> &dyn OperatorKey {
        &self.key
    }
}

/// One fully connected peer as `ForEachNode` sees it.
pub trait PeerTable: Send + Sync {
    /// The connection facts of the peers `fSuccessfullyConnected && !fDisconnect`, in
    /// `m_nodes` order.
    fn fully_connected(&self) -> Vec<ConnFacts>;
}

impl PeerTable for NetCore {
    // Core: v24.0.0 src/net.h:1353-1359 CConnman::FullyConnectedOnly, :1405-1411 ForEachNode
    fn fully_connected(&self) -> Vec<ConnFacts> {
        self.peers()
            .iter()
            .filter(|p| p.successfully_connected() && !p.disconnect_requested())
            .map(|p| ConnFacts {
                verified_pro_tx: ProTxHash::from(p.verified_proreg_hash()),
                version: p.version(),
                masternode_connection: p.masternode_connection(),
                relay_txs: p.version_relay_txs(),
                services: p.version_services().get(),
            })
            .collect()
    }
}

/// The masternode metadata `VerifyConnectionAndMinProtoVersions` reads.
pub trait MetaFacts: Send + Sync {
    /// `OutboundFailedTooManyTimes(proTxHash)`.
    fn outbound_failed_too_many(&self, pro_tx: &ProTxHash) -> bool;
    /// `IsPlatformBanned(proTxHash)`.
    fn platform_banned(&self, pro_tx: &ProTxHash) -> bool;
}

impl MetaFacts for MasternodeMetaMan {
    fn outbound_failed_too_many(&self, pro_tx: &ProTxHash) -> bool {
        self.outbound_failed_too_many_times(pro_tx)
    }

    fn platform_banned(&self, pro_tx: &ProTxHash) -> bool {
        self.is_platform_banned(pro_tx)
    }
}

/// The connection snapshot (§3c `ConnFactsSource`): sporks 21 and 23, the peer table's fully
/// connected peers in table order, and the members' metadata.
pub struct PeerConnFacts {
    pub peers: Arc<dyn PeerTable>,
    pub sporks: Arc<SporkManager>,
    pub meta: Arc<dyn MetaFacts>,
}

impl ConnFactsSource for PeerConnFacts {
    // Core: v24.0.0 src/active/dkgsession.cpp:255-290 VerifyConnectionAndMinProtoVersions
    fn snapshot(&self, t: LlmqType, members: &[ProTxHash]) -> ConnSnapshot {
        if !quorum_pose_enabled(t, &self.sporks) {
            return ConnSnapshot::default();
        }
        let pick =
            |f: &dyn Fn(&ProTxHash) -> bool| members.iter().copied().filter(|m| f(m)).collect();
        ConnSnapshot {
            pose_enabled: true,
            nodes: self.peers.fully_connected(),
            all_members_connected: all_members_connected(t, &self.sporks),
            outbound_failed_too_many: pick(&|m| self.meta.outbound_failed_too_many(m)),
            platform_banned: pick(&|m| self.meta.platform_banned(m)),
        }
    }
}

/// The sporks the DKG reads.
pub struct NodeSporks(pub Arc<SporkManager>);

impl Sporks for NodeSporks {
    // Core: v24.0.0 src/llmq/options.cpp:21-24 IsQuorumDKGEnabled
    fn quorum_dkg_enabled(&self) -> bool {
        self.0.is_active(SporkId::SPORK_17_QUORUM_DKG_ENABLED)
    }

    fn spork21_value(&self) -> i64 {
        self.0.value(SporkId::SPORK_21_QUORUM_ALL_CONNECTED)
    }

    fn spork23_value(&self) -> i64 {
        self.0.value(SporkId::SPORK_23_QUORUM_POSE)
    }
}

/// The phase threads' clock: a steady origin, 100 ms sleeps, and the chain's spacing.
pub struct NodePhaseClock {
    origin: Instant,
    pow_spacing_s: i64,
    mine_on_demand: bool,
}

impl NodePhaseClock {
    pub fn new(params: &ChainSetup) -> Self {
        Self {
            origin: Instant::now(),
            pow_spacing_s: params.consensus.pow_target_spacing,
            mine_on_demand: params.consensus.pow_no_retargeting,
        }
    }
}

impl PhaseClock for NodePhaseClock {
    fn steady_now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn sleep_tick(&self) {
        std::thread::sleep(TICK);
    }

    fn pow_spacing_s(&self) -> i64 {
        self.pow_spacing_s
    }

    // Core: v24.0.0 src/chainparams.h:124 MineBlocksOnDemand
    fn mine_on_demand(&self) -> bool {
        self.mine_on_demand
    }
}

/// Where an added commitment's inv goes.
pub trait InvRelay: Send + Sync {
    /// `PeerRelayInv(inv)`.
    fn relay_inv(&self, inv: &Inventory);
}

impl InvRelay for Shared {
    fn relay_inv(&self, inv: &Inventory) {
        Shared::relay_inv(self, inv);
    }
}

/// One route-B `AddMineableCommitment`.
pub trait MineableAdd: Send + Sync {
    /// The commitment's hash when the pool took it.
    fn add(&self, qc: &LlmqCommitment) -> Option<[u8; 32]>;
}

impl MineableAdd for ChainHandle {
    fn add(&self, qc: &LlmqCommitment) -> Option<[u8; 32]> {
        let qc = qc.clone();
        self.on_v(move |on_v, q| q.bind(on_v).add_mineable_commitment(&qc)).recv().ok().flatten()
    }
}

/// The mineable pool and the relay of an added commitment's inv (§3c `MineableSink`).
pub struct NodeMineable {
    pub pool: Arc<dyn MineableAdd>,
    pub relay: Arc<dyn InvRelay>,
}

impl MineableSink for NodeMineable {
    // Core: v24.0.0 src/llmq/blockprocessor.cpp:840-867 CQuorumBlockProcessor::AddMineableCommitment
    fn add(&self, qc: &LlmqCommitment) -> Option<[u8; 32]> {
        self.pool.add(qc)
    }

    // Core: v24.0.0 src/llmq/net_dkg.cpp:869-873, 942-947 NetDKG::HandleDKGRound
    fn relay_inv(&self, hash: &[u8; 32]) {
        self.relay.relay_inv(&Inventory::new(InvType::QuorumFinalCommitment, (*hash).into()));
    }
}

/// DK1-db's 60 s contributions cache, cleaned on every tip.
struct DbCache(Arc<NodeDkgDb>);

impl ContributionsCache for DbCache {
    fn cleanup_cache(&self) {
        self.0.cleanup_cache();
    }
}

/// `m_mn_activeman`'s `GetProTxHash`.
struct NodeActiveMn(Arc<ActiveMasternode>);

impl LocalMasternode for NodeActiveMn {
    // Core: v24.0.0 src/active/masternode.h:68 CActiveMasternodeManager::GetProTxHash
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.0.pro_tx_hash().map(ProTxHash::from)
    }
}

/// A phase thread's fault: the banner is printed; Core's `TraceThread` rethrows into
/// `std::terminate`.
// Core: v24.0.0 src/util/thread.cpp:16-27 util::TraceThread
struct Terminate;

impl DkgFailStop for Terminate {
    fn fail_stop(&self, _: SessionKey, _: &str) {
        std::process::abort();
    }
}

/// What `CDKGDebugManager::ToJson` reads outside its status.
struct NodeDebugSource {
    params: Arc<ChainSetup>,
    members: Arc<dyn MembersRead>,
}

impl DebugJsonSource for NodeDebugSource {
    fn has_llmq(&self, t: LlmqType) -> bool {
        self.params.consensus.llmq(t).is_some()
    }

    // Core: v24.0.0 src/llmq/debug.cpp:28-33 CDKGDebugSessionStatus::ToJson
    fn members(&self, t: LlmqType, quorum_hash: &QuorumHash) -> Option<Vec<ProTxHash>> {
        let read = self.members.members(t, &BlockHash::from(*quorum_hash.as_bytes()));
        Some(read.ok()??.iter().map(|m| m.pro_tx_hash).collect())
    }

    fn format_time(&self, time: i64) -> String {
        dash_clock::format_iso8601_date_time(time)
    }
}

/// `session[]` as CH5's `mine_quorum()` reads it: each entry's `llmqType`, and its status's
/// `quorumHash`, `phase` and the four received counts (or lists, above detail level 0).
fn listed_sessions(json: &Value) -> UniValue {
    const STATUS: [&str; 6] = [
        "quorumHash",
        "phase",
        "receivedContributions",
        "receivedComplaints",
        "receivedJustifications",
        "receivedPrematureCommitments",
    ];
    let sessions = json.get("session").and_then(Value::as_array).cloned().unwrap_or_default();
    UniValue::Arr(
        sessions
            .iter()
            .map(|s| {
                let mut obj = UniValue::new_object();
                if let Some(t) = s.get("llmqType") {
                    obj.push_kv_end("llmqType", univalue(t));
                }
                let mut status = UniValue::new_object();
                for key in STATUS {
                    if let Some(v) = s.get("status").and_then(|st| st.get(key)) {
                        status.push_kv_end(key, univalue(v));
                    }
                }
                obj.push_kv_end("status", status);
                obj
            })
            .collect(),
    )
}

fn univalue(v: &Value) -> UniValue {
    match v {
        Value::Null => UniValue::Null,
        Value::Bool(b) => UniValue::Bool(*b),
        Value::Number(n) => UniValue::Num(n.to_string()),
        Value::String(s) => UniValue::Str(s.clone()),
        Value::Array(a) => UniValue::Arr(a.iter().map(univalue).collect()),
        Value::Object(o) => {
            UniValue::Obj(o.iter().map(|(k, v)| (k.clone(), univalue(v))).collect())
        }
    }
}

/// What `quorum dkgstatus` reads of the DKG and the network.
struct NodeDkgStatus {
    debug: Arc<DebugManager>,
    src: NodeDebugSource,
    active: Option<Arc<ActiveMasternode>>,
    peers: Arc<dyn PeerTable>,
    sporks: Arc<SporkManager>,
}

impl DkgStatusSource for NodeDkgStatus {
    // Core: v24.0.0 src/rpc/quorums.cpp:362-366
    fn sessions(&self, detail: i32) -> UniValue {
        listed_sessions(&self.debug.to_json(detail, &self.src))
    }

    // Core: v24.0.0 src/rpc/quorums.cpp:372
    fn active_pro_tx(&self) -> Option<[u8; 32]> {
        self.active.as_ref().map(|a| a.pro_tx_hash().unwrap_or([0; 32]))
    }

    // Core: v24.0.0 src/rpc/quorums.cpp:402-407
    fn connected_pro_txs(&self) -> Vec<[u8; 32]> {
        self.peers
            .fully_connected()
            .into_iter()
            .map(|p| *p.verified_pro_tx.as_bytes())
            .filter(|h| *h != [0; 32])
            .collect()
    }

    fn all_members_connected(&self, t: LlmqType) -> bool {
        all_members_connected(t, &self.sporks)
    }
}

/// `ActiveContext::UpdatedBlockTip`'s last call, `qdkgsman->UpdatedBlockTip`, and
/// `ObserverContext::UpdatedBlockTip`'s only one: the block's height and hash from the index.
struct SessionsTip {
    sessions: Arc<DkgSessions>,
    chain: ChainHandle,
}

impl ActiveDuties for SessionsTip {
    // Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:41-55 CDKGSessionManager::UpdatedBlockTip
    fn updated_block_tip(&self, new: BlockId, ibd: bool) {
        let Some(entry) = self.chain.reader().entry(new) else {
            return;
        };
        let tip = TipRef {
            height: entry.height as i32,
            hash: entry.hash,
        };
        self.sessions.updated_block_tip(&tip, ibd);
    }
}

/// The DKG's tip duties after `NetQuorumTip`'s worker (BL-945): each tip waits until `d-q-tip`
/// has finished every event posted by the time it reaches the tip, h's own check and any later
/// tip's included ([`TipBarrier`] says why that order is safe), so a stale `CheckQuorumConnections` never removes what this tip's
/// session ensures.
struct AfterQuorumTip {
    barrier: TipBarrier,
    duties: Arc<dyn ActiveDuties>,
}

impl ActiveDuties for AfterQuorumTip {
    fn updated_block_tip(&self, new: BlockId, ibd: bool) {
        self.barrier.wait_for_posted();
        self.duties.updated_block_tip(new, ibd);
    }
}

/// The observer's `CValidationInterface`: it posts each tip to its `dkg-tip` worker in order and
/// never waits.
// Core: v24.0.0 src/llmq/observer.cpp:22-33 ObserverContext
struct ObserverListener(Mutex<Sender<(BlockId, bool)>>);

impl ValidationInterface for ObserverListener {
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        if ibd || fork == Some(new) {
            return;
        }
        let _ = self.0.lock().unwrap_or_else(|p| p.into_inner()).send((new, ibd));
    }

    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        if let Some(tip) = tip {
            self.updated_block_tip(tip, None, ibd);
        }
    }
}

/// What step 7c passes to [`NodeDkg::build`].
pub struct DkgParts<'a> {
    pub params: Arc<ChainSetup>,
    pub net_dir: &'a Path,
    /// `fReindex || fReindexChainState` at the open.
    pub wipe: bool,
    pub watch: bool,
    pub active: Option<Arc<ActiveMasternode>>,
    pub chain: ChainHandle,
    pub signals: ValidationSignals,
    pub core: Arc<NetCore>,
    pub shared: Arc<Shared>,
    pub sporks: Arc<SporkManager>,
    pub metaman: Arc<MasternodeMetaMan>,
    pub connections: Arc<dyn DkgConnections>,
    /// The quorum registry `RelayInvToParticipants` reads.
    pub registry: Arc<QuorumNodes>,
    /// The masternode's `NetDKG`, registered at the processor's build, which gets the intake.
    pub net_dkg: Option<Arc<NetDkgSlot>>,
    /// The quorum manager's `ConnectManagers`, which gets the dkgdb and the masternode's role.
    pub managers: DkgManagers,
    pub abort: Arc<dyn AbortSink>,
    /// `NetQuorumTip`'s worker's barrier, which every DKG tip waits on.
    pub quorum_tip: TipBarrier,
}

/// The node's DKG: `ActiveContext`'s or `ObserverContext`'s `qdkgsman` and `dkgdbgman`, and on an
/// active masternode `NetDKG`'s phase threads.
pub struct NodeDkg {
    sessions: Arc<DkgSessions>,
    runtime: Option<Arc<DkgRuntime>>,
    db: Arc<NodeDkgDb>,
    duties: Arc<dyn ActiveDuties>,
    status: Arc<NodeDkgStatus>,
    observer: Option<(ValidationSignals, Arc<dyn ValidationInterface>, JoinHandle<()>)>,
}

impl NodeDkg {
    /// Opens the dkgdb and builds the sessions and the debug manager for an active masternode or,
    /// with `-watchquorums`, an observer; `None` for any other node. An observer's tip listener is
    /// registered on `signals` here; an active masternode's tips come through
    /// [`NodeDkg::duties`], which its `ActiveContext` listener calls last.
    ///
    /// # Errors
    ///
    /// The dkgdb's open or wipe failure, as Core's `CDBWrapper` throws it out of `AppInitMain`;
    /// a thread that cannot be spawned.
    // Core: v24.0.0 src/init.cpp:2155-2172, src/active/context.cpp:30-53, src/llmq/observer.cpp:11-18
    pub fn build(parts: DkgParts<'_>) -> Result<Option<NodeDkg>, InitFailure> {
        if parts.active.is_none() && !parts.watch {
            return Ok(None);
        }
        let path = parts.net_dir.join(DKGDB_DIR);
        let (engine, _) = RocksEngine::open(&path, &DKGDB_SCHEMA, &engine_config())
            .map_err(|e| InitFailure::Exception(e.to_string()))?;
        let quorums = ChainQuorums::new();
        quorums.bind(parts.chain.clone());
        let db = DkgDb::open(
            engine,
            parts.wipe,
            Arc::clone(&parts.params),
            quorums,
            GlobalFailStop::new(),
            Arc::clone(&parts.abort),
        )
        .map_err(|e| InitFailure::Exception(e.to_string()))?;
        let db = Arc::new(db);
        let llmqs: Vec<(LlmqType, LlmqParams)> =
            parts.params.consensus.llmq_params().map(|(t, p)| (t, *p)).collect();
        let chain: Arc<dyn DkgChain> = Arc::new(RouteBChain {
            chain: parts.chain.clone(),
            params: Arc::clone(&parts.params),
        });
        let seams = SessionSeams {
            chain: Arc::clone(&chain),
            sporks: Arc::new(NodeSporks(Arc::clone(&parts.sporks))),
            cache: Arc::new(DbCache(Arc::clone(&db))),
        };
        let core = Arc::clone(&parts.core);
        let debug =
            Arc::new(DebugManager::new(llmqs.iter().copied(), move || core.adjusted_time()));
        let members_read: Arc<dyn MembersRead> = Arc::new(parts.chain.clone());
        let peers: Arc<dyn PeerTable> = Arc::clone(&parts.core) as _;
        let status = Arc::new(NodeDkgStatus {
            debug: Arc::clone(&debug),
            src: NodeDebugSource {
                params: Arc::clone(&parts.params),
                members: Arc::clone(&members_read),
            },
            active: parts.active.clone(),
            peers: Arc::clone(&peers),
            sporks: Arc::clone(&parts.sporks),
        });
        let (sessions, runtime) = match &parts.active {
            Some(active) => {
                let sessions = Arc::new(DkgSessions::new(llmqs.iter().copied(), seams.clone()));
                let host = Arc::new(NodeIntakeHost {
                    core: Arc::clone(&parts.core),
                    shared: Arc::clone(&parts.shared),
                    registry: Arc::clone(&parts.registry),
                    chain: parts.chain.clone(),
                    qwatch: parts
                        .net_dkg
                        .as_ref()
                        .map(|s| Arc::clone(&s.qwatch))
                        .unwrap_or_default(),
                });
                let intake = Arc::new(DkgIntake::new(
                    Arc::clone(&sessions),
                    llmqs.iter().copied(),
                    seams,
                    Arc::clone(&host) as _,
                ));
                if let Some(slot) = &parts.net_dkg {
                    slot.install(Installed {
                        intake: Arc::clone(&intake),
                        host,
                    });
                }
                let members: Arc<dyn MembersAccess> = Arc::new(FacadeMembers(parts.chain.clone()));
                let round = RoundSeams {
                    chain: Arc::clone(&chain),
                    members: Arc::clone(&members),
                    active: Arc::new(NodeActiveMn(Arc::clone(active))),
                    ports: Arc::new(NodePorts {
                        chain,
                        members,
                        handle: parts.chain.clone(),
                        db: Arc::clone(&db),
                        key: MnOperatorKey(Arc::clone(active)),
                    }),
                    queues: Arc::clone(&intake) as _,
                    batches: intake,
                    connections: parts.connections,
                    conn_facts: Arc::new(PeerConnFacts {
                        peers,
                        sporks: Arc::clone(&parts.sporks),
                        meta: Arc::clone(&parts.metaman) as _,
                    }),
                    mineable: Arc::new(NodeMineable {
                        pool: Arc::new(parts.chain.clone()),
                        relay: Arc::clone(&parts.shared) as _,
                    }),
                    clock: Arc::new(NodePhaseClock::new(&parts.params)),
                    sim: Arc::new(SimRates::default()),
                    debug,
                    fail_stop: Arc::new(Terminate),
                    watch: parts.watch,
                };
                let runtime = Arc::new(DkgRuntime::new(Arc::clone(&sessions), round));
                (sessions, Some(runtime))
            }
            None => (Arc::new(DkgSessions::observer(llmqs.iter().copied(), seams)), None),
        };
        let duties: Arc<dyn ActiveDuties> = Arc::new(AfterQuorumTip {
            barrier: parts.quorum_tip.clone(),
            duties: Arc::new(SessionsTip {
                sessions: Arc::clone(&sessions),
                chain: parts.chain.clone(),
            }),
        });
        // Core: v24.0.0 src/llmq/net_dkg.cpp:475, :496 ConnectManagers
        parts.managers.connect(
            Arc::clone(&db) as _,
            parts.active.as_ref().map(|active| Arc::new(NodeActiveMn(Arc::clone(active))) as _),
        );
        let observer = match runtime {
            Some(_) => None,
            None => {
                let (tx, rx) = channel::<(BlockId, bool)>();
                let worker_duties = Arc::clone(&duties);
                let abort = Arc::clone(&parts.abort);
                let worker = std::thread::Builder::new()
                    .name("dkg-tip".into())
                    .spawn(move || observer_worker(rx, &*worker_duties, &*abort))
                    .map_err(|e| InitFailure::Exception(e.to_string()))?;
                let listener: Arc<dyn ValidationInterface> =
                    Arc::new(ObserverListener(Mutex::new(tx)));
                parts.signals.register(Arc::clone(&listener));
                Some((parts.signals.clone(), listener, worker))
            }
        };
        Ok(Some(NodeDkg {
            sessions,
            runtime,
            db,
            duties,
            status,
            observer,
        }))
    }

    /// The handlers and their lookups.
    pub fn sessions(&self) -> &Arc<DkgSessions> {
        &self.sessions
    }

    /// Whether the phase threads are this node's (an active masternode).
    pub fn has_runtime(&self) -> bool {
        self.runtime.is_some()
    }

    /// `qdkgsman->UpdatedBlockTip` for the active masternode's `ActiveContext` listener; `None`
    /// for an observer, whose own listener calls it.
    pub fn duties(&self) -> Option<Arc<dyn ActiveDuties>> {
        self.runtime.as_ref().map(|_| Arc::clone(&self.duties))
    }

    /// What `quorum dkgstatus` reads.
    pub fn status(&self) -> Arc<dyn DkgStatusSource> {
        Arc::clone(&self.status) as _
    }

    /// `NetDKG::Start` at `StartHandlers`: an active masternode's phase threads.
    ///
    /// # Errors
    ///
    /// A phase thread that cannot be spawned.
    // Core: v24.0.0 src/init.cpp:2360
    pub fn start(&self) -> Result<(), InitFailure> {
        match &self.runtime {
            Some(runtime) => runtime.start().map_err(|e| InitFailure::Exception(e.to_string())),
            None => Ok(()),
        }
    }

    /// On an active masternode, the hourly `CleanupOldContributions` after `ScheduleHandlers`.
    /// A failed cleanup has tripped the latch and been reported.
    // Core: v24.0.0 src/init.cpp:2367-2370
    pub fn schedule_cleanup(&self, scheduler: &Scheduler) {
        if self.runtime.is_none() {
            return;
        }
        let db = Arc::clone(&self.db);
        scheduler.schedule_every(
            move || {
                let _ = db.cleanup_old_contributions();
            },
            Duration::from_secs(60 * 60),
        );
    }

    /// `NetDKG::Interrupt` as the handler for [`crate::p2p::Net::on_interrupt_handlers`]
    /// (`PeerManager::InterruptHandlers`); `None` for an observer, which has no phase threads.
    // Core: v24.0.0 src/init.cpp:291-293
    pub fn interrupter(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let runtime = self.runtime.clone()?;
        Some(Arc::new(move || runtime.interrupt()))
    }

    /// `NetDKG::Stop` at `StopHandlers`, which joins the phase threads, and the observer's
    /// listener unregistered and its worker joined: all before V stops.
    // Core: v24.0.0 src/init.cpp:337, src/net_processing.cpp:1714-1722 StopHandlers
    pub fn stop(&mut self) {
        if let Some(runtime) = &self.runtime {
            runtime.stop();
        }
        if let Some((signals, listener, worker)) = self.observer.take() {
            signals.unregister(&listener);
            drop(listener);
            let _ = worker.join();
        }
    }
}

fn observer_worker(
    rx: std::sync::mpsc::Receiver<(BlockId, bool)>,
    duties: &dyn ActiveDuties,
    abort: &dyn AbortSink,
) {
    let mut panicked = false;
    for (new, ibd) in rx {
        if panicked {
            continue;
        }
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            duties.updated_block_tip(new, ibd)
        }));
        if let Err(p) = run {
            panicked = true;
            let what = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            abort.abort_node(&format!("a validation interface listener panicked: {what}"), None);
        }
    }
}

#[cfg(test)]
mod tests;
