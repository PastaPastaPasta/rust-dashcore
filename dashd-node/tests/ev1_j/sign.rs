//! The fixture's BLS signing, as node-l1's quorum world signs (`node-l1/src/specialtx/quorum.rs`):
//! the basic scheme (V19 is active from height 1 on regtest), `sign_with(.., BlsSigId::Basic)` over
//! a 32-byte hash, and a commitment's `membersSig` as the members' secure aggregate.

use dash_consensus::hash::sha256d;
use dash_pkc::bls::{BlsPublicKey, BlsScIetf, BlsSecretKey, BlsSigId, BlsSignature};

pub type SecretKey = BlsSecretKey<BlsScIetf>;

/// The key from key material `seed`.
pub fn key(seed: u8) -> SecretKey {
    SecretKey::from_ikm(&[seed; 32]).expect("32 bytes of key material")
}

/// An operator key as `bls generate` prints its secret (hex, big-endian), checked against the
/// public key it printed beside it.
pub fn operator_key(secret: &str, public: &str) -> SecretKey {
    let bytes: [u8; 32] = crate::unhex(secret).try_into().expect("a 32-byte secret");
    let key = SecretKey::from_bytes(&bytes).expect("a scalar");
    assert_eq!(crate::v24::hex(&key.public_key().to_bytes()), public, "bls generate's key pair");
    key
}

/// `hash` signed by `key`, as its wire bytes.
pub fn sign(key: &SecretKey, hash: &[u8; 32]) -> [u8; 96] {
    key.sign_with(hash, BlsSigId::Basic).to_bytes()
}

/// The `DYNBITSET` of `bits` (`util/helpers.h`'s `DYNBITSET`: a compact size, then the bits
/// little-endian in bytes).
fn bitset(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![u8::try_from(bits.len()).unwrap()];
    out.resize(1 + bits.len().div_ceil(8), 0);
    for (i, _) in bits.iter().enumerate().filter(|(_, set)| **set) {
        out[1 + i / 8] |= 1 << (i % 8);
    }
    out
}

/// The `CFinalCommitment` (network serialization, version 3: basic scheme, not indexed) of
/// quorum `(llmq_type, base)` of size `size`, whose members in Core's order have the operator
/// keys `members`, all valid and all signing: quorum key `quorum`, vvec hash `[0x11; 32]`,
/// `quorumSig` by the quorum key and `membersSig` the members' secure aggregate, both over
/// `BuildCommitmentHash()` (`llmq/commitment.cpp`: type, base, validMembers, quorum key, vvec
/// hash).
pub fn commitment(
    llmq_type: u8,
    base: &[u8; 32],
    size: usize,
    members: &[&SecretKey],
    quorum: &SecretKey,
) -> Vec<u8> {
    let mut bits = vec![false; size];
    bits[..members.len()].fill(true);
    final_commitment(llmq_type, base, None, &bits, members, quorum)
}

/// The version 4 `CFinalCommitment` (basic scheme, indexed: `quorumIndex`, an int16, after
/// `quorumHash`, `llmq/commitment.h:102-106`) of rotated quorum `(llmq_type, base)` with quorum
/// index `index` and size `size`, whose members in Core's order have the operator keys
/// `members`: every member valid and signing but those at the positions `invalid`, whose
/// `validMembers` and `signers` bits are clear and who do not sign; otherwise as [`commitment`].
/// `BuildCommitmentHash()` leaves `quorumIndex` out.
#[allow(dead_code, reason = "the D14 target's; the J0 target builds this module too")]
pub fn indexed_commitment(
    llmq_type: u8,
    base: &[u8; 32],
    index: i16,
    size: usize,
    members: &[&SecretKey],
    invalid: &[usize],
    quorum: &SecretKey,
) -> Vec<u8> {
    let bits = valid_bits(size, members.len(), invalid);
    let signing: Vec<&SecretKey> =
        members.iter().enumerate().filter(|(i, _)| bits[*i]).map(|(_, k)| *k).collect();
    final_commitment(llmq_type, base, Some(index), &bits, &signing, quorum)
}

/// The `validMembers` bits of a quorum of size `size` with `members` members: set for each member
/// but those at `invalid`.
#[allow(dead_code, reason = "the D14 target's; the J0 target builds this module too")]
pub fn valid_bits(size: usize, members: usize, invalid: &[usize]) -> Vec<bool> {
    assert!(invalid.iter().all(|i| *i < members), "invalid positions are members'");
    (0..size).map(|i| i < members && !invalid.contains(&i)).collect()
}

/// A `CFinalCommitment` whose `signers` and `validMembers` are both `bits` and whose
/// `membersSig` is the secure aggregate of `signing`'s signatures, whatever members `bits` names:
/// version 4 with `index`, else version 3.
pub fn final_commitment(
    llmq_type: u8,
    base: &[u8; 32],
    index: Option<i16>,
    bits: &[bool],
    signing: &[&SecretKey],
    quorum: &SecretKey,
) -> Vec<u8> {
    let bits = bitset(bits);
    let public_key = quorum.public_key().to_bytes();
    let vvec_hash = [0x11; 32];
    let hash = sha256d(&[&[llmq_type][..], base, &bits, &public_key, &vvec_hash].concat());
    let sigs: Vec<BlsSignature<BlsScIetf>> =
        signing.iter().map(|k| k.sign_with(&hash, BlsSigId::Basic)).collect();
    let pks: Vec<BlsPublicKey<BlsScIetf>> = signing.iter().map(|k| k.public_key()).collect();
    let members_sig = BlsSignature::secure_aggregate(
        &sigs.iter().collect::<Vec<_>>(),
        &pks.iter().collect::<Vec<_>>(),
    )
    .expect("an aggregate")
    .to_bytes();
    let version: u16 = if index.is_some() {
        4
    } else {
        3
    };
    let mut qc = version.to_le_bytes().to_vec();
    qc.push(llmq_type);
    qc.extend(base);
    if let Some(index) = index {
        qc.extend(index.to_le_bytes());
    }
    qc.extend(&bits);
    qc.extend(&bits);
    qc.extend(public_key);
    qc.extend(vvec_hash);
    qc.extend(sign(quorum, &hash));
    qc.extend(members_sig);
    qc
}
