//! BL-636: the shutdown flushes of a start refused with coins that never reached V, one test per
//! row of DEC-1071's matrix.
// Core: node/chainstate.cpp:256-257, :272-276, :467-474; init.cpp:378-386, :449-456;
// validation.cpp:3052-3139; shutdown.cpp:27-38; dbwrapper.cpp:261-268

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use dash_chain::FlatFilePos;
use dashd_chainstate::chain::{BlockFiles, INIT_BLOCK_DB_FAILED};

use super::*;
use crate::shutdown::EXIT_FAILURE;
use crate::steps::StartFault;

type Lines = Arc<Mutex<Vec<String>>>;

/// The engine's WAL sync count as each shutdown flush starts.
type Syncs = Arc<Mutex<Vec<usize>>>;

/// The coins a start finds, from the directory it runs in.
type Fixture<'a> = dyn Fn(&Path) -> Arc<MemoryEngine> + 'a;

/// The first two blocks of [`left_ibd_at_b1`]'s chain.
type Blocks = (Arc<dash_consensus::Block>, Arc<dash_consensus::Block>);

const REGTEST: [&str; 2] = ["-regtest", "-printtoconsole=0"];
const BENCH: [&str; 4] = ["-regtest", "-printtoconsole=0", "-debug=bench", "-debug=coindb"];
const DEVNET: [&str; 3] = ["-devnet=nd1", "-listen=0", "-printtoconsole=0"];
const STARTED: &str = "FlushStateToDisk: write evodb cache to disk started\n";
const COMPLETED: &str = "FlushStateToDisk: write evodb cache to disk completed (";
/// The line [`arm`]'s probe logs as each shutdown flush starts.
const FLUSH_MARK: &str = "bl636: flush ";
const SYNC: &str = "memory error (Io): injected sync";

/// The evodb timer pairs in `lines`: the started lines and the completed ones.
pub(super) fn pairs(lines: &[String]) -> (usize, usize) {
    let started = lines.iter().filter(|l| l.ends_with(STARTED)).count();
    let completed = lines.iter().filter(|l| l.contains(COMPLETED) && l.ends_with("s)\n")).count();
    (started, completed)
}

/// Every report line in `lines`, as a flush's low disk or failed sync logs it.
fn reports(lines: &[String]) -> Vec<String> {
    let known = [&space_report()[..], &sync_report()].concat();
    failure_lines_for(SYNC, lines, &known).into_iter().map(str::to_owned).collect()
}

/// The lines from the first shutdown flush on.
fn from_first_flush(lines: &Lines) -> Vec<String> {
    let lines = lines.lock().unwrap();
    let at = lines.iter().position(|l| l.contains(FLUSH_MARK)).expect("a shutdown flush");
    lines[at..].to_vec()
}

/// A flush's report of the failed WAL sync: Core's failed synced write.
fn sync_report() -> Vec<String> {
    flush_failure_for(SYNC)
}

/// A fault at the refused start's shutdown, armed at its flushes, after genesis initialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// The blocks directory has no free space.
    BlocksLow,
    /// The blocks directory has no free space for the first flush only.
    BlocksLowFirst,
    /// The data directory has no free space; the blocks directory has.
    DataLow,
    /// The first flush's WAL sync fails, which latches.
    Sync,
    BlocksLowSync,
    DataLowSync,
    /// The first flush's WAL sync fails, which latches; the second finds the blocks directory
    /// low. Core's second flush checks the space before its block index write, so it reports the
    /// low disk, not the failed sync.
    SyncThenBlocksLow,
}

const FAULTS: [Fault; 8] = [
    Fault::None,
    Fault::BlocksLow,
    Fault::BlocksLowFirst,
    Fault::DataLow,
    Fault::Sync,
    Fault::BlocksLowSync,
    Fault::DataLowSync,
    Fault::SyncThenBlocksLow,
];

impl Fault {
    /// Core's reports and timer pairs for the fault on a start whose coins have a stored best
    /// block (`stored`) or none: the order is Core's, and nothing runs after the first failure.
    fn expected(self, stored: bool) -> (Vec<String>, usize) {
        let space = || space_report().to_vec();
        let twice = |r: Vec<String>| [r.clone(), r].concat();
        let pairs = |n: usize| {
            if stored {
                n
            } else {
                0
            }
        };
        match self {
            Fault::None => (vec![], pairs(2)),
            Fault::BlocksLow | Fault::BlocksLowSync => (twice(space()), 0),
            Fault::BlocksLowFirst => (space(), pairs(1)),
            Fault::DataLow if stored => (twice(space()), 0),
            Fault::DataLow => (vec![], 0),
            Fault::Sync | Fault::DataLowSync => (twice(sync_report()), 0),
            Fault::SyncThenBlocksLow => ([sync_report(), space()].concat(), 0),
        }
    }

