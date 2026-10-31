//! The V rig: regtest blocks mined on the fly, a signal recorder, and V on a fresh genesis.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dash_chain::{
    ChainConfig, FailStop, FlatFilePos, HeaderBatch, HeaderChain, Services, StaticSporks, Status,
    Work,
};
use dash_clock::MockClock;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{Header, Tx};
use dash_consensus::{Block, BlockId};
use dash_num::CompactTarget;
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, BlockHeader, Transaction};
use dash_types::codec::BaseCodec as _;

use crate::chain::actor::V;
use crate::chain::load::LoadPrune;
use crate::chain::reply::{WaitError, reply};
use crate::chain::{
    AbortSink, BlockConnector, BlockFiles, BlockResult, BlockSource, BlockState, Blocking, CandKey,
    ChainCommand, ChainHandle, CoreChecks, Fatal, FlushSite, GuardScope, IsConflicts, LoadedIndex,
    LocalShutdown, MemBlockFiles, MempoolSink, NewBlock, NoFlush, NoInstantSend, NoUi,
    NodeBlockIndex, NodeJoinHandle, NullConnector, NullMempool, ReadAheadSetup, RecordingAbort,
    ShutdownFlag, SignalsThread, StartError, UiNotifications, VExit, VParts, ValidationInterface,
    ValidationSignals, genesis_block, load_chain_tip, open_block_index, start,
};

pub(crate) const REGTEST_BITS: u32 = 0x207f_ffff;
/// Block spacing in the mined chains.
pub(crate) const SPACING: u32 = 150;

pub(crate) fn regtest() -> Arc<ChainParams> {
    Arc::new(
        ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest"),
    )
}

/// A mined block and its height.
#[derive(Clone)]
pub(crate) struct Mined {
    pub block: Arc<Block>,
    pub height: u32,
}

impl Mined {
    /// `p`'s genesis block, at height 0.
    pub fn genesis(p: &ChainParams) -> Mined {
        Mined {
            block: genesis_block(p),
            height: 0,
        }
    }

    pub fn hash(&self) -> BlockHash {
        self.block.hash()
    }

    pub fn header(&self) -> Header {
        *self.block.header()
    }

    pub fn time(&self) -> u32 {
        self.block.header().raw().time
    }
}

/// Core `CScript() << n` for a coinbase height (`script.h` `push_int64`): `OP_1`…`OP_16`, else
/// the minimal `CScriptNum` push.
pub(crate) fn height_push(n: u32) -> Vec<u8> {
    match n {
        0 => vec![0x00],
        1..=16 => vec![0x50 + n as u8],
        _ => {
            let mut num = n.to_le_bytes().to_vec();
            while num.last() == Some(&0) {
                num.pop();
            }
            if num.last().is_some_and(|b| b & 0x80 != 0) {
                num.push(0);
            }
            let mut out = vec![num.len() as u8];
            out.extend(num);
            out
        }
    }
}

/// A version-1 coinbase with `script_sig`, paying 0 to `OP_TRUE`.
pub(crate) fn coinbase(script_sig: &[u8]) -> Arc<Tx> {
    let mut raw = 1i32.to_le_bytes().to_vec();
    raw.push(1);
    raw.extend([0u8; 32]);
    raw.extend(u32::MAX.to_le_bytes());
    raw.push(script_sig.len() as u8);
    raw.extend(script_sig);
    raw.extend(u32::MAX.to_le_bytes());
    raw.push(1);
    raw.extend(0i64.to_le_bytes());
    raw.extend([1, 0x51]);
    raw.extend(0u32.to_le_bytes());
    let tx = Transaction::decode(&mut raw.as_slice()).expect("a coinbase");
    Arc::new(Tx::new(tx))
}

/// The coinbase scriptSig Core's BIP34 rule expects at `height`, with `salt` pushed after it so
/// sibling blocks differ.
pub(crate) fn cb_script(height: u32, salt: u32) -> Vec<u8> {
    let mut s = height_push(height);
    s.push(4);
    s.extend(salt.to_le_bytes());
    s
}

/// Mines `txs` on `prev` at `time`: the merkle root, then a nonce under regtest's limit.
pub(crate) fn mine(p: &ChainParams, prev: BlockHash, time: u32, txs: Vec<Arc<Tx>>) -> Arc<Block> {
    mine_version(p, prev, time, txs, 0x2000_0000)
}

