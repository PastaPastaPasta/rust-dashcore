//! `PreVerifyMessage` for the four messages: every row of LQ4 §2.3's table, in Core's order
//! (an earlier check wins when two fail), with Core's log text and score.

use super::*;
use crate::dkg::bls::{
    BlsIesMultiBlobs, BlsPkValue, BlsSchemeKind, BlsSkValue, IesSeed, encrypt_shares,
    secret_to_public,
};
use crate::dkg::reference::TestMember;
use crate::dkg::rng::SeededRng;
use crate::dkg::session::fixture::{BLS, base, params, session, valid_sig_bytes};
use crate::dkg::wire::LlmqType;
use crate::dkg::wire::testing::{
    Prefix, complaint, contribution, justification, premature_commitment,
};

const TEST: LlmqType = LlmqType::LlmqTest;

fn prefix(members: &[TestMember], who: usize) -> Prefix {
    Prefix {
        t: TEST,
        quorum_hash: base().hash,
        pro_tx_hash: members[who].pro_tx_hash,
    }
}

fn outsider() -> Prefix {
    Prefix {
        t: TEST,
        quorum_hash: base().hash,
        pro_tx_hash: ProTxHash::from([0xee; 32]),
    }
}

fn wrong_quorum(p: Prefix) -> Prefix {
    Prefix {
        quorum_hash: QuorumHash::from([1; 32]),
        ..p
    }
}

/// The last logged line and the reject, after the check.
fn outcome(
    session: &mut DkgSession<TestMember>,
    result: Result<(), DkgReject>,
) -> (Option<DkgReject>, String) {
    let text = session.take_events().pop().map(|e| e.text).unwrap_or_default();
    (result.err(), text)
}

/// A verification vector of `threshold` distinct keys and `n` encrypted blobs.
fn contribution_parts(n: usize, threshold: usize) -> (Vec<BlsPkValue>, BlsIesMultiBlobs) {
    let mut rng = SeededRng::new(77);
    let vvec = (0..threshold)
        .map(|_| secret_to_public(&crate::dkg::bls::make_new_key(&mut rng)))
        .collect();
    let keys: Vec<(BlsPkValue, BlsSkValue)> = (0..n)
        .map(|_| {
            let sk = crate::dkg::bls::make_new_key(&mut rng);
            (secret_to_public(&sk), sk)
        })
        .collect();
    let refs: Vec<(&BlsPkValue, &BlsSkValue)> = keys.iter().map(|(pk, sk)| (pk, sk)).collect();
    let seed = IesSeed {
        ephemeral_sk: crate::dkg::bls::make_new_key(&mut rng),
        iv_seed: [3; 32],
    };
    (vvec, encrypt_shares(&seed, &refs, BlsSchemeKind::Basic).0)
}

fn read_contribution(payload: &[u8]) -> Contribution {
    Contribution::read(payload, params(TEST), BLS).expect("reads")
}

