//! Manager transitions over Core's captured objects and signed votes.
#[path = "../../tests/common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{Collaterals, Fixture, fixture, params};
use dash_dml::{KeyId, MnList, MnType, testkit};
use dash_governance::exception::ExceptionType;
use dash_governance::manager::{
    CollateralLookup, Effects, GovClock, Inventory, LocalAdd, Manager, MetaOp, ORPHAN_EXPIRATION,
};
use dash_governance::object::{self, FoundCollateral, PROPOSAL, TRIGGER};
use dash_governance::strings::Destination;
use dash_governance::strings::hash_hex;
use dash_governance::superblocks::HeightOverflow;
use dash_governance::tally::{ObjectState, VoteInstance, outpoint_key};
use dash_governance::trigger::{Status, Superblock};
use dash_governance::vote;
use dash_params::Network;
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};
use dash_primitives::{GovObject, GovVote, TxHash, VoteOutcome, VoteSignal};
use dash_types::Numeric;
use serde_json::Value;

fn checked_time(base: i64, offset: i64) -> i64 {
    base.checked_add(offset).expect("fixture time arithmetic")
}

fn now(fx: &Fixture) -> i64 {
    checked_time(fx.json["mocktime_at_tip"].as_i64().expect("time"), 7200)
}

fn checked_height(base: i32, offset: i32) -> i32 {
    base.checked_add(offset).expect("fixture height arithmetic")
}

fn voted_proposal(fx: &Fixture) -> (&GovObject, &[GovVote]) {
    fx.objects
        .iter()
        .find(|(o, v)| object::object_type(o) == PROPOSAL && v.len() > 1)
        .map(|(o, v)| (o, v.as_slice()))
        .expect("a voted proposal")
}

#[test]
fn held_postponed_and_invalid_objects() {
    let fx = fixture();
    let p = params();
    let (obj, _) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let mut lookup = Collaterals::new(&fx, 1);
    let mut m = Manager::new();
    let out = m.process_object(obj, &fx.list, now(&fx), true, &p, &lookup).expect("process");
    assert!(out.ok && !out.held && out.postponed);
    // Seen postponed objects bypass even a lookup that would now reject them.
    lookup.confirmations = Some(0);
    assert_eq!(m.process_object(obj, &fx.list, now(&fx), true, &p, &lookup), Ok(out));
    lookup.confirmations = Some(5);
    assert!(
        m.check_postponed_objects(&fx.list, now(&fx), true, &p, &lookup).expect("check").is_empty()
    );
    lookup.confirmations = Some(6);
    assert!(
        m.check_postponed_objects(&fx.list, now(&fx), false, &p, &lookup)
            .expect("check")
            .is_empty()
    );
    assert!(m.postponed().contains_key(&hash));
    let added = m.check_postponed_objects(&fx.list, now(&fx), true, &p, &lookup).expect("check");
    assert_eq!(added.len(), 1);
    assert!(added[0].ok && added[0].held && !added[0].postponed);
    assert!(m.postponed().is_empty());
    let state = m.object(&hash).expect("held");
    assert!(!state.dirty && !state.cached_funding && state.cached_valid);
    lookup.txs.clear();
    assert!(m.process_object(obj, &fx.list, now(&fx), true, &p, &lookup).expect("seen").held);
    let invalid =
        Manager::new().process_object(obj, &fx.list, now(&fx), true, &p, &lookup).expect("invalid");
    assert!(!invalid.ok && !invalid.held && !invalid.postponed);
}

#[test]
fn postponed_invalid_collateral_is_removed() {
    let fx = fixture();
    let p = params();
    let (obj, _) = voted_proposal(&fx);
    let mut lookup = Collaterals::new(&fx, 1);
    let mut m = Manager::new();
    assert!(
        m.process_object(obj, &fx.list, now(&fx), true, &p, &lookup).expect("postpone").postponed
    );
    lookup.confirmations = Some(0);
    let out = m.check_postponed_objects(&fx.list, now(&fx), true, &p, &lookup).expect("check");
    assert_eq!(out.len(), 1);
    assert!(!out[0].ok && !out[0].held);
    assert!(m.postponed().is_empty() && m.objects().is_empty());
}

#[test]
fn orphan_first_vote_expiry_and_replay() {
    assert_eq!(ORPHAN_EXPIRATION, 600);
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let now = now(&fx);
    let mut m = Manager::new();
    let out = m.process_vote(&votes[0], &fx.list, now, &p);
    let e = out.result.expect_err("orphan");
    assert_eq!((e.kind(), e.penalty()), (ExceptionType::Warning, 0));
    assert_eq!(out.to_request, Some(hash));
    assert_eq!(
        e.message(),
        format!(
            "GOVERNANCE_EXCEPTION_WARNING:CGovernanceManager::ProcessVote -- Unknown parent object {}, MN outpoint = {}",
            hash_hex(&hash),
            object::outpoint_short(&votes[0].masternode_outpoint())
        )
    );
    let second = m.process_vote(&votes[1], &fx.list, checked_time(now, 1), &p);
    assert!(second.result.is_err() && second.to_request.is_none());
    let (stored, expiry) = m.orphan(&hash).expect("orphan");
    assert_eq!((vote::hash(stored), expiry), (vote::hash(&votes[0]), checked_time(now, 600)));
    assert!(m.cleanup_orphans(checked_time(now, 600)).is_empty());
    assert!(
        m.process_object(obj, &fx.list, checked_time(now, 600), true, &p, &lookup)
            .expect("add")
            .held
    );
    assert!(m.orphan(&hash).is_none());
    assert!(m.object(&hash).expect("held").file.has(&vote::hash(&votes[0])));
    // Replayed orphans bypass the hourly rate check and are indexed as accepted votes.
    // Core: governance/governance.cpp v24.0.2:299-301 (dashpay/dash#8112), :649, :842-846,
    // :196-202
    assert!(m.accepted_vote(&vote::hash(&votes[0])));
    assert!(m.have_vote(&vote::hash(&votes[0])));
    assert!(!m.confirm_inventory_request(Inventory::Vote(vote::hash(&votes[0]))));
    let again = m.process_vote(&votes[0], &fx.list, checked_time(now, 600), &p);
    assert_eq!(again.result, Ok(false));
    let mut expired = Manager::new();
    expired.process_vote(&votes[0], &fx.list, now, &p);
    assert_eq!(expired.cleanup_orphans(checked_time(now, 601)), vec![hash]);
    expired.process_vote(&votes[0], &fx.list, now, &p);
    expired.process_object(obj, &fx.list, checked_time(now, 601), true, &p, &lookup).expect("add");
    assert!(expired.orphan(&hash).is_none());
    assert!(expired.object(&hash).expect("held").file.is_empty());
}

#[test]
fn orphan_expiry_saturates_at_max_time() {
    let fx = fixture();
    let p = params();
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    for now in [
        i64::MAX.checked_sub(600).expect("time arithmetic"),
        i64::MAX.checked_sub(599).expect("time arithmetic"),
    ] {
        let mut m = Manager::new();
        let out = m.process_vote(&votes[0], &fx.list, now, &p);
        assert_eq!(out.to_request, Some(hash));
        assert_eq!(out.result.expect_err("orphan").kind(), ExceptionType::Warning);
        assert_eq!(m.orphan(&hash).expect("orphan").1, i64::MAX);
    }
}

#[test]
fn orphan_for_failed_trigger_add_stays_pending() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let base = fx
        .objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("trigger");
    let list = trigger_list(&fx, base);
    let invalid = signed_trigger(base, b"{}".to_vec(), base.time());
    let hash = object::hash(&invalid);
    let unsigned = GovVote::new(
        base.masternode_outpoint(),
        TxHash::from_lendian(hash),
        VoteOutcome::new(vote::OUTCOME_YES as u32),
        VoteSignal::new(vote::SIGNAL_FUNDING as u32),
        base.time(),
        Vec::new(),
    );
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[42; 32]).expect("fixed key");
    let pending = GovVote::new(
        unsigned.masternode_outpoint(),
        unsigned.parent_hash(),
        unsigned.outcome(),
        unsigned.signal(),
        unsigned.time(),
        key.sign_with(&vote::signature_hash(&unsigned), BlsSigId::Basic).to_bytes().to_vec(),
    );
    let mut m = Manager::new();
    let out = m.process_vote(&pending, &list, base.time(), &p);
    assert_eq!(out.to_request, Some(hash));
    assert_eq!(out.result.expect_err("orphan").kind(), ExceptionType::Warning);
    let added = m.process_object(&invalid, &list, base.time(), true, &p, &lookup).expect("add");
    assert!(added.held);
    assert!(m.orphan(&hash).is_some());
    let state = m.object(&hash).expect("held");
    assert!(state.cached_delete && !state.expired && state.file.is_empty());
}

#[test]
fn orphan_parents_are_ordered_and_unbounded() {
    let fx = fixture();
    let p = params();
    let now = now(&fx);
    let mut m = Manager::new();
    let mut expected = Vec::new();
    for (obj, votes) in fx.objects.iter().filter(|(_, votes)| !votes.is_empty()) {
        let hash = object::hash(obj);
        let out = m.process_vote(&votes[0], &fx.list, now, &p);
        assert_eq!(out.to_request, Some(hash));
        assert_eq!(out.result.expect_err("orphan").kind(), ExceptionType::Warning);
        expected.push(hash);
    }
    expected.sort();
    assert_eq!(m.orphan_count(), expected.len());
    assert_eq!(m.orphan_parents().copied().collect::<Vec<_>>(), expected);
}

#[test]
fn orphan_replay_failure_keeps_entry() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let mut m = Manager::new();
    m.process_vote(&votes[0], &fx.list, now(&fx), &p);
    let empty = MnList::new(*fx.list.block_hash(), fx.list.height(), 0);
    m.process_object(obj, &empty, now(&fx), true, &p, &lookup).expect("add");
    assert!(m.orphan(&hash).is_some());
    assert!(m.object(&hash).expect("held").file.is_empty());
    assert!(m.take_meta().is_empty());
    assert!(m.take_relay_queue().votes.is_empty());
}

#[test]
fn known_vote_and_invalid_unknown_vote() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (obj, votes) = voted_proposal(&fx);
    let mut m = Manager::new();
    m.process_object(obj, &fx.list, now(&fx), true, &p, &lookup).expect("add");
    assert_eq!(m.process_vote(&votes[0], &fx.list, now(&fx), &p).result, Ok(true));
    assert!(m.accepted_vote(&vote::hash(&votes[0])));
    assert_eq!(m.process_vote(&votes[0], &fx.list, now(&fx), &p).result, Ok(false));
    let bad = GovVote::new(
        votes[0].masternode_outpoint(),
        votes[0].parent_hash(),
        votes[0].outcome(),
        votes[0].signal(),
        votes[0].time(),
        vec![],
    );
    let out = Manager::new().process_vote(&bad, &fx.list, now(&fx), &p);
    let e = out.result.expect_err("signature");
    assert_eq!((e.kind(), e.penalty()), (ExceptionType::PermanentError, 20));
    assert!(e.message().contains("Invalid vote for unknown parent object"));
    assert!(out.to_request.is_none());
    assert!(m.process_vote_message(&[], &fx.list, now(&fx), &p).is_none());
    assert_eq!(m.process_object_message(&[], &fx.list, now(&fx), true, &p, &lookup), Ok(None));
}

fn signed_trigger(base: &GovObject, data: Vec<u8>, time: i64) -> GovObject {
    let make = |sig| {
        GovObject::new(
            base.hash_parent(),
            base.revision(),
            time,
            base.collateral_hash(),
            data.clone(),
            base.object_type(),
            base.masternode_outpoint(),
            sig,
        )
    };
    let obj = make(Vec::new());
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[42; 32]).expect("fixed key");
    make(key.sign_with(&object::signature_hash(&obj), BlsSigId::Basic).to_bytes().to_vec())
}

fn trigger_list(fx: &Fixture, obj: &GovObject) -> MnList {
    let mut list = fx.list.clone();
    let mn = list.get_by_collateral(&obj.masternode_outpoint()).expect("signer").clone();
    let mut cold = (*mn.cold).clone();
    cold.pub_key_operator = testkit::operator_key(42, BlsSchemeKind::Basic);
    list.update(&mn.pro_tx_hash, mn.hot, Arc::new(testkit::fresh(cold))).expect("rekey");
    list
}

