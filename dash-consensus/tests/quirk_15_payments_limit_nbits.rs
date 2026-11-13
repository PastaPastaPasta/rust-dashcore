//! Quirk 15 (R4 §1.15): `CSuperblock::GetPaymentsLimit` is a subsidy formula, not a difficulty
//! one. Every superblock's limit is `GetSuperblockSubsidyInner(nBits, h - 1, h >= V20Height) *
//! nSuperblockCycle` with an artificial `nBits`: 1 (the highest difficulty, so the lowest
//! subsidy) where `fPowAllowMinDifficultyBlocks` is false, as on mainnet, and the powLimit compact
//! where it is true. The real difficulty never enters.
//!
//! Core's evidence (`tests/data/oracle/payments_limit.json`, both pins): Core's own
//! `GetPaymentsLimit(h)`, recorded through O1b's entry point, and Core's treasury part for each
//! candidate `nBits`. Before V20 the candidates give different treasuries, and Core's limit must
//! be the one for the `nBits` the crate derives ([`SubsidyConfig::payments_limit_bits`]); the
//! port's treasury for each candidate is Core's. The crate's own limit is compared with Core's
//! in `oracle_corpus.rs`.
//!
//! [`SubsidyConfig::payments_limit_bits`]: dash_consensus::subsidy::SubsidyConfig

mod common;

use common::{chain_params, flag, int, int32, oracle_cases, pow_params, subsidy_params};
use dash_consensus::subsidy::superblock_subsidy_inner;
use dash_types::Numeric;

#[test]
fn quirk_15_payments_limit_nbits() {
    // discriminating cases, by the nBits the crate picked: [the powLimit compact, 1]
    let mut discriminating = [0; 2];
    for case in oracle_cases("payments_limit.json") {
        if !flag(&case["superblock"]) || case["limit"].is_null() {
            continue;
        }
        let cp = chain_params("v24.0", case["chain"].as_str().unwrap());
        let sub = subsidy_params(&cp);
        let config = *sub.config();
        let pow_limit_bits = pow_params(&cp).pow_limit_bits().to_base();
        let (one, pow_limit) =
            (int(&case["treasury_bits_1"]), int(&case["treasury_bits_pow_limit"]));
        let h = int32(&case["height"]);
        let v20 = h >= config.v20_height;
        for (bits, core) in [(1, one), (pow_limit_bits, pow_limit)] {
            let port = superblock_subsidy_inner(bits, h - 1, v20, &sub);
            assert_eq!(port, Ok(core), "nBits {bits:#x}: {case}");
        }
        let picks_one = config.payments_limit_bits == 1;
        if !picks_one {
            assert_eq!(config.payments_limit_bits, pow_limit_bits, "{case}");
        }
        let picked = if picks_one {
            one
        } else {
            pow_limit
        };
        let cycle = i64::from(config.superblocks.cycle());
        assert_eq!(picked.wrapping_mul(cycle), int(&case["limit"]), "{case}");
        if one != pow_limit {
            discriminating[usize::from(picks_one)] += 1;
        }
    }
    // Core's limit told the two candidates apart on both kinds of network
    assert!(discriminating.iter().all(|&n| n >= 4), "{discriminating:?}");
}
