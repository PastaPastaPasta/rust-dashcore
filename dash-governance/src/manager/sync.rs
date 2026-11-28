//! Governance cache and synchronization queries.

use dash_dml::MnList;
use dash_primitives::{GovObject, GovVote, OutPoint, TxHash, VoteOutcome, VoteSignal};
use dash_types::Numeric;

use crate::vote::SigKey;

/// Inventory kinds understood by `confirm_inventory_request`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inventory {
    Object([u8; 32]),
    Vote([u8; 32]),
}

fn outpoint(key: super::OutpointKey) -> OutPoint {
    OutPoint::new(TxHash::from_lendian(key.0), key.1)
}

impl super::Manager {
    /// Whether an object is present in either held or postponed state.
    // Core: governance/governance.cpp:156-160
    pub fn have_object_for_hash(&self, hash: &[u8; 32]) -> bool {
        self.objects.contains_key(hash) || self.postponed.contains_key(hash)
    }

    /// Whether an object can be fetched, respecting erased hashes and syncability.
    // Core: governance/governance.cpp:163-177
    pub fn have_object_for_fetch(&self, hash: &[u8; 32]) -> bool {
        if self.erased.contains_key(hash) {
            return false;
        }
        self.objects.get(hash).is_some_and(|s| !s.cached_delete && !s.expired)
            || self.postponed.contains_key(hash)
    }

    /// Whether a held object is syncable.
    // Core: governance/governance.cpp:180-189
    pub fn have_syncable_object_for_hash(&self, hash: &[u8; 32]) -> bool {
        self.objects.get(hash).is_some_and(|s| !s.cached_delete && !s.expired)
    }

    /// Whether an indexed vote is still in its object's vote file.
    // Core: governance/governance.cpp:194-200
    pub fn have_vote(&self, hash: &[u8; 32]) -> bool {
        self.accepted_votes
            .get(hash)
            .and_then(|parent| self.objects.get(parent))
            .is_some_and(|state| state.file.has(hash))
    }

    /// The held or postponed object of `hash`, as `SerializeObjectForHash` streams it.
    // Core: governance/governance.cpp v24.0.2:183-194
    pub fn object_for_hash(&self, hash: &[u8; 32]) -> Option<&GovObject> {
        self.object_data.get(hash).or_else(|| self.postponed.get(hash))
    }

    /// Stores signature verdicts checked outside the lock in the memos of `parent`'s votes.
    // Core: governance/vote.cpp v24.0.2:164-207
    pub fn memoise_votes(
        &mut self,
        parent: &[u8; 32],
        verdicts: impl IntoIterator<Item = ([u8; 32], SigKey, GovVote, bool)>,
    ) {
        if let Some(state) = self.objects.get_mut(parent) {
            for (hash, key, vote, valid) in verdicts {
                state.file.memoise(hash, key, &vote, valid);
            }
        }
    }

    /// The indexed vote of `hash` from its object's vote file, as `SerializeVoteForHash` streams
    /// it.
    // Core: governance/governance.cpp v24.0.2:210-215; governance/votedb.cpp v24.0.2:49-57
    pub fn vote_for_hash(&self, hash: &[u8; 32]) -> Option<&GovVote> {
        let parent = self.accepted_votes.get(hash)?;
        self.objects.get(parent)?.file.votes().find(|v| crate::vote::hash(v) == *hash)
    }

    /// Return the current vote instances for an object, optionally restricted to one collateral.
    // Core: governance/governance.cpp:566-600
    pub fn get_current_votes(
        &self,
        parent: &[u8; 32],
        filter: Option<&OutPoint>,
        list: &MnList,
    ) -> Vec<GovVote> {
        let Some(state) = self.objects.get(parent) else {
            return Vec::new();
        };
        state
            .current
            .iter()
            .filter(|((txid, index), _)| {
                filter.is_none_or(|op| op == &outpoint((*txid, *index)))
                    && list.get_by_collateral(&outpoint((*txid, *index))).is_some()
            })
            .flat_map(|((txid, index), instances)| {
                instances.iter().map(move |(&signal, instance)| {
                    GovVote::new(
                        outpoint((*txid, *index)),
                        TxHash::from_lendian(*parent),
                        VoteOutcome::new(instance.outcome as u32),
                        VoteSignal::new(signal as u32),
                        instance.creation_time,
                        Vec::new(),
                    )
                })
            })
            .collect()
    }