#[test]
fn trigger_rate_bypass_refusal_and_failed_add_order() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let base = fx
        .objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("trigger");
    let list = trigger_list(&fx, base);
    let now = base.time();
    let key = outpoint_key(&base.masternode_outpoint());
    let mut m = Manager::new();
    // Locally valid signed triggers whose payment data fails AddTrigger still fill the rate ring.
    for i in 0..4 {
        let obj = signed_trigger(base, b"{}".to_vec(), checked_time(now, i));
        let out = m.process_object(&obj, &list, now, true, &p, &lookup).expect("add");
        assert!(out.ok && out.held);
        let state = m.object(&out.hash).expect("held");
        assert!(state.cached_delete && state.deletion_time == now);
        assert!(m.superblocks().get(&out.hash).is_none());
        let record = m.last_object_record(&key).expect("attempt counted");
        assert_eq!(record.buffer.count(), (i + 1) as usize);
        assert!(record.status_ok);
    }
    let valid = signed_trigger(base, base.data().clone(), checked_time(now, 4));
    let bad_sig = GovObject::new(
        valid.hash_parent(),
        valid.revision(),
        valid.time(),
        valid.collateral_hash(),
        valid.data().clone(),
        valid.object_type(),
        valid.masternode_outpoint(),
        Vec::new(),
    );
    let invalid = m.process_object(&bad_sig, &list, now, true, &p, &lookup).expect("signature");
    assert!(!invalid.ok && !invalid.held);
    assert!(m.last_object_record(&key).expect("record").status_ok);
    // status_ok bypasses the first rate calculation; after signature verification the forced
    // check refuses the fifth close timestamp and records failed status.
    let refused = m.process_object(&valid, &list, now, true, &p, &lookup).expect("rate");
    assert!(refused.ok && !refused.held && !refused.postponed);
    let record = m.last_object_record(&key).expect("record");
    assert_eq!(record.buffer.count(), 4);
    assert!(!record.status_ok);
    assert_eq!(m.process_object(&valid, &list, now, true, &p, &lookup), Ok(refused));
    // Unsynced processing bypasses the rate gate and resets status on insertion.
    assert!(m.process_object(&valid, &list, now, false, &p, &lookup).expect("unsynced").held);
    assert!(m.last_object_record(&key).expect("record").status_ok);
    let future = signed_trigger(base, base.data().clone(), checked_time(now, 3601));
    let mut fresh = Manager::new();
    assert!(!fresh.process_object(&future, &list, now, true, &p, &lookup).expect("future").held);
    assert!(fresh.last_object_record(&key).is_none());
    assert!(fresh.process_object(&future, &list, now, false, &p, &lookup).expect("unsynced").held);
}

#[test]
fn trigger_expiry_and_height_overflow() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::from_fixture(&fx);
    let base = fx
        .objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("trigger");
    let sb = Superblock::parse(base, &p).expect("trigger");
    let past_expiry = fx
        .list
        .clone()
        .with_height(checked_height(checked_height(sb.height, p.superblock_cycle()), 1));
    let mut expired = Manager::new();
    let out =
        expired.process_object(base, &past_expiry, base.time(), true, &p, &lookup).expect("add");
    assert!(out.ok && out.held);
    assert!(expired.object(&out.hash).expect("held").cached_delete);
    assert!(expired.superblocks().get(&out.hash).is_some());

    let mut data: serde_json::Value = serde_json::from_slice(base.data()).expect("JSON");
    data["event_block_height"] = i32::MAX.into();
    let far_trigger = signed_trigger(base, serde_json::to_vec(&data).expect("JSON"), base.time());
    let list = trigger_list(&fx, base);
    let mut m = Manager::new();
    assert_eq!(
        m.process_object(&far_trigger, &list, base.time(), true, &p, &lookup),
        Err(HeightOverflow)
    );
    assert!(m.objects().is_empty() && m.superblocks().iter().next().is_none());
    assert_eq!(
        m.last_object_record(&outpoint_key(&base.masternode_outpoint()))
            .expect("attempt")
            .buffer
            .count(),
        1
    );
}

#[test]
fn expired_and_deleted_parents_ignore_votes() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::from_fixture(&fx);
    let (obj, votes) = fx
        .objects
        .iter()
        .find(|(o, votes)| object::object_type(o) == TRIGGER && !votes.is_empty())
        .expect("a voted trigger");
    let hash = object::hash(obj);
    let sb = Superblock::parse(obj, &p).expect("trigger");
    let height = checked_height(checked_height(sb.height, p.superblock_cycle()), 1);
    for expired in [false, true] {
        let list = if expired {
            fx.list.clone()
        } else {
            fx.list.clone().with_height(height)
        };
        let mut m = Manager::new();
        assert!(m.process_object(obj, &list, obj.time(), true, &p, &lookup).expect("add").held);
        if expired {
            let mut set = m.superblocks().clone();
            assert_eq!(set.clean(height, &mut m, obj.time(), &p), vec![hash]);
        }
        let state = m.object(&hash).expect("held");
        assert_eq!(state.expired, expired);
        assert!(state.cached_delete);
        let out = m.process_vote(&votes[0], &fx.list, now(&fx), &p);
        assert_eq!(out.result, Ok(false));
        assert!(out.to_request.is_none());
        assert!(m.object(&hash).expect("held").file.is_empty());
        assert!(!m.accepted_vote(&vote::hash(&votes[0])));
        assert!(m.orphan(&hash).is_none());
    }
}

#[test]
fn expired_parent_without_delete_ignores_votes() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::from_fixture(&fx);
    let (obj, votes) = fx
        .objects
        .iter()
        .find(|(o, votes)| object::object_type(o) == TRIGGER && !votes.is_empty())
        .expect("a voted trigger");
    let hash = object::hash(obj);
    let mut m = Manager::new();
    assert!(m.process_object(obj, &fx.list, obj.time(), true, &p, &lookup).expect("add").held);
    let state = m.objects.get_mut(&hash).expect("held");
    state.expired = true;
    state.cached_delete = false;
    assert!(state.expired && !state.cached_delete);
    let out = m.process_vote(&votes[0], &fx.list, now(&fx), &p);
    assert_eq!(out.result, Ok(false));
    assert!(out.to_request.is_none());
    assert!(m.object(&hash).expect("held").file.is_empty());
    assert!(!m.accepted_vote(&vote::hash(&votes[0])));
}

#[test]
fn sync_queries_and_inventory_confirmation_follow_core_branches() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::from_fixture(&fx);
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let mut m = Manager::new();
    assert!(m.process_object(obj, &fx.list, now(&fx), true, &p, &lookup).expect("add").held);
    assert!(m.have_object_for_hash(&hash));
    assert!(m.have_syncable_object_for_hash(&hash));
    assert!(m.confirm_inventory_request(Inventory::Vote(vote::hash(&votes[0]))));
    assert_eq!(m.process_vote(&votes[0], &fx.list, now(&fx), &p).result, Ok(true));
    assert!(m.have_vote(&vote::hash(&votes[0])));
    assert!(!m.confirm_inventory_request(Inventory::Object(hash)));
    assert!(!m.confirm_inventory_request(Inventory::Vote(vote::hash(&votes[0]))));
    assert_eq!(m.syncable_object_invs(), vec![hash]);
    assert_eq!(m.syncable_vote_invs(&hash), vec![vote::hash(&votes[0])]);
    assert_eq!(m.get_current_votes(&hash, None, &fx.list).len(), 1);
}

#[test]
fn erased_proposals_are_forever_and_trigger_window_is_inclusive() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::from_fixture(&fx);
    let (proposal, _) = voted_proposal(&fx);
    let proposal_hash = object::hash(proposal);
    let trigger = fx
        .objects
        .iter()
        .find(|(o, _)| object::object_type(o) == TRIGGER)
        .map(|(o, _)| o)
        .expect("trigger");
    let trigger_hash = object::hash(trigger);
    let mut m = Manager::new();
    assert!(m.process_object(proposal, &fx.list, now(&fx), true, &p, &lookup).expect("add").held);
    assert!(
        m.process_object(trigger, &fx.list, trigger.time(), true, &p, &lookup).expect("add").held
    );
    assert!(m.superblocks().get(&trigger_hash).is_some());
    m.erase_object(proposal_hash, now(&fx), &p);
    assert_eq!(m.erased_expiry(&proposal_hash), Some(i64::MAX));
    assert!(m.superblocks().get(&trigger_hash).is_some());
    m.cleanup_erased(i64::MAX);
    assert!(m.is_erased(&proposal_hash));

    let mut trigger_manager = Manager::new();
    assert!(
        trigger_manager
            .process_object(trigger, &fx.list, trigger.time(), true, &p, &lookup)
            .expect("add")
            .held
    );
    trigger_manager.erase_object(trigger_hash, trigger.time(), &p);
    assert!(trigger_manager.superblocks().get(&trigger_hash).is_none());
    let expiry = trigger_manager.erased_expiry(&trigger_hash).expect("expiry");
    trigger_manager.cleanup_erased(expiry);
    assert!(trigger_manager.is_erased(&trigger_hash));
    trigger_manager.cleanup_erased(expiry.checked_add(1).expect("time"));
    assert!(!trigger_manager.is_erased(&trigger_hash));
}

#[test]
fn orphan_cache_keeps_one_vote_and_replays_it() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (obj, votes) = voted_proposal(&fx);
    let parent = object::hash(obj);
    let mut m = Manager::new();
    assert_eq!(m.process_vote(&votes[0], &fx.list, now(&fx), &p).to_request, Some(parent));
    assert_eq!(m.process_vote(&votes[1], &fx.list, now(&fx), &p).to_request, None);
    let (kept, _) = m.orphan(&parent).expect("one orphan");
    assert_eq!(vote::hash(kept), vote::hash(&votes[0]));
    m.process_object(obj, &fx.list, now(&fx), true, &p, &lookup).expect("parent");
    let state = m.object(&parent).expect("state");
    assert!(state.file.has(&vote::hash(&votes[0])));
    assert!(!state.file.has(&vote::hash(&votes[1])));
    // Core: governance/governance.cpp v24.0.2:299-301 (dashpay/dash#8112), :196-202
    assert!(m.accepted_vote(&vote::hash(&votes[0])));
    assert!(m.have_vote(&vote::hash(&votes[0])));
    assert!(m.orphan(&parent).is_none());
    let effects = m
        .updated_block_tip(
            fx.list.height(),
            &fx.list,
            &dash_governance::manager::GovClock::new(now(&fx), now(&fx)),
            true,
            &p,
            &lookup,
        )
        .expect("tip");
    assert_eq!(effects.relay_objects, vec![parent]);
    assert_eq!(effects.relay_votes, vec![vote::hash(&votes[0])]);
}

#[test]
fn orphan_cache_evicts_oldest_parent_at_capacity() {
    let fx = fixture();
    let p = params();
    let mut m = Manager::new();
    m.set_orphan_capacity_for_test(2);
    let votes: Vec<_> = fx.objects.iter().filter_map(|(_, votes)| votes.first()).take(3).collect();
    assert_eq!(votes.len(), 3);
    let parents: Vec<_> =
        votes.iter().map(|vote| object::txid_bytes(&vote.parent_hash())).collect();
    for vote in &votes {
        assert!(m.process_vote(vote, &fx.list, now(&fx), &p).result.is_err());
    }
    assert_eq!(m.orphan_count(), 2);
    assert!(m.orphan(&parents[0]).is_none());
    assert!(m.orphan(&parents[1]).is_some());
    assert!(m.orphan(&parents[2]).is_some());
    assert_eq!(m.process_vote(votes[0], &fx.list, now(&fx), &p).to_request, Some(parents[0]));
}

#[test]
fn m7_clock_domains_cover_arrival_cleanup_rate_validity_and_deletion() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (obj, votes) = voted_proposal(&fx);
    let base = votes[0].time();
    let mut m = Manager::new();
    let clock = dash_governance::manager::GovClock::new(base, base);
    m.process_vote_at(&votes[0], &fx.list, &clock, &p);
    assert_eq!(m.orphan(&object::hash(obj)).expect("orphan").1, checked_time(base, 600));
    let arrival = dash_governance::manager::GovClock::new(base, checked_time(base, 700));
    m.process_object_at(obj, &fx.list, &arrival, true, &p, &lookup).expect("arrival");
    assert!(m.object(&object::hash(obj)).expect("state").file.is_empty());

    let mut kept = Manager::new();
    kept.process_vote_at(&votes[0], &fx.list, &clock, &p);
    assert!(kept.cleanup_orphans_at(&arrival).is_empty());
    let apply = dash_governance::manager::GovClock::new(checked_time(base, 700), base);
    kept.process_object_at(obj, &fx.list, &apply, true, &p, &lookup).expect("arrival");
    assert!(kept.object(&object::hash(obj)).expect("state").file.has(&vote::hash(&votes[0])));

    let mut validity = Manager::new();
    let invalid_clock = dash_governance::manager::GovClock::new(base, base - 3_601);
    assert!(validity.process_vote_at(&votes[0], &fx.list, &invalid_clock, &p).result.is_err());

    let trigger = fx
        .objects
        .iter()
        .find(|(o, _)| object::object_type(o) == TRIGGER)
        .expect("trigger")
        .0
        .clone();
    let sb = Superblock::parse(&trigger, &p).expect("superblock");
    let mut rate = Manager::new();
    let rate_clock = dash_governance::manager::GovClock::new(
        checked_time(trigger.time(), 7_200),
        trigger.time(),
    );
    rate.updated_block_tip(sb.height, &fx.list, &rate_clock, true, &p, &lookup).expect("rate tip");
    let rate_out =
        rate.process_object_at(&trigger, &fx.list, &rate_clock, true, &p, &lookup).expect("rate");
    assert!(!rate.object(&rate_out.hash).expect("rate state").cached_delete);
    let mut deleted = Manager::new();
    deleted
        .updated_block_tip(
            checked_height(checked_height(sb.height, p.superblock_cycle()), 1),
            &fx.list,
            &clock,
            true,
            &p,
            &lookup,
        )
        .expect("tip");
    deleted.process_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("trigger");
    let before =
        dash_governance::manager::GovClock::new(checked_time(clock.now, 599), clock.adjusted);
    assert!(deleted.check_and_remove(&fx.list, &before, true, &p).forget_requests.is_empty());
    let after =
        dash_governance::manager::GovClock::new(checked_time(clock.now, 600), clock.adjusted);
    assert_eq!(deleted.check_and_remove(&fx.list, &after, true, &p).forget_requests.len(), 1);
}

