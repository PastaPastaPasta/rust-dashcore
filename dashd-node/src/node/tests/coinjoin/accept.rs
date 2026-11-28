//! CJ1-W7's acceptance legs: Dash Core's CoinJoin clients against rdashd's masternode server.
//! rdashd runs in this process as masternode 0 of setup-dashd's `regtest-mn` image, with that
//! masternode's `network.json` operator key and on free ports (CJ1-W plan §6 Q1, after AM1-A7).
//!
//! The harness is `contrib/node/cj1-transcripts/capture.py target` (its README): it copies the
//! image, starts the controller and asks this test, over its stdout and stdin, to start rdashd in
//! place of the masternode. It then updates that masternode's service to rdashd's port and
//! drives each scenario against it.
//!
//! - **Python leg**: Core's server scenarios as the harness recorded them from dashd v24.0.0
//!   (`dashd-coinjoin/tests/fixtures/dashd-v24/`). Every projected message and the observations
//!   taken from the wire and RPC must equal the fixture's.
//! - **Wallet leg** (ignored by default): two dashd v24.0.0 wallets mix one round through rdashd,
//!   the one valid masternode (the harness revokes the others).
//!
//! The legs take turns ([`ONE_RDASHD`]), so one rdashd runs in this process at a time.
//!
//! Both need the venv the README sets up, named by `CJ1_PYTHON`, and the directory holding its
//! `setup.json` (setup-dashd's dashd v24.0.0 and `regtest-mn` image), named by `CJ1_SCRATCH`.
//! Without them the tests skip. `CJ1_TARGET_SCENARIOS` (space-separated) runs a subset of the
//! Python leg.

use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_spv::test_utils::retain_test_dir_now;
use serde_json::{Value, json};

use crate::coinjoin::role::Role;
use crate::init::{init_config, setup_server_args};
use crate::logging::{CoreSubscriber, Logger};
use crate::node::Node;
use crate::test_log::{Tap, tap};
use crate::ui::Ui;

/// The harness's RPC credentials (`p2a.Node.rpc`).
const RPC_AUTH: &str = "p2a";

/// Held by each leg for its whole run: one in-process rdashd at a time, since its logger hears
/// every thread's lines through the global subscriber.
static ONE_RDASHD: Mutex<()> = Mutex::new(());

/// The harness's interpreter and scratch directory, or why the test skips.
fn harness() -> Result<(PathBuf, PathBuf), String> {
    let python = std::env::var_os("CJ1_PYTHON").ok_or("CJ1_PYTHON is not set")?;
    let scratch = std::env::var_os("CJ1_SCRATCH").ok_or("CJ1_SCRATCH is not set")?;
    let scratch = PathBuf::from(scratch);
    if !scratch.join("setup.json").is_file() {
        return Err(format!("no {}/setup.json: run capture.py setup", scratch.display()));
    }
    Ok((python.into(), scratch))
}

/// rdashd as the image's masternode.
struct Rdashd {
    node: Option<Node>,
    datadir: PathBuf,
    /// The node's logger on the global subscriber, so its `debug.log` has its threads' lines.
    _log: Tap,
}

