//! S3b: dashd v24.0.0's R1–R4 recordings (S0, `tests/fixtures/dashd-v24.0.0`, format in
//! `contrib/node/gv1b-transcripts/capture.py`) replayed in `dashd_p2p::sim` against the manager,
//! [`NetGovernance`] and its jobs, and SP1's sync tick over [`NodeGovernanceSync`] (GV1b plan §6
//! Replay). Each segment's outbound bytes per peer, each `check score`, each `check gobject` and
//! each recorded `mnsync` answer must equal dashd's.
//!
//! A crate test rather than `tests/transcripts.rs`: the checkpoints read the manager under the
//! governance lock, which only the crate reaches (`GovernanceHandle::lock`). `test-support`
//! exports the rig for dashd-node's R9, whose node is dashd-node's ([`NodeHost`]).
//!
//! What the replay supplies, as dashd's environment did:
//!
//! - the chain: the bootstrap's 179 blocks, and R4's `submitblock`s; the DMN-tip list is built
//!   from them by EV2's list builder, the transaction index holds their transactions, and each
//!   new block reaches governance as one `tip` command (the chain listener is S4's);
//! - the randomness (§6 Random fields): every vote-request bloom's tweak is the next `rand bloom`,
//!   through [`ReplayRng`]; each tick's object requests are compared per peer as sets, triggers
//!   first and then the others, by checking the hashes the node shuffles against the recorded
//!   set and then taking dashd's order, the per-tick cap asserted from the recorded masternode
//!   count;
//! - the scheduler: `mockscheduler` brings the governance jobs closer on their steady clock, the
//!   relay flush (every second of dashd's wall time) runs in each segment's quiet wait, as SP1's
//!   tick does; `time` sets the mock clock and `timeoffset` the adjusted offset `GovClock` reads.
//!
//! The v24.0.0→v24.0.2 differences the recordings show are named and counted in [`CoreDiffs`],
//! and the two places the recorder's timing decides a segment in [`Timing`].
// Core: governance/net_governance.cpp v24.0.2:39-303; node/sync_manager.cpp v24.0.2:51-311

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use dash_consensus::ChainAncestry as _;
use dash_consensus::primitives::{Block, decode_block, header_bytes};
use dash_dml::netinfo::NetInfoRules;
use dash_dml::testkit::replay::{CORE_NET_INFO, Headers, Members, mainnet, payee_rules};
use dash_dml::{
    DmlParams, DmlRules, MnList, NoCoins, NoTrace, PrevBlock, build_new_list_from_block,
};
use dash_governance::GovParams;
use dash_governance::manager::{Effects, GovClock, Manager};
use dash_governance::object::{self, Check, LoadedType};
use dash_governance::strings::hash_hex;
use dash_governance::trigger::{nearest_superblock_heights, payments_limit};
use dash_governance::vote;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, OutMsg, PeerId, Service};
use dash_p2p_core::Inventory;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::BlockHash;
use dashd_masternode::meta::GovHash;
use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
use dashd_masternode::sync::{BlockRef, MasternodeSync};
use dashd_masternode::sync_manager::{
    FullSyncNotifier, SyncConfig, SyncManager, SyncMsg, SyncNet, SyncPeer,
};
use dashd_masternode::sync_net::CoreSyncNet;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::peerman::Shared as PeerShared;
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::processing::seams::{ChainAccess, OverlayState, TreeRead};
use dashd_p2p::sim::chain::SimChain;
use dashd_p2p::sim::transcript::{Hooks, replay, unhex};
use dashd_p2p::sim::{Sim, VirtualClock};
use dashd_p2p::{HandlerCx, HandlerResult, NetHandler, Registry, Seams};
use serde_json::Value;

use crate::exec::{EffectsSink, GovChain, GovCommand, GovEnv, GovExec};
use crate::handle::GovernanceHandle;
use crate::helper::GovernanceBackend;
use crate::net::{GovPeers, GovSyncState, NetGovernance, RelayInvs};
use crate::rand::{Draw, GovRng, ReplayRng};
use crate::schedule::{GovScheduler, relay_interval};
use crate::sync::{DmnTip, NodeGovernanceSync};
use crate::test_support::FakeIndex;
use dashd_chainstate::connect::SuperblockSource as _;

mod superblocks;

#[cfg(feature = "test-support")]
pub use superblocks::{HostParts, HostProcess, NodeHost, r9_restart};

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dashd-v24.0.0");

/// The recordings' chain: `gov_capture.py`'s arguments (`PROVENANCE.md`, the bootstrap's
/// `source.args`).
fn chain_params() -> Arc<ChainSetup> {
    Arc::new(
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .arg(ChainOption::Dip3Params, "2:2")
            .arg(ChainOption::TestActivationHeight, "v20@100")
            .arg(ChainOption::TestActivationHeight, "mn_rr@100")
            .arg(ChainOption::BudgetParams, "10:10:10")
            .build()
            .expect("the recordings' chain"),
    )
}

