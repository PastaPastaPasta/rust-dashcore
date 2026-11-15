//! O2b-4 4e1 DESIGN §5.3, T8: each required cell of the census overlay is credited by its seed
//! on its profile in Core's session alone (`Mode::CoreOnly`), as `examples/chain_reach.rs` and
//! `l3-census.py o2b4-check --reach` check it, so node-gate checks it without Python. A seed
//! credits only when its run ends done, and one that does not is itself a failure (OD-L3-171 (2)).

#![cfg(feature = "oracle")]

use std::collections::BTreeMap;
use std::path::Path;

use dash_oracle_sys::Pin;
use node_l1::chain::Runner;
use node_l1::chain::census::Totals;
use node_l1::chain::core::Schedule;
use node_l1::chain::intent::{Program, chain_encodings};
use node_l1::chain::run::End;
use serde_json::Value;

const OVERLAY: &str = include_str!("../../contrib/node/l3-census/o2b4-overlay-v24.0.0.json");

/// The profiles with a chain target on this tree: (profile, target, schedule).
const PROFILES: &[(&str, &str, Schedule)] = &[
    ("S0", "diff_chain_s0", Schedule::S0),
    ("S1", "diff_chain_par", Schedule::S1),
    ("S2", "diff_chain_av", Schedule::S2),
    ("S0p", "diff_chain_prodflush", Schedule::S0p),
];

/// The overlay's sections: (section, the entry's key field, the census map it is counted in).
const SECTIONS: &[(&str, &str, &str)] = &[
    ("rows", "key", "inventory"),
    ("script_errors", "error", "inventory"),
    ("named_rows", "row", "rows"),
    ("ops", "op", "ops"),
    ("rejects", "reject", "rejects"),
];

#[test]
fn every_required_cell_is_reached_by_its_seed() {
    let overlay: Value = serde_json::from_str(OVERLAY).expect("the overlay");
    let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chain_seeds");
    let mut missing = Vec::new();
    for &(profile, target, schedule) in PROFILES {
        let mut runner =
            Runner::new(Pin::V24_0, schedule).unwrap_or_else(|e| panic!("start: {}", e.0));
        // The target's encoding: a `chain-v1` runner would drop the seeds' spork intents.
        runner.encoding = chain_encodings()
            .into_iter()
            .find(|&(n, _)| n == target)
            .unwrap_or_else(|| panic!("{target} is not in node-l1/chain-encodings.toml"))
            .1;
        let mut censuses: BTreeMap<String, Value> = BTreeMap::new();
        for &(section, field, map) in SECTIONS {
            for entry in overlay[section].as_array().expect("a section") {
                let cell = &entry[profile];
                // A leg-B row's evidence is the run's census, not a seed (OD-L3-165).
                if cell["status"] != "required" || cell["basis"] == "run" {
                    continue;
                }
                let name = entry[field].as_str().expect("a key");
                let key = if section == "script_errors" {
                    format!("script:{name}")
                } else {
                    name.into()
                };
                let Some(seed) = cell["seed"].as_str() else {
                    missing.push(format!("{key} on {profile}: no seed"));
                    continue;
                };
                if !censuses.contains_key(seed) {
                    let text = std::fs::read_to_string(seeds.join(seed))
                        .unwrap_or_else(|e| panic!("{seed}: {e}"));
                    let program = Program::parse(&text).unwrap_or_else(|e| panic!("{seed}: {e}"));
                    let run = runner
                        .run(&program)
                        .unwrap_or_else(|e| panic!("{seed} on {profile}: shim fault {}", e.0));
                    let census = if run.end == End::Done {
                        let mut totals = Totals::default();
                        totals.input(run.census, None);
                        totals.to_json()
                    } else {
                        missing.push(format!("{seed} on {profile}: ends {:?}, not done", run.end));
                        Value::Null
                    };
                    censuses.insert(seed.into(), census);
                }
                let census = &censuses[seed];
                if !census[map][&key].as_u64().is_some_and(|n| n >= 1) {
                    missing.push(format!("{key} on {profile}: {seed} did not reach it"));
                }
            }
        }
    }
    assert!(missing.is_empty(), "{} cells not reached:\n{}", missing.len(), missing.join("\n"));
}
