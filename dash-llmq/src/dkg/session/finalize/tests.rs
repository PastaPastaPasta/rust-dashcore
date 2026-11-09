//! The finalization phase (LQ4b-1 T1): `FinalizeCommitments` over premature commitments that
//! arrived through the batch, and `FinalizeSingleCommitment`.

use dash_evo::llmq::{CommitmentError, CommitmentReject};

use super::*;
use crate::dkg::bls::{
    BlsCtx, BlsSkValue, aggregate_secret_keys, build_quorum_verification_vector, make_new_key,
    secret_to_public, sign_stand_in, verify_insecure,
};
use crate::dkg::reference::{ReferenceVerifier, TestMember, TestOperatorKey, quorum};
use crate::dkg::rng::{Draw, ReplayRng, SeededRng};
use crate::dkg::session::fixture::{
    BLS, Round, TestEnv, base, init_params, lines, member_session, params,
};
use crate::dkg::session::{DkgEvent, DkgMsgKind, OperatorKey, Sender};
use crate::dkg::wire::{LlmqType, final_commitment_bytes, vvec_hash};

const N: usize = 5;
const MIN: u16 = 3;
const F: &str = "FinalizeCommitments";
const SKIP: &str =
    "FinalizeCommitments -- quorumPublicKey or quorumVvecHash does not match, skipping";
const NO_RECOVER: &str = "FinalizeCommitments -- failed to recover quorum sig";
const UNVERIFIED: &str = "FinalizeCommitments -- failed to verify final commitment";

/// How a premature commitment departs from the honest one for its `validMembers`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pc {
    Honest,
    /// `quorumPublicKey` the key of a fresh secret from this seed, signed as it stands.
    Key(u64),
    /// The first `quorumVvecHash` byte plus this, signed as it stands.
    VvecHash(u8),
}

fn bits(set: &[usize]) -> Vec<bool> {
    (0..N).map(|i| set.contains(&i)).collect()
}

/// An LLMQ_TEST round of `N` members, `minSize` `MIN`, threshold 2.
struct Fin {
    round: Round,
}

impl Fin {
    fn new() -> Self {
        let mut round = Round::new(N);
        round.params.min_size = MIN;
        Self {
            round,
        }
    }

    fn members(&self) -> &[TestMember] {
        &self.round.members
    }

    /// The quorum verification vector of the members `valid`.
    fn quorum_vvec(&self, valid: &[bool]) -> Vec<BlsPkValue> {
        let vvecs: Vec<&[BlsPkValue]> =
            (0..N).filter(|&c| valid[c]).map(|c| self.round.contributions[c].vvec()).collect();
        build_quorum_verification_vector(&vvecs, BLS.scheme()).expect("honest vectors")
    }

    /// Member `from`'s secret key share of the members `valid`.
    fn sk_share(&self, from: usize, valid: &[bool]) -> BlsSkValue {
        let shares: Vec<&BlsSkValue> =
            (0..N).filter(|&c| valid[c]).map(|c| &self.round.shares[c][from]).collect();
        aggregate_secret_keys(&shares, BLS.scheme())
    }

    /// Member `from`'s signed premature commitment over `valid`, as `pc` has it.
    fn pcommit(&self, from: usize, valid: &[bool], pc: Pc) -> PrematureCommitment {
        let flag = BLS.scheme();
        let vvec = self.quorum_vvec(valid);
        let mut pk = vvec[0].clone();
        let mut hash = vvec_hash(&vvec, flag);
        match pc {
            Pc::Honest => {}
            Pc::Key(seed) => pk = secret_to_public(&make_new_key(&mut SeededRng::new(seed))),
            Pc::VvecHash(by) => hash[0] = hash[0].wrapping_add(by),
        }
        let qc = PrematureCommitment::unsigned(
            LlmqType::LlmqTest,
            &base().hash,
            &self.round.members[from].pro_tx_hash,
            valid,
            &pk,
            &hash,
            flag,
        );
        let sign_hash = *qc.sign_hash();
        let sig = self.round.keys[from].sign(&sign_hash, false);
        let quorum_sig = sign_stand_in(&self.sk_share(from, valid), &sign_hash, false);
        qc.with_quorum_sig(quorum_sig, flag).with_sig(sig, flag)
    }

