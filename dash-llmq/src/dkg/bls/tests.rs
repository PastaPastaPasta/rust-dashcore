//! The BLS adapters against dash-pkc's own DKG operations.

use getrandom::SysRng;
use rand_core::UnwrapErr;

use super::*;
use crate::dkg::rng::SeededRng;

/// [`verify_contribution_shares`] aggregated under the basic scheme, its weights from the OS.
fn verify_basic(
    for_id: &[u8; 32],
    vvecs: &[&[BlsPkValue]],
    shares: &[&BlsSkValue],
) -> ContributionVerdicts {
    verify_contribution_shares(
        for_id,
        vvecs,
        shares,
        true,
        BlsSchemeKind::Basic,
        &mut UnwrapErr(SysRng),
    )
}

fn ids(n: u8) -> Vec<[u8; 32]> {
    (1..=n).map(|i| member_id(&ProTxHash::from([i; 32]))).collect()
}

#[test]
fn injected_coefficients_give_dash_pkcs_contributions() {
    let ids = ids(5);
    let mut theirs = SeededRng::new(7);
    let (vvec, shares) =
        core::generate_contributions(3, &ids, &mut theirs).expect("t = 3 succeeds");
    let mut ours = SeededRng::new(7);
    let coefficients: Vec<BlsSkValue> = (0..3).map(|_| make_new_key(&mut ours)).collect();
    let (our_vvec, our_shares) = generate_contributions(&coefficients, &ids).expect("succeeds");
    assert_eq!(our_vvec, vvec);
    assert_eq!(our_shares, shares);
    assert!(verify_verification_vector(&vvec, BlsSchemeKind::Basic));
    for (id, share) in ids.iter().zip(&shares) {
        assert!(verify_contribution_share(id, &vvec, share, BlsSchemeKind::Basic));
    }
    // Below two coefficients every share fails, as Core's SecretKeyShare does.
    assert!(generate_contributions(&coefficients[..1], &ids).is_none());
}

#[test]
fn shares_verify_in_batches_and_a_wrong_share_fails_alone() {
    let ids = ids(4);
    let mut rng = SeededRng::new(11);
    let dealers: Vec<(Vec<BlsPkValue>, Vec<BlsSkValue>)> = (0..10)
        .map(|_| {
            let coefficients: Vec<BlsSkValue> = (0..3).map(|_| make_new_key(&mut rng)).collect();
            generate_contributions(&coefficients, &ids).expect("succeeds")
        })
        .collect();
    let me = 2;
    let vvecs: Vec<&[BlsPkValue]> = dealers.iter().map(|(v, _)| v.as_slice()).collect();
    let mut shares: Vec<BlsSkValue> = dealers.iter().map(|(_, s)| s[me].clone()).collect();
    let refs: Vec<&BlsSkValue> = shares.iter().collect();
    assert_eq!(
        verify_basic(&ids[me], &vvecs, &refs),
        ContributionVerdicts {
            verdicts: vec![true; 10],
            individually: false
        }
    );
    // Dealer 9's share is for someone else: the second 8-chunk falls back to single checks.
    shares[9] = dealers[9].1[0].clone();
    let refs: Vec<&BlsSkValue> = shares.iter().collect();
    let mut expected = vec![true; 10];
    expected[9] = false;
    assert_eq!(
        verify_basic(&ids[me], &vvecs, &refs),
        ContributionVerdicts {
            verdicts: expected,
            individually: false
        }
    );
}

#[test]
fn vectors_sharing_a_coefficient_are_each_checked_alone() {
    // `VerifyVerificationVectors` refuses vectors that share a coefficient, and each
    // contribution is then checked on its own (`bls/bls_worker.cpp:706-736`, v24.0.0): only the
    // dealer whose share no longer fits its own vector fails.
    let ids = ids(4);
    let mut rng = SeededRng::new(12);
    let dealers: Vec<(Vec<BlsPkValue>, Vec<BlsSkValue>)> = (0..3)
        .map(|_| {
            let coefficients: Vec<BlsSkValue> = (0..3).map(|_| make_new_key(&mut rng)).collect();
            generate_contributions(&coefficients, &ids).expect("succeeds")
        })
        .collect();
    let me = 1;
    let mut copied = dealers[2].0.clone();
    copied[1] = dealers[1].0[1].clone();
    let vvecs: Vec<&[BlsPkValue]> = vec![&dealers[0].0, &dealers[1].0, &copied];
    let shares: Vec<&BlsSkValue> = dealers.iter().map(|(_, s)| &s[me]).collect();
    let want = ContributionVerdicts {
        verdicts: vec![true, true, false],
        individually: true,
    };
    assert_eq!(verify_basic(&ids[me], &vvecs, &shares), want);
    let unaggregated = verify_contribution_shares(
        &ids[me],
        &vvecs,
        &shares,
        false,
        BlsSchemeKind::Basic,
        &mut UnwrapErr(SysRng),
    );
    assert_eq!(unaggregated, want);
}

#[test]
fn encrypted_shares_decrypt_for_their_recipient_only() {
    let mut rng = SeededRng::new(3);
    let operators: Vec<BlsSkValue> = (0..3).map(|_| make_new_key(&mut rng)).collect();
    let pks: Vec<BlsPkValue> = operators.iter().map(secret_to_public).collect();
    let shares: Vec<BlsSkValue> = (0..3).map(|_| make_new_key(&mut rng)).collect();
    let seed = IesSeed {
        ephemeral_sk: make_new_key(&mut rng),
        iv_seed: [5; IV_SEED_LEN],
    };
    for flag in [BlsSchemeKind::Basic, BlsSchemeKind::Legacy] {
        let recipients: Vec<(&BlsPkValue, &BlsSkValue)> = pks.iter().zip(&shares).collect();
        let (blobs, ok) = encrypt_shares(&seed, &recipients, flag);
        assert!(ok);
        for (i, sk) in operators.iter().enumerate() {
            assert_eq!(decrypt_share(&blobs, i, sk, flag).as_ref(), Some(&shares[i]));
        }
        let wrong = decrypt_share(&blobs, 0, &operators[1], flag);
        assert_ne!(wrong.as_ref(), Some(&shares[0]));
    }
}

#[test]
fn an_invalid_recipient_key_stops_the_encryption() {
    let mut rng = SeededRng::new(4);
    let pks = [
        secret_to_public(&make_new_key(&mut rng)),
        BlsPkValue::invalid(),
        secret_to_public(&make_new_key(&mut rng)),
    ];
    assert!(can_encrypt_to(&pks[0]));
    assert!(!can_encrypt_to(&pks[1]));
    let share = make_new_key(&mut rng);
    let recipients: Vec<(&BlsPkValue, &BlsSkValue)> = pks.iter().map(|pk| (pk, &share)).collect();
    let seed = IesSeed {
        ephemeral_sk: make_new_key(&mut rng),
        iv_seed: [0; IV_SEED_LEN],
    };
    let (blobs, ok) = encrypt_shares(&seed, &recipients, BlsSchemeKind::Basic);
    assert!(!ok);
    assert!(!blobs.blobs[0].is_empty());
    assert!(blobs.blobs[1].is_empty() && blobs.blobs[2].is_empty());
}

#[test]
fn a_member_id_is_the_protx_hash_bytes() {
    let pro_tx = ProTxHash::from([9; 32]);
    assert_eq!(member_id(&pro_tx), [9; 32]);
}
