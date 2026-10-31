//! What the process tests share: rdashd in an isolated home, a node run to "Done loading", and
//! the dashd v24 the differential tests compare with.
#![allow(dead_code)] // each test crate uses a subset

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

pub use node_test_ports::free_port;

pub mod mine;

pub const RDASHD: &str = env!("CARGO_BIN_EXE_rdashd");

/// How long a node may take to start or stop on a loaded box.
pub const PATIENCE: Duration = Duration::from_secs(120);

/// `binary` with `HOME` (and the XDG directories) in `home`, so the default data directory is
/// never the real one.
pub fn command(binary: &Path, home: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(binary);
    cmd.args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env_remove("RDASHD_TEST_HOOK")
        .stdin(Stdio::null());
    cmd
}

pub fn rdashd(home: &Path, args: &[&str]) -> Command {
    command(Path::new(RDASHD), home, args)
}

/// Collects `child`'s output and exit, killing it and failing after [`PATIENCE`].
pub fn finish(mut child: Child) -> Output {
    fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let (out, err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let deadline = Instant::now() + PATIENCE;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("process {} did not exit within {PATIENCE:?}", child.id());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

/// Runs `cmd` to its exit, bounded by [`PATIENCE`].
pub fn output(mut cmd: Command) -> Output {
    finish(cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap())
}

pub fn run(home: &Path, args: &[&str]) -> Output {
    output(rdashd(home, args))
}

/// rdashd on regtest in `datadir`, with `extra` arguments. The RPC server gets a free port
/// unless `extra` picks one or turns the server off, and the node does not listen unless `extra`
/// decides it: tests run in parallel, and Core's default binds fixed ports.
pub fn regtest(home: &Path, datadir: &Path, extra: &[&str]) -> Command {
    regtest_on(Path::new(RDASHD), home, datadir, extra)
}

/// [`regtest`] for `binary`: rdashd, or the dashd a differential test compares with.
pub fn regtest_on(binary: &Path, home: &Path, datadir: &Path, extra: &[&str]) -> Command {
    let mut cmd = command(binary, home, &[]);
    cmd.arg(datadir_arg(datadir)).arg("-regtest");
    if !decides_listen(extra) {
        cmd.arg("-listen=0");
    }
    cmd.args(extra);
    if !chooses_rpc_port(extra) {
        cmd.arg(format!("-rpcport={}", free_port()));
    }
    cmd
}

/// Whether `extra` names a listening option or one whose parameter interaction decides
/// `-listen`, so [`regtest`] adds no `-listen=0`.
fn decides_listen(extra: &[&str]) -> bool {
    extra.iter().any(|a| {
        let name = a.trim_start_matches('-').split('=').next().unwrap_or_default();
        matches!(
            name,
            "listen" | "nolisten" | "bind" | "whitebind" | "connect" | "noconnect" | "devnet"
        )
    })
}

/// Whether `extra` picks the RPC port or turns the server off, so [`regtest`] picks none.
fn chooses_rpc_port(extra: &[&str]) -> bool {
    extra.iter().any(|a| {
        let name = a.trim_start_matches('-').split('=').next().unwrap_or_default();
        matches!(name, "rpcport" | "server" | "noserver" | "devnet")
    })
}

pub fn run_regtest(home: &Path, datadir: &Path, extra: &[&str]) -> Output {
    output(regtest(home, datadir, extra))
}

/// The regtest network directory in `datadir`.
pub fn net(datadir: &Path) -> PathBuf {
    datadir.join("regtest")
}

pub fn debug_log(datadir: &Path) -> PathBuf {
    net(datadir).join("debug.log")
}

/// `path` quoted for a shell command line.
pub fn sh_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn datadir_arg(datadir: &Path) -> String {
    format!("-datadir={}", datadir.display())
}

/// A home and an empty data directory in it.
pub fn home_and_datadir() -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let datadir = home.path().join("dd");
    std::fs::create_dir_all(&datadir).unwrap();
    (home, datadir)
}

/// Every path under `dir` with its length (directories: `None`) and modification time.
pub fn tree(dir: &Path) -> BTreeMap<PathBuf, (Option<u64>, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let meta = e.metadata().unwrap();
            out.insert(e.path(), (meta.is_file().then_some(meta.len()), meta.modified().unwrap()));
            if meta.is_dir() {
                todo.push(e.path());
            }
        }
    }
    out
}