    /// Member 0's session with every contribution received, its store answering with the
    /// round's members, less member `drop`'s `qdkg_V` row.
    fn receiver(&self, drop: Option<usize>) -> (DkgSession<TestMember>, TestEnv) {
        let (mut session, mut env) = self.round.contributed(0, 0..N);
        session.take_events();
        env.store.members = self.round.pro_txs();
        if let Some(d) = drop {
            let who = self.round.members[d].pro_tx_hash;
            env.store.rows.retain(|(_, _, row)| {
                !matches!(row, crate::dkg::reference::StoreRow::Vvec { who: w, .. } if *w == who)
            });
        }
        (session, env)
    }

    /// The premature commitments, each from its member, through one batch: every one accepted.
    fn receive(
        &self,
        session: &mut DkgSession<TestMember>,
        env: &mut TestEnv,
        msgs: &[(usize, &PrematureCommitment)],
    ) {
        let raws: Vec<(Sender<usize>, Vec<u8>)> =
            msgs.iter().map(|(from, qc)| (Sender::Peer(*from), qc.payload(BLS.scheme()))).collect();
        let batch: Vec<(Sender<usize>, &[u8])> = raws.iter().map(|(s, r)| (*s, &r[..])).collect();
        let outcome =
            session.process_batch(DkgMsgKind::PrematureCommitment, &batch, &mut env.env());
        assert!(outcome.misbehaving.is_empty(), "{:?}", outcome.misbehaving);
        assert_eq!(
            outcome.relay.len(),
            msgs.len(),
            "every one accepted: {:?}",
            lines(&outcome.events)
        );
    }

    /// `receiver(drop)` after `msgs` arrived, its events taken.
    fn received(
        &self,
        drop: Option<usize>,
        msgs: &[(usize, &PrematureCommitment)],
    ) -> (DkgSession<TestMember>, TestEnv) {
        let (mut session, mut env) = self.receiver(drop);
        self.receive(&mut session, &mut env, msgs);
        session.take_events();
        (session, env)
    }

    fn verifier(&self) -> ReferenceVerifier {
        ReferenceVerifier::new(LlmqType::LlmqTest, &self.round.params, 48, self.members())
    }
}

/// `FinalizeCommitments` under `verifier`, drawing nothing: the finals and the lines it logged.
fn finalize(
    session: &mut DkgSession<TestMember>,
    env: &mut TestEnv,
    verifier: &mut ReferenceVerifier,
) -> (Vec<LlmqCommitment>, Vec<DkgEvent>) {
    let mut rng = ReplayRng::new([]);
    let finals = session.finalize_commitments(&mut DkgEnv {
        rng: &mut rng,
        verify: verifier,
        ..env.env()
    });
    rng.finish().expect("finalization draws nothing");
    (finals, session.take_events())
}

/// The final Core builds from the commitments of `signers` over `valid`, its `quorumSig`
/// recovered from the shares of `recover_from` alone.
fn expected(
    fin: &Fin,
    valid: &[bool],
    signers: &[usize],
    recover_from: &[usize],
    pcs: &[&PrematureCommitment],
) -> LlmqCommitment {
    let flag = BLS.scheme();
    let pk = fin.quorum_vvec(valid)[0].clone();
    let by = |i: usize| {
        pcs.iter().find(|qc| *qc.pro_tx_hash() == fin.round.members[i].pro_tx_hash).expect("sent")
    };
    let sigs: Vec<&BlsSigValue> = signers.iter().map(|&i| by(i).sig()).collect();
    let pks: Vec<BlsPkValue> =
        signers.iter().map(|&i| fin.round.members[i].operator_pk.clone()).collect();
    let members_sig = aggregate_signatures_secure(&sigs, &pks.iter().collect::<Vec<_>>(), flag);
    let shares: Vec<&BlsSigValue> = recover_from.iter().map(|&i| by(i).quorum_sig()).collect();
    let ids: Vec<[u8; 32]> =
        recover_from.iter().map(|&i| *fin.round.members[i].pro_tx_hash.as_bytes()).collect();
    let quorum_sig = recover_signature(&shares, &ids).expect("two shares");
    let vvec_hash = vvec_hash(&fin.quorum_vvec(valid), flag);
    let hash = commitment_hash(LlmqType::LlmqTest, &base().hash, valid, &pk, &vvec_hash, BLS);
    assert!(verify_insecure(&quorum_sig, &pk, &hash, flag), "the quorum's signature");
    FinalFields {
        version: 3,
        llmq_type: LlmqType::LlmqTest,
        quorum_hash: base().hash,
        quorum_index: 0,
        signers: &bits(signers),
        valid_members: valid,
        quorum_public_key: &pk,
        quorum_vvec_hash: vvec_hash,
        quorum_sig,
        members_sig,
    }
    .commitment()
}

