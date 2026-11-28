//! A simulated network of DKG runtimes on one fake chain: a lockstep clock, ND1's queues and
//! batch wrapper as fakes, counting seams and a log capture per node.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use dash_consensus::primitives::P2pCommitment;
use dash_dml::Dmn;
use dash_dml::testkit::dmn_with;
use dash_evo::llmq::{LlmqCommitment, QuorumMembers};
use dash_evo::types::{MnType, ProTxHash};
use dash_llmq::dkg::bls::{BlsIesMultiBlobs, BlsPkValue, BlsSchemeKind, BlsSkValue};
use dash_llmq::dkg::conn::ConnSnapshot;
use dash_llmq::dkg::reference::{
    MemStore, ReferenceVerifier, TestMember, TestOperatorKey, node_seed, quorum,
    quorum_relay_members,
};
use dash_llmq::dkg::rng::{CoreRng, DkgRng, SeededRng};
use dash_llmq::dkg::session::{
    BatchOutcome, DkgEnv, DkgMsgKind, DkgSession, DkgStore, FinalVerifier, MissingVvec,
    OperatorKey, RelayTopology, Sender, VerifiedContributions,
};
use dash_llmq::dkg::wire::QuorumHash as WireQuorumHash;
use dash_llmq::qdata::PeerId;
use dash_params::{LlmqParams, LlmqType};
use dash_pkc::bls::BlsLazyPk;
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_quorums::LocalMasternode;
use dashd_quorums::dkg::seams::{
    BaseFacts, BatchProcessor, ConnFactsSource, ContributionsCache, DkgChain, DkgConnections,
    MembersAccess, MineableSink, PendingQueues, PhaseClock, Sporks, TipFacts, TipRef,
};
use dashd_quorums::dkg::{
    DebugJsonSource, DebugManager, DkgFailStop, DkgPorts, DkgRuntime, DkgSessions, RoundSeams,
    SessionKey, SessionSeams, SimRates,
};

/// Ticks between two blocks.
pub const TICKS_PER_BLOCK: u64 = 8;

/// The calls the counting seams saw, in order.
#[derive(Default)]
pub struct Trace(Mutex<Vec<String>>);

impl Trace {
    pub fn push(&self, what: impl Into<String>) {
        lock(&self.0).push(what.into());
    }

    pub fn get(&self) -> Vec<String> {
        lock(&self.0).clone()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The block at `height` on fork `fork`.
pub fn block_hash(height: i32, fork: u8) -> BlockHash {
    let mut bytes = [0xbb; 32];
    bytes[..4].copy_from_slice(&height.to_le_bytes());
    bytes[4] = fork;
    BlockHash::from(bytes)
}

pub fn tip(height: i32, fork: u8) -> TipFacts {
    TipRef {
        height,
        hash: block_hash(height, fork),
    }
}

/// `t`'s parameters as regtest sets them, with `size` members, `minSize` and the thresholds at
/// `threshold` (`-llmqtestparams=size:threshold`).
pub fn test_params(base: LlmqParams, size: u16, threshold: u16) -> LlmqParams {
    LlmqParams {
        size,
        min_size: threshold,
        threshold,
        dkg_bad_votes_threshold: threshold,
        ..base
    }
}

/// A chain whose fork `f > 0` branches from fork 0 at its first height.
pub struct SimChain {
    pub rotation: AtomicBool,
    forks: Mutex<Vec<i32>>,
    tip: Mutex<Option<TipFacts>>,
}

impl SimChain {
    fn new() -> Self {
        Self {
            rotation: AtomicBool::new(false),
            forks: Mutex::new(vec![0]),
            tip: Mutex::new(None),
        }
    }

    /// A new fork from `height`; its number.
    pub fn fork_at(&self, height: i32) -> u8 {
        let mut forks = lock(&self.forks);
        forks.push(height);
        u8::try_from(forks.len() - 1).expect("few forks")
    }
}

impl DkgChain for SimChain {
    fn snapshot_unvalidated(&self) -> bool {
        false
    }
    fn dip3_enforced(&self, _: i32) -> bool {
        true
    }
    fn lookup(&self, hash: &QuorumHash) -> Option<BaseFacts> {
        let b = hash.as_bytes();
        if b[5..].iter().any(|x| *x != 0xbb) {
            return None;
        }
        let height = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        Some(tip(height, b[4]))
    }
    fn ancestor_hash(&self, t: &TipFacts, height: i32) -> BlockHash {
        let fork = t.hash.as_bytes()[4];
        let from = lock(&self.forks)[usize::from(fork)];
        block_hash(
            height,
            if height >= from {
                fork
            } else {
                0
            },
        )
    }
    fn rotation_enabled(&self, params: &LlmqParams, _: &TipRef) -> bool {
        params.use_rotation && self.rotation.load(Ordering::SeqCst)
    }
    fn v19_after(&self, _: &TipRef) -> bool {
        true
    }
    fn current_tip(&self) -> Option<TipFacts> {
        *lock(&self.tip)
    }
}

/// What a lockstep step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// The check held; nothing moved.
    Done,
    /// One tick.
    Advanced,
    /// The threads ran again without a tick.
    Released,
}

struct ClockState {
    now_ms: u64,
    sleeping: usize,
    generation: u64,
    free: bool,
    progress: u64,
    seen_progress: u64,
    exited: usize,
}

/// A lockstep clock: every thread's tick parks it until the driver steps. A step advances the
/// clock by one tick only once every thread sleeps and no queue moved since the last step, so
/// each phase's messages are processed before the next tip.
pub struct SimClock {
    state: Mutex<ClockState>,
    cv: Condvar,
    spacing: i64,
    on_demand: bool,
    ticks_after_free: AtomicU64,
}

impl SimClock {
    fn new(spacing: i64, on_demand: bool) -> Self {
        Self {
            state: Mutex::new(ClockState {
                now_ms: 1_000_000,
                sleeping: 0,
                generation: 0,
                free: false,
                progress: 0,
                seen_progress: 0,
                exited: 0,
            }),
            cv: Condvar::new(),
            spacing,
            on_demand,
            ticks_after_free: AtomicU64::new(0),
        }
    }

