//! Coverage of `diff_versionbits` (CS4): the boundary rows (`versionbits_rows.rs`) through both
//! sides, and a census of generated inputs in the fuzz format in which every comparison must
//! agree and the agreements must reach what the target exists to compare (every state, counted
//! signalling, set version bits, EHF stage signals, expired ones), so a generator that stops
//! reaching them fails here rather than passing vacuously. The dynamic threshold's decay is the
//! boundary rows' (`threshold_*`).
//!
//! Output: one line with the outcome counts and what was reached (no input).

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

#[path = "support/spec.rs"]
mod spec;
#[allow(clippy::expect_used)]
#[path = "../../dash-consensus/tests/support/versionbits_rows.rs"]
mod versionbits_rows;

use std::collections::BTreeMap;

use dash_oracle_sys::Pin;
use node_l1::Outcome;
use node_l1::versionbits::scenario as versionbits;
use node_l1::versionbits::{compare_scenario, compare_versionbits};
use node_sink::sink::record_for_test;

use generate::Rng;
use spec::generate;
use versionbits::{Answer, Query, Scenario};

#[test]
fn rows_agree() {
    let mut failures = Vec::new();
    for (label, row) in versionbits_rows::rows() {
        match compare_scenario(&row) {
            Ok(Outcome::BothAccept) => {}
            Ok(_) => failures.push(format!("{label}: not compared")),
            Err(divergence) => failures.push(format!("{label}: {divergence}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));
}

const STATES: [&str; 5] = ["defined", "started", "locked in", "active", "failed"];

/// What an agreed scenario reached.
fn reached(s: &Scenario) -> Vec<&'static str> {
    let Ok(answers) = s.ours() else {
        return vec![];
    };
    let mut out = Vec::new();
    for (q, a) in s.queries.iter().zip(&answers) {
        match a {
            Answer::State(state) => out.push(STATES[usize::from(*state)]),
            Answer::Statistics {
                threshold,
                period,
                count,
                ..
            } => {
                if threshold > period {
                    out.push("threshold above the period");
                }
                if *count > 0 {
                    out.push("signalling counted");
                }
            }
            Answer::SignalsStage(signals) if !signals.is_empty() => out.push("stage signals"),
            Answer::ComputeBlockVersion(v) if *v != 0x2000_0000 => out.push("version bits set"),
            _ => {}
        }
        // the stage (past its V20 gate, so not empty) holds fewer signals than the block
        if let (Query::SignalsStage(Some(h)), Answer::SignalsStage(stage)) = (q, a)
            && !stage.is_empty()
            && stage.len()
                < s.signals.iter().rev().find(|(at, _)| at <= h).map_or(0, |(_, set)| set.len())
        {
            out.push("expired signal");
        }
    }
    out
}

#[test]
fn census() {
    let target = "diff_versionbits";
    let mut outcomes: BTreeMap<&str, usize> = BTreeMap::new();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    for seed in 0..600u64 {
        let mut rng = Rng::new(seed ^ 0xc5_4000);
        let len = 8 + rng.below(160) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
        let outcome = match compare_versionbits(&bytes, Pin::V24_0) {
            Ok(outcome) => outcome,
            Err(divergence) => {
                let recorded = record_for_test(target, &bytes, "v24.0", divergence.field);
                failures.push(format!("case {seed}: {divergence} ({recorded})"));
                continue;
            }
        };
        *outcomes
            .entry(match outcome {
                Outcome::BothAccept => "both accept",
                Outcome::BothReject => "both reject",
                Outcome::OutOfDomain => "out of domain",
                Outcome::StrictFlag => "strict flag",
                Outcome::Discarded => "discarded",
            })
            .or_default() += 1;
        if outcome == Outcome::BothAccept
            && let Some(s) = Scenario::from_bytes(&bytes)
        {
            for r in reached(&s) {
                *seen.entry(r).or_default() += 1;
            }
        }
    }
    eprintln!(
        "{target}: {} | {}",
        outcomes.iter().map(|(o, n)| format!("{o} {n}")).collect::<Vec<_>>().join(", "),
        seen.iter().map(|(r, n)| format!("{r} {n}")).collect::<Vec<_>>().join(", ")
    );
    assert!(failures.is_empty(), "{target}: {}", failures.join("; "));
    let required = [
        "defined",
        "started",
        "locked in",
        "active",
        "failed",
        "signalling counted",
        "stage signals",
        "expired signal",
        "version bits set",
    ];
    let missing: Vec<&&str> = required.iter().filter(|r| !seen.contains_key(**r)).collect();
    assert!(missing.is_empty(), "{target}: never reached {missing:?}");
}
