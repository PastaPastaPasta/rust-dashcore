use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use dash_consensus::{Tx, TxRef};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dashd_mempool::is::IsMempool;
use dashd_mempool::options::{MemPoolLimits, MemPoolOptions};
use dashd_mempool::pool::ancestors::NO_LIMITS;
use dashd_mempool::pool::index::PoolIndex;
use dashd_mempool::pool::{EntryMetadata, Mempool, PendingEvent, PoolEntry, PoolState};
use dashd_mempool::types::{LockPoints, MemPoolRemovalReason as Reason};

fn tx(n: u32, inputs: &[OutPoint]) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        inputs.iter().map(|p| TxIn::new(*p, vec![].into(), u32::MAX)).collect(),
        vec![TxOut::from_duffs(1, vec![0x51].into()); 4],
        n,
        vec![],
    )))
}
fn output(tx: &Tx, n: u32) -> OutPoint {
    OutPoint::new(tx.txid(), n)
}
fn add(pool: &mut PoolState, tx: &TxRef, fee: i64, sigops: i64) {
    let entry = pool.new_entry(
        tx.clone(),
        EntryMetadata {
            fee,
            sigops,
            ..Default::default()
        },
    );
    pool.add_unchecked(entry);
    pool.check();
}

#[test]
fn entry_metadata_vsize_and_options() {
    let tx = tx(1, &[]);
    for bytes_per_sigop in [0, 20, 30, u32::MAX] {
        let options = MemPoolOptions {
            bytes_per_sigop,
            ..Default::default()
        };
        let mut pool = PoolState::new(options);
        let metadata = EntryMetadata {
            fee: 123,
            time: 456,
            height: 789,
            spends_coinbase: true,
            sigops: 100,
            lock_points: LockPoints {
                height: 7,
                time: 8,
                max_input_block: None,
            },
        };
        let mut entry = pool.new_entry(tx.clone(), metadata);
        assert_eq!(*entry.metadata(), metadata);
        assert_eq!(entry.vsize(), u64::from(tx.size()).max(100 * u64::from(bytes_per_sigop)));
        assert_eq!(entry.ancestors().count, 1);
        assert_eq!(entry.ancestor_sigops(), 100);
        entry.update_lock_points(LockPoints::default());
        assert_eq!(entry.metadata().lock_points, LockPoints::default());
        pool.add_unchecked(entry);
        pool.check();
        assert_eq!(pool.total_fee(), 123);
        assert_eq!(pool.min_fee(1), 0);
    }
}

#[test]
fn diamond_links_stats_limits_and_mined_parent_removal() {
    let mut pool = PoolState::default();
    let a = tx(1, &[]);
    let b = tx(2, &[output(&a, 0)]);
    let c = tx(3, &[output(&a, 1)]);
    let d = tx(4, &[output(&b, 0), output(&c, 0)]);
    for (tx, fee) in [(&a, 10), (&b, 20), (&c, 30), (&d, 40)] {
        add(&mut pool, tx, fee, 2);
    }
    let all = BTreeSet::from([a.txid(), b.txid(), c.txid(), d.txid()]);
    assert_eq!(pool.calculate_descendants(a.txid()), all);
    assert_eq!(pool.get_iter(&a.txid()).unwrap().descendants().modified_fees, 100);
    assert_eq!(pool.get_iter(&d.txid()).unwrap().ancestors().count, 4);
    assert_eq!(pool.get_iter(&d.txid()).unwrap().ancestor_sigops(), 8);
    assert_eq!(
        pool.calculate_mem_pool_ancestors(pool.get_iter(&d.txid()).unwrap(), &NO_LIMITS, false)
            .unwrap(),
        BTreeSet::from([a.txid(), b.txid(), c.txid()])
    );
    pool.remove_staged(&BTreeSet::from([a.txid(), b.txid()]), true, Reason::Block);
    pool.check();
    assert_eq!(pool.sequence(), 3);
    assert!(pool.take_pending_events().is_empty());
    assert_eq!(pool.get_iter(&d.txid()).unwrap().ancestors().count, 2);
    assert_eq!(pool.get_iter(&d.txid()).unwrap().ancestors().modified_fees, 70);
    assert_eq!(pool.get_iter(&d.txid()).unwrap().ancestor_sigops(), 4);
    assert!(pool.has_no_inputs_of(&c));
    pool.remove_recursive(&a, Reason::Reorg);
    pool.check();
    assert!(pool.is_empty());
    assert_eq!(pool.take_pending_events().len(), 2);
}

