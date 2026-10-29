//! L0 vectors of the ECDSA checks (EV1 §2.5) and the verifier double.

use dash_consensus::primitives::decode_tx_exact;
use dash_core_vectors::cases::BlsScheme;
use dash_evo::spork::{key_id_of, sign_compact};
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::SpecialPayload;
use secp256k1::{PublicKey, SecretKey};

use super::*;
use crate::specialtx::trivial::tests::{pro_reg, pro_up_serv};

/// secp256k1's group order, big-endian.
const ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];

fn signer(seed: u8, compressed: bool) -> (SecretKey, KeyId) {
    let key = SecretKey::from_secret_bytes([seed; 32]).expect("a key");
    let public = PublicKey::from_secret_key(&key);
    (key, KeyId::from(key_id_of(&public, compressed)))
}

/// The same signature with S negated (`n - s`) and the recovery id's parity flipped: it recovers
/// the same key.
fn high_s(sig: &[u8]) -> Vec<u8> {
    let mut out = sig.to_vec();
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = i16::from(ORDER[i]) - i16::from(sig[33 + i]) - borrow;
        out[33 + i] = d.rem_euclid(256) as u8;
        borrow = i16::from(d < 0);
    }
    out[0] = 27 + ((sig[0] - 27) ^ 1);
    out
}

#[test]
fn signatures_verify_against_their_key() {
    let hash = [7u8; 32];
    for compressed in [true, false] {
        let (key, id) = signer(1, compressed);
        let sig = sign_compact(&hash, &key, compressed);
        assert!(verify_hash(&hash, &id, &sig));
        assert!(verify_hash_canonical(&hash, &id, &sig));
        // Another key, hash or compression.
        assert!(!verify_hash(&hash, &signer(2, compressed).1, &sig));
        assert!(!verify_hash(&[8; 32], &id, &sig));
        assert!(!verify_hash(&hash, &signer(1, !compressed).1, &sig));
        // Not 65 bytes.
        assert!(!verify_hash(&hash, &id, &sig[..64]));
        assert!(!verify_hash_canonical(&hash, &id, &[sig.clone(), vec![0]].concat()));
    }
}

/// `RecoverCompact` masks the header, so 8 above it recovers the same key; the canonical check
/// takes 27 to 34 only.
#[test]
fn header_aliases_are_not_canonical() {
    let hash = [9u8; 32];
    let (key, id) = signer(3, true);
    let sig = sign_compact(&hash, &key, true);
    assert!((31..=34).contains(&sig[0]));
    let mut alias = sig.clone();
    alias[0] += 8;
    assert!(verify_hash(&hash, &id, &alias));
    assert!(!verify_hash_canonical(&hash, &id, &alias));
    for header in [0u8, 26, 35, 255] {
        alias[0] = header;
        assert!(!verify_hash_canonical(&hash, &id, &alias), "header {header}");
    }
}

/// A high-S signature recovers the key, so `VerifyHash` takes it; `VerifyHashCanonical` does not.
#[test]
fn high_s_is_not_canonical() {
    let hash = [11u8; 32];
    for compressed in [true, false] {
        let (key, id) = signer(4, compressed);
        let sig = sign_compact(&hash, &key, compressed);
        assert!(sig[33..] <= HALF_ORDER[..]);
        let malleated = high_s(&sig);
        assert!(malleated[33..] > HALF_ORDER[..]);
        assert!(verify_hash(&hash, &id, &malleated));
        assert!(!verify_hash_canonical(&hash, &id, &malleated));
    }
    // S at the half order is low.
    let (key, id) = signer(5, true);
    let mut sig = sign_compact(&hash, &key, true);
    sig[33..].copy_from_slice(&HALF_ORDER);
    assert_eq!(verify_hash_canonical(&hash, &id, &sig), verify_hash(&hash, &id, &sig));
}

