//! The worker's read of the DKG payloads (`l/net_dkg.cpp:370-389`), their two hashes and their
//! serialization.

#![allow(clippy::disallowed_methods, reason = "tests build payloads with base-sdk's encoder")]

use dash_p2p_core::dkg_inventory_hash;
use dash_pkc::bls::{BlsIesMultiBytes, BlsPkBytes, BlsSigBytes};
use dash_types::codec::BaseCodec;

use super::*;
use crate::dkg::bls::{BlsSkValue, IesSeed, encrypt_shares, secret_to_public};

const TEST: LlmqType = LlmqType::LlmqTest;

/// Core's static `llmq_test` entry (`llmq/params.h:138`).
fn params() -> LlmqParams {
    dash_params::available_params(TEST).expect("llmq_test")
}

/// A basic secret key from a small number, deterministic and valid.
fn sk(n: u8) -> BlsSkValue {
    let mut bytes = [0u8; 32];
    bytes[0] = 0x11;
    bytes[31] = n;
    BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).expect("below r")
}

/// A well-formed `qcontrib` for `llmq_test` (3 members, threshold 2) with its BLS fields in
/// `flag`'s layout. The signature is a public key's twin in G2: any valid point serves, as the
/// read checks no signature.
fn contribution(flag: BlsSchemeKind) -> DkgContribution {
    let vvec: Vec<BlsPkValue> = [1, 2].map(|n| secret_to_public(&sk(n))).to_vec();
    let recipients: Vec<(BlsPkValue, BlsSkValue)> =
        (10..13).map(|n| (secret_to_public(&sk(n)), sk(n + 10))).collect();
    let refs: Vec<(&BlsPkValue, &BlsSkValue)> = recipients.iter().map(|(pk, s)| (pk, s)).collect();
    let seed = IesSeed {
        ephemeral_sk: sk(99),
        iv_seed: [7; 32],
    };
    let (blobs, ok) = encrypt_shares(&seed, &refs, flag);
    assert!(ok);
    DkgContribution::new(
        TEST,
        QuorumHash::from([3; 32]),
        TxHash::from([4; 32]),
        vvec.iter().map(|pk| BlsPkBytes::from_bytes(pk.to_bytes(flag))).collect(),
        BlsIesMultiBytes::new(
            BlsPkBytes::from_bytes(blobs.ephemeral.to_bytes(flag)),
            blobs.iv_seed,
            blobs.blobs,
        ),
        BlsSigBytes::from_bytes(valid_sig().to_bytes(flag)),
    )
}

/// A valid G2 point: the aggregate of nothing is invalid, so take the hash-to-curve of a
/// signature made through the core verify's own key path.
fn valid_sig() -> BlsSigValue {
    use dash_pkc::bls::{BlsScIetf, BlsSecretKey, BlsSigId};
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[1; 32]).expect("a key");
    let sig = key.sign_with(&[0; 32], BlsSigId::Basic);
    BlsSigValue::from_canonical(&sig.to_bytes(), BlsSchemeKind::Basic).expect("canonical")
}

fn encode(msg: &DkgContribution) -> Vec<u8> {
    let mut out = Vec::new();
    msg.encode(&mut out);
    out
}

#[test]
fn a_canonical_payload_reads_back_with_core_hashes() {
    for (flag, bls) in
        [(BlsSchemeKind::Basic, BlsCtx::BASIC), (BlsSchemeKind::Legacy, BlsCtx::LEGACY)]
    {
        let msg = contribution(flag);
        let payload = encode(&msg);
        let read = Contribution::read(&payload, &params(), bls).expect("reads");
        assert_eq!(read.message(), &msg);
        assert_eq!(read.payload(flag), payload, "CoreWrite writes base-sdk's bytes");
        // Nothing normalized: the session hash is the inventory hash.
        assert_eq!(read.session_hash(), dkg_inventory_hash(&payload).as_bytes());
        // BS9a's GetSignHash over the same message.
        assert_eq!(read.sign_hash(), msg.sign_hash().as_bytes());
        assert_eq!(read.pro_tx_hash(), &ProTxHash::from([4; 32]));
        assert_eq!(read.vvec().len(), 2);
        assert!(read.vvec().iter().all(BlsPkValue::is_valid));
        assert!(read.sig().is_valid());
        assert!(read.blobs().ephemeral.is_valid());
        assert_eq!(read.blobs().blobs.len(), 3);
    }
}

