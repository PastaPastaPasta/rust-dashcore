//! P3's G5 relay reads against a live pool, through a locked [`PoolState`], the [`Mempool`]'s
//! short read locks and the handle's one-lock view.

use std::cmp::Ordering;
use std::sync::Arc;

use dash_consensus::{Tx, TxRef};
use dash_dml::MnList;
use dash_pkc::bls::BlsGlobalSig;
use dash_primitives::{
    AssetUnlock, OutPoint, QuorumHash, Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_types::codec::BaseCodec;

use crate::api::{MempoolHandle, RelayRead};
use crate::pool::{EntryMetadata, Mempool, PoolState};

const OP_TRUE: u8 = 0x51;

fn spend(inputs: &[OutPoint], tag: i64) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        2,
        TxType::Spend,
        inputs.iter().map(|&prevout| TxIn::new(prevout, vec![].into(), u32::MAX)).collect(),
        vec![TxOut::from_duffs(1_000 + tag, vec![OP_TRUE].into()); 2],
        0,
        vec![],
    )))
}

fn coin(byte: u8) -> OutPoint {
    OutPoint::new(TxHash::from([byte; 32]), 0)
}

fn out(tx: &Tx, n: u32) -> OutPoint {
    OutPoint::new(tx.txid(), n)
}

/// A version-`version` asset unlock whose signature bytes are `sig`: version 2 keeps its txid
/// across signatures (`IsAssetUnlockWithStableTxid`).
fn unlock(version: u8, index: u64, sig: u8) -> TxRef {
    let sig = BlsGlobalSig::decode(&[sig; 96]).expect("decodes");
    let p = AssetUnlock::new(version, index, 1_000, 100, QuorumHash::from([1; 32]), sig);
    let mut payload = Vec::new();
    p.encode(&mut payload);
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::AssetUnlock,
        vec![],
        vec![TxOut::from_duffs(1_000, vec![OP_TRUE].into())],
        0,
        payload,
    )))
}

fn add(state: &mut PoolState, tx: &TxRef, fee: i64, time: i64) {
    let metadata = EntryMetadata {
        fee,
        time,
        ..EntryMetadata::default()
    };
    let entry = state.new_entry(tx.clone(), metadata);
    state.add_unchecked(entry, &MnList::default_pre_dip3());
}

fn txids(infos: &[crate::api::TxMempoolInfo]) -> Vec<TxHash> {
    infos.iter().map(|info| info.tx.txid()).collect()
}

/// Two equal-score roots by descending txid (`txmempool.h:131-143`).
fn by_descending_txid(a: &TxRef, c: &TxRef) -> [TxHash; 2] {
    let (a, c) = (a.txid(), c.txid());
    if a > c {
        [a, c]
    } else {
        [c, a]
    }
}

/// `infoAll`'s `GetSortedDepthAndScore` order (`txmempool.cpp:1515-1561`): ascending ancestor
/// count, then descending `CompareTxMemPoolEntryByScore` on the original fee (a prioritisation
/// never moves an entry, `txmempool.h:125-130`), then descending txid; `CompareDepthAndScore`
/// (`txmempool.cpp:1495-1513`) agrees, absent before present and two absent equal; `info`
/// (`:1572-1579`) carries the admission time.
#[test]
fn info_all_and_compare_depth_and_score_follow_cores_order_on_the_original_fee() {
    let pool = Mempool::default();
    let a = spend(&[coin(1)], 0);
    let b = spend(&[coin(2)], 0);
    let c = spend(&[coin(3)], 0);
    let d = spend(&[out(&a, 0)], 0);
    {
        let mut state = pool.write();
        add(&mut state, &a, 1_000, 10);
        add(&mut state, &b, 5_000, 20);
        add(&mut state, &c, 1_000, 30);
        add(&mut state, &d, 100_000, 40);
        state.prioritise_transaction(c.txid(), 1_000_000);
    }
    let [first, second] = by_descending_txid(&a, &c);
    let expected = [b.txid(), first, second, d.txid()];
    assert_eq!(txids(&RelayRead::info_all(&pool)), expected);
    assert_eq!(txids(&RelayRead::info_all(&*pool.read())), expected);

    let info = RelayRead::info(&pool, &c.txid()).expect("resident");
    assert_eq!((info.tx.txid(), info.time_s), (c.txid(), 30));
    let absent = TxHash::from([0xee; 32]);
    assert!(RelayRead::info(&pool, &absent).is_none());

    for pair in expected.windows(2) {
        assert_eq!(RelayRead::compare_depth_and_score(&pool, &pair[0], &pair[1]), Ordering::Less);
        assert_eq!(
            RelayRead::compare_depth_and_score(&pool, &pair[1], &pair[0]),
            Ordering::Greater
        );
    }
    let cmp = |x: &TxHash, y: &TxHash| RelayRead::compare_depth_and_score(&pool, x, y);
    assert_eq!(cmp(&absent, &d.txid()), Ordering::Less);
    assert_eq!(cmp(&d.txid(), &absent), Ordering::Greater);
    assert_eq!(cmp(&absent, &TxHash::from([0xef; 32])), Ordering::Equal);
    assert_eq!(cmp(&b.txid(), &b.txid()), Ordering::Equal);
}