    fn progress(&self) {
        lock(&self.state).progress += 1;
    }

    /// Waits until `threads` threads sleep; then, unless `done`, when nothing moved since the
    /// last step, advances one tick and runs `advance` before they wake.
    fn step(&self, threads: usize, done: impl FnOnce() -> bool, advance: impl FnOnce()) -> Step {
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut state = lock(&self.state);
        while state.sleeping + state.exited < threads {
            let (s, _) = self
                .cv
                .wait_timeout(state, Duration::from_millis(200))
                .unwrap_or_else(|p| p.into_inner());
            state = s;
            assert!(
                Instant::now() < deadline,
                "the simulation stalled: {} of {threads} threads sleep",
                state.sleeping
            );
        }
        drop(state);
        let finished = done();
        state = lock(&self.state);
        let advanced = !finished && state.progress == state.seen_progress;
        state.seen_progress = state.progress;
        if advanced {
            state.now_ms += 100;
            drop(state);
            advance();
            state = lock(&self.state);
        }
        state.sleeping = 0;
        state.generation += 1;
        self.cv.notify_all();
        if finished {
            Step::Done
        } else if advanced {
            Step::Advanced
        } else {
            Step::Released
        }
    }

    /// A thread ended at the fail-stop hook.
    fn exited(&self) {
        lock(&self.state).exited += 1;
        self.cv.notify_all();
    }

    /// From now on a tick returns at once.
    fn free_run(&self) {
        let mut state = lock(&self.state);
        state.free = true;
        self.cv.notify_all();
    }

