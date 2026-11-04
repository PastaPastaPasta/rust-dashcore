//! Smoke tests in the contexts the mainnet/testnet samples cannot reach (O1 review M2,
//! M3, M4): default regtest, devnet with a pow target spacing above the timespan, the
//! regtest reallocation chain of Core's block_reward_reallocation test (all three reward
//! eras, PlatformShare), V20-era subsidies, the v24 asset-unlock txid, and the argument
//! guards.
//!
//! Expected values come from the official v24.0.0-rc.3 dashd release
//! (tests/data/{regtest,devnet-spacing}-<pin>.json, v24-asset-unlocks.json) and from what
//! Core's own unit tests compute (dash-core-vectors' transcribed records), never from the
//! oracle. See tests/data/PROVENANCE.md.

mod common;

use std::collections::BTreeMap;

use common::{
    Coverage, HeaderRange, Subsidy, Superblock, header, header_bits, header_time, hex, load_data,
    load_records, rpc_hash, transcribed,
};
use dash_oracle_sys::{Chain, ErrorKind, Oracle, Pin, RewardEra};
use serde::Deserialize;

#[derive(Deserialize)]
struct ChainSamples {
    header_ranges: Vec<HeaderRange>,
    /// devnet: getblocktemplate's bits for the block after `height - 1` at `time`
    #[serde(default)]
    templates: Vec<Template>,
    subsidy: Vec<Subsidy>,
    superblocks: Vec<Superblock>,
    deployments: BTreeMap<String, Option<i32>>,
    provenance: Provenance,
}

#[derive(Deserialize)]
struct Template {
    height: i32,
    time: u32,
    bits: u32,
}

#[derive(Deserialize)]
struct Provenance {
    dashd: String,
    chain: ChainArgs,
}

#[derive(Deserialize)]
struct ChainArgs {
    name: String,
    args: Vec<String>,
}

impl ChainSamples {
    fn chain(&self) -> Chain {
        let args: Vec<&str> = self.provenance.chain.args.iter().map(String::as_str).collect();
        Chain::new(&self.provenance.chain.name, &args)
    }
}

/// The samples the release of `pin` produced for `scenario`.
fn samples(pin: Pin, scenario: &str) -> ChainSamples {
    let s: ChainSamples = load_data(&format!("{scenario}-{}.json", pin.name()));
    assert!(s.provenance.dashd.contains(&pin.name()[1..]), "{scenario}: {}", s.provenance.dashd);
    s
}

const SCENARIOS: [&str; 2] = ["regtest", "devnet-spacing"];

/// Outside mainnet Core requires every header's nBits to equal GetNextWorkRequired(), so
/// each header dashd mined is Core's answer for the window before it; so is the bits of
/// each block template dashd built.
#[test]
fn regtest_and_devnet_headers_are_core_next_work() {
    for oracle in Oracle::all() {
        for scenario in SCENARIOS {
            let s = samples(oracle.pin(), scenario);
            let chain = s.chain();
            let mut coverage = Coverage::default();
            for range in &s.header_ranges {
                let headers = hex(&range.hex);
                let n = headers.len() / 80;
                for i in 1..n {
                    let next: [u8; 80] = headers[i * 80..(i + 1) * 80].try_into().unwrap();
                    let last = &headers[(i - 1) * 80..i * 80];
                    let height = range.first_height + i as i32;
                    let bits = oracle
                        .next_work_required(&chain, &headers[..i * 80], range.first_height, &next)
                        .unwrap_or_else(|e| {
                            panic!("{} {scenario} height {height}: {e}", oracle.pin())
                        });
                    assert_eq!(
                        bits,
                        header_bits(&next),
                        "{} {scenario} height {height}",
                        oracle.pin()
                    );
                    if header_time(&next) > header_time(last) + 2 * 60 * 60 {
                        coverage.hit("min-difficulty after 2 hours");
                    } else if bits != header_bits(last) {
                        coverage.hit("retargeted");
                    } else {
                        coverage.hit("unchanged");
                    }
                }
            }
            for t in &s.templates {
                let range = &s.header_ranges[0];
                let headers = hex(&range.hex);
                let window = &headers[..((t.height - range.first_height) * 80) as usize];
                let bits = oracle
                    .next_work_required(&chain, window, range.first_height, &header(t.time, 0))
                    .unwrap_or_else(|e| {
                        panic!("{} {scenario} template {}: {e}", oracle.pin(), t.height)
                    });
                assert_eq!(
                    bits,
                    t.bits,
                    "{} {scenario} template at {} time {}",
                    oracle.pin(),
                    t.height,
                    t.time
                );
                coverage.hit("DGW template");
            }
            if scenario == "devnet-spacing" {
                // DGW with nPowTargetSpacing = 86401, and the 2-hour reset between segments
                coverage.require(&[("min-difficulty after 2 hours", 10), ("DGW template", 10)]);
            } else {
                coverage.require(&[("unchanged", 500)]);
            }
        }
    }
}

