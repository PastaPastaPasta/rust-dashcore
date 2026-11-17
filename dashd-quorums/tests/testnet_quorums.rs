//! QM1a's testnet acceptance (QM1a-5): the testnet oracle's recorded `quorum listextended H` and
//! `quorum info` against the manager's answers over a replay of testnet's blocks to `H`.
//!
//! `QM1A5_TESTNET_WINDOW` names a directory whose `blocks/` holds a testnet datadir's block files,
//! and `QM1A5_TESTNET_RECORDED` a directory `contrib/node/qm1a5-testnet-record.py` wrote. The
//! test checks the recording's `SHA256SUMS`, replays genesis to the recorded height through
//! `dashd_node::sut::NodeSut`, checks the block there is the recorded one, and builds each listed
//! quorum's keys through [`QuorumManager`] as a non-member node does: `C` empty, no local
//! masternode. Then, field by field:
//!
//! - `listextended`'s types are the enabled types at `H`, and each type's hashes are route B's
//!   `ScanQuorums(type, H, signingActiveQuorumCount)` in order (`quorum listextended`, v24.0.0
//!   src/rpc/quorums.cpp:109-190, over `quorum list`'s scan, :50-107);
//! - each entry's `quorumIndex` (rotated types only), `creationHeight`, `minedBlockHash`,
//!   `numValidMembers` and `healthRatio` are the quorum's (:172-180);
//! - each listed quorum's recorded `quorum info TYPE HASH false` equals Core's `BuildQuorumInfo`
//!   object (:192-252) over the manager's keys.
//!
//! Without both variables the test says it skipped. A full-height run wants an optimized build,
//! but `test-support`'s canaries in dashd-chainstate and dashd-node refuse one without debug
//! assertions, so turn those on for the two crates only (the `--config` goes before `test`):
//!
//! ```text
//! cargo --config 'profile.release.package.dashd-chainstate.debug-assertions=true' \
//!   --config 'profile.release.package.dashd-node.debug-assertions=true' \
//!   test --release -p dashd-quorums --all-features --test testnet_quorums -- --nocapture \
//!   testnet_quorum_list
//! ```

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use dash_dml::{Dmn, MnType, testkit};
use dash_params::LlmqType;
use dash_primitives::{
    AddrV1, AddrV2, BlockHash, NIEntry, NIPurpose, NetInfo, NetInfoV1, NetInfoV2, QuorumHash,
    ServiceV1, ServiceV2,
};
use dash_types::Numeric as _;
use dashd_chainstate::llmq::quorum::QuorumBuildHook;
use dashd_node::sut::NodeSut;
use dashd_node::sut::blk::locate;
use dashd_quorums::traits::Contributions;
use dashd_quorums::{QuorumManager, VerifiedContributions};
use serde_json::{Map, Value, json};
use support::{
    Inline, Local, MemStore, RouteB, SCHEME, addresses, fresh, hex, info_json, read_phase,
};

/// `GetVerifiedContributions` on a node that is no member of any quorum: it stored no vvec, so
/// Core answers `false` (v24.0.0 src/llmq/dkgsessionmgr.cpp:145-192).
struct NoContributions;

impl VerifiedContributions for NoContributions {
    fn verified(&self, _: LlmqType, _: &BlockHash, _: &[bool]) -> Option<Contributions> {
        None
    }
}

/// `sha256sum`'s lines: `<64 hex>  <name>` each.
fn parse_sums(text: &str) -> BTreeMap<String, String> {
    let mut sums = BTreeMap::new();
    for line in text.lines() {
        let (sum, name) = line
            .split_once("  ")
            .unwrap_or_else(|| panic!("SHA256SUMS: {line:?} is no `sum  name` line"));
        assert!(
            sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()),
            "SHA256SUMS: {sum:?} is no sha256"
        );
        assert!(!name.is_empty() && !name.contains('/'), "SHA256SUMS: {name:?} is no file name");
        assert!(sums.insert(name.to_owned(), sum.to_owned()).is_none(), "SHA256SUMS: {name} twice");
    }
    sums
}

