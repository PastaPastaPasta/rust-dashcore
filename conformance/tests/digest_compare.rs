//! `digest compare` (O8 §10, node program task O9a) over Dash Core's own recordings: the regtest
//! v24 set in `test-vectors/statedigest/regtest-v24`, 81 nodes of 18 functional tests, recorded
//! by the patched dashd and extracted by O8.
//!
//! - Each node's digest, projected onto its final chain, is its golden less the golden-only keys.
//! - Core against Core: nodes of the same test agree.
//!   - Linear mode, golden against golden and digest against digest, over the heights both
//!     nodes recorded: every pair except the two `rpc_invalidateblock` pairs with node2, which
//!     mines a chain of its own by design (both diverge at height 1, as asserted). Goldens are
//!     clean; digests are incomplete, uncovered on exactly the golden-only keys and on nothing
//!     else.
//!   - Coverage rests on linear mode, which reports heights either input skips; stream mode
//!     does not detect heights both inputs skip, so it checks attempt order only.
//!   - Stream mode, tip records only: the 112 pairs in [`STREAM_CLEAN`], which differ in no key
//!     (incomplete only on the golden-only keys, as digests are). Stream mode compares
//!     every tip attempt in order, so any attempt one node made and the other did not
//!     diverges. The other 118 pairs do, for these reasons:
//!     - `feature_dip3_deterministicmns`, all pairs: blocks with a bad payee are sent to single
//!       nodes (`R bad-cb-payee`), and nodes reorg at different heights (`D`);
//!     - `feature_governance` node2, `feature_llmq_chainlocks` node0, `feature_llmq_rotation`
//!       node0 and `feature_masternode_shares` node0: the node is isolated, builds or connects
//!       blocks the others never see, and reorgs back (`D`);
//!     - `feature_llmq_chainlocks` node1: blocks with bad ChainLock signatures are sent to it
//!       only (`R bad-cbtx-invalid-clsig`, `bad-cbtx-older-clsig`);
//!     - `feature_masternode_shares` node0: invalid ProTx blocks are sent to it only;
//!     - `feature_llmq_simplepose`, all pairs but 2-3: `C.o.fx` `ban_reset` carries
//!       `ResetPlatformBan`'s result, which depends on the PlatformBan messages each node
//!       received (node-local metadata);
//!     - `rpc_invalidateblock`, all pairs: the nodes mine separate chains.
//! - Planted one-field mutations of Core's records are reported at their height and block,
//!   naming the key, by the library and by the binary, with the dump routed by chain.
//! - `RecordedDecisions` loads every node and answers each recorded attempt as recorded.
//!
//! All records here are Core's except the planted mutations, which change one value of a Core
//! record (and, for the routing checks, the `H` record's chain).

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use dash_conformance::digest::compare::{Input, compare, project};
use dash_conformance::digest::dump::RpcTarget;
use dash_conformance::digest::store::{Node, Records, Vectors, read};
use dash_conformance::rpc::Auth;
use dash_statedigest::compare::{FieldId, Mode, Options, Report, Verdict, What};
use dash_statedigest::decisions::{Policy, RecordedDecisions, split};
use dash_statedigest::{
    Chain, Class, DISCONNECT_RESULTS, FIELDS, FieldSource, Hash256, Kind, Origin, Outcome,
    REJECT_RESULTS, Record, Value, parse_line,
};
use rayon::prelude::*;

fn nodes() -> Vec<Node> {
    let vectors = Vectors::regtest_v24();
    let nodes: Vec<Node> =
        vectors.tests().unwrap().iter().flat_map(|t| vectors.nodes(t).unwrap()).collect();
    assert_eq!(nodes.len(), 81, "the regtest v24 set has 81 recorded nodes");
    nodes
}

fn by_test() -> BTreeMap<String, Vec<Node>> {
    let mut tests: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    for node in nodes() {
        tests.entry(node.test.clone()).or_default().push(node);
    }
    tests
}

/// Every pair of nodes of the same test, in test and index order.
fn node_pairs() -> Vec<(Node, Node)> {
    let mut pairs = Vec::new();
    for nodes in by_test().into_values() {
        for (i, a) in nodes.iter().enumerate() {
            pairs.extend(nodes[i + 1..].iter().map(|b| (a.clone(), b.clone())));
        }
    }
    pairs
}

fn index(node: &Node) -> u32 {
    node.name.strip_prefix("node").unwrap().parse().unwrap()
}

fn run(left: &Path, right: &Path, opts: Options) -> Report {
    let mut inputs = [Input::open(left).unwrap(), Input::open(right).unwrap()];
    compare(&mut inputs, opts).unwrap()
}

fn spec_fields(kind: Kind, class: Class, source: Option<FieldSource>) -> Vec<FieldId> {
    FIELDS
        .iter()
        .filter(|f| f.kind == kind && f.class == class && source.is_none_or(|s| f.source == s))
        .map(|f| FieldId::find(f.kind, f.class, f.key).unwrap())
        .collect()
}

/// The `C.s` keys only goldens carry (§5.3).
fn golden_only() -> Vec<FieldId> {
    spec_fields(Kind::Connect, Class::Semantic, Some(FieldSource::Golden))
}

/// The `C.s` keys the patched dashd writes.
fn patch_keys() -> Vec<FieldId> {
    spec_fields(Kind::Connect, Class::Semantic, Some(FieldSource::Patch))
}

