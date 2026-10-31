//! Helpers shared by the integration tests and the bench: Core's chain parameters, recorded from
//! the O1 oracle (`tests/data/PROVENANCE.md`), as the inputs of the crate's parameter views, the
//! recorded cases, JSON accessors and an in-memory header chain.

#![allow(dead_code, reason = "each test binary uses a subset")]

use std::fmt::Debug;

use dash_consensus::payments::{MnRewardEra, PaymentParams};
use dash_consensus::pow::PowParams;
use dash_consensus::subsidy::SubsidyParams;
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_core_vectors::hex::parse_hex;
use dash_num::{Arith256, CompactTarget, Hash256};
use dash_params::{
    ChainOption, ChainParams, ChainParamsBuilder, ConsensusParams, NetworkKind, Release,
};
use dash_primitives::{Block, BlockHash, LlmqType};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use serde_json::Value;

/// A file under `tests/data/`, parsed.
pub fn data(relative: &str) -> Value {
    let path = format!("{}/tests/data/{relative}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The cases of a recorded oracle file (`tests/data/oracle/<file>`).
pub fn oracle_cases(file: &str) -> Vec<Value> {
    let Value::Array(cases) = data(&format!("oracle/{file}")) else {
        panic!("{file}: not an array of cases");
    };
    cases
}

/// Core's chain parameters for a pin (`v24.0`) and a recorded chain name, with the
/// chain selection that produced them (`tests/data/PROVENANCE.md`).
pub struct Recorded {
    pub pin: String,
    pub name: String,
    pub json: Value,
}

impl std::ops::Index<&str> for Recorded {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        &self.json[key]
    }
}

impl Recorded {
    /// The release of the recorded pin.
    pub fn release(&self) -> Release {
        match self.pin.as_str() {
            "v24.0" => Release::V24_0,
            pin => panic!("unknown pin {pin}"),
        }
    }

    /// `dash-params`' parameters for the chain selection Core recorded (PROVENANCE.md).
    pub fn chain(&self) -> ChainParams {
        let (network, args) = selection(&self.name);
        let mut builder = ChainParamsBuilder::new(network, self.release());
        for (option, value) in args {
            builder = builder.arg(option, value);
        }
        builder.build().unwrap_or_else(|e| panic!("{} {}: {e}", self.pin, self.name))
    }
}

/// (name, Core chain, args): every recorded chain selection, as Core's chain name and command-line
/// arguments (`tests/data/PROVENANCE.md`). cs2-diff records and replays the same table.
pub const SELECTIONS: &[(&str, &str, &[&str])] = &[
    ("main", "main", &[]),
    ("test", "test", &[]),
    ("regtest", "regtest", &[]),
    ("devnet", "devnet", &["-devnet=cs2"]),
    ("devnet-spacing1", "devnet", &["-devnet=cs2", "-powtargetspacing=1"]),
    ("devnet-spacing30", "devnet", &["-devnet=cs2", "-powtargetspacing=30"]),
    ("devnet-spacing60", "devnet", &["-devnet=cs2", "-powtargetspacing=60"]),
    ("devnet-spacing86400", "devnet", &["-devnet=cs2", "-powtargetspacing=86400"]),
    ("devnet-mindiff4000", "devnet", &["-devnet=cs2", "-minimumdifficultyblocks=4000"]),
    (
        "devnet-mindiff4000-spacing86400",
        "devnet",
        &["-devnet=cs2", "-minimumdifficultyblocks=4000", "-powtargetspacing=86400"],
    ),
    (
        "devnet-highsubsidy",
        "devnet",
        &["-devnet=cs2", "-highsubsidyblocks=500", "-highsubsidyfactor=10"],
    ),
    (
        "devnet-highsubsidy-maxfactor",
        "devnet",
        &["-devnet=cs2", "-highsubsidyblocks=300000", "-highsubsidyfactor=184467440"],
    ),
    (
        "devnet-highsubsidy-negative",
        "devnet",
        &["-devnet=cs2", "-highsubsidyblocks=-5", "-highsubsidyfactor=-3"],
    ),
    (
        "devnet-highsubsidy-wrap",
        "devnet",
        &["-devnet=cs2", "-highsubsidyblocks=10", "-highsubsidyfactor=2147483647"],
    ),
    (
        "devnet-highsubsidy-unused",
        "devnet",
        &["-devnet=cs2", "-highsubsidyblocks=0", "-highsubsidyfactor=200000000"],
    ),
    (
        "regtest-brr",
        "regtest",
        &[
            "-dip3params=30:50",
            "-testactivationheight=brr@1000",
            "-testactivationheight=v20@1200",
            "-testactivationheight=mn_rr@2200",
        ],
    ),
    (
        "regtest-budget",
        "regtest",
        &[
            "-budgetparams=10:20:0",
            "-testactivationheight=v20@700",
            "-testactivationheight=mn_rr@700",
        ],
    ),
];

/// Devnets whose `-powtargetspacing` is above `nPowTargetTimespan` (CS2 §2.5, §6.2): Core runs
/// them when `-minimumdifficultyblocks` keeps the BTC rule unreachable. DGW then divides by
/// `24 · spacing` above `u32::MAX` from 178,956,971 on (Core's fallback to its general
/// division), and from 2^29 on its clamped timespan, at least `8 · spacing`, no longer fits
/// `operator*=(uint32_t)`. The span of the windows' `uint32` times is below 2^32, so:
///
/// - 86,401: the clamp mostly takes its upper bound, `72 · spacing`;
/// - 178,956,971: the span falls inside the clamp, a divisor above `u32::MAX`;
/// - 2^29 − 1: the clamp takes its lower bound, `2^32 − 8`, which still fits;
/// - 2^29, 2^29 + 1, 2^31: the lower bound truncates to 0, 8 and 0;
/// - ⌊i64::MAX / 72⌋: to 954,437,176, with `72 · spacing` just inside `int64_t`.
pub const LARGE_SPACING: &[(&str, &str, &[&str])] = &[
    (
        "devnet-mindiff4000-spacing86401",
        "devnet",
        &["-devnet=cs2", "-minimumdifficultyblocks=4000", "-powtargetspacing=86401"],
    ),
    (
        "devnet-mindiff4000-spacing178956971",
        "devnet",
        &["-devnet=cs2", "-minimumdifficultyblocks=4000", "-powtargetspacing=178956971"],
    ),
    (
        "devnet-mindiff4000-spacing536870911",
        "devnet",
        &["-devnet=cs2", "-minimumdifficultyblocks=4000", "-powtargetspacing=536870911"],
    ),
    (
        "devnet-mindiff4000-spacing536870912",
        "devnet",
        &["-devnet=cs2", "-minimumdifficultyblocks=4000", "-powtargetspacing=536870912"],
    ),
    (
        "devnet-mindiff4000-spacing536870913",
        "devnet",
        &["-devnet=cs2", "-minimumdifficultyblocks=4000", "-powtargetspacing=536870913"],
    ),
    (
        "devnet-mindiff4000-spacing2147483648",
        "devnet",
        &["-devnet=cs2", "-minimumdifficultyblocks=4000", "-powtargetspacing=2147483648"],
    ),
    (
        "devnet-mindiff4000-spacing128102389400760775",
        "devnet",
        &["-devnet=cs2", "-minimumdifficultyblocks=4000", "-powtargetspacing=128102389400760775"],
    ),
];

/// `dash-params`' network and options for a recorded chain name, from [`SELECTIONS`] or
/// [`LARGE_SPACING`].
fn selection(name: &str) -> (NetworkKind, Vec<(ChainOption, String)>) {
    let (_, chain, args) = SELECTIONS
        .iter()
        .chain(LARGE_SPACING)
        .find(|s| s.0 == name)
        .unwrap_or_else(|| panic!("no chain selection recorded for {name}"));
    let network = NetworkKind::from_network_id(chain).expect("a Core chain");
    let options = args
        .iter()
        .map(|arg| {
            let (key, value) = arg.split_once('=').expect("-option=value");
            (ChainOption::from_name(key).expect("a chain option"), value.to_owned())
        })
        .collect();
    (network, options)
}

/// Core's chain parameters for a pin (`v24.0`) and a recorded chain name.
pub fn chain_params(pin: &str, name: &str) -> Recorded {
    Recorded {
        pin: pin.into(),
        name: name.into(),
        json: data(&format!("chainparams/{pin}/{name}.json")),
    }
}

pub fn int(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("not an integer: {v}"))
}

