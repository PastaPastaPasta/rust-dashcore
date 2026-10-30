//! Headers and blocks through V (N1, N2): `ProcessNewBlockHeaders`, `ProcessNewBlock`'s accept
//! half, `AcceptBlock` and `ReceivedBlockTransactions`, with `-checkblockindex` after each step.

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_chain::{ChainConfig, FlatFilePos, HeaderChain, LoadError, Status, StoreError, Work};
use dash_consensus::primitives::Header;
use dash_consensus::{Block, BlockValidationResult};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::BlockHash;

use super::support::{
    Ev, Mined, Rig, RigConfig, SPACING, SharedFiles, accept_levels, batch, branch, cb_script,
    child, coinbase, height_push, install_accept_log, levels_here, mine, services, sorted,
};
use crate::chain::{
    BlockFiles, BlockState, ChainHandle, LoadFailure, LoadedIndex, LocalShutdown,
    MIN_BLOCKS_TO_KEEP, MemBlockFiles, NodeBlockIndex, RecordingAbort, ShutdownFlag, StartError,
    StoredRecord, WRONG_DEVNET_DATADIR, WRONG_NETWORK_DATADIR, WaitError, devnet_genesis_block,
    genesis_block, load_genesis_block, open_block_index,
};

#[test]
fn headers_signal_each_insert_then_the_header_tip() {
    let rig = Rig::new();
    let chain = branch(&rig.p, &rig.genesis, 3, 0);
    rig.headers(&chain);
    let [a, b, c] = rig.ids(&chain)[..] else {
        unreachable!()
    };
    use Ev::*;
    assert_eq!(rig.rec.take(), [Header(a), Header(b), Header(c), HeaderTip(c, true)]);
    assert_eq!(rig.v.view().snap.tip.best_header, c);
    // Known headers: no insert, no header-tip move, so no signal.
    rig.headers(&chain);
    assert_eq!(rig.rec.take(), []);
}

/// Core returns at the first failed header, before `NotifyHeaderTip` (R1-m3); the next good
/// batch notifies the moved best header.
#[test]
fn a_failed_batch_does_not_notify_the_header_tip() {
    let rig = Rig::new();
    let chain = branch(&rig.p, &rig.genesis, 3, 0);
    let mut bad = *chain[1].header().raw();
    bad.bits = 0x1d00_ffff;
    let mut b = batch(&chain[..2]);
    b.headers[1] = Header::new(bad);
    b.hashes[1] = b.headers[1].hash();
    let r = rig.v.process_headers(b, None).recv().expect("V replies");
    assert!(r.stop.is_some());
    let a = rig.id(&chain[0]);
    assert_eq!(rig.rec.take(), [Ev::Header(a)]);
    assert_eq!(rig.v.view().snap.tip.best_header, a, "the scope published the best header");
    rig.headers(&chain[1..]);
    let [b, c] = rig.ids(&chain[1..])[..] else {
        unreachable!()
    };
    assert_eq!(rig.rec.take(), [Ev::Header(b), Ev::Header(c), Ev::HeaderTip(c, true)]);
}

/// In order: each block is stored, linked and connected; `nChainTx` counts every block's
/// transactions from genesis; sequence ids count up from genesis's 1; the tip is the only
/// candidate left (`PruneBlockIndexCandidates`).
#[test]
fn blocks_in_order_are_linked_and_candidates() {
    let rig = Rig::new();
    let chain = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &chain {
        assert!(rig.accept(m).new_block);
    }
    let ids = rig.ids(std::iter::once(&rig.genesis).chain(&chain));
    let (chain_tx, seq, cands) = rig.tables(&ids);
    assert_eq!(chain_tx, [1, 2, 3, 4]);
    assert_eq!(seq, [1, 2, 3, 4]);
    assert_eq!(cands, [ids[3]]);
    assert_eq!(rig.v.view().snap.tip.tip, ids[3]);
    for m in &chain {
        let s = rig.status(m);
        assert!(s.has(Status::HAVE_DATA) && s.is_valid(Status::VALID_SCRIPTS), "{s:?}");
    }
    rig.assert_no_abort();
}

/// Out of IBD: `NewPoWValidBlock` fires for a block on the tip only (Core's
/// `m_chain.Tip() == pindex->pprev`), after its header signals; its ABC then connects it
/// (`BlockChecked`). A sibling of the new tip gets neither.
#[test]
fn new_pow_valid_block_only_on_the_tip() {
    let rig = Rig::with(RigConfig {
        after_genesis: Some(3_600),
        ..RigConfig::default()
    });
    let a = child(&rig.p, &rig.genesis, 0);
    let s = child(&rig.p, &rig.genesis, 1);
    rig.accept(&a);
    rig.accept(&s);
    let [ai, si] = rig.ids([&a, &s])[..] else {
        unreachable!()
    };
    use Ev::*;
    assert_eq!(
        rig.rec.take(),
        [
            Header(ai),
            PowValid(ai),
            HeaderTip(ai, false),
            Checked(a.hash(), BlockState::Valid),
            Header(si)
        ]
    );
}

