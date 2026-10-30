//! Checks `getcanonicalmnlist` against `oracle_mnlist_canonical` (node-program O8 spec §13.4,
//! DEC-177).
//!
//! Reads samples as JSON lines on stdin, written by `contrib/node/statedigest/extract.py` from a
//! frozen copy of a StateDigest-patched node: `height`, `chain`, `chain_args` (the chainparams
//! args the node ran with, e.g. `-dip3params=…`), and the RPC's `hex` and `hash`. For each pin,
//! Core's strict canonical reader must accept `hex` and write the same bytes back, and its
//! `CanonicalMNListHash` must be the RPC's `hash` (RPC byte order) and sha256d of `hex`. Prints
//! one JSON line per sample and pin: `{"height", "pin", "result": "match" | "mismatch" |
//! "error", ...}`; a mismatch names the `failed` legs.

use std::io::{self, BufRead, Write};

use bitcoin_hashes::{Hash, sha256d};
use dash_oracle_sys::{Chain, ListEncoding, Oracle};
use serde_json::{Value, json};

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A hash as Core's `GetHex` prints it: internal byte order reversed.
fn rpc_hex(hash: &[u8; 32]) -> String {
    let mut reversed = *hash;
    reversed.reverse();
    to_hex(&reversed)
}

fn check(oracle: &Oracle, sample: &Value) -> Value {
    let field = |name: &str| sample[name].as_str().unwrap_or_else(|| panic!("sample field {name}"));
    let args: Vec<&str> = sample["chain_args"]
        .as_array()
        .expect("chain_args")
        .iter()
        .map(|a| a.as_str().expect("chain arg"))
        .collect();
    let chain = Chain::new(field("chain"), &args);
    let hex = unhex(field("hex"));
    let list = match oracle.mnlist_canonical(&chain, ListEncoding::Canonical, &hex) {
        Ok(list) => list,
        Err(e) => return json!({"result": "error", "error": format!("{e:?}")}),
    };
    let mut failed = Vec::new();
    if list.canonical != hex {
        failed.push("reencode");
    }
    if rpc_hex(&list.hash) != field("hash") {
        failed.push("oracle_hash");
    }
    if list.hash != sha256d::Hash::hash(&hex).to_byte_array() {
        failed.push("sha256d");
    }
    if failed.is_empty() {
        json!({"result": "match", "bytes": hex.len()})
    } else {
        json!({"result": "mismatch", "failed": failed, "oracle_hash": rpc_hex(&list.hash)})
    }
}

fn main() {
    let oracles: Vec<Oracle> = Oracle::all().collect();
    let stdout = io::stdout();
    for line in io::stdin().lock().lines() {
        let line = line.expect("stdin");
        if line.trim().is_empty() {
            continue;
        }
        let sample: Value = serde_json::from_str(&line).expect("sample JSON");
        for oracle in &oracles {
            let mut out = check(oracle, &sample);
            out["height"] = sample["height"].clone();
            out["pin"] = json!(oracle.pin().name());
            let mut lock = stdout.lock();
            writeln!(lock, "{out}").expect("stdout");
            lock.flush().expect("stdout");
        }
    }
}
