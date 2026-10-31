//! The `CoreConnector` leg of the digest replay (CH3a a7-1; CH3 §7.2): blocks through V and
//! CH3a's [`CoreConnector`], logged by its [`DigestLog`] sink. [`super::replay`] is O9a-2's leg
//! around the null connector; this one connects each block for real, so its log carries the
//! derived and golden keys (O8 §5.2, §5.3) as well as the header keys.
//!
//! - **Connector:** `CoreConnector` with [`NoEvo`] hooks, whose readiness guard G1 refuses the
//!   first evo rule (so only a chain below DIP3 replays) and whose H4 refuses a special-version
//!   CbTx (CH7 D57), the caller's helper, `-par=1`, and `-assumevalid` as the caller gives it.
//!   - A recorded node ([`replay_node`]) answers from its decisions: [`RecordedHelper`] under the
//!     caller's [`Policy`], each tip attempt keyed to the one behind the block's final tip `C`
//!     ([`AttemptChoice::FinalConnect`]). A decision miss stops the replay with its text.
//!   - A block window without decisions ([`replay_window`]) uses [`IbdHelper`].
//! - **Stores** (CH3a D142): real ones, wired as `dashd-node`'s `Chainstate::attach_parts` wires
//!   them. The coins are a `CoinsCache` over a guarded [`MemoryEngine`]. One [`GuardedFiles`]
//!   under the stores' latch holds V's block files ([`NodeBlockFiles`]) and the connector's undo
//!   files, in the directory the caller gives. V never flushes ([`NoFlush`]): a replay's coins
//!   fit the cache.
//! - **Input and clock:** as [`super::replay`]: genesis first and only there, streamed one block
//!   at a time (CH3a D148), and for a recorded node the mock clock set to `A.t` of each block's
//!   final connect. A node's `blocks.dat` is checked by the streaming pre-pass before V starts.
//! - **Start** ([`Start`], CH3a D165): genesis, or for L2's suffix check a pruned node at a base,
//!   from the stored index of the base's chain without block data and the base's coins. The
//!   connector's sink is made once both are loaded ([`MakeSink`]), so a seeded log can check the
//!   coins before V starts.
//! - **Result:** every block is accepted. V stops without an `AbortNode`, and neither V's
//!   fail-stop latch nor the stores' has tripped. No decision missed. V's tip is the last block
//!   at the height a linear chain gives it (for a node, the summary's tip and the golden's last
//!   block). Otherwise the output is removed, as [`super::replay`] removes it.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use dash_chain::{BlockRef, ChainConfig, HeaderChain, Services, StaticSporks};
use dash_clock::MockClock;
use dash_consensus::Block;
use dash_consensus::coins::Coin;
use dash_consensus::header::HeaderParams;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dash_statedigest::decisions::Policy;
use dash_statedigest::{Chain, FileKind, Hash256, Header, Writer};
use dashd_chainstate::chain::{
    AbortPath, ChainHandle, ChainView, CoreChecks, GuardScope, LoadedIndex, LocalShutdown, NoFlush,
    NoInstantSend, NoUi, NodeBlockIndex, NodeJoinHandle, NullMempool, ReadAheadSetup,
    RecordingAbort, SignalsThread, VExit, VParts, ValidationSignals, genesis_block, load_chain_tip,
    load_genesis_block, open_block_index, spawn,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{CoinSrc, CoinsBudget, CoinsCache, OutPointKey, Ratio, Salt};
use dashd_chainstate::connect::{
    AttemptChoice, ChainHelper, ChainstateCell, ConnectorParts, CoreConnector, DigestLog,
    DigestSink, IbdHelper, MissSlot, NoEvo, RecordedHelper,
};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles,
};
use dashd_store::MemoryEngine;
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};

use super::frames::{self, FileFrames, Shape};
use super::replay::{
    Latch, Replayed, create_new, display, load_corpus, remove_partial, replay_time,
};
use super::store::Node;

/// The coins cache's share: every replay's set fits it, so [`NoFlush`] never matters.
const COINS_SHARE: u64 = 256 << 20;

/// The mock clock for a block; `None` leaves it where it is.
pub(crate) type BlockClock = Box<dyn Fn(&Block) -> Option<i64>>;

