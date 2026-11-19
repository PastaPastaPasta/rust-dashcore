use std::collections::BTreeMap;

use dash_dml::MnList;
use dash_primitives::{GovObject, GovVote};

use crate::GovParams;
use crate::manager::Manager;
use crate::rate::LastObjectRecord;
use crate::superblocks::SuperblockSet;
use crate::tally::{ObjectState, OutpointKey};

/// Serializable governance fields in Core's store order.
// Core: governance/governance.h:202-210
#[derive(Clone, Debug, Default)]
pub struct StoreState {
    pub erased: BTreeMap<[u8; 32], i64>,
    pub orphans: Vec<([u8; 32], (GovVote, i64))>,
    pub objects: BTreeMap<[u8; 32], (GovObject, ObjectState)>,
    pub last_object: BTreeMap<OutpointKey, LastObjectRecord>,
    pub last_mn_list: Option<MnList>,
}

/// Immutable trigger read surface published to chainstate readers. `objects` holds the states of
/// the triggers in `superblocks` only.
#[derive(Clone, Debug)]
pub struct TriggerView {
    pub is_loaded: bool,
    pub cached_height: i32,
    pub superblocks: SuperblockSet,
    pub objects: BTreeMap<[u8; 32], ObjectState>,
    pub generation: u64,
}

impl Manager {
    /// Export the persisted governance fields in Core's store order.
    pub fn export_state(&self) -> StoreState {
        StoreState {
            erased: self.erased.clone(),
            orphans: self
                .orphan_cache
                .items()
                .filter_map(|(parent, vote_hash)| {
                    self.orphans.get(parent).and_then(|entry| {
                        (crate::vote::hash(&entry.0) == *vote_hash)
                            .then(|| (*parent, entry.clone()))
                    })
                })
                .collect(),
            objects: self
                .objects
                .iter()
                .filter_map(|(hash, state)| {
                    self.object_data.get(hash).cloned().map(|obj| (*hash, (obj, state.clone())))
                })
                .collect(),
            last_object: self.last_object.clone(),
            last_mn_list: self.last_mn_list.clone(),
        }
    }

    /// Import persisted governance fields; indexes are rebuilt by `init_on_load`.
    pub fn import_state(&mut self, state: StoreState) {
        self.erased = state.erased;
        self.orphans.clear();
        let max_size = self.orphan_cache.max_size();
        self.orphan_cache = crate::manager::CacheMultiMap::new(max_size);
        // Core's `listItems` is newest first; inserting oldest first restores its order.
        for (parent, (vote, expiry)) in state.orphans.into_iter().rev() {
            if !self.orphans.contains_key(&parent) {
                self.insert_orphan(parent, &vote, expiry);
            }
        }
        self.objects =
            state.objects.iter().map(|(hash, (_, state))| (*hash, state.clone())).collect();
        self.object_data = state.objects.into_iter().map(|(hash, (obj, _))| (hash, obj)).collect();
        self.object_meta = self
            .object_data
            .iter()
            .filter_map(|(hash, obj)| {
                self.objects.get(hash).map(|state| (*hash, (state.obj_type, obj.time())))
            })
            .collect();
        self.last_object = state.last_object;
        self.last_mn_list = state.last_mn_list;
        self.accepted_votes.clear();
        self.postponed.clear();
        self.additional_relay.clear();
        self.meta.clear();
        self.superblocks.clear();
        self.cached_height = 0;
        self.generation = 0;
    }

    /// Rebuild vote indexes and trigger entries after loading Core's store fields.
    // Core: governance/governance.cpp:995-1023
    pub fn init_on_load(&mut self, clock: &crate::manager::GovClock, params: &GovParams) {
        self.cached_height = 0;
        self.accepted_votes.clear();
        for state in self.objects.values() {
            self.accepted_votes.extend(state.file.votes().map(crate::vote::hash));
        }
        self.superblocks.clear();
        self.superblocks.set_loaded(true);
        let triggers: Vec<_> = self
            .objects
            .iter()
            .filter(|(_, state)| state.obj_type == crate::object::TRIGGER)
            .map(|(hash, _state)| *hash)
            .collect();
        for hash in triggers {
            let Some(obj) = self.object_data.get(&hash) else {
                continue;
            };
            if !self.superblocks.add_trigger(obj, self.cached_height, params).unwrap_or(false)
                && let Some(state) = self.objects.get_mut(&hash)
            {
                state.mark_for_deletion(clock.now);
            }
        }
    }

    /// Snapshot the trigger read surface: the set and the states of the triggers in it.
    pub fn trigger_view(&self) -> TriggerView {
        TriggerView {
            is_loaded: self.superblocks.is_loaded(),
            cached_height: self.cached_height,
            superblocks: self.superblocks.clone(),
            objects: self
                .superblocks
                .iter()
                .filter_map(|(hash, _)| self.objects.get(hash).map(|s| (*hash, s.clone())))
                .collect(),
            generation: self.generation,
        }
    }
}
