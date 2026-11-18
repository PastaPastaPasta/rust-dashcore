//! `protx diff` against dashd v24.0.0 on the O8 recorded runs (RP2a P2): every `h−1 → h` with
//! `extended` false and true, whole ranges, the pre-DIP3 target, an invalidated block, the
//! argument forms and §5b's combined-invalid cases.

use super::Pair;
use super::common;
use std::collections::BTreeSet;

use common::pair::{diff_chunks, field, replay};
use common::recorded::{RecordedRun, recorded_run};
use dash_args::univalue::SettingsValue as UniValue;

#[derive(Default, Debug)]
struct Reached {
    /// Replies with a new quorum.
    new_quorums: usize,
    /// Non-null `quorumsCLSigs` signatures.
    cl_sigs: usize,
    /// Non-null `quorumsCLSigs` groups holding new quorums of distinct `quorumIndex`es: rotated
    /// quorums of one cycle under one work block.
    rotated_groups: usize,
    /// Replies carrying `merkleRootQuorums`.
    quorum_roots: usize,
    /// `extended` replies listing more entries than the plain one (payout-only changes).
    extended_only: usize,
    /// Extended entries with `shares`.
    shares: usize,
    /// Evo entries.
    evo: usize,
}

fn entries(reply: &UniValue) -> Vec<UniValue> {
    match field(reply, "mnList") {
        UniValue::Arr(entries) => entries,
        other => panic!("{}", other.write()),
    }
}

fn has(value: &UniValue, key: &str) -> bool {
    matches!(value, UniValue::Obj(fields) if fields.iter().any(|(k, _)| k == key))
}

