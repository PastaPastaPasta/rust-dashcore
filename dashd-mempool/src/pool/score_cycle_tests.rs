//! BL-974: Core's score comparators multiply doubles, and above 2^53 the rounding can rank three
//! entries in a cycle. The pool compares exact products instead, so its score orders stay total:
//! the indexes find every key, the sorts never see an inconsistent order and the check holds.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::Arc;

use dash_consensus::{Tx, TxRef};
use dash_dml::MnList;
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use proptest::prelude::*;

use super::{AncestorScore, DescendantScore, depth_and_score};
use crate::pool::{EntryMetadata, PoolEntry, PoolState};
use crate::types::MemPoolRemovalReason;

const NOW: i64 = 1_700_000_000;
const PARENT_FEE: i64 = 1_000;
const PRIORITISED: [i64; 2] = [2_000_000_000_000_000, 1_900_000_000_000_000];
/// (fee, size) of children a, b and c: as doubles a ties b and b ties c, yet c outranks a.
const CYCLE: [(i64, u32); 3] = [
    (1_692_150_665_253_044, 5_333),
    (1_917_115_004_586_329, 6_042),
    (1_889_510_071_551_074, 5_955),
];
const UNRELATED_FEE: i64 = 10_000;
/// Eight triples from this lock time are an input on which the 1.98 `slice::sort_by` panicked
/// under the double comparator.
const WIDE_LOCK_TIME: u32 = 2_000;

fn tx(inputs: &[OutPoint], outputs: usize, pad: usize, lock_time: u32) -> TxRef {
    let mut outs = vec![TxOut::from_duffs(1, vec![0x51].into()); outputs];
    outs.push(TxOut::from_duffs(1, vec![0x51; pad].into()));
    Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        inputs.iter().map(|p| TxIn::new(*p, vec![].into(), u32::MAX)).collect(),
        outs,
        lock_time,
        vec![],
    )))
}

/// A transaction of exactly `size` bytes; the lock time only varies the txid.
fn sized(inputs: &[OutPoint], size: u32, lock_time: u32) -> TxRef {
    let bare = tx(inputs, 0, 0, lock_time).size();
    // The padding script's length prefix grows from one byte to three.
    let sized = tx(inputs, 0, (size - bare - 2) as usize, lock_time);
    assert_eq!(sized.size(), size);
    sized
}

struct Fixture {
    pool: PoolState,
    parents: [TxRef; 2],
    /// Triples of children of both parents with `CYCLE`'s fees and sizes, a then b then c.
    children: Vec<[TxRef; 3]>,
    unrelated: TxRef,
}

fn add(pool: &mut PoolState, tx: &TxRef, fee: i64, time: i64) {
    let metadata = EntryMetadata {
        fee,
        time,
        ..Default::default()
    };
    let entry = pool.new_entry(tx.clone(), metadata);
    pool.add_unchecked(entry, &MnList::default_pre_dip3());
    pool.check();
}

/// `triples` cycles. Ascending txids (a < b < c) close the ancestor-score cycle; descending ones
/// close the depth-and-score cycle. Entry times rise from a to c, which closes the
/// descendant-score cycle either way.
fn fixture(triples: usize, ascending: bool, first_lock_time: u32) -> Fixture {
    let mut pool = PoolState::default();
    let outputs = 3 * triples;
    let parents = [1u8, 2].map(|n| tx(&[OutPoint::new(TxHash::from([n; 32]), 0)], outputs, 0, 0));
    for parent in &parents {
        add(&mut pool, parent, PARENT_FEE, NOW);
    }
    let mut lock_time = first_lock_time;
    let children: Vec<[TxRef; 3]> = (0..triples)
        .map(|t| {
            let mut triple: Vec<TxRef> = Vec::new();
            for (i, (_, size)) in CYCLE.iter().enumerate() {
                let vout = (3 * t + i) as u32;
                let inputs = parents.clone().map(|p| OutPoint::new(p.txid(), vout));
                let child = loop {
                    lock_time += 1;
                    let child = sized(&inputs, *size, lock_time);
                    match triple.last() {
                        Some(prev) if (prev.txid() < child.txid()) != ascending => continue,
                        _ => break child,
                    }
                };
                triple.push(child);
            }
            triple.try_into().unwrap_or_else(|_| unreachable!())
        })
        .collect();
    for triple in &children {
        for (i, child) in triple.iter().enumerate() {
            add(&mut pool, child, CYCLE[i].0, NOW + 1 + i as i64);
        }
    }
    let unrelated = tx(&[OutPoint::new(TxHash::from([3; 32]), 0)], 1, 0, 0);
    add(&mut pool, &unrelated, UNRELATED_FEE, NOW);
    Fixture {
        pool,
        parents,
        children,
        unrelated,
    }
}