/// A regtest network directory whose chainstate is LevelDB, as Dash Core leaves it.
pub fn core_shaped(base: &Path) -> PathBuf {
    let chainstate = base.join("regtest").join("chainstate");
    std::fs::create_dir_all(&chainstate).unwrap();
    std::fs::write(chainstate.join("CURRENT"), "MANIFEST-000002\n").unwrap();
    std::fs::write(chainstate.join("MANIFEST-000002"), [0u8; 16]).unwrap();
    base.join("regtest")
}

/// rdashd's refusal of the Core network directory `net`.
pub fn core_datadir_refusal(net: &Path) -> String {
    let shown = net.display();
    format!(
        "Error: {shown} holds a Dash Core data directory; rdashd does not write to it. To import \
         it, use -importcoredatadir={shown}.\n"
    )
}

/// The log of a node that went through Core's shutdown.
pub fn assert_shut_down_in_order(log: &str) {
    let prepare = log.find("PrepareShutdown: In progress...").expect("PrepareShutdown");
    assert!(prepare < log.find("Shutdown: done").expect("Shutdown: done"), "{log}");
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Waits until `done`, failing after [`PATIENCE`].
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A node process; killed if a test fails before stopping it.
pub struct Running {
    child: Option<Child>,
    /// Its debug log.
    pub log: PathBuf,
}

impl Running {
    pub fn id(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    /// Sends `sig` and collects the exit.
    #[cfg(unix)]
    pub fn stop(mut self, sig: libc::c_int) -> Output {
        let child = self.child.take().unwrap();
        signal(child.id(), sig);
        finish(child)
    }

    /// Sends `sig` and asserts a clean exit.
    #[cfg(unix)]
    pub fn stop_ok(self, sig: libc::c_int) -> Output {
        let out = self.stop(sig);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        out
    }

    /// Waits for the process to exit by itself.
    pub fn wait(mut self) -> Output {
        finish(self.child.take().unwrap())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Running {
    /// Spawns `cmd` with piped output.
    pub fn spawn(mut cmd: Command, log: PathBuf) -> Running {
        let child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        Running {
            child: Some(child),
            log,
        }
    }

    /// Whether the process has exited (its output is left for [`Running::wait`]).
    pub fn exited(&mut self) -> bool {
        self.child.as_mut().unwrap().try_wait().unwrap().is_some()
    }
}

/// Spawns `cmd` with piped output and waits until `ready`, failing if the process exits first.
pub fn spawn_until(cmd: Command, log: PathBuf, ready: impl FnMut() -> bool) -> Running {
    try_spawn_until(cmd, log, ready).unwrap_or_else(|out| {
        panic!("the node exited ({}) during init: {}", out.status, stderr(&out))
    })
}

/// [`spawn_until`], with the output of a process that exits first as the error.
fn try_spawn_until(
    cmd: Command,
    log: PathBuf,
    mut ready: impl FnMut() -> bool,
) -> Result<Running, Output> {
    let mut running = Running::spawn(cmd, log);
    let deadline = Instant::now() + PATIENCE;
    while !ready() {
        if running.exited() {
            return Err(running.wait());
        }
        assert!(Instant::now() < deadline, "the node did not finish init");
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(running)
}

/// Spawns `cmd` and waits until `log` says init is done (in what this run appends to it).
pub fn spawn_until_loaded(cmd: Command, log: PathBuf) -> Running {
    try_spawn_until_loaded(cmd, log).unwrap_or_else(|out| {
        panic!("the node exited ({}) during init: {}", out.status, stderr(&out))
    })
}

fn try_spawn_until_loaded(cmd: Command, log: PathBuf) -> Result<Running, Output> {
    let path = log.clone();
    let before = std::fs::metadata(&path).map_or(0, |m| m.len() as usize);
    try_spawn_until(cmd, log, || {
        read(&path).get(before..).is_some_and(|new| new.contains("init message: Done loading"))
    })
}

/// Starts rdashd on regtest in `datadir` with `extra` arguments and waits until init is done.
pub fn start(home: &Path, datadir: &Path, extra: &[&str]) -> Running {
    let extra = [&["-printtoconsole=0"], extra].concat();
    if chooses_rpc_port(&extra) {
        return spawn_until_loaded(regtest(home, datadir, &extra), debug_log(datadir));
    }
    start_on_free_port(datadir, free_port, |port| {
        let rpcport = format!("-rpcport={port}");
        regtest(home, datadir, &[&extra[..], &[rpcport.as_str()]].concat())
    })
    .0
}

/// How many ports a start tries before a lost port race fails the test.
const PORT_ATTEMPTS: usize = 3;

/// Starts the node `node(port)` builds on a port from `ports` and waits until init is done. A
/// [`free_port`] is free when picked but not reserved, so another process can bind it before the
/// node does; the node then exits during init with Core's HTTP error, having bound neither
/// endpoint (BL-221). That start is undone, its lines cut from the debug log so the test reads
/// only the start that succeeded, and the node starts again on the next port, up to
/// [`PORT_ATTEMPTS`] times. Any other exit fails the test as [`spawn_until`] does, and so does a
/// node that bound only `::1`: it runs, so it is not started again.
// Core: init.cpp:1636-1637, httpserver.cpp:424-434, :498-501
pub fn start_on_free_port(
    datadir: &Path,
    mut ports: impl FnMut() -> u16,
    node: impl Fn(u16) -> Command,
) -> (Running, u16) {
    let log = debug_log(datadir);
    for attempt in 1..=PORT_ATTEMPTS {
        let port = ports();
        let before = std::fs::metadata(&log).ok().map(|m| m.len());
        let started = try_spawn_until_loaded(node(port), log.clone());
        let lost = read_since(&log, before).contains(&lost_line(port));
        let out = match started {
            // Init succeeds once one endpoint binds, so a node that lost only 127.0.0.1 (where
            // the tests connect) runs while another process may answer there. It has written its
            // datadir, so it is not retried: the test fails, saying why.
            Ok(_) if lost => {
                panic!("another process holds 127.0.0.1 port {port}; the node bound only ::1")
            }
            Ok(running) => return (running, port),
            Err(out) => out,
        };
        if !(lost && stderr(&out) == HTTP_START_FAILED) || attempt == PORT_ATTEMPTS {
            panic!("the node exited ({}) during init on port {port}: {}", out.status, stderr(&out));
        }
        match before {
            // `ShrinkDebugFile` may have cut a long log below `len`: keep what it left.
            Some(len) => {
                let file = std::fs::File::options().write(true).open(&log).unwrap();
                file.set_len(len.min(file.metadata().unwrap().len())).unwrap()
            }
            None => std::fs::remove_file(&log).unwrap(),
        }
    }
    unreachable!("the last attempt returns or panics")
}

/// What `log` gained since it was `before` bytes long (`None`: it did not exist).
fn read_since(log: &Path, before: Option<u64>) -> String {
    let text = read(log);
    text.get(before.unwrap_or(0) as usize..).unwrap_or(&text).to_owned()
}

/// The log line of a failed bind on 127.0.0.1.
fn lost_line(port: u16) -> String {
    format!("Binding RPC on address 127.0.0.1 port {port} failed.")
}

/// Core's `AppInitServers` failure on stderr.
pub const HTTP_START_FAILED: &str =
    "Error: Unable to start HTTP server. See debug log for details.\n";

#[cfg(unix)]
pub fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: signalling a process this test started.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, sig) }, 0);
}

/// Whether process `pid` has exited (a zombie counts: a daemon's new parent reaps it).
#[cfg(unix)]
pub fn gone(pid: u32) -> bool {
    // SAFETY: signal 0 only checks the process.
    let exists = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit(')').next().is_some_and(|rest| rest.trim_start().starts_with('Z'))
    });
    !exists || zombie
}

