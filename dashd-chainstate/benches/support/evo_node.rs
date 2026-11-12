//! The node `connect_evo` and `evo_deep_window` run (CH7a-5a): V connecting blocks past DIP0003
//! through the node's connector with `NodeEvo` and EV1's checks, as CH7a-4b's recorded suites wire
//! it (`chain/tests/evo_chain.rs`), without their test seams.
//!
//! [`CoreConnector`] with [`NodeEvo`], [`DmlChecks`] and the members facade, which V shares, with
//! the layer's mined rows and the mineable pool, the coins in a [`CoinsCache`] and the evo store on one guarded [`MemoryEngine`], the block and undo
//! files on disk, `-par=1`, no cross-check, [`NullMempool`], no listener, an empty masternode meta
//! store and `-checkblockindex` off. No `-assumevalid`: every script runs. V flushes never
//! ([`NoFlush`]), or through the production [`NodeFlush`] with W on its own thread, the evo layer
//! its window, as dashd-node's `Chainstate::attach_parts` wires it.

// Each bench uses part of the rig.
#![allow(dead_code)]

use std::os::unix::fs::DirBuilderExt as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use dash_chain::{BlockRef, ChainConfig, FailStop, HeaderBatch, Services, StaticSporks};
use dash_clock::MockClock;
use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::{Block, decode_block};
use dash_evo::types::ProTxHash;
use dash_params::ChainSetup;
use dashd_chainstate::chain::{
    AbortPath, ChainHandle, CoreChecks, Fatal, GuardScope, LocalShutdown, NoFlush, NoInstantSend,
    NoUi, NodeJoinHandle, NullMempool, ReadAheadSetup, RecordingAbort, SignalsThread, VExit,
    VParts, ValidationSignals, genesis_block, load_genesis_block, spawn,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use dashd_chainstate::connect::{ConnectorParts, CoreConnector, NodeHelper};
use dashd_chainstate::evo::RocksEvoStore;
use dashd_chainstate::evo::cl::AttemptClVerifier;
use dashd_chainstate::evo::effects::{
    EffectAppliers, FacadeMineable, MnListSignal, PlatformBanApplier, StoreSnapshots,
    UnreachableIsLock,
};
use dashd_chainstate::evo::ev1::DmlChecks;
use dashd_chainstate::evo::node::{
    CommittedMinedViews, FileBlocks, NodeEvo, NodeEvoParts, SharedEvoLayer, node_codec,
    node_net_info,
};
use dashd_chainstate::flush::{
    CrashRoll, EffectLedger, FlushConfig, FlushObserver, FlushPolicy, NoInputs, NoPrune, NodeFlush,
    NodeFlushParts, NullObserver, ObsMark, SubscriberHub, ThreadDriver, WThread,
};
use dashd_chainstate::llmq::facade::MembersFacade;
use dashd_chainstate::llmq::rows::RuntimeRows;
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles,
};
use dashd_chainstate::tuning::MemoryTuning;
use dashd_store::MemoryEngine;
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};

/// `text`'s blocks, one hex per line, skipping its `#` header lines.
pub fn decode(text: &str) -> Vec<Arc<Block>> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let bytes: Vec<u8> = (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).expect("hex"))
                .collect();
            let mut input = bytes.as_slice();
            let block = decode_block(&mut input).expect("a block");
            assert!(input.is_empty(), "one block per line");
            Arc::new(block.into_value())
        })
        .collect()
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

/// An empty masternode meta store: Core resets the platform ban only of a masternode with an
/// entry (`meta.cpp:187-195`), so each ProUpServTx's reset changes nothing.
struct NoMetaEntries;

impl PlatformBanApplier for NoMetaEntries {
    fn reset(&mut self, _: &ProTxHash, _: i32) -> Result<(), Fatal> {
        Ok(())
    }
}

