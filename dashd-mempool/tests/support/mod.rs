//! What the V-driven tests share: regtest parameters with DIP0003 out of reach, mined blocks over
//! `OP_TRUE` coinbases, and one V over block files on disk and coins in memory.
#![allow(dead_code, reason = "each test crate uses part of the shared support")]

use std::os::unix::fs::DirBuilderExt as _;
use std::sync::{Arc, Mutex};

use dash_chain::{ChainConfig, FailStop, HeaderChain, Services, StaticSporks};
use dash_clock::MockClock;
use dash_consensus::TxRef;
use dash_consensus::header::HeaderParams;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{
    Block, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxType,
};
use dash_consensus::script::opcodes::{OP_RESERVED, OP_TRUE};
use dash_consensus::script::{ScriptNum, push_data};
use dash_consensus::tx::COINBASE_MATURITY;
use dash_dml::testkit::replay::mainnet;
use dash_kv::MemoryEngine;
use dash_num::CompactTarget;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network};
use dash_pkc::bls::BlsGlobalSig;
use dash_primitives::{AssetUnlock, QuorumHash};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::{
    AbortPath, ChainHandle, CoreChecks, FlushSite, GuardScope, LocalShutdown, MempoolSink, NoFlush,
    NoInstantSend, NoUi, NodeBlockIndex, NodeJoinHandle, ReadAheadSetup, RecordingAbort, VExit,
    VParts, ValidationSignals, genesis_block, load_genesis_block, spawn,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use dashd_chainstate::connect::{ConnectorParts, CoreConnector, NoEvo, NodeHelper};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches, ScriptExecCache, SigCache};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles,
};
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};

/// DIP0003 and the later forks out of reach: the blocks here are plain.
pub const NO_DIP3: u32 = 10_000_000;
pub const MATURITY: u32 = COINBASE_MATURITY as u32;
pub const OUTPUT_VALUE: i64 = 50_000;

/// Regtest at the release dash-dml's testkit runs mainnet at: this crate names no release.
pub fn params() -> Arc<ChainSetup> {
    params_with(NO_DIP3)
}

/// [`params`] with DIP0003 at `dip3`.
pub fn params_with(dip3: u32) -> Arc<ChainSetup> {
    let at = |name: &str| format!("{name}@{NO_DIP3}");
    let params = ChainParamsBuilder::new(Network::Regtest, mainnet().release)
        .arg(ChainOption::Dip3Params, format!("{dip3}:{dip3}"))
        .arg(ChainOption::TestActivationHeight, at("v20"))
        .arg(ChainOption::TestActivationHeight, at("mn_rr"))
        .build()
        .expect("regtest's parameters");
    Arc::new(params)
}

pub fn pow_of(p: &ChainSetup) -> PowParams {
    *HeaderParams::from_chain_params(p).expect("regtest's header rules").pow()
}

pub struct NoReads(pub Arc<RecordingAbort>);

