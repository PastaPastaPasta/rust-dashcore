//! `connect_chain` (CH4-4, DEC-835): `-reindex-chainstate`'s shape through V. A chain is built
//! into block files on disk, its coins are wiped back to genesis, and a second V reconnects it,
//! reading every block from disk, with the read-ahead off or threaded. Each run is one process,
//! measured from outside with `perf stat -e instructions:u`: `-t` on the reconnecting V's thread
//! alone (`-i`, so not the reader it starts), `-t` on V's thread and the threads it starts (the
//! reader), and `-p` on the process.
//!
//! **The node.** As `reorg`'s: [`CoreConnector`] with [`NoEvo`] hooks and [`NodeHelper`],
//! `-par=1`, no cross-check, the coins in a [`CoinsCache`] on a guarded [`MemoryEngine`] that
//! never flushes ([`NoFlush`]), the block and undo files on disk, [`NullMempool`], no listener
//! and `-checkblockindex` off. The reconnect's `-assumevalid` is the chain's tip, as CH4-0's AV-E
//! runs had it (DEC-780), so no script runs in it. The wiring is `reorg`'s, kept apart on purpose:
//! a shared rig would be as long as what it replaces.
//!
//! **The chain.** Regtest, kept pre-DIP3 as in `reorg`. Block h's coinbase pays
//! `CONNECT_CHAIN_TXS` (T) `OP_TRUE` outputs; from h = 101, T transactions each spend one output
//! of the coinbase at h − 100 into two `OP_TRUE` outputs. `CONNECT_CHAIN_BLOCKS` (N) blocks.
//!
//! **A run.** `CONNECT_CHAIN_MODE` is `off` or `threaded`. The process builds the chain (V 1),
//! stops V 1, wipes the coins, starts V 2 at genesis over the same files and index, and runs
//! its startup ABC, which connects blocks 1 to N. It prints `reconnected` with the blocks and
//! the ABC's wall time (noise on a loaded box, DEC-835). With `CONNECT_CHAIN_SYNC=<dir>`, V 2
//! waits before its ABC: the bench writes V 2's thread id to `<dir>/tid` and starts the ABC once
//! `<dir>/go` exists, so `perf stat` attaches first. After the ABC it writes `<dir>/done` and
//! stops V 2 once `<dir>/end` exists, so the counts end with the reconnect. Without
//! `CONNECT_CHAIN_MODE` it runs both modes, each checking it ends at the chain's tip.
//!
//! The bench builds without the crate's test features, as `reorg` does.

#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dash_chain::{ChainConfig, FailStop, HeaderChain, Services, StaticSporks};
use dash_clock::MockClock;
use dash_consensus::header::HeaderParams;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{
    Block, BlockHash, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn,
    TxOut, TxType,
};
use dash_consensus::script::opcodes::{OP_RESERVED, OP_TRUE};
use dash_consensus::script::{ScriptNum, push_data};
use dash_consensus::tx::COINBASE_MATURITY;
use dash_num::CompactTarget;
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{
    AbortPath, CoreChecks, GuardScope, LocalShutdown, NoFlush, NoInstantSend, NoUi, NodeBlockIndex,
    NullMempool, ReadAheadSetup, RecordingAbort, VExit, VParts, ValidationSignals, genesis_block,
    load_chain_tip, load_genesis_block, spawn,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{BlockOverlay, BlockRef, CoinsBudget, CoinsCache, Ratio, Salt};
use dashd_chainstate::connect::{ConnectorParts, CoreConnector, NoEvo, NodeHelper};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches, ScriptExecCache, SigCache};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles,
};
use dashd_store::MemoryEngine;
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};

/// DIP0003, V20 and MN_RR, past any height a run reaches.
const NO_DIP3: u32 = 10_000_000;

const MATURITY: u32 = COINBASE_MATURITY as u32;

/// A coinbase output's value, far below regtest's subsidy.
const OUTPUT_VALUE: i64 = 50_000;

/// Regtest with DIP0003, V20 and MN_RR at [`NO_DIP3`], and `-checkblockindex` off.
fn params() -> Arc<ChainParams> {
    let at = |name: &str| format!("{name}@{NO_DIP3}");
    let mut params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, format!("{NO_DIP3}:{NO_DIP3}"))
        .arg(ChainOption::TestActivationHeight, at("v20"))
        .arg(ChainOption::TestActivationHeight, at("mn_rr"))
        .build()
        .expect("regtest's parameters");
    params.default_consistency_checks = false;
    Arc::new(params)
}