/// [`mine`] with `version`.
pub(crate) fn mine_version(
    p: &ChainParams,
    prev: BlockHash,
    time: u32,
    txs: Vec<Arc<Tx>>,
    version: i32,
) -> Arc<Block> {
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let mut raw = BlockHeader {
        version,
        prev_hash: prev,
        time,
        bits: REGTEST_BITS,
        ..BlockHeader::default()
    };
    raw.merkle_root = Block::new(Header::new(raw), txs.clone()).merkle().0;
    let header = (0..)
        .map(|nonce| {
            Header::new(BlockHeader {
                nonce,
                ..raw
            })
        })
        .find(|h| check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), &pow))
        .expect("a nonce");
    Arc::new(Block::new(header, txs))
}

/// A valid child of `parent`.
pub(crate) fn child(p: &ChainParams, parent: &Mined, salt: u32) -> Mined {
    let height = parent.height + 1;
    let cb = coinbase(&cb_script(height, salt));
    Mined {
        block: mine(p, parent.hash(), parent.time() + SPACING, vec![cb]),
        height,
    }
}

/// `n` valid blocks on `parent`.
pub(crate) fn branch(p: &ChainParams, parent: &Mined, n: u32, salt: u32) -> Vec<Mined> {
    let mut out: Vec<Mined> = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let next = child(p, out.last().unwrap_or(parent), salt);
        out.push(next);
    }
    out
}

/// An exact reference for the tables `CheckBlockIndex` only bounds (Sol cp1 #5), computed from
/// the entries alone. An entry with data has had its transactions processed (`nTx > 0`), and
/// until a prune (`have_pruned`) the converse holds too (`validation.cpp:5696-5708`).
/// - `nChainTx`: an entry's `nTx` plus its parent's total, 0 at and above any entry without
///   data (`validation.cpp:4278-4298`);
/// - each key in `setBlockIndexCandidates` is its member's live key: the entry's work, its
///   current `nSequenceId` and its id (`blockstorage.cpp:32-49`);
/// - `m_blocks_unlinked` holds each pair (prev, e) at most once, only where `e` has data and
///   `prev` has no transactions linked, and every such pair with no `BLOCK_FAILED_VALID` at or
///   below `e`. One under an entry that failed after `e` arrived may stay (`:5777-5782`).
pub(crate) fn exact_tables(
    chain: &HeaderChain,
    ix: &NodeBlockIndex,
    have_pruned: bool,
) -> Result<(), String> {
    let r = chain.reader();
    let mut entries: Vec<_> = chain.ids().map(|id| r.entry(id).expect("indexed")).collect();
    entries.sort_by_key(|e| (e.height, e.id));
    let mut chain_tx = HashMap::new();
    let mut failed = HashMap::new();
    let (mut required, mut allowed) = (Vec::new(), Vec::new());
    for e in &entries {
        let data = e.status.has(Status::HAVE_DATA);
        if (data && e.n_tx == 0) || (!have_pruned && !data && e.n_tx > 0) {
            return Err(format!("{}: HAVE_DATA without nTx, or nTx without it", e.id.0));
        }
        let below = e.prev.map(|p| (p, chain_tx[&p]));
        let total = match below {
            None => e.n_tx,
            Some((_, t)) if t > 0 && e.n_tx > 0 => t + e.n_tx,
            Some(_) => 0,
        };
        chain_tx.insert(e.id, total);
        if ix.n_chain_tx(e.id) != total {
            return Err(format!("nChainTx of {}: {} not {total}", e.id.0, ix.n_chain_tx(e.id)));
        }
        let fails = e.status.has(Status::FAILED_VALID) || e.prev.is_some_and(|p| failed[&p]);
        failed.insert(e.id, fails);
        if let Some((p, 0)) = below
            && e.status.has(Status::HAVE_DATA)
        {
            let pairs = if fails {
                &mut allowed
            } else {
                &mut required
            };
            pairs.push((p, e.id));
        }
    }
    for key in ix.candidate_keys() {
        let e = r.entry(key.id()).expect("indexed");
        let live = CandKey {
            work: e.chain_work,
            seq: Reverse(ix.sequence_id(e.id)),
            id: Reverse(e.id),
        };
        if key != live {
            return Err(format!("candidate {}: key {key:?}, live {live:?}", e.id.0));
        }
    }
    let held = sorted(ix.unlinked());
    let mut kept = held.clone();
    kept.retain(|pair| !required.contains(pair));
    if held.windows(2).any(|w| w[0] == w[1])
        || held.len() - kept.len() != required.len()
        || kept.iter().any(|pair| !allowed.contains(pair))
    {
        return Err(format!(
            "m_blocks_unlinked {held:?}: needs {required:?}, may keep {allowed:?}"
        ));
    }
    Ok(())
}

