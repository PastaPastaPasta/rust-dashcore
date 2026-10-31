//! The reference twin (EV2 §3.4, A1 §3.10; EV2b D24): Core's `CDeterministicMNList` over Core's
//! data layout, and `build_new_list_from_block_ref` over it. It is the `-consensusxcheck`
//! partner of [`MnList`] and the benchmark baseline of the build.
//!
//! [`RefMnList`] keeps Core's three maps (`mnMap`, `mnInternalIdMap`, and `mnUniquePropertyMap`
//! with its reference counts) and immer's collision buckets, and nothing else: no index, no
//! cached order or counts, no cached unique keys. Its operations are Core's, line by line:
//! `AddMN`/`UpdateMN`/`RemoveMN` recompute a state's unique keys each time, `GetCounts` and
//! `CalcMaxPoSePenalty` walk the map, the payee is `scan.rs`'s literal `GetMNPayee`, and
//! every bulk phase iterates a Core-order snapshot and `set`s each eligible masternode in that
//! order (EV2 §2.7, R2-F1). [`xcheck`] compares a fast list with a twin.
//!
//! Sharing (EV2b D24, extending EV2 §3.4's contract): the payload decoder, the canonical encoder
//! and `scan.rs` as EV2 says; the masternode types and their state methods; the unique-key
//! pre-images ([`UniqueKey`], `StateKeys`); `order.rs`'s CHAMP walk; and the
//! special-transaction handlers of `build/mod.rs`, which run over either list through
//! `BuildList`. Copying those handlers would copy their bugs too; `diff_dml_apply` checks them
//! against Core directly. What the twin checks is the data structure: the indexes, the payee
//! queue, the phase orders, the counts, the unique map and the buckets.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use dash_consensus::coins::CoinsView;
use dash_consensus::primitives::Block;
use dash_evo::effect::EffectSink;
use dash_num::Hash256;
use dash_primitives::BlockHash;
use imbl::OrdMap;

use crate::build::{self, BuildList, BuiltList, DmlBuildError, DmlRules, PrevBlock};
use crate::codec::{self, CodecError, core_disk};
use crate::list::{CollisionBuckets, MnList, MnListError};
use crate::payee::PayeeRules;
use crate::scan::{self, ScanList};
use crate::store::DmlMemberSource;
use crate::trace::{DmlEvent, DmlTrace};
use crate::types::{Counts, Dmn, MnCold, MnHot};
use crate::unique::{NetProp, Prop, StateKeys, UniqueKey, key_id_prop};
use crate::{ProTxHash, order};

/// Core's `CDeterministicMNList`, Core's layout (module docs).
#[derive(Clone)]
pub struct RefMnList {
    block_hash: BlockHash,
    height: i32,
    total_registered: u32,
    /// `mnMap`.
    mn_map: OrdMap<ProTxHash, Arc<Dmn>>,
    /// `mnInternalIdMap`.
    internal_ids: OrdMap<u64, ProTxHash>,
    /// `mnUniquePropertyMap`: the owner and the reference count of each property.
    unique: imbl::HashMap<UniqueKey, (ProTxHash, u32)>,
    /// immer's collision nodes: the keys sharing a 64-bit hash, front first.
    buckets: CollisionBuckets,
}

impl RefMnList {
    /// The twin of `list`: its position, counter, masternodes, internal ids and buckets, with
    /// the unique map rebuilt by `AddMN`'s property steps. An error is a list whose properties
    /// collide (I-U broken).
    pub fn from_list(list: &MnList) -> Result<RefMnList, MnListError> {
        let mut twin = RefMnList {
            block_hash: *list.block_hash(),
            height: list.height(),
            total_registered: list.total_registered(),
            mn_map: OrdMap::new(),
            internal_ids: OrdMap::new(),
            unique: imbl::HashMap::new(),
            buckets: list.collision_buckets_map().clone(),
        };
        for dmn in list.iter() {
            twin.add_props(dmn)?;
            twin.mn_map.insert(dmn.pro_tx_hash, Arc::clone(dmn));
            twin.internal_ids.insert(dmn.internal_id, dmn.pro_tx_hash);
        }
        Ok(twin)
    }

