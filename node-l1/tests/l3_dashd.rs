//! L3's tier 1 (CH3a a10) against a real regtest dashd v24: the version checks of plan §3.1,
//! then the legs. It fails closed: an unset or disagreeing `DASHVERSION`, no binary, or no
//! `L3_DASH_TREE` is an error, never a skip. `contrib/node/l3-tier1.sh` runs it.

use std::path::Path;

use node_l1::l3::dashd::{Process, is_synced};
use node_l1::l3::version::{self, private_dir};
use node_l1::l3::{Config, run};
use serde_json::json;

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

/// The resolved dashd starts on a private datadir, answers on regtest with no masternode sync,
/// and stops.
#[test]
fn the_pinned_dashd_starts_answers_and_stops() {
    let scratch = tempfile::tempdir().expect("a temp dir");
    let dashd = version::resolve(repo(), scratch.path()).expect("the run's dashd (plan §3.1)");
    assert_eq!(dashd.line, version::version_line(&dashd.version));
    assert_eq!(dashd.commit.len(), 40);
    let datadir = private_dir(scratch.path(), "node").expect("a datadir");
    let node = Process::start(&dashd.binary, &datadir, &[], 1_700_000_000).expect("started");
    let info = node.rpc().ok("getblockchaininfo", json!([])).expect("getblockchaininfo");
    assert_eq!(info["chain"], "regtest");
    assert_eq!(info["blocks"], 0);
    assert_eq!(is_synced(&node.rpc().ok("mnsync", json!(["status"])).expect("mnsync")), Ok(false));
    let e = node.rpc().call("submitblock", json!(["00"])).expect("an answer").expect_err("refused");
    assert_eq!((e.code, e.message.as_str()), (-22, "Block decode failed"));
    assert_eq!(node.rpc().ok("getbestblockhash", json!([])).map(|h| h.is_string()), Ok(true));
    assert!(node.argv().iter().any(|a| a == "-mocktime=1700000000"));
    node.stop().expect("stopped");
}

/// The operator the filtered run repeats: its expectations differ by `-par`.
const FILTERED: &str = "queued_script_then_bip68";

/// tier 1 (plan §3.4): both main legs and their replays, every plane after every op. The report
/// goes to `L3_REPORT`; a failed run keeps its directory (named in the panic). A full run then
/// runs [`FILTERED`] alone, whose every trial record (its bytes' hash, reply, effects, binding and
/// class) must equal the full run's (plan §3.2, M-2).
#[test]
fn tier1() {
    let config = Config::from_env().expect("the run's set");
    let report = run_kept(&config);
    assert!(report["cases"].as_u64().is_some_and(|n| n > 0), "no cases");
    if config.only.is_some() {
        return;
    }
    let filtered = run_kept(&Config {
        only: Some(FILTERED.into()),
        report: None,
        record: None,
        ..config
    });
    let trials = |r: &serde_json::Value| -> Vec<serde_json::Value> {
        r["legs"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|l| l["trials"].as_array().cloned().unwrap_or_default())
            .collect()
    };
    let full = trials(&report);
    let alone = trials(&filtered);
    assert!(!alone.is_empty(), "the filtered run ran no trial");
    for t in &alone {
        let id = &t["trial_id"];
        let same = full.iter().find(|f| f["trial_id"] == *id).expect("the full run's trial");
        assert_eq!(t, same, "{id}: the filtered run's record differs from the full run's");
    }
}

/// One run; a failed one keeps its directory and panics with each failed leg's error.
fn run_kept(config: &Config) -> serde_json::Value {
    let scratch = tempfile::tempdir().expect("a temp dir");
    let (report, done) = run(config, repo(), scratch.path()).expect("the run started");
    if !done {
        let kept = scratch.keep();
        let failed: Vec<String> = report["legs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| l["status"] != "done")
            .map(|l| format!("{}: {}", l["name"], l["error"]))
            .collect();
        panic!("L3 run failed (kept {}): {failed:?}", kept.display());
    }
    report
}
