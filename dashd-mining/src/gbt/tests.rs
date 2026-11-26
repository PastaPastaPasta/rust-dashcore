use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use dash_clock::MockClock;
use dash_consensus::ctx::DeploymentPos;
use dash_consensus::deployments::ThresholdState;
use dash_consensus::primitives::{
    BlockHeader, Header, OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxRef, TxType,
};
use dash_consensus::{Block, BlockValidationError, BlockValidationResult};
use dash_num::{Arith256, CompactTarget};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{BlockState, NotTested, WaitEnd, WaitError};

use super::*;
use crate::testkit::FakePool;
use crate::view::PackageView;
use crate::{BlockTemplate, LastBlockStats, TemplateTxs};

/// A long wait bound for a result that must come; never a timing assertion.
const PATIENCE: Duration = Duration::from_secs(60);

fn hash(n: u8) -> BlockHash {
    BlockHash::from([n; 32])
}

fn params(network: Network) -> GbtParams {
    let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
    if network == Network::Devnet {
        builder = builder.devnet_name("gbt");
    }
    GbtParams::of(&builder.build().expect("Core's parameters")).expect("payment parameters")
}

/// V's tip, `IsInitialBlockDownload()` and `g_best_block`, with a record of every wait.
struct FakeChain {
    state: Mutex<ChainState>,
    best: Mutex<Option<BlockHash>>,
    changed: Condvar,
    /// Each wait's deadline and how it ended.
    waits: Mutex<Vec<(Instant, Option<WaitEnd>)>>,
    waiting: AtomicUsize,
    tip_turns: AtomicUsize,
    fields_turns: AtomicUsize,
    /// The next proposal's answer, and the blocks proposed.
    verdict: Mutex<ProposalOutcome>,
    proposed: Mutex<Vec<BlockHash>>,
}

struct ChainState {
    tip: BlockHash,
    height: u32,
    ibd: bool,
    mtp: i64,
    min_difficulty_bits: Option<u32>,
    versionbits: Vec<(DeploymentPos, ThresholdState)>,
    dip0001: bool,
    /// Tips V moves to just before a fields turn reads it.
    moves: VecDeque<(BlockHash, u32)>,
}

