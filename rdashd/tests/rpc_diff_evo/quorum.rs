use super::Pair;
use super::common;
use common::pair::{assert_tips, diff_chunks, field, replay};
use common::recorded::recorded_run;
use dash_args::univalue::SettingsValue as UniValue;
use dash_params::{ChainOption, ChainParamsBuilder, Network, Release};
use dash_primitives::SpecialPayload;
use dash_types::Numeric;
use std::collections::BTreeSet;

#[derive(Default, Debug)]
struct Reached {
    commitments: usize,
    partial: usize,
    invalid: usize,
    failures: usize,
    single: usize,
}

fn run(name: &str, partial: bool, invalid: bool, failures: bool, single: bool) {
    if common::available_dashd().is_none() {
        eprintln!("Skipping quorum RPC DIFF: dashd v24.0.0 is absent");
        return;
    }
    let run = recorded_run(name);
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in &run.args {
        let (option, value) = arg.split_once('=').unwrap();
        builder = builder.arg(ChainOption::from_name(option).unwrap(), value);
    }
    let setup = builder.build().unwrap();
    let mut args = run.args();
    args.push("-txindex=0");
    let mut pair = Pair::start(&args);
    let mut reached = Reached::default();
    let mut seen = BTreeSet::new();
    let mut last = 0;
    for (i, bytes) in run.blocks.iter().enumerate() {
        let bytes = common::mine::parse_hex(bytes);
        let block =
            dash_consensus::primitives::decode_block(&mut bytes.as_slice()).unwrap().into_value();
        let mut quorums = Vec::new();
        for tx in block.txs() {
            let Some(Ok(payload)) = tx.payload() else {
                continue;
            };
            let SpecialPayload::QuorumCommitment(qc) = payload.payload() else {
                continue;
            };
            let qc = qc.commitment();
            if qc.valid_members().count_ones() == 0 {
                continue;
            }
            let key = (qc.llmq_type().to_base(), qc.quorum_hash().to_string());
            if seen.insert(key.clone()) {
                quorums.push(key);
            }
        }
        if quorums.is_empty() {
            continue;
        }
        let height = i + 1;
        replay(&mut pair, &run, last + 1..=height);
        last = height;
        let calls: Vec<_> = ["[]", "[null]", "[-1]", "[0]", "[1]", "[25]"]
            .into_iter()
            .map(|args| (format!("list_{height}_{args}"), "quorum list", args.to_owned()))
            .chain(std::iter::once((
                format!("extended_{height}"),
                "quorum listextended",
                "[]".into(),
            )))
            .collect();
        diff_chunks(&mut pair, &calls);
        let calls: Vec<_> = quorums
            .iter()
            .flat_map(|(t, hash)| {
                ["", ",null", ",false", ",true", ",\"1\""].into_iter().map(move |tail| {
                    (
                        format!("info_{height}_{t}_{hash}_{tail}"),
                        "quorum info",
                        format!(r#"[{t},"{hash}"{tail}]"#),
                    )
                })
            })
            .collect();
        let results = diff_chunks(&mut pair, &calls);
        for ((_, _, params), result) in calls.iter().zip(&results) {
            if !params.ends_with(",true]") {
                continue;
            }
            reached.commitments += 1;
            let UniValue::Arr(members) = field(result, "members") else {
                panic!("{}", result.write())
            };
            let type_name = field(result, "type").get_str().unwrap().to_owned();
            let configured_size = setup
                .consensus
                .llmq_params()
                .find_map(|(t, p)| (t.to_string() == type_name).then_some(usize::from(p.size)))
                .unwrap();
            reached.partial += usize::from(members.len() < configured_size);
            reached.invalid +=
                usize::from(members.iter().any(|m| field(m, "valid") == false.into()));
            if let UniValue::Obj(fields) = result {
                if let Some((_, value)) =
                    fields.iter().find(|(k, _)| k == "previousConsecutiveDKGFailures")
                {
                    reached.failures += usize::from(value.get_int64().unwrap() > 0);
                }
                assert!(!fields.iter().any(|(k, _)| k == "secretKeyShare"));
            }
            if configured_size == 1 {
                assert_eq!(members.len(), 1);
                assert_eq!(field(&members[0], "pubKeyShare"), field(&members[0], "pubKeyOperator"));
                reached.single += 1;
            } else {
                for member in members {
                    let UniValue::Obj(fields) = member else {
                        panic!()
                    };
                    assert!(!fields.iter().any(|(k, _)| k == "pubKeyShare"));
                }
            }
        }
    }
    if last < run.tip() {
        replay(&mut pair, &run, last + 1..=run.tip());
    }
    assert_tips(&pair, &run, run.tip());
    let calls: Vec<_> = (0..=run.tip())
        .map(|height| {
            (format!("extended_history_{height}"), "quorum listextended", format!("[{height}]"))
        })
        .collect();
    let historical = diff_chunks(&mut pair, &calls);
    if name == "feature_new_quorum_type_activation" {
        let enabled = |value: &UniValue| {
            let UniValue::Obj(fields) = value else {
                panic!("{}", value.write())
            };
            fields.iter().any(|(name, _)| name == "llmq_test_v17")
        };
        assert!(!enabled(historical.first().unwrap()), "type already enabled at genesis");
        assert!(enabled(historical.last().unwrap()), "type activation was not reached");
        eprintln!("{name}: llmq_test_v17 activation reached");
    } else {
        assert!(reached.commitments > 0, "no mined quorum info: {name}");
    }
    let mut calls: Vec<_> = ["[]", "[null]", "[-1]", "[0]", "[1]", "[25]"]
        .into_iter()
        .map(|args| (format!("list_tip_{args}"), "quorum list", args.to_owned()))
        .chain(std::iter::once(("extended_tip".into(), "quorum listextended", "[]".into())))
        .collect();
    for (method, args) in [
        ("quorum list", "[-2]".to_owned()),
        ("quorum list", r#"["x"]"#.to_owned()),
        ("quorum list", "[1.5]".to_owned()),
        ("quorum listextended", "[-1]".to_owned()),
        ("quorum listextended", format!("[{}]", run.tip() + 1)),
        ("quorum info", format!(r#"[100,"{}"]"#, "0".repeat(64))),
        ("quorum info", format!(r#"[100,"{}"]"#, run.hash(1))),
        ("quorum info", r#"[100,"x"]"#.to_owned()),
    ] {
        calls.push((format!("params_{method}_{args}"), method, args));
    }
    for t in [255, -1] {
        calls.push((
            format!("unknown_type_{t}"),
            "quorum info",
            format!(r#"[{t},"{}"]"#, "0".repeat(64)),
        ));
    }
    for (t, hash) in &seen {
        if *t == 100 {
            calls.push((format!("alias_{hash}"), "quorum info", format!(r#"[356,"{hash}"]"#)));
        }
    }
    diff_chunks(&mut pair, &calls);
    assert!(!partial || reached.partial > 0, "no partial quorum: {name} {reached:?}");
    assert!(!invalid || reached.invalid > 0, "no invalid member: {name} {reached:?}");
    assert!(!failures || reached.failures > 0, "no previous failures: {name} {reached:?}");
    assert!(!single || reached.single > 0, "no single-member share: {name} {reached:?}");
    eprintln!("{name}: {reached:?}; {} comparisons", pair.count);
    pair.finish();
    let mut args = run.args();
    args.extend(["-txindex=0", "-deprecatedrpc=permissive_bool", "-deprecatedrpc=service"]);
    let mut pair = Pair::start(&args);
    replay(&mut pair, &run, 1..=run.tip());
    let calls: Vec<_> = seen
        .iter()
        .flat_map(|(t, hash)| {
            [
                "",
                ",null",
                ",false",
                ",true",
                ",\"1\"",
                ",\"YES\"",
                ",1",
                ",1.0",
                ",1e0",
                ",2147483648",
                ",\"X\"",
            ]
            .into_iter()
            .map(move |tail| {
                (
                    format!("permissive_{t}_{hash}_{tail}"),
                    "quorum info",
                    format!(r#"[{t},"{hash}"{tail}]"#),
                )
            })
        })
        .collect();
    diff_chunks(&mut pair, &calls);
    eprintln!("{name}: permissive bool and deprecated service: {} comparisons", pair.count);
    pair.finish();
}

#[test]
fn quorum_rotation_matches_dashd() {
    run("feature_llmq_rotation", false, false, false, false);
}

#[test]
fn quorum_type_activation_matches_dashd() {
    run("feature_new_quorum_type_activation", false, false, false, false);
}

#[test]
fn quorum_simplepose_matches_dashd() {
    run("feature_llmq_simplepose", true, true, true, false);
}

#[test]
fn quorum_chainlocks_matches_dashd() {
    run("feature_llmq_chainlocks", false, false, true, false);
}

#[test]
fn quorum_single_member_matches_dashd() {
    run("feature_evo_shares_activation", false, false, false, true);
}

#[test]
fn quorum_parameter_forms_match_dashd() {
    if common::available_dashd().is_none() {
        eprintln!("Skipping quorum RPC DIFF: dashd v24.0.0 is absent");
        return;
    }
    for extra in [vec!["-txindex=0"], vec!["-txindex=0", "-deprecatedrpc=permissive_bool"]] {
        let mut pair = Pair::start(&extra);
        let mut calls = Vec::new();
        for method in ["quorum list", "quorum listextended"] {
            for args in [
                "[]",
                "[null]",
                "[0]",
                "[-1]",
                "[1]",
                "[25]",
                "[-2]",
                r#"["x"]"#,
                "[1.5]",
                "[1.0]",
                "[1e0]",
                "[2147483648]",
                "[true]",
                "[{}]",
                "[[]]",
                "[null,null]",
            ] {
                calls.push((format!("{method}_{args}"), method, args.into()));
            }
        }
        let hash = "0".repeat(64);
        for t in
            ["100", "356", "255", "-1", "null", r#""x""#, "true", "[]", "{}", "1.5", "2147483648"]
        {
            for tail in [
                "",
                ",null",
                ",false",
                ",true",
                ",\"1\"",
                ",1",
                ",1.0",
                ",1e0",
                ",2147483648",
                ",\"X\"",
                ",[]",
                ",{}",
            ] {
                calls.push((
                    format!("info_{t}_{tail}"),
                    "quorum info",
                    format!(r#"[{t},"{hash}"{tail}]"#),
                ));
            }
        }
        for t in [100, 356, 255, -1] {
            for hash in ["x".to_string(), "0".repeat(64)] {
                for tail in ["", r#", "x""#, ", {}"] {
                    calls.push((
                        format!("combined_{t}_{hash}_{tail}"),
                        "quorum info",
                        format!(r#"[{t},"{hash}"{tail}]"#),
                    ));
                }
            }
        }
        for value in ["null", "1", "true", "[]", "{}", r#""x""#] {
            calls.push((format!("info_hash_{value}"), "quorum info", format!("[100,{value}]")));
        }
        for (method, args) in [
            ("quorum", r#"["list",1]"#.to_string()),
            ("quorum list", r#"{"count":1}"#.to_string()),
            ("quorum", r#"{"command":"list","count":1}"#.to_string()),
            ("quorum listextended", r#"{"height":0}"#.to_string()),
            (
                "quorum info",
                format!(r#"{{"quorumHash":"{hash}","llmqType":356,"includeSkShare":true}}"#),
            ),
        ] {
            calls.push((format!("named_{method}_{args}"), method, args));
        }
        diff_chunks(&mut pair, &calls);
        pair.finish();
    }
}