/// Core's `m_blocks_unlinked`: blocks whose parent has no data wait, then link in Core's queue
/// order when it arrives.
#[test]
fn out_of_order_blocks_link_when_the_gap_fills() {
    let rig = Rig::new();
    let chain = branch(&rig.p, &rig.genesis, 3, 0);
    rig.headers(&chain);
    let ids = rig.ids(std::iter::once(&rig.genesis).chain(&chain));
    rig.accept(&chain[2]);
    rig.accept(&chain[1]);
    assert_eq!(rig.idx(|ix| sorted(ix.unlinked())), [(ids[1], ids[2]), (ids[2], ids[3])]);
    assert_eq!(rig.tables(&ids), (vec![1, 0, 0, 0], vec![1, 0, 0, 0], vec![ids[0]]));
    rig.accept(&chain[0]);
    assert_eq!(rig.idx(|ix| ix.unlinked().count()), 0);
    let (tx, seq, cands) = rig.tables(&ids);
    assert_eq!(tx, [1, 2, 3, 4]);
    assert_eq!(seq, [1, 2, 3, 4], "the queue visits parent before child");
    assert_eq!(cands, [ids[3]], "connected through to the top");
    rig.assert_no_abort();
}

/// Siblings with equal work: the earlier-received one is the better candidate (Core's
/// comparator, smaller `nSequenceId`).
#[test]
fn equal_work_candidates_order_by_arrival() {
    let rig = Rig::new();
    let x = child(&rig.p, &rig.genesis, 1);
    let y = child(&rig.p, &rig.genesis, 2);
    rig.accept(&y);
    rig.accept(&x);
    let [yi, xi] = rig.ids([&y, &x])[..] else {
        unreachable!()
    };
    assert_eq!(rig.idx(|ix| ix.best_candidate()), Some(yi));
    assert_eq!(rig.tables(&[yi, xi]).1, [2, 3]);
}

#[test]
fn a_known_block_is_accepted_without_a_write() {
    let rig = Rig::new();
    let a = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a);
    assert!(!rig.accept(&a).new_block);
}

/// `AcceptBlock`'s unrequested-block gates (`validation.cpp:4804-4815`): too far ahead of the
/// tip, and below `-minimumchainwork`; both "accepted" without a write.
#[test]
fn unrequested_blocks_too_far_ahead_are_not_stored() {
    let rig = Rig::new();
    let chain = branch(&rig.p, &rig.genesis, MIN_BLOCKS_TO_KEEP + 1, 0);
    rig.headers(&chain);
    let far = chain.last().expect("a block");
    let r = rig.submit_unrequested(far);
    assert!(r.accepted && !r.new_block, "{r:?}");
    assert!(!rig.status(far).has(Status::HAVE_DATA));
    let near = &chain[MIN_BLOCKS_TO_KEEP as usize - 1];
    let r = rig.submit_unrequested(near);
    assert!(r.accepted && r.new_block, "height {} is within reach: {r:?}", near.height);
}

/// Sol cp1 M2: an unrequested block with less work than the tip is not stored
/// (`validation.cpp:4807`); one with the same work is, even unlinked.
#[test]
fn unrequested_blocks_with_less_work_than_the_tip_are_not_stored() {
    let rig = Rig::new();
    for m in &branch(&rig.p, &rig.genesis, 2, 0) {
        rig.accept(m);
    }
    let side = branch(&rig.p, &rig.genesis, 2, 1);
    rig.headers(&side);
    let r = rig.submit_unrequested(&side[0]);
    assert!(r.accepted && !r.new_block, "less work than the tip: {r:?}");
    assert!(!rig.status(&side[0]).has(Status::HAVE_DATA));
    let r = rig.submit_unrequested(&side[1]);
    assert!(r.accepted && r.new_block, "the tip's work: {r:?}");
    assert!(rig.status(&side[1]).has(Status::HAVE_DATA));
    let (s0, s1) = (rig.id(&side[0]), rig.id(&side[1]));
    assert_eq!(rig.idx(move |ix| ix.unlinked_children(s0).to_vec()), [s1]);
    // Requested, the less-work block is stored and links its child.
    assert!(rig.accept(&side[0]).new_block);
}

/// Sol cp1 #4: each new header logs Core's `Saw new header` line (`validation.cpp:4706-4721`),
/// at debug level in IBD and by default after: the headers of a batch, and a block whose header
/// `AcceptBlock` saw first; a known header logs nothing.
#[test]
fn new_headers_log_the_saw_new_header_line() {
    let lines =
        |m: &Mined| accept_levels(&format!("Saw new header hash={} height={}", m.hash(), m.height));
    install_accept_log();
    // Salts no other test mines with, as the log is the process's.
    let rig = Rig::new();
    let chain = branch(&rig.p, &rig.genesis, 3, 4_242);
    rig.headers(&chain[..2]);
    rig.headers(&chain[..2]);
    rig.accept(&chain[2]);
    for m in &chain {
        assert_eq!(lines(m), [tracing::Level::DEBUG], "IBD, height {}", m.height);
    }

    let rig = Rig::with(RigConfig {
        after_genesis: Some(600),
        ..RigConfig::default()
    });
    let a = child(&rig.p, &rig.genesis, 4_243);
    rig.accept(&a);
    assert_eq!(lines(&a), [tracing::Level::INFO], "after IBD");
}