pub fn int32(v: &Value) -> i32 {
    i32::try_from(int(v)).unwrap_or_else(|_| panic!("not an int: {v}"))
}

pub fn uint32(v: &Value) -> u32 {
    u32::try_from(int(v)).unwrap_or_else(|_| panic!("not a uint32: {v}"))
}

/// A 64-bit value recorded as 16 hex digits (a double's bit pattern).
pub fn hex64(v: &Value) -> u64 {
    let hex = v.as_str().unwrap_or_else(|| panic!("not a hex string: {v}"));
    u64::from_str_radix(hex, 16).unwrap_or_else(|_| panic!("not 64-bit hex: {v}"))
}

pub fn flag(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| panic!("not a bool: {v}"))
}

/// Core's raw fields copied into `template`, including the network and the devnet arguments.
/// Only raw fields are copied: whatever the views derive from them (quirk 15's `nBits`, the
/// network paths) they derive themselves; the hard-coded pins are `dash-params`' own.
fn chain_from(cp: &Value, template: ChainParams) -> ChainParams {
    let c = &cp["consensus"];
    let height = |key: &str| uint32(&c[key]).into();
    let id = cp["networkID"].as_str().expect("networkID");
    let mut chain = template;
    chain.network = NetworkKind::from_network_id(id).expect("a Core network");
    let k = &mut chain.consensus;
    k.pow_limit = c["powLimit"].as_str().expect("powLimit").parse().expect("hex");
    k.pow_allow_min_difficulty_blocks = flag(&c["fPowAllowMinDifficultyBlocks"]);
    k.pow_no_retargeting = flag(&c["fPowNoRetargeting"]);
    k.pow_target_spacing = int(&c["nPowTargetSpacing"]);
    k.pow_target_timespan = int(&c["nPowTargetTimespan"]);
    k.pow_kgw_height = height("nPowKGWHeight");
    k.pow_dgw_height = height("nPowDGWHeight");
    k.subsidy_halving_interval = int32(&c["nSubsidyHalvingInterval"]);
    k.budget_payments_start_block = height("nBudgetPaymentsStartBlock");
    k.superblock_start.0 = height("nSuperblockStartBlock");
    k.superblock_cycle = int32(&c["nSuperblockCycle"]);
    k.v20_height = height("V20Height");
    k.masternode_payments_increase_block = height("nMasternodePaymentsIncreaseBlock");
    k.masternode_payments_increase_period = int32(&c["nMasternodePaymentsIncreasePeriod"]);
    k.brr_height = height("BRRHeight");
    k.minimum_difficulty_blocks = int32(&c["nMinimumDifficultyBlocks"]);
    k.high_subsidy_blocks = int32(&c["nHighSubsidyBlocks"]);
    k.high_subsidy_factor = int32(&c["nHighSubsidyFactor"]);
    chain
}

