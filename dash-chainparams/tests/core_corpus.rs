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

use dash_chainparams::{
    ChainOption, ChainParams, ChainParamsBuilder, ChainSetup, LlmqParams, LlmqType, MAX_LLMQ_SIZE,
    Network, ParamsError, Release, available_params,
};
use dash_num::Hash256;
use dash_types::Numeric;
use hex_conservative::DisplayHex;
use serde_json::Value;
use test_case::test_case;

use std::collections::BTreeMap;

/// Corpus keys holding block bytes, which dash-consensus checks.
const BLOCK_KEYS: [&str; 2] = ["params.genesisBlock", "ext.devnetGenesisBlock"];

/// Core's `uint256::GetHex()`: the reversed internal bytes.
fn core_hex(hash: Hash256) -> String {
    hash.to_bendian().to_lower_hex_string()
}

/// The hash of a null `CBlock`, Core's `DevNetGenesisBlock()` off devnets.
fn null_block_hash() -> Hash256 {
    Hash256::from_lendian(dash_x11::x11(&[0; 80]))
}

/// Leaf values keyed by dotted path; `#` is an array's length.
#[derive(Default)]
struct Flat(BTreeMap<String, Value>);

impl Flat {
    fn put(&mut self, path: &str, value: impl Into<Value>) {
        self.0.insert(path.into(), value.into());
    }