#[test]
fn ancestor_limits_match_core_boundaries_and_reject_order() {
    let mut pool = PoolState::default();
    let a = tx(1, &[]);
    let b = tx(2, &[output(&a, 0)]);
    let c = tx(3, &[output(&b, 0)]);
    add(&mut pool, &a, 1, 0);
    add(&mut pool, &b, 2, 0);
    let entry = pool.new_entry(c, Default::default());
    let size = (pool.total_vsize() + entry.vsize()) as i64;
    let exact = MemPoolLimits {
        ancestor_count: 3,
        descendant_count: 3,
        ancestor_size_vbytes: size,
        descendant_size_vbytes: size,
    };
    assert_eq!(pool.calculate_mem_pool_ancestors(&entry, &exact, true).unwrap().len(), 2);
    for (limits, expected) in [
        (
            MemPoolLimits {
                ancestor_count: 1,
                ..exact
            },
            "too many unconfirmed parents",
        ),
        (
            MemPoolLimits {
                ancestor_count: 2,
                ..exact
            },
            "too many unconfirmed ancestors",
        ),
        (
            MemPoolLimits {
                descendant_count: 2,
                ..exact
            },
            "too many descendants",
        ),
        (
            MemPoolLimits {
                descendant_size_vbytes: size - 1,
                ..exact
            },
            "exceeds descendant size limit",
        ),
        (
            MemPoolLimits {
                ancestor_size_vbytes: size - 1,
                ..exact
            },
            "exceeds ancestor size limit",
        ),
    ] {
        assert!(
            pool.calculate_mem_pool_ancestors(&entry, &limits, true)
                .unwrap_err()
                .0
                .starts_with(expected)
        );
        assert!(pool.assume_calculate_mem_pool_ancestors(&entry, &limits, true).is_empty());
    }
    // Core only checks size inside the walk; a parentless entry has no package walk.
    let isolated = pool.new_entry(tx(9, &[]), Default::default());
    assert!(
        pool.calculate_mem_pool_ancestors(
            &isolated,
            &MemPoolLimits {
                ancestor_size_vbytes: 0,
                ..NO_LIMITS
            },
            true
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn vtx_hashes_swap_remove_preserves_compact_order_and_usage() {
    let mut pool = PoolState::default();
    let txs: Vec<_> = (0..9).map(|n| tx(n, &[])).collect();
    for tx in &txs {
        add(&mut pool, tx, 0, 0);
    }
    let mut order: Vec<_> = txs.iter().map(|tx| (tx.instance_hash(), tx.txid())).collect();
    for n in [2, 0, 8, 5, 1, 7, 4, 3, 6] {
        let idx = order.iter().position(|(_, id)| *id == txs[n].txid()).unwrap();
        order.swap_remove(idx);
        pool.remove_recursive(&txs[n], Reason::Manual);
        pool.check();
        assert_eq!(pool.vtx_hashes(), order);
        assert_eq!(pool.dynamic_memory_usage(), pool.recompute_dynamic_memory_usage());
    }
    // clear() keeps the singleton vector's allocation, as Core does.
    assert!(pool.dynamic_memory_usage() > 0);
}

#[test]
fn fee_deltas_update_both_directions_and_survive_recursive_removal() {
    let mut pool = PoolState::default();
    let a = tx(1, &[]);
    let b = tx(2, &[output(&a, 0)]);
    let c = tx(3, &[output(&b, 0)]);
    pool.prioritise_transaction(b.txid(), 50);
    assert_eq!(pool.transactions_updated(), 0);
    add(&mut pool, &a, 10, 1);
    add(&mut pool, &b, 20, 2);
    add(&mut pool, &c, 30, 3);
    pool.prioritise_transaction(b.txid(), -70);
    pool.check();
    assert_eq!(pool.get_iter(&b.txid()).unwrap().modified_fee(), 0);
    assert_eq!(pool.get_iter(&a.txid()).unwrap().descendants().modified_fees, 40);
    assert_eq!(pool.get_iter(&c.txid()).unwrap().ancestors().modified_fees, 40);
    pool.clear_prioritisation(&b.txid());
    pool.check();
    assert_eq!(pool.get_iter(&b.txid()).unwrap().modified_fee(), 0);
    pool.prioritise_transaction(b.txid(), 9);
    pool.remove_recursive(&b, Reason::Manual);
    pool.check();
    assert_eq!(pool.deltas()[&b.txid()], 9);
    add(&mut pool, &b, 20, 2);
    assert_eq!(pool.info(&b.txid()).unwrap().fee_delta, 9);
    let mut delta = 7;
    pool.apply_delta(&b.txid(), &mut delta);
    assert_eq!(delta, 16);
    pool.clear_prioritisation(&b.txid());
    pool.check();
}

#[test]
fn fee_deltas_retain_zero_entries_like_core_v24() {
    let mut pool = PoolState::default();
    let resident = tx(21, &[]);
    add(&mut pool, &resident, 1, 0);

    // Core v24.0.0 txmempool.cpp:1878-1903 retains zero-valued mapDeltas entries.
    pool.prioritise_transaction(resident.txid(), 7);
    pool.prioritise_transaction(resident.txid(), -7);
    assert_eq!(pool.deltas().get(&resident.txid()), Some(&0));

    let absent = TxHash::from([22; 32]);
    pool.prioritise_transaction(absent, 0);
    assert_eq!(pool.deltas().get(&absent), Some(&0));
}

#[test]
fn conflict_removal_clears_only_root_prioritisation() {
    let mut pool = PoolState::default();
    let input = OutPoint::new(TxHash::from([1; 32]), 0);
    let a = tx(1, &[input]);
    let b = tx(2, &[output(&a, 0)]);
    let winner = tx(3, &[input]);
    add(&mut pool, &a, 1, 0);
    add(&mut pool, &b, 1, 0);
    pool.prioritise_transaction(a.txid(), 10);
    pool.prioritise_transaction(b.txid(), 20);
    pool.remove_conflicts(&a);
    assert_eq!(pool.len(), 2);
    pool.remove_conflicts(&winner);
    pool.check();
    assert!(pool.is_empty());
    assert!(!pool.deltas().contains_key(&a.txid()));
    assert_eq!(pool.deltas()[&b.txid()], 20);
    let events = pool.take_pending_events();
    assert_eq!(events.len(), 2);
    for (n, event) in events.iter().enumerate() {
        let PendingEvent::TransactionRemovedFromMempool {
            reason,
            sequence,
            ..
        } = event;
        assert_eq!(*reason, Reason::Conflict);
        assert_eq!(*sequence, n as u64 + 1);
    }
}

#[test]
fn instant_send_snapshot_includes_overlapping_roots_and_deduplicates() {
    let pool = Mempool::default();
    let input = OutPoint::new(TxHash::from([1; 32]), 0);
    let a = tx(1, &[input]);
    let b = tx(2, &[output(&a, 0)]);
    let c = tx(3, &[output(&b, 0)]);
    let locked = tx(4, &[]);
    {
        let mut state = pool.write();
        for tx in [&a, &b, &c, &locked] {
            add(&mut state, tx, 1, 0);
        }
    }
    let port: &dyn IsMempool = &pool;
    let roots = port.remove_conflicts_for_lock(&locked.txid(), &[input, output(&a, 0), input]);
    assert_eq!(
        roots.iter().map(|tx| tx.txid()).collect::<BTreeSet<_>>(),
        BTreeSet::from([a.txid(), b.txid()])
    );
    assert!(port.exists(&locked.txid()));
    assert!(!port.exists(&a.txid()));
    assert!(!port.exists(&c.txid()));
    assert!(port.spender_of(&input).is_none());
    port.with_read(&mut |read| {
        assert_eq!(read.get(&locked.txid()).unwrap().txid(), locked.txid());
    });
    pool.read().check();
    assert_eq!(pool.take_pending_events().len(), 3);
    assert!(port.remove_conflicts_for_lock(&locked.txid(), &[input]).is_empty());
    port.add_transactions_updated(u32::MAX);
    assert_eq!(pool.read().transactions_updated(), 6);
}

#[test]
fn sequence_counter_info_and_relay_order_hide_prioritisation() {
    let mut pool = PoolState::default();
    let a = tx(1, &[]);
    let b = tx(2, &[]);
    let c = tx(3, &[output(&a, 0)]);
    add(&mut pool, &a, 10, 0);
    add(&mut pool, &b, 20, 0);
    add(&mut pool, &c, 100, 0);
    assert_eq!(pool.get_sorted_depth_and_score(), [b.txid(), a.txid(), c.txid()]);
    let relay_order = pool.get_sorted_depth_and_score();
    pool.prioritise_transaction(a.txid(), 1000);
    assert_eq!(pool.get_sorted_depth_and_score(), relay_order);
    assert_eq!(pool.ancestor_score_order().next(), Some(a.txid()));
    assert_eq!(pool.info_all().iter().map(|info| info.tx.txid()).collect::<Vec<_>>(), relay_order);
    let absent = TxHash::from([99; 32]);
    assert!(pool.compare_depth_and_score(&absent, &a.txid()));
    assert!(!pool.compare_depth_and_score(&a.txid(), &absent));
    assert!(pool.info(&absent).is_none());
    assert_eq!(pool.get_iter_set(&BTreeSet::from([a.txid(), absent])), BTreeSet::from([a.txid()]));
    assert_eq!(pool.get_and_increment_sequence(), 1);
    assert_eq!(pool.sequence(), 2);
    pool.remove_staged(&BTreeSet::from([a.txid()]), true, Reason::Block);
    assert_eq!(pool.sequence(), 3);
    assert!(pool.take_pending_events().is_empty());
    pool.check();
}

#[derive(Default)]
struct IndexLog {
    inserted: usize,
    updated: usize,
    removed: usize,
}
struct CountingIndex(Arc<Mutex<IndexLog>>);
impl PoolIndex for CountingIndex {
    fn on_insert(&mut self, _: &PoolEntry) {
        self.0.lock().unwrap().inserted += 1;
    }
    fn on_update(&mut self, _: &PoolEntry, _: &PoolEntry) {
        self.0.lock().unwrap().updated += 1;
    }
    fn on_remove(&mut self, _: &PoolEntry) {
        self.0.lock().unwrap().removed += 1;
    }
}
#[test]
fn extension_index_receives_existing_new_updated_and_removed_entries() {
    let mut pool = PoolState::default();
    let a = tx(1, &[]);
    let b = tx(2, &[output(&a, 0)]);
    add(&mut pool, &a, 1, 0);
    let log = Arc::new(Mutex::new(IndexLog::default()));
    pool.register_index(Box::new(CountingIndex(log.clone())));
    add(&mut pool, &b, 1, 0);
    pool.prioritise_transaction(a.txid(), 7);
    pool.remove_recursive(&a, Reason::SizeLimit);
    pool.check();
    let log = log.lock().unwrap();
    assert_eq!(log.inserted, 2);
    assert!(log.updated > 0);
    assert_eq!(log.removed, 2);
}

#[test]
fn coin_check_walk_uses_pool_parents_and_propagates_storage_faults() {
    use dash_consensus::coins::{Coin, CoinRef, CoinWrite, CoinsView, MemoryCoins};
    use dash_primitives::BlockHash;
    let input = OutPoint::new(TxHash::from([11; 32]), 0);
    let a = tx(1, &[input]);
    let b = Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        vec![TxIn::new(output(&a, 0), vec![].into(), u32::MAX)],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        2,
        vec![],
    )));
    let mut pool = PoolState::default();
    add(&mut pool, &a, 6, 0);
    add(&mut pool, &b, 0, 0);
    let mut coins = MemoryCoins::new();
    coins.write(input, CoinWrite::Put(Coin::new(10, &[0x51], 1, false)));
    pool.check_with_coins(&coins, 100).unwrap();
    struct Fault;
    impl CoinsView for Fault {
        type Error = &'static str;
        fn coin(&self, _: &OutPoint) -> Result<Option<CoinRef<'_>>, Self::Error> {
            Err("disk fault")
        }
        fn best_block(&self) -> Option<BlockHash> {
            None
        }
    }
    assert_eq!(pool.check_with_coins(&Fault, 100), Err("disk fault"));
    let empty = MemoryCoins::new();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || pool.check_with_coins(&empty, 100)
        ))
        .is_err()
    );
}

