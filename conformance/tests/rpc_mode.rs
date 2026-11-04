//! End-to-end RPC mode through the `dash-conformance` binary, against a mock oracle serving a
//! synthetic regtest chain: the exit status of a gate (fresh, resumed, after a policy change,
//! after a reorg), range validation, a chain that changes during a unit, and reorgs between
//! the reads that make up a run (review r2 finding 1).

mod common;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use common::block;
use dashcore::BlockHash;
use dashcore::block::Block;
use dashcore::consensus::serialize;
use dashcore::hashes::Hash;
use hex_conservative::DisplayHex;
use serde_json::{Value, json};

#[derive(Default)]
struct State {
    chain: Vec<Block>,
    /// Blocks reachable by hash only (another branch).
    extra: Vec<Block>,
    /// `getblock 1` reports this height's time one second late.
    tamper_time: Option<u32>,
    /// `getblockhash` answers with this block for its height.
    fork: Option<(u32, Block)>,
    /// What `getnetworkinfo` reports as the version.
    version: u64,
    calls: HashMap<String, usize>,
    /// After answering `getblockhash` for this height, the oracle switches to this chain.
    reorg_after_getblockhash: Option<(u32, Vec<Block>)>,
    /// After this many `getblock` calls in total, the oracle switches to this chain.
    reorg_after_getblocks: Option<(usize, Vec<Block>)>,
    getblocks_total: usize,
}

impl State {
    /// A known block and its height.
    fn by_hash(&self, hash: &str) -> Option<(usize, &Block)> {
        if let Some((h, b)) = &self.fork
            && b.block_hash().to_string() == hash
        {
            return Some((*h as usize, b));
        }
        if let Some(found) =
            self.chain.iter().enumerate().find(|(_, b)| b.block_hash().to_string() == hash)
        {
            return Some(found);
        }
        let b = self.extra.iter().find(|b| b.block_hash().to_string() == hash)?;
        let parent = self.by_hash(&b.header.prev_blockhash.to_string())?.0;
        Some((parent + 1, b))
    }

    /// Makes `chain` the oracle's chain (a reorg, or growth when it extends the old one). The
    /// blocks it drops stay known by hash, as they do in dashd.
    fn switch_to(&mut self, chain: Vec<Block>) {
        let old = std::mem::replace(&mut self.chain, chain);
        let kept: Vec<BlockHash> = self.chain.iter().map(Block::block_hash).collect();
        self.extra.extend(old.into_iter().filter(|b| !kept.contains(&b.block_hash())));
    }

    fn reply(&mut self, method: &str, params: &Value) -> Value {
        let out = self.answer(method, params);
        if method == "getblockhash"
            && let Some((_, chain)) = self
                .reorg_after_getblockhash
                .take_if(|(h, _)| params[0].as_u64() == Some(u64::from(*h)))
        {
            self.switch_to(chain);
        }
        if method == "getblock" {
            self.getblocks_total += 1;
            let total = self.getblocks_total;
            if let Some((_, chain)) = self.reorg_after_getblocks.take_if(|(n, _)| *n == total) {
                self.switch_to(chain);
            }
        }
        out
    }

    fn answer(&mut self, method: &str, params: &Value) -> Value {
        *self.calls.entry(method.to_string()).or_default() += 1;
        match method {
            "getnetworkinfo" => json!({"version": self.version, "subversion": "/mock/"}),
            "getbestblockhash" => json!(self.chain.last().unwrap().block_hash().to_string()),
            "getblockhash" => {
                let h = params[0].as_u64().unwrap() as u32;
                match &self.fork {
                    Some((fh, b)) if *fh == h => json!(b.block_hash().to_string()),
                    _ => json!(self.chain[h as usize].block_hash().to_string()),
                }
            }
            "getblockheader" => {
                let hash = params[0].as_str().unwrap();
                json!({"hash": hash, "height": self.by_hash(hash).expect("known block").0})
            }
            "getblock" => {
                let (height, b) = self.by_hash(params[0].as_str().unwrap()).expect("known block");
                match params[1].as_u64().unwrap() {
                    0 => json!(serialize(b).to_lower_hex_string()),
                    1 => {
                        let mut time = b.header.time;
                        if self.tamper_time == Some(height as u32) {
                            time += 1;
                        }
                        let mut v = json!({
                            "hash": b.block_hash().to_string(),
                            "height": height,
                            "version": b.header.version.to_consensus(),
                            "merkleroot": b.header.merkle_root.to_string(),
                            "time": time,
                            "nonce": b.header.nonce,
                            "bits": format!("{:08x}", b.header.bits.to_consensus()),
                            "tx": b.txdata.iter().map(|t| t.txid().to_string()).collect::<Vec<_>>(),
                        });
                        if height > 0 {
                            v["previousblockhash"] = json!(b.header.prev_blockhash.to_string());
                        }
                        v
                    }
                    v => panic!("getblock verbosity {v} not mocked"),
                }
            }
            m => panic!("{m} not mocked"),
        }
    }
}

