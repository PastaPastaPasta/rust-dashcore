//! `script/sign.cpp`'s signing core without PSBT (BL-688), which `combinerawtransaction` and
//! `signrawtransactionwithkey` share: signature data and its merge, the signing providers the
//! two commands use, the transaction signature creator, `ProduceSignature`,
//! `DataFromTransaction` and `SignTransaction`.
//!
//! Dash has no segwit, so every script is `SigVersion::BASE` and the legacy `SignatureHash`
//! signs; it ignores the amount, which is why the creator holds none.

use std::cell::RefCell;
use std::collections::BTreeMap;

use dash_consensus::coins::Coin;
use dash_consensus::primitives::{OutPoint, Transaction, TxIn};
use dash_consensus::script::interpreter::MAX_SCRIPT_ELEMENT_SIZE;
use dash_consensus::script::opcodes::{
    OP_0, OP_1, OP_1NEGATE, OP_CHECKSIG, OP_DUP, OP_EQUALVERIFY, OP_HASH160, OP_RETURN,
};
use dash_consensus::script::{
    DirectVerifier, NoSignatureChecker, SIGHASH_SINGLE, ScriptError, ScriptFlags, ScriptNum,
    SignatureChecker, Stack, TransactionSignatureChecker, TxSigData, eval_script,
    is_pay_to_script_hash, is_push_only, push_data, signature_hash, verify_script,
};
use secp256k1::Message;

use crate::core_write::TxoutType;
use crate::descriptor::match_multisig;
use crate::key_io::{Key, hash160, match_pay_to_pubkey};

/// `CKeyID`: a public key's Hash160.
pub type KeyId = [u8; 20];

/// `CScriptID`: a script's Hash160.
pub type ScriptId = [u8; 20];

/// `SigPair`: the serialized public key and its signature (with the hash-type byte).
pub type SigPair = (Vec<u8>, Vec<u8>);

/// Core's `SignatureData` without the PSBT-only key origins.
// Core: script/sign.h:65-78
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureData {
    /// Whether `script_sig` is complete.
    pub complete: bool,
    pub script_sig: Vec<u8>,
    /// The redeem script, if any.
    pub redeem_script: Vec<u8>,
    /// The partial signatures by key id.
    pub signatures: BTreeMap<KeyId, SigPair>,
    pub missing_pubkeys: Vec<KeyId>,
    pub missing_sigs: Vec<KeyId>,
    /// `uint160` zero when nothing is missing.
    pub missing_redeem_script: ScriptId,
}

impl SignatureData {
    /// `MergeSignatureData`: a complete side wins and is never replaced; the first non-empty
    /// redeem script is kept; a signature already held for a key id is never replaced.
    // Core: script/sign.cpp:314-325
    pub fn merge(&mut self, other: SignatureData) {
        if self.complete {
            return;
        }
        if other.complete {
            *self = other;
            return;
        }
        if self.redeem_script.is_empty() && !other.redeem_script.is_empty() {
            self.redeem_script = other.redeem_script;
        }
        for (id, pair) in other.signatures {
            self.signatures.entry(id).or_insert(pair);
        }
    }
}

/// Core's `SigningProvider`, the lookups signing makes (`GetPubKey` is `get_key`'s public key).
pub trait SigningProvider {
    fn get_key(&self, id: &KeyId) -> Option<&Key>;
    fn get_cscript(&self, id: &ScriptId) -> Option<&[u8]>;
}

/// `DUMMY_SIGNING_PROVIDER`: nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct DummySigningProvider;

impl SigningProvider for DummySigningProvider {
    fn get_key(&self, _: &KeyId) -> Option<&Key> {
        None
    }
    fn get_cscript(&self, _: &ScriptId) -> Option<&[u8]> {
        None
    }
}

/// Core's `FillableSigningProvider`: keys by the id of their public key, scripts by Hash160.
#[derive(Default)]
pub struct FillableSigningProvider {
    keys: BTreeMap<KeyId, Key>,
    scripts: BTreeMap<ScriptId, Vec<u8>>,
}

impl FillableSigningProvider {
    /// `AddKey`: `AddKeyPubKey(key, key.GetPubKey())`, so the key's compression picks its id.
    /// A key already held for the id is replaced.
    // Core: script/signingprovider.cpp:76-81
    pub fn add_key(&mut self, key: Key) {
        self.keys.insert(hash160(&key.pubkey()), key);
    }

