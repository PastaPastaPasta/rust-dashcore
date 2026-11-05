//! Dash Core node test infrastructure for integration testing.
//!
//! This provides utilities for managing a dashd instance and loading test wallet data.

use dashcore::{Address, Amount, BlockHash, Transaction, Txid};
use dashcore_rpc::json as rpc_json;
use dashcore_rpc::jsonrpc::{self, http::simple_http::SimpleHttpTransport};
use dashcore_rpc::{Auth, Client, RpcApi};
use serde::Deserialize;
use serde_json::{Map, Value};
use socket2::{Domain, Socket, Type};
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tokio::process::Child;
use tokio::time::{sleep, timeout};

use super::fs_helpers::{copy_dir, retain_test_dir_now};

/// Base of the port range used by tests. Sits above the standard Dash regtest
/// ports (19898/19899) and below the Linux ephemeral range (32768+).
const PORT_RANGE_BASE: u16 = 20000;
/// Ports handed out per test process before wrapping into a neighbour's lane.
///
/// Sized for the heaviest target: `dashd_masternode` runs 3 controller-only
/// tests (2 ports each) plus 7 full-network ones (controller + 4 masternodes at
/// 2 ports apiece), so ~76 ports in one process.
const PORT_LANE_WIDTH: u16 = 100;
/// Number of lanes carved out of the range
const PORT_LANE_COUNT: u16 = 120;

const _: () = assert!(
    PORT_RANGE_BASE as u32 + PORT_LANE_WIDTH as u32 * PORT_LANE_COUNT as u32 <= 32768,
    "range must stay below the Linux ephemeral range"
);

const MAX_PORT_ATTEMPTS: usize = 100;

/// Atomic counter for unique port allocation across parallel tests.
/// Seeded lazily from the PID and a random offset.
static NEXT_PORT: AtomicU16 = AtomicU16::new(0);

/// Allocate a unique, available TCP port for test use.
///
/// One counter per test process, shared by every harness built on these helpers, so nodes
/// started by different harnesses in the same test binary never get the same port.
pub fn find_available_port() -> u16 {
    // The PID picks a lane, and a random offset within it moves the start, so two processes
    // whose PIDs share a lane do not walk the same ports.
    let lane = std::process::id() % u32::from(PORT_LANE_COUNT) * u32::from(PORT_LANE_WIDTH);
    let start = lane + rand::random::<u32>() % u32::from(PORT_LANE_WIDTH);
    let _ = NEXT_PORT.compare_exchange(
        0,
        PORT_RANGE_BASE + start as u16,
        Ordering::Relaxed,
        Ordering::Relaxed,
    );

    for _ in 0..MAX_PORT_ATTEMPTS {
        let raw = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        // Keep the counter inside the range no matter how many are allocated.
        let port = PORT_RANGE_BASE
            + raw.wrapping_sub(PORT_RANGE_BASE) % (PORT_LANE_WIDTH * PORT_LANE_COUNT);
        if port_is_free(port) {
            return port;
        }
    }
    panic!("failed to find an available port after {} attempts", MAX_PORT_ATTEMPTS);
}

/// Whether `port` is free on 127.0.0.1, where the nodes bind: a probe that binds and never
/// listens (BL-437). A listening probe could be inherited by a child spawned on another thread
/// at that moment and, held until the child's `exec`, keep the node from binding the port. A
/// bound-only socket with `SO_REUSEADDR` blocks no binder that sets it too (dashd's, rdashd's
/// and std's `TcpListener` do): on Linux such sockets conflict only when one of them listens.
/// It still meets a listener on 127.0.0.1 or a wildcard address. `SO_REUSEADDR` is set on Unix
/// only: on Windows it lets a bind take over a listener.
fn port_is_free(port: u16) -> bool {
    let probe = || -> std::io::Result<()> {
        let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
        #[cfg(unix)]
        socket.set_reuse_address(true)?;
        socket.bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)).into())
    };
    probe().is_ok()
}

/// Which node implementation the dashd harness runs: the `DASHD_IMPL` switch.
///
/// Unset or `dashd` selects Dash Core from `DASHD_PATH`. `rdashd` selects the Rust node from
/// `RDASHD_PATH`, which must be set: the harness never guesses a binary, so it cannot test a
/// stale build. The harness passes either one the same arguments, so whatever rdashd does not
/// support yet is refused by rdashd itself, naming the task that adds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeImpl {
    Dashd,
    Rdashd,
}

impl NodeImpl {
    /// The implementation `DASHD_IMPL` selects. Panics on an unknown value.
    pub fn from_env() -> Self {
        let value = std::env::var_os("DASHD_IMPL");
        Self::parse(value.as_ref().map(|v| v.to_string_lossy()).as_deref())
            .unwrap_or_else(|| panic!("DASHD_IMPL must be `dashd` or `rdashd`, got {value:?}"))
    }

    /// The implementation a `DASHD_IMPL` value selects: unset, empty or `dashd`, or `rdashd`.
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value {
            None | Some("") | Some("dashd") => Some(NodeImpl::Dashd),
            Some("rdashd") => Some(NodeImpl::Rdashd),
            Some(_) => None,
        }
    }

    /// The binary for this implementation, or how to provide one.
    pub fn binary(self) -> Result<PathBuf, &'static str> {
        let (var, missing) = match self {
            NodeImpl::Dashd => ("DASHD_PATH", DASHD_MISSING),
            NodeImpl::Rdashd => ("RDASHD_PATH", RDASHD_MISSING),
        };
        std::env::var_os(var).map(PathBuf::from).ok_or(missing)
    }

    /// The start of the version line this implementation writes first to debug.log (Core's
    /// `LogPackageVersion`), which tells the two apart: `<PACKAGE_NAME> version …`.
    pub fn version_line(self) -> &'static str {
        match self {
            NodeImpl::Dashd => "Dash Core version ",
            NodeImpl::Rdashd => "rdashd version ",
        }
    }
}

