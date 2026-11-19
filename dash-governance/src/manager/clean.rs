use dash_dml::MnList;

use crate::manager::{Effects, GovClock, Manager, MetaOp};
use crate::object::PROPOSAL;
use crate::proposal::validate_proposal;
use crate::strings::hex_str;

const DELETION_DELAY: i64 = 600;

impl Manager {
    /// Refresh sentinels and erase expired governance objects while `IsBlockchainSynced()` is true.
    // Core: governance/governance.cpp:400-513; governance/superblock.cpp:473-527
    pub fn check_and_remove(
        &mut self,
        list: &MnList,
        clock: &GovClock,
        synced: bool,
        params: &crate::GovParams,
    ) -> Effects {
        if !synced {
            return Effects::default();
        }
        // Core's dirty governance-object hash list is never populated in v24.0.0, so its
        // masternode-vote clear pass does not run.
        for (hash, state) in &mut self.objects {
            if state.dirty {
                if let Some(obj) = self.object_data.get(hash) {
                    state.update_local_validity(obj, params);
                }
                state.update_sentinel(list, clock.now, params);
            }
        }

        // Core: governance/superblock.cpp:473-527 drops triggers from the set at once.
        self.superblocks.clean(self.cached_height, &mut self.objects, clock.now, params);

        for (hash, state) in &mut self.objects {
            if state.obj_type != PROPOSAL || state.cached_delete || state.expired {
                continue;
            }
            let Some(obj) = self.object_data.get(hash) else {
                continue;
            };
            if validate_proposal(hex_str(obj.data()).as_bytes(), true, true, clock.adjusted, params)
                .is_err()
            {
                state.mark_for_deletion(clock.now);
            }
        }

        let erased: Vec<_> = self
            .objects
            .iter()
            .filter_map(|(hash, state)| {
                let due = (state.cached_delete || state.expired)
                    && state
                        .deletion_time
                        .checked_add(DELETION_DELAY)
                        .is_some_and(|t| t <= clock.now);
                due.then_some(*hash)
            })
            .collect();
        for hash in &erased {
            self.erase_object(*hash, clock.now, params);
        }
        self.cleanup_erased(clock.now);
        // The sentinel updates, the set's clean and the deletion marks all reach the trigger
        // view; one move covers whichever of them changed it.
        self.generation = self.generation.wrapping_add(1);
        let relay = self.take_relay_queue();
        let mut meta = self.take_meta();
        meta.extend(erased.iter().copied().map(MetaOp::RemoveObject));
        Effects {
            relay_objects: relay.objects,
            relay_votes: relay.votes,
            forget_requests: erased.clone(),
            meta,
            ..Effects::default()
        }
    }

    /// Remove invalid votes while `IsSynced()` is true, using explicit clock and parameters.
    pub fn remove_invalid_votes(
        &mut self,
        list: &MnList,
        clock: &GovClock,
        params: &crate::GovParams,
        synced: bool,
    ) -> Effects {
        if !synced {
            return Effects::default();
        }
        let Some(old) = self.last_mn_list.as_ref() else {
            self.last_mn_list = Some(list.clone());
            return Effects::default();
        };
        let mut updated = Vec::new();
        let mut removed = Vec::new();
        for dmn in old.iter() {
            match list.get_by_collateral(&dmn.collateral) {
                None => removed.push(dmn.collateral),
                Some(new)
                    if new.cold.key_id_voting != dmn.cold.key_id_voting
                        || new.cold.pub_key_operator != dmn.cold.pub_key_operator =>
                {
                    updated.push(dmn.collateral)
                }
                Some(_) => {}
            }
        }
        let mut removed_hashes = Vec::new();
        updated.extend(removed);
        for op in updated {
            for state in self.objects.values_mut() {
                removed_hashes.extend(state.remove_invalid_votes(
                    list,
                    &op,
                    clock.adjusted,
                    params,
                ));
            }
        }
        self.last_mn_list = Some(list.clone());
        for hash in &removed_hashes {
            self.accepted_votes.remove(hash);
            // Core erases by vote hash from the parent-keyed orphan cache.
            self.remove_orphan_parent(hash);
        }
        if !removed_hashes.is_empty() {
            self.generation = self.generation.wrapping_add(1);
        }
        Effects {
            forget_requests: removed_hashes,
            ..Effects::default()
        }
    }

    /// Return orphan parents that still need object requests; request caps and shuffling are caller work.
    pub fn orphan_parent_candidates(&mut self, now: i64) -> Vec<[u8; 32]> {
        self.cleanup_orphans(now);
        self.orphans.keys().filter(|hash| !self.objects.contains_key(*hash)).copied().collect()
    }
}