impl Fixture {
    fn prioritise_parents(&mut self) {
        for (parent, fee) in self.parents.iter().zip(PRIORITISED) {
            self.pool.prioritise_transaction(parent.txid(), fee - PARENT_FEE);
            self.pool.check();
        }
    }

    fn keys<'a, K>(&'a self, key: impl Fn(&'a PoolEntry) -> K) -> [K; 3] {
        self.children[0].clone().map(|child| key(&self.pool.entries[&child.txid()]))
    }

    fn all(&self) -> BTreeSet<TxHash> {
        self.pool.entries.keys().copied().collect()
    }

    /// Both score indexes hold every entry once.
    fn assert_indexed(&self) {
        let ancestor: Vec<_> = self.pool.ancestor_score_order().collect();
        let descendant: Vec<_> = self.pool.descendant_score_order().collect();
        for order in [ancestor, descendant] {
            assert_eq!(order.len(), self.pool.len());
            assert_eq!(order.into_iter().collect::<BTreeSet<_>>(), self.all());
        }
    }
}

/// `cmp` is a strict total order on `items`: antisymmetric, `Equal` only for an item and itself,
/// and transitive across every triple.
fn assert_total<K>(items: &[K], cmp: impl Fn(&K, &K) -> Ordering) {
    for (i, x) in items.iter().enumerate() {
        for (j, y) in items.iter().enumerate() {
            assert_eq!(cmp(x, y), cmp(y, x).reverse());
            assert_eq!(cmp(x, y) == Ordering::Equal, i == j);
            for z in items {
                if cmp(x, y).is_lt() && cmp(y, z).is_lt() {
                    assert!(cmp(x, z).is_lt());
                }
            }
        }
    }
}

/// `sorted` is strictly ascending under `cmp`, which is a strict total order on it.
fn assert_sorted<K>(sorted: &[K], cmp: impl Fn(&K, &K) -> Ordering) {
    assert!(sorted.windows(2).all(|pair| cmp(&pair[0], &pair[1]).is_lt()));
    assert_total(sorted, cmp);
}

/// M-1's values tie as doubles a with b and b with c while c outranks a; the exact scores order
/// them.
#[test]
fn the_probe_values_cycle_as_doubles_but_order_exactly() {
    let double = |(f, s): (i64, u32), (g, t): (i64, u32)| {
        (f as f64 * f64::from(t)).partial_cmp(&(g as f64 * f64::from(s)))
    };
    let [a, b, c] = CYCLE;
    let equal = Some(Ordering::Equal);
    assert_eq!([double(a, b), double(b, c), double(c, a)], [equal, equal, Some(Ordering::Greater)]);
    for ascending in [true, false] {
        let mut fx = fixture(1, ascending, 0);
        fx.prioritise_parents();
        assert_total(&fx.keys(AncestorScore::from), Ord::cmp);
        assert_total(&fx.keys(DescendantScore::from), Ord::cmp);
        assert_total(&fx.keys(|entry| entry), |x, y| depth_and_score(x, y));
    }
}

