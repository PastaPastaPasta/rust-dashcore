use super::*;

/// The functional test framework's regtest spork key (`test_framework.py:1514`), whose address
/// is regtest's `strSporkAddress` `yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW` (`chainparams.cpp:904`).
const REGTEST_KEY: [u8; 32] = [
    0x2b, 0xff, 0xcd, 0x23, 0x3d, 0x06, 0x6e, 0xee, 0xcf, 0x77, 0x05, 0xbd, 0x37, 0x95, 0x8d, 0xdf,
    0xb1, 0xc0, 0xe6, 0xf6, 0x09, 0x4f, 0xd0, 0xb5, 0xcd, 0xaf, 0x80, 0x42, 0xb5, 0xcd, 0x3f, 0xf0,
];

fn key() -> SecretKey {
    SecretKey::from_secret_bytes(REGTEST_KEY).unwrap()
}

fn key_id(compressed: bool) -> KeyId {
    key_id_of(&key().public_key(), compressed)
}

fn spork(id: i32, value: i64, time: i64) -> Spork {
    sign(SporkId(id), value, time, &key(), true, SigForm::Message)
}

#[test]
fn signatures_verify_in_their_own_form_only() {
    for form in [SigForm::Hash, SigForm::Message] {
        for compressed in [true, false] {
            let s = sign(SporkId(10001), 0, 1_700_000_000, &key(), compressed, form);
            assert_eq!(s.sig.len(), COMPACT_SIGNATURE_SIZE);
            assert!(check_signature(&s, &key_id(compressed), form));
            assert!(!check_signature(&s, &key_id(!compressed), form));
            let other = if form == SigForm::Hash {
                SigForm::Message
            } else {
                SigForm::Hash
            };
            assert!(!check_signature(&s, &key_id(compressed), other));
        }
    }
}

#[test]
fn the_message_is_three_decimals() {
    let s = Spork {
        spork_id: 10018,
        value: -5,
        time_signed: 1_700_000_000,
        sig: vec![],
    };
    assert_eq!(signed_message(&s), "10018-51700000000");
}

#[test]
fn the_hash_covers_the_signature() {
    let s = spork(10001, 0, 1);
    let mut t = s.clone();
    t.sig[64] ^= 1;
    assert_ne!(spork_hash(&s), spork_hash(&t));
    assert_eq!(signature_hash(&s), signature_hash(&t));
    assert_eq!(core_bytes(&s).len(), 4 + 8 + 8 + 1 + 65);
}

/// `RecoverCompact` masks the header: any byte with the same low three bits of `header - 27`
/// recovers the same key, including bytes below 27.
#[test]
fn header_bytes_alias_as_in_core() {
    let s = spork(10001, 0, 1);
    let h = i32::from(s.sig[0]) - 27;
    for b in 0..=255u8 {
        let mut t = s.clone();
        t.sig[0] = b;
        let same = (i32::from(b) - 27) & 7 == h & 7;
        assert_eq!(check_signature(&t, &key_id(true), SigForm::Message), same, "header {b}");
    }
}

/// `secp256k1_ecdsa_recover` accepts a high S: the negated signature with the other parity
/// recovers the same key.
#[test]
fn high_s_recovers() {
    let s = spork(10001, 0, 1);
    let mut t = s.clone();
    let n = secp256k1::constants::CURVE_ORDER;
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = i16::from(n[i]) - i16::from(s.sig[33 + i]) - borrow;
        t.sig[33 + i] = d.rem_euclid(256) as u8;
        borrow = i16::from(d < 0);
    }
    t.sig[0] = 27 + (((s.sig[0] - 27) & 3) ^ 1) + 4;
    assert!(check_signature(&t, &key_id(true), SigForm::Message));
}

#[test]
fn malformed_signatures_recover_nothing() {
    let s = spork(10001, 0, 1);
    let hash = SigForm::Message.signing_hash(&s);
    assert!(recover_key_id(&hash, &s.sig[..64]).is_none());
    assert!(recover_key_id(&hash, &[s.sig.clone(), vec![0]].concat()).is_none());
    let mut zero = s.sig.clone();
    zero[1..33].fill(0);
    assert!(recover_key_id(&hash, &zero).is_none());
    let mut overflow = s.sig.clone();
    overflow[33..].fill(0xff);
    assert!(recover_key_id(&hash, &overflow).is_none());
}

#[test]
fn validation_checks_the_time_then_the_key() {
    let now = 1_700_000_000;
    let id = key_id(true);
    let ok = spork(10001, 0, now + MAX_TIME_AHEAD);
    assert_eq!(validate(&ok, now, Some(&id), SigForm::Message), Ok(()));
    let late = spork(10001, 0, now + MAX_TIME_AHEAD + 1);
    assert_eq!(
        validate(&late, now, Some(&id), SigForm::Message),
        Err(InvalidSpork::TooFarInFuture)
    );
    assert_eq!(validate(&late, now, None, SigForm::Message), Err(InvalidSpork::TooFarInFuture));
    assert_eq!(validate(&ok, now, None, SigForm::Message), Err(InvalidSpork::InvalidSignature));
    assert_eq!(
        validate(&ok, now, Some(&[0; 20]), SigForm::Message),
        Err(InvalidSpork::InvalidSignature)
    );
}

