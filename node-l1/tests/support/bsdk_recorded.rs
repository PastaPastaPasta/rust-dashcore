//! The schema of `data/bsdk_decode_recorded.json`: Dash Core's verdicts on the BS4b rows
//! (`bsdk_rows.rs`, `rows()`), recorded through the oracle. Shared by `l1_bsdk_decode` (which
//! writes the file and checks it against the oracle) and `bsdk_recorded` (which replays the rows
//! through base-sdk without the oracle). The rows' bytes are rebuilt by `rows()` and their
//! SHA-256 is recorded, so a row whose bytes change no longer matches Core's verdict without the
//! oracle noticing.

#![allow(dead_code)]

use crate::bsdk_decode::{CoreDecode, CorePayload};
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
    pub tag: String,
    /// SHA-256 of the row's input, in hex ([`input_digest`]).
    pub input_sha256: String,
    /// What the judge made of Core's verdict and base-sdk's: `accept` (both decode, under at
    /// least one BLS flag for a payload) or `reject`.
    pub agreement: String,
    /// Core's verdict: one entry for a transaction, block or header; two for a payload, under
    /// the basic and then the legacy BLS scheme flag. `None` is a rejection.
    pub core: Vec<Option<RecordedCore>>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct RecordedCore {
    /// Bytes read (transaction, block, header).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub consumed: Option<usize>,
    /// Whether every byte was read (payload).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub consumed_all: Option<bool>,
    /// The object serialized again, in hex.
    pub reencoded: String,
}

impl RecordedCore {
    pub fn of_object(d: &CoreDecode) -> Self {
        Self {
            consumed: Some(d.consumed),
            consumed_all: None,
            reencoded: to_hex(&d.reencoded),
        }
    }

    pub fn of_payload(d: &CorePayload) -> Self {
        Self {
            consumed: None,
            consumed_all: Some(d.consumed_all),
            reencoded: to_hex(&d.reencoded),
        }
    }

    pub fn object(&self) -> CoreDecode {
        CoreDecode {
            consumed: self.consumed.expect("an object row records its length"),
            reencoded: from_hex(&self.reencoded),
        }
    }

    pub fn payload(&self) -> CorePayload {
        CorePayload {
            consumed_all: self.consumed_all.expect("a payload row records consumed_all"),
            reencoded: from_hex(&self.reencoded),
        }
    }
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("recorded hex"))
        .collect()
}

/// The digest recorded for a row's input.
pub fn input_digest(input: &[u8]) -> String {
    to_hex(&dash_consensus::hash::sha256(input))
}

pub fn path() -> String {
    format!("{}/tests/data/bsdk_decode_recorded.json", env!("CARGO_MANIFEST_DIR"))
}

pub fn load() -> Recorded {
    let text = std::fs::read_to_string(path()).expect("bsdk_decode_recorded.json");
    serde_json::from_str(&text).expect("bsdk_decode_recorded.json parses")
}