/// O1 review M2: Core accepts any devnet -powtargetspacing >= 1. Only its BTC-era
/// arithmetic divides by the resulting DifficultyAdjustmentInterval() == 0 (a stock dashd
/// dies with SIGFPE mining block 1 of such a devnet; PROVENANCE.md), so the shim selects
/// the network and refuses exactly those heights.
#[test]
fn a_devnet_spacing_above_the_timespan_is_accepted() {
    for oracle in Oracle::all() {
        let s = samples(oracle.pin(), "devnet-spacing");
        let chain = s.chain();
        let params: serde_json::Value =
            serde_json::from_str(&oracle.chainparams_json(&chain).unwrap()).unwrap();
        assert_eq!(params["consensus"]["nPowTargetSpacing"], 86401, "{}", oracle.pin());

        // The same devnet without -minimumdifficultyblocks reaches the BTC algorithm.
        let args: Vec<&str> = s
            .provenance
            .chain
            .args
            .iter()
            .map(String::as_str)
            .filter(|a| !a.starts_with("-minimumdifficultyblocks"))
            .collect();
        let btc = Chain::new("devnet", &args);
        let h = header(1_700_000_000, 0x207fffff);
        let e = oracle.next_work_required(&btc, &h, 10, &h).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Argument, "{}: {e}", oracle.pin());
        assert!(e.message.contains("DifficultyAdjustmentInterval"), "{e}");
    }
}

#[test]
fn regtest_and_devnet_subsidies_and_budgets_match_dashd() {
    for oracle in Oracle::all() {
        for scenario in SCENARIOS {
            let s = samples(oracle.pin(), scenario);
            let chain = s.chain();
            let v20 = s.deployments["v20"].unwrap_or(i32::MAX);
            let mut coverage = Coverage::default();
            for x in &s.subsidy {
                let got = oracle
                    .block_subsidy_inner(&chain, x.prev_bits, x.height - 1, x.height >= v20)
                    .unwrap();
                assert_eq!(got, x.subsidy, "{} {scenario} height {}", oracle.pin(), x.height);
                coverage.hit(if x.height >= v20 {
                    "v20"
                } else {
                    "pre-v20"
                });
                // devnet: prev heights below -highsubsidyblocks (500) pay -highsubsidyfactor times more
                coverage.hit(if x.height - 1 < 500 {
                    "high-subsidy window"
                } else {
                    "after it"
                });
            }
            for sb in &s.superblocks {
                let got = oracle.superblock_payments_limit(&chain, sb.height).unwrap();
                assert_eq!(got, sb.budget, "{} {scenario} superblock {}", oracle.pin(), sb.height);
                coverage.hit(if sb.budget == 0 {
                    "not a superblock"
                } else {
                    "superblock"
                });
            }
            coverage.require(&[("superblock", 10), ("not a superblock", 1)]);
            if scenario == "regtest" {
                coverage.require(&[("v20", 3), ("pre-v20", 3)]);
            } else {
                coverage.require(&[("high-subsidy window", 3), ("after it", 3)]);
            }
        }
    }
}

#[derive(Deserialize)]
struct RewardCheckpoint {
    label: String,
    height: i32,
    prev_bits: u32,
    era: i32,
    v20_active: bool,
    block_subsidy: i64,
    superblock_subsidy: i64,
    masternode_payment: i64,
    template_masternode_payments: Vec<i64>,
}