    /// Whether flush `n` finds the blocks directory low.
    fn blocks_low(self, n: usize) -> bool {
        match self {
            Fault::BlocksLow | Fault::BlocksLowSync => true,
            Fault::BlocksLowFirst => n == 1,
            Fault::SyncThenBlocksLow => n == 2,
            _ => false,
        }
    }

    /// The WAL syncs that reach the engine in each flush: one per flush past the blocks directory's
    /// check, the failing one included, and none once the store's latch refuses the sync.
    fn syncs(self) -> [usize; 2] {
        let sync_fails =
            matches!(self, Fault::Sync | Fault::DataLowSync | Fault::SyncThenBlocksLow);
        [1, 2].map(|n| usize::from(!self.blocks_low(n) && !(sync_fails && n == 2)))
    }

    /// Core's log of both flushes under `-debug=bench -debug=coindb`, each line as [`flush_log`]
    /// keeps it, after each flush's mark: the BENCH timer pairs and COINDB commit lines around its
    /// steps, the default-level evodb pair, and the reports in their places. A failed sync's
    /// report comes after the block index's pair (Core's `catch` is outside every timer); a low
    /// data directory's `AbortNode` inside the coins pair, its failure line after it.
    // Core: validation.cpp:3058-3137; txdb.cpp:170-172; logging/timer.h:38-75; dashd's own log in
    // rdashd/tests/dashd_v24.rs `a_coins_tip_the_index_lacks_flushes_twice_on_dashd`
    fn core_log(self, stored: bool) -> Vec<String> {
        let bench = |what: &str| {
            [
                format!("[bench] FlushStateToDisk: {what} started"),
                format!("[bench] FlushStateToDisk: {what} completed ("),
            ]
        };
        let trim = |r: &[String]| r.iter().map(|l| l.trim_end().to_owned()).collect::<Vec<_>>();
        let (space, sync) = (trim(&space_report()), trim(&sync_report()));
        let data_low = matches!(self, Fault::DataLow | Fault::DataLowSync);
        let sync_fails =
            matches!(self, Fault::Sync | Fault::DataLowSync | Fault::SyncThenBlocksLow);
        let mut log = vec![];
        for n in 1..=2 {
            log.push(format!("{FLUSH_MARK}{n}"));
            if self.blocks_low(n) {
                log.extend(space.iter().cloned());
                continue;
            }
            log.extend(bench("write block and undo data to disk"));
            let [started, completed] = bench("write block index to disk");
            log.push(started);
            if sync_fails {
                log.extend(sync[..2].iter().cloned());
                log.push(completed);
                log.extend(sync[2..].iter().cloned());
                continue;
            }
            log.push(completed);
            if !stored {
                continue;
            }
            let [started, completed] = bench("write coins cache to disk (0 coins, 262kB)");
            log.push(started);
            if data_low {
                log.extend(space[..2].iter().cloned());
                log.push(completed);
                log.push(space[2].clone());
                continue;
            }
            log.push("[coindb] Writing final batch of 0.00 MiB".to_owned());
            log.push(
                "[coindb] Committed 0 changed transaction outputs (out of 0) to coin database..."
                    .to_owned(),
            );
            log.push(completed);
            log.push(STARTED.trim_end().to_owned());
            log.push(COMPLETED.to_owned());
        }
        log
    }
}

/// The lines of `lines` the shutdown flushes log, from each flush's mark on: the timers (each
/// completed line up to its duration), the COINDB lines, and the reports, without their
/// timestamps.
fn flush_log(lines: &[String]) -> Vec<String> {
    flush_log_for(SYNC, lines)
}

/// [`flush_log`] for a failure whose raw engine text, `HandleError`'s first line, is
/// `engine_text`.
pub(super) fn flush_log_for(engine_text: &str, lines: &[String]) -> Vec<String> {
    let keys = [
        FLUSH_MARK,
        "[bench] ",
        "[coindb] ",
        "FlushStateToDisk: ",
        "ForceFlushStateToDisk: ",
        "*** ",
        "Error: ",
        engine_text,
        "You can use -debug=leveldb",
    ];
    let line = |l: &String| {
        let at = keys.iter().filter_map(|k| l.find(k)).min()?;
        let l = l[at..].trim_end();
        let timed = l.find(" completed (").map(|i| i + " completed (".len());
        Some(timed.map_or(l, |end| &l[..end]).to_owned())
    };
    lines.iter().filter_map(line).collect()
}

