// Rust Dash Library
// Originally written in 2014 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
//     For Bitcoin
// Updated for Dash in 2022 by
//     The Dash Core Developers
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! Dash transactions.
//!
//! A transaction describes a transfer of money. It consumes previously-unspent
//! transaction outputs and produces new ones, satisfying the condition to spend
//! the old outputs (typically a digital signature with a specific key must be
//! provided) and defining the condition to spend the new ones. The use of digital
//! signatures ensures that coins cannot be spent by unauthorized parties.
//!
//! This module provides the structures and functions needed to support transactions.
//!

pub mod outpoint;
pub mod special_transaction;
pub mod txin;
pub mod txout;

use core::default::Default;

#[cfg(feature = "bincode")]
use bincode::{Decode, Encode};
use hashes::{Hash, sha256d};

use crate::blockdata::constants::WITNESS_SCALE_FACTOR;
#[cfg(feature = "bitcoinconsensus")]
use crate::blockdata::script;
use crate::blockdata::script::Script;
use crate::blockdata::transaction::special_transaction::asset_unlock::qualified_asset_unlock::{
    ASSET_UNLOCK_PAYLOAD_SIZE, ASSET_UNLOCK_QUORUM_INFO_SIZE, ASSET_UNLOCK_STABLE_TXID_VERSION,
};
pub use crate::blockdata::transaction::special_transaction::{TransactionPayload, TransactionType};
use crate::blockdata::transaction::txin::TxIn;
use crate::blockdata::transaction::txout::TxOut;
use crate::blockdata::witness::Witness;
use crate::consensus::encode::VarInt;
use crate::consensus::{Decodable, Encodable, encode};
use crate::hash_types::{InputsHash, Txid, Wtxid};
use crate::prelude::*;
pub use crate::transaction::outpoint::*;
use crate::{ScriptBuf, Weight, io};

/// Result of `SighashCache::legacy_encode_signing_data_to`.
///
/// This type forces the caller to handle SIGHASH_SINGLE bug case.
///
/// This corner case can't be expressed using standard `Result`,
/// in a way that is both convenient and not-prone to accidental
/// mistakes (like calling `.expect("writer never fails")`).
#[must_use]
pub enum EncodeSigningDataResult<E> {
    /// Input data is an instance of `SIGHASH_SINGLE` bug
    SighashSingleBug,
    /// Operation performed normally.
    WriteResult(Result<(), E>),
}

impl<E> EncodeSigningDataResult<E> {
    /// Checks for SIGHASH_SINGLE bug returning error if the writer failed.
    ///
    /// This method is provided for easy and correct handling of the result because
    /// SIGHASH_SINGLE bug is a special case that must not be ignored nor cause panicking.
    /// Since the data is usually written directly into a hasher which never fails,
    /// the recommended pattern to handle this is:
    ///
    /// ```rust
    /// # use dashcore::consensus::deserialize;
    /// # use dashcore::hashes::Hash;
    /// # use dashcore::sighash::{LegacySighash, SighashCache};
    /// # use dashcore::Transaction;
    /// # let mut writer = LegacySighash::engine();
    /// # let input_index = 0;
    /// # let script_pubkey = dashcore::ScriptBuf::new();
    /// # let sighash_u32 = 0u32;
    /// # const SOME_TX: &'static str = "0100000001a15d57094aa7a21a28cb20b59aab8fc7d1149a3bdbcddba9c622e4f5f6a99ece010000006c493046022100f93bb0e7d8db7bd46e40132d1f8242026e045f03a0efe71bbb8e3f475e970d790221009337cd7f1f929f00cc6ff01f03729b069a7c21b59b1736ddfee5db5946c5da8c0121033b9b137ee87d5a812d6f506efdd37f0affa7ffc310711c06c7f3e097c9447c52ffffffff0100e1f505000000001976a9140389035a9225b3839e2bbf32d826a1e222031fd888ac00000000";
    /// # let raw_tx = hex_conservative::decode_to_vec(SOME_TX).unwrap();
    /// # let tx: Transaction = deserialize(&raw_tx).unwrap();
    /// let cache = SighashCache::new(&tx);
    /// if cache.legacy_encode_signing_data_to(&mut writer, input_index, &script_pubkey, sighash_u32)
    ///         .is_sighash_single_bug()
    ///         .expect("writer can't fail") {
    ///     // use a hash value of "1", instead of computing the actual hash due to SIGHASH_SINGLE bug
    /// }
    /// ```
    #[allow(clippy::wrong_self_convention)] // E is not Copy so we consume self.
    pub fn is_sighash_single_bug(self) -> Result<bool, E> {
        match self {
            EncodeSigningDataResult::SighashSingleBug => Ok(true),
            EncodeSigningDataResult::WriteResult(Ok(())) => Ok(false),
            EncodeSigningDataResult::WriteResult(Err(e)) => Err(e),
        }
    }

    /// Maps a `Result<T, E>` to `Result<T, F>` by applying a function to a
    /// contained [`Err`] value, leaving an [`Ok`] value untouched.
    ///
    /// Like [`Result::map_err`].
    pub fn map_err<E2, F>(self, f: F) -> EncodeSigningDataResult<E2>
    where
        F: FnOnce(E) -> E2,
    {
        match self {
            EncodeSigningDataResult::SighashSingleBug => EncodeSigningDataResult::SighashSingleBug,
            EncodeSigningDataResult::WriteResult(Err(e)) => {
                EncodeSigningDataResult::WriteResult(Err(f(e)))
            }
            EncodeSigningDataResult::WriteResult(Ok(o)) => {
                EncodeSigningDataResult::WriteResult(Ok(o))
            }
        }
    }
}

/// A Dash transaction, which describes an authenticated movement of coins.
///
/// The serialization is Dash Core's: the version and type, the inputs, the outputs, the lock time
/// and, for a special transaction, its payload. Dash has no BIP141/BIP144 serialization, so
/// there is no marker or flag and a transaction is never written or read with witnesses: the
/// `witness` of an input is not part of the encoding. A transaction without inputs is
/// serialized like any other, and a `00 01` after the version reads as no inputs and one output
/// (an invalid transaction, which a node rejects when it checks it).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Transaction {
    /// The protocol version, is currently expected to be 1 or 2 (BIP 68).
    pub version: u16,
    /// Block number before which this transaction is valid, or 0 for valid immediately.
    pub lock_time: u32,
    /// List of transaction inputs.
    pub input: Vec<TxIn>,
    /// List of transaction outputs.
    pub output: Vec<TxOut>,
    /// Special Transaction Payload. As in Dash Core (`HasExtraPayloadField`), it is only
    /// serialized when `version` is a special transaction version (3 or more) and the type isn't
    /// classic: a payload set on a lower version is left out of the encoding and the txid.
    pub special_transaction_payload: Option<TransactionPayload>,
}

impl Transaction {
    /// Computes a "normalized TXID" which does not include any signatures.
    /// This gives a way to identify a transaction that is "the same" as
    /// another in the sense of having same inputs and outputs.
    pub fn ntxid(&self) -> sha256d::Hash {
        let cloned_tx = Transaction {
            version: self.version,
            lock_time: self.lock_time,
            input: self
                .input
                .iter()
                .map(|txin| TxIn {
                    script_sig: ScriptBuf::new(),
                    witness: Witness::default(),
                    ..*txin
                })
                .collect(),
            output: self.output.clone(),
            special_transaction_payload: self.special_transaction_payload.clone(),
        };
        cloned_tx.txid().into()
    }