#[test]
fn unrequested_blocks_below_minimum_chain_work_are_not_stored() {
    let rig = Rig::with(RigConfig {
        min_chain_work: Some(Work::from_u64(1 << 40)),
        ..RigConfig::default()
    });
    let a = child(&rig.p, &rig.genesis, 0);
    let r = rig.submit_unrequested(&a);
    assert!(r.accepted && !r.new_block, "{r:?}");
    // Requested, the same block is stored.
    assert!(rig.accept(&a).new_block);
}

/// A contextual failure marks the entry `FAILED_VALID` (`validation.cpp:4822-4825`); the block
/// is reported to `BlockChecked`.
#[test]
fn a_contextual_failure_marks_the_entry_failed() {
    let rig = Rig::new();
    let g = &rig.genesis;
    // BIP34 (regtest height 1): the coinbase must push height 1, not 2.
    let block = mine(&rig.p, g.hash(), g.time() + SPACING, vec![coinbase(&cb_script(2, 0))]);
    let m = Mined {
        block,
        height: 1,
    };
    let r = rig.submit(&m);
    assert!(!r.accepted);
    let BlockState::Invalid(e) = &r.state else {
        panic!("{r:?}")
    };
    assert_eq!(e.result(), BlockValidationResult::Consensus);
    assert_eq!(e.reject_reason(), "bad-cb-height");
    assert!(rig.status(&m).has(Status::FAILED_VALID));
    let id = rig.id(&m);
    assert_eq!(rig.rec.take(), [Ev::Header(id), Ev::Checked(m.hash(), r.state.clone())]);
    assert!(!rig.idx(move |ix| ix.is_candidate(id)));
    rig.assert_no_abort();
}

/// A mutated block fails `CheckBlock` before `AcceptBlock`: no entry, nothing marked.
#[test]
fn a_mutated_block_leaves_no_trace() {
    let rig = Rig::new();
    let a = child(&rig.p, &rig.genesis, 0);
    let txs = vec![a.block.txs()[0].clone(), coinbase(&cb_script(1, 9))];
    let mutated = Mined {
        block: Arc::new(Block::new(a.header(), txs)),
        height: 1,
    };
    let r = rig.submit(&mutated);
    let BlockState::Invalid(e) = &r.state else {
        panic!("{r:?}")
    };
    assert_eq!(e.result(), BlockValidationResult::Mutated);
    assert!(rig.v.reader().lookup(&a.hash()).is_none());
    assert_eq!(rig.rec.take(), [Ev::Checked(a.hash(), r.state.clone())]);
    // The real block is still accepted.
    rig.accept(&a);
}

/// Block files that fail after genesis: no position (`full`), or an I/O error. Genesis is
/// stored, for init's ABC to read.
struct Failing {
    full: bool,
    genesis: Option<Arc<Block>>,
}

impl BlockFiles for Failing {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        _: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        match (height, self.full) {
            (0, _) => {
                self.genesis = Some(block.clone());
                Ok(Some(FlatFilePos {
                    file: 0,
                    pos: 8,
                }))
            }
            (_, true) => Ok(None),
            (_, false) => Err("disk on fire".into()),
        }
    }

    fn read_block(&self, _: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.genesis.clone().filter(|g| g.hash() == *hash).ok_or_else(|| "not genesis".into())
    }

    fn open_block_file(&self, file: i32) -> bool {
        file == 0 && self.genesis.is_some()
    }

    fn current_usage(&self) -> u64 {
        0
    }
}

fn failing(full: bool) -> Rig {
    Rig::with(RigConfig {
        files: Some(Box::new(Failing {
            full,
            genesis: None,
        })),
        ..RigConfig::default()
    })
}

/// No position is `state.Error` (the block fails, V runs on); an I/O failure is `AbortNode`.
#[test]
fn block_file_failures() {
    let rig = failing(true);
    let a = child(&rig.p, &rig.genesis, 0);
    let r = rig.submit(&a);
    assert!(!r.accepted && r.new_block, "{r:?}");
    assert!(matches!(&r.state, BlockState::Error(m) if m.contains("Failed to find position")));
    rig.assert_no_abort();

    let mut rig = failing(false);
    let r = rig.v.process_block(a.block.clone(), true, None, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0), Some("System error: disk on fire".into()));
    assert_eq!(rig.abort.messages(), ["System error: disk on fire"]);
}

#[test]
fn coinbase_height_pushes_match_core() {
    assert_eq!(height_push(1), [0x51]);
    assert_eq!(height_push(16), [0x60]);
    assert_eq!(height_push(17), [1, 17]);
    assert_eq!(height_push(128), [2, 0x80, 0]);
    assert_eq!(height_push(300), [2, 0x2c, 1]);
}

