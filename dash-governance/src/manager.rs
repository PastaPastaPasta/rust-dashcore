//! The in-memory v24 `CGovernanceManager` state machine.
//!
//! The manager owns held objects, postponed objects, orphan votes, the accepted-vote map, trigger
//! rate records and the [`SuperblockSet`]. It performs no I/O and receives the tip
//! list, clock, sync state and collateral index as arguments. Its object and vote transitions are
//! the harness model in `node-l1/src/gov.rs`, which was checked against Core's v24 manager.

use std::collections::{BTreeMap, BTreeSet, btree_map};

use dash_dml::MnList;
use dash_primitives::{GovObject, GovVote};

use crate::GovParams;
use crate::exception::{ExceptionType, GovException};
use crate::object::{self, Check, FoundCollateral, Validity};
use crate::rate::{
    LastObjectRecord, MAX_TIME_FUTURE_DEVIATION, masternode_rate_check, orphan_deadline,
};
use crate::strings::hash_hex;
use crate::superblocks::{AtTip, HeightOverflow, Objects, SuperblockSet};
use crate::tally::{ObjectState, OutpointKey, outpoint_key};
use crate::vote;

pub mod cache;
mod clean;
pub mod relay;
mod signer;
mod state;
pub mod sync;
mod tip;

pub use cache::{CacheMap, CacheMultiMap, MAX_CACHE_SIZE};
pub use relay::RelayQueue;
pub use signer::LocalAdd;
pub use state::{StoreState, TriggerView};
pub use sync::Inventory;

/// The two Core time domains used by governance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GovClock {
    /// `GetTime()`; used for deletion and orphan maintenance.
    pub now: i64,
    /// `GetAdjustedTime()`; used for validity and rate checks.
    pub adjusted: i64,
}

impl GovClock {
    pub const fn new(now: i64, adjusted: i64) -> Self {
        Self {
            now,
            adjusted,
        }
    }
}

/// `g_txindex->FindTx` plus the active-chain confirmation information used by
/// `IsCollateralValid`.
// Core: governance/object.cpp:790-878
pub trait CollateralLookup {
    fn find_collateral(&self, txid: &[u8; 32]) -> Option<FoundCollateral<'_>>;
}

/// Core's ten-minute `GOVERNANCE_ORPHAN_EXPIRATION_TIME`.
// Core: governance/governance.cpp:36
pub const ORPHAN_EXPIRATION: i64 = 600;

/// Core's `RELIABLE_PROPAGATION_TIME`: the margin below the future limit that peers with a
/// lagging clock need before they accept a trigger.
// Core: governance/governance.cpp:39
const RELIABLE_PROPAGATION_TIME: i64 = 60;

/// The result of `ProcessObject`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectOutcome {
    pub hash: [u8; 32],
    /// Core's return value, also true for seen, postponed and rate-limited objects.
    pub ok: bool,
    /// Present in the held map, including objects marked for deletion.
    pub held: bool,
    pub postponed: bool,
}

/// `ProcessVote`'s answer and the parent to request for a newly stored orphan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteOutcome {
    pub hash: [u8; 32],
    /// `Ok(false)` is ignored without an exception; an error carries Core's peer penalty.
    pub result: Result<bool, GovException>,
    pub to_request: Option<[u8; 32]>,
}

/// Inventory hash emitted to the node runtime. Wire inventory typing is owned by the node layer.
pub type Inv = [u8; 32];

/// Metadata operation emitted alongside a governance mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaOp {
    RemoveObject([u8; 32]),
    AddVote([u8; 32]),
    /// `CMasternodeMetaMan::AddGovernanceVote(proTxHash, nGovernanceObjectHash)`.
    AddGovernanceVote {
        pro_tx_hash: [u8; 32],
        object: [u8; 32],
    },
}

/// Effects that the node runtime applies after releasing the governance state lock. The runtime
/// announces `relay_objects` before `relay_votes`, so an added object goes out before the orphan
/// votes it replayed, as in Core's single relay queue.
// Core: governance/governance.cpp:366, 372
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    pub relay_objects: Vec<Inv>,
    pub relay_votes: Vec<Inv>,
    /// Erased object hashes for the caller's information. Core takes no request action here.
    pub forget_requests: Vec<Inv>,
    pub sync_bumps: Vec<&'static str>,
    pub meta: Vec<MetaOp>,
    pub zmq: bool,
    pub request_parent: Option<Inv>,
}

