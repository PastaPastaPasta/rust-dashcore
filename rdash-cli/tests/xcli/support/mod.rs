//! The XCLI differential harness: runs `rdash-cli` and the stock `dash-cli` v24 with identical
//! arguments, standard input and environment against the same server, and compares standard
//! output, standard error and the exit code byte for byte (PRD: XCLI).
//!
//! Provenance: the stock binary is the release `dash-cli` next to the v24.0.0-rc.3 dashd that
//! `DASHVERSION=24.0.0-rc.3 contrib/setup-dashd.py` caches. Nothing here is hand-written
//! expectation; the reference is always the stock binary's own output.

pub mod fake;
pub mod node;
pub mod says;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dash_rpc_spec::CORE_VERSION;
pub use node_test_ports::free_port;

/// The binaries under test.
#[derive(Clone)]
pub struct Tools {
    pub dashd: PathBuf,
    pub stock: PathBuf,
    pub rdash: PathBuf,
}

/// The first line of `-version`. dashd rewrites `settings.json` in its data directory even for
/// `-version`, so concurrent probes of the shared default directory race: use a private one.
fn version_line(path: &Path) -> Option<String> {
    let datadir = tempfile::tempdir().ok()?;
    let out = Command::new(path)
        .arg("-version")
        .arg(format!("-datadir={}", datadir.path().display()))
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.lines().next().map(str::to_owned)
}

fn is_pinned(path: &Path) -> bool {
    version_line(path).is_some_and(|v| v.ends_with(&format!("version {CORE_VERSION}")))
}

/// The v24 tools, or `None` when dashd tests are skipped (`SKIP_DASHD_TESTS`).
///
/// The dashd is `DASHD_V24_PATH`, else the first pinned binary of `ST2_DASHD` (the gate's v24
/// copy), else `DASHD_PATH` if pinned, else setup-dashd's cache. The stock `dash-cli` is its
/// sibling.
pub fn tools() -> Option<Tools> {
    static TOOLS: std::sync::OnceLock<Option<Tools>> = std::sync::OnceLock::new();
    TOOLS.get_or_init(find_tools).clone()
}

fn find_tools() -> Option<Tools> {
    if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
        eprintln!("Skipping XCLI test (SKIP_DASHD_TESTS is set)");
        return None;
    }
    let version = CORE_VERSION.strip_prefix('v').unwrap();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("DASHD_V24_PATH") {
        candidates.push(PathBuf::from(p));
    }
    if let Some(list) = std::env::var_os("ST2_DASHD") {
        candidates.extend(std::env::split_paths(&list));
    }
    if let Some(p) = std::env::var_os("DASHD_PATH") {
        let p = PathBuf::from(p);
        let cache = p.ancestors().nth(3).map(|c| c.join(format!("dashcore-{version}/bin/dashd")));
        candidates.push(p);
        candidates.extend(cache);
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home).join(format!(".rust-dashcore-test/dashcore-{version}/bin/dashd")),
        );
    }
    let Some(dashd) = candidates.into_iter().find(|p| is_pinned(p)) else {
        if !cfg!(target_os = "linux") {
            eprintln!(
                "XCLI SKIPPED: no dashd {CORE_VERSION} on this platform; full parity is proven on \
                 Linux only (set DASHD_V24_PATH to run the portable subset)"
            );
            return None;
        }
        panic!(
            "dashd {CORE_VERSION} not found. Run `DASHVERSION={version} python3 contrib/setup-dashd.py`, \
             or set DASHD_V24_PATH, or set SKIP_DASHD_TESTS=1 to skip."
        )
    };
    let stock = dashd.with_file_name(if cfg!(windows) {
        "dash-cli.exe"
    } else {
        "dash-cli"
    });
    assert!(stock.exists(), "stock dash-cli not found next to {}", dashd.display());
    Some(Tools {
        dashd,
        stock,
        rdash: PathBuf::from(env!("CARGO_BIN_EXE_rdash-cli")),
    })
}