/// The heights a node's golden covers, projected once per test binary.
fn range(node: &Node) -> (i64, i64) {
    static RANGES: Mutex<BTreeMap<PathBuf, (i64, i64)>> = Mutex::new(BTreeMap::new());
    let golden = node.golden();
    if let Some(range) = RANGES.lock().unwrap().get(&golden) {
        return *range;
    }
    let range = project(&golden).unwrap().range().unwrap();
    RANGES.lock().unwrap().insert(golden, range);
    range
}

/// The heights both nodes' goldens cover (nodes started later begin higher).
fn common_range(a: &Node, b: &Node) -> (i64, i64) {
    let ((alo, ahi), (blo, bhi)) = (range(a), range(b));
    (alo.max(blo), ahi.min(bhi))
}

fn linear(range: (i64, i64)) -> Options {
    let mut opts = Options::new(Mode::Linear);
    opts.from = Some(range.0);
    opts.to = Some(range.1);
    opts
}

fn own_chain(a: &Node, b: &Node) -> bool {
    a.test == "rpc_invalidateblock" && (a.name == "node2" || b.name == "node2")
}

#[test]
fn every_node_verifies_and_projects_onto_its_golden() {
    nodes().par_iter().for_each(|node| {
        // O8's store (bc3fa7831) binds all four files and carries blocks.dat.
        let checked = node.verify().unwrap();
        assert_eq!(checked, ["digest", "decisions", "golden", "blocks.dat"], "{}", node.label());

        let golden = project(&node.golden()).unwrap();
        let digest = project(&node.digest()).unwrap();
        assert_eq!(digest.range(), golden.range(), "{}", node.label());
        let golden_connects = outcomes(&lines(&node.golden())).count();
        assert_eq!(golden.chain.len(), golden_connects, "{}", node.label());

        let report = run(&node.digest(), &node.golden(), Options::new(Mode::Linear));
        assert!(report.mismatches.is_empty(), "{}: {:?}", node.label(), report.first());
        assert_eq!(report.pairs, golden_connects as u64, "{}", node.label());
        assert_eq!(report.verdict, Verdict::Incomplete, "{}", node.label());
        assert_eq!(report.uncovered, golden_only(), "{}", node.label());
        for field in golden_only() {
            assert_eq!(report.coverage[&field].left_absent, report.pairs);
        }
    });
}

/// A copy of a stored node's files in `dir`, with `edit` applied to its `extract.json`.
fn node_copy(dir: &Path, edit: impl FnOnce(&mut serde_json::Value)) -> Node {
    let source = nodes().into_iter().next().unwrap();
    for entry in std::fs::read_dir(&source.dir).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
    }
    let extract = dir.join("extract.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&extract).unwrap()).unwrap();
    edit(&mut json);
    std::fs::write(&extract, serde_json::to_vec(&json).unwrap()).unwrap();
    Node {
        dir: dir.to_owned(),
        ..source
    }
}

#[test]
fn a_node_missing_a_file_or_a_hash_fails_to_verify() {
    let files = [
        ("digest", "digest.jsonl.zst"),
        ("decisions", "decisions.jsonl.zst"),
        ("golden", "golden.jsonl.zst"),
        ("blocks.dat", "blocks.dat.zst"),
    ];
    let hash = |json: &mut serde_json::Value, key: &str| -> serde_json::Value {
        match key {
            "blocks.dat" => json["blocks_dat_sha256"].take(),
            _ => json[key]["sha256"].take(),
        }
    };
    let intact = tempdir();
    assert_eq!(node_copy(intact.path(), |_| ()).verify().unwrap().len(), 4);
    for (key, file) in files {
        let dir = tempdir();
        let node = node_copy(dir.path(), |_| ());
        std::fs::remove_file(dir.path().join(file)).unwrap();
        let err = node.verify().unwrap_err().to_string();
        assert!(err.ends_with(&format!("{key} is not stored")), "{key}: {err}");

        let dir = tempdir();
        let node = node_copy(dir.path(), |json| {
            hash(json, key);
        });
        let err = node.verify().unwrap_err().to_string();
        assert!(err.ends_with(&format!("extract.json has no sha256 for {key}")), "{key}: {err}");

        let dir = tempdir();
        let node = node_copy(dir.path(), |json| {
            let mut want = hash(json, key).as_str().unwrap().to_owned();
            want.replace_range(
                ..1,
                if want.starts_with('0') {
                    "1"
                } else {
                    "0"
                },
            );
            match key {
                "blocks.dat" => json["blocks_dat_sha256"] = want.into(),
                _ => json[key]["sha256"] = want.into(),
            }
        });
        let err = node.verify().unwrap_err().to_string();
        assert!(err.contains(&format!("{file}: sha256 ")), "{key}: {err}");
    }
}

#[test]
fn nodes_of_one_test_agree_linearly() {
    let clean = node_pairs()
        .par_iter()
        .filter(|(a, b)| {
            let range = common_range(a, b);
            let goldens = run(&a.golden(), &b.golden(), linear(range));
            let mut digest_opts = linear(range);
            digest_opts.require = patch_keys();
            let digests = run(&a.digest(), &b.digest(), digest_opts);
            let pair = format!("{} {}", a.label(), b.name);
            if own_chain(a, b) {
                for report in [goldens, digests] {
                    let m = report.first().unwrap_or_else(|| panic!("{pair}: no divergence"));
                    assert_eq!((m.h, &m.what), (1, &What::Identity), "{pair}");
                }
                return false;
            }
            assert_eq!(goldens.verdict, Verdict::Clean, "{pair}: {:?}", goldens.first());
            assert_eq!(goldens.range, Some(range), "{pair}");
            assert_eq!(digests.verdict, Verdict::Incomplete, "{pair}: {:?}", digests.first());
            assert_eq!(digests.uncovered, golden_only(), "{pair}");
            assert_eq!(digests.pairs, goldens.pairs, "{pair}");
            true
        })
        .count();
    assert_eq!(clean, 228);
}