    /// `AddCScript`: a script over `MAX_SCRIPT_ELEMENT_SIZE` bytes is refused (logged, not
    /// stored, `false`).
    // Core: script/signingprovider.cpp:111-120
    pub fn add_cscript(&mut self, script: &[u8]) -> bool {
        if script.len() > MAX_SCRIPT_ELEMENT_SIZE {
            tracing::error!(
                "FillableSigningProvider::AddCScript(): redeemScripts > {MAX_SCRIPT_ELEMENT_SIZE} \
                 bytes are invalid"
            );
            return false;
        }
        self.scripts.insert(hash160(script), script.to_vec());
        true
    }
}

impl SigningProvider for FillableSigningProvider {
    fn get_key(&self, id: &KeyId) -> Option<&Key> {
        self.keys.get(id)
    }

    fn get_cscript(&self, id: &ScriptId) -> Option<&[u8]> {
        self.scripts.get(id).map(Vec::as_slice)
    }
}

/// Core's `MutableTransactionSignatureCreator` for input `n_in` of `tx`.
pub struct Creator<'a> {
    pub tx: &'a Transaction,
    pub n_in: usize,
    pub hash_type: i32,
}

impl Creator<'_> {
    /// `CreateSig`: the key's signature of the legacy `SignatureHash`, DER with the hash-type
    /// byte. No key, or `SIGHASH_SINGLE` without an output at the input's index, signs nothing.
    // Core: script/sign.cpp:34-50
    pub fn create_sig(
        &self,
        provider: &dyn SigningProvider,
        id: &KeyId,
        script_code: &[u8],
    ) -> Option<Vec<u8>> {
        let key = provider.get_key(id)?;
        // A SINGLE signature without its output commits to no output at all, so it would stay
        // valid if an output were swapped in: don't produce it.
        if self.hash_type & 0x1f == SIGHASH_SINGLE && self.n_in >= self.tx.outputs().len() {
            return None;
        }
        let hash = signature_hash(script_code, self.tx, self.n_in, self.hash_type);
        let mut sig = sign(key, &hash);
        sig.push(self.hash_type as u8);
        Some(sig)
    }
}

/// `CKey::Sign(hash, sig)`: RFC 6979 with the low-R grind (a 32-byte little-endian counter as
/// extra entropy until R's first byte is below 0x80), low S, DER.
// Core: key.cpp:213-238
pub fn sign(key: &Key, hash: &[u8; 32]) -> Vec<u8> {
    key.secret
        .with(|secret| secp256k1::ecdsa::sign_low_r(Message::from_digest(*hash), secret))
        .expect("a valid secret")
        .serialize_der()
        .to_vec()
}

/// `Solver`: the type and Core's `vSolutions` (the script hash, the key, the key hash, or the
/// threshold, the keys and their count, each count one byte).
// Core: script/standard.cpp:115-159
pub fn solver_solutions(script: &[u8]) -> (TxoutType, Vec<Vec<u8>>) {
    if is_pay_to_script_hash(script) {
        return (TxoutType::ScriptHash, vec![script[2..22].to_vec()]);
    }
    if script.first() == Some(&OP_RETURN) && is_push_only(&script[1..]) {
        return (TxoutType::NullData, Vec::new());
    }
    if let Some(key) = match_pay_to_pubkey(script) {
        return (TxoutType::Pubkey, vec![key.to_vec()]);
    }
    if let [OP_DUP, OP_HASH160, 20, hash @ .., OP_EQUALVERIFY, OP_CHECKSIG] = script
        && hash.len() == 20
    {
        return (TxoutType::PubkeyHash, vec![hash.to_vec()]);
    }
    if let Some((required, keys)) = match_multisig(script) {
        let mut solutions = Vec::with_capacity(keys.len() + 2);
        solutions.push(vec![required as u8]);
        solutions.extend(keys.iter().map(|k| k.to_vec()));
        solutions.push(vec![keys.len() as u8]);
        return (TxoutType::Multisig, solutions);
    }
    (TxoutType::Nonstandard, Vec::new())
}

