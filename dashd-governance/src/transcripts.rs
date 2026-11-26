//! S3b: dashd v24.0.0's R1–R4 recordings (S0, `tests/fixtures/dashd-v24.0.0`, format in
//! `contrib/node/gv1b-transcripts/capture.py`) replayed in `dashd_p2p::sim` against the manager,
//! [`NetGovernance`] and its jobs, and SP1's sync tick over [`NodeGovernanceSync`] (GV1b plan §6
//! Replay). Each segment's outbound bytes per peer, each `check score`, each `check gobject` and
//! each recorded `mnsync` answer must equal dashd's.
//!
//! A crate test rather than `tests/transcripts.rs`: the checkpoints read the manager under the
//! governance lock, which only the crate reaches (`GovernanceHandle::lock`).
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
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
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
use dash_p2p::{CommandString, NetCore, OutMsg, PeerId, Service};
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
use crate::net::{GovPeers, NetGovernance, RelayInvs};
use crate::rand::{Draw, GovRng, ReplayRng};
use crate::schedule::{GovScheduler, relay_interval};
use crate::sync::{DmnTip, NodeGovernanceSync};
use crate::tests::FakeIndex;

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
/// then takes from the back, so the hashes the node shuffles must be the recorded set, and
/// dashd's order is then given back reversed.
struct TickNet {
    inner: Arc<CoreSyncNet>,
    orders: Mutex<VecDeque<(&'static str, Vec<GovHash>)>>,
}

impl SyncNet for TickNet {
    fn fully_connected(&self) -> Vec<SyncPeer> {
        self.inner.fully_connected()
    }

    fn all_addrs(&self) -> Vec<Service> {
        self.inner.all_addrs()
    }