pub(super) fn devnet() -> Arc<ChainParams> {
    devnet_named("ch1")
}

fn devnet_named(name: &str) -> Arc<ChainParams> {
    Arc::new(
        ChainParamsBuilder::new(NetworkKind::Devnet, Release::V24_0)
            .devnet_name(name)
            .build()
            .expect("devnet"),
    )
}

/// D18, Core's `LoadGenesisBlock` on a devnet (`v:5408-5416`): V accepts the devnet genesis
/// block as a requested block before it serves anything, and init's ABC connects it at height
/// one. Core's step runs only on an empty index, so a restart on that index does not repeat
/// it. `-checkblockindex` is off, a devnet's default (D48).
#[test]
fn a_devnet_starts_on_its_devnet_genesis_block() {
    let p = devnet();
    let dg = devnet_genesis_block(&p).expect("a devnet");
    let files = SharedFiles::default();
    let (sent, before_abc) = std::sync::mpsc::channel();
    let mut rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        files: Some(Box::new(files.clone())),
        no_check_block_index: true,
        // Once V serves, the step's scope has published the block as the best header.
        before_init: Some(Box::new(move |v: &ChainHandle| {
            v.query(|_| ()).recv().expect("V serves");
            sent.send(v.view().snap.tip.best_header).expect("the test waits");
        })),
        ..RigConfig::default()
    });
    let id = rig.v.reader().lookup(&dg.hash()).expect("accepted");
    assert_eq!(before_abc.recv(), Ok(id), "published before init's ABC");
    let e = rig.v.reader().entry(id).expect("held");
    assert_eq!((e.height, e.prev), (1, Some(rig.id(&rig.genesis))));
    assert!(e.status.has(Status::HAVE_DATA) && e.n_tx == 1);
    assert_eq!(rig.v.view().snap.tip.tip, id, "init's ABC connected it");
    rig.assert_no_abort();
    let exit = rig.stop();
    assert_eq!(exit.chain.ids().count(), 2);

    // M-2: the datadir is this devnet's, so it opens and the step does not repeat. The
    // restart's -checkblockindex runs with an active chain.
    let mut files = Box::new(files);
    let (chain, idx) =
        open_loaded(p.clone(), exit.chain, &mut *files).expect("this devnet's index");
    let cfg = RigConfig {
        params: Some(p),
        ..RigConfig::default()
    };
    let rig = Rig::reopen(chain, idx, files, id, cfg).expect("restarts");
    assert_eq!(rig.v.view().snap.tip.tip, id);
    assert_eq!(rig.v.reader().lookup(&dg.hash()), Some(id));
    rig.assert_tables();
    rig.assert_no_abort();
}

/// D48: with `-checkblockindex` on, a fresh devnet fails Core's assert in `AcceptBlock`'s
/// `CheckBlockIndex` (`v:4778`, `v:5618-5619`): two entries and no active chain. V stops before
/// init's ABC with its port of that assert.
#[test]
fn a_fresh_devnet_fails_cores_check_block_index_assert() {
    let abort = Arc::new(RecordingAbort::default());
    let started = Rig::try_with(RigConfig {
        params: Some(devnet()),
        abort: Some(abort.clone()),
        ..RigConfig::default()
    });
    assert!(matches!(started, Err(StartError::Load(LoadFailure::Failure { .. }))));
    assert_eq!(
        abort.messages(),
        ["CheckBlockIndex: no active chain, yet more than the genesis entry at entry 1"]
    );
}

/// Block files with no position for any block at `height` (Core's null `FlatFilePos`).
struct NoRoomAt(u32, MemBlockFiles);

impl BlockFiles for NoRoomAt {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        if height == self.0 {
            return Ok(None);
        }
        self.1.save_block(block, height, known)
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.1.read_block(pos, hash)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.1.open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.1.current_usage()
    }
}

/// D48 (N-5): `AcceptBlock` rejects the devnet genesis block (no room to write it), so
/// `LoadGenesisBlock` returns `false`: init's error ("Error initializing block database",
/// `node/chainstate.cpp:210-211`), not `AbortNode`. V stops before init's ABC.
#[test]
fn a_rejected_devnet_genesis_block_is_an_init_error() {
    let abort = Arc::new(RecordingAbort::default());
    let shutdown = Arc::new(LocalShutdown::default());
    let started = Rig::try_with(RigConfig {
        params: Some(devnet()),
        files: Some(Box::new(NoRoomAt(1, MemBlockFiles::default()))),
        no_check_block_index: true,
        abort: Some(abort.clone()),
        shutdown: Some(shutdown.clone()),
        ..RigConfig::default()
    });
    let Err(StartError::Load(failure)) = started else {
        panic!("init's error")
    };
    assert_eq!(
        failure,
        LoadFailure::Failure {
            cause: None
        }
    );
    assert_eq!(failure.to_string(), "Error initializing block database");
    assert!(abort.messages().is_empty(), "no AbortNode");
    // D52: V's own stop, not the node's `StartShutdown()`, so init asks Core's reindex question.
    assert!(!shutdown.shutdown_requested());
}