/// In-memory v24 governance manager.
#[derive(Clone, Debug, Default)]
pub struct Manager {
    objects: BTreeMap<[u8; 32], ObjectState>,
    postponed: BTreeMap<[u8; 32], GovObject>,
    superblocks: SuperblockSet,
    /// Core's `cmapVoteToObject`: each accepted vote's hash to its parent object's hash.
    accepted_votes: BTreeMap<[u8; 32], [u8; 32]>,
    /// Core's `cmmapOrphanVotes`: the cache insert keeps only the first vote per parent because
    /// `CGovernanceVote::operator<` is false for every pair with the same parent.
    orphans: BTreeMap<[u8; 32], (GovVote, i64)>,
    orphan_cache: CacheMultiMap<[u8; 32], [u8; 32]>,
    last_object: BTreeMap<OutpointKey, LastObjectRecord>,
    /// Core's `nCachedBlockHeight`; set to the tip height by `load_cache` and advanced by
    /// `updated_block_tip`.
    cached_height: i32,
    object_meta: BTreeMap<[u8; 32], (i32, i64)>,
    erased: BTreeMap<[u8; 32], i64>,
    relay: relay::RelayQueue,
    meta: Vec<MetaOp>,
    object_data: BTreeMap<[u8; 32], GovObject>,
    last_mn_list: Option<MnList>,
    generation: u64,
    /// Core's `setAdditionalRelayObjects`; not part of the store.
    additional_relay: BTreeSet<[u8; 32]>,
}