const DASHD_MISSING: &str = "DASHD_PATH is not set: run `eval $(python3 contrib/setup-dashd.py)`, \
     or set SKIP_DASHD_TESTS=1 to skip the dashd tests. In CI, the setup-dashd step in \
     build-and-test.yml sets it.";

/// There is deliberately no fallback to `target/<profile>/rdashd`: it may be a stale build.
const RDASHD_MISSING: &str = "RDASHD_PATH is not set. Build rdashd with `cargo build -p rdashd` \
     and set RDASHD_PATH=$PWD/target/debug/rdashd. The harness never guesses the binary, so it \
     cannot test a stale build.";

/// Read/write timeout of the harness RPC clients. Core's functional tests allow 60 s; a heavily
/// loaded host needs more. Past it, jsonrpc's transport sends the same request once more, so a
/// call that is not idempotent can run twice.
const RPC_TIMEOUT: Duration = Duration::from_secs(120);

/// Core's `RPC_WALLET_NOT_FOUND`: no wallet by that name (or a wallet directory without a
/// database).
const RPC_WALLET_NOT_FOUND: i32 = -18;
/// Core's `RPC_WALLET_ALREADY_LOADED`.
const RPC_WALLET_ALREADY_LOADED: i32 = -35;

/// The JSON-RPC error code of an error dashd returned, `None` for transport or decode errors.
fn rpc_error_code(error: &dashcore_rpc::Error) -> Option<i32> {
    match error {
        dashcore_rpc::Error::JsonRpc(jsonrpc::Error::Rpc(e)) => Some(e.code),
        _ => None,
    }
}

/// What `lsof` reports holding TCP `port`, for a failed bind. Best effort: empty when `lsof` is
/// missing or fails.
fn port_holders(port: u16) -> String {
    std::process::Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{}", port)])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

/// Selects which pre-built regtest blockchain to use for integration tests.
#[derive(Debug, Clone, Copy)]
pub enum TestChain {
    /// Full 40,000-block regtest chain (wallet integration tests).
    Full,
    /// Minimal 200-block regtest chain (faster tests).
    Minimal,
}

impl TestChain {
    pub(crate) fn variant_dir(self) -> &'static str {
        match self {
            TestChain::Full => "regtest-40000",
            TestChain::Minimal => "regtest-200",
        }
    }
}

/// Configuration for Dash Core node.
pub struct DashCoreConfig {
    /// Path to dashd binary
    pub dashd_path: PathBuf,
    /// Path to existing datadir with blockchain data
    pub datadir: PathBuf,
    /// Wallet name to load on startup
    pub wallet: String,
    /// P2P port for the node
    pub p2p_port: u16,
    /// RPC port for the node
    pub rpc_port: u16,
    pub extra_args: Vec<String>,
    /// How long `start` waits for P2P and RPC to answer.
    pub start_timeout: Duration,
    /// Which implementation `dashd_path` is: `start` checks the binary's debug.log says so.
    pub node_impl: NodeImpl,
}

/// The default [`DashCoreConfig::start_timeout`]. Generous: on a loaded box a regtest node has
/// taken minutes to start, and a node that exits during start fails at once.
pub const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(300);

impl DashCoreConfig {
    /// Create a config for the given test chain variant under `DASHD_TEST_DATA`.
    ///
    /// `DASHD_TEST_DATA` points to the root directory containing all variants
    /// (e.g. `regtest-40000`, `regtest-200`). Returns `None` if the variant
    /// directory doesn't exist. Panics if env vars are missing.
    pub fn from_env(chain: TestChain) -> Option<Self> {
        let node_impl = NodeImpl::from_env();
        let dashd_path = node_impl.binary().unwrap_or_else(|missing| panic!("{missing}"));

        assert!(dashd_path.exists(), "the node binary does not exist: {}", dashd_path.display());

        let base_datadir = std::env::var("DASHD_TEST_DATA").ok().map(PathBuf::from).expect(
            "DASHD_TEST_DATA is not set: run `eval $(python3 contrib/setup-dashd.py)`, \
             or set SKIP_DASHD_TESTS=1 to skip the dashd tests",
        );
        let datadir = base_datadir.join(chain.variant_dir());

        if !datadir.exists() {
            return None;
        }

        Some(Self {
            dashd_path,
            datadir,
            wallet: "default".to_string(),
            p2p_port: find_available_port(),
            rpc_port: find_available_port(),
            extra_args: Vec::new(),
            start_timeout: DEFAULT_START_TIMEOUT,
            node_impl,
        })
    }

    /// A Dash Core node with no wallet and no extra arguments: a fresh regtest chain when
    /// `datadir` is empty.
    pub fn new(dashd_path: PathBuf, datadir: PathBuf, p2p_port: u16, rpc_port: u16) -> Self {
        Self {
            dashd_path,
            datadir,
            wallet: String::new(),
            p2p_port,
            rpc_port,
            extra_args: Vec::new(),
            start_timeout: DEFAULT_START_TIMEOUT,
            node_impl: NodeImpl::Dashd,
        }
    }