#[test]
fn recursive_removal_of_missing_transaction_only_checks_its_outputs() {
    let mut pool = PoolState::default();
    let missing = tx(1, &[]);
    let child = tx(2, &[output(&missing, 0)]);
    let grandchild = tx(3, &[output(&child, 0)]);
    let unrelated = tx(4, &[]);
    for tx in [&child, &grandchild, &unrelated] {
        add(&mut pool, tx, 1, 0);
    }
    pool.remove_recursive(&missing, Reason::Expiry);
    pool.check();
    assert_eq!(pool.len(), 1);
    assert!(pool.exists(&unrelated.txid()));
    for event in pool.take_pending_events() {
        let PendingEvent::TransactionRemovedFromMempool {
            reason,
            ..
        } = event;
        assert_eq!(reason, Reason::Expiry);
    }
}

#[test]
fn equal_fee_ties_use_opposite_hash_directions_for_relay_and_ancestor_order() {
    let mut pool = PoolState::default();
    let a = tx(1, &[]);
    let b = tx(2, &[]);
    add(&mut pool, &a, 10, 0);
    add(&mut pool, &b, 10, 0);
    let mut ids = [a.txid(), b.txid()];
    ids.sort();
    assert_eq!(pool.ancestor_score_order().collect::<Vec<_>>(), ids);
    ids.reverse();
    assert_eq!(pool.get_sorted_depth_and_score(), ids);
    let order = pool.get_sorted_depth_and_score();
    pool.prioritise_transaction(order[1], 1);
    assert_eq!(pool.get_sorted_depth_and_score(), order);
    assert_eq!(pool.ancestor_score_order().next(), Some(order[1]));
}

