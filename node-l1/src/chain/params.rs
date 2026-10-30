//! The schedule parity check (O2b §2.5, D8): the two sides take one argument list, Core through
//! `gArgs` and rdashd through its init's `ChainParamsBuilder`. At process start Core's chain
//! parameters for the schedule (`oracle_chainparams_json`) are compared, leaf by leaf, with the
//! parameters rdashd's init builds from the same strings, rendered in the oracle's shape. A
//! difference stops the leg: it is a harness error, never a finding.
//!
//! The oracle parses the chain options only (`SetupChainParamsOptions`; it refuses the node's,
//! `-fastprune` among them), so the comparison is at [`Schedule::chain_args`]. One node option
//! reaches the chain parameters: on regtest `-fastprune` sets `nPruneAfterHeight` to 100
//! (`chainparams.cpp:875`). The parameters the node runs with must differ from the compared ones
//! by exactly that leaf.
//!
//! The rendering is the `params` half of base-sdk's corpus dump (`pkgs/params/tests/
//! core_corpus.rs` `dump`, at the fork's pin), which base-sdk's own test holds equal to Core's on
//! both pins. Only what the oracle prints is compared. Every `ConsensusParams` field is; these
//! `ChainParams` fields are not:
//! - `spork` and `spork_signers` (the signers' addresses and the minimum key count, after
//!   `-sporkaddr` and `-minsporkkeys`): a SPORK op's reply depends on them and is compared when
//!   the op is an input's first, but `post.sporks` comes after `post.flushes`, which the node
//!   does not fill until a10. Until then only `l1_chain_diff`'s direct asserts hold the sporks
//!   equal.
//! - `credit_pool_period_blocks`, `assumeutxo` and `chain_tx_data`.
//! - the network and policy fields and the `ext` members.
//!
//! None of the last two groups is read by a compared plane of the ops served today.

use std::collections::BTreeMap;

use dash_args::ArgsManager;
use dash_oracle_sys::{Chain, Oracle, Pin};
use dash_params::{ChainParams, LlmqParams};
use dash_primitives::{Block, BlockHeader};
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};
use dashd_node::init::{select_params, setup_server_args};
use serde_json::Value;

use super::core::Schedule;
use super::shape::{block_hash_hex, core_hex, hex};

/// Leaf values keyed by dotted path; `#` is an array's length (the corpus' keys).
type Flat = BTreeMap<String, Value>;

/// A leaf where the two sides differ: its path, then each side's value (`None`: no such leaf).
type Difference = (String, Option<Value>, Option<Value>);

fn serialize(block: &Block) -> String {
    let mut buf = Vec::new();
    block.encode(&mut buf);
    hex(buf)
}

fn put(out: &mut Flat, path: &str, value: impl Into<Value>) {
    out.insert(path.to_owned(), value.into());
}

