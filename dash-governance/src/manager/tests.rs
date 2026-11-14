//! Manager transitions over Core's captured objects and signed votes.
#[path = "../../tests/common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{Collaterals, Fixture, fixture, params};
use dash_dml::{MnList, testkit};
use dash_governance::exception::ExceptionType;
use dash_governance::manager::{Manager, ORPHAN_EXPIRATION};
use dash_governance::object::{self, PROPOSAL, TRIGGER};
use dash_governance::strings::hash_hex;
use dash_governance::superblocks::HeightOverflow;
use dash_governance::tally::outpoint_key;
use dash_governance::trigger::Superblock;
use dash_governance::vote;
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};
use dash_primitives::{GovObject, GovVote, TxHash, VoteOutcome, VoteSignal};
use dash_types::Numeric;

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
    assert!(!m.accepted_vote(&vote::hash(&votes[0])));
    // Replayed orphans bypass the hourly rate check and are known by the object's vote file,
    // but are absent from the manager's accepted-vote map.
    let e = m
        .process_vote(&votes[0], &fx.list, checked_time(now, 600), &p)
        .result
        .expect_err("file known");
    assert!(e.message().ends_with("Already known valid vote"));
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