fn fixture(name: &str) -> String {
    let path = PathBuf::from(DIR).join(format!("{name}.txt"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The frozen bootstrap: the blocks dashd held before each recording, and its masternodes.
struct Bootstrap {
    blocks: Vec<Vec<u8>>,
    masternodes: usize,
}

fn bootstrap() -> Bootstrap {
    let path = PathBuf::from(DIR).join("bootstrap.json.gz");
    let gz = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut text = String::new();
    flate2::read::GzDecoder::new(&gz[..]).read_to_string(&mut text).expect("gzip");
    let json: Value = serde_json::from_str(&text).expect("JSON");
    let blocks = json["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .map(|b| unhex(b.as_str().expect("block hex")))
        .collect();
    let masternodes = json["source"]["masternodes"].as_array().expect("masternodes").len();
    Bootstrap {
        blocks,
        masternodes,
    }
}

fn block(raw: &[u8]) -> Block {
    decode_block(&mut &raw[..]).expect("a block").into_value()
}

/// The DMN-tip list, built block by block as V's chainstate builds it.
struct Dml {
    params: Arc<ChainSetup>,
    dml: DmlParams,
    net_info: NetInfoRules,
    headers: Vec<[u8; 80]>,
    list: Arc<MnList>,
}

impl Dml {
    fn new(params: Arc<ChainSetup>, genesis: [u8; 80]) -> Self {
        Self {
            dml: DmlParams::new(&params),
            net_info: NetInfoRules::new(&params, mainnet(), CORE_NET_INFO),
            params,
            headers: vec![genesis],
            list: Arc::new(MnList::default_pre_dip3()),
        }
    }

    /// `ProcessSpecialTxsInBlock`'s list for `block`, the child of the last block.
    // Core: evo/specialtxman.cpp v24.0.2:309-822
    fn connect(&mut self, block: &Block) {
        let headers = Headers::new(0, &self.headers).expect("the headers");
        let tip = headers.tip();
        let prev = headers.header(tip);
        let height = i32::try_from(prev.height).expect("a height");
        assert_eq!(prev.hash, block.header().prev_hash(), "blocks in height order");
        if height + 1 >= self.dml.dip3_height {
            let buried = payee_rules(&self.params, height);
            let rules = DmlRules {
                params: &self.dml,
                net_info: &self.net_info,
                v19: buried.v19,
                mn_rr: buried.mn_rr,
                v24: false,
            };
            let members = Members::new(&headers, &[]).expect("no quorums");
            let at = PrevBlock {
                id: tip,
                hash: prev.hash,
                height,
                chain: &headers,
            };
            let built = build_new_list_from_block(
                block,
                at,
                &self.list,
                &NoCoins,
                &members,
                &rules,
                &mut Vec::new(),
                &mut NoTrace,
            )
            .unwrap_or_else(|e| panic!("block {}: {e}", height + 1));
            self.list = Arc::new(built.list.with_block_hash(block.hash()));
        }
        self.headers.push(header_bytes(block.header().raw()));
    }
}

/// The DMN-tip list the handler, the seam and the commands read.
struct TipList(Mutex<Arc<MnList>>);

impl TipList {
    fn set(&self, list: Arc<MnList>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = list;
    }
}

impl DmnTip for TipList {
    fn dmn_tip_list(&self) -> Arc<MnList> {
        Arc::clone(&self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// V's view for a governance command: the sim's active chain and the DMN-tip list.
struct SimView {
    chain: Arc<SimChain>,
    tip: Arc<TipList>,
}

impl GovChain for SimView {
    fn active_height(&self, block: &BlockHash) -> Option<u32> {
        let id = self.chain.lookup(block)?;
        let height = self.chain.entry(id)?.height;
        let tip = self.chain.snapshot().tip.tip;
        (self.chain.ancestor(tip, height) == Some(id)).then_some(height)
    }

    fn tip_height(&self) -> u32 {
        self.chain.snapshot().tip.tip_height
    }

    fn dmn_list(&self) -> Arc<MnList> {
        self.tip.dmn_tip_list()
    }
}

/// Runs each command on the poster's thread, one at a time, as V serves its lane.
struct SimExec(Mutex<SimView>);

impl GovExec for SimExec {
    fn post(&self, command: GovCommand) {
        let view = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        command(&*view);
    }
}

/// Where the commands run: on the sim's chain (R1–R4), or on V ([`superblocks`]).
enum Exec {
    Sim(SimExec),
    V(superblocks::VExec),
}

impl GovExec for Exec {
    fn post(&self, command: GovCommand) {
        match self {
            Exec::Sim(e) => e.post(command),
            Exec::V(e) => e.post(command),
        }
    }
}

/// The mock clock with the adjusted offset `timeoffset` gives, and mnsync's state.
struct Env {
    clock: Arc<VirtualClock>,
    sync: Arc<MasternodeSync>,
    offset: AtomicI64,
}

impl GovEnv for Env {
    fn clock(&self) -> GovClock {
        let now = dash_clock::Clock::node_now(&*self.clock).0;
        GovClock::new(now, now + self.offset.load(Ordering::SeqCst))
    }

    fn synced(&self) -> bool {
        self.sync.is_synced()
    }

    fn blockchain_synced(&self) -> bool {
        self.sync.is_blockchain_synced()
    }
}

/// N's effects worker: relays queued for the flush, sync bumps applied; the meta is not
/// observable here.
struct Worker {
    relay: Arc<RelayInvs>,
    sync: Arc<MasternodeSync>,
}

impl EffectsSink for Worker {
    fn submit(&self, effects: Effects) {
        self.relay.queue(&effects);
        crate::sync::apply_sync_bumps(&self.sync, &effects);
    }
}

/// The processor's sends, as dashd-node's implementation makes them (S5).
struct SimPeers {
    shared: Arc<PeerShared>,
    net: Arc<CoreSyncNet>,
}

impl GovPeers for SimPeers {
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

/// The node scheduler's steady time for the governance jobs: `mockscheduler` alone moves it.
/// The recordings' wall time between scheduler moves is a few seconds, far under the 5-minute
/// job's period, so it is left out.
#[derive(Default)]
struct Scheduler(Mutex<Vec<Job>>);

struct Job {
    run: Box<dyn Fn() + Send + Sync>,
    every: Duration,
    left: Duration,
}

impl GovScheduler for Scheduler {
    fn schedule_every(&self, run: Box<dyn Fn() + Send + Sync>, every: Duration) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(Job {
            run,
            every,
            left: every,
        });
    }
}

impl Scheduler {
    /// `MockForward(d)`: every job `d` closer; one that comes due runs once and is rescheduled a
    /// full period from now (Core's `Repeat`).
    // Core: scheduler.cpp v24.0.2:80-108
    fn mock_forward(&self, d: Duration) {
        let mut jobs = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut due: Vec<&mut Job> = Vec::new();
        for job in jobs.iter_mut() {
            match job.left.checked_sub(d) {
                Some(left) if !left.is_zero() => job.left = left,
                _ => due.push(job),
            }
        }
        due.sort_by_key(|j| j.left);
        for job in due {
            (job.run)();
            job.left = job.every;
        }
    }

    /// The jobs of period `every`, run now: the relay flush, within each quiet wait.
    fn run_every(&self, every: Duration) {
        for job in self.0.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            if job.every == every {
                (job.run)();
            }
        }
    }
}

/// The recorded order of one tick's object requests: Core shuffles the triggers and the others,
/// then takes from the back, so the hashes dashd asked for must be among those the node shuffles,
/// and go to the back in dashd's order. On regtest an object every peer was asked for within the
/// hour is still offered and skipped at each peer
/// ([`REGTEST_PEERS_PER_HASH`](dashd_masternode::sync_manager::REGTEST_PEERS_PER_HASH)); those
/// go to the front, each one the node asked every relaying peer for within the hour, and the
/// peers' outbound comparison holds the node to sending none of them.
// Core: node/sync_manager.cpp v24.0.2:60, :97-131; governance/governance.cpp v24.0.2:973-995
struct TickNet {
    inner: Arc<dyn SyncNet>,
    orders: Mutex<VecDeque<(&'static str, Vec<GovHash>)>>,
    clock: Arc<dyn dash_clock::Clock>,
    /// The tick's own vote requests: by object, each peer asked and when.
    asked: Mutex<BTreeMap<GovHash, BTreeMap<i64, i64>>>,
    /// The node's objects not pending deletion, triggers then the rest, as the seam's last
    /// fetch saw them ([`held_at_fetch`]).
    held: Option<Held>,
}

type Held = Arc<Mutex<Option<[BTreeSet<GovHash>; 2]>>>;

/// Records into `held`, under the seam's fetch lock, Core's regtest fetch from the objects'
/// states: it never skips an object for its asked peers (`nPeersPerHashMax` is `SIZE_MAX`
/// there). The tick takes no second lock to read it.
// Core: governance/governance.cpp v24.0.2:981-993; node/sync_manager.cpp v24.0.2:60
fn held_at_fetch(held: &Held) -> crate::sync::FetchProbe {
    let held = Arc::clone(held);
    Box::new(move |m: &Manager| {
        let mut out = [BTreeSet::new(), BTreeSet::new()];
        for (hash, state) in m.objects().iter().filter(|(_, s)| !s.cached_delete) {
            out[usize::from(state.obj_type != object::TRIGGER)].insert(*hash);
        }
        *held.lock().unwrap_or_else(PoisonError::into_inner) = Some(out);
    })
}

impl TickNet {
    fn now(&self) -> i64 {
        dash_clock::Clock::node_now(&*self.clock).secs()
    }

    /// Whether the node asked every relaying peer for `hash`'s votes within the hour:
    /// `mapAskedRecently`'s entries for it, none expired.
    ///
    /// Stricter than Core in two places, both failing closed: an entry is keyed by the peer's
    /// id where Core keys it by `pnode->addr`, so a reconnect from the same address counts as
    /// not asked; and an inbound peer counts on an active masternode, where Core skips it.
    // Core: node/sync_manager.cpp v24.0.2:64-65, :112, :115-119; governance/governance.cpp
    // v24.0.2:983-985
    fn asked_everywhere(&self, hash: &GovHash) -> bool {
        let now = self.now();
        let asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        let peers = asked.get(hash);
        self.inner.fully_connected().iter().filter(|p| p.can_relay).all(|p| {
            peers.and_then(|m| m.get(&p.id)).is_some_and(|at| at.saturating_add(ASKED_HOUR) >= now)
        })
    }
}

/// Core's `nTimeout` for a peer asked for an object's votes.
const ASKED_HOUR: i64 = 60 * 60;

impl SyncNet for TickNet {
    fn fully_connected(&self) -> Vec<SyncPeer> {
        self.inner.fully_connected()
    }

    fn all_addrs(&self) -> Vec<Service> {
        self.inner.all_addrs()
    }

    fn push(&self, peer: i64, msg: SyncMsg) {
        if let SyncMsg::GovSync {
            hash,
            ..
        } = &msg
            && *hash != [0; 32]
        {
            let mut asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
            asked.entry(*hash).or_default().insert(peer, self.now());
        }
        self.inner.push(peer, msg);
    }

    fn disconnect(&self, peer: i64) {
        self.inner.disconnect(peer);
    }

    fn requested_object_count(&self, peer: i64) -> usize {
        self.inner.requested_object_count(peer)
    }

    fn is_active_masternode(&self) -> bool {
        self.inner.is_active_masternode()
    }

    fn shuffle(&self, hashes: &mut [GovHash]) {
        let (group, want) = self
            .orders
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or(("unexpected", Vec::new()));
        let offered: BTreeSet<GovHash> = hashes.iter().copied().collect();
        assert_eq!(offered.len(), hashes.len(), "the tick's {group}: no repeats");
        if let Some(held) = &self.held {
            let at = ["triggers", "others"].iter().position(|g| *g == group);
            let at = at.unwrap_or_else(|| panic!("the tick's {group}: unexpected"));
            let held = held.lock().unwrap_or_else(PoisonError::into_inner);
            let want = &held.as_ref().expect("the seam's fetch recorded the held objects")[at];
            assert_eq!(&offered, want, "the tick's {group}: Core's fetch, the held objects");
        }
        let missing: Vec<String> =
            want.iter().filter(|h| !offered.contains(*h)).map(hash_hex).collect();
        assert!(missing.is_empty(), "the tick's {group}: dashd asked for {missing:?}");
        let rest: Vec<GovHash> = offered.iter().filter(|h| !want.contains(h)).copied().collect();
        let fresh: Vec<String> =
            rest.iter().filter(|h| !self.asked_everywhere(h)).map(hash_hex).collect();
        assert!(fresh.is_empty(), "the tick's {group}: offered {fresh:?}, not asked of every peer");
        for (slot, h) in hashes.iter_mut().zip(rest.iter().chain(want.iter().rev())) {
            *slot = *h;
        }
    }
}

/// The bloom tweaks, drawn in the recording's order: shared so the test can see what is left.
#[derive(Clone)]
struct Tweaks(Arc<Mutex<ReplayRng>>);

impl GovRng for Tweaks {
    fn bloom_tweak(&mut self) -> u32 {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).bloom_tweak()
    }

    fn sample(&mut self, v: &mut Vec<[u8; 32]>, k: usize) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).sample(v, k);
    }

    fn shuffle(&mut self, v: &mut [[u8; 32]]) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).shuffle(v);
    }
}

/// The handler and the overlay, registered before the sim exists and bound after; a restart
/// binds the next process's.
#[derive(Clone, Default)]
struct Late(Arc<OnceLock<Bound>>);

struct Bound {
    net: Mutex<Box<dyn NetHandler>>,
    seam: Mutex<Arc<dyn GovSyncState>>,
}

impl Late {
    fn net(&self) -> MutexGuard<'_, Box<dyn NetHandler>> {
        self.0.get().expect("bound").net.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bind(&self, net: Box<dyn NetHandler>, seam: Arc<dyn GovSyncState>) {
        let Some(b) = self.0.get() else {
            let bound = Bound {
                net: Mutex::new(net),
                seam: Mutex::new(seam),
            };
            assert!(self.0.set(bound).is_ok(), "bound once");
            return;
        };
        *b.net.lock().unwrap_or_else(PoisonError::into_inner) = net;
        *b.seam.lock().unwrap_or_else(PoisonError::into_inner) = seam;
    }
}

impl NetHandler for Late {
    fn commands(&self) -> &'static [CommandString] {
        &[CommandString::GOVSYNC, CommandString::GOVOBJ, CommandString::GOVOBJVOTE]
    }

    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::NetGovernance)
    }

    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        unreachable!("the registry calls process_message_result")
    }

    fn process_message_result(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<HandlerResult, Throw> {
        self.net().process_message_result(cx, cmd, payload)
    }

    fn already_have(&self, inv: &Inventory) -> bool {
        self.net().already_have(inv)
    }

    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        self.net().process_getdata(cx, inv)
    }
}

