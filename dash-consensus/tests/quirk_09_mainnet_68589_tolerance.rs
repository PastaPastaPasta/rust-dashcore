//! Quirk 9 (R4 §1.15): on mainnet up to height 68589, `ContextualCheckBlockHeader` does not
//! require a header's `nBits` to equal `GetNextWorkRequired`'s, only to be within 50% of it in
//! `ConvertBitsToDouble` terms: it rejects when `|n1 − n2| > n1 · 0.5`. From 68590 on it requires
//! equality.
//!
//! - The boundary is inclusive: `|n1 − n2| = n1 / 2` is accepted on both sides
//!   (`n2 = n1 / 2` and `n2 = 1.5 · n1`, exact doubles), one mantissa step beyond is rejected.
//! - The comparison is false on NaN, so two zero mantissas (+∞ − +∞) are accepted, and so is a
//!   zero header mantissa (`n1 = +∞`) against any expected target: headers that skipped
//!   `CheckProofOfWork`, as a `getblocktemplate` proposal does. A zero expected target
//!   (`n2 = +∞`) is rejected.
//!
//! The expected values are Core's: its `ConvertBitsToDouble` of each word, both pins, under its
//! literal predicate (`tests/data/oracle/quirks.json`). Core's own `ContextualCheckBlockHeader`
//! verdict, the 68589/68590 switch included, comes with CS3's L1 target (O2a); the history sample
//! holds Core's chain's evidence for the pin (`history_sample.rs`).

mod common;

use std::collections::BTreeMap;

use common::{chain_params, data, flag, hex64, pow_params, uint32};
use dash_consensus::fp::{Difficulty, within_pre_dgw_tolerance};
use dash_consensus::pow::nbits_acceptable;
use dash_num::CompactTarget;

#[test]
fn quirk_09_mainnet_68589_tolerance() {
    let main = pow_params(&chain_params("v24.0", "main"));
    // dash-params' pin
    let until = main.config().pre_dgw_tolerance_until.expect("mainnet's pin");
    assert_eq!(until, 68_589);
    let until = until as u32;
    let quirks = data("oracle/quirks.json");
    let mut verdicts: BTreeMap<&str, Vec<bool>> = BTreeMap::new();
    for case in quirks["tolerance"].as_array().unwrap() {
        let (header, expected) = (uint32(&case["header_bits"]), uint32(&case["expected_bits"]));
        let core = flag(&case["accepts"]);
        // the port's doubles are Core's
        assert_eq!(Difficulty::from_bits(header).to_bits(), hex64(&case["n1"]), "{case}");
        assert_eq!(Difficulty::from_bits(expected).to_bits(), hex64(&case["n2"]), "{case}");
        assert_eq!(within_pre_dgw_tolerance(header, expected), core, "{case}");
        let (h, e) = (CompactTarget::new(header), CompactTarget::new(expected));
        assert_eq!(nbits_acceptable(until, h, e, &main), core, "{case}");
        // from 68590, equality
        assert_eq!(nbits_acceptable(until + 1, h, e, &main), header == expected, "{case}");
        verdicts.entry(case["label"].as_str().unwrap()).or_default().push(core);
    }
    let all = |label: &str, accepts: bool| {
        let v = &verdicts[label];
        assert!(!v.is_empty() && v.iter().all(|&a| a == accepts), "{label}: {v:?}");
    };
    for side in ["half", "one and a half"] {
        all(&format!("{side}: at the boundary"), true);
        all(&format!("{side}: inside"), true);
        all(&format!("{side}: beyond"), false);
    }
    all("equal", true);
    all("zero mantissas (NaN, no PoW)", true);
    all("zero header mantissa (no PoW)", true);
    all("zero expected target", false);
    all("non-canonical, same target", true);
}

#[path = "support/checks.rs"]
mod checks;
#[path = "support/checks_recorded.rs"]
mod checks_recorded;
#[path = "support/checks_replay.rs"]
mod checks_replay;
#[path = "support/checks_rows.rs"]
mod checks_rows;

/// Core's `ContextualCheckBlockHeader` at the switch (`data/checks_recorded.json`, a synthetic
/// DGW chain): bits whose target is 1.4 times the expected one pass at 68589 and fail at 68590,
/// 1.6 times fail at both, and only the 68590 rejection carries a debug message.
#[test]
fn contextual_check_switches_at_68590() {
    use checks_replay::core_verdict;
    let invalid_header = |debug: &str| Err((4, "bad-diffbits".to_owned(), debug.to_owned()));
    let row = |height: u32, factor: &str| {
        core_verdict(&format!("quirk 9: height {height}, bits {factor} the target"))
    };
    assert_eq!(row(68_589, "exact"), Ok(()));
    assert_eq!(row(68_589, "1.4x"), Ok(()));
    assert_eq!(row(68_589, "1.6x"), invalid_header(""));
    assert_eq!(row(68_590, "exact"), Ok(()));
    let at_68590 = invalid_header("incorrect proof of work at 68590");
    assert_eq!(row(68_590, "1.4x"), at_68590);
    assert_eq!(row(68_590, "1.6x"), at_68590);
}
