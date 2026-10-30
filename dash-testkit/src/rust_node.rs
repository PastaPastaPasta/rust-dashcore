//! `rdashd` as a test node: started in its own data directory, waited for, stopped cleanly.

use std::fmt;
use std::fs::File;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use dash_spv::test_utils::NodeImpl;

use crate::PATIENCE;
use crate::dir::create_private_dir;
use crate::mocktime::MockTime;
use crate::rpc::{RpcEndpoint, RpcError};

/// The debug.log line that ends rdashd's init (Core's `InitMessage(_("Done loading"))`).
const DONE_LOADING: &str = "init message: Done loading";
/// The debug.log line that ends Core's `Shutdown()`.
const SHUTDOWN_DONE: &str = "Shutdown: done";
const POLL: Duration = Duration::from_millis(100);
/// Options that daemonize: the node forks and its first process exits, so the harness would
/// watch, and stop, the wrong process.
const DAEMON_OPTIONS: &[&str] = &["daemon", "daemonwait"];
/// Options the harness owns: it runs regtest and reads `<datadir>/regtest/{debug.log,dashd.pid}`
/// (and dashd's `.cookie`). A config file could set any option, daemonizing included.
const HARNESS_OPTIONS: &[&str] = &[
    "datadir",
    "debuglogfile",
    "pid",
    "conf",
    "includeconf",
    "chain",
    "regtest",
    "testnet",
    "devnet",
    // debug.log is read from this run's start offset: shrinking it at init would move that.
    "shrinkdebugfile",
];
/// Settings the harness makes and reports ([`MixedNetwork::mocktime`](crate::MixedNetwork),
/// [`RustNode::p2p_addr`], [`RustNode::rpc`] with cookie auth): a later argument would make
/// those wrong.
const REPORTED_OPTIONS: &[&str] = &[
    "mocktime",
    "listen",
    "port",
    "bind",
    "connect",
    "server",
    "rpcport",
    "rpcbind",
    "rpcuser",
    "rpcpassword",
    "rpcauth",
];

/// How to start one rdashd.
///
/// Every node runs on regtest, in `datadir`, with `HOME` and the XDG directories inside it, so the
/// real default data directory and config are never read. Its stdout and stderr go to
/// `rdashd.stdout` and `rdashd.stderr` in `datadir` (appended across restarts), next to
/// `regtest/debug.log`. With `DASHD_TEST_LOG` set, stdout goes to the test's console instead, with
/// `-printtoconsole=1`.
///
/// P2P listening and the RPC server are off unless configured (`-listen=0 -server=0`), so nodes
/// never take the default regtest ports. `extra_args` must not daemonize (`-daemon`,
/// `-daemonwait`), set what the harness owns (the chain, `-datadir`, `-debuglogfile`, `-pid`,
/// `-shrinkdebugfile`, config files) or what it reports (`-mocktime`, the P2P and RPC
/// settings): [`RustNodeConfig::with_args`] panics on one, and [`RustNode::start`] refuses it.
/// Nor should a test write option-bearing `dash.conf` into the data directory.
#[derive(Clone, Debug)]
pub struct RustNodeConfig {
    pub binary: PathBuf,
    pub datadir: PathBuf,
    pub mocktime: Option<MockTime>,
    /// Serve P2P on this loopback port (`-listen=1 -port -bind`; rdashd: P2b).
    pub listen_port: Option<u16>,
    /// Connect only to these peers (`-connect`; rdashd: P2a-net).
    pub connect: Vec<SocketAddr>,
    /// Serve RPC on this loopback port (`-server -rpcport`; rdashd: RP0).
    pub rpc_port: Option<u16>,
    pub extra_args: Vec<String>,
}

impl RustNodeConfig {
    /// A node in `datadir`, running the rdashd at `RDASHD_PATH`. Panics if it is not set.
    pub fn new(datadir: impl Into<PathBuf>) -> Self {
        let binary = NodeImpl::Rdashd.binary().unwrap_or_else(|missing| panic!("{missing}"));
        Self::with_binary(binary, datadir)
    }

