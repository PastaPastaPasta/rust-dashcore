//! The justification phase (LQ4a-2 T1): `VerifyAndJustify` and `SendJustification` with their
//! draws, `ReceiveMessage` for a justification, and `VerifyAndCommit`'s marking.

use super::*;
use crate::dkg::bls::{BlsPkValue, BlsSchemeKind, member_id, verify_contribution_share};
use crate::dkg::conn::ConnSnapshot;
use crate::dkg::reference::{DebugUpdate, StoreRow, TestMember};
use crate::dkg::rng::{Draw, ReplayRng};
use crate::dkg::session::fixture::{BLS, Round, TestEnv, feed, lines, member_session};
use crate::dkg::session::{DkgMsgKind, MemberStatusBit, Sender};

fn logged(session: &mut DkgSession<TestMember>) -> Vec<String> {
    lines(&session.take_events())
}

/// Member `me`'s session past `VerifyAndComplain`, with every contribution received and the
/// complaints `(from, bad, complain_for)` received after it.
fn complained(
    round: &Round,
    me: usize,
    complaints: &[(usize, &[usize], &[usize])],
) -> (DkgSession<TestMember>, TestEnv) {
    let n = round.members.len();
    let (mut session, mut env) = round.contributed(me, 0..n);
    session.verify_and_complain(&ConnSnapshot::default(), &mut env.env()).expect("in order");
    let raws: Vec<(usize, Vec<u8>)> = complaints
        .iter()
        .map(|(from, bad, cf)| (*from, round.complaint(*from, bad, cf).payload(BLS.scheme())))
        .collect();
    feed(&mut session, &mut env, DkgMsgKind::Complaint, me, &raws);
    session.take_events();
    (session, env)
}

#[test]
fn verify_and_justify_runs_once_after_the_complaint() {
    let round = Round::new(4);
    let (mut session, mut env) = round.contributed(0, 0..4);
    assert_eq!(session.verify_and_justify(&mut env.env()).err(), Some(DkgFault::PhaseOrder));
    let (mut session, mut env) = complained(&round, 0, &[]);
    assert!(session.verify_and_justify(&mut env.env()).expect("first call").is_none());
    assert_eq!(session.verify_and_justify(&mut env.env()).err(), Some(DkgFault::PhaseOrder));
    // An observer moves through the stages too.
    let mut observer = member_session(&round.params, &round.members, None, true);
    observer.adopt_contribution(Vec::new(), Vec::new()).unwrap();
    observer.verify_and_complain(&ConnSnapshot::default(), &mut env.env()).unwrap();
    assert!(observer.verify_and_justify(&mut env.env()).expect("in order").is_none());
}

#[test]
fn bad_votes_at_the_threshold_mark_a_member_bad() {
    // Mutant: `>` for `>=`.
    let round = Round::new(4);
    let threshold = usize::try_from(round.params.dkg_bad_votes_threshold).unwrap();
    assert_eq!(threshold, 2);
    let (mut session, mut env) = complained(&round, 0, &[(1, &[3], &[]), (2, &[3, 1], &[])]);
    assert!(session.verify_and_justify(&mut env.env()).unwrap().is_none());
    assert!(session.is_bad(3), "two votes of two");
    assert!(!session.is_bad(1), "one vote of two");
    let p3 = round.members[3].pro_tx_hash;
    assert_eq!(
        logged(&mut session),
        [format!("VerifyAndJustify -- {p3} marked as bad as 2 other members voted for this")]
    );
    assert!(env.debug.updates.ends_with(&[DebugUpdate::Member(3, MemberStatusBit::Bad)]));
}