#[test]
fn contribution_checks_in_cores_order() {
    let (mut s, members, _) = session(TEST, 3, Some(0));
    let (vvec, blobs) = contribution_parts(3, 2);
    let sig = valid_sig_bytes();
    let check = |s: &mut DkgSession<TestMember>,
                 p: &Prefix,
                 vvec: &[BlsPkValue],
                 blobs: &BlsIesMultiBlobs,
                 sig| {
        let msg = read_contribution(&contribution(p, vvec, blobs, sig));
        let result = s.pre_verify_contribution(&msg, BLS);
        outcome(s, result)
    };
    assert_eq!(check(&mut s, &prefix(&members, 1), &vvec, &blobs, sig).0, None);

    // The quorum before membership, membership before the signature.
    let row = check(&mut s, &wrong_quorum(outsider()), &vvec, &blobs, [0; 96]);
    assert_eq!(
        row,
        (Some(DkgReject::WrongQuorum), "contribution for wrong quorum, rejecting".into())
    );
    let row = check(&mut s, &outsider(), &vvec, &blobs, [0; 96]);
    assert_eq!(
        row,
        (
            Some(DkgReject::NotMember),
            "contributor not a member of this quorum, rejecting contribution".into()
        )
    );
    // The signature before the blob count.
    let (_, two_blobs) = contribution_parts(2, 2);
    let row = check(&mut s, &prefix(&members, 1), &vvec, &two_blobs, [0; 96]);
    assert_eq!(row, (Some(DkgReject::InvalidSig), "invalid contribution signature".into()));
    let row = check(&mut s, &prefix(&members, 1), &vvec, &two_blobs, sig);
    assert_eq!(
        row,
        (Some(DkgReject::InvalidContributionsCount), "invalid contributions count".into())
    );
    // `vvec.size() != threshold` is the structure check's first (Core checks it twice), so a
    // read contribution meets the vector check next: an invalid key, then a repeated one.
    let row =
        check(&mut s, &prefix(&members, 1), &[vvec[0].clone(), BlsPkValue::invalid()], &blobs, sig);
    assert_eq!(row, (Some(DkgReject::InvalidVvec), "invalid verification vector".into()));
    let row = check(&mut s, &prefix(&members, 1), &[vvec[0].clone(), vvec[0].clone()], &blobs, sig);
    assert_eq!(row.0, Some(DkgReject::InvalidVvec));

    // Two recorded already: dropped without a score.
    s.member_state[1].contributions.extend([[1; 32], [2; 32]]);
    let (reject, text) = check(&mut s, &prefix(&members, 1), &vvec, &blobs, sig);
    assert_eq!(reject, Some(DkgReject::TooMany));
    assert_eq!(
        text,
        format!(
            "dropping contribution from {} as we already got 2 contributions",
            members[1].pro_tx_hash
        )
    );
    assert!(!DkgReject::TooMany.bans() && !DkgReject::WrongQuorum.bans());
    assert!(DkgReject::InvalidVvec.bans() && DkgReject::Deserialize.bans());
}

#[test]
fn a_contribution_with_fewer_members_than_size_needs_one_blob_each() {
    // Two members of llmq_test (size 3): three blobs pass the structure check, not PreVerify.
    let (mut s, members, _) = session(TEST, 2, Some(0));
    let (vvec, blobs) = contribution_parts(3, 2);
    let msg =
        read_contribution(&contribution(&prefix(&members, 1), &vvec, &blobs, valid_sig_bytes()));
    assert_eq!(s.pre_verify_contribution(&msg, BLS), Err(DkgReject::InvalidContributionsCount));
}

#[test]
fn complaint_checks_in_cores_order() {
    let (mut s, members, _) = session(TEST, 3, Some(0));
    let sig = valid_sig_bytes();
    let check = |s: &mut DkgSession<TestMember>, p: &Prefix, bits: usize, sig| {
        let payload = complaint(p, &vec![false; bits], &vec![false; bits], sig);
        let msg = Complaint::read(&payload, params(TEST), BLS).expect("reads");
        let result = s.pre_verify_complaint(&msg);
        outcome(s, result)
    };
    assert_eq!(check(&mut s, &prefix(&members, 2), 3, sig).0, None);
    let row = check(&mut s, &wrong_quorum(outsider()), 3, [0; 96]);
    assert_eq!(row, (Some(DkgReject::WrongQuorum), "complaint for wrong quorum, rejecting".into()));
    let row = check(&mut s, &outsider(), 3, [0; 96]);
    assert_eq!(
        row,
        (
            Some(DkgReject::NotMember),
            "complainer not a member of this quorum, rejecting complaint".into()
        )
    );
    let row = check(&mut s, &prefix(&members, 2), 2, [0; 96]);
    assert_eq!(row, (Some(DkgReject::InvalidSig), "invalid complaint signature".into()));
    // The structure check makes both bitsets one size, so badMembers' check is the one met.
    let row = check(&mut s, &prefix(&members, 2), 2, sig);
    assert_eq!(
        row,
        (Some(DkgReject::InvalidBadMembersSize), "invalid badMembers bitset size".into())
    );
    s.member_state[2].complaints.extend([[1; 32], [2; 32]]);
    let (reject, text) = check(&mut s, &prefix(&members, 2), 3, sig);
    assert_eq!(reject, Some(DkgReject::TooMany));
    assert_eq!(
        text,
        format!(
            "dropping complaint from {} as we already got 2 complaints",
            members[2].pro_tx_hash
        )
    );
}

