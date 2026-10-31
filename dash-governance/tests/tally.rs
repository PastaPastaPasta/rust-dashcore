//! L0: the tallies, `UpdateSentinelVariables`, `ProcessVote`'s ordering rules and the vote file's
//! removals (`governance/object.cpp`, `governance/votedb.cpp`) on the masternode list and the
//! signed votes of the Core-produced regtest network (`common`). The list has 4 regular
//! masternodes (weight 1) and 2 evo masternodes (weight 4), 12 valid weighted, and regtest's
//! minimum quorum is 1: `UpdateSentinelVariables` needs an absolute count of
//! `max(1, 12 / 10) = 1` and `max(1, 2 * 12 / 3) = 8` for deletion (`object.cpp:970-976`).

mod common;

use std::sync::Arc;

use common::{Fixture, fixture, params, params_of};
use dash_dml::{Dmn, MnList, testkit};
use dash_governance::GovParams;
use dash_governance::exception::ExceptionType;
use dash_governance::object::{self, PROPOSAL, TRIGGER};
use dash_governance::superblocks::SuperblockSet;
use dash_governance::tally::{ObjectState, UPDATE_MIN, UniqueVoters, VoteInstance, outpoint_key};
use dash_governance::vote::{
    self, OUTCOME_ABSTAIN, OUTCOME_NO, OUTCOME_YES, SIGNAL_DELETE, SIGNAL_ENDORSED, SIGNAL_FUNDING,
    SIGNAL_VALID,
};
use dash_params::Network;
use dash_primitives::BlockHash;
use dash_primitives::{GovVote, OutPoint, TxHash, VoteOutcome, VoteSignal};
use dash_types::Numeric;

/// A time every captured vote is valid at.
fn now(fx: &Fixture) -> i64 {
    fx.json["mocktime_at_tip"].as_i64().expect("mocktime") + 2 * UPDATE_MIN
}

/// The list's masternodes: (evo, regular).
fn members(fx: &Fixture) -> (Vec<Arc<Dmn>>, Vec<Arc<Dmn>>) {
    let (evo, regular): (Vec<_>, Vec<_>) = fx.list.iter().cloned().partition(|d| d.is_evo());
    assert_eq!((evo.len(), regular.len()), (2, 4));
    assert_eq!(fx.list.counts().valid_weighted, 12);
    (evo, regular)
}

fn set_vote(state: &mut ObjectState, dmn: &Dmn, signal: i32, outcome: i32) {
    state.current.entry(outpoint_key(&dmn.collateral)).or_default().insert(
        signal,
        VoteInstance {
            outcome,
            last_update: 0,
            creation_time: 1,
        },
    );
}

#[test]
fn counts_by_weight_and_unique_voters() {
    let fx = fixture();
    let (evo, reg) = members(&fx);
    let mut state = ObjectState::new(PROPOSAL, [7; 32]);
    set_vote(&mut state, &evo[0], SIGNAL_FUNDING, OUTCOME_YES);
    set_vote(&mut state, &reg[0], SIGNAL_FUNDING, OUTCOME_NO);
    set_vote(&mut state, &reg[1], SIGNAL_FUNDING, OUTCOME_NO);
    set_vote(&mut state, &reg[2], SIGNAL_FUNDING, OUTCOME_ABSTAIN);
    set_vote(&mut state, &evo[1], SIGNAL_DELETE, OUTCOME_YES);
    // A voter no longer in the list counts for nothing.
    state.current.entry(([9; 32], 0)).or_default().insert(
        SIGNAL_FUNDING,
        VoteInstance {
            outcome: OUTCOME_YES,
            ..VoteInstance::default()
        },
    );
    let l = &fx.list;
    assert_eq!(state.yes(l, SIGNAL_FUNDING), 4);
    assert_eq!(state.no(l, SIGNAL_FUNDING), 2);
    assert_eq!(state.abstain(l, SIGNAL_FUNDING), 1);
    assert_eq!(state.absolute_yes(l, SIGNAL_FUNDING), 2);
    assert_eq!(state.absolute_no(l, SIGNAL_FUNDING), -2);
    assert_eq!(state.absolute_no(l, SIGNAL_DELETE), -4);
    assert_eq!(
        state.unique_voters(l, SIGNAL_FUNDING),
        UniqueVoters {
            regular: 3,
            evo: 1
        }
    );
    assert_eq!(
        state.unique_voters(l, SIGNAL_DELETE),
        UniqueVoters {
            regular: 0,
            evo: 1
        }
    );
    assert_eq!(state.unique_voters(l, SIGNAL_VALID), UniqueVoters::default());
}