/// O1 review M3: the CreditPool and EvoReward eras, V20 subsidies and successful
/// PlatformShare() calls, on the chain of Core's block_reward_reallocation test.
#[test]
fn reward_eras_match_core_block_reward_reallocation_test() {
    let chain = Chain::new(
        "regtest",
        &[
            "-dip3params=30:50",
            "-testactivationheight=brr@1000",
            "-testactivationheight=v20@1200",
            "-testactivationheight=mn_rr@2200",
        ],
    );
    let records: Vec<RewardCheckpoint> = transcribed("block_reward_reallocation_tests.json");
    for oracle in Oracle::all() {
        let mut coverage = Coverage::default();
        for r in &records {
            let case = format!("{} {} (height {})", oracle.pin(), r.label, r.height);
            assert_eq!(
                oracle.block_subsidy_inner(&chain, r.prev_bits, r.height, r.v20_active).unwrap(),
                r.block_subsidy,
                "{case}"
            );
            assert_eq!(
                oracle
                    .superblock_subsidy_inner(&chain, r.prev_bits, r.height, r.v20_active)
                    .unwrap(),
                r.superblock_subsidy,
                "{case}"
            );
            let era =
                [RewardEra::Classic, RewardEra::CreditPool, RewardEra::EvoReward][r.era as usize];
            let payment = oracle.mn_payment(&chain, r.height, r.block_subsidy, era).unwrap();
            assert_eq!(payment, r.masternode_payment, "{case}");
            if era == RewardEra::EvoReward {
                // the template splits the payment into the platform (credit pool) output and the rest
                let share = oracle.platform_share(payment).unwrap();
                assert_eq!(share, r.template_masternode_payments[0], "{case}");
                assert_eq!(payment - share, r.template_masternode_payments[1], "{case}");
                coverage.hit("platform share");
            } else {
                assert_eq!(r.template_masternode_payments, [payment], "{case}");
            }
            coverage.hit(format!("era {:?}", era));
            if r.v20_active {
                coverage.hit("v20");
            }
        }
        coverage.require(&[
            ("era Classic", 5),
            ("era CreditPool", 20),
            ("era EvoReward", 5),
            ("platform share", 5),
            ("v20", 20),
        ]);
    }
}

#[derive(Deserialize)]
struct SubsidyPoint {
    prev_bits: u32,
    prev_height: i32,
    v20_active: bool,
    subsidy: i64,
}

/// subsidy_tests.cpp's mainnet points, including V20 ones.
#[test]
fn mainnet_subsidy_matches_core_subsidy_test() {
    let points: Vec<SubsidyPoint> = transcribed("subsidy_tests.json");
    assert!(points.iter().filter(|p| p.v20_active).count() >= 3);
    for oracle in Oracle::all() {
        for p in &points {
            let got = oracle
                .block_subsidy_inner(&Chain::main(), p.prev_bits, p.prev_height, p.v20_active)
                .unwrap();
            assert_eq!(
                got,
                p.subsidy,
                "{} prev_height {} v20 {}",
                oracle.pin(),
                p.prev_height,
                p.v20_active
            );
        }
    }
}

/// A pow_tests.json record: the mainnet header list, then one per next-work result.
#[derive(Deserialize)]
struct PowRecord {
    kind: String,
    last_height: Option<i32>,
    headers_time_bits: Option<Vec<(u32, u32)>>,
    chain: Option<String>,
    new_block_time: Option<u32>,
    core_bits: Option<u32>,
}

/// pow_tests.cpp get_next_work: DGW on mainnet and the default devnet's slow-block rules.
#[test]
fn next_work_matches_core_pow_test() {
    let records: Vec<PowRecord> = transcribed("pow_tests.json");
    let (headers, next_work) = records.split_first().unwrap();
    assert_eq!(headers.kind, "mainnet_headers");
    let time_bits = headers.headers_time_bits.as_ref().unwrap();
    let window: Vec<u8> = time_bits.iter().flat_map(|&(t, b)| header(t, b)).collect();
    let first = headers.last_height.unwrap() + 1 - time_bits.len() as i32;
    let devnet_records = next_work.iter().filter(|r| r.chain.as_deref() != Some("main")).count();
    assert!(
        devnet_records >= 4,
        "pow_tests.json has {devnet_records} devnet cases, need 4 per pin"
    );
    let mut devnet = 0;
    for oracle in Oracle::all() {
        for r in next_work {
            assert_eq!(r.kind, "next_work");
            let (chain, time) = (r.chain.as_deref().unwrap(), r.new_block_time.unwrap());
            let c = if chain == "main" {
                Chain::main()
            } else {
                devnet += 1;
                Chain::new("devnet", &[])
            };
            let got = oracle.next_work_required(&c, &window, first, &header(time, 0)).unwrap();
            assert_eq!(got, r.core_bits.unwrap(), "{} {chain} time {time}", oracle.pin());
        }
    }
    assert_eq!(devnet, devnet_records * Pin::ALL.len(), "devnet cases run once per pin");
}

