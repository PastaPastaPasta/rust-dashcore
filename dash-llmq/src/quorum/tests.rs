use super::*;
use crate::dkg::bls::{BlsPkValue, BlsSkValue};
use crate::dkg::bls::{BlsSchemeKind, secret_to_public};
use crate::dkg::wire::QuorumHash;
use dash_evo::types::ProTxHash;
use dash_params::LlmqType;
use dash_types::Numeric;

fn sk(n: u8) -> BlsSkValue {
    let mut bytes = [0; 32];
    bytes[31] = n;
    BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).expect("canonical scalar")
}

#[test]
fn vvec_codec_round_trip_uses_basic_encoding() {
    let keys = vec![secret_to_public(&sk(1)), secret_to_public(&sk(2)), BlsPkValue::invalid()];
    let encoded = encode_vvec(&keys);
    assert_eq!(encoded[0], 3);
    assert_eq!(decode_vvec(&encoded).expect("decodes"), keys);
    assert!(decode_vvec(&encoded[..encoded.len() - 1]).is_err());
    let mut invalid = encoded.clone();
    invalid[1..49].fill(0);
    let decoded = decode_vvec(&invalid).expect("invalid element is retained as default");
    assert!(!decoded[0].is_valid());
}

#[test]
fn secret_key_codec_round_trip() {
    let value = sk(7);
    assert_eq!(decode_sk(&encode_sk(&value)), Some(value));
    assert!(decode_sk(&[1; 31]).is_none());
}

#[test]
fn quorum_db_key_is_sha256d_of_core_sequence() {
    let hash = QuorumHash::from([2; 32]);
    let members = [ProTxHash::from([3; 32]), ProTxHash::from([4; 32])];
    let got = quorum_db_key(LlmqType::LlmqTest, &hash, &members);
    let mut input = vec![LlmqType::LlmqTest.to_base()];
    input.extend_from_slice(hash.as_bytes());
    input.extend_from_slice(members[0].as_bytes());
    input.extend_from_slice(members[1].as_bytes());
    assert_eq!(got.0, dash_consensus::hash::sha256d(&input));
    assert_eq!(
        got.0.as_slice(),
        unhex("6516806c2cca823d6e5bf474e0984ae112bcf797a87b7fa657e9b6deeacc599a")
    );
    assert_ne!(got, quorum_db_key(LlmqType::LlmqTest, &hash, &[members[1], members[0]]));
    assert_ne!(got, quorum_db_key(LlmqType::LlmqTestDip0024, &hash, &members));

    let hash_bytes: [u8; 32] = (0..32).map(|i| i as u8).collect::<Vec<_>>().try_into().unwrap();
    let member_bytes: [u8; 32] =
        (0x40..0x60).map(|i| i as u8).collect::<Vec<_>>().try_into().unwrap();
    let hash = QuorumHash::from(hash_bytes);
    let member = ProTxHash::from(member_bytes);
    assert_eq!(
        quorum_db_key(LlmqType::from_base(103), &hash, &[member]).0,
        <[u8; 32]>::try_from(unhex(
            "cc412c05cbfba152216987580d171e5d073d7b3975603ff4855954f98a06aa93",
        ))
        .unwrap()
    );
}

#[test]
fn unchecked_store_requires_a_vvec() {
    assert!(
        from_store(StoredContributions {
            vvec: Ok(None),
            sk_share: Some(sk(1))
        })
        .unwrap()
        .is_none()
    );
    let stored = StoredContributions {
        vvec: Ok(Some(vec![secret_to_public(&sk(1))])),
        sk_share: Some(sk(2)),
    };
    let keys = from_store(stored).unwrap().expect("vvec present");
    assert!(keys.has_vvec());
    assert_eq!(keys.sk_share(), Some(&sk(2)));
    let roundtrip = to_store(&keys);
    assert_eq!(roundtrip.vvec.unwrap().expect("vvec").len(), 1);
}

use std::cell::Cell;
use std::sync::Arc;

use dash_consensus::primitives::{BlockHash, write_compact_size};
use dash_evo::llmq::LlmqCommitment;
use dash_params::{ChainParamsBuilder, LlmqParams, Network, Release};
use dash_types::codec::BaseCodec;

