//! `reorg` (CH3 §6.3's `reorg-depth-{1,6,30}`; CH3b b2c): a reorg of depth d through V, on a
//! synthetic regtest chain, with the real connector. Rows are `reorg/depth-{1,6,30}`; PF0's
//! counter gate gates every region counter except page faults (`workloads.toml`).
//!
//! **The node.** V as `dashd-node` wires it (`dashd-node/src/chain.rs`'s `parts`, the connector
//! in `chainstate.rs`): [`CoreConnector`] with [`NoEvo`] hooks (the node's until CH7a; the chain
//! is pre-DIP3 and needs none) and the production [`NodeHelper`], `-par=1`, no `-assumevalid`
//! (regtest's default), no cross-check, the coins in a [`CoinsCache`], the block and undo files
//! on disk under one [`GuardedFiles`], and [`NullMempool`], so `DisconnectTip` keeps no pool.
//! Unlike the node, the cache sits on a guarded [`MemoryEngine`] that never flushes
//! ([`NoFlush`]), the cache's salt and the script caches' nonces are fixed, so every run probes
//! the same buckets, and no listener is registered. `-checkblockindex` is off, mainnet's default
//! (the node runs regtest's, on): the bench clears regtest's `default_consistency_checks`, which
//! V and the header chain both read as `dashd-node` wires them; on, its walk of the whole index
//! after every step and every header would grow with the chain.
//!
//! **The chain.** Regtest from its genesis, with DIP0003 (and V20 and MN_RR, which Core keeps at
//! or above it) moved past any height a run reaches (`-dip3params`, `-testactivationheight`), so
//! the chain stays pre-DIP3 and needs no evo validation. The bits stay at regtest's limit at
//! every height (no retargeting). Block h on branch s holds a coinbase (BIP34's height and s in
//! its `scriptSig`, one `OP_TRUE` output); from h = 101, a transaction spending the coinbase at
//! h − 100, just mature, into two `OP_TRUE` outputs; and from h = 102, one spending its parent's
//! second output. Both spends carry s as their lock time, so a branch's transactions differ from
//! the ones they replace. A branch's first block therefore spends exactly the two coins the block
//! it replaces spent, which that block's disconnect restored. Every script is `OP_TRUE`, so no
//! step does script work (`connect_block_micro` gates that per input). The base chain is 140
//! blocks, so the deepest fork, at 110, is above the first spend.
//!
//! **What a row measures.** Each iteration's setup mines a new branch of d + 1 blocks from the
//! active chain's block at depth d, and hands V its headers (`ProcessNewBlockHeaders`, as a
//! headers-first peer would) and its first d blocks, each `ProcessNewBlock` with the request flag
//! set (a requested block is stored at less work); the d-th ties the tip, so the first-seen tip
//! stays. A setup connects nothing, so it stages no signal; the base chain's are synced before
//! the first, so every region starts with an empty queue. The region is the last block's
//! `ProcessNewBlock`: `CheckBlock`, `AcceptBlock` (the block's write) and the ABC it runs: d
//! `DisconnectTip`s (each block and its undo record read from disk, `DisconnectBlock`, the
//! overlay's commit) and d + 1 `ConnectTip`s (the first d read from disk, then `ConnectBlock`
//! with its undo write and status writes, and the overlay's commit), then a
//! `SyncWithValidationInterfaceQueue`, so the signals thread has run every signal V staged before
//! the region ends. A row's instructions per block are its instructions per iteration over
//! 2d + 1. Most of them are X11: each of the region's 2d block reads hashes the header again to
//! check its proof of work, as Core's `ReadBlockFromDisk` does (about 400 k instructions a read);
//! the rest is V's loop and the connector. The setup checks that V's tip is the previous
//! iteration's branch tip, reached in 2d + 1 steps, and that nothing has failed (PF0's shim exits
//! before `Drop` runs); the chain grows by one block an iteration, and each iteration adds d + 1
//! blocks to the index and the files.
//!
//! The bench builds without the crate's test features, as `coins_flush` does.

// PROCESS.md: benches measure production only, so a test feature turned on here (by a self
// dev-dependency or `--all-features`) fails the build. Clippy's `--all-features` run is exempt.
#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

