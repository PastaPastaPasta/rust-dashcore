//! CH5-5's oracle fixtures (§6.3.3): a source rdashd, its full submission log, the expected state
//! at its tip, and twins replayed from the log. Both fixtures stop every node before a test
//! copies anything.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Instant;

use dash_dml::MnList;
use dash_dml::codec::ContextFreeNetInfo;
use dash_primitives::BlockHash;
use dash_testkit::livecopy::read_cf;
use dashd_chainstate::evo::codec::EvoCodec;
use serde_json::{Value, json};

use crate::common::*;

/// The JSON of a 200 reply's result.
pub fn result(reply: &Reply) -> Value {
    serde_json::from_str(reply.result()).unwrap_or_else(|e| panic!("{e}: {}", reply.body))
}

/// `method(params)` on `node`, its result.
pub fn call(node: &RpcNode, method: &str, params: Value) -> Value {
    result(&node.call(method, &params.to_string()))
}

/// A string result.
pub fn text(v: &Value) -> String {
    v.as_str().unwrap_or_else(|| panic!("not a string: {v}")).to_owned()
}

/// A display-order hash's internal bytes.
pub fn internal(hash: &str) -> [u8; 32] {
    let mut bytes = [0; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hash[62 - 2 * i..64 - 2 * i], 16).expect("a hex hash");
    }
    bytes
}

/// A node's home and datadir.
pub struct Datadir {
    pub home: tempfile::TempDir,
    pub datadir: PathBuf,
}

impl Datadir {
    pub fn new() -> Datadir {
        let (home, datadir) = home_and_datadir();
        Datadir {
            home,
            datadir,
        }
    }

    /// A stopped node's copy of this datadir (§6.3.5's copier, never pausing).
    pub fn copy(&self) -> Datadir {
        let to = Datadir::new();
        copier::copy_stopped(&self.datadir, &to.datadir);
        to
    }
}

/// rdashd on `dir` with `extra`, serving the live-copy hooks in `hooks` if given; the output of
/// an exit during init as the error.
pub fn try_rdashd(dir: &Datadir, extra: &[&str], hooks: Option<&Path>) -> Result<RpcNode, Output> {
    try_start_on_free_port(&dir.datadir, free_port, |port| {
        let rpcport = format!("-rpcport={port}");
        let args = [&["-printtoconsole=0", rpcport.as_str()], extra].concat();
        let mut cmd = regtest(dir.home.path(), &dir.datadir, &args);
        if let Some(hooks) = hooks {
            cmd.env("RDASHD_TEST_HOOK", "live-copy").env("RDASHD_TEST_HOOK_DIR", hooks);
        }
        cmd
    })
    .map(|(node, port)| RpcNode {
        node,
        port,
        datadir: dir.datadir.clone(),
    })
    .map_err(|(_, out)| out)
}

/// [`try_rdashd`], failing the test on an exit during init.
pub fn rdashd(dir: &Datadir, extra: &[&str], hooks: Option<&Path>) -> RpcNode {
    try_rdashd(dir, extra, hooks).unwrap_or_else(|out| {
        panic!("rdashd exited during init ({}): {}", out.status, stderr(&out))
    })
}

/// `stop`, then the exit. A node that exited before the stop, or exits other than cleanly, is
/// the error, with its log's `AbortNode` (`*** `) and `Error: ` lines.
pub fn stop_checked(node: RpcNode) -> Result<(), String> {
    let datadir = node.datadir.clone();
    let asked = try_rpc(node.port, &cookie(&datadir), "stop", "[]");
    let out = node.node.wait();
    if asked.is_ok() && out.status.code() == Some(0) {
        return Ok(());
    }
    let log = read(&debug_log(&datadir));
    let errors: Vec<&str> =
        log.lines().filter(|l| l.contains("*** ") || l.contains("Error: ")).collect();
    Err(format!("the node exited {:?} by its stop: {}", out.status.code(), errors.join(" | ")))
}

