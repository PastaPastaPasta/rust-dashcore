//! The `CoreConnector` replay leg (CH3a a7-1): blocks through V and `CoreConnector` with the
//! `DigestLog` sink, compared with what Core recorded.
//!
//! - **Regtest** (CH3a D143–D145). Every non-evo node of the stored regtest v24 set, replayed
//!   with its recorded decisions under the strict policy, is compared twice:
//!   - in linear mode with its golden: every `C.s` key a7-1 writes is required and equal at
//!     every height, and the run is clean (exit 0);
//!   - in stream mode, `tip` origin, with its Core digest. `blocks.dat` holds the final chain
//!     only, so the replay makes the final chain's tip connects in height order and nothing else.
//!     The stream is clean through Core's last record that a final-chain replay makes, and the
//!     comparison stops at exactly the next one, the first side block, rejection or disconnect,
//!     with nothing differing before it. A node whose Core tip stream is the final chain in order
//!     is clean (exit 0).
//!
//!   The comparisons leave out the keys a7-1 does not write (CH3a D144): the evo keys `mnl`, `qc`,
//!   `cp` and `ehf`, and in stream mode `fx` and the golden keys, which Core's digest lacks. Core's
//!   side holds `null` (and `fx` `[]`) in every record the comparisons pair, which the tests check,
//!   so nothing Core recorded goes uncompared.
//! - **Testnet** (CH3a D146, D147). With `CH3A_TESTNET_WINDOW` naming the pinned window directory,
//!   heights 0..6999 of testnet replay with `IbdHelper` and `-assumevalid=0` and, from height 0,
//!   compare clean in linear mode with the pinned golden slice. The blocks come from the copy's
//!   `blocks/` directory through [`BlockDir`] (CH3a D150), whose height → (file, offset) map of
//!   the window is the pinned `located-0-6999.json`'s. The replay's undo records of heights
//!   1..6999 then compare equal with the ones Core wrote in the copy's rev files (CH3a D101,
//!   [`revfiles`]). Without the variable the test says it skipped. Run it with `--release`: a
//!   debug build spends about 200 ms per `C` in MuHash.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash_conformance::digest::blkdir::BlockDir;
use dash_conformance::digest::compare::{Input, compare, project};
use dash_conformance::digest::replay_core::{replay_node, replay_window};
use dash_conformance::digest::revfiles;
use dash_conformance::digest::store::{Node, Records, Vectors, sha256_file, sha256_hex};
use dash_consensus::primitives::{Header, core_serialize};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHash;
use dash_statedigest::compare::{FieldId, Mode, Options, Report, Side, Verdict, What};
use dash_statedigest::decisions::Policy;
use dash_statedigest::{Chain, Class, FIELDS, Hash256, Kind, Origin, Outcome, Record, Value};
use dashd_chainstate::chain::genesis_block;
use rayon::prelude::*;

/// The non-evo nodes of the regtest v24 set (CH3a D143): their final chains never make
/// readiness guard G1 refuse.
const NON_EVO: [&str; 8] = [
    "feature_block/node0",
    "feature_new_quorum_type_activation/node0",
    "mining_basic/node0",
    "mining_basic/node1",
    "p2p_invalid_block/node0",
    "rpc_invalidateblock/node0",
    "rpc_invalidateblock/node1",
    "rpc_invalidateblock/node2",
];

/// The `C.s` keys CH7a's hook H11 writes, absent from a non-evo log (CH3a D144).
const EVO_KEYS: [&str; 4] = ["mnl", "qc", "cp", "ehf"];

/// The golden keys (O8 §5.3), which Core's digest lacks.
const GOLDEN_KEYS: [&str; 6] = ["muhash", "txouts", "amount", "unspendable", "bi", "stats"];

/// The keys the testnet window's compare requires (CH3a a7-1 brief).
const WINDOW_REQUIRE: [&str; 10] =
    ["muhash", "undo", "stats", "bi", "txouts", "amount", "unspendable", "bls", "bur", "dep"];