use std::os::unix::fs::DirBuilderExt as _;
use std::sync::{Arc, Mutex};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_chain::{ChainConfig, FailStop, HeaderBatch, Services, StaticSporks};
use dash_clock::MockClock;
use dash_consensus::header::HeaderParams;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{
    Block, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxType,
};
use dash_consensus::script::opcodes::{OP_RESERVED, OP_TRUE};
use dash_consensus::script::{ScriptNum, push_data};
use dash_consensus::tx::COINBASE_MATURITY;
use dash_num::CompactTarget;
use dash_params::{ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{
    AbortPath, Blocking, ChainHandle, CoreChecks, GuardScope, LocalShutdown, NoFlush,
    NoInstantSend, NoUi, NodeJoinHandle, NullMempool, ReadAheadSetup, RecordingAbort,
    SignalsThread, VExit, VParts, ValidationSignals, genesis_block, load_genesis_block, spawn,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use dashd_chainstate::connect::{ConnectorParts, CoreConnector, NoEvo, NodeHelper};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches, ScriptExecCache, SigCache};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles,
};
use dashd_store::MemoryEngine;
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};
use tempfile::TempDir;

/// CH3 §6.3's depths, the deepest last.
const DEPTHS: [u32; 3] = [1, 6, 30];

/// The base chain's height.
const BASE: u32 = 140;

/// `COINBASE_MATURITY` as a height.
const MATURITY: u32 = COINBASE_MATURITY as u32;

// The deepest fork is at or above the first block that spends, so every branch block spends two
// coins.
const _: () = assert!(BASE - DEPTHS[2] > MATURITY);

/// Where DIP0003, V20 and MN_RR move: past any height a run reaches.
const NO_DIP3: u32 = 1_000_000;

const MIB: u64 = 1 << 20;

/// A coinbase's value, far below regtest's subsidy.
const COINBASE_VALUE: i64 = 50_000;

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

/// V's header-read latch: nothing in the bench reads a header back.
struct NoReads;

impl FailStop for NoReads {
    fn read_failed(&self, what: &str) {
        panic!("a header read failed: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

/// A version-2 transaction of one input, final (every sequence `SEQUENCE_FINAL`).
fn tx(prevout: OutPoint, script_sig: Vec<u8>, outputs: Vec<TxOut>, lock_time: u32) -> Arc<Tx> {
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
        lock_time,
        extra_payload: Vec::new(),
    }))
}

fn op_true(value: i64) -> TxOut {
    TxOut {
        value,
        script_pubkey: vec![OP_TRUE].into(),
    }
}

fn outpoint(tx: &Tx, index: u32) -> OutPoint {
    OutPoint {
        hash: tx.txid(),
        index,
    }
}

// Core: script/script.h:427 CScript::push_int64
/// `CScript() << n`, BIP34's coinbase height: `OP_1`…`OP_16`, else the minimal `CScriptNum` push.
fn height_push(n: u32) -> Vec<u8> {
    let mut out = Vec::new();
    match n {
        1..=16 => out.push(OP_RESERVED + n as u8),
        _ => push_data(&mut out, &ScriptNum::new(i64::from(n)).encode()),
    }
    out
}

/// The node, its active chain as the bench built it, and the next branch's number.
struct Reorg {
    pow: PowParams,
    /// Regtest's block spacing, the step between mined blocks' times.
    spacing: u32,
    depth: u32,
    v: ChainHandle,
    join: Option<NodeJoinHandle<VExit>>,
    signals: ValidationSignals,
    signals_thread: Option<SignalsThread>,
    clock: Arc<MockClock>,
    abort: Arc<RecordingAbort>,
    /// The stores' latch.
    latch: GlobalFailStop,
    /// V's active chain, by height.
    active: Vec<Arc<Block>>,
    /// The branch the next fork mines (the base chain is branch 0).
    branch: u32,
    /// V's `tip_seq` after the last setup (`None` before the first).
    seq: Option<u64>,
    _dir: TempDir,
}