/// Captures `accept.rs`'s, `seams.rs`'s (the recording sink's `***` lines), `load.rs`'s,
/// `abc.rs`'s, `failure.rs`'s, `store/blocks.rs`'s, ST2's `blockstorage`, `connect::connector`'s,
/// `connect::disconnect`'s and `evo::node`'s log lines, and `connect::serial`'s errors and INFO lines (its
/// `LogPrintf("ERROR: …")` ports, CH3a D65), from every thread (V logs on its own) with a global
/// subscriber, set once; nothing else in this crate's tests sets one, and the install fails loudly
/// if something does. Others run beside a test, so it counts only lines naming its own blocks
/// ([`accept_levels`], [`lines_with`]), lines its own threads logged ([`levels_here`],
/// [`levels_on`]) or lines it alone logs; a line every caller logs, only for its presence.
pub(crate) fn install_accept_log() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(Capture).expect("the only global subscriber");
    });
}

/// The levels `line` was logged at since [`install_accept_log`], in order.
pub(crate) fn accept_levels(line: &str) -> Vec<tracing::Level> {
    let log = CAPTURED.lock().expect("log");
    log.iter().filter(|(_, _, l)| l == line).map(|(_, level, _)| *level).collect()
}

/// [`accept_levels`] for the lines this thread logged: for a line every caller logs, from code
/// the test runs on its own thread (`open_block_index`).
pub(crate) fn levels_here(line: &str) -> Vec<tracing::Level> {
    levels_on(std::thread::current().id(), line)
}

/// [`accept_levels`] for the lines `thread` logged: for a line every caller logs, from one rig's
/// V ([`Rig::v_thread`]).
pub(crate) fn levels_on(thread: std::thread::ThreadId, line: &str) -> Vec<tracing::Level> {
    let log = CAPTURED.lock().expect("log");
    log.iter().filter(|(t, _, l)| *t == thread && l == line).map(|(_, level, _)| *level).collect()
}

/// The lines logged since [`install_accept_log`] that contain `needle`, in order.
pub(crate) fn lines_with(needle: &str) -> Vec<String> {
    let log = CAPTURED.lock().expect("log");
    log.iter().filter(|(_, _, l)| l.contains(needle)).map(|(_, _, l)| l.clone()).collect()
}

/// The lines `thread` logged since [`install_accept_log`], in order: one rig's V's
/// ([`Rig::v_thread`]), for the order of its lines.
pub(crate) fn lines_on(thread: std::thread::ThreadId) -> Vec<String> {
    lines_of(&[thread])
}

/// The lines `threads` logged since [`install_accept_log`], in order: W's and V's, for the order
/// of W's report and V's lines (BL-554).
pub(crate) fn lines_of(threads: &[std::thread::ThreadId]) -> Vec<String> {
    let log = CAPTURED.lock().expect("log");
    log.iter().filter(|(t, _, _)| threads.contains(t)).map(|(_, _, l)| l.clone()).collect()
}

/// `lines` ([`lines_on`]) log `first` before `then`, the first time each is logged.
pub(crate) fn assert_logged_before(lines: &[String], first: &str, then: &str) {
    let at = |line: &str| {
        lines
            .iter()
            .position(|l| l == line)
            .unwrap_or_else(|| panic!("{line:?} not logged: {lines:#?}"))
    };
    assert!(at(first) < at(then), "{lines:#?}");
}

static CAPTURED: Mutex<Vec<(std::thread::ThreadId, tracing::Level, String)>> =
    Mutex::new(Vec::new());

/// [`install_accept_log`]'s subscriber.
struct Capture;

impl tracing::Subscriber for Capture {
    fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
        matches!(
            m.target(),
            "dashd_chainstate::chain::accept"
                | "dashd_chainstate::chain::seams"
                | "dashd_chainstate::chain::load"
                | "dashd_chainstate::chain::abc"
                | "dashd_chainstate::chain::failure"
                | "dashd_chainstate::store::blocks"
                | "blockstorage"
                | "dashd_chainstate::connect::connector"
                | "dashd_chainstate::evo::node"
        ) || (m.target() == "dashd_chainstate::connect::serial"
            && *m.level() <= tracing::Level::INFO)
            || m.target() == "dashd_chainstate::connect::disconnect"
            // `ChainlockHandler`'s and `EnforceBlock`'s `LogPrintf`s (BL-546).
            || (m.target() == "dashd_chainstate::chain::locks"
                && *m.level() <= tracing::Level::INFO)
            // V's `HandleError` lines for a write of its own that fails (BL-546).
            || m.target() == "dashd_chainstate::flush::machine"
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{v:?}");
                }
            }
        }
        let mut m = Message(String::new());
        event.record(&mut m);
        let at = (std::thread::current().id(), *event.metadata().level(), m.0);
        CAPTURED.lock().expect("log").push(at);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// `it` collected and sorted.