/// The buried deployments G1 refuses, by their bit in `bur` (`Consensus::BuriedDeployment`'s
/// order, as `DigestLog` writes it), and the versionbits deployments, by their `dep` name.
// Core: consensus/params.h:22-39 (the buried enum), :42-48 (DeploymentPos)
const G1_BURIED: [(u32, &str); 3] = [(6, "DIP0003"), (12, "V20"), (13, "MN_RR")];
const G1_VERSIONBITS: [&str; 2] = ["v24", "evo_shares"];

/// The pinned window (CH3a D146).
const BLK_PIN: &str = "63228ddfe56966aaf83117a2640f2e947bf8e416b2a6ff1e10c6efe380adc86e";
const GOLDEN_PIN: &str = "0fc29dc2ad7ebef94ba0b6931a2b7a9ea780e002ba26e36481cff003eda6943c";
const GOLDEN_ZST_PIN: &str = "1da34398d6e6063a226d2eccc085f18c664182691f823753525a06be3cd85291";
const LOCATED_PIN: &str = "9c11b12337dd336efdb55ddf256e8c51e10ea2268f75642b3be72a4032404d4c";
/// The full testnet v24 golden the slice was cut from.
const FULL_GOLDEN_PIN: &str = "392ccf3ddf399b05fe6ccc57108b1038fd2d8172fb8f9cb4e949a54687b03cca";
const WINDOW_TIP: i64 = 6999;

fn tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn field(kind: Kind, class: Class, key: &str) -> FieldId {
    FieldId::find(kind, class, key).unwrap_or_else(|| panic!("no key {key}"))
}

/// The `C.s` keys in spec order, without `skip`.
fn connect_keys(skip: &[&str]) -> Vec<FieldId> {
    FIELDS
        .iter()
        .filter(|f| f.kind == Kind::Connect && f.class == Class::Semantic && !skip.contains(&f.key))
        .map(|f| field(f.kind, f.class, f.key))
        .collect()
}

fn evo_fields() -> Vec<FieldId> {
    EVO_KEYS.iter().map(|k| field(Kind::Connect, Class::Semantic, k)).collect()
}

/// Linear mode against a golden: the evo keys left out, every other `C.s` key required.
fn linear_options() -> Options {
    let mut opts = Options::new(Mode::Linear);
    opts.require = connect_keys(&EVO_KEYS);
    opts.exclude = evo_fields();
    opts
}

/// Stream mode, `tip` origin, against a Core digest: the evo keys, the golden keys and `fx` left
/// out, every other `C.s` key required.
fn stream_options() -> Options {
    let mut opts = Options::new(Mode::Stream);
    opts.origins = Some(vec![Origin::Tip]);
    let skip: Vec<&str> = EVO_KEYS.iter().chain(&GOLDEN_KEYS).copied().collect();
    opts.require = connect_keys(&skip);
    opts.exclude = evo_fields();
    opts.exclude.extend(GOLDEN_KEYS.iter().map(|k| field(Kind::Connect, Class::Semantic, k)));
    opts.exclude.extend(
        [Kind::Connect, Kind::Reject, Kind::Disconnect]
            .into_iter()
            .map(|kind| field(kind, Class::Observation, "fx")),
    );
    opts
}

/// Every key of `keys` is equal in `pairs` record pairs, and absent from none.
fn assert_all_equal(report: &Report, keys: &[FieldId], pairs: u64, what: &str) {
    for f in keys {
        let c = report.coverage.get(f).copied().unwrap_or_default();
        assert_eq!((c.equal, c.differ, c.absent()), (pairs, 0, 0), "{what}: {f}");
    }
}

fn run(left: &Path, right: &Path, opts: Options) -> Report {
    let mut inputs = [Input::open(left).unwrap(), Input::open(right).unwrap()];
    compare(&mut inputs, opts).unwrap()
}

