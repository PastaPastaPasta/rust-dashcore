//! A governance object's votes and cached flags: `CGovernanceObject::ProcessVote`, the vote file,
//! the weighted counts and `UpdateSentinelVariables` (notes D10).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use dash_dml::MnList;
use dash_evo::types::ProTxHash;
use dash_primitives::{GovVote, OutPoint, TxHash, VoteOutcome, VoteSignal};
use dash_types::Numeric;

use crate::GovParams;
use crate::exception::{ExceptionType, GovException};
use crate::object::{PROPOSAL, outpoint_short, txid_bytes};
use crate::strings::hash_hex;
use crate::vote::{
    self, OUTCOME_ABSTAIN, OUTCOME_NO, OUTCOME_YES, SIGNAL_DELETE, SIGNAL_ENDORSED, SIGNAL_FUNDING,
    SIGNAL_NONE, SIGNAL_UNKNOWN, SIGNAL_VALID,
};

/// `GOVERNANCE_UPDATE_MIN`: a masternode's votes on one signal of one object at most hourly.
// Core: governance/object.h:91
pub const UPDATE_MIN: i64 = 60 * 60;

/// The penalty `ProcessVote` gives a peer for a permanently invalid vote.
const PENALTY: i32 = 20;

/// `vote_instance_t`: a masternode's current vote on one signal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VoteInstance {
    pub outcome: i32,
    /// `last_update` (Unix seconds): when this node accepted it, with rate checks on.
    pub last_update: i64,
    /// `nCreationTime`: the vote's own time.
    pub creation_time: i64,
}

/// A collateral outpoint in Core's `COutPoint` order: the txid's internal bytes, then the index.
pub type OutpointKey = ([u8; 32], u32);

pub fn outpoint_key(op: &OutPoint) -> OutpointKey {
    (txid_bytes(&op.hash), op.index)
}

/// `CGovernanceObjectVoteFile`: the accepted votes, newest first, without the older votes they
/// replace.
// Core: governance/votedb.cpp
#[derive(Clone, Debug, Default)]
pub struct VoteFile {
    votes: VecDeque<GovVote>,
    index: BTreeSet<[u8; 32]>,
}

impl VoteFile {
    /// `HasVote(hash)`.
    pub fn has(&self, hash: &[u8; 32]) -> bool {
        self.index.contains(hash)
    }

    /// A loaded vote file (`RebuildIndex`): the votes in order, without the later copies of a
    /// hash, and without `RemoveOldVotes`.
    // Core: governance/votedb.cpp:121-135
    pub fn from_list(votes: impl IntoIterator<Item = GovVote>) -> Self {
        let mut file = VoteFile::default();
        for v in votes {
            if file.index.insert(vote::hash(&v)) {
                file.votes.push_back(v);
            }
        }
        file
    }

    /// `GetVoteCount()`.
    pub fn len(&self) -> usize {
        self.votes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.votes.is_empty()
    }

    /// `GetVotes()`, newest first.
    pub fn votes(&self) -> impl Iterator<Item = &GovVote> {
        self.votes.iter()
    }

    /// `AddVote`: a new vote goes first, then `RemoveOldVotes`.
    // Core: governance/votedb.cpp:32-43, 101-118
    pub fn add(&mut self, vote: &GovVote) {
        let hash = vote::hash(vote);
        if !self.index.insert(hash) {
            return;
        }
        self.votes.push_front(vote.clone());
        self.retain(|v| {
            !(v.masternode_outpoint == vote.masternode_outpoint
                && v.parent_hash == vote.parent_hash
                && v.signal == vote.signal
                && v.time < vote.time)
        });
    }

    fn retain(&mut self, mut keep: impl FnMut(&GovVote) -> bool) -> BTreeSet<[u8; 32]> {
        let mut removed = BTreeSet::new();
        self.votes.retain(|v| {
            let k = keep(v);
            if !k {
                removed.insert(vote::hash(v));
            }
            k
        });
        for hash in &removed {
            self.index.remove(hash);
        }
        removed
    }
}

