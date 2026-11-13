//! The startup contract on a DEC-196 (2) start (CH1-node N4, D38): V holds every other command
//! until startup's ABC completes, so the node's startup must send that ABC without waiting on
//! anything V serves.

use std::sync::Mutex;
use std::sync::mpsc;

use dash_chain::{ChainPolicy, FlatFilePos, HeaderBatch, StaticSporks};
use dash_clock::MockClock;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{BlockHash, Header, Tx};
use dash_consensus::{Block, BlockId};
use dash_num::CompactTarget;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{BlockHeader, Transaction};
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::{
    FlushBell, FlushHost, FlushSite, FlushStateMode, LoadFailure, LoadedIndex, LocalShutdown, NoUi,
    Queued, RecordingAbort, SiteDecision, SyncState, ValidationInterface, genesis_block,
    load_chain_tip,
};

use super::*;
use crate::shutdown::{EXIT_FAILURE, ShutdownState};
use dashd_chainstate::chain::MemBlockFiles;

const REGTEST_BITS: u32 = 0x207f_ffff;

/// How long a startup may take before the test calls it hung.
const HUNG: Duration = Duration::from_secs(120);

pub(crate) fn regtest() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest"))
}

/// Core `CScript() << height` for heights 1–127 (`OP_1`…`OP_16`, then a one-byte push), then
/// `salt`, so siblings differ.
fn coinbase(height: u32, salt: u32) -> Arc<Tx> {
    let h = u8::try_from(height).expect("a one-byte height");
    let push: &[u8] = match h {
        1..=16 => &[0x50 + h],
        17..=127 => &[1, h],
        _ => panic!("height {height}"),
    };
    let script_sig = [push, &[4], &salt.to_le_bytes()].concat();
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
    Arc::new(Tx::new(Transaction::decode(&mut raw.as_slice()).expect("a coinbase")))
}

/// `n` valid blocks on `parent` (at `height`), each mined under regtest's limit.
pub(crate) fn branch(
    p: &ChainSetup,
    parent: &Block,
    height: u32,
    n: u32,
    salt: u32,
) -> Vec<Arc<Block>> {
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let mut out: Vec<Arc<Block>> = Vec::new();
    for h in height + 1..=height + n {
        let prev = out.last().map_or(parent.header(), |b| b.header());
        let txs = vec![coinbase(h, salt)];
        let mut raw = {
            let base = BlockHeader::default();
            BlockHeader::new(
                0x2000_0000,
                prev.hash(),
                base.merkle_root(),
                prev.raw().time() + 150,
                REGTEST_BITS,
                base.nonce(),
            )
        };
        raw.set_merkle_root(Block::new(Header::new(raw), txs.clone()).merkle().0);
        let header = (0..)
            .map(|nonce| {
                Header::new(BlockHeader::new(
                    raw.version(),
                    raw.prev_hash(),
                    raw.merkle_root(),
                    raw.time(),
                    raw.bits(),
                    nonce,
                ))
            })
            .find(|h| {
                check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), &pow)
            })
            .expect("a nonce");
        out.push(Arc::new(Block::new(header, txs)));
    }
    out
}

/// Block files that outlive a V, so the restarted one reads the first one's blocks.
#[derive(Clone, Default)]
pub(crate) struct SharedFiles(pub(crate) Arc<Mutex<MemBlockFiles>>);

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
}

/// Nothing reads headers back.
struct NoReads;

impl FailStop for NoReads {
    fn read_failed(&self, what: &str) {
        panic!("the latch tripped: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

pub(crate) fn id(v: &ChainHandle, block: &Block) -> BlockId {
    v.reader().lookup(&block.hash()).expect("indexed")
}

fn tip(v: &ChainHandle) -> BlockId {
    v.view().snap.tip.tip
}

/// The node's startup on `chain`: step 7's coins attach, then step 11's ABC and the wait for
/// genesis.
fn startup(chain: &mut Chain) {
    startup_with(chain, Box::new(dashd_chainstate::chain::NoFlush));
}

/// [`startup`] with `flush` at V's flush sites.
fn startup_with(chain: &mut Chain, flush: Box<dyn FlushSite>) {
    startup_over(chain, Box::new(dashd_chainstate::chain::NullConnector), flush);
}

/// [`startup_with`] attaching `connector`.
pub(crate) fn startup_over(
    chain: &mut Chain,
    connector: Box<dyn BlockConnector>,
    flush: Box<dyn FlushSite>,
) {
    let coins = dashd_chainstate::chain::AttachCoins {
        connector,
        flush,
        facade: None,
    };
    chain.attach_coins(coins).expect("V takes the coins");
    chain.import(None).expect("the loadblk thread");
    chain.wait_for_genesis(|| false);
}

/// Waits for startup's ABC to complete: the `loadblk` thread ends once it replies, after its
/// `SetLoading`. The wait for genesis ends earlier, at the first published tip, and a command
/// sent before the ABC completes can be served at one of its release points (§2.4), so neither
/// reads the state it leaves. Then a query, served after that `SetLoading`.
pub(crate) fn imported(chain: &mut Chain) {
    chain.import.take().expect("startup ran").join().expect("the loadblk thread");
    chain.handle().query(|_| ()).recv().expect("V answers");
}

/// `f` on its own thread, failing the test if it takes [`HUNG`] (the thread stays blocked).
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (send, outcome) = mpsc::channel();
    std::thread::spawn(move || send.send(f()).expect("the test waits"));
    outcome.recv_timeout(HUNG).expect("the node's startup completes")
}

/// The node's seams for V: `clock` and `abort`, no UI, a shutdown flag of V's own, no
/// `-stopatheight` and the network's `-checkblockindex`.
fn seams(p: &ChainSetup, clock: &Arc<dyn Clock>, abort: &Arc<RecordingAbort>) -> NodeSeams {
    NodeSeams {
        clock: Arc::clone(clock),
        sporks: Arc::new(StaticSporks::for_network(p, None)),
        abort: abort.clone(),
        ui: Arc::new(NoUi),
        shutdown: Arc::new(LocalShutdown::default()),
        stop_at_height: 0,
        read_ahead: ReadAheadSetup::off(),
        check_block_index: ChainPolicy::for_setup(p, false).default_consistency_checks,
    }
}

/// V's parts on a fresh genesis chain over `files`, with no UI.
pub(crate) fn fresh(
    p: &Arc<ChainSetup>,
    clock: &Arc<dyn Clock>,
    abort: &Arc<RecordingAbort>,
    files: Box<dyn BlockFiles>,
) -> VParts {
    fresh_ui(p, clock, abort, files, Arc::new(NoUi))
}

/// [`fresh`] with `ui` hearing V's `uiInterface` notifications.
fn fresh_ui(
    p: &Arc<ChainSetup>,
    clock: &Arc<dyn Clock>,
    abort: &Arc<RecordingAbort>,
    files: Box<dyn BlockFiles>,
    ui: Arc<dyn UiNotifications>,
) -> VParts {
    fresh_with(
        p,
        files,
        NodeSeams {
            ui,
            ..seams(p, clock, abort)
        },
    )
}

/// V's parts on a fresh genesis chain over `files`, with the node's `seams`.
fn fresh_with(p: &Arc<ChainSetup>, files: Box<dyn BlockFiles>, node: NodeSeams) -> VParts {
    let svc = Services {
        clock: Arc::clone(&node.clock),
        sporks: Arc::new(StaticSporks::for_network(p, None)),
        fail: Arc::new(NoReads),
    };
    let mut files = files;
    let (chain, idx) =
        chain::load_genesis_block(Arc::clone(p), ChainConfig::node(p), svc, files.as_mut())
            .expect("genesis");
    parts(Arc::clone(p), chain, idx, files, node).expect("parts")
}

/// The `ibd` of each `UpdatedBlockTip`.
#[derive(Default)]
struct TipIbd(Mutex<Vec<bool>>);

impl ValidationInterface for TipIbd {
    fn updated_block_tip(&self, _: BlockId, _: Option<BlockId>, ibd: bool) {
        self.0.lock().expect("ibd").push(ibd);
    }
}

/// DEC-196 (2) with a candidate ranked below the failed tip, so V holds commands (D38): the
/// node's startup still completes, its ABC reorgs to the valid block, and the command held
/// before it then runs.
#[test]
fn startup_sends_its_abc_first_after_a_tied_fallback() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let files = SharedFiles::default();

    // a1-a2 and b1-b2 on genesis, a's first so a2 outranks b2 once loaded (equal sequence ids,
    // the lower id wins); `invalidateblock a1` moves the tip to b2.
    let mut first =
        Chain::start(fresh(&p, &clock, &abort, Box::new(files.clone()))).expect("V starts");
    startup(&mut first);
    assert!(first.handle().view().snap.has_tip(), "init waited for genesis");
    imported(&mut first);
    assert_eq!(first.handle().tip_watch().get(), Some(genesis.hash()));
    let main = branch(&p, &genesis, 0, 2, 0);
    let side = branch(&p, &genesis, 0, 2, 1);
    for block in main.iter().chain(&side) {
        let r = first.handle().process_block(Arc::clone(block), true, None, true).recv();
        assert!(r.is_ok_and(|r| r.accepted), "block accepted");
    }
    let (a2, b2) = (id(first.handle(), &main[1]), id(first.handle(), &side[1]));
    assert!(a2 < b2);
    assert_eq!(first.handle().invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(first.handle()), b2);
    let VExit {
        mut chain,
        ..
    } = first.stop().expect("V stopped");
    drop(first);

    // The index was written after the invalidation and the coins before it, at a2.
    let mut idx = NodeBlockIndex::load(&chain);
    load_chain_tip(&mut chain, &mut idx, a2).expect("DEC-196 (2): V starts");
    let parts2 = parts(Arc::clone(&p), chain, idx, Box::new(files), seams(&p, &clock, &abort))
        .expect("parts");
    let (tip, held, fatal) = bounded(move || {
        let mut chain = Chain::start(parts2).expect("V starts");
        // Another caller's command before startup's ABC: held (D38).
        let held = chain.handle().query(|q| q.chain.private_tip().map(|(t, _)| t.id));
        chain.allow_unawaited_send();
        assert!(held.recv_timeout(Duration::from_millis(200)).is_none(), "V holds it");
        // The wait for genesis ends on the loaded tip; the held command runs after the ABC.
        startup(&mut chain);
        let held = held.recv();
        imported(&mut chain);
        let tip = tip(chain.handle());
        (tip, held, chain.stop().and_then(|exit| exit.fatal))
    });
    assert_eq!(tip, b2, "startup's ABC reorged to the valid block");
    assert_eq!(held, Ok(Some(b2)), "the held command ran after it");
    assert_eq!(fatal, None);
    assert_eq!(abort.messages(), Vec::<String>::new());
}

/// A restart on a loaded tip no block outranks: startup's ABC connects nothing, and the wait for
/// genesis ends on the loaded tip (Core's `fHaveGenesis` from `ActiveChain().Tip()`).
#[test]
fn startup_ends_on_a_loaded_best_tip() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let files = SharedFiles::default();
    let mut first =
        Chain::start(fresh(&p, &clock, &abort, Box::new(files.clone()))).expect("V starts");
    startup(&mut first);
    let a1 = branch(&p, &genesis, 0, 1, 0).remove(0);
    let r = first.handle().process_block(Arc::clone(&a1), true, None, true).recv();
    assert!(r.is_ok_and(|r| r.accepted), "block accepted");
    let a1 = id(first.handle(), &a1);
    let VExit {
        mut chain,
        ..
    } = first.stop().expect("V stopped");
    drop(first);

    let mut idx = NodeBlockIndex::load(&chain);
    load_chain_tip(&mut chain, &mut idx, a1).expect("V starts");
    let parts2 = parts(Arc::clone(&p), chain, idx, Box::new(files), seams(&p, &clock, &abort))
        .expect("parts");
    let (tip, fatal) = bounded(move || {
        let mut chain = Chain::start(parts2).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        (tip(chain.handle()), chain.stop().and_then(|exit| exit.fatal))
    });
    assert_eq!((tip, fatal), (a1, None));
    assert_eq!(abort.messages(), Vec::<String>::new());
}

/// V on a fresh regtest chain with step 7's coins attached, and its `AbortNode` sink.
fn attached_fresh() -> (Chain, Arc<RecordingAbort>) {
    let p = regtest();
    let genesis = genesis_block(&p);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let mut chain = Chain::start(fresh(&p, &clock, &abort, Box::new(SharedFiles::default())))
        .expect("V starts");
    let coins = dashd_chainstate::chain::AttachCoins {
        connector: Box::new(dashd_chainstate::chain::NullConnector),
        flush: Box::new(dashd_chainstate::chain::NoFlush),
        facade: None,
    };
    chain.attach_coins(coins).expect("V takes the coins");
    (chain, abort)
}