/// Every file of the recording but `SHA256SUMS` is listed there, with its sha256.
fn check_sums(dir: &Path) {
    let sums = parse_sums(&std::fs::read_to_string(dir.join("SHA256SUMS")).expect("SHA256SUMS"));
    let files: BTreeSet<String> = std::fs::read_dir(dir)
        .expect("the recording")
        .map(|e| e.expect("an entry").file_name().into_string().expect("a UTF-8 name"))
        .filter(|n| n != "SHA256SUMS")
        .collect();
    assert_eq!(files, sums.keys().cloned().collect(), "the recording's files and SHA256SUMS");
    for (name, sum) in &sums {
        let bytes = std::fs::read(dir.join(name)).expect("a recorded file");
        assert_eq!(&hex(&dash_sha256::sha256(&bytes)), sum, "{name}'s sha256");
    }
}

fn read_json(path: &Path) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn read_json_opt(path: &Path) -> Option<Value> {
    path.exists().then(|| read_json(path))
}

/// One difference between the oracle's recording (left) and ours (right).
struct Mismatch {
    quorum_type: String,
    hash: String,
    path: String,
    left: String,
    right: String,
}

/// Every difference of one run, so a full-height run reports them all at once.
#[derive(Default)]
struct Mismatches(Vec<Mismatch>);

/// How many mismatches [`Mismatches::report`] prints in full.
const SHOWN: usize = 50;

impl Mismatches {
    fn push(&mut self, quorum_type: &str, hash: &str, path: &str, left: String, right: String) {
        self.0.push(Mismatch {
            quorum_type: quorum_type.to_owned(),
            hash: hash.to_owned(),
            path: path.to_owned(),
            left,
            right,
        });
    }

    /// One mismatch if `left` and `right` differ, whole.
    fn check(&mut self, quorum_type: &str, hash: &str, path: &str, left: &Value, right: &Value) {
        if left != right {
            self.push(quorum_type, hash, path, left.to_string(), right.to_string());
        }
    }

    /// One mismatch per differing leaf: objects key by key (an absent key is `<absent>`),
    /// arrays of equal length element by element, anything else whole.
    fn diff(
        &mut self,
        quorum_type: &str,
        hash: &str,
        path: &str,
        left: Option<&Value>,
        right: Option<&Value>,
    ) {
        match (left, right) {
            (Some(Value::Object(l)), Some(Value::Object(r))) => {
                let keys: BTreeSet<&String> = l.keys().chain(r.keys()).collect();
                for k in keys {
                    self.diff(quorum_type, hash, &format!("{path}.{k}"), l.get(k), r.get(k));
                }
            }
            (Some(Value::Array(l)), Some(Value::Array(r))) if l.len() == r.len() => {
                for (i, (l, r)) in l.iter().zip(r).enumerate() {
                    self.diff(quorum_type, hash, &format!("{path}[{i}]"), Some(l), Some(r));
                }
            }
            (l, r) if l != r => {
                let show =
                    |v: Option<&Value>| v.map_or_else(|| "<absent>".into(), Value::to_string);
                self.push(quorum_type, hash, path, show(l), show(r));
            }
            _ => {}
        }
    }

    /// Prints a count per field (array indices folded to `[*]`) and the first [`SHOWN`]
    /// mismatches, then fails with the total; does nothing when there are none.
    fn report(self, height: u32) {
        if self.0.is_empty() {
            return;
        }
        let mut per_field = BTreeMap::<String, usize>::new();
        for m in &self.0 {
            *per_field.entry(fold_indices(&m.path)).or_default() += 1;
        }
        let clip = |s: &str| match s.char_indices().nth(300) {
            Some((at, _)) => format!("{}…", &s[..at]),
            None => s.to_owned(),
        };
        eprintln!("QM1a-5 testnet H={height}: mismatches per field (left: recorded, right: ours)");
        for (field, n) in &per_field {
            eprintln!("  {n:>6}  {field}");
        }
        eprintln!("first {} of {}:", SHOWN.min(self.0.len()), self.0.len());
        for m in self.0.iter().take(SHOWN) {
            eprintln!(
                "  {} {} {}  left: {}  right: {}",
                m.quorum_type,
                m.hash,
                m.path,
                clip(&m.left),
                clip(&m.right)
            );
        }
        panic!("QM1a-5 testnet H={height}: {} mismatches", self.0.len());
    }
}

/// `info.members[12].valid` → `info.members[*].valid`.
fn fold_indices(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_index = false;
    for c in path.chars() {
        match c {
            '[' => {
                in_index = true;
                out.push_str("[*");
            }
            ']' => {
                in_index = false;
                out.push(']');
            }
            _ if in_index => {}
            c => out.push(c),
        }
    }
    out
}