#[test]
fn prioritisation_saturates_and_counters_wrap() {
    let mut pool = PoolState::default();
    let a = tx(1, &[]);
    add(&mut pool, &a, 1, 0);
    pool.prioritise_transaction(a.txid(), i64::MAX);
    pool.prioritise_transaction(a.txid(), 1);
    assert_eq!(pool.deltas()[&a.txid()], i64::MAX);
    assert_eq!(pool.get_iter(&a.txid()).unwrap().modified_fee(), i64::MAX);
    pool.check();
    pool.prioritise_transaction(a.txid(), i64::MIN);
    pool.prioritise_transaction(a.txid(), i64::MIN);
    assert_eq!(pool.get_iter(&a.txid()).unwrap().modified_fee(), i64::MIN);
    pool.check();
    let before = pool.transactions_updated();
    pool.add_transactions_updated(u32::MAX);
    assert_eq!(pool.transactions_updated(), before - 1);
}

#[test]
fn compact_hashes_use_instance_hash_for_stable_txids() {
    let mut payload = vec![1; 145];
    payload[0] = 2;
    let unlock = Arc::new(Tx::new(Transaction::new(
        3,
        TxType::new(9),
        vec![],
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        0,
        payload,
    )));
    assert!(unlock.is_asset_unlock_with_stable_txid());
    assert_ne!(unlock.txid(), unlock.instance_hash());
    let mut pool = PoolState::default();
    add(&mut pool, &unlock, 1, 0);
    assert_eq!(pool.vtx_hashes(), [(unlock.instance_hash(), unlock.txid())]);
    pool.remove_recursive(&unlock, Reason::Manual);
    pool.check();
}

