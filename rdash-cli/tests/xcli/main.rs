//! XCLI (PRD §2): `rdash-cli` against the stock `dash-cli` v24.0.0-rc.3, byte for byte.
//!
//! `xcli_offline` needs only the stock binary: command-line and configuration handling, every
//! failure path of the HTTP client (against a scripted server), JSON output formatting, the
//! `-rpcwait` loop, request bytes (credentials, wallet URLs) and the parameter conversion of all
//! 289 commands, positional and `-named`, compared by the request each program sends.
//!
//! `xcli_dashd` runs both programs against live regtest dashd nodes (see `dashd.rs`).
//!
//! `xcli_deviations` checks the places where `rdash-cli` differs from the stock program on purpose.
//!
//! Skipped when `SKIP_DASHD_TESTS` is set; see `support::tools` for how the v24 binaries are found.
//!
//! **Platforms.** Full parity is proven on Linux only: the pseudo-terminal rows, the signal rows
//! and the address-resolution rows depend on Linux behaviour, and the stock binary and the live
//! nodes are exercised there. Other Unix systems run the portable subset of `xcli_offline` (the
//! command-line and configuration rows and the parameter conversion sweep) when a pinned dashd is
//! found, and list the rest as an ignored test with the reason; `xcli_unsupported.rs` does the
//! same where the harness cannot be built (not Unix). Nothing is compiled out without a trace.

#![cfg(unix)]

mod dashd;
mod support;

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use dash_rpc_spec::COMMANDS;
use support::fake::{FakeServer, Generator, Reply};
use support::{Case, Invocation, Matrix, Tools, Tty, free_port, home_dir, run, tools};

pub(crate) fn inv<S: AsRef<str>>(args: &[S]) -> Invocation {
    Invocation::new(args.iter().map(|s| s.as_ref()))
}

/// The arguments that aim a CLI at `port` with a user and password.
fn srv(port: u16) -> Vec<String> {
    vec![
        "-regtest".into(),
        format!("-rpcport={port}"),
        "-rpcuser=u".into(),
        "-rpcpassword=p".into(),
    ]
}

fn with(mut base: Vec<String>, more: &[&str]) -> Vec<String> {
    base.extend(more.iter().map(|s| s.to_string()));
    base
}

/// What a fake-server row's requests must look like, so that both programs failing the same way
/// before the step the row is about cannot pass. Checked on the stock binary's requests; the two
/// programs' requests are compared separately.
#[derive(Clone, Copy, Default)]
struct Expect {
    /// How many requests arrive; `None` leaves it to `support::says::requests` (at least one,
    /// unless the row is listed there).
    requests: Option<usize>,
    /// The RPC method every request names.
    method: Option<&'static str>,
}

impl Expect {
    /// Exactly one request, for `method`.
    const fn one(method: &'static str) -> Self {
        Expect {
            requests: Some(1),
            method: Some(method),
        }
    }

    fn check(&self, group: &str, name: &str, requests: &[Vec<u8>]) -> Option<String> {
        use support::says::{Requests, requests as expected};
        let got = requests.len();
        match (self.requests, expected(group, name)) {
            (Some(n), _) if got != n => {
                return Some(format!("stock got {got} requests, expected {n}"));
            }
            (None, Requests::AtLeastOne) if got == 0 => {
                return Some("stock got no request".to_string());
            }
            (None, Requests::NoneSent) if got != 0 => {
                return Some(format!("stock got {got} requests, expected none"));
            }
            _ => {}
        }
        let method = self.method?;
        let needle = format!("\"method\":\"{method}\"");
        requests
            .iter()
            .find(|r| !String::from_utf8_lossy(r).contains(&needle))
            .map(|r| format!("a request does not name {method}: {:?}", String::from_utf8_lossy(r)))
    }
}

/// Run both programs against `server` and compare outputs and the requests received.
#[allow(clippy::too_many_arguments)] // the row is its description
fn fake_row(
    m: &Matrix,
    t: &Tools,
    home: &Path,
    group: &'static str,
    name: &str,
    server: &FakeServer,
    inv: &Invocation,
    expect_code: i32,
) {
    fake_row_expect(m, t, home, group, name, server, inv, expect_code, Expect::default());
}

/// [`fake_row`] with the requests the row expects.
#[allow(clippy::too_many_arguments)] // the row is its description
fn fake_row_expect(
    m: &Matrix,
    t: &Tools,
    home: &Path,
    group: &'static str,
    name: &str,
    server: &FakeServer,
    inv: &Invocation,
    expect_code: i32,
    expect: Expect,
) {
    eprintln!("row [{group}] {name}");
    server.clear();
    let ours = run(&t.rdash, home, inv);
    let ours_requests = server.requests();
    server.clear();
    let stock = run(&t.stock, home, inv);
    let stock_requests = server.requests();
    let mut problems = String::new();
    if let Some(problem) = expect.check(group, name, &stock_requests) {
        problems.push_str(&format!("premise failed: {problem}\n"));
    }
    if let Some(problem) = support::says::check(group, name, expect_code, &stock) {
        problems.push_str(&format!(
            "premise failed: {problem}\n{}",
            support::describe("stock", &stock)
        ));
    }
    if stock.code != Some(expect_code) {
        problems.push_str(&format!(
            "premise failed: stock exit code {:?}, expected {expect_code}\n{}",
            stock.code,
            support::describe("stock", &stock)
        ));
    }
    if ours != stock {
        problems.push_str(&format!("outputs differ\n  rdash: {ours:?}\n  stock: {stock:?}\n"));
    }
    if ours_requests != stock_requests {
        problems.push_str(&format!(
            "requests differ\n  rdash: {:?}\n  stock: {:?}\n",
            ours_requests
                .iter()
                .map(|r| String::from_utf8_lossy(r).into_owned())
                .collect::<Vec<_>>(),
            stock_requests
                .iter()
                .map(|r| String::from_utf8_lossy(r).into_owned())
                .collect::<Vec<_>>()
        ));
    }
    m.record_named(group, name.to_string(), problems.is_empty(), problems);
}

/// `X-Big: aaa…` of `n` bytes, then CR LF.
fn limit_header_line(n: usize) -> Vec<u8> {
    let mut line = b"X-Big: ".to_vec();
    line.resize(n, b'a');
    line.extend_from_slice(b"\r\n");
    line
}

fn rpc_body(result: &str) -> String {
    format!("{{\"result\":{result},\"error\":null,\"id\":1}}\n")
}

fn error_body(error: &str) -> String {
    format!("{{\"result\":null,\"error\":{error},\"id\":1}}\n")
}

#[test]
fn xcli_offline() {
    let Some(t) = tools() else {
        return;
    };
    let home = home_dir();
    let work = tempfile::tempdir().unwrap();
    let m = Matrix::default();

    offline_rows(&m, &t, home.path(), work.path());
    if cfg!(target_os = "linux") {
        no_server_rows(&m, &t, home.path());
        http_failure_rows(&m, &t, home.path());
        output_format_rows(&m, &t, home.path());
        error_object_rows(&m, &t, home.path());
        rpcwait_rows(&m, &t, home.path());
        request_byte_rows(&m, &t, home.path(), work.path());
        named_rows(&m, &t, home.path());
        handler_fake_rows(&m, &t, home.path());
        stream_rows(&m, &t, home.path());
    } else {
        eprintln!(
            "XCLI: portable subset only on this platform (offline, convert); see xcli_full_parity_is_linux_only"
        );
    }
    conversion_sweep(&m, &t, home.path());

    m.finish("xcli_offline");
}

/// Where rdash-cli deliberately differs from the stock dash-cli, each checked for its own
/// behaviour (and, where stock is cheap to run, for the premise that stock behaves otherwise).
#[test]
fn xcli_deviations() {
    let Some(t) = tools() else {
        return;
    };
    if !cfg!(target_os = "linux") {
        eprintln!(
            "XCLI SKIPPED on this platform: the deviation rows use terminals and signals as Linux has them (see xcli_full_parity_is_linux_only)"
        );
        return;
    }
    let home = home_dir();
    let m = Matrix::default();
    deviation_rows(&m, &t, home.path());
    m.finish("xcli_deviations");
}

/// BL-169: a row whose program never ends fails by name within its deadline instead of hanging
/// the test, and so does a row whose output another process keeps open. The planted program is
/// BL-169's own: it ignores SIGINT (as every program did that the prompt rows started from a
/// test run as a background job, before `support::default_prompt_signals`), gets one, and holds
/// the slave of a pseudo-terminal, whose descriptors are close-on-exec. The check that keeps
/// the prompt rows' programs from starting so is `prompt_signals_name_what_a_program_ignores`.
#[test]
#[cfg(target_os = "linux")]
fn a_stuck_row_fails_by_name_within_its_deadline() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::time::Instant;

    let message = |panic: Box<dyn std::any::Any + Send>| {
        panic.downcast_ref::<String>().cloned().unwrap_or_default()
    };
    let pty = support::open_pty();
    for fd in [pty.master.as_raw_fd(), pty.slave.as_raw_fd()] {
        // SAFETY: a descriptor owned by `pty`.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "descriptor {fd} is inheritable");
    }
    let mut cmd = Command::new("sleep");
    cmd.arg("600")
        .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: only `signal`, which is async-signal-safe, runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    // SAFETY: signalling our own child, which ignores it.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let reader = support::drain(pty.master.try_clone().unwrap(), Arc::new(Mutex::new(Vec::new())));
    let deadline = Duration::from_secs(2);

    let started = Instant::now();
    let waited = catch_unwind(AssertUnwindSafe(|| {
        support::wait_row_within(&mut child, "planted", deadline)
    }));
    let failed = message(waited.expect_err("the stuck program's row passed"));
    assert!(failed.starts_with(r#"XCLI row "planted": "#), "{failed}");
    assert!(started.elapsed() < deadline + Duration::from_secs(10), "{:?}", started.elapsed());
    assert!(child.try_wait().unwrap().is_some(), "the stuck program was not killed");

    // The slave descriptor held here keeps the reader's stream open past its deadline.
    let started = Instant::now();
    let joined =
        catch_unwind(AssertUnwindSafe(|| support::join_row_within(reader, "held", deadline)));
    let failed = message(joined.expect_err("the held output's row passed"));
    assert!(failed.starts_with(r#"XCLI row "held": "#), "{failed}");
    assert!(started.elapsed() < deadline + Duration::from_secs(10), "{:?}", started.elapsed());
}

/// BL-169: a program that ignores a signal its prompt row sends is named before the row waits on
/// it. SIGINT and SIGQUIT are ignored here, as a background job of a non-interactive shell (the
/// gate's lanes) inherits them; `support::default_prompt_signals` gives a prompt row's program
/// back their default action, and the check finds no other ignored signal then.
#[test]
#[cfg(target_os = "linux")]
fn prompt_signals_name_what_a_program_ignores() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let spawn = |reset: bool| {
        let mut cmd = Command::new("sleep");
        cmd.arg("600").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        // SAFETY: only `signal`, which is async-signal-safe, runs between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                libc::signal(libc::SIGINT, libc::SIG_IGN);
                libc::signal(libc::SIGQUIT, libc::SIG_IGN);
                if reset {
                    support::default_prompt_signals();
                }
                Ok(())
            });
        }
        cmd.spawn().unwrap()
    };
    // `sleep` has exec'd once its command line is its own.
    let execed = |child: &std::process::Child| {
        let cmdline = format!("/proc/{}/cmdline", child.id());
        let started = std::time::Instant::now();
        while !std::fs::read(&cmdline).is_ok_and(|c| c.starts_with(b"sleep\0")) {
            assert!(started.elapsed() < support::ROW_DEADLINE, "sleep never exec'd");
            std::thread::sleep(Duration::from_millis(2));
        }
        child.id() as libc::pid_t
    };

    let mut inherited = spawn(false);
    let pid = execed(&inherited);
    let why = support::prompt_signals_ignored(pid, &[]);
    let allowed = support::prompt_signals_ignored(pid, &[libc::SIGINT, libc::SIGQUIT]);
    inherited.kill().unwrap();
    inherited.wait().unwrap();
    let why = why.expect("an inherited ignored SIGINT went unnoticed");
    assert!(why.contains(&format!("ignores signal {}", libc::SIGINT)), "{why}");
    assert_eq!(allowed, None, "a signal the row ignores on purpose was named");

    let mut reset = spawn(true);
    let why = support::prompt_signals_ignored(execed(&reset), &[]);
    reset.kill().unwrap();
    reset.wait().unwrap();
    assert_eq!(why, None, "the reset left a prompt signal ignored");
}

/// Says in the test list what this platform does not run. Full parity is proven on Linux only.
#[test]
#[cfg(not(target_os = "linux"))]
#[ignore = "XCLI full parity is proven on Linux only: the live-node, terminal, signal and address-resolution rows are not run here (xcli_offline runs the portable subset)"]
fn xcli_full_parity_is_linux_only() {
    eprintln!("xcli_dashd, xcli_deviations and most of xcli_offline run on Linux only");
}

#[test]
fn xcli_dashd() {
    let Some(t) = tools() else {
        return;
    };
    if !cfg!(target_os = "linux") {
        eprintln!(
            "XCLI SKIPPED on this platform: the live-node rows run on Linux only (see xcli_full_parity_is_linux_only)"
        );
        return;
    }
    let m = Matrix::default();
    dashd::run_all(&t, &m);
    m.finish("xcli_dashd");
}

/// Rows that need no server: help and version, argument and configuration errors.
fn offline_rows(m: &Matrix, t: &Tools, home: &Path, work: &Path) {
    let g = "offline";
    let dead = free_port(); // nobody listens
    let row = |name: &str, args: &[&str], code: i32| {
        m.compare(t, home, &Case::new(g, name, inv(args), code));
    };
    row("-version", &["-version"], 0);
    row("-version with other args", &["-version", "-regtest", "getblockcount"], 0);
    row("-help", &["-help"], 0);
    row("-h", &["-h"], 0);
    row("-?", &["-?"], 0);
    row("-help-debug is not an option", &["-help-debug"], 1);
    row("-nohelp", &["-nohelp", "-regtest", "x"], 0);
    row("no arguments", &[], 1);
    row("only an option", &["-regtest"], 1);
    row("empty argument", &[""], 1);
    row("unknown option", &["-bogus", "getblockcount"], 1);
    row("unknown option with value", &["-bogus=1"], 1);
    row("negated -rpcwaittimeout", &["-norpcwaittimeout", "x"], 1);
    row("negated -color", &["-nocolor", "x"], 1);
    row("-testnet and -regtest", &["-testnet", "-regtest", "x"], 1);
    row("-chain and -testnet", &["-chain=main", "-testnet", "x"], 1);
    row("-chain=bogus", &["-chain=bogus", "x"], 1);
    row("-devnet and -regtest", &["-devnet=a", "-regtest", "x"], 1);
    row("-devnet empty name", &["-devnet=", "x"], 1);
    row("missing datadir", &["-datadir=/nonexistent/dir", "getblockcount"], 1);
    let file = work.join("not-a-directory");
    std::fs::write(&file, b"x").unwrap();
    row("datadir is a file", &[&format!("-datadir={}", file.display()), "getblockcount"], 1);
    row("command line ends at first non-option", &["-regtest", "getblockcount", "-version"], 1);
    row("lone dash", &["-", "getblockcount"], 1);
    row("double dash option", &["--regtest", "--rpcport=1", "getblockcount"], 1);
    row("-getinfo with arguments", &["-regtest", "-getinfo", "x"], 1);
    row("-addrinfo with arguments", &["-regtest", "-addrinfo", "x"], 1);
    row("-netinfo invalid level", &["-regtest", "-netinfo", "x"], 1);
    row("-netinfo level 256", &["-regtest", "-netinfo", "256"], 1);
    row("-netinfo level -1 is an option", &["-regtest", "-netinfo", "-1"], 1);
    row("-netinfo level +1", &["-regtest", &format!("-rpcport={dead}"), "-netinfo", "+1"], 1);
    row("-netinfo level empty", &["-regtest", "-netinfo", ""], 1);
    row("-netinfo level with space", &["-regtest", "-netinfo", " 1"], 1);
    row("-netinfo help", &["-netinfo", "help"], 0);
    row("-netinfo help after options", &["-regtest", "-rpcport=1", "-netinfo", "help", "extra"], 0);
    row("-netinfo -help", &["-netinfo", "-help"], 0);
    row(
        "-stdinwalletpassphrase on other command",
        &["-regtest", "-stdinwalletpassphrase", "getblockcount"],
        1,
    );
    row("-stdinwalletpassphrase without command", &["-regtest", "-stdinwalletpassphrase"], 1);
    row(
        "-named bad JSON",
        &["-regtest", &format!("-rpcport={dead}"), "-named", "getblock", "verbosity={x"],
        1,
    );
    row(
        "positional bad JSON",
        &["-regtest", &format!("-rpcport={dead}"), "getblock", "x", "[1,"],
        1,
    );
    row(
        "bad JSON in quotes",
        &["-regtest", &format!("-rpcport={dead}"), "createrawtransaction", "'[1'", "{}"],
        1,
    );
    row(
        "-color invalid with -getinfo, no server",
        &["-regtest", &format!("-rpcport={dead}"), "-color=x", "-getinfo"],
        1,
    );

    // Configuration files.
    let conf = |name: &str, content: &str| {
        let dir = work.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dash.conf"), content).unwrap();
        dir
    };
    let d = conf("conf-bad-line", "rpcuser=a\nthis is not valid\n");
    row("conf: parse error", &[&format!("-datadir={}", d.display()), "x"], 1);
    let d = conf("conf-unknown-key", "bogus=1\nrpcuser=a\n");
    row(
        "conf: unknown key is ignored",
        &[&format!("-datadir={}", d.display()), "-regtest", &format!("-rpcport={dead}"), "x"],
        1,
    );
    let d = conf("conf-includeconf-missing", "includeconf=missing.conf\n");
    row(
        "conf: missing includeconf",
        &[&format!("-datadir={}", d.display()), "-regtest", &format!("-rpcport={dead}"), "x"],
        1,
    );
    let d = conf(
        "conf-sections",
        &format!("regtest=1\n[regtest]\nrpcport={dead}\nrpcuser=z\n[main]\nrpcport=1\n"),
    );
    row("conf: network section", &[&format!("-datadir={}", d.display()), "getblockcount"], 1);
    let d = conf("conf-chain", "testnet=1\nregtest=1\n");
    row("conf: conflicting chains", &[&format!("-datadir={}", d.display()), "x"], 1);
    row("-conf missing", &["-conf=/nonexistent.conf", "-regtest", "x"], 1);
    row("-noconf", &["-noconf", "-regtest", &format!("-rpcport={dead}"), "x"], 1);
    let d = conf("conf-rpcconnect", &format!("rpcconnect=127.0.0.1:{dead}\n"));
    row(
        "conf: rpcconnect with port",
        &[&format!("-datadir={}", d.display()), "-regtest", "getblockcount"],
        1,
    );
    let d = conf("conf-nonascii", "rpcuser=caf\u{e9}\n");
    row(
        "conf: non-ASCII value",
        &[&format!("-datadir={}", d.display()), "-regtest", &format!("-rpcport={dead}"), "x"],
        1,
    );
    let cookie_dir = work.join("conf-dir");
    std::fs::create_dir_all(cookie_dir.join("dash.conf")).unwrap();
    row(
        "conf: dash.conf is a directory",
        &[
            &format!("-datadir={}", cookie_dir.display()),
            "-regtest",
            &format!("-rpcport={dead}"),
            "x",
        ],
        1,
    );
}