#[test]
fn sentinel_thresholds() {
    let fx = fixture();
    let p = params();
    let (evo, reg) = members(&fx);
    let sentinel = |votes: &[(&Dmn, i32, i32)]| {
        let mut state = ObjectState::new(PROPOSAL, [7; 32]);
        for &(dmn, signal, outcome) in votes {
            set_vote(&mut state, dmn, signal, outcome);
        }
        state.update_sentinel(&fx.list, 1000, &p);
        assert!(!state.dirty);
        state
    };
    // Funding and endorsement at an absolute yes count of 1.
    let none = sentinel(&[]);
    assert!(!none.cached_funding && !none.cached_endorsed && none.cached_valid);
    assert!(!none.cached_delete && none.deletion_time == 0);
    let one = sentinel(&[
        (&reg[0], SIGNAL_FUNDING, OUTCOME_YES),
        (&reg[1], SIGNAL_ENDORSED, OUTCOME_YES),
    ]);
    assert!(one.cached_funding && one.cached_endorsed);
    let level =
        sentinel(&[(&reg[0], SIGNAL_FUNDING, OUTCOME_YES), (&reg[1], SIGNAL_FUNDING, OUTCOME_NO)]);
    assert!(!level.cached_funding);
    // Valid until an absolute no count of 1.
    assert!(!sentinel(&[(&reg[0], SIGNAL_VALID, OUTCOME_NO)]).cached_valid);
    assert!(sentinel(&[(&reg[0], SIGNAL_VALID, OUTCOME_YES)]).cached_valid);
    // Deletion at 8.
    let eight =
        sentinel(&[(&evo[0], SIGNAL_DELETE, OUTCOME_YES), (&evo[1], SIGNAL_DELETE, OUTCOME_YES)]);
    assert!(eight.cached_delete);
    assert_eq!(eight.deletion_time, 1000);
    let seven = sentinel(&[
        (&evo[0], SIGNAL_DELETE, OUTCOME_YES),
        (&reg[0], SIGNAL_DELETE, OUTCOME_YES),
        (&reg[1], SIGNAL_DELETE, OUTCOME_YES),
        (&reg[2], SIGNAL_DELETE, OUTCOME_YES),
    ]);
    assert!(!seven.cached_delete && seven.deletion_time == 0);
}

/// A list of `regular` masternodes (weight 1 each) and `evo` evo masternodes (weight 4).
fn numbered_list(regular: usize, evo: usize) -> MnList {
    let mut list = MnList::new(BlockHash::from_lendian([0; 32]), 1, 0);
    let regulars = (1u32..).filter(|i| i % 8 != 0).take(regular);
    let evos = (1u32..).map(|i| 8 * i).take(evo);
    for i in regulars.chain(evos) {
        list.insert_for_tests(testkit::numbered(i));
    }
    assert_eq!(list.counts().valid_weighted as usize, regular + 4 * evo);
    list
}

