//! Quirk 32 (R4 §1.15): `MinBIP9WarningHeight` is a consensus floor for deployment states, and
//! the dynamic threshold's `nAttempt * nAttempt * Period` is `int` arithmetic that overflows
//! (`src/versionbits.cpp:60-70,250-270`).
//!
//! - Floor: on devnet (`MinBIP9WarningHeight` 2018, v24's window 120) every block signals v24
//!   and its EHF signal is mined at 100, yet v24 is defined at every boundary below the floor
//!   and starts only at the first boundary at or above it (2039). Testnet's floor (1150516,
//!   window 100) the same, on a window of headers that starts below it.
//! - Overflow: a period of 1 with threshold 1 and falloff 1 gives `max(1, 1 - n*n/100)`, which is
//!   1 until `n * n` wraps at attempt 46341; the threshold then jumps above the period, so the
//!   deployment can never lock in.
//!
//! The answers are Core's (`tests/data/oracle/versionbits.json`, recorded through the oracle by
//! node-l1's `versionbits_record`); the node reproduces each of them first.

#[path = "support/versionbits.rs"]
mod versionbits;
#[path = "support/versionbits_json.rs"]
mod versionbits_json;

mod common;

use dash_params::DeploymentPos;
use versionbits::{Answer, Query, Scenario};
use versionbits_json::recorded_case;

/// Core's v24 state after each queried block of a row.
fn v24_states(scenario: &Scenario, core: &[Answer]) -> Vec<(u32, &'static str)> {
    scenario
        .queries
        .iter()
        .zip(core)
        .filter_map(|(q, a)| match (q, a) {
            (Query::State(DeploymentPos::V24, Some(h)), Answer::State(s)) => {
                Some((*h, versionbits_json::STATE_NAMES[usize::from(*s)]))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn quirk_32_bip9_warning_floor_and_threshold() {
    let cases = common::oracle_cases("versionbits.json");
    let case = |label: &str| recorded_case(&cases, label);

    // the floor: devnet
    let (devnet, core) = case("quirk32_floor_devnet");
    let states = v24_states(&devnet, &core);
    for &(h, state) in &states {
        // the state of block h + 1, decided at the last boundary at or below h
        let boundary = (h + 1) / 120 * 120;
        if boundary <= 2018 {
            assert_eq!(state, "defined", "devnet after {h}: below the floor");
        }
    }
    assert!(states.contains(&(2039, "started")), "devnet starts at the first boundary above 2018");
    assert!(states.iter().any(|&(_, s)| s == "active"));

    // the floor: testnet, from a window that starts below it
    let (testnet, core) = case("quirk32_floor_testnet");
    let states = v24_states(&testnet, &core);
    for &(h, state) in &states {
        if (h + 1) / 100 * 100 <= 1_150_516 {
            assert_eq!(state, "defined", "testnet after {h}: below the floor");
        }
    }
    assert!(states.iter().any(|&(h, s)| s == "started" && h >= 1_150_516));

    // the overflow
    let (overflow, core) = case("quirk32_threshold_overflow");
    let thresholds: Vec<(u32, i32, i32)> = overflow
        .queries
        .iter()
        .zip(&core)
        .filter_map(|(q, a)| match (q, a) {
            (
                Query::Statistics(_, Some(h)),
                Answer::Statistics {
                    threshold,
                    period,
                    ..
                },
            ) => Some((*h, *threshold, *period)),
            _ => None,
        })
        .collect();
    for &(h, threshold, period) in &thresholds {
        assert_eq!(period, 1);
        // Core's nAttempt for the block at h: its period's distance from height 0
        let attempt = i64::from(h);
        if attempt * attempt <= i64::from(i32::MAX) {
            assert_eq!(threshold, 1, "attempt {attempt}: no overflow yet");
        } else {
            let wrapped = (attempt * attempt) as i32;
            assert_eq!(threshold, 1 - wrapped / 100, "attempt {attempt}: the product wrapped");
            assert!(threshold > period, "the threshold is above the period");
        }
    }
    assert!(thresholds.iter().any(|&(_, t, _)| t > 1), "the overflow is reached");
}