/// A held object's tallies and flags (the mutable part of `CGovernanceObject`).
#[derive(Clone, Debug)]
pub struct ObjectState {
    /// `m_obj.type`.
    pub obj_type: i32,
    /// `GetHash()`, internal order.
    pub hash: [u8; 32],
    pub cached_funding: bool,
    pub cached_valid: bool,
    pub cached_delete: bool,
    pub cached_endorsed: bool,
    pub dirty: bool,
    pub expired: bool,
    pub deletion_time: i64,
    /// `mapCurrentMNVotes`.
    pub current: BTreeMap<OutpointKey, BTreeMap<i32, VoteInstance>>,
    /// `fileVotes`.
    pub file: VoteFile,
}

/// `UniqueVoterCount`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UniqueVoters {
    pub regular: u32,
    pub evo: u32,
}

impl ObjectState {
    /// A new object's state (`object.h` initialisers).
    // Core: governance/object.h:150-182
    pub fn new(obj_type: i32, hash: [u8; 32]) -> Self {
        ObjectState {
            obj_type,
            hash,
            cached_funding: false,
            cached_valid: true,
            cached_delete: false,
            cached_endorsed: false,
            dirty: true,
            expired: false,
            deletion_time: 0,
            current: BTreeMap::new(),
            file: VoteFile::default(),
        }
    }

    /// `MarkForDeletion`.
    // Core: governance/object.h:352-358
    pub fn mark_for_deletion(&mut self, time: i64) {
        self.cached_delete = true;
        if self.deletion_time == 0 {
            self.deletion_time = time;
        }
    }

    /// `CGovernanceObject::ProcessVote` at `GetAdjustedTime()` = `adjusted_now`: the voter's
    /// proTxHash for `CMasternodeMetaMan::AddGovernanceVote` when the vote is accepted.
    // Core: governance/object.cpp:375-481
    pub fn process_vote(
        &mut self,
        vote: &GovVote,
        list: &MnList,
        rate_checks: bool,
        adjusted_now: i64,
        p: &GovParams,
    ) -> Result<ProTxHash, GovException> {
        const F: &str = "CGovernanceObject::ProcessVote";
        let err = |kind, penalty, msg: String| Err(GovException::new(kind, &msg, penalty));
        let vote_hash = vote::hash(vote);
        if self.file.has(&vote_hash) {
            return err(ExceptionType::None, 0, format!("{F} -- Already known valid vote"));
        }
        let outpoint = outpoint_short(&vote.masternode_outpoint);
        let Some(dmn) = list.get_by_collateral(&vote.masternode_outpoint) else {
            return err(
                ExceptionType::PermanentError,
                PENALTY,
                format!("{F} -- Masternode {outpoint} not found"),
            );
        };
        let signal = vote::signal(vote);
        if signal == SIGNAL_NONE {
            return err(ExceptionType::Warning, 0, format!("{F} -- Vote signal: none"));
        }
        if !(SIGNAL_NONE..SIGNAL_UNKNOWN).contains(&signal) {
            return err(
                ExceptionType::PermanentError,
                PENALTY,
                format!("{F} -- Unsupported vote signal: {}", vote::signal_to_string(signal)),
            );
        }
        let key = outpoint_key(&vote.masternode_outpoint);
        let current =
            self.current.get(&key).and_then(|r| r.get(&signal)).copied().unwrap_or_default();
        if vote.time < current.creation_time {
            return err(ExceptionType::None, 0, format!("{F} -- Obsolete vote"));
        }
        if vote.time == current.creation_time && vote::outcome(vote) < current.outcome {
            return err(
                ExceptionType::None,
                0,
                format!("{F} -- Invalid vote, same timestamp for the different outcome, rejected"),
            );
        }
        let object_hash = hash_hex(&self.hash);
        let mut last_update = current.last_update;
        if rate_checks {
            if current.last_update > adjusted_now.saturating_sub(UPDATE_MIN) {
                return err(
                    ExceptionType::TemporaryError,
                    0,
                    format!(
                        "{F} -- Masternode voting too often, MN outpoint = {outpoint}, governance object hash = {object_hash}, last update = {}, current time = {adjusted_now}",
                        current.last_update
                    ),
                );
            }
            last_update = adjusted_now;
        }
        let only_voting_key = self.obj_type == PROPOSAL && signal == SIGNAL_FUNDING;
        if !vote::is_valid(vote, list, only_voting_key, adjusted_now, p) {
            return err(
                ExceptionType::PermanentError,
                PENALTY,
                format!(
                    "{F} -- Invalid vote, MN outpoint = {outpoint}, governance object hash = {object_hash}, vote hash = {}",
                    hash_hex(&vote_hash)
                ),
            );
        }
        self.current.entry(key).or_default().insert(
            signal,
            VoteInstance {
                outcome: vote::outcome(vote),
                last_update,
                creation_time: vote.time,
            },
        );
        self.file.add(vote);
        self.dirty = true;
        Ok(dmn.pro_tx_hash)
    }