/// `pcs` sent by members 1, 2, … in turn.
fn from_one(pcs: &[PrematureCommitment]) -> Vec<(usize, &PrematureCommitment)> {
    pcs.iter().zip(1..N).map(|(qc, i)| (i, qc)).collect()
}

/// The commitments of `from` over `valid`, member `i`'s as `pc(i, seed)` has it, for the first
/// seed in 64 whose smallest session hash is the commitment of a member `first` takes.
fn find_group(
    fin: &Fin,
    valid: &[bool],
    from: &[usize],
    pc: impl Fn(usize, u64) -> Pc,
    first: impl Fn(usize) -> bool,
) -> Vec<PrematureCommitment> {
    (0..64)
        .map(|seed| from.iter().map(|&i| fin.pcommit(i, valid, pc(i, seed))).collect::<Vec<_>>())
        .find(|pcs| {
            let qc = pcs.iter().min_by_key(|qc| *qc.session_hash()).expect("a commitment");
            first(
                fin.members()
                    .iter()
                    .position(|m| m.pro_tx_hash == *qc.pro_tx_hash())
                    .expect("a member"),
            )
        })
        .expect("a seed in 64 orders the group")
}

fn final_line(valid: usize, pk: &BlsPkValue) -> String {
    format!(
        "{F} -- final commitment: validMembers={valid}, signers=3, quorumPublicKey={}",
        hex(pk, BLS.scheme())
    )
}

fn texts(events: &[DkgEvent]) -> Vec<String> {
    let record = events.first().and_then(|e| e.record);
    assert!(events.iter().all(|e| e.record.is_some() && e.record == record), "one record");
    lines(events)
}

#[test]
fn groups_go_in_valid_members_order_each_needing_min_size_commitments() {
    let fin = Fin::new();
    let all = bits(&[0, 1, 2, 3, 4]);
    let no0 = bits(&[1, 2, 3, 4]);
    let no4 = bits(&[0, 1, 2, 3]);
    let a: Vec<PrematureCommitment> =
        [1, 2, 3].iter().map(|&i| fin.pcommit(i, &all, Pc::Honest)).collect();
    let b: Vec<PrematureCommitment> =
        [2, 3, 4].iter().map(|&i| fin.pcommit(i, &no0, Pc::Honest)).collect();
    // Two commitments over `no4`: one fewer than minSize, so the group is skipped whole.
    let c: Vec<PrematureCommitment> =
        [1, 4].iter().map(|&i| fin.pcommit(i, &no4, Pc::Honest)).collect();
    // Grouped in the order each `validMembers` is first seen in hash order, `a` would go first.
    let smallest = a.iter().chain(&b).map(|qc| *qc.session_hash()).min().expect("six");
    assert!(a.iter().any(|qc| *qc.session_hash() == smallest));
    let msgs: Vec<(usize, &PrematureCommitment)> = a
        .iter()
        .zip([1, 2, 3])
        .chain(b.iter().zip([2, 3, 4]))
        .chain(c.iter().zip([1, 4]))
        .map(|(qc, i)| (i, qc))
        .collect();
    let (mut session, mut env) = fin.received(None, &msgs);
    let mut verifier = fin.verifier();
    let (finals, events) = finalize(&mut session, &mut env, &mut verifier);

    // `[0, 1, 1, 1, 1]` sorts before `[1, 1, 1, 1, 1]`.
    let want_b = expected(&fin, &no0, &[2, 3, 4], &[3, 4], &b.iter().collect::<Vec<_>>());
    let want_a = expected(&fin, &all, &[1, 2, 3], &[1, 2], &a.iter().collect::<Vec<_>>());
    assert_eq!(finals, [want_b.clone(), want_a.clone()]);
    let bytes: Vec<Vec<u8>> = finals.iter().map(final_commitment_bytes).collect();
    assert_eq!(bytes, [final_commitment_bytes(&want_b), final_commitment_bytes(&want_a)]);
    assert_eq!(verifier.member_reads(), [2, 2], "each verified final reads the members twice");
    assert_eq!(
        texts(&events),
        [final_line(4, &fin.quorum_vvec(&no0)[0]), final_line(5, &fin.quorum_vvec(&all)[0])]
    );
}

