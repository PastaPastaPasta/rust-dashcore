//! L0: `CGovernanceTriggerManager` and `CSuperblockManager` (`governance/superblock.cpp`) on the
//! trigger of the Core-produced regtest network (`common`): event height 180, a cycle of 20
//! blocks (`chainparams.cpp:806`, regtest's 20), so a held trigger lives to height 200.

mod common;

use std::collections::BTreeMap;

use common::{Fixture, fixture, params, params_of};
use dash_evo::payments::GovernanceView;
use dash_governance::object::{self, PROPOSAL, TRIGGER};
use dash_governance::superblocks::{AtTip, SuperblockSet};
use dash_governance::tally::{ObjectState, VoteInstance, outpoint_key};
use dash_governance::trigger::Status;
use dash_governance::vote::{OUTCOME_YES, SIGNAL_DELETE, SIGNAL_FUNDING};
use dash_params::Network;
use dash_primitives::GovObject;

fn trigger(fx: &Fixture) -> &GovObject {
    fx.objects
        .iter()
        .map(|(o, _)| o)
        .find(|o| object::object_type(o) == TRIGGER)
        .expect("node 0's trigger")
}

/// An object state with `yes` funding votes from the list's first members.
fn funded(fx: &Fixture, obj_type: i32, hash: [u8; 32], yes: usize) -> ObjectState {
    let mut state = ObjectState::new(obj_type, hash);
    for dmn in fx.list.iter().take(yes) {
        state.current.entry(outpoint_key(&dmn.collateral)).or_default().insert(
            SIGNAL_FUNDING,
            VoteInstance {
                outcome: OUTCOME_YES,
                ..VoteInstance::default()
            },
        );
    }
    state
}

#[test]
fn held_triggers_and_the_loaded_flag() {
    let fx = fixture();
    let p = params();
    let obj = trigger(&fx);
    let hash = object::hash(obj);
    let mut set = SuperblockSet::new();
    assert!(!set.is_loaded());
    set.set_loaded(true);
    assert!(set.is_loaded());
    assert_eq!(set.add_trigger(obj, 179, &p), Ok(true));
    // A held hash is not added again.
    assert_eq!(set.add_trigger(obj, 179, &p), Ok(false));
    assert_eq!(set.get(&hash).map(|sb| (sb.height, sb.status)), Some((180, Status::Valid)));
    assert_eq!(set.iter().map(|(h, _)| *h).collect::<Vec<_>>(), vec![hash]);
    set.remove_trigger(&hash);
    assert!(set.get(&hash).is_none());
    // Added at a height past its life, it is held and reported expired.
    assert_eq!(set.add_trigger(obj, 201, &p), Ok(false));
    assert!(set.get(&hash).is_some());
    set.clear();
    assert!(!set.is_loaded() && set.iter().next().is_none());
}

/// `Clean` (`superblock.cpp:474-530`): a trigger without its object, or whose object is not a
/// trigger, is invalid and dropped; an expired one marks its object expired and for deletion.
#[test]
fn clean_drops_invalid_and_expired_triggers() {
    let fx = fixture();
    let p = params();
    let obj = trigger(&fx);
    let hash = object::hash(obj);
    let other = GovObject {
        time: obj.time + 1,
        ..obj.clone()
    };
    let other_hash = object::hash(&other);
    let third = GovObject {
        time: obj.time + 2,
        ..obj.clone()
    };
    let third_hash = object::hash(&third);
    let mut set = SuperblockSet::new();
    for o in [obj, &other, &third] {
        assert_eq!(set.add_trigger(o, 179, &p), Ok(true));
    }
    let mut states = BTreeMap::new();
    states.insert(hash, ObjectState::new(TRIGGER, hash));
    states.insert(other_hash, ObjectState::new(PROPOSAL, other_hash));
    // `third` has no object.
    let mut removed = set.clean(185, &mut states, 1000, &p);
    removed.sort();
    let mut expected = vec![other_hash, third_hash];
    expected.sort();
    assert_eq!(removed, expected);
    assert!(set.get(&hash).is_some());
    let other_state = &states[&other_hash];
    assert!(!other_state.expired && other_state.cached_delete && other_state.deletion_time == 1000);
    assert!(!states[&hash].cached_delete);
    // Past height 200 the live trigger expires.
    assert_eq!(set.clean(201, &mut states, 2000, &p), vec![hash]);
    let state = &states[&hash];
    assert!(state.expired && state.cached_delete && state.deletion_time == 2000);
    assert!(set.iter().next().is_none());
}

