//! Coverage of the CS5b targets (`diff_check_tx_inputs`, `diff_sequence_locks`): each runs over
//! a fixed set of generated inputs, every comparison must agree, and the agreements must reach
//! what the target exists to compare (each reject reason; satisfied and unsatisfied locks of
//! both kinds), so a generator that stops reaching a rule fails here rather than passing
//! vacuously.
//!
//! Output: one line per target with the outcome counts and what was reached (Core's reject
//! strings and counts; no input).

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

#[path = "support/inputs_gen.rs"]
mod inputs_gen;
#[path = "support/spec.rs"]
mod spec;

use std::collections::BTreeMap;

use dash_oracle_sys::Pin;
use node_l1::inputs::{LocksInput, TxInputsInput};
use node_l1::{Compared, Outcome, compare_check_tx_inputs, compare_sequence_locks};
use node_sink::sink::record_for_test;

use generate::Rng;
use spec::generate;

fn label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::BothAccept => "both accept",
        Outcome::BothReject => "both reject",
        Outcome::OutOfDomain => "out of domain",
        Outcome::StrictFlag => "strict flag",
        Outcome::Discarded => "discarded",
    }
}

/// Runs `cases` generated inputs; `reached` names what an agreement reached.
fn census(
    target: &'static str,
    cases: u64,
    build: fn(&mut Rng) -> Vec<u8>,
    compare: fn(&[u8], Pin) -> Compared,
    reached: fn(&[u8]) -> Vec<String>,
    required: &[&str],
) {
    let mut outcomes: BTreeMap<&str, usize> = BTreeMap::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    for seed in 0..cases {
        let bytes = build(&mut Rng::new(seed ^ 0xc5_5b00));
        let outcome = match compare(&bytes, Pin::V24_0) {
            Ok(outcome) => outcome,
            Err(divergence) => {
                let recorded = record_for_test(target, &bytes, "v24.0", divergence.field);
                failures.push(format!("case {seed}: {divergence} ({recorded})"));
                continue;
            }
        };
        *outcomes.entry(label(outcome)).or_default() += 1;
        if outcome != Outcome::OutOfDomain {
            for name in reached(&bytes) {
                *seen.entry(name).or_default() += 1;
            }
        }
    }
    eprintln!(
        "{target}: {} | {}",
        outcomes.iter().map(|(o, n)| format!("{o} {n}")).collect::<Vec<_>>().join(", "),
        seen.iter().map(|(r, n)| format!("{r} {n}")).collect::<Vec<_>>().join(", ")
    );
    assert!(failures.is_empty(), "{target}: {}", failures.join("; "));
    assert!(outcomes.contains_key("both accept"), "{target}: nothing accepted");
    let missing: Vec<&&str> = required.iter().filter(|r| !seen.contains_key(**r)).collect();
    assert!(missing.is_empty(), "{target}: never reached {missing:?}");
}

#[test]
fn check_tx_inputs() {
    census(
        "diff_check_tx_inputs",
        3000,
        |rng| {
            if rng.chance(6) {
                generate::mutated(rng.next_u64(), inputs_gen::check_tx_inputs_bytes)
            } else {
                inputs_gen::check_tx_inputs_bytes(rng)
            }
        },
        compare_check_tx_inputs,
        |bytes| {
            let Some(input) = TxInputsInput::parse(bytes) else {
                return vec![];
            };
            let mut reached = match input.ours() {
                Some((Err((_, reason, _)), _)) => vec![reason],
                Some((Ok(()), Some(0))) => vec!["accepted, no fee".into()],
                Some((Ok(()), _)) => vec!["accepted".into()],
                None => vec![],
            };
            match input.p2sh_sigops() {
                Some(0) => reached.push("sigops compared".into()),
                Some(_) => reached.extend(["sigops compared".into(), "p2sh sigops".into()]),
                None => {}
            }
            reached
        },
        &[
            "accepted",
            "accepted, no fee",
            "bad-txns-inputs-missingorspent",
            "bad-txns-premature-spend-of-coinbase",
            "bad-txns-inputvalues-outofrange",
            "bad-txns-in-belowout",
            "bad-txns-assetunlock-fee-outofrange",
            "bad-assetunlocktx-payload",
            "sigops compared",
            "p2sh sigops",
        ],
    );
}

#[test]
fn sequence_locks() {
    census(
        "diff_sequence_locks",
        2000,
        inputs_gen::sequence_locks_bytes,
        compare_sequence_locks,
        |bytes| {
            let Some((min_height, min_time, satisfied)) =
                LocksInput::parse(bytes).and_then(|input| input.ours())
            else {
                return vec![];
            };
            let mut reached = vec![
                if satisfied {
                    "satisfied"
                } else {
                    "unsatisfied"
                }
                .to_owned(),
            ];
            if min_height >= 0 {
                reached.push("height lock".into());
            }
            if min_time >= 0 {
                reached.push("time lock".into());
            }
            if min_height < 0 && min_time < 0 {
                reached.push("no lock".into());
            }
            reached
        },
        &["satisfied", "unsatisfied", "height lock", "time lock", "no lock"],
    );
}

/// Core's own asset unlocks (dash-oracle-sys `tests/data/v24-asset-unlocks.json`, regtest
/// transactions with quorum signatures): each accepted with Core's fee, and each with any one
/// signature byte changed judged as Core judges it (the signature must still decode under one
/// scheme; whether it verifies is the special-transaction check's).
#[test]
fn cores_asset_unlocks() {
    #[derive(serde::Deserialize)]
    struct Fixture {
        asset_unlocks: Vec<Unlock>,
    }
    #[derive(serde::Deserialize)]
    struct Unlock {
        hex: String,
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dash-oracle-sys/tests/data/v24-asset-unlocks.json"
    );
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let fixture: Fixture =
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path}: {error}"));
    assert!(fixture.asset_unlocks.len() >= 4);
    let mut outcomes: BTreeMap<&str, usize> = BTreeMap::new();
    for unlock in &fixture.asset_unlocks {
        let Some(tx) = dash_core_vectors::hex::try_parse_hex(&unlock.hex) else {
            panic!("{path}: an asset unlock's hex does not parse");
        };
        let accepted = TxInputsInput::encode(200, &[], &tx);
        assert_eq!(compare_check_tx_inputs(&accepted, Pin::V24_0), Ok(Outcome::BothAccept));
        assert!(matches!(
            TxInputsInput::parse(&accepted).and_then(|input| input.ours()),
            Some((Ok(()), Some(fee))) if fee > 0
        ));
        // The payload ends the transaction, and its last 96 bytes are the signature.
        for at in tx.len() - 96..tx.len() {
            let mut mutated = tx.clone();
            mutated[at] ^= 0x5a;
            let outcome =
                compare_check_tx_inputs(&TxInputsInput::encode(200, &[], &mutated), Pin::V24_0);
            *outcomes.entry(outcome.map_or("diverged", label)).or_default() += 1;
        }
    }
    eprintln!(
        "Core's asset unlocks, signature bytes changed: {}",
        outcomes.iter().map(|(o, n)| format!("{o} {n}")).collect::<Vec<_>>().join(", ")
    );
    assert!(!outcomes.contains_key("diverged") && !outcomes.contains_key("out of domain"));
    assert!(outcomes.contains_key("both accept") && outcomes.contains_key("both reject"));
}
