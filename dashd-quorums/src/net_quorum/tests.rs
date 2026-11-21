use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dash_consensus::primitives::TxHash;
use dash_kv::{
    Cf, Commit, DbSchema, ErrorKind, KvEngine, KvRead, MemoryEngine, Result as KvResult,
};
use dash_llmq::dkg::bls::{BlsPkValue, BlsSchemeKind, BlsSkValue, secret_to_public};
use dash_llmq::dkg::wire::{BlsIesBlobBytes, QuorumDataError, QuorumHash};
use dash_llmq::qdata::recovery::{Claim, Plan};
use dash_llmq::qdata::tracker::{Entry, Key};
use dash_llmq::qdata::{QuorumRef, TipRef};
use dash_p2p::ConnectionType;
use dash_p2p::conn::Permissions;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::{SIM_START, Sim, SimPeer};
use dashd_p2p::{ChainAccess, Registry, Seams};

use super::*;
use crate::store::{KvQuorumStore, QUORUMDB_SCHEMA};

const LLMQ: LlmqType = LlmqType::LlmqTest;
const VVEC: u16 = Request::QUORUM_VERIFICATION_VECTOR;
const CONTRIB: u16 = Request::ENCRYPTED_CONTRIBUTIONS;
const QHASH: [u8; 32] = [0x51; 32];
const OFF_CHAIN: [u8; 32] = [0x52; 32];
const NO_QUORUM: [u8; 32] = [0x53; 32];
const BROKEN: [u8; 32] = [0x54; 32];
/// The quorum's members: a peer masternode, this node, another member.
const PEER_MN: [u8; 32] = [0x0a; 32];
const ME: [u8; 32] = [0x0b; 32];
const OTHER: [u8; 32] = [0x0c; 32];
const DB_KEY: QuorumDbKey = QuorumDbKey([0x77; 32]);
/// The contributions dealt to this node, one per valid member: their ciphertexts and shares.
const BLOBS: [u8; 3] = [21, 22, 23];
const MALLEABLE: &str = "malleable BLS object: iostream error";
const IOS_FAILURE: &str = "NSt8ios_base7failureB5cxx11E";

// ---- fakes

fn sk(n: u8) -> BlsSkValue {
    let mut bytes = [0; 32];
    bytes[31] = n;
    BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).expect("canonical scalar")
}

fn pk(n: u8) -> BlsPkValue {
    secret_to_public(&sk(n))
}

#[derive(Default)]
struct QuorumState {
    threshold: usize,
    members: Vec<ProTxHash>,
    valid: Vec<bool>,
    vvec: Mutex<Option<Arc<Vec<BlsPkValue>>>>,
    sk_share: Mutex<Option<BlsSkValue>>,
    claimed: AtomicBool,
    warmed: Arc<AtomicUsize>,
    probe: OnceLock<Arc<Probe>>,
}

#[derive(Clone)]
struct TestQuorum(Arc<QuorumState>);

impl TestQuorum {
    fn new() -> Self {
        Self(Arc::new(QuorumState {
            threshold: 2,
            members: [PEER_MN, ME, OTHER].map(ProTxHash::from).to_vec(),
            valid: vec![true; 3],
            ..QuorumState::default()
        }))
    }

    fn with_vvec(self) -> Self {
        *self.0.vvec.lock().unwrap() = Some(Arc::new(vec![pk(1), pk(2)]));
        self
    }

    fn sk_share(&self) -> Option<BlsSkValue> {
        self.0.sk_share.lock().unwrap().clone()
    }
}

impl QuorumData for TestQuorum {
    fn threshold(&self) -> usize {
        self.0.threshold
    }

    fn valid_count(&self) -> usize {
        self.0.valid.iter().filter(|v| **v).count()
    }

    fn size(&self) -> usize {
        self.0.valid.len()
    }

    fn member_index(&self, p: &ProTxHash) -> Option<usize> {
        self.0.members.iter().position(|m| m == p)
    }

    fn is_valid_member(&self, p: &ProTxHash) -> bool {
        self.member_index(p).is_some_and(|i| self.0.valid[i])
    }

    fn members(&self) -> &[ProTxHash] {
        &self.0.members
    }

    fn valid(&self) -> &[bool] {
        &self.0.valid
    }

    fn vvec(&self) -> Option<Arc<Vec<BlsPkValue>>> {
        self.0.vvec.lock().unwrap().clone()
    }

    fn has_sk_share(&self) -> bool {
        self.0.sk_share.lock().unwrap().is_some()
    }

    fn set_vvec(&self, v: Vec<BlsPkValue>, _: BlsSchemeKind) -> bool {
        *self.0.vvec.lock().unwrap() = Some(Arc::new(v));
        true
    }

    // Core: v24.0.0 src/llmq/quorums.cpp:129-137 CQuorum::SetSecretKeyShare (a null proTxHash)
    fn set_sk_share(&self, sk: BlsSkValue, me: &ProTxHash) -> bool {
        if let Some(probe) = self.0.probe.get() {
            probe.at("set_sk_share");
        }
        if *me == ProTxHash::default() {
            return false;
        }
        *self.0.sk_share.lock().unwrap() = Some(sk);
        true
    }

    fn try_claim_recovery(&self) -> bool {
        !self.0.claimed.swap(true, Ordering::SeqCst)
    }

    fn release_recovery(&self) {
        self.0.claimed.store(false, Ordering::SeqCst);
    }

    fn recovery_running(&self) -> bool {
        self.0.claimed.load(Ordering::SeqCst)
    }
}

impl StoredQuorum for TestQuorum {
    fn db_key(&self) -> QuorumDbKey {
        DB_KEY
    }

    fn contributions(&self) -> StoredContributions {
        StoredContributions {
            vvec: Ok(self.vvec().map(|v| (*v).clone())),
            sk_share: self.sk_share().filter(BlsSkValue::is_valid),
        }
    }

    fn warm_job(&self) -> Box<dyn FnOnce() + Send> {
        let warmed = Arc::clone(&self.0.warmed);
        Box::new(move || {
            warmed.fetch_add(1, Ordering::SeqCst);
        })
    }
}

#[derive(Debug)]
struct ReadFailed;

impl fmt::Display for ReadFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MALLEABLE)
    }
}

impl CaughtException for ReadFailed {
    fn type_name(&self) -> &'static str {
        IOS_FAILURE
    }
}

/// The quorum at `QHASH`, mined and cached; `NO_QUORUM` is an active block with no commitment;
/// `BROKEN`'s build fails its read.
struct TestSource {
    quorum: TestQuorum,
    probe: Arc<Probe>,
}

impl QuorumDataSource for TestSource {
    type Q = TestQuorum;
    type Error = ReadFailed;