    /// `CDeterministicMNList::Unserialize` (`deterministicmns.h` v24:246-256) into the twin: each
    /// entry of the evodb bytes through the twin's `AddMN(dmn, false)` in the file's order, so
    /// its buckets follow its own immer rules, not the fast reader's.
    // Core: evo/deterministicmns.h:246 CDeterministicMNList::Unserialize
    pub fn decode_s3(input: &mut &[u8]) -> Result<RefMnList, CodecError> {
        let (block_hash, height, total_registered, n) = core_disk::read_list_head(input)?;
        let mut twin = RefMnList {
            block_hash,
            height,
            total_registered,
            mn_map: OrdMap::new(),
            internal_ids: OrdMap::new(),
            unique: imbl::HashMap::new(),
            buckets: OrdMap::new(),
        };
        for _ in 0..n {
            let dmn = core_disk::decode_dmn(input)?;
            twin.try_add(Arc::new(dmn), false).map_err(|e| CodecError::new(e.to_string()))?;
        }
        Ok(twin)
    }

    /// `GetBlockHash()`.
    pub fn block_hash(&self) -> &BlockHash {
        &self.block_hash
    }

    /// The twin of [`MnList::default_pre_dip3`].
    pub fn default_pre_dip3() -> RefMnList {
        RefMnList::from_list(&MnList::default_pre_dip3()).expect("the default list is empty")
    }

    /// `SetBlockHash(h)`, as `ProcessBlock` sets it on the built list.
    pub fn with_block_hash(mut self, block_hash: BlockHash) -> Self {
        self.block_hash = block_hash;
        self
    }

    /// `GetHeight()`.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// `GetTotalRegisteredCount()`.
    pub fn total_registered(&self) -> u32 {
        self.total_registered
    }

    /// `GetAllMNsCount()`.
    pub fn len(&self) -> usize {
        self.mn_map.len()
    }

    /// Whether the list holds no masternode.
    pub fn is_empty(&self) -> bool {
        self.mn_map.is_empty()
    }

    /// `GetMN(proTxHash)`.
    pub fn get(&self, pro_tx: &ProTxHash) -> Option<&Arc<Dmn>> {
        self.mn_map.get(pro_tx)
    }

    /// `ForEachMN`'s order: the CHAMP walk over the keys and the buckets, computed on each call.
    pub fn core_order(&self) -> Arc<[ProTxHash]> {
        order::core_order(self.mn_map.keys(), &self.buckets).into()
    }

    /// `GetCounts()`: a walk over the map (`deterministicmns.h` v24:269-290), written out rather
    /// than through the fast list's incremental [`Counts`] step.
    // Core: evo/deterministicmns.h:269 CDeterministicMNList::GetCounts
    pub fn counts(&self) -> Counts {
        let mut ret = Counts::default();
        for dmn in self.mn_map.values() {
            let is_evo = dmn.mn_type.0 == 1;
            let is_valid = !dmn.hot.is_banned();
            // `GetMnType(nType).voting_weight` (`dmn_types.h` v24:30-62).
            let weight = match dmn.mn_type.0 {
                0 => 1,
                1 => 4,
                _ => 0,
            };
            if is_evo {
                ret.total_evo += 1;
                if is_valid {
                    ret.valid_evo += 1;
                }
            } else {
                ret.total_mn += 1;
                if is_valid {
                    ret.valid_mn += 1;
                }
            }
            if is_valid {
                ret.valid_weighted += weight;
            }
            ret.total_weighted += weight;
        }
        ret
    }

    /// `GetMNPayee(pindexPrev)`: the literal scan.
    pub fn payee(&self, rules: PayeeRules) -> Option<&Arc<Dmn>> {
        scan::payee_scan(self, rules)
    }

    /// `GetProjectedMNPayees(pindexPrev, count)`: the literal scan.
    pub fn projected_payees(&self, rules: PayeeRules, count: usize) -> Vec<Arc<Dmn>> {
        scan::projected_scan(self, rules, count)
    }

    /// `AddUniqueProperty(dmn, v)` (`deterministicmns.h` v24:550-568).
    // Core: evo/deterministicmns.h:550 CDeterministicMNList::AddUniqueProperty
    fn add_unique(&mut self, prop: &Prop, owner: &ProTxHash) -> bool {
        if prop.null {
            return false;
        }
        let refs = match self.unique.get(&prop.key) {
            Some((holder, _)) if holder != owner => return false,
            Some((_, refs)) => refs + 1,
            None => 1,
        };
        self.unique.insert(prop.key.clone(), (*owner, refs));
        true
    }