/// What the leg runs with besides the blocks.
pub(crate) struct Leg {
    pub(crate) params: Arc<ChainSetup>,
    pub(crate) header: Header,
    pub(crate) helper: Box<dyn ChainHelper>,
    /// The helper's decision misses; [`IbdHelper`] has none.
    pub(crate) miss: MissSlot,
    pub(crate) assume_valid: Option<BlockHash>,
    /// The mock clock at start.
    pub(crate) start: i64,
    pub(crate) clock: BlockClock,
    /// Where V must end: the height and block (a node's corpus says).
    pub(crate) want: Option<(i64, Hash256)>,
}

pub(crate) fn header(chain: Chain, run: String) -> Header {
    Header {
        file: FileKind::Digest,
        src: Writer::Rdashd,
        ver: format!("dash-conformance {} replay-core", env!("CARGO_PKG_VERSION")).into_bytes(),
        chain,
        run: run.into_bytes(),
    }
}

/// A stored node's `blocks.dat`, which the pre-pass checked, streamed again.
pub(crate) struct NodeBlocks {
    path: PathBuf,
    pub(crate) frames: Box<FileFrames>,
    pub(crate) shape: Shape,
}

impl NodeBlocks {
    /// The end of the pass, after it read `read` blocks: [`frames::finish_pass`].
    pub(crate) fn finish(self, read: usize) -> Result<()> {
        frames::finish_pass(&self.path, *self.frames, &self.shape, read)
    }
}

/// The leg's blocks, genesis first.
pub(crate) enum Input<'a> {
    Node(NodeBlocks),
    /// Blocks from a source that puts genesis first and nowhere else: checked as they come.
    Blocks(Box<dyn Iterator<Item = Result<Arc<Block>>> + 'a>),
}

/// Replays `node`'s `blocks.dat` through the `CoreConnector` leg with its recorded decisions
/// under `policy`, writing the log to `out`, a new file. `work` is a new directory for the
/// replay's block and undo files, which the caller removes.
pub fn replay_node(node: &Node, policy: Policy, out: &Path, work: &Path) -> Result<Replayed> {
    let (leg, blocks) = node_leg(node, policy)?;
    replay_into(leg, Input::Node(blocks), out, work)
}

/// `node`'s leg, with its recorded decisions under `policy` and the recorded clock, and its
/// `blocks.dat`, which the streaming pre-pass checked.
pub(crate) fn node_leg(node: &Node, policy: Policy) -> Result<(Leg, NodeBlocks)> {
    let corpus = load_corpus(node)?;
    let decisions = corpus.decisions.clone();
    let miss = MissSlot::new();
    let helper =
        RecordedHelper::new(corpus.decisions, policy, AttemptChoice::FinalConnect, miss.clone());
    let blocks = NodeBlocks {
        path: node.blocks(),
        frames: Box::new(frames::replay_pass(node, corpus.params.message_start)?),
        shape: corpus.shape,
    };
    let leg = Leg {
        header: header(Chain::Regtest, format!("replay-core-{}", node.label())),
        params: corpus.params,
        helper: Box::new(helper),
        miss,
        assume_valid: None,
        start: corpus.start,
        clock: Box::new(move |block| replay_time(&decisions, block)),
        want: Some(corpus.want),
    };
    Ok((leg, blocks))
}

/// Replays `blocks` (genesis first and nowhere else, streamed) on `params` through the
/// `CoreConnector` leg with [`IbdHelper`] and `-assumevalid=0`, writing a log of `chain` to
/// `out`, a new file. The mock clock stays at `time`. `work` is a new directory for the replay's
/// block and undo files, which the caller removes.
pub fn replay_window<'a>(
    params: Arc<ChainSetup>,
    chain: Chain,
    time: i64,
    blocks: impl IntoIterator<Item = Result<Arc<Block>>, IntoIter: 'a>,
    out: &Path,
    work: &Path,
) -> Result<Replayed> {
    // Genesis first is checked before V starts and the output is created.
    let mut blocks = blocks.into_iter();
    let first = blocks.next().transpose()?.context("no block: not even genesis")?;
    ensure!(first.hash() == genesis_block(&params).hash(), "block 0 is not genesis");
    let leg = Leg {
        header: header(chain, format!("replay-core-{}", chain.as_str())),
        start: time,
        params,
        helper: Box::new(IbdHelper),
        miss: MissSlot::new(),
        assume_valid: None,
        clock: Box::new(|_| None),
        want: None,
    };
    let blocks = std::iter::once(Ok(first)).chain(blocks);
    replay_into(leg, Input::Blocks(Box::new(blocks)), out, work)
}

