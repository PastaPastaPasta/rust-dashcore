//! Helpers shared by the end-to-end tests.

use dashcore::block::{Block, Header, Version};
use dashcore::hashes::Hash;
use dashcore::{
    BlockHash, CompactTarget, OutPoint, ScriptBuf, Transaction, TxIn, TxMerkleNode, TxOut, Witness,
};

/// A minimal valid block: one coinbase, PoW ground against regtest's limit.
pub fn block(prev: BlockHash, tag: u8) -> Block {
    let coinbase = Transaction {
        version: 1,
        lock_time: 0,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::from(vec![0x01, tag]),
            sequence: u32::MAX,
            witness: Witness::default(),
        }],
        output: vec![TxOut {
            value: 50_0000_0000,
            script_pubkey: ScriptBuf::from(vec![0x51]),
        }],
        special_transaction_payload: None,
    };
    let mut block = Block {
        header: Header {
            version: Version::ONE,
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time: 1_700_000_000 + u32::from(tag),
            bits: CompactTarget::from_consensus(0x207f_ffff),
            nonce: 0,
        },
        txdata: vec![coinbase],
    };
    block.header.merkle_root = block.compute_merkle_root().unwrap();
    while block.header.validate_pow(block.header.target()).is_err() {
        block.header.nonce += 1;
    }
    block
}