    /// `DeleteUniqueProperty(dmn, v)` (`deterministicmns.h` v24:570-587).
    // Core: evo/deterministicmns.h:570 CDeterministicMNList::DeleteUniqueProperty
    fn delete_unique(&mut self, prop: &Prop, owner: &ProTxHash) -> bool {
        if prop.null {
            return false;
        }
        let Some(&(holder, refs)) = self.unique.get(&prop.key) else {
            return false;
        };
        if holder != *owner {
            return false;
        }
        if refs == 1 {
            self.unique.remove(&prop.key);
        } else {
            self.unique.insert(prop.key.clone(), (holder, refs - 1));
        }
        true
    }

    /// `UpdateUniqueProperty(dmn, old, new)` (`deterministicmns.h` v24:589-612): equal values
    /// (equal pre-images; for BLS keys Core compares `GetUniquePropertyHash`) keep their slot.
    // Core: evo/deterministicmns.h:589 CDeterministicMNList::UpdateUniqueProperty
    fn update_unique(&mut self, old: &Prop, new: &Prop, owner: &ProTxHash) -> bool {
        if old.key == new.key {
            return true;
        }
        if !old.null && !self.delete_unique(old, owner) {
            return false;
        }
        new.null || self.add_unique(new, owner)
    }

    /// `AddMN`'s property steps, in Core's order (v24:450-502).
    fn add_props(&mut self, dmn: &Dmn) -> Result<(), MnListError> {
        let pro_tx = dmn.pro_tx_hash;
        if !self.add_unique(&Prop::collateral(&dmn.collateral), &pro_tx) {
            return Err(MnListError::DuplicateCollateral {
                pro_tx,
                collateral: dmn.collateral,
            });
        }
        let keys = StateKeys::build(&dmn.cold);
        for entry in keys.net.iter() {
            match entry {
                NetProp::Addr {
                    prop,
                    shown,
                } => {
                    if !self.add_unique(prop, &pro_tx) {
                        return Err(MnListError::DuplicateAddress {
                            pro_tx,
                            address: shown.clone(),
                        });
                    }
                }
                NetProp::Invalid => return Err(MnListError::InvalidAddress(pro_tx)),
            }
        }
        if dmn.cold.is_shared() {
            for share in &dmn.cold.shares {
                if !self.add_unique(&key_id_prop(&share.key_id_owner), &pro_tx) {
                    return Err(MnListError::DuplicateShareOwner(pro_tx));
                }
            }
        } else if !self.add_unique(&key_id_prop(&dmn.cold.key_id_owner), &pro_tx) {
            return Err(MnListError::DuplicateOwner(pro_tx));
        }
        if !keys.operator.null && !self.add_unique(&keys.operator, &pro_tx) {
            return Err(MnListError::DuplicateOperator(pro_tx));
        }
        if dmn.is_evo() && !keys.platform.null && !self.add_unique(&keys.platform, &pro_tx) {
            return Err(MnListError::DuplicatePlatformNodeId(pro_tx));
        }
        Ok(())
    }

    /// `RemoveMN`'s property steps, in Core's order (v24:620-668).
    fn delete_props(&mut self, dmn: &Dmn) -> Result<(), MnListError> {
        let pro_tx = dmn.pro_tx_hash;
        let fail = |what| MnListError::Delete {
            pro_tx,
            what,
        };
        if !self.delete_unique(&Prop::collateral(&dmn.collateral), &pro_tx) {
            return Err(fail("collateralOutpoint"));
        }
        let keys = StateKeys::build(&dmn.cold);
        for entry in keys.net.iter() {
            match entry {
                NetProp::Addr {
                    prop,
                    ..
                } => {
                    if !self.delete_unique(prop, &pro_tx) {
                        return Err(fail("address"));
                    }
                }
                NetProp::Invalid => return Err(MnListError::DeleteInvalidAddress(pro_tx)),
            }
        }
        if dmn.cold.is_shared() {
            for share in &dmn.cold.shares {
                if !self.delete_unique(&key_id_prop(&share.key_id_owner), &pro_tx) {
                    return Err(fail("share ownerKeyID"));
                }
            }
        } else if !self.delete_unique(&key_id_prop(&dmn.cold.key_id_owner), &pro_tx) {
            return Err(fail("keyIDOwner"));
        }
        if !keys.operator.null && !self.delete_unique(&keys.operator, &pro_tx) {
            return Err(fail("pubKeyOperator"));
        }
        if dmn.is_evo() && !keys.platform.null && !self.delete_unique(&keys.platform, &pro_tx) {
            return Err(fail("platformNodeID"));
        }
        Ok(())
    }