/// How V flushes.
#[derive(Clone, Copy)]
pub enum Flush {
    /// Never.
    Never,
    /// Through the production flush, the coins' share `-dbcache`'s, in bytes, and THRESHOLD at
    /// `f` of it (the node's is [`Ratio::HALF`]).
    Node {
        dbcache: u64,
        f: Ratio,
    },
}

/// A running V over `params` and its stores; stopped and checked when dropped, outside any
/// measured region.
pub struct Node {
    pub v: ChainHandle,
    /// The active coins, the connector's (and the flush's).
    coins: Arc<Mutex<CoinsCache<MemoryEngine>>>,
    join: Option<NodeJoinHandle<VExit>>,
    signals: Option<SignalsThread>,
    abort: Arc<RecordingAbort>,
    latch: GlobalFailStop,
    /// W's observer, with [`Flush::Node`].
    w: Arc<WMarks>,
    layer: SharedEvoLayer,
    _dir: tempfile::TempDir,
}

/// W's observer: counts the full flushes W began and, while held, stops W at the next one's
/// first step ([`ObsMark::FlushBegin`]) until released. The hold is `evo-deep-window`'s
/// self-test (`EVO_DEEP_WINDOW_HOLD_W`): a freeze W has not released.
#[derive(Default)]
struct WMarks {
    begins: AtomicU64,
    held: Mutex<bool>,
    release: Condvar,
}

impl FlushObserver for WMarks {
    fn mark(&self, m: ObsMark) {
        if let ObsMark::FlushBegin {
            ..
        } = m
        {
            self.begins.fetch_add(1, Ordering::SeqCst);
            let held = self.held.lock().expect("unpoisoned");
            drop(self.release.wait_while(held, |held| *held).expect("unpoisoned"));
        }
    }
}

/// The coins' flush state between V's commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlushState {
    /// The block the coins were last made durable at.
    pub durable: Option<BlockRef>,
    /// A layer is frozen: a flush V submitted that W has not released yet.
    pub frozen: bool,
    /// The full flushes W began.
    pub begins: u64,
}