    /// `CountMatchingVotes`: the voters' weights, by the tip's list (0 for one not in it).
    // Core: governance/object.cpp:880-896
    pub fn count_matching(&self, list: &MnList, signal: i32, outcome: i32) -> i32 {
        self.current
            .iter()
            .filter(|(_, rec)| rec.get(&signal).is_some_and(|i| i.outcome == outcome))
            .filter_map(|((txid, index), _)| list.get_by_collateral(&outpoint(*txid, *index)))
            .map(|dmn| dmn.weight() as i32)
            .sum()
    }

    /// `GetYesCount`.
    pub fn yes(&self, list: &MnList, signal: i32) -> i32 {
        self.count_matching(list, signal, OUTCOME_YES)
    }

    /// `GetNoCount`.
    pub fn no(&self, list: &MnList, signal: i32) -> i32 {
        self.count_matching(list, signal, OUTCOME_NO)
    }

    /// `GetAbstainCount`.
    pub fn abstain(&self, list: &MnList, signal: i32) -> i32 {
        self.count_matching(list, signal, OUTCOME_ABSTAIN)
    }

    /// `GetAbsoluteYesCount`: yes minus no.
    // Core: governance/object.cpp:902-906
    pub fn absolute_yes(&self, list: &MnList, signal: i32) -> i32 {
        self.yes(list, signal) - self.no(list, signal)
    }

    /// `GetAbsoluteNoCount`: no minus yes.
    pub fn absolute_no(&self, list: &MnList, signal: i32) -> i32 {
        self.no(list, signal) - self.yes(list, signal)
    }

    /// `GetUniqueVoterCount`: masternodes in the tip's list with a vote on `signal`.
    // Core: governance/object.cpp:932-951
    pub fn unique_voters(&self, list: &MnList, signal: i32) -> UniqueVoters {
        let mut count = UniqueVoters::default();
        for ((txid, index), rec) in &self.current {
            if !rec.contains_key(&signal) {
                continue;
            }
            match list.get_by_collateral(&outpoint(*txid, *index)) {
                Some(dmn) if dmn.is_evo() => count.evo += 1,
                Some(_) => count.regular += 1,
                None => {}
            }
        }
        count
    }

    /// `UpdateSentinelVariables`, with `GetTime()` = `now` for the deletion mark.
    // Core: governance/object.cpp:964-997
    pub fn update_sentinel(&mut self, list: &MnList, now: i64, p: &GovParams) {
        // `static_cast<int>(m_valid_weighted)`.
        let weighted = list.counts().valid_weighted as i32;
        if weighted == 0 {
            return;
        }
        let vote_req = p.min_quorum().max(weighted / 10);
        let delete_req = p.min_quorum().max((2 * weighted) / 3);
        self.dirty = false;
        self.cached_funding = self.absolute_yes(list, SIGNAL_FUNDING) >= vote_req;
        if self.absolute_yes(list, SIGNAL_DELETE) >= delete_req {
            self.mark_for_deletion(now);
        }
        self.cached_endorsed = self.absolute_yes(list, SIGNAL_ENDORSED) >= vote_req;
        self.cached_valid = self.absolute_no(list, SIGNAL_VALID) < vote_req;
    }

    /// `ClearMasternodeVotes`: drop the votes of masternodes no longer in the list.
    // Core: governance/object.cpp:483-497
    pub fn clear_masternode_votes(&mut self, list: &MnList) {
        let gone: Vec<OutpointKey> = self
            .current
            .keys()
            .filter(|(txid, index)| list.get_by_collateral(&outpoint(*txid, *index)).is_none())
            .copied()
            .collect();
        for key in gone {
            self.file.retain(|v| outpoint_key(&v.masternode_outpoint) != key);
            self.current.remove(&key);
            self.dirty = true;
        }
    }

