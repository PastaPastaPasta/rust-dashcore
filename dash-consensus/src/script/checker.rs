//! Signature checkers (`script/interpreter.h:141`, v23.1.8: 138), the transaction checker and
//! the signature-cache hook, Core's `GenericTransactionSignatureChecker::VerifySignature`
//! (`script/interpreter.cpp:1537`; CS1 §4.9).

use core::cell::Cell;
use core::marker::PhantomData;

use super::ecdsa::{PubKeyRef, verify_lax};
use super::interpreter::{
    LOCKTIME_THRESHOLD, SEQUENCE_FINAL, SEQUENCE_LOCKTIME_DISABLE_FLAG, SEQUENCE_LOCKTIME_MASK,
    SEQUENCE_LOCKTIME_TYPE_FLAG,
};
use super::num::ScriptNum;
use super::sighash::{TxSigData, signature_hash_with};

/// Core's `BaseSignatureChecker`. Every default returns `false`, as Core's does.
///
/// The interpreter runs the encoding checks before calling [`Self::check_sig`], and
/// `OP_CHECKDATASIG` never calls a checker (CS1 §4.9, §10 #1).
pub trait SignatureChecker {
    /// `CheckSig(sig_with_hashtype, pubkey, scriptCode, SigVersion::BASE)`.
    fn check_sig(&self, _sig: &[u8], _pubkey: &[u8], _script_code: &[u8]) -> bool {
        false
    }

    /// `CheckLockTime(nLockTime)` for `OP_CHECKLOCKTIMEVERIFY`.
    fn check_lock_time(&self, _lock_time: ScriptNum) -> bool {
        false
    }

    /// `CheckSequence(nSequence)` for `OP_CHECKSEQUENCEVERIFY`.
    fn check_sequence(&self, _sequence: ScriptNum) -> bool {
        false
    }

    /// Called with each instruction the interpreter executes, before it runs: every
    /// instruction of an executed branch and the flow-control opcodes of an unexecuted one.
    /// Observation only, with no counterpart in Core: the default does nothing, and node-l1's
    /// fuzz accounting counts evaluated opcodes with it (CS1 §7.4).
    fn observe_instruction(&self, _opcode: u8) {}
}

/// Core's `BaseSignatureChecker` itself: every check fails. Used to evaluate scripts outside a
/// transaction (Core's `EvalScript`-only tests, `AreInputsStandard`, fuzzing).
#[derive(Copy, Clone, Debug, Default)]
pub struct NoSignatureChecker;

impl SignatureChecker for NoSignatureChecker {}

/// Core's virtual `GenericTransactionSignatureChecker::VerifySignature`, the function
/// `CachingTransactionSignatureChecker` overrides: **the signature-cache hook**.
///
/// `sig` has its hash-type byte removed, `pubkey` passed `CPubKey::IsValid`, and `sighash` is the
/// 32-byte digest. An implementation may only memoise [`verify_lax`], a deterministic function, so
/// it can never change a verdict.
pub trait SigVerifier: Sync {
    fn verify(&self, sig: &[u8], pubkey: PubKeyRef<'_>, sighash: &[u8; 32]) -> bool;
}

/// `TransactionSignatureChecker`'s own `VerifySignature`: `CPubKey::Verify`, no cache.
#[derive(Copy, Clone, Debug, Default)]
pub struct DirectVerifier;

impl SigVerifier for DirectVerifier {
    fn verify(&self, sig: &[u8], pubkey: PubKeyRef<'_>, sighash: &[u8; 32]) -> bool {
        verify_lax(pubkey, sighash, sig)
    }
}

/// Core's `GenericTransactionSignatureChecker<CTransaction>(txTo, nIn, amount, txdata)`: the
/// checker of input `n_in`, verifying through `verifier` (the signature-cache hook).
///
/// One checker per input, created where the input is verified ([`super::InputCheck::run`]). It is
/// `Send` but not `Sync`: CS1 phase C keeps a per-input sighash memo in it (C2, CS1 §8.3).
/// Inputs that verify in parallel share the [`TxSigData`], never a checker.
pub struct TransactionSignatureChecker<'a, V: SigVerifier + ?Sized = DirectVerifier> {
    txdata: &'a TxSigData<'a>,
    n_in: usize,
    verifier: &'a V,
    not_sync: PhantomData<Cell<()>>,
}

