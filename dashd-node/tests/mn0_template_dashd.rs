//! MN0-1's differential (DEC-595): the template `dashd-mining` builds on the node's chain,
//! ground by `grind`, is byte for byte the block dashd v24.0.0-rc.4 mines on the same chain at
//! the same mock time. Core's `GenerateBlock` grinds from nonce 0 like ours, so
//! `generateblock <output> [] false` returns exactly the block Core would mine; each height's
//! block then goes to both nodes (dashd's `submitblock` answers null, V accepts it), so the next
//! height starts from the same tip. The run covers heights 1–431 (regtest's last block before
//! DIP3): the halvings at 150 and 300, TESTDUMMY's signalling from 144, a stretch where the
//! median time past outruns a frozen clock, and three kinds of coinbase script; then the
//! template at 432 is refused (G1).
//!
//! Needs dashd v24.0.0-rc.4: `DASHD_V24_PATH`, else setup-dashd's cache
//! (`DASHVERSION=24.0.0-rc.4 python3 contrib/setup-dashd.py`). `SKIP_DASHD_TESTS` skips it.

use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dash_clock::Clock;
use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::core_serialize;
use dash_types::Numeric as _;
use dashd_chainstate::chain::ChainHandle;
use dashd_mining::{
    BlockTemplateSource, ChainTemplateSource, Grind, LastBlockStats, TemplateError, TemplateTxs,
    grind,
};
use dashd_node::logging::Logger;
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;
use serde_json::{Value, json};

const CORE_VERSION: &str = "v24.0.0-rc.4";
const T0: i64 = 1_700_000_000;
/// rpc_generate's regtest P2PKH address.
const P2PKH: &str = "ycwedq2f3sz2Yf9JqZsBCQPxp18WU3Hp4J";
/// `CreateNewBlock`'s log line for an empty mempool's block (`node/miner.cpp:261`).
const CREATE_NEW_BLOCK_LINE: &str = "CreateNewBlock(): total size 1000 txs: 0 fees: 0 sigops 100";

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
    datadir: PathBuf,
}