#[test]
fn cached_height_is_loaded_zero_and_tip_owned() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let trigger = fx
        .objects
        .iter()
        .find(|(o, _)| object::object_type(o) == TRIGGER)
        .expect("trigger")
        .0
        .clone();
    let sb = Superblock::parse(&trigger, &p).expect("superblock");
    let mut m = Manager::new();
    m.process_object_at(
        &trigger,
        &fx.list,
        &dash_governance::manager::GovClock::new(trigger.time(), trigger.time()),
        true,
        &p,
        &lookup,
    )
    .expect("trigger");
    let state = m.export_state();
    m.import_state(state);
    m.init_on_load(&dash_governance::manager::GovClock::default(), &p);
    assert_eq!(m.cached_height(), 0);
    let tip_height = sb.height;
    m.updated_block_tip(
        tip_height,
        &fx.list,
        &dash_governance::manager::GovClock::default(),
        true,
        &p,
        &lookup,
    )
    .expect("tip");
    assert_eq!(m.cached_height(), tip_height);
    let list = fx.list.clone().with_height(tip_height);
    let mut fresh = Manager::new();
    let out =
        fresh.process_object(&trigger, &list, trigger.time(), true, &p, &lookup).expect("trigger");
    assert!(out.held);
    assert!(!fresh.object(&out.hash).expect("state").cached_delete);
    assert_eq!(fresh.cached_height(), 0);
}

#[test]
fn clean_effects_cover_sentinels_expiry_and_deletion() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let trigger = fx
        .objects
        .iter()
        .find(|(o, _)| object::object_type(o) == TRIGGER)
        .expect("trigger")
        .0
        .clone();
    let sb = Superblock::parse(&trigger, &p).expect("superblock");
    let mut m = Manager::new();
    m.updated_block_tip(
        checked_height(checked_height(sb.height, p.superblock_cycle()), 1),
        &fx.list,
        &dash_governance::manager::GovClock::default(),
        true,
        &p,
        &lookup,
    )
    .expect("tip");
    let expired_list = fx
        .list
        .clone()
        .with_height(checked_height(checked_height(sb.height, p.superblock_cycle()), 1));
    let out = m
        .process_object(&trigger, &expired_list, trigger.time(), true, &p, &lookup)
        .expect("trigger");
    assert!(m.object(&out.hash).expect("state").cached_delete);
    let effects = m.check_and_remove(
        &fx.list,
        &dash_governance::manager::GovClock::new(trigger.time() + 599, trigger.time()),
        true,
        &p,
    );
    assert!(effects.relay_objects.is_empty() && effects.relay_votes.is_empty());
    assert!(effects.forget_requests.is_empty());
    assert!(effects.sync_bumps.is_empty());
    let effects = m.check_and_remove(
        &fx.list,
        &dash_governance::manager::GovClock::new(trigger.time() + 600, trigger.time()),
        true,
        &p,
    );
    assert_eq!(effects.forget_requests, vec![out.hash]);
    assert_eq!(effects.meta, vec![MetaOp::RemoveObject(out.hash)]);
    assert!(m.is_erased(&out.hash));
    assert!(!m.have_object_for_fetch(&out.hash));
    assert!(
        !m.process_object(&trigger, &fx.list, trigger.time(), true, &p, &lookup)
            .expect("seen")
            .held
    );
}

fn proposal_data_with_payment(base: &GovObject, address: String, end_epoch: i64) -> Vec<u8> {
    let mut data: Value = serde_json::from_slice(base.data()).expect("proposal JSON");
    data["payment_address"] = Value::String(address);
    data["end_epoch"] = Value::Number(end_epoch.into());
    serde_json::to_vec(&data).expect("proposal JSON")
}

#[test]
fn proposal_expiry_allows_p2sh_and_marks_expired_payments() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (base, _) = voted_proposal(&fx);
    let clock = dash_governance::manager::GovClock::new(now(&fx), now(&fx));
    let p2pkh = Destination::PubKeyHash([7; 20]).encode(p.chain());
    let p2sh = Destination::ScriptHash([8; 20]).encode(p.chain());
    let check = |address: String, end_epoch: i64| {
        let data = proposal_data_with_payment(base, address, end_epoch);
        let mut manager = Manager::new();
        manager.process_object(base, &fx.list, clock.adjusted, true, &p, &lookup).expect("object");
        let hash = object::hash(base);
        let replacement = GovObject::new(
            base.hash_parent(),
            base.revision(),
            base.time(),
            base.collateral_hash(),
            data,
            base.object_type(),
            base.masternode_outpoint(),
            Vec::new(),
        );
        manager.object_data.insert(hash, replacement);
        let _out = manager.check_and_remove(&fx.list, &clock, true, &p);
        let state = manager.object(&hash).expect("object");
        state.cached_delete
    };
    assert!(!check(p2sh.clone(), checked_time(clock.adjusted, 3_600)));
    assert!(check(p2sh, checked_time(clock.adjusted, -1)));
    assert!(!check(p2pkh.clone(), checked_time(clock.adjusted, 3_600)));
    assert!(check(p2pkh, checked_time(clock.adjusted, -1)));
}

#[test]
fn check_and_remove_refreshes_local_validity_for_dirty_objects() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (base, _) = voted_proposal(&fx);
    let mut manager = Manager::new();
    manager.process_object(base, &fx.list, now(&fx), true, &p, &lookup).expect("object");
    let hash = object::hash(base);
    let clock = GovClock::new(now(&fx), now(&fx));
    // Core starts `fCachedLocalValidity` false (governance/object.h:154).
    assert!(!manager.object(&hash).expect("object").cached_local_validity());
    manager.objects.get_mut(&hash).expect("state").dirty = true;
    manager.check_and_remove(&fx.list, &clock, true, &p);
    assert!(manager.object(&hash).expect("object").cached_local_validity());
    manager.object_data.insert(
        hash,
        GovObject::new(
            base.hash_parent(),
            base.revision(),
            base.time(),
            base.collateral_hash(),
            b"not-json".to_vec(),
            Numeric::from_base(PROPOSAL),
            base.masternode_outpoint(),
            Vec::new(),
        ),
    );
    manager.objects.get_mut(&hash).expect("state").dirty = true;
    manager.check_and_remove(&fx.list, &clock, true, &p);
    assert!(!manager.object(&hash).expect("object").cached_local_validity());
}

#[test]
fn mainnet_executed_lifetime_uses_cycle_and_valid_uses_capped_cycle() {
    let main = common::params_of(Network::Main);
    let data = format!(
        "{{\"event_block_height\": 100000, \"payment_addresses\": \"XwyHjrAVXFSs2uHKw5BB2U31m8D41izE24\", \"payment_amounts\": \"1\", \"proposal_hashes\": \"{}\", \"type\":2}}",
        "11".repeat(32)
    );
    let trigger = GovObject::new(
        TxHash::from_lendian([0; 32]),
        1,
        0,
        TxHash::from_lendian([0; 32]),
        data.into_bytes(),
        Numeric::from_base(TRIGGER),
        dash_primitives::OutPoint::new(TxHash::from_lendian([0; 32]), u32::MAX),
        vec![],
    );
    let parsed = Superblock::parse(&trigger, &main).expect("superblock");
    let cycle = main.superblock_cycle();
    let mut executed = parsed.clone();
    executed.status = Status::Executed;
    assert_eq!(executed.is_expired(checked_height(parsed.height, cycle), &main), Some(false));
    let mut valid = parsed;
    valid.status = Status::Valid;
    let capped = cycle.min(576);
    assert_eq!(valid.is_expired(checked_height(valid.height, capped), &main), Some(false));
    assert_eq!(
        valid.is_expired(checked_height(checked_height(valid.height, capped), 1), &main),
        Some(true)
    );
}

#[test]
fn r10_key_changes_and_removal_invalidate_votes() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (proposal, proposal_votes) = fx
        .objects
        .iter()
        .find_map(|(obj, votes)| {
            let mut probe = Manager::new();
            probe.process_object(obj, &fx.list, now(&fx), true, &p, &lookup).ok()?;
            votes
                .iter()
                .rev()
                .find(|v| {
                    v.signal().to_base() == vote::SIGNAL_FUNDING as u32
                        && probe.process_vote(v, &fx.list, now(&fx), &p).result == Ok(true)
                })
                .map(|v| (obj, std::slice::from_ref(v)))
        })
        .expect("accepted voting vote");
    let voting_vote = &proposal_votes[0];
    let voting_target =
        fx.list.get_by_collateral(&voting_vote.masternode_outpoint()).expect("voting mn");
    let mut voting_list = fx.list.clone();
    let mut cold = (*voting_target.cold).clone();
    cold.key_id_voting = KeyId::from([0xfe; 20]);
    voting_list
        .update(&voting_target.pro_tx_hash, voting_target.hot, Arc::new(testkit::fresh(cold)))
        .expect("voting update");

    let trigger_base = fx
        .objects
        .iter()
        .find(|(obj, _)| object::object_type(obj) == TRIGGER)
        .map(|(obj, _)| obj)
        .expect("trigger");
    let trigger_list = trigger_list(&fx, trigger_base);
    let trigger = signed_trigger(trigger_base, trigger_base.data().clone(), trigger_base.time());
    let operator_vote_unsigned = GovVote::new(
        trigger.masternode_outpoint(),
        TxHash::from_lendian(object::hash(&trigger)),
        VoteOutcome::new(vote::OUTCOME_YES as u32),
        VoteSignal::new(vote::SIGNAL_VALID as u32),
        trigger.time(),
        Vec::new(),
    );
    let operator_key = BlsSecretKey::<BlsScIetf>::from_ikm(&[42; 32]).expect("fixed key");
    let operator_vote = GovVote::new(
        operator_vote_unsigned.masternode_outpoint(),
        operator_vote_unsigned.parent_hash(),
        operator_vote_unsigned.outcome(),
        operator_vote_unsigned.signal(),
        operator_vote_unsigned.time(),
        operator_key
            .sign_with(&vote::signature_hash(&operator_vote_unsigned), BlsSigId::Basic)
            .to_bytes()
            .to_vec(),
    );
    let operator_target =
        trigger_list.get_by_collateral(&operator_vote.masternode_outpoint()).expect("operator mn");
    let mut operator_list = trigger_list.clone();
    let mut cold = (*operator_target.cold).clone();
    cold.pub_key_operator = testkit::operator_key(91, dash_pkc::bls::BlsSchemeKind::Basic);
    operator_list
        .update(&operator_target.pro_tx_hash, operator_target.hot, Arc::new(testkit::fresh(cold)))
        .expect("operator update");
    let mut removed_list = trigger_list.clone();
    removed_list.remove(&operator_target.pro_tx_hash).expect("remove");

    let check =
        |name: &str, obj: &GovObject, selected: &GovVote, old: &MnList, list: &MnList, at: i64| {
            let mut m = Manager::new();
            let added = m.process_object(obj, old, at, true, &p, &lookup).expect(name);
            assert!(added.held, "{name}: {added:?}");
            assert_eq!(m.process_vote(selected, old, at, &p).result, Ok(true), "{name} accepted");
            m.remove_invalid_votes(old, &dash_governance::manager::GovClock::new(at, at), &p, true);
            let effects = m
                .updated_block_tip(
                    list.height(),
                    list,
                    &dash_governance::manager::GovClock::new(at, at),
                    true,
                    &p,
                    &lookup,
                )
                .expect("tip");
            assert!(effects.forget_requests.contains(&vote::hash(selected)), "{name}");
            assert!(
                !m.object(&object::hash(obj)).expect("state").file.has(&vote::hash(selected)),
                "{name}"
            );
        };
    check("voting", proposal, voting_vote, &fx.list, &voting_list, now(&fx));
    check("operator", &trigger, &operator_vote, &trigger_list, &operator_list, trigger.time());
    check("removal", &trigger, &operator_vote, &trigger_list, &removed_list, trigger.time());
}

#[test]
fn operator_key_change_keeps_funding_vote_at_supplied_clock() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (proposal, vote) = fx
        .objects
        .iter()
        .find_map(|(obj, votes)| {
            let selected =
                votes.iter().find(|v| v.signal().to_base() == vote::SIGNAL_FUNDING as u32)?;
            Some((obj, selected))
        })
        .expect("funding vote");
    let target = fx.list.get_by_collateral(&vote.masternode_outpoint()).expect("masternode");
    let mut changed = fx.list.clone();
    let mut cold = (*target.cold).clone();
    cold.pub_key_operator = testkit::operator_key(123, BlsSchemeKind::Basic);
    changed
        .update(&target.pro_tx_hash, target.hot, Arc::new(testkit::fresh(cold)))
        .expect("operator update");
    let at = now(&fx);
    let mut manager = Manager::new();
    manager.process_object(proposal, &fx.list, at, true, &p, &lookup).expect("object");
    assert_eq!(manager.process_vote(vote, &fx.list, at, &p).result, Ok(true));
    manager.remove_invalid_votes(
        &fx.list,
        &dash_governance::manager::GovClock::new(at, at),
        &p,
        true,
    );
    let effects = manager.remove_invalid_votes(
        &changed,
        &dash_governance::manager::GovClock::new(at, at),
        &p,
        true,
    );
    assert!(effects.forget_requests.is_empty());
    assert!(manager.accepted_vote(&vote::hash(vote)));
}