    /// Inventory of all syncable held objects.
    // Core: governance/governance.cpp:700-713
    pub fn syncable_object_invs(&self) -> Vec<[u8; 32]> {
        self.objects
            .iter()
            .filter(|(_, s)| !s.cached_delete && !s.expired)
            .map(|(h, _)| *h)
            .collect()
    }

    /// Inventory of votes stored for one syncable object. Bloom filtering belongs to the node.
    // Core: governance/governance.cpp:664-697
    pub fn vote_invs_for_object(&self, parent: &[u8; 32]) -> Vec<[u8; 32]> {
        self.objects
            .get(parent)
            .filter(|s| !s.cached_delete && !s.expired)
            .map(|s| s.file.votes().map(crate::vote::hash).collect())
            .unwrap_or_default()
    }

    /// Return vote inventory for a syncable object.
    pub fn syncable_vote_invs(&self, parent: &[u8; 32]) -> Vec<[u8; 32]> {
        self.vote_invs_for_object(parent)
    }
    /// Return vote inventory for a syncable object through the Core query name.
    pub fn get_syncable_vote_invs(&self, parent: &[u8; 32]) -> Vec<[u8; 32]> {
        self.vote_invs_for_object(parent)
    }

    /// Return non-deleted objects split into triggers and other objects.
    // Core: governance/governance.cpp:971-993
    pub fn fetch_governance_object_votes(&self) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
        let mut triggers = Vec::new();
        let mut other = Vec::new();
        for (hash, state) in &self.objects {
            if state.cached_delete {
                continue;
            }
            if state.obj_type == crate::object::TRIGGER {
                triggers.push(*hash);
            } else {
                other.push(*hash);
            }
        }
        (triggers, other)
    }

    /// Confirm that a known governance inventory is not already held.
    // Core: governance/governance.cpp:626-660
    pub fn confirm_inventory_request(&self, inv: Inventory) -> bool {
        match inv {
            Inventory::Object(hash) => !self.have_object_for_hash(&hash),
            // Core: governance/governance.cpp:645
            Inventory::Vote(hash) => !self.accepted_votes.contains_key(&hash),
        }
    }

    /// Record an erased object using Core's proposal-forever or trigger-window rule.
    pub fn erase_object(&mut self, hash: [u8; 32], now: i64, params: &crate::GovParams) {
        let Some((obj_type, creation)) = self.object_meta.get(&hash).copied() else {
            return;
        };
        let expiry = if obj_type == crate::object::PROPOSAL {
            i64::MAX
        } else {
            i64::from(params.superblock_cycle())
                .checked_mul(params.target_spacing())
                .and_then(|cycle| cycle.checked_mul(2))
                .and_then(|cycle| creation.checked_add(cycle))
                .and_then(|v| v.checked_add(super::ORPHAN_EXPIRATION))
                .unwrap_or(i64::MAX)
        };
        let _ = now;
        if obj_type == crate::object::TRIGGER {
            // Core: governance/governance.cpp:486-500. Remove the trigger index before erasing
            // the governance object.
            self.superblocks.remove_trigger(&hash);
        }
        // Core: governance/governance.cpp:458-468
        self.objects.remove(&hash);
        self.accepted_votes.retain(|_, parent| *parent != hash);
        self.object_data.remove(&hash);
        self.object_meta.remove(&hash);
        self.erased.insert(hash, expiry);
    }

    /// Drop erased hashes strictly after their Core expiry time.
    pub fn cleanup_erased(&mut self, now: i64) {
        self.erased.retain(|_, expiry| *expiry >= now);
    }

    /// Return the expiry of an erased object hash.
    pub fn erased_expiry(&self, hash: &[u8; 32]) -> Option<i64> {
        self.erased.get(hash).copied()
    }
    /// Whether an object hash is retained in the erased-object map.
    pub fn is_erased(&self, hash: &[u8; 32]) -> bool {
        self.erased.contains_key(hash)
    }
}