    /// A node in `datadir`, running `binary`.
    pub fn with_binary(binary: impl Into<PathBuf>, datadir: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            datadir: datadir.into(),
            mocktime: None,
            listen_port: None,
            connect: Vec::new(),
            rpc_port: None,
            extra_args: Vec::new(),
        }
    }

    pub fn with_mocktime(mut self, time: Option<MockTime>) -> Self {
        self.mocktime = time;
        self
    }

    pub fn with_listen(mut self, port: u16) -> Self {
        self.listen_port = Some(port);
        self
    }

    pub fn with_connect(mut self, peer: SocketAddr) -> Self {
        self.connect.push(peer);
        self
    }

    pub fn with_rpc(mut self, port: u16) -> Self {
        self.rpc_port = Some(port);
        self
    }

    /// Panics on an argument the harness does not allow (see [`RustNodeConfig`]).
    pub fn with_args<S: Into<String>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        let args: Vec<String> = args.into_iter().map(Into::into).collect();
        assert_args_allowed(&args);
        self.extra_args.extend(args);
        self
    }

    /// The command line, without the binary.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "-regtest".to_string(),
            format!("-datadir={}", self.datadir.display()),
            format!("-printtoconsole={}", u8::from(console_log())),
            // The command line overrides a `dash.conf` in the data directory, which could
            // daemonize the node. Both: `-daemonwait` forks on its own.
            "-daemon=0".to_string(),
            "-daemonwait=0".to_string(),
            // Core shrinks a debug.log over 11 MB at init (without `-debug`), which would move
            // this run's start in it.
            "-shrinkdebugfile=0".to_string(),
        ];
        args.extend(self.mocktime.map(MockTime::arg));
        match self.listen_port {
            Some(port) => args.extend([
                "-listen=1".to_string(),
                format!("-port={port}"),
                "-bind=127.0.0.1".to_string(),
                // As for dashd: no onion bind beside a `-bind`.
                "-listenonion=0".to_string(),
            ]),
            None => args.push("-listen=0".to_string()),
        }
        args.extend(self.connect.iter().map(|peer| format!("-connect={peer}")));
        match self.rpc_endpoint() {
            Some(endpoint) => args.extend(endpoint.args()),
            None => args.push("-server=0".to_string()),
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }

    fn rpc_endpoint(&self) -> Option<RpcEndpoint> {
        self.rpc_port.map(|port| RpcEndpoint::regtest(&self.datadir, port))
    }
}

/// Whether `arg` sets one of `options`, read as this platform's Core reads it.
fn sets_option(arg: &str, options: &[&str]) -> bool {
    option_name(arg, cfg!(windows)).is_some_and(|name| options.contains(&name.as_str()))
}

/// The option an argument sets, as Core's `ParseParameters` and `InterpretKey` read it: `--x` is
/// `-x` and `-nox` is `-x=0`; on Windows the name is lowercased and a leading `/` is a `-`.
/// `None` if it is not an option.
fn option_name(arg: &str, windows: bool) -> Option<String> {
    let key = arg.split_once('=').map_or(arg, |(key, _)| key);
    let mut key = key.to_string();
    if windows {
        key.make_ascii_lowercase();
        if key.starts_with('/') {
            key.replace_range(..1, "-");
        }
    }
    let name = key.strip_prefix("--").or_else(|| key.strip_prefix('-'))?;
    let name = name.strip_prefix("no").unwrap_or(name);
    (!name.is_empty()).then(|| name.to_string())
}

/// Refuses an extra argument (to rdashd or dashd) that daemonizes or sets what the harness owns.
fn check_args(args: &[String]) -> Result<(), StartError> {
    let not_allowed = |arg: &String| {
        let reason = if sets_option(arg, DAEMON_OPTIONS) {
            "a daemonized node outlives the harness, which would stop the wrong process"
        } else if sets_option(arg, HARNESS_OPTIONS) {
            "the harness runs regtest and reads the data directory's debug.log and pid file"
        } else if sets_option(arg, REPORTED_OPTIONS) {
            "the harness sets it and reports it (mock time, P2P and RPC addresses, RPC cookie auth); use the config"
        } else {
            return None;
        };
        Some(StartError::NotAllowed {
            arg: arg.clone(),
            reason,
        })
    };
    args.iter().find_map(not_allowed).map_or(Ok(()), Err)
}

/// Panics on an argument [`check_args`] refuses.
pub(crate) fn assert_args_allowed(args: &[String]) {
    if let Err(e) = check_args(args) {
        panic!("{e}");
    }
}

/// `DASHD_TEST_LOG`: node output on the test's console.
fn console_log() -> bool {
    std::env::var_os("DASHD_TEST_LOG").is_some()
}

/// Why a node did not start.
#[derive(Debug)]
pub enum StartError {
    Io(std::io::Error),
    /// The harness does not pass this argument; nothing was started.
    NotAllowed {
        arg: String,
        reason: &'static str,
    },
    /// rdashd refused `option`, which the task `owner` implements.
    Refused {
        option: String,
        owner: String,
        stderr: String,
    },
    /// It exited during init for another reason.
    Exited {
        status: ExitStatus,
        stderr: String,
    },
    /// The binary started, but its debug.log has no `rdashd version` line: it is another node
    /// (Dash Core, say). It was killed.
    NotRdashd {
        binary: PathBuf,
        version: Option<String>,
    },
    /// Init did not finish within [`PATIENCE`]; the node was killed.
    Timeout {
        log_tail: String,
        stderr: String,
    },
}