impl Node {
    /// V at genesis, its clock at `now`.
    pub fn start(p: Arc<ChainSetup>, now: i64, flush: Flush) -> Node {
        let dir = tempfile::tempdir_in(std::env::var_os("TMPDIR").unwrap_or_else(|| ".".into()))
            .expect("a temporary directory");
        let blocks_dir = dir.path().join("blocks");
        std::fs::DirBuilder::new().mode(0o700).create(&blocks_dir).expect("the blocks directory");
        let latch = GlobalFailStop::new();
        let abort = Arc::new(RecordingAbort::default());
        let fs = Arc::new(OsFs::new());
        let config = BlockFilesConfig::new(blocks_dir.clone(), p.message_start);
        let files = BlockFiles::with_fs(fs.clone(), config, BlockFilesState::default());
        let files = Arc::new(GuardedFiles::new(files, latch.clone()));
        let path = || AbortPath::new(abort.clone(), latch.clone());
        let pow = *HeaderParams::from_chain_params(&p).expect("regtest's header rules").pow();
        let block_files = || NodeBlockFiles::new(files.clone(), pow, Arc::new(path()));

        let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
        let db = Arc::new(GuardedEngine::new(engine, latch.clone(), abort.clone()));
        let (coins_share, f) = match flush {
            Flush::Never => (1 << 30, Ratio::HALF),
            Flush::Node {
                dbcache,
                f,
            } => (dbcache, f),
        };
        let budget = CoinsBudget::new(coins_share, 0, f, 0);
        let coins = Arc::new(Mutex::new(CoinsCache::new(db.clone(), budget, Salt::fixed(7), None)));
        let net_info = node_net_info(&p, false).expect("regtest's NetInfo rules");
        let genesis = genesis_block(&p);
        let store = Arc::new(RocksEvoStore::new(db.clone(), node_codec(net_info), genesis.hash()));
        let layer = SharedEvoLayer::new(store.resume_layer().expect("a fresh meta row"));
        let mn_list = MnListSignal::new(Default::default());
        let ledger = Arc::new(EffectLedger::default());
        let rows = Arc::new(RuntimeRows::new(store.clone(), Arc::clone(&ledger)));
        // The node's facade (dashd-node's `node_evo`): route A reads the layer's mined rows, and
        // the mineable applier erases from and re-adds to its pool (CH7b-1).
        let mined = CommittedMinedViews::new(Arc::clone(&store), layer.clone());
        let facade = MembersFacade::new(&p, Arc::clone(&rows), latch.clone())
            .expect("LLMQ parameters")
            .with_mined_views(Arc::new(mined));
        let facade = Arc::new(facade);
        let appliers = EffectAppliers {
            snapshot: Box::new(StoreSnapshots {
                rows,
            }),
            is_lock: Box::new(UnreachableIsLock),
            mineable: Box::new(FacadeMineable(Arc::clone(&facade))),
            platform_ban: Box::new(NoMetaEntries),
            notify: Box::new(mn_list.applier()),
        };
        let hooks = NodeEvo::new(NodeEvoParts {
            store,
            layer: layer.clone(),
            appliers,
            params: p.clone(),
            net_info,
            ev1: DmlChecks,
            cl: Box::new(AttemptClVerifier::default()),
            blocks: Box::new(FileBlocks(block_files())),
            scheme: dash_consensus::ctx::bls_ctx_for_block(&p, 1),
            mn_list,
        })
        .expect("regtest's payment parameters");
        let parts = ConnectorParts {
            coins: Arc::clone(&coins),
            files: files.clone(),
            queue: Arc::new(CheckQueue::serial()),
            caches: Arc::new(ScriptCaches::new(CacheSizes::default()).expect("caches")),
            helper: Box::new(NodeHelper),
            abort: path(),
            assume_valid: None,
            digest: None,
            xcheck: false,
        };
        let connector =
            CoreConnector::new(&p, parts, hooks, Arc::clone(&facade)).expect("the connector");
        let w = Arc::new(WMarks::default());
        let flush: Box<dyn dashd_chainstate::chain::FlushSite> = match flush {
            Flush::Never => Box::new(NoFlush),
            Flush::Node {
                ..
            } => {
                let hub = Arc::new(SubscriberHub::new());
                let driver = ThreadDriver::spawn(WThread {
                    engine: db,
                    files: files.clone(),
                    fs,
                    hub: Arc::clone(&hub),
                    obs: Arc::clone(&w) as Arc<dyn FlushObserver>,
                    tuning: Arc::new(MemoryTuning),
                    cfg: FlushConfig {
                        sub_batch: 16 << 20,
                        data_dir: dir.path().to_path_buf(),
                        blocks_dir,
                        live_copy: false,
                    },
                    covered_seq: 0,
                    crash: CrashRoll::seeded(0, 0),
                    on_crash: Box::new(|| {}),
                })
                .expect("W's thread");
                Box::new(NodeFlush::new(NodeFlushParts {
                    coins: Arc::clone(&coins),
                    driver,
                    policy: FlushPolicy::new(false),
                    ledger,
                    hub,
                    obs: Arc::new(NullObserver),
                    evo: Box::new(layer.clone()),
                    prune: Box::new(NoPrune),
                    inputs: Box::new(NoInputs),
                }))
            }
        };

        let clock = Arc::new(MockClock::at(now));
        let svc = Services {
            clock: clock.clone(),
            sporks: Arc::new(StaticSporks::for_network(&p, Some(false))),
            fail: Arc::new(NoReads),
        };
        let mut genesis_files = block_files();
        let (chain, idx) = load_genesis_block(p.clone(), checks_off(&p), svc, &mut genesis_files)
            .expect("genesis loads");
        let signals = ValidationSignals::default();
        let signals_thread = signals.start(abort.clone()).expect("the signals thread");
        let (v, join) = spawn(VParts {
            params: p.clone(),
            chain,
            idx,
            checks: Box::new(CoreChecks::new(p.clone()).expect("regtest's checks")),
            files: Box::new(block_files()),
            read_ahead: ReadAheadSetup::off(),
            connector: Box::new(connector),
            facade: Some(facade),
            mempool: Box::new(NullMempool),
            flush,
            is: Box::new(NoInstantSend),
            ui: Arc::new(NoUi),
            abort: abort.clone(),
            shutdown: Arc::new(LocalShutdown::default()),
            signals,
            clock,
            check_block_index: false,
            have_pruned: false,
            stop_at_height: 0,
            guard: GuardScope::of(&p),
        })
        .expect("V starts");
        v.activate_best_chain().recv().expect("V").expect("the startup ABC");
        Node {
            v,
            coins,
            join: Some(join),
            signals: Some(signals_thread),
            abort,
            latch,
            w,
            layer,
            _dir: dir,
        }
    }