impl Manager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn objects(&self) -> &BTreeMap<[u8; 32], ObjectState> {
        &self.objects
    }

    pub fn object(&self, hash: &[u8; 32]) -> Option<&ObjectState> {
        self.objects.get(hash)
    }

    pub fn postponed(&self) -> &BTreeMap<[u8; 32], GovObject> {
        &self.postponed
    }

    pub fn superblocks(&self) -> &SuperblockSet {
        &self.superblocks
    }

    /// The republish generation. It moves on every mutation that can change [`TriggerView`]: an
    /// added or refused object, a trigger vote, the tip work, a synced `check_and_remove` and a
    /// vote removal; never on a proposal vote, which affects no trigger.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Set the cache-loaded flag exposed through `AtTip`.
    pub fn set_loaded(&mut self, loaded: bool) {
        self.superblocks.set_loaded(loaded);
    }

    pub fn accepted_vote(&self, hash: &[u8; 32]) -> bool {
        self.accepted_votes.contains_key(hash)
    }

    /// `cmapVoteToObject.GetSize()`.
    // Core: governance/governance.cpp:202-206
    pub fn vote_index_len(&self) -> usize {
        self.accepted_votes.len()
    }

    pub fn orphan(&self, parent: &[u8; 32]) -> Option<(&GovVote, i64)> {
        self.orphans.get(parent).map(|(vote, expiry)| (vote, *expiry))
    }

    pub fn orphan_count(&self) -> usize {
        self.orphans.len()
    }

    pub fn orphan_parents(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.orphans.keys()
    }

    fn remove_orphan_parent(&mut self, parent: &[u8; 32]) {
        self.orphans.remove(parent);
        self.orphan_cache.erase(parent);
    }

    fn insert_orphan(&mut self, parent: [u8; 32], vote: &GovVote, expiry: i64) {
        if self.orphan_cache.len() == self.orphan_cache.max_size()
            && let Some((old_parent, _)) =
                self.orphan_cache.oldest().map(|(key, value)| (*key, *value))
        {
            self.remove_orphan_parent(&old_parent);
        }
        self.orphan_cache.insert(parent, vote::hash(vote));
        self.orphans.insert(parent, (vote.clone(), expiry));
    }

    #[cfg(test)]
    fn set_orphan_capacity_for_test(&mut self, max_size: usize) {
        self.orphan_cache = CacheMultiMap::new(max_size);
    }

    pub fn last_object_record(&self, key: &OutpointKey) -> Option<&LastObjectRecord> {
        self.last_object.get(key)
    }

    /// Build GV1a's adapter over this manager's superblocks and object state.
    pub fn at_tip<'a>(
        &'a mut self,
        list: &'a MnList,
        now: i64,
        params: &'a GovParams,
    ) -> AtTip<'a, BTreeMap<[u8; 32], ObjectState>> {
        AtTip {
            set: &self.superblocks,
            objects: &mut self.objects,
            list,
            now,
            params,
        }
    }

    /// Process an object with S1's per-call list-height semantics. Runtime callers use
    /// `process_object_at` after `updated_block_tip` for Core's cached-height path.
    /// An overflowing trigger height returns `HeightOverflow`, as in the harness model.
    // Core: governance/governance.cpp:228-275
    pub fn process_object<L: CollateralLookup + ?Sized>(
        &mut self,
        obj: &GovObject,
        list: &MnList,
        now: i64,
        synced: bool,
        params: &GovParams,
        lookup: &L,
    ) -> Result<ObjectOutcome, HeightOverflow> {
        self.process_object_inner(
            obj,
            list,
            synced,
            params,
            lookup,
            (GovClock::new(now, now), list.height()),
        )
    }

    /// `ProcessObject` with Core's wall and adjusted clocks supplied separately.
    // Core: governance/governance.cpp:227-275; governance/governance.cpp:750-762
    pub fn process_object_at<L: CollateralLookup + ?Sized>(
        &mut self,
        obj: &GovObject,
        list: &MnList,
        clock: &GovClock,
        synced: bool,
        params: &GovParams,
        lookup: &L,
    ) -> Result<ObjectOutcome, HeightOverflow> {
        self.process_object_inner(obj, list, synced, params, lookup, (*clock, self.cached_height))
    }

    fn process_object_inner<L: CollateralLookup + ?Sized>(
        &mut self,
        obj: &GovObject,
        list: &MnList,
        synced: bool,
        params: &GovParams,
        lookup: &L,
        context: (GovClock, i32),
    ) -> Result<ObjectOutcome, HeightOverflow> {
        let (clock, trigger_height) = context;
        let hash = object::hash(obj);
        let outcome = |ok, held, postponed| ObjectOutcome {
            hash,
            ok,
            held,
            postponed,
        };
        if self.objects.contains_key(&hash)
            || self.postponed.contains_key(&hash)
            || self.erased.contains_key(&hash)
        {
            return Ok(outcome(
                true,
                self.objects.contains_key(&hash),
                self.postponed.contains_key(&hash),
            ));
        }

        let obj_type = object::object_type(obj);
        // Core: governance/governance.cpp:750-762
        let gated = synced && obj_type == object::TRIGGER;
        let key = outpoint_key(&obj.masternode_outpoint());
        let mut bypassed = false;
        if gated {
            let rc = masternode_rate_check(
                obj.time(),
                clock.adjusted,
                self.last_object.get_mut(&key),
                false,
                true,
                params,
            );
            if !rc.ok {
                return Ok(outcome(true, false, false));
            }
            bypassed = rc.bypassed;
        }

        let found = lookup.find_collateral(&object::txid_bytes(&obj.collateral_hash()));
        let check = Check::Collateral {
            list,
            found,
        };
        let Validity {
            valid,
            missing_confirmations,
            ..
        } = object::is_valid_locally(obj, false, check, params);
        if bypassed
            && valid
            && !masternode_rate_check(
                obj.time(),
                clock.adjusted,
                self.last_object.get_mut(&key),
                true,
                true,
                params,
            )
            .ok
        {
            return Ok(outcome(true, false, false));
        }
        if !valid {
            if missing_confirmations {
                self.postponed.insert(hash, obj.clone());
                return Ok(outcome(true, false, true));
            }
            return Ok(outcome(false, false, false));
        }

        let (result, added) =
            self.add_object_at(obj, list, &clock, synced, params, trigger_height)?;
        if synced && added && self.objects.get(&hash).is_some_and(|state| !state.cached_delete) {
            self.relay_object(hash);
        }
        Ok(result)
    }

    /// Decode and process one network `govobj` message. Bytes after the object in the
    /// stream are ignored, as in the harness model.
    pub fn process_object_message<L: CollateralLookup + ?Sized>(
        &mut self,
        message: &[u8],
        list: &MnList,
        now: i64,
        synced: bool,
        params: &GovParams,
        lookup: &L,
    ) -> Result<Option<ObjectOutcome>, HeightOverflow> {
        let Some(obj) = object::decode_network(&mut &message[..]) else {
            return Ok(None);
        };
        self.process_object(&obj, list, now, synced, params, lookup).map(Some)
    }

    fn add_object_at(
        &mut self,
        obj: &GovObject,
        list: &MnList,
        clock: &GovClock,
        synced: bool,
        params: &GovParams,
        trigger_height: i32,
    ) -> Result<(ObjectOutcome, bool), HeightOverflow> {
        let hash = object::hash(obj);
        // Core: governance/governance.cpp:336-341
        if self.objects.contains_key(&hash) {
            let outcome = ObjectOutcome {
                hash,
                ok: true,
                held: true,
                postponed: false,
            };
            return Ok((outcome, false));
        }
        let obj_type = object::object_type(obj);
        // Core: governance/governance.cpp:306-376
        let mut state = ObjectState::new(obj_type, hash);
        state.update_sentinel(list, clock.now, params);

        if obj_type == object::TRIGGER {
            // Core: governance/governance.cpp:348-357, 715-725
            // Record the attempt before AddTrigger.
            let key = outpoint_key(&obj.masternode_outpoint());
            let record = self.last_object.entry(key).or_default();
            record.buffer.add(obj.time());
            record.status_ok = true;
            match self.superblocks.add_trigger(obj, trigger_height, params) {
                Ok(true) => self.schedule_trigger_relay(hash, obj.time(), clock.now),
                Ok(false) => {
                    state.mark_for_deletion(clock.now);
                    self.objects.insert(hash, state);
                    self.object_meta.insert(hash, (obj_type, obj.time()));
                    self.object_data.insert(hash, obj.clone());
                    self.generation = self.generation.wrapping_add(1);
                    let outcome = ObjectOutcome {
                        hash,
                        ok: true,
                        held: true,
                        postponed: false,
                    };
                    return Ok((outcome, false));
                }
                Err(_) => return Err(HeightOverflow),
            }
        }

        self.check_orphan_votes_at(hash, &mut state, list, clock, synced, params);
        self.objects.insert(hash, state);
        self.object_meta.insert(hash, (obj_type, obj.time()));
        self.object_data.insert(hash, obj.clone());
        self.generation = self.generation.wrapping_add(1);
        let outcome = ObjectOutcome {
            hash,
            ok: true,
            held: true,
            postponed: false,
        };
        Ok((outcome, true))
    }

    /// Queue a kept trigger created too close to the future limit for a later re-announcement.
    // Core: governance/governance.cpp:728-741
    fn schedule_trigger_relay(&mut self, hash: [u8; 32], creation_time: i64, now: i64) {
        let limit =
            now.saturating_add(MAX_TIME_FUTURE_DEVIATION).saturating_sub(RELIABLE_PROPAGATION_TIME);
        if creation_time > limit {
            self.additional_relay.insert(hash);
        }
    }

    /// Re-announce scheduled triggers once they have aged past the propagation margin. Unknown
    /// and out-of-window objects are dropped without relay; ones still too new stay queued.
    // Core: governance/governance.cpp:913-944
    fn relay_additional_objects(&mut self, adjusted: i64, params: &GovParams) {
        let cycle = i64::from(params.superblock_cycle()).saturating_mul(params.target_spacing());
        let future = adjusted.saturating_add(MAX_TIME_FUTURE_DEVIATION);
        let past = adjusted.saturating_sub(cycle.saturating_mul(2));
        let ready = future.saturating_sub(RELIABLE_PROPAGATION_TIME);
        for hash in std::mem::take(&mut self.additional_relay) {
            let Some(timestamp) = self.object_data.get(&hash).map(GovObject::time) else {
                continue;
            };
            if !(past..=future).contains(&timestamp) {
                continue;
            }
            if timestamp <= ready {
                self.relay_object(hash);
            } else {
                self.additional_relay.insert(hash);
            }
        }
    }

    /// `ProcessVote` over a decoded vote. `to_request` is set only for the first valid orphan for a
    /// parent. A successfully replayed orphan is indexed, and queued for relay only when it was
    /// not indexed before and the manager is synced. Every accepted vote queues its masternode
    /// meta for `take_meta`.
    // Core: governance/governance.cpp:818-877
    pub fn process_vote(
        &mut self,
        vote: &GovVote,
        list: &MnList,
        now: i64,
        params: &GovParams,
    ) -> VoteOutcome {
        self.process_vote_at(vote, list, &GovClock::new(now, now), params)
    }

    /// `ProcessVote` with separate wall and adjusted clocks. An accepted vote queues
    /// `MetaOp::AddGovernanceVote` for `take_meta`; one on a trigger also moves the republish
    /// generation.
    // Core: governance/governance.cpp:802-877; governance/vote.cpp:211-250
    pub fn process_vote_at(
        &mut self,
        vote: &GovVote,
        list: &MnList,
        clock: &GovClock,
        params: &GovParams,
    ) -> VoteOutcome {
        let hash = vote::hash(vote);
        let parent = object::txid_bytes(&vote.parent_hash());
        let outcome = |result, to_request| VoteOutcome {
            hash,
            result,
            to_request,
        };
        // Core: governance/governance.cpp:838-841
        if self.accepted_votes.contains_key(&hash) {
            return outcome(Ok(false), None);
        }
        let Some(state) = self.objects.get_mut(&parent) else {
            const F: &str = "CGovernanceManager::ProcessVote";
            let short = object::outpoint_short(&vote.masternode_outpoint());
            // Core: governance/governance.cpp:846-851
            if !vote::is_valid_for_unknown_parent(vote, list, clock.adjusted, params) {
                let msg = format!(
                    "{F} -- Invalid vote for unknown parent object {}, MN outpoint = {short}, vote hash = {}",
                    hash_hex(&parent),
                    hash_hex(&hash)
                );
                return outcome(
                    Err(GovException::new(ExceptionType::PermanentError, &msg, 20)),
                    None,
                );
            }
            let msg = format!(
                "{F} -- Unknown parent object {}, MN outpoint = {short}",
                hash_hex(&parent)
            );
            // Core: governance/governance.cpp:847-862; governance/vote.cpp:263-291
            // CacheMultiMap's insert refuses every vote after the first for one parent.
            let to_request = if self.orphans.contains_key(&parent) {
                None
            } else {
                self.insert_orphan(parent, vote, orphan_deadline(clock.now));
                Some(parent)
            };
            return outcome(Err(GovException::new(ExceptionType::Warning, &msg, 0)), to_request);
        };
        // Core: governance/governance.cpp:865-877
        if state.cached_delete || state.expired {
            return outcome(Ok(false), None);
        }
        let trigger = state.obj_type == object::TRIGGER;
        let result = state.process_vote(vote, list, true, clock.adjusted, params).map(|pro_tx| {
            self.accepted_votes.insert(hash, parent);
            // Core: governance/object.cpp:470
            self.meta.push(MetaOp::AddGovernanceVote {
                pro_tx_hash: *pro_tx.as_bytes(),
                object: parent,
            });
            // A proposal's votes affect no trigger, so the published view stays.
            if trigger {
                self.generation = self.generation.wrapping_add(1);
            }
            true
        });
        outcome(result, None)
    }

    /// Decode and process a vote using the legacy single-clock wrapper.
    pub fn process_vote_message(
        &mut self,
        message: &[u8],
        list: &MnList,
        now: i64,
        params: &GovParams,
    ) -> Option<VoteOutcome> {
        let vote = vote::decode_network(message)?;
        Some(self.process_vote(&vote, list, now, params))
    }

    /// Decode and process a vote with separate wall and adjusted clocks.
    pub fn process_vote_message_at(
        &mut self,
        message: &[u8],
        list: &MnList,
        clock: &GovClock,
        params: &GovParams,
    ) -> Option<VoteOutcome> {
        let vote = vote::decode_network(message)?;
        Some(self.process_vote_at(&vote, list, clock, params))
    }

    fn check_orphan_votes_at(
        &mut self,
        hash: [u8; 32],
        state: &mut ObjectState,
        list: &MnList,
        clock: &GovClock,
        synced: bool,
        params: &GovParams,
    ) {
        // Core: governance/governance.cpp v24.0.2:280-309. Expiry is strict (`expiration < now`).
        let Some((vote, expiry)) = self.orphans.get(&hash).cloned() else {
            return;
        };
        if expiry < clock.adjusted {
            self.remove_orphan_parent(&hash);
        } else if let Ok(pro_tx) = state.process_vote(&vote, list, false, clock.adjusted, params) {
            // Core: governance/object.cpp:470
            self.meta.push(MetaOp::AddGovernanceVote {
                pro_tx_hash: *pro_tx.as_bytes(),
                object: hash,
            });
            // Core: governance/governance.cpp v24.0.2:299-301 (dashpay/dash#8112); RelayVote
            // :126-142
            let vote_hash = vote::hash(&vote);
            if let btree_map::Entry::Vacant(entry) = self.accepted_votes.entry(vote_hash) {
                entry.insert(hash);
                if synced && list.get_by_collateral(&vote.masternode_outpoint()).is_some() {
                    self.relay_vote(vote_hash);
                }
            }
            self.remove_orphan_parent(&hash);
        }
    }

    /// Remove orphan entries whose expiry is before `now`, returning their parent hashes.
    // Core: governance/governance.cpp:1145-1159
    pub fn cleanup_orphans(&mut self, now: i64) -> Vec<[u8; 32]> {
        self.cleanup_orphans_at(&GovClock::new(now, now))
    }

    /// Periodic orphan cleanup uses wall time (`GetTime`), independently of adjusted time.
    // Core: governance/governance.cpp:1145-1159
    pub fn cleanup_orphans_at(&mut self, clock: &GovClock) -> Vec<[u8; 32]> {
        let expired: Vec<_> = self
            .orphans
            .iter()
            .filter_map(|(hash, (_, expiry))| (*expiry < clock.now).then_some(*hash))
            .collect();
        for hash in &expired {
            self.remove_orphan_parent(hash);
        }
        expired
    }

    /// Recheck postponed proposals while `IsSynced()` is true. They stay queued while confirmations
    /// are missing; once valid, they are moved to the held map and their orphan votes are replayed.
    // Core: governance/governance.cpp:880-911
    pub fn check_postponed_objects<L: CollateralLookup + ?Sized>(
        &mut self,
        list: &MnList,
        now: i64,
        synced: bool,
        params: &GovParams,
        lookup: &L,
    ) -> Result<Vec<ObjectOutcome>, HeightOverflow> {
        self.check_postponed_objects_at(list, &GovClock::new(now, now), synced, params, lookup)
    }

    /// Recheck postponed proposals with separate clock domains, then run the additional-relay
    /// pass on `clock.adjusted`. Both return early unless `synced` (Core's `IsSynced()`).
    // Core: governance/governance.cpp:880-944
    pub fn check_postponed_objects_at<L: CollateralLookup + ?Sized>(
        &mut self,
        list: &MnList,
        clock: &GovClock,
        synced: bool,
        params: &GovParams,
        lookup: &L,
    ) -> Result<Vec<ObjectOutcome>, HeightOverflow> {
        if !synced {
            return Ok(Vec::new());
        }
        let hashes: Vec<_> = self.postponed.keys().copied().collect();
        let mut outcomes = Vec::new();
        for hash in hashes {
            let Some(obj) = self.postponed.get(&hash).cloned() else {
                continue;
            };
            let found = lookup.find_collateral(&object::txid_bytes(&obj.collateral_hash()));
            let validity = object::collateral_valid(&obj, found);
            if !validity.valid && validity.missing_confirmations {
                continue;
            }
            self.postponed.remove(&hash);
            if !validity.valid
                || !object::is_valid_locally(&obj, false, Check::NoCollateral, params).valid
            {
                outcomes.push(ObjectOutcome {
                    hash,
                    ok: false,
                    held: false,
                    postponed: false,
                });
                continue;
            }
            // Core: governance/governance.cpp:885
            let (outcome, added) =
                self.add_object_at(&obj, list, clock, true, params, self.cached_height)?;
            if added && self.objects.get(&hash).is_some_and(|state| !state.cached_delete) {
                self.relay_object(hash);
            }
            outcomes.push(outcome);
        }
        self.relay_additional_objects(clock.adjusted, params);
        Ok(outcomes)
    }
}

impl Objects for Manager {
    fn state(&self, hash: &[u8; 32]) -> Option<&ObjectState> {
        self.objects.get(hash)
    }

    fn state_mut(&mut self, hash: &[u8; 32]) -> Option<&mut ObjectState> {
        self.objects.get_mut(hash)
    }
}

#[cfg(test)]
mod tests;