/// DEC-1381's pin: a command sent V before startup's ABC and not awaited, here through a clone
/// of V's handle, fails [`Chain::import`]'s check before startup's `SetLoading` is sent.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "a command sent V before startup's ABC was not awaited")]
fn an_unawaited_command_before_startups_abc_fails_the_pin() {
    let (mut chain, _abort) = attached_fresh();
    let other = chain.handle().clone();
    let _unawaited = other.query(|_| ());
    let _ = chain.import(None);
}

/// The ChainLock handler's 5 s task, armed at step 10a and never awaited, passes
/// [`Chain::import`]'s check (DEC-1381, OD-CH3-30), and startup completes.
#[test]
fn the_chainlock_timer_before_startups_abc_passes_the_pin() {
    let (fatal, messages) = bounded(|| {
        let (mut chain, abort) = attached_fresh();
        chain.handle().chainlock_timer().expect("V takes the timer's command");
        chain.import(None).expect("the loadblk thread");
        chain.wait_for_genesis(|| false);
        imported(&mut chain);
        (chain.stop().and_then(|exit| exit.fatal), abort.messages())
    });
    assert_eq!(fatal, None);
    assert_eq!(messages, Vec::<String>::new());
}

fn devnet() -> Arc<ChainSetup> {
    Arc::new(
        ChainParamsBuilder::new(Network::Devnet, Release::V24_0)
            .devnet_name("nd1")
            .build()
            .expect("devnet"),
    )
}

/// A devnet's step 7 accepts its genesis block (height 1) as V's first act, and startup ends
/// on it.
#[test]
fn a_devnet_starts_on_its_genesis_block() {
    let p = devnet();
    let dg =
        dash_consensus::params::devnet_genesis_block(&p).expect("a devnet genesis block").header();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(i64::from(dg.time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let parts = open_parts(
        Arc::clone(&p),
        seams(&p, &clock, &abort),
        std::iter::empty(),
        mem_files(),
        false,
        Reindex::default(),
    )
    .expect("a fresh index");
    let (accepted, tip, height, fatal) = bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        // Accepted before `start` returned, and not connected: that is step 11's ABC.
        let accepted = chain.handle().reader().lookup(&Header::new(dg).hash()).is_some();
        assert!(!chain.handle().view().snap.has_tip());
        startup(&mut chain);
        // The wait ends on genesis's publication, one step before the devnet block's.
        imported(&mut chain);
        let view = chain.handle().view();
        let tip = chain.handle().tip_watch().get();
        (accepted, tip, view.snap.tip.tip_height, chain.stop().and_then(|exit| exit.fatal))
    });
    assert!(accepted, "V accepted the devnet genesis block first");
    assert_eq!((tip, height, fatal), (Some(Header::new(dg).hash()), 1, None));
    assert_eq!(abort.messages(), Vec::<String>::new());
}

/// A devnet genesis block `AcceptBlock` rejects (here from the future, `time-too-new`) is
/// Core's init error from `LoadGenesisBlock`, with no `AbortNode`.
// Core: validation.cpp:5419-5421, node/chainstate.cpp:210-211
#[test]
fn a_rejected_devnet_genesis_block_fails_init() {
    let p = devnet();
    let dg =
        dash_consensus::params::devnet_genesis_block(&p).expect("a devnet genesis block").header();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(i64::from(dg.time()) - 3 * 3_600));
    let abort = Arc::new(RecordingAbort::default());
    let parts = open_parts(
        Arc::clone(&p),
        seams(&p, &clock, &abort),
        std::iter::empty(),
        mem_files(),
        false,
        Reindex::default(),
    )
    .expect("a fresh index");
    let started = bounded(move || Chain::start(parts).err());
    assert!(
        matches!(
            started,
            Some(StartError::Load(LoadFailure::Failure {
                cause: None
            }))
        ),
        "{started:?}"
    );
    assert_eq!(abort.messages(), Vec::<String>::new());
}

/// Block files that fail to write the block at height 1 (a devnet's genesis block).
pub(crate) struct NoRoomAtOne(pub(crate) Box<dyn BlockFiles>);

impl BlockFiles for NoRoomAtOne {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        if height == 1 {
            return Err("disk full".into());
        }
        self.0.save_block(block, height, known)
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.0.read_block(pos, hash)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.0.open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.0.current_usage()
    }

    fn scan_already_pruned_files(&mut self, have_pruned: bool) -> Result<(), String> {
        self.0.scan_already_pruned_files(have_pruned)
    }
}

/// Block files that refuse the startup scan, as latched ones do (BL-523).
struct ScanRefused(MemBlockFiles);

impl BlockFiles for ScanRefused {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        self.0.save_block(block, height, known)
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.0.read_block(pos, hash)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.0.open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.0.current_usage()
    }

    fn scan_already_pruned_files(&mut self, _have_pruned: bool) -> Result<(), String> {
        Err("refused".into())
    }
}

/// BL-523: `open_parts`' empty-index path runs the startup scan as `open_block_index`'s does:
/// files that refuse it fail the load as a false `LoadBlockIndex`, before the `-prune` state
/// check; files that take it are refused by that check.
#[test]
fn open_parts_scans_an_empty_index_before_its_prune_refusal() {
    let p = regtest();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_700_000_000));
    let abort = Arc::new(RecordingAbort::default());
    let files: [(Box<dyn BlockFiles>, LoadFailure); 2] = [
        (Box::new(ScanRefused(MemBlockFiles::default())), LoadFailure::LoadingBlockDb),
        (mem_files(), LoadFailure::PrunedDatadir),
    ];
    for (files, failure) in files {
        let seams = seams(&p, &clock, &abort);
        let opened =
            open_parts(Arc::clone(&p), seams, std::iter::empty(), files, true, Reindex::default());
        assert_eq!(opened.err(), Some(failure));
    }
}

/// Block files that find no position for the block at height 1 (Core's null `FlatFilePos`).
struct NoPositionAtOne(MemBlockFiles);

impl BlockFiles for NoPositionAtOne {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        if height == 1 {
            return Ok(None);
        }
        self.0.save_block(block, height, known)
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.0.read_block(pos, hash)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.0.open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.0.current_usage()
    }

    fn scan_already_pruned_files(&mut self, have_pruned: bool) -> Result<(), String> {
        self.0.scan_already_pruned_files(have_pruned)
    }
}

/// A command's result, or its error's code and message.
type Answer<'a> = Result<&'a str, (i32, &'a str)>;

/// DEC-279's `submitblock` through the node's command table over V when the block's write fails
/// (review r1 m1). Core's null position is `AcceptBlock`'s `state.Error`, which PNB's
/// `BlockChecked` hands the catcher: -25 with Core's text. A write that throws is
/// `AbortNode(state, "System error: ...")`: the node's one report, then the same -25 (BL-279).
// Core: validation.cpp:4840-4850, :2245-2249, :4896-4899; rpc/mining.cpp:532-538, :1056-1063
#[test]
fn submitblock_answers_a_failed_block_write() {
    let no_position = r#"{"code":-25,"message":"AcceptBlock: Failed to find position to write new block to disk"}"#;
    let cases: [(Box<dyn BlockFiles>, Answer, &[&str]); 2] = [
        (Box::new(NoPositionAtOne(MemBlockFiles::default())), Err((-25, no_position)), &[]),
        (
            Box::new(NoRoomAtOne(Box::new(MemBlockFiles::default()))),
            Err((-25, r#"{"code":-25,"message":"System error: disk full"}"#)),
            &["System error: disk full"],
        ),
    ];
    for (files, expected, aborts) in cases {
        let p = regtest();
        let genesis = genesis_block(&p);
        let time = i64::from(genesis.header().raw().time()) + 60;
        let clock: Arc<dyn Clock> = Arc::new(MockClock::at(time));
        let abort = Arc::new(RecordingAbort::default());
        let parts = fresh(&p, &clock, &abort, files);
        let b1 = branch(&p, &genesis, 0, 1, 0).remove(0);
        let answer = bounded(move || {
            let mut chain = Chain::start(parts).expect("V starts");
            startup(&mut chain);
            imported(&mut chain);
            let slot = Arc::new(crate::rpc::ChainSlot::default());
            slot.attach(chain.handle(), chain.signals(), None, None, None);
            let logger = Arc::new(crate::logging::Logger::new());
            let shutdown = Arc::new(ShutdownState::new());
            let table = crate::rpc::command_table(
                &p,
                &logger,
                &shutdown,
                &Arc::new(MockClock::at(time)),
                (true, &crate::scheduler::Scheduler::new()),
                &slot,
                &Arc::default(),
                &Arc::default(),
                &Arc::default(),
                Default::default(),
                std::env::temp_dir(),
            );
            table.start();
            table.set_warmup_finished();
            let hex: String = dash_consensus::primitives::core_serialize(&*b1)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let answer = table.execute(&dashd_rpc::request::JsonRpcRequest {
                method: "submitblock".into(),
                params: dashd_rpc::UniValue::Arr(vec![hex.into()]),
                ..Default::default()
            });
            slot.detach();
            table.interrupt();
            chain.stop();
            answer.map(|v| v.write()).map_err(|e| (e.code(), e.to_object().write()))
        });
        let expected = expected.map(str::to_owned).map_err(|(code, m)| (code, m.to_owned()));
        assert_eq!(answer, expected);
        let messages = abort.messages();
        assert!(
            aborts.iter().all(|a| messages.iter().any(|m| m.contains(a)))
                && messages.len() == aborts.len(),
            "{messages:?}"
        );
    }
}

/// An `AbortNode` in the devnet step's `AcceptBlock` reaches the sink before `start` returns
/// init's error, so init sees the shutdown it requested (Core skips the reindex question).
// Core: validation.cpp:4850, init.cpp:2109
#[test]
fn an_abort_in_the_devnet_step_comes_before_init_s_error() {
    let p = devnet();
    let dg =
        dash_consensus::params::devnet_genesis_block(&p).expect("a devnet genesis block").header();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(i64::from(dg.time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let parts =
        fresh(&p, &clock, &abort, Box::new(NoRoomAtOne(Box::new(MemBlockFiles::default()))));
    let sink = abort.clone();
    let (started, aborted) = bounded(move || {
        let started = Chain::start(parts).err();
        (started, sink.messages())
    });
    assert!(
        matches!(
            started,
            Some(StartError::Load(LoadFailure::Failure {
                cause: None
            }))
        ),
        "{started:?}"
    );
    assert_eq!(aborted, ["System error: disk full"]);
}

/// Startup's ABC runs with `fImporting` set, so it is in IBD on a recent genesis; the flag is
/// cleared once it completes.
// Core: node/blockstorage.cpp:876-892
#[test]
fn startup_activates_while_importing() {
    let p = regtest();
    let genesis = genesis_block(&p);
    // A minute after genesis: not in IBD but for the import.
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 60));
    let abort = Arc::new(RecordingAbort::default());
    let tips = Arc::new(TipIbd::default());
    let parts = fresh(&p, &clock, &abort, Box::new(MemBlockFiles::default()));
    parts.signals.register(tips.clone());
    bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        assert!(!chain.handle().loading().importing(), "the import ended");
        chain.stop();
        chain.flush_background_callbacks();
    });
    assert_eq!(*tips.0.lock().expect("ibd"), [true]);
}

/// A flush side whose first site, startup's ABC connecting genesis, waits for the test.
struct GatedFlush(Option<mpsc::Receiver<()>>);

impl FlushSite for GatedFlush {
    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        if let Some(gate) = self.0.take() {
            let _ = gate.recv();
        }
        dashd_chainstate::chain::NoFlush.at_site(m, h)
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }
}