impl OverlayState for Late {
    fn governance_ready(&self) -> bool {
        self.0.get().is_some_and(|b| {
            let seam = b.seam.lock().unwrap_or_else(PoisonError::into_inner);
            seam.is_valid() && seam.is_blockchain_synced()
        })
    }

    fn instantsend_enabled(&self) -> bool {
        false
    }
}

/// A scratch directory for the netfulfilled file, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let dir = std::env::temp_dir()
            .join(format!("dashd-governance-s3b-{name}-{}", std::process::id()));
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).expect("scratch dir");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A recorded segment: what opened it and the vote requests dashd sent in it, per peer.
#[derive(Debug, Default)]
struct Segment {
    input: String,
    requests: BTreeMap<u8, Vec<GovHash>>,
    sends: usize,
    checks: usize,
    /// The tick runs in the next segment ([`Timing::tick_after_advance_time`]).
    tick_later: bool,
    /// The relay flush runs in the next segment ([`Timing::delivery_bursts`]).
    flush_later: bool,
    /// V's tip signals reach the peers in the next segment
    /// ([`superblocks::VCounts::announced_after_next_input`]).
    announce_later: bool,
}

impl Segment {
    fn is(&self, cmd: &str) -> bool {
        self.input.split(' ').next() == Some(cmd)
    }

    fn delivers(&self) -> bool {
        matches!(self.input.split(' ').nth(2), Some("govobj" | "govobjvote")) && self.is(">")
    }
}

/// The v24.0.0→v24.0.2 differences the recordings show, each counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CoreDiffs {
    /// dashpay/dash#8112: v24.0.0 relays the full-sync and vote-sync invs (`PeerRelayInv`), so
    /// they also reach the peers that did not ask; v24.0.2 pushes them to the requester alone
    /// (`net_governance.cpp` v24.0.2:150-167). The replay drops each recorded copy at another
    /// peer (`# declared-difference: R2-govsync-cross-peer`) and expects none.
    pr8112_cross_peer_invs: usize,
    /// dashpay/dash#8097: v24.0.0's `LoadCache` ran `CheckAndRemove` and `InitOnLoad` with a
    /// cached height of 0, so the trigger R9 stored for height 180 was too far into the future
    /// (`CSuperblock::IsExpired`, R9's row 95), marked for deletion at the start, and erased at
    /// the next tip; v24.0.2 sets the cached height first (`governance/governance.cpp`
    /// v24.0.2:100-112) and keeps it unmarked. The replay expects each recorded observation of
    /// that trigger after the restart unmarked, and the trigger still held where dashd had
    /// erased it: the observations so rewritten.
    pr8097_trigger_kept_at_load: usize,
}

/// Where the recorder's timing, not Core, places dashd's 1 s jobs, each counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Timing {
    /// dashd's sync tick fired after the tool's quiet wait on a `time` record and before the
    /// `mockscheduler` of the same `advance`, so its vote requests are recorded in the second
    /// segment. The node's tick runs there.
    tick_after_advance_time: usize,
    /// The tool's `deliver` sends without a quiet wait, so deliveries back to back fall in one
    /// of dashd's 1 s relay flushes. The node's flush runs after the last delivery of each such
    /// burst: deliveries the next one followed with nothing sent or checked between.
    delivery_bursts: usize,
}

/// The chain facts a checkpoint compares: the sim's, or V's, which the sim's follow a segment
/// later when dashd announced them after the next input.
struct ChainTip {
    height: u32,
    hash: BlockHash,
    time: u32,
    headers: u32,
}

