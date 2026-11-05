//! Core's transaction data files through `CheckTransaction` (CS3 L0), as Core's
//! `transaction_tests.cpp` runs them: every `tx_valid.json` transaction passes, and a
//! `tx_invalid.json` transaction fails exactly when the row is marked `BADTX`.

use dash_consensus::tx::check_transaction;
use dash_consensus::{Tx, decode_tx};
use dash_core_vectors::cases::{TxCase, TxFlags, load_tx_invalid, load_tx_valid};
use dash_core_vectors::read_core_text;

/// Core deserializes each row's transaction from the front of the stream.
fn decode(case: &TxCase) -> Tx {
    let mut cursor = &case.tx[..];
    match decode_tx(&mut cursor) {
        Ok(decoded) => decoded.into_value(),
        Err(error) => panic!("row {}: {error}", case.index),
    }
}

#[test]
fn tx_valid_passes_check_transaction() {
    let valid = load_tx_valid(&read_core_text("src/test/data/tx_valid.json")).unwrap();
    assert_eq!(valid.len(), 83);
    for case in &valid {
        if let Err(e) = check_transaction(&decode(case)) {
            panic!("tx_valid row {}: {}", case.index, e.reject_reason());
        }
    }
}

#[test]
fn tx_invalid_fails_check_transaction_exactly_when_badtx() {
    let invalid = load_tx_invalid(&read_core_text("src/test/data/tx_invalid.json")).unwrap();
    let mut reasons = Vec::new();
    for case in &invalid {
        let result = check_transaction(&decode(case));
        assert_eq!(
            result.is_err(),
            case.flags == TxFlags::BadTx,
            "tx_invalid row {}: {result:?}",
            case.index
        );
        if let Err(e) = result {
            reasons.push(e.reject_reason().to_owned());
        }
    }
    reasons.sort();
    reasons.dedup();
    // The file's BADTX rows cover these CheckTransaction rejections.
    assert_eq!(
        reasons,
        [
            "bad-cb-length",
            "bad-txns-inputs-duplicate",
            "bad-txns-prevout-null",
            "bad-txns-txouttotal-toolarge",
            "bad-txns-vin-empty",
            "bad-txns-vout-empty",
            "bad-txns-vout-negative",
            "bad-txns-vout-toolarge",
        ]
    );
}