/// V's block files on disk, in a new `0700` directory `blocks` under `work`, as
/// `Chainstate::attach_parts` wires them: one [`GuardedFiles`] under the stores' `latch`, and
/// [`NodeBlockFiles`] reporting through `path`. On disk, a replay's memory does not grow with the
/// blocks it stores (CH3a D148).
pub(crate) fn disk_block_files(
    p: &ChainSetup,
    work: &Path,
    latch: &GlobalFailStop,
    path: &AbortPath,
) -> Result<(Arc<GuardedFiles<OsFs>>, NodeBlockFiles<OsFs>)> {
    let blocks_dir = work.join("blocks");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&blocks_dir)
        .with_context(|| format!("creating {}", blocks_dir.display()))?;
    let config = BlockFilesConfig::new(blocks_dir, p.message_start);
    let files = BlockFiles::with_fs(Arc::new(OsFs::new()), config, BlockFilesState::default());
    let files = Arc::new(GuardedFiles::new(files, latch.clone()));
    let pow = *HeaderParams::from_chain_params(p).map_err(|e| anyhow!("{e:?}"))?.pow();
    let block_files = NodeBlockFiles::new(files.clone(), pow, Arc::new(path.clone()));
    Ok((files, block_files))
}

fn replay_into(leg: Leg, input: Input<'_>, out: &Path, work: &Path) -> Result<Replayed> {
    let file = create_new(out)?;
    run(leg, input, file, work).map_err(|e| remove_partial(out, e))
}

/// What V and its stores run with besides the leg: the a7-1 replay's settings, or L2's.
pub(crate) struct Settings {
    pub(crate) config: ChainConfig,
    /// `-checkblockindex` for V's block steps.
    pub(crate) check_block_index: bool,
    /// `-par`'s queue.
    pub(crate) queue: CheckQueue,
    /// The coins cache's share, in bytes.
    pub(crate) coins_share: u64,
    pub(crate) start: Start,
}

impl Settings {
    /// The a7-1 replay's: the node's defaults, `-checkblockindex`, `-par=1`, [`COINS_SHARE`]
    /// and genesis.
    fn replay(p: &ChainSetup) -> Settings {
        Settings {
            config: ChainConfig::node(p),
            check_block_index: true,
            queue: CheckQueue::serial(),
            coins_share: COINS_SHARE,
            start: Start::Genesis,
        }
    }
}

/// Where V starts (CH3a D165).
pub(crate) enum Start {
    /// A new chain: genesis is loaded, and init's ABC ([`feed`]'s) connects it.
    Genesis,
    /// A pruned node at a base: Core's state once `-prune` has removed every block file up to the
    /// base, which a restart loads with `m_have_pruned` set (`node/blockstorage.cpp:393`).
    Pruned(Pruned),
}

/// A pruned node's chainstate at its base.
pub(crate) struct Pruned {
    /// The block index records of the base's chain, heights `0..=base`, keyed by hash: each
    /// `VALID_SCRIPTS` (genesis's `VALID_TRANSACTIONS`) with its `nTx`, and neither data nor undo,
    /// as `PruneOneBlockFile` leaves them (`node/blockstorage.cpp:136-144`).
    pub(crate) records: Vec<(BlockHash, Vec<u8>)>,
    /// The base: the coins' best block and V's tip.
    pub(crate) base: BlockHash,
    /// The base's coins.
    pub(crate) coins: Vec<(OutPointKey, Coin)>,
}

/// Makes the connector's digest sink once V's chain and coins are loaded, given the coins and the
/// block they are at: `None` for genesis, whose coins are empty until init's ABC.
pub(crate) type MakeSink<'a> =
    Box<dyn FnOnce(Option<BlockRef>, &Coins) -> Result<Box<dyn DigestSink>> + 'a>;

/// The coins cache over the memory engine.
pub(crate) type Coins = ChainstateCell<MemoryEngine>;

