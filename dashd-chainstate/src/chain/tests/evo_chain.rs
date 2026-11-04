//! CH3's reorg and ChainLock suites on a DIP0003-active chain (CH7 D15, §7.2; CH7a-4b). Each
//! suite body runs on two chain sources of one shape: the synthetic regtest chain CH3b's suites
//! were written on, over [`NullConnector`](crate::chain::NullConnector), and dashd
//! v24.0.0-rc.4's recorded chain (`tests/data/ch7a4b`), over the node's connector with
//! `NodeEvo` and EV1's checks, so every disconnect runs H7 and H9 and every connect H4.
//!
//! The recorded chain has DIP0003 at 48 and DIP0024 moved out (`-dip3params=48:48
//! -testactivationheight=dip0024@1000`): no rotation cycle base, and from 58 every mining window
//! of `llmq_test` (58-66, then every 24) carries dashd's null commitments (type 6). Main runs to
//! 200; fork a branches off 61, inside the first window, for 150 blocks; fork b off 40, below
//! DIP0003, for 7.

use std::os::unix::fs::DirBuilderExt as _;
use std::sync::{Arc, Mutex};

use dash_chain::Status;
use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::{TxRef, decode_block};
use dash_consensus::{Block, BlockId};
use dash_dml::MnList;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, TxType};
use dash_types::Numeric as _;
use dashd_store::MemoryEngine;
use dashd_store::flatfile::{BlockFiles as StoreFiles, BlockFilesConfig, BlockFilesState, OsFs};

use super::failure::tip;
use super::locks::{ClLog, cl_rig, intake, lock, record, settle};
use super::support::{AbortThenShutdown, Mined, Rig, RigConfig, branch};
use crate::chain::{
    AbortPath, AbortSink, DisconnectPool, LocalShutdown, MempoolSink, RecordingAbort, RemovalReason,
};
use crate::checkqueue::CheckQueue;
use crate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use crate::connect::{ChainHelper, ChainstateCell, ConnectorParts, CoreConnector, NodeHelper};
use crate::evo::RocksEvoStore;
use crate::evo::cl::{AttemptClVerifier, ClVerifier};
use crate::evo::effects::{
    EffectAppliers, FacadeMineable, MemPlatformBans, MnListSignal, StoreSnapshots,
    UnreachableIsLock,
};
use crate::evo::ev1::{DmlChecks, Ev1Checks};
use crate::evo::node::{
    EvoBlocks, FileBlocks, NodeEvo, NodeEvoParts, SharedEvoLayer, node_codec, node_net_info,
};
use crate::flush::{EffectLedger, EvoWindow as _};
use crate::llmq::facade::MembersFacade;
use crate::llmq::rows::RuntimeRows;
use crate::sigcache::{CacheSizes, ScriptCaches};
use crate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles,
};

/// dashd's main chain, blocks 1 to 200, one hex per line after `#` header lines.
const MAIN: &str = include_str!("../../../tests/data/ch7a4b/blocks.txt");
/// Fork a: blocks 62 to 211 off main's 61.
const FORK_A: &str = include_str!("../../../tests/data/ch7a4b/fork-a.txt");
/// Fork b: blocks 41 to 47 off main's 40.
const FORK_B: &str = include_str!("../../../tests/data/ch7a4b/fork-b.txt");

/// dashd's recorded masternode chain (`tests/data/ch7a5`, CH7a-5a), under the same parameters:
/// an EvoNode registered at 116, a regular masternode at 117, the EvoNode's ProUpServTx at 141, a
/// ProUpRegTx at 161, a ProUpRevTx at 181 and a collateral spend at 201, to 300.
pub(super) const MN_CHAIN: &str = include_str!("../../../tests/data/ch7a5/blocks.txt");
/// The recording's `-mocktime`: its blocks' times start there.
pub(super) const MOCKTIME: i64 = 1_700_000_000;
/// The height fork a branches off; its first block, 62, is inside `llmq_test`'s first window.
const FORK_A_OFF: u32 = 61;
/// The height fork b branches off, below DIP0003 (48).
const FORK_B_OFF: u32 = 40;
/// DIP0003's height on the recorded chain.
const DIP3: u32 = 48;

/// The chains a suite body runs on: main 1 to 200, fork a off 61 (62 to 211) and fork b off 40
/// (41 to 47). Element `i` of `main` is at height `i + 1`.
struct Chains {
    pub main: Vec<Mined>,
    pub fork_a: Vec<Mined>,
    pub fork_b: Vec<Mined>,
}