fn put_all<V: Into<Value>>(
    out: &mut Flat,
    prefix: &str,
    fields: impl IntoIterator<Item = (&'static str, V)>,
) {
    for (name, value) in fields {
        put(out, &format!("{prefix}.{name}"), value);
    }
}

fn list<T>(
    out: &mut Flat,
    path: &str,
    items: &[T],
    mut each: impl FnMut(&mut Flat, &str, usize, &T),
) {
    put(out, &format!("{path}.#"), items.len());
    for (i, item) in items.iter().enumerate() {
        each(out, &format!("{path}.{i}"), i, item);
    }
}

fn llmq(out: &mut Flat, path: &str, q: &LlmqParams) {
    put(out, &format!("{path}.type"), q.llmq_type.to_base());
    put(out, &format!("{path}.name"), q.name);
    put(out, &format!("{path}.useRotation"), q.use_rotation);
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
    put_all(out, path, fields);
}

/// `p` as the oracle's `ChainParamsJson()` prints it, flattened (Core's `core_version` aside).
pub fn core_shape(p: &ChainParams) -> Flat {
    let mut flat = Flat::new();
    let out = &mut flat;
    let c = &p.consensus;
    let a = &p.addr_params;
    // A null `CBlock` off devnets (Core's `DevNetGenesisBlock()`).
    let devnet_genesis = p.devnet_genesis.clone().unwrap_or_else(|| Block {
        header: BlockHeader::default(),
        transactions: Vec::new(),
    });
    put(out, "networkID", p.network_id());
    put(out, "messageStart", hex(p.message_start));
    put(out, "defaultPort", p.default_port);
    put(out, "pruneAfterHeight", p.prune_after_height);
    put(out, "genesisHash", block_hash_hex(p.genesis.header.hash()));
    put(out, "genesisBlock", serialize(&p.genesis));
    put(out, "devnetGenesisHash", block_hash_hex(devnet_genesis.header.hash()));
    put(out, "extCoinType", p.ext_coin_type());
    put(out, "base58Prefixes.PUBKEY_ADDRESS", format!("{:02x}", a.pubkey_addr));
    put(out, "base58Prefixes.SCRIPT_ADDRESS", format!("{:02x}", a.script_addr));
    put(out, "base58Prefixes.SECRET_KEY", format!("{:02x}", a.secret_key));
    put(out, "base58Prefixes.EXT_PUBLIC_KEY", hex(a.ext_pubkey));
    put(out, "base58Prefixes.EXT_SECRET_KEY", hex(a.ext_secret));
    for (h, hash) in &p.checkpoints {
        put(out, &format!("checkpoints.{}", h.to_u32()), core_hex(*hash));
    }

    let hashes = [
        ("hashGenesisBlock", c.hash_genesis_block),
        ("hashDevnetGenesisBlock", c.hash_devnet_genesis_block),
        ("nSuperblockStartHash", c.superblock_start.1),
        ("BIP34Hash", c.bip34.1),
        ("DIP0003EnforcementHash", c.dip0003_enforcement.1),
        ("defaultAssumeValid", c.default_assume_valid),
    ];
    put_all(out, "consensus", hashes.map(|(n, h)| (n, core_hex(h))));
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
    put_all(out, "consensus", heights.map(|(n, h)| (n, h.to_u32())));
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
    put_all(out, "consensus", ints);
    put(out, "consensus.nRuleChangeActivationThreshold", c.rule_change_activation_threshold);
    put(out, "consensus.nMinerConfirmationWindow", c.miner_confirmation_window);
    let positions = p.release.deployments();
    list(out, "consensus.vDeployments", &c.deployments, |out, path, i, d| {
        put(out, &format!("{path}.name"), positions[i].name());
        put(out, &format!("{path}.bit"), d.bit);
        put(out, &format!("{path}.min_activation_height"), d.min_activation_height.to_u32());
        let fields = [
            ("nStartTime", d.start_time),
            ("nTimeout", d.timeout),
            ("nWindowSize", d.window_size),
            ("nThresholdStart", d.threshold_start),
            ("nThresholdMin", d.threshold_min),
            ("nFalloffCoeff", d.falloff_coeff),
        ];
        put_all(out, path, fields);
        put(out, &format!("{path}.useEHF"), d.use_ehf);
    });
    put(out, "consensus.powLimit", hex(c.pow_limit.to_bendian()));
    put(out, "consensus.fPowAllowMinDifficultyBlocks", c.pow_allow_min_difficulty_blocks);
    put(out, "consensus.fPowNoRetargeting", c.pow_no_retargeting);
    put(out, "consensus.nPowTargetSpacing", c.pow_target_spacing);
    put(out, "consensus.nPowTargetTimespan", c.pow_target_timespan);
    put(out, "consensus.nMinimumChainWork", hex(c.minimum_chain_work.to_bendian()));
    list(out, "consensus.llmqs", &c.llmqs, |out, path, _, q| llmq(out, path, q));
    put(out, "consensus.llmqTypeChainLocks", c.llmq_type_chain_locks.to_base());
    put(out, "consensus.llmqTypeDIP0024InstantSend", c.llmq_type_dip0024_instant_send.to_base());
    put(out, "consensus.llmqTypePlatform", c.llmq_type_platform.to_base());
    put(out, "consensus.llmqTypeMnhf", c.llmq_type_mnhf.to_base());
    flat
}

/// A JSON value's leaves keyed as [`core_shape`] keys them.
fn flatten(value: &Value) -> Flat {
    fn walk(value: &Value, prefix: &str, out: &mut Flat) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    walk(v, &format!("{prefix}{k}."), out);
                }
            }
            Value::Array(items) => {
                out.insert(format!("{prefix}#"), items.len().into());
                for (i, v) in items.iter().enumerate() {
                    walk(v, &format!("{prefix}{i}."), out);
                }
            }
            leaf => {
                out.insert(prefix.trim_end_matches('.').to_owned(), leaf.clone());
            }
        }
    }
    let mut out = Flat::new();
    walk(value, "", &mut out);
    out
}

