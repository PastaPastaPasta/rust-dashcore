//! The commitment phase (LQ4b-1 T1): `ReceiveMessage` for a premature commitment through the
//! batch, `GetPrematureCommitment`, the receiver column of the lieType table (LQ4 §4.4), and
//! `VerifyAndCommit` with `SendCommitment`, the liar's column.

use super::*;
use crate::dkg::bls::BlsCtx;
use crate::dkg::bls::{
    BlsSigValue, BlsSkValue, aggregate_secret_keys, make_new_key, secret_to_public, sign_stand_in,
};
use crate::dkg::reference::{DebugUpdate, StoreRead, TestMember, quorum};
use crate::dkg::rng::{CoreRng, DkgRng, Draw, ReplayRng, SeededRng};
use crate::dkg::session::fixture::{
    BLS, Round, TestEnv, base, batch_of, lie_draws, lie_loopback_line, lines, member_session,
};
use crate::dkg::session::{BatchOutcome, DkgReject, DkgStore, Misbehaviour, OperatorKey, Sender};
use crate::dkg::wire::LlmqType;

const N: usize = 4;
const F: &str = "ReceiveMessage";

/// How a sender's premature commitment departs from the honest one: Core's `commit-lie`
/// lieTypes (`a/dkgsession.cpp:539-570`), and the forced variants whose perturbed bytes still
/// decode (LQ4 §4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lie {
    Honest,
    /// 0: `quorumPublicKey` of a fresh key, signed as it stands.
    FreshKey,
    /// 1: the first vvecHash byte +1, signed as it stands.
    VvecHash,
    /// 2: the first byte of the commitment hash +1 before both signatures.
    SignHash,
    /// 3: byte 5 of `sig` +1, then `SetBytes`.
    Sig,
    /// 3, forced: `sig` a valid point that does not verify.
    SigForced,
    /// 4: byte 5 of `quorumSig` +1, then `SetBytes`.
    QuorumSig,
    /// 4, forced: `quorumSig` a valid point that does not verify.
    QuorumSigForced,
}

/// Byte 5 of `sig` +1 then `SetBytes`, as Core's lie does: the invalid object when the bytes
/// are no canonical point, which is all but a negligible fraction of draws.
fn perturbed(sig: &BlsSigValue) -> BlsSigValue {
    let flag = BLS.scheme();
    let mut bytes = sig.to_bytes(flag);
    bytes[5] = bytes[5].wrapping_add(1);
    let value = BlsSigValue::from_canonical(&bytes, flag).unwrap_or_else(BlsSigValue::invalid);
    assert!(!value.is_valid(), "the negligible case: pick another key");
    value
}

/// A round of `N` members whose every contribution member `me` received.
struct Commit {
    round: Round,
    quorum_vvec: Vec<BlsPkValue>,
}

impl Commit {
    fn new() -> Self {
        let round = Round::new(N);
        let vvecs: Vec<&[BlsPkValue]> = round.contributions.iter().map(|c| c.vvec()).collect();
        let quorum_vvec =
            build_quorum_verification_vector(&vvecs, BLS.scheme()).expect("honest vectors");
        Self {
            round,
            quorum_vvec,
        }
    }

    /// Member `me`'s session with every contribution received (so every `qdkg_V` row
    /// written), its lines taken, and its store answering with the round's members.
    fn receiver(&self, me: usize) -> (DkgSession<TestMember>, TestEnv) {
        let (mut session, mut env) = self.round.contributed(me, 0..N);
        session.take_events();
        env.store.members = self.round.pro_txs();
        env.store.reads.clear();
        (session, env)
    }

    /// Member `from`'s premature commitment over every member valid, with `lie`.
    fn pcommit(&self, from: usize, lie: Lie) -> PrematureCommitment {
        let flag = BLS.scheme();
        let mut pk = self.quorum_vvec[0].clone();
        let mut hash = vvec_hash(&self.quorum_vvec, flag);
        match lie {
            Lie::FreshKey => pk = secret_to_public(&lie_key()),
            Lie::VvecHash => hash[0] = hash[0].wrapping_add(1),
            _ => {}
        }
        let pro_tx = &self.round.members[from].pro_tx_hash;
        let qc = PrematureCommitment::unsigned(
            LlmqType::LlmqTest,
            &base().hash,
            pro_tx,
            &[true; N],
            &pk,
            &hash,
            flag,
        );
        let mut sign_hash = *qc.sign_hash();
        if lie == Lie::SignHash {
            sign_hash[0] = sign_hash[0].wrapping_add(1);
        }
        let sk_share = self.sk_share(from);
        let mut sig = self.round.keys[from].sign(&sign_hash, false);
        let mut quorum_sig = sign_stand_in(&sk_share, &sign_hash, false);
        match lie {
            Lie::Sig => sig = perturbed(&sig),
            Lie::SigForced => sig = self.round.keys[(from + 1) % N].sign(&sign_hash, false),
            Lie::QuorumSig => quorum_sig = perturbed(&quorum_sig),
            Lie::QuorumSigForced => quorum_sig = self.round.keys[from].sign(&sign_hash, false),
            _ => {}
        }
        qc.with_quorum_sig(quorum_sig, flag).with_sig(sig, flag)
    }
}

/// `msgs` into the session as one batch of premature commitments, each `(peer, message)`.
fn process(
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    msgs: &[(usize, &PrematureCommitment)],
) -> BatchOutcome<usize> {
    let raws: Vec<(Sender<usize>, Vec<u8>)> =
        msgs.iter().map(|(peer, qc)| (Sender::Peer(*peer), qc.payload(BLS.scheme()))).collect();
    let batch: Vec<(Sender<usize>, &[u8])> = raws.iter().map(|(s, r)| (*s, &r[..])).collect();
    session.process_batch(DkgMsgKind::PrematureCommitment, &batch, &mut env.env())
}

fn relayed(outcome: &BatchOutcome<usize>) -> Vec<[u8; 32]> {
    outcome.relay.iter().map(|r| r.inv.hash).collect()
}

/// The receiver drops the `[0]` member's row so that it cannot build the vector.
fn without_vvec_of_member_0(env: &mut TestEnv, round: &Round) {
    let p0 = round.members[0].pro_tx_hash;
    env.store.rows.retain(|(_, _, row)| {
        !matches!(row, crate::dkg::reference::StoreRow::Vvec { who, .. } if *who == p0)
    });
}