impl FakeChain {
    fn new(tip: BlockHash, height: u32) -> FakeChain {
        FakeChain {
            state: Mutex::new(ChainState {
                tip,
                height,
                ibd: false,
                mtp: 1_000,
                min_difficulty_bits: None,
                versionbits: Vec::new(),
                dip0001: true,
                moves: VecDeque::new(),
            }),
            best: Mutex::new(Some(tip)),
            changed: Condvar::new(),
            waits: Mutex::new(Vec::new()),
            waiting: AtomicUsize::new(0),
            tip_turns: AtomicUsize::new(0),
            fields_turns: AtomicUsize::new(0),
            verdict: Mutex::new(Ok(BlockState::Valid)),
            proposed: Mutex::new(Vec::new()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ChainState> {
        self.state.lock().unwrap()
    }

    fn tip(&self) -> (BlockHash, u32) {
        let s = self.state();
        (s.tip, s.height)
    }

    fn set_tip(&self, tip: BlockHash, height: u32) {
        {
            let mut s = self.state();
            s.tip = tip;
            s.height = height;
        }
        *self.best.lock().unwrap() = Some(tip);
        self.changed.notify_all();
    }

    fn wake_all(&self) {
        let _best = self.best.lock().unwrap();
        self.changed.notify_all();
    }

    fn wait_for_waiters(&self, n: usize) {
        let start = Instant::now();
        while self.waiting.load(Ordering::SeqCst) < n {
            assert!(start.elapsed() < PATIENCE, "no waiter arrived");
            thread::yield_now();
        }
    }

    fn wait_for_waits(&self, n: usize) {
        let start = Instant::now();
        while self.waits.lock().unwrap().len() < n {
            assert!(start.elapsed() < PATIENCE, "no wait arrived");
            thread::yield_now();
        }
    }

    fn waits(&self) -> Vec<(Instant, Option<WaitEnd>)> {
        self.waits.lock().unwrap().clone()
    }
}

impl GbtChain for FakeChain {
    fn tip_turn(&self) -> Result<TipTurn, TemplateError> {
        self.tip_turns.fetch_add(1, Ordering::SeqCst);
        let s = self.state();
        Ok(TipTurn {
            tip: s.tip,
            height: s.height,
            ibd: s.ibd,
        })
    }

    fn fields_turn(&self, at: FieldsAt) -> Result<FieldsTurn, TemplateError> {
        self.fields_turns.fetch_add(1, Ordering::SeqCst);
        let moved = self.state().moves.pop_front();
        if let Some((tip, height)) = moved {
            self.set_tip(tip, height);
        }
        let s = self.state();
        if s.tip != at.prev {
            return Ok(FieldsTurn::Moved(s.tip));
        }
        Ok(FieldsTurn::At(TipFields {
            mtp: s.mtp,
            time: update_time(at.time, s.mtp, at.now),
            min_difficulty_bits: s.min_difficulty_bits,
            versionbits: s.versionbits.clone(),
            dip0001: s.dip0001,
        }))
    }

    fn wait_tip_while(
        &self,
        deadline: Instant,
        keep_waiting: &mut dyn FnMut(Option<BlockHash>) -> bool,
    ) -> Result<WaitEnd, WaitError> {
        let index = {
            let mut waits = self.waits.lock().unwrap();
            waits.push((deadline, None));
            waits.len() - 1
        };
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let mut best = self.best.lock().unwrap();
        let end = loop {
            if !keep_waiting(*best) {
                break WaitEnd::Woken;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break WaitEnd::Deadline;
            }
            best = self.changed.wait_timeout(best, left).unwrap().0;
        };
        drop(best);
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        self.waits.lock().unwrap()[index].1 = Some(end);
        Ok(end)
    }

    fn propose_block(&self, block: Arc<Block>) -> Result<ProposalOutcome, TemplateError> {
        self.proposed.lock().unwrap().push(block.hash());
        Ok(self.verdict.lock().unwrap().clone())
    }
}

/// A pool behind a read-write lock, whose update counter is read without it.
#[derive(Default)]
struct TestPool {
    counter: FakePool,
    lock: RwLock<()>,
    read_locks: AtomicUsize,
    r1_reads: AtomicUsize,
    /// Bumps the counter right after the R1 read with this (1-based) index.
    bump_after_read: Mutex<Option<usize>>,
}

impl TestPool {
    fn bump(&self) {
        self.counter.bump_transactions_updated(1);
    }
}

impl MiningMempool for TestPool {
    fn with_read(&self, f: &mut dyn FnMut(&dyn PackageView)) {
        self.read_locks.fetch_add(1, Ordering::SeqCst);
        let _guard = self.lock.read().unwrap();
        f(&self.counter);
    }

    fn transactions_updated(&self) -> u32 {
        let n = self.r1_reads.fetch_add(1, Ordering::SeqCst) + 1;
        let value = self.counter.transactions_updated();
        if *self.bump_after_read.lock().unwrap() == Some(n) {
            self.bump();
        }
        value
    }
}

/// `CreateNewBlock` on the fake chain's tip: the coinbase and one transaction per update the
/// pool shows under its read lock.
struct FakeSource {
    chain: Arc<FakeChain>,
    pool: Arc<TestPool>,
    builds: AtomicUsize,
    fail: AtomicBool,
    header: Mutex<(i32, u32, u32, u32)>,
    /// Runs before the pool is read, after the engine's counter load.
    before_read: Mutex<Option<Box<dyn FnMut() + Send>>>,
    /// The next templates' transactions (the coinbase first) and payments, instead of the
    /// pool's.
    shape: Mutex<Option<Shape>>,
}

#[derive(Clone)]
struct Shape {
    txs: Vec<TxRef>,
    fees: Vec<i64>,
    sigops: Vec<i64>,
    prev_bits: u32,
    masternode: Vec<TxOut>,
    superblock: Vec<TxOut>,
}

impl FakeSource {
    fn new(chain: Arc<FakeChain>, pool: Arc<TestPool>) -> FakeSource {
        FakeSource {
            chain,
            pool,
            builds: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            header: Mutex::new((0x2000_0000, 1_000, 0x207f_ffff, 0)),
            before_read: Mutex::new(None),
            shape: Mutex::new(None),
        }
    }

    fn builds(&self) -> usize {
        self.builds.load(Ordering::SeqCst)
    }
}

fn spend(prevouts: &[TxHash], marker: u32) -> TxRef {
    let inputs =
        prevouts.iter().map(|&h| TxIn::new(OutPoint::new(h, 0), Vec::new().into(), 0)).collect();
    Arc::new(Tx::new(Transaction::new(
        1,
        TxType::new(0),
        inputs,
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        marker,
        Vec::new(),
    )))
}

fn template_on(
    prev: BlockHash,
    height: u32,
    header: (i32, u32, u32, u32),
    txs: Vec<TxRef>,
) -> BlockTemplate {
    let (version, time, bits, nonce) = header;
    let mut facts = crate::TipFacts {
        prev,
        height,
        version,
        time,
        bits,
        subsidy: 50,
    };
    facts.time = time;
    let coinbase = crate::assemble::block(&facts, &[0x51], &[], 0, &[]).txs()[0].clone();
    let raw = BlockHeader::new(version, prev, Default::default(), time, bits, nonce);
    let mut all = vec![coinbase];
    all.extend(txs);
    let n = all.len();
    BlockTemplate {
        block: Block::new(Header::new(raw), all),
        prev,
        height,
        tx_fees: vec![0; n],
        tx_sigops: vec![0; n],
        prev_bits: bits,
        masternode_payments: Vec::new(),
        superblock_payments: Vec::new(),
    }
}

impl BlockTemplateSource for FakeSource {
    fn create_new_block(
        &self,
        script: &[u8],
        txs: TemplateTxs,
    ) -> Result<BlockTemplate, TemplateError> {
        assert_eq!((script, txs), (&[0x51][..], TemplateTxs::Mempool));
        self.builds.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(TemplateError::Failed("CreateNewBlock: injected".to_owned()));
        }
        if let Some(hook) = self.before_read.lock().unwrap().as_mut() {
            hook();
        }
        let mut updates = 0;
        self.pool.with_read(&mut |_| updates = self.pool.counter.transactions_updated());
        let (tip, height) = self.chain.tip();
        let header = *self.header.lock().unwrap();
        if let Some(shape) = self.shape.lock().unwrap().clone() {
            let (version, time, bits, nonce) = header;
            let raw = BlockHeader::new(version, tip, Default::default(), time, bits, nonce);
            return Ok(BlockTemplate {
                block: Block::new(Header::new(raw), shape.txs),
                prev: tip,
                height: height + 1,
                tx_fees: shape.fees,
                tx_sigops: shape.sigops,
                prev_bits: shape.prev_bits,
                masternode_payments: shape.masternode,
                superblock_payments: shape.superblock,
            });
        }
        let txs = (0..updates).map(|i| spend(&[], i)).collect();
        Ok(template_on(tip, height + 1, header, txs))
    }

    fn last_block_stats(&self) -> Option<LastBlockStats> {
        None
    }
}

/// Node status the tests flip while calls run.
struct TestStatus {
    connman: AtomicBool,
    peers: AtomicUsize,
    synced: AtomicBool,
    rpc: AtomicBool,
}

impl Default for TestStatus {
    fn default() -> Self {
        TestStatus {
            connman: AtomicBool::new(true),
            peers: AtomicUsize::new(1),
            synced: AtomicBool::new(true),
            rpc: AtomicBool::new(true),
        }
    }
}

impl NodeStatus for TestStatus {
    fn has_connman(&self) -> bool {
        self.connman.load(Ordering::SeqCst)
    }
    fn peer_count(&self) -> usize {
        self.peers.load(Ordering::SeqCst)
    }
    fn mn_synced(&self) -> bool {
        self.synced.load(Ordering::SeqCst)
    }
    fn rpc_running(&self) -> bool {
        self.rpc.load(Ordering::SeqCst)
    }
}

struct Rig {
    chain: Arc<FakeChain>,
    pool: Arc<TestPool>,
    source: Arc<FakeSource>,
    status: Arc<TestStatus>,
    clock: Arc<MockClock>,
    engine: Arc<GbtEngine>,
}

impl Rig {
    fn new(network: Network) -> Rig {
        Rig::with(network, true, LongpollTimes::default())
    }

    fn with(network: Network, mempool: bool, times: LongpollTimes) -> Rig {
        let chain = Arc::new(FakeChain::new(hash(1), 100));
        let pool = Arc::new(TestPool::default());
        let source = Arc::new(FakeSource::new(Arc::clone(&chain), Arc::clone(&pool)));
        let status = Arc::new(TestStatus::default());
        let clock = Arc::new(MockClock::at(10_000));
        let engine = GbtEngine::new(GbtParts {
            chain: chain.clone(),
            source: source.clone(),
            mempool: mempool.then(|| pool.clone() as Arc<dyn MiningMempool>),
            status: status.clone(),
            clock: clock.clone(),
            params: params(network),
        })
        .with_longpoll_times(times);
        Rig {
            chain,
            pool,
            source,
            status,
            clock,
            engine: Arc::new(engine),
        }
    }

    fn call(&self, longpoll: LongPoll) -> Result<GbtReply, GbtError> {
        self.engine.handle(request(longpoll))
    }

    fn template(&self, longpoll: LongPoll) -> GbtTemplate {
        match self.call(longpoll) {
            Ok(GbtReply::Template(t)) => t,
            other => panic!("expected a template, got {other:?}"),
        }
    }

    /// Runs a call on its own thread.
    fn spawn(&self, longpoll: LongPoll) -> mpsc::Receiver<Result<GbtReply, GbtError>> {
        let engine = Arc::clone(&self.engine);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(engine.handle(request(longpoll)));
        });
        rx
    }

