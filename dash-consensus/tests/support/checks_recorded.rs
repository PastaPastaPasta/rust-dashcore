//! The schema of `data/checks_recorded.json`: Dash Core's verdicts on the CS3 boundary rows
//! (`checks_rows.rs`, `rows()`), recorded through the oracle. Shared by node-l1's `l1_check_rows`
//! (which writes the file and checks it against the oracle) and `checks_recorded.rs` here (which
//! replays the rows through the node). The rows' bytes are rebuilt by `rows()`, and their SHA-256
//! is recorded, so a row whose bytes change no longer matches Core's verdict without the oracle
//! noticing.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    pub provenance: String,
    pub rows: Vec<RecordedRow>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordedRow {
    pub target: String,
    pub name: String,
    /// SHA-256 of the row's input, in hex ([`input_digest`]).
    pub input_sha256: String,
    /// Core's verdict: `None` for accept.
    pub rejection: Option<RecordedRejection>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordedRejection {
    pub result: i32,
    pub reason: String,
    pub debug_message: String,
}

/// The digest recorded for a row's input.
pub fn input_digest(input: &[u8]) -> String {
    dash_consensus::hash::sha256(input).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn path() -> String {
    format!("{}/../dash-consensus/tests/data/checks_recorded.json", env!("CARGO_MANIFEST_DIR"))
}

pub fn load() -> Recorded {
    let text = std::fs::read_to_string(path()).expect("checks_recorded.json");
    serde_json::from_str(&text).expect("checks_recorded.json parses")
}