/// One `submitblock` or `submitheader` the source took, with its answer.
#[derive(Clone, Debug)]
pub struct Submission {
    pub method: &'static str,
    pub hex: String,
    /// The block's hash, display order.
    pub hash: String,
    /// The answer's JSON text.
    pub result: String,
}

impl Submission {
    /// `hash`'s block from `from`, as a `submitblock` the source answered `null`.
    pub fn block(from: &RpcNode, hash: &str) -> Submission {
        Submission {
            method: "submitblock",
            hex: text(&call(from, "getblock", json!([hash, 0]))),
            hash: hash.to_owned(),
            result: "null".into(),
        }
    }

    /// Submits it to `node`, returning the answer's JSON text.
    pub fn to(&self, node: &RpcNode) -> String {
        node.call(self.method, &json!([self.hex]).to_string()).result().to_owned()
    }
}

/// `method(params)` on `node`, its result; a node that does not answer, or answers with an
/// error, is the error.
pub fn try_call(node: &RpcNode, method: &str, params: Value) -> Result<Value, String> {
    let reply = try_rpc(node.port, &cookie(&node.datadir), method, &params.to_string())
        .map_err(|e| format!("{method}: {e}"))?;
    if reply.status != 200 {
        return Err(format!("{method}: {}", reply.body));
    }
    serde_json::from_str(reply.result()).map_err(|e| format!("{method}: {e}"))
}

/// What the source served at its tip (§6.3.2 step 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expect {
    pub tip: String,
    pub height: u64,
    pub hash_serialized_2: String,
    pub txouts: u64,
}

impl Expect {
    /// [`Expect::of`] on a node that may have stopped.
    pub fn try_of(node: &RpcNode) -> Result<Expect, String> {
        let info = try_call(node, "gettxoutsetinfo", json!([]))?;
        Ok(Expect {
            tip: text(&try_call(node, "getbestblockhash", json!([]))?),
            height: try_call(node, "getblockcount", json!([]))?.as_u64().ok_or("a height")?,
            hash_serialized_2: text(&info["hash_serialized_2"]),
            txouts: info["txouts"].as_u64().ok_or("txouts")?,
        })
    }

    pub fn of(node: &RpcNode) -> Expect {
        let info = call(node, "gettxoutsetinfo", json!([]));
        Expect {
            tip: text(&call(node, "getbestblockhash", json!([]))),
            height: call(node, "getblockcount", json!([])).as_u64().expect("a height"),
            hash_serialized_2: text(&info["hash_serialized_2"]),
            txouts: info["txouts"].as_u64().expect("txouts"),
        }
    }
}

/// A built fixture: the stopped source, its log, its tip's state, and the block that continues
/// it (§6.3.2 step 5), which no node of the fixture has.
pub struct Fixture {
    pub source: Datadir,
    pub log: Vec<Submission>,
    pub expect: Expect,
    pub extra: &'static [&'static str],
    /// The block after the tip, from the generator; `None` where the copy mines it (plain).
    pub next: Option<Submission>,
    /// Block hashes by height (0 is genesis).
    pub hashes: Vec<String>,
    /// The stopped twin at the tip (§6.3.2 step 3), built once; checks run on copies of it.
    pub twin: Datadir,
}

/// A twin (§6.3.2 step 3): a fresh rdashd that replays `log` up to and including the
/// submission of `tip`, each with the answer the source gave, then stops cleanly. With
/// `flush_each`, a scripted flush (the `flush` hook) follows every submission.
pub fn twin(log: &[Submission], tip: &str, extra: &[&str], flush_each: bool) -> Datadir {
    let dir = Datadir::new();
    let hooks = tempfile::tempdir().unwrap();
    let node = rdashd(&dir, extra, flush_each.then_some(hooks.path()));
    let end = log.iter().position(|s| s.hash == tip).expect("the tip is in the log");
    for s in &log[..=end] {
        assert_eq!(s.to(&node), s.result, "the twin's {} of {}", s.method, s.hash);
        if flush_each {
            let flush = ask(hooks.path(), "flush", "");
            assert!(flush.starts_with("steps="), "{flush}");
        }
    }
    node.stop();
    dir
}

