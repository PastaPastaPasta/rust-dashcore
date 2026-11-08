//! Core's data files through the decoders and `core_serialize` (CS0 §9.1): every transaction of
//! `tx_valid.json` and `tx_invalid.json`, and Core's benchmark block block813851.

use dash_consensus::primitives::core_serialize;
use dash_consensus::{decode_block, decode_tx};
use dash_core_vectors::cases::{load_tx_invalid, load_tx_valid};
use dash_core_vectors::hex::hex_str;
use dash_core_vectors::{read_core_file, read_core_text};

#[path = "support/recorded.rs"]
mod recorded;

#[test]
fn tx_vectors_decode_and_reserialize_to_their_bytes() {
    let valid =
        load_tx_valid(&read_core_text("src/test/data/tx_valid.json")).expect("tx_valid loads");
    let invalid = load_tx_invalid(&read_core_text("src/test/data/tx_invalid.json"))
        .expect("tx_invalid loads");
    assert_eq!((valid.len(), invalid.len()), (83, 76));
    for case in valid.iter().chain(&invalid) {
        // Core reads the transaction from the front of the stream (TxCase::tx).
        let mut cursor = &case.tx[..];
        let decoded = match decode_tx(&mut cursor) {
            Ok(decoded) => decoded,
            Err(error) => panic!("row {}: {error}", case.index),
        };
        assert!(cursor.is_empty(), "row {}: bytes left", case.index);
        assert_eq!(core_serialize(decoded.value()), case.tx, "row {}", case.index);
        assert!(decoded.wire_is_canonical(), "row {}", case.index);
    }
}

#[test]
fn block813851_decodes_to_cores_values() {
    let raw = read_core_file("src/bench/data/block813851.raw");
    let recorded = recorded::load().block813851;
    let mut cursor = &raw[..];
    let decoded = decode_block(&mut cursor).expect("block813851 decodes");
    assert!(cursor.is_empty());
    assert!(decoded.wire_is_canonical());
    let block = decoded.value();
    assert_eq!(block.txs().len(), 999);
    assert_eq!(block.size() as usize, raw.len());
    assert_eq!(core_serialize(block), raw);
    assert_eq!(hex_str(block.hash().as_bytes()), recorded.hash);
    let (root, mutated) = block.merkle();
    assert_eq!(hex_str(root.as_bytes()), recorded.merkle_root);
    assert_eq!(mutated, recorded.mutated);
}