/// The `C`, `R` and `D` records of a file, with their line numbers.
fn outcomes(path: &Path) -> Vec<(usize, Outcome)> {
    Records::open(path)
        .unwrap()
        .filter_map(|r| match r.unwrap() {
            (at, Record::Outcome(o)) => Some((at, o)),
            _ => None,
        })
        .collect()
}

/// The golden's `C` records, one per height from `first`.
fn golden_connects(path: &Path) -> Vec<Outcome> {
    let cs: Vec<Outcome> = outcomes(path)
        .into_iter()
        .map(|(_, o)| o)
        .filter(|o| o.kind() == Kind::Connect && o.id.org == Origin::Tip)
        .collect();
    let first = cs.first().expect("a golden C").id.h;
    for (i, c) in cs.iter().enumerate() {
        assert_eq!(c.id.h, first + i as i64, "{}: one C per height", path.display());
    }
    cs
}

fn text(v: Option<&Value>) -> &[u8] {
    match v {
        Some(Value::Str(s)) => s,
        other => panic!("{other:?} is not a string"),
    }
}

fn int(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Int(i)) => *i,
        other => panic!("{other:?} is not an integer"),
    }
}

/// The G1 rules active for the block after `c`, from its `bur` and `dep`: those keys hold
/// `DeploymentActiveAfter(pindex, …)` (O8 §5.2), as G1 asks them of the block's parent.
fn g1_rules(c: &Outcome) -> Vec<String> {
    let bur = int(c.s().get("bur"));
    let mut rules: Vec<String> = G1_BURIED
        .iter()
        .filter(|(bit, _)| bur >> bit & 1 == 1)
        .map(|(_, name)| (*name).to_owned())
        .collect();
    let Some(Value::Arr(dep)) = c.s().get("dep") else {
        panic!("{c:?}: no dep")
    };
    for row in dep {
        let Value::Arr(row) = row else {
            panic!("{row:?}")
        };
        let name = String::from_utf8(text(row.first()).to_vec()).unwrap();
        if G1_VERSIONBITS.contains(&name.as_str()) && text(row.get(1)) == b"active" {
            rules.push(name);
        }
    }
    rules
}

/// Where a node's final chain first makes G1 refuse: the height and the rules, if anywhere.
type FirstEvo = Option<(i64, Vec<String>)>;

fn first_evo(node: &Node) -> FirstEvo {
    let cs = golden_connects(&node.golden());
    let (_, below_tip) = cs.split_last().expect("a golden C");
    below_tip.iter().find_map(|c| {
        let rules = g1_rules(c);
        (!rules.is_empty()).then(|| (c.id.h + 1, rules))
    })
}

fn other_params(node: &Node) -> bool {
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(node.dir.join("extract.json")).unwrap()).unwrap();
    !json["other_params_runs"].as_array().is_some_and(Vec::is_empty)
}

fn every_node() -> Vec<Node> {
    let v = Vectors::regtest_v24();
    v.tests().unwrap().iter().flat_map(|t| v.nodes(t).unwrap()).collect()
}

fn stored(label: &str) -> Node {
    let (test, name) = label.split_once('/').unwrap();
    let nodes = Vectors::regtest_v24().nodes(test).unwrap();
    nodes.into_iter().find(|n| n.name == name).unwrap_or_else(|| panic!("no node {label}"))
}

/// The evo keys are `null` in `c`: below DIP3 Core has no list, quorum, ChainLock or EHF state
/// to write, so leaving them out of a comparison passes nothing over.
fn assert_evo_null(c: &Outcome, what: &str) {
    for key in EVO_KEYS {
        assert_eq!(c.s().get(key), Some(&Value::Null), "{what} h {}: {key}", c.id.h);
    }
}

