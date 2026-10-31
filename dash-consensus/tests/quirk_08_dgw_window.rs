//! Quirk 8 (R4 §1.15): Dark Gravity Wave's window and arithmetic, and devnet's ×10 rule.
//!
//! - The actual timespan covers the 23 intervals of the 24-block window; the target is 24
//!   spacings, so steady blocks retarget harder.
//! - The "average" is `(avg * k + t) / (k + 1)`, truncated at every step: weighted, and not
//!   the mean of the window.
//! - `avg * k + t` wraps mod 2^256 (`operator*(uint32_t)`), which nBits above powLimit reach.
//! - Devnet's "10× easier after 4 spacings" multiplies by 10 mod 2^256.
//!
//! The expected values are Core's (`tests/data/oracle/quirks.json`, both pins). Each case also
//! checks that the quirk shows in it.

mod common;

use std::collections::BTreeSet;

use common::{VecChain, chain_params, data, pow_params, uint32};
use dash_consensus::pow::get_next_work_required;
use dash_num::Arith256;
use dash_types::Numeric;

/// The window's targets, oldest first.
fn targets(chain: &VecChain) -> Vec<Arith256> {
    chain.headers().iter().rev().take(24).rev().map(|h| h.bits.expand().value).collect()
}

#[test]
fn quirk_08_dgw_window() {
    let quirks = data("oracle/quirks.json");
    let mut seen = BTreeSet::new();
    for case in quirks["next_work"].as_array().unwrap() {
        let label = case["label"].as_str().unwrap();
        let p = pow_params(&chain_params("v24.0", case["chain"].as_str().unwrap()));
        let chain = VecChain::from_case(case);
        let new_time = uint32(&case["new_time"]);
        let core = uint32(&case["bits"]);
        let got = get_next_work_required(&chain, chain.tip(), new_time, &p);
        assert_eq!(got.to_base(), core, "{label}: {case}");

        let headers = chain.headers();
        let (first, last) = (headers[headers.len() - 24], headers[headers.len() - 1]);
        match label {
            "dgw_23_intervals" => {
                // 24 equal targets 150 s apart: the window spans 23 * 150 s against 24 * 150 s
                let t = last.bits.expand().value;
                assert_eq!(i64::from(last.time - first.time), 23 * 150);
                assert_eq!(core, (t * 3450u32 / 3600u64).compact(false).to_base());
                assert_ne!(core, last.bits.to_base(), "a 24-interval span would keep the target");
            }
            "dgw_not_a_mean" => {
                let mean = targets(&chain).iter().fold(Arith256::ZERO, |s, t| s + *t) / 24u64;
                let span = (i64::from(last.time) - i64::from(first.time)).clamp(1200, 10_800);
                let from_mean =
                    (mean * u32::try_from(span).unwrap() / 3600u64).compact(false).to_base();
                assert_ne!(core, from_mean, "the window's mean would give the same result");
            }
            "dgw_wrap" => {
                let ts = targets(&chain);
                let mut avg = ts[ts.len() - 1];
                let mut wrapped = false;
                // newest first, as Core walks: avg = (avg * k + t) / (k + 1) for k = 2..=24
                for (k, t) in (2u32..).zip(ts.iter().rev().skip(1)) {
                    let (product, overflow) = avg.mul_u64(u64::from(k));
                    let sum = product + *t;
                    wrapped |= overflow || sum < product;
                    avg = sum / u64::from(k + 1);
                }
                assert!(wrapped, "no step of the window wraps");
                assert_ne!(core, p.pow_limit_bits().to_base(), "the powLimit cap hides the wrap");
            }
            "devnet_x10" => {
                assert!(new_time > last.time + 600 && new_time <= last.time + 7200);
                let (times_ten, overflow) = last.bits.expand().value.mul_u64(10);
                let expected = if times_ten > p.config().pow_limit {
                    p.pow_limit_bits()
                } else {
                    times_ten.compact(false)
                };
                assert_eq!(core, expected.to_base(), "{case}");
                if overflow && core != p.pow_limit_bits().to_base() {
                    seen.insert("devnet_x10 wrapped");
                }
            }
            other => panic!("unknown label {other}"),
        }
        seen.insert(label);
    }
    for label in
        ["dgw_23_intervals", "dgw_not_a_mean", "dgw_wrap", "devnet_x10", "devnet_x10 wrapped"]
    {
        assert!(seen.contains(label), "no {label} case: {seen:?}");
    }
}