#[test]
fn the_vvec_scheme_flag_is_the_reads_and_false_when_built_in_place() {
    // `vvecLegacyScheme` is the scheme the read decodes the whole vvec under, and `false` on a
    // contribution built in place (`l/dkgmessages.h:29-30,57-58`, v24.0.0). The session's own
    // contribution reaches `ReceiveMessage` serialized and read again (`EnqueueOwn`,
    // `l/net_dkg.cpp:369-378,388,895`, v24.0.0), so it carries the read's flag too.
    for (flag, bls) in
        [(BlsSchemeKind::Basic, BlsCtx::BASIC), (BlsSchemeKind::Legacy, BlsCtx::LEGACY)]
    {
        let read = Contribution::read(&encode(&contribution(flag)), &params(), bls).expect("reads");
        assert_eq!(read.vvec_legacy_scheme(), flag.is_legacy());
        let built = Contribution::unsigned(
            read.llmq_type(),
            read.quorum_hash(),
            read.pro_tx_hash(),
            read.vvec(),
            read.blobs(),
            flag,
        )
        .with_sig(valid_sig(), flag);
        assert!(!built.vvec_legacy_scheme());
        let again = Contribution::read(&built.payload(flag), &params(), bls).expect("reads");
        assert_eq!(again.vvec_legacy_scheme(), flag.is_legacy());
    }
}

#[test]
fn eager_normalization_moves_the_session_hash_off_the_inventory_hash() {
    // A basic-layout signature read under the legacy flag is canonical only in the other
    // scheme, so Core's eager read zeroes it (BS9a `dkg.rs:117-124`).
    let payload = encode(&contribution(BlsSchemeKind::Basic));
    let read = Contribution::read(&payload, &params(), BlsCtx::LEGACY).expect("reads");
    assert!(!read.sig().is_valid());
    assert_eq!(read.message().sig().as_bytes(), &[0; 96]);
    assert_ne!(read.payload(BlsSchemeKind::Legacy), payload);
    assert_ne!(read.session_hash(), dkg_inventory_hash(&payload).as_bytes());
    assert_eq!(
        read.session_hash(),
        dkg_inventory_hash(&read.payload(BlsSchemeKind::Legacy)).as_bytes()
    );
    assert_eq!(read.sign_hash(), read.message().sign_hash().as_bytes());
}

#[test]
fn a_read_failure_is_deserialize_or_structure_in_cores_order() {
    let msg = contribution(BlsSchemeKind::Basic);
    let payload = encode(&msg);
    let read = |bytes: &[u8]| Contribution::read(bytes, &params(), BlsCtx::BASIC).err();

    let mut trailing = payload.clone();
    trailing.push(0);
    assert_eq!(read(&trailing), Some(ReadFailure::Deserialize));
    assert_eq!(read(&payload[..payload.len() - 1]), Some(ReadFailure::Deserialize));

    // A field canonical in neither scheme throws inside the read.
    let mut bad_key = msg.clone();
    {
        let mut vvec = bad_key.vvec().clone();
        vvec[0] = BlsPkBytes::from_bytes([0xff; 48]);
        bad_key = dash_p2p_core::DkgContribution::new(
            bad_key.llmq_type(),
            bad_key.quorum_hash(),
            bad_key.pro_tx_hash(),
            vvec,
            bad_key.contributions().clone(),
            bad_key.sig(),
        );
    }
    assert_eq!(read(&encode(&bad_key)), Some(ReadFailure::Deserialize));

    // CheckDKGMessageStructure: vvec length, then the blob count's bounds.
    let mut short = msg.clone();
    {
        let mut vvec = short.vvec().clone();
        vvec.pop();
        short = dash_p2p_core::DkgContribution::new(
            short.llmq_type(),
            short.quorum_hash(),
            short.pro_tx_hash(),
            vvec,
            short.contributions().clone(),
            short.sig(),
        );
    }
    assert_eq!(read(&encode(&short)), Some(ReadFailure::Structure));
    let shares = msg.contributions();
    let with_blobs = |n: usize| {
        let mut m = msg.clone();
        m = dash_p2p_core::DkgContribution::new(
            m.llmq_type(),
            m.quorum_hash(),
            m.pro_tx_hash(),
            m.vvec().clone(),
            BlsIesMultiBytes::new(*shares.ephemeral_pk(), *shares.iv_seed(), vec![vec![0; 48]; n]),
            m.sig(),
        );
        encode(&m)
    };
    assert_eq!(read(&with_blobs(1)), Some(ReadFailure::Structure));
    assert_eq!(read(&with_blobs(4)), Some(ReadFailure::Structure));
    assert!(read(&with_blobs(2)).is_none(), "minSize blobs pass the structure check");

    // The read's failure comes first: a bad key and a short vvec is a deserialize failure.
    let mut both = short;
    {
        let mut vvec = both.vvec().clone();
        vvec[0] = BlsPkBytes::from_bytes([0xff; 48]);
        both = dash_p2p_core::DkgContribution::new(
            both.llmq_type(),
            both.quorum_hash(),
            both.pro_tx_hash(),
            vvec,
            both.contributions().clone(),
            both.sig(),
        );
    }
    assert_eq!(read(&encode(&both)), Some(ReadFailure::Deserialize));
}