/// The non-evo set is the nodes whose final chain G1 never refuses, and every other node meets a
/// G1 rule below its tip.
#[test]
fn the_non_evo_nodes_are_the_ones_g1_never_refuses() {
    let survey: BTreeMap<String, (FirstEvo, bool)> = every_node()
        .par_iter()
        .map(|node| (node.label(), (first_evo(node), other_params(node))))
        .collect();
    let non_evo: Vec<&str> = survey
        .iter()
        .filter(|(_, (evo, _))| evo.is_none())
        .map(|(label, _)| label.as_str())
        .collect();
    assert_eq!(non_evo, NON_EVO);
    // The nodes that ran more than one parameter set, which the replay refuses, are evo too.
    for (label, (evo, other)) in &survey {
        assert!(!other || evo.is_some(), "{label}: more than one parameter set, and non-evo");
    }
    for (label, (evo, other)) in survey.iter().filter(|(_, (evo, _))| evo.is_some()) {
        let (h, rules) = evo.as_ref().unwrap();
        let other = if *other {
            "; more than one parameter set"
        } else {
            ""
        };
        eprintln!("excluded {label}: G1 refuses block {h} ({}){other}", rules.join(", "));
    }
}

/// Where Core's tip stream first leaves what a final-chain replay makes: the index among its tip
/// records and the line of the first one that is not the `C` of the next final-chain block in
/// height order. `None` when the whole stream is the final chain in order.
fn first_off_chain(tip: &[(usize, Outcome)], chain: &[(i64, Hash256)]) -> Option<(usize, usize)> {
    tip.iter().enumerate().find_map(|(k, (at, o))| {
        let next = chain.get(k);
        let on = o.kind() == Kind::Connect && next == Some(&(o.id.h, o.id.b));
        (!on).then_some((k, *at))
    })
}

/// Replays `label` strictly and compares it with its golden and its Core digest.
fn replays_like_core(label: &str, dir: &Path) {
    let node = stored(label);
    let out = dir.join(format!("{}-{}.jsonl", node.test, node.name));
    let work = dir.join(format!("{}-{}-files", node.test, node.name));
    let replayed = replay_node(&node, Policy::Strict, &out, &work)
        .unwrap_or_else(|e| panic!("{label}: {e:#}"));
    let golden = golden_connects(&node.golden());
    assert_eq!(golden[0].id.h, 0, "{label}: the golden starts at genesis");
    let chain: Vec<(i64, Hash256)> = golden.iter().map(|c| (c.id.h, c.id.b)).collect();
    let last = chain.last().unwrap();
    assert_eq!((i64::from(replayed.tip.0), &replayed.tip.1), (last.0, &last.1), "{label}");
    assert_eq!(replayed.blocks as i64, last.0, "{label}: every block but genesis");
    assert_eq!(
        chain,
        project(&node.golden()).unwrap().chain.iter().map(|s| (s.h, s.b)).collect::<Vec<_>>()
    );

    // Linear mode against the golden: clean, every height paired, every key equal.
    golden.iter().for_each(|c| assert_evo_null(c, &format!("{label} golden")));
    let report = run(&out, &node.golden(), linear_options());
    assert_eq!(report.verdict, Verdict::Clean, "{label}: {:?}", report.first());
    assert_eq!(report.verdict.exit_code(), 0);
    assert_eq!(report.pairs, chain.len() as u64, "{label}");
    assert!(report.skipped.is_empty(), "{label}: {:?}", report.skipped);
    assert!(report.uncovered.is_empty(), "{label}: {:?}", report.uncovered);
    assert_all_equal(&report, &connect_keys(&EVO_KEYS), report.pairs, label);

    // Stream mode, tip origin, against Core's digest.
    let core: Vec<(usize, Outcome)> =
        outcomes(&node.digest()).into_iter().filter(|(_, o)| o.id.org == Origin::Tip).collect();
    let off = first_off_chain(&core, &chain);
    let paired = off.map_or(core.len(), |(k, _)| k);
    for (at, c) in &core[..paired] {
        assert_evo_null(c, &format!("{label} digest line {at}"));
        assert_eq!(c.o().get("fx"), Some(&Value::Arr(Vec::new())), "{label} digest line {at}: fx");
    }
    let opts = stream_options();
    let required = opts.require.clone();
    let report = run(&out, &node.digest(), opts);
    assert_all_equal(&report, &required, paired as u64, label);
    match off {
        None => {
            assert_eq!(core.len(), chain.len(), "{label}: Core's tip stream is the final chain");
            assert_eq!(report.verdict, Verdict::Clean, "{label}: {:?}", report.first());
            assert_eq!(report.verdict.exit_code(), 0);
            assert_eq!(report.pairs, chain.len() as u64, "{label}");
            eprintln!("{label}: stream clean over {} tip records", core.len());
        }
        Some((k, line)) => {
            let first = report.first().unwrap_or_else(|| panic!("{label}: no mismatch"));
            assert_eq!(report.verdict, Verdict::Diverged, "{label}");
            assert_eq!(report.mismatches.len(), 1, "{label}: {:?}", report.mismatches);
            assert_eq!(first.right.as_ref().map(|r| r.at), Some(line), "{label}: {first:?}");
            if k < chain.len() {
                // The replay's next C, against Core's off-chain record.
                assert_eq!(first.what, What::Identity, "{label}: {first:?}");
                assert_eq!(first.left.as_ref().map(|l| l.id.h), Some(k as i64), "{label}");
                assert_eq!(report.pairs, k as u64 + 1, "{label}");
            } else {
                // The replay has ended; Core goes on.
                assert_eq!(first.what, What::Gap(Side::Left), "{label}: {first:?}");
                assert_eq!(report.pairs, k as u64, "{label}");
            }
            let r = &core[k].1;
            eprintln!(
                "{label}: stream clean over Core's first {k} of {} tip records; record {k} (line \
                 {line}) is {:?} at height {} {}, off the final chain",
                core.len(),
                r.kind(),
                r.id.h,
                r.id.b
            );
        }
    }
}

