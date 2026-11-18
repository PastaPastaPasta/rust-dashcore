//! A graph model with no cached relationships or package statistics.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_consensus::{Tx, TxRef};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dashd_mempool::options::MemPoolOptions;
use dashd_mempool::pool::ancestors::NO_LIMITS;
use dashd_mempool::pool::{EntryMetadata, PackageStats, PoolState};
use dashd_mempool::types::MemPoolRemovalReason as Reason;
use proptest::prelude::*;

struct ModelEntry {
    tx: TxRef,
    fee: i64,
    modified_fee: i64,
    size: u64,
    sigops: i64,
}
#[derive(Default)]
struct Model {
    entries: BTreeMap<TxHash, ModelEntry>,
    deltas: BTreeMap<TxHash, i64>,
    order: Vec<(TxHash, TxHash)>,
}

impl Model {
    fn relatives(&self, root: TxHash, ancestors: bool) -> BTreeSet<TxHash> {
        let mut seen = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let entry = &self.entries[&id];
            if ancestors {
                pending.extend(
                    entry
                        .tx
                        .inputs()
                        .iter()
                        .map(|input| input.prevout().txid())
                        .filter(|id| self.entries.contains_key(id)),
                );
            } else {
                pending.extend(
                    self.entries
                        .iter()
                        .filter(|(_, child)| {
                            child.tx.inputs().iter().any(|input| input.prevout().txid() == id)
                        })
                        .map(|(id, _)| *id),
                );
            }
        }
        seen
    }
    fn stats(&self, ids: &BTreeSet<TxHash>) -> PackageStats {
        PackageStats {
            count: ids.len() as u64,
            size: ids.iter().map(|id| self.entries[id].size).sum(),
            modified_fees: ids.iter().map(|id| self.entries[id].modified_fee).sum(),
        }
    }
    fn remove(&mut self, ids: &BTreeSet<TxHash>) {
        for id in ids {
            self.entries.remove(id);
            let idx = self.order.iter().position(|(_, txid)| txid == id).unwrap();
            self.order.swap_remove(idx);
        }
    }
    fn check(&self, pool: &PoolState) {
        pool.check();
        assert_eq!(pool.len(), self.entries.len());
        assert_eq!(pool.vtx_hashes(), self.order);
        assert_eq!(pool.deltas(), &self.deltas);
        assert_eq!(pool.total_vsize(), self.entries.values().map(|entry| entry.size).sum::<u64>());
        assert_eq!(pool.total_fee(), self.entries.values().map(|entry| entry.fee).sum::<i64>());
        let mut next = BTreeMap::new();
        for (id, entry) in &self.entries {
            let actual = pool.get_iter(id).unwrap();
            let ancestors = self.relatives(*id, true);
            let descendants = self.relatives(*id, false);
            assert_eq!(actual.ancestors(), self.stats(&ancestors));
            assert_eq!(actual.descendants(), self.stats(&descendants));
            assert_eq!(
                actual.ancestor_sigops(),
                ancestors.iter().map(|id| self.entries[id].sigops).sum::<i64>()
            );
            assert_eq!(actual.modified_fee(), entry.modified_fee);
            let mut proper_ancestors = ancestors;
            proper_ancestors.remove(id);
            assert_eq!(
                pool.calculate_mem_pool_ancestors(actual, &NO_LIMITS, false).unwrap(),
                proper_ancestors
            );
            assert_eq!(
                pool.calculate_mem_pool_ancestors(actual, &NO_LIMITS, true).unwrap(),
                proper_ancestors
            );
            assert_eq!(pool.calculate_descendants(*id), descendants);
            for input in entry.tx.inputs() {
                next.insert(input.prevout(), *id);
            }
        }
        assert_eq!(pool.next_tx(), &next);
        assert_eq!(pool.dynamic_memory_usage(), pool.recompute_dynamic_memory_usage());
        // Independently compute the minimum of individual and ancestor fee rates.
        let score = |id: &TxHash| {
            let entry = &self.entries[id];
            let package = self.stats(&self.relatives(*id, true));
            if (entry.modified_fee as f64) * (package.size as f64)
                > (package.modified_fees as f64) * (entry.size as f64)
            {
                (package.modified_fees, package.size)
            } else {
                (entry.modified_fee, entry.size)
            }
        };
        let mut expected: Vec<_> = self.entries.keys().copied().collect();
        expected.sort_by(|a, b| {
            let (af, az) = score(a);
            let (bf, bz) = score(b);
            ((bf as f64) * (az as f64))
                .partial_cmp(&((af as f64) * (bz as f64)))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(b))
        });
        assert_eq!(pool.ancestor_score_order().collect::<Vec<_>>(), expected);
    }
}