/// V over the leg's stores, running: what the node wires (CH3a D142).
pub(crate) struct Rig {
    pub(crate) v: ChainHandle,
    join: NodeJoinHandle<VExit>,
    signals_thread: SignalsThread,
    pub(crate) clock: Arc<MockClock>,
    /// V's fail-stop latch.
    v_latch: Arc<Latch>,
    /// The stores' latch.
    latch: GlobalFailStop,
    abort: Arc<RecordingAbort>,
    pub(crate) coins: Coins,
}

impl Rig {
    /// Starts V on `p` with the clock at `start`, over new stores in `work` and a
    /// `CoreConnector` with [`NoEvo`] hooks, `helper`, `assume_valid` and the sink `sink` makes.
    /// From genesis, genesis is loaded and init's ABC, which connects it, is [`feed`]'s. From a
    /// pruned base, V's chain and coins are at the base before the sink is made.
    pub(crate) fn start(
        p: &Arc<ChainSetup>,
        helper: Box<dyn ChainHelper>,
        assume_valid: Option<BlockHash>,
        start: i64,
        sink: MakeSink<'_>,
        settings: Settings,
        work: &Path,
    ) -> Result<Rig> {
        let Settings {
            config,
            check_block_index,
            queue,
            coins_share,
            start: from,
        } = settings;
        // The stores, under one latch and one abort sink, as the node's.
        let latch = GlobalFailStop::new();
        let abort = Arc::new(RecordingAbort::default());
        let path = AbortPath::new(abort.clone(), latch.clone());
        let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
        let engine = Arc::new(GuardedEngine::new(engine, latch.clone(), abort.clone()));
        let budget = CoinsBudget::new(coins_share, 0, Ratio::HALF, 0);
        let coins =
            Arc::new(std::sync::Mutex::new(CoinsCache::new(engine, budget, Salt::random(), None)));
        let (files, mut block_files) = disk_block_files(p, work, &latch, &path)?;

        let clock = Arc::new(MockClock::at(start));
        let v_latch = Arc::new(Latch::default());
        let svc = Services {
            clock: clock.clone(),
            sporks: Arc::new(StaticSporks::for_network(p, Some(false))),
            fail: v_latch.clone(),
        };
        let (chain, idx, at) = match from {
            Start::Genesis => {
                let (chain, idx) = load_genesis_block(p.clone(), config, svc, &mut block_files)
                    .map_err(|e| anyhow!("loading genesis: {e:?}"))?;
                (chain, idx, None)
            }
            Start::Pruned(pruned) => {
                let (chain, idx, at) =
                    load_pruned(p, config, svc, &mut block_files, pruned, &coins)?;
                (chain, idx, Some(at))
            }
        };
        let have_pruned = at.is_some();
        let sink = sink(at, &coins)?;

        let parts = ConnectorParts {
            coins: coins.clone(),
            files,
            queue: Arc::new(queue),
            caches: Arc::new(ScriptCaches::new(CacheSizes::default()).context("script caches")?),
            helper,
            abort: path,
            assume_valid,
            digest: Some(sink),
            // L2 runs with `-consensusxcheck` (CH3 §6.2 L1, CH3a D189).
            xcheck: true,
        };
        let connector =
            CoreConnector::new(p, parts, NoEvo, Arc::new(())).map_err(|e| anyhow!("{}", e.0))?;

        let signals = ValidationSignals::default();
        let signals_thread = signals.start(abort.clone()).context("starting the signals thread")?;
        let (v, join) = spawn(VParts {
            params: p.clone(),
            chain,
            idx,
            checks: Box::new(CoreChecks::new(p.clone()).map_err(|e| anyhow!("{e:?}"))?),
            files: Box::new(block_files),
            // The seam reads every block on V (Q5).
            read_ahead: ReadAheadSetup::off(),
            connector: Box::new(connector),
            mempool: Box::new(NullMempool),
            flush: Box::new(NoFlush),
            is: Box::new(NoInstantSend),
            ui: Arc::new(NoUi),
            abort: abort.clone(),
            shutdown: Arc::new(LocalShutdown::default()),
            signals,
            clock: clock.clone(),
            check_block_index,
            have_pruned,
            stop_at_height: 0,
            guard: GuardScope::of(p),
        })
        .context("starting V")?;
        Ok(Rig {
            v,
            join,
            signals_thread,
            clock,
            v_latch,
            latch,
            abort,
            coins,
        })
    }