/// Arms `fault` at `sut`'s shutdown flushes, and logs [`FLUSH_MARK`] as each starts; the
/// engine's WAL sync count at each start.
fn arm(sut: &mut NodeSut, fault: Fault) -> Syncs {
    let blocks = sut.args.get_blocks_dir_path().unwrap();
    let data = sut.args.get_data_dir_net().unwrap();
    let (low, sync) = (Arc::clone(&sut.low_dir), Arc::clone(&sut.sync_once));
    let logger = Arc::clone(sut.ui.logger());
    let (syncs, counted) = (Arc::clone(&sut.syncs), Syncs::default());
    let at_flush = Arc::clone(&counted);
    let n = AtomicU32::new(0);
    sut.probe = Some(Arc::new(move |point| {
        if !matches!(point, Point::Flush { .. }) {
            return;
        }
        let n = n.fetch_add(1, Ordering::SeqCst) + 1;
        at_flush.lock().unwrap().push(syncs.load(Ordering::SeqCst));
        logger.log_printf(&format!("{FLUSH_MARK}{n}\n"));
        let dir = match fault {
            Fault::BlocksLow | Fault::BlocksLowSync => Some(&blocks),
            Fault::BlocksLowFirst => (n == 1).then_some(&blocks),
            Fault::SyncThenBlocksLow => (n == 2).then_some(&blocks),
            Fault::DataLow | Fault::DataLowSync => Some(&data),
            Fault::None | Fault::Sync => None,
        };
        *low.lock().unwrap() = dir.cloned();
        let armed = matches!(
            fault,
            Fault::Sync | Fault::BlocksLowSync | Fault::DataLowSync | Fault::SyncThenBlocksLow
        );
        sync.store(n == 1 && armed, Ordering::SeqCst);
    }));
    counted
}

/// The WAL syncs that reached `sut`'s engine in each shutdown flush, from the counts [`arm`]
/// took; the opening's are before the first.
fn syncs_per_flush(sut: &NodeSut, counted: &Syncs) -> Vec<usize> {
    let mut at = counted.lock().unwrap().clone();
    at.push(sut.syncs.load(Ordering::SeqCst));
    at.windows(2).map(|w| w[1] - w[0]).collect()
}

/// A session in `dir` on `coins` with `argv`, logged, `setup` applied before its first open.
fn start(
    dir: &Path,
    argv: &[&str],
    coins: Arc<MemoryEngine>,
    setup: impl FnOnce(&mut NodeSut),
) -> (NodeSut, Lines) {
    let (ui, lines) = logged_ui();
    let mut sut = NodeSut::unopened(dir, argv, MOCKTIME, ui, coins).expect("accepted");
    setup(&mut sut);
    sut.open();
    (sut, lines)
}

/// `sut` stopped cleanly, its coins as the next process finds them.
fn stopped(mut sut: NodeSut) -> Arc<MemoryEngine> {
    sut.steps.shutdown();
    let coins = Arc::clone(&sut.coins);
    drop(sut);
    coins.restart();
    coins
}

fn put_meta(coins: &MemoryEngine, key: &[u8], value: &[u8]) {
    let mut put = coins.batch();
    put.put(meta_cf(), key, value);
    coins.write(put, Commit::Durable).unwrap();
}

fn put_row(coins: &MemoryEngine, cf: &str) {
    let mut put = coins.batch();
    put.put(CHAINSTATE_SCHEMA.cf(cf).unwrap(), b"stray", b"row");
    coins.write(put, Commit::Durable).unwrap();
}

/// A chainstate with nothing in it but `fill`'s rows: the start initializes genesis on an empty
/// index (DEC-1071's fresh index).
fn fresh(fill: impl FnOnce(&MemoryEngine)) -> Arc<MemoryEngine> {
    let coins = Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA));
    fill(&coins);
    coins
}

/// [`left_ibd_at_b1`]'s session stopped cleanly in `dir` (its coins and evo state at b2), then
/// `change` to its coins.
fn stored(dir: &Path, change: impl FnOnce(&MemoryEngine, &Blocks)) -> Arc<MemoryEngine> {
    let LeftIbd {
        sut,
        b1,
        b2,
        ..
    } = left_ibd_at_b1(dir);
    let coins = stopped(sut);
    change(&coins, &(b1, b2));
    coins
}