impl Reorg {
    /// V on regtest's genesis over new stores, with the base chain connected.
    fn new(depth: u32) -> Reorg {
        let p = &params();
        let dir = tempfile::tempdir().expect("a temporary directory");
        let latch = GlobalFailStop::new();
        let abort = Arc::new(RecordingAbort::default());
        let path = AbortPath::new(abort.clone(), latch.clone());
        let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
        let engine = Arc::new(GuardedEngine::new(engine, latch.clone(), abort.clone()));
        let budget = CoinsBudget::new(64 * MIB, 0, Ratio::HALF, 0);
        let coins = CoinsCache::new(engine, budget, Salt::fixed(7), None);
        let coins = Arc::new(Mutex::new(coins));

        let blocks_dir = dir.path().join("blocks");
        std::fs::DirBuilder::new().mode(0o700).create(&blocks_dir).expect("the blocks directory");
        let config = BlockFilesConfig::new(blocks_dir, p.message_start);
        let files = BlockFiles::with_fs(Arc::new(OsFs::new()), config, BlockFilesState::default());
        let files = Arc::new(GuardedFiles::new(files, latch.clone()));
        let pow = *HeaderParams::from_chain_params(p).expect("regtest's header rules").pow();
        let mut block_files = NodeBlockFiles::new(files.clone(), pow, Arc::new(path.clone()));

        let genesis = genesis_block(p);
        let clock = Arc::new(MockClock::at(i64::from(genesis.header().raw().time)));
        let svc = Services {
            clock: clock.clone(),
            sporks: Arc::new(StaticSporks::for_network(p, Some(false))),
            fail: Arc::new(NoReads),
        };
        let (chain, idx) =
            load_genesis_block(p.clone(), ChainConfig::node(p), svc, &mut block_files)
                .expect("genesis loads");

        // Core's default cache sizes, salted with fixed nonces (as connect_block_micro's).
        let sizes = CacheSizes::default();
        let caches = Arc::new(ScriptCaches {
            sig: SigCache::with_nonce(sizes.signature_cache_bytes, [0x5a; 32]).expect("sigcache"),
            exec: ScriptExecCache::with_nonce(sizes.script_execution_cache_bytes, [0xa5; 32])
                .expect("script execution cache"),
        });
        let parts = ConnectorParts {
            coins,
            files,
            queue: Arc::new(CheckQueue::serial()),
            caches,
            helper: Box::new(NodeHelper),
            abort: path,
            assume_valid: None,
            digest: None,
            xcheck: false,
        };
        let connector = CoreConnector::new(p, parts, NoEvo, Arc::new(())).expect("the connector");

        let signals = ValidationSignals::default();
        let signals_thread = signals.start(abort.clone()).expect("the signals thread");
        let (v, join) = spawn(VParts {
            params: p.clone(),
            chain,
            idx,
            checks: Box::new(CoreChecks::new(p.clone()).expect("regtest's checks")),
            files: Box::new(block_files),
            // PF0's rows read every block on V (Q5).
            read_ahead: ReadAheadSetup::off(),
            connector: Box::new(connector),
            mempool: Box::new(NullMempool),
            flush: Box::new(NoFlush),
            is: Box::new(NoInstantSend),
            ui: Arc::new(NoUi),
            abort: abort.clone(),
            shutdown: Arc::new(LocalShutdown::default()),
            signals: signals.clone(),
            clock: clock.clone(),
            check_block_index: p.default_consistency_checks,
            have_pruned: false,
            stop_at_height: 0,
            guard: GuardScope::of(p),
        })
        .expect("V starts");
        // Init's ABC connects genesis.
        v.activate_best_chain().recv().expect("init's ABC");

        let mut reorg = Reorg {
            pow,
            spacing: u32::try_from(p.consensus.pow_target_spacing).expect("seconds"),
            depth,
            v,
            join: Some(join),
            signals,
            signals_thread: Some(signals_thread),
            clock,
            abort,
            latch,
            active: vec![genesis],
            branch: 1,
            seq: None,
            _dir: dir,
        };
        for h in 1..=BASE {
            let block = reorg.mine(h, 0);
            reorg.clock.set_mocktime(i64::from(block.header().raw().time));
            reorg.process(&block);
            reorg.active.push(block);
        }
        reorg.signals.sync(&mut Blocking).expect("the signals thread runs");
        reorg
    }

    /// Block `h` on branch `branch`, on the active chain's block `h - 1` (see the module docs).
    fn mine(&self, h: u32, branch: u32) -> Arc<Block> {
        let parent = &self.active[h as usize - 1];
        let mut script_sig = height_push(h);
        push_data(&mut script_sig, &branch.to_le_bytes());
        let null = OutPoint {
            hash: TxHash::from([0; 32]),
            index: u32::MAX,
        };
        let mut txs = vec![tx(null, script_sig, vec![op_true(COINBASE_VALUE)], 0)];
        if h > MATURITY {
            let coinbase = &self.active[(h - MATURITY) as usize].txs()[0];
            let outputs = vec![op_true(20_000), op_true(20_000)];
            txs.push(tx(outpoint(coinbase, 0), Vec::new(), outputs, branch));
        }
        if let Some(spend) = parent.txs().get(1) {
            txs.push(tx(outpoint(spend, 1), Vec::new(), vec![op_true(10_000)], branch));
        }
        let prev = parent.header().raw();
        let (root, _) = compute_merkle_root(txs.iter().map(|tx| *tx.txid().as_bytes()).collect());
        let mut raw = BlockHeader {
            version: 0x2000_0000,
            prev_hash: parent.hash(),
            merkle_root: MerkleRoot::from_lendian(root),
            time: prev.time + self.spacing,
            bits: prev.bits,
            nonce: 0,
        };
        // One X11 a nonce: the setup's cost is mostly mining.
        let target = CompactTarget::new(raw.bits);
        let header = loop {
            let header = Header::new(raw);
            if check_proof_of_work(header.hash().as_bytes(), target, &self.pow) {
                break header;
            }
            raw.nonce += 1;
        };
        Arc::new(Block::new(header, txs))
    }

