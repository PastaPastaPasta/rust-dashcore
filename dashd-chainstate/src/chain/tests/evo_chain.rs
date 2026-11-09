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

use std::collections::{BTreeSet, HashMap};
use std::os::unix::fs::DirBuilderExt as _;
use std::sync::{Arc, Mutex};

use super::connect::Latched;
use super::read_faults::{Faults, ReadFaults};
use dash_chain::{IndexAccess as _, Status};
use dash_consensus::coins::{BlockUndo, Coin, TxUndo};
use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::{Header, OutPoint, TxHash, TxIn, TxOut, TxRef, decode_block};
use dash_consensus::{Amount, Block, BlockId, Tx};
use dash_dml::{MnList, ProTxHash};
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, BlockHeader, Transaction, TxType};
use dash_types::Numeric as _;
use dashd_store::flatfile::{
    BlockFiles as StoreFiles, BlockFilesConfig, BlockFilesState, OsFs, undo_checksum,
};

use super::evo_chain_cl::{CHAINLOCKS, run_blocks, run_params};
use super::failure::tip;
use super::locks::{ClLog, cl_rig, intake, lock, record, settle};
use super::support::{AbortThenShutdown, Mined, Rig, RigConfig, branch};
use crate::chain::{
    AbortPath, AbortSink, BlockFiles as _, DisconnectPool, LocalShutdown, MempoolSink,
    RecordingAbort, RemovalReason,
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
use crate::evo::rpc_shape::{BlockPayments, RenderParams, masternode_payments, payments_at_fees};
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

/// An engine row: its column family's name, key and value.
pub(super) type StoredRow = (&'static str, Vec<u8>, Vec<u8>);

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

    /// `m`'s undo record, read from the recorded rig's block files at the position V's index
    /// holds for it, as `getblock`'s undo read does.
    pub fn undo(&self, rig: &Rig, m: &Mined) -> BlockUndo {
        let hash = m.hash();
        let pos = rig.v.view_handle().query(move |q| {
            let reader = q.chain.reader();
            reader.undo_pos(reader.lookup(&hash)?)
        });
        let pos = pos.recv().expect("V serves").expect("an undo position");
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        let pow = *HeaderParams::from_chain_params(&disk.p).expect("regtest").pow();
        let path = Arc::new(AbortPath::new(disk.sink.clone(), disk.latch.clone()));
        let files = NodeBlockFiles::new(disk.files.clone(), pow, path);
        files.read_undo(pos, &m.block.header().prev_hash()).expect("the undo")
    }

    /// Writes `undo` over `m`'s undo record in the recorded rig's rev file, with its checksum, so
    /// it reads back whole (VerifyDB's level 2) as `undo`.
    pub fn replace_undo(&self, rig: &Rig, m: &Mined, undo: &BlockUndo) {
        use std::os::unix::fs::FileExt as _;
        let hash = m.hash();
        let pos = rig.v.view_handle().query(move |q| {
            let reader = q.chain.reader();
            reader.undo_pos(reader.lookup(&hash)?)
        });
        let pos = pos.recv().expect("V serves").expect("an undo position");
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        let path = disk._dir.path().join("blocks").join(format!("rev{:05}.dat", pos.file));
        let bytes = undo.core_bytes();
        let prev = m.block.header().prev_hash();
        let checksum = undo_checksum(prev.as_bytes(), bytes.as_slice());
        let file = std::fs::OpenOptions::new().write(true).open(path).expect("opens");
        file.write_all_at(&[bytes.as_slice(), &checksum].concat(), u64::from(pos.pos))
            .expect("writes");
    }

    /// Whether the node latch has tripped (a recorded rig's).
    pub fn tripped(&self) -> bool {
        self.disk.as_ref().expect("a recorded rig's stores").latch.tripped().is_some()
    }

    /// Whether V's stop asked the node to shut down (a recorded rig's).
    pub fn shutdown_requested(&self) -> bool {
        use crate::chain::ShutdownFlag as _;
        self.disk.as_ref().expect("a recorded rig's stores").shutdown.shutdown_requested()
    }

    /// The engine's read faults (a recorded rig's).
    pub fn faults(&self) -> &Faults {
        &self.disk.as_ref().expect("a recorded rig's stores").faults
    }

    /// CH2's evo flush, run on the rig's stores between attempts: the layer's committed rows
    /// written with `meta.evo_tip` and released, so later reads of them go to the engine.
    pub fn flush_evo(&self) {
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        crate::evo::testkit::flush(&disk.store, &mut disk.layer.lock());
    }

    /// CH2's coins flush, run on the rig's stores with V stopped: the cache's dirty coins written
    /// and released, so later reads of them go to the engine.
    pub fn flush_coins(&self) {
        use crate::coins::CoinLayerOps as _;
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        let mut cache = disk.coins.lock().expect("coins");
        let frozen = cache.freeze(0);
        let table = crate::coins::table::CoinsTable::default();
        let mut batch = cache.db().batch();
        for (k, flags, coin) in frozen.coins().iter_dirty() {
            if flags.is_spent() {
                table.erase(&mut batch, &k);
            } else {
                table.put(&mut batch, &k, coin);
            }
        }
        cache.db().write(batch, dashd_store::Commit::Durable).expect("the coins write");
        let generation = frozen.generation();
        drop(frozen);
        cache.release(generation);
    }

    /// The `llmq_mined` keys (the column family's, without the mined tag) of the rows V's evo
    /// layer holds live.
    pub fn resident_mined(&self) -> Vec<Vec<u8>> {
        use crate::evo::codec::MinedCf;
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        let (live, frozen) = disk.layer.lock().indexes();
        let tag = [MinedCf::Mined as u8];
        let mut keys: Vec<Vec<u8>> = frozen
            .iter()
            .chain([&live])
            .flat_map(|index| {
                index
                    .mined_from(&tag, &tag)
                    .filter(|(_, row)| row.is_some())
                    .map(|(key, _)| key[1..].to_vec())
                    .collect::<Vec<_>>()
            })
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// The `mn_snapshot` row for `block`, from the evo layer or the store, decoded.
    pub fn snapshot_at(&self, block: &BlockHash) -> Option<Arc<dash_dml::MnList>> {
        use crate::evo::codec::EvoCf;
        use dashd_store::KvRead as _;
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        let (live, frozen) = disk.layer.lock().indexes();
        let bytes = live
            .get(EvoCf::MnSnapshot, block)
            .or_else(|| frozen.as_ref().and_then(|f| f.get(EvoCf::MnSnapshot, block)))
            .map(|b| b.to_vec())
            .or_else(|| {
                let key = EvoCf::MnSnapshot.key(block);
                disk.store.db().get(EvoCf::MnSnapshot.cf(), key.as_bytes()).expect("a read")
            })?;
        Some(disk.store.codec().decode_snapshot(&bytes, block).expect("a snapshot"))
    }

    /// Every row of the recorded rig's engine (every column family's), by family name, key and
    /// value, with the evo layer's committed rows as it holds them.
    pub fn stored(&self) -> (BTreeSet<StoredRow>, String) {
        use dashd_store::{Cf, KvEngine as _, KvRead as _};
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        let db = disk.store.db();
        let rows = (db.schema().cfs.iter().enumerate())
            .flat_map(|(i, spec)| {
                let cf = Cf::new(u16::try_from(i).expect("few families"));
                db.iter_prefix(cf, &[])
                    .map(|row| {
                        let (key, value) = row.expect("a read");
                        (spec.name, key, value)
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        (rows, format!("{:?}", disk.layer.lock().indexes()))
    }

    /// The engine's row at `key` in `cf`, past V's caches.
    pub fn row(&self, cf: dashd_store::Cf, key: &[u8]) -> Option<Vec<u8>> {
        use dashd_store::KvRead as _;
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        disk.store.db().get(cf, key).expect("a read")
    }

    /// Writes `value` at `key` in the engine's `cf`, under V's caches.
    pub fn put_row(&self, cf: dashd_store::Cf, key: &[u8], value: &[u8]) {
        use dashd_store::{Commit, KvBatch as _};
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        let db = disk.store.db();
        let mut batch = db.batch();
        batch.put(cf, key, value);
        db.write(batch, Commit::Durable).expect("the write");
    }

    /// Drops `block`'s inputs from the coins cache, so its connect reads them from the engine.
    pub fn uncache_inputs(&self, block: &Block) {
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        let mut cache = disk.coins.lock().expect("coins");
        for tx in &block.txs()[1..] {
            for input in tx.inputs() {
                cache.uncache(&crate::coins::OutPointKey::new(&input.prevout()));
            }
        }
    }

    /// Whether the coins cache holds each of `outpoints`, and its usage: what a read that must
    /// leave the cache alone is checked against.
    pub fn coins_cache(
        &self,
        outpoints: &[dash_primitives::OutPoint],
    ) -> (Vec<bool>, crate::coins::Usage) {
        let disk = self.disk.as_ref().expect("a recorded rig's stores");
        let cache = disk.coins.lock().expect("coins");
        let held = outpoints
            .iter()
            .map(|op| cache.have_in_cache(&crate::coins::OutPointKey::new(op)))
            .collect();
        (held, cache.usage())
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
    let faults = Faults::default();
    let db = Arc::new(GuardedEngine::new(
        ReadFaults::new(&CHAINSTATE_SCHEMA, faults.clone()),
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
        faults,
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
    coins: ChainstateCell<ReadFaults>,
    files: Arc<GuardedFiles<OsFs>>,
    store: Arc<RocksEvoStore<ReadFaults>>,
    layer: SharedEvoLayer,
    faults: Faults,
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
            flush: Some(cfg.flush.unwrap_or_else(|| Box::new(Latched(self.latch.clone())))),
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

/// The next block's `merkleRootQuorums`, read
/// through [`EvoView::with_qc`](crate::chain::EvoView::with_qc) at V's tip under the next block's
/// scheme.
fn next_root_through_with_qc(rig: &Rig, tip: BlockId, next: &Mined) -> dash_primitives::MerkleRoot {
    let (p, block, height) = (rig.p.clone(), next.block.clone(), next.height);
    rig.v
        .view_handle()
        .query(move |q| {
            let bls = dash_consensus::ctx::bls_ctx_for_block(&p, height);
            q.evo_view()
                .with_qc(bls, |qc| {
                    let mut cache = dash_dml::blockprocessor::QcHashCache::new();
                    dash_dml::blockprocessor::quorum_root(
                        &qc.llmq, qc.mined, &mut cache, &block, tip,
                    )
                    .expect("reads")
                    .expect("a root")
                })
                .expect("evo state")
        })
        .recv()
        .expect("V serves")
}

/// MN0-2b-2 commit 1 (OD-CP-19 Q1): `with_qc` lends the template the block processor's reads at
/// V's tip. On dashd's masternode chain, at every tip from DIP0003 to 299, the quorum root it
/// computes for the next block is the one dashd mined into that block's CbTx. The chain mines
/// null commitments only; `mineable`'s `with_qc_reads_the_committed_mined_commitment` reads a
/// mined one.
#[test]
fn with_qc_reads_the_quorum_root_dashd_mined() {
    let (cfg, kept) = Source::Recorded.config(RigConfig::default());
    let rig = Rig::with(cfg);
    let main = linked(&rig.genesis, decode(MN_CHAIN));
    let mut checked = 0;
    for (m, next) in main.iter().zip(&main[1..]) {
        rig.accept(m);
        if next.height <= DIP3 {
            continue;
        }
        let want = cbtx_root(&next.block);
        let Some(want) = want else {
            continue;
        };
        let root = next_root_through_with_qc(&rig, rig.id(m), next);
        assert_eq!(root, want, "the root at {}", next.height);
        checked += 1;
    }
    assert!(checked > 200, "{checked} CbTx roots compared");
    kept.assert_no_abort(&rig);
}

/// setup-dashd's masternode network (`regtest-blockchain-v0.0.4` `regtest-mn`, recorded from its
/// controller by dashd v23.1.0): four masternodes and eight DKG cycles to 406, whose
/// `llmq_test` and `llmq_test_dip0024` commitments are signed from 227 and 228.
pub(super) const QC_CHAIN: &str = include_str!("../../../tests/data/mn0-qc/blocks.txt");

/// That network's parameters: `-dip3params=2:2 -testactivationheight=v20@100
/// -testactivationheight=mn_rr@100`.
pub(super) fn qc_chain_params() -> Arc<ChainSetup> {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "2:2")
        .arg(ChainOption::TestActivationHeight, "v20@100")
        .arg(ChainOption::TestActivationHeight, "mn_rr@100")
        .build()
        .expect("regtest");
    Arc::new(p)
}

/// OD-CP-60 (2): `with_qc`'s quorum root over a non-empty active set. On setup-dashd's
/// masternode network, whose CbTxs carry ChainLocks from 237, at every tip from the first signed
/// commitment to 405 the root `with_qc` computes for the next block is the one dashd mined into
/// its CbTx; every one of those blocks has a non-empty active set.
#[test]
fn with_qc_reads_a_non_empty_quorum_root_dashd_mined() {
    let p = qc_chain_params();
    let blocks = decode(QC_CHAIN);
    let last = blocks.iter().map(|b| i64::from(b.header().time())).max().expect("blocks");
    let after_genesis = last - i64::from(Mined::genesis(&p).time()) + 86_400;
    let cl = Box::new(AttemptClVerifier::default());
    let (cfg, kept) = node_config(
        p,
        after_genesis,
        RigConfig::default(),
        DmlChecks,
        cl,
        Box::new(NodeHelper),
        |b| b,
    );
    let rig = Rig::with(cfg);
    let main = linked(&rig.genesis, blocks);
    assert!(
        main.iter().any(|m| cbtx_carries_chainlock(&m.block)),
        "a CbTx ChainLock in the recording"
    );
    let first_signed = main
        .iter()
        .find(|m| m.block.txs().iter().skip(1).any(|tx| signed_commitment(tx)))
        .expect("a signed commitment in the recording")
        .height;
    let (mut checked, mut non_empty) = (0, 0);
    for (m, next) in main.iter().zip(&main[1..]) {
        rig.accept(m);
        if next.height <= first_signed {
            continue;
        }
        let want = cbtx_root(&next.block).expect("a CbTx with a quorum root");
        let root = next_root_through_with_qc(&rig, rig.id(m), next);
        assert_eq!(root, want, "the root at {}", next.height);
        checked += 1;
        non_empty += u32::from(active_commitments(&rig, rig.id(m), next.height) > 0);
    }
    assert!(checked > 0, "no block past the first signed commitment was checked");
    assert_eq!(non_empty, checked, "checked blocks with a non-empty active set");
    kept.assert_no_abort(&rig);
}

/// The active commitments `GetQcHashes` reads at `tip` for the block at `height`.
fn active_commitments(rig: &Rig, tip: BlockId, height: u32) -> usize {
    let p = rig.p.clone();
    rig.v
        .view_handle()
        .query(move |q| {
            let bls = dash_consensus::ctx::bls_ctx_for_block(&p, height);
            q.evo_view()
                .with_qc(bls, |qc| {
                    let mut cache = dash_dml::blockprocessor::QcHashCache::new();
                    let h =
                        dash_dml::blockprocessor::qc_hashes(&qc.llmq, qc.mined, &mut cache, tip)
                            .expect("reads");
                    h.plain.values().map(Vec::len).sum::<usize>()
                        + h.indexed.values().map(|v| v.len()).sum::<usize>()
                })
                .expect("evo state")
        })
        .recv()
        .expect("V serves")
}

/// Whether `block`'s CbTx carries a non-null best ChainLock.
fn cbtx_carries_chainlock(block: &Block) -> bool {
    let Some(tx) = block.txs().first() else {
        return false;
    };
    matches!(
        tx.payload(),
        Some(Ok(d)) if matches!(
            d.payload(),
            dash_primitives::SpecialPayload::CoinbaseCommitment(c)
                if c.best_cl_signature().as_ref().is_some_and(|s| s.to_bytes() != [0; 96])
        )
    )
}

/// Whether `tx` carries a commitment with signers.
fn signed_commitment(tx: &dash_consensus::primitives::Tx) -> bool {
    matches!(
        tx.payload(),
        Some(Ok(d)) if matches!(
            d.payload(),
            dash_primitives::SpecialPayload::QuorumCommitment(p) if !p.commitment().is_null()
        )
    )
}

/// The CbTx's `merkleRootQuorums`, for a CbTx of version 2 or more.
fn cbtx_root(block: &Block) -> Option<dash_primitives::MerkleRoot> {
    let tx = block.txs().first()?;
    match &tx.payload()?.ok()?.payload() {
        dash_primitives::SpecialPayload::CoinbaseCommitment(c) => c.merkle_root_quorums(),
        _ => None,
    }
}

/// `with_qc`'s members are the template's (MN0-3 C0): over a node with the members facade they
/// answer and emit no effect. The assembler never precomputes, so its precompute refuses
/// (OD-CP-134 nit).
#[test]
fn with_qc_computes_members_emitting_nothing_and_refuses_a_precompute() {
    let (cfg, kept) = Source::Recorded.config(RigConfig::default());
    let rig = Rig::with(cfg);
    let main = linked(&rig.genesis, decode(MN_CHAIN));
    for m in &main[..120] {
        rig.accept(m);
    }
    let (p, tip) = (rig.p.clone(), rig.id(&main[119]));
    let got = rig
        .v
        .view_handle()
        .query(move |q| {
            let bls = dash_consensus::ctx::bls_ctx_for_block(&p, 121);
            q.evo_view()
                .with_qc(bls, |qc| {
                    let mut sink = Vec::new();
                    let members =
                        qc.members.members(dash_params::LlmqType::LlmqTest, tip, &mut sink);
                    let pre = qc.precompute.precompute(
                        tip,
                        dash_evo::llmq::PrecomputeMode::Connect,
                        &mut sink,
                    );
                    (format!("{:?}", members.map(|m| m.len())), format!("{pre:?}"), sink.is_empty())
                })
                .expect("evo state")
        })
        .recv()
        .expect("V serves");
    assert!(got.0.starts_with("Ok(") && got.1.contains("NotBuilt") && got.2, "{got:?}");
    kept.assert_no_abort(&rig);
}

/// Without evo state (`NoEvoReads`), `with_qc` is the readiness refusal and `f` is not called.
#[test]
fn with_qc_without_evo_state_is_the_readiness_refusal() {
    let (rig, _, _kept) = Source::Synthetic.rig(RigConfig::default());
    let p = rig.p.clone();
    let (called, err) = rig
        .v
        .view_handle()
        .query(move |q| {
            let mut called = false;
            let bls = dash_consensus::ctx::bls_ctx_for_block(&p, 1);
            let got = q.evo_view().with_qc(bls, |_| called = true);
            (called, got.err().map(|e| format!("{e:?}")))
        })
        .recv()
        .expect("V serves");
    assert!(!called, "f was called");
    assert!(err.is_some_and(|e| e.contains("NotBuilt")), "a readiness refusal");
}

/// `feature_asset_locks.py`'s run: asset unlocks from 189; V20 at 100 and MN_RR at 560.
const ASSET_LOCKS: &str = "feature_asset_locks";

/// `OP_TRUE`, the scripts of the render tests' own outputs.
const OP_TRUE: u8 = 0x51;

/// `OP_RETURN`, the platform share's script.
const OP_RETURN: u8 = 0x6a;

/// [`masternode_payments`] of `block`, whose undo is `undo`, in one V query on `rig`.
fn payments_of(rig: &Rig, block: &Arc<Block>, undo: BlockUndo) -> BlockPayments {
    let (p, block) = (rig.p.clone(), Arc::clone(block));
    let rendered = rig.v.view_handle().query(move |q| {
        let params = RenderParams::new(&p).expect("the render's parameters");
        masternode_payments(q, &block, &undo, &params).map_err(|e| e.to_string())
    });
    rendered.recv().expect("V serves").expect("an entry")
}

/// [`payments_at_fees`] of `block` at no fees, in one V query on `rig`.
fn unpaid_of(rig: &Rig, block: &Arc<Block>) -> BlockPayments {
    let (p, block) = (rig.p.clone(), Arc::clone(block));
    let rendered = rig.v.view_handle().query(move |q| {
        let params = RenderParams::new(&p).expect("the render's parameters");
        payments_at_fees(q, &block, 0, &params).map_err(|e| e.to_string())
    });
    rendered.recv().expect("V serves").expect("an entry")
}

/// Each block of `main`, rendered, against dashd's coinbase: its payees, and its subsidy and
/// fees, all of which dashd's miner claims. How many blocks' fees move their payment, and the
/// masternodes paid.
fn assert_dashd_paid(rig: &Rig, kept: &Kept, main: &[Mined]) -> (usize, BTreeSet<ProTxHash>) {
    let (mut moved, mut paid) = (0, BTreeSet::new());
    for m in main {
        let got = payments_of(rig, &m.block, kept.undo(rig, m));
        assert_eq!(got.payees, dashd_payees(&m.block), "{}", m.height);
        let claimed = m.block.txs()[0].value_out();
        assert_eq!(got.subsidy + got.fees, claimed, "{}: what dashd's miner claimed", m.height);
        moved += usize::from(unpaid_of(rig, &m.block).amount() != got.amount());
        paid.extend(got.pro_tx_hash);
    }
    (moved, paid)
}

/// What dashd's coinbase pays after its miner: `FillBlockPayments`' masternode outputs, built on
/// the block's whole fees, which dashd's miner claims (these chains trigger no superblock).
// Core: node/miner.cpp:272-275, :338; masternode/payments.cpp:456-460
fn dashd_payees(block: &Block) -> &[TxOut] {
    &block.txs()[0].outputs()[1..]
}

/// A version-2 transaction spending `inputs` to `outputs`, each to `OP_TRUE`.
fn spend(inputs: Vec<OutPoint>, outputs: &[Amount]) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        inputs.into_iter().map(|prevout| TxIn::new(prevout, Vec::new().into(), u32::MAX)).collect(),
        outputs.iter().map(|&value| TxOut::from_duffs(value, vec![OP_TRUE].into())).collect(),
        0,
        Vec::new(),
    )))
}

/// CH7b-3 C9's render before the first registration (PLAN C9): on dashd's masternode chain each
/// block to 116, the EvoNode's, pays from a parent's list with no masternode, so its entry has no
/// payee and no proTxHash, and dashd's coinbase pays its miner alone.
#[test]
fn payments_render_is_empty_without_a_payee() {
    let (cfg, kept) = Source::Recorded.config(RigConfig::default());
    let rig = Rig::with(cfg);
    let main = linked(&rig.genesis, decode(MN_CHAIN));
    for m in &main[..116] {
        rig.accept(m);
    }
    for m in &main[..116] {
        let got = payments_of(&rig, &m.block, kept.undo(&rig, m));
        assert_eq!((got.height, got.block_hash), (m.height, m.hash()));
        assert_eq!(got.pro_tx_hash, None, "{}", m.height);
        assert_eq!(got.payees, [], "{}", m.height);
        assert_eq!(dashd_payees(&m.block), [], "{}: dashd's coinbase", m.height);
    }
    kept.assert_no_abort(&rig);
}

/// CH7b-3 C9's render recomputes the fees as Core's RPC does (PLAN C9, P-1, P-2). On dashd's
/// masternode chain from 117, the first block a masternode is paid in, to 300, each block's
/// payees are what dashd's coinbase paid: on the fees its undo gives, the ProTxs' among them,
/// from its parent's list, as the payee moves between the masternodes. Its subsidy and fees are
/// what the coinbase claims, and some block's fees move its payment. Then a block at 125 with
/// one transaction paying F in fees, under a coinbase claiming its subsidy plus F − U: its
/// payment is `GetMasternodePayment` of the subsidy plus F, not of what the coinbase claims. At
/// 125 that is Classic's 52.6%: regtest's BRR from 1 and superblock cycle 20 start the
/// reallocation at 20, a period each 60 blocks, and V20 is regtest's 432.
// Core: rpc/masternode.cpp:431-459; masternode/payments.cpp:62-128 GetMasternodePayment;
// chainparams.cpp:802-803, :811, :823, :828, :833 (regtest)
#[test]
fn payments_render_recomputes_fees_when_the_coinbase_underclaims() {
    const H: u32 = 125;
    const F: Amount = 1_000_000;
    const U: Amount = 600_000;
    const SPENT: Amount = 500_000_000;
    let (cfg, kept) = Source::Recorded.config(RigConfig::default());
    let rig = Rig::with(cfg);
    let main = linked(&rig.genesis, decode(MN_CHAIN));
    for m in &main {
        rig.accept(m);
    }
    let (moved, paid) = assert_dashd_paid(&rig, &kept, &main[116..]);
    assert!(paid.len() >= 2, "the payee moves");
    assert!(moved > 0, "a block whose fees move its payment");

    let real = &main[H as usize - 1];
    assert_eq!(real.block.txs().len(), 1, "{H}: a coinbase alone");
    // dashd's coinbase claims the subsidy and fees of none.
    let subsidy = real.block.txs()[0].value_out();
    let classic = |value: Amount| value * 526 / 1000;
    assert_ne!(classic(subsidy + F), classic(subsidy + F - U), "the claim moves the payment");
    let paid = payments_of(&rig, &real.block, kept.undo(&rig, real));
    assert_eq!(paid.amount(), classic(subsidy), "{H}: dashd's payment");

    let null = OutPoint::new(TxHash::from([0; 32]), u32::MAX);
    let spent = OutPoint::new(TxHash::from([7; 32]), 0);
    let txs = vec![spend(vec![null], &[subsidy + F - U]), spend(vec![spent], &[SPENT - F])];
    let header = {
        let base = BlockHeader::default();
        BlockHeader::new(
            base.version(),
            real.block.header().prev_hash(),
            base.merkle_root(),
            base.time(),
            base.bits(),
            base.nonce(),
        )
    };
    let block = Arc::new(Block::new(Header::new(header), txs));
    let undo = BlockUndo {
        txs: vec![TxUndo {
            coins: vec![Coin::new(SPENT, &[OP_TRUE], 120, false)],
        }],
    };
    let got = payments_of(&rig, &block, undo);
    assert_eq!(got.amount(), classic(subsidy + F), "the masternode's amount");
    assert_eq!((got.height, got.fees), (H, F));
    assert_eq!(got.pro_tx_hash, paid.pro_tx_hash, "the parent's payee");
    let scripts = |p: &BlockPayments| -> Vec<Vec<u8>> {
        p.payees.iter().map(|out| out.script_pubkey().as_bytes().to_vec()).collect()
    };
    assert_eq!(scripts(&got), scripts(&paid));
    kept.assert_no_abort(&rig);
}

/// CH7b-3 C9's render takes an asset unlock's fee from its payload (PLAN C9, P-3): the unlock
/// spends no coin, so its inputs less its outputs are not its fee. On dashd's recorded
/// `feature_asset_locks` run each block to 189 renders dashd's payees, its subsidy and fees what
/// the coinbase claims, across V20 at 100. Block 189, the first with an unlock, renders the fees
/// the test decodes: each unlock's payload fee (`CAssetUnlockPayload`'s `fee`, after its version
/// and index), and each other transaction's inputs, found in the run's blocks, less its outputs.
/// Its payment is dashd's, three quarters of the coinbase's subsidy and fees after V20, and the
/// unlocks' fees as their inputs less their outputs would move it.
// Core: rpc/masternode.cpp:438-443; evo/assetlocktx.h:89-117; masternode/payments.cpp:95-100
#[test]
fn payments_render_takes_an_asset_unlocks_fee_from_its_payload() {
    const H: usize = 189;
    let raw = run_blocks(ASSET_LOCKS);
    let time = |b: &Block| i64::from(b.header().raw().time());
    let after_genesis = time(&raw[H]) - time(&raw[0]) + 86_400;
    let cl = Box::new(AttemptClVerifier::default());
    let (cfg, kept) = node_config(
        run_params(ASSET_LOCKS),
        after_genesis,
        RigConfig::default(),
        DmlChecks,
        cl,
        Box::new(NodeHelper),
        |blocks| blocks,
    );
    let rig = Rig::with(cfg);
    assert_eq!(rig.genesis.hash(), raw[0].hash(), "the run's genesis");
    let main = linked(&rig.genesis, raw[1..=H].to_vec());
    for m in &main {
        rig.accept(m);
    }
    assert_dashd_paid(&rig, &kept, &main);
    let unlocks = |b: &Block| b.txs().iter().filter(|tx| tx.is_platform_transfer()).count();
    assert!(raw[..H].iter().all(|b| unlocks(b) == 0), "an unlock before {H}");
    let block = &raw[H];
    assert!(unlocks(block) > 0, "{H}: an unlock");

    let by_id: HashMap<_, _> =
        raw[..=H].iter().flat_map(|b| b.txs()).map(|tx| (tx.txid(), Arc::clone(tx))).collect();
    let (mut want, mut unlocked) = (0, 0);
    for tx in &block.txs()[1..] {
        if tx.is_platform_transfer() {
            let fee = u32::from_le_bytes(tx.extra_payload()[9..13].try_into().expect("a fee"));
            want += Amount::from(fee);
            unlocked += Amount::from(fee) + tx.value_out();
        } else {
            let value_in: Amount = tx
                .inputs()
                .iter()
                .map(|i| by_id[&i.prevout().txid()].outputs()[i.prevout().index() as usize].value())
                .sum();
            want += value_in - tx.value_out();
        }
    }
    let value = block.txs()[0].value_out();
    assert_ne!((value - unlocked) * 3 / 4, value * 3 / 4, "the unlocks' fees move the payment");
    let got = payments_of(&rig, &main[H - 1].block, kept.undo(&rig, &main[H - 1]));
    assert_eq!(got.amount(), value * 3 / 4, "the masternode's amount");
    assert_eq!(got.payees, dashd_payees(block), "dashd's payees");
    assert_eq!((got.height, got.fees), (H as u32, want));
    kept.assert_no_abort(&rig);
}

/// CH7b-3 C9's render pays in the reward era after the parent (PLAN C9, P-4). On dashd's recorded
/// `feature_llmq_chainlocks` run, where V20 and MN_RR activate at 200, each block to 210 renders
/// dashd's payees, its subsidy and fees what the coinbase claims. Block 200 pays the platform
/// share first, then a masternode, and 199 a masternode alone: 200's era is the one after its
/// parent, EvoReward, not its parent's own.
// Core: rpc/masternode.cpp:465; masternode/payments.cpp:142-150; deploymentstatus.h:14-18
#[test]
fn payments_render_pays_in_the_era_after_the_parent() {
    const H: usize = 210;
    const ACTIVE: usize = 200;
    let raw = run_blocks(CHAINLOCKS);
    let time = |b: &Block| i64::from(b.header().raw().time());
    let after_genesis = time(&raw[H]) - time(&raw[0]) + 86_400;
    let (cfg, kept) = node_config(
        run_params(CHAINLOCKS),
        after_genesis,
        RigConfig::default(),
        DmlChecks,
        Box::new(AttemptClVerifier::default()),
        Box::new(NodeHelper),
        |blocks| blocks,
    );
    let rig = Rig::with(cfg);
    assert_eq!(rig.genesis.hash(), raw[0].hash(), "the run's genesis");
    let main = linked(&rig.genesis, raw[1..=H].to_vec());
    for m in &main {
        rig.accept(m);
    }
    let platform = |b: &Block| {
        let first = dashd_payees(b).first();
        first.is_some_and(|out| out.script_pubkey().as_bytes() == [OP_RETURN])
    };
    let (at, before) = (&main[ACTIVE - 1].block, &main[ACTIVE - 2].block);
    assert!(platform(at) && dashd_payees(at).len() >= 2, "{ACTIVE}: the share and a masternode");
    assert!(!platform(before) && !dashd_payees(before).is_empty(), "{}: a masternode", ACTIVE - 1);
    assert_dashd_paid(&rig, &kept, &main);
    kept.assert_no_abort(&rig);
}