/// `-stopafterblockimport`: the stop runs once startup's ABC has succeeded, with `fImporting`
/// still set; the import's tail then runs as without it.
// Core: node/blockstorage.cpp:950-963
#[test]
fn the_stop_after_import_runs_after_a_successful_abc_while_importing() {
    let p = regtest();
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis_block(&p).header().raw().time())));
    let abort = Arc::new(RecordingAbort::default());
    let parts = fresh(&p, &clock, &abort, Box::new(MemBlockFiles::default()));
    bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        let (release, gate) = mpsc::channel();
        let coins = dashd_chainstate::chain::AttachCoins {
            connector: Box::new(dashd_chainstate::chain::NullConnector),
            flush: Box::new(GatedFlush(Some(gate))),
            facade: None,
        };
        chain.attach_coins(coins).expect("V takes the coins");
        let v = chain.handle().clone();
        let (send, stopped) = mpsc::channel();
        let stop = move || {
            send.send((v.view().snap.has_tip(), v.loading().importing())).expect("the test");
        };
        chain.import(Some(Box::new(stop))).expect("the loadblk thread");
        assert_eq!(
            stopped.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "no stop while the ABC runs"
        );
        release.send(()).expect("the gate");
        chain.wait_for_genesis(|| false);
        imported(&mut chain);
        assert_eq!(stopped.try_recv(), Ok((true, true)), "after the ABC, while importing");
        assert!(stopped.try_recv().is_err(), "once");
        assert!(!chain.handle().loading().importing(), "the import ended");
        chain.stop();
        chain.flush_background_callbacks();
    });
    assert!(abort.messages().is_empty());
}

/// A flush side whose every site fails with an `AbortNode`, which V reports.
struct FailingFlush;

impl FlushSite for FailingFlush {
    fn at_site(&mut self, _: FlushStateMode, _: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        Err(Fatal::new("disk on fire"))
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }
}

/// [`FailingFlush`] from its second site on: startup's ABC connects genesis first.
#[derive(Default)]
struct FailingFlushAfterOne(bool);

impl FlushSite for FailingFlushAfterOne {
    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        if std::mem::replace(&mut self.0, true) {
            return FailingFlush.at_site(m, h);
        }
        dashd_chainstate::chain::NoFlush.at_site(m, h)
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }
}

/// Each `InitializeCurrentBlockTip`: its tip and `ibd`, and how many `AbortNode`s the sink had
/// heard by then.
struct HeardInit {
    abort: Arc<RecordingAbort>,
    calls: Mutex<Vec<(Option<BlockId>, bool, usize)>>,
}

impl ValidationInterface for HeardInit {
    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        let aborts = self.abort.messages().len();
        self.calls.lock().expect("calls").push((tip, ibd, aborts));
    }
}

/// A sync with no peers to tell.
struct NoNotices;

impl dashd_masternode::sync::NodeSyncNotifier for NoNotices {
    fn sync_reset(&self) {}

    fn sync_finished(&self) {}
}

/// Startup's ABC failing on an `AbortNode`: V's report, then `ThreadImport`'s own with Core's
/// `state.ToString()` (BL-472). The import still ends: V has stopped, and its reaper serves
/// loadblk's `SetLoading` (CH3b D220), then its `InitializeCurrentBlockTip` (CH7-notes D72),
/// whose listener runs after both reports, at V's tip, out of IBD once `fImporting` is cleared.
/// The node's real `SyncListener` runs there too (Opus r1 n-3): the sync records that the tip is
/// the best header, which only an `UpdatedBlockTip` out of IBD does.
// Core: node/blockstorage.cpp:883-887, :949-952, :960, shutdown.cpp:27-38, init.cpp:2455-2473
#[test]
fn a_failed_startup_abc_aborts_the_node_again() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 60));
    let abort = Arc::new(RecordingAbort::default());
    let parts = fresh(&p, &clock, &abort, Box::new(MemBlockFiles::default()));
    let heard = Arc::new(HeardInit {
        abort: Arc::clone(&abort),
        calls: Mutex::default(),
    });
    parts.signals.register(heard.clone());
    let sync = Arc::new(dashd_masternode::sync::MasternodeSync::new(
        Arc::clone(&clock),
        Box::new(NoNotices),
    ));
    let synced = Arc::clone(&sync);
    let tip = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&tip);
    bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        let listener = crate::mnsync::SyncListener::new(synced, chain.handle().view_handle());
        chain.queue.register(Arc::new(listener));
        let coins = dashd_chainstate::chain::AttachCoins {
            connector: Box::new(dashd_chainstate::chain::NullConnector),
            flush: Box::new(FailingFlushAfterOne::default()),
            facade: None,
        };
        chain.attach_coins(coins).expect("V takes the coins");
        // `-stopafterblockimport` does not stop after a failed ABC (`blockstorage.cpp:952-955`).
        let stop = || panic!("a stop after a failed ABC");
        chain.import(Some(Box::new(stop))).expect("the loadblk thread");
        chain.import.take().expect("the import").join().expect("the loadblk thread");
        // The reaper serves the lane in order: a query sent after loadblk's `SetLoading`
        // disconnects once the flag is cleared.
        assert!(chain.handle().query(|_| ()).recv().is_err(), "V has stopped");
        assert!(!chain.handle().loading().importing(), "the import ended");
        let exit = chain.stop().expect("V's state");
        assert!(exit.fatal.is_some(), "V stopped on it");
        *seen.lock().expect("tip") = exit.chain.private_tip().map(|(t, _)| t.id);
        chain.flush_background_callbacks();
    });
    assert_eq!(abort.messages(), ["disk on fire", "Failed to connect best block (disk on fire)"]);
    let tip = *tip.lock().expect("tip");
    assert!(tip.is_some(), "genesis connected before the flush failed");
    assert_eq!(*heard.calls.lock().expect("calls"), [(tip, false, 2)]);
    assert!(sync.is_reached_best_header(), "the sync heard init's tip");
}

/// The node's `AbortNode` as `abort.rs` ends it: the report, then exit status 1 and the
/// shutdown request.
struct Exits {
    rec: RecordingAbort,
    /// The node's `AbortNode`, so each report sets the misc warning and the exit status as
    /// the node's does. A read failure stays with `rec`: the node's would end the process.
    node: crate::abort::NodeAbort,
    state: Arc<ShutdownState>,
    warnings: Arc<crate::warnings::Warnings>,
}

impl Exits {
    fn new() -> Exits {
        let logger = Arc::new(crate::logging::Logger::new());
        let ui = Arc::new(crate::ui::Ui::with_stderr(logger, Box::new(|_| {})));
        let state = Arc::new(ShutdownState::new());
        let warnings = Arc::new(crate::warnings::Warnings::new());
        Exits {
            rec: RecordingAbort::default(),
            node: crate::abort::NodeAbort::new(ui, Arc::clone(&warnings), Arc::clone(&state)),
            state,
            warnings,
        }
    }
}

impl AbortSink for Exits {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        self.rec.abort_node(log_msg, user_msg);
        self.node.abort_node(log_msg, user_msg);
    }

    fn read_failed(&self, what: &str) {
        self.rec.read_failed(what);
    }
}

/// The `BlockChecked`s and tip notifications V sends, `UpdatedBlockTip` and `NotifyBlockTip`.
#[derive(Default)]
struct HeardSignals(Mutex<Vec<String>>);

impl ValidationInterface for HeardSignals {
    fn block_checked(&self, block: &Block, _: &dashd_chainstate::chain::BlockState, _: bool) {
        self.0.lock().expect("heard").push(format!("BlockChecked {}", block.hash()));
    }

    fn updated_block_tip(&self, new: BlockId, _: Option<BlockId>, _: bool) {
        self.0.lock().expect("heard").push(format!("UpdatedBlockTip {new:?}"));
    }
}

impl UiNotifications for HeardSignals {
    fn notify_block_tip(&self, _: SyncState, hash: BlockHash, _: u32) {
        self.0.lock().expect("heard").push(format!("NotifyBlockTip {hash}"));
    }
}

/// The node's flush with V's bell left off, as `dashd-chainstate`'s `NoBell` does: every
/// [`FlushSite`] method but `attach` goes to the node's flush, so W's trip does not ring idle V, so V takes its next command first, and that command's admission is where V
/// first reads the latch. With the bell ([`BellWatched`]), idle V answers the ring first and
/// stops there; its reaper answers startup's ABC `Ok` (D241).
struct Unbelled<F>(F);

/// The node's flush with V's bell attached, as in production, every [`FlushSite`] method
/// forwarded: W's trip rings idle V. `failed` hears once `answer_due` has returned an error, the
/// failure V's bell answer stops on.
struct BellWatched<F> {
    flush: F,
    failed: mpsc::Sender<()>,
}

impl<F: FlushSite> FlushSite for BellWatched<F> {
    fn attach(&mut self, bell: FlushBell) {
        self.flush.attach(bell);
    }

