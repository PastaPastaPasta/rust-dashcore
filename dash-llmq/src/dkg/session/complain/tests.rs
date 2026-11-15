//! The complaint phase (LQ4a-2 T1): `VerifyAndComplain` with the connection check and
//! `SendComplaint`, and `ReceiveMessage` for a complaint.

use super::*;
use crate::dkg::bls::{BlsSchemeKind, verify_insecure};
use crate::dkg::conn::MIN_MASTERNODE_PROTO_VERSION;
use crate::dkg::reference::{DebugUpdate, TestMember};
use crate::dkg::session::fixture::{BLS, Round, TestEnv, feed, lines, member_session};
use crate::dkg::session::{DkgMsgKind, MemberStatusBit};

fn logged(session: &mut DkgSession<TestMember>) -> Vec<String> {
    lines(&session.take_events())
}

/// A snapshot with PoSe on in which every member of `round` but `except` is fully connected
/// and compliant.
fn connected(round: &Round, except: &[usize]) -> ConnSnapshot {
    let p = round.pro_txs();
    ConnSnapshot::connected((0..p.len()).filter(|i| !except.contains(i)).map(|i| p[i]))
}

#[test]
fn verify_and_complain_runs_once_after_the_contribution() {
    let round = Round::new(4);
    let mut session = member_session(&round.params, &round.members, Some(0), true);
    let mut env = TestEnv::new(&round.keys[0], 1);
    let conn = ConnSnapshot::default();
    assert_eq!(
        session.verify_and_complain(&conn, &mut env.env()).err(),
        Some(DkgFault::PhaseOrder)
    );
    let (mut session, mut env) = round.contributed(0, 0..4);
    assert!(session.verify_and_complain(&conn, &mut env.env()).expect("first call").is_none());
    assert_eq!(
        session.verify_and_complain(&conn, &mut env.env()).err(),
        Some(DkgFault::PhaseOrder)
    );
}

#[test]
fn an_observer_moves_on_without_a_complaint() {
    let round = Round::new(4);
    let mut session = member_session(&round.params, &round.members, None, true);
    let mut env = TestEnv::new(&round.keys[0], 1);
    session.adopt_contribution(Vec::new(), Vec::new()).expect("first stage");
    // Even with every member missing: an observer checks nothing.
    let conn = connected(&round, &[0, 1, 2, 3]);
    assert!(session.verify_and_complain(&conn, &mut env.env()).expect("in order").is_none());
    assert!(session.take_events().is_empty());
    assert!((0..4).all(|i| !session.is_bad(i) && !session.is_bad_connection(i)));
}

#[test]
fn a_silent_member_is_marked_and_voted_bad() {
    // Mutant: skip `MarkBadMember` for a member without a contribution.
    let round = Round::new(4);
    let (mut session, mut env) = round.contributed(0, [0, 1, 2]);
    let qc = session
        .verify_and_complain(&connected(&round, &[]), &mut env.env())
        .expect("in order")
        .expect("a complaint");
    let p3 = round.members[3].pro_tx_hash;
    assert!(session.is_bad(3));
    assert_eq!((0..4).map(|i| qc.bad_member(i)).collect::<Vec<_>>(), [false, false, false, true]);
    assert!((0..4).all(|i| !qc.complain_for(i)));
    assert_eq!(
        logged(&mut session),
        [
            "VerifyPendingContributions -- verified 3 pending contributions".into(),
            format!("VerifyAndComplain -- {p3} did not send any contribution"),
            "VerifyAndComplain -- verified contributions".into(),
            "SendComplaint -- sending complaint. badCount=1, complaintCount=0".into(),
        ]
    );
    assert!(env.debug.updates.ends_with(&[
        DebugUpdate::Member(3, MemberStatusBit::Bad),
        DebugUpdate::Session(SessionStatusBit::SentComplaint),
    ]));
    // Signed by our operator key over Core's sign hash, under the session's scheme.
    let pk = round.members[0].operator_pk.clone();
    assert!(verify_insecure(qc.sig(), &pk, qc.sign_hash(), BlsSchemeKind::Basic));
    let read = Complaint::read(&qc.payload(BLS.scheme()), &round.params, BLS).expect("reads");
    assert_eq!(read.session_hash(), qc.session_hash());
}

