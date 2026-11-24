use std::collections::HashMap;

use dash_consensus::Tx;
use dash_consensus::primitives::CorePayload;
use dash_dml::{MnList, OperatorKey, ProTxHash};
use dash_pkc::bls::{BlsPkBytes, BlsPublicKey, BlsScChia, BlsScIetf};
use dash_primitives::{MnType, NIEntry, NITrait, NetInfo, ProTxVersion, SpecialPayload, TxHash};

use super::super::PoolState;
use crate::types::MemPoolRemovalReason;

fn core_payload(tx: &Tx) -> Option<&CorePayload> {
    tx.payload()?.ok().filter(|p| p.is_complete())
}

pub(crate) fn payload(tx: &Tx) -> Option<&SpecialPayload> {
    core_payload(tx).map(CorePayload::payload)
}

/// The operator key of a ProRegTx, ProUpRegTx or ProUpSharedRegTx: the object the payload
/// side-car built at decode.
pub(crate) fn operator_key(tx: &Tx) -> Option<&OperatorKey> {
    core_payload(tx)?.operator_key()
}

pub(crate) fn pro_tx_hash(tx: &Tx) -> Option<TxHash> {
    match payload(tx)? {
        SpecialPayload::ProviderUpdateService(p) => Some(p.pro_tx_hash()),
        SpecialPayload::ProviderUpdateRegistrar(p) => Some(p.pro_tx_hash()),
        SpecialPayload::ProviderUpdateRevoke(p) => Some(p.pro_tx_hash()),
        SpecialPayload::ProviderDissolve(p) => Some(p.pro_tx_hash()),
        SpecialPayload::ProviderUpdateShare(p) => Some(p.pro_tx_hash()),
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => Some(p.pro_tx_hash()),
        _ => None,
    }
}

fn pro_tx_key(tx: &Tx) -> Option<(BlsPkBytes, bool)> {
    let legacy = match payload(tx)? {
        SpecialPayload::ProviderRegister(p) => p.version() == ProTxVersion::LegacyBls,
        SpecialPayload::ProviderUpdateRegistrar(p) => p.version() == ProTxVersion::LegacyBls,
        SpecialPayload::ProviderUpdateSharedRegistrar(_) => false,
        _ => return None,
    };
    Some((BlsPkBytes::from_bytes(operator_key(tx)?.to_bytes()), legacy))
}

fn bls_hash(bytes: BlsPkBytes) -> [u8; 32] {
    dash_consensus::hash::sha256d(bytes.as_bytes())
}

pub(crate) fn key_hash(tx: &Tx) -> Option<[u8; 32]> {
    let (bytes, _) = pro_tx_key(tx)?;
    Some(bls_hash(bytes))
}

pub(crate) fn key_hashes(tx: &Tx) -> Option<[Option<[u8; 32]>; 2]> {
    let (bytes, legacy) = pro_tx_key(tx)?;
    let (legacy_hash, basic_hash) = if legacy {
        let key = BlsPublicKey::<BlsScChia>::try_from(bytes).ok()?;
        (
            Some(bls_hash(BlsPkBytes::from_bytes(key.to_bytes()))),
            key.to_scheme::<BlsScIetf>()
                .ok()
                .map(|key| bls_hash(BlsPkBytes::from_bytes(key.to_bytes()))),
        )
    } else {
        let key = BlsPublicKey::<BlsScIetf>::try_from(bytes).ok()?;
        (
            key.to_scheme::<BlsScChia>()
                .ok()
                .map(|key| bls_hash(BlsPkBytes::from_bytes(key.to_bytes()))),
            Some(bls_hash(BlsPkBytes::from_bytes(key.to_bytes()))),
        )
    };
    Some([legacy_hash, basic_hash])
}

fn zero_key_hash() -> [u8; 32] {
    bls_hash(BlsPkBytes::from_bytes([0; 48]))
}

pub(crate) fn net_entries(net: &NetInfo) -> impl Iterator<Item = NIEntry> + '_ {
    net.entries(None).filter(|entry| !matches!(entry, NIEntry::Invalid))
}

