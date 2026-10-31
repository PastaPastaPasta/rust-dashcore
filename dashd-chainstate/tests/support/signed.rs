//! Signed test transactions (CS6): a credit transaction with P2PKH outputs to harness keys, and a
//! transaction spending them whose inputs are each signed, signed over the wrong digest, or given
//! a raw scriptSig. Signatures are libsecp256k1's (low-S DER) over CS1's legacy signature hash.

#![allow(dead_code)]

use dash_consensus::hash::hash160;
use dash_consensus::primitives::{OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxType};
use dash_consensus::script::{SIGHASH_ALL, push_data, signature_hash};
use secp256k1::{Message, PublicKey, SecretKey};

/// How one input of [`spend`] is unlocked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unlock {
    /// A valid signature by the output's key.
    Signed,
    /// The key's signature over another digest: CHECKSIG returns false.
    WrongDigest,
    /// This scriptSig, as is.
    Raw(Vec<u8>),
}

pub fn secret(i: usize) -> SecretKey {
    let mut bytes = [0x11; 32];
    bytes[31] = u8::try_from(i % 200).unwrap() + 1;
    SecretKey::from_secret_bytes(bytes).unwrap()
}

pub fn pubkey(i: usize) -> [u8; 33] {
    PublicKey::from_secret_key(&secret(i)).serialize()
}

/// `OP_DUP OP_HASH160 <hash160(pubkey)> OP_EQUALVERIFY OP_CHECKSIG`.
pub fn p2pkh(pubkey: &[u8]) -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 20];
    script.extend_from_slice(&hash160(pubkey));
    script.extend_from_slice(&[0x88, 0xac]);
    script
}

fn tx(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Tx {
    Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs,
        outputs,
        lock_time: 0,
        extra_payload: Vec::new(),
    })
}

/// A transaction with one output per script, spending a made-up outpoint named by `tag`.
pub fn credit(scripts: &[Vec<u8>], tag: u32) -> Tx {
    let input = TxIn {
        prevout: OutPoint {
            hash: TxHash::from([0xc5; 32]),
            index: tag,
        },
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    };
    let outputs = scripts
        .iter()
        .map(|s| TxOut {
            value: 10_000,
            script_pubkey: s.clone().into(),
        })
        .collect();
    tx(vec![input], outputs)
}

/// A signed spend: the credit transaction, the spend and the scripts it spends, in input order.
pub struct Spend {
    pub credit: Tx,
    pub tx: Tx,
    pub spent: Vec<Vec<u8>>,
}

/// `key_of(i)`'s signature over `digest`, DER with the SIGHASH_ALL byte appended.
pub fn sign_all(digest: [u8; 32], i: usize) -> Vec<u8> {
    let mut sig =
        secp256k1::ecdsa::sign(Message::from_digest(digest), &secret(i)).serialize_der().to_vec();
    sig.push(SIGHASH_ALL as u8);
    sig
}

/// A spend of `unlocks.len()` P2PKH outputs, output `i` to key `i % 4`.
pub fn spend(unlocks: &[Unlock], tag: u32) -> Spend {
    let spent = (0..unlocks.len()).map(|i| p2pkh(&pubkey(i % 4))).collect();
    spend_scripts(spent, unlocks, tag)
}

/// A spend of the outputs with scripts `spent`, input `i` unlocked per `unlocks[i]` and signed
/// with key `i % 4`.
pub fn spend_scripts(spent: Vec<Vec<u8>>, unlocks: &[Unlock], tag: u32) -> Spend {
    let credit = credit(&spent, tag);
    let mut raw = Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: (0..unlocks.len())
            .map(|i| TxIn {
                prevout: OutPoint {
                    hash: credit.txid(),
                    index: u32::try_from(i).unwrap(),
                },
                script_sig: Vec::new().into(),
                sequence: u32::MAX,
            })
            .collect(),
        outputs: vec![TxOut {
            value: 1_000,
            script_pubkey: vec![0x51].into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    };
    // Every input's signature hash blanks the other scriptSigs, so sign before filling any.
    let script_sigs: Vec<Vec<u8>> = unlocks
        .iter()
        .enumerate()
        .map(|(i, unlock)| {
            if let Unlock::Raw(script_sig) = unlock {
                return script_sig.clone();
            }
            let mut digest = signature_hash(&spent[i], &raw, i, SIGHASH_ALL);
            if matches!(unlock, Unlock::WrongDigest) {
                digest[0] ^= 1;
            }
            let mut script_sig = Vec::new();
            push_data(&mut script_sig, &sign_all(digest, i % 4));
            push_data(&mut script_sig, &pubkey(i % 4));
            script_sig
        })
        .collect();
    for (input, script_sig) in raw.inputs.iter_mut().zip(script_sigs) {
        input.script_sig = script_sig.into();
    }
    Spend {
        credit,
        tx: Tx::new(raw),
        spent,
    }
}
