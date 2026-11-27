use super::Pair;
use super::common;
use common::pair::{assert_tips, diff_chunks, field, replay};
use common::recorded::{RecordedRun, chain_txt_run, recorded_run};
use dash_args::univalue::SettingsValue as UniValue;
use dash_params::{ChainOption, ChainParamsBuilder, Network, Release};
use dash_primitives::SpecialPayload;
use dash_types::Numeric;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Debug)]
struct Reached {
    commitments: usize,
    partial: usize,
    invalid: usize,
    failures: usize,
    single: usize,
    member_of: usize,
}

/// The quorums with valid members whose commitments the block at `hex` mines: (type, hash).
fn mined_quorums(hex: &str) -> Vec<(String, String)> {
    let bytes = common::mine::parse_hex(hex);
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
        quorums.push((qc.llmq_type().to_base().to_string(), qc.quorum_hash().to_string()));
    }
    quorums
}

fn run(name: &str, partial: bool, invalid: bool, failures: bool, single: bool) {
    if common::available_dashd().is_none() {
        eprintln!("Skipping quorum RPC DIFF: dashd v24.0.3 is absent");
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
    for (i, block) in run.blocks.iter().enumerate() {
        let quorums: Vec<_> =
            mined_quorums(block).into_iter().filter(|key| seen.insert(key.clone())).collect();
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
        // BL-953: `quorum memberof` for every member of the quorums just mined, at the default
        // scan count and at 1 and 25.
        let members: BTreeSet<String> = calls
            .iter()
            .zip(&results)
            .filter(|((_, _, params), _)| params.ends_with(",true]"))
            .flat_map(|(_, result)| match field(result, "members") {
                UniValue::Arr(members) => members
                    .iter()
                    .map(|m| field(m, "proTxHash").get_str().unwrap().to_owned())
                    .collect::<Vec<_>>(),
                other => panic!("{}", other.write()),
            })
            .collect();
        let calls: Vec<_> = members
            .iter()
            .flat_map(|h| {
                ["", ",1", ",25"].into_iter().map(move |tail| {
                    (
                        format!("memberof_{height}_{h}{tail}"),
                        "quorum memberof",
                        format!(r#"["{h}"{tail}]"#),
                    )
                })
            })
            .collect();
        reached.member_of += diff_chunks(&mut pair, &calls)
            .iter()
            .filter(|r| matches!(r, UniValue::Arr(quorums) if !quorums.is_empty()))
            .count();
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
        assert!(reached.member_of > 0, "no quorum memberof: {name}");
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
        ("quorum memberof", format!(r#"["{}"]"#, "0".repeat(64))),
        ("quorum memberof", format!(r#"["{}",0]"#, "0".repeat(64))),
        ("quorum memberof", format!(r#"["{}",-1]"#, "0".repeat(64))),
        ("quorum memberof", r#"["x"]"#.to_owned()),
        ("quorum memberof", "[]".to_owned()),
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
        if t == "100" {
            calls.push((format!("alias_{hash}"), "quorum info", format!(r#"[356,"{hash}"]"#)));
        }
    }
    calls.extend(signing_calls(&seen));
    let chain_locks = setup.consensus.llmq_type_chain_locks.to_base();
    let verify = verify_calls(&run, chain_locks, &seen);
    calls.extend(verify.iter().cloned());
    let results = diff_chunks(&mut pair, &calls);
    let verified = results[calls.len() - verify.len()..].iter().filter(|r| **r == true.into());
    eprintln!("{name}: quorum verify true {} of {}", verified.count(), verify.len());
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

/// `quorum selectquorum` for each mined quorum's type, by request ids that include the quorum
/// hash, and every type's "no quorums active"; `quorum sign` and `platformsign` on a node that is
/// not a masternode, and the argument errors that come before that check.
fn signing_calls(seen: &BTreeSet<(String, String)>) -> Vec<(String, &'static str, String)> {
    let (zero, one) = ("0".repeat(64), format!("{}1", "0".repeat(63)));
    let mut calls = Vec::new();
    for (t, hash) in seen {
        for id in [hash, &zero, &one] {
            calls.push((
                format!("select_{t}_{id}"),
                "quorum selectquorum",
                format!(r#"[{t},"{id}"]"#),
            ));
        }
    }
    for t in [1, 2, 3, 4, 5, 6, 100, 101, 102, 103, 104, 105, 106, 107, 255, -1, 356] {
        calls.push((
            format!("select_type_{t}"),
            "quorum selectquorum",
            format!(r#"[{t},"{one}"]"#),
        ));
    }
    for (method, args) in [
        ("quorum selectquorum", r#"[100,"x"]"#.to_owned()),
        ("quorum selectquorum", r#"[1.5,"x"]"#.to_owned()),
        ("quorum selectquorum", "[100]".to_owned()),
        ("quorum sign", format!(r#"[100,"{one}","{one}"]"#)),
        ("quorum sign", format!(r#"[100,"{one}","{one}","",false]"#)),
        ("quorum sign", r#"[255,"x","y"]"#.to_owned()),
        ("quorum sign", r#"[1.5,"x","y"]"#.to_owned()),
        ("quorum sign", "[100]".to_owned()),
        ("quorum platformsign", format!(r#"["{one}","{one}"]"#)),
        ("quorum platformsign", r#"["x","y"]"#.to_owned()),
        ("quorum platformsign", "[]".to_owned()),
    ] {
        calls.push((format!("signing_{method}_{args}"), method, args));
    }
    calls
}

/// The newest ChainLock a coinbase carries: its height, block hash and signature.
fn coinbase_chainlock(run: &RecordedRun) -> Option<(usize, String, String)> {
    run.blocks.iter().enumerate().rev().find_map(|(i, hex)| {
        let bytes = common::mine::parse_hex(hex);
        let block =
            dash_consensus::primitives::decode_block(&mut bytes.as_slice()).unwrap().into_value();
        let Some(Ok(payload)) = block.txs().first()?.payload() else {
            return None;
        };
        let SpecialPayload::CoinbaseCommitment(cb) = payload.payload() else {
            return None;
        };
        let sig = cb.best_cl_signature().as_ref()?.to_bytes();
        let height = i.checked_sub(usize::try_from(cb.best_cl_height_diff()?).ok()?)?;
        let sig: String = sig.iter().map(|b| format!("{b:02x}")).collect();
        (height > 0 && sig.bytes().any(|c| c != b'0'))
            .then(|| (height, run.hash(height).to_owned(), sig))
    })
}

/// `quorum verify` over the newest coinbase ChainLock, which the ChainLock type's quorum selected
/// at its height less `SIGN_HEIGHT_OFFSET` signed for `("clsig", height)`: the latest quorums at
/// the tip and at heights around it, each quorum of the type by hash, another message hash, a
/// quorum not held, and the argument errors.
// Core: v24.0.0 src/rpc/quorums.cpp:664-724; src/chainlock/clsig.cpp:248-262
fn verify_calls(
    run: &RecordedRun,
    t: u8,
    seen: &BTreeSet<(String, String)>,
) -> Vec<(String, &'static str, String)> {
    let Some((height, msg, sig)) = coinbase_chainlock(run) else {
        return Vec::new();
    };
    let request = [&[5u8][..], b"clsig", &(height as i32).to_le_bytes()].concat();
    let id = bitcoin_hashes::sha256d::Hash::hash(&request).to_string();
    let other = run.hash(height - 1);
    let mut tails =
        vec![String::new(), r#","",-1"#.into(), format!(r#",null,{}"#, height.saturating_sub(8))];
    for h in [height.saturating_sub(8), height, height + 1, height + 16, height + 24, run.tip() + 1]
    {
        tails.push(format!(r#","",{h}"#));
    }
    let quorums = seen.iter().filter(|(q, _)| *q == t.to_string());
    tails.extend(quorums.map(|(_, hash)| format!(r#","{hash}""#)));
    tails.push(format!(r#","{}""#, "0".repeat(64)));
    let mut args: Vec<_> =
        tails.iter().map(|tail| format!(r#"[{t},"{id}","{msg}","{sig}"{tail}]"#)).collect();
    args.extend(tails.iter().map(|tail| format!(r#"[{t},"{id}","{other}","{sig}"{tail}]"#)));
    let zeros = "0".repeat(192);
    for bad in [
        format!(r#"[{t},"{id}","{msg}","{zeros}"]"#),
        format!(r#"[{t},"{id}","{msg}","{}"]"#, &sig[2..]),
        format!(r#"[{t},"{id}","{msg}","zz"]"#),
        format!(r#"[{t},"{id}","{msg}",7]"#),
        format!(r#"[{t},"{id}","{msg}","{sig}",""]"#),
        format!(r#"[{t},"{id}","{msg}","{sig}","",null]"#),
        format!(r#"[{t},"{id}","{msg}","{sig}","","x"]"#),
        format!(r#"[{t},"{id}","{msg}","{sig}","z"]"#),
        format!(r#"[255,"{id}","{msg}","{sig}"]"#),
        format!(r#"[{t},"x","{msg}","{sig}"]"#),
        format!(r#"[{t},"{id}","y","{sig}"]"#),
        format!("[{t}]"),
    ] {
        args.push(bad);
    }
    args.into_iter().map(|args| (format!("verify_{args}"), "quorum verify", args)).collect()
}

#[test]
pub fn quorum_rotation_matches_dashd() {
    run("feature_llmq_rotation", false, false, false, false);
}

#[test]
pub fn quorum_type_activation_matches_dashd() {
    run("feature_new_quorum_type_activation", false, false, false, false);
}

#[test]
pub fn quorum_simplepose_matches_dashd() {
    run("feature_llmq_simplepose", true, true, true, false);
}

#[test]
pub fn quorum_chainlocks_matches_dashd() {
    run("feature_llmq_chainlocks", false, false, true, false);
}

#[test]
pub fn quorum_single_member_matches_dashd() {
    run("feature_evo_shares_activation", false, false, false, true);
}

#[test]
pub fn quorum_parameter_forms_match_dashd() {
    if common::available_dashd().is_none() {
        eprintln!("Skipping quorum RPC DIFF: dashd v24.0.3 is absent");
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
        eprintln!("quorum parameter forms {extra:?}: {} comparisons", pair.count);
        pair.finish();
    }
}

/// `quorum info` of the quorums mined before V19, from two blocks before its activation to the
/// tip, on `v19-window/r2` (V19 at 236): Core v24.0.0 prints a quorum's public key under the
/// node-wide scheme, the tip's, not the scheme the quorum was mined under, so a pre-V19 quorum's
/// key prints in the basic encoding from tip V19 - 1 on, once the next block activates V19.
// Core: rpc/quorums.cpp BuildQuorumInfo (quorumPublicKey.ToString()), bls/bls.h
// bls_legacy_scheme (v24.0.0)
#[test]
pub fn quorum_pre_v19_key_after_v19_matches_dashd() {
    let _dashd = common::require_dashd();
    let run = chain_txt_run("v19-window-r2", "v19-window/r2/chain.txt");
    let v19: usize = run
        .args
        .iter()
        .find_map(|a| a.strip_prefix("-testactivationheight=v19@"))
        .expect("the chain's V19 height")
        .parse()
        .unwrap();
    let early = v19 - 2;
    let quorums: BTreeSet<_> = run.blocks[..early].iter().flat_map(|b| mined_quorums(b)).collect();
    assert!(!quorums.is_empty(), "no quorum mined before V19");
    let mut args = run.args();
    args.push("-txindex=0");
    let mut pair = Pair::start(&args);
    let mut keys: BTreeMap<&(String, String), BTreeMap<usize, BTreeSet<String>>> = BTreeMap::new();
    let mut last = 0;
    for height in [early, v19 - 1, v19, run.tip()] {
        replay(&mut pair, &run, last + 1..=height);
        last = height;
        let mut calls = vec![
            (format!("v19_list_{height}"), "quorum list", "[]".to_owned()),
            (format!("v19_extended_{height}"), "quorum listextended", "[]".to_owned()),
        ];
        for (t, hash) in &quorums {
            for tail in ["", ",true"] {
                calls.push((
                    format!("v19_info_{height}_{t}_{hash}{tail}"),
                    "quorum info",
                    format!(r#"[{t},"{hash}"{tail}]"#),
                ));
            }
        }
        let results = diff_chunks(&mut pair, &calls);
        for (quorum, result) in quorums.iter().flat_map(|q| [q, q]).zip(&results[2..]) {
            let key = field(result, "quorumPublicKey").get_str().unwrap().to_owned();
            keys.entry(quorum).or_default().entry(height).or_default().insert(key);
        }
    }
    assert_tips(&pair, &run, run.tip());
    assert_eq!(keys.len(), quorums.len(), "{keys:?}");
    for (quorum, printed) in &keys {
        let at = |height: usize| {
            let forms = &printed[&height];
            assert_eq!(forms.len(), 1, "{quorum:?} at {height}: {forms:?}");
            forms.first().unwrap()
        };
        assert_ne!(at(early), at(v19 - 1), "{quorum:?}: key not switched at tip V19 - 1");
        assert_eq!(at(v19 - 1), at(v19), "{quorum:?}");
        assert_eq!(at(v19), at(run.tip()), "{quorum:?}");
    }
    eprintln!(
        "v19-window-r2: {} pre-V19 quorums, every key switched at tip V19 - 1; {} comparisons",
        quorums.len(),
        pair.count
    );
    pair.finish();
}
