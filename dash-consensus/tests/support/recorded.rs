//! The schema of `data/primitives_recorded.json`: the few CS0 §9.1 expectations that are
//! recorded (rows that decode no generated transaction), computed by Dash Core's oracle. Shared
//! by node-l1's `recorded_fixtures` (which writes and checks the file against the oracle) and the
//! dash-consensus tests that read it.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    pub provenance: String,
    pub merkle: Vec<MerkleCase>,
    pub block813851: BlockCase,
    pub check_block_tx_failure: CheckBlockCase,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MerkleCase {
    pub name: String,
    /// One byte per leaf: leaf `n` is 32 bytes of `n`.
    pub leaves: Vec<u8>,
    /// Core's internal byte order, hex.
    pub root: String,
    pub mutated: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BlockCase {
    pub hash: String,
    pub merkle_root: String,
    pub mutated: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckBlockCase {
    /// The block: a coinbase, then a transaction spending one outpoint twice.
    pub block_hex: String,
    pub result: i32,
    pub reason: String,
    pub debug_message: String,
}

/// The committed file's path.
pub fn path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dash-consensus/tests/data/primitives_recorded.json")
}

/// The committed fixtures.
pub fn load() -> Recorded {
    let text = std::fs::read_to_string(path()).expect("the recorded fixtures");
    serde_json::from_str(&text).expect("the recorded fixtures parse")
}
