//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! The corpus shape of a built setup, shared by the corpus test and the fork-equivalence test.

#![allow(dead_code, reason = "each test crate uses its own part")]

use dash_chainparams::{LlmqParams, LlmqParamsExt, LlmqType};
use dash_num::Hash256;
use dash_types::Numeric;
use hex_conservative::DisplayHex;
use serde_json::Value;

use std::collections::BTreeMap;

/// Corpus keys holding block bytes, which dash-consensus checks.
pub const BLOCK_KEYS: [&str; 2] = ["params.genesisBlock", "ext.devnetGenesisBlock"];

/// Core's `uint256::GetHex()`: the reversed internal bytes.
pub fn core_hex(hash: Hash256) -> String {
    hash.to_bendian().to_lower_hex_string()
}

/// The hash of a null `CBlock`, Core's `DevNetGenesisBlock()` off devnets.
pub fn null_block_hash() -> Hash256 {
    Hash256::from_lendian(dash_x11::x11(&[0; 80]))
}

/// Leaf values keyed by dotted path; `#` is an array's length.
#[derive(Default)]
pub struct Flat(pub BTreeMap<String, Value>);

impl Flat {
    pub fn put(&mut self, path: &str, value: impl Into<Value>) {
        self.0.insert(path.into(), value.into());
    }

    pub fn put_all<V: Into<Value>>(
        &mut self,
        prefix: &str,
        fields: impl IntoIterator<Item = (&'static str, V)>,
    ) {
        for (name, value) in fields {
            self.put(&format!("{prefix}.{name}"), value);
        }
    }

    pub fn list<T>(
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

pub fn llmq(out: &mut Flat, path: &str, llmq_type: LlmqType, q: &LlmqParams, derived: bool) {
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
        out.put(
            &format!("{path}.max_store_depth"),
            <LlmqParams as LlmqParamsExt>::max_store_depth(q),
        );
        out.put(
            &format!("{path}.is_single_member"),
            <LlmqParams as LlmqParamsExt>::is_single_member(q),
        );
    }
}

/// `$setup` in the shape of the corpus, flattened, with the runtime policy `$policy`: the oracle's
/// `ChainParamsJson()` under `params` and the other `CChainParams` members
/// under `ext`, without the [`BLOCK_KEYS`].
///
/// The values of Core's static LLMQ table (`ext.availableLlmqs`, `ext.MAX_LLMQ_SIZE`) are not the
/// setup's, so the caller adds them. A macro, since the fork-equivalence test renders the fork's
/// `ChainSetup` too: the field reads are the same text for both types.
macro_rules! dump_setup {
    ($setup:expr, $policy:expr) => {{
        use dash_types::Numeric as _;
        use hex_conservative::DisplayHex as _;

        let p = $setup;
        let policy: &dash_chainparams::ChainPolicy = $policy;
        let mut out = $crate::dump::Flat::default();
        let c = &p.consensus;
        let a = &p.addr_params;
        let devnet_genesis_hash =
            p.devnet_genesis.as_ref().map_or_else($crate::dump::null_block_hash, |g| g.hash);
        out.put("params.networkID", p.network_id);
        out.put("params.messageStart", p.message_start.to_lower_hex_string());
        out.put("params.defaultPort", p.default_port);
        out.put("params.pruneAfterHeight", policy.prune_after_height);
        out.put("params.genesisHash", $crate::dump::core_hex(c.hash_genesis_block));
        out.put("params.devnetGenesisHash", $crate::dump::core_hex(devnet_genesis_hash));
        out.put("params.extCoinType", a.bip44_idx);
        out.put("params.base58Prefixes.PUBKEY_ADDRESS", format!("{:02x}", a.pubkey_addr));
        out.put("params.base58Prefixes.SCRIPT_ADDRESS", format!("{:02x}", a.script_addr));
        out.put("params.base58Prefixes.SECRET_KEY", format!("{:02x}", a.secret_key));
        out.put("params.base58Prefixes.EXT_PUBLIC_KEY", a.ext_pubkey.to_lower_hex_string());
        out.put("params.base58Prefixes.EXT_SECRET_KEY", a.ext_secret.to_lower_hex_string());
        for (h, hash) in &p.checkpoints {
            out.put(&format!("params.checkpoints.{}", h.to_u32()), $crate::dump::core_hex(*hash));
        }

        let hashes = [
            ("hashGenesisBlock", c.hash_genesis_block),
            ("hashDevnetGenesisBlock", c.hash_devnet_genesis_block),
            ("nSuperblockStartHash", c.superblock_start.1),
            ("BIP34Hash", c.bip34.1),
            ("DIP0003EnforcementHash", c.dip0003_enforcement.1),
            ("defaultAssumeValid", c.default_assume_valid),
        ];
        out.put_all("params.consensus", hashes.map(|(n, h)| (n, $crate::dump::core_hex(h))));
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
        out.put(
            "params.consensus.nRuleChangeActivationThreshold",
            c.rule_change_activation_threshold,
        );
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
            $crate::dump::llmq(out, path, *t, q, false)
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
        out.put("ext.assumedBlockchainSize", policy.assumed_blockchain_size_gb);
        out.put("ext.assumedChainStateSize", policy.assumed_chain_state_size_gb);
        out.list("ext.dnsSeeds", p.dns_seeds, |out, path, _, s| out.put(path, *s));
        out.put("ext.fixedSeeds", p.fixed_seeds.to_lower_hex_string());
        out.list("ext.sporkAddresses", &[p.spork_address], |out, path, _, s| out.put(path, *s));
        let flags = [
            ("defaultConsistencyChecks", policy.default_consistency_checks),
            ("requireRoutableExternalIP", p.require_routable_external_ip),
            ("isTestChain", p.is_test_chain),
            ("isMockableChain", policy.is_mockable_chain),
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
        out.put(
            "ext.chainTxData.dTxRateBits",
            format!("{:016x}", p.chain_tx_data.tx_rate.to_bits()),
        );
        out.list("ext.assumeutxo", p.assumeutxo, |out, path, _, au| {
            out.put(&format!("{path}.height"), au.height.to_u32());
            out.put(&format!("{path}.hash_serialized"), $crate::dump::core_hex(au.hash_serialized));
            out.put(&format!("{path}.nChainTx"), au.chain_tx);
            if let Some(evo) = au.evo_hash {
                out.put(&format!("{path}.evo_hash"), $crate::dump::core_hex(evo));
            }
        });
        out.list("ext.deploymentInfo", p.release.deployments(), |out, path, _, d| {
            out.put(&format!("{path}.name"), d.name());
            out.put(&format!("{path}.gbt_force"), d.gbt_force());
        });
        for &d in p.release.buried_deployments() {
            out.put(
                &format!(
                    "ext.buriedDeployments.{}",
                    dash_chainparams::BuriedDeploymentExt::name(d)
                ),
                c.deployment_height(d).to_u32(),
            );
        }
        out.list("ext.llmqs", &llmqs, |out, path, _, (t, q)| {
            $crate::dump::llmq(out, path, *t, q, true)
        });
        out.put("ext.bech32PlatformHRP", p.platform_hrp.expect("v24 has a Platform HRP"));
        out.put("ext.minBlocksToKeep", policy.min_blocks_to_keep);
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
        out
    }};
}

pub(crate) use dump_setup;
