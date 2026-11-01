//! The dashd v24 regtest harness of dashd-node's differentials on dashd's recorded chains
//! (CH7a-4b's `ch7a4b_dashd`, CH7a-5a's `ch7a5_dashd`): a dashd on its own datadir and free
//! ports over JSON-RPC, and V (`NodeSut`) beside it, both with the recordings' arguments
//! (`-mocktime`, DIP0003 at 48, DIP0024 moved out).
//!
//! Needs dashd v24.0.0-rc.5: `DASHD_V24_PATH`, else setup-dashd's cache
//! (`DASHVERSION=24.0.0-rc.5 python3 contrib/setup-dashd.py`). `SKIP_DASHD_TESTS` skips it.

// Each test binary uses part of it.
#![allow(dead_code)]

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dash_consensus::Block;
use dash_consensus::deployments::info::DeploymentInfo;
use dash_consensus::primitives::{core_serialize, decode_block};
use dash_dml::MnList;
use dash_primitives::BlockHash;
use dashd_node::logging::Logger;
use dashd_node::sut::{NodeSut, Teardown};
use dashd_node::ui::Ui;
use serde_json::{Value, json};

const CORE_VERSION: &str = "v24.0.0-rc.5";
/// The recordings' `-mocktime`.
pub const T0: i64 = 1_700_000_000;
/// The recordings' chain parameters, on both nodes.
pub const ARGS: [&str; 2] = ["-dip3params=48:48", "-testactivationheight=dip0024@1000"];

pub fn dashd_binary() -> Option<PathBuf> {
    if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
        eprintln!("Skipping dashd integration test (SKIP_DASHD_TESTS is set)");
        return None;
    }
    let version = CORE_VERSION.strip_prefix('v').unwrap();
    let mut candidates: Vec<PathBuf> =
        std::env::var_os("DASHD_V24_PATH").map(PathBuf::from).into_iter().collect();
    candidates.extend(std::env::var_os("HOME").map(|h| {
        PathBuf::from(h).join(format!(".rust-dashcore-test/dashcore-{version}/bin/dashd"))
    }));
    let found = candidates.into_iter().find(|p| {
        let dir = tempfile::tempdir().unwrap();
        Command::new(p)
            .arg("-version")
            .arg(format!("-datadir={}", dir.path().display()))
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .is_some_and(|v| v.lines().next().is_some_and(|l| l.ends_with(CORE_VERSION)))
    });
    Some(found.unwrap_or_else(|| {
        panic!(
            "dashd {CORE_VERSION} not found: run `DASHVERSION={version} python3 \
             contrib/setup-dashd.py` or set DASHD_V24_PATH, or set SKIP_DASHD_TESTS=1"
        )
    }))
}

/// A regtest dashd on its own datadir and ports, stopped on drop.
pub struct Dashd {
    child: Child,
    rpc_port: u16,
}

impl Dashd {
    pub fn start(binary: &Path, datadir: &Path) -> Dashd {
        std::fs::create_dir_all(datadir).unwrap();
        let rpc_port = node_test_ports::free_port();
        let p2p_port = node_test_ports::free_port();
        let child = Command::new(binary)
            .args([
                "-regtest".to_owned(),
                format!("-datadir={}", datadir.display()),
                format!("-port={p2p_port}"),
                format!("-rpcport={rpc_port}"),
                "-listen=0".to_owned(),
                "-server=1".to_owned(),
                "-rpcuser=mn0".to_owned(),
                "-rpcpassword=mn0".to_owned(),
                format!("-mocktime={T0}"),
                "-printtoconsole=0".to_owned(),
            ])
            .args(ARGS)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("dashd starts");
        let mut dashd = Dashd {
            child,
            rpc_port,
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        while dashd.try_call("getblockcount", json!([])).is_none() {
            if let Some(status) = dashd.child.try_wait().unwrap() {
                panic!("dashd exited before its RPC came up: {status}");
            }
            assert!(Instant::now() < deadline, "dashd's RPC never came up");
            std::thread::sleep(Duration::from_millis(200));
        }
        dashd
    }

    /// The whole JSON-RPC reply, or `None` when the server is not up.
    pub fn reply(&self, method: &str, params: Value) -> Option<Value> {
        let body = json!({"method": method, "params": params, "id": 1}).to_string();
        let request = format!(
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic bW4wOm1uMA==\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(("127.0.0.1", self.rpc_port)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(300))).ok()?;
        stream.write_all(request.as_bytes()).ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        let (_, body) = reply.split_once("\r\n\r\n")?;
        serde_json::from_str(body).ok()
    }

    /// The call's `result`, or `None` when the server is not up or answers an error.
    pub fn try_call(&self, method: &str, params: Value) -> Option<Value> {
        self.reply(method, params).and_then(|r| r["error"].is_null().then(|| r["result"].clone()))
    }

    pub fn call(&self, method: &str, params: Value) -> Value {
        self.try_call(method, params.clone())
            .unwrap_or_else(|| panic!("dashd {method} {params} failed"))
    }
}

impl Drop for Dashd {
    fn drop(&mut self) {
        let _ = self.try_call("stop", json!([]));
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `text`'s blocks, skipping its `#` header lines.
pub fn blocks(text: &str) -> Vec<Arc<Block>> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let bytes: Vec<u8> = (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap())
                .collect();
            let mut input = bytes.as_slice();
            let block = decode_block(&mut input).expect("a block").into_value();
            assert!(input.is_empty(), "one block per line");
            Arc::new(block)
        })
        .collect()
}