    /// Every check after the proposal dispatch fails.
    fn template_refused_everywhere(&self) {
        refuse_all(self);
        self.status.connman.store(false, Ordering::SeqCst);
    }

    fn cache_prev(&self) -> Option<BlockHash> {
        self.engine.lock().prev
    }

    fn tx_updated_last(&self) -> u32 {
        self.engine.lock().tx_updated_last
    }
}

fn request(longpoll: LongPoll) -> GbtRequest {
    GbtRequest {
        mode: Mode::Template,
        rules: BTreeSet::new(),
        longpoll,
    }
}

fn template_of(reply: Result<GbtReply, GbtError>) -> GbtTemplate {
    match reply {
        Ok(GbtReply::Template(t)) => t,
        other => panic!("expected a template, got {other:?}"),
    }
}

fn short() -> LongpollTimes {
    LongpollTimes {
        first: Duration::from_millis(20),
        step: Duration::from_millis(20),
    }
}

const TEST_CHAINS: [Network; 3] = [Network::Testnet3, Network::Devnet, Network::Regtest];

/// Mainnet's first superblock height at or after its start, less one: the tip height where the
/// next block is a superblock.
fn mainnet_superblock_tip(p: &GbtParams) -> u32 {
    let next = (p.superblocks.start()..).find(|&h| p.superblocks.is_superblock_height(h)).unwrap();
    u32::try_from(next - 1).unwrap()
}

fn refuse_all(rig: &Rig) {
    let tip = mainnet_superblock_tip(&params(Network::Main));
    rig.chain.set_tip(hash(1), tip);
    rig.chain.state().ibd = true;
    rig.status.peers.store(0, Ordering::SeqCst);
    rig.status.synced.store(false, Ordering::SeqCst);
}

// Core: v24.0.2 rpc/mining.cpp:741-754
#[test]
fn mainnet_refuses_in_core_order() {
    let rig = Rig::new(Network::Main);
    refuse_all(&rig);
    assert_eq!(rig.call(LongPoll::None), Err(GbtError::NotConnected));
    rig.status.peers.store(1, Ordering::SeqCst);
    assert_eq!(rig.call(LongPoll::None), Err(GbtError::InIbd));
    rig.chain.state().ibd = false;
    assert_eq!(rig.call(LongPoll::None), Err(GbtError::Syncing));
    let codes: Vec<_> = [GbtError::NotConnected, GbtError::InIbd, GbtError::Syncing]
        .iter()
        .map(GbtError::code)
        .collect();
    assert_eq!(codes, [-9, -10, -10]);
    // Not synced, but the next block is no superblock.
    let (_, height) = rig.chain.tip();
    rig.chain.set_tip(hash(2), height + 1);
    rig.template(LongPoll::None);
    rig.status.synced.store(true, Ordering::SeqCst);
    rig.chain.set_tip(hash(3), height);
    rig.template(LongPoll::None);
    assert_eq!(rig.source.builds(), 2);
}

// Core: v24.0.2 rpc/mining.cpp:799-806, the refusals are not rerun after the wake.
#[test]
fn mainnet_longpoll_does_not_rerun_the_refusals_after_the_wake() {
    let rig = Rig::new(Network::Main);
    let rx = rig.spawn(LongPoll::Id {
        tip: hash(1),
        tx_updated: 0,
    });
    rig.chain.wait_for_waiters(1);
    rig.chain.state().ibd = true;
    rig.status.peers.store(0, Ordering::SeqCst);
    rig.status.synced.store(false, Ordering::SeqCst);
    rig.chain.set_tip(hash(2), mainnet_superblock_tip(&params(Network::Main)));
    let t = template_of(rx.recv_timeout(PATIENCE).unwrap());
    assert_eq!(t.previousblockhash, hash(2));
    assert_eq!(rig.call(LongPoll::None), Err(GbtError::NotConnected));
}

#[test]
fn test_chains_bypass_the_refusals() {
    for network in TEST_CHAINS {
        let rig = Rig::new(network);
        refuse_all(&rig);
        let t = rig.template(LongPoll::None);
        assert_eq!(t.previousblockhash, hash(1), "{network:?}");
    }
}

#[test]
fn p2p_precedes_and_mempool_follows_the_refusals() {
    for network in [Network::Main].into_iter().chain(TEST_CHAINS) {
        let rig = Rig::with(network, false, LongpollTimes::default());
        refuse_all(&rig);
        rig.status.connman.store(false, Ordering::SeqCst);
        assert_eq!(rig.call(LongPoll::None), Err(GbtError::P2pDisabled), "{network:?}");
        assert_eq!(rig.chain.tip_turns.load(Ordering::SeqCst), 0);
        rig.status.connman.store(true, Ordering::SeqCst);
        let expected = if network == Network::Main {
            GbtError::NotConnected
        } else {
            GbtError::MempoolDisabled
        };
        assert_eq!(rig.call(LongPoll::None), Err(expected), "{network:?}");
        rig.status.peers.store(1, Ordering::SeqCst);
        rig.status.synced.store(true, Ordering::SeqCst);
        rig.chain.state().ibd = false;
        assert_eq!(rig.call(LongPoll::None), Err(GbtError::MempoolDisabled), "{network:?}");
    }
}