    fn put_all<V: Into<Value>>(
        &mut self,
        prefix: &str,
        fields: impl IntoIterator<Item = (&'static str, V)>,
    ) {
        for (name, value) in fields {
            self.put(&format!("{prefix}.{name}"), value);
        }
    }

    fn list<T>(
        &mut self,
        path: &str,
        items: &[T],
        mut each: impl FnMut(&mut Self, &str, usize, &T),
    ) {
        self.put(&format!("{path}.#"), items.len());
        for (i, item) in items.iter().enumerate() {
            each(self, &format!("{path}.{i}"), i, item);
        }
    }
}

fn llmq(out: &mut Flat, path: &str, llmq_type: LlmqType, q: &LlmqParams, derived: bool) {
    out.put(&format!("{path}.type"), llmq_type.to_base());
    out.put(&format!("{path}.name"), llmq_type.to_string());
    out.put(&format!("{path}.useRotation"), q.use_rotation);
    let fields = [
        ("size", q.size),
        ("minSize", q.min_size),
        ("threshold", q.threshold),
        ("dkgInterval", q.dkg_interval),
        ("dkgPhaseBlocks", q.dkg_phase_blocks),
        ("dkgMiningWindowStart", q.dkg_mining_window_start),
        ("dkgMiningWindowEnd", q.dkg_mining_window_end),
        ("dkgBadVotesThreshold", q.dkg_bad_votes_threshold),
        ("signingActiveQuorumCount", q.signing_active_quorum_count),
        ("keepOldConnections", q.keep_old_connections),
        ("keepOldKeys", q.keep_old_keys),
        ("recoveryMembers", q.recovery_members),
    ];
    out.put_all(path, fields);
    if derived {
        out.put(&format!("{path}.max_store_depth"), q.max_store_depth());
        out.put(&format!("{path}.is_single_member"), q.is_single_member());
    }
}

/// `p` in the shape of the corpus, flattened: the oracle's
/// `ChainParamsJson()` under `params` and the other `CChainParams` members
/// under `ext`, without the [`BLOCK_KEYS`].
fn dump(p: &ChainSetup) -> BTreeMap<String, Value> {
    let mut out = Flat::default();
    let c = &p.consensus;
    let a = &p.addr_params;
    let devnet_genesis_hash = p.devnet_genesis.as_ref().map_or_else(null_block_hash, |g| g.hash);
    out.put("params.networkID", p.network_id);
    out.put("params.messageStart", p.message_start.to_lower_hex_string());
    out.put("params.defaultPort", p.default_port);
    out.put("params.pruneAfterHeight", p.prune_after_height);
    out.put("params.genesisHash", core_hex(c.hash_genesis_block));
    out.put("params.devnetGenesisHash", core_hex(devnet_genesis_hash));
    out.put("params.extCoinType", a.bip44_idx);
    out.put("params.base58Prefixes.PUBKEY_ADDRESS", format!("{:02x}", a.pubkey_addr));
    out.put("params.base58Prefixes.SCRIPT_ADDRESS", format!("{:02x}", a.script_addr));
    out.put("params.base58Prefixes.SECRET_KEY", format!("{:02x}", a.secret_key));
    out.put("params.base58Prefixes.EXT_PUBLIC_KEY", a.ext_pubkey.to_lower_hex_string());
    out.put("params.base58Prefixes.EXT_SECRET_KEY", a.ext_secret.to_lower_hex_string());
    for (h, hash) in &p.checkpoints {
        out.put(&format!("params.checkpoints.{}", h.to_u32()), core_hex(*hash));
    }

    let hashes = [
        ("hashGenesisBlock", c.hash_genesis_block),
        ("hashDevnetGenesisBlock", c.hash_devnet_genesis_block),
        ("nSuperblockStartHash", c.superblock_start.1),
        ("BIP34Hash", c.bip34.1),
        ("DIP0003EnforcementHash", c.dip0003_enforcement.1),
        ("defaultAssumeValid", c.default_assume_valid),
    ];
    out.put_all("params.consensus", hashes.map(|(n, h)| (n, core_hex(h))));
    let heights = [
        ("nMasternodePaymentsStartBlock", c.masternode_payments_start_block),
        ("nMasternodePaymentsIncreaseBlock", c.masternode_payments_increase_block),
        ("nBudgetPaymentsStartBlock", c.budget_payments_start_block),
        ("nSuperblockStartBlock", c.superblock_start.0),
        ("BIP34Height", c.bip34.0),
        ("BIP65Height", c.bip65_height),
        ("BIP66Height", c.bip66_height),
        ("BIP147Height", c.bip147_height),
        ("CSVHeight", c.csv_height),
        ("DIP0001Height", c.dip0001_height),
        ("DIP0003Height", c.dip0003_height),
        ("DIP0003EnforcementHeight", c.dip0003_enforcement.0),
        ("DIP0008Height", c.dip0008_height),
        ("BRRHeight", c.brr_height),
        ("DIP0020Height", c.dip0020_height),
        ("DIP0024Height", c.dip0024_height),
        ("DIP0024QuorumsHeight", c.dip0024_quorums_height),
        ("V19Height", c.v19_height),
        ("V20Height", c.v20_height),
        ("MN_RRHeight", c.mn_rr_height),
        ("WithdrawalsHeight", c.withdrawals_height),
        ("MinBIP9WarningHeight", c.min_bip9_warning_height),
        ("nPowKGWHeight", c.pow_kgw_height),
        ("nPowDGWHeight", c.pow_dgw_height),
    ];
    out.put_all("params.consensus", heights.map(|(n, h)| (n, h.to_u32())));
    let ints = [
        ("nSubsidyHalvingInterval", c.subsidy_halving_interval),
        ("nMasternodePaymentsIncreasePeriod", c.masternode_payments_increase_period),
        ("nInstantSendConfirmationsRequired", c.instant_send_confirmations_required),
        ("nInstantSendKeepLock", c.instant_send_keep_lock),
        ("nBudgetPaymentsCycleBlocks", c.budget_payments_cycle_blocks),
        ("nBudgetPaymentsWindowBlocks", c.budget_payments_window_blocks),
        ("nSuperblockCycle", c.superblock_cycle),
        ("nSuperblockMaturityWindow", c.superblock_maturity_window),
        ("nGovernanceMinQuorum", c.governance_min_quorum),
        ("nGovernanceFilterElements", c.governance_filter_elements),
        ("nMasternodeMinimumConfirmations", c.masternode_minimum_confirmations),
        ("nMinimumDifficultyBlocks", c.minimum_difficulty_blocks),
        ("nHighSubsidyBlocks", c.high_subsidy_blocks),
        ("nHighSubsidyFactor", c.high_subsidy_factor),
    ];
    out.put_all("params.consensus", ints);
    out.put("params.consensus.nRuleChangeActivationThreshold", c.rule_change_activation_threshold);
    out.put("params.consensus.nMinerConfirmationWindow", c.miner_confirmation_window);
    let deployments: Vec<_> = p.deployments().collect();
    out.list("params.consensus.vDeployments", &deployments, |out, path, _, (pos, d)| {
        out.put(&format!("{path}.name"), pos.name());
        out.put(&format!("{path}.bit"), d.bit);
        out.put(&format!("{path}.min_activation_height"), d.min_activation_height.to_u32());
        let fields = [
            ("nStartTime", d.start_time),
            ("nTimeout", d.timeout),
            ("nWindowSize", d.window_size),
            ("nThresholdStart", d.threshold_start),
            ("nThresholdMin", d.threshold_min),
            ("nFalloffCoeff", d.falloff_coeff),
        ];
        out.put_all(path, fields);
        out.put(&format!("{path}.useEHF"), d.use_ehf);
    });
    out.put("params.consensus.powLimit", c.pow_limit.to_bendian().to_lower_hex_string());
    out.put("params.consensus.fPowAllowMinDifficultyBlocks", c.pow_allow_min_difficulty_blocks);
    out.put("params.consensus.fPowNoRetargeting", c.pow_no_retargeting);
    out.put("params.consensus.nPowTargetSpacing", c.pow_target_spacing);
    out.put("params.consensus.nPowTargetTimespan", c.pow_target_timespan);
    out.put(
        "params.consensus.nMinimumChainWork",
        c.minimum_chain_work.to_bendian().to_lower_hex_string(),
    );
    let llmqs: Vec<_> = c.llmq_params().collect();
    out.list("params.consensus.llmqs", &llmqs, |out, path, _, (t, q)| {
        llmq(out, path, *t, q, false)
    });
    out.put("params.consensus.llmqTypeChainLocks", c.llmq_type_chain_locks.to_base());
    out.put(
        "params.consensus.llmqTypeDIP0024InstantSend",
        c.llmq_type_dip0024_instant_send.to_base(),
    );
    out.put("params.consensus.llmqTypePlatform", c.llmq_type_platform.to_base());
    out.put("params.consensus.llmqTypeMnhf", c.llmq_type_mnhf.to_base());

    out.put("ext.dataDir", p.data_dir());
    out.put("ext.rpcPort", p.rpc_port);
    out.put("ext.onionServiceTargetPort", p.onion_service_target_port);
    out.put("ext.defaultPlatformP2PPort", p.default_platform_p2p_port);
    out.put("ext.defaultPlatformHTTPPort", p.default_platform_http_port);
    out.put("ext.assumedBlockchainSize", p.assumed_blockchain_size_gb);
    out.put("ext.assumedChainStateSize", p.assumed_chain_state_size_gb);
    out.list("ext.dnsSeeds", p.dns_seeds, |out, path, _, s| out.put(path, *s));
    out.put("ext.fixedSeeds", p.fixed_seeds.to_lower_hex_string());
    out.list("ext.sporkAddresses", &[p.spork_address], |out, path, _, s| out.put(path, *s));
    let flags = [
        ("defaultConsistencyChecks", p.default_consistency_checks),
        ("requireRoutableExternalIP", p.require_routable_external_ip),
        ("isTestChain", p.is_test_chain),
        ("isMockableChain", p.is_mockable_chain),
        ("mineBlocksOnDemand", c.pow_no_retargeting),
        ("allowMultipleAddressesFromGroup", p.allow_multiple_addresses_from_group),
    ];
    out.put_all("ext", flags);
    let ints = [
        ("llmqConnectionRetryTimeout", p.llmq_connection_retry_timeout),
        ("poolMinParticipants", p.pool_min_participants),
        ("poolMaxParticipants", p.pool_max_participants),
        ("fulfilledRequestExpireTime", p.fulfilled_request_expire_time),
        ("creditPoolPeriodBlocks", p.credit_pool_period_blocks),
    ];
    out.put_all("ext", ints);
    out.put("ext.chainTxData.nTime", p.chain_tx_data.timestamp);
    out.put("ext.chainTxData.nTxCount", p.chain_tx_data.tx_count);
    out.put("ext.chainTxData.dTxRateBits", format!("{:016x}", p.chain_tx_data.tx_rate.to_bits()));
    out.list("ext.assumeutxo", p.assumeutxo, |out, path, _, au| {
        out.put(&format!("{path}.height"), au.height.to_u32());
        out.put(&format!("{path}.hash_serialized"), core_hex(au.hash_serialized));
        out.put(&format!("{path}.nChainTx"), au.chain_tx);
        if let Some(evo) = au.evo_hash {
            out.put(&format!("{path}.evo_hash"), core_hex(evo));
        }
    });
    out.list("ext.deploymentInfo", p.release.deployments(), |out, path, _, d| {
        out.put(&format!("{path}.name"), d.name());
        out.put(&format!("{path}.gbt_force"), d.gbt_force());
    });
    for &d in p.release.buried_deployments() {
        out.put(&format!("ext.buriedDeployments.{}", d.name()), c.deployment_height(d).to_u32());
    }
    out.list("ext.llmqs", &llmqs, |out, path, _, (t, q)| llmq(out, path, *t, q, true));
    out.list("ext.availableLlmqs", &LlmqType::AVAILABLE, |out, path, _, t| {
        llmq(out, path, *t, &available_params(*t).unwrap(), true)
    });
    out.put("ext.bech32PlatformHRP", p.platform_hrp.expect("v24 has a Platform HRP"));
    out.put("ext.MAX_LLMQ_SIZE", MAX_LLMQ_SIZE);
    out.put("ext.minBlocksToKeep", p.min_blocks_to_keep);
    out.put("params.consensus.IPv4OptionalHeight", c.ipv4_optional_height.to_u32());
    let signers = &p.spork_signers;
    out.list("spork.keyIDs", &signers.key_ids, |out, path, _, id| {
        out.put(path, id.as_bytes().to_lower_hex_string());
    });
    if let Some(min) = signers.min_spork_keys {
        out.put("spork.minSporkKeys", min);
    }
    if let Some(warning) = &signers.warning {
        out.put("spork.warning", warning.as_str());
    }
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
        for key in BLOCK_KEYS {
            want.remove(key);
        }
        let got = dump(&p);
        for (path, v) in &want {
            assert_eq!(got.get(path), Some(v), "{corpus} {name}: {path}");
        }
        assert_eq!(got.len(), want.len(), "{corpus} {name}: extra keys");
    }
}