#[test]
fn an_entry_whose_key_or_vvec_hash_differs_from_the_first_is_skipped() {
    let fin = Fin::new();
    let all = bits(&[0, 1, 2, 3, 4]);
    for (what, pc) in
        [("key", Pc::Key as fn(u64) -> Pc), ("vvec hash", |s| Pc::VvecHash(1 + s as u8))]
    {
        // Accepted unverified: the receiver lacks member 0's `qdkg_V` row.
        let pcs = find_group(
            &fin,
            &all,
            &[1, 2, 3, 4],
            |i, s| {
                if i == 4 {
                    pc(s)
                } else {
                    Pc::Honest
                }
            },
            |f| f != 4,
        );
        let (mut session, mut env) = fin.received(Some(0), &from_one(&pcs));
        let mut verifier = fin.verifier();
        let (finals, events) = finalize(&mut session, &mut env, &mut verifier);
        let want = expected(&fin, &all, &[1, 2, 3], &[2, 3], &pcs.iter().collect::<Vec<_>>());
        assert_eq!(finals, std::slice::from_ref(&want), "{what}");
        assert_eq!(
            finals.iter().map(final_commitment_bytes).collect::<Vec<_>>(),
            [final_commitment_bytes(&want)],
            "{what}"
        );
        assert_eq!(verifier.member_reads(), [2], "{what}");
        let texts = texts(&events);
        assert_eq!(texts.len(), 2, "{what}: {texts:?}");
        assert_eq!(texts[0], SKIP, "{what}");
        assert!(
            texts[1].starts_with(&format!("{F} -- final commitment: validMembers=5, signers=3,")),
            "{what}"
        );
    }
}

#[test]
fn an_incompatible_first_is_kept() {
    // The first commitment's key is not the others': Core keeps it, skips the three that agree
    // with each other, and recovers from one share, which fails. No regrouping, no largest
    // subset.
    let fin = Fin::new();
    let all = bits(&[0, 1, 2, 3, 4]);
    let pcs = find_group(
        &fin,
        &all,
        &[1, 2, 3, 4],
        |i, s| {
            if i == 2 {
                Pc::Key(s)
            } else {
                Pc::Honest
            }
        },
        |f| f == 2,
    );
    let (mut session, mut env) = fin.received(Some(0), &from_one(&pcs));
    let mut verifier = fin.verifier();
    let (finals, events) = finalize(&mut session, &mut env, &mut verifier);
    assert!(finals.is_empty());
    assert!(verifier.calls.is_empty(), "no Verify, no members read");
    assert_eq!(texts(&events), [SKIP, SKIP, SKIP, NO_RECOVER]);
}

#[test]
fn incompatible_firsts_recover_and_fail_the_quorum_signature() {
    // Members 1 to 3 commit to the same fresh key, and one of them is first: the final has
    // their three signatures, its recovered `quorumSig` is not the fresh key's, and `Verify`
    // reaches the signature checks.
    let fin = Fin::new();
    let all = bits(&[0, 1, 2, 3, 4]);
    let pcs = find_group(
        &fin,
        &all,
        &[1, 2, 3, 4],
        |i, s| {
            if i <= 3 {
                Pc::Key(s)
            } else {
                Pc::Honest
            }
        },
        |f| f != 4,
    );
    let (mut session, mut env) = fin.received(Some(0), &from_one(&pcs));
    let mut verifier = fin.verifier();
    let (finals, events) = finalize(&mut session, &mut env, &mut verifier);
    assert!(finals.is_empty());
    assert_eq!(texts(&events), [SKIP, UNVERIFIED]);
    let [call] = &verifier.calls[..] else {
        panic!("one Verify")
    };
    assert_eq!(call.member_reads, 2);
    assert_eq!(call.verdict, Err(CommitmentError::Rejected(CommitmentReject::QuorumSignature)));
    let signers: Vec<bool> =
        (0..N as u64).map(|i| call.qc.signers().get(i) == Some(true)).collect();
    assert_eq!(signers, bits(&[1, 2, 3]));
}