#[test]
fn invalid_mode_precedes_every_check() {
    let rig = Rig::with(Network::Main, false, LongpollTimes::default());
    refuse_all(&rig);
    rig.status.connman.store(false, Ordering::SeqCst);
    let req = GbtRequest {
        mode: Mode::Invalid,
        ..request(LongPoll::None)
    };
    assert_eq!(rig.engine.handle(req), Err(GbtError::InvalidMode));
}

#[test]
fn error_codes_and_texts_are_cores() {
    let cases = [
        (GbtError::InvalidMode, -8, "Invalid mode"),
        (GbtError::P2pDisabled, -31, "Error: Peer-to-peer functionality missing or disabled"),
        (GbtError::NotConnected, -9, "pkg is not connected!"),
        (GbtError::InIbd, -10, "pkg is in initial sync and waiting for blocks..."),
        (GbtError::Syncing, -10, "pkg is syncing with network..."),
        (GbtError::MempoolDisabled, -33, "Mempool disabled or instance not found"),
        (GbtError::ShuttingDown, -9, "Shutting down"),
        (
            GbtError::Template(TemplateError::Failed("CreateNewBlock: x".into())),
            -1,
            "CreateNewBlock: x",
        ),
        (
            GbtError::Template(TemplateError::NotReady {
                height: 7,
                rule: "r",
                owner: "o",
            }),
            -1,
            "block 7 needs r, which is not served until o",
        ),
        (GbtError::Template(TemplateError::NoChain), -32603, "Node chainman not found"),
    ];
    for (e, code, text) in cases {
        assert_eq!((e.code(), e.message("pkg").as_str()), (code, text));
    }
}

// Core: v24.0.2 rpc/mining.cpp:785-799
#[test]
fn longpoll_waits_for_the_tip_and_never_reads_the_counter_before_a_deadline() {
    let rig = Rig::new(Network::Regtest);
    rig.template(LongPoll::None);
    let reads = rig.pool.r1_reads.load(Ordering::SeqCst);
    rig.pool.bump();
    let rx = rig.spawn(LongPoll::Id {
        tip: hash(1),
        tx_updated: 0,
    });
    rig.chain.wait_for_waiters(1);
    assert!(rx.try_recv().is_err());
    assert_eq!(rig.pool.r1_reads.load(Ordering::SeqCst), reads);
    rig.chain.set_tip(hash(2), 101);
    let t = template_of(rx.recv_timeout(PATIENCE).unwrap());
    assert_eq!((t.previousblockhash, t.height), (hash(2), 102));
    assert_eq!(rig.chain.waits().iter().map(|w| w.1).collect::<Vec<_>>(), [Some(WaitEnd::Woken)]);
}

#[test]
fn longpoll_deadlines_are_first_then_steps_on_one_steady_clock() {
    let times = LongpollTimes {
        first: Duration::from_millis(30),
        step: Duration::from_millis(17),
    };
    let rig = Rig::with(Network::Regtest, true, times);
    let start = Instant::now();
    let rx = rig.spawn(LongPoll::Id {
        tip: hash(1),
        tx_updated: 0,
    });
    rig.chain.wait_for_waits(4);
    rig.chain.set_tip(hash(2), 101);
    template_of(rx.recv_timeout(PATIENCE).unwrap());
    let waits = rig.chain.waits();
    assert!(waits[0].0 >= start + times.first);
    for pair in waits.windows(2) {
        assert_eq!(pair[1].0 - pair[0].0, times.step);
    }
    let ends: Vec<_> = waits.iter().map(|w| w.1).collect();
    assert!(ends[..ends.len() - 1].iter().all(|e| *e == Some(WaitEnd::Deadline)));
    assert_eq!(*ends.last().unwrap(), Some(WaitEnd::Woken));
}

#[test]
fn longpoll_ends_at_the_first_deadline_after_an_update() {
    let rig = Rig::with(Network::Regtest, true, short());
    rig.pool.bump();
    let rx = rig.spawn(LongPoll::Id {
        tip: hash(1),
        tx_updated: 0,
    });
    let t = template_of(rx.recv_timeout(PATIENCE).expect("the first deadline ends the wait"));
    assert_eq!(t.previousblockhash, hash(1));
    assert_eq!(
        rig.chain.waits().iter().map(|w| w.1).collect::<Vec<_>>(),
        [Some(WaitEnd::Deadline)]
    );
}

#[test]
fn longpoll_default_steps_are_60_then_10_seconds() {
    assert_eq!(
        LongpollTimes::default(),
        LongpollTimes {
            first: Duration::from_secs(60),
            step: Duration::from_secs(10),
        }
    );
    let rig = Rig::new(Network::Regtest);
    let start = Instant::now();
    let rx = rig.spawn(LongPoll::NonString);
    rig.chain.wait_for_waiters(1);
    let deadline = rig.chain.waits()[0].0;
    assert!(deadline >= start + Duration::from_secs(60));
    assert!(deadline <= Instant::now() + Duration::from_secs(60));
    rig.chain.set_tip(hash(2), 101);
    template_of(rx.recv_timeout(PATIENCE).unwrap());
}

// Core: v24.0.2 rpc/mining.cpp:786, :802-803
#[test]
fn shutdown_ends_a_longpoll_before_its_deadline() {
    let rig = Rig::new(Network::Regtest);
    let start = Instant::now();
    let rx = rig.spawn(LongPoll::NonString);
    rig.chain.wait_for_waiters(1);
    rig.status.rpc.store(false, Ordering::SeqCst);
    rig.chain.wake_all();
    assert_eq!(rx.recv_timeout(PATIENCE).unwrap(), Err(GbtError::ShuttingDown));
    assert!(start.elapsed() < Duration::from_secs(60));
    assert_eq!(rig.chain.waits().iter().map(|w| w.1).collect::<Vec<_>>(), [Some(WaitEnd::Woken)]);
    assert_eq!(rig.source.builds(), 0);
}