    fn at_site(
        &mut self,
        mode: FlushStateMode,
        host: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal> {
        self.flush.at_site(mode, host)
    }

    fn after_step(&mut self, decided: SiteDecision, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.flush.after_step(decided, host)
    }

    fn service(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.flush.service(host)
    }

    fn answer_due(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        let answered = self.flush.answer_due(host);
        if answered.is_err() {
            let _ = self.failed.send(());
        }
        answered
    }

    fn move_prune_locks_back(&mut self, height: u32) {
        self.flush.move_prune_locks_back(height);
    }

    fn flush(
        &mut self,
        mode: FlushStateMode,
        manual_prune_height: Option<u32>,
        host: &mut FlushHost<'_>,
    ) -> Result<(), Fatal> {
        self.flush.flush(mode, manual_prune_height, host)
    }

    fn coins_stats(
        &mut self,
        hash_type: dash_consensus::coins::stats::CoinStatsHashType,
        interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        dump: Option<dashd_chainstate::coins::CoinsDump>,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<dashd_chainstate::coins::CoinsStatsJob>, Fatal> {
        self.flush.coins_stats(hash_type, interrupted, dump, host)
    }

    fn coins_scan(
        &mut self,
        scan: dashd_chainstate::coins::CoinsScan,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<dashd_chainstate::coins::CoinsScanJob>, Fatal> {
        self.flush.coins_scan(scan, host)
    }

    fn prune_and_flush(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.flush.prune_and_flush(host)
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn staged_record(
        &mut self,
        key: &BlockHash,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<dashd_chainstate::chain::StagedRecord>, Fatal> {
        self.flush.staged_record(key, host)
    }

    fn stop(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.flush.stop(host)
    }

    fn abort(
        &mut self,
        cause: dashd_chainstate::chain::AbortCause,
        host: &mut FlushHost<'_>,
    ) -> bool {
        self.flush.abort(cause, host)
    }

    fn latched(&self) -> Option<Fatal> {
        self.flush.latched()
    }

    fn admit(&self) -> Result<(), Fatal> {
        self.flush.admit()
    }

    fn cache_usage(&self) -> dashd_chainstate::chain::CacheUsage {
        self.flush.cache_usage()
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn settle(
        &mut self,
        host: &mut FlushHost<'_>,
    ) -> Result<dashd_chainstate::chain::Quiet, Fatal> {
        self.flush.settle(host)
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn label(&mut self, label: Option<Arc<str>>) {
        self.flush.label(label);
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn panic_w_at_next_step(&mut self) {
        self.flush.panic_w_at_next_step();
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn panic_at_next_flush(&mut self, at: dashd_chainstate::chain::FlushPanic) {
        self.flush.panic_at_next_flush(at);
    }
}

impl<F: FlushSite> FlushSite for Unbelled<F> {
    fn at_site(
        &mut self,
        mode: FlushStateMode,
        host: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal> {
        self.0.at_site(mode, host)
    }

    fn after_step(&mut self, decided: SiteDecision, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.after_step(decided, host)
    }

    fn service(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.service(host)
    }

    fn answer_due(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.answer_due(host)
    }

    fn move_prune_locks_back(&mut self, height: u32) {
        self.0.move_prune_locks_back(height);
    }

    fn flush(
        &mut self,
        mode: FlushStateMode,
        manual_prune_height: Option<u32>,
        host: &mut FlushHost<'_>,
    ) -> Result<(), Fatal> {
        self.0.flush(mode, manual_prune_height, host)
    }

    fn coins_stats(
        &mut self,
        hash_type: dash_consensus::coins::stats::CoinStatsHashType,
        interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        dump: Option<dashd_chainstate::coins::CoinsDump>,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<dashd_chainstate::coins::CoinsStatsJob>, Fatal> {
        self.0.coins_stats(hash_type, interrupted, dump, host)
    }

    fn coins_scan(
        &mut self,
        scan: dashd_chainstate::coins::CoinsScan,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<dashd_chainstate::coins::CoinsScanJob>, Fatal> {
        self.0.coins_scan(scan, host)
    }

    fn prune_and_flush(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.prune_and_flush(host)
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn staged_record(
        &mut self,
        key: &BlockHash,
        host: &mut FlushHost<'_>,
    ) -> Result<Option<dashd_chainstate::chain::StagedRecord>, Fatal> {
        self.0.staged_record(key, host)
    }

    fn stop(&mut self, host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.stop(host)
    }

    fn abort(
        &mut self,
        cause: dashd_chainstate::chain::AbortCause,
        host: &mut FlushHost<'_>,
    ) -> bool {
        self.0.abort(cause, host)
    }

    fn latched(&self) -> Option<Fatal> {
        self.0.latched()
    }

    fn admit(&self) -> Result<(), Fatal> {
        self.0.admit()
    }

    fn cache_usage(&self) -> dashd_chainstate::chain::CacheUsage {
        self.0.cache_usage()
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn settle(
        &mut self,
        host: &mut FlushHost<'_>,
    ) -> Result<dashd_chainstate::chain::Quiet, Fatal> {
        self.0.settle(host)
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn label(&mut self, label: Option<Arc<str>>) {
        self.0.label(label);
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn panic_w_at_next_step(&mut self) {
        self.0.panic_w_at_next_step();
    }

    #[cfg(any(feature = "test-support", feature = "test-hooks"))]
    fn panic_at_next_flush(&mut self, at: dashd_chainstate::chain::FlushPanic) {
        self.0.panic_at_next_flush(at);
    }
}

/// W's store failure text, as W's step reports it.
const W_FAILED: &str = "System error while flushing: injected store failure";

/// W's store failure as W's step makes it, on W's thread: latched with its report deferred,
/// then reported to `sink` and marked made.
fn w_fails(latch: &dash_kv::GlobalFailStop, sink: Arc<dyn AbortSink>) {
    let latch = latch.clone();
    std::thread::Builder::new()
        .name("dashd-flush".into())
        .spawn(move || {
            let deferred = latch.defer_reports();
            let failure = dash_kv::Failure {
                kind: dash_kv::ErrorKind::Io,
                site: dashd_chainstate::store::sites::WRITE_SITE,
                message: "injected store failure".into(),
            };
            assert!(latch.trip(failure), "W's failure latches first");
            sink.abort_node(W_FAILED, None);
            deferred.reported();
        })
        .expect("W's thread")
        .join()
        .expect("W reports");
}

/// What a restart on a loaded best tip left once W's store failure latched, `early` before
/// startup's import or else after it, then a query.
#[derive(Debug)]
struct AfterWsFailure {
    reports: Vec<String>,
    exit_status: i32,
    /// `GetWarnings(false)`, which the RPCs' `warnings` field shows.
    warning: String,
    /// V's stop: its text, and all of it (who reported it, and which of Core's ends it is).
    fatal: Option<(String, String)>,
    tip: Option<BlockId>,
    loaded: BlockId,
    durable: Option<BlockHash>,
    durable_before: Option<BlockHash>,
    /// The coins database's stored best block (`meta`'s `tip`), after V's stop and before the trip.
    stored: Option<BlockHash>,
    stored_before: Option<BlockHash>,
    loaded_hash: BlockHash,
    /// The flush begins and durable marks after the trip.
    flushes: usize,
    heard: Vec<String>,
    importing: bool,
    /// With `stop`, each run of the import's `-stopafterblockimport` closure: `fImporting` and
    /// the node's reports as it ran.
    stopped: Vec<(bool, Vec<String>)>,
}

/// [`AfterWsFailure`] of a restart whose import runs a `-stopafterblockimport` closure if `stop`.
/// With `belled`, V's bell is attached ([`BellWatched`]), and with `early` the import starts only
/// once V has met W's failure at its bell, with no command sent since the trip.
fn restart_with_ws_failure(early: bool, stop: bool, belled: bool) -> AfterWsFailure {
    use dashd_chainstate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
    use dashd_chainstate::flush::{
        CrashRoll, FlushConfig, FlushPolicy, NoEvo, NoInputs, NoPrune, NodeFlush, NodeFlushParts,
        ObsMark, RecordingObserver, SubscriberHub, ThreadDriver, WThread,
    };
    use dashd_chainstate::store::{CHAINSTATE_SCHEMA, GuardedEngine, GuardedFiles, Meta};
    use dashd_chainstate::tuning::MemoryTuning;
    use dashd_store::flatfile::{
        BlockFiles as StoreFiles, BlockFilesConfig, BlockFilesState, OsFs,
    };
    use dashd_store::{Commit, MemoryEngine};

    let p = regtest();
    let genesis = genesis_block(&p);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
    let files = SharedFiles::default();
    let first_abort = Arc::new(RecordingAbort::default());
    let mut first =
        Chain::start(fresh(&p, &clock, &first_abort, Box::new(files.clone()))).expect("V starts");
    startup(&mut first);
    let a1 = branch(&p, &genesis, 0, 1, 0).remove(0);
    let r = first.handle().process_block(Arc::clone(&a1), true, None, true).recv();
    assert!(r.is_ok_and(|r| r.accepted), "block accepted");
    let loaded = id(first.handle(), &a1);
    let VExit {
        mut chain,
        ..
    } = first.stop().expect("V stopped");
    drop(first);
    assert_eq!(first_abort.messages(), Vec::<String>::new());

    let dir = tempfile::tempdir().expect("tempdir");
    let latch = dash_kv::GlobalFailStop::new();
    let exits = Arc::new(Exits::new());
    let sink: Arc<dyn AbortSink> = exits.clone();
    let db = Arc::new(GuardedEngine::new(
        MemoryEngine::new(&CHAINSTATE_SCHEMA),
        latch.clone(),
        Arc::clone(&sink),
    ));
    // The coins as the first run left them: durable at a1, its best block stored.
    let loaded_ref = dash_chain::BlockRef {
        id: loaded,
        hash: a1.hash(),
        height: 1,
    };
    let mut seed = db.batch();
    Meta::put_tip(&mut seed, &loaded_ref.hash);
    db.write(seed, Commit::Durable).expect("the coins' best block");
    let stored = {
        let db = Arc::clone(&db);
        move || Meta::read(&*db).expect("the coins' meta").tip
    };
    let stored_before = stored();
    let budget = CoinsBudget::new(8 << 20, 0, Ratio::HALF, 0);
    let coins = Arc::new(Mutex::new(CoinsCache::new(
        Arc::clone(&db),
        budget,
        Salt::random(),
        Some(loaded_ref),
    )));
    let fs = Arc::new(OsFs::new());
    let blocks = dir.path().join("blocks");
    std::fs::create_dir(&blocks).expect("the blocks dir");
    let store = StoreFiles::with_fs(
        Arc::clone(&fs),
        BlockFilesConfig::new(blocks.clone(), [0xfc, 0xc1, 0xb7, 0xdc]),
        BlockFilesState::default(),
    );
    let guarded = Arc::new(GuardedFiles::new(store, latch.clone()));
    let hub = Arc::new(SubscriberHub::new());
    let obs = Arc::new(RecordingObserver::default());
    let driver = ThreadDriver::spawn(WThread {
        engine: Arc::clone(&db),
        files: guarded,
        fs,
        hub: Arc::clone(&hub),
        obs: obs.clone(),
        tuning: Arc::new(MemoryTuning),
        cfg: FlushConfig {
            sub_batch: 4 << 10,
            data_dir: dir.path().to_path_buf(),
            blocks_dir: blocks,
            live_copy: false,
        },
        covered_seq: 0,
        crash: CrashRoll::new(0),
        on_crash: Box::new(|| {}),
    })
    .expect("W's thread");
    let flush = NodeFlush::new(NodeFlushParts {
        coins: Arc::clone(&coins),
        driver,
        policy: FlushPolicy::new(false),
        ledger: Arc::default(),
        hub,
        obs: obs.clone(),
        evo: Box::new(NoEvo),
        prune: Box::new(NoPrune),
        inputs: Box::new(NoInputs),
    });
    let durable = move || coins.lock().expect("coins").durable().map(|b| b.hash);
    let durable_before = durable();

    let mut idx = NodeBlockIndex::load(&chain);
    load_chain_tip(&mut chain, &mut idx, loaded).expect("V starts");
    let heard = Arc::new(HeardSignals::default());
    let node = NodeSeams {
        abort: Arc::clone(&sink),
        ui: heard.clone(),
        ..seams(&p, &clock, &first_abort)
    };
    let parts2 = parts(Arc::clone(&p), chain, idx, Box::new(files), node).expect("parts");
    parts2.signals.register(heard.clone());
    let stopped = Arc::new(Mutex::new(Vec::new()));
    let (seen, at_stop) = (Arc::clone(&stopped), Arc::clone(&exits));
    let (tip, fatal, importing, flushes) = bounded(move || {
        let mut chain = Chain::start(parts2).expect("V starts");
        let (failed, at_bell) = mpsc::channel();
        let flush: Box<dyn FlushSite> = if belled {
            Box::new(BellWatched {
                flush,
                failed,
            })
        } else {
            Box::new(Unbelled(flush))
        };
        let coins = dashd_chainstate::chain::AttachCoins {
            connector: Box::new(dashd_chainstate::chain::NullConnector),
            flush,
            facade: None,
        };
        chain.attach_coins(coins).expect("V takes the coins");
        let mut marks = obs.marks().len();
        if early {
            w_fails(&latch, Arc::clone(&sink));
            if belled {
                at_bell.recv_timeout(HUNG).expect("V meets W's failure at its bell");
            }
        }
        let stop_after_import = stop.then(|| {
            let v = chain.handle().clone();
            Box::new(move || {
                let at = (v.loading().importing(), at_stop.rec.messages());
                seen.lock().expect("stopped").push(at);
            }) as Box<dyn FnOnce() + Send>
        });
        chain.import(stop_after_import).expect("the loadblk thread");
        chain.import.take().expect("the import").join().expect("the loadblk thread");
        if !early {
            marks = obs.marks().len();
            w_fails(&latch, Arc::clone(&sink));
        }
        assert!(chain.handle().query(|_| ()).recv().is_err(), "V has stopped");
        let importing = chain.handle().loading().importing();
        let exit = chain.stop().expect("V's state");
        chain.flush_background_callbacks();
        let flushes = obs.marks()[marks..]
            .iter()
            .filter(|m| matches!(m, ObsMark::FlushBegin { .. } | ObsMark::Durable { .. }))
            .count();
        let tip = exit.chain.private_tip().map(|(t, _)| t.id);
        let fatal = exit.fatal.map(|f| (f.0.clone(), format!("{f:?}")));
        (tip, fatal, importing, flushes)
    });
    let heard = std::mem::take(&mut *heard.0.lock().expect("heard"));
    AfterWsFailure {
        reports: exits.rec.messages(),
        exit_status: exits.state.exit_status(),
        warning: exits.warnings.get_warnings(false),
        fatal,
        tip,
        loaded,
        durable: durable(),
        durable_before,
        stored: stored(),
        stored_before,
        loaded_hash: a1.hash(),
        flushes,
        heard,
        importing,
        stopped: std::mem::take(&mut *stopped.lock().expect("stopped")),
    }
}

/// BL-678, D240 (DEC-1373): a restart on a loaded best tip whose W store failure latches before
/// startup's import, V taking the import's first command before it hears W ([`Unbelled`]).
/// V refuses that command at its admission, and the reaper answers startup's ABC `Ok`, as it
/// took no step: Core's ABC has no work here and returns `true`, so `ThreadImport` raises nothing
/// and the node has W's one report, as with W's failure latched after the import. So the misc
/// warning (`GetWarnings(false)`, which the `warnings` field of `getblockchaininfo`,
/// `getmininginfo` and `getnetworkinfo` shows) is W's text, the exit status and V's stop are the
/// same, and nothing else changed: the tip is the loaded one, the coins' durable best block is
/// the same, no flush ran after the trip, V sent no `BlockChecked` or tip notification, and the
/// import cleared `fImporting`.
// Core: validation.cpp:3789-3791, :3823; node/blockstorage.cpp:952-953, :963;
// shutdown.cpp:27-38; warnings.cpp:36-65
#[test]
fn a_w_failure_before_a_restarts_import_reports_once() {
    let early = restart_with_ws_failure(true, false, false);
    let late = restart_with_ws_failure(false, false, false);
    assert_eq!(early.reports, [W_FAILED], "{early:?}");
    assert_eq!(early.reports, late.reports, "{late:?}");
    assert_eq!(early.warning, W_FAILED, "W's misc warning, as Core's one report leaves it");
    assert_eq!(early.warning, late.warning);
    assert_eq!(early.exit_status, late.exit_status);
    assert_eq!(early.exit_status, EXIT_FAILURE);
    assert_eq!(early.fatal.as_ref().map(|f| f.0.as_str()), Some(W_FAILED), "{early:?}");
    assert_eq!(early.fatal, late.fatal, "the same stop");
    for (run, r) in [("early", &early), ("late", &late)] {
        assert_eq!(r.tip, Some(r.loaded), "{run}: the loaded tip");
        assert_eq!(r.stored_before, Some(r.loaded_hash), "{run}: the coins stored at a1");
        assert_eq!(r.stored, r.stored_before, "{run}: the coins' stored best block");
        assert_eq!(r.durable_before, Some(r.loaded_hash), "{run}: the coins durable at a1");
        assert_eq!(r.durable, r.durable_before, "{run}: the coins' durable best block");
        assert_eq!(r.flushes, 0, "{run}: no flush after the trip");
        assert!(r.heard.is_empty(), "{run}: {:?}", r.heard);
        assert!(!r.importing, "{run}: the import cleared fImporting");
    }
}

/// D240 (DEC-1373): with `-stopafterblockimport`, the restart of
/// [`a_w_failure_before_a_restarts_import_reports_once`] runs its stop closure once, as Core's
/// `ThreadImport` does after an ABC that returned `true`: inside `CImportingNow` (`fImporting`
/// still set) and after W's report; the import then clears `fImporting`. With W's failure
/// latched after the import, the closure runs before any report.
// Core: node/blockstorage.cpp:886-890, :952-962
#[test]
fn a_w_failure_before_a_restarts_import_runs_its_stop_after_import() {
    let early = restart_with_ws_failure(true, true, false);
    assert_eq!(early.stopped, [(true, vec![W_FAILED.to_owned()])], "{early:?}");
    assert!(!early.importing, "the import cleared fImporting");
    assert_eq!(early.reports, [W_FAILED], "{early:?}");
    let late = restart_with_ws_failure(false, true, false);
    assert_eq!(late.stopped, [(true, vec![])], "{late:?}");
    assert!(!late.importing, "the import cleared fImporting");
}

/// D241 (D237's bell race, the bell first): the restart of
/// [`a_w_failure_before_a_restarts_import_runs_its_stop_after_import`] with V's bell attached.
/// W's trip rings idle V, which meets W's failure at its bell before the import sends
/// anything, with no command refused; the reaper then answers startup's ABC `Ok`, so the import
/// runs its stop closure once, inside `fImporting` and after W's one report, and the node ends
/// as in the refusal order: the same reports, misc warning, exit status, closure runs and stop
/// text. The tip is the loaded one, and nothing was flushed or signalled.
// Core: node/blockstorage.cpp:886-890, :950-962; validation.cpp:3789-3791, :3823
#[test]
fn a_w_failure_at_vs_bell_before_a_restarts_import_runs_its_stop_after_import() {
    let belled = restart_with_ws_failure(true, true, true);
    assert_eq!(belled.stopped, [(true, vec![W_FAILED.to_owned()])], "{belled:?}");
    assert_eq!(belled.reports, [W_FAILED], "{belled:?}");
    assert_eq!(belled.warning, W_FAILED, "W's misc warning");
    assert_eq!(belled.exit_status, EXIT_FAILURE);
    assert_eq!(belled.tip, Some(belled.loaded), "the loaded tip");
    assert_eq!(belled.stored_before, Some(belled.loaded_hash), "the coins stored at a1");
    assert_eq!(belled.stored, belled.stored_before, "the coins' stored best block");
    assert_eq!(belled.durable_before, Some(belled.loaded_hash), "the coins durable at a1");
    assert_eq!(belled.durable, belled.durable_before, "the coins' durable best block");
    assert_eq!(belled.flushes, 0, "no flush after the trip");
    assert!(belled.heard.is_empty(), "{:?}", belled.heard);
    assert!(!belled.importing, "the import cleared fImporting");
    let refused = restart_with_ws_failure(true, true, false);
    assert_eq!(belled.reports, refused.reports);
    assert_eq!(belled.warning, refused.warning);
    assert_eq!(belled.exit_status, refused.exit_status);
    assert_eq!(belled.stopped, refused.stopped);
    let text = |r: &AfterWsFailure| r.fatal.as_ref().map(|f| f.0.clone());
    assert_eq!(text(&belled), Some(W_FAILED.to_owned()), "{belled:?}");
    assert_eq!(text(&belled), text(&refused), "the same stop text");
}

/// Records V's `NotifyBlockTip`s on their way to the node's sink.
#[cfg(unix)]
struct HeardTips {
    sink: Arc<dyn UiNotifications>,
    tips: Mutex<Vec<(SyncState, BlockHash)>>,
}

#[cfg(unix)]
impl UiNotifications for HeardTips {
    fn notify_block_tip(&self, state: SyncState, hash: BlockHash, height: u32) {
        self.tips.lock().unwrap().push((state, hash));
        self.sink.notify_block_tip(state, hash, height);
    }
}

/// `-blocknotify` hears V's `NotifyBlockTip`: not genesis, which startup's ABC connects while
/// importing (`INIT_DOWNLOAD`), but a block connected after init, named in Core's hex.
// Core: init.cpp:2431-2433, validation.cpp:3830-3831
#[cfg(unix)]
#[test]
fn block_notify_runs_for_a_tip_after_init() {
    let p = regtest();
    let genesis = genesis_block(&p);
    // A minute after genesis: not in IBD once the import ends.
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 60));
    let abort = Arc::new(RecordingAbort::default());
    let dir = tempfile::tempdir().expect("a directory");
    let args =
        crate::init::test_args(&[&format!("-blocknotify=touch '{}'/%s", dir.path().display())]);
    let (logger, _) = crate::init::test_logger();
    let heard = Arc::new(HeardTips {
        sink: crate::notify::chain_ui(&logger, &args, &Arc::default(), &Arc::default()).unwrap(),
        tips: Mutex::default(),
    });
    let parts = fresh_ui(&p, &clock, &abort, Box::new(MemBlockFiles::default()), heard.clone());
    let a1 = branch(&p, &genesis, 0, 1, 0).remove(0);
    let block = Arc::clone(&a1);
    bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        let r = chain.handle().process_block(block, true, None, true).recv();
        assert!(r.is_ok_and(|r| r.accepted), "block accepted");
        chain.stop();
    });
    assert_eq!(
        *heard.tips.lock().unwrap(),
        [(SyncState::InitDownload, genesis.hash()), (SyncState::PostInit, a1.hash())]
    );
    // Core's `GetHex` of a hash computed here, not parsed back.
    assert_eq!(
        genesis.hash().to_string(),
        "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e"
    );
    let file = dir.path().join(a1.hash().to_string());
    crate::notify::tests::wait_for_file(&file);
    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(files, [file], "genesis connected while importing");
    assert_eq!(abort.messages(), Vec::<String>::new());
}

/// RP1a-2a: the waits read `latestblock` through V's `NotifyBlockTip`, which the RPC server's
/// start connects before the chain loads, as Core's `AppInitServers` precedes the load. Init's
/// `RPCNotifyBlockChange(ActiveTip())` sets the loaded tip; a block V connects wakes a wait for
/// it; and the server's interrupt and stop end a wait that nothing would satisfy.
// Core: init.cpp:558-568, :2761-2769; rpc/blockchain.cpp:300-308, :398-441
#[test]
fn the_waits_hear_the_tips_v_connects() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let time = i64::from(genesis.header().raw().time()) + 60;
    let mock = Arc::new(MockClock::at(time));
    let clock: Arc<dyn Clock> = mock.clone();
    let abort = Arc::new(RecordingAbort::default());
    let (logger, _) = crate::init::test_logger();
    let slot = Arc::new(crate::rpc::ChainSlot::default());
    let ui = crate::notify::chain_ui(
        &logger,
        &crate::init::test_args(&[]),
        &Arc::default(),
        slot.block_change(),
    )
    .unwrap();
    let parts = fresh_ui(&p, &clock, &abort, Box::new(MemBlockFiles::default()), ui);
    let a1 = branch(&p, &genesis, 0, 1, 0).remove(0);
    let block = Arc::clone(&a1);
    let genesis_hash = genesis.hash();
    bounded(move || {
        let table = crate::rpc::command_table(
            &p,
            &Arc::new(crate::logging::Logger::new()),
            &Arc::new(ShutdownState::new()),
            &mock,
            (true, &crate::scheduler::Scheduler::new()),
            &slot,
            &Arc::default(),
            &Arc::default(),
            &Arc::default(),
            Default::default(),
            std::env::temp_dir(),
        );
        table.start();
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        slot.attach(chain.handle(), chain.signals(), None, None, None);
        slot.notify_loaded_tip();
        table.set_warmup_finished();
        let at = |hash: BlockHash, height: u32| format!(r#"{{"hash":"{hash}","height":{height}}}"#);
        assert_eq!(rpc(&table, "waitforblockheight", "[0, 1]"), Ok(at(genesis_hash, 0)));

        let waiting = |params: &'static str| {
            let t = Arc::clone(&table);
            std::thread::spawn(move || rpc(&t, "waitforblockheight", params))
        };
        let wait = waiting("[1]");
        let r = chain.handle().process_block(block, true, None, true).recv();
        assert!(r.is_ok_and(|r| r.accepted), "block accepted");
        assert_eq!(wait.join().unwrap(), Ok(at(a1.hash(), 1)));
        let hash = format!(r#"["{}", 1]"#, a1.hash());
        assert_eq!(rpc(&table, "waitforblock", &hash), Ok(at(a1.hash(), 1)));

        // Interrupted before or during the wait, it answers the same.
        let wait = waiting("[100]");
        table.interrupt();
        table.stop();
        assert_eq!(wait.join().unwrap(), Ok(at(a1.hash(), 1)));
        slot.detach();
        chain.stop();
    });
    assert_eq!(abort.messages(), Vec::<String>::new());
}

/// V reads and sets the node's own shutdown flag (CH1-node D52): `-stopatheight` wakes the
/// node's wait once a block reaches it, and V's checks see the node's request.
// Core: validation.cpp:3836, shutdown.cpp:66-105
#[test]
fn v_shares_the_node_s_shutdown_flag() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let node = Arc::new(ShutdownState::new());
    let seams = NodeSeams {
        shutdown: node.clone(),
        stop_at_height: 2,
        ..seams(&p, &clock, &abort)
    };
    let parts = fresh_with(&p, Box::new(MemBlockFiles::default()), seams);
    let blocks = branch(&p, &genesis, 0, 2, 0);
    let waiter = {
        let node = Arc::clone(&node);
        std::thread::spawn(move || node.wait_for_shutdown_timeout(HUNG))
    };
    let (below, at, v_sees) = bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        let mut requested = Vec::new();
        for b in blocks {
            let r = chain.handle().process_block(b, true, None, true).recv();
            assert!(r.is_ok_and(|r| r.accepted), "block accepted");
            requested.push(chain.handle().shutdown_requested());
        }
        chain.stop();
        (requested[0], requested[1], requested)
    });
    assert_eq!((below, at), (false, true), "{v_sees:?}");
    assert!(node.shutdown_requested());
    assert!(waiter.join().expect("the waiter"), "the node's wait woke");
    assert_eq!(abort.messages(), Vec::<String>::new());
}

/// With a shutdown requested, an `ActivateBestChain` runs one step and stops. A step returns
/// once the tip has more work than before, so a reconsider over 40 blocks reconnects one. The
/// request is the node's, not V's `Shutdown`.
// Core: validation.cpp:3671-3674, :3847
#[test]
fn a_shutdown_request_stops_an_abc_after_one_step() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let node = Arc::new(ShutdownState::new());
    let seams = NodeSeams {
        shutdown: node.clone(),
        ..seams(&p, &clock, &abort)
    };
    let parts = fresh_with(&p, Box::new(MemBlockFiles::default()), seams);
    let blocks = branch(&p, &genesis, 0, 40, 0);
    let first = blocks[0].hash();
    let heights = bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        for b in blocks {
            let r = chain.handle().process_block(b, true, None, true).recv();
            assert!(r.is_ok_and(|r| r.accepted), "block accepted");
        }
        let height = |c: &Chain| c.handle().view().snap.tip.tip_height;
        let connected = height(&chain);
        assert_eq!(chain.handle().invalidate_block(first).recv(), Ok(Ok(())));
        let invalidated = height(&chain);
        node.start_shutdown();
        assert_eq!(chain.handle().reconsider_block(first, false).recv(), Ok(Ok(())));
        let reconsidered = height(&chain);
        chain.stop();
        (connected, invalidated, reconsidered)
    });
    assert_eq!(heights, (40, 0, 1));
    assert_eq!(abort.messages(), Vec::<String>::new());
}

/// `open_parts` is CH1-node's `open_block_index` on an empty index (ND1-D5): the same genesis
/// chain and tables, and INTERRUPTED once shutdown is requested, as `open_parts` is
/// ([`the_index_load_sees_the_node_s_shutdown`]). A tripwire for a change to CH1-node's empty
/// path: both sides share `load_genesis_block`.
#[test]
fn open_parts_is_open_block_index_on_an_empty_index() {
    let p = regtest();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_700_000_000));
    let abort = Arc::new(RecordingAbort::default());
    let ours = open_parts(
        Arc::clone(&p),
        seams(&p, &clock, &abort),
        std::iter::empty(),
        mem_files(),
        false,
        Reindex::default(),
    )
    .unwrap();
    let svc = Services {
        clock: Arc::clone(&clock),
        sporks: Arc::new(StaticSporks::for_network(&p, None)),
        fail: Arc::new(ReadFailStop {
            abort: abort.clone(),
            tripped: AtomicBool::new(false),
        }),
    };
    let found = LoadedIndex::Empty {
        cfg: ChainConfig::node(&p),
        svc,
    };
    let mut files = MemBlockFiles::default();
    let (chain, idx) = chain::open_block_index(
        Arc::clone(&p),
        found,
        &mut files,
        &LocalShutdown::default(),
        false,
        false,
    )
    .unwrap();
    let genesis = genesis_block(&p).hash();
    assert_eq!(ours.chain.reader().lookup(&genesis), Some(ours.chain.best_header()));
    assert_eq!(chain.reader().lookup(&genesis), Some(chain.best_header()));
    assert_eq!(ours.chain.ids().count(), chain.ids().count());
    assert_eq!(ours.idx, idx);

    let stopped = LocalShutdown::default();
    stopped.start_shutdown();
    let found = LoadedIndex::Empty {
        cfg: ChainConfig::node(&p),
        svc: Services {
            clock: Arc::clone(&clock),
            sporks: Arc::new(StaticSporks::for_network(&p, None)),
            fail: Arc::new(ReadFailStop {
                abort: abort.clone(),
                tripped: AtomicBool::new(false),
            }),
        },
    };
    let theirs = chain::open_block_index(Arc::clone(&p), found, &mut files, &stopped, false, false);
    assert!(matches!(theirs, Err(LoadFailure::Interrupted)));
}

/// `-checkblockindex` (BL-525) reaches both of its consumers through `open_parts`: V's
/// `CheckBlockIndex` and the chain's header subset of it.
// Core: validation.cpp:5613-5617, :6774
#[test]
fn open_parts_hands_checkblockindex_to_v_and_the_chain() {
    let p = regtest();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_700_000_000));
    let abort = Arc::new(RecordingAbort::default());
    for on in [false, true] {
        let node = NodeSeams {
            check_block_index: on,
            ..seams(&p, &clock, &abort)
        };
        let parts = open_parts(
            Arc::clone(&p),
            node,
            std::iter::empty(),
            mem_files(),
            false,
            Reindex::default(),
        )
        .expect("a fresh index");
        assert_eq!((parts.check_block_index, parts.chain.config().consistency_checks), (on, on));
    }
}

/// The index load polls the node's flag: a shutdown requested before it is Core's INTERRUPTED.
// Core: node/chainstate.cpp:177
#[test]
fn the_index_load_sees_the_node_s_shutdown() {
    let p = regtest();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_700_000_000));
    let abort = Arc::new(RecordingAbort::default());
    let node = Arc::new(ShutdownState::new());
    node.start_shutdown();
    let seams = NodeSeams {
        shutdown: node,
        ..seams(&p, &clock, &abort)
    };
    assert!(matches!(
        open_parts(p, seams, std::iter::empty(), mem_files(), false, Reindex::default()),
        Err(LoadFailure::Interrupted)
    ));
}