/// `UpdateSentinelVariables` (`object.cpp:964-997`) where neither term of
/// `max(nGovernanceMinQuorum, n / 10)` collapses to 1: mainnet's quorum of 10 on the 12-weight
/// list, then `n / 10 = 4` on a 42-weight regtest list; the deletion threshold `(2n) / 3` at
/// a weight of 14 (9, not `2 * (14 / 3) = 8`); an empty list returns before touching anything.
#[test]
fn sentinel_thresholds_where_each_term_wins() {
    let update = |list: &MnList, voters: &[Arc<Dmn>], signal, p: &GovParams| {
        let mut state = ObjectState::new(TRIGGER, [7; 32]);
        for dmn in voters {
            set_vote(&mut state, dmn, signal, OUTCOME_YES);
        }
        state.update_sentinel(list, 1000, p);
        state
    };
    // Mainnet: the quorum of 10 wins over 12 / 10 = 1.
    let fx = fixture();
    let main = params_of(Network::Main);
    assert_eq!(main.min_quorum(), 10);
    let (evo, reg) = members(&fx);
    let nine = [evo[0].clone(), evo[1].clone(), reg[0].clone()];
    let ten = [evo[0].clone(), evo[1].clone(), reg[0].clone(), reg[1].clone()];
    assert!(!update(&fx.list, &nine, SIGNAL_FUNDING, &main).cached_funding);
    assert!(update(&fx.list, &ten, SIGNAL_FUNDING, &main).cached_funding);
    assert!(!update(&fx.list, &nine, SIGNAL_ENDORSED, &main).cached_endorsed);
    assert!(update(&fx.list, &ten, SIGNAL_ENDORSED, &main).cached_endorsed);
    // Deletion: max(10, 2 * 12 / 3 = 8) = 10.
    assert!(!update(&fx.list, &nine, SIGNAL_DELETE, &main).cached_delete);
    assert!(update(&fx.list, &ten, SIGNAL_DELETE, &main).cached_delete);

    // Regtest's quorum of 1 against 42 / 10 = 4.
    let p = params();
    let list = numbered_list(26, 4);
    let voters: Vec<Arc<Dmn>> = list.iter().filter(|d| !d.is_evo()).cloned().collect();
    assert!(!update(&list, &voters[..3], SIGNAL_FUNDING, &p).cached_funding);
    assert!(update(&list, &voters[..4], SIGNAL_FUNDING, &p).cached_funding);
    assert!(!update(&list, &voters[..3], SIGNAL_ENDORSED, &p).cached_endorsed);
    assert!(update(&list, &voters[..4], SIGNAL_ENDORSED, &p).cached_endorsed);
    let no = |n: usize| {
        let mut state = ObjectState::new(TRIGGER, [7; 32]);
        for dmn in &voters[..n] {
            set_vote(&mut state, dmn, SIGNAL_VALID, OUTCOME_NO);
        }
        state.update_sentinel(&list, 1000, &p);
        state.cached_valid
    };
    assert!(no(3) && !no(4));

    // Weight 14: (2 * 14) / 3 = 9.
    let list = numbered_list(14, 0);
    let voters: Vec<Arc<Dmn>> = list.iter().cloned().collect();
    assert!(!update(&list, &voters[..8], SIGNAL_DELETE, &p).cached_delete);
    assert!(update(&list, &voters[..9], SIGNAL_DELETE, &p).cached_delete);

    // An empty list: Core returns before `fDirtyCache = false` (`object.cpp:971`).
    let empty = numbered_list(0, 0);
    let mut state = ObjectState::new(TRIGGER, [7; 32]);
    state.cached_funding = true;
    state.cached_valid = false;
    state.update_sentinel(&empty, 1000, &p);
    assert!(state.dirty && state.cached_funding && !state.cached_valid && !state.cached_delete);
}

/// `MarkForDeletion` keeps the first time (`object.h:352-358`).
#[test]
fn deletion_keeps_the_first_time() {
    let mut state = ObjectState::new(PROPOSAL, [7; 32]);
    state.mark_for_deletion(5);
    state.mark_for_deletion(9);
    assert!(state.cached_delete);
    assert_eq!(state.deletion_time, 5);
}

fn with_outcome(v: &GovVote, outcome: i32) -> GovVote {
    GovVote {
        outcome: VoteOutcome::new(outcome as u32),
        ..v.clone()
    }
}

fn with_signal(v: &GovVote, signal: i32) -> GovVote {
    GovVote {
        signal: VoteSignal::new(signal as u32),
        ..v.clone()
    }
}

