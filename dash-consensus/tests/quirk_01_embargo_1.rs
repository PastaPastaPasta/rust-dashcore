// EMBARGO-1
//! quirk_01_embargo_1 (PLAN §7 handling; CS0 §9.4, §10).
//!
//! Replays the private quirk-1 cases through the decoders and compares Core's recorded outputs:
//! the fields `diff_tx_decode` and `diff_block_decode` compare, nothing else. Cases are read only
//! through `$DASH_EMBARGO_DIR/quirk1/MANIFEST`. Output names the case id and the field only.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

use std::path::{Path, PathBuf};

use dash_consensus::primitives::core_serialize;
use dash_consensus::{decode_block, decode_tx};
use dash_core_vectors::hex::hex_str;
use node_sink::manifest::load_cases;
use node_sink::manifest::testing::write_case_corpus;
use node_sink::{private_tree, report, write_receipt};
use serde::Deserialize;

const TEST: &str = "quirk_01_embargo_1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    pin: String,
    kind: String,
    input_hex: String,
    expect: Expect,
}

/// Core's outputs for the input, as the L1 decode targets compare them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    decode_ok: bool,
    consumed: Option<usize>,
    serialized_hex: Option<String>,
    txid: Option<String>,
    instance_hash: Option<String>,
    txids: Option<Vec<String>>,
    instance_hashes: Option<Vec<String>>,
    block_hash: Option<String>,
    merkle_root: Option<String>,
    mutated: Option<bool>,
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// Compares `actual` with an expectation that must be present.
fn same<T: PartialEq>(
    expected: &Option<T>,
    actual: T,
    field: &'static str,
) -> Result<(), &'static str> {
    match expected {
        Some(expected) if *expected == actual => Ok(()),
        _ => Err(field),
    }
}

/// Our decode against Core's verdict: `Some` to compare the fields, `None` when both reject.
fn verdict<T, E>(ours: Result<T, E>, expect: &Expect) -> Result<Option<T>, &'static str> {
    match (ours, expect.decode_ok) {
        (Ok(decoded), true) => Ok(Some(decoded)),
        (Err(_), false) => Ok(None),
        _ => Err("decode verdict"),
    }
}

/// [`dash_consensus::Decoded::core_bytes`] and the size must equal Core's serialization.
fn stored_bytes(core_bytes: &[u8], size: u32, expect: &Expect) -> Result<(), &'static str> {
    same(&expect.serialized_hex, hex_str(core_bytes), "core bytes")?;
    same(&expect.serialized_hex.as_ref().map(|hex| hex.len() / 2), size as usize, "size")
}

/// Checks one case; the error is the field that differs.
fn check(case: &Case) -> Result<(), &'static str> {
    // v24 only (DEC-82). A case the corpus recorded at v23.1 keeps its place (the receipt counts
    // it). The txid rule is v24's, so the txid, merkle root and mutation flag are compared on
    // every v24.0 case, and on a v23.1 case only where the rule cannot matter: a transaction
    // whose txid is its instance hash has the same txid under both rules.
    let v24 = match case.pin.as_str() {
        "v23.1" => false,
        "v24.0" => true,
        _ => return Err("pin"),
    };
    let input = unhex(&case.input_hex).ok_or("input")?;
    let expect = &case.expect;
    let mut cursor = &input[..];
    match case.kind.as_str() {
        "tx" => {
            let Some(decoded) = verdict(decode_tx(&mut cursor), expect)? else {
                return Ok(());
            };
            let tx = decoded.value();
            let serialized = core_serialize(tx);
            same(&expect.consumed, decoded.wire().len(), "consumed")?;
            same(&expect.serialized_hex, hex_str(&serialized), "serialization")?;
            stored_bytes(decoded.core_bytes().as_slice(), tx.size(), expect)?;
            if v24 || tx.txid() == tx.instance_hash() {
                same(&expect.txid, hex_str(tx.txid().as_bytes()), "txid")?;
            }
            same(&expect.instance_hash, hex_str(tx.instance_hash().as_bytes()), "instance hash")?;
            if decoded.wire_is_canonical() != (serialized == decoded.wire()) {
                return Err("wire_is_canonical");
            }
            Ok(())
        }
        "block" => {
            let Some(decoded) = verdict(decode_block(&mut cursor), expect)? else {
                return Ok(());
            };
            let block = decoded.value();
            let serialized = core_serialize(block);
            let (root, mutated) = block.merkle();
            same(&expect.consumed, decoded.wire().len(), "consumed")?;
            same(&expect.serialized_hex, hex_str(&serialized), "serialization")?;
            stored_bytes(decoded.core_bytes().as_slice(), block.size(), expect)?;
            same(&expect.block_hash, hex_str(block.hash().as_bytes()), "block hash")?;
            if v24 || block.txs().iter().all(|tx| tx.txid() == tx.instance_hash()) {
                same(&expect.merkle_root, hex_str(root.as_bytes()), "merkle root")?;
                same(&expect.mutated, mutated, "mutated")?;
                let txids: Vec<String> =
                    block.txs().iter().map(|tx| hex_str(tx.txid().as_bytes())).collect();
                same(&expect.txids, txids, "txids")?;
            }
            let instance: Vec<String> =
                block.txs().iter().map(|tx| hex_str(tx.instance_hash().as_bytes())).collect();
            same(&expect.instance_hashes, instance, "instance hashes")?;
            if decoded.wire_is_canonical() != (serialized == decoded.wire()) {
                return Err("wire_is_canonical");
            }
            Ok(())
        }
        _ => Err("kind"),
    }
}