#[test]
fn messages_sign_their_magic_hash() {
    let (key, id) = signer(6, true);
    let message = "XtB2b2pWzHzeGcRfGKYCZpvgvhQZBkEvxT|0|Xo…|Xo…|00ff";
    let sig = sign_compact(&message_hash(message), &key, true);
    assert!(verify_message(&id, &sig, message));
    assert!(!verify_message(&id, &sig, &message[1..]));
    // The hash of the text, not the text's own hash.
    assert!(!verify_hash(&sha256d(message.as_bytes()), &id, &sig));
}

#[test]
fn fixed_verifier_answers_by_scheme() {
    let verifier = FixedVerifier {
        basic: Some(true),
        legacy: None,
        ..FixedVerifier::default()
    };
    // Core's payloads: a ProUpServTx's signature and a ProRegTx's operator key.
    let (sig, pk) = (pro_up_serv(BlsScheme::Basic).sig, pro_reg(BlsScheme::Basic).pub_key_operator);
    let msg = [0; 32];
    let (basic, legacy) = (VerifyScheme::after_v19(true), VerifyScheme::after_v19(false));
    assert_eq!(verifier.bls_verify_insecure(&sig, &pk, &msg, basic), Ok(true));
    assert_eq!(verifier.bls_verify_insecure(&sig, &pk, &msg, legacy), Err(VerifyUnavailable));
    assert_eq!(verifier.calls.get(), 2);
}

/// The verify scheme is Core's global flag: legacy until V19 is active after the block's parent,
/// whatever scheme the payload's own key or signature decoded with.
#[test]
fn verify_scheme_follows_v19() {
    assert_eq!(VerifyScheme::after_v19(false).kind(), BlsSchemeKind::Legacy);
    assert_eq!(VerifyScheme::after_v19(true).kind(), BlsSchemeKind::Basic);
}

/// BS6b's Core-signed transactions (`tests/data/core_signed_protx.txt`): the transaction and its
/// signers' key ids (none listed when the payload names them: a shared registration's owners).
fn core_signed() -> Vec<(Tx, Vec<KeyId>)> {
    let text = include_str!("../../../tests/data/core_signed_protx.txt");
    let unhex = |s: &str| -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    };
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .map(|line| {
            let [_, network, ids, raw] = line.split(' ').collect::<Vec<_>>()[..] else {
                panic!("a fixture line: {line}");
            };
            assert_eq!(network, "regtest", "the fixture's network");
            let ids = ids.split(',').filter(|id| *id != "-");
            let ids = ids.map(|id| KeyId::from(<[u8; 20]>::try_from(unhex(id)).expect("20 bytes")));
            let raw = unhex(raw);
            let tx = decode_tx_exact(&raw).expect("Core's transaction").into_value();
            (tx, ids.collect())
        })
        .collect()
}