    /// `UpdateMN`'s property steps from `old`'s state to `new` (v24:518-590).
    fn update_props(&mut self, old: &Dmn, new: &MnCold) -> Result<(), MnListError> {
        let pro_tx = old.pro_tx_hash;
        let (before, after) = (StateKeys::build(&old.cold), StateKeys::build(new));
        if old.cold.net_info != new.net_info {
            let fail = |reason: String| MnListError::UpdateAddresses {
                pro_tx,
                reason,
            };
            for entry in before.net.iter() {
                match entry {
                    NetProp::Addr {
                        prop,
                        ..
                    } => {
                        if !self.delete_unique(prop, &pro_tx) {
                            return Err(fail("internal error".into()));
                        }
                    }
                    NetProp::Invalid => return Err(fail("invalid address".into())),
                }
            }
            for entry in after.net.iter() {
                match entry {
                    NetProp::Addr {
                        prop,
                        shown,
                    } => {
                        if !self.add_unique(prop, &pro_tx) {
                            return Err(fail(format!("duplicate ({shown})")));
                        }
                    }
                    NetProp::Invalid => return Err(fail("invalid address".into())),
                }
            }
        }
        let (old_owner, new_owner) =
            (key_id_prop(&old.cold.key_id_owner), key_id_prop(&new.key_id_owner));
        if !self.update_unique(&old_owner, &new_owner, &pro_tx) {
            return Err(MnListError::UpdateOwner(pro_tx));
        }
        if !self.update_unique(&before.operator, &after.operator, &pro_tx) {
            return Err(MnListError::UpdateOperator(pro_tx));
        }
        if old.is_evo() && !self.update_unique(&before.platform, &after.platform, &pro_tx) {
            return Err(MnListError::UpdatePlatformNodeId(pro_tx));
        }
        Ok(())
    }

    /// The keys of the map whose 64-bit hash is `hash`.
    fn with_hash(&self, hash: u64) -> impl Iterator<Item = &ProTxHash> {
        let (mut lo, mut hi) = ([0u8; 32], [0xffu8; 32]);
        lo[..8].copy_from_slice(&hash.to_le_bytes());
        hi[..8].copy_from_slice(&hash.to_le_bytes());
        self.mn_map.range(ProTxHash::from(lo)..=ProTxHash::from(hi)).map(|(k, _)| k)
    }

    /// `mnMap.set(k, v)`, with immer's collision-node rules (`node.hpp`
    /// `copy_collision_insert` prepends, `copy_collision_replace` moves to the front; a second
    /// key with one hash makes a node of the new key and the old one).
    fn map_set(&mut self, dmn: Arc<Dmn>) {
        let pro_tx = dmn.pro_tx_hash;
        let hash = pro_tx.immer_hash();
        self.mn_map.insert(pro_tx, dmn);
        let mut bucket = VecDeque::from([pro_tx]);
        match self.buckets.get(&hash) {
            Some(old) => bucket.extend(old.iter().filter(|k| **k != pro_tx)),
            None => bucket.extend(self.with_hash(hash).filter(|k| **k != pro_tx)),
        }
        if bucket.len() > 1 {
            self.buckets.insert(hash, Arc::new(bucket));
        }
    }

    /// `mnMap.erase(k)`: a collision node of one survivor is inlined again (`champ.hpp`
    /// `do_sub`).
    fn map_erase(&mut self, pro_tx: &ProTxHash) {
        self.mn_map.remove(pro_tx);
        let hash = pro_tx.immer_hash();
        if let Some(old) = self.buckets.get(&hash) {
            let rest: VecDeque<ProTxHash> = old.iter().filter(|k| *k != pro_tx).copied().collect();
            if rest.len() < 2 {
                self.buckets.remove(&hash);
            } else {
                self.buckets.insert(hash, Arc::new(rest));
            }
        }
    }