/// Core's `SoftForkDescPushBack` object of one entry (`rpc/blockchain.cpp:1445-1544`), from V's
/// typed report, keyed as dashd prints it.
pub fn deployment_json(d: &DeploymentInfo) -> Value {
    match d {
        DeploymentInfo::Buried(b) => {
            json!({"type": "buried", "active": b.active, "height": b.height})
        }
        DeploymentInfo::Bip9(b) => {
            let mut bip9 = json!({
                "start_time": b.start_time,
                "timeout": b.timeout,
                "ehf": b.ehf,
                "min_activation_height": b.min_activation_height,
                "status": b.status.name(),
                "since": b.since,
                "status_next": b.status_next.name(),
            });
            let o = bip9.as_object_mut().unwrap();
            if let Some(bit) = b.bit {
                o.insert("bit".into(), json!(bit));
            }
            if let Some(h) = b.ehf_height {
                o.insert("ehf_height".into(), json!(h));
            }
            if let Some(h) = b.activation_height {
                o.insert("activation_height".into(), json!(h));
            }
            if let Some(s) = &b.statistics {
                let mut stats = json!({"period": s.period, "elapsed": s.elapsed, "count": s.count});
                let so = stats.as_object_mut().unwrap();
                if let Some(t) = s.threshold {
                    so.insert("threshold".into(), json!(t));
                }
                if let Some(p) = s.possible {
                    so.insert("possible".into(), json!(p));
                }
                o.insert("statistics".into(), stats);
            }
            if let Some(sig) = &b.signalling {
                o.insert("signalling".into(), json!(sig));
            }
            let mut rv = json!({"type": "bip9", "active": b.active, "bip9": bip9});
            if let Some(h) = b.height {
                rv.as_object_mut().unwrap().insert("height".into(), json!(h));
            }
            rv
        }
    }
}

/// Both nodes of the test on one temp dir. Fields drop in order: the SUT, then dashd, then the
/// dir.
pub struct Harness {
    pub sut: NodeSut,
    /// `None` only inside [`Harness::restart`].
    dashd: Option<Dashd>,
    binary: PathBuf,
    dir: tempfile::TempDir,
}

impl Harness {
    /// `None` when the dashd tests are skipped.
    pub fn start() -> Option<Harness> {
        let binary = dashd_binary()?;
        let dir = tempfile::tempdir().unwrap();
        let dashd = Dashd::start(&binary, &dir.path().join("dashd"));
        let sut_dir = dir.path().join("rdashd");
        std::fs::create_dir_all(&sut_dir).unwrap();
        let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
        let argv = ["-regtest", "-printtoconsole=0", ARGS[0], ARGS[1]];
        let sut = NodeSut::new(&sut_dir, &argv, T0, ui).expect("the SUT starts");
        Some(Harness {
            sut,
            dashd: Some(dashd),
            binary,
            dir,
        })
    }

    pub fn dashd(&self) -> &Dashd {
        self.dashd.as_ref().expect("dashd runs")
    }

    /// `submitblock` on dashd, which must answer null, and `ProcessNewBlock` on V, which must
    /// accept it.
    pub fn submit(&self, block: &Arc<Block>, height: usize) {
        let answer = self.dashd().call("submitblock", json!([hex(&core_serialize(&**block))]));
        assert_eq!(answer, Value::Null, "dashd at {height}");
        let chain = self.sut.chain().expect("V runs");
        let r = chain.process_block(Arc::clone(block), true, None, true).recv().expect("V");
        assert!(r.accepted && r.state.is_valid(), "V at {height}: {r:?}");
    }

    /// Both nodes are at `tip`, at `height`, with the same deployment report; V's lists at its
    /// tip and at its DMN tip, for the caller's checks. From DIP0003 the tip's list is the tip's
    /// own.
    pub fn assert_agree_on_tip(
        &self,
        tip: &BlockHash,
        height: usize,
    ) -> (Arc<MnList>, Arc<MnList>) {
        let chain = self.sut.chain().expect("V runs");
        let dashd_tip = self.dashd().call("getbestblockhash", json!([]));
        assert_eq!(dashd_tip, json!(tip.to_string()), "dashd's tip at {height}");
        let (v_tip, v_height, report, lists) = chain
            .query(|q| {
                let (tip, _) = q.chain.private_tip().expect("a tip");
                let e = q.evo_view();
                let lists = (e.tip_list().expect("reads"), e.dmn_list().expect("reads"));
                (tip.hash, tip.height, q.deployments.deployment_info(tip.id), lists)
            })
            .recv()
            .expect("V serves");
        assert_eq!((v_tip, v_height as usize), (*tip, height), "V's tip");

        let core = self.dashd().call("getdeploymentinfo", json!([tip.to_string()]));
        assert_eq!(core["hash"], json!(tip.to_string()));
        let ours: serde_json::Map<String, Value> =
            report.iter().map(|d| (d.name().to_owned(), deployment_json(d))).collect();
        assert_eq!(core["deployments"], Value::Object(ours), "getdeploymentinfo at {height}");
        // `active` is for the block after the tip: DIP0003's block is 48.
        let dip3 = &core["deployments"]["dip0003"];
        assert_eq!(dip3["active"], json!(height >= 47), "dip0003 at {height}: {dip3}");
        if height >= 48 {
            assert_eq!(lists.0.block_hash(), tip, "V's list at {height}");
        }
        lists
    }

    /// The SUT's tip hash, as published.
    pub fn sut_tip(&self) -> BlockHash {
        self.sut.chain().expect("V runs").view().snap.tip.tip_hash
    }

    /// Restarts both nodes on their datadirs: dashd stopped and started again, V through a
    /// clean shutdown.
    pub fn restart(&mut self) {
        // Drop stops dashd and waits for it before the next one opens the datadir.
        self.dashd = None;
        self.dashd = Some(Dashd::start(&self.binary, &self.dir.path().join("dashd")));
        self.sut.restart(Teardown::Clean);
        assert_eq!(self.sut.down(), None, "V loads");
    }
}