    /// Computes the txid: the double SHA-256 of the consensus serialization, which has no witness
    /// data.
    ///
    /// For an asset unlock with a version 2+ payload, the txid excludes the quorum signing info
    /// (`requestedHeight`, `quorumHash`, `quorumSig`, the last 132 serialized bytes), so every
    /// re-signed instance of one withdrawal shares one txid. See [`Self::instance_hash`] for the
    /// hash of the full serialization.
    ///
    /// This is the rule of Dash Core v24, which applies it to the bytes whether or not the V24
    /// deployment is active (see [`Self::has_stable_txid`]). Dash Core before v24 (v23.1.8, for
    /// one) hashes the full serialization of every transaction, so for these transactions its
    /// txid is [`Self::instance_hash`]; it also rejects asset unlock payloads above version 1, so
    /// they only occur on chains where V24 is active.
    ///
    /// # Panics
    ///
    /// If the special payload can't be encoded, which only a payload built in code can hit (a
    /// version 3 ProRegTx without `net_info`, a CbTx version 4 without its asset unlock root, ...):
    /// decoded transactions always encode.
    pub fn txid(&self) -> Txid {
        if !self.has_stable_txid() {
            return self.instance_hash();
        }
        let mut buf = Vec::with_capacity(self.size());
        self.encode_without_witness(&mut buf).expect("vectors don't error");
        let quorum_info_start = buf.len() - ASSET_UNLOCK_QUORUM_INFO_SIZE;
        buf[quorum_info_start..].fill(0);
        Txid::hash(&buf)
    }

    /// Computes the hash of the full serialization (without witnesses), Core's
    /// `GetInstanceHash()`. It equals the txid except for version 2+ asset unlocks, where it
    /// tells re-signed instances of one withdrawal apart. Core announces those by this hash
    /// (`MSG_ASSET_UNLOCK`), derives compact block short IDs from it and commits to it in the
    /// coinbase `merkleRootAssetUnlocks`. It is also the txid Dash Core before v24 computes.
    ///
    /// The quorum signs a withdrawal over the instance hash of the transaction with an empty
    /// `quorumSig`, not over the txid: anything that signs or verifies a version 2 asset unlock
    /// must use this hash.
    ///
    /// # Panics
    ///
    /// Like [`Self::txid`], if the special payload can't be encoded.
    pub fn instance_hash(&self) -> Txid {
        let mut enc = Txid::engine();
        self.encode_without_witness(&mut enc).expect("engines don't error");
        Txid::from_engine(enc)
    }

    /// Whether the txid excludes the quorum signing info: a special transaction version, type
    /// asset unlock and a 145-byte payload whose version byte is at least 2. Like Dash Core v24
    /// (`IsAssetUnlockWithStableTxid`), this looks at the bytes only, not at whether the V24
    /// deployment is active.
    pub fn has_stable_txid(&self) -> bool {
        if !self.has_special_payload() {
            return false;
        }
        match &self.special_transaction_payload {
            Some(TransactionPayload::AssetUnlockPayloadType(payload)) => {
                payload.base.version >= ASSET_UNLOCK_STABLE_TXID_VERSION
            }
            Some(TransactionPayload::UnknownPayloadType(9, bytes)) => {
                bytes.len() == ASSET_UNLOCK_PAYLOAD_SIZE
                    && bytes[0] >= ASSET_UNLOCK_STABLE_TXID_VERSION
            }
            _ => false,
        }
    }

    /// Whether the version is a special transaction version (3 or more as a signed 16-bit
    /// integer, Core's `IsSpecialTxVersion()`).
    pub fn is_special_version(&self) -> bool {
        is_special_version(self.version)
    }

    /// Whether the payload section is serialized: a special version with a non-zero type
    /// (Core's `HasExtraPayloadField()`).
    fn has_special_payload(&self) -> bool {
        self.is_special_version() && self.tx_type().to_u16() != 0
    }

    /// Writes the version and type as Core does: one 32-bit integer `nVersion | (nType << 16)`
    /// built from a signed 16-bit `nVersion` (match Core's serialization of nVersion/nType).
    fn encode_version_and_type<W: io::Write + ?Sized>(
        &self,
        w: &mut W,
    ) -> Result<usize, io::Error> {
        let n32bit_version =
            (self.version as i16 as i32 as u32) | ((self.tx_type().to_u16() as u32) << 16);
        n32bit_version.consensus_encode(w)
    }

    /// Writes the payload section: the length-prefixed payload of a special transaction.
    fn encode_special_payload<W: io::Write + ?Sized>(&self, w: &mut W) -> Result<usize, io::Error> {
        match &self.special_transaction_payload {
            Some(payload) if self.has_special_payload() => {
                let mut buf = Vec::with_capacity(payload.len());
                payload.consensus_encode(&mut buf)?;
                buf.consensus_encode(w)
            }
            _ => Ok(0),
        }
    }

    /// Writes the serialization without witnesses (Dash Core's serialization).
    fn encode_without_witness<W: io::Write + ?Sized>(&self, w: &mut W) -> Result<usize, io::Error> {
        let mut len = self.encode_version_and_type(w)?;
        len += self.input.consensus_encode(w)?;
        len += self.output.consensus_encode(w)?;
        len += self.lock_time.consensus_encode(w)?;
        len += self.encode_special_payload(w)?;
        Ok(len)
    }

    /// Get the transaction type. If a classical transaction this would be 0.
    /// Otherwise it is gotten by association from the payload type.
    pub fn tx_type(&self) -> TransactionType {
        TransactionType::from_optional_payload(&self.special_transaction_payload)
    }

    /// Computes the hash of the full consensus serialization, as a [`Wtxid`]. Dash has no
    /// witness data, so this is Core's `GetInstanceHash()`, the same value as
    /// [`Self::instance_hash`] (which returns it as a [`Txid`]; use that in new code).
    ///
    /// It equals [`Self::txid`] except for a version 2+ asset unlock, whose txid leaves out the
    /// quorum signing info (see [`Self::has_stable_txid`]).
    pub fn wtxid(&self) -> Wtxid {
        let mut enc = Wtxid::engine();
        self.consensus_encode(&mut enc).expect("engines don't error");
        Wtxid::from_engine(enc)
    }