/// The pinned Dash Core release the differential tests compare with.
pub const CORE_VERSION: &str = "v24.0.0-rc.4";

fn dashd_version(path: &Path) -> Option<String> {
    // A private data directory: a concurrent `-version` on a shared one races on settings.json.
    let dir = tempfile::tempdir().ok()?;
    let out = Command::new(path).arg("-version").arg(datadir_arg(dir.path())).output().ok()?;
    String::from_utf8(out.stdout).ok()?.lines().next().map(str::to_owned)
}

fn is_pinned(path: &Path) -> bool {
    dashd_version(path).is_some_and(|v| v.ends_with(&format!("version {CORE_VERSION}")))
}

/// The v24 dashd for a test that cannot pass without it, such as a differential gate (DIFF-RPC):
/// [`find_dashd`] without the `SKIP_DASHD_TESTS` opt-out, so skipping fails instead of passing.
pub fn require_dashd() -> PathBuf {
    assert!(
        std::env::var_os("SKIP_DASHD_TESTS").is_none(),
        "this test compares rdashd with dashd {CORE_VERSION} and cannot be skipped: unset \
         SKIP_DASHD_TESTS (see contrib/setup-dashd.py)"
    );
    pinned_dashd(".")
}

/// The v24 dashd, or `None` when dashd tests are skipped: `DASHD_V24_PATH`, else setup-dashd's
/// cache next to `DASHD_PATH` or in the home directory (as dash-rpc-spec's tests find it).
pub fn find_dashd() -> Option<PathBuf> {
    if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
        eprintln!("Skipping dashd integration test (SKIP_DASHD_TESTS is set)");
        return None;
    }
    Some(pinned_dashd(", or set SKIP_DASHD_TESTS=1 to skip."))
}

