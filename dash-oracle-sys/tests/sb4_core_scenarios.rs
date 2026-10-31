//! Gate checks for SB4's Core-judged vectors (`contrib/node/sb4-core-scenarios`,
//! designs/SB4-dash-chain.md §15).
//!
//! Always: every committed vector parses, names the v24.0 oracle pin's commit, and
//! was produced by harness.cpp and the scenario file as they are now (SHA-256),
//! under the boundary-flush delivery policy (DEC-147), and every harness-oracled
//! §15.1 row has a vector. Every build step carries each block's 80-byte header
//! (replayable: X11 hash and parent link checked). Rows whose label alone would not show the case (A-21b,
//! A-29(x) and (x'), A-1a's networks) must also show it in the vector's own states.
//! Every scenario vector's ConnectBlock/DisconnectBlock transcript (HX1 X1) agrees
//! with Core's own counters, and each connected or checked block's submitted bytes
//! hash to the transcript's hash; every step carries its complete view (the
//! exact trace, every status change and the full X3 dump, never a count in their
//! place), and every op a scenario uses has an X5 adapter row in the README.
//! Vectors are gzipped JSON (`<case>.json.gz`, written by regen.py). When
//! `SB4_CORE_SCENARIOS_BIN` names a build.sh output directory, the smoke set is
//! also regenerated with the harness and must be byte-identical; without it that
//! half is skipped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use bitcoin_hashes::{Hash, sha256};
use serde_json::Value;

/// Every §15.1 / §15.3 item whose oracle column names the harness.
const HARNESS_ROWS: &[&str] = &[
    "A-1a",
    "A-1a/devnet",
    "A-1a/testnet",
    "A-1a/mainnet",
    "A-2",
    "A-16",
    "A-17",
    "A-18",
    "A-19",
    "A-20",
    "A-21",
    "A-21a",
    "A-21b",
    "A-22",
    "A-24",
    "A-25",
    "A-26",
    "A-28",
    "A-29(i)",
    "A-29(ii)",
    "A-29(iii)",
    "A-29(iv)",
    "A-29(v)",
    "A-29(vii)",
    "A-29(viii)",
    "A-29(ix)",
    "A-29(x)",
    "A-29(x')",
    "A-30",
    "A-35",
    "A-36",
    "A-38",
    "R2-M2",
    "HX1-X1",
    "HX1-X2",
    "HX1-X3",
    "DEC-167",
    "codec",
    "locator",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn vectors_dir() -> PathBuf {
    repo().join("dash-chain/tests/data/core-scenarios")
}

fn scenarios_dir() -> PathBuf {
    repo().join("contrib/node/sb4-core-scenarios/scenarios")
}

fn pinned_v24_commit() -> String {
    let pins = fs::read_to_string(repo().join("dash-oracle-sys/pins.txt")).unwrap();
    pins.lines()
        .find_map(|l| {
            let mut f = l.split_whitespace();
            (f.next() == Some("v24.0")).then(|| f.next()).flatten()
        })
        .expect("v24.0 pin in pins.txt")
        .to_string()
}

/// The files in `dir` named `*<suffix>`, sorted, with the name before the suffix.
fn files(dir: &Path, suffix: &str) -> Vec<(String, PathBuf)> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.strip_suffix(suffix)?.to_string();
            Some((name, p))
        })
        .collect();
    v.sort();
    v
}

/// A committed vector file, decompressed and parsed.
fn read_vector(path: &Path) -> Value {
    let mut text = String::new();
    flate2::read::GzDecoder::new(fs::File::open(path).unwrap())
        .read_to_string(&mut text)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// BLOCK_CONFLICT_CHAINLOCK (chain.h:118).
const CONFLICT_CHAINLOCK: u64 = 128;
/// BLOCK_HAVE_DATA (chain.h:110): the block's data is stored.
const BLOCK_HAVE_DATA: u64 = 8;

/// A scenario's labels as the harness names them: `<branch><height>`, `G` (genesis),
/// `DG` (a devnet's block 1), `R<height>` (preloaded real headers).
fn height(label: &str) -> i64 {
    match label {
        "G" => 0,
        "DG" => 1,
        _ => label.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().unwrap(),
    }
}

/// Every label's parent, from the scenario's build ops and preload.
fn parents(scenario: &Value) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("DG".to_string(), "G".to_string());
    let preloaded = scenario["preload"]["count"].as_i64().unwrap_or(0);
    for h in 1..preloaded {
        map.insert(
            format!("R{h}"),
            if h == 1 {
                "G".to_string()
            } else {
                format!("R{}", h - 1)
            },
        );
    }
    for op in scenario["ops"].as_array().unwrap().iter().filter(|o| o["op"] == "build") {
        let branch = op["branch"].as_str().unwrap();
        let mut parent = op["parent"].as_str().unwrap().to_string();
        for _ in 0..op["count"].as_i64().unwrap() {
            let label = format!("{branch}{}", height(&parent) + 1);
            map.insert(label.clone(), parent);
            parent = label;
        }
    }
    map
}