/// The static `GetPubKey`: the partial signatures, then the provider.
// Core: script/sign.cpp:65-81
fn get_pubkey(
    provider: &dyn SigningProvider,
    sigdata: &SignatureData,
    id: &KeyId,
) -> Option<Vec<u8>> {
    if let Some((pubkey, _)) = sigdata.signatures.get(id) {
        return Some(pubkey.clone());
    }
    // Core's `misc_pubkeys` comes next, but only PSBT key origins fill it.
    provider.get_key(id).map(Key::pubkey)
}

/// The static `CreateSig`: a signature already held for the key is reused whatever its hash
/// type; else the creator's, recorded; else the key id is missing.
// Core: script/sign.cpp:83-103
fn create_sig(
    creator: &Creator<'_>,
    sigdata: &mut SignatureData,
    provider: &dyn SigningProvider,
    pubkey: &[u8],
    script_code: &[u8],
) -> Option<Vec<u8>> {
    let id = hash160(pubkey);
    if let Some((_, sig)) = sigdata.signatures.get(&id) {
        return Some(sig.clone());
    }
    if let Some(sig) = creator.create_sig(provider, &id, script_code) {
        sigdata.signatures.insert(id, (pubkey.to_vec(), sig.clone()));
        return Some(sig);
    }
    sigdata.missing_sigs.push(id);
    None
}

/// `SignStep`: whether `script_pubkey` is solved, its type and the stack items that solve it
/// (for P2SH, the redeem script).
// Core: script/sign.cpp:111-174
fn sign_step(
    provider: &dyn SigningProvider,
    creator: &Creator<'_>,
    script_pubkey: &[u8],
    sigdata: &mut SignatureData,
) -> (bool, TxoutType, Vec<Vec<u8>>) {
    let mut ret = Vec::new();
    let (which, solutions) = solver_solutions(script_pubkey);
    let solved = match which {
        TxoutType::Nonstandard | TxoutType::NullData => false,
        TxoutType::Pubkey => {
            match create_sig(creator, sigdata, provider, &solutions[0], script_pubkey) {
                Some(sig) => {
                    ret.push(sig);
                    true
                }
                None => false,
            }
        }
        TxoutType::PubkeyHash => {
            let id: KeyId = solutions[0].as_slice().try_into().expect("a 20-byte key hash");
            match get_pubkey(provider, sigdata, &id) {
                None => {
                    sigdata.missing_pubkeys.push(id);
                    false
                }
                Some(pubkey) => {
                    match create_sig(creator, sigdata, provider, &pubkey, script_pubkey) {
                        Some(sig) => {
                            ret.push(sig);
                            ret.push(pubkey);
                            true
                        }
                        None => false,
                    }
                }
            }
        }
        TxoutType::ScriptHash => {
            let id: ScriptId = solutions[0].as_slice().try_into().expect("a 20-byte script hash");
            // `GetCScript`: the provider's, then the signature data's.
            let script = match provider.get_cscript(&id) {
                Some(script) => Some(script.to_vec()),
                None => {
                    (hash160(&sigdata.redeem_script) == id).then(|| sigdata.redeem_script.clone())
                }
            };
            match script {
                Some(script) => {
                    ret.push(script);
                    true
                }
                None => {
                    sigdata.missing_redeem_script = id;
                    false
                }
            }
        }
        TxoutType::Multisig => {
            let required = usize::from(solutions[0][0]);
            // The CHECKMULTISIG bug's extra item.
            ret.push(Vec::new());
            // Every key is tried, so the data holds every signature that can be made.
            for pubkey in &solutions[1..solutions.len() - 1] {
                if let Some(sig) = create_sig(creator, sigdata, provider, pubkey, script_pubkey)
                    && ret.len() < required + 1
                {
                    ret.push(sig);
                }
            }
            let ok = ret.len() == required + 1;
            // Core's loop as written: `ret` grows inside it.
            let mut i = 0;
            while i + ret.len() < required + 1 {
                ret.push(Vec::new());
                i += 1;
            }
            ok
        }
    };
    (solved, which, ret)
}

/// `PushAll`: each item as its minimal push (`OP_0`, `OP_1`..`OP_16`, `OP_1NEGATE`).
// Core: script/sign.cpp:176-191
pub fn push_all(values: &[Vec<u8>]) -> Vec<u8> {
    let mut script = Vec::new();
    for v in values {
        match v.as_slice() {
            [] => script.push(OP_0),
            &[n @ 1..=16] => script.push(OP_1 + n - 1),
            [0x81] => script.push(OP_1NEGATE),
            _ => push_data(&mut script, v),
        }
    }
    script
}