/// The pinned dashd; panics with a setup hint ending in `hint` when there is none.
fn pinned_dashd(hint: &str) -> PathBuf {
    if let Some(p) = std::env::var_os("DASHD_V24_PATH") {
        let p = PathBuf::from(p);
        assert!(is_pinned(&p), "DASHD_V24_PATH ({}) is not dashd {CORE_VERSION}", p.display());
        return p;
    }
    let version = CORE_VERSION.strip_prefix('v').unwrap();
    let mut candidates = Vec::new();
    if let Some(p) = std::env::var_os("DASHD_PATH") {
        let p = PathBuf::from(p);
        // <cache>/dashcore-<version>/bin/dashd
        let cache = p.ancestors().nth(3).map(|c| c.join(format!("dashcore-{version}/bin/dashd")));
        candidates.push(p);
        candidates.extend(cache);
    }
    for home in ["HOME", "USERPROFILE"].into_iter().filter_map(std::env::var_os) {
        candidates.push(
            PathBuf::from(home).join(format!(".rust-dashcore-test/dashcore-{version}/bin/dashd")),
        );
    }
    let Some(found) = candidates.into_iter().find(|p| is_pinned(p)) else {
        panic!(
            "dashd {CORE_VERSION} not found. Run `DASHVERSION={version} python3 \
             contrib/setup-dashd.py` or set DASHD_V24_PATH{hint}"
        )
    };
    found
}

/// Base64 with padding (Python's `b64encode`, for Basic credentials).
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n =
            chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// A raw HTTP exchange on a fresh connection: the reply's bytes until the server closes, or