#[test]
fn logical_usage_matches_core_allocation_formula() {
    let mut pool = PoolState::default();
    let a = tx(1, &[]);
    let b = tx(2, &[output(&a, 0)]);
    assert_eq!(pool.dynamic_memory_usage(), 0);
    add(&mut pool, &a, 1, 0);
    assert_eq!(pool.dynamic_memory_usage(), 832);
    add(&mut pool, &b, 1, 0);
    assert_eq!(pool.dynamic_memory_usage(), 1920);
    pool.prioritise_transaction(b.txid(), 7);
    pool.check();
    assert_eq!(pool.dynamic_memory_usage(), 2016);
    pool.remove_staged(&BTreeSet::from([a.txid()]), true, Reason::Block);
    pool.check();
    assert_eq!(pool.dynamic_memory_usage(), 1120);
    pool.remove_recursive(&b, Reason::Manual);
    pool.check();
    assert_eq!(pool.dynamic_memory_usage(), 192);
    pool.clear_prioritisation(&b.txid());
    pool.check();
    assert_eq!(pool.dynamic_memory_usage(), 96);
}

#[test]
fn unbroadcast_set_tracks_only_resident_transactions_and_returns_a_snapshot() {
    let pool = Mempool::default();
    assert_eq!(pool.size(), 0);
    let a = tx(801, &[]);
    let b = tx(802, &[]);
    pool.add_unbroadcast_tx(a.txid());
    assert!(pool.get_unbroadcast_txs().is_empty());
    {
        let mut state = pool.write();
        add(&mut state, &a, 1, 0);
        add(&mut state, &b, 1, 0);
    }
    assert!(pool.get_unbroadcast_txs().is_empty());
    assert_eq!(pool.size(), 2);
    let usage = pool.read().dynamic_memory_usage();
    let updated = pool.read().transactions_updated();
    let sequence = pool.read().sequence();
    pool.add_unbroadcast_tx(a.txid());
    pool.add_unbroadcast_tx(a.txid());
    pool.add_unbroadcast_tx(b.txid());
    assert_eq!(pool.read().dynamic_memory_usage(), usage);
    assert_eq!(pool.read().transactions_updated(), updated);
    assert_eq!(pool.read().sequence(), sequence);
    let snapshot = pool.get_unbroadcast_txs();
    assert_eq!(snapshot, BTreeSet::from([a.txid(), b.txid()]));
    assert!(pool.is_unbroadcast_tx(&a.txid()));
    pool.remove_unbroadcast_tx(&a.txid(), false);
    assert!(!pool.is_unbroadcast_tx(&a.txid()));
    assert_eq!(snapshot.len(), 2);
    pool.remove_unbroadcast_tx(&a.txid(), true);
    pool.remove_unbroadcast_tx(&b.txid(), true);
    assert!(pool.get_unbroadcast_txs().is_empty());
    pool.add_unbroadcast_tx(a.txid());
    pool.add_unbroadcast_tx(b.txid());
    {
        let mut state = pool.write();
        state.remove_staged(&BTreeSet::from([a.txid()]), true, Reason::Block);
        state.remove_recursive(&b, Reason::Manual);
        state.check();
        assert_eq!(state.size(), 0);
        assert!(state.get_unbroadcast_txs().is_empty());
    }
}