/// Our preimages are the hashes Core's signers signed (EV1 §3.18): each ECDSA signature of a
/// Core-signed ProRegTx (external collateral message, shared registration consent), ProUpRegTx,
/// ProDisTx, ProUpShareTx and ProUpSharedRegTx verifies over ours.
#[test]
fn core_signatures_verify_over_our_preimages() {
    let params = dash_params::ChainParamsBuilder::new(
        dash_params::NetworkKind::Regtest,
        dash_params::Release::V24_0,
    )
    .build()
    .expect("Core's parameters");
    let signed_by = |ids: &[KeyId], hash: &[u8; 32], sig: &[u8]| {
        ids.iter().any(|id| verify_hash_canonical(hash, id, sig))
    };
    let mut verified = std::collections::BTreeMap::<&str, usize>::new();
    for (tx, ids) in core_signed() {
        let Some(Ok(decoded)) = tx.payload() else {
            panic!("{}: Core's payload", tx.txid());
        };
        let txid = tx.txid();
        let kind = match &decoded.payload {
            SpecialPayload::ProviderRegister(p) if !p.shares.is_empty() => {
                let consent = shared_reg_consent_hash(p, &tx).expect("a consent hash");
                for (share, sig) in p.shares.iter().zip(&p.join_sigs) {
                    assert!(verify_hash_canonical(&consent, &share.key_id_owner, sig.as_bytes()));
                }
                "shared registration"
            }
            SpecialPayload::ProviderRegister(p) => {
                let text = make_sign_string(p, &params).expect("a sign string");
                assert!(ids.iter().any(|id| verify_message(id, &p.vch_sig, &text)), "{txid}");
                "registration message"
            }
            SpecialPayload::ProviderUpdateRegistrar(p) => {
                let hash = payload_sig_hash(p).expect("a hash");
                assert!(ids.iter().any(|id| verify_hash(&hash, id, &p.vch_sig)), "{txid}");
                "registrar update"
            }
            SpecialPayload::ProviderDissolve(p) => {
                let count = u8::try_from(p.sigs.len()).expect("at most 8 signatures");
                let hash = pro_dis_sign_hash(p, &tx, count);
                assert!(p.sigs.iter().all(|s| signed_by(&ids, &hash, s.as_bytes())), "{txid}");
                "dissolution"
            }
            SpecialPayload::ProviderUpdateShare(p) => {
                let hash = payload_sig_hash(p).expect("a hash");
                assert!(signed_by(&ids, &hash, &p.vch_sig), "{txid}");
                "share update"
            }
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
                let hash = payload_sig_hash(p).expect("a hash");
                assert!(p.sigs.iter().all(|s| signed_by(&ids, &hash, s.as_bytes())), "{txid}");
                "shared registrar update"
            }
            other => panic!("{txid}: not an ECDSA-signed payload: {other:?}"),
        };
        *verified.entry(kind).or_default() += 1;
    }
    assert_eq!(verified.len(), 6, "every signer kind: {verified:?}");
    assert!(verified.values().all(|n| *n == 2), "{verified:?}");
}

/// The inputs hash a Core-signed ProTx carries is ours (`CalcTxInputsHash`, EV1 §3.18).
#[test]
fn core_inputs_hashes_are_ours() {
    let mut checked = 0;
    for (tx, _) in core_signed() {
        let Some(Ok(decoded)) = tx.payload() else {
            panic!("{}: Core's payload", tx.txid());
        };
        let carried = match &decoded.payload {
            SpecialPayload::ProviderRegister(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateService(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateRegistrar(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateRevoke(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateShare(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => p.inputs_hash,
            _ => continue,
        };
        assert_eq!(carried.as_bytes(), &inputs_hash(&tx), "{}", tx.txid());
        checked += 1;
    }
    // Two of the twelve are ProDisTx, which carries none.
    assert_eq!(checked, 10);
}

/// `CalcTxInputsHash` streams each prevout (hash, then the index little-endian) in input order,
/// with no count: no inputs hash the empty string, and swapping two inputs changes it.
#[test]
fn inputs_hash_streams_the_prevouts() {
    let (tx, _) = core_signed().remove(0);
    let mut raw = tx.to_mutable();
    let first = raw.inputs[0].clone();
    let mut second = first.clone();
    second.prevout.index = first.prevout.index.wrapping_add(1);

    raw.inputs = vec![];
    assert_eq!(inputs_hash(&Tx::new(raw.clone())), sha256d(&[]));

    raw.inputs = vec![first.clone(), second.clone()];
    let mut stream = Vec::new();
    for input in [&first, &second] {
        stream.extend_from_slice(input.prevout.hash.as_bytes());
        stream.extend_from_slice(&input.prevout.index.to_le_bytes());
    }
    let ordered = inputs_hash(&Tx::new(raw.clone()));
    assert_eq!(ordered, sha256d(&stream));

    raw.inputs = vec![second, first];
    assert_ne!(inputs_hash(&Tx::new(raw)), ordered);
}