#[test]
fn verify_reads_the_members_zero_one_or_two_times() {
    let fin = Fin::new();
    let all = bits(&[0, 1, 2, 3, 4]);
    let honest: Vec<PrematureCommitment> =
        (1..4).map(|i| fin.pcommit(i, &all, Pc::Honest)).collect();
    let run =
        |verifier: &mut ReferenceVerifier, pcs: &[PrematureCommitment], drop: Option<usize>| {
            let (mut session, mut env) = fin.received(drop, &from_one(pcs));
            let (finals, events) = finalize(&mut session, &mut env, verifier);
            (finals.len(), texts(&events))
        };

    // Two of four agree with the first: two signers, below minSize, refused before the first
    // members read (`l/commitment.cpp:156-160`).
    let pcs = find_group(
        &fin,
        &all,
        &[1, 2, 3, 4],
        |i, s| {
            if i >= 3 {
                Pc::Key(s + i as u64)
            } else {
                Pc::Honest
            }
        },
        |f| f == 1 || f == 2,
    );
    let mut verifier = fin.verifier();
    assert_eq!(
        run(&mut verifier, &pcs, Some(0)),
        (0, vec![SKIP.into(), SKIP.into(), UNVERIFIED.into()])
    );
    assert_eq!(
        verifier.calls[0].verdict,
        Err(CommitmentError::Rejected(CommitmentReject::SignersCount))
    );
    assert_eq!(verifier.member_reads(), [0]);

    // The members list ends before member 4, whose valid bit is set: refused after the first
    // read (:177-189).
    let mut verifier = fin.verifier();
    verifier.members = verifier.members[..4].into();
    assert_eq!(run(&mut verifier, &honest, None), (0, vec![UNVERIFIED.into()]));
    assert_eq!(
        verifier.calls[0].verdict,
        Err(CommitmentError::Rejected(CommitmentReject::ValidMemberBit(4)))
    );
    assert_eq!(verifier.member_reads(), [1]);

    // Member 1's operator key is another one: refused at the members signature, after the
    // second read (:53).
    let mut verifier = fin.verifier();
    let mut members = fin.members().to_vec();
    members[1].operator_pk = fin.round.keys[4].public_key();
    verifier.members = members.into();
    assert_eq!(run(&mut verifier, &honest, None), (0, vec![UNVERIFIED.into()]));
    assert_eq!(
        verifier.calls[0].verdict,
        Err(CommitmentError::Rejected(CommitmentReject::MembersSignature))
    );
    assert_eq!(verifier.member_reads(), [2]);

    // Accepted: two reads.
    let mut verifier = fin.verifier();
    assert_eq!(run(&mut verifier, &honest, None).0, 1);
    assert_eq!(verifier.member_reads(), [2]);
}

#[test]
fn the_version_and_quorum_index_follow_rotation_and_v19() {
    let fin = Fin::new();
    let all = bits(&[0, 1, 2, 3, 4]);
    let pcs: Vec<PrematureCommitment> = (1..4).map(|i| fin.pcommit(i, &all, Pc::Honest)).collect();
    for (rotation, v19, version, index) in [
        (false, true, 3, None),
        (false, false, 1, None),
        (true, true, 4, Some(2)),
        (true, false, 2, Some(2)),
    ] {
        let (mut session, mut env) = fin.received(None, &from_one(&pcs));
        session.rotation_enabled = rotation;
        session.v19_after_base = v19;
        session.quorum_index = 2;
        env.verify.accept = true;
        let finals = session.finalize_commitments(&mut env.env());
        let [qc] = &finals[..] else {
            panic!("one final")
        };
        assert_eq!(
            (qc.version(), qc.quorum_index()),
            (version, index),
            "rotation {rotation}, v19 {v19}"
        );
        assert_eq!(env.verify.calls.len(), 1);
    }
}

