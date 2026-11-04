//! `block_mutation` (Core's `IsBlockMutated`, validation.cpp:4434-4457) on Core's data: the
//! benchmark block block813851 (`src/bench/data/block813851.raw`, 999 transactions) and the one
//! 64-byte transaction in `src/test/data/tx_invalid.json`. The mutated blocks are built from
//! those bytes the way CVE-2012-2459 and the 64-byte case describe; no transaction is
//! hand-written.

use std::sync::Arc;

use dash_consensus::block::{Mutation, block_mutation, is_block_mutated};
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::{Block, Header, decode_block, decode_tx};
use dash_core_vectors::cases::load_tx_invalid;
use dash_core_vectors::{read_core_file, read_core_text};
use dash_primitives::MerkleRoot;
use dash_types::Numeric as _;

fn block813851() -> Block {
    let raw = read_core_file("src/bench/data/block813851.raw");
    let mut cursor = &raw[..];
    decode_block(&mut cursor).expect("block813851 decodes").into_value()
}

#[test]
fn an_intact_block_is_not_mutated() {
    let block = block813851();
    assert_eq!(block_mutation(&block), None);
    assert!(!is_block_mutated(&block));
}

#[test]
fn a_duplicated_last_transaction_is_the_duplicate_mutation() {
    let block = block813851();
    // An odd count: repeating the last transaction leaves the root unchanged (CVE-2012-2459).
    assert_eq!(block.txs().len() % 2, 1);
    let mut txs = block.txs().to_vec();
    txs.push(Arc::clone(txs.last().expect("transactions")));
    let mutated = Block::new(*block.header(), txs);
    assert_eq!(mutated.merkle().0, block.merkle().0);
    let Some(Mutation::MerkleRoot(e)) = block_mutation(&mutated) else {
        panic!("expected a merkle-root mutation");
    };
    assert_eq!(e.to_string(), "bad-txns-duplicate, duplicate transaction");
}

#[test]
fn a_dropped_transaction_is_a_merkle_root_mismatch() {
    let block = block813851();
    let mut txs = block.txs().to_vec();
    txs.pop();
    let Some(Mutation::MerkleRoot(e)) = block_mutation(&Block::new(*block.header(), txs)) else {
        panic!("expected a merkle-root mutation");
    };
    assert_eq!(e.to_string(), "bad-txnmrklroot, hashMerkleRoot mismatch");
}

#[test]
fn a_64_byte_transaction_without_a_coinbase_is_mutated() {
    let cases = load_tx_invalid(&read_core_text("src/test/data/tx_invalid.json"))
        .expect("tx_invalid loads");
    let raw = cases.iter().map(|c| &c.tx).find(|t| t.len() == 64).expect("a 64-byte tx");
    let mut cursor = &raw[..];
    let tx = Arc::new(decode_tx(&mut cursor).expect("decodes").into_value());
    assert!(!tx.is_coinbase());
    assert_eq!(tx.size(), 64);

    // A header committing to exactly this transaction, so `CheckMerkleRoot` passes.
    let mut header = *block813851().header().raw();
    let (root, _) = compute_merkle_root(vec![*tx.txid().as_bytes()]);
    header.merkle_root = MerkleRoot::from_lendian(root);
    let block = Block::new(Header::new(header), vec![Arc::clone(&tx)]);
    assert_eq!(block_mutation(&block), Some(Mutation::Tx64));

    // With a coinbase first, Core neglects the 64-byte case (it needs 224 bits of work).
    let coinbase = Arc::clone(&block813851().txs()[0]);
    let txs = vec![coinbase, tx];
    let (root, _) = compute_merkle_root(txs.iter().map(|t| *t.txid().as_bytes()).collect());
    header.merkle_root = MerkleRoot::from_lendian(root);
    assert_eq!(block_mutation(&Block::new(Header::new(header), txs)), None);
}
