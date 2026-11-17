//! Deterministic basic-scheme BLS keys for test fixtures and node sessions.
//!
//! The only BLS key derivation of the fixtures: the recorder's operator keys (through the
//! `testkit-keys` CLI) and in-process session keys both come from [`derive_basic_bls_key`].

use dash_pkc::bls::{BlsPkBytes, BlsScIetf, BlsSecretKey, BlsSkBytes};
use dashcore_hashes::{HashEngine, sha256};

/// Domain tag of the derivation; bumping it is a breaking change to every image.
pub const BASIC_BLS_DOMAIN: &[u8] = b"dash-testkit/basic-bls/v1";

/// A basic-scheme BLS key pair from [`derive_basic_bls_key`], as base-sdk byte bags.
///
/// `sk` is the 32-byte big-endian scalar and `pk` the 48-byte serialized public key, the byte
/// orders Core's `bls generate` and `bls fromsecret` print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasicBlsKey {
    pub sk: BlsSkBytes,
    pub pk: BlsPkBytes,
}

/// The basic-scheme (IETF, post-V19) BLS key number `index` under `seed`.
///
/// `ikm = SHA-256(BASIC_BLS_DOMAIN ‖ seed ‖ index as 4 big-endian bytes)`, then IETF KeyGen.
pub fn derive_basic_bls_key(seed: &[u8; 32], index: u32) -> BasicBlsKey {
    let mut engine = sha256::Hash::engine();
    engine.input(BASIC_BLS_DOMAIN);
    engine.input(seed);
    engine.input(&index.to_be_bytes());
    let ikm = sha256::Hash::from_engine(engine);
    let sk = BlsSecretKey::<BlsScIetf>::from_ikm(ikm.as_byte_array())
        .expect("from_ikm fails only for ikm shorter than 32 bytes");
    BasicBlsKey {
        pk: sk.public_key().into(),
        sk: sk.into(),
    }
}