    fn llmq_registered(&self, t: LlmqType) -> bool {
        t == LLMQ
    }

    fn active_block(&self, h: &BlockHash) -> bool {
        [QHASH, NO_QUORUM, BROKEN].contains(h.as_bytes())
    }

    fn quorum(&self, t: LlmqType, h: &BlockHash) -> Result<Option<TestQuorum>, ReadFailed> {
        self.probe.at("quorum");
        if *h.as_bytes() == BROKEN {
            return Err(ReadFailed);
        }
        Ok((t == LLMQ && *h.as_bytes() == QHASH).then(|| self.quorum.clone()))
    }

    fn cached_quorum(&self, t: LlmqType, h: &BlockHash) -> Option<TestQuorum> {
        self.probe.at("cached_quorum");
        (t == LLMQ && *h.as_bytes() == QHASH).then(|| self.quorum.clone())
    }
}

fn blob(n: u8, scheme: BlsSchemeKind) -> BlsIesBlobBytes {
    BlsIesBlobBytes::new(pk(n).to_bytes(scheme).into(), [n; 32], vec![n; 32])
}

/// The dkgdb rows for `PEER_MN`, written under the request's scheme.
struct TestDkg(Arc<Probe>);

impl EncryptedContributions for TestDkg {
    fn get(
        &self,
        _: LlmqType,
        _: &BlockHash,
        _: &[bool],
        p: &ProTxHash,
        scheme: BlsSchemeKind,
    ) -> Option<Vec<BlsIesBlobBytes>> {
        self.0.at("contributions");
        (*p == ProTxHash::from(PEER_MN)).then(|| BLOBS.map(|n| blob(n, scheme)).to_vec())
    }
}

/// This node's operator key: each blob's ciphertext `[n; 32]` decrypts to `sk(n)`.
struct TestOperator(Arc<Probe>);

impl OperatorDecrypt for TestOperator {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        Some(ProTxHash::from(ME))
    }

    fn decrypt_sk(&self, b: &BlsIesBlobBytes, _: usize, _: BlsSchemeKind) -> Option<BlsSkValue> {
        self.0.at("decrypt_sk");
        Some(sk(b.data()[0]))
    }
}

struct TestMns;

impl ValidMnsAt for TestMns {
    fn valid_pro_tx_hashes(&self, _: &BlockHash) -> Option<Vec<ProTxHash>> {
        Some([PEER_MN, ME, OTHER].map(ProTxHash::from).to_vec())
    }
}

/// Once armed, tries at each fake's call to take the tracker from another thread: a lock held
/// there shows as `false`.
#[derive(Default)]
struct Probe {
    nq: OnceLock<Weak<NqInner>>,
    seen: Mutex<Vec<(&'static str, bool)>>,
}

impl Probe {
    fn arm(&self, nq: &Nq) {
        assert!(self.nq.set(Arc::downgrade(&nq.inner)).is_ok());
    }

    fn at(&self, site: &'static str) {
        let Some(inner) = self.nq.get().and_then(Weak::upgrade) else {
            return;
        };
        let nq = NetQuorum {
            inner,
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            nq.with_tracker(|_| ());
            let _ = tx.send(());
        });
        let free = rx.recv_timeout(Duration::from_secs(10)).is_ok();
        self.seen.lock().unwrap().push((site, free));
    }

