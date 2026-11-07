//! The BLS adapters against dash-pkc's own DKG operations.

use super::*;
use crate::dkg::rng::SeededRng;

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
        verify_contribution_shares(&ids[me], &vvecs, &refs, BlsSchemeKind::Basic),
        vec![true; 10]
    );
    // Dealer 9's share is for someone else: the second 8-chunk falls back to single checks.
    shares[9] = dealers[9].1[0].clone();
    let refs: Vec<&BlsSkValue> = shares.iter().collect();
    let mut expected = vec![true; 10];
    expected[9] = false;
    assert_eq!(verify_contribution_shares(&ids[me], &vvecs, &refs, BlsSchemeKind::Basic), expected);
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

/// The legacy key read from 47 zero bytes and `04`: its basic bytes are canonical only in the
/// legacy scheme.
fn legacy_only_key() -> BlsPkValue {
    let mut raw = [0u8; 48];
    raw[47] = 4;
    BlsPkValue::from_canonical(&raw, BlsSchemeKind::Legacy).expect("a legacy key")
}

/// The legacy key read from `80` and 47 zero bytes: its basic bytes, `a0` and 47 zero bytes,
/// decode in neither scheme.
fn neither_scheme_key() -> BlsPkValue {
    let mut raw = [0u8; 48];
    raw[0] = 0x80;
    BlsPkValue::from_canonical(&raw, BlsSchemeKind::Legacy).expect("a legacy key")
}

#[test]
fn a_stored_vector_key_is_read_back_in_the_basic_encoding() {
    let legacy_only = legacy_only_key();
    assert!(legacy_only.is_valid());
    assert_eq!(stored_vvec_key(&legacy_only), BlsPkValue::invalid());
    assert_eq!(stored_vvec_key(&neither_scheme_key()), BlsPkValue::invalid());

    let key = secret_to_public(&make_new_key(&mut SeededRng::new(7)));
    assert_eq!(stored_vvec_key(&key), key);
    assert_eq!(stored_vvec_key(&BlsPkValue::invalid()), BlsPkValue::invalid());
}

#[test]
fn the_store_returns_the_vector_its_basic_encoding_reads_back() {
    use crate::dkg::reference::MemStore;
    use crate::dkg::session::DkgStore;
    use crate::dkg::wire::{LlmqType, QuorumHash};

    let who = ProTxHash::from([1; 32]);
    let base = QuorumHash::from([2; 32]);
    let key = secret_to_public(&make_new_key(&mut SeededRng::new(8)));
    let mut store = MemStore::with_members(vec![who]);
    store.write_vvec(LlmqType::LlmqTest, &base, &who, &[legacy_only_key(), key.clone()]);
    let read = store.verified_contributions(LlmqType::LlmqTest, &base, &[true]).expect("a row");
    assert_eq!(read.vvecs, vec![vec![BlsPkValue::invalid(), key]]);
}

#[test]
fn the_store_returns_a_key_whose_basic_bytes_do_not_decode_as_invalid() {
    use crate::dkg::reference::MemStore;
    use crate::dkg::session::DkgStore;
    use crate::dkg::wire::{LlmqType, QuorumHash};

    let neither = neither_scheme_key();
    let basic = neither.to_bytes(BlsSchemeKind::Basic);
    assert!(BlsPkValue::from_canonical(&basic, BlsSchemeKind::Basic).is_none());
    assert!(BlsPkValue::from_canonical(&basic, BlsSchemeKind::Legacy).is_none());
    assert_ne!(basic, [0; 48]);

    let who = ProTxHash::from([1; 32]);
    let base = QuorumHash::from([2; 32]);
    let key = secret_to_public(&make_new_key(&mut SeededRng::new(9)));
    let mut store = MemStore::with_members(vec![who]);
    store.write_vvec(LlmqType::LlmqTest, &base, &who, &[neither, key.clone()]);
    let read = store.verified_contributions(LlmqType::LlmqTest, &base, &[true]).expect("a row");
    assert_eq!(read.vvecs, vec![vec![BlsPkValue::invalid(), key]]);
}

