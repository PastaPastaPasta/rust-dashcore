//! The manager operations `GovernanceSigner` calls: the approved proposals, the data-hash lookup,
//! the local object add and the local vote (GS1 D5).

use dash_dml::MnList;
use dash_primitives::{GovObject, GovVote};

use crate::GovParams;
use crate::manager::{CollateralLookup, Effects, GovClock, Manager, VoteOutcome};
use crate::object::{self, Check, PROPOSAL};
use crate::signer::{ApprovedProposal, arith_cmp};
use crate::superblocks::HeightOverflow;
use crate::vote::SIGNAL_FUNDING;

/// What `AddGovernanceObject(govobj, "<local>")` did with the object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalAdd {
    /// `IsValidLocally` failed with this `strError`; nothing changed.
    Invalid(String),
    /// Already held; nothing changed.
    Duplicate,
    /// `AddTrigger` refused it: held, marked for deletion and not announced.
    Refused,
    /// Held and announced.
    Added,
}

impl Manager {
    /// `GetApprovedProposals(tip_mn_list)`: the proposals whose absolute FUNDING yes weight is at
    /// least `max(nGovernanceMinQuorum, m_valid_weighted / 10)`, by that weight descending and
    /// then by hash descending in `UintToArith256` order. Deleted and expired proposals count.
    // Core: governance/governance.cpp:1228-1259
    pub fn approved_proposals(&self, list: &MnList, p: &GovParams) -> Vec<ApprovedProposal<'_>> {
        let weighted = i32::try_from(list.counts().valid_weighted).unwrap_or(i32::MAX);
        let required = p.min_quorum().max(weighted / 10);
        let mut approved: Vec<_> = self
            .objects
            .iter()
            .filter(|(_, state)| state.obj_type == PROPOSAL)
            .map(|(hash, state)| (state.absolute_yes(list, SIGNAL_FUNDING), hash))
            .filter(|(yes, _)| *yes >= required)
            .filter_map(|(yes, hash)| self.object_data.get(hash).map(|obj| (yes, *hash, obj)))
            .collect();
        approved.sort_by(|(l_yes, l_hash, _), (r_yes, r_hash, _)| {
            r_yes.cmp(l_yes).then_with(|| arith_cmp(r_hash, l_hash))
        });
        approved
            .into_iter()
            .map(|(_, hash, obj)| ApprovedProposal {
                hash,
                data: obj.data(),
            })
            .collect()
    }

    /// `FindGovernanceObjectByDataHash`: the first held object in hash (memcmp) order whose data
    /// hash matches, including objects marked for deletion or expired.
    // Core: governance/governance.cpp:554-562
    pub fn find_by_data_hash(&self, data_hash: &[u8; 32]) -> Option<&GovObject> {
        self.objects
            .keys()
            .filter_map(|hash| self.object_data.get(hash))
            .find(|obj| object::data_hash(obj) == *data_hash)
    }

    /// `AddGovernanceObject(govobj, "<local>")` at the cached height. Relays only when `synced`,
    /// as `RelayObject` and `RelayVote` do; the replayed orphan vote also needs its masternode in
    /// `list`, which its acceptance already proved. Only a proposal reads `lookup`, and a local
    /// proposal with missing confirmations is `Invalid`, never postponed.
    ///
    /// `obj`'s data must parse and its type must match the JSON `"type"`, as a locally built
    /// object's `LoadData` guarantees before `IsValidLocally` runs; the data is not reparsed here.
    ///
    /// A trigger's rate record is updated before `AddTrigger` runs, so `Err(HeightOverflow)`
    /// leaves that record changed and nothing else. A signer-built trigger's height comes from
    /// `GetNearestSuperblocksHeights`, so it never overflows.
    // Core: governance/governance.cpp:307-383
    pub fn add_local_object_at<L: CollateralLookup + ?Sized>(
        &mut self,
        obj: &GovObject,
        list: &MnList,
        clock: &GovClock,
        synced: bool,
        params: &GovParams,
        lookup: &L,
    ) -> Result<(LocalAdd, Effects), HeightOverflow> {
        // Core: governance/object.cpp:740, 746-765
        let found = if object::object_type(obj) == PROPOSAL {
            lookup.find_collateral(&object::txid_bytes(&obj.collateral_hash()))
        } else {
            None
        };
        let check = Check::Collateral {
            list,
            found,
        };
        let validity = object::is_valid_locally(obj, false, check, params);
        if !validity.valid {
            return Ok((LocalAdd::Invalid(validity.error), Effects::default()));
        }
        let hash = object::hash(obj);
        if self.objects.contains_key(&hash) {
            return Ok((LocalAdd::Duplicate, Effects::default()));
        }
        let (_, kept) = self.add_object_at(obj, list, clock, synced, params, self.cached_height)?;
        if !kept {
            return Ok((LocalAdd::Refused, Effects::default()));
        }
        if synced {
            self.relay_object(hash);
        }
        let mut effects = Effects {
            sync_bumps: self.take_sync_bumps(),
            meta: self.take_meta(),
            zmq: true,
            ..Effects::default()
        };
        let relay = self.take_relay_queue();
        effects.relay_objects = relay.objects;
        effects.relay_votes = relay.votes;
        Ok((LocalAdd::Added, effects))
    }

    /// `ProcessVoteAndRelay`: `ProcessVote`, then on acceptance `RelayVote` when `synced` and the
    /// voter is in `list`, the masternode meta's `AddGovernanceVote` and the vote notification.
    /// A parent to request is dropped, as Core does for local votes.
    // Core: governance/governance.cpp:814-824, 124-140; governance/object.cpp:470-479
    pub fn process_local_vote_at(
        &mut self,
        vote: &GovVote,
        list: &MnList,
        clock: &GovClock,
        synced: bool,
        params: &GovParams,
    ) -> (VoteOutcome, Effects) {
        let mut outcome = self.process_vote_at(vote, list, clock, params);
        outcome.to_request = None;
        let mut effects = Effects::default();
        if outcome.result != Ok(true) {
            return (outcome, effects);
        }
        if synced && list.get_by_collateral(&vote.masternode_outpoint()).is_some() {
            self.relay_vote(outcome.hash);
        }
        effects.meta = self.take_meta();
        effects.zmq = true;
        let relay = self.take_relay_queue();
        effects.relay_objects = relay.objects;
        effects.relay_votes = relay.votes;
        (outcome, effects)
    }
}
