//! `governance::SuperblockManager`: the held triggers, the best superblock at a height, its
//! payments, and the [`GovernanceView`] EV3's block checks read (notes D3, D11).
//!
//! Core's trigger entries share the governance object with the manager's map; here the object
//! states stay with their owner (GV1b's store) and are looked up by hash through [`Objects`].

use std::collections::BTreeMap;
use std::convert::Infallible;

use dash_dml::MnList;
use dash_evo::payments::GovernanceView;
use dash_primitives::{GovObject, TxOut};

use crate::GovParams;
use crate::object::TRIGGER;
use crate::tally::ObjectState;
use crate::trigger::{Status, Superblock, is_valid_block_height};
use crate::vote::SIGNAL_FUNDING;

/// The object states a trigger's entry points at, by object hash (internal order).
pub trait Objects {
    fn state(&self, hash: &[u8; 32]) -> Option<&ObjectState>;
    fn state_mut(&mut self, hash: &[u8; 32]) -> Option<&mut ObjectState>;
}

impl Objects for BTreeMap<[u8; 32], ObjectState> {
    fn state(&self, hash: &[u8; 32]) -> Option<&ObjectState> {
        self.get(hash)
    }
    fn state_mut(&mut self, hash: &[u8; 32]) -> Option<&mut ObjectState> {
        self.get_mut(hash)
    }
}

/// `AddTrigger`'s refusal: the event height plus the Valid horizon overflows `int` in
/// `IsExpired` (undefined behaviour in Core; the oracle refuses the same, notes D13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeightOverflow;

/// The held triggers by object hash, in Core's `std::map<uint256>` order (`memcmp` of the
/// internal bytes).
#[derive(Clone, Debug, Default)]
pub struct SuperblockSet {
    triggers: BTreeMap<[u8; 32], Superblock>,
    loaded: bool,
}

