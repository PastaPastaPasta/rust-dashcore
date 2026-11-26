//! `protx list`, `protx info` and `protx listdiff` against dashd v24.0.2 on the O8 recorded runs
//! (RP2a P1), after both nodes' mock time is pinned: the type × detailed × height matrix, the
//! list at every height, `info` at the tip and at every masternode's first and last listed
//! block (spent collateral answering Core's internal bug), `listdiff` over every `h−1 → h` and
//! whole, reversed, equal and genesis-based ranges, an invalidated block, the argument and
//! dispatch rows and §5b's combined-invalid cases 11-16 and 21-26.

use super::Pair;
use super::common;
use std::collections::{BTreeMap, BTreeSet};

use common::pair::{both_internal_bug_of, diff_chunks, field, replay, reply_on};
use common::recorded::{RecordedRun, recorded_run};
use dash_args::univalue::SettingsValue as UniValue;

/// The mock time both nodes are pinned to after the replay, so every `metaInfo` field, the
/// `*Elapsed` ones too, is the same on every run and in the goldens (`golden.rs`).
pub const PINNED_MOCKTIME: u64 = 1_767_225_600;

/// The check Core v24.0.0's `protx info` fails for a masternode whose collateral is spent by the
/// tip, asked at a block it is listed at (`BuildDMNListEntry`).
pub const PINDEX_MINED: &str = "pindexMined != nullptr";

#[derive(Default, Debug)]
struct Reached {
    /// `protx info` replies dashd answered with the pindexMined internal bug.
    spent_info: usize,
    /// Historical `protx info` replies with a body.
    historical_info: usize,
    /// `listdiff` replies adding, removing and updating masternodes.
    added: usize,
    removed: usize,
    updated: usize,
    /// `listdiff` replies adding, and removing, two or more masternodes.
    multi_added: usize,
    multi_removed: usize,
    /// Of those, replies whose order (internal-id order) is not displayed proTxHash order.
    added_off_hash_order: usize,
    removed_off_hash_order: usize,
    /// Updated entries whose diff carries `netInfo` (the replaced `addresses`).
    net_info_updates: usize,
    /// Of those, Evo nodes whose replaced `addresses` add the legacy platform endpoints.
    evo_net_info_updates: usize,
    /// Updated entries printing an address the diff does not carry (a port-only change).
    port_only_updates: usize,
    /// Detailed list entries of Evo nodes.
    evo_entries: usize,
}

fn array(value: &UniValue) -> Vec<UniValue> {
    match value {
        UniValue::Arr(items) => items.clone(),
        other => panic!("{}", other.write()),
    }
}

fn object(value: &UniValue) -> Vec<(String, UniValue)> {
    match value {
        UniValue::Obj(fields) => fields.clone(),
        other => panic!("{}", other.write()),
    }
}

/// dashd's `dip0003` activation height, from `getdeploymentinfo`.
fn dip3_height(pair: &Pair) -> usize {
    let info = field(&reply_on(pair.ports[0], "getdeploymentinfo", "[]"), "result");
    let height = field(&field(&field(&info, "deployments"), "dip0003"), "height");
    usize::try_from(height.get_int64().unwrap()).unwrap()
}

/// The sampled heights {1, DIP3−1, DIP3, mid, tip}, inside the chain.
fn sample_heights(run: &RecordedRun, dip3: usize) -> Vec<usize> {
    let tip = run.tip();
    let heights: BTreeSet<usize> = [1, dip3.saturating_sub(1), dip3, tip / 2, tip]
        .into_iter()
        .filter(|h| (1..=tip).contains(h))
        .collect();
    heights.into_iter().collect()
}