/// The stream-mode pairs that compare clean on tip records; see the module docs for the rest.
enum Pairs {
    All,
    Without(&'static [u32]),
    Only(&'static [(u32, u32)]),
}

const STREAM_CLEAN: &[(&str, Pairs)] = &[
    ("feature_asset_locks", Pairs::All),
    ("feature_dip4_coinbasemerkleroots", Pairs::All),
    ("feature_evo_shares_activation", Pairs::All),
    ("feature_governance", Pairs::Without(&[2])),
    ("feature_governance_cl", Pairs::All),
    (
        "feature_llmq_chainlocks",
        Pairs::Only(&[
            (0, 5),
            (0, 6),
            (2, 3),
            (2, 4),
            (2, 5),
            (2, 6),
            (3, 4),
            (3, 5),
            (3, 6),
            (4, 5),
            (4, 6),
            (5, 6),
        ]),
    ),
    ("feature_llmq_evo", Pairs::All),
    ("feature_llmq_is_cl_conflicts", Pairs::All),
    ("feature_llmq_rotation", Pairs::Without(&[0])),
    ("feature_llmq_simplepose", Pairs::Only(&[(2, 3)])),
    ("feature_mnehf", Pairs::All),
    ("mining_basic", Pairs::All),
];

#[test]
fn follower_pairs_agree_on_tip_streams() {
    let mut require = patch_keys();
    require.push(FieldId::find(Kind::Connect, Class::Observation, "fx").unwrap());
    let gated: Vec<(Node, Node)> = node_pairs()
        .into_iter()
        .filter(|(a, b)| {
            let (i, j) = (index(a), index(b));
            STREAM_CLEAN.iter().any(|(test, pairs)| {
                a.test == *test
                    && match pairs {
                        Pairs::All => true,
                        Pairs::Without(out) => !out.contains(&i) && !out.contains(&j),
                        Pairs::Only(only) => only.contains(&(i, j)),
                    }
            })
        })
        .collect();
    assert_eq!(gated.len(), 112);
    gated.par_iter().for_each(|(a, b)| {
        let range = common_range(a, b);
        let mut opts = Options::new(Mode::Stream);
        opts.origins = Some(vec![Origin::Tip]);
        opts.from = Some(range.0);
        opts.to = Some(range.1);
        opts.require = require.clone();
        let report = run(&a.digest(), &b.digest(), opts);
        let pair = format!("{} {}", a.label(), b.name);
        assert!(report.mismatches.is_empty(), "{pair}: {:?}", report.first());
        assert_eq!(report.verdict, Verdict::Incomplete, "{pair}");
        assert_eq!(report.uncovered, golden_only(), "{pair}");
    });
}

// Planted mutations.

/// Values of `v`'s shape that differ from it, best first. The caller keeps the first one the
/// key's shape check accepts.
fn variants(v: &Value) -> Vec<Value> {
    match v {
        Value::Null => vec![Value::hash(&Hash256::from_display_bytes([0x11; 32]))],
        Value::Bool(b) => vec![Value::Bool(!b)],
        Value::Int(i) => vec![Value::Int(i ^ 1)],
        Value::Num(_) => vec![],
        Value::Str(s) => {
            let mut out = Vec::new();
            if let Some(&last) = s.last() {
                // Stays a hex digit, so a hash stays a hash.
                let other = match last {
                    b'0'..=b'8' | b'a'..=b'e' => last + 1,
                    b'9' => b'0',
                    b'f' => b'a',
                    b'x' => b'y',
                    _ => b'x',
                };
                let mut t = s.clone();
                *t.last_mut().unwrap() = other;
                out.push(Value::Str(t));
            }
            for alt in REJECT_RESULTS.iter().chain(DISCONNECT_RESULTS) {
                if alt.as_bytes() != s.as_slice() {
                    out.push(Value::string(alt));
                }
            }
            out
        }
        Value::Arr(items) => {
            let mut out = Vec::new();
            for (i, item) in items.iter().enumerate() {
                for v in variants(item) {
                    let mut t = items.clone();
                    t[i] = v;
                    out.push(Value::Arr(t));
                }
            }
            out
        }
        Value::Obj(pairs) => {
            let mut out = Vec::new();
            for (i, (_, item)) in pairs.iter().enumerate() {
                for v in variants(item) {
                    let mut t = pairs.clone();
                    t[i].1 = v;
                    out.push(Value::Obj(t));
                }
            }
            out
        }
    }
}

/// `o` with its `class.key` changed to a different value of the right shape, if it has one.
fn mutate(o: &Outcome, class: Class, key: &str) -> Option<Outcome> {
    let value = o.fields(class).get(key)?;
    variants(value).into_iter().find_map(|v| {
        let mut m = o.clone();
        let set = match class {
            Class::Semantic => m.s_mut().set(key, v),
            Class::Observation => m.o_mut().set(key, v),
            Class::Schedule => m.x_mut().set(key, v),
        };
        let line = Record::Outcome(m.clone()).to_line();
        (set.is_ok() && parse_line(&line).is_ok()).then_some(m)
    })
}

/// A file's lines, decompressed.
fn lines(path: &Path) -> Vec<Vec<u8>> {
    read(path).unwrap().split_inclusive(|&b| b == b'\n').map(<[u8]>::to_vec).collect()
}

/// The start of an `S` line. Sparse records make up most of a golden's bytes and no comparison
/// reads them, so the helpers below skip them unparsed.
const SPARSE: &[u8] = br#"{"k":"S""#;

/// The outcome records among `lines`, with their 0-based indexes.
fn outcomes(lines: &[Vec<u8>]) -> impl Iterator<Item = (usize, Outcome)> + '_ {
    lines.iter().enumerate().filter(|(_, l)| !l.starts_with(SPARSE)).filter_map(|(i, l)| {
        match parse_line(l).unwrap() {
            Record::Outcome(o) => Some((i, o)),
            _ => None,
        }
    })
}

fn write_lines(path: &Path, lines: &[Vec<u8>]) {
    std::fs::write(path, lines.concat()).unwrap();
}

/// One planted mutation: the original file (plain), the mutated copy and what was changed.
struct Planted {
    original: PathBuf,
    mutated: PathBuf,
    /// 1-based line of the mutated record.
    line: usize,
    h: i64,
    b: Hash256,
    field: FieldId,
    new: Value,
}

/// Plants the change of `class.key` in each record `pick` chooses among those of `kind` (and
/// `org`) that can take it. Both files leave out the `S` records unless `sparse`.
#[allow(clippy::too_many_arguments)]
fn plant(
    dir: &Path,
    source: &Path,
    sparse: bool,
    kind: Kind,
    org: Option<Origin>,
    class: Class,
    key: &str,
    pick: impl Fn(usize) -> Vec<usize>,
) -> Vec<Planted> {
    let mut lines = lines(source);
    if !sparse {
        lines.retain(|l| !l.starts_with(SPARSE));
    }
    let candidates: Vec<(usize, Outcome)> = outcomes(&lines)
        .filter(|(_, o)| {
            o.kind() == kind
                && org.is_none_or(|org| o.id.org == org)
                && mutate(o, class, key).is_some()
        })
        .collect();
    let field = FieldId::find(kind, class, key).unwrap();
    let tag = format!("{}-{}", (kind.letter() as char), field.to_string().replace('.', "-"));
    let original = dir.join(format!("{tag}-original.jsonl"));
    write_lines(&original, &lines);
    let mut planted = Vec::new();
    for n in pick(candidates.len()) {
        let (i, o) = &candidates[n];
        let m = mutate(o, class, key).unwrap();
        let mut copy = lines.clone();
        copy[*i] = Record::Outcome(m.clone()).to_line();
        let mutated = dir.join(format!("{tag}-{n}.jsonl"));
        write_lines(&mutated, &copy);
        planted.push(Planted {
            original: original.clone(),
            mutated,
            line: i + 1,
            h: o.id.h,
            b: o.id.b,
            field,
            new: m.fields(class).get(key).unwrap().clone(),
        });
    }
    planted
}

fn first_mid_last(n: usize) -> Vec<usize> {
    assert!(n > 0, "no record to mutate");
    let mut picks = vec![0, n / 2, n - 1];
    picks.dedup();
    picks
}

/// The mutation is the first mismatch, at its record, with exactly its key.
fn assert_reported(report: &Report, p: &Planted) {
    let m = report.first().unwrap_or_else(|| panic!("{}: not reported", p.field));
    assert_eq!((m.h, *m.b()), (p.h, p.b), "{}", p.field);
    assert_eq!(m.right.as_ref().unwrap().at, p.line, "{}", p.field);
    let What::Keys(diffs) = &m.what else {
        panic!("{}: {:?}", p.field, m.what);
    };
    assert_eq!(diffs.len(), 1, "{}", p.field);
    assert_eq!(diffs[0].field, p.field);
    assert_eq!(diffs[0].right.as_ref(), Some(&p.new));
    assert_eq!(report.verdict, Verdict::Diverged);
}

fn tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// The first node whose golden can take a change of `C.s.key` (keys like `qc` or `ehf` are
/// only set in some tests).
fn golden_with(key: &str) -> PathBuf {
    nodes()
        .into_iter()
        .map(|n| n.golden())
        .find(|g| outcomes(&lines(g)).any(|(_, o)| mutate(&o, Class::Semantic, key).is_some()))
        .unwrap_or_else(|| panic!("no golden can take a change of C.s.{key}"))
}

#[test]
fn every_semantic_key_change_is_reported_where_it_was_planted() {
    let dir = tempdir();
    let semantic = FIELDS.iter().filter(|f| f.kind == Kind::Connect && f.class == Class::Semantic);
    let mut planted = 0;
    for spec in semantic {
        let golden = golden_with(spec.key);
        let cases = plant(
            dir.path(),
            &golden,
            false,
            Kind::Connect,
            None,
            Class::Semantic,
            spec.key,
            first_mid_last,
        );
        for p in &cases {
            assert_reported(&run(&p.original, &p.mutated, Options::new(Mode::Stream)), p);
            let mut inputs = [Input::open(&p.original).unwrap(), Input::open(&p.mutated).unwrap()];
            match compare(&mut inputs, Options::new(Mode::Linear)) {
                Ok(report) => assert_reported(&report, p),
                // A changed `prev` unlinks the chain: the mutated file is no projection.
                Err(e) if spec.key == "prev" => {
                    assert!(format!("{e:#}").contains(&format!("record {}:", p.line)), "{e:#}")
                }
                Err(e) => panic!("{}: {e:#}", p.field),
            }
            planted += 1;
        }
    }
    assert_eq!(planted, 3 * 22, "three heights for each C.s key");
}

fn digest_with(kind: Kind, org: Origin, class: Class, key: &str) -> PathBuf {
    nodes()
        .into_iter()
        .map(|n| n.digest())
        .find(|d| {
            outcomes(&lines(d)).any(|(_, o)| {
                o.kind() == kind && o.id.org == org && mutate(&o, class, key).is_some()
            })
        })
        .unwrap()
}

#[test]
fn observations_compare_in_stream_mode_only_and_schedules_never() {
    let dir = tempdir();
    let fx = digest_with(Kind::Connect, Origin::Tip, Class::Observation, "fx");
    for p in plant(
        dir.path(),
        &fx,
        false,
        Kind::Connect,
        Some(Origin::Tip),
        Class::Observation,
        "fx",
        first_mid_last,
    ) {
        assert_reported(&run(&p.original, &p.mutated, Options::new(Mode::Stream)), &p);
        let linear = run(&p.original, &p.mutated, Options::new(Mode::Linear));
        assert!(linear.mismatches.is_empty(), "{:?}", linear.first());
    }

    for kind in [Kind::Connect, Kind::Reject, Kind::Disconnect] {
        let seq = digest_with(kind, Origin::Tip, Class::Schedule, "seq");
        let cases =
            plant(dir.path(), &seq, false, kind, None, Class::Schedule, "seq", first_mid_last);
        assert!(!cases.is_empty(), "no {} seq planted", kind.letter() as char);
        for p in cases {
            for mode in [Mode::Stream, Mode::Linear] {
                let report = run(&p.original, &p.mutated, Options::new(mode));
                assert!(report.mismatches.is_empty(), "{:?}", report.first());
            }
        }
    }
}

#[test]
fn reject_and_disconnect_keys_are_reported() {
    let dir = tempdir();
    for (kind, key) in [(Kind::Reject, "why"), (Kind::Reject, "res"), (Kind::Disconnect, "res")] {
        let digest = digest_with(kind, Origin::Tip, Class::Semantic, key);
        let cases = plant(
            dir.path(),
            &digest,
            false,
            kind,
            Some(Origin::Tip),
            Class::Semantic,
            key,
            first_mid_last,
        );
        for p in &cases {
            assert_reported(&run(&p.original, &p.mutated, Options::new(Mode::Stream)), p);
        }
    }
}

// The binary: exit codes, stdout and the dump's routing.

fn cli(args: &[&std::ffi::OsStr], embargo: Option<&Path>) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dash-conformance"));
    cmd.args(["digest", "compare"]).args(args);
    match embargo {
        Some(dir) => cmd.env("DASH_EMBARGO_DIR", dir),
        None => cmd.env_remove("DASH_EMBARGO_DIR"),
    };
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn os(s: &str) -> &std::ffi::OsStr {
    s.as_ref()
}

/// The dump directory the binary printed.
fn dump_dir(stdout: &str) -> PathBuf {
    PathBuf::from(stdout.lines().find_map(|l| l.strip_prefix("dump: ")).expect("a dump line"))
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Every file of the dump is 0600 and every directory 0700.
fn assert_private(dir: &Path) {
    assert_eq!(mode_of(dir), 0o700, "{}", dir.display());
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_private(&path);
        } else {
            assert_eq!(mode_of(&path), 0o600, "{}", path.display());
        }
    }
}