/// A listener that counts the flushes it heard.
#[derive(Default)]
struct Flushes(Mutex<usize>);

impl ValidationInterface for Flushes {
    fn chain_state_flushed(&self, _: &[BlockHash]) {
        *self.0.lock().expect("flushes") += 1;
    }
}

/// Shutdown's first `FlushBackgroundCallbacks` (ND1-D9): once V has stopped, the drain returns
/// only after the queued signals ran, and the signals thread still runs later ones.
// Core: init.cpp:360, :390
#[test]
fn the_drain_before_the_flush_runs_the_queued_signals() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let parts = fresh(&p, &clock, &abort, Box::new(MemBlockFiles::default()));
    let heard = Arc::new(Flushes::default());
    parts.signals.register(heard.clone());
    let ran = bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        chain.stop();
        let queued = || chain.queue.enqueue(Queued::ChainStateFlushed(Vec::new()));
        queued();
        chain.drain_background_callbacks();
        let drained = *heard.0.lock().expect("flushes");
        queued();
        chain.flush_background_callbacks();
        (drained, *heard.0.lock().expect("flushes"))
    });
    assert_eq!(ran, (1, 2));
    assert_eq!(abort.messages(), Vec::<String>::new());
}

/// A listener whose `ChainStateFlushed` panics.
struct PanickingFlush;