#[test]
fn an_honest_commitment_is_verified_recorded_and_relayed() {
    // Mutants: no relay; no `validCommitments` entry; the debug bit left unset; the count over
    // contributions.
    let c = Commit::new();
    let (mut session, mut env) = c.receiver(0);
    let qc = c.pcommit(1, Lie::Honest);
    let out = process(&mut session, &mut env, &[(1, &qc)]);
    assert!(out.misbehaving.is_empty());
    assert_eq!(relayed(&out), [*qc.session_hash()]);
    assert_eq!(out.relay[0].inv.kind, DkgMsgKind::PrematureCommitment);
    let p1 = c.round.members[1].pro_tx_hash;
    assert_eq!(
        lines(&out.events),
        [
            format!("{F} -- received premature commitment from {p1}. validMembers=4"),
            format!("{F} -- verified premature commitment. received=1/4"),
        ]
    );
    assert_eq!(
        env.debug.updates.last(),
        Some(&DebugUpdate::Member(1, MemberStatusBit::ReceivedPrematureCommitment))
    );
    let served = session.get_premature_commitment(qc.session_hash()).expect("valid");
    assert_eq!(served.payload(BLS.scheme()), qc.payload(BLS.scheme()));
    // The count is of members with a commitment: a second sender makes 2.
    let out = process(&mut session, &mut env, &[(2, &c.pcommit(2, Lie::Honest))]);
    assert!(lines(&out.events)[1].ends_with("received=2/4"), "{:?}", out.events);
}

#[test]
fn the_members_access_precedes_every_row_read() {
    // Mutant: the rows read before the members (or without them).
    let c = Commit::new();
    let (mut session, mut env) = c.receiver(0);
    process(&mut session, &mut env, &[(1, &c.pcommit(1, Lie::Honest))]);
    let p = c.round.pro_txs();
    let mut want = vec![StoreRead::Members];
    for who in &p {
        want.extend([StoreRead::Vvec(*who), StoreRead::Sk(*who)]);
    }
    assert_eq!(env.store.reads, want);
    // Each later Receive accesses the members again; the cache answers the rows.
    process(&mut session, &mut env, &[(2, &c.pcommit(2, Lie::Honest))]);
    want.push(StoreRead::Members);
    assert_eq!(env.store.reads, want);
    assert_eq!(env.store.member_accesses(), 2);
    // A message the batch drops makes no access.
    let flag = BLS.scheme();
    let bad = c.pcommit(3, Lie::Honest).with_sig(BlsSigValue::invalid(), flag);
    process(&mut session, &mut env, &[(3, &bad)]);
    assert_eq!(env.store.member_accesses(), 2);
}

#[test]
fn each_failed_check_drops_the_commitment_without_relay_or_score() {
    // Mutants: each check skipped, or relayed after it fails.
    let c = Commit::new();
    for (lie, text) in [
        (Lie::FreshKey, "calculated quorum public key does not match"),
        (Lie::VvecHash, "calculated quorum vvec hash does not match"),
        (Lie::QuorumSigForced, "failed to verify quorumSig"),
    ] {
        let (mut session, mut env) = c.receiver(0);
        let qc = c.pcommit(1, lie);
        let out = process(&mut session, &mut env, &[(1, &qc)]);
        assert!(out.misbehaving.is_empty(), "{lie:?}");
        assert!(out.relay.is_empty(), "{lie:?}");
        let got = lines(&out.events);
        assert_eq!(got.len(), 2, "{lie:?}: {got:?}");
        assert_eq!(got[1], format!("{F} -- {text}"), "{lie:?}");
        // Recorded all the same, and counted for the next PreVerify, but never served.
        assert!(session.premature_commitments.contains_key(qc.session_hash()));
        assert_eq!(session.member_state[1].premature_commitments.len(), 1);
        assert!(session.get_premature_commitment(qc.session_hash()).is_none(), "{lie:?}");
        assert!(session.valid_commitments.is_empty());
        assert!(
            !env.debug
                .updates
                .contains(&DebugUpdate::Member(1, MemberStatusBit::ReceivedPrematureCommitment)),
            "{lie:?}"
        );
    }
}

#[test]
fn a_public_key_share_that_does_not_build_drops_the_commitment() {
    // `PublicKeyShare` fails on a vector of one key; rows of one key each build such a vector.
    // Mutant: the share's validity unchecked (the quorumSig then fails instead).
    let c = Commit::new();
    let (mut session, mut env) = c.receiver(0);
    let flag = BLS.scheme();
    env.store.rows.clear();
    let keys: Vec<BlsPkValue> =
        (0..N).map(|i| secret_to_public(&make_new_key(&mut SeededRng::new(i as u64)))).collect();
    for (i, who) in c.round.pro_txs().iter().enumerate() {
        env.store.write_vvec(LlmqType::LlmqTest, &base().hash, who, &keys[i..=i]);
    }
    let rows: Vec<&[BlsPkValue]> = keys.iter().map(std::slice::from_ref).collect();
    let quorum_vvec = build_quorum_verification_vector(&rows, flag).expect("one key each");
    assert_eq!(quorum_vvec.len(), 1);
    let pro_tx = &c.round.members[1].pro_tx_hash;
    let hash = vvec_hash(&quorum_vvec, flag);
    let qc = PrematureCommitment::unsigned(
        LlmqType::LlmqTest,
        &base().hash,
        pro_tx,
        &[true; N],
        &quorum_vvec[0],
        &hash,
        flag,
    );
    let sig = c.round.keys[1].sign(qc.sign_hash(), false);
    let qc = qc.with_quorum_sig(sig.clone(), flag).with_sig(sig, flag);
    let out = process(&mut session, &mut env, &[(1, &qc)]);
    assert!(out.misbehaving.is_empty() && out.relay.is_empty());
    assert_eq!(lines(&out.events)[1], format!("{F} -- failed to calculate public key share"));
    assert!(session.get_premature_commitment(qc.session_hash()).is_none());
}

#[test]
fn a_missing_vector_row_accepts_the_commitment_unverified() {
    // Rc.5 :589-593. Mutants: a missing row taken as a reject; the line in the record.
    let c = Commit::new();
    let (mut session, mut env) = c.receiver(2);
    without_vvec_of_member_0(&mut env, &c.round);
    let qc = c.pcommit(1, Lie::Honest);
    let out = process(&mut session, &mut env, &[(1, &qc)]);
    assert!(out.misbehaving.is_empty());
    assert_eq!(relayed(&out), [*qc.session_hash()]);
    let (p0, p1) = (c.round.members[0].pro_tx_hash, c.round.members[1].pro_tx_hash);
    let block = base().hash;
    assert_eq!(
        lines(&out.events),
        [
            format!(
                "GetVerifiedContributions -- this node does not have vvec for llmq=100 \
                 block={block} protx={p0}"
            ),
            format!("{F} -- received premature commitment from {p1}. validMembers=4"),
            format!(
                "{F} -- failed to build quorum verification vector. skipping full verification"
            ),
            format!("{F} -- verified premature commitment. received=1/4"),
        ]
    );
    // Core's `LogPrint` writes it on its own, before the call's record.
    assert_eq!(out.events[0].record, None);
    assert!(out.events[1].record.is_some());
    // The store stopped at the missing row.
    assert_eq!(env.store.reads, [StoreRead::Members, StoreRead::Vvec(p0)]);
    assert!(session.get_premature_commitment(qc.session_hash()).is_some());
}