#[test]
fn sign_is_cores_sign_under_either_scheme() {
    let sk = make_new_key(&mut SeededRng::new(5));
    let hash = [0x5a; 32];
    for legacy in [true, false] {
        let sig = sign(&sk, &hash, legacy);
        assert_eq!(sig, sign_stand_in(&sk, &hash, legacy), "legacy {legacy}");
        let flag = BlsSchemeKind::from_legacy(legacy);
        assert!(verify_insecure(&sig, &secret_to_public(&sk), &hash, flag), "legacy {legacy}");
    }
    assert_ne!(sign(&sk, &hash, true), sign(&sk, &hash, false));
    assert!(!sign(&BlsSkValue::invalid(), &hash, false).is_valid());
}

/// v24.0.0 `src/test/bls_tests.cpp:716-775` under `flag`, which follows v24.0.0 (#8016): its
/// assertions on `FindVerificationVectorsWithSharedCoefficients` and
/// `BuildQuorumVerificationVector`, with its dealers, ids, threshold and copies.
fn duplicate_coefficient(flag: BlsSchemeKind) {
    let threshold = 2;
    let ids: Vec<[u8; 32]> = (1..=4u64)
        .map(|i| {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&i.to_le_bytes());
            id
        })
        .collect();
    let mut rng = SeededRng::new(0xdc);
    let mut vvecs: Vec<Vec<BlsPkValue>> = (0..3)
        .map(|_| {
            let coefficients: Vec<BlsSkValue> =
                (0..threshold).map(|_| make_new_key(&mut rng)).collect();
            generate_contributions(&coefficients, &ids).expect("t = 2 succeeds").0
        })
        .collect();
    fn some(vvecs: &[Vec<BlsPkValue>]) -> Vec<Option<&[BlsPkValue]>> {
        vvecs.iter().map(|v| Some(v.as_slice())).collect()
    }
    fn slices(vvecs: &[Vec<BlsPkValue>]) -> Vec<&[BlsPkValue]> {
        vvecs.iter().map(Vec::as_slice).collect()
    }
    let shared = |vvecs: &[Option<&[BlsPkValue]>]| vvecs_with_shared_coefficients(vvecs, flag);

    vvecs[0][0] = vvecs[1][0].clone();
    assert_eq!(shared(&some(&vvecs)), BTreeSet::from([0, 1]), "{flag:?}");

    vvecs[0] = vvecs[1].clone();
    assert!(build_quorum_verification_vector(&slices(&vvecs), flag).is_none(), "{flag:?}");
    assert_eq!(shared(&some(&vvecs)), BTreeSet::from([0, 1]), "{flag:?}");
    assert!(build_quorum_verification_vector(&slices(&vvecs[2..]), flag).is_some(), "{flag:?}");

    let distinct = [Some(vvecs[1].as_slice()), None, Some(vvecs[2].as_slice())];
    assert!(shared(&distinct).is_empty(), "{flag:?}");
}

#[test]
fn shared_coefficients_as_cores_duplicate_coefficient_test_finds_them() {
    // Run for both schemes, as Core's test case does (:777-781).
    duplicate_coefficient(BlsSchemeKind::Legacy);
    duplicate_coefficient(BlsSchemeKind::Basic);
}

#[test]
fn a_repeat_within_one_vector_is_not_shared_and_invalid_keys_are_one_value() {
    // Not Core's test: the edges of `FindVerificationVectorsWithSharedCoefficients` it implies.
    // Mutants: an owner taken as sharing with itself; invalid keys skipped.
    let mut rng = SeededRng::new(0xdd);
    let keys: Vec<BlsPkValue> = (0..3).map(|_| secret_to_public(&make_new_key(&mut rng))).collect();
    let flag = BlsSchemeKind::Basic;
    let repeat = [keys[0].clone(), keys[0].clone()];
    let other = [keys[1].clone(), keys[2].clone()];
    assert!(vvecs_with_shared_coefficients(&[Some(&repeat), Some(&other)], flag).is_empty());
    let invalid = [BlsPkValue::invalid(), keys[1].clone()];
    let also_invalid = [keys[2].clone(), BlsPkValue::invalid()];
    let first = [keys[0].clone()];
    assert_eq!(
        vvecs_with_shared_coefficients(
            &[Some(&first), Some(&invalid), None, Some(&also_invalid)],
            flag
        ),
        BTreeSet::from([1, 3])
    );
    // A key held by three vectors: the first owns it, and every holder is listed.
    let held = [keys[2].clone()];
    let vvecs = [Some(&held[..]), Some(&other[..]), Some(&held[..])];
    assert_eq!(vvecs_with_shared_coefficients(&vvecs, flag), BTreeSet::from([0, 1, 2]));
}