impl ValidationInterface for PanickingFlush {
    fn chain_state_flushed(&self, _: &[BlockHash]) {
        panic!("a listener fails");
    }
}

/// A listener's panic in the drain is the node's `AbortNode` by the time the drain returns, so
/// shutdown's flushes see it; the closed queue then drains at once.
// Core: init.cpp:360, :385
#[test]
fn an_abort_in_the_drain_lands_before_it_returns() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let parts = fresh(&p, &clock, &abort, Box::new(MemBlockFiles::default()));
    parts.signals.register(Arc::new(PanickingFlush));
    let seen = Arc::clone(&abort);
    let heard = bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        chain.stop();
        chain.queue.enqueue(Queued::ChainStateFlushed(Vec::new()));
        chain.drain_background_callbacks();
        let heard = seen.messages().len();
        chain.drain_background_callbacks();
        chain.flush_background_callbacks();
        heard
    });
    assert_eq!(heard, 1, "{:?}", abort.messages());
}

fn engine_error(kind: ErrorKind) -> dash_kv::StoreError {
    dash_kv::StoreError::Engine {
        engine: "rows",
        kind,
        message: "planted".to_owned(),
    }
}

/// The rows come back as records: each 32-byte key as the block's hash, its value unread, in
/// the rows' order (the store's key order, `Opened::block_index`).
// Core: txdb.cpp:304-307
#[test]
fn index_records_key_each_row_by_its_hash() {
    let rows =
        [Ok(([1; 32].to_vec(), b"first".to_vec())), Ok(([2; 32].to_vec(), b"second".to_vec()))];
    let records: Vec<_> = index_records(rows.into_iter()).map(Result::unwrap).collect();
    assert_eq!(
        records,
        [
            (BlockHash::from([1; 32]), b"first".to_vec()),
            (BlockHash::from([2; 32]), b"second".to_vec())
        ]
    );
}

/// A read the engine reports as corrupt is a corrupt index (LoadBlockIndexGuts' false), and any
/// other failed read is the store's, which init reports as its exception. A key that is not a
/// block hash is damage, never the end of the index.
// Core: txdb.cpp:338-339
#[test]
fn index_records_keeps_a_corrupt_index_apart_from_a_failed_read() {
    let rows = [
        Err(engine_error(ErrorKind::Io)),
        Err(engine_error(ErrorKind::NoSpace)),
        Err(engine_error(ErrorKind::Corruption)),
        Ok((vec![7; 31], b"short".to_vec())),
    ];
    let read: Vec<_> = index_records(rows.into_iter()).collect();
    let corrupt = |r: &StoredRecord| match r {
        Err(LoadError::Read(e)) => e.is_corrupt(),
        other => panic!("{other:?}"),
    };
    assert!(!corrupt(&read[0]) && !corrupt(&read[1]), "{read:?}");
    assert!(corrupt(&read[2]) && corrupt(&read[3]), "{read:?}");
    assert_eq!(
        read[3].as_ref().unwrap_err().to_string(),
        "header store corrupt: a block_index key of 31 bytes, not a block hash"
    );
}

/// V's block files in memory, for the parts' tests that never read one back.
fn mem_files() -> Box<dyn dashd_chainstate::chain::BlockFiles> {
    Box::new(MemBlockFiles::default())
}

/// One command through `table`: its reply written, or its error's code and object.
pub(crate) fn rpc(
    table: &dashd_rpc::server::RpcServer,
    method: &str,
    params: &str,
) -> Result<String, (i32, String)> {
    table
        .execute(&dashd_rpc::request::JsonRpcRequest {
            method: method.into(),
            params: dashd_rpc::UniValue::read(params.as_bytes()).expect("JSON params"),
            ..Default::default()
        })
        .map(|v| v.write())
        .map_err(|e| (e.code(), e.to_object().write()))
}

