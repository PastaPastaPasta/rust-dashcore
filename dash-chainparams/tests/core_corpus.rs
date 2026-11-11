//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Built chain parameters equal Dash Core's, case by case, at v24.0.
//!
//! The corpus comes from Core commit 586c0ebb45df (v24.0.0, 938fba5028,
//! plus the oracle and state-digest test hooks; the chain params, their
//! options and the spork signer code differ from the tag only in hook
//! functions).
//!
//! From rc.5 to v24.0.0 only the new buried deployment `ipv4_optional`
//! moved, checked here against v24.0.0's `chainparams.cpp`: not scheduled on
//! mainnet and regtest (:197, :839) and 0 on testnet and devnets (:409,
//! :594). Its regtest `-testactivationheight` name (:1089-1090) is checked
//! here by `extra/tah_ipv4_optional`.
//!
//! From rc.4 to rc.5 these values moved, each checked here against rc.5's
//! `chainparams.cpp`: mainnet's `v24` and `evo_shares` schedule (:213-214,
//! :222-223); the minimum chain work and assumed-valid block (:230-231,
//! :441-442), the newest checkpoint (:348, :533) and the transaction
//! statistics (:357-362, :542-547) of mainnet and testnet; the fixed seeds
//! (:279, :462, `chainparamsseeds.h`); the assumed mainnet size (:247); the
//! testnet and devnet prune height (:451, :635); and the blocks pruning keeps
//! (:246, :452, :636, :881).
//!
//! The corpus must hold exactly the cases of the hand-kept manifest
//! `corpus/chainparams_cases.json5`, so a regeneration cannot drop one.
//!
//! The genesis and devnet genesis block bytes are checked in dash-consensus
//! (`tests/chainparams_blocks.rs`), which builds blocks; this crate does not.

#![expect(clippy::panic, clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;
mod dump;

use dash_chainparams::{
    ChainOption, ChainParams, ChainParamsBuilder, ChainSetup, LlmqType, LlmqTypeExt, MAX_LLMQ_SIZE,
    Network, ParamsError, Release, available_params,
};
use serde_json::Value;
use test_case::test_case;

use std::collections::BTreeMap;

/// `p` in the shape of the corpus, flattened: the oracle's `ChainParamsJson()` under `params` and
/// the other `CChainParams` members under `ext`, without the [`dump::BLOCK_KEYS`].
fn dump(p: &ChainSetup) -> BTreeMap<String, Value> {
    let mut out = dump::dump_setup(p);
    out.list("ext.availableLlmqs", &<LlmqType as LlmqTypeExt>::AVAILABLE, |out, path, _, t| {
        dump::llmq(out, path, *t, &available_params(*t).unwrap(), true)
    });
    out.put("ext.MAX_LLMQ_SIZE", MAX_LLMQ_SIZE);
    out.0
}

/// Leaf values of a corpus value keyed by dotted path, as [`Flat`] keys them.
fn flatten(value: &Value, prefix: &str, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                flatten(v, &format!("{prefix}{k}."), out);
            }
        }
        Value::Array(items) => {
            out.insert(format!("{prefix}#"), items.len().into());
            for (i, v) in items.iter().enumerate() {
                flatten(v, &format!("{prefix}{i}."), out);
            }
        }
        leaf => {
            out.insert(prefix.trim_end_matches('.').into(), leaf.clone());
        }
    }
}

fn flat(value: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    flatten(value, "", &mut out);
    out
}

fn network(chain: &str) -> Network {
    Network::from_network_id(chain).expect("one of Core's chains")
}

/// Core's `LocaleIndependentAtoi<int>`: trimmed, one `+` (`+-` is 0),
/// leading digits only, saturating.
fn atoi_int(s: &str) -> i64 {
    let s = s.trim_matches(|c| matches!(c, ' ' | '\x0c' | '\n' | '\r' | '\t' | '\x0b'));
    if s.starts_with("+-") {
        return 0;
    }
    let s = s.strip_prefix('+').unwrap_or(s);
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return 0;
    }
    let limit = if negative {
        i64::from(i32::MIN)
    } else {
        i64::from(i32::MAX)
    };
    match digits.parse::<i64>() {
        Ok(v) if negative => (-v).max(limit),
        Ok(v) => v.min(limit),
        Err(_) => limit,
    }
}

/// Core's `InterpretBool`: empty is true, else `atoi_int` is compared to 0.
fn interpret_bool(s: &str) -> bool {
    s.is_empty() || atoi_int(s) != 0
}

/// Adds one command-line argument `-name[=value]` or `-noname[=value]` to
/// `builder`, as Core's `InterpretValue` stores it: `-noname=v` with a false
/// `v` is a double negative, stored as true, if Core allows the negation.
fn add_arg(builder: ChainParamsBuilder, arg: &str) -> ChainParamsBuilder {
    let (key, value) = match arg.split_once('=') {
        Some((k, v)) => (k, Some(v)),
        None => (arg, None),
    };
    let option = |name: &str| ChainOption::from_name(name).expect("chain option");
    match key.strip_prefix("-no").map(|name| option(&format!("-{name}"))) {
        Some(o) if o.negatable() && value.is_some_and(|v| !interpret_bool(v)) => {
            builder.arg(o, "1")
        }
        Some(o) => builder.negate(o),
        None => builder.arg(option(key), value.unwrap_or("")),
    }
}