#[test]
fn every_non_evo_node_replays_like_core() {
    let dir = tempdir();
    NON_EVO.par_iter().for_each(|label| replays_like_core(label, dir.path()));
}

/// The window's pins (CH3a D146): each file's sha256, as `pins.sha256` lists it.
fn check_pins(dir: &Path) {
    let pins = std::fs::read_to_string(dir.join("pins.sha256")).unwrap();
    let raw = |name: &str| sha256_hex(&std::fs::read(dir.join(name)).unwrap());
    for (pin, name, have) in [
        (BLK_PIN, "blocks/blk00000.dat", sha256_file(&dir.join("blocks/blk00000.dat")).unwrap()),
        (GOLDEN_PIN, "golden-0-6999.jsonl", raw("golden-0-6999.jsonl")),
        (GOLDEN_ZST_PIN, "golden-0-6999.jsonl.zst", raw("golden-0-6999.jsonl.zst")),
        (LOCATED_PIN, "located-0-6999.json", raw("located-0-6999.json")),
    ] {
        assert_eq!(have, pin, "{name}");
        assert!(pins.lines().any(|l| l == format!("{pin}  {name}")), "pins.sha256: {name}");
    }
    // The compressed slice is the plain one.
    assert_eq!(sha256_file(&dir.join("golden-0-6999.jsonl.zst")).unwrap(), GOLDEN_PIN);
    assert!(
        pins.lines().any(|l| l.starts_with(&format!("{FULL_GOLDEN_PIN}  "))),
        "pins.sha256: the full golden"
    );
}