#[test]
fn the_probe_values_survive_adding_and_prioritising() {
    for ascending in [true, false] {
        let mut fx = fixture(1, ascending, 0);
        fx.assert_indexed();
        fx.prioritise_parents();
        fx.assert_indexed();
        let [a, ..] = fx.children[0].clone();
        fx.pool.prioritise_transaction(a.txid(), 1);
        fx.pool.check();
        fx.assert_indexed();
    }
}

#[test]
fn the_probe_values_survive_removal_for_a_block() {
    for ascending in [true, false] {
        let mut fx = fixture(1, ascending, 0);
        fx.prioritise_parents();
        let [a, b, c] = fx.children[0].clone();
        let block = [fx.parents[0].clone(), fx.parents[1].clone(), b, a, c];
        fx.pool.remove_for_block(&block, &MnList::default_pre_dip3());
        fx.pool.check();
        assert_eq!(fx.all(), BTreeSet::from([fx.unrelated.txid()]));
        fx.assert_indexed();
    }
}

#[test]
fn the_probe_values_survive_trimming() {
    for ascending in [true, false] {
        let mut fx = fixture(1, ascending, 0);
        fx.prioritise_parents();
        fx.pool.trim_to_size(fx.pool.dynamic_memory_usage() - 1);
        fx.pool.check();
        assert!(!fx.pool.exists(&fx.unrelated.txid()), "the lowest descendant score goes first");
        fx.assert_indexed();
        fx.pool.trim_to_size(0);
        fx.pool.check();
        assert!(fx.pool.is_empty());
    }
}

#[test]
fn the_probe_values_survive_expiry() {
    for ascending in [true, false] {
        let mut fx = fixture(1, ascending, 0);
        fx.prioritise_parents();
        assert_eq!(fx.pool.expire(NOW + 1), 6);
        fx.pool.check();
        assert!(fx.pool.is_empty());
        fx.assert_indexed();
    }
}

/// Eight triples were enough for `slice::sort_by` to detect the double comparator's cycle.
#[test]
fn the_probe_values_survive_the_depth_and_score_sort() {
    for triples in [1, 8] {
        let mut fx = fixture(triples, false, WIDE_LOCK_TIME);
        fx.prioritise_parents();
        let sorted = fx.pool.get_sorted_depth_and_score();
        assert_eq!(sorted.len(), fx.pool.len());
        assert_eq!(sorted.iter().copied().collect::<BTreeSet<_>>(), fx.all());
        let entries: Vec<_> = sorted.iter().map(|id| &fx.pool.entries[id]).collect();
        assert_sorted(&entries, |x, y| depth_and_score(x, y));
        let infos: Vec<_> = fx.pool.info_all().iter().map(|info| info.tx.txid()).collect();
        assert_eq!(infos, sorted);
        fx.pool.check();
    }
}

/// The testkit fake sorts its `info_all` with the same score.
#[cfg(feature = "test-support")]
#[test]
fn the_probe_values_survive_the_testkit_sort() {
    let fake = Arc::new(crate::testkit::FakeIsMempool::default());
    let children: Vec<TxRef> =
        fixture(8, false, WIDE_LOCK_TIME).children.into_iter().flatten().collect();
    for (i, child) in children.iter().enumerate() {
        fake.insert_entry(child.clone(), NOW, CYCLE[i % 3].0);
    }
    let handle: Arc<dyn crate::api::MempoolHandle> = fake;
    let infos = handle.read(|read| read.info_all());
    let got: BTreeSet<_> = infos.iter().map(|info| info.tx.txid()).collect();
    assert_eq!((infos.len(), got), (children.len(), children.iter().map(|tx| tx.txid()).collect()));
}

/// Core's tie is `uint256 <`, a memcmp of the internal bytes, not the order of the hex.
#[test]
fn txid_ties_compare_the_raw_bytes() {
    let mut low_first = [0u8; 32];
    low_first[0] = 1;
    let mut high_first = [0u8; 32];
    high_first[31] = 1;
    let (low_first, high_first) = (TxHash::from(low_first), TxHash::from(high_first));
    assert!(low_first > high_first);
    assert!(
        TxHash::from_hex(&format!("{:064x}", 1)).unwrap()
            > TxHash::from_hex(&format!("1{:063x}", 0)).unwrap()
    );
    assert_eq!(low_first.as_bytes().cmp(high_first.as_bytes()), low_first.cmp(&high_first));
}