    /// `ProcessNewBlock` for a requested block, accepted.
    fn process(&self, block: &Arc<Block>) {
        let r = self.v.process_block(block.clone(), true, None, true).recv().expect("V replies");
        assert!(r.accepted && r.state.is_valid(), "{}: {:?}", block.hash(), r.state);
    }

    /// V's tip is the bench's, the last region's reorg took 2d + 1 steps (d disconnects and
    /// d + 1 connects, a `tip_seq` each), and nothing has failed.
    fn check_tip(&self) {
        let snap = &self.v.view().snap;
        let want = self.active.last().expect("genesis");
        assert_eq!(
            (snap.tip.tip_hash, snap.tip.tip_height as usize),
            (want.hash(), self.active.len() - 1),
            "V's tip"
        );
        if let Some(seq) = self.seq {
            assert_eq!(snap.tip_seq, seq + 2 * u64::from(self.depth) + 1, "the reorg's steps");
        }
        assert!(self.abort.messages().is_empty(), "AbortNode: {:?}", self.abort.messages());
        assert!(self.latch.tripped().is_none(), "the stores' latch: {:?}", self.latch.tripped());
    }

    /// The setup: mines the next branch and hands V its headers and its first `depth` blocks.
    /// Returns the branch's last block, which the region hands over.
    fn fork(&mut self) -> Arc<Block> {
        self.check_tip();
        let tip = self.active.len() as u32 - 1;
        let old_tip = self.active[tip as usize].hash();
        let fork = tip - self.depth;
        self.active.truncate(fork as usize + 1);
        for h in fork + 1..=tip + 1 {
            let block = self.mine(h, self.branch);
            self.active.push(block);
        }
        self.branch += 1;
        let branch = &self.active[fork as usize + 1..];
        let (last, first) = branch.split_last().expect("d + 1 blocks");
        self.clock.set_mocktime(i64::from(last.header().raw().time));
        let batch = HeaderBatch {
            headers: branch.iter().map(|b| *b.header()).collect(),
            hashes: branch.iter().map(|b| b.hash()).collect(),
        };
        let headers = self.v.process_headers(batch, None).recv().expect("V replies");
        assert!(headers.stop.is_none(), "{:?}", headers.stop);
        for block in first {
            self.process(block);
        }
        // The d-th block ties the tip, which stays.
        let snap = &self.v.view().snap;
        assert_eq!(snap.tip.tip_hash, old_tip);
        self.seq = Some(snap.tip_seq);
        last.clone()
    }
}

/// The region: the branch's last block, its ABC (the reorg), then the signals' sync.
fn region(v: &ChainHandle, signals: &ValidationSignals, block: Arc<Block>) {
    let r = v.process_block(block, true, None, true).recv().expect("V replies");
    assert!(r.accepted && r.state.is_valid(), "{:?}", r.state);
    signals.sync(&mut Blocking).expect("the signals thread runs");
}

impl Drop for Reorg {
    fn drop(&mut self) {
        self.v.shutdown();
        let exit = self.join.take().expect("joined once").join();
        let _ = self.signals_thread.take().expect("stopped once").stop();
        if std::thread::panicking() {
            return;
        }
        let exit = exit.expect("V's thread").expect("V does not panic");
        assert!(exit.fatal.is_none(), "AbortNode: {:?}", exit.fatal);
        assert!(self.abort.messages().is_empty(), "AbortNode: {:?}", self.abort.messages());
        assert!(self.latch.tripped().is_none(), "the stores' latch: {:?}", self.latch.tripped());
    }
}

fn reorg(c: &mut Criterion) {
    for depth in DEPTHS {
        // Built on the row's first call (only a selected row's), and dropped after it.
        let mut fixture = None;
        c.bench_function(&format!("reorg/depth-{depth}"), |b| {
            let fx = fixture.get_or_insert_with(|| Reorg::new(depth));
            // The setup borrows the fixture mutably, so the region takes its own handles.
            let (v, signals) = (fx.v.clone(), fx.signals.clone());
            b.iter_batched(
                || fx.fork(),
                |block| region(&v, &signals, block),
                BatchSize::PerIteration,
            );
            fx.check_tip();
        });
    }
}

criterion_group!(benches, reorg);
criterion_main!(benches);