/// The id a failure may print: the case's own id when it is opaque (`case-<digits>`), otherwise
/// its position in the manifest.
fn printable_id(id: &str, index: usize) -> String {
    match id.strip_prefix("case-") {
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => id.to_owned(),
        _ => format!("entry {index}"),
    }
}

/// Runs every case of `dir`; returns the number checked, or a sanitized failure.
fn run(dir: &Path) -> Result<u64, String> {
    let cases = load_cases(dir).map_err(|e| e.to_string())?;
    let mut failures = Vec::new();
    for (index, bytes) in cases.iter().enumerate() {
        match serde_json::from_slice::<Case>(bytes) {
            Ok(case) => {
                if let Err(field) = check(&case) {
                    failures.push(format!("{}: {field}", printable_id(&case.id, index)));
                }
            }
            Err(_) => failures.push(format!("entry {index}: the case does not parse")),
        }
    }
    if failures.is_empty() {
        Ok(cases.len() as u64)
    } else {
        Err(failures.join("; "))
    }
}

#[test]
fn quirk_01_embargo_1() {
    let Some(root) = private_tree(TEST) else {
        return;
    };
    match run(&root.join("quirk1")) {
        Ok(cases) => {
            let receipt = write_receipt(&root, TEST, &[("cases", cases)]);
            report(&format!("{TEST}: PASS {cases} cases"));
            assert!(receipt.is_ok(), "{TEST}: the receipt could not be written");
        }
        Err(message) => {
            report(&format!("{TEST}: FAIL"));
            panic!("{TEST}: {message}");
        }
    }
}

// Self-tests of the driver on synthetic corpora (no private data).

fn synthetic(name: &str, header: &str, cases: &[&str]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("quirk-driver").join(name);
    let cases: Vec<&[u8]> = cases.iter().map(|c| c.as_bytes()).collect();
    write_case_corpus(&dir, header, &cases, &[]);
    dir
}

const REJECTED: &str =
    r#"{"id":"case-001","pin":"v24.0","kind":"tx","input_hex":"00","expect":{"decode_ok":false}}"#;

/// A version-1 transaction with no inputs or outputs, which both sides decode, with every field
/// Core reports (the txid is SHA256d of the ten bytes).
const DECODED: &str = r#"{"id":"case-002","pin":"v24.0","kind":"tx","input_hex":"01000000000000000000","expect":{"decode_ok":true,"consumed":10,"serialized_hex":"01000000000000000000","txid":"43ec7a579f5561a42a7e9637ad4156672735a658be2752181801f723ba3316d2","instance_hash":"43ec7a579f5561a42a7e9637ad4156672735a658be2752181801f723ba3316d2"}}"#;

#[test]
fn an_empty_corpus_fails() {
    assert!(run(&synthetic("empty", "cases 0\nmin 0\n", &[])).is_err());
    assert!(run(&synthetic("below-min", "cases 1\nmin 2\n", &[REJECTED])).is_err());
}

#[test]
fn cases_pass_or_name_their_field() {
    assert!(run(&synthetic("pass", "cases 1\nmin 1\n", &[REJECTED])) == Ok(1));
    // A decoded case runs every comparison, core bytes and size included.
    assert!(run(&synthetic("decoded", "cases 1\nmin 1\n", &[DECODED])) == Ok(1));
    let wrong_txid = DECODED.replacen("43ec7a", "53ec7a", 1);
    assert!(
        run(&synthetic("decoded-txid", "cases 1\nmin 1\n", &[&wrong_txid]))
            == Err("case-002: txid".into())
    );
    let wrong = REJECTED.replace("\"decode_ok\":false", "\"decode_ok\":true");
    assert!(
        run(&synthetic("wrong", "cases 1\nmin 1\n", &[&wrong]))
            == Err("case-001: decode verdict".into())
    );
    let unknown = REJECTED.replace("\"decode_ok\":false", "\"decode_ok\":false,\"other\":1");
    assert!(run(&synthetic("unknown-field", "cases 1\nmin 1\n", &[&unknown])).is_err());
}

#[test]
fn failures_print_only_opaque_ids() {
    let named = REJECTED
        .replace("case-001", "a-descriptive-name")
        .replace("\"decode_ok\":false", "\"decode_ok\":true");
    assert!(
        run(&synthetic("named", "cases 1\nmin 1\n", &[&named]))
            == Err("entry 0: decode verdict".into())
    );
}