#[derive(Deserialize)]
struct AssetUnlocks {
    asset_unlocks: Vec<AssetUnlock>,
}

#[derive(Deserialize)]
struct AssetUnlock {
    hex: String,
    txid: String,
    instance_hash: String,
    payload_version: u8,
}

/// O1 review M3: v24 gives a version-2 asset unlock a txid without its quorum signature
/// (stable across re-signs) and keeps the full-serialization hash as GetInstanceHash().
#[test]
fn v24_asset_unlock_txid_differs_from_its_instance_hash() {
    let data: AssetUnlocks = load_data("v24-asset-unlocks.json");
    let mut stable = 0;
    for u in &data.asset_unlocks {
        let tx = hex(&u.hex);
        let (txid, instance) = (rpc_hash(&u.txid), rpc_hash(&u.instance_hash));
        assert_eq!(txid != instance, u.payload_version >= 2);
        stable += usize::from(txid != instance);
        let v24 = Oracle::new(Pin::V24_0).tx_decode(&tx).unwrap();
        assert_eq!((v24.txid, v24.instance_hash), (txid, instance), "v24.0 {}", u.txid);
    }
    assert!(stable >= 2, "no version-2 asset unlocks");
}

/// O1 review M4: arguments are refused only where Core asserts or overflows.
#[test]
fn guards_refuse_only_core_assert_and_ub_arguments() {
    for oracle in Oracle::all() {
        let main = Chain::main();
        // Classic height 1 only divides block_value (by 5). The value is the review's
        // direct call of Core's GetMasternodePayment() at the pin.
        assert_eq!(
            oracle.mn_payment(&main, 1, i64::MAX, RewardEra::Classic).unwrap(),
            1_844_674_407_370_955_161,
            "{}",
            oracle.pin()
        );
        // After mainnet's reallocation start Core multiplies block_value: by 3 outside the
        // Classic era, by the period's per-mille share in it.
        for era in [RewardEra::CreditPool, RewardEra::EvoReward, RewardEra::Classic] {
            let e = oracle.mn_payment(&main, 2_000_000, i64::MAX, era).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Argument, "{} {era:?}: {e}", oracle.pin());
            assert!(oracle.mn_payment(&main, 2_000_000, i64::MAX / 600, era).is_ok());
        }
        // PlatformShare() refuses only where reward * 375 overflows or the share leaves MoneyRange.
        assert_eq!(
            oracle.platform_share(i64::MAX / 375 + 1).unwrap_err().kind,
            ErrorKind::Argument
        );
        assert!(oracle.platform_share(21_000_000 * 100_000_000).is_ok());

        // A KGW window shorter than its lookback is missing context, not an argument
        // Core asserts on (the deliberate domain restriction, see oracle.h).
        let window: Vec<u8> =
            (0..30).flat_map(|i| header(1_400_000_000 + 150 * i, 0x1c100000)).collect();
        let e = oracle
            .next_work_required(&main, &window, 19_971, &header(1_400_004_500, 0))
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Context, "{}: {e}", oracle.pin());
    }
}

#[derive(Deserialize)]
struct SplitRecord {
    total: i64,
    weights: Vec<i64>,
    amounts: Vec<i64>,
}

/// SplitAmountByShares() (O1b, DEC-35 D7) on the cases of Core's evo_sharedmn_tests
/// (tests/data/core-tests/evo_sharedmn_tests.json).
#[test]
fn split_amount_by_shares_matches_core_evo_sharedmn_test() {
    let records: Vec<SplitRecord> = load_records("core-tests/evo_sharedmn_tests.json");
    assert_eq!(records.len(), 2);
    let v24 = Oracle::new(Pin::V24_0);
    for r in &records {
        assert_eq!(
            v24.split_amount_by_shares(r.total, &r.weights).unwrap(),
            r.amounts,
            "total {}",
            r.total
        );
    }
    // the weights' int64 sum overflowing is undefined in Core: refused
    assert_eq!(
        v24.split_amount_by_shares(100, &[i64::MAX, 1]).unwrap_err().kind,
        ErrorKind::Argument
    );
}