    /// Encodes the signing data from which a signature hash for a given input index with a given
    /// sighash flag can be computed.
    ///
    /// To actually produce a scriptSig, this hash needs to be run through an ECDSA signer, the
    /// `EcdsaSighashType` appended to the resulting sig, and a script written around this, but
    /// this is the general (and hard) part.
    ///
    /// The `sighash_type` supports an arbitrary `u32` value, instead of just `EcdsaSighashType`,
    /// because internally 4 bytes are being hashed, even though only the lowest byte is appended to
    /// signature in a transaction.
    ///
    /// # Warning
    ///
    /// - Does NOT attempt to support OP_CODESEPARATOR. In general this would require evaluating
    ///   `script_pubkey` to determine which separators get evaluated and which don't, which we don't
    ///   have the information to determine.
    /// - Does NOT handle the sighash single bug, you should either handle that manually or use
    ///   `Self::signature_hash()` instead.
    ///
    /// # Panics
    ///
    /// If `input_index` is out of bounds (greater than or equal to `self.input.len()`).
    /// Encodes the signing data from which a signature hash for a given input index with a given
    /// sighash flag can be computed.
    ///
    /// To actually produce a scriptSig, this hash needs to be run through an ECDSA signer, the
    /// `EcdsaSighashType` appended to the resulting sig, and a script written around this, but
    /// this is the general (and hard) part.
    ///
    /// The `sighash_type` supports an arbitrary `u32` value, instead of just `EcdsaSighashType`,
    /// because internally 4 bytes are being hashed, even though only the lowest byte is appended to
    /// signature in a transaction.
    ///
    /// # Warning
    ///
    /// - Does NOT attempt to support OP_CODESEPARATOR. In general this would require evaluating
    ///   `script_pubkey` to determine which separators get evaluated and which don't, which we don't
    ///   have the information to determine.
    /// - Does NOT handle the sighash single bug (see "Returns" section)
    ///
    /// # Returns
    ///
    /// This function can't handle the SIGHASH_SINGLE bug internally, so it returns [`EncodeSigningDataResult`]
    /// that must be handled by the caller (see [`EncodeSigningDataResult::is_sighash_single_bug`]).
    ///
    /// # Panics
    ///
    /// If `input_index` is out of bounds (greater than or equal to `self.input.len()`).
    #[deprecated(
        since = "0.30.0",
        note = "Use SighashCache::legacy_encode_signing_data_to instead"
    )]
    pub fn encode_signing_data_to<Write: io::Write, U: Into<u32>>(
        &self,
        writer: Write,
        input_index: usize,
        script_pubkey: &Script,
        sighash_type: U,
    ) -> EncodeSigningDataResult<io::Error> {
        use EncodeSigningDataResult::*;

        use crate::sighash::{self, SighashCache};

        assert!(input_index < self.input.len()); // Panic on OOB

        let cache = SighashCache::new(self);
        match cache.legacy_encode_signing_data_to(writer, input_index, script_pubkey, sighash_type)
        {
            SighashSingleBug => SighashSingleBug,
            WriteResult(res) => match res {
                Ok(()) => WriteResult(Ok(())),
                Err(e) => match e {
                    sighash::Error::Io(e) => WriteResult(Err(e.into())),
                    _ => unreachable!("we check input_index above"),
                },
            },
        }
    }

    /// This will hash all input outpoints
    pub fn hash_inputs(&self) -> InputsHash {
        let mut enc = InputsHash::engine();
        for input in self.input.iter() {
            input.previous_output.consensus_encode(&mut enc).expect("engines don't error");
        }
        InputsHash::from_engine(enc)
    }

    /// Returns the "weight" of this transaction: the consensus-serialized size times four, as
    /// witnesses are never serialized.
    #[inline]
    pub fn weight(&self) -> Weight {
        Weight::from_wu(self.scaled_size(WITNESS_SCALE_FACTOR) as u64)
    }
    /// Returns the regular byte-wise consensus-serialized size of this transaction.
    #[inline]
    #[deprecated(since = "0.28.0", note = "Please use `transaction::size` instead.")]
    pub fn get_size(&self) -> usize {
        self.size()
    }

    /// Returns the regular byte-wise consensus-serialized size of this transaction.
    #[inline]
    pub fn size(&self) -> usize {
        self.scaled_size(1)
    }

    /// Returns the "virtual size" (vsize) of this transaction.
    #[inline]
    #[deprecated(since = "0.28.0", note = "Please use `transaction::vsize` instead.")]
    pub fn get_vsize(&self) -> usize {
        self.vsize()
    }

    /// Returns the "virtual size" (vsize) of this transaction.
    ///
    /// Will be `ceil(weight / 4.0)`. Note this implements the virtual size as per [`BIP141`], which
    /// is different to what is implemented in Bitcoin Core. The computation should be the same for
    /// any remotely sane transaction, and a standardness-rule-correct version is available in the
    /// [`policy`] module.
    ///
    /// [`BIP141`]: https://github.com/bitcoin/bips/blob/master/bip-0141.mediawiki
    /// [`policy`]: ../policy/mod.rs.html
    #[inline]
    pub fn vsize(&self) -> usize {
        // No overflow because it's computed from data in memory
        self.weight().to_vbytes_ceil() as usize
    }

    /// Returns the size of this transaction excluding the witness data.
    #[deprecated(since = "0.28.0", note = "Please use `transaction::strippedsize` instead.")]
    pub fn get_strippedsize(&self) -> usize {
        self.strippedsize()
    }

    /// Returns the size of this transaction excluding the witness data.
    pub fn strippedsize(&self) -> usize {
        let mut input_size = 0;
        for input in &self.input {
            input_size += 32 + 4 + 4 + // outpoint (32+4) + nSequence
                VarInt(input.script_sig.len() as u64).len() +
                input.script_sig.len();
        }
        let mut output_size = 0;
        for output in &self.output {
            output_size += 8 + // value
                VarInt(output.script_pubkey.len() as u64).len() +
                output.script_pubkey.len();
        }
        let special_tx_len = self.special_transaction_len();
        let non_input_size =
            // version:
            4 +
                // count varints:
                VarInt(self.input.len() as u64).len() +
                VarInt(self.output.len() as u64).len() +
                output_size +
                // lock_time
                4 +
                special_tx_len;
        non_input_size + input_size
    }

    /// Internal utility function for size/weight functions.
    ///
    /// Witnesses are never serialized, so every byte counts `scale_factor` times.
    fn scaled_size(&self, scale_factor: usize) -> usize {
        let mut input_size = 0;
        for input in &self.input {
            input_size += 32 + 4 + 4 + // outpoint (32+4) + nSequence
                VarInt(input.script_sig.len() as u64).len() +
                input.script_sig.len();
        }
        let mut output_size = 0;
        for output in &self.output {
            output_size += 8 + // value
                VarInt(output.script_pubkey.len() as u64).len() +
                output.script_pubkey.len();
        }
        let special_tx_len = self.special_transaction_len();
        let non_input_size =
            // version:
            4 +
                // count varints:
                VarInt(self.input.len() as u64).len() +
                VarInt(self.output.len() as u64).len() +
                output_size +
                // lock_time
                4 +
                special_tx_len;
        (non_input_size + input_size) * scale_factor
    }

    /// Returns the length of the serialized payload section (payload and length prefix), if
    /// any.
    pub fn special_transaction_len(&self) -> usize {
        match self.special_transaction_payload.as_ref() {
            // A non-special-version type kept on a special version still gets an empty
            // payload section (a zero length prefix).
            Some(TransactionPayload::ClassicalWithNonStandardVersionTypeBytesPayloadType(_))
                if self.has_special_payload() =>
            {
                1
            }
            Some(payload) if self.has_special_payload() => payload.len(),
            _ => 0,
        }
    }

    /// Shorthand for [`Self::verify_with_flags`] with flag [`bitcoinconsensus::VERIFY_ALL`].
    #[cfg(feature = "bitcoinconsensus")]
    pub fn verify<S>(&self, spent: S) -> Result<(), script::Error>
    where
        S: FnMut(&OutPoint) -> Option<TxOut>,
    {
        self.verify_with_flags(spent, ::bitcoinconsensus::VERIFY_ALL)
    }

    /// Verify that this transaction is able to spend its inputs.
    /// The `spent` closure should not return the same [`TxOut`] twice!
    #[cfg(feature = "bitcoinconsensus")]
    pub fn verify_with_flags<S, F>(&self, mut spent: S, flags: F) -> Result<(), script::Error>
    where
        S: FnMut(&OutPoint) -> Option<TxOut>,
        F: Into<u32>,
    {
        let tx = encode::serialize(self);
        let flags: u32 = flags.into();
        for (idx, input) in self.input.iter().enumerate() {
            if let Some(output) = spent(&input.previous_output) {
                output.script_pubkey.verify_with_flags(
                    idx,
                    crate::Amount::from_sat(output.value),
                    tx.as_slice(),
                    flags,
                )?;
            } else {
                return Err(script::Error::UnknownSpentOutput(input.previous_output));
            }
        }
        Ok(())
    }

    /// Is this a coin base transaction?
    pub fn is_coin_base(&self) -> bool {
        self.input.len() == 1 && self.input[0].previous_output.is_null()
    }

    /// Returns `true` if the transaction itself opted in to be BIP-125-replaceable (RBF). This
    /// **does not** cover the case where a transaction becomes replaceable due to ancestors being
    /// RBF.
    pub fn is_explicitly_rbf(&self) -> bool {
        self.input.iter().any(|input| input.sequence < (0xffffffff - 1))
    }

    /// Adds an output that burns Dash. Used to top up a Dash Identity;
    /// accepts hash of the public key to prove ownership of the burnt
    /// dash on Dash Platform.
    pub fn add_burn_output(&mut self, satoshis_to_burn: u64, data: &[u8; 20]) {
        let burn_script = ScriptBuf::new_op_return(data);
        let output = TxOut {
            value: satoshis_to_burn,
            script_pubkey: burn_script,
        };
        self.output.push(output)
    }

    /// Gives an OutPoint buffer for the output at a given index
    pub fn out_point_buffer(&self, output_index: usize) -> Option<[u8; 36]> {
        self.output.get(output_index).map(|_a| {
            let mut result: [u8; 36] = [0; 36];
            let hash = self.txid();

            let (one, two) = result.split_at_mut(32);
            one.copy_from_slice(hash.as_raw_hash().as_byte_array().as_slice());
            let output_index_bytes: [u8; 4] = (output_index as u32).to_le_bytes();
            two.copy_from_slice(&output_index_bytes);
            result
        })
    }

    /// Computes the weight and checks that it matches the output of `predict_weight`.
    #[cfg(test)]
    fn check_weight(&self) -> Weight {
        let weight1 = self.weight();
        let inputs = self.input.iter().map(|txin| {
            InputWeightPrediction::new(
                txin.script_sig.len(),
                txin.witness.iter().map(|elem| elem.len()),
            )
        });
        let outputs = self.output.iter().map(|txout| txout.script_pubkey.len());
        let weight2 = predict_weight(inputs, outputs);
        assert_eq!(weight1, weight2);
        weight1
    }
}