#[test]
fn a_payload_is_written_under_the_scheme_it_is_served_in() {
    // BL-480: Core keeps the BLS objects and writes each under the scheme in force when it
    // serializes. Mutant: write the bytes as the read left them.
    let legacy = contribution(BlsSchemeKind::Legacy);
    let read = Contribution::read(&encode(&legacy), &params(), BlsCtx::LEGACY).expect("reads");
    assert_eq!(read.payload(BlsSchemeKind::Legacy), encode(&legacy));
    let basic = contribution(BlsSchemeKind::Basic);
    let ephemeral = secret_to_public(&sk(99)).to_bytes(BlsSchemeKind::Basic);
    let want = DkgContribution::new(
        basic.llmq_type(),
        basic.quorum_hash(),
        basic.pro_tx_hash(),
        basic.vvec().clone(),
        BlsIesMultiBytes::new(
            BlsPkBytes::from_bytes(ephemeral),
            read.blobs().iv_seed,
            read.blobs().blobs.clone(),
        ),
        basic.sig(),
    );
    assert_ne!(encode(&want), encode(&legacy));
    assert_eq!(read.payload(BlsSchemeKind::Basic), encode(&want));
}

#[test]
fn a_complaint_is_written_as_cores_dynbitsets() {
    // Mutants: the bits MSB first; the bit count left out; the signature written as read.
    let flag = BlsSchemeKind::Basic;
    let (qh, pro_tx) = (QuorumHash::from([3; 32]), ProTxHash::from([4; 32]));
    let bad = [true, false, false];
    let complain_for = [false, false, true];
    let qc = Complaint::unsigned(TEST, &qh, &pro_tx, &bad, &complain_for, flag)
        .with_sig(valid_sig(), flag);
    let raw = qc.payload(flag);
    assert_eq!(raw.len(), 65 + 4 + 96);
    assert_eq!(raw[65..69], [3, 0b001, 3, 0b100]);
    let read = Complaint::read(&raw, &params(), BlsCtx::BASIC).expect("reads");
    assert_eq!(read.payload(flag), raw);
    assert_eq!(read.session_hash(), dkg_inventory_hash(&raw).as_bytes());
    // BS9a's GetSignHash over the same message.
    assert_eq!(read.sign_hash(), read.msg.sign_hash().as_bytes());
    assert_eq!((0..3).map(|i| read.bad_member(i)).collect::<Vec<_>>(), bad);
    assert_eq!((0..3).map(|i| read.complain_for(i)).collect::<Vec<_>>(), complain_for);
    assert!(!read.bad_member(3) && !read.complain_for(3), "false past the end");
    // Under the legacy scheme only the signature's layout changes.
    let legacy = read.payload(BlsSchemeKind::Legacy);
    assert_eq!(legacy[..69], raw[..69]);
    assert_eq!(legacy[69..], valid_sig().to_bytes(BlsSchemeKind::Legacy));
}

#[test]
fn a_justification_is_written_as_index_and_key_pairs() {
    let flag = BlsSchemeKind::Basic;
    let (qh, pro_tx) = (QuorumHash::from([3; 32]), ProTxHash::from([4; 32]));
    let shares = [(0, sk(5)), (2, sk(6))];
    let qj = Justification::unsigned(TEST, &qh, &pro_tx, &shares, flag).with_sig(valid_sig(), flag);
    let raw = qj.payload(flag);
    assert_eq!(raw.len(), 65 + 1 + 2 * 36 + 96);
    assert_eq!(raw[65], 2);
    assert_eq!(raw[66..70], 0u32.to_le_bytes());
    assert_eq!(raw[70..102], sk(5).to_bytes(flag));
    assert_eq!(raw[102..106], 2u32.to_le_bytes());
    let read = Justification::read(&raw, &params(), BlsCtx::BASIC).expect("reads");
    assert_eq!(read.keys(), shares);
    assert_eq!(read.payload(flag), raw);
    assert_eq!(read.session_hash(), dkg_inventory_hash(&raw).as_bytes());
    assert_eq!(read.sign_hash(), read.msg.sign_hash().as_bytes());
    // Mutant: the signature written as read. Secret keys share one layout, so under the legacy
    // scheme only the signature's changes.
    let legacy = read.payload(BlsSchemeKind::Legacy);
    let sig_at = raw.len() - 96;
    assert_eq!(legacy[..sig_at], raw[..sig_at]);
    assert_eq!(legacy[sig_at..], valid_sig().to_bytes(BlsSchemeKind::Legacy));
}