/// An empty `HOME` that has both default data directories (stock `~/.dashcore`, rdash-cli
/// `~/.rdashcore`, DEC-201), so neither program fails reading its (absent) configuration file.
pub fn home_dir() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".dashcore")).unwrap();
    std::fs::create_dir(home.path().join(".rdashcore")).unwrap();
    home
}

/// Which standard streams are a terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tty {
    /// Pipes throughout.
    #[default]
    None,
    /// Standard output is a pseudo-terminal.
    Stdout,
    /// Standard input is a pseudo-terminal that already holds the input when the program starts
    /// (the program finds it ready and prints no prompt).
    StdinReady,
    /// Standard input is a pseudo-terminal that gets its input once the program has printed a
    /// prompt. What the terminal echoed is part of the output, so a program that fails to turn
    /// echo off differs.
    StdinPrompted,
}

/// One command line: arguments, standard input.
#[derive(Clone, Debug, Default)]
pub struct Invocation {
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
    pub tty: Tty,
}

impl Invocation {
    pub fn new<S: AsRef<str>>(args: impl IntoIterator<Item = S>) -> Self {
        Invocation {
            args: args.into_iter().map(|s| s.as_ref().to_string()).collect(),
            stdin: Vec::new(),
            tty: Tty::None,
        }
    }

    pub fn stdin(mut self, bytes: &[u8]) -> Self {
        self.stdin = bytes.to_vec();
        self
    }

    pub fn tty(mut self, tty: Tty) -> Self {
        self.tty = tty;
        self
    }
}

/// What a run produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub code: Option<i32>,
    /// Through a pseudo-terminal when the invocation asks for one.
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// What a pseudo-terminal standard input echoed back (`Tty::StdinPrompted` only).
    pub echo: Vec<u8>,
}

/// A pseudo-terminal pair.
pub struct Pty {
    pub master: std::fs::File,
    pub slave: std::os::fd::OwnedFd,
}

/// Opens a pseudo-terminal whose two descriptors are close-on-exec: a program another thread
/// spawns meanwhile does not inherit them, and so cannot keep the slave open after the row's own
/// program exited (its readers would wait for the hang-up until their deadline). The program a
/// row runs on it gets its own copy as standard input, which `dup2` makes inheritable. (The
/// helper `background_prompt_verdict` forks never execs; it closes what it inherited itself.)
pub fn open_pty() -> Pty {
    use std::os::fd::{FromRawFd, OwnedFd};
    // SAFETY: plain system calls; both descriptors are owned from here on.
    unsafe {
        #[cfg(target_os = "linux")]
        let (master, slave) = {
            let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
            let master = libc::open(c"/dev/ptmx".as_ptr(), flags);
            assert!(master >= 0, "cannot open /dev/ptmx");
            assert_eq!(libc::grantpt(master), 0, "grantpt failed");
            assert_eq!(libc::unlockpt(master), 0, "unlockpt failed");
            let mut name = [0 as libc::c_char; 128];
            assert_eq!(libc::ptsname_r(master, name.as_mut_ptr(), name.len()), 0, "ptsname_r");
            let slave = libc::open(name.as_ptr(), flags);
            assert!(slave >= 0, "cannot open the slave of a pseudo-terminal");
            (master, slave)
        };
        // Elsewhere close-on-exec is set right after `openpty`, which leaves a short window.
        #[cfg(not(target_os = "linux"))]
        let (master, slave) = {
            let (mut master, mut slave) = (0, 0);
            let rc = libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            assert_eq!(rc, 0, "openpty failed");
            for fd in [master, slave] {
                assert_eq!(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC), 0);
            }
            (master, slave)
        };
        Pty {
            master: std::fs::File::from_raw_fd(master),
            slave: OwnedFd::from_raw_fd(slave),
        }
    }
}

