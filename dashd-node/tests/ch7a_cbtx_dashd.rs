//! CH7a-2b-2's Core verdicts (DEC-730, BL-516, CH7 D57, D59): regtest blocks below DIP0003 whose
//! coinbase carries the CbTx type bits, each sent as the same bytes to dashd v24.0.0-rc.4's
//! `submitblock` and to V (V decodes the bytes dashd got). dashd's answer is the verdict (null,
//! or the reject reason); V's is the state its `BlockChecked` carries, as Core's
//! `submitblock_StateCatcher` reads it. The reject's class is checked on both sides too: a
//! `BLOCK_CONSENSUS` block is marked failed (dashd lists it as an `invalid` chain tip), a
//! `BLOCK_MUTATED` one is never stored (dashd does not know its header). Both start on the same
//! chain and mock time. Each block is V's template for the height (MN0-1's, byte for byte
//! dashd's) with its coinbase replaced, so only the coinbase decides: version 3 with a CbTx
//! payload (versions 0 to 5, a wrong height, a non-zero root, a payload not read to its end, a
//! non-zero asset-unlock root), or version 1 or 2 with type bits 5 and no payload field.
//! Regtest's DIP0008 is active from block 1, so a version-1 CbTx passes in block 1 and not in
//! block 2 (quirk 16). Plain template blocks lead to height 30, where `llmq_test_dip0024`'s
//! rotation check reads an ancestor below the cycle base 24 as step 9 runs (Opus r1 n-2).
//!
//! Needs dashd v24.0.0-rc.4: `DASHD_V24_PATH`, else setup-dashd's cache
//! (`DASHVERSION=24.0.0-rc.4 python3 contrib/setup-dashd.py`). `SKIP_DASHD_TESTS` skips it.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::primitives::{
    Header, OutPoint, Transaction, TxIn, core_serialize, decode_block,
};
use dash_consensus::{Block, BlockValidationResult, Tx};
use dash_primitives::{BlockHash, TxHash, TxType};
use dashd_chainstate::chain::{BlockState, ChainHandle, ValidationInterface};
use dashd_mining::{BlockTemplateSource, ChainTemplateSource, Grind, TemplateTxs, grind};
use dashd_node::logging::Logger;
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;
use serde_json::{Value, json};