impl Chains {
    /// Main's block at `height`.
    pub fn at(&self, height: u32) -> &Mined {
        &self.main[height as usize - 1]
    }
}

/// Where a suite body's blocks come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// Regtest blocks mined on the fly (CH3b's), over [`NullConnector`].
    Synthetic,
    /// dashd's DIP0003-active chain, over the node's connector and `NodeEvo`.
    Recorded,
}

/// What a rig over [`Source::Recorded`] keeps besides V: its stores, which a reopen opens again,
/// the evo layer, whose tip the bodies check, and the abort sink the rig installs in place of its
/// own.
pub(super) struct Kept {
    disk: Option<NodeDisk>,
    layer: Option<SharedEvoLayer>,
    sink: Option<Arc<RecordingAbort>>,
}

impl Kept {
    /// The evo layer's tip, the block H6 last committed (`None` for the synthetic source).
    pub fn evo_tip(&self) -> Option<BlockHash> {
        self.layer.as_ref().and_then(|l| l.best_block())
    }

    /// The active coins' best block, as the connector's cache holds it (`None` for the
    /// synthetic source).
    #[cfg(all(feature = "statedigest", not(any(miri, loom))))]
    pub fn coins_best(&self) -> Option<BlockHash> {
        let disk = self.disk.as_ref()?;
        disk.coins.lock().expect("coins").best_block().map(|b| b.hash)
    }

    /// The installed sink's `AbortNode` reports: text and user message.
    pub fn reports(&self) -> Vec<(String, Option<String>)> {
        self.sink.as_ref().map(|sink| sink.reports()).unwrap_or_default()
    }

    /// No `AbortNode` was reported: to the installed sink, where the recorded rig's V, connector
    /// and stores report, or else to the rig's own.
    pub fn assert_no_abort(&self, rig: &Rig) {
        match &self.sink {
            Some(sink) => {
                let reports = sink.reports();
                assert!(reports.is_empty(), "the installed sink heard {reports:?}");
            }
            None => rig.assert_no_abort(),
        }
    }

    /// `cfg` with the node's V seams opened again on the recorded rig's stores, for a V
    /// restarted on them ([`NodeDisk::open`]); `next` is the height after the tip.
    pub fn reopen<C: Ev1Checks + Send + 'static>(
        &self,
        cfg: RigConfig,
        next: u32,
        seams: OpenSeams<C>,
    ) -> RigConfig {
        self.disk.as_ref().expect("a recorded rig's stores").open(cfg, next, seams)
    }
}

impl Source {
    /// `cfg` with this source's parameters, clock and connector.
    pub fn config(self, cfg: RigConfig) -> (RigConfig, Kept) {
        match self {
            Source::Synthetic => (
                cfg,
                Kept {
                    disk: None,
                    layer: None,
                    sink: None,
                },
            ),
            Source::Recorded => recorded_config(cfg),
        }
    }

    /// The source's chains, on `rig`'s genesis.
    pub fn chains(self, rig: &Rig) -> Chains {
        match self {
            Source::Synthetic => {
                let main = branch(&rig.p, &rig.genesis, 200, 0);
                let fork_a = branch(&rig.p, &main[FORK_A_OFF as usize - 1], 150, 1);
                let fork_b = branch(&rig.p, &main[FORK_B_OFF as usize - 1], 7, 2);
                Chains {
                    main,
                    fork_a,
                    fork_b,
                }
            }
            Source::Recorded => recorded_chains(&rig.genesis),
        }
    }

    /// A rig over this source with `cfg`'s other seams, and its chains.
    pub fn rig(self, cfg: RigConfig) -> (Rig, Chains, Kept) {
        let (cfg, kept) = self.config(cfg);
        let rig = Rig::with(cfg);
        let chains = self.chains(&rig);
        (rig, chains, kept)
    }

    /// [`Source::rig`] with SPORK_19 on and a [`ClLog`] (CH3b's ChainLock rig).
    pub fn cl_rig(self, cfg: RigConfig) -> (Rig, Arc<ClLog>, Chains, Kept) {
        let (cfg, kept) = self.config(cfg);
        let (rig, log) = cl_rig(cfg);
        let chains = self.chains(&rig);
        (rig, log, chains, kept)
    }
}

/// The recorded chain's parameters: regtest with `-dip3params=48:48
/// -testactivationheight=dip0024@1000`, as `record.sh` ran dashd.
pub(super) fn recorded_params() -> Arc<ChainSetup> {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "48:48")
        .arg(ChainOption::TestActivationHeight, "dip0024@1000")
        .build()
        .expect("regtest");
    Arc::new(p)
}