/// A view built through the crate's constructor, from `dash-params`' parameters for the recorded
/// chain selection. It is built a second time from Core's raw fields copied into [`scrambled`]
/// parameters, which differ in every other field; the two must be equal. A field the views read
/// but `chain_from` does not copy, or a builder value that is not Core's, shows up as a
/// difference.
fn view<T: PartialEq + Debug>(cp: &Recorded, build: impl Fn(&ChainParams) -> T) -> T {
    let built = cp.chain();
    let from_built = build(&built);
    let other = scrambled(built.clone());
    assert_all_differ(&built, &other);
    let from_core = build(&chain_from(&cp.json, other));
    assert_eq!(from_core, from_built, "{} {}: views differ from Core's fields", cp.pin, cp.name);
    from_built
}

/// The top-level fields of a pretty `Debug` rendering, by name: each field is its first line at
/// the outer indent plus the lines nested under it.
fn debug_fields(debug: &str) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in debug.lines().skip(1) {
        let field = line.strip_prefix("    ").filter(|r| !r.starts_with(' '));
        if let Some((name, _)) = field.and_then(|rest| rest.split_once(':')) {
            fields.push((name.to_owned(), String::new()));
        }
        if let Some(last) = fields.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    fields
}

/// Every field of `scrambled` differs from `chain`'s but `network`, `release` and `pins`, and
/// every field of its consensus parameters differs: a field `dash-params` adds and [`scrambled`]
/// misses fails here (`ChainParams` is `non_exhaustive`, so the compiler cannot).
fn assert_all_differ(chain: &ChainParams, scrambled: &ChainParams) {
    for (a, b, kept) in [
        (format!("{chain:#?}"), format!("{scrambled:#?}"), &["network", "release", "pins"][..]),
        (format!("{:#?}", chain.consensus), format!("{:#?}", scrambled.consensus), &[][..]),
    ] {
        let (a, b) = (debug_fields(&a), debug_fields(&b));
        assert_eq!(a.len(), b.len());
        assert!(a.len() > 20, "too few fields parsed: {}", a.len());
        for ((name, x), (_, y)) in a.iter().zip(&b) {
            assert_eq!(x == y, kept.contains(&name.as_str()), "scrambled field {name}");
        }
    }
}