pub(crate) fn sorted<T: Ord>(it: impl Iterator<Item = T>) -> Vec<T> {
    let mut v: Vec<T> = it.collect();
    v.sort();
    v
}

pub(crate) fn batch(blocks: &[Mined]) -> HeaderBatch {
    HeaderBatch {
        hashes: blocks.iter().map(Mined::hash).collect(),
        headers: blocks.iter().map(Mined::header).collect(),
    }
}

/// A latch that fails the test.
pub(crate) struct Panics;

/// Services for a chain built outside a rig: a clock a day past `p`'s genesis, `p`'s sporks,
/// and [`Panics`].
pub(crate) fn services(p: &ChainParams) -> Services {
    let now = i64::from(genesis_block(p).header().time()) + 86_400;
    Services {
        clock: Arc::new(MockClock::at(now)),
        sporks: Arc::new(StaticSporks::for_network(p, None)),
        fail: Arc::new(Panics),
    }
}

impl FailStop for Panics {
    fn read_failed(&self, what: &str) {
        panic!("the latch tripped: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

/// A signal, as a [`Recorder`] saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ev {
    Checked(BlockHash, BlockState),
    PowValid(BlockId),
    Header(BlockId),
    HeaderTip(BlockId, bool),
}

/// Records every synchronous signal.
#[derive(Default)]
pub(crate) struct Recorder(Mutex<Vec<Ev>>);

impl Recorder {
    pub fn take(&self) -> Vec<Ev> {
        std::mem::take(&mut *self.0.lock().expect("recorder"))
    }

    fn push(&self, e: Ev) {
        self.0.lock().expect("recorder").push(e);
    }
}

impl ValidationInterface for Recorder {
    fn block_checked(&self, block: &Block, state: &BlockState, _ibd: bool) {
        self.push(Ev::Checked(block.hash(), state.clone()));
    }

    fn new_pow_valid_block(&self, nb: &NewBlock<'_>) {
        self.push(Ev::PowValid(nb.id));
    }

    fn accepted_block_header(&self, id: BlockId) {
        self.push(Ev::Header(id));
    }

    fn notify_header_tip(&self, id: BlockId, ibd: bool) {
        self.push(Ev::HeaderTip(id, ibd));
    }
}

/// What a test changes before V starts.
#[derive(Default)]
pub(crate) struct RigConfig {
    /// The chain parameters; default regtest.
    pub params: Option<Arc<ChainParams>>,
    /// The clock's start, in seconds after genesis; default a day (IBD, as the tip is genesis).
    pub after_genesis: Option<i64>,
    /// `-minimumchainwork`.
    pub min_chain_work: Option<Work>,
    /// Turns `-checkblockindex` off, V's and the chain's header subset of it; it is on by default.
    pub no_check_block_index: bool,
    pub files: Option<Box<dyn BlockFiles>>,
    /// `ConnectTip`'s read-ahead (CH4-4); default off.
    pub read_ahead: ReadAheadSetup,
    /// Default [`NullConnector`].
    pub connector: Option<Box<dyn BlockConnector>>,
    /// Default [`NullMempool`].
    pub mempool: Option<Box<dyn MempoolSink>>,
    /// Default [`NoUi`].
    pub ui: Option<Arc<dyn UiNotifications>>,
    /// Default [`NoInstantSend`].
    pub is: Option<Box<dyn IsConflicts>>,
    /// Default [`NoFlush`].
    pub flush: Option<Box<dyn FlushSite>>,
    /// SPORK_19's live state at start (off by default, as on regtest).
    pub chainlocks: bool,
    /// The feeds' queue capacity Q (SB5); default SB4's.
    pub feed_capacity: Option<u32>,
    /// `-stopatheight`.
    pub stop_at_height: i64,
    /// The node's shutdown flag; default a fresh [`LocalShutdown`].
    pub shutdown: Option<Arc<LocalShutdown>>,
    /// The `AbortNode` sink of V and the signals thread; default the rig's [`RecordingAbort`].
    /// The rig follows it with the node's `StartShutdown`, as ND1's `NodeAbort` does.
    pub abort: Option<Arc<dyn AbortSink>>,
    /// Sends commands to V after it starts, before init's ABC.
    pub before_init: Option<BeforeInit>,
    /// V stops with `AbortNode` before init's ABC, whose reply then disconnects, or in it, whose
    /// reply is then `Err` with the state (BL-472).
    pub aborts_before_init: bool,
    /// The readiness guard's scope; default the network's ([`GuardScope::of`]).
    pub guard: Option<GuardScope>,
    /// The header chain's fail-stop; default [`Panics`].
    pub fail: Option<Arc<dyn FailStop>>,
    /// A listener registered with the rig's [`Recorder`], before V starts (DEC-230).
    pub listener: Option<Arc<dyn ValidationInterface>>,
}

/// [`RigConfig::before_init`].
pub(crate) type BeforeInit = Box<dyn FnOnce(&ChainHandle)>;

/// ND1's `NodeAbort` in the rig: the sink, then the node's `StartShutdown()`, as Core's
/// `AbortNode` ends (`shutdown.cpp:36`). If the sink panics, the flag is left to V's stop and
/// the signals thread, which set it after the sink either way (D52).
pub(super) struct AbortThenShutdown {
    pub(super) sink: Arc<dyn AbortSink>,
    pub(super) shutdown: Arc<LocalShutdown>,
}

impl AbortSink for AbortThenShutdown {
    fn abort_node(&self, msg: &str, user: Option<&str>) {
        self.sink.abort_node(msg, user);
        self.shutdown.start_shutdown();
    }

    fn read_failed(&self, what: &str) {
        self.sink.read_failed(what);
    }
}

/// Block files that outlive a V, for [`Rig::restart`].
#[derive(Clone, Default)]
pub(super) struct SharedFiles(Arc<Mutex<MemBlockFiles>>);

impl SharedFiles {
    /// The blocks written.
    pub fn stored(&self) -> usize {
        self.0.lock().expect("files").stored()
    }
}

impl BlockFiles for SharedFiles {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        self.0.lock().expect("files").save_block(block, height, known)
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.0.lock().expect("files").read_block(pos, hash)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.0.lock().expect("files").open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.0.lock().expect("files").current_usage()
    }

    fn scan_already_pruned_files(&mut self, have_pruned: bool) -> Result<(), String> {
        self.0.lock().expect("files").scan_already_pruned_files(have_pruned)
    }

    fn read_ahead(&self) -> Option<Arc<dyn BlockSource>> {
        Some(Arc::new(self.clone()))
    }
}

impl BlockSource for SharedFiles {
    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.0.lock().expect("files").read_block(pos, hash)
    }
}