fn ancestors<'a>(parents: &'a BTreeMap<String, String>, label: &'a str) -> Vec<&'a str> {
    let mut out = vec![];
    let mut cur = label;
    while let Some(p) = parents.get(cur) {
        out.push(p.as_str());
        cur = p;
    }
    out
}

/// Each step with the accumulated nStatus of every label after it.
fn states(vector: &Value) -> Vec<(&Value, BTreeMap<String, u64>)> {
    let mut status = BTreeMap::new();
    let mut out = vec![];
    for step in vector["steps"].as_array().unwrap() {
        for (label, s) in step["view"]["status_changes"].as_object().unwrap() {
            match s.as_u64() {
                Some(n) => status.insert(label.clone(), n),
                None => status.remove(label),
            };
        }
        out.push((step, status.clone()));
    }
    out
}

/// A step's state is never summarised by a count (`*_count` fields, which a
/// removed summary mode printed instead of the changes); any such key in a step
/// fails.
fn reject_step_counts(name: &str, vector: &Value) {
    for step in vector["steps"].as_array().into_iter().flatten() {
        reject_counts(name, step);
    }
}

fn reject_counts(name: &str, value: &Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                assert!(!k.ends_with("_count"), "{name}: count-only field {k}");
                reject_counts(name, v);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| reject_counts(name, v)),
        _ => {}
    }
}

/// A-29(x) and (x'): some step's floor has a CONFLICT-marked ancestor (the kept mark).
fn has_kept_mark_on_floor_path(scenario: &Value, vector: &Value) -> bool {
    let parents = parents(scenario);
    states(vector).iter().any(|(step, status)| {
        let Some(floor) = step["view"]["cl"]["floor"].as_str() else {
            return false;
        };
        ancestors(&parents, floor)
            .iter()
            .any(|a| status.get(*a).is_some_and(|s| s & CONFLICT_CHAINLOCK != 0))
    })
}

/// A-21b: the floor is an ancestor 30 below the active tip, and a later
/// `enforce` or `timer` enforces that same floor.
fn has_ancestor_floor_repeat(scenario: &Value, vector: &Value) -> bool {
    let parents = parents(scenario);
    let steps = vector["steps"].as_array().unwrap();
    steps.iter().enumerate().any(|(i, step)| {
        let (Some(floor), Some(tip)) =
            (step["view"]["cl"]["floor"].as_str(), step["view"]["tip"].as_str())
        else {
            return false;
        };
        height(tip) - height(floor) == 30
            && ancestors(&parents, tip).contains(&floor)
            && steps[i + 1..].iter().any(|later| {
                ["enforce", "timer"].contains(&later["op"]["op"].as_str().unwrap_or(""))
                    && later["view"]["enforce_block_boundaries"]
                        .as_array()
                        .is_some_and(|b| b.iter().any(|b| b["locked"] == floor))
            })
    })
}

/// DEC-167: some op's first pass ends away from its fixed point (a transient
/// tip), and a later pass moves the state there.
fn has_pass_one_transient(vector: &Value) -> bool {
    vector["steps"].as_array().unwrap().iter().any(|step| {
        let Some(passes) = step["view"]["enforce_block_boundaries"].as_array() else {
            return false;
        };
        passes.iter().any(|b| b["pass"] == 1 && b["pass_end"]["tip"] != step["view"]["tip"])
            && passes.iter().any(|b| b["repeat"] == true && b["pass_end"]["changed_state"] == true)
    })
}

