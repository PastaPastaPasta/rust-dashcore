//! [`CoreAbortSink`]: the production [`AbortSink`] (CH2 §2.10).

use std::io::Write as _;

use super::AbortSink;

/// The message `coins_error_cb` shows (`init.cpp:2089`).
const READ_FAILED_USER: &str = "Error reading from database, shutting down.";

/// The production sink. [`read_failed`](AbortSink::read_failed) is Core's `ExecuteBackedWrapper`
/// over the coins view: the error callback's message box, which without a GUI logs and prints
/// `"Error: "` and the message, then the log line `"Error reading from database: %s"`, then
/// `std::abort()`, with no shutdown and no flush. [`abort_node`](AbortSink::abort_node) is ND1's
/// `AbortNode`, which the node supplies, as it supplies the log.
pub struct CoreAbortSink {
    log: Box<dyn Fn(&str) + Send + Sync>,
    abort_node: Box<AbortNode>,
}

/// ND1's `AbortNode(log_msg, user_msg)`.
type AbortNode = dyn Fn(&str, Option<&str>) + Send + Sync;

impl CoreAbortSink {
    /// `log` writes one line to the debug log (`LogPrintf`). It must write the line through
    /// before it returns, as Core's unbuffered debug log does (`logging.cpp:61, :505`):
    /// [`read_failed`](AbortSink::read_failed) aborts right after its last line, so a buffered or
    /// non-blocking appender would lose both lines.
    pub fn new(
        log: impl Fn(&str) + Send + Sync + 'static,
        abort_node: impl Fn(&str, Option<&str>) + Send + Sync + 'static,
    ) -> Self {
        Self {
            log: Box::new(log),
            abort_node: Box::new(abort_node),
        }
    }
}

impl AbortSink for CoreAbortSink {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        (self.abort_node)(log_msg, user_msg);
    }

    // Core: coins.cpp:355-371 ExecuteBackedWrapper; init.cpp:2087-2091 coins_error_cb;
    // noui.cpp:22-47 noui_ThreadSafeMessageBox (MSG_ERROR)
    fn read_failed(&self, what: &str) {
        // The abort must not unwind: if formatting or the log panics (a `println!` into a broken
        // pipe), unwinding drops the guard, which aborts.
        let _abort = AbortOnUnwind;
        let user = format!("Error: {READ_FAILED_USER}");
        (self.log)(&user);
        // Not `eprintln!`: a failing stderr (a broken pipe) would panic it, and the guard would
        // abort before Core's second log line.
        let _ = writeln!(std::io::stderr(), "{user}");
        (self.log)(&format!("Error reading from database: {what}"));
        std::process::abort();
    }
}

/// Aborts when dropped. [`CoreAbortSink::read_failed`] holds one, so a panic on its way to
/// `abort()` aborts as it unwinds instead of reaching the caller.
struct AbortOnUnwind;

impl Drop for AbortOnUnwind {
    fn drop(&mut self) {
        std::process::abort();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{BufRead as _, BufReader, Read, Write};
    use std::os::unix::process::ExitStatusExt as _;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    /// Set in the child process, to the name of its [`Fault`].
    const CHILD: &str = "CH2A_CORE_ABORT_SINK_CHILD";
    /// The line the child writes to the stream whose reader then goes away.
    const READY: &str = "ready for a broken pipe";
    /// The child's exit status if `read_failed` unwound into its `catch_unwind`, or returned.
    const UNWOUND: i32 = 3;
    const RETURNED: i32 = 4;
    const LINES: [&str; 2] = [
        "Error: Error reading from database, shutting down.",
        "Error reading from database: injected read failure",
    ];

    /// How the log or the streams fail in the child's `read_failed` call.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Fault {
        /// None: the log returns.
        None,
        /// The log panics at its first (0) or second (1) call.
        PanicAt(usize),
        /// The log `println!`s into a stdout whose reader has gone.
        BrokenStdout,
        /// stderr's reader has gone.
        BrokenStderr,
        /// stderr is closed at exec (std reopens it on `/dev/null`).
        ClosedStderr,
    }

    impl Fault {
        const ALL: [Fault; 6] = [
            Fault::None,
            Fault::PanicAt(0),
            Fault::PanicAt(1),
            Fault::BrokenStdout,
            Fault::BrokenStderr,
            Fault::ClosedStderr,
        ];
    }