/// V on a fresh regtest genesis after Core's init (`LoadGenesisBlock`, then the first
/// `ActivateBestChain`, which connects genesis), with `-checkblockindex` on, and the signals
/// thread running.
pub(crate) struct Rig {
    pub p: Arc<ChainParams>,
    pub v: ChainHandle,
    pub join: Option<NodeJoinHandle<VExit>>,
    pub signals: ValidationSignals,
    signals_thread: Option<SignalsThread>,
    pub rec: Arc<Recorder>,
    pub abort: Arc<RecordingAbort>,
    /// The node's shutdown flag (D52).
    pub shutdown: Arc<LocalShutdown>,
    pub genesis: Mined,
    /// SPORK_19's live state; `None` after [`Rig::restart`], whose chain keeps its own.
    pub sporks: Option<Arc<StaticSporks>>,
    /// Init's ABC reply: `Err` with the state when V stopped in it (BL-472).
    pub init: Result<Result<(), String>, WaitError>,
}

impl Rig {
    pub fn new() -> Rig {
        Rig::with(RigConfig::default())
    }

    pub fn with(cfg: RigConfig) -> Rig {
        Rig::try_with(cfg).expect("V starts")
    }

    /// [`Rig::with`], or [`start`]'s error.
    pub fn try_with(mut cfg: RigConfig) -> Result<Rig, StartError> {
        let (p, genesis, clock) = Rig::start(cfg.params.clone(), cfg.after_genesis);
        let sporks = Arc::new(StaticSporks::for_network(&p, Some(cfg.chainlocks)));
        let svc = Services {
            clock: clock.clone(),
            sporks: sporks.clone(),
            fail: cfg.fail.take().unwrap_or_else(|| Arc::new(Panics)),
        };
        let mut ccfg = ChainConfig::node(&p);
        // V's `-checkblockindex` and the chain's header subset of it are one option (BL-525).
        ccfg.consistency_checks = !cfg.no_check_block_index;
        ccfg.min_chain_work = cfg.min_chain_work;
        if let Some(q) = cfg.feed_capacity {
            ccfg.feeds.capacity = q;
        }
        let mut files: Box<dyn BlockFiles> =
            cfg.files.take().unwrap_or_else(|| Box::new(MemBlockFiles::default()));
        let fresh = LoadedIndex::Empty {
            cfg: ccfg,
            svc,
        };
        let (chain, idx) =
            open_block_index(p.clone(), fresh, files.as_mut(), &LocalShutdown::default(), false)
                .expect("genesis");
        let mut rig = Rig::launch(p, genesis, clock, chain, idx, files, cfg)?;
        rig.sporks = Some(sporks);
        Ok(rig)
    }