/// A masternode whose voting key changed is matched across the two lists by its registration,
/// whatever its internal id in the new list.
// Core: governance/governance.cpp v24.0.2:1184-1234, :1197-1215 (dashpay/dash#8100)
#[test]
fn remove_invalid_votes_matches_masternodes_across_internal_ids() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (proposal, vote) = fx
        .objects
        .iter()
        .find_map(|(obj, votes)| {
            let mut probe = Manager::new();
            probe.process_object(obj, &fx.list, now(&fx), true, &p, &lookup).ok()?;
            votes
                .iter()
                .find(|v| {
                    v.signal().to_base() == vote::SIGNAL_FUNDING as u32
                        && probe.process_vote(v, &fx.list, now(&fx), &p).result == Ok(true)
                })
                .map(|v| (obj, v))
        })
        .expect("accepted funding vote");
    let target = fx.list.get_by_collateral(&vote.masternode_outpoint()).expect("masternode");
    let unused_id = fx.list.iter().map(|dmn| dmn.internal_id).max().expect("masternodes") + 1;
    let mut cold = (*target.cold).clone();
    cold.key_id_voting = KeyId::from([0xfd; 20]);
    let mut moved = (**target).clone();
    moved.internal_id = unused_id;
    moved.cold = Arc::new(testkit::fresh(cold));
    let mut changed = fx.list.clone();
    changed.remove(&target.pro_tx_hash).expect("remove");
    changed.try_add(Arc::new(moved), false).expect("add");
    assert!(changed.get_by_internal_id(target.internal_id).is_none());

    let at = GovClock::new(now(&fx), now(&fx));
    let mut m = Manager::new();
    m.process_object(proposal, &fx.list, at.now, true, &p, &lookup).expect("object");
    assert_eq!(m.process_vote(vote, &fx.list, at.now, &p).result, Ok(true));
    m.remove_invalid_votes(&fx.list, &at, &p, true);
    let effects = m.remove_invalid_votes(&changed, &at, &p, true);
    assert_eq!(effects.forget_requests, vec![vote::hash(vote)]);
    assert!(!m.accepted_vote(&vote::hash(vote)));
}

#[test]
fn dip0003_enforcement_gates_tip_vote_cleanup() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (proposal, proposal_votes) = fx
        .objects
        .iter()
        .find_map(|(obj, votes)| {
            let mut probe = Manager::new();
            probe.process_object(obj, &fx.list, now(&fx), true, &p, &lookup).ok()?;
            votes
                .iter()
                .find(|v| {
                    v.signal().to_base() == vote::SIGNAL_FUNDING as u32
                        && probe.process_vote(v, &fx.list, now(&fx), &p).result == Ok(true)
                })
                .map(|v| (obj, v))
        })
        .expect("accepted voting vote");
    let voting_target =
        fx.list.get_by_collateral(&proposal_votes.masternode_outpoint()).expect("voting mn");
    let old_voting_key = voting_target.cold.key_id_voting;
    let mut changed_list = fx.list.clone();
    let mut cold = (*voting_target.cold).clone();
    cold.key_id_voting = KeyId::from([0xfd; 20]);
    changed_list
        .update(&voting_target.pro_tx_hash, voting_target.hot, Arc::new(testkit::fresh(cold)))
        .expect("voting update");

    let mut m = Manager::new();
    let hash = object::hash(proposal);
    m.process_object(proposal, &fx.list, now(&fx), true, &p, &lookup).expect("object");
    assert_eq!(m.process_vote(proposal_votes, &fx.list, now(&fx), &p).result, Ok(true));
    m.remove_invalid_votes(
        &fx.list,
        &dash_governance::manager::GovClock::new(now(&fx), now(&fx)),
        &p,
        true,
    );
    let enforcement = i32::try_from(p.chain().consensus.dip0003_enforcement.0.to_u32())
        .expect("fixture enforcement height");
    let below = enforcement.checked_sub(1).expect("positive enforcement height");
    assert!(!p.dip0003_enforced(-1));
    assert!(!p.dip0003_enforced(below));
    assert!(p.dip0003_enforced(enforcement));

    m.updated_block_tip(
        below,
        &changed_list,
        &dash_governance::manager::GovClock::new(now(&fx), now(&fx)),
        true,
        &p,
        &lookup,
    )
    .expect("below enforcement tip");
    assert!(m.object(&hash).expect("object").file.has(&vote::hash(proposal_votes)));
    assert_eq!(
        m.last_mn_list
            .as_ref()
            .and_then(|list| list.get_by_collateral(&proposal_votes.masternode_outpoint()))
            .expect("last list entry")
            .cold
            .key_id_voting,
        old_voting_key
    );

    let effects = m
        .updated_block_tip(
            enforcement,
            &changed_list,
            &dash_governance::manager::GovClock::new(now(&fx), now(&fx)),
            true,
            &p,
            &lookup,
        )
        .expect("enforcement tip");
    assert_eq!(effects.forget_requests, vec![vote::hash(proposal_votes)]);
    assert!(!m.object(&hash).expect("object").file.has(&vote::hash(proposal_votes)));
}

#[test]
fn state_round_trip_rebuilds_triggers_and_orphans() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (obj, votes) = voted_proposal(&fx);
    let (trigger, trigger_votes) = fx
        .objects
        .iter()
        .find(|(object, votes)| object::object_type(object) == TRIGGER && !votes.is_empty())
        .expect("trigger");
    let mut m = Manager::new();
    m.process_vote(&votes[0], &fx.list, now(&fx), &p);
    m.process_object(trigger, &fx.list, trigger.time(), true, &p, &lookup).expect("trigger");
    assert_eq!(m.process_vote(&trigger_votes[0], &fx.list, trigger.time(), &p).result, Ok(true));
    let accepted = vote::hash(&trigger_votes[0]);
    let state = m.export_state();
    let mut restored = Manager::new();
    restored.import_state(state);
    assert_eq!(restored.orphan_count(), 1);
    assert!(restored.orphan(&object::hash(obj)).is_some());
    assert!(restored.object(&object::hash(trigger)).is_some());
    assert!(!restored.accepted_vote(&accepted));
    let before = m.superblocks().iter().count();
    assert_eq!(restored.superblocks().iter().count(), 0);
    restored.init_on_load(&dash_governance::manager::GovClock::default(), &p);
    assert_eq!(restored.orphan_count(), 1);
    assert!(restored.accepted_vote(&accepted));
    assert_eq!(restored.superblocks().iter().count(), before);
}

#[test]
fn out_of_range_clocks_and_heights_do_not_panic() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (obj, votes) = voted_proposal(&fx);
    let mut m = Manager::new();
    let clock = dash_governance::manager::GovClock::new(i64::MAX, i64::MIN);
    assert!(m.process_vote_at(&votes[0], &fx.list, &clock, &p).result.is_err());
    assert!(m.process_object_at(obj, &fx.list, &clock, true, &p, &lookup).expect("object").held);
    assert!(m.updated_block_tip(i32::MAX, &fx.list, &clock, true, &p, &lookup).is_ok());
    assert!(m.updated_block_tip(i32::MIN, &fx.list, &clock, true, &p, &lookup).is_ok());
}

#[test]
fn object_state_starts_without_local_validity() {
    let state = dash_governance::tally::ObjectState::new(PROPOSAL, [7; 32]);
    assert!(!state.cached_local_validity());
}

fn parent_of(i: u32) -> [u8; 32] {
    let mut parent = [0; 32];
    parent[..4].copy_from_slice(&i.to_be_bytes());
    parent
}

#[test]
fn orphan_cache_scales_to_200k_parents() {
    const COUNT: u32 = 200_000;
    let fx = fixture();
    let (_, votes) = voted_proposal(&fx);
    let started = std::time::Instant::now();
    let mut m = Manager::new();
    for i in 0..COUNT {
        let expiry = if i % 2 == 0 {
            0
        } else {
            i64::MAX
        };
        m.insert_orphan(parent_of(i), &votes[0], expiry);
    }
    assert_eq!(m.orphan_count(), COUNT as usize);
    assert_eq!(m.orphan_cache.len(), COUNT as usize);
    assert_eq!(m.cleanup_orphans(1).len(), (COUNT / 2) as usize);
    for i in (1..COUNT).step_by(2) {
        m.remove_orphan_parent(&parent_of(i));
    }
    assert_eq!(m.orphan_count(), 0);
    assert!(m.orphan_cache.is_empty());
    eprintln!("orphan_cache_scales_to_200k_parents: {:?}", started.elapsed());
}

#[test]
fn restored_orphans_keep_insertion_order_for_eviction() {
    let fx = fixture();
    let (_, votes) = voted_proposal(&fx);
    let mut m = Manager::new();
    m.set_orphan_capacity_for_test(3);
    // Insertion order differs from hash order: the oldest is neither the smallest nor the
    // largest parent.
    let (oldest, largest, smallest) = ([2; 32], [3; 32], [1; 32]);
    for parent in [oldest, largest, smallest] {
        m.insert_orphan(parent, &votes[0], i64::MAX);
    }
    let state = m.export_state();
    assert_eq!(
        state.orphans.iter().map(|(parent, _)| *parent).collect::<Vec<_>>(),
        vec![smallest, largest, oldest]
    );
    m.import_state(state);
    assert_eq!(m.orphan_cache.max_size(), 3);
    m.insert_orphan([9; 32], &votes[0], i64::MAX);
    assert!(m.orphan(&oldest).is_none());
    assert!(m.orphan(&largest).is_some());
    assert!(m.orphan(&smallest).is_some());
    assert!(m.orphan(&[9; 32]).is_some());
    assert_eq!(m.orphan_count(), 3);
}

#[test]
fn legacy_process_object_uses_list_height_for_trigger_expiry() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::from_fixture(&fx);
    let trigger = fx
        .objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("trigger");
    let sb = Superblock::parse(trigger, &p).expect("trigger");
    let expired_height = checked_height(checked_height(sb.height, p.superblock_cycle()), 1);
    let clock = GovClock::new(trigger.time(), trigger.time());
    let at = |cached: i32, list_height: i32, legacy: bool| {
        let mut m = Manager::new();
        m.updated_block_tip(cached, &fx.list, &clock, true, &p, &lookup).expect("tip");
        assert_eq!(m.cached_height(), cached);
        let list = fx.list.clone().with_height(list_height);
        let out = if legacy {
            m.process_object(trigger, &list, trigger.time(), true, &p, &lookup)
        } else {
            m.process_object_at(trigger, &list, &clock, true, &p, &lookup)
        }
        .expect("trigger");
        m.object(&out.hash).expect("held").cached_delete
    };
    // The legacy path decides expiry from `list.height()`, the runtime path from the cached tip.
    assert!(at(sb.height, expired_height, true));
    assert!(!at(sb.height, expired_height, false));
    assert!(!at(expired_height, sb.height, true));
    assert!(at(expired_height, sb.height, false));
}

/// `LoadCache` adds the stored triggers at the active chain's height: a trigger is kept at its
/// own superblock height and marked for deletion at wall time once that height is past its
/// expiry, and the cached height is the tip's after the load.
// Core: governance/governance.cpp v24.0.2:100-112 (dashpay/dash#8097), :1022-1025
#[test]
fn load_cache_adds_triggers_at_the_tip_height() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::from_fixture(&fx);
    let trigger = fx
        .objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("trigger");
    let hash = object::hash(trigger);
    let sb = Superblock::parse(trigger, &p).expect("trigger");
    let expired_height = checked_height(checked_height(sb.height, p.superblock_cycle()), 1);
    let clock = GovClock::new(trigger.time(), trigger.time());
    let mut m = Manager::new();
    m.updated_block_tip(sb.height, &fx.list, &clock, true, &p, &lookup).expect("tip");
    let out = m.process_object_at(trigger, &fx.list, &clock, true, &p, &lookup).expect("trigger");
    assert!(out.held && !m.object(&hash).expect("held").cached_delete);
    let load = GovClock::new(checked_time(clock.now, 50), checked_time(clock.adjusted, 90));
    for (tip, deleted) in [(sb.height, false), (expired_height, true)] {
        let mut restored = Manager::new();
        let effects = restored.load_cache(m.export_state(), tip, &fx.list, &load, false, &p);
        assert!(effects.relay_objects.is_empty() && effects.forget_requests.is_empty());
        assert_eq!(restored.cached_height(), tip);
        assert!(restored.superblocks().get(&hash).is_some(), "tip {tip}");
        let state = restored.object(&hash).expect("loaded");
        assert_eq!(state.cached_delete, deleted, "tip {tip}");
        let deletion_time = if deleted {
            load.now
        } else {
            0
        };
        assert_eq!(state.deletion_time, deletion_time, "tip {tip}");
        assert_eq!(restored.trigger_view().cached_height, tip);
    }
}