pub(crate) fn owner_ids(tx: &Tx) -> Vec<[u8; 20]> {
    match payload(tx) {
        Some(SpecialPayload::ProviderRegister(p)) if p.shares().is_empty() => {
            vec![*p.key_id_owner().as_bytes()]
        }
        Some(SpecialPayload::ProviderRegister(p)) => {
            p.shares().iter().map(|share| *share.key_id_owner().as_bytes()).collect()
        }
        _ => Vec::new(),
    }
}

pub(crate) fn platform_id(tx: &Tx) -> Option<[u8; 20]> {
    let id = match payload(tx) {
        Some(SpecialPayload::ProviderRegister(p)) if p.mn_type() == MnType::Evo => {
            p.platform_node_id()
        }
        Some(SpecialPayload::ProviderUpdateService(p)) if p.mn_type() == MnType::Evo => {
            p.platform_node_id()
        }
        _ => None,
    }?;
    (!id.is_null()).then_some(*id.as_bytes())
}

pub(crate) fn collateral(tx: &Tx) -> Option<dash_primitives::OutPoint> {
    let SpecialPayload::ProviderRegister(p) = payload(tx)? else {
        return None;
    };
    Some(dash_primitives::OutPoint::new(p.collateral_hash(), p.collateral_index()))
}

fn list_key_hash(list: &MnList, pro: TxHash) -> Option<[u8; 32]> {
    let dmn = list.get(&ProTxHash::from(*pro.as_bytes()))?;
    Some(bls_hash(BlsPkBytes::from_bytes(dmn.cold.pub_key_operator.to_bytes())))
}

/// The key ID of Core's `ExtractDestination` when it yields a `PKHash`: pay-to-pubkey (with a
/// valid-size key, script/standard.cpp:42-53) or pay-to-pubkey-hash.
fn reward_key_id(script: &[u8]) -> Option<[u8; 20]> {
    match script {
        [0x76, 0xa9, 20, hash @ .., 0x88, 0xac] if hash.len() == 20 => hash.try_into().ok(),
        [65, key @ .., 0xac] if key.len() == 65 && matches!(key[0], 4 | 6 | 7) => {
            Some(dash_consensus::hash::hash160(key))
        }
        [33, key @ .., 0xac] if key.len() == 33 && matches!(key[0], 2 | 3) => {
            Some(dash_consensus::hash::hash160(key))
        }
        _ => None,
    }
}