    /// `params` (default regtest), its genesis, and a clock `after_genesis` seconds past it
    /// (default a day).
    fn start(
        params: Option<Arc<ChainParams>>,
        after_genesis: Option<i64>,
    ) -> (Arc<ChainParams>, Mined, Arc<MockClock>) {
        let p = params.unwrap_or_else(regtest);
        let genesis = Mined::genesis(&p);
        let now = i64::from(genesis.time()) + after_genesis.unwrap_or(86_400);
        (p, genesis, Arc::new(MockClock::at(now)))
    }

    /// V restarted on `exit`'s index after an unclean stop that left the coins at `coins_tip`
    /// (§7.3): the node half of Core's load rules ([`NodeBlockIndex::load`]), `LoadChainTip`,
    /// then init's ABC, reading `files`. `Err` is V's refusal to start.
    ///
    /// It reuses the stopped V's `HeaderChain` rather than SB4's reload from a store, so it keeps
    /// what Core does not persist (the runtime best header, `m_failed_blocks`, the dirty set) and
    /// skips SB4's load-time spread of `FAILED_CHILD` to descendants. Its tests therefore mark
    /// every block they need failed themselves (an `invalidateblock` marks each disconnected
    /// block `FAILED_VALID`).
    pub fn restart(
        exit: VExit,
        files: Box<dyn BlockFiles>,
        coins_tip: BlockId,
    ) -> Result<Rig, Fatal> {
        Rig::restart_with(exit, files, coins_tip, RigConfig::default())
    }

    /// [`Rig::restart`] with `cfg`'s seams (its files are `files`).
    pub fn restart_with(
        exit: VExit,
        files: Box<dyn BlockFiles>,
        coins_tip: BlockId,
        cfg: RigConfig,
    ) -> Result<Rig, Fatal> {
        let idx = NodeBlockIndex::load(&exit.chain);
        Rig::reopen(exit.chain, idx, files, coins_tip, cfg)
    }

    /// [`Rig::restart_with`] on an opened index (`open_block_index`'s).
    pub fn reopen(
        mut chain: HeaderChain,
        mut idx: NodeBlockIndex,
        files: Box<dyn BlockFiles>,
        coins_tip: BlockId,
        cfg: RigConfig,
    ) -> Result<Rig, Fatal> {
        // BL-240: production loads a tip-less chain, so `load_chain_tip` always sets the tip and
        // prunes the candidates. A rig chain that kept the stopped V's tip takes Core's early
        // return (`validation.cpp:5004-5006`) and skips the prune, so the rig runs production's,
        // then checks the tables once init's ABC is done: it runs no CheckBlockIndex when it
        // connects nothing, which hid the skipped prune.
        let kept = chain.private_tip().is_some_and(|(t, _)| t.id == coins_tip);
        load_chain_tip(&mut chain, &mut idx, coins_tip)?;
        if kept {
            let reader = chain.reader();
            LoadPrune::decide(&reader, &idx, coins_tip)?.run(&mut idx, &reader, coins_tip);
        }
        let (p, genesis, clock) = Rig::start(cfg.params.clone(), cfg.after_genesis);
        let checked = kept && !cfg.aborts_before_init;
        let rig = Rig::launch(p, genesis, clock, chain, idx, files, cfg).expect("V starts");
        if checked {
            rig.assert_check_block_index();
        }
        Ok(rig)
    }

    /// Core's `CheckBlockIndex` on V's tables now, as V runs it (none under
    /// [`RigConfig::no_check_block_index`]).
    pub fn assert_check_block_index(&self) {
        let (send, checked) = std::sync::mpsc::channel();
        let check = ChainCommand::TestOnV(Box::new(move |v| {
            send.send(v.check_block_index().map_err(|e| e.0)).expect("the rig waits");
            Ok(())
        }));
        self.v.send(check).expect("V runs");
        assert_eq!(checked.recv().expect("V ran it"), Ok(()));
    }