#[test]
fn init_on_load_marks_refused_triggers_for_deletion_at_wall_time() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let base = fx
        .objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("trigger");
    let list = trigger_list(&fx, base);
    let mut m = Manager::new();
    let refused = signed_trigger(base, b"{}".to_vec(), base.time());
    let refused_hash =
        m.process_object(&refused, &list, base.time(), true, &p, &lookup).expect("refused").hash;
    let kept_hash =
        m.process_object(base, &fx.list, base.time(), true, &p, &lookup).expect("kept").hash;
    let mut state = m.export_state();
    let stored = &mut state.objects.get_mut(&refused_hash).expect("stored").1;
    assert!(stored.cached_delete);
    stored.cached_delete = false;
    stored.deletion_time = 0;
    let mut restored = Manager::new();
    restored.import_state(state);
    let load = GovClock::new(checked_time(base.time(), 50), checked_time(base.time(), 90));
    restored.init_on_load(&load, &p);
    assert_eq!(restored.cached_height(), 0);
    let state = restored.object(&refused_hash).expect("refused state");
    assert!(state.cached_delete);
    assert_eq!(state.deletion_time, load.now);
    assert!(restored.superblocks().get(&refused_hash).is_none());
    assert!(restored.superblocks().get(&kept_hash).is_some());
}

/// A manager at the fixture trigger's tip, plus the trigger and its tip height.
fn trigger_relay_setup(fx: &Fixture) -> (Manager, GovObject) {
    let p = params();
    let lookup = Collaterals::new(fx, 6);
    let trigger = fx
        .objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("trigger")
        .clone();
    let sb = Superblock::parse(&trigger, &p).expect("superblock");
    let mut m = Manager::new();
    m.updated_block_tip(sb.height, &fx.list, &GovClock::default(), true, &p, &lookup).expect("tip");
    (m, trigger)
}

fn postponed_relays(m: &mut Manager, fx: &Fixture, adjusted: i64, synced: bool) -> Vec<[u8; 32]> {
    let p = params();
    let lookup = Collaterals::new(fx, 6);
    let clock = GovClock::new(adjusted, adjusted);
    m.check_postponed_objects_at(&fx.list, &clock, synced, &p, &lookup).expect("postponed");
    m.take_relay_queue().objects
}

#[test]
fn near_future_trigger_is_reannounced_once_after_aging() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (mut m, trigger) = trigger_relay_setup(&fx);
    let t = trigger.time();
    // Created 30 s inside the last minute before the future limit, by the wall clock.
    let arrival = GovClock::new(checked_time(t, -3570), checked_time(t, -3570));
    let out = m.process_object_at(&trigger, &fx.list, &arrival, true, &p, &lookup).expect("add");
    assert!(out.held && !m.object(&out.hash).expect("held").cached_delete);
    assert_eq!(m.additional_relay.iter().copied().collect::<Vec<_>>(), vec![out.hash]);
    assert_eq!(m.take_relay_queue().objects, vec![out.hash]);
    // Unsynced managers return before the additional-relay pass.
    assert!(postponed_relays(&mut m, &fx, checked_time(t, -3540), false).is_empty());
    assert!(m.additional_relay.contains(&out.hash));
    // Still inside the propagation margin: kept, not relayed.
    assert!(postponed_relays(&mut m, &fx, checked_time(t, -3541), true).is_empty());
    assert!(m.additional_relay.contains(&out.hash));
    assert_eq!(postponed_relays(&mut m, &fx, checked_time(t, -3540), true), vec![out.hash]);
    assert!(m.additional_relay.is_empty());
    assert!(postponed_relays(&mut m, &fx, checked_time(t, -3540), true).is_empty());

    // A trigger arriving 60 s before the limit is not scheduled.
    let (mut edge, _) = trigger_relay_setup(&fx);
    let at_edge = GovClock::new(checked_time(t, -3540), checked_time(t, -3540));
    edge.process_object_at(&trigger, &fx.list, &at_edge, true, &p, &lookup).expect("add");
    assert!(edge.additional_relay.is_empty());
}

#[test]
fn trigger_relay_schedules_by_wall_time_and_releases_by_adjusted_time() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);

    // Wall clock inside the margin, adjusted clock outside it: scheduled.
    let (mut wall, trigger) = trigger_relay_setup(&fx);
    let ts = trigger.time();
    let by_wall = GovClock::new(checked_time(ts, -3570), ts);
    let hash =
        wall.process_object_at(&trigger, &fx.list, &by_wall, true, &p, &lookup).expect("add").hash;
    assert_eq!(wall.additional_relay.iter().copied().collect::<Vec<_>>(), vec![hash]);
    wall.take_relay_queue();

    // Wall clock outside the margin, adjusted clock inside it: not scheduled.
    let (mut adjusted, _) = trigger_relay_setup(&fx);
    let by_adjusted = GovClock::new(ts, checked_time(ts, -3570));
    adjusted.process_object_at(&trigger, &fx.list, &by_adjusted, true, &p, &lookup).expect("add");
    assert!(adjusted.additional_relay.is_empty());

    // Release: a ready wall clock with a not-ready adjusted clock keeps the entry.
    let not_ready = GovClock::new(checked_time(ts, -3540), checked_time(ts, -3541));
    wall.check_postponed_objects_at(&fx.list, &not_ready, true, &p, &lookup).expect("postponed");
    assert!(wall.take_relay_queue().objects.is_empty());
    assert!(wall.additional_relay.contains(&hash));
    // A not-ready wall clock with a ready adjusted clock relays it.
    let ready = GovClock::new(checked_time(ts, -3570), checked_time(ts, -3540));
    wall.check_postponed_objects_at(&fx.list, &ready, true, &p, &lookup).expect("postponed");
    assert_eq!(wall.take_relay_queue().objects, vec![hash]);
    assert!(wall.additional_relay.is_empty());
}

#[test]
fn scheduled_trigger_outside_the_window_is_dropped_without_relay() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (mut m, trigger) = trigger_relay_setup(&fx);
    let t = trigger.time();
    let arrival = GovClock::new(checked_time(t, -3570), checked_time(t, -3570));
    let hash =
        m.process_object_at(&trigger, &fx.list, &arrival, true, &p, &lookup).expect("add").hash;
    m.take_relay_queue();
    let cycle = i64::from(p.superblock_cycle()) * p.target_spacing();
    let old = checked_time(t, cycle.checked_mul(2).expect("cycle") + 1);
    assert!(postponed_relays(&mut m, &fx, old, true).is_empty());
    assert!(m.additional_relay.is_empty());
    assert!(m.object(&hash).is_some());

    // Both window edges are inclusive.
    let (mut oldest, _) = trigger_relay_setup(&fx);
    oldest.process_object_at(&trigger, &fx.list, &arrival, true, &p, &lookup).expect("add");
    oldest.take_relay_queue();
    let at_past_edge = checked_time(t, cycle.checked_mul(2).expect("cycle"));
    assert_eq!(postponed_relays(&mut oldest, &fx, at_past_edge, true), vec![hash]);

    let (mut ahead, _) = trigger_relay_setup(&fx);
    ahead.process_object_at(&trigger, &fx.list, &arrival, true, &p, &lookup).expect("add");
    ahead.take_relay_queue();
    assert!(postponed_relays(&mut ahead, &fx, checked_time(t, -3600), true).is_empty());
    assert!(ahead.additional_relay.contains(&hash));
    assert!(postponed_relays(&mut ahead, &fx, checked_time(t, -3601), true).is_empty());
    assert!(ahead.additional_relay.is_empty());
}

#[test]
fn only_kept_triggers_are_scheduled_for_relay() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (proposal, _) = voted_proposal(&fx);
    let mut m = Manager::new();
    let early = checked_time(proposal.time(), -3570);
    let out = m
        .process_object_at(proposal, &fx.list, &GovClock::new(early, early), true, &p, &lookup)
        .expect("proposal");
    assert!(out.held);
    assert!(m.additional_relay.is_empty());

    let (base, trigger_list_for) = {
        let (_, trigger) = trigger_relay_setup(&fx);
        let list = trigger_list(&fx, &trigger);
        (trigger, list)
    };
    let refused = signed_trigger(&base, b"{}".to_vec(), base.time());
    let arrival = GovClock::new(checked_time(base.time(), -3570), checked_time(base.time(), -3570));
    let (mut triggers, _) = trigger_relay_setup(&fx);
    let out = triggers
        .process_object_at(&refused, &trigger_list_for, &arrival, true, &p, &lookup)
        .expect("refused");
    assert!(out.held && triggers.object(&out.hash).expect("held").cached_delete);
    assert!(triggers.additional_relay.is_empty());
}

#[test]
fn erased_scheduled_trigger_is_dropped_and_set_is_not_stored() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (mut m, trigger) = trigger_relay_setup(&fx);
    let t = trigger.time();
    let arrival = GovClock::new(checked_time(t, -3570), checked_time(t, -3570));
    let hash =
        m.process_object_at(&trigger, &fx.list, &arrival, true, &p, &lookup).expect("add").hash;
    m.take_relay_queue();
    assert!(m.additional_relay.contains(&hash));

    let mut restored = Manager::new();
    restored.import_state(m.export_state());
    assert!(restored.additional_relay.is_empty());
    let mut reloaded = m.clone();
    reloaded.import_state(m.export_state());
    assert!(reloaded.additional_relay.is_empty());

    m.erase_object(hash, arrival.now, &p);
    assert!(m.object(&hash).is_none());
    assert!(m.additional_relay.contains(&hash));
    assert!(postponed_relays(&mut m, &fx, checked_time(t, -3540), true).is_empty());
    assert!(m.additional_relay.is_empty());
}

fn synthetic_list(fx: &Fixture, regular: u8, evo: u8) -> MnList {
    let mut list = MnList::new(*fx.list.block_hash(), fx.list.height(), 0);
    for n in 1..=regular {
        list.insert_for_tests(testkit::dmn(n, MnType::Regular));
    }
    for n in regular + 1..=regular + evo {
        list.insert_for_tests(testkit::dmn(n, MnType::Evo));
    }
    list
}

fn hash_with(first: u8, last: u8) -> [u8; 32] {
    let mut hash = [0u8; 32];
    hash[0] = first;
    hash[31] = last;
    hash
}

/// Hold `obj` under `hash` with FUNDING votes from masternodes `yes` and `no` of `list`.
fn hold_voted(
    m: &mut Manager,
    hash: [u8; 32],
    obj: &GovObject,
    list: &MnList,
    yes: &[u8],
    no: &[u8],
) {
    let mut state = ObjectState::new(object::object_type(obj), hash);
    for (voters, outcome) in [(yes, vote::OUTCOME_YES), (no, vote::OUTCOME_NO)] {
        for n in voters {
            let dmn = list.iter().find(|d| d.pro_tx_hash == testkit::pro_tx(*n)).expect("voter");
            state.current.entry(outpoint_key(&dmn.collateral)).or_default().insert(
                vote::SIGNAL_FUNDING,
                VoteInstance {
                    outcome,
                    last_update: 0,
                    creation_time: 0,
                },
            );
        }
    }
    m.objects.insert(hash, state);
    m.object_data.insert(hash, obj.clone());
}

fn with_data(base: &GovObject, data: &[u8]) -> GovObject {
    GovObject::new(
        base.hash_parent(),
        base.revision(),
        base.time(),
        base.collateral_hash(),
        data.to_vec(),
        base.object_type(),
        base.masternode_outpoint(),
        base.sig().clone(),
    )
}

fn approved_hashes(m: &Manager, list: &MnList) -> Vec<[u8; 32]> {
    m.approved_proposals(list, &params()).iter().map(|a| a.hash).collect()
}

#[test]
fn approved_proposals_threshold_uses_evonode_weights_and_skips_non_proposals() {
    let fx = fixture();
    let p = params();
    let (proposal, _) = voted_proposal(&fx);
    let trigger = fx
        .objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("trigger");
    // 6 regular + 11 evonodes: weight 50, so 5 yes are needed (17 by count would need 1).
    let list = synthetic_list(&fx, 6, 11);
    assert_eq!(list.counts().valid_weighted, 50);
    assert!(p.min_quorum() < 5);
    let mut m = Manager::new();
    hold_voted(&mut m, hash_with(1, 0), &with_data(proposal, b"one evonode"), &list, &[7], &[]);
    hold_voted(&mut m, hash_with(2, 0), &with_data(proposal, b"evo+regular"), &list, &[7, 1], &[]);
    hold_voted(&mut m, hash_with(3, 0), &with_data(proposal, b"net four"), &list, &[7, 1], &[2]);
    hold_voted(&mut m, hash_with(4, 0), trigger, &list, &[7, 8], &[]);
    hold_voted(&mut m, hash_with(5, 0), &with_data(proposal, b"deleted"), &list, &[7, 8], &[]);
    let deleted = m.objects.get_mut(&hash_with(5, 0)).expect("held");
    deleted.cached_delete = true;
    deleted.expired = true;
    let approved = m.approved_proposals(&list, &p);
    let got: Vec<_> = approved.iter().map(|a| (a.hash, a.data)).collect();
    assert_eq!(
        got,
        vec![(hash_with(5, 0), &b"deleted"[..]), (hash_with(2, 0), &b"evo+regular"[..])]
    );

    // A small list: weight / 10 is 0, so nGovernanceMinQuorum decides.
    let small = synthetic_list(&fx, 2, 0);
    let quorum = u8::try_from(p.min_quorum()).expect("a small quorum");
    assert!((1..=2).contains(&quorum));
    let mut m = Manager::new();
    let at_quorum: Vec<u8> = (1..=quorum).collect();
    let below: Vec<u8> = (1..quorum).collect();
    hold_voted(&mut m, hash_with(1, 0), proposal, &small, &at_quorum, &[]);
    hold_voted(&mut m, hash_with(2, 0), proposal, &small, &below, &[]);
    assert_eq!(approved_hashes(&m, &small), vec![hash_with(1, 0)]);
}

