//! What the spork integration tests share.

// Each test binary uses part of it.
#![allow(dead_code)]

use dash_pkc::ecdsa::EcdsaSkBytes;
use secp256k1::SecretKey;

/// Core's functional-test spork key (`test_framework.py:1514`), regtest's spork address's key
/// and the fixtures' `-sporkkey`.
pub const REGTEST_WIF: &str = "cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";

/// [`REGTEST_WIF`]'s secret key (compressed).
pub fn regtest_key() -> SecretKey {
    let (sk, _) = EcdsaSkBytes::from_wif(REGTEST_WIF).expect("a WIF");
    SecretKey::from_secret_bytes(*sk.to_bytes()).expect("a valid scalar")
}
