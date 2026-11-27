//! Dash Core v24.0.3 observer goldens for the evo DIFF tests (RP2a P4(c)): dashd's reply to every
//! call the `protx` and `quorum` DIFF tests of this binary compare, stored per recorded run in
//! `test-vectors/rp2a/<run>/replies.jsonl.zst` with its `PROVENANCE.md` (`fresh` holds the pairs
//! that replay no run), and replayed against rdashd alone.
//!
//! Record mode is the ignored test [`record_goldens`], with dashd v24.0.3 set up
//! (`eval $(DASHVERSION=24.0.3 python3 contrib/setup-dashd.py)`):
//!
//! ```text
//! cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens
//! ```
//!
//! It runs every DIFF test in [`TESTS`] live, dashd against rdashd under their own conditions
//! (`-txindex=0`, no peers, the `protx list/info/listdiff` pairs' mock time pinned to
//! [`PINNED_MOCKTIME`]), keeps each finished pair's records (`pair::recording`), and writes the
//! goldens only when every test passed. A golden's lines are JSON: a section header
//! `{"section":n,"test":…,"args":[…]}` per pair (its extra node arguments), then that pair's
//! calls in order, `{"name","method","params","batch":[ordinal,index]|null,"kind",…}`:
//!
//! - `"kind":"body"`, `"body"`: the reply object, compared byte for byte (`collateralAddress`
//!   absence, every `metaInfo` field, `*Elapsed` too, and every ordinary error text);
//! - `"kind":"listdiff"`, `"body"`: `protx listdiff`, its `updatedMNs` compared as a set
//!   (`pair::sorted_updates`), every other field byte for byte;
//! - `"kind":"internal_bug"`, `"code"`, `"check"`, `"function"`: compared as
//!   `pair::both_internal_bug_of` does, as the rest of the message names each node's own package,
//!   source and report URL. The reference must have failed [`PINDEX_MINED`] with code -1, in
//!   recording and in replay, or the case fails as "reference did not fail".
//!
//! [`replay_goldens`] starts rdashd per section with its arguments, sends the calls again in the
//! same batches (or alone), and compares rdashd's replies with the stored ones by the same kinds.
//! The batch envelope (headers, whitespace and bytes between the items) is compared only live; a
//! batched call's stored body is its item as canonical JSON (UniValue keeps number text and key
//! order), compared with rdashd's item written the same way. Up to [`REPLAY_NODES`] sections
//! replay at once.

use super::common;
use super::protx::{PINDEX_MINED, PINNED_MOCKTIME};
use common::pair::{
    DASHD_ARGS, NODE_ARGS, Observed, Policy, Record, Section, call, empty_warnings, exchange, post,
    recording, solo,
};
use dash_args::univalue::SettingsValue as UniValue;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The DIFF tests whose dashd replies the goldens keep.
pub const TESTS: [(&str, fn()); 18] = [
    ("family_usage_matches_dashd_v24", super::family_usage_matches_dashd_v24),
    ("protx_diff_dip3_matches_dashd", super::diff::protx_diff_dip3_matches_dashd),
    ("protx_diff_chainlocks_matches_dashd", super::diff::protx_diff_chainlocks_matches_dashd),
    ("protx_diff_rotation_matches_dashd", super::diff::protx_diff_rotation_matches_dashd),
    ("protx_diff_coinbase_roots_match_dashd", super::diff::protx_diff_coinbase_roots_match_dashd),
    ("protx_diff_evo_matches_dashd", super::diff::protx_diff_evo_matches_dashd),
    ("protx_diff_shares_match_dashd", super::diff::protx_diff_shares_match_dashd),
    (
        "protx_list_info_listdiff_dip3_match_dashd",
        super::protx::protx_list_info_listdiff_dip3_match_dashd,
    ),
    (
        "protx_list_info_listdiff_evo_match_dashd",
        super::protx::protx_list_info_listdiff_evo_match_dashd,
    ),
    (
        "protx_list_info_listdiff_shares_match_dashd",
        super::protx::protx_list_info_listdiff_shares_match_dashd,
    ),
    (
        "protx_list_info_listdiff_shares_activation_match_dashd",
        super::protx::protx_list_info_listdiff_shares_activation_match_dashd,
    ),
    ("quorum_rotation_matches_dashd", super::quorum::quorum_rotation_matches_dashd),
    ("quorum_type_activation_matches_dashd", super::quorum::quorum_type_activation_matches_dashd),
    ("quorum_simplepose_matches_dashd", super::quorum::quorum_simplepose_matches_dashd),
    ("quorum_chainlocks_matches_dashd", super::quorum::quorum_chainlocks_matches_dashd),
    ("quorum_single_member_matches_dashd", super::quorum::quorum_single_member_matches_dashd),
    ("quorum_parameter_forms_match_dashd", super::quorum::quorum_parameter_forms_match_dashd),
    (
        "quorum_pre_v19_key_after_v19_matches_dashd",
        super::quorum::quorum_pre_v19_key_after_v19_matches_dashd,
    ),
];