    /// The ticks taken since [`Self::free_run`].
    pub fn ticks_after_free(&self) -> u64 {
        self.ticks_after_free.load(Ordering::SeqCst)
    }
}

impl PhaseClock for SimClock {
    fn steady_now(&self) -> Duration {
        Duration::from_millis(lock(&self.state).now_ms)
    }
    fn sleep_tick(&self) {
        let mut state = lock(&self.state);
        if state.free {
            state.now_ms += 100;
            self.ticks_after_free.fetch_add(1, Ordering::SeqCst);
            return;
        }
        state.sleeping += 1;
        let generation = state.generation;
        self.cv.notify_all();
        while state.generation == generation && !state.free {
            state = self.cv.wait(state).unwrap_or_else(|p| p.into_inner());
        }
    }
    fn pow_spacing_s(&self) -> i64 {
        self.spacing
    }
    fn mine_on_demand(&self) -> bool {
        self.on_demand
    }
}

const KINDS: [DkgMsgKind; 4] = [
    DkgMsgKind::Contribution,
    DkgMsgKind::Complaint,
    DkgMsgKind::Justification,
    DkgMsgKind::PrematureCommitment,
];

fn kind_index(kind: DkgMsgKind) -> usize {
    KINDS.iter().position(|k| *k == kind).expect("a DKG message kind")
}

/// One `CDKGPendingMessages`: FIFO, a seen set and Core's per-proTxHash quota.
#[derive(Default)]
struct Pending {
    messages: VecDeque<(Sender<PeerId>, Vec<u8>)>,
    per_pro_tx: BTreeMap<ProTxHash, usize>,
    seen: BTreeSet<u64>,
}

type QueueKey = (u8, i32, usize);

/// Every node's queues; an enqueued payload reaches every node.
pub struct SimNet {
    queues: Vec<Mutex<BTreeMap<QueueKey, Pending>>>,
    quotas: BTreeMap<u8, usize>,
    clock: Arc<SimClock>,
    probe: Arc<LockProbe>,
    /// Each node's own payloads, by kind.
    pub sent: Vec<Mutex<Vec<DkgMsgKind>>>,
}

fn queue_key(key: SessionKey, kind: DkgMsgKind) -> QueueKey {
    (key.llmq_type.to_base(), key.quorum_index, kind_index(kind))
}

impl SimNet {
    /// `PushPendingMessage` into node `to`'s queue.
    // Core: v24.0.0 src/llmq/dkgsessionhandler.cpp:29-56 CDKGPendingMessages::PushPendingMessage
    fn push(
        &self,
        to: usize,
        key: SessionKey,
        kind: DkgMsgKind,
        from: Sender<PeerId>,
        sender: &ProTxHash,
        payload: &[u8],
    ) {
        let mut h = DefaultHasher::new();
        payload.hash(&mut h);
        let hash = h.finish();
        let quota = self.quotas.get(&key.llmq_type.to_base()).copied().unwrap_or(0);
        let mut queues = lock(&self.queues[to]);
        let pending = queues.entry(queue_key(key, kind)).or_default();
        if pending.seen.contains(&hash) {
            return;
        }
        let count = pending.per_pro_tx.entry(*sender).or_default();
        if *count >= quota {
            return;
        }
        *count += 1;
        pending.seen.insert(hash);
        pending.messages.push_back((from, payload.to_vec()));
    }
}

/// Node `me`'s view of the queues.
pub struct SimQueues {
    net: Arc<SimNet>,
    me: usize,
}

impl PendingQueues for SimQueues {
    fn clear(&self, key: SessionKey) {
        self.net.probe.check("clear");
        let mut queues = lock(&self.net.queues[self.me]);
        for kind in KINDS {
            queues.remove(&queue_key(key, kind));
        }
    }
    fn pop_batch(
        &self,
        key: SessionKey,
        kind: DkgMsgKind,
        max: usize,
    ) -> Vec<(Sender<PeerId>, Vec<u8>)> {
        self.net.probe.check("pop_batch");
        let popped: Vec<_> = {
            let mut queues = lock(&self.net.queues[self.me]);
            let Some(pending) = queues.get_mut(&queue_key(key, kind)) else {
                return Vec::new();
            };
            let take = pending.messages.len().min(max);
            pending.messages.drain(..take).collect()
        };
        if !popped.is_empty() {
            self.net.clock.progress();
        }
        popped
    }
    fn enqueue_own(&self, key: SessionKey, kind: DkgMsgKind, own: &ProTxHash, payload: Vec<u8>) {
        self.net.probe.check("enqueue_own");
        lock(&self.net.sent[self.me]).push(kind);
        for to in 0..self.net.queues.len() {
            let from = if to == self.me {
                Sender::Own
            } else {
                Sender::Peer(self.me as PeerId)
            };
            self.net.push(to, key, kind, from, own, &payload);
        }
        self.net.clock.progress();
    }
}

/// ND1's batch wrapper: the session's batch, its scores kept.
pub struct SimBatches {
    pub misbehaving: Mutex<usize>,
    probe: Arc<LockProbe>,
}

impl BatchProcessor for SimBatches {
    fn process_batch(
        &self,
        session: &mut DkgSession<Arc<Dmn>>,
        env: &mut DkgEnv<'_>,
        kind: DkgMsgKind,
        batch: Vec<(Sender<PeerId>, Vec<u8>)>,
    ) {
        self.probe.check("process_batch");
        let refs: Vec<(Sender<PeerId>, &[u8])> = batch.iter().map(|(s, p)| (*s, &p[..])).collect();
        let BatchOutcome {
            misbehaving,
            events,
            ..
        } = session.process_batch(kind, &refs, env);
        dashd_quorums::dkg::round::log_events(&events);
        *lock(&self.misbehaving) += misbehaving.len();
    }
}

/// The members of every quorum, counted as access #1.
pub struct SimMembers {
    pub members: BTreeMap<u8, QuorumMembers<Arc<Dmn>>>,
    /// Bases at which the members are too few.
    pub short_at: Mutex<BTreeSet<i32>>,
    pub trace: Arc<Trace>,
    probe: Arc<LockProbe>,
}

impl MembersAccess for SimMembers {
    fn all_members(&self, t: LlmqType, base: &BaseFacts) -> QuorumMembers<Arc<Dmn>> {
        self.probe.check("all_members");
        self.trace.push("access#1");
        let all = self.members[&t.to_base()].clone();
        if lock(&self.short_at).contains(&base.height) {
            return all[..1].to_vec().into();
        }
        all
    }
}

/// `GetQuorumRelayMembers` over the quorum's members (access #2).
struct SimTopology<'a> {
    members: &'a [ProTxHash],
    trace: &'a Trace,
    probe: &'a LockProbe,
}