    fn push(&self, peer: i64, msg: SyncMsg) {
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
        let sorted = |v: &[GovHash]| v.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(
            sorted(hashes).iter().map(hash_hex).collect::<Vec<_>>(),
            sorted(&want).iter().map(hash_hex).collect::<Vec<_>>(),
            "the tick's {group} requested, as a set"
        );
        assert_eq!(hashes.len(), want.len(), "the tick's {group}: no repeats");
        for (slot, h) in hashes.iter_mut().zip(want.iter().rev()) {
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

/// The handler and the overlay, registered before the sim exists and bound after.
#[derive(Clone, Default)]
struct Late(Arc<OnceLock<Bound>>);

struct Bound {
    net: Mutex<NetGovernance<SimExec>>,
    seam: Arc<NodeGovernanceSync<SimExec>>,
}

impl Late {
    fn net(&self) -> std::sync::MutexGuard<'_, NetGovernance<SimExec>> {
        self.0.get().expect("bound").net.lock().unwrap_or_else(PoisonError::into_inner)
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
        self.0.get().is_some_and(|b| b.seam.governance_ready())
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
#[derive(Debug, Default, PartialEq, Eq)]
struct CoreDiffs {
    /// dashpay/dash#8112: v24.0.0 relays the full-sync and vote-sync invs (`PeerRelayInv`), so
    /// they also reach the peers that did not ask; v24.0.2 pushes them to the requester alone
    /// (`net_governance.cpp` v24.0.2:150-167). The replay drops each recorded copy at another
    /// peer (`# declared-difference: R2-govsync-cross-peer`) and expects none.
    pr8112_cross_peer_invs: usize,
}

/// Where the recorder's timing, not Core, places dashd's 1 s jobs, each counted.
#[derive(Debug, Default, PartialEq, Eq)]
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

/// What a replay counted.
#[derive(Debug, Default, PartialEq, Eq)]
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
    }
}

/// The node under replay.
struct Node {
    params: GovParams,
    chain: Arc<SimChain>,
    sync: Arc<MasternodeSync>,
    env: Arc<Env>,
    handle: Arc<GovernanceHandle<SimExec>>,
    tip: Arc<TipList>,
    index: Arc<FakeIndex>,
    dml: Dml,
    tick: SyncManager,
    tick_net: Arc<TickNet>,
    sched: Arc<Scheduler>,
    tweaks: Tweaks,
    /// The block bytes the recording submits, by hash.
    submitted: BTreeMap<BlockHash, Vec<u8>>,
    /// The height the node last told governance about.
    told: u32,
    _scratch: Scratch,
}

impl Node {
    /// Tells the index, the list, mnsync and governance about each block the sim's chain
    /// connected since the last call, oldest first.
    fn connect_new_blocks(&mut self) {
        let snap = self.chain.snapshot();
        if snap.tip.tip_height == self.told {
            return;
        }
        let from = self.chain.ancestor(snap.tip.tip, self.told);
        let mut hashes = self.chain.new_blocks(snap.tip.tip, from);
        hashes.reverse();
        for hash in hashes {
            let raw = self.submitted.get(&hash).expect("a submitted block").clone();
            self.connect(&raw);
        }
    }

    fn connect(&mut self, raw: &[u8]) {
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
        self.sync.updated_block_tip(Some(at), at, false);
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
            other => panic!("{}: unexpected rpc {other:?}", self.name),
        }
    }

    fn check_score(&mut self, sim: &Sim, json: &Value) {
        let label = json["label"].as_str().expect("a label");
        let live: BTreeSet<PeerId> = sim.core.peers().iter().map(|p| p.id()).collect();
        for (n, peer) in json["peers"].as_object().expect("peers") {
            let id = peer["core_id"].as_i64().expect("an id");
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

    fn check_gobject(&mut self, sim: &Sim, json: &Value) {
        self.checkpoints += 1;
        let label = json["label"].as_str().expect("a label");
        let at = format!("{}: {label}", self.name);
        let n = &self.node;
        let list = n.tip.dmn_tip_list();
        let tip = sim.chain.snapshot().tip;
        {
            let m = n.handle.lock();
            assert_eq!(
                gobject_list(&m, &list, &n.params),
                *json["objects"].as_object().expect("objects"),
                "{at}: gobject list all"
            );
            assert_eq!(
                current_votes(&m, &list),
                *json["votes"].as_object().expect("votes"),
                "{at}: gobject getcurrentvotes"
            );
            assert_eq!(
                i64::from(m.cached_height()),
                i64::from(tip.tip_height),
                "{at}: cached height"
            );
        }
        let next = nearest_superblock_heights(tip.tip_height as i32, &n.params).1;
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
            "bestblockhash": hash_hex(tip.tip_hash.as_bytes()),
            "blocks": tip.tip_height,
            "headers": tip.best_header_height,
            "initialblockdownload": ibd,
            "time": tip.tip_time,
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
            }
            "timeoffset" => {
                let offset: i64 = f[1].parse().expect("seconds");
                assert_eq!(self.offset, Some(offset), "{}: getnetworkinfo saw it", self.name);
                self.node.env.offset.store(offset, Ordering::SeqCst);
            }
            _ => return false,
        }
        true
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

/// Replays recording `name` against a fresh node; returns what it counted.
fn run(name: &str) -> Outcome {
    let script = script(&fixture(name));
    let boot = bootstrap();
    let p = chain_params();
    let gov_params = GovParams::from_chain_params(&p).expect("its parameters");
    let id = local_identity(&p, &IdentityOptions::new()).expect("an identity");
    let late = Late::default();
    let mut registry = Registry::new();
    registry.register(Box::new(late.clone()));
    let seams = Seams {
        overlay: Arc::new(late.clone()),
        ..Seams::default()
    };
    let sim = Sim::with(Arc::clone(&p), id, ProcessorOptions::default(), registry, seams, |_| {});

    // dashd's chain before the recording: the bootstrap's blocks, connected.
    let blocks: Vec<Arc<Block>> = boot.blocks.iter().map(|raw| Arc::new(block(raw))).collect();
    let tip = sim.chain.store_blocks(&blocks);
    sim.chain.set_tip(tip.id);
    assert_eq!(tip.height, 179);

    let clock = Arc::clone(&sim.clock);
    let core: Arc<NetCore> = Arc::clone(&sim.core);
    let net = Arc::new(CoreSyncNet::new(Arc::clone(&core), false));
    net.bind_requests(sim.lp.logic().requested_objects());
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
    let tip_list = Arc::new(TipList(Mutex::new(Arc::new(MnList::default_pre_dip3()))));
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
    let exec = SimExec(Mutex::new(SimView {
        chain: Arc::clone(&sim.chain),
        tip: Arc::clone(&tip_list),
    }));
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
    let tweaks = Tweaks(Arc::new(Mutex::new(ReplayRng::new(
        script.tweaks.iter().map(|t| Draw::BloomTweak(*t)),
    ))));
    let seam = Arc::new(NodeGovernanceSync::new(
        Arc::clone(&handle),
        Arc::clone(&sync),
        tip_list.clone(),
        Box::new(tweaks.clone()),
        &gov_params,
    ));
    let peers = Arc::new(SimPeers {
        shared: Arc::clone(&sim.shared),
        net: net.clone(),
    });
    let gov = NetGovernance::new(
        Arc::clone(&handle),
        seam.clone(),
        tip_list.clone(),
        Arc::clone(&fulfilled),
        peers,
        Arc::clone(&relay),
        sink,
    );
    let sched = Arc::new(Scheduler::default());
    gov.schedule(&*sched, Box::new(ReplayRng::default()));
    let bound = Bound {
        net: Mutex::new(gov),
        seam: seam.clone(),
    };
    assert!(late.0.set(bound).is_ok());
    let tick_net = Arc::new(TickNet {
        inner: net,
        orders: Mutex::new(VecDeque::new()),
    });
    let cfg = SyncConfig {
        mockable_chain: true,
        mainnet: false,
        sync_mempool: true,
    };
    let tick = SyncManager::new(clock, cfg, Arc::clone(&sync), fulfilled, tick_net.clone(), seam);

    let genesis = header_bytes(&sim.chain.genesis().header);
    let mut node = Node {
        params: gov_params,
        chain: Arc::clone(&sim.chain),
        sync,
        env,
        handle,
        tip: tip_list,
        index,
        dml: Dml::new(Arc::clone(&p), genesis),
        tick,
        tick_net,
        sched,
        tweaks,
        submitted: BTreeMap::new(),
        told: 0,
        _scratch: scratch,
    };
    for raw in &boot.blocks {
        node.connect(raw);
    }
    assert_eq!(node.tip.dmn_tip_list().counts().enabled() as usize, boot.masternodes);
    for line in script.text.lines() {
        if let Some(hex) = line.strip_prefix("submitblock ") {
            let raw = unhex(hex);
            node.submitted.insert(block(&raw).hash(), raw);
        }
    }
    let cap = 2000 / boot.masternodes;
    let mut hooks = Replayer {
        name,
        node,
        script: &script,
        at: 0,
        scores: BTreeMap::new(),
        offset: None,
        cap,
        checkpoints: 0,
    };
    replay(name, &script.text, sim, &mut hooks);
    assert_eq!(hooks.at, script.segments.len(), "{name}: every segment settled");
    let left = hooks.node.tweaks.0.lock().unwrap_or_else(PoisonError::into_inner).remaining();
    assert_eq!(left, 0, "{name}: every recorded bloom tweak drawn");
    let checkpoints = hooks.checkpoints;
    Outcome {
        checkpoints,
        ..script.counts
    }
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