#[test]
fn mismatches_collect_every_leaf() {
    let mut m = Mismatches::default();
    let left = json!({"a": 1, "m": [{"v": true}, {"v": true}], "x": [1]});
    let right = json!({"a": 2, "m": [{"v": false}, {"v": true}], "x": [1, 2], "y": 0});
    m.diff("t", "h", "info", Some(&left), Some(&right));
    let got: Vec<(&str, &str, &str)> =
        m.0.iter().map(|m| (m.path.as_str(), m.left.as_str(), m.right.as_str())).collect();
    assert_eq!(
        got,
        [
            ("info.a", "1", "2"),
            ("info.m[0].v", "true", "false"),
            ("info.x", "[1]", "[1,2]"),
            ("info.y", "<absent>", "0"),
        ]
    );
    assert_eq!(fold_indices("info.m[12].v[3]"), "info.m[*].v[*]");
}

#[test]
#[should_panic(expected = "QM1a-5 testnet H=7: 2 mismatches")]
fn mismatches_fail_once_with_the_total() {
    let mut m = Mismatches::default();
    m.check("t", "-", "list.hashes", &json!(["a", "b"]), &json!(["b", "a"]));
    m.check("u", "-", "list.hashes", &json!(["a"]), &json!([]));
    m.check("v", "-", "list.hashes", &json!([]), &json!([]));
    m.report(7);
}

/// `healthRatio`: valid members over members, printed `std::fixed` with two decimals
/// (v24.0.0 src/rpc/quorums.cpp:166-169).
fn health_ratio(valid: usize, members: usize) -> String {
    let ratio = if members > 0 {
        valid as f64 / members as f64
    } else {
        0.0
    };
    format!("{ratio:.2}")
}

/// One `quorum listextended` entry as Core prints it from `route` (:172-180).
fn listextended_entry(route: &RouteB, use_rotation: bool) -> Value {
    let q = &route.quorum;
    let members = q.members.len();
    let valid = (0..members).filter(|&i| q.commitment.valid_members().get(i as u64) == Some(true));
    let valid = valid.count();
    let mut j = Map::new();
    if use_rotation {
        j.insert("quorumIndex".into(), json!(q.commitment.quorum_index().unwrap_or(0)));
    }
    j.insert("creationHeight".into(), json!(route.base_height));
    j.insert("minedBlockHash".into(), json!(q.mined_block.to_string()));
    j.insert("numValidMembers".into(), json!(valid));
    j.insert("healthRatio".into(), json!(health_ratio(valid, members)));
    Value::Object(j)
}

/// `listextended`'s types in its order, each with its `(hash, entry)` pairs in order.
fn listed(listextended: &Value) -> Vec<(String, Vec<(String, Value)>)> {
    let types = listextended.as_object().expect("listextended: an object of types");
    types
        .iter()
        .map(|(name, entries)| {
            let entries = entries.as_array().unwrap_or_else(|| panic!("{name}: not an array"));
            let entries = entries
                .iter()
                .map(|e| {
                    let e = e.as_object().unwrap_or_else(|| panic!("{name}: an entry"));
                    assert_eq!(e.len(), 1, "{name}: an entry is not keyed by one quorum hash");
                    let (hash, fields) = e.iter().next().unwrap();
                    (hash.clone(), fields.clone())
                })
                .collect();
            (name.clone(), entries)
        })
        .collect()
}

#[test]
fn parse_sums_reads_sha256sum_lines() {
    let a = "a".repeat(64);
    let b = "0".repeat(64);
    let sums = parse_sums(&format!("{a}  x.json\n{b}  SHA-y\n"));
    assert_eq!(sums, BTreeMap::from([("x.json".into(), a), ("SHA-y".into(), b)]));
}

#[test]
#[should_panic(expected = "twice")]
fn parse_sums_refuses_a_name_twice() {
    let a = "a".repeat(64);
    parse_sums(&format!("{a}  x\n{a}  x\n"));
}

#[test]
#[should_panic(expected = "no sha256")]
fn parse_sums_refuses_a_short_sum() {
    parse_sums("abc  x\n");
}

#[test]
fn health_ratio_prints_two_fixed_decimals() {
    assert_eq!(health_ratio(50, 50), "1.00");
    assert_eq!(health_ratio(49, 50), "0.98");
    assert_eq!(health_ratio(2, 3), "0.67");
    assert_eq!(health_ratio(0, 0), "0.00");
    // Exact binary ties round to even, as glibc's `std::fixed` does.
    assert_eq!(health_ratio(50, 400), "0.12");
    assert_eq!(health_ratio(150, 400), "0.38");
}