/// Obsolete votes and the same-timestamp rule (`object.cpp:419-441`) come before the signature.
#[test]
fn vote_order_rules() {
    let fx = fixture();
    let p = params();
    let now = now(&fx);
    let (obj, v) = fx
        .objects
        .iter()
        .flat_map(|(o, votes)| votes.iter().map(move |v| (o, v)))
        .find(|(_, v)| vote::outcome(v) == OUTCOME_NO)
        .expect("a no vote");
    let mut state = ObjectState::new(object::object_type(obj), object::hash(obj));
    state.process_vote(v, &fx.list, false, now, &p).unwrap_or_else(|e| panic!("{e}"));
    let refused = |state: &mut ObjectState, w: &GovVote| {
        state.process_vote(w, &fx.list, false, now, &p).expect_err("refused")
    };
    let older = GovVote {
        time: v.time - 1,
        ..v.clone()
    };
    let e = refused(&mut state, &older);
    assert_eq!(e.kind(), ExceptionType::None);
    assert!(e.message().ends_with("-- Obsolete vote"), "{e}");
    let e = refused(&mut state, &with_outcome(v, OUTCOME_YES));
    assert_eq!(e.kind(), ExceptionType::None);
    assert!(e.message().ends_with("same timestamp for the different outcome, rejected"), "{e}");
    // A larger outcome at the same time reaches the signature check.
    let e = refused(&mut state, &with_outcome(v, OUTCOME_ABSTAIN));
    assert_eq!(e.kind(), ExceptionType::PermanentError, "{e}");
    assert!(e.message().contains("Invalid vote, MN outpoint"), "{e}");
}

/// `ClearMasternodeVotes` (`object.cpp:483-497`) drops the votes of a collateral not in the list.
#[test]
fn votes_of_gone_masternodes_are_cleared() {
    let fx = fixture();
    let p = params();
    let (obj, votes) = fx.objects.iter().find(|(_, v)| !v.is_empty()).expect("votes");
    let v = &votes[0];
    let mut state = ObjectState::new(object::object_type(obj), object::hash(obj));
    state.process_vote(v, &fx.list, false, now(&fx), &p).unwrap_or_else(|e| panic!("{e}"));
    let gone = GovVote {
        masternode_outpoint: OutPoint {
            hash: TxHash::from_lendian([9; 32]),
            index: 0,
        },
        ..v.clone()
    };
    state.file.add(&gone);
    state.current.insert(outpoint_key(&gone.masternode_outpoint), Default::default());
    state.dirty = false;
    state.clear_masternode_votes(&fx.list);
    assert!(state.dirty);
    assert!(!state.current.contains_key(&outpoint_key(&gone.masternode_outpoint)));
    assert!(state.current.contains_key(&outpoint_key(&v.masternode_outpoint)));
    assert!(!state.file.has(&vote::hash(&gone)));
    assert!(state.file.has(&vote::hash(v)));
    assert_eq!(state.file.len(), 1);
}

