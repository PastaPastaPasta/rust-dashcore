use std::{array, collections::BTreeSet, slice};

use dash_consensus::hash::sha256;
use dash_evo::locks::sign_hash;
use dash_params::{ChainParamsBuilder, LlmqType, LlmqTypeExt, Network, Release};
use dash_types::Numeric;
use proptest::prelude::*;
use rand_core::Rng;
use serde_json::Value;

use super::*;
use crate::dkg::bls::{
    BlsPkValue, BlsSchemeKind, generate_contributions, make_new_key, sign, verify_insecure,
};
use crate::dkg::rng::SeededRng;
use crate::dkg::wire::QuorumHash;

const SCHEMES: [BlsSchemeKind; 2] = [BlsSchemeKind::Legacy, BlsSchemeKind::Basic];
const ORDER: [u8; 32] = [
    0x73, 0xed, 0xa7, 0x53, 0x29, 0x9d, 0x7d, 0x48, 0x33, 0x39, 0xd8, 0x08, 0x09, 0xa1, 0xd8, 0x05,
    0x53, 0xbd, 0xa4, 0x02, 0xff, 0xfe, 0x5b, 0xfe, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
];
const LANE_C_SHA256: &str = "a02daa19a3a96a36c4ecea2553ee4f2b20fe22f20206340fd6c29dac0f3f7758";

fn llmq(t: LlmqType) -> LlmqParams {
    [Network::Main, Network::Testnet3, Network::Regtest, Network::Devnet]
        .into_iter()
        .find_map(|network| {
            let builder = ChainParamsBuilder::new(network, Release::V24_0);
            let builder = if network == Network::Devnet {
                builder.devnet_name("recovery")
            } else {
                builder
            };
            builder.build().unwrap().consensus.llmq(t).copied()
        })
        .expect("every table type is configured on a chain")
}

fn scalar(n: u64) -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[24..].copy_from_slice(&n.to_be_bytes());
    bytes
}

fn signature() -> BlsSigValue {
    let mut rng = SeededRng::new(123);
    sign(&make_new_key(&mut rng), &[0x42; 32], false)
}

fn same_group_value(a: &BlsSigValue, b: &BlsSigValue) {
    for scheme in SCHEMES {
        assert_eq!(a.to_bytes(scheme), b.to_bytes(scheme));
    }
    assert_eq!(a.is_valid(), b.is_valid());
    assert_eq!(a, b);
    assert!(a.core_eq(b));
    assert!(b.core_eq(a));
    assert!(BlsSigElem::from_impl(a).core_eq(&BlsSigElem::from_impl(b)));
    assert!(BlsSigElem::from_impl(b).core_eq(&BlsSigElem::from_impl(a)));
}

fn same_value(a: &BlsSigValue, b: &BlsSigValue) {
    same_group_value(a, b);
    let point = BlsSigElem::from_impl(&signature());
    let projc_infinity = point.add(&point.neg());
    assert_eq!(
        BlsSigElem::from_impl(a).core_eq(&projc_infinity),
        BlsSigElem::from_impl(b).core_eq(&projc_infinity)
    );
}

fn transcript_hex(value: &Value) -> Vec<u8> {
    let hex = value.as_str().expect("a hex string");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

fn transcript_rpc(value: &Value) -> [u8; 32] {
    let mut bytes: [u8; 32] = transcript_hex(value).try_into().expect("32 bytes");
    bytes.reverse();
    bytes
}

fn transcript_sig(value: &Value) -> BlsSigValue {
    let bytes: [u8; 96] = transcript_hex(value).try_into().expect("96-byte signature");
    BlsSigValue::from_canonical(&bytes, BlsSchemeKind::Basic).expect("basic signature")
}

fn transcript_pk(value: &Value) -> BlsPkValue {
    let bytes: [u8; 48] = transcript_hex(value).try_into().expect("48-byte public key");
    BlsPkValue::from_canonical(&bytes, BlsSchemeKind::Basic).expect("basic public key")
}

fn transcript_quorum<'a>(data: &'a Value, t: u8, hash: &str) -> &'a Value {
    data["phases"]
        .as_object()
        .expect("phases")
        .values()
        .flat_map(|phase| phase["quorums"].as_array().expect("quorums"))
        .chain(data["signing_quorums"].as_object().expect("signing quorums").values())
        .find(|quorum| {
            quorum["type"].as_u64() == Some(u64::from(t))
                && quorum["quorumHash"].as_str() == Some(hash)
        })
        .expect("recorded quorum")
}