/// `ProcessGetData`'s parent walk (`net_processing.cpp:2996-3010`): direct parents only, in
/// `GetMemPoolParentsConst` order (ascending txid), admitted strictly after `since_s`; nothing for
/// an absent txid.
#[test]
fn parents_entered_after_lists_direct_parents_strictly_newer_in_ascending_txid() {
    let pool = Mempool::default();
    let a = spend(&[coin(1)], 0);
    let c = spend(&[coin(3)], 0);
    let e = spend(&[out(&a, 1), out(&c, 1)], 0);
    let grandchild = spend(&[out(&e, 0)], 0);
    {
        let mut state = pool.write();
        add(&mut state, &a, 1_000, 10);
        add(&mut state, &c, 1_000, 30);
        add(&mut state, &e, 1_000, 40);
        add(&mut state, &grandchild, 1_000, 50);
    }
    let mut both = [a.txid(), c.txid()];
    both.sort();
    assert_eq!(pool.parents_entered_after(&e.txid(), 5), both);
    assert_eq!(pool.parents_entered_after(&e.txid(), 10), [c.txid()]);
    assert_eq!(pool.parents_entered_after(&e.txid(), 30), Vec::<TxHash>::new());
    assert_eq!(pool.parents_entered_after(&grandchild.txid(), 0), [e.txid()]);
    assert!(pool.parents_entered_after(&TxHash::from([0xee; 32]), 0).is_empty());
}

/// `GetAssetUnlockByInstanceHash` (`txmempool.cpp:852-858`) finds only version-2+ unlocks
/// (`:677-684`), and follows `ReplaceAssetUnlockInstance` (`:860-875`), which keeps the entry's
/// time and its `vTxHashes` slot rekeyed (`:871`).
#[test]
fn asset_unlock_instance_lookup_is_v2_only_and_follows_replacement() {
    let pool = Mempool::default();
    let v1 = unlock(1, 1, 0);
    let old = unlock(2, 2, 0);
    let fresh = unlock(2, 2, 1);
    assert_eq!(old.txid(), fresh.txid());
    {
        let mut state = pool.write();
        add(&mut state, &v1, 1_000, 10);
        add(&mut state, &old, 1_000, 20);
    }
    assert_eq!(pool.asset_unlock_by_instance_hash(&v1.instance_hash()), None);
    assert_eq!(pool.asset_unlock_by_instance_hash(&v1.txid()), None);
    assert_eq!(pool.asset_unlock_by_instance_hash(&old.instance_hash()), Some(old.txid()));

    pool.write().replace_asset_unlock_instance(old.txid(), fresh.clone());
    assert_eq!(pool.asset_unlock_by_instance_hash(&old.instance_hash()), None);
    assert_eq!(pool.asset_unlock_by_instance_hash(&fresh.instance_hash()), Some(old.txid()));
    let info = RelayRead::info(&pool, &old.txid()).expect("resident");
    assert_eq!((info.tx.instance_hash(), info.time_s), (fresh.instance_hash(), 20));
    let mut slots = Vec::new();
    RelayRead::for_each_in_vtxhashes_order(&pool, &mut |instance, tx| {
        slots.push((*instance, tx.txid()));
        true
    });
    assert_eq!(slots, [(v1.instance_hash(), v1.txid()), (fresh.instance_hash(), old.txid())]);
}

/// `AddUnbroadcastTx` only for a resident txid (`txmempool.h:773-780`), `RemoveUnbroadcastTx`
/// (`txmempool.cpp:1990-1997`), `GetUnbroadcastTxs` ascending (`txmempool.h:785-790`); `size`
/// (`:702-706`) and `DynamicMemoryUsage` (`txmempool.cpp:1984-1988`) as the pool keeps them; the
/// handle's `with_relay_read` runs once under one lock and sees the same.
#[test]
fn unbroadcast_size_usage_and_the_handle_view_read_the_live_pool() {
    let pool = Arc::new(Mempool::default());
    let txs: Vec<_> = (1..=3).map(|n| spend(&[coin(n)], 0)).collect();
    {
        let mut state = pool.write();
        for (n, tx) in txs.iter().enumerate() {
            add(&mut state, tx, 1_000, n as i64);
        }
    }
    let handle: Arc<dyn MempoolHandle> = pool.clone();
    for tx in txs.iter().rev() {
        handle.add_unbroadcast(&tx.txid());
    }
    handle.add_unbroadcast(&TxHash::from([0xee; 32]));
    let mut all: Vec<_> = txs.iter().map(|tx| tx.txid()).collect();
    all.sort();
    assert_eq!(RelayRead::unbroadcast(&*pool), all);
    handle.remove_unbroadcast(&all[1], true);
    handle.remove_unbroadcast(&all[1], false);
    assert_eq!(RelayRead::unbroadcast(&*pool), [all[0], all[2]]);

    let state = pool.read();
    let (size, usage) = (state.size(), state.dynamic_memory_usage());
    assert_eq!(usage, state.recompute_dynamic_memory_usage());
    drop(state);
    assert_eq!((RelayRead::size(&*pool), RelayRead::dynamic_memory_usage(&*pool)), (3, usage));
    let mut runs = 0;
    let seen = handle.read(|read| {
        runs += 1;
        let mut walked = Vec::new();
        read.for_each_in_vtxhashes_order(&mut |_, tx| {
            walked.push(tx.txid());
            true
        });
        (read.size(), read.dynamic_memory_usage(), read.unbroadcast(), walked)
    });
    assert_eq!(runs, 1);
    let order: Vec<_> = txs.iter().map(|tx| tx.txid()).collect();
    assert_eq!(seen, (size, usage, vec![all[0], all[2]], order));
}