    pub fn with_start_timeout(mut self, start_timeout: Duration) -> Self {
        self.start_timeout = start_timeout;
        self
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args.extend(args);
        self
    }
}

/// How often startup checks whether dashd is ready or has exited.
const READY_CHECK_INTERVAL: Duration = Duration::from_millis(500);
/// How long dashd gets to shut down after RPC `stop` before it is killed.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// Launches a node's first start may take when dashd can't bind one of its ports.
const MAX_START_ATTEMPTS: usize = 3;
/// Lines of debug.log quoted when dashd fails to start.
const LOG_TAIL_LINES: usize = 40;

/// A node port that dashd's startup log says it could not bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindFailure {
    P2p,
    Rpc,
}

impl BindFailure {
    /// Finds dashd's error for a failed bind of the P2P port `p2p_port` or of the RPC port
    /// `rpc_port` in the log of one launch. Dash Core and rdashd log the same text; Core lines
    /// are v23.1.0's, the release setup-dashd.py installs.
    ///
    /// dashd also binds its default onion target port (`src/init.cpp:2613-2617`) and exits only
    /// when every bind fails, so a failed P2P bind can leave it running with no P2P listener.
    /// The P2P error is therefore matched by port, and other ports' errors are ignored.
    fn from_log(log: &str, p2p_port: u16, rpc_port: u16) -> Option<Self> {
        const LISTEN_FAILED: &str =
            "Listening for incoming connections failed (listen returned error";
        const BOUND: &str = "Bound to 127.0.0.1:";
        let p2p = p2p_port.to_string();
        // Core: net.cpp:3820, bound beside another SO_REUSEADDR socket, which listened first. The
        // line names no address: it is the P2P port's when the last bind before it is (each bind
        // logs `Bound to` before it listens, net.cpp:3815; v23.1.0's onion bind comes after).
        let listen_failed = || {
            log.match_indices(LISTEN_FAILED).any(|(at, _)| {
                log[..at].rfind(BOUND).is_some_and(|bound| {
                    log[bound + BOUND.len()..].split(|c: char| !c.is_ascii_digit()).next()
                        == Some(p2p.as_str())
                })
            })
        };
        // Core: net.cpp:3809 (rdashd: dashd-net binds.rs)
        if log.contains(&format!("Unable to bind to 127.0.0.1:{} ", p2p_port)) || listen_failed() {
            Some(Self::P2p)
        } else if log.contains("Unable to bind any endpoint for RPC server")
            // Core: httpserver.cpp:364 (rdashd: dashd-rpc http/mod.rs)
            || log.contains(&format!("Binding RPC on address 127.0.0.1 port {} failed.", rpc_port))
        {
            Some(Self::Rpc)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::P2p => "P2P",
            Self::Rpc => "RPC",
        }
    }
}

/// Why one launch of dashd did not become ready.
#[derive(Debug)]
enum StartFailure {
    Exited(ExitStatus),
    TimedOut,
    /// dashd answered RPC, but its log shows it could not bind this port.
    BindFailed(BindFailure),
}

/// Test infrastructure for managing a Dash Core node.
pub struct DashCoreNode {
    config: DashCoreConfig,
    process: Option<Child>,
    /// Set by the first [`DashCoreNode::start`]; later starts keep their ports.
    started: bool,
}

impl DashCoreNode {
    /// Create a new Dash Core node with custom configuration
    pub fn with_config(config: DashCoreConfig) -> Self {
        Self {
            config,
            process: None,
            started: false,
        }
    }

    /// A node, not yet started, on a copy of `chain`'s datadir made in `datadir`, with ports
    /// from `find_available_port` and no wallet.
    ///
    /// Returns `None` if the chain variant is not under `DASHD_TEST_DATA`. Panics if the dashd
    /// environment variables are missing or the copy fails.
    pub fn on_chain_copy(chain: TestChain, datadir: &Path) -> Option<Self> {
        let mut config = DashCoreConfig::from_env(chain)?;
        copy_dir(&config.datadir, datadir).expect("failed to copy datadir");
        config.datadir = datadir.to_path_buf();
        config.wallet = String::new();
        Some(Self::with_config(config))
    }

    /// Has dashd load the datadir's wallet `name` when it starts, and makes it the wallet the
    /// node's RPC helpers use.
    pub(crate) fn with_wallet(mut self, name: &str) -> Self {
        self.config.wallet = name.to_string();
        self
    }