#[test]
fn transactions_updated_read_does_not_wait_for_pool_write_guard() {
    let pool = Mempool::default();
    let _write_guard = pool.write();
    assert_eq!(pool.transactions_updated(), 0);
}

#[test]
fn info_exposes_entry_time_and_info_all_uses_depth_and_score_order() {
    let pool = Mempool::default();
    let a = tx(811, &[]);
    let b = tx(812, &[]);
    let c = tx(813, &[output(&a, 0)]);
    {
        let mut state = pool.write();
        for (tx, time, fee) in [(&a, 31, 10), (&b, 17, 20), (&c, 9, 100)] {
            let entry = state.new_entry(
                tx.clone(),
                EntryMetadata {
                    time,
                    fee,
                    ..Default::default()
                },
            );
            state.add_unchecked(entry);
        }
        state.prioritise_transaction(a.txid(), 77);
    }
    let info = pool.info(&a.txid()).unwrap();
    assert_eq!(info.tx.txid(), a.txid());
    assert_eq!(info.time, 31);
    assert_eq!(info.fee, 10);
    assert_eq!(info.fee_delta, 77);
    assert_eq!(info.vsize, u64::from(a.size()));
    assert!(pool.info(&TxHash::from([87; 32])).is_none());
    let infos = pool.info_all();
    assert_eq!(
        infos.iter().map(|info| info.tx.txid()).collect::<Vec<_>>(),
        pool.get_sorted_depth_and_score()
    );
    assert_eq!(infos.iter().map(|info| info.time).collect::<Vec<_>>(), [17, 31, 9]);
    assert_eq!(pool.size(), 3);
    assert!(pool.compare_depth_and_score(&b.txid(), &a.txid()));
    pool.read().check();
}