/// `text`'s blocks, skipping its `#` header lines.
pub(super) fn decode(text: &str) -> Vec<Arc<Block>> {
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

/// `blocks` from `parent`, each the child of the one before.
pub(super) fn linked(parent: &Mined, blocks: Vec<Arc<Block>>) -> Vec<Mined> {
    let mut prev = parent.clone();
    blocks
        .into_iter()
        .map(|block| {
            assert_eq!(block.header().prev_hash(), prev.hash(), "a chain");
            prev = Mined {
                block,
                height: prev.height + 1,
            };
            prev.clone()
        })
        .collect()
}

fn recorded_chains(genesis: &Mined) -> Chains {
    let main = linked(genesis, decode(MAIN));
    assert_eq!(main.len(), 200);
    let fork_a = linked(&main[FORK_A_OFF as usize - 1], decode(FORK_A));
    let fork_b = linked(&main[FORK_B_OFF as usize - 1], decode(FORK_B));
    assert_eq!((fork_a.len(), fork_b.len()), (150, 7));
    Chains {
        main,
        fork_a,
        fork_b,
    }
}

/// The node's V seams for the recorded chain ([`node_config`]), with its ChainLock verifier
/// (CH7b-3 C5); the clock is the recording's, a day on.
fn recorded_config(cfg: RigConfig) -> (RigConfig, Kept) {
    recorded_config_on(cfg, recorded_params(), MOCKTIME, None)
}

/// [`recorded_config`] for `p`'s chain recorded at `mocktime`, its connector writing its
/// state-hash records to `log` (CH7a-5b's scratch harness).
pub(super) fn recorded_config_on(
    cfg: RigConfig,
    p: Arc<ChainSetup>,
    mocktime: i64,
    log: Option<Box<dyn crate::connect::DigestSink>>,
) -> (RigConfig, Kept) {
    let after_genesis = mocktime - i64::from(Mined::genesis(&p).time()) + 86_400;
    let seams = OpenSeams {
        ev1: DmlChecks,
        cl: Box::new(AttemptClVerifier::default()),
        helper: Box::new(NodeHelper),
        blocks: Box::new(|blocks: Box<dyn EvoBlocks>| blocks),
    };
    node_config_logged(p, after_genesis, cfg, seams, log)
}

/// The node's V seams for a recorded chain under `p`, as dashd-node's `attach_parts` wires them:
/// CH3a's connector over in-memory coins and on-disk block files, with `NodeEvo`, the EV1 checks
/// `ev1` and the members facade over the same engine, its evo layer never flushed; V and the
/// connector share the facade, whose mineable pool takes a mined commitment's effects. The
/// stores and the connector report to the node's abort, the installed sink and then the rig's
/// shutdown, as V does. Step 10 verifies with `cl` and reads its parent through `blocks` over
/// the node's block files; `ConnectBlock` asks `helper` (the node's is [`NodeHelper`]); the clock
/// starts `after_genesis` seconds after genesis. The stores are kept for [`Kept::reopen`].
pub(super) fn node_config<C: Ev1Checks + Send + 'static>(
    p: Arc<ChainSetup>,
    after_genesis: i64,
    cfg: RigConfig,
    ev1: C,
    cl: Box<dyn ClVerifier + Send>,
    helper: Box<dyn ChainHelper>,
    blocks: impl FnOnce(Box<dyn EvoBlocks>) -> Box<dyn EvoBlocks> + 'static,
) -> (RigConfig, Kept) {
    let seams = OpenSeams {
        ev1,
        cl,
        helper,
        blocks: Box::new(blocks),
    };
    node_config_logged(p, after_genesis, cfg, seams, None)
}