/// Core's chain parameters for `schedule` at `pin`, flattened, without `core_version`.
pub fn core_params(pin: Pin, schedule: Schedule) -> Result<Flat, String> {
    let chain = Chain::new("regtest", &schedule.chain_args());
    let json = Oracle::new(pin)
        .chainparams_json(&chain)
        .map_err(|e| format!("oracle_chainparams_json: {}", e.message))?;
    let value: Value =
        serde_json::from_str(&json).map_err(|e| format!("oracle_chainparams_json: {e}"))?;
    let mut flat = flatten(&value);
    flat.remove("core_version");
    Ok(flat)
}

/// The leaves where `core` and `rust` differ, by path: `(Core's, rdashd's)`, `None` where a
/// side has no such leaf.
pub fn differences(core: &Flat, rust: &Flat) -> Vec<Difference> {
    let mut keys: Vec<&String> = core.keys().chain(rust.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| core.get(*k) != rust.get(*k))
        .map(|k| (k.clone(), core.get(k).cloned(), rust.get(k).cloned()))
        .collect()
}

/// rdashd's chain parameters for `args`: its init's parse, then `SelectParams` (regtest).
pub fn node_params(args: &[&str]) -> Result<ChainParams, String> {
    let manager = ArgsManager::new();
    setup_server_args(&manager);
    let mut argv = vec!["rdashd", "-regtest"];
    argv.extend_from_slice(args);
    manager.parse_parameters(&argv)?;
    Ok(select_params(&manager, "regtest").map_err(|e| e.0)?.chain)
}

fn report(what: &str, diffs: &[Difference]) -> String {
    let lines: Vec<String> = diffs.iter().map(|(k, a, b)| format!("{k}: {a:?} vs {b:?}")).collect();
    format!("{what} ({} leaves): {}", diffs.len(), lines.join("; "))
}

/// D8's check for `schedule`, given the parameters the node runs with (`NodeSut::params`).
/// `Err` lists every differing leaf (a harness error; the leg stops).
pub fn check(pin: Pin, schedule: Schedule, running: &ChainParams) -> Result<(), String> {
    let compared = core_shape(&node_params(&schedule.chain_args())?);
    let diffs = differences(&core_params(pin, schedule)?, &compared);
    if !diffs.is_empty() {
        let what = format!("schedule {}: Core's chain parameters vs rdashd's", schedule.name());
        return Err(report(&what, &diffs));
    }
    // Core: chainparams.cpp:875
    let expected: Vec<Difference> = if schedule.args().contains(&"-fastprune") {
        vec![("pruneAfterHeight".to_owned(), Some(Value::from(1000)), Some(Value::from(100)))]
    } else {
        Vec::new()
    };
    let diffs = differences(&compared, &core_shape(running));
    if diffs != expected {
        let what = format!(
            "schedule {}: the node's parameters vs its chain options' (only -fastprune's \
             pruneAfterHeight may differ, by {expected:?})",
            schedule.name()
        );
        return Err(report(&what, &diffs));
    }
    Ok(())
}