impl RelayTopology for SimTopology<'_> {
    fn relay_members(
        &mut self,
        _: LlmqType,
        _: &WireQuorumHash,
        me: &ProTxHash,
    ) -> BTreeSet<ProTxHash> {
        self.probe.check("relay_members");
        self.trace.push("access#2");
        quorum_relay_members(self.members, me, true)
    }
}

/// dkgdb: a shared in-memory store, `&SimStore` as DK1-db's `&DkgDb`.
pub struct SimStore {
    store: Mutex<MemStore>,
    trace: Arc<Trace>,
    probe: Arc<LockProbe>,
    panics: bool,
}

impl DkgStore for &SimStore {
    fn write_vvec(
        &mut self,
        t: LlmqType,
        base: &WireQuorumHash,
        who: &ProTxHash,
        vvec: &[BlsPkValue],
    ) {
        lock(&self.store).write_vvec(t, base, who, vvec);
    }
    fn write_sk(&mut self, t: LlmqType, base: &WireQuorumHash, who: &ProTxHash, sk: &BlsSkValue) {
        lock(&self.store).write_sk(t, base, who, sk);
    }
    fn write_encrypted(
        &mut self,
        t: LlmqType,
        base: &WireQuorumHash,
        who: &ProTxHash,
        blobs: &BlsIesMultiBlobs,
    ) {
        lock(&self.store).write_encrypted(t, base, who, blobs);
    }
    fn verified_contributions(
        &mut self,
        t: LlmqType,
        base: &WireQuorumHash,
        valid: &[bool],
    ) -> Result<VerifiedContributions, MissingVvec> {
        self.probe.check("verified_contributions");
        assert!(!self.panics, "dkgdb read failed");
        self.trace.push("access#3/4");
        lock(&self.store).verified_contributions(t, base, valid)
    }
}

/// `CFinalCommitment::Verify` by the reference verifier at the base's height (accesses #5 and
/// #6, counted from its reads).
pub struct SimVerify {
    t: LlmqType,
    params: LlmqParams,
    members: Vec<TestMember>,
    accept_all: bool,
    reject_all: bool,
    trace: Arc<Trace>,
    probe: Arc<LockProbe>,
}

impl FinalVerifier for &SimVerify {
    fn verify(&mut self, base: &WireQuorumHash, qc: &P2pCommitment) -> bool {
        self.probe.check("verify");
        if self.reject_all {
            self.trace.push("verify");
            return false;
        }
        if self.accept_all {
            self.trace.push("verify");
            return true;
        }
        let height = i32::from_le_bytes(base.as_bytes()[..4].try_into().expect("4 bytes"));
        let mut verifier = ReferenceVerifier::new(
            self.t,
            &self.params,
            u32::try_from(height).expect("a height"),
            &self.members,
        );
        verifier.v19_after_base = true;
        verifier.dip24 = self.params.use_rotation;
        let ok = verifier.verify(base, qc);
        for i in 0..verifier.member_reads()[0] {
            self.trace.push(format!("access#{}", 5 + i));
        }
        ok
    }
}

/// A node's environment objects.
pub struct SimPorts {
    key: TestOperatorKey,
    store: SimStore,
    verify: SimVerify,
    pro_txs: Vec<ProTxHash>,
    seed: u64,
    idx: usize,
    rounds: AtomicU64,
    trace: Arc<Trace>,
    probe: Arc<LockProbe>,
}

impl DkgPorts for SimPorts {
    fn topology(&self) -> Box<dyn RelayTopology + '_> {
        Box::new(SimTopology {
            members: &self.pro_txs,
            trace: &self.trace,
            probe: &self.probe,
        })
    }
    fn store(&self) -> Box<dyn DkgStore + '_> {
        Box::new(&self.store)
    }
    fn verifier(&self) -> Box<dyn FinalVerifier + '_> {
        Box::new(&self.verify)
    }
    fn operator_key(&self) -> &dyn OperatorKey {
        &self.key
    }
    fn session_rng(&self, _: SessionKey) -> Box<dyn DkgRng + Send> {
        let round = self.rounds.fetch_add(1, Ordering::SeqCst);
        Box::new(CoreRng(SeededRng::new(node_seed(self.seed ^ (round << 48), self.idx))))
    }
}

/// One `EnsureQuorumConnections`: type, base height, `ProTx()`, watch, member.
pub type ConnCall = (LlmqType, i32, ProTxHash, bool, bool);