#[test]
fn justification_checks_in_cores_order() {
    let (mut s, members, _) = session(TEST, 3, Some(0));
    let sig = valid_sig_bytes();
    let mut key = [0u8; 32];
    key[31] = 5;
    let check = |s: &mut DkgSession<TestMember>, p: &Prefix, shares: &[(u32, [u8; 32])], sig| {
        let payload = justification(p, shares, sig);
        let msg = Justification::read(&payload, params(TEST), BLS).expect("reads");
        let result = s.pre_verify_justification(&msg);
        outcome(s, result)
    };
    assert_eq!(check(&mut s, &prefix(&members, 1), &[(0, key), (2, key)], sig).0, None);
    let row = check(&mut s, &wrong_quorum(outsider()), &[], [0; 96]);
    assert_eq!(
        row,
        (Some(DkgReject::WrongQuorum), "justification for wrong quorum, rejecting".into())
    );
    let row = check(&mut s, &outsider(), &[], [0; 96]);
    assert_eq!(
        row,
        (
            Some(DkgReject::NotMember),
            "justifier not a member of this quorum, rejecting justification".into()
        )
    );
    let row = check(&mut s, &prefix(&members, 1), &[], [0; 96]);
    assert_eq!(row, (Some(DkgReject::InvalidSig), "invalid justification signature".into()));
    let row = check(&mut s, &prefix(&members, 1), &[], sig);
    assert_eq!(
        row,
        (Some(DkgReject::NoContributions), "justification with no contributions".into())
    );
    let row = check(&mut s, &prefix(&members, 1), &[(3, key)], sig);
    assert_eq!(
        row,
        (Some(DkgReject::InvalidContributionIndex), "invalid contribution index".into())
    );
    let row = check(&mut s, &prefix(&members, 1), &[(1, key), (1, key)], sig);
    assert_eq!(
        row,
        (Some(DkgReject::DuplicateContributionIndex), "duplicate contribution index".into())
    );
    let row = check(&mut s, &prefix(&members, 1), &[(1, [0; 32])], sig);
    assert_eq!(row, (Some(DkgReject::InvalidContribution), "invalid contribution".into()));
    // Entries are checked in order: an invalid key before a bad index wins.
    let row = check(&mut s, &prefix(&members, 1), &[(0, [0; 32]), (9, key)], sig);
    assert_eq!(row.0, Some(DkgReject::InvalidContribution));
    s.member_state[1].justifications.extend([[1; 32], [2; 32]]);
    let (reject, text) = check(&mut s, &prefix(&members, 1), &[(0, key)], sig);
    assert_eq!(reject, Some(DkgReject::TooMany));
    assert_eq!(
        text,
        format!(
            "dropping justification from {} as we already got 2 justifications",
            members[1].pro_tx_hash
        )
    );
}