/// Serves JSON-RPC (single and batch requests) from `state` on a local port.
fn serve(state: Arc<Mutex<State>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let req: Value = serde_json::from_slice(&body).unwrap();
            let mut st = state.lock().unwrap();
            let mut one = |r: &Value| {
                let result = st.reply(r["method"].as_str().unwrap(), &r["params"]);
                json!({"id": r["id"], "error": null, "result": result})
            };
            let out = match &req {
                Value::Array(reqs) => Value::Array(reqs.iter().map(&mut one).collect()),
                r => one(r),
            };
            let out = serde_json::to_vec(&out).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                out.len()
            )
            .unwrap();
            stream.write_all(&out).unwrap();
        }
    });
    url
}

/// A chain of `n` blocks from genesis.
fn chain(n: usize) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    extend(&mut out, n, 0);
    out
}

/// Extends `chain` to `n` blocks; `tag` tells the new blocks apart from other branches.
fn extend(chain: &mut Vec<Block>, n: usize, tag: u8) {
    for i in chain.len()..n {
        let prev = chain.last().map_or(BlockHash::all_zeros(), Block::block_hash);
        chain.push(block(prev, tag.wrapping_add(i as u8)));
    }
}

/// `chain` reorganised from `height` up: the same blocks below it, new ones (tagged `tag`)
/// from it to `n`.
fn reorged(chain: &[Block], height: usize, n: usize, tag: u8) -> Vec<Block> {
    let mut out = chain[..height].to_vec();
    extend(&mut out, n, tag);
    out
}

struct Harness {
    url: String,
    state: Arc<Mutex<State>>,
}

impl Harness {
    fn new() -> Self {
        let state = Arc::new(Mutex::new(State {
            chain: chain(25),
            ..Default::default()
        }));
        Harness {
            url: serve(state.clone()),
            state,
        }
    }

    /// Runs the binary; returns its exit code and stderr.
    fn run(&self, out: &Path, args: &[&str]) -> (i32, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_dash-conformance"))
            .args(["rpc", "--net", "regtest", "--rpc-url", &self.url])
            .args(["--rpc-user", "u", "--rpc-password", "p", "--chunk", "10", "--batch", "4"])
            .args(["--verbose-every", "0", "--threads", "2", "--fail-on-mismatch", "--out"])
            .arg(out)
            .args(args)
            .output()
            .unwrap();
        (o.status.code().unwrap(), String::from_utf8_lossy(&o.stderr).into_owned())
    }

    /// `getblock` calls since the last call of this function.
    fn getblocks(&self) -> usize {
        self.state.lock().unwrap().calls.remove("getblock").unwrap_or(0)
    }
}