/// V's header-read latch: nothing reads a header back.
struct NoReads;

impl FailStop for NoReads {
    fn read_failed(&self, what: &str) {
        panic!("a header read failed: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

fn op_true(value: i64) -> TxOut {
    TxOut {
        value,
        script_pubkey: vec![OP_TRUE].into(),
    }
}

/// A version-2 transaction of one input, final.
fn tx(prevout: OutPoint, script_sig: Vec<u8>, outputs: Vec<TxOut>) -> Arc<Tx> {
    let input = TxIn {
        prevout,
        script_sig: script_sig.into(),
        sequence: u32::MAX,
    };
    Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![input],
        outputs,
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

// Core: script/script.h:427 CScript::push_int64
/// BIP34's coinbase height push.
fn height_push(n: u32) -> Vec<u8> {
    let mut out = Vec::new();
    match n {
        1..=16 => out.push(OP_RESERVED + n as u8),
        _ => push_data(&mut out, &ScriptNum::new(i64::from(n)).encode()),
    }
    out
}

/// The chain's blocks by height, genesis first.
fn mine(p: &ChainParams, pow: &PowParams, n: u32, t: u32) -> Vec<Arc<Block>> {
    let spacing = u32::try_from(p.consensus.pow_target_spacing).expect("seconds");
    let mut chain = vec![genesis_block(p)];
    for h in 1..=n {
        let parent = &chain[h as usize - 1];
        let null = OutPoint {
            hash: TxHash::from([0; 32]),
            index: u32::MAX,
        };
        // `bad-cb-length` wants 2 to 100 bytes, and heights 1 to 16 push one.
        let mut script_sig = height_push(h);
        script_sig.push(0);
        let outputs = (0..t).map(|_| op_true(OUTPUT_VALUE)).collect();
        let mut txs = vec![tx(null, script_sig, outputs)];
        if h > MATURITY {
            let coinbase = &chain[(h - MATURITY) as usize].txs()[0];
            for i in 0..t {
                let prevout = OutPoint {
                    hash: coinbase.txid(),
                    index: i,
                };
                txs.push(tx(prevout, Vec::new(), vec![op_true(20_000), op_true(20_000)]));
            }
        }
        let prev = parent.header().raw();
        let (root, _) = compute_merkle_root(txs.iter().map(|tx| *tx.txid().as_bytes()).collect());
        let mut raw = BlockHeader {
            version: 0x2000_0000,
            prev_hash: parent.hash(),
            merkle_root: MerkleRoot::from_lendian(root),
            time: prev.time + spacing,
            bits: prev.bits,
            nonce: 0,
        };
        let target = CompactTarget::new(raw.bits);
        let header = loop {
            let header = Header::new(raw);
            if check_proof_of_work(header.hash().as_bytes(), target, pow) {
                break header;
            }
            raw.nonce += 1;
        };
        chain.push(Arc::new(Block::new(header, txs)));
    }
    chain
}

/// The stores both Vs share: the block and undo files.
struct Stores {
    p: Arc<ChainParams>,
    pow: PowParams,
    files: Arc<GuardedFiles<OsFs>>,
    latch: GlobalFailStop,
    abort: Arc<RecordingAbort>,
    clock: Arc<MockClock>,
    _dir: tempfile::TempDir,
}

impl Stores {
    fn new() -> Stores {
        let p = params();
        let dir = tempfile::tempdir_in(std::env::var_os("TMPDIR").unwrap_or_else(|| ".".into()))
            .expect("a temporary directory");
        let latch = GlobalFailStop::new();
        let blocks_dir = dir.path().join("blocks");
        std::fs::DirBuilder::new().mode(0o700).create(&blocks_dir).expect("the blocks directory");
        let config = BlockFilesConfig::new(blocks_dir, p.message_start);
        let files = BlockFiles::with_fs(Arc::new(OsFs::new()), config, BlockFilesState::default());
        let files = Arc::new(GuardedFiles::new(files, latch.clone()));
        let pow = *HeaderParams::from_chain_params(&p).expect("regtest's header rules").pow();
        let genesis = genesis_block(&p);
        Stores {
            pow,
            files,
            latch,
            abort: Arc::new(RecordingAbort::default()),
            clock: Arc::new(MockClock::at(i64::from(genesis.header().raw().time))),
            p,
            _dir: dir,
        }
    }

    fn block_files(&self) -> NodeBlockFiles<OsFs> {
        let path = AbortPath::new(self.abort.clone(), self.latch.clone());
        NodeBlockFiles::new(self.files.clone(), self.pow, Arc::new(path))
    }

    /// V over `chain` and `idx` doing `job`, then stopped: its exit, and the wall time from its
    /// startup ABC to the job's end.
    fn run_v(&self, chain: HeaderChain, idx: NodeBlockIndex, job: Job<'_>) -> (VExit, Duration) {
        let p = &self.p;
        let (coins, read_ahead, av, feed) = match job {
            Job::Build(feed) => (None, ReadAheadSetup::off(), None, feed),
            Job::Reconnect {
                at,
                read_ahead,
                tip,
            } => (Some(at), read_ahead, Some(tip), &[][..]),
        };
        let path = AbortPath::new(self.abort.clone(), self.latch.clone());
        let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
        let engine = Arc::new(GuardedEngine::new(engine, self.latch.clone(), self.abort.clone()));
        let budget = CoinsBudget::new(1 << 30, 0, Ratio::HALF, 0);
        let mut cache = CoinsCache::new(engine, budget, Salt::fixed(7), None);
        if let Some(at) = coins {
            BlockOverlay::new(&mut cache).commit(at).expect("the wiped coins at genesis");
        }
        let sizes = CacheSizes::default();
        let caches = Arc::new(ScriptCaches {
            sig: SigCache::with_nonce(sizes.signature_cache_bytes, [0x5a; 32]).expect("sigcache"),
            exec: ScriptExecCache::with_nonce(sizes.script_execution_cache_bytes, [0xa5; 32])
                .expect("script execution cache"),
        });
        let parts = ConnectorParts {
            coins: Arc::new(Mutex::new(cache)),
            files: self.files.clone(),
            queue: Arc::new(CheckQueue::serial()),
            caches,
            helper: Box::new(NodeHelper),
            abort: path,
            assume_valid: av,
            digest: None,
            xcheck: false,
        };
        let connector = CoreConnector::new(p, parts, NoEvo, Arc::new(())).expect("the connector");
        let signals = ValidationSignals::default();
        let signals_thread = signals.start(self.abort.clone()).expect("the signals thread");
        let (v, join) = spawn(VParts {
            params: p.clone(),
            chain,
            idx,
            checks: Box::new(CoreChecks::new(p.clone()).expect("regtest's checks")),
            files: Box::new(self.block_files()),
            read_ahead,
            connector: Box::new(connector),
            facade: None,
            mempool: Box::new(NullMempool),
            flush: Box::new(NoFlush),
            is: Box::new(NoInstantSend),
            ui: Arc::new(NoUi),
            abort: self.abort.clone(),
            shutdown: Arc::new(LocalShutdown::default()),
            signals: signals.clone(),
            clock: self.clock.clone(),
            check_block_index: p.default_consistency_checks,
            have_pruned: false,
            stop_at_height: 0,
            guard: GuardScope::of(p),
        })
        .expect("V starts");
        let sync = coins.and_then(|_| sync_point());
        let start = Instant::now();
        v.activate_best_chain().recv().expect("V").expect("the startup ABC");
        for block in feed {
            self.clock.set_mocktime(i64::from(block.header().raw().time));
            let r = v.process_block(block.clone(), true, None, true).recv().expect("V replies");
            assert!(r.accepted && r.state.is_valid(), "{}: {:?}", block.hash(), r.state);
        }
        let wall = start.elapsed();
        if let Some(dir) = sync {
            sync_end(&dir);
        }
        v.shutdown();
        let exit = join.join().expect("an unmarked joiner").expect("V does not panic");
        let _ = signals_thread.stop();
        assert!(exit.fatal.is_none(), "AbortNode: {:?}", exit.fatal);
        assert!(self.abort.messages().is_empty(), "AbortNode: {:?}", self.abort.messages());
        assert!(self.latch.tripped().is_none(), "the stores' latch: {:?}", self.latch.tripped());
        (exit, wall)
    }
}

/// What a V does.
enum Job<'a> {
    /// From genesis, with every script run: these blocks processed after the startup ABC.
    Build(&'a [Arc<Block>]),
    /// The startup ABC on coins wiped back to `at`, with `read_ahead`, `-assumevalid` `tip`,
    /// after [`sync_point`].
    Reconnect {
        at: BlockRef,
        read_ahead: ReadAheadSetup,
        tip: BlockHash,
    },
}

/// With `CONNECT_CHAIN_SYNC=<dir>`: writes V's thread id to `<dir>/tid`, then waits for
/// `<dir>/go`, and returns the directory for [`sync_end`]. V 1 has exited, so V 2 is the one
/// `dashd-v` thread once it has named itself.
fn sync_point() -> Option<PathBuf> {
    let dir = std::env::var_os("CONNECT_CHAIN_SYNC").map(PathBuf::from)?;
    let named_v = || {
        let tasks = std::fs::read_dir("/proc/self/task").expect("the process's threads");
        let is_v =
            |t: &PathBuf| std::fs::read_to_string(t.join("comm")).is_ok_and(|c| c == "dashd-v\n");
        let vs: Vec<PathBuf> = tasks.map(|t| t.expect("a thread").path()).filter(is_v).collect();
        assert!(vs.len() <= 1, "one V thread: {vs:?}");
        vs.first().map(|t| t.file_name().expect("a thread id").to_string_lossy().into_owned())
    };
    let tid = loop {
        match named_v() {
            Some(tid) => break tid,
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    };
    std::fs::write(dir.join("tid.part"), tid).expect("the thread id");
    std::fs::rename(dir.join("tid.part"), dir.join("tid")).expect("the thread id");
    wait_for(&dir.join("go"));
    Some(dir)
}

/// After the reconnect: writes `<dir>/done`, then waits for `<dir>/end`, so `perf stat` stops
/// counting before V stops.
fn sync_end(dir: &Path) {
    std::fs::write(dir.join("done"), "").expect("the reconnect's end");
    wait_for(&dir.join("end"));
}

fn wait_for(file: &Path) {
    while !file.exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Builds `n` blocks of `t` spends, then reconnects them with the read-ahead `mode`.
fn run(n: u32, t: u32, mode: &str) {
    let read_ahead = match mode {
        "off" => ReadAheadSetup::off(),
        "threaded" => ReadAheadSetup::threaded(false),
        _ => panic!("CONNECT_CHAIN_MODE={mode}: off or threaded"),
    };
    let s = Stores::new();
    let p = s.p.clone();
    let blocks = mine(&p, &s.pow, n, t);
    let svc = Services {
        clock: s.clock.clone(),
        sporks: Arc::new(StaticSporks::for_network(&p, Some(false))),
        fail: Arc::new(NoReads),
    };
    let mut files = s.block_files();
    let (chain, idx) = load_genesis_block(p.clone(), ChainConfig::node(&p), svc, &mut files)
        .expect("genesis loads");
    let tip = blocks.last().expect("genesis").hash();
    let (built, _) = s.run_v(chain, idx, Job::Build(&blocks[1..]));

    let mut chain = built.chain;
    let genesis = chain.reader().lookup(&blocks[0].hash()).expect("genesis is indexed");
    let at = BlockRef {
        id: genesis,
        hash: blocks[0].hash(),
        height: 0,
    };
    let mut idx = NodeBlockIndex::load(&chain);
    load_chain_tip(&mut chain, &mut idx, genesis).expect("the tip at genesis");
    let job = Job::Reconnect {
        at,
        read_ahead,
        tip,
    };
    let (exit, wall) = s.run_v(chain, idx, job);
    let (end, _) = exit.chain.private_tip().expect("a tip");
    let end = exit.chain.reader().entry(end.id).expect("the tip's entry").hash;
    assert_eq!(end, tip, "{mode}: the reconnect's tip");
    println!("reconnected mode={mode} blocks={n} txs={t} wall_ms={}", wall.as_millis());
}

fn main() {
    let var = |name: &str| std::env::var(name).ok();
    let num = |name: &str, default: u32| var(name).map_or(default, |v| v.parse().expect(name));
    let (n, t) = (num("CONNECT_CHAIN_BLOCKS", 300), num("CONNECT_CHAIN_TXS", 4));
    match var("CONNECT_CHAIN_MODE") {
        Some(mode) => run(n, t, &mode),
        None => ["off", "threaded"].into_iter().for_each(|mode| run(n, t, mode)),
    }
}