/// Products above 2^53 with near-equal feerates: M-1's values with ascending and with
/// descending txids, and a grid of fees within a duff of one feerate. Each is unrelated and
/// enters at the same time.
fn near_equal_pool() -> (PoolState, Vec<TxRef>) {
    const RATE: i64 = 317_300_000_000;
    let mut txs: Vec<(TxRef, i64)> = Vec::new();
    let mut coin = 0u8;
    let mut lock_time = 0;
    for ascending in [true, false] {
        let mut prev: Option<TxHash> = None;
        for (fee, size) in CYCLE {
            coin += 1;
            let tx = loop {
                lock_time += 1;
                let tx = sized(&[OutPoint::new(TxHash::from([coin; 32]), 0)], size, lock_time);
                if prev.is_none_or(|prev| (prev < tx.txid()) == ascending) {
                    break tx;
                }
            };
            prev = Some(tx.txid());
            txs.push((tx, fee));
        }
    }
    for size in [5_333, 5_955, 6_042, 6_421, 7_013] {
        for offset in [-1, 0, 1] {
            coin += 1;
            let tx = sized(&[OutPoint::new(TxHash::from([coin; 32]), 0)], size, 0);
            txs.push((tx, RATE * i64::from(size) + offset));
        }
    }
    let mut pool = PoolState::default();
    for (tx, fee) in &txs {
        let metadata = EntryMetadata {
            fee: *fee,
            time: NOW,
            ..Default::default()
        };
        let entry = pool.new_entry(tx.clone(), metadata);
        pool.add_unchecked(entry, &MnList::default_pre_dip3());
    }
    (pool, txs.into_iter().map(|(tx, _)| tx).collect())
}

#[test]
fn near_equal_feerates_above_2_53_insert_and_remove_by_key() {
    let (mut pool, txs) = near_equal_pool();
    for tx in &txs {
        pool.prioritise_transaction(tx.txid(), 0);
    }
    let mut ids: Vec<_> = txs.iter().map(|tx| tx.txid()).collect();
    ids.sort();
    for id in ids {
        let tx = pool.get(&id).expect("resident");
        pool.remove_recursive(&tx, MemPoolRemovalReason::Expiry);
    }
    assert!(pool.is_empty());
    pool.check();
}

#[test]
fn near_equal_feerates_above_2_53_sort_totally() {
    let (pool, _) = near_equal_pool();
    pool.check();
    let ancestor: Vec<_> =
        pool.ancestor_score_order().map(|id| AncestorScore::from(&pool.entries[&id])).collect();
    assert_sorted(&ancestor, Ord::cmp);
    let descendant: Vec<_> =
        pool.descendant_score_order().map(|id| DescendantScore::from(&pool.entries[&id])).collect();
    assert_sorted(&descendant, Ord::cmp);
    let depth: Vec<_> =
        pool.get_sorted_depth_and_score().iter().map(|id| &pool.entries[id]).collect();
    assert_sorted(&depth, |x, y| depth_and_score(x, y));
    assert_eq!(depth.len(), pool.len());
}

proptest! {
    /// Any fees and sizes, near-equal feerates included, give a strict total order.
    #[test]
    fn ancestor_scores_are_a_total_order(
        base in (1i64 << 30)..(1i64 << 40),
        keys in prop::collection::vec((1u64..100_000, -3i64..3, any::<[u8; 32]>()), 3..12),
    ) {
        let keys: Vec<_> = keys
            .into_iter()
            .map(|(size, offset, txid)| AncestorScore {
                txid: TxHash::from(txid),
                fee: base * size as i64 + offset,
                size,
            })
            .collect();
        let mut seen = BTreeSet::new();
        prop_assume!(keys.iter().all(|key| seen.insert(key.txid)));
        assert_total(&keys, Ord::cmp);
    }
}