#[test]
fn each_member_is_either_voted_bad_or_complained_about() {
    // Mutant: `if` for `else if`, which would complain about a bad member too.
    let round = Round::new(4);
    let (mut session, mut env) = round.contributed(0, 0..4);
    for i in [1, 2] {
        session.member_state[i].we_complain = true;
    }
    session.member_state[1].bad = true;
    session.member_state[3].bad_connection = true;
    let qc = session
        .verify_and_complain(&ConnSnapshot::default(), &mut env.env())
        .expect("in order")
        .expect("a complaint");
    assert_eq!((0..4).map(|i| qc.bad_member(i)).collect::<Vec<_>>(), [false, true, false, true]);
    assert_eq!((0..4).map(|i| qc.complain_for(i)).collect::<Vec<_>>(), [false, false, true, false]);
    assert_eq!(
        logged(&mut session).last().map(String::as_str),
        Some("SendComplaint -- sending complaint. badCount=2, complaintCount=1")
    );
}

#[test]
fn nothing_to_complain_about_sends_and_logs_nothing() {
    let round = Round::new(4);
    let (mut session, mut env) = round.contributed(0, 0..4);
    let before = env.debug.updates.len();
    assert!(
        session.verify_and_complain(&connected(&round, &[]), &mut env.env()).unwrap().is_none()
    );
    assert_eq!(
        logged(&mut session),
        [
            "VerifyPendingContributions -- verified 4 pending contributions",
            "VerifyAndComplain -- verified contributions"
        ]
    );
    assert_eq!(env.debug.updates.len(), before, "no SentComplaint");
}

#[test]
fn bad_connections_are_voted_bad_with_pose_on_only() {
    // Mutant: run the connection check whatever `IsQuorumPoseEnabled` says.
    let round = Round::new(4);
    let mut conn = connected(&round, &[0, 2]);
    conn.outbound_failed_too_many.insert(round.members[3].pro_tx_hash);
    let (p2, p3) = (round.members[2].pro_tx_hash, round.members[3].pro_tx_hash);

    let (mut session, mut env) = round.contributed(0, 0..4);
    let qc = session.verify_and_complain(&conn, &mut env.env()).unwrap().expect("a complaint");
    assert_eq!((0..4).map(|i| qc.bad_member(i)).collect::<Vec<_>>(), [false, false, true, true]);
    assert!(!session.is_bad(2) && session.is_bad_connection(2), "a bad connection is not bad");
    let got = logged(&mut session);
    assert_eq!(
        got,
        [
            "VerifyPendingContributions -- verified 4 pending contributions".to_owned(),
            "VerifyAndComplain -- verified contributions".to_owned(),
            format!(
                "VerifyConnectionAndMinProtoVersions -- {p2} is not connected to us, badConnection=1"
            ),
            format!(
                "VerifyConnectionAndMinProtoVersions -- {p3} failed to connect to it too many times"
            ),
            "SendComplaint -- sending complaint. badCount=2, complaintCount=0".to_owned(),
        ],
        "we are not checked: member 0 is missing from the nodes too"
    );

    conn.pose_enabled = false;
    let (mut session, mut env) = round.contributed(0, 0..4);
    assert!(session.verify_and_complain(&conn, &mut env.env()).unwrap().is_none());
    assert!((0..4).all(|i| !session.is_bad_connection(i)));
}

