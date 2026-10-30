// EMBARGO-1
//! Core's serialization of every transaction field, and Core's predicates on a transaction
//! (CS0 §4.3, PLAN §7 handling).
//!
//! One writer per field of `CTransaction::Serialize`, in Core's order. Each takes the field's
//! value rather than the transaction, so CS1's `CTransactionSignatureSerializer` can pass the
//! modified inputs and outputs Core's sighash uses. Every Core predicate on `CTransaction` is
//! here too, so they are ported, reviewed and tested in one place.
//!
//! Every function ports Core's own expression; type comparisons use the numeric value
//! (`tx_type.to_base()`), as Core compares `nType`, never `TxType` equality.

use dash_primitives::{OutPoint, Transaction};
use dash_types::Numeric as _;

use super::serialize::{CoreWrite, write_bytes, write_compact_size};

/// `CTransaction::SPECIAL_VERSION`.
pub const SPECIAL_VERSION: i16 = 3;
/// `TRANSACTION_NORMAL`.
pub const TRANSACTION_NORMAL: u16 = 0;
/// `TRANSACTION_COINBASE`.
pub const TRANSACTION_COINBASE: u16 = 5;
/// `TRANSACTION_ASSET_UNLOCK`.
pub const TRANSACTION_ASSET_UNLOCK: u16 = 9;
/// `ASSET_UNLOCK_QUORUM_INFO_SIZE` (v24): the trailing payload bytes a re-signed asset unlock
/// changes.
pub const ASSET_UNLOCK_QUORUM_INFO_SIZE: usize = 4 + 32 + 96;
/// `ASSET_UNLOCK_PAYLOAD_SIZE` (v24).
pub const ASSET_UNLOCK_PAYLOAD_SIZE: usize = 1 + 8 + 4 + ASSET_UNLOCK_QUORUM_INFO_SIZE;
/// `ASSET_UNLOCK_STABLE_TXID_VERSION` (v24).
pub const ASSET_UNLOCK_STABLE_TXID_VERSION: u8 = 2;

// Core: primitives/transaction.h:271 CTransaction::Serialize (v23.1.8 :244):
// int32_t n32bitVersion = this->nVersion | (this->nType << 16);
/// The leading 4-byte field.
pub fn write_version_and_type(tx: &Transaction, w: &mut impl CoreWrite) {
    let n32bit_version: i32 = i32::from(tx.version) | (i32::from(tx.tx_type.to_base()) << 16);
    w.write(&n32bit_version.to_le_bytes());
}

// Core: primitives/transaction.h:70 COutPoint SERIALIZE_METHODS (v23.1.8 :50): hash, n
/// An input's `prevout` (DEC-25).
pub fn write_prevout(prevout: &OutPoint, w: &mut impl CoreWrite) {
    w.write(prevout.hash.as_bytes());
    w.write(&prevout.index.to_le_bytes());
}

/// An input's `nSequence` (DEC-25).
pub fn write_sequence(sequence: u32, w: &mut impl CoreWrite) {
    w.write(&sequence.to_le_bytes());
}

// Core: primitives/transaction.h:161 CTxIn SERIALIZE_METHODS (v23.1.8 :141): prevout, scriptSig, nSequence
/// One input: [`write_prevout`] ‖ the script ‖ [`write_sequence`].
pub fn write_input(prevout: &OutPoint, script_sig: &[u8], sequence: u32, w: &mut impl CoreWrite) {
    write_prevout(prevout, w);
    write_bytes(script_sig, w);
    write_sequence(sequence, w);
}

// Core: primitives/transaction.h:199 CTxOut SERIALIZE_METHODS (v23.1.8 :179): nValue, scriptPubKey
/// One output. `value` is Core's `CAmount`, so the null output `CTxOut()` (−1) is writable.
pub fn write_output(value: i64, script_pubkey: &[u8], w: &mut impl CoreWrite) {
    w.write(&value.to_le_bytes());
    write_bytes(script_pubkey, w);
}

/// `nLockTime`.
pub fn write_lock_time(lock_time: u32, w: &mut impl CoreWrite) {
    w.write(&lock_time.to_le_bytes());
}

// Core: primitives/transaction.h:276 (v23.1.8 :249): if (this->HasExtraPayloadField()) s << vExtraPayload;
/// `vExtraPayload`; writes nothing when the transaction has no payload field.
pub fn write_extra_payload(tx: &Transaction, w: &mut impl CoreWrite) {
    if has_extra_payload_field(tx) {
        write_bytes(&tx.extra_payload, w);
    }
}

// Core: primitives/transaction.h:270 CTransaction::Serialize (v23.1.8 :243)
/// The whole `CTransaction::Serialize`.
pub fn write_tx(tx: &Transaction, w: &mut impl CoreWrite) {
    write_version_and_type(tx, w);
    write_compact_size(tx.inputs.len() as u64, w);
    for input in &tx.inputs {
        write_input(&input.prevout, input.script_sig.as_bytes(), input.sequence, w);
    }
    write_compact_size(tx.outputs.len() as u64, w);
    for output in &tx.outputs {
        write_output(output.value, output.script_pubkey.as_bytes(), w);
    }
    write_lock_time(tx.lock_time, w);
    write_extra_payload(tx, w);
}

// Core: primitives/transaction.h:285 CTransaction::IsNull (v23.1.8 :258)
pub fn is_null(tx: &Transaction) -> bool {
    tx.inputs.is_empty() && tx.outputs.is_empty()
}

// Core: primitives/transaction.h:308 CTransaction::IsCoinBase (v23.1.8 :274)
/// Exactly one input, with a null prevout. base-sdk's own check accepts any input count.
pub fn is_coinbase(tx: &Transaction) -> bool {
    tx.inputs.len() == 1 && tx.inputs[0].prevout.is_null()
}

// Core: primitives/transaction.h:325 CTransaction::IsSpecialTxVersion (v23.1.8 :291)
pub fn is_special_tx_version(tx: &Transaction) -> bool {
    tx.version >= SPECIAL_VERSION
}

// Core: primitives/transaction.h:330 CTransaction::IsPlatformTransfer (v23.1.8 :296)
pub fn is_platform_transfer(tx: &Transaction) -> bool {
    is_special_tx_version(tx) && tx.tx_type.to_base() == TRANSACTION_ASSET_UNLOCK
}

// Core: primitives/transaction.h:335 CTransaction::HasExtraPayloadField (v23.1.8 :301)
pub fn has_extra_payload_field(tx: &Transaction) -> bool {
    is_special_tx_version(tx) && tx.tx_type.to_base() != TRANSACTION_NORMAL
}

// Core: primitives/transaction.h:383 IsAssetUnlockPayload (v24.0.0-rc.3 only)
pub fn is_asset_unlock_payload(tx: &Transaction) -> bool {
    tx.version >= SPECIAL_VERSION
        && tx.tx_type.to_base() == TRANSACTION_ASSET_UNLOCK
        && tx.extra_payload.len() == ASSET_UNLOCK_PAYLOAD_SIZE
}

// Core: primitives/transaction.h:392 IsAssetUnlockWithStableTxid (v24.0.0-rc.3 only)
pub fn is_asset_unlock_with_stable_txid(tx: &Transaction) -> bool {
    is_asset_unlock_payload(tx) && tx.extra_payload[0] >= ASSET_UNLOCK_STABLE_TXID_VERSION
}