    /// The child process of [`aborted`]: `read_failed` meets the [`Fault`] that `CHILD` names,
    /// inside a `catch_unwind`. Without `CHILD`, in an ordinary test run, it does nothing.
    #[test]
    fn the_child_process_of_the_abort_tests() {
        let Ok(name) = std::env::var(CHILD) else {
            return;
        };
        let fault = Fault::ALL.into_iter().find(|f| format!("{f:?}") == name).unwrap();
        match fault {
            Fault::BrokenStdout => wait_for_a_broken_pipe(std::io::stdout()),
            Fault::BrokenStderr => wait_for_a_broken_pipe(std::io::stderr()),
            _ => {}
        }
        let calls = AtomicUsize::new(0);
        let log = move |line: &str| {
            if fault == Fault::PanicAt(calls.fetch_add(1, Ordering::Relaxed)) {
                panic!("the log failed");
            }
            println!("log: {line}");
        };
        let sink = CoreAbortSink::new(log, |_, _| unreachable!());
        let unwound = catch_unwind(AssertUnwindSafe(|| sink.read_failed("injected read failure")));
        let status = if unwound.is_err() {
            UNWOUND
        } else {
            RETURNED
        };
        std::process::exit(status);
    }

    /// Writes [`READY`] to `pipe`, then newlines until one fails: the parent read up to `READY`
    /// and closed the pipe.
    fn wait_for_a_broken_pipe(mut pipe: impl Write) {
        pipe.write_all(format!("{READY}\n").as_bytes()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while pipe.write_all(b"\n").is_ok() {
            assert!(Instant::now() < deadline, "the pipe never broke");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Reads `pipe` up to the child's [`READY`] line, then closes it.
    fn close_at_ready(pipe: impl Read) {
        let mut lines = BufReader::new(pipe).lines();
        assert!(lines.any(|l| l.unwrap().ends_with(READY)), "the child never got ready");
    }

    /// Runs the child process (the test binary, running only its child test, without a core
    /// dump) with `fault`, and checks that it died of SIGABRT. Returns the lines it logged and its
    /// stderr.
    fn aborted(fault: Fault) -> (Vec<String>, String) {
        let close_stderr = if fault == Fault::ClosedStderr {
            " 2>&-"
        } else {
            ""
        };
        let mut child = Command::new("sh")
            .args(["-c", &format!(r#"ulimit -c 0; exec "$0" "$@"{close_stderr}"#)])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "store::abort::tests::the_child_process_of_the_abort_tests"])
            .args(["--nocapture", "--test-threads=1"])
            .env(CHILD, format!("{fault:?}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        match fault {
            Fault::BrokenStdout => close_at_ready(child.stdout.take().unwrap()),
            Fault::BrokenStderr => close_at_ready(child.stderr.take().unwrap()),
            _ => {}
        }
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(
            out.status.signal(),
            Some(6),
            "{fault:?}: {:?}, not SIGABRT (unwound {UNWOUND}, returned {RETURNED}): {stderr}",
            out.status
        );
        // The harness's `test <name> ... ` has no newline yet when the first line is logged.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let logged = stdout.lines().filter_map(|l| l.split_once("log: ")).map(|(_, l)| l.into());
        (logged.collect(), stderr)
    }

    /// Core's user message to the log and to stderr, then its log line, then SIGABRT.
    #[test]
    fn read_failed_writes_cores_lines_then_aborts() {
        let (logged, stderr) = aborted(Fault::None);
        assert_eq!(logged, LINES);
        assert!(stderr.contains(&format!("{}\n", LINES[0])), "{stderr}");
    }

    /// A log that panics, at its first call or its second, still ends in SIGABRT: the panic does
    /// not unwind into the caller's `catch_unwind`.
    #[test]
    fn a_panicking_log_still_aborts() {
        for n in 0..2 {
            let (logged, stderr) = aborted(Fault::PanicAt(n));
            assert_eq!(logged, LINES[..n], "a panic at call {n}");
            assert!(stderr.contains("the log failed"), "a panic at call {n}: {stderr}");
        }
    }

    /// A log that `println!`s into a broken pipe panics, and still ends in SIGABRT.
    #[test]
    fn a_log_into_a_broken_pipe_still_aborts() {
        let (_, stderr) = aborted(Fault::BrokenStdout);
        assert!(stderr.contains("failed printing to stdout"), "{stderr}");
    }

    /// A broken or closed stderr neither panics nor stops the second log line.
    #[test]
    fn a_failing_stderr_does_not_stop_the_lines_or_the_abort() {
        for fault in [Fault::BrokenStderr, Fault::ClosedStderr] {
            assert_eq!(aborted(fault).0, LINES, "{fault:?}");
        }
    }
}