impl StartError {
    fn exited(status: ExitStatus, stderr: String) -> Self {
        match parse_refusal(&stderr) {
            Some((option, owner)) => StartError::Refused {
                option,
                owner,
                stderr,
            },
            None => StartError::Exited {
                status,
                stderr,
            },
        }
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Io(e) => write!(f, "rdashd start: {e}"),
            StartError::NotAllowed {
                arg,
                reason,
            } => write!(f, "`{arg}` is not allowed: {reason}"),
            StartError::Refused {
                option,
                owner,
                ..
            } => write!(f, "rdashd does not support {option} yet ({owner})"),
            StartError::Exited {
                status,
                stderr,
            } => write!(f, "rdashd exited during init ({status}): {stderr}"),
            StartError::NotRdashd {
                binary,
                version,
            } => write!(
                f,
                "{} is not rdashd (its version line: {}); check RDASHD_PATH",
                binary.display(),
                version.as_deref().unwrap_or("none")
            ),
            StartError::Timeout {
                log_tail,
                stderr,
            } => write!(
                f,
                "rdashd did not finish init within {PATIENCE:?}; log tail:\n{log_tail}\nstderr:\n{stderr}"
            ),
        }
    }
}

impl std::error::Error for StartError {}

/// rdashd's refusal of an option it does not support yet:
/// `Error: -<opt> is not supported by rdashd yet (<owner>).`
fn parse_refusal(stderr: &str) -> Option<(String, String)> {
    stderr.lines().find_map(|line| {
        let rest = line.strip_prefix("Error: ")?;
        let (option, rest) = rest.split_once(" is not supported by rdashd yet (")?;
        let owner = rest.strip_suffix(").")?;
        Some((option.to_string(), owner.to_string()))
    })
}

/// Why a stop was not clean.
#[derive(Debug)]
pub enum StopError {
    Io(std::io::Error),
    /// The node did not exit within [`PATIENCE`]; it was killed.
    Timeout,
}

impl fmt::Display for StopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopError::Io(e) => write!(f, "rdashd stop: {e}"),
            StopError::Timeout => write!(f, "rdashd did not exit within {PATIENCE:?}"),
        }
    }
}

impl std::error::Error for StopError {}

/// A stopped node's exit and this run's debug.log.
#[derive(Debug)]
pub struct Stopped {
    /// The process that was stopped: the one [`RustNode::start`] spawned.
    pub pid: u32,
    pub status: ExitStatus,
    pub log: String,
}

impl Stopped {
    /// Exit code 0, and Core's shutdown ran to its end.
    pub fn is_clean(&self) -> bool {
        self.status.success() && self.log.contains(SHUTDOWN_DONE)
    }
}

/// A running rdashd. Dropping it kills the process; [`RustNode::stop`] stops it the way Core
/// stops.
pub struct RustNode {
    config: RustNodeConfig,
    child: Option<Child>,
    /// Where this run starts in debug.log and in `rdashd.stderr`: a restart appends to both.
    log_start: u64,
    stderr_start: u64,
}