/// [`node_config`] with its seams whole, the connector writing its state-hash records to `log`.
fn node_config_logged<C: Ev1Checks + Send + 'static>(
    p: Arc<ChainSetup>,
    after_genesis: i64,
    cfg: RigConfig,
    seams: OpenSeams<C>,
    log: Option<Box<dyn crate::connect::DigestSink>>,
) -> (RigConfig, Kept) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::DirBuilder::new().mode(0o700).create(dir.path().join("blocks")).expect("dir");
    let latch = GlobalFailStop::new();
    let recorder = Arc::new(RecordingAbort::default());
    let shutdown = cfg.shutdown.clone().unwrap_or_default();
    let sink: Arc<dyn AbortSink> = Arc::new(AbortThenShutdown {
        sink: recorder.clone(),
        shutdown: shutdown.clone(),
    });
    let db = Arc::new(GuardedEngine::new(
        MemoryEngine::new(&CHAINSTATE_SCHEMA),
        latch.clone(),
        sink.clone(),
    ));
    let budget = CoinsBudget::new(8 << 20, 0, Ratio::HALF, 0);
    let coins = Arc::new(Mutex::new(CoinsCache::new(db.clone(), budget, Salt::fixed(7), None)));
    let files = Arc::new(GuardedFiles::new(
        StoreFiles::with_fs(
            Arc::new(OsFs::new()),
            BlockFilesConfig::new(dir.path().join("blocks"), p.message_start),
            BlockFilesState::default(),
        ),
        latch.clone(),
    ));
    let net_info = node_net_info(&p, false).expect("regtest's NetInfo rules");
    let genesis = Mined::genesis(&p).hash();
    let store = Arc::new(RocksEvoStore::new(db, node_codec(net_info), genesis));
    let layer = SharedEvoLayer::new(store.resume_layer().expect("a fresh meta row"));
    let disk = NodeDisk {
        _dir: dir,
        p,
        after_genesis,
        latch,
        recorder: recorder.clone(),
        sink,
        shutdown,
        coins,
        files,
        store,
        layer: layer.clone(),
    };
    let cfg = disk.open_logged(cfg, 1, seams, log);
    let kept = Kept {
        disk: Some(disk),
        layer: Some(layer),
        sink: Some(recorder),
    };
    (cfg, kept)
}

/// A recorded rig's stores, which outlive its V: the block files and their directory, the
/// engine's coins and evo store with the evo layer (the rig flushes nothing, so the in-memory
/// coins and layer are what a clean stop leaves), and the node's latch and abort path.
struct NodeDisk {
    _dir: tempfile::TempDir,
    p: Arc<ChainSetup>,
    after_genesis: i64,
    latch: GlobalFailStop,
    recorder: Arc<RecordingAbort>,
    sink: Arc<dyn AbortSink>,
    shutdown: Arc<LocalShutdown>,
    coins: ChainstateCell<MemoryEngine>,
    files: Arc<GuardedFiles<OsFs>>,
    store: Arc<RocksEvoStore<MemoryEngine>>,
    layer: SharedEvoLayer,
}

/// Step 10's parent reads over the node's block files, as a test wraps them.
pub(super) type WrapBlocks = Box<dyn FnOnce(Box<dyn EvoBlocks>) -> Box<dyn EvoBlocks>>;

/// What an open of [`NodeDisk`] takes besides its stores: [`node_config`]'s `ev1`, `cl`,
/// `helper` and `blocks`.
pub(super) struct OpenSeams<C> {
    pub ev1: C,
    pub cl: Box<dyn ClVerifier + Send>,
    pub helper: Box<dyn ChainHelper>,
    pub blocks: WrapBlocks,
}

impl NodeDisk {
    /// `cfg` with the node's V seams on these stores, built as dashd-node's `node_evo` builds
    /// them at each open: the rows, the members facade (so step 10's cell starts empty), the
    /// appliers, `NodeEvo` with the scheme for `next` and the connector.
    fn open<C: Ev1Checks + Send + 'static>(
        &self,
        cfg: RigConfig,
        next: u32,
        seams: OpenSeams<C>,
    ) -> RigConfig {
        self.open_logged(cfg, next, seams, None)
    }

    /// [`NodeDisk::open`], the connector writing its state-hash records to `log`.
    fn open_logged<C: Ev1Checks + Send + 'static>(
        &self,
        cfg: RigConfig,
        next: u32,
        seams: OpenSeams<C>,
        log: Option<Box<dyn crate::connect::DigestSink>>,
    ) -> RigConfig {
        let p = &self.p;
        let path = || AbortPath::new(self.sink.clone(), self.latch.clone());
        let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
        let block_files = || NodeBlockFiles::new(self.files.clone(), pow, Arc::new(path()));
        let net_info = node_net_info(p, false).expect("regtest's NetInfo rules");
        let mn_list = MnListSignal::new(Default::default());
        let rows =
            Arc::new(RuntimeRows::new(self.store.clone(), Arc::new(EffectLedger::default())));
        let facade = Arc::new(
            MembersFacade::new(p, Arc::clone(&rows), self.latch.clone()).expect("LLMQ parameters"),
        );
        let appliers = EffectAppliers {
            snapshot: Box::new(StoreSnapshots {
                rows,
            }),
            is_lock: Box::new(UnreachableIsLock),
            mineable: Box::new(FacadeMineable(Arc::clone(&facade))),
            platform_ban: Box::new(MemPlatformBans::default()),
            notify: Box::new(mn_list.applier()),
        };
        let hooks = NodeEvo::new(NodeEvoParts {
            store: self.store.clone(),
            layer: self.layer.clone(),
            appliers,
            params: p.clone(),
            net_info,
            ev1: seams.ev1,
            cl: seams.cl,
            blocks: (seams.blocks)(Box::new(FileBlocks(block_files()))),
            scheme: dash_consensus::ctx::bls_ctx_for_block(p, next),
            mn_list,
        })
        .expect("regtest's payment parameters");
        let parts = ConnectorParts {
            coins: self.coins.clone(),
            files: self.files.clone(),
            queue: Arc::new(CheckQueue::serial()),
            caches: Arc::new(ScriptCaches::new(CacheSizes::default()).expect("caches")),
            helper: seams.helper,
            abort: path(),
            assume_valid: None,
            digest: log,
            xcheck: true,
        };
        let connector =
            CoreConnector::new(p, parts, hooks, Arc::clone(&facade)).expect("connector");
        RigConfig {
            after_genesis: Some(self.after_genesis),
            files: Some(Box::new(block_files())),
            connector: Some(Box::new(connector)),
            facade: Some(facade),
            abort: Some(self.recorder.clone()),
            shutdown: Some(self.shutdown.clone()),
            params: Some(p.clone()),
            ..cfg
        }
    }
}