#[test]
fn approved_proposals_order_by_yes_then_arith_hash_descending() {
    let fx = fixture();
    let (proposal, _) = voted_proposal(&fx);
    let list = synthetic_list(&fx, 3, 0);
    let mut m = Manager::new();
    // Map (memcmp) order is a, b, c, top; UintToArith256 order is c < top < a < b.
    let (a, b, c, top) = (hash_with(1, 1), hash_with(2, 2), hash_with(3, 0), hash_with(4, 0));
    for hash in [a, b, c] {
        hold_voted(&mut m, hash, proposal, &list, &[1], &[]);
    }
    hold_voted(&mut m, top, proposal, &list, &[1, 2], &[]);
    assert_eq!(approved_hashes(&m, &list), vec![top, b, a, c]);
}

#[test]
fn find_by_data_hash_returns_first_in_memcmp_order_including_deleted() {
    let fx = fixture();
    let (proposal, _) = voted_proposal(&fx);
    let first = with_data(proposal, b"same");
    let second = GovObject::new(
        first.hash_parent(),
        first.revision(),
        checked_time(first.time(), 1),
        first.collateral_hash(),
        first.data().clone(),
        first.object_type(),
        first.masternode_outpoint(),
        first.sig().clone(),
    );
    let other = with_data(proposal, b"other");
    let list = synthetic_list(&fx, 1, 0);
    let mut m = Manager::new();
    // The first in memcmp order is neither first nor last in UintToArith256 order, and it is
    // marked for deletion.
    let (deleted, kept, third) = (hash_with(1, 5), hash_with(2, 9), hash_with(3, 1));
    hold_voted(&mut m, kept, &second, &list, &[], &[]);
    hold_voted(&mut m, third, &second, &list, &[], &[]);
    hold_voted(&mut m, deleted, &first, &list, &[], &[]);
    hold_voted(&mut m, hash_with(0, 0), &other, &list, &[], &[]);
    m.objects.get_mut(&deleted).expect("held").mark_for_deletion(1);
    let found = m.find_by_data_hash(&object::data_hash(&first)).expect("found");
    assert_eq!(found.time(), first.time());
    assert_eq!(
        m.find_by_data_hash(&object::data_hash(&other)).map(GovObject::time),
        Some(other.time())
    );
    assert!(m.find_by_data_hash(&[0; 32]).is_none());
}

/// A voted fixture trigger, its first vote and a manager whose cached height is the trigger's.
fn local_setup(fx: &Fixture) -> (Manager, GovObject, GovVote) {
    let p = params();
    let (trigger, votes) = fx
        .objects
        .iter()
        .find(|(o, votes)| object::object_type(o) == TRIGGER && !votes.is_empty())
        .expect("a voted trigger");
    let sb = Superblock::parse(trigger, &p).expect("superblock");
    let mut m = Manager::new();
    let lookup = Collaterals::new(fx, 6);
    m.updated_block_tip(sb.height, &fx.list, &GovClock::default(), true, &p, &lookup).expect("tip");
    m.take_relay_queue();
    (m, trigger.clone(), votes[0].clone())
}

struct NoLookup;

impl CollateralLookup for NoLookup {
    fn find_collateral(&self, _: &[u8; 32]) -> Option<FoundCollateral<'_>> {
        panic!("a trigger has no collateral to look up")
    }
}

fn voter_pro_tx(list: &MnList, vote: &GovVote) -> [u8; 32] {
    *list.get_by_collateral(&vote.masternode_outpoint()).expect("voter").pro_tx_hash.as_bytes()
}

#[test]
fn local_add_relays_only_when_synced_and_bumps_sync() {
    let fx = fixture();
    let p = params();
    let clock = GovClock::new(now(&fx), now(&fx));
    for synced in [true, false] {
        let (mut m, trigger, _) = local_setup(&fx);
        let hash = object::hash(&trigger);
        let generation = m.generation;
        let (added, effects) =
            m.add_local_object_at(&trigger, &fx.list, &clock, synced, &p, &NoLookup).expect("add");
        assert_eq!(added, LocalAdd::Added);
        let relayed = if synced {
            vec![hash]
        } else {
            vec![]
        };
        assert_eq!(effects.relay_objects, relayed);
        assert!(effects.relay_votes.is_empty() && effects.meta.is_empty());
        assert_eq!(effects.sync_bumps, vec!["CGovernanceManager::AddGovernanceObject"]);
        assert!(effects.zmq);
        assert!(m.take_relay_queue().objects.is_empty());
        assert!(m.superblocks().get(&hash).is_some());
        assert!(!m.object(&hash).expect("held").cached_delete);
        assert_eq!(m.generation, generation.wrapping_add(1));
        assert_eq!(m.find_by_data_hash(&object::data_hash(&trigger)), Some(&trigger));
    }
}

#[test]
fn local_add_validity_and_duplicate_return_before_the_rate_update() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    let (mut m, trigger, _) = local_setup(&fx);
    let key = outpoint_key(&trigger.masternode_outpoint());
    let empty = MnList::new(*fx.list.block_hash(), fx.list.height(), 0);
    let (invalid, effects) =
        m.add_local_object_at(&trigger, &empty, &clock, true, &p, &lookup).expect("invalid");
    assert!(matches!(invalid, LocalAdd::Invalid(e) if e.starts_with("Failed to find Masternode")));
    assert_eq!(effects, Effects::default());
    assert!(m.objects().is_empty() && m.last_object_record(&key).is_none());

    m.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("add");
    let generation = m.generation;
    let (dup, effects) =
        m.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("dup");
    assert_eq!(dup, LocalAdd::Duplicate);
    assert_eq!(effects, Effects::default());
    assert_eq!(m.last_object_record(&key).expect("record").buffer.count(), 1);
    assert_eq!(m.generation, generation);
    assert!(m.take_relay_queue().objects.is_empty());
}

#[test]
fn local_add_refused_trigger_counts_the_attempt_and_is_not_relayed() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (mut m, base, _) = local_setup(&fx);
    let list = trigger_list(&fx, &base);
    let clock = GovClock::new(checked_time(now(&fx), 5), now(&fx));
    // Inside the additional-relay window, so a schedule before AddTrigger would show.
    let refused = signed_trigger(&base, b"{}".to_vec(), checked_time(clock.now, 3541));
    let hash = object::hash(&refused);
    let key = outpoint_key(&refused.masternode_outpoint());
    let unsigned = GovVote::new(
        refused.masternode_outpoint(),
        TxHash::from_lendian(hash),
        VoteOutcome::new(vote::OUTCOME_YES as u32),
        VoteSignal::new(vote::SIGNAL_FUNDING as u32),
        base.time(),
        Vec::new(),
    );
    let key42 = BlsSecretKey::<BlsScIetf>::from_ikm(&[42; 32]).expect("fixed key");
    let pending = GovVote::new(
        unsigned.masternode_outpoint(),
        unsigned.parent_hash(),
        unsigned.outcome(),
        unsigned.signal(),
        unsigned.time(),
        key42.sign_with(&vote::signature_hash(&unsigned), BlsSigId::Basic).to_bytes().to_vec(),
    );
    assert_eq!(m.process_vote_at(&pending, &list, &clock, &p).to_request, Some(hash));
    let generation = m.generation;
    let (out, effects) =
        m.add_local_object_at(&refused, &list, &clock, true, &p, &lookup).expect("refused");
    assert_eq!(out, LocalAdd::Refused);
    assert_eq!(effects, Effects::default());
    assert!(m.orphan(&hash).is_some());
    assert!(m.object(&hash).expect("held").file.is_empty());
    let state = m.object(&hash).expect("held");
    assert!(state.cached_delete && state.deletion_time == clock.now);
    assert!(m.superblocks().get(&hash).is_none());
    let record = m.last_object_record(&key).expect("attempt counted");
    assert!(record.buffer.count() == 1 && record.status_ok);
    assert!(m.take_relay_queue().objects.is_empty());
    assert_eq!(m.generation, generation.wrapping_add(1));
    assert!(m.additional_relay.is_empty());
    let (dup, _) = m.add_local_object_at(&refused, &list, &clock, true, &p, &lookup).expect("dup");
    assert_eq!(dup, LocalAdd::Duplicate);

    // An expired trigger stays in the set although AddTrigger refuses it.
    let (_, trigger, _) = local_setup(&fx);
    let sb = Superblock::parse(&trigger, &p).expect("superblock");
    let past = checked_height(checked_height(sb.height, p.superblock_cycle()), 1);
    let mut late = Manager::new();
    late.updated_block_tip(past, &fx.list, &GovClock::default(), true, &p, &lookup).expect("tip");
    late.take_relay_queue();
    let generation = late.generation;
    let (out, effects) =
        late.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("expired");
    assert_eq!(out, LocalAdd::Refused);
    assert_eq!(effects, Effects::default());
    let hash = object::hash(&trigger);
    assert!(late.object(&hash).expect("held").cached_delete);
    assert!(late.superblocks().get(&hash).is_some());
    assert!(late.take_relay_queue().objects.is_empty());
    assert_eq!(late.generation, generation.wrapping_add(1));
}

#[test]
fn network_refused_trigger_moves_the_generation() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let (mut m, base) = trigger_relay_setup(&fx);
    m.take_relay_queue();
    let list = trigger_list(&fx, &base);
    let refused = signed_trigger(&base, b"{}".to_vec(), base.time());
    let clock = GovClock::new(base.time(), base.time());
    let generation = m.generation;
    let out = m.process_object_at(&refused, &list, &clock, true, &p, &lookup).expect("refused");
    assert!(out.ok && out.held);
    assert!(m.object(&out.hash).expect("held").cached_delete);
    assert!(m.take_relay_queue().objects.is_empty());
    assert_eq!(m.generation, generation.wrapping_add(1));
}

#[test]
fn local_add_replays_the_orphan_vote() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    for synced in [true, false] {
        let (mut m, trigger, vote) = local_setup(&fx);
        let hash = object::hash(&trigger);
        let orphan = m.process_vote_at(&vote, &fx.list, &clock, &p);
        assert_eq!(orphan.to_request, Some(hash));
        let (added, effects) =
            m.add_local_object_at(&trigger, &fx.list, &clock, synced, &p, &lookup).expect("add");
        assert_eq!(added, LocalAdd::Added);
        assert!(m.orphan(&hash).is_none());
        assert!(m.object(&hash).expect("held").file.has(&vote::hash(&vote)));
        let (objects, votes) = if synced {
            (vec![hash], vec![vote::hash(&vote)])
        } else {
            (vec![], vec![])
        };
        // The object goes out first: `relay_objects` is announced before `relay_votes`.
        assert_eq!((effects.relay_objects, effects.relay_votes), (objects, votes));
        assert_eq!(
            effects.meta,
            vec![MetaOp::AddGovernanceVote {
                pro_tx_hash: voter_pro_tx(&fx.list, &vote),
                object: hash,
            }]
        );
        assert!(m.take_relay_queue().votes.is_empty());
    }

    // Core: governance/governance.cpp:295. An orphan whose expiry equals the adjusted time
    // is not expired (`expiration < now`) and is replayed.
    let (mut m, trigger, vote) = local_setup(&fx);
    let hash = object::hash(&trigger);
    m.process_vote_at(&vote, &fx.list, &clock, &p);
    assert_eq!(m.orphan(&hash).expect("orphan").1, checked_time(clock.now, ORPHAN_EXPIRATION));
    let edge = GovClock::new(clock.now, checked_time(clock.now, ORPHAN_EXPIRATION));
    let (added, effects) =
        m.add_local_object_at(&trigger, &fx.list, &edge, true, &p, &lookup).expect("add");
    assert_eq!(added, LocalAdd::Added);
    assert!(m.orphan(&hash).is_none());
    assert!(m.object(&hash).expect("held").file.has(&vote::hash(&vote)));
    assert_eq!(effects.relay_votes, vec![vote::hash(&vote)]);
    assert_eq!(effects.meta.len(), 1);

    // An orphan past its expiry is dropped without a replay, a relay or a meta record.
    let (mut m, trigger, vote) = local_setup(&fx);
    let hash = object::hash(&trigger);
    m.process_vote_at(&vote, &fx.list, &clock, &p);
    let late = GovClock::new(clock.now, checked_time(clock.now, ORPHAN_EXPIRATION + 1));
    let (added, effects) =
        m.add_local_object_at(&trigger, &fx.list, &late, true, &p, &lookup).expect("add");
    assert_eq!(added, LocalAdd::Added);
    assert!(m.orphan(&hash).is_none());
    assert!(m.object(&hash).expect("held").file.is_empty());
    assert!(effects.relay_votes.is_empty() && effects.meta.is_empty());
}