impl Encodable for Transaction {
    fn consensus_encode<W: io::Write + ?Sized>(&self, w: &mut W) -> Result<usize, io::Error> {
        self.encode_without_witness(w)
    }
}

impl Decodable for Transaction {
    fn consensus_decode_from_finite_reader<R: io::Read + ?Sized>(
        r: &mut R,
    ) -> Result<Self, encode::Error> {
        // One 32-bit integer: a signed 16-bit version and a 16-bit type (DIP-0002).
        let n32bit_version = u32::consensus_decode_from_finite_reader(r)?;
        let version = n32bit_version as u16;
        let special_transaction_type_u16 = (n32bit_version >> 16) as u16;
        let special_transaction_type = if special_transaction_type_u16 == 0 {
            TransactionType::Classic
        } else if is_special_version(version) {
            // Keep types we don't know as raw payloads, so a block carrying a newer special
            // transaction type still decodes.
            TransactionType::try_from(special_transaction_type_u16)
                .unwrap_or(TransactionType::Unknown(special_transaction_type_u16))
        } else {
            // Core reads no payload below a special version: such transactions are logically
            // Classic, but at least one mainnet tx put non-zero bytes in the type slot. Preserve
            // the raw u16 so consensus_encode/txid keep matching the on-chain bytes.
            TransactionType::ClassicalWithNonStandardVersionTypeBytes(special_transaction_type_u16)
        };
        // Dash Core's serialization has no segwit marker or flag (no BIP144): the inputs, then the
        // outputs, whether or not there are any inputs.
        let input = Vec::<TxIn>::consensus_decode_from_finite_reader(r)?;
        Ok(Transaction {
            version,
            input,
            output: Decodable::consensus_decode_from_finite_reader(r)?,
            lock_time: Decodable::consensus_decode_from_finite_reader(r)?,
            special_transaction_payload: special_transaction_type.consensus_decode(r)?,
        })
    }
}

/// Whether `version` is a special transaction version: 3 or more as a signed 16-bit integer.
fn is_special_version(version: u16) -> bool {
    version as i16 >= 3
}

/// Weight prediction of an individual input.
///
/// This helper type collects information about an input to be used in [`predict_weight`] function.
/// It can only be created using the [`new`](InputWeightPrediction::new) function.
///
/// Dash has no segregated witness: a transaction serializes its inputs and outputs and nothing
/// else (`CTransaction` in `primitives/transaction.h`), so the weight of an input is four times the
/// size of its outpoint, script and sequence, whatever witness data is passed in.
#[derive(Copy, Clone, Debug)]
pub struct InputWeightPrediction {
    script_size: usize,
}

impl InputWeightPrediction {
    /// A Bitcoin P2WPKH spend. Dash has no segwit, so no such input exists and this predicts an
    /// input with an empty script.
    #[deprecated(
        note = "Dash has no segregated witness; use `InputWeightPrediction::new` with the length of the script_sig"
    )]
    pub const P2WPKH_MAX: Self = InputWeightPrediction {
        script_size: 0,
    };

    /// A Bitcoin taproot key spend. Dash has no segwit, so no such input exists and this predicts
    /// an input with an empty script.
    #[deprecated(
        note = "Dash has no segregated witness; use `InputWeightPrediction::new` with the length of the script_sig"
    )]
    pub const P2TR_KEY_DEFAULT_SIGHASH: Self = InputWeightPrediction {
        script_size: 0,
    };

    /// A Bitcoin taproot key spend with a non-default sighash. Dash has no segwit, so no such input
    /// exists and this predicts an input with an empty script.
    #[deprecated(
        note = "Dash has no segregated witness; use `InputWeightPrediction::new` with the length of the script_sig"
    )]
    pub const P2TR_KEY_NON_DEFAULT_SIGHASH: Self = InputWeightPrediction {
        script_size: 0,
    };

    /// A Bitcoin P2WPKH spend with signature grinding. Dash has no segwit, so no such input exists
    /// and this predicts an input with an empty script.
    ///
    /// # Panics
    ///
    /// The function panics in const context and debug builds if `bytes_to_grind` is higher than 62.
    #[deprecated(
        note = "Dash has no segregated witness; use `InputWeightPrediction::new` with the length of the script_sig"
    )]
    pub const fn ground_p2wpkh(bytes_to_grind: usize) -> Self {
        // Written to trigger const/debug panic for unreasonably high values.
        let _der_signature_size = 10 + (62 - bytes_to_grind);
        InputWeightPrediction {
            script_size: 0,
        }
    }

    /// Computes the prediction for a single input from the length of its `script_sig`.
    ///
    /// The lengths of the witness elements are accepted for compatibility and ignored: Dash does
    /// not serialize witnesses, so they add nothing to the weight.
    pub fn new<T>(input_script_len: usize, witness_element_lengths: T) -> Self
    where
        T: IntoIterator,
        T::Item: Borrow<usize>,
    {
        let _ = witness_element_lengths;
        let script_size = input_script_len + VarInt(input_script_len as u64).len();

        InputWeightPrediction {
            script_size,
        }
    }

    /// Computes the prediction for a single input in `const` context.
    ///
    /// This is a `const` version of [`new`](Self::new) which only allows slices due to current Rust
    /// limitations around `const fn`. The witness element lengths are ignored, as in `new`.
    pub const fn from_slice(input_script_len: usize, witness_element_lengths: &[usize]) -> Self {
        let _ = witness_element_lengths;
        let script_size = input_script_len + VarInt(input_script_len as u64).len();

        InputWeightPrediction {
            script_size,
        }
    }
}