#[test]
fn each_connection_fault_is_logged_in_cores_words() {
    let round = Round::new(4);
    let mut conn = connected(&round, &[]);
    conn.nodes[1].version = MIN_MASTERNODE_PROTO_VERSION - 1;
    conn.nodes[2].relay_txs = false;
    conn.nodes[3].services = 0;
    conn.platform_banned.insert(round.members[3].pro_tx_hash);
    let (mut session, mut env) = round.contributed(0, 0..4);
    session.verify_and_complain(&conn, &mut env.env()).unwrap().expect("a complaint");
    let p: Vec<_> = round.pro_txs();
    let v = MIN_MASTERNODE_PROTO_VERSION;
    let f = "VerifyConnectionAndMinProtoVersions";
    assert_eq!(
        logged(&mut session)[2..6],
        [
            format!("{f} -- {} does not have min proto version {v} (has {})", p[1], v - 1),
            format!("{f} -- {} does not relay transactions", p[2]),
            format!("{f} -- {} does not serve compact block filters", p[3]),
            format!("{f} -- {} is Platform PoSe banned", p[3]),
        ]
    );
}

#[test]
fn a_complaint_counts_votes_and_complaints_per_member() {
    // Mutants: no `badMemberVotes` entry; no `complaintsFromOthers` entry; the complainer's
    // index instead of the member's in the debug status.
    let round = Round::new(4);
    let (mut session, mut env) = round.contributed(0, 0..4);
    session.verify_and_complain(&ConnSnapshot::default(), &mut env.env()).unwrap();
    session.take_events();
    let qc = round.complaint(2, &[0, 3], &[0, 1]);
    let p: Vec<_> = round.pro_txs();
    let got =
        feed(&mut session, &mut env, DkgMsgKind::Complaint, 0, &[(2, qc.payload(BLS.scheme()))]);
    assert_eq!(
        session.complaint(qc.session_hash()).map(Complaint::session_hash),
        Some(qc.session_hash())
    );
    let (voted, complained) = ([true, false, false, true], [true, true, false, false]);
    for i in 0..4 {
        let from_2 = |yes: bool| {
            if yes {
                vec![p[2]]
            } else {
                vec![]
            }
        };
        let votes: Vec<_> = session.bad_member_votes(i).iter().copied().collect();
        assert_eq!(votes, from_2(voted[i]), "votes on {i}");
        let open: Vec<_> = session.complaints_from_others(i).iter().copied().collect();
        assert_eq!(open, from_2(complained[i]), "complaints on {i}");
        assert_eq!(session.member_state[i].someone_complain, complained[i]);
    }
    assert!(env.debug.updates.ends_with(&[
        DebugUpdate::Member(2, MemberStatusBit::ReceivedComplaint),
        DebugUpdate::ComplaintFrom(0, 2),
        DebugUpdate::ComplaintFrom(1, 2),
    ]));
    let f = "ReceiveMessage";
    assert_eq!(
        got,
        [
            format!("{f} -- received complaint from {}", p[2]),
            format!("{f} -- {} voted for {} to be bad", p[2], p[0]),
            format!("{f} -- {} voted for us to be bad", p[2]),
            format!("{f} -- {} complained about us", p[2]),
            format!("{f} -- {} voted for {} to be bad", p[2], p[3]),
            format!("{f} -- received and relayed complaint. received=1"),
        ]
    );
}

#[test]
fn a_second_complaint_marks_its_sender_bad_unprocessed() {
    // Mutant: process the second complaint (its votes would count).
    let round = Round::new(4);
    let (mut session, mut env) = round.contributed(0, 0..4);
    session.verify_and_complain(&ConnSnapshot::default(), &mut env.env()).unwrap();
    session.take_events();
    let first = round.complaint(2, &[3], &[]);
    let second = round.complaint(2, &[1], &[]);
    let raw = |qc: &Complaint| (2, qc.payload(BLS.scheme()));
    let got = feed(&mut session, &mut env, DkgMsgKind::Complaint, 0, &[raw(&first), raw(&second)]);
    assert!(session.is_bad(2));
    assert_eq!(session.bad_member_votes(3).len(), 1);
    assert!(session.bad_member_votes(1).is_empty(), "the second is relayed, not processed");
    let p2 = round.members[2].pro_tx_hash;
    assert!(
        got.contains(&format!("ReceiveMessage -- {p2} did send multiple complaints")),
        "{got:?}"
    );
    assert_eq!(got.iter().filter(|l| l.contains("received and relayed complaint")).count(), 1);
}