/// `EnsureQuorumConnections` and the probes, counted.
pub struct SimConnections {
    pub calls: Mutex<Vec<ConnCall>>,
    /// Whether each call's thread carried LO-9's no-wait marker.
    pub marked: Mutex<Vec<bool>>,
    trace: Arc<Trace>,
    probe: Arc<LockProbe>,
}

impl DkgConnections for SimConnections {
    fn on_initialized(
        &self,
        llmq_type: LlmqType,
        _: &LlmqParams,
        quorum_base: &TipRef,
        my_pro_tx: &ProTxHash,
        watch: bool,
        we_are_member: bool,
    ) {
        self.probe.check("on_initialized");
        lock(&self.marked).push(dashd_chainstate::chain::no_wait_marked());
        self.trace.push("ensure");
        if we_are_member {
            self.trace.push("probes");
        }
        lock(&self.calls).push((llmq_type, quorum_base.height, *my_pro_tx, watch, we_are_member));
    }
}

pub struct SimConnFacts {
    trace: Arc<Trace>,
    probe: Arc<LockProbe>,
}

impl ConnFactsSource for SimConnFacts {
    fn snapshot(&self, _: LlmqType, _: &[ProTxHash]) -> ConnSnapshot {
        self.probe.check("conn_snapshot");
        self.trace.push("conn_snapshot");
        ConnSnapshot::default()
    }
}

/// The mineable pool: a commitment the first time it is added, by its serialization.
pub struct SimMineable {
    pub added: Mutex<Vec<LlmqCommitment>>,
    pub relayed: Mutex<Vec<[u8; 32]>>,
    probe: Arc<LockProbe>,
}

pub fn commitment_bytes(qc: &LlmqCommitment) -> Vec<u8> {
    let mut out = Vec::new();
    qc.encode(&mut out);
    out
}

impl MineableSink for SimMineable {
    fn add(&self, qc: &P2pCommitment) -> Option<[u8; 32]> {
        self.probe.check("mineable_add");
        let mut added = lock(&self.added);
        if added.contains(qc) {
            return None;
        }
        added.push((**qc).clone());
        let mut h = DefaultHasher::new();
        commitment_bytes(qc).hash(&mut h);
        let mut hash = [0u8; 32];
        hash[..8].copy_from_slice(&h.finish().to_le_bytes());
        Some(hash)
    }
    fn relay_inv(&self, hash: &[u8; 32]) {
        self.probe.check("relay_inv");
        lock(&self.relayed).push(*hash);
    }
}

pub struct SimFailStop {
    pub faults: Mutex<Vec<(SessionKey, String)>>,
    clock: Arc<SimClock>,
}

impl DkgFailStop for SimFailStop {
    fn fail_stop(&self, key: SessionKey, why: &str) {
        lock(&self.faults).push((key, why.to_owned()));
        self.clock.exited();
    }
}

/// Checks from another thread that no runtime lock is held: a seam call runs it and waits for
/// the handlers' phase, a lookup and every debug status.
#[derive(Default)]
pub struct LockProbe {
    pub enabled: AtomicBool,
    nodes: OnceLock<Vec<(Arc<DkgSessions>, Arc<DebugManager>)>>,
    pub held: Mutex<Vec<String>>,
    pub checks: AtomicU64,
}

struct AnyType;

impl DebugJsonSource for AnyType {
    fn has_llmq(&self, _: LlmqType) -> bool {
        true
    }
    fn members(&self, _: LlmqType, _: &QuorumHash) -> Option<Vec<ProTxHash>> {
        None
    }
    fn format_time(&self, time: i64) -> String {
        time.to_string()
    }
}

impl LockProbe {
    fn check(&self, at: &str) {
        if !self.enabled.load(Ordering::SeqCst) {
            return;
        }
        let Some(nodes) = self.nodes.get().cloned() else {
            return;
        };
        self.checks.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (sessions, debug) in &nodes {
                for key in sessions.keys() {
                    let _ = sessions.phase(key);
                }
                let _ = sessions.get_contribution(&[0; 32]);
                let _ = sessions.get_complaint(&[0; 32]);
                let _ = sessions.get_justification(&[0; 32]);
                let _ = sessions.get_premature_commitment(&[0; 32]);
                let _ = debug.to_json(2, &AnyType);
            }
            let _ = tx.send(());
        });
        if rx.recv_timeout(Duration::from_secs(20)).is_err() {
            lock(&self.held).push(at.to_owned());
        }
    }
}

struct Active(Option<ProTxHash>);

impl LocalMasternode for Active {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.0
    }
}

struct SporksOn;

impl Sporks for SporksOn {
    fn quorum_dkg_enabled(&self) -> bool {
        true
    }
    fn spork21_value(&self) -> i64 {
        0
    }
    fn spork23_value(&self) -> i64 {
        0
    }
}

struct NoCache;

