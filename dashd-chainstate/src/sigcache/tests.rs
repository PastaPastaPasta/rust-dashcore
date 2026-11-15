use dash_consensus::hash::sha256;
use dash_consensus::script::{PubKeyRef, ScriptFlags, SigVerifier, verify_lax};
use secp256k1::{Message, PublicKey, SecretKey};

use super::*;

const NONCE: [u8; 32] = [0x5a; 32];

/// Core's entry: `SHA256(nonce ‖ nonce ‖ parts…)`.
fn reference_entry(parts: &[&[u8]]) -> CacheEntry {
    let mut preimage = [NONCE, NONCE].concat();
    for part in parts {
        preimage.extend_from_slice(part);
    }
    let digest = sha256(&preimage);
    CacheEntry(digest)
}

/// A made-up entry for the one-bucket tests.
fn e(k: u64) -> CacheEntry {
    let mut key = [0; 32];
    key[..8].copy_from_slice(&k.to_le_bytes());
    CacheEntry(key)
}

#[test]
fn sizes_follow_apply_args_man_options() {
    let half = DEFAULT_MAX_SIG_CACHE_BYTES / 2;
    assert_eq!(
        CacheSizes::from_max_sig_cache_size(None),
        CacheSizes {
            signature_cache_bytes: half,
            script_execution_cache_bytes: half
        }
    );
    let each = |mib| CacheSizes::from_max_sig_cache_size(Some(mib)).signature_cache_bytes;
    assert_eq!(each(32), half);
    // The functional framework's -maxsigcachesize=1.
    assert_eq!(each(1), 512 << 10);
    assert_eq!(each(0), 0);
    assert_eq!(each(-7), 0);
    let upper = i64::MAX / (1 << 20);
    assert_eq!(each(i64::MAX), usize::try_from(upper * (1 << 20) / 2).unwrap());
    let sizes = CacheSizes::from_max_sig_cache_size(Some(3));
    assert_eq!(sizes.signature_cache_bytes, sizes.script_execution_cache_bytes);
    assert_eq!(sizes.signature_cache_bytes, 3 << 19);
}

#[test]
fn oversized_caches_are_refused_where_core_refuses_them() {
    // Core's setup_bytes refuses more than u32::MAX elements of 32 bytes, before allocating.
    let refused = (u32::MAX as usize + 1) * CORE_ELEMENT_BYTES;
    assert_eq!(
        SigCache::with_nonce(refused, NONCE).unwrap_err(),
        CacheError::TooLarge {
            bytes: refused
        }
    );
    let huge = CacheSizes::from_max_sig_cache_size(Some(i64::MAX));
    assert!(matches!(
        ScriptExecCache::with_nonce(huge.script_execution_cache_bytes, NONCE),
        Err(CacheError::TooLarge { .. })
    ));
}

#[test]
fn capacity_matches_cores_at_the_same_budget() {
    let default = SigCache::with_nonce(DEFAULT_MAX_SIG_CACHE_BYTES / 2, NONCE).unwrap();
    let core = DEFAULT_MAX_SIG_CACHE_BYTES / 2 / CORE_ELEMENT_BYTES;
    assert_eq!(default.cache().shards.len(), MAX_SHARDS);
    // 8 entries of 32 bytes and 3 bytes of state per bucket, less the rounding to whole shards.
    assert!(default.cache().capacity() * 100 >= core * 98, "{}", default.cache().capacity());
    assert!(default.cache().table_bytes() <= DEFAULT_MAX_SIG_CACHE_BYTES / 2);
    // Core's floor is 2 elements; ours is one bucket.
    let empty = SigCache::with_nonce(0, NONCE).unwrap();
    assert_eq!(empty.cache().capacity(), WAYS);
    let small = SigCache::with_nonce(512 << 10, NONCE).unwrap();
    // A small table loses up to 63 buckets to the rounding to 64 shards.
    assert!(small.cache().capacity() * 100 >= (512 << 10) / CORE_ELEMENT_BYTES * 96);
}

#[test]
fn entries_are_cores_salted_hashes() {
    let sig = SigCache::with_nonce(1 << 16, NONCE).unwrap();
    let (sighash, pubkey, der) = ([7; 32], [2; 33], [0x30; 70]);
    assert_eq!(sig.entry(&sighash, &pubkey, &der), reference_entry(&[&sighash, &pubkey, &der]));
    let exec = ScriptExecCache::with_nonce(1 << 16, NONCE).unwrap();
    let txid = [9; 32];
    let flags = ScriptFlags::from_bits(0x0001_4dcf);
    assert_eq!(exec.entry(&txid, flags), reference_entry(&[&txid, &0x0001_4dcfu32.to_le_bytes()]));
    // Another salt gives other entries.
    let other = ScriptExecCache::with_nonce(1 << 16, [0x5b; 32]).unwrap();
    assert_ne!(other.entry(&txid, flags), exec.entry(&txid, flags));
}

/// One bucket: every entry lands in it.
fn one_bucket() -> SaltedCache {
    let cache = SaltedCache::with_bytes(0, NONCE).unwrap();
    assert_eq!((cache.shards.len(), cache.buckets_per_shard), (1, 1));
    cache
}