/// D48 (N-5): a write error in the devnet step is `AcceptBlock`'s `AbortNode`, and then init's
/// error as well, as in Core.
#[test]
fn a_devnet_genesis_block_write_error_aborts_then_fails_init() {
    let abort = Arc::new(RecordingAbort::default());
    let started = Rig::try_with(RigConfig {
        params: Some(devnet()),
        files: Some(Box::new(Failing {
            full: false,
            genesis: None,
        })),
        no_check_block_index: true,
        abort: Some(abort.clone()),
        ..RigConfig::default()
    });
    assert!(matches!(
        started,
        Err(StartError::Load(LoadFailure::Failure {
            cause: None
        }))
    ));
    assert_eq!(abort.messages(), ["System error: disk on fire"]);
}

/// A devnet's index after its devnet step, stopped: V's chain as SB4 would load it.
fn devnet_index(p: &Arc<ChainParams>) -> (HeaderChain, Box<dyn BlockFiles>) {
    let files = SharedFiles::default();
    let mut rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        files: Some(Box::new(files.clone())),
        no_check_block_index: true,
        ..RigConfig::default()
    });
    (rig.stop().chain, Box::new(files))
}

/// M-2 (D48): Core's datadir check (`node/chainstate.cpp:195-198`). A devnet's index opened
/// under another `-devnet` name lacks that devnet's genesis block (all devnets share block 0),
/// so init fails with Core's `FAILURE_INCOMPATIBLE_DB` text.
#[test]
fn a_devnet_index_opened_as_another_devnet_is_refused() {
    let (chain, mut files) = devnet_index(&devnet());
    let opened = open_loaded(devnet_named("ch2"), chain, &mut *files);
    assert_eq!(opened.err(), Some(LoadFailure::Incompatible(WRONG_DEVNET_DATADIR)));
}

/// M-2 (D48): an index holding genesis but not the devnet genesis block (the step never
/// completed: a reused or damaged datadir) is refused, not loaded with the step skipped.
#[test]
fn an_index_without_its_devnet_genesis_block_is_refused() {
    let p = devnet();
    let mut files = MemBlockFiles::default();
    let (chain, _) = load_genesis_block(p.clone(), ChainConfig::node(&p), services(&p), &mut files)
        .expect("genesis");
    let opened = open_loaded(p, chain, &mut files);
    assert_eq!(opened.err(), Some(LoadFailure::Incompatible(WRONG_DEVNET_DATADIR)));
}

/// Core's check before it (`node/chainstate.cpp:188-193`): another network's index, with its
/// block files (a devnet's block 0 is regtest's genesis, so the devnet check is the one that
/// tells those apart).
#[test]
fn another_networks_index_is_refused() {
    let (_, chain, mut files) = stopped_index();
    let opened = open_loaded(mainnet(), chain, &mut files);
    assert_eq!(opened.err(), Some(LoadFailure::Incompatible(WRONG_NETWORK_DATADIR)));
}

/// `open_block_index` on a loaded index, with shutdown never requested.
fn open_loaded(
    p: Arc<ChainParams>,
    chain: HeaderChain,
    files: &mut dyn BlockFiles,
) -> Result<(HeaderChain, NodeBlockIndex), LoadFailure> {
    open_block_index(p, LoadedIndex::Loaded(chain), files, &LocalShutdown::default())
}

fn mainnet() -> Arc<ChainParams> {
    Arc::new(
        ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().expect("mainnet"),
    )
}

/// The rig's block files, where `.1` decides whether a file opens.
struct OpenWith<F>(SharedFiles, F);

impl<F: Fn(&SharedFiles, i32) -> bool + Send> BlockFiles for OpenWith<F> {
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
        (self.1)(&self.0, file)
    }

    fn current_usage(&self) -> u64 {
        self.0.current_usage()
    }
}

/// The rig's block files with file `lost` deleted.
fn lost(files: SharedFiles, lost: i32) -> impl BlockFiles {
    OpenWith(files, move |f: &SharedFiles, file| file != lost && f.open_block_file(file))
}

/// A stopped rig's index with two blocks in file 0, and its block files.
fn stopped_index() -> (Arc<ChainParams>, HeaderChain, SharedFiles) {
    let files = SharedFiles::default();
    let mut rig = Rig::with(RigConfig {
        files: Some(Box::new(files.clone())),
        ..RigConfig::default()
    });
    for m in &branch(&rig.p, &rig.genesis, 2, 0) {
        rig.accept(m);
    }
    let p = rig.p.clone();
    (p, rig.stop().chain, files)
}

/// BL-127 (D53): Core's `LoadBlockIndexDB` opens every block file that holds a block's data
/// (`node/blockstorage.cpp:377-390`); one that does not open is `LoadBlockIndex`'s `false`,
/// init's "Error loading block database" (`node/chainstate.cpp:183-185`).
#[test]
fn a_block_file_that_does_not_open_fails_the_load() {
    install_accept_log();
    let (p, chain, files) = stopped_index();
    let failure = open_loaded(p, chain, &mut lost(files, 0)).err().expect("refused");
    assert_eq!(failure, LoadFailure::LoadingBlockDb);
    assert_eq!(failure.to_string(), "Error loading block database");
    assert!(!accept_levels("Checking all blk files are present...").is_empty());
}