#[test_case("garbage", false)]
#[test_case("0x1", false)]
#[test_case("0abc", false)]
#[test_case("1abc", true)]
#[test_case("", true)]
#[test_case(" -3", true)]
#[test_case("+-1", false)]
#[test_case("99999999999", true)]
fn interpret_bool_as_core(s: &str, want: bool) {
    assert_eq!(interpret_bool(s), want);
}

/// The cases `corpus` must hold, from the hand-kept manifest
/// `corpus/chainparams_cases.json5`: chain and arguments by name.
fn required_cases(corpus: &str) -> BTreeMap<String, Value> {
    let manifest = common::corpus("chainparams_cases");
    let mut out = BTreeMap::new();
    for entry in manifest[corpus].as_array().unwrap() {
        let name = entry[0].as_str().unwrap();
        let case = Value::Array(vec![entry[1].clone(), entry[2].clone()]);
        assert!(out.insert(name.to_owned(), case).is_none(), "manifest {corpus}: duplicate {name}");
    }
    out
}

/// Asserts that `cases` are exactly the required cases of `corpus`, each
/// once, with its chain and arguments.
fn assert_required_cases(corpus: &str, cases: &[Value]) {
    let mut got = BTreeMap::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let chain_args = Value::Array(vec![case["chain"].clone(), case["args"].clone()]);
        assert!(got.insert(name, chain_args).is_none(), "{corpus}: duplicate case {name}");
    }
    let want = required_cases(corpus);
    assert!(!want.is_empty(), "{corpus}: the case manifest lists no cases");
    let missing: Vec<&String> = want.keys().filter(|n| !got.contains_key(n.as_str())).collect();
    let unlisted: Vec<&&str> = got.keys().filter(|n| !want.contains_key(**n)).collect();
    assert!(
        missing.is_empty() && unlisted.is_empty(),
        "{corpus}: missing {missing:?}, not in the manifest {unlisted:?}"
    );
    for (name, chain_args) in &want {
        assert_eq!(&got[name.as_str()], chain_args, "{corpus} {name}: chain and arguments");
    }
}

#[test]
fn chain_params_equal_core() {
    let (release, corpus) = (Release::V24_0, "chainparams_v24_0");
    let root = common::corpus(corpus);
    let bases: BTreeMap<String, BTreeMap<String, Value>> = root["bases"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(chain, v)| (chain.clone(), flat(v)))
        .collect();
    let cases = root["cases"].as_array().unwrap();
    assert_required_cases(corpus, cases);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let chain = case["chain"].as_str().unwrap();
        let mut builder = ChainParamsBuilder::new(network(chain), release);
        for arg in case["args"].as_array().unwrap() {
            builder = add_arg(builder, arg.as_str().unwrap());
        }
        let built = builder.build();
        if let Some(refused) = case.get("refused") {
            // Core accepts this input (its dump is in the corpus); dash-params
            // refuses it: a negative height (BL-31), or an LLMQ size outside u16,
            // the registered deviation quirk 46.
            assert!(case.get("delta").is_some(), "{corpus} {name}: Core refused too");
            let err = built.map(|_| ()).expect_err(name);
            assert_eq!(err.to_string(), refused.as_str().unwrap(), "{corpus} {name}");
            let deviation = case.get("deviation").map(|d| d.as_u64().unwrap());
            let quirk_46 = matches!(err, ParamsError::LlmqOutOfRange(..));
            assert_eq!(deviation, quirk_46.then_some(46), "{corpus} {name}");
            continue;
        }
        assert!(case.get("deviation").is_none(), "{corpus} {name}: deviation without refusal");
        if let Some(error) = case.get("error") {
            let got = built.map(|_| ()).expect_err(name).to_string();
            assert_eq!(got, error.as_str().unwrap(), "{corpus} {name}");
            continue;
        }
        let p = built.unwrap_or_else(|e| panic!("{corpus} {name}: unmarked refusal: {e}"));
        // The ChainParams copy holds every value it can (r2 m-6).
        let copy: &ChainParams = &p;
        assert_eq!(copy.consensus, *p.consensus, "{corpus} {name}");
        assert_eq!(copy.checkpoints, p.checkpoints, "{corpus} {name}");
        // The corpus has no key for a member the release lacks.
        assert!(p.platform_hrp.is_some(), "{corpus} {name}");
        assert!(p.require_standard.is_none(), "{corpus} {name}");
        assert!(p.spork_signers.min_spork_keys.is_none(), "{corpus} {name}");
        let mut want = bases[chain].clone();
        for (path, v) in case["delta"].as_object().unwrap() {
            assert!(
                want.insert(path.clone(), v.clone()).is_some(),
                "{corpus} {name}: {path} not in base"
            );
        }
        for (path, v) in case.get("extra_keys").and_then(Value::as_object).into_iter().flatten() {
            assert!(want.insert(path.clone(), v.clone()).is_none(), "{corpus} {name}: {path}");
        }
        for path in case.get("missing_keys").and_then(Value::as_array).into_iter().flatten() {
            assert!(want.remove(path.as_str().unwrap()).is_some(), "{corpus} {name}: {path}");
        }
        for key in dump::BLOCK_KEYS {
            want.remove(key);
        }
        let got = dump(&p);
        for (path, v) in &want {
            assert_eq!(got.get(path), Some(v), "{corpus} {name}: {path}");
        }
        assert_eq!(got.len(), want.len(), "{corpus} {name}: extra keys");
    }
}