/// The run label of the pairs that replay no recorded run.
const FRESH: &str = "fresh";

const REPLIES: &str = "replies.jsonl.zst";

/// How many rdashd nodes [`replay_goldens`] runs at once.
const REPLAY_NODES: usize = 12;

fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-vectors/rp2a")
}

/// One golden section: the test whose pair it was and the pair's section.
struct Stored {
    test: String,
    section: Section,
}

#[test]
#[ignore = "records the goldens from dashd v24.0.3 (see the module docs)"]
fn record_goldens() {
    let dashd = common::require_dashd();
    let outcomes: Vec<_> = std::thread::scope(|scope| {
        let running: Vec<_> = TESTS
            .iter()
            .map(|(name, test)| (*name, scope.spawn(move || recording(test).1)))
            .collect();
        running.into_iter().map(|(name, thread)| (name, thread.join())).collect()
    });
    let failed: Vec<_> = outcomes.iter().filter(|(_, o)| o.is_err()).map(|(n, _)| *n).collect();
    assert!(failed.is_empty(), "no goldens written: {failed:?} failed");
    let mut runs: BTreeMap<String, Vec<Stored>> = BTreeMap::new();
    for (test, outcome) in outcomes {
        for section in outcome.unwrap() {
            for record in &section.records {
                let policy = record.reply.policy();
                assert!(
                    policy != Policy::InternalBug || record.reply.failed(Some(PINDEX_MINED)),
                    "{test} {}: reference did not fail",
                    record.name
                );
            }
            let run = section.run.clone().unwrap_or_else(|| FRESH.to_owned());
            runs.entry(run).or_default().push(Stored {
                test: test.to_owned(),
                section,
            });
        }
    }
    let binary = std::fs::read(&dashd).unwrap();
    let dashd_sha = bitcoin_hashes::sha256::Hash::hash(&binary).to_string();
    let home = std::env::var("HOME").unwrap_or_default();
    let shown = dashd.display().to_string().replacen(&home, "~", 1);
    let when = std::process::Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output();
    let when = String::from_utf8(when.unwrap().stdout).unwrap().trim().to_owned();
    if goldens_dir().exists() {
        std::fs::remove_dir_all(goldens_dir()).unwrap();
    }
    for (run, stored) in &runs {
        let dir = goldens_dir().join(run);
        std::fs::create_dir_all(&dir).unwrap();
        let text = write_golden(stored);
        let packed = zstd::encode_all(text.as_bytes(), 19).unwrap();
        std::fs::write(dir.join(REPLIES), &packed).unwrap();
        let golden_sha = bitcoin_hashes::sha256::Hash::hash(&packed).to_string();
        let provenance = provenance(run, stored, &shown, &dashd_sha, &golden_sha, &when);
        std::fs::write(dir.join("PROVENANCE.md"), provenance).unwrap();
        let calls: usize = stored.iter().map(|s| s.section.records.len()).sum();
        eprintln!("golden {run}: {} sections, {calls} calls recorded", stored.len());
    }
}