/// The tips `getchaintips` lists, as (hash, branchlen, status).
fn tips(table: &dashd_rpc::server::RpcServer) -> Vec<(String, i64, String)> {
    let reply = rpc(table, "getchaintips", "[]").expect("getchaintips");
    let reply = dashd_rpc::UniValue::read(reply.as_bytes()).expect("JSON");
    let field = |t: &dashd_rpc::UniValue, k: &str| -> dashd_rpc::UniValue {
        let dashd_rpc::UniValue::Obj(o) = t else {
            panic!("a tip object")
        };
        o.iter().find(|(key, _)| key == k).expect("the field").1.clone()
    };
    reply
        .get_values()
        .expect("an array")
        .into_iter()
        .map(|t| {
            (
                field(t, "hash").get_str().unwrap().to_owned(),
                field(t, "branchlen").get_int64().unwrap(),
                field(t, "status").get_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// getblockchaininfo's `verificationprogress` reads the system clock, Core's `time(nullptr)`,
/// never the node's mock time: on testnet, whose `chainTxData` has a rate, the two give
/// different answers (RP1a-2b r1 Opus n-2).
// Core: validation.cpp:5913-5928 GuessVerificationProgress; rpc/blockchain.cpp:1607
#[test]
fn the_tip_progress_reads_the_system_clock() {
    use dash_clock::SystemClock;
    use dashd_chainstate::chain::guess_verification_progress;
    use dashd_rpc::blockchain::NodeChainState as _;

    let p = Arc::new(
        ChainParamsBuilder::new(Network::Testnet3, Release::V24_0).build().expect("testnet"),
    );
    let genesis = genesis_block(&p);
    let genesis_time = genesis.header().raw().time();
    // The mock clock at testnet's `chainTxData` time, where progress is higher than today's.
    let data = p.chain_tx_data;
    let mock_now = data.timestamp;
    let mock = Arc::new(MockClock::at(mock_now));
    let clock: Arc<dyn Clock> = mock.clone();
    let abort = Arc::new(RecordingAbort::default());
    let parts = fresh(&p, &clock, &abort, Box::new(MemBlockFiles::default()));
    bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        let slot = crate::rpc::ChainSlot::default();
        slot.attach(chain.handle(), chain.signals(), None, None, None);
        let before = SystemClock.node_now().secs();
        let tip = slot.chain_state().expect("attached").tip_info().expect("V answers");
        let after = SystemClock.node_now().secs();
        let at = |now| guess_verification_progress(&data, 1, genesis_time, now);
        let mocked = at(mock_now);
        assert!(
            (at(after)..=at(before)).contains(&tip.verification_progress),
            "{} not between {} and {}",
            tip.verification_progress,
            at(after),
            at(before)
        );
        assert!(mocked > at(before), "the mock clock's {mocked} is outside the range");
    });
}

/// RP1a-1's chain commands through the node's command table over a real V: tips and their
/// statuses across `invalidateblock`, `reconsiderblock` and `preciousblock`, the header's
/// depth and neighbours, and `setmocktime` landing on V's clock.
// Core: rpc/blockchain.cpp:136-166, :1827-2072; rpc/node.cpp:250-289
#[test]
fn the_chain_commands_move_and_read_vs_tip() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let time = i64::from(genesis.header().raw().time()) + 3600;
    let mock = Arc::new(MockClock::at(time));
    let clock: Arc<dyn Clock> = mock.clone();
    let abort = Arc::new(RecordingAbort::default());
    let parts = fresh(&p, &clock, &abort, Box::new(MemBlockFiles::default()));
    let a = branch(&p, &genesis, 0, 3, 0);
    let b = branch(&p, &genesis, 0, 2, 1);
    let c = branch(&p, &genesis, 0, 3, 2);
    let hex = |b: &Block| b.hash().to_string();
    let g = hex(&genesis);
    let d = branch(&p, &a[2], 3, 2, 3);
    let (a1, a2, a3, b2, c3) = (hex(&a[0]), hex(&a[1]), hex(&a[2]), hex(&b[1]), hex(&c[2]));
    let header_hex = |b: &Block| -> String {
        dash_consensus::primitives::core_serialize(b.header().raw())
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    };
    let (d4, d4_header, d5_header) = (hex(&d[0]), header_hex(&d[0]), header_hex(&d[1]));
    bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        for block in a.iter().chain(&b) {
            let r = chain.handle().process_block(Arc::clone(block), true, None, true).recv();
            assert!(r.expect("V answers").accepted);
        }
        let slot = Arc::new(crate::rpc::ChainSlot::default());
        slot.attach(chain.handle(), chain.signals(), None, None, None);
        let table = crate::rpc::command_table(
            &p,
            &Arc::new(crate::logging::Logger::new()),
            &Arc::new(ShutdownState::new()),
            &mock,
            (true, &crate::scheduler::Scheduler::new()),
            &slot,
            &Arc::default(),
            &Arc::default(),
            &Arc::default(),
            Default::default(),
            std::env::temp_dir(),
        );
        table.start();
        table.set_warmup_finished();
        let quoted = |h: &str| format!(r#""{h}""#);
        let arg = |h: &str| format!(r#"["{h}"]"#);

        assert_eq!(rpc(&table, "getbestblockhash", "[]"), Ok(quoted(&a3)));
        // B never connected: its blocks are valid to their tree only.
        assert_eq!(
            tips(&table),
            [(a3.clone(), 0, "active".into()), (b2.clone(), 2, "valid-headers".into())]
        );
        let header = rpc(&table, "getblockheader", &arg(&a2)).expect("A2's header");
        assert!(header.contains(r#""confirmations":2,"height":2,"#), "{header}");
        assert!(
            header.contains(&format!(r#""previousblockhash":"{a1}","nextblockhash":"{a3}""#)),
            "{header}"
        );
        let header = rpc(&table, "getblockheader", &arg(&b2)).expect("B2's header");
        assert!(
            header.contains(r#""confirmations":-1,"#) && !header.contains("nextblockhash"),
            "{header}"
        );
        let raw = rpc(&table, "getblockheader", &format!(r#"["{g}", false]"#)).expect("hex");
        assert_eq!(raw.len(), 2 + 160, "{raw}");

        // Invalidating A2 activates B, which has more work than A1; A forks at genesis.
        assert_eq!(rpc(&table, "invalidateblock", &arg(&a2)), Ok("null".into()));
        assert_eq!(rpc(&table, "getbestblockhash", "[]"), Ok(quoted(&b2)));
        assert_eq!(
            tips(&table),
            [(a3.clone(), 3, "invalid".into()), (b2.clone(), 0, "active".into())]
        );
        assert_eq!(rpc(&table, "reconsiderblock", &arg(&a2)), Ok("null".into()));
        assert_eq!(rpc(&table, "getbestblockhash", "[]"), Ok(quoted(&a3)));

        // C3 ties A3's work: precious makes it the tip, and A3 again after it.
        for block in &c {
            let r = chain.handle().process_block(Arc::clone(block), true, None, true).recv();
            assert!(r.expect("V answers").accepted);
        }
        assert_eq!(rpc(&table, "getbestblockhash", "[]"), Ok(quoted(&a3)));
        assert_eq!(rpc(&table, "preciousblock", &arg(&c3)), Ok("null".into()));
        assert_eq!(rpc(&table, "getbestblockhash", "[]"), Ok(quoted(&c3)));
        assert_eq!(rpc(&table, "preciousblock", &arg(&a3)), Ok("null".into()));
        assert_eq!(rpc(&table, "getbestblockhash", "[]"), Ok(quoted(&a3)));

        let unknown = arg(&"00".repeat(32));
        let not_found = r#"{"code":-5,"message":"Block not found"}"#.to_owned();
        for method in ["invalidateblock", "reconsiderblock", "preciousblock", "getblockheader"] {
            assert_eq!(rpc(&table, method, &unknown), Err((-5, not_found.clone())), "{method}");
        }

        // A header with no indexed parent is refused; with one, it is a headers-only tip.
        let no_parent =
            format!(r#"{{"code":-25,"message":"Must submit previous header ({d4}) first"}}"#);
        assert_eq!(rpc(&table, "submitheader", &arg(&d5_header)), Err((-25, no_parent)));
        assert_eq!(rpc(&table, "submitheader", &arg(&d4_header)), Ok("null".into()));
        assert_eq!(
            rpc(&table, "submitheader", &arg(&d4_header)),
            Ok("null".into()),
            "a known header"
        );
        assert!(
            tips(&table).contains(&(d4.clone(), 1, "headers-only".into())),
            "{:?}",
            tips(&table)
        );
        assert_eq!(rpc(&table, "getbestblockhash", "[]"), Ok(quoted(&a3)));

        // These coins have no database (NoFlush): Core's unreadable set.
        let unreadable = r#"{"code":-32603,"message":"Unable to read UTXO set"}"#.to_owned();
        assert_eq!(rpc(&table, "gettxoutsetinfo", r#"["muhash"]"#), Err((-32603, unreadable)));

        assert_eq!(rpc(&table, "syncwithvalidationinterfacequeue", "[]"), Ok("null".into()));
        assert_eq!(rpc(&table, "setmocktime", "[1800000000]"), Ok("null".into()));
        assert_eq!(mock.mocktime(), 1_800_000_000);
        assert_eq!(rpc(&table, "mockscheduler", "[60]"), Ok("null".into()));

        slot.detach();
        table.interrupt();
        chain.stop();
    });
    assert!(abort.messages().is_empty(), "{:?}", abort.messages());
}

/// Prunes block file 0 at the first flush site after it is armed: `PruneOneBlockFile`'s index
/// side (`HAVE_DATA` cleared, `nTx` kept, `m_have_pruned` set), without a target.
struct PruneFileZero(Arc<AtomicBool>);

impl FlushSite for PruneFileZero {
    fn at_site(
        &mut self,
        _: FlushStateMode,
        host: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal> {
        use dashd_chainstate::flush::FlushIndex as _;
        if self.0.swap(false, Ordering::SeqCst) {
            host.prune_clear(&[0]).expect("V's index");
        }
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }
}

/// DEC-606: `getblock` through the node's command table reads the slot's own block files at
/// verbosity 0 and 1; once the block's file is pruned, `GetBlockChecked`'s `IsBlockPruned` refuses
/// it before any read, though the files still hold the bytes, while a header-only entry on the
/// pruned node is still "not found on disk".
// Core: rpc/blockchain.cpp:786-804 GetBlockChecked; node/blockstorage.cpp:443-447 IsBlockPruned
#[test]
fn getblock_refuses_a_pruned_block_before_its_read() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let time = i64::from(genesis.header().raw().time()) + 3600;
    let mock = Arc::new(MockClock::at(time));
    let clock: Arc<dyn Clock> = mock.clone();
    let abort = Arc::new(RecordingAbort::default());
    let files = SharedFiles::default();
    let parts = fresh(&p, &clock, &abort, Box::new(files.clone()));
    let armed = Arc::new(AtomicBool::new(false));
    let a = branch(&p, &genesis, 0, 4, 4);
    bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup_with(&mut chain, Box::new(PruneFileZero(Arc::clone(&armed))));
        imported(&mut chain);
        let submit = |b: &Arc<Block>| {
            let r = chain.handle().process_block(Arc::clone(b), true, None, true).recv();
            assert!(r.expect("V answers").accepted);
        };
        submit(&a[0]);
        submit(&a[1]);
        let slot = Arc::new(crate::rpc::ChainSlot::default());
        slot.attach(chain.handle(), chain.signals(), Some(Box::new(files.clone())), None, None);
        let table = crate::rpc::command_table(
            &p,
            &Arc::new(crate::logging::Logger::new()),
            &Arc::new(ShutdownState::new()),
            &mock,
            (true, &crate::scheduler::Scheduler::new()),
            &slot,
            &Arc::default(),
            &Arc::default(),
            &Arc::default(),
            Default::default(),
            std::env::temp_dir(),
        );
        table.start();
        table.set_warmup_finished();
        let arg = |b: &Block, rest: &str| format!(r#"["{}"{rest}]"#, b.hash());
        let bytes: String = dash_consensus::CoreSerialize::core_serialize(&*a[0])
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();
        assert_eq!(rpc(&table, "getblock", &arg(&a[0], ", 0")), Ok(format!(r#""{bytes}""#)));
        let json = rpc(&table, "getblock", &arg(&a[0], "")).expect("verbosity 1");
        let txid = a[0].txs()[0].txid();
        assert!(json.ends_with(&format!(r#","tx":["{txid}"]}}"#)), "{json}");
        assert!(json.contains(r#""confirmations":2,"#), "{json}");

        armed.store(true, Ordering::SeqCst);
        submit(&a[2]);
        assert!(!armed.load(Ordering::SeqCst), "a flush site ran");
        assert_eq!(
            rpc(&table, "getblock", &arg(&a[0], ", 0")),
            Err((-1, r#"{"code":-1,"message":"Block not available (pruned data)"}"#.into()))
        );
        // MemBlockFiles' positions count up from 8: genesis, then A1.
        let pos = FlatFilePos {
            file: 0,
            pos: 9,
        };
        let held = files.read_block(pos, &a[0].hash()).map(|b| b.hash());
        assert_eq!(held, Ok(a[0].hash()), "the bytes are still there");
        // A header-only entry is not pruned data (`nTx > 0`): its null position fails to open.
        let header = HeaderBatch {
            hashes: vec![a[3].hash()],
            headers: vec![*a[3].header()],
        };
        let headers = chain.handle().process_headers(header, None).recv().expect("V answers");
        assert!(headers.stop.is_none(), "{:?}", headers.stop);
        assert_eq!(
            rpc(&table, "getblock", &arg(&a[3], ", 0")),
            Err((-1, r#"{"code":-1,"message":"Block not found on disk"}"#.into()))
        );
        slot.detach();
        table.interrupt();
        chain.stop();
    });
}

/// RP1a-1b's `generate*` through the node's command table over a real V and MN0's assembler:
/// blocks mined to an address or a descriptor connect and pay its script, `maxtries` 0 mines
/// none, `generateblock` without `submit` leaves the tip to `submitblock`, and its raw
/// transactions follow the template's.
// Core: rpc/mining.cpp:139-420; test/functional/rpc_generate.py:28-94
#[test]
fn the_generate_commands_mine_on_vs_tip() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let time = i64::from(genesis.header().raw().time()) + 3600;
    let mock = Arc::new(MockClock::at(time));
    let clock: Arc<dyn Clock> = mock.clone();
    let abort = Arc::new(RecordingAbort::default());
    let files = SharedFiles::default();
    let parts = fresh(&p, &clock, &abort, Box::new(files.clone()));
    bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        startup(&mut chain);
        imported(&mut chain);
        let slot = Arc::new(crate::rpc::ChainSlot::default());
        let miner = dashd_mining::ChainTemplateSource::new(
            chain.handle().clone(),
            Arc::clone(&p),
            clock,
            None,
        );
        slot.attach(
            chain.handle(),
            chain.signals(),
            Some(Box::new(files)),
            Some(Arc::new(miner)),
            None,
        );
        let table = crate::rpc::command_table(
            &p,
            &Arc::new(crate::logging::Logger::new()),
            &Arc::new(ShutdownState::new()),
            &mock,
            (true, &crate::scheduler::Scheduler::new()),
            &slot,
            &Arc::default(),
            &Arc::default(),
            &Arc::default(),
            Default::default(),
            std::env::temp_dir(),
        );
        table.start();
        table.set_warmup_finished();
        let strings = |reply: &str| -> Vec<String> {
            dashd_rpc::UniValue::read(reply.as_bytes())
                .expect("JSON")
                .get_values()
                .expect("an array")
                .iter()
                .map(|h| h.get_str().unwrap().to_owned())
                .collect()
        };
        let coinbase_script = |hash: &str| -> Vec<u8> {
            let hex = rpc(&table, "getblock", &format!(r#"["{hash}", 0]"#)).expect("the block");
            let hex = hex.trim_matches('"');
            let bytes: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let block =
                dash_consensus::decode_block(&mut bytes.as_slice()).expect("a block").into_value();
            block.txs()[0].outputs()[0].script_pubkey().to_vec()
        };

        // rpc_generate.py's regtest address of the generator point's key.
        let address = "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6";
        let p2pkh = |script: &[u8]| script.len() == 25 && script[..3] == [0x76, 0xa9, 0x14];
        let mined = strings(
            &rpc(&table, "generatetoaddress", &format!(r#"[3, "{address}"]"#)).expect("mined"),
        );
        assert_eq!(mined.len(), 3);
        assert_eq!(rpc(&table, "getblockcount", "[]"), Ok("3".into()));
        assert_eq!(rpc(&table, "getbestblockhash", "[]"), Ok(format!(r#""{}""#, mined[2])));
        let paid = coinbase_script(&mined[0]);
        assert!(p2pkh(&paid), "{paid:02x?}");
        let combo = "combo(0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798)";
        let by_descriptor = strings(
            &rpc(&table, "generatetodescriptor", &format!(r#"[1, "{combo}"]"#)).expect("mined"),
        );
        assert_eq!(coinbase_script(&by_descriptor[0]), paid);
        let raw = strings(&rpc(&table, "generatetodescriptor", r#"[1, "raw(51)"]"#).unwrap());
        assert_eq!(coinbase_script(&raw[0]), [0x51]);
        assert_eq!(
            rpc(&table, "generatetoaddress", &format!(r#"[2, "{address}", 0]"#)),
            Ok("[]".into())
        );
        assert_eq!(rpc(&table, "getblockcount", "[]"), Ok("5".into()));

        // Not submitted: the tip stays until submitblock takes the block.
        let made = rpc(&table, "generateblock", &format!(r#"["{address}", [], false]"#)).unwrap();
        let made = dashd_rpc::UniValue::read(made.as_bytes()).unwrap();
        let dashd_rpc::UniValue::Obj(fields) = &made else {
            panic!("an object")
        };
        let field = |k: &str| {
            fields.iter().find(|(key, _)| key == k).unwrap().1.get_str().unwrap().to_owned()
        };
        assert_eq!(rpc(&table, "getblockcount", "[]"), Ok("5".into()));
        assert_eq!(
            rpc(&table, "submitblock", &format!(r#"["{}"]"#, field("hex"))),
            Ok("null".into())
        );
        assert_eq!(rpc(&table, "getbestblockhash", "[]"), Ok(format!(r#""{}""#, field("hash"))));
        let submitted = rpc(&table, "generateblock", &format!(r#"["{address}", []]"#)).unwrap();
        assert!(submitted.starts_with(r#"{"hash":""#) && !submitted.contains("hex"), "{submitted}");
        assert_eq!(rpc(&table, "getblockcount", "[]"), Ok("7".into()));

        // A raw transaction follows the template's coinbase. (These parts' connector accepts
        // every block, so `TestBlockValidity`'s refusals are DIFF-RPC's, not this test's.)
        let tx = format!(
            "0100000001{}000000000151ffffffff0100e1f505000000000151{}",
            "11".repeat(32),
            "00".repeat(4)
        );
        let with_tx = rpc(&table, "generateblock", &format!(r#"["{address}", ["{tx}"]]"#)).unwrap();
        let hash = &with_tx[r#"{"hash":""#.len()..with_tx.len() - 2];
        let hex = rpc(&table, "getblock", &format!(r#"["{hash}", 0]"#)).expect("the block");
        assert!(hex.trim_matches('"').ends_with(&tx), "{hex}");

        // V's `NotBestPrev` for a template a block overtook is the tip-moved verdict, so
        // `generateblock` rebuilds (D17); a template on the tip is valid.
        use dashd_rpc::mining::{BlockCheck, Miner as _};
        let script = coinbase_script(&mined[0]);
        let stale = slot.create_new_block(&script, false).expect("a template");
        rpc(&table, "generatetoaddress", &format!(r#"[1, "{address}"]"#)).expect("mined");
        assert_eq!(slot.test_block_validity(Arc::new(stale)), BlockCheck::TipMoved);
        let fresh = slot.create_new_block(&script, false).expect("a template");
        assert_eq!(slot.test_block_validity(Arc::new(fresh)), BlockCheck::Valid);
        table.interrupt();
    });
}

/// CH7-notes D62: `loadblk`'s tail clears `fImporting` before it sends
/// `InitializeCurrentBlockTip`, so V reads `ibd` with the import over (r1 Opus m-1).
// Core: node/blockstorage.cpp:960-961, init.cpp:2460-2473
#[test]
fn loadblk_clears_importing_before_the_init_tip_command() {
    #[derive(Default)]
    struct Order(Mutex<Vec<&'static str>>);

    impl LoadblkV for Order {
        fn send(&self, cmd: ChainCommand) {
            let what = match cmd {
                ChainCommand::SetLoading {
                    importing: Some(false),
                    reindex: None,
                } => "importing(false)",
                _ => "another command",
            };
            self.0.lock().unwrap().push(what);
        }

        fn initialize_current_block_tip(&self) {
            self.0.lock().unwrap().push("InitializeCurrentBlockTip");
        }
    }

    let order = Order::default();
    after_import(&order);
    assert_eq!(*order.0.lock().unwrap(), ["importing(false)", "InitializeCurrentBlockTip"]);
}

/// The flush side's reindex clear, counted, ending as `end` says.
struct Clears {
    count: Arc<std::sync::atomic::AtomicU32>,
    fail: ClearEnd,
}

/// How [`Clears`] ends the clear (OD-CH5-77).
#[derive(Clone, Copy)]
enum ClearEnd {
    Written,
    /// The clear's own job fails: an `AbortNode` V reports and answers.
    Failed,
    /// An `AbortNode` met before the clear ran (an earlier job's, another store's): V reports it
    /// and drops the reply.
    Before,
}

const CLEAR_FAILED: &str = "System error: the clear failed";
const FAILED_BEFORE: &str = "System error: an earlier flush failed";

impl Clears {
    fn site(fail: ClearEnd) -> (Box<dyn FlushSite>, Arc<std::sync::atomic::AtomicU32>) {
        let count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let site = Clears {
            count: Arc::clone(&count),
            fail,
        };
        (Box::new(site), count)
    }
}

impl FlushSite for Clears {
    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        NoFlush.at_site(m, h)
    }

    fn after_step(&mut self, d: SiteDecision, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        NoFlush.after_step(d, h)
    }

    fn service(&mut self, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        NoFlush.service(h)
    }

    fn reindex_clear(&mut self, _host: &mut FlushHost<'_>) -> Result<Result<(), Fatal>, Fatal> {
        self.count.fetch_add(1, Ordering::SeqCst);
        match self.fail {
            ClearEnd::Written => Ok(Ok(())),
            ClearEnd::Failed => Ok(Err(Fatal::abort_node(CLEAR_FAILED))),
            ClearEnd::Before => Err(Fatal::abort_node(FAILED_BEFORE)),
        }
    }
}

/// V on the empty index `-reindex` opened: no genesis written, `fReindex` set as V starts.
pub(crate) fn reindexing(
    p: &Arc<ChainSetup>,
    clock: &Arc<dyn Clock>,
    abort: &Arc<RecordingAbort>,
) -> VParts {
    let reindex = Reindex {
        argument: true,
        stored: false,
    };
    open_parts(
        Arc::clone(p),
        seams(p, clock, abort),
        std::iter::empty(),
        mem_files(),
        false,
        reindex,
    )
    .expect("an empty index")
}

/// What one reindexing import did: whether `-stopafterblockimport`'s call ran (startup's ABC
/// answered `Ok`), and the reindex flag and V's tip when `on_reindex_finished` ran, if it did.
struct ReindexRun {
    stopped: bool,
    finished: Vec<(bool, bool)>,
}

/// Starts V on `parts` with `flush`, then step 11's import with the hooks set, and joins it.
fn reindex_run(parts: VParts, flush: Box<dyn FlushSite>) -> (Chain, ReindexRun) {
    let mut chain = Chain::start(parts).expect("V starts");
    assert!(chain.handle().loading().reindex(), "set as V starts");
    let coins = dashd_chainstate::chain::AttachCoins {
        connector: Box::new(dashd_chainstate::chain::NullConnector),
        flush,
        facade: None,
    };
    chain.attach_coins(coins).expect("V takes the coins");
    let (send, finished) = mpsc::channel();
    let v = chain.handle().clone();
    chain.set_import_hooks(ImportHooks {
        on_reindex_finished: Some(Box::new(move || {
            let seen = (v.loading().reindex(), v.view().snap.has_tip());
            send.send(seen).expect("the test");
        })),
        ..ImportHooks::default()
    });
    let (stop, stopped) = mpsc::channel();
    let stop = move || stop.send(()).expect("the test");
    chain.import(Some(Box::new(stop))).expect("the loadblk thread");
    chain.import.take().expect("started").join().expect("the loadblk thread");
    let run = ReindexRun {
        stopped: stopped.try_recv().is_ok(),
        finished: finished.try_iter().collect(),
    };
    (chain, run)
}

/// `ThreadImport` under `fReindex` with no block file: the clear (fired hook, `fReindex` reset),
/// then the genesis retry, then startup's ABC, which connects genesis (Core's `fReindex` is read
/// once, at the thread's start).
// Core: node/blockstorage.cpp:900-928, :948-955 at v24.0.0
#[test]
fn a_reindex_import_clears_and_retries_genesis_before_startups_abc() {
    let p = regtest();
    let clock = clock_at(&p);
    let abort = Arc::new(RecordingAbort::default());
    let parts = reindexing(&p, &clock, &abort);
    let (site, clears) = Clears::site(ClearEnd::Written);
    bounded(move || {
        let (mut chain, run) = reindex_run(parts, site);
        assert!(run.stopped, "startup's ABC answered Ok");
        assert_eq!(run.finished, [(false, false)], "after the clear, before the retry");
        assert_eq!(clears.load(Ordering::SeqCst), 1);
        let v = chain.handle().clone();
        assert_eq!(v.query(|_| ()).recv(), Ok(()));
        let snap = Arc::clone(&v.view().snap);
        assert!(snap.has_tip() && snap.tip.tip_height == 0, "genesis connected");
        assert!(!v.loading().reindex() && !v.loading().importing());
        chain.stop();
        chain.flush_background_callbacks();
    });
    assert!(abort.messages().is_empty());
}

/// A clear whose own job fails (its write, Core's `dbwrapper_error` throw that ends Core's
/// process, AD-CH5-3): V answers `Err` and stops, and the import ends there (OD-CH5-76): no hook,
/// no "Reindexing finished", no genesis retry, no startup's ABC and so no
/// `-stopafterblockimport`, and `fImporting` cleared with `InitializeCurrentBlockTip` sent.
// Core: node/blockstorage.cpp:922-927; txdb.cpp:188-193; dbwrapper.cpp:261-268 at v24.0.0
#[test]
fn a_clear_answered_err_ends_the_import() {
    let p = regtest();
    let clock = clock_at(&p);
    let abort = Arc::new(RecordingAbort::default());
    let parts = reindexing(&p, &clock, &abort);
    let (site, clears) = Clears::site(ClearEnd::Failed);
    bounded(move || {
        let (mut chain, run) = reindex_run(parts, site);
        assert!(!run.stopped, "no ABC, no stop call");
        assert_eq!(run.finished, []);
        assert_eq!(clears.load(Ordering::SeqCst), 1);
        assert!(chain.handle().loading().reindex(), "nothing cleared");
        assert!(!chain.handle().loading().importing(), "after_import ran");
        assert!(!chain.handle().view().snap.has_tip());
        chain.stop();
        chain.flush_background_callbacks();
    });
    assert_eq!(abort.messages(), [CLEAR_FAILED]);
}

/// A clear V drops (V stopped on an `AbortNode` met before the clear ran, where Core's write
/// would succeed): no hook and no "Reindexing finished", and the import goes on to the retry and
/// startup's ABC, which V's reaper answers `Ok`, so `-stopafterblockimport` still runs (CH3b
/// D241, OD-CH5-77).
// Core: node/blockstorage.cpp:923-962 at v24.0.0
#[test]
fn a_clear_v_drops_goes_on_to_startups_abc_without_the_hook() {
    let p = regtest();
    let clock = clock_at(&p);
    let abort = Arc::new(RecordingAbort::default());
    let parts = reindexing(&p, &clock, &abort);
    let (site, clears) = Clears::site(ClearEnd::Before);
    bounded(move || {
        let (mut chain, run) = reindex_run(parts, site);
        assert!(run.stopped, "the reaper's Ok");
        assert_eq!(run.finished, []);
        assert_eq!(clears.load(Ordering::SeqCst), 1);
        assert!(chain.handle().loading().reindex(), "nothing cleared");
        assert!(!chain.handle().view().snap.has_tip());
        chain.stop();
        chain.flush_background_callbacks();
    });
    assert_eq!(abort.messages(), [FAILED_BEFORE]);
}

/// A clock a day past `p`'s genesis.
pub(crate) fn clock_at(p: &ChainSetup) -> Arc<dyn Clock> {
    let genesis = genesis_block(p);
    Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400))
}