/// BL-127: only entries with `BLOCK_HAVE_DATA` name a file to check; a header's file is not
/// opened, as Core's `nStatus & BLOCK_HAVE_DATA` filter skips it.
#[test]
fn only_files_holding_block_data_are_checked() {
    let files = SharedFiles::default();
    let mut rig = Rig::with(RigConfig {
        files: Some(Box::new(files.clone())),
        no_check_block_index: true,
        ..RigConfig::default()
    });
    let a = child(&rig.p, &rig.genesis, 0);
    rig.headers(std::slice::from_ref(&a));
    let id = rig.id(&a);
    rig.on_v(move |v| {
        v.chain.node_index()?.modify(id, |g| g.file = Some(5));
        Ok(())
    });
    let p = rig.p.clone();
    let chain = rig.stop().chain;
    let opened = open_loaded(p, chain, &mut lost(files, 5));
    assert!(opened.is_ok(), "{:?}", opened.err());
}

/// D53 (Q3): Core's order. `LoadBlockIndex`, the block-file check included, comes before the
/// datadir checks (`node/chainstate.cpp:183` before `:188`), so another network's index whose
/// block files are gone fails the load, not the network check.
#[test]
fn a_lost_block_file_is_found_before_the_network_check() {
    let (_, chain, files) = stopped_index();
    let opened = open_loaded(mainnet(), chain, &mut lost(files, 0));
    assert_eq!(opened.err(), Some(LoadFailure::LoadingBlockDb));
}

/// D53: shutdown requested before the load is Core's INTERRUPTED (`node/chainstate.cpp:177`),
/// for an empty index too.
#[test]
fn a_shutdown_before_the_load_interrupts_it() {
    let shutdown = LocalShutdown::default();
    shutdown.start_shutdown();
    let (p, chain, mut files) = stopped_index();
    let opened = open_block_index(p.clone(), LoadedIndex::Loaded(chain), &mut files, &shutdown);
    assert_eq!(opened.err(), Some(LoadFailure::Interrupted));
    let empty = LoadedIndex::Empty {
        cfg: ChainConfig::node(&p),
        svc: services(&p),
    };
    let opened = open_block_index(p, empty, &mut MemBlockFiles::default(), &shutdown);
    assert_eq!(opened.err(), Some(LoadFailure::Interrupted));
}

/// D53: Core's two index passes each ask `ShutdownRequested()` before each entry
/// (`node/blockstorage.cpp:296`, `validation.cpp:5342`): a shutdown during the second stops it.
#[test]
fn a_shutdown_during_the_index_passes_stops_them() {
    let (_, chain, _) = stopped_index();
    let asked = std::cell::Cell::new(0);
    // Three entries: the first pass asks three times, the second stops at its second.
    let loaded = NodeBlockIndex::load_until(&chain, || {
        asked.set(asked.get() + 1);
        asked.get() > 4
    });
    assert!(loaded.is_none());
    assert_eq!(asked.get(), 5, "asked before each entry of each pass");
    assert!(NodeBlockIndex::load_until(&chain, || false).is_some());
}

/// D53: a load that fails with shutdown requested by then is INTERRUPTED, not "Error loading
/// block database" (`node/chainstate.cpp:183-184`): here a signal arrives as the check fails.
#[test]
fn a_failed_load_with_shutdown_requested_is_interrupted() {
    let (p, chain, files) = stopped_index();
    let shutdown = LocalShutdown::default();
    let mut checking = OpenWith(files, |_: &SharedFiles, _| {
        shutdown.start_shutdown();
        false
    });
    let opened = open_block_index(p, LoadedIndex::Loaded(chain), &mut checking, &shutdown);
    assert_eq!(opened.err(), Some(LoadFailure::Interrupted));
}

/// D53 (validator L-5): a signal during a check that passes still interrupts the load, as
/// Core's second pass asks `ShutdownRequested()` before its first entry (`validation.cpp:5342`).
#[test]
fn a_shutdown_during_a_passing_check_interrupts_the_load() {
    let (p, chain, files) = stopped_index();
    let shutdown = LocalShutdown::default();
    let mut checking = OpenWith(files, |f: &SharedFiles, file| {
        shutdown.start_shutdown();
        f.open_block_file(file)
    });
    let opened = open_block_index(p, LoadedIndex::Loaded(chain), &mut checking, &shutdown);
    assert_eq!(opened.err(), Some(LoadFailure::Interrupted));
}

/// The index records `chain` persists, in the key order SB4's store yields them
/// (`DB_BLOCK_INDEX` + hash).
fn persisted(chain: &mut HeaderChain) -> Vec<(BlockHash, Vec<u8>)> {
    let writes = chain.node_index().expect("a node chain").take_dirty();
    let mut recs: Vec<_> = writes.into_iter().map(|w| (w.hash, w.bytes)).collect();
    recs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    recs
}