impl Dashd {
    fn start(binary: &Path, datadir: &Path, extra: &[&str]) -> Dashd {
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
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("dashd starts");
        let mut dashd = Dashd {
            child,
            rpc_port,
            datadir: datadir.to_owned(),
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

    /// The messages of `debug.log`'s lines that start with `prefix`, timestamps cut.
    fn logged(&self, prefix: &str) -> Vec<String> {
        let log = std::fs::read_to_string(self.datadir.join("regtest/debug.log")).unwrap();
        log.lines().filter_map(|l| l.find(prefix).map(|i| l[i..].to_owned())).collect()
    }

    /// The call's `result`, or `None` when the server is not up or answers an error.
    fn try_call(&self, method: &str, params: Value) -> Option<Value> {
        self.reply(method, params).and_then(|r| r["error"].is_null().then(|| r["result"].clone()))
    }

    /// The call's `error`, which it must answer.
    fn call_error(&self, method: &str, params: Value) -> Value {
        let reply = self.reply(method, params.clone()).expect("a reply");
        assert!(!reply["error"].is_null(), "dashd {method} {params} succeeded");
        reply["error"].clone()
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

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn start_sut(dir: &Path, network: &str) -> NodeSut {
    std::fs::create_dir_all(dir).unwrap();
    let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
    NodeSut::new(dir, &[network, "-printtoconsole=0"], T0, ui).expect("the SUT starts")
}

fn source(sut: &NodeSut, block_version: Option<i32>) -> (ChainHandle, ChainTemplateSource) {
    let chain = sut.chain().expect("V runs").clone();
    let clock: Arc<dyn Clock> = sut.clock().clone();
    let params = Arc::new(sut.params().clone());
    (chain.clone(), ChainTemplateSource::new(chain, params, clock, block_version))
}

/// Both nodes of a test on one temp dir. Fields drop in order: the SUT, then dashd, then the dir;
/// tests borrow the fields, so a destructured temporary never drops the dir under dashd.
struct Harness {
    sut: NodeSut,
    dashd: Dashd,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// `None` when the dashd tests are skipped.
    fn start(dashd_args: &[&str]) -> Option<Harness> {
        let binary = dashd_binary()?;
        let dir = tempfile::tempdir().unwrap();
        let dashd = Dashd::start(&binary, &dir.path().join("dashd"), dashd_args);
        let sut = start_sut(&dir.path().join("rdashd"), "-regtest");
        Some(Harness {
            sut,
            dashd,
            _dir: dir,
        })
    }
}

/// `last_block_stats` after any assembly of an empty mempool's block, a refused one included.
const RESERVED_STATS: LastBlockStats = LastBlockStats {
    num_txs: 0,
    size: 1000,
};

/// The coinbase outputs the run rotates through: `generateblock`'s argument and its script.
fn outputs(dashd: &Dashd) -> Vec<(String, Vec<u8>)> {
    let script = |addr: &str| {
        unhex(dashd.call("validateaddress", json!([addr]))["scriptPubKey"].as_str().unwrap())
    };
    let p2sh = dashd.call("decodescript", json!(["51"]))["p2sh"].as_str().unwrap().to_owned();
    vec![
        (P2PKH.to_owned(), script(P2PKH)),
        (p2sh.clone(), script(&p2sh)),
        ("raw(51)".to_owned(), vec![0x51]),
    ]
}

/// What a run went through: each block version, and how many blocks `UpdateTime` dated after
/// the clock (the median time past ahead of it).
#[derive(Default)]
struct Seen {
    versions: BTreeSet<i32>,
    raised: u32,
}

/// Mines heights `from..=to` on both nodes and checks every block is the same bytes. The clock
/// moves 150 s a block except over `frozen`, where it stays put and the median time past
/// overtakes it.
fn mine_both(
    dashd: &Dashd,
    sut: &NodeSut,
    chain: &ChainHandle,
    source: &ChainTemplateSource,
    outputs: &[(String, Vec<u8>)],
    heights: std::ops::RangeInclusive<u32>,
    frozen: std::ops::Range<u32>,
) -> Seen {
    let mut seen = Seen::default();
    let pow = *HeaderParams::from_chain_params(sut.params()).unwrap().pow();
    let mut time = T0;
    for h in heights {
        if !frozen.contains(&h) {
            time += 150;
        }
        dashd.call("setmocktime", json!([time]));
        sut.clock().set_mocktime(time);
        let (output, script) = &outputs[h as usize % outputs.len()];
        let core = dashd.call("generateblock", json!([output, [], false]));
        let core_hex = core["hex"].as_str().unwrap();

        let template = source.create_new_block(script, TemplateTxs::None).expect("a template");
        assert_eq!(template.height, h);
        let mut block = template.block;
        let mut tries = 1_000_000;
        assert_eq!(grind(&mut block, &pow, &mut tries, &|| false), Grind::Found);
        assert_eq!(hex(&core_serialize(&block)), core_hex, "height {h}, output {output}");
        assert_eq!(block.hash().to_string(), core["hash"].as_str().unwrap());
        seen.versions.insert(block.header().version());
        seen.raised += u32::from(i64::from(block.header().time()) > time);

        assert_eq!(dashd.call("submitblock", json!([core_hex])), Value::Null, "height {h}");
        let r = chain.process_block(Arc::new(block), true, None, true).recv().expect("V");
        assert!(r.accepted, "height {h}: V refused our block");
        assert_eq!(dashd.call("getbestblockhash", json!([])), core["hash"]);
    }
    seen
}

/// Under `sut::`, the gate's feature-step filter (`[package.metadata.gate]`).
mod sut {
    use super::*;

    #[test]
    fn templates_equal_dashds_blocks_up_to_dip3() {
        let Some(h) = Harness::start(&[]) else {
            return;
        };
        let (sut, dashd) = (&h.sut, &h.dashd);
        let (chain, source) = source(sut, None);
        assert_eq!(source.last_block_stats(), None);
        let outputs = outputs(dashd);

        let seen = mine_both(dashd, sut, &chain, &source, &outputs, 1..=431, 200..260);
        // TESTDUMMY (bit 28) is signalled from 144 (STARTED) through 431 (LOCKED_IN).
        assert_eq!(seen.versions.into_iter().collect::<Vec<_>>(), [0x2000_0000, 0x3000_0000]);
        assert!(seen.raised > 0, "no block dated by the median time past");
        // Core logged its line once per template, as the source's constant has it.
        assert_eq!(dashd.logged("CreateNewBlock(): total size"), [CREATE_NEW_BLOCK_LINE; 431]);

        assert_eq!(source.last_block_stats(), Some(RESERVED_STATS));
        // Block 432 is DIP3's first: Core assembles a CbTx there, MN0-2 will.
        match source.create_new_block(&outputs[0].1, TemplateTxs::Mempool) {
            Err(TemplateError::NotReady {
                height: 432,
                rule: "DIP0003",
                owner: "MN0-2",
            }) => {}
            Err(e) => panic!("{e:?}"),
            Ok(t) => panic!("a template at {}", t.height),
        }
    }

    #[test]
    fn block_version_overrides_equal_dashds() {
        let Some(h) = Harness::start(&["-blockversion=1337"]) else {
            return;
        };
        let (sut, dashd) = (&h.sut, &h.dashd);
        let args = dash_args::ArgsManager::new();
        dashd_node::init::setup_server_args(&args);
        args.parse_parameters(&["rdashd", "-regtest", "-blockversion=1337"]).unwrap();
        let version = dashd_mining::block_version_arg(&args, sut.params()).unwrap();
        assert_eq!(version, Some(1337));
        let (chain, source) = source(sut, version);
        let outputs = outputs(dashd);
        let seen = mine_both(dashd, sut, &chain, &source, &outputs, 1..=5, 0..0);
        assert_eq!(seen.versions.into_iter().collect::<Vec<_>>(), [1337]);
    }

    /// A template `TestBlockValidity` refuses fails with Core's text: version 1 is below BIP34's
    /// floor from height 1 on regtest.
    #[test]
    fn a_refused_template_fails_with_cores_text() {
        let Some(h) = Harness::start(&["-blockversion=1"]) else {
            return;
        };
        let (sut, dashd) = (&h.sut, &h.dashd);
        let (_, source) = source(sut, Some(1));
        let core = dashd.call_error("generateblock", json!([P2PKH, [], false]));
        let script = outputs(dashd).swap_remove(0).1;
        match source.create_new_block(&script, TemplateTxs::None) {
            Err(TemplateError::Failed(msg)) => {
                assert_eq!(core, json!({"code": -1, "message": msg}));
            }
            Err(e) => panic!("{e:?}"),
            Ok(_) => panic!("a template"),
        }
        // Core sets the counters before its TestBlockValidity.
        assert_eq!(source.last_block_stats(), Some(RESERVED_STATS));
    }

    /// Off regtest the next work is not the tip's: testnet's block 1, more than two spacings after
    /// its 2014 genesis (`0x1e0ffff0`), takes the minimum-difficulty powLimit, `0x1e0fffff`.
    #[test]
    fn testnet_height_one_takes_the_min_difficulty_bits() {
        let dir = tempfile::tempdir().unwrap();
        let sut = start_sut(&dir.path().join("rdashd"), "-testnet");
        let (chain, source) = source(&sut, None);
        let genesis_bits = chain
            .query(|q| {
                let (tip, _) = q.chain.private_tip().unwrap();
                q.chain.complete().unwrap().header(tip.id).bits.to_base()
            })
            .recv()
            .unwrap();
        assert_eq!(genesis_bits, 0x1e0f_fff0);
        let template = source.create_new_block(&[0x51], TemplateTxs::None).unwrap();
        assert_eq!((template.height, template.block.header().bits()), (1, 0x1e0f_ffff));
    }
}
