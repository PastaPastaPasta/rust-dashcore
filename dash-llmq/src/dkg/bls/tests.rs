//! The BLS adapters against dash-pkc's own DKG operations.

use getrandom::SysRng;
use rand_core::UnwrapErr;

use super::*;
use crate::dkg::rng::SeededRng;
use crate::dkg::session::{DkgBlsRunner, SerialRunner};

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

fn slices(vvecs: &[Vec<BlsPkValue>]) -> Vec<&[BlsPkValue]> {
    vvecs.iter().map(Vec::as_slice).collect()
}

/// Each dealer's verification vector and its shares, one per id.
type Dealers = (Vec<Vec<BlsPkValue>>, Vec<Vec<BlsSkValue>>);

/// Core's test and bench ids: `WriteLE64(i + 1)` for `i` in `0..n`.
fn le64_ids(n: u64) -> Vec<[u8; 32]> {
    (1..=n)
        .map(|i| {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&i.to_le_bytes());
            id
        })
        .collect()
}

/// `dealers` dealers' verification vectors and shares for `ids` at `threshold`, their
/// coefficients drawn from `SeededRng::new(seed)`.
fn dealt(ids: &[[u8; 32]], dealers: usize, threshold: usize, seed: u64) -> Dealers {
    let mut rng = SeededRng::new(seed);
    (0..dealers)
        .map(|_| {
            let coefficients: Vec<BlsSkValue> =
                (0..threshold).map(|_| make_new_key(&mut rng)).collect();
            generate_contributions(&coefficients, ids).expect("t >= 2 succeeds")
        })
        .unzip()
}

/// `bls_tests.cpp:723-736`: four ids (`WriteLE64(i + 1)`), and three dealers' verification
/// vectors and shares at threshold 2.
fn core_dealers() -> (Vec<[u8; 32]>, Dealers) {
    let ids = le64_ids(4);
    let dealers = dealt(&ids, 3, 2, 0xdc);
    (ids, dealers)
}

/// A seeded generator counting its draws.
struct CountingRng {
    inner: SeededRng,
    draws: usize,
}

impl rand_core::TryRng for CountingRng {
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        self.draws += 1;
        self.inner.try_next_u32()
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        self.draws += 1;
        self.inner.try_next_u64()
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.draws += 1;
        self.inner.try_fill_bytes(dst)
    }
}

impl rand_core::TryCryptoRng for CountingRng {}

/// `VerifyContributionShares(receiver, vvecs, shares, aggregated)` through
/// [`verify_contribution_shares`] and through [`SerialRunner`]: the verdicts of each, and the
/// draws each made.
fn verify_both_ways(
    receiver: &[u8; 32],
    vvecs: &[Vec<BlsPkValue>],
    shares: &[&BlsSkValue],
    aggregated: bool,
    flag: BlsSchemeKind,
) -> [(Vec<bool>, usize); 2] {
    let vvecs = slices(vvecs);
    let mut direct = CountingRng {
        inner: SeededRng::new(0xdd),
        draws: 0,
    };
    let by_wrapper =
        verify_contribution_shares(receiver, &vvecs, shares, aggregated, flag, &mut direct);
    let mut runner = CountingRng {
        inner: SeededRng::new(0xdd),
        draws: 0,
    };
    let by_runner = SerialRunner.verify_contribution_shares(
        receiver,
        &vvecs,
        shares,
        aggregated,
        flag,
        &mut runner,
    );
    [(by_wrapper.verdicts, direct.draws), (by_runner.verdicts, runner.draws)]
}

/// v24.0.0 `src/test/bls_tests.cpp:716-781` under `flag`, which follows v24.0.0 (#8016): its
/// assertions on `VerifyContributionShares`, `FindVerificationVectorsWithSharedCoefficients` and
/// `BuildQuorumVerificationVector`, with its dealers, ids, threshold, receiver and copies.
fn duplicate_coefficient(flag: BlsSchemeKind) {
    fn some(vvecs: &[Vec<BlsPkValue>]) -> Vec<Option<&[BlsPkValue]>> {
        vvecs.iter().map(|v| Some(v.as_slice())).collect()
    }
    let (ids, (mut vvecs, sk_shares)) = core_dealers();
    let receiver = &ids[0];
    let mut recv_shares: Vec<&BlsSkValue> = sk_shares.iter().map(|s| &s[0]).collect();
    // Each call also counts the weights it drew: one per contribution when aggregated and the
    // vectors pass `VerifyVerificationVectors`, none otherwise.
    let verify = |vvecs: &[Vec<BlsPkValue>],
                  shares: &[&BlsSkValue],
                  aggregated: bool,
                  want: [bool; 3],
                  weights: usize| {
        for (verdicts, draws) in verify_both_ways(receiver, vvecs, shares, aggregated, flag) {
            assert_eq!(verdicts, want, "{flag:?}");
            assert_eq!(draws, weights, "{flag:?}");
        }
    };
    let shared = |vvecs: &[Option<&[BlsPkValue]>]| vvecs_with_shared_coefficients(vvecs, flag);

    verify(&vvecs, &recv_shares, true, [true, true, true], 3);
    verify(&vvecs, &recv_shares, false, [true, true, true], 0);

    vvecs[0][0] = vvecs[1][0].clone();
    verify(&vvecs, &recv_shares, true, [false, true, true], 0);
    assert_eq!(shared(&some(&vvecs)), BTreeSet::from([0, 1]), "{flag:?}");

    vvecs[0] = vvecs[1].clone();
    recv_shares[0] = &sk_shares[1][0];
    verify(&vvecs, &recv_shares, true, [true, true, true], 0);
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

/// v24.0.0 `src/bench/bls_dkg.cpp:204` and `:208`, `BENCH_VerifyContributionShares` at 100
/// dealers, 5 invalid shares, `simple` and `aggregated`, under the basic scheme
/// (`bench_bitcoin.cpp:69`): threshold 51 (`:76`), the receiver's shares (`:40-46`) with five
/// replaced by new keys (`:93-110`), and each verdict `false` exactly for a replaced share
/// (`:48-59`). The replaced shares are the first and another in the first batch of 8
/// (`bls/bls_worker.h:105`), one opening a batch, one in a middle batch and one in the short last
/// batch. Aggregated, one weight is drawn per contribution; unaggregated, none.
#[test]
fn the_100_dealer_bench_shape_fails_exactly_the_replaced_shares() {
    const DEALERS: usize = 100;
    const REPLACED: [usize; 5] = [0, 5, 8, 50, 99];
    let ids = le64_ids(DEALERS as u64);
    let (vvecs, sk_shares) = dealt(&ids, DEALERS, DEALERS / 2 + 1, 0xbe);
    let receiver = 0;
    let mut rng = SeededRng::new(0xbf);
    let received: Vec<BlsSkValue> = sk_shares
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if REPLACED.contains(&i) {
                make_new_key(&mut rng)
            } else {
                s[receiver].clone()
            }
        })
        .collect();
    let refs: Vec<&BlsSkValue> = received.iter().collect();
    let want: Vec<bool> = (0..DEALERS).map(|i| !REPLACED.contains(&i)).collect();
    for aggregated in [true, false] {
        let weights = usize::from(aggregated) * DEALERS;
        for (verdicts, draws) in
            verify_both_ways(&ids[receiver], &vvecs, &refs, aggregated, BlsSchemeKind::Basic)
        {
            assert_eq!(verdicts, want, "aggregated {aggregated}");
            assert_eq!(draws, weights, "aggregated {aggregated}");
        }
    }
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