#[test]
fn two_waiters_wake_on_one_tip_change_and_on_shutdown() {
    let rig = Rig::new(Network::Regtest);
    let a = rig.spawn(LongPoll::NonString);
    let b = rig.spawn(LongPoll::Id {
        tip: hash(1),
        tx_updated: 0,
    });
    rig.chain.wait_for_waiters(2);
    rig.chain.set_tip(hash(2), 101);
    for rx in [a, b] {
        assert_eq!(template_of(rx.recv_timeout(PATIENCE).unwrap()).previousblockhash, hash(2));
    }
    let a = rig.spawn(LongPoll::NonString);
    let b = rig.spawn(LongPoll::NonString);
    rig.chain.wait_for_waiters(2);
    rig.status.rpc.store(false, Ordering::SeqCst);
    rig.chain.wake_all();
    for rx in [a, b] {
        assert_eq!(rx.recv_timeout(PATIENCE).unwrap(), Err(GbtError::ShuttingDown));
    }
}

#[test]
fn a_call_completes_while_another_waits() {
    let rig = Rig::new(Network::Regtest);
    let waiter = rig.spawn(LongPoll::NonString);
    rig.chain.wait_for_waiters(1);
    let other = rig.spawn(LongPoll::None);
    assert_eq!(template_of(other.recv_timeout(PATIENCE).unwrap()).previousblockhash, hash(1));
    assert!(waiter.try_recv().is_err());
    rig.chain.set_tip(hash(2), 101);
    template_of(waiter.recv_timeout(PATIENCE).unwrap());
}

// Core: v24.0.2 rpc/mining.cpp:775-779
#[test]
fn a_non_string_id_watches_the_tip_with_the_caches_counter() {
    let rig = Rig::with(Network::Regtest, true, short());
    rig.template(LongPoll::None);
    assert_eq!(rig.tx_updated_last(), 0);
    rig.pool.bump();
    // The cache still holds 0: the first deadline sees 1 and ends the wait.
    let rx = rig.spawn(LongPoll::NonString);
    template_of(rx.recv_timeout(PATIENCE).expect("the first deadline ends the wait"));
    assert_eq!(
        rig.chain.waits().iter().map(|w| w.1).collect::<Vec<_>>(),
        [Some(WaitEnd::Deadline)]
    );
    // Within 5 seconds the cache kept 0 as well; past them it rebuilt and holds 1.
    rig.clock.set_mocktime(10_006);
    assert_eq!(rig.template(LongPoll::None).longpollid.1, 1);
    let rx = rig.spawn(LongPoll::NonString);
    rig.chain.wait_for_waits(4);
    assert!(rx.try_recv().is_err());
    rig.chain.set_tip(hash(2), 101);
    template_of(rx.recv_timeout(PATIENCE).unwrap());
    let ends: Vec<_> = rig.chain.waits().iter().map(|w| w.1).collect();
    assert!(ends[1..ends.len() - 1].iter().all(|e| *e == Some(WaitEnd::Deadline)));
    assert_eq!(*ends.last().unwrap(), Some(WaitEnd::Woken));
}

// Core: v24.0.2 rpc/mining.cpp:812-813
#[test]
fn the_cache_rebuilds_on_a_new_tip_or_an_update_after_more_than_five_seconds() {
    let rig = Rig::new(Network::Regtest);
    rig.template(LongPoll::None);
    assert_eq!(rig.source.builds(), 1);
    rig.clock.set_mocktime(20_000);
    rig.template(LongPoll::None);
    assert_eq!(rig.source.builds(), 1, "no update: a hit at any time");
    rig.pool.bump();
    rig.clock.set_mocktime(10_005);
    rig.template(LongPoll::None);
    assert_eq!(rig.source.builds(), 1, "exactly 5 seconds: a hit");
    rig.clock.set_mocktime(10_006);
    let t = rig.template(LongPoll::None);
    assert_eq!((rig.source.builds(), t.longpollid.1), (2, 1));
    rig.clock.set_mocktime(10_006);
    rig.chain.set_tip(hash(2), 101);
    let t = rig.template(LongPoll::None);
    assert_eq!((rig.source.builds(), t.previousblockhash), (3, hash(2)));
}

// Core: v24.0.2 rpc/mining.cpp:815-833
#[test]
fn a_failed_build_leaves_no_prev() {
    let rig = Rig::new(Network::Regtest);
    rig.template(LongPoll::None);
    rig.chain.set_tip(hash(2), 101);
    rig.source.fail.store(true, Ordering::SeqCst);
    let failed = GbtError::Template(TemplateError::Failed("CreateNewBlock: injected".into()));
    assert_eq!(rig.call(LongPoll::None), Err(failed.clone()));
    assert_eq!(rig.cache_prev(), None);
    rig.pool.bump();
    assert_eq!(rig.call(LongPoll::None), Err(failed));
    assert_eq!(rig.tx_updated_last(), 1, "the counter is stored before the build");
    rig.source.fail.store(false, Ordering::SeqCst);
    rig.template(LongPoll::None);
    assert_eq!((rig.source.builds(), rig.cache_prev()), (4, Some(hash(2))));
}

// Core: v24.0.2 rpc/mining.cpp:836-837, node/miner.cpp:49-61
#[test]
fn update_time_and_the_nonce_edit_the_cached_block_in_place() {
    let rig = Rig::new(Network::Regtest);
    *rig.source.header.lock().unwrap() = (0x2000_0000, 1_000, 0x207f_ffff, 7);
    rig.chain.state().min_difficulty_bits = Some(0x1d00_ffff);
    let t = rig.template(LongPoll::None);
    assert_eq!((t.curtime, t.bits, t.mintime), (10_000, 0x1d00_ffff, 1_001));
    assert_eq!(rig.engine.lock().cached().nonce, 0);
    // A hit keeps the raised time when the clock steps back, and the recomputed bits.
    rig.chain.state().min_difficulty_bits = None;
    rig.clock.set_mocktime(9_000);
    let t = rig.template(LongPoll::None);
    assert_eq!((t.curtime, t.bits, rig.source.builds()), (10_000, 0x1d00_ffff, 1));
    rig.clock.set_mocktime(10_500);
    assert_eq!(rig.template(LongPoll::None).curtime, 10_500);
}

