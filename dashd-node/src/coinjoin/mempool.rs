//! The server's mempool until W6 (D17): every admission and test-accept refused.

use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_primitives::{OutPoint, Transaction, TxHash};
use dashd_coinjoin::env::Mempool;

/// A [`Mempool`] that admits nothing, has no coins and sees no InstantSend lock, so a session
/// stops at collateral validation.
#[derive(Clone, Copy, Debug, Default)]
pub struct GatedMempool;

impl MempoolAccept for GatedMempool {
    fn test_accept(&self, _: &Transaction) -> Option<i64> {
        None
    }
}

impl CoinView for GatedMempool {
    fn coin(&self, _: &OutPoint) -> Option<ViewCoin> {
        None
    }

    /// Core's answer while InstantSend is disabled, until LK1b.
    // Core: instantsend/instantsend.cpp:400
    fn is_instantsend_locked(&self, _: &TxHash) -> bool {
        false
    }
}

impl Mempool for GatedMempool {
    fn submit(&mut self, _: &Transaction) -> bool {
        false
    }

    fn prioritise(&mut self, _: &TxHash, _: i64) {}
}