#[test]
fn vectors_that_do_not_build_accept_the_commitment_unverified() {
    // Rows of different lengths: `VerifyVerificationVectors` refuses them.
    let c = Commit::new();
    let (mut session, mut env) = c.receiver(0);
    let p0 = c.round.members[0].pro_tx_hash;
    let short = c.round.contributions[0].vvec()[..1].to_vec();
    env.store.rows.clear();
    for (i, who) in c.round.pro_txs().iter().enumerate() {
        let vvec = if *who == p0 {
            short.clone()
        } else {
            c.round.contributions[i].vvec().to_vec()
        };
        env.store.write_vvec(LlmqType::LlmqTest, &base().hash, who, &vvec);
    }
    let qc = c.pcommit(1, Lie::FreshKey);
    let out = process(&mut session, &mut env, &[(1, &qc)]);
    assert_eq!(relayed(&out), [*qc.session_hash()]);
    let got = lines(&out.events);
    assert_eq!(
        got[1..],
        [
            format!(
                "{F} -- failed to build quorum verification vector. skipping full verification"
            ),
            format!("{F} -- verified premature commitment. received=1/4"),
        ]
    );
}

#[test]
fn only_the_valid_members_rows_are_read() {
    // Mutant: every member's rows read, whatever `validMembers` says.
    let c = Commit::new();
    let (mut session, mut env) = c.receiver(0);
    without_vvec_of_member_0(&mut env, &c.round);
    let flag = BLS.scheme();
    let valid = [false, true, true, true];
    let vvecs: Vec<&[BlsPkValue]> = c.round.contributions[1..].iter().map(|x| x.vvec()).collect();
    let quorum_vvec = build_quorum_verification_vector(&vvecs, flag).expect("three vectors");
    let pro_tx = &c.round.members[1].pro_tx_hash;
    let qc = PrematureCommitment::unsigned(
        LlmqType::LlmqTest,
        &base().hash,
        pro_tx,
        &valid,
        &quorum_vvec[0],
        &vvec_hash(&quorum_vvec, flag),
        flag,
    );
    let shares: Vec<&BlsSkValue> = c.round.shares[1..].iter().map(|s| &s[1]).collect();
    let quorum_sig = sign_stand_in(&aggregate_secret_keys(&shares, flag), qc.sign_hash(), false);
    let sig = c.round.keys[1].sign(qc.sign_hash(), false);
    let qc = qc.with_quorum_sig(quorum_sig, flag).with_sig(sig, flag);
    // `LLMQ_TEST` with 4 members needs all 4; a minSize of 3 lets three be valid.
    session.params.min_size = 3;
    let out = process(&mut session, &mut env, &[(1, &qc)]);
    assert_eq!(relayed(&out), [*qc.session_hash()]);
    assert_eq!(
        lines(&out.events)[1],
        format!("{F} -- verified premature commitment. received=1/4")
    );
    assert!(!env.store.reads.contains(&StoreRead::Vvec(c.round.members[0].pro_tx_hash)));
}

#[test]
fn a_recorded_hash_is_kept_once_and_processed_again() {
    // Set semantics (m6 (d)): the same message again leaves the count at 1, is checked again
    // and relayed again; a second distinct one makes 2, and a third is dropped unscored.
    let c = Commit::new();
    let (mut session, mut env) = c.receiver(0);
    let qc = c.pcommit(1, Lie::Honest);
    for _ in 0..2 {
        let out = process(&mut session, &mut env, &[(1, &qc)]);
        assert_eq!(relayed(&out), [*qc.session_hash()]);
        assert_eq!(session.member_state[1].premature_commitments.len(), 1);
        assert_eq!(session.premature_commitments.len(), 1);
    }
    let lied = c.pcommit(1, Lie::VvecHash);
    let out = process(&mut session, &mut env, &[(1, &lied)]);
    assert!(out.relay.is_empty() && out.misbehaving.is_empty());
    assert_eq!(session.member_state[1].premature_commitments.len(), 2);
    let third = c.pcommit(1, Lie::FreshKey);
    let out = process(&mut session, &mut env, &[(1, &third)]);
    assert!(out.relay.is_empty() && out.misbehaving.is_empty());
    let p1 = c.round.members[1].pro_tx_hash;
    assert!(lines(&out.events).contains(&format!(
        "PreVerifyMessage -- dropping commitment from {p1} as we already got 2 commitments"
    )));
    assert!(!session.premature_commitments.contains_key(third.session_hash()));
    // The first stays served, the second never is.
    assert!(session.get_premature_commitment(qc.session_hash()).is_some());
    assert!(session.get_premature_commitment(lied.session_hash()).is_none());
}

#[test]
fn a_non_member_commitment_is_banned_at_preverify_and_never_recorded() {
    let c = Commit::new();
    let (mut session, mut env) = c.receiver(0);
    let (outsiders, keys) = quorum(1, 0xbad);
    let flag = BLS.scheme();
    let honest = c.pcommit(1, Lie::Honest);
    let qc = PrematureCommitment::unsigned(
        LlmqType::LlmqTest,
        &base().hash,
        &outsiders[0].pro_tx_hash,
        &[true; N],
        honest.quorum_public_key(),
        honest.quorum_vvec_hash(),
        flag,
    );
    let sig = keys[0].sign(qc.sign_hash(), false);
    let qc = qc.with_quorum_sig(sig.clone(), flag).with_sig(sig, flag);
    let out = process(&mut session, &mut env, &[(3, &qc)]);
    assert_eq!(
        out.misbehaving.iter().map(|s| s.misbehaviour).collect::<Vec<_>>(),
        [Misbehaviour::PreVerifyBan {
            peer: 3,
            reason: DkgReject::NotMember,
        }]
    );
    assert!(out.relay.is_empty() && session.premature_commitments.is_empty());
    assert!(env.store.reads.is_empty());
}

#[test]
fn an_observer_checks_and_relays_commitments_as_a_member_does() {
    // Core's Receive runs on members and non-members alike.
    let c = Commit::new();
    let (_, member_env) = c.receiver(0);
    let mut session = member_session(&c.round.params, &c.round.members, None, true);
    let mut env = TestEnv::new(&c.round.keys[0], 1);
    env.store = member_env.store.clone();
    let qc = c.pcommit(1, Lie::Honest);
    let out = process(&mut session, &mut env, &[(1, &qc)]);
    assert_eq!(relayed(&out), [*qc.session_hash()]);
    let lied = c.pcommit(2, Lie::FreshKey);
    let out = process(&mut session, &mut env, &[(2, &lied)]);
    assert!(out.relay.is_empty());
    assert_eq!(
        lines(&out.events)[1],
        format!("{F} -- calculated quorum public key does not match")
    );
}

