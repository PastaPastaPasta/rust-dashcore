//! CH7a-4b's dashd differential (L1, CH7 §7.2, D15, D64): dashd v24.0.0-rc.4's recorded
//! DIP0003-active regtest chain (`dashd-chainstate/tests/data/ch7a4b`, DIP0003 at 48, DIP0024
//! moved out) goes as the same bytes to a fresh dashd's `submitblock` and to V. After each block
//! both nodes are compared: the tip, `getdeploymentinfo`'s report field by field, and the
//! masternode list (dashd's `protx list` and V's lists at its tip and DMN tip, all empty). V's
//! H4 checks each CbTx's `merkleRootMNList` and `merkleRootQuorums` against the roots it
//! computes, so a block V connects is one whose roots V computed as dashd did.
//!
//! Then a reorg back below DIP0003 restarts on both (BL-539, D64): `invalidateblock 41`, fork b's
//! blocks 41 to 47, a restart of each node on its own datadir, and `reconsiderblock 41`, which
//! returns both to main's tip across DIP0003's block 48.
//!
//! Needs dashd v24.0.0-rc.4: `DASHD_V24_PATH`, else setup-dashd's cache
//! (`DASHVERSION=24.0.0-rc.4 python3 contrib/setup-dashd.py`). `SKIP_DASHD_TESTS` skips it.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dash_consensus::Block;
use dash_consensus::deployments::info::DeploymentInfo;
use dash_consensus::primitives::{core_serialize, decode_block};
use dash_primitives::BlockHash;
use dashd_node::logging::Logger;
use dashd_node::sut::{NodeSut, Teardown};
use dashd_node::ui::Ui;
use serde_json::{Value, json};

const CORE_VERSION: &str = "v24.0.0-rc.4";
/// The recording's `-mocktime`.
const T0: i64 = 1_700_000_000;
/// The recording's chain parameters, on both nodes.
const ARGS: [&str; 2] = ["-dip3params=48:48", "-testactivationheight=dip0024@1000"];
/// dashd's main chain, blocks 1 to 200.
const MAIN: &str = include_str!("../../dashd-chainstate/tests/data/ch7a4b/blocks.txt");
/// Fork b: blocks 41 to 47 off main's 40, below DIP0003.
const FORK_B: &str = include_str!("../../dashd-chainstate/tests/data/ch7a4b/fork-b.txt");
/// The height fork b branches off.
const FORK_B_OFF: usize = 40;