/// Row 10 on a stored tip: the coins' best block b2 has lost its index record.
fn unknown_tip(coins: &MemoryEngine, (_, b2): &Blocks) {
    let mut drop_tip = coins.batch();
    drop_tip.delete(CHAINSTATE_SCHEMA.cf("block_index").unwrap(), b2.hash().as_bytes());
    coins.write(drop_tip, Commit::Durable).unwrap();
}

/// R2's invalid stored tip (a10-1's fixture): b1, below the stored tip, marked failed.
fn invalid_tip(coins: &MemoryEngine, (b1, _): &Blocks) {
    let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
    let bytes = coins.get(cf, b1.hash().as_bytes()).unwrap().expect("stored");
    let mut record = dash_chain::BlockIndexRecord::decode(&bytes).unwrap();
    record.status = dash_chain::Status(record.status.bits() | dash_chain::Status::FAILED_VALID);
    let mut put = coins.batch();
    put.put(cf, b1.hash().as_bytes(), &record.to_bytes());
    coins.write(put, Commit::Durable).unwrap();
}

/// Regtest's genesis hash.
fn regtest_genesis() -> BlockHash {
    use dash_params::{ChainParamsBuilder, Network, Release};
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    dashd_chainstate::chain::genesis_block(&params).hash()
}

/// Block files that read no block (the readiness guard's failed read of a replayed block).
struct Unreadable(Box<dyn BlockFiles>);

impl BlockFiles for Unreadable {
    fn save_block(
        &mut self,
        block: &Arc<dash_consensus::Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        self.0.save_block(block, height, known)
    }