#[test]
fn local_vote_relays_iff_synced_and_records_meta() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    for synced in [true, false] {
        let (mut m, trigger, vote) = local_setup(&fx);
        let hash = object::hash(&trigger);
        m.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("add");
        let generation = m.generation;
        let (out, effects) = m.process_local_vote_at(&vote, &fx.list, &clock, synced, &p);
        assert_eq!(out.result, Ok(true));
        assert!(m.accepted_vote(&vote::hash(&vote)));
        let relayed = if synced {
            vec![vote::hash(&vote)]
        } else {
            vec![]
        };
        assert_eq!(effects.relay_votes, relayed);
        assert_eq!(
            effects.meta,
            vec![MetaOp::AddGovernanceVote {
                pro_tx_hash: voter_pro_tx(&fx.list, &vote),
                object: hash,
            }]
        );
        assert!(effects.zmq && effects.sync_bumps.is_empty());
        assert!(m.take_relay_queue().votes.is_empty());
        assert_eq!(m.generation, generation.wrapping_add(1));
        assert_eq!(m.trigger_view().generation, m.generation);
    }

    // The voter is not in the list: ProcessVote fails, nothing is relayed or recorded.
    let (mut m, trigger, vote) = local_setup(&fx);
    m.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("add");
    let generation = m.generation;
    let empty = MnList::new(*fx.list.block_hash(), fx.list.height(), 0);
    let (out, effects) = m.process_local_vote_at(&vote, &empty, &clock, true, &p);
    assert_eq!(out.result.expect_err("unknown voter").kind(), ExceptionType::PermanentError);
    assert_eq!(effects, Effects::default());
    assert!(m.take_relay_queue().votes.is_empty());
    assert_eq!(m.generation, generation);
}

#[test]
fn local_vote_for_an_unknown_parent_requests_nothing() {
    let fx = fixture();
    let p = params();
    let clock = GovClock::new(now(&fx), now(&fx));
    let (mut m, trigger, vote) = local_setup(&fx);
    let generation = m.generation;
    let (out, effects) = m.process_local_vote_at(&vote, &fx.list, &clock, true, &p);
    assert_eq!(out.result.expect_err("orphan").kind(), ExceptionType::Warning);
    assert_eq!(out.to_request, None);
    assert_eq!(effects, Effects::default());
    assert!(m.orphan(&object::hash(&trigger)).is_some());
    assert_eq!(m.generation, generation);
}

#[test]
fn second_local_vote_within_the_hour_is_temporary() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    let (mut m, trigger, vote) = local_setup(&fx);
    m.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("add");
    assert_eq!(m.process_local_vote_at(&vote, &fx.list, &clock, true, &p).0.result, Ok(true));
    let generation = m.generation;
    // A later vote that only the rate check can refuse before its signature is read.
    let again = GovVote::new(
        vote.masternode_outpoint(),
        vote.parent_hash(),
        VoteOutcome::new(vote::OUTCOME_NO as u32),
        vote.signal(),
        checked_time(vote.time(), 1),
        Vec::new(),
    );
    let within = GovClock::new(clock.now, checked_time(clock.adjusted, 3599));
    let (out, effects) = m.process_local_vote_at(&again, &fx.list, &within, true, &p);
    let e = out.result.expect_err("rate");
    assert_eq!(e.kind(), ExceptionType::TemporaryError);
    assert!(e.message().contains("Masternode voting too often"));
    assert_eq!(effects, Effects::default());
    assert_eq!(m.generation, generation);
    // An hour later the rate check passes and the missing signature is what refuses it.
    let after = GovClock::new(clock.now, checked_time(clock.adjusted, 3600));
    let e = m.process_local_vote_at(&again, &fx.list, &after, true, &p).0.result.expect_err("sig");
    assert_eq!(e.kind(), ExceptionType::PermanentError);
    assert!(e.message().contains("Invalid vote, MN outpoint"));
}

#[test]
fn local_vote_returning_false_has_no_effects() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));

    // Core: governance/governance.cpp:838-842. A known vote is skipped.
    let (mut m, trigger, vote) = local_setup(&fx);
    m.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("add");
    assert_eq!(m.process_local_vote_at(&vote, &fx.list, &clock, true, &p).0.result, Ok(true));
    m.take_relay_queue();
    let generation = m.generation;
    let (out, effects) = m.process_local_vote_at(&vote, &fx.list, &clock, true, &p);
    assert_eq!((out.result, out.to_request), (Ok(false), None));
    assert_eq!(effects, Effects::default());
    assert!(m.take_relay_queue().votes.is_empty());
    assert_eq!(m.generation, generation);

    // Core: governance/governance.cpp:867-870. A parent marked for deletion ignores the vote.
    let (_, trigger, vote) = local_setup(&fx);
    let sb = Superblock::parse(&trigger, &p).expect("superblock");
    let past = checked_height(checked_height(sb.height, p.superblock_cycle()), 1);
    let mut late = Manager::new();
    late.updated_block_tip(past, &fx.list, &GovClock::default(), true, &p, &lookup).expect("tip");
    let (refused, _) =
        late.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("refused");
    assert_eq!(refused, LocalAdd::Refused);
    late.take_relay_queue();
    let generation = late.generation;
    let (out, effects) = late.process_local_vote_at(&vote, &fx.list, &clock, true, &p);
    assert_eq!((out.result, out.to_request), (Ok(false), None));
    assert_eq!(effects, Effects::default());
    assert!(late.take_relay_queue().votes.is_empty());
    assert_eq!(late.generation, generation);
    assert!(!late.accepted_vote(&vote::hash(&vote)));
    assert!(late.object(&object::hash(&trigger)).expect("held").file.is_empty());
}

#[test]
fn local_proposal_add_checks_collateral_and_never_postpones() {
    let fx = fixture();
    let p = params();
    let (obj, _) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let key = outpoint_key(&obj.masternode_outpoint());
    let clock = GovClock::new(now(&fx), now(&fx));

    let mut missing = Collaterals::new(&fx, 6);
    missing.txs.clear();
    for lookup in [missing, Collaterals::new(&fx, 1)] {
        let mut m = Manager::new();
        let (out, effects) =
            m.add_local_object_at(obj, &fx.list, &clock, true, &p, &lookup).expect("invalid");
        assert_eq!(out, LocalAdd::Invalid("Invalid proposal collateral".to_owned()));
        assert_eq!(effects, Effects::default());
        assert!(m.objects().is_empty() && m.postponed().is_empty());
        assert!(m.take_relay_queue().objects.is_empty());
    }

    let lookup = Collaterals::new(&fx, 6);
    let mut m = Manager::new();
    let generation = m.generation;
    let (out, effects) =
        m.add_local_object_at(obj, &fx.list, &clock, true, &p, &lookup).expect("add");
    assert_eq!(out, LocalAdd::Added);
    assert_eq!(effects.relay_objects, vec![hash]);
    assert_eq!(effects.sync_bumps, vec!["CGovernanceManager::AddGovernanceObject"]);
    assert!(m.object(&hash).is_some() && m.postponed().is_empty());
    assert!(m.last_object_record(&key).is_none());
    assert_eq!(m.generation, generation.wrapping_add(1));
}

#[test]
fn postponed_pass_skips_a_proposal_added_locally_meanwhile() {
    let fx = fixture();
    let p = params();
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let key = outpoint_key(&obj.masternode_outpoint());
    let clock = GovClock::new(now(&fx), now(&fx));
    let mut m = Manager::new();
    let unconfirmed = Collaterals::new(&fx, 1);
    let out = m.process_object_at(obj, &fx.list, &clock, true, &p, &unconfirmed).expect("postpone");
    assert!(out.postponed && !out.held);

    let confirmed = Collaterals::new(&fx, 6);
    let (added, _) =
        m.add_local_object_at(obj, &fx.list, &clock, true, &p, &confirmed).expect("add");
    assert_eq!(added, LocalAdd::Added);
    assert!(m.postponed().contains_key(&hash));
    let (voted, _) = m.process_local_vote_at(&votes[0], &fx.list, &clock, true, &p);
    assert_eq!(voted.result, Ok(true));
    m.take_relay_queue();
    let generation = m.generation;
    let file_len = m.object(&hash).expect("held").file.len();
    assert!(m.last_object_record(&key).is_none());

    let outcomes =
        m.check_postponed_objects_at(&fx.list, &clock, true, &p, &confirmed).expect("postponed");
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].ok && outcomes[0].held);
    assert!(m.postponed().is_empty());
    let state = m.object(&hash).expect("held");
    assert_eq!(state.file.len(), file_len);
    assert!(state.file.has(&vote::hash(&votes[0])));
    assert!(m.accepted_vote(&vote::hash(&votes[0])));
    assert!(m.take_relay_queue().objects.is_empty());
    assert!(m.last_object_record(&key).is_none());
    assert_eq!(m.generation, generation);
}

#[test]
fn synced_clean_dropping_an_expired_trigger_moves_the_generation() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    let (mut m, trigger, _) = local_setup(&fx);
    let hash = object::hash(&trigger);
    m.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("add");
    let sb = Superblock::parse(&trigger, &p).expect("superblock");
    let past = checked_height(checked_height(sb.height, p.superblock_cycle()), 1);
    m.updated_block_tip(past, &fx.list, &clock, true, &p, &lookup).expect("tip");
    assert!(m.trigger_view().superblocks.get(&hash).is_some());
    assert!(m.trigger_view().objects.contains_key(&hash));

    // Unsynced, the clean returns before touching anything.
    let generation = m.generation();
    assert_eq!(m.check_and_remove(&fx.list, &clock, false, &p), Effects::default());
    assert_eq!(m.generation(), generation);

    let effects = m.check_and_remove(&fx.list, &clock, true, &p);
    // Marked now, so erased only by a clean at least 600 s later.
    assert!(effects.forget_requests.is_empty());
    assert!(m.object(&hash).expect("held").expired);
    assert_eq!(m.generation(), generation.wrapping_add(1));
    let view = m.trigger_view();
    assert!(view.superblocks.get(&hash).is_none());
    assert!(!view.objects.contains_key(&hash));
    assert_eq!(view.generation, m.generation());
}

#[test]
fn trigger_view_holds_only_the_triggers_in_the_set() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    let (mut m, trigger, _) = local_setup(&fx);
    let (proposal, _) = voted_proposal(&fx);
    m.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("add");
    m.add_local_object_at(proposal, &fx.list, &clock, true, &p, &lookup).expect("add");
    assert_eq!(m.objects().len(), 2);
    let view = m.trigger_view();
    assert_eq!(view.objects.keys().collect::<Vec<_>>(), vec![&object::hash(&trigger)]);
    let held = m.object(&object::hash(&trigger)).expect("held");
    assert_eq!(format!("{:?}", view.objects[&object::hash(&trigger)]), format!("{held:?}"));
}

fn governance_vote_meta(list: &MnList, vote: &GovVote, object: [u8; 32]) -> MetaOp {
    MetaOp::AddGovernanceVote {
        pro_tx_hash: voter_pro_tx(list, vote),
        object,
    }
}

#[test]
fn network_vote_records_meta_and_moves_the_generation_only_when_accepted() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let mut m = Manager::new();

    // An orphan for an unknown parent.
    let generation = m.generation;
    let orphan = m.process_vote_at(&votes[0], &fx.list, &clock, &p);
    assert_eq!(orphan.result.expect_err("orphan").kind(), ExceptionType::Warning);
    assert!(m.take_meta().is_empty());
    assert_eq!(m.generation, generation);
    m.remove_orphan_parent(&hash);

    assert!(m.process_object_at(obj, &fx.list, &clock, true, &p, &lookup).expect("add").held);
    m.take_meta();

    // A rejected vote: the voter is not in the list.
    let empty = MnList::new(*fx.list.block_hash(), fx.list.height(), 0);
    let generation = m.generation;
    let rejected = m.process_vote_at(&votes[0], &empty, &clock, &p);
    assert_eq!(rejected.result.expect_err("unknown voter").kind(), ExceptionType::PermanentError);
    assert!(m.take_meta().is_empty());
    assert_eq!(m.generation, generation);

    // An accepted proposal vote records meta but affects no trigger: the generation stays.
    let accepted = m.process_vote_at(&votes[0], &fx.list, &clock, &p);
    assert_eq!(accepted.result, Ok(true));
    assert_eq!(m.take_meta(), vec![governance_vote_meta(&fx.list, &votes[0], hash)]);
    assert_eq!(m.generation, generation);

    // A duplicate is `Ok(false)`.
    let duplicate = m.process_vote_at(&votes[0], &fx.list, &clock, &p);
    assert_eq!(duplicate.result, Ok(false));
    assert!(m.take_meta().is_empty());
    assert_eq!(m.generation, generation);

    // An accepted trigger vote records meta and moves the generation once.
    let (mut m, trigger, trigger_vote) = local_setup(&fx);
    m.add_local_object_at(&trigger, &fx.list, &clock, true, &p, &lookup).expect("add");
    m.take_meta();
    let generation = m.generation;
    let accepted = m.process_vote_at(&trigger_vote, &fx.list, &clock, &p);
    assert_eq!(accepted.result, Ok(true));
    assert_eq!(
        m.take_meta(),
        vec![governance_vote_meta(&fx.list, &trigger_vote, object::hash(&trigger))]
    );
    assert_eq!(m.generation, generation.wrapping_add(1));
    assert_eq!(m.trigger_view().generation, m.generation);
    assert_eq!(m.generation(), m.generation);

    // A parent marked for deletion ignores the vote.
    let (trigger, trigger_votes) = fx
        .objects
        .iter()
        .find(|(o, votes)| object::object_type(o) == TRIGGER && !votes.is_empty())
        .expect("a voted trigger");
    let sb = Superblock::parse(trigger, &p).expect("trigger");
    let past = fx
        .list
        .clone()
        .with_height(checked_height(checked_height(sb.height, p.superblock_cycle()), 1));
    let all = Collaterals::from_fixture(&fx);
    let mut m = Manager::new();
    assert!(m.process_object(trigger, &past, trigger.time(), true, &p, &all).expect("add").held);
    assert!(m.object(&object::hash(trigger)).expect("held").cached_delete);
    m.take_meta();
    let generation = m.generation;
    let deleted = m.process_vote_at(&trigger_votes[0], &fx.list, &clock, &p);
    assert_eq!(deleted.result, Ok(false));
    assert!(m.take_meta().is_empty());
    assert_eq!(m.generation, generation);
}