/// What a replay counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Outcome {
    core: CoreDiffs,
    timing: Timing,
    checkpoints: usize,
}

/// The replay's script: the recording with the #8112 copies dropped, its segments, the bloom
/// tweaks in order, and the triggers it delivers.
struct Script {
    text: String,
    segments: Vec<Segment>,
    tweaks: Vec<u32>,
    triggers: BTreeSet<GovHash>,
    counts: Outcome,
    /// Generated blocks dashd announced with the next ([`superblocks::VCounts`]).
    coalesced: BTreeSet<BlockHash>,
}

/// Whether the hooks take record `f` as a check: it opens no segment.
fn is_check(f0: &str) -> bool {
    matches!(f0, "check" | "log")
}

fn script(raw: &str) -> Script {
    let mut counts = Outcome::default();
    let mut tweaks = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    let mut held: Vec<&str> = Vec::new();
    let mut drop_next = false;
    for line in raw.lines() {
        if drop_next {
            assert!(line.starts_with("< ") && line.contains(" inv "), "a declared copy: {line}");
            drop_next = false;
            counts.core.pr8112_cross_peer_invs += 1;
            lines.push("#");
            continue;
        }
        if line.starts_with("# declared-difference: R2-govsync-cross-peer ") {
            drop_next = true;
        }
        if let Some(h) = line.strip_prefix("rand bloom ") {
            tweaks.push(u32::from_le_bytes(unhex(h).try_into().expect("four bytes")));
            lines.push("#");
            continue;
        }
        // The tool's `submitblock` and `getblock` answers come before the quiet wait that
        // records the block's announcement; a check closes the segment, so they follow it here.
        let block_rpc = line.starts_with("check rpc {\"method\":\"submitblock\"")
            || line.starts_with("check rpc {\"method\":\"getblock\"");
        if block_rpc
            && (!held.is_empty() || lines.last().is_some_and(|l| l.starts_with("submitblock ")))
        {
            held.push(line);
            continue;
        }
        if !line.starts_with("< ") {
            lines.append(&mut held);
        }
        lines.push(line);
    }
    lines.append(&mut held);
    let mut segments: Vec<Segment> = Vec::new();
    let mut triggers = BTreeSet::new();
    for line in &lines {
        let f: Vec<&str> = line.split(' ').collect();
        match f[0] {
            "" | "#" | "mocktime" | "chain" | "ibd" => {}
            "<" => {
                segments.last_mut().expect("a segment").sends += 1;
                if f[2] == "govsync" {
                    let b = unhex(f[3]);
                    let filter_len = usize::from(b[32]);
                    if filter_len > 0 {
                        let hash: GovHash = b[..32].try_into().expect("a hash");
                        let n: u8 = f[1].parse().expect("a peer");
                        let seg = segments.last_mut().expect("a segment");
                        seg.requests.entry(n).or_default().push(hash);
                    }
                }
            }
            ">" if f[2] == "govobj" => {
                let obj = object::decode_network(&mut &unhex(f[3])[..]).expect("an object");
                if object::object_type(&obj) == object::TRIGGER {
                    triggers.insert(object::hash(&obj));
                }
                segments.push(Segment {
                    input: (*line).to_owned(),
                    ..Segment::default()
                });
            }
            f0 if is_check(f0) => {
                if let Some(seg) = segments.last_mut() {
                    seg.checks += 1;
                }
            }
            _ => segments.push(Segment {
                input: (*line).to_owned(),
                ..Segment::default()
            }),
        }
    }
    for i in 1..segments.len() {
        let (before, after) = segments.split_at_mut(i);
        let (seg, next) = (&mut before[i - 1], &after[0]);
        if seg.is("time")
            && next.is("mockscheduler")
            && seg.requests.is_empty()
            && !next.requests.is_empty()
        {
            seg.tick_later = true;
            counts.timing.tick_after_advance_time += 1;
        }
        if seg.delivers() && next.delivers() && seg.sends == 0 && seg.checks == 0 {
            seg.flush_later = true;
            if i < 2 || !before[i - 2].flush_later {
                counts.timing.delivery_bursts += 1;
            }
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    Script {
        text,
        segments,
        tweaks,
        triggers,
        counts,
        coalesced: BTreeSet::new(),
    }
}

/// The manager under the governance lock, whichever executor its handle posts to: the rig's, or
/// the node's ([`NodeHost`]).
trait ReadManager: Send + Sync {
    fn read(&self, f: &mut dyn FnMut(&Manager));

    /// `CreateNewBlock`'s governance reads for a block at `h`: `FillBlockPayments`'
    /// `IsSuperblockTriggered`, whose sentinel update stands, and the payments when triggered.
    // Core: masternode/payments.cpp v24.0.2:444-454; governance/superblock.cpp v24.0.2:609
    fn template(self: Arc<Self>, dmn: Arc<dyn DmnTip>, h: u32);
}

impl<E: GovExec + 'static> ReadManager for GovernanceHandle<E> {
    fn read(&self, f: &mut dyn FnMut(&Manager)) {
        f(&self.lock());
    }

    fn template(self: Arc<Self>, dmn: Arc<dyn DmnTip>, h: u32) {
        let mut backend = GovernanceBackend::new(self, dmn);
        backend.begin_block();
        if backend.sb_triggered(h) {
            backend.sb_payments(h);
        }
    }
}

/// The node under replay.
struct Node {
    params: GovParams,
    chain: Arc<SimChain>,
    sync: Arc<MasternodeSync>,
    env: Arc<Env>,
    handle: Arc<dyn ReadManager>,
    /// The handler and overlay the sim's processor calls.
    late: Late,
    /// The DMN-tip list the handler, the seam and the checkpoints read.
    tip: Arc<dyn DmnTip>,
    tick: SyncManager,
    tick_net: Arc<TickNet>,
    sched: Arc<Scheduler>,
    tweaks: Tweaks,
    blocks: Blocks,
    _scratch: Scratch,
}

/// How blocks reach the node.
enum Blocks {
    /// R1–R4: the sim's chain takes them, and the list, the index and governance's `tip` follow
    /// here.
    Sim(SimBlocks),
    /// R6–R10: V judges and connects them, and its listeners tell governance.
    V(Box<superblocks::VChain>),
}

struct SimBlocks {
    handle: Arc<GovernanceHandle<Exec>>,
    tip: Arc<TipList>,
    index: Arc<FakeIndex>,
    dml: Dml,
    /// The block bytes the recording submits, by hash.
    submitted: BTreeMap<BlockHash, Vec<u8>>,
    /// The height the node last told governance about.
    told: u32,
}

impl Node {
    /// Tells the index, the list, mnsync and governance about each block the sim's chain
    /// connected since the last call, oldest first. V's blocks reach them through V's signals.
    fn connect_new_blocks(&mut self) {
        let Blocks::Sim(b) = &mut self.blocks else {
            return;
        };
        let snap = self.chain.snapshot();
        if snap.tip.tip_height == b.told {
            return;
        }
        let from = self.chain.ancestor(snap.tip.tip, b.told);
        let mut hashes = self.chain.new_blocks(snap.tip.tip, from);
        hashes.reverse();
        for hash in hashes {
            let raw = b.submitted.get(&hash).expect("a submitted block").clone();
            b.connect(&raw, &self.sync);
        }
    }
}

impl SimBlocks {
    fn connect(&mut self, raw: &[u8], sync: &MasternodeSync) {
        let b = block(raw);
        self.dml.connect(&b);
        self.tip.set(Arc::clone(&self.dml.list));
        for tx in b.txs() {
            self.index.put(tx, b.hash());
        }
        let height = u32::try_from(self.dml.headers.len() - 1).expect("a height");
        let at = BlockRef {
            height,
            hash: *b.hash().as_bytes(),
        };
        sync.updated_block_tip(Some(at), at, false);
        self.told = height;
        let done = self.handle.tip(i32::try_from(height).expect("a height")).wait();
        let done = done.expect("the tip command ran");
        assert!(done.result.is_ok() && !done.index_fault, "tip {height}");
    }
}

/// The recordings' `check` records and inputs, and what the node runs in each quiet wait.
struct Replayer<'a> {
    name: &'a str,
    node: Node,
    script: &'a Script,
    /// The next segment to settle.
    at: usize,
    /// Each peer's last score, kept once it is gone (`check score`'s rule).
    scores: BTreeMap<PeerId, i32>,
    /// The sim's id for dashd's `core_id` 0: a restarted dashd numbers its peers from 0 again,
    /// the sim's network core goes on from the ids it gave before.
    id_base: i64,
    /// The `timeoffset` a `getnetworkinfo` observation announces.
    offset: Option<i64>,
    /// The per-tick object-request cap from the recorded masternode count.
    cap: usize,
    checkpoints: usize,
}