/// `ProduceSignature`: nothing for complete data; else `SignStep`, P2SH's second step, the
/// scriptSig rebuilt from the result **whether solved or not**, and complete when solved and
/// it verifies under the standard flags against the creator's transaction.
// Core: script/sign.cpp:193-222
pub fn produce_signature(
    provider: &dyn SigningProvider,
    creator: &Creator<'_>,
    script_pubkey: &[u8],
    sigdata: &mut SignatureData,
) -> bool {
    if sigdata.complete {
        return true;
    }
    let (mut solved, which, mut result) = sign_step(provider, creator, script_pubkey, sigdata);
    if solved && which == TxoutType::ScriptHash {
        // The solution is the redeem script, which is signed in turn and goes last.
        let redeem = result.swap_remove(0);
        sigdata.redeem_script = redeem.clone();
        let (inner_solved, inner_which, inner) = sign_step(provider, creator, &redeem, sigdata);
        solved = inner_solved && inner_which != TxoutType::ScriptHash;
        result = inner;
        result.push(redeem);
    }
    sigdata.script_sig = push_all(&result);
    let txdata = TxSigData::new(creator.tx);
    let checker = TransactionSignatureChecker::new(&txdata, creator.n_in, &DirectVerifier);
    sigdata.complete = solved
        && verify_script(&sigdata.script_sig, script_pubkey, ScriptFlags::STANDARD, &checker)
            .is_ok();
    sigdata.complete
}

/// `SignatureExtractorChecker`: the inner checker's verdicts, recording each passing
/// signature by its key's id (an earlier one is kept).
// Core: script/sign.cpp:225-242
struct Extractor<'a, C: SignatureChecker> {
    inner: &'a C,
    signatures: RefCell<BTreeMap<KeyId, SigPair>>,
}

impl<C: SignatureChecker> SignatureChecker for Extractor<'_, C> {
    fn check_sig(&self, sig: &[u8], pubkey: &[u8], script_code: &[u8]) -> bool {
        if !self.inner.check_sig(sig, pubkey, script_code) {
            return false;
        }
        self.signatures
            .borrow_mut()
            .entry(hash160(pubkey))
            .or_insert_with(|| (pubkey.to_vec(), sig.to_vec()));
        true
    }

    fn check_lock_time(&self, lock_time: ScriptNum) -> bool {
        self.inner.check_lock_time(lock_time)
    }

    fn check_sequence(&self, sequence: ScriptNum) -> bool {
        self.inner.check_sequence(sequence)
    }

    fn observe_instruction(&self, opcode: u8) {
        self.inner.observe_instruction(opcode);
    }
}

/// `DataFromTransaction`: the signature data in input `n_in`'s scriptSig for an output paying
/// `script_pubkey`, checked against `tx` itself.
///
/// The scriptSig's stack is its evaluation under `STRICTENC` with no signature checks, kept as
/// far as it got when evaluation fails. A scriptSig that verifies is complete. Else, for P2SH,
/// the stack's last item (when not empty) is the redeem script; for a multisig, each stack item
/// is matched to the keys from the last match on: a key already holding a signature (one the
/// failed verification recorded) takes the item without checking it.
///
/// # Panics
///
/// If `n_in` is not an input of `tx`, as Core asserts.
// Core: script/sign.cpp:244-307
pub fn data_from_transaction(tx: &Transaction, n_in: usize, script_pubkey: &[u8]) -> SignatureData {
    assert!(n_in < tx.inputs().len(), "DataFromTransaction: input index out of range");
    let mut data = SignatureData {
        script_sig: tx.inputs()[n_in].script_sig().as_bytes().to_vec(),
        ..SignatureData::default()
    };
    let mut stack: Stack = Vec::new();
    let _ = eval_script(&mut stack, &data.script_sig, ScriptFlags::STRICTENC, &NoSignatureChecker);

    let txdata = TxSigData::new(tx);
    let tx_checker = TransactionSignatureChecker::new(&txdata, n_in, &DirectVerifier);
    let extractor = Extractor {
        inner: &tx_checker,
        signatures: RefCell::new(BTreeMap::new()),
    };
    if verify_script(&data.script_sig, script_pubkey, ScriptFlags::STANDARD, &extractor).is_ok() {
        data.signatures = extractor.signatures.into_inner();
        data.complete = true;
        return data;
    }

    let (mut script_type, mut solutions) = solver_solutions(script_pubkey);
    let mut next_script = script_pubkey.to_vec();
    if script_type == TxoutType::ScriptHash
        && let Some(redeem) = stack.last().filter(|item| !item.is_empty())
    {
        data.redeem_script = redeem.clone();
        next_script = redeem.clone();
        (script_type, solutions) = solver_solutions(&next_script);
        stack.pop();
    }

    if script_type == TxoutType::Multisig && !stack.is_empty() {
        let num_pubkeys = solutions.len() - 2;
        let mut last_success_key = 0;
        for sig in &stack {
            for i in last_success_key..num_pubkeys {
                let pubkey = &solutions[i + 1];
                let recorded = extractor.signatures.borrow().contains_key(&hash160(pubkey));
                if recorded || extractor.check_sig(sig, pubkey, &next_script) {
                    last_success_key = i + 1;
                    break;
                }
            }
        }
    }
    data.signatures = extractor.signatures.into_inner();
    data
}

