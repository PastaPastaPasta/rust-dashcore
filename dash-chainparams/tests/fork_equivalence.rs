//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! The chain parameters this crate builds equal the ones the base-sdk fork's own `dash-params`
//! builds, case by case, while both exist (POL-3c §6).
//!
//! Every v24.0 case of the hand-kept manifest `corpus/chainparams_cases.json5` and every builder
//! recipe of `tests/builder.rs` is built through both builders. Both setups are rendered in the
//! corpus test's shape and the leaf maps compared; refusals are compared by their `Display`,
//! which is Core's text. The values a rendering does not reach (the `ChainParams` copy, the devnet
//! genesis block's fields, the pins, the static tables, the extension traits' values) are compared
//! directly. The fork's runtime-policy fields are compared with the setup's [`ChainPolicy`]
//! (BL-637), so a fork sync cannot move one side alone.
//!
//! The fork's builder exists only on the fork's `dash-params`, so this test runs behind the
//! `fork-equivalence` feature, and goes when the fork drops it (POL-3d).

#![expect(clippy::panic, clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;
mod dump;

use bsdk_params as fork;
use dash_chainparams::{
    self as facade, BuriedDeployment, BuriedDeploymentExt, ChainParams, ChainPolicy,
    ConsensusParams, LlmqParams, LlmqParamsExt, LlmqType, LlmqTypeExt,
};

use std::collections::BTreeMap;

type BlockHeight = bitcoin_units::BlockHeight;

