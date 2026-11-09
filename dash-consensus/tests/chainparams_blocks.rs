//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! The genesis and devnet genesis blocks built from the chain setup equal Dash Core's bytes, case
//! by case, in dash-chainparams' chain-params corpus. dash-chainparams checks every other value of
//! that corpus itself. The corpus must hold exactly the cases of dash-chainparams'
//! `corpus/chainparams_cases.json5`.
//!
//! The devnet genesis block built from its parameters, and the genesis blocks of each setup, hash
//! to the hashes the parameters hold.

#![expect(clippy::panic, clippy::unwrap_used, clippy::expect_used, reason = "test code")]
#![expect(
    clippy::disallowed_methods,
    reason = "checks base-sdk's own merkle root and the static network tables"
)]

use std::collections::BTreeMap;
use std::fs;

use dash_params::{
    ChainOption, ChainParamsBuilder, ChainSetup, DevnetGenesisParams, Network, Release,
};
use dash_primitives::{Block, BlockHash, BlockHeader, MerkleRoot};
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};
use hex_conservative::{DisplayHex, hex};
use serde_json::Value;
use test_case::test_case;

const GENESIS: &str = "params.genesisBlock";
const DEVNET_GENESIS: &str = "ext.devnetGenesisBlock";

/// Parses `corpus/<name>.json5` of dash-chainparams: JSON behind `//` header lines.
fn corpus(name: &str) -> Value {
    let path = format!("{}/../dash-chainparams/corpus/{name}.json5", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let body: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
    serde_json::from_str(&body.join("\n")).unwrap_or_else(|e| panic!("{name}: parse: {e}"))
}

fn serialize(block: &Block) -> String {
    let mut buf = Vec::new();
    block.encode(&mut buf);
    buf.to_lower_hex_string()
}

/// Core's `DevNetGenesisBlock()`: the devnet genesis block on a devnet, a null `CBlock` elsewhere.
fn devnet_genesis(setup: &ChainSetup) -> Block {
    setup
        .devnet_genesis
        .as_ref()
        .map_or_else(|| Block::new(BlockHeader::default(), Vec::new()), Block::from)
}

/// Core's `InterpretBool`: empty is true, else `LocaleIndependentAtoi<int>` (trimmed, one `+`,
/// leading digits only) is compared to 0.
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

/// Adds one command-line argument `-name[=value]` or `-noname[=value]` to `builder`, as Core's
/// `InterpretValue` stores it (a double negative is stored as true if Core allows the negation).
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

/// The cases `corpus` must hold, from the hand-kept manifest `corpus/chainparams_cases.json5`:
/// chain and arguments by name.
fn required_cases(corpus_name: &str) -> BTreeMap<String, Value> {
    let manifest = corpus("chainparams_cases");
    let mut out = BTreeMap::new();
    for entry in manifest[corpus_name].as_array().unwrap() {
        let name = entry[0].as_str().unwrap();
        let case = Value::Array(vec![entry[1].clone(), entry[2].clone()]);
        assert!(
            out.insert(name.to_owned(), case).is_none(),
            "manifest {corpus_name}: duplicate {name}"
        );
    }
    out
}

/// Asserts that `cases` are exactly the required cases of `corpus_name`, each once, with its chain
/// and arguments.
fn assert_required_cases(corpus_name: &str, cases: &[Value]) {
    let mut got = BTreeMap::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let chain_args = Value::Array(vec![case["chain"].clone(), case["args"].clone()]);
        assert!(got.insert(name, chain_args).is_none(), "{corpus_name}: duplicate case {name}");
    }
    let want = required_cases(corpus_name);
    assert!(!want.is_empty(), "{corpus_name}: the case manifest lists no cases");
    let missing: Vec<&String> = want.keys().filter(|n| !got.contains_key(n.as_str())).collect();
    let unlisted: Vec<&&str> = got.keys().filter(|n| !want.contains_key(**n)).collect();
    assert!(
        missing.is_empty() && unlisted.is_empty(),
        "{corpus_name}: missing {missing:?}, not in the manifest {unlisted:?}"
    );
    for (name, chain_args) in &want {
        assert_eq!(&got[name.as_str()], chain_args, "{corpus_name} {name}: chain and arguments");
    }
}

