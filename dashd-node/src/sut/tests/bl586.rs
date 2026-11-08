//! BL-586: the shutdown flushes after one of Core's asserts on a real node, as `NodeSut` runs it.
//! Core's assert ends the process with no shutdown, so neither flush says anything; rdashd's one
//! report stands alone (CH3b BL-586, BL-601).
// Core: util/check.h:49-51; init.cpp:383, :453; validation.cpp:3133-3139; shutdown.cpp:27-38

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use dashd_chainstate::chain::BetweenStores;
use dashd_chainstate::flush::{Kept, kept};
use dashd_chainstate::store::GlobalFailStop;
use dashd_chainstate::store::sites::W_PANIC_SITE;

use super::*;
use crate::abort::{AbortSink as _, DEFAULT_ABORT_USER_MESSAGE, NodeAbort};
use crate::shutdown::EXIT_FAILURE;

type Lines = Arc<Mutex<Vec<String>>>;

const REGTEST: [&str; 2] = ["-regtest", "-printtoconsole=0"];
/// W's panic text, as its thread's end reports it.
const PANICKED: &str = "the chainstate flush thread panicked";
/// The line the probe logs as each shutdown flush starts.
const FLUSH_MARK: &str = "bl586: flush ";

/// A session's first load on `dir`, unopened until here, its log and stderr recorded: a probe
/// logs [`FLUSH_MARK`] as each shutdown flush starts, and `before` runs at step 11's start
/// (`ChainSteps::before_import`). Its steps are the session's own (`NodeSut::unopened`), so the
/// load runs them as `NodeSut::open` would. The load's result, the node's `AbortNode` and the
/// chainstate's latch, which `before` sees.
struct Loaded {
    sut: NodeSut,
    lines: Lines,
    stderr: Lines,
    loaded: Result<(), Refusal>,
    abort: Arc<NodeAbort>,
    latch: GlobalFailStop,
}

fn load(
    dir: &Path,
    before: impl FnOnce(&dashd_chainstate::chain::ChainHandle, &GlobalFailStop, &Arc<NodeAbort>)
    + Send
    + 'static,
) -> Loaded {
    let (lines, stderr) = (Lines::default(), Lines::default());
    let logger = Arc::new(Logger::new());
    let sink = Arc::clone(&lines);
    logger.push_back_callback(move |s| sink.lock().unwrap().push(s.to_owned()));
    assert!(logger.start_logging());
    let err = Arc::clone(&stderr);
    let ui = Arc::new(Ui::with_stderr(
        Arc::clone(&logger),
        Box::new(move |s| err.lock().unwrap().push(s.to_owned())),
    ));
    let coins = Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA));
    let mut sut = NodeSut::unopened(dir, &REGTEST, MOCKTIME, ui, coins).expect("accepted");
    sut.steps.pools = Some(sut.pools.clone());
    let n = AtomicU32::new(0);
    sut.steps.probe = Some(Box::new(move |point| {
        if matches!(point, Point::Flush { .. }) {
            let n = n.fetch_add(1, Ordering::SeqCst) + 1;
            logger.log_printf(&format!("{FLUSH_MARK}{n}\n"));
        }
    }));
    let seen = Arc::new(Mutex::new(None));
    let saw = Arc::clone(&seen);
    sut.steps.before_import = Some(Box::new(move |v, latch, abort| {
        *saw.lock().unwrap() = Some((latch.clone(), Arc::clone(abort)));
        before(v, latch, abort);
    }));
    let loaded = sut.load();
    let (latch, abort) = seen.lock().unwrap().take().expect("step 11 started");
    Loaded {
        sut,
        lines,
        stderr,
        loaded,
        abort,
        latch,
    }
}

/// Each line of `lines` about a failure or a shutdown flush's start, from its key on
/// and without its newline, in order.
fn failure_sequence(lines: &Lines) -> Vec<String> {
    let keys = [
        FLUSH_MARK,
        "*** ",
        "Error: ",
        "ForceFlushStateToDisk: ",
        "System error while flushing",
        "Fatal LevelDB error",
        "-debug=leveldb",
        "Failed to connect best block",
    ];
    let lines = lines.lock().unwrap();
    (lines.iter())
        .filter_map(|l| {
            let at = keys.iter().filter_map(|k| l.find(k)).min()?;
            Some(l[at..].trim_end().to_owned())
        })
        .collect()
}