#[test]
fn own_commitments_are_received_without_scores() {
    // `EnqueueOwn`: our own pcommit is processed like any other; a bad one scores nobody.
    let c = Commit::new();
    let (mut session, mut env) = c.receiver(1);
    let own = c.pcommit(1, Lie::Honest);
    let lie = c.pcommit(1, Lie::Sig);
    let raws = [(1, own.payload(BLS.scheme())), (1, lie.payload(BLS.scheme()))];
    let out =
        session.process_batch(DkgMsgKind::PrematureCommitment, &batch_of(1, &raws), &mut env.env());
    assert!(out.misbehaving.is_empty());
    assert_eq!(relayed(&out), [*own.session_hash()]);
}

// The receiver column of the lieType table (LQ4 §4.4).

#[test]
fn lie_2_and_forced_3_fail_the_batch_signature_once_per_peer() {
    // `l/net_dkg.cpp:425-437`: 100 once per peer, and every message of that peer in the batch
    // skipped, a valid one included. Mutants: a score per message; the peer's valid message
    // received.
    let c = Commit::new();
    for lie in [Lie::SignHash, Lie::SigForced] {
        let (mut session, mut env) = c.receiver(0);
        let lied = c.pcommit(1, lie);
        let valid_same_peer = c.pcommit(2, Lie::Honest);
        let other_peer = c.pcommit(3, Lie::Honest);
        let out = process(
            &mut session,
            &mut env,
            &[(1, &lied), (1, &valid_same_peer), (3, &other_peer), (1, &lied)],
        );
        assert_eq!(
            out.misbehaving.iter().map(|s| s.misbehaviour).collect::<Vec<_>>(),
            [Misbehaviour::BadSignature {
                peer: 1,
            }],
            "{lie:?}"
        );
        assert_eq!(relayed(&out), [*other_peer.session_hash()], "{lie:?}");
        assert!(lines(&out.events).contains(
            &"ProcessPendingMessageBatch -- failed to verify signature, peer=1".to_owned()
        ));
        assert!(!session.premature_commitments.contains_key(lied.session_hash()));
        assert!(!session.premature_commitments.contains_key(valid_same_peer.session_hash()));
        assert!(session.member_state[2].premature_commitments.is_empty());
    }
}

#[test]
fn ordinary_lies_3_and_4_are_preverify_bans_per_message() {
    // `l/net_dkg.cpp:410-418`: the zeros Core writes for the invalid object read back invalid,
    // and PreVerify refuses it with `retBan`, once per message. Mutant: one score per peer.
    let c = Commit::new();
    for (lie, reason, text) in [
        (Lie::Sig, DkgReject::InvalidSig, "invalid membersSig"),
        (Lie::QuorumSig, DkgReject::InvalidQuorumSig, "invalid quorumSig"),
    ] {
        let (mut session, mut env) = c.receiver(0);
        let a = c.pcommit(1, lie);
        let b = c.pcommit(2, lie);
        let honest = c.pcommit(3, Lie::Honest);
        let out = process(&mut session, &mut env, &[(1, &a), (1, &b), (1, &honest)]);
        let ban = Misbehaviour::PreVerifyBan {
            peer: 1,
            reason,
        };
        assert_eq!(
            out.misbehaving.iter().map(|s| s.misbehaviour).collect::<Vec<_>>(),
            [ban, ban],
            "{lie:?}"
        );
        let got = lines(&out.events);
        assert_eq!(got.iter().filter(|l| **l == format!("PreVerifyMessage -- {text}")).count(), 2);
        // The peer's valid message still goes through: PreVerify skips per message.
        assert_eq!(relayed(&out), [*honest.session_hash()], "{lie:?}");
        assert!(session.member_state[1].premature_commitments.is_empty());
    }
}

#[test]
fn lies_0_1_and_forced_4_are_rejected_where_the_vector_builds_and_relayed_where_not() {
    // The same split for each: no relay and no score, or accepted unverified and relayed.
    let c = Commit::new();
    for (lie, text) in [
        (Lie::FreshKey, "calculated quorum public key does not match"),
        (Lie::VvecHash, "calculated quorum vvec hash does not match"),
        (Lie::QuorumSigForced, "failed to verify quorumSig"),
    ] {
        let qc = c.pcommit(1, lie);
        let (mut session, mut env) = c.receiver(0);
        let out = process(&mut session, &mut env, &[(1, &qc)]);
        assert!(out.misbehaving.is_empty() && out.relay.is_empty(), "{lie:?}");
        assert_eq!(lines(&out.events).last(), Some(&format!("{F} -- {text}")), "{lie:?}");

        let (mut session, mut env) = c.receiver(0);
        without_vvec_of_member_0(&mut env, &c.round);
        let out = process(&mut session, &mut env, &[(1, &qc)]);
        assert!(out.misbehaving.is_empty(), "{lie:?}");
        assert_eq!(relayed(&out), [*qc.session_hash()], "{lie:?}");
        assert!(session.get_premature_commitment(qc.session_hash()).is_some(), "{lie:?}");
    }
}

// The sender (`VerifyAndCommit`, `SendCommitment`) and the liar's own loopback (LQ4 §4.4).

const S: &str = "SendCommitment";
const V: &str = "VerifyAndCommit";

/// The lie type each [`Lie`] stands for.
const LIES: [(u64, Lie); 5] = [
    (0, Lie::FreshKey),
    (1, Lie::VvecHash),
    (2, Lie::SignHash),
    (3, Lie::Sig),
    (4, Lie::QuorumSig),
];

/// The fresh key [`Commit::pcommit`]'s lie type 0 takes, as the session's lie draw.
fn lie_key() -> BlsSkValue {
    make_new_key(&mut SeededRng::new(7))
}

/// The draws of lie type `k`, lie type 0 drawing [`lie_key`].
fn lies(k: u64) -> Vec<Draw> {
    lie_draws(k, lie_key())
}

/// Whether the session set `sentPrematureCommitment`.
fn sent_bit(env: &TestEnv) -> bool {
    env.debug.updates.contains(&DebugUpdate::Session(SessionStatusBit::SentPrematureCommitment))
}

impl Commit {
    /// Member `me`'s session at the commitment: every contribution received and every share
    /// verified, so every `qdkg_V` and `qdkg_S` row written; its lines and the store's reads
    /// cleared.
    fn committer(&self, me: usize) -> (DkgSession<TestMember>, TestEnv) {
        let (mut session, mut env) = self.receiver(me);
        session.verify_pending_contributions(&mut env.env());
        assert_eq!(env.store.writers("S").len(), N);
        session.take_events();
        env.store.reads.clear();
        (session, env)
    }

    /// Member `me`'s share of the quorum's secret: the sum of its true shares.
    fn sk_share(&self, me: usize) -> BlsSkValue {
        let shares: Vec<&BlsSkValue> = self.round.shares.iter().map(|s| &s[me]).collect();
        aggregate_secret_keys(&shares, BLS.scheme())
    }