/// `chain` with every field changed but `network` (which `chain_from` sets), `release` (v24 is
/// the only release, DEC-82) and the hard-coded `pins` (`dash-params`' own, not in Core's dump).
/// [`assert_all_differ`] checks that no field is missed.
fn scrambled(mut chain: ChainParams) -> ChainParams {
    chain.devnet_name = match chain.devnet_name {
        Some(_) => None,
        None => Some("devnet-scrambled".into()),
    };
    chain.consensus = scrambled_consensus(chain.consensus);
    chain.message_start = chain.message_start.map(|b| !b);
    chain.default_port ^= 1;
    chain.default_platform_p2p_port ^= 1;
    chain.default_platform_http_port ^= 1;
    chain.rpc_port ^= 1;
    chain.onion_service_target_port ^= 1;
    chain.prune_after_height ^= 1;
    chain.assumed_blockchain_size_gb ^= 1;
    chain.assumed_chain_state_size_gb ^= 1;
    chain.dns_seeds = if chain.dns_seeds.is_empty() {
        &["scrambled."]
    } else {
        &[]
    };
    chain.fixed_seeds = if chain.fixed_seeds.is_empty() {
        &[0]
    } else {
        &[]
    };
    let a = &mut chain.addr_params;
    a.pubkey_addr ^= 1;
    a.script_addr ^= 1;
    a.secret_key ^= 1;
    a.ext_pubkey[0] ^= 1;
    a.ext_secret[0] ^= 1;
    a.bip44_idx ^= 1;
    chain.platform_hrp = if chain.platform_hrp.is_some() {
        None
    } else {
        Some("scrambled")
    };
    chain.is_test_chain = !chain.is_test_chain;
    chain.require_standard = chain.require_standard.map_or(Some(true), |r| (!r).then_some(true));
    chain.default_consistency_checks = !chain.default_consistency_checks;
    chain.is_mockable_chain = !chain.is_mockable_chain;
    chain.require_routable_external_ip = !chain.require_routable_external_ip;
    chain.allow_multiple_addresses_from_group = !chain.allow_multiple_addresses_from_group;
    chain.llmq_connection_retry_timeout ^= 1;
    chain.pool_min_participants ^= 1;
    chain.pool_max_participants ^= 1;
    chain.fulfilled_request_expire_time ^= 1;
    chain.credit_pool_period_blocks ^= 1;
    chain.spork.addresses = if chain.spork.addresses.is_empty() {
        &["scrambled"]
    } else {
        &[]
    };
    chain.spork.min_spork_keys = chain.spork.min_spork_keys.map_or(Some(7), |_| None);
    if chain.spork_signers.key_ids.pop().is_none() {
        chain.spork_signers.key_ids.push(Default::default());
    }
    chain.spork_signers.min_spork_keys =
        chain.spork_signers.min_spork_keys.map_or(Some(7), |_| None);
    chain.spork_signers.warning = Some("scrambled".into());
    // one checkpoint fewer, or one more
    if chain.checkpoints.pop().is_none() {
        chain.checkpoints.push((1u32.into(), Hash256::default()));
    }
    chain.assumeutxo = if chain.assumeutxo.is_empty() {
        let regtest = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build();
        regtest.expect("Core's parameters").assumeutxo
    } else {
        &[]
    };
    chain.chain_tx_data.timestamp ^= 1;
    chain.chain_tx_data.tx_count ^= 1;
    chain.chain_tx_data.tx_rate += 1.0;
    chain.genesis.header.nonce ^= 1;
    chain.devnet_genesis = match chain.devnet_genesis {
        Some(_) => None,
        None => Some(chain.genesis.clone()),
    };
    chain
}