impl Rdashd {
    /// Started as the harness asks: an outbound peer of the controller at `controller`, the
    /// image's chain parameters (`extra`), its spork address and the masternode's operator key.
    /// The reply names its ports, its datadir and its arguments with the key masked.
    fn start(request: &Value, root: &Path) -> (Rdashd, Value) {
        let (p2p, rpc) = (node_test_ports::free_port(), node_test_ports::free_port());
        let name = request["name"].as_str().expect("a name");
        let datadir = root.join(format!("{name}-{p2p}"));
        std::fs::create_dir_all(&datadir).unwrap();
        let mut args: Vec<String> = vec![
            format!("-datadir={}", datadir.display()),
            "-regtest".into(),
            "-printtoconsole=0".into(),
            "-server=1".into(),
            format!("-rpcport={rpc}"),
            "-rpcbind=127.0.0.1".into(),
            "-rpcallowip=127.0.0.1".into(),
            format!("-rpcuser={RPC_AUTH}"),
            format!("-rpcpassword={RPC_AUTH}"),
            "-listen=1".into(),
            format!("-bind=127.0.0.1:{p2p}"),
            format!("-port={p2p}"),
            format!("-externalip=127.0.0.1:{p2p}"),
            format!("-connect=127.0.0.1:{}", request["controller"]),
            format!("-mocktime={}", request["mocktime"]),
            format!("-sporkaddr={}", request["sporkaddr"].as_str().expect("a spork address")),
            "-listenonion=0".into(),
            "-discover=0".into(),
            "-dnsseed=0".into(),
            "-fixedseeds=0".into(),
            "-upnp=0".into(),
            "-natpmp=0".into(),
            "-v2transport=0".into(),
            "-logips=0".into(),
            "-debug=coinjoin".into(),
            "-debug=net".into(),
            "-debug=mempool".into(),
            "-txindex=1".into(),
        ];
        let extra = request["extra"].as_array().expect("the image's arguments");
        args.extend(extra.iter().map(|a| a.as_str().expect("an argument").to_owned()));
        let masked = args.iter().cloned().chain(["-masternodeblsprivkey=<network.json>".into()]);
        let masked: Vec<String> = masked.collect();
        let key = request["operator_key"].as_str().expect("the operator key");
        args.push(format!("-masternodeblsprivkey={key}"));
        let argv: Vec<String> = std::iter::once("rdashd".to_owned()).chain(args).collect();
        let parsed = ArgsManager::new();
        setup_server_args(&parsed);
        parsed.parse_parameters(&argv).expect("rdashd's arguments");
        let params = init_config(&parsed).expect("rdashd's config");
        let logger = Arc::new(Logger::new());
        let log = tap(CoreSubscriber::new(Arc::clone(&logger)));
        let mut node = Node::new(Arc::new(parsed), Arc::new(Ui::new(logger)), params);
        let started = node.app_init_early() && node.app_init_main();
        let rdashd = Rdashd {
            node: Some(node),
            datadir,
            _log: log,
        };
        assert!(started, "rdashd starts as {name}");
        let reply = json!({"p2p": p2p, "rpc": rpc, "datadir": rdashd.datadir, "args": masked});
        (rdashd, reply)
    }

    /// `getcoinjoininfo` on a masternode: the server's `GetJsonInfo`.
    // Core: rpc/coinjoin.cpp:474-479, coinjoin/server.cpp:1435-1443 (v24.0.2)
    fn coinjoin_info(&self) -> Value {
        let node = self.node.as_ref().expect("running");
        let role = node.net.as_ref().and_then(|net| net.coinjoin_role());
        let server = role.and_then(Role::server).expect("a masternode runs the server");
        let info = server.info();
        json!({
            "queue_size": info.queue_size,
            "denomination": info.denomination as f64 / 1e8,
            "state": info.state.as_str(),
            "entries_count": info.entries_count,
        })
    }

    fn stop(&mut self) {
        if let Some(mut node) = self.node.take() {
            node.interrupt();
            node.shutdown();
            assert_eq!(node.exit_status(), 0, "rdashd's exit status");
        }
    }
}

impl Drop for Rdashd {
    fn drop(&mut self) {
        let failed = std::thread::panicking();
        self.stop();
        if failed || std::env::var_os("DASHD_TEST_RETAIN_ALWAYS").is_some() {
            let label = self.datadir.file_name().unwrap().to_string_lossy().into_owned();
            retain_test_dir_now(&self.datadir, &label);
        }
    }
}

/// The harness's process. Dropping it closes its stdin, which makes a waiting harness stop its
/// dashd nodes and exit; it is killed only if it has not exited after [`Harness::REAP`].
struct Harness {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Harness {
    const REAP: Duration = Duration::from_secs(600);

