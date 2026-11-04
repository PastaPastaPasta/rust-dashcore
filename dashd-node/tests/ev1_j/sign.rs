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
    assert_eq!(crate::hex(&key.public_key().to_bytes()), public, "bls generate's key pair");
    key
}

/// `hash` signed by `key`, as its wire bytes.
pub fn sign(key: &SecretKey, hash: &[u8; 32]) -> [u8; 96] {
    key.sign_with(hash, BlsSigId::Basic).to_bytes()
}

/// The size-`size` `DYNBITSET` with its first `set` bits set (`util/helpers.h`'s `DYNBITSET`:
/// a compact size, then the bits little-endian in bytes).
fn bitset(size: usize, set: usize) -> Vec<u8> {
    let mut bits = vec![u8::try_from(size).unwrap()];
    bits.resize(1 + size.div_ceil(8), 0);
    for i in 0..set {
        bits[1 + i / 8] |= 1 << (i % 8);
    }
    bits
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
    let bits = bitset(size, members.len());
    let public_key = quorum.public_key().to_bytes();
    let vvec_hash = [0x11; 32];
    let hash = sha256d(&[&[llmq_type][..], base, &bits, &public_key, &vvec_hash].concat());
    let sigs: Vec<BlsSignature<BlsScIetf>> =
        members.iter().map(|k| k.sign_with(&hash, BlsSigId::Basic)).collect();
    let pks: Vec<BlsPublicKey<BlsScIetf>> = members.iter().map(|k| k.public_key()).collect();
    let members_sig = BlsSignature::secure_aggregate(
        &sigs.iter().collect::<Vec<_>>(),
        &pks.iter().collect::<Vec<_>>(),
    )
    .expect("an aggregate")
    .to_bytes();
    let mut qc = 3u16.to_le_bytes().to_vec();
    qc.push(llmq_type);
    qc.extend(base);
    qc.extend(&bits);
    qc.extend(&bits);
    qc.extend(public_key);
    qc.extend(vvec_hash);
    qc.extend(sign(quorum, &hash));
    qc.extend(members_sig);
    qc
}