#[test]
fn update_time_is_cores() {
    assert_eq!(update_time(100, 200, 150), 201);
    assert_eq!(update_time(100, 200, 300), 300);
    assert_eq!(update_time(400, 200, 300), 400);
    assert_eq!(update_time(201, 200, 0), 201);
    assert_eq!(update_time(0, i64::from(u32::MAX) + 5, 0), 5);
}

// PLAN-MN1 §3.3: Core holds cs_main from the cache check to the reply
#[test]
fn a_tip_moved_before_the_fields_turn_reruns_the_cache_check() {
    let rig = Rig::new(Network::Regtest);
    rig.chain.state().moves.push_back((hash(2), 101));
    let t = rig.template(LongPoll::None);
    assert_eq!((t.previousblockhash, t.longpollid.0, t.height), (hash(2), hash(2), 102));
    assert_eq!(rig.source.builds(), 2);
    assert_eq!(rig.chain.fields_turns.load(Ordering::SeqCst), 2);
}

/// R1 (a): the counter is read while a pool writer holds its lock, and sees the writer's bump.
#[test]
fn the_counter_is_read_while_a_writer_holds_the_pool() {
    let rig = Rig::with(Network::Regtest, true, short());
    rig.template(LongPoll::None);
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let pool = Arc::clone(&rig.pool);
    let writer = thread::spawn(move || {
        let _guard = pool.lock.write().unwrap();
        pool.bump();
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    held_rx.recv().unwrap();
    assert_eq!(rig.pool.transactions_updated(), 1);
    let rx = rig.spawn(LongPoll::Id {
        tip: hash(1),
        tx_updated: 0,
    });
    let t = template_of(rx.recv_timeout(PATIENCE).unwrap());
    assert_eq!(t.longpollid.1, 0);
    assert_eq!(
        rig.chain.waits().iter().map(|w| w.1).collect::<Vec<_>>(),
        [Some(WaitEnd::Deadline)]
    );
    release_tx.send(()).unwrap();
    writer.join().unwrap();
}

/// R1 (b): an update between the check's load and the rebuild's is the stored value.
// Core: v24.0.2 rpc/mining.cpp:813, :819
#[test]
fn the_rebuild_stores_its_own_second_load() {
    let rig = Rig::new(Network::Regtest);
    rig.template(LongPoll::None);
    rig.pool.bump();
    rig.clock.set_mocktime(10_006);
    let reads = rig.pool.r1_reads.load(Ordering::SeqCst);
    *rig.pool.bump_after_read.lock().unwrap() = Some(reads + 1);
    let t = rig.template(LongPoll::None);
    assert_eq!((rig.source.builds(), t.longpollid.1), (2, 2));
    assert_eq!(rig.pool.r1_reads.load(Ordering::SeqCst), reads + 2);
}

/// R1 (b): an update between the rebuild's load and the pool read is in the template, not in
/// the stored value, and rebuilds once more more than 5 seconds later.
#[test]
fn an_update_after_the_second_load_rebuilds_again_later() {
    let rig = Rig::new(Network::Regtest);
    let pool = Arc::clone(&rig.pool);
    *rig.source.before_read.lock().unwrap() = Some(Box::new(move || {
        if pool.counter.transactions_updated() == 0 {
            pool.bump();
        }
    }));
    rig.template(LongPoll::None);
    let cached = rig.engine.lock().cached().template.block.txs().len();
    assert_eq!((cached, rig.tx_updated_last()), (2, 0));
    rig.clock.set_mocktime(10_005);
    rig.template(LongPoll::None);
    assert_eq!(rig.source.builds(), 1);
    rig.clock.set_mocktime(10_006);
    assert_eq!(rig.template(LongPoll::None).longpollid.1, 1);
    assert_eq!(rig.source.builds(), 2);
}

/// R1 (d): no read of the counter takes the pool lock: only builds do.
#[test]
fn no_counter_read_takes_the_pool_lock() {
    let rig = Rig::with(Network::Regtest, true, short());
    rig.template(LongPoll::None);
    rig.template(LongPoll::None);
    let rx = rig.spawn(LongPoll::NonString);
    rig.chain.wait_for_waits(3);
    rig.pool.bump();
    template_of(rx.recv_timeout(PATIENCE).unwrap());
    rig.clock.set_mocktime(10_006);
    rig.template(LongPoll::None);
    assert!(rig.pool.r1_reads.load(Ordering::SeqCst) >= 6);
    assert_eq!(rig.pool.read_locks.load(Ordering::SeqCst), rig.source.builds());
    assert_eq!(rig.source.builds(), 2);
}

fn coinbase(outputs: Vec<TxOut>, payload: Vec<u8>) -> TxRef {
    let input = crate::assemble::coinbase_input(vec![0x01, 0x65]);
    Arc::new(Tx::new(Transaction::new(3, TxType::new(5), vec![input], outputs, 0, payload)))
}

fn out(value: i64, script: u8) -> TxOut {
    TxOut::from_duffs(value, vec![script].into())
}

fn shaped(rig: &Rig, txs: Vec<TxRef>) {
    let n = txs.len() as i64;
    *rig.source.shape.lock().unwrap() = Some(Shape {
        txs,
        fees: (0..n)
            .map(|i| {
                if i == 0 {
                    -1000
                } else {
                    100 + i
                }
            })
            .collect(),
        sigops: (0..n).map(|i| 10 * i + 1).collect(),
        prev_bits: 0x1e0f_ffff,
        masternode: Vec::new(),
        superblock: Vec::new(),
    });
}

fn depends(t: &GbtTemplate) -> Vec<Vec<u32>> {
    t.transactions.iter().map(|e| e.depends.clone()).collect()
}

// Core: v24.0.2 rpc/mining.cpp:841-871
#[test]
fn depends_index_progressively_from_the_coinbase_per_input() {
    let rig = Rig::new(Network::Regtest);
    let cb = coinbase(vec![out(500, 0x51)], Vec::new());
    let a = spend(&[cb.txid(), cb.txid()], 1);
    let unknown = TxHash::from([9; 32]);
    let b = spend(&[a.txid(), unknown, a.txid(), cb.txid()], 2);
    let later = spend(&[], 3);
    let d = spend(&[later.txid()], 4);
    let e = spend(&[d.txid(), b.txid()], 5);
    shaped(&rig, vec![cb, a.clone(), b, d, later, e]);
    let t = rig.template(LongPoll::None);
    assert_eq!(depends(&t), [vec![0, 0], vec![1, 1, 0], vec![], vec![], vec![3, 2]]);
    let fees: Vec<_> = t.transactions.iter().map(|e| (e.fee, e.sigops)).collect();
    assert_eq!(fees, [(101, 11), (102, 21), (103, 31), (104, 41), (105, 51)]);
    assert_eq!((t.transactions[0].txid, t.transactions[0].tx.txid()), (a.txid(), a.txid()));
}

fn vb(pos: DeploymentPos, name: &'static str, gbt_force: bool, bit: u8) -> VbDeployment {
    VbDeployment {
        pos,
        name,
        gbt_force,
        bit,
    }
}

fn vb_rig(states: Vec<(DeploymentPos, ThresholdState)>) -> Rig {
    let mut rig = Rig::new(Network::Regtest);
    let mut p = params(Network::Regtest);
    p.deployments = vec![
        vb(DeploymentPos::TestDummy, "testdummy", true, 28),
        vb(DeploymentPos::V24, "v24", false, 13),
        vb(DeploymentPos::EvoShares, "evo_shares", false, 14),
    ];
    let engine = GbtEngine::new(GbtParts {
        chain: rig.chain.clone(),
        source: rig.source.clone(),
        mempool: Some(rig.pool.clone()),
        status: rig.status.clone(),
        clock: rig.clock.clone(),
        params: p,
    });
    rig.engine = Arc::new(engine);
    rig.chain.state().versionbits = states;
    *rig.source.header.lock().unwrap() = (0x2000_6000, 1_000, 0x207f_ffff, 0);
    rig
}

fn with_rules(rig: &Rig, rules: &[&str]) -> Result<GbtTemplate, GbtError> {
    let req = GbtRequest {
        rules: rules.iter().map(|r| (*r).to_owned()).collect(),
        ..request(LongPoll::None)
    };
    rig.engine.handle(req).map(|reply| match reply {
        GbtReply::Template(t) => t,
        GbtReply::Proposal(_) => panic!("a template request answered a proposal"),
    })
}

// Core: v24.0.2 rpc/mining.cpp:885-934
#[test]
fn version_bits_build_rules_and_edit_the_cached_version() {
    use ThresholdState::{Active, Defined, Failed, LockedIn, Started};
    let rig = vb_rig(vec![
        (DeploymentPos::TestDummy, LockedIn),
        (DeploymentPos::V24, Started),
        (DeploymentPos::EvoShares, Active),
    ]);
    // The active non-force rule throws after the loop's earlier edits, which the cache keeps.
    assert_eq!(with_rules(&rig, &[]), Err(GbtError::RuleRequired("evo_shares".into())));
    assert_eq!(rig.engine.lock().cached().version, 0x3000_4000);
    // The client lists the name, not the gbt name.
    assert_eq!(
        with_rules(&rig, &["!evo_shares"]),
        Err(GbtError::RuleRequired("evo_shares".into()))
    );
    let t = with_rules(&rig, &["evo_shares"]).unwrap();
    assert_eq!(t.rules, ["csv", "!evo_shares"]);
    assert_eq!(t.vbavailable, [("testdummy".to_owned(), 28), ("!v24".to_owned(), 13)]);
    assert_eq!((t.version, t.vbrequired), (0x3000_4000, 0));
    // Listing v24 now keeps its bit, but the cleared bit stays cleared on the cached block.
    let t = with_rules(&rig, &["evo_shares", "v24"]).unwrap();
    assert_eq!(t.version, 0x3000_4000);
    assert_eq!(rig.source.builds(), 1);
    // A fresh block keeps a listed started bit and a force deployment's.
    rig.chain.set_tip(hash(2), 101);
    rig.chain.state().versionbits = vec![
        (DeploymentPos::TestDummy, Started),
        (DeploymentPos::V24, Started),
        (DeploymentPos::EvoShares, Defined),
    ];
    *rig.source.header.lock().unwrap() = (0x3000_6000, 1_000, 0x207f_ffff, 0);
    let t = with_rules(&rig, &["v24"]).unwrap();
    assert_eq!((t.version, t.rules.as_slice()), (0x3000_6000, &["csv".to_owned()][..]));
    assert_eq!(t.vbavailable, [("testdummy".to_owned(), 28), ("!v24".to_owned(), 13)]);
    // Defined and failed deployments are not shown and leave the version alone.
    rig.chain.set_tip(hash(3), 102);
    rig.chain.state().versionbits =
        vec![(DeploymentPos::V24, Failed), (DeploymentPos::EvoShares, Defined)];
    let t = with_rules(&rig, &[]).unwrap();
    assert_eq!((t.version, t.rules.len(), t.vbavailable.len()), (0x3000_6000, 1, 0));
}

#[test]
fn gbt_vb_names_follow_gbt_force() {
    assert_eq!(gbt_vb_name("v24", true), "v24");
    assert_eq!(gbt_vb_name("v24", false), "!v24");
    let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let names: Vec<_> = params(Network::Regtest)
        .deployments
        .iter()
        .map(|d| (d.pos, d.name, d.gbt_force, i32::from(d.bit)))
        .collect();
    let expected: Vec<_> =
        setup.consensus.deployments.iter().map(|(pos, d)| (pos, pos.name(), true, d.bit)).collect();
    assert_eq!(names, expected);
    assert_eq!(names.iter().map(|n| n.1).collect::<Vec<_>>(), ["testdummy", "v24", "evo_shares"]);
}

// Core: v24.0.2 rpc/mining.cpp:938
#[test]
fn the_longpoll_id_is_the_reply_tip_and_the_caches_counter() {
    let rig = Rig::new(Network::Regtest);
    rig.pool.bump();
    assert_eq!(rig.template(LongPoll::None).longpollid, (hash(1), 1));
    rig.pool.bump();
    rig.pool.bump();
    assert_eq!(rig.template(LongPoll::None).longpollid, (hash(1), 1));
    rig.chain.state().moves.push_back((hash(2), 101));
    assert_eq!(rig.template(LongPoll::None).longpollid, (hash(2), 3));
}

// Core: v24.0.2 rpc/mining.cpp:942-982
#[test]
fn payments_started_flags_and_coinbase_fields() {
    let rig = Rig::new(Network::Regtest);
    let p = params(Network::Regtest);
    let mn = vec![out(30, 0x52), out(20, 0x53)];
    let sb = vec![out(7, 0x54)];
    let cb = coinbase(
        vec![out(400, 0x51), out(30, 0x52), out(20, 0x53), out(7, 0x54)],
        vec![2, 0, 9, 8],
    );
    shaped(&rig, vec![cb]);
    rig.source.shape.lock().unwrap().as_mut().unwrap().masternode = mn.clone();
    rig.source.shape.lock().unwrap().as_mut().unwrap().superblock = sb.clone();
    let flags = |tip: i32| {
        rig.chain.set_tip(hash((tip % 200) as u8 + 10), u32::try_from(tip).unwrap());
        let t = rig.template(LongPoll::None);
        (t.masternode_payments_started, t.superblocks_started)
    };
    let mn_start = p.masternode_payments_start;
    let sb_start = p.superblocks.start();
    assert_ne!(mn_start, sb_start);
    assert!(!flags(mn_start - 1).0);
    assert!(flags(mn_start).0);
    assert!(!flags(sb_start - 1).1);
    assert!(flags(sb_start).1);
    let t = rig.template(LongPoll::None);
    assert_eq!((t.masternode, t.superblock), (mn, sb));
    assert_eq!((t.masternode_payments_enforced, t.superblocks_enabled), (true, true));
    assert_eq!((t.coinbasevalue, t.coinbase_payload), (457, vec![2, 0, 9, 8]));
    assert_eq!((t.previousbits, t.height), (0x1e0f_ffff, sb_start as u32 + 1));
    assert!(t.transactions.is_empty());
}

// Core: v24.0.2 rpc/mining.cpp:874, :942-943; consensus/consensus.h:12-19
#[test]
fn limits_follow_dip0001_after_the_parent() {
    let rig = Rig::new(Network::Regtest);
    rig.chain.state().dip0001 = false;
    let t = rig.template(LongPoll::None);
    assert_eq!((t.sizelimit, t.sigoplimit), (1_000_000, 20_000));
    rig.chain.state().dip0001 = true;
    let t = rig.template(LongPoll::None);
    assert_eq!((t.sizelimit, t.sigoplimit), (2_000_000, 40_000));
}

// Core: v24.0.2 rpc/mining.cpp:874 arith_uint256().SetCompact(nBits)
#[test]
fn the_target_is_set_compact_without_its_flags() {
    let rig = Rig::new(Network::Regtest);
    let target = |bits: u32| {
        rig.chain.set_tip(hash((bits & 0x7f) as u8 + 100), 100);
        *rig.source.header.lock().unwrap() = (0x2000_0000, 1_000, bits, 0);
        let t = rig.template(LongPoll::None);
        assert_eq!(t.bits, bits);
        t.target
    };
    assert_eq!(target(0x1d00_ffff), Arith256::from_u64(0xffff).wrapping_shl(8 * 26));
    assert_eq!(target(0x0492_3456), Arith256::from_u64(0x1234_5600));
    assert_eq!(target(0x2300_0001), Arith256::from_u64(0));
    assert_eq!(target(0x207f_ffff), CompactTarget::new(0x207f_ffff).expand().value);
}

// Core: v24.0.2 rpc/mining.cpp:697-724
#[test]
fn a_proposal_passes_vs_verdict_through_and_touches_nothing_else() {
    let rig = Rig::with(Network::Main, false, LongpollTimes::default());
    rig.template_refused_everywhere();
    let block =
        Arc::new(template_on(hash(1), 101, (0x2000_0000, 1, 0x207f_ffff, 0), Vec::new()).block);
    let invalid = BlockValidationError::new(BlockValidationResult::Consensus, "bad-cb-amount");
    let verdicts = [
        Ok(BlockState::Valid),
        Ok(BlockState::Invalid(invalid)),
        Ok(BlockState::Error("disk".to_owned())),
        Err(NotTested::Duplicate),
        Err(NotTested::DuplicateInvalid),
        Err(NotTested::DuplicateInconclusive),
        Err(NotTested::NotBestPrev),
    ];
    for verdict in verdicts {
        *rig.chain.verdict.lock().unwrap() = verdict.clone();
        let req = GbtRequest {
            mode: Mode::Proposal(Arc::clone(&block)),
            ..request(LongPoll::NonString)
        };
        assert_eq!(rig.engine.handle(req), Ok(GbtReply::Proposal(verdict)));
    }
    assert_eq!(*rig.chain.proposed.lock().unwrap(), vec![block.hash(); 7]);
    let turns = (
        rig.chain.tip_turns.load(Ordering::SeqCst),
        rig.chain.fields_turns.load(Ordering::SeqCst),
        rig.chain.waits().len(),
    );
    assert_eq!(
        (turns, rig.source.builds(), rig.pool.r1_reads.load(Ordering::SeqCst)),
        ((0, 0, 0), 0, 0)
    );
}

#[test]
fn a_proposal_leaves_the_cache_as_it_was() {
    let rig = Rig::new(Network::Regtest);
    rig.template(LongPoll::None);
    let before = {
        let cache = rig.engine.lock();
        (
            cache.prev,
            cache.tx_updated_last,
            cache.time_start,
            cache.template.as_ref().map(|c| (c.version, c.time, c.bits)),
        )
    };
    rig.pool.bump();
    rig.clock.set_mocktime(20_000);
    rig.chain.set_tip(hash(2), 101);
    let block =
        Arc::new(template_on(hash(2), 102, (0x2000_0000, 1, 0x207f_ffff, 0), Vec::new()).block);
    let req = GbtRequest {
        mode: Mode::Proposal(block),
        ..request(LongPoll::None)
    };
    assert_eq!(rig.engine.handle(req), Ok(GbtReply::Proposal(Ok(BlockState::Valid))));
    let after = {
        let cache = rig.engine.lock();
        (
            cache.prev,
            cache.tx_updated_last,
            cache.time_start,
            cache.template.as_ref().map(|c| (c.version, c.time, c.bits)),
        )
    };
    assert_eq!((after, rig.source.builds()), (before, 1));
}