fn obj(fields: Vec<(&str, UniValue)>) -> UniValue {
    UniValue::Obj(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn num(n: usize) -> UniValue {
    UniValue::from(i64::try_from(n).unwrap())
}

/// A golden's JSON lines.
fn write_golden(stored: &[Stored]) -> String {
    let mut out = String::new();
    for (
        n,
        Stored {
            test,
            section,
        },
    ) in stored.iter().enumerate()
    {
        let args = UniValue::Arr(section.args.iter().map(|a| UniValue::from(a.as_str())).collect());
        let header =
            obj(vec![("section", num(n)), ("test", UniValue::from(test.as_str())), ("args", args)]);
        out.push_str(&header.write());
        out.push('\n');
        for record in &section.records {
            let batch = match record.batch {
                Some((ordinal, index)) => UniValue::Arr(vec![num(ordinal), num(index)]),
                None => UniValue::Null,
            };
            let mut fields = vec![
                ("name", UniValue::from(record.name.as_str())),
                ("method", UniValue::from(record.method.as_str())),
                ("params", UniValue::from(record.params.as_str())),
                ("batch", batch),
            ];
            match &record.reply {
                Observed::Body(body) => fields.extend([
                    ("kind", UniValue::from("body")),
                    ("body", UniValue::from(body.as_str())),
                ]),
                Observed::ListDiff(body) => fields.extend([
                    ("kind", UniValue::from("listdiff")),
                    ("body", UniValue::from(body.as_str())),
                ]),
                Observed::InternalBug {
                    code,
                    check,
                    function,
                } => fields.extend([
                    ("kind", UniValue::from("internal_bug")),
                    ("code", UniValue::from(*code)),
                    ("check", UniValue::from(check.as_str())),
                    ("function", UniValue::from(function.as_str())),
                ]),
            }
            out.push_str(&obj(fields).write());
            out.push('\n');
        }
    }
    out
}

/// A golden's lines read back.
fn read_golden(text: &str) -> Vec<Stored> {
    let mut out: Vec<Stored> = Vec::new();
    for line in text.lines() {
        let value = UniValue::read(line.as_bytes()).unwrap_or_else(|| panic!("{line}"));
        let UniValue::Obj(fields) = &value else {
            panic!("{line}")
        };
        let get = |k: &str| fields.iter().find(|(key, _)| key == k).map(|(_, v)| v);
        let text = |k: &str| get(k).unwrap().get_str().unwrap().to_owned();
        if get("section").is_some() {
            let args = get("args").unwrap().get_values().unwrap();
            out.push(Stored {
                test: text("test"),
                section: Section {
                    run: None,
                    args: args.iter().map(|a| a.get_str().unwrap().to_owned()).collect(),
                    records: Vec::new(),
                },
            });
            continue;
        }
        let batch = match get("batch").unwrap() {
            UniValue::Null => None,
            UniValue::Arr(pair) => {
                let n = |v: &UniValue| usize::try_from(v.get_int64().unwrap()).unwrap();
                Some((n(&pair[0]), n(&pair[1])))
            }
            other => panic!("{}", other.write()),
        };
        let reply = match text("kind").as_str() {
            "body" => Observed::Body(text("body")),
            "listdiff" => Observed::ListDiff(text("body")),
            "internal_bug" => Observed::InternalBug {
                code: get("code").unwrap().get_int64().unwrap(),
                check: text("check"),
                function: text("function"),
            },
            kind => panic!("kind {kind}"),
        };
        out.last_mut().expect("a section header first").section.records.push(Record {
            name: text("name"),
            method: text("method"),
            params: text("params"),
            batch,
            reply,
        });
    }
    out
}

fn provenance(
    run: &str,
    stored: &[Stored],
    dashd: &str,
    dashd_sha: &str,
    golden_sha: &str,
    when: &str,
) -> String {
    let chain = match run {
        FRESH => "none: the pairs start on a fresh regtest chain".to_owned(),
        "v19-window-r2" => {
            "`dashd-chainstate/tests/data/v19-window/r2/chain.txt`, by `submitblock`".to_owned()
        }
        _ => format!(
            "`test-vectors/statedigest/regtest-v24/{run}/node0/blocks.dat.zst`, by `submitblock`"
        ),
    };
    let calls: usize = stored.iter().map(|s| s.section.records.len()).sum();
    let mut out = format!(
        "# RP2a observer goldens: `{run}`\n\n\
         Dash Core v24.0.3's replies to the evo DIFF tests' calls on `{run}`, recorded by \
         `golden::record_goldens` in `rdashd/tests/rpc_diff_evo/golden.rs` (its module docs give \
         the format and the comparison kinds) while each test compared dashd with rdashd live.\n\n\
         | | |\n|---|---|\n\
         | Recorded | {when} |\n\
         | Command | `cargo test -p rdashd --test rpc_diff_evo -- --ignored --exact golden::record_goldens` |\n\
         | dashd | `{dashd}` (`contrib/setup-dashd.py`), sha256 `{dashd_sha}` |\n\
         | Chain | {chain} |\n\
         | Node args | `-datadir=<tmp> {}`, dashd also `{}`, then the section's args |\n\
         | Peers | none: `-listen=0`, no `-connect` or `-addnode`, and regtest has no seeds |\n\
         | `{REPLIES}` sha256 | `{golden_sha}` |\n\
         | Calls | {calls} in {} sections |\n\n\
         ## Sections\n\n\
         | # | Test | Section args | Mock time | Calls | Internal bugs |\n|---|---|---|---|---|---|\n",
        NODE_ARGS.join(" "),
        DASHD_ARGS.join(" "),
        stored.len(),
    );
    for (
        n,
        Stored {
            test,
            section,
        },
    ) in stored.iter().enumerate()
    {
        let mocktime = section
            .records
            .iter()
            .find(|r| r.method == "setmocktime")
            .map_or("not set".to_owned(), |r| format!("`{}` pinned after the replay", r.params));
        let bugs =
            section.records.iter().filter(|r| r.reply.policy() == Policy::InternalBug).count();
        out.push_str(&format!(
            "| {n} | `{test}` | `{}` | {mocktime} | {} | {bugs} |\n",
            section.args.join(" "),
            section.records.len()
        ));
    }
    out.push_str(&format!(
        "\nThe `protx list/info/listdiff` pairs pin the mock time to {PINNED_MOCKTIME} once the \
         chain is replayed, so `metaInfo` and its `*Elapsed` fields are fixed; the other pairs \
         compare no reply that reads the clock.\n"
    ));
    out
}

/// Every golden's sections against rdashd alone.
#[test]
fn replay_goldens() {
    let dir = goldens_dir();
    let mut runs: Vec<_> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries.map(|e| e.unwrap().path()).filter(|p| p.join(REPLIES).is_file()).collect()
        })
        .unwrap_or_default();
    runs.sort();
    assert!(!runs.is_empty(), "no goldens under {}: record them (golden.rs)", dir.display());
    let goldens: Vec<(String, Vec<Stored>)> = runs
        .iter()
        .map(|path| {
            let run = path.file_name().unwrap().to_string_lossy().into_owned();
            let packed = std::fs::read(path.join(REPLIES)).unwrap();
            let sha = bitcoin_hashes::sha256::Hash::hash(&packed).to_string();
            let provenance = std::fs::read_to_string(path.join("PROVENANCE.md")).unwrap();
            assert!(
                provenance.contains(&format!("| `{REPLIES}` sha256 | `{sha}` |")),
                "golden {run}: {REPLIES} is not the file its PROVENANCE.md names"
            );
            let text = String::from_utf8(zstd::decode_all(&packed[..]).unwrap()).unwrap();
            let stored = read_golden(&text);
            assert!(!stored.is_empty(), "golden {run}: no sections");
            for s in &stored {
                assert!(!s.section.records.is_empty(), "golden {run}: {} has no calls", s.test);
            }
            (run, stored)
        })
        .collect();
    let recorded: BTreeSet<&str> =
        goldens.iter().flat_map(|(_, stored)| stored.iter().map(|s| s.test.as_str())).collect();
    let expected: BTreeSet<&str> = TESTS.iter().map(|(name, _)| *name).collect();
    assert_eq!(recorded, expected, "the goldens do not cover golden::TESTS: record them again");
    // Each section on its own rdashd, up to REPLAY_NODES at once: a run's sections are
    // independent pairs.
    let sections: Vec<(usize, &str, &Section)> = goldens
        .iter()
        .enumerate()
        .flat_map(|(run, (_, stored))| {
            stored.iter().map(move |s| (run, s.test.as_str(), &s.section))
        })
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::Mutex::new(vec![(0, Vec::<String>::new()); goldens.len()]);
    std::thread::scope(|scope| {
        for _ in 0..REPLAY_NODES {
            scope.spawn(|| {
                while let Some(&(run, test, section)) =
                    sections.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                {
                    let mut failures = Vec::new();
                    let count = replay_section(section, &mut failures);
                    let mut done = done.lock().unwrap();
                    done[run].0 += count;
                    done[run].1.extend(failures.into_iter().map(|f| format!("{test}: {f}")));
                }
            });
        }
    });
    let results = done.into_inner().unwrap();
    for ((run, stored), (count, failures)) in goldens.iter().zip(&results) {
        eprintln!(
            "golden {run}: {} sections, {count} calls compared, {} differ",
            stored.len(),
            failures.len()
        );
    }
    let count: usize = results.iter().map(|(c, _)| c).sum();
    let failures: Vec<_> = results.into_iter().flat_map(|(_, f)| f).collect();
    eprintln!("goldens: {} runs, {count} calls compared", runs.len());
    assert!(count > 0, "the goldens compared nothing");
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
}

