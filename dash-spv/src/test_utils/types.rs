use crate::types::HashedBlock;
use dashcore::prelude::CoreBlockHeight;
use dashcore::{Address, Block, Network, Transaction};

impl HashedBlock {
    pub fn dummy(height: CoreBlockHeight, transactions: Vec<Transaction>) -> Self {
        let transactions = if transactions.is_empty() {
            let address = Address::dummy(Network::Regtest, 0);
            vec![Transaction::dummy_coinbase(&address, 50 + u64::from(height))]
        } else {
            transactions
        };
        let mut block = Block::dummy(height, transactions);
        block.header.merkle_root = block.compute_merkle_root().expect("at least one transaction");
        Self::from(block)
    }
}