/// A port nobody listens on, name resolution failures and odd `-rpcconnect` / `-rpcport` values.
fn no_server_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "no-server";
    let dead = free_port();
    let base = srv(dead);
    let row = |name: &str, more: &[&str], code: i32| {
        m.compare(t, home, &Case::new(g, name, Invocation::new(with(base.clone(), more)), code));
    };
    row("connection refused", &["getblockcount"], 1);
    row("-getinfo", &["-getinfo"], 1);
    row("-netinfo", &["-netinfo"], 1);
    row("-addrinfo", &["-addrinfo"], 1);
    row("-generate", &["-generate"], 1);
    row("-generate with -rpcwallet", &["-generate", "-rpcwallet=w"], 1);
    row("-named", &["-named", "getblock", "blockhash=x"], 1);
    row("-rpcwait with timeout", &["-rpcwait", "-rpcwaittimeout=2", "getblockcount"], 1);
    row("-rpcwait=0", &["-rpcwait=0", "getblockcount"], 1);
    row("-norpcwait", &["-norpcwait", "getblockcount"], 1);
    row("-rpcwait -getinfo timeout", &["-rpcwait", "-rpcwaittimeout=1", "-getinfo"], 1);

    let p = |h: &str| {
        vec![
            "-regtest".to_string(),
            format!("-rpcconnect={h}"),
            "-rpcport=1".to_string(),
            "getblockcount".to_string(),
        ]
    };
    for host in [
        "nonexistent.invalid",
        "",
        "999.999.999.999",
        "[::1]",
        "[::1]:1",
        "localhost:1",
        ":1",
        "127.0.0.1:0",
        "127.0.0.1:99999",
        "a:b:c",
        "::1",
        "[",
        "]",
        "[]",
        "127.0.0.1:",
        "127.0.0.1:+5",
        "127.0.0.1:-5",
        "127.0.0.1:5x",
        " 127.0.0.1",
        "0.0.0.0",
        "[::1",
        "::1]",
        "[::1]x:5",
        // Not refused but unreachable: libevent reports those as an EOF.
        "255.255.255.255",
        "224.0.0.1",
        "[ff02::1]",
        "[fe80::1]",
    ] {
        m.compare(
            t,
            home,
            &Case::new(g, format!("-rpcconnect={host:?}"), Invocation::new(p(host)), 1),
        );
    }
    // The port: -rpcport wins over the port in -rpcconnect, which wins over the default.
    for (name, args) in [
        (
            "rpcconnect port, no -rpcport",
            vec![
                "-regtest".to_string(),
                format!("-rpcconnect=127.0.0.1:{dead}"),
                "getblockcount".to_string(),
            ],
        ),
        (
            "rpcport beats rpcconnect port",
            vec![
                "-regtest".to_string(),
                "-rpcconnect=127.0.0.1:2".to_string(),
                format!("-rpcport={dead}"),
                "getblockcount".to_string(),
            ],
        ),
        ("default regtest port", vec!["-regtest".to_string(), "getblockcount".to_string()]),
        ("default main port", vec!["getblockcount".to_string()]),
        ("default testnet port", vec!["-testnet".to_string(), "getblockcount".to_string()]),
        ("default devnet port", vec!["-devnet=x".to_string(), "getblockcount".to_string()]),
    ] {
        m.compare(t, home, &Case::new(g, name, Invocation::new(args), 1));
    }
    for port in [
        "0",
        "-1",
        "65535",
        "65536",
        "70000",
        "abc",
        "",
        "1x",
        " 7",
        "+7",
        "1e3",
        "99999999999999999999",
        "-99999999999999999999",
    ] {
        m.compare(
            t,
            home,
            &Case::new(
                g,
                format!("-rpcport={port:?}"),
                inv(&["-regtest", &format!("-rpcport={port}"), "getblockcount"]),
                1,
            ),
        );
    }
    m.compare(
        t,
        home,
        &Case::new(g, "-norpcport", inv(&["-regtest", "-norpcport", "getblockcount"]), 1),
    );
}

