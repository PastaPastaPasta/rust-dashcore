//! The fast sample of the history sweep (CS2 §6.4), run on every `cargo test`: mainnet and
//! testnet headers from a reference node's chain, over the ranges where the rules change, with
//! Core's `GetNextWorkRequired` and subsidy for each checked height (`tests/data/history/`,
//! recorded by `contrib/node/cs2-diff`; `tests/data/PROVENANCE.md`). The full sweep over every
//! header is `cs2-diff sweep`.
//!
//! Per checked height `h`, with the block before it as the chain tip:
//! - `CheckProofOfWork` passes for the header (Core accepted it);
//! - the port's `GetNextWorkRequired` is Core's, KGW included (its ranges carry KGW's whole
//!   4032-block lookback); where Core requires equality the header carries it, and on mainnet
//!   up to 68589 the tolerance rule accepts the header, as Core did, whether or not its `nBits`
//!   equal Core's;
//! - the subsidy is Core's, from the formulas before V20 and the fixed base after.
//!
//! Mainnet's record also holds Core's evidence for the tolerance pin: the headers up to 68589
//! whose `nBits` differ from Core's `GetNextWorkRequired`, which Core accepted. The highest is
//! 68588, so the pin is at least that. The bound from above, that Core's
//! `ContextualCheckBlockHeader` rejects at 68590 bits it accepts at 68589, is
//! `contextual_check_switches_at_68590` in the quirk 9 test file (CS3's recorded verdicts).

mod common;

use common::{VecChain, chain_params, data, int, pow_params, subsidy_params, uint32};
use dash_consensus::pow::{check_proof_of_work, get_next_work_required, nbits_acceptable};
use dash_consensus::subsidy::{block_subsidy, block_subsidy_helper};
use dash_consensus::{BlockId, ChainAncestry};
use dash_core_vectors::hex::parse_hex;
use dash_num::CompactTarget;
use dash_types::Numeric;
use serde_json::Value;

/// What a sample checked, per kind.
#[derive(Default, Debug)]
struct Counts {
    pow: usize,
    next_work_exact: usize,
    tolerance_equal: usize,
    tolerance_differ: usize,
    kgw: usize,
    subsidy_formula: usize,
    subsidy_fixed: usize,
}

fn check_sample(file: &str) -> (Value, Counts) {
    let sample = data(&format!("history/{file}"));
    let network = sample["network"].as_str().unwrap();
    let cp = chain_params("v24.0", network);
    let (pow, sub) = (pow_params(&cp), subsidy_params(&cp));
    let c = *pow.config();
    let v20_height = sub.config().v20_height;
    let mut n = Counts::default();
    for range in sample["ranges"].as_array().unwrap() {
        let first = uint32(&range["first_height"]);
        let headers = range["headers"].as_array().unwrap();
        let chain = VecChain::from_json(first, &range["headers"]);
        for (core, h) in
            range["core"].as_array().unwrap().iter().zip(uint32(&range["check_from"])..)
        {
            let header = chain.header(BlockId(h - first));
            let entry = &headers[(h - first) as usize];
            if let Some(hash) = entry[2].as_str() {
                let hash: [u8; 32] = parse_hex(hash).try_into().unwrap();
                assert!(check_proof_of_work(&hash, header.bits, &pow), "{network} {h}");
                n.pow += 1;
            }

            let tip = BlockId(h - 1 - first);
            let expected = get_next_work_required(&chain, tip, header.time, &pow);
            assert_eq!(expected.to_base(), uint32(&core[0]), "{network} {h}");
            // Core accepted every header of its chain
            assert!(nbits_acceptable(h, header.bits, expected, &pow), "{network} {h}");
            if c.tolerance_applies(h) {
                if header.bits == expected {
                    n.tolerance_equal += 1;
                } else {
                    n.tolerance_differ += 1;
                }
            } else {
                // Core requires equality here
                assert_eq!(header.bits, expected, "{network} {h}");
                n.next_work_exact += 1;
            }
            n.kgw += usize::from((c.kgw_height..c.dgw_height).contains(&(h as i32)));

            let prev = chain.header(tip);
            let (block, treasury) = (int(&core[1]), int(&core[2]));
            let v20 = h as i32 >= v20_height;
            assert_eq!(block_subsidy(Some(&prev), v20, &sub), Ok(block), "{network} {h}");
            let split = block_subsidy_helper(prev.bits.to_base(), h as i32 - 1, v20, &sub);
            assert_eq!(split.map(|s| s.treasury), Ok(treasury), "{network} {h}");
            if v20 {
                n.subsidy_fixed += 1;
            } else {
                n.subsidy_formula += 1;
            }
        }
    }
    (sample, n)
}

/// A header the sample holds, with Core's `GetNextWorkRequired` for it.
fn sampled(sample: &Value, height: u32) -> Option<(CompactTarget, u32)> {
    sample["ranges"].as_array().unwrap().iter().find_map(|range| {
        let (first, from) = (uint32(&range["first_height"]), uint32(&range["check_from"]));
        let core = range["core"].as_array().unwrap().get(height.checked_sub(from)? as usize)?;
        let entry = &range["headers"].as_array().unwrap()[(height - first) as usize];
        Some((CompactTarget::new(uint32(&entry[1])), uint32(&core[0])))
    })
}

#[test]
fn mainnet_history_sample() {
    let (sample, n) = check_sample("mainnet.json");
    // every rule and boundary of the ranges was reached
    assert!(n.pow > 750 && n.next_work_exact > 50 && n.kgw >= 40, "{n:?}");
    assert!(n.tolerance_equal > 500 && n.tolerance_differ > 20, "{n:?}");
    assert!(n.subsidy_fixed >= 50 && n.subsidy_formula > 700, "{n:?}");

    // Core's evidence for the tolerance pin, from below: Core accepted a header at the highest
    // such height although its nBits differ from GetNextWorkRequired
    let tolerance = &sample["tolerance"];
    let highest = uint32(&tolerance["highest_differing"]);
    assert!(int(&tolerance["differing"]) > 30_000, "{tolerance}");
    let pin = pow_params(&chain_params("v24.0", "main")).config().pre_dgw_tolerance_until;
    assert!(pin.is_some_and(|until| i64::from(highest) <= i64::from(until)), "{tolerance}");
    assert_eq!(highest, 68_588, "{tolerance}");
    let (bits, core) = sampled(&sample, highest).expect("the sample holds the highest one");
    assert_ne!(bits.to_base(), core, "{tolerance}");
}

#[test]
fn testnet_history_sample() {
    let (sample, n) = check_sample("testnet.json");
    assert!(sample["tolerance"].is_null());
    assert!(n.pow > 1200 && n.next_work_exact > 1200, "{n:?}");
    assert_eq!((n.kgw, n.tolerance_equal, n.tolerance_differ), (0, 0, 0), "{n:?}");
    assert!(n.subsidy_fixed >= 50 && n.subsidy_formula > 1000, "{n:?}");
}