impl FailStop for NoReads {
    fn read_failed(&self, what: &str) {
        dashd_chainstate::chain::AbortSink::read_failed(&*self.0, what);
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

pub fn op_true(value: i64) -> TxOut {
    TxOut::from_duffs(value, vec![OP_TRUE].into())
}

/// A version-2 spend of `inputs` (`(outpoint, nSequence)`) paying `value` to each of two
/// `OP_TRUE`s.
pub fn spend(inputs: &[(OutPoint, u32)], lock_time: u32, value: i64) -> TxRef {
    let inputs =
        inputs.iter().map(|&(p, seq)| TxIn::new(p, Vec::new().into(), seq)).collect::<Vec<_>>();
    let tx =
        Transaction::new(2, TxType::new(0), inputs, vec![op_true(value); 2], lock_time, vec![]);
    Arc::new(Tx::new(tx))
}

pub fn child(parent: &Tx, value: i64) -> TxRef {
    spend(&[(OutPoint::new(parent.txid(), 0), u32::MAX)], 0, value)
}

/// A version-1 asset unlock of `index` requested at `requested`: no stable txid, so the pool
/// keeps its expiry (`txmempool.cpp:677-684` at v24.0.0).
pub fn unlock(index: u64, requested: u32) -> TxRef {
    let sig = BlsGlobalSig::decode(&[0; 96]).expect("decodes");
    let p = AssetUnlock::new(1, index, 1_000, requested, QuorumHash::from([1; 32]), sig);
    let mut payload = Vec::new();
    p.encode(&mut payload);
    let tx = Transaction::new(3, TxType::AssetUnlock, vec![], vec![op_true(1_000)], 0, payload);
    Arc::new(Tx::new(tx))
}

// Core: script/script.h:427 CScript::push_int64
pub fn height_push(n: u32) -> Vec<u8> {
    let mut out = Vec::new();
    match n {
        1..=16 => out.push(OP_RESERVED + n as u8),
        _ => push_data(&mut out, &ScriptNum::new(i64::from(n)).encode()),
    }
    out
}

/// A block on `parent` at height `h`: a coinbase (`tag` in its script, so siblings differ), then
/// `txs`.
pub fn block_on(pow: &PowParams, parent: &Block, h: u32, tag: u8, txs: &[TxRef]) -> Arc<Block> {
    let mut script_sig = height_push(h);
    script_sig.push(tag);
    let coinbase = Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        vec![TxIn::new(
            OutPoint::new(TxHash::from([0; 32]), u32::MAX),
            script_sig.into(),
            u32::MAX,
        )],
        vec![op_true(OUTPUT_VALUE)],
        0,
        Vec::new(),
    )));
    let txs: Vec<TxRef> = std::iter::once(coinbase).chain(txs.iter().cloned()).collect();
    let prev = parent.header().raw();
    let (root, _) = compute_merkle_root(txs.iter().map(|tx| *tx.txid().as_bytes()).collect());
    let mut raw = BlockHeader::new(
        0x2000_0000,
        parent.hash(),
        MerkleRoot::from_lendian(root),
        prev.time() + 150,
        prev.bits(),
        0,
    );
    let target = CompactTarget::new(raw.bits());
    let header = loop {
        let header = Header::new(raw);
        if check_proof_of_work(header.hash().as_bytes(), target, pow) {
            break header;
        }
        raw.set_nonce(raw.nonce() + 1);
    };
    Arc::new(Block::new(header, txs))
}

/// Genesis, then coinbase-only blocks to height `MATURITY`: the next block may spend the
/// coinbase at height 1.
pub struct Matured {
    pub pow: PowParams,
    pub chain: Vec<Arc<Block>>,
}

impl Matured {
    pub fn new() -> Matured {
        let p = params();
        let pow = pow_of(&p);
        let mut chain = vec![genesis_block(&p)];
        for h in 1..=MATURITY {
            let block = block_on(&pow, chain.last().expect("genesis"), h, 0, &[]);
            chain.push(block);
        }
        Matured {
            pow,
            chain,
        }
    }

    /// The coinbase output at height `h`.
    pub fn coin_at(&self, h: usize) -> OutPoint {
        OutPoint::new(self.chain[h].txs()[0].txid(), 0)
    }

    /// The block after `parent` (at `h - 1`) holding `txs`.
    pub fn on(&self, parent: &Block, h: u32, tag: u8, txs: &[TxRef]) -> Arc<Block> {
        block_on(&self.pow, parent, h, tag, txs)
    }

    pub fn tip(&self) -> &Arc<Block> {
        self.chain.last().expect("genesis")
    }
}

/// One V over block files on disk, coins in memory and `NoEvo`, started with `mempool`.
pub struct V {
    pub abort: Arc<RecordingAbort>,
    pub clock: Arc<MockClock>,
    pub v: ChainHandle,
    pub join: NodeJoinHandle<VExit>,
    pub dir: tempfile::TempDir,
}

impl V {
    /// V on `p` after its startup ABC, its signals `signals` (whose thread reports to `abort`).
    pub fn start(
        chainlocks: bool,
        p: &Arc<ChainSetup>,
        mempool: Box<dyn MempoolSink>,
        signals: &ValidationSignals,
        abort: &Arc<RecordingAbort>,
    ) -> V {
        V::start_with_flush(chainlocks, p, mempool, Box::new(NoFlush), signals, abort)
    }