/// Masternode 1 of `mn_type` with state `version`, `net_info` and the legacy platform ports
/// 26656 (P2P) and 1443 (HTTP).
fn dmn_with(mn_type: MnType, version: i32, net_info: NetInfo) -> Dmn {
    let mut cold = testkit::cold_state(1, mn_type);
    cold.version = version;
    cold.net_info = Arc::new(net_info);
    cold.platform_p2p_port = 26656;
    cold.platform_http_port = 1443;
    let mut dmn = (*testkit::dmn(1, mn_type)).clone();
    dmn.cold = Arc::new(cold);
    dmn
}

fn v4(port: u16) -> NIEntry {
    NIEntry::Service(ServiceV2::new(AddrV2::Ipv4([1, 2, 3, 4]), port))
}

fn legacy(port: u16) -> NetInfo {
    let mut ip = [0u8; 16];
    ip[10..].copy_from_slice(&[0xff, 0xff, 1, 2, 3, 4]);
    NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(AddrV1::from_bytes(ip), port)))
}

#[test]
fn addresses_of_an_extended_core_p2p_only_net_info() {
    let mut v6 = [0u8; 16];
    v6[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
    v6[15] = 1;
    let v6 = NIEntry::Service(ServiceV2::new(AddrV2::Ipv6(v6), 19999));
    let info =
        NetInfo::Extended(NetInfoV2::new(1, vec![(NIPurpose::CoreP2p, vec![v4(19999), v6])]));
    let want = json!({"core_p2p": ["1.2.3.4:19999", "[2001:db8::1]:19999"]});
    assert_eq!(addresses(&dmn_with(MnType::Regular, 3, info.clone())), want);
    // `CanStorePlatform()`: no legacy platform fields beside an extended net info.
    assert_eq!(addresses(&dmn_with(MnType::Evo, 3, info)), want);
}

#[test]
fn addresses_of_an_extended_evo_net_info_with_platform_entries() {
    let domain = NIEntry::Domain {
        name: b"platform.example.org".to_vec(),
        port: 443,
    };
    let info = NetInfo::Extended(NetInfoV2::new(
        1,
        vec![
            (NIPurpose::CoreP2p, vec![v4(19999)]),
            (NIPurpose::PlatformP2p, vec![v4(36656)]),
            (NIPurpose::PlatformHttps, vec![domain, NIEntry::Invalid]),
        ],
    ));
    assert_eq!(
        addresses(&dmn_with(MnType::Evo, 3, info)),
        json!({
            "core_p2p": ["1.2.3.4:19999"],
            "platform_p2p": ["1.2.3.4:36656"],
            "platform_https": ["platform.example.org:443", "[invalid entry]"],
        })
    );
}

#[test]
fn addresses_of_a_legacy_net_info_are_unchanged() {
    assert_eq!(
        addresses(&dmn_with(MnType::Evo, 2, legacy(19999))),
        json!({
            "core_p2p": ["1.2.3.4:19999"],
            "platform_https": ["1.2.3.4:1443"],
            "platform_p2p": ["1.2.3.4:26656"],
        })
    );
    assert_eq!(
        addresses(&dmn_with(MnType::Regular, 2, legacy(19999))),
        json!({"core_p2p": ["1.2.3.4:19999"]})
    );
    // The null service is `IsEmpty()`, so no platform fields either.
    let null = NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(AddrV1::from_bytes([0; 16]), 0)));
    assert_eq!(addresses(&dmn_with(MnType::Evo, 1, null)), json!({}));
}

#[test]
fn addresses_of_an_empty_or_unknown_version_extended_net_info() {
    let empty = NetInfo::Extended(NetInfoV2::new(1, Vec::new()));
    assert_eq!(addresses(&dmn_with(MnType::Evo, 3, empty)), json!({}));
    // An unknown version is not `IsEmpty()` but holds no entries, and stores platform itself.
    let unknown = NetInfo::Extended(NetInfoV2::new(2, vec![(NIPurpose::CoreP2p, vec![v4(1)])]));
    assert_eq!(addresses(&dmn_with(MnType::Evo, 3, unknown)), json!({}));
}

