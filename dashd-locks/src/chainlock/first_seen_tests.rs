//! [`TxFirstSeen`]: the handler's first-seen map, its listener and its cleanup.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_clock::MockClock;
use dash_consensus::BlockId;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::ValidationInterface;

use super::*;
use crate::chainlock::fakes::{FakeChain, FakeTxs, coinbase, no_inputs, spend};

const T0: i64 = 1_700_000_000;

type Aborts = Arc<parking_lot::Mutex<Vec<String>>>;

fn seen() -> (TxFirstSeen, Arc<MockClock>, Arc<AtomicBool>, Aborts) {
    let clock = Arc::new(MockClock::at(T0));
    let synced = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&synced);
    let aborts = Aborts::default();
    let sink = Arc::clone(&aborts);
    let seen = TxFirstSeen::new(
        clock.clone(),
        Arc::new(move || flag.load(Ordering::Relaxed)),
        Arc::new(move |e: &str| sink.lock().push(e.to_owned())),
    );
    (seen, clock, synced, aborts)
}

fn txid(tag: u32) -> TxHash {
    spend(tag).txid()
}

// Core: v24.0.2 src/chainlock/handler.cpp:100-107 UpdateTxFirstSeenMap
#[test]
fn an_update_keeps_the_first_time() {
    let (seen, clock, _, _) = seen();
    seen.update([txid(1)], T0 - WAIT_FOR_ISLOCK_TIMEOUT);
    seen.update([txid(1)], T0);
    assert!(seen.is_tx_safe_for_mining(&txid(1)));
    clock.set_mocktime(T0 + 1);
    assert!(seen.is_tx_safe_for_mining(&txid(1)));
}

// Core: v24.0.2 src/chainlock/handler.cpp:262-274 IsTxSafeForMining
#[test]
fn a_transaction_is_safe_from_ten_minutes_and_an_unknown_one_never() {
    let (seen, clock, _, _) = seen();
    seen.update([txid(1)], T0);
    clock.set_mocktime(T0 + WAIT_FOR_ISLOCK_TIMEOUT - 1);
    assert!(!seen.is_tx_safe_for_mining(&txid(1)));
    clock.set_mocktime(T0 + WAIT_FOR_ISLOCK_TIMEOUT);
    assert!(seen.is_tx_safe_for_mining(&txid(1)));
    assert!(!seen.is_tx_safe_for_mining(&txid(2)));
}

// Core: v24.0.2 src/chainlock/handler.cpp:229-237 TransactionAddedToMempool
#[test]
fn a_mempool_add_records_its_accept_time_but_skips_coinbase_and_input_less() {
    let (seen, _, _, _) = seen();
    for tx in [spend(1), coinbase(2), no_inputs(3)] {
        seen.transaction_added_to_mempool(&Arc::new(tx), T0 - WAIT_FOR_ISLOCK_TIMEOUT, 0);
    }
    assert_eq!(seen.len(), 1);
    assert!(seen.is_tx_safe_for_mining(&txid(1)));
}

// Core: v24.0.2 src/chainlock/handler.cpp:244-260 BlockConnected
#[test]
fn a_connected_block_records_now_only_while_synced() {
    let (seen, clock, synced, _) = seen();
    let chain = FakeChain::default();
    synced.store(false, Ordering::Relaxed);
    let block = chain.mine(vec![coinbase(0), spend(1), no_inputs(2)], 1);
    assert!(matches!(seen.block_connected(&block, BlockId(0)), Cont::Done));
    assert_eq!(seen.len(), 0);
    synced.store(true, Ordering::Relaxed);
    assert!(matches!(seen.block_connected(&block, BlockId(0)), Cont::Done));
    assert_eq!(seen.len(), 1);
    clock.set_mocktime(T0 + WAIT_FOR_ISLOCK_TIMEOUT);
    assert!(seen.is_tx_safe_for_mining(&txid(1)));
}

// Core: v24.0.2 src/chainlock/handler.cpp:329-337 CleanupFromSigner
#[test]
fn cleanup_from_signer_erases_the_given_txids() {
    let (seen, _, _, _) = seen();
    seen.update([txid(1), txid(2)], T0);
    seen.cleanup_from_signer(&[txid(1)]);
    assert_eq!(seen.len(), 1);
}

