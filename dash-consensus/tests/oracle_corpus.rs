//! L1, recorded: Dash Core's answers from the O1 oracle on both pins (`tests/data/oracle/`,
//! see `tests/data/PROVENANCE.md`), replayed on every run. The live differential over random
//! inputs runs against the oracle itself.

mod common;

use std::collections::BTreeMap;

use common::{
    Recorded, VecChain, chain_params, era, flag, hex64, int, int32, oracle_cases, payment_params,
    pow_params, subsidy_params, uint32,
};
use dash_consensus::fp::{Difficulty, within_pre_dgw_tolerance};
use dash_consensus::payments::{
    PaymentError, masternode_payment, platform_share, split_amount_by_shares,
};
use dash_consensus::pow::{check_proof_of_work, get_next_work_required};
use dash_consensus::subsidy::{SubsidyError, block_subsidy_helper, superblock_payments_limit};
use dash_core_vectors::hex::parse_hex;
use dash_num::CompactTarget;
use dash_types::Numeric;
use serde_json::Value;

/// Parameter views per recorded chain, built once.
struct Chains<T> {
    built: BTreeMap<String, T>,
    build: fn(&Recorded) -> T,
}

impl<T> Chains<T> {
    fn new(build: fn(&Recorded) -> T) -> Self {
        Self {
            built: BTreeMap::new(),
            build,
        }
    }

    fn get(&mut self, case: &Value) -> &T {
        let name = case["chain"].as_str().expect("chain");
        let build = self.build;
        self.built.entry(name.to_owned()).or_insert_with(|| build(&chain_params("v24.0", name)))
    }
}

/// `GetNextWorkRequired` over the recorded windows, and over DGW windows on the devnets whose
/// spacing is above the timespan (`next_work_large_spacing.json`): a divisor above `u32::MAX`
/// from a spacing of 178,956,971, and from 2^29 a clamped timespan `operator*=(uint32_t)`
/// truncates (the spacings are listed in `PROVENANCE.md`). At least 80 windows of each
/// large-spacing devnet are at DGW's heights.
#[test]
fn next_work_required() {
    let mut params = Chains::new(pow_params);
    let cases = oracle_cases("next_work.json");
    let large_spacing = oracle_cases("next_work_large_spacing.json");
    assert!(cases.len() >= 1500, "{}", cases.len());
    let mut dgw: BTreeMap<String, usize> = BTreeMap::new();
    for case in &large_spacing {
        let chain = VecChain::from_case(case);
        let c = params.get(case).config();
        let next = i64::from(chain.headers()[chain.headers().len() - 1].height) + 1;
        let reaches =
            next > i64::from(c.minimum_difficulty_blocks) && next >= i64::from(c.dgw_height);
        *dgw.entry(case["chain"].as_str().unwrap().to_owned()).or_default() += usize::from(reaches);
    }
    assert_eq!(dgw.len(), 7, "{dgw:?}");
    assert!(dgw.values().all(|&n| n >= 80), "{dgw:?}");
    for case in cases.iter().chain(&large_spacing) {
        let chain = VecChain::from_case(case);
        let bits = get_next_work_required(
            &chain,
            chain.tip(),
            uint32(&case["new_time"]),
            params.get(case),
        );
        assert_eq!(bits.to_base(), uint32(&case["bits"]), "{case}");
    }
}

#[test]
fn check_proof_of_work_cases() {
    let mut params = Chains::new(pow_params);
    for case in oracle_cases("check_proof_of_work.json") {
        // internal byte order, as the oracle takes it
        let hash: [u8; 32] = parse_hex(case["hash"].as_str().unwrap()).try_into().unwrap();
        let ok = check_proof_of_work(
            &hash,
            CompactTarget::new(uint32(&case["bits"])),
            params.get(&case),
        );
        assert_eq!(ok, flag(&case["ok"]), "{case}");
    }
}

/// Core's subsidy split, and `Unreachable` where Core asserts. Each case also has Core's block
/// part at the same height with `nBits` 1 and `0x207fffff`, which the port must give too: the
/// pre-V20 formulas with two more difficulties, the fixed base where `nBits` plays no part.
#[test]
fn subsidy_helper() {
    let mut params = Chains::new(subsidy_params);
    let mut equal = 0;
    for case in oracle_cases("subsidy.json") {
        let p = params.get(&case);
        let (bits, height, v20) =
            (uint32(&case["prev_bits"]), int32(&case["prev_height"]), flag(&case["v20"]));
        let got = block_subsidy_helper(bits, height, v20, p);
        if case["block"].is_null() {
            // The oracle refuses where Core asserts, and where Core's halving loop overflows int.
            let interval = p.config().halving_interval;
            if height < (i32::MAX / interval) * interval {
                assert_eq!(got, Err(SubsidyError::Unreachable), "{case}");
            }
            continue;
        }
        let split = got.unwrap_or_else(|e| panic!("{case}: {e:?}"));
        assert_eq!((split.block, split.treasury), (int(&case["block"]), int(&case["treasury"])));
        for (alternative, key) in [(1, "block_bits_1"), (0x207f_ffff, "block_bits_207fffff")] {
            let block = block_subsidy_helper(alternative, height, v20, p).map(|s| s.block);
            assert_eq!(block, Ok(int(&case[key])), "{key}: {case}");
        }
        equal += 1;
    }
    assert!(equal > 1500, "{equal}");
}

