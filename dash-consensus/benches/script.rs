//! Script and ECDSA benchmarks under Dash Core's `bench_dash` names (CS1 §8.4), so B1's
//! `microbench --rust-bench-bin` comparison and PF0's counters line up row for row.
//!
//! - `ECDSAVerify` / `ECDSAVerify_LargeBlock` (`src/bench/ecdsa.cpp`): `CPubKey::Verify` over 100
//!   (one per iteration, round robin) or 1,000 (all per iteration) uncompressed keys, each signing
//!   `SerializeHash((int)i)`. Core draws random keys; these are fixed (secret i + 1), which does not
//!   change the work. This is also the loop EC1 measures (DEC-38).
//! - `VerifyNestedIfScript` (`src/bench/verify_script.cpp`): `EvalScript` of 100 nested `OP_1 OP_IF`,
//!   1,000 `OP_1` and 100 `OP_ENDIF` with `BaseSignatureChecker`.
//!
//! The sighash and block-input rows come with CS1 phases B and C.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use dash_consensus::hash::sha256d;
use dash_consensus::script::opcodes::{OP_1, OP_ENDIF, OP_IF};
use dash_consensus::script::{NoSignatureChecker, PubKeyRef, ScriptFlags, eval_script, verify_lax};
use secp256k1::{Message, PublicKey, SecretKey};

/// `count` (uncompressed key, `SerializeHash((int)i)`, DER signature) triples, each checked to
/// verify here so that the timed loops (which, like Core's, ignore the result) measure the success
/// path.
fn signed(count: u32) -> Vec<(Vec<u8>, [u8; 32], Vec<u8>)> {
    (0..count)
        .map(|i| {
            let mut secret = [0u8; 32];
            secret[28..].copy_from_slice(&(i + 1).to_be_bytes());
            let key = SecretKey::from_secret_bytes(secret).expect("valid secret");
            // SerializeHash((int)i): SHA256d of the 4-byte little-endian integer.
            let hash = sha256d(&(i as i32).to_le_bytes());
            let sig = secp256k1::ecdsa::sign(Message::from_digest(hash), &key);
            let pubkey = PublicKey::from_secret_key(&key).serialize_uncompressed().to_vec();
            let der = sig.serialize_der().to_vec();
            assert!(verify_lax(PubKeyRef::new(&pubkey).expect("valid key"), &hash, &der));
            (pubkey, hash, der)
        })
        .collect()
}

fn ecdsa_verify(c: &mut Criterion) {
    let keys = signed(100);
    let mut i = 0;
    c.bench_function("ECDSAVerify", |b| {
        b.iter(|| {
            let (pubkey, hash, sig) = &keys[i];
            let ok = verify_lax(PubKeyRef::new(pubkey).expect("valid key"), hash, sig);
            i = (i + 1) % keys.len();
            black_box(ok)
        })
    });
}

fn ecdsa_verify_large_block(c: &mut Criterion) {
    let keys = signed(1000);
    c.bench_function("ECDSAVerify_LargeBlock", |b| {
        b.iter(|| {
            for (pubkey, hash, sig) in &keys {
                black_box(verify_lax(PubKeyRef::new(pubkey).expect("valid key"), hash, sig));
            }
        })
    });
}

fn verify_nested_if_script(c: &mut Criterion) {
    let mut script = Vec::with_capacity(1200 + 100);
    for _ in 0..100 {
        script.extend([OP_1, OP_IF]);
    }
    script.extend([OP_1; 1000]);
    script.extend([OP_ENDIF; 100]);
    let stack: Vec<Vec<u8>> = Vec::new();
    c.bench_function("VerifyNestedIfScript", |b| {
        b.iter(|| {
            let mut stack_copy = stack.clone();
            let ret = eval_script(&mut stack_copy, &script, ScriptFlags::NONE, &NoSignatureChecker);
            assert!(ret.is_ok());
            black_box(stack_copy)
        })
    });
}

criterion_group!(benches, ecdsa_verify, ecdsa_verify_large_block, verify_nested_if_script);
criterion_main!(benches);