/// `consensus` with every field changed.
fn scrambled_consensus(mut k: ConsensusParams) -> ConsensusParams {
    fn bump<H: Copy + Into<u32> + From<u32>>(h: &mut H) {
        *h = H::from((*h).into() ^ 1);
    }
    fn flip(h: &mut Hash256) {
        *h = Hash256::from_bendian(h.as_bytes().map(|b| !b));
    }
    fn other(t: &mut LlmqType) {
        *t = if *t == LlmqType::Llmq50_60 {
            LlmqType::Llmq60_75
        } else {
            LlmqType::Llmq50_60
        };
    }
    flip(&mut k.hash_genesis_block);
    flip(&mut k.hash_devnet_genesis_block);
    k.subsidy_halving_interval ^= 1;
    bump(&mut k.masternode_payments_start_block);
    bump(&mut k.masternode_payments_increase_block);
    k.masternode_payments_increase_period ^= 1;
    k.instant_send_confirmations_required ^= 1;
    k.instant_send_keep_lock ^= 1;
    bump(&mut k.budget_payments_start_block);
    k.budget_payments_cycle_blocks ^= 1;
    k.budget_payments_window_blocks ^= 1;
    bump(&mut k.superblock_start.0);
    flip(&mut k.superblock_start.1);
    k.superblock_cycle ^= 1;
    k.superblock_maturity_window ^= 1;
    k.governance_min_quorum ^= 1;
    k.governance_filter_elements ^= 1;
    k.masternode_minimum_confirmations ^= 1;
    bump(&mut k.bip34.0);
    flip(&mut k.bip34.1);
    for h in [
        &mut k.bip65_height,
        &mut k.bip66_height,
        &mut k.bip147_height,
        &mut k.csv_height,
        &mut k.dip0001_height,
        &mut k.dip0003_height,
        &mut k.dip0003_enforcement.0,
        &mut k.dip0008_height,
        &mut k.brr_height,
        &mut k.dip0020_height,
        &mut k.dip0024_height,
        &mut k.dip0024_quorums_height,
        &mut k.v19_height,
        &mut k.v20_height,
        &mut k.mn_rr_height,
        &mut k.withdrawals_height,
        &mut k.min_bip9_warning_height,
        &mut k.pow_kgw_height,
        &mut k.pow_dgw_height,
    ] {
        bump(h);
    }
    flip(&mut k.dip0003_enforcement.1);
    k.rule_change_activation_threshold ^= 1;
    k.miner_confirmation_window ^= 1;
    for d in &mut k.deployments {
        d.bit ^= 1;
        d.start_time ^= 1;
        d.timeout ^= 1;
        bump(&mut d.min_activation_height);
        d.window_size ^= 1;
        d.threshold_start ^= 1;
        d.threshold_min ^= 1;
        d.falloff_coeff ^= 1;
        d.use_ehf = !d.use_ehf;
    }
    k.pow_limit += Arith256::ONE;
    k.pow_allow_min_difficulty_blocks = !k.pow_allow_min_difficulty_blocks;
    k.pow_no_retargeting = !k.pow_no_retargeting;
    k.pow_target_spacing ^= 1;
    k.pow_target_timespan ^= 1;
    k.minimum_chain_work += Arith256::ONE;
    flip(&mut k.default_assume_valid);
    k.minimum_difficulty_blocks ^= 1;
    k.high_subsidy_blocks ^= 1;
    k.high_subsidy_factor ^= 1;
    for q in &mut k.llmqs {
        other(&mut q.llmq_type);
        q.name = "scrambled";
        q.use_rotation = !q.use_rotation;
        for f in [
            &mut q.size,
            &mut q.min_size,
            &mut q.threshold,
            &mut q.dkg_interval,
            &mut q.dkg_phase_blocks,
            &mut q.dkg_mining_window_start,
            &mut q.dkg_mining_window_end,
            &mut q.dkg_bad_votes_threshold,
            &mut q.signing_active_quorum_count,
            &mut q.keep_old_connections,
            &mut q.keep_old_keys,
            &mut q.recovery_members,
        ] {
            *f ^= 1;
        }
    }
    other(&mut k.llmq_type_chain_locks);
    other(&mut k.llmq_type_dip0024_instant_send);
    other(&mut k.llmq_type_platform);
    other(&mut k.llmq_type_mnhf);
    k
}