/// Predicts the weight of a to-be-constructed transaction.
///
/// This function computes the weight of a transaction which is not fully known. All that is needed
/// is the lengths of the scripts. The result is four times the serialized size, which is what
/// [`Transaction::weight`] returns for the finished transaction; Dash serializes no witnesses.
///
/// # Arguments
///
/// * `inputs` - an iterator which returns `InputWeightPrediction` for each input of the
///   to-be-constructed transaction.
/// * `output_script_lens` - an iterator which returns the length of `script_pubkey` of each output
///   of the to-be-constructed transaction.
///
/// Note that lengths of the scripts must be non-serialized, IOW *without* the preceding compact
/// size. The length of preceding compact size is computed and added inside the function for
/// convenience.
///
/// The size of a special transaction payload is not predicted: add it to the result.
///
/// # Usage
///
/// When signing a transaction one doesn't know the signature before knowing the transaction fee and
/// the transaction fee is not known before knowing the transaction size which is not known before
/// knowing the signature. This apparent dependency cycle can be broken by knowing the length of the
/// signature without knowing the contents of the signature e.g., we know the length of a compact
/// ECDSA signature.
///
/// Additionally, some protocols may require calculating the amounts before knowing various parts
/// of the transaction (assuming their length is known).
///
/// # Notes on integer overflow
///
/// Overflows are intentionally not checked because one of the following holds:
///
/// * The transaction is valid (obeys the block size limit) and the code feeds correct values to
///   this function - no overflow can happen.
/// * The transaction will be so large it doesn't fit in the memory - overflow will happen but
///   then the transaction will fail to construct and even if one serialized it on disk directly
///   it'd be invalid anyway so overflow doesn't matter.
/// * The values fed into this function are inconsistent with the actual lengths the transaction
///   will have - the code is already broken and checking overflows doesn't help. Unfortunately
///   this probably cannot be avoided.
pub fn predict_weight<I, O>(inputs: I, output_script_lens: O) -> Weight
where
    I: IntoIterator<Item = InputWeightPrediction>,
    O: IntoIterator<Item = usize>,
{
    let (input_count, input_scripts_size) =
        inputs.into_iter().fold((0, 0), |(count, scripts_size), prediction| {
            (count + 1, scripts_size + prediction.script_size)
        });
    let (output_count, output_scripts_size) = output_script_lens.into_iter().fold(
        (0, 0),
        |(output_count, total_scripts_size), script_len| {
            let script_size = script_len + VarInt(script_len as u64).len();
            (output_count + 1, total_scripts_size + script_size)
        },
    );
    predict_weight_internal(input_count, input_scripts_size, output_count, output_scripts_size)
}