/// Whether `block` carries a quorum commitment (type 6).
fn has_qc(block: &Block) -> bool {
    block
        .txs()
        .iter()
        .any(|tx| tx.is_special_tx_version() && tx.tx_type() == TxType::QuorumCommitment)
}

/// The recorded chain's shape, which the suites' evo claims rest on: DIP0003 at 48, every block
/// of `llmq_test`'s mining windows (58-66 from the cycle at 48, then every 24) carries a
/// commitment, and no other block does; fork a's 62-66 do too.
#[test]
fn the_recorded_chain_carries_null_commitments_in_each_window() {
    let p = recorded_params();
    let c = recorded_chains(&Mined::genesis(&p));
    let in_window = |h: u32| h >= DIP3 && (h - DIP3) % 24 >= 10 && (h - DIP3) % 24 <= 18;
    for m in c.main.iter().chain(&c.fork_a).chain(&c.fork_b) {
        assert_eq!(has_qc(&m.block), in_window(m.height), "{}", m.height);
    }
}

/// The DMN tip, V's tip and the lists `evo_view` reads at both, by block hash.
fn evo_at(rig: &Rig) -> (Option<BlockId>, Option<BlockId>, BlockHash, BlockHash) {
    rig.v
        .view_handle()
        .query(|q| {
            let e = q.evo_view();
            let tip_list = *e.tip_list().expect("reads").block_hash();
            (e.tip, e.dmn_tip, tip_list, *e.dmn_list().expect("reads").block_hash())
        })
        .recv()
        .expect("V serves")
}

/// The source's evo state agrees with V's tip `m`: the evo layer's tip is `m` and `evo_view`'s
/// lists are `m`'s (the recorded source), and the DMN tip is V's tip (both). Below DIP0003 a list
/// is Core's default, with a null block hash (`GetListForBlockInternal`, `dmn:830-832`).
fn assert_evo_at(src: Source, rig: &Rig, kept: &Kept, m: &Mined) {
    let id = rig.id(m);
    let (tip, dmn_tip, tip_list, dmn_list) = evo_at(rig);
    assert_eq!((tip, dmn_tip), (Some(id), Some(id)), "{src:?} at {}", m.height);
    if src == Source::Recorded {
        assert_eq!(kept.evo_tip(), Some(m.hash()), "the evo layer's tip at {}", m.height);
        let list = if m.height >= DIP3 {
            m.hash()
        } else {
            BlockHash::ZERO
        };
        assert_eq!((tip_list, dmn_list), (list, list), "the lists at {}", m.height);
    }
    kept.assert_no_abort(rig);
}

/// The list a drain handed the mempool, by its block and height; `None` for a non-adding drain.
type Drained = Option<(BlockHash, i32)>;

/// A mempool that records each `MaybeUpdateMempoolForReorg`'s list (D60).
#[derive(Clone, Default)]
struct Drains(Arc<Mutex<Vec<Drained>>>);

impl Drains {
    fn take(&self) -> Vec<Drained> {
        std::mem::take(&mut *self.0.lock().expect("drains"))
    }
}