/// DEC-167: every pass records where it ended, and each op's last pass leaves
/// the state as it found it (the view is the fixed point).
fn check_fixed_points(name: &str, vector: &Value) {
    for (i, step) in vector["steps"].as_array().unwrap().iter().enumerate().skip(1) {
        let Some(passes) = step["view"]["enforce_block_boundaries"].as_array() else {
            continue;
        };
        assert!(
            passes.iter().all(|b| b["pass_end"].is_object()),
            "{name}: step {i}: a pass without pass_end"
        );
        assert_eq!(
            passes.last().unwrap()["pass_end"]["changed_state"],
            false,
            "{name}: step {i} ends on a pass that moved the state"
        );
    }
}

/// Each single-header submission in a vector: (label, "accepted" or the reject reason).
fn header_verdicts(vector: &Value) -> Vec<(String, String)> {
    let mut out = vec![];
    for step in vector["steps"].as_array().unwrap() {
        let (r, op) = (&step["result"], &step["op"]);
        let (p, label) = if r["pnbh"].is_object() {
            (
                &r["pnbh"],
                r["built"]
                    .as_str()
                    .and_then(|b| b.split_once(".."))
                    .filter(|(a, b)| a == b)
                    .map(|(a, _)| a),
            )
        } else {
            let one = op["headers"].as_array().filter(|h| h.len() == 1).and_then(|h| h[0].as_str());
            (r, one.filter(|h| !h.contains("..")))
        };
        if let (Some(label), Some(ok), Some(reason)) =
            (label, p["ok"].as_bool(), p["state"]["reason"].as_str())
        {
            let verdict = if ok {
                "accepted"
            } else {
                reason
            };
            out.push((label.to_string(), verdict.to_string()));
        }
    }
    out
}

/// Every `build` step carries each block's 80-byte header (`headers`), so the
/// scenario can be replayed: one per label in `built`, each hashing (X11) to the
/// listed hash and naming its parent's hash, where the parent is a built block.
fn check_build_headers(name: &str, scenario: &Value, vector: &Value) {
    let parents = parents(scenario);
    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    for step in vector["steps"].as_array().unwrap().iter().filter(|s| s["op"]["op"] == "build") {
        let r = &step["result"];
        let headers = r["headers"].as_object().unwrap_or_else(|| panic!("{name}: no headers"));
        let (first, last) = r["built"].as_str().unwrap().split_once("..").unwrap();
        assert_eq!(headers.len() as i64, height(last) - height(first) + 1, "{name}: {first}..");
        let mut headers: Vec<_> = headers.iter().collect();
        headers.sort_by_key(|(label, _)| height(label));
        for (label, hex) in headers {
            let hex = hex.as_str().unwrap();
            assert_eq!(hex.len(), 160, "{name}: {label} is not an 80-byte header");
            let header: dashcore::block::Header =
                dashcore::consensus::deserialize(&from_hex(hex)).unwrap();
            let hash = header.block_hash().to_string();
            if let Some(listed) = r["hashes"][label].as_str() {
                assert_eq!(hash, listed, "{name}: {label}'s header does not hash to its hash");
            }
            if let Some(parent) = hashes.get(&parents[label]) {
                assert_eq!(&header.prev_blockhash.to_string(), parent, "{name}: {label}'s parent");
            }
            hashes.insert(label.clone(), hash);
        }
    }
}

/// What HX1's acceptance needs shown across the vectors (designs/CH1-node.md §11).
#[derive(Default)]
struct CallEvidence {
    /// X1: a block failing bad-cb-amount at ConnectTip (A-18's invalid chain).
    cb_amount_at_connect: bool,
    /// X1: the same bytes failing bad-chainlock at ConnectTip, then connecting.
    chainlock_then_connect: bool,
    /// X1: TestBlockValidity checks, one passing and one failing.
    check_ok: bool,
    check_failed: bool,
    /// X2: the ops' outcomes, as Core judged them (x2_outcomes).
    outcomes: BTreeSet<&'static str>,
}