    /// Starts V on the loaded parts with the signals thread, then runs init's ABC.
    fn launch(
        p: Arc<ChainParams>,
        genesis: Mined,
        clock: Arc<MockClock>,
        chain: HeaderChain,
        idx: NodeBlockIndex,
        files: Box<dyn BlockFiles>,
        cfg: RigConfig,
    ) -> Result<Rig, StartError> {
        let rec = Arc::new(Recorder::default());
        let signals = ValidationSignals::default();
        signals.register(rec.clone());
        if let Some(l) = cfg.listener {
            signals.register(l);
        }
        let abort = Arc::new(RecordingAbort::default());
        let shutdown = cfg.shutdown.unwrap_or_default();
        let sink: Arc<dyn AbortSink> = Arc::new(AbortThenShutdown {
            sink: cfg.abort.unwrap_or_else(|| abort.clone()),
            shutdown: shutdown.clone(),
        });
        let signals_thread = signals.start(sink.clone()).expect("start the signals thread");
        let (v, join) = start(VParts {
            params: p.clone(),
            chain,
            idx,
            checks: Box::new(CoreChecks::new(p.clone()).expect("checks")),
            files,
            read_ahead: cfg.read_ahead,
            connector: cfg.connector.unwrap_or_else(|| Box::new(NullConnector)),
            mempool: cfg.mempool.unwrap_or_else(|| Box::new(NullMempool)),
            flush: cfg.flush.unwrap_or_else(|| Box::new(NoFlush)),
            is: cfg.is.unwrap_or_else(|| Box::new(NoInstantSend)),
            ui: cfg.ui.unwrap_or_else(|| Arc::new(NoUi)),
            abort: sink,
            shutdown: shutdown.clone(),
            signals: signals.clone(),
            clock,
            check_block_index: !cfg.no_check_block_index,
            have_pruned: false,
            stop_at_height: cfg.stop_at_height,
            guard: cfg.guard.unwrap_or_else(|| GuardScope::of(&p)),
        })?;
        if let Some(send) = cfg.before_init {
            send(&v);
        }
        let init = v.activate_best_chain().recv();
        assert_eq!(init != Ok(Ok(())), cfg.aborts_before_init, "init's ABC: {init:?}");
        // Init's signals (genesis's `BlockChecked`) are not the tests'.
        rec.take();
        Ok(Rig {
            p,
            v,
            join: Some(join),
            signals,
            signals_thread: Some(signals_thread),
            rec,
            abort,
            shutdown,
            genesis,
            sporks: None,
            init,
        })
    }

    /// The id of `m` (it must be indexed).
    pub fn id(&self, m: &Mined) -> BlockId {
        self.v.reader().lookup(&m.hash()).expect("indexed")
    }

