//! `script_checks` (CS6's PF0 rows): the signature and script-execution caches and the script
//! check queue, gated on instructions and allocations by PF0's counter gate
//! (`crit:dashd-chainstate script_checks`).
//!
//! - `sigcache/verify_hit_1k`: 1,000 signatures verified through [`CachingVerifier`], each a hit
//!   (block connection's lookup, which soft-erases); `sigcache/lookup_miss_1k`: the entry and
//!   lookup every verification pays before a miss verifies.
//! - `exec/hit_1k`: `check_input_scripts` of 1,000 transactions the script-execution cache holds.
//! - `serial/<n>_{cold,warm}`: a block's `n` inputs checked as `-par=1` checks them, with the
//!   signature cache empty or holding every transaction signature (Core's mempool-then-block
//!   path; data signatures are never cached, in Core either); the script-execution cache misses.
//! - `queue/<n>_{cold,warm}`: the same through a queue with one script-check thread (`-par=2`).
//!   PF0 pins each process to one CPU, where the helpers' timing decides how many start, so these
//!   rows report their instructions and allocations rather than gate them.
//!
//! The inputs mix P2PKH spends with outputs whose scripts are chains of three
//! `OP_CHECKDATASIGVERIFY`s (every fourth input), at the sizes around the queue's batch of 128
//! (1, 16, 127, 128, 129) and a full block's worth (680). Transactions have up to eight inputs.

#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

#[path = "../tests/support/signed.rs"]
mod signed;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use dash_consensus::Tx;
use dash_consensus::hash::sha256;
use dash_consensus::script::opcodes::{OP_CHECKDATASIGVERIFY, OP_TRUE};
use dash_consensus::script::{PubKeyRef, ScriptFlags, ScriptPolicy, SigVerifier, push_data};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::checkqueue::script::{ScriptOptions, TxSlot, check_input_scripts};
use dashd_chainstate::sigcache::{
    CacheSizes, CachingVerifier, ScriptCaches, ScriptExecCache, SigCache,
};
use secp256k1::Message;
use signed::{Unlock, p2pkh, pubkey, secret, sign_all, spend_scripts};

const SIZES: [usize; 6] = [1, 16, 127, 128, 129, 680];
const INPUTS_PER_TX: usize = 8;

/// A script checking three data signatures by key `k`, then true. Core checks a data signature's
/// encoding as a transaction signature's (`CheckSignatureEncoding`), so it ends in a hash-type
/// byte, which the lax DER parse then ignores.
fn data_sig_chain(k: usize) -> Vec<u8> {
    let mut script = Vec::new();
    for m in 0..3u8 {
        let message = [m; 32];
        push_data(&mut script, &sign_all(sha256(&message), k));
        push_data(&mut script, &message);
        push_data(&mut script, &pubkey(k));
        script.push(OP_CHECKDATASIGVERIFY);
    }
    script.push(OP_TRUE);
    script
}

/// A block's worth of spends: `inputs` inputs over transactions of up to eight, every fourth
/// spending a data-signature chain and the rest P2PKH outputs, and the scripts each spends.
fn block(inputs: usize) -> Vec<(Tx, Vec<Vec<u8>>)> {
    let is_chain = |i: usize| i % 4 == 3;
    let mut txs = Vec::new();
    let mut left = inputs;
    let mut tag = 0;
    while left > 0 {
        let n = left.min(INPUTS_PER_TX);
        left -= n;
        let spent = (0..n)
            .map(|i| {
                if is_chain(i) {
                    data_sig_chain(i % 4)
                } else {
                    p2pkh(&pubkey(i % 4))
                }
            })
            .collect();
        let unlocks: Vec<_> = (0..n)
            .map(|i| {
                if is_chain(i) {
                    Unlock::Raw(Vec::new())
                } else {
                    Unlock::Signed
                }
            })
            .collect();
        let s = spend_scripts(spent, &unlocks, tag);
        tag += 1;
        txs.push((s.tx, s.spent));
    }
    txs
}

