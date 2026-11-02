//! The pool's order, removal, eviction and accounting rules. Core's own figures, for every call
//! on Core-generated sequences, are `tests/disconnect_pool_vectors.rs`'s.

use std::slice;
use std::sync::Arc;

use dash_consensus::{Tx, TxRef};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};

use super::DisconnectedBlockTransactions;
use super::memusage::{list_usage, malloc_usage, recursive_dynamic_usage, unordered_map_usage};
use crate::chain::DisconnectPool;

/// A transaction spending outpoint `[n; 32]:0` with an `n + 20`-byte scriptSig.
fn tx(n: u8) -> TxRef {
    Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![TxIn {
            prevout: OutPoint {
                hash: TxHash::from([n; 32]),
                index: 0,
            },
            script_sig: vec![0x51; usize::from(n) + 20].into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 1,
            script_pubkey: vec![0x51].into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

fn txids(txs: &[TxRef]) -> Vec<TxHash> {
    txs.iter().map(|tx| tx.txid()).collect()
}

fn queued(pool: &DisconnectedBlockTransactions) -> Vec<TxHash> {
    pool.queued().map(|tx| tx.txid()).collect()
}

/// Core's `DynamicMemoryUsage` from its parts: every call keeps this.
fn assert_accounted(pool: &DisconnectedBlockTransactions, map_entries: usize) {
    let inner: usize = pool.queued().map(|tx| recursive_dynamic_usage(tx)).sum();
    assert_eq!(pool.inner_usage(), inner);
    assert_eq!(
        pool.dynamic_memory_usage(),
        inner
            + unordered_map_usage(map_entries, pool.bucket_count() as usize)
            + list_usage(pool.len())
    );
}

#[test]
fn each_block_queues_in_reverse_behind_the_last() {
    let [a, b, c, d, e] = [1, 2, 3, 4, 5].map(tx);
    let mut pool = DisconnectedBlockTransactions::default();
    assert!(pool.add_from_block(&[a.clone(), b.clone(), c.clone()]).is_empty());
    assert!(pool.add_from_block(&[d.clone(), e.clone()]).is_empty());
    assert_eq!(queued(&pool), txids(&[c.clone(), b.clone(), a.clone(), e.clone(), d.clone()]));
    assert_accounted(&pool, 5);
    assert_eq!(txids(&pool.take()), txids(&[c, b, a, e, d]));
    assert!(pool.is_empty());
}

#[test]
fn a_connected_block_removes_its_txids() {
    let [a, b, c, x] = [1, 2, 3, 9].map(tx);
    let mut pool = DisconnectedBlockTransactions::default();
    pool.remove_for_block(slice::from_ref(&a));
    assert!(pool.is_empty());
    let _ = pool.add_from_block(&[a.clone(), b.clone(), c.clone()]);
    pool.remove_for_block(&[b.clone(), x]);
    assert_eq!(queued(&pool), txids(&[c.clone(), a.clone()]));
    assert_accounted(&pool, 2);
    assert_eq!(txids(&pool.take()), txids(&[c, a]));
}

/// Over the budget, the pool evicts from the front until it is at or under it.
#[test]
fn the_budget_evicts_from_the_front_in_order() {
    let [a, b, c] = [1, 2, 3].map(tx);
    let block = [a.clone(), b.clone(), c.clone()];
    // reserve(3) gives 3 buckets, which eviction keeps.
    let with = |txs: &[&TxRef]| {
        txs.iter().map(|tx| recursive_dynamic_usage(tx)).sum::<usize>()
            + unordered_map_usage(txs.len(), 3)
            + list_usage(txs.len())
    };
    let mut pool = DisconnectedBlockTransactions::new(with(&[&b, &a]));
    assert_eq!(txids(&pool.add_from_block(&block)), txids(slice::from_ref(&c)));
    assert_eq!(pool.bucket_count(), 3);
    assert_eq!(pool.dynamic_memory_usage(), with(&[&b, &a]));
    assert_eq!(txids(&pool.take()), txids(&[b.clone(), a.clone()]));

    let mut pool = DisconnectedBlockTransactions::new(with(&[&b, &a]) - 1);
    assert_eq!(txids(&pool.add_from_block(&block)), txids(&[c.clone(), b]));
    assert_eq!(txids(&pool.take()), txids(&[a]));

    let mut pool = DisconnectedBlockTransactions::new(0);
    assert_eq!(txids(&pool.add_from_block(&block)).len(), 3);
    assert_eq!(pool.dynamic_memory_usage(), malloc_usage(8 * 3));
}

/// Core assumes a txid is never queued twice. If it is, the map keeps its first entry and the
/// queue both, so a removal takes only the first.
#[test]
fn a_queued_txid_keeps_its_first_map_entry() {
    let [a, b] = [1, 2].map(tx);
    let again = tx(1);
    let mut pool = DisconnectedBlockTransactions::default();
    let _ = pool.add_from_block(&[a.clone(), b.clone()]);
    let _ = pool.add_from_block(slice::from_ref(&again));
    assert_eq!(pool.len(), 3);
    assert_accounted(&pool, 2);
    pool.remove_for_block(slice::from_ref(&a));
    pool.remove_for_block(slice::from_ref(&a));
    let left: Vec<_> = pool.queued().cloned().collect();
    assert!(Arc::ptr_eq(&left[0], &b) && Arc::ptr_eq(&left[1], &again));
    assert_accounted(&pool, 1);
    let _ = pool.take();
}

/// Once a removal has taken the mapped copy of a txid, a later copy has no map entry and stays
/// through a removal; the next add of the txid maps its own copy, which the next removal takes.
#[test]
fn an_unmapped_copy_stays_through_a_removal() {
    let [a, x, y] = [1, 8, 9].map(tx);
    let [again, third] = [tx(1), tx(1)];
    let mut pool = DisconnectedBlockTransactions::default();
    for block in [&a, &x, &again] {
        let _ = pool.add_from_block(slice::from_ref(block));
    }
    pool.remove_for_block(slice::from_ref(&a));
    for block in [&y, &third] {
        let _ = pool.add_from_block(slice::from_ref(block));
    }
    assert_accounted(&pool, 3);
    pool.remove_for_block(slice::from_ref(&a));
    let left: Vec<_> = pool.queued().cloned().collect();
    assert_eq!(left.len(), 3);
    assert!(
        Arc::ptr_eq(&left[0], &x) && Arc::ptr_eq(&left[1], &again) && Arc::ptr_eq(&left[2], &y)
    );
    assert_accounted(&pool, 2);
    let _ = pool.take();
}

/// `take()` clears the map but keeps its buckets, which the accounting still charges.
#[test]
fn take_keeps_the_buckets() {
    let mut pool = DisconnectedBlockTransactions::default();
    let _ = pool.add_from_block(&(1..=40).map(tx).collect::<Vec<_>>());
    let buckets = pool.bucket_count();
    assert_eq!(buckets, 41);
    assert_eq!(pool.take().len(), 40);
    assert_eq!(pool.bucket_count(), buckets);
    assert_eq!(pool.dynamic_memory_usage(), malloc_usage(8 * 41));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "a disconnect pool is dropped with 1 transactions queued")]
fn a_pool_dropped_full_trips_the_drop_assertion() {
    let mut pool = DisconnectedBlockTransactions::default();
    let _ = pool.add_from_block(&[tx(1)]);
}
