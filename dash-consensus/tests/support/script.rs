//! Loaders shared by the script tests: the Core-recorded fixture (parsed once per test binary)
//! and O4's transcribed `EvalScript` records, with field accessors.

#![allow(dead_code)]

use std::sync::LazyLock;

use dash_consensus::hash::{hash160, sha256};
use dash_consensus::script::opcodes::{OP_EQUAL, OP_HASH160};
use dash_consensus::script::{ScriptFlags, ScriptNum, SignatureChecker, Stack};
use dash_core_vectors::hex::{hex_str, parse_hex};
use dash_core_vectors::read_core_text;
use dash_core_vectors::univalue::UniValue;

/// `data/script_recorded.json`: what Dash Core's script code computes, recorded on both pins by
/// `contrib/node/cs1-script-recorder` (see its `provenance`).
static RECORDED: LazyLock<UniValue> = LazyLock::new(|| {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/script_recorded.json");
    UniValue::parse(&std::fs::read_to_string(path).expect("recorded fixtures")).expect("parses")
});

/// The records of one section of the recorded fixture.
pub fn recorded(section: &str) -> &'static [UniValue] {
    RECORDED.find_value(section).get_array().unwrap_or_else(|e| panic!("{section}: {e}"))
}

/// The recorded record called `name`.
pub fn record(section: &str, name: &str) -> &'static UniValue {
    recorded(section)
        .iter()
        .find(|r| r.find_value("name").get_str().ok() == Some(name))
        .unwrap_or_else(|| panic!("{section}: no record named {name:?}"))
}

/// O4's transcribed records of Core's unit test `name` (`transcribed/<name>.json`).
pub fn transcribed(name: &str) -> Vec<UniValue> {
    let doc = UniValue::parse(&read_core_text(&format!("transcribed/{name}.json"))).unwrap();
    doc.find_value("records").get_array().unwrap().to_vec()
}

pub fn name(row: &UniValue) -> &str {
    row.find_value("name").get_str().unwrap_or("")
}

pub fn bytes(row: &UniValue, key: &str) -> Vec<u8> {
    parse_hex(row.find_value(key).get_str().unwrap_or_else(|e| panic!("{key}: {e}")))
}

pub fn int(row: &UniValue, key: &str) -> i64 {
    row.find_value(key).get_i64().unwrap_or_else(|e| panic!("{key}: {e}"))
}

pub fn boolean(row: &UniValue, key: &str) -> bool {
    row.find_value(key).get_bool().unwrap_or_else(|e| panic!("{key}: {e}"))
}

pub fn flags(row: &UniValue) -> ScriptFlags {
    ScriptFlags::from_bits(int(row, "flags") as u32)
}

/// A stack written as an array of hex strings.
pub fn stack(row: &UniValue, key: &str) -> Stack {
    let items = row.find_value(key).get_array().unwrap_or_else(|e| panic!("{key}: {e}"));
    items.iter().map(|e| parse_hex(e.get_str().unwrap())).collect()
}

// Core: script/standard.cpp:212 GetScriptForDestination(ScriptHash(redeem))
/// The P2SH scriptPubKey paying to `redeem`: `OP_HASH160 <hash160(redeem)> OP_EQUAL`.
pub fn p2sh(redeem: &[u8]) -> Vec<u8> {
    let mut spk = vec![OP_HASH160, 20];
    spk.extend(hash160(redeem));
    spk.push(OP_EQUAL);
    spk
}

pub fn show(stack: &[Vec<u8>]) -> String {
    stack.iter().map(|e| format!("[{}]", hex_str(e))).collect()
}

/// `record.cpp`'s `MockChecker`: a deterministic stand-in for the transaction checker, so that
/// recorded cases reach the success side of CHECKSIG, CHECKMULTISIG, CLTV and CSV. A signature
/// check passes depending on SHA-256 of the length-prefixed (signature, public key, scriptCode),
/// so the recorded verdicts also pin the scriptCode the interpreter hands the checker.
pub struct MockChecker;

impl SignatureChecker for MockChecker {
    fn check_sig(&self, sig: &[u8], pubkey: &[u8], script_code: &[u8]) -> bool {
        let mut buf = Vec::new();
        for part in [sig, pubkey, script_code] {
            buf.extend_from_slice(&(part.len() as u64).to_le_bytes());
            buf.extend_from_slice(part);
        }
        sha256(&buf)[0] < 0xc0
    }

    fn check_lock_time(&self, lock_time: ScriptNum) -> bool {
        lock_time.value() % 3 != 0
    }

    fn check_sequence(&self, sequence: ScriptNum) -> bool {
        sequence.value() % 3 != 1
    }
}
