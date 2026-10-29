//! Quirk 12 (R4 §1.15): the masternode payment schedule on mainnet.
//!
//! - Every increment is truncated on its own, so the "50%" stage is not `block_value / 2`.
//! - The increase steps skip period 8: nothing changes at height 296240 + 1.
//! - Block reward reallocation activates at 1374912, but the schedule only moves at the next
//!   superblock cycle, 1379128, and then every 3 cycles.
//! - Each of the 19 entries of Core's reallocation table (`vecPeriods`) is reached, at the first
//!   height of its period, and the last one stays.
//!
//! The expected values are Core's (`tests/data/oracle/quirks.json`, both pins); each check below
//! compares Core's answers with each other to show the quirk.

mod common;

use std::collections::BTreeMap;

use common::{chain_params, data, era, int, int32, payment_params};
use dash_consensus::payments::masternode_payment;

#[test]
fn quirk_12_mn_share_schedule() {
    let p = payment_params(&chain_params("v24.0", "main"));
    let quirks = data("oracle/quirks.json");
    // (label, height, block value) -> Core's payment
    let mut core = BTreeMap::new();
    for case in quirks["masternode_payment"].as_array().unwrap() {
        assert_eq!(case["chain"], "main");
        let (height, value) = (int32(&case["height"]), int(&case["block_value"]));
        let payment = int(&case["payment"]);
        let got = masternode_payment(height, value, era(case["era"].as_str().unwrap()), &p);
        assert_eq!(got, payment, "{case}");
        if case["era"] == "classic" {
            core.insert((case["label"].as_str().unwrap().to_owned(), height, value), payment);
        }
    }
    let at = |label: &str, height: i32, value: i64| core[&(label.to_owned(), height, value)];

    // separate truncation: 39/5 + 3 * (39/20) + 6 * (39/40) = 7 + 3 + 0
    assert_ne!(at("separate_truncation", 313_521, 39), 39 / 2);

    // period 8 skipped, period 9 counted
    let value = 5_000_000_007;
    assert_eq!(at("period_8_skipped", 296_241, value), at("period_8_skipped", 296_240, value));
    assert!(at("period_8_skipped", 313_521, value) > at("period_8_skipped", 313_520, value));

    // reallocation: unchanged from activation until the next cycle, then a step every 3 cycles,
    // capped after period 19
    let (value, start, step) = (361_111_111, 1_379_128, 3 * 16_616);
    let before = at("reallocation", 1_374_911, value);
    assert_eq!(at("reallocation", 1_374_912, value), before);
    assert_eq!(at("reallocation", start - 1, value), before);
    assert!(at("reallocation", start, value) > before);
    assert_eq!(at("reallocation", start + step - 1, value), at("reallocation", start, value));
    assert!(at("reallocation", start + step, value) > at("reallocation", start, value));
    assert_eq!(
        at("reallocation", start + 18 * step, value),
        at("reallocation", start + 20 * step, value)
    );

    // every table entry: Core's payment at the start of periods 0..=19 (compared above), a
    // distinct step for each of the 19 entries, then the cap
    let value = 1_000_000_000_000;
    let periods: Vec<i64> =
        (0..=19).map(|k| at("reallocation_period", start + k * step, value)).collect();
    assert!(periods[..19].windows(2).all(|w| w[0] < w[1]), "{periods:?}");
    assert_eq!(periods[19], periods[18]);
    assert_eq!(periods[0], value * 513 / 1000, "the reallocation starts at 51.3%");
}