fn mismatch_json(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join("mismatch.json")).unwrap()).unwrap()
}

#[test]
fn the_binary_reports_each_planted_key_and_dumps_it() {
    let dir = tempdir();
    let dumps = dir.path().join("dumps");
    let semantic = FIELDS.iter().filter(|f| f.kind == Kind::Connect && f.class == Class::Semantic);
    for spec in semantic {
        let golden = golden_with(spec.key);
        let p = plant(
            dir.path(),
            &golden,
            false,
            Kind::Connect,
            None,
            Class::Semantic,
            spec.key,
            |n| vec![n / 2],
        )
        .remove(0);
        let mode = if spec.key == "prev" {
            "stream"
        } else {
            "linear"
        };
        let (code, stdout, stderr) = cli(
            &[
                os("--mode"),
                os(mode),
                os("--dump-dir"),
                dumps.as_os_str(),
                os("--label"),
                os(spec.key),
                p.original.as_os_str(),
                p.mutated.as_os_str(),
            ],
            None,
        );
        assert_eq!(code, 1, "{}: {stdout}{stderr}", p.field);
        let name = p.field.to_string();
        assert!(
            stdout.contains(&format!(
                "first mismatch at height {} block {}: keys {name}\n",
                p.h, p.b
            )),
            "{stdout}"
        );
        // Regtest: values on stdout.
        assert!(stdout.contains(&format!("  {name}: left ")), "{stdout}");
        let dump = dump_dir(&stdout);
        assert!(dump.starts_with(dumps.join("regtest").join(spec.key)), "{}", dump.display());
        let json = mismatch_json(&dump);
        assert_eq!(json["keys"][0]["field"], name.as_str());
        assert_eq!(json["h"], p.h);
        assert_eq!(json["b"], p.b.to_hex());
        assert_eq!(json["right"]["line"], p.line);
        for file in ["README.txt", "left.jsonl", "right.jsonl", "sparse-left.jsonl"] {
            assert!(dump.join(file).is_file(), "{file}");
        }
        assert_private(&dump);
    }

    // Clean and incomplete exit codes.
    let node = &nodes()[0];
    let golden = node.golden();
    let (code, ..) = cli(&[os("--no-dump"), golden.as_os_str(), golden.as_os_str()], None);
    assert_eq!(code, 0);
    let digest = node.digest();
    let (code, stdout, _) = cli(&[os("--no-dump"), digest.as_os_str(), golden.as_os_str()], None);
    assert_eq!(code, 3, "{stdout}");
    assert!(stdout.contains("uncovered (never passed): C.s.muhash"), "{stdout}");

    // Linear mode has no origin choice and compares no observation keys; a range must not be
    // inverted.
    for (args, why) in [
        (["--org", "tip"], "--org applies to stream mode only"),
        (["--require", "fx"], "--require fx: not compared in linear mode"),
        (["--from", "10"], "--from 10 is above --to 5"),
    ] {
        let (code, stdout, stderr) = cli(
            &[
                os("--no-dump"),
                os(args[0]),
                os(args[1]),
                os("--to=5"),
                golden.as_os_str(),
                golden.as_os_str(),
            ],
            None,
        );
        assert_eq!((code, stdout.as_str()), (2, ""), "{args:?}: {stderr}");
        assert!(stderr.contains(why), "{args:?}: {stderr}");
    }
    let (code, ..) = cli(
        &[os("--no-dump"), os("--from=5"), os("--to=5"), golden.as_os_str(), golden.as_os_str()],
        None,
    );
    assert_eq!(code, 0);
}