#[test]
fn gate_reflects_every_unit_in_scope() {
    let h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    h.state.lock().unwrap().tamper_time = Some(3);

    let (code, log) = h.run(&out, &[]);
    assert_eq!(code, 1, "{log}");
    assert_eq!(h.getblocks(), 2 * 25, "getblock 0 and 1 for every block");

    // Review r1 finding 1: resuming checks nothing new but still fails the gate.
    let (code, log) = h.run(&out, &["--resume"]);
    assert_eq!(code, 1, "{log}");
    assert_eq!(h.getblocks(), 0, "{log}");
    assert!(log.contains("gate: 3 units in scope, 0 not covered, 1 failing"), "{log}");

    // A scope without the failing unit passes; the units are whole chunks.
    let (code, log) = h.run(&out, &["--resume", "--from", "12", "--to", "14"]);
    assert_eq!((code, h.getblocks()), (0, 0), "{log}");

    // Review r1 finding 2: another comparison policy re-checks everything.
    let (code, log) = h.run(&out, &["--resume", "--verbose-types", "2"]);
    assert_eq!((code, h.getblocks()), (1, 50), "{log}");

    // A reorg from height 15 invalidates the chunks above it, which are re-checked.
    {
        let mut st = h.state.lock().unwrap();
        st.chain.truncate(15);
        for i in 15..27 {
            let prev = st.chain.last().unwrap().block_hash();
            st.chain.push(block(prev, 100 + i as u8));
        }
    }
    let (code, log) = h.run(&out, &["--resume", "--verbose-types", "2"]);
    assert_eq!(code, 1, "{log}");
    assert_eq!(h.getblocks(), 2 * 17, "rpc:10 and rpc:20 (now to 26) re-checked: {log}");
    let invalidated = std::fs::read_to_string(out.join("invalidated.ndjson")).unwrap();
    assert!(invalidated.contains("\"rpc:10\"") && invalidated.contains("\"rpc:20\""));
    assert!(!invalidated.contains("\"rpc:0\""), "{invalidated}");
    let summary: Value =
        serde_json::from_slice(&std::fs::read(out.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["totals"]["blocks"], 27);
    assert_eq!(summary["mismatches_by_key"]["header.field:time:both"]["count"], 1);

    // Another oracle version judges differently: everything is re-checked.
    h.state.lock().unwrap().version = 240001;
    let (code, log) = h.run(&out, &["--resume", "--verbose-types", "2"]);
    assert_eq!((code, h.getblocks()), (1, 2 * 27), "{log}");

    // A checkpoint is a recorded result: once the oracle stops disagreeing, a resumed gate
    // keeps failing until the unit is re-checked.
    h.state.lock().unwrap().tamper_time = None;
    let (code, _) = h.run(&out, &["--resume", "--verbose-types", "2"]);
    assert_eq!(code, 1);
    let (code, log) = h.run(&out, &["--verbose-types", "2", "--from", "0", "--to", "9"]);
    assert_eq!(code, 0, "{log}");
}

#[test]
fn ranges_are_validated() {
    let h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    // Review r1 finding 7: an inverted range is an error, before anything is written.
    let out = dir.path().join("inverted");
    let (code, log) = h.run(&out, &["--from", "7", "--to", "6"]);
    assert_eq!(code, 2, "{log}");
    assert!(!out.exists());
    let beyond = dir.path().join("beyond");
    let (code, log) = h.run(&beyond, &["--to", "25"]);
    assert_eq!(code, 2, "{log}");
    assert!(log.contains("above the oracle's tip"), "{log}");
    assert!(!beyond.exists(), "rejected before the output directory is created");
}

#[test]
fn chain_change_during_a_unit_fails_it() {
    let h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    {
        // getblockhash 12 answers with a block of another branch.
        let mut st = h.state.lock().unwrap();
        let side = block(st.chain[5].block_hash(), 200);
        let fork = block(side.block_hash(), 201);
        st.extra.push(side);
        st.fork = Some((12, fork));
    }
    let (code, log) = h.run(&dir.path().join("out"), &[]);
    assert_eq!(code, 2, "{log}");
    assert!(log.contains("does not link"), "{log}");
    assert!(log.contains("not covered: rpc:10"), "{log}");
}

/// Review r2 finding 1: a reorg between the reads of the scope's boundary hashes. The `rpc:0`
/// checkpoint of chain A matches the first read (height 9); the oracle then reorganises from
/// height 1 to chain B, whose block 3 disagrees with the libraries, and answers the rest on B.
/// The resumed run re-uses A's `rpc:0` and checks only B's `rpc:10`: that mixes two chains, and
/// B's block 3 was never checked. The gate must not pass. Once the chain is stable, resuming
/// re-checks `rpc:0` and agrees with a fresh run over B.
#[test]
fn reorg_between_boundary_reads_cannot_pass_a_resumed_gate() {
    let h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let (code, log) = h.run(&out, &["--to", "9"]);
    assert_eq!(code, 0, "{log}");
    h.getblocks();
    {
        let mut st = h.state.lock().unwrap();
        let b = reorged(&st.chain, 1, 25, 100);
        st.reorg_after_getblockhash = Some((9, b));
        st.tamper_time = Some(3);
    }

    let (code, log) = h.run(&out, &["--resume", "--to", "19"]);
    eprintln!("--- resumed run across the reorg ---\n{log}");
    assert_eq!(code, 2, "a resumed gate over two chains must fail closed: {log}");
    assert_eq!(h.getblocks(), 20, "only rpc:10 was checked: {log}");
    assert!(log.contains("NOT one chain ending in block 19"), "{log}");
    assert!(log.contains("not one chain: rpc:10: a result of run"), "ends off the pin: {log}");
    assert!(log.contains("not one chain: the oracle's block at height 19 was"), "{log}");

    // The chain is stable now: rpc:0 no longer holds and is re-checked; rpc:10 (checked on B)
    // is re-used.
    let (code, log) = h.run(&out, &["--resume", "--to", "19"]);
    eprintln!("--- resumed again on a stable chain ---\n{log}");
    assert_eq!(code, 1, "{log}");
    assert_eq!(h.getblocks(), 20, "rpc:0 re-checked: {log}");
    assert!(log.contains("header.field:time:both"), "{log}");

    let (code, log) = h.run(&dir.path().join("fresh"), &["--to", "19"]);
    assert_eq!(code, 1, "a fresh run over B fails the same way: {log}");
    assert!(log.contains("header.field:time:both"), "{log}");
}

/// A reorg after a run checked its blocks (here, after the last `getblock` of a run over the
/// whole chain) leaves results that no longer describe the oracle's chain: the gate fails
/// closed, deep or shallow. Ordinary growth of the tip during the run does not.
#[test]
fn reorg_during_a_run_fails_it_but_tip_growth_does_not() {
    let runs = [("growth", None, 0), ("shallow", Some(20), 2), ("deep", Some(5), 2)];
    for (name, reorg_from, expected) in runs {
        let h = Harness::new();
        let dir = tempfile::tempdir().unwrap();
        {
            let mut st = h.state.lock().unwrap();
            let next = match reorg_from {
                None => reorged(&st.chain, st.chain.len(), 30, 0),
                Some(from) => reorged(&st.chain, from, 25, 100),
            };
            st.reorg_after_getblocks = Some((2 * 25, next));
        }
        let (code, log) = h.run(&dir.path().join("out"), &[]);
        eprintln!("--- {name} ---\n{log}");
        assert_eq!(code, expected, "{name}: {log}");
        assert_eq!(h.getblocks(), 2 * 25, "{name}: every block checked once: {log}");
    }
}

/// The same mixture, caught by the links between units alone: the oracle is on chain A when
/// the run pins block 19 and checks `rpc:10`, but answers the one boundary read at height 9
/// from chain C, so `rpc:0`'s checkpoint of chain C is reused. Every other check agrees: the
/// last unit ends in the pinned block, which is still the oracle's at the end.
#[test]
fn units_that_do_not_link_cannot_pass_a_resumed_gate() {
    let h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let (code, log) = h.run(&out, &["--to", "9"]);
    assert_eq!(code, 0, "{log}");
    h.getblocks();
    {
        let mut st = h.state.lock().unwrap();
        let c9 = st.chain[9].clone();
        let a = reorged(&st.chain, 1, 25, 100);
        st.switch_to(a);
        st.fork = Some((9, c9));
        st.tamper_time = Some(3);
    }

    let (code, log) = h.run(&out, &["--resume", "--to", "19"]);
    eprintln!("--- resumed run, boundary read at 9 answered from another chain ---\n{log}");
    assert_eq!(code, 2, "{log}");
    assert_eq!(h.getblocks(), 20, "only rpc:10 was checked: {log}");
    let problems: Vec<&str> = log.lines().filter(|l| l.contains("not one chain:")).collect();
    assert_eq!(problems.len(), 1, "{log}");
    assert!(problems[0].contains("rpc:10: the parent of block 10 is"), "{log}");
    assert!(log.contains("0 not covered, 0 failing mismatches"), "everything else passes: {log}");

    // Once the oracle answers consistently, rpc:0 is re-checked on A and fails.
    h.state.lock().unwrap().fork = None;
    let (code, log) = h.run(&out, &["--resume", "--to", "19"]);
    assert_eq!((code, h.getblocks()), (1, 20), "{log}");
    assert!(log.contains("header.field:time:both"), "{log}");
}