/// The recording alone, no replay: each listed quorum has its info file and no other is there,
/// and every member's `addresses` keys are purposes `PurposeToString` names (evo/netinfo.h:92-103),
/// each an array of strings. Needs only `QM1A5_TESTNET_RECORDED`.
#[test]
fn testnet_recording_member_addresses_and_info_files() {
    let Some(recorded) = std::env::var_os("QM1A5_TESTNET_RECORDED").map(PathBuf::from) else {
        eprintln!(
            "\n*** SKIPPED testnet_recording_member_addresses_and_info_files: set QM1A5_TESTNET_RECORDED. ***\n"
        );
        return;
    };
    check_sums(&recorded);
    let height = read_json(&recorded.join("provenance.json"))["height"].as_u64().expect("height");
    let listed = listed(&read_json(&recorded.join(format!("listextended-{height}.json"))));
    let mut want = BTreeSet::new();
    for (name, entries) in &listed {
        let t = (0..=u8::MAX).map(LlmqType::from_base).find(|t| t.to_string() == *name);
        let t = t.unwrap_or_else(|| panic!("{name} is no LLMQ type"));
        want.extend(entries.iter().map(|(hash, _)| format!("info-{}-{hash}.json", t.to_base())));
    }
    let files: BTreeSet<String> = std::fs::read_dir(&recorded)
        .expect("the recording")
        .map(|e| e.expect("an entry").file_name().into_string().expect("a UTF-8 name"))
        .filter(|n| n.starts_with("info-"))
        .collect();
    assert_eq!(files, want, "the info files are the listed quorums'");
    let known = ["core_p2p", "platform_p2p", "platform_https"];
    let mut members = 0;
    for file in &files {
        for m in read_json(&recorded.join(file))["members"].as_array().expect("members") {
            for (key, value) in m["addresses"].as_object().expect("addresses") {
                assert!(known.contains(&key.as_str()), "{file}: {} has {key}", m["proTxHash"]);
                let strings = value.as_array().is_some_and(|a| a.iter().all(Value::is_string));
                assert!(strings, "{file}: {}'s {key} is {value}", m["proTxHash"]);
            }
            members += 1;
        }
    }
    eprintln!(
        "QM1a-5 recording H={height}: {} info files, {members} members' addresses",
        files.len()
    );
}