    /// Appends `dashd` arguments after the defaults, as [`DashCoreConfig::with_extra_args`].
    pub(crate) fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.config.extra_args.extend(args);
        self
    }

    /// Start the Dash Core node.
    ///
    /// `find_available_port` checks that a port is free and then releases it, and dashd binds
    /// its ports only late in its startup, so another socket can take one in between. When
    /// dashd's log shows that the node's first start could not bind its P2P or RPC port, whether
    /// dashd exited or kept running without a P2P listener, it is stopped and launched again on a
    /// fresh pair of ports, up to `MAX_START_ATTEMPTS` launches in all. Any other
    /// failure panics at once with the end of dashd's `debug.log`, after copying the datadir to
    /// `DASHD_TEST_RETAIN_DIR` when that is set.
    ///
    /// On Windows, dashd's `SO_REUSEADDR` lets it bind its P2P port over another socket, so a
    /// taken P2P port gives no bind error there and is not retried.
    pub async fn start(&mut self) -> SocketAddr {
        let max_attempts = if self.started {
            1
        } else {
            MAX_START_ATTEMPTS
        };
        self.started = true;
        fs::create_dir_all(&self.config.datadir).expect("failed to create datadir");

        let mut attempt = 1;
        loop {
            let log_start = self.debug_log_len();
            let outcome = self.launch().await;
            let log = self.debug_log_since(log_start);
            let Some(bind_failure) =
                BindFailure::from_log(&log, self.config.p2p_port, self.config.rpc_port)
            else {
                match outcome {
                    Ok(addr) => {
                        self.check_node_impl(&log);
                        return addr;
                    }
                    Err(failure) => {
                        // A dashd that timed out still runs; stop it so the retained datadir is
                        // complete. The tail stays the one from before the stop, so shutdown
                        // lines don't push out what led to the failure.
                        self.stop_launch().await;
                        self.fail_start(failure, attempt, &log)
                    }
                }
            };
            // A failed P2P bind can leave dashd running without a P2P listener.
            self.stop_launch().await;
            let port = match bind_failure {
                BindFailure::P2p => self.config.p2p_port,
                BindFailure::Rpc => self.config.rpc_port,
            };
            // Written to the stderr handle itself: libtest captures `eprintln!`, and this should
            // reach the CI log even when a retry saves the test.
            let _ = writeln!(
                std::io::stderr(),
                "dashd could not bind its {} port {} (launch {} of {}). lsof -iTCP:{}:\n{}",
                bind_failure.name(),
                port,
                attempt,
                max_attempts,
                port,
                port_holders(port)
            );
            if attempt == max_attempts {
                let failure = outcome.err().unwrap_or(StartFailure::BindFailed(bind_failure));
                self.fail_start(failure, attempt, &log);
            }
            attempt += 1;
            self.config.p2p_port = find_available_port();
            self.config.rpc_port = find_available_port();
        }
    }

    /// Stops the launched dashd, if it still runs, and waits for it to exit: through RPC `stop`
    /// when dashd answers it within [`STOP_TIMEOUT`], otherwise by killing it.
    async fn stop_launch(&mut self) {
        let Some(mut process) = self.process.take() else {
            return;
        };
        if self.try_rpc_call("stop", &[]).is_some()
            && matches!(timeout(STOP_TIMEOUT, process.wait()).await, Ok(Ok(_)))
        {
            return;
        }
        if let Err(e) = process.kill().await {
            tracing::warn!("Failed to kill dashd process: {}", e);
        }
    }

    /// Launches dashd once and waits until it is ready.
    async fn launch(&mut self) -> Result<SocketAddr, StartFailure> {
        tracing::info!("Starting dashd...");
        tracing::info!("  Binary: {:?}", self.config.dashd_path);
        tracing::info!("  Datadir: {:?}", self.config.datadir);
        tracing::info!("  P2P port: {}", self.config.p2p_port);
        tracing::info!("  RPC port: {}", self.config.rpc_port);

        let mut args_vec = vec![
            "-regtest".to_string(),
            format!("-datadir={}", self.config.datadir.display()),
            format!("-port={}", self.config.p2p_port),
            format!("-rpcport={}", self.config.rpc_port),
            "-server=1".to_string(),
            // Both: `-daemonwait` in a datadir's dash.conf forks on its own. (Dash Core releases
            // before Bitcoin #21007's backport lack `-daemonwait` and would refuse this.)
            "-daemon=0".to_string(),
            "-daemonwait=0".to_string(),
            "-fallbackfee=0.00001".to_string(),
            "-rpcbind=127.0.0.1".to_string(),
            "-rpcallowip=127.0.0.1".to_string(),
            "-bind=127.0.0.1".to_string(),
            "-listen=1".to_string(),
            // Core v24 refuses `-bind` without a dedicated onion bind unless the service is off.
            "-listenonion=0".to_string(),
            "-txindex=0".to_string(),
            "-addressindex=0".to_string(),
            "-spentindex=0".to_string(),
            "-timestampindex=0".to_string(),
            "-blockfilterindex=1".to_string(),
            "-peerblockfilters=1".to_string(),
            "-peerbloomfilters=1".to_string(),
            "-whitelist=127.0.0.1".to_string(),
            "-debug=all".to_string(),
            // A copied debug.log over 11 MB must not shrink below `log_start`.
            "-shrinkdebugfile=0".to_string(),
        ];
        if !self.config.wallet.is_empty() {
            args_vec.push(format!("-wallet={}", self.config.wallet));
        }
        args_vec.extend(self.config.extra_args.iter().cloned());

        let mut cmd = tokio::process::Command::new(&self.config.dashd_path);
        cmd.args(&args_vec)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());

        let child = cmd.spawn().expect("failed to spawn dashd process");

        self.process = Some(child);

        tracing::info!("Waiting for dashd to be ready...");
        self.wait_for_ready().await?;

        let addr = SocketAddr::from(([127, 0, 0, 1], self.config.p2p_port));
        tracing::info!("dashd started and ready at {}", addr);

        Ok(addr)
    }

    /// Waits for the P2P port to accept connections and RPC to answer, and notices within one
    /// check interval if dashd exits first.
    async fn wait_for_ready(&mut self) -> Result<(), StartFailure> {
        let p2p_addr = SocketAddr::from(([127, 0, 0, 1], self.config.p2p_port));
        let result = timeout(self.config.start_timeout, async {
            let mut p2p_open = false;
            loop {
                if let Some(status) = self.exit_status() {
                    return Err(StartFailure::Exited(status));
                }
                if !p2p_open {
                    p2p_open = tokio::net::TcpStream::connect(p2p_addr).await.is_ok();
                }
                // Wait for RPC to be fully responsive (not just "warming up")
                if p2p_open && self.rpc_ready() {
                    return Ok(());
                }
                sleep(READY_CHECK_INTERVAL).await;
            }
        })
        .await;

        result.unwrap_or(Err(StartFailure::TimedOut))
    }

    /// Returns dashd's exit status once it has exited, and forgets the process.
    fn exit_status(&mut self) -> Option<ExitStatus> {
        let status = self.process.as_mut()?.try_wait().ok().flatten()?;
        self.process = None;
        Some(status)
    }

    fn rpc_ready(&self) -> bool {
        let cookie_path = self.config.datadir.join("regtest/.cookie");
        if !cookie_path.exists() {
            return false;
        }
        let url = format!("http://127.0.0.1:{}", self.config.rpc_port);
        let Ok(client) = Client::new(&url, Auth::CookieFile(cookie_path)) else {
            return false;
        };
        match client.get_blockchain_info() {
            Ok(_) => true,
            Err(e) => {
                tracing::debug!("RPC not ready yet: {}", e);
                false
            }
        }
    }

    fn debug_log_path(&self) -> PathBuf {
        self.config.datadir.join("regtest/debug.log")
    }

    fn debug_log_len(&self) -> u64 {
        fs::metadata(self.debug_log_path()).map(|m| m.len()).unwrap_or(0)
    }

    /// The part of `debug.log` written after it was `start` bytes long.
    fn debug_log_since(&self, start: u64) -> String {
        let bytes = fs::read(self.debug_log_path()).unwrap_or_default();
        let start = usize::try_from(start).unwrap_or(usize::MAX).min(bytes.len());
        String::from_utf8_lossy(&bytes[start..]).into_owned()
    }

    /// Checks that `log`, what a ready launch wrote to debug.log, comes from the implementation
    /// `DASHD_IMPL` selects: a wrong binary fails the test instead of testing the other node.
    fn check_node_impl(&self, log: &str) {
        let version_line = self.config.node_impl.version_line();
        assert!(
            !log.is_empty(),
            "{} wrote nothing to {}",
            self.config.dashd_path.display(),
            self.debug_log_path().display()
        );
        assert!(
            log.contains(version_line),
            "{} is not {:?}: its debug.log has no `{version_line}` line. Check DASHD_PATH and \
             RDASHD_PATH.",
            self.config.dashd_path.display(),
            self.config.node_impl,
        );
    }

    fn fail_start(&self, failure: StartFailure, attempt: usize, log: &str) -> ! {
        retain_test_dir_now(&self.config.datadir, &format!("dashd-{}", self.config.p2p_port));
        let what = match failure {
            StartFailure::Exited(status) => format!("dashd exited during startup with {}", status),
            StartFailure::TimedOut => {
                format!("dashd failed to start within {:?}", self.config.start_timeout)
            }
            StartFailure::BindFailed(port) => {
                format!("dashd could not bind its {} port", port.name())
            }
        };
        let lines: Vec<&str> = log.lines().collect();
        let tail = match lines.len() {
            0 => "(nothing)".to_string(),
            len => lines[len.saturating_sub(LOG_TAIL_LINES)..].join("\n"),
        };
        panic!(
            "{} (launch {}, P2P port {}, RPC port {}, {}). debug.log from this launch ends \
             with:\n{}",
            what,
            attempt,
            self.config.p2p_port,
            self.config.rpc_port,
            self.config.dashd_path.display(),
            tail
        );
    }

    /// Get block count via RPC.
    pub fn get_block_count(&self) -> u32 {
        let client = self.rpc_client();
        client.get_block_count().expect("failed to get block count")
    }

    /// Get an RPC client targeting the primary wallet.
    fn rpc_client(&self) -> Client {
        self.rpc_client_for_wallet(&self.config.wallet)
    }

    /// Get an RPC client targeting a specific wallet.
    pub(crate) fn rpc_client_for_wallet(&self, wallet_name: &str) -> Client {
        let url = format!("http://127.0.0.1:{}/wallet/{}", self.config.rpc_port, wallet_name);
        let cookie_path = self.config.datadir.join("regtest/.cookie");
        assert!(
            cookie_path.exists(),
            "RPC cookie file not found at {}. Is dashd running with this datadir?",
            cookie_path.display()
        );
        let cookie = fs::read_to_string(&cookie_path).expect("failed to read rpc cookie");
        // jsonrpc's 15 s default is shorter than a wallet load can take under load, and on a
        // read timeout its transport silently sends the same request again.
        let transport = SimpleHttpTransport::builder()
            .url(&url)
            .expect("invalid rpc url")
            .cookie_auth(cookie.trim())
            .timeout(RPC_TIMEOUT)
            .build();
        Client::from_jsonrpc(jsonrpc::Client::with_transport(transport))
    }

    /// Load a wallet by name, creating it only if dashd reports it doesn't exist.
    pub fn ensure_wallet(&self, wallet_name: &str) {
        let client = self.rpc_client();
        match client.load_wallet(wallet_name) {
            Ok(_) => tracing::info!("Loaded wallet: {}", wallet_name),
            Err(e) if rpc_error_code(&e) == Some(RPC_WALLET_ALREADY_LOADED) => {
                tracing::info!("Wallet already loaded: {}", wallet_name)
            }
            Err(e) if rpc_error_code(&e) == Some(RPC_WALLET_NOT_FOUND) => {
                client
                    .create_wallet(wallet_name, None, None, None, None)
                    .unwrap_or_else(|e| panic!("failed to create wallet '{}': {}", wallet_name, e));
                tracing::info!("Created wallet: {}", wallet_name);
            }
            Err(e) => panic!("failed to load wallet '{}': {}", wallet_name, e),
        }
    }

    pub fn get_new_address(&self) -> Address {
        self.get_new_address_from_wallet(&self.config.wallet)
    }

    /// Get a new address from a specific dashd wallet.
    pub fn get_new_address_from_wallet(&self, wallet_name: &str) -> Address {
        let client = self.rpc_client_for_wallet(wallet_name);
        let address = client.get_new_address(None).expect("failed to get new address");
        address.assume_checked()
    }

    /// Check if the connected dashd supports `generatetoaddress` (RPC miner).
    ///
    /// Some builds (e.g. Windows release binaries) ship without the RPC miner compiled in.
    /// Callers skip their tests without one; for rdashd that would pass the suites while testing
    /// nothing, so a missing miner panics there.
    pub fn supports_mining(&self) -> bool {
        let client = self.rpc_client();
        let addr = Address::dummy(dashcore::Network::Regtest, 0);
        match client.generate_to_address(0, &addr) {
            Ok(_) => true,
            Err(dashcore_rpc::Error::JsonRpc(dashcore_rpc::jsonrpc::Error::Rpc(ref e)))
                if e.message.contains("not available") =>
            {
                assert_eq!(
                    self.config.node_impl,
                    NodeImpl::Dashd,
                    "rdashd has no RPC miner ({}); the tests that need one must not be skipped",
                    e.message
                );
                false
            }
            // Any other error (auth, network) still counts as "available" —
            // a real generate call will surface the actual error.
            Err(_) => true,
        }
    }

    /// Generate blocks to the given address.
    pub fn generate_blocks(&self, count: u64, address: &Address) -> Vec<BlockHash> {
        let client = self.rpc_client();
        let hashes = client.generate_to_address(count, address).expect("failed to generate blocks");
        tracing::info!("Generated {} blocks to {}", count, address);
        hashes
    }

    /// Send DASH to an address from the primary wallet.
    pub fn send_to_address(&self, address: &Address, amount: Amount) -> Txid {
        let client = self.rpc_client();
        let txid = client
            .send_to_address(address, amount, None, None, None, None, None, None, None, None)
            .expect("failed to send to address");
        tracing::info!("Sent {} to {}, txid: {}", amount, address, txid);
        txid
    }

    /// Send DASH to many addresses in a single transaction from the primary
    /// wallet, so one transaction carries one output per `(address, amount)`
    /// pair.
    pub fn send_many(&self, payments: &[(Address, Amount)]) -> Txid {
        let client = self.rpc_client();
        let amounts: Map<String, Value> = payments
            .iter()
            .map(|(address, amount)| (address.to_string(), serde_json::json!(amount.to_dash())))
            .collect();
        let txid: Txid = client
            .call("sendmany", &[serde_json::json!(""), Value::Object(amounts)])
            .expect("failed to sendmany");
        tracing::info!("Sent {} outputs in one transaction, txid: {}", payments.len(), txid);
        txid
    }

    /// Send DASH to an address from a specific wallet.
    pub fn send_to_address_from_wallet(
        &self,
        wallet_name: &str,
        address: &Address,
        amount: Amount,
    ) -> Txid {
        let client = self.rpc_client_for_wallet(wallet_name);
        let txid = client
            .send_to_address(address, amount, None, None, None, None, None, None, None, None)
            .expect("failed to send to address");
        tracing::info!("Sent {} to {} (wallet: {}), txid: {}", amount, address, wallet_name, txid);
        txid
    }

    /// List unspent outputs for a specific wallet.
    pub fn list_unspent_from_wallet(
        &self,
        wallet_name: &str,
    ) -> Vec<rpc_json::ListUnspentResultEntry> {
        let client = self.rpc_client_for_wallet(wallet_name);
        client.list_unspent(None, None, None, None, None).expect("failed to list unspent")
    }

    /// Create, sign, and broadcast a raw transaction spending a single UTXO.
    /// Sends the input amount minus fee to the destination address.
    pub fn send_raw_from_wallet(
        &self,
        wallet_name: &str,
        input_txid: Txid,
        input_vout: u32,
        input_amount: Amount,
        destination: &Address,
        fee: Amount,
    ) -> Txid {
        let client = self.rpc_client_for_wallet(wallet_name);

        let inputs = vec![rpc_json::CreateRawTransactionInput {
            txid: input_txid,
            vout: input_vout,
            sequence: None,
        }];
        let send_amount = input_amount.checked_sub(fee).expect("fee exceeds input amount");
        let mut outputs = HashMap::new();
        outputs.insert(destination.to_string(), send_amount);

        let raw_tx: Transaction = client
            .create_raw_transaction(&inputs, &outputs, None)
            .expect("failed to create raw tx");

        let signed = client
            .sign_raw_transaction_with_wallet(&raw_tx, None, None)
            .expect("failed to sign raw tx");
        assert!(signed.complete, "raw transaction signing incomplete");

        let txid = client
            .send_raw_transaction(&signed.transaction().expect("invalid signed tx"))
            .expect("failed to send raw tx");
        tracing::info!(
            "Sent raw tx from wallet '{}': {} -> {}, txid: {}",
            wallet_name,
            input_amount,
            destination,
            txid
        );
        txid
    }

    /// Create and sign a raw transaction without broadcasting it.
    ///
    /// Returns the signed transaction for use with `broadcast_transaction()`.
    pub fn create_signed_transaction(
        &self,
        wallet_name: &str,
        input_txid: Txid,
        input_vout: u32,
        input_amount: Amount,
        destination: &Address,
        fee: Amount,
    ) -> Transaction {
        let client = self.rpc_client_for_wallet(wallet_name);

        let inputs = vec![rpc_json::CreateRawTransactionInput {
            txid: input_txid,
            vout: input_vout,
            sequence: None,
        }];
        let send_amount = input_amount.checked_sub(fee).expect("fee exceeds input amount");
        let mut outputs = HashMap::new();
        outputs.insert(destination.to_string(), send_amount);

        let raw_tx: Transaction = client
            .create_raw_transaction(&inputs, &outputs, None)
            .expect("failed to create raw tx");

        let signed = client
            .sign_raw_transaction_with_wallet(&raw_tx, None, None)
            .expect("failed to sign raw tx");
        assert!(signed.complete, "raw transaction signing incomplete");

        signed.transaction().expect("invalid signed tx")
    }

    /// Connect this dashd node to another dashd node via P2P and wait for the
    /// connection to be established.
    pub async fn connect_to_node(&self, addr: SocketAddr) {
        let client = self.rpc_client();
        client.onetry_node(&addr.to_string()).expect("failed to connect to node");

        for _ in 0..30 {
            let peers = client.get_peer_info().expect("failed to get peer info");
            if peers.iter().any(|p| p.addr.to_string().starts_with(&addr.ip().to_string())) {
                tracing::info!("Connected to node {}", addr);
                return;
            }
            sleep(Duration::from_millis(500)).await;
        }
        panic!("Timed out waiting for connection to {}", addr);
    }

    /// Disconnect a specific peer by address.
    pub fn disconnect_peer(&self, addr: SocketAddr) {
        let client = self.rpc_client();
        client.disconnect_node(&addr.to_string()).expect("failed to disconnect peer");
        tracing::info!("Disconnected peer {}", addr);
    }

    /// Enable or disable all P2P network activity on this node.
    pub fn set_network_active(&self, active: bool) {
        let client = self.rpc_client();
        client.set_network_active(active).expect("failed to set network active");
        tracing::info!("Set network active={} on dashd", active);
    }

    /// Set mock time on this node.
    pub fn set_mocktime(&self, time: u64) {
        let client = self.rpc_client();
        let _: Value = client.call("setmocktime", &[time.into()]).expect("setmocktime failed");
    }

    pub fn get_best_block_hash(&self) -> BlockHash {
        let client = self.rpc_client();
        client.get_best_block_hash().expect("getbestblockhash failed")
    }

    pub fn get_block_hash(&self, height: u32) -> BlockHash {
        let client = self.rpc_client();
        client.get_block_hash(height).expect("getblockhash failed")
    }

    /// Mark a block and its descendants invalid, rewinding the chain to its parent.
    pub fn invalidate_block(&self, hash: &BlockHash) {
        let client = self.rpc_client();
        client.invalidate_block(hash).expect("invalidateblock failed");
    }

    /// Call getblocktemplate to trigger CreateNewBlock (includes quorum commitments).
    pub fn get_block_template(&self) {
        let client = self.rpc_client();
        let _: Result<Value, _> = client.call("getblocktemplate", &[]);
    }

    /// Disconnect all currently connected peers.
    pub fn disconnect_all_peers(&self) {
        let client = self.rpc_client();
        let peers = client.get_peer_info().expect("failed to get peer info");
        for peer in &peers {
            let addr = peer.addr.to_string();
            let _ = client.disconnect_node(&addr);
            tracing::info!("Disconnected peer {}", addr);
        }
        tracing::info!("Disconnected {} peers", peers.len());
    }

    /// Execute an RPC call, returning None on failure instead of panicking.
    ///
    /// Uses the base URL (no wallet path) which works for all non-wallet RPCs.
    /// Useful during DKG orchestration where transient failures are expected.
    pub fn try_rpc_call(&self, method: &str, params: &[serde_json::Value]) -> Option<Value> {
        let url = format!("http://127.0.0.1:{}", self.config.rpc_port);
        let cookie_path = self.config.datadir.join("regtest/.cookie");
        if !cookie_path.exists() {
            return None;
        }
        let auth = Auth::CookieFile(cookie_path);
        let client = Client::new(&url, auth).ok()?;
        client.call(method, params).ok()
    }

    pub fn datadir(&self) -> &Path {
        &self.config.datadir
    }

    pub fn p2p_port(&self) -> u16 {
        self.config.p2p_port
    }

    pub fn rpc_port(&self) -> u16 {
        self.config.rpc_port
    }
}