/// Core's default caches, salted with fixed nonces so that every run probes the same buckets.
fn new_caches() -> ScriptCaches {
    let sizes = CacheSizes::default();
    ScriptCaches {
        sig: SigCache::with_nonce(sizes.signature_cache_bytes, [0x5a; 32]).unwrap(),
        exec: ScriptExecCache::with_nonce(sizes.script_execution_cache_bytes, [0xa5; 32]).unwrap(),
    }
}

fn opts(caches: &ScriptCaches, store: bool) -> ScriptOptions<'_> {
    ScriptOptions {
        flags: ScriptFlags::MANDATORY_V24_0,
        policy: ScriptPolicy::V24_0,
        caches: Some(caches),
        cache_sig_store: store,
        cache_full_script_store: false,
    }
}

/// `ConnectBlock`'s script checks of `txs` through `queue`: the verdict.
fn connect(queue: &CheckQueue, txs: &[(Tx, Vec<Vec<u8>>)], opts: &ScriptOptions<'_>) -> bool {
    let slots: Vec<TxSlot<'_>> = txs.iter().map(|(tx, _)| TxSlot::new(tx)).collect();
    let (result, ok) = queue.control(|control| {
        for ((_, spent), slot) in txs.iter().zip(&slots) {
            check_input_scripts(slot, |n| spent[n].as_slice(), opts, Some(control))?;
        }
        Ok::<_, dash_consensus::script::CheckInputsError>(())
    });
    result.is_ok() && ok
}

fn sigcache(c: &mut Criterion) {
    let caches = new_caches();
    let sigs: Vec<([u8; 32], Vec<u8>)> = (0..1_000u32)
        .map(|i| {
            let mut digest = [0; 32];
            digest[..4].copy_from_slice(&i.to_le_bytes());
            let der =
                secp256k1::ecdsa::sign(Message::from_digest(digest), &secret(0)).serialize_der();
            (digest, der.to_vec())
        })
        .collect();
    let key = pubkey(0);
    let pk = PubKeyRef::new(&key).unwrap();
    for (digest, der) in &sigs {
        assert!(CachingVerifier::new(&caches.sig, true).verify(der, pk, digest));
    }
    c.bench_function("sigcache/verify_hit_1k", |b| {
        b.iter(|| {
            let verifier = CachingVerifier::new(&caches.sig, false);
            sigs.iter().all(|(digest, der)| verifier.verify(black_box(der), pk, digest))
        });
    });
    let cold = new_caches();
    c.bench_function("sigcache/lookup_miss_1k", |b| {
        b.iter(|| {
            sigs.iter()
                .filter(|(digest, der)| {
                    let entry = cold.sig.entry(digest, &key, black_box(der));
                    cold.sig.cache().contains(entry, true)
                })
                .count()
        });
    });
}

fn exec(c: &mut Criterion) {
    let caches = new_caches();
    let txs = block(1_000 * INPUTS_PER_TX / 4);
    let store = ScriptOptions {
        cache_full_script_store: true,
        ..opts(&caches, true)
    };
    assert!(connect(&CheckQueue::serial(), &txs, &store));
    let txs: Vec<_> = txs.iter().cycle().take(1_000).collect();
    c.bench_function("exec/hit_1k", |b| {
        b.iter(|| {
            txs.iter().all(|(tx, spent)| {
                let slot = TxSlot::new(tx);
                check_input_scripts(&slot, |n| spent[n].as_slice(), &opts(&caches, false), None)
                    .is_ok()
            })
        });
    });
}

fn inputs(c: &mut Criterion) {
    let serial = CheckQueue::serial();
    let queue = CheckQueue::new(1).unwrap();
    for n in SIZES {
        let txs = block(n);
        for (mode, q) in [("serial", &serial), ("queue", &queue)] {
            let cold = new_caches();
            assert!(connect(q, &txs, &opts(&cold, false)));
            c.bench_function(&format!("{mode}/{n}_cold"), |b| {
                b.iter(|| assert!(connect(q, black_box(&txs), &opts(&cold, false))));
            });
            let warm = new_caches();
            assert!(connect(q, &txs, &opts(&warm, true)));
            c.bench_function(&format!("{mode}/{n}_warm"), |b| {
                b.iter(|| assert!(connect(q, black_box(&txs), &opts(&warm, false))));
            });
        }
    }
}

criterion_group!(benches, sigcache, exec, inputs);
criterion_main!(benches);