#[test]
fn a_premature_commitment_is_written_and_hashed_as_cores() {
    // Mutants: the proTxHash in the sign hash; the signatures swapped or left out of the session
    // hash; the key written as read.
    let flag = BlsSchemeKind::Basic;
    let (qh, pro_tx) = (QuorumHash::from([3; 32]), ProTxHash::from([4; 32]));
    let valid = [true, true, false];
    let pk = secret_to_public(&sk(1));
    let qc = PrematureCommitment::unsigned(TEST, &qh, &pro_tx, &valid, &pk, &[9; 32], flag);
    assert!(!qc.sig().is_valid() && !qc.quorum_sig().is_valid());
    let quorum_sig = valid_sig();
    let sig = BlsSigValue::from_canonical(
        &{
            let mut bytes = quorum_sig.to_bytes(flag);
            bytes.reverse();
            bytes
        },
        flag,
    )
    .unwrap_or_else(|| crate::dkg::bls::sign_stand_in(&sk(2), &[1; 32], false));
    assert_ne!(sig, quorum_sig);
    let qc = qc.with_quorum_sig(quorum_sig.clone(), flag).with_sig(sig.clone(), flag);
    let raw = qc.payload(flag);
    assert_eq!(raw.len(), 65 + 2 + 48 + 32 + 2 * 96);
    assert_eq!(raw[65..67], [3, 0b011]);
    assert_eq!(raw[67..115], pk.to_bytes(flag));
    assert_eq!(raw[115..147], [9; 32]);
    assert_eq!(raw[147..243], quorum_sig.to_bytes(flag));
    assert_eq!(raw[243..], sig.to_bytes(flag));
    let read = PrematureCommitment::read(&raw, &params(), BlsCtx::BASIC).expect("reads");
    assert_eq!(read.payload(flag), raw);
    assert_eq!(read.session_hash(), dkg_inventory_hash(&raw).as_bytes());
    assert_eq!(qc.session_hash(), read.session_hash());
    assert_eq!(read.sign_hash(), read.msg.sign_hash().as_bytes());
    assert_eq!(read.sign_hash(), qc.sign_hash());
    assert_eq!(read.valid_members(), valid);
    assert_eq!(read.quorum_vvec_hash(), &[9; 32]);
    assert!(read.quorum_public_key().core_eq(&pk));
    // `BuildCommitmentHash` (LQ2's) over the same fields: no proTxHash and no signature.
    let bits = read.msg.valid_members().clone();
    let eager = dash_pkc::bls::BlsEagerPk::new(pk.clone(), flag);
    let want =
        dash_evo::llmq::build_commitment_hash(TEST, &qh, &bits, &eager, &[9; 32], BlsCtx::BASIC);
    assert_eq!(*read.sign_hash(), want);
    let other = PrematureCommitment::unsigned(
        TEST,
        &qh,
        &ProTxHash::from([5; 32]),
        &valid,
        &pk,
        &[9; 32],
        flag,
    );
    assert_eq!(other.sign_hash(), qc.sign_hash());
    // Under the legacy scheme the key and both signatures change layout.
    let legacy = read.payload(BlsSchemeKind::Legacy);
    assert_eq!(legacy[..67], raw[..67]);
    assert_eq!(legacy[67..115], pk.to_bytes(BlsSchemeKind::Legacy));
    assert_eq!(legacy[147..243], quorum_sig.to_bytes(BlsSchemeKind::Legacy));
    assert_eq!(legacy[243..], sig.to_bytes(BlsSchemeKind::Legacy));
}

#[test]
fn a_vvec_hash_is_the_serialize_hash_of_the_vector() {
    // Mutants: no length prefix; the keys in the other layout.
    let vvec: Vec<BlsPkValue> = [1, 2, 3].map(|n| secret_to_public(&sk(n))).to_vec();
    for flag in [BlsSchemeKind::Basic, BlsSchemeKind::Legacy] {
        let want = dash_pkc::bls::core::serialize_hash_vector(&vvec, flag);
        assert_eq!(vvec_hash(&vvec, flag), *want.as_bytes(), "{flag:?}");
    }
    assert_ne!(vvec_hash(&vvec, BlsSchemeKind::Basic), vvec_hash(&vvec, BlsSchemeKind::Legacy));
    let mut raw = vec![3u8];
    for pk in &vvec {
        raw.extend_from_slice(&pk.to_bytes(BlsSchemeKind::Basic));
    }
    assert_eq!(vvec_hash(&vvec, BlsSchemeKind::Basic), *dkg_inventory_hash(&raw).as_bytes());
}