use crate::dkg::bls::{
    build_pub_key_share, build_quorum_verification_vector, generate_contributions, member_id,
};
use crate::dkg::wire::vvec_hash;

const SCHEMES: [BlsSchemeKind; 2] = [BlsSchemeKind::Basic, BlsSchemeKind::Legacy];

struct Fixture {
    params: LlmqParams,
    commitment: LlmqCommitment,
    members: Vec<ProTxHash>,
    operator_keys: Vec<BlsPkValue>,
    vvec: Vec<BlsPkValue>,
    shares: Vec<BlsSkValue>,
}

impl Fixture {
    fn new(valid: &[bool], scheme: BlsSchemeKind) -> Self {
        let members: Vec<_> = (1..=valid.len())
            .map(|i| {
                let mut bytes = [0; 32];
                bytes[31] = i as u8;
                ProTxHash::from(bytes)
            })
            .collect();
        let ids: Vec<_> = members.iter().map(member_id).collect();
        let (vvec, shares) =
            generate_contributions(&[sk(3), sk(5)], &ids).expect("polynomial shares");
        let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        let params = LlmqParams {
            size: valid.len() as u16,
            ..*setup.consensus.llmq(LlmqType::LlmqTest).unwrap()
        };
        let commitment = commitment(&vvec, valid, scheme);
        let operator_keys =
            members.iter().enumerate().map(|(i, _)| secret_to_public(&sk(70 + i as u8))).collect();
        Self {
            params,
            commitment,
            members,
            operator_keys,
            vvec,
            shares,
        }
    }

    fn view(&self) -> QuorumView<'_> {
        QuorumView {
            llmq: &self.params,
            commitment: &self.commitment,
            members: &self.members,
            operator_keys: &self.operator_keys,
        }
    }
}

fn commitment(vvec: &[BlsPkValue], valid: &[bool], scheme: BlsSchemeKind) -> LlmqCommitment {
    let version = if scheme == BlsSchemeKind::Legacy {
        2u16
    } else {
        4u16
    };
    let mut bytes = version.to_le_bytes().to_vec();
    bytes.push(LlmqType::LlmqTest.to_base());
    bytes.extend_from_slice(&[2; 32]);
    bytes.extend_from_slice(&2i16.to_le_bytes());
    for _ in 0..2 {
        write_compact_size(valid.len() as u64, &mut bytes);
        let mut bits = vec![0; valid.len().div_ceil(8)];
        for (i, set) in valid.iter().enumerate() {
            if *set {
                bits[i / 8] |= 1 << (i % 8);
            }
        }
        bytes.extend_from_slice(&bits);
    }
    bytes.extend_from_slice(&vvec[0].to_bytes(scheme));
    bytes.extend_from_slice(&vvec_hash(vvec, scheme));
    bytes.extend_from_slice(&[0; 192]);
    LlmqCommitment::decode(&mut bytes.as_slice()).expect("commitment bytes")
}

#[test]
fn vector_hash_checks_use_the_injected_scheme_and_keep_previous_state_on_failure() {
    for scheme in SCHEMES {
        let f = Fixture::new(&[true; 3], scheme);
        let q = f.view();
        let mut keys = QuorumKeys::default();
        assert!(!keys.has_vvec());
        assert!(!keys.pub_key_share(&q, 0).is_valid());
        let other = if scheme == BlsSchemeKind::Legacy {
            BlsSchemeKind::Basic
        } else {
            BlsSchemeKind::Legacy
        };
        assert!(!keys.set_verification_vector(&q, f.vvec.clone(), other));
        assert!(!keys.has_vvec());
        assert!(keys.set_verification_vector(&q, f.vvec.clone(), scheme));
        let before = keys.pub_key_share(&q, 0);
        assert!(!keys.set_verification_vector(&q, vec![secret_to_public(&sk(99))], scheme));
        assert_eq!(keys.pub_key_share(&q, 0), before);
    }
}

