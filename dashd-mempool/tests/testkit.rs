use std::collections::BTreeSet;
use std::sync::Arc;

use dash_consensus::{Tx, TxRef};
use dash_num::Hash256;
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dashd_mempool::is::{IsAcceptHooks, IsMempool};
use dashd_mempool::testkit::{FakeIsHooks, FakeIsMempool};
use dashd_mempool::types::IsConflict;

fn tx(n: u32, inputs: &[OutPoint]) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        inputs.iter().map(|outpoint| TxIn::new(*outpoint, vec![].into(), u32::MAX)).collect(),
        vec![TxOut::from_duffs(1, vec![0x51].into())],
        n,
        vec![],
    )))
}

#[test]
fn fake_pool_lookups_and_recursive_conflict_removal() {
    let fake = FakeIsMempool::default();
    let pool: &dyn IsMempool = &fake;
    let input = OutPoint::new(TxHash::from([1; 32]), 0);
    let root = tx(1, &[input]);
    let child = tx(2, &[OutPoint::new(root.txid(), 0)]);
    let grandchild = tx(3, &[OutPoint::new(child.txid(), 0)]);
    let locked = tx(4, &[]);
    let other_unlock = tx(5, &[]);
    let unlock_child = tx(6, &[OutPoint::new(other_unlock.txid(), 0)]);
    let unrelated = tx(7, &[]);
    for tx in [&root, &child, &grandchild, &locked, &other_unlock, &unlock_child, &unrelated] {
        fake.insert(tx.clone());
    }
    assert!(fake.set_asset_unlock(&locked.txid(), 42, 100));
    assert!(fake.set_asset_unlock(&other_unlock.txid(), 42, 200));
    assert!(!fake.set_asset_unlock(&TxHash::from([9; 32]), 42, 200));
    assert!(Arc::ptr_eq(&pool.get(&root.txid()).unwrap(), &root));
    assert_eq!(pool.spender_of(&input).unwrap().txid(), root.txid());
    assert_eq!(pool.pending_asset_unlock_amount(), 300);
    assert_eq!(pool.asset_unlock_txids_by_index(42).len(), 2);
    let mut calls = 0;
    pool.with_read(&mut |read| {
        calls += 1;
        assert!(read.exists(&locked.txid()));
        assert_eq!(read.get(&child.txid()).unwrap().txid(), child.txid());
        assert_eq!(read.spender_of(&input).unwrap().txid(), root.txid());
        assert_eq!(read.asset_unlock_txids_by_index(42).len(), 2);
        assert_eq!(read.pending_asset_unlock_amount(), 300);
    });
    assert_eq!(calls, 1);
    let roots = pool.remove_conflicts_for_lock(&locked.txid(), &[input, input]);
    assert_eq!(roots.iter().map(|tx| tx.txid()).collect::<Vec<_>>(), [root.txid()]);
    for removed in [&root, &child, &grandchild, &other_unlock, &unlock_child] {
        assert!(!pool.exists(&removed.txid()));
    }
    assert!(pool.exists(&locked.txid()));
    assert!(pool.exists(&unrelated.txid()));
    assert!(pool.spender_of(&input).is_none());
    assert_eq!(pool.asset_unlock_txids_by_index(42), [locked.txid()]);
    assert_eq!(pool.pending_asset_unlock_amount(), 100);
    assert!(pool.remove_conflicts_for_lock(&locked.txid(), &[input]).is_empty());
    pool.add_transactions_updated(2);
    pool.add_transactions_updated(3);
    assert_eq!(fake.transactions_updated(), 5);
    pool.add_transactions_updated(u32::MAX);
    assert_eq!(fake.transactions_updated(), 4);
}

#[test]
fn fake_pool_returns_snapshot_roots_even_when_one_descends_from_another() {
    let inputs = [OutPoint::new(TxHash::from([1; 32]), 0), OutPoint::new(TxHash::from([2; 32]), 0)];
    for lock_inputs in [[inputs[0], inputs[1]], [inputs[1], inputs[0]]] {
        let pool = FakeIsMempool::default();
        let parent = tx(1, &inputs[..1]);
        let child = tx(2, &[OutPoint::new(parent.txid(), 0), inputs[1]]);
        pool.insert(parent.clone());
        pool.insert(child.clone());
        let locked = tx(3, &lock_inputs);
        let roots = pool.remove_conflicts_for_lock(&locked.txid(), &lock_inputs);
        assert_eq!(
            roots.iter().map(|root| root.txid()).collect::<BTreeSet<_>>(),
            BTreeSet::from([parent.txid(), child.txid()]),
            "{lock_inputs:?}"
        );
        assert!(!pool.exists(&parent.txid()));
        assert!(!pool.exists(&child.txid()));
    }
}

#[test]
fn fake_pool_preserves_locked_spender_and_caps_only_the_pending_view() {
    let pool = FakeIsMempool::default();
    let input = OutPoint::new(TxHash::from([1; 32]), 0);
    let locked = tx(1, &[input]);
    let other = tx(2, &[]);
    pool.insert(locked.clone());
    pool.insert(other.clone());
    assert!(!pool.set_asset_unlock(&locked.txid(), 1, -1));
    assert!(pool.set_asset_unlock(&locked.txid(), 1, 100));
    assert!(pool.set_asset_unlock(&other.txid(), 1, i64::MAX));
    assert_eq!(pool.pending_asset_unlock_amount(), i64::MAX);
    pool.with_read(&mut |read| assert_eq!(read.pending_asset_unlock_amount(), i64::MAX));
    assert!(pool.remove_conflicts_for_lock(&locked.txid(), &[input]).is_empty());
    assert!(pool.exists(&locked.txid()));
    assert!(!pool.exists(&other.txid()));
    assert_eq!(pool.pending_asset_unlock_amount(), 100);
}

#[test]
fn fake_hooks_return_scripted_answers() {
    let candidate = tx(1, &[]);
    let other = tx(2, &[]);
    let conflict = IsConflict {
        lock_hash: Hash256::from([3; 32]),
        txid: other.txid(),
    };
    let mut fake = FakeIsHooks::default();
    assert!(!fake.is_enabled());
    fake.enabled = true;
    fake.conflicts.insert(candidate.txid(), conflict);
    fake.waiting.insert(candidate.txid());
    fake.locked.insert(other.txid());
    let hooks: &dyn IsAcceptHooks = &fake;
    assert!(hooks.is_enabled());
    assert_eq!(hooks.conflicting_lock(&candidate), Some(conflict));
    assert!(hooks.is_waiting_for_tx(&candidate.txid()));
    assert!(hooks.is_locked(&other.txid()));
    assert_eq!(hooks.conflicting_lock(&other), None);
    assert!(!hooks.is_waiting_for_tx(&other.txid()));
    assert!(!hooks.is_locked(&candidate.txid()));
}