#[test]
fn a_group_that_fails_does_not_stop_the_next() {
    let fin = Fin::new();
    let all = bits(&[0, 1, 2, 3, 4]);
    let no0 = bits(&[1, 2, 3, 4]);
    let a: Vec<PrematureCommitment> =
        [1, 2, 3].iter().map(|&i| fin.pcommit(i, &all, Pc::Honest)).collect();
    let want_a = expected(&fin, &all, &[1, 2, 3], &[1, 2], &a.iter().collect::<Vec<_>>());
    let line_a = final_line(5, &fin.quorum_vvec(&all)[0]);
    fn sent<'a>(
        a: &'a [PrematureCommitment],
        b: &'a [PrematureCommitment],
    ) -> Vec<(usize, &'a PrematureCommitment)> {
        a.iter().zip([1, 2, 3]).chain(b.iter().zip([2, 3, 4])).map(|(qc, i)| (i, qc)).collect()
    }

    // `[0, 1, 1, 1, 1]` goes first: its first commitment has member 2's own key, the other two
    // are skipped, and one share does not recover. Accepted unverified: the receiver lacks
    // member 4's `qdkg_V` row.
    let b = find_group(
        &fin,
        &no0,
        &[2, 3, 4],
        |i, s| {
            if i == 2 {
                Pc::Key(s)
            } else {
                Pc::Honest
            }
        },
        |f| f == 2,
    );
    let (mut session, mut env) = fin.received(Some(4), &sent(&a, &b));
    let mut verifier = fin.verifier();
    let (finals, events) = finalize(&mut session, &mut env, &mut verifier);
    assert_eq!(finals, std::slice::from_ref(&want_a));
    assert_eq!(texts(&events), [SKIP, SKIP, NO_RECOVER, line_a.as_str()]);
    assert_eq!(verifier.member_reads(), [2]);

    // `[0, 1, 1, 1, 1]` recovers, but member 4's operator key is another one at the verifier:
    // `Verify` refuses it.
    let b: Vec<PrematureCommitment> =
        [2, 3, 4].iter().map(|&i| fin.pcommit(i, &no0, Pc::Honest)).collect();
    let (mut session, mut env) = fin.received(None, &sent(&a, &b));
    let mut verifier = fin.verifier();
    let mut members = fin.members().to_vec();
    members[4].operator_pk = fin.round.keys[0].public_key();
    verifier.members = members.into();
    let (finals, events) = finalize(&mut session, &mut env, &mut verifier);
    assert_eq!(finals, [want_a]);
    assert_eq!(texts(&events), [UNVERIFIED, line_a.as_str()]);
    assert_eq!(
        verifier.calls[0].verdict,
        Err(CommitmentError::Rejected(CommitmentReject::MembersSignature))
    );
    assert_eq!(verifier.member_reads(), [2, 2]);
}

#[test]
fn a_commitment_the_receiver_rejected_is_left_out() {
    // Member 4's key is not the one the received contributions build: recorded, not accepted,
    // so the group is members 1-3's alone, with nothing skipped.
    let fin = Fin::new();
    let all = bits(&[0, 1, 2, 3, 4]);
    let honest: Vec<PrematureCommitment> =
        (1..4).map(|i| fin.pcommit(i, &all, Pc::Honest)).collect();
    let (mut session, mut env) = fin.received(None, &from_one(&honest));
    let rejected = fin.pcommit(4, &all, Pc::Key(7)).payload(BLS.scheme());
    let out = session.process_batch(
        DkgMsgKind::PrematureCommitment,
        &[(Sender::Peer(4), &rejected[..])],
        &mut env.env(),
    );
    assert!(out.relay.is_empty());
    assert_eq!(session.premature_commitments.len(), 4);
    session.take_events();
    let mut verifier = fin.verifier();
    let (finals, events) = finalize(&mut session, &mut env, &mut verifier);
    let want = expected(&fin, &all, &[1, 2, 3], &[1, 2], &honest.iter().collect::<Vec<_>>());
    assert_eq!(finals, [want]);
    assert_eq!(texts(&events), [final_line(5, &fin.quorum_vvec(&all)[0])]);
}

#[test]
fn an_observer_finalizes_nothing_from_the_commitments_it_accepted() {
    let fin = Fin::new();
    let all = bits(&[0, 1, 2, 3, 4]);
    let honest: Vec<PrematureCommitment> =
        (1..4).map(|i| fin.pcommit(i, &all, Pc::Honest)).collect();
    let mut session = member_session(&fin.round.params, fin.members(), None, true);
    let mut env = TestEnv::new(&fin.round.keys[0], 1);
    env.store = fin.receiver(None).1.store;
    fin.receive(&mut session, &mut env, &from_one(&honest));
    session.take_events();
    assert_eq!(session.valid_commitments.len(), 3);
    let mut verifier = fin.verifier();
    let (finals, events) = finalize(&mut session, &mut env, &mut verifier);
    assert!(finals.is_empty());
    assert!(verifier.calls.is_empty());
    assert!(events.is_empty());
}