    /// `ProcessNewBlock` of each of `blocks`, which V must connect.
    pub fn connect(&self, blocks: &[Arc<Block>]) {
        for block in blocks {
            let r = self.v.process_block(Arc::clone(block), true, None, true).recv().expect("V");
            assert!(r.accepted && r.state.is_valid(), "{}: {:?}", block.hash(), r.state);
        }
    }

    /// Accepts a branch's headers and blocks, including blocks that do not yet beat the tip.
    pub fn connect_branch(&self, blocks: &[Arc<Block>]) {
        let batch = HeaderBatch {
            headers: blocks.iter().map(|b| *b.header()).collect(),
            hashes: blocks.iter().map(|b| b.hash()).collect(),
        };
        let r = self.v.process_headers(batch, None).recv().expect("V");
        assert!(r.stop.is_none(), "branch headers: {:?}", r.stop);
        self.connect(blocks);
    }

    /// Mined entries in the unflushed indexes, including tombstones, and the tombstone subset.
    pub fn mined_counts(&self) -> (usize, usize) {
        let (active, frozen) = self.layer.lock().indexes();
        let mut counts = (0, 0);
        for index in std::iter::once(&active).chain(frozen.as_ref()) {
            for (_, value) in index.mined_from(&[], &[]) {
                counts.0 += 1;
                counts.1 += usize::from(value.is_none());
            }
        }
        counts
    }

    /// The block the coins were last made durable at. V's leaf: read between V's commands.
    pub fn durable(&self) -> Option<BlockRef> {
        self.coins.lock().expect("unpoisoned").durable()
    }

    /// The coins' durable block and frozen layer, read together, and W's count. V's leaf: read
    /// between V's commands.
    pub fn flush_state(&self) -> FlushState {
        let coins = self.coins.lock().expect("unpoisoned");
        FlushState {
            durable: coins.durable(),
            frozen: coins.frozen().is_some(),
            begins: self.w.begins.load(Ordering::SeqCst),
        }
    }

    /// Holds W at the next full flush's first step, or releases it.
    pub fn hold_w(&self, hold: bool) {
        *self.w.held.lock().expect("unpoisoned") = hold;
        self.w.release.notify_all();
    }
}

impl Drop for Node {
    /// Stops V; nothing reported `AbortNode` and the stores' latch is clear.
    fn drop(&mut self) {
        self.hold_w(false);
        self.v.shutdown();
        let join = self.join.take().expect("V runs until the drop");
        let exit = join.join().expect("an unmarked joiner").expect("V does not panic");
        if let Some(signals) = self.signals.take() {
            let _ = signals.stop();
        }
        assert!(exit.fatal.is_none(), "AbortNode: {:?}", exit.fatal);
        assert!(self.abort.messages().is_empty(), "AbortNode: {:?}", self.abort.messages());
        assert!(self.latch.tripped().is_none(), "the stores' latch: {:?}", self.latch.tripped());
    }
}

/// The node's header-chain configuration with `-checkblockindex` off.
fn checks_off(p: &ChainSetup) -> ChainConfig {
    ChainConfig {
        consistency_checks: false,
        ..ChainConfig::node(p)
    }
}