impl ContributionsCache for NoCache {
    fn cleanup_cache(&self) {}
}

/// A captured line: its level, target and text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub level: tracing::Level,
    pub target: String,
    pub text: String,
}

/// A subscriber that keeps every event.
#[derive(Default)]
pub struct Capture(Mutex<Vec<Line>>);

impl Capture {
    pub fn lines(&self) -> Vec<Line> {
        lock(&self.0).clone()
    }

    /// The `llmq-dkg` lines' texts.
    pub fn dkg_lines(&self) -> Vec<String> {
        self.lines().into_iter().filter(|l| l.target == "llmq-dkg").map(|l| l.text).collect()
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn fmt::Debug) {
                if f.name() == "message" {
                    self.0.push_str(&format!("{v:?}"));
                }
            }
        }
        let mut m = Message(String::new());
        event.record(&mut m);
        lock(&self.0).push(Line {
            level: *event.metadata().level(),
            target: event.metadata().target().to_owned(),
            text: m.0,
        });
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Runs `f` with `capture` as this thread's subscriber, never the only registered one (BL-479).
pub fn with_capture<T>(capture: &Arc<Capture>, f: impl FnOnce() -> T) -> T {
    static KEEP: OnceLock<tracing::Dispatch> = OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    tracing::subscriber::with_default(Arc::clone(capture), f)
}

/// One node: a runtime with its seams.
pub struct Node {
    pub runtime: DkgRuntime,
    pub sessions: Arc<DkgSessions>,
    pub debug: Arc<DebugManager>,
    pub mineable: Arc<SimMineable>,
    pub connections: Arc<SimConnections>,
    pub members: Arc<SimMembers>,
    pub batches: Arc<SimBatches>,
    pub fail: Arc<SimFailStop>,
    pub trace: Arc<Trace>,
    pub capture: Arc<Capture>,
    pub pro_tx: ProTxHash,
    seams: RoundSeams,
    session_seams: SessionSeams,
}

impl Node {
    pub fn round_seams(&self) -> RoundSeams {
        self.seams.clone()
    }

    pub fn sessions_seams(&self) -> SessionSeams {
        self.session_seams.clone()
    }
}

/// What a simulation runs.
#[derive(Clone)]
pub struct SimConfig {
    /// The types the nodes run, each with its members' count.
    pub llmqs: Vec<(LlmqType, LlmqParams)>,
    /// The members of every quorum.
    pub members: usize,
    /// Nodes that run outside the quorums.
    pub non_members: usize,
    pub seed: u64,
    pub spacing: i64,
    pub on_demand: bool,
    pub start_height: i32,
    /// Accept every final commitment instead of the reference verifier's verdict.
    pub accept_all: bool,
    /// Refuse every final commitment.
    pub reject_all: bool,
    /// The node whose dkgdb read panics.
    pub store_panics: Option<usize>,
}

/// Type `t`'s parameters on regtest.
pub fn chain_llmq(t: LlmqType) -> LlmqParams {
    let setup = dash_params::ChainParamsBuilder::new(
        dash_params::Network::Regtest,
        dash_params::Release::V24_0,
    )
    .build()
    .expect("regtest's chain parameters");
    setup.consensus.llmq(t).copied().expect("regtest runs the type")
}

impl SimConfig {
    pub fn llmq_test(members: usize, seed: u64) -> Self {
        let base = chain_llmq(LlmqType::LlmqTest);
        let params = if members == 3 {
            base
        } else {
            test_params(base, members as u16, members as u16 / 2 + 1)
        };
        Self {
            llmqs: vec![(LlmqType::LlmqTest, params)],
            members,
            non_members: 0,
            seed,
            spacing: 60,
            on_demand: false,
            start_height: 47,
            accept_all: false,
            reject_all: false,
            store_panics: None,
        }
    }
}

/// The member `i` of [`quorum`]`(n, seed)` as a masternode list entry.
pub fn dmn_of(i: usize, m: &TestMember) -> Arc<Dmn> {
    let pk = m.operator_pk.clone();
    dmn_with(u8::try_from(i + 1).expect("few members"), MnType::Regular, |dmn, cold| {
        dmn.pro_tx_hash = m.pro_tx_hash;
        cold.pub_key_operator = BlsLazyPk::from_value(pk, BlsSchemeKind::Basic);
    })
}

/// A network of nodes on one chain.
pub struct Sim {
    pub probe: Arc<LockProbe>,
    pub cfg: SimConfig,
    pub clock: Arc<SimClock>,
    pub chain: Arc<SimChain>,
    pub net: Arc<SimNet>,
    pub nodes: Vec<Node>,
    pub test_members: Vec<TestMember>,
    pub height: i32,
    pub fork: u8,
    pub ticks: u64,
    threads: usize,
    started: bool,
}