/// Replays `name` on a pair with `extra`, then compares every `h−1 → h` with `extended` false
/// and true (the base of height 1 is the null hash), and returns what the replies reached.
fn every_block(name: &str, extra: &[&str]) -> (Pair, RecordedRun, Reached) {
    let run = recorded_run(name);
    let mut args = run.args();
    args.push("-txindex=0");
    args.extend(extra);
    let mut pair = Pair::start(&args);
    replay(&mut pair, &run, 1..=run.tip());
    let zero = format!(r#""{}""#, "0".repeat(64));
    let calls: Vec<_> = (1..=run.tip())
        .flat_map(|h| {
            let base = if h == 1 {
                zero.clone()
            } else {
                (h - 1).to_string()
            };
            ["false", "true"].map(|extended| {
                (format!("{name}_{h}_{extended}"), "protx diff", format!("[{base},{h},{extended}]"))
            })
        })
        .collect();
    let replies = diff_chunks(&mut pair, &calls);
    let mut reached = Reached::default();
    for pair in replies.chunks(2) {
        let [plain, extended] = pair else {
            unreachable!()
        };
        let (plain_entries, extended_entries) = (entries(plain), entries(extended));
        reached.extended_only += usize::from(extended_entries.len() > plain_entries.len());
        reached.shares += extended_entries.iter().filter(|e| has(e, "shares")).count();
        reached.evo += plain_entries.iter().filter(|e| has(e, "platformNodeID")).count();
        let UniValue::Arr(quorums) = field(plain, "newQuorums") else {
            panic!("{}", plain.write())
        };
        reached.new_quorums += usize::from(!quorums.is_empty());
        let UniValue::Arr(sigs) = field(plain, "quorumsCLSigs") else {
            panic!("{}", plain.write())
        };
        for group in &sigs {
            let UniValue::Obj(group) = group else {
                panic!("{}", group.write())
            };
            let [(sig, UniValue::Arr(members))] = group.as_slice() else {
                panic!("{group:?}")
            };
            if sig.bytes().all(|b| b == b'0') {
                continue;
            }
            reached.cl_sigs += 1;
            let indexes: BTreeSet<i64> = members
                .iter()
                .map(|i| {
                    let i = usize::try_from(i.get_int64().unwrap()).unwrap();
                    field(&quorums[i], "quorumIndex").get_int64().unwrap()
                })
                .collect();
            reached.rotated_groups += usize::from(indexes.len() > 1);
        }
        reached.quorum_roots += usize::from(has(plain, "merkleRootQuorums"));
    }
    (pair, run, reached)
}

/// Ranges, the null and genesis bases, the reversed range and an unknown hash on a replayed run.
fn ranges(pair: &mut Pair, run: &RecordedRun, dip3: Option<usize>) {
    let tip = run.tip();
    let zero = "0".repeat(64);
    let unknown = "ab".repeat(32);
    let mut calls = vec![
        ("range_1_tip".to_owned(), "protx diff", format!("[1,{tip}]")),
        ("range_1_tip_ext".to_owned(), "protx diff", format!("[1,{tip},true]")),
        ("range_null_tip".to_owned(), "protx diff", format!(r#"["{zero}",{tip}]"#)),
        ("range_null_tip_ext".to_owned(), "protx diff", format!(r#"["{zero}",{tip},true]"#)),
        (
            "range_hashes".to_owned(),
            "protx diff",
            format!(r#"["{}","{}"]"#, run.hash(1), run.hash(tip)),
        ),
        ("equal_tip".to_owned(), "protx diff", format!("[{tip},{tip}]")),
        ("reversed".to_owned(), "protx diff", format!("[{tip},1]")),
        ("unknown_base".to_owned(), "protx diff", format!(r#"["{unknown}",1]"#)),
        ("unknown_block".to_owned(), "protx diff", format!(r#"[1,"{unknown}"]"#)),
    ];
    if let Some(dip3) = dip3 {
        for (base, block) in [(1, dip3 - 1), (dip3 - 2, dip3 - 1), (dip3 - 1, dip3), (1, dip3)] {
            calls.push((format!("dip3_{base}_{block}"), "protx diff", format!("[{base},{block}]")));
        }
    }
    let results = diff_chunks(pair, &calls);
    if let Some(dip3) = dip3 {
        let pre = &results[calls.len() - 4];
        assert_eq!(field(pre, "blockHash"), "0".repeat(64).into(), "a pre-DIP3 target at {dip3}");
        assert!(!has(pre, "merkleRootMNList"), "no roots before DIP3");
    }
}

/// An invalidated block is off the active chain for both arguments; reconsidered, it is back.
fn invalidated(pair: &mut Pair, run: &RecordedRun) {
    let tip = run.tip();
    let hash = run.hash(tip);
    pair.both("invalidate", "invalidateblock", &format!(r#"["{hash}"]"#));
    let calls = vec![
        ("side_block".to_owned(), "protx diff", format!(r#"[1,"{hash}"]"#)),
        ("side_base".to_owned(), "protx diff", format!(r#"["{hash}",1]"#)),
        ("side_base_higher".to_owned(), "protx diff", format!(r#"["{hash}",{}]"#, tip - 1)),
        ("side_tip_height".to_owned(), "protx diff", format!("[1,{tip}]")),
        ("side_new_tip".to_owned(), "protx diff", format!("[1,{}]", tip - 1)),
    ];
    diff_chunks(pair, &calls);
    pair.both("reconsider", "reconsiderblock", &format!(r#"["{hash}"]"#));
    pair.both("reconsidered", "protx diff", &format!(r#"[1,"{hash}"]"#));
}

/// Every ParseBlock form for each argument, every `extended` form, and §5b's combined-invalid
/// cases 1-10 with the run's tip in place of 10.
fn arguments(pair: &mut Pair, run: &RecordedRun) {
    let tip = run.tip();
    let hash = run.hash(5);
    let unknown_a = "ab".repeat(32);
    let unknown_b = "cd".repeat(32);
    let zero = "0".repeat(64);
    let forms = [
        "1".to_owned(),
        r#""1""#.to_owned(),
        r#""5""#.to_owned(),
        r#"" 5""#.to_owned(),
        r#""5x""#.to_owned(),
        "0".to_owned(),
        "-1".to_owned(),
        (tip + 1).to_string(),
        format!(r#""{}""#, tip + 1),
        "1.5".to_owned(),
        "1.0".to_owned(),
        "2147483648".to_owned(),
        format!(r#""{hash}""#),
        format!(r#""{}""#, hash.to_ascii_uppercase()),
        format!(r#""{zero}""#),
        format!(r#""{unknown_a}""#),
        r#""zz""#.to_owned(),
        r#""""#.to_owned(),
        "true".to_owned(),
        "false".to_owned(),
        "null".to_owned(),
        "[]".to_owned(),
        "{}".to_owned(),
    ];
    let mut calls = Vec::new();
    for (i, form) in forms.iter().enumerate() {
        calls.push((format!("base_form_{i}_{form}"), "protx diff", format!("[{form},{tip}]")));
        calls.push((format!("block_form_{i}_{form}"), "protx diff", format!("[1,{form}]")));
    }
    for (i, extended) in [
        "true",
        "false",
        "null",
        "1",
        "0",
        "2",
        r#""1""#,
        r#""yes""#,
        r#""YES""#,
        r#""no""#,
        r#""x""#,
        "1.0",
        "1e0",
        "2147483648",
        "[]",
        "{}",
    ]
    .into_iter()
    .enumerate()
    {
        calls.push((format!("extended_{i}_{extended}"), "protx diff", format!("[1,2,{extended}]")));
    }
    for (case, params) in [
        (1, r#"[0,1,"x"]"#.to_owned()),
        (2, format!(r#"[{},"zz"]"#, tip + 1)),
        (3, r#"[1,"zz","x"]"#.to_owned()),
        (4, r#"[true,"zz"]"#.to_owned()),
        (5, "[1,true]".to_owned()),
        (6, format!(r#"["{unknown_a}","{unknown_b}","x"]"#)),
        (7, format!(r#"["{unknown_a}","{unknown_b}",true]"#)),
        (8, format!(r#"["{hash}","{unknown_b}"]"#)),
        (9, format!(r#"["{}",1]"#, unknown_a.to_ascii_uppercase())),
        (10, format!(r#"["{zero}",5]"#)),
    ] {
        calls.push((format!("combined_{case}"), "protx diff", params));
    }
    for (name, method, params) in [
        ("bare", "protx diff", "[]".to_owned()),
        ("one", "protx diff", "[1]".to_owned()),
        ("four", "protx diff", "[1,2,true,1]".to_owned()),
        ("family", "protx", r#"["diff",1,2]"#.to_owned()),
        ("named", "protx diff", r#"{"baseBlock":1,"block":2,"extended":true}"#.to_owned()),
        ("family_named", "protx", r#"{"command":"diff","baseBlock":1,"block":2}"#.to_owned()),
    ] {
        calls.push((format!("dispatch_{name}"), method, params));
    }
    diff_chunks(pair, &calls);
}

#[test]
fn protx_diff_dip3_matches_dashd() {
    let _dashd = common::require_dashd();
    let (mut pair, run, reached) = every_block("feature_dip3_deterministicmns", &[]);
    ranges(&mut pair, &run, Some(135));
    arguments(&mut pair, &run);
    invalidated(&mut pair, &run);
    eprintln!("feature_dip3_deterministicmns: {reached:?}; {} comparisons", pair.count);
    pair.finish();
    let mut pair =
        Pair::start(&[&run.args()[..], &["-txindex=0", "-deprecatedrpc=permissive_bool"]].concat());
    replay(&mut pair, &run, 1..=run.tip());
    arguments(&mut pair, &run);
    eprintln!("feature_dip3_deterministicmns permissive: {} comparisons", pair.count);
    pair.finish();
}

#[test]
fn protx_diff_chainlocks_matches_dashd() {
    let _dashd = common::require_dashd();
    let (mut pair, run, reached) = every_block("feature_llmq_chainlocks", &[]);
    ranges(&mut pair, &run, None);
    invalidated(&mut pair, &run);
    eprintln!("feature_llmq_chainlocks: {reached:?}; {} comparisons", pair.count);
    assert!(reached.new_quorums > 0, "{reached:?}");
    assert!(reached.cl_sigs > 0, "{reached:?}");
    pair.finish();
}

#[test]
fn protx_diff_rotation_matches_dashd() {
    let _dashd = common::require_dashd();
    let (mut pair, run, reached) = every_block("feature_llmq_rotation", &[]);
    ranges(&mut pair, &run, None);
    eprintln!("feature_llmq_rotation: {reached:?}; {} comparisons", pair.count);
    assert!(reached.new_quorums > 0, "{reached:?}");
    assert!(reached.rotated_groups > 0, "{reached:?}");
    assert!(reached.cl_sigs > 0, "{reached:?}");
    pair.finish();
}

#[test]
fn protx_diff_coinbase_roots_match_dashd() {
    let _dashd = common::require_dashd();
    let (mut pair, run, reached) = every_block("feature_dip4_coinbasemerkleroots", &[]);
    ranges(&mut pair, &run, None);
    eprintln!("feature_dip4_coinbasemerkleroots: {reached:?}; {} comparisons", pair.count);
    assert!(reached.quorum_roots > 0, "{reached:?}");
    pair.finish();
}

#[test]
fn protx_diff_evo_matches_dashd() {
    let _dashd = common::require_dashd();
    let (mut pair, run, reached) = every_block("feature_llmq_evo", &["-deprecatedrpc=service"]);
    ranges(&mut pair, &run, None);
    eprintln!("feature_llmq_evo (service): {reached:?}; {} comparisons", pair.count);
    assert!(reached.evo > 0, "{reached:?}");
    pair.finish();
    let (pair, _, reached) = every_block("feature_llmq_evo", &[]);
    eprintln!("feature_llmq_evo: {reached:?}; {} comparisons", pair.count);
    pair.finish();
}

#[test]
fn protx_diff_shares_match_dashd() {
    let _dashd = common::require_dashd();
    let (mut extended_only, mut shares) = (0, 0);
    for name in ["feature_evo_shares_activation", "feature_masternode_shares"] {
        let (mut pair, run, reached) = every_block(name, &[]);
        ranges(&mut pair, &run, None);
        eprintln!("{name}: {reached:?}; {} comparisons", pair.count);
        extended_only += reached.extended_only;
        shares += reached.shares;
        pair.finish();
    }
    assert!(shares > 0, "no extended entry with shares");
    assert!(extended_only > 0, "no payout-only change under extended");
}