#[test]
fn a_non_member_finalizes_nothing() {
    let (members, keys) = quorum(3, 0x0b);
    let mut session = member_session(&params(LlmqType::LlmqTest), &members, None, true);
    let mut env = TestEnv::new(&keys[0], 0);
    let mut rng = ReplayRng::new([]);
    {
        let mut dkg_env = DkgEnv {
            rng: &mut rng,
            ..env.env()
        };
        assert!(session.finalize_commitments(&mut dkg_env).is_empty());
        assert_eq!(session.finalize_single_commitment(&mut dkg_env), Ok(None));
    }
    rng.finish().expect("no draw");
    assert!(env.verify.calls.is_empty());
    assert!(session.take_events().is_empty());
}

/// LLMQ_TEST overridden to one member, `-llmqtestparams=1:1`.
fn single_params() -> dash_params::LlmqParams {
    dash_params::LlmqParams {
        size: 1,
        min_size: 1,
        threshold: 1,
        dkg_bad_votes_threshold: 1,
        ..params(LlmqType::LlmqTest)
    }
}

fn single(v19: bool) -> (DkgSession<TestMember>, TestMember, TestOperatorKey) {
    let (members, keys) = quorum(1, 0x51);
    let (session, _, _) = init_params(
        LlmqType::LlmqTest,
        &single_params(),
        &members,
        Some(members[0].pro_tx_hash),
        v19,
    );
    let mut session = session.expect("one member");
    session.take_events();
    (session, members[0].clone(), keys[0].clone())
}

#[test]
fn a_single_member_signs_with_its_operator_key_after_one_sk1_draw() {
    for (v19, bls, version) in [(true, BlsCtx::BASIC, 3), (false, BlsCtx::LEGACY, 1)] {
        let (mut session, member, key) = single(v19);
        // `GetVersion(false, …)`, whatever the session's rotation.
        session.rotation_enabled = true;
        session.quorum_index = 1;
        let mut env = TestEnv::new(&key, 0);
        let mut verifier = ReferenceVerifier::new(
            LlmqType::LlmqTest,
            &single_params(),
            48,
            std::slice::from_ref(&member),
        );
        verifier.v19_after_base = v19;
        verifier.bls = bls;
        let sk1 = make_new_key(&mut SeededRng::new(9));
        let mut rng = ReplayRng::new([Draw::Key(KeyDraw::SingleMemberSk1, sk1)]);
        let qc = session
            .finalize_single_commitment(&mut DkgEnv {
                bls,
                rng: &mut rng,
                verify: &mut verifier,
                ..env.env()
            })
            .expect("verified")
            .expect("a member");
        rng.finish().expect("one sk1 draw");
        let flag = bls.scheme();
        let pk = key.public_key();
        let hash = commitment_hash(LlmqType::LlmqTest, &base().hash, &[true], &pk, &[0; 32], bls);
        let sig = key.sign(&hash, !v19);
        let want = FinalFields {
            version,
            llmq_type: LlmqType::LlmqTest,
            quorum_hash: base().hash,
            quorum_index: 0,
            signers: &[true],
            valid_members: &[true],
            quorum_public_key: &pk,
            quorum_vvec_hash: [0; 32],
            quorum_sig: sig.clone(),
            members_sig: sig,
        }
        .commitment();
        assert_eq!(final_commitment_bytes(&qc), final_commitment_bytes(&want), "v19 {v19}");
        assert_eq!(qc, want);
        assert_eq!(verifier.member_reads(), [2]);
        assert_eq!(
            lines(&session.take_events()),
            [format!(
                "FinalizeSingleCommitment -- final commitment: validMembers=1, signers=1, quorumPublicKey={}",
                hex(&pk, flag)
            )]
        );
    }
}

#[test]
fn a_single_member_commitment_that_does_not_verify_is_a_fault() {
    let (mut session, member, key) = single(true);
    let mut env = TestEnv::new(&key, 0);
    let mut other = member;
    other.operator_pk = secret_to_public(&make_new_key(&mut SeededRng::new(3)));
    let mut verifier = ReferenceVerifier::new(LlmqType::LlmqTest, &single_params(), 48, &[other]);
    let got = session.finalize_single_commitment(&mut DkgEnv {
        verify: &mut verifier,
        ..env.env()
    });
    assert_eq!(got, Err(DkgFault::SingleCommitmentUnverified));
    assert_eq!(
        verifier.calls[0].verdict,
        Err(CommitmentError::Rejected(CommitmentReject::MemberSignature))
    );
    assert!(session.take_events().is_empty(), "Core aborts before its record is written");
}