/// `UpdateInput`.
// Core: script/sign.cpp:309-312
pub fn update_input(tx: &mut Transaction, n_in: usize, data: &SignatureData) {
    let mut inputs = tx.inputs().clone();
    let txin = &inputs[n_in];
    inputs[n_in] = TxIn::new(txin.prevout(), data.script_sig.clone().into(), txin.sequence());
    *tx = Transaction::new(
        tx.version(),
        tx.tx_type(),
        inputs,
        tx.outputs().clone(),
        tx.lock_time(),
        tx.extra_payload().clone(),
    );
}

/// `SignTransaction(mtx, keystore, coins, nHashType, input_errors)`: signs what it can of
/// `mtx`, each input over the transaction as signed so far, then verifies it against the
/// transaction as given. The errors by input index; none means complete.
///
/// `coins` holds the unspent coins only: Core's map entry for a missing or spent coin is a
/// spent `Coin`, which every use reads as absent.
// Core: script/sign.cpp:406-464
pub fn sign_transaction(
    mtx: &mut Transaction,
    provider: &dyn SigningProvider,
    coins: &BTreeMap<OutPoint, Coin>,
    hash_type: i32,
) -> BTreeMap<usize, String> {
    let tx_const = mtx.clone();
    let const_data = TxSigData::new(&tx_const);
    let mut errors = BTreeMap::new();
    for i in 0..mtx.inputs().len() {
        let Some(coin) = coins.get(&mtx.inputs()[i].prevout()) else {
            errors.insert(i, "Input not found or already spent".to_owned());
            continue;
        };
        let script_pubkey = coin.script_pubkey().as_slice();
        let mut sigdata = data_from_transaction(mtx, i, script_pubkey);
        let creator = Creator {
            tx: mtx,
            n_in: i,
            hash_type,
        };
        produce_signature(provider, &creator, script_pubkey, &mut sigdata);
        update_input(mtx, i, &sigdata);

        let checker = TransactionSignatureChecker::new(&const_data, i, &DirectVerifier);
        let verified = verify_script(
            mtx.inputs()[i].script_sig().as_bytes(),
            script_pubkey,
            ScriptFlags::STANDARD,
            &checker,
        );
        if let Err(e) = verified {
            errors.insert(i, sign_error_text(e).to_owned());
        }
    }
    errors
}

/// `SignTransaction`'s text for a failed verification.
// Core: script/sign.cpp:449-457
pub fn sign_error_text(e: ScriptError) -> &'static str {
    match e {
        // Unable to sign input and verification failed (a partial signing, possibly).
        ScriptError::InvalidStackOperation => {
            "Unable to sign input, invalid stack size (possibly missing key)"
        }
        // Verification failed (too few signatures, possibly).
        ScriptError::SigNullFail => {
            "CHECK(MULTI)SIG failing with non-zero signature (possibly need more signatures)"
        }
        e => e.message(),
    }
}

#[cfg(test)]
mod tests;