/// `section`'s calls on a fresh rdashd with its arguments, compared with the stored replies;
/// the number of calls compared.
fn replay_section(section: &Section, failures: &mut Vec<String>) -> usize {
    let home = tempfile::tempdir().unwrap();
    let args: Vec<&str> = section.args.iter().map(String::as_str).collect();
    let (node, port) = solo(home.path(), &args);
    let records = &section.records;
    let mut i = 0;
    while i < records.len() {
        let Some((ordinal, _)) = records[i].batch else {
            let reply =
                empty_warnings(&exchange(port, &call(&records[i].method, &records[i].params)));
            let reply = String::from_utf8_lossy(&reply).into_owned();
            let body = reply.split_once("\r\n\r\n").map(|(_, b)| b.to_owned());
            compare(&records[i], body, failures);
            i += 1;
            continue;
        };
        let end =
            i + records[i..].iter().take_while(|r| r.batch.is_some_and(|b| b.0 == ordinal)).count();
        let group = &records[i..end];
        let body: Vec<String> = group
            .iter()
            .map(|r| {
                let id = r.batch.unwrap().1;
                format!(r#"{{"method":"{}","params":{},"id":{id}}}"#, r.method, r.params)
            })
            .collect();
        let reply = exchange(port, &post(&format!("[{}]", body.join(","))));
        let reply = String::from_utf8_lossy(&reply).into_owned();
        let items =
            match reply.split_once("\r\n\r\n").and_then(|(_, b)| UniValue::read(b.as_bytes())) {
                Some(UniValue::Arr(items)) => items,
                _ => Vec::new(),
            };
        for (k, record) in group.iter().enumerate() {
            compare(record, items.get(k).map(UniValue::write), failures);
        }
        i = end;
    }
    node.stop_ok(libc::SIGTERM);
    records.len()
}

/// rdashd's reply `body` to `record`'s call against its stored reply, by its kind.
fn compare(record: &Record, body: Option<String>, failures: &mut Vec<String>) {
    let name = &record.name;
    if record.reply.policy() == Policy::InternalBug && !record.reply.failed(Some(PINDEX_MINED)) {
        failures.push(format!("{name}: reference did not fail {PINDEX_MINED}: {:?}", record.reply));
        return;
    }
    let ours = body.map(|b| Observed::read(record.reply.policy(), &b));
    if !ours.as_ref().is_some_and(|ours| record.reply.matches(ours)) {
        failures.push(format!("{name}:\n  dashd  {:?}\n  rdashd {ours:?}", record.reply));
    }
}