/// CH3a D150: height `h` is where the pinned `located` puts it, in the same file at the same
/// offset (after the frame's 8 bytes), framed in the network's magic, with the hash `located`
/// and the golden's `C` give it. The golden's header keys are the header's.
fn check_located(
    source: &BlockDir,
    entry: &serde_json::Value,
    h: u32,
    magic: [u8; 4],
    (header, hash): &(Header, BlockHash),
    c: &Outcome,
) {
    assert_eq!(entry["h"].as_u64(), Some(u64::from(h)));
    let file = entry["file"].as_str().unwrap();
    let file = file.strip_prefix("blk").and_then(|f| f.strip_suffix(".dat")).unwrap();
    let off = entry["off"].as_u64().unwrap();
    assert_eq!(source.location(h), Some((file.parse().unwrap(), off)), "h {h}");
    assert_eq!(entry["magic"].as_str().map(str::as_bytes), Some(hex(&magic).as_bytes()));
    let hash = Hash256::from_hex(hash.to_string().as_bytes()).unwrap();
    assert_eq!(entry["b"].as_str().map(str::as_bytes), Some(hash.to_hex().as_bytes()), "h {h}");
    assert_eq!((c.id.h, &c.id.b), (i64::from(h), &hash), "h {h}: the golden's C");
    let s = c.s();
    assert_eq!(text(s.get("mrkl")), header.merkle_root().to_string().as_bytes(), "h {h}: mrkl");
    assert_eq!(int(s.get("time")), i64::from(header.time()), "h {h}: time");
    assert_eq!(int(s.get("nonce")), i64::from(header.nonce()), "h {h}: nonce");
    assert_eq!(int(s.get("bits")), i64::from(header.bits()), "h {h}: bits");
    assert_eq!(int(s.get("ver")), i64::from(header.version()), "h {h}: ver");
    match s.get("prev") {
        Some(Value::Null) => assert_eq!(h, 0, "only genesis has no prev"),
        prev => assert_eq!(text(prev), header.prev_hash().to_string().as_bytes(), "h {h}: prev"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// CH3a D146, D147: the testnet window replays onto its golden slice, clean.
#[test]
fn a_testnet_window_replays_onto_its_golden() {
    let Some(dir) = std::env::var_os("CH3A_TESTNET_WINDOW").map(PathBuf::from) else {
        // Straight to stderr: the harness captures `eprintln!` of a passing test.
        let _ = writeln!(
            std::io::stderr(),
            "\n*** SKIPPED a_testnet_window_replays_onto_its_golden: CH3A_TESTNET_WINDOW is not \
             set. Set it to the pinned window directory (CH3a D146) and run with --release. ***\n"
        );
        return;
    };
    if cfg!(debug_assertions) {
        eprintln!("note: a debug build spends about 200 ms per C in MuHash; use --release");
    }
    check_pins(&dir);

    let p = Arc::new(ChainParamsBuilder::new(Network::Testnet3, Release::V24_0).build().unwrap());
    let golden_path = dir.join("golden-0-6999.jsonl.zst");
    let golden = golden_connects(&golden_path);
    assert_eq!((golden.first().unwrap().id.h, golden.len() as i64), (0, WINDOW_TIP + 1));
    for c in &golden {
        assert_evo_null(c, "testnet golden");
    }

    // The source over the copy's blocks directory (CH3a D150), cross-checked with the pinned
    // locator's map of the window.
    let genesis = genesis_block(&p).hash();
    let source = BlockDir::open(&dir.join("blocks"), p.message_start, genesis, None).unwrap();
    let tip = WINDOW_TIP as u32;
    assert!(source.tip() > tip, "the copy's best chain ends at {}", source.tip());
    let located: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("located-0-6999.json")).unwrap()).unwrap();
    assert_eq!(located["files"], serde_json::json!(["blk00000.dat"]));
    let entries = located["blocks"].as_array().unwrap();
    assert_eq!(entries.len(), golden.len());
    let mut last = None;
    for (h, (header, (entry, c))) in
        source.headers(0, tip).zip(entries.iter().zip(&golden)).enumerate()
    {
        let header = header.unwrap();
        check_located(&source, entry, h as u32, p.message_start, &header, c);
        last = Some(header.0);
    }
    let files: Vec<_> = source.files(tip).unwrap().into_iter().map(|(p, _)| p).collect();
    assert_eq!(files, [dir.join("blocks/blk00000.dat")]);

    let work = tempdir();
    let out = work.path().join("replay.jsonl");
    let time = i64::from(last.unwrap().time());
    let magic = p.message_start;
    let replayed =
        replay_window(p, Chain::Test, time, source.blocks(tip), &out, &work.path().join("files"))
            .unwrap();
    assert_eq!(replayed.blocks as i64, WINDOW_TIP);
    assert_eq!((i64::from(replayed.tip.0), &replayed.tip.1), (WINDOW_TIP, &golden[6999].id.b));

    let mut opts = Options::new(Mode::Linear);
    opts.from = Some(0);
    opts.to = Some(WINDOW_TIP);
    opts.require =
        WINDOW_REQUIRE.iter().map(|k| field(Kind::Connect, Class::Semantic, k)).collect();
    opts.exclude = evo_fields();
    let report = run(&out, &golden_path, opts);
    assert_eq!(report.verdict.exit_code(), 0, "{:?} {:?}", report.verdict, report.first());
    assert_eq!(report.pairs, WINDOW_TIP as u64 + 1);
    assert_eq!((report.range, report.short_range), (Some((0, WINDOW_TIP)), false));
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(report.uncovered.is_empty(), "{:?}", report.uncovered);
    let compared = connect_keys(&EVO_KEYS);
    assert_all_equal(&report, &compared, report.pairs, "testnet");
    for f in &compared {
        eprintln!("coverage {f}: equal {} of {}", report.coverage[f].equal, report.pairs);
    }
    eprintln!("testnet window: exit {}, pairs {}", report.verdict.exit_code(), report.pairs);

    // D101: the replay's undo records against Core's, each height's parent and blk file from the
    // replay's own chain.
    let ours = work.path().join("files/blocks");
    let run = BlockDir::open(&ours, magic, genesis, None).unwrap();
    assert_eq!(run.tip(), tip);
    let blocks = revfiles::run_blocks(&run, tip).unwrap();
    let rev = revfiles::compare(&ours, &dir.join("blocks"), magic, &blocks).unwrap();
    assert!(!rev.diverged(), "{:?} {:?}", rev.first, rev.run_issues);
    assert_eq!(rev.compared, tip);
    // Both sides hold heights 1..6999 first in rev00000.dat (`located-0-6999.json`), in height
    // order: the run's records are a byte prefix of Core's file.
    assert_eq!((rev.same_position, rev.same_position_to), (tip, tip));
    assert!(rev.core_ragged.is_empty(), "{:?}", rev.core_ragged);
    eprintln!(
        "rev records: equal {} of {}, {} at Core's file and offset (heights 1..={}); Core's files \
         hold {} ({} repeated)",
        rev.compared - rev.mismatched,
        rev.compared,
        rev.same_position,
        rev.same_position_to,
        rev.core_records,
        rev.core_duplicates
    );
}

/// CH3a D101 (Opus a7-3 r1 Nit 3): `digest rev-compare --to 0` is refused. On a run whose best
/// chain is genesis alone, with an empty rev file, it would otherwise compare no record and pass.
#[test]
fn rev_compare_refuses_a_run_of_no_heights() {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let genesis = core_serialize(&*genesis_block(&p));
    let dirs = tempdir();
    for side in ["core", "ours"] {
        let blocks = dirs.path().join(side);
        std::fs::create_dir(&blocks).unwrap();
        let mut blk = p.message_start.to_vec();
        blk.extend((genesis.len() as u32).to_le_bytes());
        blk.extend(&genesis);
        std::fs::write(blocks.join("blk00000.dat"), blk).unwrap();
        std::fs::write(blocks.join("rev00000.dat"), []).unwrap();
    }
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_dash-conformance"))
        .args(["digest", "rev-compare", "--chain", "regtest", "--to", "0", "--core"])
        .arg(dirs.path().join("core"))
        .arg("--ours")
        .arg(dirs.path().join("ours"))
        .output()
        .unwrap();
    let stderr = String::from_utf8(o.stderr).unwrap();
    assert_eq!(o.status.code(), Some(2), "{stderr}");
    assert_eq!(
        stderr,
        "error: --to 0: the run's undo records are heights 1..=N, so N is at least 1\n"
    );
}