/// `open_block_index` on stored records (D54), on network `p`.
fn open_stored<'a>(
    p: Arc<ChainParams>,
    records: impl Iterator<Item = StoredRecord> + 'a,
    pool: &'a rayon::ThreadPool,
    files: &mut dyn BlockFiles,
    shutdown: &dyn ShutdownFlag,
) -> Result<(HeaderChain, NodeBlockIndex), LoadFailure> {
    let stored = LoadedIndex::Stored {
        cfg: ChainConfig::node(&p),
        svc: services(&p),
        records: Box::new(records),
        pool,
    };
    open_block_index(p, stored, files, shutdown)
}

fn pool() -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new().num_threads(2).build().expect("a test pool")
}

fn testnet() -> Arc<ChainParams> {
    Arc::new(
        ChainParamsBuilder::new(NetworkKind::Testnet, Release::V24_0).build().expect("testnet"),
    )
}

/// D54 (Sol r1 M-1): Core's order on a datadir's stored index. `LoadBlockIndexGuts` has no
/// network check (`txdb.cpp:300-344`), so a mainnet index opened as testnet loads; its block
/// files are checked next (`node/blockstorage.cpp:377-390`) and the network last
/// (`node/chainstate.cpp:188-193`). With its genesis block's file there, the datadir is
/// another network's; with it gone, the load fails first.
#[test]
fn a_stored_index_of_another_network_is_checked_for_files_then_network() {
    let main = mainnet();
    let files = SharedFiles::default();
    let (mut chain, _) =
        load_genesis_block(main.clone(), ChainConfig::node(&main), services(&main), &mut {
            files.clone()
        })
        .expect("a mainnet genesis");
    let recs = persisted(&mut chain);
    assert_eq!(recs.len(), 1);
    let pool = pool();
    let never = LocalShutdown::default();

    let opened =
        open_stored(testnet(), recs.clone().into_iter().map(Ok), &pool, &mut files.clone(), &never);
    assert_eq!(opened.err(), Some(LoadFailure::Incompatible(WRONG_NETWORK_DATADIR)));
    let opened =
        open_stored(testnet(), recs.into_iter().map(Ok), &pool, &mut lost(files, 0), &never);
    let failure = opened.err().expect("refused");
    assert_eq!(failure, LoadFailure::LoadingBlockDb);
    assert_eq!(failure.to_string(), "Error loading block database");
}

/// D54: stored records of this network open as the loaded index does, and none is an empty
/// index (`LoadGenesisBlock`).
#[test]
fn stored_records_open_as_the_loaded_index() {
    let (p, mut chain, files) = stopped_index();
    let recs = persisted(&mut chain);
    let pool = pool();
    let never = LocalShutdown::default();
    let opened =
        open_stored(p.clone(), recs.into_iter().map(Ok), &pool, &mut files.clone(), &never);
    let (opened, _) = opened.expect("opens");
    let held = |c: &HeaderChain| -> BTreeSet<BlockHash> {
        c.ids().map(|id| c.reader().entry(id).expect("set").hash).collect()
    };
    assert_eq!(held(&opened), held(&chain));

    let mut fresh = MemBlockFiles::default();
    let opened = open_stored(p.clone(), std::iter::empty(), &pool, &mut fresh, &never);
    let (opened, _) = opened.expect("a new chain");
    let genesis = opened.reader().entry(opened.best_header()).expect("set").hash;
    assert_eq!(genesis, genesis_block(&p).hash());
}

/// D54 (Sol r1 M-2): `LoadBlockIndexGuts` asks `ShutdownRequested()` before each entry
/// (`txdb.cpp:307-309`): a shutdown requested between two records stops the load before the
/// next read, as INTERRUPTED, so that read's error is never reported.
#[test]
fn a_shutdown_between_stored_records_interrupts_before_the_next_read() {
    install_accept_log();
    let (p, mut chain, files) = stopped_index();
    let recs = persisted(&mut chain);
    assert!(recs.len() > 2);
    let shutdown = LocalShutdown::default();
    let reads = std::cell::Cell::new(0);
    let mut it = recs.into_iter();
    let controlled = std::iter::from_fn(|| {
        reads.set(reads.get() + 1);
        if reads.get() == 1 {
            let first = it.next().map(Ok);
            shutdown.start_shutdown();
            first
        } else {
            Some(Err(LoadError::Read(StoreError::Io("the next read".into()))))
        }
    });
    let pool = pool();
    let opened = open_stored(p, controlled, &pool, &mut files.clone(), &shutdown);
    assert_eq!(opened.err(), Some(LoadFailure::Interrupted));
    assert_eq!(reads.get(), 1, "no read after the request");
    // Core's `return false` on shutdown logs nothing.
    assert!(levels_here("LoadBlockIndexGuts: the block index load was interrupted").is_empty());
}