#[test]
fn masternode_payment_cases() {
    let mut params = Chains::new(payment_params);
    for case in oracle_cases("masternode_payment.json") {
        let era = era(case["era"].as_str().unwrap());
        let got = masternode_payment(
            int32(&case["height"]),
            int(&case["block_value"]),
            era,
            params.get(&case),
        );
        assert_eq!(got, int(&case["payment"]), "{case}");
    }
}

/// Every recorded reward lies in the oracle's domain, `[i64::MIN / 375, i64::MAX / 375]`, where
/// the product cannot wrap; the wrap is tested against the binaries' semantics in `payments`.
#[test]
fn platform_share_cases() {
    for case in oracle_cases("platform_share.json") {
        let expected = (!case["share"].is_null()).then(|| int(&case["share"]));
        assert_eq!(platform_share(int(&case["reward"])).ok(), expected, "{case}");
    }
}

/// Core's `CSuperblock::GetPaymentsLimit` itself (O1b's entry point): equal everywhere, 0 off
/// superblock heights, and refused together where Core asserts. Quirk 15's `nBits` are checked
/// in `quirk_15_payments_limit_nbits.rs`.
#[test]
fn superblock_payments_limit_cases() {
    let mut params = Chains::new(subsidy_params);
    let (mut equal, mut before_v20) = (0, 0);
    for case in oracle_cases("payments_limit.json") {
        let p = params.get(&case);
        let height = int32(&case["height"]);
        let got = superblock_payments_limit(height, p);
        if case["limit"].is_null() {
            assert_eq!(got, Err(SubsidyError::Unreachable), "{case}");
            continue;
        }
        assert_eq!(got, Ok(int(&case["limit"])), "{case}");
        equal += 1;
        // `superblock` labels the input (the recorder's)
        before_v20 += usize::from(flag(&case["superblock"]) && height < p.config().v20_height);
    }
    assert!(equal > 500 && before_v20 > 10, "{equal} {before_v20}");
}

/// `fp::Difficulty::from_bits` is Core's `ConvertBitsToDouble`, bit for bit: every size byte with
/// the edge mantissas, the encodings of 2^232, and random words. The exhaustive sweep over every
/// word is `cs2-diff exhaustive bits`.
#[test]
fn convert_bits_to_double_cases() {
    let cases = oracle_cases("convert_bits_to_double.json");
    assert!(cases.len() > 2500, "{}", cases.len());
    for case in &cases {
        let bits = uint32(&case[0]);
        assert_eq!(Difficulty::from_bits(bits).to_bits(), hex64(&case[1]), "{bits:#010x}");
    }
}

/// The mainnet ≤ 68589 tolerance is Core's predicate on Core's doubles, for random pairs; quirk
/// 9's test has the boundary.
#[test]
fn tolerance_cases() {
    let (mut accepted, mut rejected) = (0, 0);
    for case in oracle_cases("tolerance.json") {
        let (header, expected, core) = (uint32(&case[0]), uint32(&case[1]), flag(&case[2]));
        assert_eq!(within_pre_dgw_tolerance(header, expected), core, "{case}");
        if core {
            accepted += 1;
        } else {
            rejected += 1;
        }
    }
    assert!(accepted > 100 && rejected > 100, "{accepted} {rejected}");
}

/// Core's `SplitAmountByShares` (v24.0): real collateral sizes, products above 2^64, awkward
/// remainders, and the zero share total where Core throws.
#[test]
fn split_amount_by_shares_cases() {
    let mut refused = 0;
    for case in oracle_cases("split_amount_by_shares.json") {
        let total = int(&case[0]);
        let shares: Vec<i64> = case[1].as_array().unwrap().iter().map(int).collect();
        let got = split_amount_by_shares(total, &shares);
        if case[2].is_null() {
            assert_eq!(got, Err(PaymentError::ZeroShareTotal), "{case}");
            refused += 1;
        } else {
            let core: Vec<i64> = case[2].as_array().unwrap().iter().map(int).collect();
            assert_eq!(got, Ok(core), "{case}");
        }
    }
    assert_eq!(refused, 1);
}