impl Sim {
    pub fn new(cfg: SimConfig) -> Self {
        let clock = Arc::new(SimClock::new(cfg.spacing, cfg.on_demand));
        let probe = Arc::new(LockProbe::default());
        let chain = Arc::new(SimChain::new());
        let (test_members, keys) = quorum(cfg.members, cfg.seed);
        let (_, extra_keys) = quorum(cfg.non_members, cfg.seed ^ 0xdead_beef);
        let dmns: Vec<Arc<Dmn>> =
            test_members.iter().enumerate().map(|(i, m)| dmn_of(i, m)).collect();
        let pro_txs: Vec<ProTxHash> = test_members.iter().map(|m| m.pro_tx_hash).collect();
        let n = cfg.members + cfg.non_members;
        let net = Arc::new(SimNet {
            queues: (0..n).map(|_| Mutex::new(BTreeMap::new())).collect(),
            quotas: cfg.llmqs.iter().map(|(t, p)| (t.to_base(), usize::from(p.size) * 2)).collect(),
            clock: clock.clone(),
            probe: probe.clone(),
            sent: (0..n).map(|_| Mutex::new(Vec::new())).collect(),
        });
        let members: BTreeMap<u8, QuorumMembers<Arc<Dmn>>> =
            cfg.llmqs.iter().map(|(t, _)| (t.to_base(), dmns.clone().into())).collect();
        let mut nodes = Vec::new();
        for idx in 0..n {
            let (key, pro_tx) = if idx < cfg.members {
                (keys[idx].clone(), pro_txs[idx])
            } else {
                let mut bytes = [0xee; 32];
                bytes[0] = idx as u8;
                (extra_keys[idx - cfg.members].clone(), ProTxHash::from(bytes))
            };
            let trace = Arc::new(Trace::default());
            let (t, params) = cfg.llmqs[0];
            let ports = Arc::new(SimPorts {
                key,
                store: SimStore {
                    store: Mutex::new(MemStore::with_members(pro_txs.clone())),
                    trace: trace.clone(),
                    probe: probe.clone(),
                    panics: cfg.store_panics == Some(idx),
                },
                verify: SimVerify {
                    t,
                    params,
                    members: test_members.clone(),
                    accept_all: cfg.accept_all,
                    reject_all: cfg.reject_all,
                    trace: trace.clone(),
                    probe: probe.clone(),
                },
                pro_txs: pro_txs.clone(),
                seed: cfg.seed,
                idx,
                rounds: AtomicU64::new(0),
                trace: trace.clone(),
                probe: probe.clone(),
            });
            let session_seams = SessionSeams {
                chain: chain.clone(),
                sporks: Arc::new(SporksOn),
                cache: Arc::new(NoCache),
            };
            let sessions = Arc::new(DkgSessions::new(cfg.llmqs.clone(), session_seams.clone()));
            let debug = Arc::new(DebugManager::new(cfg.llmqs.clone(), || 1_700_000_000));
            let mineable = Arc::new(SimMineable {
                added: Mutex::new(Vec::new()),
                relayed: Mutex::new(Vec::new()),
                probe: probe.clone(),
            });
            let connections = Arc::new(SimConnections {
                calls: Mutex::new(Vec::new()),
                marked: Mutex::new(Vec::new()),
                trace: trace.clone(),
                probe: probe.clone(),
            });
            let sim_members = Arc::new(SimMembers {
                members: members.clone(),
                short_at: Mutex::new(BTreeSet::new()),
                trace: trace.clone(),
                probe: probe.clone(),
            });
            let batches = Arc::new(SimBatches {
                misbehaving: Mutex::new(0),
                probe: probe.clone(),
            });
            let fail = Arc::new(SimFailStop {
                faults: Mutex::new(Vec::new()),
                clock: clock.clone(),
            });
            let seams = RoundSeams {
                chain: chain.clone(),
                members: sim_members.clone(),
                active: Arc::new(Active(Some(pro_tx))),
                ports,
                queues: Arc::new(SimQueues {
                    net: net.clone(),
                    me: idx,
                }),
                batches: batches.clone(),
                connections: connections.clone(),
                conn_facts: Arc::new(SimConnFacts {
                    trace: trace.clone(),
                    probe: probe.clone(),
                }),
                mineable: mineable.clone(),
                clock: clock.clone(),
                sim: Arc::new(SimRates::default()),
                debug: debug.clone(),
                fail_stop: fail.clone(),
                watch: false,
            };
            nodes.push(Node {
                runtime: DkgRuntime::new(sessions.clone(), seams.clone()),
                seams,
                session_seams,
                sessions,
                debug,
                mineable,
                connections,
                members: sim_members,
                batches,
                fail,
                trace,
                capture: Arc::new(Capture::default()),
                pro_tx,
            });
        }
        let _ =
            probe.nodes.set(nodes.iter().map(|n| (n.sessions.clone(), n.debug.clone())).collect());
        let height = cfg.start_height;
        let sim = Self {
            probe,
            cfg,
            clock,
            chain,
            net,
            nodes,
            test_members,
            height,
            fork: 0,
            ticks: 0,
            threads: 0,
            started: false,
        };
        sim.deliver();
        sim
    }

