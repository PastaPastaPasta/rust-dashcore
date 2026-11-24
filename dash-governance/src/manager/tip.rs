use dash_dml::MnList;

use crate::manager::{CollateralLookup, Effects, GovClock, Manager};

impl Manager {
    /// Advance Core's cached tip height with explicit parameters and sync state, clean invalid
    /// votes, recheck postponed objects and execute the best superblock. Returns the relays and
    /// the queued masternode meta, including those of orphan votes the postponed pass replayed.
    // Core: governance/governance.cpp:1119-1143; governance/superblock.cpp:668-676
    pub fn updated_block_tip<L: CollateralLookup + ?Sized>(
        &mut self,
        height: i32,
        list: &MnList,
        clock: &GovClock,
        synced: bool,
        params: &crate::GovParams,
        lookup: &L,
    ) -> Result<Effects, crate::superblocks::HeightOverflow> {
        self.cached_height = height;
        let mut effects = if params.dip0003_enforced(height) {
            self.remove_invalid_votes(list, clock, params, synced)
        } else {
            Effects::default()
        };
        let _ = self.check_postponed_objects_at(list, clock, synced, params, lookup)?;
        self.superblocks.execute_best(list, height, &self.objects, params);
        self.generation = self.generation.wrapping_add(1);
        let relay = self.take_relay_queue();
        effects.relay_objects.extend(relay.objects);
        effects.relay_votes.extend(relay.votes);
        effects.meta.extend(self.take_meta());
        Ok(effects)
    }

    /// Return Core's cached governance tip height.
    pub fn cached_height(&self) -> i32 {
        self.cached_height
    }
}