impl<'a, V: SigVerifier + ?Sized> TransactionSignatureChecker<'a, V> {
    /// # Panics
    ///
    /// If `n_in` is not an input of the transaction.
    pub fn new(txdata: &'a TxSigData<'a>, n_in: usize, verifier: &'a V) -> Self {
        assert!(n_in < txdata.tx().inputs.len(), "the checker's input index is out of range");
        Self {
            txdata,
            n_in,
            verifier,
            not_sync: PhantomData,
        }
    }
}

impl<V: SigVerifier + ?Sized> SignatureChecker for TransactionSignatureChecker<'_, V> {
    // Core: script/interpreter.cpp:1543 GenericTransactionSignatureChecker::CheckSig
    fn check_sig(&self, sig: &[u8], pubkey: &[u8], script_code: &[u8]) -> bool {
        let Some(pubkey) = PubKeyRef::new(pubkey) else {
            return false;
        };
        // The hash type is one byte tacked on to the end of the signature.
        let Some((&hash_type, sig)) = sig.split_last() else {
            return false;
        };
        let sighash =
            signature_hash_with(script_code, self.txdata, self.n_in, i32::from(hash_type));
        self.verifier.verify(sig, pubkey, &sighash)
    }

    // Core: script/interpreter.cpp:1565 GenericTransactionSignatureChecker::CheckLockTime
    fn check_lock_time(&self, lock_time: ScriptNum) -> bool {
        let tx = self.txdata.tx();
        let threshold = i64::from(LOCKTIME_THRESHOLD);
        // Compare lock times of the same kind: heights below the threshold, times above.
        if !((tx.lock_time < LOCKTIME_THRESHOLD && lock_time < threshold)
            || (tx.lock_time >= LOCKTIME_THRESHOLD && lock_time >= threshold))
        {
            return false;
        }
        if lock_time > i64::from(tx.lock_time) {
            return false;
        }
        // A final input could bypass the lock time, so this input must not be final.
        tx.inputs[self.n_in].sequence != SEQUENCE_FINAL
    }

    // Core: script/interpreter.cpp:1602 GenericTransactionSignatureChecker::CheckSequence
    fn check_sequence(&self, sequence: ScriptNum) -> bool {
        let tx = self.txdata.tx();
        let tx_to_sequence = i64::from(tx.inputs[self.n_in].sequence);
        // Fail if the transaction's version number is not set high enough to trigger BIP 68 rules.
        if (i32::from(tx.version) as u32) < 2 {
            return false;
        }
        // A sequence with its top bit set is not constrained.
        if tx_to_sequence & i64::from(SEQUENCE_LOCKTIME_DISABLE_FLAG) != 0 {
            return false;
        }
        let lock_time_mask = i64::from(SEQUENCE_LOCKTIME_TYPE_FLAG | SEQUENCE_LOCKTIME_MASK);
        let tx_to_sequence_masked = tx_to_sequence & lock_time_mask;
        let sequence_masked = sequence & lock_time_mask;
        let type_flag = i64::from(SEQUENCE_LOCKTIME_TYPE_FLAG);
        // Compare relative lock times of the same kind.
        if !((tx_to_sequence_masked < type_flag && sequence_masked < type_flag)
            || (tx_to_sequence_masked >= type_flag && sequence_masked >= type_flag))
        {
            return false;
        }
        sequence_masked <= tx_to_sequence_masked
    }
}

#[cfg(test)]
mod tests {
    use secp256k1::{Message, PublicKey, SecretKey};

    use super::*;

    #[test]
    fn direct_verifier_is_verify_lax() {
        let key = SecretKey::from_secret_bytes([0x22; 32]).unwrap();
        let pubkey = PublicKey::from_secret_key(&key).serialize();
        let sig = secp256k1::ecdsa::sign(Message::from_digest([5; 32]), &key).serialize_der();
        let pubkey = PubKeyRef::new(&pubkey).unwrap();
        assert!(DirectVerifier.verify(&sig, pubkey, &[5; 32]));
        assert!(!DirectVerifier.verify(&sig, pubkey, &[6; 32]));
    }
}