fn dashd_binary() -> Option<PathBuf> {
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
struct Dashd {
    child: Child,
    rpc_port: u16,
}

impl Dashd {
    fn start(binary: &Path, datadir: &Path) -> Dashd {
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
    fn reply(&self, method: &str, params: Value) -> Option<Value> {
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
    fn try_call(&self, method: &str, params: Value) -> Option<Value> {
        self.reply(method, params).and_then(|r| r["error"].is_null().then(|| r["result"].clone()))
    }

    fn call(&self, method: &str, params: Value) -> Value {
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `text`'s blocks, skipping its `#` header lines.
fn blocks(text: &str) -> Vec<Arc<Block>> {
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
fn deployment_json(d: &DeploymentInfo) -> Value {
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
struct Harness {
    sut: NodeSut,
    /// `None` only inside [`Harness::restart`].
    dashd: Option<Dashd>,
    binary: PathBuf,
    dir: tempfile::TempDir,
}

impl Harness {
    /// `None` when the dashd tests are skipped.
    fn start() -> Option<Harness> {
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

    fn dashd(&self) -> &Dashd {
        self.dashd.as_ref().expect("dashd runs")
    }

    /// `submitblock` on dashd, which must answer null, and `ProcessNewBlock` on V, which must
    /// accept it.
    fn submit(&self, block: &Arc<Block>, height: usize) {
        let answer = self.dashd().call("submitblock", json!([hex(&core_serialize(&**block))]));
        assert_eq!(answer, Value::Null, "dashd at {height}");
        let chain = self.sut.chain().expect("V runs");
        let r = chain.process_block(Arc::clone(block), true, None, true).recv().expect("V");
        assert!(r.accepted && r.state.is_valid(), "V at {height}: {r:?}");
    }

    /// Both nodes agree at `height`, whose tip must be `tip`: their tips, their deployment
    /// reports and their (empty) masternode lists.
    fn assert_agree(&self, tip: &BlockHash, height: usize) {
        let chain = self.sut.chain().expect("V runs");
        let dashd_tip = self.dashd().call("getbestblockhash", json!([]));
        assert_eq!(dashd_tip, json!(tip.to_string()), "dashd's tip at {height}");
        let (v_tip, v_height, report, lists) = chain
            .query(|q| {
                let (tip, _) = q.chain.private_tip().expect("a tip");
                let e = q.evo_view();
                let list = e.tip_list().expect("reads");
                let dmn = e.dmn_list().expect("reads");
                let lists = (*list.block_hash(), list.len(), dmn.len());
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

        let protx = self.dashd().call("protx", json!(["list"]));
        assert_eq!(protx, json!([]), "dashd's list at {height}");
        // `evo_view`'s list at the tip is the tip's own from DIP0003; the DMN tip's list
        // (`GetListAtChainTip`, which `protx list` reads) can lag it after a disconnect, as V
        // has no mempool to move it (D16), and is empty either way.
        let (list_at, list_len, dmn_len) = lists;
        if height >= 48 {
            assert_eq!(list_at, *tip, "V's list at {height}");
        }
        assert_eq!((list_len, dmn_len), (0, 0), "V's lists at {height}");
    }

    /// The SUT's tip hash, as published.
    fn sut_tip(&self) -> BlockHash {
        self.sut.chain().expect("V runs").view().snap.tip.tip_hash
    }

    /// Restarts both nodes on their datadirs: dashd stopped and started again, V through a
    /// clean shutdown.
    fn restart(&mut self) {
        // Drop stops dashd and waits for it before the next one opens the datadir.
        self.dashd = None;
        self.dashd = Some(Dashd::start(&self.binary, &self.dir.path().join("dashd")));
        self.sut.restart(Teardown::Clean);
        assert_eq!(self.sut.down(), None, "V loads");
    }
}

/// Feeds main's blocks 1 to 200 to both nodes, comparing after each (every mining window's null
/// commitments through 186), then runs the reorg below DIP0003 and the restart (BL-539).
fn differential() {
    let Some(mut h) = Harness::start() else {
        return;
    };
    let main = blocks(MAIN);
    let fork_b = blocks(FORK_B);
    assert_eq!((main.len(), fork_b.len()), (200, 7));
    for (i, b) in main.iter().enumerate() {
        h.submit(b, i + 1);
        h.assert_agree(&b.hash(), i + 1);
    }

    // `invalidateblock 41`: both walk back to 40, below DIP0003, then take fork b.
    let cut = main[FORK_B_OFF].hash();
    h.dashd().call("invalidateblock", json!([cut.to_string()]));
    let chain = h.sut.chain().expect("V runs");
    assert_eq!(chain.invalidate_block(cut).recv(), Ok(Ok(())));
    h.assert_agree(&main[FORK_B_OFF - 1].hash(), FORK_B_OFF);
    for (i, b) in fork_b.iter().enumerate() {
        h.submit(b, FORK_B_OFF + 1 + i);
        h.assert_agree(&b.hash(), FORK_B_OFF + 1 + i);
    }
    let fork_tip = fork_b.last().unwrap().hash();

    // Both restart at a tip below DIP0003 whose datadir holds rows main's evo blocks wrote.
    h.restart();
    assert_eq!(h.sut_tip(), fork_tip, "V restarts at fork b's tip");
    h.assert_agree(&fork_tip, FORK_B_OFF + fork_b.len());

    // `reconsiderblock 41`: main has more work, so both return to its tip across 48.
    h.dashd().call("reconsiderblock", json!([cut.to_string()]));
    let chain = h.sut.chain().expect("V runs");
    assert_eq!(chain.reconsider_block(cut, false).recv(), Ok(Ok(())));
    h.assert_agree(&main[199].hash(), 200);
}

/// Under `sut::`, the gate's feature-step filter (`[package.metadata.gate]`).
mod sut {
    use super::*;

    #[test]
    fn the_dip3_chain_past_the_first_window_equals_dashds() {
        differential();
    }
}