impl Replayer<'_> {
    fn settle_scores(&mut self, sim: &Sim) {
        for peer in sim.core.peers().iter() {
            if let Some((score, _)) = sim.shared.misbehavior(peer.id()) {
                self.scores.insert(peer.id(), score);
            }
        }
    }

    /// The tick's recorded requests, triggers then others, in dashd's order.
    fn tick_orders(&self, seg: &Segment) -> [Vec<GovHash>; 2] {
        let mut order: Vec<GovHash> = Vec::new();
        for (n, hashes) in &seg.requests {
            assert!(
                hashes.len() < self.cap,
                "{}: peer {n}: under nMaxObjRequestsPerNode",
                seg.input
            );
            let k = hashes.iter().take_while(|h| self.script.triggers.contains(*h)).count();
            assert!(
                hashes[k..].iter().all(|h| !self.script.triggers.contains(h)),
                "{}: peer {n}: dashd asked for triggers first",
                seg.input
            );
            let mut i = 0;
            for h in hashes {
                match order.iter().position(|o| o == h) {
                    Some(p) => {
                        assert!(p >= i, "{}: peer {n}: one order across peers", seg.input);
                        i = p + 1;
                    }
                    None => {
                        order.push(*h);
                        i = order.len();
                    }
                }
            }
        }
        let (t, o): (Vec<_>, Vec<_>) =
            order.into_iter().partition(|h| self.script.triggers.contains(h));
        [t, o]
    }

    fn check_rpc(&mut self, sim: &Sim, json: &Value) {
        let method = json["method"].as_str().expect("a method");
        let params: Vec<&str> =
            json["params"].as_array().expect("params").iter().filter_map(Value::as_str).collect();
        if self.v_check_rpc(method, &params, json) {
            return;
        }
        let s = &self.node.sync;
        match (method, params.as_slice()) {
            ("mnsync", ["next"]) => {
                let want = format!("sync updated to {}", s.asset_name());
                assert_eq!(
                    json["result"].as_str(),
                    Some(want.as_str()),
                    "{}: mnsync next",
                    self.name
                );
            }
            ("mnsync", ["reset"]) => assert_eq!(json["result"], "success"),
            // An observation of how dashd got the offset that `timeoffset` then supplies.
            ("getnetworkinfo", []) => {
                self.offset = json["result"]["timeoffset"].as_i64();
            }
            ("submitblock", [hex]) => {
                assert!(json["result"].is_null(), "{}: dashd accepted the block", self.name);
                let tip = sim.chain.snapshot().tip.tip_hash;
                assert_eq!(tip, block(&unhex(hex)).hash(), "{}: the block is the tip", self.name);
            }
            // The chainstate's answer for the block just submitted; nothing of governance.
            ("getblock", _) => {}
            ("getgovernanceinfo", []) => self.check_governance_info(sim, &json["result"]),
            // Core: rpc/governance.cpp v24.0.2:625-656 ListObjects
            ("gobject", ["list", "all", "triggers"]) => {
                let list = self.node.tip.dmn_tip_list();
                let mut got = serde_json::Map::new();
                self.node.handle.read(&mut |m| got = gobject_list(m, &list, &self.node.params));
                got.retain(|_, e| e["ObjectType"] == object::TRIGGER);
                let want = json["result"].as_object().expect("objects");
                assert_eq!(got, *want, "{}: gobject list all triggers", self.name);
            }
            ("gobject", ["get", hash]) => {
                let list = self.node.tip.dmn_tip_list();
                let mut got = Value::Null;
                self.node
                    .handle
                    .read(&mut |m| got = gobject_get(m, &list, &self.node.params, hash));
                assert_eq!(got, json["result"], "{}: gobject get {hash}", self.name);
            }
            other => panic!("{}: unexpected rpc {other:?}", self.name),
        }
    }

    fn check_score(&mut self, sim: &Sim, json: &Value) {
        let label = json["label"].as_str().expect("a label");
        let live: BTreeSet<PeerId> = sim.core.peers().iter().map(|p| p.id()).collect();
        for (n, peer) in json["peers"].as_object().expect("peers") {
            let id = peer["core_id"].as_i64().expect("an id") + self.id_base;
            let connected = live.contains(&id) && sim.shared.has_peer(id);
            assert_eq!(
                connected,
                peer["connected"] == true,
                "{}: {label}: peer {n} connected",
                self.name
            );
            let score =
                sim.shared.misbehavior(id).map(|(s, _)| s).or(self.scores.get(&id).copied());
            assert_eq!(
                score.map(i64::from),
                peer["score"].as_i64(),
                "{}: {label}: peer {n} score",
                self.name
            );
        }
    }

    /// `getgovernanceinfo` at the node's tip.
    // Core: rpc/governance.cpp v24.0.2:964-990
    fn check_governance_info(&self, sim: &Sim, want: &Value) {
        let p = &self.node.params;
        let height = i32::try_from(self.chain_tip(sim).height).expect("a height");
        let (last, next) = nearest_superblock_heights(height, p);
        let weighted = self.node.tip.dmn_tip_list().counts().valid_weighted;
        let sats = |v: &Value| (v.as_f64().expect("an amount") * 1e8).round() as i64;
        let got = serde_json::json!({
            "governanceminquorum": p.min_quorum(),
            "proposalfee": object::PROPOSAL_FEE,
            "superblockcycle": p.superblock_cycle(),
            "superblockmaturitywindow": p.superblock_maturity_window(),
            "lastsuperblock": last,
            "nextsuperblock": next,
            "fundingthreshold": weighted / 10,
            "governancebudget": payments_limit(next, p).expect("a budget"),
        });
        let mut want = want.clone();
        for key in ["proposalfee", "governancebudget"] {
            want[key] = sats(&want[key]).into();
        }
        assert_eq!(got, want, "{}: getgovernanceinfo", self.name);
    }

    /// The node's tip: V's, or the sim chain's.
    fn chain_tip(&self, sim: &Sim) -> ChainTip {
        match &self.node.blocks {
            Blocks::V(v) => v.tip(),
            Blocks::Sim(_) => {
                let t = sim.chain.snapshot().tip;
                ChainTip {
                    height: t.tip_height,
                    hash: t.tip_hash,
                    time: t.tip_time,
                    headers: t.best_header_height,
                }
            }
        }
    }

    fn check_gobject(&mut self, sim: &Sim, json: &Value) {
        self.checkpoints += 1;
        let label = json["label"].as_str().expect("a label");
        let at = format!("{}: {label}", self.name);
        let tip = self.chain_tip(sim);
        let n = &self.node;
        let list = n.tip.dmn_tip_list();
        n.handle.read(&mut |m| {
            assert_eq!(
                gobject_list(m, &list, &n.params),
                *json["objects"].as_object().expect("objects"),
                "{at}: gobject list all"
            );
            assert_eq!(
                current_votes(m, &list),
                *json["votes"].as_object().expect("votes"),
                "{at}: gobject getcurrentvotes"
            );
            assert_eq!(i64::from(m.cached_height()), i64::from(tip.height), "{at}: cached height");
        });
        let next = nearest_superblock_heights(tip.height as i32, &n.params).1;
        assert_eq!(
            json["budget"]["height"].as_i64(),
            Some(i64::from(next)),
            "{at}: next superblock"
        );
        let amount = payments_limit(next, &n.params).expect("a budget");
        let recorded = (json["budget"]["amount"].as_f64().expect("an amount") * 1e8).round() as i64;
        assert_eq!(amount, recorded, "{at}: getsuperblockbudget");
        let s = &n.sync;
        let sync = serde_json::json!({
            "AssetID": s.asset_id(),
            "AssetName": s.asset_name(),
            "AssetStartTime": s.asset_start_time(),
            "Attempt": s.attempt(),
            "IsBlockchainSynced": s.is_blockchain_synced(),
            "IsSynced": s.is_synced(),
        });
        assert_eq!(sync, json["sync"], "{at}: mnsync status");
        let snap = sim.chain.snapshot();
        let ibd =
            sim.chain.is_initial_block_download(&snap, dash_clock::Clock::node_now(&*sim.clock));
        let chain = serde_json::json!({
            "bestblockhash": hash_hex(tip.hash.as_bytes()),
            "blocks": tip.height,
            "headers": tip.headers,
            "initialblockdownload": ibd,
            "time": tip.time,
        });
        for (k, v) in chain.as_object().expect("an object") {
            assert_eq!(json["chain"][k], *v, "{at}: getblockchaininfo.{k}");
        }
    }
}