#[test]
fn two_complaints_mark_their_sender_bad_before_any_justification() {
    let round = Round::new(4);
    let (mut session, mut env) = complained(&round, 0, &[(1, &[], &[0]), (1, &[], &[2])]);
    // The preamble already marked member 1 bad, so VerifyAndJustify skips it.
    assert!(session.is_bad(1));
    assert!(session.verify_and_justify(&mut env.env()).unwrap().is_none());
    assert!(logged(&mut session).is_empty());
    // Two recorded complaints on a member not yet bad: "sent multiple complaints".
    let (mut session, mut env) = complained(&round, 0, &[(1, &[], &[0])]);
    session.member_state[1].complaints.insert([7; 32]);
    assert!(session.verify_and_justify(&mut env.env()).unwrap().is_none());
    assert!(session.is_bad(1));
    let p1 = round.members[1].pro_tx_hash;
    assert_eq!(
        logged(&mut session),
        [format!("VerifyAndJustify -- {p1} sent multiple complaints")]
    );
}

#[test]
fn we_justify_only_the_complaints_about_us_with_our_true_shares() {
    // Mutants: justify every complainer; reveal a share other than `m_sk_contributions[i]`.
    let round = Round::new(4);
    let (mut session, mut env) =
        complained(&round, 0, &[(3, &[], &[0]), (2, &[], &[1]), (1, &[], &[0, 2])]);
    let qj = session.verify_and_justify(&mut env.env()).unwrap().expect("a justification");
    let keys: Vec<(u32, BlsSkValue)> = qj.keys().to_vec();
    assert_eq!(keys, [(1, round.shares[0][1].clone()), (3, round.shares[0][3].clone())]);
    let vvec: &[BlsPkValue] = round.contributions[0].vvec();
    for (i, sk) in &keys {
        let id = member_id(&round.members[*i as usize].pro_tx_hash);
        assert!(verify_contribution_share(&id, vvec, sk, BlsSchemeKind::Basic));
    }
    let p: Vec<_> = round.pro_txs();
    let f = "SendJustification";
    assert_eq!(
        logged(&mut session),
        [
            format!("{f} -- sending justification for 2 members"),
            format!("{f} -- justifying for {}", p[1]),
            format!("{f} -- justifying for {}", p[3]),
        ]
    );
    assert!(
        env.debug.updates.ends_with(&[DebugUpdate::Session(SessionStatusBit::SentJustification)])
    );
    let read = Justification::read(&qj.payload(BLS.scheme()), &round.params, BLS).expect("reads");
    assert_eq!(read.session_hash(), qj.session_hash());
}

#[test]
fn the_lie_draws_precede_the_omit_draw_and_skip_ourselves() {
    // Mutants: draw JUSTIFY_OMIT first; draw JUSTIFY_LIE for our own index.
    let round = Round::new(4);
    // Member 0 complains about itself (its own share did not decrypt), and member 2 about it.
    let (mut session, mut env) = complained(&round, 0, &[(0, &[], &[0]), (2, &[], &[0])]);
    let lie = BlsSkValue::from_canonical(&[0x11; 32], BlsSchemeKind::Basic).expect("a key");
    env.rng = Box::new(ReplayRng::new([
        Draw::Bool(SimError::JustifyLie, 100, true),
        Draw::Key(KeyDraw::JustifyLie(2), lie.clone()),
        Draw::Bool(SimError::JustifyOmit, 100, false),
    ]));
    env.sim.set(SimError::JustifyLie, 100).unwrap();
    env.sim.set(SimError::JustifyOmit, 100).unwrap();
    let qj = session.verify_and_justify(&mut env.env()).unwrap().expect("not omitted");
    assert_eq!(qj.keys(), [(0, round.shares[0][0].clone()), (2, lie)]);
    let p2 = round.members[2].pro_tx_hash;
    assert!(logged(&mut session).contains(&format!("SendJustification -- lying for {p2}")));

    let (mut session, mut env) = complained(&round, 0, &[(2, &[], &[0])]);
    env.rng = Box::new(ReplayRng::new([Draw::Bool(SimError::JustifyOmit, 100, true)]));
    env.sim.set(SimError::JustifyOmit, 100).unwrap();
    let before = env.debug.updates.len();
    assert!(session.verify_and_justify(&mut env.env()).unwrap().is_none());
    assert_eq!(
        logged(&mut session).last().map(String::as_str),
        Some("SendJustification -- omitting")
    );
    assert_eq!(env.debug.updates.len(), before, "no SentJustification");
}