    /// Stops V and the signals thread. A decision miss in `miss` comes first, as the reason the
    /// attempt failed and so V stopped; then an `AbortNode`, V's latch and the stores' latch.
    pub(crate) fn stop(self, miss: &MissSlot) -> Result<()> {
        let Rig {
            v,
            join,
            signals_thread,
            v_latch,
            latch,
            abort,
            ..
        } = self;
        v.shutdown();
        let exit = join
            .join()
            .map_err(|e| anyhow!("joining V: {e:?}"))?
            .map_err(|_| anyhow!("V panicked"))?;
        let _ = signals_thread.stop();
        if let Some(text) = miss.get() {
            bail!("the replay stopped: {text}");
        }
        if let Some(fatal) = exit.fatal {
            bail!("V stopped on an AbortNode: {}", fatal.0);
        }
        ensure!(abort.messages().is_empty(), "AbortNode: {:?}", abort.messages());
        if let Some(what) = v_latch.reason() {
            bail!("V's fail-stop latch tripped: {what}");
        }
        if let Some(failure) = latch.tripped() {
            bail!("the stores' latch tripped: {failure:?}");
        }
        Ok(())
    }
}

/// Loads `pruned`: its coins into `coins` at its base, and its records as ND1's step 7 opens a
/// stored index ([`open_block_index`]), then the base as the chain tip ([`load_chain_tip`]).
fn load_pruned(
    p: &Arc<ChainSetup>,
    config: ChainConfig,
    svc: Services,
    files: &mut NodeBlockFiles<OsFs>,
    pruned: Pruned,
    coins: &Coins,
) -> Result<(HeaderChain, NodeBlockIndex, BlockRef)> {
    let Pruned {
        records,
        base,
        coins: set,
    } = pruned;
    // ND1's pool for the load (dashd-node `LOAD_THREADS`).
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().context("the load pool")?;
    let found = LoadedIndex::Stored {
        cfg: config,
        svc,
        records: Box::new(records.into_iter().map(Ok)),
        pool: &pool,
    };
    let shutdown = LocalShutdown::default();
    let (mut chain, mut idx) = open_block_index(p.clone(), found, files, &shutdown, false)
        .map_err(|e| anyhow!("opening the pruned index: {e:?}"))?;
    let reader = chain.reader();
    let id = reader.lookup(&base).context("the base is not in the pruned index")?;
    let height = reader.entry(id).context("the base has no entry")?.height;
    load_chain_tip(&mut chain, &mut idx, id).map_err(|e| anyhow!("LoadChainTip: {}", e.0))?;
    let at = BlockRef {
        id,
        hash: base,
        height,
    };
    let mut cache = coins.lock().map_err(|_| anyhow!("the coins lock is poisoned"))?;
    for (k, coin) in &set {
        cache
            .add(k, CoinSrc::Bytes(coin.as_ref()), false)
            .map_err(|e| anyhow!("adding the coin {:?}: {e:?}", k.outpoint()))?;
    }
    cache.set_best_block(Some(at));
    Ok((chain, idx, at))
}

/// A step of [`feed`]'s, which its caller can time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// A block is about to be sent to V.
    Send,
    /// V replied.
    Reply,
}

/// The block a feed starts at, which V's chain holds already: genesis, or a pruned start's base.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FeedStart {
    pub(crate) hash: BlockHash,
    pub(crate) height: u32,
}

impl FeedStart {
    pub(crate) fn genesis(p: &ChainSetup) -> FeedStart {
        FeedStart {
            hash: genesis_block(p).hash(),
            height: 0,
        }
    }
}