#[test]
fn erase_is_soft_as_in_cuckoocache() {
    let cache = one_bucket();
    let x = e(42);
    assert!(!cache.contains(x, false));
    cache.insert(x);
    assert!(cache.contains(x, true));
    // `allow_erase` only frees the slot: the entry stays visible until overwritten.
    assert!(cache.contains(x, false));
    assert!(cache.contains(x, true));
    // Filling the free ways leaves it.
    for k in 100..107 {
        cache.insert(e(k));
    }
    assert!(cache.contains(x, false));
    // The next insert takes the erasable way.
    cache.insert(e(200));
    assert!(!cache.contains(x, false));
    assert!((100..107).all(|k| cache.contains(e(k), false)));
    assert!(cache.contains(e(200), false));
}

#[test]
fn inserting_again_keeps_an_erasable_entry() {
    let cache = one_bucket();
    for k in 0..8 {
        cache.insert(e(k));
    }
    assert!(cache.contains(e(3), true));
    // Core's `please_keep`.
    cache.insert(e(3));
    cache.insert(e(99));
    assert!(cache.contains(e(3), false));
    // With nothing erasable the round-robin victim goes: way 0 holds entry 0.
    assert!(!cache.contains(e(0), false));
    assert!(cache.contains(e(99), false));
    cache.insert(e(100));
    assert!(!cache.contains(e(1), false));
}

fn key(seed: u8) -> (SecretKey, [u8; 33]) {
    let secret = SecretKey::from_secret_bytes([seed.max(1); 32]).unwrap();
    (secret, PublicKey::from_secret_key(&secret).serialize())
}

/// The model test (PLAN CS6 "sigcache hits vs twin"): over random sequences of signatures, valid
/// and not, verified with and without `store`, [`CachingVerifier`]'s verdict is always
/// `verify_lax`'s, and the cache holds exactly what Core's would.
#[test]
fn verdicts_never_depend_on_the_cache() {
    let cache = SigCache::with_nonce(1 << 20, NONCE).unwrap();
    let mut state = 0x243f_6a88_85a3_08d3_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let keys: Vec<_> = (1..=4).map(key).collect();
    let mut stored = std::collections::HashSet::new();
    for _ in 0..600 {
        let r = next();
        let (secret, pubkey) = &keys[(r % 4) as usize];
        let digest = [(r >> 8) as u8 % 6; 32];
        let signed = [(r >> 16) as u8 % 6; 32];
        let der = secp256k1::ecdsa::sign(Message::from_digest(signed), secret).serialize_der();
        let pk = PubKeyRef::new(pubkey).unwrap();
        let store = (r >> 24) & 1 == 1;
        let entry = cache.entry(&digest, pubkey, &der);
        let hit = cache.cache().contains(entry, false);
        assert_eq!(hit, stored.contains(&entry), "only verified signatures are ever stored");
        let ours = CachingVerifier::new(&cache, store).verify(&der, pk, &digest);
        assert_eq!(ours, verify_lax(pk, &digest, &der));
        if ours && store {
            stored.insert(entry);
        }
    }
    assert!(stored.len() > 10, "the sequence stored {} signatures", stored.len());
}

#[test]
fn a_hit_without_store_soft_erases() {
    let cache = SigCache::with_nonce(1 << 16, NONCE).unwrap();
    let (secret, pubkey) = key(9);
    let der = secp256k1::ecdsa::sign(Message::from_digest([1; 32]), &secret).serialize_der();
    let pk = PubKeyRef::new(&pubkey).unwrap();
    // Block connection (no store) verifies and stores nothing.
    assert!(CachingVerifier::new(&cache, false).verify(&der, pk, &[1; 32]));
    let entry = cache.entry(&[1; 32], &pubkey, &der);
    assert!(!cache.cache().contains(entry, false));
    // The mempool stores it; the block then hits and marks it erasable.
    assert!(CachingVerifier::new(&cache, true).verify(&der, pk, &[1; 32]));
    assert!(cache.cache().contains(entry, false));
    assert!(CachingVerifier::new(&cache, false).verify(&der, pk, &[1; 32]));
    let erasable = cache.cache().bucket(entry, |b| b.erasable);
    assert_eq!(erasable.count_ones(), 1);
    // A failing signature is never stored, whatever `store` says.
    assert!(!CachingVerifier::new(&cache, true).verify(&der, pk, &[2; 32]));
    assert!(!cache.cache().contains(cache.entry(&[2; 32], &pubkey, &der), false));
}

#[test]
fn concurrent_inserts_never_create_hits() {
    let cache = SigCache::with_nonce(1 << 14, NONCE).unwrap();
    std::thread::scope(|s| {
        for t in 0..4u8 {
            let cache = &cache;
            s.spawn(move || {
                for i in 0..4_000u32 {
                    let inserted = cache.entry(&[t; 32], &i.to_le_bytes(), b"in");
                    cache.cache().insert(inserted);
                    let never = cache.entry(&[t; 32], &i.to_le_bytes(), b"out");
                    assert!(!cache.cache().contains(never, i % 2 == 0));
                }
            });
        }
    });
}