/// CH3a D167 (DEC-348 (3)): `--exclude` cannot hide a regression. A comparison clean but for its
/// excluded keys exits 4, never 0: its verdict line names them, and each is listed with the pairs
/// holding it on each side. Excluding keys leaves the others as they were: a digest stays
/// incomplete on the golden-only keys left in. An excluded key no compared record holds, an
/// unknown key, a key the mode does not compare and a key also required are errors (exit 2).
#[test]
fn the_binary_names_its_excluded_keys_and_never_passes_them() {
    let node = &nodes()[0];
    let (digest, golden) = (node.digest(), node.golden());
    let only = golden_only();
    let keys: Vec<&str> = only.iter().map(|f| f.spec().key).collect();
    let names: Vec<String> = only.iter().map(ToString::to_string).collect();
    let mut opts = Options::new(Mode::Linear);
    opts.exclude = only.clone();
    let report = run(&digest, &golden, opts);
    assert_eq!(report.verdict, Verdict::Clean, "{:?}", report.uncovered);

    let exclude = format!("--exclude={}", keys.join(","));
    let (code, stdout, stderr) =
        cli(&[os("--no-dump"), os(&exclude), digest.as_os_str(), golden.as_os_str()], None);
    assert_eq!(code, 4, "{stdout}{stderr}");
    let first = stdout.lines().next().unwrap();
    let verdict = format!(": CLEAN EXCEPT THE EXCLUDED KEYS {} (exit 4)", names.join(", "));
    assert!(first.ends_with(&verdict), "{first}");
    let mut listed = String::from("excluded, not compared (pairs holding the key: left, right):\n");
    for (f, name) in only.iter().zip(&names) {
        assert_eq!(report.excluded[f], [0, report.pairs], "{name}");
        listed += &format!("  {name:<14} 0, {}\n", report.pairs);
    }
    assert!(stdout.ends_with(&listed), "{stdout}");

    // One key out: the others stay uncovered, so the run is incomplete; the one is listed.
    let (code, stdout, _) = cli(
        &[os("--no-dump"), os("--exclude=muhash"), digest.as_os_str(), golden.as_os_str()],
        None,
    );
    assert_eq!(code, 3, "{stdout}");
    let uncovered = stdout.lines().find(|l| l.starts_with("uncovered (never passed): ")).unwrap();
    assert!(!uncovered.contains("C.s.muhash") && uncovered.contains("C.s.txouts"), "{uncovered}");
    assert!(stdout.ends_with(&format!("  C.s.muhash     0, {}\n", report.pairs)), "{stdout}");

    // A golden against itself holds the key on both sides.
    let mut opts = Options::new(Mode::Linear);
    opts.exclude = vec![FieldId::find(Kind::Connect, Class::Semantic, "muhash").unwrap()];
    let pairs = run(&golden, &golden, opts).pairs;
    let (code, stdout, _) = cli(
        &[os("--no-dump"), os("--exclude=muhash"), golden.as_os_str(), golden.as_os_str()],
        None,
    );
    assert_eq!(code, 4, "{stdout}");
    assert!(stdout.ends_with(&format!("  C.s.muhash     {pairs}, {pairs}\n")), "{stdout}");

    // Each input against itself: a digest holds no golden-only key.
    let unheld = "--exclude C.s.muhash: no compared record holds it, on either side";
    for (args, why, input) in [
        (&["--exclude=muhash"][..], unheld, &digest),
        (&["--exclude=bogus"], "--exclude bogus: not a compared key", &golden),
        (&["--exclude=fx"], "--exclude fx: not compared in linear mode", &golden),
        (
            &["--exclude=muhash", "--require=muhash"],
            "--exclude C.s.muhash: also a --require key",
            &golden,
        ),
    ] {
        let mut argv = vec![os("--no-dump")];
        argv.extend(args.iter().map(|a| os(a)));
        argv.extend([input.as_os_str(), input.as_os_str()]);
        let (code, stdout, stderr) = cli(&argv, None);
        assert_eq!((code, stdout.as_str()), (2, ""), "{args:?}: {stderr}");
        assert_eq!(stderr, format!("error: {why}\n"), "{args:?}");
    }
}