/// Member 0 after phase 3 of a round in which member 1 lied to it and member 2, and every
/// member complained: 0 and 2 about member 1, 3 about member 2.
fn justifying(round: &Round) -> (DkgSession<TestMember>, TestEnv) {
    let (mut session, mut env) =
        complained(round, 0, &[(0, &[], &[1]), (2, &[], &[1]), (3, &[], &[2])]);
    session.member_state[1].we_complain = true;
    session.verify_and_justify(&mut env.env()).unwrap();
    session.take_events();
    (session, env)
}

fn feed_justification(
    round: &Round,
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    qj: &Justification,
) -> Vec<String> {
    let from = round.members.iter().position(|m| m.pro_tx_hash == *qj.pro_tx_hash()).unwrap();
    feed(session, env, DkgMsgKind::Justification, 0, &[(from, qj.payload(BLS.scheme()))])
}

#[test]
fn a_true_justification_closes_the_complaints_and_gives_us_our_share() {
    // Mutants: keep `complaintsFromOthers`; keep `weComplain`; skip `qdkg_S`.
    let round = Round::new(4);
    let (mut session, mut env) = justifying(&round);
    let qj =
        round.justification(1, &[(0, round.shares[1][0].clone()), (2, round.shares[1][2].clone())]);
    let got = feed_justification(&round, &mut session, &mut env, &qj);
    assert!(session.complaints_from_others(1).is_empty());
    assert!(!session.we_complain(1));
    assert!(!session.is_bad(1));
    assert_eq!(session.member_state[1].received_sk.as_ref(), Some(&round.shares[1][0]));
    let p: Vec<_> = round.pro_txs();
    assert!(matches!(
        env.store.rows.last(),
        Some((_, _, StoreRow::Sk { who, sk })) if *who == p[1] && *sk == round.shares[1][0]
    ));
    assert_eq!(
        session.justification(qj.session_hash()).map(Justification::session_hash),
        Some(qj.session_hash())
    );
    let f = "ReceiveMessage";
    assert_eq!(
        got,
        [
            format!("{f} -- received justification from {}", p[1]),
            format!("{f} --   {} justified for {}", p[1], p[0]),
            format!("{f} --   {} justified for {}", p[1], p[2]),
            format!("{f} -- verified justification: received=1/2"),
        ]
    );
    // Member 2's complaint about it stays open until its own justification.
    assert_eq!(session.complaints_from_others(2).len(), 1);
}

#[test]
fn a_false_share_marks_the_justifier_bad() {
    // Mutant: ignore a failed share check.
    let round = Round::new(4);
    let (mut session, mut env) = justifying(&round);
    let wrong = round.shares[3][0].clone();
    let qj = round.justification(1, &[(0, wrong), (2, round.shares[1][2].clone())]);
    let got = feed_justification(&round, &mut session, &mut env, &qj);
    assert!(session.is_bad(1));
    assert!(session.we_complain(1));
    let p: Vec<_> = round.pro_txs();

    assert_eq!(
        got[1],
        format!("ReceiveMessage --   {} did send an invalid justification for {}", p[1], p[0])
    );
    assert_eq!(got[2], format!("ReceiveMessage --   {} justified for {}", p[1], p[2]));
    assert_eq!(session.complaints_from_others(1).iter().copied().collect::<Vec<_>>(), [p[0]]);
}