/// Every way the HTTP exchange can fail or answer unusually.
fn http_failure_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "http";
    let ok = rpc_body("\"hi\"");
    let cases: Vec<(&str, Reply, i32)> = vec![
        ("server closes without a reply", Reply::Close, 1),
        ("garbage instead of a status line", Reply::Raw(b"hello world\r\n\r\n".to_vec()), 1),
        ("status line without a version", Reply::Raw(b"200\r\n\r\n".to_vec()), 1),
        ("HTTP/2 status line", Reply::Raw(b"HTTP/2 200 OK\r\n\r\n".to_vec()), 1),
        ("status code 0", Reply::Raw(b"HTTP/1.1 0 Zero\r\nContent-Length: 0\r\n\r\n".to_vec()), 1),
        ("status code not a number", Reply::Raw(b"HTTP/1.1 abc OK\r\nContent-Length: 0\r\n\r\n".to_vec()), 1),
        ("header without a colon", Reply::Raw(b"HTTP/1.1 200 OK\r\nbroken header\r\n\r\n".to_vec()), 1),
        ("body shorter than Content-Length", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n{\"result\":1}".to_vec()), 1),
        ("Content-Length not a number", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n{}".to_vec()), 1),
        ("status line only, then close", Reply::Raw(b"HTTP/1.1 200 OK\r\n".to_vec()), 1),
        ("401", Reply::http("401 Unauthorized", ""), 1),
        ("401 with body", Reply::http("401 Unauthorized", "no"), 1),
        ("403", Reply::http("403 Forbidden", ""), 1),
        ("404 empty", Reply::http("404 Not Found", ""), 1),
        ("404 with JSON error", Reply::http("404 Not Found", &error_body("{\"code\":-32601,\"message\":\"Method not found\"}")), 89),
        ("400 with JSON error", Reply::http("400 Bad Request", &error_body("{\"code\":-32700,\"message\":\"Parse error\"}")), 188),
        ("500 empty", Reply::http("500 Internal Server Error", ""), 1),
        ("503 with text", Reply::http("503 Service Unavailable", "Work queue depth exceeded"), 1),
        ("503 empty", Reply::http("503 Service Unavailable", ""), 1),
        ("301", Reply::http("301 Moved Permanently", ""), 1),
        ("502 with JSON", Reply::http("502 Bad Gateway", &ok), 1),
        ("200 empty body", Reply::http("200 OK", ""), 1),
        ("200 not JSON", Reply::http("200 OK", "abc"), 1),
        ("200 trailing garbage", Reply::http("200 OK", "{\"result\":1,\"error\":null,\"id\":1} x"), 1),
        ("200 array", Reply::http("200 OK", "[]"), 1),
        ("200 number", Reply::http("200 OK", "5"), 1),
        ("200 string", Reply::http("200 OK", "\"x\""), 1),
        ("200 null", Reply::http("200 OK", "null"), 1),
        ("200 empty object", Reply::http("200 OK", "{}"), 1),
        ("200 object without result", Reply::http("200 OK", "{\"error\":null,\"id\":1}"), 0),
        ("200 object without error", Reply::http("200 OK", "{\"result\":\"r\",\"id\":1}"), 0),
        ("200 object with unrelated keys", Reply::http("200 OK", "{\"x\":1}"), 0),
        ("200 ok", Reply::json(&ok), 0),
        ("HTTP/1.0 reply read to close", Reply::Raw(format!("HTTP/1.0 200 OK\r\n\r\n{ok}").into_bytes()), 0),
        ("HTTP/1.1 without Content-Length, read to close", Reply::Raw(format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{ok}").into_bytes()), 0),
        ("chunked body", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 0),
        ("chunked body in pieces", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n{\"result\r\n14\r\n\":\"hi\",\"error\":null,\r\n8\r\n\"id\":1}\n\r\n0\r\n\r\n".to_vec()), 0),
        ("chunked with bad size", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("chunked cut short", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n20\r\n{}".to_vec()), 1),
        ("chunk size with extension", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2;ext=1\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("chunk size followed by a space", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2 junk\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("chunk size 0x prefix", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0x2\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("chunk size negative", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n-2\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("chunk size leading space", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n 2\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("chunk size empty line first", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\r\n2\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("chunk size 8 hex digits overflow", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFFFFFFFFFFF\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("chunked, no terminating chunk, then close", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n".to_vec()), 1),
        ("chunked, last chunk without trailer CRLF", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n".to_vec()), 1),
        ("chunked with trailer", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\nX-T: 1\r\n\r\n".to_vec()), 1),
        ("Transfer-Encoding: identity with Content-Length", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: identity\r\nContent-Length: 2\r\n\r\n{}".to_vec()), 1),
        ("Transfer-Encoding: gzip is chunked", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n2\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("Transfer-Encoding and Content-Length", Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n2\r\n{}\r\n0\r\n\r\n".to_vec()), 1),
        ("Content-Length negative", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n{}".to_vec()), 1),
        ("Content-Length with trailing junk", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 2x\r\n\r\n{}".to_vec()), 1),
        ("Content-Length with spaces", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length:   2  \r\n\r\n{}".to_vec()), 1),
        ("Content-Length plus sign", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\n{}".to_vec()), 1),
        ("Content-Length 60000000 and a short body", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 60000000\r\n\r\n{}".to_vec()), 1),
        ("Content-Length overflows", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999\r\n\r\n{}".to_vec()), 1),
        ("Content-Length i64 max", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 9223372036854775807\r\n\r\n{}".to_vec()), 1),
        ("Content-Length hex", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 0x2\r\n\r\n{}".to_vec()), 1),
        ("Content-Length empty", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length:\r\n\r\n{}".to_vec()), 1),
        ("header value trailing tab", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 2\t\r\n\r\n{}".to_vec()), 1),
        ("header name with space", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent Length: 2\r\n\r\n{}".to_vec()), 1),
        ("header with empty name", Reply::Raw(b"HTTP/1.1 200 OK\r\n: 2\r\n\r\n{}".to_vec()), 1),
        ("Content-Length twice", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 9\r\n\r\n{}".to_vec()), 1),
        ("header block above 32 KiB", Reply::Raw([b"HTTP/1.1 200 OK\r\nX-Big: ".to_vec(), vec![b'a'; 40000], b"\r\nContent-Length: 2\r\n\r\n{}".to_vec()].concat()), 1),
        ("many headers", Reply::Raw([b"HTTP/1.1 200 OK\r\n".to_vec(), "X: 1\r\n".repeat(999).into_bytes(), b"Content-Length: 2\r\n\r\n{}".to_vec()].concat()), 1),
        ("status line above 8 KiB", Reply::Raw([b"HTTP/1.1 200 ".to_vec(), vec![b'a'; 9000], b"\r\nContent-Length: 2\r\n\r\n{}".to_vec()].concat()), 1),
        ("101 Switching Protocols", Reply::Raw(b"HTTP/1.1 101 Switching Protocols\r\n\r\n".to_vec()), 1),
        ("102 Processing then 200", Reply::Raw(b"HTTP/1.1 102 Processing\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec()), 1),
        ("lone CR line endings", Reply::Raw(b"HTTP/1.1 200 OK\rContent-Length: 2\r\r{}".to_vec()), 1),
        ("status line without a reason", Reply::Raw(format!("HTTP/1.1 200\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("LF-only line endings", Reply::Raw(format!("HTTP/1.1 200 OK\nContent-Length: {}\n\n{ok}", ok.len()).into_bytes()), 0),
        ("lower-case header names", Reply::Raw(format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("folded header", Reply::Raw(format!("HTTP/1.1 200 OK\r\nX-Folded: a\r\n b\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("fold with no previous header", Reply::Raw(format!("HTTP/1.1 200 OK\r\n folded\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("extra bytes after the body", Reply::Raw(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{ok}EXTRA", ok.len()).into_bytes()), 0),
        (
            // With the peer closing together with the data the request fails instead, but
            // whether libevent sees the close first is a race in the stock binary.
            "100 Continue and the reply in one segment, connection kept open",
            Reply::Pieces(
                Duration::from_millis(100),
                vec![(
                    format!(
                        "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{ok}",
                        ok.len()
                    )
                    .into_bytes(),
                    Duration::from_millis(400),
                )],
            ),
            0,
        ),
        (
            "100 Continue, the reply 200 ms later",
            Reply::Pieces(
                Duration::from_millis(100),
                vec![
                    (b"HTTP/1.1 100 Continue\r\n\r\n".to_vec(), Duration::from_millis(200)),
                    (
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes(),
                        Duration::from_millis(100),
                    ),
                ],
            ),
            0,
        ),
        ("204 No Content", Reply::Raw(b"HTTP/1.1 204 No Content\r\n\r\n".to_vec()), 1),
        ("200 with Content-Length 0 and body bytes", Reply::Raw(format!("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n{ok}").into_bytes()), 1),
        ("binary body", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n\xff\xfe\xfd".to_vec()), 1),
        ("invalid UTF-8 inside a string", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 38\r\n\r\n{\"result\":\"a\xffb\",\"error\":null,\"id\":1}\n".to_vec()), 1),
        ("two JSON documents", Reply::http("200 OK", "{\"result\":1,\"error\":null,\"id\":1}{\"result\":2}"), 1),
        ("UTF-8 BOM", Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 38\r\n\r\n\xef\xbb\xbf{\"result\":1,\"error\":null,\"id\":1}\n".to_vec()), 1),
        // libevent's header and status-line parsing, one quirk per row.
        ("keep-alive without Content-Length", Reply::Raw(format!("HTTP/1.1 200 OK\r\nConnection: keep-alive\r\n\r\n{ok}").into_bytes()), 1),
        ("Connection: CLOSE without Content-Length", Reply::Raw(format!("HTTP/1.1 200 OK\r\nConnection: CLOSE\r\n\r\n{ok}").into_bytes()), 0),
        ("keep-alive with Content-Length", Reply::Raw(format!("HTTP/1.1 200 OK\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("Transfer-Encoding value led by a tab", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding:\tchunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 1),
        ("Transfer-Encoding value led by two spaces", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding:  chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 0),
        ("Transfer-Encoding value followed by a tab", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\t\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 0),
        ("Transfer-Encoding folded in the middle", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chun\r\n ked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 1),
        ("Content-Length folded in the middle", Reply::Raw(format!("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n 7\r\n\r\n{ok}").into_bytes()), 1),
        ("Content-Length folded onto the next line", Reply::Raw(format!("HTTP/1.1 200 OK\r\nContent-Length:\r\n {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("fold with trailing blanks", Reply::Raw(format!("HTTP/1.1 200 OK\r\nX-F: a\r\n \tb \t\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("bare CR inside a header value", Reply::Raw(format!("HTTP/1.1 200 OK\r\nX-A: a\rb\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("bare CR followed by a blank inside a header value", Reply::Raw(format!("HTTP/1.1 200 OK\r\nX-A: a\r b\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("bare CR ending a header value", Reply::Raw(format!("HTTP/1.1 200 OK\r\nX-A: a\r\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("header name with a bare CR", Reply::Raw(format!("HTTP/1.1 200 OK\r\nX\rA: a\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("header line starting with NUL ends the headers", Reply::Raw(format!("HTTP/1.1 200 OK\r\n\0junk\r\n{ok}").into_bytes()), 0),
        ("NUL inside a header line", Reply::Raw(format!("HTTP/1.1 200 OK\r\nX-A: a\0b\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("NUL inside the Content-Length value", Reply::Raw(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\0junk\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("vertical tab after the Content-Length value", Reply::Raw(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\x0b\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("form feed after the Content-Length value", Reply::Raw(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\x0c\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("HTTP/1.2", Reply::Raw(format!("HTTP/1.2 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("HTTP/0.9", Reply::Raw(format!("HTTP/0.9 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("HTTP/1.01", Reply::Raw(format!("HTTP/1.01 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("HTTP/01.1", Reply::Raw(format!("HTTP/01.1 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("HTTP/-1.1", Reply::Raw(format!("HTTP/-1.1 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("HTTP/2.0", Reply::Raw(format!("HTTP/2.0 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("HTTP/1.1x", Reply::Raw(format!("HTTP/1.1x 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("HTTP/1.", Reply::Raw(format!("HTTP/1. 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("HTTP/1", Reply::Raw(format!("HTTP/1 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("HTTP/ tab before the major version", Reply::Raw(format!("HTTP/\t1.1 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("HTTP/1.1 with a tab after it", Reply::Raw(format!("HTTP/1.1\t 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("lower-case http/1.1", Reply::Raw(format!("http/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("status code 4294967496 wraps to 200", Reply::Raw(format!("HTTP/1.1 4294967496 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("status code 4294967296 wraps to 0", Reply::Raw(format!("HTTP/1.1 4294967296 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("status code beyond long", Reply::Raw(format!("HTTP/1.1 99999999999999999999 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("status code +200", Reply::Raw(format!("HTTP/1.1 +200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("status code 200abc", Reply::Raw(format!("HTTP/1.1 200abc OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("status code negative", Reply::Raw(format!("HTTP/1.1 -5 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("two spaces before the status code", Reply::Raw(format!("HTTP/1.1  200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("tab before the status code", Reply::Raw(format!("HTTP/1.1 \t200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("no-break space before the status code", Reply::Raw(format!("HTTP/1.1 \u{a0}200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 1),
        ("status line with NUL in the reason", Reply::Raw(format!("HTTP/1.1 200 O\0K\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()), 0),
        ("chunked trailer starting with a fold", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n x\r\n\r\n", ok.len(), ok).into_bytes()), 0),
        ("chunked trailer with a bad header", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\nbroken\r\n\r\n", ok.len(), ok).into_bytes()), 1),
        ("chunk size line starting with NUL", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\0\r\n{:x}\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 0),
        ("chunk size followed by NUL and junk", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\0junk\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 0),
        ("chunk size 0x-prefixed hex", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0x{:x}\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 0),
        ("chunk size with a plus sign", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n+{:x}\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 0),
        ("chunk size, leading tab", Reply::Raw(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\t{:x}\r\n{}\r\n0\r\n\r\n", ok.len(), ok).into_bytes()), 0),
        ("100 Continue with Content-Length, then the reply", Reply::Pieces(
            Duration::from_millis(100),
            vec![
                (b"HTTP/1.1 100 Continue\r\nContent-Length: 5\r\n\r\n".to_vec(), Duration::from_millis(200)),
                (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes(), Duration::from_millis(100)),
            ],
        ), 1),
        ("100 Continue with a fold on the real reply's first header", Reply::Pieces(
            Duration::from_millis(100),
            vec![
                (b"HTTP/1.1 100 Continue\r\nX-A: a\r\n\r\n".to_vec(), Duration::from_millis(200)),
                (format!("HTTP/1.1 200 OK\r\n b\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes(), Duration::from_millis(100)),
            ],
        ), 0),
        ("503 with a body that is not UTF-8", Reply::Raw(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 3\r\n\r\na\xffb".to_vec()), 1),
        ("503 with a NUL in the body", Reply::Raw(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 3\r\n\r\na\0b".to_vec()), 1),
        // Just inside rdash-cli's limits (`http::Limits::DEFAULT`), which stock does not have;
        // the `deviations` group has the replies just beyond.
        ("header line of exactly 64 KiB", Reply::Raw([b"HTTP/1.1 200 OK\r\n".to_vec(), limit_header_line(64 * 1024), format!("Content-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()].concat()), 0),
        ("status line of exactly 64 KiB", Reply::Raw([b"HTTP/1.1 200 ".to_vec(), vec![b'a'; 64 * 1024 - 13], format!("\r\nContent-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()].concat()), 0),
        ("1000 headers", Reply::Raw([b"HTTP/1.1 200 OK\r\n".to_vec(), "X: 1\r\n".repeat(999).into_bytes(), format!("Content-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()].concat()), 0),
        ("header bytes just under 256 KiB", Reply::Raw([b"HTTP/1.1 200 OK\r\n".to_vec(), limit_header_line(65_000).repeat(4), format!("Content-Length: {}\r\n\r\n{ok}", ok.len()).into_bytes()].concat()), 0),
        ("header bytes just under 256 KiB, in the trailer", Reply::Raw([b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(), format!("{:x}\r\n{ok}\r\n0\r\n", ok.len()).into_bytes(), limit_header_line(65_000).repeat(4), b"\r\n".to_vec()].concat()), 0),
    ];
    for (name, reply, code) in cases {
        let server = FakeServer::always(reply);
        let i = Invocation::new(with(srv(server.port), &["getblockcount"]));
        fake_row_expect(m, t, home, g, name, &server, &i, code, Expect::one("getblockcount"));
    }

    // No password and no cookie: 401 names the configuration file.
    let server = FakeServer::always(Reply::http("401 Unauthorized", ""));
    let args = vec![
        "-regtest".to_string(),
        format!("-rpcport={}", server.port),
        "getblockcount".to_string(),
    ];
    fake_row(m, t, home, g, "401 with no credentials at all", &server, &Invocation::new(args), 1);
    let args = vec![
        "-regtest".to_string(),
        format!("-rpcport={}", server.port),
        "-conf=/some/where/else.conf".to_string(),
        "getblockcount".to_string(),
    ];
    fake_row(
        m,
        t,
        home,
        g,
        "401 with no credentials, -conf in the message",
        &server,
        &Invocation::new(args),
        1,
    );

    // The reply never comes. The server holds the connection far past the clients' 1 s timeout,
    // so a client starved under load still times out ("timeout reached") rather than seeing
    // the server close ("EOF reached") (BL-441). Each connection has its own detached thread
    // (`Drop` joins only the accept loop), so the row does not wait for it. The hold is longer
    // than ROW_DEADLINE on purpose: a client that ignored the timeout fails the row by name.
    let server = FakeServer::always(Reply::Hang(Duration::from_secs(120)));
    let i = Invocation::new(with(srv(server.port), &["-rpcclienttimeout=1", "getblockcount"]));
    fake_row(m, t, home, g, "no reply, -rpcclienttimeout=1", &server, &i, 1);
    let server = FakeServer::always(Reply::json(&ok));
    for (name, extra) in [
        ("-rpcclienttimeout=0", "-rpcclienttimeout=0"),
        ("-rpcclienttimeout=-5", "-rpcclienttimeout=-5"),
        ("-norpcclienttimeout", "-norpcclienttimeout"),
        ("-rpcclienttimeout=abc", "-rpcclienttimeout=abc"),
    ] {
        let i = Invocation::new(with(srv(server.port), &[extra, "getblockcount"]));
        fake_row(m, t, home, g, name, &server, &i, 0);
    }
}

/// How results are printed: strings raw, everything else pretty-printed with two spaces.
fn output_format_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "output";
    let results: Vec<(&str, &str)> = vec![
        ("string", "\"hi\""),
        (
            "string with escapes",
            "\"line1\\nline2 \\u00e9 \\ud83d\\ude00 \\\"q\\\" \\/ \\\\ \\t \\b \\f \\r\"",
        ),
        ("string with control character", "\"a\\u0001\\u001fb\""),
        ("string with NUL escape", "\"a\\u0000b\""),
        ("empty string", "\"\""),
        ("number", "5"),
        ("negative zero", "-0"),
        ("float", "1.0"),
        ("exponent", "1E+2"),
        ("small exponent", "1e-7"),
        ("long decimal", "0.12345678901234567890"),
        ("huge integer", "123456789012345678901234567890"),
        ("true", "true"),
        ("false", "false"),
        ("null", "null"),
        ("empty array", "[]"),
        ("empty object", "{}"),
        ("array of numbers", "[1,2,3]"),
        ("array of strings", "[\"a\",\"b\"]"),
        ("nested", "{\"a\":[1,2,{\"b\":null}],\"c\":{\"d\":\"e\"},\"f\":[],\"g\":{}}"),
        ("array in array", "[[],[[]],[[1]]]"),
        ("duplicate keys", "{\"a\":1,\"a\":2}"),
        ("escaped key", "{\"k\\\"ey\\n\":\"v\"}"),
        ("unicode key and value", "{\"\u{e9}\u{4e2d}\":\"\u{1f600}\"}"),
        ("whitespace in the document", "  {  \"a\" :  [ 1 , 2 ]  }  "),
        ("deep nesting", "DEEP"),
    ];
    for (name, result) in results {
        let result = if result == "DEEP" {
            format!("{}{}", "[".repeat(100), "]".repeat(100))
        } else {
            result.to_string()
        };
        let server = FakeServer::always(Reply::json(&rpc_body(&result)));
        let i = Invocation::new(with(srv(server.port), &["getblockcount"]));
        fake_row(m, t, home, g, name, &server, &i, 0);
    }
    // A large document.
    let big = format!(
        "[{}]",
        (0..2000).map(|i| format!("{{\"i\":{i},\"s\":\"x{i}\"}}")).collect::<Vec<_>>().join(",")
    );
    let server = FakeServer::always(Reply::json(&rpc_body(&big)));
    fake_row(
        m,
        t,
        home,
        g,
        "large array of objects",
        &server,
        &Invocation::new(with(srv(server.port), &["getblockcount"])),
        0,
    );
    // Replies where `result` is not the first key, or a reply with extra members.
    let server = FakeServer::always(Reply::json(
        "{\"id\":1,\"error\":null,\"result\":{\"b\":1},\"extra\":true}",
    ));
    fake_row(
        m,
        t,
        home,
        g,
        "members in another order",
        &server,
        &Invocation::new(with(srv(server.port), &["getblockcount"])),
        0,
    );
}

/// `error` members of every shape: what `ParseError` prints and the exit code it picks.
fn error_object_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "errors";
    let errors: Vec<(&str, &str, i32)> = vec![
        ("code and message", "{\"code\":-5,\"message\":\"Invalid address\"}", 5),
        (
            "wallet not specified (-19)",
            "{\"code\":-19,\"message\":\"Wallet file not specified\"}",
            19,
        ),
        ("warm-up (-28)", "{\"code\":-28,\"message\":\"Loading block index...\"}", 28),
        ("code only", "{\"code\":-5}", 5),
        ("message only", "{\"message\":\"x\"}", 1),
        ("message not a string", "{\"code\":-5,\"message\":7}", 5),
        ("code is a string", "{\"code\":\"-5\",\"message\":\"x\"}", 1),
        ("code is float", "{\"code\":-5.5,\"message\":\"x\"}", 1),
        ("code zero", "{\"code\":0,\"message\":\"zero\"}", 0),
        ("code positive", "{\"code\":7,\"message\":\"x\"}", 7),
        ("code 300 wraps", "{\"code\":-300,\"message\":\"x\"}", 44),
        ("code 256 wraps to 0", "{\"code\":256,\"message\":\"x\"}", 0),
        ("code out of int range", "{\"code\":2147483648,\"message\":\"x\"}", 1),
        ("code INT_MIN", "{\"code\":-2147483648,\"message\":\"x\"}", 0),
        ("code is null", "{\"code\":null,\"message\":\"x\"}", 1),
        ("empty error object", "{}", 1),
        ("error is a string", "\"boom\"", 1),
        ("error is a number", "5", 1),
        ("error is true", "true", 1),
        ("error is an array", "[]", 1),
        ("message with newline", "{\"code\":-1,\"message\":\"a\\nb\"}", 1),
        ("message with unicode", "{\"code\":-1,\"message\":\"caf\\u00e9\"}", 1),
        ("extra members", "{\"code\":-1,\"message\":\"m\",\"data\":{\"x\":1}}", 1),
    ];
    for (name, error, code) in errors {
        let server =
            FakeServer::always(Reply::http("500 Internal Server Error", &error_body(error)));
        let i = Invocation::new(with(srv(server.port), &["getblockcount"]));
        // The premise: an exit code that is the error code's low byte (or 1 when it cannot be read).
        fake_row(m, t, home, g, name, &server, &i, code);
    }
    // `result` is ignored when `error` is set; both set.
    let server = FakeServer::always(Reply::http(
        "500 Internal Server Error",
        "{\"result\":\"r\",\"error\":{\"code\":-1,\"message\":\"m\"},\"id\":1}",
    ));
    fake_row(
        m,
        t,
        home,
        g,
        "result and error both set",
        &server,
        &Invocation::new(with(srv(server.port), &["getblockcount"])),
        1,
    );
    // The same error with HTTP 200.
    let server =
        FakeServer::always(Reply::http("200 OK", &error_body("{\"code\":-8,\"message\":\"m\"}")));
    fake_row(
        m,
        t,
        home,
        g,
        "error with HTTP 200",
        &server,
        &Invocation::new(with(srv(server.port), &["getblockcount"])),
        8,
    );
}

/// `-rpcwait` and `-rpcwaittimeout` against servers that start late or never answer properly.
fn rpcwait_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "rpcwait";
    let warmup = || {
        Reply::http(
            "500 Internal Server Error",
            &error_body("{\"code\":-28,\"message\":\"Loading block index...\"}"),
        )
    };

    // Each program gets its own server, so the script restarts for it.
    let scripted = |name: &str,
                    flaky: usize,
                    flaky_reply: fn() -> Reply,
                    extra: &[&str],
                    code: i32| {
        let mk = || {
            FakeServer::start(move |i, _| {
                if i < flaky {
                    flaky_reply()
                } else {
                    Reply::json(&rpc_body("\"ready\""))
                }
            })
        };
        let args = |port| Invocation::new(with(srv(port), &[extra, &["getblockcount"]].concat()));
        let (a, b) = (mk(), mk());
        let ours = run(&t.rdash, home, &args(a.port));
        let stock = run(&t.stock, home, &args(b.port));
        let mut problems = String::new();
        if stock.code != Some(code) {
            problems.push_str(&format!(
                "premise failed: stock exit code {:?}, expected {code}\n{}",
                stock.code,
                support::describe("stock", &stock)
            ));
        }
        // The port is part of some messages: compare with it masked.
        let mask = |o: &support::Output, p: u16| support::Output {
            code: o.code,
            stdout: String::from_utf8_lossy(&o.stdout).replace(&p.to_string(), "PORT").into_bytes(),
            stderr: String::from_utf8_lossy(&o.stderr).replace(&p.to_string(), "PORT").into_bytes(),
            echo: o.echo.clone(),
        };
        if mask(&ours, a.port) != mask(&stock, b.port) {
            problems.push_str(&format!("outputs differ\n  rdash: {ours:?}\n  stock: {stock:?}\n"));
        }
        // A loop that retries until a deadline makes one attempt more or less when the box
        // is loaded and a process starts a second late.
        let slack = if flaky >= 1000 {
            1
        } else {
            0
        };
        if a.requests().len().abs_diff(b.requests().len()) > slack {
            problems.push_str(&format!(
                "connection counts differ: {} vs {}\n",
                a.requests().len(),
                b.requests().len()
            ));
        }
        m.record_named(g, name.to_string(), problems.is_empty(), problems);
    };
    scripted("warm-up twice, then ready", 2, warmup, &["-rpcwait"], 0);
    scripted("warm-up, no -rpcwait", 2, warmup, &[], 28);
    scripted(
        "warm-up forever, -rpcwaittimeout=2",
        1000,
        warmup,
        &["-rpcwait", "-rpcwaittimeout=2"],
        1,
    );
    scripted("closed connection twice, then ready", 2, || Reply::Close, &["-rpcwait"], 0);
    scripted("closed connection, no -rpcwait", 2, || Reply::Close, &[], 1);
    scripted(
        "closed forever, -rpcwaittimeout=1",
        1000,
        || Reply::Close,
        &["-rpcwait", "-rpcwaittimeout=1"],
        1,
    );
    scripted(
        "401 is not retried",
        1000,
        || Reply::http("401 Unauthorized", ""),
        &["-rpcwait", "-rpcwaittimeout=3"],
        1,
    );
    scripted(
        "empty body is not retried",
        1000,
        || Reply::http("200 OK", ""),
        &["-rpcwait", "-rpcwaittimeout=3"],
        1,
    );
    scripted(
        "other JSON-RPC error is not retried",
        1000,
        || Reply::http("500 Internal Server Error", &error_body("{\"code\":-5,\"message\":\"m\"}")),
        &["-rpcwait", "-rpcwaittimeout=3"],
        5,
    );
    scripted(
        "warm-up error without a code",
        1000,
        || Reply::http("500 Internal Server Error", &error_body("{\"message\":\"m\"}")),
        &["-rpcwait", "-rpcwaittimeout=3"],
        1,
    );
    scripted("-rpcwaittimeout without -rpcwait", 1, || Reply::Close, &["-rpcwaittimeout=5"], 1);
}

/// The bytes of the request: credentials, cookie handling, wallet URL, host header.
fn request_byte_rows(m: &Matrix, t: &Tools, home: &Path, work: &Path) {
    let g = "request";
    let server = FakeServer::always(Reply::json(&rpc_body("1")));
    let port = server.port;
    let base = |more: &[&str]| {
        Invocation::new(with(vec!["-regtest".into(), format!("-rpcport={port}")], more))
    };
    let row = |name: &str, more: &[&str]| fake_row(m, t, home, g, name, &server, &base(more), 0);

    row("user and password", &["-rpcuser=u", "-rpcpassword=p", "getblockcount"]);
    row("password only", &["-rpcpassword=p", "getblockcount"]);
    // A user without a password: the cookie is read, and there is none.
    fake_row(
        m,
        t,
        home,
        g,
        "user only, no cookie",
        &server,
        &base(&[
            "-rpcuser=u",
            &format!("-datadir={}", work.join("no-such-dir").display()),
            "getblockcount",
        ]),
        1,
    );
    row("colon in password", &["-rpcuser=u", "-rpcpassword=a:b:c", "getblockcount"]);
    row(
        "non-ASCII credentials",
        &["-rpcuser=us\u{e9}r", "-rpcpassword=p\u{e4}ss\u{1f600}", "getblockcount"],
    );
    row(
        "long credentials",
        &[
            &format!("-rpcuser={}", "u".repeat(300)),
            &format!("-rpcpassword={}", "p".repeat(300)),
            "getblockcount",
        ],
    );
    row("empty user", &["-rpcuser=", "-rpcpassword=p", "getblockcount"]);
    row("-rpcpassword given twice", &["-rpcpassword=a", "-rpcpassword=b", "getblockcount"]);
    row("-norpcpassword", &["-norpcpassword", "getblockcount"]);

    // Cookie files.
    let cookie_dir = work.join("cookies");
    std::fs::create_dir_all(cookie_dir.join("regtest")).unwrap();
    let write = |name: &str, bytes: &[u8]| {
        let p = cookie_dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.display().to_string()
    };
    let cookie_row =
        |name: &str, file: String| row(name, &[&format!("-rpccookiefile={file}"), "getblockcount"]);
    cookie_row("cookie file", write("c1", b"__cookie__:abcdef\n"));
    cookie_row("cookie file without newline", write("c2", b"__cookie__:abcdef"));
    cookie_row("cookie file with CRLF", write("c3", b"__cookie__:abcdef\r\n"));
    cookie_row("cookie file with two lines", write("c4", b"__cookie__:abcdef\nsecond\n"));
    cookie_row("empty cookie file", write("c5", b""));
    cookie_row("cookie file with only a newline", write("c6", b"\n"));
    cookie_row("cookie file with binary bytes", write("c7", &[0xff, 0x00, 0x41, 0x0a]));
    cookie_row("cookie file that is a directory", cookie_dir.join("regtest").display().to_string());
    cookie_row("missing cookie file", cookie_dir.join("missing").display().to_string());
    std::fs::write(cookie_dir.join("regtest").join(".cookie"), b"__cookie__:indatadir\n").unwrap();
    row(
        "cookie in the network datadir",
        &[&format!("-datadir={}", cookie_dir.display()), "getblockcount"],
    );
    std::fs::write(cookie_dir.join("rel"), b"__cookie__:relative\n").unwrap();
    std::fs::write(cookie_dir.join("regtest").join("rel"), b"__cookie__:relative-net\n").unwrap();
    row(
        "relative -rpccookiefile is net-specific",
        &[&format!("-datadir={}", cookie_dir.display()), "-rpccookiefile=rel", "getblockcount"],
    );
    row(
        "-norpccookiefile",
        &[&format!("-datadir={}", cookie_dir.display()), "-norpccookiefile", "getblockcount"],
    );
    row(
        "cookie ignored when a password is set",
        &[&format!("-datadir={}", cookie_dir.display()), "-rpcpassword=p", "getblockcount"],
    );

    // The wallet endpoint.
    for wallet in [
        "w",
        "",
        " ",
        "a b",
        "a/b",
        "../x",
        "é",
        "a+b",
        "100%",
        "a&b=c#d?e",
        "~-._",
        "\u{1f600}",
        "w\tx",
        "a\"b",
        "a'b",
        "a;b",
        "a:b@c",
        "[]{}",
    ] {
        row(
            &format!("-rpcwallet={wallet:?}"),
            &["-rpcuser=u", "-rpcpassword=p", &format!("-rpcwallet={wallet}"), "getblockcount"],
        );
    }
    let all_ascii: String = (0x20u8..0x7f).map(char::from).filter(|c| *c != '\\').collect();
    row(
        "-rpcwallet with every printable ASCII",
        &["-rpcuser=u", "-rpcpassword=p", &format!("-rpcwallet={all_ascii}"), "getblockcount"],
    );
    row("-norpcwallet", &["-rpcuser=u", "-rpcpassword=p", "-norpcwallet", "getblockcount"]);
    row(
        "-rpcwallet twice",
        &["-rpcuser=u", "-rpcpassword=p", "-rpcwallet=a", "-rpcwallet=b", "getblockcount"],
    );

    // The host header and the address connected to.
    for host in ["127.0.0.1", "localhost", "[127.0.0.1]", "0.0.0.0"] {
        // The server listens on 127.0.0.1 only: `localhost` works where the resolver
        // returns that address first.
        let first_is_v4 = || {
            use std::net::ToSocketAddrs;
            ("localhost", 80)
                .to_socket_addrs()
                .ok()
                .and_then(|mut a| a.next())
                .is_some_and(|a| a.is_ipv4())
        };
        let code = if host == "localhost" && !first_is_v4() {
            1
        } else {
            0
        };
        fake_row(
            m,
            t,
            home,
            g,
            &format!("-rpcconnect={host}"),
            &server,
            &base(&[
                "-rpcuser=u",
                "-rpcpassword=p",
                &format!("-rpcconnect={host}"),
                "getblockcount",
            ]),
            code,
        );
    }
    let with_port = format!("-rpcconnect=127.0.0.1:{port}");
    fake_row(
        m,
        t,
        home,
        g,
        "-rpcconnect with port, no -rpcport",
        &server,
        &Invocation::new([
            "-regtest",
            "-rpcuser=u",
            "-rpcpassword=p",
            with_port.as_str(),
            "getblockcount",
        ]),
        0,
    );

    // Standard input: the arguments the request carries.
    let stdin_row = |name: &str, more: &[&str], stdin: &[u8]| {
        fake_row(m, t, home, g, name, &server, &base(more).stdin(stdin), 0);
    };
    stdin_row(
        "-stdin appends lines",
        &["-rpcuser=u", "-rpcpassword=p", "-stdin", "getblock"],
        b"hash\n2\n",
    );
    stdin_row(
        "-stdin without trailing newline",
        &["-rpcuser=u", "-rpcpassword=p", "-stdin", "getblock"],
        b"hash\n2",
    );
    stdin_row(
        "-stdin with CRLF",
        &["-rpcuser=u", "-rpcpassword=p", "-stdin", "getblock"],
        b"hash\r\n2\r\n",
    );
    // An empty line is an argument that is not JSON.
    fake_row(
        m,
        t,
        home,
        g,
        "-stdin with empty lines",
        &server,
        &base(&["-rpcuser=u", "-rpcpassword=p", "-stdin", "getblock"]).stdin(b"\n\nx\n\n"),
        1,
    );
    stdin_row(
        "-stdin with empty input",
        &["-rpcuser=u", "-rpcpassword=p", "-stdin", "getblock", "a"],
        b"",
    );
    stdin_row(
        "-stdin with non-UTF-8",
        &["-rpcuser=u", "-rpcpassword=p", "-stdin", "getblock"],
        b"a\n",
    );
    stdin_row(
        "-stdin JSON argument",
        &["-rpcuser=u", "-rpcpassword=p", "-stdin", "createrawtransaction"],
        b"[{\"txid\":\"00\",\"vout\":0}]\n{\"x\":1}\n",
    );
    stdin_row("-stdinrpcpass", &["-rpcuser=u", "-stdinrpcpass", "getblockcount"], b"secret\n");
    stdin_row(
        "-stdinrpcpass without newline",
        &["-rpcuser=u", "-stdinrpcpass", "getblockcount"],
        b"secret",
    );
    stdin_row(
        "-stdinrpcpass with CRLF",
        &["-rpcuser=u", "-stdinrpcpass", "getblockcount"],
        b"secret\r\n",
    );
    stdin_row("-stdinrpcpass empty line", &["-rpcuser=u", "-stdinrpcpass", "getblockcount"], b"\n");
    stdin_row(
        "-stdinrpcpass and -stdin",
        &["-rpcuser=u", "-stdinrpcpass", "-stdin", "getblock"],
        b"secret\nhash\n2\n",
    );
    stdin_row(
        "-stdinwalletpassphrase",
        &["-rpcuser=u", "-rpcpassword=p", "-stdinwalletpassphrase", "walletpassphrase", "60"],
        b"pass phrase\n",
    );
    stdin_row(
        "-stdinwalletpassphrase and -stdin",
        &["-rpcuser=u", "-rpcpassword=p", "-stdinwalletpassphrase", "-stdin", "walletpassphrase"],
        b"pw\n60\n",
    );
    stdin_row(
        "-stdinwalletpassphrase on walletpassphrasechange",
        &[
            "-rpcuser=u",
            "-rpcpassword=p",
            "-stdinwalletpassphrase",
            "walletpassphrasechange",
            "new",
        ],
        b"old\n",
    );
    stdin_row(
        "all three stdin options",
        &["-rpcuser=u", "-stdinrpcpass", "-stdinwalletpassphrase", "-stdin", "walletpassphrase"],
        b"rpcpw\nwalletpw\n60\n",
    );
    stdin_row(
        "-nostdin",
        &["-rpcuser=u", "-rpcpassword=p", "-nostdin", "getblockcount"],
        b"ignored\n",
    );
    // Failures reading standard input.
    for (name, more) in [
        ("-stdinrpcpass with empty input", vec!["-rpcuser=u", "-stdinrpcpass", "getblockcount"]),
        (
            "-stdinwalletpassphrase with empty input",
            vec![
                "-rpcuser=u",
                "-rpcpassword=p",
                "-stdinwalletpassphrase",
                "walletpassphrase",
                "60",
            ],
        ),
    ] {
        fake_row(m, t, home, g, name, &server, &base(&more).stdin(b""), 1);
    }
}

/// `-named` against the scripted server: how arguments are keyed and typed.
fn named_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "named";
    let server = FakeServer::always(Reply::json(&rpc_body("null")));
    let base = srv(server.port);
    let row = |name: &str, more: &[&str], code: i32| {
        fake_row(m, t, home, g, name, &server, &Invocation::new(with(base.clone(), more)), code);
    };
    row("name=value", &["-named", "getblock", "blockhash=abc", "verbosity=2"], 0);
    row("positional then named", &["-named", "getblock", "abc", "verbosity=2"], 0);
    row("named then positional", &["-named", "getblock", "verbosity=2", "abc"], 0);
    row("duplicate names: the last wins", &["-named", "getblock", "verbosity=1", "verbosity=2"], 0);
    row("explicit args and positional", &["-named", "getblock", "args=x", "abc"], 0);
    row("unknown names pass through", &["-named", "getblock", "nonsense=1", "x=y"], 0);
    row("value with equals signs", &["-named", "getblock", "blockhash=a=b=c"], 0);
    row("empty name", &["-named", "getblock", "=5"], 0);
    row("empty value", &["-named", "getblock", "blockhash="], 0);
    row(
        "JSON value by name",
        &["-named", "createrawtransaction", "inputs=[]", "outputs={\"a\":1}"],
        0,
    );
    row(
        "quoted JSON by name",
        &["-named", "createrawtransaction", "inputs='[]'", "outputs=\"{}\""],
        0,
    );
    row("positional JSON in -named mode", &["-named", "createrawtransaction", "[]", "{}"], 0);
    row("bad JSON by name", &["-named", "createrawtransaction", "inputs=[1,"], 1);
    row("sub-command by name", &["-named", "protx", "info", "protxhash=abc"], 0);
    row("sub-command with equals first", &["-named", "protx", "protxhash=abc", "info"], 0);
    row("two-word method already given", &["-named", "protx info", "protxhash=abc"], 0);
    row("-named with no arguments", &["-named", "getblockcount"], 0);
    row("-nonamed", &["-nonamed", "getblock", "abc", "2"], 0);
    row("-named=0", &["-named=0", "getblock", "abc", "2"], 0);
    row("-named=1", &["-named=1", "getblock", "blockhash=abc"], 0);
    row("-named with -rpcwallet", &["-named", "-rpcwallet=w", "getwalletinfo"], 0);
    row("sub-command positional", &["protx", "info", "abc", "def"], 0);
    row("sub-command unknown", &["protx", "nonsense", "abc"], 0);
    row(
        "sub-command with arguments typed",
        &["protx", "register_fund", "a", "b", "1", "true", "[1]", "{\"a\":1}"],
        0,
    );
    row("quoted number stays a number", &["getblock", "abc", "'2'"], 0);
    row("double-quoted number", &["getblock", "abc", "\"2\""], 0);
    row("quoted string", &["getblock", "'abc'", "2"], 0);
    row("single quote unbalanced", &["getblock", "'abc", "2"], 0);
    row("sendmany amounts must be JSON", &["sendmany", "", "abc"], 1);
    row("preserve_str parses JSON containers", &["sendmany", "", "{\"a\":1}"], 0);
    row("preserve_str with quoted JSON", &["sendmany", "", "'{\"a\":1}'"], 0);
    row("preserve_str with array", &["sendmany", "", "[1]"], 0);
    row("boolean words", &["getblock", "abc", "true"], 0);
    row("number forms", &["getblock", "abc", "1e2"], 0);
    row("null argument", &["getblock", "abc", "null"], 0);
    row("hex number is not JSON", &["getblock", "abc", "0x10"], 1);
    row("leading plus is not JSON", &["getblock", "abc", "+1"], 1);
    row("leading space number", &["getblock", "abc", " 1"], 0);
    row("trailing space number", &["getblock", "abc", "1 "], 0);
    row("NaN", &["getblock", "abc", "NaN"], 1);
    row(
        "too many positional arguments",
        &["getblockcount", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12"],
        0,
    );
}

/// `-getinfo`, `-netinfo`, `-addrinfo` and `-generate` against scripted replies.
fn handler_fake_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "handlers";
    let network_info = |extra: &str| {
        format!(
            "{{\"version\":240000,\"subversion\":\"/Dash Core:24.0.0/\",\"protocolversion\":70240,\"timeoffset\":-1,\"connections\":3,\"connections_in\":1,\"connections_out\":2,\"connections_mn\":0,\"connections_mn_in\":0,\"connections_mn_out\":0,\"networks\":[{{\"name\":\"ipv4\",\"limited\":false,\"reachable\":true,\"proxy\":\"\",\"proxy_randomize_credentials\":false}},{{\"name\":\"ipv6\",\"limited\":false,\"reachable\":true,\"proxy\":\"127.0.0.1:9050\",\"proxy_randomize_credentials\":true}},{{\"name\":\"onion\",\"limited\":true,\"reachable\":false,\"proxy\":\"\",\"proxy_randomize_credentials\":false}}],\"relayfee\":0.00001000,\"incrementalfee\":0.00001000,\"localaddresses\":[],\"warnings\":\"{extra}\"}}"
        )
    };
    let chain_info = |progress: &str| {
        format!(
            "{{\"chain\":\"regtest\",\"blocks\":120,\"headers\":125,\"bestblockhash\":\"00\",\"difficulty\":4.656542373906925e-10,\"mediantime\":1,\"verificationprogress\":{progress},\"initialblockdownload\":false,\"chainwork\":\"00\",\"size_on_disk\":1,\"pruned\":false}}"
        )
    };
    let wallet_info = "{\"walletname\":\"\",\"walletversion\":1,\"balance\":1.0,\"coinjoin_balance\":2.5,\"keypoolsize\":1000,\"paytxfee\":0.0001,\"unlocked_until\":12345}";
    let balances = "{\"mine\":{\"trusted\":12.5,\"untrusted_pending\":0,\"immature\":0}}";
    // A batch reply, with ids in a shuffled order as a server may answer.
    let batch = |net: &str, chain: &str, wallet: &str, bal: &str| {
        format!(
            "[{{\"result\":{bal},\"error\":null,\"id\":3}},{{\"result\":{net},\"error\":null,\"id\":0}},{{\"result\":{wallet},\"error\":null,\"id\":2}},{{\"result\":{chain},\"error\":null,\"id\":1}}]"
        )
    };
    let wallet_err =
        "{\"result\":null,\"error\":{\"code\":-32601,\"message\":\"Method not found\"},\"id\":2}";
    let bal_err =
        "{\"result\":null,\"error\":{\"code\":-32601,\"message\":\"Method not found\"},\"id\":3}";
    let getinfo_cases: Vec<(&str, String, Vec<&str>, i32)> = vec![
        ("-getinfo full", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo color always", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo", "-color=always"], 0),
        ("-getinfo color never", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo", "-color=never"], 0),
        ("-getinfo color auto", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo", "-color=auto"], 0),
        ("-getinfo color invalid", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo", "-color=blue"], 1),
        ("-getinfo color empty", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo", "-color="], 1),
        ("-getinfo progress 0", batch(&network_info(""), &chain_info("0"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress 1", batch(&network_info(""), &chain_info("1"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress 0.99", batch(&network_info(""), &chain_info("0.99"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress 0.98999", batch(&network_info(""), &chain_info("0.98999"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress 0.123456789", batch(&network_info(""), &chain_info("0.123456789"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress 0.9999999e-5", batch(&network_info(""), &chain_info("0.9999999e-5"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress above 1", batch(&network_info(""), &chain_info("1.5"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress negative", batch(&network_info(""), &chain_info("-0.5"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress not a number", batch(&network_info(""), &chain_info("\"x\""), wallet_info, balances), vec!["-getinfo"], 1),
        ("-getinfo progress 1e999", batch(&network_info(""), &chain_info("1e999"), wallet_info, balances), vec!["-getinfo"], 1),
        ("-getinfo no wallet", batch(&network_info(""), &chain_info("0.5"), wallet_err, bal_err), vec!["-getinfo"], 0),
        ("-getinfo wallet error only for balances", batch(&network_info(""), &chain_info("0.5"), wallet_info, bal_err), vec!["-getinfo"], 0),
        ("-getinfo with warnings", batch(&network_info("Warning: test <b>"), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo -rpcwallet", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo", "-rpcwallet=w"], 0),
        ("-getinfo -named", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo", "-named"], 0),
        ("-nogetinfo", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-nogetinfo"], 0),
        ("-getinfo=0", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo=0"], 0),
        ("-getinfo reply is not a batch", "{\"result\":1,\"error\":null,\"id\":1}".to_string(), vec!["-getinfo"], 1),
        ("-getinfo member not an object", "[1]".to_string(), vec!["-getinfo"], 1),
        ("-getinfo id too large", "[{\"id\":9,\"result\":null,\"error\":null}]".to_string(), vec!["-getinfo"], 1),
        ("-getinfo id missing", "[{\"result\":null,\"error\":null}]".to_string(), vec!["-getinfo"], 1),
        ("-getinfo progress tie 0.0003125", batch(&network_info(""), &chain_info("0.0003125"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress tie 0.0009375", batch(&network_info(""), &chain_info("0.0009375"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress tie 0.0000125", batch(&network_info(""), &chain_info("0.0000125"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo progress 0.99995", batch(&network_info(""), &chain_info("0.99995"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo one proxy for every network", batch(&network_info("").replace("\"proxy\":\"\"", "\"proxy\":\"127.0.0.1:9050\""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo two proxies, one shared", batch(&network_info("").replacen("\"proxy\":\"\"", "\"proxy\":\"9.9.9.9:1\"", 1), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo"], 0),
        ("-getinfo wide balance", batch(&network_info(""), &chain_info("0.5"), wallet_info, "{\"mine\":{\"trusted\":12345678901.12345678,\"untrusted_pending\":0,\"immature\":0}}"), vec!["-getinfo"], 0),
        ("-getinfo negative balance", batch(&network_info(""), &chain_info("0.5"), wallet_info, "{\"mine\":{\"trusted\":-5,\"untrusted_pending\":0,\"immature\":0}}"), vec!["-getinfo"], 0),
        ("-getinfo getinfo and -netinfo together", batch(&network_info(""), &chain_info("0.5"), wallet_info, balances), vec!["-getinfo", "-netinfo"], 0),
        ("-getinfo networkinfo error", "[{\"id\":0,\"result\":null,\"error\":{\"code\":-28,\"message\":\"Loading\"}},{\"id\":1,\"result\":null,\"error\":null},{\"id\":2,\"result\":null,\"error\":null},{\"id\":3,\"result\":null,\"error\":null}]".to_string(), vec!["-getinfo"], 28),
    ];
    for (name, body, args, code) in getinfo_cases {
        // After the batch, -getinfo asks `listwallets` for the balance table; the node has one.
        let server = FakeServer::start(move |_, req| {
            if String::from_utf8_lossy(req).contains("\"listwallets\"") {
                Reply::json(&rpc_body("[\"w\"]"))
            } else {
                Reply::json(&body)
            }
        });
        fake_row(
            m,
            t,
            home,
            g,
            name,
            &server,
            &Invocation::new(with(srv(server.port), &args)),
            code,
        );
    }

    // -getinfo with several wallets: the balances table is built from listwallets/getbalances.
    let wallets = |list: &str| {
        let list = list.to_string();
        FakeServer::start(move |_, req| {
            let text = String::from_utf8_lossy(req);
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
            let reply = if body.contains("\"getnetworkinfo\"") {
                batch(&network_info(""), &chain_info("0.5"), wallet_info, balances)
            } else if body.contains("\"listwallets\"") {
                rpc_body(&list)
            } else if body.contains("\"getbalances\"") {
                let n = text.lines().next().unwrap_or("").len() as u32;
                rpc_body(&format!("{{\"mine\":{{\"trusted\":{}.5}}}}", n % 1000))
            } else {
                rpc_body("null")
            };
            Reply::json(&reply)
        })
    };
    for (name, list) in [
        ("-getinfo two wallets", "[\"a\",\"b\"]"),
        ("-getinfo three wallets incl. the default", "[\"\",\"w1\",\"a very long wallet name\"]"),
        ("-getinfo one wallet", "[\"a\"]"),
        ("-getinfo no wallets", "[]"),
        ("-getinfo listwallets not an array", "\"x\""),
        ("-getinfo wallet name not a string", "[1,2]"),
    ] {
        let server = wallets(list);
        let code = if list.contains("[1,2]") {
            1
        } else {
            0
        };
        fake_row(
            m,
            t,
            home,
            g,
            name,
            &server,
            &Invocation::new(with(srv(server.port), &["-getinfo"])),
            code,
        );
    }
    // Balance tables: wide numbers, and a wallet whose getbalances fails.
    let balance_table = |list: &'static str, replies: Vec<(&'static str, Reply)>| {
        FakeServer::start(move |_, req| {
            let text = String::from_utf8_lossy(req);
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
            if body.contains("\"getnetworkinfo\"") {
                Reply::json(&batch(&network_info(""), &chain_info("0.5"), wallet_info, balances))
            } else if body.contains("\"listwallets\"") {
                Reply::json(&rpc_body(list))
            } else if body.contains("\"getbalances\"") {
                let line = text.lines().next().unwrap_or("");
                match replies.iter().find(|(w, _)| line.contains(&format!("/wallet/{w} "))) {
                    Some((_, reply)) => reply.clone(),
                    None => Reply::json(&rpc_body("{\"mine\":{\"trusted\":1}}")),
                }
            } else {
                Reply::json(&rpc_body("null"))
            }
        })
    };
    let wide = |v: &str| Reply::json(&rpc_body(&format!("{{\"mine\":{{\"trusted\":{v}}}}}")));
    let table_cases: Vec<(&str, FakeServer, i32)> = vec![
        (
            "-getinfo balance table with a wide number",
            balance_table(
                "[\"a\",\"b\"]",
                vec![("a", wide("12345678901.12345678")), ("b", wide("0.00000001"))],
            ),
            0,
        ),
        (
            "-getinfo balance table, negative and huge",
            balance_table(
                "[\"a\",\"b\",\"c\"]",
                vec![("a", wide("-5")), ("b", wide("1e15")), ("c", wide("0"))],
            ),
            0,
        ),
        (
            "-getinfo balance table, getbalances fails for one wallet",
            balance_table(
                "[\"a\",\"b\"]",
                vec![(
                    "b",
                    Reply::http(
                        "500 Internal Server Error",
                        &error_body("{\"code\":-32601,\"message\":\"nope\"}"),
                    ),
                )],
            ),
            0,
        ),
        (
            "-getinfo balance table, getbalances reply without mine",
            balance_table("[\"a\",\"b\"]", vec![("a", Reply::json(&rpc_body("{}")))]),
            0,
        ),
    ];
    for (name, server, code) in table_cases {
        fake_row(
            m,
            t,
            home,
            g,
            name,
            &server,
            &Invocation::new(with(srv(server.port), &["-getinfo"])),
            code,
        );
    }
    // On a terminal, -color=auto (the default) colours the output.
    for (name, extra) in [
        ("-getinfo on a terminal", vec!["-getinfo"]),
        ("-getinfo -color=auto on a terminal", vec!["-getinfo", "-color=auto"]),
        ("-getinfo -color=always on a terminal", vec!["-getinfo", "-color=always"]),
        ("-getinfo -color=never on a terminal", vec!["-getinfo", "-color=never"]),
    ] {
        let server = balance_table("[\"a\",\"b\"]", vec![]);
        let i = Invocation::new(with(srv(server.port), &extra)).tty(Tty::Stdout);
        fake_row(m, t, home, g, name, &server, &i, 0);
    }

    // -getinfo with -named sends `"params":{}` for listwallets.
    let server = wallets("[\"a\",\"b\"]");
    fake_row(
        m,
        t,
        home,
        g,
        "-getinfo -named multiwallet",
        &server,
        &Invocation::new(with(srv(server.port), &["-getinfo", "-named"])),
        0,
    );

    // -addrinfo
    let node = |network: &str| {
        format!(
            "{{\"time\":1,\"services\":1,\"address\":\"1.2.3.4\",\"port\":9999,\"network\":\"{network}\"}}"
        )
    };
    let addr_cases: Vec<(&str, String, i32)> = vec![
        ("-addrinfo empty", rpc_body("[]"), 0),
        (
            "-addrinfo mixed",
            rpc_body(&format!(
                "[{}]",
                [
                    "ipv4",
                    "ipv4",
                    "ipv6",
                    "onion",
                    "i2p",
                    "cjdns",
                    "internal",
                    "not_publicly_routable",
                    "bogus"
                ]
                .map(node)
                .join(",")
            )),
            0,
        ),
        ("-addrinfo old server", rpc_body("[{\"time\":1,\"address\":\"1.2.3.4\"}]"), 1),
        ("-addrinfo error", error_body("{\"code\":-1,\"message\":\"m\"}"), 1),
        ("-addrinfo result not an array", rpc_body("5"), 1),
        ("-addrinfo network not a string", rpc_body("[{\"network\":5}]"), 1),
        ("-addrinfo null result", rpc_body("null"), 1),
        ("-addrinfo result object", rpc_body(&format!("{{\"a\":{}}}", node("ipv4"))), 0),
    ];
    for (name, body, code) in addr_cases {
        let reply = if body.contains("\"error\":{") {
            Reply::http("500 Internal Server Error", &body)
        } else {
            Reply::json(&body)
        };
        let server = FakeServer::always(reply);
        fake_row(
            m,
            t,
            home,
            g,
            name,
            &server,
            &Invocation::new(with(srv(server.port), &["-addrinfo"])),
            code,
        );
    }

    // -netinfo from canned getpeerinfo/getnetworkinfo.
    netinfo_fake_rows(m, t, home);

    // -generate
    let gen_server = |newaddr: Reply, generated: Reply| {
        FakeServer::start(move |_, req| {
            let text = String::from_utf8_lossy(req);
            if text.contains("\"getnewaddress\"") {
                newaddr.clone()
            } else {
                generated.clone()
            }
        })
    };
    let addr = rpc_body("\"yTestAddress111111111111111111111\"");
    let blocks = rpc_body("[\"aa\",\"bb\"]");
    let generate_cases: Vec<(&str, Vec<&str>, Reply, Reply, i32)> = vec![
        ("-generate", vec!["-generate"], Reply::json(&addr), Reply::json(&blocks), 0),
        ("-generate 3", vec!["-generate", "3"], Reply::json(&addr), Reply::json(&blocks), 0),
        (
            "-generate 2 1000",
            vec!["-generate", "2", "1000"],
            Reply::json(&addr),
            Reply::json(&blocks),
            0,
        ),
        ("-generate 0", vec!["-generate", "0"], Reply::json(&addr), Reply::json(&blocks), 1),
        ("-generate 00", vec!["-generate", "00"], Reply::json(&addr), Reply::json(&blocks), 1),
        (
            "-generate 1 2 3",
            vec!["-generate", "1", "2", "3"],
            Reply::json(&addr),
            Reply::json(&blocks),
            1,
        ),
        ("-generate abc", vec!["-generate", "abc"], Reply::json(&addr), Reply::json(&blocks), 1),
        (
            "-generate -1 is an option",
            vec!["-generate", "-1"],
            Reply::json(&addr),
            Reply::json(&blocks),
            1,
        ),
        ("-generate 1.5", vec!["-generate", "1.5"], Reply::json(&addr), Reply::json(&blocks), 0),
        (
            "-generate with -rpcwallet",
            vec!["-generate", "-rpcwallet=w1"],
            Reply::json(&addr),
            Reply::json(&blocks),
            0,
        ),
        (
            "-generate named",
            vec!["-generate", "-named", "2"],
            Reply::json(&addr),
            Reply::json(&blocks),
            0,
        ),
        (
            "-generate wallet not specified",
            vec!["-generate"],
            Reply::http(
                "500 Internal Server Error",
                &error_body("{\"code\":-19,\"message\":\"Wallet file not specified\"}"),
            ),
            Reply::json(&blocks),
            19,
        ),
        (
            "-generate no wallet",
            vec!["-generate"],
            Reply::http(
                "404 Not Found",
                &error_body("{\"code\":-32601,\"message\":\"Method not found\"}"),
            ),
            Reply::json(&blocks),
            89,
        ),
        (
            "-generate address not a string",
            vec!["-generate"],
            Reply::json(&rpc_body("5")),
            Reply::json(&blocks),
            1,
        ),
        (
            "-generate address null",
            vec!["-generate"],
            Reply::json(&rpc_body("null")),
            Reply::json(&blocks),
            1,
        ),
        (
            "-generate server error on generatetoaddress",
            vec!["-generate"],
            Reply::json(&addr),
            Reply::http(
                "500 Internal Server Error",
                &error_body("{\"code\":-8,\"message\":\"bad maxtries\"}"),
            ),
            0,
        ),
        (
            "-generate reply not an object",
            vec!["-generate"],
            Reply::json(&addr),
            Reply::json("[1]"),
            1,
        ),
        (
            "-generate empty reply object",
            vec!["-generate"],
            Reply::json(&addr),
            Reply::json("{}"),
            0,
        ),
        (
            "-nogenerate",
            vec!["-nogenerate", "getblockcount"],
            Reply::json(&addr),
            Reply::json(&blocks),
            0,
        ),
    ];
    for (name, args, newaddr, generated, code) in generate_cases {
        let server = gen_server(newaddr, generated);
        fake_row(
            m,
            t,
            home,
            g,
            name,
            &server,
            &Invocation::new(with(srv(server.port), &args)),
            code,
        );
    }
}

/// One canned peer, with times relative to `now` (the request time).
fn netinfo_peer(
    now: u64,
    id: u32,
    inbound: bool,
    network: &str,
    conn: &str,
    extra: &str,
) -> String {
    format!(
        "{{\"id\":{id},\"addr\":\"10.0.0.{id}:19999\",\"network\":\"{network}\",\"services\":\"0000000000000409\",\"relaytxes\":true,\"lastsend\":{s},\"lastrecv\":{r},\"last_transaction\":{tx},\"last_block\":{blk},\"conntime\":{c},\"timeoffset\":0,\"pingtime\":0.0123,\"minping\":0.0011,\"version\":70240,\"subver\":\"/Dash Core:24.0.0/\",\"inbound\":{inbound},\"bip152_hb_to\":{hb_to},\"bip152_hb_from\":{hb_from},\"connection_type\":\"{conn}\",\"transport_protocol_type\":\"v2\",\"addr_relay_enabled\":true,\"addr_processed\":{ap},\"addr_rate_limited\":{al}{extra}}}",
        s = now - 5,
        r = now - 12,
        tx = now - 600,
        blk = now - 3600,
        c = now - 7200,
        hb_to = id.is_multiple_of(2),
        hb_from = id.is_multiple_of(3),
        ap = id * 100,
        al = id,
    )
}

fn netinfo_network_info(locals: &str) -> String {
    format!(
        "{{\"version\":240000,\"subversion\":\"/Dash Core:24.0.0/\",\"protocolversion\":70240,\"networks\":[{{\"name\":\"ipv4\",\"reachable\":true}},{{\"name\":\"ipv6\",\"reachable\":true}},{{\"name\":\"onion\",\"reachable\":false}},{{\"name\":\"i2p\",\"reachable\":true}},{{\"name\":\"cjdns\",\"reachable\":true}}],\"localaddresses\":[{locals}]}}"
    )
}

fn netinfo_batch(peers: &str, net: &str) -> String {
    format!(
        "[{{\"result\":[{peers}],\"error\":null,\"id\":0}},{{\"result\":{net},\"error\":null,\"id\":1}}]"
    )
}

type Body = Box<dyn Fn(u64) -> String + Send + Sync>;

/// A server whose reply is built when the request arrives, so the ages the dashboard prints do
/// not depend on how long the two programs are apart.
fn netinfo_server(body: Body) -> FakeServer {
    FakeServer::start(move |_, _| {
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        Reply::json(&body(now))
    })
}

/// Canned peer lists for the `-netinfo` dashboard: every column and every branch.
fn netinfo_fake_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "netinfo";
    let locals = "{\"address\":\"1.2.3.4\",\"port\":9999,\"score\":1},{\"address\":\"2001:db8::1234:5678:9abc:def0\",\"port\":19999,\"score\":12345}";
    let mixed = |now: u64| {
        [
            netinfo_peer(now, 1, false, "ipv4", "outbound-full-relay", ""),
            netinfo_peer(now, 2, false, "ipv6", "block-relay-only", ""),
            netinfo_peer(now, 3, true, "ipv4", "inbound", ""),
            netinfo_peer(now, 4, false, "onion", "manual", ""),
            netinfo_peer(now, 5, true, "i2p", "inbound", ",\"mapped_as\":64496"),
            netinfo_peer(now, 6, false, "not_publicly_routable", "feeler", ""),
            netinfo_peer(now, 7, false, "cjdns", "addr-fetch", ""),
            netinfo_peer(now, 8, true, "internal", "inbound", ""),
            netinfo_peer(now, 10, false, "ipv4", "outbound-full-relay", ""),
            netinfo_peer(now, 100000, true, "bogus-network", "inbound", ""),
        ]
        .join(",")
    };
    let bare = "{\"id\":1,\"addr\":\"1.2.3.4:5\",\"network\":\"ipv4\",\"lastsend\":0,\"lastrecv\":0,\"last_transaction\":0,\"last_block\":0,\"conntime\":0,\"version\":0,\"subver\":\"\",\"inbound\":true,\"bip152_hb_to\":false,\"bip152_hb_from\":false,\"connection_type\":\"inbound\",\"relaytxes\":false}";
    let cases: Vec<(&str, Body, i32)> = vec![
        (
            "mixed peers",
            Box::new(move |n| netinfo_batch(&mixed(n), &netinfo_network_info(locals))),
            0,
        ),
        ("no peers", Box::new(move |_| netinfo_batch("", &netinfo_network_info(""))), 0),
        (
            "peer without optional fields",
            Box::new(move |_| netinfo_batch(bare, &netinfo_network_info(locals))),
            0,
        ),
        (
            "many peers",
            Box::new(move |n| {
                let peers = (1..40)
                    .map(|i| netinfo_peer(n, i, i % 2 == 0, "ipv4", "outbound-full-relay", ""))
                    .collect::<Vec<_>>()
                    .join(",");
                netinfo_batch(&peers, &netinfo_network_info(locals))
            }),
            0,
        ),
        (
            "old server",
            Box::new(move |_| {
                netinfo_batch("", &netinfo_network_info("").replace("240000", "190000"))
            }),
            1,
        ),
        (
            "peer error",
            Box::new(|_| {
                "[{\"result\":null,\"error\":{\"code\":-1,\"message\":\"m\"},\"id\":0},{\"result\":{},\"error\":null,\"id\":1}]".to_string()
            }),
            1,
        ),
        (
            "networkinfo error",
            Box::new(|_| {
                "[{\"result\":[],\"error\":null,\"id\":0},{\"result\":null,\"error\":{\"code\":-2,\"message\":\"n\"},\"id\":1}]".to_string()
            }),
            2,
        ),
        (
            "peer missing a field",
            Box::new(move |_| netinfo_batch("{\"network\":\"ipv4\"}", &netinfo_network_info(""))),
            1,
        ),
        (
            "peers is not an array",
            Box::new(move |_| {
                netinfo_batch("", &netinfo_network_info(""))
                    .replace("\"result\":[]", "\"result\":5")
            }),
            1,
        ),
        (
            // Every column wider than its default: 7-digit id, 8-digit addrp, 6-digit addrl,
            // 8-digit age and asmap.
            "wide columns",
            Box::new(move |n| {
                let wide = netinfo_peer(
                    n,
                    3,
                    false,
                    "ipv4",
                    "outbound-full-relay",
                    ",\"mapped_as\":12345678",
                )
                .replace("\"id\":3,", "\"id\":1234567,")
                .replace("\"addr_processed\":300", "\"addr_processed\":12345678")
                .replace("\"addr_rate_limited\":3,", "\"addr_rate_limited\":123456,")
                .replace(&format!("\"conntime\":{}", n - 7200), "\"conntime\":1")
                .replace(
                    "\"subver\":\"/Dash Core:24.0.0/\"",
                    "\"subver\":\"/a much longer user agent string:1.2.3/\"",
                );
                let other = netinfo_peer(n, 4, true, "ipv6", "inbound", "");
                netinfo_batch(&format!("{wide},{other}"), &netinfo_network_info(locals))
            }),
            0,
        ),
        (
            // Ages in the future, zero times, other transports.
            "negative ages and odd transports",
            Box::new(move |n| {
                let a = netinfo_peer(n, 1, false, "ipv4", "outbound-full-relay", "")
                    .replace(
                        &format!("\"conntime\":{}", n - 7200),
                        &format!("\"conntime\":{}", n + 7200),
                    )
                    .replace(
                        &format!("\"lastsend\":{}", n - 5),
                        &format!("\"lastsend\":{}", n + 50),
                    )
                    .replace(
                        "\"transport_protocol_type\":\"v2\"",
                        "\"transport_protocol_type\":\"detecting\"",
                    );
                let b = netinfo_peer(n, 2, false, "ipv4", "block-relay-only", "")
                    .replace(
                        "\"transport_protocol_type\":\"v2\"",
                        "\"transport_protocol_type\":\"v10\"",
                    )
                    .replace("\"addr_processed\":200", "\"addr_processed\":0")
                    .replace("\"addr_relay_enabled\":true", "\"addr_relay_enabled\":false");
                let c = netinfo_peer(n, 3, true, "ipv4", "inbound", "")
                    .replace("\"pingtime\":0.0123", "\"pingtime\":-0.0")
                    .replace("\"minping\":0.0011", "\"minping\":-0.0");
                netinfo_batch(&format!("{a},{b},{c}"), &netinfo_network_info(locals))
            }),
            0,
        ),
        (
            "unknown and unroutable network names",
            Box::new(move |n| {
                let net = netinfo_network_info(locals)
                    .replace("\"name\":\"cjdns\"", "\"name\":\"weird\"")
                    .replace("\"name\":\"i2p\"", "\"name\":\"not_publicly_routable\"");
                netinfo_batch(&mixed(n), &net)
            }),
            0,
        ),
    ];
    for (name, body, code) in cases {
        let body: std::sync::Arc<Body> = std::sync::Arc::new(body);
        for level in ["0", "1", "2", "3", "4", "5", "255"] {
            let shared = body.clone();
            let server = netinfo_server(Box::new(move |n| shared(n)));
            let i = Invocation::new(with(srv(server.port), &["-netinfo", level]));
            let marker = if code == 0 {
                "client"
            } else {
                ""
            };
            fake_row_volatile(
                m,
                t,
                home,
                g,
                &format!("{name}, level {level}"),
                &server,
                &i,
                code,
                marker,
            );
        }
    }
    // The peer order: random peer lists with many ties on (direction, min ping), where the
    // unstable `std::sort` decides. Sizes straddle the introsort thresholds.
    let mut state = 0x2545_f491_u32;
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state >> 8
    };
    for round in 0..80 {
        let n = [1, 2, 5, 16, 17, 18, 33, 70][round % 8] + (next() % 3) * (round as u32 / 8);
        let specs: Vec<(bool, u32)> = (0..n).map(|_| (next() % 2 == 0, next() % 4)).collect();
        let server = netinfo_server(Box::new(move |now| {
            let peers = specs
                .iter()
                .enumerate()
                .map(|(i, &(inbound, ping))| {
                    let min_ping = format!("{}", f64::from(ping) / 1000.0);
                    netinfo_peer(
                        now,
                        i as u32 + 1,
                        inbound,
                        "ipv4",
                        if inbound {
                            "inbound"
                        } else {
                            "outbound-full-relay"
                        },
                        "",
                    )
                    .replace("\"minping\":0.0011", &format!("\"minping\":{min_ping}"))
                })
                .collect::<Vec<_>>()
                .join(",");
            netinfo_batch(&peers, &netinfo_network_info(""))
        }));
        let i = Invocation::new(with(srv(server.port), &["-netinfo", "1"]));
        fake_row_volatile(
            m,
            t,
            home,
            "netinfo-order",
            &format!("round {round}, {n} peers"),
            &server,
            &i,
            0,
            "client",
        );
    }
    // Chains named in the header.
    for chain in ["-regtest", "-testnet", "-devnet=abc", "-chain=main"] {
        let server = netinfo_server(Box::new(move |n| {
            netinfo_batch(&mixed_peers(n), &netinfo_network_info(locals))
        }));
        let mut args = vec![
            chain.to_string(),
            format!("-rpcport={}", server.port),
            "-rpcuser=u".into(),
            "-rpcpassword=p".into(),
            "-netinfo".into(),
            "4".into(),
        ];
        if chain == "-chain=main" {
            args.retain(|a| a != chain);
        }
        fake_row_volatile(
            m,
            t,
            home,
            g,
            &format!("header for {chain}"),
            &server,
            &Invocation::new(args),
            0,
            "client",
        );
    }
    // On a terminal, -color=auto (the default) colours the dashboard.
    for color in [None, Some("-color=auto"), Some("-color=always"), Some("-color=never")] {
        let server = netinfo_server(Box::new(move |n| {
            netinfo_batch(&mixed_peers(n), &netinfo_network_info(locals))
        }));
        let mut args = with(srv(server.port), &["-netinfo", "4"]);
        args.extend(color.map(String::from));
        // The options come first; -netinfo takes the level after them.
        args.sort_by_key(|a| a == "4" || a == "-netinfo");
        let i = Invocation::new(args).tty(Tty::Stdout);
        fake_row_volatile(
            m,
            t,
            home,
            g,
            &format!("terminal, {}", color.unwrap_or("default color")),
            &server,
            &i,
            0,
            "client",
        );
    }
    // An enormous ping, and extra arguments.
    let server = netinfo_server(Box::new(move |n| {
        let slow = netinfo_peer(n, 9, false, "ipv4", "outbound-full-relay", "")
            .replace("\"pingtime\":0.0123", "\"pingtime\":12345.5")
            .replace("\"minping\":0.0011", "\"minping\":1000.0");
        netinfo_batch(&slow, &netinfo_network_info(locals))
    }));
    let i = Invocation::new(with(srv(server.port), &["-netinfo", "4", "ignored", "also ignored"]));
    fake_row_volatile(m, t, home, g, "huge ping, extra arguments", &server, &i, 0, "client");
}

fn mixed_peers(now: u64) -> String {
    [
        ("ipv4", false, "outbound-full-relay"),
        ("ipv6", false, "block-relay-only"),
        ("ipv4", true, "inbound"),
        ("onion", false, "manual"),
    ]
    .iter()
    .enumerate()
    .map(|(i, (net, inbound, conn))| netinfo_peer(now, i as u32 + 1, *inbound, net, conn, ""))
    .collect::<Vec<_>>()
    .join(",")
}

/// Like [`fake_row`], but a pass needs one of five attempts to match (output built from the
/// clock). The stock binary must exit with `expect_code` and print `expect_stdout`, so that two
/// programs failing alike on a broken canned reply cannot pass.
#[allow(clippy::too_many_arguments)] // the row is its description
fn fake_row_volatile(
    m: &Matrix,
    t: &Tools,
    home: &Path,
    group: &'static str,
    name: &str,
    server: &FakeServer,
    inv: &Invocation,
    expect_code: i32,
    expect_stdout: &str,
) {
    eprintln!("row [{group}] {name}");
    let mut last = String::new();
    for _ in 1..=5 {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        if now.subsec_millis() > 40 {
            std::thread::sleep(Duration::from_millis(1000 - u64::from(now.subsec_millis()) + 20));
        }
        // The canned replies are built from the time of the request; the programs read the
        // clock themselves, so both see the same offsets within a second boundary.
        server.clear();
        let ours = run(&t.rdash, home, inv);
        let stock = run(&t.stock, home, inv);
        let requests_equal = {
            let r = server.requests();
            r.len() == 2 && r[0] == r[1]
        };
        let premise = stock.code == Some(expect_code)
            && String::from_utf8_lossy(&stock.stdout).contains(expect_stdout);
        if !premise {
            last = format!(
                "premise failed: stock exit code {:?}, expected {expect_code} and {expect_stdout:?} in stdout\n{}",
                stock.code,
                support::describe("stock", &stock)
            );
        } else if ours == stock && requests_equal {
            m.record_named(group, name.to_string(), true, String::new());
            return;
        } else {
            last = format!(
                "outputs differ\n  rdash: {ours:?}\n  stock: {stock:?}\n  requests equal: {requests_equal}\n"
            );
        }
    }
    m.record_named(group, name.to_string(), false, format!("after 5 attempts: {last}"));
}

/// Every command in Core's RPC table, typed through `vRPCConvertParams`: the request each
/// program sends for the same command line must be byte-identical, positional and `-named`.
fn conversion_sweep(m: &Matrix, t: &Tools, home: &Path) {
    let g = "convert";
    // Values that exercise the converter: JSON scalars, containers, quoting and non-JSON text.
    const PALETTE: [&str; 16] = [
        "1",
        "true",
        "abc",
        "[1,2]",
        "{\"a\":1}",
        "'x y'",
        "\"dq\"",
        "[\"x\"]",
        "1.5",
        "",
        "null",
        "-1",
        "'[1]'",
        "\"{\\\"k\\\":2}\"",
        "00ff",
        "1e3",
    ];
    // Argument counts to try: none, the declared count, and past it.
    let jobs: Vec<(usize, &'static dash_rpc_spec::RpcCommand)> =
        COMMANDS.iter().enumerate().collect();
    let results: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = 4;
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                let server = FakeServer::always(Reply::json(&rpc_body("null")));
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(idx, cmd)) = jobs.get(i) else { break };
                    let words: Vec<&str> = cmd.name.split(' ').collect();
                    let declared = cmd.args.len().min(10);
                    let mut problems = String::new();
                    let mut variants: Vec<(String, Vec<String>)> = Vec::new();
                    let value = |k: usize, v: usize| PALETTE[(k + idx * 3 + v * 5) % PALETTE.len()].to_string();
                    for (label, count) in [("no arguments", 0usize), ("declared arguments", declared), ("extra arguments", declared + 3)] {
                        let mut a: Vec<String> = srv(server.port);
                        a.extend(words.iter().map(|w| w.to_string()));
                        a.extend((0..count).map(|k| value(k, count)));
                        variants.push((format!("positional, {label}"), a));
                    }
                    // Each argument alone as a JSON-looking and as a string value.
                    for v in 0..2 {
                        let mut a: Vec<String> = srv(server.port);
                        a.extend(words.iter().map(|w| w.to_string()));
                        a.extend((0..declared).map(|k| value(k, v + 7)));
                        variants.push((format!("positional, palette shift {v}"), a));
                    }
                    // -named: the declared names, a positional prefix, an undeclared name.
                    for (label, positional_prefix, with_unknown) in [("by name", 0usize, false), ("one positional then names", 1, false), ("names and an unknown", 0, true)] {
                        let mut a: Vec<String> = srv(server.port);
                        a.push("-named".into());
                        a.extend(words.iter().map(|w| w.to_string()));
                        for (k, arg) in cmd.args.iter().enumerate().take(declared) {
                            if k < positional_prefix {
                                a.push(value(k, 3));
                            } else {
                                a.push(format!("{}={}", arg.first_name(), value(k, 3)));
                            }
                        }
                        if with_unknown {
                            a.push("zz_unknown=[1]".into());
                        }
                        variants.push((format!("named, {label}"), a));
                    }
                    for (label, args) in variants {
                        let inv = Invocation::new(args.clone());
                        server.clear();
                        let ours = run(&t.rdash, home, &inv);
                        let ours_req = server.requests();
                        server.clear();
                        let stock = run(&t.stock, home, &inv);
                        let stock_req = server.requests();
                        if ours != stock || ours_req != stock_req {
                            problems.push_str(&format!(
                                "variant {label}: {:?}\n  rdash: {ours:?}\n  request {:?}\n  stock: {stock:?}\n  request {:?}\n",
                                &args[4..],
                                ours_req.iter().map(|r| String::from_utf8_lossy(r).into_owned()).collect::<Vec<_>>(),
                                stock_req.iter().map(|r| String::from_utf8_lossy(r).into_owned()).collect::<Vec<_>>()
                            ));
                        }
                    }
                    results.lock().unwrap().push((cmd.name.to_string(), problems));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort();
    for (name, problems) in results {
        m.record_named(g, format!("convert {name}"), problems.is_empty(), problems);
    }
}

/// Terminal input and output, bytes that are not UTF-8, handler precedence and a closed pipe.
fn stream_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "streams";
    let server = FakeServer::always(Reply::json(&rpc_body("1")));
    let port = server.port;
    // No password: it comes from standard input.
    let base = |more: &[&str]| {
        Invocation::new(with(
            vec!["-regtest".into(), format!("-rpcport={port}"), "-rpcuser=u".into()],
            more,
        ))
    };
    let row =
        |name: &str, i: Invocation, code: i32| fake_row(m, t, home, g, name, &server, &i, code);

    // A password and a passphrase read from a terminal: the prompt appears only when nothing
    // is waiting, echo is off while typing, and a newline follows on standard output.
    for (label, tty) in
        [("prompted", Tty::StdinPrompted), ("input already waiting", Tty::StdinReady)]
    {
        row(
            &format!("-stdinrpcpass on a terminal, {label}"),
            base(&["-stdinrpcpass", "getblockcount"]).stdin(b"secret\n").tty(tty),
            0,
        );
        row(
            &format!("-stdinwalletpassphrase on a terminal, {label}"),
            base(&["-rpcpassword=p", "-stdinwalletpassphrase", "walletpassphrase", "60"])
                .stdin(b"hunter2\n")
                .tty(tty),
            0,
        );
        row(
            &format!("-stdinrpcpass and -stdinwalletpassphrase on a terminal, {label}"),
            base(&["-stdinrpcpass", "-stdinwalletpassphrase", "walletpassphrase", "60"])
                .stdin(b"secret\nhunter2\n")
                .tty(tty),
            0,
        );
    }
    row(
        "-stdinrpcpass on a terminal, end of input at once",
        base(&["-stdinrpcpass", "getblockcount"]).stdin(b"\x04").tty(Tty::StdinReady),
        1,
    );
    row(
        "-stdinwalletpassphrase on a terminal, end of input at once",
        base(&["-rpcpassword=p", "-stdinwalletpassphrase", "walletpassphrase", "60"])
            .stdin(b"\x04")
            .tty(Tty::StdinReady),
        1,
    );
    row(
        "-stdin on a terminal",
        base(&["-rpcpassword=p", "-stdin", "getblock"]).stdin(b"abc\n1\n\x04").tty(Tty::StdinReady),
        0,
    );
    row(
        "-stdin on a terminal, end of input at once",
        base(&["-rpcpassword=p", "-stdin", "getblockcount"]).stdin(b"\x04").tty(Tty::StdinReady),
        0,
    );
    row(
        "-stdin and -stdinrpcpass on a terminal",
        base(&["-stdin", "-stdinrpcpass", "getblock"])
            .stdin(b"secret\nabc\n\x04")
            .tty(Tty::StdinReady),
        0,
    );

    // Passwords and parameters are bytes: a Latin-1 password reaches the server unchanged.
    row(
        "-stdinrpcpass with a password that is not UTF-8",
        base(&["-stdinrpcpass", "getblockcount"]).stdin(b"p\xe4ss\n"),
        0,
    );
    row(
        "-stdinrpcpass with CR LF and NUL bytes",
        base(&["-stdinrpcpass", "getblockcount"]).stdin(b"a\r\n"),
        0,
    );
    row(
        "-stdinrpcpass with an empty line",
        base(&["-stdinrpcpass", "getblockcount"]).stdin(b"\n"),
        0,
    );
    row(
        "-stdinrpcpass with no newline",
        base(&["-stdinrpcpass", "getblockcount"]).stdin(b"nonl"),
        0,
    );

    // Handlers: -getinfo, -netinfo, -generate and -addrinfo are tried in that order.
    let order_server = FakeServer::always(Reply::json(&rpc_body("1")));
    for (name, args) in [
        ("-getinfo and -addrinfo", vec!["-getinfo", "-addrinfo"]),
        ("-addrinfo and -netinfo", vec!["-addrinfo", "-netinfo"]),
        ("-generate and -addrinfo", vec!["-generate", "-addrinfo"]),
        ("-generate and -netinfo", vec!["-generate", "-netinfo"]),
        ("-netinfo and -getinfo", vec!["-netinfo", "-getinfo"]),
        ("-generate and -getinfo", vec!["-generate", "-getinfo"]),
        ("-getinfo and a command", vec!["-getinfo", "getblockcount"]),
        ("-generate and a command", vec!["-generate", "getblockcount"]),
    ] {
        fake_row(
            m,
            t,
            home,
            g,
            &format!("{name}, generic reply"),
            &order_server,
            &Invocation::new(with(srv(order_server.port), &args)),
            1,
        );
    }

    sigpipe_rows(m, t, home);
}

/// DEC-201: with no `-datadir`, rdash-cli reads `~/.rdashcore` where the stock dash-cli reads
/// `~/.dashcore`. The same cookie in each place must give the same request.
fn default_datadir_rows(m: &Matrix, t: &Tools) {
    let g = "deviations";
    let server = FakeServer::always(Reply::json(&rpc_body("1")));
    let args = Invocation::new(["-regtest", &format!("-rpcport={}", server.port), "getblockcount"]);
    let home_with = |dir: &str| {
        let home = home_dir();
        let net = home.path().join(dir).join("regtest");
        std::fs::create_dir_all(&net).unwrap();
        std::fs::write(net.join(".cookie"), b"__cookie__:abc\n").unwrap();
        home
    };
    let (ours_home, stock_home) = (home_with(".rdashcore"), home_with(".dashcore"));
    server.clear();
    let ours = run(&t.rdash, ours_home.path(), &args);
    let ours_requests = server.requests();
    server.clear();
    let stock = run(&t.stock, stock_home.path(), &args);
    let stock_requests = server.requests();
    let ok = ours == stock && ours.code == Some(0) && ours_requests == stock_requests;
    m.record_named(
        g,
        "default datadir: ~/.rdashcore read as stock reads ~/.dashcore".to_string(),
        ok,
        format!("{}{}", support::describe("rdash", &ours), support::describe("stock", &stock)),
    );
    // The Core location is not read: the cookie in `~/.dashcore` never reaches the server.
    let wrong = home_with(".dashcore");
    std::fs::remove_dir_all(wrong.path().join(".rdashcore")).unwrap();
    std::fs::create_dir(wrong.path().join(".rdashcore")).unwrap();
    server.clear();
    let ours = support::run_raw(&t.rdash, wrong.path(), &args);
    let requests = server.requests();
    let ok = ours.code == Some(0)
        && requests.len() == 1
        && !String::from_utf8_lossy(&requests[0]).contains("X19jb29raWVfXzphYmM");
    m.record_named(
        g,
        "default datadir: ~/.dashcore is not read".to_string(),
        ok,
        support::describe("rdash", &ours),
    );
}

/// A closed standard output: Core's dash-cli dies of SIGPIPE.
fn sigpipe_rows(m: &Matrix, t: &Tools, home: &Path) {
    use std::os::unix::process::ExitStatusExt;
    let g = "streams";
    let outcome = |bin: &Path, args: &[String]| {
        let body = rpc_body("\"hi\"");
        let server = FakeServer::always(Reply::Pieces(
            Duration::from_millis(400),
            vec![(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                    .into_bytes(),
                Duration::from_millis(50),
            )],
        ));
        let mut args = args.to_vec();
        args.insert(0, format!("-rpcport={}", server.port));
        let mut child = std::process::Command::new(bin)
            .args(&args)
            .env_clear()
            .env("HOME", home)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // The reader goes away before the program has anything to say.
        drop(child.stdout.take());
        let status = support::wait_row(&mut child, "[streams] closed stdout, a command");
        (status.code(), status.signal())
    };
    let args: Vec<String> =
        ["-regtest", "-rpcuser=u", "-rpcpassword=p", "getblockcount"].map(String::from).to_vec();
    let (ours, stock) = (outcome(&t.rdash, &args), outcome(&t.stock, &args));
    m.record_named(
        g,
        "closed stdout, a command".to_string(),
        ours == stock && stock.1 == Some(13),
        format!("rdash {ours:?}, stock {stock:?} (code, signal)"),
    );
}

/// Parameters read from standard input are JSON strings; stock writes bytes that are not UTF-8
/// into the request as they are, so the request is not valid JSON. Here that is an error.
/// Not comparable with stock: a documented deviation, checked for its own behaviour.
fn non_utf8_input_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "deviations";
    let server = FakeServer::always(Reply::json(&rpc_body("1")));
    for (name, args, input) in [
        (
            "-stdin with a line that is not UTF-8",
            vec!["-stdin", "getblock"],
            b"a\xffb\n".as_slice(),
        ),
        (
            "-stdinwalletpassphrase with a passphrase that is not UTF-8",
            vec!["-stdinwalletpassphrase", "walletpassphrase", "60"],
            b"p\xe4ss\n".as_slice(),
        ),
    ] {
        server.clear();
        let i = Invocation::new(with(srv(server.port), &args)).stdin(input);
        let ours = run(&t.rdash, home, &i);
        let ok = ours.code == Some(1)
            && ours.stdout.is_empty()
            && ours.stderr == b"error: standard input is not valid UTF-8\n"
            && server.requests().is_empty();
        m.record_named(g, name.to_string(), ok, support::describe("rdash", &ours));
    }
}

fn deviation_rows(m: &Matrix, t: &Tools, home: &Path) {
    default_datadir_rows(m, t);
    non_utf8_input_rows(m, t, home);
    non_utf8_argv_rows(m, t, home);
    generate_failure_rows(m, t, home);
    short_batch_rows(m, t, home);
    sigint_prompt_rows(m, t, home);
    limit_rows(m, t, home);
}

/// A rejected argument is named by its position, and by its option name when that is plain
/// ASCII; its content (a password, say) is never echoed. Stock passes the bytes on.
fn non_utf8_argv_rows(m: &Matrix, t: &Tools, home: &Path) {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let g = "deviations";
    const CANARY: &str = "CANARY-5d1c";
    let bad = |prefix: &str| {
        let mut bytes = prefix.as_bytes().to_vec();
        bytes.extend_from_slice(b"\xff\xfe");
        OsString::from_vec(bytes)
    };
    let rows: Vec<(&str, Vec<OsString>, &str)> = vec![
        (
            "-rpcpassword value, then -version",
            vec![bad(&format!("-rpcpassword={CANARY}")), "-version".into()],
            "argument 1 (option -rpcpassword) is not valid UTF-8",
        ),
        (
            "-rpcpassword value after the command is the command's value",
            vec!["getblockcount".into(), bad(&format!("-rpcpassword={CANARY}"))],
            "argument 2 is not valid UTF-8",
        ),
        (
            "a dash-led passphrase after the command",
            vec!["walletpassphrase".into(), bad(&format!("-{CANARY}=x")), "60".into()],
            "argument 2 is not valid UTF-8",
        ),
        (
            "a dash-led passphrase with a plain name after the command",
            vec!["-regtest".into(), "walletpassphrase".into(), bad(&format!("-{CANARY}"))],
            "argument 3 is not valid UTF-8",
        ),
        (
            "-rpcpassword value after other options",
            vec!["-regtest".into(), bad(&format!("-rpcpassword={CANARY}"))],
            "argument 2 (option -rpcpassword) is not valid UTF-8",
        ),
        (
            "--rpcuser value",
            vec![bad(&format!("--rpcuser={CANARY}"))],
            "argument 1 (option -rpcuser) is not valid UTF-8",
        ),
        ("positional value", vec!["-version".into(), bad(CANARY)], "argument 2 is not valid UTF-8"),
        (
            "an option that is not registered (a misplaced secret) is not named",
            vec![bad(&format!("-{CANARY}-private-passphrase="))],
            "argument 1 is not valid UTF-8",
        ),
        (
            "an unregistered key with a plain name, then -version",
            vec![bad(&format!("--{CANARY}")), "-version".into()],
            "argument 1 is not valid UTF-8",
        ),
        (
            "a registered key is still named, -norpcwait",
            vec![bad("-norpcwait=")],
            "argument 1 (option -norpcwait) is not valid UTF-8",
        ),
        (
            "option name that is not plain ASCII",
            vec![bad(&format!("-{CANARY}"))],
            "argument 1 is not valid UTF-8",
        ),
        (
            "option name that is not plain ASCII, then -version",
            vec![bad(&format!("-{CANARY}")), "-version".into()],
            "argument 1 is not valid UTF-8",
        ),
    ];
    for (name, args, says) in rows {
        let row = format!("argument that is not UTF-8: {name}");
        let out = support::output_row(
            std::process::Command::new(&t.rdash).args(&args).env_clear().env("HOME", home),
            &row,
        );
        let ok = out.status.code() == Some(1)
            && out.stdout.is_empty()
            && out.stderr == format!("Error parsing command line arguments: {says}\n").as_bytes();
        m.record_named(
            g,
            row,
            ok,
            format!(
                "code {:?}, stdout {:?}, stderr {:?}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        );
    }
}

/// `-generate` after a `getnewaddress` error with code 0: stock calls `ParseError`, which leaves
/// the request handler null, and dereferences it: the process aborts. rdash-cli reports the
/// error on standard error and exits 1, and does not say a block was generated.
fn generate_failure_rows(m: &Matrix, t: &Tools, home: &Path) {
    use std::os::unix::process::ExitStatusExt;
    let g = "deviations";
    let server = FakeServer::always(Reply::http(
        "500 Internal Server Error",
        &error_body("{\"code\":0,\"message\":\"CANARY failure\"}"),
    ));
    for (name, extra) in
        [("-generate", vec!["-generate"]), ("-generate with a count", vec!["-generate", "3"])]
    {
        let i = Invocation::new(with(srv(server.port), &extra));
        server.clear();
        let row = format!("{name}, getnewaddress error with code 0: exit 1, error on stderr");
        let stock = run_signal(&t.stock, home, &i, &row);
        let stock_requests = server.requests().len();
        server.clear();
        let ours = run(&t.rdash, home, &i);
        let requests = server.requests();
        let sent_getnewaddress = requests.len() == 1
            && String::from_utf8_lossy(&requests[0]).contains("\"method\":\"getnewaddress\"");
        let ok = stock.signal().is_some()
            && stock_requests == 1
            && ours.code == Some(1)
            && ours.stdout.is_empty()
            && ours.stderr.windows(14).any(|w| w == b"CANARY failure")
            && sent_getnewaddress;
        m.record_named(
            g,
            row,
            ok,
            format!(
                "stock status {stock:?}, requests {stock_requests}\n{}",
                support::describe("rdash", &ours)
            ),
        );
    }
}

/// How a program that may die of a signal ended.
fn run_signal(bin: &Path, home: &Path, inv: &Invocation, row: &str) -> std::process::ExitStatus {
    let mut child = std::process::Command::new(bin)
        .args(&inv.args)
        .env_clear()
        .env("HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    support::wait_row(&mut child, row)
}

/// A batch reply with fewer members than were asked for: Core reads `reply[id]` past the end of
/// the array (undefined behaviour; stock aborts on an empty one). rdash-cli reads a null in its
/// place, which the handler rejects as an ordinary error.
fn short_batch_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "deviations";
    let cases: [(&str, &[&str], &str); 5] = [
        ("-getinfo, empty batch reply", &["-getinfo"], "[]"),
        (
            "-getinfo, one member of four",
            &["-getinfo"],
            "[{\"result\":null,\"error\":null,\"id\":0}]",
        ),
        (
            "-getinfo, two members of four",
            &["-getinfo"],
            "[{\"result\":null,\"error\":null,\"id\":0},{\"result\":null,\"error\":null,\"id\":1}]",
        ),
        ("-netinfo, empty batch reply", &["-netinfo"], "[]"),
        ("-netinfo, one member of two", &["-netinfo"], "[{\"result\":[],\"error\":null,\"id\":0}]"),
    ];
    for (name, args, batch) in cases {
        let server = FakeServer::start(move |_, req| {
            let text = String::from_utf8_lossy(req);
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
            if body.starts_with('[') {
                Reply::json(batch)
            } else {
                Reply::json(&rpc_body("[\"w\"]"))
            }
        });
        let i = Invocation::new(with(srv(server.port), args));
        let ours = run_raw_status(&t.rdash, home, &i);
        let batches = server
            .requests()
            .iter()
            .filter(|r| {
                String::from_utf8_lossy(r)
                    .split("\r\n\r\n")
                    .nth(1)
                    .is_some_and(|b| b.starts_with('['))
            })
            .count();
        let stderr = String::from_utf8_lossy(&ours.stderr);
        let ok = ours.code == Some(1)
            && ours.stdout.is_empty()
            && stderr.starts_with("error: ")
            && !stderr.contains("panicked")
            && batches == 1;
        m.record_named(
            g,
            format!("short batch reply: {name}"),
            ok,
            format!("batches {batches}\n{}", support::describe("rdash", &ours)),
        );
    }
}

fn run_raw_status(bin: &Path, home: &Path, inv: &Invocation) -> support::Output {
    support::run_raw(bin, home, inv)
}

/// A program waiting at the password prompt of a pseudo-terminal, echo off.
struct AtPrompt {
    child: std::process::Child,
    pty: support::Pty,
    readers: Vec<std::thread::JoinHandle<()>>,
    /// The row it is run for, which a deadline names.
    row: String,
}

impl AtPrompt {
    /// Start `bin -stdinrpcpass` on a fresh pseudo-terminal; `ignoring` signals are ignored in
    /// the child, as under `trap '' INT`. `row` names the row it is run for.
    fn start(bin: &Path, home: &Path, port: u16, ignoring: &[i32], row: &str) -> AtPrompt {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::sync::Arc;
        let pty = support::open_pty();
        let mut cmd = Command::new(bin);
        cmd.args(["-regtest", "-rpcuser=u", "-stdinrpcpass", "getblockcount"])
            .arg(format!("-rpcport={port}"))
            .env_clear()
            .env("HOME", home)
            .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let ignored = ignoring.to_vec();
        // SAFETY: only `setrlimit` and `signal`, which are async-signal-safe, run between fork
        // and exec; a signal that dumps core (SIGQUIT) must not leave a core file behind. The
        // signals the rows send start at their default action whatever this test inherited.
        unsafe {
            cmd.pre_exec(move || {
                let none = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::setrlimit(libc::RLIMIT_CORE, &none);
                support::ignore_prompt_signals();
                support::default_prompt_signals();
                for &signal in &ignored {
                    libc::signal(signal, libc::SIG_IGN);
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().unwrap();
        drop(cmd);
        let err = Arc::new(Mutex::new(Vec::new()));
        let readers = vec![
            support::drain(child.stderr.take().unwrap(), err.clone()),
            support::drain(child.stdout.take().unwrap(), Arc::new(Mutex::new(Vec::new()))),
        ];
        // The prompt is out and echo is off: the program waits for the password.
        let started = std::time::Instant::now();
        let mut at = AtPrompt {
            child,
            pty,
            readers,
            row: format!("{row} ({})", bin.display()),
        };
        while !(err.lock().unwrap().ends_with(b"> ") && !at.echo_on()) {
            if started.elapsed() > Duration::from_secs(30) {
                let _ = at.child.kill();
                let _ = at.child.wait();
                panic!("XCLI row {:?}: never prompted; killed", at.row);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if let Some(why) = support::prompt_signals_ignored(at.child.id() as libc::pid_t, ignoring) {
            let _ = at.child.kill();
            let _ = at.child.wait();
            panic!("XCLI row {:?}: {why}; killed", at.row);
        }
        at
    }

    fn echo_on(&self) -> bool {
        use std::os::fd::AsRawFd;
        // SAFETY: the master of a pseudo-terminal; `termios` is plain data that `tcgetattr` fills.
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        assert_eq!(unsafe { libc::tcgetattr(self.pty.master.as_raw_fd(), &mut termios) }, 0);
        termios.c_lflag & libc::ECHO != 0
    }

    fn signal(&self, signal: i32) {
        // SAFETY: signalling our own child.
        unsafe { libc::kill(self.child.id() as i32, signal) };
    }

    /// Wait for the end; the signal that ended the program and whether echo is on afterwards.
    /// Past [`support::ROW_DEADLINE`] the program is killed and the row fails by name.
    fn finish(mut self) -> (Option<i32>, bool) {
        use std::os::unix::process::ExitStatusExt;
        let status = support::wait_row(&mut self.child, &self.row);
        let echo = self.echo_on();
        for r in self.readers {
            support::join_row(r, &self.row);
        }
        (status.signal(), echo)
    }
}

/// A signal that ends the password prompt: stock dies with the terminal's echo still off, so the
/// user's shell types invisibly; rdash-cli restores the terminal first and dies of the same
/// signal. A signal the parent ignored stays ignored in both.
fn sigint_prompt_rows(m: &Matrix, t: &Tools, home: &Path) {
    let g = "deviations";
    let dead = free_port();
    let outcome = |bin: &Path, signal: i32, row: &str| {
        let at = AtPrompt::start(bin, home, dead, &[], row);
        at.signal(signal);
        at.finish()
    };
    for (name, signal) in [
        ("SIGINT", libc::SIGINT),
        ("SIGTERM", libc::SIGTERM),
        ("SIGHUP", libc::SIGHUP),
        ("SIGQUIT", libc::SIGQUIT),
    ] {
        let row = format!("password prompt ended by {name}: terminal echo restored");
        let (ours, stock) = (outcome(&t.rdash, signal, &row), outcome(&t.stock, signal, &row));
        // Stock's SIGQUIT ends as SIGABRT; it is the echo that the row is about.
        m.record_named(
            g,
            row,
            ours == (Some(signal), true) && stock.0.is_some() && !stock.1,
            format!(
                "rdash (signal, echo on) {ours:?}, expected (Some({signal}), true); \
                 stock {stock:?}, expected a signal and echo off"
            ),
        );
    }

    // SIGINT ignored by the parent: the program keeps prompting, with echo still off.
    let survives_row =
        "password prompt, SIGINT ignored by the parent: still prompting, as stock".to_string();
    let survives = |bin: &Path| {
        let at = AtPrompt::start(bin, home, dead, &[libc::SIGINT], &survives_row);
        at.signal(libc::SIGINT);
        std::thread::sleep(Duration::from_millis(300));
        let mut at = at;
        let alive = at.child.try_wait().unwrap().is_none();
        let still_off = !at.echo_on();
        at.child.kill().unwrap();
        let _ = at.finish();
        (alive, still_off)
    };
    let (ours, stock) = (survives(&t.rdash), survives(&t.stock));
    m.record_named(
        g,
        survives_row,
        ours == (true, true) && stock == (true, true),
        format!(
            "rdash (alive, echo off) {ours:?}; stock {stock:?}; expected (true, true) for both"
        ),
    );
    // A prompt whose job was put in the background, then `kill %1`: SIGTERM and SIGCONT. The
    // handler writes the terminal settings back, which from a background process group would
    // stop the process with SIGTTOU unless that signal is blocked; it must still die.
    let row =
        "password prompt in a background job, SIGTERM and SIGCONT: killed by SIGTERM, as stock"
            .to_string();
    let (ours, stock) = (
        background_prompt_verdict(&t.rdash, home, dead, &row),
        background_prompt_verdict(&t.stock, home, dead, &row),
    );
    m.record_named(
        g,
        row,
        ours == b'T' && stock == b'T',
        format!(
            "rdash {:?}, stock {:?}; expected 'T' (killed by SIGTERM) for both; 'S' is stopped \
             again and again, 'A' alive, 'E' exited",
            ours as char, stock as char
        ),
    );
}

/// Runs `bin -stdinrpcpass` as the foreground job of a new session on a pseudo-terminal, waits
/// for the prompt, moves the job to the background, sends it SIGTERM and SIGCONT and reports
/// how it ended: `T` killed by SIGTERM, `S` stopped (by SIGTTOU), `A` alive, `E` exited, `?`
/// killed by another signal. The session's leader is a forked helper that only makes
/// async-signal-safe calls until it exits. Past [`support::ROW_DEADLINE`] both are killed and
/// the row `row` fails by name.
fn background_prompt_verdict(bin: &Path, home: &Path, port: u16, row: &str) -> u8 {
    use std::ffi::{CString, c_char, c_void};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::Arc;
    let pty = support::open_pty();
    let (slave, master) = (pty.slave.as_raw_fd(), pty.master.as_raw_fd());
    let c = |s: &str| CString::new(s).unwrap();
    let args = [
        c(bin.to_str().unwrap()),
        c("-regtest"),
        c("-rpcuser=u"),
        c("-stdinrpcpass"),
        c("getblockcount"),
        c(&format!("-rpcport={port}")),
    ];
    let argv: Vec<*const c_char> =
        args.iter().map(|a| a.as_ptr()).chain([std::ptr::null()]).collect();
    let home_env = c(&format!("HOME={}", home.display()));
    let envp = [home_env.as_ptr(), std::ptr::null()];
    // Close-on-exec, as the pseudo-terminal: the program and anything another thread spawns
    // meanwhile do not inherit them. Owned, so that a row failing on any path closes its ends.
    // SAFETY: four fresh descriptors, owned from here on.
    let [go_read, go_write] = support::cloexec_pipe().map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
    // SAFETY: as above.
    let [done_read, done_write] =
        support::cloexec_pipe().map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
    let (go_r, go_w) = (go_read.as_raw_fd(), go_write.as_raw_fd());
    let (done_r, done_w) = (done_read.as_raw_fd(), done_write.as_raw_fd());
    let helper_ends = (go_read, done_write);
    let row = format!("{row} ({})", bin.display());
    // Kills the program (when its id is known) and the helper, and fails the row.
    let abandon = |helper: libc::pid_t, program: libc::pid_t, why: &str| -> ! {
        // SAFETY: signalling and reaping processes forked here.
        unsafe {
            if program > 0 {
                libc::kill(program, libc::SIGKILL);
            }
            libc::kill(helper, libc::SIGKILL);
            libc::waitpid(helper, std::ptr::null_mut(), 0);
        }
        panic!("XCLI row {row:?}: {why}; killed")
    };
    // Whether `fd` has something to read before `deadline`.
    let readable = |fd: libc::c_int, deadline: std::time::Instant| loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd, owned here.
        match unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) } {
            1 => return true,
            0 => return false,
            _ => {
                let e = std::io::Error::last_os_error();
                assert_eq!(e.kind(), std::io::ErrorKind::Interrupted, "poll: {e}");
            }
        }
    };
    // Reads one `T` the helper wrote; its end of the pipe is the only one, so a helper that died
    // ends the read at once.
    fn read_from_helper<T: Default>(fd: libc::c_int) -> Option<T> {
        let mut value = T::default();
        // SAFETY: `value` is `size_of::<T>()` bytes of plain data, owned here.
        let n =
            unsafe { libc::read(fd, (&raw mut value).cast::<std::ffi::c_void>(), size_of::<T>()) };
        (n == size_of::<T>() as isize).then_some(value)
    }
    let deadline = std::time::Instant::now() + support::ROW_DEADLINE;
    // SAFETY: plain system calls on descriptors and arrays owned here. In the forked children
    // only async-signal-safe functions run (no allocation, no locks) until `execve` or `_exit`.
    unsafe {
        let helper = libc::fork();
        assert!(helper >= 0);
        if helper != 0 {
            // The helper's ends: a helper that dies closes the only writer of `done`.
            drop(helper_ends);
        }
        if helper == 0 {
            libc::close(master);
            // The helper never execs, so close-on-exec does not apply: it keeps the slave and
            // its two pipe ends, at 3, 4 and 5, and closes every other descriptor it inherited
            // from this process (the terminals of rows other threads run among them). Copied
            // past 5 first, so that no copy lands on a descriptor still to be moved. A step that
            // fails ends the helper, and the row fails as "the helper died before starting the
            // program"; only a kernel without close_range (before 5.9) keeps what it inherited.
            #[cfg(target_os = "linux")]
            let (slave, go_r, done_w) = {
                let high =
                    [slave, go_r, done_w].map(|fd| libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 6));
                for (fd, to) in high.into_iter().zip(3..) {
                    if fd < 0 || libc::dup3(fd, to, libc::O_CLOEXEC) != to {
                        libc::_exit(126);
                    }
                }
                if libc::syscall(libc::SYS_close_range, 6u32, u32::MAX, 0u32) != 0
                    && *libc::__errno_location() != libc::ENOSYS
                {
                    libc::_exit(126);
                }
                (3, 4, 5)
            };
            libc::setsid();
            libc::ioctl(slave, libc::TIOCSCTTY, 0);
            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            let child = libc::fork();
            if child == 0 {
                libc::setpgid(0, 0);
                libc::tcsetpgrp(slave, libc::getpgrp());
                libc::signal(libc::SIGTTOU, libc::SIG_DFL);
                // SIGTERM as the row needs it, and SIGHUP too: killing the helper (the session's
                // leader) hangs the program up, which must end it even if this test ignores
                // SIGHUP.
                support::ignore_prompt_signals();
                support::default_prompt_signals();
                libc::dup2(slave, 0);
                libc::dup2(slave, 2);
                libc::execve(args[0].as_ptr(), argv.as_ptr(), envp.as_ptr());
                libc::_exit(127);
            }
            // The program's id first, so that a deadline in the parent can kill it.
            libc::write(done_w, (&raw const child).cast::<c_void>(), size_of::<libc::pid_t>());
            let mut byte = 0u8;
            libc::read(go_r, (&raw mut byte).cast::<c_void>(), 1);
            libc::tcsetpgrp(slave, libc::getpgrp());
            libc::kill(child, libc::SIGTERM);
            libc::kill(child, libc::SIGCONT);
            let mut verdict = b'A';
            for _ in 0..60 {
                let mut status = 0;
                if libc::waitpid(child, &mut status, libc::WUNTRACED | libc::WNOHANG) == child {
                    if libc::WIFSTOPPED(status) {
                        verdict = b'S';
                        libc::kill(child, libc::SIGCONT);
                    } else {
                        verdict = if !libc::WIFSIGNALED(status) {
                            b'E'
                        } else if libc::WTERMSIG(status) == libc::SIGTERM {
                            b'T'
                        } else {
                            b'?'
                        };
                        break;
                    }
                }
                libc::usleep(50_000);
            }
            if verdict == b'A' || verdict == b'S' {
                libc::kill(child, libc::SIGKILL);
                libc::waitpid(child, std::ptr::null_mut(), 0);
            }
            libc::write(done_w, (&raw const verdict).cast::<c_void>(), 1);
            libc::_exit(0);
        }
        if !readable(done_r, deadline) {
            abandon(helper, 0, "the helper did not start the program within its deadline");
        }
        let program = match read_from_helper::<libc::pid_t>(done_r) {
            Some(program) if program > 0 => program,
            Some(_) => abandon(helper, 0, "the helper could not fork the program"),
            None => abandon(helper, 0, "the helper died before starting the program"),
        };
        // The prompt is out and echo is off: the program waits for the password.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let reader = support::drain(pty.master.try_clone().unwrap(), seen.clone());
        let started = std::time::Instant::now();
        loop {
            let mut termios: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(master, &mut termios), 0);
            if seen.lock().unwrap().ends_with(b"> ") && termios.c_lflag & libc::ECHO == 0 {
                break;
            }
            if started.elapsed() > Duration::from_secs(30) {
                abandon(helper, program, "the program never prompted within 30 s");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if let Some(why) = support::prompt_signals_ignored(program, &[]) {
            abandon(helper, program, &why);
        }
        let go_byte = 0u8;
        libc::write(go_w, (&raw const go_byte).cast::<c_void>(), 1);
        if !readable(done_r, deadline) {
            abandon(helper, program, "no verdict within its deadline");
        }
        let Some(verdict) = read_from_helper::<u8>(done_r) else {
            abandon(helper, program, "the helper died before its verdict")
        };
        // The helper exits right after its verdict.
        libc::waitpid(helper, std::ptr::null_mut(), 0);
        drop(pty);
        support::join_row(reader, &row);
        verdict
    }
}

/// libevent's client has no bounds on a reply (a stock dash-cli reads a 16 MiB header line, a
/// million headers, any body); rdash-cli's `http::Limits::DEFAULT` does. The replies just inside
/// the limits are rows of the `http` group, compared with stock; here are the ones just beyond.
fn limit_rows(m: &Matrix, t: &Tools, home: &Path) {
    let ok = rpc_body("\"hi\"");
    let with_length = |head: &[u8]| {
        [head, format!("Content-Length: {}\r\n\r\n{ok}", ok.len()).as_bytes()].concat()
    };
    let header_line = limit_header_line;
    // Stock reads all of these; the programs are compared only in that stock accepts them.
    let cheap: Vec<(&str, Vec<u8>)> = vec![
        (
            "header line of 64 KiB + 1",
            with_length(&[b"HTTP/1.1 200 OK\r\n".to_vec(), header_line(64 * 1024 + 1)].concat()),
        ),
        (
            "status line of 64 KiB + 1",
            with_length(
                &[b"HTTP/1.1 200 ".to_vec(), vec![b'a'; 64 * 1024 + 1 - 13], b"\r\n".to_vec()]
                    .concat(),
            ),
        ),
        (
            "1001 headers",
            with_length(
                &[b"HTTP/1.1 200 OK\r\n".to_vec(), "X: 1\r\n".repeat(1000).into_bytes()].concat(),
            ),
        ),
        (
            "header bytes above 256 KiB",
            with_length(&[b"HTTP/1.1 200 OK\r\n".to_vec(), header_line(60_000).repeat(5)].concat()),
        ),
        (
            "header bytes above 256 KiB, in the trailer",
            [
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
                format!("{:x}\r\n{ok}\r\n0\r\n", ok.len()).into_bytes(),
                header_line(60_000).repeat(5),
                b"\r\n".to_vec(),
            ]
            .concat(),
        ),
    ];
    for (name, wire) in cheap {
        limit_row(m, t, home, name, Reply::Raw(wire), true);
    }
    // Bodies beyond 1 GiB: the generated bytes cannot make a reply stock would print, so only
    // rdash-cli runs.
    const GIB: u64 = 1 << 30;
    let zeros = |out: &mut dyn std::io::Write, mut n: u64| -> std::io::Result<()> {
        let block = vec![b'0'; 1 << 20];
        while n > 0 {
            let take = n.min(block.len() as u64) as usize;
            out.write_all(&block[..take])?;
            n -= take as u64;
        }
        Ok(())
    };
    limit_row(
        m,
        t,
        home,
        "body of 1 GiB + 1 by Content-Length",
        Reply::Stream(Generator::new(move |out| {
            out.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", GIB + 1).as_bytes(),
            )?;
            zeros(out, GIB + 1)
        })),
        false,
    );
    limit_row(
        m,
        t,
        home,
        "body of 1 GiB + 1 MiB in chunks",
        Reply::Stream(Generator::new(move |out| {
            out.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")?;
            for _ in 0..=(GIB >> 20) {
                out.write_all(b"100000\r\n")?;
                zeros(out, 1 << 20)?;
                out.write_all(b"\r\n")?;
            }
            out.write_all(b"0\r\n\r\n")
        })),
        false,
    );
    limit_row(
        m,
        t,
        home,
        "body of 1 GiB + 1 up to the close",
        Reply::Stream(Generator::new(move |out| {
            out.write_all(b"HTTP/1.0 200 OK\r\n\r\n")?;
            zeros(out, GIB + 1)
        })),
        false,
    );
}

/// One reply beyond a limit: rdash-cli exits 1 with libevent's "too long" error after exactly one
/// request; stock (when `stock_accepts`) prints the result.
fn limit_row(m: &Matrix, t: &Tools, home: &Path, name: &str, reply: Reply, stock_accepts: bool) {
    let g = "deviations";
    eprintln!("row [{g}] limit: {name}");
    let server = FakeServer::always(reply);
    let i = Invocation::new(with(srv(server.port), &["getblockcount"]));
    let mut problems = String::new();
    if stock_accepts {
        let stock = run(&t.stock, home, &i);
        if stock.code != Some(0) || stock.stdout != b"hi\n" {
            problems.push_str(&format!(
                "premise failed: stock takes this reply\n{}",
                support::describe("stock", &stock)
            ));
        }
        server.clear();
    }
    let ours = run(&t.rdash, home, &i);
    let requests = server.requests().len();
    let says_too_long = String::from_utf8_lossy(&ours.stderr)
        .contains("error code 5 - \"response body is larger than allowed\"");
    if ours.code != Some(1) || !ours.stdout.is_empty() || !says_too_long || requests != 1 {
        problems.push_str(&format!("requests {requests}\n{}", support::describe("rdash", &ours)));
    }
    m.record_named(g, format!("limit: {name}"), problems.is_empty(), problems);
}