/// `source` with its tip connects at heights 5 to 7 removed and a new run (an `H`) starting
/// before height 8: a restart that rolled 5 to 7 forward unrecorded (spec §1).
fn with_hole(source: &Path, out: &Path) {
    let mut lines = lines(source);
    let tip = |h: i64| {
        move |(_, o): &(usize, Outcome)| {
            o.kind() == Kind::Connect && o.id.org == Origin::Tip && o.id.h == h
        }
    };
    let (at8, _) = outcomes(&lines).find(tip(8)).expect("a tip connect at height 8");
    let gone: Vec<usize> = (5..=7)
        .flat_map(|h| outcomes(&lines).filter(tip(h)).map(|(i, _)| i).collect::<Vec<_>>())
        .collect();
    assert_eq!(gone.len(), 3);
    let Record::Header(mut h) = parse_line(&lines[0]).unwrap() else {
        panic!("not an H")
    };
    h.run = b"hole".to_vec();
    lines.insert(at8, Record::Header(h).to_line());
    for i in gone.into_iter().rev() {
        lines.remove(i);
    }
    write_lines(out, &lines);
}

#[test]
fn unrecorded_heights_are_never_clean() {
    let dir = tempdir();
    let golden = nodes()[0].golden();
    let holed = dir.path().join("holed.jsonl");
    with_hole(&golden, &holed);
    assert_eq!(project(&holed).unwrap().holes.len(), 1);

    // The same hole in both inputs: no mismatch, but incomplete.
    let (code, stdout, stderr) =
        cli(&[os("--no-dump"), holed.as_os_str(), holed.as_os_str()], None);
    assert_eq!(code, 3, "{stdout}{stderr}");
    assert!(
        stdout.contains("unrecorded heights (uncovered): left 5..=7, right 5..=7\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("mismatch"), "{stdout}");

    // A hole on one side: a gap at its first height, and the dump lists the hole.
    let dumps = dir.path().join("dumps");
    let (code, stdout, stderr) =
        cli(&[os("--dump-dir"), dumps.as_os_str(), golden.as_os_str(), holed.as_os_str()], None);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stdout.contains("first mismatch at height 5 "), "{stdout}");
    assert!(stdout.contains("gap: the right lacks this record"), "{stdout}");
    assert!(stdout.contains("unrecorded heights (uncovered): right 5..=7\n"), "{stdout}");
    let json = mismatch_json(&dump_dir(&stdout));
    assert_eq!(json["skipped"], serde_json::json!([{"side": "right", "from": 5, "to": 7}]));
}

#[test]
fn mainnet_and_testnet_comparisons_need_the_private_corpus_even_without_a_dump() {
    let dir = tempdir();
    let golden = nodes()[0].golden();
    let corpus = dir.path().join("corpus");
    std::fs::create_dir(&corpus).unwrap();
    for chain in [Chain::Main, Chain::Test] {
        let input = dir.path().join(format!("{}.jsonl", chain.as_str()));
        rechain(&golden, chain, &input);
        let args = [os("--no-dump"), input.as_os_str(), input.as_os_str()];
        for embargo in [None, Some(Path::new("relative")), Some(&dir.path().join("missing"))] {
            let (code, stdout, stderr) = cli(&args, embargo);
            assert_eq!((code, stdout.as_str()), (2, ""), "{}: {embargo:?}", chain.as_str());
            assert!(stderr.contains("DASH_EMBARGO_DIR"), "{stderr}");
        }
        let (code, stdout, _) = cli(&args, Some(&corpus));
        assert_eq!(code, 0, "{}: {stdout}", chain.as_str());
    }
}

/// The lines of `path` with the `H` record's chain set to `chain`.
fn rechain(path: &Path, chain: Chain, out: &Path) {
    let lines: Vec<Vec<u8>> = lines(path)
        .into_iter()
        .map(|l| match parse_line(&l).unwrap() {
            Record::Header(mut h) => {
                h.chain = chain;
                Record::Header(h).to_line()
            }
            _ => l,
        })
        .collect();
    write_lines(out, &lines);
}

#[test]
fn testnet_dumps_go_to_the_private_corpus_only() {
    let dir = tempdir();
    let p = plant(
        dir.path(),
        &nodes()[0].golden(),
        true,
        Kind::Connect,
        None,
        Class::Semantic,
        "mrkl",
        |n| vec![n / 2],
    )
    .remove(0);
    let left = dir.path().join("test-left.jsonl");
    let right = dir.path().join("test-right.jsonl");
    rechain(&p.original, Chain::Test, &left);
    rechain(&p.mutated, Chain::Test, &right);
    let secret = String::from_utf8(p.new.to_vec()).unwrap();

    // No private corpus: refused before anything is compared or shown.
    let (code, stdout, stderr) = cli(&[left.as_os_str(), right.as_os_str()], None);
    assert_eq!(code, 2, "{stdout}{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("DASH_EMBARGO_DIR"), "{stderr}");

    // A private corpus that is empty, relative or missing: refused.
    let missing = dir.path().join("missing");
    for bad in [Path::new(""), Path::new("embargo"), missing.as_path()] {
        let (code, stdout, stderr) = cli(&[left.as_os_str(), right.as_os_str()], Some(bad));
        assert_eq!((code, stdout.as_str()), (2, ""), "{}: {stderr}", bad.display());
    }

    let embargo = dir.path().join("embargo");
    std::fs::create_dir(&embargo).unwrap();
    // A --dump-dir outside it: refused.
    let outside = dir.path().join("elsewhere");
    let (code, _, stderr) = cli(
        &[os("--dump-dir"), outside.as_os_str(), left.as_os_str(), right.as_os_str()],
        Some(&embargo),
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(!outside.exists());

    // With it: the dump lands under DASH_EMBARGO_DIR/statedigest; stdout names the key only.
    let (code, stdout, stderr) = cli(&[left.as_os_str(), right.as_os_str()], Some(&embargo));
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stdout.contains("  C.s.mrkl: values in the private dump\n"), "{stdout}");
    assert!(!stdout.contains(&secret), "{stdout}");
    let dump = dump_dir(&stdout);
    assert!(dump.starts_with(embargo.join("statedigest").join("test")), "{}", dump.display());
    assert_private(&dump);
    assert_eq!(mismatch_json(&dump)["keys"][0]["field"], "C.s.mrkl");
    assert!(std::fs::read_to_string(dump.join("README.txt")).unwrap().contains(&secret));
    // The planted files kept their S records: the dump carries the nearest ones.
    let sparse = std::fs::read_to_string(dump.join("sparse-right.jsonl")).unwrap();
    assert!(sparse.lines().count() > 0 && sparse.lines().all(|l| l.contains(r#"{"k":"S""#)));
}

#[test]
fn the_dump_never_queries_an_oracle() {
    let cookie = || Auth::Cookie(PathBuf::from("/work/scratch/copy/.cookie"));
    for url in ["http://127.0.0.1:27998", "http://127.0.0.1:37998/", "127.0.0.1:27998/wallet/x"] {
        assert!(RpcTarget::new(url.into(), cookie()).is_err(), "{url}");
    }
    let oracle_cookie = Auth::Cookie(PathBuf::from("/work/chains/test/../main/.cookie"));
    assert!(RpcTarget::new("http://127.0.0.1:19998".into(), oracle_cookie).is_err());
    assert!(RpcTarget::new("http://127.0.0.1:19998".into(), cookie()).is_ok());
}

#[test]
fn recorded_decisions_answer_every_attempt_as_recorded() {
    let mut attempts = 0;
    let mut finals = 0;
    for node in nodes() {
        let rec = RecordedDecisions::load(
            &read(&node.digest()).unwrap(),
            &read(&node.decisions()).unwrap(),
        )
        .unwrap_or_else(|e| panic!("{}: {e}", node.label()));
        let recorded = Records::open(&node.decisions())
            .unwrap()
            .filter(|r| matches!(r, Ok((_, Record::Attempt(_)))))
            .count();
        assert_eq!(rec.attempts().len(), recorded, "{}", node.label());

        for policy in [Policy::Keyed, Policy::Strict] {
            let mut session = rec.session(policy);
            for a in rec.attempts() {
                let mut cursor = session.begin(&a.id.b, a.id.org).unwrap();
                assert_eq!((cursor.attempt().run, cursor.attempt().seq), (a.run, a.seq));
                for q in &a.queries {
                    let (ask, answer) = split(q);
                    assert_eq!(cursor.ask(&ask), Ok(answer), "{} line {}", node.label(), a.line);
                }
                assert_eq!(cursor.finish(), Ok(vec![]));
            }
        }
        attempts += rec.attempts().len();

        // The golden's C of each block is the digest's last tip C of it (§9.2), over every
        // run; `final_connect` names that C's attempt.
        let mut last: BTreeMap<Hash256, (usize, i64)> = BTreeMap::new();
        let mut run_index = 0;
        for item in Records::open(&node.digest()).unwrap() {
            match item.unwrap().1 {
                Record::Header(_) => run_index += 1,
                Record::Outcome(c) if c.kind() == Kind::Connect && c.id.org == Origin::Tip => {
                    let seq = c.x().get("seq").and_then(Value::as_int).unwrap();
                    last.insert(c.id.b, (run_index, seq));
                }
                _ => {}
            }
        }
        for item in Records::open(&node.golden()).unwrap() {
            let Record::Outcome(c) = item.unwrap().1 else {
                continue;
            };
            let at = format!("{} h {}", node.label(), c.id.h);
            let key = rec.final_connect(&c.id.b).unwrap_or_else(|| panic!("{at}: none"));
            let attempt = rec.get(&key).unwrap();
            assert!(attempt.connected, "{at}");
            assert_eq!(Some(&(attempt.run, attempt.seq)), last.get(&c.id.b), "{at}");
            finals += 1;
        }
    }
    assert!(attempts > 0 && finals > 0);
}