impl SuperblockSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// `IsValid()`: the cache is loaded.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// `SetLoaded`.
    pub fn set_loaded(&mut self, loaded: bool) {
        self.loaded = loaded;
    }

    pub fn get(&self, hash: &[u8; 32]) -> Option<&Superblock> {
        self.triggers.get(hash)
    }

    /// The triggers in map order (`GetActiveTriggers` without the objects check).
    pub fn iter(&self) -> impl Iterator<Item = (&[u8; 32], &Superblock)> {
        self.triggers.iter()
    }

    /// `AddTrigger(obj, cachedHeight)`: holds the trigger `obj` describes, by its hash, with the Valid status;
    /// whether it is live (`!IsExpired(cachedHeight)`). `Ok(false)` for a held hash or data that
    /// does not parse.
    // Core: governance/superblock.cpp:436-466
    pub fn add_trigger(
        &mut self,
        obj: &GovObject,
        cached_height: i32,
        p: &GovParams,
    ) -> Result<bool, HeightOverflow> {
        // `obj->GetHash()`, never a caller's key.
        let hash = crate::object::hash(obj);
        if self.triggers.contains_key(&hash) {
            return Ok(false);
        }
        let Ok(mut sb) = Superblock::parse(obj, p) else {
            return Ok(false);
        };
        sb.status = Status::Valid;
        let expired = sb.is_expired(cached_height, p).ok_or(HeightOverflow)?;
        self.triggers.insert(hash, sb);
        Ok(!expired)
    }

    /// `RemoveTrigger`.
    pub fn remove_trigger(&mut self, hash: &[u8; 32]) {
        self.triggers.remove(hash);
    }

    /// `Clean(cachedHeight)`: drops invalid and expired triggers, marking their objects expired
    /// (when expired) and for deletion at `GetTime()` = `now`; the hashes dropped.
    // Core: governance/superblock.cpp:474-530
    pub fn clean(
        &mut self,
        cached_height: i32,
        objects: &mut impl Objects,
        now: i64,
        p: &GovParams,
    ) -> Vec<[u8; 32]> {
        let mut removed = Vec::new();
        for (hash, sb) in &mut self.triggers {
            let state = objects.state_mut(hash);
            if state.as_ref().is_none_or(|s| s.obj_type != TRIGGER) {
                sb.status = Status::ErrorInvalid;
            }
            let remove = match sb.status {
                Status::ErrorInvalid | Status::Unknown => true,
                // `AddTrigger` refused the heights that overflow.
                Status::Valid | Status::Executed => sb.is_expired(cached_height, p).unwrap_or(true),
            };
            if remove {
                if let Some(state) = state {
                    if matches!(sb.status, Status::Valid | Status::Executed) {
                        state.expired = true;
                    }
                    state.mark_for_deletion(now);
                }
                removed.push(*hash);
            }
        }
        for hash in &removed {
            self.triggers.remove(hash);
        }
        removed
    }

    /// `Clear()`.
    pub fn clear(&mut self) {
        self.triggers.clear();
        self.loaded = false;
    }

    /// `GetBestSuperblock(tip_mn_list, height)`: the first trigger for `height`, in map order,
    /// with a strictly larger absolute funding yes count; none unless that count is above 0.
    // Core: governance/superblock.cpp:556-576
    pub fn best(
        &self,
        list: &MnList,
        height: i32,
        objects: &impl Objects,
        p: &GovParams,
    ) -> Option<[u8; 32]> {
        if !is_valid_block_height(height, p) {
            return None;
        }
        let mut yes = 0;
        let mut best = None;
        for (hash, sb) in &self.triggers {
            let Some(state) = objects.state(hash) else {
                continue;
            };
            if sb.height != height {
                continue;
            }
            let count = state.absolute_yes(list, SIGNAL_FUNDING);
            if count > yes {
                yes = count;
                best = Some(*hash);
            }
        }
        best.filter(|_| yes > 0)
    }

    /// `IsSuperblockTriggered(tip_mn_list, height)`: updates the sentinel flags of the triggers
    /// for `height`, in map order, until one is funded.
    // Core: governance/superblock.cpp:578-620
    pub fn is_triggered(
        &self,
        list: &MnList,
        height: i32,
        objects: &mut impl Objects,
        now: i64,
        p: &GovParams,
    ) -> bool {
        if !is_valid_block_height(height, p) {
            return false;
        }
        for (hash, sb) in &self.triggers {
            if sb.height != height {
                continue;
            }
            let Some(state) = objects.state_mut(hash) else {
                continue;
            };
            state.update_sentinel(list, now, p);
            if state.cached_funding {
                return true;
            }
        }
        false
    }

    /// `GetSuperblockPayments(tip_mn_list, height)`: the best trigger's payments.
    // Core: governance/superblock.cpp:633-666
    pub fn payments(
        &self,
        list: &MnList,
        height: i32,
        objects: &impl Objects,
        p: &GovParams,
    ) -> Option<Vec<TxOut>> {
        let best = self.best(list, height, objects, p)?;
        self.triggers.get(&best).map(Superblock::tx_outs)
    }

    /// `ExecuteBestSuperblock(tip_mn_list, height)`.
    // Core: governance/superblock.cpp:668-675
    pub fn execute_best(
        &mut self,
        list: &MnList,
        height: i32,
        objects: &impl Objects,
        p: &GovParams,
    ) {
        if let Some(sb) =
            self.best(list, height, objects, p).and_then(|best| self.triggers.get_mut(&best))
        {
            sb.status = Status::Executed;
        }
    }
}

/// EV3's [`GovernanceView`] over a [`SuperblockSet`] and its objects at the tip (`CChainstateHelper`'s
/// superblock back end): `GetTime()` = `now` for the deletion marks `IsSuperblockTriggered` can
/// make.
pub struct AtTip<'a, O: Objects> {
    pub set: &'a SuperblockSet,
    pub objects: &'a mut O,
    pub list: &'a MnList,
    pub now: i64,
    pub params: &'a GovParams,
}

impl<O: Objects> GovernanceView for AtTip<'_, O> {
    type Error = Infallible;

    fn gov_valid(&mut self) -> Result<bool, Infallible> {
        Ok(self.set.is_loaded())
    }

    fn sb_triggered(&mut self, height: u32) -> Result<bool, Infallible> {
        let Ok(height) = i32::try_from(height) else {
            return Ok(false);
        };
        Ok(self.set.is_triggered(self.list, height, self.objects, self.now, self.params))
    }

    fn sb_payments(&mut self, height: u32) -> Result<Option<Vec<TxOut>>, Infallible> {
        let Ok(height) = i32::try_from(height) else {
            return Ok(None);
        };
        Ok(self.set.payments(self.list, height, &*self.objects, self.params))
    }
}