#[test]
fn values_follow_core() {
    let mut store = SporkStore::new();
    let s19 = SporkId::SPORK_19_CHAINLOCKS_ENABLED;
    assert_eq!(store.value(s19, false), SPORK_OFF);
    assert_eq!(store.value(SporkId(1), false), -1);
    assert_eq!(store.value(s19, true), 0);
    assert_eq!(store.value(SporkId::SPORK_21_QUORUM_ALL_CONNECTED, true), 1);
    assert_eq!(store.value(SporkId(1), true), 0);
    store.process(spork(10018, 5, 1));
    assert_eq!(store.value(s19, false), 5);
    assert_eq!(store.value(s19, true), 0);
    assert!(is_active(5, 6));
    assert!(!is_active(5, 5));
    assert_eq!(id_by_name("SPORK_19_CHAINLOCKS_ENABLED"), Some(s19));
    assert_eq!(id_by_name("SPORK_3_INSTANTSEND_BLOCK_FILTERING"), None);
}

#[test]
fn process_keeps_the_newest_per_id() {
    let mut store = SporkStore::new();
    let a = spork(10001, 0, 10);
    assert_eq!(store.process(a.clone()), Processed::New);
    assert_eq!(store.process(a.clone()), Processed::Seen);
    assert_eq!(store.process(spork(10001, 1, 10)), Processed::Seen);
    assert_eq!(store.process(spork(10001, 1, 9)), Processed::Seen);
    let b = spork(10001, 2, 11);
    assert_eq!(store.process(b.clone()), Processed::Updated);
    assert_eq!(store.active(), std::slice::from_ref(&b));
    // Both stay servable by hash.
    assert_eq!(store.by_hash(&spork_hash(&a)), Some(&a));
    assert_eq!(store.by_hash(&spork_hash(&b)), Some(&b));
    // An unknown id is stored like any other (Core checks no id).
    assert_eq!(store.process(spork(-7, 0, 1)), Processed::New);
    assert_eq!(store.active().len(), 2);
}

#[test]
fn check_and_remove_drops_what_the_key_did_not_sign() {
    let mut store = SporkStore::new();
    store.process(spork(10001, 0, 1));
    let other = SecretKey::from_secret_bytes([7; 32]).unwrap();
    store.process(sign(SporkId(10016), 0, 1, &other, true, SigForm::Message));
    store.check_and_remove(None, SigForm::Message);
    assert_eq!(store.active().len(), 2);
    store.check_and_remove(Some(&key_id(true)), SigForm::Message);
    assert_eq!(store.active().len(), 1);
    assert_eq!(store.all().count(), 1);
}

fn ids(store: &SporkStore) -> Vec<i32> {
    store.active().iter().map(|s| s.spork_id).collect()
}

/// Core's five sporks fall in distinct buckets of 13, so each insert goes to the front.
#[test]
fn the_active_order_is_libstdcxx_s() {
    let mut store = SporkStore::new();
    for id in [10001, 10016, 10018, 10020, 10022] {
        store.process(spork(id, 0, 1));
    }
    assert_eq!(ids(&store), [10022, 10020, 10018, 10016, 10001]);
    // An update keeps the node where it is.
    store.process(spork(10016, 0, 2));
    assert_eq!(ids(&store), [10022, 10020, 10018, 10016, 10001]);
    // 10014 shares 10001's bucket (both 4 mod 13): it goes before it.
    store.process(spork(10014, 0, 1));
    assert_eq!(ids(&store), [10022, 10020, 10018, 10016, 10014, 10001]);
    // A negative id sign-extends: -1 is (2^64 - 1) mod 13 = 2, an empty bucket.
    store.process(spork(-1, 0, 1));
    assert_eq!(ids(&store), [-1, 10022, 10020, 10018, 10016, 10014, 10001]);
}

/// Orders printed by a g++ 15 program inserting the same keys into
/// `std::unordered_map<SporkId, int>` with Core's `std::hash<SporkId>`; the 14th key rehashes to
/// 29 buckets.
#[test]
fn rehash_reorders_as_libstdcxx() {
    let mut store = SporkStore::new();
    for id in 0..14 {
        store.process(spork(id, 0, 1));
    }
    assert_eq!(ids(&store), [13, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    let mut store = SporkStore::new();
    for id in [0, 13, 26, 39, 1, 14, 27, 2, 15, 3, 4, 5, 6, 7] {
        store.process(spork(id, 0, 1));
    }
    assert_eq!(ids(&store), [7, 0, 13, 26, 39, 1, 14, 27, 2, 15, 3, 4, 5, 6]);
}

#[test]
fn restore_reinserts_in_saved_order() {
    let mut store = SporkStore::new();
    for id in [10001, 10016, 10018] {
        store.process(spork(id, 0, 1));
    }
    let saved: Vec<_> = store.active().to_vec();
    let restored = SporkStore::restore(store.all().cloned().collect(), saved);
    // Core writes [18, 16, 1] and reads them back to the front one by one.
    assert_eq!(ids(&restored), [10001, 10016, 10018]);
    assert_eq!(restored.all().count(), 3);
}

/// `UnserializeMap` inserts: of two saved entries with one id, the first stays.
#[test]
fn restore_keeps_the_first_of_an_id() {
    let (a, b) = (spork(10001, 1, 1), spork(10001, 2, 2));
    let restored = SporkStore::restore(vec![a.clone(), b.clone()], vec![a.clone(), b]);
    assert_eq!(restored.active(), [a]);
}
