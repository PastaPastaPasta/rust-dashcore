//! The mining reads (R2–R5, R9) on a locked [`PoolState`] and through the [`Mempool`]'s short
//! read locks.

use std::cmp::Ordering;
use std::sync::Arc;

use dash_consensus::primitives::core_hash_cmp;
use dash_consensus::{Tx, TxRef};
use dash_dml::MnList;
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};

use crate::api::{MempoolRead, MiningEntry};
use crate::pool::{EntryMetadata, Mempool, PoolState};

fn spend(inputs: &[OutPoint], tag: i64) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        2,
        TxType::Spend,
        inputs.iter().map(|&prevout| TxIn::new(prevout, vec![].into(), u32::MAX)).collect(),
        vec![TxOut::from_duffs(1_000 + tag, vec![0x51].into())],
        0,
        vec![],
    )))
}

fn coin(byte: u8) -> OutPoint {
    OutPoint::new(TxHash::from([byte; 32]), 0)
}

fn add(state: &mut PoolState, tx: &TxRef, fee: i64, sigops: i64) {
    let metadata = EntryMetadata {
        fee,
        sigops,
        ..EntryMetadata::default()
    };
    let entry = state.new_entry(tx.clone(), metadata);
    state.add_unchecked(entry, &MnList::default_pre_dip3());
}

fn sorted(mut txids: Vec<TxHash>) -> Vec<TxHash> {
    txids.sort();
    txids
}

fn fields(e: &MiningEntry) -> (TxHash, i64, i64, u64, i64, u64, i64, i64, u64) {
    (
        e.txid,
        e.fee,
        e.modified_fee,
        e.tx_size,
        e.sigops,
        e.size_with_ancestors,
        e.mod_fees_with_ancestors,
        e.sigops_with_ancestors,
        e.count_with_ancestors,
    )
}

/// A chain `p → c → g` and an unrelated root `r`.
fn chain(pool: &Mempool) -> [TxRef; 4] {
    let p = spend(&[coin(1)], 0);
    let c = spend(&[OutPoint::new(p.txid(), 0)], 0);
    let g = spend(&[OutPoint::new(c.txid(), 0)], 0);
    let r = spend(&[coin(2)], 0);
    let mut state = pool.write();
    add(&mut state, &p, 100, 1);
    add(&mut state, &c, 300, 2);
    add(&mut state, &g, 50, 0);
    add(&mut state, &r, 1_000, 1);
    state.prioritise_transaction(c.txid(), 40);
    [p, c, g, r]
}

/// R4 leaves the entry out and R5 keeps it in; R3 carries Core's cached ancestor fields; R9 is
/// presence. The `Mempool`'s short reads answer as the locked state.
// Core: v24.0.2 src/txmempool.cpp:264-276, :883-905, :1927-1932; src/kernel/mempool_entry.h:133-178
#[test]
fn ancestors_exclude_and_descendants_include_the_entry() {
    let pool = Mempool::default();
    let [p, c, g, r] = chain(&pool);
    let absent = coin(9).txid();
    let state = pool.read();
    for read in [&*state as &dyn MempoolRead, &pool] {
        assert_eq!(sorted(read.ancestors_no_limits(&g.txid())), sorted(vec![p.txid(), c.txid()]));
        assert_eq!(read.ancestors_no_limits(&p.txid()), Vec::<TxHash>::new());
        assert_eq!(read.ancestors_no_limits(&absent), Vec::<TxHash>::new());
        assert_eq!(sorted(read.descendants(&p.txid())), sorted(vec![p.txid(), c.txid(), g.txid()]));
        assert_eq!(read.descendants(&g.txid()), vec![g.txid()]);
        assert_eq!(read.descendants(&r.txid()), vec![r.txid()]);
        assert_eq!(read.descendants(&absent), Vec::<TxHash>::new());
        assert!(read.exists(&c.txid()) && !read.exists(&absent));
        assert!(read.mining_entry(&absent).is_none());

        let e = read.mining_entry(&c.txid()).expect("c is in the pool");
        assert!(Arc::ptr_eq(&e.tx, &c));
        let (vp, vc) = (u64::from(p.size()), u64::from(c.size()));
        assert_eq!(fields(&e), (c.txid(), 300, 340, vc, 2, vp + vc, 440, 3, 2));
        let e = read.mining_entry(&g.txid()).expect("g is in the pool");
        assert_eq!(e.count_with_ancestors, 3);
        assert_eq!(e.mod_fees_with_ancestors, 100 + 340 + 50);
    }
}

/// R2 is best first by the lower of the entry's own and its with-ancestors feerate; equal
/// scores in ascending raw-byte txid order (`core_hash_cmp`), for a pair whose byte-reversed
/// (display) order is the other one.
// Core: v24.0.2 src/txmempool.h:158-197, src/uint256.h:55-59
#[test]
fn ancestor_score_is_best_first_with_the_core_hash_tie() {
    let reversed = |t: &TxRef| {
        let mut b = *t.txid().as_bytes();
        b.reverse();
        b
    };
    let roots: Vec<TxRef> = (10..18).map(|byte| spend(&[coin(byte)], 0)).collect();
    let (a, b) = roots
        .iter()
        .flat_map(|a| roots.iter().map(move |b| (a, b)))
        .find(|(a, b)| {
            core_hash_cmp(a.txid().as_bytes(), b.txid().as_bytes()) == Ordering::Less
                && core_hash_cmp(&reversed(a), &reversed(b)) == Ordering::Greater
        })
        .expect("a pair whose byte and display orders differ");
    let a2 = spend(&[OutPoint::new(a.txid(), 0)], 0);
    let rich = spend(&[coin(4)], 0);
    let mid = spend(&[coin(5)], 0);
    let pool = Mempool::default();
    {
        let mut state = pool.write();
        add(&mut state, b, 500, 0);
        add(&mut state, a, 500, 0);
        add(&mut state, &rich, 5_000, 0);
        add(&mut state, &mid, 575, 0);
        // a2's own feerate is above mid's; its package's, with a, is below.
        add(&mut state, &a2, 600, 0);
    }
    let want = vec![rich.txid(), mid.txid(), a2.txid(), a.txid(), b.txid()];
    let state = pool.read();
    assert_eq!(state.ancestor_score_iter().collect::<Vec<_>>(), want);
    assert_eq!(MempoolRead::ancestor_score_iter(&pool).collect::<Vec<_>>(), want);
}