/// Init's ABC, then `blocks` one at a time but the first (`first`, which V holds: genesis, which
/// init connected, or the base; a stored node's pre-pass checked it, and a source's `blocks` are
/// checked here), each accepted and valid, with the clock set by `clock` first. After each reply,
/// `miss` stops the feed with its text. `on` sees each send and reply. What was read and handed,
/// and the last block.
pub(crate) fn feed(
    rig: &Rig,
    blocks: &mut dyn Iterator<Item = Result<Arc<Block>>>,
    first: FeedStart,
    clock: &dyn Fn(&Block) -> Option<i64>,
    miss: &MissSlot,
    mut on: impl FnMut(Step),
) -> Fed {
    let mut fed = Fed {
        read: 0,
        handed: Ok(0),
        last: first.hash,
    };
    let name = if first.height == 0 {
        "genesis"
    } else {
        "the base"
    };
    let result = (|| -> Result<usize> {
        rig.v
            .activate_best_chain()
            .recv()
            .map_err(|e| anyhow!("init's ABC: {e:?}"))?
            .map_err(|state| anyhow!("init's ABC: {state}"))?;
        let mut handed = 0;
        for (i, block) in blocks.enumerate() {
            let block = block?;
            fed.read += 1;
            let h = first.height as usize + i;
            if (i == 0) != (block.hash() == first.hash) {
                match i {
                    0 => bail!("block {h} is not {name}"),
                    _ => bail!("block {h} is {name} again"),
                }
            }
            if i == 0 {
                continue;
            }
            if let Some(t) = clock(&block) {
                rig.clock.set_mocktime(t);
            }
            on(Step::Send);
            let r = rig
                .v
                .process_block(block.clone(), true, None, true)
                .recv()
                .map_err(|e| anyhow!("block {h}: {e:?}"))?;
            on(Step::Reply);
            ensure!(
                r.accepted && r.state.is_valid(),
                "block {h} ({}) was not accepted: {:?}",
                block.hash(),
                r.state
            );
            // A miss found as an attempt ended stops the replay before the next block.
            if let Some(text) = miss.get() {
                bail!("block {h}: {text}");
            }
            fed.last = block.hash();
            handed += 1;
        }
        Ok(handed)
    })();
    fed.handed = result;
    fed
}

/// What [`feed`] did.
pub(crate) struct Fed {
    /// Blocks read, genesis included.
    pub(crate) read: usize,
    /// Blocks handed to V, or why the feed stopped.
    pub(crate) handed: Result<usize>,
    /// The last block handed (the first if none was).
    pub(crate) last: BlockHash,
}

/// V's tip is `last` at `height`: from genesis, the number of blocks handed after it.
pub(crate) fn check_tip(
    view: &ChainView,
    last: BlockHash,
    height: usize,
) -> Result<(u32, Hash256)> {
    let tip = &view.snap.tip;
    ensure!(
        tip.tip_hash == last && tip.tip_height as usize == height,
        "V's tip is {} at {}, not the last block {last} at {height}",
        tip.tip_hash,
        tip.tip_height
    );
    Ok((tip.tip_height, display(tip.tip_hash)))
}

fn run(leg: Leg, mut input: Input<'_>, file: std::fs::File, work: &Path) -> Result<Replayed> {
    let Leg {
        params: p,
        header,
        helper,
        miss,
        assume_valid,
        start,
        clock,
        want,
    } = leg;
    let sink = DigestLog::new(file, header).map_err(|e| anyhow!("{}", e.0))?;
    let settings = Settings::replay(&p);
    let sink: MakeSink<'_> = Box::new(|_, _| Ok(Box::new(sink)));
    let rig = Rig::start(&p, helper, assume_valid, start, sink, settings, work)?;
    let blocks: &mut dyn Iterator<Item = Result<Arc<Block>>> = match &mut input {
        Input::Node(node) => &mut node.frames,
        Input::Blocks(blocks) => blocks,
    };
    let fed = feed(&rig, blocks, FeedStart::genesis(&p), &clock, &miss, |_| ());
    let view = rig.v.view();
    rig.stop(&miss)?;
    let handed = fed.handed?;
    if let Input::Node(node) = input {
        node.finish(fed.read)?;
    }

    let tip = check_tip(&view, fed.last, handed)?;
    if let Some((height, hash)) = want {
        ensure!(
            i64::from(tip.0) == height,
            "V's tip height {} is not extract.json's summary tip {height}",
            tip.0
        );
        ensure!(tip.1 == hash, "V's tip {} is not the golden's last block {hash}", tip.1);
    }
    Ok(Replayed {
        blocks: handed,
        tip,
    })
}