#[test]
fn chain_params_blocks_equal_core() {
    let (release, corpus_name) = (Release::V24_0, "chainparams_v24_0");
    let root = corpus(corpus_name);
    let bases = root["bases"].as_object().unwrap();
    let cases = root["cases"].as_array().unwrap();
    assert_required_cases(corpus_name, cases);
    for case in cases {
        if case.get("error").is_some() || case.get("refused").is_some() {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let chain = case["chain"].as_str().unwrap();
        let network = Network::from_network_id(chain).expect("one of Core's chains");
        let mut builder = ChainParamsBuilder::new(network, release);
        for arg in case["args"].as_array().unwrap() {
            builder = add_arg(builder, arg.as_str().unwrap());
        }
        let setup = builder.build().unwrap_or_else(|e| panic!("{corpus_name} {name}: {e}"));
        let delta = case["delta"].as_object().unwrap();
        let want = |key: &str| -> &Value {
            delta.get(key).unwrap_or_else(|| match key {
                GENESIS => &bases[chain]["params"]["genesisBlock"],
                _ => &bases[chain]["ext"]["devnetGenesisBlock"],
            })
        };
        let genesis = Block::from(&setup.genesis);
        assert_eq!(serialize(&genesis), want(GENESIS).as_str().unwrap(), "{corpus_name} {name}");
        let devnet = devnet_genesis(&setup);
        assert_eq!(
            serialize(&devnet),
            want(DEVNET_GENESIS).as_str().unwrap(),
            "{corpus_name} {name}"
        );
    }
}

/// The devnet table's genesis block has the merkle root of its coinbase transaction and hashes to
/// the genesis hash.
#[test]
fn devnet_genesis_block_hash_matches() {
    let genesis = Block::from(&Network::Devnet.chain().genesis);
    let params = Network::Devnet.chain();
    let expected_merkle_root = MerkleRoot::from_bendian(hex!(
        "e0028eb9648db56b1ac77cf090b99048a8007e2bb64b68f092c03c7f56a662c7"
    ));
    assert_eq!(genesis.header().merkle_root(), expected_merkle_root);
    let (root, mutated) = genesis.merkle();
    assert_eq!(root, expected_merkle_root);
    assert!(!mutated);

    let expected = BlockHash::from(params.consensus.hash_genesis_block);
    assert_eq!(genesis.header().hash(), expected);
}

/// The devnet genesis block built from the parameters hashes to the hash and nonce the
/// parameters' own search found. The names straddle each push size of the coinbase input script,
/// whose prefix is checked.
#[test_case(String::new(), &[0x51, 0x00]; "empty")]
#[test_case(String::from("x"), &[0x51, 0x01]; "one_byte")]
#[test_case("x".repeat(75), &[0x51, 0x4b]; "longest_direct_push")]
#[test_case("x".repeat(76), &[0x51, 0x4c, 0x4c]; "shortest_pushdata1")]
#[test_case("x".repeat(255), &[0x51, 0x4c, 0xff]; "longest_pushdata1")]
#[test_case("x".repeat(256), &[0x51, 0x4d, 0x00, 0x01]; "shortest_pushdata2")]
#[test_case(String::from("devnet-\u{fc}n\u{ef}c\u{f8}d\u{e9}"), &[0x51, 0x12]; "non_ascii")]
fn devnet_genesis_blocks_match_their_params(name: String, push: &[u8]) {
    let chain = Network::Regtest.chain();
    let params =
        DevnetGenesisParams::find(&name, &chain.genesis, chain.consensus.hash_genesis_block)
            .unwrap();
    let script_sig = params.script_sig();
    assert_eq!(script_sig[..push.len()], *push);
    assert_eq!(script_sig[push.len()..], *name.as_bytes());
    let block = Block::from(&params);
    assert_eq!(block.header().hash(), BlockHash::from(params.hash));
    assert_eq!(block.header().nonce(), params.nonce);
    assert_eq!(block.header().prev_hash(), BlockHash::from(chain.consensus.hash_genesis_block));
    assert_eq!(block.header().merkle_root(), block.merkle().0);
}

/// At v24.0, the setup's genesis block hashes to its genesis hash and, on a devnet, its devnet
/// genesis block to the devnet genesis hash, which the setup's height-1 checkpoint holds.
#[test_case(Network::Main, None; "mainnet")]
#[test_case(Network::Main, Some("bs5"); "mainnet_named")]
#[test_case(Network::Testnet3, None; "testnet")]
#[test_case(Network::Testnet3, Some("bs5"); "testnet_named")]
#[test_case(Network::Devnet, None; "devnet")]
#[test_case(Network::Devnet, Some("bs5"); "devnet_named")]
#[test_case(Network::Regtest, None; "regtest")]
#[test_case(Network::Regtest, Some("bs5"); "regtest_named")]
fn setup_genesis_blocks_match_their_hashes(network: Network, devnet_name: Option<&str>) {
    let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
    if let Some(name) = devnet_name {
        builder = builder.devnet_name(name);
    }
    let setup = builder.build().unwrap();
    let genesis = Block::from(&setup.genesis);
    assert_eq!(genesis.header().hash(), BlockHash::from(setup.consensus.hash_genesis_block));
    let Some(params) = &setup.devnet_genesis else {
        assert_ne!(network, Network::Devnet);
        return;
    };
    let block = Block::from(params);
    let hash = BlockHash::from(setup.consensus.hash_devnet_genesis_block);
    assert_eq!(block.header().hash(), hash);
    assert_eq!(block.header().prev_hash(), genesis.header().hash());
    let (height, checkpoint) = setup.checkpoints[1];
    assert_eq!((height.to_u32(), BlockHash::from(checkpoint)), (1, hash));
}