/// `RemoveInvalidVotes` (`object.cpp:499-536`, `votedb.cpp:80-99`): only the masternode's votes
/// whose signatures fail, by the key `ProcessVote` used for each signal.
#[test]
fn invalid_votes_of_one_masternode_are_removed() {
    let fx = fixture();
    let p = params();
    let now = now(&fx);
    // A proposal one masternode voted funding (the voting key) and other signals (the operator
    // key) on.
    let (obj, votes) = fx
        .objects
        .iter()
        .find(|(o, votes)| {
            object::object_type(o) == PROPOSAL
                && votes.iter().any(|v| {
                    vote::signal(v) == SIGNAL_FUNDING
                        && votes.iter().any(|w| {
                            w.masternode_outpoint == v.masternode_outpoint
                                && vote::signal(w) != SIGNAL_FUNDING
                        })
                })
        })
        .expect("a proposal with funding and other votes from one masternode");
    let mut state = ObjectState::new(PROPOSAL, object::hash(obj));
    for v in votes.iter().rev() {
        state.process_vote(v, &fx.list, false, now, &p).unwrap_or_else(|e| panic!("{e}"));
    }
    let mine = votes
        .iter()
        .find(|v| {
            vote::signal(v) == SIGNAL_FUNDING
                && votes.iter().any(|w| {
                    w.masternode_outpoint == v.masternode_outpoint
                        && vote::signal(w) != SIGNAL_FUNDING
                })
        })
        .expect("checked above");
    let other = votes
        .iter()
        .find(|w| w.masternode_outpoint != mine.masternode_outpoint)
        .expect("another masternode's vote");
    // A signal neither voted: the copied signature does not sign it.
    let unvoted = |v: &GovVote| {
        [SIGNAL_ENDORSED, SIGNAL_DELETE, SIGNAL_VALID]
            .into_iter()
            .find(|&s| {
                !votes
                    .iter()
                    .any(|w| w.masternode_outpoint == v.masternode_outpoint && vote::signal(w) == s)
            })
            .expect("an unvoted signal")
    };
    let forged = |state: &mut ObjectState, v: &GovVote| {
        let f = with_signal(v, unvoted(v));
        state.file.add(&f);
        state.current.entry(outpoint_key(&f.masternode_outpoint)).or_default().insert(
            vote::signal(&f),
            VoteInstance {
                outcome: vote::outcome(&f),
                last_update: 0,
                creation_time: f.time,
            },
        );
        f
    };
    let mine_forged = forged(&mut state, mine);
    let other_forged = forged(&mut state, other);
    let held = state.file.len();
    let mine_key = outpoint_key(&mine.masternode_outpoint);
    let signals_before = state.current[&mine_key].len();

    let removed = state.remove_invalid_votes(&fx.list, &mine.masternode_outpoint, now, &p);
    assert_eq!(removed.into_iter().collect::<Vec<_>>(), vec![vote::hash(&mine_forged)]);
    assert_eq!(state.file.len(), held - 1);
    assert!(!state.file.has(&vote::hash(&mine_forged)));
    assert!(state.file.has(&vote::hash(&other_forged)));
    let rec = &state.current[&mine_key];
    assert_eq!(rec.len(), signals_before - 1);
    assert!(!rec.contains_key(&vote::signal(&mine_forged)));
    assert!(
        state.current[&outpoint_key(&other.masternode_outpoint)]
            .contains_key(&vote::signal(&other_forged))
    );
    // Nothing to remove for a collateral without votes, or for valid votes alone.
    let absent = OutPoint {
        hash: TxHash::from_lendian([9; 32]),
        index: 0,
    };
    assert!(state.remove_invalid_votes(&fx.list, &absent, now, &p).is_empty());
    assert!(state.remove_invalid_votes(&fx.list, &mine.masternode_outpoint, now, &p).is_empty());

    // A masternode whose only vote fails leaves the tallies.
    let mut lone = ObjectState::new(PROPOSAL, object::hash(obj));
    let f = forged(&mut lone, other);
    let removed = lone.remove_invalid_votes(&fx.list, &other.masternode_outpoint, now, &p);
    assert_eq!(removed.into_iter().collect::<Vec<_>>(), vec![vote::hash(&f)]);
    assert!(lone.current.is_empty() && lone.file.is_empty());
}

/// `GetBestSuperblock` (`superblock.cpp:556-576`): among triggers for one height, the first in
/// map order (internal hash bytes) with a strictly larger absolute funding count.
#[test]
fn best_superblock_ties_go_to_the_first_hash() {
    let fx = fixture();
    let p = params();
    let (_, reg) = members(&fx);
    let trigger = fx
        .objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("node 0's trigger");
    let copy = dash_primitives::GovObject {
        time: trigger.time + 1,
        ..trigger.clone()
    };
    let mut hashes = [object::hash(trigger), object::hash(&copy)];
    let mut set = SuperblockSet::new();
    for o in [trigger, &copy] {
        assert_eq!(set.add_trigger(o, 179, &p), Ok(true));
    }
    hashes.sort();
    let states = |yes: [usize; 2]| {
        let mut m = std::collections::BTreeMap::new();
        for (h, n) in hashes.iter().zip(yes) {
            let mut state = ObjectState::new(TRIGGER, *h);
            for dmn in &reg[..n] {
                set_vote(&mut state, dmn, SIGNAL_FUNDING, OUTCOME_YES);
            }
            m.insert(*h, state);
        }
        m
    };
    assert_eq!(set.best(&fx.list, 180, &states([1, 1]), &p), Some(hashes[0]));
    assert_eq!(set.best(&fx.list, 180, &states([1, 2]), &p), Some(hashes[1]));
    assert_eq!(set.best(&fx.list, 180, &states([2, 1]), &p), Some(hashes[0]));
    assert_eq!(set.best(&fx.list, 180, &states([0, 0]), &p), None);
}