/// `gobject list all`'s entries, built from the manager as Core's `ListObjects` reads it.
// Core: rpc/governance.cpp v24.0.2:376-428 ListObjects
fn gobject_list(m: &Manager, list: &MnList, p: &GovParams) -> serde_json::Map<String, Value> {
    let funding = vote::SIGNAL_FUNDING;
    m.objects()
        .iter()
        .map(|(hash, st)| {
            let obj = m.object_for_hash(hash).expect("the object");
            let unparsable = matches!(object::load_data(obj.data()), LoadedType::Unparsable);
            let local = object::is_valid_locally(obj, unparsable, Check::NoCollateral, p);
            let mut e = serde_json::json!({
                "DataHex": hex(obj.data()),
                "DataString": String::from_utf8_lossy(obj.data()),
                "Hash": hash_hex(hash),
                "CollateralHash": hash_hex(&object::txid_bytes(&obj.collateral_hash())),
                "ObjectType": st.obj_type,
                "CreationTime": obj.time(),
                "AbsoluteYesCount": st.absolute_yes(list, funding),
                "YesCount": st.yes(list, funding),
                "NoCount": st.no(list, funding),
                "AbstainCount": st.abstain(list, funding),
                "fBlockchainValidity": local.valid,
                "IsValidReason": local.error,
                "fCachedValid": st.cached_valid,
                "fCachedFunding": st.cached_funding,
                "fCachedDelete": st.cached_delete,
                "fCachedEndorsed": st.cached_endorsed,
            });
            let op = obj.masternode_outpoint();
            if !op.is_null() {
                e["SigningMasternode"] = object::outpoint_short(&op).into();
            }
            (hash_hex(hash), e)
        })
        .collect()
}

/// `gobject get`: the object's state with `fLocalValidity`, and its counts by signal.
// Core: rpc/governance.cpp v24.0.2:749-767; governance/object.cpp v24.0.2:603-695
fn gobject_get(m: &Manager, list: &MnList, p: &GovParams, hash: &str) -> Value {
    let (_, st) = m.objects().iter().find(|(h, _)| hash_hex(h) == hash).expect("the object");
    let mut entry = gobject_list(m, list, p).remove(hash).expect("its entry");
    let e = entry.as_object_mut().expect("an entry");
    for key in ["AbsoluteYesCount", "YesCount", "NoCount", "AbstainCount"] {
        e.remove(key);
    }
    let valid = e.remove("fBlockchainValidity").expect("its validity");
    e.insert("fLocalValidity".to_owned(), valid);
    let signals = [
        ("FundingResult", vote::SIGNAL_FUNDING),
        ("ValidResult", vote::SIGNAL_VALID),
        ("DeleteResult", vote::SIGNAL_DELETE),
        ("EndorsedResult", vote::SIGNAL_ENDORSED),
    ];
    for (key, signal) in signals {
        let counts = serde_json::json!({
            "AbsoluteYesCount": st.absolute_yes(list, signal),
            "YesCount": st.yes(list, signal),
            "NoCount": st.no(list, signal),
            "AbstainCount": st.abstain(list, signal),
        });
        e.insert(key.to_owned(), counts);
    }
    entry
}

/// `gobject getcurrentvotes` for each object, by object hash: each instance as a vote keyed by
/// its hash, `outpoint:time:outcome:signal:weight`.
// Core: rpc/governance.cpp v24.0.2:880-924; governance/vote.cpp v24.0.2:93-105
fn current_votes(m: &Manager, list: &MnList) -> serde_json::Map<String, Value> {
    m.objects()
        .keys()
        .map(|parent| {
            let votes: serde_json::Map<String, Value> = m
                .get_current_votes(parent, None, list)
                .iter()
                .map(|v| {
                    let op = v.masternode_outpoint();
                    let weight = list.get_by_collateral(&op).map_or(0, |d| d.weight());
                    let text = format!(
                        "{}:{}:{}:{}:{weight}",
                        object::outpoint_short(&op),
                        v.time(),
                        vote::outcome_to_string(vote::outcome(v)),
                        vote::signal_to_string(vote::signal(v)),
                    );
                    (hash_hex(&vote::hash(v)), Value::String(text))
                })
                .collect();
            (hash_hex(parent), Value::Object(votes))
        })
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Hooks for Replayer<'_> {
    fn input(&mut self, sim: &mut Sim, f: &[&str]) -> bool {
        match f[0] {
            // Core: rpc/node.cpp v24.0.2:139-147
            "mnsync" => match f[1] {
                "next" => self.node.sync.switch_to_next_asset(),
                "reset" => self.node.sync.reset(true, true),
                other => panic!("mnsync {other}"),
            },
            // Core: rpc/node.cpp v24.0.2:893-915
            "mockscheduler" => {
                let d = Duration::from_secs(f[1].parse().expect("seconds"));
                sim.shared.mock_scheduler(d);
                self.node.sched.mock_forward(d);
                self.v_settle_posted();
            }
            "restart" => self.v_restart(sim),
            "timeoffset" => {
                let offset: i64 = f[1].parse().expect("seconds");
                assert_eq!(self.offset, Some(offset), "{}: getnetworkinfo saw it", self.name);
                self.node.env.offset.store(offset, Ordering::SeqCst);
            }
            _ => return self.v_input(f),
        }
        true
    }

    fn later(&self, cmd: &str, payload: &[u8]) -> bool {
        matches!(self.node.blocks, Blocks::V(_)) && superblocks::other_handler(cmd, payload)
    }

    fn check(&mut self, sim: &Sim, f: &[&str]) -> bool {
        if !is_check(f[0]) {
            return false;
        }
        if f[0] == "check" {
            let json: Value = serde_json::from_str(&f[2..].join(" ")).expect("a check's JSON");
            match f[1] {
                "rpc" => self.check_rpc(sim, &json),
                "gobject" => self.check_gobject(sim, &json),
                "score" => self.check_score(sim, &json),
                other => panic!("check {other}"),
            }
        }
        true
    }

    fn runs_sync_tick(&self) -> bool {
        true
    }

    fn settled(&mut self, sim: &mut Sim) {
        let segments = &self.script.segments;
        let seg = &segments[self.at];
        self.at += 1;
        self.node.connect_new_blocks();
        self.v_settled(sim, seg);
        if !seg.flush_later {
            self.node.sched.run_every(relay_interval(&self.node.params));
        }
        if !seg.tick_later {
            let [triggers, others] = self.tick_orders(seg);
            {
                let mut orders =
                    self.node.tick_net.orders.lock().unwrap_or_else(PoisonError::into_inner);
                orders.clear();
                orders.push_back(("triggers", triggers));
                orders.push_back(("others", others));
            }
            self.node.tick.process_tick();
            let left: Vec<_> = std::mem::take(
                &mut *self.node.tick_net.orders.lock().unwrap_or_else(PoisonError::into_inner),
            )
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .collect();
            assert!(
                left.is_empty(),
                "{}: `{}`: dashd asked for votes the node did not: {left:?}",
                self.name,
                seg.input
            );
        }
        self.settle_scores(sim);
    }
}