    /// Member `me`'s public key share, `pubKeyShare=` as Core logs it.
    fn pub_key_share_line(&self, me: usize) -> String {
        let pk = secret_to_public(&self.sk_share(me));
        format!("{S} -- pubKeyShare={}", crate::dkg::session::hex(&pk, BLS.scheme()))
    }
}

/// `verify_and_commit` with the draws `draws` under `rates`, which must all be made.
fn commit_with(
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    rates: &[(SimError, i32)],
    draws: Vec<Draw>,
) -> Option<PrematureCommitment> {
    for (kind, rate) in rates {
        env.sim.set(*kind, *rate).expect("a rate");
    }
    let mut rng = ReplayRng::new(draws);
    let qc = session.verify_and_commit(&mut DkgEnv {
        rng: &mut rng,
        ..env.env()
    });
    rng.finish().expect("the draws, in order");
    qc
}

/// `qc` into its sender's own session as [`Sender::Own`], alone in its batch.
fn loopback(
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    me: usize,
    qc: &PrematureCommitment,
) -> BatchOutcome<usize> {
    let raws = [(me, qc.payload(BLS.scheme()))];
    session.process_batch(DkgMsgKind::PrematureCommitment, &batch_of(me, &raws), &mut env.env())
}

#[test]
fn an_honest_member_sends_its_commitment_and_its_loopback_relays_it() {
    // Mutants: the quorum signature from another key or scheme; the operator signature
    // skipped; the debug bit unset; validMembers off by a member.
    let c = Commit::new();
    let (mut session, mut env) = c.committer(1);
    let qc = commit_with(&mut session, &mut env, &[], Vec::new()).expect("a commitment");
    assert_eq!(qc.payload(BLS.scheme()), c.pcommit(1, Lie::Honest).payload(BLS.scheme()));
    assert_eq!(qc.valid_members(), [true; N]);
    assert_eq!(
        lines(&session.take_events()),
        [
            format!("{S} -- sending commitment"),
            c.pub_key_share_line(1),
            format!("{S} -- built premature commitment"),
        ]
    );
    assert_eq!(
        env.debug.updates.last(),
        Some(&DebugUpdate::Session(SessionStatusBit::SentPrematureCommitment))
    );
    let mut want = vec![StoreRead::Members];
    for who in c.round.pro_txs() {
        want.extend([StoreRead::Vvec(who), StoreRead::Sk(who)]);
    }
    assert_eq!(env.store.reads, want, "the members access first, then each member's rows");

    let out = loopback(&mut session, &mut env, 1, &qc);
    assert!(out.misbehaving.is_empty());
    assert_eq!(relayed(&out), [*qc.session_hash()]);
    assert!(session.get_premature_commitment(qc.session_hash()).is_some());
}

#[test]
fn the_draws_come_in_cores_order() {
    // LQ4 §2.9: COMMIT_OMIT, then COMMIT_LIE, its lie type, and lie type 0's key. Mutants:
    // the lie draw before the omit draw; the key drawn for every lie type; no lie type draw.
    let c = Commit::new();
    let (mut session, mut env) = c.committer(0);
    let draws = [vec![Draw::Bool(SimError::CommitOmit, 50, false)], lies(0)].concat();
    let rates = [(SimError::CommitOmit, 50), (SimError::CommitLie, 100)];
    let qc = commit_with(&mut session, &mut env, &rates, draws).expect("a lie is sent");
    assert_eq!(qc.payload(BLS.scheme()), c.pcommit(0, Lie::FreshKey).payload(BLS.scheme()));
    assert!(
        lines(&session.take_events()).contains(&format!("{S} -- lying on commitment. lieType=0"))
    );
    for k in 1..5 {
        let (mut session, mut env) = c.committer(0);
        commit_with(&mut session, &mut env, &[(SimError::CommitLie, 100)], lies(k))
            .expect("a lie is sent");
    }
}

#[test]
fn commit_omit_sends_nothing_before_any_store_read() {
    // Mutants: the omit check skipped; it after the store read; the debug bit set.
    let c = Commit::new();
    let (mut session, mut env) = c.committer(0);
    let draws = vec![Draw::Bool(SimError::CommitOmit, 100, true)];
    let qc = commit_with(&mut session, &mut env, &[(SimError::CommitOmit, 100)], draws);
    assert!(qc.is_none());
    assert_eq!(
        lines(&session.take_events()),
        [format!("{S} -- sending commitment"), format!("{S} -- omitting")]
    );
    assert!(env.store.reads.is_empty());
    assert!(!sent_bit(&env));
}

#[test]
fn below_min_size_nothing_is_sent_or_drawn() {
    // `CountValidMembers() < minSize` (rc.5 :496-500), before COMMIT_OMIT's draw. Mutants: the
    // gate skipped; `<=`; the omit draw first.
    let c = Commit::new();
    let (mut session, mut env) = c.committer(0);
    session.member_state[3].bad = true;
    let qc = commit_with(&mut session, &mut env, &[(SimError::CommitOmit, 100)], Vec::new());
    assert!(qc.is_none());
    let p3 = c.round.members[3].pro_tx_hash;
    assert_eq!(
        lines(&session.take_events()),
        [
            format!("{V} -- verification result:"),
            format!("{V} --   members previously determined as bad:"),
            format!("{V} --     {p3}"),
            format!("{S} -- sending commitment"),
            format!("{S} -- not enough valid members. not sending commitment"),
        ]
    );
    assert!(env.store.reads.is_empty());
    // Exactly minSize valid members commit, the bad one's bit clear and its rows unread.
    let (mut session, mut env) = c.committer(0);
    session.member_state[3].bad = true;
    session.params.min_size = 3;
    let qc = commit_with(&mut session, &mut env, &[], Vec::new()).expect("three of three");
    assert_eq!(qc.valid_members(), [true, true, true, false]);
    assert!(!env.store.reads.contains(&StoreRead::Vvec(p3)));
}

#[test]
fn verify_and_commit_leaves_members_with_open_complaints_out() {
    // Mutant: `mark_open_complaints` not called (member 2 stays valid).
    let c = Commit::new();
    let (mut session, mut env) = c.committer(0);
    session.params.min_size = 3;
    session.member_state[2].complaints_from_others.insert(c.round.members[1].pro_tx_hash);
    let qc = commit_with(&mut session, &mut env, &[], Vec::new()).expect("a commitment");
    assert_eq!(qc.valid_members(), [true, true, false, true]);
    let p2 = c.round.members[2].pro_tx_hash;
    assert_eq!(
        lines(&session.take_events())[..3],
        [
            format!("{V} -- verification result:"),
            format!("{V} --   members with open complaints and now marked as bad:"),
            format!("{V} --     {p2}"),
        ]
    );
}

#[test]
fn a_non_member_commits_nothing() {
    // A bad member to list: mutant, the marking before the membership check.
    let c = Commit::new();
    let mut session = member_session(&c.round.params, &c.round.members, None, true);
    session.member_state[1].bad = true;
    let mut env = TestEnv::new(&c.round.keys[0], 1);
    env.store.members = c.round.pro_txs();
    let qc = commit_with(&mut session, &mut env, &[(SimError::CommitOmit, 100)], Vec::new());
    assert!(qc.is_none());
    assert!(session.take_events().is_empty());
    assert!(env.store.reads.is_empty() && env.debug.updates.is_empty());
}