/// The latch at [`W_PANIC_SITE`] with [`PANICKED`], one of Core's asserts.
fn assert_w_panic_latched(latch: &GlobalFailStop) {
    let f = latch.tripped().expect("tripped").clone();
    assert_eq!((f.site, f.message.as_str()), (W_PANIC_SITE, PANICKED));
    assert_eq!(kept(&f), Kept::Assert);
}

/// W's one report and the shutdown after it: one `AbortNode` (counted first, so a second report
/// fails here with the lines that show it) with Core's default user message, the misc warning its
/// text, exit 1, and `want` the whole ordered sequence of failure lines.
fn assert_one_report(l: &Loaded, want: &[String]) {
    assert_eq!(l.abort.reports(), 1, "one report: {:?}", failure_sequence(&l.lines));
    assert_eq!(failure_sequence(&l.lines), want);
    assert_eq!(l.sut.flushes(), 2, "both shutdown flushes ran");
    let report = l.abort.report(0).expect("a report");
    assert_eq!((report.log.as_str(), report.user), (PANICKED, None));
    let error = format!("Error: {DEFAULT_ABORT_USER_MESSAGE}\n");
    assert_eq!(*l.stderr.lock().unwrap(), [error], "stderr");
    assert_eq!(l.sut.warnings(), PANICKED);
    assert_eq!(l.sut.shutdown.exit_status(), EXIT_FAILURE);
    assert_w_panic_latched(&l.latch);
}

/// N4 (P3): W panics at the first step of the first shutdown flush's job, as an assert inside
/// Core's `FlushStateToDisk`. W's report is that flush's only output: no failure line follows it
/// (no `ForceFlushStateToDisk: failed to flush state`), and the second flush, which Core never
/// runs, says nothing (no BL-482 repeat).
#[test]
fn a_w_panic_in_the_first_shutdown_flush_is_its_only_report() {
    let dir = tempfile::tempdir().unwrap();
    let mut l = load(dir.path(), |_, _, _| {});
    assert!(l.loaded.is_ok(), "a fresh regtest start");
    assert!(l.sut.steps.join_import(), "loadblk ended without a panic");
    assert!(l.latch.tripped().is_none());
    l.sut.steps.panic_w_in_first_flush = true;
    l.sut.steps.shutdown();
    let want = [
        format!("{FLUSH_MARK}1"),
        format!("*** {PANICKED}"),
        format!("Error: {DEFAULT_ABORT_USER_MESSAGE}"),
        format!("{FLUSH_MARK}2"),
    ];
    assert_one_report(&l, &want);
}

/// N5 (Sol r1 B-1, r2 M-3): W's panic latched inside startup's ABC, on V's thread at the release
/// of genesis's connect on a fresh directory, and reported as W's thread's end reports it. ABC's
/// next flush-side call (its PERIODIC flush) is the first reader; the assert stays an assert, so
/// ABC drops init's reply and `loadblk` adds no "Failed to connect best block" (Core's assert
/// never returns to `ThreadImport`). The report stands alone through the node's import and
/// shutdown path.
// Core: util/check.h:49-51; node/blockstorage.cpp:949-952
#[test]
fn a_w_panic_before_startups_abc_is_reported_once() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&calls);
    let mut l = load(dir.path(), move |v, latch, abort| {
        assert!(latch.tripped().is_none(), "armed before startup's ABC");
        let (latch, abort) = (latch.clone(), Arc::clone(abort));
        let hook: BetweenStores = Arc::new(move || {
            if counted.fetch_add(1, Ordering::SeqCst) != 0 {
                return;
            }
            let failure = dash_kv::Failure {
                kind: dash_kv::ErrorKind::Other,
                site: W_PANIC_SITE,
                message: PANICKED.to_owned(),
            };
            assert!(latch.trip(failure), "W's panic latches first");
            abort.abort_node(PANICKED, None);
        });
        v.set_between_stores(Some(hook));
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the hook ran once, before init's import returned");
    assert!(l.sut.steps.join_import(), "loadblk ended without a panic");
    l.sut.steps.shutdown();
    let want = [
        format!("*** {PANICKED}"),
        format!("Error: {DEFAULT_ABORT_USER_MESSAGE}"),
        format!("{FLUSH_MARK}1"),
        format!("{FLUSH_MARK}2"),
    ];
    assert_one_report(&l, &want);
    let refused = l.loaded.as_ref().expect_err("the report requested a shutdown");
    assert_eq!(refused.result.status, LoadStatus::Interrupted, "{:?}", refused.result);
}