    /// `RemoveInvalidVotes(tip_mn_list, mnOutpoint)`: the masternode's votes whose signatures no
    /// longer check (a key change), and the hashes removed.
    // Core: governance/object.cpp:499-536, governance/votedb.cpp:80-99
    pub fn remove_invalid_votes(
        &mut self,
        list: &MnList,
        op: &OutPoint,
        adjusted_now: i64,
        p: &GovParams,
    ) -> BTreeSet<[u8; 32]> {
        let key = outpoint_key(op);
        if !self.current.contains_key(&key) {
            return BTreeSet::new();
        }
        let proposal = self.obj_type == PROPOSAL;
        let removed = self.file.retain(|v| {
            v.masternode_outpoint != *op || {
                let use_voting_key = proposal && vote::signal(v) == SIGNAL_FUNDING;
                vote::is_valid(v, list, use_voting_key, adjusted_now, p)
            }
        });
        if removed.is_empty() {
            return removed;
        }
        let parent = TxHash::from_lendian(self.hash);
        if let Some(rec) = self.current.get_mut(&key) {
            rec.retain(|&signal, instance| {
                // The vote the instance stands for, rebuilt (`tmpVote`): its hash has no signature.
                let rebuilt = GovVote {
                    masternode_outpoint: *op,
                    parent_hash: parent,
                    outcome: VoteOutcome::new(instance.outcome as u32),
                    signal: VoteSignal::new(signal as u32),
                    time: instance.creation_time,
                    sig: Vec::new(),
                };
                !removed.contains(&vote::hash(&rebuilt))
            });
            if rec.is_empty() {
                self.current.remove(&key);
            }
        }
        self.dirty = true;
        removed
    }
}

fn outpoint(txid: [u8; 32], index: u32) -> OutPoint {
    OutPoint {
        hash: TxHash::from_lendian(txid),
        index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vote(mn: u8, signal: u32, time: i64) -> GovVote {
        GovVote {
            masternode_outpoint: outpoint([mn; 32], 0),
            parent_hash: TxHash::from_lendian([7; 32]),
            outcome: VoteOutcome::new(1),
            signal: VoteSignal::new(signal),
            time,
            sig: Vec::new(),
        }
    }

    /// `RemoveOldVotes` (`votedb.cpp:101-118`) drops only the same masternode's older votes on
    /// the same object and signal.
    #[test]
    fn vote_file_replaces_older_votes_of_one_masternode() {
        let mut file = VoteFile::default();
        let (a1, a2, b1, a_other) = (vote(1, 1, 10), vote(1, 1, 20), vote(2, 1, 5), vote(1, 2, 5));
        for v in [&a1, &b1, &a_other] {
            file.add(v);
        }
        assert_eq!(file.len(), 3);
        file.add(&a2);
        let held: Vec<[u8; 32]> = file.votes().map(vote::hash).collect();
        assert_eq!(held, [&a2, &a_other, &b1].map(vote::hash));
        assert!(!file.has(&vote::hash(&a1)));
        // An older vote added later stays, behind the newer one it does not replace.
        file.add(&a1);
        assert_eq!(file.len(), 4);
        assert_eq!(file.votes().next().map(vote::hash), Some(vote::hash(&a1)));
        // A known vote is not added again.
        file.add(&a2);
        assert_eq!(file.len(), 4);
    }

    /// `RebuildIndex` keeps an older vote and drops only a repeated hash.
    #[test]
    fn a_loaded_file_keeps_older_votes() {
        let (a1, a2) = (vote(1, 1, 10), vote(1, 1, 20));
        let file = VoteFile::from_list([a1.clone(), a2.clone(), a1.clone()]);
        let held: Vec<[u8; 32]> = file.votes().map(vote::hash).collect();
        assert_eq!(held, [&a1, &a2].map(vote::hash));
        assert!(file.has(&vote::hash(&a1)) && file.has(&vote::hash(&a2)));
    }
}