/// until `Content-Length` bytes of body have come.
pub fn http_raw(port: u16, request: &[u8]) -> Vec<u8> {
    use std::io::{Read as _, Write as _};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    stream.write_all(request).unwrap();
    let mut out = Vec::new();
    let mut buf = [0; 16 * 1024];
    loop {
        if let Some(end) = complete_reply(&out) {
            out.truncate(end);
            return out;
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return out,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
}

/// The length of the first whole reply in `bytes` that states its `Content-Length`.
pub fn complete_reply(bytes: &[u8]) -> Option<usize> {
    let head_end = bytes.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = String::from_utf8_lossy(&bytes[..head_end]).to_ascii_lowercase();
    let length: usize =
        head.lines().find_map(|l| l.strip_prefix("content-length: "))?.trim().parse().ok()?;
    (bytes.len() >= head_end + length).then_some(head_end + length)
}

/// An HTTP reply: status, headers, body.
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Reply {
    pub fn parse(bytes: &[u8]) -> Reply {
        let text = String::from_utf8_lossy(bytes);
        let (head, body) = text.split_once("\r\n\r\n").expect("a reply head");
        let mut lines = head.split("\r\n");
        let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
        let headers = lines
            .map(|l| {
                let (k, v) = l.split_once(": ").unwrap();
                (k.to_owned(), v.to_owned())
            })
            .collect();
        Reply {
            status,
            headers,
            body: body.to_owned(),
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The JSON text of a 200 reply's `result` to a call with id 1.
    pub fn result(&self) -> &str {
        assert_eq!(self.status, 200, "{}", self.body);
        let body = self.body.strip_prefix(r#"{"result":"#).expect(&self.body);
        body.strip_suffix(",\"error\":null,\"id\":1}\n").expect(&self.body)
    }

    /// The HTTP status and the JSON text of the `error` of a reply to a call with id 1.
    pub fn error(&self) -> (u16, &str) {
        let body = self.body.strip_prefix(r#"{"result":null,"error":"#).expect(&self.body);
        (self.status, body.strip_suffix(",\"id\":1}\n").expect(&self.body))
    }
}

/// `POST <path>` of `body` with Basic credentials `user_pass` (none when `None`).
pub fn post(port: u16, path: &str, user_pass: Option<&str>, body: &str) -> Reply {
    let auth = user_pass
        .map(|up| format!("Authorization: Basic {}\r\n", base64(up.as_bytes())))
        .unwrap_or_default();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    Reply::parse(&http_raw(port, request.as_bytes()))
}

/// A JSON-RPC 1.0 call of `method` with `params` (JSON text) on `/`.
pub fn rpc(port: u16, user_pass: &str, method: &str, params: &str) -> Reply {
    let body = format!(r#"{{"method":"{method}","params":{params},"id":1}}"#);
    post(port, "/", Some(user_pass), &body)
}

/// The cookie's `__cookie__:<secret>`.
pub fn cookie(datadir: &Path) -> String {
    read(&net(datadir).join(".cookie"))
}

/// A started regtest node with its RPC port.
pub struct RpcNode {
    pub node: Running,
    pub port: u16,
    pub datadir: PathBuf,
}

impl RpcNode {
    pub fn start(home: &Path, datadir: &Path, extra: &[&str]) -> RpcNode {
        RpcNode::start_on(Path::new(RDASHD), home, datadir, extra)
    }

    /// [`RpcNode::start`] for `binary`, rdashd or dashd.
    pub fn start_on(binary: &Path, home: &Path, datadir: &Path, extra: &[&str]) -> RpcNode {
        let (node, port) = start_on_free_port(datadir, free_port, |port| {
            let rpcport = format!("-rpcport={port}");
            let args = [&["-printtoconsole=0", rpcport.as_str()], extra].concat();
            regtest_on(binary, home, datadir, &args)
        });
        RpcNode {
            node,
            port,
            datadir: datadir.to_owned(),
        }
    }

    /// A call with the cookie.
    pub fn call(&self, method: &str, params: &str) -> Reply {
        rpc(self.port, &cookie(&self.datadir), method, params)
    }

    /// `stop`, then the exit.
    pub fn stop(self) -> Output {
        assert_eq!(self.call("stop", "[]").result(), r#""rdashd stopping""#);
        let out = self.node.wait();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        out
    }
}