#[test]
fn membership_and_public_share_checks_follow_the_commitment_bits() {
    for scheme in SCHEMES {
        let f = Fixture::new(&[true, false, true], scheme);
        let q = f.view();
        let absent = ProTxHash::from([9; 32]);
        assert!(!q.is_member(&absent));
        assert!(!q.is_valid_member(&absent));
        assert_eq!(q.member_index(&absent), None);
        let mut keys = QuorumKeys::default();
        assert!(keys.set_verification_vector(&q, f.vvec.clone(), scheme));
        for i in 0..3 {
            assert!(q.is_member(&f.members[i]));
            assert_eq!(q.member_index(&f.members[i]), Some(i));
            assert_eq!(q.is_valid_member(&f.members[i]), i != 1);
            let share = keys.pub_key_share(&q, i);
            if i == 1 {
                assert!(!share.is_valid());
            } else {
                assert!(share.core_eq(&secret_to_public(&f.shares[i])));
                assert_eq!(share, build_pub_key_share(&f.vvec, &member_id(&f.members[i])));
                assert_eq!(keys.pub_key_share(&q, i), share);
            }
        }
        assert!(!keys.pub_key_share(&q, 3).is_valid());
        assert!(!keys.pub_key_share(&q, usize::MAX).is_valid());
    }
}

#[test]
fn secret_shares_are_accepted_only_for_the_matching_valid_member() {
    for scheme in SCHEMES {
        let f = Fixture::new(&[true, true, false], scheme);
        let q = f.view();
        let mut keys = QuorumKeys::default();
        assert!(!keys.set_secret_key_share(&q, f.shares[0].clone(), &f.members[0]));
        assert!(keys.set_verification_vector(&q, f.vvec.clone(), scheme));
        for i in 0..3 {
            for j in 0..3 {
                assert_eq!(
                    keys.set_secret_key_share(&q, f.shares[i].clone(), &f.members[j]),
                    i == j && i != 2
                );
            }
        }
        assert!(!keys.set_secret_key_share(&q, BlsSkValue::invalid(), &f.members[2]));
        let before = keys.sk_share().cloned();
        assert!(!keys.set_secret_key_share(&q, BlsSkValue::invalid(), &f.members[0]));
        assert!(!keys.set_secret_key_share(&q, f.shares[0].clone(), &ProTxHash::from([0; 32])));
        assert!(!keys.set_secret_key_share(&q, f.shares[0].clone(), &ProTxHash::from([99; 32])));
        assert_eq!(keys.sk_share().cloned(), before);
    }
}

#[test]
fn warming_stops_before_the_next_member_and_leaves_the_same_shares() {
    let f = Fixture::new(&[true, false, true], BlsSchemeKind::Basic);
    let q = f.view();
    let mut keys = QuorumKeys::default();
    keys.set_vvec_unchecked(Arc::new(f.vvec.clone()));
    let calls = Cell::new(0);
    warm_members(&mut keys, &q, &|| {
        calls.set(calls.get() + 1);
        calls.get() == 2
    });
    assert_eq!(calls.get(), 2);
    warm_members(&mut keys, &q, &|| false);
    for i in [0, 2] {
        assert_eq!(keys.pub_key_share(&q, i), secret_to_public(&f.shares[i]));
    }
}

#[test]
fn contributions_build_even_when_the_secret_share_is_unavailable_or_wrong() {
    for scheme in SCHEMES {
        let f = Fixture::new(&[true; 3], scheme);
        let q = f.view();
        let mut keys = QuorumKeys::default();
        let inputs = [Arc::new(f.vvec.clone())];
        assert!(build_contributions(
            &mut keys,
            &q,
            &inputs,
            &[f.shares[0].clone()],
            Some(&f.members[0]),
            scheme
        ));
        assert_eq!(keys.sk_share(), Some(&f.shares[0]));
        let mut observer = QuorumKeys::default();
        assert!(build_contributions(&mut observer, &q, &inputs, &[], None, scheme));
        assert!(observer.has_vvec());
        assert!(observer.sk_share().is_none());
        assert!(build_contributions(&mut observer, &q, &inputs, &[], Some(&f.members[0]), scheme));
        assert!(observer.sk_share().is_none());
        assert!(build_contributions(
            &mut observer,
            &q,
            &inputs,
            &[sk(99)],
            Some(&f.members[0]),
            scheme
        ));
        assert!(observer.sk_share().is_none());
        assert!(!build_contributions(&mut observer, &q, &[], &[], None, scheme));
        assert!(!observer.has_vvec());
        assert!(!observer.pub_key_share(&q, 0).is_valid());
    }
}