    pub fn ids<'a>(&self, blocks: impl IntoIterator<Item = &'a Mined>) -> Vec<BlockId> {
        blocks.into_iter().map(|m| self.id(m)).collect()
    }

    /// [`Rig::ids`] of exactly `N` blocks.
    pub fn ids_n<const N: usize>(&self, blocks: &[Mined]) -> [BlockId; N] {
        self.ids(blocks).try_into().expect("N blocks")
    }

    pub fn status(&self, m: &Mined) -> Status {
        self.v.reader().entry(self.id(m)).expect("indexed").status
    }

    /// `ProcessNewBlockHeaders`, which must take the whole batch.
    pub fn headers(&self, blocks: &[Mined]) {
        let r = self.v.process_headers(batch(blocks), None).recv().expect("V replies");
        assert!(r.stop.is_none(), "{:?}", r.stop);
        self.assert_tables();
    }

    /// No `AbortNode` so far.
    pub fn assert_no_abort(&self) {
        let messages = self.abort.messages();
        assert!(messages.is_empty(), "{messages:?}");
    }

    /// [`exact_tables`] on V's live tables.
    pub fn assert_tables(&self) {
        let exact =
            self.v.query(|q| exact_tables(q.chain, q.idx, q.have_pruned)).recv().expect("V serves");
        exact.expect("the tables match the reference");
    }

    /// `ProcessNewBlock(force_processing)`, waiting for Core's return.
    pub fn submit(&self, m: &Mined) -> BlockResult {
        self.process(m, true)
    }

    /// `ProcessNewBlock(force_processing)` of a block whose connect stops V with an `AbortNode`:
    /// V never replies, so the wait ends disconnected. `label` joins the failure message.
    pub fn submit_stopping(&self, m: &Mined, label: &str) {
        let r = self.v.process_block(m.block.clone(), true, None, true).recv();
        assert_eq!(r.map(|_| ()), Err(WaitError::Disconnected), "{label}");
    }

    /// An unrequested block (`force_processing = false`).
    pub fn submit_unrequested(&self, m: &Mined) -> BlockResult {
        self.process(m, false)
    }

    fn process(&self, m: &Mined, force: bool) -> BlockResult {
        let r = self.v.process_block(m.block.clone(), force, None, true).recv().expect("V replies");
        self.assert_tables();
        r
    }

    /// [`Rig::submit`], which must accept the block.
    pub fn accept(&self, m: &Mined) -> BlockResult {
        let r = self.submit(m);
        assert!(r.accepted && r.state.is_valid(), "{r:?}");
        r
    }

    /// `nChainTx`, `nSequenceId` of `ids`, and the candidates worst first.
    pub fn tables(&self, ids: &[BlockId]) -> (Vec<u32>, Vec<i32>, Vec<BlockId>) {
        let ids = ids.to_vec();
        self.idx(move |ix| {
            (
                ids.iter().map(|&i| ix.n_chain_tx(i)).collect(),
                ids.iter().map(|&i| ix.sequence_id(i)).collect(),
                ix.candidates().collect(),
            )
        })
    }

    /// A read of V's tables, on V.
    pub fn idx<T: Send + 'static>(
        &self,
        f: impl FnOnce(&NodeBlockIndex) -> T + Send + 'static,
    ) -> T {
        self.v.query(move |q| f(q.idx)).recv().expect("V serves the query")
    }

    /// Runs `f` on V as a class C operation and waits for it.
    pub fn on_v(&self, f: impl FnOnce(&mut V) -> Result<(), Fatal> + Send + 'static) {
        let (done, wait) = reply();
        let cmd = ChainCommand::TestOnV(Box::new(move |v| {
            let r = f(v);
            done.send(());
            r
        }));
        self.v.send(cmd).expect("V runs");
        wait.recv().expect("V ran it");
    }

    /// Waits until the signals thread has run every queued signal.
    pub fn sync(&self) {
        self.signals.sync(&mut Blocking).expect("the signals thread runs");
    }

    /// V's thread, whose lines [`levels_on`] reads.
    pub fn v_thread(&self) -> std::thread::ThreadId {
        self.join.as_ref().expect("running").thread_id()
    }

    /// Stops V and returns its state.
    pub fn stop(&mut self) -> VExit {
        self.v.shutdown();
        let join = self.join.take().expect("running").join().expect("an unmarked joiner");
        let exit = join.expect("V does not panic");
        if exit.fatal.is_none() {
            exact_tables(&exit.chain, &exit.idx, exit.have_pruned)
                .expect("the tables match the reference");
        }
        exit
    }

    /// [`Self::stop`] for a V that stopped on the failure `msg`, which the sink heard once.
    pub fn stop_on(&mut self, msg: &str) {
        assert_eq!(self.stop().fatal.map(|f| f.0).as_deref(), Some(msg));
        assert_eq!(self.abort.messages(), [msg], "one report");
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(j) = self.join.take() {
            self.v.shutdown();
            let _ = j.join();
        }
        if let Some(t) = self.signals_thread.take() {
            let _ = t.stop();
        }
    }
}

/// `f` over V's context and a tip-path connect context of V's own chain and deployments, as V's
/// `with_connector` builds it, for tests that open an evo attempt directly.
pub(crate) fn with_tip_cx<R>(
    v: &mut V,
    f: impl FnOnce(&mut crate::chain::VContext, &crate::chain::ConnectCx<'_>) -> R,
) -> R {
    let deployments = v.deployments.view();
    let chain = std::cell::RefCell::new(&mut v.chain);
    let best_header = chain.borrow().best_header();
    let chainlocks = crate::chain::seams::HeldChainLocks::new(&chain, -1);
    let cx = crate::chain::ConnectCx {
        params: &v.params,
        reader: &v.reader,
        deployments: &deployments,
        best_header,
        min_chain_work: v.reader.min_chain_work(),
        is_ibd: false,
        chainlocks: &chainlocks,
        guard: GuardScope::Live,
        origin: crate::chain::AttemptOrigin::Tip,
    };
    f(&mut v.vcx, &cx)
}