impl MempoolSink for Drains {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        Box::new(())
    }

    fn maybe_update_for_reorg_at(
        &mut self,
        _: Box<dyn DisconnectPool>,
        list: Option<&Arc<MnList>>,
    ) {
        let at = list.map(|l| (*l.block_hash(), l.height()));
        self.0.lock().expect("drains").push(at);
    }

    fn remove_for_block_at(&mut self, _: &[TxRef], _: u32, _: &Arc<MnList>) {}

    fn remove_expired_asset_unlock(&mut self, _: u32) {}

    fn remove_recursive(&mut self, _: &TxRef, _: RemovalReason) {}

    fn add_transactions_updated(&mut self, _: u32) {}

    fn check(&mut self, _: u32) {}
}

/// D16's publication of a non-empty list (OD-CP-21; the `mnlist-tip-publish` PF0 row): on dashd's
/// masternode chain through the node's connector, at every tip from 116, the first registration,
/// to 300, the list V publishes in `ChainView` is the very `Arc` that the DMN tip's and the tip's
/// list reads return, the list cache's. A publication clones that pointer whatever the list's
/// size, and copies no list.
#[test]
fn the_published_list_is_the_tip_lists_arc() {
    let (cfg, kept) = Source::Recorded.config(RigConfig::default());
    let rig = Rig::with(cfg);
    let main = linked(&rig.genesis, decode(MN_CHAIN));
    assert_eq!(main.len(), 300);
    let h = rig.v.view_handle();
    for m in &main {
        rig.accept(m);
        if m.height < 116 {
            continue;
        }
        let view = h.view();
        let (dmn, tip) = h
            .query(|q| {
                let e = q.evo_view();
                (e.dmn_list().expect("reads"), e.tip_list().expect("reads"))
            })
            .recv()
            .expect("V");
        assert!(!view.mn_list.is_empty(), "{}: an empty list", m.height);
        assert_eq!(*view.mn_list.block_hash(), m.hash(), "{}", m.height);
        assert!(Arc::ptr_eq(&view.mn_list, &dmn), "{}: not the DMN tip's list", m.height);
        assert!(Arc::ptr_eq(&dmn, &tip), "{}: not the tip's list", m.height);
    }
    kept.assert_no_abort(&rig);
}

/// ABC's reorg onto a longer fork across mining windows (CH3b `abc`): main to 200, then fork a
/// from 62 to 211. One hold disconnects 200 to 62, the first window's 62-66 and every later
/// window's commitment blocks among them, through H9, and connects fork a's own windows through
/// H4; the evo layer and the lists follow the tip.
fn reorg_onto_a_longer_fork_across_windows(src: Source) {
    let (rig, c, kept) = src.rig(RigConfig::default());
    for m in &c.main {
        rig.accept(m);
    }
    assert_evo_at(src, &rig, &kept, c.at(200));
    for m in &c.fork_a {
        rig.accept(m);
    }
    let fork_tip = c.fork_a.last().expect("150 blocks");
    assert_eq!(tip(&rig), rig.id(fork_tip));
    assert_evo_at(src, &rig, &kept, fork_tip);
    for m in &c.main[FORK_A_OFF as usize..] {
        assert!(!rig.status(m).has(Status::FAILED_MASK), "{}", m.height);
    }
    assert_eq!(rig.idx(|ix| ix.candidates().collect::<Vec<_>>()), [rig.id(fork_tip)]);
}

#[test]
fn reorg_onto_a_longer_fork_across_windows_synthetic() {
    reorg_onto_a_longer_fork_across_windows(Source::Synthetic);
}

#[test]
fn reorg_onto_a_longer_fork_across_windows_recorded() {
    reorg_onto_a_longer_fork_across_windows(Source::Recorded);
}