const fn predict_weight_internal(
    input_count: usize,
    input_scripts_size: usize,
    output_count: usize,
    output_scripts_size: usize,
) -> Weight {
    let input_size = input_scripts_size + input_count * (32 + 4 + 4);
    let output_size = 8 * output_count + output_scripts_size;
    let size =
        // version:
        4 +
            // count varints:
            VarInt(input_count as u64).len() +
            VarInt(output_count as u64).len() +
            input_size +
            output_size +
            // lock_time
            4;
    Weight::from_wu((size * 4) as u64)
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::blockdata::constants::WITNESS_SCALE_FACTOR;
    use crate::consensus::encode::{deserialize, deserialize_partial, serialize};
    use crate::internal_macros::hex;
    use crate::network::message::{NetworkMessage, RawNetworkMessage};

    #[test]
    fn test_is_coinbase() {
        use crate::Network;
        use crate::blockdata::constants;

        let genesis = constants::genesis_block(Network::Mainnet);
        assert!(genesis.txdata[0].is_coin_base());
        let tx_bytes = hex_conservative::decode_to_vec("0100000001a15d57094aa7a21a28cb20b59aab8fc7d1149a3bdbcddba9c622e4f5f6a99ece010000006c493046022100f93bb0e7d8db7bd46e40132d1f8242026e045f03a0efe71bbb8e3f475e970d790221009337cd7f1f929f00cc6ff01f03729b069a7c21b59b1736ddfee5db5946c5da8c0121033b9b137ee87d5a812d6f506efdd37f0affa7ffc310711c06c7f3e097c9447c52ffffffff0100e1f505000000001976a9140389035a9225b3839e2bbf32d826a1e222031fd888ac00000000").unwrap();
        let tx: Transaction = deserialize(&tx_bytes).unwrap();
        assert!(!tx.is_coin_base());
    }

    #[test]
    fn test_nonsegwit_transaction() {
        let tx_bytes = hex!(
            "0100000001a15d57094aa7a21a28cb20b59aab8fc7d1149a3bdbcddba9c622e4f5f6a99ece010000006c493046022100f93bb0e7d8db7bd46e40132d1f8242026e045f03a0efe71bbb8e3f475e970d790221009337cd7f1f929f00cc6ff01f03729b069a7c21b59b1736ddfee5db5946c5da8c0121033b9b137ee87d5a812d6f506efdd37f0affa7ffc310711c06c7f3e097c9447c52ffffffff0100e1f505000000001976a9140389035a9225b3839e2bbf32d826a1e222031fd888ac00000000"
        );
        let tx: Result<Transaction, _> = deserialize(&tx_bytes);
        assert!(tx.is_ok());
        let realtx = tx.unwrap();
        // All these tests aren't really needed because if they fail, the hash check at the end
        // will also fail. But these will show you where the failure is so I'll leave them in.
        assert_eq!(realtx.version, 1);
        assert_eq!(realtx.input.len(), 1);
        // In particular this one is easy to get backward -- in dash hashes are encoded
        // as little-endian 256-bit numbers rather than as data strings.
        assert_eq!(
            format!("{:x}", realtx.input[0].previous_output.txid),
            "ce9ea9f6f5e422c6a9dbcddb3b9a14d1c78fab9ab520cb281aa2a74a09575da1".to_string()
        );
        assert_eq!(realtx.input[0].previous_output.vout, 1);
        assert_eq!(realtx.output.len(), 1);
        assert_eq!(realtx.lock_time, 0);

        assert_eq!(
            format!("{:x}", realtx.txid()),
            "a6eab3c14ab5272a58a5ba91505ba1a4b6d7a3a9fcbd187b6cd99a7b6d548cb7".to_string()
        );
        assert_eq!(
            format!("{:x}", realtx.wtxid()),
            "a6eab3c14ab5272a58a5ba91505ba1a4b6d7a3a9fcbd187b6cd99a7b6d548cb7".to_string()
        );
        assert_eq!(realtx.check_weight().to_wu() as usize, tx_bytes.len() * WITNESS_SCALE_FACTOR);
        assert_eq!(realtx.size(), tx_bytes.len());
        assert_eq!(realtx.vsize(), tx_bytes.len());
        assert_eq!(realtx.strippedsize(), tx_bytes.len());
    }

    /// Dash has no BIP144: a witness in memory never reaches the wire, whatever the transaction
    /// looks like.
    #[test]
    fn witness_is_never_serialized() {
        let input_with_witness = |witness: Witness| TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([7; 32]), 1),
            script_sig: ScriptBuf::new(),
            sequence: 0xffff_fffe,
            witness,
        };
        let witness = Witness::from_slice(&[vec![1u8, 2, 3], vec![4u8; 33]]);
        let output = vec![TxOut {
            value: 50_000,
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        }];
        for inputs in [
            vec![input_with_witness(witness.clone())],
            vec![input_with_witness(Witness::new()), input_with_witness(witness.clone())],
            vec![],
        ] {
            let mut tx = Transaction {
                version: 2,
                lock_time: 0,
                input: inputs,
                output: output.clone(),
                special_transaction_payload: None,
            };
            let with_witness = serialize(&tx);
            assert_eq!(tx.size(), with_witness.len());
            assert_eq!(tx.vsize(), with_witness.len());
            tx.input.iter_mut().for_each(|input| input.witness.clear());
            let without_witness = serialize(&tx);
            assert_eq!(with_witness, without_witness);
            // version, input count, ...: no 0x00 marker and 0x01 flag after the version
            let expected_count = tx.input.len() as u8;
            assert_eq!(with_witness[4], expected_count);
            assert_eq!(tx.size(), with_witness.len());
            assert_eq!(tx.wtxid().to_raw_hash(), tx.txid().to_raw_hash());
            // And it reads back as the transaction without witnesses.
            assert_eq!(deserialize::<Transaction>(&with_witness).unwrap(), tx);
        }
    }

    #[test]
    fn a_transaction_without_inputs_weighs_four_times_its_size() {
        // Version 1, no inputs, no outputs, lock time 0. The old BIP144 reading took the two zero
        // bytes for a marker and flag; the weight came out eight units short.
        let bytes = hex!("01000000000000000000");
        let tx: Transaction = deserialize(&bytes).unwrap();
        assert!(tx.input.is_empty() && tx.output.is_empty());
        assert_eq!(serialize(&tx), bytes);
        assert_eq!(tx.size(), 10);
        assert_eq!(tx.weight().to_wu(), 40);
        assert_eq!(tx.check_weight().to_wu(), 40);
    }

    #[test]
    fn predicted_weight_equals_weight_whatever_the_witness() {
        let output = vec![
            TxOut {
                value: 1,
                script_pubkey: ScriptBuf::from(vec![0x51; 25]),
            },
            TxOut {
                value: 2,
                script_pubkey: ScriptBuf::from(vec![0x52; 300]),
            },
        ];
        let input = |script_len: usize, witness: &[&[u8]]| TxIn {
            previous_output: OutPoint::default(),
            script_sig: ScriptBuf::from(vec![0x53; script_len]),
            sequence: 0xffff_fffe,
            witness: Witness::from_slice(witness),
        };
        for inputs in [
            vec![],
            vec![input(0, &[])],
            vec![input(107, &[]), input(0, &[&[1u8; 72], &[2u8; 33]])],
            vec![input(300, &[&[3u8; 1000]]), input(252, &[&[4u8; 5], &[5u8; 0], &[6u8; 70000]])],
        ] {
            let tx = Transaction {
                version: 2,
                lock_time: 0,
                input: inputs,
                output: output.clone(),
                special_transaction_payload: None,
            };
            let serialized = serialize(&tx).len();
            let predicted = predict_weight(
                tx.input.iter().map(|txin| {
                    InputWeightPrediction::new(
                        txin.script_sig.len(),
                        txin.witness.iter().map(|elem| elem.len()),
                    )
                }),
                tx.output.iter().map(|txout| txout.script_pubkey.len()),
            );
            assert_eq!(predicted, tx.weight());
            assert_eq!(predicted.to_wu() as usize, 4 * serialized);
            // The const constructor ignores the witness too.
            let lens: Vec<Vec<usize>> = tx
                .input
                .iter()
                .map(|txin| txin.witness.iter().map(|e| e.len()).collect())
                .collect();
            let from_slice = predict_weight(
                tx.input.iter().zip(&lens).map(|(txin, lens)| {
                    InputWeightPrediction::from_slice(txin.script_sig.len(), lens)
                }),
                tx.output.iter().map(|txout| txout.script_pubkey.len()),
            );
            assert_eq!(from_slice, predicted);
        }
    }

    #[test]
    fn test_transaction_version() {
        let tx_bytes = hex_conservative::decode_to_vec("ffff00000100000000000000000000000000000000000000000000000000000000000000000000000000ffffffff0100f2052a01000000434104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac00000000").unwrap();
        let tx: Result<Transaction, _> = deserialize(&tx_bytes);
        assert!(tx.is_ok());
        let realtx = tx.unwrap();
        assert_eq!(realtx.version, 65535);

        let tx2_bytes = hex_conservative::decode_to_vec("000000000100000000000000000000000000000000000000000000000000000000000000000000000000ffffffff0100f2052a01000000434104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac00000000").unwrap();
        let tx2: Result<Transaction, _> = deserialize(&tx2_bytes);
        assert!(tx2.is_ok());
        let realtx2 = tx2.unwrap();
        assert_eq!(realtx2.version, 0);
    }

    #[test]
    fn test_pre_dip2_classical_with_non_standard_version_type_bytes_roundtrip() {
        use crate::blockdata::transaction::special_transaction::{
            TransactionPayload, TransactionType,
        };

        // Same tx as `test_transaction_version`'s second case, but with the on-wire
        // `nTxType` slot set to a non-zero u16 (0x2A in little-endian => 0x002A).
        // Pre-DIP-0002 these bytes were ignored by consensus but still part of the
        // serialized transaction and therefore part of the txid pre-image. Bytes 2..4
        // are the `nTxType` field; bytes 0..2 are version=0.
        let tx_bytes = hex_conservative::decode_to_vec("00002a000100000000000000000000000000000000000000000000000000000000000000000000000000ffffffff0100f2052a01000000434104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac00000000").unwrap();
        let tx: Transaction =
            deserialize(&tx_bytes).expect("decoder must accept pre-DIP-0002 raw type bytes");
        assert_eq!(tx.version, 0);
        assert_eq!(
            tx.tx_type(),
            TransactionType::ClassicalWithNonStandardVersionTypeBytes(0x002A),
            "raw on-wire u16 must be preserved on the decoded type"
        );
        assert!(matches!(
            tx.special_transaction_payload,
            Some(TransactionPayload::ClassicalWithNonStandardVersionTypeBytesPayloadType(0x002A))
        ));

        // Round-trip: re-serializing must reproduce the original bytes.
        let reser = serialize(&tx);
        assert_eq!(reser, tx_bytes, "consensus_encode must round-trip raw type bytes");

        // The txid must match the txid computed from the original on-wire bytes
        // (double-SHA256 of the serialized transaction). Since the encoder now
        // reproduces the input bytes exactly, the txid is the hash of those bytes.
        assert_eq!(tx.txid(), Txid::hash(&tx_bytes), "txid must hash the original bytes");

        // The txid must differ from the same transaction with type bytes zeroed —
        // proves we no longer collapse different on-chain transactions to one id.
        let mut zero_type_bytes = tx_bytes.clone();
        zero_type_bytes[2] = 0;
        zero_type_bytes[3] = 0;
        let zero_type_tx: Transaction = deserialize(&zero_type_bytes).unwrap();
        assert_ne!(
            tx.txid(),
            zero_type_tx.txid(),
            "txids of pre-DIP-0002 txs must depend on the raw type bytes"
        );
    }

    // TODO: these tests build the unknown-type transaction by hand. Add a real regtest
    // ProDisTx (type 10) as a regression vector once #1131 parses types 10-12.
    fn unknown_special_tx(input: Vec<TxIn>) -> Transaction {
        Transaction {
            version: 3,
            lock_time: 0,
            input,
            output: vec![TxOut {
                value: 1_000,
                script_pubkey: ScriptBuf::new_op_return(&[0xab; 4]),
            }],
            special_transaction_payload: Some(TransactionPayload::UnknownPayloadType(
                13,
                vec![0x01, 0x00, 0xde, 0xad, 0xbe, 0xef],
            )),
        }
    }

    #[test]
    fn unknown_special_transaction_type_round_trips() {
        let tx = unknown_special_tx(vec![TxIn::default()]);
        let bytes = serialize(&tx);

        // nTxType 13 on the wire, and the payload is its length prefix plus the raw bytes.
        assert_eq!(&bytes[2..4], &[0x0d, 0x00]);
        assert_eq!(&bytes[bytes.len() - 7..], &[0x06, 0x01, 0x00, 0xde, 0xad, 0xbe, 0xef]);

        let decoded: Transaction = deserialize(&bytes).expect("unknown types must decode");
        assert_eq!(decoded.tx_type(), TransactionType::Unknown(13));
        assert_eq!(decoded, tx);
        assert_eq!(serialize(&decoded), bytes);
        assert_eq!(decoded.txid(), Txid::hash(&bytes));
        assert_eq!(decoded.size(), bytes.len());
    }

    #[test]
    fn unknown_special_transaction_type_keeps_the_stream_aligned() {
        // Back to back, as inside a block: the transaction after the unknown one must
        // decode from the right offset.
        let unknown = unknown_special_tx(vec![TxIn::default()]);
        let classic = Transaction {
            special_transaction_payload: None,
            ..unknown_special_tx(vec![TxIn::default()])
        };
        let mut bytes = serialize(&unknown);
        bytes.extend(serialize(&classic));

        let mut cursor = io::Cursor::new(&bytes);
        assert_eq!(Transaction::consensus_decode(&mut cursor).unwrap(), unknown);
        assert_eq!(Transaction::consensus_decode(&mut cursor).unwrap(), classic);
        assert_eq!(cursor.position() as usize, bytes.len());
    }

    #[test]
    fn unknown_special_transaction_type_without_inputs_is_not_segwit() {
        let tx = unknown_special_tx(vec![]);
        let bytes = serialize(&tx);
        let decoded: Transaction = deserialize(&bytes).expect("input-less unknown type decodes");
        assert_eq!(decoded, tx);
        assert_eq!(serialize(&decoded), bytes);
    }

    #[test]
    fn tx_no_input_deserialization() {
        // Dash Core's serialization of a transaction with no input and one output: the input
        // count 00, the output count 01, the output, the lock time. There is no marker and flag.
        let tx_bytes = hex_conservative::decode_to_vec(
            "01000000\
             00\
             01\
             00e1f50500000000\
             1976a9140389035a9225b3839e2bbf32d826a1e222031fd888ac\
             00000000",
        )
        .unwrap();
        let tx: Transaction = deserialize(&tx_bytes).expect("deserialize tx");

        assert_eq!(tx.input.len(), 0);
        assert_eq!(tx.output.len(), 1);

        let reser = serialize(&tx);
        assert_eq!(tx_bytes, reser);
    }

    #[test]
    fn non_special_version_with_type_bits_has_no_payload() {
        // Core reads a payload only from version 3: a version 2 transaction with type bits set
        // is a plain transaction whose type bytes must be kept for its txid.
        let classic = Transaction {
            special_transaction_payload: None,
            ..unknown_special_tx(vec![TxIn::default()])
        };
        let mut bytes = serialize(&Transaction {
            version: 2,
            ..classic.clone()
        });
        bytes[2] = 0x05;
        let next = serialize(&classic);
        let mut stream = bytes.clone();
        stream.extend(&next);

        let mut cursor = io::Cursor::new(&stream);
        let decoded = Transaction::consensus_decode(&mut cursor).unwrap();
        assert_eq!(decoded.tx_type(), TransactionType::ClassicalWithNonStandardVersionTypeBytes(5));
        assert!(!decoded.is_special_version());
        assert_eq!(Transaction::consensus_decode(&mut cursor).unwrap(), classic);
        assert_eq!(cursor.position() as usize, stream.len());
        assert_eq!(serialize(&decoded), bytes);
        assert_eq!(decoded.txid(), Txid::hash(&bytes));
        assert_eq!(decoded.size(), bytes.len());
    }

    #[test]
    fn special_payload_is_read_through_its_length_prefix() {
        use crate::transaction::special_transaction::coinbase::CoinbasePayload;

        let coinbase = CoinbasePayload {
            version: 2,
            height: 1,
            merkle_root_masternode_list: Hash::all_zeros(),
            merkle_root_quorums: Hash::all_zeros(),
            best_cl_height: None,
            best_cl_signature: None,
            asset_locked_amount: None,
            merkle_root_asset_unlocks: None,
        };
        let tx = Transaction {
            special_transaction_payload: Some(TransactionPayload::CoinbasePayloadType(coinbase)),
            ..unknown_special_tx(vec![TxIn::default()])
        };
        assert_eq!(deserialize::<Transaction>(&serialize(&tx)).unwrap(), tx);

        // Payloads that don't decode exactly as their type (a trailing byte, a payload version
        // this crate doesn't know) are kept raw; the stream stays aligned and the bytes and txid
        // are preserved.
        let mut with_trailing_byte = serialize(tx.special_transaction_payload.as_ref().unwrap());
        with_trailing_byte.push(0xff);
        let mut truncated_payload = serialize(tx.special_transaction_payload.as_ref().unwrap());
        truncated_payload.truncate(truncated_payload.len() - 32);
        for (tx_type, raw) in [(5, with_trailing_byte), (2, vec![0x04, 0x00, 0x01, 0x02])] {
            let raw_tx = Transaction {
                special_transaction_payload: Some(TransactionPayload::UnknownPayloadType(
                    tx_type,
                    raw.clone(),
                )),
                ..tx.clone()
            };
            let bytes = serialize(&raw_tx);
            let mut stream = bytes.clone();
            stream.extend(serialize(&tx));
            let mut cursor = io::Cursor::new(&stream);
            let decoded = Transaction::consensus_decode(&mut cursor).unwrap();
            assert_eq!(decoded.tx_type(), TransactionType::Unknown(tx_type));
            assert_eq!(decoded, raw_tx);
            assert_eq!(Transaction::consensus_decode(&mut cursor).unwrap(), tx);
            assert_eq!(cursor.position() as usize, stream.len());
            assert_eq!(decoded.txid(), Txid::hash(&bytes));
        }
        // A CbTx version 2 cut short of its quorum root no longer decodes as one either.
        let mut truncated = tx.clone();
        truncated.special_transaction_payload =
            Some(TransactionPayload::UnknownPayloadType(5, truncated_payload));
        let decoded: Transaction = deserialize(&serialize(&truncated)).unwrap();
        assert_eq!(decoded, truncated);
    }

    #[test]
    fn size_matches_encoding_for_type_bytes_kept_on_a_special_version() {
        let mut tx = unknown_special_tx(vec![TxIn::default()]);
        tx.special_transaction_payload =
            Some(TransactionPayload::ClassicalWithNonStandardVersionTypeBytesPayloadType(5));
        assert_eq!(tx.size(), serialize(&tx).len());
    }

    #[test]
    fn size_and_weight_leave_out_witnesses_special_transactions_do_not_serialize() {
        let mut input = TxIn::default();
        input.witness.push([1, 2, 3]);
        let mut tx = unknown_special_tx(vec![input]);
        tx.special_transaction_payload = Some(TransactionPayload::UnknownPayloadType(13, vec![7]));
        let encoded = serialize(&tx);
        assert_eq!(tx.size(), encoded.len());
        assert_eq!(tx.weight(), Weight::from_wu(4 * encoded.len() as u64));
    }

    #[test]
    fn size_counts_the_whole_payload_length_prefix() {
        let mut tx = unknown_special_tx(vec![TxIn::default()]);
        tx.special_transaction_payload =
            Some(TransactionPayload::UnknownPayloadType(13, vec![7; 300]));
        assert_eq!(tx.size(), serialize(&tx).len());
        assert_eq!(tx.special_transaction_len(), 3 + 300);
    }

    fn asset_unlock_tx(payload_version: u8, quorum_sig: [u8; 96]) -> Transaction {
        use crate::bls_sig_utils::BLSSignature;
        use crate::transaction::special_transaction::asset_unlock::qualified_asset_unlock::AssetUnlockPayload;
        use crate::transaction::special_transaction::asset_unlock::request_info::AssetUnlockRequestInfo;
        use crate::transaction::special_transaction::asset_unlock::unqualified_asset_unlock::AssetUnlockBasePayload;

        Transaction {
            version: 3,
            lock_time: 0,
            input: vec![],
            output: vec![TxOut {
                value: 100_000,
                script_pubkey: ScriptBuf::new_op_return(&[0xab; 4]),
            }],
            special_transaction_payload: Some(TransactionPayload::AssetUnlockPayloadType(
                AssetUnlockPayload {
                    base: AssetUnlockBasePayload {
                        version: payload_version,
                        index: 801,
                        fee: 70_000,
                    },
                    request_info: AssetUnlockRequestInfo {
                        request_height: 968,
                        quorum_hash: Hash::from_byte_array([0x5f; 32]),
                    },
                    quorum_sig: BLSSignature::from(quorum_sig),
                },
            )),
        }
    }

    #[test]
    fn asset_unlock_v2_txid_excludes_quorum_signing_info() {
        let tx = asset_unlock_tx(2, [0xac; 96]);
        let bytes = serialize(&tx);
        let mut stable = bytes.clone();
        let len = stable.len();
        stable[len - 132..].fill(0);
        assert!(tx.has_stable_txid());
        assert_eq!(tx.txid(), Txid::hash(&stable));
        assert_eq!(tx.instance_hash(), Txid::hash(&bytes));
        // `wtxid` is the full-serialization hash, Core's `GetInstanceHash()`, not the txid.
        assert_eq!(tx.wtxid().to_raw_hash(), tx.instance_hash().to_raw_hash());
        assert_ne!(tx.wtxid().to_raw_hash(), tx.txid().to_raw_hash());

        // A re-signed instance keeps the txid but not the instance hash.
        let resigned = asset_unlock_tx(2, [0xbd; 96]);
        assert_eq!(resigned.txid(), tx.txid());
        assert_ne!(resigned.instance_hash(), tx.instance_hash());

        // The rule looks at the payload bytes, so it holds for an undecoded payload too.
        let raw = Transaction {
            special_transaction_payload: Some(TransactionPayload::UnknownPayloadType(
                9,
                serialize(tx.special_transaction_payload.as_ref().unwrap()),
            )),
            ..tx.clone()
        };
        assert_eq!(raw.txid(), tx.txid());

        // Version 1 payloads and non-special versions hash the full serialization.
        let v1 = asset_unlock_tx(1, [0xac; 96]);
        assert!(!v1.has_stable_txid());
        assert_eq!(v1.txid(), Txid::hash(&serialize(&v1)));
        let non_special = Transaction {
            version: 2,
            ..tx
        };
        assert!(!non_special.has_stable_txid());
        assert_eq!(non_special.txid(), non_special.instance_hash());
    }

    #[test]
    fn test_ntxid() {
        let tx_bytes = hex_conservative::decode_to_vec("0100000001a15d57094aa7a21a28cb20b59aab8fc7d1149a3bdbcddba9c622e4f5f6a99ece010000006c493046022100f93bb0e7d8db7bd46e40132d1f8242026e045f03a0efe71bbb8e3f475e970d790221009337cd7f1f929f00cc6ff01f03729b069a7c21b59b1736ddfee5db5946c5da8c0121033b9b137ee87d5a812d6f506efdd37f0affa7ffc310711c06c7f3e097c9447c52ffffffff0100e1f505000000001976a9140389035a9225b3839e2bbf32d826a1e222031fd888ac00000000").unwrap();
        let mut tx: Transaction = deserialize(&tx_bytes).unwrap();

        let old_ntxid = tx.ntxid();
        assert_eq!(
            format!("{:x}", old_ntxid),
            "c3573dbea28ce24425c59a189391937e00d255150fa973d59d61caf3a06b601d"
        );
        // changing sigs does not affect it
        tx.input[0].script_sig = ScriptBuf::new();
        assert_eq!(old_ntxid, tx.ntxid());
        // changing pks does
        tx.output[0].script_pubkey = ScriptBuf::new();
        assert_ne!(old_ntxid, tx.ntxid());
    }

    #[test]
    fn test_txid() {
        // non-segwit tx from my mempool
        let tx_bytes = hex!(
            "01000000010c7196428403d8b0c88fcb3ee8d64f56f55c8973c9ab7dd106bb4f3527f5888d000000006a47\
             30440220503a696f55f2c00eee2ac5e65b17767cd88ed04866b5637d3c1d5d996a70656d02202c9aff698f\
             343abb6d176704beda63fcdec503133ea4f6a5216b7f925fa9910c0121024d89b5a13d6521388969209df2\
             7a8469bd565aff10e8d42cef931fad5121bfb8ffffffff02b825b404000000001976a914ef79e7ee9fff98\
             bcfd08473d2b76b02a48f8c69088ac0000000000000000296a273236303039343836393731373233313237\
             3633313032313332353630353838373931323132373000000000"
        );
        let tx: Transaction = deserialize(&tx_bytes).unwrap();

        assert_eq!(
            format!("{:x}", tx.wtxid()),
            "971ed48a62c143bbd9c87f4bafa2ef213cfa106c6e140f111931d0be307468dd"
        );
        assert_eq!(
            format!("{:x}", tx.txid()),
            "971ed48a62c143bbd9c87f4bafa2ef213cfa106c6e140f111931d0be307468dd"
        );
    }

    #[test]
    #[cfg(feature = "serde")]
    fn test_txn_encode_decode() {
        let tx_bytes = hex_conservative::decode_to_vec("0100000001a15d57094aa7a21a28cb20b59aab8fc7d1149a3bdbcddba9c622e4f5f6a99ece010000006c493046022100f93bb0e7d8db7bd46e40132d1f8242026e045f03a0efe71bbb8e3f475e970d790221009337cd7f1f929f00cc6ff01f03729b069a7c21b59b1736ddfee5db5946c5da8c0121033b9b137ee87d5a812d6f506efdd37f0affa7ffc310711c06c7f3e097c9447c52ffffffff0100e1f505000000001976a9140389035a9225b3839e2bbf32d826a1e222031fd888ac00000000").unwrap();
        let tx: Transaction = deserialize(&tx_bytes).unwrap();
        serde_round_trip!(tx);
    }

    // Transaction `4be105f158ea44aec57bf12c5817d073a712ab131df6f37786872cfc70734188` from Bitcoin
    // testnet, the first BIP144-encoded transaction encountered. Dash Core has no BIP144: `00 01`
    // after the version is no inputs and one output, never a marker and a flag, so the witness
    // never comes out of these bytes.
    #[test]
    fn bip144_encoding_is_not_read_as_segwit() {
        let tx_bytes = hex_conservative::decode_to_vec("010000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff3603da1b0e00045503bd5704c7dd8a0d0ced13bb5785010800000000000a636b706f6f6c122f4e696e6a61506f6f6c2f5345475749542fffffffff02b4e5a212000000001976a914876fbb82ec05caa6af7a3b5e5a983aae6c6cc6d688ac0000000000000000266a24aa21a9edf91c46b49eb8a29089980f02ee6b57e7d63d33b18b4fddac2bcd7db2a39837040120000000000000000000000000000000000000000000000000000000000000000000000000").unwrap();
        // Read as Core reads it: no inputs, one output, then bytes that are not a lock time.
        assert!(deserialize::<Transaction>(&tx_bytes).is_err());
        let (tx, used) = deserialize_partial::<Transaction>(&tx_bytes).unwrap();
        assert!(tx.input.is_empty());
        assert_eq!(tx.output.len(), 1);
        assert!(used < tx_bytes.len());
    }

    #[test]
    fn add_burn_output() {
        let mut tx = Transaction {
            version: 0,
            lock_time: 0,
            input: vec![],
            output: vec![],
            special_transaction_payload: None,
        };

        let pk_data =
            hex_conservative::decode_to_vec("b8e2d839dd21088b78bebfea3e3e632181197982").unwrap();

        let mut pk_array: [u8; 20] = [0; 20];
        for (index, kek) in pk_array.iter_mut().enumerate() {
            *kek = *pk_data.get(index).unwrap();
        }

        tx.add_burn_output(10000, &pk_array);

        let output = tx.output.first().unwrap();

        assert_eq!(output.value, 10000);
        assert!(output.script_pubkey.is_op_return());

        let data = &output.script_pubkey.as_bytes()[2..];

        assert_eq!(data.len(), 20);
        assert_eq!(&data, &pk_data.as_slice());
    }

    #[test]
    fn deserialize_serialize_coinbase_transaction_in_dml() {
        let block_hex = include_str!("../../../tests/data/test_DML_diffs/DML_0_2221605.hex");
        let data = hex_conservative::decode_to_vec(block_hex).expect("decode hex");
        let mn_list_diff: RawNetworkMessage = deserialize(&data).expect("deserialize MnListDiff");
        if let NetworkMessage::MnListDiff(diff) = mn_list_diff.payload {
            let serialized = serialize(&diff.coinbase_tx);
            let deserialized: Transaction =
                deserialize(serialized.as_slice()).expect("expected to deserialize");
            assert_eq!(deserialized, diff.coinbase_tx);
        }
    }
}