/// `PowParams` from Core's chain parameters.
pub fn pow_params(cp: &Recorded) -> PowParams {
    view(cp, |chain| PowParams::from_chain_params(chain).expect("Core's"))
}

/// `SubsidyParams` from Core's chain parameters. The genesis reward comes from each chain's own
/// genesis block, and `dash-params`' must equal Core's.
pub fn subsidy_params(cp: &Recorded) -> SubsidyParams {
    let core = genesis_reward(cp);
    let own = |chain: &ChainParams| {
        dash_consensus::subsidy::genesis_reward(&chain.genesis).expect("a CAmount")
    };
    assert_eq!(own(&cp.chain()), core, "{} {}: genesis reward", cp.pin, cp.name);
    view(cp, |chain| {
        SubsidyParams::from_chain_params(chain, own(chain)).expect("Core's parameters")
    })
}

/// `PaymentParams` from Core's chain parameters.
pub fn payment_params(cp: &Recorded) -> PaymentParams {
    view(cp, |chain| PaymentParams::from_chain_params(chain).expect("Core's parameters"))
}

/// The value of the genesis coinbase, from Core's serialized genesis block.
pub fn genesis_reward(cp: &Recorded) -> i64 {
    let bytes = parse_hex(cp["genesisBlock"].as_str().expect("genesisBlock"));
    let block = Block::decode(&mut bytes.as_slice()).expect("Core's genesis block decodes");
    dash_consensus::subsidy::genesis_reward(&block).expect("a CAmount")
}

/// The one place tests build a [`HeaderCtx`]: the fields `pow` and `subsidy` read, the others
/// zero.
pub fn header_ctx(height: u32, time: u32, bits: u32) -> HeaderCtx {
    HeaderCtx {
        height,
        hash: BlockHash::from([0; 32]),
        time,
        bits: CompactTarget::new(bits),
        version: 0,
        mtp: 0,
        chain_work: Arith256::ZERO,
    }
}

/// The eras and the names the recorded data uses for them.
pub const ERAS: [(MnRewardEra, &str); 3] = [
    (MnRewardEra::Classic, "classic"),
    (MnRewardEra::CreditPool, "credit_pool"),
    (MnRewardEra::EvoReward, "evo_reward"),
];

/// The era a recorded name stands for.
pub fn era(name: &str) -> MnRewardEra {
    ERAS.iter().find(|e| e.1 == name).unwrap_or_else(|| panic!("unknown era {name}")).0
}

/// The recorded name of an era.
pub fn era_name(era: MnRewardEra) -> &'static str {
    ERAS.iter().find(|e| e.0 == era).expect("every era").1
}

/// Consecutive headers from `first_height`. Like the oracle's window, the first has no `prev`
/// even above height 0, and heights below it have no ancestor: above genesis, this breaks
/// `ChainAncestry`'s contract at the window's start, which the recorded cases never walk past
/// (the oracle refuses windows too short for Core's walk).
pub struct VecChain {
    first_height: u32,
    headers: Vec<HeaderCtx>,
}

impl VecChain {
    pub fn new(first_height: u32, time_bits: impl IntoIterator<Item = (u32, u32)>) -> Self {
        let headers = time_bits
            .into_iter()
            .zip(first_height..)
            .map(|((time, bits), height)| header_ctx(height, time, bits))
            .collect();
        Self {
            first_height,
            headers,
        }
    }