#[test]
fn a_missing_vector_row_sends_nothing() {
    // Rc.5 :513-516. Mutants: the failure ignored; the store's line in the record.
    let c = Commit::new();
    let (mut session, mut env) = c.committer(1);
    without_vvec_of_member_0(&mut env, &c.round);
    let qc = commit_with(&mut session, &mut env, &[(SimError::CommitLie, 100)], Vec::new());
    assert!(qc.is_none());
    let events = session.take_events();
    let p0 = c.round.members[0].pro_tx_hash;
    let block = base().hash;
    assert_eq!(
        lines(&events),
        [
            format!(
                "GetVerifiedContributions -- this node does not have vvec for llmq=100 \
                 block={block} protx={p0}"
            ),
            format!("{S} -- sending commitment"),
            format!("{S} -- failed to get valid contributions"),
        ]
    );
    assert_eq!(events[0].record, None);
    assert!(events[1].record.is_some() && events[1].record == events[2].record);
    assert!(!sent_bit(&env));
}

#[test]
fn vectors_that_do_not_build_send_nothing() {
    // Rc.5 :518-522. Mutant: the build failure ignored.
    let c = Commit::new();
    let (mut session, mut env) = c.committer(1);
    let p0 = c.round.members[0].pro_tx_hash;
    let short = c.round.contributions[0].vvec()[..1].to_vec();
    env.store.write_vvec(LlmqType::LlmqTest, &base().hash, &p0, &short);
    let qc = commit_with(&mut session, &mut env, &[(SimError::CommitLie, 100)], Vec::new());
    assert!(qc.is_none());
    assert_eq!(
        lines(&session.take_events()),
        [
            format!("{S} -- sending commitment"),
            format!("{S} -- failed to build quorum verification vector"),
        ]
    );
}

#[test]
fn an_invalid_share_row_sends_nothing() {
    // Rc.5 :526-530: `AggregateSecretKeys` returns the invalid key for an invalid input, a
    // missing `qdkg_S` row included (the store gives the invalid key). Mutant: the share's
    // validity unchecked (an invalid `quorumSig` is then sent).
    let c = Commit::new();
    for missing in [false, true] {
        let (mut session, mut env) = c.committer(1);
        let p0 = c.round.members[0].pro_tx_hash;
        if missing {
            env.store.rows.retain(|(_, _, row)| {
                !matches!(row, crate::dkg::reference::StoreRow::Sk { who, .. } if *who == p0)
            });
        } else {
            env.store.write_sk(LlmqType::LlmqTest, &base().hash, &p0, &BlsSkValue::invalid());
        }
        let qc = commit_with(&mut session, &mut env, &[(SimError::CommitLie, 100)], Vec::new());
        assert!(qc.is_none(), "missing row: {missing}");
        assert_eq!(
            lines(&session.take_events()),
            [
                format!("{S} -- sending commitment"),
                format!("{S} -- failed to build own secret share")
            ]
        );
    }
}

#[test]
fn each_lie_type_changes_the_sent_commitment_as_cores_does() {
    // The sender column of LQ4 §4.4: the bytes of Core's lie, each lie type on its own.
    // Mutants: each arm skipped or swapped; lie type 2 applied after the signatures.
    let c = Commit::new();
    for (k, lie) in LIES {
        let (mut session, mut env) = c.committer(2);
        let qc = commit_with(&mut session, &mut env, &[(SimError::CommitLie, 100)], lies(k))
            .expect("a lie is sent");
        assert_eq!(qc.payload(BLS.scheme()), c.pcommit(2, lie).payload(BLS.scheme()), "{lie:?}");
        let got = lines(&session.take_events());
        assert_eq!(
            got[1..],
            [
                c.pub_key_share_line(2),
                format!("{S} -- lying on commitment. lieType={k}"),
                format!("{S} -- built premature commitment"),
            ],
            "{lie:?}"
        );
    }
}

#[test]
fn the_liar_relays_none_of_its_lies_and_scores_nobody() {
    // The liar's column of LQ4 §4.4 (X2): each lie arrives at its own loopback as `Own`, is
    // rejected at its stage, relayed to nobody, never served, and its score dropped.
    let c = Commit::new();
    let mut sent: Vec<(String, PrematureCommitment, &str)> = Vec::new();
    for k in 0..5 {
        let (mut session, mut env) = c.committer(2);
        let qc = commit_with(&mut session, &mut env, &[(SimError::CommitLie, 100)], lies(k))
            .expect("a lie is sent");
        sent.push((format!("lie type {k}"), qc, lie_loopback_line(k)));
    }
    // The forced variants: perturbed bytes that still decode, here valid points that do not
    // verify.
    let (mut session, mut env) = c.committer(2);
    let honest = commit_with(&mut session, &mut env, &[], Vec::new()).expect("a commitment");
    let flag = BLS.scheme();
    let other = c.round.keys[0].sign(honest.sign_hash(), false);
    sent.push((
        "forced 3".into(),
        honest.clone().with_sig(other.clone(), flag),
        "ProcessPendingMessageBatch -- failed to verify signature, peer=-1",
    ));
    sent.push((
        "forced 4".into(),
        honest.with_quorum_sig(other, flag),
        "ReceiveMessage -- failed to verify quorumSig",
    ));
    for (label, qc, text) in sent {
        let (mut session, mut env) = c.committer(2);
        let out = loopback(&mut session, &mut env, 2, &qc);
        assert!(out.misbehaving.is_empty(), "{label}");
        assert!(out.relay.is_empty(), "{label}");
        assert!(lines(&out.events).contains(&text.to_owned()), "{label}: {:?}", out.events);
        assert!(session.get_premature_commitment(qc.session_hash()).is_none(), "{label}");
        assert!(session.valid_commitments.is_empty(), "{label}");
    }
}