/// Distinct threshold subsets: all of them when there are at most 10, else 10 sampled.
fn transcript_subsets(size: usize, threshold: usize) -> Vec<Vec<usize>> {
    // C(size, k) grows with k up to size / 2, so the walk can stop once it reaches 10.
    let mut wanted = 1usize;
    for i in 0..threshold.min(size - threshold) {
        if wanted >= 10 {
            break;
        }
        wanted = wanted * (size - i) / (i + 1);
    }
    let wanted = wanted.min(10);
    let mut rng = SeededRng::new(0x4c_51_35_31);
    let mut subsets = BTreeSet::new();
    while subsets.len() < wanted {
        let mut indexes: Vec<_> = (0..size).collect();
        for i in (1..indexes.len()).rev() {
            indexes.swap(i, (rng.next_u64() % (i + 1) as u64) as usize);
        }
        indexes.truncate(threshold);
        indexes.sort_unstable();
        subsets.insert(indexes);
    }
    subsets.into_iter().collect()
}

#[test]
fn transcript_subsets_are_distinct_threshold_subsets() {
    let cases = [
        (3, 2, 3),
        (4, 3, 4),
        (5, 3, 10),
        (5, 4, 5),
        (5, 5, 1),
        (10, 6, 10),
        (400, 240, 10),
        (400, 340, 10),
    ];
    for (size, threshold, count) in cases {
        let subsets = transcript_subsets(size, threshold);
        assert_eq!(subsets.len(), count, "size {size} threshold {threshold}");
        assert_eq!(subsets.iter().collect::<BTreeSet<_>>().len(), count);
        for subset in &subsets {
            assert_eq!(subset.len(), threshold);
            assert!(subset.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(subset.iter().all(|index| *index < size));
        }
    }
}

#[test]
fn lane_c_transcript_recovered_signatures_match_core() {
    let bytes = include_bytes!("../../../test-vectors/lane-c/regtest-v24.0.0.json");
    let digest: String = sha256(bytes).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(digest, LANE_C_SHA256, "lane-c fixture hash");
    let data: Value = serde_json::from_slice(bytes).expect("lane-c JSON");
    let requests = data["requests"].as_array().expect("requests");
    assert!(!requests.is_empty());

    for request in requests {
        let shares = request["offline_shares"].as_object().expect("recorded shares");
        assert!(!shares.is_empty(), "request has no recorded shares");
        let recsig = request["recsig"].as_object().expect("recorded recovered signature");
        let request_type = request["type"].as_u64().expect("type") as u8;
        let quorum_hash_text = request["quorumHash"].as_str().expect("quorum hash");
        let quorum = transcript_quorum(&data, request_type, quorum_hash_text);
        let variant = quorum["quorum_info"]["variants"]
            .as_array()
            .expect("quorum variants")
            .iter()
            .find(|variant| {
                variant["members"].as_array().is_some_and(|members| {
                    members.iter().all(|member| member["pubKeyShare"].as_str().is_some())
                })
            })
            .expect("quorum public key shares");
        let members = variant["members"].as_array().expect("quorum members");
        let threshold = request["threshold"].as_u64().expect("threshold") as usize;
        assert!(threshold >= 2 && threshold <= members.len());

        let llmq_type = LlmqType::from_base(request_type);
        let quorum_hash = QuorumHash::from(transcript_rpc(&request["quorumHash"]));
        let id = transcript_rpc(&request["id"]);
        let msg_hash = transcript_rpc(&request["msgHash"]);
        let expected_sign_hash = sign_hash(llmq_type, &quorum_hash, &id, &msg_hash);
        assert_eq!(expected_sign_hash, transcript_rpc(&request["signHash"]));
        let quorum_public_key = transcript_pk(&variant["quorumPublicKey"]);
        let expected = transcript_sig(&recsig["sig"]);
        assert!(verify_insecure(
            &expected,
            &quorum_public_key,
            &expected_sign_hash,
            BlsSchemeKind::Basic
        ));

        let mut recorded = Vec::with_capacity(shares.len());
        for (member_text, share) in shares {
            let member_index = share["quorumMember"].as_u64().expect("member index") as usize;
            assert!(member_index < members.len());
            assert_eq!(
                member_text,
                members[member_index]["proTxHash"].as_str().expect("member proTxHash")
            );
            assert_eq!(share["signHash"], request["signHash"]);
            let signature = transcript_sig(&share["signature"]);
            let pub_key_share = transcript_pk(&members[member_index]["pubKeyShare"]);
            assert!(verify_insecure(
                &signature,
                &pub_key_share,
                &expected_sign_hash,
                BlsSchemeKind::Basic
            ));
            recorded.push((member_index, signature));
        }
        recorded.sort_by_key(|(index, _)| *index);
        let ids: Vec<_> =
            members.iter().map(|member| transcript_rpc(&member["proTxHash"])).collect();
        for subset in transcript_subsets(members.len(), threshold) {
            let selected: Vec<_> = subset
                .iter()
                .map(|index| {
                    recorded.iter().find(|(member, _)| member == index).expect("share").1.clone()
                })
                .collect();
            let selected_ids: Vec<_> = subset.iter().map(|index| ids[*index]).collect();
            let references: Vec<_> = selected.iter().collect();
            let naive = recover_naive(&references, &selected_ids).expect("naive recovery");
            let msm = recover_msm(&references, &selected_ids).expect("MSM recovery");
            assert_eq!(
                naive.to_bytes(BlsSchemeKind::Basic),
                expected.to_bytes(BlsSchemeKind::Basic)
            );
            assert_eq!(msm.to_bytes(BlsSchemeKind::Basic), expected.to_bytes(BlsSchemeKind::Basic));
            same_value(&naive, &msm);
        }
    }
}

fn twins(sigs: &[BlsSigValue], ids: &[[u8; 32]]) -> Option<BlsSigValue> {
    let refs: Vec<_> = sigs.iter().collect();
    let naive = recover_naive(&refs, ids);
    let msm = recover_msm(&refs, ids);
    let production = recover_signature(&refs, ids);
    match (naive, msm, production) {
        (Some(naive), Some(msm), Some(production)) => {
            same_value(&naive, &msm);
            same_value(&msm, &production);
            Some(msm)
        }
        (None, None, None) => None,
        other => panic!("recovery outcomes differ: {other:?}"),
    }
}

fn polynomial(
    seed: u64,
    threshold: usize,
    n: usize,
    legacy: bool,
) -> (Vec<[u8; 32]>, Vec<BlsSigValue>, BlsSigValue) {
    let mut rng = SeededRng::new(seed);
    let coefficients: Vec<_> = (0..threshold).map(|_| make_new_key(&mut rng)).collect();
    let ids: Vec<_> = (1..=n).map(|i| scalar(i as u64)).collect();
    let (_, shares) = generate_contributions(&coefficients, &ids).unwrap();
    let sigs = shares.iter().map(|sk| sign(sk, &[0x42; 32], legacy)).collect();
    (ids, sigs, sign(&coefficients[0], &[0x42; 32], legacy))
}

#[test]
fn all_llmq_thresholds_and_random_ids_match_the_twin() {
    let mut sizes = vec![2, 3];
    sizes.extend(LlmqType::AVAILABLE.map(|t| usize::from(llmq(t).threshold)));
    for legacy in [false, true] {
        let (ids, sigs, master) = polynomial(11, 3, 340, legacy);
        let mut random_ids = ids.clone();
        let mut rng = SeededRng::new(44);
        for id in &mut random_ids {
            rng.fill_bytes(id);
        }
        for n in sizes.iter().copied().filter(|n| *n >= 2) {
            assert!(twins(&sigs[..n], &random_ids[..n]).is_some());
        }
        for n in [3, 30, 45, 67, 240, 340] {
            same_group_value(&twins(&sigs[..n], &ids[..n]).unwrap(), &master);
        }
    }
}

#[test]
fn none_cases_match_the_twin() {
    let sig = signature();
    assert!(twins(&[], &[]).is_none());
    assert!(twins(slice::from_ref(&sig), &[scalar(1)]).is_none());
    assert!(twins(&[sig.clone(), sig.clone()], &[scalar(1)]).is_none());
    assert!(twins(slice::from_ref(&sig), &[scalar(1), scalar(2)]).is_none());
    for zero in [[0; 32], ORDER] {
        assert!(lagrange_at_zero(&[scalar(1), zero]).is_none());
        assert!(twins(&[sig.clone(), sig.clone()], &[scalar(1), zero]).is_none());
    }
    let mut order_plus_one = ORDER;
    order_plus_one[31] += 1;
    for duplicate in [scalar(1), order_plus_one] {
        let ids = [scalar(1), duplicate];
        assert!(lagrange_at_zero(&ids).is_none());
        assert!(twins(&[sig.clone(), sig.clone()], &ids).is_none());
    }
    assert!(lagrange_at_zero(&[]).is_none());
    assert!(lagrange_at_zero(&[scalar(1)]).is_none());
    let mut stale = sig.clone();
    stale.invalidate();
    assert!(twins(&[stale, sig.clone()], &[scalar(1), scalar(2)]).is_none());
    assert!(twins(&[BlsSigValue::invalid(), sig], &[scalar(1), scalar(2)]).is_none());
}

#[test]
fn infinity_inputs_and_infinite_sums_match_the_twin() {
    let sig = signature();
    let point = BlsSigElem::from_impl(&sig);
    let infinity: BlsSigValue = BlsSigElem::default().into();
    let cancelled: BlsSigValue = point.add(&point.neg()).into();
    let twice: BlsSigValue = point.add(&point).into();
    for inf in [infinity, cancelled] {
        assert!(inf.is_valid());
        twins(&[inf.clone(), sig.clone()], &[scalar(1), scalar(2)]).unwrap();
        twins(&[sig.clone(), inf.clone()], &[scalar(1), scalar(2)]).unwrap();
        twins(&[inf.clone(), inf.clone(), twice.clone()], &[scalar(1), scalar(2), scalar(3)])
            .unwrap();
        twins(&[inf.clone(), inf], &[scalar(1), scalar(2)]).unwrap();
    }
    let recovered = twins(&[sig, twice], &[scalar(1), scalar(2)]).unwrap();
    assert!(recovered.is_valid());
    assert!(BlsSigElem::from_impl(&recovered).core_eq(&BlsSigElem::default()));
}

#[test]
fn coefficient_one_and_order_minus_one_match_core() {
    let mut minus_one = ORDER;
    minus_one[31] -= 1;
    assert_eq!(lagrange_at_zero(&[scalar(1), scalar(2)]).unwrap(), [scalar(2), minus_one]);
    assert_eq!(lagrange_at_zero(&[scalar(1), scalar(2), scalar(3)]).unwrap()[2], scalar(1));
    let sig = signature();
    twins(&[sig.clone(), sig.clone()], &[scalar(1), scalar(2)]).unwrap();
    let point = BlsSigElem::from_impl(&sig);
    let twice = point.add(&point);
    twins(
        &[sig, twice.clone().into(), twice.add(&twice).into()],
        &[scalar(1), scalar(2), scalar(3)],
    )
    .unwrap();
}

#[test]
fn finite_weighted_prefix_cancellation_preserves_the_group_value() {
    let sig = signature();
    let sigs = [&sig; 3];
    let ids = [scalar(1), scalar(2), scalar(3)];
    same_group_value(&recover_naive(&sigs, &ids).unwrap(), &recover_msm(&sigs, &ids).unwrap());
}

fn legacy_off_subgroup_share() -> BlsSigValue {
    // Core: v24.0.0 src/dashbls/src/test.cpp:1599-1617 CheckValid for Legacy
    let hex = "0888879c99852460912fd28c7a9138926c1e87fd6609fd2d3d307764e49feb85702fd8f9b3b836bc11f7ce151b769dc70b760879d26f8c33a29e24f69297f45ef028f0794e63ddb0610db7de1a608b6d6a2129ada62b845004a408f651fd44a6";
    let bytes: [u8; 96] =
        array::from_fn(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap());
    BlsSigValue::from_canonical(&bytes, BlsSchemeKind::Legacy).unwrap()
}

#[test]
fn legacy_off_subgroup_share_uses_the_twin() {
    let exotic = legacy_off_subgroup_share();
    assert!(exotic.is_valid());
    assert!(!BlsSigElem::from_impl(&exotic).is_valid());
    twins(&[exotic.clone(), signature()], &[scalar(1), scalar(2)]).unwrap();
    twins(&[signature(), exotic], &[scalar(1), scalar(3)]).unwrap();
}

#[test]
fn valid_relic_held_share_matches_the_twin() {
    let exotic = BlsSigElem::from_impl(&legacy_off_subgroup_share());
    let sig = signature();
    let point = BlsSigElem::from_impl(&sig);
    let relic = exotic.add(&point).add(&exotic.neg());
    let twice: BlsSigValue = point.add(&point).into();
    assert!(relic.is_valid());
    assert!(relic.__is_relic());
    let relic: BlsSigValue = relic.into();
    same_group_value(&relic, &sig);
    for n in [2, 3, 20] {
        let ids: Vec<_> = (1..=n).map(scalar).collect();
        let shares: Vec<_> = (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    relic.clone()
                } else {
                    twice.clone()
                }
            })
            .collect();
        twins(&shares, &ids).unwrap();
    }
}

