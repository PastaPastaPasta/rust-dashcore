//! BL-648 T3: the BLS scheme lines of a reorg across V19 on rdashd and dashd v24 side by side,
//! over R1 (`dashd-chainstate/tests/data/v19-window/r1`, PROVENANCE.md): V19 at 212, so 211 (B) is
//! the last block under the legacy scheme. Both nodes take M to 215, `invalidateblock 209`, S
//! (209-214), a restart on S's 214, `reconsiderblock 209` and the rest of M, the steps dashd's
//! probe ran. Each step's scheme lines, its times cut, must be equal on both nodes, and Core's:
//!
//! - M to 215: `ProcessSpecialTxsInBlock`'s switch to basic at B's connect;
//! - `invalidateblock 209`: the disconnect's guard entering legacy at B;
//! - S: the switch at S's B;
//! - the restart: init's startup scheme line first (BL-801);
//! - `reconsiderblock 209`: legacy entered for S's disconnect, the switch at M's B;
//! - the rest of M: none.
//!
//! The restart runs VerifyDB at level 2 on both (`-checklevel=2`): rdashd's walk keeps evo blocks
//! out of level 3 (DEC-993) where dashd's level-3 walk over 6 blocks crosses B, so a level-3 window
//! is not comparable (BL-648 plan: VerifyDB across V19 with DIP0003 is not tested here). It cannot
//! be skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};

use common::mine::parse_hex;
use common::*;

const DATA: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../dashd-chainstate/tests/data/v19-window/r1");

/// R1's fork point P (S starts at P + 1) and M's tip when S was mined.
const P: usize = 208;
const M_AT_FORK: usize = 215;

// Core: evo/specialtxman.cpp:1043-1047, :1068-1072; validation.cpp:154-161 (v24.0.0)
const SWITCHED_BASIC: &str = "CSpecialTxProcessor::ProcessSpecialTxsInBlock -- bls_legacy_scheme=0";
const ENTERED_LEGACY: &str = "ScopedBLSLegacyScheme: entered bls_legacy_scheme=1";

/// One node under test: its binary, home and datadir, and the RPC node while it runs.
struct Side {
    binary: PathBuf,
    home: tempfile::TempDir,
    datadir: PathBuf,
    args: Vec<String>,
    rpc: Option<RpcNode>,
}

impl Side {
    fn new(binary: PathBuf, args: Vec<String>) -> Side {
        let (home, datadir) = home_and_datadir();
        Side {
            binary,
            home,
            datadir,
            args,
            rpc: None,
        }
    }

    fn start(&mut self, more: &[&str]) {
        let args: Vec<&str> =
            self.args.iter().map(String::as_str).chain(more.iter().copied()).collect();
        self.rpc = Some(RpcNode::start_on(&self.binary, self.home.path(), &self.datadir, &args));
    }

    fn stop(&mut self) {
        let rpc = self.rpc.take().expect("running");
        assert!(rpc.call("stop", "[]").result().contains("stopping"));
        let out = rpc.node.wait();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    }

    fn call(&self, method: &str, params: &str) -> String {
        self.rpc.as_ref().expect("running").call(method, params).result().to_owned()
    }