    /// From a recorded `[[time, bits], ...]` array.
    pub fn from_json(first_height: u32, headers: &Value) -> Self {
        let pairs =
            headers.as_array().expect("headers").iter().map(|p| (uint32(&p[0]), uint32(&p[1])));
        Self::new(first_height, pairs)
    }

    /// From a recorded next-work case (`first_height`, `headers`).
    pub fn from_case(case: &Value) -> Self {
        Self::from_json(uint32(&case["first_height"]), &case["headers"])
    }

    pub fn tip(&self) -> BlockId {
        BlockId(u32::try_from(self.headers.len() - 1).unwrap())
    }

    pub fn headers(&self) -> &[HeaderCtx] {
        &self.headers
    }
}

impl ChainAncestry for VecChain {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.headers[id.0 as usize]
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        if height > self.headers[id.0 as usize].height {
            return None;
        }
        height.checked_sub(self.first_height).map(BlockId)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

/// KGW's masses where the event horizon is evaluated on mainnet (`PastBlocksMin`..=`PastBlocksMax`).
pub const KGW_MASSES: std::ops::RangeInclusive<u16> = 14..=4032;

/// Mainnet's block spacing, KGW's `nPowTargetSpacing`.
pub const KGW_SPACING: i64 = 150;

/// The largest actual-seconds value KGW can see: a difference of two `uint32` times.
pub const A_MAX: i64 = u32::MAX as i64;

/// The three encodings of 2^232: one canonical word and two that only `ConvertBitsToDouble` and
/// the early-mainnet difficulty tell apart from it.
pub const ENCODINGS_OF_2_232: [u32; 3] = [0x1e01_0000, 0x1f00_0100, 0x2000_0001];

/// `(A_fast, A_slow)` of a break predicate `crossed(a)` at mass `m`: the largest `a` at which it
/// breaks on the fast side (0 if none) and the smallest at which it breaks on the slow side
/// (`2^32` if none), by bisection.
///
/// This assumes the shape of KGW's predicate rather than probing it: the rate ratio `fl(150m / a)`
/// is non-increasing in `a`, above 1 below `a = 150m` and at most 1 from there, and the horizon is
/// above 1 and its inverse below, so only the fast side can break below `150m`, only the slow side
/// from it, and `crossed` is true, then false, then true along `a`.
pub fn kgw_thresholds(m: u16, crossed: impl Fn(i64) -> bool) -> (i64, i64) {
    let t = KGW_SPACING * i64::from(m);
    // the largest a in [lo, hi] where `holds` is true, `holds` true then false along a
    let last_true = |mut lo: i64, mut hi: i64, holds: &dyn Fn(i64) -> bool| {
        while lo < hi {
            let mid = lo + (hi - lo + 1) / 2;
            if holds(mid) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    };
    let a_fast = if crossed(1) {
        last_true(1, t - 1, &crossed)
    } else {
        0
    };
    let a_slow = if crossed(A_MAX) {
        last_true(t, A_MAX - 1, &|a| !crossed(a)) + 1
    } else {
        A_MAX + 1
    };
    (a_fast, a_slow)
}

/// The port's KGW break decision at mass `m` and `a` actual seconds, at mainnet's spacing.
pub fn kgw_crossed(m: u16, a: i64) -> bool {
    use dash_consensus::fp::{KgwMass, kgw_horizon_crossed};
    let mass = KgwMass::new(m.into()).expect("a KGW mass");
    let a = u32::try_from(a).expect("a difference of two uint32 times");
    kgw_horizon_crossed(mass, KGW_SPACING * i64::from(m), a)
}

/// The port's KGW thresholds at mass `m` ([`kgw_thresholds`] of [`kgw_crossed`]).
pub fn kgw_port_thresholds(m: u16) -> (i64, i64) {
    kgw_thresholds(m, |a| kgw_crossed(m, a))
}

/// A KGW `pow` table of `tests/data/kgw_pow/` (`m hex(x) hex(pow)` lines) as bit patterns,
/// index `m - 1`.
pub fn parse_kgw_pow_table(text: &str) -> Vec<u64> {
    let values: Vec<u64> = text
        .lines()
        .zip(1u32..)
        .map(|(line, m)| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(fields[0], m.to_string(), "{line}");
            u64::from_str_radix(fields[2], 16).expect("hex")
        })
        .collect();
    assert_eq!(values.len(), 4032, "a KGW pow table has 4032 masses");
    values
}

/// This process's `pow` at KGW's 4032 inputs, `pow(fl(m / 28.2), fl(-1.228))`, as bit patterns.
#[allow(clippy::disallowed_methods, reason = "the host libm is what is measured")]
pub fn host_kgw_pow() -> Vec<u64> {
    (1..=4032u32).map(|m| (f64::from(m) / 28.2).powf(-1.228).to_bits()).collect()
}

/// Which shipped glibc `pow` this process calls (a `tests/data/kgw_pow/` name), or `None` for
/// glibc's FMA4 variant, which has no fixture.
///
/// On x86_64 glibc picks the FMA variant when FMA and AVX2 are usable, else FMA4 when it is, else
/// SSE2. `GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2,-FMA` masks features (glibc 2.33 and later; 2.31
/// spells them `-AVX2_Usable,-FMA_Usable`, and this reads both), so a spelling the host's glibc
/// ignores fails the comparison instead of measuring the FMA table twice. aarch64 and riscv64
/// glibc ship the FMA table.
#[cfg(target_arch = "x86_64")]
#[allow(clippy::disallowed_methods, reason = "the environment says which variant to expect")]
pub fn expected_glibc_pow() -> Option<&'static str> {
    let tunables = std::env::var("GLIBC_TUNABLES").unwrap_or_default();
    let masked: Vec<&str> = tunables
        .split(':')
        .filter_map(|t| t.strip_prefix("glibc.cpu.hwcaps="))
        .flat_map(|list| list.split(','))
        .filter_map(|f| f.strip_prefix('-'))
        .map(|f| f.strip_suffix("_Usable").unwrap_or(f))
        .collect();
    let info = std::fs::read_to_string("/proc/cpuinfo").expect("/proc/cpuinfo");
    let flags = info.lines().find(|l| l.starts_with("flags")).expect("a flags line");
    let usable = |cpu: &str, glibc: &str| {
        flags.split_whitespace().any(|f| f == cpu) && !masked.contains(&glibc)
    };
    if usable("fma", "FMA") && usable("avx2", "AVX2") {
        Some("glibc-fma")
    } else if usable("fma4", "FMA4") {
        None
    } else {
        Some("glibc-sse2")
    }
}