/// The plain fixture: rdashd mines 30 blocks to `raw(51)`.
pub fn plain() -> Fixture {
    let start = Instant::now();
    let source = Datadir::new();
    let node = rdashd(&source, &[], None);
    let genesis = text(&call(&node, "getblockhash", json!([0])));
    let mined = call(&node, "generatetodescriptor", json!([30, "raw(51)"]));
    let mined: Vec<String> = mined.as_array().expect("hashes").iter().map(text).collect();
    let log: Vec<Submission> = mined.iter().map(|h| Submission::block(&node, h)).collect();
    let expect = Expect::of(&node);
    assert_eq!(expect.height, 30);
    node.stop();
    let twin = twin(&log, &expect.tip, &[], false);
    let fixture = Fixture {
        source,
        log,
        expect,
        extra: &[],
        next: None,
        hashes: [vec![genesis], mined].concat(),
        twin,
    };
    assert_non_empty(&fixture.source.datadir, &["meta", "block_index", "block_files", "coins"]);
    eprintln!("CH5-5 plain fixture built in {:.1?}", start.elapsed());
    fixture
}

/// The column families `cfs` hold rows in `datadir`.
fn assert_non_empty(datadir: &Path, cfs: &[&str]) {
    for cf in cfs {
        assert!(!read_cf(datadir, cf).expect("rows").is_empty(), "{cf} is empty");
    }
}

/// The evo fixture's heights (OD-CH5-23), derived from regtest's LLMQ parameters: the fixture's
/// DIP3 chain lies between two rotation cycle bases, as G1q required before CH7b-2 (T92) removed
/// it; BL-645 restores D2-3 as designed (OD-CH5-30). DIP3 starts after the first cycle base `c0`
/// at or above [`FUNDED`], and the tip `t` is two below the next, `c1`: the continuation block is
/// `c1 − 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heights {
    pub dip3: u64,
    pub registered: u64,
    pub updated: u64,
    pub t: u64,
    pub next: u64,
    pub c1: u64,
}

/// Where the wallet holds the collateral: the coinbases of heights 1–100 have matured.
pub const FUNDED: u64 = 200;

pub fn heights() -> Heights {
    use dash_params::{ChainParamsBuilder, Network, Release};
    let params =
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest");
    let intervals: Vec<u64> = params
        .consensus
        .llmq_params()
        .filter(|(_, p)| p.use_rotation)
        .map(|(_, p)| u64::from(p.dkg_interval))
        .collect();
    let interval = intervals[0];
    assert!(intervals.iter().all(|&i| i == interval), "one rotation interval: {intervals:?}");
    let c0 = FUNDED.div_ceil(interval) * interval;
    let c1 = c0 + interval;
    let h = Heights {
        dip3: c0 + 1,
        registered: c0 + 2,
        updated: c0 + 3,
        t: c1 - 2,
        next: c1 - 1,
        c1,
    };
    assert_eq!((h.dip3, h.t, h.next, h.c1), (217, 238, 239, 240), "{h:?}");
    h
}

/// `-dip3params=<dip3>:<dip3>`, for every node of the evo fixture.
fn dip3_arg(h: &Heights) -> &'static [&'static str] {
    let arg: &'static str = Box::leak(format!("-dip3params={0}:{0}", h.dip3).into_boxed_str());
    Box::leak(vec![arg].into_boxed_slice())
}

/// The evo fixture's facts the tests read.
pub struct Evo {
    pub fixture: Fixture,
    pub heights: Heights,
    /// The masternode's P2P port.
    pub port: u16,
    pub pro_tx: String,
}

/// The node's evo codec for decoding only.
pub fn codec() -> EvoCodec {
    EvoCodec::new(std::sync::Arc::new(ContextFreeNetInfo))
}