#[test]
fn testnet_quorum_list_and_info_equal_the_managers_answers() {
    let window = std::env::var_os("QM1A5_TESTNET_WINDOW").map(PathBuf::from);
    let recorded = std::env::var_os("QM1A5_TESTNET_RECORDED").map(PathBuf::from);
    let (Some(window), Some(recorded)) = (window, recorded) else {
        eprintln!(
            "\n*** SKIPPED testnet_quorum_list_and_info_equal_the_managers_answers: \
             QM1A5_TESTNET_WINDOW and QM1A5_TESTNET_RECORDED must both be set. Set the first to \
             a directory whose blocks/ holds testnet's block files and the second to a \
             contrib/node/qm1a5-testnet-record.py output directory. ***\n"
        );
        return;
    };
    let started = Instant::now();
    check_sums(&recorded);
    let provenance = read_json(&recorded.join("provenance.json"));
    let height = u32::try_from(provenance["height"].as_u64().expect("provenance: height"))
        .expect("a height");
    let recorded_hash = provenance["block_hash"].as_str().expect("provenance: block_hash");
    let listextended = read_json(&recorded.join(format!("listextended-{height}.json")));
    let listed = listed(&listextended);
    let info_files: BTreeSet<String> = std::fs::read_dir(&recorded)
        .expect("the recording")
        .map(|e| e.expect("an entry").file_name().into_string().expect("a UTF-8 name"))
        .filter(|n| n.starts_with("info-"))
        .collect();

    let argv = ["-testnet", "-printtoconsole=0", "-assumevalid=0"];
    let ui = || {
        Arc::new(dashd_node::ui::Ui::with_stderr(
            Arc::new(dashd_node::logging::Logger::new()),
            Box::new(|_| {}),
        ))
    };
    let probe = tempfile::tempdir().expect("a temp dir");
    let p = NodeSut::new(probe.path(), &argv[..2], 1_700_000_000, ui()).expect("the probe starts");
    let message_start = p.params().message_start;
    let genesis = dashd_chainstate::chain::genesis_block(p.params()).hash();
    drop(p);
    let chain = locate(&window.join("blocks"), message_start, genesis, height);
    assert_eq!(
        chain[height as usize].0.to_string(),
        recorded_hash,
        "the located block at {height}"
    );

    // A day past the last block, so none is too far ahead of the clock.
    let last = chain.last().expect("blocks").1.read();
    let mocktime = i64::from(last.header().time()) + 86_400;
    let dir = tempfile::tempdir().expect("a temp dir");
    let sut = NodeSut::new(dir.path(), &argv, mocktime, ui()).expect("the SUT starts");
    let params = Arc::new(sut.params().clone());
    let v = sut.chain().expect("V runs").clone();
    for (h, (hash, located)) in chain.iter().enumerate().skip(1) {
        let b = located.read();
        assert_eq!(b.hash(), *hash, "{h}: the located bytes");
        let r = v.process_block(b, true, None, true).recv().expect("V replies");
        assert!(r.accepted && r.state.is_valid(), "V at {h}: {r:?}");
    }
    let replayed = started.elapsed();

    let tip = chain[height as usize].0;
    let quorums: Vec<(LlmqType, QuorumHash)> = listed
        .iter()
        .flat_map(|(name, entries)| {
            let t = params
                .consensus
                .llmq_params()
                .map(|(t, _)| t)
                .find(|t| support::type_name(*t) == *name)
                .unwrap_or_else(|| panic!("listextended's {name} is no configured LLMQ type"));
            entries.iter().map(move |(hash, _)| (t, QuorumHash::from_hex(hash).expect("a hash")))
        })
        .collect();
    let reads = read_phase(&v, &params, tip, quorums.clone());

    let mut mismatches = Mismatches::default();

    // (a) The type set, and each type's hashes in order: `quorum list` at H, default count.
    let ours = reads.lists["default"].as_object().expect("route B's list");
    let theirs: BTreeSet<&String> = listed.iter().map(|(name, _)| name).collect();
    let our_types: BTreeSet<&String> = ours.keys().collect();
    mismatches.check("-", "-", "list.types", &json!(theirs), &json!(our_types));
    for (name, entries) in &listed {
        let theirs: Vec<&String> = entries.iter().map(|(hash, _)| hash).collect();
        let our_hashes = ours.get(name).unwrap_or(&Value::Null);
        mismatches.check(name, "-", "list.hashes", &json!(theirs), our_hashes);
    }

    // (b) Each entry's fields; (c) each quorum's `quorum info TYPE HASH false`.
    let manager = QuorumManager::new(
        params.clone(),
        SCHEME,
        MemStore::default(),
        NoContributions,
        Local(None),
        Inline,
    );
    let mut infos = BTreeSet::new();
    let entries = listed.iter().flat_map(|(name, entries)| entries.iter().map(move |e| (name, e)));
    for (((name, (hash, fields)), route), (t, _)) in entries.zip(&reads.quorums).zip(&quorums) {
        let file = format!("info-{}-{hash}.json", t.to_base());
        if !infos.insert(file.clone()) {
            mismatches.push(name, hash, "info.file", "listed twice".into(), "once".into());
        }
        let route = match route {
            Ok(route) => route,
            Err(e) => {
                mismatches.push(name, hash, "routeB", "a quorum".into(), e.clone());
                continue;
            }
        };
        let llmq = params.consensus.llmq(*t).expect("configured");
        assert_eq!(route.quorum.commitment.quorum_hash().to_string(), *hash, "route B's quorum");
        let want = listextended_entry(route, llmq.use_rotation);
        mismatches.diff(name, hash, "listextended", Some(fields), Some(&want));

        let Some(theirs) = read_json_opt(&recorded.join(&file)) else {
            mismatches.push(name, hash, "info.file", "<absent>".into(), file);
            continue;
        };
        let ours = fresh(&route.quorum);
        if let Err(e) = manager.built(&ours, true) {
            mismatches.push(name, hash, "info.build", "built".into(), format!("{e:?}"));
            continue;
        }
        let built = info_json(route, &ours, llmq, false);
        mismatches.diff(name, hash, "info", Some(&theirs), Some(&built));
    }
    assert_eq!(reads.quorums.len(), quorums.len(), "route B read every listed quorum");
    for file in info_files.difference(&infos) {
        mismatches.push("-", "-", "info.file", file.clone(), "<not listed>".into());
    }
    eprintln!(
        "QM1a-5 testnet H={height}: types [{}], {} quorums, {} quorum info objects compared \
         (replay {:.0?}, total {:.0?})",
        listed.iter().map(|(name, e)| format!("{name}:{}", e.len())).collect::<Vec<_>>().join(" "),
        quorums.len(),
        infos.len(),
        replayed,
        started.elapsed(),
    );
    mismatches.report(height);
}
