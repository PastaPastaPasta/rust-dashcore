//! Every checked-in vector file is intact and loads under Core's row rules.
//!
//! The expected counts are the ones Core's harness sees at the pinned commit (comment rows
//! excluded). A count that drifts means the data or the loader changed.

use dash_core_vectors::cases::*;
use dash_core_vectors::{core_dir, read_core_text};
use dashcore_hashes::{Hash, sha256};

fn data(name: &str) -> String {
    read_core_text(&format!("src/test/data/{name}"))
}

#[test]
fn verbatim_files_match_their_checksums() {
    let list = std::fs::read_to_string(core_dir().join("FILES.sha256")).unwrap();
    let mut n = 0;
    for line in list.lines() {
        let (digest, path) = line.split_once("  ").expect("`<sha256>  <path>` line");
        let bytes = std::fs::read(core_dir().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        let actual = sha256::Hash::hash(&bytes);
        assert_eq!(format!("{actual}"), digest, "{path} was modified");
        n += 1;
    }
    assert_eq!(n, 62);
}

#[test]
fn script_tests_load() {
    let cases = load_script_tests(&data("script_tests.json")).unwrap();
    assert_eq!(cases.len(), 1282);
    assert_eq!(cases.iter().filter(|c| c.expected_error == "OK").count(), 740);
    // The DIP0020 additions exercise Dash-only error names.
    for name in ["CHECKDATASIGVERIFY", "SPLIT_RANGE", "OPERAND_SIZE", "DIV_BY_ZERO", "MOD_BY_ZERO"]
    {
        assert!(cases.iter().any(|c| c.expected_error == name), "{name}");
    }
}

#[test]
fn tx_valid_and_invalid_load() {
    let valid = load_tx_valid(&read_core_text("src/test/data/tx_valid.json")).unwrap();
    assert_eq!(valid.len(), 83);
    assert!(valid.iter().all(|c| matches!(c.flags, TxFlags::Script(_))));
    let invalid = load_tx_invalid(&read_core_text("src/test/data/tx_invalid.json")).unwrap();
    assert_eq!(invalid.len(), 76);
    assert_eq!(invalid.iter().filter(|c| c.flags == TxFlags::BadTx).count(), 10);
    for c in valid.iter().chain(&invalid) {
        assert!(!c.prevouts.is_empty(), "row {}", c.index);
        assert!(!c.tx.is_empty(), "row {}", c.index);
    }
}

#[test]
fn sighash_loads() {
    let cases = load_sighash(&data("sighash.json")).unwrap();
    assert_eq!(cases.len(), 500);
    for c in &cases {
        assert_eq!(c.expected.len(), 64, "row {}", c.index);
    }
}

#[test]
fn trivial_validation_loads() {
    let valid = load_trivial("trivially_valid.json", &data("trivially_valid.json")).unwrap();
    assert_eq!(valid.len(), 25);
    assert!(valid.iter().all(|c| c.expected_error.is_none()));
    let invalid = load_trivial("trivially_invalid.json", &data("trivially_invalid.json")).unwrap();
    assert_eq!(invalid.len(), 12);
    assert!(invalid.iter().all(|c| c.expected_error.is_some()));
}

#[test]
fn key_and_encoding_files_load() {
    assert_eq!(load_base58(&data("base58_encode_decode.json")).unwrap().len(), 14);
    let valid = load_key_io_valid(&data("key_io_valid.json")).unwrap();
    assert_eq!(valid.len(), 50);
    assert_eq!(valid.iter().filter(|c| c.is_privkey).count(), 24);
    assert_eq!(load_key_io_invalid(&data("key_io_invalid.json")).unwrap().len(), 50);
    assert_eq!(load_bip39(&data("bip39_vectors.json")).unwrap().len(), 24);
    assert_eq!(load_proposals_valid(&data("proposals_valid.json")).unwrap().len(), 6);
    assert_eq!(load_proposals_invalid(&data("proposals_invalid.json")).unwrap().len(), 19);
}