/// A line of `n` blocks, each with no transactions.
fn line(n: usize) -> (Arc<FakeChain>, Vec<BlockHash>) {
    let chain = Arc::new(FakeChain::default());
    let hashes = (0..n).map(|h| chain.mine(vec![], h as u32).hash()).collect();
    (chain, hashes)
}

// Core: v24.0.2 src/chainlock/handler.cpp:367-369 Cleanup
#[test]
fn cleanup_drops_a_vanished_transaction() {
    let (seen, _, _, _) = seen();
    let (chain, _) = line(2);
    seen.update([txid(1)], T0);
    seen.cleanup(&FakeTxs::default(), &*chain);
    assert_eq!(seen.len(), 0);
}

// Core: v24.0.2 src/chainlock/handler.cpp:380-382 Cleanup
#[test]
fn cleanup_keeps_a_mempool_transaction() {
    let (seen, _, _, _) = seen();
    let (chain, _) = line(2);
    let txs = FakeTxs::default();
    txs.0.lock().insert(txid(1), None);
    seen.update([txid(1)], T0);
    seen.cleanup(&txs, &*chain);
    assert_eq!(seen.len(), 1);
}

/// Heights 0..=7: the block at height 1 is 6 deep, at height 2 is 5 deep, and a fork block at
/// height 1 is off the active chain.
// Core: v24.0.2 src/chainlock/handler.cpp:370-379 Cleanup
#[test]
fn cleanup_drops_a_mined_transaction_only_more_than_five_deep_on_the_active_chain() {
    let (seen, _, _, _) = seen();
    let (chain, hashes) = line(8);
    let fork = chain.mine_on(Some(hashes[0]), vec![], 99).hash();
    chain.set_tip(hashes[7]);
    let txs = FakeTxs::default();
    for (tag, block) in [(1, hashes[1]), (2, hashes[2]), (3, fork)] {
        txs.0.lock().insert(txid(tag), Some(block));
    }
    seen.update([txid(1), txid(2), txid(3)], T0);
    seen.cleanup(&txs, &*chain);
    let left = seen.seen.lock();
    assert!(!left.contains_key(&txid(1)));
    assert!(left.contains_key(&txid(2)) && left.contains_key(&txid(3)));
}

// Core: v24.0.2 src/chainlock/handler.cpp:342-344 Cleanup
#[test]
fn cleanup_does_nothing_while_unsynced() {
    let (seen, _, synced, _) = seen();
    let (chain, _) = line(2);
    seen.update([txid(1)], T0);
    synced.store(false, Ordering::Relaxed);
    seen.cleanup(&FakeTxs::default(), &*chain);
    assert_eq!(seen.len(), 1);
}

// Core: v24.0.2 src/chainlock/handler.cpp:341, :346-348 Cleanup; util/time.h:50-64 TryCleanup
#[test]
fn cleanup_runs_at_most_every_thirty_seconds() {
    let (seen, clock, _, _) = seen();
    let (chain, _) = line(2);
    let none = FakeTxs::default();
    seen.cleanup(&none, &*chain);
    seen.update([txid(1)], T0);
    clock.set_mocktime(T0 + 29);
    seen.cleanup(&none, &*chain);
    assert_eq!(seen.len(), 1);
    clock.set_mocktime(T0 + 30);
    seen.cleanup(&none, &*chain);
    assert_eq!(seen.len(), 0);
}

// Core: v24.0.2 src/chainlock/handler.cpp:363-367 Cleanup
#[test]
fn a_tx_source_fault_aborts_and_erases_nothing() {
    let (seen, _, _, aborts) = seen();
    let (chain, _) = line(2);
    let txs = FakeTxs::default();
    *txs.1.lock() = true;
    seen.update([txid(1)], T0);
    seen.cleanup(&txs, &*chain);
    assert_eq!(seen.len(), 1);
    assert_eq!(aborts.lock().len(), 1);
}