/// One step of a builder recipe, applied to either builder.
#[derive(Clone, Debug)]
enum Op {
    /// An occurrence of the option named (with its dash) and its value.
    Arg(String, String),
    /// A negated occurrence of the option named.
    Negate(String),
    FastPrune(bool),
    DevnetName(&'static str),
    Activation(BuriedDeployment, u32),
    Dip3(u32, u32),
    Budget(u32, u32, u32),
    Spacing(i64),
}

use Op::{Activation, Budget, DevnetName, Dip3, FastPrune, Negate, Spacing};

fn arg(name: &str, value: impl Into<String>) -> Op {
    Op::Arg(name.into(), value.into())
}

fn negate(name: &str) -> Op {
    Negate(name.into())
}

/// A recipe: its name, the network (by `network_id`) and the steps.
type Case = (String, String, Vec<Op>);

/// Builds `ops` on `network` through `$krate`'s builder at v24.0.
macro_rules! build_with {
    ($krate:ident, $network:expr, $ops:expr) => {{
        let network = $krate::Network::ALL
            .into_iter()
            .find(|n| n.network_id() == $network)
            .expect("a network");
        let option = |name: &str| $krate::ChainOption::from_name(name).expect("a chain option");
        let mut b = $krate::ChainParamsBuilder::new(network, $krate::Release::V24_0);
        for op in $ops {
            b = match op.clone() {
                Op::Arg(name, value) => b.arg(option(&name), value),
                Op::Negate(name) => b.negate(option(&name)),
                Op::FastPrune(on) => b.fast_prune(on),
                Op::DevnetName(name) => b.devnet_name(name),
                Op::Activation(d, h) => b.activation_height(d, BlockHeight::from_u32(h)),
                Op::Dip3(a, e) => b.dip3(BlockHeight::from_u32(a), BlockHeight::from_u32(e)),
                Op::Budget(m, b2, s) => b.budget(
                    BlockHeight::from_u32(m),
                    BlockHeight::from_u32(b2),
                    BlockHeight::from_u32(s),
                ),
                Op::Spacing(s) => b.pow_target_spacing(s),
            };
        }
        b.build()
    }};
}

/// The fork's runtime-policy fields, as this crate's policy.
fn fork_policy(p: &fork::ChainSetup) -> ChainPolicy {
    ChainPolicy {
        prune_after_height: p.prune_after_height,
        min_blocks_to_keep: p.min_blocks_to_keep,
        assumed_blockchain_size_gb: p.assumed_blockchain_size_gb,
        assumed_chain_state_size_gb: p.assumed_chain_state_size_gb,
        default_consistency_checks: p.default_consistency_checks,
        is_mockable_chain: p.is_mockable_chain,
    }
}

/// `c` with the fork-only devnet values set to `from`'s. This crate's parameters hold the
/// table's values there (it reads its own fields, which upstream's type lacks), so those are
/// compared through the setups' consensus instead.
fn masked(mut c: ConsensusParams, from: &ConsensusParams) -> ConsensusParams {
    c.hash_devnet_genesis_block = from.hash_devnet_genesis_block;
    c.minimum_difficulty_blocks = from.minimum_difficulty_blocks;
    c.high_subsidy_blocks = from.high_subsidy_blocks;
    c.high_subsidy_factor = from.high_subsidy_factor;
    c
}

/// `chain` with its consensus parameters [`masked`] by `from`'s.
fn with_fork_devnet_values(chain: ChainParams, from: &ChainParams) -> ChainParams {
    ChainParams {
        consensus: masked(chain.consensus, &from.consensus),
        ..chain
    }
}

/// Asserts that the two setups of case `name` are the same, field by field.
fn assert_same(name: &str, ours: &facade::ChainSetup, theirs: &fork::ChainSetup) {
    // BL-637: the fork's flat policy fields equal the setup's policy.
    let policy = fork_policy(theirs);
    assert_eq!(ours.policy, policy, "{name}: policy");
    let (a, b) = (dump::dump_setup!(ours, &ours.policy), dump::dump_setup!(theirs, &policy));
    for (path, v) in &b.0 {
        assert_eq!(a.0.get(path), Some(v), "{name}: {path}");
    }
    assert_eq!(a.0.len(), b.0.len(), "{name}: keys");

    assert_eq!(ours.network as u8, theirs.network as u8, "{name}: network");
    assert_eq!(format!("{:?}", ours.release), format!("{:?}", theirs.release), "{name}");
    let copy: &ChainParams = ours;
    let fork_copy: &ChainParams = theirs;
    assert_eq!(*copy, with_fork_devnet_values(*fork_copy, copy), "{name}: ChainParams copy");
    let base = masked(*theirs.consensus, &ours.consensus);
    assert_eq!(*ours.consensus, base, "{name}: consensus parameters");
    let devnet = (
        ours.consensus.hash_devnet_genesis_block,
        ours.consensus.minimum_difficulty_blocks,
        ours.consensus.high_subsidy_blocks,
        ours.consensus.high_subsidy_factor,
    );
    let fork_devnet = (
        theirs.consensus.hash_devnet_genesis_block,
        theirs.consensus.minimum_difficulty_blocks,
        theirs.consensus.high_subsidy_blocks,
        theirs.consensus.high_subsidy_factor,
    );
    assert_eq!(devnet, fork_devnet, "{name}: devnet values");
    assert_eq!(
        ours.consensus.difficulty_adjustment_interval(),
        theirs.consensus.difficulty_adjustment_interval(),
        "{name}"
    );
    let llmqs: Vec<_> = ours.consensus.llmq_params().map(|(t, q)| (t, *q)).collect();
    let fork_llmqs: Vec<_> = theirs.consensus.llmq_params().map(|(t, q)| (t, *q)).collect();
    assert_eq!(llmqs, fork_llmqs, "{name}: LLMQs");
    let deployments: Vec<_> = ours.deployments().map(|(pos, d)| (pos.index(), *d)).collect();
    let fork_deployments: Vec<_> = theirs.deployments().map(|(pos, d)| (pos.index(), *d)).collect();
    assert_eq!(deployments, fork_deployments, "{name}: deployments");
    assert_eq!(ours.base_sdk_addr_params(), theirs.base_sdk_addr_params(), "{name}");
    assert_eq!(ours.checkpoints, theirs.checkpoints, "{name}: checkpoints");
    assert_eq!(ours.platform_hrp, theirs.platform_hrp, "{name}");
    assert_eq!(ours.devnet_name, theirs.devnet_name, "{name}");
    let genesis = ours
        .devnet_genesis
        .as_ref()
        .map(|g| (g.name.clone(), g.prev_hash, g.time, g.bits, g.nonce, g.hash, g.script_sig()));
    let fork_genesis = theirs
        .devnet_genesis
        .as_ref()
        .map(|g| (g.name.clone(), g.prev_hash, g.time, g.bits, g.nonce, g.hash, g.script_sig()));
    assert_eq!(genesis, fork_genesis, "{name}: devnet genesis");
    assert_eq!(ours.spork_signers.key_ids, theirs.spork_signers.key_ids, "{name}");
    assert_eq!(ours.spork_signers.min_spork_keys, theirs.spork_signers.min_spork_keys, "{name}");
    assert_eq!(ours.spork_signers.warning, theirs.spork_signers.warning, "{name}");
    assert_eq!(ours.require_standard, theirs.require_standard, "{name}");
    let assumeutxo = |a: &[facade::AssumeutxoData]| {
        a.iter().map(|u| (u.height, u.hash_serialized, u.evo_hash, u.chain_tx)).collect::<Vec<_>>()
    };
    let fork_assumeutxo = |a: &[fork::AssumeutxoData]| {
        a.iter().map(|u| (u.height, u.hash_serialized, u.evo_hash, u.chain_tx)).collect::<Vec<_>>()
    };
    assert_eq!(assumeutxo(ours.assumeutxo), fork_assumeutxo(theirs.assumeutxo), "{name}");
    assert_eq!(ours.fixed_seeds, theirs.fixed_seeds, "{name}: fixed seeds");
    let pins = (ours.pins.pre_dgw_tolerance_until, ours.pins.llmq_25_67_activation_height);
    let fork_pins = (theirs.pins.pre_dgw_tolerance_until, theirs.pins.llmq_25_67_activation_height);
    assert_eq!(pins, fork_pins, "{name}: pins");
    let extras = |r, m, l, f| (r, m, l, f);
    assert_eq!(
        extras(
            ours.require_routable_external_ip,
            ours.allow_multiple_addresses_from_group,
            ours.llmq_connection_retry_timeout,
            ours.fulfilled_request_expire_time,
        ),
        extras(
            theirs.require_routable_external_ip,
            theirs.allow_multiple_addresses_from_group,
            theirs.llmq_connection_retry_timeout,
            theirs.fulfilled_request_expire_time,
        ),
        "{name}: extras"
    );
}

/// Builds case `name` through both builders and compares the results. Returns whether both
/// refused it.
fn check(name: &str, network: &str, ops: &[Op]) -> bool {
    let ours = build_with!(facade, network, ops);
    let theirs = build_with!(fork, network, ops);
    match (ours, theirs) {
        (Ok(a), Ok(b)) => {
            assert_same(name, &a, &b);
            false
        }
        (Err(a), Err(b)) => {
            assert_eq!(a.to_string(), b.to_string(), "{name}: refusal");
            true
        }
        (a, b) => panic!("{name}: ours {:?}, the fork's {:?}", a.map(|_| ()), b.map(|_| ())),
    }
}

/// Core's `InterpretBool` (the corpus test's): empty is true, else the leading integer is
/// compared to 0.
fn interpret_bool(s: &str) -> bool {
    let s = s.trim_matches(|c| matches!(c, ' ' | '\x0c' | '\n' | '\r' | '\t' | '\x0b'));
    if s.is_empty() {
        return true;
    }
    if s.starts_with("+-") {
        return false;
    }
    let s = s.strip_prefix('+').unwrap_or(s);
    let digits = s.strip_prefix('-').unwrap_or(s);
    digits.chars().take_while(char::is_ascii_digit).any(|c| c != '0')
}

/// One command-line argument as Core's `InterpretValue` stores it (the corpus test's
/// `add_arg`): `-noname=v` with a false `v` is a double negative, stored as true, if Core allows
/// the negation.
fn core_arg(raw: &str) -> Op {
    let (key, value) = match raw.split_once('=') {
        Some((k, v)) => (k, Some(v)),
        None => (raw, None),
    };
    match key.strip_prefix("-no").map(|name| format!("-{name}")) {
        Some(name) => {
            let option = facade::ChainOption::from_name(&name).expect("chain option");
            if option.negatable() && value.is_some_and(|v| !interpret_bool(v)) {
                arg(&name, "1")
            } else {
                Negate(name)
            }
        }
        None => arg(key, value.unwrap_or("")),
    }
}

/// The v24.0 cases of the corpus manifest.
fn corpus_cases() -> Vec<Case> {
    let manifest = common::corpus("chainparams_cases");
    let entries = manifest["chainparams_v24_0"].as_array().expect("v24.0 cases");
    entries
        .iter()
        .map(|entry| {
            let name = entry[0].as_str().unwrap();
            let chain = entry[1].as_str().unwrap().to_owned();
            let ops = entry[2].as_array().unwrap().iter().map(|a| core_arg(a.as_str().unwrap()));
            (format!("corpus/{name}"), chain, ops.collect())
        })
        .collect()
}

/// How many recipes [`builder_cases`] holds. A recipe added to or dropped from
/// `tests/builder.rs` is carried here too, and this count moves with it.
const BUILDER_CASES: usize = 122;

/// The builder recipes of `tests/builder.rs`, test by test.
fn builder_cases() -> Vec<Case> {
    const NETWORKS: [&str; 4] = ["main", "test", "devnet", "regtest"];
    const REGTEST_SIGNER: &str = "yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW";
    const TESTNET_SIGNER: &str = "yjPtiKh2uwk3bDutTEA2q9mCtXyiZRWn55";
    let mut cases: Vec<Case> = Vec::new();
    let mut add = |name: &str, network: &str, ops: Vec<Op>| {
        cases.push((format!("builder/{name}"), network.to_owned(), ops));
    };
    for n in NETWORKS {
        add(&format!("plain_{n}"), n, vec![]);
        add(&format!("fast_prune_{n}"), n, vec![FastPrune(true)]);
        add(&format!("negated_spacing_{n}"), n, vec![negate("-powtargetspacing")]);
        add(
            &format!("negated_spacing_after_errors_{n}"),
            n,
            vec![
                arg("-dip3params", "x"),
                arg("-llmqdevnetparams", "x"),
                arg("-llmqtestparams", "-1:2"),
                negate("-powtargetspacing"),
                Spacing(60),
            ],
        );
    }
    add(
        "overrides_reach_the_static_copy_regtest",
        "regtest",
        vec![
            arg("-vbparams", "v24:5:9:10"),
            arg("-llmqtestparams", "5:4"),
            Dip3(105, 106),
            FastPrune(true),
        ],
    );
    add(
        "overrides_reach_the_static_copy_devnet",
        "devnet",
        vec![
            DevnetName("bs5"),
            arg("-llmqchainlocks", "llmq_50_60"),
            arg("-highsubsidyblocks", "7"),
        ],
    );
    add(
        "typed_setters",
        "regtest",
        vec![
            Activation(BuriedDeployment::V20, 1000),
            Activation(BuriedDeployment::MnRr, 1001),
            Dip3(105, 106),
            Budget(10, 20, 30),
            FastPrune(true),
        ],
    );
    add(
        "typed_setters_as_arguments",
        "regtest",
        vec![
            arg("-testactivationheight", "v20@1000"),
            arg("-testactivationheight", "mn_rr@1001"),
            arg("-dip3params", "105:106"),
            arg("-budgetparams", "10:20:30"),
            arg("-fastprune", ""),
        ],
    );
    add("typed_setters_devnet", "devnet", vec![DevnetName("bs5"), Spacing(60)]);
    add("named_devnet", "devnet", vec![DevnetName("bs5")]);
    add("empty_devnet_name", "devnet", vec![DevnetName("")]);
    add("activation_without_name", "regtest", vec![Activation(BuriedDeployment::Dip0003, 5)]);
    add("ipv4_optional", "regtest", vec![Activation(BuriedDeployment::Ipv4Optional, 15)]);
    for (option, value) in [
        ("-budgetparams", "10:-1:30"),
        ("-budgetparams", "-5:20:30"),
        ("-budgetparams", "10:20:-2147483648"),
        ("-dip3params", "-1:106"),
        ("-dip3params", "105:-106"),
        ("-vbparams", "v24:0:999:-350"),
    ] {
        add(&format!("negative_height {option}={value}"), "regtest", vec![arg(option, value)]);
    }
    for second in ["v24:0:9:0", "v24:0:9"] {
        add(
            &format!("overwritten_negative {second}"),
            "regtest",
            vec![arg("-vbparams", "v24:0:9:-1"), arg("-vbparams", second)],
        );
    }
    add(
        "overwritten_negative_still_negative",
        "regtest",
        vec![
            arg("-vbparams", "testdummy:0:9:-5"),
            arg("-vbparams", "v24:0:9:-1"),
            arg("-vbparams", "v24:0:9:0"),
        ],
    );
    add("overwritten_negative_dip3", "regtest", vec![arg("-dip3params", "-1:5"), Dip3(4, 5)]);
    let cores_first: [&[(&str, &str)]; 5] = [
        &[("-dip3params", "-1:x")],
        &[("-vbparams", "nope:0:9:-1")],
        &[("-dip3params", "-1:600"), ("-budgetparams", "x")],
        &[("-vbparams", "v24:0:9:-1"), ("-vbparams", "nope:1:2")],
        &[("-budgetparams", "-1:2:3"), ("-llmqtestinstantsenddip0024", "x")],
    ];
    for (i, args) in cores_first.into_iter().enumerate() {
        let ops = args.iter().map(|(o, v)| arg(o, *v)).collect();
        add(&format!("cores_errors_come_first_{i}"), "regtest", ops);
    }
    add(
        "negation_repeatable",
        "regtest",
        vec![
            arg("-testactivationheight", "x"),
            negate("-testactivationheight"),
            arg("-testactivationheight", "v19@5"),
        ],
    );
    add("negation_vbparams", "regtest", vec![arg("-vbparams", "x"), negate("-vbparams")]);
    add("negation_dip3", "regtest", vec![Dip3(1, 2), negate("-dip3params")]);
    add("negation_fast_prune", "regtest", vec![FastPrune(true), negate("-fastprune")]);
    add("negation_first", "regtest", vec![negate("-powtargetspacing"), arg("-minsporkkeys", "1")]);
    add("min_keys_first", "regtest", vec![arg("-minsporkkeys", "1"), negate("-powtargetspacing")]);
    add("spacing_150", "devnet", vec![Spacing(150)]);
    add("spacing_86401", "devnet", vec![Spacing(86_401)]);
    add("dip3_600", "regtest", vec![Dip3(600, 600)]);
    add("mn_rr_5", "regtest", vec![Activation(BuriedDeployment::MnRr, 5)]);
    add(
        "height_order_ok",
        "regtest",
        vec![
            Dip3(600, 600),
            Activation(BuriedDeployment::V20, 600),
            Activation(BuriedDeployment::MnRr, 600),
        ],
    );
    add("budget", "regtest", vec![Budget(10, 20, 30)]);
    for n in ["main", "test"] {
        let ops = facade::ChainOption::ALL
            .into_iter()
            .filter(|o| {
                !matches!(o, facade::ChainOption::SporkAddr | facade::ChainOption::MinSporkKeys)
            })
            .map(|o| arg(o.name(), "x"))
            .collect();
        add(&format!("options_of_other_chains_{n}"), n, ops);
    }
    for (network, option, value) in [
        ("devnet", "-llmqchainlocks", "llmq_none"),
        ("devnet", "-llmqplatform", "llmq_test_platform"),
        ("devnet", "-llmqmnhf", "llmq_x"),
        ("devnet", "-llmqmnhf", "llmq_400_85"),
        ("regtest", "-llmqtestinstantsenddip0024", "llmq_none"),
        ("regtest", "-llmqtestinstantsenddip0024", "llmq_devnet"),
        ("regtest", "-llmqtestinstantsenddip0024", "llmq_test"),
        ("devnet", "-llmqchainlocks", "llmq_devnet_dip0024"),
        ("devnet", "-llmqinstantsenddip0024", "llmq_devnet"),
    ] {
        add(&format!("llmq_role {network} {option}={value}"), network, vec![arg(option, value)]);
    }
    for (option, network) in [("-llmqtestparams", "regtest"), ("-llmqdevnetparams", "devnet")] {
        for value in [
            "3:65535",
            "0:0",
            "3:65536",
            "-1:2",
            "3:-1",
            "-2147483648:2147483647",
            "400:2",
            "401:2",
            "65535:2",
            "65536:2",
            "1000000:-1",
            "-1:2b",
            "x:70000",
            "-1",
            "-1:2:3",
        ] {
            add(&format!("quirk_46 {option}={value}"), network, vec![arg(option, value)]);
        }
        add(
            &format!("quirk_46 {option} replaced"),
            network,
            vec![arg(option, "-1:2"), arg(option, "4:3")],
        );
        for other in NETWORKS.into_iter().filter(|n| *n != network) {
            add(&format!("ignored {option} on {other}"), other, vec![arg(option, "-1:2")]);
        }
    }
    let after_cores: [(&'static str, &[(&str, &str)]); 6] = [
        ("regtest", &[("-llmqtestparams", "-1:2"), ("-llmqtestinstantsenddip0024", "bogus")]),
        ("regtest", &[("-llmqtestparams", "-1:2"), ("-llmqtestinstantsendparams", "1:2b")]),
        ("regtest", &[("-llmqtestplatformparams", "3:70000"), ("-dip3params", "600:600")]),
        ("devnet", &[("-llmqdevnetparams", "-2:9"), ("-powtargetspacing", "abc")]),
        ("regtest", &[("-llmqtestparams", "-1:2"), ("-sporkaddr", "invalid")]),
        ("regtest", &[("-llmqtestplatformparams", "3:70000"), ("-llmqtestparams", "-1:2")]),
    ];
    for (i, (network, args)) in after_cores.into_iter().enumerate() {
        let ops = args.iter().map(|(o, v)| arg(o, *v)).collect();
        add(&format!("llmq_out_of_range_after_cores_errors_{i}"), network, ops);
    }
    for n in NETWORKS {
        let address = build_with!(facade, n, &[] as &[Op]).unwrap().spork_address;
        add(&format!("own_spork_address_{n}"), n, vec![arg("-sporkaddr", address)]);
    }
    let two = || vec![arg("-sporkaddr", REGTEST_SIGNER), arg("-sporkaddr", TESTNET_SIGNER)];
    add("two_spork_addresses", "regtest", two());
    add("two_spork_addresses_min_keys", "regtest", {
        let mut ops = two();
        ops.push(arg("-minsporkkeys", "2"));
        ops
    });
    add("negated_min_spork_keys", "regtest", vec![negate("-minsporkkeys")]);
    for invalid in [
        "invalid",
        "",
        "tdash1qqqqqqqq",
        "yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW\0",
        "Xgtyuk76vhuFW2iT7UAiHgNdWXCf3J34wh",
    ] {
        add(&format!("spork_address {invalid:?}"), "regtest", vec![arg("-sporkaddr", invalid)]);
    }
    add("spork_address_spaced", "regtest", vec![arg("-sporkaddr", format!(" {TESTNET_SIGNER}\t"))]);
    add("spork_address_plain", "regtest", vec![arg("-sporkaddr", TESTNET_SIGNER)]);
    add(
        "spork_after_chain_errors",
        "regtest",
        vec![arg("-sporkaddr", "invalid"), arg("-dip3params", "x")],
    );
    cases
}

#[test]
fn builds_equal_the_forks() {
    let corpus = corpus_cases();
    let builder = builder_cases();
    assert!(corpus.len() > 100, "the manifest's v24.0 cases: {}", corpus.len());
    assert_eq!(builder.len(), BUILDER_CASES, "the builder recipe inventory");
    let mut names = BTreeMap::new();
    let mut refused = [0, 0];
    for (part, cases) in [&corpus, &builder].into_iter().enumerate() {
        for (name, network, ops) in cases {
            assert!(names.insert(name.clone(), ()).is_none(), "duplicate case {name}");
            refused[part] += usize::from(check(name, network, ops));
        }
    }
    // Both builders refuse exactly the corpus cases Core refuses (`error`) or the node does
    // (`refused`), so every other corpus case compared two setups.
    let dumps = common::corpus("chainparams_v24_0");
    let refusals = dumps["cases"].as_array().expect("v24.0 dumps").iter();
    let expected = refusals.filter(|c| c.get("error").is_some() || c.get("refused").is_some());
    assert_eq!(refused[0], expected.count(), "corpus refusals");
    assert!(refused[1] < builder.len(), "every builder case was refused");
    println!(
        "{} cases ({} corpus, {} refused; {} builder, {} refused)",
        corpus.len() + builder.len(),
        corpus.len(),
        refused[0],
        builder.len(),
        refused[1]
    );
}

#[test]
fn networks_equal_the_forks() {
    assert_eq!(facade::Network::ALL.len(), fork::Network::ALL.len());
    for (ours, theirs) in facade::Network::ALL.into_iter().zip(fork::Network::ALL) {
        assert_eq!(ours as u8, theirs as u8);
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
        assert_eq!(ours.network_id(), theirs.network_id());
        assert_eq!(facade::Network::from_network_id(ours.network_id()), Some(ours));
        let fork_chain = *theirs.chain();
        assert_eq!(*ours.chain(), with_fork_devnet_values(fork_chain, ours.chain()), "{ours:?}");
        assert_eq!(ours.addr(), theirs.addr(), "{ours:?}");
        // The devnet's devnet-only values are the setup's, which `builds_equal_the_forks`
        // compares; base-sdk's three networks are the same table.
        if ours != facade::Network::Devnet {
            assert_eq!(ours.chain(), theirs.chain(), "{ours:?}");
            assert_eq!(fork::Network::try_from(ours), Ok(theirs));
        } else {
            assert_eq!(fork::Network::try_from(ours), Err(ours));
        }
    }
}

#[test]
fn options_and_releases_equal_the_forks() {
    let names = |o: [facade::ChainOption; 21]| o.map(|o| (o.name(), o.negatable()));
    let fork_names = |o: [fork::ChainOption; 21]| o.map(|o| (o.name(), o.negatable()));
    assert_eq!(names(facade::ChainOption::ALL), fork_names(fork::ChainOption::ALL));
    let ours = facade::Release::V24_0;
    let theirs = fork::Release::V24_0;
    let positions: Vec<_> =
        ours.deployments().iter().map(|d| (d.index(), d.name(), d.gbt_force())).collect();
    let fork_positions: Vec<_> =
        theirs.deployments().iter().map(|d| (d.index(), d.name(), d.gbt_force())).collect();
    assert_eq!(positions, fork_positions);
    assert_eq!(ours.buried_deployments(), theirs.buried_deployments());
    assert_eq!(<BuriedDeployment as BuriedDeploymentExt>::ALL, BuriedDeployment::ALL);
    for d in BuriedDeployment::ALL {
        assert_eq!(BuriedDeploymentExt::name(d), d.name());
        assert_eq!(BuriedDeploymentExt::test_activation_name(d), d.test_activation_name());
    }
}

#[test]
fn llmq_values_equal_the_forks() {
    assert_eq!(facade::MAX_LLMQ_SIZE, fork::MAX_LLMQ_SIZE);
    assert_eq!(<LlmqType as LlmqTypeExt>::AVAILABLE, LlmqType::AVAILABLE);
    let mut types = LlmqType::AVAILABLE.to_vec();
    types.extend([LlmqType::LlmqNone, LlmqType::Unknown(0xfe)]);
    for t in types {
        assert_eq!(facade::available_params(t), fork::available_params(t), "{t:?}");
        let Some(q) = fork::available_params(t) else {
            continue;
        };
        let variants = [
            q,
            LlmqParams {
                signing_active_quorum_count: 0,
                ..q
            },
        ];
        for q in variants {
            for count in [i32::MIN, -7, -1, 0, 1, 2, 7, 24, 64, 1440, 4032, i32::MAX] {
                assert_eq!(LlmqParamsExt::max_cycles(&q, count), q.max_cycles(count), "{t:?}");
            }
            assert_eq!(LlmqParamsExt::max_store_depth(&q), q.max_store_depth(), "{t:?}");
            for size in [0, 1, 2, 400] {
                let q = LlmqParams {
                    size,
                    ..q
                };
                assert_eq!(LlmqParamsExt::is_single_member(&q), q.is_single_member(), "{t:?}");
            }
        }
    }
}

#[test]
fn devnet_genesis_equals_the_forks() {
    use facade::DevnetGenesisParams as Ours;
    use fork::DevnetGenesisParams as Theirs;
    assert_eq!(Ours::VERSION, Theirs::VERSION);
    assert_eq!(Ours::REWARD, Theirs::REWARD);
    assert_eq!(Ours::SCRIPT_PUBKEY, Theirs::SCRIPT_PUBKEY);
    let genesis = facade::Network::Devnet.chain().genesis;
    let hash = facade::Network::Devnet.chain().consensus.hash_genesis_block;
    for name in ["devnet", "devnet-bs5", "devnet-a", &format!("devnet-{}", "x".repeat(300))] {
        let ours = Ours::find(name, &genesis, hash).unwrap();
        let theirs = Theirs::find(name, &genesis, hash).unwrap();
        let fields =
            (ours.name.clone(), ours.prev_hash, ours.time, ours.bits, ours.nonce, ours.hash);
        let fork_fields = (
            theirs.name.clone(),
            theirs.prev_hash,
            theirs.time,
            theirs.bits,
            theirs.nonce,
            theirs.hash,
        );
        assert_eq!(fields, fork_fields, "{name}");
        assert_eq!(ours.script_sig(), theirs.script_sig(), "{name}");
        assert!(!ours.hash.is_null());
    }
}
