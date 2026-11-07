//! Unit checks of the signing core: Core's `script_combineSigs` with fixed keys, the extraction
//! and padding rules, the providers and the error texts.

use bitcoin_hashes::sha256d;
use dash_consensus::primitives::{TxHash, TxIn, TxOut, TxType};
use dash_consensus::script::opcodes::{OP_CHECKMULTISIG, OP_CHECKSIG, OP_EQUAL};
use dash_consensus::script::{SIGHASH_ALL, SIGHASH_NONE};

use super::*;
use crate::core_write::solver;
use crate::key_io::{Destination, SecretBytes, decode_secret};

fn key(n: u8, compressed: bool) -> Key {
    Key {
        secret: SecretBytes::new(&[n; 32]).expect("a valid secret"),
        compressed,
    }
}

fn push(data: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    push_data(&mut s, data);
    s
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// `GetScriptForMultisig(required, keys)`.
fn multisig(required: u8, keys: &[Vec<u8>]) -> Vec<u8> {
    let mut s = vec![OP_1 + required - 1];
    for k in keys {
        push_data(&mut s, k);
    }
    s.push(OP_1 + keys.len() as u8 - 1);
    s.push(OP_CHECKMULTISIG);
    s
}

/// `BuildSpendingTransaction(scriptSig, txCredit)`: one input spending output 0, one output of
/// the credit's value with an empty script.
// Core: test/util/transaction_utils.cpp:26-41
fn spending(script_sig: &[u8], outputs: usize) -> Transaction {
    Transaction::new(
        1,
        TxType::Spend,
        vec![TxIn::new(
            OutPoint::new(TxHash::from([7; 32]), 0),
            script_sig.to_vec().into(),
            u32::MAX,
        )],
        (0..outputs).map(|_| TxOut::from_duffs(0, Vec::new().into())).collect(),
        0,
        Vec::new(),
    )
}

fn with_script_sig(tx: &Transaction, script_sig: &[u8]) -> Transaction {
    let mut tx = tx.clone();
    {
        let mut inputs = tx.inputs().clone();
        inputs[0] = dash_primitives::TxIn::new(
            inputs[0].prevout(),
            script_sig.to_vec().into(),
            inputs[0].sequence(),
        );
        tx = dash_primitives::Transaction::new(
            tx.version(),
            tx.tx_type(),
            inputs,
            tx.outputs().clone(),
            tx.lock_time(),
            tx.extra_payload().clone(),
        );
    }
    tx
}

/// `SignSignature(provider, fromPubKey, txTo, 0, hash_type)`.
fn sign_signature(
    provider: &dyn SigningProvider,
    script_pubkey: &[u8],
    tx: &mut Transaction,
    hash_type: i32,
) -> bool {
    let mut sigdata = SignatureData::default();
    let creator = Creator {
        tx,
        n_in: 0,
        hash_type,
    };
    let ok = produce_signature(provider, &creator, script_pubkey, &mut sigdata);
    update_input(tx, 0, &sigdata);
    ok
}

/// script_tests.cpp's `CombineSignatures`: both merged, then produced with no keys.
// Core: test/script_tests.cpp:1090-1098
fn combine(
    script_pubkey: &[u8],
    tx: &Transaction,
    a: &SignatureData,
    b: &SignatureData,
) -> SignatureData {
    let mut data = SignatureData::default();
    data.merge(a.clone());
    data.merge(b.clone());
    let creator = Creator {
        tx,
        n_in: 0,
        hash_type: SIGHASH_ALL,
    };
    produce_signature(&DummySigningProvider, &creator, script_pubkey, &mut data);
    data
}

fn raw_sig(k: &Key, script_code: &[u8], tx: &Transaction, hash_type: i32) -> Vec<u8> {
    let mut sig = sign(k, &signature_hash(script_code, tx, 0, hash_type));
    sig.push(hash_type as u8);
    sig
}

fn sigs_of(pairs: &[(&Key, &[u8])]) -> SignatureData {
    let mut data = SignatureData::default();
    for (k, sig) in pairs {
        data.signatures.insert(hash160(&k.pubkey()), (k.pubkey(), sig.to_vec()));
    }
    data
}

fn ids(data: &SignatureData) -> Vec<KeyId> {
    data.signatures.keys().copied().collect()
}

/// Core's `script_combineSigs` with fixed keys (Core draws random ones: key `i` compressed for
/// odd `i`), every expected scriptSig spelled out byte for byte.
// Core: test/script_tests.cpp:1100-1211
#[test]
fn combine_sigs_follows_core_script_tests() {
    let keys: Vec<Key> = (0..3).map(|i| key(0x11 * (i + 1), i % 2 == 1)).collect();
    let pubkeys: Vec<Vec<u8>> = keys.iter().map(Key::pubkey).collect();
    assert_eq!((pubkeys[0].len(), pubkeys[1].len(), pubkeys[2].len()), (65, 33, 65));
    let mut keystore = FillableSigningProvider::default();
    for i in 0..3 {
        keystore.add_key(key(0x11 * (i + 1), i % 2 == 1));
    }
    let mut tx_to = spending(&[], 1);
    let empty = SignatureData::default();

    // P2PKH.
    let spk = Destination::PkHash(hash160(&pubkeys[0])).script();
    assert!(combine(&spk, &tx_to, &empty, &empty).script_sig.is_empty());
    assert!(sign_signature(&keystore, &spk, &mut tx_to, SIGHASH_ALL));
    let sig = raw_sig(&keys[0], &spk, &tx_to, SIGHASH_ALL);
    let script_sig = data_from_transaction(&tx_to, 0, &spk);
    assert!(script_sig.complete);
    assert_eq!(script_sig.script_sig, concat(&[&push(&sig), &push(&pubkeys[0])]));
    assert_eq!(combine(&spk, &tx_to, &script_sig, &empty).script_sig, script_sig.script_sig);
    assert_eq!(combine(&spk, &tx_to, &empty, &script_sig).script_sig, script_sig.script_sig);
    // Signing again: RFC 6979 gives the same signature, which either way combines.
    let copy = script_sig.clone();
    assert!(sign_signature(&keystore, &spk, &mut tx_to, SIGHASH_ALL));
    let script_sig = data_from_transaction(&tx_to, 0, &spk);
    assert_eq!(script_sig.script_sig, copy.script_sig);
    assert_eq!(combine(&spk, &tx_to, &copy, &script_sig).script_sig, copy.script_sig);

    // P2SH, single signature.
    let pk_single = concat(&[&push(&pubkeys[0]), &[OP_CHECKSIG]]);
    assert!(keystore.add_cscript(&pk_single));
    let spk = Destination::ScriptHash(hash160(&pk_single)).script();
    assert!(sign_signature(&keystore, &spk, &mut tx_to, SIGHASH_ALL));
    let sig = raw_sig(&keys[0], &pk_single, &tx_to, SIGHASH_ALL);
    let script_sig = data_from_transaction(&tx_to, 0, &spk);
    assert_eq!(script_sig.script_sig, concat(&[&push(&sig), &push(&pk_single)]));
    assert_eq!(script_sig.redeem_script, Vec::<u8>::new(), "complete data keeps no redeem script");
    assert_eq!(combine(&spk, &tx_to, &script_sig, &empty).script_sig, script_sig.script_sig);
    assert_eq!(combine(&spk, &tx_to, &empty, &script_sig).script_sig, script_sig.script_sig);
    let copy = script_sig.clone();
    assert!(sign_signature(&keystore, &spk, &mut tx_to, SIGHASH_ALL));
    let script_sig = data_from_transaction(&tx_to, 0, &spk);
    assert_eq!(combine(&spk, &tx_to, &copy, &script_sig).script_sig, copy.script_sig);

    // Bare 2-of-3: the first two keys in script order sign.
    let spk = multisig(2, &pubkeys);
    assert!(keystore.add_cscript(&spk));
    assert!(sign_signature(&keystore, &spk, &mut tx_to, SIGHASH_ALL));
    let s0 = raw_sig(&keys[0], &spk, &tx_to, SIGHASH_ALL);
    let s1 = raw_sig(&keys[1], &spk, &tx_to, SIGHASH_ALL);
    let script_sig = data_from_transaction(&tx_to, 0, &spk);
    assert_eq!(script_sig.script_sig, concat(&[&[OP_0], &push(&s0), &push(&s1)]));
    assert_eq!(combine(&spk, &tx_to, &script_sig, &empty).script_sig, script_sig.script_sig);
    assert_eq!(combine(&spk, &tx_to, &empty, &script_sig).script_sig, script_sig.script_sig);

    // Partial signatures of three hash types.
    let sig1 = raw_sig(&keys[0], &spk, &tx_to, SIGHASH_ALL);
    let sig2 = raw_sig(&keys[1], &spk, &tx_to, SIGHASH_NONE);
    let sig3 = raw_sig(&keys[2], &spk, &tx_to, SIGHASH_SINGLE);
    let partial1a = concat(&[&[OP_0], &push(&sig1), &[OP_0]]);
    let partial3c = concat(&[&[OP_0], &push(&sig3), &[OP_0]]);
    let complete12 = concat(&[&[OP_0], &push(&sig1), &push(&sig2)]);
    let complete13 = concat(&[&[OP_0], &push(&sig1), &push(&sig3)]);
    let complete23 = concat(&[&[OP_0], &push(&sig2), &push(&sig3)]);
    let p1 = sigs_of(&[(&keys[0], &sig1)]);
    let p2 = sigs_of(&[(&keys[1], &sig2)]);
    let p3 = sigs_of(&[(&keys[2], &sig3)]);
    let combined = |a: &SignatureData, b: &SignatureData| combine(&spk, &tx_to, a, b).script_sig;
    assert_eq!(combined(&p1, &p1), partial1a);
    assert_eq!(combined(&p1, &p2), complete12);
    assert_eq!(combined(&p2, &p1), complete12);
    assert_eq!(combined(&p3, &p1), complete13);
    assert_eq!(combined(&p2, &p3), complete23);
    assert_eq!(combined(&p3, &p2), complete23);
    assert_eq!(combined(&p3, &p3), partial3c);
}

/// script_tests.cpp's raw partial scriptSigs, extracted rather than given as maps: placeholders
/// missing, extra or in any order, each signature found under its key (R2-M2).
// Core: test/script_tests.cpp:1180-1189; script/sign.cpp:257-307
#[test]
fn extraction_finds_signatures_among_any_placeholders() {
    let keys: Vec<Key> = (1..=3).map(|i| key(i, true)).collect();
    let pubkeys: Vec<Vec<u8>> = keys.iter().map(Key::pubkey).collect();
    let spk = multisig(2, &pubkeys);
    let tx = spending(&[], 1);
    let sig1 = raw_sig(&keys[0], &spk, &tx, SIGHASH_ALL);
    let sig2 = raw_sig(&keys[1], &spk, &tx, SIGHASH_NONE);
    let sig3 = raw_sig(&keys[2], &spk, &tx, SIGHASH_SINGLE);
    let id = |i: usize| hash160(&pubkeys[i]);
    let cases: [(Vec<u8>, Vec<KeyId>); 7] = [
        (concat(&[&[OP_0], &push(&sig1), &[OP_0]]), vec![id(0)]),
        (concat(&[&[OP_0, OP_0], &push(&sig1)]), vec![id(0)]),
        (concat(&[&[OP_0], &push(&sig2)]), vec![id(1)]),
        (concat(&[&push(&sig2), &[OP_0]]), vec![id(1)]),
        (push(&sig3), vec![id(2)]),
        (concat(&[&[OP_0, OP_0], &push(&sig3)]), vec![id(2)]),
        (concat(&[&[OP_0], &push(&sig3), &[OP_0]]), vec![id(2)]),
    ];
    for (script_sig, want) in cases {
        let data = data_from_transaction(&with_script_sig(&tx, &script_sig), 0, &spk);
        assert!(!data.complete);
        assert_eq!(ids(&data), want, "{}", hex(&script_sig));
        assert_eq!(data.script_sig, script_sig);
    }
    // Both signatures in either order: in order completes, reversed fails verification and
    // the scan finds only the first key's after the second's.
    let ordered = concat(&[&[OP_0], &push(&sig1), &push(&sig2)]);
    let data = data_from_transaction(&with_script_sig(&tx, &ordered), 0, &spk);
    assert!(data.complete);
    assert_eq!(ids(&data).len(), 2);
    let reversed = concat(&[&[OP_0], &push(&sig2), &push(&sig1)]);
    let data = data_from_transaction(&with_script_sig(&tx, &reversed), 0, &spk);
    assert!(!data.complete);
    assert_eq!(ids(&data), vec![id(1)], "sig2 matches K2; sig1 finds no key after it");
}

fn hex(bytes: &[u8]) -> String {
    crate::descriptor::hex(bytes)
}

/// Sol r1 M3: a 2-of-3 stack `[sig3, empty, empty, sig2]`. The failed verification records
/// K2's signature; the scan then gives sig3 to K2 without checking it (a key already holding a
/// signature takes the item), so K3's signature is never found and the result has K2's alone.
// Core: script/sign.cpp:289-303; script/interpreter.cpp:1132-1167
#[test]
fn a_recorded_key_takes_the_stack_item_unchecked() {
    let keys: Vec<Key> = (1..=3).map(|i| key(i, true)).collect();
    let pubkeys: Vec<Vec<u8>> = keys.iter().map(Key::pubkey).collect();
    let spk = multisig(2, &pubkeys);
    let tx = spending(&[], 1);
    let sig2 = raw_sig(&keys[1], &spk, &tx, SIGHASH_ALL);
    let sig3 = raw_sig(&keys[2], &spk, &tx, SIGHASH_ALL);
    let stack = concat(&[&push(&sig3), &[OP_0, OP_0], &push(&sig2)]);
    let tx = with_script_sig(&tx, &stack);
    let data = data_from_transaction(&tx, 0, &spk);
    assert!(!data.complete);
    assert_eq!(ids(&data), vec![hash160(&pubkeys[1])]);
    assert_eq!(data.signatures[&hash160(&pubkeys[1])].1, sig2);
    // Combined alone: dummy, K2's signature, one empty.
    let combined = combine(&spk, &tx, &data, &SignatureData::default());
    assert!(!combined.complete);
    assert_eq!(combined.script_sig, concat(&[&[OP_0], &push(&sig2), &[OP_0]]));
}

/// The scriptSig's stack is its evaluation as far as it got: a redeem script pushed before a
/// failing opcode is still taken from the stack.
// Core: script/sign.cpp:244-252, 274-283
#[test]
fn extraction_keeps_the_stack_of_a_failed_evaluation() {
    let redeem = multisig(1, &[key(1, true).pubkey()]);
    let spk = Destination::ScriptHash(hash160(&redeem)).script();
    let script_sig = concat(&[&push(&redeem), &[OP_RETURN]]);
    let data = data_from_transaction(&spending(&script_sig, 1), 0, &spk);
    assert!(!data.complete);
    assert_eq!(data.redeem_script, redeem);
    // A number too long to decode fails the op before it pops (Core's scriptnum_error), so the
    // number is still the last item and is taken as the redeem script.
    let number = [1, 2, 3, 4, 5];
    let script_sig = concat(&[&push(&redeem), &push(&number), &[0x8b]]);
    let data = data_from_transaction(&spending(&script_sig, 1), 0, &spk);
    assert_eq!(data.redeem_script, number);
    // An empty last item is no redeem script.
    let data = data_from_transaction(&spending(&[OP_0], 1), 0, &spk);
    assert!(data.redeem_script.is_empty());
}

/// R2-M2: an incomplete scriptSig is rebuilt from what signing finds, so an unsolvable
/// (nonstandard) one becomes empty; a complete one is left as it is.
// Core: script/sign.cpp:193-222
#[test]
fn an_incomplete_nonstandard_script_sig_is_rebuilt_empty() {
    let spk = [OP_1, OP_EQUAL];
    let tx = spending(&push(&[2]), 1);
    let mut data = data_from_transaction(&tx, 0, &spk);
    assert!(!data.complete);
    let creator = Creator {
        tx: &tx,
        n_in: 0,
        hash_type: SIGHASH_ALL,
    };
    assert!(!produce_signature(&DummySigningProvider, &creator, &spk, &mut data));
    assert!(data.script_sig.is_empty());

    let tx = spending(&[OP_1], 1);
    let mut data = data_from_transaction(&tx, 0, &spk);
    assert!(data.complete);
    assert!(produce_signature(&DummySigningProvider, &creator, &spk, &mut data));
    assert_eq!(data.script_sig, [OP_1]);
}

/// `MergeSignatureData`: the first complete side wins, a complete later side replaces partial
/// data, the first non-empty redeem script stays, a held signature is never replaced.
// Core: script/sign.cpp:314-325
#[test]
fn merge_follows_core() {
    let complete = |s: u8| SignatureData {
        complete: true,
        script_sig: vec![s],
        ..SignatureData::default()
    };
    let mut d = complete(1);
    d.merge(complete(2));
    assert_eq!(d.script_sig, [1]);

    let mut d = sigs_of(&[(&key(1, true), &[1])]);
    d.merge(complete(2));
    assert_eq!(d, complete(2));

    let mut d = SignatureData::default();
    d.merge(SignatureData {
        redeem_script: vec![0xaa],
        ..SignatureData::default()
    });
    d.merge(SignatureData {
        redeem_script: vec![0xbb],
        ..SignatureData::default()
    });
    assert_eq!(d.redeem_script, [0xaa]);

    let k = key(1, true);
    let mut d = sigs_of(&[(&k, &[1])]);
    let mut other = sigs_of(&[(&k, &[2])]);
    other.signatures.insert([9; 20], (vec![9], vec![9]));
    d.merge(other);
    assert_eq!(d.signatures[&hash160(&k.pubkey())].1, [1]);
    assert_eq!(d.signatures.len(), 2);
}

/// Sol r1 m1: Core's padding loop, whose bound reads `ret` as it grows, at thresholds two and
/// three with zero, one and two signatures available.
// Core: script/sign.cpp:152-170
#[test]
fn multisig_padding_follows_the_literal_loop() {
    let tx = spending(&[], 1);
    // The empties after the dummy and the signatures: Sol r2's `00 00`, `00 S1 00`, `00 S1 S2`
    // and `00 00 00`, `00 S1 00`, `00 S1 S2 00`, not a fill to the threshold.
    for (required, n, want_empties) in [(2, 3, [1, 1, 0]), (3, 5, [2, 1, 1])] {
        let keys: Vec<Key> = (1..=n).map(|i| key(i, true)).collect();
        let pubkeys: Vec<Vec<u8>> = keys.iter().map(Key::pubkey).collect();
        let spk = multisig(required, &pubkeys);
        for (signers, empties) in want_empties.into_iter().enumerate() {
            let mut keystore = FillableSigningProvider::default();
            for i in 1..=signers as u8 {
                keystore.add_key(key(i, true));
            }
            let mut data = SignatureData::default();
            let creator = Creator {
                tx: &tx,
                n_in: 0,
                hash_type: SIGHASH_ALL,
            };
            let complete = produce_signature(&keystore, &creator, &spk, &mut data);
            assert_eq!(complete, signers == usize::from(required));
            let mut want = vec![OP_0];
            for k in &keys[..signers] {
                want.extend(push(&raw_sig(k, &spk, &tx, SIGHASH_ALL)));
            }
            want.extend(std::iter::repeat_n(OP_0, empties));
            assert_eq!(hex(&data.script_sig), hex(&want), "{required}-of-{n}, {signers} signers");
            assert_eq!(data.missing_sigs.len(), usize::from(n) - signers);
        }
    }
    // The literal bytes Sol r2 gave for no signature at all.
    let keys: Vec<Vec<u8>> = (1..=3).map(|i| key(i, true).pubkey()).collect();
    let mut data = SignatureData::default();
    let creator = Creator {
        tx: &tx,
        n_in: 0,
        hash_type: SIGHASH_ALL,
    };
    produce_signature(&DummySigningProvider, &creator, &multisig(2, &keys), &mut data);
    assert_eq!(data.script_sig, [0x00, 0x00]);
}

/// Sol r1 M4: `AddCScript` stores 520 bytes and refuses 521.
// Core: script/signingprovider.cpp:111-120; script/script.h:24
#[test]
fn add_cscript_refuses_over_520_bytes() {
    let mut keystore = FillableSigningProvider::default();
    let at_limit = vec![OP_1; 520];
    let over = vec![OP_1; 521];
    assert!(keystore.add_cscript(&at_limit));
    assert!(!keystore.add_cscript(&over));
    assert_eq!(keystore.get_cscript(&hash160(&at_limit)), Some(at_limit.as_slice()));
    assert_eq!(keystore.get_cscript(&hash160(&over)), None);

    // A 1-of-16 P2SH redeem script of compressed keys is 547 bytes: unstored, so the input
    // stays unsigned even with its key.
    let pubkeys: Vec<Vec<u8>> = (1..=16).map(|i| key(i, true).pubkey()).collect();
    let mut redeem = vec![OP_1];
    for k in &pubkeys {
        push_data(&mut redeem, k);
    }
    redeem.extend([OP_1 + 15, OP_CHECKMULTISIG]);
    assert_eq!(redeem.len(), 547);
    assert!(!keystore.add_cscript(&redeem));
    keystore.add_key(key(1, true));
    let spk = Destination::ScriptHash(hash160(&redeem)).script();
    let mut tx = spending(&[], 1);
    assert!(!sign_signature(&keystore, &spk, &mut tx, SIGHASH_ALL));
    assert!(tx.inputs()[0].script_sig().as_bytes().is_empty());
}

/// `FillableSigningProvider` keys a key by its compression: an uncompressed WIF signs only the
/// uncompressed key's P2PKH, and a key's public key comes from the key itself.
// Core: script/signingprovider.cpp:66-81
#[test]
fn keys_are_held_by_their_compression() {
    let mut keystore = FillableSigningProvider::default();
    keystore.add_key(key(5, false));
    let uncompressed = key(5, false).pubkey();
    let compressed = key(5, true).pubkey();
    assert_eq!(
        keystore.get_key(&hash160(&uncompressed)).map(Key::pubkey),
        Some(uncompressed.clone())
    );
    assert_eq!(keystore.get_key(&hash160(&compressed)).map(Key::pubkey), None);
    let mut tx = spending(&[], 1);
    let spk = Destination::PkHash(hash160(&compressed)).script();
    assert!(!sign_signature(&keystore, &spk, &mut tx, SIGHASH_ALL));
    let spk = Destination::PkHash(hash160(&uncompressed)).script();
    assert!(sign_signature(&keystore, &spk, &mut tx, SIGHASH_ALL));
}

/// `CKey::Sign`'s deterministic vectors: RFC 6979 with the low-R grind, the same for a key
/// whichever its compression.
// Core: test/key_tests.cpp:21-24, 134-146
#[test]
fn signatures_match_core_key_tests_vectors() {
    let p = crate::descriptor::tests::mainnet();
    let hash = sha256d::Hash::hash(b"Very deterministic message").to_byte_array();
    for (secrets, want) in [
        (
            [
                "7qh6LYnLN2w2ntz2wwUhRUEgkQ2j8XB16FGw77ZRDZmC29bn7cD",
                "XBuxZHH6TqXUuaSjbVTFR1DQSYecxCB9QA1Koyx5tTc3ddhqEnhm",
            ],
            "304402205dbbddda71772d95ce91cd2d14b592cfbc1dd0aabd6a394b6c2d377bbe59d31d022014ddda21494a4e221f0824f0b8b924c43fa43c0ad57dccdaa11f81a6bd4582f6",
        ),
        (
            [
                "7rve4MxeWFQHGbSYH6J2yaaZd3MBUqoDEwN6ZAZ6ZHmhTT4r3hW",
                "XHMkZqWcY6Zkoq1j42NBijD8z5N5FtNy2Wx7WyAfXX2HZgxry8cr",
            ],
            "3044022052d8a32079c11e79db95af63bb9600c5b04f21a9ca33dc129c2bfa8ac9dc1cd5022061d8ae5e0f6c1a16bde3719c64c2fd70e404b6428ab9a69566962e8771b5944d",
        ),
    ] {
        for secret in secrets {
            let k = decode_secret(secret, &p).expect("Core's test key");
            assert_eq!(hex(&sign(&k, &hash)), want, "{secret}");
        }
    }
    // Low R: every one of 256 signatures has R of at most 32 bytes (key_signature_tests).
    let k = decode_secret("7qh6LYnLN2w2ntz2wwUhRUEgkQ2j8XB16FGw77ZRDZmC29bn7cD", &p).unwrap();
    for i in 0..256 {
        let hash = sha256d::Hash::hash(format!("A message to be signed{i}").as_bytes());
        let sig = sign(&k, &hash.to_byte_array());
        assert!(sig[3] <= 32 && sig.len() <= 70, "{i}");
    }
}

/// The SINGLE guard: no signature for an input without an output at its index, the key
/// recorded as missing; ALL signs the same input.
// Core: script/sign.cpp:41-43
#[test]
fn single_without_its_output_signs_nothing() {
    let k = key(3, true);
    let mut keystore = FillableSigningProvider::default();
    keystore.add_key(key(3, true));
    let spk = Destination::PkHash(hash160(&k.pubkey())).script();
    let tx = spending(&[], 0);
    for hash_type in [SIGHASH_SINGLE, SIGHASH_SINGLE | 0x80] {
        let creator = Creator {
            tx: &tx,
            n_in: 0,
            hash_type,
        };
        assert_eq!(creator.create_sig(&keystore, &hash160(&k.pubkey()), &spk), None);
        let mut data = SignatureData::default();
        assert!(!produce_signature(&keystore, &creator, &spk, &mut data));
        assert_eq!(data.missing_sigs, vec![hash160(&k.pubkey())]);
    }
    let creator = Creator {
        tx: &tx,
        n_in: 0,
        hash_type: SIGHASH_ALL,
    };
    assert!(produce_signature(&keystore, &creator, &spk, &mut SignatureData::default()));
}

/// `solver_solutions` classifies as `core_write::solver` does, with Core's `vSolutions`.
// Core: script/standard.cpp:115-159
#[test]
fn solver_solutions_agree_with_solver() {
    let c = key(1, true).pubkey();
    let u = key(2, false).pubkey();
    let p2sh = Destination::ScriptHash([4; 20]).script();
    let p2pkh = Destination::PkHash([3; 20]).script();
    let p2pk = concat(&[&push(&c), &[OP_CHECKSIG]]);
    let ms = multisig(1, &[c.clone(), u.clone()]);
    let cases: Vec<(Vec<u8>, TxoutType, Vec<Vec<u8>>)> = vec![
        (p2sh, TxoutType::ScriptHash, vec![vec![4; 20]]),
        (vec![OP_RETURN, 1, 7], TxoutType::NullData, vec![]),
        (vec![OP_RETURN], TxoutType::NullData, vec![]),
        (vec![OP_RETURN, OP_CHECKSIG], TxoutType::Nonstandard, vec![]),
        (p2pk, TxoutType::Pubkey, vec![c.clone()]),
        (concat(&[&push(&u), &[OP_CHECKSIG]]), TxoutType::Pubkey, vec![u.clone()]),
        (p2pkh, TxoutType::PubkeyHash, vec![vec![3; 20]]),
        (ms, TxoutType::Multisig, vec![vec![1], c.clone(), u.clone(), vec![2]]),
        (multisig(2, std::slice::from_ref(&c)), TxoutType::Nonstandard, vec![]),
        (concat(&[&push(&[2; 33]), &[OP_CHECKSIG]]), TxoutType::Pubkey, vec![vec![2; 33]]),
        (concat(&[&push(&[5; 33]), &[OP_CHECKSIG]]), TxoutType::Nonstandard, vec![]),
        (vec![], TxoutType::Nonstandard, vec![]),
        (vec![OP_1, OP_EQUAL], TxoutType::Nonstandard, vec![]),
    ];
    for (script, ty, solutions) in cases {
        assert_eq!(solver_solutions(&script), (ty, solutions), "{}", hex(&script));
        assert_eq!(solver(&script), ty, "{}", hex(&script));
    }
}

/// `PushAll`'s minimal pushes.
// Core: script/sign.cpp:176-191
#[test]
fn push_all_uses_minimal_pushes() {
    let items = [vec![], vec![1], vec![16], vec![17], vec![0x81], vec![0], vec![1, 2]];
    assert_eq!(push_all(&items), [0x00, 0x51, 0x60, 0x01, 0x11, 0x4f, 0x01, 0x00, 0x02, 1, 2]);
}

fn coin(script: &[u8]) -> Coin {
    Coin::new(1_000, script, 5, false)
}

/// `SignTransaction`: a missing coin, a missing key, too few multisig signatures, a complete
/// input. Each input is signed over the transaction as signed so far and verified against the
/// transaction as given, which legacy signature hashes make the same.
// Core: script/sign.cpp:406-464
#[test]
fn sign_transaction_reports_core_errors() {
    let k1 = key(1, true);
    let k2 = key(2, true);
    let mut keystore = FillableSigningProvider::default();
    keystore.add_key(key(1, true));
    let p2pkh = Destination::PkHash(hash160(&k1.pubkey())).script();
    let other = Destination::PkHash(hash160(&k2.pubkey())).script();
    let two_of_two = multisig(2, &[k1.pubkey(), k2.pubkey()]);
    let mut tx = spending(&[], 2);
    let input = |n: u8| {
        TxIn::new(
            OutPoint::new(TxHash::from([n; 32]), u32::from(n)),
            Vec::new().into(),
            u32::MAX - u32::from(n),
        )
    };
    tx = dash_primitives::Transaction::new(
        tx.version(),
        tx.tx_type(),
        (1..=5).map(input).collect(),
        tx.outputs().clone(),
        tx.lock_time(),
        tx.extra_payload().clone(),
    );
    let mut coins = BTreeMap::new();
    coins.insert(tx.inputs()[0].prevout(), coin(&p2pkh));
    coins.insert(tx.inputs()[1].prevout(), coin(&other));
    coins.insert(tx.inputs()[2].prevout(), coin(&two_of_two));
    coins.insert(tx.inputs()[4].prevout(), coin(&p2pkh));
    let given = tx.clone();
    let errors = sign_transaction(&mut tx, &keystore, &coins, SIGHASH_ALL);
    assert_eq!(
        errors.into_iter().collect::<Vec<_>>(),
        [
            (1, "Unable to sign input, invalid stack size (possibly missing key)".to_owned()),
            (
                2,
                "CHECK(MULTI)SIG failing with non-zero signature (possibly need more signatures)"
                    .to_owned()
            ),
            (3, "Input not found or already spent".to_owned()),
        ]
    );
    for i in [0, 4] {
        let sig = sign(&k1, &signature_hash(&p2pkh, &given, i, SIGHASH_ALL));
        let want = concat(&[&push(&[sig, vec![1]].concat()), &push(&k1.pubkey())]);
        assert_eq!(tx.inputs()[i].script_sig().as_bytes(), want.as_slice(), "input {i}");
    }
    let s1 = [sign(&k1, &signature_hash(&two_of_two, &given, 2, SIGHASH_ALL)), vec![1]].concat();
    assert_eq!(tx.inputs()[2].script_sig().as_bytes(), concat(&[&[OP_0], &push(&s1), &[OP_0]]));
    assert!(tx.inputs()[1].script_sig().as_bytes().is_empty());
    // Everything else is the transaction as given.
    for (a, b) in tx.inputs().iter().zip(given.inputs()) {
        assert_eq!((a.prevout(), a.sequence()), (b.prevout(), b.sequence()));
    }
    assert_eq!(tx.outputs(), given.outputs());
}

/// `ScriptErrorString` for every error `SignTransaction` can report, against Core's table, and
/// the two texts it substitutes.
// Core: script/script_error.cpp:10-99; script/sign.cpp:449-457
#[test]
fn script_error_texts_match_core() {
    use ScriptError::*;
    let core: [(ScriptError, &str); 40] = [
        (
            EvalFalse,
            "Script evaluated without error but finished with a false/empty top stack element",
        ),
        (Verify, "Script failed an OP_VERIFY operation"),
        (EqualVerify, "Script failed an OP_EQUALVERIFY operation"),
        (CheckMultisigVerify, "Script failed an OP_CHECKMULTISIGVERIFY operation"),
        (CheckSigVerify, "Script failed an OP_CHECKSIGVERIFY operation"),
        (CheckDataSigVerify, "Script failed an OP_CHECKDATASIGVERIFY operation"),
        (NumEqualVerify, "Script failed an OP_NUMEQUALVERIFY operation"),
        (ScriptSize, "Script is too big"),
        (PushSize, "Push value size limit exceeded"),
        (OpCount, "Operation limit exceeded"),
        (StackSize, "Stack size limit exceeded"),
        (SigCount, "Signature count negative or greater than pubkey count"),
        (PubkeyCount, "Pubkey count negative or limit exceeded"),
        (BadOpcode, "Opcode missing or not understood"),
        (DisabledOpcode, "Attempted to use a disabled opcode"),
        (InvalidStackOperation, "Operation not valid with the current stack size"),
        (InvalidAltstackOperation, "Operation not valid with the current altstack size"),
        (OpReturn, "OP_RETURN was encountered"),
        (UnbalancedConditional, "Invalid OP_IF construction"),
        (InvalidSplitRange, "Invalid OP_SPLIT range"),
        (InvalidOperandSize, "Invalid operand size"),
        (InvalidNumberRange, "Given operand is not a number within the valid range [-2^31...2^31]"),
        (ImpossibleEncoding, "The requested encoding is impossible to satisfy"),
        (DivByZero, "Division by zero error"),
        (ModByZero, "Modulo by zero error"),
        (NegativeLocktime, "Negative locktime"),
        (UnsatisfiedLocktime, "Locktime requirement not satisfied"),
        (SigHashtype, "Signature hash type missing or not understood"),
        (SigDer, "Non-canonical DER signature"),
        (MinimalData, "Data push larger than necessary"),
        (SigPushOnly, "Only push operators allowed in signatures"),
        (SigHighS, "Non-canonical signature: S value is unnecessarily high"),
        (SigNullDummy, "Dummy CHECKMULTISIG argument must be zero"),
        (SigNullFail, "Signature must be zero for failed CHECK(MULTI)SIG operation"),
        (DiscourageUpgradableNops, "NOPx reserved for soft-fork upgrades"),
        (PubkeyType, "Public key is neither compressed or uncompressed"),
        (CleanStack, "Stack size must be exactly one after execution"),
        (OpCodeSeparator, "Using OP_CODESEPARATOR"),
        (SigFindAndDelete, "Signature is found in scriptCode"),
        (UnknownError, "unknown error"),
    ];
    let mut seen: Vec<ScriptError> = core.iter().map(|(e, _)| *e).collect();
    seen.sort_by_key(|e| e.code());
    assert_eq!(seen, ScriptError::ALL, "every error once");
    for (e, text) in core {
        assert_eq!(e.message(), text, "{e:?}");
        let want = match e {
            InvalidStackOperation => {
                "Unable to sign input, invalid stack size (possibly missing key)"
            }
            SigNullFail => {
                "CHECK(MULTI)SIG failing with non-zero signature (possibly need more signatures)"
            }
            _ => text,
        };
        assert_eq!(sign_error_text(e), want, "{e:?}");
    }
}