    /// `f`, with the unique map restored if it throws (`AddMN`, `UpdateMN` and `RemoveMN` save
    /// `mnUniquePropertyMap` and restore it in their `catch`).
    fn restoring_unique(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<(), MnListError>,
    ) -> Result<(), MnListError> {
        let saved = self.unique.clone();
        f(self).inspect_err(|_| self.unique = saved)
    }

    /// `CalcMaxPoSePenalty()` (v24:267-273).
    // Core: evo/deterministicmns.cpp:267 CDeterministicMNList::CalcMaxPoSePenalty
    fn calc_max_pose_penalty(&self) -> i32 {
        100.max(self.counts().total() as i32)
    }
}

/// A state outside Core's state domain is refused before anything moves, as [`MnList`] does (EV2a
/// D17; not a Core error).
fn in_state_domain(pro_tx: &ProTxHash, cold: &MnCold) -> Result<(), MnListError> {
    match cold.version_hidden_field() {
        None => Ok(()),
        Some(field) => Err(MnListError::VersionHiddenField {
            pro_tx: *pro_tx,
            version: cold.version,
            field,
        }),
    }
}

impl ScanList for RefMnList {
    fn height(&self) -> i32 {
        RefMnList::height(self)
    }

    fn is_empty(&self) -> bool {
        RefMnList::is_empty(self)
    }

    fn core_order(&self) -> Arc<[ProTxHash]> {
        RefMnList::core_order(self)
    }

    fn get(&self, pro_tx: &ProTxHash) -> Option<&Arc<Dmn>> {
        RefMnList::get(self, pro_tx)
    }

    fn counts(&self) -> Counts {
        RefMnList::counts(self)
    }
}

impl BuildList for RefMnList {
    fn block_hash(&self) -> &BlockHash {
        RefMnList::block_hash(self)
    }

    fn is_default(&self) -> bool {
        self.block_hash.is_null()
            && self.height == -1
            && self.total_registered == 0
            && self.mn_map.is_empty()
    }

    fn start_build(&self, height: i32) -> Self {
        RefMnList {
            block_hash: BlockHash::from([0; 32]),
            height,
            ..self.clone()
        }
    }

    fn total_registered(&self) -> u32 {
        RefMnList::total_registered(self)
    }

    fn has_unique(&self, key: &UniqueKey) -> bool {
        self.unique.contains_key(key)
    }

    fn unique_owner(&self, key: &UniqueKey) -> Option<&Arc<Dmn>> {
        self.unique.get(key).and_then(|(owner, _)| self.mn_map.get(owner))
    }

    /// `AddMN(dmn, fBumpTotalCount)` (v24:439-516): the unique map is restored on a throw.
    // Core: evo/deterministicmns.cpp:439 CDeterministicMNList::AddMN
    fn try_add(&mut self, dmn: Arc<Dmn>, bump_total: bool) -> Result<(), MnListError> {
        let pro_tx = dmn.pro_tx_hash;
        in_state_domain(&pro_tx, &dmn.cold)?;
        if self.mn_map.contains_key(&pro_tx) {
            return Err(MnListError::DuplicateProTxHash(pro_tx));
        }
        if self.internal_ids.contains_key(&dmn.internal_id) {
            return Err(MnListError::DuplicateInternalId(dmn.internal_id));
        }
        self.restoring_unique(|twin| twin.add_props(&dmn))?;
        self.internal_ids.insert(dmn.internal_id, pro_tx);
        if bump_total {
            let next = dmn.internal_id.wrapping_add(1).max(u64::from(self.total_registered));
            self.total_registered = next as u32;
        }
        self.map_set(dmn);
        Ok(())
    }

    /// `UpdateMN(proTxHash, pdmnState)` (v24:518-610): the unique map is restored on a throw.
    // Core: evo/deterministicmns.cpp:518 CDeterministicMNList::UpdateMN
    fn update(
        &mut self,
        pro_tx: &ProTxHash,
        hot: MnHot,
        cold: Arc<MnCold>,
    ) -> Result<(), MnListError> {
        let old =
            Arc::clone(self.mn_map.get(pro_tx).ok_or(MnListError::NotFound("UpdateMN", *pro_tx))?);
        in_state_domain(pro_tx, &cold)?;
        self.restoring_unique(|twin| twin.update_props(&old, &cold))?;
        self.map_set(Arc::new(Dmn {
            hot,
            cold,
            ..(*old).clone()
        }));
        Ok(())
    }

