use dash_consensus::Tx;
use dash_primitives::{SpecialPayload, TxHash};

use super::super::PoolState;

/// Core `CAssetUnlockPayload::HEIGHT_DIFF_EXPIRING` (evo/assetlocktx.h).
pub(crate) const HEIGHT_DIFF_EXPIRING: u32 = 48;

fn payload(tx: &Tx) -> Option<&dash_primitives::AssetUnlock> {
    match tx.payload()?.as_ref().ok().filter(|p| p.is_complete())?.payload() {
        SpecialPayload::AssetUnlock(p) => Some(p),
        _ => None,
    }
}

fn amount(tx: &Tx, p: &dash_primitives::AssetUnlock) -> i128 {
    i128::from(tx.value_out()) + i128::from(p.fee())
}

impl PoolState {
    pub(crate) fn add_asset_unlock(&mut self, tx: &Tx) {
        let Some(p) = payload(tx) else {
            return;
        };
        let id = tx.txid();
        self.dash.asset_unlock_indexes.entry(p.index()).or_default().push(id);
        let value = amount(tx, p);
        self.dash.pending_asset_unlock_amount =
            self.dash.pending_asset_unlock_amount.checked_add(value).unwrap_or_else(|| {
                debug_assert!(false, "asset unlock amount overflow");
                i128::MAX
            });
        if tx.is_asset_unlock_with_stable_txid() {
            self.dash.asset_unlock_instances.insert(tx.instance_hash(), id);
        } else {
            self.dash
                .asset_unlock_expiry
                .insert(id, p.requested_height().wrapping_add(HEIGHT_DIFF_EXPIRING) as i32);
        }
    }

    pub(crate) fn remove_asset_unlock(&mut self, tx: &Tx) {
        let Some(p) = payload(tx) else {
            return;
        };
        let id = tx.txid();
        if let Some(ids) = self.dash.asset_unlock_indexes.get_mut(&p.index()) {
            ids.retain(|candidate| *candidate != id);
            if ids.is_empty() {
                self.dash.asset_unlock_indexes.remove(&p.index());
            }
        }
        let value = amount(tx, p);
        self.dash.pending_asset_unlock_amount =
            self.dash.pending_asset_unlock_amount.checked_sub(value).unwrap_or_else(|| {
                debug_assert!(false, "asset unlock amount underflow");
                0
            });
        if tx.is_asset_unlock_with_stable_txid() {
            self.dash.asset_unlock_instances.remove(&tx.instance_hash());
        } else {
            self.dash.asset_unlock_expiry.remove(&id);
        }
    }

    pub fn asset_unlock_by_instance_hash(&self, hash: TxHash) -> Option<TxHash> {
        self.dash.asset_unlock_instances.get(&hash).copied()
    }

    pub fn replace_asset_unlock_instance(&mut self, id: TxHash, tx: dash_consensus::TxRef) {
        assert_eq!(id, tx.txid());
        let entry = self.entries.get_mut(&id).expect("asset unlock must be resident");
        assert_eq!(entry.tx.size(), tx.size());
        let old_instance = entry.tx.instance_hash();
        entry.replace_tx(tx.clone());
        self.vtx_hashes[entry.vtx_hashes_idx].0 = tx.instance_hash();
        self.dash.asset_unlock_instances.remove(&old_instance);
        self.dash.asset_unlock_instances.insert(tx.instance_hash(), id);
        self.add_transactions_updated(1);
    }

    pub(crate) fn remove_asset_unlock_conflicts_impl(&mut self, tx: &Tx) {
        let Some(p) = payload(tx) else {
            return;
        };
        let ids = self.asset_unlock_txids_by_index(p.index());
        for id in ids {
            if id != tx.txid()
                && let Some(other) = self.get(&id)
            {
                self.remove_recursive(&other, crate::types::MemPoolRemovalReason::Conflict);
            }
        }
    }

    pub fn remove_expired_asset_unlock(&mut self, height: u32) {
        let ids: Vec<_> = self
            .dash
            .asset_unlock_expiry
            .iter()
            .filter_map(|(id, expiry)| (*expiry < height as i32).then_some(*id))
            .collect();
        for id in ids {
            if let Some(tx) = self.get(&id) {
                self.remove_recursive(&tx, crate::types::MemPoolRemovalReason::Expiry);
            }
        }
    }
}