#[test]
fn ancestor_score_order_uses_rounded_double_ties_and_ascending_txids() {
    let mut pool = PoolState::default();
    let a = Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        vec![],
        vec![TxOut::from_duffs(1, vec![0x51].into()); 4],
        1,
        vec![],
    )));
    let b = Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        vec![],
        vec![TxOut::from_duffs(1, vec![0x51].into()); 4],
        2,
        vec![],
    )));
    let (low, high) = if a.txid() < b.txid() {
        (a, b)
    } else {
        (b, a)
    };
    for (tx, fee) in [(&low, 1i64 << 53), (&high, (1i64 << 53) + 1)] {
        let entry = pool.new_entry(
            tx.clone(),
            EntryMetadata {
                fee,
                ..Default::default()
            },
        );
        pool.add_unchecked(entry);
    }
    assert_eq!(pool.ancestor_score_order().collect::<Vec<_>>(), [low.txid(), high.txid()]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn random_pool_operations_match_graph(
        bytes_per_sigop in prop_oneof![Just(0u32), Just(20), Just(30)],
        ops in prop::collection::vec((0u8..10, any::<u16>(), any::<u16>(), -2000i64..2000), 1..100),
    ) {
        let mut pool = PoolState::new(MemPoolOptions { bytes_per_sigop, ..Default::default() });
        let mut model = Model::default();
        let mut history: Vec<TxRef> = vec![];
        for (nonce, (op, a, b, amount)) in ops.into_iter().enumerate() {
            if op < 4 || model.entries.is_empty() {
                let spent: BTreeSet<_> = model.entries.values().flat_map(|entry| entry.tx.inputs().iter().map(|i| i.prevout())).collect();
                let available: Vec<_> = model.entries.values().flat_map(|entry| {
                    (0..entry.tx.outputs().len()).map(|n| OutPoint::new(entry.tx.txid(), n as u32))
                }).filter(|p| !spent.contains(p)).collect();
                let mut inputs = BTreeSet::new();
                if !available.is_empty() {
                    inputs.insert(available[usize::from(a) % available.len()]);
                    if op == 0 { inputs.insert(available[usize::from(b) % available.len()]); }
                }
                let inputs = if op == 3 { vec![OutPoint::new(TxHash::from([255;32]), nonce as u32)] }
                    else { inputs.into_iter().collect() };
                let tx = Arc::new(Tx::new(Transaction::new(2, TxType::new(0),
                    inputs.into_iter().map(|p| TxIn::new(p, vec![0x51; (b % 40) as usize].into(), u32::MAX)).collect(),
                    vec![TxOut::from_duffs(1, vec![0x51].into()); 4], nonce as u32, vec![])));
                let sigops = i64::from(a % 15); let fee = amount.abs();
                let size = u64::from(tx.size()).max(sigops as u64 * u64::from(bytes_per_sigop));
                let entry = pool.new_entry(tx.clone(), EntryMetadata { fee, sigops, ..Default::default() });
                pool.add_unchecked(entry);
                let id = tx.txid();
                model.order.push((tx.instance_hash(), id));
                let modified_fee = fee + model.deltas.get(&id).copied().unwrap_or(0);
                model.entries.insert(id, ModelEntry { tx: tx.clone(), fee, modified_fee, size, sigops });
                history.push(tx);
            } else {
                let id = *model.entries.keys().nth(usize::from(a) % model.entries.len()).unwrap();
                match op {
                    4 => {
                        let tx = model.entries[&id].tx.clone();
                        let removed = model.relatives(id, false);
                        pool.remove_recursive(&tx, Reason::Manual); model.remove(&removed);
                    },
                    5 => {
                        let mined = model.relatives(id, true);
                        pool.remove_staged(&mined, true, Reason::Block);
                        model.remove(&mined);
                    },
                    6 | 7 => {
                        // Include absent txids so retained deltas and no-op counter behavior run.
                        let target = if op == 7 { history[usize::from(b) % history.len()].txid() } else { id };
                        pool.prioritise_transaction(target, amount);
                        *model.deltas.entry(target).or_default() += amount;
                        if let Some(entry) = model.entries.get_mut(&target) { entry.modified_fee += amount; }
                    },
                    8 => {
                        pool.clear_prioritisation(&id); model.deltas.remove(&id);
                    },
                    _ => {
                        let tx = history[usize::from(b) % history.len()].clone();
                        let mut removed = BTreeSet::new();
                        if model.entries.contains_key(&tx.txid()) { removed.extend(model.relatives(tx.txid(), false)); }
                        else {
                            for (id, entry) in &model.entries {
                                if entry.tx.inputs().iter().any(|input| input.prevout().txid() == tx.txid()) {
                                    removed.extend(model.relatives(*id, false));
                                }
                            }
                        }
                        pool.remove_recursive(&tx, Reason::Reorg); model.remove(&removed);
                    },
                }
            }
            model.check(&pool);
            pool.take_pending_events();
        }
    }
}
