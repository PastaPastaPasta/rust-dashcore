//! BL-674: a panic in V's flush-side code inside a shutdown flush on a real node, as `NodeSut`
//! runs the shutdown. Core's assert inside `FlushStateToDisk` ends the process; rdashd's flush
//! catches the panic, and V's one report stands alone (CH3b BL-674, AD-CH3-1).
// Core: util/check.h:49-51; init.cpp:383, :415-417, :453; validation.cpp:2980-2986

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use dashd_chainstate::chain::ShutdownFlush;
use dashd_chainstate::store::GlobalFailStop;
use dashd_chainstate::store::sites::V_PANIC_SITE;

use super::*;
use crate::abort::{DEFAULT_ABORT_USER_MESSAGE, NodeAbort};
use crate::shutdown::EXIT_FAILURE;

type Lines = Arc<Mutex<Vec<String>>>;
/// Step 11's latch and `AbortNode`, once it starts.
type Seen = Arc<Mutex<Option<(GlobalFailStop, Arc<NodeAbort>)>>>;

const REGTEST: [&str; 2] = ["-regtest", "-printtoconsole=0"];
/// V's report of the planted panic.
const PANICKED: &str = "V panicked: planted panic in the shutdown flush";
/// The line the probe logs as each shutdown flush starts.
const FLUSH_MARK: &str = "bl674: flush ";

/// A session's first load on `dir`, its log and stderr recorded: a probe logs [`FLUSH_MARK`] as
/// each shutdown flush starts and records the site of the latch's first failure there. The
/// node's `AbortNode` and the chainstate's latch, as step 11 saw them.
struct Loaded {
    sut: NodeSut,
    lines: Lines,
    stderr: Lines,
    /// The latch's first failure's site at each shutdown flush's start.
    sites: Arc<Mutex<Vec<Option<&'static str>>>>,
    abort: Arc<NodeAbort>,
    latch: GlobalFailStop,
}

fn load(dir: &Path) -> Loaded {
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
    let seen = Seen::default();
    let sites: Arc<Mutex<Vec<Option<&'static str>>>> = Arc::default();
    let (latch_at, recorded) = (Arc::clone(&seen), Arc::clone(&sites));
    let n = AtomicU32::new(0);
    sut.steps.probe = Some(Box::new(move |point| {
        if matches!(point, Point::Flush { .. }) {
            let n = n.fetch_add(1, Ordering::SeqCst) + 1;
            logger.log_printf(&format!("{FLUSH_MARK}{n}\n"));
            let seen = latch_at.lock().unwrap();
            let latch = &seen.as_ref().expect("step 11 started").0;
            recorded.lock().unwrap().push(latch.tripped().map(|f| f.site));
        }
    }));
    let saw = Arc::clone(&seen);
    sut.steps.before_import = Some(Box::new(move |_, latch, abort| {
        *saw.lock().unwrap() = Some((latch.clone(), Arc::clone(abort)));
    }));
    assert!(sut.load().is_ok(), "a fresh regtest start");
    let (latch, abort) = seen.lock().unwrap().clone().expect("step 11 started");
    Loaded {
        sut,
        lines,
        stderr,
        sites,
        abort,
        latch,
    }
}

/// Each line of `lines` about a failure or a shutdown flush's start, from its key on and without
/// its newline, in order.
fn failure_sequence(lines: &Lines) -> Vec<String> {
    let keys =
        [FLUSH_MARK, "*** ", "Error: ", "ForceFlushStateToDisk: ", "System error while flushing"];
    let lines = lines.lock().unwrap();
    (lines.iter())
        .filter_map(|l| {
            let at = keys.iter().filter_map(|k| l.find(k)).min()?;
            Some(l[at..].trim_end().to_owned())
        })
        .collect()
}

/// N-674: V's flush side panics before its job in either shutdown flush. The flush catches it:
/// V's one report, inside that flush's window, with Core's default user message and exit 1; no
/// `ForceFlushStateToDisk` failure line; both flushes counted, and after a panic in the first the
/// second starts with the latch at V's site and says nothing.
#[test]
fn a_v_panic_in_a_shutdown_flush_is_its_only_report() {
    for which in [ShutdownFlush::First, ShutdownFlush::Second] {
        let dir = tempfile::tempdir().unwrap();
        let mut l = load(dir.path());
        assert!(l.sut.steps.join_import(), "loadblk ended without a panic");
        assert!(l.latch.tripped().is_none());
        l.sut.steps.panic_v_in_flush = Some(which);
        l.sut.steps.shutdown();
        let sequence = failure_sequence(&l.lines);
        assert_eq!(l.abort.reports(), 1, "{which:?}: one report: {sequence:?}");
        let report = l.abort.report(0).expect("a report");
        assert_eq!((report.log.as_str(), report.user), (PANICKED, None));
        let error = format!("Error: {DEFAULT_ABORT_USER_MESSAGE}");
        let (one, two) = (format!("{FLUSH_MARK}1"), format!("{FLUSH_MARK}2"));
        let want = match which {
            ShutdownFlush::First => [one, format!("*** {PANICKED}"), error.clone(), two],
            ShutdownFlush::Second => [one, two, format!("*** {PANICKED}"), error.clone()],
        };
        assert_eq!(sequence, want, "{which:?}");
        assert_eq!(l.sut.flushes(), 2, "{which:?}: both shutdown flushes ran");
        assert_eq!(*l.stderr.lock().unwrap(), [format!("{error}\n")], "{which:?}: stderr");
        assert_eq!(l.sut.shutdown.exit_status(), EXIT_FAILURE, "{which:?}");
        let at_second = (which == ShutdownFlush::First).then_some(V_PANIC_SITE);
        assert_eq!(*l.sites.lock().unwrap(), [None, at_second], "{which:?}");
        assert_eq!(l.latch.tripped().map(|f| f.site), Some(V_PANIC_SITE), "{which:?}");
    }
}