    fn seen(&self) -> Vec<(&'static str, bool)> {
        self.seen.lock().unwrap().clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PeerEvent {
    Push(PeerId, Request),
    Disconnect(PeerId),
    AddPending(ProTxHash),
}

#[derive(Default)]
struct TestPeers {
    verified: Mutex<Vec<(PeerId, [u8; 32])>>,
    events: Mutex<Vec<PeerEvent>>,
    probe: Arc<Probe>,
}

impl TestPeers {
    fn events(&self) -> Vec<PeerEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl QuorumPeers for TestPeers {
    fn verified_peers(&self) -> Vec<(PeerId, [u8; 32])> {
        self.verified.lock().unwrap().clone()
    }

    fn push_qgetdata(&self, peer: PeerId, req: &QuorumDataRequest) -> bool {
        self.probe.at("push_qgetdata");
        self.events.lock().unwrap().push(PeerEvent::Push(peer, Request::from(req)));
        true
    }

    fn disconnect(&self, peer: PeerId) {
        self.events.lock().unwrap().push(PeerEvent::Disconnect(peer));
    }

    fn add_pending_masternode(&self, pro_tx: ProTxHash) {
        self.events.lock().unwrap().push(PeerEvent::AddPending(pro_tx));
    }
}

/// The node's time, frozen unless a test moves it, and its sync state. A sleep only yields.
struct TestEnv {
    now: AtomicI64,
    synced: AtomicBool,
    sleeps: AtomicUsize,
}

impl RecoveryEnv for TestEnv {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }

    fn blockchain_synced(&self) -> bool {
        self.synced.load(Ordering::SeqCst)
    }

    fn sleep(&self, _: Duration, interrupt: &Interrupt) {
        self.sleeps.fetch_add(1, Ordering::SeqCst);
        interrupt.sleep_for(Duration::from_millis(2));
    }
}

type Job = Box<dyn FnOnce() + Send>;
/// The pending jobs and the stop flag, with the workers' wake-up.
type Queue = (Mutex<(VecDeque<Job>, bool)>, Condvar);

/// A worker pool of `size` threads; each job runs under the dispatcher it was pushed under.
struct TestPool {
    size: usize,
    starts: AtomicUsize,
    queue: Arc<Queue>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl TestPool {
    fn new(size: usize) -> Self {
        Self {
            size,
            starts: AtomicUsize::new(0),
            queue: Arc::default(),
            workers: Mutex::default(),
        }
    }
}

impl RecoveryPool for TestPool {
    fn start(&self) {
        self.starts.fetch_add(1, Ordering::SeqCst);
        for _ in 0..self.size {
            let queue = Arc::clone(&self.queue);
            self.workers.lock().unwrap().push(std::thread::spawn(move || {
                loop {
                    let job = {
                        let (lock, cv) = &*queue;
                        let mut q = cv
                            .wait_while(lock.lock().unwrap(), |(jobs, stop)| {
                                jobs.is_empty() && !*stop
                            })
                            .unwrap();
                        match q.0.pop_front() {
                            Some(job) => job,
                            None => return,
                        }
                    };
                    job();
                }
            }));
        }
    }

    fn push(&self, job: Job) {
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let job: Job = Box::new(move || tracing::dispatcher::with_default(&dispatch, job));
        self.queue.0.lock().unwrap().0.push_back(job);
        self.queue.1.notify_one();
    }

    fn clear_queue(&self) {
        let dropped: Vec<Job> = self.queue.0.lock().unwrap().0.drain(..).collect();
        drop(dropped);
    }

    fn stop(&self) {
        self.queue.0.lock().unwrap().1 = true;
        self.queue.1.notify_all();
        for worker in self.workers.lock().unwrap().drain(..) {
            worker.join().unwrap();
        }
    }
}

#[derive(Default)]
struct TestWarm(AtomicUsize);

impl WarmExecutor for Arc<TestWarm> {
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
        self.0.fetch_add(1, Ordering::SeqCst);
        job();
    }
}

/// A memory engine whose `fail_at`th write (from 0) fails as an I/O error.
struct FailingEngine {
    inner: MemoryEngine,
    writes: AtomicUsize,
    fail_at: Option<usize>,
}

impl KvRead for FailingEngine {
    fn get(&self, cf: Cf, key: &[u8]) -> KvResult<Option<Vec<u8>>> {
        self.inner.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> KvResult<Vec<Option<Vec<u8>>>> {
        self.inner.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = KvResult<(Vec<u8>, Vec<u8>)>> + 'a {
        self.inner.iter_prefix(cf, prefix)
    }
}

impl KvEngine for FailingEngine {
    type Batch = <MemoryEngine as KvEngine>::Batch;
    type Snapshot<'a> = <MemoryEngine as KvEngine>::Snapshot<'a>;

    fn schema(&self) -> &'static DbSchema {
        self.inner.schema()
    }

    fn batch(&self) -> Self::Batch {
        self.inner.batch()
    }

    fn write(&self, batch: Self::Batch, commit: Commit) -> KvResult<()> {
        if Some(self.writes.fetch_add(1, Ordering::SeqCst)) == self.fail_at {
            self.inner.inject_failure(ErrorKind::Io);
        }
        self.inner.write(batch, commit)
    }

    fn sync_wal(&self) -> KvResult<()> {
        self.inner.sync_wal()
    }

    fn flush_memtables(&self) -> KvResult<()> {
        self.inner.flush_memtables()
    }

    fn snapshot(&self) -> Self::Snapshot<'_> {
        self.inner.snapshot()
    }

    fn approximate_size(&self, cf: Cf) -> KvResult<u64> {
        self.inner.approximate_size(cf)
    }
}

type Store = KvQuorumStore<FailingEngine>;
type Nq = NetQuorum<TestSource, TestDkg, Store, Arc<TestWarm>>;
type NqInner = Inner<TestSource, TestDkg, Store, Arc<TestWarm>>;

struct Node {
    nq: Nq,
    quorum: TestQuorum,
    probe: Arc<Probe>,
    peers: Arc<TestPeers>,
    env: Arc<TestEnv>,
    pool: Arc<TestPool>,
    warm: Arc<TestWarm>,
}

struct NodeOpts {
    role: Role,
    operator: bool,
    fail_at: Option<usize>,
    workers: usize,
    quorum: TestQuorum,
}

impl Default for NodeOpts {
    fn default() -> Self {
        Self {
            role: masternode(),
            operator: true,
            fail_at: None,
            workers: 1,
            quorum: TestQuorum::new(),
        }
    }
}

fn masternode() -> Role {
    Role::Masternode {
        pro_tx: Some(ProTxHash::from(ME)),
        watching: false,
    }
}

fn node(opts: NodeOpts) -> Node {
    let probe = Arc::new(Probe::default());
    assert!(opts.quorum.0.probe.set(Arc::clone(&probe)).is_ok());
    let peers = Arc::new(TestPeers {
        probe: Arc::clone(&probe),
        ..TestPeers::default()
    });
    let env = Arc::new(TestEnv {
        now: AtomicI64::new(SIM_START),
        synced: AtomicBool::new(true),
        sleeps: AtomicUsize::new(0),
    });
    let pool = Arc::new(TestPool::new(opts.workers));
    let warm = Arc::new(TestWarm::default());
    let engine = FailingEngine {
        inner: MemoryEngine::new(&QUORUMDB_SCHEMA),
        writes: AtomicUsize::new(0),
        fail_at: opts.fail_at,
    };
    let nq = NetQuorum::new(NetQuorumParts {
        role: opts.role,
        source: TestSource {
            quorum: opts.quorum.clone(),
            probe: Arc::clone(&probe),
        },
        contributions: TestDkg(Arc::clone(&probe)),
        operator: opts.operator.then(|| Arc::new(TestOperator(Arc::clone(&probe))) as Arc<_>),
        valid_mns: Arc::new(TestMns),
        store: KvQuorumStore::new(engine),
        warm: Arc::clone(&warm),
        peers: peers.clone(),
        env: env.clone(),
        pool: pool.clone(),
    });
    Node {
        nq,
        quorum: opts.quorum,
        probe,
        peers,
        env,
        pool,
        warm,
    }
}

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

/// The processor's simulation with `nq` registered as a node registers it.
fn sim(nq: &Nq) -> Sim {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let mut registry = Registry::new();
    registry.register(Box::new(nq.clone()));
    for cmd in COMMANDS {
        assert!(!registry.is_stubbed(&cmd));
    }
    Sim::with(p, id, ProcessorOptions::default(), registry, Seams::default(), |_| {})
}

fn scheme(sim: &Sim) -> BlsSchemeKind {
    ChainAccess::snapshot(&*sim.chain).bls.scheme()
}

fn peer(sim: &Sim, n: u8) -> SimPeer {
    sim.connect(n, ConnectionType::Inbound, Permissions::NONE)
}

/// A peer verified as the masternode `pro`.
fn verified(sim: &Sim, n: u8, pro: [u8; 32]) -> SimPeer {
    let p = peer(sim, n);
    assert!(sim.core.try_mark_verified(p.handle.id(), pro, [n; 32]));
    p
}

fn score(sim: &Sim, p: &SimPeer) -> i32 {
    sim.shared.misbehavior(p.handle.id()).unwrap().0
}

fn encode<T: BaseCodec>(t: &T) -> Vec<u8> {
    let mut out = Vec::new();
    t.encode(&mut out);
    out
}

fn wire_req(hash: [u8; 32], mask: u16, pro: [u8; 32]) -> QuorumDataRequest {
    QuorumDataRequest::new(LLMQ, QuorumHash::from(hash), mask, TxHash::from(pro))
}

fn qgetdata(hash: [u8; 32], mask: u16, pro: [u8; 32]) -> Vec<u8> {
    encode(&wire_req(hash, mask, pro))
}

fn vvec_body(scheme: BlsSchemeKind, n: u8) -> Vec<u8> {
    let mut out = vec![n];
    for i in 1..=n {
        out.extend_from_slice(&pk(i).to_bytes(scheme));
    }
    out
}

fn contrib_body(scheme: BlsSchemeKind) -> Vec<u8> {
    let mut out = vec![3];
    for n in BLOBS {
        out.extend(blob(n, scheme).to_bytes());
    }
    out
}

fn qdata(mask: u16, body: Vec<u8>) -> Vec<u8> {
    encode(&QData::new(wire_req(QHASH, mask, ME), QuorumDataError::None, body))
}

/// Registers this node's request to the masternode `to`, as a recovery job would.
fn requested(node: &Node, to: [u8; 32], mask: u16) {
    let req = Request::from(&wire_req(QHASH, mask, ME));
    lock(&node.nq.inner.tracker).request(ProTxHash::from(to), req, SIM_START).unwrap();
}

fn entry(node: &Node, to: [u8; 32]) -> Option<Entry> {
    let key = Key {
        pro_reg_tx: ProTxHash::from(to),
        we_requested: true,
        quorum_hash: BlockHash::from(QHASH),
        llmq_type: LLMQ,
    };
    node.nq.with_tracker(|t| t.entry(&key).copied())
}

fn replies(sim: &Sim, p: &SimPeer) -> Vec<QData> {
    sim.sent(p)
        .into_iter()
        .filter(|(cmd, _)| *cmd == CommandString::QDATA)
        .map(|(_, payload)| QData::decode(&mut &payload[..]).unwrap())
        .collect()
}

// ---- log capture

type Line = (String, tracing::Level, String);

#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<Line>>>);

impl tracing::Subscriber for Lines {
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
        let meta = event.metadata();
        self.0.lock().unwrap().push((meta.target().to_owned(), *meta.level(), m.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

impl Lines {
    fn all(&self) -> Vec<Line> {
        self.0.lock().unwrap().clone()
    }

    /// The messages logged under `target`.
    fn of(&self, target: &str) -> Vec<String> {
        self.all().into_iter().filter(|(t, ..)| t == target).map(|(.., m)| m).collect()
    }

    fn misbehaving(&self) -> Vec<String> {
        self.of("net").into_iter().filter(|m| m.starts_with("Misbehaving:")).collect()
    }
}

/// `f`'s log lines, from its thread and from the jobs it queues.
fn logged<T>(f: impl FnOnce() -> T) -> (T, Lines) {
    // BL-479: never the only registered dispatcher.
    static KEEP: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let lines = Lines::default();
    let out = tracing::subscriber::with_default(lines.clone(), f);
    (out, lines)
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

// ---- the gates (plan §4 QM1b-6 [r1] F1)

/// A rejected role or peer scores +10 before any length check or read: a 68-byte `qgetdata` is
/// not "oversized" and a short one does not throw.
// Core: v24.0.0 src/llmq/net_quorum.cpp:80-83, :204-208
#[test]
fn gate_rejections_score_10_before_any_read() {
    let observer = node(NodeOpts {
        role: Role::Observer,
        ..NodeOpts::default()
    });
    let none = node(NodeOpts {
        role: Role::None,
        operator: false,
        ..NodeOpts::default()
    });
    let mn = node(NodeOpts::default());
    let long = vec![0xff; 68];
    let short = vec![0xff; 3];
    for (n, (node, cmd, verified_pro)) in [
        (&none, "qgetdata", Some([1; 32])),
        (&observer, "qgetdata", Some([1; 32])),
        (&mn, "qgetdata", None),
        (&none, "qdata", Some([1; 32])),
        (&mn, "qdata", None),
    ]
    .into_iter()
    .enumerate()
    {
        let mut sim = sim(&node.nq);
        let n = u8::try_from(n).unwrap() + 1;
        let p = match verified_pro {
            Some(pro) => verified(&sim, n, pro),
            None => peer(&sim, n),
        };
        let reason = if cmd == "qgetdata" {
            "not a verified masternode or a qwatch connection"
        } else {
            "not a verified masternode and -watchquorums is not enabled"
        };
        let (results, lines) =
            logged(|| [sim.admitted(&p, cmd, long.clone()), sim.admitted(&p, cmd, short.clone())]);
        assert_eq!(results, [Ok(()), Ok(())], "{cmd}");
        assert_eq!(score(&sim, &p), 20, "{cmd}");
        let misbehaving = lines.misbehaving();
        assert_eq!(misbehaving.len(), 2, "{cmd}");
        assert!(misbehaving.iter().all(|m| m.ends_with(reason)), "{misbehaving:?}");
        assert!(sim.sent(&p).is_empty());
    }
}

/// An unverified `qwatch` connection passes the `qgetdata` gate, and so does an observer's
/// verified peer the `qdata` gate.
// Core: v24.0.0 src/llmq/net_quorum.cpp:80, :205
#[test]
fn qwatch_and_observer_pass_their_gates() {
    let mn = node(NodeOpts::default());
    let mut sim = sim(&mn.nq);
    let p = peer(&sim, 1);
    p.handle.set_qwatch(true);
    sim.admitted(&p, "qgetdata", qgetdata(OFF_CHAIN, VVEC, [0; 32])).unwrap();
    assert_eq!(replies(&sim, &p)[0].error(), QuorumDataError::QuorumBlockNotFound);
    assert_eq!(score(&sim, &p), 0);

    let observer = node(NodeOpts {
        role: Role::Observer,
        operator: false,
        ..NodeOpts::default()
    });
    let mut sim = sim_for(&observer);
    let p = verified(&sim, 1, PEER_MN);
    let (_, lines) = logged(|| sim.admitted(&p, "qdata", qdata(VVEC, Vec::new())).unwrap());
    assert!(lines.misbehaving()[0].ends_with(": not requested"), "{:?}", lines.all());
}

fn sim_for(node: &Node) -> Sim {
    sim(&node.nq)
}

/// A `qgetdata` over the request's 67 bytes scores +100 "oversized qgetdata" before it is read;
/// a short one throws.
// Core: v24.0.0 src/llmq/net_quorum.cpp:85-93
#[test]
fn oversized_qgetdata_scores_100_and_a_short_one_throws() {
    let mn = node(NodeOpts::default());
    let mut sim = sim_for(&mn);
    let p = verified(&sim, 1, PEER_MN);
    let mut long = qgetdata(QHASH, VVEC, [0; 32]);
    long.push(0);
    let (_, lines) = logged(|| sim.admitted(&p, "qgetdata", long).unwrap());
    assert!(lines.misbehaving()[0].ends_with(": oversized qgetdata"));
    assert_eq!(score(&sim, &p), 100);

    let q = verified(&sim, 2, OTHER);
    let t = sim.admitted(&q, "qgetdata", vec![0; 66]).unwrap_err();
    assert_eq!(
        t.log_line(&CommandString::QGETDATA, 66),
        "ProcessMessages(qgetdata, 66 bytes): Exception 'DataStream::read(): end of data: iostream error' (NSt8ios_base7failureB5cxx11E) caught"
    );
    assert_eq!(score(&sim, &q), 0);
    assert!(replies(&sim, &q).is_empty() && replies(&sim, &p).is_empty());
}

// ---- serve on the wire (QM1b-2's outcomes)

/// Each outcome of `serve` on the wire: the reply, then the score.
// Core: v24.0.0 src/llmq/net_quorum.cpp:94-201, :273-301
#[test]
fn serve_outcomes_on_the_wire() {
    let with_vvec = node(NodeOpts {
        quorum: TestQuorum::new().with_vvec(),
        ..NodeOpts::default()
    });
    let mut sim = sim_for(&with_vvec);
    let scheme = scheme(&sim);
    let mut n = 0;
    let mut ask = |sim: &mut Sim, payload: Vec<u8>| {
        n += 1;
        let p = verified(sim, n, [n; 32]);
        sim.admitted(&p, "qgetdata", payload).unwrap();
        let replies = replies(sim, &p);
        assert!(replies.len() <= 1);
        (replies.into_iter().next(), score(sim, &p))
    };

    let mut bad_type = wire_req(QHASH, VVEC, [0; 32]);
    bad_type = QuorumDataRequest::new(
        LlmqType::from_base(0xee),
        bad_type.quorum_hash(),
        VVEC,
        bad_type.pro_tx_hash(),
    );
    let (reply, s) = ask(&mut sim, encode(&bad_type));
    assert_eq!((reply.unwrap().error(), s), (QuorumDataError::QuorumTypeInvalid, 100));

    let (reply, s) = ask(&mut sim, qgetdata(OFF_CHAIN, VVEC, [0; 32]));
    assert_eq!((reply.unwrap().error(), s), (QuorumDataError::QuorumBlockNotFound, 0));

    let (reply, s) = ask(&mut sim, qgetdata(NO_QUORUM, VVEC, [0; 32]));
    assert_eq!((reply.unwrap().error(), s), (QuorumDataError::QuorumNotFound, 0));

    let (reply, s) = ask(&mut sim, qgetdata(QHASH, VVEC, [0; 32]));
    let reply = reply.unwrap();
    assert_eq!((reply.error(), s), (QuorumDataError::None, 0));
    assert_eq!(reply.data(), &vvec_body(scheme, 2));

    let (reply, s) = ask(&mut sim, qgetdata(QHASH, VVEC | CONTRIB, PEER_MN));
    let reply = reply.unwrap();
    assert_eq!((reply.error(), s), (QuorumDataError::None, 0));
    let mut body = vvec_body(scheme, 2);
    body.extend(contrib_body(scheme));
    assert_eq!(reply.data(), &body);

    let (reply, s) = ask(&mut sim, qgetdata(QHASH, CONTRIB, [0x99; 32]));
    assert_eq!((reply.unwrap().error(), s), (QuorumDataError::MasternodeIsNoMember, 0));

    // A repeat within the expiry window from the same masternode is rate-limited.
    let p = verified(&sim, 200, [200; 32]);
    for _ in 0..2 {
        sim.admitted(&p, "qgetdata", qgetdata(NO_QUORUM, VVEC, [0; 32])).unwrap();
    }
    assert_eq!(replies(&sim, &p).len(), 2);
    assert_eq!(score(&sim, &p), 25);

    // Without the vvec: `QUORUM_VERIFICATION_VECTOR_MISSING`; with no operator key, a
    // contributions request is `ENCRYPTED_CONTRIBUTIONS_MISSING`.
    let bare = node(NodeOpts {
        operator: false,
        ..NodeOpts::default()
    });
    let mut sim = sim_for(&bare);
    let (reply, s) = ask(&mut sim, qgetdata(QHASH, VVEC, [0; 32]));
    assert_eq!((reply.unwrap().error(), s), (QuorumDataError::QuorumVerificationVectorMissing, 0));
    let (reply, s) = ask(&mut sim, qgetdata(QHASH, CONTRIB, PEER_MN));
    assert_eq!((reply.unwrap().error(), s), (QuorumDataError::EncryptedContributionsMissing, 0));
}

/// The quorum build's read error is Core's throw out of `GetQuorum`: `ProcessMessages`' line, no
/// reply and no score; the registration stays.
// Core: v24.0.0 src/llmq/net_quorum.cpp:148-160; src/net_processing.cpp:5958-5959
#[test]
fn serve_read_error_is_the_caught_exception_line() {
    let mn = node(NodeOpts::default());
    let mut sim = sim_for(&mn);
    let p = verified(&sim, 1, PEER_MN);
    let t = sim.admitted(&p, "qgetdata", qgetdata(BROKEN, VVEC, [0; 32])).unwrap_err();
    assert_eq!(
        t.log_line(&CommandString::QGETDATA, 67),
        format!(
            "ProcessMessages(qgetdata, 67 bytes): Exception '{MALLEABLE}' ({IOS_FAILURE}) caught"
        )
    );
    assert!(replies(&sim, &p).is_empty());
    assert_eq!(score(&sim, &p), 0);
    let key = Key {
        pro_reg_tx: ProTxHash::from(PEER_MN),
        we_requested: false,
        quorum_hash: BlockHash::from(BROKEN),
        llmq_type: LLMQ,
    };
    assert!(mn.nq.with_tracker(|t| t.entry(&key).is_some()));
}

// ---- intake on the wire (QM1b-3's outcomes)

/// Each outcome of `intake` on the wire, with its effects applied in order: the scores, the
/// `llmq` lines, the warm-up and the write.
// Core: v24.0.0 src/llmq/net_quorum.cpp:203-270, :303-355
#[test]
fn intake_outcomes_on_the_wire() {
    let mn = node(NodeOpts::default());
    let mut sim = sim_for(&mn);
    let scheme = scheme(&sim);
    let p = verified(&sim, 1, PEER_MN);
    let reasons = |sim: &mut Sim, payload: Vec<u8>| {
        let (r, lines) = logged(|| sim.admitted(&p, "qdata", payload));
        (r, lines)
    };

    // Not requested.
    let (r, lines) = reasons(&mut sim, qdata(VVEC, vvec_body(scheme, 2)));
    assert_eq!(r, Ok(()));
    assert!(lines.misbehaving()[0].ends_with(": not requested"));

    // Not like requested.
    requested(&mn, PEER_MN, VVEC | CONTRIB);
    let (_, lines) = reasons(&mut sim, qdata(VVEC, vvec_body(scheme, 2)));
    assert!(lines.misbehaving()[0].ends_with(": not like requested"));

    // The answer: the vvec set and its warm-up queued, the share set, the rows written.
    let mut body = vvec_body(scheme, 2);
    body.extend(contrib_body(scheme));
    let (r, lines) = reasons(&mut sim, qdata(VVEC | CONTRIB, body.clone()));
    assert_eq!(r, Ok(()));
    assert!(lines.misbehaving().is_empty(), "{:?}", lines.all());
    assert_eq!(score(&sim, &p), 20);
    assert_eq!(mn.warm.0.load(Ordering::SeqCst), 1);
    assert_eq!(mn.quorum.0.warmed.load(Ordering::SeqCst), 1);
    assert_eq!(mn.quorum.vvec().unwrap().as_slice(), &[pk(1), pk(2)]);
    let share = dash_llmq::dkg::bls::aggregate_secret_keys(
        &BLOBS.map(sk).iter().collect::<Vec<_>>(),
        scheme,
    );
    assert_eq!(mn.quorum.sk_share(), Some(share.clone()));
    let stored = mn.nq.inner.parts.store.read(&DB_KEY).unwrap().unwrap();
    assert_eq!(stored.vvec.unwrap().unwrap(), vec![pk(1), pk(2)]);
    assert_eq!(stored.sk_share, Some(share));
    assert!(entry(&mn, PEER_MN).unwrap().processed);

    // Already received.
    let (_, lines) = reasons(&mut sim, qdata(VVEC | CONTRIB, body));
    assert!(lines.misbehaving()[0].ends_with(": already received"));
    assert_eq!(score(&sim, &p), 30);
}

/// A vvec of the wrong size scores +100; a valid vvec followed by contributions cut short
/// queues the warm-up, then throws, with no write.
// Core: v24.0.0 src/llmq/net_quorum.cpp:243-257, :327-332; src/streams.h:287
#[test]
fn intake_size_and_truncation() {
    let mn = node(NodeOpts::default());
    let mut sim = sim_for(&mn);
    let scheme = scheme(&sim);
    let p = verified(&sim, 1, PEER_MN);
    requested(&mn, PEER_MN, VVEC);
    let (_, lines) = logged(|| sim.admitted(&p, "qdata", qdata(VVEC, vvec_body(scheme, 1))));
    assert!(lines.misbehaving()[0].ends_with(": invalid quorum verification vector size"));
    assert_eq!(score(&sim, &p), 100);

    let q = verified(&sim, 2, OTHER);
    requested(&mn, OTHER, VVEC | CONTRIB);
    let mut body = vvec_body(scheme, 2);
    body.extend(&contrib_body(scheme)[..40]);
    let mut payload =
        encode(&QData::new(wire_req(QHASH, VVEC | CONTRIB, ME), QuorumDataError::None, Vec::new()));
    payload.extend(body);
    let size = payload.len();
    let t = sim.admitted(&q, "qdata", payload).unwrap_err();
    assert_eq!(
        t.log_line(&CommandString::QDATA, size),
        format!(
            "ProcessMessages(qdata, {size} bytes): Exception 'DataStream::read(): end of data: iostream error' ({IOS_FAILURE}) caught"
        )
    );
    assert_eq!(mn.warm.0.load(Ordering::SeqCst), 1);
    assert!(mn.quorum.sk_share().is_none());
    assert!(mn.nq.inner.parts.store.read(&DB_KEY).unwrap().is_none());
    assert_eq!(score(&sim, &q), 0);
}

/// Intake's lines are `llmq` debug lines; the error-code line carries the request's raw error
/// byte as Core's logger writes it.
// Core: v24.0.0 src/llmq/net_quorum.cpp:231-233; src/logging.cpp:420-431
#[test]
fn intake_lines_are_llmq_debug_and_the_error_line_has_the_raw_byte() {
    let mn = node(NodeOpts::default());
    let mut sim = sim_for(&mn);
    let p = verified(&sim, 1, PEER_MN);
    requested(&mn, PEER_MN, VVEC);
    let mut payload = encode(&wire_req(QHASH, VVEC, ME));
    payload.push(0x01);
    let (_, lines) = logged(|| sim.admitted(&p, "qdata", payload).unwrap());
    let llmq: Vec<_> = lines.all().into_iter().filter(|(t, ..)| t == "llmq").collect();
    let id = p.handle.id();
    assert_eq!(
        llmq,
        [(
            "llmq".to_owned(),
            tracing::Level::DEBUG,
            format!(
                "NetQuorum::ProcessMessage -- qdata: Error \\x01 (QUORUM_TYPE_INVALID), from peer={id}"
            )
        )]
    );
    assert_eq!(score(&sim, &p), 0);
}

/// The line's bytes: a control byte escaped, 0xFF (UNDEFINED, also a missing byte) one raw byte.
#[test]
fn error_line_bytes_follow_cores_logger() {
    let line = |e: u8| {
        format!("NetQuorum::ProcessMessage -- qdata: Error {} (X), from peer=3", char::from(e))
    };
    assert_eq!(
        qdata_error_line(&line(0x01), 0x01).unwrap(),
        b"NetQuorum::ProcessMessage -- qdata: Error \\x01 (X), from peer=3"
    );
    let mut want = b"NetQuorum::ProcessMessage -- qdata: Error ".to_vec();
    want.push(0xff);
    want.extend_from_slice(b" (X), from peer=3");
    assert_eq!(qdata_error_line(&line(0xff), 0xff).unwrap(), want);
    assert_eq!(qdata_error_line("NetQuorum::ProcessMessage -- qdata: Quorum not found", 1), None);
}

/// A 67-byte `qdata` has no error byte: UNDEFINED, whose line carries 0xFF.
#[test]
fn a_missing_error_byte_reads_as_undefined() {
    let mn = node(NodeOpts::default());
    let mut sim = sim_for(&mn);
    let p = verified(&sim, 1, PEER_MN);
    requested(&mn, PEER_MN, VVEC);
    let (_, lines) =
        logged(|| sim.admitted(&p, "qdata", encode(&wire_req(QHASH, VVEC, ME))).unwrap());
    let llmq = lines.of("llmq");
    assert_eq!(llmq.len(), 1);
    assert!(llmq[0].ends_with(&format!(" (UNDEFINED), from peer={}", p.handle.id())));
    assert_eq!(score(&sim, &p), 0);
}

/// Declared divergence: the node's log writer takes `&str`, so the 0xFF byte Core writes raw,
/// for an explicit 0xFF and for a missing byte alike, is logged as U+FFFD (EF BF BD).
// Core: v24.0.0 src/llmq/net_quorum.cpp:231-233; src/logging.cpp:420-431
#[test]
fn the_undefined_error_byte_logs_as_u_fffd() {
    let mut explicit = encode(&wire_req(QHASH, VVEC, ME));
    explicit.push(0xff);
    let missing = encode(&wire_req(QHASH, VVEC, ME));
    for payload in [explicit, missing] {
        let mn = node(NodeOpts::default());
        let mut sim = sim_for(&mn);
        let p = verified(&sim, 1, PEER_MN);
        requested(&mn, PEER_MN, VVEC);
        let (_, lines) = logged(|| sim.admitted(&p, "qdata", payload).unwrap());
        let want = format!(
            "NetQuorum::ProcessMessage -- qdata: Error \u{fffd} (UNDEFINED), from peer={}",
            p.handle.id()
        );
        assert!(want.as_bytes().windows(3).any(|w| w == [0xef, 0xbf, 0xbd]));
        assert_eq!(lines.of("llmq"), [want]);
        assert_eq!(score(&sim, &p), 0);
    }
}

// ---- DEC-1762: a failed store write

/// A failed write of `fail_at` (0: the vvec row, 1: the share row) is `HandleError`'s two
/// lines, then `ProcessMessages`' line for the throw; no score, no disconnect, the warm-up
/// queued, and the state v24.0.0 has after the throw. A repeat is "already received".
// Core: v24.0.0 src/dbwrapper.cpp:261-268; src/llmq/quorums.cpp:192-207; src/net_processing.cpp:5958-5959
fn write_failure(fail_at: usize) {
    let mn = node(NodeOpts {
        fail_at: Some(fail_at),
        ..NodeOpts::default()
    });
    let mut sim = sim_for(&mn);
    let scheme = scheme(&sim);
    let p = verified(&sim, 1, PEER_MN);
    requested(&mn, PEER_MN, VVEC | CONTRIB);
    let mut body = vvec_body(scheme, 2);
    body.extend(contrib_body(scheme));
    let payload = qdata(VVEC | CONTRIB, body);
    let size = payload.len();
    let (t, lines) = logged(|| sim.admitted(&p, "qdata", payload.clone()).unwrap_err());
    let always: Vec<_> = lines
        .all()
        .into_iter()
        .filter(|(_, level, _)| *level == tracing::Level::INFO)
        .map(|(.., m)| m)
        .collect();
    assert_eq!(always.len(), 2, "{always:?}");
    assert!(always[0].starts_with("Fatal LevelDB error: "), "{always:?}");
    assert_eq!(always[1], DEBUG_LEVELDB);
    assert_eq!(
        t.log_line(&CommandString::QDATA, size),
        format!(
            "ProcessMessages(qdata, {size} bytes): Exception '{}' (15dbwrapper_error) caught",
            always[0]
        )
    );
    assert_eq!(score(&sim, &p), 0);
    sim.pass();
    assert!(!p.handle.disconnect_requested());
    assert_eq!(mn.warm.0.load(Ordering::SeqCst), 1);
    assert!(entry(&mn, PEER_MN).unwrap().processed);
    assert!(mn.quorum.vvec().is_some());
    assert!(mn.quorum.sk_share().is_some());
    let stored = mn.nq.inner.parts.store.read(&DB_KEY).unwrap();
    match fail_at {
        0 => assert!(stored.is_none()),
        _ => {
            let stored = stored.unwrap();
            assert_eq!(stored.vvec.unwrap().unwrap(), vec![pk(1), pk(2)]);
            assert!(stored.sk_share.is_none());
        }
    }

    let (r, lines) = logged(|| sim.admitted(&p, "qdata", payload));
    assert_eq!(r, Ok(()));
    assert!(lines.misbehaving()[0].ends_with(": already received"));
    assert_eq!(score(&sim, &p), 10);
}

#[test]
fn write_failure_on_the_vvec_row() {
    write_failure(0);
}

#[test]
fn write_failure_on_the_share_row() {
    write_failure(1);
}

// ---- lifecycle and the recovery job driver (plan F6)

fn claimed(node: &Node, mask: u16) -> ClaimedJob {
    let plan = Plan::SkShare {
        q: QuorumRef {
            llmq_type: LLMQ,
            quorum_hash: BlockHash::from(QHASH),
        },
        mask,
        trigger: TipRef {
            height: 100,
            hash: BlockHash::from([0x60; 32]),
        },
    };
    match plan.claim(&node.quorum) {
        Claim::Claimed(job) => job,
        other => panic!("{other:?}"),
    }
}

/// A node without a quorum role starts no pool.
// Core: v24.0.0 src/llmq/net_quorum.cpp:58-64
#[test]
fn start_needs_a_role() {
    let none = node(NodeOpts {
        role: Role::None,
        ..NodeOpts::default()
    });
    none.nq.start();
    assert_eq!(none.pool.starts.load(Ordering::SeqCst), 0);
    for role in [masternode(), Role::Observer] {
        let n = node(NodeOpts {
            role,
            ..NodeOpts::default()
        });
        n.nq.start();
        assert_eq!(n.pool.starts.load(Ordering::SeqCst), 1);
        n.nq.stop();
    }
}

/// `Stop` drops the queued jobs, which release their claims; nothing of them runs.
// Core: v24.0.0 src/llmq/net_quorum.cpp:66-70
#[test]
fn stop_drops_queued_jobs_and_releases_their_claims() {
    let mn = node(NodeOpts::default());
    let (_, lines) = logged(|| {
        mn.nq.enqueue_recovery(claimed(&mn, VVEC | CONTRIB), mn.quorum.clone());
        assert!(mn.quorum.recovery_running());
        mn.nq.stop();
    });
    assert!(!mn.quorum.recovery_running());
    assert!(lines.of("llmq").is_empty());
}

/// Shutdown while unsynced with queued jobs: the interrupt ends the running job's sync wait
/// ("Aborted"), `Stop` drops the rest, and every claim is released.
// Core: v24.0.0 src/llmq/net_quorum.cpp:554-562, :66-70
#[test]
fn shutdown_while_unsynced_with_queued_jobs() {
    let quorums: Vec<_> = (0..3).map(|_| TestQuorum::new()).collect();
    let mn = node(NodeOpts::default());
    mn.env.synced.store(false, Ordering::SeqCst);
    let (_, lines) = logged(|| {
        mn.nq.start();
        for q in &quorums {
            let job = match (Plan::VvecSync {
                q: QuorumRef {
                    llmq_type: LLMQ,
                    quorum_hash: BlockHash::from(QHASH),
                },
            })
            .claim(q)
            {
                Claim::Claimed(job) => job,
                other => panic!("{other:?}"),
            };
            mn.nq.enqueue_recovery(job, q.clone());
        }
        wait_until("the first job's sync wait", || mn.env.sleeps.load(Ordering::SeqCst) > 0);
        let mut handler = mn.nq.clone();
        NetHandler::interrupt(&mut handler);
        NetHandler::stop(&mut handler);
    });
    assert!(quorums.iter().all(|q| !q.recovery_running()));
    let llmq = lines.of("llmq");
    assert!(llmq[0].starts_with("NetQuorum::DataRecoveryThread -- Start - "), "{llmq:?}");
    assert!(llmq[1].starts_with("NetQuorum::DataRecoveryThread -- Aborted - "), "{llmq:?}");
    assert!(!llmq.iter().any(|l| l.contains("Try to request") || l.contains("-- Done")));
    assert!(mn.peers.events().is_empty());
}

/// A job asks its first member (`OTHER`: this node is second of the enabled set, so the walk
/// starts at offset 1) through the tracker, then waits; the interrupt ends its loop with "Done"
/// and no "Aborted", and the claim is released.
// Core: v24.0.0 src/llmq/net_quorum.cpp:572-637
#[test]
fn interrupt_mid_job_ends_the_request_loop() {
    let mn = node(NodeOpts::default());
    *mn.peers.verified.lock().unwrap() = vec![(5, OTHER), (6, [0x0d; 32])];
    let (_, lines) = logged(|| {
        mn.nq.start();
        mn.nq.enqueue_recovery(claimed(&mn, VVEC | CONTRIB), mn.quorum.clone());
        wait_until("the request", || {
            mn.peers.events().iter().any(|e| matches!(e, PeerEvent::Push(..)))
        });
        mn.nq.interrupt();
        mn.nq.stop();
    });
    assert!(!mn.quorum.recovery_running());
    let want = Request::from(&wire_req(QHASH, VVEC | CONTRIB, ME));
    assert_eq!(
        mn.peers.events(),
        [PeerEvent::AddPending(ProTxHash::from(OTHER)), PeerEvent::Push(5, want)]
    );
    assert!(entry(&mn, OTHER).is_some());
    let llmq = lines.of("llmq");
    let mut steps: Vec<_> = llmq
        .iter()
        .filter_map(|l| l.strip_prefix("NetQuorum::DataRecoveryThread -- "))
        .map(|l| l.split(" - ").next().unwrap())
        .filter(|step| *step != "Waiting")
        .collect();
    steps.dedup();
    assert_eq!(steps, ["Start", "Try to request", "Connect", "Requested", "Done"]);
    assert!(llmq.iter().any(|l| l.starts_with("NetQuorum::RequestQuorumData -- sending QGETDATA")));
}

/// `Schedule` hands the scheduler the expired-request cleanup every 60 s.
// Core: v24.0.0 src/llmq/net_quorum.cpp:72-75; src/llmq/quorumsman.cpp:460-483
#[test]
fn schedule_runs_the_expired_request_cleanup() {
    let mn = node(NodeOpts::default());
    requested(&mn, PEER_MN, VVEC);
    let mut task = None;
    mn.nq.schedule(|f, every| task = Some((f, every)));
    let (f, every) = task.unwrap();
    assert_eq!(every, Duration::from_secs(60));
    f();
    assert!(entry(&mn, PEER_MN).is_some());
    mn.env.now.store(SIM_START + 360, Ordering::SeqCst);
    f();
    assert!(entry(&mn, PEER_MN).is_none());
}

/// `quorum getdata`: registered under the peer's verified hash, then pushed; a repeat is
/// refused, and so is an unverified or unknown peer.
// Core: v24.0.0 src/rpc/quorums.cpp:942-954
#[test]
fn request_rpc_registers_then_pushes() {
    let mn = node(NodeOpts::default());
    *mn.peers.verified.lock().unwrap() = vec![(5, PEER_MN), (6, [0; 32])];
    let req = Request::from(&wire_req(QHASH, VVEC, [0; 32]));
    assert!(mn.nq.request_rpc(5, req));
    assert!(!mn.nq.request_rpc(5, req));
    assert!(!mn.nq.request_rpc(6, req));
    assert!(!mn.nq.request_rpc(7, req));
    assert_eq!(mn.peers.events(), [PeerEvent::Push(5, req)]);
    assert!(entry(&mn, PEER_MN).is_some());
}

// ---- the tracker's lock scope (Core's cs_data_requests)

/// Serve's quorum build and dkgdb read, and the recovery and `quorum getdata` pushes, run with
/// the tracker unlocked: Core takes `cs_data_requests` only inside `RegisterDataRequest`.
// Core: v24.0.0 src/llmq/quorumsman.cpp:567-598; src/llmq/net_quorum.cpp:148-160, :293, :371-372
#[test]
fn serve_and_pushes_run_with_the_tracker_unlocked() {
    let mn = node(NodeOpts {
        quorum: TestQuorum::new().with_vvec(),
        ..NodeOpts::default()
    });
    mn.probe.arm(&mn.nq);
    let mut sim = sim_for(&mn);
    let p = verified(&sim, 1, PEER_MN);
    sim.admitted(&p, "qgetdata", qgetdata(QHASH, VVEC | CONTRIB, PEER_MN)).unwrap();
    assert_eq!(replies(&sim, &p)[0].error(), QuorumDataError::None);
    assert_eq!(mn.probe.seen(), [("quorum", true), ("contributions", true)]);

    *mn.peers.verified.lock().unwrap() = vec![(5, PEER_MN)];
    assert!(mn.nq.request_rpc(5, Request::from(&wire_req(QHASH, VVEC, [0; 32]))));
    assert_eq!(mn.probe.seen()[2..], [("push_qgetdata", true)]);

    mn.peers.verified.lock().unwrap().push((6, OTHER));
    mn.nq.start();
    mn.nq.enqueue_recovery(claimed(&mn, VVEC | CONTRIB), mn.quorum.clone());
    wait_until("the recovery push", || mn.probe.seen().len() > 3);
    mn.nq.interrupt();
    mn.nq.stop();
    assert_eq!(mn.probe.seen()[3..], [("push_qgetdata", true)]);
}

/// Intake's quorum lookup, decrypts and share set run with the tracker unlocked: Core takes
/// `cs_data_requests` only inside `ValidateDataResponse`.
// Core: v24.0.0 src/llmq/quorumsman.cpp:600-616; src/llmq/net_quorum.cpp:236, :339, :348
#[test]
fn intake_runs_with_the_tracker_unlocked() {
    let mn = node(NodeOpts::default());
    mn.probe.arm(&mn.nq);
    let mut sim = sim_for(&mn);
    let scheme = scheme(&sim);
    let p = verified(&sim, 1, PEER_MN);
    requested(&mn, PEER_MN, VVEC | CONTRIB);
    let mut body = vvec_body(scheme, 2);
    body.extend(contrib_body(scheme));
    sim.admitted(&p, "qdata", qdata(VVEC | CONTRIB, body)).unwrap();
    assert!(mn.quorum.sk_share().is_some());
    let mut want = vec![("cached_quorum", true)];
    want.extend([("decrypt_sk", true); BLOBS.len()]);
    want.push(("set_sk_share", true));
    assert_eq!(mn.probe.seen(), want);
}