    fn update_hot(&mut self, pro_tx: &ProTxHash, hot: MnHot) -> Result<(), MnListError> {
        let cold = Arc::clone(
            &self.mn_map.get(pro_tx).ok_or(MnListError::NotFound("UpdateMN", *pro_tx))?.cold,
        );
        self.update(pro_tx, hot, cold)
    }

    /// `RemoveMN(proTxHash)` (v24:612-679).
    // Core: evo/deterministicmns.cpp:612 CDeterministicMNList::RemoveMN
    fn remove(&mut self, pro_tx: &ProTxHash) -> Result<(), MnListError> {
        let dmn =
            Arc::clone(self.mn_map.get(pro_tx).ok_or(MnListError::NotFound("RemoveMN", *pro_tx))?);
        self.restoring_unique(|twin| twin.delete_props(&dmn))?;
        self.map_erase(pro_tx);
        self.internal_ids.remove(&dmn.internal_id);
        Ok(())
    }

    /// `CalcPenalty(percent)` (v24:275-279).
    // Core: evo/deterministicmns.cpp:275 CDeterministicMNList::CalcPenalty
    fn calc_penalty(&self, percent: i32) -> i32 {
        self.calc_max_pose_penalty().wrapping_mul(percent) / 100
    }

    /// `PoSePunish(proTxHash, penalty)` (v24:281-307).
    // Core: evo/deterministicmns.cpp:281 CDeterministicMNList::PoSePunish
    fn pose_punish(
        &mut self,
        pro_tx: &ProTxHash,
        penalty: i32,
        trace: &mut dyn DmlTrace,
    ) -> Result<(), MnListError> {
        let dmn = Arc::clone(
            self.mn_map.get(pro_tx).ok_or(MnListError::NotFound("PoSePunish", *pro_tx))?,
        );
        let max = self.calc_max_pose_penalty();
        let mut hot = dmn.hot;
        hot.pose_penalty = hot.pose_penalty.wrapping_add(penalty);
        hot.pose_penalty = max.min(hot.pose_penalty);
        if !dmn.hot.is_banned() {
            if hot.pose_penalty >= max {
                hot.ban_if_not_banned(self.height);
            }
            trace.event(DmlEvent::Punished {
                pro_tx: *pro_tx,
                height: self.height,
                banned: hot.is_banned(),
                from: dmn.hot.pose_penalty,
                to: hot.pose_penalty,
                max,
            });
        }
        self.update_hot(pro_tx, hot)
    }

    fn payee(&self, rules: PayeeRules) -> Option<Arc<Dmn>> {
        RefMnList::payee(self, rules).cloned()
    }

    /// `prevList.ForEachMN(false, …)`: each unconfirmed masternode of `prev` old enough gets
    /// `prev`'s state with the confirmed hash (v24:330-343).
    fn confirm(
        &mut self,
        prev: &Self,
        prev_height: i32,
        min_conf: i32,
        confirmed: &Hash256,
    ) -> Result<(), MnListError> {
        for pro_tx in prev.core_order().iter() {
            let Some(dmn) = prev.get(pro_tx) else {
                continue;
            };
            if !dmn.cold.confirmed_hash.is_null() {
                continue;
            }
            if prev_height.wrapping_sub(dmn.cold.registered_height) >= min_conf {
                let mut cold = (*dmn.cold).clone();
                cold.update_confirmed_hash(pro_tx, confirmed);
                self.update(pro_tx, dmn.hot, Arc::new(cold))?;
            }
        }
        Ok(())
    }

    /// `DecreaseScores()` (v24:309-334): `toDecrease` by `ForEachMNShared(true, …)`, then
    /// `PoSeDecrease` of each.
    // Core: evo/deterministicmns.cpp:309 CDeterministicMNList::DecreaseScores
    fn decrease_scores(&mut self) -> Result<(), MnListError> {
        let to_decrease: Vec<Arc<Dmn>> = self
            .core_order()
            .iter()
            .filter_map(|pro_tx| self.get(pro_tx))
            .filter(|dmn| dmn.is_valid() && dmn.hot.pose_penalty > 0)
            .cloned()
            .collect();
        for dmn in to_decrease {
            let mut hot = dmn.hot;
            hot.pose_penalty -= 1;
            self.update_hot(&dmn.pro_tx_hash, hot)?;
        }
        Ok(())
    }