/// Replays recording `name` against a fresh node, its blocks on the sim's chain; returns what it
/// counted.
fn run(name: &str) -> Outcome {
    run_on(name, &script(&fixture(name)), None).0
}

/// Replays `script` against a fresh node: its blocks on the sim's chain, or with `v` on V from
/// that bootstrap. Returns what it counted, and how the blocks went.
fn run_on(name: &str, script: &Script, v: Option<superblocks::Boot>) -> (Outcome, Blocks) {
    let boot = bootstrap();
    let p = chain_params();
    let gov_params = GovParams::from_chain_params(&p).expect("its parameters");
    // dashd's chain before the recording: the bootstrap's blocks, connected.
    let raws = v.as_ref().map_or(&boot.blocks, |v| &v.blocks).clone();
    let (sim, late) = sim_with(&p, &raws);

    let clock = Arc::clone(&sim.clock);
    let net = sync_net(&sim);
    let scratch = Scratch::new(name);
    let fulfilled = Arc::new(NetFulfilledRequestManager::new(
        clock.clone(),
        i64::from(p.fulfilled_request_expire_time),
    ));
    assert!(fulfilled.load_cache(&scratch.0.join("netfulfilled.dat"), p.message_start, false));
    let notifier = FullSyncNotifier {
        net: net.clone(),
        netfulfilled: Arc::clone(&fulfilled),
    };
    let sync = Arc::new(MasternodeSync::new(clock.clone(), Box::new(notifier)));
    let env = Arc::new(Env {
        clock: clock.clone(),
        sync: Arc::clone(&sync),
        offset: AtomicI64::new(0),
    });
    let relay = Arc::new(RelayInvs::default());
    let sink = Arc::new(Worker {
        relay: Arc::clone(&relay),
        sync: Arc::clone(&sync),
    });
    let index = Arc::new(FakeIndex::default());
    let tip_list = Arc::new(TipList(Mutex::new(Arc::new(MnList::default_pre_dip3()))));
    let v_exec = superblocks::VExec::default();
    let exec = match v {
        None => Exec::Sim(SimExec(Mutex::new(SimView {
            chain: Arc::clone(&sim.chain),
            tip: Arc::clone(&tip_list),
        }))),
        Some(_) => Exec::V(v_exec.clone()),
    };
    let mut manager = Manager::new();
    manager.set_loaded(true);
    let handle = GovernanceHandle::new(
        manager,
        exec,
        env.clone(),
        sink.clone(),
        index.clone(),
        gov_params.clone(),
    );
    let genesis = header_bytes(&sim.chain.genesis().header);
    let (tip, mut chain_side): (Arc<dyn DmnTip>, Blocks) = match v {
        None => {
            let mut submitted = BTreeMap::new();
            for line in script.text.lines() {
                if let Some(hex) = line.strip_prefix("submitblock ") {
                    let raw = unhex(hex);
                    submitted.insert(block(&raw).hash(), raw);
                }
            }
            let blocks = SimBlocks {
                handle: Arc::clone(&handle),
                tip: Arc::clone(&tip_list),
                index: index.clone(),
                dml: Dml::new(Arc::clone(&p), genesis),
                submitted,
                told: 0,
            };
            (tip_list, Blocks::Sim(blocks))
        }
        Some(boot) => {
            let v = superblocks::VChain::start(
                &p,
                sim.clock.clone(),
                &handle,
                v_exec,
                &sync,
                &index,
                boot,
                script,
            );
            (Arc::new(v.view()), Blocks::V(Box::new(v)))
        }
    };
    if let Blocks::Sim(b) = &mut chain_side {
        for raw in &boot.blocks {
            b.connect(raw, &sync);
        }
    }
    let masternodes = tip.dmn_tip_list().counts().enabled() as usize;
    if raws.len() == boot.blocks.len() {
        assert_eq!(masternodes, boot.masternodes);
    }
    let tweaks = Tweaks(Arc::new(Mutex::new(ReplayRng::new(
        script.tweaks.iter().map(|t| Draw::BloomTweak(*t)),
    ))));
    let (seam, held) = tick_seam(&handle, &sync, &tip, &tweaks, &gov_params);
    let peers = Arc::new(SimPeers {
        shared: Arc::clone(&sim.shared),
        net: net.clone(),
    });
    let gov = NetGovernance::new(
        Arc::clone(&handle),
        seam.clone(),
        tip.clone(),
        Arc::clone(&fulfilled),
        peers,
        Arc::clone(&relay),
        sink,
    );
    let (tick, tick_net, sched) = wire(&late, gov, seam, held, net, clock, &sync, fulfilled);
    let node = Node {
        params: gov_params,
        chain: Arc::clone(&sim.chain),
        sync,
        env,
        handle,
        late,
        tip,
        tick,
        tick_net,
        sched,
        tweaks,
        blocks: chain_side,
        _scratch: scratch,
    };
    play(name, script, sim, node, masternodes)
}

/// The sim with the governance handler registered and bound later, and dashd's chain before the
/// recording, `raws`, connected on it.
fn sim_with(p: &Arc<ChainSetup>, raws: &[Vec<u8>]) -> (Sim, Late) {
    let id = local_identity(p, &IdentityOptions::new()).expect("an identity");
    let late = Late::default();
    let mut registry = Registry::new();
    registry.register(Box::new(late.clone()));
    let seams = Seams {
        overlay: Arc::new(late.clone()),
        ..Seams::default()
    };
    let sim = Sim::with(Arc::clone(p), id, ProcessorOptions::default(), registry, seams, |_| {});
    let blocks: Vec<Arc<Block>> = raws.iter().map(|raw| Arc::new(block(raw))).collect();
    let tip = sim.chain.store_blocks(&blocks);
    sim.chain.set_tip(tip.id);
    assert_eq!(tip.height as usize, raws.len());
    (sim, late)
}

/// SP1's network over the sim's core, which reads the processor's object requests.
fn sync_net(sim: &Sim) -> Arc<CoreSyncNet> {
    let net = Arc::new(CoreSyncNet::new(Arc::clone(&sim.core), false));
    net.bind_requests(sim.lp.logic().requested_objects());
    net
}

/// The tick's seam over `handle`: its bloom tweaks the recorded ones, each fetch's held objects
/// recorded ([`held_at_fetch`]).
fn tick_seam<E: GovExec + 'static>(
    handle: &Arc<GovernanceHandle<E>>,
    sync: &Arc<MasternodeSync>,
    tip: &Arc<dyn DmnTip>,
    tweaks: &Tweaks,
    params: &GovParams,
) -> (Arc<NodeGovernanceSync<E>>, Held) {
    let held = Held::default();
    let seam = NodeGovernanceSync::new(
        Arc::clone(handle),
        Arc::clone(sync),
        Arc::clone(tip),
        Box::new(tweaks.clone()),
        params,
    )
    .on_fetch(held_at_fetch(&held));
    (Arc::new(seam), held)
}