/// The type × detailed × height matrix, then `registered true` at every height. Returns
/// dashd's masternodes at each height.
fn lists(
    pair: &mut Pair,
    run: &RecordedRun,
    dip3: usize,
    reached: &mut Reached,
) -> BTreeMap<usize, BTreeSet<String>> {
    let mut calls = Vec::new();
    for h in sample_heights(run, dip3) {
        for kind in ["registered", "valid", "evo"] {
            calls.push((format!("list_{kind}_{h}"), "protx list", format!(r#"["{kind}"]"#)));
            for detailed in ["false", "true"] {
                calls.push((
                    format!("list_{kind}_{detailed}_{h}"),
                    "protx list",
                    format!(r#"["{kind}",{detailed},{h}]"#),
                ));
            }
        }
    }
    calls.push(("list_default".into(), "protx list", "[]".into()));
    calls.push(("list_named".into(), "protx list", r#"{"type":"valid","detailed":true}"#.into()));
    calls.push(("list_family".into(), "protx", r#"["list","registered",true]"#.into()));
    diff_chunks(pair, &calls);
    let calls: Vec<_> = (1..=run.tip())
        .map(|h| (format!("list_every_{h}"), "protx list", format!(r#"["registered",true,{h}]"#)))
        .collect();
    let results = diff_chunks(pair, &calls);
    let mut at = BTreeMap::new();
    for (h, result) in (1..=run.tip()).zip(results) {
        let entries = array(&result);
        reached.evo_entries +=
            entries.iter().filter(|e| field(e, "type") == UniValue::from("Evo")).count();
        let hashes =
            entries.iter().map(|e| field(e, "proTxHash").get_str().unwrap().to_owned()).collect();
        at.insert(h, hashes);
    }
    at
}

/// `protx info` at the tip for every listed masternode, then at the first and last block each
/// masternode is listed at, and before its registration. Cases dashd answers with the
/// internal bug are compared by [`both_internal_bug_of`] its check.
fn infos(
    pair: &mut Pair,
    run: &RecordedRun,
    at: &BTreeMap<usize, BTreeSet<String>>,
    reached: &mut Reached,
) {
    let tip = run.tip();
    let mut seen: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (h, hashes) in at {
        for hash in hashes {
            seen.entry(hash).and_modify(|(_, last)| *last = *h).or_insert((*h, *h));
        }
    }
    let mut calls = Vec::new();
    for hash in &at[&tip] {
        calls.push((format!("info_tip_{hash}"), "protx info", format!(r#"["{hash}"]"#)));
        calls.push((
            format!("info_tip_hash_{hash}"),
            "protx info",
            format!(r#"["{hash}","{}"]"#, run.hash(tip)),
        ));
    }
    let mut historical = Vec::new();
    for (hash, (first, last)) in &seen {
        for h in BTreeSet::from([*first, (*first + *last) / 2, *last]) {
            historical
                .push((format!("info_{h}_{hash}"), format!(r#"["{hash}","{}"]"#, run.hash(h))));
        }
        if *first > 1 {
            let h = *first - 1;
            calls.push((
                format!("info_before_{h}_{hash}"),
                "protx info",
                format!(r#"["{hash}","{}"]"#, run.hash(h)),
            ));
        }
    }
    for (name, params) in historical {
        let reply = reply_on(pair.ports[0], "protx info", &params);
        let failed = matches!(&reply, UniValue::Obj(fields) if fields.iter().any(|(k, v)| {
            k == "error" && v.write().contains("Internal bug detected")
        }));
        if failed {
            reached.spent_info += 1;
            both_internal_bug_of(pair, Some(PINDEX_MINED), &name, "protx info", &params);
        } else {
            reached.historical_info += 1;
            calls.push((name, "protx info", params));
        }
    }
    diff_chunks(pair, &calls);
}

/// `listdiff` over every `h−1 → h` (the genesis hash as the base of 1), then ranges.
fn list_diffs(pair: &mut Pair, run: &RecordedRun, dip3: usize, reached: &mut Reached) {
    let tip = run.tip();
    let genesis = field(&reply_on(pair.ports[0], "getblockhash", "[0]"), "result");
    let genesis = genesis.get_str().unwrap().to_owned();
    let mut calls: Vec<_> = (1..=tip)
        .map(|h| {
            let base = if h == 1 {
                format!(r#""{genesis}""#)
            } else {
                (h - 1).to_string()
            };
            (format!("listdiff_{h}"), "protx listdiff", format!("[{base},{h}]"))
        })
        .collect();
    let mid = tip / 2;
    for (name, params) in [
        ("forward", format!("[1,{tip}]")),
        ("reversed", format!("[{tip},1]")),
        ("equal", format!("[{mid},{mid}]")),
        ("genesis_tip", format!(r#"["{genesis}",{tip}]"#)),
        ("dip3_tip", format!("[{},{tip}]", dip3.clamp(1, tip))),
        ("dip3_mid", format!("[{},{mid}]", dip3.saturating_sub(1).clamp(1, tip))),
        ("mid_tip_reversed", format!("[{tip},{mid}]")),
        ("hashes", format!(r#"["{}","{}"]"#, run.hash(mid), run.hash(tip))),
        ("string_heights", format!(r#"["{mid}","{tip}"]"#)),
    ] {
        calls.push((format!("listdiff_{name}"), "protx listdiff", params));
    }
    for step in [2, 5, 10, 25] {
        for h in (step..=tip).step_by(step) {
            calls.push((
                format!("listdiff_step_{step}_{h}"),
                "protx listdiff",
                format!("[{},{h}]", h + 1 - step),
            ));
        }
    }
    for result in diff_chunks(pair, &calls) {
        let added: Vec<String> = array(&field(&result, "addedMNs"))
            .iter()
            .map(|dmn| field(dmn, "proTxHash").get_str().unwrap().to_owned())
            .collect();
        let removed: Vec<String> = array(&field(&result, "removedMNs"))
            .iter()
            .map(|hash| hash.get_str().unwrap().to_owned())
            .collect();
        reached.added += usize::from(!added.is_empty());
        reached.removed += usize::from(!removed.is_empty());
        reached.multi_added += usize::from(added.len() >= 2);
        reached.multi_removed += usize::from(removed.len() >= 2);
        reached.added_off_hash_order += usize::from(!added.is_sorted());
        reached.removed_off_hash_order += usize::from(!removed.is_sorted());
        let updated = array(&field(&result, "updatedMNs"));
        reached.updated += usize::from(!updated.is_empty());
        for entry in &updated {
            for (_, diff) in object(entry) {
                let Some((_, addresses)) =
                    object(&diff).into_iter().find(|(k, _)| k == "addresses")
                else {
                    continue;
                };
                if addresses.write().contains("255.255.255.255") {
                    reached.port_only_updates += 1;
                } else {
                    reached.net_info_updates += 1;
                    let text = addresses.write();
                    reached.evo_net_info_updates +=
                        usize::from(text.contains("core_p2p") && text.contains("platform_"));
                }
            }
        }
    }
}

/// The argument rows: `detailed` in every form, heights and types, info and listdiff argument
/// forms, dispatch forms, and §5b's combined-invalid cases with the run's tip in place of 10.
fn arguments(pair: &mut Pair, run: &RecordedRun) {
    let tip = run.tip();
    let hash = run.hash(5).to_owned();
    let unknown_a = "ab".repeat(32);
    let unknown_b = "cd".repeat(32);
    let mut calls = Vec::new();
    for (i, detailed) in [
        "true",
        r#""YES""#,
        "1",
        "1.0",
        "1e0",
        "2147483648",
        r#""x""#,
        "null",
        "false",
        "0",
        "2",
        r#""no""#,
        r#""1""#,
        "[]",
        "{}",
    ]
    .into_iter()
    .enumerate()
    {
        calls.push((
            format!("detailed_{i}_{detailed}"),
            "protx list",
            format!(r#"["registered",{detailed},{tip}]"#),
        ));
    }
    for (i, params) in [
        r#"["wallet"]"#.to_owned(),
        r#"["wallet","x",0]"#.to_owned(),
        r#"["foo"]"#.to_owned(),
        r#"["REGISTERED"]"#.to_owned(),
        "[1]".to_owned(),
        "[null]".to_owned(),
        r#"["valid",null,null]"#.to_owned(),
        r#"["registered",false,0]"#.to_owned(),
        r#"["registered",false,-1]"#.to_owned(),
        format!(r#"["registered",false,{}]"#, tip + 1),
        r#"["registered",false,"5"]"#.to_owned(),
        r#"["registered",false,1.5]"#.to_owned(),
        r#"["registered",false,2147483648]"#.to_owned(),
        r#"["registered",false,1,1]"#.to_owned(),
    ]
    .into_iter()
    .enumerate()
    {
        calls.push((format!("list_args_{i}"), "protx list", params));
    }
    for (i, params) in [
        "[]".to_owned(),
        r#"["zz"]"#.to_owned(),
        "[1]".to_owned(),
        format!(r#"["{unknown_a}"]"#),
        format!(r#"["{unknown_a}","{hash}"]"#),
        format!(r#"["{}"]"#, unknown_a.to_ascii_uppercase()),
        format!(r#"["{unknown_a}",null]"#),
        format!(r#"["{unknown_a}","{hash}",1]"#),
        format!(r#"["{unknown_a}",5]"#),
    ]
    .into_iter()
    .enumerate()
    {
        calls.push((format!("info_args_{i}"), "protx info", params));
    }
    let forms = [
        "1".to_owned(),
        r#""1""#.to_owned(),
        r#"" 5""#.to_owned(),
        r#""5x""#.to_owned(),
        "0".to_owned(),
        "-1".to_owned(),
        (tip + 1).to_string(),
        "1.5".to_owned(),
        "1.0".to_owned(),
        "2147483648".to_owned(),
        format!(r#""{hash}""#),
        format!(r#""{}""#, hash.to_ascii_uppercase()),
        format!(r#""{}""#, "0".repeat(64)),
        format!(r#""{unknown_a}""#),
        r#""zz""#.to_owned(),
        r#""""#.to_owned(),
        "true".to_owned(),
        "false".to_owned(),
        "null".to_owned(),
        "[]".to_owned(),
        "{}".to_owned(),
    ];
    for (i, form) in forms.iter().enumerate() {
        calls.push((
            format!("listdiff_base_{i}_{form}"),
            "protx listdiff",
            format!("[{form},{tip}]"),
        ));
        calls.push((format!("listdiff_block_{i}_{form}"), "protx listdiff", format!("[1,{form}]")));
    }
    for (case, method, params) in [
        (11, "protx listdiff", format!(r#"["{unknown_a}","{unknown_b}"]"#)),
        (12, "protx listdiff", format!(r#"[1,"{unknown_b}"]"#)),
        (13, "protx listdiff", format!(r#"[{},"zz"]"#, tip + 1)),
        (14, "protx listdiff", "[true,1]".to_owned()),
        (140, "protx listdiff", "[false,1]".to_owned()),
        (21, "protx list", r#"["foo","x"]"#.to_owned()),
        (22, "protx list", r#"["registered","x",999999]"#.to_owned()),
        (23, "protx list", r#"["registered",true,0]"#.to_owned()),
        (24, "protx info", r#"["zz","yy"]"#.to_owned()),
        (25, "protx info", format!(r#"["{hash}","yy"]"#)),
        (26, "protx info", format!(r#"["{unknown_a}","{unknown_b}"]"#)),
    ] {
        calls.push((format!("combined_{case}"), method, params));
    }
    for (name, method, params) in [
        ("listdiff_bare", "protx listdiff", "[]".to_owned()),
        ("listdiff_one", "protx listdiff", "[1]".to_owned()),
        ("listdiff_three", "protx listdiff", "[1,2,3]".to_owned()),
        ("listdiff_family", "protx", r#"["listdiff",1,2]"#.to_owned()),
        ("listdiff_named", "protx listdiff", r#"{"baseBlock":1,"block":2}"#.to_owned()),
        ("list_four", "protx list", r#"["registered",true,1,1]"#.to_owned()),
        ("info_family", "protx", format!(r#"["info","{unknown_a}"]"#)),
        ("info_named", "protx info", format!(r#"{{"proTxHash":"{unknown_a}"}}"#)),
        ("family_named", "protx", r#"{"command":"list","type":"valid"}"#.to_owned()),
    ] {
        calls.push((format!("dispatch_{name}"), method, params));
    }
    diff_chunks(pair, &calls);
}

/// An invalidated tip is off the active chain: cases 15 and 16, and `info` at it.
fn invalidated(pair: &mut Pair, run: &RecordedRun, at: &BTreeMap<usize, BTreeSet<String>>) {
    let tip = run.tip();
    let hash = run.hash(tip);
    let unknown_b = "cd".repeat(32);
    pair.both("invalidate", "invalidateblock", &format!(r#"["{hash}"]"#));
    let mut calls = vec![
        ("combined_15".to_owned(), "protx listdiff", format!(r#"["{hash}","{unknown_b}"]"#)),
        ("combined_16".to_owned(), "protx listdiff", format!(r#"["{hash}",5]"#)),
        ("side_block".to_owned(), "protx listdiff", format!(r#"[1,"{hash}"]"#)),
        ("side_tip_height".to_owned(), "protx listdiff", format!("[1,{tip}]")),
        ("side_list_tip".to_owned(), "protx list", format!(r#"["registered",true,{tip}]"#)),
        ("side_list".to_owned(), "protx list", r#"["registered",true]"#.to_owned()),
    ];
    if let Some(mn) = at[&tip].iter().next() {
        calls.push(("side_info".to_owned(), "protx info", format!(r#"["{mn}","{hash}"]"#)));
        calls.push(("side_info_tip".to_owned(), "protx info", format!(r#"["{mn}"]"#)));
    }
    diff_chunks(pair, &calls);
    pair.both("reconsider", "reconsiderblock", &format!(r#"["{hash}"]"#));
    pair.both("reconsidered", "protx listdiff", &format!(r#"[1,"{hash}"]"#));
}

/// Replays `name` with `extra`, pins the mock time, and compares every P1 case.
fn run(name: &str, extra: &[&str]) -> Reached {
    let run = recorded_run(name);
    let mut args = run.args();
    args.push("-txindex=0");
    args.extend(extra);
    let mut pair = Pair::start(&args);
    replay(&mut pair, &run, 1..=run.tip());
    pair.mock_at(PINNED_MOCKTIME);
    let dip3 = dip3_height(&pair);
    let mut reached = Reached::default();
    let at = lists(&mut pair, &run, dip3, &mut reached);
    infos(&mut pair, &run, &at, &mut reached);
    list_diffs(&mut pair, &run, dip3, &mut reached);
    arguments(&mut pair, &run);
    invalidated(&mut pair, &run, &at);
    eprintln!("{name} {extra:?}: dip3 {dip3}; {reached:?}; {} comparisons", pair.count);
    pair.finish();
    reached
}

#[test]
pub fn protx_list_info_listdiff_dip3_match_dashd() {
    let _dashd = common::require_dashd();
    let reached = run("feature_dip3_deterministicmns", &[]);
    assert!(reached.historical_info > 0, "{reached:?}");
    assert!(reached.added > 0 && reached.removed > 0 && reached.updated > 0, "{reached:?}");
    assert!(reached.net_info_updates > 0, "{reached:?}");
    // Internal-id order of addedMNs and removedMNs is exercised: replies with two or more
    // entries in an order that is not displayed proTxHash order.
    assert!(reached.multi_added > 0 && reached.multi_removed > 0, "{reached:?}");
    assert!(reached.added_off_hash_order > 0 && reached.removed_off_hash_order > 0, "{reached:?}");
    run("feature_dip3_deterministicmns", &["-deprecatedrpc=permissive_bool"]);
}

#[test]
pub fn protx_list_info_listdiff_evo_match_dashd() {
    let _dashd = common::require_dashd();
    for extra in [&[][..], &["-deprecatedrpc=service"][..]] {
        let reached = run("feature_llmq_evo", extra);
        assert!(reached.evo_entries > 0, "{reached:?}");
        assert!(reached.port_only_updates > 0, "no port-only change: {reached:?}");
    }
}

#[test]
pub fn protx_list_info_listdiff_shares_match_dashd() {
    let _dashd = common::require_dashd();
    // Its masternodes' collaterals are all spent by the tip, and an Evo node's ProUpServTx
    // changes its legacy network info.
    let reached = run("feature_masternode_shares", &[]);
    assert!(reached.spent_info > 0, "no spent-collateral info: {reached:?}");
    assert!(reached.added > 0 && reached.removed > 0 && reached.updated > 0, "{reached:?}");
    assert!(reached.evo_net_info_updates > 0, "no Evo address replacement: {reached:?}");
}

#[test]
pub fn protx_list_info_listdiff_shares_activation_match_dashd() {
    let _dashd = common::require_dashd();
    let reached = run("feature_evo_shares_activation", &[]);
    assert!(reached.added > 0, "{reached:?}");
}