#[test]
fn a_true_share_for_us_after_a_false_one_is_still_ours() {
    // Core's loop goes on past the bad mark (`l/dkgsession.cpp:462-480`). Mutant: no `qdkg_S`
    // and no `weComplain` clear once the justifier is bad.
    let round = Round::new(4);
    let (mut session, mut env) = justifying(&round);
    let wrong = round.shares[3][2].clone();
    let ours = round.shares[1][0].clone();
    let qj = round.justification(1, &[(2, wrong), (0, ours.clone())]);
    let rows = env.store.rows.len();
    let got = feed_justification(&round, &mut session, &mut env, &qj);
    let p: Vec<_> = round.pro_txs();
    assert_eq!(
        got[1..3],
        [
            format!("ReceiveMessage --   {} did send an invalid justification for {}", p[1], p[2]),
            format!("ReceiveMessage --   {} justified for {}", p[1], p[0]),
        ]
    );
    assert!(session.is_bad(1));
    assert!(!session.we_complain(1));
    assert!(matches!(
        &env.store.rows[rows..],
        [(_, _, StoreRow::Sk { who, sk })] if *who == p[1] && *sk == ours
    ));
    assert_eq!(session.complaints_from_others(1).iter().copied().collect::<Vec<_>>(), [p[2]]);
}

#[test]
fn an_unasked_justification_marks_the_justifier_bad_unverified() {
    // Mutant: drop the "didn't complain" check.
    let round = Round::new(4);
    let (mut session, mut env) = justifying(&round);
    let qj =
        round.justification(1, &[(0, round.shares[1][0].clone()), (3, round.shares[1][3].clone())]);
    let got = feed_justification(&round, &mut session, &mut env, &qj);
    assert!(session.is_bad(1));
    let p: Vec<_> = round.pro_txs();
    assert_eq!(
        got[1..],
        [format!(
            "ReceiveMessage -- got justification from {} for {} even though he didn't complain",
            p[1], p[3]
        )]
    );
    assert_eq!(session.complaints_from_others(1).len(), 2, "nothing closed");
}

#[test]
fn a_bad_justifier_is_relayed_but_not_heard() {
    let round = Round::new(4);
    let (mut session, mut env) = justifying(&round);
    session.member_state[1].bad = true;
    let qj =
        round.justification(1, &[(0, round.shares[1][0].clone()), (2, round.shares[1][2].clone())]);
    let outcome = session.process_batch(
        DkgMsgKind::Justification,
        &[(Sender::Peer(1usize), &qj.payload(BLS.scheme())[..])],
        &mut env.env(),
    );
    assert_eq!(outcome.relay.len(), 1);
    assert_eq!(session.complaints_from_others(1).len(), 2);
    assert!(session.we_complain(1));
    let p1 = round.members[1].pro_tx_hash;
    assert_eq!(
        lines(&outcome.events),
        [format!("ReceiveMessage -- received justification from {p1}")]
    );
}

#[test]
fn a_justification_against_no_received_vector_fails_its_shares() {
    let round = Round::new(4);
    let (mut session, mut env) = justifying(&round);
    session.member_state[1].received_vvec = None;
    let qj =
        round.justification(1, &[(0, round.shares[1][0].clone()), (2, round.shares[1][2].clone())]);
    feed_justification(&round, &mut session, &mut env, &qj);
    assert!(session.is_bad(1));
}

#[test]
fn open_complaints_mark_their_members_bad_at_commit() {
    // Mutant: skip the marking (member 1 stays valid).
    let round = Round::new(4);
    let (mut session, mut env) = justifying(&round);
    session.member_state[3].bad = true;
    session.mark_open_complaints(&mut env.debug);
    assert_eq!(session.valid_members(), [true, false, false, false]);
    let p: Vec<_> = round.pro_txs();
    let f = "VerifyAndCommit";
    assert_eq!(
        logged(&mut session),
        [
            format!("{f} -- verification result:"),
            format!("{f} --   members previously determined as bad:"),
            format!("{f} --     {}", p[3]),
            format!("{f} --   members with open complaints and now marked as bad:"),
            format!("{f} --     {}", p[1]),
            format!("{f} --     {}", p[2]),
        ]
    );
    let (mut session, mut env) = complained(&round, 0, &[]);
    session.mark_open_complaints(&mut env.debug);
    assert!(logged(&mut session).is_empty());
}