#[test]
fn every_sampled_threshold_subset_recovers_the_master() {
    for legacy in [false, true] {
        let (ids, sigs, master) = polynomial(83, 5, 12, legacy);
        let mut rng = SeededRng::new(56);
        for _ in 0..20 {
            let mut indexes: Vec<_> = (0..ids.len()).collect();
            for i in (1..indexes.len()).rev() {
                indexes.swap(i, (rng.next_u64() % (i + 1) as u64) as usize);
            }
            let selected_ids: Vec<_> = indexes[..5].iter().map(|i| ids[*i]).collect();
            let selected: Vec<_> = indexes[..5].iter().map(|i| sigs[*i].clone()).collect();
            same_group_value(&twins(&selected, &selected_ids).unwrap(), &master);
        }
    }
}

#[test]
fn try_recover_threshold_order_and_failures() {
    let params = llmq(LlmqType::LlmqTest);
    let (ids, sigs, master) = polynomial(93, usize::from(params.threshold), 4, false);
    let members: Vec<_> = ids.iter().copied().map(ProTxHash::from).collect();
    let mut shares = BTreeMap::new();
    let mut counter = 99;
    assert!(matches!(try_recover(&params, &members, &shares, &mut counter), TryRecover::NotEnough));
    shares.insert(1, sigs[1].clone());
    assert!(matches!(try_recover(&params, &members, &shares, &mut counter), TryRecover::NotEnough));
    for i in (0..sigs.len()).rev() {
        shares.insert(i as u16, sigs[i].clone());
    }
    shares.insert(3, BlsSigValue::invalid());
    let TryRecover::Recovered {
        sig,
        self_verify,
    } = try_recover(&params, &members, &shares, &mut counter)
    else {
        panic!("enough shares")
    };
    same_group_value(&sig, &master);
    assert!(!self_verify);
    assert_eq!(counter, 100);
    shares.insert(0, BlsSigValue::invalid());
    assert!(matches!(try_recover(&params, &members, &shares, &mut counter), TryRecover::Failed));
    assert_eq!(counter, 100);
}

