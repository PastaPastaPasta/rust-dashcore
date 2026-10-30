//! `CDKGSession::Init` (`l/dkgsession.cpp:90-155`).

use super::fixture::{base, init_with};
use super::*;
use crate::dkg::reference::{DebugUpdate, quorum, quorum_relay_members};

const TEST: LlmqType = LlmqType::LlmqTest;

#[test]
fn too_few_members_refuse_before_any_member_work() {
    let (members, _) = quorum(1, 1);
    let (session, topo, debug) = init_with(TEST, &members, Some(members[0].pro_tx_hash));
    let Some(InitRefusal::NotEnoughMembers {
        have,
        min,
        events,
    }) = session.err()
    else {
        panic!("a refusal");
    };
    assert_eq!((have, min), (1, 2));
    let lines: Vec<String> = events.iter().map(ToString::to_string).collect();
    assert_eq!(lines, ["Init -- not enough members (1 < 2), aborting init"]);
    assert!(events[0].record.is_some());
    assert_eq!(topo.calls, 0);
    assert!(debug.updates.is_empty());
}

#[test]
fn a_member_finds_its_index_and_asks_for_its_relay_members_once() {
    let (members, _) = quorum(3, 2);
    let me = members[1].pro_tx_hash;
    let (session, topo, debug) = init_with(TEST, &members, Some(me));
    let mut session = session.expect("enough members");
    assert_eq!(session.my_index(), Some(1));
    assert!(session.are_we_member());
    assert_eq!(topo.calls, 1);
    let pro_txs: Vec<ProTxHash> = members.iter().map(|m| m.pro_tx_hash).collect();
    assert_eq!(session.relay_members(), &quorum_relay_members(&pro_txs, &me, true));
    assert_eq!(
        debug.updates,
        vec![DebugUpdate::InitSession {
            base: base().hash,
            height: 48
        }]
    );
    assert_eq!(session.members(), members.as_slice(), "Core's order is kept");
    assert_eq!(session.member_index(&members[2].pro_tx_hash), Some(2));
    assert_eq!(session.member_ids()[2], *members[2].pro_tx_hash.as_bytes());
    assert!(!session.signs_legacy());
    let events = session.take_events();
    assert_eq!(events.last().map(|e| e.text.as_str()), Some("initialized as member. mns=3"));
}

#[test]
fn an_observer_has_no_index_relay_members_or_debug_status() {
    let (members, _) = quorum(3, 3);
    let (outsiders, _) = quorum(1, 4);
    for me in [None, Some(outsiders[0].pro_tx_hash), Some(ProTxHash::default())] {
        let (session, topo, debug) = init_with(TEST, &members, me);
        let mut session = session.expect("enough members");
        assert_eq!(session.my_index(), None);
        assert_eq!(topo.calls, 0);
        assert!(session.relay_members().is_empty());
        assert!(debug.updates.is_empty());
        assert_eq!(
            session.take_events().last().map(|e| e.text.clone()),
            Some("initialized as observer. mns=3".to_string())
        );
    }
}

#[test]
fn relay_members_follow_cores_ring() {
    let ids: Vec<ProTxHash> = (0..10u8).map(|i| ProTxHash::from([i; 32])).collect();
    // n = 10 from index 0: gaps 1, 2, 4.
    let outbound = quorum_relay_members(&ids, &ids[0], true);
    assert_eq!(outbound, [ids[1], ids[2], ids[4]].into_iter().collect());
    // With inbound: also the members whose ring reaches 0 (9 via 1, 8 via 2, 6 via 4).
    let both = quorum_relay_members(&ids, &ids[0], false);
    assert_eq!(both, [ids[1], ids[2], ids[4], ids[6], ids[8], ids[9]].into_iter().collect());
    // Two members: the other one; one member: nobody.
    assert_eq!(quorum_relay_members(&ids[..2], &ids[0], true), [ids[1]].into_iter().collect());
    assert!(quorum_relay_members(&ids[..1], &ids[0], true).is_empty());
}