#[test]
fn premature_commitment_checks_in_cores_order() {
    // Two members of llmq_test (size 3, minSize 2), so bit 2 lies past the members.
    let (mut s, members, _) = session(TEST, 2, Some(0));
    let sig = valid_sig_bytes();
    let check = |s: &mut DkgSession<TestMember>, p: &Prefix, valid: &[bool], quorum_sig, sig| {
        let payload = premature_commitment(p, valid, quorum_sig, sig);
        let msg = PrematureCommitment::read(&payload, params(TEST), BLS).expect("reads");
        let result = s.pre_verify_premature_commitment(&msg);
        outcome(s, result)
    };
    let ok = [true, true, false];
    assert_eq!(check(&mut s, &prefix(&members, 1), &ok, sig, sig).0, None);
    let row = check(&mut s, &wrong_quorum(outsider()), &ok, sig, sig);
    assert_eq!(
        row,
        (Some(DkgReject::WrongQuorum), "commitment for wrong quorum, rejecting".into())
    );
    let row = check(&mut s, &outsider(), &[true], [0; 96], [0; 96]);
    assert_eq!(
        row,
        (
            Some(DkgReject::NotMember),
            "committer not a member of this quorum, rejecting premature commitment".into()
        )
    );
    let row = check(&mut s, &prefix(&members, 1), &[true, true], [0; 96], [0; 96]);
    assert_eq!(
        row,
        (Some(DkgReject::InvalidValidMembersSize), "invalid validMembers bitset size".into())
    );
    // The count before either signature.
    let row = check(&mut s, &prefix(&members, 1), &[true, false, false], [0; 96], [0; 96]);
    assert_eq!(
        row,
        (
            Some(DkgReject::InvalidValidMembersCount),
            "invalid validMembers count. validMembersCount=1".into()
        )
    );
    let row = check(&mut s, &prefix(&members, 1), &ok, [0; 96], [0; 96]);
    assert_eq!(row, (Some(DkgReject::InvalidSig), "invalid membersSig".into()));
    let row = check(&mut s, &prefix(&members, 1), &ok, [0; 96], sig);
    assert_eq!(row, (Some(DkgReject::InvalidQuorumSig), "invalid quorumSig".into()));
    let row = check(&mut s, &prefix(&members, 1), &[true, true, true], sig, sig);
    assert_eq!(
        row,
        (
            Some(DkgReject::ValidMemberBitPastMembers),
            "invalid validMembers bitset. bit 2 should not be set".into()
        )
    );
    s.member_state[1].premature_commitments.extend([[1; 32], [2; 32]]);
    let (reject, text) = check(&mut s, &prefix(&members, 1), &ok, sig, sig);
    assert_eq!(reject, Some(DkgReject::TooMany));
    assert_eq!(
        text,
        format!(
            "dropping commitment from {} as we already got 2 commitments",
            members[1].pro_tx_hash
        )
    );
}

#[test]
fn each_pre_verify_starts_its_own_record() {
    let (mut s, members, _) = session(TEST, 3, Some(0));
    let mut records = BTreeSet::new();
    let mut last_record = |s: &mut DkgSession<TestMember>| {
        let record = s.take_events().last().and_then(|e| e.record).expect("a batched line");
        assert!(records.insert(record), "record {record} reused");
    };
    last_record(&mut s);
    let p = wrong_quorum(prefix(&members, 1));
    let sig = valid_sig_bytes();
    let (vvec, blobs) = contribution_parts(3, 2);
    let qc = read_contribution(&contribution(&p, &vvec, &blobs, sig));
    assert_eq!(s.pre_verify_contribution(&qc, BLS), Err(DkgReject::WrongQuorum));
    last_record(&mut s);
    let qc = Complaint::read(&complaint(&p, &[false; 3], &[false; 3], sig), params(TEST), BLS);
    assert_eq!(s.pre_verify_complaint(&qc.expect("reads")), Err(DkgReject::WrongQuorum));
    last_record(&mut s);
    let mut key = [0u8; 32];
    key[31] = 5;
    let qj = Justification::read(&justification(&p, &[(0, key)], sig), params(TEST), BLS);
    assert_eq!(s.pre_verify_justification(&qj.expect("reads")), Err(DkgReject::WrongQuorum));
    last_record(&mut s);
    let payload = premature_commitment(&p, &[true, true, true], sig, sig);
    let qc = PrematureCommitment::read(&payload, params(TEST), BLS).expect("reads");
    assert_eq!(s.pre_verify_premature_commitment(&qc), Err(DkgReject::WrongQuorum));
    last_record(&mut s);
}