/// aarch64 and riscv64 glibc ship the FMA table.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn expected_glibc_pow() -> Option<&'static str> {
    Some("glibc-fma")
}

/// A KGW boundary window (CS2 §2.4): 4033 mainnet headers ending at height 30000 (KGW runs for
/// next heights 15200..=34139) that make KGW evaluate mass `m` with `a` actual seconds, as
/// `(first_height, [(nTime, nBits)], new_time)`.
///
/// The k-th block back from the last (mass k) is `150 · (k − 1)` seconds older than the last,
/// except the m-th, which is `a` seconds older. At every other mass the rate ratio is
/// `k / (k − 1)`, inside the horizon (`kgw_port_thresholds(k).0 < 150 · (k − 1)`, which the
/// harness checks), so the loop reaches mass m and decides there. A fast-side break needs
/// `a < 150 · (m − 1)`, which makes the m-th block newer than the (m−1)-th: the timestamps are
/// deliberately not monotonic. The `nBits` vary from block to block, so that where the loop stops
/// changes the result (except at m = 4032, where the loop ends either way).
pub fn kgw_boundary_window(m: u16, a: i64) -> (u32, Vec<(u32, u32)>, u32) {
    const LAST_HEIGHT: u32 = 30_000;
    const LAST_TIME: i64 = 1_400_000_000;
    let headers = (1..=4033i64)
        .rev()
        .map(|k| {
            let age = if k == i64::from(m) {
                a
            } else {
                KGW_SPACING * (k - 1)
            };
            let mantissa = 0x10_0000 + (k * 7919 % 0x6_0000) as u32;
            (u32::try_from(LAST_TIME - age).expect("a time"), 0x1c00_0000 | mantissa)
        })
        .collect();
    (LAST_HEIGHT - 4032, headers, (LAST_TIME + KGW_SPACING) as u32)
}