/// A pipe whose two descriptors are close-on-exec, as [`open_pty`]'s.
pub fn cloexec_pipe() -> [libc::c_int; 2] {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors.
    unsafe {
        #[cfg(target_os = "linux")]
        assert_eq!(libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC), 0);
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
            for fd in fds {
                assert_eq!(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC), 0);
            }
        }
    }
    fds
}

/// The signals whose default action the prompt rows assert (BL-169).
pub const PROMPT_SIGNALS: [libc::c_int; 4] =
    [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM];

/// Gives the [`PROMPT_SIGNALS`] their default action, in a child between fork and exec. A test
/// run as a background job of a non-interactive shell (as gate.sh and nightly.sh run their lanes)
/// inherits SIGINT and SIGQUIT ignored, and neither the test runner nor `Command` resets them: a
/// program the rows signal at its prompt would ignore the signal and wait there for good
/// (BL-169's hang). Only `signal`, which is async-signal-safe, runs.
///
/// # Safety
/// Only between fork and exec, or in a process of its own.
pub unsafe fn default_prompt_signals() {
    for signal in PROMPT_SIGNALS {
        // SAFETY: the caller's.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    }
}

/// Ignores the [`PROMPT_SIGNALS`], in a child between fork and exec, just before its
/// [`default_prompt_signals`]: the worst a test can inherit, in any shell, so that a child whose
/// reset went missing fails [`prompt_signals_ignored`] in a foreground shell too, not only in the
/// gate's background lanes (BL-169).
///
/// # Safety
/// As [`default_prompt_signals`].
pub unsafe fn ignore_prompt_signals() {
    for signal in PROMPT_SIGNALS {
        // SAFETY: the caller's.
        unsafe { libc::signal(signal, libc::SIG_IGN) };
    }
}

/// Why the program `pid` cannot be a prompt row's: it ignores one of the [`PROMPT_SIGNALS`]
/// that is not in `ignoring`, so a row that sends it would wait for an end that never comes
/// (BL-169). Read from `/proc/<pid>/status`, so Linux only.
pub fn prompt_signals_ignored(pid: libc::pid_t, ignoring: &[libc::c_int]) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let status = match std::fs::read_to_string(format!("/proc/{pid}/status")) {
            Ok(status) => status,
            Err(e) => return Some(format!("cannot read /proc/{pid}/status: {e}")),
        };
        let Some(ignored) = status
            .lines()
            .find_map(|l| l.strip_prefix("SigIgn:"))
            .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        else {
            return Some(format!("no SigIgn in /proc/{pid}/status"));
        };
        PROMPT_SIGNALS
            .into_iter()
            .find(|&s| ignored & (1 << (s - 1)) != 0 && !ignoring.contains(&s))
            .map(|s| {
                format!(
                    "its program ignores signal {s} (SigIgn {ignored:#x}), one the prompt rows send"
                )
            })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, ignoring);
        None
    }
}

/// How long one row's program, and the readers of its output, may take before the row fails by
/// name (BL-169). A row that waited without a deadline hung its whole test until the harness's
/// timeout, which named no row.
pub const ROW_DEADLINE: Duration = Duration::from_secs(90);

/// Waits for the program of row `row`; past `deadline`, kills it and fails the row.
pub fn wait_row_within(
    child: &mut std::process::Child,
    row: &str,
    deadline: Duration,
) -> std::process::ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if started.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("XCLI row {row:?}: its program still ran after {deadline:?}; killed");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// [`wait_row_within`] [`ROW_DEADLINE`].
pub fn wait_row(child: &mut std::process::Child, row: &str) -> std::process::ExitStatus {
    wait_row_within(child, row, ROW_DEADLINE)
}