/// HX1 X2: what each new op must be seen doing in Core, not only requested: an
/// unforced ProcessNewBlock refusing a block and a forced one storing it, a
/// preciousblock taking the tip, and a wrong BIP34 height failing as bad-cb-height
/// (Core skips that check from DIP3 on, so asking for it is not enough).
fn x2_outcomes(vector: &Value, evidence: &mut CallEvidence) {
    let steps = vector["steps"].as_array().unwrap();
    let states = states(vector);
    let has_data =
        |i: usize, label: &str| states[i].1.get(label).is_some_and(|s| s & BLOCK_HAVE_DATA != 0);
    // AcceptBlock's unrequested-block rule: blocks ProcessNewBlock received
    // unforced, without data before and after (so Core refused to store them).
    let mut refused = BTreeSet::new();
    for (i, (prev, step)) in steps.iter().zip(steps.iter().skip(1)).enumerate() {
        let (op, r) = (&step["op"], &step["result"]);
        match (op["op"].as_str().unwrap(), &op["force"]) {
            ("pnb", Value::Bool(force)) => {
                let block = op["block"].as_str().unwrap();
                let (before, after) = (has_data(i, block), has_data(i + 1, block));
                if !before && !force && r["ok"] == true && r["new_block"] == false && !after {
                    refused.insert(block);
                }
                if !before && *force && r["new_block"] == true && after && refused.contains(block) {
                    evidence.outcomes.insert("pnb unforced refused, then forced stored");
                }
            }
            ("precious", ..)
                if step["view"]["tip"] == op["block"] && prev["view"]["tip"] != op["block"] =>
            {
                evidence.outcomes.insert("precious took the tip");
            }
            ("propose", ..) if r["verdict"] == "bad-cb-height" => {
                evidence.outcomes.insert("bad-cb-height");
            }
            _ => {}
        }
    }
    let reasons = vector["accept_checks"].as_array().into_iter().flatten().map(|e| &e["state"]);
    let reasons =
        reasons.chain(vector["transcript"].as_array().unwrap().iter().map(|e| &e["state"]));
    if reasons.into_iter().any(|s| s["reason"] == "bad-cb-height") {
        evidence.outcomes.insert("bad-cb-height");
    }
}

