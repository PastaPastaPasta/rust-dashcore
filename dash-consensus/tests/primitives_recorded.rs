//! The rows of CS0 §9.1 that keep recorded expectations: merkle roots over symbolic leaves and
//! Core's `CheckBlock` message for a failing transaction. The values in
//! `data/primitives_recorded.json` were computed by Dash Core's oracle (v24); node-l1's
//! `recorded_fixtures` test checks them against it again on every oracle run.

use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::{BlockValidationResult, TxValidationError, TxValidationResult, decode_block};
use dash_core_vectors::hex::{hex_str, parse_hex};

#[path = "support/recorded.rs"]
mod recorded;

#[test]
fn merkle_roots_and_mutation_flags_match_core() {
    let cases = recorded::load().merkle;
    assert_eq!(cases.len(), 10);
    for case in cases {
        let leaves = case.leaves.iter().map(|&n| [n; 32]).collect();
        let (root, mutated) = compute_merkle_root(leaves);
        assert_eq!(hex_str(&root), case.root, "{}", case.name);
        assert_eq!(mutated, case.mutated, "{}", case.name);
    }
}

/// `CheckBlock` wraps a transaction's rejection as "Transaction check failed (tx hash <txid>)
/// <debug>", printing the txid as `uint256::ToString()` and keeping the trailing space when the
/// transaction's debug message is empty.
#[test]
fn check_block_wraps_a_transaction_rejection_like_core() {
    let case = recorded::load().check_block_tx_failure;
    let bytes = parse_hex(&case.block_hex);
    let decoded = decode_block(&mut &bytes[..]).expect("the fixture block decodes");
    let failing = &decoded.value().txs()[1];
    let block_error = TxValidationError::new(TxValidationResult::Consensus, case.reason.clone())
        .into_block_wrapped(BlockValidationResult::Consensus, &failing.txid());
    assert_eq!(block_error.result() as i32, case.result);
    assert_eq!(block_error.debug_message(), case.debug_message);
    assert_eq!(block_error.to_string(), format!("{}, {}", case.reason, case.debug_message));
}

/// `ConnectBlock` passes the reason and debug message through (a plain conversion, so the
/// strings here are placeholders, not Core output).
#[test]
fn connect_block_keeps_the_transaction_messages() {
    let tx_error = TxValidationError::new(TxValidationResult::Consensus, "a-reason")
        .with_debug("a debug message");
    let block_error = tx_error.into_block(BlockValidationResult::Consensus);
    assert_eq!(block_error.reject_reason(), "a-reason");
    assert_eq!(block_error.debug_message(), "a debug message");
    assert_eq!(block_error.to_string(), "a-reason, a debug message");
    let bare = TxValidationError::new(TxValidationResult::Consensus, "a-reason");
    assert_eq!(bare.to_string(), "a-reason");
}

#[test]
fn result_classes_carry_cores_integers() {
    assert_eq!(TxValidationResult::NoMempool as i32, 11);
    assert_eq!(BlockValidationResult::ChainLock as i32, 10);
}