#[test]
fn network_orphan_replay_relays_only_when_synced_and_records_meta() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let vote_hash = vote::hash(&votes[0]);
    for synced in [false, true] {
        let mut m = Manager::new();
        assert_eq!(m.process_vote_at(&votes[0], &fx.list, &clock, &p).to_request, Some(hash));
        let generation = m.generation;
        let out = m.process_object_at(obj, &fx.list, &clock, synced, &p, &lookup).expect("add");
        assert!(out.ok && out.held);
        assert!(m.orphan(&hash).is_none());
        assert!(m.object(&hash).expect("held").file.has(&vote_hash));
        let relay = m.take_relay_queue();
        let (objects, relayed) = if synced {
            (vec![hash], vec![vote_hash])
        } else {
            (vec![], vec![])
        };
        assert_eq!((relay.objects, relay.votes), (objects, relayed));
        assert_eq!(m.take_meta(), vec![governance_vote_meta(&fx.list, &votes[0], hash)]);
        assert_eq!(m.generation, generation.wrapping_add(1));
    }
}

#[test]
fn postponed_pass_relays_the_replayed_orphan() {
    let fx = fixture();
    let p = params();
    let clock = GovClock::new(now(&fx), now(&fx));
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let mut m = Manager::new();
    let unconfirmed = Collaterals::new(&fx, 1);
    let out = m.process_object_at(obj, &fx.list, &clock, true, &p, &unconfirmed).expect("postpone");
    assert!(out.postponed);
    assert_eq!(m.process_vote_at(&votes[0], &fx.list, &clock, &p).to_request, Some(hash));
    m.take_relay_queue();
    let generation = m.generation;

    let confirmed = Collaterals::new(&fx, 6);
    let outcomes =
        m.check_postponed_objects_at(&fx.list, &clock, true, &p, &confirmed).expect("postponed");
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].held);
    assert!(m.object(&hash).expect("held").file.has(&vote::hash(&votes[0])));
    let relay = m.take_relay_queue();
    assert_eq!((relay.objects, relay.votes), (vec![hash], vec![vote::hash(&votes[0])]));
    assert_eq!(m.take_meta(), vec![governance_vote_meta(&fx.list, &votes[0], hash)]);
    assert_eq!(m.generation, generation.wrapping_add(1));
}

#[test]
fn tip_returns_the_postponed_replay_meta() {
    let fx = fixture();
    let p = params();
    let clock = GovClock::new(now(&fx), now(&fx));
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let mut m = Manager::new();
    let unconfirmed = Collaterals::new(&fx, 1);
    let out = m.process_object_at(obj, &fx.list, &clock, true, &p, &unconfirmed).expect("postpone");
    assert!(out.postponed);
    assert_eq!(m.process_vote_at(&votes[0], &fx.list, &clock, &p).to_request, Some(hash));
    m.take_relay_queue();

    let confirmed = Collaterals::new(&fx, 6);
    let effects =
        m.updated_block_tip(fx.list.height(), &fx.list, &clock, true, &p, &confirmed).expect("tip");
    assert!(m.object(&hash).expect("held").file.has(&vote::hash(&votes[0])));
    assert_eq!(effects.relay_votes, vec![vote::hash(&votes[0])]);
    assert_eq!(effects.meta, vec![governance_vote_meta(&fx.list, &votes[0], hash)]);
    assert!(m.take_meta().is_empty());
}

#[test]
fn clean_returns_pending_vote_meta_before_removals() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let trigger = fx
        .objects
        .iter()
        .find(|(o, _)| object::object_type(o) == TRIGGER)
        .expect("trigger")
        .0
        .clone();
    let sb = Superblock::parse(&trigger, &p).expect("superblock");
    let past = checked_height(checked_height(sb.height, p.superblock_cycle()), 1);
    let mut m = Manager::new();
    m.updated_block_tip(past, &fx.list, &GovClock::default(), true, &p, &lookup).expect("tip");
    let expired_list = fx.list.clone().with_height(past);
    let out = m
        .process_object(&trigger, &expired_list, trigger.time(), true, &p, &lookup)
        .expect("trigger");
    assert!(m.object(&out.hash).expect("state").cached_delete);
    let (obj, votes) = voted_proposal(&fx);
    let clock = GovClock::new(now(&fx), now(&fx));
    m.process_object_at(obj, &fx.list, &clock, true, &p, &lookup).expect("proposal");
    assert_eq!(m.process_vote_at(&votes[0], &fx.list, &clock, &p).result, Ok(true));

    let effects = m.check_and_remove(
        &fx.list,
        &GovClock::new(trigger.time() + 600, trigger.time()),
        true,
        &p,
    );
    assert_eq!(
        effects.meta,
        vec![
            governance_vote_meta(&fx.list, &votes[0], object::hash(obj)),
            MetaOp::RemoveObject(out.hash)
        ]
    );
    assert!(m.take_meta().is_empty());
}

#[test]
fn local_vote_returns_earlier_meta_ahead_of_its_own() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    let (obj, votes) = voted_proposal(&fx);
    let hash = object::hash(obj);
    let mut m = Manager::new();
    m.process_object_at(obj, &fx.list, &clock, true, &p, &lookup).expect("add");
    assert_eq!(m.process_vote_at(&votes[0], &fx.list, &clock, &p).result, Ok(true));
    let (out, effects) = m.process_local_vote_at(&votes[1], &fx.list, &clock, true, &p);
    assert_eq!(out.result, Ok(true));
    assert_eq!(
        effects.meta,
        vec![
            governance_vote_meta(&fx.list, &votes[0], hash),
            governance_vote_meta(&fx.list, &votes[1], hash)
        ]
    );
    assert!(m.take_meta().is_empty());
}

#[test]
fn meta_queue_is_drained_by_take_and_cleared_by_import() {
    let fx = fixture();
    let p = params();
    let lookup = Collaterals::new(&fx, 6);
    let clock = GovClock::new(now(&fx), now(&fx));
    let (obj, votes) = voted_proposal(&fx);
    let mut m = Manager::new();
    m.process_object_at(obj, &fx.list, &clock, true, &p, &lookup).expect("add");
    assert_eq!(m.process_vote_at(&votes[0], &fx.list, &clock, &p).result, Ok(true));
    assert_eq!(m.take_meta().len(), 1);
    assert!(m.take_meta().is_empty());

    assert_eq!(m.process_vote_at(&votes[1], &fx.list, &clock, &p).result, Ok(true));
    let state = m.export_state();
    m.import_state(state);
    assert!(m.take_meta().is_empty());
}

fn operator_vote(trigger: &GovObject, time: i64) -> GovVote {
    let unsigned = GovVote::new(
        trigger.masternode_outpoint(),
        TxHash::from_lendian(object::hash(trigger)),
        VoteOutcome::new(vote::OUTCOME_YES as u32),
        VoteSignal::new(vote::SIGNAL_VALID as u32),
        time,
        Vec::new(),
    );
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[42; 32]).expect("fixed key");
    let sig = key.sign_with(&vote::signature_hash(&unsigned), BlsSigId::Basic).to_bytes().to_vec();
    GovVote::new(
        unsigned.masternode_outpoint(),
        unsigned.parent_hash(),
        unsigned.outcome(),
        unsigned.signal(),
        unsigned.time(),
        sig,
    )
}

struct Superseded {
    m: Manager,
    list: MnList,
    trigger: GovObject,
    old: [u8; 32],
    new: [u8; 32],
    at: i64,
}

/// A trigger with one masternode's vote and that masternode's newer vote on the same signal.
fn superseded(fx: &Fixture) -> Superseded {
    let p = params();
    let lookup = Collaterals::new(fx, 6);
    let base = fx
        .objects
        .iter()
        .find(|(obj, _)| object::object_type(obj) == TRIGGER)
        .map(|(obj, _)| obj)
        .expect("trigger");
    let list = trigger_list(fx, base);
    let trigger = signed_trigger(base, base.data().clone(), base.time());
    let mut m = Manager::new();
    assert!(
        m.process_object(&trigger, &list, trigger.time(), true, &p, &lookup).expect("add").held
    );
    let first = operator_vote(&trigger, trigger.time());
    assert_eq!(m.process_vote(&first, &list, trigger.time(), &p).result, Ok(true));
    let at = checked_time(trigger.time(), dash_governance::tally::UPDATE_MIN);
    let second = operator_vote(&trigger, at);
    assert_eq!(m.process_vote(&second, &list, at, &p).result, Ok(true));
    Superseded {
        m,
        list,
        trigger,
        old: vote::hash(&first),
        new: vote::hash(&second),
        at,
    }
}

/// Erasing an object drops every index entry pointing at it, not only the votes left in its file.
// Core: governance/governance.cpp:458-468
#[test]
fn erase_drops_every_vote_indexed_to_the_object() {
    let fx = fixture();
    let mut s = superseded(&fx);
    assert_eq!(s.m.vote_index_len(), 2);
    assert_eq!(s.m.object(&object::hash(&s.trigger)).expect("state").file.len(), 1);
    s.m.erase_object(object::hash(&s.trigger), s.at, &params());
    assert_eq!(s.m.vote_index_len(), 0);
    assert!(!s.m.accepted_vote(&s.old));
    assert!(!s.m.accepted_vote(&s.new));
}

/// `RemoveOldVotes` leaves the superseded hash indexed: it is no longer held, yet a request for
/// it is still refused.
// Core: governance/governance.cpp:194-200; governance/governance.cpp:645; governance/votedb.cpp:103-118
#[test]
fn a_superseded_vote_stays_indexed_but_is_not_held() {
    let fx = fixture();
    let s = superseded(&fx);
    assert!(!s.m.have_vote(&s.old));
    assert!(s.m.accepted_vote(&s.old));
    assert!(!s.m.confirm_inventory_request(Inventory::Vote(s.old)));
    assert!(s.m.have_vote(&s.new));
    assert!(!s.m.confirm_inventory_request(Inventory::Vote(s.new)));
}

/// The clean pass leaves the index alone: a vote whose masternode left the list stays counted
/// and held. This is not coverage of `ClearMasternodeVotes`: v24.0.0 never runs that pass, since
/// its dirty-object list is never filled.
// Core: governance/governance.cpp:410-423; governance/object.cpp:483-497
#[test]
fn a_vote_from_a_removed_masternode_stays_counted_through_clean() {
    let fx = fixture();
    let s = superseded(&fx);
    let mut m = s.m;
    let mut removed = s.list.clone();
    let mn = removed.get_by_collateral(&s.trigger.masternode_outpoint()).expect("mn").clone();
    removed.remove(&mn.pro_tx_hash).expect("remove");
    m.check_and_remove(&removed, &GovClock::new(s.at, s.at), true, &params());
    assert_eq!(m.vote_index_len(), 2);
    assert!(m.accepted_vote(&s.old));
    assert!(m.accepted_vote(&s.new));
    assert!(m.have_vote(&s.new));
}

/// `RemoveInvalidVotes` erases each removed hash from the index; a superseded hash, no longer in
/// the file, stays.
// Core: governance/governance.cpp:1212-1219
#[test]
fn remove_invalid_votes_erases_the_removed_hashes_from_the_index() {
    let fx = fixture();
    let p = params();
    let s = superseded(&fx);
    let mut m = s.m;
    let clock = GovClock::new(s.at, s.at);
    let mut rekeyed = s.list.clone();
    let mn = rekeyed.get_by_collateral(&s.trigger.masternode_outpoint()).expect("mn").clone();
    let mut cold = (*mn.cold).clone();
    cold.pub_key_operator = testkit::operator_key(91, BlsSchemeKind::Basic);
    rekeyed.update(&mn.pro_tx_hash, mn.hot, Arc::new(testkit::fresh(cold))).expect("rekey");
    m.remove_invalid_votes(&s.list, &clock, &p, true);
    assert_eq!(m.vote_index_len(), 2);
    let effects = m.remove_invalid_votes(&rekeyed, &clock, &p, true);
    assert_eq!(effects.forget_requests, vec![s.new]);
    assert_eq!(m.vote_index_len(), 1);
    assert!(!m.accepted_vote(&s.new));
    assert!(m.accepted_vote(&s.old));
}