/// D54: a stored record that cannot be read throws in Core, which init reports as "Error
/// opening block database" with its cause; one that reads but does not decode is
/// `LoadBlockIndexGuts`' false, "Error loading block database".
#[test]
fn a_stored_record_read_error_fails_the_open() {
    install_accept_log();
    let (p, mut chain, files) = stopped_index();
    let recs = persisted(&mut chain);
    let pool = pool();
    let never = LocalShutdown::default();
    let failing = |e: StoreError| {
        recs.clone().into_iter().take(1).map(Ok).chain(std::iter::once(Err(LoadError::Read(e))))
    };
    let opened = open_stored(
        p.clone(),
        failing(StoreError::Io("disk".into())),
        &pool,
        &mut files.clone(),
        &never,
    );
    assert_eq!(
        opened.err(),
        Some(LoadFailure::Store {
            cause: "header store: disk".into()
        })
    );
    let opened = open_stored(
        p.clone(),
        failing(StoreError::Corrupt("row".into())),
        &pool,
        &mut files.clone(),
        &never,
    );
    assert_eq!(opened.err(), Some(LoadFailure::LoadingBlockDb));
    let logged = levels_here("LoadBlockIndexGuts: header store corrupt: row");
    assert_eq!(logged, [tracing::Level::ERROR]);
    let mut bad = recs.clone();
    bad[0].1.truncate(10);
    let opened = open_stored(p, bad.into_iter().map(Ok), &pool, &mut files.clone(), &never);
    assert_eq!(opened.err(), Some(LoadFailure::LoadingBlockDb));
}

/// The persisted records of `p`'s genesis alone.
fn genesis_records(p: &Arc<ChainParams>) -> Vec<(BlockHash, Vec<u8>)> {
    let mut files = MemBlockFiles::default();
    let (mut chain, _) =
        load_genesis_block(p.clone(), ChainConfig::node(p), services(p), &mut files)
            .expect("a genesis");
    persisted(&mut chain)
}

/// D54 (Opus r1 m-3): SB4's refusal of a second root holds in the staged load: testnet's index
/// holding mainnet's genesis too (both pass testnet's proof of work) fails the load, as SB4's
/// structural refusals do, and never reaches the network check it would pass.
#[test]
fn a_stored_index_with_two_roots_fails_the_load() {
    let test = testnet();
    let mut recs = genesis_records(&test);
    recs.extend(genesis_records(&mainnet()));
    recs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let pool = pool();
    let opened = open_stored(
        test,
        recs.into_iter().map(Ok),
        &pool,
        &mut MemBlockFiles::default(),
        &LocalShutdown::default(),
    );
    assert_eq!(opened.err(), Some(LoadFailure::LoadingBlockDb));
}

/// D54 (Opus r1 m-4): a stored index SB4's load refuses with shutdown requested by then is
/// INTERRUPTED (`node/chainstate.cpp:183-184`): a corrupt row read as the signal arrives, and an
/// undecodable record whose signal arrives as the scan ends, after the last poll.
#[test]
fn a_refused_stored_index_with_shutdown_requested_is_interrupted() {
    let (p, mut chain, files) = stopped_index();
    let recs = persisted(&mut chain);
    let pool = pool();

    let shutdown = LocalShutdown::default();
    let mut it = recs.clone().into_iter();
    let corrupt = std::iter::from_fn(|| {
        Some(it.next().map(Ok).unwrap_or_else(|| {
            shutdown.start_shutdown();
            Err(LoadError::Read(StoreError::Corrupt("row".into())))
        }))
    });
    let opened = open_stored(p.clone(), corrupt, &pool, &mut files.clone(), &shutdown);
    assert_eq!(opened.err(), Some(LoadFailure::Interrupted));

    let shutdown = LocalShutdown::default();
    let mut bad = recs;
    bad[0].1.truncate(10);
    let mut it = bad.into_iter();
    let undecodable = std::iter::from_fn(|| {
        let next = it.next().map(Ok);
        if next.is_none() {
            shutdown.start_shutdown();
        }
        next
    });
    let opened = open_stored(p, undecodable, &pool, &mut files.clone(), &shutdown);
    assert_eq!(opened.err(), Some(LoadFailure::Interrupted));
}

/// D53 (validator L-6, Opus r1 m-5): an empty index logs Core's blk-file check line too, as
/// `LoadBlockIndexDB` runs on a fresh datadir with no file to check: once for an empty store and
/// once for `Empty`.
#[test]
fn an_empty_index_logs_the_blk_file_check() {
    install_accept_log();
    let p = mainnet();
    let pool = pool();
    let never = LocalShutdown::default();
    let opened =
        open_stored(p.clone(), std::iter::empty(), &pool, &mut MemBlockFiles::default(), &never);
    assert!(opened.is_ok(), "{:?}", opened.err());
    let empty = LoadedIndex::Empty {
        cfg: ChainConfig::node(&p),
        svc: services(&p),
    };
    let opened = open_block_index(p, empty, &mut MemBlockFiles::default(), &never);
    assert!(opened.is_ok(), "{:?}", opened.err());
    let line = "Checking all blk files are present...";
    assert_eq!(levels_here(line), [tracing::Level::INFO, tracing::Level::INFO]);
}