    fn read_block(
        &self,
        _: FlatFilePos,
        _: &BlockHash,
    ) -> Result<Arc<dash_consensus::Block>, String> {
        Err("injected read".to_owned())
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

/// A refusal of DEC-1071 condition 1: Core's text and class for it, and whether its coins have a
/// stored best block (the timer pair's condition).
struct Refused {
    text: String,
    status: LoadStatus,
    raw: String,
    stored_tip: bool,
}

impl Refused {
    /// A `FAILURE` with init's reindex question.
    fn failure(message: &str, stored_tip: bool) -> Refused {
        Refused {
            text: reindex_question(message),
            status: LoadStatus::Failure,
            raw: message.to_owned(),
            stored_tip,
        }
    }

    /// Init's exception (`init_error`), its text as the raw message.
    fn exception(message: &str, stored_tip: bool) -> Refused {
        Refused {
            text: message.to_owned(),
            status: LoadStatus::InitError,
            raw: message.to_owned(),
            stored_tip,
        }
    }
}

/// `sut`'s start refused as `want`, with both shutdown flushes and Core's no-fault output for
/// them: no report, a timer pair per flush with a stored best block, exit status 1, no V, and
/// the storage `before` the start held, when given.
fn assert_refused(
    sut: &NodeSut,
    lines: &Lines,
    want: &Refused,
    before: Option<&Stored>,
    what: &str,
) {
    assert_eq!(sut.down(), Some(want.text.as_str()), "{what}");
    let class = LoadResult {
        status: want.status,
        message: want.raw.clone(),
    };
    assert_eq!(sut.load_result(), &class, "{what}");
    assert!(sut.chain().is_none(), "{what}: no V");
    let lines = lines.lock().unwrap();
    assert_flushes(sut, &lines, Fault::None, want.stored_tip, what);
    assert!(!lines.iter().any(|l| l.contains("LoadChainTip: unknown block")), "{what}");
    drop(lines);
    if let Some(before) = before {
        assert_eq!(&sut.persisted().expect("read"), before, "{what}: nothing stored");
    }
}

/// `sut`'s two shutdown flushes under `fault`, as Core's: exit status 1, and in `lines` Core's
/// reports and timer pairs for the fault.
fn assert_flushes(sut: &NodeSut, lines: &[String], fault: Fault, stored: bool, what: &str) {
    assert_eq!(sut.flushes(), 2, "{what}: Core's two flushes");
    // The harness's own status, set as `NodeSut::open` refuses; the product's 1 is `Node::fail`'s
    // for every init refusal (node.rs `an_unknown_coins_tip_fails_init_with_exit_status_1`).
    assert_eq!(sut.shutdown.exit_status(), EXIT_FAILURE, "{what}");
    let (reported, n) = fault.expected(stored);
    assert_eq!(reports(lines), reported, "{what}: {lines:?}");
    assert_eq!(pairs(lines), (n, n), "{what}: {lines:?}");
}

/// [`refused_at_each_start`] on regtest, with no setup, for each of `cases`: a label and the
/// coins the start finds.
fn refused_for_each(cases: &[(&str, &Fixture<'_>)], want: &Refused) {
    for (what, coins) in cases {
        refused_at_each_start(&REGTEST, coins, |_| {}, want, what);
    }
}

/// The start on `coins` refused as `want`, then each next start, after a teardown of either
/// kind, refused the same way.
fn refused_at_each_start(
    argv: &[&str],
    coins: &Fixture<'_>,
    setup: impl FnOnce(&mut NodeSut),
    want: &Refused,
    what: &str,
) {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, coins) = (tmp.path(), coins(tmp.path()));
    let mut before = None;
    let (mut sut, lines) = start(dir, argv, coins, |sut| {
        before = Some(sut.persisted().expect("read"));
        setup(sut);
    });
    let before = before.unwrap();
    assert_refused(&sut, &lines, want, Some(&before), what);
    for how in [Teardown::Kill, Teardown::Clean, Teardown::Kill] {
        lines.lock().unwrap().clear();
        let restarted = sut.restart_report(how);
        assert_eq!(restarted.persisted.expect("read"), before, "{what} {how:?}");
        let what = format!("{what}, next start {how:?}");
        assert_refused(&sut, &lines, want, Some(&before), &what);
    }
}

/// DEC-1071 condition 1 for `UnknownTip` (row 10): Core's `LoadChainTip` false, "Error
/// initializing block database" with the reindex question, past `InitCoinsCache`. On a stored
/// tip whose index record is gone, and on a fresh index whose initialized genesis is not the
/// coins' best block (X2: the genesis records stay unwritten, so each start initializes it
/// again).
// Core: node/chainstate.cpp:272-276; validation.cpp:5015-5017
#[test]
fn an_unknown_coins_tip_is_refused_with_both_flushes_at_each_start() {
    let want = Refused::failure(INIT_BLOCK_DB_FAILED, true);
    let cases: [(&str, &Fixture<'_>); 2] = [
        ("stored tip", &|d: &Path| stored(d, unknown_tip)),
        ("fresh index", &|_: &Path| fresh(|c| put_meta(c, b"tip", &[7; 32]))),
    ];
    refused_for_each(&cases, &want);
}

/// DEC-1071 condition 1 for `EvoWithoutBlocks` (row 11): Core's `VerifyLoadedChainstate`
/// failure, past `InitCoinsCache`, with no stored best block, so no timer pair. Both of its
/// fixtures: an evo row, and an evo tip, without a coins tip. It cannot arise on a stored tip:
/// the refusal is that the coins have none.
// Core: node/chainstate.cpp:467-474
#[test]
fn evo_state_without_blocks_is_refused_with_both_flushes_at_each_start() {
    let want = Refused::failure(dashd_chainstate::chain::verify::EVODB_WITHOUT_BLOCKS, false);
    let cases: [(&str, &Fixture<'_>); 2] = [
        ("an evo row", &|_: &Path| fresh(|c| put_row(c, "mn_diff"))),
        ("an evo tip", &|_: &Path| fresh(|c| put_meta(c, b"evo_tip", &[7; 32]))),
    ];
    refused_for_each(&cases, &want);
}

/// DEC-1071 condition 1 for `EvoInconsistent`, a refusal Core does not make (Core loads, past
/// `InitCoinsCache`): the coins' best block known, the evo state at another block. On a stored
/// tip, and on a fresh index at genesis, its initialized block.
// Core: validation.cpp:1939 (the text)
#[test]
fn an_evo_tip_off_the_coins_tip_is_refused_with_both_flushes_at_each_start() {
    let want = Refused::failure(dashd_chainstate::connect::EVODB_INCONSISTENCY, true);
    let genesis = regtest_genesis();
    let cases: [(&str, &Fixture<'_>); 2] = [
        ("stored tip", &|d: &Path| {
            stored(d, |c, (b1, _)| put_meta(c, b"evo_tip", b1.hash().as_bytes()))
        }),
        ("fresh index", &|_: &Path| fresh(|c| put_meta(c, b"tip", genesis.as_bytes()))),
    ];
    refused_for_each(&cases, &want);
}

/// DEC-1071 condition 1 for `Corruption`, a refusal Core does not make (DEC-165(3)): coins rows
/// with no tip, on a fresh index. It cannot arise on a stored tip: the refusal is that the
/// coins have none.
#[test]
fn coins_without_a_tip_are_refused_with_both_flushes_at_each_start() {
    let message = "Corrupted chainstate database: no tip, but `coins` holds records. You will \
                   need to rebuild the database using -reindex-chainstate.";
    let want = Refused::failure(message, false);
    refused_for_each(&[("fresh index", &|_: &Path| fresh(|c| put_row(c, "coins")))], &want);
}

/// DEC-1071 condition 1 for R2's invalid stored tip (DEC-196(1)'s refusal; Core asserts there,
/// so this output is the ruled extension of rdashd's refusal, not Core's). It cannot arise on a
/// fresh index: the only record there is the genesis just initialized, which is valid.
#[test]
fn an_invalid_stored_tip_is_refused_with_both_bare_flushes_at_each_start() {
    let want = Refused::failure(INIT_BLOCK_DB_FAILED, true);
    refused_for_each(&[("stored tip", &|d: &Path| stored(d, invalid_tip))], &want);
}

/// DEC-1071 condition 1 for R2's V spawn failure, after recovery: init's exception, with the
/// bare flushes; their timer pairs only with a stored best block. On a fresh index and on a
/// stored tip.
#[test]
fn a_failed_v_spawn_is_refused_with_both_bare_flushes_at_each_start() {
    let spawn = |sut: &mut NodeSut| sut.start_fault = Some(StartFault::Spawn);
    let message = "injected spawn failure";
    let coins: &Fixture<'_> = &|d: &Path| stored(d, |_, _| {});
    let want = Refused::exception(message, true);
    refused_at_each_start(&REGTEST, coins, spawn, &want, "stored tip");
    let coins: &Fixture<'_> = &|_: &Path| fresh(|_| {});
    let want = Refused::exception(message, false);
    refused_at_each_start(&REGTEST, coins, spawn, &want, "fresh index");
}

/// DEC-1071 condition 1 for R2's readiness refusal (CH3a D70), on a stored tip: recovery replays
/// b49 and b50 of the recorded chain (DIP0003 from 48) from a marker over b48, and the guard's
/// read of b49, below the tip, fails, so the load is VerifyDB's `CORRUPTED_BLOCK_DB` with its
/// cause, before V starts. The replay stored b50, so the next start has nothing to replay: the
/// guard reads only the tip, whose failed read is VerifyDB's, after the attach. It cannot arise
/// on a fresh index: the guard reads only the tip there, genesis, whose failed read it leaves to
/// VerifyDB.
// Core: validation.cpp:5099-5100 (the text)
#[test]
fn a_readiness_refusal_is_refused_with_both_bare_flushes() {
    use dashd_chainstate::chain::verify::CORRUPTED_BLOCK_DB;
    let blocks = recorded_blocks(CH7A4B_BLOCKS);
    let (b48, b50) = (&blocks[47], &blocks[49]);
    let dir = tempfile::tempdir().unwrap();
    let mut sut = try_sut(dir.path(), &CH7A4_ARGS, ui()).expect("accepted");
    imported(&mut sut);
    for b in &blocks[..50] {
        submit(&sut, b);
    }
    let coins = stopped(sut);
    assert_eq!(coins.get(meta_cf(), b"tip").unwrap(), Some(b50.hash().as_bytes().to_vec()));
    let mut marker = coins.batch();
    marker.delete(meta_cf(), b"tip");
    let head = [b50.hash().as_bytes().as_slice(), b48.hash().as_bytes()].concat();
    marker.put(meta_cf(), b"head_blocks", &head);
    coins.write(marker, Commit::Durable).unwrap();
    let argv = [&REGTEST[..], &CH7A4_ARGS].concat();
    let unreadable = |sut: &mut NodeSut| {
        sut.wrap_files = Some(|files| Box::new(Unreadable(files)));
    };
    let (mut sut, lines) = start(dir.path(), &argv, coins, unreadable);
    let want = Refused::failure(CORRUPTED_BLOCK_DB, true);
    // This start's storage check: the replay stored b50.
    assert_eq!(sut.persisted().expect("read").meta.tip, Some(b50.hash()), "the replay stored b50");
    assert_refused(&sut, &lines, &want, None, "the replayed blocks");
    let cause = format!("ReadBlockFromDisk failed at 49, hash={}", blocks[48].hash());
    assert!(lines.lock().unwrap().iter().any(|l| l.contains(&cause)), "{lines:?}");
    lines.lock().unwrap().clear();
    let next = sut.restart_report(Teardown::Clean);
    assert_eq!(next.load.status, LoadStatus::Failure);
    assert_eq!(next.load.message, CORRUPTED_BLOCK_DB, "VerifyDB's, after the attach");
    assert_eq!(sut.down(), Some(want.text.as_str()));
    assert_eq!(sut.flushes(), 2);
}

/// Row 10's shutdown faults (BL-636 plan §3.3 (b)-(d) and §7 m-4's order): Core's reports, in
/// Core's order, with nothing run after the first failure. A low blocks directory for the first
/// flush only reports once, then the second flush's pair (no latch). Exit status 1 in every case.
#[test]
fn an_unknown_coins_tip_s_flushes_fail_as_cores() {
    for fault in FAULTS {
        let dir = tempfile::tempdir().unwrap();
        let coins = stored(dir.path(), unknown_tip);
        faulted(dir.path(), &REGTEST, coins, |_| {}, fault, true, INIT_BLOCK_DB_FAILED);
    }
}

/// A start on `coins` in `dir`, `setup` applied and `fault` armed, refused with `message`: Core's
/// reports and pairs for the fault.
fn faulted(
    dir: &Path,
    argv: &[&str],
    coins: Arc<MemoryEngine>,
    setup: impl FnOnce(&mut NodeSut),
    fault: Fault,
    stored: bool,
    message: &str,
) -> (NodeSut, Lines) {
    let mut counted = None;
    let (sut, lines) = start(dir, argv, coins, |sut| {
        setup(sut);
        counted = Some(arm(sut, fault));
    });
    let what = format!("{fault:?}");
    assert_eq!(sut.down(), Some(reindex_question(message).as_str()), "{what}");
    assert_flushes(&sut, &from_first_flush(&lines), fault, stored, &what);
    let syncs = syncs_per_flush(&sut, &counted.unwrap());
    assert_eq!(syncs, fault.syncs(), "{what}: the WAL syncs of each flush");
    (sut, lines)
}

/// Row 11's shutdown faults, both fixtures: with no stored best block, a low data directory
/// reports nothing (Core checks it only with one), and no timer pair is logged.
#[test]
fn evo_state_without_blocks_flushes_fail_as_cores() {
    let message = dashd_chainstate::chain::verify::EVODB_WITHOUT_BLOCKS;
    let fills: [fn(&MemoryEngine); 2] =
        [|c| put_row(c, "mn_diff"), |c| put_meta(c, b"evo_tip", &[7; 32])];
    for fill in fills {
        for fault in FAULTS {
            let dir = tempfile::tempdir().unwrap();
            let coins = fresh(fill);
            let (_sut, lines) = faulted(dir.path(), &REGTEST, coins, |_| {}, fault, false, message);
            if fault == Fault::None {
                // The D71 detail, before the shutdown.
                let lines = lines.lock().unwrap();
                let detail = lines.iter().position(|l| l.contains("no tip, but"));
                let flush = lines.iter().position(|l| l.contains(FLUSH_MARK));
                assert!(detail.is_some() && detail < flush, "{lines:?}");
            }
        }
    }
}

/// R2's invalid stored tip with the blocks directory low: both bare flushes report it.
#[test]
fn an_invalid_stored_tip_s_flushes_report_a_low_disk() {
    let dir = tempfile::tempdir().unwrap();
    let coins = stored(dir.path(), invalid_tip);
    let fault = Fault::BlocksLow;
    faulted(dir.path(), &REGTEST, coins, |_| {}, fault, true, INIT_BLOCK_DB_FAILED);
}

/// A devnet's row 11 (X2's devnet variant: V's genesis step ran before recovery refused the evo
/// row): V started but the coins never reached it, so the flushes are bare. Without a fault they
/// report nothing; with the blocks directory low each reports it. Either way the listener V got
/// hears no `ChainStateFlushed`, as nothing was written (DEC-995).
#[test]
fn a_devnet_s_evo_rows_without_blocks_get_the_bare_flushes() {
    for fault in [Fault::None, Fault::BlocksLow] {
        let dir = tempfile::tempdir().unwrap();
        let mut made = None;
        let coins = fresh(|c| put_row(c, "mn_diff"));
        let message = dashd_chainstate::chain::verify::EVODB_WITHOUT_BLOCKS;
        let listen = |sut: &mut NodeSut| made = Some(listener_per_open::<Flushed>(sut));
        let _sut = faulted(dir.path(), &DEVNET, coins, listen, fault, false, message);
        let made = made.unwrap();
        let made = made.lock().unwrap();
        assert_eq!(made.len(), 1, "{fault:?}: V started");
        assert!(made[0].0.lock().unwrap().is_empty(), "{fault:?}: no ChainStateFlushed");
    }
}

/// The guard: the attach attempted, V stops on a panic before it holds a flush side, so it hands
/// back nothing (no `VExit`), with the store latch open. That keeps today's flush, the WAL sync
/// only: with the blocks directory low and a stored best block, no space check and no timer
/// pair run.
#[test]
fn an_attempted_attach_keeps_the_wal_only_flush() {
    let dir = tempfile::tempdir().unwrap();
    let coins = stored(dir.path(), |_, _| {});
    let (sut, lines) = start(dir.path(), &REGTEST, coins, |sut| {
        sut.start_fault = Some(StartFault::Attach);
        arm(sut, Fault::BlocksLow);
    });
    assert_eq!(sut.down(), Some("injected attach fault"));
    assert_eq!(sut.flushes(), 2);
    let lines = from_first_flush(&lines);
    let failed: Vec<_> = lines.iter().filter(|l| l.contains("failed to flush state")).collect();
    assert_eq!(failed, Vec::<&String>::new(), "{lines:?}");
    assert_eq!(pairs(&lines), (0, 0), "{lines:?}");
}

/// Row 14: a failed read in the no-tip scan is CH7 D71's store failure, which Core's load would
/// not see; the chainstate is not kept, so shutdown flushes nothing.
#[test]
fn a_failed_no_tip_scan_read_keeps_no_chainstate() {
    let dir = tempfile::tempdir().unwrap();
    let (sut, _) = start(dir.path(), &REGTEST, fresh(|_| {}), |sut| {
        sut.prefix_fault = Some(CHAINSTATE_SCHEMA.cf("coins").unwrap());
    });
    let down = reindex_question("Error opening block database");
    assert_eq!(sut.down(), Some(down.as_str()));
    assert_eq!(sut.flushes(), 0, "no chainstate kept");
}

/// A marker naming a block the index lacks fails `ReplayBlocks`, before `InitCoinsCache` in
/// Core: no chainstate is kept, so shutdown flushes nothing and logs no timer pair.
// Core: node/chainstate.cpp:251-256; validation.cpp:5258
#[test]
fn an_unreplayable_marker_keeps_no_chainstate() {
    let dir = tempfile::tempdir().unwrap();
    let coins = stored(dir.path(), |c, (_, b2)| {
        let mut marker = c.batch();
        marker.delete(meta_cf(), b"tip");
        let head = [[0xee; 32].as_slice(), b2.hash().as_bytes()].concat();
        marker.put(meta_cf(), b"head_blocks", &head);
        c.write(marker, Commit::Durable).unwrap();
    });
    let (sut, lines) = start(dir.path(), &BENCH, coins, |_| {});
    let unable = "Unable to replay blocks. You will need to rebuild the database using \
                  -reindex-chainstate.";
    assert_eq!(sut.down(), Some(reindex_question(unable).as_str()));
    assert_eq!(sut.flushes(), 0, "no chainstate kept");
    let lines = lines.lock().unwrap();
    let detail = "ReplayBlocks(): reorganization to unknown block requested";
    assert!(lines.iter().any(|l| l.contains(detail)), "{lines:?}");
    assert_eq!(pairs(&lines), (0, 0), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("[bench] FlushStateToDisk")), "{lines:?}");
}

/// Core's debug-category flush lines: under `-debug=bench -debug=coindb` each bare flush logs
/// W's BENCH timer pairs and COINDB commit lines in Core's order, with Core's figures for the
/// empty cache (dashd's own capture), around the reports the other tests pin. Row 10 (a stored
/// best block) and row 11 (none), under every fault.
// Core: validation.cpp:3058-3137; txdb.cpp:170-172; logging/timer.h:38-75
#[test]
fn bare_flushes_log_cores_bench_and_coindb_lines() {
    let unknown: &Fixture<'_> = &|d: &Path| stored(d, unknown_tip);
    let evo_row: &Fixture<'_> = &|_: &Path| fresh(|c| put_row(c, "mn_diff"));
    let rows = [
        (unknown, true, INIT_BLOCK_DB_FAILED),
        (evo_row, false, dashd_chainstate::chain::verify::EVODB_WITHOUT_BLOCKS),
    ];
    for (coins, has_tip, message) in rows {
        for fault in FAULTS {
            let dir = tempfile::tempdir().unwrap();
            let coins = coins(dir.path());
            let (_sut, lines) = faulted(dir.path(), &BENCH, coins, |_| {}, fault, has_tip, message);
            let got = flush_log(&from_first_flush(&lines));
            assert_eq!(got, fault.core_log(has_tip), "{fault:?}, stored best block {has_tip}");
        }
    }
}
