//! Dash-specific mempool conflict indexes (Core v24.0.0 `txmempool.cpp`).

mod assetunlock;
pub(crate) use assetunlock::HEIGHT_DIFF_EXPIRING;
mod block;
mod protx;
pub(crate) use protx::{collateral, key_hash, net_entries, owner_ids, payload, platform_id};
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use dash_primitives::{NIEntry, OutPoint, TxHash};

#[derive(Default)]
pub(crate) struct DashMaps {
    pub(crate) protx_refs: BTreeMap<TxHash, Vec<TxHash>>,
    pub(crate) addresses: HashMap<NIEntry, TxHash>,
    pub(crate) owner_ids: HashMap<[u8; 20], TxHash>,
    pub(crate) bls_keys: HashMap<[u8; 32], TxHash>,
    pub(crate) platform_ids: HashMap<[u8; 20], TxHash>,
    pub(crate) collaterals: HashMap<OutPoint, TxHash>,
    pub(crate) asset_unlock_instances: BTreeMap<TxHash, TxHash>,
    pub(crate) asset_unlock_indexes: BTreeMap<u64, Vec<TxHash>>,
    pub(crate) asset_unlock_expiry: BTreeMap<TxHash, i32>,
    pub(crate) pending_asset_unlock_amount: i128,
    pub(crate) valid_for_protx_key: BTreeMap<TxHash, [u8; 32]>,
    pub(crate) key_change_protx: BTreeSet<TxHash>,
    pub(crate) protx_index_context: BTreeMap<TxHash, (Option<[u8; 32]>, bool)>,
}
