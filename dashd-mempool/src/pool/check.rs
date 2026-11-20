//! Core v24.0.0 `txmempool.cpp:1371-1494`, plus a full rebuild of the Dash conflict maps.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::coins::{Coin, CoinRef, CoinsView, ViewOr, is_unspendable};
use dash_consensus::tx::check_tx_inputs;
use dash_primitives::{BlockHash, OutPoint, SpecialPayload, TxHash};

use super::dash::{
    HEIGHT_DIFF_EXPIRING, collateral, key_hash, net_entries, owner_ids, payload, platform_id,
};
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
        self.check_dash_maps();
    }

    /// Rebuilds every Dash index from the resident entries and compares it with the pool's,
    /// so a missing, extra or wrong key or value fails in either direction. The tip-dependent
    /// ProTx flags come from the per-entry context recorded when the entry was indexed.
    fn check_dash_maps(&self) {
        let mut expected = super::dash::DashMaps::default();
        let mut asset_unlock_instances = 0usize;
        let mut asset_unlock_indexes = 0usize;
        assert_eq!(
            self.dash.protx_index_context.keys().collect::<BTreeSet<_>>(),
            self.entries.keys().collect::<BTreeSet<_>>()
        );
        for (id, entry) in &self.entries {
            let tx = &entry.tx;
            let pro_up = match payload(tx) {
                Some(SpecialPayload::ProviderRegister(p)) => {
                    if let Some(op) = collateral(tx).filter(|op| !op.txid().is_null()) {
                        expected.protx_refs.entry(*id).or_default().push(op.txid());
                    }
                    for address in net_entries(p.net_info()) {
                        expected.addresses.insert(address, *id);
                    }
                    for owner in owner_ids(tx) {
                        expected.owner_ids.insert(owner, *id);
                    }
                    if let Some(key) = key_hash(tx) {
                        expected.bls_keys.insert(key, *id);
                    }
                    if let Some(platform) = platform_id(tx) {
                        expected.platform_ids.insert(platform, *id);
                    }
                    if let Some(mut op) = collateral(tx) {
                        if op.txid().is_null() {
                            op = OutPoint::new(*id, op.index());
                        }
                        expected.collaterals.insert(op, *id);
                    }
                    None
                }
                Some(SpecialPayload::ProviderUpdateService(p)) => {
                    for address in net_entries(p.net_info()) {
                        expected.addresses.insert(address, *id);
                    }
                    if let Some(platform) = platform_id(tx) {
                        expected.platform_ids.insert(platform, *id);
                    }
                    Some((p.pro_tx_hash(), true, false))
                }
                Some(SpecialPayload::ProviderUpdateRegistrar(p)) => {
                    if let Some(key) = key_hash(tx) {
                        expected.bls_keys.insert(key, *id);
                    }
                    Some((p.pro_tx_hash(), true, true))
                }
                Some(SpecialPayload::ProviderUpdateRevoke(p)) => {
                    Some((p.pro_tx_hash(), true, true))
                }
                Some(SpecialPayload::ProviderDissolve(p)) => Some((p.pro_tx_hash(), false, false)),
                Some(SpecialPayload::ProviderUpdateShare(p)) => {
                    Some((p.pro_tx_hash(), false, false))
                }
                Some(SpecialPayload::ProviderUpdateSharedRegistrar(p)) => {
                    if let Some(key) = key_hash(tx) {
                        expected.bls_keys.insert(key, *id);
                    }
                    Some((p.pro_tx_hash(), false, true))
                }
                _ => None,
            };
            let (valid_for, key_change) = self.dash.protx_index_context[id];
            if let Some((pro, has_valid_for, has_key_change)) = pro_up {
                expected.protx_refs.entry(pro).or_default().push(*id);
                assert!(has_valid_for || valid_for.is_none());
                assert!(has_key_change || !key_change);
                if let Some(key) = valid_for {
                    expected.valid_for_protx_key.insert(*id, key);
                }
            } else {
                assert_eq!((valid_for, key_change), (None, false));
            }
            if key_change {
                expected.key_change_protx.insert(*id);
            }
            if tx.is_asset_unlock_with_stable_txid() {
                asset_unlock_instances += 1;
            }
            if let Some(Ok(decoded)) = tx.payload()
                && decoded.is_complete()
                && let SpecialPayload::AssetUnlock(p) = decoded.payload()
            {
                asset_unlock_indexes += 1;
                expected.asset_unlock_indexes.entry(p.index()).or_default().push(*id);
                if tx.is_asset_unlock_with_stable_txid() {
                    expected.asset_unlock_instances.insert(tx.instance_hash(), *id);
                } else {
                    expected.asset_unlock_expiry.insert(
                        *id,
                        p.requested_height().wrapping_add(HEIGHT_DIFF_EXPIRING) as i32,
                    );
                }
                expected.pending_asset_unlock_amount = expected
                    .pending_asset_unlock_amount
                    .checked_add(i128::from(tx.value_out()) + i128::from(p.fee()))
                    .expect("asset-unlock amount overflow");
            }
        }
        // Core tm:1490-1491: the instance map and the index multimap hold one row per entry.
        assert_eq!(self.dash.asset_unlock_instances.len(), asset_unlock_instances);
        assert_eq!(
            self.dash.asset_unlock_indexes.values().map(Vec::len).sum::<usize>(),
            asset_unlock_indexes
        );
        assert_eq!(sorted(&self.dash.protx_refs), sorted(&expected.protx_refs));
        assert_eq!(self.dash.addresses, expected.addresses);
        assert_eq!(self.dash.owner_ids, expected.owner_ids);
        assert_eq!(self.dash.bls_keys, expected.bls_keys);
        assert_eq!(self.dash.platform_ids, expected.platform_ids);
        assert_eq!(self.dash.collaterals, expected.collaterals);
        assert_eq!(self.dash.asset_unlock_instances, expected.asset_unlock_instances);
        assert_eq!(sorted(&self.dash.asset_unlock_indexes), sorted(&expected.asset_unlock_indexes));
        assert_eq!(self.dash.asset_unlock_expiry, expected.asset_unlock_expiry);
        assert_eq!(self.dash.pending_asset_unlock_amount, expected.pending_asset_unlock_amount);
        assert_eq!(self.dash.valid_for_protx_key, expected.valid_for_protx_key);
        assert_eq!(self.dash.key_change_protx, expected.key_change_protx);
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

/// Multimap rows compared as multisets: Core's order within one key is insertion order.
fn sorted<K: Ord + Clone>(map: &BTreeMap<K, Vec<TxHash>>) -> BTreeMap<K, Vec<TxHash>> {
    map.iter()
        .map(|(key, ids)| {
            let mut ids = ids.clone();
            ids.sort_unstable();
            (key.clone(), ids)
        })
        .collect()
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