impl Drop for DashCoreNode {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            tracing::info!("Stopping dashd process in Drop...");
            if let Err(e) = process.start_kill() {
                tracing::warn!("Failed to kill dashd process: {}", e);
            }
        }
    }
}

/// Wallet file structure for test wallets.
#[derive(Debug, Deserialize)]
pub struct WalletFile {
    /// Wallet name, e.g. "default"
    pub wallet_name: String,
    /// Wallet mnemonic, in BIP39 format
    pub mnemonic: String,
    /// Wallet balance, in duffs
    pub balance: f64,
    /// Number of transactions in the wallet
    pub transaction_count: usize,
    /// Number of UTXOs in the wallet
    pub utxo_count: usize,
    /// List of transaction hashes in the wallet
    pub transactions: Vec<serde_json::Value>,
    /// List of UTXOs in the wallet, including their addresses and amounts
    pub utxos: Vec<serde_json::Value>,
}

impl WalletFile {
    /// Load a wallet file from the wallets directory in a datadir
    pub fn from_json(datadir: &Path, wallet_name: &str) -> Self {
        let wallet_path = datadir.join("wallets").join(format!("{}.json", wallet_name));
        if !wallet_path.exists() {
            panic!("Wallet file not found: {:?}", wallet_path);
        }

        let contents = fs::read_to_string(&wallet_path).expect("Failed to read wallet file");
        serde_json::from_str(&contents).expect("Failed to deserialize wallet file")
    }