/// `InvalidateBlock` through a window, then `ReconsiderBlock` (CH3b `failure`): invalidating 62,
/// inside the first window, walks 200 back to 61, one disconnect per iteration, each marked
/// failed. Each walk iteration drains the mempool (`v:3965-3971`, D60): the first ten add, so
/// each moves the DMN tip to the new tip and hands the mempool the list there; the rest add
/// nothing and read no list. Reconsidering 62 reconnects to 200.
fn invalidate_and_reconsider_through_a_window(src: Source) {
    let drains = Drains::default();
    let (rig, c, kept) = src.rig(RigConfig {
        mempool: Some(Box::new(drains.clone())),
        ..RigConfig::default()
    });
    for m in &c.main {
        rig.accept(m);
    }
    drains.take();
    let cut = c.at(FORK_A_OFF + 1);
    assert_eq!(rig.v.invalidate_block(cut.hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(c.at(FORK_A_OFF)));
    for m in &c.main[FORK_A_OFF as usize..] {
        assert!(rig.status(m).has(Status::FAILED_MASK), "{}", m.height);
    }
    let drained = drains.take();
    assert_eq!(drained.len(), 200 - FORK_A_OFF as usize, "one drain per disconnect");
    // The first ten disconnects leave 199 to 190 the tip.
    let adding: Vec<_> = (190..200).rev().map(|h| (c.at(h).hash(), h as i32)).collect();
    let (first, rest) = drained.split_at(10);
    let first: Vec<_> = first.iter().map(|d| d.expect("an adding drain")).collect();
    match src {
        // Core's empty list before DIP0003, as `NoEvoReads` gives it, names no block.
        Source::Synthetic => assert!(first.iter().all(|&(h, _)| h == BlockHash::ZERO)),
        Source::Recorded => assert_eq!(first, adding),
    }
    assert!(rest.iter().all(Option::is_none), "{rest:?}");

    assert_eq!(rig.v.reconsider_block(cut.hash(), false).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(c.at(200)));
    assert_evo_at(src, &rig, &kept, c.at(200));
}

#[test]
fn invalidate_and_reconsider_through_a_window_synthetic() {
    invalidate_and_reconsider_through_a_window(Source::Synthetic);
}

#[test]
fn invalidate_and_reconsider_through_a_window_recorded() {
    invalidate_and_reconsider_through_a_window(Source::Recorded);
}

/// A reorg back below DIP0003 and up again (CH3b `failure`, `abc`): invalidating 41 walks 200
/// back to 40, through DIP0003's own block 48, whose undo leaves the evo rows (D6, D64); fork b
/// connects 41 to 47 below DIP0003; reconsidering 41 reorgs to main's 200, more work, across 48
/// and every window again.
fn a_reorg_below_dip3_and_back(src: Source) {
    let (rig, c, kept) = src.rig(RigConfig::default());
    for m in &c.main {
        rig.accept(m);
    }
    let cut = c.at(FORK_B_OFF + 1);
    assert_eq!(rig.v.invalidate_block(cut.hash()).recv(), Ok(Ok(())));
    for m in &c.fork_b {
        rig.accept(m);
    }
    let fork_tip = c.fork_b.last().expect("7 blocks");
    assert!(fork_tip.height < DIP3);
    assert_eq!(tip(&rig), rig.id(fork_tip));
    assert_evo_at(src, &rig, &kept, fork_tip);

    assert_eq!(rig.v.reconsider_block(cut.hash(), false).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(c.at(200)));
    assert_evo_at(src, &rig, &kept, c.at(200));
}

#[test]
fn a_reorg_below_dip3_and_back_synthetic() {
    a_reorg_below_dip3_and_back(Source::Synthetic);
}

#[test]
fn a_reorg_below_dip3_and_back_recorded() {
    a_reorg_below_dip3_and_back(Source::Recorded);
}

/// A disconnect that ends on a window block: main to 66, the first window's last block, then
/// `invalidateblock 66`. H9 undoes 66 and H6 commits its parent as the evo tip, which the evo
/// check reads after that one disconnect, before a later disconnect's or connect's
/// `VerifyBestBlock` (`v:2294`, `:2533`) could refuse a wrong one. `reconsiderblock 66` then
/// connects it again, and main goes on to 200.
fn an_invalidate_ending_on_a_window_block(src: Source) {
    let (rig, c, kept) = src.rig(RigConfig::default());
    let last = c.at(66);
    if src == Source::Recorded {
        assert!(has_qc(&last.block) && !has_qc(&c.at(67).block), "66 ends the first window");
    }
    for m in &c.main[..66] {
        rig.accept(m);
    }
    assert_evo_at(src, &rig, &kept, last);
    assert_eq!(rig.v.invalidate_block(last.hash()).recv(), Ok(Ok(())));
    assert_evo_at(src, &rig, &kept, c.at(65));
    assert_eq!(rig.v.reconsider_block(last.hash(), false).recv(), Ok(Ok(())));
    assert_evo_at(src, &rig, &kept, last);
    for m in &c.main[66..] {
        rig.accept(m);
    }
    assert_evo_at(src, &rig, &kept, c.at(200));
}

#[test]
fn an_invalidate_ending_on_a_window_block_synthetic() {
    an_invalidate_ending_on_a_window_block(Source::Synthetic);
}

#[test]
fn an_invalidate_ending_on_a_window_block_recorded() {
    an_invalidate_ending_on_a_window_block(Source::Recorded);
}

/// The evo check's no-abort half reads the sink the recorded rig installs: a report there, with
/// V and the evo state otherwise in agreement, fails it.
#[test]
#[should_panic(expected = "the installed sink heard")]
fn a_report_to_the_installed_sink_fails_the_evo_check() {
    let (rig, c, kept) = Source::Recorded.rig(RigConfig::default());
    for m in &c.main[..DIP3 as usize] {
        rig.accept(m);
    }
    kept.sink.as_ref().expect("the recorded rig's").abort_node("a planted report", None);
    assert_evo_at(Source::Recorded, &rig, &kept, c.at(DIP3));
}

/// ChainLock enforcement onto a fork across a window (CH3b `locks`, §5.2): main to 200, fork a
/// to 90 (its own windows 62-66 and 82-90), then a lock on fork a's 70. `EnforceBlock` marks
/// main's 62 conflicting, disconnecting 200 to 62 in one pool, drains it once adding (`v:4125`,
/// so the DMN tip moves to 61), and ABC activates fork a to 90; the lock is notified once.
fn chainlock_enforcement_onto_a_fork_across_a_window(src: Source) {
    let drains = Drains::default();
    let (rig, log, c, kept) = src.cl_rig(RigConfig {
        mempool: Some(Box::new(drains.clone())),
        ..RigConfig::default()
    });
    let fork = &c.fork_a[..(90 - FORK_A_OFF) as usize];
    for m in c.main.iter().chain(fork) {
        rig.accept(m);
    }
    assert_eq!(tip(&rig), rig.id(c.at(200)));
    intake(&rig);
    drains.take();
    let locked = &fork[(70 - FORK_A_OFF - 1) as usize];
    assert_eq!(locked.height, 70);
    let sig = lock(locked);
    assert!(record(&rig, &sig));
    settle(&rig);

    let fork_tip = fork.last().expect("to 90");
    assert_eq!(tip(&rig), rig.id(fork_tip));
    assert!(rig.status(c.at(FORK_A_OFF + 1)).has(Status::CONFLICT_CHAINLOCK));
    assert!(!rig.status(c.at(FORK_A_OFF)).has(Status::CONFLICT_CHAINLOCK));
    assert_eq!(log.take(), [(rig.id(locked), sig)]);
    let fork_point = c.at(FORK_A_OFF);
    let drained = drains.take();
    match src {
        Source::Synthetic => assert_eq!(drained, [Some((BlockHash::ZERO, -1))]),
        Source::Recorded => {
            assert_eq!(drained, [Some((fork_point.hash(), fork_point.height as i32))]);
        }
    }
    assert_evo_at(src, &rig, &kept, fork_tip);
}

#[test]
fn chainlock_enforcement_onto_a_fork_across_a_window_synthetic() {
    chainlock_enforcement_onto_a_fork_across_a_window(Source::Synthetic);
}

#[test]
fn chainlock_enforcement_onto_a_fork_across_a_window_recorded() {
    chainlock_enforcement_onto_a_fork_across_a_window(Source::Recorded);
}

/// EnforceBlock on an invalidated locked chain (CH3b `locks`, `v:4079-4082`): invalidating 62
/// walks back to 61; a lock on 100 resets the failure flags, and ABC reconnects 62 to 200
/// through every window.
fn enforcement_resets_a_chain_invalidated_inside_a_window(src: Source) {
    let (rig, log, c, kept) = src.cl_rig(RigConfig::default());
    for m in &c.main {
        rig.accept(m);
    }
    let cut = c.at(FORK_A_OFF + 1);
    assert_eq!(rig.v.invalidate_block(cut.hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(c.at(FORK_A_OFF)));
    intake(&rig);
    let sig = lock(c.at(100));
    assert!(record(&rig, &sig));
    settle(&rig);
    assert_eq!(tip(&rig), rig.id(c.at(200)));
    for m in &c.main {
        assert!(!rig.status(m).has(Status::FAILED_MASK), "{}", m.height);
    }
    assert_eq!(log.take(), [(rig.id(c.at(100)), sig)]);
    assert_evo_at(src, &rig, &kept, c.at(200));
}

#[test]
fn enforcement_resets_a_chain_invalidated_inside_a_window_synthetic() {
    enforcement_resets_a_chain_invalidated_inside_a_window(Source::Synthetic);
}

#[test]
fn enforcement_resets_a_chain_invalidated_inside_a_window_recorded() {
    enforcement_resets_a_chain_invalidated_inside_a_window(Source::Recorded);
}