/// `IsSuperblockTriggered`, `GetSuperblockPayments` and `ExecuteBestSuperblock`
/// (`superblock.cpp:578-675`), directly and through EV3's `GovernanceView`.
#[test]
fn triggered_payments_and_execution() {
    let fx = fixture();
    let p = params();
    let obj = trigger(&fx);
    let hash = object::hash(obj);
    let mut set = SuperblockSet::new();
    assert_eq!(set.add_trigger(obj, 179, &p), Ok(true));
    let mut unfunded = BTreeMap::from([(hash, funded(&fx, TRIGGER, hash, 0))]);
    assert!(!set.is_triggered(&fx.list, 180, &mut unfunded, 1000, &p));
    assert!(set.payments(&fx.list, 180, &unfunded, &p).is_none());
    let mut states = BTreeMap::from([(hash, funded(&fx, TRIGGER, hash, 2))]);
    assert!(set.is_triggered(&fx.list, 180, &mut states, 1000, &p));
    assert!(states[&hash].cached_funding);
    assert!(!set.is_triggered(&fx.list, 181, &mut states, 1000, &p));
    let payments = set.payments(&fx.list, 180, &states, &p).expect("the best trigger pays");
    assert_eq!(payments, set.get(&hash).expect("held").tx_outs());
    assert!(!payments.is_empty());

    set.set_loaded(true);
    {
        let mut view = AtTip {
            set: &set,
            objects: &mut states,
            list: &fx.list,
            now: 1000,
            params: &p,
        };
        assert_eq!(view.gov_valid(), Ok(true));
        assert_eq!(view.sb_triggered(180), Ok(true));
        assert_eq!(view.sb_triggered(181), Ok(false));
        assert_eq!(view.sb_payments(180), Ok(Some(payments.clone())));
        assert_eq!(view.sb_payments(181), Ok(None));
        assert_eq!(view.sb_triggered(u32::MAX), Ok(false));
    }
    let unloaded = SuperblockSet::new();
    let mut view = AtTip {
        set: &unloaded,
        objects: &mut unfunded,
        list: &fx.list,
        now: 1000,
        params: &p,
    };
    assert_eq!(view.gov_valid(), Ok(false));

    set.execute_best(&fx.list, 180, &states, &p);
    assert_eq!(set.get(&hash).map(|sb| sb.status), Some(Status::Executed));
}

/// The fixture's trigger with its JSON's `event_block_height` replaced.
fn at_height(fx: &Fixture, height: i32) -> GovObject {
    let obj = trigger(fx);
    let text = String::from_utf8(obj.data.clone()).expect("JSON text");
    let from = "\"event_block_height\": 180";
    assert!(text.contains(from), "{text}");
    let data = text.replace(from, &format!("\"event_block_height\": {height}")).into_bytes();
    GovObject {
        data,
        ..obj.clone()
    }
}

/// `GetBestSuperblock` and `IsSuperblockTriggered` answer nothing off a superblock height
/// (`superblock.cpp:560-562`, `:581-583`), though `AddTrigger` holds a funded trigger for it
/// (`:436-465` checks no height).
#[test]
fn off_cycle_heights_are_never_triggered() {
    let fx = fixture();
    let p = params();
    let obj = at_height(&fx, 181);
    let hash = object::hash(&obj);
    let mut set = SuperblockSet::new();
    assert_eq!(set.add_trigger(&obj, 179, &p), Ok(true));
    assert_eq!(set.get(&hash).map(|sb| sb.height), Some(181));
    let mut states = BTreeMap::from([(hash, funded(&fx, TRIGGER, hash, 2))]);
    assert!(set.best(&fx.list, 181, &states, &p).is_none());
    assert!(!set.is_triggered(&fx.list, 181, &mut states, 1000, &p));
    // Not even the sentinel flags are updated.
    assert!(states[&hash].dirty && !states[&hash].cached_funding);
    assert!(set.payments(&fx.list, 181, &states, &p).is_none());
}