fn from_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "odd-length hex");
    (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// A step's listed blocks (blocks_submitted or blocks_proposed): each must decode as
/// a whole block, every byte consumed, with a merkle root matching its transactions.
fn listed_blocks(name: &str, step: &Value, key: &str, into: &mut BTreeMap<String, String>) {
    for (label, hex) in step[key].as_object().into_iter().flatten() {
        let hex = hex.as_str().unwrap();
        let block: dashcore::Block = dashcore::consensus::deserialize(&from_hex(hex))
            .unwrap_or_else(|e| panic!("{name}: {key} {label} is not a whole block: {e}"));
        assert!(block.check_merkle_root(), "{name}: {key} {label}'s merkle root");
        into.insert(block.block_hash().to_string(), hex.to_string());
    }
}

/// HX1 X5: the op names the README's "Replaying the ops through V" table documents
/// (each row's first column, every backticked op name before its `{` or space).
fn documented_ops() -> BTreeSet<String> {
    let readme =
        fs::read_to_string(repo().join("contrib/node/sb4-core-scenarios/README.md")).unwrap();
    let section = readme
        .split("### Replaying the ops through V")
        .nth(1)
        .expect("README has no X5 section")
        .split("\n#")
        .next()
        .unwrap();
    let mut ops = BTreeSet::new();
    for row in section.lines().filter(|l| l.starts_with("| `")) {
        let first = row.split(" | ").next().unwrap();
        for name in first.split('`').skip(1).step_by(2) {
            ops.insert(name.split([' ', '{']).next().unwrap().to_string());
        }
    }
    ops
}

/// HX1 X1: the transcript of every ConnectBlock/DisconnectBlock call. Per site its
/// length equals Core's own counter (core_counts, from Core's log lines and
/// ConnectTip's BlockChecked), occurrences count each (phase, hash) from 1, and
/// every block ConnectTip connected was given to ProcessNewBlock earlier, and every
/// checked block proposed, with whole-block bytes hashing to it.
fn check_calls(name: &str, vector: &Value, evidence: &mut CallEvidence) {
    let transcript =
        vector["transcript"].as_array().unwrap_or_else(|| panic!("{name}: no transcript"));
    let counts = &vector["core_counts"];
    let mut per_site: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    let mut occurrences: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    let mut submitted: BTreeMap<String, String> = BTreeMap::new();
    let mut proposed: BTreeMap<String, String> = BTreeMap::new();
    let steps = vector["steps"].as_array().unwrap();
    let mut next_step = 0;
    let mut failed_by_chainlock = BTreeSet::new();
    for e in transcript {
        let step = e["step"].as_u64().unwrap() as usize;
        // steps[0] is the setup view; transcript step N is steps[N].
        while next_step <= step.min(steps.len() - 1) {
            listed_blocks(name, &steps[next_step], "blocks_submitted", &mut submitted);
            listed_blocks(name, &steps[next_step], "blocks_proposed", &mut proposed);
            next_step += 1;
        }
        let (phase, site, hash) = (
            e["phase"].as_str().unwrap(),
            e["site"].as_str().unwrap(),
            e["hash"].as_str().unwrap(),
        );
        let known = matches!(
            (phase, site),
            ("connect", "Chainstate::ConnectTip" | "CVerifyDB::VerifyDB")
                | ("check", "TestBlockValidity")
                | (
                    "disconnect",
                    "Chainstate::DisconnectTip"
                        | "CVerifyDB::VerifyDB"
                        | "Chainstate::ReplayBlocks"
                )
        );
        assert!(known, "{name}: {phase} from {site}");
        *per_site.entry((phase, site)).or_default() += 1;
        let n = occurrences.entry((phase, hash)).or_default();
        *n += 1;
        assert_eq!(e["occurrence"].as_u64(), Some(*n), "{name}: {phase} {hash} occurrence");
        // A connect needs the block given to ProcessNewBlock, a check its proposal
        // (VerifyDB reconnects blocks already on disk).
        let given = match (phase, site) {
            ("check", _) => Some(&proposed),
            ("connect", "Chainstate::ConnectTip") => Some(&submitted),
            _ => None,
        };
        if let Some(given) = given {
            assert!(given.contains_key(hash), "{name}: {} reached {phase} unsubmitted", e["block"]);
        }
        let reason = e["state"]["reason"].as_str().unwrap_or_default();
        match (phase, &e["result"]) {
            ("connect", Value::Bool(false)) if reason == "bad-cb-amount" => {
                evidence.cb_amount_at_connect = true
            }
            ("connect", Value::Bool(false)) if reason == "bad-chainlock" => {
                failed_by_chainlock.insert(hash);
            }
            ("connect", Value::Bool(true)) if failed_by_chainlock.contains(hash) => {
                evidence.chainlock_then_connect = true
            }
            ("check", Value::Bool(true)) => evidence.check_ok = true,
            ("check", Value::Bool(false)) => evidence.check_failed = true,
            _ => {}
        }
    }
    let site = |phase, site| per_site.get(&(phase, site)).copied().unwrap_or(0);
    let connect_tip = site("connect", "Chainstate::ConnectTip");
    assert_eq!(counts["connect_tip"].as_u64(), Some(connect_tip), "{name}: ConnectTip calls");
    assert_eq!(
        counts["tip_block_checked"].as_u64(),
        Some(connect_tip),
        "{name}: ConnectTip BlockChecked"
    );
    assert_eq!(
        counts["disconnect_tip"].as_u64(),
        Some(site("disconnect", "Chainstate::DisconnectTip")),
        "{name}: DisconnectTip calls"
    );
    for (key, phase, s) in [
        ("verify_db_connect", "connect", "CVerifyDB::VerifyDB"),
        ("verify_db_disconnect", "disconnect", "CVerifyDB::VerifyDB"),
        ("replay_disconnect", "disconnect", "Chainstate::ReplayBlocks"),
    ] {
        assert_eq!(counts[key].as_u64(), Some(site(phase, s)), "{name}: {s} {phase} calls");
    }
    assert_eq!(
        counts["test_block_validity"].as_u64(),
        Some(site("check", "TestBlockValidity")),
        "{name}: TestBlockValidity calls"
    );
}

/// A-1a's network rows: the network, and the Core verdicts that make the case
/// (pooled over the row's vectors).
/// (row, network, [(label, verdict)]).
type NetworkRow = (&'static str, &'static str, &'static [(&'static str, &'static str)]);

const NETWORK_ROWS: &[NetworkRow] = &[
    (
        "A-1a/devnet",
        "devnet",
        &[
            ("X1", "devnet-genesis"),
            ("B1152", "accepted"),
            ("B1152", "bad-diffbits"),
            ("C1152", "accepted"),
        ],
    ),
    (
        "A-1a/testnet",
        "test",
        &[
            ("T3501", "accepted"),
            ("U3501", "bad-diffbits"),
            ("V3501", "accepted"),
            ("W3502", "bad-diffbits"),
            ("X3502", "accepted"),
            ("Y5001", "accepted"),
            ("Z5001", "bad-diffbits"),
        ],
    ),
    (
        "A-1a/mainnet",
        "main",
        &[
            ("S201", "bad-chainlock"),
            ("S201", "duplicate"),
            ("S202", "bad-prevblk-chainlock"),
            ("L250", "bad-chainlock"),
            ("F261", "accepted"),
        ],
    ),
];

/// Every step that submits headers through ProcessNewBlockHeaders (a direct
/// `pnbh`, or a `build` with `submit: headers`) must record what Core wrote.
fn check_records_written(name: &str, i: usize, step: &Value) {
    let op = &step["op"];
    let pnbh = match op["op"].as_str() {
        Some("pnbh") => &step["result"],
        Some("build") if op["submit"] == "headers" => &step["result"]["pnbh"],
        _ => return,
    };
    assert!(pnbh["records_written"].is_object(), "{name}: step {i} without records_written");
}

#[test]
fn records_written_required_on_every_pnbh_step() {
    let records = serde_json::json!({"C2": "00"});
    let direct = |result: Value| serde_json::json!({"op": {"op": "pnbh"}, "result": result});
    let built = |pnbh: Value| serde_json::json!({"op": {"op": "build", "submit": "headers"}, "result": {"pnbh": pnbh}});
    check_records_written("ok", 1, &direct(serde_json::json!({"records_written": records})));
    check_records_written("ok", 1, &built(serde_json::json!({"records_written": records})));
    check_records_written(
        "ok",
        1,
        &serde_json::json!({"op": {"op": "build", "submit": "connect"}}),
    );
    for step in [
        direct(serde_json::json!({"ok": true})),
        built(serde_json::json!({"ok": true})),
        serde_json::json!({"op": {"op": "build", "submit": "headers"}, "result": {}}),
    ] {
        let missing = std::panic::catch_unwind(|| check_records_written("mutated", 1, &step));
        assert!(missing.is_err(), "accepted a pnbh step without records_written: {step}");
    }
}

#[test]
fn vectors_match_their_inputs_and_pin() {
    let commit = pinned_v24_commit();
    let harness = fs::read(repo().join("contrib/node/sb4-core-scenarios/harness.cpp")).unwrap();
    let harness_sha = sha256::Hash::hash(&harness).to_string();
    let mut covered = BTreeSet::new();
    let mut network_verdicts: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();
    let mut evidence = CallEvidence::default();
    let documented = documented_ops();
    let vectors = files(&vectors_dir(), ".json.gz");
    assert!(!vectors.is_empty(), "no vectors under {}", vectors_dir().display());
    let stray = files(&vectors_dir(), ".json");
    assert!(stray.is_empty(), "uncompressed vector files: {stray:?}");
    for (name, path) in &vectors {
        let name = name.as_str();
        let doc = read_vector(path);
        reject_step_counts(name, &doc["vector"]);
        let prov = &doc["provenance"];
        assert_eq!(prov["core_commit"], commit.as_str(), "{name}: not produced at the v24.0 pin");
        assert_eq!(prov["core_tag"], "v24.0.0-rc.5", "{name}");
        assert_eq!(
            prov["harness_source_sha256"],
            harness_sha.as_str(),
            "{name}: harness.cpp changed since the vector was generated (run regen.py)"
        );
        assert!(
            prov["command"].as_str().is_some_and(|c| c.starts_with("sb4_scenarios ")),
            "{name}"
        );
        assert!(prov["archives_sha256"].as_object().is_some_and(|a| !a.is_empty()), "{name}");
        assert!(prov["binary_sha256"].as_str().is_some_and(|h| h.len() == 64), "{name}");
        let vector = &doc["vector"];
        let command = prov["command"].as_str().unwrap();
        if command.starts_with("sb4_scenarios run ") {
            // A scenario vector: its input must be the scenario file as committed.
            let rows = vector["rows"].as_array().unwrap_or_else(|| panic!("{name}: no rows"));
            let rel = format!("contrib/node/sb4-core-scenarios/scenarios/{name}.json");
            let bytes =
                fs::read(repo().join(&rel)).unwrap_or_else(|_| panic!("{name}: no scenario {rel}"));
            assert_eq!(
                prov["inputs_sha256"][&rel],
                sha256::Hash::hash(&bytes).to_string().as_str(),
                "{name}: scenario changed since the vector was generated (run regen.py)"
            );
            assert_eq!(prov["delivery_policy"], "boundary-flush", "{name}");
            assert!(vector["steps"].as_array().is_some_and(|s| !s.is_empty()), "{name}");
            let scenario: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(vector["network"], scenario["network"], "{name}");
            check_build_headers(name, &scenario, vector);
            check_calls(name, vector, &mut evidence);
            check_fixed_points(name, vector);
            for (i, step) in vector["steps"].as_array().unwrap().iter().enumerate() {
                let (view, dump) = (&step["view"], &step["view"]["dump"]);
                assert!(
                    dump["candidates"].is_array()
                        && dump["unlinked"].is_array()
                        && dump["cl"].is_object()
                        && dump["entries"].is_object(),
                    "{name}: step {i} without a complete X3 dump"
                );
                assert!(
                    view["trace"].is_array() && view["status_changes"].is_object(),
                    "{name}: step {i} without its trace and status changes"
                );
                check_records_written(name, i, step);
            }
            for op in scenario["ops"].as_array().unwrap() {
                let op_name = op["op"].as_str().unwrap();
                assert!(
                    documented.contains(op_name),
                    "{name}: op {op_name} has no X5 adapter row in the README"
                );
            }
            x2_outcomes(vector, &mut evidence);
            for row in rows.iter().map(|r| r.as_str().unwrap()) {
                match row {
                    "A-29(x)" | "A-29(x')" => assert!(
                        has_kept_mark_on_floor_path(&scenario, vector),
                        "{name}: {row} needs a CONFLICT mark kept on the floor's own path"
                    ),
                    "DEC-167" => assert!(
                        has_pass_one_transient(vector),
                        "{name}: DEC-167 needs a pass-1 transient tip, then a pass to the fixed point"
                    ),
                    "A-21b" => assert!(
                        has_ancestor_floor_repeat(&scenario, vector),
                        "{name}: A-21b needs the floor 30 below the tip, then enforce/timer"
                    ),
                    _ => {}
                }
                if let Some((known, net, _)) = NETWORK_ROWS.iter().find(|(r, ..)| *r == row) {
                    assert_eq!(vector["network"], *net, "{name}: {row} runs on {net}");
                    network_verdicts.entry(*known).or_default().extend(header_verdicts(vector));
                }
            }
            covered.extend(rows.iter().map(|r| r.as_str().unwrap().to_string()));
        } else if name.starts_with("bench-pnbh") {
            assert!(vector["instructions_u"].as_u64().is_some_and(|n| n > 0), "{name}");
            covered.insert("A-25".to_string());
        } else if name.starts_with("locator") {
            covered.insert("locator".to_string());
        }
    }
    for (name, _) in files(&scenarios_dir(), ".json") {
        assert!(
            vectors_dir().join(format!("{name}.json.gz")).exists(),
            "scenario {name} has no committed vector"
        );
    }
    for (row, _, wanted) in NETWORK_ROWS {
        let verdicts = network_verdicts.get(row).map(Vec::as_slice).unwrap_or_default();
        for (label, verdict) in *wanted {
            assert!(
                verdicts.iter().any(|(l, v)| l == label && v == verdict),
                "{row}: no {label} {verdict} verdict"
            );
        }
    }
    assert!(evidence.cb_amount_at_connect, "HX1-X1: no bad-cb-amount at ConnectTip");
    assert!(
        evidence.chainlock_then_connect,
        "HX1-X1: no bad-chainlock occurrence followed by a connect"
    );
    assert!(
        evidence.check_ok && evidence.check_failed,
        "HX1-X1: TestBlockValidity needs a pass and a failure"
    );
    for outcome in
        ["pnb unforced refused, then forced stored", "precious took the tip", "bad-cb-height"]
    {
        assert!(evidence.outcomes.contains(outcome), "HX1-X2: no vector shows {outcome}");
    }
    let missing: Vec<_> = HARNESS_ROWS.iter().filter(|r| !covered.contains(**r)).collect();
    assert!(missing.is_empty(), "harness-oracled rows without vectors: {missing:?}");
}

#[test]
fn smoke_regenerates_byte_identical() {
    let Some(bin) = std::env::var_os("SB4_CORE_SCENARIOS_BIN") else {
        eprintln!(
            "skipped: SB4_CORE_SCENARIOS_BIN is not set (build with contrib/node/sb4-core-scenarios/build.sh)"
        );
        return;
    };
    let status = Command::new("python3")
        .arg(repo().join("contrib/node/sb4-core-scenarios/regen.py"))
        .args(["--check", "--smoke", "--bin"])
        .arg(bin)
        .status()
        .expect("python3");
    assert!(status.success(), "regen.py --check --smoke failed");
}