    /// `newList2.ForEachMN(false, …)` over a copy, updating `newList` (v24:777-790).
    fn reset_streaks(
        &mut self,
        payee: Option<ProTxHash>,
        mn_rr: bool,
        trace: &mut dyn DmlTrace,
    ) -> Result<(), MnListError> {
        let snapshot = self.clone();
        for pro_tx in snapshot.core_order().iter() {
            let Some(dmn) = snapshot.get(pro_tx) else {
                continue;
            };
            if !dmn.is_evo()
                || (payee == Some(*pro_tx) && !mn_rr)
                || dmn.hot.consecutive_payments == 0
            {
                continue;
            }
            trace.event(DmlEvent::StreakReset {
                pro_tx: *pro_tx,
                from: dmn.hot.consecutive_payments,
            });
            let mut hot = dmn.hot;
            hot.consecutive_payments = 0;
            self.update_hot(pro_tx, hot)?;
        }
        Ok(())
    }
}

/// `build_new_list_from_block` over the twin (module docs).
#[allow(clippy::too_many_arguments, reason = "Core's parameters, EV2 §2.11")]
pub fn build_new_list_from_block_ref<C: CoinsView + ?Sized>(
    block: &Block,
    prev: PrevBlock<'_>,
    prev_list: &RefMnList,
    coins: &C,
    members: &DmlMemberSource<'_>,
    rules: &DmlRules<'_>,
    effects: &mut dyn EffectSink,
    trace: &mut dyn DmlTrace,
) -> Result<BuiltList<RefMnList>, DmlBuildError> {
    build::build(block, prev, prev_list, coins, members, rules, effects, trace)
}

/// Where a fast list and its twin differ ([`xcheck`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XcheckMismatch(pub &'static str);

impl fmt::Display for XcheckMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for XcheckMismatch {}

/// The `-consensusxcheck` comparison (EV2 §2.4, §6.4): `fast`'s invariants, then position,
/// counter, every masternode's bytes (internal id included) and the internal-id map, the unique
/// map, the collision buckets, `core_order()`, the counts, and the payee and the full projection
/// under `rules`, all equal to the twin's.
pub fn xcheck(fast: &MnList, twin: &RefMnList, rules: PayeeRules) -> Result<(), XcheckMismatch> {
    let fail = |what| Err(XcheckMismatch(what));
    fast.check_invariants().map_err(|e| XcheckMismatch(e.0))?;
    if fast.block_hash() != twin.block_hash() || fast.height() != twin.height() {
        return fail("xcheck position");
    }
    if fast.total_registered() != twin.total_registered() {
        return fail("xcheck total registered");
    }
    let bytes = |dmn: &Dmn| {
        let mut out = Vec::new();
        codec::write_dmn(dmn, &mut out);
        out
    };
    if fast.len() != twin.len()
        || fast.iter().zip(twin.mn_map.values()).any(|(a, b)| bytes(a) != bytes(b))
    {
        return fail("xcheck masternodes");
    }
    if twin.internal_ids.iter().any(|(id, pro_tx)| {
        fast.get_by_internal_id(*id).is_none_or(|dmn| dmn.pro_tx_hash != *pro_tx)
    }) || twin.internal_ids.len() != fast.len()
    {
        return fail("xcheck internal ids");
    }
    let unique = fast.unique_map();
    if unique.len() != twin.unique.len()
        || twin.unique.iter().any(|(key, (owner, refs))| {
            unique.get(key).is_none_or(|slot| slot.owner != *owner || slot.refs != *refs)
        })
    {
        return fail("xcheck unique properties");
    }
    if fast.collision_buckets_map() != &twin.buckets {
        return fail("xcheck collision buckets");
    }
    if fast.core_order() != twin.core_order() {
        return fail("xcheck core order");
    }
    if fast.counts() != twin.counts() {
        return fail("xcheck counts");
    }
    let id = |dmn: Option<&Arc<Dmn>>| dmn.map(|d| d.pro_tx_hash);
    if id(fast.payee(rules)) != id(twin.payee(rules)) {
        return fail("xcheck payee");
    }
    let all = fast.len().saturating_mul(4);
    let ids = |v: Vec<Arc<Dmn>>| v.iter().map(|d| d.pro_tx_hash).collect::<Vec<_>>();
    if ids(fast.projected_payees(rules, all)) != ids(twin.projected_payees(rules, all)) {
        return fail("xcheck projected payees");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
