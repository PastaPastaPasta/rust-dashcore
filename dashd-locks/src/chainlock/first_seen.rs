//! The ChainLock handler's transaction first-seen times (`txFirstSeenTime`), which the signer
//! reads to decide whether an unlocked transaction is old enough to sign over.

use std::collections::HashMap;
use std::sync::Arc;

use dash_clock::Clock;
use dash_consensus::primitives::TxHash;
use dash_consensus::{Block, BlockId, TxRef};
use dash_llmq::sigshares::manager::CleanupThrottler;
use dashd_chainstate::chain::{Cont, ValidationInterface};
use parking_lot::Mutex;

use super::reads::{Abort, ChainReads, TX_CONFIRM_THRESHOLD};
use crate::signing::TxSource;

/// Core's `WAIT_FOR_ISLOCK_TIMEOUT`, in seconds.
// Core: v24.0.2 src/chainlock/handler.cpp:40
pub const WAIT_FOR_ISLOCK_TIMEOUT: i64 = 600;

// Core: v24.0.2 src/chainlock/handler.cpp:341 Cleanup
const CLEANUP_INTERVAL_MS: i64 = 30_000;

/// `CMasternodeSync::IsBlockchainSynced()`, read at each call.
pub type Synced = Arc<dyn Fn() -> bool + Send + Sync>;

/// The txids a block contributes: neither coinbase nor without inputs.
pub(crate) fn lockable_txids(block: &Block) -> impl Iterator<Item = TxHash> + '_ {
    block.txs().iter().filter(|tx| !tx.is_coinbase() && !tx.inputs().is_empty()).map(|tx| tx.txid())
}

/// `txFirstSeenTime` and its validation-interface half.
pub struct TxFirstSeen {
    seen: Mutex<HashMap<TxHash, i64>>,
    clock: Arc<dyn Clock>,
    synced: Synced,
    abort: Abort,
    throttle: Mutex<CleanupThrottler>,
}

impl TxFirstSeen {
    pub fn new(clock: Arc<dyn Clock>, synced: Synced, abort: Abort) -> Self {
        Self {
            seen: Mutex::default(),
            clock,
            synced,
            abort,
            throttle: Mutex::default(),
        }
    }

    /// Records `time` for each txid not yet seen.
    // Core: v24.0.2 src/chainlock/handler.cpp:100-107 UpdateTxFirstSeenMap
    pub fn update(&self, txids: impl IntoIterator<Item = TxHash>, time: i64) {
        let mut seen = self.seen.lock();
        for txid in txids {
            seen.entry(txid).or_insert(time);
        }
    }

    /// An unseen txid has age zero, so it is unsafe.
    // Core: v24.0.2 src/chainlock/handler.cpp:262-274 IsTxSafeForMining
    pub fn is_tx_safe_for_mining(&self, txid: &TxHash) -> bool {
        let first = self.seen.lock().get(txid).copied();
        let age = first.map_or(0, |t| self.clock.node_now().secs().saturating_sub(t));
        age >= WAIT_FOR_ISLOCK_TIMEOUT
    }

    // Core: v24.0.2 src/chainlock/handler.cpp:329-337 CleanupFromSigner
    pub fn cleanup_from_signer<'a>(&self, removed: impl IntoIterator<Item = &'a TxHash>) {
        let mut seen = self.seen.lock();
        for txid in removed {
            seen.remove(txid);
        }
    }

    /// The transaction half of the handler's `Cleanup`, at most every 30 s while synced: a
    /// vanished transaction is dropped, a mempool one kept, and a mined one dropped once its
    /// block is on the active chain more than [`TX_CONFIRM_THRESHOLD`] deep. `txs` is read with
    /// no lock held, and its fault goes to the abort with nothing erased. A mined block the
    /// chain does not know is kept, where Core asserts.
    // Core: v24.0.2 src/chainlock/handler.cpp:339-348, :363-383 Cleanup
    pub fn cleanup(&self, txs: &dyn TxSource, chain: &dyn ChainReads) {
        let now_ms = self.clock.node_now_micros() / 1000;
        if !(self.synced)() || !self.throttle.lock().try_cleanup(now_ms, CLEANUP_INTERVAL_MS) {
            return;
        }
        if let Err(e) = self.tx_cleanup(txs, chain) {
            (self.abort)(&e.to_string());
        }
    }

    fn tx_cleanup(&self, txs: &dyn TxSource, chain: &dyn ChainReads) -> dash_kv::Result<()> {
        let tip = chain.tip();
        let txids: Vec<TxHash> = self.seen.lock().keys().copied().collect();
        let mut drop = Vec::new();
        for txid in txids {
            let erase = match txs.get_transaction(&txid)? {
                None => true,
                Some(None) => false,
                Some(Some(block)) => {
                    chain.lookup(&block).zip(tip.as_ref()).is_some_and(|(b, t)| {
                        chain.ancestor_hash(&t.hash, b.height) == Some(block)
                            && t.height - b.height > TX_CONFIRM_THRESHOLD
                    })
                }
            };
            if erase {
                drop.push(txid);
            }
        }
        self.cleanup_from_signer(&drop);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.seen.lock().len()
    }
}

impl ValidationInterface for TxFirstSeen {
    // Core: v24.0.2 src/chainlock/handler.cpp:229-237 TransactionAddedToMempool
    fn transaction_added_to_mempool(&self, tx: &TxRef, accept_time: i64, _sequence: u64) {
        if !tx.is_coinbase() && !tx.inputs().is_empty() {
            self.update([tx.txid()], accept_time);
        }
    }

    // Core: v24.0.2 src/chainlock/handler.cpp:244-260 BlockConnected
    fn block_connected(&self, block: &Arc<Block>, _id: BlockId) -> Cont {
        if (self.synced)() {
            self.update(lockable_txids(block), self.clock.node_now().secs());
        }
        Cont::Done
    }
}

#[cfg(test)]
#[path = "first_seen_tests.rs"]
mod tests;