impl PoolState {
    pub(crate) fn on_dash_insert_at(&mut self, id: TxHash, tip_list: &MnList) {
        let tx = self.entries[&id].tx.clone();
        self.add_asset_unlock(&tx);
        let context = match payload(&tx) {
            Some(
                SpecialPayload::ProviderUpdateService(_)
                | SpecialPayload::ProviderUpdateRegistrar(_)
                | SpecialPayload::ProviderUpdateRevoke(_),
            ) => pro_tx_hash(&tx).map(|pro| {
                let valid = tip_list.get(&ProTxHash::from(*pro.as_bytes())).map(|dmn| {
                    bls_hash(BlsPkBytes::from_bytes(dmn.cold.pub_key_operator.to_bytes()))
                });
                (valid, self.key_change_target(&tx, tip_list).is_some())
            }),
            Some(SpecialPayload::ProviderUpdateSharedRegistrar(_)) => {
                pro_tx_hash(&tx).map(|_| (None, self.key_change_target(&tx, tip_list).is_some()))
            }
            _ => None,
        };
        self.dash.protx_index_context.insert(id, context.unwrap_or((None, false)));
        let is_key_change = self.key_change_target(&tx, tip_list).is_some();
        let map = &mut self.dash;
        match payload(&tx) {
            Some(SpecialPayload::MnhfSignal(_)) => {
                self.prioritise_transaction(id, 10_000_000);
            }
            Some(SpecialPayload::ProviderRegister(p)) => {
                if let Some(op) = collateral(&tx).filter(|op| !op.txid().is_null()) {
                    map.protx_refs.entry(id).or_default().push(op.txid());
                }
                for entry in net_entries(p.net_info()) {
                    map.addresses.insert(entry, id);
                }
                for owner in owner_ids(&tx) {
                    map.owner_ids.insert(owner, id);
                }
                if let Some(hash) = key_hash(&tx) {
                    map.bls_keys.insert(hash, id);
                }
                if let Some(platform) = platform_id(&tx) {
                    map.platform_ids.insert(platform, id);
                }
                let mut op = collateral(&tx).expect("registration collateral");
                if op.txid().is_null() {
                    op = dash_primitives::OutPoint::new(id, op.index());
                }
                map.collaterals.insert(op, id);
            }
            Some(SpecialPayload::ProviderUpdateService(p)) => {
                let pro = p.pro_tx_hash();
                map.protx_refs.entry(pro).or_default().push(id);
                for entry in net_entries(p.net_info()) {
                    map.addresses.insert(entry, id);
                }
                if let Some(platform) = platform_id(&tx) {
                    map.platform_ids.insert(platform, id);
                }
                if let Some(hash) = list_key_hash(tip_list, pro) {
                    map.valid_for_protx_key.insert(id, hash);
                }
            }
            Some(SpecialPayload::ProviderUpdateRegistrar(p)) => {
                let pro = p.pro_tx_hash();
                map.protx_refs.entry(pro).or_default().push(id);
                if let Some(hash) = key_hash(&tx) {
                    map.bls_keys.insert(hash, id);
                }
                if let Some(dmn) = tip_list.get(&ProTxHash::from(*pro.as_bytes())) {
                    map.valid_for_protx_key.insert(
                        id,
                        bls_hash(BlsPkBytes::from_bytes(dmn.cold.pub_key_operator.to_bytes())),
                    );
                    if is_key_change {
                        map.key_change_protx.insert(id);
                    }
                }
            }
            Some(SpecialPayload::ProviderUpdateRevoke(p)) => {
                let pro = p.pro_tx_hash();
                map.protx_refs.entry(pro).or_default().push(id);
                if let Some(dmn) = tip_list.get(&ProTxHash::from(*pro.as_bytes())) {
                    map.valid_for_protx_key.insert(
                        id,
                        bls_hash(BlsPkBytes::from_bytes(dmn.cold.pub_key_operator.to_bytes())),
                    );
                    if is_key_change {
                        map.key_change_protx.insert(id);
                    }
                }
            }
            Some(SpecialPayload::ProviderDissolve(p)) => {
                map.protx_refs.entry(p.pro_tx_hash()).or_default().push(id);
            }
            Some(SpecialPayload::ProviderUpdateShare(p)) => {
                map.protx_refs.entry(p.pro_tx_hash()).or_default().push(id);
            }
            Some(SpecialPayload::ProviderUpdateSharedRegistrar(p)) => {
                let pro = p.pro_tx_hash();
                map.protx_refs.entry(pro).or_default().push(id);
                if let Some(hash) = key_hash(&tx) {
                    map.bls_keys.insert(hash, id);
                }
                if tip_list.get(&ProTxHash::from(*pro.as_bytes())).is_some() && is_key_change {
                    map.key_change_protx.insert(id);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn remove_dash_maps_for_tx(&mut self, tx: &Tx) {
        let id = tx.txid();
        let map = &mut self.dash;
        match payload(tx) {
            Some(SpecialPayload::ProviderRegister(p)) => {
                if let Some(op) = collateral(tx).filter(|op| !op.txid().is_null()) {
                    remove_ref(&mut map.protx_refs, &id, op.txid());
                }
                for entry in net_entries(p.net_info()) {
                    remove_value(&mut map.addresses, &entry, id);
                }
                for owner in owner_ids(tx) {
                    remove_value(&mut map.owner_ids, &owner, id);
                }
                if let Some(hash) = key_hash(tx) {
                    remove_value(&mut map.bls_keys, &hash, id);
                }
                if let Some(platform) = platform_id(tx) {
                    remove_value(&mut map.platform_ids, &platform, id);
                }
                if let Some(mut op) = collateral(tx) {
                    if op.txid().is_null() {
                        op = dash_primitives::OutPoint::new(id, op.index());
                    }
                    remove_value(&mut map.collaterals, &op, id);
                }
            }
            Some(SpecialPayload::ProviderUpdateService(p)) => {
                remove_ref(&mut map.protx_refs, &p.pro_tx_hash(), id);
                for entry in net_entries(p.net_info()) {
                    remove_value(&mut map.addresses, &entry, id);
                }
                if let Some(platform) = platform_id(tx) {
                    remove_value(&mut map.platform_ids, &platform, id);
                }
            }
            Some(SpecialPayload::ProviderUpdateRegistrar(p)) => {
                remove_ref(&mut map.protx_refs, &p.pro_tx_hash(), id);
                if let Some(hash) = key_hash(tx) {
                    remove_value(&mut map.bls_keys, &hash, id);
                }
            }
            Some(SpecialPayload::ProviderUpdateRevoke(p)) => {
                remove_ref(&mut map.protx_refs, &p.pro_tx_hash(), id)
            }
            Some(SpecialPayload::ProviderDissolve(p)) => {
                remove_ref(&mut map.protx_refs, &p.pro_tx_hash(), id)
            }
            Some(SpecialPayload::ProviderUpdateShare(p)) => {
                remove_ref(&mut map.protx_refs, &p.pro_tx_hash(), id)
            }
            Some(SpecialPayload::ProviderUpdateSharedRegistrar(p)) => {
                remove_ref(&mut map.protx_refs, &p.pro_tx_hash(), id);
                if let Some(hash) = key_hash(tx) {
                    remove_value(&mut map.bls_keys, &hash, id);
                }
            }
            _ => {}
        }
        map.valid_for_protx_key.remove(&id);
        map.key_change_protx.remove(&id);
        map.protx_index_context.remove(&id);
    }

    pub fn exists_provider_tx_conflict(&self, tx: &Tx, tip_list: &MnList) -> bool {
        let pro_tx_type = matches!(
            tx.tx_type(),
            dash_primitives::TxType::ProviderRegister
                | dash_primitives::TxType::ProviderUpdateService
                | dash_primitives::TxType::ProviderUpdateRegistrar
                | dash_primitives::TxType::ProviderUpdateRevoke
                | dash_primitives::TxType::ProviderUpdateShare
                | dash_primitives::TxType::ProviderUpdateSharedRegistrar
        );
        let Some(p) = payload(tx) else {
            return pro_tx_type;
        };
        let owner_or_platform = |id: &[u8; 20]| {
            self.dash.owner_ids.contains_key(id) || self.dash.platform_ids.contains_key(id)
        };
        match p {
            SpecialPayload::ProviderRegister(p) => {
                if net_entries(p.net_info()).any(|entry| self.dash.addresses.contains_key(&entry)) {
                    return true;
                }
                if owner_ids(tx).iter().any(owner_or_platform) {
                    return true;
                }
                if key_hash(tx).is_some_and(|key| self.dash.bls_keys.contains_key(&key)) {
                    return true;
                }
                if platform_id(tx).is_some_and(|id| owner_or_platform(&id)) {
                    return true;
                }
                if let Some(op) = collateral(tx).filter(|op| !op.txid().is_null()) {
                    if self.dash.collaterals.contains_key(&op) || self.next_tx.contains_key(&op) {
                        return true;
                    }
                    if let Some(dmn) = tip_list.get_by_collateral(&op)
                        && self
                            .dash
                            .protx_refs
                            .contains_key(&TxHash::from(*dmn.pro_tx_hash.as_bytes()))
                    {
                        return true;
                    }
                }
            }
            SpecialPayload::ProviderUpdateService(p) => {
                if net_entries(p.net_info()).any(|entry| {
                    self.dash.addresses.get(&entry).is_some_and(|id| *id != p.pro_tx_hash())
                }) {
                    return true;
                }
                if platform_id(tx).is_some_and(|id| {
                    self.dash.platform_ids.get(&id).is_some_and(|owner| *owner != p.pro_tx_hash())
                        || self.dash.owner_ids.contains_key(&id)
                }) {
                    return true;
                }
                if let Some(dmn) = tip_list.get(&ProTxHash::from(*p.pro_tx_hash().as_bytes()))
                    && self.dash.collaterals.contains_key(&dmn.collateral)
                {
                    return true;
                }
            }
            SpecialPayload::ProviderUpdateRegistrar(p) => {
                let Some(dmn) = tip_list.get(&ProTxHash::from(*p.pro_tx_hash().as_bytes())) else {
                    return true;
                };
                if self.dash.collaterals.contains_key(&dmn.collateral) {
                    return true;
                }
                if Some(&dmn.cold.pub_key_operator) != operator_key(tx)
                    && self.has_key_change(p.pro_tx_hash())
                {
                    return true;
                }
                if key_hash(tx).is_some_and(|key| {
                    self.dash.bls_keys.get(&key).is_some_and(|id| *id != p.pro_tx_hash())
                }) {
                    return true;
                }
            }
            SpecialPayload::ProviderUpdateRevoke(p) => {
                let Some(dmn) = tip_list.get(&ProTxHash::from(*p.pro_tx_hash().as_bytes())) else {
                    return true;
                };
                if self.dash.collaterals.contains_key(&dmn.collateral) {
                    return true;
                }
                if dmn.cold.pub_key_operator.to_bytes().iter().any(|byte| *byte != 0)
                    && self.has_key_change(p.pro_tx_hash())
                {
                    return true;
                }
            }
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
                let Some(dmn) = tip_list.get(&ProTxHash::from(*p.pro_tx_hash().as_bytes())) else {
                    return true;
                };
                if Some(&dmn.cold.pub_key_operator) != operator_key(tx)
                    && self.has_key_change(p.pro_tx_hash())
                {
                    return true;
                }
                if key_hash(tx).is_some_and(|key| {
                    self.dash.bls_keys.get(&key).is_some_and(|id| *id != p.pro_tx_hash())
                }) {
                    return true;
                }
                if self.has_voting_payee_conflict(
                    p.pro_tx_hash(),
                    p.key_id_voting().as_bytes(),
                    true,
                ) {
                    return true;
                }
            }
            SpecialPayload::ProviderUpdateShare(p) => {
                if tip_list.get(&ProTxHash::from(*p.pro_tx_hash().as_bytes())).is_none() {
                    return true;
                }
                if let Some(owner) = reward_key_id(p.script_reward().as_bytes())
                    && self.has_voting_payee_conflict(p.pro_tx_hash(), &owner, false)
                {
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    fn has_key_change(&self, pro: TxHash) -> bool {
        self.dash
            .protx_refs
            .get(&pro)
            .is_some_and(|ids| ids.iter().any(|id| self.dash.key_change_protx.contains(id)))
    }

    fn has_voting_payee_conflict(
        &self,
        pro: TxHash,
        key: &[u8; 20],
        shared_registrar: bool,
    ) -> bool {
        self.dash.protx_refs.get(&pro).is_some_and(|ids| {
            ids.iter().filter_map(|id| self.entries.get(id)).any(|entry| match payload(&entry.tx) {
                Some(SpecialPayload::ProviderUpdateShare(p)) if shared_registrar => {
                    reward_key_id(p.script_reward().as_bytes()) == Some(*key)
                }
                Some(SpecialPayload::ProviderUpdateSharedRegistrar(p)) if !shared_registrar => {
                    p.key_id_voting().as_bytes() == key
                }
                _ => false,
            })
        })
    }

    pub fn exists_provider_tx_cross_scheme_conflict(&self, tx: &Tx, tip_list: &MnList) -> bool {
        let cross_scheme_type = matches!(
            tx.tx_type(),
            dash_primitives::TxType::ProviderRegister
                | dash_primitives::TxType::ProviderUpdateRegistrar
                | dash_primitives::TxType::ProviderUpdateSharedRegistrar
        );
        let Some(p) = payload(tx) else {
            return cross_scheme_type;
        };
        let self_pro = match p {
            SpecialPayload::ProviderRegister(_) => None,
            SpecialPayload::ProviderUpdateRegistrar(p) => Some(p.pro_tx_hash()),
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => Some(p.pro_tx_hash()),
            _ => return false,
        };
        if let Some(pro) = self_pro
            && tip_list.get(&ProTxHash::from(*pro.as_bytes())).is_some_and(|dmn| match p {
                SpecialPayload::ProviderUpdateRegistrar(_)
                | SpecialPayload::ProviderUpdateSharedRegistrar(_) => {
                    Some(&dmn.cold.pub_key_operator) == operator_key(tx)
                }
                _ => false,
            })
        {
            return false;
        }
        let Some(hashes) = key_hashes(tx) else {
            return false;
        };
        for hash in hashes.into_iter().flatten() {
            let Some(id) = self.dash.bls_keys.get(&hash) else {
                continue;
            };
            if *id == tx.txid() {
                continue;
            }
            if let Some(pro) = self.entries.get(id).and_then(|entry| pro_tx_hash(&entry.tx))
                && self_pro == Some(pro)
            {
                continue;
            }
            return true;
        }
        false
    }

    pub fn key_change_target(&self, tx: &Tx, tip_list: &MnList) -> Option<ProTxHash> {
        let p = payload(tx)?;
        let (pro, changes) = match p {
            SpecialPayload::ProviderUpdateRegistrar(p) => (
                p.pro_tx_hash(),
                tip_list
                    .get(&ProTxHash::from(*p.pro_tx_hash().as_bytes()))
                    .is_some_and(|dmn| Some(&dmn.cold.pub_key_operator) != operator_key(tx)),
            ),
            SpecialPayload::ProviderUpdateRevoke(p) => (
                p.pro_tx_hash(),
                tip_list.get(&ProTxHash::from(*p.pro_tx_hash().as_bytes())).is_some_and(|dmn| {
                    dmn.cold.pub_key_operator.to_bytes().iter().any(|byte| *byte != 0)
                }),
            ),
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => (
                p.pro_tx_hash(),
                tip_list
                    .get(&ProTxHash::from(*p.pro_tx_hash().as_bytes()))
                    .is_some_and(|dmn| Some(&dmn.cold.pub_key_operator) != operator_key(tx)),
            ),
            _ => return None,
        };
        changes.then_some(ProTxHash::from(*pro.as_bytes()))
    }

    pub fn is_unorderable_service_update(
        &self,
        tx: &Tx,
        ancestors: &[TxHash],
        tip_list: &MnList,
    ) -> bool {
        if !matches!(payload(tx), Some(SpecialPayload::ProviderUpdateService(_))) {
            return false;
        }
        ancestors.iter().any(|id| {
            self.entries.get(id).is_some_and(|entry| {
                self.key_change_target(&entry.tx, tip_list).is_some()
                    || matches!(
                        payload(&entry.tx),
                        Some(
                            SpecialPayload::AssetLock(_)
                                | SpecialPayload::AssetUnlock(_)
                                | SpecialPayload::MnhfSignal(_)
                        )
                    )
            })
        })
    }

    pub fn service_updates_before_key_change(
        &self,
        key_change: TxHash,
        tip_list: &MnList,
    ) -> Vec<TxHash> {
        let Some(pro) = self.key_change_target(&self.entries[&key_change].tx, tip_list) else {
            return Vec::new();
        };
        let descendants = self.calculate_descendants(key_change);
        self.dash
            .protx_refs
            .get(&TxHash::from(*pro.as_bytes()))
            .into_iter()
            .flat_map(|ids| ids.iter().copied())
            .filter(|id| {
                !descendants.contains(id)
                    && self.entries.get(id).is_some_and(|entry| {
                        matches!(payload(&entry.tx), Some(SpecialPayload::ProviderUpdateService(_)))
                    })
            })
            .collect()
    }

    pub fn remove_pro_tx_references(&mut self, pro: TxHash) {
        self.remove_pro_tx_references_clearing(pro, false);
    }

    /// Only removeConflicts clears each ref's delta before removing it (tm:967-983).
    pub(crate) fn remove_pro_tx_references_clearing(&mut self, pro: TxHash, clear_deltas: bool) {
        let ids: Vec<_> = self.dash.protx_refs.get(&pro).cloned().unwrap_or_default();
        for id in ids {
            if let Some(tx) = self.get(&id) {
                if clear_deltas {
                    self.clear_prioritisation(&id);
                }
                self.remove_recursive(&tx, MemPoolRemovalReason::Conflict);
            } else {
                self.dash.protx_refs.remove(&pro);
            }
        }
    }

    pub fn remove_pro_tx_conflicts(&mut self, tx: &Tx, tip_list: &MnList) {
        self.remove_pro_tx_spent_collateral_conflicts(tx, tip_list);
        let id = tx.txid();
        match payload(tx) {
            Some(SpecialPayload::ProviderRegister(p)) => {
                for entry in net_entries(p.net_info()) {
                    self.remove_claimant(&self.dash.addresses.get(&entry).copied(), id);
                }
                for owner in owner_ids(tx) {
                    self.remove_owner_platform_claimant(owner, id);
                }
                if let Some(hash) = key_hash(tx) {
                    self.remove_claimant(&self.dash.bls_keys.get(&hash).copied(), id);
                }
                if let Some(platform) = platform_id(tx) {
                    self.remove_platform_node_conflicts(platform, id);
                }
                if let Some(mut op) = collateral(tx) {
                    if op.txid().is_null() {
                        op = dash_primitives::OutPoint::new(id, op.index());
                    }
                    self.remove_claimant(&self.dash.collaterals.get(&op).copied(), id);
                    if !op.txid().is_null()
                        && let Some(dmn) = tip_list.get_by_collateral(&op)
                    {
                        self.remove_pro_tx_references(TxHash::from(*dmn.pro_tx_hash.as_bytes()));
                    }
                }
            }
            Some(SpecialPayload::ProviderUpdateService(p)) => {
                for entry in net_entries(p.net_info()) {
                    self.remove_claimant(&self.dash.addresses.get(&entry).copied(), id);
                }
                if let Some(platform) = platform_id(tx) {
                    self.remove_platform_node_conflicts(platform, id);
                }
            }
            Some(SpecialPayload::ProviderUpdateRegistrar(p)) => {
                if let Some(hash) = key_hash(tx) {
                    self.remove_claimant(&self.dash.bls_keys.get(&hash).copied(), id);
                    self.remove_pro_tx_key_changed_conflicts(p.pro_tx_hash(), hash);
                }
            }
            Some(SpecialPayload::ProviderUpdateRevoke(p)) => {
                self.remove_pro_tx_key_changed_conflicts(p.pro_tx_hash(), zero_key_hash());
            }
            Some(SpecialPayload::ProviderUpdateSharedRegistrar(p)) => {
                if let Some(hash) = key_hash(tx) {
                    self.remove_claimant(&self.dash.bls_keys.get(&hash).copied(), id);
                    self.remove_pro_tx_key_changed_conflicts(p.pro_tx_hash(), hash);
                }
                self.remove_pro_tx_voting_payee_conflicts(
                    p.pro_tx_hash(),
                    p.key_id_voting().as_bytes(),
                    true,
                );
            }
            Some(SpecialPayload::ProviderUpdateShare(p)) => {
                if let Some(owner) = reward_key_id(p.script_reward().as_bytes()) {
                    self.remove_pro_tx_voting_payee_conflicts(p.pro_tx_hash(), &owner, false);
                }
            }
            _ => {}
        }
    }

    fn remove_pro_tx_spent_collateral_conflicts(&mut self, tx: &Tx, tip_list: &MnList) {
        for input in tx.inputs() {
            if let Some(pro) = self.dash.collaterals.get(&input.prevout()).copied() {
                self.remove_pro_tx_references(pro);
            }
            if let Some(dmn) = tip_list.get_by_collateral(&input.prevout()) {
                self.remove_pro_tx_references(TxHash::from(*dmn.pro_tx_hash.as_bytes()));
            }
        }
    }

    fn remove_pro_tx_key_changed_conflicts(&mut self, pro: TxHash, new_key: [u8; 32]) {
        let ids: Vec<_> = self.dash.protx_refs.get(&pro).cloned().unwrap_or_default();
        for id in ids {
            let Some(entry) = self.entries.get(&id) else {
                continue;
            };
            if matches!(
                payload(&entry.tx),
                Some(
                    SpecialPayload::ProviderDissolve(_)
                        | SpecialPayload::ProviderUpdateShare(_)
                        | SpecialPayload::ProviderUpdateSharedRegistrar(_)
                )
            ) {
                continue;
            }
            if self.dash.valid_for_protx_key.get(&id).is_none_or(|old| *old != new_key) {
                let tx = entry.tx.clone();
                self.remove_recursive(&tx, MemPoolRemovalReason::Conflict);
            }
        }
    }

    fn remove_pro_tx_voting_payee_conflicts(
        &mut self,
        pro: TxHash,
        key: &[u8; 20],
        shared_registrar: bool,
    ) {
        let ids: Vec<_> = self.dash.protx_refs.get(&pro).cloned().unwrap_or_default();
        for id in ids {
            let Some(entry) = self.entries.get(&id) else {
                continue;
            };
            let conflict = match payload(&entry.tx) {
                Some(SpecialPayload::ProviderUpdateShare(p)) if shared_registrar => {
                    reward_key_id(p.script_reward().as_bytes()) == Some(*key)
                }
                Some(SpecialPayload::ProviderUpdateSharedRegistrar(p)) if !shared_registrar => {
                    p.key_id_voting().as_bytes() == key
                }
                _ => false,
            };
            if conflict {
                let tx = entry.tx.clone();
                self.remove_recursive(&tx, MemPoolRemovalReason::Conflict);
            }
        }
    }

    fn remove_claimant(&mut self, claimant: &Option<TxHash>, current: TxHash) {
        if let Some(id) = claimant.filter(|id| *id != current)
            && let Some(tx) = self.get(&id)
        {
            self.remove_recursive(&tx, MemPoolRemovalReason::Conflict);
        }
    }

    fn remove_owner_platform_claimant(&mut self, owner: [u8; 20], current: TxHash) {
        let owner_claimant = self.dash.owner_ids.get(&owner).copied();
        self.remove_claimant(&owner_claimant, current);
        let platform_claimant = self.dash.platform_ids.get(&owner).copied();
        self.remove_claimant(&platform_claimant, current);
    }

    fn remove_platform_node_conflicts(&mut self, platform: [u8; 20], current: TxHash) {
        let platform_claimant = self.dash.platform_ids.get(&platform).copied();
        self.remove_claimant(&platform_claimant, current);
        let owner_claimant = self.dash.owner_ids.get(&platform).copied();
        self.remove_claimant(&owner_claimant, current);
    }
}

fn remove_ref(map: &mut std::collections::BTreeMap<TxHash, Vec<TxHash>>, key: &TxHash, id: TxHash) {
    if let Some(ids) = map.get_mut(key) {
        ids.retain(|candidate| *candidate != id);
        if ids.is_empty() {
            map.remove(key);
        }
    }
}

fn remove_value<K: std::hash::Hash + Eq>(map: &mut HashMap<K, TxHash>, key: &K, id: TxHash) {
    if map.get(key).is_some_and(|value| *value == id) {
        map.remove(key);
    }
}