#[test]
fn contribution_build_uses_the_pointer_setter_without_a_commitment_hash_check() {
    for scheme in SCHEMES {
        let f = Fixture::new(&[true; 3], scheme);
        let q = f.view();
        let vvec = generate_contributions(&[sk(8), sk(13)], &[member_id(&f.members[0])]).unwrap().0;
        let mut keys = QuorumKeys::default();
        assert!(!keys.set_verification_vector(&q, vvec.clone(), scheme));
        assert!(build_contributions(&mut keys, &q, &[Arc::new(vvec.clone())], &[], None, scheme));
        assert_eq!(
            keys.pub_key_share(&q, 0),
            build_pub_key_share(&vvec, &member_id(&f.members[0]))
        );
    }
}

#[test]
fn duplicate_secret_contributions_are_aggregated_without_rejection() {
    for scheme in SCHEMES {
        let mut f = Fixture::new(&[true; 3], scheme);
        // At id 1, the distinct polynomials 3 + 5x and 6 + 2x both evaluate to 8.
        let (a, a_shares) =
            generate_contributions(&[sk(3), sk(5)], &[member_id(&f.members[0])]).unwrap();
        let (b, b_shares) =
            generate_contributions(&[sk(6), sk(2)], &[member_id(&f.members[0])]).unwrap();
        assert_eq!(a_shares, b_shares);
        let sum = build_quorum_verification_vector(&[&a, &b], scheme).unwrap();
        f.commitment = commitment(&sum, &[true; 3], scheme);
        let mut keys = QuorumKeys::default();
        assert!(build_contributions(
            &mut keys,
            &f.view(),
            &[Arc::new(a), Arc::new(b)],
            &[a_shares[0].clone(), b_shares[0].clone()],
            Some(&f.members[0]),
            scheme
        ));
        assert_eq!(keys.sk_share(), Some(&sk(16)));
    }
}

#[test]
fn quorum_info_includes_only_requested_valid_fields() {
    let scheme = BlsSchemeKind::Basic;
    let mut f = Fixture::new(&[true, false, true], scheme);
    let block = BlockHash::from([17; 32]);
    let mut keys = QuorumKeys::default();
    keys.set_vvec_unchecked(Arc::new(f.vvec.clone()));
    assert!(keys.set_secret_key_share(&f.view(), f.shares[0].clone(), &f.members[0]));
    let info = quorum_info(&f.view(), &mut keys, 120, &block, None, true, true);
    assert_eq!(info.height, 120);
    assert_eq!(info.type_name, "llmq_test");
    assert_eq!(info.quorum_hash, f.commitment.quorum_hash());
    assert_eq!(info.quorum_index, 2);
    assert_eq!(info.mined_block, block);
    assert_eq!(info.quorum_public_key, f.vvec[0]);
    assert_eq!(info.secret_key_share, Some(f.shares[0].clone()));
    assert_eq!(info.previous_consecutive_dkg_failures, None);
    let members = info.members.unwrap();
    for (i, member) in members.iter().enumerate() {
        assert_eq!(member.pro_tx_hash, f.members[i]);
        assert_eq!(member.pub_key_operator, f.operator_keys[i]);
        assert_eq!(member.valid, i != 1);
        assert_eq!(member.pub_key_share, (i != 1).then(|| secret_to_public(&f.shares[i])));
    }
    let info = quorum_info(&f.view(), &mut keys, 120, &block, None, false, false);
    assert!(info.members.is_none());
    assert!(info.secret_key_share.is_none());
    f.params.use_rotation = true;
    for (prev, expected) in [(None, 0), (Some(48), 2), (Some(96), 0)] {
        let info = quorum_info(&f.view(), &mut keys, 120, &block, prev, false, true);
        assert_eq!(info.previous_consecutive_dkg_failures, Some(expected));
    }
}