#[test]
fn the_signatures_use_the_sessions_scheme_before_v19() {
    // Both signatures sign under `m_use_legacy_bls`, and lie types 3 and 4 change their bytes
    // under it; the message is written under the step's scheme. Mutants: either signature under
    // the basic scheme; the signatures written under the signing scheme; the lie's bytes taken
    // under the step's scheme.
    let c = Commit::new();
    let flag = BLS.scheme();
    let legacy = BlsSchemeKind::Legacy;
    let pro_tx = &c.round.members[3].pro_tx_hash;
    let expected = |lie: Option<u64>| -> PrematureCommitment {
        let qc = PrematureCommitment::unsigned(
            LlmqType::LlmqTest,
            &base().hash,
            pro_tx,
            &[true; N],
            &c.quorum_vvec[0],
            &vvec_hash(&c.quorum_vvec, flag),
            flag,
        );
        let mut sig = c.round.keys[3].sign(qc.sign_hash(), true);
        let mut quorum_sig = sign_stand_in(&c.sk_share(3), qc.sign_hash(), true);
        let changed = |sig: &BlsSigValue| {
            let mut bytes = sig.to_bytes(legacy);
            bytes[5] = bytes[5].wrapping_add(1);
            BlsSigValue::from_canonical(&bytes, legacy).unwrap_or_else(BlsSigValue::invalid)
        };
        match lie {
            Some(3) => sig = changed(&sig),
            Some(4) => quorum_sig = changed(&quorum_sig),
            _ => {}
        }
        qc.with_quorum_sig(quorum_sig, flag).with_sig(sig, flag)
    };
    for lie in [None, Some(3), Some(4)] {
        let (mut session, mut env) = c.committer(3);
        session.v19_after_base = false;
        let (rates, draws) = match lie {
            Some(k) => (vec![(SimError::CommitLie, 100)], lies(k)),
            None => (Vec::new(), Vec::new()),
        };
        let qc = commit_with(&mut session, &mut env, &rates, draws).expect("a commitment");
        let want = expected(lie);
        assert_eq!(qc.payload(flag), want.payload(flag), "{lie:?}");
        assert_eq!(qc.session_hash(), want.session_hash(), "{lie:?}");
        if lie.is_none() {
            assert_ne!(qc.sig(), &c.round.keys[3].sign(qc.sign_hash(), false));
            continue;
        }
        // Whatever stage rejects it, the liar relays none of it and scores nobody.
        let out = loopback(&mut session, &mut env, 3, &qc);
        assert!(out.relay.is_empty() && out.misbehaving.is_empty(), "{lie:?}");
        assert!(session.get_premature_commitment(qc.session_hash()).is_none(), "{lie:?}");
    }
}

// v24.0.0 (#8016): members whose received verification vectors share a coefficient.

#[test]
fn members_sharing_coefficients_are_marked_bad_and_left_out_of_the_commitment() {
    // Member 2 received member 1's whole vector as its own. Mutants: the pass skipped; only one
    // of the two marked; the debug bit unset; the header missing.
    let c = Commit::new();
    let (mut session, mut env) = c.committer(0);
    session.params.min_size = 2;
    session.member_state[2].received_vvec = Some(c.round.contributions[1].vvec().to_vec());
    let before = env.debug.updates.len();
    let qc = commit_with(&mut session, &mut env, &[], Vec::new()).expect("a commitment");
    assert_eq!(qc.valid_members(), [true, false, false, true]);
    assert_eq!(
        env.debug.updates[before..before + 2],
        [
            DebugUpdate::Member(1, MemberStatusBit::Bad),
            DebugUpdate::Member(2, MemberStatusBit::Bad)
        ]
    );
    let p = c.round.pro_txs();
    assert_eq!(
        lines(&session.take_events())[..5],
        [
            format!("{V} -- verification result:"),
            format!(
                "{V} --   members sharing verification vector coefficients and now marked as bad:"
            ),
            format!("{V} --     {}", p[1]),
            format!("{V} --     {}", p[2]),
            format!("{S} -- sending commitment"),
        ]
    );
    // The commitment over members 0 and 3 verifies at another member, which relays it.
    let (mut receiver, mut renv) = c.receiver(3);
    receiver.params.min_size = 2;
    let out = process(&mut receiver, &mut renv, &[(0, &qc)]);
    assert_eq!(relayed(&out), [*qc.session_hash()]);
}

#[test]
fn only_members_still_valid_after_the_complaints_are_checked_for_shared_coefficients() {
    // Member 3 is bad and holds member 0's vector; member 2 has open complaints and shares a
    // leading coefficient with member 1. Neither pair is considered, so nobody more is marked.
    // Mutants: the pass over every member; the pass before the open complaints.
    let c = Commit::new();
    let (mut session, mut env) = c.committer(0);
    session.member_state[3].bad = true;
    session.member_state[3].received_vvec = Some(c.round.contributions[0].vvec().to_vec());
    session.member_state[2].complaints_from_others.insert(c.round.members[0].pro_tx_hash);
    let mut vvec = c.round.contributions[2].vvec().to_vec();
    vvec[0] = c.round.contributions[1].vvec()[0].clone();
    session.member_state[2].received_vvec = Some(vvec.clone());
    session.mark_open_complaints(BLS.scheme(), &mut env.debug);
    assert_eq!(session.valid_members(), [true, true, false, false]);
    let p = c.round.pro_txs();
    assert_eq!(
        lines(&session.take_events()),
        [
            format!("{V} -- verification result:"),
            format!("{V} --   members previously determined as bad:"),
            format!("{V} --     {}", p[3]),
            format!("{V} --   members with open complaints and now marked as bad:"),
            format!("{V} --     {}", p[2]),
        ]
    );
    // Without the complaint, members 1 and 2 share the leading coefficient: both are marked,
    // listed after the members bad before.
    let (mut session, mut env) = c.committer(0);
    session.member_state[3].bad = true;
    session.member_state[2].received_vvec = Some(vvec);
    session.mark_open_complaints(BLS.scheme(), &mut env.debug);
    assert_eq!(session.valid_members(), [true, false, false, false]);
    assert_eq!(
        lines(&session.take_events()),
        [
            format!("{V} -- verification result:"),
            format!("{V} --   members previously determined as bad:"),
            format!("{V} --     {}", p[3]),
            format!(
                "{V} --   members sharing verification vector coefficients and now marked as bad:"
            ),
            format!("{V} --     {}", p[1]),
            format!("{V} --     {}", p[2]),
        ]
    );
}

#[test]
fn shared_coefficients_mark_the_members_they_were_found_for() {
    // Member 0 is bad, so the pass looks at members 1, 2 and 3; member 3 holds member 2's
    // vector. Mutant: the found positions taken as member indexes (members 1 and 2 marked).
    let c = Commit::new();
    let (mut session, mut env) = c.committer(1);
    session.member_state[0].bad = true;
    session.member_state[3].received_vvec = Some(c.round.contributions[2].vvec().to_vec());
    session.mark_open_complaints(BLS.scheme(), &mut env.debug);
    assert_eq!(session.valid_members(), [false, true, false, false]);
    let p = c.round.pro_txs();
    assert_eq!(
        lines(&session.take_events())[3..],
        [
            format!(
                "{V} --   members sharing verification vector coefficients and now marked as bad:"
            ),
            format!("{V} --     {}", p[2]),
            format!("{V} --     {}", p[3]),
        ]
    );
}

#[test]
fn valid_members_has_a_bit_per_size() {
    // `CDKGPrematureCommitment(params)` sizes `validMembers` to `params.size`, past the members
    // too. Mutant: a bit per member.
    let c = Commit::new();
    let (mut session, mut env) = c.committer(0);
    session.params.size = 6;
    let qc = commit_with(&mut session, &mut env, &[], Vec::new()).expect("a commitment");
    assert_eq!(qc.valid_members(), [true, true, true, true, false, false]);
}