/// Binds `gov` and `seam` for the sim's processor, schedules `gov`'s jobs on a scheduler of their
/// own, and builds SP1's tick over `seam`.
#[allow(clippy::too_many_arguments, reason = "one process's parts")]
fn wire<E: GovExec + 'static>(
    late: &Late,
    gov: NetGovernance<E>,
    seam: Arc<NodeGovernanceSync<E>>,
    held: Held,
    net: Arc<CoreSyncNet>,
    clock: Arc<VirtualClock>,
    sync: &Arc<MasternodeSync>,
    fulfilled: Arc<NetFulfilledRequestManager>,
) -> (SyncManager, Arc<TickNet>, Arc<Scheduler>) {
    let sched = Arc::new(Scheduler::default());
    gov.schedule(&*sched, Box::new(ReplayRng::default()));
    late.bind(Box::new(gov), seam.clone());
    let tick_net = Arc::new(TickNet {
        inner: net,
        orders: Mutex::new(VecDeque::new()),
        clock: clock.clone(),
        asked: Mutex::default(),
        held: Some(held),
    });
    let cfg = SyncConfig {
        mockable_chain: true,
        mainnet: false,
        sync_mempool: true,
    };
    let tick = SyncManager::new(clock, cfg, Arc::clone(sync), fulfilled, tick_net.clone(), seam);
    (tick, tick_net, sched)
}

/// Replays `script` against `node` in `sim`; what it counted, and how the blocks went.
fn play(
    name: &str,
    script: &Script,
    sim: Sim,
    node: Node,
    masternodes: usize,
) -> (Outcome, Blocks) {
    let mut hooks = Replayer {
        name,
        node,
        script,
        at: 0,
        scores: BTreeMap::new(),
        id_base: 0,
        offset: None,
        cap: 2000 / masternodes,
        checkpoints: 0,
    };
    replay(name, &script.text, sim, &mut hooks);
    assert_eq!(hooks.at, script.segments.len(), "{name}: every segment settled");
    let left = hooks.node.tweaks.0.lock().unwrap_or_else(PoisonError::into_inner).remaining();
    assert_eq!(left, 0, "{name}: every recorded bloom tweak drawn");
    let outcome = Outcome {
        checkpoints: hooks.checkpoints,
        ..script.counts.clone()
    };
    (outcome, hooks.node.blocks)
}

/// R1: an unrequested object is dropped; two announcers, and the second's delivery is unrequested
/// once the first's was admitted (global forget); proposals and a trigger admitted and relayed
/// in batches; an unmined collateral scores +20.
// Core: governance/net_governance.cpp v24.0.2:171-198
#[test]
fn r1_gov_objects() {
    let want = Outcome {
        timing: Timing {
            delivery_bursts: 1,
            ..Timing::default()
        },
        checkpoints: 7,
        ..Outcome::default()
    };
    assert_eq!(run("R1"), want);
}

/// R2: full sync (`ssc 10`, then the invs), object fetch, bloom vote sync (`ssc 11`), a tracked
/// repeat +20, an unknown parent's vote sync repeated unscored, an oversized filter +100, and
/// nothing answered while unsynced. dashd v24.0.0 also announced the two replies' invs to the
/// peer that did not ask; v24.0.2 does not (dashpay/dash#8112).
// Core: governance/net_governance.cpp v24.0.2:86-169
#[test]
fn r2_govsync() {
    let want = Outcome {
        core: CoreDiffs {
            pr8112_cross_peer_invs: 2,
            ..CoreDiffs::default()
        },
        timing: Timing {
            delivery_bursts: 2,
            ..Timing::default()
        },
        checkpoints: 10,
    };
    assert_eq!(run("R2"), want);
}

/// R3: an orphan vote's parent asked for at once and again by the 5-minute job, the vote applied
/// on its parent's arrival, an expired orphan dropped, a malformed vote +100, and a bad
/// signature +20 synced and unscored while syncing governance.
// Core: governance/net_governance.cpp v24.0.2:45-66, 202-270; governance/governance.cpp
// v24.0.2:280-310
#[test]
fn r3_votes_orphans() {
    let want = Outcome {
        timing: Timing {
            tick_after_advance_time: 1,
            delivery_bursts: 0,
        },
        checkpoints: 8,
        ..Outcome::default()
    };
    assert_eq!(run("R3"), want);
}

/// R3's offset phase: an adjusted offset of +600 (`timeoffset`), an orphan vote, then its parent
/// 300 s later: past the orphan's adjusted deadline, before its wall one, with no periodic
/// cleanup between, so the vote is dropped unapplied.
// Core: governance/governance.cpp v24.0.2:289-297
#[test]
fn r3_offset_phase() {
    let want = Outcome {
        checkpoints: 2,
        ..Outcome::default()
    };
    assert_eq!(run("R3-offset"), want);
}

/// R4: a proposal whose collateral has no confirmation is refused (+20), with one to five it is
/// postponed, and the tip that gives it six admits it without a resend.
// Core: governance/governance.cpp v24.0.2:227-275, 885-910
#[test]
fn r4_postponed() {
    let want = Outcome {
        timing: Timing {
            delivery_bursts: 2,
            ..Timing::default()
        },
        checkpoints: 8,
        ..Outcome::default()
    };
    assert_eq!(run("R4"), want);
}

/// A relaying peer and a masternode connection, for the tick's shuffle on its own.
struct TwoPeers;

impl SyncNet for TwoPeers {
    fn fully_connected(&self) -> Vec<SyncPeer> {
        let peer = |id: u8, can_relay| SyncPeer {
            id: i64::from(id),
            addr: Service::new(dash_p2p::NetAddr::ipv4([10, 0, 0, id]), 1),
            can_relay,
            inbound: false,
            unrestricted: false,
            block_relay_only: false,
        };
        vec![peer(1, true), peer(2, false)]
    }

    fn all_addrs(&self) -> Vec<Service> {
        Vec::new()
    }

    fn push(&self, _: i64, _: SyncMsg) {}

    fn disconnect(&self, _: i64) {}

    fn requested_object_count(&self, _: i64) -> usize {
        0
    }

    fn is_active_masternode(&self) -> bool {
        false
    }

    fn shuffle(&self, _: &mut [GovHash]) {}
}

/// The tick's shuffle takes a hash beyond dashd's set only while the tick has asked every
/// relaying peer for it within the hour, Core's `mapAskedRecently`.
// Core: node/sync_manager.cpp v24.0.2:64-65, :111-118; governance/governance.cpp v24.0.2:983-986
#[test]
fn shuffle_offers_beyond_dashd_only_what_every_peer_was_asked() {
    let clock = Arc::new(dash_clock::MockClock::at(1_700_000_000));
    let net = TickNet {
        inner: Arc::new(TwoPeers),
        orders: Mutex::new(VecDeque::new()),
        clock: clock.clone(),
        asked: Mutex::default(),
        held: None,
    };
    let (want, extra) = ([1; 32], [2; 32]);
    let offer = || {
        net.orders
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(("objects", vec![want]));
        let mut hashes = [extra, want];
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| net.shuffle(&mut hashes)))
            .map(|()| hashes)
            .is_ok_and(|h| h == [extra, want])
    };
    assert!(!offer(), "never asked");
    net.push(
        2,
        SyncMsg::GovSync {
            hash: extra,
            filter: dashd_masternode::sync_manager::empty_bloom_filter(),
        },
    );
    assert!(!offer(), "asked of the masternode connection only");
    net.push(
        1,
        SyncMsg::GovSync {
            hash: extra,
            filter: dashd_masternode::sync_manager::empty_bloom_filter(),
        },
    );
    assert!(offer(), "asked of the relaying peer");
    clock.set_mocktime(1_700_003_600);
    assert!(offer(), "an hour later");
    clock.set_mocktime(1_700_003_601);
    assert!(!offer(), "past the hour");
}