    /// The tip to every node.
    fn deliver(&self) {
        deliver(&self.chain, &self.nodes, &tip(self.height, self.fork));
    }

    /// Starts every node's threads, each under its node's capture.
    pub fn start(&mut self) {
        for node in &self.nodes {
            with_capture(&node.capture, || node.runtime.start()).expect("a first start");
            self.threads += node.sessions.handlers().len();
        }
        self.started = true;
    }

    /// One lockstep step: `done` checked while every thread sleeps; else a tick when nothing
    /// moved, with the next block every [`TICKS_PER_BLOCK`] ticks. Whether `done` held.
    fn step_checked(&mut self, done: &mut dyn FnMut(&Sim) -> bool) -> bool {
        let clock = self.clock.clone();
        let next_tick = self.ticks + 1;
        let block = next_tick.is_multiple_of(TICKS_PER_BLOCK);
        let next = tip(self.height + 1, self.fork);
        let this = &*self;
        let outcome = clock.step(
            this.threads,
            || done(this),
            || {
                if block {
                    deliver(&this.chain, &this.nodes, &next);
                }
            },
        );
        match outcome {
            Step::Done => return true,
            Step::Advanced => {
                self.ticks = next_tick;
                if block {
                    self.height += 1;
                }
            }
            Step::Released => {}
        }
        false
    }

    /// One step.
    pub fn step(&mut self) {
        self.step_checked(&mut |_| false);
    }

    /// Steps until `done` holds with every thread asleep, within `max_blocks` more blocks.
    pub fn run_until(&mut self, max_blocks: i32, mut done: impl FnMut(&Sim) -> bool) {
        let last = self.height + max_blocks;
        while !self.step_checked(&mut done) {
            assert!(self.height <= last, "not done by height {last}");
        }
    }

    /// Steps until the chain is at `height`, the last tick before the next block, every
    /// thread asleep.
    pub fn run_to(&mut self, height: i32) {
        let blocks = height - self.height + 1;
        self.run_until(blocks, |s| {
            s.height == height && s.ticks % TICKS_PER_BLOCK == TICKS_PER_BLOCK - 1
        });
    }

    /// A reorg: a new fork from `from`, and its tip at the current height delivered.
    pub fn reorg_from(&mut self, from: i32) {
        self.fork = self.chain.fork_at(from);
        let now = tip(self.height, self.fork);
        let clock = self.clock.clone();
        let this = &*self;
        while clock.step(this.threads, || false, || deliver(&this.chain, &this.nodes, &now))
            != Step::Advanced
        {}
        self.ticks += 1;
    }

    /// Every member node mined a commitment.
    pub fn all_mined(&self, count: usize) -> bool {
        self.nodes[..self.cfg.members].iter().all(|n| lock(&n.mineable.added).len() >= count)
    }

    /// Interrupts every node, lets the threads run free and joins them.
    pub fn stop(&mut self) {
        if !self.started {
            return;
        }
        for node in &self.nodes {
            node.runtime.interrupt();
        }
        self.clock.free_run();
        for node in &self.nodes {
            node.runtime.stop();
        }
        self.started = false;
    }
}

/// `tip` as the chain's tip, then to every node.
fn deliver(chain: &SimChain, nodes: &[Node], tip: &TipFacts) {
    *lock(&chain.tip) = Some(*tip);
    for node in nodes {
        node.sessions.updated_block_tip(tip, false);
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn mined(node: &Node) -> Vec<LlmqCommitment> {
    lock(&node.mineable.added).clone()
}

pub fn sent(sim: &Sim, idx: usize) -> Vec<DkgMsgKind> {
    lock(&sim.net.sent[idx]).clone()
}

pub fn faults(node: &Node) -> Vec<(SessionKey, String)> {
    lock(&node.fail.faults).clone()
}

pub fn misbehaving(node: &Node) -> usize {
    *lock(&node.batches.misbehaving)
}

pub fn short_at(node: &Node, height: i32) {
    lock(&node.members.short_at).insert(height);
}

pub fn connection_calls(node: &Node) -> Vec<ConnCall> {
    lock(&node.connections.calls).clone()
}

/// The quorum hash of the base at `height` on fork `fork`.
pub fn quorum_hash(height: i32, fork: u8) -> QuorumHash {
    QuorumHash::from(*block_hash(height, fork).as_bytes())
}