impl RustNode {
    /// Starts rdashd and waits until init is done: this process is running, wrote
    /// `Done loading` to debug.log, and is the one the pid file names. This run's log must
    /// hold rdashd's version line, or the binary is something else and the start fails.
    ///
    /// The data directory and `HOME` inside it are made owner-only, even if they exist.
    ///
    /// On Linux the node gets `SIGKILL` when the thread that started it exits (`PR_SET_PDEATHSIG`),
    /// so a harness that dies without unwinding leaves no node behind. Start nodes on a thread
    /// that outlives them: a test's own thread or a runtime worker, not `spawn_blocking` (nor a
    /// worker inside `block_in_place`, which then becomes a blocking-pool thread).
    pub async fn start(config: RustNodeConfig) -> Result<Self, StartError> {
        check_args(&config.extra_args)?;
        create_private_dir(&config.datadir).map_err(StartError::Io)?;
        let home = config.datadir.join("home");
        create_private_dir(&home).map_err(StartError::Io)?;
        // Appending: a second node refused by the lock must not clobber the first one's output.
        let open =
            |name: &str| File::options().create(true).append(true).open(config.datadir.join(name));
        let stdout = if console_log() {
            Stdio::inherit()
        } else {
            open("rdashd.stdout").map_err(StartError::Io)?.into()
        };
        let stderr = open("rdashd.stderr").map_err(StartError::Io)?;
        let stderr_start = stderr.metadata().map_err(StartError::Io)?.len();
        let log_start = file_len(&debug_log_path(&config.datadir));
        tracing::info!("starting {} {:?}", config.binary.display(), config.args());
        let mut command = Command::new(&config.binary);
        command
            .args(config.args())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            // rdashd's `test-hooks` feature reads it; a test environment must not trigger them.
            .env_remove("RDASHD_TEST_HOOK")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        #[cfg(target_os = "linux")]
        kill_with_parent(&mut command);
        let child = command.spawn().map_err(StartError::Io)?;
        let mut node = Self {
            config,
            child: Some(child),
            log_start,
            stderr_start,
        };

        let deadline = Instant::now() + PATIENCE;
        loop {
            // Another rdashd on the same data directory writes the same debug.log: the line is
            // this node's only once the pid file names it.
            let ready = node.log().contains(DONE_LOADING) && node.pid_file() == Some(node.pid());
            // Reap after reading: a node that logged `Done loading` and then exited did not start.
            let child = node.child.as_mut().expect("running");
            if let Some(status) = child.try_wait().map_err(StartError::Io)? {
                node.child = None;
                return Err(StartError::exited(status, node.stderr()));
            }
            if ready {
                let log = node.log();
                if log.contains(NodeImpl::Rdashd.version_line()) {
                    return Ok(node);
                }
                node.kill();
                return Err(StartError::NotRdashd {
                    binary: node.config.binary.clone(),
                    version: log.lines().find(|l| l.contains(" version ")).map(str::to_string),
                });
            }
            if Instant::now() > deadline {
                node.kill();
                let log = node.log();
                let lines: Vec<_> = log.lines().collect();
                return Err(StartError::Timeout {
                    log_tail: lines[lines.len().saturating_sub(20)..].join("\n"),
                    stderr: node.stderr(),
                });
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Stops the node as Core stops on SIGTERM (on other platforms: kills it) and waits for it.
    ///
    /// Cancel-safe: the node keeps its process until it is reaped, so dropping this future (a
    /// `timeout` around it, say) drops the node, which kills the process.
    pub async fn stop(mut self) -> Result<Stopped, StopError> {
        let pid = self.pid();
        #[cfg(unix)]
        {
            // SAFETY: signalling the child this node started and has not reaped.
            if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
                let err = std::io::Error::last_os_error();
                self.kill();
                return Err(StopError::Io(err));
            }
        }
        #[cfg(not(unix))]
        self.child.as_mut().expect("running").kill().map_err(StopError::Io)?;

        let deadline = Instant::now() + PATIENCE;
        loop {
            // An error returns with the child still held: dropping the node kills it.
            let child = self.child.as_mut().expect("running");
            if let Some(status) = child.try_wait().map_err(StopError::Io)? {
                self.child = None;
                return Ok(Stopped {
                    pid,
                    status,
                    log: self.log(),
                });
            }
            if Instant::now() > deadline {
                self.kill();
                return Err(StopError::Timeout);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().expect("running").id()
    }

    pub fn datadir(&self) -> &Path {
        &self.config.datadir
    }

    /// The pid in `regtest/dashd.pid`, if the file holds one.
    pub fn pid_file(&self) -> Option<u32> {
        std::fs::read_to_string(self.config.datadir.join("regtest").join("dashd.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    pub fn debug_log_path(&self) -> PathBuf {
        debug_log_path(&self.config.datadir)
    }

    /// This run's debug.log so far.
    pub fn log(&self) -> String {
        read_from(&self.debug_log_path(), self.log_start)
    }

    /// What this run wrote to stderr.
    pub fn stderr(&self) -> String {
        read_from(&self.config.datadir.join("rdashd.stderr"), self.stderr_start)
    }

    /// The P2P address it serves, if it listens.
    pub fn p2p_addr(&self) -> Option<SocketAddr> {
        self.config.listen_port.map(crate::loopback)
    }

    /// An RPC client, if it serves RPC (rdashd: RP0).
    pub fn rpc(&self) -> Result<dashcore_rpc::Client, RpcError> {
        self.config.rpc_endpoint().ok_or("this rdashd does not serve RPC (no rpc_port)")?.client()
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Has the kernel send `command`'s process `SIGKILL` when the spawning thread exits.
#[cfg(target_os = "linux")]
fn kill_with_parent(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: getpid is async-signal-safe.
    let parent = unsafe { libc::getpid() };
    // SAFETY: the closure runs in the forked child before exec and calls only prctl and getppid,
    // which are async-signal-safe.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The parent may have died before the prctl: then nothing will signal.
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

fn debug_log_path(datadir: &Path) -> PathBuf {
    datadir.join("regtest").join("debug.log")
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

/// The file from byte `start` on, lossily decoded; empty if it is missing.
fn read_from(path: &Path, start: u64) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    String::from_utf8_lossy(bytes.get(start as usize..).unwrap_or_default()).into_owned()
}

impl Drop for RustNode {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_read_as_core_reads_them() {
        for arg in ["-daemon", "-daemon=1", "--daemon", "-nodaemon", "-daemon=0", "-daemonwait=1"] {
            assert!(sets_option(arg, DAEMON_OPTIONS), "{arg}");
        }
        for arg in ["-debug=daemon", "-daemons", "daemon", "-notify=1"] {
            assert!(!sets_option(arg, DAEMON_OPTIONS), "{arg}");
        }
        for arg in ["-noregtest", "-regtest=0", "-chain=main", "-conf=x", "-pid=x"] {
            assert!(sets_option(arg, HARNESS_OPTIONS), "{arg}");
        }
        assert!(sets_option("-shrinkdebugfile=1", HARNESS_OPTIONS));
        for arg in ["-mocktime=1", "-port=1", "-rpcport=1", "-bind=x", "-connect=x", "-server=1"] {
            assert!(sets_option(arg, REPORTED_OPTIONS), "{arg}");
        }
        assert!(check_args(&["-mocktime=5".to_string()]).is_err());
    }

    #[test]
    fn option_names_follow_the_platform() {
        for (arg, windows, name) in [
            ("-daemon=1", false, Some("daemon")),
            ("--nodaemon", false, Some("daemon")),
            ("-Daemon=1", false, Some("Daemon")),
            ("/daemon=1", false, None),
            ("-Daemon=1", true, Some("daemon")),
            ("/DataDir=C:\\x", true, Some("datadir")),
            ("/noDaemonWait", true, Some("daemonwait")),
            ("/-pid=x", true, Some("pid")),
            // `/` becomes `-`, then Core strips two dashes: not an option name.
            ("/--pid=x", true, Some("-pid")),
            ("daemon", true, None),
            ("--", false, None),
        ] {
            assert_eq!(option_name(arg, windows).as_deref(), name, "{arg} windows={windows}");
        }
    }

    #[test]
    #[should_panic(expected = "`-daemon=1` is not allowed")]
    fn a_daemon_argument_is_rejected() {
        assert_args_allowed(&["-debug=net".to_string(), "-daemon=1".to_string()]);
    }

    #[test]
    fn a_refusal_names_the_option_and_its_owner() {
        let stderr = "Error: -port is not supported by rdashd yet (P2b).\n";
        assert_eq!(parse_refusal(stderr), Some(("-port".to_string(), "P2b".to_string())));
        assert_eq!(parse_refusal("Error: Cannot obtain a lock on data directory /x.\n"), None);
    }

    #[test]
    fn p2p_and_rpc_are_off_unless_configured() {
        let config = RustNodeConfig {
            binary: PathBuf::from("rdashd"),
            datadir: PathBuf::from("/d"),
            mocktime: None,
            listen_port: None,
            connect: Vec::new(),
            rpc_port: None,
            extra_args: Vec::new(),
        };
        let args = config.args();
        assert_eq!(
            args[3..],
            ["-daemon=0", "-daemonwait=0", "-shrinkdebugfile=0", "-listen=0", "-server=0"]
        );
    }

    #[test]
    fn the_command_line_follows_the_config() {
        let config = RustNodeConfig {
            binary: PathBuf::from("rdashd"),
            datadir: PathBuf::from("/d"),
            mocktime: Some(MockTime::at(7)),
            listen_port: Some(1),
            connect: vec![SocketAddr::from(([127, 0, 0, 1], 2))],
            rpc_port: Some(3),
            extra_args: vec!["-x".to_string()],
        };
        let args = config.args();
        let console = format!("-printtoconsole={}", u8::from(console_log()));
        assert_eq!(
            args,
            [
                "-regtest",
                "-datadir=/d",
                &console,
                "-daemon=0",
                "-daemonwait=0",
                "-shrinkdebugfile=0",
                "-mocktime=7",
                "-listen=1",
                "-port=1",
                "-bind=127.0.0.1",
                "-listenonion=0",
                "-connect=127.0.0.1:2",
                "-server=1",
                "-rpcport=3",
                "-rpcbind=127.0.0.1",
                "-rpcallowip=127.0.0.1",
                "-x",
            ]
        );
    }
}