    /// The sum of the exported UTXOs in duffs: dashd's spendable balance.
    pub fn utxo_balance(&self) -> u64 {
        self.utxos
            .iter()
            .filter_map(|u| u.get("amount").and_then(|v| v.as_f64()))
            .map(|dash| (dash * 100_000_000.0).round() as u64)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines as Dash Core v23.1.0 writes them, for a node on P2P 20856 and RPC 20857.
    #[test]
    fn lost_port_reads_only_this_nodes_ports() {
        let node = DashCoreNode::with_config(DashCoreConfig::new(
            PathBuf::from("dashd"),
            PathBuf::new(),
            20856,
            20857,
        ));
        let lost = |log: &str| {
            BindFailure::from_log(log, node.config.p2p_port, node.config.rpc_port).is_some()
        };
        const IN_USE: &str = "on this computer. Dash Core is probably already running.";
        const LISTEN: &str = "[net:error] Listening for incoming connections failed (listen \
                              returned error Address already in use (98))";

        assert!(!lost("[libevent] Bound to port 20857 - Awaiting connections ...\n"));
        assert!(lost(&format!("[net:error] Unable to bind to 127.0.0.1:20856 {IN_USE}\n")));
        assert!(lost("Binding RPC on address 127.0.0.1 port 20857 failed.\n"));
        // v23.1.0's onion bind, which a node runs on without.
        assert!(!lost(&format!(
            "Bound to 127.0.0.1:20856\n[net:error] Unable to bind to 127.0.0.1:19896 {IN_USE}\n"
        )));
        // Other ports that start with this node's.
        assert!(!lost(&format!("[net:error] Unable to bind to 127.0.0.1:208560 {IN_USE}\n")));
        assert!(!lost("Binding RPC on address 127.0.0.1 port 208570 failed.\n"));

        // A listen failure counts only after this node's P2P bind.
        assert!(lost(&format!(
            "[libevent] Bound to port 20857\nBound to 127.0.0.1:20856\n{LISTEN}\n"
        )));
        assert!(!lost(&format!("Bound to 127.0.0.1:20856\nBound to 127.0.0.1:19896\n{LISTEN}\n")));
        assert!(!lost(&format!("Bound to 127.0.0.1:208560\n{LISTEN}\n")));
        assert!(!lost(&format!("{LISTEN}\n")));
    }

    #[test]
    fn dashd_impl_selects_the_node() {
        assert_eq!(NodeImpl::parse(None), Some(NodeImpl::Dashd));
        assert_eq!(NodeImpl::parse(Some("")), Some(NodeImpl::Dashd));
        assert_eq!(NodeImpl::parse(Some("dashd")), Some(NodeImpl::Dashd));
        assert_eq!(NodeImpl::parse(Some("rdashd")), Some(NodeImpl::Rdashd));
        assert_eq!(NodeImpl::parse(Some("Rdashd")), None);
    }

    /// dashd 23.1.0's log lines for a taken P2P port, a taken onion target port and a taken RPC
    /// port.
    const P2P_BIND_ERROR: &str = "2026-11-01T11:27:02Z [net:error] Unable to bind to 127.0.0.1:23411 on this computer. Dash Core is probably already running.\n";
    const ONION_BIND_ERROR: &str = "2026-11-01T11:27:02Z [net:error] Unable to bind to 127.0.0.1:19896 on this computer. Dash Core is probably already running.\n";
    const RPC_BIND_ERROR: &str = "2026-11-01T11:29:02Z Binding RPC on address 127.0.0.1 port 23414 failed.\n2026-11-01T11:29:02Z Unable to bind any endpoint for RPC server\n";

    #[test]
    fn bind_failures_are_read_for_the_node_s_own_ports() {
        assert_eq!(BindFailure::from_log(P2P_BIND_ERROR, 23411, 23414), Some(BindFailure::P2p));
        assert_eq!(BindFailure::from_log(P2P_BIND_ERROR, 2341, 23414), None);
        assert_eq!(BindFailure::from_log(ONION_BIND_ERROR, 23411, 23414), None);
        assert_eq!(BindFailure::from_log(RPC_BIND_ERROR, 23411, 23414), Some(BindFailure::Rpc));
        assert_eq!(BindFailure::from_log("Done loading\n", 23411, 23414), None);
    }
}