    fn submit(&self, blocks: &[String]) {
        for block in blocks {
            assert_eq!(self.call("submitblock", &format!(r#"["{block}"]"#)), "null");
        }
    }

    fn log_len(&self) -> usize {
        read(&debug_log(&self.datadir)).len()
    }

    /// The scheme lines logged since `before`, their times cut.
    fn scheme_lines(&self, before: usize) -> Vec<String> {
        let log = read(&debug_log(&self.datadir));
        log[before..]
            .lines()
            .map(|l| unstamped(l).to_owned())
            .filter(|l| {
                l.starts_with("ScopedBLSLegacyScheme: ") || l.contains(" bls_legacy_scheme=")
            })
            .collect()
    }
}

/// A log line's text: its time cut, and the ` (mocktime: N)` beside it (`logging.cpp:401-404`).
fn unstamped(line: &str) -> &str {
    let text = line.split_once(' ').map_or(line, |(_, text)| text);
    text.strip_prefix("(mocktime: ").and_then(|t| t.split_once(") ")).map_or(text, |(_, t)| t)
}

/// A fixture file's blocks: one hex per line after its `#` lines.
fn blocks(name: &str) -> Vec<String> {
    let text = read(&Path::new(DATA).join(name));
    text.lines().filter(|l| !l.starts_with('#')).map(str::to_owned).collect()
}

/// R1's chain arguments, and a clock a minute after M's last block.
fn r1_args(last: &str) -> Vec<String> {
    let summary = read(&Path::new(DATA).join("summary.json"));
    let summary: serde_json::Value = serde_json::from_str(&summary).expect("summary.json");
    let mut args: Vec<String> = summary["chain_args"]
        .as_array()
        .expect("chain_args")
        .iter()
        .map(|a| a.as_str().expect("an arg").to_owned())
        .collect();
    let header = parse_hex(&last[..160]);
    let time = u32::from_le_bytes(header[68..72].try_into().unwrap());
    args.push(format!("-mocktime={}", i64::from(time) + 60));
    // No seed threads: nothing but the steps logs.
    args.extend(["-dnsseed=0", "-fixedseeds=0"].map(str::to_owned));
    args
}

/// `step` on both nodes: their tips after it and their scheme lines in its window, equal;
/// rdashd's lines.
fn step(nodes: &mut [Side; 2], what: &str, run: impl Fn(&mut Side)) -> Vec<String> {
    let [ours, core] = nodes.each_mut().map(|n| {
        let before = n.log_len();
        run(n);
        (n.scheme_lines(before), n.call("getbestblockhash", "[]"))
    });
    assert_eq!(ours.1, core.1, "{what}: the tips");
    assert_eq!(ours.0, core.0, "{what}");
    ours.0
}

#[test]
fn the_scheme_lines_of_a_reorg_across_v19_match_dashd() {
    let (m, s) = (blocks("chain.txt"), blocks("fork.txt"));
    assert_eq!((m.len(), s.len()), (259, 6));
    let args = r1_args(m.last().unwrap());
    let mut nodes = [
        Side::new(PathBuf::from(RDASHD), args.clone()),
        Side::new(require_dashd(), [&args[..], &["-disablewallet".to_owned()]].concat()),
    ];
    for n in &mut nodes {
        n.start(&[]);
    }

    let lines = step(&mut nodes, "M to 215", |n| n.submit(&m[..M_AT_FORK]));
    assert_eq!(lines, [SWITCHED_BASIC]);
    let [cut, core_cut] = nodes.each_ref().map(|n| n.call("getblockhash", &format!("[{}]", P + 1)));
    assert_eq!(cut, core_cut);

    let invalidate = format!("[{cut}]");
    let lines = step(&mut nodes, "invalidateblock 209", |n| {
        assert_eq!(n.call("invalidateblock", &invalidate), "null");
    });
    assert_eq!(lines, [ENTERED_LEGACY]);

    let lines = step(&mut nodes, "S", |n| n.submit(&s));
    assert_eq!(lines, [SWITCHED_BASIC]);

    let lines = step(&mut nodes, "the restart on S's 214", |n| {
        n.stop();
        n.start(&["-checklevel=2"]);
    });
    assert_eq!(lines, [STARTUP_SCHEME]);

    let lines = step(&mut nodes, "reconsiderblock 209", |n| {
        assert_eq!(n.call("reconsiderblock", &invalidate), "null");
    });
    assert_eq!(lines, [ENTERED_LEGACY, SWITCHED_BASIC]);

    let lines = step(&mut nodes, "M to 259", |n| n.submit(&m[M_AT_FORK..]));
    assert_eq!(lines, Vec::<String>::new());
    for n in &mut nodes {
        n.stop();
    }
}