    /// As [`V::start`], with `flush` at Core's `FlushStateToDisk` sites.
    pub fn start_with_flush(
        chainlocks: bool,
        p: &Arc<ChainSetup>,
        mempool: Box<dyn MempoolSink>,
        flush: Box<dyn FlushSite>,
        signals: &ValidationSignals,
        abort: &Arc<RecordingAbort>,
    ) -> V {
        let latch = GlobalFailStop::new();
        let dir = tempfile::tempdir().expect("a temporary directory");
        let blocks_dir = dir.path().join("blocks");
        std::fs::DirBuilder::new().mode(0o700).create(&blocks_dir).expect("the blocks directory");
        let config = BlockFilesConfig::new(blocks_dir, p.message_start);
        let files = BlockFiles::with_fs(Arc::new(OsFs::new()), config, BlockFilesState::default());
        let files = Arc::new(GuardedFiles::new(files, latch.clone()));
        let pow = pow_of(p);
        let genesis = genesis_block(p);
        let clock = Arc::new(MockClock::at(i64::from(genesis.header().raw().time())));
        let path = || AbortPath::new(abort.clone(), latch.clone());
        let svc = Services {
            clock: clock.clone(),
            sporks: Arc::new(StaticSporks::for_network(p, Some(chainlocks))),
            fail: Arc::new(NoReads(abort.clone())),
        };
        let mut block_files = NodeBlockFiles::new(files.clone(), pow, Arc::new(path()));
        let checks = ChainConfig {
            consistency_checks: false,
            ..ChainConfig::node(p)
        };
        let (chain, idx): (HeaderChain, NodeBlockIndex) =
            load_genesis_block(p.clone(), checks, svc, &mut block_files).expect("genesis loads");
        let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
        let engine = Arc::new(GuardedEngine::new(engine, latch.clone(), abort.clone()));
        let budget = CoinsBudget::new(1 << 30, 0, Ratio::HALF, 0);
        let cache = CoinsCache::new(engine, budget, Salt::fixed(7), None);
        let sizes = CacheSizes::default();
        let caches = Arc::new(ScriptCaches {
            sig: SigCache::with_nonce(sizes.signature_cache_bytes, [0x5a; 32]).expect("sigcache"),
            exec: ScriptExecCache::with_nonce(sizes.script_execution_cache_bytes, [0xa5; 32])
                .expect("script execution cache"),
        });
        let parts = ConnectorParts {
            coins: Arc::new(Mutex::new(cache)),
            files: files.clone(),
            queue: Arc::new(CheckQueue::serial()),
            caches,
            helper: Box::new(NodeHelper),
            abort: path(),
            assume_valid: None,
            digest: None,
            xcheck: false,
        };
        let connector = CoreConnector::new(p, parts, NoEvo, Arc::new(())).expect("the connector");
        let (v, join) = spawn(VParts {
            params: p.clone(),
            chain,
            idx,
            checks: Box::new(CoreChecks::new(p.clone()).expect("regtest's checks")),
            files: Box::new(NodeBlockFiles::new(files, pow, Arc::new(path()))),
            read_ahead: ReadAheadSetup::off(),
            connector: Box::new(connector),
            facade: None,
            mempool,
            flush,
            is: Box::new(NoInstantSend),
            ui: Arc::new(NoUi),
            abort: abort.clone(),
            shutdown: Arc::new(LocalShutdown::default()),
            signals: signals.clone(),
            clock: clock.clone(),
            check_block_index: false,
            have_pruned: false,
            stop_at_height: 0,
            reindex: false,
            guard: GuardScope::of(p),
        })
        .expect("V starts");
        v.activate_best_chain().recv().expect("V").expect("the startup ABC");
        V {
            abort: abort.clone(),
            clock,
            v,
            join,
            dir,
        }
    }

    /// `block` processed: `false` once V has stopped.
    pub fn submit(&self, block: &Arc<Block>) -> bool {
        self.clock.set_mocktime(i64::from(block.header().raw().time()));
        let Ok(result) = self.v.process_block(block.clone(), true, None, true).recv() else {
            return false;
        };
        assert!(result.accepted, "{} accepted: {result:?}", block.hash());
        true
    }

    /// Each block processed and connected as the tip.
    pub fn connect(&self, blocks: &[Arc<Block>]) {
        for block in blocks {
            assert!(self.submit(block), "V stopped at {}", block.hash());
            let reports = self.abort.messages();
            assert_eq!(
                self.v.view().snap.tip.tip_hash,
                block.hash(),
                "{} is the tip: {reports:?}",
                block.hash()
            );
        }
    }
}