/// `IsSuperblockTriggered` stops at the first funded trigger in map order
/// (`superblock.cpp:589-617`): a later trigger for the height keeps its flags and gets no
/// deletion mark, though its delete votes reach the threshold.
#[test]
fn triggered_stops_at_the_first_funded_trigger() {
    let fx = fixture();
    let p = params();
    let a = trigger(&fx).clone();
    let b = GovObject {
        time: a.time + 1,
        ..a.clone()
    };
    let (mut first, mut second) = (object::hash(&a), object::hash(&b));
    if second < first {
        std::mem::swap(&mut first, &mut second);
    }
    let mut set = SuperblockSet::new();
    for o in [&a, &b] {
        assert_eq!(set.add_trigger(o, 179, &p), Ok(true));
    }
    let mut later = ObjectState::new(TRIGGER, second);
    // The two evo masternodes: weight 8, regtest's deletion threshold for 12.
    for dmn in fx.list.iter().filter(|d| d.is_evo()) {
        later.current.entry(outpoint_key(&dmn.collateral)).or_default().insert(
            SIGNAL_DELETE,
            VoteInstance {
                outcome: OUTCOME_YES,
                ..VoteInstance::default()
            },
        );
    }
    let mut states =
        BTreeMap::from([(first, funded(&fx, TRIGGER, first, 2)), (second, later.clone())]);
    assert!(set.is_triggered(&fx.list, 180, &mut states, 1000, &p));
    assert!(states[&first].cached_funding && !states[&first].dirty);
    let flags = |s: &ObjectState| {
        (
            s.dirty,
            s.cached_funding,
            s.cached_valid,
            s.cached_delete,
            s.cached_endorsed,
            s.deletion_time,
        )
    };
    assert_eq!(flags(&states[&second]), flags(&later));
    // On its own, the later trigger is marked for deletion.
    let mut alone = BTreeMap::from([(second, later)]);
    assert!(!set.is_triggered(&fx.list, 180, &mut alone, 1000, &p));
    assert!(alone[&second].cached_delete && !alone[&second].dirty);
}

/// Trigger lifetimes on mainnet, where the three horizons differ (`superblock.cpp:345-357`):
/// `AddTrigger` judges a new trigger as Valid (576 blocks, `:461-464`), `Clean` drops a Valid
/// one after 576 and an Executed one after the cycle (`:500-506`).
#[test]
fn trigger_lifetimes_on_mainnet() {
    let fx = fixture();
    let p = params_of(Network::Main);
    let cycle = p.superblock_cycle();
    assert!(cycle > 576);
    let event = (p.superblock_start() + cycle - 1) / cycle * cycle;
    let data = format!(
        "{{\"event_block_height\": {event}, \"payment_addresses\": \"XwyHjrAVXFSs2uHKw5BB2U31m8D41izE24\", \"payment_amounts\": \"1.00000000\", \"proposal_hashes\": \"{}\", \"type\":2}}",
        "11".repeat(32)
    );
    let obj = GovObject {
        data: data.into_bytes(),
        ..trigger(&fx).clone()
    };
    let hash = object::hash(&obj);
    for (cached, live) in
        [(event + 24, true), (event + 25, true), (event + 576, true), (event + 577, false)]
    {
        let mut set = SuperblockSet::new();
        assert_eq!(set.add_trigger(&obj, cached, &p), Ok(live), "{cached}");
        assert_eq!(set.get(&hash).map(|sb| sb.status), Some(Status::Valid));
    }
    let held = || {
        let mut set = SuperblockSet::new();
        assert_eq!(set.add_trigger(&obj, event - 1, &p), Ok(true));
        set
    };
    let mut states = BTreeMap::from([(hash, funded(&fx, TRIGGER, hash, 2))]);
    // Valid: kept to event + 576.
    let mut set = held();
    assert!(set.clean(event + 576, &mut states, 1000, &p).is_empty());
    assert_eq!(set.clean(event + 577, &mut states, 1000, &p), vec![hash]);
    // Executed: kept to event + cycle.
    let mut states = BTreeMap::from([(hash, funded(&fx, TRIGGER, hash, 2))]);
    let mut set = held();
    set.execute_best(&fx.list, event, &states, &p);
    assert_eq!(set.get(&hash).map(|sb| sb.status), Some(Status::Executed));
    assert!(set.clean(event + cycle, &mut states, 1000, &p).is_empty());
    assert_eq!(set.clean(event + cycle + 1, &mut states, 1000, &p), vec![hash]);
    assert!(states[&hash].expired);
}