#[test]
fn depth_score_uses_double_products_and_descending_txid_ties() {
    let mut pool = PoolState::default();
    let a = tx(821, &[]);
    let b = tx(822, &[]);
    let (low, high) = if a.txid() < b.txid() {
        (a, b)
    } else {
        (b, a)
    };
    // The extra duff is lost when the fee is converted to double, so Core breaks the tie by hash.
    add(&mut pool, &low, (1i64 << 53) + 1, 0);
    add(&mut pool, &high, 1i64 << 53, 0);
    assert!(pool.compare_depth_and_score(&high.txid(), &low.txid()));
    assert!(!pool.compare_depth_and_score(&low.txid(), &high.txid()));
    assert!(!pool.compare_depth_and_score(&low.txid(), &low.txid()));
    assert_eq!(pool.get_sorted_depth_and_score(), [high.txid(), low.txid()]);
    let absent = TxHash::from([88; 32]);
    assert!(pool.compare_depth_and_score(&absent, &low.txid()));
    assert!(!pool.compare_depth_and_score(&low.txid(), &absent));
    assert!(!pool.compare_depth_and_score(&absent, &absent));
}

#[test]
fn mem_pool_parents_const_is_unique_and_sorted_by_txid() {
    let mut pool = PoolState::default();
    let a = tx(831, &[]);
    let b = tx(832, &[]);
    add(&mut pool, &a, 1, 0);
    add(&mut pool, &b, 1, 0);
    let (low, high) = if a.txid() < b.txid() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let child = tx(833, &[output(high, 0), output(low, 0), output(high, 1)]);
    add(&mut pool, &child, 1, 0);
    let entry = pool.get_iter(&child.txid()).unwrap();
    let mut expected = [a.txid(), b.txid()];
    expected.sort();
    assert_eq!(entry.get_mem_pool_parents_const().iter().copied().collect::<Vec<_>>(), expected);
    assert_eq!(entry.parents(), entry.get_mem_pool_parents_const());
    assert_eq!(pool.size(), pool.len());
    pool.check();
}