#[test]
fn single_member_returns_its_share_without_counter_or_share_checks() {
    let params = LlmqParams {
        size: 1,
        threshold: 1,
        ..llmq(LlmqType::LlmqTest)
    };
    let mut counter = 100;
    assert!(matches!(
        try_recover(&params, &[], &BTreeMap::new(), &mut counter),
        TryRecover::NotEnough
    ));
    for sig in [signature(), BlsSigValue::invalid()] {
        let shares = BTreeMap::from([(0, sig.clone())]);
        let TryRecover::Recovered {
            sig: recovered,
            self_verify,
        } = try_recover(&params, &[], &shares, &mut counter)
        else {
            panic!("one share")
        };
        same_value(&sig, &recovered);
        assert!(!self_verify);
        assert_eq!(counter, 100);
    }
}

#[test]
fn self_verify_uses_the_value_before_the_increment_and_wraps() {
    let params = llmq(LlmqType::LlmqTest);
    let (ids, sigs, _) = polynomial(12, usize::from(params.threshold), 3, false);
    let members: Vec<_> = ids.into_iter().map(ProTxHash::from).collect();
    let shares = sigs.into_iter().enumerate().map(|(i, sig)| (i as u16, sig)).collect();
    let mut counter = 0;
    for old in 0u32..=201 {
        let TryRecover::Recovered {
            self_verify,
            ..
        } = try_recover(&params, &members, &shares, &mut counter)
        else {
            panic!("recover")
        };
        assert_eq!(self_verify, old.is_multiple_of(100));
        assert_eq!(counter, old + 1);
    }
    counter = u32::MAX;
    for expected in [false, true, false] {
        let TryRecover::Recovered {
            self_verify,
            ..
        } = try_recover(&params, &members, &shares, &mut counter)
        else {
            panic!("recover")
        };
        assert_eq!(self_verify, expected);
    }
    assert_eq!(counter, 2);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn random_polynomials_match_the_twin_and_master(seed in any::<u64>(), threshold in 2usize..8, legacy in any::<bool>()) {
        let (ids, sigs, master) = polynomial(seed, threshold, threshold + 3, legacy);
        for offset in 0..=3 {
            let recovered = twins(&sigs[offset..offset + threshold], &ids[offset..offset + threshold]).unwrap();
            same_group_value(&recovered, &master);
        }
    }
}
