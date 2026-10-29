//! [`Tx`]: an immutable, shared, hash-cached transaction (Core's `CTransaction`; CS0 §5).

use std::sync::{Arc, OnceLock};

use dash_primitives::{PayloadDecode, PayloadError, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::Numeric as _;

use super::canonical;
use super::serialize::CoreSerialize;
use crate::hash;
use crate::script::script_sigop_count;
use crate::{Amount, money_range};

type PayloadSlot = OnceLock<Box<Result<PayloadDecode, PayloadError>>>;

/// An immutable transaction with its hashes computed once.
///
/// Built only by the decoders (from bytes) or [`Tx::new`] (from parts). [`Tx::txid`] selects the
/// stable txid or the instance hash from the two it caches (CS0 §5.1).
///
/// There is no `PartialEq`, `Hash` or `Clone`: Core's `operator==` compares txids, which is
/// wrong for re-signed asset unlocks. Compare [`Tx::txid`] or [`Tx::instance_hash`] explicitly,
/// and share through [`TxRef`].
pub struct Tx {
    raw: Transaction,
    /// SHA256d of Core's serialization: `GetInstanceHash()`.
    instance_hash: TxHash,
    /// `ComputeStableUnlockHash`, exactly when the transaction has the
    /// `IsAssetUnlockWithStableTxid` shape.
    stable_txid: Option<TxHash>,
    /// `GetTotalSize()`.
    size: u32,
    /// The decoded payload, decoded at most once.
    payload: PayloadSlot,
    /// `GetLegacySigOpCount()`, counted at most once (DEC-25: CS1 counts, CS3 owns the slot).
    legacy_sigops: OnceLock<u32>,
}

/// A shared transaction (Core's `CTransactionRef`).
pub type TxRef = Arc<Tx>;

impl Tx {
    /// `CTransaction(CMutableTransaction)`. Canonicalises `tx_type` to the decoder's
    /// representation.
    pub fn new(mut raw: Transaction) -> Tx {
        raw.tx_type = TxType::new(raw.tx_type.to_base());
        let serialized = raw.core_serialize();
        Tx::from_serialized(raw, &serialized)
    }

    /// Hashes `serialized`, which must be Core's serialization of `raw`.
    pub(crate) fn from_serialized(raw: Transaction, serialized: &[u8]) -> Tx {
        let instance_hash = TxHash::from_lendian(hash::sha256d(serialized));
        let stable_txid = canonical::is_asset_unlock_with_stable_txid(&raw)
            .then(|| stable_unlock_hash(serialized));
        Tx {
            raw,
            instance_hash,
            stable_txid,
            size: u32::try_from(serialized.len()).unwrap_or(u32::MAX),
            payload: OnceLock::new(),
            legacy_sigops: OnceLock::new(),
        }
    }

    /// base-sdk's transaction.
    pub fn raw(&self) -> &Transaction {
        &self.raw
    }

    /// `CMutableTransaction(const CTransaction&)`: a copy to edit, then [`Tx::new`] again.
    pub fn to_mutable(&self) -> Transaction {
        self.raw.clone()
    }

    pub fn into_raw(self) -> Transaction {
        self.raw
    }

    /// `GetHash()`: the stable txid of a version-2+ asset unlock, the instance hash otherwise.
    // Core: primitives/transaction.cpp:115 CTransaction::ComputeHash (v24.0.0-rc.3)
    pub fn txid(&self) -> TxHash {
        self.stable_txid.unwrap_or(self.instance_hash)
    }

    /// `GetInstanceHash()`: SHA256d of Core's serialization.
    pub fn instance_hash(&self) -> TxHash {
        self.instance_hash
    }

    /// `GetTotalSize()`.
    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn is_null(&self) -> bool {
        canonical::is_null(&self.raw)
    }

    pub fn is_coinbase(&self) -> bool {
        canonical::is_coinbase(&self.raw)
    }

    pub fn is_special_tx_version(&self) -> bool {
        canonical::is_special_tx_version(&self.raw)
    }

    pub fn has_extra_payload_field(&self) -> bool {
        canonical::has_extra_payload_field(&self.raw)
    }

    pub fn is_platform_transfer(&self) -> bool {
        canonical::is_platform_transfer(&self.raw)
    }

    pub fn is_asset_unlock_payload(&self) -> bool {
        canonical::is_asset_unlock_payload(&self.raw)
    }

    pub fn is_asset_unlock_with_stable_txid(&self) -> bool {
        canonical::is_asset_unlock_with_stable_txid(&self.raw)
    }

    /// The decoded special payload, or `None` when the transaction has no payload field.
    /// [`PayloadDecode::is_complete`] says whether the decoder read every byte, as Core's
    /// `GetTxPayload` requires.
    ///
    /// Decoded at most once, as nothing in it depends on the BLS scheme (DEC-71): base-sdk
    /// decodes the fields Core reads under its global flag (a CbTx's chainlock signature, an
    /// asset unlock's quorum signature) as [`BlsGlobal`](dash_pkc::bls::BlsGlobal), which the
    /// code using one resolves under its own [`BlsCtx`](crate::ctx::BlsCtx) (A1 §1.4.1).
    pub fn payload(&self) -> Option<Result<&PayloadDecode, &PayloadError>> {
        if !self.has_extra_payload_field() {
            return None;
        }
        let decoded = match self.payload.get() {
            Some(decoded) => decoded,
            // `tx_type` is canonical in a `Tx`, so base-sdk's payload-field test agrees with
            // Core's and this is `Some`.
            None => {
                let decoded = self.raw.decode_payload()?;
                self.payload.get_or_init(|| Box::new(decoded))
            }
        };
        Some(decoded.as_ref().as_ref())
    }

    /// `GetLegacySigOpCount()`: the inaccurate count over every scriptSig and every output
    /// script, counted once. `CheckBlock`, `ContextualCheckBlock` and `ConnectBlock` each sum it.
    // Core: consensus/tx_verify.cpp:118 GetLegacySigOpCount
    pub fn legacy_sigop_count(&self) -> u32 {
        *self.legacy_sigops.get_or_init(|| {
            let inputs = self.raw.inputs.iter().map(|input| input.script_sig.as_bytes());
            let outputs = self.raw.outputs.iter().map(|output| output.script_pubkey.as_bytes());
            inputs
                .chain(outputs)
                .fold(0u32, |n, script| n.wrapping_add(script_sigop_count(script, false)))
        })
    }

    pub fn version(&self) -> i16 {
        self.raw.version
    }

    pub fn tx_type(&self) -> TxType {
        self.raw.tx_type
    }

    pub fn inputs(&self) -> &[TxIn] {
        &self.raw.inputs
    }

    pub fn outputs(&self) -> &[TxOut] {
        &self.raw.outputs
    }

    /// `CTransaction::GetValueOut`.
    ///
    /// # Panics
    ///
    /// When a value or the running sum leaves the money range (Core throws).
    // Core: primitives/transaction.cpp:130 CTransaction::GetValueOut
    pub fn value_out(&self) -> Amount {
        let mut total: Amount = 0;
        for output in self.outputs() {
            let sum = total.checked_add(output.value);
            assert!(
                money_range(output.value) && sum.is_some_and(money_range),
                "GetValueOut: value out of range"
            );
            total += output.value;
        }
        total
    }

    pub fn lock_time(&self) -> u32 {
        self.raw.lock_time
    }

    pub fn extra_payload(&self) -> &[u8] {
        &self.raw.extra_payload
    }
}

// Core: primitives/transaction.cpp:82 ComputeStableUnlockHash (v24.0.0-rc.3): the serialization
// with its trailing ASSET_UNLOCK_QUORUM_INFO_SIZE bytes zeroed, hashed.
fn stable_unlock_hash(serialized: &[u8]) -> TxHash {
    let mut scratch = serialized.to_vec();
    let tail = scratch.len().saturating_sub(canonical::ASSET_UNLOCK_QUORUM_INFO_SIZE);
    scratch[tail..].fill(0);
    TxHash::from_lendian(hash::sha256d(&scratch))
}

// Tx is shared across threads through TxRef (CS0 §5).
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Tx>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{core_serialize, decode_tx};
    use dash_primitives::{OutPoint, TxHash};

    fn special(tx_type: TxType) -> Transaction {
        Transaction {
            version: 3,
            tx_type,
            inputs: vec![TxIn {
                prevout: OutPoint {
                    hash: TxHash::from_lendian([7; 32]),
                    index: 0,
                },
                script_sig: Default::default(),
                sequence: u32::MAX,
            }],
            outputs: vec![],
            lock_time: 0,
            extra_payload: vec![1, 2, 3],
        }
    }

    /// base-sdk's `Unknown(n)` catch-all can hold a value that has a named variant; `Tx::new`
    /// gives it the decoder's representation, and the hashes and predicates use the number.
    #[test]
    fn a_shadowing_unknown_type_behaves_as_its_named_variant() {
        let named = Tx::new(special(TxType::CoinbaseCommitment));
        let shadow = Tx::new(special(TxType::Unknown(5)));
        assert_eq!(shadow.tx_type(), TxType::CoinbaseCommitment);
        assert_eq!(shadow.instance_hash(), named.instance_hash());
        assert_eq!(shadow.has_extra_payload_field(), named.has_extra_payload_field());
        let wire = core_serialize(&named);
        let decoded = decode_tx(&mut &wire[..]).map(|d| d.into_value().instance_hash());
        assert_eq!(decoded.ok(), Some(named.instance_hash()));

        let spend_shadow = Tx::new(special(TxType::Unknown(0)));
        assert_eq!(spend_shadow.tx_type(), TxType::Spend);
        assert!(!spend_shadow.has_extra_payload_field());
    }

    /// The payload is decoded once, whatever the caller's BLS scheme (DEC-71): every call
    /// returns the same value.
    #[test]
    fn the_payload_is_decoded_once() {
        let addr = |r: Result<&PayloadDecode, &PayloadError>| match r {
            Ok(p) => std::ptr::from_ref(p).cast::<()>(),
            Err(e) => std::ptr::from_ref(e).cast::<()>(),
        };
        let tx = Tx::new(special(TxType::CoinbaseCommitment));
        let first = tx.payload().map(addr);
        assert!(first.is_some());
        assert_eq!(tx.payload().map(addr), first);
        assert!(Tx::new(special(TxType::Spend)).payload().is_none());
    }
}