const CORE_VERSION: &str = "v24.0.0-rc.4";
const T0: i64 = 1_700_000_000;
/// rpc_generate's regtest P2PKH address and its script.
const P2PKH: &str = "ycwedq2f3sz2Yf9JqZsBCQPxp18WU3Hp4J";

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
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("dashd starts");
        let mut dashd = Dashd {
            child,
            rpc_port,
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        // Up once a call succeeds: during warmup dashd answers RPC_IN_WARMUP (-28).
        while !dashd.reply("getblockcount", json!([])).is_some_and(|r| r["error"].is_null()) {
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

    fn call(&self, method: &str, params: Value) -> Value {
        let reply = self.reply(method, params.clone()).expect("a reply");
        assert!(reply["error"].is_null(), "dashd {method} {params}: {}", reply["error"]);
        reply["result"].clone()
    }
}

impl Drop for Dashd {
    fn drop(&mut self) {
        let _ = self.reply("stop", json!([]));
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

/// `submitblock_StateCatcher` on V's signals: the state of each block checked, by hash.
#[derive(Default)]
struct Checked(Mutex<Vec<(BlockHash, BlockState)>>);

impl ValidationInterface for Checked {
    fn block_checked(&self, block: &Block, state: &BlockState, _ibd: bool) {
        self.0.lock().unwrap().push((block.hash(), state.clone()));
    }
}

/// `submitblock`'s answer for a checked block (`rpc/mining.cpp`'s `BIP22ValidationResult`):
/// null when valid, else the reject reason.
fn bip22(state: &BlockState) -> Value {
    match state {
        BlockState::Valid => Value::Null,
        BlockState::Invalid(e) => json!(e.reject_reason()),
        BlockState::Error(msg) => json!(msg),
    }
}

/// Both nodes of a test on one temp dir. Fields drop in order: the SUT, then dashd, then the dir.
struct Harness {
    sut: NodeSut,
    dashd: Dashd,
    _dir: tempfile::TempDir,
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
        let sut = NodeSut::new(&sut_dir, &["-regtest", "-printtoconsole=0"], T0, ui)
            .expect("the SUT starts");
        Some(Harness {
            sut,
            dashd,
            _dir: dir,
        })
    }
}

/// The bytes of a CbTx of `version` at `height` with the given roots, a null ChainLock, no
/// balance and a zero asset-unlock root (`cbtx.h:54-67`): the fields its version has.
fn cbtx(version: u16, height: u32, mn_root: u8, quorum_root: u8) -> Vec<u8> {
    let mut bytes = [&version.to_le_bytes()[..], &height.to_le_bytes(), &[mn_root; 32]].concat();
    if version >= 2 {
        bytes.extend([quorum_root; 32]);
    }
    if version >= 3 {
        bytes.push(0);
        bytes.extend([0; 96]);
        bytes.extend(0i64.to_le_bytes());
    }
    if version >= 4 {
        bytes.extend([0; 32]);
    }
    bytes
}

/// What replaces the template's coinbase.
#[derive(Clone)]
enum Coinbase {
    /// The template's own coinbase: a plain block.
    Template,
    /// Version `.0` with type bits 5 and payload `.1` (none below version 3).
    Typed(i16, Vec<u8>),
    /// The template's coinbase, then a version-3 type-5 transaction that is no coinbase.
    SecondCbTx,
}

/// One block on both nodes.
struct Case {
    name: &'static str,
    coinbase: Coinbase,
    /// dashd's answer, which V must give.
    want: Value,
    /// The reject's class: `None` when valid.
    class: Option<BlockValidationResult>,
}

/// A valid block (`want` `None`) or a `BLOCK_CONSENSUS` reject, which marks it failed.
fn case(name: &'static str, coinbase: Coinbase, want: Option<&str>) -> Case {
    Case {
        name,
        coinbase,
        want: want.map_or(Value::Null, |r| json!(r)),
        class: want.map(|_| BlockValidationResult::Consensus),
    }
}

/// A `BLOCK_MUTATED` reject from `CheckBlock`: never stored, never marked failed.
fn mutated(name: &'static str, coinbase: Coinbase, want: &str) -> Case {
    Case {
        name,
        coinbase,
        want: json!(want),
        class: Some(BlockValidationResult::Mutated),
    }
}

/// `template` with its coinbase replaced as `coinbase` says, ground on `pow`.
fn block_of(template: &Block, coinbase: &Coinbase, pow: &dash_consensus::pow::PowParams) -> Block {
    let mut txs = template.txs().to_vec();
    let mut cb = txs[0].to_mutable();
    match coinbase {
        Coinbase::Template => {}
        Coinbase::Typed(version, payload) => {
            cb.version = *version;
            cb.tx_type = TxType::new(TRANSACTION_COINBASE);
            cb.extra_payload = payload.clone();
            txs[0] = Arc::new(Tx::new(cb));
        }
        Coinbase::SecondCbTx => {
            let spend = Transaction {
                version: 3,
                tx_type: TxType::new(TRANSACTION_COINBASE),
                inputs: vec![TxIn {
                    prevout: OutPoint {
                        hash: TxHash::from([7; 32]),
                        index: 0,
                    },
                    script_sig: vec![0x51].into(),
                    sequence: u32::MAX,
                }],
                outputs: cb.outputs.clone(),
                lock_time: 0,
                extra_payload: cbtx(2, 0, 0, 0),
            };
            txs.push(Arc::new(Tx::new(spend)));
        }
    }
    let mut block = Block::new(Header::new(*template.header().raw()), txs);
    let mut tries = 1_000_000;
    assert_eq!(grind(&mut block, pow, &mut tries, &|| false), Grind::Found);
    block
}

/// One node's verdict on a block: `submitblock`'s answer and the reject's class.
#[derive(Debug, PartialEq)]
struct Verdict {
    answer: Value,
    class: Option<BlockValidationResult>,
}

/// Sends `block`'s bytes to both nodes: dashd's `submitblock` answer, V's `BlockChecked` state,
/// and each side's reject class. dashd's class is read from what it kept: a block marked failed
/// is an `invalid` chain tip (`BLOCK_CONSENSUS` here, every check after `CheckBlock`), a block it
/// never stored has no header (`BLOCK_MUTATED`, `CheckBlock`'s).
fn submit_both(h: &Harness, chain: &ChainHandle, checked: &Checked, block: &Block) -> [Verdict; 2] {
    let bytes = core_serialize(block);
    let core = h.dashd.call("submitblock", json!([hex(&bytes)]));
    let hash = block.hash();
    let core_class = if core.is_null() {
        None
    } else if h.dashd.reply("getblockheader", json!([hash.to_string()])).expect("a reply")["error"]
        ["code"]
        == json!(-5)
    {
        Some(BlockValidationResult::Mutated)
    } else {
        let tips = h.dashd.call("getchaintips", json!([]));
        let tip = tips.as_array().unwrap().iter().find(|t| t["hash"] == json!(hash.to_string()));
        assert_eq!(
            tip.map(|t| t["status"].clone()),
            Some(json!("invalid")),
            "dashd marks {hash} failed"
        );
        Some(BlockValidationResult::Consensus)
    };
    let mut input = bytes.as_slice();
    let ours = decode_block(&mut input).expect("V decodes dashd's bytes").into_value();
    assert!(input.is_empty() && ours.hash() == hash, "the same block");
    let r = chain.process_block(Arc::new(ours), true, None, true).recv().expect("V");
    assert!(h.sut.sync_signals(), "V's signal queue drained");
    let seen = checked.0.lock().unwrap();
    let state = seen.iter().rev().find(|(b, _)| *b == hash).map(|(_, s)| s.clone());
    let state = state.expect("V checked the block");
    let ours_class = match &state {
        BlockState::Invalid(e) => Some(e.result()),
        _ => None,
    };
    // `CheckBlock` and `AcceptBlock` pass but for `CheckBlock`'s mutated reject.
    assert_eq!(r.accepted, ours_class != Some(BlockValidationResult::Mutated), "{state:?}");
    [
        Verdict {
            answer: core,
            class: core_class,
        },
        Verdict {
            answer: bip22(&state),
            class: ours_class,
        },
    ]
}

/// Under `sut::`, the gate's feature-step filter (`[package.metadata.gate]`).
mod sut {
    use super::*;

    #[test]
    fn pre_dip3_cbtx_verdicts_equal_dashds() {
        let Some(h) = Harness::start() else {
            return;
        };
        let checked = Arc::new(Checked::default());
        assert!(h.sut.register_listener(checked.clone()));
        let chain = h.sut.chain().expect("V runs").clone();
        let clock: Arc<dyn dash_clock::Clock> = h.sut.clock().clone();
        let params = Arc::new(h.sut.params().clone());
        let source = ChainTemplateSource::new(chain.clone(), params, clock, None);
        let pow = *HeaderParams::from_chain_params(h.sut.params()).unwrap().pow();
        let script = unhex(
            h.dashd.call("validateaddress", json!([P2PKH]))["scriptPubKey"].as_str().unwrap(),
        );
        let typed = Coinbase::Typed;
        let mut unlock_root = cbtx(4, 2, 0, 0);
        let n = unlock_root.len();
        unlock_root[n - 32..].fill(1);
        let heights: [(u32, Vec<Case>); 4] = [
            (
                1,
                vec![
                    case("v1 mn root", typed(3, cbtx(1, 1, 1, 0)), Some("bad-cbtx-mnmerkleroot")),
                    case("v1 in block 1", typed(3, cbtx(1, 1, 0, 0)), None),
                ],
            ),
            (
                2,
                vec![
                    case("v1 after DIP0008", typed(3, cbtx(1, 2, 0, 0)), Some("bad-cbtx-version")),
                    case("v0", typed(3, cbtx(0, 2, 0, 0)), Some("bad-cbtx-version")),
                    case("v3 before V20", typed(3, cbtx(3, 2, 0, 0)), Some("bad-cbtx-version")),
                    case("v4 before V20", typed(3, cbtx(4, 2, 0, 0)), Some("bad-cbtx-version")),
                    case("v5 unknown", typed(3, cbtx(5, 2, 0, 0)), Some("bad-cbtx-version")),
                    mutated(
                        "v4 unlock root",
                        typed(3, unlock_root),
                        "bad-cbtx-assetunlockmerkleroot",
                    ),
                    case("height", typed(3, cbtx(2, 3, 0, 0)), Some("bad-cbtx-height")),
                    case("mn root", typed(3, cbtx(2, 2, 1, 0)), Some("bad-cbtx-mnmerkleroot")),
                    case(
                        "quorum root",
                        typed(3, cbtx(2, 2, 0, 1)),
                        Some("bad-cbtx-quorummerkleroot"),
                    ),
                    case("both roots", typed(3, cbtx(2, 2, 1, 1)), Some("bad-cbtx-mnmerkleroot")),
                    case(
                        "trailing byte",
                        typed(3, [cbtx(2, 2, 0, 0), vec![0]].concat()),
                        Some("bad-cbtx-payload"),
                    ),
                    case(
                        "short",
                        typed(3, cbtx(2, 2, 0, 0)[..69].to_vec()),
                        Some("bad-cbtx-payload"),
                    ),
                    case("no payload", typed(3, Vec::new()), Some("bad-cbtx-payload")),
                    case("v2 in block 2", typed(3, cbtx(2, 2, 0, 0)), None),
                ],
            ),
            (
                3,
                vec![
                    case("tx v1 type 5", typed(1, Vec::new()), Some("bad-cbtx-payload")),
                    case("tx v2 type 5", typed(2, Vec::new()), Some("bad-cbtx-payload")),
                    case("second CbTx", Coinbase::SecondCbTx, Some("bad-tx-type-dip3-inactive")),
                    case("v2 in block 3", typed(3, cbtx(2, 3, 0, 0)), None),
                ],
            ),
            (
                30,
                vec![
                    case(
                        "quorum root at 30",
                        typed(3, cbtx(2, 30, 0, 1)),
                        Some("bad-cbtx-quorummerkleroot"),
                    ),
                    case(
                        "mn root at 30",
                        typed(3, cbtx(2, 30, 1, 0)),
                        Some("bad-cbtx-mnmerkleroot"),
                    ),
                    case("v2 in block 30", typed(3, cbtx(2, 30, 0, 0)), None),
                ],
            ),
        ];
        let mut time = T0;
        let mut template_at = |height: u32| {
            time += 150;
            h.dashd.call("setmocktime", json!([time]));
            h.sut.clock().set_mocktime(time);
            let template = source.create_new_block(&script, TemplateTxs::None).expect("a template");
            assert_eq!(template.height, height);
            template.block
        };
        let mut tip = 0;
        for (height, cases) in heights {
            // Plain template blocks up to the height.
            for plain in tip + 1..height {
                let block = block_of(&template_at(plain), &Coinbase::Template, &pow);
                let [core, ours] = submit_both(&h, &chain, &checked, &block);
                assert_eq!([core.answer, ours.answer], [Value::Null, Value::Null], "plain {plain}");
            }
            tip = height;
            let template = template_at(height);
            for c in cases {
                let block = block_of(&template, &c.coinbase, &pow);
                let [core, ours] = submit_both(&h, &chain, &checked, &block);
                let want = Verdict {
                    answer: c.want,
                    class: c.class,
                };
                assert_eq!(core, want, "dashd, {} at {height}", c.name);
                assert_eq!(ours, want, "V, {} at {height}", c.name);
            }
            let best = h.dashd.call("getbestblockhash", json!([]));
            let ours = chain.view().snap.tip.tip_hash;
            assert_eq!(best, json!(ours.to_string()), "the same tip at {height}");
        }
    }
}
