//! Core v24.0.0 `txmempool.cpp:1371-1494`, structural half without Dash maps.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::coins::{Coin, CoinRef, CoinsView, ViewOr, is_unspendable};
use dash_consensus::tx::check_tx_inputs;
use dash_primitives::{BlockHash, OutPoint, TxHash};

use super::{PackageStats, PoolState, index::AncestorScore};

impl PoolState {
    /// Checks every cache/index independently of admission limits. The caller chooses sampling.
    /// Use check_with_coins for Core's additional chain coin and CheckTxInputs assertions.
    pub fn check(&self) {
        assert!(self.unbroadcast_txids.iter().all(|id| self.entries.contains_key(id)));
        let mut next = BTreeMap::new();
        let mut total_vsize = 0;
        let mut total_fee = 0;
        assert_eq!(self.vtx_hashes.len(), self.entries.len());
        assert!(self.vtx_hashes.len() <= self.vtx_capacity);
        for (id, entry) in &self.entries {
            assert_eq!(*id, entry.txid());
            total_vsize += entry.vsize;
            total_fee += entry.metadata.fee;
            let mut parents = BTreeSet::new();
            for input in entry.tx.inputs() {
                assert!(next.insert(input.prevout(), *id).is_none());
                if let Some(parent) = self.entries.get(&input.prevout().txid()) {
                    assert!((input.prevout().index() as usize) < parent.tx.outputs().len());
                    assert_ne!(parent.tx.outputs()[input.prevout().index() as usize].value(), -1);
                    parents.insert(parent.txid());
                }
            }
            let children: BTreeSet<_> = self
                .entries
                .iter()
                .filter(|(_, child)| {
                    child.tx.inputs().iter().any(|input| input.prevout().txid() == *id)
                })
                .map(|(id, _)| *id)
                .collect();
            assert_eq!(entry.parents, parents);
            assert_eq!(entry.children, children);
            let ancestors = self.walk(*id, true);
            let descendants = self.walk(*id, false);
            assert_eq!(entry.ancestors, self.stats(&ancestors));
            assert_eq!(entry.descendants, self.stats(&descendants));
            assert_eq!(
                entry.ancestor_sigops,
                ancestors.iter().map(|id| self.entries[id].metadata.sigops).sum::<i64>()
            );
            assert_eq!(self.vtx_hashes[entry.vtx_hashes_idx], (entry.tx.instance_hash(), *id));
        }
        assert_eq!(self.next_tx, next);
        assert_eq!(self.total_vsize, total_vsize);
        assert_eq!(self.total_fee, total_fee);
        let scores: BTreeSet<_> = self.entries.values().map(AncestorScore::from).collect();
        assert_eq!(self.ancestor_score, scores);
        assert_eq!(self.dynamic_memory_usage(), self.recompute_dynamic_memory_usage());
    }

    /// Core's duplicate-coins walk. Storage faults propagate; invalid pool contents assert.
    pub fn check_with_coins<V: CoinsView + ?Sized>(
        &self,
        coins: &V,
        spend_height: u32,
    ) -> Result<(), V::Error> {
        self.check();
        let mut overlay = CheckCoins {
            base: coins,
            added: BTreeMap::new(),
            spent: BTreeSet::new(),
        };
        for id in self.get_sorted_depth_and_score() {
            let tx = &self.entries[&id].tx;
            assert!(!tx.is_coinbase());
            match check_tx_inputs(tx, &overlay, spend_height) {
                Ok(_) => {}
                Err(ViewOr::View(error)) => return Err(error),
                Err(ViewOr::Reject(error)) => panic!("mempool CheckTxInputs: {error:?}"),
            }
            for input in tx.inputs() {
                assert!(overlay.coin(&input.prevout())?.is_some());
                overlay.added.remove(&input.prevout());
                overlay.spent.insert(input.prevout());
            }
            for (n, output) in tx.outputs().iter().enumerate() {
                if !is_unspendable(output.script_pubkey().as_bytes()) {
                    overlay.added.insert(
                        OutPoint::new(id, n as u32),
                        Coin::from_output(output, i32::MAX as u32, false),
                    );
                }
            }
        }
        Ok(())
    }

    fn walk(&self, root: TxHash, parents: bool) -> BTreeSet<TxHash> {
        let mut visited = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let entry = &self.entries[&id];
            let links = if parents {
                &entry.parents
            } else {
                &entry.children
            };
            assert!(!links.contains(&root), "cyclic mempool graph");
            pending.extend(links);
        }
        visited
    }

    fn stats(&self, ids: &BTreeSet<TxHash>) -> PackageStats {
        let mut stats = PackageStats {
            count: ids.len() as u64,
            size: 0,
            modified_fees: 0,
        };
        for id in ids {
            stats.size += self.entries[id].vsize;
            stats.modified_fees = stats.modified_fees.saturating_add(self.entries[id].modified_fee);
        }
        stats
    }
}

struct CheckCoins<'a, V: CoinsView + ?Sized> {
    base: &'a V,
    added: BTreeMap<OutPoint, Coin>,
    spent: BTreeSet<OutPoint>,
}

impl<V: CoinsView + ?Sized> CoinsView for CheckCoins<'_, V> {
    type Error = V::Error;
    fn coin(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'_>>, Self::Error> {
        if let Some(coin) = self.added.get(outpoint) {
            return Ok(Some(coin.as_ref()));
        }
        if self.spent.contains(outpoint) {
            return Ok(None);
        }
        self.base.coin(outpoint)
    }
    fn best_block(&self) -> Option<BlockHash> {
        self.base.best_block()
    }
}