/// The row of `hash` in `cf` of `datadir`.
pub fn row(datadir: &Path, cf: &str, hash: &str) -> Vec<u8> {
    let key = internal(hash);
    let rows = read_cf(datadir, cf).expect("rows");
    rows.into_iter().find(|(k, _)| k[..] == key[..]).unwrap_or_else(|| panic!("no {cf} row")).1
}

/// The `mn_snapshot` row of `hash` in `datadir`, decoded.
pub fn snapshot(datadir: &Path, hash: &str) -> std::sync::Arc<MnList> {
    let value = row(datadir, "mn_snapshot", hash);
    codec().decode_snapshot(&value, &BlockHash::from(internal(hash))).expect("the snapshot decodes")
}

/// The list at `heights.t` built from `datadir`'s rows as the node resolves it
/// (`dash-dml/src/store/resolve.rs:166-223`): the snapshot at the first DIP3 block, then each
/// later block's `mn_diff` record.
pub fn built_list(datadir: &Path, hashes: &[String], heights: &Heights) -> Result<MnList, String> {
    let mut list = (*snapshot(datadir, &hashes[heights.dip3 as usize])).clone();
    for hash in &hashes[heights.dip3 as usize + 1..=heights.t as usize] {
        let record =
            codec().decode_diff(&row(datadir, "mn_diff", hash)).map_err(|e| e.to_string())?;
        list.apply_record(BlockHash::from(internal(hash)), &record)
            .map_err(|e| format!("mn_diff row for block {hash}: {e}"))?;
    }
    Ok(list)
}