/// Joins a reader ([`drain`]) of row `row`; past `deadline`, fails the row. A reader ends when
/// every descriptor of its stream is closed, so one that another process holds keeps it.
pub fn join_row_within(reader: std::thread::JoinHandle<()>, row: &str, deadline: Duration) {
    let started = Instant::now();
    while !reader.is_finished() {
        if started.elapsed() > deadline {
            panic!(
                "XCLI row {row:?}: its output was still open after {deadline:?} (another process \
                 holds a descriptor of it)"
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    reader.join().unwrap();
}

/// [`join_row_within`] [`ROW_DEADLINE`].
pub fn join_row(reader: std::thread::JoinHandle<()>, row: &str) {
    join_row_within(reader, row, ROW_DEADLINE);
}

/// `cmd.output()` for row `row`, with [`wait_row`] and [`join_row`]'s deadlines; standard input
/// is closed, as `output` has it.
pub fn output_row(cmd: &mut Command, row: &str) -> std::process::Output {
    let mut child =
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let (out, err) =
        (std::sync::Arc::new(Mutex::new(Vec::new())), std::sync::Arc::new(Mutex::new(Vec::new())));
    let readers = [
        drain(child.stdout.take().unwrap(), out.clone()),
        drain(child.stderr.take().unwrap(), err.clone()),
    ];
    let status = wait_row(&mut child, row);
    for r in readers {
        join_row(r, row);
    }
    let take = |b: std::sync::Arc<Mutex<Vec<u8>>>| std::mem::take(&mut *b.lock().unwrap());
    std::process::Output {
        status,
        stdout: take(out),
        stderr: take(err),
    }
}

/// Collect a stream on a thread, so a reply larger than a pipe buffer cannot block the child.
pub fn drain(
    mut r: impl std::io::Read + Send + 'static,
    into: std::sync::Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        // A pseudo-terminal master reports EIO once the last slave descriptor is closed.
        while let Ok(n) = r.read(&mut chunk) {
            if n == 0 {
                break;
            }
            into.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    })
}

/// Registry entry (class 1, DEC-201/DEC-03): rdash-cli's default data directory is
/// `~/.rdashcore` where stock's is `~/.dashcore`, and messages that name it differ by that
/// spelling only. Applied to rdash-cli's output.
fn default_datadir_spelling(bytes: Vec<u8>) -> Vec<u8> {
    fn replace(bytes: Vec<u8>, from: &[u8], to: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(from) {
                out.extend_from_slice(to);
                i += from.len();
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }
    replace(replace(bytes, b".rdashcore", b".dashcore"), b"RDashCore", b"DashCore")
}

/// Run `bin` with `HOME` set to `home` and an otherwise empty environment, so neither program
/// reads the invoking user's `~/.dashcore`. rdash-cli's output goes through
/// [`default_datadir_spelling`].
pub fn run(bin: &Path, home: &Path, inv: &Invocation) -> Output {
    run_until(bin, home, inv, &AtomicBool::new(false)).expect("never cancelled")
}

/// [`run`], unless `cancel` is set while the program runs: then the program is killed and there
/// is no output (BL-374).
pub fn run_until(bin: &Path, home: &Path, inv: &Invocation, cancel: &AtomicBool) -> Option<Output> {
    let mut out = run_raw_until(bin, home, inv, cancel)?;
    if bin.file_name().is_some_and(|n| n == "rdash-cli") {
        out.stdout = default_datadir_spelling(out.stdout);
        out.stderr = default_datadir_spelling(out.stderr);
    }
    Some(out)
}

/// [`run`] without the registry normalization.
pub fn run_raw(bin: &Path, home: &Path, inv: &Invocation) -> Output {
    run_raw_until(bin, home, inv, &AtomicBool::new(false)).expect("never cancelled")
}

/// [`run_until`] without the registry normalization.
fn run_raw_until(bin: &Path, home: &Path, inv: &Invocation, cancel: &AtomicBool) -> Option<Output> {
    let mut cmd = Command::new(bin);
    cmd.args(&inv.args)
        .env_clear()
        .env("HOME", home)
        // Coverage runs write their profile where the harness says, not into the test's
        // working directory.
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|v| ("LLVM_PROFILE_FILE", v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut stdin_pty = None;
    let mut stdout_pty = None;
    match inv.tty {
        Tty::None => {}
        Tty::Stdout => {
            let pty = open_pty();
            cmd.stdout(Stdio::from(pty.slave.try_clone().unwrap()));
            stdout_pty = Some(pty);
        }
        Tty::StdinReady | Tty::StdinPrompted => {
            let pty = open_pty();
            cmd.stdin(Stdio::from(pty.slave.try_clone().unwrap()));
            stdin_pty = Some(pty);
        }
    }
    if inv.tty == Tty::StdinReady {
        let pty = stdin_pty.as_ref().unwrap();
        (&pty.master).write_all(&inv.stdin).unwrap();
    }
    let mut child = cmd.spawn().unwrap_or_else(|e| panic!("cannot run {}: {e}", bin.display()));
    // The child holds the only slave descriptors that matter from here on.
    drop(cmd);
    let stdin_pty = stdin_pty.map(|p| (p.master, p.slave));
    let stdout_pty = stdout_pty.map(|p| (p.master, p.slave));

    let out_buf = std::sync::Arc::new(Mutex::new(Vec::new()));
    let err_buf = std::sync::Arc::new(Mutex::new(Vec::new()));
    let echo_buf = std::sync::Arc::new(Mutex::new(Vec::new()));
    let mut readers = Vec::new();
    readers.push(drain(child.stderr.take().unwrap(), err_buf.clone()));
    match (&stdout_pty, &stdin_pty) {
        (Some((master, _)), _) => {
            readers.push(drain(master.try_clone().unwrap(), out_buf.clone()));
        }
        _ => readers.push(drain(child.stdout.take().unwrap(), out_buf.clone())),
    }
    let mut prompted_input = None;
    match (&stdin_pty, inv.tty) {
        (Some((master, _)), Tty::StdinPrompted) => {
            readers.push(drain(master.try_clone().unwrap(), echo_buf.clone()));
            prompted_input = Some((master.try_clone().unwrap(), inv.stdin.clone()));
        }
        (Some(_), _) => {}
        (None, _) => {
            if let Some(mut stdin) = child.stdin.take() {
                // A program that exits without reading its standard input closes the pipe.
                let _ = stdin.write_all(&inv.stdin);
            }
        }
    }
    // This process keeps its slave descriptors until the child has exited: only then do the
    // readers of the master see the hang-up that ends them.
    let row = format!("{} {:?}", bin.display(), inv.args);
    let started = Instant::now();
    let status = loop {
        if prompted_input.is_some() && err_buf.lock().unwrap().ends_with(b"> ") {
            let (mut master, input) = prompted_input.take().unwrap();
            let _ = master.write_all(&input);
        }
        match child.try_wait().unwrap() {
            Some(status) => break Some(status),
            None if cancel.load(Ordering::Relaxed) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None if started.elapsed() > ROW_DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("XCLI row {row:?}: its program still ran after {ROW_DEADLINE:?}; killed");
            }
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    };
    drop(stdin_pty);
    drop(stdout_pty);
    for r in readers {
        join_row(r, &row);
    }
    let take = |b: std::sync::Arc<Mutex<Vec<u8>>>| std::mem::take(&mut *b.lock().unwrap());
    Some(Output {
        code: status?.code(),
        stdout: take(out_buf),
        stderr: take(err_buf),
        echo: take(echo_buf),
    })
}

/// Rewrites output before comparing: an entry of the allowed-differences registry. Each use
/// names the registry class and the reason in `Case::registry`.
pub type Normalize = fn(&[u8]) -> Vec<u8>;

/// One matrix row.
pub struct Case {
    pub group: &'static str,
    pub name: String,
    pub inv: Invocation,
    /// The stock binary's exit code. It guards against both programs failing identically for
    /// the wrong reason (a dead node, a typo in the case).
    pub expect_code: i32,
    /// Output that changes with time (`lastsend`, uptime): a pass needs one of up to five
    /// attempts to match, each started just after a second boundary.
    pub volatile: bool,
    /// An allowed-differences registry entry (class and reason) and its normalizer.
    pub registry: Option<(&'static str, Normalize)>,
}

impl Case {
    pub fn new(
        group: &'static str,
        name: impl Into<String>,
        inv: Invocation,
        expect_code: i32,
    ) -> Self {
        Case {
            group,
            name: name.into(),
            inv,
            expect_code,
            volatile: false,
            registry: None,
        }
    }

    pub fn volatile(mut self) -> Self {
        self.volatile = true;
        self
    }

    pub fn registry(mut self, reason: &'static str, f: Normalize) -> Self {
        self.registry = Some((reason, f));
        self
    }
}

/// A finished row.
#[derive(Clone, Debug)]
pub struct Row {
    pub group: &'static str,
    pub name: String,
    pub pass: bool,
    pub attempts: usize,
    pub registry: Option<&'static str>,
    pub detail: String,
}

impl Row {
    /// Names a failed row on standard error as it is recorded, past the test harness's capture;
    /// with `XCLI_ANNOUNCE=1`, every row, so a test killed from outside shows the last row it
    /// finished. A row past its deadline fails by name already (BL-169).
    fn announce(&self) {
        if !self.pass {
            let _ = writeln!(std::io::stderr(), "XCLI row [{}] {}: FAIL", self.group, self.name);
        } else if std::env::var_os("XCLI_ANNOUNCE").is_some_and(|v| v == "1") {
            let _ = writeln!(std::io::stderr(), "XCLI row [{}] {}: pass", self.group, self.name);
        }
    }
}

/// The rows of one test.
#[derive(Default)]
pub struct Matrix {
    rows: Mutex<Vec<Row>>,
}

fn lossy(bytes: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(bytes))
}

pub fn describe(label: &str, o: &Output) -> String {
    format!(
        "  {label}: code={:?}\n    stdout={}\n    stderr={}\n    echo={}\n",
        o.code,
        lossy(&o.stdout),
        lossy(&o.stderr),
        lossy(&o.echo)
    )
}

fn sleep_to_next_second_start() {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let into = now.subsec_millis();
    if into > 40 {
        std::thread::sleep(Duration::from_millis(1000 - u64::from(into) + 20));
    }
}

impl Matrix {
    /// Run `case` through both programs. `home` is an empty directory for `HOME`.
    pub fn compare(&self, tools: &Tools, home: &Path, case: &Case) -> Row {
        let attempts = if case.volatile {
            5
        } else {
            1
        };
        let mut last = String::new();
        for attempt in 1..=attempts {
            if case.volatile {
                sleep_to_next_second_start();
            }
            // Alternate the order so neither program is always second.
            let (ours, stock) = if attempt % 2 == 1 {
                let o = run(&tools.rdash, home, &case.inv);
                (o, run(&tools.stock, home, &case.inv))
            } else {
                let s = run(&tools.stock, home, &case.inv);
                (run(&tools.rdash, home, &case.inv), s)
            };
            let (mut a, mut b) = (ours.clone(), stock.clone());
            if let Some((_, f)) = case.registry {
                a.stdout = f(&a.stdout);
                a.stderr = f(&a.stderr);
                b.stdout = f(&b.stdout);
                b.stderr = f(&b.stderr);
            }
            if stock.code != Some(case.expect_code) {
                last = format!(
                    "premise failed: stock exit code {:?}, expected {}\n{}{}",
                    stock.code,
                    case.expect_code,
                    describe("stock", &stock),
                    describe("rdash", &ours)
                );
                continue;
            }
            if let Some(problem) = says::check(case.group, &case.name, case.expect_code, &stock) {
                last = format!(
                    "premise failed: {problem}\n{}{}",
                    describe("stock", &stock),
                    describe("rdash", &ours)
                );
                continue;
            }
            if a == b {
                return self.record(case, true, attempt, String::new());
            }
            last = format!(
                "outputs differ\n{}{}",
                describe("rdash", &ours),
                describe("stock", &stock)
            );
        }
        self.record(case, false, attempts, last)
    }

    pub fn record(&self, case: &Case, pass: bool, attempts: usize, detail: String) -> Row {
        let row = Row {
            group: case.group,
            name: case.name.clone(),
            pass,
            attempts,
            registry: case.registry.map(|(r, _)| r),
            detail,
        };
        row.announce();
        self.rows.lock().unwrap().push(row.clone());
        row
    }

    /// Record a row whose comparison the caller made (the fake-server rows compare requests too).
    pub fn record_named(
        &self,
        group: &'static str,
        name: String,
        pass: bool,
        detail: String,
    ) -> Row {
        let row = Row {
            group,
            name,
            pass,
            attempts: 1,
            registry: None,
            detail,
        };
        row.announce();
        self.rows.lock().unwrap().push(row.clone());
        row
    }

    /// Print the matrix; panic listing every failure.
    pub fn finish(&self, title: &str) {
        let rows = self.rows.lock().unwrap().clone();
        let mut groups: Vec<&'static str> = Vec::new();
        for r in &rows {
            if !groups.contains(&r.group) {
                groups.push(r.group);
            }
        }
        let mut report = format!(
            "## XCLI matrix: {title}\n\n| group | rows | pass | fail | registry entries |\n|---|---|---|---|---|\n"
        );
        let mut total = (0, 0);
        for g in &groups {
            let in_group: Vec<&Row> = rows.iter().filter(|r| r.group == *g).collect();
            let pass = in_group.iter().filter(|r| r.pass).count();
            let reg = in_group.iter().filter(|r| r.registry.is_some()).count();
            report.push_str(&format!(
                "| {g} | {} | {pass} | {} | {reg} |\n",
                in_group.len(),
                in_group.len() - pass
            ));
            total.0 += in_group.len();
            total.1 += pass;
        }
        report.push_str(&format!(
            "| **total** | {} | {} | {} | |\n",
            total.0,
            total.1,
            total.0 - total.1
        ));
        let retried = rows.iter().filter(|r| r.pass && r.attempts > 1).count();
        report.push_str(&format!("\nrows that needed a timing retry: {retried}\n"));
        report.push_str(
            "registry (every row): [class 1] rdash-cli's default data directory is ~/.rdashcore, stock's ~/.dashcore (DEC-201); its output is compared with `.rdashcore`/`RDashCore` spelled as stock's\n",
        );
        for r in rows.iter().filter(|r| r.registry.is_some()) {
            report.push_str(&format!(
                "registry: [{}] {} — {}\n",
                r.group,
                r.name,
                r.registry.unwrap()
            ));
        }
        println!("{report}");
        if let Some(dir) = std::env::var_os("XCLI_REPORT_DIR") {
            let dir = PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&dir);
            let mut full = report.clone();
            full.push_str("\n### rows\n\n");
            for r in &rows {
                full.push_str(&format!(
                    "- {} | {} | {}{}\n",
                    r.group,
                    r.name,
                    if r.pass {
                        "pass"
                    } else {
                        "FAIL"
                    },
                    if r.attempts > 1 {
                        format!(" (attempt {})", r.attempts)
                    } else {
                        String::new()
                    }
                ));
            }
            let _ = std::fs::write(dir.join(format!("{title}.md")), full);
        }
        let failures: Vec<&Row> = rows.iter().filter(|r| !r.pass).collect();
        if !failures.is_empty() {
            let mut msg = format!("{} XCLI rows failed:\n", failures.len());
            for r in failures {
                msg.push_str(&format!("\n[{}] {}\n{}", r.group, r.name, r.detail));
            }
            panic!("{msg}");
        }
    }
}