#[test]
fn cached_shares_survive_checked_and_contribution_vector_replacements() {
    for scheme in SCHEMES {
        let mut f = Fixture::new(&[true; 3], scheme);
        let mut keys = QuorumKeys::default();
        keys.set_vvec_unchecked(Arc::new(f.vvec.clone()));
        let cached = keys.pub_key_share(&f.view(), 0);

        let second = generate_contributions(
            &[sk(17), sk(19)],
            &f.members.iter().map(member_id).collect::<Vec<_>>(),
        )
        .unwrap()
        .0;
        f.commitment = commitment(&second, &[true; 3], scheme);
        assert!(keys.set_verification_vector(&f.view(), second.clone(), scheme));
        assert_eq!(keys.pub_key_share(&f.view(), 0), cached);
        assert_eq!(
            keys.pub_key_share(&f.view(), 1),
            build_pub_key_share(&second, &member_id(&f.members[1]))
        );

        let third = generate_contributions(
            &[sk(23), sk(29)],
            &f.members.iter().map(member_id).collect::<Vec<_>>(),
        )
        .unwrap()
        .0;
        assert!(build_contributions(
            &mut keys,
            &f.view(),
            &[Arc::new(third.clone())],
            &[],
            None,
            scheme
        ));
        assert_eq!(keys.pub_key_share(&f.view(), 0), cached);
        assert_eq!(
            keys.pub_key_share(&f.view(), 2),
            build_pub_key_share(&third, &member_id(&f.members[2]))
        );
    }
}

#[test]
fn single_member_info_uses_the_operator_key_without_a_verification_vector() {
    let f = Fixture::new(&[true], BlsSchemeKind::Basic);
    let mut keys = QuorumKeys::default();
    let info = quorum_info(&f.view(), &mut keys, 24, &BlockHash::from([1; 32]), None, true, true);
    assert_eq!(info.members.unwrap()[0].pub_key_share, Some(f.operator_keys[0].clone()));
    assert!(info.secret_key_share.is_none());
    assert!(!keys.set_secret_key_share(&f.view(), sk(70), &f.members[0]));
    let f = Fixture::new(&[false], BlsSchemeKind::Basic);
    let info = quorum_info(&f.view(), &mut keys, 24, &BlockHash::from([1; 32]), None, true, true);
    assert!(info.members.unwrap()[0].pub_key_share.is_none());
}

#[test]
fn missing_or_invalid_vector_shares_are_omitted_from_info() {
    let f = Fixture::new(&[true; 3], BlsSchemeKind::Basic);
    let mut keys = QuorumKeys::default();
    for vvec in [None, Some(Arc::new(vec![])), Some(Arc::new(vec![BlsPkValue::invalid(); 2]))] {
        if let Some(vvec) = vvec {
            keys.set_vvec_unchecked(vvec);
        }
        let info =
            quorum_info(&f.view(), &mut keys, 24, &BlockHash::from([1; 32]), None, true, true);
        assert!(info.members.unwrap().iter().all(|m| m.pub_key_share.is_none()));
        assert!(info.secret_key_share.is_none());
    }
}

#[test]
fn a_cached_share_is_kept_when_the_vector_is_replaced() {
    let f = Fixture::new(&[true; 3], BlsSchemeKind::Basic);
    let q = f.view();
    let mut keys = QuorumKeys::default();
    keys.set_vvec_unchecked(Arc::new(f.vvec.clone()));
    let before = keys.pub_key_share(&q, 0);
    let vector = vec![secret_to_public(&sk(20)), secret_to_public(&sk(30))];
    keys.set_vvec_unchecked(Arc::new(vector.clone()));
    assert_eq!(keys.pub_key_share(&q, 0), before);
    assert_ne!(before, build_pub_key_share(&vector, &member_id(&f.members[0])));
    assert_eq!(keys.pub_key_share(&q, 1), build_pub_key_share(&vector, &member_id(&f.members[1])));
}

#[test]
fn stored_vector_key_that_decodes_under_neither_scheme_is_a_read_error() {
    let mut legacy_raw = [0; 48];
    legacy_raw[0] = 0x80;
    let neither = BlsPkValue::from_canonical(&legacy_raw, BlsSchemeKind::Legacy)
        .unwrap()
        .to_bytes(BlsSchemeKind::Basic);
    let mut infinity = [0; 48];
    infinity[0] = 0xc0;
    for raw in [neither, [0xff; 48], infinity] {
        assert!(BlsPkValue::from_canonical(&raw, BlsSchemeKind::Basic).is_none());
        assert!(BlsPkValue::from_canonical(&raw, BlsSchemeKind::Legacy).is_none());
        let mut bytes = vec![1];
        bytes.extend_from_slice(&raw);
        assert!(decode_vvec(&bytes).is_err());
        assert_eq!(
            from_store(StoredContributions {
                vvec: decode_vvec(&bytes).map(Some),
                sk_share: None,
            })
            .unwrap_err(),
            VvecReadError::UndecodableElement {
                index: 0
            }
        );
    }

    let valid = secret_to_public(&sk(11)).to_bytes(BlsSchemeKind::Basic);
    let mut bytes = vec![2];
    bytes.extend_from_slice(&valid);
    bytes.extend_from_slice(&neither);
    assert_eq!(
        decode_vvec(&bytes),
        Err(VvecReadError::UndecodableElement {
            index: 1
        })
    );
}