#[test]
fn the_lie_type_is_drawn_below_five() {
    // `GetRand<int>(5)`: with a seeded RNG, the lie type is that RNG's draw below 5 after the
    // `COMMIT_LIE` decision. Mutant: another bound.
    let c = Commit::new();
    let draw = |seed: u64, n: u64| {
        let mut rng = CoreRng(SeededRng::new(seed));
        assert!(rng.rand_bool(100, SimError::CommitLie));
        rng.rand_range(n, RangeDraw::CommitLieType)
    };
    let mut told_apart = false;
    for seed in 0..8 {
        let want = draw(seed, 5);
        told_apart |= want != draw(seed, 6);
        let (mut session, mut env) = c.committer(0);
        env.sim.set(SimError::CommitLie, 100).expect("a rate");
        env.rng = Box::new(CoreRng(SeededRng::new(seed)));
        session.verify_and_commit(&mut env.env()).expect("a lie is sent");
        let line = format!("{S} -- lying on commitment. lieType={want}");
        assert!(lines(&session.take_events()).contains(&line), "seed {seed}");
    }
    assert!(told_apart, "a seed whose draws below 5 and below 6 differ");
}

/// The legacy bytes lie types 3 and 4 make of `sig`: byte 5 + 1.
fn bumped_legacy(sig: &BlsSigValue) -> [u8; 96] {
    let mut bytes = sig.to_bytes(BlsSchemeKind::Legacy);
    bytes[5] = bytes[5].wrapping_add(1);
    bytes
}

#[test]
fn a_legacy_lie_keeps_the_object_its_bytes_give() {
    // Under the legacy scheme, changed bytes that decode give their element, and signed bytes
    // that do not decode give the element of the same bytes with the sign bit clear; under the
    // basic scheme bytes that do not decode give the invalid signature. Mutants: no fallback;
    // the bit left set. The fallback under the basic scheme is equivalent: no basic encoding
    // with the sign bit clear decodes.
    let legacy = BlsSchemeKind::Legacy;
    let mut rng = SeededRng::new(0x44);
    let (mut direct, mut cleared) = (0, 0);
    for i in 0..64u64 {
        let sk = make_new_key(&mut rng);
        let mut hash = [0u8; 32];
        hash[..8].copy_from_slice(&i.to_le_bytes());
        let sig = sign_stand_in(&sk, &hash, true);
        let bytes = bumped_legacy(&sig);
        let got = super::perturbed(&sig, legacy);
        match BlsSigValue::from_canonical(&bytes, legacy) {
            Some(want) => {
                direct += 1;
                assert_eq!(got, want, "{i}");
            }
            None => {
                assert_ne!(bytes[0] & 0x80, 0, "{i}");
                let mut clear = bytes;
                clear[0] &= 0x7f;
                cleared += 1;
                assert!(got.is_valid(), "{i}");
                assert_eq!(got.to_bytes(legacy), clear, "{i}");
            }
        }
        let basic = sign_stand_in(&sk, &hash, false);
        let mut b = basic.to_bytes(BlsSchemeKind::Basic);
        b[5] = b[5].wrapping_add(1);
        let want = BlsSigValue::from_canonical(&b, BlsSchemeKind::Basic)
            .unwrap_or_else(BlsSigValue::invalid);
        assert_eq!(super::perturbed(&basic, BlsSchemeKind::Basic), want, "{i}");
    }
    assert!(direct > 0 && cleared > 0, "both cases met: {direct} {cleared}");
}

#[test]
fn a_pure_legacy_liar_rejects_lie_3_at_the_batch_and_records_lie_4() {
    // A session with V19 off at its base and the process-wide scheme legacy: lie types 3 and 4
    // keep a valid object, so the loopback rejects lie type 3 at the batch signature check and
    // records lie type 4 before "failed to verify quorumSig". Mutant: no legacy fallback in
    // `perturbed` (the members whose changed bytes do not decode reach PreVerify instead).
    let c = Commit::new();
    let ctx = BlsCtx::LEGACY;
    let mut fallbacks = [0, 0];
    for me in 0..N {
        for (slot, k) in [(0, 3u64), (1, 4)] {
            let (mut session, mut env) = c.committer(me);
            session.v19_after_base = false;
            env.sim.set(SimError::CommitLie, 100).expect("a rate");
            let mut rng = ReplayRng::new(lies(k));
            let qc = session
                .verify_and_commit(&mut DkgEnv {
                    bls: ctx,
                    rng: &mut rng,
                    ..env.env()
                })
                .expect("a lie is sent");
            rng.finish().expect("the draws, in order");
            let lied = if k == 3 {
                qc.sig()
            } else {
                qc.quorum_sig()
            };
            assert!(lied.is_valid(), "member {me} lie {k}");
            let (mut probe_session, mut probe_env) = c.committer(me);
            probe_session.v19_after_base = false;
            let honest_qc = {
                let mut rng = ReplayRng::new(Vec::new());
                let qc = probe_session
                    .verify_and_commit(&mut DkgEnv {
                        bls: ctx,
                        rng: &mut rng,
                        ..probe_env.env()
                    })
                    .expect("a commitment");
                rng.finish().expect("no draws");
                qc
            };
            let original = if k == 3 {
                honest_qc.sig()
            } else {
                honest_qc.quorum_sig()
            };
            if BlsSigValue::from_canonical(&bumped_legacy(original), BlsSchemeKind::Legacy)
                .is_none()
            {
                fallbacks[slot] += 1;
            }
            session.take_events();
            let raws = [(me, qc.payload(ctx.scheme()))];
            let out = session.process_batch(
                DkgMsgKind::PrematureCommitment,
                &batch_of(me, &raws),
                &mut DkgEnv {
                    bls: ctx,
                    ..env.env()
                },
            );
            assert!(out.relay.is_empty() && out.misbehaving.is_empty(), "member {me} lie {k}");
            assert!(session.get_premature_commitment(qc.session_hash()).is_none());
            let text = lines(&out.events);
            if k == 3 {
                assert!(
                    text.contains(
                        &"ProcessPendingMessageBatch -- failed to verify signature, peer=-1".into()
                    ),
                    "member {me}: {text:?}"
                );
                assert!(!session.premature_commitments.contains_key(qc.session_hash()));
                assert!(session.member_state[me].premature_commitments.is_empty());
            } else {
                assert!(
                    text.contains(&"ReceiveMessage -- failed to verify quorumSig".into()),
                    "member {me}: {text:?}"
                );
                assert!(session.premature_commitments.contains_key(qc.session_hash()));
                assert_eq!(session.member_state[me].premature_commitments.len(), 1);
            }
        }
    }
    assert!(fallbacks[0] > 0 && fallbacks[1] > 0, "the fallback is met: {fallbacks:?}");
}
