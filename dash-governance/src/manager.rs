//! The in-memory v24 `CGovernanceManager` state machine.
//!
//! The manager owns held objects, postponed objects, orphan votes, the accepted-vote map, trigger
//! rate records and the [`SuperblockSet`]. It performs no I/O and receives the tip
//! list, clock, sync state and collateral index as arguments. Its object and vote transitions are
//! the harness model in `node-l1/src/gov.rs`, which was checked against Core's v24 manager.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use dash_dml::MnList;
use dash_primitives::{GovObject, GovVote};

use crate::GovParams;
use crate::exception::{ExceptionType, GovException};
use crate::object::{self, Check, FoundCollateral, Validity};
use crate::rate::{LastObjectRecord, masternode_rate_check};
use crate::strings::hash_hex;
use crate::superblocks::{AtTip, HeightOverflow, Objects, SuperblockSet};
use crate::tally::{ObjectState, OutpointKey, outpoint_key};
use crate::vote;

/// `g_txindex->FindTx` plus the active-chain confirmation information used by
/// `IsCollateralValid`.
// Core: governance/object.cpp:790-878
pub trait CollateralLookup {
    fn find_collateral(&self, txid: &[u8; 32]) -> Option<FoundCollateral<'_>>;
}

/// Core's ten-minute `GOVERNANCE_ORPHAN_EXPIRATION_TIME`.
// Core: governance/governance.cpp:36
pub const ORPHAN_EXPIRATION: i64 = 600;

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

/// In-memory v24 governance manager.
#[derive(Clone, Debug, Default)]
pub struct Manager {
    objects: BTreeMap<[u8; 32], ObjectState>,
    postponed: BTreeMap<[u8; 32], GovObject>,
    superblocks: SuperblockSet,
    accepted_votes: BTreeSet<[u8; 32]>,
    /// Core's `cmmapOrphanVotes`: the cache insert keeps only the first vote per parent because
    /// `CGovernanceVote::operator<` is false for every pair with the same parent.
    orphans: BTreeMap<[u8; 32], (GovVote, i64)>,
    last_object: BTreeMap<OutpointKey, LastObjectRecord>,
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

    /// Set the cache-loaded flag exposed through `AtTip`.
    pub fn set_loaded(&mut self, loaded: bool) {
        self.superblocks.set_loaded(loaded);
    }

    pub fn accepted_vote(&self, hash: &[u8; 32]) -> bool {
        self.accepted_votes.contains(hash)
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

    /// `ProcessObject` over a decoded network object. The tip list supplies Core's cached height.
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
        let hash = object::hash(obj);
        let outcome = |ok, held, postponed| ObjectOutcome {
            hash,
            ok,
            held,
            postponed,
        };
        if self.objects.contains_key(&hash) || self.postponed.contains_key(&hash) {
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
                now,
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
                now,
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

        self.add_object(obj, list, now, params, key)
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

    fn add_object(
        &mut self,
        obj: &GovObject,
        list: &MnList,
        now: i64,
        params: &GovParams,
        key: OutpointKey,
    ) -> Result<ObjectOutcome, HeightOverflow> {
        let hash = object::hash(obj);
        let obj_type = object::object_type(obj);
        // Core: governance/governance.cpp:306-376
        let mut state = ObjectState::new(obj_type, hash);
        state.update_sentinel(list, now, params);

        if obj_type == object::TRIGGER {
            // Core: governance/governance.cpp:348-357, 715-725
            // Record the attempt before AddTrigger.
            let record = self.last_object.entry(key).or_default();
            record.buffer.add(obj.time());
            record.status_ok = true;
            match self.superblocks.add_trigger(obj, list.height(), params) {
                Ok(true) => {}
                Ok(false) => {
                    state.mark_for_deletion(now);
                    self.objects.insert(hash, state);
                    return Ok(ObjectOutcome {
                        hash,
                        ok: true,
                        held: true,
                        postponed: false,
                    });
                }
                Err(_) => return Err(HeightOverflow),
            }
        }

        self.check_orphan_votes(hash, &mut state, list, now, params);
        self.objects.insert(hash, state);
        Ok(ObjectOutcome {
            hash,
            ok: true,
            held: true,
            postponed: false,
        })
    }

    /// `ProcessVote` over a decoded vote. `to_request` is set only for the first valid orphan for a
    /// parent. The vote hash is not inserted into `accepted_votes` when an orphan is replayed.
    // Core: governance/governance.cpp:818-877
    pub fn process_vote(
        &mut self,
        vote: &GovVote,
        list: &MnList,
        now: i64,
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
        if self.accepted_votes.contains(&hash) {
            return outcome(Ok(false), None);
        }
        let Some(state) = self.objects.get_mut(&parent) else {
            const F: &str = "CGovernanceManager::ProcessVote";
            let short = object::outpoint_short(&vote.masternode_outpoint());
            // Core: governance/governance.cpp:846-851
            if !vote::is_valid_for_unknown_parent(vote, list, now, params) {
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
            let to_request = match self.orphans.entry(parent) {
                Entry::Occupied(_) => None,
                Entry::Vacant(entry) => {
                    entry.insert((vote.clone(), now.saturating_add(ORPHAN_EXPIRATION)));
                    Some(parent)
                }
            };
            return outcome(Err(GovException::new(ExceptionType::Warning, &msg, 0)), to_request);
        };
        // Core: governance/governance.cpp:865-877
        if state.cached_delete || state.expired {
            return outcome(Ok(false), None);
        }
        let result = state.process_vote(vote, list, true, now, params).map(|_| {
            self.accepted_votes.insert(hash);
            true
        });
        outcome(result, None)
    }

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

    fn check_orphan_votes(
        &mut self,
        hash: [u8; 32],
        state: &mut ObjectState,
        list: &MnList,
        now: i64,
        params: &GovParams,
    ) {
        // Core: governance/governance.cpp:278-304. Expiry is strict (`expiration < now`).
        let Some((vote, expiry)) = self.orphans.get(&hash) else {
            return;
        };
        if *expiry < now || state.process_vote(vote, list, false, now, params).is_ok() {
            self.orphans.remove(&hash);
        }
    }

    /// Remove orphan entries whose expiry is before `now`, returning their parent hashes.
    // Core: governance/governance.cpp:1145-1159
    pub fn cleanup_orphans(&mut self, now: i64) -> Vec<[u8; 32]> {
        let expired: Vec<_> = self
            .orphans
            .iter()
            .filter_map(|(hash, (_, expiry))| (*expiry < now).then_some(*hash))
            .collect();
        for hash in &expired {
            self.orphans.remove(hash);
        }
        expired
    }

    /// Recheck postponed proposals. They stay queued while confirmations are missing; once valid,
    /// they are moved to the held map and their orphan votes are replayed. Unsynced managers leave
    /// the queue untouched, as Core's `CheckPostponedObjects` does.
    // Core: governance/governance.cpp:880-911
    pub fn check_postponed_objects<L: CollateralLookup + ?Sized>(
        &mut self,
        list: &MnList,
        now: i64,
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
            outcomes.push(self.add_object(
                &obj,
                list,
                now,
                params,
                outpoint_key(&obj.masternode_outpoint()),
            )?);
        }
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
#[path = "manager/tests.rs"]
mod fixture_tests;