    fn reply(&mut self, reply: &Value) {
        let stdin = self.stdin.as_mut().expect("open");
        writeln!(stdin, "{reply}").and_then(|()| stdin.flush()).expect("the harness reads");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Self::REAP;
        while self.child.try_wait().ok().flatten().is_none() {
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Runs `capture.py target` with `scenarios`, serving its requests, and returns its exit
/// success and its summary. `None` when the harness is not set up.
fn run_target(label: &str, scenarios: &[&str]) -> Option<(bool, Value)> {
    let (python, scratch) = match harness() {
        Ok(found) => found,
        Err(why) => {
            eprintln!("skipped: {why} (contrib/node/cj1-transcripts/README.md sets them up)");
            return None;
        }
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../contrib/node/cj1-transcripts/capture.py")
        .canonicalize()
        .unwrap();
    let _one = ONE_RDASHD.lock().unwrap_or_else(PoisonError::into_inner);
    let root = tempfile::Builder::new().prefix("cj1-w7-").tempdir().unwrap();
    let log_path = scratch.join(format!("{label}.log"));
    let child = Command::new(python)
        .arg("-u")
        .arg(&script)
        .arg("target")
        .args(scenarios)
        .arg("--setup")
        .arg(scratch.join("setup.json"))
        .env("CJ1_SCRATCH", &scratch)
        .env("P2A_QUIET", "0.3")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(File::create(&log_path).unwrap())
        .spawn()
        .expect("the harness starts");
    let mut harness = Harness {
        child,
        stdin: None,
    };
    harness.stdin = harness.child.stdin.take();
    let stdout = BufReader::new(harness.child.stdout.take().unwrap());
    let (mut target, mut summary) = (None::<Rdashd>, Value::Null);
    for line in stdout.lines() {
        let line = line.expect("the harness's output");
        let Some(request) = line.strip_prefix("CJ1-TARGET ") else {
            eprintln!("harness: {line}");
            continue;
        };
        let request: Value = serde_json::from_str(request).expect("a request");
        let reply = match request["op"].as_str() {
            Some("start") => {
                assert!(target.is_none(), "one rdashd at a time");
                let (started, reply) = Rdashd::start(&request, root.path());
                target = Some(started);
                reply
            }
            Some("getcoinjoininfo") => {
                json!({"result": target.as_ref().expect("started").coinjoin_info()})
            }
            Some("stop") => {
                drop(target.take());
                json!({})
            }
            Some("done") => {
                summary = request["results"].clone();
                json!({})
            }
            _ => panic!("unknown request {request}"),
        };
        harness.reply(&reply);
    }
    drop(target);
    let status = harness.child.wait().expect("the harness exits");
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let tail: Vec<&str> = log.lines().rev().take(80).collect();
    eprintln!("harness log {} (tail):", log_path.display());
    for line in tail.into_iter().rev() {
        eprintln!("  {line}");
    }
    Some((status.success(), summary))
}

/// Core's server scenarios from dashd's clients, as the fixtures recorded them: the session's
/// happy path with the BL-180 pre-decode guards, the empty entry (MNO-19), ERR_RECENT, the
/// revoked masternode (ERR_MN_LIST), the DSTX of a removed masternode, and the timeouts without
/// and with a charge.
// Core: coinjoin/server.cpp (v24.0.2), net_processing.cpp:3642-3720
#[test]
fn dashd_clients_see_the_rdashd_server_answer_as_dashd() {
    let started = Instant::now();
    let only = std::env::var("CJ1_TARGET_SCENARIOS").unwrap_or_default();
    let only: Vec<&str> = only.split_whitespace().collect();
    let Some((passed, summary)) = run_target("w7-python-leg", &only) else {
        return;
    };
    eprintln!("summary {summary} in {:?}", started.elapsed());
    assert!(passed, "the harness found a difference: {summary}");
}

/// Two dashd v24.0.0 wallets mix one round through rdashd, the one valid masternode: `dsc`
/// MSG_SUCCESS to both, the mixing transaction in both wallets and the controller sees its DSTX.
/// Core's client timing (queues, signing, random waits) makes it minutes long and sensitive to
/// load, so it runs only when asked.
#[test]
#[ignore = "CJ1-W7 wallet leg: minutes of Core client timing; run with --ignored (Actions)"]
fn dashd_wallets_mix_a_round_through_the_rdashd_server() {
    let started = Instant::now();
    let Some((passed, summary)) = run_target("w7-wallet-leg", &["wallet"]) else {
        return;
    };
    eprintln!("summary {summary} in {:?}", started.elapsed());
    assert!(passed, "the round did not complete: {summary}");
}