/// The evo fixture (D2-3 in OD-CH5-23's form): dashd, with its wallet, generates the chain, and
/// the source rdashd gets every block by `submitblock` from the log. The wallet is funded before
/// DIP3; the first DIP3 block stores an empty list; a masternode registers in the next block and
/// updates its service in the one after; the chain ends at [`Heights::t`]. Dashd's next block is
/// kept for the continuation, and a throwaway copy takes it.
pub fn evo() -> Evo {
    let start = Instant::now();
    let h = heights();
    let extra = dip3_arg(&h);
    let core_dir = Datadir::new();
    let dashd = require_dashd();
    let core = RpcNode::start_on(
        &dashd,
        core_dir.home.path(),
        &core_dir.datadir,
        &[extra[0], "-fallbackfee=0.0002"],
    );
    let source = Datadir::new();
    let node = rdashd(&source, extra, None);
    let genesis = text(&call(&node, "getblockhash", json!([0])));
    let mut log: Vec<Submission> = Vec::new();
    let relay = |log: &mut Vec<Submission>, n: u64, to: &str| {
        for hash in call(&core, "generatetoaddress", json!([n, to])).as_array().expect("hashes") {
            let s = Submission::block(&core, &text(hash));
            assert_eq!(s.to(&node), "null", "the source's submitblock of {}", s.hash);
            log.push(s);
        }
        log.len() as u64
    };
    // 1. The wallet.
    call(&core, "createwallet", json!(["w"]));
    let [fund, collateral, owner, voting, payout] =
        ["fund", "collateral", "owner", "voting", "payout"]
            .map(|label| text(&call(&core, "getnewaddress", json!([label]))));
    // 2. Funding, before DIP3; 3. the first DIP3 block.
    relay(&mut log, h.dip3 - 1, &fund);
    let balance = call(&core, "getbalance", json!([])).as_f64().expect("a balance");
    assert!(balance >= 1001.0, "{balance} DASH at {}", h.dip3 - 1);
    assert_eq!(relay(&mut log, 1, &fund), h.dip3);
    // 4. Registration.
    let bls = call(&core, "bls", json!(["generate"]));
    let port = free_port();
    let pro_tx = text(&call(
        &core,
        "protx",
        json!([
            "register_fund",
            collateral,
            format!("127.0.0.1:{port}"),
            owner,
            bls["public"],
            voting,
            "0",
            payout,
            fund
        ]),
    ));
    assert_eq!(relay(&mut log, 1, &fund), h.registered);
    // 5. The service update.
    call(
        &core,
        "protx",
        json!(["update_service", pro_tx, format!("127.0.0.2:{port}"), bls["secret"], "", fund]),
    );
    assert_eq!(relay(&mut log, 1, &fund), h.updated);
    // 6. Through the tip.
    assert_eq!(relay(&mut log, h.t - h.updated, &fund), h.t);
    let expect = Expect::of(&node);
    assert_eq!(expect.height, h.t);
    node.stop();
    // The continuation, which neither rdashd of the fixture gets.
    let hash = call(&core, "generatetoaddress", json!([1, fund]));
    let next = Submission::block(&core, &text(&hash[0]));
    assert_eq!(core.call("stop", "[]").result(), r#""Dash Core stopping""#);
    core.node.wait();
    let probe = source.copy();
    let on_probe = rdashd(&probe, extra, None);
    assert_eq!(next.to(&on_probe), "null", "the copy takes {}", h.next);
    on_probe.stop();
    let hashes = [vec![genesis], log.iter().map(|s| s.hash.clone()).collect()].concat();
    let twin = twin(&log, &expect.tip, extra, false);
    let evo = Evo {
        fixture: Fixture {
            source,
            log,
            expect,
            extra,
            next: Some(next),
            hashes,
            twin,
        },
        heights: h,
        port,
        pro_tx,
    };
    evo.assert_rows();
    eprintln!("CH5-5 evo fixture built in {:.1?}", start.elapsed());
    evo
}

impl Evo {
    /// §6.3.3 steps 3, 4 and 7 (OD-CH5-23's form) on the stopped source's rows.
    fn assert_rows(&self) {
        let (dir, hashes, h) = (&self.fixture.source.datadir, &self.fixture.hashes, &self.heights);
        assert_non_empty(dir, &["meta", "block_index", "block_files", "coins"]);
        // 3. The first DIP3 block's snapshot, the only one, is an explicit empty list.
        let snapshots = read_cf(dir, "mn_snapshot").expect("mn_snapshot");
        assert_eq!(snapshots.len(), 1, "one snapshot row, at the first DIP3 block");
        assert!(snapshot(dir, &hashes[h.dip3 as usize]).is_empty(), "the first DIP3 list");
        // 7. One mn_diff row per DIP3 block.
        let diffs = read_cf(dir, "mn_diff").expect("mn_diff");
        assert_eq!(diffs.len() as u64, h.t - h.dip3 + 1, "one mn_diff row per DIP3 block");
        let diff_at = |height: u64| {
            codec().decode_diff(&row(dir, "mn_diff", &hashes[height as usize])).expect("decodes")
        };
        assert_eq!(diff_at(h.registered).diff.added.len(), 1, "the registration adds one entry");
        let upd = diff_at(h.updated);
        assert_eq!(upd.diff.updated.len(), 1, "the update changes one entry");
        assert_ne!(
            upd.diff.updated[0].1.fields & dash_dml::MnStateDiff::NET_INFO,
            0,
            "its service"
        );
        // The list built at the tip: exactly the registered entry, at its updated service.
        let list = built_list(dir, hashes, h).expect("the list at the tip");
        assert_eq!(list.len(), 1, "the list at the tip");
        let mn = list.iter().next().expect("the entry");
        assert_eq!(mn.pro_tx_hash.as_bytes(), &internal(&self.pro_tx), "the registered entry");
        let service = dash_dml::netinfo::primary(&mn.cold.net_info).to_string();
        assert_eq!(service, format!("127.0.0.2:{}", self.port));
        for cf in ["credit_pool", "mnhf", "llmq_mined", "llmq_mined_h", "llmq_snapshot"] {
            let n = read_cf(dir, cf).expect("rows").len();
            eprintln!("CH5-5 evo fixture: {cf} holds {n} rows (before V20; no quorum forms)");
        }
    }
}