#[test]
fn stored_vector_key_that_decodes_only_under_legacy_becomes_default() {
    let mut legacy_raw = [0; 48];
    legacy_raw[47] = 4;
    let key = BlsPkValue::from_canonical(&legacy_raw, BlsSchemeKind::Legacy).unwrap();
    let basic = key.to_bytes(BlsSchemeKind::Basic);
    assert!(BlsPkValue::from_canonical(&basic, BlsSchemeKind::Basic).is_none());
    assert!(BlsPkValue::from_canonical(&basic, BlsSchemeKind::Legacy).is_some());

    let mut bytes = vec![1];
    bytes.extend_from_slice(&basic);
    assert_eq!(decode_vvec(&bytes), Ok(vec![BlsPkValue::invalid()]));
}

#[test]
fn stored_vector_all_zero_key_becomes_default() {
    let mut bytes = vec![1];
    bytes.extend_from_slice(&[0; 48]);
    assert_eq!(decode_vvec(&bytes), Ok(vec![BlsPkValue::invalid()]));
}

#[test]
fn vector_compact_size_and_truncation_follow_core_reads() {
    for count in [0usize, 1, 252, 253] {
        let vector = vec![secret_to_public(&sk(1)); count];
        let bytes = encode_vvec(&vector);
        assert_eq!(decode_vvec(&bytes), Ok(vector.clone()));
        for end in 0..bytes.len() {
            assert!(decode_vvec(&bytes[..end]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(99);
        assert_eq!(decode_vvec(&trailing), Ok(vector));
    }
    let key = encode_vvec(&[secret_to_public(&sk(1))]);
    for prefix in
        [&[253, 1, 0][..], &[254, 1, 0, 0, 0], &[255, 1, 0, 0, 0, 0, 0, 0, 0], &[254, 1, 0, 0, 2]]
    {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&key[1..]);
        assert_eq!(decode_vvec(&bytes), Err(VvecReadError::MalformedCount));
    }
}

#[test]
fn load_or_build_contributions_preserves_the_three_read_outcomes() {
    use std::cell::Cell;

    let builds = Cell::new(0);
    let writes = Cell::new(0);
    let err = load_or_build_contributions(
        || Err::<Option<QuorumKeys>, _>("unreadable"),
        || {
            builds.set(builds.get() + 1);
            Some(QuorumKeys::default())
        },
        |_| {
            writes.set(writes.get() + 1);
            Ok::<(), &str>(())
        },
    )
    .unwrap_err();
    assert_eq!(err, "unreadable");
    assert_eq!((builds.get(), writes.get()), (0, 0));

    let builds = Cell::new(0);
    let writes = Cell::new(0);
    let found = load_or_build_contributions(
        || Ok::<Option<QuorumKeys>, &str>(Some(QuorumKeys::default())),
        || {
            builds.set(builds.get() + 1);
            Some(QuorumKeys::default())
        },
        |_| {
            writes.set(writes.get() + 1);
            Ok(())
        },
    )
    .unwrap();
    assert!(found.is_some());
    assert_eq!((builds.get(), writes.get()), (0, 0));

    let builds = Cell::new(0);
    let writes = Cell::new(0);
    let built = load_or_build_contributions(
        || Ok::<Option<QuorumKeys>, &str>(None),
        || {
            builds.set(builds.get() + 1);
            Some(QuorumKeys::default())
        },
        |_| {
            writes.set(writes.get() + 1);
            Ok(())
        },
    )
    .unwrap();
    assert!(built.is_some());
    assert_eq!((builds.get(), writes.get()), (1, 1));

    let builds = Cell::new(0);
    let writes = Cell::new(0);
    let failed = load_or_build_contributions(
        || Ok::<Option<QuorumKeys>, &str>(None),
        || {
            builds.set(builds.get() + 1);
            None
        },
        |_| {
            writes.set(writes.get() + 1);
            Ok(())
        },
    )
    .unwrap();
    assert!(failed.is_none());
    assert_eq!((builds.get(), writes.get()), (1, 0));

    let builds = Cell::new(0);
    let write_failed = load_or_build_contributions(
        || Ok::<Option<QuorumKeys>, &str>(None),
        || {
            builds.set(builds.get() + 1);
            Some(QuorumKeys::default())
        },
        |_| Err("write failed"),
    )
    .unwrap_err();
    assert_eq!(write_failed, "write failed");
    assert_eq!(builds.get(), 1);
}

#[test]
fn secret_rows_can_read_default_keys_and_ignore_trailing_bytes() {
    assert_eq!(decode_sk(&[0; 32]), Some(BlsSkValue::invalid()));
    assert_eq!(encode_sk(&BlsSkValue::invalid()), [0; 32]);
    assert!(decode_sk(&[255; 32]).is_none());
    let mut bytes = encode_sk(&sk(7)).to_vec();
    bytes.push(99);
    assert_eq!(decode_sk(&bytes), Some(sk(7)));
    let keys = from_store(StoredContributions {
        vvec: Ok(Some(vec![])),
        sk_share: decode_sk(&[0; 32]),
    })
    .unwrap();
    let keys = keys.expect("vvec present");
    assert!(keys.has_vvec());
    assert!(keys.sk_share().is_none());
    assert!(to_store(&keys).sk_share.is_none());
    assert!(
        from_store(StoredContributions {
            vvec: Ok(Some(vec![])),
            sk_share: None
        })
        .unwrap()
        .unwrap()
        .sk_share()
        .is_none()
    );
}

#[test]
fn zero_secret_row_is_omitted_from_quorum_info_even_when_requested() {
    let f = Fixture::new(&[true; 3], BlsSchemeKind::Basic);
    let mut keys = from_store(StoredContributions {
        vvec: Ok(Some(f.vvec.clone())),
        sk_share: decode_sk(&[0; 32]),
    })
    .unwrap()
    .unwrap();
    let info = quorum_info(&f.view(), &mut keys, 24, &BlockHash::from([1; 32]), None, false, true);
    assert!(info.secret_key_share.is_none());
}

#[test]
fn quorum_info_omits_an_invalid_secret_share_when_requested() {
    let f = Fixture::new(&[true; 3], BlsSchemeKind::Basic);
    let mut keys = QuorumKeys::default();
    keys.sk_share = Some(BlsSkValue::invalid());
    let info = quorum_info(&f.view(), &mut keys, 24, &BlockHash::from([1; 32]), None, false, true);
    assert!(info.secret_key_share.is_none());
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn three_entry_core_key_vector_is_stored_basic_even_for_legacy_decoded_inputs() {
    let bytes = include_bytes!("../../../test-vectors/lq4/bls_llmq_100.json5");
    assert_eq!(
        dash_consensus::hash::sha256(bytes),
        <[u8; 32]>::try_from(unhex(
            "72bec3e551d22a6b511b87e73bbc8f0215736a91bf9ee7efe79ac514c8c33557"
        ))
        .unwrap()
    );
    let json = std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let corpus: serde_json::Value = serde_json::from_str(&json).unwrap();
    let mut expected = vec![3];
    let mut legacy_keys = Vec::new();
    for contribution in corpus["ietf"]["contribute"].as_array().unwrap() {
        let basic: [u8; 48] = unhex(contribution["vvec"][0].as_str().unwrap()).try_into().unwrap();
        expected.extend_from_slice(&basic);
        // Core: v24.0.0 src/dashbls/src/elements.cpp:178-196 G1Element::SerializeToArray
        let mut legacy = basic;
        legacy[0] = (basic[0] & 0x1f) | ((basic[0] & 0x20) << 2);
        legacy_keys.push(BlsPkValue::from_canonical(&legacy, BlsSchemeKind::Legacy).unwrap());
    }
    assert_eq!(legacy_keys.len(), 3);
    assert_eq!(encode_vvec(&legacy_keys), expected);
    assert_eq!(decode_vvec(&expected).unwrap(), legacy_keys);
}
